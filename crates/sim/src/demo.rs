//! Deterministic playback of retail PBAK pad frames.

pub const DEMO_INTERRUPT_MASK: u32 = 0x09f0;
pub const MAX_DEMO_FRAMES: usize = 30 * 60 * 30;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DemoFrame {
    pub ticks_elapsed: i32,
    pub held: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Demo {
    pub seed: u32,
    pub draw_stamp: u32,
    pub ticks_per_frame: i32,
    frames: Vec<DemoFrame>,
}

impl Demo {
    pub fn new(
        seed: u32,
        draw_stamp: u32,
        ticks_per_frame: i32,
        frames: Vec<DemoFrame>,
    ) -> Result<Self, DemoError> {
        if frames.is_empty() {
            return Err(DemoError::Empty);
        }
        if frames.len() > MAX_DEMO_FRAMES {
            return Err(DemoError::TooLong);
        }
        if ticks_per_frame <= 0 {
            return Err(DemoError::InvalidTicksPerFrame);
        }
        Ok(Self {
            seed,
            draw_stamp,
            ticks_per_frame,
            frames,
        })
    }

    #[must_use]
    pub fn frames(&self) -> &[DemoFrame] {
        &self.frames
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DemoError {
    Empty,
    TooLong,
    InvalidTicksPerFrame,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DemoStep {
    Playing {
        held: u32,
        ticks_elapsed: i32,
        first_frame: bool,
    },
    Interrupted,
    Finished,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DemoPlayer {
    demo: Demo,
    cursor: usize,
    active: bool,
}

impl DemoPlayer {
    #[must_use]
    pub const fn new(demo: Demo) -> Self {
        Self {
            demo,
            cursor: 0,
            active: true,
        }
    }

    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.active
    }

    #[must_use]
    pub const fn seed(&self) -> u32 {
        self.demo.seed
    }

    #[must_use]
    pub const fn draw_stamp(&self) -> u32 {
        self.demo.draw_stamp
    }

    #[must_use]
    pub const fn ticks_per_frame(&self) -> i32 {
        self.demo.ticks_per_frame
    }

    /// Supplies real pad input and returns the effective recorded pad state.
    pub fn advance(&mut self, user_held: u32) -> DemoStep {
        if !self.active {
            return DemoStep::Finished;
        }
        if user_held & DEMO_INTERRUPT_MASK != 0 {
            self.active = false;
            return DemoStep::Interrupted;
        }
        let Some(frame) = self.demo.frames.get(self.cursor).copied() else {
            self.active = false;
            return DemoStep::Finished;
        };
        let first_frame = self.cursor == 0;
        self.cursor += 1;
        if self.cursor == self.demo.frames.len() {
            // The final recorded input is observable for this frame. The next
            // call reports Finished, matching the source frame pointer order.
            self.active = false;
        }
        DemoStep::Playing {
            held: frame.held,
            ticks_elapsed: frame.ticks_elapsed,
            first_frame,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo() -> Demo {
        Demo::new(
            7,
            11,
            17,
            vec![
                DemoFrame {
                    ticks_elapsed: 20,
                    held: 0x1000,
                },
                DemoFrame {
                    ticks_elapsed: 37,
                    held: 0x2000,
                },
            ],
        )
        .unwrap()
    }

    #[test]
    fn playback_overrides_pad_and_preserves_final_frame() {
        let mut player = DemoPlayer::new(demo());
        assert_eq!(
            player.advance(0),
            DemoStep::Playing {
                held: 0x1000,
                ticks_elapsed: 20,
                first_frame: true
            }
        );
        assert_eq!(
            player.advance(0),
            DemoStep::Playing {
                held: 0x2000,
                ticks_elapsed: 37,
                first_frame: false
            }
        );
        assert_eq!(player.advance(0), DemoStep::Finished);
    }

    #[test]
    fn retail_face_start_select_mask_interrupts() {
        for bit in [0x10, 0x20, 0x40, 0x80, 0x100, 0x800] {
            let mut player = DemoPlayer::new(demo());
            assert_eq!(player.advance(bit), DemoStep::Interrupted);
        }
    }
}
