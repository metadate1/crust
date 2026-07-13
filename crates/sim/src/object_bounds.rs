//! Retail object-animation bounds and ordered per-frame bound snapshots.
//!
//! The original engine derives collision bounds from the current SVTX/CVTX
//! frame, then appends a 28-byte `{ AABB, object pointer }` record while it
//! walks the 96-object pool. This module preserves the arithmetic and ordering
//! without native pointers or C overflow behavior.

use core::fmt;

use crate::math::{Angle12, Angles, Bounds3, Vec3};

/// Maximum number of object bounds in one retail frame.
///
/// The retail addresses leave `0xA80` bytes between the bounds array and its
/// count. At a 28-byte PS1 record stride that is exactly 96 records, matching
/// the object-pool capacity. Several C declarations incorrectly say 28.
pub const MAX_FRAME_BOUNDS: usize = 96;

const Q12_ONE: i32 = 0x1000;
const NON_VERTEX_HALF_EXTENT: i32 = 200;

/// Animation information needed by the local/world bound calculations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnimationBoundSource {
    /// A type-one GOOL animation resolved to one SVTX/CVTX frame.
    Vertex {
        /// Six serialized frame-bound words, before object scaling.
        serialized_bound: Bounds3,
        /// Frame collision-center offset used by rendering and collision.
        collision_center: Vec3,
    },
    /// Sprite, font, text, fragment, or another non-vertex animation.
    NonVertex,
}

/// Transform fields consumed while converting a local bound to world space.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BoundTransform {
    pub translation: Vec3,
    pub rotation: Angles,
    pub scale: Vec3,
}

/// Calculates the local object AABB using the retail 32-bit operations.
///
/// Only X scale is made positive. Negative Y/Z scale is deliberately allowed
/// to invert serialized corners, as it does in the original executable.
#[must_use]
pub const fn calculate_local_bound(
    source: AnimationBoundSource,
    scale: Vec3,
    is_crash: bool,
) -> Bounds3 {
    let effective_scale = Vec3 {
        x: scale.x.wrapping_abs(),
        y: scale.y,
        z: scale.z,
    };
    match source {
        AnimationBoundSource::Vertex {
            serialized_bound,
            collision_center,
        } => {
            let adjusted = if is_crash {
                Bounds3 {
                    min: serialized_bound.min.wrapping_add(collision_center),
                    max: serialized_bound.max.wrapping_add(collision_center),
                }
            } else {
                serialized_bound
            };
            Bounds3 {
                min: scale_local_corner(adjusted.min, effective_scale),
                max: scale_local_corner(adjusted.max, effective_scale),
            }
        }
        AnimationBoundSource::NonVertex => {
            let extent = Vec3 {
                x: effective_scale.x.wrapping_mul(NON_VERTEX_HALF_EXTENT) >> 4,
                y: effective_scale.y.wrapping_mul(NON_VERTEX_HALF_EXTENT) >> 4,
                z: effective_scale.z.wrapping_mul(NON_VERTEX_HALF_EXTENT) >> 4,
            };
            Bounds3 {
                min: Vec3 {
                    x: extent.x.wrapping_neg(),
                    y: extent.y.wrapping_neg(),
                    z: extent.z.wrapping_neg(),
                },
                max: extent,
            }
        }
    }
}

const fn scale_local_corner(corner: Vec3, scale: Vec3) -> Vec3 {
    Vec3 {
        x: (corner.x >> 8).wrapping_mul(scale.x) >> 4,
        y: (corner.y >> 8).wrapping_mul(scale.y) >> 4,
        z: (corner.z >> 8).wrapping_mul(scale.z) >> 4,
    }
}

/// Calculates the ordered frame AABB for the current animation frame.
///
/// Vertex animations rotate their collision center with the exact retail YXY
/// transform, then approximate the local box orientation using four yaw
/// sectors. Other animation types only translate their local box.
#[must_use]
pub fn calculate_world_bound(
    local_bound: Bounds3,
    source: AnimationBoundSource,
    transform: BoundTransform,
) -> Bounds3 {
    let AnimationBoundSource::Vertex {
        collision_center, ..
    } = source
    else {
        return local_bound.translated(transform.translation);
    };

    let yaw = transform.rotation.x.raw();
    let center = if yaw == 0 && transform.scale.x == Q12_ONE {
        transform.translation.wrapping_add(collision_center)
    } else {
        retail_yxy_transform(collision_center, transform)
    };
    orient_vertex_bound(local_bound, center, yaw)
}

