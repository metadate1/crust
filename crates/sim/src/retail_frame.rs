//! Retail-compatible gameplay-frame and presentation sequencing.
//!
//! One call to [`RetailFrameState::tick`] represents one cooperative 30 Hz
//! simulation step. The trace deliberately exposes the ordering boundary
//! between camera/world work, GOOL, and presentation so those contracts can be
//! preserved as the real subsystems are connected.

use core::num::NonZeroU16;

/// One whole path point in the engine's signed 8.8 progress format.
pub const PATH_POINT_STEP: i32 = 0x100;
/// Draw-skip value installed after a direct loading image is written.
pub const LOADING_DRAW_SKIP: u8 = 2;

/// A path position stored in signed 8.8 fixed-point units.
///
/// Values held by this type are always clamped to the supplied path's valid
/// interval: `0..=(point_count << 8) - 1`.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct PathProgress(i32);

impl PathProgress {
    /// The first point on a path.
    pub const ZERO: Self = Self(0);

    /// Clamps a raw signed 8.8 value to a non-empty path.
    #[must_use]
    pub fn clamped(raw: i32, point_count: NonZeroU16) -> Self {
        Self(raw.clamp(0, Self::maximum_raw(point_count)))
    }

    /// Returns the underlying signed 8.8 value.
    #[must_use]
    pub const fn raw(self) -> i32 {
        self.0
    }

    /// Returns the integer path-point index.
    #[must_use]
    pub const fn point_index(self) -> u16 {
        (self.0 >> 8) as u16
    }

    /// Returns the fractional byte within the current point.
    #[must_use]
    pub const fn fraction(self) -> u8 {
        (self.0 & 0xff) as u8
    }

    /// Adds signed 8.8 units and clamps the result to the path.
    #[must_use]
    pub fn advance(self, delta: i32, point_count: NonZeroU16) -> Self {
        Self::clamped(self.0.saturating_add(delta), point_count)
    }

    const fn maximum_raw(point_count: NonZeroU16) -> i32 {
        (point_count.get() as i32) * PATH_POINT_STEP - 1
    }
}

/// The framebuffer content made visible by a simulation tick.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresentedFrame {
    /// No buffer was presented by this tick.
    None,
    /// The direct loading image was presented while gameplay drawing was skipped.
    LoadingImage,
    /// A gameplay ordering table was drawn and presented.
    Gameplay {
        /// Camera-selected path progress used to build the frame.
        progress: PathProgress,
        /// Animation counter used while transforming this frame.
        draw_count: u32,
    },
}

/// An observable stage in one retail gameplay tick.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameEvent {
    /// Resolve a pending level transition before touching the active level.
    HandleLevelTransition,
    /// Spawn entities made visible by the active zone set.
    SpawnObjects,
    /// Advance the automatic camera by one path point.
    CameraUpdated {
        /// Progress entering the camera update.
        before: PathProgress,
        /// Progress selected for this tick's world transform.
        after: PathProgress,
    },
    /// Snapshot texture-page generations after any camera-driven zone crossing.
    TexturesBeginFrame,
    /// Transform worlds using the camera result and the pre-increment draw count.
    WorldsTransformed {
        /// Progress used for world visibility and camera matrices.
        progress: PathProgress,
        /// Animation counter used by texture selection.
        draw_count: u32,
    },
    /// Run GOOL after camera/world work; camera therefore sees prior GOOL state.
    GoolUpdated,
    /// Freeze the pre-decrement draw-skip decision.
    PresentationGate {
        /// Draw-skip value observed at frame begin.
        draw_skip: u8,
        /// Whether this tick is eligible to draw its gameplay ordering table.
        render_frame: bool,
    },
    /// Advance the animation counter after primitive generation.
    DrawCountIncremented {
        /// Counter used by this tick's transforms.
        before: u32,
        /// Counter available to the following tick.
        after: u32,
    },
    /// Consume one pending presentation skip.
    DrawSkipDecremented {
        /// Counter frozen by the presentation gate.
        before: u8,
        /// Counter tested before drawing the gameplay ordering table.
        after: u8,
    },
    /// Record what, if anything, reached the front buffer.
    Presented(PresentedFrame),
}

