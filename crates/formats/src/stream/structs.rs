//! Explicit decoded forms of representative entry-item structures.
//!
//! These types deliberately do not use `#[repr(C)]` transmutation. Parsing is
//! field-by-field so host alignment and pointer width can never alter the disk
//! contract.

use crate::binary::{Eid, FormatError, Reader};

/// Common six-word GOOL entry header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GoolHeader {
    pub object_type: u32,
    pub category: u32,
    pub unknown_08: u32,
    pub initial_stack_pointer: u32,
    pub subtype_map_index: u32,
    pub unknown_14: u32,
}

impl GoolHeader {
    pub const BYTE_LEN: usize = 24;

    /// Parses the fixed header from the start of an item.
    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let mut reader = Reader::new(bytes);
        Ok(Self {
            object_type: reader.u32_le()?,
            category: reader.u32_le()?,
            unknown_08: reader.u32_le()?,
            initial_stack_pointer: reader.u32_le()?,
            subtype_map_index: reader.u32_le()?,
            unknown_14: reader.u32_le()?,
        })
    }
}

/// Sixteen-byte GOOL state descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GoolState {
    pub flags: u32,
    pub status_c: u32,
    pub external_index: u16,
    pub event_pc: u16,
    pub transition_pc: u16,
    pub code_pc: u16,
}

impl GoolState {
    pub const BYTE_LEN: usize = 16;

    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let mut reader = Reader::new(bytes);
        Ok(Self {
            flags: reader.u32_le()?,
            status_c: reader.u32_le()?,
            external_index: reader.u16_le()?,
            event_pc: reader.u16_le()?,
            transition_pc: reader.u16_le()?,
            code_pc: reader.u16_le()?,
        })
    }
}

/// Packed four-byte primitive color descriptor.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ColorInfo(u32);

impl ColorInfo {
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn red(self) -> u8 {
        self.0 as u8
    }

    #[must_use]
    pub const fn green(self) -> u8 {
        (self.0 >> 8) as u8
    }

    #[must_use]
    pub const fn blue(self) -> u8 {
        (self.0 >> 16) as u8
    }

    #[must_use]
    pub const fn palette_x(self) -> u8 {
        ((self.0 >> 24) & 0x0f) as u8
    }

    #[must_use]
    pub const fn no_cull(self) -> bool {
        self.0 & (1 << 28) != 0
    }

    #[must_use]
    pub const fn semi_transparency(self) -> u8 {
        ((self.0 >> 29) & 3) as u8
    }

    #[must_use]
    pub const fn color_type(self) -> u8 {
        ((self.0 >> 31) & 1) as u8
    }
}

/// Packed four-byte texture-region descriptor.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RegionInfo(u32);

impl RegionInfo {
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn offset_y(self) -> u8 {
        (self.0 & 0x1f) as u8
    }

    #[must_use]
    pub const fn palette_y(self) -> u8 {
        ((self.0 >> 6) & 0x7f) as u8
    }

    #[must_use]
    pub const fn offset_x(self) -> u8 {
        ((self.0 >> 13) & 0x1f) as u8
    }

    #[must_use]
    pub const fn segment(self) -> u8 {
        ((self.0 >> 18) & 3) as u8
    }

    #[must_use]
    pub const fn color_mode(self) -> u8 {
        ((self.0 >> 20) & 3) as u8
    }

    #[must_use]
    pub const fn uv_index(self) -> u16 {
        ((self.0 >> 22) & 0x03ff) as u16
    }
}

/// Twelve-byte textured-geometry texture descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextureInfo {
    pub color: ColorInfo,
    pub texture_page: Eid,
    pub region: RegionInfo,
}

impl TextureInfo {
    pub const BYTE_LEN: usize = 12;

    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let mut reader = Reader::new(bytes);
        Ok(Self {
            color: ColorInfo::from_raw(reader.u32_le()?),
            texture_page: Eid::from_raw(reader.u32_le()?),
            region: RegionInfo::from_raw(reader.u32_le()?),
        })
    }
}

/// Eight-byte world/sprite texture descriptor without an embedded page EID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextureInfo2 {
    pub color: ColorInfo,
    pub region: RegionInfo,
}

impl TextureInfo2 {
    pub const BYTE_LEN: usize = 8;

    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let mut reader = Reader::new(bytes);
        Ok(Self {
            color: ColorInfo::from_raw(reader.u32_le()?),
            region: RegionInfo::from_raw(reader.u32_le()?),
        })
    }
}

