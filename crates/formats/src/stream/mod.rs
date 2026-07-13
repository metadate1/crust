//! C1 stream catalogs and bounds-checked NSD/NSF readers.

mod catalog;
mod gool;
mod gool_animation;
mod nsd;
mod nsf;
mod object;
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
pub use nsd::{LDAT_IMAGE_CAPACITY, LDAT_PREFIX_SIZE, Nsd, NsdHeader, NsdKind, NsdPte, parse_nsd};
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
pub use slst::{PolygonId, SlstCursor, SlstDelta, SlstDirection, SlstItem};
pub use wgeo::{WorldGeometry, WorldPolygon, WorldTexture, WorldVertex, parse_world_geometry};
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
