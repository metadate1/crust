//! Checked retail world-shader arithmetic.
//!
//! The original software renderer applies these effects after its saturated
//! RTPS projection.  Keeping the arithmetic here makes that ordering explicit
//! and avoids relying on C signed overflow or invalid shift counts.

use core::fmt;

use crate::command::ScreenPoint;
use crate::projection::Vec3i;

pub const ZONE_FLAG_FOG: u32 = 0x10;
pub const ZONE_FLAG_RIPPLE: u32 = 0x100;
pub const ZONE_FLAG_LIGHTNING: u32 = 0x200;
pub const ZONE_FLAG_DARK2: u32 = 0x400;

/// Source-order world transform selected by the active ZDAT graphics flags.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorldShaderMode {
    Plain,
    Fog,
    Ripple,
    Lightning,
    Dark,
    Dark2,
}

impl WorldShaderMode {
    /// Reproduces `LevelUpdate`'s mutually exclusive dispatch priority.
    #[must_use]
    pub const fn from_flags(flags: u32) -> Self {
        if flags & ZONE_FLAG_DARK2 != 0 {
            Self::Dark2
        } else if flags & (ZONE_FLAG_FOG | ZONE_FLAG_LIGHTNING)
            == ZONE_FLAG_FOG | ZONE_FLAG_LIGHTNING
        {
            Self::Dark
        } else if flags & ZONE_FLAG_FOG != 0 {
            Self::Fog
        } else if flags & ZONE_FLAG_RIPPLE != 0 {
            Self::Ripple
        } else if flags & ZONE_FLAG_LIGHTNING != 0 {
            Self::Lightning
        } else {
            Self::Plain
        }
    }
}

/// One source lightning interpolation channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LightningChannel {
    /// Raw 32-bit component globals. The wrapper multiplies each by sixteen
    /// before assigning it to an eight-bit scratch color.
    pub color: [u32; 3],
    /// Twelve-bit interpolation weight.
    pub t: i32,
}

/// Inputs retained by the source Dark2 renderer globals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Dark2Parameters {
    /// Q24.8 world-space illumination point.
    pub illumination: [i32; 3],
    pub shift_add: u32,
    pub shift_sub: u32,
    pub ambient_effect_clear: i32,
    pub ambient_effect_set: i32,
    /// Persistent `params.far_color1` scratch bytes.
    pub target: [u8; 3],
}

/// Checked rejection of malformed or undefined source shader arithmetic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorldShaderError {
    InvalidShift { context: &'static str, value: u32 },
    InvalidFogCutoff(i64),
    ArithmeticOutOfRange(&'static str),
}

impl fmt::Display for WorldShaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidShift { context, value } => {
                write!(formatter, "world shader {context} shift {value} is invalid")
            }
            Self::InvalidFogCutoff(value) => {
                write!(
                    formatter,
                    "world shader fog cutoff {value} does not fit u16"
                )
            }
            Self::ArithmeticOutOfRange(context) => {
                write!(
                    formatter,
                    "world shader {context} exceeds signed 32-bit range"
                )
            }
        }
    }
}

impl std::error::Error for WorldShaderError {}

