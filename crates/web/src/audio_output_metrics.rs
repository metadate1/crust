//! Browser-independent metrics for the stereo buffers scheduled to `WebAudio`.

#[cfg(any(test, feature = "browser-test-harness"))]
use crust_audio::mixer::SAMPLE_RATE;

/// Carries the fractional 44.1 kHz sample remainder across fixed-duration
/// browser-harness frames.
///
/// Production audio remains driven by `AudioContext.currentTime()`. The
/// accelerated manual harness cannot use that wall clock: identical simulated
/// frames would otherwise mix different sample counts depending on host speed,
/// changing retail voice completion and the shared RNG-B stream.
#[cfg(any(test, feature = "browser-test-harness"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct FixedMillisecondSampleClock {
    thousandth_sample_remainder: u32,
}

#[cfg(any(test, feature = "browser-test-harness"))]
impl FixedMillisecondSampleClock {
    pub(crate) fn next_frames(&mut self, duration_ms: u32) -> usize {
        let numerator = u64::from(self.thousandth_sample_remainder)
            + u64::from(SAMPLE_RATE) * u64::from(duration_ms);
        self.thousandth_sample_remainder =
            u32::try_from(numerator % 1_000).expect("sample remainder is below 1000");
        usize::try_from(numerator / 1_000).expect("one fixed-duration audio frame count fits usize")
    }
}

/// Converts one final software-mixed planar stereo chunk to deterministic
/// interleaved signed 16-bit little-endian PCM for the browser test harness.
///
/// This sits beside the fixed sample clock so native unit tests can pin the
/// capture format without compiling the WebAudio-only module.
#[cfg(any(test, feature = "browser-test-harness"))]
pub(crate) fn interleaved_pcm_s16le(left: &[f32], right: &[f32]) -> Vec<u8> {
    assert_eq!(left.len(), right.len(), "stereo capture planes must match");
    let mut bytes = Vec::with_capacity(left.len().saturating_mul(4));
    for (left, right) in left.iter().copied().zip(right.iter().copied()) {
        bytes.extend_from_slice(&pcm_s16(left).to_le_bytes());
        bytes.extend_from_slice(&pcm_s16(right).to_le_bytes());
    }
    bytes
}

#[cfg(any(test, feature = "browser-test-harness"))]
#[allow(
    clippy::cast_possible_truncation,
    reason = "finite samples are clamped to the signed 16-bit range before conversion"
)]
fn pcm_s16(sample: f32) -> i16 {
    if sample.is_nan() {
        return 0;
    }
    let scaled = if sample.is_sign_negative() {
        sample.max(-1.0) * 32_768.0
    } else {
        sample.min(1.0) * 32_767.0
    };
    scaled.round() as i16
}

/// Metrics for the most recently scheduled final software-mixed stereo chunk.
///
/// `peak` uses the signed 16-bit PCM full-scale convention already exposed by
/// `__crustDebug.audioPeak`: silence is zero and either polarity at full scale
/// has magnitude 32,768. The samples are observed before the browser master
/// gain, which keeps mute and the retail master fade out of diagnostic data.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ScheduledAudioMetrics {
    pub(crate) callbacks: u64,
    pub(crate) peak: i32,
}

impl ScheduledAudioMetrics {
    /// Records one planar stereo chunk after all software buses are combined.
    pub(crate) fn record_chunk(&mut self, left: &[f32], right: &[f32]) {
        self.callbacks = self.callbacks.saturating_add(1);
        self.peak = left
            .iter()
            .chain(right)
            .copied()
            .map(sample_magnitude)
            .max()
            .unwrap_or_default();
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a finite value clamped to 0..=1 and scaled by 32768 always fits i32"
)]
fn sample_magnitude(sample: f32) -> i32 {
    if sample.is_nan() {
        return 0;
    }
    (sample.abs().min(1.0) * 32_768.0) as i32
}

#[cfg(test)]
mod tests {
    use crust_audio::output::OutputOptions;

    use super::*;

    #[test]
    fn fixed_millisecond_clock_carries_fractional_samples_without_drift() {
        let mut clock = FixedMillisecondSampleClock::default();
        assert_eq!(clock.next_frames(34), 1_499);
        assert_eq!(clock.next_frames(34), 1_499);
        assert_eq!(clock.next_frames(34), 1_500);

        let mut clock = FixedMillisecondSampleClock::default();
        let frames = (0..500).map(|_| clock.next_frames(34)).sum::<usize>();
        assert_eq!(frames, 749_700);
    }

    #[test]
    fn capture_pcm_is_interleaved_clamped_and_little_endian() {
        assert_eq!(
            interleaved_pcm_s16le(&[0.0, 1.0, -1.0, f32::NAN], &[0.5, -0.5, 2.0, -2.0],),
            [
                0x00, 0x00, 0x00, 0x40, // 0.0 L, +0.5 R
                0xff, 0x7f, 0x00, 0xc0, // +1.0 L, -0.5 R
                0x00, 0x80, 0xff, 0x7f, // -1.0 L, clamped +1.0 R
                0x00, 0x00, 0x00, 0x80, // NaN L, clamped -1.0 R
            ],
        );
    }

    #[test]
    fn measures_the_final_three_bus_mix() {
        let output = OutputOptions::new(u8::MAX, u8::MAX, false);
        let option_mixed = output.mix_frame([0.25, -0.25], [0.125, -0.125]);
        let mixed = output.add_prescaled_sfx_frame(option_mixed, [0.25, -0.5]);
        assert!((mixed[0] - 0.625).abs() < f32::EPSILON);
        assert!((mixed[1] + 0.875).abs() < f32::EPSILON);

        let mut metrics = ScheduledAudioMetrics::default();
        metrics.record_chunk(&[mixed[0]], &[mixed[1]]);

        assert_eq!(
            metrics,
            ScheduledAudioMetrics {
                callbacks: 1,
                peak: 28_672,
            }
        );
    }

    #[test]
    fn peak_describes_the_latest_chunk_while_callbacks_accumulate() {
        let mut metrics = ScheduledAudioMetrics::default();
        metrics.record_chunk(&[-1.0, 0.5], &[0.25, 0.0]);
        metrics.record_chunk(&[0.125], &[-0.25]);

        assert_eq!(metrics.callbacks, 2);
        assert_eq!(metrics.peak, 8_192);
    }

    #[test]
    fn malformed_samples_cannot_poison_debug_metrics() {
        let mut metrics = ScheduledAudioMetrics::default();
        metrics.record_chunk(&[f32::NAN, f32::INFINITY], &[f32::NEG_INFINITY]);

        assert_eq!(metrics.callbacks, 1);
        assert_eq!(metrics.peak, 32_768);
    }

    #[test]
    fn empty_chunk_is_a_silent_callback() {
        let mut metrics = ScheduledAudioMetrics::default();
        metrics.record_chunk(&[], &[]);

        assert_eq!(
            metrics,
            ScheduledAudioMetrics {
                callbacks: 1,
                peak: 0,
            }
        );
    }
}
