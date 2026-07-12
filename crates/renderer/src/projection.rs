//! Deterministic fixed-point transforms and PSX viewport classification.

use crate::command::{ScreenPoint, ScreenRect};

/// Twelve-bit fixed-point identity value.
pub const FIXED_ONE: i32 = 0x1000;
const SCREEN_MIN: i64 = -0x400;
const SCREEN_MAX: i64 = 0x3ff;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Vec3i {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl From<Vec3i> for ScreenPoint {
    fn from(value: Vec3i) -> Self {
        Self {
            x: value.x,
            y: value.y,
            z: value.z,
        }
    }
}

/// A 3x3 signed 4.12 fixed-point matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Matrix3 {
    pub values: [[i16; 3]; 3],
}

impl Matrix3 {
    pub const IDENTITY: Self = Self {
        values: [[0x1000, 0, 0], [0, 0x1000, 0], [0, 0, 0x1000]],
    };

    #[must_use]
    pub const fn diagonal(x: i16, y: i16, z: i16) -> Self {
        Self {
            values: [[x, 0, 0], [0, y, 0], [0, 0, z]],
        }
    }

    /// Fixed-point matrix multiplication with an arithmetic shift after each
    /// full dot product, matching the software GTE path.
    #[must_use]
    pub fn multiply(self, right: Self) -> Self {
        let mut values = [[0_i16; 3]; 3];
        for (row, output_row) in values.iter_mut().enumerate() {
            for (column, output) in output_row.iter_mut().enumerate() {
                let dot = (0..3).fold(0_i64, |sum, index| {
                    sum + i64::from(self.values[row][index])
                        * i64::from(right.values[index][column])
                });
                *output = wrapping_i16(dot >> 12);
            }
        }
        Self { values }
    }
}

