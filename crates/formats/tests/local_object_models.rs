//! Opt-in exhaustive TGEO/SVTX/CVTX validation against legally local streams.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crust_formats::stream::{
    CVTX_ENTRY_TYPE, KNOWN_LEVELS, ObjectGeometry, ObjectModelFrame, ObjectVertexKind,
    SVTX_ENTRY_TYPE, TGEO_ENTRY_TYPE, parse_nsd, parse_nsf, parse_object_frame,
    parse_object_geometry,
};

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn parses_every_retail_object_geometry_and_animation_frame() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name local extracted retail streams"),
    );
    let mut geometry_variants: BTreeMap<_, Vec<ObjectGeometry>> = BTreeMap::new();
    let mut geometry_occurrence_count = 0_usize;
    let mut frame_count = 0_usize;
    let mut resolved_frame_count = 0_usize;
    let mut dormant_frame_count = 0_usize;
    let mut dormant_geometry_eids = BTreeSet::new();
    let mut vertex_reference_count = 0_usize;

    // Characterize every pair-scoped TGEO variant first. EIDs are reused
    // across retail pairs and their definitions can differ, so this catalog
    // is diagnostic only: runtime resolution must retain the active pair.
    for known in KNOWN_LEVELS {
        let nsd_path = root.join(known.nsd_filename());
        let nsf_path = root.join(known.nsf_filename());
        let nsd_bytes = std::fs::read(&nsd_path)
            .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
        let nsf_bytes = std::fs::read(&nsf_path)
            .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));
        let nsd = parse_nsd(&nsd_bytes, known.id)
            .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
        let nsf = parse_nsf(&nsf_bytes, &nsd)
            .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));

        for entry in nsf.entries() {
            if entry.entry_type == TGEO_ENTRY_TYPE {
                let header = entry
                    .item(0)
                    .unwrap_or_else(|| panic!("{} TGEO has no item zero", entry.eid))
                    .bytes(&nsf_bytes)
                    .unwrap();
                let polygons = entry
                    .item(1)
                    .unwrap_or_else(|| panic!("{} TGEO has no item one", entry.eid))
                    .bytes(&nsf_bytes)
                    .unwrap();
                let geometry = parse_object_geometry(header, polygons)
                    .unwrap_or_else(|error| panic!("{} {}: {error}", known.name, entry.eid));
                geometry_occurrence_count += 1;
                let variants = geometry_variants.entry(entry.eid).or_default();
                if !variants.contains(&geometry) {
                    variants.push(geometry);
                }
            }
        }
    }

    let polygon_variant_count: usize = geometry_variants
        .values()
        .flatten()
        .map(|geometry| geometry.polygons.len())
        .sum();

    // Validate every animation frame against its resident pair. Dormant
    // references can name a TGEO found only in another pair; those are parsed
    // and reported but must not be resolved through an ambiguous global EID.
    for known in KNOWN_LEVELS {
        let nsd_path = root.join(known.nsd_filename());
        let nsf_path = root.join(known.nsf_filename());
        let nsd_bytes = std::fs::read(&nsd_path)
            .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
        let nsf_bytes = std::fs::read(&nsf_path)
            .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));
        let nsd = parse_nsd(&nsd_bytes, known.id)
            .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
        let nsf = parse_nsf(&nsf_bytes, &nsd)
            .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));

        let mut resident_geometries = BTreeMap::new();
        for entry in nsf
            .entries()
            .filter(|entry| entry.entry_type == TGEO_ENTRY_TYPE)
        {
            let header = entry.item(0).expect("retail TGEO has item zero");
            let polygons = entry.item(1).expect("retail TGEO has item one");
            let geometry = parse_object_geometry(
                header.bytes(&nsf_bytes).unwrap(),
                polygons.bytes(&nsf_bytes).unwrap(),
            )
            .unwrap_or_else(|error| panic!("{} {}: {error}", known.name, entry.eid));
            if let Some(previous) = resident_geometries.insert(entry.eid, geometry.clone()) {
                assert_eq!(
                    previous, geometry,
                    "{} contains conflicting resident definitions for {}",
                    known.name, entry.eid
                );
            }
        }

        for entry in nsf
            .entries()
            .filter(|entry| matches!(entry.entry_type, SVTX_ENTRY_TYPE | CVTX_ENTRY_TYPE))
        {
            let kind = ObjectVertexKind::from_entry_type(entry.entry_type).unwrap();
            for (frame_index, item) in entry.items.iter().enumerate() {
                let frame_index =
                    u16::try_from(frame_index).expect("retail entry item count fits a frame index");
                let frame = parse_object_frame(item.bytes(&nsf_bytes).unwrap(), kind)
                    .unwrap_or_else(|error| {
                        panic!("{} {} frame {frame_index}: {error}", known.name, entry.eid)
                    });
                frame_count += 1;
                let Some(geometry) = resident_geometries.get(&frame.header.geometry_eid).cloned()
                else {
                    assert!(
                        geometry_variants.contains_key(&frame.header.geometry_eid),
                        "{} {} frame {frame_index}: TGEO {} is absent from every retail pair",
                        known.name,
                        entry.eid,
                        frame.header.geometry_eid
                    );
                    dormant_frame_count += 1;
                    dormant_geometry_eids.insert(frame.header.geometry_eid);
                    continue;
                };
                let model = ObjectModelFrame::validated(entry.eid, frame_index, frame, geometry)
                    .unwrap_or_else(|error| {
                        panic!("{} {} frame {frame_index}: {error}", known.name, entry.eid)
                    });
                resolved_frame_count += 1;
                vertex_reference_count += model.geometry.polygons.len() * 3;
            }
        }
    }

    assert!(geometry_occurrence_count > 0);
    assert!(!geometry_variants.is_empty());
    assert!(frame_count > 0);
    assert!(resolved_frame_count > 0);
    assert!(polygon_variant_count > 0);
    assert!(vertex_reference_count > 0);
    eprintln!(
        "parsed {geometry_occurrence_count} TGEO occurrences ({} EIDs, {} exact variants and {polygon_variant_count} variant polygons); parsed {frame_count} SVTX/CVTX frames and fully validated {resolved_frame_count} resident frames / {vertex_reference_count} vertex references; characterized {dormant_frame_count} dormant cross-pair frames naming {dormant_geometry_eids:?}",
        geometry_variants.len(),
        geometry_variants.values().map(Vec::len).sum::<usize>()
    );
}