/// One vertically stacked palette group in an MDAT header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClutLine {
    pub x: u32,
    pub y: u32,
    pub count: u32,
}

/// Fixed 560-byte title-state MDAT header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MdatHeader {
    pub width_tiles: i32,
    pub height_tiles: i32,
    /// Total CLUT count. Retail groups these into at most 46 IPAL entries with
    /// up to 120 palette items in each entry.
    pub palette_count: i32,
    pub entity_count: i32,
    pub unknown_4: i32,
    pub geometry_count: i32,
    pub clut_lines: [ClutLine; 8],
    pub palettes: [Eid; 46],
    pub geometries: [Eid; 32],
    pub images: [Eid; 32],
}

impl MdatHeader {
    pub const BYTE_LEN: usize = 560;

    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let mut reader = Reader::new(bytes);
        let width_tiles = reader.i32_le()?;
        let height_tiles = reader.i32_le()?;
        let palette_count = reader.i32_le()?;
        let entity_count = reader.i32_le()?;
        let unknown_4 = reader.i32_le()?;
        let geometry_count = reader.i32_le()?;
        if !(0..=46 * 120).contains(&palette_count) {
            return Err(FormatError::at(
                8,
                "MDAT CLUT count exceeds its 46 by 120-entry IPAL table",
            ));
        }
        if !(0..=32).contains(&geometry_count) {
            return Err(FormatError::at(
                20,
                "MDAT geometry count exceeds its 32-entry table",
            ));
        }
        if entity_count < 0 {
            return Err(FormatError::at(12, "MDAT entity count is negative"));
        }
        let mut clut_lines = [ClutLine {
            x: 0,
            y: 0,
            count: 0,
        }; 8];
        for line in &mut clut_lines {
            *line = ClutLine {
                x: reader.u32_le()?,
                y: reader.u32_le()?,
                count: reader.u32_le()?,
            };
        }
        let palettes = read_eid_array::<46>(&mut reader)?;
        let geometries = read_eid_array::<32>(&mut reader)?;
        let images = read_eid_array::<32>(&mut reader)?;
        Ok(Self {
            width_tiles,
            height_tiles,
            palette_count,
            entity_count,
            unknown_4,
            geometry_count,
            clut_lines,
            palettes,
            geometries,
            images,
        })
    }
}

/// Fixed MIDI entry header: one sequence and seven instrument EIDs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MidiHeader {
    pub track_count: i32,
    pub sequence: Eid,
    pub instruments: [Eid; 7],
}

impl MidiHeader {
    pub const BYTE_LEN: usize = 36;

    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let mut reader = Reader::new(bytes);
        let track_count = reader.i32_le()?;
        if !(0..=7).contains(&track_count) {
            return Err(FormatError::at(0, "MIDI track count is outside 0..=7"));
        }
        Ok(Self {
            track_count,
            sequence: Eid::from_raw(reader.u32_le()?),
            instruments: read_eid_array::<7>(&mut reader)?,
        })
    }
}

/// One eight-byte deterministic demo-input frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlaybackFrame {
    pub ticks_elapsed: i32,
    pub held_buttons: u32,
}

impl PlaybackFrame {
    pub const BYTE_LEN: usize = 8;

    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let mut reader = Reader::new(bytes);
        Ok(Self {
            ticks_elapsed: reader.i32_le()?,
            held_buttons: reader.u32_le()?,
        })
    }
}

/// Twelve-byte zone camera/path point.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ZonePathPoint {
    pub x: i16,
    pub y: i16,
    pub z: i16,
    pub rotation_y: i16,
    pub rotation_x: i16,
    pub rotation_z: i16,
}

impl ZonePathPoint {
    pub const BYTE_LEN: usize = 12;

    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let mut reader = Reader::new(bytes);
        Ok(Self {
            x: reader.i16_le()?,
            y: reader.i16_le()?,
            z: reader.i16_le()?,
            rotation_y: reader.i16_le()?,
            rotation_x: reader.i16_le()?,
            rotation_z: reader.i16_le()?,
        })
    }
}

/// Fixed portion of a zone entity before its variable path points.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ZoneEntityHeader {
    pub parent_zone: Eid,
    pub spawn_flags: u16,
    pub group: u16,
    pub id: u16,
    pub path_length: u16,
    pub initial_rotation: [i16; 3],
    pub object_type: u8,
    pub subtype: u8,
}

