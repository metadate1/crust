//! Browser-independent metrics for the stereo buffers scheduled to `WebAudio`.

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
