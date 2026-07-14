//! Safe, pointer-free construction of a retail world path snapshot.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;

use crust_formats::binary::Eid;
use crust_formats::stream::structs::ZonePathPoint;
use crust_formats::stream::{
    Entry, GoolAnimationDescriptor, GoolFontAnimation, GoolFragmentAnimation, GoolSpriteAnimation,
    GoolTextAnimation, GoolTextureInfo, GoolVertexAnimation, Nsd, Nsf, NsfPage, ObjectMaterial,
    ObjectModelFrame, ObjectVertexKind, PolygonId, SlstCursor, SlstItem, WorldGeometry, ZoneHeader,
    ZonePath, ZoneRect, load_object_model_frame, parse_gool_animation_descriptor,
    parse_object_frame, parse_world_geometry,
};
use crust_renderer::cache::{TextureCache, TextureHandle};
use crust_renderer::command::{
    BlendMode, ColoredTriangle, ColoredVertex, CommandSource, PrimitiveCommand, PrimitiveStyle,
    TexturedQuad, TexturedTriangle, TexturedVertex, Uv,
};
use crust_renderer::projection::{Matrix3, Vec3i, project, rotate};
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
use crust_renderer::{
    GoolObjectLighting, ObjectDarkShaderInput, ObjectProjectionParameters,
    ObjectProjectionTransform, ProjectedObjectPolygon, apply_object_zone_shader,
    project_object_model,
};
use crust_sim::Angle12;
use crust_sim::retail_runtime::{RetailRenderObject, RuntimeObjectHandle};

const ZDAT_ENTRY_TYPE: u32 = 7;
const SLST_ENTRY_TYPE: u32 = 4;
const WGEO_ENTRY_TYPE: u32 = 3;
const RETAIL_TEXTURE_PAGE_SLOTS: usize = 8;
const RETAIL_OBJECT_MODEL_CACHE_FRAMES: usize = 256;
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
    pub draw_count: u32,
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
    worlds: Vec<WorldGeometry>,
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

/// Pair-scoped owner of parsed ZDAT/SLST/WGEO data and decoded textures.
///
/// The active graph is keyed by the exact zone/path pair. Moving to another
/// zone or path replaces the graph and texture-page state. Constructing a new
/// builder at stream-pair mount provides the stronger pair boundary, so no
/// handles or decoded pixels can survive a level transition accidentally.
#[derive(Debug)]
pub struct RetailSceneBuilder {
    active_graph: Option<CachedSceneGraph>,
    object_models: HashMap<(Eid, u16), Arc<ObjectModelFrame>>,
    object_model_lru: VecDeque<(Eid, u16)>,
    texture_cache: TextureCache,
    texture_pages: [Option<u32>; RETAIL_TEXTURE_PAGE_SLOTS],
    diagnostics: RetailSceneCacheDiagnostics,
}

