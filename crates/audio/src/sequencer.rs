//! Small deterministic polyphonic software synthesizer and event sequencer.

use std::f32::consts::TAU;

use crate::mixer::SAMPLE_RATE;

pub const SYNTH_VOICES: usize = 64;
const MIDI_CHANNELS: u8 = 16;
const MIDI_NOTE_MAX: u8 = 127;
const SAMPLE_RATE_F32: f32 = 44_100.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Waveform {
    Sine,
    Triangle,
    Saw,
    Square,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventKind {
    NoteOn { channel: u8, note: u8, velocity: u8 },
    NoteOff { channel: u8, note: u8 },
    Program { channel: u8, program: u8 },
    Volume { channel: u8, value: u8 },
    Pan { channel: u8, value: u8 },
    Tempo { micros_per_quarter: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SequenceEvent {
    pub tick: u64,
    pub kind: EventKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sequence {
    pub ticks_per_quarter: u16,
    pub events: Vec<SequenceEvent>,
    pub loop_tick: Option<u64>,
}

impl Sequence {
    #[must_use]
    pub fn new(ticks_per_quarter: u16, mut events: Vec<SequenceEvent>) -> Self {
        events.sort_by_key(|event| event.tick);
        Self {
            ticks_per_quarter: ticks_per_quarter.max(1),
            events,
            loop_tick: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Channel {
    waveform: Waveform,
    volume: f32,
    pan: f32,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            waveform: Waveform::Triangle,
            volume: 0.7,
            pan: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct SynthVoice {
    channel: u8,
    note: u8,
    phase: f32,
    phase_step: f32,
    amplitude: f32,
    release: bool,
    age: u64,
}

#[derive(Debug)]
pub struct Sequencer {
    sequence: Option<Sequence>,
    channels: [Channel; 16],
    voices: Vec<SynthVoice>,
    event_index: usize,
    sample_clock: u64,
    tick_position: u64,
    tick_fraction: f64,
    micros_per_quarter: u32,
    playing: bool,
    age_clock: u64,
}

impl Default for Sequencer {
    fn default() -> Self {
        Self::new()
    }
}

impl Sequencer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            sequence: None,
            channels: [Channel::default(); 16],
            voices: Vec::with_capacity(SYNTH_VOICES),
            event_index: 0,
            sample_clock: 0,
            tick_position: 0,
            tick_fraction: 0.0,
            micros_per_quarter: 500_000,
            playing: false,
            age_clock: 0,
        }
    }

    pub fn load(&mut self, mut sequence: Sequence) {
        sequence.ticks_per_quarter = sequence.ticks_per_quarter.max(1);
        sequence.events.sort_by_key(|event| event.tick);
        self.sequence = Some(sequence);
        self.rewind();
    }

    pub fn rewind(&mut self) {
        self.channels = [Channel::default(); 16];
        self.voices.clear();
        self.event_index = 0;
        self.sample_clock = 0;
        self.tick_position = 0;
        self.tick_fraction = 0.0;
        self.micros_per_quarter = 500_000;
        self.age_clock = 0;
    }

    pub const fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
    }

    #[must_use]
    pub const fn is_playing(&self) -> bool {
        self.playing
    }

    #[must_use]
    pub fn active_voice_count(&self) -> usize {
        self.voices.len()
    }

    /// Render additive stereo samples into `destination`.
    pub fn render(&mut self, destination: &mut [f32]) {
        destination.fill(0.0);
        if !self.playing || self.sequence.is_none() {
            return;
        }
        let frame_count = destination.len() / 2;
        for frame in 0..frame_count {
            self.apply_loop();
            self.dispatch_due_events();
            // The final event defines the exclusive loop boundary. Dispatch it first, then apply
            // events at the loop start at that same musical instant.
            if self.apply_loop() {
                self.dispatch_due_events();
            }
            let mut left = 0.0_f32;
            let mut right = 0.0_f32;
            for voice in &mut self.voices {
                let channel = self.channels[usize::from(voice.channel)];
                let value = oscillator(channel.waveform, voice.phase) * voice.amplitude;
                voice.phase = (voice.phase + voice.phase_step).fract();
                if voice.release {
                    voice.amplitude *= 0.9992;
                }
                let left_gain = (1.0 - channel.pan).clamp(0.0, 1.0);
                let right_gain = (1.0 + channel.pan).clamp(0.0, 1.0);
                left += value * channel.volume * left_gain;
                right += value * channel.volume * right_gain;
            }
            self.voices.retain(|voice| voice.amplitude > 0.0005);
            destination[frame * 2] = left.clamp(-1.0, 1.0);
            destination[frame * 2 + 1] = right.clamp(-1.0, 1.0);
            self.sample_clock = self.sample_clock.saturating_add(1);
            self.advance_tick_position();
        }
    }

    fn ticks_per_sample(&self) -> f64 {
        let tpqn = self
            .sequence
            .as_ref()
            .map_or(1, |sequence| sequence.ticks_per_quarter);
        f64::from(tpqn) * 1_000_000.0
            / (f64::from(self.micros_per_quarter) * f64::from(SAMPLE_RATE))
    }

    fn advance_tick_position(&mut self) {
        self.tick_fraction += self.ticks_per_sample();
        let whole = whole_ticks(self.tick_fraction);
        self.tick_fraction -= f64::from(u16::try_from(whole).unwrap_or(u16::MAX));
        self.tick_position = self.tick_position.saturating_add(whole);
    }

    /// Apply a sequence loop after all events at the loop end have been dispatched. The loop end
    /// is the final event tick, matching the only end marker available in the compact sequence
    /// representation. Invalid and zero-length loop ranges remain one-shot.
    fn apply_loop(&mut self) -> bool {
        let Some(sequence) = self.sequence.as_ref() else {
            return false;
        };
        if self.event_index < sequence.events.len() {
            return false;
        }
        let Some(end) = sequence.events.last().map(|event| event.tick) else {
            return false;
        };
        let Some(start) = sequence.loop_tick.filter(|start| *start < end) else {
            return false;
        };
        if self.tick_position < end {
            return false;
        }
        let loop_len = end - start;
        let overshoot = self.tick_position - end;
        self.tick_position = start + overshoot % loop_len;
        self.event_index = sequence.events.partition_point(|event| event.tick < start);
        true
    }

    fn dispatch_due_events(&mut self) {
        while let Some(event) = self
            .sequence
            .as_ref()
            .and_then(|sequence| sequence.events.get(self.event_index))
            .copied()
        {
            if event.tick > self.tick_position {
                break;
            }
            self.event_index += 1;
            self.apply(event.kind);
        }
    }

    fn apply(&mut self, event: EventKind) {
        match event {
            EventKind::NoteOn {
                channel,
                note,
                velocity,
            } if velocity > 0 && channel < MIDI_CHANNELS && note <= MIDI_NOTE_MAX => {
                self.note_on(channel, note, velocity);
            }
            EventKind::NoteOn { channel, note, .. } | EventKind::NoteOff { channel, note }
                if channel < MIDI_CHANNELS && note <= MIDI_NOTE_MAX =>
            {
                self.note_off(channel, note);
            }
            EventKind::Program { channel, program } if channel < MIDI_CHANNELS => {
                self.channels[usize::from(channel)].waveform = match program & 3 {
                    0 => Waveform::Triangle,
                    1 => Waveform::Saw,
                    2 => Waveform::Square,
                    _ => Waveform::Sine,
                };
            }
            EventKind::Volume { channel, value } if channel < MIDI_CHANNELS => {
                self.channels[usize::from(channel)].volume = f32::from(value.min(127)) / 127.0;
            }
            EventKind::Pan { channel, value } if channel < MIDI_CHANNELS => {
                self.channels[usize::from(channel)].pan = (f32::from(value.min(127)) - 64.0) / 64.0;
            }
            EventKind::Tempo { micros_per_quarter } => {
                self.micros_per_quarter = micros_per_quarter.clamp(10_000, 10_000_000);
            }
            _ => {}
        }
    }

    fn note_on(&mut self, channel: u8, note: u8, velocity: u8) {
        if self.voices.len() >= SYNTH_VOICES {
            let oldest = self
                .voices
                .iter()
                .enumerate()
                .min_by_key(|(_, voice)| voice.age)
                .map_or(0, |(index, _)| index);
            self.voices.swap_remove(oldest);
        }
        self.age_clock = self.age_clock.saturating_add(1);
        let semitones = f32::from(note) - 69.0;
        let frequency = 440.0 * 2.0_f32.powf(semitones / 12.0);
        self.voices.push(SynthVoice {
            channel,
            note,
            phase: 0.0,
            phase_step: frequency / SAMPLE_RATE_F32,
            amplitude: f32::from(velocity.min(127)) / 127.0 * 0.18,
            release: false,
            age: self.age_clock,
        });
    }

    fn note_off(&mut self, channel: u8, note: u8) {
        for voice in &mut self.voices {
            if voice.channel == channel && voice.note == note {
                voice.release = true;
            }
        }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn whole_ticks(value: f64) -> u64 {
    debug_assert!(value.is_finite() && value >= 0.0);
    // With the public tempo and TPQN bounds this is at most 149 ticks per sample.
    value.floor() as u64
}

fn oscillator(waveform: Waveform, phase: f32) -> f32 {
    match waveform {
        Waveform::Sine => (phase * TAU).sin(),
        Waveform::Triangle => 1.0 - 4.0 * (phase - 0.5).abs(),
        Waveform::Saw => phase * 2.0 - 1.0,
        Waveform::Square => {
            if phase < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_synthesizes_and_releases_note() {
        let sequence = Sequence::new(
            60,
            vec![
                SequenceEvent {
                    tick: 0,
                    kind: EventKind::NoteOn {
                        channel: 0,
                        note: 69,
                        velocity: 100,
                    },
                },
                SequenceEvent {
                    tick: 1,
                    kind: EventKind::NoteOff {
                        channel: 0,
                        note: 69,
                    },
                },
            ],
        );
        let mut sequencer = Sequencer::new();
        sequencer.load(sequence);
        sequencer.set_playing(true);
        let mut output = vec![0.0; 4_096];
        sequencer.render(&mut output);
        assert!(output.iter().any(|sample| sample.abs() > 0.001));
        assert!(sequencer.active_voice_count() <= 1);
    }

    #[test]
    fn invalid_channel_is_ignored() {
        let sequence = Sequence::new(
            1,
            vec![SequenceEvent {
                tick: 0,
                kind: EventKind::NoteOn {
                    channel: 99,
                    note: 60,
                    velocity: 127,
                },
            }],
        );
        let mut sequencer = Sequencer::new();
        sequencer.load(sequence);
        sequencer.set_playing(true);
        let mut output = [1.0; 64];
        sequencer.render(&mut output);
        assert!(output.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn invalid_note_is_ignored() {
        let sequence = Sequence::new(
            1,
            vec![SequenceEvent {
                tick: 0,
                kind: EventKind::NoteOn {
                    channel: 0,
                    note: 200,
                    velocity: 127,
                },
            }],
        );
        let mut sequencer = Sequencer::new();
        sequencer.load(sequence);
        sequencer.set_playing(true);
        sequencer.render(&mut [0.0; 2]);
        assert_eq!(sequencer.active_voice_count(), 0);
    }

    #[test]
    fn rewind_resets_channel_controllers() {
        let sequence = Sequence::new(
            1,
            vec![SequenceEvent {
                tick: 0,
                kind: EventKind::Volume {
                    channel: 0,
                    value: 0,
                },
            }],
        );
        let mut sequencer = Sequencer::new();
        sequencer.load(sequence);
        sequencer.set_playing(true);
        sequencer.render(&mut [0.0; 2]);
        assert!(sequencer.channels[0].volume.abs() < f32::EPSILON);
        sequencer.rewind();
        assert!((sequencer.channels[0].volume - Channel::default().volume).abs() < f32::EPSILON);
    }

    #[test]
    fn loop_tick_replays_start_at_the_end_tick() {
        let mut sequence = Sequence::new(
            441,
            vec![
                SequenceEvent {
                    tick: 0,
                    kind: EventKind::Tempo {
                        micros_per_quarter: 10_000,
                    },
                },
                SequenceEvent {
                    tick: 1,
                    kind: EventKind::Volume {
                        channel: 0,
                        value: 0,
                    },
                },
                SequenceEvent {
                    tick: 3,
                    kind: EventKind::Volume {
                        channel: 0,
                        value: 127,
                    },
                },
            ],
        );
        sequence.loop_tick = Some(1);
        let mut sequencer = Sequencer::new();
        sequencer.load(sequence);
        sequencer.set_playing(true);

        // At this tempo, one output frame is exactly one sequence tick. The event at tick 3 and
        // the replayed event at tick 1 share the loop boundary; the latter therefore wins before
        // that output frame is synthesized.
        sequencer.render(&mut [0.0; 8]);
        assert!(sequencer.channels[0].volume.abs() < f32::EPSILON);
        assert_eq!(sequencer.tick_position, 2);
    }

    #[test]
    fn voice_count_is_bounded() {
        let events = (0..100)
            .map(|note| SequenceEvent {
                tick: 0,
                kind: EventKind::NoteOn {
                    channel: 0,
                    note,
                    velocity: 127,
                },
            })
            .collect();
        let mut sequencer = Sequencer::new();
        sequencer.load(Sequence::new(60, events));
        sequencer.set_playing(true);
        sequencer.render(&mut [0.0; 2]);
        assert_eq!(sequencer.active_voice_count(), SYNTH_VOICES);
    }
}