/// Computes the low-half cutoff packed into each source world tag.
///
/// `fog_z` is retained even though normal level initialization currently sets
/// it to zero. Fog exempts backdrops with `0xffff`; Dark deliberately does not.
///
/// # Errors
///
/// Returns [`WorldShaderError`] when serialized values require an invalid
/// shift, overflow signed source arithmetic, or produce a cutoff outside the
/// packed 16-bit range.
pub fn fog_cutoff(
    level: u32,
    visibility_depth: u32,
    fog_z: i32,
    shift: u32,
    is_backdrop: bool,
    exempt_backdrop: bool,
) -> Result<i32, WorldShaderError> {
    validate_shift("fog", shift)?;
    if is_backdrop && exempt_backdrop {
        return Ok(i32::from(u16::MAX));
    }

    let visibility = i32::try_from(visibility_depth)
        .map_err(|_| WorldShaderError::ArithmeticOutOfRange("fog visibility"))?;
    let cutoff = if matches!(level, 0x14 | 0x16) {
        let fog = fog_z
            .checked_mul(3_200)
            .ok_or(WorldShaderError::ArithmeticOutOfRange("fog-z product"))?;
        let adjusted = visibility
            .checked_sub(fog)
            .ok_or(WorldShaderError::ArithmeticOutOfRange("fog visibility"))?
            >> 8;
        i64::from(adjusted)
            .checked_sub(1_600)
            .ok_or(WorldShaderError::ArithmeticOutOfRange("fog cutoff"))?
    } else {
        let mut cutoff = i64::from(visibility >> 8)
            .checked_sub(800)
            .ok_or(WorldShaderError::ArithmeticOutOfRange("fog cutoff"))?;
        if shift != 0 {
            cutoff = cutoff
                .checked_sub(1_200)
                .ok_or(WorldShaderError::ArithmeticOutOfRange("shifted fog cutoff"))?;
        }
        cutoff
    };
    if !(0..=i64::from(u16::MAX)).contains(&cutoff) {
        return Err(WorldShaderError::InvalidFogCutoff(cutoff));
    }
    i32::try_from(cutoff).map_err(|_| WorldShaderError::InvalidFogCutoff(cutoff))
}

/// Applies source fog to one already-projected vertex color.
///
/// # Errors
///
/// Returns [`WorldShaderError`] for invalid shifts or overflowing fixed-point
/// interpolation arithmetic.
pub fn apply_fog(
    color: [u8; 3],
    screen_z: i32,
    cutoff: i32,
    shift: u32,
    target: [u8; 3],
) -> Result<[u8; 3], WorldShaderError> {
    validate_shift("fog", shift)?;
    if screen_z <= cutoff {
        return Ok(color);
    }
    let distance = screen_z
        .checked_sub(cutoff)
        .ok_or(WorldShaderError::ArithmeticOutOfRange("fog distance"))?;
    let widened = i64::from(distance)
        .checked_shl(shift)
        .ok_or(WorldShaderError::ArithmeticOutOfRange("fog interpolation"))?;
    let t = i32::try_from(widened)
        .map_err(|_| WorldShaderError::ArithmeticOutOfRange("fog interpolation"))?;
    fixed_color_blend(color, target, t)
}

/// Applies the effect-bit-selected source lightning channel.
///
/// # Errors
///
/// Returns [`WorldShaderError`] when the fixed-point color interpolation
/// exceeds the defined signed arithmetic range.
pub fn apply_lightning(
    color: [u8; 3],
    effect: bool,
    clear: LightningChannel,
    set: LightningChannel,
) -> Result<[u8; 3], WorldShaderError> {
    let channel = if effect { set } else { clear };
    fixed_color_blend(color, wrapped_lightning_color(channel.color), channel.t)
}

/// Applies source Dark in its required Lightning-then-Fog order.
///
/// # Errors
///
/// Returns [`WorldShaderError`] for invalid fog shifts or overflowing
/// fixed-point shader arithmetic.
pub fn apply_dark(
    color: [u8; 3],
    effect: bool,
    screen_z: i32,
    cutoff: i32,
    shift: u32,
    clear: LightningChannel,
    set: LightningChannel,
) -> Result<[u8; 3], WorldShaderError> {
    let lightning = apply_lightning(color, effect, clear, set)?;
    apply_fog(
        lightning,
        screen_z,
        cutoff,
        shift,
        wrapped_lightning_color(clear.color),
    )
}

