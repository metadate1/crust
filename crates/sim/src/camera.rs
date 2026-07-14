//! Camera state and characterized map/death-camera helpers.

use core::{fmt, num::NonZeroU16};
use std::collections::BTreeSet;

use crust_formats::binary::FormatError;
use crust_formats::stream::{RetailPathId, RetailZoneGraph, ZonePath};

use crate::math::{Angle12, Angles, Vec3, integer_sqrt, seek};
use crate::retail_frame::{PATH_POINT_STEP, PathProgress};

/// Input is accepted by GOOL-controlled gameplay.
pub const GAME_STATE_PLAYING: i32 = 0x100;
/// Input is suppressed while a retail automatic camera owns the frame.
pub const GAME_STATE_CUTSCENE: i32 = 0;

const AUTO_SKIP_BUTTON_MASK: u32 = 0xf0;
const AUTO_SKIP_DISABLE_FLAGS: u32 = 0x8_1000;
const SAVE_STATE_DISABLE_FLAG: u32 = 0x1000;
const ZONE_AUTO_CAM_Z_OFFSET: u32 = 0x80;
const ZONE_SIDE_SCROLL: u32 = 0x4000;
const ZONE_BACKWARD: u32 = 0x8000;
const ZONE_DISCARD_BELOW_PATHS: u32 = 0x4_0000;

const PAD_UP: u32 = 0x1000;
const PAD_RIGHT: u32 = 0x2000;
const PAD_DOWN: u32 = 0x4000;
const PAD_LEFT: u32 = 0x8000;

const DEFAULT_OFFSET_Z: i32 = -0x12c00;
const DEFAULT_ZOOM: i32 = 0x6a400;
const DEFAULT_OFFSET_Y: i32 = 0x3e800;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CameraMode {
    Follow,
    Path,
    Fixed,
    Orbit,
    Death,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CameraState {
    pub translation: Vec3,
    pub rotation: Angles,
    pub mode: CameraMode,
    pub offset: Vec3,
    pub zoom: i32,
    pub death_acceleration: i32,
    pub death_orbit: i32,
    pub death_flip_velocity: i32,
}

impl Default for CameraState {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Angles::default(),
            mode: CameraMode::Follow,
            offset: Vec3 {
                x: 0,
                y: 0x3e800,
                z: -0x12c00,
            },
            zoom: 0x6a400,
            death_acceleration: 0,
            death_orbit: 0,
            death_flip_velocity: 0,
        }
    }
}

impl CameraState {
    /// Deterministically follows a target using per-axis seek limits.
    pub fn follow(&mut self, target: Vec3, speed: i32) {
        self.mode = CameraMode::Follow;
        let desired = target.wrapping_add(self.offset);
        self.translation.x = seek(self.translation.x, desired.x, speed);
        self.translation.y = seek(self.translation.y, desired.y, speed);
        self.translation.z = seek(self.translation.z, desired.z, speed);
    }

    /// Advances the characterized orbit acceleration used by death cameras.
    pub fn death_step(&mut self, focus: Vec3, flip_speed: i32, zoom_speed: i32, accelerate: bool) {
        self.mode = CameraMode::Death;
        self.death_acceleration = 22;
        self.death_flip_velocity = flip_speed;
        if accelerate {
            self.death_orbit = self.death_orbit.wrapping_add(self.death_acceleration);
            self.rotation.x = self.rotation.x.wrapping_add(self.death_acceleration);
        }
        self.translation.y = seek(self.translation.y, focus.y.saturating_add(120_000), 102_400);
        self.zoom = seek(self.zoom, 175_000, zoom_speed);
        let sin = i32::from(Angle12::new(self.death_orbit).sin_q12());
        let cos = i32::from(Angle12::new(self.death_orbit).cos_q12());
        self.translation.x = focus
            .x
            .wrapping_add(((i64::from(self.zoom) * i64::from(sin)) >> 12) as i32);
        self.translation.z = focus
            .z
            .wrapping_add(((i64::from(self.zoom) * i64::from(cos)) >> 12) as i32);
    }
}

/// Per-frame input consumed by the retail camera-path state machine.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetailCameraInput {
    /// Buttons newly pressed during this 30 Hz simulation tick.
    pub tapped: u32,
}

/// External world-map globals consumed by retail camera modes seven and eight.
///
/// `island_cam_state` is written by title/GOOL logic outside `CamUpdate`, while
/// `island_cam_rot_x` supplies mode seven's target orbit angle. Keeping these
/// words at an explicit boundary avoids inventing values in the camera runtime.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetailIslandCameraInput {
    pub island_cam_state: i32,
    pub island_cam_rot_x: i32,
}

/// Frame-local object and global words consumed by retail `CamFollow`.
///
/// Translations and zooms retain the engine's signed 24.8 representation.
/// Frame stamps use wrapping 32-bit subtraction, matching the retail MIPS
/// comparison used by timed camera links.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetailCameraFollowInput {
    pub player_translation: Vec3,
    pub player_cam_zoom: i32,
    pub held_buttons: u32,
    pub level_id: i32,
    pub frames_elapsed: u32,
    pub gem_stamp: u32,
}

/// Persistent globals owned by retail `CamFollow`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailCameraFollowState {
    pub offset_z: i32,
    pub zoom: i32,
    pub offset_dir_z: bool,
    pub offset_dir_x: bool,
    pub offset_y: i32,
    pub offset_x: i32,
    pub speed: i32,
}

impl Default for RetailCameraFollowState {
    fn default() -> Self {
        Self {
            offset_z: DEFAULT_OFFSET_Z,
            zoom: DEFAULT_ZOOM,
            offset_dir_z: false,
            offset_dir_x: false,
            offset_y: DEFAULT_OFFSET_Y,
            offset_x: 0,
            speed: 0,
        }
    }
}

/// Stable camera-path location with signed 8.8 progress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailCameraLocation {
    pub path: RetailPathId,
    pub progress: PathProgress,
}

/// Camera transform sampled from one retail ZDAT path position.
///
/// Translation uses the engine's signed 24.8 world units. Rotation preserves
/// the serialized/runtime `y`, `x`, `z` order consumed by `GfxUpdateMatrices`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailCameraPose {
    pub translation: [i32; 3],
    pub rotation_yxz: [i32; 3],
}

/// Behavior selected by one source-compatible camera update.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetailCameraOutcome {
    /// Camera mode zero deliberately leaves the path location unchanged.
    Stationary,
    /// An automatic mode advanced or crossed one or more paths.
    AutoAdvanced { skipped: bool, path_crossings: u32 },
    /// Modes five and six hand position/orientation work to `CamFollow`.
    ///
    /// [`RetailCameraRuntime::update`] returns this boundary when no typed
    /// player transform was supplied; [`RetailCameraRuntime::update_follow`]
    /// executes the characterized follow-camera path instead.
    FollowBoundary { mode: u16 },
    /// `CamFollow` evaluated the current path and all eligible neighbors.
    FollowEvaluated {
        mode: u16,
        candidate_count: u8,
        moved: bool,
        crossed_path: bool,
    },
    /// Modes seven/eight need world-map globals owned outside `CamUpdate`.
    ///
    /// The legacy [`RetailCameraRuntime::update`] entry point returns this safe
    /// no-op until a host calls [`RetailCameraRuntime::update_with_island`] or
    /// [`RetailCameraRuntime::update_island`] with those typed values.
    IslandBoundary { mode: u16 },
    /// A source-compatible world-map orbit or directed path step.
    IslandAdvanced {
        mode: u16,
        state_before: i32,
        state_after: i32,
        /// Path links traversed while selecting the final location. Mode seven
        /// may traverse several locally but still emits one final `LevelUpdate`.
        path_crossings: u32,
        moved: bool,
    },
}

/// A side-effect handshake requested by the retail camera state machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetailCameraEffect {
    /// One exact `LevelUpdate` call produced by camera traversal.
    ///
    /// Automatic skip can cross several paths/zones in a single frame. The
    /// ordered trace is therefore required; comparing only a step's aggregate
    /// `before`/`after` locations would skip intermediate termination and
    /// paging work.
    LevelUpdate {
        before: RetailCameraLocation,
        after: RetailCameraLocation,
        flags: u8,
    },
    /// Retail saves after entering a zone whose graphics flag `0x1000` is clear.
    SaveStateHandshake { location: RetailCameraLocation },
}

/// Complete deterministic result of one camera update.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetailCameraStep {
    pub before: RetailCameraLocation,
    pub after: RetailCameraLocation,
    pub outcome: RetailCameraOutcome,
    pub game_state: i32,
    pub effects: Vec<RetailCameraEffect>,
}

/// Checked failures at the boundary between parsed ZDAT data and camera logic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RetailCameraError {
    Graph(FormatError),
    InvalidPathPointCount {
        path: RetailPathId,
        point_count: usize,
    },
    UnsupportedMode {
        path: RetailPathId,
        mode: u16,
    },
    FollowModeRequired {
        path: RetailPathId,
        mode: u16,
    },
    IslandModeRequired {
        path: RetailPathId,
        mode: u16,
    },
    InvalidAverageNodeDistance {
        path: RetailPathId,
        distance: i16,
    },
    FollowArithmetic {
        path: RetailPathId,
        operation: &'static str,
    },
    AutoSkipCycle {
        path: RetailPathId,
    },
    IslandOrbitCycle {
        path: RetailPathId,
    },
}

impl fmt::Display for RetailCameraError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Graph(error) => write!(formatter, "camera zone graph: {error}"),
            Self::InvalidPathPointCount { path, point_count } => write!(
                formatter,
                "camera path {}:{} has invalid point count {point_count}",
                path.zone, path.index
            ),
            Self::UnsupportedMode { path, mode } => write!(
                formatter,
                "camera path {}:{} uses unsupported mode {mode}",
                path.zone, path.index
            ),
            Self::FollowModeRequired { path, mode } => write!(
                formatter,
                "camera path {}:{} uses mode {mode}; retail follow requires mode 5 or 6",
                path.zone, path.index
            ),
            Self::IslandModeRequired { path, mode } => write!(
                formatter,
                "camera path {}:{} uses mode {mode}; retail island camera requires mode 7 or 8",
                path.zone, path.index
            ),
            Self::InvalidAverageNodeDistance { path, distance } => write!(
                formatter,
                "camera path {}:{} has invalid average-node distance {distance}",
                path.zone, path.index
            ),
            Self::FollowArithmetic { path, operation } => write!(
                formatter,
                "camera path {}:{} cannot safely reproduce retail {operation}",
                path.zone, path.index
            ),
            Self::AutoSkipCycle { path } => write!(
                formatter,
                "automatic-camera skip cycles through path {}:{}",
                path.zone, path.index
            ),
            Self::IslandOrbitCycle { path } => write!(
                formatter,
                "world-map camera orbit cycles without returning through path {}:{}",
                path.zone, path.index
            ),
        }
    }
}

impl std::error::Error for RetailCameraError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Graph(error) => Some(error),
            Self::InvalidPathPointCount { .. }
            | Self::UnsupportedMode { .. }
            | Self::FollowModeRequired { .. }
            | Self::IslandModeRequired { .. }
            | Self::InvalidAverageNodeDistance { .. }
            | Self::FollowArithmetic { .. }
            | Self::AutoSkipCycle { .. }
            | Self::IslandOrbitCycle { .. } => None,
        }
    }
}

impl From<FormatError> for RetailCameraError {
    fn from(error: FormatError) -> Self {
        Self::Graph(error)
    }
}

/// Source-characterized camera path state for cooperative 30 Hz simulation.
///
/// Updates are transactional: malformed links, unsupported modes and skip
/// cycles leave the persistent location and game-state word unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailCameraRuntime {
    location: RetailCameraLocation,
    game_state: i32,
    follow: RetailCameraFollowState,
}

impl RetailCameraRuntime {
    /// Starts at the LDAT spawn path and progress zero in cutscene state.
    pub fn new(graph: &RetailZoneGraph) -> Result<Self, RetailCameraError> {
        Self::at_path(graph, graph.spawn_path(), 0, GAME_STATE_CUTSCENE)
    }

