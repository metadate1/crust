//! Pure projection and shading of validated SVTX/CVTX object models.
//!
//! This module consumes the pointer-free [`ObjectModelFrame`] produced by the
//! formats crate. It does not resolve NSF entries or allocate textures: each
//! surviving polygon retains its validated [`ObjectMaterial`] for a later
//! browser/backend stage.

use core::fmt;

use crust_formats::binary::FormatError;
use crust_formats::stream::structs::ColorInfo;
use crust_formats::stream::{ObjectMaterial, ObjectModelFrame, ObjectVertex, ObjectVertexKind};

use crate::command::ScreenPoint;
use crate::projection::{Matrix3, Vec3i, object_rotation_matrix, project};
use crate::texture::Rgba8;

const MAX_COLORED_SHIFT: u8 = 8;
const ORDERING_DEPTH_MAX: i64 = 0x7ff;
const TRIG_Q: u32 = 48;
const TRIG_HALF: i128 = 1_i128 << (TRIG_Q - 1);
const HALF_PI_Q48: i128 = 442_139_859_501_778;

/// Final fixed-point transform applied to every local model vertex.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectProjectionTransform {
    /// Camera/object/asset matrix, including the retail Y aspect adjustment
    /// and camera-space Z flip.
    pub matrix: Matrix3,
    /// Object origin already transformed into camera space.
    pub translation: Vec3i,
}

impl ObjectProjectionTransform {
    /// Builds the final transform from retail GOOL/TGEO values.
    ///
    /// `rotation_yxz` preserves GOOL register order. `camera_translation` is
    /// the object origin already translated relative to the camera and rotated
    /// by the camera's adjusted translation matrix; translation has a distinct
    /// source path and is therefore not inferred from `camera_matrix`.
    #[must_use]
    pub fn from_retail(
        camera_matrix: Matrix3,
        rotation_yxz: [i32; 3],
        object_scale: Vec3i,
        geometry_scale: [i32; 3],
        camera_translation: Vec3i,
    ) -> Self {
        Self {
            matrix: object_model_matrix(camera_matrix, rotation_yxz, object_scale, geometry_scale),
            translation: camera_translation,
        }
    }
}

/// Builds the exact camera × object-YXY × object/TGEO-scale matrix.
///
/// The helper also applies the source renderer's multiply-before-shift `-5/8`
/// Y adjustment and camera-space Z negation. It lets platform code supply raw
/// GOOL rotations and scales without reproducing private trigonometry.
#[must_use]
pub fn object_model_matrix(
    camera_matrix: Matrix3,
    rotation_yxz: [i32; 3],
    object_scale: Vec3i,
    geometry_scale: [i32; 3],
) -> Matrix3 {
    let local_rotation = yxy_rotation_matrix([
        angle12(rotation_yxz[1].wrapping_sub(rotation_yxz[2])),
        angle12(rotation_yxz[0]),
        angle12(rotation_yxz[2]),
    ]);
    object_rotation_matrix(
        camera_matrix,
        local_rotation,
        object_scale,
        Vec3i {
            x: geometry_scale[0],
            y: geometry_scale[1],
            z: geometry_scale[2],
        },
    )
}

/// Retail projection, culling, and ordering-table inputs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectProjectionParameters {
    /// Logical projection center, normally `[0, 0]` for gameplay.
    pub screen_offset: [i32; 2],
    /// GTE H/projection distance.
    pub projection_distance: u32,
    /// Object-specific ordering-table far value.
    pub ordering_far: u32,
    /// Source `scale.x`; only its sign participates in winding culling.
    pub cull_face: i32,
    /// CVTX zone-color right-shift, constrained to the retail range `0..=8`.
    pub colored_shift: u8,
}

/// Exact GOOL object-color snapshot plus the transform needed to orient it.
///
/// `words` preserves the 24 serialized halfwords in retail order: nine light
/// matrix coefficients, ambient RGB, nine color matrix coefficients, then
/// intensity RGB. GOOL stores rotations as `(y, x, z)`; the lighting path
/// derives the source's inverse YXY matrix and negative-X-scale reflection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GoolObjectLighting {
    pub words: [u16; 24],
    pub rotation_yxz: [i32; 3],
    pub scale_x: i32,
}

