//! Opt-in characterization of retail ZDAT entities and their GOOL bindings.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crust_formats::binary::Eid;
use crust_formats::disc::{DiscImage, DiscStreamSet};
use crust_formats::stream::{
    KNOWN_LEVELS, StreamKind, StreamName, ZoneEntity, ZoneHeader, ZoneRect, load_gool_program,
    parse_nsd, parse_nsf,
};

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
    image
        .read_stream(
            streams
                .get(name)
                .unwrap_or_else(|| panic!("disc is missing {name}")),
        )
        .unwrap_or_else(|error| panic!("could not read {name} from disc: {error}"))
}

fn mix(fingerprint: &mut u64, value: u32) {
    for byte in value.to_le_bytes() {
        *fingerprint ^= u64::from(byte);
        *fingerprint = fingerprint.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

#[test]
#[ignore = "set C1_STREAM_DIR or C1_DISC_IMAGE to legally local NTSC-U data"]
fn parses_every_retail_entity_and_resolves_its_gool_program() {
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
    let disc_image = disc_bytes
        .as_deref()
        .map(|bytes| DiscImage::open(bytes).expect("could not open local disc"));
    let disc_streams = disc_image.as_ref().map(|image| {
        image
            .discover_streams()
            .expect("could not discover local disc streams")
    });
    if let Some(streams) = &disc_streams {
        streams
            .validate_complete_retail()
            .expect("local disc stream set is incomplete");
    }
    let disc = disc_image.as_ref().zip(disc_streams.as_ref());

    let mut entity_count = 0_usize;
    let mut path_point_count = 0_usize;
    let mut group_three_count = 0_usize;
    let mut main_candidate_count = 0_usize;
    let mut program_attempts = BTreeSet::new();
    let mut valid_program_bindings = 0_usize;
    let mut rejected_program_bindings = 0_usize;
    let mut executable_histogram = BTreeMap::<u8, usize>::new();
    let mut fingerprint = 0xcbf2_9ce4_8422_2325_u64;

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
        let Some(ldat) = nsd.ldat() else {
            continue;
        };

        for zone in nsf
            .entries()
            .filter(|entry| entry.entry_type == ZDAT_ENTRY_TYPE)
        {
            let header = ZoneHeader::parse(zone.item(0).unwrap().bytes(&nsf_bytes).unwrap())
                .unwrap_or_else(|error| panic!("{} ZDAT {}: {error}", level.name, zone.eid));
            let rect = ZoneRect::parse(zone.item(1).unwrap().bytes(&nsf_bytes).unwrap())
                .unwrap_or_else(|error| {
                    panic!("{} ZDAT {} rectangle: {error}", level.name, zone.eid)
                });
            mix(&mut fingerprint, level.id.get());
            mix(&mut fingerprint, zone.eid.raw());
            mix(&mut fingerprint, header.entity_count);

            for entity_index in 0..header.entity_count {
                let item_index = usize::try_from(
                    header
                        .entity_item_index(entity_index)
                        .expect("bounded entity index must resolve"),
                )
                .expect("retail item index fits the host");
                let entity = ZoneEntity::parse(
                    zone.item(item_index)
                        .unwrap_or_else(|| {
                            panic!(
                                "{} ZDAT {} entity {entity_index} points outside its items",
                                level.name, zone.eid
                            )
                        })
                        .bytes(&nsf_bytes)
                        .unwrap(),
                )
                .unwrap_or_else(|error| {
                    panic!(
                        "{} ZDAT {} entity {entity_index}: {error}",
                        level.name, zone.eid
                    )
                });
                let global_eid = ldat.executable_map[usize::from(entity.executable)];
                assert_ne!(
                    global_eid,
                    Eid::NONE,
                    "{} ZDAT {} entity {entity_index} executable 0x{:02x} is absent",
                    level.name,
                    zone.eid,
                    entity.executable
                );
                if program_attempts.insert((level.id, global_eid, entity.subtype)) {
                    if load_gool_program(
                        &nsd,
                        &nsf,
                        &nsf_bytes,
                        global_eid,
                        u16::from(entity.subtype),
                    )
                    .is_ok()
                    {
                        valid_program_bindings += 1;
                    } else {
                        // Retail also rejects an entity whose subtype maps to
                        // 0xff, returns CODE_ERROR, and leaves its spawn bit
                        // set. Preserve that data fact rather than inventing a
                        // fallback subtype.
                        rejected_program_bindings += 1;
                    }
                }

                let first = entity.path_points[0];
                for (origin, coordinate) in rect.origin.into_iter().zip([first.x, first.y, first.z])
                {
                    let location = i64::from(origin) + (i64::from(coordinate) << 2);
                    assert!(
                        (i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(&(location << 8)),
                        "{} ZDAT {} entity {entity_index} start location overflows i32",
                        level.name,
                        zone.eid
                    );
                }

                entity_count += 1;
                path_point_count += entity.path_points.len();
                group_three_count += usize::from(entity.group == 3);
                let is_main = entity.group == 3
                    && (entity.executable == 0
                        || (1..5).contains(&entity.id)
                        || (entity.executable == 0x2c && entity.subtype == 0)
                        || (entity.executable == 0x30 && entity.subtype == 0));
                main_candidate_count += usize::from(is_main);
                *executable_histogram.entry(entity.executable).or_default() += 1;
                mix(&mut fingerprint, u32::from(entity.id));
                mix(&mut fingerprint, u32::from(entity.group));
                mix(&mut fingerprint, u32::from(entity.spawn_flags));
                mix(&mut fingerprint, u32::from(entity.executable));
                mix(&mut fingerprint, u32::from(entity.subtype));
                mix(
                    &mut fingerprint,
                    u32::try_from(entity.path_points.len()).unwrap(),
                );
                for point in &entity.path_points {
                    mix(
                        &mut fingerprint,
                        u32::from_ne_bytes(i32::from(point.x).to_ne_bytes()),
                    );
                    mix(
                        &mut fingerprint,
                        u32::from_ne_bytes(i32::from(point.y).to_ne_bytes()),
                    );
                    mix(
                        &mut fingerprint,
                        u32::from_ne_bytes(i32::from(point.z).to_ne_bytes()),
                    );
                }
            }
        }
    }

    eprintln!(
        "entities={entity_count}, path_points={path_point_count}, group3={group_three_count}, \
         main_candidates={main_candidate_count}, programs={valid_program_bindings}, \
         rejected_programs={rejected_program_bindings}, fingerprint={fingerprint:#018x}, \
         executable_histogram={executable_histogram:?}",
    );
    assert_eq!(entity_count, 4_292);
    assert_eq!(path_point_count, 16_363);
    assert_eq!(group_three_count, 4_292);
    assert_eq!(main_candidate_count, 52);
    assert_eq!(valid_program_bindings, 624);
    assert_eq!(rejected_program_bindings, 7);
    assert_eq!(program_attempts.len(), 631);
    assert_eq!(fingerprint, 0x7152_4c62_fcbf_6ddb);
}
