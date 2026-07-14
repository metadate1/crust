//! Opt-in, read-only characterization of retail scene-format data.

use std::path::{Path, PathBuf};

use crust_formats::disc::{DiscImage, DiscStreamSet};
use crust_formats::stream::{
    KNOWN_LEVELS, LevelId, PolygonId, SlstDirection, SlstItem, StreamKind, StreamName,
    WorldGeometry, WorldMapPathList, WorldMapPathRecord, ZoneHeader, ZonePath, ZoneRect, parse_nsd,
    parse_nsf, parse_world_geometry,
};

const WGEO_ENTRY_TYPE: u32 = 3;
const SLST_ENTRY_TYPE: u32 = 4;
const ZDAT_ENTRY_TYPE: u32 = 7;

fn read_stream(
    root: Option<&Path>,
    disc: Option<(&DiscImage<'_>, &DiscStreamSet)>,
    name: StreamName,
) -> Vec<u8> {
    if let Some(root) = root {
        let path = root.join(name.filename());
        return std::fs::read(&path)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()));
    }
    let (image, streams) = disc.expect("local retail source was not initialized");
    let stream = streams
        .get(name)
        .unwrap_or_else(|| panic!("disc is missing {name}"));
    image
        .read_stream(stream)
        .unwrap_or_else(|error| panic!("could not read {name} from disc: {error}"))
}

fn mix_fingerprint(fingerprint: &mut u64, value: u32) {
    for byte in value.to_le_bytes() {
        *fingerprint ^= u64::from(byte);
        *fingerprint = fingerprint.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

fn validate_polygon_references(
    level_name: &str,
    zone_eid: impl std::fmt::Display,
    path_index: u32,
    point_index: usize,
    polygons: &[PolygonId],
    geometries: &[WorldGeometry],
) {
    for polygon in polygons {
        let world = geometries
            .get(usize::from(polygon.world_index))
            .unwrap_or_else(|| {
                panic!(
                    "{level_name} ZDAT {zone_eid} path {path_index} point {point_index} \
                     references inactive world {}",
                    polygon.world_index
                )
            });
        assert!(
            usize::from(polygon.polygon_index) < world.polygons.len(),
            "{level_name} ZDAT {zone_eid} path {path_index} point {point_index} \
             references WGEO {} polygon {}, but it has {} polygons",
            polygon.world_index,
            polygon.polygon_index,
            world.polygons.len()
        );
    }
}

/// Characterizes the four authored title-map ZDATs without copying any
/// proprietary bytes into the repository. This also proves that item three's
/// unusual `len + type-as-record-zero` layout consumes the legal 0x19 corpus.
#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn title_island_map_wgeo_groups_match_the_safe_item_three_contract() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let nsd_bytes = read_stream(
        Some(&root),
        None,
        StreamName::new(LevelId::TITLE, StreamKind::Nsd),
    );
    let nsf_bytes = read_stream(
        Some(&root),
        None,
        StreamName::new(LevelId::TITLE, StreamKind::Nsf),
    );
    let nsd = parse_nsd(&nsd_bytes, LevelId::TITLE).expect("invalid title NSD");
    let nsf = parse_nsf(&nsf_bytes, &nsd).expect("invalid title NSF");

    let mut item_count = 0_usize;
    let mut group_records = 0_usize;
    let mut polygon_records = 0_usize;
    let mut fingerprint = 0xcbf2_9ce4_8422_2325_u64;
    for zone_name in ["1a_pZ", "1e_pZ", "2b_pZ", "3a_pZ"] {
        let zone_eid = crust_formats::binary::Eid::from_name(zone_name).unwrap();
        let zone_entry = nsf.resolve_entry(&nsd, zone_eid).unwrap();
        assert_eq!(zone_entry.entry_type, ZDAT_ENTRY_TYPE);
        let zone = ZoneHeader::parse(zone_entry.item(0).unwrap().bytes(&nsf_bytes).unwrap())
            .unwrap_or_else(|error| panic!("title map ZDAT {zone_name}: {error}"));
        let mut active_group = 0_u16;
        for (world_index, world) in zone.worlds.iter().enumerate() {
            let wgeo = nsf.resolve_entry(&nsd, world.geometry).unwrap();
            assert_eq!(wgeo.entry_type, WGEO_ENTRY_TYPE);
            if wgeo.items.len() < 4 {
                continue;
            }
            let geometry = parse_world_geometry(
                wgeo.item(0).unwrap().bytes(&nsf_bytes).unwrap(),
                wgeo.item(1).unwrap().bytes(&nsf_bytes).unwrap(),
                wgeo.item(2).unwrap().bytes(&nsf_bytes).unwrap(),
            )
            .unwrap_or_else(|error| panic!("title map WGEO {}: {error}", wgeo.eid));
            let paths = WorldMapPathList::parse(wgeo.item(3).unwrap().bytes(&nsf_bytes).unwrap())
                .unwrap_or_else(|error| panic!("title map WGEO {} item three: {error}", wgeo.eid));
            let overrides = paths
                .mask_overrides(
                    geometry.polygons.len(),
                    &mut active_group,
                    u32::MAX,
                    u32::MAX,
                )
                .unwrap_or_else(|error| panic!("title map WGEO {} groups: {error}", wgeo.eid));
            assert!(overrides.iter().all(|entry| entry.animation_mask == 7));
            item_count += 1;
            mix_fingerprint(
                &mut fingerprint,
                u32::try_from(world_index).expect("retail world index fits u32"),
            );
            for record in paths.records() {
                let (tag, index) = match *record {
                    WorldMapPathRecord::Group(index) => {
                        group_records += 1;
                        (1_u32, index)
                    }
                    WorldMapPathRecord::Polygon(index) => {
                        polygon_records += 1;
                        (0_u32, index)
                    }
                };
                mix_fingerprint(&mut fingerprint, tag);
                mix_fingerprint(&mut fingerprint, u32::from(index));
            }
        }
    }

    eprintln!(
        "title map item-three golden: {item_count} items, {group_records} group records, \
         {polygon_records} polygon records, fingerprint {fingerprint:016x}"
    );
    assert_eq!(item_count, 4);
    assert_eq!(group_records, 42);
    assert_eq!(polygon_records, 368);
    assert_eq!(fingerprint, 0x1c1c_2ddf_b2c7_c7ab);
}

