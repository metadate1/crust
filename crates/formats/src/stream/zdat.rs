//! Bounds-checked readers for retail ZDAT zone metadata.

use crate::binary::{Eid, EntryRef, FormatError, PageIndex, PageRef, Reader, checked_slice};

use super::structs::ZonePathPoint;

const ZONE_WORLD_CAPACITY: usize = 8;
const ZONE_WORLD_BYTE_LEN: usize = 64;
const ZONE_NEIGHBOR_CAPACITY: usize = 8;
const LOAD_LIST_ENTRY_CAPACITY: usize = 8;
const LOAD_LIST_PAGE_CAPACITY: usize = 32;
const ZONE_PATH_NEIGHBOR_CAPACITY: usize = 4;
const GOOL_EXECUTABLE_COUNT: u8 = 64;
const GOOL_SPAWN_COUNT: u16 = 304;

/// One serialized world slot from a ZDAT header.
///
/// Only the first word is persistent input. The remaining 60 bytes are runtime
/// scratch fields that the C engine overwrites with transforms and pointers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ZoneWorld {
    pub geometry: Eid,
}

/// Fixed header of ZDAT item one, including its serialized octree root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ZoneRect {
    pub origin: [i32; 3],
    pub dimensions: [u32; 3],
    pub unknown: u32,
    /// Zero for an empty tree, odd for an inline leaf tag, or an even byte
    /// offset from the beginning of item one to a child table.
    pub octree_root: u16,
    pub octree_max_depth: [u16; 3],
}

impl ZoneRect {
    pub const BYTE_LEN: usize = 36;

    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let header = checked_slice(bytes, 0, Self::BYTE_LEN, "ZDAT zone rectangle")?;
        let mut reader = Reader::new(header);
        let origin = [reader.i32_le()?, reader.i32_le()?, reader.i32_le()?];
        let dimensions = [reader.u32_le()?, reader.u32_le()?, reader.u32_le()?];
        let unknown = reader.u32_le()?;
        let octree_root = reader.u16_le()?;
        let octree_max_depth = [reader.u16_le()?, reader.u16_le()?, reader.u16_le()?];
        for (axis, depth) in octree_max_depth.iter().copied().enumerate() {
            if depth > 31 {
                return Err(FormatError::at(
                    30 + axis * 2,
                    "ZDAT octree depth exceeds the 32-bit coordinate width",
                ));
            }
        }
        if octree_root != 0 && octree_root & 1 == 0 {
            let offset = usize::from(octree_root);
            if offset < Self::BYTE_LEN {
                return Err(FormatError::at(
                    28,
                    "ZDAT octree root points into the fixed rectangle header",
                ));
            }
            let active_axes = octree_max_depth.iter().filter(|depth| **depth != 0).count();
            let child_count = 1_usize << active_axes;
            let child_bytes = child_count
                .checked_mul(2)
                .ok_or_else(|| FormatError::at(28, "ZDAT octree root table overflows"))?;
            checked_slice(bytes, offset, child_bytes, "ZDAT octree root child table")?;
        }
        Ok(Self {
            origin,
            dimensions,
            unknown,
            octree_root,
            octree_max_depth,
        })
    }
}

/// The fixed-capacity entry/page load list embedded in every zone header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZoneLoadList {
    pub entries: Vec<Eid>,
    pub pages: Vec<PageIndex>,
}

impl ZoneLoadList {
    pub const BYTE_LEN: usize = 0xa8;

