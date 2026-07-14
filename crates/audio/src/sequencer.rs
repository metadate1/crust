//! Small deterministic polyphonic software synthesizer and event sequencer.

use std::f32::consts::TAU;

use crate::{
    mixer::{SAMPLE_RATE, Sample},
    spu_envelope::SpuAdsrEnvelope,
};

pub const SYNTH_VOICES: usize = 64;
const MIDI_CHANNELS: u8 = 16;
const MIDI_NOTE_MAX: u8 = 127;
const SAMPLE_RATE_F32: f32 = 44_100.0;
const PITCH_UNITS: u64 = 4_096;
const PITCH_RATIO_UNITS: u64 = 1_u64 << 32;
const CENT_RATIO_UP_Q32: u64 = 4_297_448_883;
const CENT_RATIO_DOWN_Q32: u64 = 4_292_487_142;
const SAMPLED_OUTPUT_GAIN: f32 = 0.32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Waveform {
    Sine,
    Triangle,
    Saw,
    Square,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventKind {
    NoteOn {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    NoteOff {
        channel: u8,
        note: u8,
    },
    PolyphonicPressure {
        channel: u8,
        note: u8,
        pressure: u8,
    },
    ChannelPressure {
        channel: u8,
        pressure: u8,
    },
    Program {
        channel: u8,
        program: u8,
    },
    Volume {
        channel: u8,
        value: u8,
    },
    Pan {
        channel: u8,
        value: u8,
    },
    Expression {
        channel: u8,
        value: u8,
    },
    Sustain {
        channel: u8,
        enabled: bool,
    },
    PitchBend {
        channel: u8,
        value: u16,
    },
    AllNotesOff {
        channel: u8,
    },
    AllSoundsOff {
        channel: u8,
    },
    ResetControllers {
        channel: u8,
    },
    /// A preserved but currently unsupported MIDI controller.
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
    },
    Tempo {
        micros_per_quarter: u32,
    },
    /// Defines a sequence boundary and releases any one-shot note tails.
    Marker,
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

/// One decoded VAB tone ready for allocation by the software sequencer.
///
/// Modulation fields are retained for later fidelity work. The two ADSR
/// registers drive an exact fixed-point SPU envelope during playback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SampleTone {
    pub sample: Sample,
    pub priority: u8,
    pub mode: u8,
    pub volume: u8,
    pub pan: u8,
    pub center_note: u8,
    pub pitch_shift: u8,
    pub note_min: u8,
    pub note_max: u8,
    pub vibrato_width: u8,
    pub vibrato_time: u8,
    pub portamento_width: u8,
    pub portamento_time: u8,
    pub pitch_bend_min: u8,
    pub pitch_bend_max: u8,
    pub adsr1: u16,
    pub adsr2: u16,
}

/// One sparse MIDI program in a decoded VAB bank.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SampleProgram {
    pub volume: u8,
    pub priority: u8,
    pub mode: u8,
    pub pan: u8,
    pub attribute: i16,
    pub tones: Vec<SampleTone>,
}

/// Decoded, proprietary-byte-free runtime representation of a VAB bank.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SampleBank {
    master_volume: u8,
    master_pan: u8,
    programs: [Option<SampleProgram>; 128],
}

impl SampleBank {
    #[must_use]
    pub fn new(master_volume: u8, master_pan: u8) -> Self {
        Self {
            master_volume: master_volume.min(127),
            master_pan: master_pan.min(127),
            programs: std::array::from_fn(|_| None),
        }
    }

    #[must_use]
    pub const fn master_volume(&self) -> u8 {
        self.master_volume
    }

    #[must_use]
    pub const fn master_pan(&self) -> u8 {
        self.master_pan
    }

    /// Installs one MIDI program. Invalid indices are rejected without
    /// disturbing the existing bank.
    #[must_use]
    pub fn set_program(&mut self, index: u8, program: SampleProgram) -> bool {
        let Some(slot) = self.programs.get_mut(usize::from(index)) else {
            return false;
        };
        *slot = Some(program);
        true
    }

