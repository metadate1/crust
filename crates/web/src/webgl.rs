#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "diagnostic coordinates and colors are clamped before conversion"
)]

use crust_renderer::cache::TextureHandle;
use crust_renderer::command::{
    BlendMode, ColoredQuad, ColoredTriangle, ColoredVertex, OrderingTable, PrimitiveCommand,
    PrimitiveStyle, ScreenPoint, ScreenRect, SpriteCommand, UvRect,
};
use crust_renderer::projection::Viewport;
use crust_renderer::texture::{DecodedTexture, Rgba8};
use wasm_bindgen::JsValue;
use web_sys::HtmlCanvasElement;

use crate::renderer_backend::{RenderOptions, RendererBackend};

const LOADING_IMAGE_HANDLE: TextureHandle = TextureHandle::new(u64::MAX);
const LOADING_IMAGE_FRAMES: u8 = 30;
const LOADING_IMAGE_DEPTH: u16 = 2_047;
const NEUTRAL_TEXTURE_COLOR: Rgba8 = Rgba8 {
    r: 128,
    g: 128,
    b: 128,
    a: 255,
};

#[derive(Clone, Copy, Debug)]
pub struct VisualState {
    pub time: f32,
    pub seed: u32,
    pub player_x: f32,
    pub player_y: f32,
    pub active: bool,
}

#[derive(Debug)]
pub struct GlStage {
    backend: RendererBackend,
    ordering: OrderingTable,
    loading_image_dimensions: Option<[i32; 2]>,
    loading_image_frames: u8,
    last_error: u32,
}

impl GlStage {
    pub fn new(canvas: &HtmlCanvasElement) -> Result<Self, JsValue> {
        let backend = RendererBackend::new(canvas).map_err(|error| backend_error(&error))?;
        Ok(Self {
            backend,
            ordering: OrderingTable::default(),
            loading_image_dimensions: None,
            loading_image_frames: 0,
            last_error: 0,
        })
    }

    /// Upload a decoded retail loading image and display it for the next 30
    /// successful presentation frames.
    pub fn install_loading_image(&mut self, image: &DecodedTexture) -> Result<(), JsValue> {
        let dimensions = [
            i32::try_from(image.width())
                .map_err(|_| JsValue::from_str("loading-image width exceeds WebGL limits"))?,
            i32::try_from(image.height())
                .map_err(|_| JsValue::from_str("loading-image height exceeds WebGL limits"))?,
        ];
        self.backend
            .upload_texture(LOADING_IMAGE_HANDLE, image)
            .map_err(|error| backend_error(&error))?;
        self.loading_image_dimensions = Some(dimensions);
        self.loading_image_frames = LOADING_IMAGE_FRAMES;
        Ok(())
    }

    pub fn render(&mut self, state: VisualState) -> Result<(), JsValue> {
        self.ordering.clear();
        submit_diagnostic_scene(&mut self.ordering, state)
            .map_err(|error| command_error(&error))?;

        if self.loading_image_frames != 0
            && let Some([width, height]) = self.loading_image_dimensions
        {
            let rect = ScreenRect {
                x: -(width / 2),
                y: -(height / 2),
                width,
                height,
            };
            self.ordering
                .submit_overlay(
                    LOADING_IMAGE_DEPTH,
                    PrimitiveCommand::Sprite(SpriteCommand {
                        rect,
                        depth: i32::from(LOADING_IMAGE_DEPTH),
                        color: NEUTRAL_TEXTURE_COLOR,
                        texture: LOADING_IMAGE_HANDLE,
                        uv: UvRect::default(),
                        blend: BlendMode::Opaque,
                    }),
                )
                .map_err(|error| command_error(&error))?;
        }

        let frame = self.ordering.generate(Viewport::PSX);
        let diagnostics = self
            .backend
            .render(
                &frame,
                RenderOptions {
                    clear_color: Some([0.0, 0.0, 0.0, 1.0]),
                    ..RenderOptions::default()
                },
            )
            .map_err(|error| backend_error(&error))?;
        self.last_error = diagnostics
            .preexisting_gl_errors
            .first()
            .or_else(|| diagnostics.gl_errors.first())
            .copied()
            .unwrap_or(0);
        if self.loading_image_frames != 0 {
            self.loading_image_frames -= 1;
        }
        Ok(())
    }

    #[must_use]
    pub fn error(&self) -> u32 {
        if self.last_error == 0 {
            self.backend.next_gl_error()
        } else {
            self.last_error
        }
    }
}

