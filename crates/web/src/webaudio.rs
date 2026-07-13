use crust_audio::mixer::{AudioMetrics, Mixer, SAMPLE_RATE, Sample};
use crust_audio::output::OutputOptions;
use crust_audio::retail::RetailAudioEngine;
use crust_audio::sequencer::{EventKind, Sequence, SequenceEvent, Sequencer};
use wasm_bindgen::JsValue;
use web_sys::{AudioContext, GainNode};

const CHUNK_FRAMES: usize = 1024;
const SCHEDULE_AHEAD_SECONDS: f64 = 0.12;

#[derive(Debug)]
pub struct WebAudio {
    context: AudioContext,
    gain: GainNode,
    mixer: Mixer,
    sequencer: Sequencer,
    output: OutputOptions,
    next_time: f64,
}

impl WebAudio {
    pub fn new(seed: u32) -> Result<Self, JsValue> {
        let context = AudioContext::new()?;
        let gain = context.create_gain()?;
        gain.connect_with_audio_node(&context.destination())?;
        gain.gain().set_value(0.78);
        let mut sequencer = Sequencer::new();
        sequencer.load(original_sequence(seed));
        sequencer.set_playing(true);
        Ok(Self {
            context,
            gain,
            mixer: Mixer::new(),
            sequencer,
            output: OutputOptions::new(u8::MAX, u8::MAX, false),
            next_time: 0.0,
        })
    }

    pub fn resume(&self) {
        let _ = self.context.resume();
    }

    pub fn set_muted(&self, muted: bool) {
        self.gain.gain().set_value(if muted { 0.0 } else { 0.78 });
    }

    pub const fn set_output_options(&mut self, options: OutputOptions) {
        self.output = options;
    }

    #[must_use]
    pub const fn output_options(&self) -> OutputOptions {
        self.output
    }

    pub fn tick_30_hz(&mut self) {
        self.mixer.tick_30_hz();
    }

    pub fn trigger_sfx(&mut self, pitch_seed: u8) {
        let frequency = 240.0 + f32::from(pitch_seed % 12) * 22.0;
        let mut samples = Vec::with_capacity(2_800);
        for index in 0_u16..2_800 {
            let index = f32::from(index);
            let t = index / sample_rate_f32();
            let envelope = (1.0 - index / 2_800.0).powi(2);
            let value = (t * frequency * std::f32::consts::TAU).sin() * envelope * 12_000.0;
            samples.push(sfx_sample(value));
        }
        let sample = Sample::new(samples, None);
        let voice = 1 + usize::from(pitch_seed) % 22;
        self.mixer.play(voice, sample, 13_000, 13_000, 4_096, 0, 1);
    }

    pub fn schedule(&mut self, retail_audio: &mut RetailAudioEngine) -> Result<(), JsValue> {
        let now = self.context.current_time();
        if self.next_time < now {
            self.next_time = now + 0.035;
        }
        while self.next_time < now + SCHEDULE_AHEAD_SECONDS {
            let mut music = vec![0.0_f32; CHUNK_FRAMES * 2];
            self.sequencer.render(&mut music);
            let mut sfx = vec![0_i16; CHUNK_FRAMES * 2];
            self.mixer.mix(&mut sfx);
            let mut retail_sfx = vec![0_i16; CHUNK_FRAMES * 2];
            retail_audio.mix(&mut retail_sfx);
            let mut left = vec![0.0_f32; CHUNK_FRAMES];
            let mut right = vec![0.0_f32; CHUNK_FRAMES];
            for frame in 0..CHUNK_FRAMES {
                let option_mixed = self.output.mix_frame(
                    [music[frame * 2], music[frame * 2 + 1]],
                    [
                        f32::from(sfx[frame * 2]) / 32_768.0,
                        f32::from(sfx[frame * 2 + 1]) / 32_768.0,
                    ],
                );
                // RetailAudioEngine applies the source `init_vol` when a
                // voice is created. Add that already-scaled bus after the
                // synthetic SFX option gain so it is not attenuated twice.
                let mixed = self.output.add_prescaled_sfx_frame(
                    option_mixed,
                    [
                        f32::from(retail_sfx[frame * 2]) / 32_768.0,
                        f32::from(retail_sfx[frame * 2 + 1]) / 32_768.0,
                    ],
                );
                left[frame] = mixed[0];
                right[frame] = mixed[1];
            }
            let buffer = self.context.create_buffer(
                2,
                u32::try_from(CHUNK_FRAMES).expect("chunk size fits u32"),
                sample_rate_f32(),
            )?;
            buffer.copy_to_channel(&left, 0)?;
            buffer.copy_to_channel(&right, 1)?;
            let source = self.context.create_buffer_source()?;
            source.set_buffer(Some(&buffer));
            source.connect_with_audio_node(&self.gain)?;
            source.start_with_when(self.next_time)?;
            self.next_time +=
                f64::from(u32::try_from(CHUNK_FRAMES).expect("audio chunk frame count fits u32"))
                    / f64::from(SAMPLE_RATE);
        }
        Ok(())
    }

    #[must_use]
    pub const fn metrics(&self) -> AudioMetrics {
        self.mixer.metrics()
    }
}

fn sample_rate_f32() -> f32 {
    f32::from(u16::try_from(SAMPLE_RATE).expect("44.1 kHz sample rate fits u16 exactly"))
}

#[allow(clippy::cast_possible_truncation)]
fn sfx_sample(value: f32) -> i16 {
    // The oscillator and [0, 1] envelope bound this value to +/-12,000, inside `i16`; truncation
    // matches the mixer sample quantization used by the existing implementation.
    value as i16
}

fn original_sequence(seed: u32) -> Sequence {
    const SCALE: [u8; 8] = [48, 51, 55, 58, 60, 63, 67, 70];
    let rotation = usize::try_from(seed & 7).unwrap_or(0);
    let mut events = Vec::with_capacity(512);
    for step in 0..128_u64 {
        let note = SCALE[(usize::try_from(step).unwrap_or(0) + rotation) % SCALE.len()];
        let tick = step * 30;
        events.push(SequenceEvent {
            tick,
            kind: EventKind::NoteOn {
                channel: 0,
                note,
                velocity: 74,
            },
        });
        events.push(SequenceEvent {
            tick: tick + 23,
            kind: EventKind::NoteOff { channel: 0, note },
        });
        if step % 4 == 0 {
            let bass = note.saturating_sub(12);
            events.push(SequenceEvent {
                tick,
                kind: EventKind::NoteOn {
                    channel: 1,
                    note: bass,
                    velocity: 58,
                },
            });
            events.push(SequenceEvent {
                tick: tick + 52,
                kind: EventKind::NoteOff {
                    channel: 1,
                    note: bass,
                },
            });
        }
    }
    events.insert(
        0,
        SequenceEvent {
            tick: 0,
            kind: EventKind::Program {
                channel: 0,
                program: 0,
            },
        },
    );
    events.insert(
        1,
        SequenceEvent {
            tick: 0,
            kind: EventKind::Program {
                channel: 1,
                program: 1,
            },
        },
    );
    Sequence::new(60, events)
}
