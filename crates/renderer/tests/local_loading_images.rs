//! Opt-in loading-image characterization against legally local retail streams.

use std::path::PathBuf;

use crust_formats::stream::{KNOWN_LEVELS, parse_nsd};
use crust_renderer::texture::decode_loading_image;

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn decodes_every_local_retail_loading_image_without_copying_assets() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name a local extracted stream directory"),
    );
    let mut decoded_images = 0_usize;

    for known in KNOWN_LEVELS.iter().filter(|known| known.bootable) {
        let path = root.join(known.nsd_filename());
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()));
        let metadata = parse_nsd(&bytes, known.id)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let Some(payload) = metadata
            .image_data(&bytes)
            .unwrap_or_else(|error| panic!("{} loading image: {error}", path.display()))
        else {
            continue;
        };
        let image = decode_loading_image(
            payload,
            metadata.header.loading_image_width,
            metadata.header.loading_image_height,
        )
        .unwrap_or_else(|error| panic!("{} loading image: {error}", path.display()));
        assert_eq!(image.width(), metadata.header.loading_image_width);
        assert_eq!(image.height(), metadata.header.loading_image_height);
        assert_eq!(
            image.byte_len(),
            usize::try_from(image.width()).unwrap() * usize::try_from(image.height()).unwrap() * 4
        );
        decoded_images += 1;
    }

    assert!(
        decoded_images > 0,
        "the local retail stream set did not contain a loading image"
    );
    eprintln!("decoded {decoded_images} legally local retail loading images");
}