impl Default for Matrix3 {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransformResult {
    pub point: Vec3i,
    /// False when a GTE input/output range would have saturated.
    pub valid: bool,
}

/// Rotate a point and retain the unsaturated intermediate result.
#[must_use]
pub fn rotate(point: Vec3i, matrix: Matrix3) -> TransformResult {
    rotate_translate(point, Vec3i::default(), matrix)
}

/// Rotate and translate using 64-bit multiply-accumulate operations.
#[must_use]
pub fn rotate_translate(point: Vec3i, translation: Vec3i, matrix: Matrix3) -> TransformResult {
    let input = [point.x, point.y, point.z];
    let translations = [translation.x, translation.y, translation.z];
    let mut output = [0_i32; 3];
    for row in 0..3 {
        let accumulator = i64::from(translations[row]) * i64::from(FIXED_ONE)
            + (0..3).fold(0_i64, |sum, column| {
                sum + i64::from(matrix.values[row][column]) * i64::from(input[column])
            });
        output[row] = clamp_i64_to_i32(accumulator >> 12);
    }
    let point = Vec3i {
        x: output[0],
        y: output[1],
        z: output[2],
    };
    let valid = (-0x8000..=0x7fff).contains(&point.x)
        && (-0x8000..=0x7fff).contains(&point.y)
        && (0..=0xffff).contains(&point.z);
    TransformResult { point, valid }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectionResult {
    /// Unsaturated camera-space transform.
    pub camera: Vec3i,
    /// Saturated PSX screen result.
    pub screen: ScreenPoint,
    /// False when transform, depth, quotient, or screen saturation occurred.
    pub valid: bool,
}

/// Emulate the PSX RTPS quotient and screen saturation behavior.
#[must_use]
pub fn project(
    point: Vec3i,
    translation: Vec3i,
    matrix: Matrix3,
    screen_offset: [i32; 2],
    projection_distance: u32,
) -> ProjectionResult {
    let transformed = rotate_translate(point, translation, matrix);
    let camera = transformed.point;
    let z = camera.z.clamp(0, 0xffff);
    let mut saturated = z != camera.z;
    let x = project_axis(
        camera.x,
        z,
        screen_offset[0],
        projection_distance,
        &mut saturated,
    );
    let y = project_axis(
        camera.y,
        z,
        screen_offset[1],
        projection_distance,
        &mut saturated,
    );
    ProjectionResult {
        camera,
        screen: ScreenPoint { x, y, z },
        valid: transformed.valid && !saturated,
    }
}

fn project_axis(value: i32, z: i32, offset: i32, projection: u32, saturated: &mut bool) -> i32 {
    let ir = value.clamp(-0x8000, 0x7fff);
    if ir != value {
        *saturated = true;
    }
    let projected = if u32::try_from(z).unwrap_or(0).saturating_mul(2) <= projection {
        *saturated = true;
        i64::from(offset) + ((i64::from(ir) * 0x1_ffff) >> 16)
    } else {
        let numerator = i64::from(ir) * (i64::from(projection) * 0x1_0000);
        (i64::from(offset) * 0x1_0000 + numerator / i64::from(z)) >> 16
    };
    if !(SCREEN_MIN..=SCREEN_MAX).contains(&projected) {
        *saturated = true;
    }
    clamp_i64_to_i32(projected.clamp(SCREEN_MIN, SCREEN_MAX))
}

/// Build an object matrix while preserving retail multiply-before-shift order.
#[must_use]
pub fn object_rotation_matrix(
    camera: Matrix3,
    local_rotation: Matrix3,
    object_scale: Vec3i,
    asset_scale: Vec3i,
) -> Matrix3 {
    let scaled = [
        fixed_scale_component(object_scale.x, asset_scale.x),
        fixed_scale_component(object_scale.y, asset_scale.y),
        fixed_scale_component(object_scale.z, asset_scale.z),
    ];
    let mut matrix = camera
        .multiply(local_rotation)
        .multiply(Matrix3::diagonal(scaled[0], scaled[1], scaled[2]));
    apply_psx_aspect_and_depth_flip(&mut matrix, true);
    matrix
}

/// Build a screen-aligned sprite matrix from its local rotation and scale.
#[must_use]
pub fn sprite_rotation_matrix(local_rotation: Matrix3, scale: Vec3i, shrink: u8) -> Matrix3 {
    let shift = u32::from(shrink).min(31);
    let diagonal = Matrix3::diagonal(
        wrapping_i16(i64::from(scale.x >> shift)),
        wrapping_i16(i64::from(scale.y >> shift)),
        wrapping_i16(i64::from(scale.z >> shift)),
    );
    let mut matrix = local_rotation.multiply(diagonal);
    apply_psx_aspect_and_depth_flip(&mut matrix, false);
    matrix.values[2] = [0, 0, 0];
    matrix
}

fn fixed_scale_component(object: i32, asset: i32) -> i16 {
    wrapping_i16((i64::from(object) * i64::from(asset)) >> 12)
}

fn apply_psx_aspect_and_depth_flip(matrix: &mut Matrix3, flip_depth: bool) {
    for column in 0..3 {
        let value = i32::from(matrix.values[1][column]);
        matrix.values[1][column] = wrapping_i16(i64::from(-((value * 5) >> 3)));
        if flip_depth {
            matrix.values[2][column] = wrapping_i16(-i64::from(matrix.values[2][column]));
        }
    }
}

// Truncation is the specified two's-complement behavior of the original GTE
// coefficient registers, made explicit here instead of relying on C overflow.
#[allow(clippy::cast_possible_truncation)]
fn wrapping_i16(value: i64) -> i16 {
    value as i16
}

fn clamp_i64_to_i32(value: i64) -> i32 {
    i32::try_from(value).unwrap_or_else(|_| {
        if value.is_negative() {
            i32::MIN
        } else {
            i32::MAX
        }
    })
}

/// Logical PSX viewport, before WebGL's Y-axis inversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Viewport {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Viewport {
    pub const PSX: Self = Self {
        x: -256,
        y: -120,
        width: 512,
        height: 240,
    };

    /// Gameplay draw area after the 12-pixel top/bottom mask.
    pub const GAMEPLAY: Self = Self {
        x: -256,
        y: -108,
        width: 512,
        height: 216,
    };

    #[must_use]
    pub fn clip_flags(self, point: ScreenPoint) -> ClipFlags {
        let right = i64::from(self.x) + i64::from(self.width);
        let bottom = i64::from(self.y) + i64::from(self.height);
        let mut flags = ClipFlags::NONE;
        if point.x < self.x {
            flags |= ClipFlags::LEFT;
        } else if i64::from(point.x) > right {
            flags |= ClipFlags::RIGHT;
        }
        if point.y < self.y {
            flags |= ClipFlags::TOP;
        } else if i64::from(point.y) > bottom {
            flags |= ClipFlags::BOTTOM;
        }
        flags
    }

    #[must_use]
    pub fn classify_triangle(self, points: [ScreenPoint; 3]) -> TriangleVisibility {
        let flags = points.map(|point| self.clip_flags(point));
        let shared = flags[0] & flags[1] & flags[2];
        if !shared.is_empty() {
            TriangleVisibility::Outside
        } else if flags.into_iter().all(ClipFlags::is_empty) {
            TriangleVisibility::Inside
        } else {
            TriangleVisibility::Intersecting
        }
    }

    /// Convert logical coordinates to WebGL normalized device coordinates.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn logical_to_ndc(self, point: ScreenPoint) -> [f32; 2] {
        if self.width == 0 || self.height == 0 {
            return [0.0, 0.0];
        }
        let x = ((f64::from(point.x) - f64::from(self.x)) / f64::from(self.width)) * 2.0 - 1.0;
        // The C backend negates projected Y before issuing OpenGL vertices.
        let y = 1.0 - ((f64::from(point.y) - f64::from(self.y)) / f64::from(self.height)) * 2.0;
        [x as f32, y as f32]
    }

