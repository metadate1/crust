//! Opt-in PBAK census against the user's legally local NTSC-U disc.

use std::path::PathBuf;

use crust_formats::{
    disc::{DiscImage, SectorLayout},
    stream::{
        KNOWN_LEVELS, PBAK_ENTRY_TYPE, PbakLayout, StreamKind, StreamName, load_pbak_entry,
        parse_nsd, parse_nsf,
    },
};

#[test]
#[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
fn parses_every_retail_pbak_entry_directly_from_local_disc() {
    let path = PathBuf::from(
        std::env::var_os("C1_DISC_IMAGE")
            .expect("C1_DISC_IMAGE must name a legally local NTSC-U raw BIN"),
    );
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()));
    let disc = DiscImage::open(&bytes)
        .unwrap_or_else(|error| panic!("could not open {}: {error}", path.display()));
    assert_eq!(disc.layout(), SectorLayout::RawMode2_2352);
    let streams = disc
        .discover_streams()
        .unwrap_or_else(|error| panic!("could not index {}: {error}", path.display()));
    streams.validate_complete_retail().unwrap();

    let mut total_frames = 0_usize;
    let mut census = Vec::new();
    for known in KNOWN_LEVELS {
        let nsd_name = StreamName::new(known.id, StreamKind::Nsd);
        let nsf_name = StreamName::new(known.id, StreamKind::Nsf);
        let nsd_bytes = disc
            .read_stream(streams.get(nsd_name).expect("validated NSD is present"))
            .unwrap_or_else(|error| panic!("{} NSD extraction: {error}", known.name));
        let nsf_bytes = disc
            .read_stream(streams.get(nsf_name).expect("validated NSF is present"))
            .unwrap_or_else(|error| panic!("{} NSF extraction: {error}", known.name));
        let metadata = parse_nsd(&nsd_bytes, known.id)
            .unwrap_or_else(|error| panic!("{} NSD parse: {error}", known.name));
        let nsf = parse_nsf(&nsf_bytes, &metadata)
            .unwrap_or_else(|error| panic!("{} NSF parse: {error}", known.name));

        for entry in nsf
            .entries()
            .filter(|entry| entry.entry_type == PBAK_ENTRY_TYPE)
        {
            let pbak = load_pbak_entry(entry, &nsf_bytes)
                .unwrap_or_else(|error| panic!("{} {} PBAK: {error}", known.name, entry.eid));
            if entry.eid.name().as_deref() == Some("pb0eB") {
                // These words retain their provenance as a recording from the
                // user's local disc. Physical replay classification only says
                // that a live pad can reproduce them exactly: every Boulders
                // word is 16-bit and contains no opposing direction pair.
                assert_eq!(pbak.frames.len(), 990);
                for (frame_index, frame) in pbak.frames.iter().enumerate() {
                    assert!(
                        u16::try_from(frame.held).is_ok(),
                        "Boulders PBAK frame {} exceeds a physical 16-bit pad word: {:#010x}",
                        frame_index + 1,
                        frame.held
                    );
                    assert_ne!(
                        frame.held & 0x5000,
                        0x5000,
                        "Boulders PBAK frame {} holds Up+Down: {:#06x}",
                        frame_index + 1,
                        frame.held
                    );
                    assert_ne!(
                        frame.held & 0xa000,
                        0xa000,
                        "Boulders PBAK frame {} holds Left+Right: {:#06x}",
                        frame_index + 1,
                        frame.held
                    );
                }
            }
            if entry.eid.name().as_deref() == Some("pb0fB") {
                // Upstream's late frames are byte-swapped-looking on the
                // owned NTSC-U disc, but native reads the same little-endian
                // words. Preserve both the first discontinuity and a later
                // u32 wrap; neither is a license to normalize the recording.
                assert_eq!(pbak.frames[830].ticks_elapsed.cast_unsigned(), 0x0001_f984);
                assert_eq!(pbak.frames[830].held, 0x0000_1000);
                assert_eq!(pbak.frames[831].ticks_elapsed.cast_unsigned(), 0xa7f9_0100);
                assert_eq!(pbak.frames[831].held, 0x0010_0000);
                assert_eq!(pbak.frames[833].ticks_elapsed.cast_unsigned(), 0xecf9_0100);
                assert_eq!(pbak.frames[834].ticks_elapsed.cast_unsigned(), 0x0ffa_0100);
                assert_eq!(pbak.frames[929].ticks_elapsed.cast_unsigned(), 0xde06_0200);
                assert_eq!(pbak.frames[930].ticks_elapsed.cast_unsigned(), 0x0107_0200);
            }
            total_frames += pbak.frame_count();
            census.push((
                known.id.get(),
                entry.eid.name().expect("legal PBAK EID is named"),
                pbak.save_state.level.get(),
                pbak.layout,
                pbak.frame_count(),
            ));
        }
    }

    assert_eq!(
        census,
        [
            (
                0x0a,
                "pb0aB".to_owned(),
                0x0a,
                PbakLayout::SpawnWords304,
                872
            ),
            (
                0x0c,
                "pb0cB".to_owned(),
                0x0c,
                PbakLayout::SpawnWords304,
                1_348
            ),
            (
                0x0e,
                "pb0eB".to_owned(),
                0x0e,
                PbakLayout::SpawnWords304,
                990
            ),
            (
                0x0f,
                "pb0fB".to_owned(),
                0x0f,
                PbakLayout::SpawnWords511,
                934
            ),
            (
                0x12,
                "pb0iB".to_owned(),
                0x12,
                PbakLayout::SpawnWords304,
                1_240
            ),
            (
                0x1c,
                "pb0sB".to_owned(),
                0x1c,
                PbakLayout::SpawnWords304,
                998
            ),
            (
                0x1d,
                "pb0tB".to_owned(),
                0x1d,
                PbakLayout::SpawnWords304,
                1_804
            ),
            (
                0x20,
                "pb0wB".to_owned(),
                0x20,
                PbakLayout::SpawnWords304,
                1_878
            ),
            (
                0x29,
                "pb0FB".to_owned(),
                0x29,
                PbakLayout::SpawnWords304,
                902
            ),
        ]
    );
    assert_eq!(total_frames, 10_966);
}