/// Applies the fixed-point YXY transform used by retail `GoolTransform`.
///
/// Input and output points are Q16, scale and matrix values are Q12, and Euler
/// angles use the engine's unusual `(y, x, z)` storage order. Matrix
/// coefficients retain their retail signed-halfword truncation between stages.
#[must_use]
pub fn retail_yxy_transform(point: Vec3, transform: BoundTransform) -> Vec3 {
    let first_y = rotation_y(transform.rotation.z);
    let middle_x = rotation_x(transform.rotation.y);
    let mut matrix = multiply_rotation_matrices(first_y, middle_x);
    let final_y = rotation_y(Angle12::new(
        i32::from(transform.rotation.x.raw()).wrapping_sub(i32::from(transform.rotation.z.raw())),
    ));
    matrix = multiply_rotation_matrices(matrix, final_y);
    matrix = scale_matrix_columns(matrix, transform.scale);

    // Retail 0x8002465C uses signed division toward zero here, not an
    // arithmetic right shift for negative inputs as the C transcription says.
    let input = Vec3 {
        x: divide_by_16_toward_zero(point.x),
        y: divide_by_16_toward_zero(point.y),
        z: divide_by_16_toward_zero(point.z),
    };
    let rotated = transform_by_matrix(input, matrix);
    Vec3 {
        x: rotated
            .x
            .wrapping_shl(4)
            .wrapping_add(transform.translation.x),
        y: rotated
            .y
            .wrapping_shl(4)
            .wrapping_add(transform.translation.y),
        z: rotated
            .z
            .wrapping_shl(4)
            .wrapping_add(transform.translation.z),
    }
}

type Matrix3 = [[i16; 3]; 3];

fn rotation_y(angle: Angle12) -> Matrix3 {
    let sine = angle.sin_q12();
    let cosine = angle.cos_q12();
    [
        [cosine, 0, sine],
        [0, Q12_ONE as i16, 0],
        [sine.wrapping_neg(), 0, cosine],
    ]
}

fn rotation_x(angle: Angle12) -> Matrix3 {
    let sine = angle.sin_q12();
    let cosine = angle.cos_q12();
    [
        [Q12_ONE as i16, 0, 0],
        [0, cosine, sine.wrapping_neg()],
        [0, sine, cosine],
    ]
}

fn multiply_rotation_matrices(left: Matrix3, right: Matrix3) -> Matrix3 {
    let mut result = [[0_i16; 3]; 3];
    for row in 0..3 {
        for column in 0..3 {
            let sum = i64::from(left[row][0]) * i64::from(right[0][column])
                + i64::from(left[row][1]) * i64::from(right[1][column])
                + i64::from(left[row][2]) * i64::from(right[2][column]);
            // PS1 MulMatrix uses signed GTE IR output (LM=0), which saturates
            // the shifted result to one signed halfword.
            result[row][column] =
                (sum >> 12).clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16;
        }
    }
    result
}

fn scale_matrix_columns(mut matrix: Matrix3, scale: Vec3) -> Matrix3 {
    let scale = [scale.x, scale.y, scale.z];
    for row in &mut matrix {
        for (coefficient, axis_scale) in row.iter_mut().zip(scale) {
            *coefficient = (i32::from(*coefficient).wrapping_mul(axis_scale) >> 12) as i16;
        }
    }
    matrix
}

fn transform_by_matrix(point: Vec3, matrix: Matrix3) -> Vec3 {
    fn axis(row: [i16; 3], point: Vec3) -> i32 {
        i32::from(row[0])
            .wrapping_mul(point.x)
            .wrapping_add(i32::from(row[1]).wrapping_mul(point.y))
            .wrapping_add(i32::from(row[2]).wrapping_mul(point.z))
            >> 12
    }
    Vec3 {
        x: axis(matrix[0], point),
        y: axis(matrix[1], point),
        z: axis(matrix[2], point),
    }
}

const fn divide_by_16_toward_zero(value: i32) -> i32 {
    if value < 0 {
        value.wrapping_add(15) >> 4
    } else {
        value >> 4
    }
}

