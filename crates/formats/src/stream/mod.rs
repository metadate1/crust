//! C1 stream catalogs and bounds-checked NSD/NSF readers.

mod catalog;
mod gool;
mod gool_animation;
mod mdat;
mod music;
mod nsd;
mod nsf;
mod object;
mod pbak;
mod slst;
pub mod structs;
mod wgeo;
mod zdat;
mod zone_graph;

pub use catalog::{KNOWN_LEVELS, KnownLevel, LevelId, StreamKind, StreamName, known_level};
pub use gool::{GOOL_PC_NONE, GoolProgram, load_gool_program, load_gool_state_program};
pub use gool_animation::{
    GOOL_ANIMATION_HEADER_LEN, GOOL_FONT_GLYPH_COUNT, GOOL_FRAGMENT_LEN, GOOL_GLYPH_LEN,
    GOOL_MAX_FONT_ANIMATION_LEN, GOOL_TEXTURE_INFO_LEN, GOOL_VERTEX_ANIMATION_LEN,
    GoolAnimationDescriptor, GoolAnimationHeader, GoolAnimationKind, GoolFontAnimation,
    GoolFragment, GoolFragmentAnimation, GoolGlyph, GoolSpriteAnimation, GoolTextAnimation,
    GoolTextureInfo, GoolVertexAnimation, parse_gool_animation_descriptor,
    parse_gool_animation_header,
};
pub use mdat::{MDAT_ENTRY_TYPE, TitleMdat, load_title_mdat, title_mdat_eid};
pub use music::{
    EID_NONE_RAW, INST_ENTRY_TYPE, InstrumentFragment, MIDI_ENTRY_TYPE, RetailMidiAsset, Sep,
    SepEvent, SepEventKind, SepSequence, VabBank, VabProgram, VabTone, parse_instrument_entry,
    parse_retail_midi,
};
pub use nsd::{
    LDAT_IMAGE_CAPACITY, LDAT_PREFIX_SIZE, NSF_PAGE_SECTOR_COUNT, Nsd, NsdHeader, NsdKind, NsdPte,
    NsfPageSectorCount, parse_nsd,
};
pub use nsf::{
    ENTRY_MAGIC, Entry, EntryItem, NSF_PAGE_SIZE, Nsf, NsfPage, Page, PageHeader, TexturePage,
    parse_nsf,
};
pub use object::{
    CVTX_ENTRY_TYPE, ColoredObjectVertex, LitObjectVertex, ObjectFrame, ObjectFrameHeader,
    ObjectGeometry, ObjectGeometryHeader, ObjectMaterial, ObjectModelFrame, ObjectPolygon,
    ObjectVertex, ObjectVertexKind, SVTX_ENTRY_TYPE, TGEO_ENTRY_TYPE, load_object_model_frame,
    parse_object_frame, parse_object_geometry,
};
pub use pbak::{
    PBAK_ENTRY_TYPE, PBAK_EXTENDED_FRAMES_OFFSET, PBAK_EXTENDED_LEVEL_STATE_LEN,
    PBAK_EXTENDED_SPAWN_WORD_COUNT, PBAK_FRAME_LEN, PBAK_FRAMES_OFFSET, PBAK_LEVEL_STATE_LEN,
    PBAK_SPAWN_WORD_COUNT, PbakBound, PbakFrame, PbakHeader, PbakLayout, PbakLevelState,
    load_pbak_entry, parse_pbak_header,
};
pub use slst::{PolygonId, SlstCursor, SlstDelta, SlstDirection, SlstItem};
pub use wgeo::{
    WORLD_MAP_PATH_GROUP_COUNT, WorldGeometry, WorldMapPathList, WorldMapPathMaskOverride,
    WorldMapPathRecord, WorldPolygon, WorldTexture, WorldVertex, parse_world_geometry,
};
pub use zdat::{
    ZoneColors, ZoneEntity, ZoneEntityPathPoint, ZoneGraphics, ZoneHeader, ZoneLoadList,
    ZoneNeighborPath, ZonePath, ZoneRect, ZoneWorld,
};
pub use zone_graph::{RetailPathId, RetailZoneGraph, RetailZoneNode};

#[cfg(test)]
mod local_tests {
    use super::*;
    use std::path::PathBuf;

    /// Opt-in characterization against streams extracted from the user's own disc.
    /// The test only reads `C1_STREAM_DIR`; it never copies data into the repository.
    #[test]
    #[ignore = "set C1_STREAM_DIR to a local legally extracted stream directory"]
    fn parses_all_local_retail_pairs_without_copying_them() {
        let root = PathBuf::from(
            std::env::var_os("C1_STREAM_DIR")
                .expect("C1_STREAM_DIR must name a local extracted stream directory"),
        );
        for level in KNOWN_LEVELS {
            let nsd_path = root.join(level.nsd_filename());
            let nsf_path = root.join(level.nsf_filename());
            let nsd_bytes = std::fs::read(&nsd_path)
                .unwrap_or_else(|error| panic!("could not read {}: {error}", nsd_path.display()));
            let metadata = parse_nsd(&nsd_bytes, level.id)
                .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
            assert_eq!(metadata.is_bootable(), level.bootable);
            let nsf_bytes = std::fs::read(&nsf_path)
                .unwrap_or_else(|error| panic!("could not read {}: {error}", nsf_path.display()));
            let pages = parse_nsf(&nsf_bytes, &metadata)
                .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));
            assert_eq!(pages.pages.len(), metadata.header.page_count as usize);
        }
    }
}
