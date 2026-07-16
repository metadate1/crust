//! Retail VAB/SEP decoding into the browser-independent software sequencer.
//!
//! Serialized INST waveform bodies are concatenated only long enough to split
//! and decode their ADPCM waves. The resulting runtime bank owns PCM samples,
//! not proprietary source bytes.

use std::fmt;

use crust_formats::{
    binary::FormatError,
    stream::{InstrumentFragment, RetailMidiAsset, SepEventKind, SepSequence, VabBank},
};

use crate::{
    mixer::Sample,
    sequencer::{EventKind, SampleBank, SampleProgram, SampleTone, Sequence, SequenceEvent},
};

/// A decoded VAB bank and all playable sequences from one retail MIDI entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetailMusic {
    pub bank: SampleBank,
    pub sequences: Vec<Sequence>,
}

impl RetailMusic {
    /// Decodes one parsed retail MIDI entry and its referenced INST bodies.
    ///
    /// Sequences loop from tick zero because the game opens retail SEP tracks
    /// in infinite-play mode. Call [`sequence_from_sep`] directly for one-shot
    /// tooling or characterization.
    ///
    /// # Errors
    ///
    /// Returns an error when INST fragments do not exactly cover the VAB wave
    /// table, temporary allocation fails, or an ADPCM wave cannot be decoded.
    pub fn decode(
        asset: &RetailMidiAsset,
        fragments: &[InstrumentFragment<'_>],
    ) -> Result<Self, RetailMusicError> {
        let bank = decode_sample_bank(&asset.vab, fragments)?;
        let sequences = asset
            .sep
            .sequences
            .iter()
            .map(|sequence| sequence_from_sep(sequence, true))
            .collect();
        Ok(Self { bank, sequences })
    }
}

/// Failures that remain after the serialized containers have been validated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RetailMusicError {
    Format(FormatError),
    TemporaryAllocation {
        bytes: usize,
    },
    EmptyDecodedWave {
        index: usize,
    },
    WaveRange {
        index: usize,
        offset: usize,
        length: usize,
        available: usize,
    },
    WaveTableCoverage {
        described: usize,
        available: usize,
    },
    InvalidWaveReference {
        program: u8,
        wave_index: u16,
        wave_count: usize,
    },
    InvalidProgramIndex {
        index: u8,
    },
}

impl fmt::Display for RetailMusicError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format(error) => write!(formatter, "{error}"),
            Self::TemporaryAllocation { bytes } => {
                write!(
                    formatter,
                    "could not allocate {bytes} temporary VAB body bytes"
                )
            }
            Self::EmptyDecodedWave { index } => {
                write!(formatter, "VAB waveform {index} decoded to no PCM samples")
            }
            Self::WaveRange {
                index,
                offset,
                length,
                available,
            } => write!(
                formatter,
                "VAB waveform {index} range {offset}+{length} exceeds {available} INST bytes"
            ),
            Self::WaveTableCoverage {
                described,
                available,
            } => write!(
                formatter,
                "VAB wave table describes {described} bytes but INST fragments contain {available}"
            ),
            Self::InvalidWaveReference {
                program,
                wave_index,
                wave_count,
            } => write!(
                formatter,
                "VAB program {program} references one-based waveform {wave_index}, but the bank has {wave_count}"
            ),
            Self::InvalidProgramIndex { index } => {
                write!(formatter, "VAB program index {index} is outside 0..128")
            }
        }
    }
}

impl std::error::Error for RetailMusicError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Format(error) => Some(error),
            _ => None,
        }
    }
}

impl From<FormatError> for RetailMusicError {
    fn from(error: FormatError) -> Self {
        Self::Format(error)
    }
}