    /// Parses the exact 32-bit `ns_loadlist` disk representation.
    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        Self::parse_at(bytes, 0)
    }

    fn parse_at(bytes: &[u8], base_offset: usize) -> Result<Self, FormatError> {
        let bytes = checked_slice(bytes, 0, Self::BYTE_LEN, "ZDAT load list")?;
        let mut reader = Reader::new(bytes);
        let entry_count = reader.i32_le()?;
        let page_count = reader.i32_le()?;
        if !(0..=8).contains(&entry_count) {
            return Err(FormatError::at(
                base_offset,
                "ZDAT load-list entry count is outside 0..=8",
            ));
        }
        if !(0..=32).contains(&page_count) {
            return Err(FormatError::at(
                base_offset + 4,
                "ZDAT load-list page count is outside 0..=32",
            ));
        }

        let mut all_entries = Vec::with_capacity(LOAD_LIST_ENTRY_CAPACITY);
        for _ in 0..LOAD_LIST_ENTRY_CAPACITY {
            all_entries.push(Eid::from_raw(reader.u32_le()?));
        }

        let active_page_count =
            usize::try_from(page_count).expect("validated page count fits usize");
        let mut pages = Vec::with_capacity(active_page_count);
        for index in 0..LOAD_LIST_PAGE_CAPACITY {
            let raw = reader.u32_le()?;
            if index < active_page_count {
                let PageRef::Page(page) = PageRef::from_raw(raw) else {
                    return Err(FormatError::at(
                        base_offset + 40 + index * 4,
                        "ZDAT load-list page does not carry the odd page-id tag",
                    ));
                };
                pages.push(page);
            }
        }

        all_entries
            .truncate(usize::try_from(entry_count).expect("validated entry count fits usize"));
        Ok(Self {
            entries: all_entries,
            pages,
        })
    }
}

/// Lighting/color matrices retained as 24 endian-explicit 16-bit words.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ZoneColors {
    pub words: [u16; 24],
}

impl ZoneColors {
    const BYTE_LEN: usize = 48;

    fn parse(reader: &mut Reader<'_>) -> Result<Self, FormatError> {
        let mut words = [0_u16; 24];
        for word in &mut words {
            *word = reader.u16_le()?;
        }
        Ok(Self { words })
    }
}

/// Render, fog, music and lighting parameters embedded at ZDAT offset `0x2e0`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ZoneGraphics {
    pub vram_fill_height: u32,
    pub unknown_a: u32,
    pub visibility_depth: u32,
    pub unknown_b_to_e: [u32; 4],
    pub flags: u32,
    pub water_y: i32,
    pub midi: Eid,
    pub unknown_g: u32,
    pub transition_color: [u8; 3],
    pub vram_fill: [u8; 3],
    pub far_color: [u8; 3],
    pub object_colors: ZoneColors,
    pub player_colors: ZoneColors,
}

impl ZoneGraphics {
    pub const BYTE_LEN: usize = 0x98;

    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let bytes = checked_slice(bytes, 0, Self::BYTE_LEN, "ZDAT graphics")?;
        let mut reader = Reader::new(bytes);
        let vram_fill_height = reader.u32_le()?;
        let unknown_a = reader.u32_le()?;
        let visibility_depth = reader.u32_le()?;
        let unknown_b_to_e = [
            reader.u32_le()?,
            reader.u32_le()?,
            reader.u32_le()?,
            reader.u32_le()?,
        ];
        let flags = reader.u32_le()?;
        let water_y = reader.i32_le()?;
        let midi = Eid::from_raw(reader.u32_le()?);
        let unknown_g = reader.u32_le()?;
        let transition_color = read_padded_rgb(&mut reader)?;
        let vram_fill = read_padded_rgb(&mut reader)?;
        let far_color = read_padded_rgb(&mut reader)?;
        let object_colors = ZoneColors::parse(&mut reader)?;
        let player_colors = ZoneColors::parse(&mut reader)?;
        debug_assert_eq!(reader.position(), Self::BYTE_LEN);
        debug_assert_eq!(ZoneColors::BYTE_LEN, 48);
        Ok(Self {
            vram_fill_height,
            unknown_a,
            visibility_depth,
            unknown_b_to_e,
            flags,
            water_y,
            midi,
            unknown_g,
            transition_color,
            vram_fill,
            far_color,
            object_colors,
            player_colors,
        })
    }
}

fn read_padded_rgb(reader: &mut Reader<'_>) -> Result<[u8; 3], FormatError> {
    let result = [reader.u8()?, reader.u8()?, reader.u8()?];
    let _padding = reader.u8()?;
    Ok(result)
}

/// One of the four possible links from a zone camera path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ZoneNeighborPath {
    pub relation: u8,
    pub neighbor_zone_index: u8,
    pub path_index: u8,
    pub goal: u8,
}

