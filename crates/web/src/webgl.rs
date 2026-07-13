use core::fmt;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crust_renderer::cache::TextureHandle;
use crust_renderer::command::{
    BlendMode, OrderingTable, PrimitiveCommand, ScreenRect, SpriteCommand, UvRect,
};
use crust_renderer::projection::Viewport;
use crust_renderer::texture::{DecodedTexture, Rgba8};
use wasm_bindgen::JsValue;
use web_sys::HtmlCanvasElement;

use crate::renderer_backend::{RenderOptions, RendererBackend};
use crate::retail_scene::{RetailScene, RetailSceneCommand, RetailSceneTexture};

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

/// Work performed while replacing one camera-projected retail scene.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetailSceneUpdateDiagnostics {
    pub commands: usize,
    pub scene_textures: usize,
    pub uploaded_textures: usize,
    pub reused_textures: usize,
    pub removed_textures: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RetailSceneUpdatePlan {
    upload_indices: Vec<usize>,
    incoming_handles: Vec<TextureHandle>,
    reused_textures: usize,
    removed_textures: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetailSceneUpdateError {
    Duplicate(TextureHandle),
    Reserved(TextureHandle),
    UndeclaredCommand(TextureHandle),
}

impl fmt::Display for RetailSceneUpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate(handle) => write!(
                formatter,
                "retail scene declares texture handle {} more than once",
                handle.get()
            ),
            Self::Reserved(handle) => write!(
                formatter,
                "retail scene uses reserved presentation texture handle {}",
                handle.get()
            ),
            Self::UndeclaredCommand(handle) => write!(
                formatter,
                "retail scene command references undeclared texture handle {}",
                handle.get()
            ),
        }
    }
}

#[derive(Debug)]
pub struct GlStage {
    backend: RendererBackend,
    ordering: OrderingTable,
    loading_image_dimensions: Option<[i32; 2]>,
    title_image_dimensions: Option<[i32; 2]>,
    retail_scene_commands: Vec<RetailSceneCommand>,
    retail_scene_textures: BTreeMap<TextureHandle, Arc<DecodedTexture>>,
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
            retail_scene_textures: BTreeMap::new(),
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

    /// Replaces the complete resident retail world snapshot.
    ///
    /// This preserves the original installation API while using the same
    /// immutable-content-identity texture diff as [`Self::update_retail_scene`].
    pub fn install_retail_scene(&mut self, scene: RetailScene) -> Result<(), JsValue> {
        self.update_retail_scene(scene).map(|_| ())
    }

