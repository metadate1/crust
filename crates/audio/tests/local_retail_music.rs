use std::collections::HashMap;
use std::path::PathBuf;

use crust_audio::retail_music::RetailMusic;
use crust_audio::retail_player::{RetailMusicPlayer, RetailMusicState};
use crust_formats::binary::Eid;
use crust_formats::disc::{DiscImage, SectorLayout};
use crust_formats::stream::{
    EID_NONE_RAW, INST_ENTRY_TYPE, KNOWN_LEVELS, KnownLevel, MIDI_ENTRY_TYPE, StreamKind,
    StreamName, parse_instrument_entry, parse_nsd, parse_nsf, parse_retail_midi,
};

fn decode_pair(known: KnownLevel, nsd_bytes: &[u8], nsf_bytes: &[u8]) -> usize {
    let nsd = parse_nsd(nsd_bytes, known.id)
        .unwrap_or_else(|error| panic!("{} NSD: {error}", known.name));
    let nsf =
        parse_nsf(nsf_bytes, &nsd).unwrap_or_else(|error| panic!("{} NSF: {error}", known.name));
    let entries = nsf
        .entries()
        .map(|entry| (entry.eid, entry))
        .collect::<HashMap<Eid, _>>();

    let mut midi_entries = 0_usize;
    for entry in nsf
        .entries()
        .filter(|entry| entry.entry_type == MIDI_ENTRY_TYPE)
    {
        let midi = parse_retail_midi(entry, nsf_bytes)
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
                parse_instrument_entry(instrument, nsf_bytes)
                    .unwrap_or_else(|error| panic!("{} INST {eid}: {error}", known.name))
            })
            .collect::<Vec<_>>();
        let expected_programs = midi.vab.programs.len();
        let expected_sequences = midi.sep.sequences.len();
        let music = RetailMusic::decode(&midi, &fragments)
            .unwrap_or_else(|error| panic!("{} MIDI {}: {error}", known.name, entry.eid));
        assert_eq!(music.bank.program_count(), expected_programs);
        assert_eq!(music.sequences.len(), expected_sequences);
        let mut player = RetailMusicPlayer::new();
        player
            .start_immediate(entry.eid, music)
            .unwrap_or_else(|error| {
                panic!("{} MIDI {} playback owner: {error}", known.name, entry.eid)
            });
        let mut rendered = vec![0.0_f32; 2_048];
        player.render(&mut rendered);
        assert!(rendered.iter().all(|sample| sample.is_finite()));
        assert_eq!(player.state(), RetailMusicState::Primary);
        midi_entries += 1;
    }
    midi_entries
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn decodes_every_local_vab_body_and_sep_without_copying_assets() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let mut midi_entries = 0_usize;
    for known in KNOWN_LEVELS {
        let nsd_bytes = std::fs::read(root.join(known.nsd_filename()))
            .unwrap_or_else(|error| panic!("{} NSD: {error}", known.name));
        let nsf_bytes = std::fs::read(root.join(known.nsf_filename()))
            .unwrap_or_else(|error| panic!("{} NSF: {error}", known.name));
        midi_entries += decode_pair(known, &nsd_bytes, &nsf_bytes);
    }
    assert!(
        midi_entries > 0,
        "the local retail streams contained no MIDI entries"
    );
}

#[test]
#[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
fn decodes_and_starts_every_retail_midi_directly_from_local_disc() {
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

    let mut midi_entries = 0_usize;
    for known in KNOWN_LEVELS {
        let nsd_name = StreamName::new(known.id, StreamKind::Nsd);
        let nsf_name = StreamName::new(known.id, StreamKind::Nsf);
        let nsd_bytes = disc
            .read_stream(streams.get(nsd_name).expect("validated NSD is present"))
            .unwrap_or_else(|error| panic!("{} NSD extraction: {error}", known.name));
        let nsf_bytes = disc
            .read_stream(streams.get(nsf_name).expect("validated NSF is present"))
            .unwrap_or_else(|error| panic!("{} NSF extraction: {error}", known.name));
        midi_entries += decode_pair(known, &nsd_bytes, &nsf_bytes);
    }
    assert!(
        midi_entries > 0,
        "the local retail disc contained no MIDI entries"
    );
}
