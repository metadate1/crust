//! Opt-in end-to-end runtime bridge checks against the user's own retail data.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use crust_formats::binary::{Eid, PageIndex};
use crust_formats::disc::DiscImage;
use crust_formats::stream::{
    LevelId, RetailZoneGraph, StreamKind, StreamName, ZoneEntity, ZoneHeader, ZoneRect,
    load_gool_state_program, parse_nsd, parse_nsf,
};
use crust_sim::camera::{RetailCameraInput, RetailCameraRuntime};
use crust_sim::gool::{
    CollisionObjectReference, MAX_OBJECTS, ObjectHandle as VmObjectHandle, RetailPadSnapshot,
    RetailTransformVectorsCamera, VmEffect, process_register,
};
use crust_sim::object_arena::{
    ENEMY_OBJECT_ROOT, MAIN_OBJECT_ROOT, NeighborZone, ObjectOrigin, TreeParent, ZONE_OBJECT_ROOT,
};
use crust_sim::paging::{Pager, PagerUpdateOutcome};
use crust_sim::retail_runtime::{
    NsfProgramHost, PagedNsfProgramHost, ProgramHost, RetailLevelStateContext, RetailRuntime,
};
use crust_sim::zone_lifecycle::{
    OrderedZoneLoadList, SpawnScanZone, ZoneLifecycle, ZoneLifecycleZone, ZoneTransitionAction,
};
use crust_sim::{Angle12, Vec3};

const N_SANITY_E0_ENTRIES: &[&str] = &["WillT", "Ju89T", "JuA9T", "Ju19T", "Ju49T", "Ju59T"];
const N_SANITY_A0_ENTRIES: &[&str] = N_SANITY_E0_ENTRIES;
const N_SANITY_A1_ENTRIES: &[&str] = &["WillT", "JuA9T", "Ju19T", "Ju49T", "Ju89T", "Ju59T"];
const N_SANITY_E0_PAGES: &[u32] = &[
    0, 1, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28,
];
const N_SANITY_A0_PAGES: &[u32] = &[
    0, 1, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 31, 32, 33,
];
const N_SANITY_A1_PAGES: &[u32] = &[
    0, 1, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 31, 32, 33, 34, 35, 36, 37,
];

fn retail_eid(name: &str) -> Eid {
    Eid::from_name(name).expect("golden EID uses the retail alphabet")
}

fn close_load_actions(entries: &[&str], pages: &[u32]) -> Vec<ZoneTransitionAction> {
    entries
        .iter()
        .map(|name| ZoneTransitionAction::CloseEntry(retail_eid(name)))
        .chain(
            pages
                .iter()
                .copied()
                .map(|page| ZoneTransitionAction::ClosePage(PageIndex::new(page))),
        )
        .collect()
}

fn open_load_actions(entries: &[&str], pages: &[u32]) -> Vec<ZoneTransitionAction> {
    entries
        .iter()
        .map(|name| ZoneTransitionAction::OpenEntry(retail_eid(name)))
        .chain(
            pages
                .iter()
                .copied()
                .map(|page| ZoneTransitionAction::OpenPage(PageIndex::new(page))),
        )
        .collect()
}

fn scan_signature(scan: &[SpawnScanZone]) -> Vec<(usize, Eid, u32)> {
    scan.iter()
        .map(|zone| (zone.neighbor_index, zone.zone, zone.display_flags))
        .collect()
}