    /// Starts at an explicit validated graph path and clamps raw 8.8 progress.
    pub fn at_path(
        graph: &RetailZoneGraph,
        path: RetailPathId,
        raw_progress: i32,
        game_state: i32,
    ) -> Result<Self, RetailCameraError> {
        let point_count = retail_point_count(graph, path)?;
        Ok(Self {
            location: RetailCameraLocation {
                path,
                progress: PathProgress::clamped(raw_progress, point_count),
            },
            game_state,
            follow: RetailCameraFollowState::default(),
        })
    }

    /// Current graph path and signed 8.8 progress.
    #[must_use]
    pub const fn location(&self) -> RetailCameraLocation {
        self.location
    }

    /// Applies one explicit source `LevelUpdate` from title/restart logic.
    ///
    /// The target path is validated and raw progress is clamped exactly like
    /// the native function. `game_state` is deliberately unchanged: only
    /// `CamUpdate`'s mode arms write that global. The ordered effect lets a
    /// host update paging and zone activation before the next spawn scan.
    pub fn level_update(
        &mut self,
        graph: &RetailZoneGraph,
        path: RetailPathId,
        raw_progress: i32,
        flags: u8,
    ) -> Result<RetailCameraStep, RetailCameraError> {
        let point_count = retail_point_count(graph, path)?;
        let before = self.location;
        let after = RetailCameraLocation {
            path,
            progress: PathProgress::clamped(raw_progress, point_count),
        };
        self.location = after;
        Ok(self.step(
            before,
            RetailCameraOutcome::Stationary,
            vec![RetailCameraEffect::LevelUpdate {
                before,
                after,
                flags,
            }],
        ))
    }

    /// Current retail gameplay/cutscene state word.
    #[must_use]
    pub const fn game_state(&self) -> i32 {
        self.game_state
    }

    /// Returns the no-op result used when current display bit two disables
    /// `CamUpdate` for this frame.
    #[must_use]
    pub fn stationary_step(&self) -> RetailCameraStep {
        RetailCameraStep {
            before: self.location,
            after: self.location,
            outcome: RetailCameraOutcome::Stationary,
            game_state: self.game_state,
            effects: Vec::new(),
        }
    }

    /// Persistent retail follow-camera offset, zoom, and smoothing globals.
    #[must_use]
    pub const fn follow_state(&self) -> RetailCameraFollowState {
        self.follow
    }

    /// Camera-relative XZ heading consumed by native GOOL player physics.
    ///
    /// `LevelUpdate` samples the current path with the negated signed-8.8
    /// progress, then `GfxUpdateMatrices` publishes `cam_rot_after.x` unless
    /// the active zone's fixed-bounce camera flag forces zero. Path sampling
    /// itself uses the progress magnitude, so this method can operate on the
    /// runtime's canonical location without manufacturing a render matrix.
    pub fn rotation_xz(&self, graph: &RetailZoneGraph) -> Result<Angle12, RetailCameraError> {
        let zone = graph.zone(self.location.path.zone).ok_or_else(|| {
            RetailCameraError::Graph(FormatError::global(format!(
                "camera zone {} is absent",
                self.location.path.zone
            )))
        })?;
        if zone.graphics_flags & 0x1000 != 0 {
            return Ok(Angle12::new(0));
        }
        let path = graph.path(self.location.path).ok_or_else(|| {
            RetailCameraError::Graph(FormatError::global(format!(
                "camera path {}:{} is absent",
                self.location.path.zone, self.location.path.index
            )))
        })?;
        let magnitude = self.location.progress.raw().unsigned_abs();
        let point_index =
            usize::try_from(magnitude >> 8).map_err(|_| RetailCameraError::FollowArithmetic {
                path: self.location.path,
                operation: "XZ rotation point lookup",
            })?;
        let point = path.points.get(point_index).ok_or({
            RetailCameraError::InvalidPathPointCount {
                path: self.location.path,
                point_count: path.points.len(),
            }
        })?;
        let fraction =
            i32::try_from(magnitude & 0xff).map_err(|_| RetailCameraError::FollowArithmetic {
                path: self.location.path,
                operation: "XZ rotation fraction",
            })?;
        let mut next_rotation_x = point.rotation_x;

        if fraction != 0 && point_index == path.points.len() - 1 {
            if let Some((link_index, link)) = path
                .neighbors
                .iter()
                .copied()
                .enumerate()
                .find(|(_, link)| link.relation & 2 != 0)
            {
                let (next_id, _) = graph.resolve_neighbor(self.location.path, link_index)?;
                let next_path = graph.path(next_id).ok_or_else(|| {
                    RetailCameraError::Graph(FormatError::global(format!(
                        "camera path {}:{} is absent",
                        next_id.zone, next_id.index
                    )))
                })?;
                // Retail refuses to interpolate into a mode-one camera.
                if next_path.camera_mode != 1 {
                    let next_index = if link.goal & 2 != 0 {
                        next_path.points.len() - 1
                    } else {
                        0
                    };
                    next_rotation_x = next_path.points[next_index].rotation_x;
                }
            }
        } else if let Some(next) = path.points.get(point_index.saturating_add(1)) {
            next_rotation_x = next.rotation_x;
        }

        let rotation = i32::from(point.rotation_x).wrapping_add(
            (i32::from(next_rotation_x)
                .wrapping_sub(i32::from(point.rotation_x))
                .wrapping_mul(fraction))
                >> 8,
        );
        Ok(Angle12::new(rotation))
    }

    /// Samples the exact source `ZonePathProgressToLoc` camera transform.
    ///
    /// A fractional final point may interpolate into the first eligible
    /// following path. Mode-one automatic cameras deliberately refuse that
    /// cross-path interpolation, matching the retail boundary.
    pub fn pose(&self, graph: &RetailZoneGraph) -> Result<RetailCameraPose, RetailCameraError> {
        let zone = graph.zone(self.location.path.zone).ok_or_else(|| {
            RetailCameraError::Graph(FormatError::global(format!(
                "camera zone {} is absent",
                self.location.path.zone
            )))
        })?;
        let path = graph.path(self.location.path).ok_or_else(|| {
            RetailCameraError::Graph(FormatError::global(format!(
                "camera path {}:{} is absent",
                self.location.path.zone, self.location.path.index
            )))
        })?;
        let point_index = usize::from(self.location.progress.point_index());
        let point = path.points.get(point_index).ok_or({
            RetailCameraError::InvalidPathPointCount {
                path: self.location.path,
                point_count: path.points.len(),
            }
        })?;
        let fraction = i32::from(self.location.progress.fraction());
        let mut next_zone = zone;
        let mut next_point = *path
            .points
            .get(point_index.saturating_add(1))
            .unwrap_or(point);

        if fraction != 0
            && point_index == path.points.len() - 1
            && let Some((neighbor_index, link)) = path
                .neighbors
                .iter()
                .copied()
                .enumerate()
                .find(|(_, link)| link.relation & 2 != 0)
        {
            let (next_path_id, _) = graph.resolve_neighbor(self.location.path, neighbor_index)?;
            let following = graph.path(next_path_id).ok_or_else(|| {
                RetailCameraError::Graph(FormatError::global(format!(
                    "camera path {}:{} is absent",
                    next_path_id.zone, next_path_id.index
                )))
            })?;
            if following.camera_mode != 1 {
                next_zone = graph.zone(next_path_id.zone).ok_or_else(|| {
                    RetailCameraError::Graph(FormatError::global(format!(
                        "camera zone {} is absent",
                        next_path_id.zone
                    )))
                })?;
                let next_index = if link.goal & 2 != 0 {
                    following.points.len() - 1
                } else {
                    0
                };
                next_point = following.points[next_index];
            }
        }

        let world_translation = |origin: [i32; 3], coordinates: [i16; 3]| {
            [
                origin[0]
                    .wrapping_add(i32::from(coordinates[0]))
                    .wrapping_shl(8),
                origin[1]
                    .wrapping_add(i32::from(coordinates[1]))
                    .wrapping_shl(8),
                origin[2]
                    .wrapping_add(i32::from(coordinates[2]))
                    .wrapping_shl(8),
            ]
        };
        let current_translation = world_translation(zone.origin, [point.x, point.y, point.z]);
        let next_translation =
            world_translation(next_zone.origin, [next_point.x, next_point.y, next_point.z]);
        let interpolate = |current: i32, next: i32| {
            current.wrapping_add(next.wrapping_sub(current).wrapping_mul(fraction) >> 8)
        };
        let current_rotation = [
            i32::from(point.rotation_y),
            i32::from(point.rotation_x),
            i32::from(point.rotation_z),
        ];
        let next_rotation = [
            i32::from(next_point.rotation_y),
            i32::from(next_point.rotation_x),
            i32::from(next_point.rotation_z),
        ];
        let rotation_y = current_rotation[0].wrapping_add(
            i32::from(
                Angle12::new(current_rotation[0]).difference_to(Angle12::new(next_rotation[0])),
            )
            .wrapping_mul(fraction)
                >> 8,
        );

        Ok(RetailCameraPose {
            translation: [0_usize, 1, 2]
                .map(|axis| interpolate(current_translation[axis], next_translation[axis])),
            rotation_yxz: [
                rotation_y,
                interpolate(current_rotation[1], next_rotation[1]),
                interpolate(current_rotation[2], next_rotation[2]),
            ],
        })
    }

    /// Executes the retail mode-five/six `CamFollow` path selection slice.
    ///
    /// The ordinary [`Self::update`] method deliberately retains its explicit
    /// boundary when no Crash/player transform is available. Supplying that
    /// state here enables exact projection and smoothing. As with automatic
    /// camera updates, failures are transactional.
    pub fn update_follow(
        &mut self,
        graph: &RetailZoneGraph,
        input: RetailCameraFollowInput,
    ) -> Result<RetailCameraStep, RetailCameraError> {
        let before_runtime = *self;
        let mut next = before_runtime;
        match next.update_follow_inner(graph, input) {
            Ok(step) => {
                *self = next;
                Ok(step)
            }
            Err(error) => Err(error),
        }
    }

    /// Executes retail `CamUpdate` with no world-map globals bound.
    ///
    /// Modes seven and eight return an [`RetailCameraOutcome::IslandBoundary`]
    /// no-op through this compatibility entry point. Hosts with the title/GOOL
    /// globals should call [`Self::update_with_island`] instead.
    pub fn update(
        &mut self,
        graph: &RetailZoneGraph,
        input: RetailCameraInput,
    ) -> Result<RetailCameraStep, RetailCameraError> {
        self.update_with_island(graph, input, None)
    }

    /// Executes retail `CamUpdate`, including modes seven/eight when their
    /// external world-map globals are supplied.
    pub fn update_with_island(
        &mut self,
        graph: &RetailZoneGraph,
        input: RetailCameraInput,
        island: Option<RetailIslandCameraInput>,
    ) -> Result<RetailCameraStep, RetailCameraError> {
        let before = self.location;
        let initial_path = graph.path(before.path).ok_or_else(|| {
            RetailCameraError::Graph(FormatError::global(format!(
                "camera path {}:{} is absent",
                before.path.zone, before.path.index
            )))
        })?;
        match initial_path.camera_mode {
            0 => {
                self.game_state = GAME_STATE_PLAYING;
                Ok(self.step(before, RetailCameraOutcome::Stationary, Vec::new()))
            }
            mode @ (5 | 6) => {
                self.game_state = GAME_STATE_PLAYING;
                Ok(self.step(
                    before,
                    RetailCameraOutcome::FollowBoundary { mode },
                    Vec::new(),
                ))
            }
            1 | 3 => self.update_automatic(graph, input, before),
            mode @ (7 | 8) => match island {
                Some(island) => self.update_island(graph, island),
                None => Ok(self.step(
                    before,
                    RetailCameraOutcome::IslandBoundary { mode },
                    Vec::new(),
                )),
            },
            // Retail's default switch arm returns success without touching
            // either the camera location or `game_state`. In particular, the
            // shipped streams use modes two and four as authored no-op camera
            // paths; rejecting them would halt otherwise valid levels.
            _ => Ok(self.step(before, RetailCameraOutcome::Stationary, Vec::new())),
        }
    }