    #[must_use]
    pub fn program(&self, index: u8) -> Option<&SampleProgram> {
        self.programs.get(usize::from(index))?.as_ref()
    }

    #[must_use]
    pub fn program_count(&self) -> usize {
        self.programs.iter().flatten().count()
    }
}

#[derive(Clone, Copy, Debug)]
struct Channel {
    waveform: Waveform,
    program: u8,
    volume: f32,
    expression: f32,
    pan: f32,
    sustain: bool,
    pitch_bend: u16,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            waveform: Waveform::Triangle,
            program: 0,
            volume: 0.7,
            expression: 1.0,
            pan: 0.0,
            sustain: false,
            pitch_bend: 8_192,
        }
    }
}

#[derive(Clone, Debug)]
enum VoiceSource {
    Oscillator {
        phase: f32,
        base_phase_step: f32,
        phase_step: f32,
    },
    Sampled {
        sample: Sample,
        position: u64,
        base_cents: i32,
        step: u64,
    },
}

#[derive(Clone, Debug)]
struct SynthVoice {
    channel: u8,
    note: u8,
    source: VoiceSource,
    amplitude: f32,
    pan: f32,
    priority: u8,
    bend_down_cents: u16,
    bend_up_cents: u16,
    spu_adsr: Option<SpuAdsrEnvelope>,
    release_factor: f32,
    release: bool,
    key_released: bool,
    finished: bool,
    age: u64,
}

#[derive(Debug)]
pub struct Sequencer {
    sequence: Option<Sequence>,
    sample_bank: Option<SampleBank>,
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
            sample_bank: None,
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

    /// Releases the loaded sequence, decoded sample bank, and every live
    /// voice. Browser mount owners use this at a level boundary so no retail
    /// PCM or playback cursor can leak into the next stream pair.
    pub fn clear(&mut self) {
        self.sequence = None;
        self.sample_bank = None;
        self.playing = false;
        self.rewind();
    }

    /// Selects decoded retail instruments for subsequent note events.
    /// Passing `None` restores the procedural oscillator fallback.
    pub fn set_sample_bank(&mut self, bank: Option<SampleBank>) {
        self.voices.clear();
        self.sample_bank = bank;
    }