/// One signed point from a ZDAT entity's movement path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ZoneEntityPathPoint {
    pub x: i16,
    pub y: i16,
    pub z: i16,
}

impl ZoneEntityPathPoint {
    pub const BYTE_LEN: usize = 6;

    fn parse(reader: &mut Reader<'_>) -> Result<Self, FormatError> {
        Ok(Self {
            x: reader.i16_le()?,
            y: reader.i16_le()?,
            z: reader.i16_le()?,
        })
    }
}

/// One pointer-free ZDAT entity descriptor consumed by GOOL spawning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZoneEntity {
    /// Serialized placeholder overwritten by `ZdatOnLoad` in the C runtime.
    pub serialized_parent: EntryRef,
    pub spawn_flags: u16,
    pub group: u16,
    pub id: u16,
    /// The same three words are interpreted as rotation or mode flags
    /// according to `spawn_flags & 1`; preserve their exact signed values.
    pub initializer: [i16; 3],
    /// Index into the LDAT 64-entry executable map.
    pub executable: u8,
    pub subtype: u8,
    pub path_points: Vec<ZoneEntityPathPoint>,
}

impl ZoneEntity {
    pub const HEADER_BYTE_LEN: usize = 20;

    /// Parses the exact 32-bit `zone_entity` disk representation.
    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let header = checked_slice(bytes, 0, Self::HEADER_BYTE_LEN, "ZDAT entity header")?;
        let mut reader = Reader::new(header);
        let serialized_parent = EntryRef::from_raw(reader.u32_le()?);
        let spawn_flags = reader.u16_le()?;
        let group = reader.u16_le()?;
        let id = reader.u16_le()?;
        if id >= GOOL_SPAWN_COUNT {
            return Err(FormatError::at(
                8,
                "ZDAT entity id exceeds the retail spawn table",
            ));
        }
        let path_length = reader.u16_le()?;
        if path_length == 0 {
            return Err(FormatError::at(10, "ZDAT entity path contains no points"));
        }
        let initializer = [reader.i16_le()?, reader.i16_le()?, reader.i16_le()?];
        let executable = reader.u8()?;
        if executable >= GOOL_EXECUTABLE_COUNT {
            return Err(FormatError::at(
                18,
                "ZDAT entity executable is outside the LDAT map",
            ));
        }
        let subtype = reader.u8()?;
        debug_assert_eq!(reader.position(), Self::HEADER_BYTE_LEN);

        let points_bytes = usize::from(path_length)
            .checked_mul(ZoneEntityPathPoint::BYTE_LEN)
            .ok_or_else(|| FormatError::at(10, "ZDAT entity path byte count overflows"))?;
        let required_len = Self::HEADER_BYTE_LEN
            .checked_add(points_bytes)
            .ok_or_else(|| FormatError::at(10, "ZDAT entity byte count overflows"))?;
        let points = checked_slice(
            bytes,
            Self::HEADER_BYTE_LEN,
            points_bytes,
            "ZDAT entity path points",
        )?;
        debug_assert_eq!(required_len, Self::HEADER_BYTE_LEN + points.len());
        let mut reader = Reader::new(points);
        let mut path_points = Vec::with_capacity(usize::from(path_length));
        for _ in 0..path_length {
            path_points.push(ZoneEntityPathPoint::parse(&mut reader)?);
        }

        Ok(Self {
            serialized_parent,
            spawn_flags,
            group,
            id,
            initializer,
            executable,
            subtype,
            path_points,
        })
    }
}

/// A complete variable-length ZDAT camera path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZonePath {
    pub visibility_list: Eid,
    /// Serialized placeholder overwritten by `ZdatOnLoad` in the C runtime.
    pub serialized_parent: EntryRef,
    pub neighbors: Vec<ZoneNeighborPath>,
    pub entrance_index: u8,
    pub exit_index: u8,
    pub camera_mode: u16,
    pub average_node_distance: i16,
    pub camera_zoom: i16,
    pub unknown: [u16; 3],
    pub direction: [i16; 3],
    pub points: Vec<ZonePathPoint>,
}

