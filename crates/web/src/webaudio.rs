use crust_audio::mixer::Mixer;
#[cfg(not(feature = "browser-test-harness"))]
use crust_audio::mixer::SAMPLE_RATE;
use crust_audio::output::OutputOptions;
use crust_audio::retail::RetailAudioEngine;
use crust_audio::retail_music::RetailMusic;
use crust_audio::retail_player::{
    RetailMusicChange, RetailMusicPlayer, RetailMusicPlayerError, RetailMusicState,
};
use crust_formats::binary::Eid;
use wasm_bindgen::JsValue;
use web_sys::{AudioContext, GainNode};

#[cfg(feature = "browser-test-harness")]
use crate::audio_output_metrics::FixedMillisecondSampleClock;
use crate::audio_output_metrics::ScheduledAudioMetrics;

#[cfg(not(feature = "browser-test-harness"))]
const CHUNK_FRAMES: usize = 1024;
#[cfg(not(feature = "browser-test-harness"))]
const SCHEDULE_AHEAD_SECONDS: f64 = 0.12;
const BROWSER_OUTPUT_GAIN: f32 = 0.78;

#[derive(Debug)]
pub struct WebAudio {
    context: AudioContext,
    gain: GainNode,
    mixer: Mixer,
    music: RetailMusicPlayer,
    output: OutputOptions,
    metrics: ScheduledAudioMetrics,
    muted: bool,
    retail_master_gain: f32,
    #[cfg(not(feature = "browser-test-harness"))]
    next_time: f64,
    #[cfg(feature = "browser-test-harness")]
    browser_test_sample_clock: FixedMillisecondSampleClock,
}

impl WebAudio {
    pub fn new() -> Result<Self, JsValue> {
        let context = AudioContext::new()?;
        let gain = context.create_gain()?;
        gain.connect_with_audio_node(&context.destination())?;
        gain.gain().set_value(BROWSER_OUTPUT_GAIN);
        Ok(Self {
            context,
            gain,
            mixer: Mixer::new(),
            music: RetailMusicPlayer::new(),
            output: OutputOptions::new(u8::MAX, u8::MAX, false),
            metrics: ScheduledAudioMetrics::default(),
            muted: false,
            retail_master_gain: 1.0,
            #[cfg(not(feature = "browser-test-harness"))]
            next_time: 0.0,
            #[cfg(feature = "browser-test-harness")]
            browser_test_sample_clock: FixedMillisecondSampleClock::default(),
        })
    }

    pub fn resume(&self) {
        let _ = self.context.resume();
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        self.refresh_master_gain();
    }

    pub fn set_retail_master_gain(&mut self, gain: f32) {
        self.retail_master_gain = gain.clamp(0.0, 1.0);
        self.refresh_master_gain();
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
        self.music.tick_30_hz();
    }

    pub fn start_retail_music(
        &mut self,
        eid: Eid,
        music: RetailMusic,
    ) -> Result<RetailMusicChange, RetailMusicPlayerError> {
        self.music.start_immediate(eid, music)
    }

    pub fn request_retail_music(
        &mut self,
        target: Option<(Eid, RetailMusic)>,
    ) -> Result<RetailMusicChange, RetailMusicPlayerError> {
        self.music.request(target)
    }

    pub fn toggle_retail_music(&mut self, value: u32) -> RetailMusicChange {
        self.music.toggle_secondary(value)
    }

    pub fn clear_retail_music(&mut self) -> RetailMusicChange {
        self.music.stop_immediate()
    }

    #[must_use]
    pub fn requested_retail_music_eid(&self) -> Option<Eid> {
        self.music.requested_eid()
    }

    #[must_use]
    pub const fn retail_music_state(&self) -> RetailMusicState {
        self.music.state()
    }

    #[cfg(not(feature = "browser-test-harness"))]
    pub fn schedule(&mut self, retail_audio: &mut RetailAudioEngine) -> Result<(), JsValue> {
        let now = self.context.current_time();
        if self.next_time < now {
            self.next_time = now + 0.035;
        }
        while self.next_time < now + SCHEDULE_AHEAD_SECONDS {
            let (left, right) = self.render_chunk(retail_audio, CHUNK_FRAMES);
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

    /// Advances every software audio bus by one deterministic 34 ms harness
    /// frame without queuing thousands of accelerated buffers into `WebAudio`.
    #[cfg(feature = "browser-test-harness")]
    pub fn schedule_browser_test_frame(&mut self, retail_audio: &mut RetailAudioEngine) {
        let frames = self.browser_test_sample_clock.next_frames(34);
        let _ = self.render_chunk(retail_audio, frames);
    }

    #[must_use]
    pub const fn metrics(&self) -> ScheduledAudioMetrics {
        self.metrics
    }

    fn render_chunk(
        &mut self,
        retail_audio: &mut RetailAudioEngine,
        frames: usize,
    ) -> (Vec<f32>, Vec<f32>) {
        let mut music = vec![0.0_f32; frames * 2];
        self.music.render(&mut music);
        let mut sfx = vec![0_i16; frames * 2];
        self.mixer.mix(&mut sfx);
        let mut retail_sfx = vec![0_i16; frames * 2];
        retail_audio.mix(&mut retail_sfx);
        let mut left = vec![0.0_f32; frames];
        let mut right = vec![0.0_f32; frames];
        for frame in 0..frames {
            let option_mixed = self.output.mix_frame(
                [music[frame * 2], music[frame * 2 + 1]],
                [
                    f32::from(sfx[frame * 2]) / 32_768.0,
                    f32::from(sfx[frame * 2 + 1]) / 32_768.0,
                ],
            );
            // RetailAudioEngine applies the source `init_vol` when a voice is
            // created. Add that already-scaled bus after the synthetic SFX
            // option gain so it is not attenuated twice.
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
        self.metrics.record_chunk(&left, &right);
        (left, right)
    }

    fn refresh_master_gain(&self) {
        self.gain.gain().set_value(if self.muted {
            0.0
        } else {
            BROWSER_OUTPUT_GAIN * self.retail_master_gain
        });
    }
}

#[cfg(not(feature = "browser-test-harness"))]
fn sample_rate_f32() -> f32 {
    f32::from(u16::try_from(SAMPLE_RATE).expect("44.1 kHz sample rate fits u16 exactly"))
}
