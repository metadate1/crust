//! Safe, pointer-free reconstruction of the retail solid-motion pass.
//!
//! The original runtime overlays flattened octree records with native pointers
//! and keeps wall-query scratch buffers in global storage.  This module keeps
//! the serialized child links as validated little-endian byte offsets and makes
//! every piece of mutable scratch state caller-owned.

use crate::math::{Bounds3, Vec3};

const ZDAT_RECT_BYTES: usize = 36;
const MAX_QUERY_UNITS: usize = 512;
const MAX_OCTREE_LEVELS: u16 = 64;
const MAX_PULL_STEPS: usize = 16_384;

/// Retail's sentinel for an absent floor or ceiling.
pub const NO_SURFACE_Y: i32 = -999_999_999;
/// Normal landing probe height in 24.8 world units.
pub const STANDARD_LAND_OFFSET: i32 = 62_500;
/// Landing probe height used by the two hog levels.
pub const HOG_LAND_OFFSET: i32 = 162_500;
/// Maximum X/Z displacement handled by one retail collision iteration.
pub const MAX_HORIZONTAL_DISPLACEMENT: i32 = 25_600;
/// Maximum Y displacement handled by one retail collision iteration.
pub const MAX_VERTICAL_DISPLACEMENT: i32 = 153_600;

pub const STATUS_GROUNDLAND: u32 = 0x0000_0001;
pub const STATUS_HIT_CEILING: u32 = 0x0000_0080;
pub const STATUS_CLEAR_OF_WALL: u32 = 0x0000_0100;
pub const STATUS_SURFACE_EVENT: u32 = 0x0000_0400;
pub const STATUS_HOTSPOT_COLLISION: u32 = 0x0000_1000;
pub const STATUS_WADING_EVENT: u32 = 0x0000_2000;
pub const STATUS_BOUNCED_FROM_SOLID: u32 = 0x0010_0000;

pub const SOLID_SIDE: u32 = 0x0001_0000;
pub const SOLID_TOP: u32 = 0x0002_0000;
pub const BOX_OBJECT: u32 = 0x0040_0000;
pub const SOLID_BOTTOM: u32 = 0x0800_0000;

pub const EVENT_FALL_KILL: u32 = 0x0900;
pub const EVENT_EXPLODE: u32 = 0x1e00;
pub const EVENT_BURN: u32 = 0x1f00;
pub const EVENT_DROWN: u32 = 0x2100;
pub const EVENT_SHOCK: u32 = 0x2300;

const TEST_BOUND_EVENT: Bounds3 = Bounds3 {
    min: Vec3 {
        x: -38_400,
        y: 0,
        z: -38_400,
    },
    max: Vec3 {
        x: 38_400,
        y: 170_240,
        z: 38_400,
    },
};
const TEST_BOUND_SURFACE: Bounds3 = Bounds3 {
    min: Vec3 {
        x: -1_600,
        y: 0,
        z: -1_600,
    },
    max: Vec3 {
        x: 1_600,
        y: 170_240,
        z: 1_600,
    },
};
const TEST_BOUND_OBJECT: Bounds3 = Bounds3 {
    min: Vec3 {
        x: -19_200,
        y: 0,
        z: -19_200,
    },
    max: Vec3 {
        x: 19_200,
        y: 170_240,
        z: 19_200,
    },
};
const TEST_BOUND_OBJECT_TOP: Bounds3 = Bounds3 {
    min: Vec3 {
        x: -19_200,
        y: 127_680,
        z: -19_200,
    },
    max: Vec3 {
        x: 19_200,
        y: 170_240,
        z: 19_200,
    },
};
const TEST_BOUND_CEILING: Bounds3 = Bounds3 {
    min: Vec3 {
        x: -9_600,
        y: 127_680,
        z: -9_600,
    },
    max: Vec3 {
        x: 9_600,
        y: 170_240,
        z: 9_600,
    },
};

/// One borrowed ZDAT rectangle and its serialized octree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SolidZoneView<'a> {
    pub origin: [i32; 3],
    pub dimensions: [u32; 3],
    pub root: u16,
    pub max_depth: [u16; 3],
    pub bytes: &'a [u8],
    /// Runtime zone graphics flags used by top/bottom/water constraints.
    pub graphics_flags: u32,
    /// Runtime water height in 24.8 world units.
    pub water_y: i32,
}

impl<'a> SolidZoneView<'a> {
    /// Builds and shallow-validates a borrowed ZDAT octree view.
    pub fn new(
        origin: [i32; 3],
        dimensions: [u32; 3],
        root: u16,
        max_depth: [u16; 3],
        bytes: &'a [u8],
    ) -> Result<Self, SolidMotionError> {
        for (axis, depth) in max_depth.into_iter().enumerate() {
            if depth > 31 {
                return Err(SolidMotionError::InvalidDepth { axis, depth });
            }
        }
        let view = Self {
            origin,
            dimensions,
            root,
            max_depth,
            bytes,
            graphics_flags: 0,
            water_y: i32::MIN,
        };
        view.scaled_rect()?;
        if root != 0 && root & 1 == 0 {
            view.child_table(root, 0)?;
        }
        Ok(view)
    }

    /// Adds runtime metadata that is not stored in ZDAT item one.
    #[must_use]
    pub const fn with_graphics(mut self, graphics_flags: u32, water_y: i32) -> Self {
        self.graphics_flags = graphics_flags;
        self.water_y = water_y;
        self
    }

    fn child_table(self, offset: u16, level: u16) -> Result<&'a [u8], SolidMotionError> {
        let offset = usize::from(offset);
        if offset < ZDAT_RECT_BYTES {
            return Err(SolidMotionError::MalformedOctreeOffset { offset });
        }
        let active_axes = self
            .max_depth
            .into_iter()
            .filter(|depth| level < *depth)
            .count();
        let byte_len = (1_usize << active_axes)
            .checked_mul(2)
            .ok_or(SolidMotionError::ArithmeticOverflow)?;
        self.bytes
            .get(offset..offset.saturating_add(byte_len))
            .ok_or(SolidMotionError::MalformedOctreeOffset { offset })
    }

    fn scaled_rect(self) -> Result<ScaledRect, SolidMotionError> {
        let mut origin = [0_i32; 3];
        let mut dimensions = [0_i32; 3];
        for axis in 0..3 {
            origin[axis] = self.origin[axis]
                .checked_mul(0x100)
                .ok_or(SolidMotionError::ArithmeticOverflow)?;
            dimensions[axis] = i32::try_from(self.dimensions[axis])
                .ok()
                .and_then(|value| value.checked_mul(0x100))
                .ok_or(SolidMotionError::InvalidDimensions { axis })?;
            origin[axis]
                .checked_add(dimensions[axis])
                .ok_or(SolidMotionError::ArithmeticOverflow)?;
        }
        Ok(ScaledRect {
            origin: Vec3 {
                x: origin[0],
                y: origin[1],
                z: origin[2],
            },
            dimensions: Vec3 {
                x: dimensions[0],
                y: dimensions[1],
                z: dimensions[2],
            },
        })
    }
}

/// Runtime state of the object being moved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SolidMotionState {
    pub translation: Vec3,
    /// Unscaled process velocity; floor and ceiling hits update its Y component.
    pub velocity: Vec3,
    /// Object-local runtime bound used for object and zone interactions.
    pub local_bound: Bounds3,
    pub status_a: u32,
    pub status_b: u32,
    pub status_c: u32,
    pub state_flags: u32,
    pub invincibility_state: i32,
    pub animation_stamp: i32,
    pub floor_impact_stamp: i32,
    pub floor_impact_velocity: i32,
    pub event: u32,
    pub hotspot_size: i32,
    /// Stable ID of the currently registered collider, if any.
    pub collider: Option<u32>,
}

impl Default for SolidMotionState {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            velocity: Vec3::ZERO,
            local_bound: Bounds3::default(),
            status_a: 0,
            status_b: 0,
            status_c: 0,
            state_flags: 0,
            invincibility_state: 0,
            animation_stamp: 0,
            floor_impact_stamp: 0,
            floor_impact_velocity: 0,
            event: 0,
            hotspot_size: 0,
            collider: None,
        }
    }
}

/// One frame-bound object collision candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SolidObjectCandidate {
    pub id: u32,
    pub translation: Vec3,
    pub bounds: Bounds3,
    pub status_b: u32,
    pub status_c: u32,
    pub state_flags: u32,
    pub category: u32,
    pub object_type: u32,
    pub hotspot_size: i32,
}

/// Level-dependent collision behavior kept explicit instead of consulting globals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SolidLevelQuirks {
    pub land_offset: i32,
    /// Cortex Power and Toxic Waste turn type-four pits into drowning surfaces.
    pub type_four_pits_drown: bool,
    /// Ripper Roo sends a zero-argument drown event before its outside-zone rule.
    pub drown_when_below_zone: bool,
    /// Upstream and Up The Creek apply the water-height death check.
    pub lethal_river_water: bool,
}

impl Default for SolidLevelQuirks {
    fn default() -> Self {
        Self {
            land_offset: STANDARD_LAND_OFFSET,
            type_four_pits_drown: false,
            drown_when_below_zone: false,
            lethal_river_water: false,
        }
    }
}

/// Caller-owned context for one retail solid-motion pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SolidMotionContext {
    pub frame_stamp: i32,
    /// Index in the ordered neighbor slice corresponding to the object's zone.
    pub object_zone: Option<usize>,
    /// Graphics flags of global `cur_zone`, which controls wall plotting.
    pub current_world_graphics_flags: u32,
    pub quirks: SolidLevelQuirks,
}

