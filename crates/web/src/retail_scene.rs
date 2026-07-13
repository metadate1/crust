//! Safe, pointer-free construction of a retail world path snapshot.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use crust_formats::binary::Eid;
use crust_formats::stream::structs::ZonePathPoint;
use crust_formats::stream::{
    Entry, Nsd, Nsf, PolygonId, SlstCursor, SlstItem, WorldGeometry, ZoneHeader, ZonePath,
    ZoneRect, parse_world_geometry,
};
use crust_renderer::cache::{TextureCache, TextureHandle};
use crust_renderer::command::{
    BlendMode, ColoredTriangle, ColoredVertex, PrimitiveCommand, PrimitiveStyle, TexturedTriangle,
    TexturedVertex, Uv,
};
use crust_renderer::projection::{Matrix3, Vec3i, project, rotate};
use crust_renderer::retail_texture::{
    RetailTextureReference, TextureInfo2, TpagReference, resolve_texture_page,
};
use crust_renderer::texture::{DecodedTexture, Rgba8};
use crust_sim::Angle12;

const ZDAT_ENTRY_TYPE: u32 = 7;
const SLST_ENTRY_TYPE: u32 = 4;
const WGEO_ENTRY_TYPE: u32 = 3;
const RETAIL_TEXTURE_PAGE_SLOTS: usize = 8;

/// One world command with exact SLST provenance and ordering-table depth.
#[derive(Clone, Debug, PartialEq)]
pub struct RetailSceneCommand {
    pub depth: u16,
    pub zone: u32,
    pub polygon: u32,
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
    texture_cache: TextureCache,
    texture_pages: [Option<u32>; RETAIL_TEXTURE_PAGE_SLOTS],
    diagnostics: RetailSceneCacheDiagnostics,
}

impl Default for RetailSceneBuilder {
    fn default() -> Self {
        Self {
            active_graph: None,
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
        build_retail_scene_cached(self, nsd, nsf, nsf_bytes, location)
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

fn build_retail_scene_cached(
    builder: &mut RetailSceneBuilder,
    nsd: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    location: RetailSceneProgressLocation,
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
    if graph.zone_header.worlds.is_empty() {
        return Ok(RetailScene {
            commands: Vec::new(),
            textures: Vec::new(),
            stats: RetailSceneStats::default(),
            zone: location.zone,
            path_index: location.path_index,
            path_point_count,
            path_point_index: path_point_index_u16,
            draw_count,
        });
    }

    let visibility = graph
        .visibility
        .as_mut()
        .expect("a world-bearing graph always owns an SLST cursor");
    visibility
        .seek(path_point_index)
        .map_err(|error| scene_error(format!("spawn SLST state: {error}")))?;
    let visible_polygons = visibility.visibility().to_vec();
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
    let camera_matrix =
        world_camera_matrix(camera.rotation_y, camera.rotation_x, camera.rotation_z);
    let projection_distance = projection_distance(ldat.field_of_view)?;

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
            zone: location.zone.raw(),
            polygon: u32::from(polygon_id.raw()),
            primitive,
        });
    }

    let commands = prepared.into_iter().flatten().collect::<Vec<_>>();
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
            submitted_polygons: commands.len(),
            unique_textures: textures.len(),
            saturated_vertices,
            skipped_textured_polygons,
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
    rotation_y: i32,
    rotation_x: i32,
    rotation_z: i32,
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
    let translation = Vec3i {
        x: interpolate_coordinate(current_coordinates[0], next_coordinates[0], fraction)?,
        y: interpolate_coordinate(current_coordinates[1], next_coordinates[1], fraction)?,
        z: interpolate_coordinate(current_coordinates[2], next_coordinates[2], fraction)?,
    };
    let yaw_difference = i32::from(
        Angle12::new(i32::from(point.rotation_y))
            .difference_to(Angle12::new(i32::from(next.rotation_y))),
    );
    Ok(CameraSample {
        translation,
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

fn interpolate_coordinate(current: i32, next: i32, fraction: i32) -> Result<i32, RetailSceneError> {
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
    i32::try_from(fixed >> 8)
        .map_err(|_| scene_error("interpolated camera coordinate exceeds signed world space"))
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

fn world_camera_matrix(rotation_y: i32, rotation_x: i32, rotation_z: i32) -> Matrix3 {
    let angle = |value: i32| Angle12::new(-value);
    let z = angle(rotation_z);
    let y_stored = angle(rotation_y);
    let x_stored = angle(rotation_x);
    let mut matrix = Matrix3 {
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
    });
    for column in 0..3 {
        matrix.values[1][column] = wrapping_i16((-5 * i32::from(matrix.values[1][column])) >> 3);
        matrix.values[2][column] = wrapping_i16(-i32::from(matrix.values[2][column]));
    }
    matrix
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
        KNOWN_LEVELS, LevelId, RetailPathId, RetailZoneGraph, StreamKind, StreamName, parse_nsd,
        parse_nsf,
    };
    use crust_sim::camera::{RetailCameraInput, RetailCameraRuntime};
    use std::path::PathBuf;

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
    fn projection_distance_matches_every_retail_fov() {
        assert_eq!(projection_distance(30).unwrap(), 960);
        assert_eq!(projection_distance(37).unwrap(), 800);
        assert_eq!(projection_distance(55).unwrap(), 500);
        assert_eq!(projection_distance(60).unwrap(), 460);
        assert_eq!(projection_distance(90).unwrap(), 288);
        assert!(projection_distance(45).is_err());
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

        // The C implementation keeps 24.8 precision until the graphics-side
        // arithmetic shift, so a negative fraction rounds toward -infinity.
        assert_eq!(interpolate_coordinate(-1, 0, 1).unwrap(), -1);
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
}