/// Parses every retail ZDAT, SLST and WGEO entry from all 44 pairs without
/// extracting or writing any user-supplied game data.
#[test]
#[ignore = "set C1_STREAM_DIR or C1_DISC_IMAGE to legally local NTSC-U data"]
fn parses_all_retail_scene_entries_and_spawn_zones() {
    let stream_root = std::env::var_os("C1_STREAM_DIR").map(PathBuf::from);
    let disc_bytes = if stream_root.is_none() {
        let path = PathBuf::from(
            std::env::var_os("C1_DISC_IMAGE")
                .expect("set C1_STREAM_DIR or C1_DISC_IMAGE to legally local NTSC-U data"),
        );
        Some(
            std::fs::read(&path)
                .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display())),
        )
    } else {
        None
    };
    let disc_image = disc_bytes.as_deref().map(|bytes| {
        DiscImage::open(bytes).unwrap_or_else(|error| panic!("could not open local disc: {error}"))
    });
    let disc_streams = disc_image.as_ref().map(|image| {
        image
            .discover_streams()
            .unwrap_or_else(|error| panic!("could not discover local disc streams: {error}"))
    });
    if let Some(streams) = &disc_streams {
        streams
            .validate_complete_retail()
            .unwrap_or_else(|error| panic!("local disc stream set is incomplete: {error}"));
    }
    let disc = disc_image.as_ref().zip(disc_streams.as_ref());

    let mut zdat_entries = 0_usize;
    let mut zdat_paths = 0_usize;
    let mut resolved_path_slsts = 0_usize;
    let mut external_path_slsts = 0_usize;
    let mut wgeo_entries = 0_usize;
    let mut slst_entries = 0_usize;
    let mut slst_items = 0_usize;
    let mut spawn_zones = 0_usize;
    let mut spawn_fingerprint = 0xcbf2_9ce4_8422_2325_u64;
    let mut decoded_slst_paths = 0_usize;
    let mut decoded_slst_states = 0_usize;
    let mut decoded_slst_transitions = 0_usize;
    let mut slst_inverse_round_trips = 0_usize;
    let mut validated_polygon_references = 0_usize;
    let mut slst_fingerprint = 0xcbf2_9ce4_8422_2325_u64;

    for level in KNOWN_LEVELS {
        let nsd_bytes = read_stream(
            stream_root.as_deref(),
            disc,
            StreamName::new(level.id, StreamKind::Nsd),
        );
        let nsf_bytes = read_stream(
            stream_root.as_deref(),
            disc,
            StreamName::new(level.id, StreamKind::Nsf),
        );
        let nsd = parse_nsd(&nsd_bytes, level.id)
            .unwrap_or_else(|error| panic!("{} NSD: {error}", level.name));
        let nsf = parse_nsf(&nsf_bytes, &nsd)
            .unwrap_or_else(|error| panic!("{} NSF: {error}", level.name));

        for entry in nsf.entries() {
            match entry.entry_type {
                WGEO_ENTRY_TYPE => {
                    let header = entry
                        .item(0)
                        .unwrap_or_else(|| panic!("WGEO {} has no item zero", entry.eid))
                        .bytes(&nsf_bytes)
                        .unwrap();
                    let polygons = entry
                        .item(1)
                        .unwrap_or_else(|| panic!("WGEO {} has no polygon item", entry.eid))
                        .bytes(&nsf_bytes)
                        .unwrap();
                    let vertices = entry
                        .item(2)
                        .unwrap_or_else(|| panic!("WGEO {} has no vertex item", entry.eid))
                        .bytes(&nsf_bytes)
                        .unwrap();
                    parse_world_geometry(header, polygons, vertices).unwrap_or_else(|error| {
                        panic!("{} WGEO {}: {error}", level.name, entry.eid)
                    });
                    wgeo_entries += 1;
                }
                SLST_ENTRY_TYPE => {
                    for item in &entry.items {
                        let bytes = item.bytes(&nsf_bytes).unwrap();
                        SlstItem::parse(bytes).unwrap_or_else(|error| {
                            panic!(
                                "{} SLST {} item {}: {error}",
                                level.name, entry.eid, item.index
                            )
                        });
                        slst_items += 1;
                    }
                    slst_entries += 1;
                }
                ZDAT_ENTRY_TYPE => {
                    let item_zero = entry
                        .item(0)
                        .unwrap_or_else(|| panic!("ZDAT {} has no item zero", entry.eid))
                        .bytes(&nsf_bytes)
                        .unwrap();
                    assert_eq!(
                        item_zero.len(),
                        ZoneHeader::BYTE_LEN,
                        "{} ZDAT {} item zero size",
                        level.name,
                        entry.eid
                    );
                    let header = ZoneHeader::parse(item_zero).unwrap_or_else(|error| {
                        panic!("{} ZDAT {} header: {error}", level.name, entry.eid)
                    });
                    let geometries: Vec<_> = header
                        .worlds
                        .iter()
                        .enumerate()
                        .map(|(world_index, world)| {
                            let geometry_entry = nsf
                                .resolve_entry(&nsd, world.geometry)
                                .unwrap_or_else(|error| {
                                    panic!(
                                        "{} ZDAT {} world {world_index} WGEO {}: {error}",
                                        level.name, entry.eid, world.geometry
                                    )
                                });
                            assert_eq!(
                                geometry_entry.entry_type, WGEO_ENTRY_TYPE,
                                "{} ZDAT {} world {world_index} entry type",
                                level.name, entry.eid
                            );
                            parse_world_geometry(
                                geometry_entry.item(0).unwrap().bytes(&nsf_bytes).unwrap(),
                                geometry_entry.item(1).unwrap().bytes(&nsf_bytes).unwrap(),
                                geometry_entry.item(2).unwrap().bytes(&nsf_bytes).unwrap(),
                            )
                            .unwrap_or_else(|error| {
                                panic!(
                                    "{} ZDAT {} world {world_index} WGEO {}: {error}",
                                    level.name, entry.eid, world.geometry
                                )
                            })
                        })
                        .collect();
                    let rect_bytes = entry
                        .item(1)
                        .unwrap_or_else(|| panic!("ZDAT {} has no rectangle item", entry.eid))
                        .bytes(&nsf_bytes)
                        .unwrap();
                    ZoneRect::parse(rect_bytes).unwrap_or_else(|error| {
                        panic!("{} ZDAT {} rectangle: {error}", level.name, entry.eid)
                    });

                    for path_index in 0..header.path_count {
                        let item_index = usize::try_from(
                            header
                                .path_item_index(path_index)
                                .expect("bounded path index must resolve"),
                        )
                        .expect("retail item index fits the host");
                        let path_bytes = entry
                            .item(item_index)
                            .unwrap_or_else(|| {
                                panic!(
                                    "{} ZDAT {} path {path_index} points outside its items",
                                    level.name, entry.eid
                                )
                            })
                            .bytes(&nsf_bytes)
                            .unwrap();
                        let path = ZonePath::parse(path_bytes).unwrap_or_else(|error| {
                            panic!(
                                "{} ZDAT {} path {path_index}: {error}",
                                level.name, entry.eid
                            )
                        });
                        if nsd.pte(path.visibility_list).is_some() {
                            let visibility = nsf
                                .resolve_entry(&nsd, path.visibility_list)
                                .unwrap_or_else(|error| {
                                    panic!(
                                        "{} ZDAT {} path {path_index} SLST {}: {error}",
                                        level.name, entry.eid, path.visibility_list
                                    )
                                });
                            assert_eq!(visibility.entry_type, SLST_ENTRY_TYPE);
                            assert_eq!(
                                visibility.items.len(),
                                path.points.len() + 1,
                                "{} SLST {} item/path-point relationship",
                                level.name,
                                visibility.eid
                            );

                            let items: Vec<_> = visibility
                                .items
                                .iter()
                                .map(|item| {
                                    SlstItem::parse(item.bytes(&nsf_bytes).unwrap()).unwrap_or_else(
                                        |error| {
                                            panic!(
                                                "{} ZDAT {} path {path_index} SLST {} item {}: \
                                                 {error}",
                                                level.name, entry.eid, visibility.eid, item.index
                                            )
                                        },
                                    )
                                })
                                .collect();
                            assert!(
                                matches!(items.first(), Some(SlstItem::Raw { .. })),
                                "{} ZDAT {} path {path_index} SLST {} first item is not raw",
                                level.name,
                                entry.eid,
                                visibility.eid
                            );
                            assert!(
                                matches!(items.last(), Some(SlstItem::Raw { .. })),
                                "{} ZDAT {} path {path_index} SLST {} last item is not raw",
                                level.name,
                                entry.eid,
                                visibility.eid
                            );

                            mix_fingerprint(&mut slst_fingerprint, level.id.get());
                            mix_fingerprint(&mut slst_fingerprint, entry.eid.raw());
                            mix_fingerprint(&mut slst_fingerprint, path_index);
                            mix_fingerprint(&mut slst_fingerprint, path.visibility_list.raw());
                            mix_fingerprint(
                                &mut slst_fingerprint,
                                u32::try_from(geometries.len()).unwrap(),
                            );
                            for (world, geometry) in header.worlds.iter().zip(&geometries) {
                                mix_fingerprint(&mut slst_fingerprint, world.geometry.raw());
                                mix_fingerprint(
                                    &mut slst_fingerprint,
                                    u32::try_from(geometry.polygons.len()).unwrap(),
                                );
                            }

                            let first = items[0].apply(&[], SlstDirection::Forward).unwrap_or_else(
                                |error| {
                                    panic!(
                                        "{} ZDAT {} path {path_index} SLST {} first raw item: \
                                         {error}",
                                        level.name, entry.eid, visibility.eid
                                    )
                                },
                            );
                            let mut forward_states = Vec::with_capacity(path.points.len());
                            forward_states.push(first);
                            for (item_index, item) in
                                items.iter().enumerate().take(path.points.len()).skip(1)
                            {
                                let source = forward_states.last().unwrap();
                                let decoded = item
                                    .apply(source, SlstDirection::Forward)
                                    .unwrap_or_else(|error| {
                                        panic!(
                                            "{} ZDAT {} path {path_index} SLST {} item \
                                             {item_index} forward: {error}",
                                            level.name, entry.eid, visibility.eid
                                        )
                                    });
                                let restored = item
                                    .apply(&decoded, SlstDirection::Backward)
                                    .unwrap_or_else(|error| {
                                        panic!(
                                            "{} ZDAT {} path {path_index} SLST {} item \
                                             {item_index} inverse backward: {error}",
                                            level.name, entry.eid, visibility.eid
                                        )
                                    });
                                assert_eq!(
                                    restored, *source,
                                    "{} ZDAT {} path {path_index} SLST {} item {item_index} \
                                     forward/backward round-trip",
                                    level.name, entry.eid, visibility.eid
                                );
                                forward_states.push(decoded);
                                decoded_slst_transitions += 1;
                                slst_inverse_round_trips += 1;
                            }

                            let endpoint = items[path.points.len()]
                                .apply(&[], SlstDirection::Backward)
                                .unwrap_or_else(|error| {
                                    panic!(
                                        "{} ZDAT {} path {path_index} SLST {} endpoint raw item: \
                                         {error}",
                                        level.name, entry.eid, visibility.eid
                                    )
                                });
                            assert_eq!(
                                endpoint,
                                *forward_states.last().unwrap(),
                                "{} ZDAT {} path {path_index} SLST {} forward endpoint/raw \
                                 mismatch",
                                level.name,
                                entry.eid,
                                visibility.eid
                            );

                            let mut backward = endpoint;
                            for item_index in (1..path.points.len()).rev() {
                                let decoded = items[item_index]
                                    .apply(&backward, SlstDirection::Backward)
                                    .unwrap_or_else(|error| {
                                        panic!(
                                            "{} ZDAT {} path {path_index} SLST {} item \
                                             {item_index} backward: {error}",
                                            level.name, entry.eid, visibility.eid
                                        )
                                    });
                                assert_eq!(
                                    decoded,
                                    forward_states[item_index - 1],
                                    "{} ZDAT {} path {path_index} SLST {} item {item_index} \
                                     backward state",
                                    level.name,
                                    entry.eid,
                                    visibility.eid
                                );
                                let restored = items[item_index]
                                    .apply(&decoded, SlstDirection::Forward)
                                    .unwrap_or_else(|error| {
                                        panic!(
                                            "{} ZDAT {} path {path_index} SLST {} item \
                                             {item_index} inverse forward: {error}",
                                            level.name, entry.eid, visibility.eid
                                        )
                                    });
                                assert_eq!(
                                    restored, backward,
                                    "{} ZDAT {} path {path_index} SLST {} item {item_index} \
                                     backward/forward round-trip",
                                    level.name, entry.eid, visibility.eid
                                );
                                backward = decoded;
                                slst_inverse_round_trips += 1;
                            }
                            assert_eq!(
                                backward, forward_states[0],
                                "{} ZDAT {} path {path_index} SLST {} backward endpoint/raw \
                                 mismatch",
                                level.name, entry.eid, visibility.eid
                            );

                            for (point_index, polygons) in forward_states.iter().enumerate() {
                                validate_polygon_references(
                                    level.name,
                                    entry.eid,
                                    path_index,
                                    point_index,
                                    polygons,
                                    &geometries,
                                );
                                mix_fingerprint(
                                    &mut slst_fingerprint,
                                    u32::try_from(point_index).unwrap(),
                                );
                                mix_fingerprint(
                                    &mut slst_fingerprint,
                                    u32::try_from(polygons.len()).unwrap(),
                                );
                                for polygon in polygons {
                                    mix_fingerprint(
                                        &mut slst_fingerprint,
                                        u32::from(polygon.raw()),
                                    );
                                }
                                validated_polygon_references += polygons.len();
                            }
                            decoded_slst_paths += 1;
                            decoded_slst_states += forward_states.len();
                            resolved_path_slsts += 1;
                        } else {
                            // A few neighbor-zone paths name visibility entries
                            // supplied by another stream; the local pair cannot
                            // resolve those by design.
                            external_path_slsts += 1;
                        }
                        zdat_paths += 1;
                    }
                    zdat_entries += 1;
                }
                _ => {}
            }
        }

        if let Some(ldat) = nsd.ldat() {
            let zone = nsf
                .resolve_entry(&nsd, ldat.spawn_zone)
                .unwrap_or_else(|error| panic!("{} spawn ZDAT: {error}", level.name));
            assert_eq!(zone.entry_type, ZDAT_ENTRY_TYPE);
            let zone_header = ZoneHeader::parse(zone.item(0).unwrap().bytes(&nsf_bytes).unwrap())
                .unwrap_or_else(|error| panic!("{} spawn header: {error}", level.name));
            ZoneRect::parse(zone.item(1).unwrap().bytes(&nsf_bytes).unwrap())
                .unwrap_or_else(|error| panic!("{} spawn rectangle: {error}", level.name));
            let spawn_path_index = u32::try_from(ldat.spawn_path_index)
                .unwrap_or_else(|_| panic!("{} has a negative spawn path", level.name));
            let spawn_item_index = usize::try_from(
                zone_header
                    .path_item_index(spawn_path_index)
                    .unwrap_or_else(|| panic!("{} spawn path is outside its zone", level.name)),
            )
            .expect("retail item index fits the host");
            let spawn_path = ZonePath::parse(
                zone.item(spawn_item_index)
                    .unwrap()
                    .bytes(&nsf_bytes)
                    .unwrap(),
            )
            .unwrap_or_else(|error| panic!("{} spawn path: {error}", level.name));
            let spawn_slst_is_local = if nsd.pte(spawn_path.visibility_list).is_some() {
                let spawn_slst = nsf
                    .resolve_entry(&nsd, spawn_path.visibility_list)
                    .unwrap_or_else(|error| panic!("{} spawn SLST: {error}", level.name));
                assert_eq!(spawn_slst.entry_type, SLST_ENTRY_TYPE);
                true
            } else {
                false
            };
            for world in &zone_header.worlds {
                let geometry = nsf
                    .resolve_entry(&nsd, world.geometry)
                    .unwrap_or_else(|error| {
                        panic!("{} spawn WGEO {}: {error}", level.name, world.geometry)
                    });
                assert_eq!(geometry.entry_type, WGEO_ENTRY_TYPE);
            }

            mix_fingerprint(&mut spawn_fingerprint, level.id.get());
            mix_fingerprint(&mut spawn_fingerprint, ldat.spawn_zone.raw());
            mix_fingerprint(&mut spawn_fingerprint, spawn_path.visibility_list.raw());
            mix_fingerprint(
                &mut spawn_fingerprint,
                u32::try_from(zone_header.worlds.len()).unwrap(),
            );
            mix_fingerprint(
                &mut spawn_fingerprint,
                u32::try_from(spawn_path.points.len()).unwrap(),
            );
            eprintln!(
                "{}: spawn ZDAT {}, path {}, SLST {} ({}), worlds {}, points {}",
                level.name,
                ldat.spawn_zone,
                spawn_path_index,
                spawn_path.visibility_list,
                if spawn_slst_is_local {
                    "local"
                } else {
                    "external"
                },
                zone_header.worlds.len(),
                spawn_path.points.len()
            );
            spawn_zones += 1;
        }
    }

    assert_eq!(zdat_entries, 1_223);
    assert_eq!(zdat_paths, 1_735);
    assert_eq!(resolved_path_slsts, 1_726);
    assert_eq!(external_path_slsts, 9);
    assert_eq!(wgeo_entries, 520);
    assert_eq!(slst_entries, 1_726);
    assert_eq!(slst_items, 138_038);
    assert_eq!(spawn_zones, 43);
    assert_eq!(spawn_fingerprint, 0xc273_d37f_ea8d_2f99);
    assert_eq!(decoded_slst_paths, 1_726);
    assert_eq!(decoded_slst_states, 136_312);
    assert_eq!(decoded_slst_transitions, 134_586);
    assert_eq!(slst_inverse_round_trips, 269_172);
    assert_eq!(validated_polygon_references, 89_666_970);
    assert_eq!(slst_fingerprint, 0x1400_935c_08cf_e148);
    eprintln!(
        "SLST traversal characterization: {decoded_slst_paths} paths, \
         {decoded_slst_states} states, {decoded_slst_transitions} transitions, \
         {slst_inverse_round_trips} inverse round-trips, \
         {validated_polygon_references} WGEO polygon references, \
         fingerprint {slst_fingerprint:#018x}"
    );
    eprintln!(
        "retail scene characterization: {zdat_entries} ZDAT, {zdat_paths} paths \
         ({resolved_path_slsts} local SLST/{external_path_slsts} external), \
         {wgeo_entries} WGEO, {slst_entries} SLST/{slst_items} items, \
         spawn fingerprint {spawn_fingerprint:#018x}"
    );
}
