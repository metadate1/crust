//! Opt-in type-four text/font characterization on legally local streams.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crust_formats::stream::structs::GoolHeader;
use crust_formats::stream::{
    GoolAnimationDescriptor, KNOWN_LEVELS, parse_gool_animation_descriptor, parse_nsd, parse_nsf,
};
use crust_renderer::retail_texture::{RetailTextureReference, TextureInfo2, TpagReference};
use crust_renderer::sprite::{RetailSpriteTransform, RetailSpriteVectors};
use crust_renderer::text::{RetailTextProjection, format_retail_text, project_retail_text};

const GOOL_ENTRY_TYPE: u32 = 11;
const DECODE_SAMPLE_LIMIT: usize = 64;

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn local_retail_text_terms_and_font_links_are_characterized() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name local extracted retail streams"),
    );
    let mut text_descriptors = 0_usize;
    let mut terms = 0_usize;
    let mut formatted_terms = 0_usize;
    let mut projected_terms = 0_usize;
    let mut projected_quads = 0_usize;
    let mut decoded_glyphs = 0_usize;
    let mut controller_icon_terms = 0_usize;
    let mut decoded_controller_icon_terms = 0_usize;
    let mut projection_errors = BTreeMap::<String, usize>::new();
    let transform = RetailSpriteTransform::screen_2d(
        RetailSpriteVectors {
            translation: [0, 0, 0],
            rotation_yxz: [0, 0, 0],
            scale: [0x1000; 3],
        },
        0,
        500,
    )
    .unwrap();
    let arguments = [Some(2); 10];

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
            let header = entry.item(0).unwrap().bytes(&nsf_bytes).unwrap();
            if header.len() != GoolHeader::BYTE_LEN || GoolHeader::parse(header).is_err() {
                continue;
            }
            let animations = entry.item(5).unwrap().bytes(&nsf_bytes).unwrap();
            for offset in 0..animations.len() {
                if animations[offset] != 4 {
                    continue;
                }
                let Ok(GoolAnimationDescriptor::Text(text)) =
                    parse_gool_animation_descriptor(animations, offset)
                else {
                    continue;
                };
                let font_offset = usize::try_from(text.font_word_offset)
                    .ok()
                    .and_then(|value| value.checked_mul(4))
                    .unwrap();
                let Ok(GoolAnimationDescriptor::Font(font)) =
                    parse_gool_animation_descriptor(animations, font_offset)
                else {
                    // Byte-wise discovery intentionally encounters embedded
                    // type tags. A complete header-length-bounded font parse
                    // is the ownership proof for a real type-four descriptor.
                    continue;
                };
                text_descriptors += 1;
                terms += text.terms.len();
                if decoded_glyphs < DECODE_SAMPLE_LIMIT {
                    for glyph in font.glyphs.iter().filter(|glyph| glyph.has_texture()) {
                        let reference = RetailTextureReference::new(
                            TpagReference::new(font.texture_page),
                            TextureInfo2 {
                                color: glyph.texture.color,
                                region: glyph.texture.region,
                            },
                        );
                        if reference.decode(&nsf, &nsf_bytes).is_ok() {
                            decoded_glyphs += 1;
                            if decoded_glyphs == DECODE_SAMPLE_LIMIT {
                                break;
                            }
                        }
                    }
                }
                for term in text.terms {
                    let controller_icon = matches!(term.as_slice(), b"c" | b"s" | b"t" | b"x");
                    if controller_icon {
                        controller_icon_terms += 1;
                        let glyph = font.glyphs[usize::from(term[0] - 0x20)];
                        RetailTextureReference::new(
                            TpagReference::new(font.texture_page),
                            TextureInfo2 {
                                color: glyph.texture.color,
                                region: glyph.texture.region,
                            },
                        )
                        .decode(&nsf, &nsf_bytes)
                        .expect("CardC controller icon texture must decode");
                        decoded_controller_icon_terms += 1;
                    }
                    if format_retail_text(&term, &arguments).is_ok() {
                        formatted_terms += 1;
                    }
                    match project_retail_text(RetailTextProjection {
                        term: &term,
                        font: &font,
                        negative_stack_arguments: &arguments,
                        transform,
                        shrink: 0,
                        projection_distance: 500,
                        object_size: 0,
                        center_by_width: true,
                        center_backdrop: true,
                        vertex_colors: [[256; 3]; 4],
                    }) {
                        Ok(projected) => {
                            projected_terms += 1;
                            projected_quads += projected.quads.len();
                        }
                        Err(error) => {
                            *projection_errors.entry(error.to_string()).or_default() += 1;
                        }
                    }
                }
            }
        }
    }

    eprintln!(
        "legal corpus: {text_descriptors} validated text/font pairs, {terms} terms, {formatted_terms} formatted, {projected_terms} projected, {projected_quads} glyph/backdrop quads, {decoded_glyphs} decoded glyph samples"
    );
    for (error, count) in &projection_errors {
        eprintln!("projection error {count}: {error}");
    }
    assert!(text_descriptors > 0);
    assert!(terms > 0);
    assert_eq!(formatted_terms, terms);
    assert_eq!(projected_terms, terms);
    assert!(projection_errors.is_empty(), "{projection_errors:?}");
    // CardC retains eight copies of each controller-button icon. They live in
    // the variable tail of its 90-record font and are active title UI terms,
    // not malformed lowercase text or non-drawable state sentinels.
    assert_eq!(controller_icon_terms, 32);
    assert_eq!(decoded_controller_icon_terms, controller_icon_terms);
    assert!(projected_quads > 0);
    assert_eq!(decoded_glyphs, DECODE_SAMPLE_LIMIT);
}