fn scan_entity_ids(scan: &[SpawnScanZone], entity_ids: &BTreeMap<Eid, Vec<u16>>) -> Vec<u16> {
    scan.iter()
        .flat_map(|zone| entity_ids[&zone.zone].iter().copied())
        .collect()
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn n_sanity_a3_authored_crate_pair_has_native_bidirectional_links() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name a legally local extracted stream directory"),
    );
    let level = LevelId::N_SANITY_BEACH;
    let nsd_bytes =
        std::fs::read(root.join(StreamName::new(level, StreamKind::Nsd).filename())).unwrap();
    let nsf_bytes =
        std::fs::read(root.join(StreamName::new(level, StreamKind::Nsf).filename())).unwrap();
    let nsd = parse_nsd(&nsd_bytes, level).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
    let zone = retail_eid("a3_9Z");
    let entry = nsf.resolve_entry(&nsd, zone).unwrap();
    let header = ZoneHeader::parse(entry.item(0).unwrap().bytes(&nsf_bytes).unwrap()).unwrap();
    let entities = (0..header.entity_count)
        .map(|entity_index| {
            let item_index =
                usize::try_from(header.entity_item_index(entity_index).unwrap()).unwrap();
            ZoneEntity::parse(entry.item(item_index).unwrap().bytes(&nsf_bytes).unwrap()).unwrap()
        })
        .collect::<Vec<_>>();
    let lower = entities.iter().find(|entity| entity.id == 23).unwrap();
    let upper = entities.iter().find(|entity| entity.id == 24).unwrap();
    assert_eq!((lower.executable, upper.executable), (0x22, 0x22));
    assert_eq!(lower.path_points[0].x, upper.path_points[0].x);
    assert_eq!(lower.path_points[0].z, upper.path_points[0].z);
    assert_eq!(upper.path_points[0].y - lower.path_points[0].y, 100);

    let neighbors = [NeighborZone {
        eid: zone,
        display_flags: 2,
        entities: &entities,
    }];
    let mut runtime = RetailRuntime::new_for_level(256, level);
    let mut host = NsfProgramHost::new(&nsd, &nsf, &nsf_bytes);
    let attempts = runtime.spawn_current_zone_neighbors(&neighbors, &mut host);
    let lower = attempts
        .iter()
        .find(|attempt| attempt.descriptor.id == 23)
        .unwrap()
        .result
        .as_ref()
        .unwrap();
    let upper = attempts
        .iter()
        .find(|attempt| attempt.descriptor.id == 24)
        .unwrap()
        .result
        .as_ref()
        .unwrap();

    assert_eq!(
        CollisionObjectReference::from_word(
            runtime
                .machine()
                .object(lower.vm())
                .unwrap()
                .register(process_register::MISC_A_Y)
                .unwrap(),
        )
        .map(CollisionObjectReference::object),
        Some(upper.vm()),
    );
    assert_eq!(
        CollisionObjectReference::from_word(
            runtime
                .machine()
                .object(upper.vm())
                .unwrap()
                .register(process_register::MISC_A_X)
                .unwrap(),
        )
        .map(CollisionObjectReference::object),
        Some(lower.vm()),
    );
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn ripper_roo_mount_creates_authored_root_controller_before_zone_scan() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name a legally local extracted stream directory"),
    );
    let level = LevelId::new_const(0x17);
    let nsd_bytes =
        std::fs::read(root.join(StreamName::new(level, StreamKind::Nsd).filename())).unwrap();
    let nsf_bytes =
        std::fs::read(root.join(StreamName::new(level, StreamKind::Nsf).filename())).unwrap();
    let nsd = parse_nsd(&nsd_bytes, level).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
    let graph = RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes).unwrap();
    let ldat = nsd.ldat().unwrap();
    assert_eq!(ldat.executable_map[39].name().as_deref(), Some("RooOC"));

    // This controller is not serialized as a zone entity. It exists only
    // because CoreObjectsCreate calls LevelInitMisc(1) at each stream mount.
    let mut serialized_controller_entities = Vec::new();
    for node in graph.zones() {
        let entry = nsf.resolve_entry(&nsd, node.eid).unwrap();
        let header = ZoneHeader::parse(entry.item(0).unwrap().bytes(&nsf_bytes).unwrap()).unwrap();
        for entity_index in 0..header.entity_count {
            let item_index =
                usize::try_from(header.entity_item_index(entity_index).unwrap()).unwrap();
            let entity =
                ZoneEntity::parse(entry.item(item_index).unwrap().bytes(&nsf_bytes).unwrap())
                    .unwrap();
            if (entity.executable, entity.subtype) == (39, 4) {
                serialized_controller_entities.push((node.eid, entity.id));
            }
        }
    }
    assert!(serialized_controller_entities.is_empty());

    let mut runtime = RetailRuntime::new_for_level(256, level);
    let mut host = NsfProgramHost::new(&nsd, &nsf, &nsf_bytes);
    let controller = runtime
        .create_retail_level_misc_object(graph.spawn_path().zone, &mut host)
        .expect("Ripper Roo LevelInitMisc(1) controller must bind")
        .expect("Ripper Roo must create one root-four controller");
    let spawned = runtime.arena().get(controller.arena()).unwrap();
    assert_eq!(spawned.zone(), Eid::NONE);
    assert_eq!(spawned.parent(), TreeParent::Root(ENEMY_OBJECT_ROOT));
    assert_eq!(
        spawned.origin(),
        ObjectOrigin::Runtime {
            executable: 39,
            subtype: 4,
        }
    );
    assert_eq!(
        CollisionObjectReference::from_word(runtime.global_word(8).unwrap())
            .map(CollisionObjectReference::object),
        Some(controller.vm()),
        "Ripper Roo publishes the controller through ambiance_obj"
    );
    assert_eq!(
        runtime
            .arena()
            .preorder(TreeParent::Root(ENEMY_OBJECT_ROOT))
            .unwrap()
            .collect::<Vec<_>>(),
        [controller.arena()]
    );

    runtime.set_frame_timing(34, 34);
    let frame = runtime.run_frame(&mut host, 1_024).unwrap();
    assert_eq!(frame.executions.len(), 1);
    assert_eq!(frame.executions[0].object, controller);
    assert!(frame.executions[0].result.is_ok());
    assert_eq!(runtime.faulted_object_count(), 0);
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn ripper_roo_big_tnt_children_copy_authored_waterfall_path() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name a legally local extracted stream directory"),
    );
    let level = LevelId::new_const(0x17);
    let nsd_bytes =
        std::fs::read(root.join(StreamName::new(level, StreamKind::Nsd).filename())).unwrap();
    let nsf_bytes =
        std::fs::read(root.join(StreamName::new(level, StreamKind::Nsf).filename())).unwrap();
    let nsd = parse_nsd(&nsd_bytes, level).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
    let roo_eid = nsd.ldat().unwrap().executable_map[39];
    let roo_state_two = load_gool_state_program(&nsd, &nsf, &nsf_bytes, roo_eid, 2).unwrap();
    assert_eq!(roo_state_two.transition_pc(), Some(26));
    assert_eq!(
        roo_state_two.code().get(41),
        Some(&0x11e2_0e22),
        "RooOC state two must move the live post-fetch PC into process.tp"
    );
    let graph = RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes).unwrap();
    let zone = graph.spawn_path().zone;
    let entry = nsf.resolve_entry(&nsd, zone).unwrap();
    let header = ZoneHeader::parse(entry.item(0).unwrap().bytes(&nsf_bytes).unwrap()).unwrap();
    let entities = (0..header.entity_count)
        .map(|entity_index| {
            let item_index =
                usize::try_from(header.entity_item_index(entity_index).unwrap()).unwrap();
            ZoneEntity::parse(entry.item(item_index).unwrap().bytes(&nsf_bytes).unwrap()).unwrap()
        })
        .collect::<Vec<_>>();
    let neighbors = [NeighborZone {
        eid: zone,
        display_flags: header.display_flags | 7,
        entities: &entities,
    }];

    let mut camera = RetailCameraRuntime::new(&graph).unwrap();
    let mut runtime = RetailRuntime::new_for_level(256, level);
    runtime.set_level_state_context(RetailLevelStateContext {
        location: camera.location(),
        graphics_flags: graph.zone(zone).unwrap().graphics_flags,
        box_count: 0,
        checkpoint_id: -1,
        checkpoint_translation: [0; 3],
        first_spawn: false,
        active_neighbor_zones: vec![zone],
    });
    let mut initial_host = NsfProgramHost::new(&nsd, &nsf, &nsf_bytes);
    runtime
        .create_retail_core_objects(zone, &mut initial_host)
        .unwrap();
    runtime
        .create_retail_level_misc_object(zone, &mut initial_host)
        .unwrap();
    let initial_load_list = OrderedZoneLoadList::from(&header.load_list);
    let mut pager = Pager::mount_retail_level(
        &nsd,
        &nsf,
        level,
        zone,
        initial_load_list.entries().iter().copied(),
        initial_load_list.pages().iter().copied(),
    )
    .unwrap();
    runtime
        .seed_platform_paging_state_with_capacity(
            u32::try_from(pager.page_count()).unwrap(),
            u32::try_from(pager.physical_slot_count()).unwrap(),
            pager.resolved_pages(),
            pager.page_reference_counts(),
            pager.uncounted_pages(),
        )
        .unwrap();
    let mut host = PagedNsfProgramHost::new(&nsd, &nsf, &nsf_bytes, &mut pager);

    let boss_sample = |runtime: &RetailRuntime| {
        runtime
            .arena()
            .postorder_snapshot()
            .unwrap()
            .into_iter()
            .find_map(|arena| {
                let spawned = runtime.arena().get(arena)?;
                if spawned.entity_descriptor()?.id != 8 {
                    return None;
                }
                let object = runtime.object_for_arena(arena)?;
                let vm = runtime.machine().object(object.vm()).ok()?;
                Some((
                    vm.state(),
                    [
                        vm.register(process_register::TRANSLATION_X)
                            .ok()?
                            .cast_signed(),
                        vm.register(process_register::TRANSLATION_Y)
                            .ok()?
                            .cast_signed(),
                        vm.register(process_register::TRANSLATION_Z)
                            .ok()?
                            .cast_signed(),
                    ],
                ))
            })
            .expect("Ripper Roo entity 8 must remain live")
    };

    let mut materialized_big_tnts = 0_usize;
    let mut big_tnt_materialization_frames = Vec::new();
    let mut requested_big_tnts = 0_usize;
    let mut boss_samples = Vec::new();
    let mut first_big_tnt_path_sample = None;
    for frame in 1_u32..=300 {
        runtime.set_frame_timing(34, 34);
        if let Some(outcome) = host.pager_mut().update_pending_virtual_page().unwrap() {
            match outcome {
                PagerUpdateOutcome::Invalidated(pages) => {
                    runtime.apply_platform_paging_evictions(&pages).unwrap();
                }
                PagerUpdateOutcome::Resolved(outcome) => {
                    runtime
                        .apply_platform_paging_resolution(outcome.page, outcome.invalidated)
                        .unwrap();
                }
            }
        }
        let _ = runtime.spawn_current_zone_neighbors(&neighbors, &mut host);
        runtime.advance_level_shader().unwrap();
        let camera_step = if runtime.current_display_mask() & 2 == 0 {
            camera.stationary_step()
        } else {
            camera.update(&graph, RetailCameraInput::default()).unwrap()
        };
        let location = camera_step.after;
        runtime.set_level_state_context(RetailLevelStateContext {
            location,
            graphics_flags: graph.zone(location.path.zone).unwrap().graphics_flags,
            box_count: 0,
            checkpoint_id: -1,
            checkpoint_translation: [0; 3],
            first_spawn: false,
            active_neighbor_zones: vec![zone],
        });
        let pose = camera.pose(&graph).unwrap();
        runtime.set_frame_context(camera_step.game_state, camera.rotation_xz(&graph).unwrap());
        runtime.set_transform_vectors_camera(RetailTransformVectorsCamera::from_retail_pose(
            pose.translation,
            pose.rotation_yxz,
            288,
        ));
        let report = runtime.run_frame(&mut host, 67).unwrap();
        assert!(
            report
                .executions
                .iter()
                .all(|execution| execution.result.is_ok()),
            "Ripper Roo frame {frame} crossed a checked VM boundary: {:?}",
            report
                .executions
                .iter()
                .filter(|execution| execution.result.is_err())
                .collect::<Vec<_>>()
        );

        let frame_big_tnts = report
            .spawned_children
            .iter()
            .filter(|child| {
                runtime.arena().get(child.arena()).is_some_and(|spawned| {
                    spawned.origin()
                        == (ObjectOrigin::Runtime {
                            executable: 39,
                            subtype: 1,
                        })
                })
            })
            .count();
        materialized_big_tnts += frame_big_tnts;
        if frame_big_tnts != 0 {
            big_tnt_materialization_frames.push(frame);
        }
        requested_big_tnts += report
            .effects
            .iter()
            .filter_map(|effect| match effect {
                VmEffect::SpawnChildren {
                    executable: 39,
                    subtype: 1,
                    count,
                    allow_reclaim: false,
                    ..
                } => Some(usize::try_from(*count).unwrap()),
                _ => None,
            })
            .sum::<usize>();

        if frame == 1 {
            let source = runtime
                .arena()
                .postorder_snapshot()
                .unwrap()
                .into_iter()
                .find_map(|arena| {
                    (runtime.arena().get(arena)?.entity_descriptor()?.id == 7)
                        .then(|| runtime.object_for_arena(arena))
                        .flatten()
                })
                .expect("RooOC entity 7 must own the waterfall path");
            let big_tnt = report
                .spawned_children
                .iter()
                .copied()
                .find(|child| {
                    runtime.arena().get(child.arena()).is_some_and(|spawned| {
                        spawned.origin()
                            == (ObjectOrigin::Runtime {
                                executable: 39,
                                subtype: 1,
                            })
                    })
                })
                .expect("RooOC entity 7 must create its first Big TNT on frame one");
            let source_vm = runtime.machine().object(source.vm()).unwrap();
            let big_tnt_vm = runtime.machine().object(big_tnt.vm()).unwrap();
            first_big_tnt_path_sample = Some((
                source_vm
                    .register(process_register::ENTITY_REFERENCE)
                    .unwrap(),
                big_tnt_vm
                    .register(process_register::ENTITY_REFERENCE)
                    .unwrap(),
                [
                    big_tnt_vm
                        .register(process_register::TRANSLATION_X)
                        .unwrap()
                        .cast_signed(),
                    big_tnt_vm
                        .register(process_register::TRANSLATION_Z)
                        .unwrap()
                        .cast_signed(),
                ],
            ));
        }

        if matches!(frame, 1 | 80 | 151 | 200 | 230 | 270 | 300) {
            boss_samples.push((frame, boss_sample(&runtime)));
        }
    }

    // RooOC state two creates exactly one Big TNT per authored waterfall
    // controller. Its transition starts at PC 26 and source word 41 moves the
    // live post-fetch PC into `process.tp`, advancing future draws to PC 42
    // instead of repeating the spawn prefix. State three then copies the
    // parent's native `process.entity` pointer before states four/five sample
    // that controller's path. The second displayed controller becomes active
    // at frame 199 in this deterministic idle camera route.
    assert_eq!(requested_big_tnts, 2);
    assert_eq!(
        materialized_big_tnts, 2,
        "materialized on frames {big_tnt_materialization_frames:?}"
    );
    assert_eq!(big_tnt_materialization_frames, [1, 199]);
    assert_eq!(runtime.arena().remaining_pool_capacity(), 76);
    assert_eq!(runtime.arena().len(), 21);
    assert_eq!(runtime.faulted_object_count(), 0);

    let (source_reference, copied_reference, first_xz) =
        first_big_tnt_path_sample.expect("frame-one Big TNT sample");
    assert_ne!(source_reference, 0, "authored entity pointers are non-null");
    assert_eq!(copied_reference, source_reference);
    assert_eq!(first_xz, [-251_392, -1_383_332]);

    let mut live_big_tnt_samples = BTreeMap::new();
    for arena in runtime.arena().postorder_snapshot().unwrap() {
        let spawned = runtime.arena().get(arena).unwrap();
        let ObjectOrigin::Runtime {
            executable,
            subtype,
        } = spawned.origin()
        else {
            continue;
        };
        if (executable, subtype) == (39, 1) {
            let TreeParent::Object(parent_arena) = spawned.parent() else {
                panic!("Big TNT must remain parented to its waterfall controller");
            };
            let parent_id = runtime
                .arena()
                .get(parent_arena)
                .and_then(crust_sim::object_arena::SpawnedObject::entity_descriptor)
                .map(|descriptor| descriptor.id)
                .expect("Big TNT parent must be an authored entity");
            let object = runtime.object_for_arena(arena).unwrap();
            let vm = runtime.machine().object(object.vm()).unwrap();
            let parent = runtime
                .object_for_arena(parent_arena)
                .expect("Big TNT parent remains live");
            let parent_vm = runtime.machine().object(parent.vm()).unwrap();
            assert_eq!(
                vm.register(process_register::ENTITY_REFERENCE),
                parent_vm.register(process_register::ENTITY_REFERENCE)
            );
            assert!(
                live_big_tnt_samples
                    .insert(
                        parent_id,
                        (
                            vm.state(),
                            [
                                vm.register(process_register::TRANSLATION_X)
                                    .unwrap()
                                    .cast_signed(),
                                vm.register(process_register::TRANSLATION_Y)
                                    .unwrap()
                                    .cast_signed(),
                                vm.register(process_register::TRANSLATION_Z)
                                    .unwrap()
                                    .cast_signed(),
                            ],
                        ),
                    )
                    .is_none(),
                "each authored waterfall controller creates one Big TNT"
            );
        }
    }
    assert_eq!(
        live_big_tnt_samples,
        BTreeMap::from([
            (7, (5, [-251_392, -47_616, -386_592])),
            (10, (5, [260_608, -44_544, -911_336])),
        ])
    );
    let big_tnt_render = runtime
        .render_objects()
        .unwrap()
        .into_iter()
        .filter(|object| (object.executable, object.subtype) == (39, 1))
        .collect::<Vec<_>>();
    assert_eq!(big_tnt_render.len(), 2);
    assert!(big_tnt_render.iter().all(|object| object.display_eligible));
    let mut rendered_x = big_tnt_render
        .iter()
        .map(|object| object.transform.translation[0])
        .collect::<Vec<_>>();
    rendered_x.sort_unstable();
    assert_eq!(rendered_x, [-251_392, 260_608]);
    assert_eq!(
        boss_samples,
        [
            (1, (0, [485_888, -25_600, -397_312])),
            (80, (0, [485_888, -25_600, -397_312])),
            (151, (1, [485_888, 0, -397_312])),
            (200, (1, [-512, 0, -397_312])),
            (230, (1, [-486_912, 0, 114_688])),
            (270, (1, [-512, 0, -397_312])),
            (300, (1, [323_754, 0, -55_979])),
        ],
        "the supported idle path must enter RRooC state one and traverse its authored pad loop"
    );
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn brio_boxsc_creator_link_survives_brioc_pool_reclaim() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name a legally local extracted stream directory"),
    );
    let level = LevelId::new_const(0x1b);
    let nsd_bytes =
        std::fs::read(root.join(StreamName::new(level, StreamKind::Nsd).filename())).unwrap();
    let nsf_bytes =
        std::fs::read(root.join(StreamName::new(level, StreamKind::Nsf).filename())).unwrap();
    let nsd = parse_nsd(&nsd_bytes, level).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
    let graph = RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes).unwrap();
    let zone = graph.spawn_path().zone;
    let entry = nsf.resolve_entry(&nsd, zone).unwrap();
    let header = ZoneHeader::parse(entry.item(0).unwrap().bytes(&nsf_bytes).unwrap()).unwrap();
    let entities = (0..header.entity_count)
        .map(|entity_index| {
            let item_index =
                usize::try_from(header.entity_item_index(entity_index).unwrap()).unwrap();
            ZoneEntity::parse(entry.item(item_index).unwrap().bytes(&nsf_bytes).unwrap()).unwrap()
        })
        .collect::<Vec<_>>();
    let neighbors = [NeighborZone {
        eid: zone,
        // The initial LevelUpdate marks the current-zone band loaded,
        // displayed, and activation-scannable before the first entity pass.
        display_flags: header.display_flags | 7,
        entities: &entities,
    }];

    let mut camera = RetailCameraRuntime::new(&graph).unwrap();
    let mut runtime = RetailRuntime::new_for_level(256, level);
    runtime.set_level_state_context(RetailLevelStateContext {
        location: camera.location(),
        graphics_flags: graph.zone(zone).unwrap().graphics_flags,
        box_count: 0,
        checkpoint_id: -1,
        checkpoint_translation: [0; 3],
        first_spawn: false,
        active_neighbor_zones: vec![zone],
    });
    let mut host = NsfProgramHost::new(&nsd, &nsf, &nsf_bytes);
    runtime.create_retail_core_objects(zone, &mut host).unwrap();
    runtime
        .create_retail_level_misc_object(zone, &mut host)
        .unwrap();

    let boxsc = retail_eid("BoxsC");
    let brioc = retail_eid("BriOC");
    let mut held_previous = 0;
    let mut held_previous_2 = 0;
    let mut tapped_previous = 0;
    let mut creator = None;
    let mut creator_word = 0;
    let mut retained_children = Vec::new();
    for frame in 1_u32..=406 {
        runtime.set_frame_timing(34, 34);
        let held = match (frame - 1) % 120 {
            0..=31 => 0x1000,
            32..=39 => 0x1040,
            40..=55 => 0x2000,
            56..=63 => 0x2080,
            64..=71 => 0x0040,
            72..=79 => 0x4000,
            80..=87 => 0x8000,
            88..=95 => 0x0020,
            96..=103 => 0x0010,
            104 => 0x0800,
            _ => 0,
        };
        let tapped = held & !held_previous;
        runtime
            .set_pad_snapshot(
                0,
                RetailPadSnapshot {
                    tapped,
                    held,
                    held_previous,
                    tapped_previous,
                    held_previous_2,
                },
            )
            .unwrap();
        held_previous_2 = held_previous;
        held_previous = held;
        tapped_previous = tapped;

        let _ = runtime.spawn_current_zone_neighbors(&neighbors, &mut host);
        runtime.advance_level_shader().unwrap();
        let camera_step = if runtime.current_display_mask() & 2 == 0 {
            camera.stationary_step()
        } else {
            camera.update(&graph, RetailCameraInput::default()).unwrap()
        };
        let location = camera_step.after;
        runtime.set_level_state_context(RetailLevelStateContext {
            location,
            graphics_flags: graph.zone(location.path.zone).unwrap().graphics_flags,
            box_count: 0,
            checkpoint_id: -1,
            checkpoint_translation: [0; 3],
            first_spawn: false,
            active_neighbor_zones: vec![zone],
        });
        let pose = camera.pose(&graph).unwrap();
        runtime.set_frame_context(camera_step.game_state, camera.rotation_xz(&graph).unwrap());
        runtime.set_transform_vectors_camera(RetailTransformVectorsCamera::from_retail_pose(
            pose.translation,
            pose.rotation_yxz,
            288,
        ));
        let report = runtime.run_frame(&mut host, 67).unwrap();
        assert!(
            report
                .executions
                .iter()
                .all(|execution| execution.result.is_ok()),
            "Brio frame {frame} crossed a checked VM boundary: {:?}",
            report
                .executions
                .iter()
                .filter(|execution| execution.result.is_err())
                .collect::<Vec<_>>()
        );

        if frame == 405 {
            for handle in (0..MAX_OBJECTS)
                .filter_map(|index| u16::try_from(index).ok())
                .filter_map(VmObjectHandle::new)
            {
                let Ok(object) = runtime.machine().object(handle) else {
                    continue;
                };
                if object
                    .program_identity()
                    .is_none_or(|identity| identity.global_eid() != boxsc)
                {
                    continue;
                }
                let word = object.register(4).unwrap();
                let Some(reference) = CollisionObjectReference::from_word(word) else {
                    continue;
                };
                let Ok(creator_object) = runtime.machine().object(reference.object()) else {
                    continue;
                };
                if creator_object
                    .program_identity()
                    .is_some_and(|identity| identity.global_eid() == brioc)
                {
                    creator.get_or_insert(reference);
                    if creator_word == 0 {
                        creator_word = word;
                    }
                    assert_eq!(word, creator_word);
                    retained_children.push(handle);
                }
            }
            assert_eq!(
                retained_children.len(),
                8,
                "the legal frame-405 trace must expose the authored Brio box wave"
            );
        }
    }

    let creator = creator.expect("the authored frame-405 BoxsC wave must retain creator BriOC");
    assert_eq!(
        runtime.object_for_vm(creator.object()),
        None,
        "BriOC must take its authored frame-406 reclaim path"
    );
    for child in retained_children {
        let object = runtime
            .machine()
            .object(child)
            .expect("BoxsC must remain live across its creator's reclaim");
        assert_eq!(
            object.register(4).unwrap(),
            creator_word,
            "native link four retains the reclaimed physical pool pointer"
        );
    }
    assert_eq!(runtime.faulted_object_count(), 0);
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn n_sanity_zone_lifecycle_matches_local_retail_band_and_transition_goldens() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name a legally local extracted stream directory"),
    );
    let level = LevelId::N_SANITY_BEACH;
    let nsd_path = root.join(StreamName::new(level, StreamKind::Nsd).filename());
    let nsf_path = root.join(StreamName::new(level, StreamKind::Nsf).filename());
    let nsd_bytes = std::fs::read(&nsd_path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", nsd_path.display()));
    let nsf_bytes = std::fs::read(&nsf_path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", nsf_path.display()));
    let nsd = parse_nsd(&nsd_bytes, level).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
    let graph = RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes).unwrap();

    let mut entity_ids = BTreeMap::new();
    let mut lifecycle_zones = Vec::with_capacity(graph.zone_count());
    for node in graph.zones() {
        let entry = nsf.resolve_entry(&nsd, node.eid).unwrap();
        let header = ZoneHeader::parse(entry.item(0).unwrap().bytes(&nsf_bytes).unwrap()).unwrap();
        let ids = (0..header.entity_count)
            .map(|entity_index| {
                let item_index =
                    usize::try_from(header.entity_item_index(entity_index).unwrap()).unwrap();
                ZoneEntity::parse(entry.item(item_index).unwrap().bytes(&nsf_bytes).unwrap())
                    .unwrap()
                    .id
            })
            .collect::<Vec<_>>();
        entity_ids.insert(node.eid, ids);
        lifecycle_zones.push(ZoneLifecycleZone::new(
            node.eid,
            header.display_flags,
            header.neighbors,
            OrderedZoneLoadList::from(&header.load_list),
        ));
    }

    let e0 = retail_eid("e0_9Z");
    let a0 = retail_eid("a0_9Z");
    let a1 = retail_eid("a1_9Z");
    let a2 = retail_eid("a2_9Z");
    assert_eq!(graph.spawn_path().zone, e0);
    assert_eq!(entity_ids[&e0].as_slice(), [6, 7, 8, 9, 10]);
    assert_eq!(entity_ids[&a0].as_slice(), [13, 14]);
    assert_eq!(entity_ids[&a1].as_slice(), [11, 12]);
    assert_eq!(entity_ids[&a2].as_slice(), [15, 16, 17, 18]);

    let mut lifecycle = ZoneLifecycle::new(lifecycle_zones).unwrap();
    let initial = lifecycle.transition_with_marker(e0, true).unwrap();
    assert_eq!(initial.previous_zone(), None);
    assert_eq!(initial.next_zone(), e0);
    assert!(initial.activation_marker());
    assert_eq!(
        scan_signature(initial.next_frame_spawn_scan()),
        [(0, e0, 7), (1, a0, 7)]
    );
    let initial_spawned_ids = scan_entity_ids(initial.next_frame_spawn_scan(), &entity_ids);
    assert_eq!(initial_spawned_ids, [6, 7, 8, 9, 10, 13, 14]);
    assert!(
        entity_ids[&a1]
            .iter()
            .all(|id| !initial_spawned_ids.contains(id)),
        "a1 entities must not spawn during e0's already-completed scan"
    );
    let mut expected_initial = open_load_actions(N_SANITY_E0_ENTRIES, N_SANITY_E0_PAGES);
    expected_initial.extend([
        ZoneTransitionAction::SetDisplayFlags {
            zone: e0,
            before: 0,
            after: 7,
        },
        ZoneTransitionAction::SetDisplayFlags {
            zone: a0,
            before: 0,
            after: 7,
        },
    ]);
    assert_eq!(initial.actions(), expected_initial);

    let into_a0 = lifecycle.transition_with_marker(a0, false).unwrap();
    assert_eq!(into_a0.previous_zone(), Some(e0));
    assert_eq!(into_a0.next_zone(), a0);
    assert!(!into_a0.activation_marker());
    assert_eq!(
        scan_signature(into_a0.next_frame_spawn_scan()),
        [(0, a0, 3), (1, e0, 3), (2, a1, 3)]
    );
    let first_new_spawn_ids = scan_entity_ids(into_a0.next_frame_spawn_scan(), &entity_ids)
        .into_iter()
        .filter(|id| !initial_spawned_ids.contains(id))
        .collect::<Vec<_>>();
    assert_eq!(
        first_new_spawn_ids,
        [11, 12],
        "a1's entities first become spawnable in the frame after e0 -> a0"
    );
    let mut expected_into_a0 = close_load_actions(N_SANITY_E0_ENTRIES, N_SANITY_E0_PAGES);
    expected_into_a0.extend(open_load_actions(N_SANITY_A0_ENTRIES, N_SANITY_A0_PAGES));
    expected_into_a0.extend([
        ZoneTransitionAction::SetDisplayFlags {
            zone: a0,
            before: 7,
            after: 3,
        },
        ZoneTransitionAction::SetDisplayFlags {
            zone: e0,
            before: 7,
            after: 3,
        },
        ZoneTransitionAction::SetDisplayFlags {
            zone: a1,
            before: 0,
            after: 3,
        },
    ]);
    assert_eq!(into_a0.actions(), expected_into_a0);

    let into_a1 = lifecycle.transition_with_marker(a1, false).unwrap();
    assert_eq!(into_a1.previous_zone(), Some(a0));
    assert_eq!(into_a1.next_zone(), a1);
    assert_eq!(
        scan_signature(into_a1.next_frame_spawn_scan()),
        [(0, a1, 3), (1, a0, 3), (2, a2, 3)]
    );
    let mut known_spawned_ids = initial_spawned_ids;
    known_spawned_ids.extend(first_new_spawn_ids);
    let second_new_spawn_ids = scan_entity_ids(into_a1.next_frame_spawn_scan(), &entity_ids)
        .into_iter()
        .filter(|id| !known_spawned_ids.contains(id))
        .collect::<Vec<_>>();
    assert_eq!(second_new_spawn_ids, [15, 16, 17, 18]);

    let mut expected_into_a1 = vec![
        ZoneTransitionAction::TerminateZoneObjects(e0),
        ZoneTransitionAction::SetDisplayFlags {
            zone: e0,
            before: 3,
            after: 0,
        },
    ];
    expected_into_a1.extend(close_load_actions(N_SANITY_A0_ENTRIES, N_SANITY_A0_PAGES));
    expected_into_a1.extend(open_load_actions(N_SANITY_A1_ENTRIES, N_SANITY_A1_PAGES));
    expected_into_a1.push(ZoneTransitionAction::SetDisplayFlags {
        zone: a2,
        before: 0,
        after: 3,
    });
    assert_eq!(into_a1.actions(), expected_into_a1);
    assert_eq!(lifecycle.current_zone(), Some(a1));
    assert_eq!(lifecycle.zone(e0).unwrap().display_flags(), 0);
    assert_eq!(lifecycle.zone(a0).unwrap().display_flags(), 3);
    assert_eq!(lifecycle.zone(a1).unwrap().display_flags(), 3);
    assert_eq!(lifecycle.zone(a2).unwrap().display_flags(), 3);
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn n_sanity_mount_drains_only_the_initial_level_update_queue() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name a legally local extracted stream directory"),
    );
    let level = LevelId::N_SANITY_BEACH;
    let nsd_bytes =
        std::fs::read(root.join(StreamName::new(level, StreamKind::Nsd).filename())).unwrap();
    let nsf_bytes =
        std::fs::read(root.join(StreamName::new(level, StreamKind::Nsf).filename())).unwrap();
    let nsd = parse_nsd(&nsd_bytes, level).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
    let graph = RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes).unwrap();
    let spawn_zone = graph.spawn_path().zone;
    let spawn_entry = nsf.resolve_entry(&nsd, spawn_zone).unwrap();
    let spawn_header =
        ZoneHeader::parse(spawn_entry.item(0).unwrap().bytes(&nsf_bytes).unwrap()).unwrap();
    let initial_load_list = OrderedZoneLoadList::from(&spawn_header.load_list);

    let pager = Pager::mount_retail_level(
        &nsd,
        &nsf,
        level,
        spawn_zone,
        initial_load_list.entries().iter().copied(),
        initial_load_list.pages().iter().copied(),
    )
    .unwrap();
    let resolved = pager.resolved_pages().collect::<BTreeSet<_>>();
    let pending = pager.pending_virtual_pages().collect::<BTreeSet<_>>();
    let initial_pages = initial_load_list
        .pages()
        .iter()
        .copied()
        .chain(
            initial_load_list
                .entries()
                .iter()
                .map(|eid| nsd.pte(*eid).unwrap().page_index()),
        )
        .collect::<BTreeSet<_>>();
    assert!(
        initial_pages.is_subset(&resolved),
        "LevelUpdate's NSUpdate2 must resolve the complete initial load list"
    );
    assert!(
        initial_pages.is_disjoint(&pending),
        "no initial LevelUpdate request may survive the NSUpdate2 drain"
    );

    let ldat = nsd.ldat().unwrap();
    let core_object_pages = [0, 5, 29, 34, 3, 4]
        .into_iter()
        .map(|index| nsd.pte(ldat.executable_map[index]).unwrap().page_index())
        .collect::<BTreeSet<_>>();
    assert!(
        pending.is_subset(&core_object_pages),
        "only CoreObjectsCreate's flag-zero opens may remain queued after mount"
    );
    assert!(
        core_object_pages.is_subset(&resolved.union(&pending).copied().collect::<BTreeSet<_>>()),
        "every CoreObjectsCreate preload must be either already resolved or queued"
    );
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn n_sanity_hard_restart_reloads_the_complete_initial_band() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name a legally local extracted stream directory"),
    );
    let level = LevelId::N_SANITY_BEACH;
    let nsd_bytes =
        std::fs::read(root.join(StreamName::new(level, StreamKind::Nsd).filename())).unwrap();
    let nsf_bytes =
        std::fs::read(root.join(StreamName::new(level, StreamKind::Nsf).filename())).unwrap();
    let nsd = parse_nsd(&nsd_bytes, level).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
    let graph = RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes).unwrap();
    let mut lifecycle_zones = Vec::with_capacity(graph.zone_count());
    for node in graph.zones() {
        let entry = nsf.resolve_entry(&nsd, node.eid).unwrap();
        let header = ZoneHeader::parse(entry.item(0).unwrap().bytes(&nsf_bytes).unwrap()).unwrap();
        lifecycle_zones.push(ZoneLifecycleZone::new(
            node.eid,
            header.display_flags,
            header.neighbors,
            OrderedZoneLoadList::from(&header.load_list),
        ));
    }

    let e0 = retail_eid("e0_9Z");
    let a0 = retail_eid("a0_9Z");
    let mut lifecycle = ZoneLifecycle::new(lifecycle_zones).unwrap();
    lifecycle.transition_with_marker(e0, true).unwrap();
    let plan = lifecycle.plan_hard_restart(e0, true).unwrap();

    let mut expected = vec![
        ZoneTransitionAction::TerminateZoneObjects(e0),
        ZoneTransitionAction::SetDisplayFlags {
            zone: e0,
            before: 7,
            after: 4,
        },
        ZoneTransitionAction::TerminateZoneObjects(a0),
        ZoneTransitionAction::SetDisplayFlags {
            zone: a0,
            before: 7,
            after: 4,
        },
    ];
    expected.extend(close_load_actions(N_SANITY_E0_ENTRIES, N_SANITY_E0_PAGES));
    expected.extend(open_load_actions(N_SANITY_E0_ENTRIES, N_SANITY_E0_PAGES));
    expected.extend([
        ZoneTransitionAction::SetDisplayFlags {
            zone: e0,
            before: 4,
            after: 7,
        },
        ZoneTransitionAction::SetDisplayFlags {
            zone: a0,
            before: 4,
            after: 7,
        },
    ]);
    assert_eq!(plan.actions(), expected);
    assert_eq!(
        scan_signature(plan.next_frame_spawn_scan()),
        [(0, e0, 7), (1, a0, 7)]
    );
    lifecycle.commit_hard_restart(&plan).unwrap();
    assert_eq!(lifecycle.active_neighbor_zones(), [e0, a0]);
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn title_flag_two_level_updates_reach_every_authored_screen_zone() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name a legally local extracted stream directory"),
    );
    let level = LevelId::TITLE;
    let nsd_bytes =
        std::fs::read(root.join(StreamName::new(level, StreamKind::Nsd).filename())).unwrap();
    let nsf_bytes =
        std::fs::read(root.join(StreamName::new(level, StreamKind::Nsf).filename())).unwrap();
    let nsd = parse_nsd(&nsd_bytes, level).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
    let graph = RetailZoneGraph::from_pair_with_roots(
        &nsd,
        &nsf,
        &nsf_bytes,
        [
            "0a_pZ", "0b_pZ", "0c_pZ", "0d_pZ", "0e_pZ", "0f_pZ", "1a_pZ", "1e_pZ", "2b_pZ",
            "3a_pZ",
        ]
        .map(retail_eid),
    )
    .unwrap();
    let mut lifecycle_zones = Vec::with_capacity(graph.zone_count());
    for node in graph.zones() {
        let entry = nsf.resolve_entry(&nsd, node.eid).unwrap();
        let header = ZoneHeader::parse(entry.item(0).unwrap().bytes(&nsf_bytes).unwrap()).unwrap();
        lifecycle_zones.push(ZoneLifecycleZone::new(
            node.eid,
            header.display_flags,
            header.neighbors,
            OrderedZoneLoadList::from(&header.load_list),
        ));
    }
    let mut lifecycle = ZoneLifecycle::new(lifecycle_zones).unwrap();
    lifecycle
        .transition_with_marker(graph.spawn_path().zone, false)
        .unwrap();

    for name in [
        "0b_pZ", "0c_pZ", "0d_pZ", "0e_pZ", "0f_pZ", "1a_pZ", "1e_pZ", "2b_pZ", "3a_pZ", "0a_pZ",
    ] {
        let zone = retail_eid(name);
        assert!(
            graph
                .path(crust_formats::stream::RetailPathId { zone, index: 0 })
                .is_some(),
            "title screen zone {name} must expose its first camera path"
        );
        let plan = lifecycle.plan_transition_with_marker(zone, true).unwrap();
        assert!(
            plan.activation_marker(),
            "{name} must retain flag-two marker"
        );
        lifecycle.commit_transition(&plan).unwrap();
        assert!(
            lifecycle
                .next_frame_spawn_scan()
                .iter()
                .all(|candidate| candidate.display_flags & 4 != 0),
            "{name} neighbors must receive the title activation marker"
        );
    }
}

