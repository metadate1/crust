//! Safe fixed-point projection for retail GOOL sprites and fragments.
//!
//! The source runtime uses one ZXY matrix path for type-two sprites,
//! type-five fragments, text glyphs, and the special 2D CVTX status bit. This
//! module keeps that transform pointer-free and makes the two distinct GTE
//! validity rules explicit: sprites require all four corners, while fragments
//! and glyphs test only the first three corners before projecting the fourth.

use core::fmt;

use crate::command::ScreenPoint;
use crate::projection::{Matrix3, Vec3i, project, rotate, sprite_rotation_matrix};
use crate::rotation::{angle12, zxy_rotation_matrix};

const MAX_EFFECTIVE_SHIFT: u8 = 31;
const ORDERING_DEPTH_MAX: i64 = 0x7ff;

/// GOOL transform vectors consumed by the retail sprite matrix path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailSpriteVectors {
    pub translation: [i32; 3],
    /// Serialized/runtime `ang` order: Y, X, Z.
    pub rotation_yxz: [i32; 3],
    pub scale: [i32; 3],
}

/// Camera values selected by the caller (`cam_prev` or `cam`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailSpriteCamera {
    pub translation: [i32; 3],
    /// Serialized/runtime `ang` order: Y, X, Z.
    pub rotation_yxz: [i32; 3],
    /// Source `ms_rot`/`ms_cam_rot`, including the 5/8 Y adjustment and Z
    /// flip used for camera-relative sprite translation.
    pub matrix: Matrix3,
}

/// Final matrix and camera-space origin used by RTPS.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailSpriteTransform {
    pub matrix: Matrix3,
    pub translation: Vec3i,
}

impl RetailSpriteTransform {
    /// Builds the `GOOL_FLAG_2D` path. Its translation is already in logical
    /// screen space and Z is the projection distance.
    ///
    /// # Errors
    ///
    /// Rejects a shift outside the defined signed-integer range or a
    /// projection distance which cannot fit retail's signed transform word.
    pub fn screen_2d(
        vectors: RetailSpriteVectors,
        shrink: u8,
        projection_distance: u32,
    ) -> Result<Self, RetailSpriteError> {
        validate_shrink(shrink)?;
        let projection = i32::try_from(projection_distance)
            .map_err(|_| RetailSpriteError::ProjectionOutOfRange(projection_distance))?;
        Ok(Self {
            matrix: sprite_matrix(vectors.rotation_yxz, vectors.scale, shrink),
            translation: Vec3i {
                x: vectors.translation[0] >> 8,
                y: vectors.translation[1].wrapping_neg() >> 8,
                z: projection,
            },
        })
    }

    /// Builds the camera-relative path used by world-space sprites/fragments.
    /// Camera X/Y are sign-extended as 11-bit values and clamped to ±170
    /// before subtraction, exactly as `SwCalcSpriteRotMatrix` does.
    ///
    /// # Errors
    ///
    /// Rejects a shift outside the defined signed-integer range.
    pub fn world(
        vectors: RetailSpriteVectors,
        camera: RetailSpriteCamera,
        shrink: u8,
    ) -> Result<Self, RetailSpriteError> {
        validate_shrink(shrink)?;
        let relative = Vec3i {
            x: vectors.translation[0].wrapping_sub(camera.translation[0]) >> 8,
            y: vectors.translation[1].wrapping_sub(camera.translation[1]) >> 8,
            z: vectors.translation[2].wrapping_sub(camera.translation[2]) >> 8,
        };
        let camera_y = sign_extend(camera.rotation_yxz[0], 11).clamp(-170, 170);
        let camera_x = sign_extend(camera.rotation_yxz[1], 11).clamp(-170, 170);
        let camera_z = sign_extend(camera.rotation_yxz[2], 12);
        let rotation_yxz = [
            vectors.rotation_yxz[0].wrapping_sub(camera_y),
            vectors.rotation_yxz[1].wrapping_sub(camera_x),
            vectors.rotation_yxz[2].wrapping_sub(camera_z),
        ];
        Ok(Self {
            matrix: sprite_matrix(rotation_yxz, vectors.scale, shrink),
            translation: rotate(relative, camera.matrix).point,
        })
    }
}

/// Four projected source-order corners and their clamped 11-bit OT bucket.
/// Corner order is lower-left, lower-right, upper-left, upper-right, matching
/// `poly4i` and the texture UV map rather than geometric winding labels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectedSpriteQuad {
    pub vertices: [ScreenPoint; 4],
    pub ordering_depth: u16,
}