    #[must_use]
    pub const fn sample_bank(&self) -> Option<&SampleBank> {
        self.sample_bank.as_ref()
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
                let Some(source_value) = next_voice_sample(&mut voice.source, channel.waveform)
                else {
                    voice.finished = true;
                    continue;
                };
                let envelope_gain = voice.spu_adsr.as_ref().map_or(1.0, SpuAdsrEnvelope::gain);
                let value = source_value * voice.amplitude * envelope_gain;
                if let Some(envelope) = &mut voice.spu_adsr {
                    envelope.tick();
                } else if voice.release {
                    voice.amplitude *= voice.release_factor;
                }
                let pan = (channel.pan + voice.pan).clamp(-1.0, 1.0);
                let left_gain = (1.0 - pan).clamp(0.0, 1.0);
                let right_gain = (1.0 + pan).clamp(0.0, 1.0);
                let controller_gain = channel.volume * channel.expression;
                left += value * controller_gain * left_gain;
                right += value * controller_gain * right_gain;
            }
            self.voices.retain(|voice| {
                !voice.finished
                    && voice
                        .spu_adsr
                        .as_ref()
                        .map_or_else(|| voice.amplitude > 0.0005, |envelope| !envelope.is_off())
            });
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
        // A SEP loop restarts its hardware voices. Retaining sample tails here
        // would add another layer on every cycle and eventually hit the voice
        // ceiling even for a single sustained note.
        self.voices.clear();
        if start == 0 {
            self.channels = [Channel::default(); 16];
            self.micros_per_quarter = 500_000;
        }
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
                let state = &mut self.channels[usize::from(channel)];
                state.program = program;
                state.waveform = match program & 3 {
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
            EventKind::Expression { channel, value } if channel < MIDI_CHANNELS => {
                self.channels[usize::from(channel)].expression = f32::from(value.min(127)) / 127.0;
            }
            EventKind::Sustain { channel, enabled } if channel < MIDI_CHANNELS => {
                self.set_sustain(channel, enabled);
            }
            EventKind::PitchBend { channel, value } if channel < MIDI_CHANNELS => {
                self.set_pitch_bend(channel, value.min(16_383));
            }
            EventKind::AllNotesOff { channel } if channel < MIDI_CHANNELS => {
                self.all_notes_off(channel);
            }
            EventKind::AllSoundsOff { channel } if channel < MIDI_CHANNELS => {
                self.voices.retain(|voice| voice.channel != channel);
            }
            EventKind::ResetControllers { channel } if channel < MIDI_CHANNELS => {
                self.reset_controllers(channel);
            }
            EventKind::Tempo { micros_per_quarter } => {
                self.micros_per_quarter = micros_per_quarter.clamp(10_000, 10_000_000);
            }
            EventKind::Marker => {
                self.release_all();
            }
            _ => {
                // Invalid fields and unsupported modulation events are inert.
            }
        }
    }

    fn note_on(&mut self, channel: u8, note: u8, velocity: u8) {
        if self.sample_bank.is_some() {
            self.note_on_sampled(channel, note, velocity);
            return;
        }
        self.age_clock = self.age_clock.saturating_add(1);
        let semitones = f32::from(note) - 69.0;
        let frequency = 440.0 * 2.0_f32.powf(semitones / 12.0);
        let base_phase_step = frequency / SAMPLE_RATE_F32;
        let pitch_bend = self.channels[usize::from(channel)].pitch_bend;
        let bend = bend_cents(pitch_bend, 200, 200);
        let phase_step = base_phase_step * pitch_ratio_f32(bend);
        self.push_voice(SynthVoice {
            channel,
            note,
            source: VoiceSource::Oscillator {
                phase: 0.0,
                base_phase_step,
                phase_step,
            },
            amplitude: f32::from(velocity.min(127)) / 127.0 * 0.18,
            pan: 0.0,
            priority: 64,
            bend_down_cents: 200,
            bend_up_cents: 200,
            spu_adsr: None,
            release_factor: 0.9992,
            release: false,
            key_released: false,
            finished: false,
            age: self.age_clock,
        });
    }

    fn note_on_sampled(&mut self, channel: u8, note: u8, velocity: u8) {
        let channel_state = self.channels[usize::from(channel)];
        let Some(bank) = self.sample_bank.as_ref() else {
            return;
        };
        let Some(program) = bank.program(channel_state.program) else {
            // A loaded VAB has no General MIDI fallback: an absent retail
            // program is intentionally silent.
            return;
        };
        let bank_gain = normalized_midi_gain(bank.master_volume);
        let program_gain = normalized_midi_gain(program.volume);
        let bank_pan = midi_pan(bank.master_pan);
        let program_pan = midi_pan(program.pan);
        let mut age = self.age_clock;
        let new_voices = program
            .tones
            .iter()
            .filter(|tone| (tone.note_min..=tone.note_max).contains(&note))
            .map(|tone| {
                age = age.saturating_add(1);
                let fine_tune = i32::from(tone.pitch_shift) * 99 / 127;
                let base_cents = (i32::from(note) - i32::from(tone.center_note)) * 100 - fine_tune;
                let bend_down_cents = pitch_bend_range(tone.pitch_bend_min);
                let bend_up_cents = pitch_bend_range(tone.pitch_bend_max);
                let bend = bend_cents(channel_state.pitch_bend, bend_down_cents, bend_up_cents);
                let amplitude = normalized_midi_gain(velocity)
                    * bank_gain
                    * program_gain
                    * normalized_midi_gain(tone.volume)
                    * SAMPLED_OUTPUT_GAIN;
                SynthVoice {
                    channel,
                    note,
                    source: VoiceSource::Sampled {
                        sample: tone.sample.clone(),
                        position: 0,
                        base_cents,
                        step: sample_step(base_cents.saturating_add(bend)),
                    },
                    amplitude,
                    pan: (bank_pan + program_pan + midi_pan(tone.pan)).clamp(-1.0, 1.0),
                    priority: tone.priority,
                    bend_down_cents,
                    bend_up_cents,
                    spu_adsr: Some(SpuAdsrEnvelope::new(tone.adsr1, tone.adsr2)),
                    release_factor: 1.0,
                    release: false,
                    key_released: false,
                    finished: false,
                    age,
                }
            })
            .collect::<Vec<_>>();
        self.age_clock = age;
        for voice in new_voices {
            self.push_voice(voice);
        }
    }

    fn push_voice(&mut self, voice: SynthVoice) {
        if self.voices.len() >= SYNTH_VOICES {
            let victim = self
                .voices
                .iter()
                .enumerate()
                .min_by_key(|(_, candidate)| (candidate.priority, candidate.age))
                .map_or(0, |(index, _)| index);
            if voice.priority < self.voices[victim].priority {
                return;
            }
            self.voices.swap_remove(victim);
        }
        self.voices.push(voice);
    }

    fn note_off(&mut self, channel: u8, note: u8) {
        let sustained = self.channels[usize::from(channel)].sustain;
        for voice in &mut self.voices {
            if voice.channel == channel && voice.note == note {
                voice.key_released = true;
                if !sustained {
                    begin_release(voice);
                }
            }
        }
    }

    fn set_sustain(&mut self, channel: u8, enabled: bool) {
        let state = &mut self.channels[usize::from(channel)];
        let was_enabled = state.sustain;
        state.sustain = enabled;
        if was_enabled && !enabled {
            for voice in &mut self.voices {
                if voice.channel == channel && voice.key_released {
                    begin_release(voice);
                }
            }
        }
    }

    fn all_notes_off(&mut self, channel: u8) {
        let sustained = self.channels[usize::from(channel)].sustain;
        for voice in &mut self.voices {
            if voice.channel == channel {
                voice.key_released = true;
                if !sustained {
                    begin_release(voice);
                }
            }
        }
    }

    fn release_all(&mut self) {
        for voice in &mut self.voices {
            voice.key_released = true;
            begin_release(voice);
        }
    }

    fn reset_controllers(&mut self, channel: u8) {
        let index = usize::from(channel);
        let program = self.channels[index].program;
        let waveform = self.channels[index].waveform;
        self.channels[index] = Channel {
            waveform,
            program,
            ..Channel::default()
        };
        self.set_pitch_bend(channel, 8_192);
        for voice in &mut self.voices {
            if voice.channel == channel && voice.key_released {
                begin_release(voice);
            }
        }
    }

    fn set_pitch_bend(&mut self, channel: u8, value: u16) {
        self.channels[usize::from(channel)].pitch_bend = value;
        for voice in &mut self.voices {
            if voice.channel != channel {
                continue;
            }
            let bend = bend_cents(value, voice.bend_down_cents, voice.bend_up_cents);
            match &mut voice.source {
                VoiceSource::Oscillator {
                    base_phase_step,
                    phase_step,
                    ..
                } => {
                    *phase_step = *base_phase_step * pitch_ratio_f32(bend);
                }
                VoiceSource::Sampled {
                    base_cents, step, ..
                } => {
                    *step = sample_step(base_cents.saturating_add(bend));
                }
            }
        }
    }
}

