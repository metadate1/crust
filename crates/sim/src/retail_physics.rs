//! Source-compatible fixed-point movement for retail GOOL objects.
//!
//! The original update interleaves pure object math with zone collision and
//! path queries. This module keeps that ordering explicit: [`begin_retail_physics`]
//! produces a movement plan, the caller resolves free/solid translation and an
//! optional path query, and [`finalize_retail_physics`] applies limits, the
//! synthetic floor plane, and gravity. All arithmetic that can cross the
//! 32-bit register boundary uses explicit wrapping operations.

use crate::{Angle12, Vec2, Vec3};

/// Object has touched the configured floor plane during this update.
pub const STATUS_A_GROUND_LAND: u32 = 0x0000_0001;
/// Rotation is proceeding in the source's clockwise direction.
pub const STATUS_A_TARGET_ROTATION_DIRECTION: u32 = 0x0000_0008;
/// The object reached its target rotation.
pub const STATUS_A_REACHED_TARGET_ROTATION: u32 = 0x0000_0800;
/// Controller state used by the retail acceleration-state selection.
pub const STATUS_A_MOVEMENT_ACTIVE: u32 = 0x0000_2000;
/// Path orientation may replace the object's translation with its path point.
pub const STATUS_A_INVALID_PATH: u32 = 0x0000_0200;

pub const STATUS_B_ROTATE_Y: u32 = 0x0000_0001;
pub const STATUS_B_STOPPED_BY_SOLID: u32 = 0x0000_0008;
pub const STATUS_B_COLLIDABLE: u32 = 0x0000_0010;
pub const STATUS_B_GRAVITY: u32 = 0x0000_0020;
pub const STATUS_B_TRANSLATION_MOTION: u32 = 0x0000_0040;
pub const STATUS_B_DPAD_CONTROL: u32 = 0x0000_0080;
/// Clamp the velocity/misc-A vector to `-target_rotation.y..target_rotation.y`.
pub const STATUS_B_LIMIT_VELOCITY: u32 = 0x0000_1000;
pub const STATUS_B_ROTATE_X: u32 = 0x0000_2000;
pub const STATUS_B_SOLID_GROUND: u32 = 0x0000_4000;
pub const STATUS_B_ORIENT_ON_PATH: u32 = 0x0000_8000;
pub const STATUS_B_ROTATE_Y_ALTERNATE: u32 = 0x0008_0000;
/// Enables the source rotation routine's four-band approach deceleration.
pub const STATUS_B_ROTATION_DECELERATION: u32 = 0x2000_0000;

pub const STATE_FLAG_GROUND: u32 = 0x0000_0004;
pub const STATE_FLAG_AIR: u32 = 0x0000_0008;
pub const STATE_FLAG_FLING: u32 = 0x0000_0010;

const STATUS_A_FRAME_CLEAR_MASK: u32 = 0xffca_a07e;
const FLOOR_EVENT_PRESERVE_MOVEMENT: u32 = 0x1200;
const SOLID_RESPONSE_EVENT: u32 = 0x00ff;
const TERMINAL_FALL_VELOCITY: i32 = -0x2e_e000;

/// Raw GOOL Euler angles in their retail Y/X/Z storage order.
///
/// Fields remain `i32` because GOOL registers may temporarily contain values
/// outside twelve bits. Individual rotation operations normalize only the
/// component they update, matching the source runtime.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetailAngles {
    pub y: i32,
    pub x: i32,
    pub z: i32,
}

/// Scalar and vector fields consumed or changed by retail object physics.
///
/// `velocity` is the source `misc_a` union. `angular_velocity_x` and
/// `target_rotation` occupy the source `misc_b` union, so
/// `target_rotation.y` is also the per-axis velocity limit when
/// [`STATUS_B_LIMIT_VELOCITY`] is set.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetailPhysicsState {
    pub translation: Vec3,
    pub rotation: RetailAngles,
    pub velocity: Vec3,
    pub angular_velocity_x: i32,
    pub target_rotation: Vec2,
    pub status_a: u32,
    pub status_b: u32,
    pub state_flags: u32,
    pub speed: i32,
    pub invincibility_state: u32,
    pub floor_y: i32,
    pub floor_impact_stamp: u32,
    pub floor_impact_velocity: i32,
    pub event: u32,
    pub angular_velocity_y: i32,
}

/// Per-frame values that are external to one GOOL object.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetailPhysicsContext {
    /// Retail time ticks; values above `0x66` are capped by every physics path.
    pub ticks_per_frame: u32,
    pub game_state_playing: bool,
    /// Camera yaw added to the directional-pad movement table.
    pub camera_rotation_xz: i32,
    /// Normalized retail pad bits. The directional nibble is bits 12 through 15.
    pub pad_held: u32,
    pub frame_stamp: u32,
    /// GOOL header type. Type five retains its collider link between frames.
    pub object_type: u32,
}

