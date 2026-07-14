//! Opt-in MDAT/entity characterization against legally local retail data.

use std::path::PathBuf;

use crust_formats::{
    disc::DiscImage,
    stream::{LevelId, StreamKind, StreamName, load_title_mdat, parse_nsd, parse_nsf},
};

#[test]
#[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
fn parses_every_authored_image_title_mdat_directly_from_raw_disc() {
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
    let nsd_stream = streams
        .get(StreamName::new(LevelId::TITLE, StreamKind::Nsd))
        .expect("retail disc is missing the title NSD");
    let nsf_stream = streams
        .get(StreamName::new(LevelId::TITLE, StreamKind::Nsf))
        .expect("retail disc is missing the title NSF");
    let nsd_bytes = disc
        .read_stream(nsd_stream)
        .expect("could not read title NSD");
    let nsf_bytes = disc
        .read_stream(nsf_stream)
        .expect("could not read title NSF");
    let metadata =
        parse_nsd(&nsd_bytes, LevelId::TITLE).unwrap_or_else(|error| panic!("title NSD: {error}"));
    let nsf = parse_nsf(&nsf_bytes, &metadata).unwrap_or_else(|error| panic!("title NSF: {error}"));

    for state in [5_u8, 7, 8, 10] {
        let mdat = load_title_mdat(&metadata, &nsf, &nsf_bytes, state)
            .unwrap_or_else(|error| panic!("title state {state}: {error}"));
        assert_eq!(
            usize::try_from(mdat.header.entity_count).unwrap(),
            mdat.entities.len(),
            "title state {state} did not retain every serialized entity"
        );
        assert!(
            mdat.entities
                .iter()
                .all(|entity| !entity.path_points.is_empty()),
            "title state {state} contains an entity without its retail path"
        );
        eprintln!(
            "title state {state} {}: entities={} spawnable={}",
            mdat.eid,
            mdat.entities.len(),
            mdat.entities
                .iter()
                .filter(|entity| entity.group == 3)
                .count(),
        );
    }
}
