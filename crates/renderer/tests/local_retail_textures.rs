//! Opt-in characterization of WGEO texture references from legally local data.

use std::collections::HashSet;
use std::path::PathBuf;

use crust_formats::stream::structs::{ColorInfo, WorldGeometryHeader};
use crust_formats::stream::{LevelId, StreamKind, StreamName, parse_nsd, parse_nsf};
use crust_renderer::retail_texture::{RetailTextureReference, TextureInfo2, TpagReference};
use crust_renderer::texture::ColorMode;

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn decodes_representative_wgeo_regions_without_copying_assets() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name a local extracted stream directory"),
    );
    let nsd_path = root.join(StreamName::new(LevelId::TITLE, StreamKind::Nsd).filename());
    let nsf_path = root.join(StreamName::new(LevelId::TITLE, StreamKind::Nsf).filename());
    let nsd_bytes = std::fs::read(&nsd_path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", nsd_path.display()));
    let nsf_bytes = std::fs::read(&nsf_path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", nsf_path.display()));
    let metadata = parse_nsd(&nsd_bytes, LevelId::TITLE)
        .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
    let nsf = parse_nsf(&nsf_bytes, &metadata)
        .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));

    let mut references = HashSet::new();
    for entry in nsf.entries().filter(|entry| entry.entry_type == 3) {
        let header_item = entry
            .item(0)
            .expect("WGEO is missing its header item")
            .bytes(&nsf_bytes)
            .expect("WGEO header range was already validated");
        let polygon_item = entry
            .item(1)
            .expect("WGEO is missing its polygon item")
            .bytes(&nsf_bytes)
            .expect("WGEO polygon range was already validated");
        let header = WorldGeometryHeader::parse(header_item)
            .unwrap_or_else(|error| panic!("WGEO {}: {error}", entry.eid));
        let texture_word_count = usize::try_from(header.texture_info_count)
            .expect("WGEO texture word count does not fit host");
        let table_bytes = texture_word_count
            .checked_mul(4)
            .expect("WGEO texture table size overflow");
        let table_end = WorldGeometryHeader::BYTE_LEN
            .checked_add(table_bytes)
            .expect("WGEO texture table end overflow");
        let table = header_item
            .get(WorldGeometryHeader::BYTE_LEN..table_end)
            .unwrap_or_else(|| panic!("WGEO {} has a truncated texture table", entry.eid));
        let polygon_count =
            usize::try_from(header.polygon_count).expect("WGEO polygon count does not fit host");
        let polygon_bytes = polygon_count
            .checked_mul(8)
            .expect("WGEO polygon table size overflow");
        assert!(
            polygon_item.len() >= polygon_bytes,
            "WGEO {} has a truncated polygon table",
            entry.eid
        );

        for polygon in polygon_item[..polygon_bytes].chunks_exact(8) {
            // Exact packed word zero: phase[0:5], TPAG[5:8], TINF[8:20], C[20:32].
            let word = u32::from_le_bytes(polygon[..4].try_into().unwrap());
            let texture_word_index = usize::try_from((word >> 8) & 0x0fff).unwrap();
            let color_offset = texture_word_index
                .checked_mul(4)
                .expect("WGEO color-word offset overflow");
            let color_bytes = table
                .get(color_offset..color_offset + 4)
                .unwrap_or_else(|| panic!("WGEO {} color word is truncated", entry.eid));
            let color = ColorInfo::from_raw(u32::from_le_bytes(color_bytes.try_into().unwrap()));
            if color.color_type() == 0 {
                continue;
            }
            let info = TextureInfo2::parse_wgeo_table(table, texture_word_index, 0)
                .unwrap_or_else(|error| panic!("WGEO {} texture: {error}", entry.eid));
            let texture_page_index = usize::try_from((word >> 5) & 7).unwrap();
            assert!(
                texture_page_index < usize::try_from(header.texture_page_count).unwrap(),
                "WGEO {} polygon selects an undeclared TPAG",
                entry.eid
            );
            references.insert(RetailTextureReference::new(
                TpagReference::new(header.texture_pages[texture_page_index]),
                info,
            ));
        }
    }

    let mut mode_counts = [0_usize; 3];
    let mut decoded_pixels = 0_usize;
    let mut visible_pixels = 0_usize;
    for reference in references.iter().copied() {
        let layout = reference
            .layout()
            .unwrap_or_else(|error| panic!("{reference:?}: {error}"));
        mode_counts[layout.request.color_mode as usize] += 1;
        let decoded = reference
            .decode(&nsf, &nsf_bytes)
            .unwrap_or_else(|error| panic!("{reference:?}: {error}"));
        decoded_pixels += decoded.rgba().len() / 4;
        visible_pixels += decoded
            .rgba()
            .chunks_exact(4)
            .filter(|pixel| pixel[3] != 0 && pixel[..3] != [0, 0, 0])
            .count();
    }

    assert!(
        references.len() > 1_000,
        "unexpectedly little WGEO coverage"
    );
    assert!(mode_counts[ColorMode::Indexed4 as usize] > 0);
    assert!(mode_counts[ColorMode::Indexed8 as usize] > 0);
    assert!(mode_counts[ColorMode::Direct15 as usize] > 0);
    assert!(decoded_pixels > references.len());
    assert!(visible_pixels > 0, "all decoded retail regions were blank");
}
