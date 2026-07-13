use crust_renderer::cache::TextureHandle;
use crust_renderer::command::{
    BlendMode, OrderingTable, PrimitiveCommand, ScreenRect, SpriteCommand, UvRect,
};
use crust_renderer::projection::Viewport;
use crust_renderer::texture::{DecodedTexture, Rgba8};
use wasm_bindgen::JsValue;
use web_sys::HtmlCanvasElement;

use crate::renderer_backend::{RenderOptions, RendererBackend};
use crate::retail_scene::{RetailScene, RetailSceneCommand};

const LOADING_IMAGE_HANDLE: TextureHandle = TextureHandle::new(u64::MAX);
const TITLE_IMAGE_HANDLE: TextureHandle = TextureHandle::new(u64::MAX - 1);
const LOADING_IMAGE_DEPTH: u16 = 2_047;
const TITLE_IMAGE_DEPTH: u16 = 0;
const NEUTRAL_TEXTURE_COLOR: Rgba8 = Rgba8 {
    r: 128,
    g: 128,
    b: 128,
    a: 255,
};

#[derive(Clone, Copy, Debug)]
pub struct VisualState {
    pub show_title_image: bool,
    pub show_retail_scene: bool,
    pub show_loading_image: bool,
}

#[derive(Debug)]
pub struct GlStage {
    backend: RendererBackend,
    ordering: OrderingTable,
    loading_image_dimensions: Option<[i32; 2]>,
    title_image_dimensions: Option<[i32; 2]>,
    retail_scene_commands: Vec<RetailSceneCommand>,
    last_error: u32,
}

impl GlStage {
    pub fn new(canvas: &HtmlCanvasElement) -> Result<Self, JsValue> {
        let backend = RendererBackend::new(canvas).map_err(|error| backend_error(&error))?;
        Ok(Self {
            backend,
            ordering: OrderingTable::default(),
            loading_image_dimensions: None,
            title_image_dimensions: None,
            retail_scene_commands: Vec::new(),
            last_error: 0,
        })
    }

    /// Uploads a decoded retail loading image. Simulation-controlled
    /// [`VisualState`] decides whether it is presented on a given frame.
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
        Ok(())
    }

    /// Upload a title card composed directly from the retail MDAT/IPAL/IMAG
    /// asset graph. It remains resident until another title state replaces it.
    pub fn install_title_image(&mut self, image: &DecodedTexture) -> Result<(), JsValue> {
        let dimensions = [
            i32::try_from(image.width())
                .map_err(|_| JsValue::from_str("title-image width exceeds WebGL limits"))?,
            i32::try_from(image.height())
                .map_err(|_| JsValue::from_str("title-image height exceeds WebGL limits"))?,
        ];
        self.backend
            .upload_texture(TITLE_IMAGE_HANDLE, image)
            .map_err(|error| backend_error(&error))?;
        self.title_image_dimensions = Some(dimensions);
        Ok(())
    }

    /// Replaces the resident progress-zero retail world snapshot.
    pub fn install_retail_scene(&mut self, scene: RetailScene) -> Result<(), JsValue> {
        let retained = [LOADING_IMAGE_HANDLE, TITLE_IMAGE_HANDLE]
            .into_iter()
            .chain(scene.textures.iter().map(|texture| texture.handle))
            .collect::<Vec<_>>();
        self.backend
            .upload_textures_atomically(
                scene
                    .textures
                    .iter()
                    .map(|texture| (texture.handle, &texture.pixels)),
            )
            .map_err(|error| backend_error(&error))?;
        self.retail_scene_commands = scene.commands;
        self.backend.retain_textures(retained);
        Ok(())
    }

    pub fn render(&mut self, state: VisualState) -> Result<(), JsValue> {
        self.ordering.clear();

        if state.show_retail_scene {
            for command in &self.retail_scene_commands {
                self.ordering
                    .submit_world(
                        command.depth,
                        command.zone,
                        command.polygon,
                        command.primitive.clone(),
                    )
                    .map_err(|error| command_error(&error))?;
            }
        }

        if state.show_title_image
            && let Some([width, height]) = self.title_image_dimensions
        {
            self.submit_image(TITLE_IMAGE_HANDLE, TITLE_IMAGE_DEPTH, width, height)?;
        }

        if state.show_loading_image
            && let Some([width, height]) = self.loading_image_dimensions
        {
            self.submit_image(LOADING_IMAGE_HANDLE, LOADING_IMAGE_DEPTH, width, height)?;
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
        Ok(())
    }

    fn submit_image(
        &mut self,
        texture: TextureHandle,
        depth: u16,
        width: i32,
        height: i32,
    ) -> Result<(), JsValue> {
        self.ordering
            .submit_overlay(
                depth,
                PrimitiveCommand::Sprite(SpriteCommand {
                    rect: ScreenRect {
                        x: -(width / 2),
                        y: -(height / 2),
                        width,
                        height,
                    },
                    depth: i32::from(depth),
                    color: NEUTRAL_TEXTURE_COLOR,
                    texture,
                    uv: UvRect::default(),
                    blend: BlendMode::Opaque,
                }),
            )
            .map_err(|error| command_error(&error))
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

fn backend_error(error: &impl core::fmt::Display) -> JsValue {
    JsValue::from_str(&error.to_string())
}

fn command_error(error: &crust_renderer::command::CommandError) -> JsValue {
    JsValue::from_str(&error.to_string())
}