fn next_voice_sample(source: &mut VoiceSource, waveform: Waveform) -> Option<f32> {
    match source {
        VoiceSource::Oscillator {
            phase, phase_step, ..
        } => {
            let value = oscillator(waveform, *phase);
            *phase = (*phase + *phase_step).fract();
            Some(value)
        }
        VoiceSource::Sampled {
            sample,
            position,
            step,
            ..
        } => sampled_next(sample, position, *step),
    }
}

fn sampled_next(sample: &Sample, position: &mut u64, step: u64) -> Option<f32> {
    if sample.is_empty() {
        return None;
    }
    let mut index = usize::try_from(*position / PITCH_UNITS).ok()?;
    if index >= sample.len() {
        let start = sample.loop_start()?;
        let loop_samples = sample.len().checked_sub(start)?;
        if loop_samples == 0 {
            return None;
        }
        let start_units = u64::try_from(start).ok()?.checked_mul(PITCH_UNITS)?;
        let loop_units = u64::try_from(loop_samples).ok()?.checked_mul(PITCH_UNITS)?;
        *position = start_units + position.checked_sub(start_units)? % loop_units;
        index = usize::try_from(*position / PITCH_UNITS).ok()?;
    }
    let current = f32::from(sample.sample(index)?);
    let next_index = if index + 1 < sample.len() {
        index + 1
    } else {
        sample.loop_start().unwrap_or(index)
    };
    let next = f32::from(sample.sample(next_index)?);
    let fraction = f32::from(u16::try_from(*position % PITCH_UNITS).ok()?)
        / f32::from(u16::try_from(PITCH_UNITS).expect("pitch unit fits u16"));
    *position = position.saturating_add(step.max(1));
    Some((current + fraction * (next - current)) / 32_768.0)
}