#[test]
#[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
fn n_sanity_szon_resolves_last_serialized_neighbor_at_inclusive_origin() {
    let disc_path = PathBuf::from(
        std::env::var_os("C1_DISC_IMAGE")
            .expect("C1_DISC_IMAGE must name a legally local NTSC-U raw BIN"),
    );
    let disc_bytes = std::fs::read(&disc_path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", disc_path.display()));
    let image = DiscImage::open(&disc_bytes).unwrap();
    let streams = image.discover_streams().unwrap();
    let level = LevelId::N_SANITY_BEACH;
    let nsd_bytes = image
        .read_stream(
            streams
                .get(StreamName::new(level, StreamKind::Nsd))
                .unwrap(),
        )
        .unwrap();
    let nsf_bytes = image
        .read_stream(
            streams
                .get(StreamName::new(level, StreamKind::Nsf))
                .unwrap(),
        )
        .unwrap();
    let nsd = parse_nsd(&nsd_bytes, level).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
    let current_zone = nsd.ldat().unwrap().spawn_zone;
    let current_entry = nsf.resolve_entry(&nsd, current_zone).unwrap();
    let current_header =
        ZoneHeader::parse(current_entry.item(0).unwrap().bytes(&nsf_bytes).unwrap()).unwrap();
    let last_neighbor = *current_header.neighbors.last().unwrap();
    let neighbor_entry = nsf.resolve_entry(&nsd, last_neighbor).unwrap();
    let rect = ZoneRect::parse(neighbor_entry.item(1).unwrap().bytes(&nsf_bytes).unwrap()).unwrap();
    let inclusive_origin = rect.origin.map(|coordinate| coordinate.wrapping_shl(8));
    let mut host = NsfProgramHost::new(&nsd, &nsf, &nsf_bytes);

    assert_eq!(
        host.find_neighbor_zone(current_zone, inclusive_origin),
        Ok(Some(last_neighbor))
    );
}

#[test]
#[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
fn n_sanity_neighbors_spawn_and_crash_hosts_both_boot_children() {
    let disc_path = PathBuf::from(
        std::env::var_os("C1_DISC_IMAGE")
            .expect("C1_DISC_IMAGE must name a legally local NTSC-U raw BIN"),
    );
    let disc_bytes = std::fs::read(&disc_path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", disc_path.display()));
    let image = DiscImage::open(&disc_bytes).unwrap();
    let streams = image.discover_streams().unwrap();
    let level = LevelId::N_SANITY_BEACH;
    let nsd_bytes = image
        .read_stream(
            streams
                .get(StreamName::new(level, StreamKind::Nsd))
                .unwrap(),
        )
        .unwrap();
    let nsf_bytes = image
        .read_stream(
            streams
                .get(StreamName::new(level, StreamKind::Nsf))
                .unwrap(),
        )
        .unwrap();
    let nsd = parse_nsd(&nsd_bytes, level).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
    let spawn_zone = nsd.ldat().unwrap().spawn_zone;
    let current_entry = nsf.resolve_entry(&nsd, spawn_zone).unwrap();
    let current_header =
        ZoneHeader::parse(current_entry.item(0).unwrap().bytes(&nsf_bytes).unwrap()).unwrap();

    let mut owned_neighbors = Vec::new();
    for eid in current_header.neighbors {
        let entry = nsf.resolve_entry(&nsd, eid).unwrap();
        let header = ZoneHeader::parse(entry.item(0).unwrap().bytes(&nsf_bytes).unwrap()).unwrap();
        let mut entities = Vec::new();
        for entity_index in 0..header.entity_count {
            let item_index =
                usize::try_from(header.entity_item_index(entity_index).unwrap()).unwrap();
            entities.push(
                ZoneEntity::parse(entry.item(item_index).unwrap().bytes(&nsf_bytes).unwrap())
                    .unwrap(),
            );
        }
        // The first LevelUpdate marks every current-zone neighbor loaded and
        // displayed (`|= 3`) immediately before LevelSpawnObjects scans it.
        owned_neighbors.push((eid, header.display_flags | 3, entities));
    }
    let neighbors = owned_neighbors
        .iter()
        .map(|(eid, display_flags, entities)| NeighborZone {
            eid: *eid,
            display_flags: *display_flags,
            entities,
        })
        .collect::<Vec<_>>();
    let mut host = NsfProgramHost::new(&nsd, &nsf, &nsf_bytes);
    let mut runtime = RetailRuntime::new_for_level(256, level);
    let first = runtime
        .spawn_and_run_frame(&neighbors, &mut host, 256)
        .unwrap();
    assert_eq!(first.spawn_attempts.len(), 7);
    assert!(
        first
            .spawn_attempts
            .iter()
            .all(|attempt| attempt.result.is_ok())
    );
    let entity_ids_under = |root| {
        runtime
            .arena()
            .preorder(TreeParent::Root(root))
            .unwrap()
            .filter_map(|handle| match runtime.arena().get(handle)?.origin() {
                ObjectOrigin::Entity(descriptor) => Some(descriptor.id),
                ObjectOrigin::Runtime { .. } => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(entity_ids_under(ZONE_OBJECT_ROOT), [8]);
    assert_eq!(entity_ids_under(ENEMY_OBJECT_ROOT), [6, 7, 10, 13, 14]);
    assert_eq!(entity_ids_under(MAIN_OBJECT_ROOT), [9]);
    let crash = *first
        .spawn_attempts
        .iter()
        .find(|attempt| attempt.descriptor.executable == 0)
        .unwrap()
        .result
        .as_ref()
        .unwrap();
    let crash_spawns = first
        .frame
        .effects
        .iter()
        .filter_map(|effect| match effect {
            VmEffect::SpawnChildren {
                parent,
                executable,
                subtype,
                arguments,
                ..
            } if *parent == crash.vm() => Some((*executable, *subtype, arguments.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(crash_spawns, [(5, 0, vec![0]), (29, 0, vec![0, 4096, 0])]);

    let shadow = first
        .frame
        .spawned_children
        .iter()
        .copied()
        .find(|child| {
            runtime.arena().get(child.arena()).is_some_and(|object| {
                object.origin()
                    == ObjectOrigin::Runtime {
                        executable: 29,
                        subtype: 0,
                    }
            })
        })
        .expect("Crash must synchronously bind ShadC");
    let path_object = first
        .frame
        .executions
        .iter()
        .find(|execution| {
            runtime
                .arena()
                .get(execution.object.arena())
                .is_some_and(|object| {
                    matches!(
                        object.origin(),
                        ObjectOrigin::Entity(descriptor)
                            if descriptor.id == 14
                                && descriptor.group == 3
                                && descriptor.executable == 31
                                && descriptor.subtype == 0
                    )
                })
        })
        .expect("N. Sanity path object must execute in frame one");
    assert!(
        path_object.result.is_ok(),
        "0x85 path orientation must cross its old boundary: {:?}",
        path_object.result
    );
    let path_vm = runtime.machine().object(path_object.object.vm()).unwrap();
    assert_eq!(
        [
            path_vm.register(8).unwrap().cast_signed(),
            path_vm.register(9).unwrap().cast_signed(),
            path_vm.register(10).unwrap().cast_signed(),
        ],
        [1_842_944, 1_076_736, 31_948_288]
    );
    assert_eq!(path_vm.register(47), Ok(1_076_736));
    assert_eq!(path_vm.register(13), Ok(0x400));
    assert_eq!(path_vm.register(21), Ok(0x400));
    let shadow_execution = first
        .frame
        .executions
        .iter()
        .find(|execution| execution.object == shadow)
        .expect("Crash's shadow child must execute in frame one");
    assert!(
        shadow_execution.result.is_ok(),
        "solid suboperation one must use only registered frame snapshots: {:?}",
        shadow_execution.result
    );
    assert!(
        first
            .frame
            .executions
            .iter()
            .all(|execution| execution.result.is_ok()),
        "every frame-one retail object must cross the checked VM boundaries: {:?}",
        first
            .frame
            .executions
            .iter()
            .filter(|execution| execution.result.is_err())
            .collect::<Vec<_>>()
    );
    let first_crash_vm = runtime.machine().object(crash.vm()).unwrap();
    let mut crash_state_trace = vec![(
        1_u32,
        first_crash_vm.state(),
        first_crash_vm.pc(),
        first_crash_vm.register(26).unwrap(),
        first_crash_vm.register(27).unwrap(),
    )];
    let mut previous_crash_state = first_crash_vm.state();
    let mut frame_192_query = None;
    for frame in 2_u32..=1_000 {
        if frame == 192 {
            runtime.set_physics_frame_context(true, Angle12::new(0));
            runtime
                .set_pad_snapshot(
                    0,
                    RetailPadSnapshot {
                        held: 0x2000,
                        ..RetailPadSnapshot::default()
                    },
                )
                .unwrap();
        }
        let report = runtime.run_frame(&mut host, 256).unwrap();
        assert!(
            report
                .executions
                .iter()
                .all(|execution| execution.result.is_ok()),
            "retail object faulted on frame {frame}: {:?}",
            report
                .executions
                .iter()
                .filter(|execution| execution.result.is_err())
                .collect::<Vec<_>>()
        );
        let crash_vm = runtime.machine().object(crash.vm()).unwrap();
        if frame == 192 {
            frame_192_query = Some(
                runtime
                    .machine()
                    .retail_solid_query_cache()
                    .expect("settled Crash must populate native cur_zone_query")
                    .clone(),
            );
        } else if frame == 193 {
            let previous_query = frame_192_query
                .as_ref()
                .expect("frame 192 must capture cur_zone_query");
            let live_query = runtime
                .machine()
                .retail_solid_query_cache()
                .expect("the state-17 movement keeps cur_zone_query initialized");
            assert_eq!(
                live_query, previous_query,
                "native reuses process-global cur_zone_query across the state-17 transition"
            );
            let translation = Vec3 {
                x: crash_vm.register(8).unwrap().cast_signed(),
                y: crash_vm.register(9).unwrap().cast_signed(),
                z: crash_vm.register(10).unwrap().cast_signed(),
            };
            assert!(
                live_query
                    .strictly_contains_event_probe(translation)
                    .unwrap(),
                "the state-17 event probe must take native's cached-query branch"
            );
            assert!(
                report.effects.iter().all(|effect| !matches!(
                    effect,
                    VmEffect::Solid { object, .. } if *object == crash.vm()
                )),
                "frame-193 STATUS_A is not the result of an inline solid-event handler"
            );
        }
        if crash_vm.state() != previous_crash_state {
            previous_crash_state = crash_vm.state();
            crash_state_trace.push((
                frame,
                crash_vm.state(),
                crash_vm.pc(),
                crash_vm.register(26).unwrap(),
                crash_vm.register(27).unwrap(),
            ));
        }
    }
    let crash_vm = runtime.machine().object(crash.vm()).unwrap();
    let final_translation = [
        crash_vm.register(8).unwrap().cast_signed(),
        crash_vm.register(9).unwrap().cast_signed(),
        crash_vm.register(10).unwrap().cast_signed(),
    ];
    assert_eq!(
        crash_state_trace,
        [
            (1, 34, 8, 0x0002_0800, 0x0404_2069),
            // State 17's authored, ineligible 0x87 opcode still clears
            // KEEP_EVENT_STACK before the condition check. The horizontal
            // movement then reuses cur_zone_query and records no floor hit.
            (193, 17, 1126, 0, 0x0404_20e8),
            (205, 19, 1245, 0x0006_0901, 0x0404_20e9),
            (210, 2, 499, 0x0006_0901, 0x0404_20e9),
        ],
        "N. Sanity entrance and first held-Right transition must match the native timeline"
    );
    assert_eq!(
        [
            crash_vm.register(17).unwrap().cast_signed(),
            crash_vm.register(18).unwrap().cast_signed(),
            crash_vm.register(19).unwrap().cast_signed(),
        ],
        [614_400, -136_000, 0],
        "held Right must reach the source terminal velocity on the settled floor"
    );
    assert_eq!(
        final_translation,
        [2_396_928, 1_374_720, 34_188_544],
        "the source wall clamp and solid floor response must remain deterministic"
    );
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn jungle_rollers_second_frame_matches_late_bound_range_golden() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name a legally local extracted stream directory"),
    );
    let level = LevelId::new_const(0x0c);
    let nsd_path = root.join(StreamName::new(level, StreamKind::Nsd).filename());
    let nsf_path = root.join(StreamName::new(level, StreamKind::Nsf).filename());
    let nsd_bytes = std::fs::read(&nsd_path).unwrap();
    let nsf_bytes = std::fs::read(&nsf_path).unwrap();
    let nsd = parse_nsd(&nsd_bytes, level).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
    let ldat = nsd.ldat().unwrap();
    assert_eq!(ldat.executable_map[22].name().as_deref(), Some("JunOC"));
    let graph = RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes).unwrap();
    let spawn_zone = graph.spawn_path().zone;
    let current_entry = nsf.resolve_entry(&nsd, spawn_zone).unwrap();
    let current_header =
        ZoneHeader::parse(current_entry.item(0).unwrap().bytes(&nsf_bytes).unwrap()).unwrap();
    let mut owned_neighbors = Vec::new();
    for eid in current_header.neighbors {
        let entry = nsf.resolve_entry(&nsd, eid).unwrap();
        let header = ZoneHeader::parse(entry.item(0).unwrap().bytes(&nsf_bytes).unwrap()).unwrap();
        let mut entities = Vec::new();
        for entity_index in 0..header.entity_count {
            let item_index =
                usize::try_from(header.entity_item_index(entity_index).unwrap()).unwrap();
            entities.push(
                ZoneEntity::parse(entry.item(item_index).unwrap().bytes(&nsf_bytes).unwrap())
                    .unwrap(),
            );
        }
        owned_neighbors.push((eid, header.display_flags | 3, entities));
    }
    let neighbors = owned_neighbors
        .iter()
        .map(|(eid, display_flags, entities)| NeighborZone {
            eid: *eid,
            display_flags: *display_flags,
            entities,
        })
        .collect::<Vec<_>>();
    let mut host = NsfProgramHost::new(&nsd, &nsf, &nsf_bytes);
    let mut runtime = RetailRuntime::new_for_level(256, level);
    let first = runtime
        .spawn_and_run_frame(&neighbors, &mut host, 256)
        .unwrap();
    let junoc = *first
        .spawn_attempts
        .iter()
        .find(|attempt| attempt.descriptor.executable == 22)
        .unwrap()
        .result
        .as_ref()
        .unwrap();
    let box_14 = *first
        .spawn_attempts
        .iter()
        .find(|attempt| attempt.descriptor.id == 14)
        .unwrap()
        .result
        .as_ref()
        .unwrap();
    let bound_entity_ids = |runtime: &RetailRuntime| {
        runtime
            .machine()
            .frame_bounds()
            .iter()
            .map(|bound| {
                let object = runtime.object_for_vm(bound.object).unwrap();
                match runtime.arena().get(object.arena()).unwrap().origin() {
                    ObjectOrigin::Entity(descriptor) => descriptor.id,
                    ObjectOrigin::Runtime { .. } => panic!("unexpected runtime-child bound"),
                }
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(bound_entity_ids(&runtime), [9, 12, 13, 14]);
    assert_eq!(
        runtime
            .machine()
            .object(box_14.vm())
            .unwrap()
            .register(process_register::STATUS_A)
            .unwrap()
            & 0x8000,
        0,
        "frame zero stamps match, so entity 14 takes the pre-physics bound path"
    );

    let second = runtime.run_frame(&mut host, 256).unwrap();

    assert!(
        second
            .executions
            .iter()
            .all(|execution| execution.result.is_ok()),
        "Jungle Rollers frame two must stay inside checked VM boundaries: {:?}",
        second
            .executions
            .iter()
            .filter(|execution| execution.result.is_err())
            .collect::<Vec<_>>()
    );
    assert_eq!(bound_entity_ids(&runtime), [9, 12, 13]);
    let main = runtime
        .arena()
        .main_object()
        .and_then(|arena| runtime.object_for_arena(arena))
        .unwrap();
    let main_vm = runtime.machine().object(main.vm()).unwrap();
    assert_eq!(
        (
            main_vm.register(process_register::ANIMATION_STAMP).unwrap(),
            main_vm.retail_transform().unwrap().translation,
        ),
        (1, [2_201_344, 1_041_870, 32_101_632])
    );
    let junoc_vm = runtime.machine().object(junoc.vm()).unwrap();
    assert_eq!(
        (
            junoc_vm
                .register(process_register::ANIMATION_STAMP)
                .unwrap(),
            junoc_vm.retail_transform().unwrap().translation,
            junoc_vm.register(process_register::STATUS_B).unwrap(),
        ),
        (1, [2_068_894, 1_252_821, 31_769_096], 0x7),
        "the named JunOC controller must retain its legal frame-two trace"
    );
    let box_vm = runtime.machine().object(box_14.vm()).unwrap();
    assert_eq!(
        (
            box_vm.register(process_register::ANIMATION_STAMP).unwrap(),
            box_vm.retail_transform().unwrap().translation,
            box_vm.register(process_register::STATUS_A).unwrap(),
        ),
        (1, [2_405_888, 947_456, 31_180_800], 0x000a_8001),
        "entity 14 is 920,832 units behind Crash on Z, beyond the 0x7d000 late-bound range"
    );
}