/// Number of ordered events emitted for every frame.
pub const FRAME_EVENT_COUNT: usize = 10;

/// Deterministic trace and resulting counters for one 30 Hz tick.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameTrace {
    tick: u64,
    events: [FrameEvent; FRAME_EVENT_COUNT],
    progress: PathProgress,
    draw_skip: u8,
    draw_count: u32,
    presented: PresentedFrame,
}

impl FrameTrace {
    /// Returns the one-based simulation tick number.
    #[must_use]
    pub const fn tick(&self) -> u64 {
        self.tick
    }

    /// Returns the events in exact execution order.
    #[must_use]
    pub const fn events(&self) -> &[FrameEvent; FRAME_EVENT_COUNT] {
        &self.events
    }

    /// Returns path progress after the camera update.
    #[must_use]
    pub const fn progress(&self) -> PathProgress {
        self.progress
    }

    /// Returns draw-skip state after presentation processing.
    #[must_use]
    pub const fn draw_skip(&self) -> u8 {
        self.draw_skip
    }

    /// Returns the animation counter available to the following tick.
    #[must_use]
    pub const fn draw_count(&self) -> u32 {
        self.draw_count
    }

    /// Returns the content presented by this tick.
    #[must_use]
    pub const fn presented(&self) -> PresentedFrame {
        self.presented
    }
}

/// Persistent counters needed to reproduce retail camera/presentation timing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailFrameState {
    point_count: NonZeroU16,
    progress: PathProgress,
    tick: u64,
    draw_skip: u8,
    draw_count: u32,
    direct_loading_image_written: bool,
}

impl RetailFrameState {
    /// Creates a state that can present gameplay on its first tick.
    #[must_use]
    pub fn ready(point_count: NonZeroU16, initial_progress: i32) -> Self {
        Self {
            point_count,
            progress: PathProgress::clamped(initial_progress, point_count),
            tick: 0,
            draw_skip: 0,
            draw_count: 0,
            direct_loading_image_written: false,
        }
    }

    /// Creates the state immediately after a direct loading image was written.
    ///
    /// Retail installs a skip count of two here. The first simulation tick is
    /// fully executed but its gameplay primitives are discarded while the
    /// loading image is presented. The second tick presents gameplay.
    #[must_use]
    pub fn after_loading_image(point_count: NonZeroU16, initial_progress: i32) -> Self {
        Self {
            draw_skip: LOADING_DRAW_SKIP,
            direct_loading_image_written: true,
            ..Self::ready(point_count, initial_progress)
        }
    }

    /// Returns the completed simulation-tick count.
    #[must_use]
    pub const fn tick_count(&self) -> u64 {
        self.tick
    }

    /// Returns current signed 8.8 path progress.
    #[must_use]
    pub const fn progress(&self) -> PathProgress {
        self.progress
    }

    /// Returns the pending presentation-skip count.
    #[must_use]
    pub const fn draw_skip(&self) -> u8 {
        self.draw_skip
    }

    /// Returns the animation counter available to the next world transform.
    #[must_use]
    pub const fn draw_count(&self) -> u32 {
        self.draw_count
    }