fn normalized_midi_gain(value: u8) -> f32 {
    f32::from(value.min(127)) / 127.0
}

fn midi_pan(value: u8) -> f32 {
    (f32::from(value.min(127)) - 64.0) / 64.0
}

fn pitch_bend_range(value: u8) -> u16 {
    u16::try_from(u32::from(value.min(127)) * 1_200 / 127)
        .expect("a 12-semitone pitch-bend range fits u16")
}

fn bend_cents(value: u16, down: u16, up: u16) -> i32 {
    if value < 8_192 {
        let magnitude = u32::from(8_192 - value) * u32::from(down) / 8_192;
        -i32::try_from(magnitude).expect("a 1,200-cent bend fits i32")
    } else {
        let magnitude = u32::from(value - 8_192) * u32::from(up) / 8_191;
        i32::try_from(magnitude).expect("a 1,200-cent bend fits i32")
    }
}

fn sample_step(cents: i32) -> u64 {
    let ratio = pitch_ratio_q32(cents);
    let scaled = (u128::from(PITCH_UNITS) * u128::from(ratio) + (1_u128 << 31)) >> 32;
    u64::try_from(scaled).unwrap_or(u64::MAX).max(1)
}

fn pitch_ratio_q32(cents: i32) -> u64 {
    let mut exponent = cents.unsigned_abs();
    let mut base = if cents.is_negative() {
        CENT_RATIO_DOWN_Q32
    } else {
        CENT_RATIO_UP_Q32
    };
    let mut result = PITCH_RATIO_UNITS;
    while exponent != 0 {
        if exponent & 1 != 0 {
            result = fixed_mul_q32(result, base);
        }
        exponent >>= 1;
        if exponent != 0 {
            base = fixed_mul_q32(base, base);
        }
    }
    result
}

fn fixed_mul_q32(left: u64, right: u64) -> u64 {
    let rounded = (u128::from(left) * u128::from(right) + (1_u128 << 31)) >> 32;
    u64::try_from(rounded).unwrap_or(u64::MAX)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "the fixed-point pitch ratio is intentionally converted to the oscillator's f32 phase"
)]
fn pitch_ratio_f32(cents: i32) -> f32 {
    (pitch_ratio_q32(cents) as f64 / 4_294_967_296.0) as f32
}