fn orient_vertex_bound(local: Bounds3, center: Vec3, yaw: u16) -> Bounds3 {
    match yaw {
        0x000..=0x1ff | 0xe01..=0xfff => Bounds3 {
            min: center.wrapping_add(local.min),
            max: center.wrapping_add(local.max),
        },
        0x200..=0x5ff => Bounds3 {
            min: Vec3 {
                x: center.x.wrapping_add(local.min.z),
                y: center.y.wrapping_add(local.min.y),
                z: center.z.wrapping_sub(local.max.x),
            },
            max: Vec3 {
                x: center.x.wrapping_add(local.max.z),
                y: center.y.wrapping_add(local.max.y),
                z: center.z.wrapping_sub(local.min.x),
            },
        },
        0x600..=0x9ff => Bounds3 {
            min: Vec3 {
                x: center.x.wrapping_add(local.min.x),
                y: center.y.wrapping_add(local.min.y),
                z: center.z.wrapping_sub(local.max.z),
            },
            max: Vec3 {
                x: center.x.wrapping_add(local.max.x),
                y: center.y.wrapping_add(local.max.y),
                z: center.z.wrapping_sub(local.min.z),
            },
        },
        0xa00..=0xe00 => Bounds3 {
            min: Vec3 {
                x: center.x.wrapping_sub(local.max.z),
                y: center.y.wrapping_add(local.min.y),
                z: center.z.wrapping_add(local.min.x),
            },
            max: Vec3 {
                x: center.x.wrapping_sub(local.min.z),
                y: center.y.wrapping_add(local.max.y),
                z: center.z.wrapping_add(local.max.x),
            },
        },
        _ => unreachable!("yaw is a twelve-bit angle"),
    }
}

/// Retail `TestPointInBound`: lower faces included, upper faces excluded.
#[must_use]
pub const fn point_in_bound(point: Vec3, bound: Bounds3) -> bool {
    point.x >= bound.min.x
        && point.y >= bound.min.y
        && point.z >= bound.min.z
        && point.x < bound.max.x
        && point.y < bound.max.y
        && point.z < bound.max.z
}

/// Retail `TestBoundIntersection`, including its directional face rules.
///
/// `candidate` is the source function's second argument. Its maximum faces are
/// inclusive against `tested.min`, while its minimum faces are exclusive
/// against `tested.max`; swapping the arguments can therefore change a result.
#[must_use]
pub const fn bounds_intersect_asymmetric(tested: Bounds3, candidate: Bounds3) -> bool {
    candidate.max.y >= tested.min.y
        && candidate.min.y < tested.max.y
        && candidate.min.x < tested.max.x
        && candidate.min.z < tested.max.z
        && candidate.max.x >= tested.min.x
        && candidate.max.z >= tested.min.z
}

/// One AABB snapshot in exact frame insertion order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameBound<H> {
    pub bound: Bounds3,
    pub object: H,
}

/// A bounded replacement for retail's global `object_bounds` array.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameBounds<H> {
    entries: Vec<FrameBound<H>>,
}

impl<H> FrameBounds<H> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::with_capacity(MAX_FRAME_BOUNDS),
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn as_slice(&self) -> &[FrameBound<H>] {
        &self.entries
    }

    pub fn iter(&self) -> core::slice::Iter<'_, FrameBound<H>> {
        self.entries.iter()
    }

    pub fn push(&mut self, bound: FrameBound<H>) -> Result<(), FrameBoundsError> {
        if self.entries.len() == MAX_FRAME_BOUNDS {
            return Err(FrameBoundsError::CapacityExceeded);
        }
        self.entries.push(bound);
        Ok(())
    }

    /// Clears snapshots without reallocating the retail-sized host buffer.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

impl<H> Default for FrameBounds<H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a, H> IntoIterator for &'a FrameBounds<H> {
    type Item = &'a FrameBound<H>;
    type IntoIter = core::slice::Iter<'a, FrameBound<H>>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

/// Failure to append a malformed ninety-seventh per-frame object bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameBoundsError {
    CapacityExceeded,
}

impl fmt::Display for FrameBoundsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CapacityExceeded => formatter.write_str("retail frame-bound capacity exceeded"),
        }
    }
}

impl std::error::Error for FrameBoundsError {}

#[cfg(test)]
mod tests {
    use super::*;

    const fn vector(x: i32, y: i32, z: i32) -> Vec3 {
        Vec3 { x, y, z }
    }

    const fn bounds(min: Vec3, max: Vec3) -> Bounds3 {
        Bounds3 { min, max }
    }