    /// Advances one cooperative 30 Hz gameplay tick and returns its event trace.
    pub fn tick(&mut self) -> FrameTrace {
        let tick = self.tick.wrapping_add(1);
        let progress_before = self.progress;
        let progress_after = progress_before.advance(PATH_POINT_STEP, self.point_count);
        let draw_count_before = self.draw_count;
        let draw_count_after = draw_count_before.wrapping_add(1);
        let draw_skip_before = self.draw_skip;
        let render_frame = matches!(draw_skip_before, 0 | 1);
        let draw_skip_after = draw_skip_before.saturating_sub(1);
        let gameplay_drawn = render_frame && draw_skip_after == 0;

        self.tick = tick;
        self.progress = progress_after;
        self.draw_count = draw_count_after;
        self.draw_skip = draw_skip_after;

        let presented = if gameplay_drawn {
            PresentedFrame::Gameplay {
                progress: progress_after,
                draw_count: draw_count_before,
            }
        } else if self.direct_loading_image_written {
            PresentedFrame::LoadingImage
        } else {
            PresentedFrame::None
        };
        self.direct_loading_image_written = false;

        let events = [
            FrameEvent::HandleLevelTransition,
            FrameEvent::SpawnObjects,
            FrameEvent::CameraUpdated {
                before: progress_before,
                after: progress_after,
            },
            FrameEvent::TexturesBeginFrame,
            FrameEvent::WorldsTransformed {
                progress: progress_after,
                draw_count: draw_count_before,
            },
            FrameEvent::GoolUpdated,
            FrameEvent::PresentationGate {
                draw_skip: draw_skip_before,
                render_frame,
            },
            FrameEvent::DrawCountIncremented {
                before: draw_count_before,
                after: draw_count_after,
            },
            FrameEvent::DrawSkipDecremented {
                before: draw_skip_before,
                after: draw_skip_after,
            },
            FrameEvent::Presented(presented),
        ];

        FrameTrace {
            tick,
            events,
            progress: progress_after,
            draw_skip: draw_skip_after,
            draw_count: draw_count_after,
            presented,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point_count(value: u16) -> NonZeroU16 {
        NonZeroU16::new(value).expect("test paths are non-empty")
    }

    #[test]
    fn signed_progress_clamps_to_path_and_preserves_fraction() {
        let points = point_count(3);

        assert_eq!(PathProgress::clamped(-0x100, points), PathProgress::ZERO);
        assert_eq!(PathProgress::clamped(0x17f, points).point_index(), 1);
        assert_eq!(PathProgress::clamped(0x17f, points).fraction(), 0x7f);
        assert_eq!(PathProgress::clamped(i32::MAX, points).raw(), 0x2ff);
        assert_eq!(
            PathProgress::clamped(0x2f0, points)
                .advance(PATH_POINT_STEP, points)
                .raw(),
            0x2ff
        );
    }

    #[test]
    fn camera_precedes_gool_and_worlds_use_pre_increment_draw_count() {
        let mut state = RetailFrameState::ready(point_count(8), 0);
        let trace = state.tick();
        let events = trace.events();
        let camera = events
            .iter()
            .position(|event| matches!(event, FrameEvent::CameraUpdated { .. }))
            .expect("camera event");
        let worlds = events
            .iter()
            .position(|event| matches!(event, FrameEvent::WorldsTransformed { .. }))
            .expect("world event");
        let gool = events
            .iter()
            .position(|event| matches!(event, FrameEvent::GoolUpdated))
            .expect("GOOL event");

        assert!(camera < worlds && worlds < gool);
        assert_eq!(
            events[worlds],
            FrameEvent::WorldsTransformed {
                progress: PathProgress::clamped(0x100, point_count(8)),
                draw_count: 0,
            }
        );
        assert_eq!(trace.draw_count(), 1);
    }

    #[test]
    fn n_sanity_loading_contract_presents_gameplay_on_tick_two() {
        let mut state = RetailFrameState::after_loading_image(point_count(72), 0);

        let first = state.tick();
        assert_eq!(first.tick(), 1);
        assert_eq!(first.progress().raw(), 0x100);
        assert_eq!(first.draw_skip(), 1);
        assert_eq!(first.draw_count(), 1);
        assert_eq!(first.presented(), PresentedFrame::LoadingImage);
        assert_eq!(
            first.events()[6],
            FrameEvent::PresentationGate {
                draw_skip: 2,
                render_frame: false,
            }
        );

        let second = state.tick();
        assert_eq!(second.tick(), 2);
        assert_eq!(second.progress().raw(), 0x200);
        assert_eq!(second.draw_skip(), 0);
        assert_eq!(second.draw_count(), 2);
        assert_eq!(
            second.presented(),
            PresentedFrame::Gameplay {
                progress: PathProgress::clamped(0x200, point_count(72)),
                draw_count: 1,
            }
        );
        assert_eq!(
            second.events()[6],
            FrameEvent::PresentationGate {
                draw_skip: 1,
                render_frame: true,
            }
        );
    }

    #[test]
    fn auto_camera_stays_at_last_fractional_path_unit() {
        let points = point_count(2);
        let mut state = RetailFrameState::ready(points, 0x1f0);

        let trace = state.tick();

        assert_eq!(trace.progress().raw(), 0x1ff);
        assert_eq!(state.progress().raw(), 0x1ff);
    }
}
