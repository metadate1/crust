//! Opt-in PBAK caption-controller characterization against legally local data.
//!
//! The test reads the user's disc in memory and never writes extracted bytes.

use std::path::PathBuf;

use crust_formats::{
    disc::DiscImage,
    stream::{
        KNOWN_LEVELS, PBAK_ENTRY_TYPE, StreamKind, StreamName, load_pbak_entry, parse_nsd,
        parse_nsf,
    },
};
use crust_sim::{
    camera::RetailCameraLocation,
    gool::{GAME_STATE_GLOBAL, VmEffect},
    retail_frame::PathProgress,
    retail_runtime::{
        ISLAND_CAMERA_ROTATION_GLOBAL, NsfProgramHost, RetailDemoFinishOutcome,
        RetailLevelStateContext, RetailRuntime,
    },
};

const RETAIL_GLOBAL_WORDS: usize = 256;

#[test]
#[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
fn every_legal_pbak_pair_binds_the_native_caption_controller() {
    let disc_path = PathBuf::from(
        std::env::var_os("C1_DISC_IMAGE")
            .expect("C1_DISC_IMAGE must name a legally local NTSC-U raw BIN"),
    );
    let disc_bytes = std::fs::read(&disc_path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", disc_path.display()));
    let disc = DiscImage::open(&disc_bytes)
        .unwrap_or_else(|error| panic!("{}: {error}", disc_path.display()));
    let streams = disc.discover_streams().expect("could not discover streams");

    let mut recordings = 0_usize;
    for known in KNOWN_LEVELS {
        let nsd_name = StreamName::new(known.id, StreamKind::Nsd);
        let nsf_name = StreamName::new(known.id, StreamKind::Nsf);
        let nsd_bytes = disc
            .read_stream(
                streams
                    .get(nsd_name)
                    .unwrap_or_else(|| panic!("disc is missing {nsd_name}")),
            )
            .unwrap_or_else(|error| panic!("could not extract {nsd_name}: {error}"));
        let nsf_bytes = disc
            .read_stream(
                streams
                    .get(nsf_name)
                    .unwrap_or_else(|| panic!("disc is missing {nsf_name}")),
            )
            .unwrap_or_else(|error| panic!("could not extract {nsf_name}: {error}"));
        let metadata =
            parse_nsd(&nsd_bytes, known.id).unwrap_or_else(|error| panic!("{nsd_name}: {error}"));
        let nsf =
            parse_nsf(&nsf_bytes, &metadata).unwrap_or_else(|error| panic!("{nsf_name}: {error}"));
        let entries = nsf
            .entries()
            .filter(|entry| entry.entry_type == PBAK_ENTRY_TYPE)
            .collect::<Vec<_>>();
        if entries.is_empty() {
            continue;
        }
        assert_eq!(entries.len(), 1, "{nsf_name} has multiple PBAK entries");
        let header = load_pbak_entry(entries[0], &nsf_bytes)
            .unwrap_or_else(|error| panic!("{nsf_name} PBAK: {error}"));

        let mut runtime = RetailRuntime::new_for_level(RETAIL_GLOBAL_WORDS, known.id);
        let mut host = NsfProgramHost::new(&metadata, &nsf, &nsf_bytes);
        let caption = runtime
            .create_retail_demo_caption(header.save_state.zone, &mut host)
            .unwrap_or_else(|error| panic!("{nsf_name} caption controller: {error:?}"));
        let object = runtime
            .render_objects()
            .expect("caption render snapshot is valid")
            .into_iter()
            .find(|object| object.object == caption)
            .expect("caption controller remains live");
        assert_eq!((object.executable, object.subtype), (4, 8));
        assert!(object.program.is_some());
        assert_eq!(object.zone, crust_formats::binary::Eid::NONE);
        recordings += 1;
    }

    assert_eq!(recordings, 9);
}

#[test]
#[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
fn jungle_rollers_attract_completion_returns_through_the_live_caption() {
    const CAPTION_OBJECT_GLOBAL: usize = 76;
    const PBAK_STATE_GLOBAL: usize = 105;

    let disc_path = PathBuf::from(
        std::env::var_os("C1_DISC_IMAGE")
            .expect("C1_DISC_IMAGE must name a legally local NTSC-U raw BIN"),
    );
    let disc_bytes = std::fs::read(&disc_path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", disc_path.display()));
    let disc = DiscImage::open(&disc_bytes)
        .unwrap_or_else(|error| panic!("{}: {error}", disc_path.display()));
    let streams = disc.discover_streams().expect("could not discover streams");
    let level = crust_formats::stream::LevelId::new_const(0x0c);
    let nsd_name = StreamName::new(level, StreamKind::Nsd);
    let nsf_name = StreamName::new(level, StreamKind::Nsf);
    let nsd_bytes = disc
        .read_stream(streams.get(nsd_name).expect("disc is missing S0C.NSD"))
        .expect("could not extract S0C.NSD");
    let nsf_bytes = disc
        .read_stream(streams.get(nsf_name).expect("disc is missing S0C.NSF"))
        .expect("could not extract S0C.NSF");
    let metadata = parse_nsd(&nsd_bytes, level).expect("S0C.NSD is valid");
    let nsf = parse_nsf(&nsf_bytes, &metadata).expect("S0C.NSF is valid");
    let pbak_entry = nsf
        .entries()
        .find(|entry| entry.entry_type == PBAK_ENTRY_TYPE)
        .expect("Jungle Rollers has one attract recording");
    let pbak = load_pbak_entry(pbak_entry, &nsf_bytes).expect("pb0cB is valid");

    let mut runtime = RetailRuntime::new_for_level(RETAIL_GLOBAL_WORDS, level);
    let mut host = NsfProgramHost::new(&metadata, &nsf, &nsf_bytes);
    runtime.set_level_state_context(RetailLevelStateContext {
        location: RetailCameraLocation {
            path: crust_formats::stream::RetailPathId {
                zone: pbak.save_state.zone,
                index: pbak.save_state.path_index,
            },
            progress: PathProgress::ZERO,
        },
        graphics_flags: 0,
        box_count: pbak.save_state.box_count,
        checkpoint_id: -1,
        checkpoint_translation: [0; 3],
        first_spawn: false,
        active_neighbor_zones: Vec::new(),
    });
    runtime
        .create_retail_demo_caption(pbak.save_state.zone, &mut host)
        .expect("the authored caption controller materializes");
    runtime
        .set_global_word(PBAK_STATE_GLOBAL, 2)
        .expect("PBAK state is writable");

    for _ in 0..2 {
        let frame = runtime
            .run_frame(&mut host, 67)
            .expect("caption initialization executes");
        assert!(
            frame
                .executions
                .iter()
                .all(|execution| execution.result.is_ok()),
            "caption initialization failures: {:?}",
            frame
                .executions
                .iter()
                .filter(|execution| execution.result.is_err())
                .collect::<Vec<_>>()
        );
    }
    assert_ne!(
        runtime.global_word(CAPTION_OBJECT_GLOBAL).unwrap(),
        0,
        "the subtype-nine caption child must publish itself"
    );
    assert_eq!(
        runtime.global_word(ISLAND_CAMERA_ROTATION_GLOBAL),
        Ok(0),
        "the retail completion contract must not depend on global 64"
    );

    assert!(matches!(
        runtime.finish_retail_demo(&mut host),
        Ok(RetailDemoFinishOutcome::CaptionEvent { .. })
    ));
    assert_eq!(runtime.global_word(PBAK_STATE_GLOBAL), Ok(3));

    let mut transition = None;
    for _ in 0..16 {
        let frame = runtime
            .run_frame(&mut host, 67)
            .expect("caption return executes");
        assert!(
            frame
                .executions
                .iter()
                .all(|execution| execution.result.is_ok()),
            "caption return failures: {:?}",
            frame
                .executions
                .iter()
                .filter(|execution| execution.result.is_err())
                .collect::<Vec<_>>()
        );
        transition = frame.effects.iter().find_map(|effect| match effect {
            VmEffect::Transition(level) => Some(*level),
            _ => None,
        });
        if transition.is_some() {
            break;
        }
    }
    assert_eq!(transition, Some(0x19));
    assert_eq!(runtime.global_word(GAME_STATE_GLOBAL), Ok(0x600));
}