/// One entry in the exact sixteen-entry directional-pad movement table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailMoveState {
    pub direction: u8,
    pub angle: i32,
    pub speed_scale: i32,
}

/// One entry in the retail acceleration table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailAccelerationState {
    pub acceleration: i32,
    pub maximum_speed: i32,
    pub unknown: u32,
    pub deceleration: i32,
}

const MOVE_STATES: [RetailMoveState; 16] = [
    RetailMoveState {
        direction: 8,
        angle: 0,
        speed_scale: 0x100,
    },
    RetailMoveState {
        direction: 0,
        angle: 0x800,
        speed_scale: 0x100,
    },
    RetailMoveState {
        direction: 2,
        angle: 0x400,
        speed_scale: 0x100,
    },
    RetailMoveState {
        direction: 1,
        angle: 0x600,
        speed_scale: 0x147,
    },
    RetailMoveState {
        direction: 4,
        angle: 0,
        speed_scale: 0x100,
    },
    RetailMoveState {
        direction: 8,
        angle: 0,
        speed_scale: 0,
    },
    RetailMoveState {
        direction: 3,
        angle: 0x200,
        speed_scale: 0x147,
    },
    RetailMoveState {
        direction: 8,
        angle: 0,
        speed_scale: 0,
    },
    RetailMoveState {
        direction: 6,
        angle: 0xc00,
        speed_scale: 0x100,
    },
    RetailMoveState {
        direction: 7,
        angle: 0xa00,
        speed_scale: 0x147,
    },
    RetailMoveState {
        direction: 8,
        angle: 0,
        speed_scale: 0,
    },
    RetailMoveState {
        direction: 8,
        angle: 0,
        speed_scale: 0,
    },
    RetailMoveState {
        direction: 5,
        angle: 0xe00,
        speed_scale: 0x147,
    },
    RetailMoveState {
        direction: 8,
        angle: 0,
        speed_scale: 0,
    },
    RetailMoveState {
        direction: 8,
        angle: 0,
        speed_scale: 0,
    },
    RetailMoveState {
        direction: 8,
        angle: 0,
        speed_scale: 0,
    },
];

const ACCELERATION_STATES: [RetailAccelerationState; 7] = [
    RetailAccelerationState {
        acceleration: 0,
        maximum_speed: 0x7d000,
        unknown: 0,
        deceleration: 0,
    },
    RetailAccelerationState {
        acceleration: 0x0027_1000,
        maximum_speed: 0x96000,
        unknown: 0x1e,
        deceleration: 0x0027_1000,
    },
    RetailAccelerationState {
        acceleration: 0x0013_8800,
        maximum_speed: 0x96000,
        unknown: 0x1e,
        deceleration: 0x9c400,
    },
    RetailAccelerationState {
        acceleration: 0x0019_0000,
        maximum_speed: 0xaae60,
        unknown: 0x0f,
        deceleration: 0x0019_0000,
    },
    RetailAccelerationState {
        acceleration: 0x0027_1000,
        maximum_speed: 0xc8000,
        unknown: 0x1e,
        deceleration: 0x0027_1000,
    },
    RetailAccelerationState {
        acceleration: 0x001a_0aaa,
        maximum_speed: 0x64000,
        unknown: 0x1e,
        deceleration: 0x0027_1000,
    },
    RetailAccelerationState {
        acceleration: 0x000d_0555,
        maximum_speed: 0x64000,
        unknown: 0x1e,
        deceleration: 0x0009_c400,
    },
];

/// Translation work that must occur between the two pure physics phases.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetailTranslationMode {
    None,
    Free,
    StoppedBySolid,
}

/// Result of the controller/rotation phase and input to environment movement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailPhysicsPlan {
    pub translation_mode: RetailTranslationMode,
    pub displacement: Vec3,
    /// The caller should clear its checked collider handle when true.
    pub clear_collider: bool,
}

/// Environment-facing result of the final physics phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailPhysicsResult {
    /// The caller still applies the Crash stamp/range gates before registering.
    pub register_collision_bound: bool,
}

/// Returns the movement-table entry selected by retail pad bits 12 through 15.
#[must_use]
pub const fn move_state_for_pad(pad_held: u32) -> RetailMoveState {
    MOVE_STATES[((pad_held >> 12) & 0x0f) as usize]
}