/// Applies source Dark2 using saturated screen coordinates and the already
/// camera-transformed world translation.
///
/// # Errors
///
/// Returns [`WorldShaderError`] for invalid shifts or any source arithmetic
/// that would overflow signed 32-bit values.
pub fn apply_dark2(
    color: [u8; 3],
    effect: bool,
    screen: ScreenPoint,
    world_translation: Vec3i,
    parameters: Dark2Parameters,
) -> Result<[u8; 3], WorldShaderError> {
    validate_shift("Dark2 add", parameters.shift_add)?;
    validate_shift("Dark2 subtract", parameters.shift_sub)?;
    let illumination = parameters.illumination.map(|component| component >> 8);
    let screen = [screen.x, screen.y, screen.z];
    let translation = [
        world_translation.x,
        world_translation.y,
        world_translation.z,
    ];
    let mut distance = 0_i32;
    for axis in 0..3 {
        let delta = screen[axis]
            .checked_add(translation[axis])
            .and_then(|value| value.checked_sub(illumination[axis]))
            .ok_or(WorldShaderError::ArithmeticOutOfRange("Dark2 delta"))?;
        distance = distance
            .checked_add(
                delta
                    .checked_abs()
                    .ok_or(WorldShaderError::ArithmeticOutOfRange(
                        "Dark2 absolute delta",
                    ))?,
            )
            .ok_or(WorldShaderError::ArithmeticOutOfRange("Dark2 distance"))?;
    }
    let adjustment = (distance >> parameters.shift_add)
        .checked_sub(distance >> parameters.shift_sub)
        .ok_or(WorldShaderError::ArithmeticOutOfRange(
            "Dark2 shift adjustment",
        ))?;
    let ambient = if effect {
        parameters.ambient_effect_set
    } else {
        parameters.ambient_effect_clear
    };
    let t = distance
        .checked_add(adjustment)
        .and_then(|value| value.checked_add(ambient))
        .ok_or(WorldShaderError::ArithmeticOutOfRange(
            "Dark2 interpolation",
        ))?
        .clamp(0, 4_096);
    fixed_color_blend(color, parameters.target, t)
}

/// Source fixed-point color interpolation, including its asymmetric IR clamp.
///
/// # Errors
///
/// Returns [`WorldShaderError`] if a multiply or addition exceeds the defined
/// signed 32-bit source arithmetic range.
pub fn fixed_color_blend(
    color: [u8; 3],
    target: [u8; 3],
    t: i32,
) -> Result<[u8; 3], WorldShaderError> {
    let mut output = [0_u8; 3];
    for channel in 0..3 {
        let c1 = i32::from(color[channel]) << 4;
        let c2 = i32::from(target[channel]) << 4;
        let delta = (c2 - c1).clamp(-0x800, 0x7ff);
        let numerator = (c1 << 12)
            .checked_add(
                t.checked_mul(delta)
                    .ok_or(WorldShaderError::ArithmeticOutOfRange("color product"))?,
            )
            .ok_or(WorldShaderError::ArithmeticOutOfRange("color numerator"))?;
        output[channel] = u8::try_from((numerator >> 16).clamp(0, 255))
            .map_err(|_| WorldShaderError::ArithmeticOutOfRange("color channel"))?;
    }
    Ok(output)
}

#[must_use]
pub fn wrapped_lightning_color(color: [u32; 3]) -> [u8; 3] {
    color.map(|component| component.wrapping_mul(16).to_le_bytes()[0])
}