/// Decodes a validated VAB and its exact set of external INST fragments.
///
/// # Errors
///
/// Returns an error for missing, duplicate, out-of-order-numbered, or
/// incorrectly sized INST fragments, allocation failure, or an empty decoded
/// waveform.
pub fn decode_sample_bank(
    vab: &VabBank,
    fragments: &[InstrumentFragment<'_>],
) -> Result<SampleBank, RetailMusicError> {
    vab.validate_fragments(fragments)?;

    let mut ordered = fragments.to_vec();
    ordered.sort_by_key(|fragment| fragment.part_index);
    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(vab.total_wave_bytes())
        .map_err(|_| RetailMusicError::TemporaryAllocation {
            bytes: vab.total_wave_bytes(),
        })?;
    for fragment in ordered {
        encoded.extend_from_slice(fragment.body);
    }

    let mut waves = Vec::new();
    waves.try_reserve_exact(vab.wave_sizes.len()).map_err(|_| {
        RetailMusicError::TemporaryAllocation {
            bytes: vab.wave_sizes.len().saturating_mul(size_of::<Sample>()),
        }
    })?;
    let mut wave_offset = 0_usize;
    for (index, &wave_size) in vab.wave_sizes.iter().enumerate() {
        let Some(end) = wave_offset.checked_add(wave_size) else {
            return Err(RetailMusicError::WaveRange {
                index,
                offset: wave_offset,
                length: wave_size,
                available: encoded.len(),
            });
        };
        let Some(bytes) = encoded.get(wave_offset..end) else {
            return Err(RetailMusicError::WaveRange {
                index,
                offset: wave_offset,
                length: wave_size,
                available: encoded.len(),
            });
        };
        let sample = Sample::from_adpcm(bytes);
        if sample.is_empty() {
            return Err(RetailMusicError::EmptyDecodedWave { index });
        }
        waves.push(sample);
        wave_offset = end;
    }
    if wave_offset != encoded.len() {
        return Err(RetailMusicError::WaveTableCoverage {
            described: wave_offset,
            available: encoded.len(),
        });
    }

    let mut bank = SampleBank::new(vab.master_volume, vab.master_pan);
    for program in &vab.programs {
        let tones = program
            .tones
            .iter()
            .map(|tone| {
                let Some(zero_based) = tone.wave_index.checked_sub(1) else {
                    return Err(RetailMusicError::InvalidWaveReference {
                        program: program.index,
                        wave_index: tone.wave_index,
                        wave_count: waves.len(),
                    });
                };
                let Some(sample) = waves.get(usize::from(zero_based)) else {
                    return Err(RetailMusicError::InvalidWaveReference {
                        program: program.index,
                        wave_index: tone.wave_index,
                        wave_count: waves.len(),
                    });
                };
                Ok(SampleTone {
                    sample: sample.clone(),
                    priority: tone.priority,
                    mode: tone.mode,
                    volume: tone.volume,
                    pan: tone.pan,
                    center_note: tone.center_note,
                    pitch_shift: tone.pitch_shift,
                    note_min: tone.note_min,
                    note_max: tone.note_max,
                    vibrato_width: tone.vibrato_width,
                    vibrato_time: tone.vibrato_time,
                    portamento_width: tone.portamento_width,
                    portamento_time: tone.portamento_time,
                    pitch_bend_min: tone.pitch_bend_min,
                    pitch_bend_max: tone.pitch_bend_max,
                    adsr1: tone.adsr1,
                    adsr2: tone.adsr2,
                })
            })
            .collect::<Result<Vec<_>, RetailMusicError>>()?;
        let decoded = SampleProgram {
            volume: program.volume,
            priority: program.priority,
            mode: program.mode,
            pan: program.pan,
            attribute: program.attribute,
            tones,
        };
        if !bank.set_program(program.index, decoded) {
            return Err(RetailMusicError::InvalidProgramIndex {
                index: program.index,
            });
        }
    }
    Ok(bank)
}

/// Converts one endian-neutral SEP sequence to the deterministic event model.
#[must_use]
pub fn sequence_from_sep(sequence: &SepSequence, looped: bool) -> Sequence {
    let mut events = Vec::with_capacity(sequence.events.len().saturating_add(1));
    events.push(SequenceEvent {
        tick: 0,
        kind: EventKind::Tempo {
            micros_per_quarter: sequence.initial_tempo,
        },
    });
    let (loop_starts, skipped) = collapse_sequence_loops(sequence);
    for (index, event) in sequence.events.iter().enumerate() {
        if let Some(repeat_count) = loop_starts[index] {
            events.push(SequenceEvent {
                tick: event.tick,
                kind: EventKind::LoopStart { repeat_count },
            });
        }
        if skipped[index] {
            continue;
        }
        let kind = match event.kind {
            SepEventKind::ControlChange {
                controller: 99,
                value: 30,
                ..
            } => {
                let next_tick = sequence
                    .events
                    .get(index.saturating_add(1))
                    .map_or(sequence.end_tick, |next| next.tick);
                EventKind::LoopEnd {
                    finite_delay_ticks: next_tick.saturating_sub(event.tick),
                }
            }
            kind => event_kind(kind),
        };
        events.push(SequenceEvent {
            tick: event.tick,
            kind,
        });
    }
    if let Some(repeat_count) = loop_starts[sequence.events.len()] {
        events.push(SequenceEvent {
            tick: sequence.end_tick,
            kind: EventKind::LoopStart { repeat_count },
        });
    }
    let mut result = Sequence::new(sequence.ticks_per_quarter, events);
    if looped && sequence.end_tick > 0 {
        result.loop_tick = Some(0);
    }
    result
}

/// Collapses libsnd's global NRPN state into explicit loop boundaries. NRPN
/// 20 saves the pointer to the immediately following event, while a later
/// controller-6 data entry supplies the count. Folding that count back onto
/// the saved boundary preserves interleaved events and makes the repeated
/// pass start immediately, as the source's indefinite-loop jump does.
fn collapse_sequence_loops(sequence: &SepSequence) -> (Vec<Option<u8>>, Vec<bool>) {
    let mut loop_starts = vec![None; sequence.events.len().saturating_add(1)];
    let mut skipped = vec![false; sequence.events.len()];
    let mut pending_start = None;
    for (index, event) in sequence.events.iter().enumerate() {
        match event.kind {
            SepEventKind::ControlChange {
                controller: 99,
                value: 20,
                ..
            } => {
                pending_start = Some(index.saturating_add(1));
                skipped[index] = true;
            }
            SepEventKind::ControlChange {
                controller: 99,
                value: 30,
                ..
            } => pending_start = None,
            SepEventKind::ControlChange {
                controller: 6,
                value,
                ..
            } => {
                if let Some(start) = pending_start.take() {
                    loop_starts[start] = Some(value);
                    skipped[index] = true;
                }
            }
            _ => {}
        }
    }
    (loop_starts, skipped)
}

fn event_kind(kind: SepEventKind) -> EventKind {
    match kind {
        SepEventKind::NoteOff { channel, note, .. }
        | SepEventKind::NoteOn {
            channel,
            note,
            velocity: 0,
        } => EventKind::NoteOff { channel, note },
        SepEventKind::NoteOn {
            channel,
            note,
            velocity,
        } => EventKind::NoteOn {
            channel,
            note,
            velocity,
        },
        SepEventKind::PolyphonicPressure {
            channel,
            note,
            pressure,
        } => EventKind::PolyphonicPressure {
            channel,
            note,
            pressure,
        },
        SepEventKind::ControlChange {
            channel,
            controller,
            value,
        } => controller_event(channel, controller, value),
        SepEventKind::ProgramChange { channel, program } => EventKind::Program { channel, program },
        SepEventKind::ChannelPressure { channel, pressure } => {
            EventKind::ChannelPressure { channel, pressure }
        }
        SepEventKind::PitchBend { channel, value } => EventKind::PitchBend { channel, value },
        SepEventKind::Tempo { micros_per_quarter } => EventKind::Tempo { micros_per_quarter },
        SepEventKind::End => EventKind::Marker,
    }
}

fn controller_event(channel: u8, controller: u8, value: u8) -> EventKind {
    match controller {
        7 => EventKind::Volume { channel, value },
        10 => EventKind::Pan { channel, value },
        11 => EventKind::Expression { channel, value },
        64 => EventKind::Sustain {
            channel,
            enabled: value >= 64,
        },
        120 => EventKind::AllSoundsOff { channel },
        121 => EventKind::ResetControllers { channel },
        123 => EventKind::AllNotesOff { channel },
        _ => EventKind::ControlChange {
            channel,
            controller,
            value,
        },
    }
}

#[cfg(test)]
mod tests {
    use crust_formats::{
        binary::Eid,
        stream::{Sep, SepEvent, VabBank, structs::MidiHeader},
    };
    use proptest::prelude::*;

    use super::*;
    use crate::sequencer::Sequencer;

    fn one_wave_vab() -> VabBank {
        const HEADER_LEN: usize = 32 + 128 * 16 + 16 * 32 + 2 + 255 * 2;
        let mut bytes = vec![0_u8; HEADER_LEN];
        bytes[..4].copy_from_slice(b"pBAV");
        bytes[4..8].copy_from_slice(&6_u32.to_le_bytes());
        bytes[12..16].copy_from_slice(
            &u32::try_from(HEADER_LEN + 16)
                .expect("small fixture length fits u32")
                .to_le_bytes(),
        );
        bytes[18..20].copy_from_slice(&1_u16.to_le_bytes());
        bytes[20..22].copy_from_slice(&1_u16.to_le_bytes());
        bytes[22..24].copy_from_slice(&1_u16.to_le_bytes());
        bytes[24] = 127;
        bytes[25] = 64;

        let program = 32;
        bytes[program] = 1;
        bytes[program + 1] = 127;
        bytes[program + 2] = 64;
        bytes[program + 4] = 64;

        let tone = 32 + 128 * 16;
        bytes[tone] = 64;
        bytes[tone + 2] = 127;
        bytes[tone + 3] = 64;
        bytes[tone + 4] = 60;
        bytes[tone + 6] = 0;
        bytes[tone + 7] = 127;
        bytes[tone + 12] = 127;
        bytes[tone + 13] = 127;
        bytes[tone + 18..tone + 20].copy_from_slice(&31_u16.to_le_bytes());
        bytes[tone + 22..tone + 24].copy_from_slice(&1_i16.to_le_bytes());

        let wave_table = 32 + 128 * 16 + 16 * 32;
        bytes[wave_table + 2..wave_table + 4].copy_from_slice(&2_u16.to_le_bytes());
        VabBank::parse(&bytes).unwrap()
    }

    fn simple_sequence(events: Vec<SepEvent>, end_tick: u64) -> SepSequence {
        SepSequence {
            number: 0,
            ticks_per_quarter: 480,
            initial_tempo: 500_000,
            time_signature_numerator: 4,
            time_signature_denominator: 2,
            unused: 0,
            events,
            end_tick,
        }
    }

    #[test]
    fn decodes_vab_wave_and_renders_selected_program() {
        let mut adpcm = [0x77_u8; 16];
        adpcm[0] = 0;
        adpcm[1] = 1;
        let fragment = InstrumentFragment::parse(&[0, 0, 0, 0, 16, 0, 0, 0], &adpcm).unwrap();
        let sequence = simple_sequence(
            vec![
                SepEvent {
                    tick: 0,
                    kind: SepEventKind::ProgramChange {
                        channel: 0,
                        program: 0,
                    },
                },
                SepEvent {
                    tick: 0,
                    kind: SepEventKind::NoteOn {
                        channel: 0,
                        note: 60,
                        velocity: 127,
                    },
                },
                SepEvent {
                    tick: 96,
                    kind: SepEventKind::End,
                },
            ],
            96,
        );
        let asset = RetailMidiAsset {
            header: MidiHeader {
                track_count: 1,
                sequence: Eid::from_raw(1),
                instruments: [Eid::from_raw(0x6396_347f); 7],
            },
            vab: one_wave_vab(),
            sep: Sep {
                version: 0,
                sequences: vec![sequence],
            },
        };
        let music = RetailMusic::decode(&asset, &[fragment]).unwrap();
        assert_eq!(music.bank.program_count(), 1);
        assert_eq!(music.sequences.len(), 1);

        let mut sequencer = Sequencer::new();
        sequencer.set_sample_bank(Some(music.bank));
        sequencer.load(music.sequences[0].clone());
        sequencer.set_playing(true);
        let mut output = [0.0_f32; 128];
        sequencer.render(&mut output);
        assert!(output.iter().any(|sample| sample.abs() > f32::EPSILON));
    }

    #[test]
    fn fragment_total_must_match_wave_table() {
        let body = [0_u8; 32];
        let fragment = InstrumentFragment {
            part_index: 0,
            body: &body,
        };
        let error = decode_sample_bank(&one_wave_vab(), &[fragment]).unwrap_err();
        assert!(matches!(error, RetailMusicError::Format(_)));
    }

    #[test]
    fn mutated_runtime_metadata_is_rejected_without_indexing_panics() {
        let mut adpcm = [0_u8; 16];
        adpcm[1] = 1;
        let fragment = InstrumentFragment {
            part_index: 0,
            body: &adpcm,
        };

        let mut bad_range = one_wave_vab();
        bad_range.wave_sizes[0] = usize::MAX;
        assert!(matches!(
            decode_sample_bank(&bad_range, &[fragment]),
            Err(RetailMusicError::WaveRange { .. })
        ));

        let mut bad_reference = one_wave_vab();
        bad_reference.programs[0].tones[0].wave_index = 0;
        assert!(matches!(
            decode_sample_bank(&bad_reference, &[fragment]),
            Err(RetailMusicError::InvalidWaveReference { .. })
        ));
    }

    #[test]
    fn conversion_preserves_timing_and_supported_controllers() {
        let sequence = simple_sequence(
            vec![
                SepEvent {
                    tick: 7,
                    kind: SepEventKind::ControlChange {
                        channel: 3,
                        controller: 64,
                        value: 127,
                    },
                },
                SepEvent {
                    tick: 9,
                    kind: SepEventKind::PitchBend {
                        channel: 3,
                        value: 12_000,
                    },
                },
                SepEvent {
                    tick: 11,
                    kind: SepEventKind::ControlChange {
                        channel: 3,
                        controller: 1,
                        value: 42,
                    },
                },
                SepEvent {
                    tick: 12,
                    kind: SepEventKind::End,
                },
            ],
            12,
        );
        let converted = sequence_from_sep(&sequence, true);
        assert_eq!(converted.loop_tick, Some(0));
        assert_eq!(converted.events[0].tick, 0);
        assert!(matches!(
            converted.events[1].kind,
            EventKind::Sustain {
                channel: 3,
                enabled: true
            }
        ));
        assert_eq!(
            converted.events[2].kind,
            EventKind::PitchBend {
                channel: 3,
                value: 12_000
            }
        );
        assert_eq!(
            converted.events[3].kind,
            EventKind::ControlChange {
                channel: 3,
                controller: 1,
                value: 42
            }
        );
        assert_eq!(converted.events.last().unwrap().kind, EventKind::Marker);
    }

    #[test]
    fn conversion_collapses_sony_nrpn_loop_markers_without_losing_other_controls() {
        let sequence = simple_sequence(
            vec![
                SepEvent {
                    tick: 2,
                    kind: SepEventKind::ControlChange {
                        channel: 4,
                        controller: 99,
                        value: 20,
                    },
                },
                SepEvent {
                    tick: 3,
                    kind: SepEventKind::ControlChange {
                        channel: 4,
                        controller: 7,
                        value: 80,
                    },
                },
                SepEvent {
                    tick: 4,
                    kind: SepEventKind::ControlChange {
                        channel: 4,
                        controller: 6,
                        value: 3,
                    },
                },
                SepEvent {
                    tick: 5,
                    kind: SepEventKind::NoteOn {
                        channel: 4,
                        note: 60,
                        velocity: 100,
                    },
                },
                SepEvent {
                    tick: 8,
                    kind: SepEventKind::ControlChange {
                        channel: 4,
                        controller: 99,
                        value: 30,
                    },
                },
                SepEvent {
                    tick: 9,
                    kind: SepEventKind::ControlChange {
                        channel: 4,
                        controller: 99,
                        value: 40,
                    },
                },
                SepEvent {
                    tick: 10,
                    kind: SepEventKind::ControlChange {
                        channel: 4,
                        controller: 6,
                        value: 7,
                    },
                },
                SepEvent {
                    tick: 11,
                    kind: SepEventKind::End,
                },
            ],
            11,
        );

        let converted = sequence_from_sep(&sequence, false);
        assert_eq!(
            converted.events,
            vec![
                SequenceEvent {
                    tick: 0,
                    kind: EventKind::Tempo {
                        micros_per_quarter: 500_000,
                    },
                },
                SequenceEvent {
                    tick: 3,
                    kind: EventKind::LoopStart { repeat_count: 3 },
                },
                SequenceEvent {
                    tick: 3,
                    kind: EventKind::Volume {
                        channel: 4,
                        value: 80,
                    },
                },
                SequenceEvent {
                    tick: 5,
                    kind: EventKind::NoteOn {
                        channel: 4,
                        note: 60,
                        velocity: 100,
                    },
                },
                SequenceEvent {
                    tick: 8,
                    kind: EventKind::LoopEnd {
                        finite_delay_ticks: 1,
                    },
                },
                SequenceEvent {
                    tick: 9,
                    kind: EventKind::ControlChange {
                        channel: 4,
                        controller: 99,
                        value: 40,
                    },
                },
                SequenceEvent {
                    tick: 10,
                    kind: EventKind::ControlChange {
                        channel: 4,
                        controller: 6,
                        value: 7,
                    },
                },
                SequenceEvent {
                    tick: 11,
                    kind: EventKind::Marker,
                },
            ]
        );
    }

    proptest! {
        #[test]
        fn controller_conversion_is_sorted_and_bounded(
            controls in prop::collection::vec((0_u64..10_000, 0_u8..16, 0_u8..128, 0_u8..128), 0..512)
        ) {
            let mut events = controls
                .into_iter()
                .map(|(tick, channel, controller, value)| SepEvent {
                    tick,
                    kind: SepEventKind::ControlChange {
                        channel,
                        controller,
                        value,
                    },
                })
                .collect::<Vec<_>>();
            events.push(SepEvent {
                tick: 10_000,
                kind: SepEventKind::End,
            });
            let converted = sequence_from_sep(&simple_sequence(events, 10_000), true);
            prop_assert_eq!(converted.loop_tick, Some(0));
            prop_assert!(converted.events.windows(2).all(|pair| pair[0].tick <= pair[1].tick));
            prop_assert!(converted.events.len() <= 514);
        }

        #[test]
        fn mutated_vab_metadata_never_panics(wave_size in any::<usize>(), wave_index in any::<u16>()) {
            let mut bank = one_wave_vab();
            bank.wave_sizes[0] = wave_size;
            bank.programs[0].tones[0].wave_index = wave_index;
            let body = [0_u8; 16];
            let fragment = InstrumentFragment {
                part_index: 0,
                body: &body,
            };
            let _ = decode_sample_bank(&bank, &[fragment]);
        }
    }
}
