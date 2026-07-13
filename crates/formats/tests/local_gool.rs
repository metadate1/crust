//! Opt-in characterization of GOOL entry graphs on a legally local disc.

use std::path::PathBuf;

use crust_formats::binary::Eid;
use crust_formats::disc::DiscImage;
use crust_formats::stream::{
    LevelId, NsdKind, StreamKind, StreamName, load_gool_program, parse_nsd, parse_nsf,
};

#[test]
#[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
fn resolves_every_mounted_title_executable_for_at_least_one_subtype() {
    let disc_path = PathBuf::from(
        std::env::var_os("C1_DISC_IMAGE")
            .expect("C1_DISC_IMAGE must name a legally local NTSC-U raw BIN"),
    );
    let disc_bytes = std::fs::read(&disc_path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", disc_path.display()));
    let image = DiscImage::open(&disc_bytes).unwrap();
    let streams = image.discover_streams().unwrap();
    let nsd_stream = streams
        .get(StreamName::new(LevelId::TITLE, StreamKind::Nsd))
        .unwrap();
    let nsf_stream = streams
        .get(StreamName::new(LevelId::TITLE, StreamKind::Nsf))
        .unwrap();
    let nsd_bytes = image.read_stream(nsd_stream).unwrap();
    let nsf_bytes = image.read_stream(nsf_stream).unwrap();
    let metadata = parse_nsd(&nsd_bytes, LevelId::TITLE).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &metadata).unwrap();
    let NsdKind::Playable(ldat) = &metadata.kind else {
        panic!("title stream must have playable LDAT metadata");
    };

    let mut executable_count = 0;
    let mut program_count = 0;
    for eid in ldat.executable_map {
        if eid == Eid::NONE {
            continue;
        }
        executable_count += 1;
        let mut resolved = false;
        for subtype in 0..=u8::MAX {
            if load_gool_program(&metadata, &nsf, &nsf_bytes, eid, u16::from(subtype)).is_ok() {
                resolved = true;
                program_count += 1;
            }
        }
        assert!(
            resolved,
            "mounted title executable {eid} has no valid subtype"
        );
    }
    assert!(executable_count > 0);
    assert!(program_count >= executable_count);
}