/// Persistent scratch used by retail's smooth-stop heuristic.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SmoothStopMemory {
    pub being_stopped: bool,
    pub previous_displacement: Vec3,
}

/// A typed target for a collision-generated GOOL event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SolidEventTarget {
    MovingObject,
    Candidate(u32),
}

/// Why a GOOL event was requested.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SolidEventReason {
    Surface,
    ObjectHitFromBelow,
    OutsideZone,
    Water,
}

/// Side effects which the pure solver cannot apply to the object arena itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SolidEffect {
    NodeContact {
        zone_index: usize,
        raw_node: u16,
        event: Option<u32>,
        status_bits: u32,
        excluded_from_floor: bool,
    },
    ObjectCollision {
        candidate: u32,
        accepted: bool,
    },
    SetCandidateCollider {
        candidate: u32,
    },
    SetCandidateStatus {
        candidate: u32,
        status_bits: u32,
    },
    SendEvent {
        target: SolidEventTarget,
        event: u32,
        argument: u32,
        reason: SolidEventReason,
    },
    ZoneChanged {
        previous: Option<usize>,
        current: usize,
    },
    MissingZone,
}

/// Surface heights retained by the C query scratch structure.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SolidQuerySummary {
    pub ceiling: Option<i32>,
    pub solid_nodes_y: Option<i32>,
    pub floor_nodes_y: Option<i32>,
    pub solid_objects_y: Option<i32>,
}

/// Result of a complete subdivided solid-motion pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SolidMotionOutcome {
    pub state: SolidMotionState,
    pub object_zone: Option<usize>,
    pub smooth_stop: SmoothStopMemory,
    pub summary: SolidQuerySummary,
    pub floor: Option<i32>,
    pub effects: Vec<SolidEffect>,
    pub movement_iterations: usize,
    pub stopped_by_wall: bool,
}

/// Bounds/offset validation failures from a solid query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SolidMotionError {
    ArithmeticOverflow,
    InvalidDimensions { axis: usize },
    InvalidDepth { axis: usize, depth: u16 },
    MalformedOctreeOffset { offset: usize },
    OctreeDepthExceeded,
    QueryCapacityExceeded,
    MovementIterationLimit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScaledRect {
    origin: Vec3,
    dimensions: Vec3,
}

impl ScaledRect {
    fn end(self) -> Result<Vec3, SolidMotionError> {
        checked_vec_add(self.origin, self.dimensions)
    }

    fn contains(self, point: Vec3) -> Result<bool, SolidMotionError> {
        let end = self.end()?;
        Ok(point.x >= self.origin.x
            && point.y >= self.origin.y
            && point.z >= self.origin.z
            && point.x <= end.x
            && point.y <= end.y
            && point.z <= end.z)
    }
}

/// One safely decoded leaf in a flattened neighborhood query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SolidQueryNode {
    pub zone_index: usize,
    pub raw_node: u16,
    pub level: u16,
    /// Origin relative to `SolidQuery::nodes_bound`, rounded like retail.
    pub relative_origin: Vec3,
    pub zone_dimensions: Vec3,
    pub max_depth: [u16; 3],
}

impl SolidQueryNode {
    #[must_use]
    pub const fn kind(self) -> u8 {
        ((self.raw_node & 0x000e) >> 1) as u8
    }

    #[must_use]
    pub const fn subtype(self) -> u8 {
        ((self.raw_node & 0x03f0) >> 4) as u8
    }
}

/// Owned, pointer-free result of querying all intersecting neighbor octrees.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SolidQuery {
    pub nodes_bound: Bounds3,
    nodes: Vec<SolidQueryNode>,
    query_units: usize,
}

impl SolidQuery {
    #[must_use]
    pub fn nodes(&self) -> &[SolidQueryNode] {
        &self.nodes
    }
}

