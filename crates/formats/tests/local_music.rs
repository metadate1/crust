use std::collections::HashMap;
use std::path::PathBuf;

use crust_formats::binary::Eid;
use crust_formats::stream::{
    EID_NONE_RAW, INST_ENTRY_TYPE, KNOWN_LEVELS, MIDI_ENTRY_TYPE, parse_instrument_entry,
    parse_nsd, parse_nsf, parse_retail_midi,
};

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn parses_every_local_midi_vab_sep_and_referenced_inst_without_copying_assets() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    for known in KNOWN_LEVELS {
        let nsd_bytes = std::fs::read(root.join(known.nsd_filename()))
            .unwrap_or_else(|error| panic!("{} NSD: {error}", known.name));
        let nsd = parse_nsd(&nsd_bytes, known.id)
            .unwrap_or_else(|error| panic!("{} NSD: {error}", known.name));
        let nsf_bytes = std::fs::read(root.join(known.nsf_filename()))
            .unwrap_or_else(|error| panic!("{} NSF: {error}", known.name));
        let nsf = parse_nsf(&nsf_bytes, &nsd)
            .unwrap_or_else(|error| panic!("{} NSF: {error}", known.name));
        let entries = nsf
            .entries()
            .map(|entry| (entry.eid, entry))
            .collect::<HashMap<Eid, _>>();

        for entry in nsf
            .entries()
            .filter(|entry| entry.entry_type == MIDI_ENTRY_TYPE)
        {
            let midi = parse_retail_midi(entry, &nsf_bytes)
                .unwrap_or_else(|error| panic!("{} MIDI {}: {error}", known.name, entry.eid));
            let fragments = midi
                .header
                .instruments
                .iter()
                .copied()
                .filter(|eid| eid.raw() != EID_NONE_RAW)
                .map(|eid| {
                    let instrument = entries.get(&eid).unwrap_or_else(|| {
                        panic!("{} MIDI {} misses INST {eid}", known.name, entry.eid)
                    });
                    assert_eq!(instrument.entry_type, INST_ENTRY_TYPE);
                    parse_instrument_entry(instrument, &nsf_bytes)
                        .unwrap_or_else(|error| panic!("{} INST {eid}: {error}", known.name))
                })
                .collect::<Vec<_>>();
            midi.vab
                .validate_fragments(&fragments)
                .unwrap_or_else(|error| {
                    panic!("{} MIDI {} VAB body: {error}", known.name, entry.eid)
                });
            assert!(!midi.sep.sequences.is_empty());
            assert!(
                midi.sep.sequences.len()
                    <= usize::try_from(midi.header.track_count)
                        .expect("parsed MIDI track count is nonnegative")
            );
        }
    }
}