fn begin_release(voice: &mut SynthVoice) {
    voice.release = true;
    if let Some(envelope) = &mut voice.spu_adsr {
        envelope.key_off();
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

    #[test]
    fn fixed_point_pitch_is_centered_and_octave_scaled() {
        assert_eq!(sample_step(0), PITCH_UNITS);
        assert!(sample_step(1_200).abs_diff(PITCH_UNITS * 2) <= 2);
        assert!(sample_step(-1_200).abs_diff(PITCH_UNITS / 2) <= 2);
        assert_eq!(bend_cents(0, 1_200, 1_200), -1_200);
        assert_eq!(bend_cents(8_192, 1_200, 1_200), 0);
        assert_eq!(bend_cents(16_383, 1_200, 1_200), 1_200);
    }

    #[test]
    fn sustain_defers_release_until_pedal_up() {
        let mut sequencer = Sequencer::new();
        sequencer.note_on(0, 60, 127);
        sequencer.set_sustain(0, true);
        sequencer.note_off(0, 60);
        assert!(sequencer.voices[0].key_released);
        assert!(!sequencer.voices[0].release);
        sequencer.set_sustain(0, false);
        assert!(sequencer.voices[0].release);
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "a zero hardware envelope must produce an exact silent sample"
    )]
    fn sampled_voice_applies_spu_adsr_before_mixing_each_frame() {
        let mut bank = SampleBank::new(127, 64);
        assert!(bank.set_program(
            0,
            SampleProgram {
                volume: 127,
                priority: 64,
                mode: 0,
                pan: 64,
                attribute: 0,
                tones: vec![SampleTone {
                    sample: Sample::new(vec![i16::MAX; 8], Some(0)),
                    priority: 64,
                    mode: 0,
                    volume: 127,
                    pan: 64,
                    center_note: 60,
                    pitch_shift: 0,
                    note_min: 0,
                    note_max: 127,
                    vibrato_width: 0,
                    vibrato_time: 0,
                    portamento_width: 0,
                    portamento_time: 0,
                    pitch_bend_min: 0,
                    pitch_bend_max: 0,
                    // Fast linear attack, fast exponential decay to 0x800,
                    // and fast linear release.
                    adsr1: 0,
                    adsr2: 0,
                }],
            }
        ));

        let mut sequencer = Sequencer::new();
        sequencer.set_sample_bank(Some(bank));
        sequencer.load(Sequence::new(
            60,
            vec![SequenceEvent {
                tick: 0,
                kind: EventKind::NoteOn {
                    channel: 0,
                    note: 60,
                    velocity: 127,
                },
            }],
        ));
        sequencer.set_playing(true);

        let mut left = Vec::new();
        let mut levels = Vec::new();
        for _ in 0..4 {
            let mut frame = [f32::NAN; 2];
            sequencer.render(&mut frame);
            left.push(frame[0]);
            levels.push(
                sequencer.voices[0]
                    .spu_adsr
                    .as_ref()
                    .expect("sampled voice has an ADSR envelope")
                    .level(),
            );
        }

        assert_eq!(left[0], 0.0, "key-on begins at a zero Q15 envelope");
        assert!(left[1] > left[0]);
        assert!(left[2] > left[1]);
        assert!(left[3] > left[2]);
        assert_eq!(levels, [14_336, 28_672, 32_767, 16_383]);

        sequencer.note_off(0, 60);
        assert_eq!(
            sequencer.voices[0]
                .spu_adsr
                .as_ref()
                .expect("sampled voice has an ADSR envelope")
                .phase(),
            crate::spu_envelope::SpuAdsrPhase::Release
        );
        sequencer.render(&mut [0.0; 2]);
        assert_eq!(sequencer.active_voice_count(), 0);
    }
}
