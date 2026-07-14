//! Opt-in Ending lifecycle regression against the user's legally local streams.
//!
//! No game bytes or derived assets are written by this test. It follows the
//! browser's spawn -> camera -> GOOL frame order through the first authored
//! credits-object state-level return, then keeps running long enough to prove
//! that returned objects are reclaimed instead of filling the 97-object arena.

#![allow(clippy::too_many_lines)]

use std::{collections::BTreeMap, path::PathBuf};

use crust_formats::{
    binary::Eid,
    stream::{
        LevelId, Nsd, Nsf, RetailZoneGraph, StreamKind, StreamName, ZoneEntity, ZoneHeader,
        load_gool_state_program, parse_nsd, parse_nsf,
    },
};
use crust_sim::{
    Vec3,
    camera::{
        RetailCameraEffect, RetailCameraFollowInput, RetailCameraInput, RetailCameraLocation,
        RetailCameraRuntime,
    },
    gool::{
        CodeSegment, RetailPadSnapshot, RetailTransformVectorsCamera, VmEffect, process_register,
    },
    object_arena::{NeighborZone, ObjectOrigin},
    retail_runtime::{
        NsfProgramHost, RetailLevelStateContext, RetailRuntime, RuntimeObjectHandle,
        ZoneTerminationMode,
    },
    zone_lifecycle::{OrderedZoneLoadList, ZoneLifecycle, ZoneLifecycleZone, ZoneTransitionAction},
};

const GLOBAL_WORDS: usize = 256;
const INSTRUCTION_BUDGET: usize = 67;
const END_FRAME: u32 = 1_800;
const CREDITS_EXECUTABLE: u8 = 61;
const CREDITS_SUBTYPE: u8 = 3;
const RETURN_STATE: u16 = 1;
const RETURN_PC: usize = 53;
const RETURN_WORD: u32 = 0x8289_4000;
// The authored Ending population peaks at 82 during this window after
// reclamation. Keep eight spare pool slots as a regression margin; the broken
// lifecycle deterministically saturated all 97 slots by frame 1,437.
const MAX_BOUNDED_LIVE_OBJECTS: usize = 89;

#[derive(Debug)]
struct OwnedZone {
    eid: Eid,
    entities: Vec<ZoneEntity>,
}

fn read_pair(root: &std::path::Path) -> (Vec<u8>, Vec<u8>) {
    let read = |kind| {
        let path = root.join(StreamName::new(LevelId::ENDING, kind).filename());
        std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    };
    (read(StreamKind::Nsd), read(StreamKind::Nsf))
}

fn zone_catalog(
    nsd: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    graph: &RetailZoneGraph,
) -> (BTreeMap<Eid, OwnedZone>, ZoneLifecycle) {
    let mut zones = BTreeMap::new();
    let mut lifecycle_zones = Vec::with_capacity(graph.zone_count());
    for node in graph.zones() {
        let entry = nsf
            .resolve_entry(nsd, node.eid)
            .unwrap_or_else(|error| panic!("ZDAT {}: {error}", node.eid));
        let header = ZoneHeader::parse(
            entry
                .item(0)
                .unwrap_or_else(|| panic!("ZDAT {} has no header", node.eid))
                .bytes(nsf_bytes)
                .unwrap_or_else(|error| panic!("ZDAT {} header bytes: {error}", node.eid)),
        )
        .unwrap_or_else(|error| panic!("ZDAT {} header: {error}", node.eid));
        let entities = (0..header.entity_count)
            .map(|entity_index| {
                let item_index = header
                    .entity_item_index(entity_index)
                    .and_then(|index| usize::try_from(index).ok())
                    .unwrap_or_else(|| {
                        panic!("ZDAT {} entity {entity_index} item is absent", node.eid)
                    });
                ZoneEntity::parse(
                    entry
                        .item(item_index)
                        .unwrap_or_else(|| panic!("ZDAT {} item {item_index} is absent", node.eid))
                        .bytes(nsf_bytes)
                        .unwrap_or_else(|error| {
                            panic!("ZDAT {} entity {entity_index}: {error}", node.eid)
                        }),
                )
                .unwrap_or_else(|error| panic!("ZDAT {} entity {entity_index}: {error}", node.eid))
            })
            .collect();
        lifecycle_zones.push(ZoneLifecycleZone::new(
            node.eid,
            header.display_flags,
            header.neighbors,
            OrderedZoneLoadList::from(&header.load_list),
        ));
        zones.insert(
            node.eid,
            OwnedZone {
                eid: node.eid,
                entities,
            },
        );
    }

    let mut lifecycle = ZoneLifecycle::new(lifecycle_zones).expect("Ending zone lifecycle builds");
    lifecycle
        .transition_with_marker(graph.spawn_path().zone, true)
        .expect("Ending spawn zone activates");
    (zones, lifecycle)
}