/// One projected vertex with its final source-compatible RGBA color.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectedObjectVertex {
    pub position: ScreenPoint,
    pub color: Rgba8,
}

/// One visible TGEO polygon after transform, culling, shading, and OT depth.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectedObjectPolygon {
    /// Polygon index used as [`crate::command::CommandSource::Object::part`].
    pub source_part: u16,
    /// Original TGEO material; texture resolution remains a later operation.
    pub material: ObjectMaterial,
    /// Serialized vertex order is retained exactly.
    pub vertices: [ProjectedObjectVertex; 3],
    /// Clamped 11-bit ordering-table bucket.
    pub ordering_depth: u16,
    /// TGEO flat-shading bit. It is meaningful for SVTX models.
    pub flat_shaded: bool,
    /// True only for a CVTX no-cull polygon accepted on its reverse side.
    pub back_facing: bool,
}

/// Visible polygons and deterministic skip diagnostics for one model frame.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProjectedObjectModel {
    pub polygons: Vec<ProjectedObjectPolygon>,
    pub skipped_saturated: u32,
    pub skipped_culled: u32,
}

/// Failure before a model can be projected safely.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObjectProjectionError {
    ColoredShiftOutOfRange(u8),
    SourcePartOutOfRange(usize),
    InvalidModel(FormatError),
    VertexKindMismatch,
}

impl fmt::Display for ObjectProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ColoredShiftOutOfRange(shift) => {
                write!(formatter, "CVTX color shift {shift} is outside 0..=8")
            }
            Self::SourcePartOutOfRange(part) => {
                write!(
                    formatter,
                    "object polygon index {part} exceeds the 16-bit source part"
                )
            }
            Self::InvalidModel(error) => write!(formatter, "invalid object model: {error}"),
            Self::VertexKindMismatch => {
                formatter.write_str("object frame vertex payload does not match its declared kind")
            }
        }
    }
}

impl std::error::Error for ObjectProjectionError {}

impl From<FormatError> for ObjectProjectionError {
    fn from(error: FormatError) -> Self {
        Self::InvalidModel(error)
    }
}

/// Project every visible polygon of a validated object-model frame.
///
/// A polygon is skipped as a unit when any vertex trips a GTE transform,
/// quotient, depth, or screen saturation flag. Winding, no-cull behavior,
/// shading, and the different SVTX/CVTX depth formulas then match the source
/// software renderer.
///
/// # Errors
///
/// Returns an error for a CVTX color shift outside the retail range, a model
/// with more polygons than the renderer's 16-bit source-part field, or an
/// inconsistent model that bypassed format validation.
pub fn project_object_model(
    model: &ObjectModelFrame,
    transform: ObjectProjectionTransform,
    parameters: ObjectProjectionParameters,
    lighting: Option<GoolObjectLighting>,
) -> Result<ProjectedObjectModel, ObjectProjectionError> {
    if parameters.colored_shift > MAX_COLORED_SHIFT {
        return Err(ObjectProjectionError::ColoredShiftOutOfRange(
            parameters.colored_shift,
        ));
    }

    let resolved_lighting = lighting.map(resolve_lighting);
    let mut result = ProjectedObjectModel {
        polygons: Vec::with_capacity(model.geometry.polygons.len()),
        ..ProjectedObjectModel::default()
    };

    for (polygon_index, polygon) in model.geometry.polygons.iter().copied().enumerate() {
        let source_part = u16::try_from(polygon_index)
            .map_err(|_| ObjectProjectionError::SourcePartOutOfRange(polygon_index))?;
        let [offset_a, offset_b, offset_c] = polygon.vertex_offsets;
        let source_vertices = [
            model.frame.vertex_at_offset(offset_a)?,
            model.frame.vertex_at_offset(offset_b)?,
            model.frame.vertex_at_offset(offset_c)?,
        ];
        let local_positions = [
            model.frame.local_position(offset_a)?,
            model.frame.local_position(offset_b)?,
            model.frame.local_position(offset_c)?,
        ];

        let mut projected = [ScreenPoint::default(); 3];
        let mut saturated = false;
        for (index, local) in local_positions.into_iter().enumerate() {
            let projection = project(
                Vec3i {
                    x: local[0],
                    y: local[1],
                    z: local[2],
                },
                transform.translation,
                transform.matrix,
                parameters.screen_offset,
                parameters.projection_distance,
            );
            if !projection.valid {
                saturated = true;
                break;
            }
            projected[index] = projection.screen;
        }
        if saturated {
            result.skipped_saturated = result.skipped_saturated.saturating_add(1);
            continue;
        }

        let material = model.geometry.material_for_polygon(polygon)?;
        let color_info = material_color(material);
        let rejected_winding = winding_rejected(projected, parameters.cull_face);
        let mut back_facing = false;
        match model.frame.kind {
            ObjectVertexKind::Lit => {
                if rejected_winding && !color_info.no_cull() {
                    result.skipped_culled = result.skipped_culled.saturating_add(1);
                    continue;
                }
            }
            ObjectVertexKind::Colored => {
                if rejected_winding {
                    if !color_info.no_cull() {
                        result.skipped_culled = result.skipped_culled.saturating_add(1);
                        continue;
                    }
                    back_facing = true;
                }
            }
        }

        let colors = shade_vertices(
            model.frame.kind,
            source_vertices,
            color_info,
            polygon.flat_shaded,
            parameters.colored_shift,
            resolved_lighting,
        )?;
        let vertices = core::array::from_fn(|index| ProjectedObjectVertex {
            position: projected[index],
            color: colors[index],
        });
        let ordering_depth = object_ordering_depth(
            model.frame.kind,
            projected,
            parameters.ordering_far,
            back_facing,
        );
        result.polygons.push(ProjectedObjectPolygon {
            source_part,
            material,
            vertices,
            ordering_depth,
            flat_shaded: polygon.flat_shaded,
            back_facing,
        });
    }

    Ok(result)
}

