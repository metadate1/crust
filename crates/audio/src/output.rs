//! Retail option gains and final music/SFX bus mixing.

/// Full-scale value of the retail signed 32-bit master-volume accumulator.
pub const RETAIL_MASTER_VOLUME_MAX: i32 = 0x3fff;

/// Fade-out delta installed by retail's `MidiResetFadeStep` helper.
pub const RETAIL_MASTER_FADE_OUT_STEP: i32 = -682;

/// Browser-independent state for retail's whole-output master fade.
///
/// This is distinct from the MIDI cross-fade: the resulting gain applies to
/// both music and SFX. The signed values and update order mirror the original
/// 30 Hz audio update. In particular, requesting a fade only reinstalls the
/// step; it never restores the current volume.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailMasterFade {
    volume: i32,
    step: i32,
}

impl Default for RetailMasterFade {
    fn default() -> Self {
        Self::new()
    }
}

impl RetailMasterFade {
    /// Creates the full-volume, inactive retail master fade state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            volume: RETAIL_MASTER_VOLUME_MAX,
            step: 0,
        }
    }

    /// Reinstalls the retail fade-out step without changing current volume.
    pub const fn reset_step(&mut self) {
        self.step = RETAIL_MASTER_FADE_OUT_STEP;
    }

    /// Advances the master fade once at the cooperative 30 Hz update rate.
    pub fn tick_30_hz(&mut self) {
        if self.step == 0 {
            return;
        }

        if self.step < 0 && self.volume < self.step.wrapping_abs() {
            self.volume = 0;
            self.step = 0;
        }
        if self.step > 0 && RETAIL_MASTER_VOLUME_MAX.wrapping_sub(self.volume) < self.step {
            self.volume = RETAIL_MASTER_VOLUME_MAX;
            self.step = 0;
        }
        self.volume = self.volume.wrapping_add(self.step);
    }

    /// Current signed retail master-volume accumulator.
    #[must_use]
    pub const fn volume(self) -> i32 {
        self.volume
    }

    /// Current signed per-tick delta; zero means the fade is inactive.
    #[must_use]
    pub const fn step(self) -> i32 {
        self.step
    }

    /// Current whole-output gain normalized to `0.0..=1.0`.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "the private invariant restricts both values to exactly representable 14-bit integers"
    )]
    pub fn normalized_gain(self) -> f32 {
        self.volume as f32 / RETAIL_MASTER_VOLUME_MAX as f32
    }
}

/// Largest volume value stored by the retail options and save payload.
pub const RETAIL_VOLUME_MAX: u8 = u8::MAX;

/// Output policy derived from the retail SFX, music, and mono options.
///
/// This type intentionally does not contain a mute flag. Browser mute is an
/// orthogonal master-output policy and should be applied after [`Self::mix_frame`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputOptions {
    sfx_volume: u8,
    music_volume: u8,
    mono: bool,
}

impl OutputOptions {
    /// Creates an output policy from the exact `0..=255` retail option values.
    #[must_use]
    pub const fn new(sfx_volume: u8, music_volume: u8, mono: bool) -> Self {
        Self {
            sfx_volume,
            music_volume,
            mono,
        }
    }

    #[must_use]
    pub const fn sfx_volume(self) -> u8 {
        self.sfx_volume
    }

    #[must_use]
    pub const fn music_volume(self) -> u8 {
        self.music_volume
    }

    #[must_use]
    pub const fn mono(self) -> bool {
        self.mono
    }

    /// Normalized SFX-bus gain. Retail zero is silence and 255 is unity.
    #[must_use]
    pub fn sfx_gain(self) -> f32 {
        normalized_gain(self.sfx_volume)
    }

    /// Normalized music-bus gain. Retail zero is silence and 255 is unity.
    #[must_use]
    pub fn music_gain(self) -> f32 {
        normalized_gain(self.music_volume)
    }

    /// Mixes one interleaved-stereo frame from the music and SFX buses.
    ///
    /// Mono folds each source independently before applying its gain. All
    /// arithmetic uses `f64`, so averaging two extreme finite `f32` samples
    /// cannot overflow. NaN is treated as silence, infinities saturate, and the
    /// returned channels are always finite values in `-1.0..=1.0`.
    #[must_use]
    pub fn mix_frame(self, music: [f32; 2], sfx: [f32; 2]) -> [f32; 2] {
        let music = source_frame(music, self.mono);
        let sfx = source_frame(sfx, self.mono);
        let music_gain = f64::from(self.music_gain());
        let sfx_gain = f64::from(self.sfx_gain());
        [
            clipped_sample(music[0] * music_gain + sfx[0] * sfx_gain),
            clipped_sample(music[1] * music_gain + sfx[1] * sfx_gain),
        ]
    }

