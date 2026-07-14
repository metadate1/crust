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
use crust_sim::retail_runtime::{NsfProgramHost, RetailRuntime};

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