fn submit_diagnostic_scene(
    ordering: &mut OrderingTable,
    state: VisualState,
) -> Result<(), crust_renderer::command::CommandError> {
    let hue = ((state.seed.rotate_left(7) & 0xff) as f32) / 255.0;
    let pulse = (state.time * 0.45).sin() * 0.025;
    let sky = color([0.025 + hue * 0.035, 0.07 + pulse, 0.085 + hue * 0.045]);
    let horizon = color([0.07 + hue * 0.08, 0.13 + pulse, 0.14]);
    let ground = color([0.025, 0.045, 0.04 + hue * 0.04]);
    let amber = color([0.95, 0.48 + hue * 0.18, 0.13]);
    let cyan = color([0.17, 0.72 + pulse, 0.63]);

    ordering.submit_overlay(0, colored_quad(-1.0, -1.0, 1.0, 1.0, sky, horizon))?;
    // Low-poly distant silhouette, intentionally original and data-independent.
    ordering.submit_overlay(
        1,
        colored_triangle([[-1.0, -0.32], [-0.56, 0.18], [-0.16, -0.32]], ground),
    )?;
    ordering.submit_overlay(
        1,
        colored_triangle([[-0.34, -0.32], [0.08, 0.31], [0.52, -0.32]], ground),
    )?;
    ordering.submit_overlay(
        1,
        colored_triangle([[0.32, -0.32], [0.78, 0.12], [1.0, -0.32]], ground),
    )?;
    ordering.submit_overlay(2, colored_quad(-1.0, -1.0, 1.0, -0.31, ground, ground))?;

    if state.active {
        let x = finite_or_zero(state.player_x).clamp(-0.88, 0.88);
        let y = finite_or_zero(state.player_y).clamp(-0.78, 0.56);
        let bob = (finite_or_zero(state.time) * 5.0).sin() * 0.012;
        ordering.submit_overlay(
            3,
            colored_triangle(
                [
                    [x - 0.055, y - 0.08 + bob],
                    [x, y + 0.07 + bob],
                    [x + 0.055, y - 0.08 + bob],
                ],
                amber,
            ),
        )?;
        ordering.submit_overlay(
            4,
            colored_quad(x - 0.09, -0.83, x + 0.09, -0.80, cyan, cyan),
        )?;
    }
    Ok(())
}

fn colored_triangle(points: [[f32; 2]; 3], color: Rgba8) -> PrimitiveCommand {
    PrimitiveCommand::ColoredTriangle(ColoredTriangle {
        vertices: points.map(|[x, y]| ColoredVertex {
            position: screen_point(x, y),
            color,
        }),
        blend: BlendMode::Opaque,
        style: PrimitiveStyle::Fill,
    })
}

fn colored_quad(
    left: f32,
    bottom: f32,
    right: f32,
    top: f32,
    bottom_color: Rgba8,
    top_color: Rgba8,
) -> PrimitiveCommand {
    PrimitiveCommand::ColoredQuad(ColoredQuad {
        vertices: [
            ColoredVertex {
                position: screen_point(left, top),
                color: top_color,
            },
            ColoredVertex {
                position: screen_point(right, top),
                color: top_color,
            },
            ColoredVertex {
                position: screen_point(left, bottom),
                color: bottom_color,
            },
            ColoredVertex {
                position: screen_point(right, bottom),
                color: bottom_color,
            },
        ],
        blend: BlendMode::Opaque,
        style: PrimitiveStyle::Fill,
    })
}

fn screen_point(x: f32, y: f32) -> ScreenPoint {
    ScreenPoint {
        x: (finite_or_zero(x).clamp(-1.0, 1.0) * 256.0).round() as i32,
        y: (-finite_or_zero(y).clamp(-1.0, 1.0) * 120.0).round() as i32,
        z: 0,
    }
}

fn color(rgb: [f32; 3]) -> Rgba8 {
    let [r, g, b] =
        rgb.map(|component| (finite_or_zero(component).clamp(0.0, 1.0) * 255.0).round() as u8);
    Rgba8 { r, g, b, a: 255 }
}

fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

fn backend_error(error: &impl core::fmt::Display) -> JsValue {
    JsValue::from_str(&error.to_string())
}

fn command_error(error: &crust_renderer::command::CommandError) -> JsValue {
    JsValue::from_str(&error.to_string())
}