fn refresh_level_context(
    runtime: &mut RetailRuntime,
    graph: &RetailZoneGraph,
    lifecycle: &ZoneLifecycle,
    location: RetailCameraLocation,
) {
    let graphics_flags = graph
        .zone(location.path.zone)
        .unwrap_or_else(|| panic!("camera zone {} is absent", location.path.zone))
        .graphics_flags;
    let previous = runtime.level_state_context().cloned();
    runtime.set_level_state_context(RetailLevelStateContext {
        location,
        graphics_flags,
        box_count: previous.as_ref().map_or(0, |state| state.box_count),
        checkpoint_id: previous.as_ref().map_or(-1, |state| state.checkpoint_id),
        checkpoint_translation: previous
            .as_ref()
            .map_or([0; 3], |state| state.checkpoint_translation),
        first_spawn: previous.as_ref().is_some_and(|state| state.first_spawn),
        active_neighbor_zones: lifecycle.active_neighbor_zones(),
    });
}

fn projection(field_of_view: u32) -> u32 {
    match field_of_view {
        30 => 960,
        37 => 800,
        55 => 500,
        60 => 460,
        90 => 288,
        _ => panic!("unsupported retail field of view {field_of_view}"),
    }
}

fn follow_input(runtime: &RetailRuntime) -> RetailCameraFollowInput {
    let arena = runtime
        .arena()
        .main_object()
        .expect("follow camera has a main object");
    let object = runtime
        .object_for_arena(arena)
        .expect("main arena object has a VM binding");
    let player = runtime
        .machine()
        .object(object.vm())
        .expect("main VM object remains live");
    let register = |index| {
        player
            .register(index)
            .unwrap_or_else(|error| panic!("main register {index}: {error:?}"))
            .cast_signed()
    };
    RetailCameraFollowInput {
        player_translation: Vec3 {
            x: register(process_register::TRANSLATION_X),
            y: register(process_register::TRANSLATION_Y),
            z: register(process_register::TRANSLATION_Z),
        },
        player_cam_zoom: register(process_register::CAMERA_ZOOM),
        held_buttons: 0,
        level_id: i32::try_from(LevelId::ENDING.get()).expect("Ending level ID fits i32"),
        frames_elapsed: runtime.machine().frames_elapsed(),
        gem_stamp: 0,
    }
}