/// Derives retail's effective exponential sprite-shrink shift from signed
/// scale X.
///
/// Native computes `abs(scale_x) / 27279`, then consumes that value only as a
/// MIPS variable-shift count. `SRAV` and `SLLV` use its low five bits. Authored
/// GOOL relies on this when projection scratch makes the raw count exceed 31.
///
/// # Errors
///
/// Rejects `i32::MIN`, whose C `abs` has no representable signed result.
pub fn retail_sprite_shrink(scale_x: i32) -> Result<u8, RetailSpriteError> {
    let magnitude = scale_x
        .checked_abs()
        .ok_or(RetailSpriteError::ScaleOutOfRange(scale_x))?;
    let raw = magnitude.cast_unsigned() / 27_279;
    u8::try_from(raw & u32::from(MAX_EFFECTIVE_SHIFT))
        .map_err(|_| RetailSpriteError::ScaleOutOfRange(scale_x))
}

/// Reproduces one PS1 variable left shift without relying on C signed-overflow
/// behavior.
///
/// # Errors
///
/// Rejects counts outside the effective five-bit MIPS shift range.
pub fn retail_sprite_shift_word(value: i32, shrink: u8) -> Result<i32, RetailSpriteError> {
    validate_shrink(shrink)?;
    Ok(value.wrapping_shl(u32::from(shrink)))
}

/// Reproduces the PS1 `200 << shrink` sprite half-size calculation without
/// relying on C signed-overflow behavior.
///
/// Authored GOOL can transiently select a shift whose mathematical result is
/// larger than `i32::MAX`. The retail MIPS instruction retains the low 32
/// bits, and the following GTE path either projects that signed value or
/// rejects the saturated quad. Treating the same value as a scene-construction
/// error pauses the whole browser even though retail only skips that sprite.
///
/// # Errors
///
/// Rejects shifts outside the validated range used by the matching sprite
/// matrix calculation.
pub fn retail_sprite_half_size(shrink: u8) -> Result<i32, RetailSpriteError> {
    retail_sprite_shift_word(200, shrink)
}

/// Projects the source's `[-size, size]` sprite square. All four RTPS results
/// must be valid, matching the accumulated RTPT+RTPS flag test.
#[must_use]
pub fn project_retail_sprite(
    transform: RetailSpriteTransform,
    half_size: i32,
    projection_distance: u32,
    ordering_far: u32,
) -> Option<ProjectedSpriteQuad> {
    let negative = half_size.checked_neg()?;
    let bounds = [negative, negative, half_size, half_size];
    let (vertices, z_sum) = project_quad(
        transform,
        bounds,
        projection_distance,
        ValidityPolicy::AllFour,
    )?;
    Some(ProjectedSpriteQuad {
        vertices,
        ordering_depth: ordering_depth(i64::from(ordering_far), z_sum),
    })
}

