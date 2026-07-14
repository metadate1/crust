//! Retail object-lighting calculations that mutate live GOOL color state.
//!
//! The original renderer writes its mode-four light matrix and ambient color
//! directly into each displayed object before traversing that object's
//! children. Keeping the calculation in the simulation crate lets both the
//! source-ordered runtime writeback and the renderer's checked projection path
//! use one fixed-point implementation.

use core::fmt;

use crust_formats::stream::ObjectVertexKind;

use crate::math::integer_sqrt;

/// Live inputs consumed by ZDAT object-shader mode four.
///
/// Translations use GOOL's native Q24.8 representation. The reference is the
/// pause object while it exists and the dedicated player otherwise.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectDarkShaderInput {
    pub reference_translation: [i32; 3],
    pub object_translation: [i32; 3],
    pub dark_distance: i32,
}

/// Checked failure while reproducing mode-four's signed 32-bit arithmetic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetailDarkShaderError {
    /// Authored coordinates exceeded an intermediate for which the original
    /// executable relied on signed overflow or another undefined operation.
    ArithmeticOutOfRange(&'static str),
}

impl fmt::Display for RetailDarkShaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ArithmeticOutOfRange(context) => {
                write!(formatter, "object shader mode-four {context} exceeds i32")
            }
        }
    }
}

impl std::error::Error for RetailDarkShaderError {}

/// Source-compatible result of applying one current-zone object shader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailObjectZoneShading {
    /// Effective colors used by this vertex transform and persisted by modes
    /// that mutate the native GOOL object.
    pub colors: [u16; 24],
    /// CVTX color right shift produced only by mode three.
    pub colored_shift: u8,
}

/// Checked failure while evaluating the active ZDAT object shader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetailObjectZoneShaderError {
    /// Mode four cannot run without the live pause/player translation.
    MissingDarkInput,
    Dark(RetailDarkShaderError),
}

