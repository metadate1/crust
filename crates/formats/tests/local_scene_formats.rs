//! Opt-in, read-only characterization of retail scene-format data.

use std::path::{Path, PathBuf};

use crust_formats::disc::{DiscImage, DiscStreamSet};
use crust_formats::stream::{
    KNOWN_LEVELS, SlstItem, StreamKind, StreamName, ZoneHeader, ZonePath, ZoneRect, parse_nsd,
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
    eprintln!(
        "retail scene characterization: {zdat_entries} ZDAT, {zdat_paths} paths \
         ({resolved_path_slsts} local SLST/{external_path_slsts} external), \
         {wgeo_entries} WGEO, {slst_entries} SLST/{slst_items} items, \
         spawn fingerprint {spawn_fingerprint:#018x}"
    );
}