    /// Executes the source world-map modes with explicit title/GOOL globals.
    ///
    /// Like [`Self::update_follow`], errors are transactional: graph failures
    /// and malformed cycles leave the persistent camera byte-for-byte unchanged.
    pub fn update_island(
        &mut self,
        graph: &RetailZoneGraph,
        input: RetailIslandCameraInput,
    ) -> Result<RetailCameraStep, RetailCameraError> {
        let before_runtime = *self;
        let mut next = before_runtime;
        match next.update_island_inner(graph, input) {
            Ok(step) => {
                *self = next;
                Ok(step)
            }
            Err(error) => Err(error),
        }
    }

    fn update_island_inner(
        &mut self,
        graph: &RetailZoneGraph,
        input: RetailIslandCameraInput,
    ) -> Result<RetailCameraStep, RetailCameraError> {
        let before = self.location;
        let mode = graph
            .path(before.path)
            .ok_or_else(|| {
                RetailCameraError::Graph(FormatError::global(format!(
                    "camera path {}:{} is absent",
                    before.path.zone, before.path.index
                )))
            })?
            .camera_mode;
        match mode {
            7 => self.update_island_orbit(graph, input, before),
            8 => self.update_island_directed(graph, input, before),
            mode => Err(RetailCameraError::IslandModeRequired {
                path: before.path,
                mode,
            }),
        }
    }

    fn update_island_orbit(
        &mut self,
        graph: &RetailZoneGraph,
        input: RetailIslandCameraInput,
        before: RetailCameraLocation,
    ) -> Result<RetailCameraStep, RetailCameraError> {
        let state_before = input.island_cam_state;
        let mut state_after = state_before;
        let mut next_state = state_before;
        let angle = if state_before == 0 {
            0
        } else if state_before == -1 {
            next_state = 1;
            -(i32::from(Angle12::new(input.island_cam_rot_x).raw()))
        } else if matches!(state_before, 5 | 6) {
            0x22
        } else {
            let camera_rotation_x = self.pose(graph)?.rotation_yxz[1];
            let difference = i32::from(
                Angle12::new(camera_rotation_x).difference_to(Angle12::new(input.island_cam_rot_x)),
            );
            if difference.unsigned_abs() < 0x17 {
                0
            } else {
                difference.signum()
            }
        };

        let mut cursor = before;
        let mut selected = None;
        let mut path_crossings = 0_u32;
        let mut visited = BTreeSet::new();
        visited.insert((cursor.path, cursor.progress.point_index()));
        loop {
            if angle == 0 {
                break;
            }
            selected = Some(cursor);
            let path = graph.path(cursor.path).ok_or_else(|| {
                RetailCameraError::Graph(FormatError::global(format!(
                    "camera path {}:{} is absent",
                    cursor.path.zone, cursor.path.index
                )))
            })?;
            if state_before == -1 {
                let point = path
                    .points
                    .get(usize::from(cursor.progress.point_index()))
                    .ok_or(RetailCameraError::InvalidPathPointCount {
                        path: cursor.path,
                        point_count: path.points.len(),
                    })?;
                let difference = Angle12::new(i32::from(point.rotation_y))
                    .difference_to(Angle12::new(angle.abs()));
                if difference.unsigned_abs() < 0x17 {
                    break;
                }
            }

            let raw_step = if next_state & 4 != 0 { 0x400 } else { 0x100 };
            let next_progress = cursor.progress.raw().wrapping_add(raw_step);
            let path_length = i32::from(retail_point_count(graph, cursor.path)?.get()) << 8;
            if next_progress >= path_length {
                let map_neighbors = path
                    .neighbors
                    .iter()
                    .map(|neighbor| MapNeighbor {
                        goal: neighbor.goal,
                    })
                    .collect::<Vec<_>>();
                let Some(neighbor_index) = select_island_neighbor(&map_neighbors, next_state)
                else {
                    break;
                };
                let (next_path, link) = graph.resolve_neighbor(cursor.path, neighbor_index)?;
                let next_count = retail_point_count(graph, next_path)?;
                let next_raw = if link.goal & 1 != 0 {
                    0
                } else {
                    (i32::from(next_count.get()) - 1) << 8
                };
                cursor = RetailCameraLocation {
                    path: next_path,
                    progress: PathProgress::clamped(next_raw, next_count),
                };
                path_crossings = path_crossings.saturating_add(1);
                if next_state & 4 != 0 || angle > 0 {
                    selected = Some(cursor);
                    break;
                }
            } else if next_state & 4 != 0 || angle > 0 {
                selected = Some(location_at_raw(graph, cursor.path, next_progress)?);
                break;
            } else {
                cursor = location_at_raw(graph, cursor.path, next_progress)?;
            }

            if cursor.path == before.path
                && cursor.progress.point_index() == before.progress.point_index()
            {
                break;
            }
            if !visited.insert((cursor.path, cursor.progress.point_index())) {
                return Err(RetailCameraError::IslandOrbitCycle { path: cursor.path });
            }
        }

        if state_before == -1 {
            state_after = next_state;
        }
        let after = selected.unwrap_or(before);
        let moved = after != before;
        self.location = after;
        let effects = moved
            .then_some(RetailCameraEffect::LevelUpdate {
                before,
                after,
                flags: 0,
            })
            .into_iter()
            .collect();
        Ok(self.step(
            before,
            RetailCameraOutcome::IslandAdvanced {
                mode: 7,
                state_before,
                state_after,
                path_crossings,
                moved,
            },
            effects,
        ))
    }

    fn update_island_directed(
        &mut self,
        graph: &RetailZoneGraph,
        input: RetailIslandCameraInput,
        before: RetailCameraLocation,
    ) -> Result<RetailCameraStep, RetailCameraError> {
        let state_before = input.island_cam_state;
        let path = graph.path(before.path).ok_or_else(|| {
            RetailCameraError::Graph(FormatError::global(format!(
                "camera path {}:{} is absent",
                before.path.zone, before.path.index
            )))
        })?;
        let desired_relation = ((state_before & 3) ^ 3) as u8;
        let neighbor_index = path
            .neighbors
            .iter()
            .position(|neighbor| neighbor.relation & 3 == desired_relation);
        let point_count = retail_point_count(graph, before.path)?;
        let point_index = i32::from(before.progress.point_index());
        let mut after = before;
        let mut called_level_update = false;
        let mut path_crossings = 0_u32;

        if state_before & 1 != 0 {
            if point_index + 1 < i32::from(point_count.get()) {
                after = location_at_raw(
                    graph,
                    before.path,
                    before.progress.raw().wrapping_add(PATH_POINT_STEP),
                )?;
                called_level_update = true;
            } else if let Some(neighbor_index) = neighbor_index {
                let (next_path, link) = graph.resolve_neighbor(before.path, neighbor_index)?;
                let next_count = retail_point_count(graph, next_path)?;
                let next_progress = if link.goal & 1 != 0 {
                    0
                } else {
                    (i32::from(next_count.get()) - 1) << 8
                };
                after = RetailCameraLocation {
                    path: next_path,
                    progress: PathProgress::clamped(next_progress, next_count),
                };
                called_level_update = true;
                path_crossings = 1;
            }
        } else if point_index > 0 {
            after = location_at_raw(
                graph,
                before.path,
                before.progress.raw().wrapping_sub(PATH_POINT_STEP),
            )?;
            called_level_update = true;
        } else if let Some(neighbor_index) = neighbor_index {
            let (next_path, link) = graph.resolve_neighbor(before.path, neighbor_index)?;
            let next_count = retail_point_count(graph, next_path)?;
            let next_progress = if link.goal & 1 != 0 {
                0
            } else {
                (i32::from(next_count.get()) - 1) << 8
            };
            after = RetailCameraLocation {
                path: next_path,
                progress: PathProgress::clamped(next_progress, next_count),
            };
            called_level_update = true;
            path_crossings = 1;
        }

        self.location = after;
        let final_mode = graph
            .path(after.path)
            .ok_or_else(|| {
                RetailCameraError::Graph(FormatError::global(format!(
                    "camera path {}:{} is absent",
                    after.path.zone, after.path.index
                )))
            })?
            .camera_mode;
        let state_after = if final_mode == 8 { state_before } else { 1 };
        let effects = called_level_update
            .then_some(RetailCameraEffect::LevelUpdate {
                before,
                after,
                flags: 0,
            })
            .into_iter()
            .collect();
        Ok(self.step(
            before,
            RetailCameraOutcome::IslandAdvanced {
                mode: 8,
                state_before,
                state_after,
                path_crossings,
                moved: after != before,
            },
            effects,
        ))
    }

    fn update_follow_inner(
        &mut self,
        graph: &RetailZoneGraph,
        input: RetailCameraFollowInput,
    ) -> Result<RetailCameraStep, RetailCameraError> {
        let before = self.location;
        let current_path = graph.path(before.path).ok_or_else(|| {
            RetailCameraError::Graph(FormatError::global(format!(
                "camera path {}:{} is absent",
                before.path.zone, before.path.index
            )))
        })?;
        let mode = current_path.camera_mode;
        if !matches!(mode, 5 | 6) {
            return Err(RetailCameraError::FollowModeRequired {
                path: before.path,
                mode,
            });
        }
        let current_zone = graph.zone(before.path.zone).ok_or_else(|| {
            RetailCameraError::Graph(FormatError::global(format!(
                "camera zone {} is absent",
                before.path.zone
            )))
        })?;
        let point_count = retail_point_count(graph, before.path)?;
        let point_index = i32::from(before.progress.point_index());
        let length = i32::from(point_count.get());
        let mut relation_flags = 0_u8;
        if point_index < length / 2 || point_index < 50 {
            relation_flags |= 1;
        }
        if point_index >= length / 2 || length - point_index < 50 {
            relation_flags |= 2;
        }

        update_follow_offsets(
            &mut self.follow,
            current_zone.graphics_flags,
            current_path,
            input,
        );
        update_follow_pan_zoom(
            graph,
            before.path,
            current_path,
            point_index,
            relation_flags,
            &mut self.follow,
        )?;
        let total_zoom = self
            .follow
            .offset_z
            .wrapping_add(self.follow.zoom)
            .wrapping_add(input.player_cam_zoom);

        let projection = FollowProjection {
            pan_x: self.follow.offset_x,
            pan_y: self.follow.offset_y,
            zoom: total_zoom,
        };
        let mut candidates = Vec::with_capacity(current_path.neighbors.len() + 1);
        if let Some(mut candidate) = project_follow_candidate(
            graph,
            before,
            before.path,
            projection,
            input.player_translation,
            current_zone.graphics_flags,
            0,
            false,
        )? {
            candidate.delta_progress =
                progress_delta(before.path, candidate.progress, before.progress.raw())?;
            candidate.direction = if candidate.progress < before.progress.raw() {
                1
            } else {
                2
            };
            candidates.push(candidate);
        }

        for (link_index, link) in current_path.neighbors.iter().copied().enumerate() {
            if link.goal & 4 != 0 && input.frames_elapsed.wrapping_sub(input.gem_stamp) > 15 {
                continue;
            }
            if link.relation & relation_flags == 0 {
                continue;
            }
            let (target_path_id, _) = graph.resolve_neighbor(before.path, link_index)?;
            let target_path = graph.path(target_path_id).ok_or_else(|| {
                RetailCameraError::Graph(FormatError::global(format!(
                    "camera target path {}:{} is absent",
                    target_path_id.zone, target_path_id.index
                )))
            })?;
            let same_direction = current_path
                .direction
                .iter()
                .zip(target_path.direction.iter())
                .all(|(left, right)| left.unsigned_abs() == right.unsigned_abs());
            let Some(mut candidate) = project_follow_candidate(
                graph,
                before,
                target_path_id,
                projection,
                input.player_translation,
                current_zone.graphics_flags,
                relation_flags,
                !same_direction,
            )?
            else {
                continue;
            };

            let (exit, distance_to_exit) = if link.relation & 1 != 0 {
                (0, before.progress.raw())
            } else {
                let end = path_maximum_raw(graph, before.path)?;
                (end, end.wrapping_sub(before.progress.raw()))
            };
            let (entrance, relation, distance_on_target) = if link.goal & 1 != 0 {
                (0, 2, candidate.progress)
            } else {
                let end = path_maximum_raw(graph, target_path_id)?;
                (end, 1, end.wrapping_sub(candidate.progress))
            };
            let delta_progress = distance_to_exit
                .wrapping_add(distance_on_target)
                .wrapping_add(PATH_POINT_STEP);
            if delta_progress < 0 {
                return Err(RetailCameraError::FollowArithmetic {
                    path: target_path_id,
                    operation: "neighbor progress delta",
                });
            }
            candidate.exit = exit;
            candidate.entrance = entrance;
            candidate.relation = relation;
            candidate.delta_progress = delta_progress;
            candidate.direction = i32::from(link.relation);
            candidates.push(candidate);
        }

        self.game_state = GAME_STATE_PLAYING;
        let candidate_count =
            u8::try_from(candidates.len()).map_err(|_| RetailCameraError::FollowArithmetic {
                path: before.path,
                operation: "candidate count",
            })?;
        let Some(selected_index) = select_follow_candidate(graph, &candidates)? else {
            return Ok(self.step(
                before,
                RetailCameraOutcome::FollowEvaluated {
                    mode,
                    candidate_count,
                    moved: false,
                    crossed_path: false,
                },
                Vec::new(),
            ));
        };
        let selected = candidates[selected_index];
        if selected.delta_progress == 0 {
            return Ok(self.step(
                before,
                RetailCameraOutcome::FollowEvaluated {
                    mode,
                    candidate_count,
                    moved: false,
                    crossed_path: false,
                },
                Vec::new(),
            ));
        }

        let average_node_distance = i32::from(current_path.average_node_distance);
        if average_node_distance == 0 {
            return Err(RetailCameraError::InvalidAverageNodeDistance {
                path: before.path,
                distance: current_path.average_node_distance,
            });
        }
        let distance_delta = selected.delta_progress.wrapping_mul(average_node_distance);
        if distance_delta <= 30_000 {
            self.location = location_at_raw(graph, selected.path, selected.progress)?;
            self.follow.speed = selected.delta_progress;
        } else {
            self.follow.speed = if selected.delta_progress <= 0x200 {
                selected.delta_progress / 2
            } else if selected.delta_progress < 0x500 {
                0x200
            } else {
                (selected.delta_progress / 2)
                    .min(self.follow.speed)
                    .wrapping_add(PATH_POINT_STEP)
            };
            self.location = adjust_follow_progress(graph, before, self.follow.speed, selected)?;
        }
        let crossed_path = self.location.path != before.path;
        let moved = self.location != before;
        let effects = if crossed_path {
            vec![RetailCameraEffect::LevelUpdate {
                before,
                after: self.location,
                flags: 0,
            }]
        } else {
            Vec::new()
        };
        Ok(self.step(
            before,
            RetailCameraOutcome::FollowEvaluated {
                mode,
                candidate_count,
                moved,
                crossed_path,
            },
            effects,
        ))
    }

