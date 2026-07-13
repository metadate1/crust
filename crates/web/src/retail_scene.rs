//! Safe, pointer-free construction of a retail world path snapshot.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

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
    pub pixels: DecodedTexture,
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
    build_retail_scene_at_path_point(nsd, nsf, nsf_bytes, 0, 0)
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
    let ldat = nsd
        .ldat()
        .ok_or_else(|| scene_error("index-only NSD has no LDAT scene"))?;
    let path_index = u32::try_from(ldat.spawn_path_index)
        .map_err(|_| scene_error("LDAT spawn path index is negative"))?;
    build_retail_scene_at_location(
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
    let path_point_index = i32::try_from(location.path_point_index)
        .map_err(|_| scene_error("active path point index does not fit signed progress"))?;
    let path_progress = path_point_index
        .checked_mul(0x100)
        .ok_or_else(|| scene_error("active path point progress overflows signed 8.8 space"))?;
    build_retail_scene_at_progress(
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
    let ldat = nsd
        .ldat()
        .ok_or_else(|| scene_error("index-only NSD has no LDAT scene"))?;
    let zone_entry = typed_entry(nsf, nsd, location.zone, ZDAT_ENTRY_TYPE, "active ZDAT")?;
    let zone_header = ZoneHeader::parse(entry_item(zone_entry, nsf_bytes, 0, "ZDAT header")?)
        .map_err(|error| scene_error(format!("active ZDAT header: {error}")))?;
    let zone_rect = ZoneRect::parse(entry_item(zone_entry, nsf_bytes, 1, "ZDAT rectangle")?)
        .map_err(|error| scene_error(format!("active ZDAT rectangle: {error}")))?;

    let path_item_index = zone_header
        .path_item_index(location.path_index)
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
    let path_progress = location
        .path_progress
        .checked_abs()
        .ok_or_else(|| scene_error("signed path progress cannot be i32::MIN"))?;
    let path_point_index = usize::try_from(path_progress >> 8)
        .map_err(|_| scene_error("active path point index does not fit the host"))?;
    if path_point_index >= path.points.len() {
        return Err(scene_error("active path progress is outside its ZDAT path"));
    }
    let draw_count = location.draw_count;
    let path_point_count = u16::try_from(path.points.len())
        .map_err(|_| scene_error("spawn path point count does not fit u16"))?;
    let path_point_index_u16 = u16::try_from(path_point_index)
        .map_err(|_| scene_error("spawn path point index does not fit u16"))?;

    // LevelUpdate deliberately does not open the SLST when the zone has no
    // worlds. Title, Hog Wild and Whole Hog use this as an external-transition
    // dummy start, and their placeholder SLST EID is absent from this stream.
    if zone_header.worlds.is_empty() {
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
    let visible_polygons = visibility.visibility().to_vec();
    validate_visibility(&visible_polygons, &worlds)?;

    let camera = sample_camera(
        nsd,
        nsf,
        nsf_bytes,
        &zone_header,
        &zone_rect,
        &path,
        location.path_progress,
    )?;
    let camera_translation = camera.translation;
    let camera_matrix =
        world_camera_matrix(camera.rotation_y, camera.rotation_x, camera.rotation_z);
    let projection_distance = projection_distance(ldat.field_of_view)?;

    let mut page_ids = BTreeSet::new();
    for polygon_id in &visible_polygons {
        let geometry = &worlds[usize::from(polygon_id.world_index)];
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

    let mut cache = TextureCache::default();
    for (slot, raw_eid) in page_ids.iter().copied().enumerate() {
        let reference = TpagReference::new(Eid::from_raw(raw_eid));
        let page = resolve_texture_page(nsf, nsf_bytes, reference)
            .map_err(|error| scene_error(format!("spawn TPAG: {error}")))?;
        cache
            .install_page(slot, raw_eid, page.bytes().to_vec())
            .map_err(|error| scene_error(format!("install spawn TPAG: {error}")))?;
    }
    cache.begin_frame();

    let world_translations = worlds
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
    let mut prepared = vec![None; visible_polygons.len()];
    let mut minimum_depth = 0x07ff_i32;
    let mut saturated_vertices = 0_usize;
    let mut skipped_textured_polygons = 0_usize;

    // Retail transforms SLST entries backwards, applies a running minimum OT
    // depth, and head-inserts. We prepare backwards but later submit forwards
    // to compensate for the Rust ordering table's FIFO buckets.
    for (visible_index, polygon_id) in visible_polygons.iter().copied().enumerate().rev() {
        let world_index = usize::from(polygon_id.world_index);
        let geometry = &worlds[world_index];
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
            let Ok(cached) = cache.load(layout.request) else {
                skipped_textured_polygons = skipped_textured_polygons.saturating_add(1);
                continue;
            };
            textures
                .entry(cached.handle)
                .or_insert_with(|| (*cached.pixels).clone());
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
                texture: cached.handle,
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
    Ok(RetailScene {
        stats: RetailSceneStats {
            worlds: worlds.len(),
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
        KNOWN_LEVELS, LevelId, StreamKind, StreamName, parse_nsd, parse_nsf,
    };
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
