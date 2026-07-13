//! Safe, pointer-free construction of a retail world spawn snapshot.
//!
//! This module deliberately models the LDAT progress-zero scene. The retail
//! runtime runs entity spawning and `CamUpdate` before its first presentation,
//! so exact first-frame camera behavior remains simulation work rather than a
//! hidden approximation in the renderer.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use crust_formats::binary::Eid;
use crust_formats::stream::{
    Entry, Nsd, Nsf, PolygonId, SlstItem, WorldGeometry, ZoneHeader, ZonePath, ZoneRect,
    parse_world_geometry,
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
    pub handle: TextureHandle,
    pub pixels: DecodedTexture,
}

/// Read-only diagnostics for the current progress-zero world snapshot.
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
    let ldat = nsd
        .ldat()
        .ok_or_else(|| scene_error("index-only NSD has no LDAT scene"))?;
    let zone_entry = typed_entry(nsf, nsd, ldat.spawn_zone, ZDAT_ENTRY_TYPE, "spawn ZDAT")?;
    let zone_header = ZoneHeader::parse(entry_item(zone_entry, nsf_bytes, 0, "ZDAT header")?)
        .map_err(|error| scene_error(format!("spawn ZDAT header: {error}")))?;
    let zone_rect = ZoneRect::parse(entry_item(zone_entry, nsf_bytes, 1, "ZDAT rectangle")?)
        .map_err(|error| scene_error(format!("spawn ZDAT rectangle: {error}")))?;

    let path_index = u32::try_from(ldat.spawn_path_index)
        .map_err(|_| scene_error("LDAT spawn path index is negative"))?;
    let path_item_index = zone_header
        .path_item_index(path_index)
        .ok_or_else(|| scene_error("LDAT spawn path index is outside its ZDAT"))?;
    let path_item_index = usize::try_from(path_item_index)
        .map_err(|_| scene_error("ZDAT spawn path index does not fit the host"))?;
    let path = ZonePath::parse(entry_item(
        zone_entry,
        nsf_bytes,
        path_item_index,
        "ZDAT spawn path",
    )?)
    .map_err(|error| scene_error(format!("ZDAT spawn path: {error}")))?;

    // LevelUpdate deliberately does not open the SLST when the zone has no
    // worlds. Title, Hog Wild and Whole Hog use this as an external-transition
    // dummy start, and their placeholder SLST EID is absent from this stream.
    if zone_header.worlds.is_empty() {
        return Ok(RetailScene {
            commands: Vec::new(),
            textures: Vec::new(),
            stats: RetailSceneStats::default(),
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
    // LevelUpdate starts from the nearer raw endpoint. At progress zero this
    // is item zero except for one-point paths, where retail selects item one.
    let raw_item_index = usize::from(path.points.len() == 1);
    let raw_visibility = SlstItem::parse(entry_item(
        slst_entry,
        nsf_bytes,
        raw_item_index,
        "spawn SLST endpoint",
    )?)
    .map_err(|error| scene_error(format!("spawn SLST endpoint: {error}")))?;
    let SlstItem::Raw {
        polygons: visible_polygons,
        ..
    } = raw_visibility
    else {
        return Err(scene_error(
            "spawn SLST endpoint is a delta, not a raw list",
        ));
    };

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
    validate_visibility(&visible_polygons, &worlds)?;

    let first_point = path
        .points
        .first()
        .copied()
        .ok_or_else(|| scene_error("spawn path contains no camera point"))?;
    let camera_translation = Vec3i {
        x: zone_rect.origin[0].saturating_add(i32::from(first_point.x)),
        y: zone_rect.origin[1].saturating_add(i32::from(first_point.y)),
        z: zone_rect.origin[2].saturating_add(i32::from(first_point.z)),
    };
    let camera_matrix = world_camera_matrix(
        first_point.rotation_y,
        first_point.rotation_x,
        first_point.rotation_z,
    );
    let projection_distance = projection_distance(ldat.field_of_view)?;

    let mut page_ids = BTreeSet::new();
    for polygon_id in &visible_polygons {
        let geometry = &worlds[usize::from(polygon_id.world_index)];
        let polygon = geometry.polygons[usize::from(polygon_id.polygon_index)];
        if let Some(texture) = geometry
            .texture_for_polygon(polygon, 0)
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
            .texture_for_polygon(polygon, 0)
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
            zone: ldat.spawn_zone.raw(),
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

fn world_camera_matrix(rotation_y: i16, rotation_x: i16, rotation_z: i16) -> Matrix3 {
    let angle = |value: i16| Angle12::new(-i32::from(value));
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
                }
                Err(error) => panic!("{}: {error}", known.name),
            }
        }
        eprintln!("empty external-transition spawn snapshots: {empty:#?}");
        assert_eq!(built, 43);
        assert_eq!(empty, ["Hog Wild", "Title / Island Map", "Whole Hog"]);
    }
}
