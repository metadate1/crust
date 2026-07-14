//! Opt-in end-to-end runtime bridge checks against the user's own retail data.

use std::{collections::BTreeMap, path::PathBuf};

use crust_formats::binary::{Eid, PageIndex};
use crust_formats::disc::DiscImage;
use crust_formats::stream::{
    LevelId, RetailZoneGraph, StreamKind, StreamName, ZoneEntity, ZoneHeader, ZoneRect, parse_nsd,
    parse_nsf,
};
use crust_sim::Angle12;
use crust_sim::gool::{RetailPadSnapshot, VmEffect};
use crust_sim::object_arena::{
    ENEMY_OBJECT_ROOT, MAIN_OBJECT_ROOT, NeighborZone, ObjectOrigin, TreeParent, ZONE_OBJECT_ROOT,
};
use crust_sim::retail_runtime::{NsfProgramHost, ProgramHost, RetailRuntime};
use crust_sim::zone_lifecycle::{
    OrderedZoneLoadList, SpawnScanZone, ZoneLifecycle, ZoneLifecycleZone, ZoneTransitionAction,
};

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
            (193, 17, 1126, 0x0006_0001, 0x0404_20e8),
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