impl ZonePath {
    pub const HEADER_BYTE_LEN: usize = 50;

    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let header = checked_slice(bytes, 0, Self::HEADER_BYTE_LEN, "ZDAT path header")?;
        let mut reader = Reader::new(header);
        let visibility_list = Eid::from_raw(reader.u32_le()?);
        let serialized_parent = EntryRef::from_raw(reader.u32_le()?);
        let neighbor_count = reader.u32_le()?;
        if neighbor_count > ZONE_PATH_NEIGHBOR_CAPACITY as u32 {
            return Err(FormatError::at(
                8,
                "ZDAT path has more than four neighbor links",
            ));
        }
        let mut all_neighbors = Vec::with_capacity(ZONE_PATH_NEIGHBOR_CAPACITY);
        for _ in 0..ZONE_PATH_NEIGHBOR_CAPACITY {
            all_neighbors.push(ZoneNeighborPath {
                relation: reader.u8()?,
                neighbor_zone_index: reader.u8()?,
                path_index: reader.u8()?,
                goal: reader.u8()?,
            });
        }
        let entrance_index = reader.u8()?;
        let exit_index = reader.u8()?;
        let point_count = reader.u16_le()?;
        if point_count == 0 {
            return Err(FormatError::at(30, "ZDAT path contains no camera points"));
        }
        let camera_mode = reader.u16_le()?;
        let average_node_distance = reader.i16_le()?;
        let camera_zoom = reader.i16_le()?;
        let unknown = [reader.u16_le()?, reader.u16_le()?, reader.u16_le()?];
        let direction = [reader.i16_le()?, reader.i16_le()?, reader.i16_le()?];
        debug_assert_eq!(reader.position(), Self::HEADER_BYTE_LEN);

        let points_bytes = usize::from(point_count)
            .checked_mul(ZonePathPoint::BYTE_LEN)
            .ok_or_else(|| FormatError::at(30, "ZDAT path point byte count overflows"))?;
        let required_len = Self::HEADER_BYTE_LEN
            .checked_add(points_bytes)
            .ok_or_else(|| FormatError::at(30, "ZDAT path byte count overflows"))?;
        checked_slice(bytes, 0, required_len, "ZDAT path points")?;
        let mut points = Vec::with_capacity(usize::from(point_count));
        for index in 0..usize::from(point_count) {
            let offset = Self::HEADER_BYTE_LEN + index * ZonePathPoint::BYTE_LEN;
            let point = checked_slice(bytes, offset, ZonePathPoint::BYTE_LEN, "ZDAT path point")?;
            points.push(ZonePathPoint::parse(point)?);
        }
        all_neighbors.truncate(neighbor_count as usize);

        Ok(Self {
            visibility_list,
            serialized_parent,
            neighbors: all_neighbors,
            entrance_index,
            exit_index,
            camera_mode,
            average_node_distance,
            camera_zoom,
            unknown,
            direction,
            points,
        })
    }
}

/// Fixed ZDAT item-zero metadata needed to resolve a zone's scene and paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZoneHeader {
    pub worlds: Vec<ZoneWorld>,
    pub paths_item_index: u32,
    pub path_count: u32,
    pub entity_count: u32,
    pub neighbors: Vec<Eid>,
    pub load_list: ZoneLoadList,
    pub display_flags: u32,
    pub graphics: ZoneGraphics,
}

impl ZoneHeader {
    pub const BYTE_LEN: usize = 0x378;
    pub const LOAD_LIST_OFFSET: usize = 0x234;
    pub const GRAPHICS_OFFSET: usize = 0x2e0;

    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let bytes = checked_slice(bytes, 0, Self::BYTE_LEN, "ZDAT zone header")?;
        let mut reader = Reader::new(bytes);
        let world_count = reader.u32_le()?;
        if world_count > ZONE_WORLD_CAPACITY as u32 {
            return Err(FormatError::at(0, "ZDAT zone has more than eight worlds"));
        }
        let mut worlds = Vec::with_capacity(world_count as usize);
        for index in 0..ZONE_WORLD_CAPACITY {
            let offset = reader.position();
            let reference = EntryRef::from_raw(reader.u32_le()?);
            if index < world_count as usize {
                let EntryRef::Eid(geometry) = reference else {
                    return Err(FormatError::at(
                        offset,
                        "active ZDAT world does not contain an EID",
                    ));
                };
                worlds.push(ZoneWorld { geometry });
            }
            reader.take(ZONE_WORLD_BYTE_LEN - 4)?;
        }
        debug_assert_eq!(reader.position(), 0x204);