/// Reproduces the source's ordered acceleration-state override rules.
#[must_use]
pub const fn acceleration_state_for(
    state_flags: u32,
    status_a: u32,
    invincibility_state: u32,
) -> RetailAccelerationState {
    let mut index = 0;
    if state_flags & STATE_FLAG_GROUND != 0 {
        index = if status_a & STATUS_A_MOVEMENT_ACTIVE != 0 {
            5
        } else {
            1
        };
    }
    if invincibility_state == 5 {
        index = 4;
    }
    if state_flags & STATE_FLAG_AIR != 0 {
        index = if status_a & STATUS_A_MOVEMENT_ACTIVE != 0 {
            6
        } else {
            2
        };
    }
    if state_flags & STATE_FLAG_FLING != 0 {
        index = 3;
    }
    ACCELERATION_STATES[index]
}

const fn frame_scale(ticks_per_frame: u32) -> i32 {
    if ticks_per_frame > 0x66 {
        0x66
    } else {
        ticks_per_frame as i32
    }
}

fn wrapping_mul_div(value: i32, multiplier: i32, divisor: i32) -> i32 {
    value.wrapping_mul(multiplier) / divisor
}

const fn angle12(value: i32) -> i32 {
    value & 0x0fff
}

fn shortest_rotation_delta(current: i32, target: i32) -> i32 {
    let current = angle12(current);
    let target = angle12(target);
    let mut delta = target.wrapping_sub(current);
    if delta.wrapping_abs() > 0x800 {
        if delta > 0 {
            delta = delta.wrapping_sub(0x1000);
        } else {
            delta = delta.wrapping_add(0x1000);
        }
    }
    delta
}

/// Source-compatible `GoolObjectRotate`.
///
/// `rotation_status` corresponds to the optional object pointer in the C
/// routine: when present it receives direction/reached flags. `decelerate`
/// corresponds to status-B bit `0x2000_0000` on that object.
#[must_use]
pub fn rotate_toward(
    current: i32,
    target: i32,
    speed: i32,
    ticks_per_frame: u32,
    decelerate: bool,
    mut rotation_status: Option<&mut u32>,
) -> i32 {
    let mut velocity = wrapping_mul_div(speed, frame_scale(ticks_per_frame), 1024);
    let current = angle12(current);
    let target = angle12(target);
    let mut delta = shortest_rotation_delta(current, target);
    let absolute_delta = delta.wrapping_abs();

    if decelerate && absolute_delta < velocity.wrapping_mul(4) {
        if absolute_delta >= velocity.wrapping_mul(2) {
            if absolute_delta >= velocity.wrapping_mul(3) {
                velocity = wrapping_mul_div(velocity, 3, 4);
            } else {
                velocity /= 2;
            }
        } else if absolute_delta >= velocity {
            velocity /= 4;
        } else {
            velocity /= 8;
        }
    }

    if absolute_delta < velocity {
        if let Some(status_a) = rotation_status.as_deref_mut() {
            *status_a |= STATUS_A_REACHED_TARGET_ROTATION;
        }
        return target;
    }

    if absolute_delta == 0x800 && target >= 0x800 {
        delta = delta.wrapping_neg();
    }
    if delta >= 0 {
        if let Some(status_a) = rotation_status {
            *status_a &= !STATUS_A_TARGET_ROTATION_DIRECTION;
        }
        angle12(current.wrapping_add(velocity))
    } else {
        if let Some(status_a) = rotation_status {
            *status_a |= STATUS_A_TARGET_ROTATION_DIRECTION;
        }
        angle12(current.wrapping_sub(velocity))
    }
}

/// Source-compatible `GoolObjectRotate2`.
#[must_use]
pub fn rotate_toward_alternate(
    current: i32,
    target: i32,
    speed: i32,
    ticks_per_frame: u32,
    rotation_status: Option<&mut u32>,
) -> i32 {
    let velocity = wrapping_mul_div(speed, frame_scale(ticks_per_frame), 1024);
    let current = angle12(current);
    let target = angle12(target);
    let delta = shortest_rotation_delta(current, target);
    let absolute_delta = delta.wrapping_abs();
    if delta != 0 && (absolute_delta >= velocity.wrapping_abs() || (delta ^ velocity) < 0) {
        angle12(current.wrapping_add(velocity))
    } else {
        if let Some(status_a) = rotation_status {
            *status_a |= STATUS_A_REACHED_TARGET_ROTATION;
        }
        target
    }
}

