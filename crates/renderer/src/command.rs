//! Renderer-neutral scene commands and WebGL2-ready triangle batches.

use core::fmt;
use core::ops::Range;

use crate::cache::TextureHandle;
use crate::projection::{TriangleVisibility, Viewport};
use crate::texture::Rgba8;

/// Number of depth buckets used by the original ordering table.
pub const ORDERING_TABLE_DEPTH: usize = 2048;
/// Conservative default cap for commands retained during one frame.
pub const DEFAULT_COMMAND_CAPACITY: usize = 131_072;

/// C1/PSX semi-transparency selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum BlendMode {
    /// `(background + foreground) / 2` for STP texels.
    Average = 0,
    /// `background + foreground` for STP texels.
    Additive = 1,
    /// `background - foreground` for STP texels.
    Subtractive = 2,
    /// Port convention for ordinary opaque primitives.
    #[default]
    Opaque = 3,
}

impl TryFrom<u8> for BlendMode {
    type Error = InvalidBlendMode;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Average),
            1 => Ok(Self::Additive),
            2 => Ok(Self::Subtractive),
            3 => Ok(Self::Opaque),
            value => Err(InvalidBlendMode(value)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidBlendMode(pub u8);

impl fmt::Display for InvalidBlendMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid PSX blend mode {}", self.0)
    }
}

impl std::error::Error for InvalidBlendMode {}

/// WebGL blend equation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlendEquation {
    Add,
    ReverseSubtract,
}

/// WebGL blend factor used by the fixed pipeline mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlendFactor {
    Zero,
    One,
    SourceAlpha,
    OneMinusSourceAlpha,
}

/// Alpha comparison used to isolate ordinary and STP texels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlphaTest {
    Disabled,
    GreaterThanThreeQuarters,
    LessThanThreeQuarters,
}

/// One ordered backend pass for a primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderPass {
    pub blend_enabled: bool,
    pub equation: BlendEquation,
    pub source_factor: BlendFactor,
    pub destination_factor: BlendFactor,
    pub alpha_test: AlphaTest,
}

/// Resolve PSX blending into one or two adjacent WebGL2 passes.
///
/// Textured subtractive primitives need an opaque pass for ordinary texels and
/// a reverse-subtract pass for STP texels. Keeping those passes adjacent is
/// necessary for ordering-table correctness.
#[must_use]
pub fn render_passes(textured: bool, blend: BlendMode) -> Vec<RenderPass> {
    if textured && blend == BlendMode::Subtractive {
        return vec![
            RenderPass {
                blend_enabled: false,
                equation: BlendEquation::Add,
                source_factor: BlendFactor::One,
                destination_factor: BlendFactor::Zero,
                alpha_test: AlphaTest::GreaterThanThreeQuarters,
            },
            RenderPass {
                blend_enabled: true,
                equation: BlendEquation::ReverseSubtract,
                source_factor: BlendFactor::One,
                destination_factor: BlendFactor::One,
                alpha_test: AlphaTest::LessThanThreeQuarters,
            },
        ];
    }

    let (equation, source_factor, destination_factor) = match blend {
        BlendMode::Average | BlendMode::Opaque => (
            BlendEquation::Add,
            BlendFactor::SourceAlpha,
            BlendFactor::OneMinusSourceAlpha,
        ),
        BlendMode::Additive => (
            BlendEquation::Add,
            BlendFactor::One,
            BlendFactor::OneMinusSourceAlpha,
        ),
        BlendMode::Subtractive => (
            BlendEquation::ReverseSubtract,
            BlendFactor::One,
            BlendFactor::One,
        ),
    };
    vec![RenderPass {
        blend_enabled: true,
        equation,
        source_factor,
        destination_factor,
        alpha_test: AlphaTest::Disabled,
    }]
}

/// Exact legacy alpha word selected before primitive conversion.
#[must_use]
pub const fn primitive_vertex_alpha(textured: bool, blend: BlendMode) -> u32 {
    if textured {
        return u32::MAX;
    }
    match blend {
        BlendMode::Average => 0x7fff_ffff,
        BlendMode::Additive => 0,
        BlendMode::Subtractive | BlendMode::Opaque => u32::MAX,
    }
}