        let paths_item_index = reader.u32_le()?;
        let path_count = reader.u32_le()?;
        let entity_count = reader.u32_le()?;
        let paths_end = paths_item_index
            .checked_add(path_count)
            .ok_or_else(|| FormatError::at(0x204, "ZDAT path item range overflows"))?;
        paths_end
            .checked_add(entity_count)
            .ok_or_else(|| FormatError::at(0x20c, "ZDAT entity item range overflows"))?;

        let neighbor_count = reader.u32_le()?;
        if neighbor_count > ZONE_NEIGHBOR_CAPACITY as u32 {
            return Err(FormatError::at(
                0x210,
                "ZDAT zone has more than eight neighbors",
            ));
        }
        let mut neighbors = Vec::with_capacity(ZONE_NEIGHBOR_CAPACITY);
        for _ in 0..ZONE_NEIGHBOR_CAPACITY {
            neighbors.push(Eid::from_raw(reader.u32_le()?));
        }
        neighbors.truncate(neighbor_count as usize);
        debug_assert_eq!(reader.position(), Self::LOAD_LIST_OFFSET);

        let load_list_bytes = reader.take(ZoneLoadList::BYTE_LEN)?;
        let load_list = ZoneLoadList::parse_at(load_list_bytes, Self::LOAD_LIST_OFFSET)?;
        let display_flags = reader.u32_le()?;
        debug_assert_eq!(reader.position(), Self::GRAPHICS_OFFSET);
        let graphics = ZoneGraphics::parse(reader.take(ZoneGraphics::BYTE_LEN)?)?;
        debug_assert_eq!(reader.position(), Self::BYTE_LEN);