impl fmt::Display for RetailObjectZoneShaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDarkInput => {
                formatter.write_str("object shader mode four has no live dark-shader input")
            }
            Self::Dark(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for RetailObjectZoneShaderError {}

/// Applies the source renderer's ZDAT object-shader branch to a live color
/// snapshot. A `None` result is the deliberate mode-two/three depth reject.
/// Modes outside `2..=4` preserve the object unchanged.
///
/// # Errors
///
/// Returns an error when mode four lacks its live reference or malformed
/// coordinates exceed the checked retail signed intermediates.
pub fn apply_retail_object_zone_shader(
    mode: u32,
    vertex_kind: ObjectVertexKind,
    object_colors: [u16; 24],
    zone_colors: [u16; 24],
    camera_depth: i32,
    depth_anchor: i32,
    dark: Option<ObjectDarkShaderInput>,
) -> Result<Option<RetailObjectZoneShading>, RetailObjectZoneShaderError> {
    let mut shading = RetailObjectZoneShading {
        colors: object_colors,
        colored_shift: 0,
    };
    let depth_delta = i64::from(camera_depth) - i64::from(depth_anchor);
    match mode {
        2 => {
            let ramp = depth_delta.saturating_mul(8).max(0);
            if ramp > 0x7fff {
                return Ok(None);
            }
            for (index, output) in shading.colors[..12].iter_mut().enumerate() {
                *output = u16::try_from((i64::from(zone_colors[index]) + ramp).min(0x7fff))
                    .unwrap_or(0x7fff);
            }
            for (relative, output) in shading.colors[12..].iter_mut().enumerate() {
                *output = u16::try_from((i64::from(zone_colors[12 + relative]) + ramp).min(0x1000))
                    .unwrap_or(0x1000);
            }
        }
        3 if vertex_kind == ObjectVertexKind::Lit => {
            let ramp = (depth_delta / 4).max(0);
            if ramp > 28_000 {
                return Ok(None);
            }
            for (source, output) in zone_colors.into_iter().zip(&mut shading.colors) {
                *output = u16::try_from((i64::from(source) - ramp).max(0)).unwrap_or_default();
            }
        }
        3 => {
            shading.colored_shift = u8::try_from((depth_delta / 200).clamp(0, 8)).unwrap_or(8);
        }
        4 => {
            shading.colors = apply_mode_four_object_colors(
                shading.colors,
                dark.ok_or(RetailObjectZoneShaderError::MissingDarkInput)?,
            )
            .map_err(RetailObjectZoneShaderError::Dark)?;
        }
        _ => {}
    }
    Ok(Some(shading))
}

/// Replaces the live object's light matrix and ambient RGB exactly as native
/// ZDAT object-shader mode four does. Color-matrix and intensity words are
/// preserved.
///
/// # Errors
///
/// Returns an error rather than reproducing undefined signed overflow for
/// malformed coordinates outside the retail corpus.
pub fn apply_mode_four_object_colors(
    mut colors: [u16; 24],
    input: ObjectDarkShaderInput,
) -> Result<[u16; 24], RetailDarkShaderError> {
    let initial_x = checked_difference(
        input.object_translation[0],
        input.reference_translation[0],
        "initial x difference",
    )? >> 8;
    let initial_y = (checked_difference(
        input.object_translation[1],
        input.reference_translation[1],
        "initial y difference",
    )? >> 8)
        .checked_sub(800)
        .ok_or(RetailDarkShaderError::ArithmeticOutOfRange(
            "initial y offset",
        ))?;
    let initial_z = checked_difference(
        input.object_translation[2],
        input.reference_translation[2],
        "initial z difference",
    )? >> 8;
    let distance_squared = checked_square_sum(
        [initial_x, initial_y, initial_z],
        "initial distance squared",
    )?;
    let distance = retail_sqrt(distance_squared).max(1);

    let mut direction = [0_i32; 3];
    for (axis, output) in direction.iter_mut().enumerate() {
        let difference = checked_difference(
            input.reference_translation[axis],
            input.object_translation[axis],
            "normalized direction difference",
        )?;
        let numerator =
            difference
                .checked_mul(0x100)
                .ok_or(RetailDarkShaderError::ArithmeticOutOfRange(
                    "normalized direction numerator",
                ))?;
        *output = numerator / distance;
    }

    let complementary_distance = 6_000 - distance;
    if complementary_distance < 0 {
        direction[0] = 0;
        direction[2] = 0;
    }
    let dark_distance = input.dark_distance.max(1);
    let mut light_direction = [0_i32; 3];
    for (source, output) in direction.into_iter().zip(&mut light_direction) {
        let product = source.checked_mul(complementary_distance).ok_or(
            RetailDarkShaderError::ArithmeticOutOfRange("light direction product"),
        )?;
        *output = ((product >> 8) / dark_distance).clamp(-6_000, 6_000);
    }

    for row in 0..3 {
        for (column, component) in light_direction.into_iter().enumerate() {
            colors[row * 3 + column] = wrapping_u16(component);
        }
    }
    let light_magnitude = retail_sqrt(checked_square_sum(
        light_direction,
        "light direction magnitude",
    )?);
    let ambient = u16::try_from(light_magnitude / 32).unwrap_or(u16::MAX);
    colors[9..12].fill(ambient);
    Ok(colors)
}

fn checked_difference(
    left: i32,
    right: i32,
    context: &'static str,
) -> Result<i32, RetailDarkShaderError> {
    left.checked_sub(right)
        .ok_or(RetailDarkShaderError::ArithmeticOutOfRange(context))
}

fn checked_square_sum(
    values: [i32; 3],
    context: &'static str,
) -> Result<i32, RetailDarkShaderError> {
    values.into_iter().try_fold(0_i32, |sum, value| {
        value
            .checked_mul(value)
            .and_then(|square| sum.checked_add(square))
            .ok_or(RetailDarkShaderError::ArithmeticOutOfRange(context))
    })
}

fn retail_sqrt(value: i32) -> i32 {
    if value == 0 {
        return 0;
    }
    debug_assert!(value > 0);
    let leading_zeros = value.leading_zeros() & !1;
    let table_index = if leading_zeros < 24 {
        value >> (24 - leading_zeros)
    } else {
        value << (leading_zeros - 24)
    };
    debug_assert!((64..=255).contains(&table_index));
    // Native's 192-entry table is floor(sqrt(index / 64) * 4096), which is
    // equivalently floor(sqrt(index) * 512).
    let table_value = integer_sqrt(u64::try_from(table_index).unwrap_or_default() << 18);
    let table_value = i32::try_from(table_value).unwrap_or(i32::MAX);
    (table_value << ((31 - leading_zeros) / 2)) >> 12
}

fn wrapping_u16(value: i32) -> u16 {
    let bytes = value.to_le_bytes();
    u16::from_le_bytes([bytes[0], bytes[1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_dark_light_golden_preserves_color_matrix_and_intensity() {
        let mut original = [0_u16; 24];
        for (index, word) in original[12..].iter_mut().enumerate() {
            *word = u16::try_from(index + 1).unwrap();
        }
        let shaded = apply_mode_four_object_colors(
            original,
            ObjectDarkShaderInput {
                reference_translation: [0, 0, 0],
                object_translation: [0, 0, 600 << 8],
                dark_distance: 2_000,
            },
        )
        .unwrap();

        assert_eq!(&shaded[..9], &[0, 0, 65_152, 0, 0, 65_152, 0, 0, 65_152]);
        assert_eq!(&shaded[9..12], &[12; 3]);
        assert_eq!(&shaded[12..], &original[12..]);
    }

    #[test]
    fn malformed_overflow_is_rejected() {
        assert_eq!(
            apply_mode_four_object_colors(
                [0; 24],
                ObjectDarkShaderInput {
                    reference_translation: [i32::MIN, 0, 0],
                    object_translation: [i32::MAX, 0, 0],
                    dark_distance: 1,
                },
            ),
            Err(RetailDarkShaderError::ArithmeticOutOfRange(
                "initial x difference"
            ))
        );
    }
}