fn material_color(material: ObjectMaterial) -> ColorInfo {
    match material {
        ObjectMaterial::Color(color) | ObjectMaterial::Texture { color, .. } => color,
    }
}

fn winding_rejected(points: [ScreenPoint; 3], cull_face: i32) -> bool {
    let ndot = points[0].x * points[1].y + points[1].x * points[2].y + points[2].x * points[0].y
        - points[0].x * points[2].y
        - points[1].x * points[0].y
        - points[2].x * points[1].y;
    ndot == 0 || (ndot ^ cull_face).is_negative()
}

fn object_ordering_depth(
    kind: ObjectVertexKind,
    points: [ScreenPoint; 3],
    ordering_far: u32,
    back_facing: bool,
) -> u16 {
    let zs = points.map(|point| point.z);
    let depth_term = match kind {
        ObjectVertexKind::Lit => {
            let minimum = zs.into_iter().min().unwrap_or_default();
            let maximum = zs.into_iter().max().unwrap_or_default();
            (3_i64 * (i64::from(minimum) + i64::from(maximum))) / 2
        }
        ObjectVertexKind::Colored => zs.into_iter().map(i64::from).sum(),
    };
    let backface_adjustment = i64::from(u8::from(back_facing)) * 12;
    let depth = i64::from(ordering_far) - (depth_term / 32).saturating_add(backface_adjustment);
    u16::try_from(depth.clamp(0, ORDERING_DEPTH_MAX)).unwrap_or_default()
}

fn shade_vertices(
    kind: ObjectVertexKind,
    vertices: [ObjectVertex; 3],
    material: ColorInfo,
    flat_shaded: bool,
    colored_shift: u8,
    lighting: Option<ResolvedLighting>,
) -> Result<[Rgba8; 3], ObjectProjectionError> {
    let material_color = rgba(material.red(), material.green(), material.blue());
    match kind {
        ObjectVertexKind::Lit => {
            if flat_shaded || lighting.is_none() {
                return Ok([material_color; 3]);
            }
            let lighting = lighting.expect("checked above");
            let [a, b, c] = vertices;
            Ok([
                shade_lit_object_vertex(a, material, lighting)?,
                shade_lit_object_vertex(b, material, lighting)?,
                shade_lit_object_vertex(c, material, lighting)?,
            ])
        }
        ObjectVertexKind::Colored => {
            let [a, b, c] = vertices;
            Ok([
                shade_colored_object_vertex(a, material, colored_shift)?,
                shade_colored_object_vertex(b, material, colored_shift)?,
                shade_colored_object_vertex(c, material, colored_shift)?,
            ])
        }
    }
}

