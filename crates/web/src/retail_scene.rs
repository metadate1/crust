//! Safe, pointer-free construction of a retail world path snapshot.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use crust_formats::binary::Eid;
use crust_formats::stream::structs::ZonePathPoint;
use crust_formats::stream::{
    Entry, GoolAnimationDescriptor, GoolFontAnimation, GoolFragmentAnimation, GoolSpriteAnimation,
    GoolTextAnimation, GoolTextureInfo, GoolVertexAnimation, LevelId, Nsd, Nsf, NsfPage,
    ObjectMaterial, ObjectModelFrame, ObjectVertexKind, PolygonId, SlstCursor, SlstItem,
    WorldGeometry, WorldMapPathList, ZoneHeader, ZonePath, ZoneRect, load_object_model_frame,
    parse_gool_animation_descriptor, parse_object_frame, parse_world_geometry,
};
use crust_renderer::cache::{TextureCache, TextureHandle, TextureRequest, TextureUvBounds};
use crust_renderer::command::{
    BlendMode, ColoredTriangle, ColoredVertex, CommandSource, PrimitiveCommand, PrimitiveStyle,
    TexturedQuad, TexturedTriangle, TexturedVertex, Uv,
};
use crust_renderer::projection::{
    Matrix3, ProjectionResult, TriangleVisibility, Vec3i, Viewport, project, rotate,
    rotate_translate,
};
use crust_renderer::retail_texture::{
    RetailTextureReference, TextureInfo2, TpagReference, resolve_texture_page,
};
use crust_renderer::sprite::{
    ProjectedSpriteQuad, RetailSpriteCamera, RetailSpriteTransform, RetailSpriteVectors,
    project_retail_fragment, project_retail_sprite, retail_sprite_half_size,
    retail_sprite_shift_word, retail_sprite_shrink,
};
use crust_renderer::text::{RetailTextProjection, project_retail_text};
use crust_renderer::texture::{DecodedTexture, Rgba8};
use crust_renderer::world::wrapped_lightning_color;
use crust_renderer::{
    Dark2Parameters, GoolObjectLighting, LightningChannel, ObjectDarkShaderInput,
    ObjectProjectionParameters, ObjectProjectionTransform, ProjectedObjectPolygon, WorldShaderMode,
    apply_dark, apply_dark2, apply_fog, apply_lightning, apply_object_zone_shader, fog_cutoff,
    project_object_model,
};
use crust_sim::Angle12;
use crust_sim::camera::RetailCameraPose;
use crust_sim::gool::{AnimationSource, ProcessAnimationKind, ProcessTextAnimation};
use crust_sim::paging::TextureFrameSnapshot;
use crust_sim::retail_runtime::{
    RetailRenderObject, RetailWorldShaderSnapshot, RuntimeObjectHandle,
};

const ZDAT_ENTRY_TYPE: u32 = 7;
const SLST_ENTRY_TYPE: u32 = 4;
const WGEO_ENTRY_TYPE: u32 = 3;
const RETAIL_TEXTURE_PAGE_SLOTS: usize = 8;
const RETAIL_OBJECT_MODEL_CACHE_FRAMES: usize = 256;
const PRESENTATION_TEXTURE_BUDGET_BYTES: usize = 64 * 1024 * 1024;
const PRESENTATION_TEXTURE_ENTRY_LIMIT: usize = 8_192;
const PRESENTATION_WORLD_LIMIT: usize = 4_096;
const PRESENTATION_POLYGON_LIMIT: usize = 131_072;
#[cfg(test)]
const ZONE_FLAG_RIPPLE: u32 = crust_renderer::world::ZONE_FLAG_RIPPLE;
// `LdatInit` initializes the global current/next GOOL display masks to
// DISPLAY_WORLDS | DISPANIM_OBJECTS | CAM_UPDATE. The ZDAT field with the
// same C-era name is a separate neighbor-zone lifecycle mask.
const RETAIL_INITIAL_DISPLAY_FLAGS: u32 = 0xffff;

/// One world/object command with exact provenance and ordering-table depth.
#[derive(Clone, Debug, PartialEq)]
pub struct RetailSceneCommand {
    pub depth: u16,
    pub source: CommandSource,
    pub primitive: PrimitiveCommand,
}

/// One decoded texture required by the scene's commands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetailSceneTexture {
    /// Builder-local reference used by this scene's commands. Independent
    /// scene builds may reuse the number for different decoded pixels, so a
    /// consumer must validate the complete texture manifest before reuse.
    pub handle: TextureHandle,
    /// Immutable decoded pixels leased directly from the pair-scoped cache.
    /// Cloning a scene manifest clones this lease, not the pixel allocation.
    pub pixels: Arc<DecodedTexture>,
}

/// Read-only diagnostics for the current world snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetailSceneStats {
    pub worlds: usize,
    /// Non-backdrop WGEOs retained from the complete reachable ZDAT graph by
    /// the optional presentation preloader. These meshes are not submitted
    /// simultaneously because retail zones may reuse world coordinate space.
    pub preloaded_worlds: usize,
    pub visible_polygons: usize,
    pub submitted_polygons: usize,
    pub unique_textures: usize,
    pub saturated_vertices: usize,
    pub skipped_textured_polygons: usize,
    pub visible_objects: usize,
    pub submitted_object_polygons: usize,
    pub submitted_object_quads: usize,
    pub saturated_object_polygons: usize,
    pub culled_object_polygons: usize,
    pub skipped_object_animations: usize,
    pub skipped_object_textured_polygons: usize,
}

/// Renderer-owned data derived only from a validated NSD/NSF pair.
#[derive(Clone, Debug, PartialEq)]
pub struct RetailScene {
    pub commands: Vec<RetailSceneCommand>,
    pub textures: Vec<RetailSceneTexture>,
    pub stats: RetailSceneStats,
    pub zone: Eid,
    pub path_index: u32,
    pub path_point_count: u16,
    pub path_point_index: u16,
    pub draw_count: u32,
}

/// Process-lifetime software-renderer scratch retained independently from a
/// mounted pair. Native Dark2 deliberately reuses the last target installed by
/// plain/fog/ripple or Lightning/Dark world dispatch.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetailWorldShaderRenderState {
    far_color1: [u8; 3],
}

impl RetailWorldShaderRenderState {
    #[must_use]
    pub const fn far_color1(self) -> [u8; 3] {
        self.far_color1
    }
}

/// Exact validated world/camera state selected by the retail level runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailSceneLocation {
    pub zone: Eid,
    pub path_index: u32,
    pub path_point_index: usize,
    pub draw_count: u32,
}

/// Exact signed 8.8 path progress selected by the retail camera runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailSceneProgressLocation {
    pub zone: Eid,
    pub path_index: u32,
    pub path_progress: i32,
    /// Native `frames_elapsed` consumed by the graphics-flag `0x1000`
    /// object-only camera. This advances independently from `draw_count`.
    pub frame_stamp: u32,
    pub draw_count: u32,
}

/// Presentation-only scene options which never feed back into simulation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailScenePresentation {
    /// Direct GTE-style projection distance. `None` uses the authored FOV.
    pub projection_distance: Option<u32>,
    /// Preload the reachable non-backdrop WGEO graph, then draw every ordinary
    /// polygon in the active authored zone instead of its retail SLST subset.
    /// Mutually exclusive zones and backdrop swaps remain SLST-authored.
    pub extended_world: bool,
    /// Presentation viewport used to discard complete-level triangles which
    /// cannot affect this output shape.
    pub viewport: Viewport,
}

impl Default for RetailScenePresentation {
    fn default() -> Self {
        Self {
            projection_distance: None,
            extended_world: false,
            viewport: Viewport::PSX,
        }
    }
}

/// Explicit native camera transform used by non-path camera modes.
///
/// Ordinary gameplay derives this transform from the active ZDAT path. The
/// spin-death camera instead mutates `cam` around an authored object vertex,
/// while visibility and paging remain owned by that same path location.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailSceneCameraPose {
    pub translation: [i32; 3],
    /// Retail camera rotation order is Y, X, Z.
    pub rotation_yxz: [i32; 3],
}

impl From<RetailCameraPose> for RetailSceneCameraPose {
    fn from(pose: RetailCameraPose) -> Self {
        Self {
            translation: pose.translation,
            rotation_yxz: pose.rotation_yxz,
        }
    }
}

/// Pre-GOOL island-map path flags consumed by native `GfxLoadWorlds`.
///
/// The scene builder additionally verifies that the mounted pair is title
/// level 0x19 and that `title_state` is exactly 15 before applying these
/// values. Keeping the state beside the flags makes accidental animation on a
/// different title screen or retail level impossible at this boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailMapPathAnimation {
    pub title_state: u32,
    pub map_level_links: u32,
    pub map_key_links: u32,
}

/// Controlled failure while following the retail LDAT scene graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetailSceneError(String);

impl fmt::Display for RetailSceneError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RetailSceneError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RetailSceneCacheKey {
    zone: Eid,
    path_index: u32,
}

#[derive(Debug)]
struct CachedSceneGraph {
    key: RetailSceneCacheKey,
    zone_header: ZoneHeader,
    zone_rect: ZoneRect,
    path: ZonePath,
    visibility: Option<SlstCursor>,
    world_eids: Vec<Eid>,
    worlds: Vec<Arc<WorldGeometry>>,
    map_paths_parsed: bool,
    world_map_paths: Vec<Option<WorldMapPathList>>,
    /// Last masks written by `GfxAnimMapPaths` for this active graph.
    ///
    /// Native writes into the resident WGEO entry, so those values survive a
    /// title-state change until the graph is replaced. Keeping the writes in a
    /// sidecar preserves that lifetime without mutating parsed source data.
    world_map_path_masks: Vec<Vec<Option<u8>>>,
}

#[derive(Clone, Debug)]
struct CachedWorldGeometry {
    eid: Eid,
    geometry: Arc<WorldGeometry>,
}

#[derive(Clone, Debug)]
struct FullLevelSceneGraph {
    level: LevelId,
    worlds: Vec<CachedWorldGeometry>,
}

#[derive(Clone, Debug)]
struct SceneWorld {
    geometry: Arc<WorldGeometry>,
    active_index: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
struct SelectedWorldPolygon {
    world_index: usize,
    polygon_index: usize,
    source: CommandSource,
    retail_authored: bool,
}

#[derive(Debug)]
struct PresentationTexture {
    pixels: Arc<DecodedTexture>,
    content_uv: TextureUvBounds,
    byte_len: usize,
    last_used: u64,
}

/// Pair-scoped, presentation-only texture cache for complete-level WGEOs.
///
/// It is intentionally independent from native's eight live TPAG slots. The
/// simulation pager remains exact; this cache only leases decoded pixels for
/// geometry that the optional wider presentation can see.
#[derive(Debug, Default)]
struct PresentationTextureCache {
    entries: HashMap<TextureRequest, PresentationTexture>,
    resident_bytes: usize,
    use_clock: u64,
}

impl PresentationTextureCache {
    fn load(
        &mut self,
        reference: RetailTextureReference,
        request: TextureRequest,
        nsf: &Nsf,
        nsf_bytes: &[u8],
    ) -> Result<(Arc<DecodedTexture>, TextureUvBounds), RetailSceneError> {
        self.use_clock = self.use_clock.saturating_add(1);
        if let Some(cached) = self.entries.get_mut(&request) {
            cached.last_used = self.use_clock;
            return Ok((Arc::clone(&cached.pixels), cached.content_uv));
        }

        let decoded = reference
            .decode(nsf, nsf_bytes)
            .map_err(|error| scene_error(format!("extended WGEO texture decode: {error}")))?
            .with_edge_padding(1)
            .map_err(|error| scene_error(format!("extended WGEO texture padding: {error}")))?;
        let byte_len = decoded.byte_len();
        if byte_len > PRESENTATION_TEXTURE_BUDGET_BYTES || PRESENTATION_TEXTURE_ENTRY_LIMIT == 0 {
            return Err(scene_error(format!(
                "extended WGEO texture needs {byte_len} bytes beyond the presentation cache budget"
            )));
        }
        while self.entries.len() >= PRESENTATION_TEXTURE_ENTRY_LIMIT
            || self.resident_bytes.saturating_add(byte_len) > PRESENTATION_TEXTURE_BUDGET_BYTES
        {
            let Some(eviction) = self
                .entries
                .iter()
                .min_by_key(|(key, cached)| (cached.last_used, key.page_id))
                .map(|(key, _)| *key)
            else {
                return Err(scene_error(
                    "extended WGEO texture cache cannot free enough resident space",
                ));
            };
            if let Some(removed) = self.entries.remove(&eviction) {
                self.resident_bytes = self.resident_bytes.saturating_sub(removed.byte_len);
            }
        }

        let pixels = Arc::new(decoded);
        let content_uv = presentation_content_uv(request, &pixels);
        self.resident_bytes = self.resident_bytes.saturating_add(byte_len);
        self.entries.insert(
            request,
            PresentationTexture {
                pixels: Arc::clone(&pixels),
                content_uv,
                byte_len,
                last_used: self.use_clock,
            },
        );
        Ok((pixels, content_uv))
    }
}

fn presentation_content_uv(request: TextureRequest, texture: &DecodedTexture) -> TextureUvBounds {
    let width = f32::from(u16::try_from(texture.width()).unwrap_or(u16::MAX));
    let height = f32::from(u16::try_from(texture.height()).unwrap_or(u16::MAX));
    let region_width = f32::from(u16::try_from(request.region.width).unwrap_or(u16::MAX));
    let region_height = f32::from(u16::try_from(request.region.height).unwrap_or(u16::MAX));
    TextureUvBounds {
        left: 1.5 / width,
        top: 1.5 / height,
        right: (region_width + 0.5) / width,
        bottom: (region_height + 0.5) / height,
    }
}

fn stable_scene_texture_handle(
    request: TextureRequest,
    by_request: &mut HashMap<TextureRequest, TextureHandle>,
    by_handle: &mut HashMap<TextureHandle, TextureRequest>,
) -> Result<TextureHandle, RetailSceneError> {
    if let Some(handle) = by_request.get(&request) {
        return Ok(*handle);
    }
    // Fixed FNV-1a over every decoded-image identity field. Restricting the
    // result to the lower 63 bits keeps it disjoint from stage-reserved image
    // handles. A same-frame collision is rejected instead of aliasing pixels.
    let clut = request.clut.map_or(u32::MAX, |clut| {
        u32::from(clut.block_x) | (u32::from(clut.row) << 8)
    });
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for word in [
        request.page_id,
        request.region.x,
        request.region.y,
        request.region.width,
        request.region.height,
        u32::from(request.color_mode as u8),
        u32::from(request.blend_mode as u8),
        clut,
    ] {
        for byte in word.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    let handle = TextureHandle::new((hash & 0x7fff_ffff_ffff_ffff).max(1));
    if let Some(existing) = by_handle.get(&handle)
        && *existing != request
    {
        return Err(scene_error(format!(
            "stable scene texture handle {} collided across distinct requests",
            handle.get()
        )));
    }
    by_request.insert(request, handle);
    by_handle.insert(handle, request);
    Ok(handle)
}

/// Cumulative evidence that immutable retail scene data is reused.
///
/// These counters describe parsing and CPU-side texture decoding only. They
/// are intentionally not presented as frame-time or low-end-device metrics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetailSceneCacheDiagnostics {
    pub graph_builds: u64,
    pub graph_reuses: u64,
    pub texture_page_installs: u64,
    pub texture_requests: u64,
    pub texture_hits: u64,
    pub texture_misses: u64,
}

/// Pair-scoped state of native's 16-cell `tri_wave` buffer.
#[derive(Clone, Debug, Eq, PartialEq)]
struct RetailRippleState {
    level: crust_formats::stream::LevelId,
    speed: i32,
    period: i32,
    cells: [i32; 16],
}

impl RetailRippleState {
    fn new(level: crust_formats::stream::LevelId) -> Self {
        let (speed, period) = retail_ripple_rate(level);
        let stride = (period + 1) / 8;
        Self {
            level,
            speed,
            period,
            cells: std::array::from_fn(|index| {
                let index = i32::try_from(index).expect("a 16-cell wave index fits i32");
                -(period - index * stride)
            }),
        }
    }

    fn magnitudes(&mut self, advance: bool) -> [i32; 16] {
        if advance {
            for cell in &mut self.cells {
                *cell += self.speed;
                if *cell > self.period {
                    *cell = -(self.period - 1);
                }
            }
        }
        self.cells.map(i32::abs)
    }
}

/// Pair-scoped owner of parsed ZDAT/SLST/WGEO data and decoded textures.
///
/// The active graph is keyed by the exact zone/path pair. Moving to another
/// zone or path replaces the graph and texture-page state. Constructing a new
/// builder at stream-pair mount provides the stronger pair boundary, so no
/// handles or decoded pixels can survive a level transition accidentally.
#[derive(Debug)]
pub struct RetailSceneBuilder {
    active_graph: Option<CachedSceneGraph>,
    full_level_graph: Option<Arc<FullLevelSceneGraph>>,
    object_models: HashMap<(Eid, u16), Arc<ObjectModelFrame>>,
    object_model_lru: VecDeque<(Eid, u16)>,
    texture_cache: TextureCache,
    presentation_texture_cache: PresentationTextureCache,
    texture_pages: [Option<u32>; RETAIL_TEXTURE_PAGE_SLOTS],
    texture_page_generations: [Option<u32>; RETAIL_TEXTURE_PAGE_SLOTS],
    ripple: Option<RetailRippleState>,
    diagnostics: RetailSceneCacheDiagnostics,
}

impl Default for RetailSceneBuilder {
    fn default() -> Self {
        Self {
            active_graph: None,
            full_level_graph: None,
            object_models: HashMap::new(),
            object_model_lru: VecDeque::new(),
            texture_cache: TextureCache::default(),
            presentation_texture_cache: PresentationTextureCache::default(),
            texture_pages: [None; RETAIL_TEXTURE_PAGE_SLOTS],
            texture_page_generations: [None; RETAIL_TEXTURE_PAGE_SLOTS],
            ripple: None,
            diagnostics: RetailSceneCacheDiagnostics::default(),
        }
    }
}

impl RetailSceneBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub const fn diagnostics(&self) -> RetailSceneCacheDiagnostics {
        self.diagnostics
    }

    /// Builds the progress-zero spawn-zone scene using this pair-scoped cache.
    ///
    /// # Errors
    ///
    /// Returns an error when the validated pair's scene graph or referenced
    /// texture data cannot be represented safely.
    pub fn build(
        &mut self,
        nsd: &Nsd,
        nsf: &Nsf,
        nsf_bytes: &[u8],
    ) -> Result<RetailScene, RetailSceneError> {
        self.build_at_path_point(nsd, nsf, nsf_bytes, 0, 0)
    }

    /// Builds an integral spawn-path camera point using this pair-scoped cache.
    ///
    /// # Errors
    ///
    /// Returns an error for an out-of-range point or malformed scene graph.
    pub fn build_at_path_point(
        &mut self,
        nsd: &Nsd,
        nsf: &Nsf,
        nsf_bytes: &[u8],
        path_point_index: usize,
        draw_count: u32,
    ) -> Result<RetailScene, RetailSceneError> {
        let ldat = nsd
            .ldat()
            .ok_or_else(|| scene_error("index-only NSD has no LDAT scene"))?;
        let path_index = u32::try_from(ldat.spawn_path_index)
            .map_err(|_| scene_error("LDAT spawn path index is negative"))?;
        self.build_at_location(
            nsd,
            nsf,
            nsf_bytes,
            RetailSceneLocation {
                zone: ldat.spawn_zone,
                path_index,
                path_point_index,
                draw_count,
            },
        )
    }

    /// Builds an integral spawn-path point while mirroring the mounted
    /// pager's exact slots instead of allocating texture pages from demand.
    ///
    /// # Errors
    ///
    /// Returns an error when the spawn location is invalid, a mirrored pager
    /// slot disagrees with the mounted NSF, or any referenced scene resource
    /// is malformed.
    #[allow(clippy::too_many_arguments)]
    pub fn build_at_path_point_with_texture_frame(
        &mut self,
        nsd: &Nsd,
        nsf: &Nsf,
        nsf_bytes: &[u8],
        path_point_index: usize,
        draw_count: u32,
        texture_frame_snapshot: TextureFrameSnapshot,
        world_shader_snapshot: RetailWorldShaderSnapshot,
        world_shader_render_state: &mut RetailWorldShaderRenderState,
    ) -> Result<RetailScene, RetailSceneError> {
        let ldat = nsd
            .ldat()
            .ok_or_else(|| scene_error("index-only NSD has no LDAT scene"))?;
        let path_index = u32::try_from(ldat.spawn_path_index)
            .map_err(|_| scene_error("LDAT spawn path index is negative"))?;
        let path_point_index = i32::try_from(path_point_index)
            .map_err(|_| scene_error("active path point index does not fit signed progress"))?;
        let path_progress = path_point_index
            .checked_mul(0x100)
            .ok_or_else(|| scene_error("active path point progress overflows signed 8.8 space"))?;
        self.build_at_progress_with_runtime_snapshots(
            nsd,
            nsf,
            nsf_bytes,
            RetailSceneProgressLocation {
                zone: ldat.spawn_zone,
                path_index,
                path_progress,
                frame_stamp: draw_count,
                draw_count,
            },
            true,
            &[],
            None,
            RETAIL_INITIAL_DISPLAY_FLAGS,
            ldat.field_of_view,
            None,
            None,
            texture_frame_snapshot,
            world_shader_snapshot,
            world_shader_render_state,
        )
    }

    /// Builds an integral arbitrary zone/path camera state.
    ///
    /// # Errors
    ///
    /// Returns an error for an out-of-range point or malformed scene graph.
    pub fn build_at_location(
        &mut self,
        nsd: &Nsd,
        nsf: &Nsf,
        nsf_bytes: &[u8],
        location: RetailSceneLocation,
    ) -> Result<RetailScene, RetailSceneError> {
        let path_point_index = i32::try_from(location.path_point_index)
            .map_err(|_| scene_error("active path point index does not fit signed progress"))?;
        let path_progress = path_point_index
            .checked_mul(0x100)
            .ok_or_else(|| scene_error("active path point progress overflows signed 8.8 space"))?;
        self.build_at_progress(
            nsd,
            nsf,
            nsf_bytes,
            RetailSceneProgressLocation {
                zone: location.zone,
                path_index: location.path_index,
                path_progress,
                frame_stamp: location.draw_count,
                draw_count: location.draw_count,
            },
        )
    }

    /// Builds an exact signed 8.8 arbitrary zone/path camera state.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid progress or malformed referenced data.
    pub fn build_at_progress(
        &mut self,
        nsd: &Nsd,
        nsf: &Nsf,
        nsf_bytes: &[u8],
        location: RetailSceneProgressLocation,
    ) -> Result<RetailScene, RetailSceneError> {
        build_retail_scene_cached(
            self,
            nsd,
            nsf,
            nsf_bytes,
            location,
            true,
            &[],
            None,
            RETAIL_INITIAL_DISPLAY_FLAGS,
            None,
            None,
            None,
            None,
            None,
            None,
            RetailScenePresentation::default(),
        )
    }

    /// Builds an exact camera-path state with presentation-only overrides.
    ///
    /// # Errors
    ///
    /// Returns the same checked errors as [`Self::build_at_progress`].
    pub fn build_at_progress_with_presentation(
        &mut self,
        nsd: &Nsd,
        nsf: &Nsf,
        nsf_bytes: &[u8],
        location: RetailSceneProgressLocation,
        presentation: RetailScenePresentation,
    ) -> Result<RetailScene, RetailSceneError> {
        build_retail_scene_cached(
            self,
            nsd,
            nsf,
            nsf_bytes,
            location,
            true,
            &[],
            None,
            RETAIL_INITIAL_DISPLAY_FLAGS,
            None,
            None,
            None,
            None,
            None,
            None,
            presentation,
        )
    }

    /// Builds one world frame plus post-GOOL vertex objects against one
    /// pair-scoped, frame-frozen texture cache.
    ///
    /// The object list must be the immutable preorder snapshot captured after
    /// the cooperative GOOL update. Camera selection still precedes GOOL in
    /// the application; NSF data is immutable, so collecting the complete
    /// world/object TPAG union here does not change simulation ordering.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed pair-scoped animation/model data,
    /// invalid scene references, or a texture union that cannot fit retail's
    /// eight resident TPAG slots.
    pub fn build_at_progress_with_objects(
        &mut self,
        nsd: &Nsd,
        nsf: &Nsf,
        nsf_bytes: &[u8],
        location: RetailSceneProgressLocation,
        objects: &[RetailRenderObject],
        main_object: Option<RuntimeObjectHandle>,
    ) -> Result<RetailScene, RetailSceneError> {
        self.build_at_progress_with_objects_and_world_display_mask(
            nsd,
            nsf,
            nsf_bytes,
            location,
            objects,
            main_object,
            RETAIL_INITIAL_DISPLAY_FLAGS,
        )
    }

    /// Builds the post-GOOL snapshot using the exact pre-GOOL world mask.
    ///
    /// Native submits world geometry before traversing GOOL objects. The
    /// caller must therefore capture current global nine after spawn/camera
    /// setup but before GOOL. Each [`RetailRenderObject`] independently carries
    /// the live global-nine value sampled at its later transform boundary, so
    /// a same-frame authored write cannot retroactively alter the world or be
    /// collapsed into one post-GOOL mask.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed scene, animation, object, paging, or
    /// texture data referenced by the mounted pair.
    #[allow(clippy::too_many_arguments)]
    pub fn build_at_progress_with_objects_and_world_display_mask(
        &mut self,
        nsd: &Nsd,
        nsf: &Nsf,
        nsf_bytes: &[u8],
        location: RetailSceneProgressLocation,
        objects: &[RetailRenderObject],
        main_object: Option<RuntimeObjectHandle>,
        world_display_mask: u32,
    ) -> Result<RetailScene, RetailSceneError> {
        build_retail_scene_cached(
            self,
            nsd,
            nsf,
            nsf_bytes,
            location,
            true,
            objects,
            main_object,
            world_display_mask,
            None,
            None,
            None,
            None,
            None,
            None,
            RetailScenePresentation::default(),
        )
    }

    /// Builds a post-GOOL scene with the title runtime's state-specific FOV.
    ///
    /// Retail mutates the title LDAT projection before loading each menu state;
    /// the mounted NSD stays immutable here, so the browser passes that checked
    /// scalar explicitly instead of rewriting parsed game data.
    ///
    /// # Errors
    ///
    /// Returns the same checked scene and asset errors as
    /// [`Self::build_at_progress_with_objects_and_world_display_mask`],
    /// including an
    /// unsupported field of view.
    #[allow(clippy::too_many_arguments)]
    pub fn build_at_progress_with_objects_and_world_display_mask_and_fov(
        &mut self,
        nsd: &Nsd,
        nsf: &Nsf,
        nsf_bytes: &[u8],
        location: RetailSceneProgressLocation,
        advance_world_ripple: bool,
        objects: &[RetailRenderObject],
        main_object: Option<RuntimeObjectHandle>,
        world_display_mask: u32,
        field_of_view: u32,
        map_path_animation: Option<RetailMapPathAnimation>,
    ) -> Result<RetailScene, RetailSceneError> {
        self.build_at_progress_with_objects_and_world_display_mask_and_fov_and_camera(
            nsd,
            nsf,
            nsf_bytes,
            location,
            advance_world_ripple,
            objects,
            main_object,
            world_display_mask,
            field_of_view,
            map_path_animation,
            None,
        )
    }

    /// Builds a scene with an optional explicit native camera transform.
    ///
    /// The override changes only projection. The active ZDAT path continues
    /// to own SLST visibility, world selection, paging identity, and object
    /// zone graphics exactly as it does during native `CamDeath`.
    ///
    /// # Errors
    ///
    /// Returns the same checked errors as
    /// [`Self::build_at_progress_with_objects_and_world_display_mask_and_fov`].
    #[allow(clippy::too_many_arguments)]
    pub fn build_at_progress_with_objects_and_world_display_mask_and_fov_and_camera(
        &mut self,
        nsd: &Nsd,
        nsf: &Nsf,
        nsf_bytes: &[u8],
        location: RetailSceneProgressLocation,
        advance_world_ripple: bool,
        objects: &[RetailRenderObject],
        main_object: Option<RuntimeObjectHandle>,
        world_display_mask: u32,
        field_of_view: u32,
        map_path_animation: Option<RetailMapPathAnimation>,
        camera_pose: Option<RetailSceneCameraPose>,
    ) -> Result<RetailScene, RetailSceneError> {
        build_retail_scene_cached(
            self,
            nsd,
            nsf,
            nsf_bytes,
            location,
            advance_world_ripple,
            objects,
            main_object,
            world_display_mask,
            Some(field_of_view),
            map_path_animation,
            camera_pose,
            None,
            None,
            None,
            RetailScenePresentation::default(),
        )
    }

    /// Builds the browser runtime's source-ordered frame from the one
    /// `TexturesBeginFrame` slot snapshot plus per-object live slot snapshots.
    /// The latter reproduce mid-frame GOOL replacement misses without making
    /// a newly opened page visible before the following frame.
    ///
    /// # Errors
    ///
    /// Returns an error when the runtime snapshot names an invalid object,
    /// camera, texture slot, or malformed scene resource.
    #[allow(clippy::too_many_arguments)]
    pub fn build_at_progress_with_runtime_snapshots(
        &mut self,
        nsd: &Nsd,
        nsf: &Nsf,
        nsf_bytes: &[u8],
        location: RetailSceneProgressLocation,
        advance_world_ripple: bool,
        objects: &[RetailRenderObject],
        main_object: Option<RuntimeObjectHandle>,
        world_display_mask: u32,
        field_of_view: u32,
        map_path_animation: Option<RetailMapPathAnimation>,
        camera_pose: Option<RetailSceneCameraPose>,
        texture_frame_snapshot: TextureFrameSnapshot,
        world_shader_snapshot: RetailWorldShaderSnapshot,
        world_shader_render_state: &mut RetailWorldShaderRenderState,
    ) -> Result<RetailScene, RetailSceneError> {
        self.build_at_progress_with_runtime_snapshots_and_presentation(
            nsd,
            nsf,
            nsf_bytes,
            location,
            advance_world_ripple,
            objects,
            main_object,
            world_display_mask,
            field_of_view,
            map_path_animation,
            camera_pose,
            texture_frame_snapshot,
            world_shader_snapshot,
            world_shader_render_state,
            RetailScenePresentation::default(),
        )
    }

    /// Builds the runtime snapshot with opt-in display-only projection and
    /// visibility upgrades.
    ///
    /// # Errors
    ///
    /// Returns the same checked scene errors as
    /// [`Self::build_at_progress_with_runtime_snapshots`].
    #[allow(clippy::too_many_arguments)]
    pub fn build_at_progress_with_runtime_snapshots_and_presentation(
        &mut self,
        nsd: &Nsd,
        nsf: &Nsf,
        nsf_bytes: &[u8],
        location: RetailSceneProgressLocation,
        advance_world_ripple: bool,
        objects: &[RetailRenderObject],
        main_object: Option<RuntimeObjectHandle>,
        world_display_mask: u32,
        field_of_view: u32,
        map_path_animation: Option<RetailMapPathAnimation>,
        camera_pose: Option<RetailSceneCameraPose>,
        texture_frame_snapshot: TextureFrameSnapshot,
        world_shader_snapshot: RetailWorldShaderSnapshot,
        world_shader_render_state: &mut RetailWorldShaderRenderState,
        presentation: RetailScenePresentation,
    ) -> Result<RetailScene, RetailSceneError> {
        build_retail_scene_cached(
            self,
            nsd,
            nsf,
            nsf_bytes,
            location,
            advance_world_ripple,
            objects,
            main_object,
            world_display_mask,
            Some(field_of_view),
            map_path_animation,
            camera_pose,
            Some(texture_frame_snapshot),
            Some(world_shader_snapshot),
            Some(world_shader_render_state),
            presentation,
        )
    }
}