impl ZoneEntityHeader {
    pub const BYTE_LEN: usize = 20;

    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let mut reader = Reader::new(bytes);
        Ok(Self {
            parent_zone: Eid::from_raw(reader.u32_le()?),
            spawn_flags: reader.u16_le()?,
            group: reader.u16_le()?,
            id: reader.u16_le()?,
            path_length: reader.u16_le()?,
            initial_rotation: [reader.i16_le()?, reader.i16_le()?, reader.i16_le()?],
            object_type: reader.u8()?,
            subtype: reader.u8()?,
        })
    }
}

/// Fixed 64-byte WGEO header before its variable texture table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldGeometryHeader {
    pub translation: [i32; 3],
    pub polygon_count: u32,
    pub vertex_count: u32,
    pub texture_info_count: u32,
    pub texture_page_count: u32,
    pub is_backdrop: bool,
    pub texture_pages: [Eid; 8],
}

impl WorldGeometryHeader {
    pub const BYTE_LEN: usize = 64;

    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let mut reader = Reader::new(bytes);
        let translation = [reader.i32_le()?, reader.i32_le()?, reader.i32_le()?];
        let polygon_count = reader.u32_le()?;
        let vertex_count = reader.u32_le()?;
        let texture_info_count = reader.u32_le()?;
        let texture_page_count = reader.u32_le()?;
        let is_backdrop_raw = reader.u32_le()?;
        if texture_page_count > 8 {
            return Err(FormatError::at(
                24,
                "WGEO references more than eight texture pages",
            ));
        }
        if is_backdrop_raw > 1 {
            return Err(FormatError::at(28, "WGEO backdrop flag is not boolean"));
        }
        Ok(Self {
            translation,
            polygon_count,
            vertex_count,
            texture_info_count,
            texture_page_count,
            is_backdrop: is_backdrop_raw != 0,
            texture_pages: read_eid_array::<8>(&mut reader)?,
        })
    }
}

fn read_eid_array<const N: usize>(reader: &mut Reader<'_>) -> Result<[Eid; N], FormatError> {
    let mut result = [Eid::from_raw(0); N];
    for eid in &mut result {
        *eid = Eid::from_raw(reader.u32_le()?);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gool_state_offsets_are_exact() {
        let bytes = [1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 4, 0, 5, 0, 6, 0];
        assert_eq!(
            GoolState::parse(&bytes).unwrap(),
            GoolState {
                flags: 1,
                status_c: 2,
                external_index: 3,
                event_pc: 4,
                transition_pc: 5,
                code_pc: 6,
            }
        );
        assert!(GoolState::parse(&bytes[..15]).is_err());
    }

    #[test]
    fn texture_bitfields_match_the_disk_bit_order() {
        let color = ColorInfo::from_raw(0b1101_1010_u32 << 24 | 0x00_33_22_11);
        assert_eq!(
            (color.red(), color.green(), color.blue()),
            (0x11, 0x22, 0x33)
        );
        assert_eq!(color.palette_x(), 0x0a);
        assert!(color.no_cull());
        assert_eq!(color.semi_transparency(), 2);
        assert_eq!(color.color_type(), 1);

        let raw = 0x11 | (0x2a << 6) | (0x13 << 13) | (2 << 18) | (1 << 20) | (0x1ff << 22);
        let region = RegionInfo::from_raw(raw);
        assert_eq!(region.offset_y(), 17);
        assert_eq!(region.palette_y(), 42);
        assert_eq!(region.offset_x(), 19);
        assert_eq!(region.segment(), 2);
        assert_eq!(region.color_mode(), 1);
        assert_eq!(region.uv_index(), 511);
    }

    #[test]
    fn representative_struct_lengths_are_enforced() {
        assert!(GoolHeader::parse(&[0; GoolHeader::BYTE_LEN]).is_ok());
        assert!(MdatHeader::parse(&vec![0; MdatHeader::BYTE_LEN]).is_ok());
        assert!(MidiHeader::parse(&[0; MidiHeader::BYTE_LEN]).is_ok());
        assert!(ZoneEntityHeader::parse(&[0; ZoneEntityHeader::BYTE_LEN]).is_ok());
        assert!(WorldGeometryHeader::parse(&[0; WorldGeometryHeader::BYTE_LEN]).is_ok());
        assert!(MdatHeader::parse(&vec![0; MdatHeader::BYTE_LEN - 1]).is_err());
    }
}
