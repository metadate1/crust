//! Cooperative browser-frame scheduling.

/// The retail simulation cadence used by C1.
pub const SIMULATION_HZ: u32 = 30;
/// Duration of one simulation frame in microseconds, rounded down.
pub const FRAME_US: u64 = 1_000_000 / (SIMULATION_HZ as u64);
const EARLY_TOLERANCE_US: u64 = 250;

/// Decision returned by [`FrameScheduler::sample`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameDecision {
    /// The animation callback arrived before the next deadline.
    Wait,
    /// Advance exactly one simulation frame.
    Step,
}

/// A deterministic form of the source browser's cooperative 30 Hz gate.
///
/// It never runs a catch-up loop. If a callback is more than two frames late,
/// the following deadline is rebased one frame after the current timestamp.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FrameScheduler {
    next_frame_us: Option<u64>,
    frame_count: u64,
    paused: bool,
}

impl FrameScheduler {
    /// Creates an unarmed scheduler. The first sample steps immediately.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next_frame_us: None,
            frame_count: 0,
            paused: false,
        }
    }

    /// Pauses or resumes simulation without changing the current deadline.
    pub const fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Returns the number of simulation steps issued by this scheduler.
    #[must_use]
    pub const fn frame_count(&self) -> u64 {
        self.frame_count
    }

    /// Returns the next absolute deadline, if the scheduler has been armed.
    #[must_use]
    pub const fn next_frame_us(&self) -> Option<u64> {
        self.next_frame_us
    }

    /// Samples an absolute monotonic timestamp in microseconds.
    pub fn sample(&mut self, now_us: u64) -> FrameDecision {
        if self.paused {
            return FrameDecision::Wait;
        }

        let deadline = *self.next_frame_us.get_or_insert(now_us);
        if now_us.saturating_add(EARLY_TOLERANCE_US) < deadline {
            return FrameDecision::Wait;
        }

        self.frame_count = self.frame_count.wrapping_add(1);
        let regular_next = deadline.saturating_add(FRAME_US);
        self.next_frame_us = Some(if now_us.saturating_sub(regular_next) > FRAME_US * 2 {
            now_us.saturating_add(FRAME_US)
        } else {
            regular_next
        });
        FrameDecision::Step
    }

    /// Re-arms the scheduler so the next callback steps immediately.
    pub const fn reset_deadline(&mut self) {
        self.next_frame_us = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_callback_steps_and_early_callback_waits() {
        let mut scheduler = FrameScheduler::new();
        assert_eq!(scheduler.sample(1_000_000), FrameDecision::Step);
        assert_eq!(scheduler.sample(1_001_000), FrameDecision::Wait);
        assert_eq!(scheduler.sample(1_033_083), FrameDecision::Step);
        assert_eq!(scheduler.frame_count(), 2);
    }

    #[test]
    fn severe_lateness_rebases_without_catching_up() {
        let mut scheduler = FrameScheduler::new();
        assert_eq!(scheduler.sample(0), FrameDecision::Step);
        assert_eq!(scheduler.sample(500_000), FrameDecision::Step);
        assert_eq!(scheduler.frame_count(), 2);
        assert_eq!(scheduler.next_frame_us(), Some(500_000 + FRAME_US));
        assert_eq!(scheduler.sample(500_001), FrameDecision::Wait);
    }

    #[test]
    fn pause_does_not_consume_a_frame() {
        let mut scheduler = FrameScheduler::new();
        scheduler.set_paused(true);
        assert_eq!(scheduler.sample(99), FrameDecision::Wait);
        scheduler.set_paused(false);
        assert_eq!(scheduler.sample(99), FrameDecision::Step);
    }
}
