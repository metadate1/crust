//! Opt-in GOOL animation-payload characterization on legally local streams.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crust_formats::stream::structs::GoolHeader;
use crust_formats::stream::{
    GoolAnimationDescriptor, GoolAnimationKind, KNOWN_LEVELS, parse_gool_animation_descriptor,
    parse_nsd, parse_nsf,
};

const GOOL_ENTRY_TYPE: u32 = 11;

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn parses_representative_payloads_of_all_five_kinds_from_retail_items() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name local extracted retail streams"),
    );
    let mut counts = BTreeMap::new();
    let mut global_entries = 0_usize;
    let mut candidate_offsets = 0_usize;

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

        for entry in nsf
            .entries()
            .filter(|entry| entry.entry_type == GOOL_ENTRY_TYPE && entry.items.len() >= 6)
        {
            let header_bytes = entry.item(0).unwrap().bytes(&nsf_bytes).unwrap();
            if header_bytes.len() != GoolHeader::BYTE_LEN
                || GoolHeader::parse(header_bytes).is_err()
            {
                continue;
            }
            global_entries += 1;
            let animation_bytes = entry.item(5).unwrap().bytes(&nsf_bytes).unwrap();

            // Item five may contain tables and padding in addition to packed
            // descriptors; only successfully validated descriptor candidates
            // are characterization evidence. Runtime tests separately parse
            // exact typed AnimationReference offsets selected by GOOL.
            for offset in 0..animation_bytes.len() {
                if !(1..=5).contains(&animation_bytes[offset]) {
                    continue;
                }
                let Ok(descriptor) = parse_gool_animation_descriptor(animation_bytes, offset)
                else {
                    continue;
                };
                candidate_offsets += 1;
                *counts
                    .entry(descriptor.header().kind as u8)
                    .or_insert(0_usize) += 1;

                if let GoolAnimationDescriptor::Vertex(vertex) = descriptor
                    && let Ok(model) = nsf.resolve_entry(&nsd, vertex.model_eid)
                {
                    assert!(
                        usize::from(vertex.header.length) <= model.items.len(),
                        "{} {} names {} frames but resident {} has only {}",
                        known.name,
                        entry.eid,
                        vertex.header.length,
                        vertex.model_eid,
                        model.items.len()
                    );
                }
            }
        }
    }

    assert!(global_entries > 0);
    for kind in 1..=GoolAnimationKind::Fragment as u8 {
        assert!(
            counts.get(&kind).copied().unwrap_or(0) > 0,
            "retail corpus yielded no validated type-{kind} payload"
        );
    }
    eprintln!(
        "validated {candidate_offsets} candidate GOOL payload offsets across {global_entries} global entries: {counts:?}"
    );
}