/// Builds the progress-zero spawn-zone world scene for one validated pair.
///
/// Texture descriptors which the original C port would resolve outside its
/// fixed UV table are omitted safely and counted. Structural graph failures
/// return an error rather than dereferencing serialized values as pointers.
///
/// # Errors
///
/// Returns an error for an absent/mistyped scene entry, malformed item,
/// invalid cross-reference, unsupported FOV, missing TPAG, or a scene needing
/// more than the retail eight simultaneous texture-page slots.
pub fn build_retail_scene(
    nsd: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
) -> Result<RetailScene, RetailSceneError> {
    RetailSceneBuilder::new().build(nsd, nsf, nsf_bytes)
}

/// Builds one exact integer camera-path state with the corresponding mutable
/// SLST visibility and texture-animation counter.
///
/// # Errors
///
/// Returns the same structural errors as [`build_retail_scene`], plus an error
/// when the requested point is outside the active path.
pub fn build_retail_scene_at_path_point(
    nsd: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    path_point_index: usize,
    draw_count: u32,
) -> Result<RetailScene, RetailSceneError> {
    RetailSceneBuilder::new().build_at_path_point(nsd, nsf, nsf_bytes, path_point_index, draw_count)
}

/// Builds the world snapshot for an arbitrary validated ZDAT path state.
///
/// This is the rendering boundary needed by `LevelUpdate`: callers may move
/// between zones and paths without falling back to the LDAT spawn location.
///
/// # Errors
///
/// Returns an error when the active zone/path/SLST/WGEO graph is malformed or
/// cannot be represented by the bounded renderer.
pub fn build_retail_scene_at_location(
    nsd: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    location: RetailSceneLocation,
) -> Result<RetailScene, RetailSceneError> {
    RetailSceneBuilder::new().build_at_location(nsd, nsf, nsf_bytes, location)
}

/// Builds the world snapshot at exact signed 8.8 retail camera progress.
///
/// The camera is interpolated exactly like `ZonePathProgressToLoc`, including
/// its shortest-route yaw interpolation and its following-path endpoint at a
/// fractional final point. Serialized references remain validated handles.
///
/// # Errors
///
/// Returns an error for out-of-range progress or any malformed active or
/// following zone/path reference.
pub fn build_retail_scene_at_progress(
    nsd: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    location: RetailSceneProgressLocation,
) -> Result<RetailScene, RetailSceneError> {
    RetailSceneBuilder::new().build_at_progress(nsd, nsf, nsf_bytes, location)
}