fn shade_lit_object_vertex(
    vertex: ObjectVertex,
    material: ColorInfo,
    lighting: ResolvedLighting,
) -> Result<Rgba8, ObjectProjectionError> {
    match vertex {
        ObjectVertex::Lit(vertex) => Ok(shade_lit_vertex(vertex.normal, material, lighting)),
        ObjectVertex::Colored(_) => Err(ObjectProjectionError::VertexKindMismatch),
    }
}

fn shade_colored_object_vertex(
    vertex: ObjectVertex,
    material: ColorInfo,
    colored_shift: u8,
) -> Result<Rgba8, ObjectProjectionError> {
    match vertex {
        ObjectVertex::Colored(vertex) => Ok(rgba(
            transform_colored_channel(vertex.color[0], material.red(), colored_shift),
            transform_colored_channel(vertex.color[1], material.green(), colored_shift),
            transform_colored_channel(vertex.color[2], material.blue(), colored_shift),
        )),
        ObjectVertex::Lit(_) => Err(ObjectProjectionError::VertexKindMismatch),
    }
}

fn transform_colored_channel(vertex: u8, material: u8, shift: u8) -> u8 {
    let signed = i32::from(material.cast_signed());
    let magnitude = signed.abs();
    let mixed = if signed >= 0 {
        magnitude * i32::from(vertex)
    } else {
        (128 - magnitude) * 255 + (magnitude - 1) * i32::from(vertex)
    };
    let scaled = (i64::from(mixed) * 32) >> (u32::from(shift) + 12);
    u8::try_from(scaled).unwrap_or_else(|_| if scaled.is_negative() { 0 } else { u8::MAX })
}

