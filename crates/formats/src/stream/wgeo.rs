//! Bounds-checked WGEO polygon, packed-vertex and texture-table decoding.

use crate::binary::{Eid, FormatError, Reader, checked_slice};

use super::structs::{ColorInfo, RegionInfo, WorldGeometryHeader};

/// Number of path-animation groups addressable by the island map's two
/// 32-bit GOOL globals.
pub const WORLD_MAP_PATH_GROUP_COUNT: u16 = 64;

/// One record from WGEO item three on the island map.
///
/// The serialized item reuses the 16-bit `poly_id` bit layout. A set flag
/// changes the active group; an unset flag assigns one polygon in this WGEO
/// to that group. The three `world_idx` bits are deliberately ignored here,
/// matching `GfxAnimMapPaths`, which reads only `flag` and `poly_idx`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorldMapPathRecord {
    Group(u16),
    Polygon(u16),
}

/// A validated effective animation-mask write for one WGEO polygon.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldMapPathMaskOverride {
    pub polygon_index: u16,
    pub animation_mask: u8,
}

/// Bounds-checked WGEO item-three map-path program.
///
/// Native casts this item to `poly_id_list`, then intentionally reads
/// `ids[ii - 1]`. Since `ids` begins after the two-word list header, record
/// zero is the serialized `type` halfword at byte two. Consequently the
/// exact wire layout is one little-endian `len` followed by `len` packed
/// 16-bit records, not a four-byte header followed by `len` records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldMapPathList {
    records: Vec<WorldMapPathRecord>,
}

impl WorldMapPathList {
    /// Parses one complete WGEO item-three path list.
    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let mut reader = Reader::new(bytes);
        let record_count = usize::from(reader.u16_le()?);
        let record_bytes = record_count
            .checked_mul(2)
            .ok_or_else(|| FormatError::at(0, "WGEO map-path record byte count overflows"))?;
        let expected_len = 2_usize
            .checked_add(record_bytes)
            .ok_or_else(|| FormatError::at(0, "WGEO map-path item length overflows"))?;
        require_exact_len(bytes, expected_len, "WGEO map-path item")?;

        let mut records = Vec::with_capacity(record_count);
        for _ in 0..record_count {
            let raw = reader.u16_le()?;
            let polygon_id = super::slst::PolygonId::from_raw(raw);
            records.push(if polygon_id.flag {
                WorldMapPathRecord::Group(polygon_id.polygon_index)
            } else {
                WorldMapPathRecord::Polygon(polygon_id.polygon_index)
            });
        }
        Ok(Self { records })
    }

    /// Ordered source records, including group changes.
    #[must_use]
    pub fn records(&self) -> &[WorldMapPathRecord] {
        &self.records
    }

    /// Resolves this world's non-mutating polygon-mask writes.
    ///
    /// `active_group` is shared by every world in ZDAT order because native
    /// initializes `poly_idx2` once outside its WGEO loop. Groups 0..31 read
    /// `map_level_links`; groups 32..63 read `map_key_links`. Enabled groups
    /// force mask seven and disabled groups force zero. Duplicate polygon
    /// records remain ordered so a caller can preserve native's last write.
    pub fn mask_overrides(
        &self,
        polygon_count: usize,
        active_group: &mut u16,
        map_level_links: u32,
        map_key_links: u32,
    ) -> Result<Vec<WorldMapPathMaskOverride>, FormatError> {
        let mut overrides = Vec::new();
        for (record_index, record) in self.records.iter().copied().enumerate() {
            let record_offset = 2 + record_index * 2;
            match record {
                WorldMapPathRecord::Group(group) => {
                    if group >= WORLD_MAP_PATH_GROUP_COUNT {
                        return Err(FormatError::at(
                            record_offset,
                            "WGEO map-path group is outside the 64 authored groups",
                        ));
                    }
                    *active_group = group;
                }
                WorldMapPathRecord::Polygon(polygon_index) => {
                    if usize::from(polygon_index) >= polygon_count {
                        return Err(FormatError::at(
                            record_offset,
                            "WGEO map-path record references a polygon outside item one",
                        ));
                    }
                    if *active_group >= WORLD_MAP_PATH_GROUP_COUNT {
                        return Err(FormatError::at(
                            record_offset,
                            "WGEO map-path active group is outside the 64 authored groups",
                        ));
                    }
                    let bit_index = u32::from(*active_group % 32);
                    let mask = 1_u32 << bit_index;
                    let enabled = if *active_group < 32 {
                        map_level_links & mask != 0
                    } else {
                        map_key_links & mask != 0
                    };
                    overrides.push(WorldMapPathMaskOverride {
                        polygon_index,
                        animation_mask: if enabled { 7 } else { 0 },
                    });
                }
            }
        }
        Ok(overrides)
    }
}