fn control_direction(
    state: &mut RetailPhysicsState,
    context: RetailPhysicsContext,
    scale: i32,
) -> i32 {
    let acceleration_state =
        acceleration_state_for(state.state_flags, state.status_a, state.invincibility_state);
    let move_state = move_state_for_pad(context.pad_held);
    if move_state.direction == 8 {
        let deceleration = wrapping_mul_div(acceleration_state.deceleration, scale, 1024);
        state.speed = state.speed.wrapping_sub(deceleration);
        if state.speed < 0 {
            state.speed = 0;
        }
        return move_state.speed_scale;
    }

    let current_angle = state.target_rotation.x;
    let target_angle = angle12(move_state.angle.wrapping_add(context.camera_rotation_xz));
    let mut delta = target_angle.wrapping_sub(current_angle).wrapping_abs();
    if delta > 0x800 {
        delta = 0x1000_i32.wrapping_sub(delta);
    }

    if state.state_flags & STATE_FLAG_AIR != 0 {
        let scaled_acceleration = wrapping_mul_div(acceleration_state.acceleration, scale, 1024);
        let cosine = i32::from(Angle12::new(delta).cos_q12()) >> 6;
        let acceleration = cosine.wrapping_mul(scaled_acceleration) >> 6;
        state.speed = state.speed.wrapping_add(acceleration);
        if state.speed <= 0x100 {
            state.target_rotation.x = target_angle;
        } else if delta < 0x7c8 {
            state.target_rotation.x = rotate_toward(
                current_angle,
                target_angle,
                0x0f00,
                context.ticks_per_frame,
                false,
                None,
            );
        } else {
            let deceleration = wrapping_mul_div(acceleration_state.deceleration, scale, 1024);
            state.speed = state.speed.wrapping_sub(deceleration);
            if state.speed < 0 {
                state.speed = 0;
            }
        }
    } else if state.speed <= 0x100 || delta <= 0x400 {
        state.target_rotation.x = target_angle;
        let acceleration = wrapping_mul_div(acceleration_state.acceleration, scale, 1024);
        state.speed = state.speed.wrapping_add(acceleration);
    } else {
        state.speed = 0;
    }

    if state.speed > acceleration_state.maximum_speed {
        state.speed = acceleration_state.maximum_speed;
    }
    move_state.speed_scale
}

/// Runs controller, per-frame status clearing, rotations, and displacement
/// calculation. The caller must next resolve the returned translation mode.
pub fn begin_retail_physics(
    state: &mut RetailPhysicsState,
    context: RetailPhysicsContext,
) -> RetailPhysicsPlan {
    let initial_status_a = state.status_a;
    let initial_status_b = state.status_b;
    let scale = frame_scale(context.ticks_per_frame);

    if initial_status_b & STATUS_B_DPAD_CONTROL != 0 && context.game_state_playing {
        let speed_scale = control_direction(state, context, scale);
        let speed = state.speed.wrapping_mul(speed_scale) >> 8;
        let target_angle = Angle12::new(state.target_rotation.x);
        state.velocity.x = (i32::from(target_angle.sin_q12()) >> 4).wrapping_mul(speed) >> 8;
        state.velocity.z = (i32::from(target_angle.cos_q12()) >> 4).wrapping_mul(speed) >> 8;
    }

    if initial_status_a & STATUS_A_GROUND_LAND != 0 && state.event != FLOOR_EVENT_PRESERVE_MOVEMENT
    {
        state.status_a &= !STATUS_A_MOVEMENT_ACTIVE;
    }
    state.status_a &= STATUS_A_FRAME_CLEAR_MASK;

    let decelerate_rotation = initial_status_b & STATUS_B_ROTATION_DECELERATION != 0;
    if initial_status_b & STATUS_B_ROTATE_Y != 0 {
        state.rotation.x = rotate_toward(
            state.rotation.x,
            state.target_rotation.x,
            state.angular_velocity_x,
            context.ticks_per_frame,
            decelerate_rotation,
            Some(&mut state.status_a),
        );
    }
    if initial_status_b & STATUS_B_ROTATE_Y_ALTERNATE != 0 {
        state.rotation.x = rotate_toward_alternate(
            state.rotation.x,
            state.target_rotation.x,
            state.angular_velocity_x,
            context.ticks_per_frame,
            Some(&mut state.status_a),
        );
    }
    if initial_status_b & STATUS_B_ROTATE_X != 0 {
        state.rotation.y = rotate_toward(
            state.rotation.y,
            state.target_rotation.y,
            state.angular_velocity_y,
            context.ticks_per_frame,
            false,
            None,
        );
    }

    let mut translation_mode = RetailTranslationMode::None;
    let mut displacement = Vec3::ZERO;
    if initial_status_b & STATUS_B_TRANSLATION_MOTION != 0 {
        displacement = Vec3 {
            x: wrapping_mul_div(state.velocity.x, scale, 1024),
            y: wrapping_mul_div(state.velocity.y, scale, 1024),
            z: wrapping_mul_div(state.velocity.z, scale, 1024),
        };
        if initial_status_b & STATUS_B_STOPPED_BY_SOLID != 0 {
            state.event = SOLID_RESPONSE_EVENT;
            translation_mode = RetailTranslationMode::StoppedBySolid;
        } else {
            translation_mode = RetailTranslationMode::Free;
        }
    }

    RetailPhysicsPlan {
        translation_mode,
        displacement,
        clear_collider: context.object_type != 5,
    }
}