    const fn vertex_source(bound: Bounds3, collision_center: Vec3) -> AnimationBoundSource {
        AnimationBoundSource::Vertex {
            serialized_bound: bound,
            collision_center,
        }
    }

    fn transform_with_yaw(yaw: i32) -> BoundTransform {
        BoundTransform {
            translation: vector(100, 200, 300),
            rotation: Angles {
                y: Angle12::new(0),
                x: Angle12::new(yaw),
                z: Angle12::new(0),
            },
            scale: vector(Q12_ONE, Q12_ONE, Q12_ONE),
        }
    }

    #[test]
    fn vertex_local_bound_applies_crash_center_and_only_absolutizes_x_scale() {
        let serialized = bounds(vector(-256, -512, -768), vector(256, 512, 768));
        let center = vector(256, 256, 256);
        let scale = vector(-4096, 2048, -4096);

        assert_eq!(
            calculate_local_bound(vertex_source(serialized, center), scale, true),
            bounds(vector(0, -128, 512), vector(512, 384, -1024))
        );
        assert_eq!(
            calculate_local_bound(vertex_source(serialized, center), scale, false),
            bounds(vector(-256, -256, 768), vector(256, 256, -768))
        );
    }

    #[test]
    fn non_vertex_cube_preserves_negative_y_and_z_corner_inversion() {
        assert_eq!(
            calculate_local_bound(
                AnimationBoundSource::NonVertex,
                vector(-4096, -2048, 1024),
                false,
            ),
            bounds(
                vector(-51_200, 25_600, -12_800),
                vector(51_200, -25_600, 12_800),
            )
        );
    }

    #[test]
    fn local_bound_uses_wrapping_abs_and_wrapping_products() {
        let source = vertex_source(bounds(vector(256, 0, 0), vector(512, 0, 0)), Vec3::ZERO);
        assert_eq!(
            calculate_local_bound(source, vector(i32::MIN, 0, 0), false),
            bounds(vector(i32::MIN >> 4, 0, 0), vector(0, 0, 0),)
        );
        assert_eq!(
            calculate_local_bound(
                AnimationBoundSource::NonVertex,
                vector(i32::MIN, 0, 0),
                false,
            )
            .max
            .x,
            0
        );
    }

    #[test]
    fn yxy_transform_truncates_negative_q16_input_toward_zero() {
        let transformed = retail_yxy_transform(
            vector(-31, 31, -17),
            BoundTransform {
                translation: vector(5, -5, 7),
                rotation: Angles::default(),
                scale: vector(Q12_ONE, Q12_ONE, Q12_ONE),
            },
        );
        assert_eq!(transformed, vector(-11, 11, -9));
    }

    #[test]
    fn yxy_transform_matches_cardinal_yaw_and_pitch() {
        let scale = vector(Q12_ONE, Q12_ONE, Q12_ONE);
        let yawed = retail_yxy_transform(
            vector(16, 0, 0),
            BoundTransform {
                translation: Vec3::ZERO,
                rotation: Angles {
                    y: Angle12::new(0),
                    x: Angle12::new(0x400),
                    z: Angle12::new(0),
                },
                scale,
            },
        );
        assert_eq!(yawed, vector(0, 0, -16));

        let pitched = retail_yxy_transform(
            vector(0, 16, 0),
            BoundTransform {
                translation: Vec3::ZERO,
                rotation: Angles {
                    y: Angle12::new(0x400),
                    x: Angle12::new(0),
                    z: Angle12::new(0),
                },
                scale,
            },
        );
        assert_eq!(pitched, vector(0, 0, 16));
    }

    #[test]
    fn vertex_center_fast_path_ignores_y_and_z_scale() {
        let local = bounds(Vec3::ZERO, Vec3::ZERO);
        let source = vertex_source(local, vector(11, 22, 33));
        let world = calculate_world_bound(
            local,
            source,
            BoundTransform {
                translation: vector(100, 200, 300),
                rotation: Angles::default(),
                scale: vector(Q12_ONE, -123, 456),
            },
        );
        assert_eq!(world, bounds(vector(111, 222, 333), vector(111, 222, 333)));
    }

