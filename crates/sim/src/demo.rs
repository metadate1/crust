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
        tapped: u32,
        ticks_elapsed: i32,
        ticks_per_frame: i32,
        first_frame: bool,
        end: Option<DemoEnd>,
    },
    Finished,
}

/// Why native PBAK input ownership ends after an observable recorded frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DemoEnd {
    Interrupted,
    Finished,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DemoPlayer {
    demo: Demo,
    cursor: usize,
    active: bool,
    next_ticks_per_frame: i32,
    draw_stamp: i32,
    previous_held: u32,
}

impl DemoPlayer {
    #[must_use]
    pub const fn new(demo: Demo) -> Self {
        let next_ticks_per_frame = demo.ticks_per_frame;
        let draw_stamp = demo.draw_stamp as i32;
        Self {
            demo,
            cursor: 0,
            active: true,
            next_ticks_per_frame,
            draw_stamp,
            previous_held: 0,
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

    /// Number of validated recorded pad frames.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.demo.frames.len()
    }

    /// Supplies real pad input and returns the effective recorded pad state.
    pub fn advance(&mut self, user_held: u32) -> DemoStep {
        if !self.active {
            return DemoStep::Finished;
        }
        let Some(frame) = self.demo.frames.get(self.cursor).copied() else {
            self.active = false;
            return DemoStep::Finished;
        };
        let first_frame = self.cursor == 0;
        let ticks_per_frame = self.next_ticks_per_frame;
        let tapped = (!self.previous_held & frame.held) & 0xf9ff;
        self.previous_held = frame.held;
        self.cursor += 1;
        let end = if user_held & DEMO_INTERRUPT_MASK != 0 {
            Some(DemoEnd::Interrupted)
        } else if self.cursor == self.demo.frames.len() {
            Some(DemoEnd::Finished)
        } else {
            None
        };
        if end.is_some() {
            // `PadUpdatePbak` copies the recorded word before it checks either
            // physical interruption or the final-frame pointer. Ownership is
            // released immediately, but that recorded word remains the pad
            // value observed by GOOL for this cooperative frame.
            self.active = false;
        } else if let Some(next) = self.demo.frames.get(self.cursor) {
            let elapsed = next.ticks_elapsed.wrapping_sub(self.draw_stamp);
            self.draw_stamp = next.ticks_elapsed;
            self.next_ticks_per_frame = round_retail_ticks(elapsed);
        }
        DemoStep::Playing {
            held: frame.held,
            tapped,
            ticks_elapsed: frame.ticks_elapsed,
            ticks_per_frame,
            first_frame,
            end,
        }
    }
}

const fn round_retail_ticks(ticks: i32) -> i32 {
    match ticks {
        0..=18 => 17,
        ..0 | 19..=35 => 34,
        36..=52 => 51,
        _ => ticks,
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
                tapped: 0x1000,
                ticks_elapsed: 20,
                ticks_per_frame: 17,
                first_frame: true,
                end: None,
            }
        );
        assert_eq!(
            player.advance(0),
            DemoStep::Playing {
                held: 0x2000,
                tapped: 0x2000,
                ticks_elapsed: 37,
                ticks_per_frame: 34,
                first_frame: false,
                end: Some(DemoEnd::Finished),
            }
        );
        assert_eq!(player.advance(0), DemoStep::Finished);
    }

    #[test]
    fn retail_face_start_select_mask_interrupts() {
        for bit in [0x10, 0x20, 0x40, 0x80, 0x100, 0x800] {
            let mut player = DemoPlayer::new(demo());
            assert_eq!(
                player.advance(bit),
                DemoStep::Playing {
                    held: 0x1000,
                    tapped: 0x1000,
                    ticks_elapsed: 20,
                    ticks_per_frame: 17,
                    first_frame: true,
                    end: Some(DemoEnd::Interrupted),
                }
            );
            assert!(!player.is_active());
        }
    }

    #[test]
    fn recorded_timeline_controls_the_following_frame_rate() {
        let demo = Demo::new(
            7,
            100,
            34,
            vec![
                DemoFrame {
                    ticks_elapsed: 120,
                    held: 1,
                },
                DemoFrame {
                    ticks_elapsed: 134,
                    held: 2,
                },
                DemoFrame {
                    ticks_elapsed: 185,
                    held: 3,
                },
            ],
        )
        .unwrap();
        let mut player = DemoPlayer::new(demo);

        let DemoStep::Playing {
            ticks_per_frame, ..
        } = player.advance(0)
        else {
            panic!("first recorded frame must play");
        };
        assert_eq!(ticks_per_frame, 34);
        let DemoStep::Playing {
            ticks_per_frame, ..
        } = player.advance(0)
        else {
            panic!("second recorded frame must play");
        };
        assert_eq!(ticks_per_frame, 34);
        let DemoStep::Playing {
            ticks_per_frame, ..
        } = player.advance(0)
        else {
            panic!("final recorded frame must play");
        };
        assert_eq!(ticks_per_frame, 51);
    }
}
