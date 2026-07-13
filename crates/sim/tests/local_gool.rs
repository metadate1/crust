//! Opt-in binding check against GOOL entries on a legally local disc.

use std::path::PathBuf;

use crust_formats::binary::Eid;
use crust_formats::disc::DiscImage;
use crust_formats::stream::{
    LevelId, NsdKind, StreamKind, StreamName, load_gool_program, parse_nsd, parse_nsf,
};
use crust_sim::gool::{ObjectHandle, VmObject};

#[test]
#[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
fn binds_every_mounted_title_executable_to_a_vm_object() {
    let disc_path = PathBuf::from(
        std::env::var_os("C1_DISC_IMAGE")
            .expect("C1_DISC_IMAGE must name a legally local NTSC-U raw BIN"),
    );
    let disc_bytes = std::fs::read(&disc_path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", disc_path.display()));
    let image = DiscImage::open(&disc_bytes).unwrap();
    let streams = image.discover_streams().unwrap();
    let nsd_bytes = image
        .read_stream(
            streams
                .get(StreamName::new(LevelId::TITLE, StreamKind::Nsd))
                .unwrap(),
        )
        .unwrap();
    let nsf_bytes = image
        .read_stream(
            streams
                .get(StreamName::new(LevelId::TITLE, StreamKind::Nsf))
                .unwrap(),
        )
        .unwrap();
    let metadata = parse_nsd(&nsd_bytes, LevelId::TITLE).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &metadata).unwrap();
    let NsdKind::Playable(ldat) = &metadata.kind else {
        panic!("title stream must have playable LDAT metadata");
    };

    let mut object_index = 0_u16;
    for eid in ldat.executable_map {
        if eid == Eid::NONE {
            continue;
        }
        let program = (0..=u8::MAX)
            .find_map(|subtype| {
                load_gool_program(&metadata, &nsf, &nsf_bytes, eid, u16::from(subtype)).ok()
            })
            .unwrap_or_else(|| panic!("mounted title executable {eid} has no valid subtype"));
        let handle = ObjectHandle::new(object_index).unwrap();
        VmObject::from_gool_program(handle, &program)
            .unwrap_or_else(|error| panic!("could not bind title executable {eid}: {error:?}"));
        object_index += 1;
    }
    assert!(object_index > 0);
}