    /// Adds a source-scaled SFX frame after the ordinary option buses.
    ///
    /// Retail voices already apply `init_vol` when they are created, so the
    /// SFX option gain must not be applied to them a second time. Mono folding
    /// and final finite clipping still match [`Self::mix_frame`].
    #[must_use]
    pub fn add_prescaled_sfx_frame(self, mixed: [f32; 2], sfx: [f32; 2]) -> [f32; 2] {
        let sfx = source_frame(sfx, self.mono);
        [
            clipped_sample(finite_sample(mixed[0]) + sfx[0]),
            clipped_sample(finite_sample(mixed[1]) + sfx[1]),
        ]
    }
}

fn normalized_gain(volume: u8) -> f32 {
    f32::from(volume) / f32::from(RETAIL_VOLUME_MAX)
}

fn source_frame(frame: [f32; 2], mono: bool) -> [f64; 2] {
    let left = finite_sample(frame[0]);
    let right = finite_sample(frame[1]);
    if mono {
        let folded = (left + right) * 0.5;
        [folded, folded]
    } else {
        [left, right]
    }
}

fn finite_sample(sample: f32) -> f64 {
    if sample.is_nan() {
        0.0
    } else if sample.is_infinite() {
        if sample.is_sign_positive() {
            f64::MAX
        } else {
            f64::MIN
        }
    } else {
        f64::from(sample)
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "the sample is clamped to the exactly representable f32 unit range first"
)]
fn clipped_sample(sample: f64) -> f32 {
    if sample.is_nan() {
        0.0
    } else {
        sample.clamp(-1.0, 1.0) as f32
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "these contracts require exact zero, unity, pass-through, and clipping values"
)]
mod tests {
    use super::*;

    const COMPLETE_FADE_SEQUENCE: [i32; 25] = [
        15_701, 15_019, 14_337, 13_655, 12_973, 12_291, 11_609, 10_927, 10_245, 9_563, 8_881,
        8_199, 7_517, 6_835, 6_153, 5_471, 4_789, 4_107, 3_425, 2_743, 2_061, 1_379, 697, 15, 0,
    ];

    fn assert_frame_close(actual: [f32; 2], expected: [f32; 2]) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!(
                (actual - expected).abs() <= 1.0e-6,
                "expected {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn master_fade_matches_every_retail_update_through_zero() {
        let mut fade = RetailMasterFade::new();
        assert_eq!(fade.volume(), RETAIL_MASTER_VOLUME_MAX);
        assert_eq!(fade.step(), 0);
        assert_eq!(fade.normalized_gain(), 1.0);

        fade.reset_step();
        assert_eq!(fade.step(), RETAIL_MASTER_FADE_OUT_STEP);
        for expected in COMPLETE_FADE_SEQUENCE {
            fade.tick_30_hz();
            assert_eq!(fade.volume(), expected);
            assert_eq!(
                fade.step(),
                if expected == 0 {
                    0
                } else {
                    RETAIL_MASTER_FADE_OUT_STEP
                }
            );
            assert!(fade.normalized_gain().is_finite());
            assert!((0.0..=1.0).contains(&fade.normalized_gain()));
        }

        assert_eq!(fade.normalized_gain(), 0.0);
        fade.tick_30_hz();
        assert_eq!(fade.volume(), 0);
        assert_eq!(fade.step(), 0);
    }

    #[test]
    fn resetting_step_never_restores_or_double_advances_volume() {
        let mut fade = RetailMasterFade::new();
        fade.reset_step();
        for expected in COMPLETE_FADE_SEQUENCE.into_iter().take(7) {
            fade.tick_30_hz();
            assert_eq!(fade.volume(), expected);
        }

        let partial_volume = fade.volume();
        let partial_gain = fade.normalized_gain();
        fade.reset_step();
        fade.reset_step();
        assert_eq!(fade.volume(), partial_volume);
        assert_eq!(fade.normalized_gain(), partial_gain);
        assert_eq!(fade.step(), RETAIL_MASTER_FADE_OUT_STEP);

        fade.tick_30_hz();
        assert_eq!(
            fade.volume(),
            partial_volume.wrapping_add(RETAIL_MASTER_FADE_OUT_STEP)
        );
    }

    #[test]
    fn retriggering_at_silence_clamps_without_signed_underflow() {
        let mut fade = RetailMasterFade::new();
        fade.reset_step();
        for _ in COMPLETE_FADE_SEQUENCE {
            fade.tick_30_hz();
        }
        assert_eq!(fade.volume(), 0);

        fade.reset_step();
        assert_eq!(fade.volume(), 0);
        assert_eq!(fade.step(), RETAIL_MASTER_FADE_OUT_STEP);
        fade.tick_30_hz();
        assert_eq!(fade.volume(), 0);
        assert_eq!(fade.step(), 0);
        assert_eq!(fade.normalized_gain(), 0.0);
    }

