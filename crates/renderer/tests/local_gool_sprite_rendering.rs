//! Opt-in non-vertex rendering characterization on legally local streams.

use std::path::PathBuf;

use crust_formats::stream::structs::GoolHeader;
use crust_formats::stream::{
    GoolAnimationDescriptor, KNOWN_LEVELS, parse_gool_animation_descriptor, parse_nsd, parse_nsf,
};
use crust_renderer::retail_texture::{RetailTextureReference, TextureInfo2, TpagReference};
use crust_renderer::sprite::{
    RetailSpriteTransform, RetailSpriteVectors, project_retail_fragment, project_retail_sprite,
};

const GOOL_ENTRY_TYPE: u32 = 11;
const DECODE_SAMPLE_LIMIT: usize = 64;

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn local_retail_sprite_and_fragment_payloads_decode_and_project() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name local extracted retail streams"),
    );
    let vectors = RetailSpriteVectors {
        translation: [0, 0, 0],
        rotation_yxz: [0, 0, 0],
        scale: [0x1000; 3],
    };
    let transform = RetailSpriteTransform::screen_2d(vectors, 0, 500).unwrap();
    let mut sprite_descriptors = 0_usize;
    let mut fragment_descriptors = 0_usize;
    let mut decoded_sprites = 0_usize;
    let mut decoded_fragments = 0_usize;
    let mut projected_sprites = 0_usize;
    let mut projected_fragments = 0_usize;

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
                if !matches!(animations[offset], 2 | 5) {
                    continue;
                }
                let Ok(descriptor) = parse_gool_animation_descriptor(animations, offset) else {
                    continue;
                };
                match descriptor {
                    GoolAnimationDescriptor::Sprite(sprite) => {
                        sprite_descriptors += 1;
                        if project_retail_sprite(transform, 200, 500, 1798).is_some() {
                            projected_sprites += 1;
                        }
                        if decoded_sprites < DECODE_SAMPLE_LIMIT {
                            for frame in sprite.frames {
                                let reference = RetailTextureReference::new(
                                    TpagReference::new(sprite.texture_page),
                                    TextureInfo2 {
                                        color: frame.color,
                                        region: frame.region,
                                    },
                                );
                                if reference.decode(&nsf, &nsf_bytes).is_ok() {
                                    decoded_sprites += 1;
                                    break;
                                }
                            }
                        }
                    }
                    GoolAnimationDescriptor::Fragment(animation) => {
                        fragment_descriptors += 1;
                        if let Some(fragment) = animation.fragments.first() {
                            if project_retail_fragment(
                                transform,
                                fragment.bounds.map(i32::from),
                                500,
                                0,
                            )
                            .is_some()
                            {
                                projected_fragments += 1;
                            }
                            if decoded_fragments < DECODE_SAMPLE_LIMIT {
                                let reference = RetailTextureReference::new(
                                    TpagReference::new(animation.texture_page),
                                    TextureInfo2 {
                                        color: fragment.texture.color,
                                        region: fragment.texture.region,
                                    },
                                );
                                if reference.decode(&nsf, &nsf_bytes).is_ok() {
                                    decoded_fragments += 1;
                                }
                            }
                        }
                    }
                    _ => unreachable!("candidate byte filter selected only types two and five"),
                }
            }
        }
    }

    eprintln!(
        "legal corpus: {sprite_descriptors} sprite descriptors ({decoded_sprites} decoded samples, {projected_sprites} projected); {fragment_descriptors} fragment descriptors ({decoded_fragments} decoded samples, {projected_fragments} projected)"
    );
    assert!(sprite_descriptors > 0);
    assert!(fragment_descriptors > 0);
    assert_eq!(decoded_sprites, DECODE_SAMPLE_LIMIT);
    assert_eq!(decoded_fragments, DECODE_SAMPLE_LIMIT);
    assert!(projected_sprites > 0);
    assert!(projected_fragments > 0);
}