impl Default for RetailSceneBuilder {
    fn default() -> Self {
        Self {
            active_graph: None,
            object_models: HashMap::new(),
            object_model_lru: VecDeque::new(),
            texture_cache: TextureCache::default(),
            texture_pages: [None; RETAIL_TEXTURE_PAGE_SLOTS],
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
            &[],
            None,
            RETAIL_INITIAL_DISPLAY_FLAGS,
            None,
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
        self.build_at_progress_with_objects_and_display_mask(
            nsd,
            nsf,
            nsf_bytes,
            location,
            objects,
            main_object,
            RETAIL_INITIAL_DISPLAY_FLAGS,
        )
    }

    /// Builds the post-GOOL scene using the exact current-frame display mask.
    ///
    /// The caller must pass current global nine after GOOL, never the
    /// script-owned next global four. This also preserves the source's rare
    /// same-frame behavior if an opcode writes current directly.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed scene, animation, object, paging, or
    /// texture data referenced by the mounted pair.
    #[allow(clippy::too_many_arguments)]
    pub fn build_at_progress_with_objects_and_display_mask(
        &mut self,
        nsd: &Nsd,
        nsf: &Nsf,
        nsf_bytes: &[u8],
        location: RetailSceneProgressLocation,
        objects: &[RetailRenderObject],
        main_object: Option<RuntimeObjectHandle>,
        display_mask: u32,
    ) -> Result<RetailScene, RetailSceneError> {
        build_retail_scene_cached(
            self,
            nsd,
            nsf,
            nsf_bytes,
            location,
            objects,
            main_object,
            display_mask,
            None,
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
    /// [`Self::build_at_progress_with_objects_and_display_mask`], including an
    /// unsupported field of view.
    #[allow(clippy::too_many_arguments)]
    pub fn build_at_progress_with_objects_display_mask_and_fov(
        &mut self,
        nsd: &Nsd,
        nsf: &Nsf,
        nsf_bytes: &[u8],
        location: RetailSceneProgressLocation,
        objects: &[RetailRenderObject],
        main_object: Option<RuntimeObjectHandle>,
        display_mask: u32,
        field_of_view: u32,
    ) -> Result<RetailScene, RetailSceneError> {
        build_retail_scene_cached(
            self,
            nsd,
            nsf,
            nsf_bytes,
            location,
            objects,
            main_object,
            display_mask,
            Some(field_of_view),
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

#[allow(clippy::too_many_arguments)]
fn build_retail_scene_cached(
    builder: &mut RetailSceneBuilder,
    nsd: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    location: RetailSceneProgressLocation,
    render_objects: &[RetailRenderObject],
    main_object: Option<RuntimeObjectHandle>,
    display_mask: u32,
    field_of_view_override: Option<u32>,
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
    let key = RetailSceneCacheKey {
        zone: location.zone,
        path_index: location.path_index,
    };
    if builder.active_graph.as_ref().map(|graph| graph.key) == Some(key) {
        builder.diagnostics.graph_reuses = builder.diagnostics.graph_reuses.saturating_add(1);
    } else {
        let graph = parse_scene_graph(nsd, nsf, nsf_bytes, key, path_point_index)?;
        builder.active_graph = Some(graph);
        builder.texture_cache = TextureCache::default();
        builder.texture_pages = [None; RETAIL_TEXTURE_PAGE_SLOTS];
        builder.diagnostics.graph_builds = builder.diagnostics.graph_builds.saturating_add(1);
    }

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
    let visible_polygons = if graph.zone_header.worlds.is_empty() {
        Vec::new()
    } else {
        let visibility = graph
            .visibility
            .as_mut()
            .expect("a world-bearing graph always owns an SLST cursor");
        visibility
            .seek(path_point_index)
            .map_err(|error| scene_error(format!("spawn SLST state: {error}")))?;
        if display_mask & 1 == 0 {
            Vec::new()
        } else {
            visibility.visibility().to_vec()
        }
    };
    validate_visibility(&visible_polygons, &graph.worlds)?;

    let camera = sample_camera(
        nsd,
        nsf,
        nsf_bytes,
        &graph.zone_header,
        &graph.zone_rect,
        &graph.path,
        location.path_progress,
    )?;
    let camera_translation = camera.translation;
    let raw_world_camera_matrix =
        raw_camera_matrix(camera.rotation_y, camera.rotation_x, camera.rotation_z);
    let camera_matrix = adjusted_camera_matrix(raw_world_camera_matrix);
    let object_camera = object_camera_sample(camera, graph.zone_header.graphics.flags, draw_count);
    let raw_object_camera_matrix = raw_camera_matrix(
        object_camera.rotation_y,
        object_camera.rotation_x,
        object_camera.rotation_z,
    );
    let object_camera_matrix = adjusted_camera_matrix(raw_object_camera_matrix);
    let projection_distance =
        projection_distance(field_of_view_override.unwrap_or(ldat.field_of_view))?;
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
        display_mask,
    )?;

    let mut page_ids = BTreeSet::new();
    for polygon_id in &visible_polygons {
        let geometry = &graph.worlds[usize::from(polygon_id.world_index)];
        let polygon = geometry.polygons[usize::from(polygon_id.polygon_index)];
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
    let resident_texture_pages = resident_texture_pages(nsd, nsf, &graph.zone_header)?;
    page_ids.retain(|page| resident_texture_pages.contains(page));
    if page_ids.len() > RETAIL_TEXTURE_PAGE_SLOTS {
        return Err(scene_error(format!(
            "spawn scene needs {} simultaneous TPAGs; retail has eight slots",
            page_ids.len()
        )));
    }

    install_missing_texture_pages(
        &mut builder.texture_cache,
        &mut builder.texture_pages,
        &mut builder.diagnostics,
        nsf,
        nsf_bytes,
        &page_ids,
    )?;
    builder.texture_cache.begin_frame();

    let world_translations = graph
        .worlds
        .iter()
        .map(|world| {
            if world.header.is_backdrop {
                Vec3i::default()
            } else {
                rotate(
                    Vec3i {
                        x: world.header.translation[0].saturating_sub(camera_translation.x),
                        y: world.header.translation[1].saturating_sub(camera_translation.y),
                        z: world.header.translation[2].saturating_sub(camera_translation.z),
                    },
                    camera_matrix,
                )
                .point
            }
        })
        .collect::<Vec<_>>();

    let mut textures = BTreeMap::new();
    let mut texture_handles = HashMap::new();
    let mut prepared = vec![None; visible_polygons.len()];
    let mut minimum_depth = 0x07ff_i32;
    let mut saturated_vertices = 0_usize;
    let mut skipped_textured_polygons = 0_usize;

    // Retail transforms SLST entries backwards, applies a running minimum OT
    // depth, and head-inserts. We prepare backwards but later submit forwards
    // to compensate for the Rust ordering table's FIFO buckets.
    for (visible_index, polygon_id) in visible_polygons.iter().copied().enumerate().rev() {
        let world_index = usize::from(polygon_id.world_index);
        let geometry = &graph.worlds[world_index];
        let polygon_index = usize::from(polygon_id.polygon_index);
        let polygon = geometry.polygons[polygon_index];
        let mut screens = [crust_renderer::command::ScreenPoint::default(); 3];
        let mut colors = [Rgba8::default(); 3];
        for vertex_index in 0..3 {
            let vertex = geometry.vertices[usize::from(polygon.vertex_indices[vertex_index])];
            let [x, y, z] = vertex.expanded_position();
            let projected = project(
                Vec3i { x, y, z },
                world_translations[world_index],
                camera_matrix,
                [0, 0],
                projection_distance,
            );
            if !projected.valid {
                saturated_vertices = saturated_vertices.saturating_add(1);
            }
            screens[vertex_index] = projected.screen;
            colors[vertex_index] = Rgba8 {
                r: vertex.color[0],
                g: vertex.color[1],
                b: vertex.color[2],
                a: u8::MAX,
            };
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
            let Ok(cached) = builder.texture_cache.load(layout.request) else {
                skipped_textured_polygons = skipped_textured_polygons.saturating_add(1);
                continue;
            };
            // TextureCache handles intentionally live for the whole pair, but
            // RetailScene handles remain deterministic build-local IDs. This
            // preserves byte-for-byte scene equality with the static builder
            // while still reusing the expensive decoded pixels underneath.
            let output_handle = if let Some(handle) = texture_handles.get(&layout.request) {
                *handle
            } else {
                let next = u64::try_from(texture_handles.len())
                    .ok()
                    .and_then(|value| value.checked_add(1))
                    .ok_or_else(|| scene_error("retail texture handle count overflows"))?;
                let handle = TextureHandle::new(next);
                texture_handles.insert(layout.request, handle);
                handle
            };
            textures
                .entry(output_handle)
                .or_insert_with(|| Arc::clone(&cached.pixels));
            let uvs = layout.coordinates.cache_uvs(cached.content_uv);
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
        let raw_depth = (0x0800_i32 - i32::try_from(projection_distance / 2).unwrap_or(i32::MAX))
            .saturating_sub(z_sum / 32)
            .clamp(0, 0x07ff);
        let depth = raw_depth.min(minimum_depth);
        minimum_depth = depth;
        let depth = u16::try_from(depth)
            .map_err(|_| scene_error("clamped ordering depth does not fit u16"))?;
        prepared[visible_index] = Some(RetailSceneCommand {
            depth,
            source: CommandSource::World {
                zone: location.zone.raw(),
                polygon: u32::from(polygon_id.raw()),
            },
            primitive,
        });
    }

    let world_commands = prepared.into_iter().flatten().collect::<Vec<_>>();
    let submitted_polygons = world_commands.len();
    let mut object_commands = Vec::new();
    let mut skipped_object_textured_polygons = 0_usize;
    for object in &prepared_objects.objects {
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
                    if !resident_texture_pages.contains(&texture_page.raw()) {
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
                    let output_handle = if let Some(handle) = texture_handles.get(&layout.request) {
                        *handle
                    } else {
                        let next = u64::try_from(texture_handles.len())
                            .ok()
                            .and_then(|value| value.checked_add(1))
                            .ok_or_else(|| scene_error("retail texture handle count overflows"))?;
                        let handle = TextureHandle::new(next);
                        texture_handles.insert(layout.request, handle);
                        handle
                    };
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
        }
    }
    let submitted_object_polygons = object_commands.len();
    let mut submitted_object_quads = 0_usize;
    for quad in &prepared_objects.quads {
        if !resident_texture_pages.contains(&quad.texture_page.raw()) {
            skipped_object_textured_polygons = skipped_object_textured_polygons.saturating_add(1);
            continue;
        }
        let reference =
            RetailTextureReference::new(TpagReference::new(quad.texture_page), quad.texture);
        let Ok(layout) = reference.layout() else {
            skipped_object_textured_polygons = skipped_object_textured_polygons.saturating_add(1);
            continue;
        };
        let Ok(cached) = builder.texture_cache.load(layout.request) else {
            skipped_object_textured_polygons = skipped_object_textured_polygons.saturating_add(1);
            continue;
        };
        let output_handle = if let Some(handle) = texture_handles.get(&layout.request) {
            *handle
        } else {
            let next = u64::try_from(texture_handles.len())
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| scene_error("retail texture handle count overflows"))?;
            let handle = TextureHandle::new(next);
            texture_handles.insert(layout.request, handle);
            handle
        };
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
            worlds: graph.worlds.len(),
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
    display_flags: u32,
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
        let Some(animation_reference) = object.animation_reference else {
            prepared.skipped_animations = prepared.skipped_animations.saturating_add(1);
            continue;
        };
        let global = typed_entry(nsf, nsd, program.global_eid(), 11, "GOOL object program")?;
        let animations = entry_item(global, nsf_bytes, 5, "GOOL object animations")?;
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
                display_flags,
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
    display_flags: u32,
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
        if display_flags & 0x1_0000 == 0
            && object.status_b & 0x4_0000 == 0
            && i32::try_from(projection_distance).unwrap_or(i32::MAX) >= camera_translation.z
        {
            return Ok(());
        }
        let is_main = main_object == Some(object.object);
        let mut effective_colors = object.colors;
        let mut colored_shift = 0;
        if display_flags & 0x1_0000 == 0
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
    let font = resolve_text_font(
        animations,
        animation.font_word_offset,
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
            worlds: Vec::new(),
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
    for (world_index, world) in zone_header.worlds.iter().enumerate() {
        let entry = typed_entry(nsf, nsd, world.geometry, WGEO_ENTRY_TYPE, "spawn WGEO")?;
        let geometry = parse_world_geometry(
            entry_item(entry, nsf_bytes, 0, "WGEO header")?,
            entry_item(entry, nsf_bytes, 1, "WGEO polygons")?,
            entry_item(entry, nsf_bytes, 2, "WGEO vertices")?,
        )
        .map_err(|error| scene_error(format!("spawn WGEO {world_index}: {error}")))?;
        worlds.push(geometry);
    }
    let world_polygon_counts = worlds
        .iter()
        .map(|world| world.polygons.len())
        .collect::<Vec<_>>();
    let visibility = SlstCursor::new(&slst_items, &world_polygon_counts, path_point_index)
        .map_err(|error| scene_error(format!("spawn SLST state: {error}")))?;

    Ok(CachedSceneGraph {
        key,
        zone_header,
        zone_rect,
        path,
        visibility: Some(visibility),
        worlds,
    })
}

fn install_missing_texture_pages(
    texture_cache: &mut TextureCache,
    texture_pages: &mut [Option<u32>; RETAIL_TEXTURE_PAGE_SLOTS],
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
    worlds: &[WorldGeometry],
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
    // 128-frame triangular path, and substitutes a fixed 125-angle pitch for
    // GOOL objects only. World geometry continues to use `camera` above.
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
        rotation_y: 125,
        rotation_x: 0,
        rotation_z: 0,
    }
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

fn blend_mode(raw: u8) -> BlendMode {
    match raw & 3 {
        0 => BlendMode::Average,
        1 => BlendMode::Additive,
        2 => BlendMode::Subtractive,
        _ => BlendMode::Opaque,
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
    use crust_formats::disc::DiscImage;
    use crust_formats::stream::{
        KNOWN_LEVELS, LevelId, RetailPathId, RetailZoneGraph, StreamKind, StreamName, ZoneEntity,
        parse_nsd, parse_nsf,
    };
    use crust_sim::camera::{
        RetailCameraEffect, RetailCameraFollowInput, RetailCameraInput, RetailCameraLocation,
        RetailCameraRuntime, RetailCameraStep,
    };
    use crust_sim::gool::{
        CollisionObjectReference, RetailPadSnapshot, RetailTransformVectorsCamera, process_register,
    };
    use crust_sim::math::Vec3;
    use crust_sim::object_arena::NeighborZone;
    use crust_sim::retail_runtime::{
        NsfProgramHost, RetailLevelStateContext, RetailPauseUpdate, RetailRestartOutcome,
        RetailRuntime, ZoneTerminationMode,
    };
    use crust_sim::zone_lifecycle::{
        OrderedZoneLoadList, ZoneLifecycle, ZoneLifecycleZone, ZoneTransitionAction,
    };
    use std::path::PathBuf;

    use crate::pbak_runtime::{RetailPbakPlayback, pbak_event_pad_snapshot, prepare_pair_pbak};

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
            [125, 0, 0]
        );

        let trough = object_camera_sample(world, 0x1000, 64);
        assert_eq!(trough.translation_fixed, [0, 901_600, 6_144_000]);
        assert_eq!(
            raw_camera_matrix(trough.rotation_y, trough.rotation_x, trough.rotation_z),
            raw_camera_matrix(125, 0, 0)
        );
        assert_eq!(object_camera_sample(world, 0x1000, 128), start);
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

        let first_presented =
            build_retail_scene_at_path_point(&nsd, &nsf, &nsf_bytes, 2, 1).unwrap();
        assert_eq!(first_presented.stats.worlds, 4);
        assert_eq!(first_presented.stats.visible_polygons, 679);
        assert_eq!(
            first_presented.stats.submitted_polygons,
            first_presented.commands.len()
        );
        let ldat = nsd.ldat().unwrap();
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
    fn local_pbak_restored_scene_is_renderable() {
        const RETAIL_GLOBAL_WORDS: usize = 256;
        const PBAK_STATE_GLOBAL: usize = 105;

        let root = PathBuf::from(
            std::env::var_os("C1_STREAM_DIR")
                .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
        );
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
        let nsd_path = root.join(StreamName::new(level, StreamKind::Nsd).filename());
        let nsf_path = root.join(StreamName::new(level, StreamKind::Nsf).filename());
        let nsd_bytes = std::fs::read(&nsd_path)
            .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
        let nsf_bytes = std::fs::read(&nsf_path)
            .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));
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
        let publish_camera = |runtime: &mut RetailRuntime,
                              camera: &RetailCameraRuntime,
                              game_state: i32| {
            let pose = camera.pose(&graph).unwrap();
            runtime.set_frame_context(game_state, camera.rotation_xz(&graph).unwrap());
            runtime.set_transform_vectors_camera(RetailTransformVectorsCamera::from_retail_pose(
                pose.translation,
                pose.rotation_yxz,
                projection_distance(nsd.ldat().unwrap().field_of_view).unwrap(),
            ));
        };
        let update_camera = |camera: &mut RetailCameraRuntime,
                             runtime: &RetailRuntime,
                             held_buttons: u32| {
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
        publish_camera(&mut runtime, &camera, initial_camera_step.game_state);
        runtime.set_frame_timing(34, 34);
        runtime
            .set_pad_snapshot(0, RetailPadSnapshot::default())
            .unwrap();
        runtime.run_frame(&mut host, 67).unwrap();

        let prepared = prepare_pair_pbak(&nsd, &nsf, &nsf_bytes, &graph)
            .unwrap()
            .expect("selected level has one PBAK recording");
        assert_eq!(prepared.snapshot.level, level);
        let trace_frames = std::env::var("C1_PBAK_FRAMES").ok().map_or(232, |value| {
            value
                .parse::<usize>()
                .unwrap_or_else(|error| panic!("C1_PBAK_FRAMES {value:?}: {error}"))
        });
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
        let mut observed_offender = None;
        let mut finish_outcome = None;
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
            publish_camera(&mut runtime, &camera, camera_step.game_state);
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
                draw_count,
            };
            builder
                .build_at_progress_with_objects_and_display_mask(
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

            let offender = (level == LevelId::new_const(0x0c))
                .then(|| {
                    objects.iter().find(|object| {
                        object.executable == 3
                            && object.subtype == 13
                            && object.program.is_some_and(|program| {
                                program.global_eid().name().as_deref() == Some("FruiC")
                            })
                            && object.transform.scale == [-110_121, 279_039, 936]
                    })
                })
                .flatten();
            // The synchronous 0x83/0x84 local-bound refresh lets successive
            // authored burst children reach this same scale. Pin the first
            // matching incarnation so the later checks follow one lifecycle.
            if observed_offender.is_none()
                && let Some(offender) = offender
            {
                observed_offender = Some(offender.object);
                assert_eq!(pbak_frame, 190);
                assert_eq!(pad_boundaries, 191);
                assert_eq!(camera_step.after.progress.raw(), 0x600);
                assert_eq!(
                    runtime.object_for_arena(offender.object.arena()),
                    Some(offender.object)
                );
                assert_eq!(
                    runtime.object_for_vm(offender.object.vm()),
                    Some(offender.object)
                );
                assert_eq!(offender.animation_reference.unwrap().offset(), 0);
                assert_eq!(retail_sprite_shrink(offender.transform.scale[0]), Ok(4));
                let vm = runtime.machine().object(offender.object.vm()).unwrap();
                assert_eq!(vm.state(), 12);
                assert_eq!(vm.register(process_register::ANIMATION_STAMP).unwrap(), 191);
            }
            if let Some((raw_shrink, effective_shrink)) = (level == LevelId::new_const(0x0c))
                .then_some(match pbak_frame {
                    191 | 192 => Some((4_u32, 4_u8)),
                    193 | 194 => Some((5, 5)),
                    215 => Some((41, 9)),
                    216 => Some((45, 13)),
                    217 => Some((49, 17)),
                    _ => None,
                })
                .flatten()
            {
                let transient = objects
                    .iter()
                    .find(|object| Some(object.object) == observed_offender)
                    .expect("the authored FruiC transient remains live");
                assert_eq!(
                    transient.transform.scale[0].unsigned_abs() / 27_279,
                    raw_shrink,
                    "{} frame {pbak_frame} raw scale quotient",
                    prepared.eid,
                );
                assert_eq!(
                    retail_sprite_shrink(transient.transform.scale[0]),
                    Ok(effective_shrink),
                    "{} frame {pbak_frame} effective five-bit shift",
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
        if level == LevelId::new_const(0x0c) && trace_frames >= 207 {
            assert!(
                observed_offender.is_some(),
                "pb0cB did not reach its authored transient shrink-4 sprite"
            );
        }
        if trace_frames == prepared.frame_count() {
            assert!(
                playback.is_returning(),
                "{pad_boundaries} Crash pad boundaries ran across {trace_frames} wall frames"
            );
            assert!(
                finish_outcome.is_some(),
                "the final recorded pad boundary must complete the retail demo handshake"
            );
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
                            if object.status_b & 0x200 != 0
                                && nsf
                                    .resolve_entry(&nsd, vertex.model_eid)
                                    .is_ok_and(|entry| entry.entry_type == 20)
                            {
                                live_2d_cvtx += 1;
                            }
                            if current_header.graphics.unknown_a == 4
                                && current_header.display_flags & 0x1_0000 == 0
                                && Some(object.object) != main_object
                                && object.status_b & 0x400 == 0
                            {
                                live_mode_four_vertices += 1;
                                has_live_non_vertex = true;
                                mode_four_handles.insert(u32::from(object.object.vm().get()));
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
            "live non-vertex boot frames: sprites={live_sprites}, fragments={live_fragments}, texts={live_texts} ({live_dynamic_fonts} dynamic-font overrides), 2D CVTX={live_2d_cvtx}, mode-4 vertices={live_mode_four_vertices}, sprite quads={emitted_sprite_quads}, fragment quads={emitted_fragment_quads}, text quads={emitted_text_quads}, mode-4 primitives={emitted_mode_four_primitives}, levels={observed_levels:?}"
        );
        assert!(live_sprites > 0);
        assert!(live_fragments > 0);
        assert!(emitted_sprite_quads > 0);
        assert!(emitted_fragment_quads > 0);
        assert!(live_texts > 0);
        assert!(emitted_text_quads > 0);
        assert!(live_mode_four_vertices > 0);
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