        Ok(Self {
            worlds,
            paths_item_index,
            path_count,
            entity_count,
            neighbors,
            load_list,
            display_flags,
            graphics,
        })
    }

    #[must_use]
    pub fn path_item_index(&self, path_index: u32) -> Option<u32> {
        (path_index < self.path_count).then(|| self.paths_item_index + path_index)
    }

    #[must_use]
    pub fn entity_item_index(&self, entity_index: u32) -> Option<u32> {
        (entity_index < self.entity_count)
            .then(|| self.paths_item_index + self.path_count + entity_index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_i16(bytes: &mut [u8], offset: usize, value: i16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn load_list_enforces_capacities_and_page_tags() {
        let mut bytes = [0_u8; ZoneLoadList::BYTE_LEN];
        put_u32(&mut bytes, 0, 2);
        put_u32(&mut bytes, 4, 2);
        put_u32(&mut bytes, 8, 0x1111_1111);
        put_u32(&mut bytes, 12, 0x2222_2223);
        put_u32(&mut bytes, 40, PageIndex::new(3).tagged());
        put_u32(&mut bytes, 44, PageIndex::new(7).tagged());
        let parsed = ZoneLoadList::parse(&bytes).unwrap();
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(parsed.pages, [PageIndex::new(3), PageIndex::new(7)]);

        put_u32(&mut bytes, 40, 6);
        assert_eq!(ZoneLoadList::parse(&bytes).unwrap_err().offset(), Some(40));
        put_u32(&mut bytes, 0, 9);
        assert!(ZoneLoadList::parse(&bytes).is_err());
    }

    #[test]
    fn path_offsets_and_points_match_the_32_bit_layout() {
        let mut bytes = vec![0_u8; ZonePath::HEADER_BYTE_LEN + 2 * ZonePathPoint::BYTE_LEN];
        put_u32(&mut bytes, 0, 0x1234_5679);
        put_u32(&mut bytes, 4, 0);
        put_u32(&mut bytes, 8, 1);
        bytes[12..16].copy_from_slice(&[2, 3, 4, 5]);
        bytes[28] = 6;
        bytes[29] = 7;
        put_u16(&mut bytes, 30, 2);
        put_u16(&mut bytes, 32, 5);
        put_i16(&mut bytes, 34, -11);
        put_i16(&mut bytes, 36, 12);
        put_i16(&mut bytes, 44, -1);
        put_i16(&mut bytes, 46, 2);
        put_i16(&mut bytes, 48, -3);
        for (index, value) in [-10_i16, 20, -30, 40, -50, 60].into_iter().enumerate() {
            put_i16(&mut bytes, 50 + index * 2, value);
        }
        for (index, value) in [70_i16, -80, 90, -100, 110, -120].into_iter().enumerate() {
            put_i16(&mut bytes, 62 + index * 2, value);
        }

        let path = ZonePath::parse(&bytes).unwrap();
        assert_eq!(path.visibility_list, Eid::from_raw(0x1234_5679));
        assert_eq!(path.neighbors[0].neighbor_zone_index, 3);
        assert_eq!(path.camera_mode, 5);
        assert_eq!(path.average_node_distance, -11);
        assert_eq!(path.direction, [-1, 2, -3]);
        assert_eq!(path.points[0].x, -10);
        assert_eq!(path.points[1].rotation_z, -120);
        assert!(ZonePath::parse(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn entity_layout_preserves_spawn_and_gool_fields() {
        let mut bytes = vec![0_u8; ZoneEntity::HEADER_BYTE_LEN + 2 * ZoneEntityPathPoint::BYTE_LEN];
        put_u32(&mut bytes, 0, 0x1234_5679);
        put_u16(&mut bytes, 4, 1);
        put_u16(&mut bytes, 6, 3);
        put_u16(&mut bytes, 8, 42);
        put_u16(&mut bytes, 10, 2);
        for (index, value) in [-10_i16, 20, -30].into_iter().enumerate() {
            put_i16(&mut bytes, 12 + index * 2, value);
        }
        bytes[18] = 7;
        bytes[19] = 9;
        for (index, value) in [-100_i16, 200, -300, 400, -500, 600]
            .into_iter()
            .enumerate()
        {
            put_i16(&mut bytes, 20 + index * 2, value);
        }

        let entity = ZoneEntity::parse(&bytes).unwrap();
        assert_eq!(entity.serialized_parent, EntryRef::from_raw(0x1234_5679));
        assert_eq!(entity.spawn_flags, 1);
        assert_eq!(entity.group, 3);
        assert_eq!(entity.id, 42);
        assert_eq!(entity.initializer, [-10, 20, -30]);
        assert_eq!(entity.executable, 7);
        assert_eq!(entity.subtype, 9);
        assert_eq!(
            entity.path_points,
            [
                ZoneEntityPathPoint {
                    x: -100,
                    y: 200,
                    z: -300,
                },
                ZoneEntityPathPoint {
                    x: 400,
                    y: -500,
                    z: 600,
                },
            ]
        );
        assert!(ZoneEntity::parse(&bytes[..bytes.len() - 1]).is_err());

        put_u16(&mut bytes, 10, 0);
        assert!(ZoneEntity::parse(&bytes).is_err());
        put_u16(&mut bytes, 10, 2);
        put_u16(&mut bytes, 8, GOOL_SPAWN_COUNT);
        assert!(ZoneEntity::parse(&bytes).is_err());
        put_u16(&mut bytes, 8, 42);
        bytes[18] = GOOL_EXECUTABLE_COUNT;
        assert!(ZoneEntity::parse(&bytes).is_err());
    }

    #[test]
    fn zone_rectangle_validates_octree_root_and_depths() {
        let mut bytes = vec![0_u8; ZoneRect::BYTE_LEN + 16];
        put_u32(&mut bytes, 0, (-100_i32).cast_unsigned());
        put_u32(&mut bytes, 4, 200);
        put_u32(&mut bytes, 8, (-300_i32).cast_unsigned());
        put_u32(&mut bytes, 12, 40);
        put_u32(&mut bytes, 16, 50);
        put_u32(&mut bytes, 20, 60);
        put_u16(&mut bytes, 28, ZoneRect::BYTE_LEN as u16);
        put_u16(&mut bytes, 30, 1);
        put_u16(&mut bytes, 32, 1);
        put_u16(&mut bytes, 34, 1);
        let rect = ZoneRect::parse(&bytes).unwrap();
        assert_eq!(rect.origin, [-100, 200, -300]);
        assert_eq!(rect.dimensions, [40, 50, 60]);
        assert_eq!(rect.octree_max_depth, [1, 1, 1]);

        put_u16(&mut bytes, 28, 34);
        assert!(ZoneRect::parse(&bytes).is_err());
        put_u16(&mut bytes, 28, 1);
        assert!(ZoneRect::parse(&bytes).is_ok());
        put_u16(&mut bytes, 30, 32);
        assert!(ZoneRect::parse(&bytes).is_err());
    }

    #[test]
    fn zone_header_resolves_scene_indices_without_runtime_pointers() {
        let mut bytes = vec![0_u8; ZoneHeader::BYTE_LEN];
        put_u32(&mut bytes, 0, 2);
        put_u32(&mut bytes, 4, 0x1111_1111);
        put_u32(&mut bytes, 4 + ZONE_WORLD_BYTE_LEN, 0x2222_2223);
        put_u32(&mut bytes, 0x204, 3);
        put_u32(&mut bytes, 0x208, 2);
        put_u32(&mut bytes, 0x20c, 4);
        put_u32(&mut bytes, 0x210, 1);
        put_u32(&mut bytes, 0x214, 0x3333_3333);
        put_u32(&mut bytes, ZoneHeader::LOAD_LIST_OFFSET, 1);
        put_u32(&mut bytes, ZoneHeader::LOAD_LIST_OFFSET + 4, 1);
        put_u32(&mut bytes, ZoneHeader::LOAD_LIST_OFFSET + 8, 0x4444_4445);
        put_u32(
            &mut bytes,
            ZoneHeader::LOAD_LIST_OFFSET + 40,
            PageIndex::new(9).tagged(),
        );
        put_u32(&mut bytes, 0x2dc, 0xaabb_ccdd);
        put_u32(&mut bytes, ZoneHeader::GRAPHICS_OFFSET + 8, 0x1234_0000);
        put_u32(&mut bytes, ZoneHeader::GRAPHICS_OFFSET + 28, 0x55aa);
        put_u32(
            &mut bytes,
            ZoneHeader::GRAPHICS_OFFSET + 32,
            (-45_i32).cast_unsigned(),
        );
        put_u32(&mut bytes, ZoneHeader::GRAPHICS_OFFSET + 36, 0x5555_5555);
        bytes[ZoneHeader::GRAPHICS_OFFSET + 44..ZoneHeader::GRAPHICS_OFFSET + 47]
            .copy_from_slice(&[1, 2, 3]);

        let zone = ZoneHeader::parse(&bytes).unwrap();
        assert_eq!(zone.worlds[1].geometry, Eid::from_raw(0x2222_2223));
        assert_eq!(zone.path_item_index(1), Some(4));
        assert_eq!(zone.entity_item_index(3), Some(8));
        assert_eq!(zone.entity_item_index(4), None);
        assert_eq!(zone.load_list.pages, [PageIndex::new(9)]);
        assert_eq!(zone.graphics.visibility_depth, 0x1234_0000);
        assert_eq!(zone.graphics.water_y, -45);
        assert_eq!(zone.graphics.transition_color, [1, 2, 3]);
        assert_eq!(zone.display_flags, 0xaabb_ccdd);
    }

    proptest! {
        #[test]
        fn malformed_zdat_inputs_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..1200)) {
            let _ = ZoneHeader::parse(&bytes);
            let _ = ZonePath::parse(&bytes);
            let _ = ZoneEntity::parse(&bytes);
            let _ = ZoneLoadList::parse(&bytes);
            let _ = ZoneRect::parse(&bytes);
        }
    }
}
