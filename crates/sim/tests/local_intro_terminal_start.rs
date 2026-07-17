//! Opt-in Intro terminal-input characterization against legally local streams.
//!
//! The test reads the user's extracted stream pair in place and never writes
//! game bytes or derived assets.

use std::path::{Path, PathBuf};

use crust_formats::{
    binary::Eid,
    stream::{
        LevelId, Nsd, Nsf, RetailPathId, RetailZoneGraph, StreamKind, StreamName, ZoneEntity,
        ZoneHeader, parse_nsd, parse_nsf,
    },
};
use crust_sim::{
    camera::{GAME_STATE_CUTSCENE, RetailCameraEffect, RetailCameraInput, RetailCameraRuntime},
    gool::{GAME_STATE_GLOBAL, RetailPadSnapshot, RetailTransformVectorsCamera, VmEffect},
    object_arena::NeighborZone,
    player::PAD_START,
    retail_runtime::{NsfProgramHost, RetailLevelStateContext, RetailRuntime},
};

const GLOBAL_WORDS: usize = 256;
const INSTRUCTION_BUDGET: usize = 67;
const IDLE_TERMINAL_FRAMES: u32 = 64;

fn read_intro_pair(root: &Path) -> (Vec<u8>, Vec<u8>) {
    let read = |kind| {
        let path = root.join(StreamName::new(LevelId::INTRO, kind).filename());
        std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    };
    (read(StreamKind::Nsd), read(StreamKind::Nsf))
}

fn zone_header<'a>(nsd: &Nsd, nsf: &'a Nsf, nsf_bytes: &'a [u8], zone: Eid) -> ZoneHeader {
    let entry = nsf
        .resolve_entry(nsd, zone)
        .unwrap_or_else(|error| panic!("ZDAT {zone}: {error}"));
    let bytes = entry
        .item(0)
        .unwrap_or_else(|| panic!("ZDAT {zone} has no header"))
        .bytes(nsf_bytes)
        .unwrap_or_else(|error| panic!("ZDAT {zone} header bytes: {error}"));
    ZoneHeader::parse(bytes).unwrap_or_else(|error| panic!("ZDAT {zone} header: {error}"))
}