    /// Installs one camera-projected retail scene without re-uploading exact
    /// decoded textures which are already resident.
    ///
    /// The complete texture manifest is deliberately required even for a
    /// camera-only update. `TextureCache` handles are local to one scene build
    /// and may be reused for different pixels by the next build, so a numeric
    /// handle alone is not a content identity. The pair-scoped cache returns
    /// immutable reference-counted pixels; the retained manifest validates
    /// those allocation identities in O(handles) without cloning or scanning
    /// pixel bytes. A different allocation is conservatively re-uploaded even
    /// when its bytes happen to match.
    ///
    /// Every changed or new texture is prepared before any GPU handle is
    /// replaced. Commands, the retained texture set, and CPU texture identity
    /// state are changed only after all required uploads succeed.
    pub fn update_retail_scene(
        &mut self,
        scene: RetailScene,
    ) -> Result<RetailSceneUpdateDiagnostics, JsValue> {
        let backend_resident = scene
            .textures
            .iter()
            .filter_map(|texture| {
                self.backend
                    .has_texture(texture.handle)
                    .then_some(texture.handle)
            })
            .collect::<BTreeSet<_>>();
        let plan = plan_retail_scene_update(
            &self.retail_scene_textures,
            &backend_resident,
            &scene.commands,
            &scene.textures,
        )
        .map_err(|error| JsValue::from_str(&error.to_string()))?;

        self.backend
            .upload_textures_atomically(
                plan.upload_indices
                    .iter()
                    .map(|&index| &scene.textures[index])
                    .map(|texture| (texture.handle, texture.pixels.as_ref())),
            )
            .map_err(|error| backend_error(&error))?;

        let diagnostics = RetailSceneUpdateDiagnostics {
            commands: scene.commands.len(),
            scene_textures: scene.textures.len(),
            uploaded_textures: plan.upload_indices.len(),
            reused_textures: plan.reused_textures,
            removed_textures: plan.removed_textures,
        };
        let retained = [LOADING_IMAGE_HANDLE, TITLE_IMAGE_HANDLE]
            .into_iter()
            .chain(plan.incoming_handles.iter().copied())
            .collect::<Vec<_>>();
        self.retail_scene_commands = scene.commands;
        self.retail_scene_textures = scene
            .textures
            .into_iter()
            .map(|texture| (texture.handle, texture.pixels))
            .collect();
        self.backend.retain_textures(retained);
        Ok(diagnostics)
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

fn plan_retail_scene_update(
    current: &BTreeMap<TextureHandle, Arc<DecodedTexture>>,
    backend_resident: &BTreeSet<TextureHandle>,
    commands: &[RetailSceneCommand],
    textures: &[RetailSceneTexture],
) -> Result<RetailSceneUpdatePlan, RetailSceneUpdateError> {
    let mut incoming = BTreeSet::new();
    let mut upload_indices = Vec::new();
    let mut reused_textures = 0_usize;
    for (index, texture) in textures.iter().enumerate() {
        if texture.handle == LOADING_IMAGE_HANDLE || texture.handle == TITLE_IMAGE_HANDLE {
            return Err(RetailSceneUpdateError::Reserved(texture.handle));
        }
        if !incoming.insert(texture.handle) {
            return Err(RetailSceneUpdateError::Duplicate(texture.handle));
        }
        let exact_cpu_match = current
            .get(&texture.handle)
            .is_some_and(|pixels| Arc::ptr_eq(pixels, &texture.pixels));
        if exact_cpu_match && backend_resident.contains(&texture.handle) {
            reused_textures = reused_textures.saturating_add(1);
        } else {
            upload_indices.push(index);
        }
    }
    validate_command_textures(
        commands,
        |handle| incoming.contains(&handle),
        RetailSceneUpdateError::UndeclaredCommand,
    )?;

    let removed_textures = current
        .keys()
        .filter(|handle| !incoming.contains(handle))
        .count();
    Ok(RetailSceneUpdatePlan {
        upload_indices,
        incoming_handles: incoming.into_iter().collect(),
        reused_textures,
        removed_textures,
    })
}

fn validate_command_textures(
    commands: &[RetailSceneCommand],
    contains: impl Fn(TextureHandle) -> bool,
    error: impl Fn(TextureHandle) -> RetailSceneUpdateError,
) -> Result<(), RetailSceneUpdateError> {
    for command in commands {
        let texture = match &command.primitive {
            PrimitiveCommand::ColoredTriangle(_) | PrimitiveCommand::ColoredQuad(_) => None,
            PrimitiveCommand::TexturedTriangle(triangle) => Some(triangle.texture),
            PrimitiveCommand::TexturedQuad(quad) => Some(quad.texture),
            PrimitiveCommand::Sprite(sprite) => Some(sprite.texture),
        };
        if let Some(texture) = texture
            && !contains(texture)
        {
            return Err(error(texture));
        }
    }
    Ok(())
}

fn backend_error(error: &impl core::fmt::Display) -> JsValue {
    JsValue::from_str(&error.to_string())
}

fn command_error(error: &crust_renderer::command::CommandError) -> JsValue {
    JsValue::from_str(&error.to_string())
}

#[cfg(test)]
mod tests {
    use crust_renderer::texture::decode_indexed8;

    use super::*;

    fn decoded_texture(handle: u64, color: u16) -> RetailSceneTexture {
        let mut palette = vec![0_u16; 256];
        palette[1] = color;
        RetailSceneTexture {
            handle: TextureHandle::new(handle),
            pixels: Arc::new(
                decode_indexed8(&[1], 1, 1, &palette, BlendMode::Opaque)
                    .expect("one-pixel indexed texture must decode"),
            ),
        }
    }

    fn textured_command(handle: u64) -> RetailSceneCommand {
        RetailSceneCommand {
            depth: 1,
            zone: 2,
            polygon: 3,
            primitive: PrimitiveCommand::Sprite(SpriteCommand {
                rect: ScreenRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                depth: 1,
                color: NEUTRAL_TEXTURE_COLOR,
                texture: TextureHandle::new(handle),
                uv: UvRect::default(),
                blend: BlendMode::Opaque,
            }),
        }
    }

    #[test]
    fn planner_reuses_shared_pixel_identity_and_uploads_only_changes() {
        let shared = decoded_texture(1, 0x001f);
        let old_changed = decoded_texture(2, 0x03e0);
        let removed = decoded_texture(4, 0x7c00);
        let current = [shared.clone(), old_changed, removed]
            .into_iter()
            .map(|texture| (texture.handle, texture.pixels))
            .collect::<BTreeMap<_, _>>();
        let incoming = vec![
            shared,
            decoded_texture(2, 0x7fff),
            decoded_texture(3, 0x4210),
        ];
        let backend_resident = [TextureHandle::new(1), TextureHandle::new(2)]
            .into_iter()
            .collect::<BTreeSet<_>>();

        let plan = plan_retail_scene_update(
            &current,
            &backend_resident,
            &[textured_command(1), textured_command(2)],
            &incoming,
        )
        .expect("complete scene should produce an update plan");

        assert_eq!(plan.upload_indices, vec![1, 2]);
        assert_eq!(
            plan.incoming_handles,
            vec![
                TextureHandle::new(1),
                TextureHandle::new(2),
                TextureHandle::new(3)
            ]
        );
        assert_eq!(plan.reused_textures, 1);
        assert_eq!(plan.removed_textures, 1);
    }

    #[test]
    fn planner_never_treats_a_reused_numeric_handle_as_content_identity() {
        let installed = decoded_texture(5, 0x001f);
        let replacement = decoded_texture(5, 0x7c00);
        assert!(!Arc::ptr_eq(&installed.pixels, &replacement.pixels));
        assert_ne!(installed.pixels, replacement.pixels);
        let current = [(installed.handle, installed.pixels)]
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let backend_resident = [TextureHandle::new(5)].into_iter().collect::<BTreeSet<_>>();

        let plan = plan_retail_scene_update(
            &current,
            &backend_resident,
            &[textured_command(5)],
            &[replacement],
        )
        .expect("a reused builder-local handle must produce a safe replacement plan");

        assert_eq!(plan.upload_indices, vec![0]);
        assert_eq!(plan.reused_textures, 0);
        assert_eq!(plan.removed_textures, 0);
    }

    #[test]
    fn planner_reuploads_equal_pixels_with_a_distinct_content_identity() {
        let installed = decoded_texture(6, 0x4210);
        let replacement = decoded_texture(6, 0x4210);
        assert_eq!(installed.pixels, replacement.pixels);
        assert!(!Arc::ptr_eq(&installed.pixels, &replacement.pixels));
        let current = [(installed.handle, installed.pixels)]
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let backend_resident = [TextureHandle::new(6)].into_iter().collect::<BTreeSet<_>>();

        let plan = plan_retail_scene_update(
            &current,
            &backend_resident,
            &[textured_command(6)],
            &[replacement],
        )
        .expect("a distinct immutable allocation is conservatively uploaded");

        assert_eq!(plan.upload_indices, vec![0]);
        assert_eq!(plan.reused_textures, 0);
    }

    #[test]
    fn planner_uses_zero_upload_fast_path_only_with_complete_shared_manifest() {
        let first = decoded_texture(1, 0x001f);
        let second = decoded_texture(2, 0x03e0);
        let current = [first.clone(), second.clone()]
            .into_iter()
            .map(|texture| (texture.handle, texture.pixels))
            .collect::<BTreeMap<_, _>>();
        let backend_resident = [first.handle, second.handle]
            .into_iter()
            .collect::<BTreeSet<_>>();

        let plan = plan_retail_scene_update(
            &current,
            &backend_resident,
            &[textured_command(2), textured_command(1)],
            &[first, second],
        )
        .expect("a complete shared texture manifest can safely update commands only");

        assert!(plan.upload_indices.is_empty());
        assert_eq!(plan.reused_textures, 2);

        assert_eq!(
            plan_retail_scene_update(
                &current,
                &backend_resident,
                &[textured_command(2)],
                &[decoded_texture(1, 0x001f)],
            ),
            Err(RetailSceneUpdateError::UndeclaredCommand(
                TextureHandle::new(2)
            ))
        );
    }

    #[test]
    fn planner_reuploads_shared_cpu_copy_missing_from_backend() {
        let incoming = decoded_texture(7, 0x4210);
        let current = [(incoming.handle, incoming.pixels.clone())]
            .into_iter()
            .collect::<BTreeMap<_, _>>();

        let plan = plan_retail_scene_update(
            &current,
            &BTreeSet::new(),
            &[textured_command(7)],
            &[incoming],
        )
        .expect("missing GPU copy should be recoverable by upload");

        assert_eq!(plan.upload_indices, vec![0]);
        assert_eq!(plan.reused_textures, 0);
    }

    #[test]
    fn planner_rejects_duplicate_reserved_and_undeclared_handles() {
        let duplicate = decoded_texture(9, 0x001f);
        assert_eq!(
            plan_retail_scene_update(
                &BTreeMap::new(),
                &BTreeSet::new(),
                &[],
                &[duplicate.clone(), duplicate]
            ),
            Err(RetailSceneUpdateError::Duplicate(TextureHandle::new(9)))
        );

        let mut reserved = decoded_texture(1, 0x001f);
        reserved.handle = LOADING_IMAGE_HANDLE;
        assert_eq!(
            plan_retail_scene_update(&BTreeMap::new(), &BTreeSet::new(), &[], &[reserved]),
            Err(RetailSceneUpdateError::Reserved(LOADING_IMAGE_HANDLE))
        );

        assert_eq!(
            plan_retail_scene_update(
                &BTreeMap::new(),
                &BTreeSet::new(),
                &[textured_command(11)],
                &[]
            ),
            Err(RetailSceneUpdateError::UndeclaredCommand(
                TextureHandle::new(11)
            ))
        );
    }
}