/// One eight-byte world polygon in its decoded field order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldPolygon {
    pub vertex_indices: [u16; 3],
    /// Index in 32-bit words, not in `TextureInfo2` records.
    pub texture_info_word_index: u16,
    pub texture_page_index: u8,
    pub animation_period: u8,
    pub animation_mask: u8,
    pub animation_phase: u8,
    pub reserved: bool,
}

impl WorldPolygon {
    pub const BYTE_LEN: usize = 8;

    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let mut reader = Reader::new(bytes);
        let low = reader.u32_le()?;
        let high = reader.u32_le()?;
        Ok(Self {
            // The source's BITFIELDS_09 macro reverses declarations for the
            // little-endian compiler layout. C/tinfo/page/phase occupy `low`.
            vertex_indices: [
                ((high >> 20) & 0x0fff) as u16,
                ((high >> 8) & 0x0fff) as u16,
                ((low >> 20) & 0x0fff) as u16,
            ],
            texture_info_word_index: ((low >> 8) & 0x0fff) as u16,
            texture_page_index: ((low >> 5) & 7) as u8,
            animation_period: ((high >> 5) & 7) as u8,
            animation_mask: ((high >> 1) & 0x0f) as u8,
            animation_phase: (low & 0x1f) as u8,
            reserved: high & 1 != 0,
        })
    }

    #[must_use]
    pub const fn animation_frame(self, counter: u32) -> usize {
        if self.animation_mask == 0 {
            0
        } else {
            let mask = ((self.animation_mask as u32) << 1) | 1;
            ((self.animation_phase as u32 + (counter >> self.animation_period)) & mask) as usize
        }
    }
}

/// One eight-byte packed world vertex.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldVertex {
    pub color: [u8; 3],
    pub x: i16,
    pub y: i16,
    pub z: i16,
    pub effect: bool,
}

impl WorldVertex {
    pub const BYTE_LEN: usize = 8;

    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let mut reader = Reader::new(bytes);
        let color = [reader.u8()?, reader.u8()?, reader.u8()?];
        let z_low = u16::from(reader.u8()?);
        let packed = reader.u32_le()?;
        let x = sign_extend_13((packed >> 3) & 0x1fff);
        let y = sign_extend_13((packed >> 19) & 0x1fff);
        let z_middle = (packed >> 1) & 3;
        let z_high = (packed >> 16) & 7;
        Ok(Self {
            color,
            x,
            y,
            z: sign_extend_13(u32::from(z_low) | (z_middle << 8) | (z_high << 10)),
            effect: packed & 1 != 0,
        })
    }

    /// Coordinates after the source renderer's fixed factor-of-eight expansion.
    #[must_use]
    pub const fn expanded_position(self) -> [i32; 3] {
        [self.x as i32 * 8, self.y as i32 * 8, self.z as i32 * 8]
    }
}

const fn sign_extend_13(value: u32) -> i16 {
    ((value << 19).cast_signed() >> 19) as i16
}

/// Texture descriptor selected for one polygon at a particular animation frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldTexture {
    pub color: ColorInfo,
    pub texture_page: Eid,
    pub region: RegionInfo,
}

/// All three WGEO items materialized with cross-item indices validated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldGeometry {
    pub header: WorldGeometryHeader,
    /// Raw 32-bit table because polygon indices address words and animated
    /// descriptors contain one color word followed by multiple region words.
    pub texture_words: Vec<u32>,
    pub polygons: Vec<WorldPolygon>,
    pub vertices: Vec<WorldVertex>,
}

