//! Retail option gains and final music/SFX bus mixing.

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

    fn assert_frame_close(actual: [f32; 2], expected: [f32; 2]) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!(
                (actual - expected).abs() <= 1.0e-6,
                "expected {expected}, got {actual}"
            );
        }
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