fn update_world_shader_render_state(
    state: &mut RetailWorldShaderRenderState,
    mode: WorldShaderMode,
    shader: RetailWorldShaderSnapshot,
    zone_far_color: [u8; 3],
    dispatch_active: bool,
    has_visible_polygons: bool,
) {
    if !dispatch_active || (mode != WorldShaderMode::Fog && !has_visible_polygons) {
        return;
    }
    match mode {
        WorldShaderMode::Plain | WorldShaderMode::Fog | WorldShaderMode::Ripple => {
            state.far_color1 = zone_far_color;
        }
        WorldShaderMode::Lightning | WorldShaderMode::Dark => {
            state.far_color1 = wrapped_lightning_color(shader.clear_color);
        }
        WorldShaderMode::Dark2 => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn build_retail_scene_cached(
    builder: &mut RetailSceneBuilder,
    nsd: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    location: RetailSceneProgressLocation,
    advance_world_ripple: bool,
    render_objects: &[RetailRenderObject],
    main_object: Option<RuntimeObjectHandle>,
    world_display_mask: u32,
    field_of_view_override: Option<u32>,
    map_path_animation: Option<RetailMapPathAnimation>,
    camera_pose: Option<RetailSceneCameraPose>,
    texture_frame_snapshot: Option<TextureFrameSnapshot>,
    world_shader_snapshot: Option<RetailWorldShaderSnapshot>,
    world_shader_render_state: Option<&mut RetailWorldShaderRenderState>,
    presentation: RetailScenePresentation,
) -> Result<RetailScene, RetailSceneError> {
    let ldat = nsd
        .ldat()
        .ok_or_else(|| scene_error("index-only NSD has no LDAT scene"))?;
    let path_progress = location
        .path_progress
        .checked_abs()
        .ok_or_else(|| scene_error("signed path progress cannot be i32::MIN"))?;
    let path_point_index = usize::try_from(path_progress >> 8)
        .map_err(|_| scene_error("active path point index does not fit the host"))?;
    let map_path_animation = active_map_path_animation(nsd.level(), map_path_animation);
    let key = RetailSceneCacheKey {
        zone: location.zone,
        path_index: location.path_index,
    };
    let needs_map_paths = map_path_animation.is_some();
    let graph_reusable = scene_graph_cache_matches(
        builder.active_graph.as_ref().map(|graph| graph.key),
        builder
            .active_graph
            .as_ref()
            .is_some_and(|graph| graph.map_paths_parsed),
        key,
        needs_map_paths,
    );
    if graph_reusable {
        builder.diagnostics.graph_reuses = builder.diagnostics.graph_reuses.saturating_add(1);
    } else {
        let graph = parse_scene_graph(nsd, nsf, nsf_bytes, key, path_point_index, needs_map_paths)?;
        builder.active_graph = Some(graph);
        builder.texture_cache = TextureCache::default();
        builder.texture_pages = [None; RETAIL_TEXTURE_PAGE_SLOTS];
        builder.texture_page_generations = [None; RETAIL_TEXTURE_PAGE_SLOTS];
        builder.diagnostics.graph_builds = builder.diagnostics.graph_builds.saturating_add(1);
    }
    if presentation.extended_world
        && nsd.level() != LevelId::TITLE
        && builder
            .full_level_graph
            .as_ref()
            .is_none_or(|graph| graph.level != nsd.level())
    {
        builder.full_level_graph =
            Some(Arc::new(parse_full_level_scene_graph(nsd, nsf, nsf_bytes)?));
        builder.presentation_texture_cache = PresentationTextureCache::default();
    }
    let full_level_graph = presentation
        .extended_world
        .then(|| builder.full_level_graph.clone())
        .flatten();
    let preloaded_worlds = full_level_graph
        .as_ref()
        .map_or(0, |graph| graph.worlds.len());

    let graph = builder
        .active_graph
        .as_mut()
        .expect("a successful graph parse installs the requested key");
    if path_point_index >= graph.path.points.len() {
        return Err(scene_error("active path progress is outside its ZDAT path"));
    }
    let draw_count = location.draw_count;
    let path_point_count = u16::try_from(graph.path.points.len())
        .map_err(|_| scene_error("spawn path point count does not fit u16"))?;
    let path_point_index_u16 = u16::try_from(path_point_index)
        .map_err(|_| scene_error("spawn path point index does not fit u16"))?;

    // LevelUpdate deliberately does not open the SLST when the zone has no
    // worlds. Title, Hog Wild and Whole Hog use this as an external-transition
    // dummy start, and their placeholder SLST EID is absent from this stream.
    // GOOL objects may still be present, so only the world list becomes empty.
    let retail_visible = if graph.zone_header.worlds.is_empty() {
        Vec::new()
    } else {
        let visibility = graph
            .visibility
            .as_mut()
            .expect("a world-bearing graph always owns an SLST cursor");
        visibility
            .seek(path_point_index)
            .map_err(|error| scene_error(format!("spawn SLST state: {error}")))?;
        visibility.visibility().to_vec()
    };
    validate_visibility(&retail_visible, &graph.worlds)?;
    update_persistent_world_map_path_masks(graph, map_path_animation)?;
    let world_map_path_masks = graph.world_map_path_masks.clone();
    let (scene_worlds, mut visible_polygons) = select_scene_worlds(
        graph,
        &retail_visible,
        full_level_graph.as_deref(),
        location,
        presentation.extended_world,
    )?;
    if world_display_mask & 1 == 0 {
        visible_polygons.clear();
    }

    let camera = if let Some(camera_pose) = camera_pose {
        camera_sample_from_pose(camera_pose)
    } else {
        sample_camera(
            nsd,
            nsf,
            nsf_bytes,
            &graph.zone_header,
            &graph.zone_rect,
            &graph.path,
            location.path_progress,
        )?
    };
    let camera_translation = camera.translation;
    let raw_world_camera_matrix =
        raw_camera_matrix(camera.rotation_y, camera.rotation_x, camera.rotation_z);
    let camera_matrix = adjusted_camera_matrix(raw_world_camera_matrix);
    let object_camera =
        object_camera_sample_for_location(camera, graph.zone_header.graphics.flags, location);
    let raw_object_camera_matrix = raw_camera_matrix(
        object_camera.rotation_y,
        object_camera.rotation_x,
        object_camera.rotation_z,
    );
    let object_camera_matrix = adjusted_camera_matrix(raw_object_camera_matrix);
    let authored_projection_distance =
        projection_distance(field_of_view_override.unwrap_or(ldat.field_of_view))?;
    let projection_distance = if let Some(distance) = presentation.projection_distance {
        if !(64..=2_048).contains(&distance) {
            return Err(scene_error(format!(
                "presentation projection distance {distance} is outside 64..=2048"
            )));
        }
        distance
    } else {
        authored_projection_distance
    };
    let world_shader_mode = WorldShaderMode::from_flags(graph.zone_header.graphics.flags);
    let world_shader_snapshot = world_shader_snapshot
        .unwrap_or_else(|| RetailWorldShaderSnapshot::initialized_for_level(nsd.level()));
    let mut local_world_shader_render_state = RetailWorldShaderRenderState::default();
    let world_shader_render_state =
        world_shader_render_state.unwrap_or(&mut local_world_shader_render_state);
    let world_dispatch_active = world_display_mask & 1 != 0 && !graph.worlds.is_empty();
    update_world_shader_render_state(
        world_shader_render_state,
        world_shader_mode,
        world_shader_snapshot,
        graph.zone_header.graphics.far_color,
        world_dispatch_active,
        !retail_visible.is_empty(),
    );
    let ripple_wave = if world_shader_mode == WorldShaderMode::Ripple {
        if builder
            .ripple
            .as_ref()
            .is_none_or(|ripple| ripple.level != nsd.level())
        {
            builder.ripple = Some(RetailRippleState::new(nsd.level()));
        }
        Some(
            builder
                .ripple
                .as_mut()
                .expect("a ripple world installs pair-scoped wave state")
                .magnitudes(
                    advance_world_ripple
                        && world_display_mask & 1 != 0
                        && !retail_visible.is_empty(),
                ),
        )
    } else {
        None
    };
    let prepared_objects = prepare_objects(
        nsd,
        nsf,
        nsf_bytes,
        &mut builder.object_models,
        &mut builder.object_model_lru,
        &graph.zone_header,
        render_objects,
        main_object,
        object_camera,
        raw_object_camera_matrix,
        object_camera_matrix,
        projection_distance,
    )?;

    let mut page_ids = BTreeSet::new();
    for selected in &visible_polygons {
        if presentation.extended_world && !selected.retail_authored {
            continue;
        }
        let world = &scene_worlds[selected.world_index];
        let geometry = &world.geometry;
        let polygon_index = selected.polygon_index;
        let animation_mask = world
            .active_index
            .and_then(|active_index| world_map_path_masks.get(active_index))
            .and_then(|masks| masks.get(polygon_index))
            .copied()
            .flatten();
        let polygon = polygon_with_map_path_mask(geometry.polygons[polygon_index], animation_mask);
        if let Some(texture) = geometry
            .texture_for_polygon(polygon, draw_count)
            .map_err(|error| scene_error(format!("WGEO texture reference: {error}")))?
        {
            let reference = RetailTextureReference::new(
                TpagReference::new(texture.texture_page),
                TextureInfo2 {
                    color: texture.color,
                    region: texture.region,
                },
            );
            if reference.layout().is_ok() {
                page_ids.insert(texture.texture_page.raw());
            }
        }
    }
    for object in &prepared_objects.objects {
        for polygon in &object.polygons {
            if let ObjectMaterial::Texture { texture_page, .. } = polygon.material {
                page_ids.insert(texture_page.raw());
            }
        }
    }
    for quad in &prepared_objects.quads {
        page_ids.insert(quad.texture_page.raw());
    }
    let resident_texture_pages = texture_frame_snapshot.map_or_else(
        || resident_texture_pages(nsd, nsf, &graph.zone_header),
        |snapshot| Ok(texture_snapshot_page_eids(snapshot)),
    )?;
    page_ids.retain(|page| resident_texture_pages.contains(page));
    if page_ids.len() > RETAIL_TEXTURE_PAGE_SLOTS {
        return Err(scene_error(format!(
            "spawn scene needs {} simultaneous TPAGs; retail has eight slots",
            page_ids.len()
        )));
    }

    if let Some(snapshot) = texture_frame_snapshot {
        install_texture_frame_snapshot(
            &mut builder.texture_cache,
            &mut builder.texture_pages,
            &mut builder.texture_page_generations,
            &mut builder.diagnostics,
            nsf,
            nsf_bytes,
            snapshot,
        )?;
    } else {
        install_missing_texture_pages(
            &mut builder.texture_cache,
            &mut builder.texture_pages,
            &mut builder.texture_page_generations,
            &mut builder.diagnostics,
            nsf,
            nsf_bytes,
            &page_ids,
        )?;
    }
    builder.texture_cache.begin_frame();
    let mut extended_retail_texture_leases = HashMap::new();
    if presentation.extended_world {
        // Preserve native's backwards SLST texture-request order exactly.
        // The extended polygon list is geometry-ordered, so deriving these
        // leases from it would perturb the eight-slot cache's LRU stamps.
        for polygon_id in retail_visible.iter().rev() {
            let active_index = usize::from(polygon_id.world_index);
            let geometry = graph.worlds.get(active_index).ok_or_else(|| {
                scene_error("SLST texture world slot is outside the active graph")
            })?;
            let polygon_index = usize::from(polygon_id.polygon_index);
            let animation_mask = world_map_path_masks
                .get(active_index)
                .and_then(|masks| masks.get(polygon_index))
                .copied()
                .flatten();
            let polygon =
                polygon_with_map_path_mask(geometry.polygons[polygon_index], animation_mask);
            let Some(texture) = geometry
                .texture_for_polygon(polygon, draw_count)
                .map_err(|error| scene_error(format!("WGEO texture reference: {error}")))?
            else {
                continue;
            };
            let reference = RetailTextureReference::new(
                TpagReference::new(texture.texture_page),
                TextureInfo2 {
                    color: texture.color,
                    region: texture.region,
                },
            );
            let Ok(layout) = reference.layout() else {
                continue;
            };
            if let Ok(cached) = builder.texture_cache.load(layout.request) {
                extended_retail_texture_leases
                    .entry(layout.request)
                    .or_insert((cached.pixels, cached.content_uv));
            }
        }
    }

    let world_translations = scene_worlds
        .iter()
        .map(|world| {
            if world.geometry.header.is_backdrop {
                Vec3i::default()
            } else {
                rotate(
                    Vec3i {
                        x: world.geometry.header.translation[0]
                            .saturating_sub(camera_translation.x),
                        y: world.geometry.header.translation[1]
                            .saturating_sub(camera_translation.y),
                        z: world.geometry.header.translation[2]
                            .saturating_sub(camera_translation.z),
                    },
                    camera_matrix,
                )
                .point
            }
        })
        .collect::<Vec<_>>();
    let world_fog_cutoffs = if matches!(
        world_shader_mode,
        WorldShaderMode::Fog | WorldShaderMode::Dark
    ) {
        scene_worlds
            .iter()
            .map(|world| {
                fog_cutoff(
                    nsd.level().get(),
                    graph.zone_header.graphics.visibility_depth,
                    0,
                    graph.zone_header.graphics.unknown_b_to_e[0],
                    world.geometry.header.is_backdrop,
                    world_shader_mode == WorldShaderMode::Fog,
                )
                .map_err(|error| scene_error(format!("WGEO fog parameters: {error}")))
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };

    let mut textures = BTreeMap::new();
    let mut texture_handles = HashMap::new();
    let mut texture_requests_by_handle = HashMap::new();
    let mut presentation_frame_requests = HashSet::new();
    let mut presentation_frame_bytes = 0_usize;
    let mut prepared = vec![None; visible_polygons.len()];
    let mut minimum_depth = 0x07ff_i32;
    let mut saturated_vertices = 0_usize;
    let mut skipped_textured_polygons = 0_usize;

    // Retail transforms SLST entries backwards, applies a running minimum OT
    // depth, and head-inserts. We prepare backwards but later submit forwards
    // to compensate for the Rust ordering table's FIFO buckets.
    for (visible_index, selected) in visible_polygons.iter().copied().enumerate().rev() {
        let world_index = selected.world_index;
        let world = &scene_worlds[world_index];
        let geometry = &world.geometry;
        let polygon_projection_distance = if geometry.header.is_backdrop {
            authored_projection_distance
        } else {
            projection_distance
        };
        let polygon_index = selected.polygon_index;
        let animation_mask = world
            .active_index
            .and_then(|active_index| world_map_path_masks.get(active_index))
            .and_then(|masks| masks.get(polygon_index))
            .copied()
            .flatten();
        let polygon = polygon_with_map_path_mask(geometry.polygons[polygon_index], animation_mask);
        let vertices = polygon
            .vertex_indices
            .map(|index| geometry.vertices[usize::from(index)]);
        let mut screens = [crust_renderer::command::ScreenPoint::default(); 3];
        let mut camera_depths = [0_i32; 3];
        let mut projection_valid = [true; 3];
        for vertex_index in 0..3 {
            let vertex = vertices[vertex_index];
            let [x, mut y, z] = vertex.expanded_position();
            if vertex.effect
                && let Some(wave) = ripple_wave.as_ref()
            {
                // `SwRippleShader` runs before the world transform. WGEO
                // coordinates have already received their factor-of-eight
                // expansion, so this is the source's exact
                // `((x + y) / 8) & 0xf` wave-cell selection.
                let wave_index = usize::try_from(((x + y) / 8) & 0x0f)
                    .expect("a masked ripple-wave index fits usize");
                y = y.saturating_add(wave[wave_index]);
            }
            // Backdrop alternatives remain an authored SLST presentation:
            // keep their original PSX projection and saturation even when the
            // ordinary zone geometry uses the wider unclamped projection.
            let mut projected = if presentation.extended_world && !geometry.header.is_backdrop {
                project_presentation(
                    Vec3i { x, y, z },
                    world_translations[world_index],
                    camera_matrix,
                    polygon_projection_distance,
                )
            } else {
                project(
                    Vec3i { x, y, z },
                    world_translations[world_index],
                    camera_matrix,
                    [0, 0],
                    polygon_projection_distance,
                )
            };
            if geometry.header.is_backdrop && presentation.viewport != Viewport::PSX {
                projected.screen =
                    expand_backdrop_to_viewport(projected.screen, presentation.viewport);
            }
            if !projected.valid {
                saturated_vertices = saturated_vertices.saturating_add(1);
            }
            screens[vertex_index] = projected.screen;
            camera_depths[vertex_index] = projected.camera.z;
            projection_valid[vertex_index] = projected.valid;
        }
        if presentation.extended_world && !geometry.header.is_backdrop {
            let near = i32::try_from(polygon_projection_distance / 2).unwrap_or(i32::MAX);
            if projection_valid.contains(&false)
                || camera_depths.iter().any(|depth| *depth <= near)
                || presentation.viewport.classify_triangle(screens) == TriangleVisibility::Outside
            {
                continue;
            }
        }
        let clear_channel = LightningChannel {
            color: world_shader_snapshot.clear_color,
            t: world_shader_snapshot.clear_t,
        };
        let effect_channel = LightningChannel {
            color: world_shader_snapshot.effect_color,
            t: world_shader_snapshot.effect_t,
        };
        let mut colors = [Rgba8::default(); 3];
        let mut shader_failed = false;
        for vertex_index in 0..3 {
            let vertex = vertices[vertex_index];
            let color = match world_shader_mode {
                WorldShaderMode::Plain | WorldShaderMode::Ripple => Ok(vertex.color),
                WorldShaderMode::Fog => apply_fog(
                    vertex.color,
                    screens[vertex_index].z,
                    world_fog_cutoffs[world_index],
                    graph.zone_header.graphics.unknown_b_to_e[0],
                    graph.zone_header.graphics.far_color,
                )
                .map_err(|error| scene_error(format!("WGEO fog shader: {error}"))),
                WorldShaderMode::Lightning => {
                    apply_lightning(vertex.color, vertex.effect, clear_channel, effect_channel)
                        .map_err(|error| scene_error(format!("WGEO lightning shader: {error}")))
                }
                WorldShaderMode::Dark => apply_dark(
                    vertex.color,
                    vertex.effect,
                    screens[vertex_index].z,
                    world_fog_cutoffs[world_index],
                    graph.zone_header.graphics.unknown_b_to_e[0],
                    clear_channel,
                    effect_channel,
                )
                .map_err(|error| scene_error(format!("WGEO dark shader: {error}"))),
                WorldShaderMode::Dark2 => apply_dark2(
                    vertex.color,
                    vertex.effect,
                    screens[vertex_index],
                    world_translations[world_index],
                    Dark2Parameters {
                        illumination: world_shader_snapshot.dark2_illumination,
                        shift_add: world_shader_snapshot.dark2_shift_add,
                        shift_sub: world_shader_snapshot.dark2_shift_sub,
                        ambient_effect_clear: world_shader_snapshot.dark2_ambient_clear,
                        ambient_effect_set: world_shader_snapshot.dark2_ambient_effect,
                        target: world_shader_render_state.far_color1,
                    },
                )
                .map_err(|error| scene_error(format!("WGEO Dark2 shader: {error}"))),
            };
            let color = match color {
                Ok(color) => color,
                Err(_) if presentation.extended_world && !selected.retail_authored => {
                    shader_failed = true;
                    break;
                }
                Err(error) => return Err(error),
            };
            colors[vertex_index] = Rgba8 {
                r: color[0],
                g: color[1],
                b: color[2],
                a: u8::MAX,
            };
        }
        if shader_failed {
            continue;
        }

        let texture = geometry
            .texture_for_polygon(polygon, draw_count)
            .map_err(|error| scene_error(format!("WGEO texture reference: {error}")))?;
        let primitive = if let Some(texture) = texture {
            let reference = RetailTextureReference::new(
                TpagReference::new(texture.texture_page),
                TextureInfo2 {
                    color: texture.color,
                    region: texture.region,
                },
            );
            let Ok(layout) = reference.layout() else {
                skipped_textured_polygons = skipped_textured_polygons.saturating_add(1);
                continue;
            };
            let presentation_texture = presentation.extended_world && !selected.retail_authored;
            let cached = if presentation_texture {
                if presentation_frame_requests.len() >= PRESENTATION_TEXTURE_ENTRY_LIMIT
                    && !presentation_frame_requests.contains(&layout.request)
                {
                    None
                } else {
                    builder
                        .presentation_texture_cache
                        .load(reference, layout.request, nsf, nsf_bytes)
                        .ok()
                }
            } else if presentation.extended_world {
                extended_retail_texture_leases
                    .get(&layout.request)
                    .map(|(pixels, content_uv)| (Arc::clone(pixels), *content_uv))
            } else {
                builder
                    .texture_cache
                    .load(layout.request)
                    .ok()
                    .map(|cached| (cached.pixels, cached.content_uv))
            };
            let Some((pixels, content_uv)) = cached else {
                skipped_textured_polygons = skipped_textured_polygons.saturating_add(1);
                continue;
            };
            if presentation_texture && presentation_frame_requests.insert(layout.request) {
                let next_bytes = presentation_frame_bytes.saturating_add(pixels.byte_len());
                if next_bytes > PRESENTATION_TEXTURE_BUDGET_BYTES {
                    presentation_frame_requests.remove(&layout.request);
                    skipped_textured_polygons = skipped_textured_polygons.saturating_add(1);
                    continue;
                }
                presentation_frame_bytes = next_bytes;
            }
            let output_handle = stable_scene_texture_handle(
                layout.request,
                &mut texture_handles,
                &mut texture_requests_by_handle,
            )?;
            textures
                .entry(output_handle)
                .or_insert_with(|| Arc::clone(&pixels));
            let uvs = layout.coordinates.cache_uvs(content_uv);
            PrimitiveCommand::TexturedTriangle(TexturedTriangle {
                vertices: std::array::from_fn(|index| TexturedVertex {
                    position: screens[index],
                    color: colors[index],
                    uv: Uv {
                        u: uvs[index][0],
                        v: uvs[index][1],
                    },
                }),
                texture: output_handle,
                blend: layout.request.blend_mode,
            })
        } else {
            let color_word = crust_formats::stream::structs::ColorInfo::from_raw(
                geometry.texture_words[usize::from(polygon.texture_info_word_index)],
            );
            PrimitiveCommand::ColoredTriangle(ColoredTriangle {
                vertices: std::array::from_fn(|index| ColoredVertex {
                    position: screens[index],
                    color: colors[index],
                }),
                blend: blend_mode(color_word.semi_transparency()),
                style: PrimitiveStyle::Fill,
            })
        };

        let z_sum = screens
            .iter()
            .fold(0_i32, |sum, point| sum.saturating_add(point.z));
        let raw_depth = (0x0800_i32
            - i32::try_from(polygon_projection_distance / 2).unwrap_or(i32::MAX))
        .saturating_sub(z_sum / 32)
        .clamp(0, 0x07ff);
        let depth = if presentation.extended_world {
            if geometry.header.is_backdrop {
                0
            } else {
                // Reserve the farthest bucket for the authored backdrop so a
                // distant ordinary polygon cannot be overpainted by sky.
                raw_depth.max(1)
            }
        } else {
            let depth = raw_depth.min(minimum_depth);
            minimum_depth = depth;
            depth
        };
        let depth = u16::try_from(depth)
            .map_err(|_| scene_error("clamped ordering depth does not fit u16"))?;
        prepared[visible_index] = Some(RetailSceneCommand {
            depth,
            source: selected.source,
            primitive,
        });
    }

    let world_commands = prepared.into_iter().flatten().collect::<Vec<_>>();
    let submitted_polygons = world_commands.len();
    let mut object_commands = Vec::new();
    let mut skipped_object_textured_polygons = 0_usize;
    let mut submitted_object_polygons = 0_usize;
    let mut submitted_object_quads = 0_usize;
    for (render_index, render_object) in render_objects.iter().enumerate() {
        let object_resident_texture_pages = if let Some(object_snapshot) =
            effective_object_texture_snapshot(
                texture_frame_snapshot,
                render_object.texture_frame_snapshot,
            ) {
            install_texture_frame_snapshot(
                &mut builder.texture_cache,
                &mut builder.texture_pages,
                &mut builder.texture_page_generations,
                &mut builder.diagnostics,
                nsf,
                nsf_bytes,
                object_snapshot,
            )?;
            Some(texture_snapshot_page_eids(object_snapshot))
        } else {
            None
        };
        // World polygons consume the slots latched by TexturesBeginFrame,
        // while each object consumes the live slots captured at its own
        // display boundary. GOOL may synchronously open a TPAG during the
        // object's update; testing only the frame-opening snapshot would
        // install those bytes but still discard the object's textured faces.
        let object_resident_texture_pages = object_resident_texture_pages
            .as_ref()
            .unwrap_or(&resident_texture_pages);
        for object in prepared_objects
            .objects
            .iter()
            .filter(|object| object.render_index == render_index)
        {
            for (emission_index, polygon) in object.polygons.iter().enumerate() {
                let primitive = match polygon.material {
                    ObjectMaterial::Color(color) => {
                        PrimitiveCommand::ColoredTriangle(ColoredTriangle {
                            vertices: polygon.vertices.map(|vertex| ColoredVertex {
                                position: vertex.position,
                                color: vertex.color,
                            }),
                            blend: blend_mode(color.semi_transparency()),
                            style: PrimitiveStyle::Fill,
                        })
                    }
                    ObjectMaterial::Texture {
                        color,
                        texture_page,
                        region,
                    } => {
                        if !object_resident_texture_pages.contains(&texture_page.raw()) {
                            skipped_object_textured_polygons =
                                skipped_object_textured_polygons.saturating_add(1);
                            continue;
                        }
                        let reference = RetailTextureReference::new(
                            TpagReference::new(texture_page),
                            TextureInfo2 { color, region },
                        );
                        let Ok(layout) = reference.layout() else {
                            skipped_object_textured_polygons =
                                skipped_object_textured_polygons.saturating_add(1);
                            continue;
                        };
                        let Ok(cached) = builder.texture_cache.load(layout.request) else {
                            skipped_object_textured_polygons =
                                skipped_object_textured_polygons.saturating_add(1);
                            continue;
                        };
                        let output_handle = stable_scene_texture_handle(
                            layout.request,
                            &mut texture_handles,
                            &mut texture_requests_by_handle,
                        )?;
                        textures
                            .entry(output_handle)
                            .or_insert_with(|| Arc::clone(&cached.pixels));
                        let uvs = layout.coordinates.cache_uvs(cached.content_uv);
                        PrimitiveCommand::TexturedTriangle(TexturedTriangle {
                            vertices: std::array::from_fn(|index| TexturedVertex {
                                position: polygon.vertices[index].position,
                                color: polygon.vertices[index].color,
                                uv: Uv {
                                    u: uvs[index][0],
                                    v: uvs[index][1],
                                },
                            }),
                            texture: output_handle,
                            blend: layout.request.blend_mode,
                        })
                    }
                };
                object_commands.push((
                    object.render_index,
                    emission_index,
                    RetailSceneCommand {
                        depth: polygon.ordering_depth,
                        source: CommandSource::Object {
                            handle: object.handle,
                            part: polygon.source_part,
                        },
                        primitive,
                    },
                ));
                submitted_object_polygons = submitted_object_polygons.saturating_add(1);
            }
        }
        for quad in prepared_objects
            .quads
            .iter()
            .filter(|quad| quad.render_index == render_index)
        {
            if !object_resident_texture_pages.contains(&quad.texture_page.raw()) {
                skipped_object_textured_polygons =
                    skipped_object_textured_polygons.saturating_add(1);
                continue;
            }
            let reference =
                RetailTextureReference::new(TpagReference::new(quad.texture_page), quad.texture);
            let Ok(layout) = reference.layout() else {
                skipped_object_textured_polygons =
                    skipped_object_textured_polygons.saturating_add(1);
                continue;
            };
            let Ok(cached) = builder.texture_cache.load(layout.request) else {
                skipped_object_textured_polygons =
                    skipped_object_textured_polygons.saturating_add(1);
                continue;
            };
            let output_handle = stable_scene_texture_handle(
                layout.request,
                &mut texture_handles,
                &mut texture_requests_by_handle,
            )?;
            textures
                .entry(output_handle)
                .or_insert_with(|| Arc::clone(&cached.pixels));
            let uvs = layout.coordinates.cache_uvs(cached.content_uv);
            object_commands.push((
                quad.render_index,
                usize::from(quad.part),
                RetailSceneCommand {
                    depth: quad.projected.ordering_depth,
                    source: CommandSource::Object {
                        handle: quad.handle,
                        part: quad.part,
                    },
                    primitive: PrimitiveCommand::TexturedQuad(TexturedQuad {
                        vertices: std::array::from_fn(|index| TexturedVertex {
                            position: quad.projected.vertices[index],
                            color: quad.colors[index],
                            uv: Uv {
                                u: uvs[index][0],
                                v: uvs[index][1],
                            },
                        }),
                        texture: output_handle,
                        blend: layout.request.blend_mode,
                    }),
                },
            ));
            submitted_object_quads = submitted_object_quads.saturating_add(1);
        }
    }
    // Source object primitives are head-inserted after all world primitives.
    // The Rust ordering table is FIFO inside a depth bucket, so reverse the
    // complete object insertion stream and place it before the compensated
    // world stream.
    object_commands
        .sort_by_key(|(render_index, emission_index, _)| (*render_index, *emission_index));
    let mut object_commands = object_commands
        .into_iter()
        .rev()
        .map(|(_, _, command)| command)
        .collect::<Vec<_>>();
    object_commands.extend(world_commands);
    let commands = object_commands;
    let textures = textures
        .into_iter()
        .map(|(handle, pixels)| RetailSceneTexture { handle, pixels })
        .collect::<Vec<_>>();
    let cache_frame = builder.texture_cache.metrics().frame;
    builder.diagnostics.texture_requests = builder
        .diagnostics
        .texture_requests
        .saturating_add(cache_frame.requests);
    builder.diagnostics.texture_hits = builder
        .diagnostics
        .texture_hits
        .saturating_add(cache_frame.hits);
    builder.diagnostics.texture_misses = builder
        .diagnostics
        .texture_misses
        .saturating_add(cache_frame.misses);
    Ok(RetailScene {
        stats: RetailSceneStats {
            worlds: scene_worlds.len(),
            preloaded_worlds,
            visible_polygons: visible_polygons.len(),
            submitted_polygons,
            unique_textures: textures.len(),
            saturated_vertices,
            skipped_textured_polygons,
            visible_objects: prepared_objects.visible_objects,
            submitted_object_polygons,
            submitted_object_quads,
            saturated_object_polygons: prepared_objects.saturated_polygons,
            culled_object_polygons: prepared_objects.culled_polygons,
            skipped_object_animations: prepared_objects.skipped_animations,
            skipped_object_textured_polygons,
        },
        commands,
        textures,
        zone: location.zone,
        path_index: location.path_index,
        path_point_count,
        path_point_index: path_point_index_u16,
        draw_count,
    })
}

#[derive(Debug)]
struct PreparedVertexObject {
    render_index: usize,
    handle: u32,
    polygons: Vec<ProjectedObjectPolygon>,
}

#[derive(Debug)]
struct PreparedObjectQuad {
    render_index: usize,
    handle: u32,
    part: u16,
    texture_page: Eid,
    texture: TextureInfo2,
    projected: ProjectedSpriteQuad,
    colors: [Rgba8; 4],
}

#[derive(Debug, Default)]
struct PreparedObjects {
    objects: Vec<PreparedVertexObject>,
    quads: Vec<PreparedObjectQuad>,
    visible_objects: usize,
    saturated_polygons: usize,
    culled_polygons: usize,
    skipped_animations: usize,
}

#[allow(clippy::too_many_arguments)]
fn prepare_objects(
    nsd: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    model_cache: &mut HashMap<(Eid, u16), Arc<ObjectModelFrame>>,
    model_lru: &mut VecDeque<(Eid, u16)>,
    zone_header: &ZoneHeader,
    render_objects: &[RetailRenderObject],
    main_object: Option<RuntimeObjectHandle>,
    camera: CameraSample,
    raw_camera_matrix: Matrix3,
    adjusted_camera_matrix: Matrix3,
    projection_distance: u32,
) -> Result<PreparedObjects, RetailSceneError> {
    let mut prepared = PreparedObjects::default();
    for (render_index, object) in render_objects.iter().enumerate() {
        if !object.display_eligible {
            continue;
        }
        let Some(program) = object.program else {
            prepared.skipped_animations = prepared.skipped_animations.saturating_add(1);
            continue;
        };
        let Some(animation_source) = object.animation_source.as_ref() else {
            prepared.skipped_animations = prepared.skipped_animations.saturating_add(1);
            continue;
        };
        let global = typed_entry(nsf, nsd, program.global_eid(), 11, "GOOL object program")?;
        let animations = entry_item(global, nsf_bytes, 5, "GOOL object animations")?;
        match animation_source {
            AnimationSource::ItemFive(animation_reference) => {
                let descriptor = parse_gool_animation_descriptor(
                    animations,
                    usize::try_from(animation_reference.offset())
                        .map_err(|_| scene_error("GOOL animation offset does not fit the host"))?,
                )
                .map_err(|error| scene_error(format!("GOOL object animation: {error}")))?;
                match descriptor {
                    GoolAnimationDescriptor::Vertex(animation) => prepare_vertex_animation(
                        nsd,
                        nsf,
                        nsf_bytes,
                        model_cache,
                        model_lru,
                        zone_header,
                        object,
                        main_object,
                        camera,
                        raw_camera_matrix,
                        adjusted_camera_matrix,
                        projection_distance,
                        animation,
                        render_index,
                        &mut prepared,
                    )?,
                    GoolAnimationDescriptor::Sprite(animation) => prepare_sprite_animation(
                        object,
                        camera,
                        adjusted_camera_matrix,
                        projection_distance,
                        &animation,
                        render_index,
                        &mut prepared,
                    )?,
                    GoolAnimationDescriptor::Fragment(animation) => prepare_fragment_animation(
                        object,
                        camera,
                        adjusted_camera_matrix,
                        projection_distance,
                        &animation,
                        render_index,
                        &mut prepared,
                    )?,
                    GoolAnimationDescriptor::Text(animation) => prepare_text_animation(
                        animations,
                        object,
                        camera,
                        adjusted_camera_matrix,
                        projection_distance,
                        &animation,
                        render_index,
                        &mut prepared,
                    )?,
                    // Fonts are packed resources selected by type-four descriptors.
                    GoolAnimationDescriptor::Font(_) => {
                        prepared.skipped_animations = prepared.skipped_animations.saturating_add(1);
                    }
                }
            }
            AnimationSource::Process(process) => match process.kind() {
                ProcessAnimationKind::Vertex(animation) => prepare_vertex_animation(
                    nsd,
                    nsf,
                    nsf_bytes,
                    model_cache,
                    model_lru,
                    zone_header,
                    object,
                    main_object,
                    camera,
                    raw_camera_matrix,
                    adjusted_camera_matrix,
                    projection_distance,
                    *animation,
                    render_index,
                    &mut prepared,
                )?,
                ProcessAnimationKind::Sprite(animation) => prepare_sprite_animation(
                    object,
                    camera,
                    adjusted_camera_matrix,
                    projection_distance,
                    animation,
                    render_index,
                    &mut prepared,
                )?,
                ProcessAnimationKind::Fragment(animation) => prepare_fragment_animation(
                    object,
                    camera,
                    adjusted_camera_matrix,
                    projection_distance,
                    animation,
                    render_index,
                    &mut prepared,
                )?,
                ProcessAnimationKind::Text(animation) => prepare_process_text_animation(
                    animations,
                    object,
                    camera,
                    adjusted_camera_matrix,
                    projection_distance,
                    animation,
                    render_index,
                    &mut prepared,
                )?,
                // Native's type-three transform case is empty. Unknown/type-
                // zero aliases reach the switch default and are also no-draw.
                ProcessAnimationKind::Font(_) | ProcessAnimationKind::NoDraw => {
                    prepared.skipped_animations = prepared.skipped_animations.saturating_add(1);
                }
            },
        }
    }
    Ok(prepared)
}

#[allow(clippy::too_many_arguments)]
fn prepare_vertex_animation(
    nsd: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    model_cache: &mut HashMap<(Eid, u16), Arc<ObjectModelFrame>>,
    model_lru: &mut VecDeque<(Eid, u16)>,
    zone_header: &ZoneHeader,
    object: &RetailRenderObject,
    main_object: Option<RuntimeObjectHandle>,
    camera: CameraSample,
    raw_camera_matrix: Matrix3,
    adjusted_camera_matrix: Matrix3,
    projection_distance: u32,
    animation: GoolVertexAnimation,
    render_index: usize,
    prepared: &mut PreparedObjects,
) -> Result<(), RetailSceneError> {
    let Ok(frame_index) = u16::try_from(object.animation_frame >> 8) else {
        prepared.skipped_animations = prepared.skipped_animations.saturating_add(1);
        return Ok(());
    };

    // Retail NSLookup simply declines a dormant frame whose model is not
    // resident in the mounted pair. Never fall back to another pair with the
    // same EID.
    if nsd.pte(animation.model_eid).is_none() {
        prepared.skipped_animations = prepared.skipped_animations.saturating_add(1);
        return Ok(());
    }
    let vertex_entry = nsf
        .resolve_entry(nsd, animation.model_eid)
        .map_err(|error| scene_error(format!("GOOL object frame entry: {error}")))?;
    let vertex_kind = ObjectVertexKind::from_entry_type(vertex_entry.entry_type)
        .map_err(|error| scene_error(format!("GOOL object frame type: {error}")))?;
    let Some(frame_item) = vertex_entry.item(usize::from(frame_index)) else {
        prepared.skipped_animations = prepared.skipped_animations.saturating_add(1);
        return Ok(());
    };
    let frame = parse_object_frame(
        frame_item
            .bytes(nsf_bytes)
            .map_err(|error| scene_error(format!("GOOL object frame bytes: {error}")))?,
        vertex_kind,
    )
    .map_err(|error| scene_error(format!("GOOL object frame: {error}")))?;
    if nsd.pte(frame.header.geometry_eid).is_none() {
        prepared.skipped_animations = prepared.skipped_animations.saturating_add(1);
        return Ok(());
    }
    let cache_key = (animation.model_eid, frame_index);
    let model = if let Some(model) = model_cache.get(&cache_key) {
        touch_object_model_lru(model_lru, cache_key);
        Arc::clone(model)
    } else {
        let model = Arc::new(
            load_object_model_frame(nsd, nsf, nsf_bytes, animation.model_eid, frame_index)
                .map_err(|error| scene_error(format!("GOOL object model: {error}")))?,
        );
        while model_cache.len() >= RETAIL_OBJECT_MODEL_CACHE_FRAMES {
            let Some(evicted) = model_lru.pop_front() else {
                break;
            };
            model_cache.remove(&evicted);
        }
        model_cache.insert(cache_key, Arc::clone(&model));
        model_lru.push_back(cache_key);
        model
    };

    let is_2d_cvtx = model.frame.kind == ObjectVertexKind::Colored && object.status_b & 0x200 != 0;
    let (transform, colored_shift, lighting) = if is_2d_cvtx {
        let sprite =
            RetailSpriteTransform::screen_2d(object_sprite_vectors(object), 0, projection_distance)
                .map_err(|error| scene_error(format!("2D CVTX transform: {error}")))?;
        (
            ObjectProjectionTransform {
                matrix: sprite.matrix,
                translation: sprite.translation,
            },
            0,
            None,
        )
    } else {
        let relative = Vec3i {
            x: object.transform.translation[0].wrapping_sub(camera.translation_fixed[0]) >> 8,
            y: object.transform.translation[1].wrapping_sub(camera.translation_fixed[1]) >> 8,
            z: object.transform.translation[2].wrapping_sub(camera.translation_fixed[2]) >> 8,
        };
        let camera_translation = rotate(relative, adjusted_camera_matrix).point;
        // Retail's generic visibility-depth rejection is disabled in the
        // executable. Only the near-plane check here and the mode-two/three
        // shader cutoffs below may reject the object origin by depth.
        if object.display_mask & 0x1_0000 == 0
            && object.status_b & 0x4_0000 == 0
            && i32::try_from(projection_distance).unwrap_or(i32::MAX) >= camera_translation.z
        {
            return Ok(());
        }
        let is_main = main_object == Some(object.object);
        let mut effective_colors = object.colors;
        let mut colored_shift = 0;
        if object.display_mask & 0x1_0000 == 0
            && !is_main
            && object.status_b & 0x400 == 0
            && matches!(zone_header.graphics.unknown_a, 2..=4)
        {
            let dark = object
                .dark_reference_translation
                .map(|reference_translation| ObjectDarkShaderInput {
                    reference_translation,
                    object_translation: object.transform.translation,
                    dark_distance: object.dark_distance,
                });
            let Some(shading) = apply_object_zone_shader(
                zone_header.graphics.unknown_a,
                model.frame.kind,
                object.colors,
                zone_header.graphics.object_colors.words,
                camera_translation.z,
                object_zone_depth_anchor(nsd, zone_header),
                dark,
            )
            .map_err(|error| scene_error(format!("GOOL object zone shader: {error}")))?
            else {
                return Ok(());
            };
            effective_colors = shading.colors;
            colored_shift = shading.colored_shift;
        }
        (
            ObjectProjectionTransform::from_retail(
                raw_camera_matrix,
                object.transform.rotation_yxz,
                Vec3i {
                    x: object.transform.scale[0],
                    y: object.transform.scale[1],
                    z: object.transform.scale[2],
                },
                model.geometry.header.scale,
                camera_translation,
            ),
            colored_shift,
            Some(GoolObjectLighting {
                words: effective_colors,
                rotation_yxz: object.transform.rotation_yxz,
                scale_x: object.transform.scale[0],
            }),
        )
    };
    let ordering_far = object
        .size
        .wrapping_add(0x800)
        .wrapping_sub(i32::try_from(projection_distance / 2).unwrap_or(i32::MAX))
        .cast_unsigned();
    let projected = project_object_model(
        &model,
        transform,
        ObjectProjectionParameters {
            screen_offset: [0, 0],
            projection_distance,
            ordering_far,
            cull_face: object.transform.scale[0],
            colored_shift,
        },
        lighting,
    )
    .map_err(|error| scene_error(format!("GOOL object projection: {error}")))?;
    prepared.visible_objects = prepared.visible_objects.saturating_add(1);
    prepared.saturated_polygons = prepared
        .saturated_polygons
        .saturating_add(projected.skipped_saturated as usize);
    prepared.culled_polygons = prepared
        .culled_polygons
        .saturating_add(projected.skipped_culled as usize);
    prepared.objects.push(PreparedVertexObject {
        render_index,
        handle: u32::from(object.object.vm().get()),
        polygons: projected.polygons,
    });
    Ok(())
}

fn prepare_sprite_animation(
    object: &RetailRenderObject,
    camera: CameraSample,
    camera_matrix: Matrix3,
    projection_distance: u32,
    animation: &GoolSpriteAnimation,
    render_index: usize,
    prepared: &mut PreparedObjects,
) -> Result<(), RetailSceneError> {
    let frame_index = usize::try_from(object.animation_frame >> 8)
        .map_err(|_| scene_error("GOOL sprite frame index does not fit the host"))?;
    let Some(texture) = animation.frames.get(frame_index).copied() else {
        prepared.skipped_animations = prepared.skipped_animations.saturating_add(1);
        return Ok(());
    };
    let shrink = retail_sprite_shrink(object.transform.scale[0])
        .map_err(|error| scene_error(format!("GOOL sprite shrink: {error}")))?;
    let transform =
        object_sprite_transform(object, camera, camera_matrix, projection_distance, shrink)?;
    let half_size = retail_sprite_half_size(shrink)
        .map_err(|error| scene_error(format!("GOOL sprite half-size: {error}")))?;
    let ordering_far = object
        .size
        .wrapping_add(0x800)
        .wrapping_sub(i32::try_from(projection_distance / 2).unwrap_or(i32::MAX))
        .cast_unsigned();
    let Some(projected) =
        project_retail_sprite(transform, half_size, projection_distance, ordering_far)
    else {
        return Ok(());
    };
    prepared.visible_objects = prepared.visible_objects.saturating_add(1);
    prepared.quads.push(prepared_object_quad(
        object,
        render_index,
        0,
        animation.texture_page,
        texture,
        projected,
    ));
    Ok(())
}

fn prepare_fragment_animation(
    object: &RetailRenderObject,
    camera: CameraSample,
    camera_matrix: Matrix3,
    projection_distance: u32,
    animation: &GoolFragmentAnimation,
    render_index: usize,
    prepared: &mut PreparedObjects,
) -> Result<(), RetailSceneError> {
    let frame_index = usize::try_from(object.animation_frame >> 8)
        .map_err(|_| scene_error("GOOL fragment frame index does not fit the host"))?;
    let Some(fragments) = animation.frame(frame_index) else {
        prepared.skipped_animations = prepared.skipped_animations.saturating_add(1);
        return Ok(());
    };
    let shrink = retail_sprite_shrink(object.transform.scale[0])
        .map_err(|error| scene_error(format!("GOOL fragment shrink: {error}")))?;
    let transform =
        object_sprite_transform(object, camera, camera_matrix, projection_distance, shrink)?;
    let mut emitted = false;
    for (part, fragment) in fragments.iter().enumerate() {
        let part =
            u16::try_from(part).map_err(|_| scene_error("GOOL fragment part index exceeds u16"))?;
        let bounds = fragment
            .bounds
            .map(|value| scaled_fragment_bound(value, shrink))
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        let bounds: [i32; 4] = bounds
            .try_into()
            .map_err(|_| scene_error("GOOL fragment lost a validated bound"))?;
        let Some(projected) =
            project_retail_fragment(transform, bounds, projection_distance, object.size)
        else {
            continue;
        };
        prepared.quads.push(prepared_object_quad(
            object,
            render_index,
            part,
            animation.texture_page,
            fragment.texture,
            projected,
        ));
        emitted = true;
    }
    if emitted {
        prepared.visible_objects = prepared.visible_objects.saturating_add(1);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn prepare_text_animation(
    animations: &[u8],
    object: &RetailRenderObject,
    camera: CameraSample,
    camera_matrix: Matrix3,
    projection_distance: u32,
    animation: &GoolTextAnimation,
    render_index: usize,
    prepared: &mut PreparedObjects,
) -> Result<(), RetailSceneError> {
    let term_index = usize::try_from(object.animation_frame >> 8)
        .map_err(|_| scene_error("GOOL text term index does not fit the host"))?;
    let Some(term) = animation.terms.get(term_index) else {
        prepared.skipped_animations = prepared.skipped_animations.saturating_add(1);
        return Ok(());
    };
    prepare_text_term(
        animations,
        object,
        camera,
        camera_matrix,
        projection_distance,
        animation.font_word_offset,
        term,
        render_index,
        prepared,
    )
}

#[allow(clippy::too_many_arguments)]
fn prepare_process_text_animation(
    animations: &[u8],
    object: &RetailRenderObject,
    camera: CameraSample,
    camera_matrix: Matrix3,
    projection_distance: u32,
    animation: &ProcessTextAnimation,
    render_index: usize,
    prepared: &mut PreparedObjects,
) -> Result<(), RetailSceneError> {
    let term_index = usize::try_from(object.animation_frame >> 8)
        .map_err(|_| scene_error("GOOL process text term index does not fit the host"))?;
    let Some(term) = animation.terms.get(term_index) else {
        prepared.skipped_animations = prepared.skipped_animations.saturating_add(1);
        return Ok(());
    };
    prepare_text_term(
        animations,
        object,
        camera,
        camera_matrix,
        projection_distance,
        animation.font_word_offset,
        term,
        render_index,
        prepared,
    )
}

#[allow(clippy::too_many_arguments)]
fn prepare_text_term(
    animations: &[u8],
    object: &RetailRenderObject,
    camera: CameraSample,
    camera_matrix: Matrix3,
    projection_distance: u32,
    font_word_offset: u32,
    term: &[u8],
    render_index: usize,
    prepared: &mut PreparedObjects,
) -> Result<(), RetailSceneError> {
    let font = resolve_text_font(
        animations,
        font_word_offset,
        object.text_font_override_word_offset,
    )?;
    let shrink = retail_sprite_shrink(object.transform.scale[0])
        .map_err(|error| scene_error(format!("GOOL text shrink: {error}")))?;
    let transform =
        object_sprite_transform(object, camera, camera_matrix, projection_distance, shrink)?;
    let vertex_colors = std::array::from_fn(|vertex| {
        let start = 12 + vertex * 3;
        [
            object.colors[start],
            object.colors[start + 1],
            object.colors[start + 2],
        ]
    });
    let rendered = project_retail_text(RetailTextProjection {
        term,
        font: &font,
        negative_stack_arguments: &object.text_arguments,
        transform,
        shrink,
        projection_distance,
        object_size: object.size,
        center_by_width: object.status_b & 0x400 != 0,
        center_backdrop: object.status_b & 0x0400_0000 != 0,
        vertex_colors,
    })
    .map_err(|error| scene_error(format!("GOOL text rendering: {error}")))?;
    if !rendered.quads.is_empty() {
        prepared.visible_objects = prepared.visible_objects.saturating_add(1);
    }
    for quad in rendered.quads {
        prepared.quads.push(PreparedObjectQuad {
            render_index,
            handle: u32::from(object.object.vm().get()),
            part: quad.source_part,
            texture_page: font.texture_page,
            texture: TextureInfo2 {
                color: quad.texture.color,
                region: quad.texture.region,
            },
            projected: quad.projected,
            colors: quad.colors,
        });
    }
    Ok(())
}

fn resolve_text_font(
    animations: &[u8],
    default_font_word_offset: u32,
    override_font_word_offset: u32,
) -> Result<GoolFontAnimation, RetailSceneError> {
    let font_word_offset = if override_font_word_offset == 0 {
        default_font_word_offset
    } else {
        override_font_word_offset
    };
    let font_offset = usize::try_from(font_word_offset)
        .ok()
        .and_then(|offset| offset.checked_mul(4))
        .ok_or_else(|| scene_error("GOOL text font word offset exceeds the animation item"))?;
    match parse_gool_animation_descriptor(animations, font_offset)
        .map_err(|error| scene_error(format!("GOOL text font: {error}")))?
    {
        GoolAnimationDescriptor::Font(font) => Ok(font),
        _ => Err(scene_error(
            "GOOL text font override is not a font descriptor",
        )),
    }
}

fn object_sprite_vectors(object: &RetailRenderObject) -> RetailSpriteVectors {
    RetailSpriteVectors {
        translation: object.transform.translation,
        rotation_yxz: object.transform.rotation_yxz,
        scale: object.transform.scale,
    }
}

fn object_sprite_transform(
    object: &RetailRenderObject,
    camera: CameraSample,
    camera_matrix: Matrix3,
    projection_distance: u32,
    shrink: u8,
) -> Result<RetailSpriteTransform, RetailSceneError> {
    let vectors = object_sprite_vectors(object);
    if object.status_b & 0x200 != 0 {
        RetailSpriteTransform::screen_2d(vectors, shrink, projection_distance)
            .map_err(|error| scene_error(format!("2D GOOL sprite transform: {error}")))
    } else {
        // The immutable scene snapshot is taken against the completed camera
        // update, which is the browser runtime's `cam_prev` render sample.
        RetailSpriteTransform::world(
            vectors,
            RetailSpriteCamera {
                translation: camera.translation_fixed,
                rotation_yxz: [camera.rotation_y, camera.rotation_x, camera.rotation_z],
                matrix: camera_matrix,
            },
            shrink,
        )
        .map_err(|error| scene_error(format!("world GOOL sprite transform: {error}")))
    }
}

fn scaled_fragment_bound(value: i16, shrink: u8) -> Result<i32, RetailSceneError> {
    retail_sprite_shift_word(i32::from(value), shrink)
        .map_err(|error| scene_error(format!("GOOL fragment bound: {error}")))
}

fn prepared_object_quad(
    object: &RetailRenderObject,
    render_index: usize,
    part: u16,
    texture_page: Eid,
    texture: GoolTextureInfo,
    projected: ProjectedSpriteQuad,
) -> PreparedObjectQuad {
    PreparedObjectQuad {
        render_index,
        handle: u32::from(object.object.vm().get()),
        part,
        texture_page,
        texture: TextureInfo2 {
            color: texture.color,
            region: texture.region,
        },
        projected,
        colors: [Rgba8 {
            r: texture.color.red(),
            g: texture.color.green(),
            b: texture.color.blue(),
            a: u8::MAX,
        }; 4],
    }
}

fn touch_object_model_lru(lru: &mut VecDeque<(Eid, u16)>, key: (Eid, u16)) {
    if let Some(index) = lru.iter().position(|candidate| *candidate == key) {
        lru.remove(index);
    }
    lru.push_back(key);
}

fn object_zone_depth_anchor(nsd: &Nsd, zone_header: &ZoneHeader) -> i32 {
    let visibility = i32::try_from(zone_header.graphics.visibility_depth >> 8).unwrap_or(i32::MAX);
    if matches!(nsd.level().get(), 0x14 | 0x16) {
        // `fog_z` is zero in the current runtime, matching source LevelInit.
        visibility.wrapping_add(400)
    } else {
        visibility.wrapping_sub(if zone_header.graphics.unknown_b_to_e[0] == 0 {
            0
        } else {
            1200
        })
    }
}

fn resident_texture_pages(
    nsd: &Nsd,
    nsf: &Nsf,
    zone_header: &ZoneHeader,
) -> Result<BTreeSet<u32>, RetailSceneError> {
    let mut resident = BTreeSet::new();
    for page_index in &zone_header.load_list.pages {
        let page = nsf
            .pages
            .get(
                usize::try_from(page_index.get())
                    .map_err(|_| scene_error("ZDAT load-list page index does not fit the host"))?,
            )
            .ok_or_else(|| scene_error("ZDAT load-list page is outside the NSF"))?;
        if let NsfPage::Texture(texture) = page {
            resident.insert(texture.eid.raw());
        }
    }
    for eid in &zone_header.load_list.entries {
        let Some(pte) = nsd.pte(*eid) else {
            continue;
        };
        let Some(NsfPage::Texture(texture)) = nsf.pages.get(
            usize::try_from(pte.page_index().get())
                .map_err(|_| scene_error("ZDAT load-list EID page does not fit the host"))?,
        ) else {
            continue;
        };
        if texture.eid == *eid {
            resident.insert(texture.eid.raw());
        }
    }
    Ok(resident)
}

fn parse_scene_graph(
    nsd: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    key: RetailSceneCacheKey,
    path_point_index: usize,
    parse_map_paths: bool,
) -> Result<CachedSceneGraph, RetailSceneError> {
    let zone_entry = typed_entry(nsf, nsd, key.zone, ZDAT_ENTRY_TYPE, "active ZDAT")?;
    let zone_header = ZoneHeader::parse(entry_item(zone_entry, nsf_bytes, 0, "ZDAT header")?)
        .map_err(|error| scene_error(format!("active ZDAT header: {error}")))?;
    let zone_rect = ZoneRect::parse(entry_item(zone_entry, nsf_bytes, 1, "ZDAT rectangle")?)
        .map_err(|error| scene_error(format!("active ZDAT rectangle: {error}")))?;
    let path_item_index = zone_header
        .path_item_index(key.path_index)
        .ok_or_else(|| scene_error("active path index is outside its ZDAT"))?;
    let path_item_index = usize::try_from(path_item_index)
        .map_err(|_| scene_error("ZDAT spawn path index does not fit the host"))?;
    let path = ZonePath::parse(entry_item(
        zone_entry,
        nsf_bytes,
        path_item_index,
        "ZDAT spawn path",
    )?)
    .map_err(|error| scene_error(format!("ZDAT spawn path: {error}")))?;
    if path_point_index >= path.points.len() {
        return Err(scene_error("active path progress is outside its ZDAT path"));
    }

    // LevelUpdate deliberately does not open the SLST when the zone has no
    // worlds. Keep that exact boundary in the cached representation too.
    if zone_header.worlds.is_empty() {
        return Ok(CachedSceneGraph {
            key,
            zone_header,
            zone_rect,
            path,
            visibility: None,
            world_eids: Vec::new(),
            worlds: Vec::new(),
            map_paths_parsed: parse_map_paths,
            world_map_paths: Vec::new(),
            world_map_path_masks: Vec::new(),
        });
    }

    let slst_entry = typed_entry(
        nsf,
        nsd,
        path.visibility_list,
        SLST_ENTRY_TYPE,
        "spawn SLST",
    )?;
    let expected_slst_items = path
        .points
        .len()
        .checked_add(1)
        .ok_or_else(|| scene_error("spawn path item count overflows"))?;
    if slst_entry.items.len() != expected_slst_items {
        return Err(scene_error(format!(
            "spawn SLST has {} items; expected {expected_slst_items}",
            slst_entry.items.len()
        )));
    }
    let slst_items = slst_entry
        .items
        .iter()
        .enumerate()
        .map(|(item_index, _)| {
            SlstItem::parse(entry_item(
                slst_entry,
                nsf_bytes,
                item_index,
                "spawn SLST item",
            )?)
            .map_err(|error| scene_error(format!("spawn SLST item {item_index}: {error}")))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut worlds = Vec::with_capacity(zone_header.worlds.len());
    let mut world_eids = Vec::with_capacity(zone_header.worlds.len());
    let mut world_map_paths = Vec::with_capacity(zone_header.worlds.len());
    for (world_index, world) in zone_header.worlds.iter().enumerate() {
        let entry = typed_entry(nsf, nsd, world.geometry, WGEO_ENTRY_TYPE, "spawn WGEO")?;
        let geometry = parse_world_geometry(
            entry_item(entry, nsf_bytes, 0, "WGEO header")?,
            entry_item(entry, nsf_bytes, 1, "WGEO polygons")?,
            entry_item(entry, nsf_bytes, 2, "WGEO vertices")?,
        )
        .map_err(|error| scene_error(format!("spawn WGEO {world_index}: {error}")))?;
        let map_paths = if parse_map_paths && entry.items.len() >= 4 {
            Some(
                WorldMapPathList::parse(entry_item(entry, nsf_bytes, 3, "WGEO map paths")?)
                    .map_err(|error| {
                        scene_error(format!("spawn WGEO {world_index} map paths: {error}"))
                    })?,
            )
        } else {
            None
        };
        world_eids.push(world.geometry);
        worlds.push(Arc::new(geometry));
        world_map_paths.push(map_paths);
    }
    let world_polygon_counts = worlds
        .iter()
        .map(|world| world.polygons.len())
        .collect::<Vec<_>>();
    let world_map_path_masks = empty_world_map_path_masks(&world_polygon_counts);
    let visibility = SlstCursor::new(&slst_items, &world_polygon_counts, path_point_index)
        .map_err(|error| scene_error(format!("spawn SLST state: {error}")))?;

    Ok(CachedSceneGraph {
        key,
        zone_header,
        zone_rect,
        path,
        visibility: Some(visibility),
        world_eids,
        worlds,
        map_paths_parsed: parse_map_paths,
        world_map_paths,
        world_map_path_masks,
    })
}

fn parse_full_level_scene_graph(
    nsd: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
) -> Result<FullLevelSceneGraph, RetailSceneError> {
    let ldat = nsd
        .ldat()
        .ok_or_else(|| scene_error("index-only NSD has no complete level graph"))?;
    let mut queued = BTreeSet::from([ldat.spawn_zone]);
    let mut queue = VecDeque::from([ldat.spawn_zone]);
    let mut world_eids = BTreeSet::new();
    let mut worlds = Vec::new();
    let mut polygon_count = 0_usize;

    while let Some(zone_eid) = queue.pop_front() {
        let zone_entry = typed_entry(nsf, nsd, zone_eid, ZDAT_ENTRY_TYPE, "complete-level ZDAT")?;
        let zone_header = ZoneHeader::parse(entry_item(
            zone_entry,
            nsf_bytes,
            0,
            "complete-level ZDAT header",
        )?)
        .map_err(|error| scene_error(format!("complete-level ZDAT {zone_eid} header: {error}")))?;
        for neighbor in zone_header.neighbors.iter().copied() {
            if queued.insert(neighbor) {
                queue.push_back(neighbor);
            }
        }
        for zone_world in &zone_header.worlds {
            if !world_eids.insert(zone_world.geometry) {
                continue;
            }
            if world_eids.len() > PRESENTATION_WORLD_LIMIT {
                return Err(scene_error(format!(
                    "complete level references more than {PRESENTATION_WORLD_LIMIT} unique WGEOs"
                )));
            }
            let entry = typed_entry(
                nsf,
                nsd,
                zone_world.geometry,
                WGEO_ENTRY_TYPE,
                "complete-level WGEO",
            )?;
            let geometry = parse_world_geometry(
                entry_item(entry, nsf_bytes, 0, "complete-level WGEO header")?,
                entry_item(entry, nsf_bytes, 1, "complete-level WGEO polygons")?,
                entry_item(entry, nsf_bytes, 2, "complete-level WGEO vertices")?,
            )
            .map_err(|error| {
                scene_error(format!(
                    "complete-level WGEO {}: {error}",
                    zone_world.geometry
                ))
            })?;
            // A backdrop WGEO contains camera-authored alternatives. Those
            // continue to come from the active SLST below; including every
            // backdrop in the complete graph would overlap unrelated skies.
            if geometry.header.is_backdrop {
                continue;
            }
            polygon_count = polygon_count
                .checked_add(geometry.polygons.len())
                .ok_or_else(|| scene_error("complete-level polygon count overflows"))?;
            if polygon_count > PRESENTATION_POLYGON_LIMIT {
                return Err(scene_error(format!(
                    "complete level exceeds the {PRESENTATION_POLYGON_LIMIT}-polygon presentation limit"
                )));
            }
            worlds.push(CachedWorldGeometry {
                eid: zone_world.geometry,
                geometry: Arc::new(geometry),
            });
        }
    }
    Ok(FullLevelSceneGraph {
        level: nsd.level(),
        worlds,
    })
}

fn select_scene_worlds(
    graph: &CachedSceneGraph,
    retail_visible: &[PolygonId],
    full_level_graph: Option<&FullLevelSceneGraph>,
    location: RetailSceneProgressLocation,
    extended_world: bool,
) -> Result<(Vec<SceneWorld>, Vec<SelectedWorldPolygon>), RetailSceneError> {
    if !extended_world {
        let worlds = graph
            .worlds
            .iter()
            .cloned()
            .enumerate()
            .map(|(active_index, geometry)| SceneWorld {
                geometry,
                active_index: Some(active_index),
            })
            .collect::<Vec<_>>();
        let polygons = retail_visible
            .iter()
            .copied()
            .map(|polygon| SelectedWorldPolygon {
                world_index: usize::from(polygon.world_index),
                polygon_index: usize::from(polygon.polygon_index),
                source: CommandSource::World {
                    zone: location.zone.raw(),
                    polygon: u32::from(polygon.raw()),
                },
                retail_authored: true,
            })
            .collect();
        return Ok((worlds, polygons));
    }

    // Retail ZDAT zones may deliberately reuse the same world coordinate
    // space. The full reachable graph is therefore retained as a preload, but
    // only the active authored zone is submitted. Within that zone we ignore
    // the ordinary SLST polygon subset so a wider viewport can reveal all
    // local geometry without waking objects or mixing future course sections.
    let worlds = graph
        .world_eids
        .iter()
        .copied()
        .zip(graph.worlds.iter())
        .enumerate()
        .map(|(active_index, (eid, geometry))| SceneWorld {
            geometry: full_level_graph
                .and_then(|full_graph| {
                    full_graph
                        .worlds
                        .iter()
                        .find(|cached| cached.eid == eid)
                        .map(|cached| Arc::clone(&cached.geometry))
                })
                .unwrap_or_else(|| Arc::clone(geometry)),
            active_index: Some(active_index),
        })
        .collect::<Vec<_>>();

    let retail_authored = retail_visible
        .iter()
        .filter_map(|polygon| {
            let active_index = usize::from(polygon.world_index);
            let geometry = graph.worlds.get(active_index)?;
            (!geometry.header.is_backdrop)
                .then(|| (active_index, usize::from(polygon.polygon_index)))
        })
        .collect::<BTreeSet<_>>();
    let mut polygons = Vec::new();
    for (world_index, world) in worlds.iter().enumerate() {
        if world.geometry.header.is_backdrop {
            continue;
        }
        for polygon_index in 0..world.geometry.polygons.len() {
            let source_polygon = presentation_polygon_source(world_index, polygon_index)?;
            polygons.push(SelectedWorldPolygon {
                world_index,
                polygon_index,
                source: CommandSource::World {
                    zone: location.zone.raw(),
                    polygon: source_polygon,
                },
                retail_authored: retail_authored.contains(&(world_index, polygon_index)),
            });
        }
    }

    for polygon in retail_visible {
        let active_index = usize::from(polygon.world_index);
        let geometry = graph
            .worlds
            .get(active_index)
            .ok_or_else(|| scene_error("SLST references an inactive backdrop world slot"))?;
        if !geometry.header.is_backdrop {
            continue;
        }
        polygons.push(SelectedWorldPolygon {
            world_index: active_index,
            polygon_index: usize::from(polygon.polygon_index),
            source: CommandSource::World {
                zone: location.zone.raw(),
                polygon: presentation_polygon_source(
                    active_index,
                    usize::from(polygon.polygon_index),
                )?,
            },
            retail_authored: true,
        });
    }
    if polygons.len() > PRESENTATION_POLYGON_LIMIT {
        return Err(scene_error(format!(
            "complete level exceeds the {PRESENTATION_POLYGON_LIMIT}-polygon presentation limit"
        )));
    }
    Ok((worlds, polygons))
}

fn presentation_polygon_source(
    active_world_index: usize,
    polygon_index: usize,
) -> Result<u32, RetailSceneError> {
    let world = u32::try_from(active_world_index)
        .map_err(|_| scene_error("presentation world slot does not fit u32"))?;
    let polygon = u32::try_from(polygon_index)
        .map_err(|_| scene_error("presentation polygon index does not fit u32"))?;
    if world > u32::from(u8::MAX) || polygon > 0x00ff_ffff {
        return Err(scene_error(
            "presentation world/polygon identity exceeds its checked packing",
        ));
    }
    Ok((world << 24) | polygon)
}

fn update_persistent_world_map_path_masks(
    graph: &mut CachedSceneGraph,
    animation: Option<RetailMapPathAnimation>,
) -> Result<(), RetailSceneError> {
    let polygon_counts = graph
        .worlds
        .iter()
        .map(|world| world.polygons.len())
        .collect::<Vec<_>>();
    update_persistent_world_map_path_masks_for_counts(
        &mut graph.world_map_path_masks,
        &polygon_counts,
        &graph.world_map_paths,
        animation,
    )
}

fn active_map_path_animation(
    level: crust_formats::stream::LevelId,
    animation: Option<RetailMapPathAnimation>,
) -> Option<RetailMapPathAnimation> {
    animation.filter(|animation| {
        level == crust_formats::stream::LevelId::TITLE && animation.title_state == 15
    })
}

fn scene_graph_cache_matches(
    cached_key: Option<RetailSceneCacheKey>,
    cached_map_paths_parsed: bool,
    requested_key: RetailSceneCacheKey,
    needs_map_paths: bool,
) -> bool {
    cached_key == Some(requested_key) && (!needs_map_paths || cached_map_paths_parsed)
}

fn empty_world_map_path_masks(polygon_counts: &[usize]) -> Vec<Vec<Option<u8>>> {
    polygon_counts
        .iter()
        .map(|polygon_count| vec![None; *polygon_count])
        .collect()
}

fn update_persistent_world_map_path_masks_for_counts(
    persistent_masks: &mut Vec<Vec<Option<u8>>>,
    polygon_counts: &[usize],
    world_map_paths: &[Option<WorldMapPathList>],
    animation: Option<RetailMapPathAnimation>,
) -> Result<(), RetailSceneError> {
    if polygon_counts.len() != world_map_paths.len() {
        return Err(scene_error(
            "WGEO map-path list count does not match the active world count",
        ));
    }
    if persistent_masks.len() != polygon_counts.len()
        || persistent_masks
            .iter()
            .zip(polygon_counts)
            .any(|(masks, polygon_count)| masks.len() != *polygon_count)
    {
        return Err(scene_error(
            "WGEO map-path mask sidecar does not match the active world layout",
        ));
    }
    let Some(animation) = animation.filter(|animation| animation.title_state == 15) else {
        // Native does not restore serialized WGEO masks when title state 15
        // ends. Retain the last sidecar writes until this graph is replaced.
        return Ok(());
    };

    let mut masks = empty_world_map_path_masks(polygon_counts);

    let mut active_group = 0_u16;
    for (world_index, ((polygon_count, map_paths), world_masks)) in polygon_counts
        .iter()
        .zip(world_map_paths)
        .zip(&mut masks)
        .enumerate()
    {
        let Some(map_paths) = map_paths else {
            continue;
        };
        let overrides = map_paths
            .mask_overrides(
                *polygon_count,
                &mut active_group,
                animation.map_level_links,
                animation.map_key_links,
            )
            .map_err(|error| scene_error(format!("spawn WGEO {world_index} map paths: {error}")))?;
        for path_override in overrides {
            world_masks[usize::from(path_override.polygon_index)] =
                Some(path_override.animation_mask);
        }
    }
    *persistent_masks = masks;
    Ok(())
}

fn polygon_with_map_path_mask(
    mut polygon: crust_formats::stream::WorldPolygon,
    animation_mask: Option<u8>,
) -> crust_formats::stream::WorldPolygon {
    if let Some(animation_mask) = animation_mask {
        polygon.animation_mask = animation_mask;
    }
    polygon
}

fn install_missing_texture_pages(
    texture_cache: &mut TextureCache,
    texture_pages: &mut [Option<u32>; RETAIL_TEXTURE_PAGE_SLOTS],
    texture_page_generations: &mut [Option<u32>; RETAIL_TEXTURE_PAGE_SLOTS],
    diagnostics: &mut RetailSceneCacheDiagnostics,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    required_pages: &BTreeSet<u32>,
) -> Result<(), RetailSceneError> {
    for raw_eid in required_pages.iter().copied() {
        if texture_pages.contains(&Some(raw_eid)) {
            continue;
        }
        let slot = texture_pages
            .iter()
            .position(Option::is_none)
            .or_else(|| {
                texture_pages.iter().position(|page| {
                    page.is_some_and(|resident| !required_pages.contains(&resident))
                })
            })
            .ok_or_else(|| scene_error("retail texture slots have no replaceable page"))?;
        let reference = TpagReference::new(Eid::from_raw(raw_eid));
        let page = resolve_texture_page(nsf, nsf_bytes, reference)
            .map_err(|error| scene_error(format!("spawn TPAG: {error}")))?;
        texture_cache
            .install_page(slot, raw_eid, page.bytes().to_vec())
            .map_err(|error| scene_error(format!("install spawn TPAG: {error}")))?;
        texture_pages[slot] = Some(raw_eid);
        texture_page_generations[slot] = None;
        diagnostics.texture_page_installs = diagnostics.texture_page_installs.saturating_add(1);
    }
    Ok(())
}

fn texture_snapshot_page_eids(snapshot: TextureFrameSnapshot) -> BTreeSet<u32> {
    snapshot
        .slots()
        .iter()
        .filter_map(|binding| binding.map(|binding| binding.eid.raw()))
        .collect()
}

fn effective_object_texture_snapshot(
    frame_snapshot: Option<TextureFrameSnapshot>,
    object_snapshot: Option<TextureFrameSnapshot>,
) -> Option<TextureFrameSnapshot> {
    frame_snapshot.map(|frame_snapshot| object_snapshot.unwrap_or(frame_snapshot))
}

fn install_texture_frame_snapshot(
    texture_cache: &mut TextureCache,
    texture_pages: &mut [Option<u32>; RETAIL_TEXTURE_PAGE_SLOTS],
    texture_page_generations: &mut [Option<u32>; RETAIL_TEXTURE_PAGE_SLOTS],
    diagnostics: &mut RetailSceneCacheDiagnostics,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    snapshot: TextureFrameSnapshot,
) -> Result<(), RetailSceneError> {
    for slot in 0..RETAIL_TEXTURE_PAGE_SLOTS {
        let desired = snapshot.slot(slot);
        let desired_identity = desired.map(|binding| (binding.eid.raw(), binding.generation));
        let installed_identity = texture_pages[slot].zip(texture_page_generations[slot]);
        if installed_identity == desired_identity {
            continue;
        }

        let Some(binding) = desired else {
            texture_cache
                .remove_page(slot)
                .map_err(|error| scene_error(format!("remove retail TPAG slot {slot}: {error}")))?;
            texture_pages[slot] = None;
            texture_page_generations[slot] = None;
            continue;
        };
        let reference = TpagReference::new(binding.eid);
        let page = resolve_texture_page(nsf, nsf_bytes, reference).map_err(|error| {
            scene_error(format!("resolve retail TPAG {}: {error}", binding.eid))
        })?;
        if page.page_index != binding.page {
            return Err(scene_error(format!(
                "retail TPAG {} snapshot names page {}, but mounted bytes resolve page {}",
                binding.eid,
                binding.page.get(),
                page.page_index.get(),
            )));
        }
        texture_cache
            .install_page(slot, binding.eid.raw(), page.bytes().to_vec())
            .map_err(|error| scene_error(format!("install retail TPAG slot {slot}: {error}")))?;
        texture_pages[slot] = Some(binding.eid.raw());
        texture_page_generations[slot] = Some(binding.generation);
        diagnostics.texture_page_installs = diagnostics.texture_page_installs.saturating_add(1);
    }
    Ok(())
}

fn typed_entry<'a>(
    nsf: &'a Nsf,
    nsd: &Nsd,
    eid: Eid,
    expected_type: u32,
    context: &str,
) -> Result<&'a Entry, RetailSceneError> {
    let entry = nsf
        .resolve_entry(nsd, eid)
        .map_err(|error| scene_error(format!("{context} {eid}: {error}")))?;
    if entry.entry_type != expected_type {
        return Err(scene_error(format!(
            "{context} {eid} has type {}; expected {expected_type}",
            entry.entry_type
        )));
    }
    Ok(entry)
}

fn entry_item<'a>(
    entry: &Entry,
    nsf_bytes: &'a [u8],
    index: usize,
    context: &str,
) -> Result<&'a [u8], RetailSceneError> {
    entry
        .item(index)
        .ok_or_else(|| scene_error(format!("{context} item {index} is absent")))?
        .bytes(nsf_bytes)
        .map_err(|error| scene_error(format!("{context} item {index}: {error}")))
}

fn validate_visibility(
    visible: &[PolygonId],
    worlds: &[Arc<WorldGeometry>],
) -> Result<(), RetailSceneError> {
    for polygon in visible {
        let world = worlds
            .get(usize::from(polygon.world_index))
            .ok_or_else(|| scene_error("SLST references an inactive world slot"))?;
        if usize::from(polygon.polygon_index) >= world.polygons.len() {
            return Err(scene_error("SLST references a polygon outside its WGEO"));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CameraSample {
    translation: Vec3i,
    translation_fixed: [i32; 3],
    rotation_y: i32,
    rotation_x: i32,
    rotation_z: i32,
}

fn camera_sample_from_pose(pose: RetailSceneCameraPose) -> CameraSample {
    CameraSample {
        translation: Vec3i {
            x: pose.translation[0] >> 8,
            y: pose.translation[1] >> 8,
            z: pose.translation[2] >> 8,
        },
        translation_fixed: pose.translation,
        rotation_y: pose.rotation_yxz[0],
        rotation_x: pose.rotation_yxz[1],
        rotation_z: pose.rotation_yxz[2],
    }
}

fn object_camera_sample(
    camera: CameraSample,
    graphics_flags: u32,
    frame_stamp: u32,
) -> CameraSample {
    if graphics_flags & 0x1000 == 0 {
        return camera;
    }
    // GfxInitMatrices seeds cam_prev to (0, 921600, 6144000). In these
    // zones GfxUpdateMatrices deliberately retains X/Z, replaces Y with a
    // 128-frame triangular path, and substitutes a fixed positive-125 matrix
    // for GOOL objects only. `raw_camera_matrix` negates stored camera angles,
    // so the pointer-free sample retains the inverse scalar. World geometry
    // continues to use `camera` above.
    let phase = i32::try_from(frame_stamp % 128).expect("a modulo-128 frame fits i32");
    let y = 901_600 + (phase - 64).abs() * 800;
    let translation_fixed = [0, y, 6_144_000];
    CameraSample {
        translation: Vec3i {
            x: translation_fixed[0] >> 8,
            y: translation_fixed[1] >> 8,
            z: translation_fixed[2] >> 8,
        },
        translation_fixed,
        rotation_y: -125,
        rotation_x: 0,
        rotation_z: 0,
    }
}

fn object_camera_sample_for_location(
    camera: CameraSample,
    graphics_flags: u32,
    location: RetailSceneProgressLocation,
) -> CameraSample {
    object_camera_sample(camera, graphics_flags, location.frame_stamp)
}

fn sample_camera(
    nsd: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    zone_header: &ZoneHeader,
    zone_rect: &ZoneRect,
    path: &ZonePath,
    progress: i32,
) -> Result<CameraSample, RetailSceneError> {
    let magnitude = progress
        .checked_abs()
        .ok_or_else(|| scene_error("signed path progress cannot be i32::MIN"))?;
    let point_index = usize::try_from(magnitude >> 8)
        .map_err(|_| scene_error("camera path point index does not fit the host"))?;
    let point = path
        .points
        .get(point_index)
        .copied()
        .ok_or_else(|| scene_error("camera progress is outside the active path"))?;
    let fraction = magnitude & 0xff;

    let mut next_point = path.points[point_index.saturating_add(1).min(path.points.len() - 1)];
    let mut next_origin = zone_rect.origin;

    if fraction != 0
        && point_index == path.points.len() - 1
        && let Some(neighbor_path) = path
            .neighbors
            .iter()
            .find(|neighbor| neighbor.relation & 2 != 0)
    {
        let neighbor_zone = *zone_header
            .neighbors
            .get(usize::from(neighbor_path.neighbor_zone_index))
            .ok_or_else(|| scene_error("following path references a missing neighbor zone"))?;
        let neighbor_entry =
            typed_entry(nsf, nsd, neighbor_zone, ZDAT_ENTRY_TYPE, "following ZDAT")?;
        let neighbor_header = ZoneHeader::parse(entry_item(
            neighbor_entry,
            nsf_bytes,
            0,
            "following ZDAT header",
        )?)
        .map_err(|error| scene_error(format!("following ZDAT header: {error}")))?;
        let neighbor_rect = ZoneRect::parse(entry_item(
            neighbor_entry,
            nsf_bytes,
            1,
            "following ZDAT rectangle",
        )?)
        .map_err(|error| scene_error(format!("following ZDAT rectangle: {error}")))?;
        let neighbor_item_index = neighbor_header
            .path_item_index(u32::from(neighbor_path.path_index))
            .ok_or_else(|| scene_error("following path index is outside its ZDAT"))?;
        let neighbor_item_index = usize::try_from(neighbor_item_index)
            .map_err(|_| scene_error("following path item index does not fit the host"))?;
        let neighbor = ZonePath::parse(entry_item(
            neighbor_entry,
            nsf_bytes,
            neighbor_item_index,
            "following ZDAT path",
        )?)
        .map_err(|error| scene_error(format!("following ZDAT path: {error}")))?;

        // Retail deliberately refuses to interpolate into camera-mode-one
        // paths, falling back to the current path's final point.
        if neighbor.camera_mode != 1 {
            let next_index = if neighbor_path.goal & 2 != 0 {
                neighbor.points.len() - 1
            } else {
                0
            };
            next_point = neighbor.points[next_index];
            next_origin = neighbor_rect.origin;
        }
    }

    interpolate_camera(zone_rect.origin, point, next_origin, next_point, fraction)
}

fn interpolate_camera(
    origin: [i32; 3],
    point: ZonePathPoint,
    next_origin: [i32; 3],
    next: ZonePathPoint,
    fraction: i32,
) -> Result<CameraSample, RetailSceneError> {
    let current_coordinates = [
        path_coordinate(origin[0], point.x)?,
        path_coordinate(origin[1], point.y)?,
        path_coordinate(origin[2], point.z)?,
    ];
    let next_coordinates = [
        path_coordinate(next_origin[0], next.x)?,
        path_coordinate(next_origin[1], next.y)?,
        path_coordinate(next_origin[2], next.z)?,
    ];
    let translation_fixed = [
        interpolate_coordinate_fixed(current_coordinates[0], next_coordinates[0], fraction)?,
        interpolate_coordinate_fixed(current_coordinates[1], next_coordinates[1], fraction)?,
        interpolate_coordinate_fixed(current_coordinates[2], next_coordinates[2], fraction)?,
    ];
    let translation = Vec3i {
        x: translation_fixed[0] >> 8,
        y: translation_fixed[1] >> 8,
        z: translation_fixed[2] >> 8,
    };
    let yaw_difference = i32::from(
        Angle12::new(i32::from(point.rotation_y))
            .difference_to(Angle12::new(i32::from(next.rotation_y))),
    );
    Ok(CameraSample {
        translation,
        translation_fixed,
        rotation_y: i32::from(point.rotation_y) + ((yaw_difference * fraction) >> 8),
        rotation_x: interpolate_rotation(point.rotation_x, next.rotation_x, fraction),
        rotation_z: interpolate_rotation(point.rotation_z, next.rotation_z, fraction),
    })
}

fn path_coordinate(origin: i32, point: i16) -> Result<i32, RetailSceneError> {
    origin
        .checked_add(i32::from(point))
        .ok_or_else(|| scene_error("camera path coordinate overflows signed world space"))
}

#[cfg(test)]
fn interpolate_coordinate(current: i32, next: i32, fraction: i32) -> Result<i32, RetailSceneError> {
    Ok(interpolate_coordinate_fixed(current, next, fraction)? >> 8)
}

fn interpolate_coordinate_fixed(
    current: i32,
    next: i32,
    fraction: i32,
) -> Result<i32, RetailSceneError> {
    debug_assert!((0..=0xff).contains(&fraction));
    let fixed = i64::from(current)
        .checked_shl(8)
        .and_then(|base| {
            i64::from(next)
                .checked_sub(i64::from(current))
                .and_then(|delta| delta.checked_mul(i64::from(fraction)))
                .and_then(|delta| base.checked_add(delta))
        })
        .ok_or_else(|| scene_error("interpolated camera coordinate overflows fixed space"))?;
    i32::try_from(fixed)
        .map_err(|_| scene_error("interpolated fixed camera coordinate exceeds signed Q24.8 space"))
}

fn interpolate_rotation(current: i16, next: i16, fraction: i32) -> i32 {
    i32::from(current) + (((i32::from(next) - i32::from(current)) * fraction) >> 8)
}

fn projection_distance(field_of_view: u32) -> Result<u32, RetailSceneError> {
    match field_of_view {
        30 => Ok(960),
        37 => Ok(800),
        55 => Ok(500),
        60 => Ok(460),
        90 => Ok(288),
        value => Err(scene_error(format!("unsupported retail FOV {value}"))),
    }
}

fn retail_ripple_rate(level: crust_formats::stream::LevelId) -> (i32, i32) {
    match level.get() {
        // Upstream, Ripper Roo and Up the Creek.
        0x0f | 0x17 | 0x18 => (10, 127),
        // Tawna bonus rooms one and two.
        0x24 | 0x33 => (4, 127),
        _ => (1, 23),
    }
}

fn blend_mode(raw: u8) -> BlendMode {
    match raw & 3 {
        0 => BlendMode::Average,
        1 => BlendMode::Additive,
        2 => BlendMode::Subtractive,
        _ => BlendMode::Opaque,
    }
}

/// Modern presentation projection without the PSX screen/depth saturation
/// registers. The camera transform remains the same fixed-point transform;
/// only the final quotient retains the full logical output range so far WGEOs
/// cannot collapse into giant `±1024` panels before viewport classification.
fn project_presentation(
    point: Vec3i,
    translation: Vec3i,
    matrix: Matrix3,
    projection_distance: u32,
) -> ProjectionResult {
    let camera = rotate_translate(point, translation, matrix).point;
    let (x, x_valid) = presentation_project_axis(camera.x, camera.z, projection_distance);
    let (y, y_valid) = presentation_project_axis(camera.y, camera.z, projection_distance);
    ProjectionResult {
        camera,
        screen: crust_renderer::command::ScreenPoint { x, y, z: camera.z },
        valid: camera.z > 0 && x_valid && y_valid,
    }
}

/// Backdrop WGEOs are camera-authored to cover the 4:3 retail framebuffer.
///
/// Wider presentation viewports reveal real world geometry horizontally, but
/// there is no additional authored sky beyond those framebuffer bounds.
/// Expanding backdrop screen X around the viewport center fills that newly
/// revealed area without changing ordinary world projection or simulation.
/// A small horizontal overscan keeps the irregular authored triangle boundary
/// outside 16:9 and 21:9 even when the original sky did not quite reach x=±256.
fn expand_backdrop_to_viewport(
    mut screen: crust_renderer::command::ScreenPoint,
    viewport: Viewport,
) -> crust_renderer::command::ScreenPoint {
    let source_center = i64::from(Viewport::PSX.x) + i64::from(Viewport::PSX.width) / 2;
    let target_center = i64::from(viewport.x) + i64::from(viewport.width) / 2;
    let centered = i64::from(screen.x).saturating_sub(source_center);
    let overscan = i64::from(u8::from(viewport.width > Viewport::PSX.width));
    let scaled = centered
        .saturating_mul(i64::from(viewport.width))
        .saturating_mul(8 + overscan)
        .checked_div(i64::from(Viewport::PSX.width).saturating_mul(8))
        .unwrap_or(centered)
        .saturating_add(target_center);
    screen.x = i32::try_from(scaled).unwrap_or_else(|_| {
        if scaled.is_negative() {
            i32::MIN
        } else {
            i32::MAX
        }
    });
    screen
}

fn presentation_project_axis(value: i32, depth: i32, projection_distance: u32) -> (i32, bool) {
    if depth <= 0 {
        return (
            if value.is_negative() {
                i32::MIN
            } else {
                i32::MAX
            },
            false,
        );
    }
    let projected =
        i64::from(value).saturating_mul(i64::from(projection_distance)) / i64::from(depth);
    match i32::try_from(projected) {
        Ok(projected) => (projected, true),
        Err(_) => (
            if projected.is_negative() {
                i32::MIN
            } else {
                i32::MAX
            },
            false,
        ),
    }
}

fn raw_camera_matrix(rotation_y: i32, rotation_x: i32, rotation_z: i32) -> Matrix3 {
    let angle = |value: i32| Angle12::new(-value);
    let z = angle(rotation_z);
    let y_stored = angle(rotation_y);
    let x_stored = angle(rotation_x);
    Matrix3 {
        values: [
            [z.cos_q12(), wrapping_i16(-i32::from(z.sin_q12())), 0],
            [z.sin_q12(), z.cos_q12(), 0],
            [0, 0, 0x1000],
        ],
    }
    .multiply(Matrix3 {
        values: [
            [0x1000, 0, 0],
            [
                0,
                y_stored.cos_q12(),
                wrapping_i16(-i32::from(y_stored.sin_q12())),
            ],
            [0, y_stored.sin_q12(), y_stored.cos_q12()],
        ],
    })
    .multiply(Matrix3 {
        values: [
            [x_stored.cos_q12(), 0, x_stored.sin_q12()],
            [0, 0x1000, 0],
            [
                wrapping_i16(-i32::from(x_stored.sin_q12())),
                0,
                x_stored.cos_q12(),
            ],
        ],
    })
}

fn adjusted_camera_matrix(mut matrix: Matrix3) -> Matrix3 {
    for column in 0..3 {
        matrix.values[1][column] = wrapping_i16((-5 * i32::from(matrix.values[1][column])) >> 3);
        matrix.values[2][column] = wrapping_i16(-i32::from(matrix.values[2][column]));
    }
    matrix
}

#[cfg(test)]
fn world_camera_matrix(rotation_y: i32, rotation_x: i32, rotation_z: i32) -> Matrix3 {
    adjusted_camera_matrix(raw_camera_matrix(rotation_y, rotation_x, rotation_z))
}

fn wrapping_i16(value: i32) -> i16 {
    let bytes = value.to_le_bytes();
    i16::from_le_bytes([bytes[0], bytes[1]])
}

fn scene_error(message: impl Into<String>) -> RetailSceneError {
    RetailSceneError(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crust_formats::binary::PageIndex;
    use crust_formats::disc::DiscImage;
    use crust_formats::stream::{
        KNOWN_LEVELS, LevelId, RetailPathId, RetailZoneGraph, StreamKind, StreamName, ZoneEntity,
        parse_nsd, parse_nsf,
    };
    use crust_renderer::texture::{ColorMode, TextureRegion};
    use crust_sim::camera::{
        RetailCameraEffect, RetailCameraFollowInput, RetailCameraInput, RetailCameraLocation,
        RetailCameraRuntime, RetailCameraStep,
    };
    use crust_sim::gool::{
        CollisionObjectReference, GAME_STATE_GLOBAL, RetailPadSnapshot,
        RetailTransformVectorsCamera, process_register,
    };
    use crust_sim::math::Vec3;
    use crust_sim::object_arena::NeighborZone;
    use crust_sim::paging::Pager;
    use crust_sim::retail_lighting::{ObjectDarkShaderInput, apply_retail_object_zone_shader};
    use crust_sim::retail_runtime::{
        ISLAND_CAMERA_ROTATION_GLOBAL, NsfProgramHost, RetailDemoFinishOutcome,
        RetailLevelStateContext, RetailPauseUpdate, RetailRestartOutcome, RetailRuntime,
        ZoneTerminationMode,
    };
    use crust_sim::zone_lifecycle::{
        OrderedZoneLoadList, ZoneLifecycle, ZoneLifecycleZone, ZoneTransitionAction,
    };
    use std::path::PathBuf;

    use crate::pbak_runtime::{RetailPbakPlayback, pbak_event_pad_snapshot, prepare_pair_pbak};

    #[test]
    fn object_texture_residency_uses_the_live_display_boundary_snapshot() {
        let mut pager = Pager::new();
        let texture_eids = [
            "Tex0T", "Tex1T", "Tex2T", "Tex3T", "Tex4T", "Tex5T", "Tex6T", "Tex7T", "Tex8T",
        ]
        .map(|name| Eid::from_name(name).unwrap());
        for (index, eid) in texture_eids.iter().copied().enumerate() {
            let page = PageIndex::new(u32::try_from(index).unwrap());
            pager.register_page(page, []).unwrap();
            pager.bind_page_eid(eid, page).unwrap();
        }
        for eid in texture_eids.iter().copied().take(8) {
            pager.materialize_texture_eid(eid).unwrap();
        }
        let frame_snapshot = pager.texture_frame_snapshot();
        assert!(frame_snapshot.find_eid(texture_eids[0]).is_some());
        assert!(frame_snapshot.find_eid(texture_eids[8]).is_none());

        pager.materialize_texture_eid(texture_eids[8]).unwrap();
        let object_snapshot = pager.texture_frame_snapshot();
        assert!(object_snapshot.find_eid(texture_eids[0]).is_none());
        assert!(object_snapshot.find_eid(texture_eids[8]).is_some());

        let effective =
            effective_object_texture_snapshot(Some(frame_snapshot), Some(object_snapshot)).unwrap();
        assert!(effective.find_eid(texture_eids[0]).is_none());
        assert!(effective.find_eid(texture_eids[8]).is_some());
        assert_eq!(
            effective_object_texture_snapshot(Some(frame_snapshot), None),
            Some(frame_snapshot),
        );
    }

    fn refresh_pbak_level_context(
        graph: &RetailZoneGraph,
        lifecycle: &ZoneLifecycle,
        runtime: &mut RetailRuntime,
        location: RetailCameraLocation,
    ) -> Result<(), String> {
        let existing = runtime.level_state_context().cloned();
        let read_global = |index| {
            runtime
                .global_word(index)
                .map(u32::cast_signed)
                .map_err(|error| format!("retail global {index}: {error:?}"))
        };
        let graphics_flags = graph
            .zone(location.path.zone)
            .ok_or_else(|| format!("camera graph has no zone {}", location.path.zone))?
            .graphics_flags;
        runtime.set_level_state_context(RetailLevelStateContext {
            location,
            graphics_flags,
            box_count: read_global(62)?,
            checkpoint_id: read_global(69)?,
            checkpoint_translation: [read_global(102)?, read_global(103)?, read_global(104)?],
            first_spawn: existing.as_ref().is_some_and(|state| state.first_spawn),
            active_neighbor_zones: lifecycle.active_neighbor_zones(),
        });
        Ok(())
    }

    fn apply_pbak_camera_effects(
        level: LevelId,
        graph: &RetailZoneGraph,
        lifecycle: &mut ZoneLifecycle,
        runtime: &mut RetailRuntime,
        host: &mut NsfProgramHost<'_>,
        step: &RetailCameraStep,
    ) -> Result<(), String> {
        for effect in &step.effects {
            match *effect {
                RetailCameraEffect::GameStateWrite { value } => runtime
                    .set_global_word(GAME_STATE_GLOBAL, value.cast_unsigned())
                    .map_err(|error| format!("camera game-state write: {error:?}"))?,
                RetailCameraEffect::LevelUpdate {
                    before,
                    after,
                    flags,
                } => {
                    if before.path.zone != after.path.zone {
                        let activation_marker = (lifecycle.current_zone().is_none()
                            && level != LevelId::TITLE)
                            || flags & 2 != 0;
                        let plan = lifecycle
                            .plan_transition_with_marker(after.path.zone, activation_marker)
                            .map_err(|error| error.to_string())?;
                        for action in plan.actions().iter().copied() {
                            if let ZoneTransitionAction::TerminateZoneObjects(zone) = action {
                                let report = runtime
                                    .terminate_zone_objects(
                                        zone,
                                        ZoneTerminationMode::Departure {
                                            target: after.path.zone,
                                        },
                                        host,
                                    )
                                    .map_err(|error| format!("TERM {zone}: {error:?}"))?;
                                if let Some(failure) = report.event_failures.first() {
                                    return Err(format!(
                                        "TERM {zone} object {:?}: {:?}",
                                        failure.object, failure.error
                                    ));
                                }
                            }
                        }
                        lifecycle
                            .commit_transition(&plan)
                            .map_err(|error| error.to_string())?;
                    }
                    refresh_pbak_level_context(graph, lifecycle, runtime, after)?;
                }
                RetailCameraEffect::SaveStateHandshake { location } => {
                    refresh_pbak_level_context(graph, lifecycle, runtime, location)?;
                    let main = runtime
                        .arena()
                        .main_object()
                        .and_then(|arena| runtime.object_for_arena(arena))
                        .ok_or_else(|| "camera save handshake has no main object".to_owned())?;
                    runtime
                        .save_level_state(main, true)
                        .map_err(|error| format!("camera save handshake: {error:?}"))?;
                }
            }
        }
        Ok(())
    }

    #[test]
    fn text_font_resolution_prefers_the_validated_dynamic_word_offset() {
        let font_len = crust_formats::stream::GOOL_MAX_FONT_ANIMATION_LEN;
        let override_offset = font_len;
        let mut animations = vec![0_u8; font_len * 2];
        let default_page = Eid::from_name("font1").unwrap();
        let override_page = Eid::from_name("font2").unwrap();
        for (offset, page, header_length) in [
            (0, default_page, 64_u8),
            (override_offset, override_page, 95_u8),
        ] {
            animations[offset..offset + 4].copy_from_slice(&[3, 0, header_length, 0]);
            animations[offset + 4..offset + 8].copy_from_slice(&page.raw().to_le_bytes());
        }
        let override_word_offset = u32::try_from(override_offset / 4).unwrap();

        assert_eq!(
            resolve_text_font(&animations, 0, 0).unwrap().texture_page,
            default_page
        );
        assert_eq!(
            resolve_text_font(&animations, 0, override_word_offset)
                .unwrap()
                .texture_page,
            override_page
        );
        assert!(resolve_text_font(&animations, 0, u32::MAX).is_err());
    }

    #[test]
    fn zero_rotation_camera_matches_retail_world_adjustment() {
        assert_eq!(
            world_camera_matrix(0, 0, 0).values,
            [[0x1000, 0, 0], [0, -0x0a00, 0], [0, 0, -0x1000]]
        );
    }

    #[test]
    fn world_shader_scratch_matches_dispatch_writes_empty_lists_and_dark2_retention() {
        let mut state = RetailWorldShaderRenderState::default();
        let mut shader = RetailWorldShaderSnapshot::initialized_for_level(LevelId::N_SANITY_BEACH);
        shader.clear_color = [1, 2, 3];

        update_world_shader_render_state(
            &mut state,
            WorldShaderMode::Plain,
            shader,
            [9, 8, 7],
            true,
            true,
        );
        assert_eq!(state.far_color1(), [9, 8, 7]);

        update_world_shader_render_state(
            &mut state,
            WorldShaderMode::Dark2,
            shader,
            [6, 5, 4],
            true,
            true,
        );
        assert_eq!(state.far_color1(), [9, 8, 7]);

        update_world_shader_render_state(
            &mut state,
            WorldShaderMode::Lightning,
            shader,
            [0; 3],
            true,
            true,
        );
        assert_eq!(state.far_color1(), [16, 32, 48]);

        // Every wrapper except pure Fog returns before touching scratch when
        // the selected SLST contains no polygons.
        update_world_shader_render_state(
            &mut state,
            WorldShaderMode::Ripple,
            shader,
            [4, 5, 6],
            true,
            false,
        );
        assert_eq!(state.far_color1(), [16, 32, 48]);
        update_world_shader_render_state(
            &mut state,
            WorldShaderMode::Fog,
            shader,
            [4, 5, 6],
            true,
            false,
        );
        assert_eq!(state.far_color1(), [4, 5, 6]);

        update_world_shader_render_state(
            &mut state,
            WorldShaderMode::Fog,
            shader,
            [1, 1, 1],
            false,
            true,
        );
        assert_eq!(state.far_color1(), [4, 5, 6]);
    }

    #[test]
    fn exact_row_adjustment_multiplies_before_shifting() {
        let matrix = world_camera_matrix(1, 0, 0);
        let source = Angle12::new(-1).sin_q12();
        assert_eq!(
            matrix.values[1][2],
            wrapping_i16((-5 * -i32::from(source)) >> 3)
        );
    }

    #[test]
    fn graphics_flag_1000_substitutes_the_fixed_object_camera_only() {
        let world = CameraSample {
            translation: Vec3i {
                x: 100,
                y: 200,
                z: 300,
            },
            translation_fixed: [100 << 8, 200 << 8, 300 << 8],
            rotation_y: 10,
            rotation_x: 20,
            rotation_z: 30,
        };
        assert_eq!(object_camera_sample(world, 0, 64), world);

        let start = object_camera_sample(world, 0x1000, 0);
        assert_eq!(start.translation_fixed, [0, 952_800, 6_144_000]);
        assert_eq!(
            start.translation,
            Vec3i {
                x: 0,
                y: 3_721,
                z: 24_000
            }
        );
        assert_eq!(
            [start.rotation_y, start.rotation_x, start.rotation_z],
            [-125, 0, 0]
        );

        let trough = object_camera_sample(world, 0x1000, 64);
        assert_eq!(trough.translation_fixed, [0, 901_600, 6_144_000]);
        let web_matrix = adjusted_camera_matrix(raw_camera_matrix(
            trough.rotation_y,
            trough.rotation_x,
            trough.rotation_z,
        ));
        let sim_camera = RetailTransformVectorsCamera::from_retail_pose(
            world.translation_fixed,
            [world.rotation_y, world.rotation_x, world.rotation_z],
            500,
        )
        .for_object_display(0x1000, 64);
        assert_eq!(trough.translation_fixed, sim_camera.translation);
        assert_eq!(web_matrix.values, sim_camera.rotation_matrix);
        let point: [i32; 3] = [321 << 8, 4_567 << 8, 23_100 << 8];
        let relative = Vec3i {
            x: point[0].wrapping_sub(trough.translation_fixed[0]) >> 8,
            y: point[1].wrapping_sub(trough.translation_fixed[1]) >> 8,
            z: point[2].wrapping_sub(trough.translation_fixed[2]) >> 8,
        };
        let web_point = rotate(relative, web_matrix).point;
        assert_eq!(
            [web_point.x, web_point.y, web_point.z],
            sim_camera.camera_space_point(point)
        );
        let frozen_draw_location = RetailSceneProgressLocation {
            zone: Eid::NONE,
            path_index: 0,
            path_progress: 0,
            frame_stamp: 64,
            draw_count: 0,
        };
        assert_eq!(
            object_camera_sample_for_location(world, 0x1000, frozen_draw_location),
            trough,
            "object bob follows GOOL time even while texture draw_count is frozen"
        );
        assert_eq!(object_camera_sample(world, 0x1000, 128), start);
    }

    #[test]
    fn explicit_non_path_camera_pose_preserves_fixed_translation_and_yxz_rotation() {
        let pose = RetailSceneCameraPose {
            translation: [-257, 0x12_345, i32::MIN + 255],
            rotation_yxz: [0x123, -0x456, 0x789],
        };

        let camera = camera_sample_from_pose(pose);

        assert_eq!(camera.translation_fixed, pose.translation);
        assert_eq!(
            camera.translation,
            Vec3i {
                x: -2,
                y: 0x123,
                z: (i32::MIN + 255) >> 8,
            }
        );
        assert_eq!(
            [camera.rotation_y, camera.rotation_x, camera.rotation_z],
            pose.rotation_yxz
        );
    }

    #[test]
    fn projection_distance_matches_every_retail_fov() {
        assert_eq!(projection_distance(30).unwrap(), 960);
        assert_eq!(projection_distance(37).unwrap(), 800);
        assert_eq!(projection_distance(55).unwrap(), 500);
        assert_eq!(projection_distance(60).unwrap(), 460);
        assert_eq!(projection_distance(90).unwrap(), 288);
        assert!(projection_distance(45).is_err());
    }

    #[test]
    fn scene_texture_handles_are_stable_across_visibility_order() {
        let first = TextureRequest {
            page_id: 0x1234,
            region: TextureRegion::new(8, 12, 16, 20).unwrap(),
            color_mode: ColorMode::Direct15,
            blend_mode: BlendMode::Opaque,
            clut: None,
        };
        let second = TextureRequest {
            page_id: 0x5678,
            region: TextureRegion::new(4, 6, 8, 10).unwrap(),
            color_mode: ColorMode::Indexed4,
            blend_mode: BlendMode::Average,
            clut: Some(crust_renderer::texture::ClutLocation { block_x: 2, row: 3 }),
        };
        let mut first_order = HashMap::new();
        let mut first_handles = HashMap::new();
        let first_handle =
            stable_scene_texture_handle(first, &mut first_order, &mut first_handles).unwrap();
        let second_handle =
            stable_scene_texture_handle(second, &mut first_order, &mut first_handles).unwrap();

        let mut reverse_order = HashMap::new();
        let mut reverse_handles = HashMap::new();
        assert_eq!(
            stable_scene_texture_handle(second, &mut reverse_order, &mut reverse_handles).unwrap(),
            second_handle
        );
        assert_eq!(
            stable_scene_texture_handle(first, &mut reverse_order, &mut reverse_handles).unwrap(),
            first_handle
        );
        assert_ne!(first_handle, second_handle);
        assert!(first_handle.get() < (1_u64 << 63));
        assert!(second_handle.get() < (1_u64 << 63));
    }

    #[test]
    fn presentation_projection_retains_offscreen_coordinates_for_safe_culling() {
        let point = Vec3i {
            x: 100_000,
            y: 0,
            z: 10_000,
        };
        let retail = project(point, Vec3i::default(), Matrix3::IDENTITY, [0, 0], 500);
        assert!(!retail.valid);
        assert_eq!(retail.screen.x, 1_023);

        let modern = project_presentation(point, Vec3i::default(), Matrix3::IDENTITY, 500);
        assert!(modern.valid);
        assert_eq!(modern.screen.x, 5_000);
        assert_eq!(
            Viewport::PSX.classify_triangle([
                modern.screen,
                crust_renderer::command::ScreenPoint {
                    x: 5_001,
                    ..modern.screen
                },
                crust_renderer::command::ScreenPoint {
                    x: 5_002,
                    ..modern.screen
                },
            ]),
            TriangleVisibility::Outside
        );
    }

    #[test]
    fn authored_backdrops_expand_horizontally_to_cover_wider_viewports() {
        let ultrawide = Viewport {
            x: -448,
            y: -120,
            width: 896,
            height: 240,
        };
        for (retail_x, ultrawide_x) in [(-256, -504), (0, 0), (256, 504)] {
            let source = crust_renderer::command::ScreenPoint {
                x: retail_x,
                y: 37,
                z: 901,
            };
            let expanded = expand_backdrop_to_viewport(source, ultrawide);
            assert_eq!(expanded.x, ultrawide_x);
            assert_eq!(expanded.y, source.y);
            assert_eq!(expanded.z, source.z);
        }
        assert_eq!(
            expand_backdrop_to_viewport(
                crust_renderer::command::ScreenPoint {
                    x: 91,
                    y: -7,
                    z: 313
                },
                Viewport::PSX
            ),
            crust_renderer::command::ScreenPoint {
                x: 91,
                y: -7,
                z: 313
            }
        );
    }

    #[test]
    fn ripple_state_matches_source_seed_advance_wrap_pause_and_level_rates() {
        for (level, speed, period) in [
            (LevelId::new_const(0x0f), 10_i32, 127_i32),
            (LevelId::new_const(0x24), 4, 127),
            (LevelId::new_const(0x26), 1, 23),
        ] {
            let mut state = RetailRippleState::new(level);
            let stride = (period + 1) / 8;
            let mut iterative = std::array::from_fn(|index| {
                let index = i32::try_from(index).unwrap();
                -(period - index * stride)
            });
            assert_eq!(state.magnitudes(false), iterative.map(i32::abs));
            assert_eq!(
                state.magnitudes(false),
                iterative.map(i32::abs),
                "pause/hidden submissions retain the exact seeded cells"
            );

            for step in 1..=2_048 {
                for cell in &mut iterative {
                    *cell += speed;
                    if *cell > period {
                        *cell = -(period - 1);
                    }
                }
                let expected = iterative.map(i32::abs);
                assert_eq!(state.magnitudes(true), expected, "step {step}");
                assert_eq!(
                    state.magnitudes(false),
                    expected,
                    "a paused/hidden frame after step {step} must not advance"
                );
            }
        }
    }

    #[test]
    fn island_map_exit_retains_last_masks_without_mutating_wgeo() {
        let paths = [
            Some(WorldMapPathList::parse(&[2, 0, 0x21, 0x80, 1, 0]).unwrap()),
            Some(WorldMapPathList::parse(&[3, 0, 0, 0, 2, 0x80, 1, 0]).unwrap()),
        ];
        let polygon_counts = [2, 2];
        let mut persistent_masks = empty_world_map_path_masks(&polygon_counts);
        let animation = RetailMapPathAnimation {
            title_state: 15,
            map_level_links: 1 << 2,
            map_key_links: 1 << 1,
        };
        update_persistent_world_map_path_masks_for_counts(
            &mut persistent_masks,
            &polygon_counts,
            &paths,
            Some(animation),
        )
        .unwrap();
        assert_eq!(
            persistent_masks,
            [vec![None, Some(7)], vec![Some(7), Some(7)]]
        );

        // GOOL can request the next title state while the map graph remains
        // resident for its fade. Native no longer calls GfxAnimMapPaths, but
        // its prior writes remain in WGEO memory. The sidecar must do likewise.
        let masks_before_exit = persistent_masks.clone();
        let wrong_title_state = RetailMapPathAnimation {
            title_state: 14,
            ..animation
        };
        update_persistent_world_map_path_masks_for_counts(
            &mut persistent_masks,
            &polygon_counts,
            &paths,
            Some(wrong_title_state),
        )
        .unwrap();
        assert_eq!(persistent_masks, masks_before_exit);
        update_persistent_world_map_path_masks_for_counts(
            &mut persistent_masks,
            &polygon_counts,
            &paths,
            None,
        )
        .unwrap();
        assert_eq!(persistent_masks, masks_before_exit);
        let map_graph = RetailSceneCacheKey {
            zone: Eid::from_raw(0x1234_5679),
            path_index: 2,
        };
        assert!(scene_graph_cache_matches(
            Some(map_graph),
            true,
            map_graph,
            false,
        ));
        assert!(!scene_graph_cache_matches(
            Some(map_graph),
            false,
            map_graph,
            true,
        ));

        // Replacing the active graph constructs a fresh sidecar, just as
        // unloading the native WGEO discards its mutated resident copy.
        assert_eq!(
            empty_world_map_path_masks(&polygon_counts),
            [vec![None, None], vec![None, None]]
        );
        assert!(!scene_graph_cache_matches(
            Some(map_graph),
            true,
            RetailSceneCacheKey {
                path_index: 3,
                ..map_graph
            },
            false,
        ));

        let original = crust_formats::stream::WorldPolygon {
            vertex_indices: [0, 1, 2],
            texture_info_word_index: 0,
            texture_page_index: 0,
            animation_period: 0,
            animation_mask: 9,
            animation_phase: 0,
            reserved: false,
        };
        let effective = polygon_with_map_path_mask(original, Some(7));
        assert_eq!(original.animation_mask, 9);
        assert_eq!(effective.animation_mask, 7);
        assert_eq!(original.animation_frame(16), 16);
        assert_eq!(effective.animation_frame(16), 0);

        assert_eq!(
            active_map_path_animation(LevelId::TITLE, Some(animation)),
            Some(animation)
        );
        assert_eq!(
            active_map_path_animation(LevelId::N_SANITY_BEACH, Some(animation)),
            None
        );
    }

    #[test]
    fn island_map_mask_resolution_reports_world_list_mismatch() {
        let mut persistent_masks = vec![vec![None]];
        assert!(
            update_persistent_world_map_path_masks_for_counts(
                &mut persistent_masks,
                &[1],
                &[],
                None,
            )
            .unwrap_err()
            .to_string()
            .contains("does not match")
        );
    }

    #[test]
    fn fragment_bounds_use_the_effective_mips_shift_word() {
        assert_eq!(scaled_fragment_bound(1, 31), Ok(i32::MIN));
        assert_eq!(scaled_fragment_bound(i16::MIN, 17), Ok(0));
        assert_eq!(
            scaled_fragment_bound(1, 32),
            Err(scene_error(
                "GOOL fragment bound: retail sprite shift 32 exceeds 31"
            ))
        );
    }

    #[test]
    fn world_camera_coordinates_add_signed_path_points_before_source_24_8_expansion() {
        assert_eq!(path_coordinate(1_000, -2).unwrap(), 998);
        assert_eq!(path_coordinate(-1_000, 2).unwrap(), -998);
        assert!(path_coordinate(i32::MAX, 1).is_err());
    }

    #[test]
    fn fractional_camera_sample_matches_source_fixed_point_and_shortest_yaw() {
        let point = ZonePathPoint {
            x: -1,
            y: 10,
            z: -20,
            rotation_y: 0x0ff0,
            rotation_x: -100,
            rotation_z: 40,
        };
        let next = ZonePathPoint {
            x: 1,
            y: 14,
            z: -10,
            rotation_y: 0x0010,
            rotation_x: 100,
            rotation_z: -40,
        };
        let sample = interpolate_camera([0, 100, -100], point, [0, 100, -100], next, 0x80).unwrap();
        assert_eq!(
            sample.translation,
            Vec3i {
                x: 0,
                y: 112,
                z: -115
            }
        );
        assert_eq!(sample.rotation_y, 0x1000);
        assert_eq!(sample.rotation_x, 0);
        assert_eq!(sample.rotation_z, 0);
        assert_eq!(sample.translation_fixed, [0, 112 << 8, -115 << 8]);

        // The C implementation keeps 24.8 precision until the graphics-side
        // arithmetic shift, so a negative fraction rounds toward -infinity.
        assert_eq!(interpolate_coordinate(-1, 0, 1).unwrap(), -1);
        assert_eq!(interpolate_coordinate_fixed(-1, 0, 1).unwrap(), -255);
    }

    #[test]
    fn scene_builder_diagnostics_start_at_a_pair_boundary() {
        let first_pair = RetailSceneBuilder::new();
        let next_pair = RetailSceneBuilder::new();
        assert_eq!(
            first_pair.diagnostics(),
            RetailSceneCacheDiagnostics::default()
        );
        assert_eq!(
            next_pair.diagnostics(),
            RetailSceneCacheDiagnostics::default()
        );
    }

    #[test]
    #[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
    fn builds_n_sanity_spawn_snapshot_from_local_retail_streams() {
        let root = PathBuf::from(
            std::env::var_os("C1_STREAM_DIR")
                .expect("C1_STREAM_DIR must name local extracted retail streams"),
        );
        let level = LevelId::N_SANITY_BEACH;
        let nsd_path = root.join(StreamName::new(level, StreamKind::Nsd).filename());
        let nsf_path = root.join(StreamName::new(level, StreamKind::Nsf).filename());
        let nsd_bytes = std::fs::read(&nsd_path)
            .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
        let nsf_bytes = std::fs::read(&nsf_path)
            .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));
        let nsd = parse_nsd(&nsd_bytes, level).unwrap();
        let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
        let scene = build_retail_scene(&nsd, &nsf, &nsf_bytes).unwrap();
        eprintln!("N. Sanity Beach spawn scene: {:?}", scene.stats);
        assert_eq!(
            scene.stats,
            RetailSceneStats {
                worlds: 4,
                visible_polygons: 681,
                submitted_polygons: 681,
                unique_textures: 52,
                saturated_vertices: 0,
                skipped_textured_polygons: 0,
                ..RetailSceneStats::default()
            }
        );
        assert!(!scene.commands.is_empty());
        assert_eq!(scene.stats.unique_textures, scene.textures.len());

        let ldat = nsd.ldat().unwrap();
        let extended = RetailSceneBuilder::new()
            .build_at_progress_with_presentation(
                &nsd,
                &nsf,
                &nsf_bytes,
                RetailSceneProgressLocation {
                    zone: ldat.spawn_zone,
                    path_index: u32::try_from(ldat.spawn_path_index).unwrap(),
                    path_progress: 0,
                    frame_stamp: 0,
                    draw_count: 0,
                },
                RetailScenePresentation {
                    projection_distance: Some(425),
                    extended_world: true,
                    viewport: Viewport::PSX,
                },
            )
            .unwrap();
        eprintln!("N. Sanity Beach complete-level scene: {:?}", extended.stats);
        let full_graph = parse_full_level_scene_graph(&nsd, &nsf, &nsf_bytes).unwrap();
        assert_eq!(extended.stats.worlds, scene.stats.worlds);
        assert_eq!(extended.stats.preloaded_worlds, full_graph.worlds.len());
        assert!(extended.stats.preloaded_worlds > extended.stats.worlds);
        assert!(extended.stats.visible_polygons > scene.stats.visible_polygons);
        assert!(extended.stats.submitted_polygons > scene.stats.submitted_polygons);
        assert!(extended.stats.submitted_polygons <= extended.stats.visible_polygons);
        assert_eq!(extended.stats.unique_textures, extended.textures.len());

        let spawn_location = RetailSceneProgressLocation {
            zone: ldat.spawn_zone,
            path_index: u32::try_from(ldat.spawn_path_index).unwrap(),
            path_progress: 0,
            frame_stamp: 0,
            draw_count: 0,
        };
        let mut retail_cache_builder = RetailSceneBuilder::new();
        retail_cache_builder
            .build_at_progress(&nsd, &nsf, &nsf_bytes, spawn_location)
            .unwrap();
        retail_cache_builder
            .build_at_progress(&nsd, &nsf, &nsf_bytes, spawn_location)
            .unwrap();
        let retail_cache_frame = retail_cache_builder.texture_cache.metrics().frame;
        let mut toggle_cache_builder = RetailSceneBuilder::new();
        toggle_cache_builder
            .build_at_progress(&nsd, &nsf, &nsf_bytes, spawn_location)
            .unwrap();
        toggle_cache_builder
            .build_at_progress_with_presentation(
                &nsd,
                &nsf,
                &nsf_bytes,
                spawn_location,
                RetailScenePresentation {
                    projection_distance: Some(425),
                    extended_world: true,
                    viewport: Viewport::PSX,
                },
            )
            .unwrap();
        assert_eq!(
            toggle_cache_builder.texture_cache.metrics().frame,
            retail_cache_frame,
            "presentation-only WGEO requests must not perturb the native texture cache"
        );
        let retail_after_toggle = retail_cache_builder
            .build_at_progress(&nsd, &nsf, &nsf_bytes, spawn_location)
            .unwrap();
        let toggled_after_toggle = toggle_cache_builder
            .build_at_progress(&nsd, &nsf, &nsf_bytes, spawn_location)
            .unwrap();
        assert_eq!(
            toggled_after_toggle, retail_after_toggle,
            "returning to retail presentation must reproduce the untouched native scene"
        );
        assert_eq!(
            toggle_cache_builder.texture_cache.metrics().frame,
            retail_cache_builder.texture_cache.metrics().frame,
            "returning to retail presentation must preserve native LRU behavior"
        );
        assert!(
            !toggle_cache_builder
                .presentation_texture_cache
                .entries
                .is_empty()
        );

        let first_presented =
            build_retail_scene_at_path_point(&nsd, &nsf, &nsf_bytes, 2, 1).unwrap();
        assert_eq!(first_presented.stats.worlds, 4);
        assert_eq!(first_presented.stats.visible_polygons, 679);
        assert_eq!(
            first_presented.stats.submitted_polygons,
            first_presented.commands.len()
        );
        let explicit = build_retail_scene_at_location(
            &nsd,
            &nsf,
            &nsf_bytes,
            RetailSceneLocation {
                zone: ldat.spawn_zone,
                path_index: u32::try_from(ldat.spawn_path_index).unwrap(),
                path_point_index: 2,
                draw_count: 1,
            },
        )
        .unwrap();
        assert_eq!(explicit, first_presented);
        let integral_progress = build_retail_scene_at_progress(
            &nsd,
            &nsf,
            &nsf_bytes,
            RetailSceneProgressLocation {
                zone: ldat.spawn_zone,
                path_index: u32::try_from(ldat.spawn_path_index).unwrap(),
                path_progress: 2 << 8,
                frame_stamp: 1,
                draw_count: 1,
            },
        )
        .unwrap();
        assert_eq!(integral_progress, first_presented);
        let fractional_progress = build_retail_scene_at_progress(
            &nsd,
            &nsf,
            &nsf_bytes,
            RetailSceneProgressLocation {
                zone: ldat.spawn_zone,
                path_index: u32::try_from(ldat.spawn_path_index).unwrap(),
                path_progress: (2 << 8) | 0x80,
                frame_stamp: 1,
                draw_count: 1,
            },
        )
        .unwrap();
        assert_eq!(fractional_progress.path_point_index, 2);
        assert_eq!(
            fractional_progress.stats.worlds,
            first_presented.stats.worlds
        );
        assert_eq!(
            fractional_progress.stats.visible_polygons,
            first_presented.stats.visible_polygons
        );

        let cached_location = RetailSceneProgressLocation {
            zone: ldat.spawn_zone,
            path_index: u32::try_from(ldat.spawn_path_index).unwrap(),
            path_progress: (2 << 8) | 0x80,
            frame_stamp: 1,
            draw_count: 1,
        };
        let mut builder = RetailSceneBuilder::new();
        let cached_first = builder
            .build_at_progress(&nsd, &nsf, &nsf_bytes, cached_location)
            .unwrap();
        assert_eq!(cached_first, fractional_progress);
        let after_first = builder.diagnostics();
        let cached_second = builder
            .build_at_progress(&nsd, &nsf, &nsf_bytes, cached_location)
            .unwrap();
        let after_second = builder.diagnostics();
        assert_eq!(cached_second, cached_first);
        assert_eq!(cached_second.textures.len(), cached_first.textures.len());
        for (first, second) in cached_first.textures.iter().zip(&cached_second.textures) {
            assert_eq!(first.handle, second.handle);
            assert!(Arc::ptr_eq(&first.pixels, &second.pixels));
        }
        assert_eq!(after_first.graph_builds, 1);
        assert_eq!(after_second.graph_builds, 1);
        assert_eq!(after_second.graph_reuses, 1);
        assert_eq!(
            after_second.texture_page_installs,
            after_first.texture_page_installs
        );
        assert_eq!(after_second.texture_misses, after_first.texture_misses);
        assert!(after_second.texture_hits > after_first.texture_hits);

        let next_animation = RetailSceneProgressLocation {
            draw_count: cached_location.draw_count + 1,
            ..cached_location
        };
        let cached_animated = builder
            .build_at_progress(&nsd, &nsf, &nsf_bytes, next_animation)
            .unwrap();
        let static_animated =
            build_retail_scene_at_progress(&nsd, &nsf, &nsf_bytes, next_animation).unwrap();
        assert_eq!(cached_animated, static_animated);

        let spawn_entry = typed_entry(
            &nsf,
            &nsd,
            ldat.spawn_zone,
            ZDAT_ENTRY_TYPE,
            "test spawn ZDAT",
        )
        .unwrap();
        let spawn_header = ZoneHeader::parse(
            entry_item(spawn_entry, &nsf_bytes, 0, "test spawn ZDAT header").unwrap(),
        )
        .unwrap();
        let alternate = spawn_header
            .neighbors
            .iter()
            .copied()
            .find_map(|zone| {
                let entry =
                    typed_entry(&nsf, &nsd, zone, ZDAT_ENTRY_TYPE, "test neighbor ZDAT").ok()?;
                let header = ZoneHeader::parse(
                    entry_item(entry, &nsf_bytes, 0, "test neighbor ZDAT header").ok()?,
                )
                .ok()?;
                (0..header.path_count).find_map(|path_index| {
                    if (zone, path_index) == (cached_location.zone, cached_location.path_index) {
                        return None;
                    }
                    let location = RetailSceneProgressLocation {
                        zone,
                        path_index,
                        path_progress: 0,
                        frame_stamp: 0,
                        draw_count: 0,
                    };
                    builder
                        .build_at_progress(&nsd, &nsf, &nsf_bytes, location)
                        .ok()
                        .map(|_| location)
                })
            })
            .expect("N. Sanity spawn graph has a buildable neighbor path");
        assert_ne!(
            (alternate.zone, alternate.path_index),
            (cached_location.zone, cached_location.path_index)
        );
        assert_eq!(builder.diagnostics().graph_builds, 2);
        let cached_after_return = builder
            .build_at_progress(&nsd, &nsf, &nsf_bytes, cached_location)
            .unwrap();
        assert_eq!(cached_after_return, cached_first);
        assert_eq!(builder.diagnostics().graph_builds, 3);
    }

    #[test]
    #[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
    fn every_local_gameplay_pair_preloads_a_bounded_graph_and_builds_extended_presentation() {
        let root = PathBuf::from(
            std::env::var_os("C1_STREAM_DIR")
                .expect("C1_STREAM_DIR must name local extracted retail streams"),
        );
        let mut built = 0_usize;
        let mut total_worlds = 0_usize;
        let mut total_preloaded_worlds = 0_usize;
        let mut total_candidates = 0_usize;
        let mut total_submitted = 0_usize;

        for known in KNOWN_LEVELS
            .iter()
            .filter(|known| known.bootable && known.id != LevelId::TITLE)
        {
            let nsd_path = root.join(known.nsd_filename());
            let nsf_path = root.join(known.nsf_filename());
            let nsd_bytes = std::fs::read(&nsd_path)
                .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
            let nsf_bytes = std::fs::read(&nsf_path)
                .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));
            let nsd = parse_nsd(&nsd_bytes, known.id).unwrap();
            let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
            let ldat = nsd.ldat().expect("bootable gameplay pair has LDAT");
            let scene = RetailSceneBuilder::new()
                .build_at_progress_with_presentation(
                    &nsd,
                    &nsf,
                    &nsf_bytes,
                    RetailSceneProgressLocation {
                        zone: ldat.spawn_zone,
                        path_index: u32::try_from(ldat.spawn_path_index)
                            .expect("retail spawn path index is non-negative"),
                        path_progress: 0,
                        frame_stamp: 0,
                        draw_count: 0,
                    },
                    RetailScenePresentation {
                        projection_distance: None,
                        extended_world: true,
                        viewport: Viewport {
                            x: -448,
                            y: -120,
                            width: 896,
                            height: 240,
                        },
                    },
                )
                .unwrap_or_else(|error| {
                    panic!("{} complete-level presentation: {error}", known.name)
                });
            assert!(scene.stats.worlds <= PRESENTATION_WORLD_LIMIT);
            assert!(scene.stats.preloaded_worlds <= PRESENTATION_WORLD_LIMIT);
            assert!(
                scene.stats.preloaded_worlds > 0,
                "{} preloaded no reachable non-backdrop worlds",
                known.name
            );
            assert!(scene.stats.visible_polygons <= PRESENTATION_POLYGON_LIMIT);
            assert!(scene.stats.submitted_polygons <= scene.stats.visible_polygons);
            assert_eq!(scene.stats.unique_textures, scene.textures.len());
            built = built.saturating_add(1);
            total_worlds = total_worlds.saturating_add(scene.stats.worlds);
            total_preloaded_worlds =
                total_preloaded_worlds.saturating_add(scene.stats.preloaded_worlds);
            total_candidates = total_candidates.saturating_add(scene.stats.visible_polygons);
            total_submitted = total_submitted.saturating_add(scene.stats.submitted_polygons);
        }

        assert_eq!(built, 42);
        assert!(total_worlds > 0);
        assert!(total_preloaded_worlds > 0);
        assert!(total_candidates > 0);
        assert!(total_submitted > 0);
        eprintln!(
            "extended-presentation corpus: pairs={built}, active_worlds={total_worlds}, preloaded_worlds={total_preloaded_worlds}, candidates={total_candidates}, submitted={total_submitted}"
        );
    }

    #[test]
    #[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
    fn n_sanity_camera_chain_drives_pair_scoped_scene_builds() {
        let root = PathBuf::from(
            std::env::var_os("C1_STREAM_DIR")
                .expect("C1_STREAM_DIR must name local extracted retail streams"),
        );
        let level = LevelId::N_SANITY_BEACH;
        let nsd_path = root.join(StreamName::new(level, StreamKind::Nsd).filename());
        let nsf_path = root.join(StreamName::new(level, StreamKind::Nsf).filename());
        let nsd_bytes = std::fs::read(&nsd_path)
            .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
        let nsf_bytes = std::fs::read(&nsf_path)
            .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));
        let nsd = parse_nsd(&nsd_bytes, level).unwrap();
        let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
        let graph = RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes).unwrap();
        let mut camera = RetailCameraRuntime::new(&graph).unwrap();
        let mut builder = RetailSceneBuilder::new();

        for draw_count in 0..192 {
            let step = camera.update(&graph, RetailCameraInput::default()).unwrap();
            let scene = builder
                .build_at_progress(
                    &nsd,
                    &nsf,
                    &nsf_bytes,
                    RetailSceneProgressLocation {
                        zone: step.after.path.zone,
                        path_index: step.after.path.index,
                        path_progress: step.after.progress.raw(),
                        frame_stamp: draw_count,
                        draw_count,
                    },
                )
                .unwrap_or_else(|error| {
                    panic!(
                        "camera path {}:{} at {:#x}: {error}",
                        step.after.path.zone,
                        step.after.path.index,
                        step.after.progress.raw()
                    )
                });
            assert_eq!(scene.zone, step.after.path.zone);
            assert_eq!(scene.path_index, step.after.path.index);
            assert_eq!(scene.path_point_index, step.after.progress.point_index());
        }

        assert_eq!(
            camera.location().path,
            RetailPathId {
                zone: graph.spawn_path().zone,
                index: 2,
            }
        );
        let diagnostics = builder.diagnostics();
        assert_eq!(diagnostics.graph_builds, 5);
        assert_eq!(diagnostics.graph_reuses, 187);
    }

    #[test]
    #[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
    fn upstream_ripple_moves_visible_effect_vertices_from_the_retail_wgeo() {
        let root = PathBuf::from(
            std::env::var_os("C1_STREAM_DIR")
                .expect("C1_STREAM_DIR must name local extracted retail streams"),
        );
        let level = LevelId::new_const(0x0f);
        let nsd_path = root.join(StreamName::new(level, StreamKind::Nsd).filename());
        let nsf_path = root.join(StreamName::new(level, StreamKind::Nsf).filename());
        let nsd_bytes = std::fs::read(&nsd_path)
            .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
        let nsf_bytes = std::fs::read(&nsf_path)
            .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));
        let nsd = parse_nsd(&nsd_bytes, level).unwrap();
        let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
        let graph = RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes).unwrap();
        let camera = RetailCameraRuntime::new(&graph).unwrap();
        let location = camera.location();
        assert_ne!(
            graph.zone(location.path.zone).unwrap().graphics_flags & ZONE_FLAG_RIPPLE,
            0,
            "Upstream's initial authored world must select GfxTransformWorldsRipple"
        );

        let mut builder = RetailSceneBuilder::new();
        let scene_at_zero = builder
            .build_at_progress(
                &nsd,
                &nsf,
                &nsf_bytes,
                RetailSceneProgressLocation {
                    zone: location.path.zone,
                    path_index: location.path.index,
                    path_progress: location.progress.raw(),
                    frame_stamp: 0,
                    draw_count: 0,
                },
            )
            .unwrap();
        let scene_at_one = builder
            .build_at_progress(
                &nsd,
                &nsf,
                &nsf_bytes,
                RetailSceneProgressLocation {
                    zone: location.path.zone,
                    path_index: location.path.index,
                    path_progress: location.progress.raw(),
                    frame_stamp: 1,
                    draw_count: 1,
                },
            )
            .unwrap();

        let base_location = RetailSceneProgressLocation {
            zone: location.path.zone,
            path_index: location.path.index,
            path_progress: location.progress.raw(),
            frame_stamp: 0,
            draw_count: 0,
        };
        let field_of_view = nsd.ldat().unwrap().field_of_view;
        let mut paused_builder = RetailSceneBuilder::new();
        let paused_first = paused_builder
            .build_at_progress_with_objects_and_world_display_mask_and_fov(
                &nsd,
                &nsf,
                &nsf_bytes,
                base_location,
                false,
                &[],
                None,
                RETAIL_INITIAL_DISPLAY_FLAGS,
                field_of_view,
                None,
            )
            .unwrap();
        let paused_hold = paused_builder
            .build_at_progress_with_objects_and_world_display_mask_and_fov(
                &nsd,
                &nsf,
                &nsf_bytes,
                base_location,
                false,
                &[],
                None,
                RETAIL_INITIAL_DISPLAY_FLAGS,
                field_of_view,
                None,
            )
            .unwrap();
        let resumed = paused_builder
            .build_at_progress_with_objects_and_world_display_mask_and_fov(
                &nsd,
                &nsf,
                &nsf_bytes,
                base_location,
                true,
                &[],
                None,
                RETAIL_INITIAL_DISPLAY_FLAGS,
                field_of_view,
                None,
            )
            .unwrap();

        let mut hidden_gap_builder = RetailSceneBuilder::new();
        let hidden = hidden_gap_builder
            .build_at_progress_with_objects_and_world_display_mask_and_fov(
                &nsd,
                &nsf,
                &nsf_bytes,
                base_location,
                true,
                &[],
                None,
                0,
                field_of_view,
                None,
            )
            .unwrap();
        assert_eq!(hidden.stats.visible_polygons, 0);
        let after_hidden_gap = hidden_gap_builder
            .build_at_progress_with_objects_and_world_display_mask_and_fov(
                &nsd,
                &nsf,
                &nsf_bytes,
                base_location,
                true,
                &[],
                None,
                RETAIL_INITIAL_DISPLAY_FLAGS,
                field_of_view,
                None,
            )
            .unwrap();
        let mut direct_builder = RetailSceneBuilder::new();
        let direct_first = direct_builder
            .build_at_progress_with_objects_and_world_display_mask_and_fov(
                &nsd,
                &nsf,
                &nsf_bytes,
                base_location,
                true,
                &[],
                None,
                RETAIL_INITIAL_DISPLAY_FLAGS,
                field_of_view,
                None,
            )
            .unwrap();

        let positions = |scene: &RetailScene| {
            scene
                .commands
                .iter()
                .filter_map(|command| {
                    let CommandSource::World { polygon, .. } = command.source else {
                        return None;
                    };
                    let points = match &command.primitive {
                        PrimitiveCommand::ColoredTriangle(triangle) => {
                            triangle.vertices.map(|vertex| vertex.position)
                        }
                        PrimitiveCommand::TexturedTriangle(triangle) => {
                            triangle.vertices.map(|vertex| vertex.position)
                        }
                        _ => return None,
                    };
                    Some((polygon, points))
                })
                .collect::<BTreeMap<_, _>>()
        };
        let zero_positions = positions(&scene_at_zero);
        let one_positions = positions(&scene_at_one);
        let paused_first_positions = positions(&paused_first);
        assert_eq!(paused_first_positions, positions(&paused_hold));
        assert_ne!(
            paused_first_positions,
            positions(&resumed),
            "the first unpaused nonempty ripple submission advances the seeded wave"
        );
        assert_eq!(
            positions(&after_hidden_gap),
            positions(&direct_first),
            "a hidden-world build must not consume a ripple advance"
        );
        assert_eq!(
            zero_positions.keys().collect::<Vec<_>>(),
            one_positions.keys().collect::<Vec<_>>()
        );
        let changed = zero_positions
            .iter()
            .filter(|(polygon, points)| {
                one_positions
                    .get(polygon)
                    .is_some_and(|next| next != *points)
            })
            .count();
        let unchanged = zero_positions.len().saturating_sub(changed);
        assert!(
            changed > 0,
            "visible effect-flagged retail vertices must ripple"
        );
        assert!(
            unchanged > 0,
            "ordinary visible retail vertices must remain stable"
        );
    }

    #[test]
    #[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
    fn every_non_title_camera_drives_300_pair_scoped_scene_builds() {
        const TICKS_PER_PAIR: u32 = 300;

        let root = PathBuf::from(
            std::env::var_os("C1_STREAM_DIR")
                .expect("C1_STREAM_DIR must name local extracted retail streams"),
        );
        let expected_pairs = KNOWN_LEVELS
            .iter()
            .filter(|known| known.bootable && known.id != LevelId::TITLE)
            .count();
        let mut successes = Vec::with_capacity(expected_pairs);
        let mut failures = Vec::new();

        for known in KNOWN_LEVELS
            .iter()
            .filter(|known| known.bootable && known.id != LevelId::TITLE)
        {
            let nsd_path = root.join(known.nsd_filename());
            let nsf_path = root.join(known.nsf_filename());
            let nsd_bytes = match std::fs::read(&nsd_path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    failures.push(format!(
                        "{} NSD {}: {error}",
                        known.name,
                        nsd_path.display()
                    ));
                    continue;
                }
            };
            let nsf_bytes = match std::fs::read(&nsf_path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    failures.push(format!(
                        "{} NSF {}: {error}",
                        known.name,
                        nsf_path.display()
                    ));
                    continue;
                }
            };
            let nsd = match parse_nsd(&nsd_bytes, known.id) {
                Ok(nsd) => nsd,
                Err(error) => {
                    failures.push(format!("{} NSD parse: {error}", known.name));
                    continue;
                }
            };
            let nsf = match parse_nsf(&nsf_bytes, &nsd) {
                Ok(nsf) => nsf,
                Err(error) => {
                    failures.push(format!("{} NSF parse: {error}", known.name));
                    continue;
                }
            };
            let graph = match RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes) {
                Ok(graph) => graph,
                Err(error) => {
                    failures.push(format!("{} camera graph: {error}", known.name));
                    continue;
                }
            };
            let mut camera = match RetailCameraRuntime::new(&graph) {
                Ok(camera) => camera,
                Err(error) => {
                    failures.push(format!("{} camera start: {error}", known.name));
                    continue;
                }
            };
            // This owner is deliberately constructed once per mounted pair;
            // no parsed graph, TPAG slot, handle, or decoded pixel survives
            // into the following level.
            let mut builder = RetailSceneBuilder::new();
            let mut built_ticks = 0_u32;

            for draw_count in 0..TICKS_PER_PAIR {
                let step = match camera.update(&graph, RetailCameraInput::default()) {
                    Ok(step) => step,
                    Err(error) => {
                        failures.push(format!(
                            "{} tick {draw_count} camera at {}:{} {:#x}: {error}",
                            known.name,
                            camera.location().path.zone,
                            camera.location().path.index,
                            camera.location().progress.raw(),
                        ));
                        break;
                    }
                };
                let location = RetailSceneProgressLocation {
                    zone: step.after.path.zone,
                    path_index: step.after.path.index,
                    path_progress: step.after.progress.raw(),
                    frame_stamp: draw_count,
                    draw_count,
                };
                let scene = match builder.build_at_progress(&nsd, &nsf, &nsf_bytes, location) {
                    Ok(scene) => scene,
                    Err(error) => {
                        failures.push(format!(
                            "{} tick {draw_count} scene at {}:{} {:#x}: {error}",
                            known.name, location.zone, location.path_index, location.path_progress,
                        ));
                        break;
                    }
                };
                let expected_point = step.after.progress.point_index();
                if scene.zone != location.zone
                    || scene.path_index != location.path_index
                    || scene.path_point_index != expected_point
                    || scene.draw_count != draw_count
                {
                    failures.push(format!(
                        "{} tick {draw_count} scene identity mismatch: camera {}:{} {:#x}, scene {}:{} point {} draw {}",
                        known.name,
                        location.zone,
                        location.path_index,
                        location.path_progress,
                        scene.zone,
                        scene.path_index,
                        scene.path_point_index,
                        scene.draw_count,
                    ));
                    break;
                }
                built_ticks += 1;
            }

            if built_ticks == TICKS_PER_PAIR {
                let diagnostics = builder.diagnostics();
                eprintln!(
                    "{}: {built_ticks} camera scenes, {} graph builds, {} graph reuses, final {}:{} {:#x}",
                    known.name,
                    diagnostics.graph_builds,
                    diagnostics.graph_reuses,
                    camera.location().path.zone,
                    camera.location().path.index,
                    camera.location().progress.raw(),
                );
                successes.push(known.name);
            }
        }

        eprintln!(
            "camera/scene 300-tick successes ({}/{}): {successes:#?}",
            successes.len(),
            expected_pairs,
        );
        if !failures.is_empty() {
            eprintln!("camera/scene failures: {failures:#?}");
        }
        assert_eq!(expected_pairs, 42);
        assert!(
            failures.is_empty(),
            "camera/scene characterization failures:\n{}",
            failures.join("\n")
        );
        assert_eq!(successes.len(), expected_pairs);
    }

    #[test]
    #[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
    fn builds_n_sanity_spawn_snapshot_directly_from_raw_disc() {
        let path = PathBuf::from(
            std::env::var_os("C1_DISC_IMAGE")
                .expect("C1_DISC_IMAGE must name a legally local NTSC-U raw BIN"),
        );
        let disc_bytes =
            std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let image = DiscImage::open(&disc_bytes).unwrap();
        let streams = image.discover_streams().unwrap();
        streams.validate_complete_retail().unwrap();
        let level = LevelId::N_SANITY_BEACH;
        let nsd_stream = streams
            .get(StreamName::new(level, StreamKind::Nsd))
            .unwrap();
        let nsf_stream = streams
            .get(StreamName::new(level, StreamKind::Nsf))
            .unwrap();
        let nsd_bytes = image.read_stream(nsd_stream).unwrap();
        let nsf_bytes = image.read_stream(nsf_stream).unwrap();
        let nsd = parse_nsd(&nsd_bytes, level).unwrap();
        let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
        let scene = build_retail_scene(&nsd, &nsf, &nsf_bytes).unwrap();
        assert_eq!(
            scene.stats,
            RetailSceneStats {
                worlds: 4,
                visible_polygons: 681,
                submitted_polygons: 681,
                unique_textures: 52,
                saturated_vertices: 0,
                skipped_textured_polygons: 0,
                ..RetailSceneStats::default()
            }
        );
        let first_presented =
            build_retail_scene_at_path_point(&nsd, &nsf, &nsf_bytes, 2, 1).unwrap();
        assert_eq!(first_presented.stats.worlds, 4);
        assert_eq!(first_presented.stats.visible_polygons, 679);
    }

    #[test]
    #[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
    fn builds_every_fractional_spawn_snapshot_directly_from_raw_disc() {
        let path = PathBuf::from(
            std::env::var_os("C1_DISC_IMAGE")
                .expect("C1_DISC_IMAGE must name a legally local NTSC-U raw BIN"),
        );
        let disc_bytes =
            std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let image = DiscImage::open(&disc_bytes).unwrap();
        let streams = image.discover_streams().unwrap();
        streams.validate_complete_retail().unwrap();

        let mut built = 0_usize;
        for known in KNOWN_LEVELS.iter().filter(|known| known.bootable) {
            let nsd_stream = streams
                .get(StreamName::new(known.id, StreamKind::Nsd))
                .unwrap();
            let nsf_stream = streams
                .get(StreamName::new(known.id, StreamKind::Nsf))
                .unwrap();
            let nsd_bytes = image.read_stream(nsd_stream).unwrap();
            let nsf_bytes = image.read_stream(nsf_stream).unwrap();
            let nsd = parse_nsd(&nsd_bytes, known.id).unwrap();
            let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
            let ldat = nsd.ldat().expect("bootable retail level has LDAT");
            let scene = build_retail_scene_at_progress(
                &nsd,
                &nsf,
                &nsf_bytes,
                RetailSceneProgressLocation {
                    zone: ldat.spawn_zone,
                    path_index: u32::try_from(ldat.spawn_path_index)
                        .expect("retail spawn path is non-negative"),
                    path_progress: 0x80,
                    frame_stamp: 0,
                    draw_count: 0,
                },
            )
            .unwrap_or_else(|error| panic!("{} fractional spawn camera: {error}", known.name));
            assert_eq!(scene.path_point_index, 0);
            built += 1;
        }
        assert_eq!(built, 43);
    }

    #[test]
    #[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
    fn characterizes_every_local_retail_spawn_snapshot() {
        let root = PathBuf::from(
            std::env::var_os("C1_STREAM_DIR")
                .expect("C1_STREAM_DIR must name local extracted retail streams"),
        );
        let mut built = 0_usize;
        let mut empty = Vec::new();
        for known in KNOWN_LEVELS.iter().filter(|known| known.bootable) {
            let nsd_path = root.join(known.nsd_filename());
            let nsf_path = root.join(known.nsf_filename());
            let nsd_bytes = std::fs::read(&nsd_path)
                .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
            let nsf_bytes = std::fs::read(&nsf_path)
                .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));
            let nsd = parse_nsd(&nsd_bytes, known.id).unwrap();
            let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
            match build_retail_scene(&nsd, &nsf, &nsf_bytes) {
                Ok(scene) => {
                    built += 1;
                    if scene.stats.worlds == 0 {
                        empty.push(known.name);
                    }
                    eprintln!("{}: {:?}", known.name, scene.stats);
                    let ldat = nsd.ldat().expect("bootable retail level has LDAT");
                    let fractional = build_retail_scene_at_progress(
                        &nsd,
                        &nsf,
                        &nsf_bytes,
                        RetailSceneProgressLocation {
                            zone: ldat.spawn_zone,
                            path_index: u32::try_from(ldat.spawn_path_index)
                                .expect("retail spawn path is non-negative"),
                            path_progress: 0x80,
                            frame_stamp: 0,
                            draw_count: 0,
                        },
                    )
                    .unwrap_or_else(|error| {
                        panic!("{} fractional spawn camera: {error}", known.name)
                    });
                    assert_eq!(fractional.path_point_index, 0);
                }
                Err(error) => panic!("{}: {error}", known.name),
            }
        }
        eprintln!("empty external-transition spawn snapshots: {empty:#?}");
        assert_eq!(built, 43);
        assert_eq!(empty, ["Hog Wild", "Title / Island Map", "Whole Hog"]);
    }

    #[test]
    #[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
    fn every_local_fog_start_shades_projected_world_colors() {
        let root = PathBuf::from(
            std::env::var_os("C1_STREAM_DIR")
                .expect("C1_STREAM_DIR must name local extracted retail streams"),
        );
        let mut fog_levels = 0_usize;
        let mut shaded_vertices = 0_usize;

        for known in KNOWN_LEVELS.iter().filter(|known| known.bootable) {
            let nsd_path = root.join(known.nsd_filename());
            let nsf_path = root.join(known.nsf_filename());
            let nsd_bytes = std::fs::read(&nsd_path)
                .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
            let nsf_bytes = std::fs::read(&nsf_path)
                .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));
            let nsd = parse_nsd(&nsd_bytes, known.id).unwrap();
            let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
            let ldat = nsd.ldat().expect("bootable retail level has LDAT");
            let path_index = u32::try_from(ldat.spawn_path_index)
                .expect("retail spawn path index is non-negative");
            let graph = parse_scene_graph(
                &nsd,
                &nsf,
                &nsf_bytes,
                RetailSceneCacheKey {
                    zone: ldat.spawn_zone,
                    path_index,
                },
                0,
                false,
            )
            .unwrap_or_else(|error| panic!("{} spawn graph: {error}", known.name));
            if WorldShaderMode::from_flags(graph.zone_header.graphics.flags) != WorldShaderMode::Fog
            {
                continue;
            }

            fog_levels = fog_levels.saturating_add(1);
            let scene = build_retail_scene(&nsd, &nsf, &nsf_bytes)
                .unwrap_or_else(|error| panic!("{} fog scene: {error}", known.name));
            for command in &scene.commands {
                let CommandSource::World { polygon, .. } = command.source else {
                    continue;
                };
                let polygon = PolygonId::from_raw(
                    u16::try_from(polygon).expect("world provenance polygon fits its wire word"),
                );
                let geometry = &graph.worlds[usize::from(polygon.world_index)];
                let polygon = geometry.polygons[usize::from(polygon.polygon_index)];
                let output = match &command.primitive {
                    PrimitiveCommand::ColoredTriangle(triangle) => {
                        triangle.vertices.map(|vertex| vertex.color)
                    }
                    PrimitiveCommand::TexturedTriangle(triangle) => {
                        triangle.vertices.map(|vertex| vertex.color)
                    }
                    _ => panic!("world geometry must produce triangles"),
                };
                for (index, color) in output.into_iter().enumerate() {
                    let source =
                        geometry.vertices[usize::from(polygon.vertex_indices[index])].color;
                    if [color.r, color.g, color.b] != source {
                        shaded_vertices = shaded_vertices.saturating_add(1);
                    }
                }
            }
        }

        assert!(fog_levels > 0, "the retail corpus must contain a fog start");
        assert!(
            shaded_vertices > 0,
            "projected retail fog vertices must differ from raw WGEO colors"
        );
        eprintln!("fog starts={fog_levels}, shaded projected vertices={shaded_vertices}");
    }

    #[test]
    #[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
    fn every_local_dynamic_shader_start_reaches_projected_world_colors() {
        let root = PathBuf::from(
            std::env::var_os("C1_STREAM_DIR")
                .expect("C1_STREAM_DIR must name local extracted retail streams"),
        );
        // Lightning, combined Dark, Dark2.
        let mut level_counts = [0_usize; 3];
        let mut vertex_counts = [0_usize; 3];
        let mut changed_counts = [0_usize; 3];

        for known in KNOWN_LEVELS.iter().filter(|known| known.bootable) {
            let nsd_path = root.join(known.nsd_filename());
            let nsf_path = root.join(known.nsf_filename());
            let nsd_bytes = std::fs::read(&nsd_path)
                .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
            let nsf_bytes = std::fs::read(&nsf_path)
                .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));
            let nsd = parse_nsd(&nsd_bytes, known.id).unwrap();
            let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
            let ldat = nsd.ldat().expect("bootable retail level has LDAT");
            let path_index = u32::try_from(ldat.spawn_path_index)
                .expect("retail spawn path index is non-negative");
            let graph = parse_scene_graph(
                &nsd,
                &nsf,
                &nsf_bytes,
                RetailSceneCacheKey {
                    zone: ldat.spawn_zone,
                    path_index,
                },
                0,
                false,
            )
            .unwrap_or_else(|error| panic!("{} spawn graph: {error}", known.name));
            let mode_index = match WorldShaderMode::from_flags(graph.zone_header.graphics.flags) {
                WorldShaderMode::Lightning => 0,
                WorldShaderMode::Dark => 1,
                WorldShaderMode::Dark2 => 2,
                _ => continue,
            };
            level_counts[mode_index] = level_counts[mode_index].saturating_add(1);
            let scene = build_retail_scene(&nsd, &nsf, &nsf_bytes)
                .unwrap_or_else(|error| panic!("{} dynamic scene: {error}", known.name));
            for command in &scene.commands {
                let CommandSource::World { polygon, .. } = command.source else {
                    continue;
                };
                let polygon = PolygonId::from_raw(
                    u16::try_from(polygon).expect("world provenance polygon fits its wire word"),
                );
                let geometry = &graph.worlds[usize::from(polygon.world_index)];
                let polygon = geometry.polygons[usize::from(polygon.polygon_index)];
                let output = match &command.primitive {
                    PrimitiveCommand::ColoredTriangle(triangle) => {
                        triangle.vertices.map(|vertex| vertex.color)
                    }
                    PrimitiveCommand::TexturedTriangle(triangle) => {
                        triangle.vertices.map(|vertex| vertex.color)
                    }
                    _ => panic!("world geometry must produce triangles"),
                };
                for (index, color) in output.into_iter().enumerate() {
                    vertex_counts[mode_index] = vertex_counts[mode_index].saturating_add(1);
                    let source =
                        geometry.vertices[usize::from(polygon.vertex_indices[index])].color;
                    if [color.r, color.g, color.b] != source {
                        changed_counts[mode_index] = changed_counts[mode_index].saturating_add(1);
                    }
                }
            }
        }

        for index in 0..3 {
            assert!(level_counts[index] > 0, "dynamic mode {index} has no start");
            assert!(
                vertex_counts[index] > 0,
                "dynamic mode {index} has no vertices"
            );
            assert!(
                changed_counts[index] > 0,
                "dynamic mode {index} never changed projected color"
            );
        }
        eprintln!(
            "dynamic starts={level_counts:?}, vertices={vertex_counts:?}, changed={changed_counts:?}"
        );
    }

    #[test]
    #[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
    fn n_sanity_gool_objects_project_through_the_pair_scoped_scene() {
        const RETAIL_GLOBAL_WORDS: usize = 256;
        const RETAIL_INSTRUCTION_BUDGET: usize = 67;

        let root = PathBuf::from(
            std::env::var_os("C1_STREAM_DIR")
                .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
        );
        let level = LevelId::N_SANITY_BEACH;
        let nsd_path = root.join(StreamName::new(level, StreamKind::Nsd).filename());
        let nsf_path = root.join(StreamName::new(level, StreamKind::Nsf).filename());
        let nsd_bytes = std::fs::read(&nsd_path)
            .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
        let nsf_bytes = std::fs::read(&nsf_path)
            .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));
        let nsd = parse_nsd(&nsd_bytes, level).unwrap();
        let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
        let ldat = nsd.ldat().unwrap();
        let current_entry =
            typed_entry(&nsf, &nsd, ldat.spawn_zone, ZDAT_ENTRY_TYPE, "spawn ZDAT").unwrap();
        let current_header = ZoneHeader::parse(
            entry_item(current_entry, &nsf_bytes, 0, "spawn ZDAT header").unwrap(),
        )
        .unwrap();
        let mut owned_neighbors = Vec::new();
        for eid in current_header.neighbors {
            let entry = typed_entry(&nsf, &nsd, eid, ZDAT_ENTRY_TYPE, "neighbor ZDAT").unwrap();
            let header = ZoneHeader::parse(
                entry_item(entry, &nsf_bytes, 0, "neighbor ZDAT header").unwrap(),
            )
            .unwrap();
            let mut entities = Vec::new();
            for entity_index in 0..header.entity_count {
                let item_index =
                    usize::try_from(header.entity_item_index(entity_index).unwrap()).unwrap();
                entities.push(
                    ZoneEntity::parse(
                        entry_item(entry, &nsf_bytes, item_index, "neighbor ZDAT entity").unwrap(),
                    )
                    .unwrap(),
                );
            }
            owned_neighbors.push((eid, header.display_flags | 3, entities));
        }
        let neighbors = owned_neighbors
            .iter()
            .map(|(eid, display_flags, entities)| NeighborZone {
                eid: *eid,
                display_flags: *display_flags,
                entities: entities.as_slice(),
            })
            .collect::<Vec<_>>();
        let graph = RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes).unwrap();
        let mut camera = RetailCameraRuntime::new(&graph).unwrap();
        let mut runtime = RetailRuntime::new(RETAIL_GLOBAL_WORDS);
        let mut host = NsfProgramHost::new(&nsd, &nsf, &nsf_bytes);
        let attempts = runtime.spawn_current_zone_neighbors(&neighbors, &mut host);
        eprintln!(
            "N. Sanity spawn attempts: {} total, {} successful",
            attempts.len(),
            attempts
                .iter()
                .filter(|attempt| attempt.result.is_ok())
                .count()
        );
        assert!(attempts.iter().any(|attempt| attempt.result.is_ok()));
        let mut builder = RetailSceneBuilder::new();
        let mut peak = RetailSceneStats::default();
        let mut peak_snapshot_objects = 0_usize;

        for draw_count in 0..300 {
            let camera_step = camera.update(&graph, RetailCameraInput::default()).unwrap();
            runtime
                .run_frame(&mut host, RETAIL_INSTRUCTION_BUDGET)
                .unwrap();
            let objects = runtime.render_objects().unwrap();
            peak_snapshot_objects = peak_snapshot_objects.max(objects.len());
            if draw_count < 2 {
                eprintln!(
                    "N. Sanity frame {draw_count}: {} render-object snapshots",
                    objects.len()
                );
            }
            let main_object = runtime
                .arena()
                .main_object()
                .and_then(|arena| runtime.object_for_arena(arena));
            if draw_count == 0 {
                let mut invalid_frames = objects.clone();
                for object in &mut invalid_frames {
                    if object.display_eligible && object.animation_reference.is_some() {
                        object.animation_frame = u32::MAX;
                    }
                }
                let safely_omitted = RetailSceneBuilder::new()
                    .build_at_progress_with_objects(
                        &nsd,
                        &nsf,
                        &nsf_bytes,
                        RetailSceneProgressLocation {
                            zone: camera_step.after.path.zone,
                            path_index: camera_step.after.path.index,
                            path_progress: camera_step.after.progress.raw(),
                            frame_stamp: draw_count,
                            draw_count,
                        },
                        &invalid_frames,
                        main_object,
                    )
                    .unwrap();
                assert!(safely_omitted.stats.skipped_object_animations > 0);
            }
            let scene = builder
                .build_at_progress_with_objects(
                    &nsd,
                    &nsf,
                    &nsf_bytes,
                    RetailSceneProgressLocation {
                        zone: camera_step.after.path.zone,
                        path_index: camera_step.after.path.index,
                        path_progress: camera_step.after.progress.raw(),
                        frame_stamp: draw_count,
                        draw_count,
                    },
                    &objects,
                    main_object,
                )
                .unwrap_or_else(|error| panic!("frame {draw_count}: {error}"));
            if scene.stats.visible_objects > peak.visible_objects
                || scene.stats.submitted_object_polygons > peak.submitted_object_polygons
                || scene.stats.skipped_object_animations > peak.skipped_object_animations
            {
                peak = scene.stats;
            }
        }

        eprintln!(
            "N. Sanity 300-frame GOOL/object scene peak: {peak:?}; snapshot objects {peak_snapshot_objects}"
        );
        assert!(peak.visible_objects > 0);
        assert!(peak.submitted_object_polygons > 0);
        assert!(builder.object_models.len() <= RETAIL_OBJECT_MODEL_CACHE_FRAMES);
        assert_eq!(builder.object_models.len(), builder.object_model_lru.len());
    }

    #[test]
    #[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
    fn n_sanity_browser_order_submits_the_authored_crash_model() {
        const RETAIL_GLOBAL_WORDS: usize = 256;
        const RETAIL_INSTRUCTION_BUDGET: usize = 67;
        const MAXIMUM_BOOT_FRAMES: u32 = 60;

        fn mix_fingerprint(fingerprint: &mut u64, bytes: &[u8]) {
            for byte in bytes {
                *fingerprint ^= u64::from(*byte);
                *fingerprint = fingerprint.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }

        let root = PathBuf::from(
            std::env::var_os("C1_STREAM_DIR")
                .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
        );
        let level = LevelId::N_SANITY_BEACH;
        let nsd_path = root.join(StreamName::new(level, StreamKind::Nsd).filename());
        let nsf_path = root.join(StreamName::new(level, StreamKind::Nsf).filename());
        let nsd_bytes = std::fs::read(&nsd_path)
            .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
        let nsf_bytes = std::fs::read(&nsf_path)
            .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));
        let nsd = parse_nsd(&nsd_bytes, level).unwrap();
        let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
        let ldat = nsd.ldat().expect("N. Sanity Beach has LDAT");
        let graph = RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes).unwrap();
        let mut camera = RetailCameraRuntime::new(&graph).unwrap();

        // Reproduce the browser mount catalog and initial activation marker,
        // rather than constructing an isolated Crash object or a synthetic
        // renderer fixture.
        let mut owned_zones = BTreeMap::new();
        let mut lifecycle_zones = Vec::new();
        for node in graph.zones() {
            let entry =
                typed_entry(&nsf, &nsd, node.eid, ZDAT_ENTRY_TYPE, "reachable ZDAT").unwrap();
            let header = ZoneHeader::parse(
                entry_item(entry, &nsf_bytes, 0, "reachable ZDAT header").unwrap(),
            )
            .unwrap();
            let entities = (0..header.entity_count)
                .map(|entity_index| {
                    let item_index =
                        usize::try_from(header.entity_item_index(entity_index).unwrap()).unwrap();
                    ZoneEntity::parse(
                        entry_item(entry, &nsf_bytes, item_index, "reachable ZDAT entity").unwrap(),
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>();
            owned_zones.insert(node.eid, entities);
            lifecycle_zones.push(ZoneLifecycleZone::new(
                node.eid,
                header.display_flags,
                header.neighbors.iter().copied(),
                OrderedZoneLoadList::from(&header.load_list),
            ));
        }
        let mut lifecycle = ZoneLifecycle::new(lifecycle_zones).unwrap();
        lifecycle
            .transition_with_marker(camera.location().path.zone, true)
            .unwrap();

        // Mirror the browser's native mount order, including virtual load-list
        // opens, the heap-derived physical pool, and core-object preloads, so
        // the scene sees the real eight-slot TPAG snapshot for this pair.
        let initial_zone = lifecycle.current_zone().unwrap();
        let load_list = lifecycle.zone(initial_zone).unwrap().load_list();
        let pager = crust_sim::paging::Pager::mount_retail_level(
            &nsd,
            &nsf,
            level,
            initial_zone,
            load_list.entries().iter().copied(),
            load_list.pages().iter().copied(),
        )
        .unwrap();

        let mut runtime = RetailRuntime::new_for_level(RETAIL_GLOBAL_WORDS, level);
        runtime.set_level_state_context(RetailLevelStateContext {
            location: camera.location(),
            graphics_flags: graph
                .zone(camera.location().path.zone)
                .unwrap()
                .graphics_flags,
            box_count: 0,
            checkpoint_id: -1,
            checkpoint_translation: [0; 3],
            first_spawn: false,
            active_neighbor_zones: lifecycle.active_neighbor_zones(),
        });
        runtime
            .seed_platform_paging_state_with_capacity(
                u32::try_from(pager.page_count()).unwrap(),
                u32::try_from(pager.physical_slot_count()).unwrap(),
                pager.resolved_pages(),
                pager.page_reference_counts(),
                pager.uncounted_pages(),
            )
            .unwrap();
        let mut host = NsfProgramHost::new(&nsd, &nsf, &nsf_bytes);
        runtime
            .create_retail_core_objects(camera.location().path.zone, &mut host)
            .unwrap();
        runtime
            .create_retail_level_misc_object(camera.location().path.zone, &mut host)
            .unwrap();

        let mut builder = RetailSceneBuilder::new();
        let mut shader_render_state = RetailWorldShaderRenderState::default();
        let mut authored_submission = None;
        for boot_frame in 1..=MAXIMUM_BOOT_FRAMES {
            runtime.set_frame_timing(34, 34);
            runtime
                .set_pad_snapshot(0, RetailPadSnapshot::default())
                .unwrap();

            let neighbors = lifecycle
                .next_frame_spawn_scan()
                .into_iter()
                .map(|candidate| NeighborZone {
                    eid: candidate.zone,
                    display_flags: candidate.display_flags,
                    entities: owned_zones[&candidate.zone].as_slice(),
                })
                .collect::<Vec<_>>();
            let attempts = runtime.spawn_current_zone_neighbors(&neighbors, &mut host);
            assert!(
                attempts.iter().all(|attempt| {
                    attempt.result.is_ok()
                        || matches!(
                            attempt.result,
                            Err(crust_sim::retail_runtime::RuntimeError::Spawn(
                                crust_sim::object_arena::SpawnError::SpawnBlocked { .. }
                                    | crust_sim::object_arena::SpawnError::MainObjectAlreadyActive
                            ))
                        )
                }),
                "browser-order frame {boot_frame} reached an unexpected spawn failure: {attempts:?}"
            );

            let world_display_mask = runtime.current_display_mask();
            let draw_count = runtime.draw_count();
            let frame_stamp = runtime.next_frame_stamp();
            runtime.advance_level_shader().unwrap();
            let location_before = camera.location();
            camera.synchronize_game_state(
                runtime
                    .global_word(GAME_STATE_GLOBAL)
                    .unwrap()
                    .cast_signed(),
            );
            let camera_mode = graph.path(location_before.path).unwrap().camera_mode;
            let main_before = runtime
                .arena()
                .main_object()
                .and_then(|arena| runtime.object_for_arena(arena));
            let camera_step = if matches!(camera_mode, 5 | 6)
                && runtime.current_display_mask() & (0x2 | 0x1_0000) == 0x2
                && let Some(main) = main_before
            {
                let player = runtime.machine().object(main.vm()).unwrap();
                let signed = |index| player.register(index).unwrap().cast_signed();
                camera
                    .update_follow(
                        &graph,
                        RetailCameraFollowInput {
                            player_translation: Vec3 {
                                x: signed(process_register::TRANSLATION_X),
                                y: signed(process_register::TRANSLATION_Y),
                                z: signed(process_register::TRANSLATION_Z),
                            },
                            player_cam_zoom: signed(process_register::CAMERA_ZOOM),
                            held_buttons: 0,
                            level_id: i32::try_from(level.get()).unwrap(),
                            frames_elapsed: runtime.machine().frames_elapsed(),
                            gem_stamp: 0,
                        },
                    )
                    .unwrap()
            } else {
                camera.update(&graph, RetailCameraInput::default()).unwrap()
            };
            apply_pbak_camera_effects(
                level,
                &graph,
                &mut lifecycle,
                &mut runtime,
                &mut host,
                &camera_step,
            )
            .unwrap();
            refresh_pbak_level_context(&graph, &lifecycle, &mut runtime, camera_step.after)
                .unwrap();
            let pose = camera.pose(&graph).unwrap();
            let live_game_state = runtime
                .global_word(GAME_STATE_GLOBAL)
                .unwrap()
                .cast_signed();
            camera.synchronize_game_state(live_game_state);
            runtime.latch_frame_context(live_game_state, camera.rotation_xz(&graph).unwrap());
            runtime.set_transform_vectors_camera(RetailTransformVectorsCamera::from_retail_pose(
                pose.translation,
                pose.rotation_yxz,
                projection_distance(ldat.field_of_view).unwrap(),
            ));
            let world_shader_snapshot = runtime.world_shader_snapshot();
            let texture_frame_snapshot = pager.texture_frame_snapshot();
            let frame = runtime
                .run_frame(&mut host, RETAIL_INSTRUCTION_BUDGET)
                .unwrap();
            assert!(
                frame
                    .executions
                    .iter()
                    .all(|execution| execution.result.is_ok()),
                "browser-order frame {boot_frame} reached a checked GOOL failure: {frame:?}"
            );

            let main = runtime
                .arena()
                .main_object()
                .and_then(|arena| runtime.object_for_arena(arena))
                .expect("N. Sanity browser mount must create its main Crash object");
            let objects = runtime.render_objects().unwrap();
            let crash = objects
                .iter()
                .find(|object| object.object == main)
                .expect("the live main object must be present in the render snapshot");
            let program = crash
                .program
                .expect("Crash retains its GOOL program identity");
            assert_eq!(program.global_eid().name().as_deref(), Some("WillC"));
            let Some(source) = crash.animation_source.as_ref() else {
                continue;
            };
            let AnimationSource::ItemFive(reference) = source else {
                panic!("Crash idle must use its WillC item-five descriptor: {source:?}");
            };
            assert_eq!(crash.animation_reference, Some(*reference));
            let global =
                typed_entry(&nsf, &nsd, program.global_eid(), 11, "Crash GOOL program").unwrap();
            let animations = entry_item(global, &nsf_bytes, 5, "Crash GOOL animations").unwrap();
            let descriptor = parse_gool_animation_descriptor(
                animations,
                usize::try_from(reference.offset()).unwrap(),
            )
            .unwrap();
            let GoolAnimationDescriptor::Vertex(vertex) = descriptor else {
                panic!("Crash idle descriptor must be a vertex animation: {descriptor:?}");
            };
            assert_eq!(vertex.model_eid.name().as_deref(), Some("WiI1V"));
            let model_frame = u16::try_from(crash.animation_frame >> 8).unwrap();
            assert!(model_frame < u16::from(vertex.header.length));
            let model =
                load_object_model_frame(&nsd, &nsf, &nsf_bytes, vertex.model_eid, model_frame)
                    .unwrap();
            assert_eq!(model.frame.vertex_count(), 381);
            assert_eq!(model.frame.header.vertex_count, 381);
            assert_eq!(
                model.frame.header.geometry_eid.name().as_deref(),
                Some("WillG")
            );
            assert_eq!(model.geometry.header.polygon_count, 732);
            assert_eq!(model.geometry.polygons.len(), 732);

            let scene = builder
                .build_at_progress_with_runtime_snapshots(
                    &nsd,
                    &nsf,
                    &nsf_bytes,
                    RetailSceneProgressLocation {
                        zone: camera_step.after.path.zone,
                        path_index: camera_step.after.path.index,
                        path_progress: camera_step.after.progress.raw(),
                        frame_stamp,
                        draw_count,
                    },
                    true,
                    &objects,
                    Some(main),
                    world_display_mask,
                    ldat.field_of_view,
                    None,
                    None,
                    texture_frame_snapshot,
                    world_shader_snapshot,
                    &mut shader_render_state,
                )
                .unwrap_or_else(|error| panic!("browser-order frame {boot_frame}: {error}"));
            let commands = scene
                .commands
                .iter()
                .filter(|command| {
                    matches!(
                        command.source,
                        CommandSource::Object { handle, .. }
                            if handle == u32::from(main.vm().get())
                    )
                })
                .collect::<Vec<_>>();
            if commands.is_empty() {
                continue;
            }

            let mut source_parts = BTreeSet::new();
            let mut command_fingerprint = 0xcbf2_9ce4_8422_2325_u64;
            for command in &commands {
                let CommandSource::Object { part, .. } = command.source else {
                    unreachable!();
                };
                assert!(
                    source_parts.insert(part),
                    "Crash polygon {part} was submitted twice"
                );
                mix_fingerprint(&mut command_fingerprint, &command.depth.to_le_bytes());
                mix_fingerprint(&mut command_fingerprint, &part.to_le_bytes());
                let polygon = model.geometry.polygons[usize::from(part)];
                match (
                    model.geometry.material_for_polygon(polygon).unwrap(),
                    &command.primitive,
                ) {
                    (ObjectMaterial::Color(_), PrimitiveCommand::ColoredTriangle(triangle)) => {
                        mix_fingerprint(&mut command_fingerprint, &[0, triangle.blend as u8]);
                        mix_fingerprint(
                            &mut command_fingerprint,
                            &[u8::from(triangle.style == PrimitiveStyle::Wireframe)],
                        );
                        for vertex in triangle.vertices {
                            mix_fingerprint(
                                &mut command_fingerprint,
                                &vertex.position.x.to_le_bytes(),
                            );
                            mix_fingerprint(
                                &mut command_fingerprint,
                                &vertex.position.y.to_le_bytes(),
                            );
                            mix_fingerprint(
                                &mut command_fingerprint,
                                &vertex.position.z.to_le_bytes(),
                            );
                            mix_fingerprint(
                                &mut command_fingerprint,
                                &vertex.color.to_legacy_u32().to_le_bytes(),
                            );
                        }
                    }
                    (
                        ObjectMaterial::Texture { .. },
                        PrimitiveCommand::TexturedTriangle(triangle),
                    ) => {
                        mix_fingerprint(&mut command_fingerprint, &[1, triangle.blend as u8]);
                        mix_fingerprint(
                            &mut command_fingerprint,
                            &triangle.texture.get().to_le_bytes(),
                        );
                        for vertex in triangle.vertices {
                            mix_fingerprint(
                                &mut command_fingerprint,
                                &vertex.position.x.to_le_bytes(),
                            );
                            mix_fingerprint(
                                &mut command_fingerprint,
                                &vertex.position.y.to_le_bytes(),
                            );
                            mix_fingerprint(
                                &mut command_fingerprint,
                                &vertex.position.z.to_le_bytes(),
                            );
                            mix_fingerprint(
                                &mut command_fingerprint,
                                &vertex.color.to_legacy_u32().to_le_bytes(),
                            );
                            mix_fingerprint(
                                &mut command_fingerprint,
                                &vertex.uv.u.to_bits().to_le_bytes(),
                            );
                            mix_fingerprint(
                                &mut command_fingerprint,
                                &vertex.uv.v.to_bits().to_le_bytes(),
                            );
                        }
                    }
                    (material, primitive) => {
                        panic!("Crash polygon {part} material {material:?} emitted {primitive:?}")
                    }
                }
            }
            assert!(scene.stats.submitted_object_polygons >= commands.len());
            authored_submission = Some((
                boot_frame,
                model_frame,
                commands.len(),
                source_parts,
                command_fingerprint,
            ));
            break;
        }

        let (boot_frame, model_frame, command_count, source_parts, command_fingerprint) =
            authored_submission
                .expect("the bounded browser-order boot must submit authored Crash geometry");
        eprintln!(
            "N. Sanity browser-order Crash frame {boot_frame}: model frame {model_frame}, {command_count}/732 authored TGEO triangles, command fingerprint {command_fingerprint:#018x}"
        );
        // `GoolObjectInterpret` completes the synchronous CHLD/CHLF
        // configuration tail before yielding. Crash is therefore configured
        // and displayed during the first browser-order update, rather than
        // leaking the temporary child link across a frame boundary.
        assert_eq!((boot_frame, model_frame, command_count), (1, 0, 337));
        assert_eq!(command_fingerprint, 0xbd06_bad0_815d_91c9);
        assert_eq!(command_count, source_parts.len());
        assert!(source_parts.iter().all(|part| usize::from(*part) < 732));
    }

    #[test]
    #[ignore = "set C1_STREAM_DIR or C1_DISC_IMAGE to legally local retail data"]
    fn local_pbak_restored_scene_is_renderable() {
        const RETAIL_GLOBAL_WORDS: usize = 256;
        const PBAK_STATE_GLOBAL: usize = 105;

        let level = std::env::var("C1_PBAK_LEVEL").ok().map_or_else(
            || LevelId::new_const(0x0c),
            |value| {
                let digits = value
                    .strip_prefix("0x")
                    .or_else(|| value.strip_prefix("0X"))
                    .unwrap_or(&value);
                let raw = u32::from_str_radix(digits, 16)
                    .unwrap_or_else(|error| panic!("C1_PBAK_LEVEL {value:?}: {error}"));
                LevelId::new(raw).expect("C1_PBAK_LEVEL fits the retail filename field")
            },
        );
        let (nsd_bytes, nsf_bytes) = if let Some(root) = std::env::var_os("C1_STREAM_DIR") {
            let root = PathBuf::from(root);
            let nsd_path = root.join(StreamName::new(level, StreamKind::Nsd).filename());
            let nsf_path = root.join(StreamName::new(level, StreamKind::Nsf).filename());
            (
                std::fs::read(&nsd_path)
                    .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display())),
                std::fs::read(&nsf_path)
                    .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display())),
            )
        } else {
            let disc_path = PathBuf::from(
                std::env::var_os("C1_DISC_IMAGE")
                    .expect("set C1_STREAM_DIR or C1_DISC_IMAGE to legally local retail data"),
            );
            let disc_bytes = std::fs::read(&disc_path)
                .unwrap_or_else(|error| panic!("{}: {error}", disc_path.display()));
            let disc = DiscImage::open(&disc_bytes).unwrap();
            let streams = disc.discover_streams().unwrap();
            let nsd_stream = streams
                .get(StreamName::new(level, StreamKind::Nsd))
                .expect("disc is missing the selected PBAK NSD");
            let nsf_stream = streams
                .get(StreamName::new(level, StreamKind::Nsf))
                .expect("disc is missing the selected PBAK NSF");
            (
                disc.read_stream(nsd_stream).unwrap(),
                disc.read_stream(nsf_stream).unwrap(),
            )
        };
        let nsd = parse_nsd(&nsd_bytes, level).unwrap();
        let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
        let graph = RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes).unwrap();
        let mut camera =
            RetailCameraRuntime::at_path(&graph, graph.spawn_path(), 0, 0x600).unwrap();

        let mut owned_zones = BTreeMap::new();
        let mut lifecycle_zones = Vec::new();
        for node in graph.zones() {
            let entry = typed_entry(&nsf, &nsd, node.eid, ZDAT_ENTRY_TYPE, "PBAK ZDAT").unwrap();
            let header =
                ZoneHeader::parse(entry_item(entry, &nsf_bytes, 0, "PBAK ZDAT header").unwrap())
                    .unwrap();
            let entities = (0..header.entity_count)
                .map(|entity_index| {
                    let item_index =
                        usize::try_from(header.entity_item_index(entity_index).unwrap()).unwrap();
                    ZoneEntity::parse(
                        entry_item(entry, &nsf_bytes, item_index, "PBAK ZDAT entity").unwrap(),
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>();
            owned_zones.insert(node.eid, entities);
            lifecycle_zones.push(ZoneLifecycleZone::new(
                node.eid,
                header.display_flags,
                header.neighbors.iter().copied(),
                OrderedZoneLoadList::from(&header.load_list),
            ));
        }
        let mut lifecycle = ZoneLifecycle::new(lifecycle_zones).unwrap();
        lifecycle
            .transition_with_marker(camera.location().path.zone, true)
            .unwrap();

        let mut runtime = RetailRuntime::new_for_level(RETAIL_GLOBAL_WORDS, level);
        runtime
            .set_global_word(crust_sim::gool::GAME_STATE_GLOBAL, 0x600)
            .unwrap();
        runtime.set_global_word(PBAK_STATE_GLOBAL, 3).unwrap();
        runtime.set_level_state_context(RetailLevelStateContext {
            location: camera.location(),
            graphics_flags: graph
                .zone(camera.location().path.zone)
                .unwrap()
                .graphics_flags,
            box_count: 0,
            checkpoint_id: -1,
            checkpoint_translation: [0; 3],
            first_spawn: false,
            active_neighbor_zones: lifecycle.active_neighbor_zones(),
        });
        let mut host = NsfProgramHost::new(&nsd, &nsf, &nsf_bytes);
        runtime
            .create_retail_core_objects(camera.location().path.zone, &mut host)
            .unwrap()
            .expect("PBAK gameplay levels create the three retail HUD roots");
        let initial_neighbors = lifecycle
            .next_frame_spawn_scan()
            .into_iter()
            .map(|candidate| NeighborZone {
                eid: candidate.zone,
                display_flags: candidate.display_flags,
                entities: owned_zones[&candidate.zone].as_slice(),
            })
            .collect::<Vec<_>>();
        let attempts = runtime.spawn_current_zone_neighbors(&initial_neighbors, &mut host);
        assert!(
            attempts.iter().any(|attempt| attempt.result.is_ok()),
            "selected PBAK level initial spawn scan must create Crash"
        );
        assert!(runtime.arena().main_object().is_some());

        let follow_input = |runtime: &RetailRuntime, held_buttons: u32| {
            let main = runtime
                .arena()
                .main_object()
                .and_then(|arena| runtime.object_for_arena(arena))
                .unwrap();
            let player = runtime.machine().object(main.vm()).unwrap();
            let signed = |index| player.register(index).unwrap().cast_signed();
            RetailCameraFollowInput {
                player_translation: Vec3 {
                    x: signed(process_register::TRANSLATION_X),
                    y: signed(process_register::TRANSLATION_Y),
                    z: signed(process_register::TRANSLATION_Z),
                },
                player_cam_zoom: signed(process_register::CAMERA_ZOOM),
                held_buttons,
                level_id: i32::try_from(level.get()).unwrap(),
                frames_elapsed: runtime.machine().frames_elapsed(),
                gem_stamp: 0,
            }
        };
        let publish_camera = |runtime: &mut RetailRuntime, camera: &RetailCameraRuntime| {
            let pose = camera.pose(&graph).unwrap();
            let live_game_state = runtime
                .global_word(GAME_STATE_GLOBAL)
                .unwrap()
                .cast_signed();
            runtime.latch_frame_context(live_game_state, camera.rotation_xz(&graph).unwrap());
            runtime.set_transform_vectors_camera(RetailTransformVectorsCamera::from_retail_pose(
                pose.translation,
                pose.rotation_yxz,
                projection_distance(nsd.ldat().unwrap().field_of_view).unwrap(),
            ));
        };
        let update_camera = |camera: &mut RetailCameraRuntime,
                             runtime: &RetailRuntime,
                             held_buttons: u32| {
            camera.synchronize_game_state(
                runtime
                    .global_word(GAME_STATE_GLOBAL)
                    .unwrap()
                    .cast_signed(),
            );
            let mode = graph.path(camera.location().path).unwrap().camera_mode;
            let display_mask = runtime.current_display_mask();
            if runtime.arena().main_object().is_none() || display_mask & (0x2 | 0x1_0000) != 0x2 {
                Ok(camera.stationary_step())
            } else if matches!(mode, 5 | 6) {
                camera.update_follow(&graph, follow_input(runtime, held_buttons))
            } else {
                camera.update(&graph, RetailCameraInput::default())
            }
        };
        let initial_camera_step = update_camera(&mut camera, &runtime, 0).unwrap();
        apply_pbak_camera_effects(
            level,
            &graph,
            &mut lifecycle,
            &mut runtime,
            &mut host,
            &initial_camera_step,
        )
        .unwrap();
        refresh_pbak_level_context(&graph, &lifecycle, &mut runtime, initial_camera_step.after)
            .unwrap();
        camera.synchronize_game_state(
            runtime
                .global_word(GAME_STATE_GLOBAL)
                .unwrap()
                .cast_signed(),
        );
        publish_camera(&mut runtime, &camera);
        runtime.set_frame_timing(34, 34);
        runtime
            .set_pad_snapshot(0, RetailPadSnapshot::default())
            .unwrap();
        runtime.run_frame(&mut host, 67).unwrap();

        let mut random_seed_b = 0_u32;
        let prepared = prepare_pair_pbak(&nsd, &nsf, &nsf_bytes, &graph, &mut random_seed_b)
            .unwrap()
            .expect("selected level has one PBAK recording");
        assert_eq!(prepared.snapshot.level, level);
        let trace_frames = std::env::var("C1_PBAK_FRAMES").ok().map_or_else(
            || prepared.frame_count(),
            |value| {
                value
                    .parse::<usize>()
                    .unwrap_or_else(|error| panic!("C1_PBAK_FRAMES {value:?}: {error}"))
            },
        );
        assert!(
            trace_frames <= prepared.frame_count(),
            "the render trace cannot outlive the recording"
        );
        if level == LevelId::new_const(0x0c) {
            assert_eq!(prepared.eid.name().as_deref(), Some("pb0cB"));
            assert_eq!(prepared.snapshot.location.progress.raw(), 0);
        }
        let mut playback = RetailPbakPlayback::new(prepared.clone());
        runtime
            .create_retail_demo_caption(camera.location().path.zone, &mut host)
            .unwrap();
        runtime
            .install_retail_demo_start(
                prepared.snapshot.clone(),
                prepared.player.seed(),
                prepared.crash_bound,
            )
            .unwrap();

        let restart_plan = lifecycle
            .plan_hard_restart(prepared.snapshot.location.path.zone, true)
            .unwrap();
        let outcome = runtime.restart_saved_level(&mut host).unwrap();
        let RetailRestartOutcome::Restarted(report) = outcome else {
            panic!("same-level PBAK restore requested a remount");
        };
        lifecycle.commit_hard_restart(&restart_plan).unwrap();
        let restart_camera_step = camera
            .level_update(
                &graph,
                report.snapshot.location.path,
                report.snapshot.location.progress.raw(),
                report.level_update_flags,
            )
            .unwrap();
        assert_eq!(restart_camera_step.after, report.snapshot.location);
        if level == LevelId::new_const(0x0c) {
            assert_eq!(report.snapshot.location.progress.raw(), 0);
        }
        refresh_pbak_level_context(&graph, &lifecycle, &mut runtime, report.snapshot.location)
            .unwrap();

        playback.mark_started();
        let mut pad = RetailPadSnapshot::default();
        let mut builder = RetailSceneBuilder::new();
        let mut observed_fruit_generations: [Option<RuntimeObjectHandle>; 2] = [None, None];
        let mut finish_outcome = None;
        let mut finish_island_rotation = None;
        let mut pad_boundaries = 0_usize;
        let maximum_wall_frames = trace_frames.saturating_mul(8).max(trace_frames + 512);
        for pbak_frame in 0..maximum_wall_frames {
            let restored_neighbors = lifecycle
                .next_frame_spawn_scan()
                .into_iter()
                .map(|candidate| NeighborZone {
                    eid: candidate.zone,
                    display_flags: candidate.display_flags,
                    entities: owned_zones[&candidate.zone].as_slice(),
                })
                .collect::<Vec<_>>();
            runtime.spawn_current_zone_neighbors(&restored_neighbors, &mut host);

            let draw_count = runtime.draw_count();
            let display_mask = runtime.current_display_mask();
            let timing = playback.frame_timing(34, 34).unwrap();
            runtime.set_frame_timing(timing.prior.current, timing.prior.period);
            let camera_step = update_camera(&mut camera, &runtime, pad.held).unwrap();
            if level == LevelId::new_const(0x0c) && trace_frames <= 232 {
                assert_eq!(camera_step.after.path, report.snapshot.location.path);
            }
            apply_pbak_camera_effects(
                level,
                &graph,
                &mut lifecycle,
                &mut runtime,
                &mut host,
                &camera_step,
            )
            .unwrap_or_else(|error| {
                panic!("{} frame {pbak_frame} camera effect: {error}", prepared.eid)
            });
            refresh_pbak_level_context(&graph, &lifecycle, &mut runtime, camera_step.after)
                .unwrap();
            camera.synchronize_game_state(
                runtime
                    .global_word(GAME_STATE_GLOBAL)
                    .unwrap()
                    .cast_signed(),
            );
            publish_camera(&mut runtime, &camera);
            let runtime_frame = runtime
                .run_frame_with_traversal_hook(&mut host, 67, |runtime, host, _point| {
                    pad_boundaries += 1;
                    let (input, end) = playback.advance_pad_boundary(0);
                    runtime.set_frame_timing(timing.crash.current, timing.crash.period);
                    let previous = pad;
                    pad = RetailPadSnapshot {
                        tapped: input.held & !previous.held,
                        held: input.held,
                        held_previous: previous.held,
                        tapped_previous: previous.tapped,
                        held_previous_2: previous.held_previous,
                    };
                    if end.is_some() {
                        finish_island_rotation = Some(
                            runtime
                                .global_word(ISLAND_CAMERA_ROTATION_GLOBAL)
                                .map_err(crust_sim::retail_runtime::RuntimeError::Vm)?,
                        );
                        runtime
                            .set_pad_snapshot(0, pbak_event_pad_snapshot(previous, pad))
                            .map_err(crust_sim::retail_runtime::RuntimeError::Vm)?;
                        finish_outcome = Some(runtime.finish_retail_demo(host)?);
                    }
                    runtime
                        .set_pad_snapshot(0, pad)
                        .map_err(crust_sim::retail_runtime::RuntimeError::Vm)
                })
                .unwrap_or_else(|error| panic!("{} frame {pbak_frame}: {error:?}", prepared.eid));
            assert!(
                runtime_frame
                    .executions
                    .iter()
                    .all(|execution| execution.result.is_ok()),
                "{} frame {pbak_frame} reached a checked object failure: {:?}",
                prepared.eid,
                runtime_frame
                    .executions
                    .iter()
                    .filter(|execution| execution.result.is_err())
                    .collect::<Vec<_>>()
            );

            if runtime.machine().level_restart_requested() {
                let saved_location = runtime
                    .saved_level_state()
                    .expect("PBAK death restart retains its installed snapshot")
                    .location;
                let restart_plan = lifecycle
                    .plan_hard_restart(saved_location.path.zone, true)
                    .unwrap();
                let outcome = runtime.restart_saved_level(&mut host).unwrap();
                let RetailRestartOutcome::Restarted(death_report) = outcome else {
                    panic!("same-level PBAK death requested an external remount");
                };
                lifecycle.commit_hard_restart(&restart_plan).unwrap();
                let death_camera_step = camera
                    .level_update(
                        &graph,
                        death_report.snapshot.location.path,
                        death_report.snapshot.location.progress.raw(),
                        death_report.level_update_flags,
                    )
                    .unwrap();
                refresh_pbak_level_context(
                    &graph,
                    &lifecycle,
                    &mut runtime,
                    death_camera_step.after,
                )
                .unwrap();
                continue;
            }

            let objects = runtime.render_objects().unwrap();
            let main_object = runtime
                .arena()
                .main_object()
                .and_then(|arena| runtime.object_for_arena(arena));
            let location = RetailSceneProgressLocation {
                zone: camera_step.after.path.zone,
                path_index: camera_step.after.path.index,
                path_progress: camera_step.after.progress.raw(),
                frame_stamp: draw_count,
                draw_count,
            };
            builder
                .build_at_progress_with_objects_and_world_display_mask(
                    &nsd,
                    &nsf,
                    &nsf_bytes,
                    location,
                    &objects,
                    main_object,
                    display_mask,
                )
                .unwrap_or_else(|error| {
                    panic!(
                        "{} frame {pbak_frame} scene at progress {:#x}: {error}",
                        prepared.eid, location.path_progress
                    )
                });

            if level == LevelId::new_const(0x0c) && (189..=210).contains(&pbak_frame) {
                const FRUIT_SCALE_SEQUENCE: [[i32; 3]; 11] = [
                    [2_764, 3_456, 4_915],
                    [-253, 658, 936],
                    [-253, 658, 936],
                    [-278, 722, 936],
                    [-306, 792, 936],
                    [-336, 869, 936],
                    [-369, 953, 936],
                    [-406, 1_046, 936],
                    [-446, 1_148, 936],
                    [-490, 1_260, 936],
                    [-538, 1_383, 936],
                ];
                let fruit = objects
                    .iter()
                    .filter(|object| {
                        object.executable == 3
                            && object.subtype == 13
                            && object.program.is_some_and(|program| {
                                program.global_eid().name().as_deref() == Some("FruiC")
                            })
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    fruit.len(),
                    1,
                    "{} frame {pbak_frame} authored FruiC child count",
                    prepared.eid,
                );
                let fruit = fruit[0];
                let (generation_index, sequence_index) = if pbak_frame < 200 {
                    (0, pbak_frame - 189)
                } else {
                    (1, pbak_frame - 200)
                };
                if sequence_index == 0 {
                    if generation_index == 1 {
                        let prior = observed_fruit_generations[0]
                            .expect("the preceding FruiC generation was observed");
                        assert_eq!(fruit.object.vm(), prior.vm());
                        assert_ne!(fruit.object.arena(), prior.arena());
                    }
                    observed_fruit_generations[generation_index] = Some(fruit.object);
                }
                assert_eq!(
                    observed_fruit_generations[generation_index],
                    Some(fruit.object),
                    "{} frame {pbak_frame} must retain one physical incarnation",
                    prepared.eid,
                );
                assert_eq!(
                    runtime.object_for_arena(fruit.object.arena()),
                    Some(fruit.object)
                );
                assert_eq!(runtime.object_for_vm(fruit.object.vm()), Some(fruit.object));
                assert_eq!(
                    fruit.transform.scale, FRUIT_SCALE_SEQUENCE[sequence_index],
                    "{} frame {pbak_frame} FruiC scale",
                    prepared.eid,
                );
                assert_eq!(
                    retail_sprite_shrink(fruit.transform.scale[0]),
                    Ok(0),
                    "{} frame {pbak_frame} FruiC shrink",
                    prepared.eid,
                );
                let vm = runtime.machine().object(fruit.object.vm()).unwrap();
                assert_eq!(vm.state(), if sequence_index == 0 { 8 } else { 12 });
                assert_eq!(
                    vm.register(process_register::ANIMATION_STAMP).unwrap(),
                    u32::try_from(pbak_frame).unwrap() + 1,
                );
            }
            if level == LevelId::new_const(0x0c) && (211..=217).contains(&pbak_frame) {
                assert!(
                    objects.iter().all(|object| {
                        object.executable != 3
                            || object.subtype != 13
                            || !object.program.is_some_and(|program| {
                                program.global_eid().name().as_deref() == Some("FruiC")
                            })
                    }),
                    "{} frame {pbak_frame} retained a reclaimed FruiC child",
                    prepared.eid,
                );
            }
            if pad_boundaries == trace_frames {
                break;
            }
        }
        assert_eq!(
            pad_boundaries, trace_frames,
            "the requested recorded pad boundaries must run within the bounded wall window"
        );
        if level == LevelId::new_const(0x0c) && trace_frames >= 211 {
            assert!(
                observed_fruit_generations
                    .into_iter()
                    .all(|handle| handle.is_some()),
                "pb0cB did not reach both source-lifetime FruiC generations"
            );
        }
        if trace_frames == prepared.frame_count() {
            assert!(
                playback.is_returning(),
                "{pad_boundaries} Crash pad boundaries ran across {trace_frames} wall frames"
            );
            let finish_outcome = finish_outcome
                .as_ref()
                .expect("the final recorded pad boundary must complete the retail demo handshake");
            let finish_island_rotation = finish_island_rotation
                .expect("the final recorded pad boundary must sample global 64");
            match finish_outcome {
                RetailDemoFinishOutcome::CaptionEvent { .. } => {
                    assert_eq!(runtime.global_word(PBAK_STATE_GLOBAL), Ok(3));
                }
                RetailDemoFinishOutcome::CaptionEventFault { .. } => {
                    panic!("the legal recording reached a malformed caption-event handler")
                }
                RetailDemoFinishOutcome::Released => {
                    panic!("the live PBAK caption must retain the authored return lock")
                }
            }
            if level == LevelId::new_const(0x0c) {
                assert_eq!(
                    finish_island_rotation, 0,
                    "pb0cB proves the caption handoff is independent of island-camera global 64"
                );
            }
        }
    }

    #[test]
    #[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
    fn every_local_gameplay_pair_materializes_retail_core_hud_roots() {
        const RETAIL_GLOBAL_WORDS: usize = 256;

        let root = PathBuf::from(
            std::env::var_os("C1_STREAM_DIR")
                .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
        );
        let mut created_levels = 0_usize;
        for known in KNOWN_LEVELS.iter().filter(|known| known.bootable) {
            let nsd_path = root.join(known.nsd_filename());
            let nsf_path = root.join(known.nsf_filename());
            let nsd_bytes = std::fs::read(&nsd_path)
                .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
            let nsf_bytes = std::fs::read(&nsf_path)
                .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));
            let nsd = parse_nsd(&nsd_bytes, known.id).unwrap();
            let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
            let current_zone = nsd.ldat().unwrap().spawn_zone;
            let mut host = NsfProgramHost::new(&nsd, &nsf, &nsf_bytes);
            let mut runtime = RetailRuntime::new_for_level(RETAIL_GLOBAL_WORDS, known.id);
            let created = runtime
                .create_retail_core_objects(current_zone, &mut host)
                .unwrap();

            if matches!(
                known.id,
                LevelId::TITLE | LevelId::LEVEL_COMPLETE | LevelId::INTRO | LevelId::ENDING
            ) {
                assert_eq!(
                    created, None,
                    "{} must not create gameplay HUDs",
                    known.name
                );
                continue;
            }

            let objects = created.unwrap_or_else(|| panic!("{} omitted gameplay HUDs", known.name));
            created_levels += 1;
            for (global, object, subtype) in [
                (7, objects.life, 0),
                (6, objects.fruit, 1),
                (14, objects.pickup, 5),
            ] {
                assert_eq!(
                    runtime.global_word(global).unwrap(),
                    CollisionObjectReference::new(object.vm()).to_word(),
                    "{} global {global}",
                    known.name,
                );
                let spawned = runtime.arena().get(object.arena()).unwrap();
                assert_eq!(
                    spawned.zone(),
                    Eid::NONE,
                    "{} subtype {subtype}",
                    known.name
                );
                assert_eq!(
                    (spawned.origin().executable(), spawned.origin().subtype()),
                    (4, subtype),
                    "{} subtype {subtype}",
                    known.name,
                );
                assert_eq!(
                    runtime
                        .machine()
                        .object(object.vm())
                        .unwrap()
                        .program_identity()
                        .unwrap()
                        .global_eid()
                        .name()
                        .as_deref(),
                    Some("DispC"),
                    "{} subtype {subtype}",
                    known.name,
                );
            }
        }
        assert_eq!(created_levels, 39);
    }

    #[test]
    #[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
    fn characterizes_live_non_vertex_commands_across_local_retail_boots() {
        const RETAIL_GLOBAL_WORDS: usize = 256;
        const RETAIL_INSTRUCTION_BUDGET: usize = 67;
        const FRAMES_PER_LEVEL: u32 = 180;

        let root = PathBuf::from(
            std::env::var_os("C1_STREAM_DIR")
                .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
        );
        let mut live_sprites = 0_usize;
        let mut live_fragments = 0_usize;
        let mut live_texts = 0_usize;
        let mut live_dynamic_fonts = 0_usize;
        let mut live_2d_cvtx = 0_usize;
        let mut emitted_sprite_quads = 0_usize;
        let mut emitted_fragment_quads = 0_usize;
        let mut emitted_text_quads = 0_usize;
        let mut live_mode_four_vertices = 0_usize;
        let mut verified_mode_four_writebacks = 0_usize;
        let mut changed_mode_four_writebacks = 0_usize;
        let mut emitted_mode_four_primitives = 0_usize;
        let mut observed_levels = std::collections::BTreeSet::new();

        for known in KNOWN_LEVELS
            .iter()
            .filter(|known| known.bootable && known.id != LevelId::TITLE)
        {
            let nsd_path = root.join(known.nsd_filename());
            let nsf_path = root.join(known.nsf_filename());
            let nsd_bytes = std::fs::read(&nsd_path)
                .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
            let nsf_bytes = std::fs::read(&nsf_path)
                .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));
            let nsd = parse_nsd(&nsd_bytes, known.id).unwrap();
            let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
            let ldat = nsd.ldat().unwrap();
            let current =
                typed_entry(&nsf, &nsd, ldat.spawn_zone, ZDAT_ENTRY_TYPE, "spawn ZDAT").unwrap();
            let current_header =
                ZoneHeader::parse(entry_item(current, &nsf_bytes, 0, "spawn ZDAT header").unwrap())
                    .unwrap();
            let mut owned_neighbors = Vec::new();
            for eid in current_header.neighbors {
                let entry = typed_entry(&nsf, &nsd, eid, ZDAT_ENTRY_TYPE, "neighbor ZDAT").unwrap();
                let header = ZoneHeader::parse(
                    entry_item(entry, &nsf_bytes, 0, "neighbor ZDAT header").unwrap(),
                )
                .unwrap();
                let entities = (0..header.entity_count)
                    .map(|entity_index| {
                        let item_index =
                            usize::try_from(header.entity_item_index(entity_index).unwrap())
                                .unwrap();
                        ZoneEntity::parse(
                            entry_item(entry, &nsf_bytes, item_index, "neighbor ZDAT entity")
                                .unwrap(),
                        )
                        .unwrap()
                    })
                    .collect::<Vec<_>>();
                owned_neighbors.push((eid, header.display_flags | 3, entities));
            }
            let neighbors = owned_neighbors
                .iter()
                .map(|(eid, display_flags, entities)| NeighborZone {
                    eid: *eid,
                    display_flags: *display_flags,
                    entities,
                })
                .collect::<Vec<_>>();
            let active_neighbor_zones = neighbors
                .iter()
                .filter(|neighbor| neighbor.display_flags & 2 != 0)
                .map(|neighbor| neighbor.eid)
                .collect::<Vec<_>>();
            let graph = RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes).unwrap();
            let mut camera = RetailCameraRuntime::new(&graph).unwrap();
            let mut runtime = RetailRuntime::new_for_level(RETAIL_GLOBAL_WORDS, known.id);
            runtime.set_level_state_context(RetailLevelStateContext {
                location: camera.location(),
                graphics_flags: graph
                    .zone(camera.location().path.zone)
                    .unwrap()
                    .graphics_flags,
                box_count: 0,
                checkpoint_id: -1,
                checkpoint_translation: [0; 3],
                first_spawn: false,
                active_neighbor_zones: active_neighbor_zones.clone(),
            });
            let mut host = NsfProgramHost::new(&nsd, &nsf, &nsf_bytes);
            runtime
                .create_retail_core_objects(camera.location().path.zone, &mut host)
                .unwrap();
            let attempts = runtime.spawn_current_zone_neighbors(&neighbors, &mut host);
            if !attempts.iter().any(|attempt| attempt.result.is_ok()) {
                continue;
            }
            let mut builder = RetailSceneBuilder::new();
            for draw_count in 0..FRAMES_PER_LEVEL {
                runtime.advance_level_shader().unwrap();
                let camera_step = camera.update(&graph, RetailCameraInput::default()).unwrap();
                runtime.set_level_state_context(RetailLevelStateContext {
                    location: camera_step.after,
                    graphics_flags: graph
                        .zone(camera_step.after.path.zone)
                        .unwrap()
                        .graphics_flags,
                    box_count: 0,
                    checkpoint_id: -1,
                    checkpoint_translation: [0; 3],
                    first_spawn: false,
                    active_neighbor_zones: active_neighbor_zones.clone(),
                });
                let pose = camera.pose(&graph).unwrap();
                let runtime_camera = RetailTransformVectorsCamera::from_retail_pose(
                    pose.translation,
                    pose.rotation_yxz,
                    projection_distance(ldat.field_of_view).unwrap(),
                );
                runtime.set_transform_vectors_camera(runtime_camera);
                let object_camera = runtime_camera.for_object_display(
                    graph
                        .zone(camera_step.after.path.zone)
                        .unwrap()
                        .graphics_flags,
                    draw_count,
                );
                runtime
                    .run_frame(&mut host, RETAIL_INSTRUCTION_BUDGET)
                    .unwrap_or_else(|error| panic!("{} frame {draw_count}: {error:?}", known.name));
                let objects = runtime.render_objects().unwrap();
                let mut has_live_non_vertex = false;
                let mut sprite_handles = std::collections::BTreeSet::new();
                let mut fragment_handles = std::collections::BTreeSet::new();
                let mut text_handles = std::collections::BTreeSet::new();
                let mut mode_four_handles = std::collections::BTreeSet::new();
                let current_entry = typed_entry(
                    &nsf,
                    &nsd,
                    camera_step.after.path.zone,
                    ZDAT_ENTRY_TYPE,
                    "camera ZDAT",
                )
                .unwrap();
                let current_header = ZoneHeader::parse(
                    entry_item(current_entry, &nsf_bytes, 0, "camera ZDAT header").unwrap(),
                )
                .unwrap();
                let main_object = runtime
                    .arena()
                    .main_object()
                    .and_then(|arena| runtime.object_for_arena(arena));
                for object in objects.iter().filter(|object| object.display_eligible) {
                    let (Some(program), Some(reference)) =
                        (object.program, object.animation_reference)
                    else {
                        continue;
                    };
                    let global =
                        typed_entry(&nsf, &nsd, program.global_eid(), 11, "GOOL program").unwrap();
                    let animations = entry_item(global, &nsf_bytes, 5, "GOOL animations").unwrap();
                    let descriptor = parse_gool_animation_descriptor(
                        animations,
                        usize::try_from(reference.offset()).unwrap(),
                    )
                    .unwrap();
                    match descriptor {
                        GoolAnimationDescriptor::Sprite(_) => {
                            live_sprites += 1;
                            has_live_non_vertex = true;
                            sprite_handles.insert(u32::from(object.object.vm().get()));
                        }
                        GoolAnimationDescriptor::Fragment(_) => {
                            live_fragments += 1;
                            has_live_non_vertex = true;
                            fragment_handles.insert(u32::from(object.object.vm().get()));
                        }
                        GoolAnimationDescriptor::Text(_) => {
                            live_texts += 1;
                            live_dynamic_fonts +=
                                usize::from(object.text_font_override_word_offset != 0);
                            has_live_non_vertex = true;
                            text_handles.insert(u32::from(object.object.vm().get()));
                        }
                        GoolAnimationDescriptor::Vertex(vertex) => {
                            let vertex_kind = nsf
                                .resolve_entry(&nsd, vertex.model_eid)
                                .ok()
                                .and_then(|entry| {
                                    ObjectVertexKind::from_entry_type(entry.entry_type).ok()
                                });
                            if object.status_b & 0x200 != 0
                                && vertex_kind == Some(ObjectVertexKind::Colored)
                            {
                                live_2d_cvtx += 1;
                            }
                            if current_header.graphics.unknown_a == 4
                                && object.display_mask & 0x1_0000 == 0
                                && Some(object.object) != main_object
                                && object.status_b & 0x400 == 0
                                && !(vertex_kind == Some(ObjectVertexKind::Colored)
                                    && object.status_b & 0x200 != 0)
                            {
                                live_mode_four_vertices += 1;
                                has_live_non_vertex = true;
                                mode_four_handles.insert(u32::from(object.object.vm().get()));

                                let camera_depth = object_camera
                                    .camera_space_point(object.transform.translation)[2];
                                let projection = i32::try_from(object_camera.screen_projection)
                                    .unwrap_or(i32::MAX);
                                if object.status_b & 0x4_0000 != 0 || projection < camera_depth {
                                    let expected = apply_retail_object_zone_shader(
                                        4,
                                        vertex_kind.unwrap(),
                                        object.colors,
                                        current_header.graphics.object_colors.words,
                                        camera_depth,
                                        object_zone_depth_anchor(&nsd, &current_header),
                                        object.dark_reference_translation.map(
                                            |reference_translation| ObjectDarkShaderInput {
                                                reference_translation,
                                                object_translation: object.transform.translation,
                                                dark_distance: object.dark_distance,
                                            },
                                        ),
                                    )
                                    .unwrap()
                                    .unwrap();
                                    assert_eq!(object.colors, expected.colors);
                                    verified_mode_four_writebacks += 1;
                                    if object.status_b & 0x10_0000 == 0 {
                                        let live = runtime
                                            .machine()
                                            .object(object.object.vm())
                                            .unwrap()
                                            .retail_colors();
                                        assert_eq!(live, &expected.colors);
                                        changed_mode_four_writebacks += usize::from(
                                            expected.colors[..12]
                                                != current_header.graphics.object_colors.words
                                                    [..12],
                                        );
                                    }
                                }
                            }
                        }
                        GoolAnimationDescriptor::Font(_) => {}
                    }
                }
                if !has_live_non_vertex {
                    continue;
                }
                let scene = builder
                    .build_at_progress_with_objects(
                        &nsd,
                        &nsf,
                        &nsf_bytes,
                        RetailSceneProgressLocation {
                            zone: camera_step.after.path.zone,
                            path_index: camera_step.after.path.index,
                            path_progress: camera_step.after.progress.raw(),
                            frame_stamp: draw_count,
                            draw_count,
                        },
                        &objects,
                        main_object,
                    )
                    .unwrap_or_else(|error| panic!("{} frame {draw_count}: {error}", known.name));
                let mut frame_quads = 0_usize;
                for command in &scene.commands {
                    let CommandSource::Object { handle, .. } = command.source else {
                        continue;
                    };
                    if mode_four_handles.contains(&handle) {
                        emitted_mode_four_primitives += 1;
                    }
                    if !matches!(&command.primitive, PrimitiveCommand::TexturedQuad(_)) {
                        continue;
                    }
                    if sprite_handles.contains(&handle) {
                        emitted_sprite_quads += 1;
                    }
                    if fragment_handles.contains(&handle) {
                        emitted_fragment_quads += 1;
                    }
                    if text_handles.contains(&handle) {
                        emitted_text_quads += 1;
                    }
                    frame_quads += 1;
                }
                if frame_quads > 0 {
                    observed_levels.insert(known.name);
                }
            }
        }

        eprintln!(
            "live non-vertex boot frames: sprites={live_sprites}, fragments={live_fragments}, texts={live_texts} ({live_dynamic_fonts} dynamic-font overrides), 2D CVTX={live_2d_cvtx}, mode-4 vertices={live_mode_four_vertices} ({verified_mode_four_writebacks} verified, {changed_mode_four_writebacks} changed+persisted), sprite quads={emitted_sprite_quads}, fragment quads={emitted_fragment_quads}, text quads={emitted_text_quads}, mode-4 primitives={emitted_mode_four_primitives}, levels={observed_levels:?}"
        );
        assert!(live_sprites > 0);
        assert!(live_fragments > 0);
        assert!(emitted_sprite_quads > 0);
        assert!(emitted_fragment_quads > 0);
        assert!(live_texts > 0);
        assert!(emitted_text_quads > 0);
        assert!(live_mode_four_vertices > 0);
        assert!(verified_mode_four_writebacks > 0);
        assert!(changed_mode_four_writebacks > 0);
        assert!(emitted_mode_four_primitives > 0);
        // Fragment/2D-CVTX descriptors are corpus-covered by renderer tests;
        // an idle direct boot is not guaranteed to enter those object states.
    }

    #[test]
    #[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
    fn n_sanity_authored_pause_panel_blinks_five_willt_fragment_quads() {
        const RETAIL_GLOBAL_WORDS: usize = 256;
        const RETAIL_INSTRUCTION_BUDGET: usize = 67;

        let root = PathBuf::from(
            std::env::var_os("C1_STREAM_DIR")
                .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
        );
        let level = LevelId::N_SANITY_BEACH;
        let nsd_path = root.join(StreamName::new(level, StreamKind::Nsd).filename());
        let nsf_path = root.join(StreamName::new(level, StreamKind::Nsf).filename());
        let nsd_bytes = std::fs::read(&nsd_path)
            .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
        let nsf_bytes = std::fs::read(&nsf_path)
            .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));
        let nsd = parse_nsd(&nsd_bytes, level).unwrap();
        let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
        let ldat = nsd.ldat().unwrap();
        let current_entry =
            typed_entry(&nsf, &nsd, ldat.spawn_zone, ZDAT_ENTRY_TYPE, "spawn ZDAT").unwrap();
        let current_header = ZoneHeader::parse(
            entry_item(current_entry, &nsf_bytes, 0, "spawn ZDAT header").unwrap(),
        )
        .unwrap();
        let mut owned_neighbors = Vec::new();
        for eid in current_header.neighbors {
            let entry = typed_entry(&nsf, &nsd, eid, ZDAT_ENTRY_TYPE, "neighbor ZDAT").unwrap();
            let header = ZoneHeader::parse(
                entry_item(entry, &nsf_bytes, 0, "neighbor ZDAT header").unwrap(),
            )
            .unwrap();
            let entities = (0..header.entity_count)
                .map(|entity_index| {
                    let item_index =
                        usize::try_from(header.entity_item_index(entity_index).unwrap()).unwrap();
                    ZoneEntity::parse(
                        entry_item(entry, &nsf_bytes, item_index, "neighbor ZDAT entity").unwrap(),
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>();
            owned_neighbors.push((eid, header.display_flags | 3, entities));
        }
        let neighbors = owned_neighbors
            .iter()
            .map(|(eid, display_flags, entities)| NeighborZone {
                eid: *eid,
                display_flags: *display_flags,
                entities,
            })
            .collect::<Vec<_>>();
        let active_neighbor_zones = neighbors
            .iter()
            .filter(|neighbor| neighbor.display_flags & 2 != 0)
            .map(|neighbor| neighbor.eid)
            .collect::<Vec<_>>();
        let graph = RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes).unwrap();
        let mut camera = RetailCameraRuntime::new(&graph).unwrap();
        let mut runtime = RetailRuntime::new_for_level(RETAIL_GLOBAL_WORDS, level);
        runtime.set_level_state_context(RetailLevelStateContext {
            location: camera.location(),
            graphics_flags: graph
                .zone(camera.location().path.zone)
                .unwrap()
                .graphics_flags,
            box_count: 0,
            checkpoint_id: -1,
            checkpoint_translation: [0; 3],
            first_spawn: false,
            active_neighbor_zones,
        });
        let mut host = NsfProgramHost::new(&nsd, &nsf, &nsf_bytes);
        runtime
            .create_retail_core_objects(camera.location().path.zone, &mut host)
            .unwrap();
        let attempts = runtime.spawn_current_zone_neighbors(&neighbors, &mut host);
        assert!(attempts.iter().any(|attempt| attempt.result.is_ok()));

        for _ in 0..12 {
            let step = camera.update(&graph, RetailCameraInput::default()).unwrap();
            runtime.set_level_state_context(RetailLevelStateContext {
                location: step.after,
                graphics_flags: graph.zone(step.after.path.zone).unwrap().graphics_flags,
                box_count: 0,
                checkpoint_id: -1,
                checkpoint_translation: [0; 3],
                first_spawn: false,
                active_neighbor_zones: neighbors
                    .iter()
                    .filter(|neighbor| neighbor.display_flags & 2 != 0)
                    .map(|neighbor| neighbor.eid)
                    .collect(),
            });
            runtime
                .run_frame(&mut host, RETAIL_INSTRUCTION_BUDGET)
                .unwrap();
        }
        let pause = runtime
            .update_retail_pause(true, camera.location().path.zone, &mut host)
            .unwrap();
        let RetailPauseUpdate::Paused { controller } = pause else {
            panic!("START did not create the authored pause controller: {pause:?}");
        };
        let controller_vm = runtime.machine().object(controller.vm()).unwrap();
        assert_eq!(controller_vm.state(), 6);
        assert_eq!(
            controller_vm
                .program_identity()
                .unwrap()
                .global_eid()
                .name()
                .as_deref(),
            Some("DispC")
        );

        let mut builder = RetailSceneBuilder::new();
        let frozen_draw_count = runtime.draw_count();
        for paused_frame in 0..=30_u32 {
            let frame = runtime
                .run_frame(&mut host, RETAIL_INSTRUCTION_BUDGET)
                .unwrap();
            let frame_stamp = u32::try_from(frame.frame_index).unwrap();
            assert!(
                frame
                    .executions
                    .iter()
                    .all(|execution| execution.result.is_ok())
            );
            assert_eq!(runtime.draw_count(), frozen_draw_count);
            let objects = runtime.render_objects().unwrap();
            let object = objects
                .iter()
                .find(|object| object.object == controller)
                .expect("pause controller remains live throughout the blink cycle");
            let expected_visible = paused_frame < 15 || paused_frame == 30;
            assert_eq!(object.display_eligible, expected_visible);
            assert_eq!(
                object.status_b,
                if expected_visible { 0x40200 } else { 0x40300 }
            );
            let reference = object
                .animation_reference
                .expect("pause controller selected its authored fragment animation");
            assert_eq!(reference.offset(), 136);

            if paused_frame == 0 {
                let program = object.program.unwrap();
                let global =
                    typed_entry(&nsf, &nsd, program.global_eid(), 11, "pause GOOL program")
                        .unwrap();
                let animations =
                    entry_item(global, &nsf_bytes, 5, "pause GOOL animations").unwrap();
                let descriptor = parse_gool_animation_descriptor(
                    animations,
                    usize::try_from(reference.offset()).unwrap(),
                )
                .unwrap();
                let GoolAnimationDescriptor::Fragment(fragment) = descriptor else {
                    panic!("pause panel must be a type-five fragment animation");
                };
                assert_eq!(fragment.texture_page.name().as_deref(), Some("WillT"));
                assert_eq!(fragment.header.length, 3);
                assert_eq!(fragment.fragments_per_frame, 5);
                assert_eq!(fragment.fragments.len(), 15);
            }

            if !matches!(paused_frame, 0 | 14 | 15 | 29 | 30) {
                continue;
            }
            let main_object = runtime
                .arena()
                .main_object()
                .and_then(|arena| runtime.object_for_arena(arena));
            let scene = builder
                .build_at_progress_with_objects(
                    &nsd,
                    &nsf,
                    &nsf_bytes,
                    RetailSceneProgressLocation {
                        zone: camera.location().path.zone,
                        path_index: camera.location().path.index,
                        path_progress: camera.location().progress.raw(),
                        frame_stamp,
                        draw_count: runtime.draw_count(),
                    },
                    &objects,
                    main_object,
                )
                .unwrap_or_else(|error| panic!("pause frame {paused_frame}: {error}"));
            assert_eq!(scene.stats.skipped_object_animations, 0);
            assert_eq!(scene.stats.skipped_object_textured_polygons, 0);
            let commands = scene
                .commands
                .iter()
                .filter(|command| {
                    matches!(
                        command.source,
                        CommandSource::Object { handle, .. }
                            if handle == u32::from(controller.vm().get())
                    ) && matches!(command.primitive, PrimitiveCommand::TexturedQuad(_))
                })
                .collect::<Vec<_>>();
            assert_eq!(commands.len(), usize::from(expected_visible) * 5);
            if !expected_visible {
                continue;
            }

            assert!(commands.iter().all(|command| command.depth == 0x07ff));
            let parts = commands
                .iter()
                .map(|command| match command.source {
                    CommandSource::Object { part, .. } => part,
                    _ => unreachable!(),
                })
                .collect::<BTreeSet<_>>();
            assert_eq!(parts, BTreeSet::from([0, 1, 2, 3, 4]));
            if paused_frame == 0 {
                let mut xs = Vec::with_capacity(20);
                let mut ys = Vec::with_capacity(20);
                for command in &commands {
                    let PrimitiveCommand::TexturedQuad(quad) = &command.primitive else {
                        unreachable!();
                    };
                    for vertex in &quad.vertices {
                        xs.push(vertex.position.x);
                        ys.push(vertex.position.y);
                    }
                }
                assert_eq!(xs.iter().copied().min(), Some(-75));
                assert_eq!(xs.iter().copied().max(), Some(84));
                assert_eq!(ys.iter().copied().min(), Some(43));
                assert_eq!(ys.iter().copied().max(), Some(93));

                let mut texture_handles = Vec::with_capacity(5);
                for command in &commands {
                    let PrimitiveCommand::TexturedQuad(quad) = &command.primitive else {
                        unreachable!();
                    };
                    if !texture_handles.contains(&quad.texture) {
                        texture_handles.push(quad.texture);
                    }
                }
                assert_eq!(texture_handles.len(), 5);
                for handle in texture_handles {
                    let texture = scene
                        .textures
                        .iter()
                        .find(|texture| texture.handle == handle)
                        .expect("each pause quad has a decoded scene texture");
                    assert!(
                        texture
                            .pixels
                            .rgba()
                            .chunks_exact(4)
                            .any(|pixel| pixel[3] != 0),
                        "each decoded pause fragment contains visible pixels"
                    );
                }
            }
        }
    }
}