    #[must_use]
    pub fn as_rect(self) -> ScreenRect {
        ScreenRect {
            x: self.x,
            y: self.y,
            width: i32::try_from(self.width).unwrap_or(i32::MAX),
            height: i32::try_from(self.height).unwrap_or(i32::MAX),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ClipFlags(u8);

impl ClipFlags {
    pub const NONE: Self = Self(0);
    pub const LEFT: Self = Self(1 << 0);
    pub const RIGHT: Self = Self(1 << 1);
    pub const TOP: Self = Self(1 << 2);
    pub const BOTTOM: Self = Self(1 << 3);

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }
}

impl core::ops::BitOr for ClipFlags {
    type Output = Self;

    fn bitor(self, right: Self) -> Self::Output {
        Self(self.0 | right.0)
    }
}

impl core::ops::BitOrAssign for ClipFlags {
    fn bitor_assign(&mut self, right: Self) {
        self.0 |= right.0;
    }
}

impl core::ops::BitAnd for ClipFlags {
    type Output = Self;

    fn bitand(self, right: Self) -> Self::Output {
        Self(self.0 & right.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TriangleVisibility {
    Inside,
    Intersecting,
    Outside,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn rolling_stones_object_projection_matches_characterization() {
        let camera = Matrix3 {
            values: [[4029, 0, -737], [-370, 3545, -2019], [637, 2052, 3487]],
        };
        let matrix = object_rotation_matrix(
            camera,
            Matrix3::IDENTITY,
            Vec3i {
                x: 0x1000,
                y: 0x1000,
                z: 0x1000,
            },
            Vec3i {
                x: 7200,
                y: 7200,
                z: 7200,
            },
        );
        assert_eq!(matrix.values[1], [407, -3894, 2219]);
        let projected = project(
            Vec3i {
                x: -48,
                y: 136,
                z: -24,
            },
            Vec3i {
                x: -1129,
                y: -2456,
                z: 6160,
            },
            matrix,
            [0, 0],
            800,
        );
        assert!(projected.valid);
        assert_eq!(
            projected.screen,
            ScreenPoint {
                x: -159,
                y: -343,
                z: 6089
            }
        );
    }

    #[test]
    fn sprite_matrix_multiplies_before_aspect_shift() {
        let matrix = sprite_rotation_matrix(
            Matrix3::IDENTITY,
            Vec3i {
                x: 0x1000,
                y: 0x0fff,
                z: 0x1000,
            },
            0,
        );
        assert_eq!(matrix.values[1][1], -2559);
        assert_eq!(matrix.values[2], [0, 0, 0]);
    }

    #[test]
    fn normal_projection_retains_fixed_rounding() {
        let result = project(
            Vec3i {
                x: -333,
                y: 125,
                z: 1000,
            },
            Vec3i::default(),
            Matrix3::IDENTITY,
            [0, 0],
            460,
        );
        assert!(result.valid);
        assert_eq!(
            result.screen,
            ScreenPoint {
                x: -154,
                y: 57,
                z: 1000
            }
        );
    }

    #[test]
    fn quotient_and_screen_saturation_match_gte() {
        let at_half = project(
            Vec3i {
                x: 100,
                y: -100,
                z: 230,
            },
            Vec3i::default(),
            Matrix3::IDENTITY,
            [0, 0],
            460,
        );
        assert!(!at_half.valid);
        assert_eq!(
            at_half.screen,
            ScreenPoint {
                x: 199,
                y: -200,
                z: 230
            }
        );

        let after_half = project(
            Vec3i {
                x: 100,
                y: -100,
                z: 231,
            },
            Vec3i::default(),
            Matrix3::IDENTITY,
            [0, 0],
            460,
        );
        assert!(after_half.valid);
        assert_eq!(
            after_half.screen,
            ScreenPoint {
                x: 199,
                y: -200,
                z: 231
            }
        );

        let behind = project(
            Vec3i {
                x: 100,
                y: -100,
                z: -100,
            },
            Vec3i::default(),
            Matrix3::IDENTITY,
            [0, 0],
            460,
        );
        assert!(!behind.valid);
        assert_eq!(
            behind.screen,
            ScreenPoint {
                x: 199,
                y: -200,
                z: 0
            }
        );

        let extreme = project(
            Vec3i {
                x: 0x7fff,
                y: -0x8000,
                z: 1,
            },
            Vec3i::default(),
            Matrix3::IDENTITY,
            [0, 0],
            460,
        );
        assert!(!extreme.valid);
        assert_eq!(extreme.screen.x, 0x3ff);
        assert_eq!(extreme.screen.y, -0x400);
    }

    #[test]
    fn translation_mac_is_wide_and_signed() {
        let result = rotate_translate(
            Vec3i {
                x: 100,
                y: -200,
                z: 300,
            },
            Vec3i {
                x: -1_234_567,
                y: 1_234_567,
                z: -400_000,
            },
            Matrix3::IDENTITY,
        );
        assert!(!result.valid);
        assert_eq!(
            result.point,
            Vec3i {
                x: -1_234_467,
                y: 1_234_367,
                z: -399_700
            }
        );
    }

    #[test]
    fn viewport_classifies_trivial_and_partial_clipping() {
        let inside = [
            ScreenPoint { x: 0, y: 0, z: 0 },
            ScreenPoint { x: 10, y: 0, z: 0 },
            ScreenPoint { x: 0, y: 10, z: 0 },
        ];
        assert_eq!(
            Viewport::PSX.classify_triangle(inside),
            TriangleVisibility::Inside
        );
        let intersecting = [
            inside[0],
            inside[1],
            ScreenPoint {
                x: 0,
                y: -121,
                z: 0,
            },
        ];
        assert_eq!(
            Viewport::PSX.classify_triangle(intersecting),
            TriangleVisibility::Intersecting
        );
        let outside = [
            ScreenPoint {
                x: -300,
                y: 0,
                z: 0,
            },
            ScreenPoint {
                x: -400,
                y: 10,
                z: 0,
            },
            ScreenPoint {
                x: -500,
                y: -10,
                z: 0,
            },
        ];
        assert_eq!(
            Viewport::PSX.classify_triangle(outside),
            TriangleVisibility::Outside
        );
        let center = Viewport::PSX.logical_to_ndc(ScreenPoint { x: 0, y: 0, z: 0 });
        assert!(center[0].abs() <= f32::EPSILON);
        assert!(center[1].abs() <= f32::EPSILON);
    }

    proptest! {
        #[test]
        fn projected_coordinates_are_always_gte_bounded(
            x in any::<i32>(), y in any::<i32>(), z in any::<i32>(), projection in any::<u16>()
        ) {
            let result = project(
                Vec3i { x, y, z },
                Vec3i::default(),
                Matrix3::IDENTITY,
                [0, 0],
                u32::from(projection),
            );
            prop_assert!((-0x400..=0x3ff).contains(&result.screen.x));
            prop_assert!((-0x400..=0x3ff).contains(&result.screen.y));
            prop_assert!((0..=0xffff).contains(&result.screen.z));
        }
    }
}