fn zone_entities(
    nsd: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    zone: Eid,
) -> (ZoneHeader, Vec<ZoneEntity>) {
    let entry = nsf
        .resolve_entry(nsd, zone)
        .unwrap_or_else(|error| panic!("ZDAT {zone}: {error}"));
    let header = zone_header(nsd, nsf, nsf_bytes, zone);
    let entities = (0..header.entity_count)
        .map(|entity_index| {
            let item_index = header
                .entity_item_index(entity_index)
                .and_then(|index| usize::try_from(index).ok())
                .unwrap_or_else(|| panic!("ZDAT {zone} entity {entity_index} item is absent"));
            let bytes = entry
                .item(item_index)
                .unwrap_or_else(|| panic!("ZDAT {zone} item {item_index} is absent"))
                .bytes(nsf_bytes)
                .unwrap_or_else(|error| panic!("ZDAT {zone} entity {entity_index}: {error}"));
            ZoneEntity::parse(bytes)
                .unwrap_or_else(|error| panic!("ZDAT {zone} entity {entity_index}: {error}"))
        })
        .collect();
    (header, entities)
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

fn install_camera_frame(
    runtime: &mut RetailRuntime,
    camera: &mut RetailCameraRuntime,
    graph: &RetailZoneGraph,
    nsd: &Nsd,
) {
    let before = camera.location();
    let step = camera
        .update(graph, RetailCameraInput::default())
        .expect("terminal automatic camera update is valid");
    assert_eq!(step.before, before);
    assert_eq!(step.after, before, "the authored terminal path must hold");
    assert_eq!(camera.location(), before);
    assert_eq!(
        step.effects,
        [RetailCameraEffect::GameStateWrite {
            value: GAME_STATE_CUTSCENE,
        }]
    );
    runtime
        .set_global_word(GAME_STATE_GLOBAL, GAME_STATE_CUTSCENE.cast_unsigned())
        .expect("terminal camera game-state write succeeds");

    let pose = camera.pose(graph).expect("terminal camera pose is valid");
    let rotation_xz = camera
        .rotation_xz(graph)
        .expect("terminal camera rotation is valid");
    let field_of_view = nsd.ldat().expect("Intro has LDAT").field_of_view;
    let live_game_state = runtime
        .global_word(GAME_STATE_GLOBAL)
        .expect("terminal game state remains readable")
        .cast_signed();
    camera.synchronize_game_state(live_game_state);
    runtime.latch_frame_context(live_game_state, rotation_xz);
    runtime.set_transform_vectors_camera(RetailTransformVectorsCamera::from_retail_pose(
        pose.translation,
        pose.rotation_yxz,
        projection(field_of_view),
    ));
}

fn install_pad(runtime: &mut RetailRuntime, held: u32, previous: u32) {
    runtime
        .set_pad_snapshot(
            0,
            RetailPadSnapshot {
                tapped: held & !previous,
                held,
                held_previous: previous,
                ..RetailPadSnapshot::default()
            },
        )
        .expect("pad zero exists");
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn start_first_tapped_after_intro_terminal_frame_requests_title() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let (nsd_bytes, nsf_bytes) = read_intro_pair(&root);
    let nsd = parse_nsd(&nsd_bytes, LevelId::INTRO).expect("Intro NSD parses");
    let nsf = parse_nsf(&nsf_bytes, &nsd).expect("Intro NSF parses");
    let graph = RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes).expect("Intro graph parses");

    let terminal_path = RetailPathId {
        zone: Eid::from_name("t1_UZ").expect("fixed terminal EID is valid"),
        index: 0,
    };
    let mut camera =
        RetailCameraRuntime::at_path(&graph, terminal_path, i32::MAX, GAME_STATE_CUTSCENE)
            .expect("Intro terminal path is present");
    let terminal = camera.location();
    assert!(
        graph
            .path(terminal.path)
            .expect("terminal path remains present")
            .neighbors
            .is_empty(),
        "this characterization requires the shipped no-link terminal"
    );

    let spawn_zone = graph.spawn_path().zone;
    let spawn_header = zone_header(&nsd, &nsf, &nsf_bytes, spawn_zone);
    let spawn_zones = spawn_header
        .neighbors
        .iter()
        .copied()
        .map(|zone| {
            let (header, entities) = zone_entities(&nsd, &nsf, &nsf_bytes, zone);
            (zone, header, entities)
        })
        .collect::<Vec<_>>();
    let terminal_graphics_flags = graph
        .zone(terminal.path.zone)
        .expect("terminal zone remains present")
        .graphics_flags;
    let mut runtime = RetailRuntime::new_for_level(GLOBAL_WORDS, LevelId::INTRO);
    runtime.set_level_state_context(RetailLevelStateContext {
        location: terminal,
        graphics_flags: terminal_graphics_flags,
        box_count: 0,
        checkpoint_id: -1,
        checkpoint_translation: [0; 3],
        first_spawn: true,
        active_neighbor_zones: spawn_header.neighbors.clone(),
    });
    let mut host = NsfProgramHost::new(&nsd, &nsf, &nsf_bytes);
    let neighbors = spawn_zones
        .iter()
        .map(|(zone, header, entities)| NeighborZone {
            eid: *zone,
            // Initial `LevelUpdate` activates each neighbor with low bits 0..2.
            display_flags: header.display_flags | 0x7,
            entities,
        })
        .collect::<Vec<_>>();
    let attempts = runtime.spawn_current_zone_neighbors(&neighbors, &mut host);
    assert!(
        attempts.iter().any(|attempt| attempt.result.is_ok()),
        "Intro spawn scan must bind at least one authored object: {attempts:?}",
    );
    let main = runtime
        .arena()
        .main_object()
        .and_then(|arena| runtime.object_for_arena(arena))
        .expect("Intro spawn creates its main controller");

    for _ in 0..IDLE_TERMINAL_FRAMES {
        runtime.set_frame_timing(34, 34);
        install_pad(&mut runtime, 0, 0);
        install_camera_frame(&mut runtime, &mut camera, &graph, &nsd);
        let frame = runtime
            .run_frame(&mut host, INSTRUCTION_BUDGET)
            .expect("idle Intro terminal GOOL frame runs");
        assert!(
            frame
                .effects
                .iter()
                .all(|effect| !matches!(effect, VmEffect::Transition(_))),
            "the terminal camera itself must not synthesize a level transition"
        );
    }

    assert_eq!(
        runtime
            .machine()
            .object(main.vm())
            .expect("main is live")
            .state(),
        15,
        "the idle Intro controller waits in its authored input state"
    );

    runtime.set_frame_timing(34, 34);
    install_pad(&mut runtime, PAD_START, 0);
    install_camera_frame(&mut runtime, &mut camera, &graph, &nsd);
    let start_frame = runtime
        .run_frame(&mut host, INSTRUCTION_BUDGET)
        .expect("terminal Start frame runs");
    assert!(start_frame.effects.iter().any(|effect| {
        matches!(effect, VmEffect::StateChanged { object, state: 16 } if *object == main.vm())
    }));

    let mut requested = start_frame.effects.iter().find_map(|effect| match effect {
        VmEffect::Transition(level) => Some(*level),
        _ => None,
    });
    let mut previous = PAD_START;
    for _ in 0..4 {
        if requested.is_some() {
            break;
        }
        runtime.set_frame_timing(34, 34);
        install_pad(&mut runtime, 0, previous);
        previous = 0;
        install_camera_frame(&mut runtime, &mut camera, &graph, &nsd);
        let frame = runtime
            .run_frame(&mut host, INSTRUCTION_BUDGET)
            .expect("post-Start Intro frame runs");
        requested = frame.effects.iter().find_map(|effect| match effect {
            VmEffect::Transition(level) => Some(*level),
            _ => None,
        });
    }

    assert_eq!(requested, Some(0x19), "authored Intro exit targets Title");
}
