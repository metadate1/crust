//! Opt-in title-card characterization against legally local retail streams.

use std::path::PathBuf;

use crust_formats::disc::DiscImage;
use crust_formats::stream::{LevelId, StreamKind, StreamName, parse_nsd, parse_nsf};
use crust_renderer::title::{TITLE_HEIGHT, TITLE_WIDTH, decode_title_card};

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn decodes_every_retail_title_state_without_copying_assets() {
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
    assert_title_cards(&nsd_bytes, &nsf_bytes);
}

#[test]
#[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
fn decodes_every_retail_title_state_directly_from_raw_disc() {
    let disc_path = PathBuf::from(
        std::env::var_os("C1_DISC_IMAGE")
            .expect("C1_DISC_IMAGE must name a legally local NTSC-U raw BIN"),
    );
    let disc_bytes = std::fs::read(&disc_path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", disc_path.display()));
    let disc = DiscImage::open(&disc_bytes)
        .unwrap_or_else(|error| panic!("{}: {error}", disc_path.display()));
    let streams = disc
        .discover_streams()
        .unwrap_or_else(|error| panic!("{}: {error}", disc_path.display()));
    let nsd = streams
        .get(StreamName::new(LevelId::TITLE, StreamKind::Nsd))
        .expect("retail disc is missing the title NSD");
    let nsf = streams
        .get(StreamName::new(LevelId::TITLE, StreamKind::Nsf))
        .expect("retail disc is missing the title NSF");
    let nsd_bytes = disc.read_stream(nsd).expect("could not read title NSD");
    let nsf_bytes = disc.read_stream(nsf).expect("could not read title NSF");
    assert_title_cards(&nsd_bytes, &nsf_bytes);
}

fn assert_title_cards(nsd_bytes: &[u8], nsf_bytes: &[u8]) {
    let metadata =
        parse_nsd(nsd_bytes, LevelId::TITLE).unwrap_or_else(|error| panic!("title NSD: {error}"));
    let nsf = parse_nsf(nsf_bytes, &metadata).unwrap_or_else(|error| panic!("title NSF: {error}"));

    // These four states use the retail tiled-image path. The options,
    // password/load, game-over, and map states are ZDAT/WGEO + GOOL screens.
    for state in [5_u8, 7, 8, 10] {
        let card = decode_title_card(&metadata, &nsf, nsf_bytes, state)
            .unwrap_or_else(|error| panic!("title state {state}: {error}"));
        assert_eq!(card.image.width(), TITLE_WIDTH);
        assert_eq!(card.image.height(), TITLE_HEIGHT);
        assert!(
            card.image
                .rgba()
                .chunks_exact(4)
                .any(|pixel| pixel[..3] != [0, 0, 0]),
            "title state {state} decoded to a black frame"
        );
        assert!(
            card.image
                .rgba()
                .chunks_exact(4)
                .all(|pixel| pixel[3] == u8::MAX),
            "title state {state} contains blended pixels"
        );
    }
}