impl WorldGeometry {
    /// Resolves the texture selected by a polygon. Untextured color records
    /// return `None`; every referenced word and texture-page index is checked.
    pub fn texture_for_polygon(
        &self,
        polygon: WorldPolygon,
        animation_counter: u32,
    ) -> Result<Option<WorldTexture>, FormatError> {
        let info_index = usize::from(polygon.texture_info_word_index);
        let color_word = *self.texture_words.get(info_index).ok_or_else(|| {
            FormatError::at(info_index * 4, "WGEO texture-info word is out of range")
        })?;
        let color = ColorInfo::from_raw(color_word);
        if color.color_type() == 0 {
            return Ok(None);
        }
        let page_index = usize::from(polygon.texture_page_index);
        if page_index >= self.header.texture_page_count as usize {
            return Err(FormatError::at(
                info_index * 4,
                "WGEO textured polygon references an inactive texture-page slot",
            ));
        }
        let frame = polygon.animation_frame(animation_counter);
        let region_index = info_index
            .checked_add(1)
            .and_then(|index| index.checked_add(frame))
            .ok_or_else(|| FormatError::at(info_index * 4, "WGEO region index overflows"))?;
        let region_word = *self.texture_words.get(region_index).ok_or_else(|| {
            FormatError::at(
                region_index * 4,
                "WGEO animated region word is out of range",
            )
        })?;
        Ok(Some(WorldTexture {
            color,
            texture_page: self.header.texture_pages[page_index],
            region: RegionInfo::from_raw(region_word),
        }))
    }
}

/// Parses WGEO items zero (header/table), one (polygons), and two (vertices).
pub fn parse_world_geometry(
    header_item: &[u8],
    polygon_item: &[u8],
    vertex_item: &[u8],
) -> Result<WorldGeometry, FormatError> {
    let header = WorldGeometryHeader::parse(header_item)?;
    let texture_word_count = usize::try_from(header.texture_info_count)
        .map_err(|_| FormatError::at(20, "WGEO texture word count does not fit the host"))?;
    let texture_byte_len = texture_word_count
        .checked_mul(4)
        .ok_or_else(|| FormatError::at(20, "WGEO texture table byte count overflows"))?;
    let expected_header_len = WorldGeometryHeader::BYTE_LEN
        .checked_add(texture_byte_len)
        .ok_or_else(|| FormatError::at(20, "WGEO item-zero byte count overflows"))?;
    require_exact_len(header_item, expected_header_len, "WGEO header/texture item")?;
    let texture_bytes = checked_slice(
        header_item,
        WorldGeometryHeader::BYTE_LEN,
        texture_byte_len,
        "WGEO texture table",
    )?;
    let mut texture_reader = Reader::new(texture_bytes);
    let mut texture_words = Vec::with_capacity(texture_word_count);
    for _ in 0..texture_word_count {
        texture_words.push(texture_reader.u32_le()?);
    }

    let polygon_count = usize::try_from(header.polygon_count)
        .map_err(|_| FormatError::at(12, "WGEO polygon count does not fit the host"))?;
    let polygon_byte_len = polygon_count
        .checked_mul(WorldPolygon::BYTE_LEN)
        .ok_or_else(|| FormatError::at(12, "WGEO polygon byte count overflows"))?;
    require_exact_len(polygon_item, polygon_byte_len, "WGEO polygon array")?;
    checked_slice(polygon_item, 0, polygon_byte_len, "WGEO polygon array")?;
    let mut polygons = Vec::with_capacity(polygon_count);
    for index in 0..polygon_count {
        let offset = index * WorldPolygon::BYTE_LEN;
        let bytes = checked_slice(polygon_item, offset, WorldPolygon::BYTE_LEN, "WGEO polygon")?;
        polygons.push(WorldPolygon::parse(bytes)?);
    }

    let vertex_count = usize::try_from(header.vertex_count)
        .map_err(|_| FormatError::at(16, "WGEO vertex count does not fit the host"))?;
    let vertex_byte_len = vertex_count
        .checked_mul(WorldVertex::BYTE_LEN)
        .ok_or_else(|| FormatError::at(16, "WGEO vertex byte count overflows"))?;
    require_exact_len(vertex_item, vertex_byte_len, "WGEO vertex array")?;
    checked_slice(vertex_item, 0, vertex_byte_len, "WGEO vertex array")?;
    let mut vertices = Vec::with_capacity(vertex_count);
    for index in 0..vertex_count {
        let offset = index * WorldVertex::BYTE_LEN;
        let bytes = checked_slice(vertex_item, offset, WorldVertex::BYTE_LEN, "WGEO vertex")?;
        vertices.push(WorldVertex::parse(bytes)?);
    }

    let geometry = WorldGeometry {
        header,
        texture_words,
        polygons,
        vertices,
    };
    validate_references(&geometry)?;
    Ok(geometry)
}

