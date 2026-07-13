//! Opt-in camera-path golden against the user's legally local retail streams.

use std::path::PathBuf;

use crust_formats::stream::{
    KNOWN_LEVELS, LevelId, RetailPathId, RetailZoneGraph, StreamKind, StreamName, parse_nsd,
    parse_nsf,
};
use crust_sim::Vec3;
use crust_sim::camera::{
    GAME_STATE_PLAYING, RetailCameraFollowInput, RetailCameraInput, RetailCameraOutcome,
    RetailCameraRuntime,
};
use crust_sim::retail_frame::PathProgress;

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn n_sanity_automatic_camera_matches_tick_and_skip_goldens() {
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
    let spawn = graph.spawn_path();
    assert_eq!(spawn.zone.raw(), 470_271_227);
    assert_eq!(spawn.index, 0);

    let terminal = RetailPathId {
        zone: spawn.zone,
        index: 2,
    };
    let mut camera = RetailCameraRuntime::new(&graph).unwrap();
    let mut save_handshakes = 0_usize;
    let mut path_crossings = 0_u32;
    for _ in 0..192 {
        let step = camera.update(&graph, RetailCameraInput::default()).unwrap();
        save_handshakes += step.effects.len();
        if let RetailCameraOutcome::AutoAdvanced {
            path_crossings: crossings,
            ..
        } = step.outcome
        {
            path_crossings += crossings;
        }
    }
    assert_eq!(
        camera.location().path,
        terminal,
        "72 + 38 + 41 + 41 automatic ticks should enter path two"
    );
    assert_eq!(camera.location().progress, PathProgress::ZERO);
    assert_eq!(path_crossings, 4);
    assert_eq!(save_handshakes, 4);

    let follow = camera.update(&graph, RetailCameraInput::default()).unwrap();
    assert_eq!(
        follow.outcome,
        RetailCameraOutcome::FollowBoundary { mode: 5 }
    );
    assert_eq!(follow.game_state, GAME_STATE_PLAYING);

    let mut skipped_camera = RetailCameraRuntime::new(&graph).unwrap();
    let skipped = skipped_camera
        .update(&graph, RetailCameraInput { tapped: 0xf0 })
        .unwrap();
    assert_eq!(skipped.after.path, terminal);
    assert_eq!(skipped.after.progress, PathProgress::ZERO);
    assert_eq!(
        skipped.outcome,
        RetailCameraOutcome::AutoAdvanced {
            skipped: true,
            path_crossings: 4,
        }
    );
    assert_eq!(skipped.effects.len(), 4);
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn n_sanity_follow_camera_projects_and_crosses_the_first_gameplay_path() {
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
    let first_follow = RetailPathId {
        zone: graph.spawn_path().zone,
        index: 2,
    };
    let zone = graph.zone(first_follow.zone).unwrap();
    let path = graph.path(first_follow).unwrap();
    assert_eq!(zone.origin, [5_599, 4_770, 128_509]);
    assert_eq!(path.camera_mode, 5);
    assert_eq!(path.average_node_distance, 40);
    assert_eq!(path.camera_zoom, 1_700);
    assert_eq!(path.direction, [0, 0, -4_096]);
    assert_eq!(path.points.len(), 43);
    assert!(path.points.iter().all(|point| point.rotation_x == 1));

    let input_for_point = |path_id: RetailPathId, point_index: usize| {
        let point_zone = graph.zone(path_id.zone).unwrap();
        let point = graph.path(path_id).unwrap().points[point_index];
        RetailCameraFollowInput {
            // For rot_x=1, rel_x=449 and rel_z=-1401 put the retail near
            // plane one fixed unit beyond the selected path point.
            player_translation: Vec3 {
                x: (point_zone.origin[0] + i32::from(point.x) + 449) << 8,
                y: ((point_zone.origin[1] + i32::from(point.y)) << 8) - 0x3e800,
                z: (point_zone.origin[2] + i32::from(point.z) - 1_401) << 8,
            },
            ..RetailCameraFollowInput::default()
        }
    };

    let mut camera =
        RetailCameraRuntime::at_path(&graph, first_follow, 0, GAME_STATE_PLAYING).unwrap();
    for point_index in 1..path.points.len() {
        let step = camera
            .update_follow(&graph, input_for_point(first_follow, point_index))
            .unwrap();
        let expected_progress = if point_index + 1 == path.points.len() {
            0x2aff
        } else {
            i32::try_from(point_index).unwrap() << 8
        };
        assert_eq!(step.after.path, first_follow);
        assert_eq!(step.after.progress.raw(), expected_progress);
    }

    let (next_path, link) = graph.resolve_neighbor(first_follow, 0).unwrap();
    assert_eq!(link.relation, 2);
    assert_eq!(link.goal, 1);
    assert_eq!(next_path.zone, first_follow.zone);
    assert_eq!(next_path.index, 5);
    let crossing = camera
        .update_follow(&graph, input_for_point(next_path, 1))
        .unwrap();
    assert_eq!(crossing.after.path, next_path);
    assert_eq!(crossing.after.progress.raw(), 0x100);
    assert_eq!(camera.follow_state().speed, 0x200);
    assert_eq!(
        crossing.outcome,
        RetailCameraOutcome::FollowEvaluated {
            mode: 5,
            candidate_count: 2,
            moved: true,
            crossed_path: true,
        }
    );
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn every_playable_retail_pair_builds_an_owned_camera_graph() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name a legally local extracted stream directory"),
    );
    let mut validated = 0_usize;
    for known in KNOWN_LEVELS.iter().filter(|known| known.bootable) {
        let nsd_path = root.join(known.nsd_filename());
        let nsf_path = root.join(known.nsf_filename());
        let nsd_bytes = std::fs::read(&nsd_path)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", nsd_path.display()));
        let nsf_bytes = std::fs::read(&nsf_path)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", nsf_path.display()));
        let nsd = parse_nsd(&nsd_bytes, known.id)
            .unwrap_or_else(|error| panic!("{} NSD: {error}", known.name));
        let nsf = parse_nsf(&nsf_bytes, &nsd)
            .unwrap_or_else(|error| panic!("{} NSF: {error}", known.name));
        let graph = RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes)
            .unwrap_or_else(|error| panic!("{} camera graph: {error}", known.name));
        assert!(graph.zone_count() > 0, "{} has no camera zones", known.name);
        assert!(graph.path_count() > 0, "{} has no camera paths", known.name);
        // The title/map pair is driven by the title state machine and never
        // enters the gameplay camera tick from its boot state.
        if known.id != LevelId::TITLE {
            let mut camera = RetailCameraRuntime::new(&graph)
                .unwrap_or_else(|error| panic!("{} initial camera: {error}", known.name));
            for tick in 0..300 {
                camera
                    .update(&graph, RetailCameraInput::default())
                    .unwrap_or_else(|error| {
                        panic!("{} camera tick {}: {error}", known.name, tick + 1)
                    });
            }
        }
        validated += 1;
    }
    assert_eq!(validated, 43, "retail catalog playable-pair count changed");
}
