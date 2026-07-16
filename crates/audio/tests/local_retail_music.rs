use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use crust_audio::retail_music::{RetailMusic, sequence_from_sep};
use crust_audio::retail_player::{RetailMusicPlayer, RetailMusicState};
use crust_audio::sequencer::EventKind;
use crust_formats::binary::Eid;
use crust_formats::disc::{DiscImage, SectorLayout};
use crust_formats::stream::{
    EID_NONE_RAW, INST_ENTRY_TYPE, KNOWN_LEVELS, KnownLevel, MIDI_ENTRY_TYPE, SepEventKind,
    StreamKind, StreamName, parse_instrument_entry, parse_nsd, parse_nsf, parse_retail_midi,
};

#[derive(Debug, Default, Eq, PartialEq)]
struct RetailAudioFeatureCensus {
    midi_entries: usize,
    sequences: usize,
    events: usize,
    tones: usize,
    tones_with_vibrato_width: usize,
    tones_with_vibrato_time: usize,
    tones_with_portamento_width: usize,
    tones_with_portamento_time: usize,
    polyphonic_pressure: usize,
    channel_pressure: usize,
    pitch_bend: usize,
    sequences_with_loop_start: usize,
    sequences_with_loop_end: usize,
    sequences_with_complete_loop: usize,
    converted_loop_starts: usize,
    converted_loop_ends: usize,
    controllers: BTreeMap<u8, usize>,
    loop_controller_values: BTreeMap<(u8, u8), usize>,
}

fn census_pair(
    known: KnownLevel,
    nsd_bytes: &[u8],
    nsf_bytes: &[u8],
    census: &mut RetailAudioFeatureCensus,
) {
    let nsd = parse_nsd(nsd_bytes, known.id)
        .unwrap_or_else(|error| panic!("{} NSD: {error}", known.name));
    let nsf =
        parse_nsf(nsf_bytes, &nsd).unwrap_or_else(|error| panic!("{} NSF: {error}", known.name));
    for entry in nsf
        .entries()
        .filter(|entry| entry.entry_type == MIDI_ENTRY_TYPE)
    {
        let midi = parse_retail_midi(entry, nsf_bytes)
            .unwrap_or_else(|error| panic!("{} MIDI {}: {error}", known.name, entry.eid));
        census.midi_entries += 1;
        census.sequences += midi.sep.sequences.len();
        for program in &midi.vab.programs {
            for tone in &program.tones {
                census.tones += 1;
                census.tones_with_vibrato_width += usize::from(tone.vibrato_width != 0);
                census.tones_with_vibrato_time += usize::from(tone.vibrato_time != 0);
                census.tones_with_portamento_width += usize::from(tone.portamento_width != 0);
                census.tones_with_portamento_time += usize::from(tone.portamento_time != 0);
            }
        }
        for sequence in &midi.sep.sequences {
            let has_start = sequence.events.iter().any(|event| {
                matches!(
                    event.kind,
                    SepEventKind::ControlChange {
                        controller: 99,
                        value: 20,
                        ..
                    }
                )
            });
            let has_end = sequence.events.iter().any(|event| {
                matches!(
                    event.kind,
                    SepEventKind::ControlChange {
                        controller: 99,
                        value: 30,
                        ..
                    }
                )
            });
            census.sequences_with_loop_start += usize::from(has_start);
            census.sequences_with_loop_end += usize::from(has_end);
            census.sequences_with_complete_loop += usize::from(has_start && has_end);
            for event in sequence_from_sep(sequence, true).events {
                match event.kind {
                    EventKind::LoopStart { .. } => census.converted_loop_starts += 1,
                    EventKind::LoopEnd { .. } => census.converted_loop_ends += 1,
                    _ => {}
                }
            }
        }
        for event in midi
            .sep
            .sequences
            .iter()
            .flat_map(|sequence| &sequence.events)
        {
            census.events += 1;
            match event.kind {
                SepEventKind::PolyphonicPressure { .. } => census.polyphonic_pressure += 1,
                SepEventKind::ChannelPressure { .. } => census.channel_pressure += 1,
                SepEventKind::PitchBend { .. } => census.pitch_bend += 1,
                SepEventKind::ControlChange {
                    controller, value, ..
                } => {
                    *census.controllers.entry(controller).or_default() += 1;
                    if matches!(controller, 6 | 99) {
                        *census
                            .loop_controller_values
                            .entry((controller, value))
                            .or_default() += 1;
                    }
                }
                SepEventKind::NoteOff { .. }
                | SepEventKind::NoteOn { .. }
                | SepEventKind::ProgramChange { .. }
                | SepEventKind::Tempo { .. }
                | SepEventKind::End => {}
            }
        }
    }
}

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
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn characterizes_retail_sequence_controls_and_tone_modulation() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let mut census = RetailAudioFeatureCensus::default();
    for known in KNOWN_LEVELS {
        let nsd_bytes = std::fs::read(root.join(known.nsd_filename()))
            .unwrap_or_else(|error| panic!("{} NSD: {error}", known.name));
        let nsf_bytes = std::fs::read(root.join(known.nsf_filename()))
            .unwrap_or_else(|error| panic!("{} NSF: {error}", known.name));
        census_pair(known, &nsd_bytes, &nsf_bytes, &mut census);
    }
    assert_eq!(
        census,
        RetailAudioFeatureCensus {
            midi_entries: 42,
            sequences: 64,
            events: 98_067,
            tones: 778,
            tones_with_vibrato_width: 0,
            tones_with_vibrato_time: 0,
            tones_with_portamento_width: 0,
            tones_with_portamento_time: 0,
            polyphonic_pressure: 0,
            channel_pressure: 0,
            pitch_bend: 78,
            sequences_with_loop_start: 4,
            sequences_with_loop_end: 4,
            sequences_with_complete_loop: 4,
            converted_loop_starts: 6,
            converted_loop_ends: 4,
            controllers: BTreeMap::from([(6, 6), (7, 242), (99, 10)]),
            loop_controller_values: BTreeMap::from([((6, 127), 6), ((99, 20), 6), ((99, 30), 4),]),
        }
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