fn require_exact_len(bytes: &[u8], expected: usize, context: &str) -> Result<(), FormatError> {
    if bytes.len() != expected {
        return Err(FormatError::global(format!(
            "{context} is {} bytes; expected exactly {expected}",
            bytes.len()
        )));
    }
    Ok(())
}

fn validate_references(geometry: &WorldGeometry) -> Result<(), FormatError> {
    for (index, polygon) in geometry.polygons.iter().copied().enumerate() {
        for vertex in polygon.vertex_indices {
            if usize::from(vertex) >= geometry.vertices.len() {
                return Err(FormatError::at(
                    index * WorldPolygon::BYTE_LEN,
                    "WGEO polygon references a vertex outside item two",
                ));
            }
        }
        // Validate the largest animated region the source mask can select, not
        // only frame zero, so later animation cannot escape the table.
        let max_counter_frame = if polygon.animation_mask == 0 {
            0
        } else {
            usize::from((polygon.animation_mask << 1) | 1)
        };
        let info_index = usize::from(polygon.texture_info_word_index);
        let color_word = *geometry.texture_words.get(info_index).ok_or_else(|| {
            FormatError::at(
                index * WorldPolygon::BYTE_LEN,
                "WGEO polygon texture-info index is outside item zero",
            )
        })?;
        let color = ColorInfo::from_raw(color_word);
        if color.color_type() != 0 {
            if usize::from(polygon.texture_page_index)
                >= geometry.header.texture_page_count as usize
            {
                return Err(FormatError::at(
                    index * WorldPolygon::BYTE_LEN,
                    "WGEO textured polygon references an inactive texture-page slot",
                ));
            }
            let final_region = info_index
                .checked_add(1)
                .and_then(|value| value.checked_add(max_counter_frame))
                .ok_or_else(|| {
                    FormatError::at(
                        index * WorldPolygon::BYTE_LEN,
                        "WGEO animated texture range overflows",
                    )
                })?;
            if final_region >= geometry.texture_words.len() {
                return Err(FormatError::at(
                    index * WorldPolygon::BYTE_LEN,
                    "WGEO animated texture regions exceed item zero",
                ));
            }
            for region_index in info_index + 1..=final_region {
                let region = RegionInfo::from_raw(geometry.texture_words[region_index]);
                if region.color_mode() > 2 {
                    return Err(FormatError::at(
                        region_index * 4,
                        "WGEO texture region uses reserved color mode three",
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn polygon_bytes(polygon: WorldPolygon) -> [u8; WorldPolygon::BYTE_LEN] {
        let low = u32::from(polygon.animation_phase)
            | (u32::from(polygon.texture_page_index) << 5)
            | (u32::from(polygon.texture_info_word_index) << 8)
            | (u32::from(polygon.vertex_indices[2]) << 20);
        let high = u32::from(polygon.reserved)
            | (u32::from(polygon.animation_mask) << 1)
            | (u32::from(polygon.animation_period) << 5)
            | (u32::from(polygon.vertex_indices[1]) << 8)
            | (u32::from(polygon.vertex_indices[0]) << 20);
        let mut bytes = [0_u8; WorldPolygon::BYTE_LEN];
        put_u32(&mut bytes, 0, low);
        put_u32(&mut bytes, 4, high);
        bytes
    }

    fn vertex_bytes(vertex: WorldVertex) -> [u8; WorldVertex::BYTE_LEN] {
        let x = u32::from(vertex.x.cast_unsigned() & 0x1fff);
        let y = u32::from(vertex.y.cast_unsigned() & 0x1fff);
        let z = u32::from(vertex.z.cast_unsigned() & 0x1fff);
        let packed = u32::from(vertex.effect)
            | (((z >> 8) & 3) << 1)
            | (x << 3)
            | (((z >> 10) & 7) << 16)
            | (y << 19);
        let mut bytes = [
            vertex.color[0],
            vertex.color[1],
            vertex.color[2],
            z as u8,
            0,
            0,
            0,
            0,
        ];
        put_u32(&mut bytes, 4, packed);
        bytes
    }

    #[test]
    fn polygon_words_match_the_reversed_c_bitfield_layout() {
        let expected = WorldPolygon {
            vertex_indices: [0xabc, 0x789, 0x456],
            texture_info_word_index: 0x123,
            texture_page_index: 5,
            animation_period: 6,
            animation_mask: 9,
            animation_phase: 17,
            reserved: true,
        };
        assert_eq!(
            WorldPolygon::parse(&polygon_bytes(expected)).unwrap(),
            expected
        );
        assert_eq!(expected.animation_frame(0), 0x11 & 0x13);
    }

    #[test]
    fn map_path_item_includes_the_type_halfword_as_record_zero() {
        let mut bytes = vec![0_u8; 10];
        put_u16(&mut bytes, 0, 4);
        put_u16(&mut bytes, 2, 0x8002); // group 2 in the C `type` field
        put_u16(&mut bytes, 4, 0x5003); // ignored world bits, polygon 3
        put_u16(&mut bytes, 6, 0x8021); // group 33
        put_u16(&mut bytes, 8, 4); // polygon 4

        let list = WorldMapPathList::parse(&bytes).unwrap();
        assert_eq!(
            list.records(),
            [
                WorldMapPathRecord::Group(2),
                WorldMapPathRecord::Polygon(3),
                WorldMapPathRecord::Group(33),
                WorldMapPathRecord::Polygon(4),
            ]
        );
        let mut active_group = 0;
        assert_eq!(
            list.mask_overrides(5, &mut active_group, 1 << 2, 1 << 1)
                .unwrap(),
            [
                WorldMapPathMaskOverride {
                    polygon_index: 3,
                    animation_mask: 7,
                },
                WorldMapPathMaskOverride {
                    polygon_index: 4,
                    animation_mask: 7,
                },
            ]
        );
        assert_eq!(active_group, 33);

        let mut active_group = 0;
        assert_eq!(
            list.mask_overrides(5, &mut active_group, 0, 0).unwrap(),
            [
                WorldMapPathMaskOverride {
                    polygon_index: 3,
                    animation_mask: 0,
                },
                WorldMapPathMaskOverride {
                    polygon_index: 4,
                    animation_mask: 0,
                },
            ]
        );
    }

    #[test]
    fn map_path_group_state_carries_between_world_items() {
        let first = WorldMapPathList::parse(&[1, 0, 0x25, 0x80]).unwrap();
        let second = WorldMapPathList::parse(&[1, 0, 3, 0]).unwrap();
        let mut active_group = 0;
        assert!(
            first
                .mask_overrides(4, &mut active_group, 0, 1 << 5)
                .unwrap()
                .is_empty()
        );
        assert_eq!(active_group, 37);
        assert_eq!(
            second
                .mask_overrides(4, &mut active_group, 0, 1 << 5)
                .unwrap(),
            [WorldMapPathMaskOverride {
                polygon_index: 3,
                animation_mask: 7,
            }]
        );
    }

    #[test]
    fn map_path_parser_rejects_bad_lengths_groups_and_polygon_indices() {
        assert!(WorldMapPathList::parse(&[]).is_err());
        assert!(WorldMapPathList::parse(&[1, 0]).is_err());
        assert!(WorldMapPathList::parse(&[0, 0, 0, 0]).is_err());

        let group_64 = WorldMapPathList::parse(&[1, 0, 0x40, 0x80]).unwrap();
        assert!(group_64.mask_overrides(1, &mut 0, 0, 0).is_err());
        let polygon_4 = WorldMapPathList::parse(&[1, 0, 4, 0]).unwrap();
        assert!(polygon_4.mask_overrides(4, &mut 0, 0, 0).is_err());
    }

    #[test]
    fn packed_vertex_sign_extends_all_three_coordinates() {
        let bytes = [0x11, 0x22, 0x33, 0xff, 0xff, 0xff, 0x07, 0x80];
        let expected = WorldVertex {
            color: [0x11, 0x22, 0x33],
            x: -1,
            y: -4096,
            z: -1,
            effect: true,
        };
        let parsed = WorldVertex::parse(&bytes).unwrap();
        assert_eq!(parsed, expected);
        assert_eq!(vertex_bytes(expected), bytes);
        assert_eq!(parsed.expanded_position(), [-8, -32768, -8]);
    }

    #[test]
    fn complete_geometry_checks_cross_item_references() {
        let mut header = vec![0_u8; WorldGeometryHeader::BYTE_LEN + 8];
        put_u32(&mut header, 12, 1);
        put_u32(&mut header, 16, 3);
        put_u32(&mut header, 20, 2);
        put_u32(&mut header, 24, 1);
        put_u32(&mut header, 28, 0);
        put_u32(&mut header, 32, 0x1234_5679);
        put_u32(&mut header, 64, 0x8000_00ff);
        put_u32(&mut header, 68, 0x5566_7788);
        let polygon = WorldPolygon {
            vertex_indices: [0, 1, 2],
            texture_info_word_index: 0,
            texture_page_index: 0,
            animation_period: 0,
            animation_mask: 0,
            animation_phase: 0,
            reserved: false,
        };
        let polygon_item = polygon_bytes(polygon);
        let mut vertex_item = Vec::new();
        for x in [-1_i16, 0, 1] {
            vertex_item.extend_from_slice(&vertex_bytes(WorldVertex {
                color: [1, 2, 3],
                x,
                y: 2,
                z: 3,
                effect: false,
            }));
        }

        let geometry = parse_world_geometry(&header, &polygon_item, &vertex_item).unwrap();
        assert_eq!(geometry.vertices.len(), 3);
        let texture = geometry.texture_for_polygon(polygon, 0).unwrap().unwrap();
        assert_eq!(texture.texture_page, Eid::from_raw(0x1234_5679));
        assert_eq!(texture.region, RegionInfo::from_raw(0x5566_7788));

        let bad_polygon = WorldPolygon {
            vertex_indices: [0, 1, 3],
            ..polygon
        };
        assert!(parse_world_geometry(&header, &polygon_bytes(bad_polygon), &vertex_item).is_err());
        assert!(parse_world_geometry(&header, &polygon_item[..7], &vertex_item).is_err());
    }

    proptest! {
        #[test]
        fn packed_polygon_decoder_is_total(low in any::<u32>(), high in any::<u32>()) {
            let mut bytes = [0_u8; WorldPolygon::BYTE_LEN];
            put_u32(&mut bytes, 0, low);
            put_u32(&mut bytes, 4, high);
            prop_assert!(WorldPolygon::parse(&bytes).is_ok());
        }

        #[test]
        fn malformed_wgeo_never_panics(
            header in proptest::collection::vec(any::<u8>(), 0..256),
            polygons in proptest::collection::vec(any::<u8>(), 0..512),
            vertices in proptest::collection::vec(any::<u8>(), 0..512),
        ) {
            let _ = parse_world_geometry(&header, &polygons, &vertices);
        }

        #[test]
        fn malformed_map_path_list_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            if let Ok(list) = WorldMapPathList::parse(&bytes) {
                let _ = list.mask_overrides(4_096, &mut 0, u32::MAX, u32::MAX);
            }
        }
    }
}