/// Projects one source fragment rectangle. Only the first three RTPS results
/// participate in the rejection flag; the fourth saturated result is retained
/// exactly as the source's unchecked final RTPS does.
#[must_use]
pub fn project_retail_fragment(
    transform: RetailSpriteTransform,
    bounds: [i32; 4],
    projection_distance: u32,
    object_size: i32,
) -> Option<ProjectedSpriteQuad> {
    let (vertices, z_sum) = project_quad(
        transform,
        bounds,
        projection_distance,
        ValidityPolicy::FirstThree,
    )?;
    let ordering_far = i64::from(object_size)
        .saturating_add(0x800)
        .saturating_sub(i64::from(projection_distance / 2));
    Some(ProjectedSpriteQuad {
        vertices,
        ordering_depth: ordering_depth(ordering_far, z_sum),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ValidityPolicy {
    AllFour,
    FirstThree,
}

fn project_quad(
    transform: RetailSpriteTransform,
    bounds: [i32; 4],
    projection_distance: u32,
    policy: ValidityPolicy,
) -> Option<([ScreenPoint; 4], i64)> {
    let [x1, y1, x2, y2] = bounds;
    let local = [
        Vec3i { x: x1, y: y2, z: 0 },
        Vec3i { x: x2, y: y2, z: 0 },
        Vec3i { x: x1, y: y1, z: 0 },
        Vec3i { x: x2, y: y1, z: 0 },
    ];
    let projected = local.map(|point| {
        project(
            point,
            transform.translation,
            transform.matrix,
            [0, 0],
            projection_distance,
        )
    });
    let checked = match policy {
        ValidityPolicy::AllFour => projected.as_slice(),
        ValidityPolicy::FirstThree => &projected[..3],
    };
    if checked.iter().any(|result| !result.valid) {
        return None;
    }
    let vertices = projected.map(|result| result.screen);
    let z_sum = vertices[..3]
        .iter()
        .fold(0_i64, |sum, vertex| sum + i64::from(vertex.z));
    Some((vertices, z_sum))
}

fn sprite_matrix(rotation_yxz: [i32; 3], scale: [i32; 3], shrink: u8) -> Matrix3 {
    let local = zxy_rotation_matrix([
        angle12(rotation_yxz[1]),
        angle12(rotation_yxz[0]),
        angle12(rotation_yxz[2]),
    ]);
    sprite_rotation_matrix(
        local,
        Vec3i {
            x: scale[0],
            y: scale[1],
            z: scale[2],
        },
        shrink,
    )
}

fn sign_extend(value: i32, bits: u8) -> i32 {
    debug_assert!((1..32).contains(&bits));
    let shift = 32 - u32::from(bits);
    (value << shift) >> shift
}

fn ordering_depth(ordering_far: i64, z_sum: i64) -> u16 {
    u16::try_from(
        ordering_far
            .saturating_sub(z_sum / 32)
            .clamp(0, ORDERING_DEPTH_MAX),
    )
    .expect("an 11-bit clamped ordering depth fits u16")
}

fn validate_shrink(shrink: u8) -> Result<(), RetailSpriteError> {
    if shrink > MAX_EFFECTIVE_SHIFT {
        Err(RetailSpriteError::ShrinkOutOfRange(shrink))
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetailSpriteError {
    ShrinkOutOfRange(u8),
    ScaleOutOfRange(i32),
    ProjectionOutOfRange(u32),
}

impl fmt::Display for RetailSpriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ShrinkOutOfRange(value) => {
                write!(formatter, "retail sprite shift {value} exceeds 31")
            }
            Self::ScaleOutOfRange(value) => {
                write!(
                    formatter,
                    "retail sprite scale {value} has no signed magnitude"
                )
            }
            Self::ProjectionOutOfRange(value) => {
                write!(
                    formatter,
                    "projection distance {value} exceeds signed transform space"
                )
            }
        }
    }
}

impl std::error::Error for RetailSpriteError {}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn vectors() -> RetailSpriteVectors {
        RetailSpriteVectors {
            translation: [0, 0, 0],
            rotation_yxz: [0, 0, 0],
            scale: [0x1000; 3],
        }
    }

    #[test]
    fn screen_sprite_matches_fixed_identity_characterization() {
        let transform = RetailSpriteTransform::screen_2d(vectors(), 0, 500).unwrap();
        assert_eq!(transform.translation, Vec3i { x: 0, y: 0, z: 500 });
        assert_eq!(
            transform.matrix.values,
            [[4096, 0, 0], [0, -2560, 0], [0, 0, 0]]
        );
        let projected = project_retail_sprite(transform, 200, 500, 1798).unwrap();
        assert_eq!(
            projected.vertices,
            [
                ScreenPoint {
                    x: -200,
                    y: -125,
                    z: 500
                },
                ScreenPoint {
                    x: 200,
                    y: -125,
                    z: 500
                },
                ScreenPoint {
                    x: -200,
                    y: 125,
                    z: 500
                },
                ScreenPoint {
                    x: 200,
                    y: 125,
                    z: 500
                },
            ]
        );
        assert_eq!(projected.ordering_depth, 1752);
    }

    #[test]
    fn world_path_clamps_signed_camera_pitch_and_yaw() {
        let transform = RetailSpriteTransform::world(
            RetailSpriteVectors {
                translation: [0x100, -0x200, 0x300],
                rotation_yxz: [200, -200, 0x20],
                scale: [0x1000; 3],
            },
            RetailSpriteCamera {
                translation: [0; 3],
                rotation_yxz: [0x7ff, 0x401, 0xff0],
                matrix: Matrix3::IDENTITY,
            },
            0,
        )
        .unwrap();
        assert_eq!(transform.translation, Vec3i { x: 1, y: -2, z: 3 });
        // 0x7ff sign-extends to -1; 0x401 sign-extends below -170 and clamps;
        // 0xff0 is -16.
        assert_eq!(
            transform.matrix,
            sprite_matrix([201, -30, 0x30], [0x1000; 3], 0)
        );
    }

    #[test]
    fn fragment_keeps_unchecked_fourth_saturated_corner() {
        let transform = RetailSpriteTransform {
            matrix: Matrix3 {
                values: [[4096, 4096, 0], [0, 0, 0], [0, 0, 0]],
            },
            translation: Vec3i { x: 0, y: 0, z: 500 },
        };
        let fragment = project_retail_fragment(transform, [0, 600, 600, 0], 500, 0).unwrap();
        assert_eq!(fragment.vertices[3].x, 0x3ff);
        assert!(project_quad(transform, [0, 600, 600, 0], 500, ValidityPolicy::AllFour).is_none());
    }

    #[test]
    fn oversized_and_minimum_scale_shifts_are_controlled() {
        assert_eq!(retail_sprite_shrink(0), Ok(0));
        assert_eq!(retail_sprite_shrink(27_279), Ok(1));
        assert_eq!(
            retail_sprite_shrink(i32::MIN),
            Err(RetailSpriteError::ScaleOutOfRange(i32::MIN))
        );
        assert_eq!(
            RetailSpriteTransform::screen_2d(vectors(), 32, 500),
            Err(RetailSpriteError::ShrinkOutOfRange(32))
        );
        assert!(RetailSpriteTransform::screen_2d(vectors(), 31, 500).is_ok());
    }

    #[test]
    fn large_raw_shift_counts_use_the_mips_low_five_bits() {
        for (raw, effective) in [
            (24_u32, 24_u8),
            (26, 26),
            (28, 28),
            (31, 31),
            (34, 2),
            (246, 22),
            (271, 15),
            (297, 9),
        ] {
            let magnitude = i32::try_from(raw * 27_279).unwrap();
            assert_eq!(retail_sprite_shrink(-magnitude), Ok(effective));
        }
    }

    #[test]
    fn large_sprite_shift_wraps_like_the_retail_mips_word() {
        // A representative negative scale with raw shift 24 is a valid
        // 32-bit retail word, not a fatal asset-format error.
        let scale = [-655_688, 1_665_544, 1_257];
        let shrink = retail_sprite_shrink(scale[0]).unwrap();
        assert_eq!(shrink, 24);
        assert_eq!(
            retail_sprite_half_size(shrink),
            Ok(0xc800_0000_u32.cast_signed())
        );

        let transform = RetailSpriteTransform::screen_2d(
            RetailSpriteVectors { scale, ..vectors() },
            shrink,
            500,
        )
        .unwrap();
        assert!(
            project_retail_sprite(
                transform,
                retail_sprite_half_size(shrink).unwrap(),
                500,
                1798,
            )
            .is_none(),
            "retail's accumulated GTE validity flag culls the saturated quad"
        );
    }

    #[test]
    fn malformed_sprite_half_size_shift_remains_rejected() {
        assert_eq!(
            retail_sprite_half_size(32),
            Err(RetailSpriteError::ShrinkOutOfRange(32))
        );
        assert_eq!(retail_sprite_half_size(31), Ok(0));
        assert_eq!(retail_sprite_shift_word(1, 31), Ok(i32::MIN));
        assert_eq!(retail_sprite_shift_word(i32::MIN, 1), Ok(0));
        assert_eq!(
            retail_sprite_shift_word(1, 32),
            Err(RetailSpriteError::ShrinkOutOfRange(32))
        );
        // A representable retail shift can still yield INT_MIN. The checked
        // corner negation safely omits it instead of reproducing C `-INT_MIN`.
        assert_eq!(retail_sprite_half_size(28), Ok(i32::MIN));
        assert!(
            project_retail_sprite(
                RetailSpriteTransform::screen_2d(vectors(), 28, 500).unwrap(),
                i32::MIN,
                500,
                1798,
            )
            .is_none()
        );
    }

    proptest! {
        #[test]
        fn arbitrary_screen_sprite_inputs_never_escape_bounded_outputs(
            translation in any::<[i32; 3]>(),
            rotation in any::<[i32; 3]>(),
            scale in any::<[i32; 3]>(),
            shrink in any::<u8>(),
            projection in 1_u32..=2_000,
            half_size in any::<i32>(),
            ordering_far in any::<u32>(),
            bounds in any::<[i16; 4]>(),
        ) {
            let vectors = RetailSpriteVectors {
                translation,
                rotation_yxz: rotation,
                scale,
            };
            if let Ok(transform) = RetailSpriteTransform::screen_2d(vectors, shrink, projection) {
                if let Some(sprite) = project_retail_sprite(
                    transform,
                    half_size,
                    projection,
                    ordering_far,
                ) {
                    prop_assert!(sprite.ordering_depth <= 0x7ff);
                    for vertex in sprite.vertices {
                        prop_assert!((-0x400..=0x3ff).contains(&vertex.x));
                        prop_assert!((-0x400..=0x3ff).contains(&vertex.y));
                        prop_assert!((0..=0xffff).contains(&vertex.z));
                    }
                }
                if let Some(fragment) = project_retail_fragment(
                    transform,
                    bounds.map(i32::from),
                    projection,
                    i32::from(bounds[0]),
                ) {
                    prop_assert!(fragment.ordering_depth <= 0x7ff);
                    for vertex in fragment.vertices {
                        prop_assert!((-0x400..=0x3ff).contains(&vertex.x));
                        prop_assert!((-0x400..=0x3ff).contains(&vertex.y));
                        prop_assert!((0..=0xffff).contains(&vertex.z));
                    }
                }
            }
        }
    }
}