fn validate_shift(context: &'static str, value: u32) -> Result<(), WorldShaderError> {
    if value >= i32::BITS {
        Err(WorldShaderError::InvalidShift { context, value })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_shader_flag_priority_matches_main_dispatch() {
        for (flags, expected) in [
            (0, WorldShaderMode::Plain),
            (0x200, WorldShaderMode::Lightning),
            (0x100, WorldShaderMode::Ripple),
            (0x10, WorldShaderMode::Fog),
            (0x210, WorldShaderMode::Dark),
            (0x400, WorldShaderMode::Dark2),
            (0x110, WorldShaderMode::Fog),
            (0x300, WorldShaderMode::Ripple),
            (0x710, WorldShaderMode::Dark2),
        ] {
            assert_eq!(WorldShaderMode::from_flags(flags), expected, "{flags:#x}");
        }
    }

    #[test]
    fn fixed_color_blend_clamps_ir_delta_before_multiply() {
        assert_eq!(fixed_color_blend([7; 3], [250; 3], 0).unwrap(), [7; 3]);
        assert_eq!(
            fixed_color_blend([0; 3], [255; 3], 4_096).unwrap(),
            [127; 3]
        );
        assert_eq!(
            fixed_color_blend([255; 3], [0; 3], 4_096).unwrap(),
            [127; 3]
        );
        assert_eq!(
            fixed_color_blend([100; 3], [120; 3], 2_048).unwrap(),
            [110; 3]
        );
    }

    #[test]
    fn fog_uses_a_strict_cutoff_and_exempts_backdrops() {
        let color = [10, 20, 30];
        assert_eq!(apply_fog(color, 100, 100, 12, [250; 3]).unwrap(), color);
        assert_eq!(
            apply_fog(color, 101, 100, 12, [250; 3]).unwrap(),
            [137, 147, 157]
        );
        assert_eq!(
            fog_cutoff(0x09, 2_000_u32 << 8, 0, 0, true, true).unwrap(),
            i32::from(u16::MAX)
        );
        assert_eq!(
            fog_cutoff(0x09, 2_000_u32 << 8, 0, 0, false, true),
            Ok(1_200)
        );
        assert_eq!(
            fog_cutoff(0x14, 3_200_u32 << 8, 0, 0, false, true),
            Ok(1_600)
        );
    }

    #[test]
    fn lightning_wraps_targets_and_selects_the_effect_channel() {
        let clear = LightningChannel {
            color: [255, 43, 11],
            t: 4_096,
        };
        let set = LightningChannel {
            color: [0, 15, 31],
            t: 4_096,
        };
        assert_eq!(wrapped_lightning_color(clear.color), [240, 176, 176]);
        assert_eq!(
            apply_lightning([0; 3], false, clear, set).unwrap(),
            [127; 3]
        );
        assert_eq!(
            apply_lightning([0; 3], true, clear, set).unwrap(),
            [0, 127, 127]
        );
    }

    #[test]
    fn dark_is_lightning_then_fog() {
        let clear = LightningChannel {
            color: [4, 8, 12],
            t: 2_048,
        };
        let set = LightningChannel {
            color: [20, 24, 28],
            t: 3_072,
        };
        let source_order = apply_dark([200, 100, 20], true, 101, 100, 12, clear, set).unwrap();
        let reversed = apply_lightning(
            apply_fog(
                [200, 100, 20],
                101,
                100,
                12,
                wrapped_lightning_color(clear.color),
            )
            .unwrap(),
            true,
            clear,
            set,
        )
        .unwrap();
        assert_ne!(source_order, reversed);
    }

    #[test]
    fn dark2_uses_saturated_screen_coordinates_and_effect_ambient() {
        let parameters = Dark2Parameters {
            illumination: [-256, 512, 768],
            shift_add: 2,
            shift_sub: 4,
            ambient_effect_clear: -10_000,
            ambient_effect_set: 3_000,
            target: [255; 3],
        };
        let clear = apply_dark2(
            [0; 3],
            false,
            ScreenPoint {
                x: 10,
                y: 20,
                z: 30,
            },
            Vec3i {
                x: 40,
                y: 50,
                z: 60,
            },
            parameters,
        )
        .unwrap();
        let set = apply_dark2(
            [0; 3],
            true,
            ScreenPoint {
                x: 10,
                y: 20,
                z: 30,
            },
            Vec3i {
                x: 40,
                y: 50,
                z: 60,
            },
            parameters,
        )
        .unwrap();
        assert_eq!(clear, [0; 3]);
        assert!(set.iter().all(|channel| *channel > 0));
    }

    #[test]
    fn malformed_shifts_and_signed_overflow_are_rejected() {
        assert!(matches!(
            apply_fog([0; 3], 1, 0, 32, [0; 3]),
            Err(WorldShaderError::InvalidShift { .. })
        ));
        assert_eq!(
            apply_fog([0; 3], 2, 0, 30, [0; 3]),
            Err(WorldShaderError::ArithmeticOutOfRange("fog interpolation"))
        );
        assert!(matches!(
            apply_dark2(
                [0; 3],
                false,
                ScreenPoint {
                    x: i32::MAX,
                    y: 0,
                    z: 0
                },
                Vec3i { x: 1, y: 0, z: 0 },
                Dark2Parameters {
                    illumination: [0; 3],
                    shift_add: 0,
                    shift_sub: 0,
                    ambient_effect_clear: 0,
                    ambient_effect_set: 0,
                    target: [0; 3],
                },
            ),
            Err(WorldShaderError::ArithmeticOutOfRange("Dark2 delta"))
        ));
    }
}