/// Applies the plan's displacement only when the source selected free motion.
/// Returns whether translation changed.
pub fn apply_free_movement(state: &mut RetailPhysicsState, plan: RetailPhysicsPlan) -> bool {
    if plan.translation_mode != RetailTranslationMode::Free {
        return false;
    }
    state.translation = state.translation.wrapping_add(plan.displacement);
    true
}

/// Whether the post-translation path callback is required.
#[must_use]
pub const fn path_orientation_requested(state: &RetailPhysicsState) -> bool {
    state.status_b & STATUS_B_ORIENT_ON_PATH != 0
}

/// Applies the location produced by the caller's path-orientation query.
///
/// The query itself may update other public state fields before this call.
pub fn apply_path_orientation(state: &mut RetailPhysicsState, path_location: Vec3) {
    state.floor_y = path_location.y;
    if state.status_a & STATUS_A_INVALID_PATH != 0 {
        state.translation = path_location;
    }
}

fn source_limit(value: i32, limit: i32) -> i32 {
    value.min(limit).max(limit.wrapping_neg())
}

/// Applies velocity limiting, the object floor plane, gravity, and exposes the
/// remaining collidable-bound request to the caller.
pub fn finalize_retail_physics(
    state: &mut RetailPhysicsState,
    context: RetailPhysicsContext,
) -> RetailPhysicsResult {
    if state.status_b & STATUS_B_LIMIT_VELOCITY != 0 {
        let limit = state.target_rotation.y;
        state.velocity.x = source_limit(state.velocity.x, limit);
        state.velocity.y = source_limit(state.velocity.y, limit);
        state.velocity.z = source_limit(state.velocity.z, limit);
    }

    let status_b = state.status_b;
    if status_b & STATUS_B_SOLID_GROUND != 0 && state.translation.y <= state.floor_y {
        state.translation.y = state.floor_y;
        state.status_a |= STATUS_A_GROUND_LAND;
        state.floor_impact_stamp = context.frame_stamp;
        if state.velocity.y < 0 {
            state.floor_impact_velocity = state.velocity.y;
            state.velocity.y = 0;
        }
    }
    if status_b & STATUS_B_GRAVITY != 0 {
        let gravity = 4000_i32.wrapping_mul(frame_scale(context.ticks_per_frame));
        state.velocity.y = state.velocity.y.wrapping_sub(gravity);
        if state.velocity.y < TERMINAL_FALL_VELOCITY {
            state.velocity.y = TERMINAL_FALL_VELOCITY;
        }
    }

    RetailPhysicsResult {
        register_collision_bound: status_b & STATUS_B_COLLIDABLE != 0,
    }
}

