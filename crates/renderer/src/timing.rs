//! Frame presentation policy and timing diagnostics.

/// Preserve the original post-decrement skip decision.
#[must_use]
pub const fn should_render_frame(draw_skip_counter: i32) -> bool {
    draw_skip_counter == 0 || draw_skip_counter == 1
}

/// Quantize elapsed milliseconds to the timing buckets stored by demos.
#[must_use]
pub const fn round_ticks(elapsed_ms: i32) -> u32 {
    if elapsed_ms < 0 {
        34
    } else if elapsed_ms < 19 {
        17
    } else if elapsed_ms < 36 {
        34
    } else if elapsed_ms < 53 {
        51
    } else {
        elapsed_ms.unsigned_abs()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameDecision {
    pub render: bool,
    pub rounded_ticks: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameTimingDiagnostics {
    pub frames: u64,
    pub rendered_frames: u64,
    pub skipped_frames: u64,
    pub negative_elapsed_samples: u64,
    pub late_frames: u64,
    pub last_elapsed_ms: i32,
    pub last_rounded_ticks: u32,
    pub maximum_elapsed_ms: i32,
    pub total_nonnegative_elapsed_ms: u64,
}

/// Stateful instrumentation around the otherwise pure frame-skip decision.
#[derive(Debug, Clone, Default)]
pub struct FrameTimingPolicy {
    diagnostics: FrameTimingDiagnostics,
}

impl FrameTimingPolicy {
    #[must_use]
    pub fn evaluate(&mut self, draw_skip_counter: i32, elapsed_ms: i32) -> FrameDecision {
        let render = should_render_frame(draw_skip_counter);
        let rounded_ticks = round_ticks(elapsed_ms);
        increment(&mut self.diagnostics.frames);
        if render {
            increment(&mut self.diagnostics.rendered_frames);
        } else {
            increment(&mut self.diagnostics.skipped_frames);
        }
        if elapsed_ms < 0 {
            increment(&mut self.diagnostics.negative_elapsed_samples);
        } else {
            self.diagnostics.total_nonnegative_elapsed_ms = self
                .diagnostics
                .total_nonnegative_elapsed_ms
                .saturating_add(u64::try_from(elapsed_ms).unwrap_or(u64::MAX));
            self.diagnostics.maximum_elapsed_ms =
                self.diagnostics.maximum_elapsed_ms.max(elapsed_ms);
            if elapsed_ms >= 53 {
                increment(&mut self.diagnostics.late_frames);
            }
        }
        self.diagnostics.last_elapsed_ms = elapsed_ms;
        self.diagnostics.last_rounded_ticks = rounded_ticks;
        FrameDecision {
            render,
            rounded_ticks,
        }
    }

    #[must_use]
    pub const fn diagnostics(&self) -> &FrameTimingDiagnostics {
        &self.diagnostics
    }

    pub fn reset_diagnostics(&mut self) {
        self.diagnostics = FrameTimingDiagnostics::default();
    }
}

fn increment(value: &mut u64) {
    *value = value.saturating_add(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn frame_skip_characterization() {
        assert!(should_render_frame(0));
        assert!(should_render_frame(1));
        assert!(!should_render_frame(2));
        assert!(!should_render_frame(3));
        assert!(!should_render_frame(-1));
    }

    #[test]
    fn timing_buckets_match_demo_contract() {
        assert_eq!(round_ticks(-1), 34);
        assert_eq!(round_ticks(0), 17);
        assert_eq!(round_ticks(18), 17);
        assert_eq!(round_ticks(19), 34);
        assert_eq!(round_ticks(35), 34);
        assert_eq!(round_ticks(36), 51);
        assert_eq!(round_ticks(52), 51);
        assert_eq!(round_ticks(53), 53);
        assert_eq!(round_ticks(100), 100);
    }

    #[test]
    fn diagnostics_distinguish_skips_and_late_frames() {
        let mut policy = FrameTimingPolicy::default();
        assert!(policy.evaluate(0, 17).render);
        assert!(!policy.evaluate(2, 60).render);
        assert!(!policy.evaluate(-1, -5).render);
        let diagnostics = policy.diagnostics();
        assert_eq!(diagnostics.frames, 3);
        assert_eq!(diagnostics.rendered_frames, 1);
        assert_eq!(diagnostics.skipped_frames, 2);
        assert_eq!(diagnostics.late_frames, 1);
        assert_eq!(diagnostics.negative_elapsed_samples, 1);
        assert_eq!(diagnostics.total_nonnegative_elapsed_ms, 77);
        assert_eq!(diagnostics.maximum_elapsed_ms, 60);
        assert_eq!(diagnostics.last_rounded_ticks, 34);
    }

    proptest! {
        #[test]
        fn rounded_ticks_are_always_positive(elapsed in any::<i32>()) {
            prop_assert!(round_ticks(elapsed) >= 17);
        }
    }
}