/// Flattens all ordered neighbor octrees intersecting `nodes_bound`.
pub fn query_zone_octrees(
    zones: &[SolidZoneView<'_>],
    nodes_bound: Bounds3,
) -> Result<SolidQuery, SolidMotionError> {
    let mut query = SolidQuery {
        nodes_bound,
        nodes: Vec::new(),
        query_units: 0,
    };
    let query_dimensions = Vec3 {
        x: nodes_bound
            .max
            .x
            .checked_sub(nodes_bound.min.x)
            .ok_or(SolidMotionError::ArithmeticOverflow)?,
        y: nodes_bound
            .max
            .y
            .checked_sub(nodes_bound.min.y)
            .ok_or(SolidMotionError::ArithmeticOverflow)?,
        z: nodes_bound
            .max
            .z
            .checked_sub(nodes_bound.min.z)
            .ok_or(SolidMotionError::ArithmeticOverflow)?,
    };
    for (zone_index, zone) in zones.iter().copied().enumerate() {
        let rect = zone.scaled_rect()?;
        if !rect_intersects_bound(rect, nodes_bound)? {
            continue;
        }
        query.query_units = query
            .query_units
            .checked_add(2)
            .ok_or(SolidMotionError::ArithmeticOverflow)?;
        if query.query_units > MAX_QUERY_UNITS {
            return Err(SolidMotionError::QueryCapacityExceeded);
        }
        let relative_origin = checked_vec_sub(rect.origin, nodes_bound.min)?;
        query_octree_recursive(
            zone,
            zone_index,
            zone.root,
            relative_origin,
            rect.dimensions,
            query_dimensions,
            0,
            &mut query,
        )?;
    }
    Ok(query)
}

#[allow(clippy::too_many_arguments)]
fn query_octree_recursive(
    zone: SolidZoneView<'_>,
    zone_index: usize,
    node: u16,
    origin: Vec3,
    dimensions: Vec3,
    query_dimensions: Vec3,
    level: u16,
    query: &mut SolidQuery,
) -> Result<(), SolidMotionError> {
    if node == 0 {
        return Ok(());
    }
    if node & 1 != 0 {
        query.query_units = query
            .query_units
            .checked_add(1)
            .ok_or(SolidMotionError::ArithmeticOverflow)?;
        if query.query_units > MAX_QUERY_UNITS {
            return Err(SolidMotionError::QueryCapacityExceeded);
        }
        query.nodes.push(SolidQueryNode {
            zone_index,
            raw_node: node,
            level,
            relative_origin: Vec3 {
                x: (origin.x >> 4) * 16,
                y: (origin.y >> 4) * 16,
                z: (origin.z >> 4) * 16,
            },
            zone_dimensions: zone.scaled_rect()?.dimensions,
            max_depth: zone.max_depth,
        });
        return Ok(());
    }
    if level >= MAX_OCTREE_LEVELS {
        return Err(SolidMotionError::OctreeDepthExceeded);
    }
    let children = zone.child_table(node, level)?;
    let active = [
        level < zone.max_depth[0],
        level < zone.max_depth[1],
        level < zone.max_depth[2],
    ];
    let child_dimensions = Vec3 {
        x: if active[0] {
            dimensions.x >> 1
        } else {
            dimensions.x
        },
        y: if active[1] {
            dimensions.y >> 1
        } else {
            dimensions.y
        },
        z: if active[2] {
            dimensions.z >> 1
        } else {
            dimensions.z
        },
    };
    let midpoint = checked_vec_add(origin, child_dimensions)?;
    let axis_flags = [
        if active[0] {
            u8::from(midpoint.x >= 0) | (u8::from(midpoint.x <= query_dimensions.x) << 1)
        } else {
            3
        },
        if active[1] {
            u8::from(midpoint.y >= 0) | (u8::from(midpoint.y <= query_dimensions.y) << 1)
        } else {
            3
        },
        if active[2] {
            u8::from(midpoint.z >= 0) | (u8::from(midpoint.z <= query_dimensions.z) << 1)
        } else {
            3
        },
    ];
    let counts = [
        if active[0] { 2 } else { 1 },
        if active[1] { 2 } else { 1 },
        if active[2] { 2 } else { 1 },
    ];
    let mut child_index = 0_usize;
    for x in 0..counts[0] {
        for y in 0..counts[1] {
            for z in 0..counts[2] {
                let mask_x = (x + usize::from(active[0])) as u8;
                let mask_y = (y + usize::from(active[1])) as u8;
                let mask_z = (z + usize::from(active[2])) as u8;
                let selected = axis_flags[0] & mask_x == mask_x
                    && axis_flags[1] & mask_y == mask_y
                    && axis_flags[2] & mask_z == mask_z;
                if selected {
                    let byte_offset = child_index
                        .checked_mul(2)
                        .ok_or(SolidMotionError::ArithmeticOverflow)?;
                    let child_bytes = children.get(byte_offset..byte_offset + 2).ok_or(
                        SolidMotionError::MalformedOctreeOffset {
                            offset: usize::from(node).saturating_add(byte_offset),
                        },
                    )?;
                    let child = u16::from_le_bytes([child_bytes[0], child_bytes[1]]);
                    let child_origin = Vec3 {
                        x: if x == 0 { origin.x } else { midpoint.x },
                        y: if y == 0 { origin.y } else { midpoint.y },
                        z: if z == 0 { origin.z } else { midpoint.z },
                    };
                    query_octree_recursive(
                        zone,
                        zone_index,
                        child,
                        child_origin,
                        child_dimensions,
                        query_dimensions,
                        level + 1,
                        query,
                    )?;
                }
                child_index += 1;
            }
        }
    }
    Ok(())
}

/// Exact fixed-point displacement used before the solid-motion pass.
pub fn scale_velocity_for_tick(
    velocity: Vec3,
    ticks_per_frame: i32,
) -> Result<Vec3, SolidMotionError> {
    let scale = ticks_per_frame.min(0x66);
    Ok(Vec3 {
        x: checked_mul_div(velocity.x, scale, 1024)?,
        y: checked_mul_div(velocity.y, scale, 1024)?,
        z: checked_mul_div(velocity.z, scale, 1024)?,
    })
}

/// Applies retail gravity after translation and clamps terminal fall speed.
pub fn apply_retail_gravity(
    velocity: Vec3,
    ticks_per_frame: i32,
) -> Result<Vec3, SolidMotionError> {
    let scale = ticks_per_frame.min(0x66);
    let gravity = 4_000_i32
        .checked_mul(scale)
        .ok_or(SolidMotionError::ArithmeticOverflow)?;
    let mut output = velocity;
    output.y = output
        .y
        .checked_sub(gravity)
        .ok_or(SolidMotionError::ArithmeticOverflow)?
        .max(-0x2e_e000);
    Ok(output)
}

/// Runs retail's smooth, subdivided static/object collision solver.
#[allow(clippy::too_many_arguments)]
pub fn solve_retail_solid_motion(
    zones: &[SolidZoneView<'_>],
    candidates: &[SolidObjectCandidate],
    state: SolidMotionState,
    displacement: Vec3,
    context: SolidMotionContext,
    smooth_stop: SmoothStopMemory,
) -> Result<SolidMotionOutcome, SolidMotionError> {
    let mut state = state;
    let mut effective_displacement = displacement;
    if smooth_stop.being_stopped {
        let slope_acceleration =
            checked_vec_sub(smooth_stop.previous_displacement, effective_displacement)?;
        if slope_acceleration.x.unsigned_abs() < 10
            && slope_acceleration.y.unsigned_abs() < 10
            && slope_acceleration.z.unsigned_abs() < 10
        {
            effective_displacement.x = 0;
            effective_displacement.z = 0;
        }
    }

    let original_translation = state.translation;
    let mut translation = state.translation;
    let mut remaining = effective_displacement;
    let maximum = maximum_step(remaining);
    let mut query = None;
    let mut effects = Vec::new();
    let mut summary = SolidQuerySummary::default();
    let mut floor = None;
    let mut object_zone = context.object_zone;
    let mut iterations = 0_usize;
    let mut stopped_by_wall = false;

    while remaining != Vec3::ZERO {
        if iterations == MAX_PULL_STEPS {
            return Err(SolidMotionError::MovementIterationLimit);
        }
        let step = Vec3 {
            x: clamp_remaining_step(remaining.x, maximum.x),
            y: clamp_remaining_step(remaining.y, maximum.y),
            z: clamp_remaining_step(remaining.z, maximum.z),
        };
        if step == Vec3::ZERO {
            return Err(SolidMotionError::MovementIterationLimit);
        }
        let step_outcome = stop_at_solid(
            zones,
            candidates,
            &mut state,
            translation,
            step,
            &mut query,
            &mut object_zone,
            context,
            &mut effects,
        )?;
        translation = step_outcome.translation;
        summary = step_outcome.summary;
        floor = step_outcome.floor;
        stopped_by_wall |= step_outcome.stopped_by_wall;
        remaining = checked_vec_sub(remaining, step)?;
        iterations += 1;
    }
    state.translation = translation;

    let delta = checked_vec_sub(translation, original_translation)?;
    let newly_stopped = !smooth_stop.being_stopped
        && (displacement.x != 0 || displacement.z != 0)
        && delta.x.unsigned_abs() < 2
        && delta.y.unsigned_abs() < 2
        && delta.z.unsigned_abs() < 2;
    let next_smooth = if newly_stopped {
        SmoothStopMemory {
            being_stopped: true,
            previous_displacement: displacement,
        }
    } else {
        SmoothStopMemory::default()
    };

    if state.status_a & STATUS_SURFACE_EVENT != 0
        && (state.status_a & STATUS_GROUNDLAND == 0 || state.event != EVENT_FALL_KILL)
    {
        effects.push(SolidEffect::SendEvent {
            target: SolidEventTarget::MovingObject,
            event: state.event,
            argument: 0x6400,
            reason: SolidEventReason::Surface,
        });
    }

    Ok(SolidMotionOutcome {
        state,
        object_zone,
        smooth_stop: next_smooth,
        summary,
        floor,
        effects,
        movement_iterations: iterations,
        stopped_by_wall,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StepOutcome {
    translation: Vec3,
    summary: SolidQuerySummary,
    floor: Option<i32>,
    stopped_by_wall: bool,
}

#[allow(clippy::too_many_arguments)]
fn stop_at_solid(
    zones: &[SolidZoneView<'_>],
    candidates: &[SolidObjectCandidate],
    state: &mut SolidMotionState,
    translation: Vec3,
    displacement: Vec3,
    query_cache: &mut Option<SolidQuery>,
    object_zone: &mut Option<usize>,
    context: SolidMotionContext,
    effects: &mut Vec<SolidEffect>,
) -> Result<StepOutcome, SolidMotionError> {
    let mut adjusted = checked_vec_add(translation, displacement)?;
    let floor_result = stop_at_floor(
        zones,
        candidates,
        state,
        translation,
        &mut adjusted,
        query_cache,
        context,
        effects,
    )?;

    let query = query_cache
        .as_ref()
        .ok_or(SolidMotionError::MalformedOctreeOffset { offset: 0 })?;
    let mut bitmap = WallScratch::default();
    plot_walls(
        query,
        candidates,
        state,
        translation,
        context,
        &mut bitmap,
        effects,
        true,
    )?;
    let desired_x = checked_mul_div(checked_sub(adjusted.x, translation.x)?, 4, 8192)?
        .checked_add(16)
        .ok_or(SolidMotionError::ArithmeticOverflow)?;
    let desired_z = checked_mul_div(checked_sub(adjusted.z, translation.z)?, 4, 8192)?
        .checked_add(16)
        .ok_or(SolidMotionError::ArithmeticOverflow)?;
    let collider_type = state.collider.and_then(|id| {
        candidates
            .iter()
            .find(|candidate| candidate.id == id)
            .map(|candidate| candidate.object_type)
    });
    let mut nearest =
        find_nearest_open(&bitmap, desired_x, desired_z, state.collider, collider_type);
    if nearest.is_none() && collider_type != Some(0x22) {
        if solid_replot_walls(query, translation, 0, false, &mut bitmap)? != 0 {
            solid_replot_walls(query, translation, 1, true, &mut bitmap)?;
            plot_object_walls(
                candidates,
                state,
                translation,
                context.current_world_graphics_flags,
                &mut bitmap,
                effects,
                false,
            )?;
        }
        nearest = find_nearest_open(&bitmap, desired_x, desired_z, state.collider, collider_type);
    }
    let (adjusted_x, adjusted_z, found_open) = nearest.unwrap_or((16, 16, false));
    if found_open {
        adjusted.x = translation
            .x
            .checked_add(checked_mul_div(adjusted_x - 16, 8192, 4)?)
            .ok_or(SolidMotionError::ArithmeticOverflow)?;
        adjusted.z = translation
            .z
            .checked_add(checked_mul_div(adjusted_z - 16, 8192, 4)?)
            .ok_or(SolidMotionError::ArithmeticOverflow)?;
    } else {
        adjusted.x = translation.x;
        adjusted.z = translation.z;
        state.status_a |= STATUS_BOUNCED_FROM_SOLID;
    }
    let stopped_by_wall = !found_open || adjusted_x != desired_x || adjusted_z != desired_z;
    if (desired_x != 16 || desired_z != 16) && adjusted_x == desired_x && adjusted_z == desired_z {
        state.status_a |= STATUS_CLEAR_OF_WALL;
    }

    let mut summary = floor_result.summary;
    let ceiling = stop_at_ceiling(zones, candidates, adjusted, query, *object_zone, effects)?;
    summary.ceiling = ceiling;
    if let Some(ceiling) = ceiling {
        let object_top = adjusted
            .y
            .checked_add(170_241)
            .ok_or(SolidMotionError::ArithmeticOverflow)?;
        if ceiling < object_top {
            let limit = ceiling
                .checked_sub(170_241)
                .ok_or(SolidMotionError::ArithmeticOverflow)?;
            if translation.y < limit {
                adjusted.y = limit;
            }
            if state.velocity.y > 0 {
                state.velocity.y = 0;
            }
            state.status_a |= STATUS_HIT_CEILING;
        }
    }
    stop_at_zone(zones, state, &mut adjusted, object_zone, context, effects)?;
    Ok(StepOutcome {
        translation: adjusted,
        summary,
        floor: floor_result.floor,
        stopped_by_wall,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FloorOutcome {
    floor: Option<i32>,
    summary: SolidQuerySummary,
}

#[allow(clippy::too_many_arguments)]
fn stop_at_floor(
    zones: &[SolidZoneView<'_>],
    candidates: &[SolidObjectCandidate],
    state: &mut SolidMotionState,
    translation: Vec3,
    next_translation: &mut Vec3,
    query_cache: &mut Option<SolidQuery>,
    context: SolidMotionContext,
    effects: &mut Vec<SolidEffect>,
) -> Result<FloorOutcome, SolidMotionError> {
    let event_bound = checked_translate_bound(TEST_BOUND_EVENT, *next_translation)?;
    let need_query = query_cache
        .as_ref()
        .is_none_or(|query| !bound_strictly_contains(query.nodes_bound, event_bound));
    if need_query {
        let nodes_bound = query_bound(*next_translation)?;
        *query_cache = Some(query_zone_octrees(zones, nodes_bound)?);
    }
    let query = query_cache
        .as_ref()
        .ok_or(SolidMotionError::MalformedOctreeOffset { offset: 0 })?;
    let max_y = translation
        .y
        .max(next_translation.y)
        .checked_add(context.quirks.land_offset)
        .ok_or(SolidMotionError::ArithmeticOverflow)?;
    let surface_bound = checked_translate_bound(TEST_BOUND_SURFACE, *next_translation)?;
    let node_summary = find_floor_y(query, surface_bound, max_y, context.quirks, state, effects)?;
    let object_floor = highest_object_below(
        candidates,
        state,
        translation,
        *next_translation,
        context.quirks.land_offset,
        effects,
    )?;
    let summary = SolidQuerySummary {
        solid_nodes_y: node_summary.solid_nodes_y,
        floor_nodes_y: node_summary.floor_nodes_y,
        solid_objects_y: object_floor,
        ..SolidQuerySummary::default()
    };
    let mut floor_offset = 0_i32;
    let mut floor_nodes_y = node_summary.floor_nodes_y;
    let solid_nodes_y = node_summary.solid_nodes_y;
    let mut flags = 0x0004_0001_u32;
    if let Some(object_y) = object_floor {
        floor_nodes_y = Some(object_y);
        flags = 0x0020_0001;
        if let Some(collider) = state
            .collider
            .and_then(|id| candidates.iter().find(|candidate| candidate.id == id))
        {
            if collider.status_b & BOX_OBJECT != 0 {
                floor_offset = 0x19_000;
            }
            if state.animation_stamp.wrapping_sub(state.floor_impact_stamp) >= 4 {
                flags |= 0x4000;
            }
        }
    }
    if state.velocity.y > 0 {
        state.status_a &= !0x0024_4001;
    }
    if (floor_nodes_y.is_none() && solid_nodes_y.is_none()) || state.velocity.y > 0 {
        return Ok(FloorOutcome {
            floor: None,
            summary,
        });
    }
    let (mut floor, max_floor) = if let Some(floor_y) = floor_nodes_y {
        (
            floor_y,
            state
                .translation
                .y
                .checked_add(floor_offset)
                .and_then(|value| value.checked_add(context.quirks.land_offset))
                .ok_or(SolidMotionError::ArithmeticOverflow)?,
        )
    } else {
        flags = STATUS_GROUNDLAND;
        (
            solid_nodes_y.expect("one surface height was checked above"),
            state.translation.y,
        )
    };
    floor = floor
        .checked_add(1)
        .ok_or(SolidMotionError::ArithmeticOverflow)?;
    if floor > max_floor {
        floor = state.translation.y;
    }
    next_translation.y = floor;
    let mut moved_surface = surface_bound;
    moved_surface.min.y = floor;
    moved_surface.max.y = floor
        .checked_add(0x2_9900)
        .ok_or(SolidMotionError::ArithmeticOverflow)?;
    if !bound_strictly_contains(query.nodes_bound, moved_surface) {
        let nodes_bound = query_bound(*next_translation)?;
        *query_cache = Some(query_zone_octrees(zones, nodes_bound)?);
    }
    if state.velocity.y < 0 && flags & STATUS_GROUNDLAND != 0 {
        state.floor_impact_velocity = state.velocity.y;
        state.velocity.y = 0;
    }
    state.status_a |= flags;
    state.floor_impact_stamp = context.frame_stamp;
    Ok(FloorOutcome {
        floor: Some(floor),
        summary,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FloorNodeSummary {
    solid_nodes_y: Option<i32>,
    floor_nodes_y: Option<i32>,
}

fn find_floor_y(
    query: &SolidQuery,
    collider: Bounds3,
    max_y: i32,
    quirks: SolidLevelQuirks,
    state: &mut SolidMotionState,
    effects: &mut Vec<SolidEffect>,
) -> Result<FloorNodeSummary, SolidMotionError> {
    let mut sums = [0_i64; 2];
    let mut counts = [0_u32; 2];
    for node in &query.nodes {
        let bounds = query_node_bounds(query, *node)?;
        if !bounds_overlap_for_floor(collider, bounds) {
            continue;
        }
        let kind = node.kind();
        let subtype = node.subtype();
        let mut excluded = false;
        if kind == 3 || kind == 4 || (1..=38).contains(&subtype) {
            let contact = process_node(node.raw_node, quirks);
            if let Some(event) = contact.event {
                state.event = event;
            }
            state.status_a |= contact.status_bits;
            excluded = contact.excluded_from_floor;
            effects.push(SolidEffect::NodeContact {
                zone_index: node.zone_index,
                raw_node: node.raw_node,
                event: contact.event,
                status_bits: contact.status_bits,
                excluded_from_floor: excluded,
            });
        }
        if excluded || bounds.max.y > max_y {
            continue;
        }
        let bucket = usize::from(kind != 0);
        sums[bucket] = sums[bucket]
            .checked_add(i64::from(bounds.max.y))
            .ok_or(SolidMotionError::ArithmeticOverflow)?;
        counts[bucket] += 1;
    }
    Ok(FloorNodeSummary {
        solid_nodes_y: average_height(sums[0], counts[0])?,
        floor_nodes_y: average_height(sums[1], counts[1])?,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NodeContact {
    event: Option<u32>,
    status_bits: u32,
    excluded_from_floor: bool,
}

fn process_node(raw_node: u16, quirks: SolidLevelQuirks) -> NodeContact {
    let node_type = ((raw_node & 0x000e) >> 1) + 1;
    let subtype = (raw_node & 0x03f0) >> 4;
    let mut event = None;
    let mut status_bits = 0;
    let mut excluded_from_floor = false;
    match subtype {
        1 => {
            event = Some(0x0700);
            status_bits |= STATUS_SURFACE_EVENT;
        }
        2 => {
            event = Some(0x0c00);
            status_bits |= STATUS_SURFACE_EVENT;
        }
        3 => {
            event = Some(EVENT_DROWN);
            status_bits |= STATUS_SURFACE_EVENT;
        }
        4 => {
            event = Some(EVENT_BURN);
            status_bits |= STATUS_SURFACE_EVENT;
        }
        5 => {
            event = Some(EVENT_EXPLODE);
            status_bits |= STATUS_SURFACE_EVENT;
        }
        6 => {
            event = Some(0x0d00);
            status_bits |= STATUS_SURFACE_EVENT;
        }
        7..=10 => {
            event = Some(0x1200);
            status_bits |= STATUS_WADING_EVENT;
        }
        11 => {
            return NodeContact {
                event: Some(EVENT_FALL_KILL),
                status_bits: STATUS_SURFACE_EVENT,
                excluded_from_floor: true,
            };
        }
        12 => {
            event = Some(EVENT_SHOCK);
            status_bits |= STATUS_SURFACE_EVENT;
        }
        _ => {}
    }
    if node_type == 4 {
        event = Some(if quirks.type_four_pits_drown {
            EVENT_DROWN
        } else {
            EVENT_FALL_KILL
        });
        status_bits |= STATUS_SURFACE_EVENT;
        excluded_from_floor = !quirks.type_four_pits_drown;
    } else if node_type == 3 || node_type == 5 {
        excluded_from_floor = true;
    }
    NodeContact {
        event,
        status_bits,
        excluded_from_floor,
    }
}

fn highest_object_below(
    candidates: &[SolidObjectCandidate],
    state: &mut SolidMotionState,
    translation: Vec3,
    next_translation: Vec3,
    land_offset: i32,
    effects: &mut Vec<SolidEffect>,
) -> Result<Option<i32>, SolidMotionError> {
    let delta_y = translation
        .y
        .checked_sub(next_translation.y)
        .ok_or(SolidMotionError::ArithmeticOverflow)?;
    let test_y = if delta_y > 0 {
        translation.y
    } else {
        next_translation.y
    }
    .checked_add(land_offset)
    .ok_or(SolidMotionError::ArithmeticOverflow)?;
    let collider_bound = checked_translate_bound(TEST_BOUND_OBJECT, next_translation)?;
    let mut highest = None;
    let mut found = None;
    for candidate in candidates {
        let higher = highest.is_none_or(|height| height < candidate.bounds.max.y);
        if (test_y >= candidate.bounds.max.y || candidate.status_b & BOX_OBJECT != 0)
            && higher
            && source_bound_intersection(collider_bound, candidate.bounds)
        {
            found = Some(*candidate);
            if candidate.status_b & SOLID_TOP != 0
                && ((state.state_flags & 0x10 == 0 && state.invincibility_state != 5)
                    || candidate.category != 0x300
                    || (candidate.status_c & 0x1012 != 0 && candidate.state_flags & 0x10020 == 0))
            {
                highest = Some(
                    candidate
                        .bounds
                        .max
                        .y
                        .checked_add(1)
                        .ok_or(SolidMotionError::ArithmeticOverflow)?,
                );
            }
        }
    }
    if let Some(candidate) = found {
        register_object_collision(state, candidate, candidates, collider_bound, effects)?;
    }
    Ok(highest)
}

fn register_object_collision(
    state: &mut SolidMotionState,
    candidate: SolidObjectCandidate,
    candidates: &[SolidObjectCandidate],
    moving_bound: Bounds3,
    effects: &mut Vec<SolidEffect>,
) -> Result<(), SolidMotionError> {
    let mut accepted = true;
    if let Some(current_id) = state.collider.filter(|current| *current != candidate.id) {
        if candidate.state_flags & 0x800 != 0 {
            effects.push(SolidEffect::ObjectCollision {
                candidate: candidate.id,
                accepted: false,
            });
            effects.push(SolidEffect::SetCandidateCollider {
                candidate: candidate.id,
            });
            return Ok(());
        }
        if let Some(current) = candidates.iter().find(|current| current.id == current_id) {
            let current_distance = approximate_distance(state.translation, current.translation)?;
            let candidate_distance =
                approximate_distance(state.translation, candidate.translation)?;
            if candidate_distance >= current_distance && current.state_flags & 0x800 == 0 {
                accepted = false;
            }
        }
    }
    effects.push(SolidEffect::ObjectCollision {
        candidate: candidate.id,
        accepted,
    });
    if !accepted {
        return Ok(());
    }
    state.collider = Some(candidate.id);
    effects.push(SolidEffect::SetCandidateCollider {
        candidate: candidate.id,
    });
    if state.hotspot_size != 0 {
        let inset = inset_horizontal(moving_bound, state.hotspot_size)?;
        if source_bound_intersection(inset, candidate.bounds) {
            state.status_a |= STATUS_HOTSPOT_COLLISION;
        }
    }
    if candidate.hotspot_size != 0 {
        let inset = inset_horizontal(candidate.bounds, candidate.hotspot_size)?;
        if source_bound_intersection(inset, moving_bound) {
            effects.push(SolidEffect::SetCandidateStatus {
                candidate: candidate.id,
                status_bits: STATUS_HOTSPOT_COLLISION,
            });
        }
    }
    Ok(())
}

fn approximate_distance(left: Vec3, right: Vec3) -> Result<i32, SolidMotionError> {
    let differences = [
        (i64::from(left.x) - i64::from(right.x)).unsigned_abs(),
        (i64::from(left.y) - i64::from(right.y)).unsigned_abs(),
        (i64::from(left.z) - i64::from(right.z)).unsigned_abs(),
    ];
    let mut maximum_axis = 0_usize;
    if differences[1] > differences[maximum_axis] {
        maximum_axis = 1;
    }
    if differences[2] > differences[maximum_axis] {
        maximum_axis = 2;
    }
    let secondary = differences
        .iter()
        .enumerate()
        .filter(|(axis, _)| *axis != maximum_axis)
        .map(|(_, value)| *value)
        .sum::<u64>()
        / 4;
    i32::try_from(differences[maximum_axis].saturating_add(secondary))
        .map_err(|_| SolidMotionError::ArithmeticOverflow)
}

#[allow(clippy::too_many_arguments)]
fn stop_at_ceiling(
    zones: &[SolidZoneView<'_>],
    candidates: &[SolidObjectCandidate],
    next_translation: Vec3,
    query: &SolidQuery,
    object_zone: Option<usize>,
    effects: &mut Vec<SolidEffect>,
) -> Result<Option<i32>, SolidMotionError> {
    let object_probe = checked_translate_bound(TEST_BOUND_OBJECT_TOP, next_translation)?;
    let mut minimum_object_y = None;
    let mut found = None;
    for candidate in candidates {
        if candidate.status_b & SOLID_BOTTOM == 0 {
            continue;
        }
        if source_bound_intersection(object_probe, candidate.bounds) {
            found = Some(*candidate);
            if minimum_object_y.is_none_or(|minimum| candidate.bounds.min.y <= minimum) {
                minimum_object_y = Some(candidate.bounds.min.y);
            }
        }
    }
    let static_probe = checked_translate_bound(TEST_BOUND_CEILING, next_translation)?;
    let mut ceiling = find_ceiling_y(query, static_probe, 2, 1)?;
    if let Some(zone_index) = object_zone
        && let Some(zone) = zones.get(zone_index).copied()
        && zone.graphics_flags & 0x0002_0000 != 0
    {
        let point_above = Vec3 {
            x: next_translation.x,
            y: static_probe.max.y,
            z: next_translation.z,
        };
        if find_containing_zone(zones, point_above)?.is_none() {
            let zone_top = zone.scaled_rect()?.origin.y;
            if zone_top < point_above.y {
                ceiling = Some(zone_top);
            }
        }
    }
    if let Some(object_y) = minimum_object_y
        && ceiling.is_none_or(|static_y| object_y < static_y)
    {
        if let Some(candidate) = found {
            effects.push(SolidEffect::SetCandidateStatus {
                candidate: candidate.id,
                status_bits: STATUS_HIT_CEILING,
            });
            effects.push(SolidEffect::SendEvent {
                target: SolidEventTarget::Candidate(candidate.id),
                event: 0x1700,
                argument: 0x6400,
                reason: SolidEventReason::ObjectHitFromBelow,
            });
        }
        return Ok(Some(object_y));
    }
    Ok(ceiling)
}

/// Averages the lower faces of all overlapping nodes of either requested type.
pub fn find_ceiling_y(
    query: &SolidQuery,
    collider: Bounds3,
    type_a: u8,
    type_b: u8,
) -> Result<Option<i32>, SolidMotionError> {
    let mut sum = 0_i64;
    let mut count = 0_u32;
    for node in &query.nodes {
        let kind = node.kind();
        if kind != type_a.saturating_sub(1) && kind != type_b.saturating_sub(1) {
            continue;
        }
        let bounds = query_node_bounds(query, *node)?;
        if bounds_overlap_for_floor(collider, bounds) {
            sum = sum
                .checked_add(i64::from(bounds.min.y))
                .ok_or(SolidMotionError::ArithmeticOverflow)?;
            count += 1;
        }
    }
    average_height(sum, count)
}

fn stop_at_zone(
    zones: &[SolidZoneView<'_>],
    state: &mut SolidMotionState,
    next_translation: &mut Vec3,
    object_zone: &mut Option<usize>,
    context: SolidMotionContext,
    effects: &mut Vec<SolidEffect>,
) -> Result<(), SolidMotionError> {
    if let Some(containing) = find_containing_zone(zones, *next_translation)? {
        if *object_zone != Some(containing) {
            effects.push(SolidEffect::ZoneChanged {
                previous: *object_zone,
                current: containing,
            });
        }
        *object_zone = Some(containing);
    } else if let Some(zone_index) = *object_zone {
        if let Some(zone) = zones.get(zone_index).copied() {
            let bottom = zone.scaled_rect()?.origin.y;
            let object_bottom = next_translation
                .y
                .checked_add(state.local_bound.min.y)
                .ok_or(SolidMotionError::ArithmeticOverflow)?;
            if object_bottom < bottom {
                if context.quirks.drown_when_below_zone {
                    effects.push(SolidEffect::SendEvent {
                        target: SolidEventTarget::MovingObject,
                        event: EVENT_DROWN,
                        argument: 0,
                        reason: SolidEventReason::OutsideZone,
                    });
                }
                if zone.graphics_flags & 2 != 0 && state.invincibility_state != 2 {
                    effects.push(SolidEffect::SendEvent {
                        target: SolidEventTarget::MovingObject,
                        event: EVENT_FALL_KILL,
                        argument: 0x6400,
                        reason: SolidEventReason::OutsideZone,
                    });
                } else {
                    next_translation.y = bottom
                        .checked_sub(state.local_bound.min.y)
                        .ok_or(SolidMotionError::ArithmeticOverflow)?;
                    state.floor_impact_velocity = state.velocity.y;
                    state.velocity.y = 0;
                    state.status_a |= STATUS_GROUNDLAND;
                    state.floor_impact_stamp = context.frame_stamp;
                }
            }
        } else {
            effects.push(SolidEffect::MissingZone);
        }
    } else {
        effects.push(SolidEffect::MissingZone);
    }
    if let Some(zone_index) = *object_zone
        && let Some(zone) = zones.get(zone_index).copied()
        && zone.graphics_flags & 4 != 0
        && state.translation.y < zone.water_y
        && context.quirks.lethal_river_water
    {
        effects.push(SolidEffect::SendEvent {
            target: SolidEventTarget::MovingObject,
            event: EVENT_DROWN,
            argument: 0x2_7100,
            reason: SolidEventReason::Water,
        });
    }
    Ok(())
}

fn find_containing_zone(
    zones: &[SolidZoneView<'_>],
    point: Vec3,
) -> Result<Option<usize>, SolidMotionError> {
    for (index, zone) in zones.iter().copied().enumerate().rev() {
        if zone.scaled_rect()?.contains(point)? {
            return Ok(Some(index));
        }
    }
    Ok(None)
}

fn query_bound(translation: Vec3) -> Result<Bounds3, SolidMotionError> {
    let min = checked_vec_add(
        translation,
        Vec3 {
            x: -76_800,
            y: -68_480,
            z: -76_800,
        },
    )?;
    let max = checked_vec_add(
        translation,
        Vec3 {
            x: 76_800,
            y: 238_720,
            z: 76_800,
        },
    )?;
    Bounds3::new(min, max).ok_or(SolidMotionError::ArithmeticOverflow)
}

fn query_node_bounds(
    query: &SolidQuery,
    node: SolidQueryNode,
) -> Result<Bounds3, SolidMotionError> {
    let min = checked_vec_add(query.nodes_bound.min, node.relative_origin)?;
    let dimensions = Vec3 {
        x: node.zone_dimensions.x >> node.level.min(node.max_depth[0]),
        y: node.zone_dimensions.y >> node.level.min(node.max_depth[1]),
        z: node.zone_dimensions.z >> node.level.min(node.max_depth[2]),
    };
    let max = checked_vec_add(min, dimensions)?;
    Bounds3::new(min, max).ok_or(SolidMotionError::ArithmeticOverflow)
}

fn rect_intersects_bound(rect: ScaledRect, bound: Bounds3) -> Result<bool, SolidMotionError> {
    let end = rect.end()?;
    Ok(rect.origin.x < bound.max.x
        && rect.origin.y < bound.max.y
        && rect.origin.z < bound.max.z
        && end.x >= bound.min.x
        && end.y >= bound.min.y
        && end.z >= bound.min.z)
}

fn bounds_overlap_for_floor(collider: Bounds3, node: Bounds3) -> bool {
    !(collider.max.x < node.min.x
        || collider.max.y < node.min.y
        || collider.max.z < node.min.z
        || node.max.x < collider.min.x
        || node.max.y < collider.min.y
        || node.max.z < collider.min.z)
}

fn source_bound_intersection(a: Bounds3, b: Bounds3) -> bool {
    b.max.y >= a.min.y
        && b.min.y < a.max.y
        && b.min.x < a.max.x
        && b.min.z < a.max.z
        && b.max.x >= a.min.x
        && b.max.z >= a.min.z
}

fn bound_strictly_contains(outer: Bounds3, inner: Bounds3) -> bool {
    inner.min.x >= outer.min.x
        && inner.min.y >= outer.min.y
        && inner.min.z >= outer.min.z
        && inner.max.x < outer.max.x
        && inner.max.y < outer.max.y
        && inner.max.z < outer.max.z
}

fn checked_translate_bound(bound: Bounds3, translation: Vec3) -> Result<Bounds3, SolidMotionError> {
    let min = checked_vec_add(bound.min, translation)?;
    let max = checked_vec_add(bound.max, translation)?;
    Bounds3::new(min, max).ok_or(SolidMotionError::ArithmeticOverflow)
}

fn inset_horizontal(bound: Bounds3, amount: i32) -> Result<Bounds3, SolidMotionError> {
    let min = Vec3 {
        x: bound
            .min
            .x
            .checked_add(amount)
            .ok_or(SolidMotionError::ArithmeticOverflow)?,
        y: bound.min.y,
        z: bound
            .min
            .z
            .checked_add(amount)
            .ok_or(SolidMotionError::ArithmeticOverflow)?,
    };
    let max = Vec3 {
        x: bound
            .max
            .x
            .checked_sub(amount)
            .ok_or(SolidMotionError::ArithmeticOverflow)?,
        y: bound.max.y,
        z: bound
            .max
            .z
            .checked_sub(amount)
            .ok_or(SolidMotionError::ArithmeticOverflow)?,
    };
    Bounds3::new(min, max).ok_or(SolidMotionError::ArithmeticOverflow)
}

fn average_height(sum: i64, count: u32) -> Result<Option<i32>, SolidMotionError> {
    if count == 0 {
        return Ok(None);
    }
    i32::try_from(sum / i64::from(count))
        .map(Some)
        .map_err(|_| SolidMotionError::ArithmeticOverflow)
}

fn maximum_step(displacement: Vec3) -> Vec3 {
    let magnitude = displacement.x.unsigned_abs() / MAX_HORIZONTAL_DISPLACEMENT as u32;
    let magnitude = magnitude.max(displacement.y.unsigned_abs() / MAX_VERTICAL_DISPLACEMENT as u32);
    let magnitude =
        magnitude.max(displacement.z.unsigned_abs() / MAX_HORIZONTAL_DISPLACEMENT as u32);
    let divisor = i32::try_from(magnitude.saturating_add(1)).unwrap_or(i32::MAX);
    Vec3 {
        x: displacement.x / divisor,
        y: displacement.y / divisor,
        z: displacement.z / divisor,
    }
}

const fn clamp_remaining_step(remaining: i32, maximum: i32) -> i32 {
    if maximum == 0 {
        return remaining;
    }
    if remaining.unsigned_abs() >= maximum.unsigned_abs() {
        maximum
    } else {
        remaining
    }
}

fn checked_mul_div(value: i32, multiplier: i32, divisor: i32) -> Result<i32, SolidMotionError> {
    let product = i64::from(value)
        .checked_mul(i64::from(multiplier))
        .ok_or(SolidMotionError::ArithmeticOverflow)?;
    i32::try_from(product / i64::from(divisor)).map_err(|_| SolidMotionError::ArithmeticOverflow)
}

fn checked_sub(left: i32, right: i32) -> Result<i32, SolidMotionError> {
    left.checked_sub(right)
        .ok_or(SolidMotionError::ArithmeticOverflow)
}

fn checked_vec_add(left: Vec3, right: Vec3) -> Result<Vec3, SolidMotionError> {
    Ok(Vec3 {
        x: left
            .x
            .checked_add(right.x)
            .ok_or(SolidMotionError::ArithmeticOverflow)?,
        y: left
            .y
            .checked_add(right.y)
            .ok_or(SolidMotionError::ArithmeticOverflow)?,
        z: left
            .z
            .checked_add(right.z)
            .ok_or(SolidMotionError::ArithmeticOverflow)?,
    })
}

fn checked_vec_sub(left: Vec3, right: Vec3) -> Result<Vec3, SolidMotionError> {
    Ok(Vec3 {
        x: left
            .x
            .checked_sub(right.x)
            .ok_or(SolidMotionError::ArithmeticOverflow)?,
        y: left
            .y
            .checked_sub(right.y)
            .ok_or(SolidMotionError::ArithmeticOverflow)?,
        z: left
            .z
            .checked_sub(right.z)
            .ok_or(SolidMotionError::ArithmeticOverflow)?,
    })
}

const CIRCLE_BITMAP: [u32; 32] = [
    0x000f_f000,
    0x003f_fc00,
    0x00ff_ff00,
    0x01ff_ff80,
    0x03ff_ffc0,
    0x07ff_ffe0,
    0x0fff_fff0,
    0x1fff_fff8,
    0x3fff_fffc,
    0x3fff_fffc,
    0x7fff_fffe,
    0x7fff_fffe,
    0xffff_ffff,
    0xffff_ffff,
    0xffff_ffff,
    0xffff_ffff,
    0xffff_ffff,
    0xffff_ffff,
    0xffff_ffff,
    0xffff_ffff,
    0x7fff_fffe,
    0x7fff_fffe,
    0x3fff_fffc,
    0x3fff_fffc,
    0x1fff_fff8,
    0x0fff_fff0,
    0x07ff_ffe0,
    0x03ff_ffc0,
    0x01ff_ff80,
    0x00ff_ff00,
    0x003f_fc00,
    0x000f_f000,
];

#[derive(Clone, Debug, Eq, PartialEq)]
struct WallScratch {
    bitmap: [u32; 32],
    cache: [u32; 128],
}

impl Default for WallScratch {
    fn default() -> Self {
        Self {
            bitmap: [0; 32],
            cache: [0; 128],
        }
    }
}

impl WallScratch {
    fn plot_circle(&mut self, x: i32, z: i32) {
        if x.unsigned_abs() >= 32 || z.unsigned_abs() >= 32 {
            return;
        }
        let (cache_index, bit) = if x < 0 {
            (((z + 32) * 2) as usize, 1_u32 << (x + 32))
        } else {
            (((z + 32) * 2 + 1) as usize, 1_u32 << x)
        };
        if self.cache[cache_index] & bit != 0 {
            return;
        }
        self.cache[cache_index] |= bit;
        let (start, end, mut row) = if z < 0 { (-z, 32, 0) } else { (0, 32 - z, z) };
        for source_row in start..end {
            let bits = CIRCLE_BITMAP[source_row as usize];
            self.bitmap[row as usize] |= if x < 0 { bits << -x } else { bits >> x };
            row += 1;
        }
    }

    fn plot_rectangle(&mut self, x1: i32, z1: i32, x2: i32, z2: i32, set: bool) {
        let x1 = x1.saturating_add(15);
        let mut z1 = z1.saturating_add(16);
        let x2 = x2.saturating_add(16);
        let mut z2 = z2.saturating_add(16);
        if x2 <= 0 || x1 >= 32 || z1 >= 32 {
            return;
        }
        let mut bits = u32::MAX;
        if x2 < 32 {
            bits <<= (32 - x2) as u32;
        }
        if x1 >= 0 {
            let arithmetic = (i32::MIN >> x1) as u32;
            bits &= !arithmetic;
        }
        if bits == 0 {
            return;
        }
        z1 = z1.max(0);
        z2 = z2.min(32);
        for row in z1.max(0)..z2.max(0) {
            if set {
                self.bitmap[row as usize] |= bits;
            } else {
                self.bitmap[row as usize] &= !bits;
            }
        }
    }

    fn is_open(&self, x: i32, z: i32) -> bool {
        if !(0..32).contains(&x) || !(0..32).contains(&z) {
            return false;
        }
        self.bitmap[z as usize] & (0x8000_0000 >> x) == 0
    }
}

#[allow(clippy::too_many_arguments)]
fn plot_walls(
    query: &SolidQuery,
    candidates: &[SolidObjectCandidate],
    state: &SolidMotionState,
    translation: Vec3,
    context: SolidMotionContext,
    scratch: &mut WallScratch,
    effects: &mut Vec<SolidEffect>,
    include_collisions: bool,
) -> Result<(), SolidMotionError> {
    if context.current_world_graphics_flags & 0x0010_0000 == 0 {
        let flags = if state.status_c & 2 != 0 || (2..=4).contains(&state.invincibility_state) {
            2
        } else {
            0
        };
        plot_query_walls(
            query,
            flags,
            state
                .translation
                .y
                .checked_add(context.quirks.land_offset)
                .ok_or(SolidMotionError::ArithmeticOverflow)?,
            state
                .translation
                .y
                .checked_add(50 << 8)
                .ok_or(SolidMotionError::ArithmeticOverflow)?,
            translation
                .y
                .checked_add(665 << 8)
                .ok_or(SolidMotionError::ArithmeticOverflow)?,
            translation.x,
            translation.z,
            scratch,
        )?;
    }
    plot_object_walls(
        candidates,
        state,
        translation,
        context.current_world_graphics_flags,
        scratch,
        effects,
        include_collisions,
    )
}

#[allow(clippy::too_many_arguments)]
fn plot_query_walls(
    query: &SolidQuery,
    flag: i32,
    test_y1_type_one: i32,
    test_y1: i32,
    test_y2: i32,
    translation_x: i32,
    translation_z: i32,
    scratch: &mut WallScratch,
) -> Result<(), SolidMotionError> {
    for node in &query.nodes {
        let bounds = query_node_bounds(query, *node)?;
        let kind = node.kind();
        let subtype = node.subtype();
        if kind == 3 || kind == 4 {
            continue;
        }
        if kind == 1 {
            if bounds.max.y <= test_y1_type_one || test_y2 <= bounds.min.y {
                continue;
            }
        } else if subtype == 0 || subtype > 38 || flag != 0 {
            if bounds.max.y <= test_y1 || test_y2 <= bounds.min.y {
                continue;
            }
        } else {
            continue;
        }
        let x1 = checked_mul_div(checked_sub(bounds.min.x, translation_x)?, 4, 8192)?;
        let x2 = checked_mul_div(checked_sub(bounds.max.x, translation_x)?, 4, 8192)?;
        let z1 = checked_mul_div(checked_sub(bounds.min.z, translation_z)?, 4, 8192)?;
        let z2 = checked_mul_div(checked_sub(bounds.max.z, translation_z)?, 4, 8192)?;
        plot_node_perimeter(scratch, x1, z1, x2, z2);
    }
    Ok(())
}

fn plot_node_perimeter(scratch: &mut WallScratch, x1: i32, z1: i32, x2: i32, z2: i32) {
    if (-31..32).contains(&z1) {
        for_forward_eight(x1, x2, false, |x| {
            scratch.plot_circle(x, z1);
        });
    }
    if (-31..32).contains(&x2) {
        for_forward_eight(z1, z2, false, |z| {
            scratch.plot_circle(x2, z);
        });
    }
    if (-31..32).contains(&z2) {
        for_reverse_eight(x2, x1, false, |x| {
            scratch.plot_circle(x, z2);
        });
    }
    if (-31..32).contains(&x1) {
        for_reverse_eight(z2, z1, false, |z| {
            scratch.plot_circle(x1, z);
        });
    }
}

fn for_forward_eight(start: i32, end: i32, inclusive: bool, mut visit: impl FnMut(i32)) {
    let mut value = i64::from(start);
    let end = i64::from(end);
    if value < -31 {
        value += ((-31 - value + 7) / 8) * 8;
    }
    while value <= 31 && (value < end || (inclusive && value == end)) {
        visit(value as i32);
        value += 8;
    }
}

fn for_reverse_eight(start: i32, end: i32, inclusive: bool, mut visit: impl FnMut(i32)) {
    let mut value = i64::from(start);
    let end = i64::from(end);
    if value > 31 {
        value -= ((value - 31 + 7) / 8) * 8;
    }
    while value >= -31 && (value > end || (inclusive && value == end)) {
        visit(value as i32);
        value -= 8;
    }
}

#[allow(clippy::too_many_arguments)]
fn plot_object_walls(
    candidates: &[SolidObjectCandidate],
    state: &SolidMotionState,
    translation: Vec3,
    current_world_graphics_flags: u32,
    scratch: &mut WallScratch,
    effects: &mut Vec<SolidEffect>,
    include_collisions: bool,
) -> Result<(), SolidMotionError> {
    let zone_has_walls = current_world_graphics_flags & 0x0010_0000 == 0;
    let object_bound = checked_translate_bound(state.local_bound, translation)?;
    let test_bound = Bounds3::new(
        Vec3 {
            x: object_bound
                .min
                .x
                .checked_sub(100 << 8)
                .ok_or(SolidMotionError::ArithmeticOverflow)?,
            y: object_bound.min.y,
            z: object_bound
                .min
                .z
                .checked_sub(100 << 8)
                .ok_or(SolidMotionError::ArithmeticOverflow)?,
        },
        Vec3 {
            x: object_bound
                .max
                .x
                .checked_add(100 << 8)
                .ok_or(SolidMotionError::ArithmeticOverflow)?,
            y: object_bound.max.y,
            z: object_bound
                .max
                .z
                .checked_add(100 << 8)
                .ok_or(SolidMotionError::ArithmeticOverflow)?,
        },
    )
    .ok_or(SolidMotionError::ArithmeticOverflow)?;
    for candidate in candidates {
        if include_collisions
            && state.collider == Some(candidate.id)
            && test_bound.min.y >= candidate.bounds.max.y
        {
            continue;
        }
        if !source_bound_intersection(test_bound, candidate.bounds) {
            continue;
        }
        let node_bound = inset_horizontal(candidate.bounds, candidate.hotspot_size)?;
        let eligible = (state.state_flags & 0x10 == 0 && state.invincibility_state != 5)
            || candidate.category != 0x300
            || (candidate.status_c & 0x1012 != 0 && candidate.state_flags & 0x10020 == 0);
        if zone_has_walls
            && candidate.status_b & SOLID_SIDE != 0
            && eligible
            && candidate.bounds.max.y >= test_bound.min.y
            && test_bound.max.y > candidate.bounds.min.y
        {
            if candidate.object_type == 11 {
                let dx = (i64::from(translation.x) - i64::from(candidate.translation.x)) >> 8;
                let dz = (i64::from(translation.z) - i64::from(candidate.translation.z)) >> 8;
                let distance = integer_sqrt((dx * dx + dz * dz) as u64)
                    .checked_mul(0x100)
                    .ok_or(SolidMotionError::ArithmeticOverflow)?;
                if distance <= 0x19_000 {
                    continue;
                }
            }
            let x1 = checked_mul_div(checked_sub(node_bound.min.x, translation.x)?, 4, 8192)?;
            let z1 = checked_mul_div(checked_sub(node_bound.min.z, translation.z)?, 4, 8192)?;
            let x2 = checked_mul_div(checked_sub(node_bound.max.x, translation.x)?, 4, 8192)?;
            let z2 = checked_mul_div(checked_sub(node_bound.max.z, translation.z)?, 4, 8192)?;
            scratch.plot_rectangle(x1, z1, x2, z2, true);
            if include_collisions {
                for_forward_eight(x1, x2, false, |x| {
                    scratch.plot_circle(x, z1);
                });
                for_forward_eight(z1, z2, false, |z| {
                    scratch.plot_circle(x2, z);
                });
                for_reverse_eight(x2, x1, true, |x| {
                    scratch.plot_circle(x, z2);
                });
                for_reverse_eight(z2, z1, true, |z| {
                    scratch.plot_circle(x1, z);
                });
            }
        }
        if include_collisions && source_bound_intersection(object_bound, candidate.bounds) {
            effects.push(SolidEffect::ObjectCollision {
                candidate: candidate.id,
                accepted: state.collider.is_none() || state.collider == Some(candidate.id),
            });
        }
    }
    Ok(())
}

fn solid_replot_walls(
    query: &SolidQuery,
    translation: Vec3,
    flags: i32,
    set: bool,
    scratch: &mut WallScratch,
) -> Result<usize, SolidMotionError> {
    let mut count = 0_usize;
    for node in &query.nodes {
        let node_type = u16::from(node.kind()) + 1;
        if node_type == 5 || (flags == 0 && node_type != 2) || (flags == 1 && node_type == 2) {
            continue;
        }
        let bounds = query_node_bounds(query, *node)?;
        let distance_min_y = checked_sub(bounds.min.y, translation.y)?;
        let distance_max_y = checked_sub(bounds.max.y, translation.y)?;
        let plot = (flags == 0 && distance_min_y <= (100 << 8) && distance_max_y >= -(400 << 8))
            || (flags == 1 && distance_max_y >= 0)
            || (flags != 0 && flags != 1);
        if !plot {
            continue;
        }
        let x1 = checked_mul_div(checked_sub(bounds.min.x, translation.x)?, 4, 8192)?;
        let z1 = checked_mul_div(checked_sub(bounds.min.z, translation.z)?, 4, 8192)?;
        let x2 = checked_mul_div(checked_sub(bounds.max.x, translation.x)?, 4, 8192)?;
        let z2 = checked_mul_div(checked_sub(bounds.max.z, translation.z)?, 4, 8192)?;
        scratch.plot_rectangle(x1, z1, x2, z2, set);
        count += 1;
    }
    Ok(count)
}

fn find_nearest_open(
    scratch: &WallScratch,
    mut x: i32,
    mut z: i32,
    collider: Option<u32>,
    collider_type: Option<u32>,
) -> Option<(i32, i32, bool)> {
    let mut retried_offset = false;
    loop {
        if scratch.is_open(x, z) {
            return Some((x, z, true));
        }
        for squared_distance in 1..=512 {
            for offset_x in (1..=16).rev() {
                for offset_z in 0..=offset_x {
                    if offset_x * offset_x + offset_z * offset_z != squared_distance {
                        continue;
                    }
                    let candidates = [
                        (x.saturating_add(offset_x), z.saturating_add(offset_z)),
                        (x.saturating_add(offset_x), z.saturating_sub(offset_z)),
                        (x.saturating_sub(offset_x), z.saturating_add(offset_z)),
                        (x.saturating_sub(offset_x), z.saturating_sub(offset_z)),
                    ];
                    for candidate in candidates {
                        if scratch.is_open(candidate.0, candidate.1) {
                            return Some((candidate.0, candidate.1, true));
                        }
                    }
                    if offset_x != offset_z {
                        let inverse = [
                            (x.saturating_add(offset_z), z.saturating_add(offset_x)),
                            (x.saturating_add(offset_z), z.saturating_sub(offset_x)),
                            (x.saturating_sub(offset_z), z.saturating_add(offset_x)),
                            (x.saturating_sub(offset_z), z.saturating_sub(offset_x)),
                        ];
                        for candidate in inverse {
                            if scratch.is_open(candidate.0, candidate.1) {
                                return Some((candidate.0, candidate.1, true));
                            }
                        }
                    }
                }
            }
        }
        if retried_offset
            || x == 16
            || z == 16
            || collider.is_none()
            || collider_type == Some(0x22)
            || scratch.bitmap.iter().all(|row| *row == u32::MAX)
        {
            break;
        }
        x = x.saturating_add(16);
        z = z.saturating_add(16);
        retried_offset = true;
    }
    None
}

fn integer_sqrt(value: u64) -> i32 {
    if value == 0 {
        return 0;
    }
    let mut result = 0_u64;
    let mut bit = 1_u64 << 62;
    while bit > value {
        bit >>= 2;
    }
    let mut remainder = value;
    while bit != 0 {
        if remainder >= result + bit {
            remainder -= result + bit;
            result = (result >> 1) + bit;
        } else {
            result >>= 1;
        }
        bit >>= 2;
    }
    i32::try_from(result).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn leaf_zone(raw_node: u16, bytes: &[u8]) -> SolidZoneView<'_> {
        SolidZoneView::new(
            [-100, -100, -100],
            [200, 100, 200],
            raw_node,
            [0, 0, 0],
            bytes,
        )
        .unwrap()
    }

    fn moving_state(translation: Vec3, velocity: Vec3) -> SolidMotionState {
        SolidMotionState {
            translation,
            velocity,
            local_bound: TEST_BOUND_EVENT,
            ..SolidMotionState::default()
        }
    }

    #[test]
    fn falling_object_lands_on_static_floor_leaf() {
        let bytes = [0_u8; ZDAT_RECT_BYTES];
        let zone = leaf_zone(0x0003, &bytes);
        let state = moving_state(
            Vec3 {
                x: 0,
                y: 10_000,
                z: 0,
            },
            Vec3 {
                x: 0,
                y: -200_000,
                z: 0,
            },
        );
        let outcome = solve_retail_solid_motion(
            &[zone],
            &[],
            state,
            Vec3 {
                x: 0,
                y: -20_000,
                z: 0,
            },
            SolidMotionContext {
                object_zone: Some(0),
                frame_stamp: 17,
                ..SolidMotionContext::default()
            },
            SmoothStopMemory::default(),
        )
        .unwrap();
        assert_eq!(outcome.state.translation.y, 1);
        assert_eq!(outcome.floor, Some(1));
        assert_eq!(outcome.state.velocity.y, 0);
        assert_eq!(outcome.state.floor_impact_velocity, -200_000);
        assert_eq!(outcome.state.floor_impact_stamp, 17);
        assert_ne!(outcome.state.status_a & STATUS_GROUNDLAND, 0);
    }

    #[test]
    fn gravity_displacement_and_terminal_speed_match_retail_order() {
        let velocity = Vec3 {
            x: 102_400,
            y: -1_024_000,
            z: -51_200,
        };
        assert_eq!(
            scale_velocity_for_tick(velocity, 34).unwrap(),
            Vec3 {
                x: 3_400,
                y: -34_000,
                z: -1_700,
            }
        );
        assert_eq!(apply_retail_gravity(velocity, 34).unwrap().y, -1_160_000);
        let terminal = apply_retail_gravity(
            Vec3 {
                x: 0,
                y: -0x2e_dfff,
                z: 0,
            },
            34,
        )
        .unwrap();
        assert_eq!(terminal.y, -0x2e_e000);
    }

    #[test]
    fn malformed_child_offset_is_reported_without_dereference() {
        let mut bytes = [0_u8; 40];
        bytes[36..38].copy_from_slice(&100_u16.to_le_bytes());
        bytes[38..40].copy_from_slice(&0x0003_u16.to_le_bytes());
        let zone =
            SolidZoneView::new([-100, -100, -100], [200, 100, 200], 36, [1, 0, 0], &bytes).unwrap();
        let bound = Bounds3::new(
            Vec3 {
                x: -20_000,
                y: -20_000,
                z: -20_000,
            },
            Vec3 {
                x: -10_000,
                y: 20_000,
                z: 20_000,
            },
        )
        .unwrap();
        assert_eq!(
            query_zone_octrees(&[zone], bound),
            Err(SolidMotionError::MalformedOctreeOffset { offset: 100 })
        );
    }

    #[test]
    fn wall_leaf_readjusts_horizontal_destination() {
        let mut bytes = [0_u8; 44];
        let children = [0x0003_u16, 0_u16, 0x0003_u16, 0x0001_u16];
        for (index, child) in children.into_iter().enumerate() {
            let offset = 36 + index * 2;
            bytes[offset..offset + 2].copy_from_slice(&child.to_le_bytes());
        }
        let zone =
            SolidZoneView::new([-100, -100, -100], [200, 200, 200], 36, [1, 1, 0], &bytes).unwrap();
        let state = moving_state(
            Vec3 {
                x: -10_000,
                y: 1,
                z: 0,
            },
            Vec3::ZERO,
        );
        let outcome = solve_retail_solid_motion(
            &[zone],
            &[],
            state,
            Vec3 {
                x: 20_000,
                y: 0,
                z: 0,
            },
            SolidMotionContext {
                object_zone: Some(0),
                ..SolidMotionContext::default()
            },
            SmoothStopMemory::default(),
        )
        .unwrap();
        assert!(outcome.stopped_by_wall);
        assert!(outcome.state.translation.x < 10_000);
    }

    #[test]
    fn event_leaf_preserves_typed_contact_and_dispatch() {
        let bytes = [0_u8; ZDAT_RECT_BYTES];
        let zone = leaf_zone(0x0013, &bytes);
        let state = moving_state(
            Vec3 {
                x: 0,
                y: 10_000,
                z: 0,
            },
            Vec3 {
                x: 0,
                y: -20_000,
                z: 0,
            },
        );
        let outcome = solve_retail_solid_motion(
            &[zone],
            &[],
            state,
            Vec3 {
                x: 0,
                y: -20_000,
                z: 0,
            },
            SolidMotionContext {
                object_zone: Some(0),
                ..SolidMotionContext::default()
            },
            SmoothStopMemory::default(),
        )
        .unwrap();
        assert_eq!(outcome.state.event, 0x0700);
        assert!(outcome.effects.iter().any(|effect| matches!(
            effect,
            SolidEffect::NodeContact {
                raw_node: 0x0013,
                event: Some(0x0700),
                ..
            }
        )));
        assert!(outcome.effects.contains(&SolidEffect::SendEvent {
            target: SolidEventTarget::MovingObject,
            event: 0x0700,
            argument: 0x6400,
            reason: SolidEventReason::Surface,
        }));
    }

    proptest! {
        #[test]
        fn arbitrary_serialized_children_never_escape_checked_results(
            payload in prop::collection::vec(any::<u8>(), 0..128),
            root in any::<u16>(),
            depth_x in 0_u16..=4,
            depth_y in 0_u16..=4,
            depth_z in 0_u16..=4,
        ) {
            let view = SolidZoneView::new(
                [-1, -1, -1],
                [2, 2, 2],
                root,
                [depth_x, depth_y, depth_z],
                &payload,
            );
            if let Ok(zone) = view {
                let bound = Bounds3::new(
                    Vec3 { x: -512, y: -512, z: -512 },
                    Vec3 { x: 512, y: 512, z: 512 },
                ).unwrap();
                let result = query_zone_octrees(&[zone], bound);
                let expected = result.is_ok() || matches!(
                    result,
                    Err(SolidMotionError::MalformedOctreeOffset { .. }
                        | SolidMotionError::OctreeDepthExceeded
                        | SolidMotionError::QueryCapacityExceeded)
                );
                prop_assert!(expected, "unexpected query result");
            }
        }
    }
}