/// Advance a signed shader parameter without stepping past its target.
pub fn shader_step_toward(current: i32, target: i32, step: &mut i32) -> i32 {
    let next = current.saturating_add(*step);
    if (target > current && next >= target) || (target < current && next <= target) {
        *step = 0;
        target
    } else {
        next
    }
}

/// Integer screen-space point. Z is retained for diagnostics and backend use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ScreenPoint {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// Normalized texture coordinate.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Uv {
    pub u: f32,
    pub v: f32,
}

/// Texture coordinate bounds for a sprite.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UvRect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Default for UvRect {
    fn default() -> Self {
        Self {
            left: 0.0,
            top: 0.0,
            right: 1.0,
            bottom: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrimitiveStyle {
    Fill,
    Wireframe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColoredVertex {
    pub position: ScreenPoint,
    pub color: Rgba8,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TexturedVertex {
    pub position: ScreenPoint,
    pub color: Rgba8,
    pub uv: Uv,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColoredTriangle {
    pub vertices: [ColoredVertex; 3],
    pub blend: BlendMode,
    pub style: PrimitiveStyle,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TexturedTriangle {
    pub vertices: [TexturedVertex; 3],
    pub texture: TextureHandle,
    pub blend: BlendMode,
}

/// Vertex order is top-left, top-right, bottom-left, bottom-right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColoredQuad {
    pub vertices: [ColoredVertex; 4],
    pub blend: BlendMode,
    pub style: PrimitiveStyle,
}

/// Vertex order is top-left, top-right, bottom-left, bottom-right.
#[derive(Debug, Clone, PartialEq)]
pub struct TexturedQuad {
    pub vertices: [TexturedVertex; 4],
    pub texture: TextureHandle,
    pub blend: BlendMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpriteCommand {
    pub rect: ScreenRect,
    pub depth: i32,
    pub color: Rgba8,
    pub texture: TextureHandle,
    pub uv: UvRect,
    pub blend: BlendMode,
}

/// Authored scene primitive before quad/sprite conversion.
#[derive(Debug, Clone, PartialEq)]
pub enum PrimitiveCommand {
    ColoredTriangle(ColoredTriangle),
    TexturedTriangle(TexturedTriangle),
    ColoredQuad(ColoredQuad),
    TexturedQuad(TexturedQuad),
    Sprite(SpriteCommand),
}

/// The provenance is retained for debugging and selection tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandSource {
    World { zone: u32, polygon: u32 },
    Object { handle: u32, part: u16 },
    Overlay,
}

/// One backend triangle after deterministic quad splitting.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderTriangle {
    pub vertices: [TexturedVertex; 3],
    pub texture: Option<TextureHandle>,
    pub blend: BlendMode,
    pub style: PrimitiveStyle,
    pub source: CommandSource,
    pub ordering_depth: u16,
    pub visibility: TriangleVisibility,
}

impl PrimitiveCommand {
    fn triangles(
        &self,
        source: CommandSource,
        depth: u16,
        viewport: Viewport,
    ) -> Vec<RenderTriangle> {
        match self {
            Self::ColoredTriangle(triangle) => vec![colored_triangle(
                triangle.vertices,
                triangle.blend,
                triangle.style,
                source,
                depth,
                viewport,
            )],
            Self::TexturedTriangle(triangle) => vec![textured_triangle(
                triangle.vertices,
                triangle.texture,
                triangle.blend,
                PrimitiveStyle::Fill,
                source,
                depth,
                viewport,
            )],
            Self::ColoredQuad(quad) => {
                let [a, b, c, d] = quad.vertices;
                vec![
                    colored_triangle([a, b, d], quad.blend, quad.style, source, depth, viewport),
                    colored_triangle([c, d, a], quad.blend, quad.style, source, depth, viewport),
                ]
            }
            Self::TexturedQuad(quad) => {
                let [a, b, c, d] = quad.vertices;
                vec![
                    textured_triangle(
                        [a, b, d],
                        quad.texture,
                        quad.blend,
                        PrimitiveStyle::Fill,
                        source,
                        depth,
                        viewport,
                    ),
                    textured_triangle(
                        [c, d, a],
                        quad.texture,
                        quad.blend,
                        PrimitiveStyle::Fill,
                        source,
                        depth,
                        viewport,
                    ),
                ]
            }
            Self::Sprite(sprite) => {
                let left = sprite.rect.x;
                let right = left.saturating_add(sprite.rect.width);
                let top = sprite.rect.y;
                let bottom = top.saturating_add(sprite.rect.height);
                let vertex = |x, y, u, v| TexturedVertex {
                    position: ScreenPoint {
                        x,
                        y,
                        z: sprite.depth,
                    },
                    color: sprite.color,
                    uv: Uv { u, v },
                };
                let vertices = [
                    vertex(left, top, sprite.uv.left, sprite.uv.top),
                    vertex(right, top, sprite.uv.right, sprite.uv.top),
                    vertex(left, bottom, sprite.uv.left, sprite.uv.bottom),
                    vertex(right, bottom, sprite.uv.right, sprite.uv.bottom),
                ];
                Self::TexturedQuad(TexturedQuad {
                    vertices,
                    texture: sprite.texture,
                    blend: sprite.blend,
                })
                .triangles(source, depth, viewport)
            }
        }
    }
}

fn colored_triangle(
    vertices: [ColoredVertex; 3],
    blend: BlendMode,
    style: PrimitiveStyle,
    source: CommandSource,
    depth: u16,
    viewport: Viewport,
) -> RenderTriangle {
    let vertices = vertices.map(|vertex| TexturedVertex {
        position: vertex.position,
        color: vertex.color,
        uv: Uv::default(),
    });
    let visibility = viewport.classify_triangle(vertices.map(|vertex| vertex.position));
    RenderTriangle {
        vertices,
        texture: None,
        blend,
        style,
        source,
        ordering_depth: depth,
        visibility,
    }
}

fn textured_triangle(
    vertices: [TexturedVertex; 3],
    texture: TextureHandle,
    blend: BlendMode,
    style: PrimitiveStyle,
    source: CommandSource,
    depth: u16,
    viewport: Viewport,
) -> RenderTriangle {
    let visibility = viewport.classify_triangle(vertices.map(|vertex| vertex.position));
    RenderTriangle {
        vertices,
        texture: Some(texture),
        blend,
        style,
        source,
        ordering_depth: depth,
        visibility,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PipelineKey {
    pub texture: Option<TextureHandle>,
    pub blend: BlendMode,
    pub style: PrimitiveStyle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrawBatch {
    pub triangles: Range<usize>,
    pub pipeline: PipelineKey,
}

/// Interleaved data that can be uploaded without retaining engine pointers.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct GpuVertex {
    pub position: [f32; 3],
    pub color: [f32; 4],
    pub uv: [f32; 2],
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandDiagnostics {
    pub submitted_commands: u64,
    pub rejected_commands: u64,
    pub generated_triangles: u64,
    pub touching_clip_triangles: u64,
    pub trivially_outside_triangles: u64,
    pub largest_triangle_area2: u32,
    pub largest_triangle_index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeneratedFrame {
    pub triangles: Vec<RenderTriangle>,
    pub vertices: Vec<GpuVertex>,
    pub batches: Vec<DrawBatch>,
    pub diagnostics: CommandDiagnostics,
}

#[derive(Debug, Clone)]
struct QueuedCommand {
    source: CommandSource,
    primitive: PrimitiveCommand,
}

/// Bounded, pointer-free replacement for the 2048-link C ordering table.
#[derive(Debug)]
pub struct OrderingTable {
    buckets: Vec<Vec<QueuedCommand>>,
    command_count: usize,
    max_commands: usize,
    rejected_commands: u64,
}

impl Default for OrderingTable {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_COMMAND_CAPACITY)
    }
}

impl OrderingTable {
    #[must_use]
    pub fn with_capacity(max_commands: usize) -> Self {
        Self {
            buckets: (0..ORDERING_TABLE_DEPTH).map(|_| Vec::new()).collect(),
            command_count: 0,
            max_commands,
            rejected_commands: 0,
        }
    }

    /// Submit a command at an exact 11-bit ordering-table depth.
    ///
    /// # Errors
    ///
    /// Returns an error when the depth is outside the 2048 buckets or the
    /// configured command capacity has been reached.
    pub fn submit(
        &mut self,
        depth: u16,
        source: CommandSource,
        primitive: PrimitiveCommand,
    ) -> Result<(), CommandError> {
        let index = usize::from(depth);
        if index >= ORDERING_TABLE_DEPTH {
            self.rejected_commands = self.rejected_commands.saturating_add(1);
            return Err(CommandError::DepthOutOfRange(depth));
        }
        if self.command_count >= self.max_commands {
            self.rejected_commands = self.rejected_commands.saturating_add(1);
            return Err(CommandError::CapacityExceeded {
                capacity: self.max_commands,
            });
        }
        self.buckets[index].push(QueuedCommand { source, primitive });
        self.command_count += 1;
        Ok(())
    }

    /// Submit world geometry while retaining its zone/polygon provenance.
    ///
    /// # Errors
    ///
    /// Propagates ordering-depth and command-capacity errors from [`Self::submit`].
    pub fn submit_world(
        &mut self,
        depth: u16,
        zone: u32,
        polygon: u32,
        primitive: PrimitiveCommand,
    ) -> Result<(), CommandError> {
        self.submit(depth, CommandSource::World { zone, polygon }, primitive)
    }

    /// Submit object geometry while retaining its handle/part provenance.
    ///
    /// # Errors
    ///
    /// Propagates ordering-depth and command-capacity errors from [`Self::submit`].
    pub fn submit_object(
        &mut self,
        depth: u16,
        handle: u32,
        part: u16,
        primitive: PrimitiveCommand,
    ) -> Result<(), CommandError> {
        self.submit(depth, CommandSource::Object { handle, part }, primitive)
    }

    /// Submit title, menu, or other overlay geometry.
    ///
    /// # Errors
    ///
    /// Propagates ordering-depth and command-capacity errors from [`Self::submit`].
    pub fn submit_overlay(
        &mut self,
        depth: u16,
        primitive: PrimitiveCommand,
    ) -> Result<(), CommandError> {
        self.submit(depth, CommandSource::Overlay, primitive)
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.command_count
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.command_count == 0
    }

    pub fn clear(&mut self) {
        for bucket in &mut self.buckets {
            bucket.clear();
        }
        self.command_count = 0;
        self.rejected_commands = 0;
    }

    /// Generate triangles in ascending ordering-table depth and insertion order.
    #[must_use]
    pub fn generate(&self, viewport: Viewport) -> GeneratedFrame {
        let mut triangles = Vec::new();
        for (depth, bucket) in self.buckets.iter().enumerate() {
            let depth = u16::try_from(depth).unwrap_or_default();
            for command in bucket {
                triangles.extend(command.primitive.triangles(command.source, depth, viewport));
            }
        }

        let mut diagnostics = CommandDiagnostics {
            submitted_commands: u64::try_from(self.command_count).unwrap_or(u64::MAX),
            rejected_commands: self.rejected_commands,
            generated_triangles: u64::try_from(triangles.len()).unwrap_or(u64::MAX),
            ..CommandDiagnostics::default()
        };
        let mut vertices = Vec::with_capacity(triangles.len().saturating_mul(3));
        let mut batches: Vec<DrawBatch> = Vec::new();
        for (index, triangle) in triangles.iter().enumerate() {
            match triangle.visibility {
                TriangleVisibility::Inside => {}
                TriangleVisibility::Intersecting => {
                    diagnostics.touching_clip_triangles =
                        diagnostics.touching_clip_triangles.saturating_add(1);
                }
                TriangleVisibility::Outside => {
                    diagnostics.touching_clip_triangles =
                        diagnostics.touching_clip_triangles.saturating_add(1);
                    diagnostics.trivially_outside_triangles =
                        diagnostics.trivially_outside_triangles.saturating_add(1);
                }
            }
            let area = triangle_area2(triangle.vertices.map(|vertex| vertex.position));
            if diagnostics.largest_triangle_index.is_none()
                || area > diagnostics.largest_triangle_area2
            {
                diagnostics.largest_triangle_area2 = area;
                diagnostics.largest_triangle_index = Some(index);
            }

            let pipeline = PipelineKey {
                texture: triangle.texture,
                blend: triangle.blend,
                style: triangle.style,
            };
            if let Some(batch) = batches
                .last_mut()
                .filter(|batch| batch.pipeline == pipeline)
            {
                batch.triangles.end += 1;
            } else {
                batches.push(DrawBatch {
                    triangles: index..index + 1,
                    pipeline,
                });
            }
            vertices.extend(
                triangle
                    .vertices
                    .iter()
                    .map(|vertex| gpu_vertex(*vertex, viewport)),
            );
        }

        GeneratedFrame {
            triangles,
            vertices,
            batches,
            diagnostics,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
    DepthOutOfRange(u16),
    CapacityExceeded { capacity: usize },
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DepthOutOfRange(depth) => {
                write!(formatter, "ordering-table depth {depth} is outside 0..2048")
            }
            Self::CapacityExceeded { capacity } => {
                write!(formatter, "renderer command capacity {capacity} exceeded")
            }
        }
    }
}

impl std::error::Error for CommandError {}

fn gpu_vertex(vertex: TexturedVertex, viewport: Viewport) -> GpuVertex {
    let [x, y] = viewport.logical_to_ndc(vertex.position);
    GpuVertex {
        // Ordering is resolved on the CPU, exactly as in the C backend. Keep
        // every converted triangle on the WebGL near plane instead of leaking
        // camera-space depth into normalized device coordinates.
        position: [x, y, -1.0],
        color: [
            f32::from(vertex.color.r) / 255.0,
            f32::from(vertex.color.g) / 255.0,
            f32::from(vertex.color.b) / 255.0,
            f32::from(vertex.color.a) / 255.0,
        ],
        uv: [vertex.uv.u, vertex.uv.v],
    }
}

/// Twice a triangle's area, saturated to the legacy diagnostic width.
#[must_use]
pub fn triangle_area2(vertices: [ScreenPoint; 3]) -> u32 {
    let ax = i128::from(vertices[1].x) - i128::from(vertices[0].x);
    let ay = i128::from(vertices[1].y) - i128::from(vertices[0].y);
    let bx = i128::from(vertices[2].x) - i128::from(vertices[0].x);
    let by = i128::from(vertices[2].y) - i128::from(vertices[0].y);
    let magnitude = (ax * by - ay * bx).unsigned_abs();
    u32::try_from(magnitude).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: Rgba8 = Rgba8 {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };

    fn colored_vertex(x: i32, y: i32) -> ColoredVertex {
        ColoredVertex {
            position: ScreenPoint { x, y, z: 10 },
            color: WHITE,
        }
    }

    #[test]
    fn primitive_alpha_matches_legacy_contract() {
        assert_eq!(
            primitive_vertex_alpha(false, BlendMode::Average),
            0x7fff_ffff
        );
        assert_eq!(primitive_vertex_alpha(false, BlendMode::Additive), 0);
        assert_eq!(
            primitive_vertex_alpha(false, BlendMode::Subtractive),
            u32::MAX
        );
        assert_eq!(primitive_vertex_alpha(false, BlendMode::Opaque), u32::MAX);
        for mode in [
            BlendMode::Average,
            BlendMode::Additive,
            BlendMode::Subtractive,
            BlendMode::Opaque,
        ] {
            assert_eq!(primitive_vertex_alpha(true, mode), u32::MAX);
        }
    }

    #[test]
    fn subtractive_textures_generate_adjacent_masked_passes() {
        let passes = render_passes(true, BlendMode::Subtractive);
        assert_eq!(passes.len(), 2);
        assert_eq!(passes[0].alpha_test, AlphaTest::GreaterThanThreeQuarters);
        assert!(!passes[0].blend_enabled);
        assert_eq!(passes[1].alpha_test, AlphaTest::LessThanThreeQuarters);
        assert_eq!(passes[1].equation, BlendEquation::ReverseSubtract);
    }

    #[test]
    fn quad_split_matches_original_diagonal_and_order() {
        let quad = PrimitiveCommand::ColoredQuad(ColoredQuad {
            vertices: [
                colored_vertex(0, 0),
                colored_vertex(10, 0),
                colored_vertex(0, 10),
                colored_vertex(10, 10),
            ],
            blend: BlendMode::Opaque,
            style: PrimitiveStyle::Fill,
        });
        let triangles = quad.triangles(CommandSource::Overlay, 0, Viewport::PSX);
        let positions: Vec<_> = triangles
            .iter()
            .map(|triangle| triangle.vertices.map(|vertex| vertex.position))
            .collect();
        assert_eq!(
            positions[0],
            [
                ScreenPoint { x: 0, y: 0, z: 10 },
                ScreenPoint { x: 10, y: 0, z: 10 },
                ScreenPoint {
                    x: 10,
                    y: 10,
                    z: 10
                },
            ]
        );
        assert_eq!(
            positions[1],
            [
                ScreenPoint { x: 0, y: 10, z: 10 },
                ScreenPoint {
                    x: 10,
                    y: 10,
                    z: 10
                },
                ScreenPoint { x: 0, y: 0, z: 10 },
            ]
        );
    }

    #[test]
    fn ordering_and_contiguous_batching_are_stable() {
        let mut table = OrderingTable::with_capacity(3);
        let make = |x| {
            PrimitiveCommand::ColoredTriangle(ColoredTriangle {
                vertices: [
                    colored_vertex(x, 0),
                    colored_vertex(x + 1, 0),
                    colored_vertex(x, 1),
                ],
                blend: BlendMode::Opaque,
                style: PrimitiveStyle::Fill,
            })
        };
        table.submit_overlay(4, make(40)).unwrap();
        table.submit_world(2, 1, 1, make(20)).unwrap();
        table.submit_object(4, 7, 0, make(41)).unwrap();
        let frame = table.generate(Viewport::PSX);
        assert_eq!(frame.triangles[0].vertices[0].position.x, 20);
        assert_eq!(frame.triangles[1].vertices[0].position.x, 40);
        assert_eq!(frame.triangles[2].vertices[0].position.x, 41);
        assert_eq!(frame.batches.len(), 1);
        assert_eq!(frame.batches[0].triangles, 0..3);
        assert_eq!(frame.vertices.len(), 9);
        assert!(
            frame
                .vertices
                .iter()
                .all(|vertex| (vertex.position[2] + 1.0).abs() <= f32::EPSILON)
        );
    }

    #[test]
    fn command_capacity_and_depth_are_validated() {
        let mut table = OrderingTable::with_capacity(1);
        let primitive = PrimitiveCommand::ColoredTriangle(ColoredTriangle {
            vertices: [
                colored_vertex(0, 0),
                colored_vertex(1, 0),
                colored_vertex(0, 1),
            ],
            blend: BlendMode::Opaque,
            style: PrimitiveStyle::Fill,
        });
        assert!(matches!(
            table.submit_overlay(2048, primitive.clone()),
            Err(CommandError::DepthOutOfRange(2048))
        ));
        table.submit_overlay(0, primitive.clone()).unwrap();
        assert!(matches!(
            table.submit_overlay(0, primitive),
            Err(CommandError::CapacityExceeded { capacity: 1 })
        ));
    }

    #[test]
    fn shader_ramps_reach_targets_without_overshoot() {
        let cases = [
            (4095, -8000, -500, 25),
            (2000, 75, -75, 26),
            (-8000, 4095, 100, 121),
            (75, 2000, 20, 97),
        ];
        for (mut current, target, initial_step, expected_steps) in cases {
            let mut step = initial_step;
            let mut values = Vec::new();
            while step != 0 {
                current = shader_step_toward(current, target, &mut step);
                values.push(current);
                assert!(values.len() < 1000);
            }
            assert_eq!(values.len(), expected_steps);
            assert_eq!(current, target);
            assert_eq!(step, 0);
        }
    }

    #[test]
    fn triangle_area_is_saturated_and_orientation_independent() {
        let triangle = [
            ScreenPoint { x: 0, y: 0, z: 0 },
            ScreenPoint { x: 10, y: 0, z: 0 },
            ScreenPoint { x: 0, y: 20, z: 0 },
        ];
        assert_eq!(triangle_area2(triangle), 200);
        assert_eq!(triangle_area2([triangle[0], triangle[2], triangle[1]]), 200);
        assert_eq!(
            triangle_area2([
                ScreenPoint {
                    x: i32::MIN,
                    y: i32::MIN,
                    z: 0
                },
                ScreenPoint {
                    x: i32::MAX,
                    y: i32::MIN,
                    z: 0
                },
                ScreenPoint {
                    x: i32::MIN,
                    y: i32::MAX,
                    z: 0
                },
            ]),
            u32::MAX
        );
    }
}