    fn update_automatic(
        &mut self,
        graph: &RetailZoneGraph,
        input: RetailCameraInput,
        before: RetailCameraLocation,
    ) -> Result<RetailCameraStep, RetailCameraError> {
        // The C routine captures `header` before its do/while loop. Both the
        // skip gate and every cutscene-state assignment therefore use the
        // initial zone flags even when a skip crosses into another zone.
        let initial_flags = graph
            .zone(before.path.zone)
            .ok_or_else(|| {
                RetailCameraError::Graph(FormatError::global(format!(
                    "camera zone {} is absent",
                    before.path.zone
                )))
            })?
            .graphics_flags;
        let skipped = input.tapped & AUTO_SKIP_BUTTON_MASK != 0
            && initial_flags & AUTO_SKIP_DISABLE_FLAGS == 0;

        let mut location = before;
        let mut game_state = self.game_state;
        let mut effects = Vec::new();
        let mut path_crossings = 0_u32;
        let mut skip_visited = BTreeSet::new();
        loop {
            if skipped && !skip_visited.insert(location.path) {
                return Err(RetailCameraError::AutoSkipCycle {
                    path: location.path,
                });
            }
            if initial_flags & SAVE_STATE_DISABLE_FLAG == 0 {
                game_state = GAME_STATE_CUTSCENE;
            }

            let point_count = retail_point_count(graph, location.path)?;
            let next_point = u32::from(location.progress.point_index()) + 1;
            if next_point < u32::from(point_count.get()) && !skipped {
                location.progress = location.progress.advance(PATH_POINT_STEP, point_count);
                self.location = location;
                self.game_state = game_state;
                return Ok(self.step(
                    before,
                    RetailCameraOutcome::AutoAdvanced {
                        skipped,
                        path_crossings,
                    },
                    effects,
                ));
            }

            // Retail asks `ZoneGetNeighborPath(..., 0)` at an automatic
            // path's endpoint and returns success without moving when that
            // lookup yields null. Some shipped cutscene paths (including the
            // Intro's terminal `t1_UZ:0`) intentionally have no link zero.
            // Check the validated slice before resolving it so the Rust port
            // preserves that authored terminal hold without reproducing the
            // C routine's preceding out-of-bounds neighbor-record read.
            let has_next_path = !graph
                .path(location.path)
                .ok_or_else(|| {
                    RetailCameraError::Graph(FormatError::global(format!(
                        "camera path {}:{} is absent",
                        location.path.zone, location.path.index
                    )))
                })?
                .neighbors
                .is_empty();
            if !has_next_path {
                self.location = location;
                self.game_state = game_state;
                return Ok(self.step(
                    before,
                    RetailCameraOutcome::AutoAdvanced {
                        skipped,
                        path_crossings,
                    },
                    effects,
                ));
            }

            let crossing_before = location;
            let (target_path, link) = graph.resolve_neighbor(location.path, 0)?;
            let target_count = retail_point_count(graph, target_path)?;
            let target_progress = if link.goal & 1 != 0 {
                PathProgress::ZERO
            } else {
                let last_point = i32::from(target_count.get()) - 1;
                PathProgress::clamped(last_point * PATH_POINT_STEP, target_count)
            };
            location = RetailCameraLocation {
                path: target_path,
                progress: target_progress,
            };
            path_crossings = path_crossings.saturating_add(1);
            effects.push(RetailCameraEffect::LevelUpdate {
                before: crossing_before,
                after: location,
                flags: 0,
            });

            let target_flags = graph
                .zone(target_path.zone)
                .ok_or_else(|| {
                    RetailCameraError::Graph(FormatError::global(format!(
                        "camera target zone {} is absent",
                        target_path.zone
                    )))
                })?
                .graphics_flags;
            if target_flags & SAVE_STATE_DISABLE_FLAG == 0 {
                effects.push(RetailCameraEffect::SaveStateHandshake { location });
            }

            if !skipped {
                self.location = location;
                self.game_state = game_state;
                return Ok(self.step(
                    before,
                    RetailCameraOutcome::AutoAdvanced {
                        skipped,
                        path_crossings,
                    },
                    effects,
                ));
            }

            let target_mode = graph
                .path(target_path)
                .ok_or_else(|| {
                    RetailCameraError::Graph(FormatError::global(format!(
                        "camera target path {}:{} is absent",
                        target_path.zone, target_path.index
                    )))
                })?
                .camera_mode;
            if !matches!(target_mode, 1 | 3) {
                self.location = location;
                self.game_state = game_state;
                return Ok(self.step(
                    before,
                    RetailCameraOutcome::AutoAdvanced {
                        skipped,
                        path_crossings,
                    },
                    effects,
                ));
            }
        }
    }