    #[test]
    fn vertex_center_slow_path_applies_scale_before_world_translation() {
        let local = bounds(Vec3::ZERO, Vec3::ZERO);
        let world = calculate_world_bound(
            local,
            vertex_source(local, vector(160, 320, -480)),
            BoundTransform {
                translation: vector(1, 2, 3),
                rotation: Angles::default(),
                scale: vector(8192, 4096, 2048),
            },
        );
        assert_eq!(
            world,
            bounds(vector(321, 322, -237), vector(321, 322, -237))
        );
    }

    #[test]
    fn yaw_sector_boundaries_match_retail_branches() {
        let local = bounds(vector(1, 2, 3), vector(10, 20, 30));
        let source = vertex_source(local, Vec3::ZERO);
        let direct = bounds(vector(101, 202, 303), vector(110, 220, 330));
        let quarter = bounds(vector(103, 202, 290), vector(130, 220, 299));
        let half = bounds(vector(101, 202, 270), vector(110, 220, 297));
        let three_quarter = bounds(vector(70, 202, 301), vector(97, 220, 310));

        for yaw in [0x000, 0x1ff, 0xe01, 0xfff] {
            assert_eq!(
                calculate_world_bound(local, source, transform_with_yaw(yaw)),
                direct
            );
        }
        for yaw in [0x200, 0x5ff] {
            assert_eq!(
                calculate_world_bound(local, source, transform_with_yaw(yaw)),
                quarter
            );
        }
        for yaw in [0x600, 0x9ff] {
            assert_eq!(
                calculate_world_bound(local, source, transform_with_yaw(yaw)),
                half
            );
        }
        for yaw in [0xa00, 0xe00] {
            assert_eq!(
                calculate_world_bound(local, source, transform_with_yaw(yaw)),
                three_quarter
            );
        }
    }

    #[test]
    fn non_vertex_world_bound_is_translation_only() {
        let local = bounds(vector(-1, -2, -3), vector(10, 20, 30));
        let transform = BoundTransform {
            translation: vector(i32::MAX, 100, i32::MIN),
            rotation: Angles {
                y: Angle12::new(0x321),
                x: Angle12::new(0x654),
                z: Angle12::new(0x987),
            },
            scale: vector(-1, -2, -3),
        };
        assert_eq!(
            calculate_world_bound(local, AnimationBoundSource::NonVertex, transform),
            bounds(
                vector(i32::MAX - 1, 98, i32::MAX - 2),
                vector(i32::MIN + 9, 120, i32::MIN + 30),
            )
        );
    }

    #[test]
    fn point_faces_are_lower_inclusive_and_upper_exclusive() {
        let bound = bounds(Vec3::ZERO, vector(10, 10, 10));
        assert!(point_in_bound(Vec3::ZERO, bound));
        assert!(point_in_bound(vector(9, 9, 9), bound));
        assert!(!point_in_bound(vector(10, 9, 9), bound));
        assert!(!point_in_bound(vector(9, 10, 9), bound));
        assert!(!point_in_bound(vector(9, 9, 10), bound));
    }

    #[test]
    fn bound_face_contact_is_intentionally_asymmetric() {
        let upper = bounds(vector(0, 0, 0), vector(10, 10, 10));
        let lower = bounds(vector(0, -10, 0), vector(10, 0, 10));
        assert!(bounds_intersect_asymmetric(upper, lower));
        assert!(!bounds_intersect_asymmetric(lower, upper));
    }

    #[test]
    fn frame_bounds_retain_insertion_order_clear_and_enforce_retail_capacity() {
        let mut frame = FrameBounds::new();
        for object in 0_u8..MAX_FRAME_BOUNDS as u8 {
            frame
                .push(FrameBound {
                    bound: bounds(vector(i32::from(object), 0, 0), Vec3::ZERO),
                    object,
                })
                .unwrap();
        }
        assert_eq!(frame.len(), MAX_FRAME_BOUNDS);
        assert_eq!(frame.as_slice()[0].object, 0);
        assert_eq!(frame.as_slice()[MAX_FRAME_BOUNDS - 1].object, 95);
        assert_eq!(
            frame.push(FrameBound {
                bound: Bounds3::default(),
                object: 96,
            }),
            Err(FrameBoundsError::CapacityExceeded)
        );

        frame.clear();
        assert!(frame.is_empty());
        frame
            .push(FrameBound {
                bound: Bounds3::default(),
                object: 7,
            })
            .unwrap();
        assert_eq!(
            frame
                .into_iter()
                .map(|entry| entry.object)
                .collect::<Vec<_>>(),
            [7]
        );
    }
}