fn update_camera(
    runtime: &mut RetailRuntime,
    host: &mut NsfProgramHost<'_>,
    nsd: &Nsd,
    graph: &RetailZoneGraph,
    camera: &mut RetailCameraRuntime,
    lifecycle: &mut ZoneLifecycle,
) {
    let location = camera.location();
    let mode = graph
        .path(location.path)
        .expect("active Ending camera path remains present")
        .camera_mode;
    let display_mask = runtime.current_display_mask();
    let step = if runtime.arena().main_object().is_none() || display_mask & (0x2 | 0x1_0000) != 0x2
    {
        camera.stationary_step()
    } else if matches!(mode, 5 | 6) {
        camera
            .update_follow(graph, follow_input(runtime))
            .expect("Ending follow-camera update succeeds")
    } else {
        camera
            .update(graph, RetailCameraInput::default())
            .expect("Ending automatic-camera update succeeds")
    };

    for effect in &step.effects {
        match *effect {
            RetailCameraEffect::LevelUpdate {
                before,
                after,
                flags,
            } => {
                if before.path.zone != after.path.zone {
                    let activation_marker = lifecycle.current_zone().is_none() || flags & 2 != 0;
                    let plan = lifecycle
                        .plan_transition_with_marker(after.path.zone, activation_marker)
                        .expect("Ending zone transition plans");
                    for action in plan.actions().iter().copied() {
                        if let ZoneTransitionAction::TerminateZoneObjects(zone) = action {
                            let report = runtime
                                .terminate_zone_objects(
                                    zone,
                                    ZoneTerminationMode::Departure {
                                        target: after.path.zone,
                                    },
                                    host,
                                )
                                .unwrap_or_else(|error| panic!("TERM {zone}: {error:?}"));
                            assert!(
                                report.event_failures.is_empty(),
                                "Ending TERM events must be clean: {:?}",
                                report.event_failures
                            );
                        }
                    }
                    lifecycle
                        .commit_transition(&plan)
                        .expect("Ending zone transition commits");
                }
                refresh_level_context(runtime, graph, lifecycle, after);
            }
            RetailCameraEffect::SaveStateHandshake { location } => {
                refresh_level_context(runtime, graph, lifecycle, location);
                let main = runtime
                    .arena()
                    .main_object()
                    .and_then(|arena| runtime.object_for_arena(arena))
                    .expect("Ending save handshake has a main object");
                runtime
                    .save_level_state(main, true)
                    .expect("Ending save handshake succeeds");
            }
        }
    }

    let rotation_xz = camera
        .rotation_xz(graph)
        .expect("Ending camera rotation is valid");
    let pose = camera.pose(graph).expect("Ending camera pose is valid");
    let field_of_view = nsd.ldat().expect("Ending has LDAT").field_of_view;
    runtime.set_frame_context(step.game_state, rotation_xz);
    runtime.set_transform_vectors_camera(RetailTransformVectorsCamera::from_retail_pose(
        pose.translation,
        pose.rotation_yxz,
        projection(field_of_view),
    ));
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn ending_state_level_returns_reclaim_credits_objects() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let (nsd_bytes, nsf_bytes) = read_pair(&root);
    let nsd = parse_nsd(&nsd_bytes, LevelId::ENDING).expect("Ending NSD parses");
    let nsf = parse_nsf(&nsf_bytes, &nsd).expect("Ending NSF parses");
    let graph =
        RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes).expect("Ending zone graph parses");
    let (zones, mut lifecycle) = zone_catalog(&nsd, &nsf, &nsf_bytes, &graph);

    let credits =
        nsd.ldat().expect("Ending has LDAT").executable_map[usize::from(CREDITS_EXECUTABLE)];
    assert_eq!(credits.name().as_deref(), Some("WinGC"));
    let return_state = load_gool_state_program(&nsd, &nsf, &nsf_bytes, credits, RETURN_STATE)
        .expect("WinGC return state loads");
    assert_eq!(return_state.code_pc(), Some(RETURN_PC));
    assert_eq!(return_state.code()[RETURN_PC], RETURN_WORD);

    let mut camera = RetailCameraRuntime::new(&graph).expect("Ending camera initializes");
    let mut runtime = RetailRuntime::new_for_level(GLOBAL_WORDS, LevelId::ENDING);
    refresh_level_context(&mut runtime, &graph, &lifecycle, camera.location());
    let mut host = NsfProgramHost::new(&nsd, &nsf, &nsf_bytes);
    runtime
        .create_retail_core_objects(camera.location().path.zone, &mut host)
        .expect("Ending core objects bind");
    runtime
        .create_retail_level_misc_object(camera.location().path.zone, &mut host)
        .expect("Ending level-misc object binds");

    let mut credit_child_spawns = 0_u32;
    let mut first_credit_child: Option<RuntimeObjectHandle> = None;
    let mut first_credit_spawn_frame = None;
    let mut saw_first_credit_reclaim = false;
    let mut max_live = runtime.arena().len();
    let mut max_generation = 1_u32;
    for frame in 1..=END_FRAME {
        runtime.set_frame_timing(34, 34);
        runtime
            .set_pad_snapshot(0, RetailPadSnapshot::default())
            .expect("pad zero exists");

        let neighbors = lifecycle
            .next_frame_spawn_scan()
            .iter()
            .map(|candidate| {
                let zone = zones
                    .get(&candidate.zone)
                    .unwrap_or_else(|| panic!("spawn zone {} is absent", candidate.zone));
                NeighborZone {
                    eid: zone.eid,
                    display_flags: candidate.display_flags,
                    entities: zone.entities.as_slice(),
                }
            })
            .collect::<Vec<_>>();
        let _attempts = runtime.spawn_current_zone_neighbors(&neighbors, &mut host);
        let _cleanup = runtime.take_cleanup_actions();
        assert!(runtime.take_reclaim_event_faults().is_empty());
        assert!(runtime.take_solid_event_faults().is_empty());

        runtime
            .advance_level_shader()
            .expect("Ending level shader advances");
        update_camera(
            &mut runtime,
            &mut host,
            &nsd,
            &graph,
            &mut camera,
            &mut lifecycle,
        );
        let report = runtime
            .run_frame(&mut host, INSTRUCTION_BUDGET)
            .unwrap_or_else(|error| panic!("Ending frame {frame}: {error:?}"));
        for execution in &report.executions {
            assert!(
                execution.result.is_ok(),
                "Ending frame {frame} object {:?}: {:?}",
                execution.object,
                execution.result
            );
        }
        for spawned in &report.spawned_children {
            let Some(arena) = runtime.arena().get(spawned.arena()) else {
                continue;
            };
            if matches!(
                arena.origin(),
                ObjectOrigin::Runtime {
                    executable: CREDITS_EXECUTABLE,
                    subtype: CREDITS_SUBTYPE,
                }
            ) && first_credit_child.is_none()
            {
                first_credit_child = Some(*spawned);
                first_credit_spawn_frame = Some(frame);
            }
        }
        for effect in &report.effects {
            if let VmEffect::SpawnChildren {
                executable,
                subtype,
                count,
                ..
            } = effect
                && (*executable, *subtype) == (CREDITS_EXECUTABLE, CREDITS_SUBTYPE)
            {
                credit_child_spawns = credit_child_spawns.saturating_add(*count);
            }
        }

        let _cleanup = runtime.take_cleanup_actions();
        assert!(runtime.take_reclaim_event_faults().is_empty());
        assert!(runtime.take_solid_event_faults().is_empty());
        let objects = runtime
            .render_objects()
            .unwrap_or_else(|error| panic!("Ending frame {frame} render objects: {error:?}"));
        max_live = max_live.max(objects.len());
        max_generation = max_generation.max(
            objects
                .iter()
                .map(|object| object.object.arena().generation())
                .max()
                .unwrap_or(1),
        );

        if let Some(first) = first_credit_child
            && runtime.arena().get(first.arena()).is_none()
        {
            saw_first_credit_reclaim = true;
            assert_ne!(
                runtime.object_for_vm(first.vm()),
                Some(first),
                "reclaim must remove the paired WinGC VM object; the compact VM slot may already be reused"
            );
            assert!(
                runtime.faulted_objects().all(|object| object != first),
                "native invalid initial return is a lifecycle signal, not a VM fault"
            );
        }

        let parked_returns = objects
            .iter()
            .filter(|object| {
                if (object.executable, object.subtype) != (CREDITS_EXECUTABLE, CREDITS_SUBTYPE) {
                    return false;
                }
                let vm = runtime
                    .machine()
                    .object(object.object.vm())
                    .expect("render object has a live VM");
                vm.state() == RETURN_STATE
                    && vm.code_address().segment == CodeSegment::External
                    && vm.code_address().pc == RETURN_PC + 1
            })
            .map(|object| object.object)
            .collect::<Vec<_>>();
        assert!(
            parked_returns.is_empty(),
            "frame {frame}: native kills WinGC state-level RETURN objects, but first child spawned at {:?} and these remained live at external PC {}: {parked_returns:?}",
            first_credit_spawn_frame,
            RETURN_PC + 1
        );
    }

    assert!(
        saw_first_credit_reclaim,
        "the bounded run must reclaim the first WinGC credits child"
    );
    assert!(
        credit_child_spawns >= 64,
        "the bounded run must exercise the authored credits-child stream; saw {credit_child_spawns} spawns"
    );
    assert!(
        max_generation > 1,
        "returned credits slots must be reclaimed and reused"
    );
    assert!(
        max_live <= MAX_BOUNDED_LIVE_OBJECTS,
        "credits lifecycle grew to {max_live} live objects instead of remaining bounded"
    );
    assert_eq!(runtime.faulted_object_count(), 0);
}