    fn step(
        &self,
        before: RetailCameraLocation,
        outcome: RetailCameraOutcome,
        effects: Vec<RetailCameraEffect>,
    ) -> RetailCameraStep {
        RetailCameraStep {
            before,
            after: self.location,
            outcome,
            game_state: self.game_state,
            effects,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FollowProjection {
    pan_x: i32,
    pan_y: i32,
    zoom: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FollowCandidate {
    path: RetailPathId,
    next_path: Option<RetailPathId>,
    progress: i32,
    exit: i32,
    entrance: i32,
    delta_progress: i32,
    distance: u32,
    direction: i32,
    relation: i32,
    progress_made: bool,
}

fn update_follow_offsets(
    state: &mut RetailCameraFollowState,
    zone_flags: u32,
    path: &ZonePath,
    input: RetailCameraFollowInput,
) {
    if zone_flags & ZONE_AUTO_CAM_Z_OFFSET == 0 && state.zoom > 0x31fff {
        if input.held_buttons & PAD_UP != 0 {
            state.offset_dir_z = false;
        } else if input.held_buttons & PAD_DOWN != 0 {
            state.offset_dir_z = true;
        }
    } else {
        state.offset_dir_z = zone_flags & ZONE_BACKWARD != 0;
    }
    if input.held_buttons & PAD_LEFT != 0 {
        state.offset_dir_x = false;
    } else if input.held_buttons & PAD_RIGHT != 0 {
        state.offset_dir_x = true;
    }

    if state.offset_dir_z {
        let maximum = if input.level_id == 3 {
            0x4b000
        } else {
            0x12c00
        };
        state.offset_z = state.offset_z.wrapping_add(0x3200).min(maximum);
    } else {
        state.offset_z = state.offset_z.wrapping_sub(0x3200).max(DEFAULT_OFFSET_Z);
    }

    if zone_flags & ZONE_SIDE_SCROLL == 0 {
        state.offset_x = 0;
    } else if path.direction[0] != 0 {
        if state.offset_dir_x {
            let maximum = if path.direction[0] > 0 { 307_200 } else { 0 };
            state.offset_x = state.offset_x.wrapping_add(25_600).min(maximum);
        } else {
            let minimum = if path.direction[0] < 0 { -307_200 } else { 0 };
            state.offset_x = state.offset_x.wrapping_sub(25_600).max(minimum);
        }
    }
}

fn update_follow_pan_zoom(
    graph: &RetailZoneGraph,
    current_id: RetailPathId,
    current: &ZonePath,
    point_index: i32,
    relation_flags: u8,
    state: &mut RetailCameraFollowState,
) -> Result<(), RetailCameraError> {
    let mut seek_pan = false;
    let mut seek_zoom = false;
    let mut new_pan = 0_i32;
    let mut new_zoom = 0_i32;
    for (link_index, link) in current.neighbors.iter().copied().enumerate() {
        if link.relation & relation_flags == 0 {
            continue;
        }
        let (target_id, _) = graph.resolve_neighbor(current_id, link_index)?;
        let target = graph.path(target_id).ok_or_else(|| {
            RetailCameraError::Graph(FormatError::global(format!(
                "camera target path {}:{} is absent",
                target_id.zone, target_id.index
            )))
        })?;
        if target.direction[1] != 0 {
            new_pan = i32::from(target.camera_zoom) << 8;
        }
        if target.direction[2] != 0 {
            new_zoom = i32::from(target.camera_zoom) << 8;
        }
    }
    if current.direction[1] != 0 {
        seek_pan = true;
        new_pan = i32::from(current.camera_zoom) << 8;
    }
    if current.direction[2] != 0 {
        seek_zoom = true;
        new_zoom = i32::from(current.camera_zoom) << 8;
    }
    if new_zoom != 0
        && point_index >= 11
        && point_index < i32::try_from(current.points.len()).unwrap_or(i32::MAX) - 10
    {
        state.zoom = if seek_zoom {
            retail_seek(state.zoom, new_zoom, 0x1900)
        } else {
            new_zoom
        };
    }
    if new_pan != 0 {
        state.offset_y = if seek_pan {
            retail_seek(state.offset_y, new_pan, 0x6400)
        } else {
            new_pan
        };
    }
    Ok(())
}

fn retail_seek(current: i32, target: i32, delta: i32) -> i32 {
    let difference = target.wrapping_sub(current);
    if delta > 0 && difference.unsigned_abs() < delta as u32 {
        target
    } else if difference > 0 {
        current.wrapping_add(delta)
    } else {
        current.wrapping_sub(delta)
    }
}

#[allow(clippy::too_many_arguments)]
fn project_follow_candidate(
    graph: &RetailZoneGraph,
    current: RetailCameraLocation,
    target_id: RetailPathId,
    projection: FollowProjection,
    player: Vec3,
    current_zone_flags: u32,
    bound_flags: u8,
    enforce_path_bounds: bool,
) -> Result<Option<FollowCandidate>, RetailCameraError> {
    let target = graph.path(target_id).ok_or_else(|| {
        RetailCameraError::Graph(FormatError::global(format!(
            "camera target path {}:{} is absent",
            target_id.zone, target_id.index
        )))
    })?;
    let projected = if target.direction[2] != 0 {
        project_follow_near_plane(
            graph,
            current,
            target_id,
            target,
            projection,
            player,
            bound_flags,
            enforce_path_bounds,
        )?
    } else {
        project_follow_linear(
            graph,
            current,
            target_id,
            target,
            projection,
            player,
            current_zone_flags,
            bound_flags,
            enforce_path_bounds,
        )?
    };
    Ok(projected.map(|projection_result| FollowCandidate {
        path: target_id,
        next_path: (target_id != current.path).then_some(target_id),
        progress: projection_result.progress,
        exit: 0,
        entrance: 0,
        delta_progress: 0,
        distance: projection_result.distance,
        direction: 0,
        relation: 0,
        progress_made: projection_result.progress_made,
    }))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProjectionResult {
    progress: i32,
    distance: u32,
    progress_made: bool,
}

#[allow(clippy::too_many_arguments)]
fn project_follow_linear(
    graph: &RetailZoneGraph,
    current: RetailCameraLocation,
    target_id: RetailPathId,
    path: &ZonePath,
    projection: FollowProjection,
    player: Vec3,
    current_zone_flags: u32,
    bound_flags: u8,
    enforce_path_bounds: bool,
) -> Result<Option<ProjectionResult>, RetailCameraError> {
    let origin = graph.zone(target_id.zone).ok_or_else(|| {
        RetailCameraError::Graph(FormatError::global(format!(
            "camera target zone {} is absent",
            target_id.zone
        )))
    })?;
    let first = path
        .points
        .first()
        .ok_or(RetailCameraError::InvalidPathPointCount {
            path: target_id,
            point_count: 0,
        })?;
    let projected_player = [
        player.x.wrapping_add(projection.pan_x),
        player.y.wrapping_add(projection.pan_y),
        player.z.wrapping_add(projection.zoom),
    ];
    let path_start = [
        origin.origin[0].wrapping_add(i32::from(first.x)),
        origin.origin[1].wrapping_add(i32::from(first.y)),
        origin.origin[2].wrapping_add(i32::from(first.z)),
    ];
    let relative = [
        (projected_player[0] >> 4).wrapping_sub(path_start[0].wrapping_shl(4)),
        (projected_player[1] >> 4).wrapping_sub(path_start[1].wrapping_shl(4)),
        (projected_player[2] >> 4).wrapping_sub(path_start[2].wrapping_shl(4)),
    ];
    let dot = i32::from(path.direction[0])
        .wrapping_mul(relative[0])
        .wrapping_add(i32::from(path.direction[1]).wrapping_mul(relative[1]))
        .wrapping_add(i32::from(path.direction[2]).wrapping_mul(relative[2]));
    let average = i32::from(path.average_node_distance);
    if average == 0 {
        return Err(RetailCameraError::InvalidAverageNodeDistance {
            path: target_id,
            distance: path.average_node_distance,
        });
    }
    let raw_distance = dot >> 8;
    let progress =
        raw_distance
            .checked_div(average)
            .ok_or(RetailCameraError::FollowArithmetic {
                path: target_id,
                operation: "linear progress division",
            })?;
    if target_id != current.path
        && path.direction[0] != 0
        && relative[1] < -12_800
        && current_zone_flags & ZONE_DISCARD_BELOW_PATHS != 0
    {
        return Ok(None);
    }
    let Some((progress, point_index, progress_made)) = validate_projected_progress(
        current.path,
        target_id,
        path,
        progress,
        true,
        bound_flags,
        enforce_path_bounds,
    )?
    else {
        return Ok(None);
    };
    let point = path.points[point_index];
    let adjustment = [
        player.x.wrapping_add(projection.pan_x) >> 8,
        player.y.wrapping_add(projection.pan_y) >> 8,
        player.z.wrapping_add(projection.zoom) >> 8,
    ];
    let point_world = [
        origin.origin[0].wrapping_add(i32::from(point.x)),
        origin.origin[1].wrapping_add(i32::from(point.y)),
        origin.origin[2].wrapping_add(i32::from(point.z)),
    ];
    let delta = [
        adjustment[0].wrapping_sub(point_world[0]),
        adjustment[1].wrapping_sub(point_world[1]),
        adjustment[2].wrapping_sub(point_world[2]),
    ];
    if !progress_made && delta.iter().any(|axis| axis.unsigned_abs() > 3_200) {
        return Ok(None);
    }
    Ok(Some(ProjectionResult {
        progress,
        distance: follow_distance(delta, target_id)?,
        progress_made,
    }))
}

#[allow(clippy::too_many_arguments)]
fn project_follow_near_plane(
    graph: &RetailZoneGraph,
    current: RetailCameraLocation,
    target_id: RetailPathId,
    path: &ZonePath,
    projection: FollowProjection,
    player: Vec3,
    bound_flags: u8,
    enforce_path_bounds: bool,
) -> Result<Option<ProjectionResult>, RetailCameraError> {
    let origin = graph.zone(target_id.zone).ok_or_else(|| {
        RetailCameraError::Graph(FormatError::global(format!(
            "camera target zone {} is absent",
            target_id.zone
        )))
    })?;
    let mut progress = -1_i32;
    let mut nearest_distance = 0_i32;
    let mut progress_made = true;
    for (index, point) in path.points.iter().copied().enumerate() {
        let point_x = origin.origin[0].wrapping_add(i32::from(point.x));
        let point_z = origin.origin[2].wrapping_add(i32::from(point.z));
        let relative_x = (player.x >> 8).wrapping_sub(point_x);
        let relative_z = (player.z >> 8).wrapping_sub(point_z);
        let sin = i32::from(Angle12::new(i32::from(point.rotation_x)).sin_q12());
        let cos = i32::from(Angle12::new(i32::from(point.rotation_x)).cos_q12());
        let plane_distance = projection.zoom.wrapping_mul(-16).wrapping_sub(
            relative_x
                .wrapping_mul(sin)
                .wrapping_add(relative_z.wrapping_mul(cos)),
        );
        let last_index = path.points.len() - 1;
        if path.direction[2] <= 0 {
            if index == 0 && plane_distance < 0 {
                progress = 0;
                if plane_distance < -128_000 {
                    progress_made = false;
                }
                break;
            }
            if index == last_index && plane_distance > 0 {
                progress = path_maximum_raw(graph, target_id)?;
                if plane_distance > 128_000 {
                    progress_made = false;
                }
                break;
            }
        } else {
            if index == 0 && plane_distance > 0 {
                progress = 0;
                if plane_distance > 128_000 {
                    progress_made = false;
                }
                break;
            }
            if index == last_index && plane_distance < 0 {
                progress = path_maximum_raw(graph, target_id)?;
                if plane_distance < -128_000 {
                    progress_made = false;
                }
                break;
            }
        }

        let nearer = index == 0 || plane_distance.unsigned_abs() < nearest_distance.unsigned_abs();
        if nearer {
            if index == 0 || plane_distance ^ nearest_distance >= 0 {
                progress =
                    i32::try_from(index).map_err(|_| RetailCameraError::FollowArithmetic {
                        path: target_id,
                        operation: "near-plane point index",
                    })? << 8;
            } else {
                let previous = nearest_distance.unsigned_abs();
                let current_distance = plane_distance.unsigned_abs();
                let numerator = previous.checked_mul(PATH_POINT_STEP as u32).ok_or(
                    RetailCameraError::FollowArithmetic {
                        path: target_id,
                        operation: "near-plane interpolation numerator",
                    },
                )?;
                let denominator = previous.checked_add(current_distance).ok_or(
                    RetailCameraError::FollowArithmetic {
                        path: target_id,
                        operation: "near-plane interpolation denominator",
                    },
                )?;
                if denominator == 0 || numerator > i32::MAX as u32 || denominator > i32::MAX as u32
                {
                    return Err(RetailCameraError::FollowArithmetic {
                        path: target_id,
                        operation: "near-plane interpolation",
                    });
                }
                let fraction = (numerator / denominator) as i32;
                progress = progress.wrapping_add(fraction);
            }
            nearest_distance = plane_distance;
        }
    }

    let Some((progress, point_index, progress_made)) = validate_projected_progress(
        current.path,
        target_id,
        path,
        progress,
        progress_made,
        bound_flags,
        enforce_path_bounds,
    )?
    else {
        return Ok(None);
    };
    let point = path.points[point_index];
    let adjustment = [
        player.x.wrapping_add(projection.pan_x) >> 8,
        player.y.wrapping_add(projection.pan_y) >> 8,
        player.z.wrapping_add(projection.zoom) >> 8,
    ];
    let point_world = [
        origin.origin[0].wrapping_add(i32::from(point.x)),
        origin.origin[1].wrapping_add(i32::from(point.y)),
        origin.origin[2].wrapping_add(i32::from(point.z)),
    ];
    let delta = [
        adjustment[0].wrapping_sub(point_world[0]),
        adjustment[1].wrapping_sub(point_world[1]),
        adjustment[2].wrapping_sub(point_world[2]),
    ];
    Ok(Some(ProjectionResult {
        progress,
        distance: follow_distance(delta, target_id)?,
        progress_made,
    }))
}

#[allow(clippy::too_many_arguments)]
fn validate_projected_progress(
    current_id: RetailPathId,
    target_id: RetailPathId,
    path: &ZonePath,
    mut progress: i32,
    mut progress_made: bool,
    bound_flags: u8,
    enforce_path_bounds: bool,
) -> Result<Option<(i32, usize, bool)>, RetailCameraError> {
    let length =
        i32::try_from(path.points.len()).map_err(|_| RetailCameraError::FollowArithmetic {
            path: target_id,
            operation: "path length",
        })?;
    let entrance = if enforce_path_bounds && bound_flags & 1 != 0 {
        i32::from(path.entrance_index)
    } else {
        0
    };
    let exit = if enforce_path_bounds && bound_flags & 2 != 0 {
        i32::from(path.exit_index)
    } else {
        0
    };
    let point_index = progress >> 8;
    if point_index >= entrance {
        if point_index >= length - exit {
            if exit != 0 || target_id != current_id {
                return Ok(None);
            }
            progress = (length << 8) - 1;
            progress_made = false;
        }
    } else {
        if entrance != 0 || target_id != current_id {
            return Ok(None);
        }
        progress = 0;
        progress_made = false;
    }
    let point_index =
        usize::try_from(progress >> 8).map_err(|_| RetailCameraError::FollowArithmetic {
            path: target_id,
            operation: "projected point index",
        })?;
    let point_index = path.points.get(point_index).map(|_| point_index).ok_or(
        RetailCameraError::FollowArithmetic {
            path: target_id,
            operation: "projected point lookup",
        },
    )?;
    Ok(Some((progress, point_index, progress_made)))
}

fn follow_distance(delta: [i32; 3], path: RetailPathId) -> Result<u32, RetailCameraError> {
    // The PS1 keeps the low 32 bits of the three integer products before its
    // square-root instruction. Model that wrap explicitly as an unsigned
    // radicand; rejecting bit 31 made the two hog paths terminate once their
    // perfectly valid camera delta crossed the signed range.
    let squared = delta[0]
        .wrapping_mul(delta[0])
        .wrapping_add(delta[1].wrapping_mul(delta[1]))
        .wrapping_add(delta[2].wrapping_mul(delta[2])) as u32;
    retail_camera_sqrt(squared).ok_or(RetailCameraError::FollowArithmetic {
        path,
        operation: "retail square root",
    })
}

fn retail_camera_sqrt(value: u32) -> Option<u32> {
    if value == 0 {
        return Some(0);
    }
    let leading = value.leading_zeros() & !1;
    let index = if leading < 24 {
        value >> (24 - leading)
    } else {
        value << (leading - 24)
    };
    if !(64..=255).contains(&index) {
        return None;
    }
    let table = integer_sqrt(u64::from(index) << 18);
    let scaled = table.checked_shl((31 - leading) / 2)?;
    Some(scaled >> 12)
}

fn progress_delta(path: RetailPathId, left: i32, right: i32) -> Result<i32, RetailCameraError> {
    let delta = left.wrapping_sub(right).unsigned_abs();
    i32::try_from(delta).map_err(|_| RetailCameraError::FollowArithmetic {
        path,
        operation: "progress delta",
    })
}

fn select_follow_candidate(
    graph: &RetailZoneGraph,
    candidates: &[FollowCandidate],
) -> Result<Option<usize>, RetailCameraError> {
    if candidates.is_empty() {
        return Ok(None);
    }
    let mut nearest_index = 0_usize;
    let mut nearest_distance = i32::MAX;
    for (index, candidate) in candidates.iter().copied().enumerate() {
        let mode = graph
            .path(candidate.path)
            .ok_or_else(|| {
                RetailCameraError::Graph(FormatError::global(format!(
                    "camera candidate path {}:{} is absent",
                    candidate.path.zone, candidate.path.index
                )))
            })?
            .camera_mode;
        if mode == 1 {
            return Ok(Some(index));
        }
        let nearest = candidates[nearest_index];
        let distance = candidate.distance as i32;
        if (!nearest.progress_made && candidate.progress_made)
            || (distance < nearest_distance && nearest.progress_made == candidate.progress_made)
        {
            nearest_index = index;
            nearest_distance = distance;
        }
    }
    Ok(Some(nearest_index))
}

fn adjust_follow_progress(
    graph: &RetailZoneGraph,
    current: RetailCameraLocation,
    speed: i32,
    selected: FollowCandidate,
) -> Result<RetailCameraLocation, RetailCameraError> {
    if selected.direction & 2 != 0 {
        let mut next_progress = current.progress.raw().wrapping_add(speed);
        if let Some(next_path) = selected.next_path {
            let current_count = retail_point_count(graph, current.path)?;
            if next_progress >> 8 >= i32::from(current_count.get()) {
                next_progress = next_progress
                    .wrapping_sub((i32::from(current_count.get()) << 8).wrapping_add(1));
                if selected.relation & 2 == 0 {
                    next_progress = next_progress.wrapping_neg();
                }
                next_progress = next_progress.wrapping_add(selected.entrance);
                return location_at_raw(graph, next_path, next_progress.min(selected.progress));
            }
        }
        return location_at_raw(graph, current.path, next_progress);
    }

    let mut next_progress = current.progress.raw().wrapping_sub(speed);
    if let Some(next_path) = selected.next_path
        && next_progress < 0
    {
        if selected.relation & 2 != 0 {
            next_progress = next_progress.wrapping_neg();
        }
        next_progress = next_progress.wrapping_add(selected.entrance);
        return location_at_raw(graph, next_path, next_progress.max(selected.progress));
    }
    location_at_raw(graph, current.path, next_progress)
}

fn location_at_raw(
    graph: &RetailZoneGraph,
    path: RetailPathId,
    raw_progress: i32,
) -> Result<RetailCameraLocation, RetailCameraError> {
    let point_count = retail_point_count(graph, path)?;
    Ok(RetailCameraLocation {
        path,
        progress: PathProgress::clamped(raw_progress, point_count),
    })
}

fn path_maximum_raw(graph: &RetailZoneGraph, path: RetailPathId) -> Result<i32, RetailCameraError> {
    let point_count = retail_point_count(graph, path)?;
    Ok((i32::from(point_count.get()) << 8) - 1)
}

fn retail_point_count(
    graph: &RetailZoneGraph,
    path: RetailPathId,
) -> Result<NonZeroU16, RetailCameraError> {
    let point_count = graph
        .path(path)
        .map_or(0, |candidate| candidate.points.len());
    u16::try_from(point_count)
        .ok()
        .and_then(NonZeroU16::new)
        .ok_or(RetailCameraError::InvalidPathPointCount { path, point_count })
}

/// One world-map path neighbor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MapNeighbor {
    pub goal: u8,
}

/// Retail preference: the last exact/direction-compatible match, then the first
/// goal without orbit bit 4, then no route.
#[must_use]
pub fn select_island_neighbor(neighbors: &[MapNeighbor], state: i32) -> Option<usize> {
    let mut selected = None;
    for (index, neighbor) in neighbors.iter().copied().enumerate() {
        if i32::from(neighbor.goal) == state
            || (selected.is_none()
                && (neighbors.len() == 1 || i32::from(neighbor.goal & 3) == (state & 3)))
        {
            selected = Some(index);
        }
    }
    selected.or_else(|| neighbors.iter().position(|neighbor| neighbor.goal & 4 == 0))
}

#[cfg(test)]
mod tests {
    use crust_formats::binary::{Eid, EntryRef};
    use crust_formats::stream::structs::ZonePathPoint;
    use crust_formats::stream::{RetailZoneNode, ZoneNeighborPath, ZonePath};

    use super::*;

    const TEST_ZONE: Eid = Eid::from_raw(1);

    #[test]
    fn follow_distance_accepts_a_wrapped_unsigned_ps1_radicand() {
        let path = RetailPathId {
            zone: TEST_ZONE,
            index: 0,
        };
        assert_eq!(follow_distance([50_000, 0, 0], path), Ok(49_992));
        assert_eq!(retail_camera_sqrt(2_500_000_000), Some(49_992));
    }

    fn retail_path(camera_mode: u16, point_count: usize, target: Option<(u8, u8, u8)>) -> ZonePath {
        let point = ZonePathPoint {
            x: 0,
            y: 0,
            z: 0,
            rotation_y: 0,
            rotation_x: 0,
            rotation_z: 0,
        };
        ZonePath {
            visibility_list: Eid::NONE,
            serialized_parent: EntryRef::from_raw(0),
            neighbors: target
                .map(|(neighbor_zone_index, path_index, goal)| {
                    vec![ZoneNeighborPath {
                        relation: 0,
                        neighbor_zone_index,
                        path_index,
                        goal,
                    }]
                })
                .unwrap_or_default(),
            entrance_index: 0,
            exit_index: 0,
            camera_mode,
            average_node_distance: 1,
            camera_zoom: 0,
            unknown: [0; 3],
            direction: [0; 3],
            points: vec![point; point_count],
        }
    }

    fn island_path(
        camera_mode: u16,
        point_count: usize,
        neighbors: &[(u8, u8, u8, u8)],
    ) -> ZonePath {
        let mut path = retail_path(camera_mode, point_count, None);
        path.neighbors = neighbors
            .iter()
            .copied()
            .map(
                |(relation, neighbor_zone_index, path_index, goal)| ZoneNeighborPath {
                    relation,
                    neighbor_zone_index,
                    path_index,
                    goal,
                },
            )
            .collect();
        path
    }

    fn follow_path(
        points: &[[i16; 3]],
        direction: [i16; 3],
        average_node_distance: i16,
        target: Option<(u8, u8, u8, u8)>,
    ) -> ZonePath {
        ZonePath {
            visibility_list: Eid::NONE,
            serialized_parent: EntryRef::from_raw(0),
            neighbors: target
                .map(|(relation, neighbor_zone_index, path_index, goal)| {
                    vec![ZoneNeighborPath {
                        relation,
                        neighbor_zone_index,
                        path_index,
                        goal,
                    }]
                })
                .unwrap_or_default(),
            entrance_index: 0,
            exit_index: 0,
            camera_mode: 5,
            average_node_distance,
            camera_zoom: 1_700,
            unknown: [0; 3],
            direction,
            points: points
                .iter()
                .map(|point| ZonePathPoint {
                    x: point[0],
                    y: point[1],
                    z: point[2],
                    rotation_y: 0,
                    rotation_x: i16::from(direction[2] != 0),
                    rotation_z: 0,
                })
                .collect(),
        }
    }

    fn single_zone_graph(graphics_flags: u32, paths: Vec<ZonePath>) -> RetailZoneGraph {
        RetailZoneGraph::new(
            RetailPathId {
                zone: TEST_ZONE,
                index: 0,
            },
            [RetailZoneNode::new(
                TEST_ZONE,
                [0; 3],
                graphics_flags,
                0,
                vec![TEST_ZONE],
                paths,
            )],
        )
        .unwrap()
    }

    #[test]
    fn island_neighbor_selection_matches_source_golden() {
        assert_eq!(
            select_island_neighbor(
                &[
                    MapNeighbor { goal: 6 },
                    MapNeighbor { goal: 1 },
                    MapNeighbor { goal: 5 }
                ],
                5,
            ),
            Some(2)
        );
        assert_eq!(
            select_island_neighbor(
                &[
                    MapNeighbor { goal: 6 },
                    MapNeighbor { goal: 2 },
                    MapNeighbor { goal: 7 }
                ],
                1,
            ),
            Some(1)
        );
        assert_eq!(
            select_island_neighbor(&[MapNeighbor { goal: 4 }, MapNeighbor { goal: 4 }], 2),
            None
        );
        assert_eq!(
            select_island_neighbor(&[MapNeighbor { goal: 1 }, MapNeighbor { goal: 1 }], 0x101),
            Some(0)
        );
    }

    #[test]
    fn retail_world_map_mode_without_external_globals_stops_at_typed_boundary() {
        for mode in [7, 8] {
            let graph = single_zone_graph(0, vec![island_path(mode, 2, &[])]);
            let mut camera = RetailCameraRuntime::new(&graph).unwrap();
            let before = camera;
            let step = camera.update(&graph, RetailCameraInput::default()).unwrap();

            assert_eq!(camera, before);
            assert_eq!(step.outcome, RetailCameraOutcome::IslandBoundary { mode });
            assert!(step.effects.is_empty());
        }
    }

    #[test]
    fn retail_mode_seven_traverses_local_orbit_but_emits_one_final_level_update() {
        let graph = single_zone_graph(
            0,
            vec![
                island_path(7, 1, &[(0, 0, 1, 1)]),
                island_path(7, 1, &[(0, 0, 2, 1)]),
                island_path(7, 1, &[(0, 0, 0, 1)]),
            ],
        );
        let mut camera = RetailCameraRuntime::new(&graph).unwrap();
        let before = camera.location();
        let step = camera
            .update_island(
                &graph,
                RetailIslandCameraInput {
                    island_cam_state: 1,
                    island_cam_rot_x: 0xf00,
                },
            )
            .unwrap();
        let after = RetailCameraLocation {
            path: RetailPathId {
                zone: TEST_ZONE,
                index: 2,
            },
            progress: PathProgress::ZERO,
        };

        assert_eq!(step.after, after);
        assert_eq!(
            step.outcome,
            RetailCameraOutcome::IslandAdvanced {
                mode: 7,
                state_before: 1,
                state_after: 1,
                path_crossings: 3,
                moved: true,
            }
        );
        assert_eq!(
            step.effects,
            [RetailCameraEffect::LevelUpdate {
                before,
                after,
                flags: 0,
            }]
        );
    }

    #[test]
    fn retail_mode_eight_advances_one_point_with_ordered_level_update() {
        let graph = single_zone_graph(0, vec![island_path(8, 3, &[])]);
        let mut camera = RetailCameraRuntime::new(&graph).unwrap();
        let before = camera.location();
        let step = camera
            .update_island(
                &graph,
                RetailIslandCameraInput {
                    island_cam_state: 1,
                    island_cam_rot_x: 0,
                },
            )
            .unwrap();

        assert_eq!(step.after.progress.raw(), PATH_POINT_STEP);
        assert_eq!(
            step.outcome,
            RetailCameraOutcome::IslandAdvanced {
                mode: 8,
                state_before: 1,
                state_after: 1,
                path_crossings: 0,
                moved: true,
            }
        );
        assert_eq!(
            step.effects,
            [RetailCameraEffect::LevelUpdate {
                before,
                after: step.after,
                flags: 0,
            }]
        );
    }

    #[test]
    fn retail_mode_eight_crosses_selected_neighbor_and_resets_state_off_mode_eight() {
        let graph = single_zone_graph(
            0,
            vec![island_path(8, 1, &[(0, 0, 1, 1)]), island_path(5, 2, &[])],
        );
        let mut camera = RetailCameraRuntime::new(&graph).unwrap();
        let before = camera.location();
        let step = camera
            .update_island(
                &graph,
                RetailIslandCameraInput {
                    island_cam_state: 3,
                    island_cam_rot_x: 0,
                },
            )
            .unwrap();

        assert_eq!(step.after.path.index, 1);
        assert_eq!(step.after.progress, PathProgress::ZERO);
        assert_eq!(
            step.outcome,
            RetailCameraOutcome::IslandAdvanced {
                mode: 8,
                state_before: 3,
                state_after: 1,
                path_crossings: 1,
                moved: true,
            }
        );
        assert_eq!(
            step.effects,
            [RetailCameraEffect::LevelUpdate {
                before,
                after: step.after,
                flags: 0,
            }]
        );
    }

    #[test]
    fn retail_mode_eight_stays_at_endpoint_without_selected_neighbor() {
        let graph = single_zone_graph(0, vec![island_path(8, 2, &[])]);
        let mut camera = RetailCameraRuntime::at_path(
            &graph,
            graph.spawn_path(),
            PATH_POINT_STEP,
            GAME_STATE_CUTSCENE,
        )
        .unwrap();
        let before = camera;
        let step = camera
            .update_island(
                &graph,
                RetailIslandCameraInput {
                    island_cam_state: 1,
                    island_cam_rot_x: 0,
                },
            )
            .unwrap();

        assert_eq!(camera, before);
        assert_eq!(step.before, step.after);
        assert!(step.effects.is_empty());
        assert_eq!(
            step.outcome,
            RetailCameraOutcome::IslandAdvanced {
                mode: 8,
                state_before: 1,
                state_after: 1,
                path_crossings: 0,
                moved: false,
            }
        );
    }

    #[test]
    fn retail_island_orbit_cycle_error_is_transactional() {
        let graph = single_zone_graph(
            0,
            vec![
                island_path(7, 2, &[(0, 0, 1, 1)]),
                island_path(7, 1, &[(0, 0, 1, 1)]),
            ],
        );
        let mut camera = RetailCameraRuntime::at_path(
            &graph,
            graph.spawn_path(),
            PATH_POINT_STEP,
            GAME_STATE_CUTSCENE,
        )
        .unwrap();
        let before = camera;
        let error = camera
            .update_island(
                &graph,
                RetailIslandCameraInput {
                    island_cam_state: 1,
                    island_cam_rot_x: 0xf00,
                },
            )
            .unwrap_err();

        assert_eq!(
            error,
            RetailCameraError::IslandOrbitCycle {
                path: RetailPathId {
                    zone: TEST_ZONE,
                    index: 1,
                },
            }
        );
        assert_eq!(camera, before);
    }

    #[test]
    fn death_camera_uses_characterized_acceleration() {
        let mut camera = CameraState {
            translation: Vec3 {
                x: 4_000,
                y: 5_000,
                z: 6_000,
            },
            ..CameraState::default()
        };
        camera.death_step(
            Vec3 {
                x: 1_000,
                y: 2_000,
                z: 3_000,
            },
            100,
            1_000,
            true,
        );
        assert_eq!(camera.death_acceleration, 22);
        assert_eq!(camera.death_orbit, 22);
        assert_eq!(camera.death_flip_velocity, 100);
        assert!(camera.translation.x.unsigned_abs() < 10_000_000);
        assert!(camera.translation.y.unsigned_abs() < 10_000_000);
        assert!(camera.translation.z.unsigned_abs() < 10_000_000);
    }

    #[test]
    fn retail_mode_zero_is_stationary_and_enables_gameplay_input() {
        let graph = single_zone_graph(0, vec![retail_path(0, 3, None)]);
        let mut camera =
            RetailCameraRuntime::at_path(&graph, graph.spawn_path(), 0x180, GAME_STATE_CUTSCENE)
                .unwrap();
        let before = camera.location();
        let step = camera.update(&graph, RetailCameraInput::default()).unwrap();

        assert_eq!(step.before, before);
        assert_eq!(step.after, before);
        assert_eq!(step.outcome, RetailCameraOutcome::Stationary);
        assert_eq!(step.game_state, GAME_STATE_PLAYING);
        assert!(step.effects.is_empty());
    }

    #[test]
    fn retail_default_camera_modes_are_successful_no_ops() {
        for mode in [2, 4, 9, u16::MAX] {
            let graph = single_zone_graph(0, vec![retail_path(mode, 3, None)]);
            let mut camera = RetailCameraRuntime::at_path(
                &graph,
                graph.spawn_path(),
                0x180,
                GAME_STATE_CUTSCENE,
            )
            .unwrap();
            let before = camera;
            let step = camera.update(&graph, RetailCameraInput::default()).unwrap();

            assert_eq!(camera, before, "mode {mode}");
            assert_eq!(step.before, before.location(), "mode {mode}");
            assert_eq!(step.after, before.location(), "mode {mode}");
            assert_eq!(step.outcome, RetailCameraOutcome::Stationary, "mode {mode}");
            assert_eq!(step.game_state, GAME_STATE_CUTSCENE, "mode {mode}");
            assert!(step.effects.is_empty(), "mode {mode}");
        }
    }

    #[test]
    fn explicit_level_update_clamps_and_preserves_flag_two_and_game_state() {
        let graph = single_zone_graph(0, vec![retail_path(0, 3, None), retail_path(2, 2, None)]);
        let mut camera =
            RetailCameraRuntime::at_path(&graph, graph.spawn_path(), 0x100, GAME_STATE_CUTSCENE)
                .unwrap();
        let before = camera.location();
        let target = RetailPathId {
            zone: TEST_ZONE,
            index: 1,
        };
        let step = camera.level_update(&graph, target, i32::MAX, 2).unwrap();

        assert_eq!(step.before, before);
        assert_eq!(step.after.path, target);
        assert_eq!(step.after.progress.raw(), 0x1ff);
        assert_eq!(step.game_state, GAME_STATE_CUTSCENE);
        assert_eq!(
            step.effects,
            [RetailCameraEffect::LevelUpdate {
                before,
                after: step.after,
                flags: 2,
            }]
        );
    }

    #[test]
    fn retail_camera_rotation_xz_interpolates_path_rotation_x() {
        let mut path = retail_path(0, 2, None);
        path.points[0].rotation_x = 0x100;
        path.points[1].rotation_x = 0x500;
        let graph = single_zone_graph(0, vec![path]);
        let camera =
            RetailCameraRuntime::at_path(&graph, graph.spawn_path(), 0x80, GAME_STATE_PLAYING)
                .unwrap();

        assert_eq!(camera.rotation_xz(&graph).unwrap(), Angle12::new(0x300));
    }

    #[test]
    fn retail_camera_pose_matches_source_path_interpolation() {
        let mut path = retail_path(0, 2, None);
        path.points[0] = ZonePathPoint {
            x: 10,
            y: -20,
            z: 30,
            rotation_y: 0x0f00,
            rotation_x: 0x100,
            rotation_z: -0x100,
        };
        path.points[1] = ZonePathPoint {
            x: 30,
            y: 20,
            z: 50,
            rotation_y: 0x0100,
            rotation_x: 0x500,
            rotation_z: 0x100,
        };
        let graph = RetailZoneGraph::new(
            RetailPathId {
                zone: TEST_ZONE,
                index: 0,
            },
            [RetailZoneNode::new(
                TEST_ZONE,
                [100, 200, 300],
                0,
                0,
                vec![TEST_ZONE],
                vec![path],
            )],
        )
        .unwrap();
        let camera =
            RetailCameraRuntime::at_path(&graph, graph.spawn_path(), 0x80, GAME_STATE_PLAYING)
                .unwrap();

        assert_eq!(
            camera.pose(&graph).unwrap(),
            RetailCameraPose {
                translation: [120 << 8, 200 << 8, 340 << 8],
                rotation_yxz: [0x1000, 0x300, 0],
            }
        );
    }

    #[test]
    fn retail_bounce_camera_forces_zero_physics_heading() {
        let mut path = retail_path(0, 1, None);
        path.points[0].rotation_x = 0x700;
        let graph = single_zone_graph(0x1000, vec![path]);
        let camera = RetailCameraRuntime::new(&graph).unwrap();

        assert_eq!(camera.rotation_xz(&graph).unwrap(), Angle12::new(0));
    }

    #[test]
    fn retail_modes_one_and_three_advance_one_whole_point_per_tick() {
        for mode in [1, 3] {
            let graph = single_zone_graph(0, vec![retail_path(mode, 3, None)]);
            let mut camera = RetailCameraRuntime::new(&graph).unwrap();
            let first = camera.update(&graph, RetailCameraInput::default()).unwrap();
            let second = camera.update(&graph, RetailCameraInput::default()).unwrap();

            assert_eq!(first.after.progress.raw(), PATH_POINT_STEP);
            assert_eq!(second.after.progress.raw(), 2 * PATH_POINT_STEP);
            assert_eq!(
                second.outcome,
                RetailCameraOutcome::AutoAdvanced {
                    skipped: false,
                    path_crossings: 0,
                }
            );
            assert_eq!(second.game_state, GAME_STATE_CUTSCENE);
            assert!(second.effects.is_empty());
        }
    }

    #[test]
    fn retail_automatic_crossing_obeys_goal_direction_and_follow_boundary() {
        let graph = single_zone_graph(
            0,
            vec![retail_path(1, 1, Some((0, 1, 0))), retail_path(5, 3, None)],
        );
        let mut camera = RetailCameraRuntime::new(&graph).unwrap();
        let crossing = camera.update(&graph, RetailCameraInput::default()).unwrap();

        assert_eq!(crossing.after.path.index, 1);
        assert_eq!(crossing.after.progress.raw(), 2 * PATH_POINT_STEP);
        assert_eq!(
            crossing.outcome,
            RetailCameraOutcome::AutoAdvanced {
                skipped: false,
                path_crossings: 1,
            }
        );
        assert_eq!(
            crossing.effects,
            [
                RetailCameraEffect::LevelUpdate {
                    before: crossing.before,
                    after: crossing.after,
                    flags: 0,
                },
                RetailCameraEffect::SaveStateHandshake {
                    location: crossing.after,
                },
            ]
        );
        assert_eq!(crossing.game_state, GAME_STATE_CUTSCENE);

        let follow = camera.update(&graph, RetailCameraInput::default()).unwrap();
        assert_eq!(
            follow.outcome,
            RetailCameraOutcome::FollowBoundary { mode: 5 }
        );
        assert_eq!(follow.after, crossing.after);
        assert_eq!(follow.game_state, GAME_STATE_PLAYING);
    }

    #[test]
    fn retail_mode_six_is_an_explicit_follow_boundary() {
        let graph = single_zone_graph(0, vec![retail_path(6, 2, None)]);
        let mut camera = RetailCameraRuntime::new(&graph).unwrap();
        let step = camera.update(&graph, RetailCameraInput::default()).unwrap();
        assert_eq!(
            step.outcome,
            RetailCameraOutcome::FollowBoundary { mode: 6 }
        );
        assert_eq!(step.game_state, GAME_STATE_PLAYING);
        assert_eq!(step.before, step.after);
    }

    #[test]
    fn retail_follow_offsets_reproduce_button_latches_and_level_three_limit() {
        let path = follow_path(&[[0, 0, 0]], [4_096, 0, 0], 10, None);
        let mut state = RetailCameraFollowState::default();
        update_follow_offsets(
            &mut state,
            0,
            &path,
            RetailCameraFollowInput {
                held_buttons: PAD_DOWN | PAD_RIGHT,
                ..RetailCameraFollowInput::default()
            },
        );
        assert!(state.offset_dir_z);
        assert!(state.offset_dir_x);
        assert_eq!(state.offset_z, -0xfa00);
        assert_eq!(state.offset_x, 0);

        update_follow_offsets(
            &mut state,
            ZONE_SIDE_SCROLL,
            &path,
            RetailCameraFollowInput {
                held_buttons: PAD_DOWN | PAD_RIGHT,
                ..RetailCameraFollowInput::default()
            },
        );
        assert_eq!(state.offset_z, -0xc800);
        assert_eq!(state.offset_x, 25_600);

        update_follow_offsets(
            &mut state,
            ZONE_SIDE_SCROLL,
            &path,
            RetailCameraFollowInput {
                held_buttons: PAD_UP | PAD_LEFT,
                ..RetailCameraFollowInput::default()
            },
        );
        assert_eq!(state.offset_z, -0xfa00);
        assert_eq!(state.offset_x, 0);

        for _ in 0..64 {
            update_follow_offsets(
                &mut state,
                0,
                &path,
                RetailCameraFollowInput {
                    held_buttons: PAD_DOWN,
                    level_id: 3,
                    ..RetailCameraFollowInput::default()
                },
            );
        }
        assert_eq!(state.offset_z, 0x4b000);
    }

    #[test]
    fn retail_linear_follow_projects_every_whole_path_point() {
        let points: Vec<[i16; 3]> = (0..=10).map(|index| [index * 10, 0, 0]).collect();
        let graph = single_zone_graph(0, vec![follow_path(&points, [4_096, 0, 0], 10, None)]);
        let mut camera = RetailCameraRuntime::new(&graph).unwrap();

        for index in 0..=10_i32 {
            let step = camera
                .update_follow(
                    &graph,
                    RetailCameraFollowInput {
                        player_translation: Vec3 {
                            x: (index * 10) << 8,
                            y: -DEFAULT_OFFSET_Y,
                            z: -(DEFAULT_OFFSET_Z + DEFAULT_ZOOM),
                        },
                        ..RetailCameraFollowInput::default()
                    },
                )
                .unwrap();
            assert_eq!(step.after.progress.raw(), index << 8);
            assert_eq!(step.after.path, graph.spawn_path());
            assert_eq!(step.game_state, GAME_STATE_PLAYING);
            assert!(matches!(
                step.outcome,
                RetailCameraOutcome::FollowEvaluated {
                    mode: 5,
                    candidate_count: 1,
                    crossed_path: false,
                    ..
                }
            ));
        }
    }

    #[test]
    fn retail_near_plane_follow_projects_rotated_z_path() {
        let graph = single_zone_graph(
            0,
            vec![follow_path(
                &[[0, 0, 41], [0, 0, 0], [0, 0, -41]],
                [0, 0, -4_096],
                40,
                None,
            )],
        );
        let mut camera = RetailCameraRuntime::new(&graph).unwrap();
        let step = camera
            .update_follow(
                &graph,
                RetailCameraFollowInput {
                    player_translation: Vec3 {
                        x: 449 << 8,
                        y: 0,
                        z: -1_401 << 8,
                    },
                    ..RetailCameraFollowInput::default()
                },
            )
            .unwrap();

        assert_eq!(step.after.progress.raw(), 0x100);
        assert_eq!(camera.follow_state().speed, 0x100);
        assert_eq!(
            step.outcome,
            RetailCameraOutcome::FollowEvaluated {
                mode: 5,
                candidate_count: 1,
                moved: true,
                crossed_path: false,
            }
        );
    }

    #[test]
    fn retail_follow_smoothing_crosses_to_neighbor_with_source_off_by_one() {
        let graph = single_zone_graph(
            0,
            vec![
                follow_path(
                    &[[0, 0, 41], [0, 0, 0]],
                    [0, 0, -4_096],
                    40,
                    Some((2, 0, 1, 1)),
                ),
                follow_path(&[[0, 0, -41], [0, 0, -82]], [0, 0, -4_096], 40, None),
            ],
        );
        let mut camera =
            RetailCameraRuntime::at_path(&graph, graph.spawn_path(), 0x100, GAME_STATE_PLAYING)
                .unwrap();
        let step = camera
            .update_follow(
                &graph,
                RetailCameraFollowInput {
                    player_translation: Vec3 {
                        x: 449 << 8,
                        y: 0,
                        z: -1_483 << 8,
                    },
                    ..RetailCameraFollowInput::default()
                },
            )
            .unwrap();

        assert_eq!(step.after.path.index, 1);
        assert_eq!(step.after.progress.raw(), 0xff);
        assert_eq!(camera.follow_state().speed, 0x200);
        assert_eq!(
            step.outcome,
            RetailCameraOutcome::FollowEvaluated {
                mode: 5,
                candidate_count: 2,
                moved: true,
                crossed_path: true,
            }
        );
        assert_eq!(
            step.effects,
            [RetailCameraEffect::LevelUpdate {
                before: step.before,
                after: step.after,
                flags: 0,
            }]
        );
    }

    #[test]
    fn retail_follow_gem_stamp_opens_timed_neighbor_for_fifteen_frames() {
        let graph = single_zone_graph(
            0,
            vec![
                follow_path(
                    &[[0, 0, 41], [0, 0, 0]],
                    [0, 0, -4_096],
                    40,
                    // Goal bit four is the source's gem-timed camera gate;
                    // bit one keeps the neighbor's entrance at its start.
                    Some((2, 0, 1, 5)),
                ),
                follow_path(&[[0, 0, -41], [0, 0, -82]], [0, 0, -4_096], 40, None),
            ],
        );
        let input = |gem_stamp| RetailCameraFollowInput {
            player_translation: Vec3 {
                x: 449 << 8,
                y: 0,
                z: -1_483 << 8,
            },
            frames_elapsed: 100,
            gem_stamp,
            ..RetailCameraFollowInput::default()
        };

        let mut expired =
            RetailCameraRuntime::at_path(&graph, graph.spawn_path(), 0x100, GAME_STATE_PLAYING)
                .unwrap();
        let expired_step = expired.update_follow(&graph, input(84)).unwrap();
        assert_eq!(expired_step.after.path, graph.spawn_path());
        assert!(matches!(
            expired_step.outcome,
            RetailCameraOutcome::FollowEvaluated {
                candidate_count: 1,
                crossed_path: false,
                ..
            }
        ));

        let mut active =
            RetailCameraRuntime::at_path(&graph, graph.spawn_path(), 0x100, GAME_STATE_PLAYING)
                .unwrap();
        let active_step = active.update_follow(&graph, input(85)).unwrap();
        assert_eq!(active_step.after.path.index, 1);
        assert!(matches!(
            active_step.outcome,
            RetailCameraOutcome::FollowEvaluated {
                candidate_count: 2,
                crossed_path: true,
                ..
            }
        ));
    }

    #[test]
    fn retail_follow_failure_is_typed_and_transactional() {
        let graph = single_zone_graph(
            0,
            vec![follow_path(
                &[[0, 0, 0], [10, 0, 0]],
                [4_096, 0, 0],
                0,
                None,
            )],
        );
        let mut camera = RetailCameraRuntime::new(&graph).unwrap();
        let before = camera;
        let error = camera
            .update_follow(&graph, RetailCameraFollowInput::default())
            .unwrap_err();
        assert_eq!(
            error,
            RetailCameraError::InvalidAverageNodeDistance {
                path: graph.spawn_path(),
                distance: 0,
            }
        );
        assert_eq!(camera, before);
    }

    #[test]
    fn retail_follow_requires_mode_five_or_six_transactionally() {
        let graph = single_zone_graph(0, vec![retail_path(1, 2, None)]);
        let mut camera = RetailCameraRuntime::new(&graph).unwrap();
        let before = camera;
        assert_eq!(
            camera
                .update_follow(&graph, RetailCameraFollowInput::default())
                .unwrap_err(),
            RetailCameraError::FollowModeRequired {
                path: graph.spawn_path(),
                mode: 1,
            }
        );
        assert_eq!(camera, before);
    }

    #[test]
    fn retail_skip_crosses_modes_one_and_three_in_one_tick() {
        let graph = single_zone_graph(
            0,
            vec![
                retail_path(1, 4, Some((0, 1, 1))),
                retail_path(3, 2, Some((0, 2, 1))),
                retail_path(5, 3, None),
            ],
        );
        let mut camera = RetailCameraRuntime::new(&graph).unwrap();
        let step = camera
            .update(&graph, RetailCameraInput { tapped: 0xf0 })
            .unwrap();

        assert_eq!(step.after.path.index, 2);
        assert_eq!(step.after.progress, PathProgress::ZERO);
        assert_eq!(
            step.outcome,
            RetailCameraOutcome::AutoAdvanced {
                skipped: true,
                path_crossings: 2,
            }
        );
        let middle = RetailCameraLocation {
            path: RetailPathId {
                zone: step.before.path.zone,
                index: 1,
            },
            progress: PathProgress::ZERO,
        };
        assert_eq!(
            step.effects,
            [
                RetailCameraEffect::LevelUpdate {
                    before: step.before,
                    after: middle,
                    flags: 0,
                },
                RetailCameraEffect::SaveStateHandshake { location: middle },
                RetailCameraEffect::LevelUpdate {
                    before: middle,
                    after: step.after,
                    flags: 0,
                },
                RetailCameraEffect::SaveStateHandshake {
                    location: step.after,
                },
            ]
        );
        assert_eq!(step.game_state, GAME_STATE_CUTSCENE);
    }

    #[test]
    fn retail_graphics_flags_disable_skip() {
        let graph = single_zone_graph(0x8_0000, vec![retail_path(1, 3, None)]);
        let mut camera = RetailCameraRuntime::new(&graph).unwrap();
        let step = camera
            .update(&graph, RetailCameraInput { tapped: 0xf0 })
            .unwrap();

        assert_eq!(step.after.progress.raw(), PATH_POINT_STEP);
        assert_eq!(
            step.outcome,
            RetailCameraOutcome::AutoAdvanced {
                skipped: false,
                path_crossings: 0,
            }
        );
    }

    #[test]
    fn retail_save_handshake_uses_destination_zone_flags() {
        let source_zone = Eid::from_raw(1);
        let target_zone = Eid::from_raw(3);
        let graph = RetailZoneGraph::new(
            RetailPathId {
                zone: source_zone,
                index: 0,
            },
            [
                RetailZoneNode::new(
                    source_zone,
                    [0; 3],
                    0,
                    0,
                    vec![target_zone],
                    vec![retail_path(1, 1, Some((0, 0, 1)))],
                ),
                RetailZoneNode::new(
                    target_zone,
                    [0; 3],
                    SAVE_STATE_DISABLE_FLAG,
                    0,
                    vec![],
                    vec![retail_path(5, 1, None)],
                ),
            ],
        )
        .unwrap();
        let mut camera = RetailCameraRuntime::new(&graph).unwrap();
        let step = camera.update(&graph, RetailCameraInput::default()).unwrap();
        assert_eq!(step.after.path.zone, target_zone);
        assert_eq!(
            step.effects,
            [RetailCameraEffect::LevelUpdate {
                before: step.before,
                after: step.after,
                flags: 0,
            }]
        );
    }

    #[test]
    fn retail_skip_cycle_is_typed_and_transactional() {
        let graph = single_zone_graph(0, vec![retail_path(1, 1, Some((0, 0, 1)))]);
        let mut camera = RetailCameraRuntime::new(&graph).unwrap();
        let before = camera;
        let error = camera
            .update(&graph, RetailCameraInput { tapped: 0xf0 })
            .unwrap_err();

        assert_eq!(
            error,
            RetailCameraError::AutoSkipCycle {
                path: graph.spawn_path(),
            }
        );
        assert_eq!(camera, before);
    }

    #[test]
    fn retail_terminal_automatic_path_holds_without_a_neighbor() {
        for tapped in [0, AUTO_SKIP_BUTTON_MASK] {
            let graph = single_zone_graph(0, vec![retail_path(1, 1, None)]);
            let mut camera = RetailCameraRuntime::new(&graph).unwrap();
            let before = camera;
            let step = camera.update(&graph, RetailCameraInput { tapped }).unwrap();

            assert_eq!(step.before, before.location());
            assert_eq!(step.after, before.location());
            assert_eq!(camera.location(), before.location());
            assert_eq!(step.game_state, GAME_STATE_CUTSCENE);
            assert_eq!(camera.game_state(), GAME_STATE_CUTSCENE);
            assert_eq!(
                step.outcome,
                RetailCameraOutcome::AutoAdvanced {
                    skipped: tapped != 0,
                    path_crossings: 0,
                }
            );
            assert!(step.effects.is_empty());
        }
    }
}