    #[test]
    fn retail_boundary_comparisons_preserve_the_original_strictness() {
        let mut exact_step = RetailMasterFade {
            volume: RETAIL_MASTER_FADE_OUT_STEP.wrapping_abs(),
            step: RETAIL_MASTER_FADE_OUT_STEP,
        };
        exact_step.tick_30_hz();
        assert_eq!(exact_step.volume(), 0);
        assert_eq!(exact_step.step(), RETAIL_MASTER_FADE_OUT_STEP);
        exact_step.tick_30_hz();
        assert_eq!(exact_step.step(), 0);

        let mut below_step = RetailMasterFade {
            volume: RETAIL_MASTER_FADE_OUT_STEP.wrapping_abs() - 1,
            step: RETAIL_MASTER_FADE_OUT_STEP,
        };
        below_step.tick_30_hz();
        assert_eq!(below_step.volume(), 0);
        assert_eq!(below_step.step(), 0);
    }

    #[test]
    fn every_retail_volume_maps_to_a_finite_monotonic_gain() {
        let mut previous = -1.0;
        for volume in u8::MIN..=u8::MAX {
            let options = OutputOptions::new(volume, volume, false);
            let sfx = options.sfx_gain();
            let music = options.music_gain();
            assert!(sfx.is_finite() && music.is_finite());
            assert!((0.0..=1.0).contains(&sfx));
            assert_eq!(sfx, music);
            assert!(sfx > previous);
            previous = sfx;
        }
        assert_eq!(OutputOptions::new(0, 0, false).sfx_gain(), 0.0);
        assert_eq!(OutputOptions::new(255, 255, false).music_gain(), 1.0);
    }

    #[test]
    fn zero_music_leaves_sfx_unchanged() {
        let options = OutputOptions::new(255, 0, false);
        assert_eq!(
            options.mix_frame([0.75, -0.25], [0.125, -0.5]),
            [0.125, -0.5]
        );
    }

    #[test]
    fn zero_sfx_leaves_music_unchanged() {
        let options = OutputOptions::new(0, 255, false);
        assert_eq!(
            options.mix_frame([0.75, -0.25], [0.125, -0.5]),
            [0.75, -0.25]
        );
    }

    #[test]
    fn full_scale_preserves_stereo_channels_before_final_sum() {
        let options = OutputOptions::new(255, 255, false);
        assert_frame_close(options.mix_frame([0.2, -0.4], [0.3, 0.1]), [0.5, -0.3]);
    }

    #[test]
    fn mono_folds_each_source_without_overflow() {
        let options = OutputOptions::new(255, 255, true);
        assert_frame_close(options.mix_frame([0.8, -0.2], [-0.4, 0.2]), [0.2, 0.2]);
        assert_eq!(
            options.mix_frame([f32::MAX, f32::MAX], [0.0, 0.0]),
            [1.0, 1.0]
        );
        assert_eq!(
            options.mix_frame([f32::MAX, -f32::MAX], [0.0, 0.0]),
            [0.0, 0.0]
        );
    }

    #[test]
    fn silent_sources_and_zero_gains_are_silent() {
        let full = OutputOptions::new(255, 255, false);
        assert_eq!(full.mix_frame([0.0; 2], [0.0; 2]), [0.0; 2]);

        let silent = OutputOptions::new(0, 0, true);
        assert_eq!(silent.mix_frame([1.0; 2], [-1.0; 2]), [0.0; 2]);
    }

    #[test]
    fn prescaled_sfx_is_not_attenuated_twice() {
        let options = OutputOptions::new(64, 255, false);
        assert_eq!(
            options.add_prescaled_sfx_frame([0.25, -0.25], [0.5, 0.125]),
            [0.75, -0.125]
        );
    }

    #[test]
    fn prescaled_sfx_obeys_mono_and_final_clipping() {
        let options = OutputOptions::new(0, 0, true);
        assert_eq!(
            options.add_prescaled_sfx_frame([0.9, 0.9], [0.8, -0.2]),
            [1.0, 1.0]
        );
    }

    #[test]
    fn output_is_clipped_and_finite_for_extreme_or_nonfinite_input() {
        let options = OutputOptions::new(255, 255, false);
        let frames = [
            options.mix_frame([f32::MAX, -f32::MAX], [f32::MAX, -f32::MAX]),
            options.mix_frame([f32::INFINITY, f32::NEG_INFINITY], [f32::NAN, f32::NAN]),
        ];
        for frame in frames {
            assert!(frame.into_iter().all(f32::is_finite));
            assert!(
                frame
                    .into_iter()
                    .all(|sample| (-1.0..=1.0).contains(&sample))
            );
        }
        assert_eq!(frames[0], [1.0, -1.0]);
        assert_eq!(frames[1], [1.0, -1.0]);
    }
}