/// Convenience path for objects that require neither solid nor path callbacks.
pub fn update_retail_physics_free(
    state: &mut RetailPhysicsState,
    context: RetailPhysicsContext,
) -> RetailPhysicsResult {
    let plan = begin_retail_physics(state, context);
    apply_free_movement(state, plan);
    finalize_retail_physics(state, context)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn context() -> RetailPhysicsContext {
        RetailPhysicsContext {
            ticks_per_frame: 34,
            game_state_playing: true,
            camera_rotation_xz: 0,
            pad_held: 0,
            frame_stamp: 77,
            object_type: 3,
        }
    }

    #[test]
    fn direction_table_matches_retail_words() {
        let expected = [
            (8, 0, 0x100),
            (0, 0x800, 0x100),
            (2, 0x400, 0x100),
            (1, 0x600, 0x147),
            (4, 0, 0x100),
            (8, 0, 0),
            (3, 0x200, 0x147),
            (8, 0, 0),
            (6, 0xc00, 0x100),
            (7, 0xa00, 0x147),
            (8, 0, 0),
            (8, 0, 0),
            (5, 0xe00, 0x147),
            (8, 0, 0),
            (8, 0, 0),
            (8, 0, 0),
        ];
        for (nibble, (direction, angle, speed_scale)) in expected.into_iter().enumerate() {
            assert_eq!(
                move_state_for_pad((nibble as u32) << 12),
                RetailMoveState {
                    direction,
                    angle,
                    speed_scale
                }
            );
        }
    }

    #[test]
    fn acceleration_selection_preserves_ordered_overrides() {
        assert_eq!(acceleration_state_for(0, 0, 0), ACCELERATION_STATES[0]);
        assert_eq!(
            acceleration_state_for(STATE_FLAG_GROUND, 0, 0),
            ACCELERATION_STATES[1]
        );
        assert_eq!(
            acceleration_state_for(STATE_FLAG_GROUND, STATUS_A_MOVEMENT_ACTIVE, 0),
            ACCELERATION_STATES[5]
        );
        assert_eq!(
            acceleration_state_for(STATE_FLAG_GROUND, 0, 5),
            ACCELERATION_STATES[4]
        );
        assert_eq!(
            acceleration_state_for(STATE_FLAG_GROUND | STATE_FLAG_AIR, 0, 5),
            ACCELERATION_STATES[2]
        );
        assert_eq!(
            acceleration_state_for(
                STATE_FLAG_GROUND | STATE_FLAG_AIR | STATE_FLAG_FLING,
                STATUS_A_MOVEMENT_ACTIVE,
                5,
            ),
            ACCELERATION_STATES[3]
        );
    }

    #[test]
    fn no_direction_decelerates_but_conflicting_direction_zeroes_velocity_scale() {
        let mut state = RetailPhysicsState {
            status_b: STATUS_B_DPAD_CONTROL,
            status_a: STATUS_A_MOVEMENT_ACTIVE,
            state_flags: STATE_FLAG_GROUND,
            speed: 0x50000,
            ..RetailPhysicsState::default()
        };
        begin_retail_physics(&mut state, context());
        let expected = 0x50000 - (0x0027_1000_i32 * 34) / 1024;
        assert_eq!(state.speed, expected);
        assert_ne!(state.velocity.z, 0);

        let mut conflicting = state;
        let mut conflicting_context = context();
        conflicting_context.pad_held = 5 << 12;
        begin_retail_physics(&mut conflicting, conflicting_context);
        assert_eq!(conflicting.velocity.x, 0);
        assert_eq!(conflicting.velocity.z, 0);
    }

    #[test]
    fn ground_acceleration_is_camera_relative_and_terminally_clamped() {
        let mut state = RetailPhysicsState {
            status_b: STATUS_B_DPAD_CONTROL,
            status_a: STATUS_A_MOVEMENT_ACTIVE,
            state_flags: STATE_FLAG_GROUND,
            speed: 0x63ff0,
            ..RetailPhysicsState::default()
        };
        let mut frame = context();
        frame.pad_held = 4 << 12;
        frame.camera_rotation_xz = 0x400;
        begin_retail_physics(&mut state, frame);
        assert_eq!(state.target_rotation.x, 0x400);
        assert_eq!(state.speed, 0x64000);
        assert_eq!(state.velocity.x, 0x64000);
        assert_eq!(state.velocity.z, 0);
    }

    #[test]
    fn airborne_reverse_input_decelerates_instead_of_turning() {
        let mut state = RetailPhysicsState {
            status_b: STATUS_B_DPAD_CONTROL,
            status_a: STATUS_A_MOVEMENT_ACTIVE,
            state_flags: STATE_FLAG_AIR,
            speed: 0x40000,
            target_rotation: Vec2 { x: 0, y: 0 },
            ..RetailPhysicsState::default()
        };
        let mut frame = context();
        frame.pad_held = 1 << 12;
        begin_retail_physics(&mut state, frame);
        let angular_acceleration = (-64_i32 * ((0x000d_0555_i32 * 34) / 1024)) >> 6;
        let after_acceleration = 0x40000_i32 + angular_acceleration;
        let expected = (after_acceleration - (0x0009_c400_i32 * 34) / 1024).max(0);
        assert_eq!(state.speed, expected);
        assert_eq!(state.target_rotation.x, 0);
    }

    #[test]
    fn rotate_toward_wraps_shortest_path_and_records_direction() {
        let mut status = u32::MAX;
        let rotated = rotate_toward(0xff0, 0x010, 0x400, 102, false, Some(&mut status));
        assert_eq!(rotated, 0x010);
        assert_ne!(status & STATUS_A_REACHED_TARGET_ROTATION, 0);

        status = 0;
        let rotated = rotate_toward(0x010, 0xff0, 0x100, 102, false, Some(&mut status));
        assert_eq!(rotated, 0xff7);
        assert_ne!(status & STATUS_A_TARGET_ROTATION_DIRECTION, 0);
    }

    #[test]
    fn exact_opposite_rotation_favors_clockwise_direction() {
        let mut status = 0;
        assert_eq!(
            rotate_toward(0, 0x800, 0x400, 102, false, Some(&mut status)),
            0xf9a
        );
        assert_ne!(status & STATUS_A_TARGET_ROTATION_DIRECTION, 0);

        status = 0;
        assert_eq!(
            rotate_toward(0x800, 0, 0x400, 102, false, Some(&mut status)),
            0x79a
        );
        assert_ne!(status & STATUS_A_TARGET_ROTATION_DIRECTION, 0);
    }

    #[test]
    fn rotation_deceleration_uses_all_four_source_bands() {
        // At 102 ticks, speed 1024 produces velocity 102.
        let cases = [(350, 76), (250, 51), (150, 25), (50, 12)];
        for (target, expected_step) in cases {
            assert_eq!(
                rotate_toward(0, target, 1024, 102, true, None),
                expected_step
            );
        }
    }

    #[test]
    fn alternate_rotation_only_marks_a_strictly_reached_target() {
        let mut status = 0;
        assert_eq!(
            rotate_toward_alternate(0, 100, 1000, 102, Some(&mut status)),
            99
        );
        assert_eq!(status & STATUS_A_REACHED_TARGET_ROTATION, 0);

        assert_eq!(
            rotate_toward_alternate(99, 100, 1000, 102, Some(&mut status)),
            100
        );
        assert_ne!(status & STATUS_A_REACHED_TARGET_ROTATION, 0);
    }

    #[test]
    fn begin_phase_clears_exact_status_bits_and_plans_solid_motion() {
        let mut state = RetailPhysicsState {
            velocity: Vec3 {
                x: 1025,
                y: -1025,
                z: 2048,
            },
            status_a: u32::MAX,
            status_b: STATUS_B_TRANSLATION_MOTION | STATUS_B_STOPPED_BY_SOLID,
            event: 0,
            ..RetailPhysicsState::default()
        };
        let plan = begin_retail_physics(&mut state, context());
        assert_eq!(
            state.status_a,
            STATUS_A_FRAME_CLEAR_MASK & !STATUS_A_MOVEMENT_ACTIVE
        );
        assert_eq!(state.event, SOLID_RESPONSE_EVENT);
        assert_eq!(plan.translation_mode, RetailTranslationMode::StoppedBySolid);
        assert_eq!(
            plan.displacement,
            Vec3 {
                x: 34,
                y: -34,
                z: 68
            }
        );
        assert!(plan.clear_collider);
        assert!(!apply_free_movement(&mut state, plan));
        assert_eq!(state.translation, Vec3::ZERO);
    }

    #[test]
    fn floor_event_1200_preserves_movement_flag_until_common_mask() {
        let mut state = RetailPhysicsState {
            status_a: STATUS_A_GROUND_LAND | STATUS_A_MOVEMENT_ACTIVE,
            event: FLOOR_EVENT_PRESERVE_MOVEMENT,
            ..RetailPhysicsState::default()
        };
        begin_retail_physics(&mut state, context());
        assert_ne!(state.status_a & STATUS_A_MOVEMENT_ACTIVE, 0);
    }

    #[test]
    fn free_motion_scales_and_wraps_translation() {
        let mut state = RetailPhysicsState {
            translation: Vec3 {
                x: i32::MAX - 2,
                y: 100,
                z: -100,
            },
            velocity: Vec3 {
                x: 1024,
                y: -1024,
                z: 512,
            },
            status_b: STATUS_B_TRANSLATION_MOTION,
            ..RetailPhysicsState::default()
        };
        let plan = begin_retail_physics(&mut state, context());
        assert_eq!(plan.translation_mode, RetailTranslationMode::Free);
        assert!(apply_free_movement(&mut state, plan));
        assert_eq!(
            state.translation,
            Vec3 {
                x: i32::MIN + 31,
                y: 66,
                z: -83
            }
        );
    }

    #[test]
    fn path_result_updates_floor_and_only_snaps_when_requested() {
        let path = Vec3 {
            x: 10,
            y: 20,
            z: 30,
        };
        let mut state = RetailPhysicsState {
            translation: Vec3 { x: 1, y: 2, z: 3 },
            status_b: STATUS_B_ORIENT_ON_PATH,
            ..RetailPhysicsState::default()
        };
        assert!(path_orientation_requested(&state));
        apply_path_orientation(&mut state, path);
        assert_eq!(state.floor_y, 20);
        assert_eq!(state.translation, Vec3 { x: 1, y: 2, z: 3 });

        state.status_a |= STATUS_A_INVALID_PATH;
        apply_path_orientation(&mut state, path);
        assert_eq!(state.translation, path);
    }

    #[test]
    fn final_phase_limits_all_velocity_components_like_source_macro() {
        let mut state = RetailPhysicsState {
            velocity: Vec3 {
                x: -200,
                y: 25,
                z: 300,
            },
            target_rotation: Vec2 { x: 0, y: 100 },
            status_b: STATUS_B_LIMIT_VELOCITY,
            ..RetailPhysicsState::default()
        };
        finalize_retail_physics(&mut state, context());
        assert_eq!(
            state.velocity,
            Vec3 {
                x: -100,
                y: 25,
                z: 100
            }
        );
    }

    #[test]
    fn floor_impact_precedes_gravity_and_records_the_falling_velocity() {
        let mut state = RetailPhysicsState {
            translation: Vec3 { x: 0, y: -1, z: 0 },
            velocity: Vec3 {
                x: 0,
                y: -20_000,
                z: 0,
            },
            status_b: STATUS_B_SOLID_GROUND | STATUS_B_GRAVITY | STATUS_B_COLLIDABLE,
            floor_y: 300,
            ..RetailPhysicsState::default()
        };
        let result = finalize_retail_physics(&mut state, context());
        assert_eq!(state.translation.y, 300);
        assert_ne!(state.status_a & STATUS_A_GROUND_LAND, 0);
        assert_eq!(state.floor_impact_stamp, 77);
        assert_eq!(state.floor_impact_velocity, -20_000);
        assert_eq!(state.velocity.y, -136_000);
        assert!(result.register_collision_bound);
    }

    #[test]
    fn gravity_caps_ticks_and_terminal_fall_velocity() {
        let mut state = RetailPhysicsState {
            velocity: Vec3 {
                x: 0,
                y: TERMINAL_FALL_VELOCITY + 1,
                z: 0,
            },
            status_b: STATUS_B_GRAVITY,
            ..RetailPhysicsState::default()
        };
        let mut frame = context();
        frame.ticks_per_frame = u32::MAX;
        finalize_retail_physics(&mut state, frame);
        assert_eq!(state.velocity.y, TERMINAL_FALL_VELOCITY);
    }

    #[test]
    fn dpad_is_ignored_outside_playing_state_but_existing_velocity_moves() {
        let mut state = RetailPhysicsState {
            velocity: Vec3 {
                x: 1024,
                y: 0,
                z: 0,
            },
            status_b: STATUS_B_DPAD_CONTROL | STATUS_B_TRANSLATION_MOTION,
            speed: 0x12345,
            ..RetailPhysicsState::default()
        };
        let mut frame = context();
        frame.game_state_playing = false;
        frame.pad_held = 4 << 12;
        let plan = begin_retail_physics(&mut state, frame);
        assert_eq!(state.speed, 0x12345);
        assert_eq!(plan.displacement.x, 34);
    }

    #[test]
    fn type_five_keeps_collider_and_other_types_clear_it() {
        let mut state = RetailPhysicsState::default();
        let mut frame = context();
        frame.object_type = 5;
        assert!(!begin_retail_physics(&mut state, frame).clear_collider);
        frame.object_type = 4;
        assert!(begin_retail_physics(&mut state, frame).clear_collider);
    }

    proptest! {
        #[test]
        fn movement_lookup_ignores_every_bit_outside_the_dpad_nibble(
            other in any::<u32>(),
            nibble in 0_u32..16,
        ) {
            let held = (other & !(0xf << 12)) | (nibble << 12);
            prop_assert_eq!(move_state_for_pad(held), MOVE_STATES[nibble as usize]);
        }

        #[test]
        fn both_rotation_routines_always_return_a_twelve_bit_angle(
            current in any::<i32>(),
            target in any::<i32>(),
            speed in 0_i32..=0x0010_0000,
            ticks in any::<u32>(),
        ) {
            let regular = rotate_toward(current, target, speed, ticks, false, None);
            let alternate = rotate_toward_alternate(current, target, speed, ticks, None);
            prop_assert!((0..0x1000).contains(&regular));
            prop_assert!((0..0x1000).contains(&alternate));
        }

        #[test]
        fn free_translation_uses_componentwise_wrapping_addition(
            x in any::<i32>(),
            y in any::<i32>(),
            z in any::<i32>(),
            dx in any::<i32>(),
            dy in any::<i32>(),
            dz in any::<i32>(),
        ) {
            let mut state = RetailPhysicsState {
                translation: Vec3 { x, y, z },
                ..RetailPhysicsState::default()
            };
            let plan = RetailPhysicsPlan {
                translation_mode: RetailTranslationMode::Free,
                displacement: Vec3 { x: dx, y: dy, z: dz },
                clear_collider: false,
            };
            prop_assert!(apply_free_movement(&mut state, plan));
            prop_assert_eq!(
                state.translation,
                Vec3 {
                    x: x.wrapping_add(dx),
                    y: y.wrapping_add(dy),
                    z: z.wrapping_add(dz),
                }
            );
        }
    }
}