fn shade_lit_vertex(normal: [i8; 3], material: ColorInfo, lighting: ResolvedLighting) -> Rgba8 {
    let normal = normal.map(|component| i32::from(component) * 256);
    let mut direction = [0_i32; 3];
    for (row, output) in direction.iter_mut().enumerate() {
        let dot = (0..3).fold(0_i64, |sum, column| {
            sum + i64::from(lighting.light.values[row][column]) * i64::from(normal[column])
        });
        *output = clamp_i64_to_i32(dot >> 12).clamp(0, 0x7fff);
    }

    let material = [material.red(), material.green(), material.blue()];
    let mut output = [0_u8; 3];
    for row in 0..3 {
        let illuminated = (0..3).fold(i64::from(lighting.back_color[row]) << 12, |sum, column| {
            sum + i64::from(lighting.color.values[row][column]) * i64::from(direction[column])
        }) >> 12;
        let illuminated = illuminated.clamp(i64::from(i16::MIN), i64::from(i16::MAX));
        let modulated = (i64::from(material[row]) * 16 * illuminated) >> 12;
        let modulated = modulated.clamp(i64::from(i16::MIN), i64::from(i16::MAX));
        output[row] = u8::try_from((modulated >> 4).clamp(0, 255)).unwrap_or_default();
    }
    rgba(output[0], output[1], output[2])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResolvedLighting {
    light: Matrix3,
    color: Matrix3,
    back_color: [u8; 3],
}

fn resolve_lighting(input: GoolObjectLighting) -> ResolvedLighting {
    let mut light = matrix_from_words(&input.words[0..9]);
    if input.scale_x < 0 {
        for row in &mut light.values {
            row[0] = row[0].wrapping_neg();
        }
    }

    // `ang` is laid out as y,x,z in memory, although the matrix routine reads
    // its named x/y/z members. Reproduce the source assignments explicitly.
    let rotation_y = input.rotation_yxz[0];
    let rotation_x = input.rotation_yxz[1];
    let rotation_z = input.rotation_yxz[2];
    let local_x = if input.scale_x >= 0 {
        rotation_x.wrapping_sub(rotation_z)
    } else {
        rotation_x.wrapping_neg().wrapping_sub(rotation_z)
    };
    let local_rotation = yxy_rotation_matrix([
        angle12(local_x.wrapping_neg()),
        angle12(rotation_y.wrapping_neg()),
        angle12(rotation_z.wrapping_neg()),
    ]);
    light = light.multiply(transpose(local_rotation));

    let color = transpose(matrix_from_words(&input.words[12..21]));
    let back_color = core::array::from_fn(|channel| {
        let product = u32::from(input.words[9 + channel]) * u32::from(input.words[21 + channel]);
        // The source stores the shifted result directly in an eight-bit field.
        ((product >> 8) & 0xff) as u8
    });
    ResolvedLighting {
        light,
        color,
        back_color,
    }
}

fn matrix_from_words(words: &[u16]) -> Matrix3 {
    Matrix3 {
        values: core::array::from_fn(|row| {
            core::array::from_fn(|column| words[row * 3 + column].cast_signed())
        }),
    }
}

fn transpose(matrix: Matrix3) -> Matrix3 {
    Matrix3 {
        values: core::array::from_fn(|row| {
            core::array::from_fn(|column| matrix.values[column][row])
        }),
    }
}

fn yxy_rotation_matrix(rotation_xyz: [u16; 3]) -> Matrix3 {
    let sx = sine_q12(rotation_xyz[0]);
    let sy = sine_q12(rotation_xyz[1]);
    let sz = sine_q12(rotation_xyz[2]);
    let cx = cosine_q12(rotation_xyz[0]);
    let cy = cosine_q12(rotation_xyz[1]);
    let cz = cosine_q12(rotation_xyz[2]);
    let sxsy = multiply_q12(sx, sy);
    let sxsz = multiply_q12(sx, sz);
    let sysz = multiply_q12(sy, sz);
    let cxsy = multiply_q12(cx, sy);
    let cxsz = multiply_q12(cx, sz);
    let sxcy = multiply_q12(sx, cy);
    let sxcz = multiply_q12(sx, cz);
    let sycz = multiply_q12(sy, cz);
    let cxcz = multiply_q12(cx, cz);
    let sxcysz = multiply_q12(sxcy, sz);
    let sxcycz = multiply_q12(sxcy, cz);
    let cxcysz = multiply_q12(cxsz, cy);
    let cxcycz = multiply_q12(cxcz, cy);
    Matrix3 {
        values: [
            [cxcz.wrapping_sub(sxcysz), sysz, sxcz.wrapping_add(cxcysz)],
            [sxsy, cy, cxsy.wrapping_neg()],
            [
                cxsz.wrapping_neg().wrapping_sub(sxcycz),
                sycz,
                sxsz.wrapping_neg().wrapping_add(cxcycz),
            ],
        ],
    }
}

fn multiply_q12(left: i16, right: i16) -> i16 {
    i16::try_from((i32::from(left) * i32::from(right)) >> 12)
        .expect("a product of two Q12 i16 coefficients still fits i16")
}

fn angle12(value: i32) -> u16 {
    u16::try_from(value.rem_euclid(0x1000)).expect("a reduced angle is in 0..4096")
}

fn sine_q12(angle: u16) -> i16 {
    let angle = angle & 0x0fff;
    let (quarter_index, sign) = match angle {
        0x000..=0x3ff => (angle, 1_i32),
        0x400..=0x7ff => (0x800 - angle, 1),
        0x800..=0xbff => (angle - 0x800, -1),
        _ => (0x1000 - angle, -1),
    };
    i16::try_from(i32::from(quarter_sine_q12(quarter_index)) * sign)
        .expect("signed Q12 sine fits i16")
}

fn cosine_q12(angle: u16) -> i16 {
    sine_q12(angle.wrapping_add(0x400) & 0x0fff)
}

fn quarter_sine_q12(index: u16) -> i16 {
    let x = (HALF_PI_Q48 * i128::from(index) + 512) / 1024;
    let x_squared = q48_multiply(x, x);
    let mut term = x;
    let mut sum = x;
    let mut subtract = true;
    for term_index in 1_i128..=8 {
        let divisor = (term_index * 2) * (term_index * 2 + 1);
        term = q48_multiply(term, x_squared) / divisor;
        if subtract {
            sum -= term;
        } else {
            sum += term;
        }
        subtract = !subtract;
    }
    i16::try_from((sum * 4096 + TRIG_HALF) >> TRIG_Q).expect("quarter-wave Q12 sine fits i16")
}

fn q48_multiply(left: i128, right: i128) -> i128 {
    (left * right + TRIG_HALF) >> TRIG_Q
}

fn rgba(r: u8, g: u8, b: u8) -> Rgba8 {
    Rgba8 {
        r,
        g,
        b,
        a: u8::MAX,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crust_formats::binary::Eid;
    use crust_formats::stream::{
        ObjectGeometryHeader, ObjectVertexKind, parse_object_frame, parse_object_geometry,
    };
    use proptest::prelude::*;

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn model(
        kind: ObjectVertexKind,
        color_word: u32,
        reversed: bool,
        flat: bool,
    ) -> ObjectModelFrame {
        let geometry_eid = Eid::from_name("geom1").unwrap();
        let mut geometry_header = vec![0_u8; ObjectGeometryHeader::BYTE_LEN + 4];
        put_u32(&mut geometry_header, 0, 1);
        for offset in [4, 8, 12] {
            put_u32(&mut geometry_header, offset, 0x1000);
        }
        put_u32(&mut geometry_header, 16, 1);
        put_u32(&mut geometry_header, 20, color_word);
        let mut polygon = [0_u8; 8];
        for (index, value) in if reversed { [0, 12, 6] } else { [0, 6, 12] }
            .into_iter()
            .enumerate()
        {
            put_u16(&mut polygon, index * 2, value);
        }
        put_u16(&mut polygon, 6, u16::from(flat) << 15);
        let geometry = parse_object_geometry(&geometry_header, &polygon).unwrap();

        let mut frame = vec![0_u8; 56 + 18 + 2];
        put_u32(&mut frame, 0, 3);
        put_u32(&mut frame, 4, geometry_eid.raw());
        let positions = [[100, 100, 128], [156, 100, 128], [128, 156, 128]];
        for (index, position) in positions.into_iter().enumerate() {
            let offset = 56 + index * 6;
            frame[offset..offset + 3].copy_from_slice(&position);
            match kind {
                ObjectVertexKind::Lit => frame[offset + 3..offset + 6].copy_from_slice(&[0, 0, 16]),
                ObjectVertexKind::Colored => {
                    frame[offset + 3..offset + 6].copy_from_slice(&[
                        200 - u8::try_from(index * 10).unwrap(),
                        100 + u8::try_from(index * 10).unwrap(),
                        50 + u8::try_from(index * 10).unwrap(),
                    ]);
                }
            }
        }
        let frame = parse_object_frame(&frame, kind).unwrap();
        ObjectModelFrame::validated(Eid::from_name("vert1").unwrap(), 0, frame, geometry).unwrap()
    }

    fn parameters() -> ObjectProjectionParameters {
        ObjectProjectionParameters {
            screen_offset: [0, 0],
            projection_distance: 500,
            ordering_far: 1000,
            cull_face: 1,
            colored_shift: 0,
        }
    }

    fn transform(z: i32) -> ObjectProjectionTransform {
        ObjectProjectionTransform {
            matrix: Matrix3::IDENTITY,
            translation: Vec3i { x: 0, y: 0, z },
        }
    }

    #[test]
    fn public_transform_builder_consumes_raw_yxz_rotation_and_tgeo_scale() {
        let translation = Vec3i { x: 7, y: 8, z: 9 };
        let transform = ObjectProjectionTransform::from_retail(
            Matrix3::IDENTITY,
            [0x400, 0, 0],
            Vec3i {
                x: 0x1000,
                y: 0x1000,
                z: 0x1000,
            },
            [0x1000; 3],
            translation,
        );
        assert_eq!(
            transform.matrix.values,
            [[4096, 0, 0], [0, 0, 2560], [0, -4096, 0]]
        );
        assert_eq!(transform.translation, translation);
    }

    #[test]
    fn flat_svtx_projection_matches_fixed_golden() {
        let model = model(ObjectVertexKind::Lit, 0x0060_4020, false, true);
        let result = project_object_model(&model, transform(1000), parameters(), None).unwrap();
        assert_eq!(result.skipped_saturated, 0);
        assert_eq!(result.skipped_culled, 0);
        assert_eq!(result.polygons.len(), 1);
        let polygon = result.polygons[0];
        assert_eq!(polygon.source_part, 0);
        assert_eq!(polygon.ordering_depth, 907);
        assert!(!polygon.back_facing);
        assert_eq!(
            polygon.vertices.map(|vertex| vertex.position),
            [
                ScreenPoint {
                    x: -56,
                    y: -56,
                    z: 1000
                },
                ScreenPoint {
                    x: 56,
                    y: -56,
                    z: 1000
                },
                ScreenPoint {
                    x: 0,
                    y: 56,
                    z: 1000
                },
            ]
        );
        assert_eq!(
            polygon.vertices.map(|vertex| vertex.color),
            [rgba(32, 64, 96); 3]
        );
    }

    #[test]
    fn lit_svtx_uses_exact_24_word_identity_lighting() {
        let model = model(ObjectVertexKind::Lit, 0x0060_4020, false, false);
        let mut words = [0_u16; 24];
        for index in [0, 4, 8, 12, 16, 20] {
            words[index] = 0x1000;
        }
        let result = project_object_model(
            &model,
            transform(1000),
            parameters(),
            Some(GoolObjectLighting {
                words,
                rotation_yxz: [0, 0, 0],
                scale_x: 0x1000,
            }),
        )
        .unwrap();
        assert_eq!(
            result.polygons[0].vertices.map(|vertex| vertex.color),
            [rgba(0, 0, 96); 3]
        );

        let rotated = resolve_lighting(GoolObjectLighting {
            words,
            rotation_yxz: [0x400, 0, 0],
            scale_x: 0x1000,
        });
        assert_eq!(
            rotated.light.values,
            [[4096, 0, 0], [0, 0, -4096], [0, 4096, 0]]
        );
        let reflected = resolve_lighting(GoolObjectLighting {
            words,
            rotation_yxz: [0, 0, 0],
            scale_x: -0x1000,
        });
        assert_eq!(
            reflected.light.values,
            [[-4096, 0, 0], [0, 4096, 0], [0, 0, 4096]]
        );
    }

    #[test]
    fn cvtx_signed_color_transform_and_no_cull_depth_match_golden() {
        let color = u32::from_le_bytes([64, 192, 127, 0x10]);
        let model = model(ObjectVertexKind::Colored, color, true, false);
        let result = project_object_model(&model, transform(1000), parameters(), None).unwrap();
        let polygon = result.polygons[0];
        assert!(polygon.back_facing);
        assert_eq!(polygon.ordering_depth, 895);
        assert_eq!(
            polygon.vertices.map(|vertex| vertex.color),
            [rgba(100, 176, 49), rgba(90, 186, 69), rgba(95, 181, 59)]
        );
    }

    #[test]
    fn saturation_culling_and_bad_shift_are_reported_without_partial_polygons() {
        let flat_model = model(ObjectVertexKind::Lit, 0x0060_4020, false, true);
        let saturated =
            project_object_model(&flat_model, transform(100), parameters(), None).unwrap();
        assert!(saturated.polygons.is_empty());
        assert_eq!(saturated.skipped_saturated, 1);

        let reversed = model(ObjectVertexKind::Lit, 0x0060_4020, true, true);
        let culled = project_object_model(&reversed, transform(1000), parameters(), None).unwrap();
        assert!(culled.polygons.is_empty());
        assert_eq!(culled.skipped_culled, 1);

        let error = project_object_model(
            &flat_model,
            transform(1000),
            ObjectProjectionParameters {
                colored_shift: 9,
                ..parameters()
            },
            None,
        )
        .unwrap_err();
        assert_eq!(error, ObjectProjectionError::ColoredShiftOutOfRange(9));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn arbitrary_fixed_transform_inputs_never_panic(
            values in proptest::array::uniform9(any::<i16>()),
            translation in proptest::array::uniform3(any::<i32>()),
            projection_distance in any::<u16>(),
            ordering_far in any::<u16>(),
            cull_face in any::<i32>(),
            colored_shift in 0_u8..=8,
        ) {
            let model = model(ObjectVertexKind::Colored, 0x1060_4020, false, false);
            let matrix = Matrix3 { values: [
                [values[0], values[1], values[2]],
                [values[3], values[4], values[5]],
                [values[6], values[7], values[8]],
            ] };
            let result = project_object_model(
                &model,
                ObjectProjectionTransform {
                    matrix,
                    translation: Vec3i {
                        x: translation[0],
                        y: translation[1],
                        z: translation[2],
                    },
                },
                ObjectProjectionParameters {
                    screen_offset: [0, 0],
                    projection_distance: u32::from(projection_distance),
                    ordering_far: u32::from(ordering_far),
                    cull_face,
                    colored_shift,
                },
                None,
            ).unwrap();
            prop_assert!(result.polygons.iter().all(|polygon| polygon.ordering_depth <= 0x7ff));
        }
    }
}
