//! Bounds-checked retail TGEO geometry and SVTX/CVTX animation frames.
//!
//! The original runtime rewrites EIDs and vertex offsets into native pointers
//! while pages are loaded. This module keeps every identity serialized: a
//! frame names its TGEO by [`Eid`], polygons retain byte offsets into the
//! six-byte vertex array, and callers resolve both through validated values.

use std::collections::BTreeSet;

use crate::binary::{Eid, FormatError, Reader, checked_slice};

use super::structs::{ColorInfo, RegionInfo};
use super::{Nsd, Nsf};

/// Normal-lit vertex animation entry type.
pub const SVTX_ENTRY_TYPE: u32 = 1;
/// Shared textured geometry entry type.
pub const TGEO_ENTRY_TYPE: u32 = 2;
/// Per-vertex-color animation entry type.
pub const CVTX_ENTRY_TYPE: u32 = 20;

const TGEO_HEADER_BYTES: usize = 20;
const OBJECT_FRAME_HEADER_BYTES: usize = 56;
const OBJECT_VERTEX_BYTES: usize = 6;

/// Fixed header at the start of TGEO item zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectGeometryHeader {
    pub polygon_count: u32,
    pub scale: [i32; 3],
    /// Length of the variable descriptor table in 32-bit words.
    pub texture_word_count: u32,
}

impl ObjectGeometryHeader {
    pub const BYTE_LEN: usize = TGEO_HEADER_BYTES;

    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let mut reader = Reader::new(bytes);
        Ok(Self {
            polygon_count: reader.u32_le()?,
            scale: [reader.i32_le()?, reader.i32_le()?, reader.i32_le()?],
            texture_word_count: reader.u32_le()?,
        })
    }
}

/// One eight-byte TGEO triangle.
///
/// Vertex values are byte offsets from the beginning of an SVTX/CVTX frame's
/// vertex payload. The final halfword stores a 15-bit texture-word index and
/// one flat-shading bit in the on-disc little-endian bitfield order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectPolygon {
    pub vertex_offsets: [u16; 3],
    pub texture_word_index: u16,
    pub flat_shaded: bool,
}

impl ObjectPolygon {
    pub const BYTE_LEN: usize = 8;

    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let mut reader = Reader::new(bytes);
        let vertex_offsets = [reader.u16_le()?, reader.u16_le()?, reader.u16_le()?];
        let packed = reader.u16_le()?;
        Ok(Self {
            vertex_offsets,
            texture_word_index: packed & 0x7fff,
            flat_shaded: packed & 0x8000 != 0,
        })
    }
}

/// Material selected by one TGEO polygon.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectMaterial {
    Color(ColorInfo),
    Texture {
        color: ColorInfo,
        texture_page: Eid,
        region: RegionInfo,
    },
}

/// Complete TGEO items zero and one with descriptor starts prevalidated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectGeometry {
    pub header: ObjectGeometryHeader,
    /// Raw words are retained because polygons address this variable-width
    /// table in words, not in `TextureInfo` records.
    pub texture_words: Vec<u32>,
    pub polygons: Vec<ObjectPolygon>,
    descriptor_starts: BTreeSet<usize>,
}

impl ObjectGeometry {
    pub fn material_for_polygon(
        &self,
        polygon: ObjectPolygon,
    ) -> Result<ObjectMaterial, FormatError> {
        let index = usize::from(polygon.texture_word_index);
        if !self.descriptor_starts.contains(&index) {
            return Err(FormatError::at(
                index.saturating_mul(4),
                "TGEO polygon does not reference the start of a texture descriptor",
            ));
        }
        let color = ColorInfo::from_raw(self.texture_words[index]);
        if color.color_type() == 0 {
            return Ok(ObjectMaterial::Color(color));
        }
        let texture_page = Eid::from_raw(self.texture_words[index + 1]);
        let region = RegionInfo::from_raw(self.texture_words[index + 2]);
        Ok(ObjectMaterial::Texture {
            color,
            texture_page,
            region,
        })
    }
}

/// Parses TGEO item zero (header/descriptors) and item one (triangles).
pub fn parse_object_geometry(
    header_item: &[u8],
    polygon_item: &[u8],
) -> Result<ObjectGeometry, FormatError> {
    let header = ObjectGeometryHeader::parse(header_item)?;
    let word_count = usize::try_from(header.texture_word_count)
        .map_err(|_| FormatError::at(16, "TGEO texture-word count does not fit the host"))?;
    let texture_bytes = word_count
        .checked_mul(4)
        .ok_or_else(|| FormatError::at(16, "TGEO texture table length overflows"))?;
    let expected_header_bytes = ObjectGeometryHeader::BYTE_LEN
        .checked_add(texture_bytes)
        .ok_or_else(|| FormatError::at(16, "TGEO header item length overflows"))?;
    require_exact_len(
        header_item,
        expected_header_bytes,
        "TGEO header/texture item",
    )?;
    let mut words = Reader::new(checked_slice(
        header_item,
        ObjectGeometryHeader::BYTE_LEN,
        texture_bytes,
        "TGEO texture words",
    )?);
    let mut texture_words = Vec::with_capacity(word_count);
    while words.remaining() != 0 {
        texture_words.push(words.u32_le()?);
    }

    let mut descriptor_starts = BTreeSet::new();
    let mut word = 0_usize;
    while word < texture_words.len() {
        descriptor_starts.insert(word);
        let color = ColorInfo::from_raw(texture_words[word]);
        let width = if color.color_type() == 0 { 1 } else { 3 };
        let end = word
            .checked_add(width)
            .ok_or_else(|| FormatError::at(word * 4, "TGEO descriptor range overflows"))?;
        if end > texture_words.len() {
            return Err(FormatError::at(
                word * 4,
                "TGEO textured descriptor is truncated",
            ));
        }
        if width == 3 {
            let texture_page = Eid::from_raw(texture_words[word + 1]);
            if !texture_page.is_named() {
                return Err(FormatError::at(
                    (word + 1) * 4,
                    "TGEO texture page is not a named EID",
                ));
            }
            let region = RegionInfo::from_raw(texture_words[word + 2]);
            if region.color_mode() > 2 {
                return Err(FormatError::at(
                    (word + 2) * 4,
                    "TGEO texture region uses reserved color mode three",
                ));
            }
        }
        word = end;
    }

    let polygon_count = usize::try_from(header.polygon_count)
        .map_err(|_| FormatError::at(0, "TGEO polygon count does not fit the host"))?;
    let polygon_bytes = polygon_count
        .checked_mul(ObjectPolygon::BYTE_LEN)
        .ok_or_else(|| FormatError::at(0, "TGEO polygon array length overflows"))?;
    require_exact_len(polygon_item, polygon_bytes, "TGEO polygon item")?;
    let mut polygons = Vec::with_capacity(polygon_count);
    for index in 0..polygon_count {
        let offset = index * ObjectPolygon::BYTE_LEN;
        let polygon = ObjectPolygon::parse(checked_slice(
            polygon_item,
            offset,
            ObjectPolygon::BYTE_LEN,
            "TGEO polygon",
        )?)?;
        if !descriptor_starts.contains(&usize::from(polygon.texture_word_index)) {
            return Err(FormatError::at(
                offset + 6,
                "TGEO polygon texture index is not a descriptor start",
            ));
        }
        polygons.push(polygon);
    }

    Ok(ObjectGeometry {
        header,
        texture_words,
        polygons,
        descriptor_starts,
    })
}

/// Vertex payload interpretation selected by the NSF entry type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectVertexKind {
    Lit,
    Colored,
}

impl ObjectVertexKind {
    pub fn from_entry_type(entry_type: u32) -> Result<Self, FormatError> {
        match entry_type {
            SVTX_ENTRY_TYPE => Ok(Self::Lit),
            CVTX_ENTRY_TYPE => Ok(Self::Colored),
            _ => Err(FormatError::global(format!(
                "entry type {entry_type} is neither SVTX nor CVTX"
            ))),
        }
    }
}

/// Fixed fields shared by SVTX and CVTX frame items.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectFrameHeader {
    /// Number of six-byte vertices following the fixed header.
    pub vertex_count: u32,
    pub geometry_eid: Eid,
    pub origin: [i32; 3],
    pub local_bound_min: [i32; 3],
    pub local_bound_max: [i32; 3],
    pub collision_center: [i32; 3],
}

impl ObjectFrameHeader {
    pub const BYTE_LEN: usize = OBJECT_FRAME_HEADER_BYTES;

    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let mut reader = Reader::new(bytes);
        Ok(Self {
            vertex_count: reader.u32_le()?,
            geometry_eid: Eid::from_raw(reader.u32_le()?),
            origin: [reader.i32_le()?, reader.i32_le()?, reader.i32_le()?],
            local_bound_min: [reader.i32_le()?, reader.i32_le()?, reader.i32_le()?],
            local_bound_max: [reader.i32_le()?, reader.i32_le()?, reader.i32_le()?],
            collision_center: [reader.i32_le()?, reader.i32_le()?, reader.i32_le()?],
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LitObjectVertex {
    pub position: [u8; 3],
    pub normal: [i8; 3],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ColoredObjectVertex {
    pub position: [u8; 3],
    pub color: [u8; 3],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectVertex {
    Lit(LitObjectVertex),
    Colored(ColoredObjectVertex),
}

impl ObjectVertex {
    #[must_use]
    pub const fn position(self) -> [u8; 3] {
        match self {
            Self::Lit(vertex) => vertex.position,
            Self::Colored(vertex) => vertex.position,
        }
    }
}

/// One owned animation frame. Vertices remain in their exact six-byte form.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectFrame {
    pub kind: ObjectVertexKind,
    pub header: ObjectFrameHeader,
    vertex_bytes: Vec<u8>,
    trailing_bytes: Vec<u8>,
}

impl ObjectFrame {
    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.vertex_bytes.len() / OBJECT_VERTEX_BYTES
    }

    /// Opaque two- or four-byte item tail retained for exact round-tripping.
    /// Retail aligns each frame to the strictly next four-byte boundary;
    /// rendering indexes only the counted six-byte vertices.
    #[must_use]
    pub fn trailing_bytes(&self) -> &[u8] {
        &self.trailing_bytes
    }

    pub fn vertex_at_offset(&self, offset: u16) -> Result<ObjectVertex, FormatError> {
        let offset = usize::from(offset);
        if !offset.is_multiple_of(OBJECT_VERTEX_BYTES) {
            return Err(FormatError::at(
                ObjectFrameHeader::BYTE_LEN + offset,
                "TGEO vertex byte offset is not aligned to a six-byte vertex",
            ));
        }
        let bytes = checked_slice(
            &self.vertex_bytes,
            offset,
            OBJECT_VERTEX_BYTES,
            "object vertex",
        )?;
        let position = [bytes[0], bytes[1], bytes[2]];
        Ok(match self.kind {
            ObjectVertexKind::Lit => ObjectVertex::Lit(LitObjectVertex {
                position,
                normal: [
                    bytes[3].cast_signed(),
                    bytes[4].cast_signed(),
                    bytes[5].cast_signed(),
                ],
            }),
            ObjectVertexKind::Colored => ObjectVertex::Colored(ColoredObjectVertex {
                position,
                color: [bytes[3], bytes[4], bytes[5]],
            }),
        })
    }

    /// Local model coordinates before TGEO/object matrices are applied.
    pub fn local_position(&self, offset: u16) -> Result<[i32; 3], FormatError> {
        let position = self.vertex_at_offset(offset)?.position();
        Ok([
            (self.header.origin[0] - 128 + i32::from(position[0])) * 4,
            (self.header.origin[1] - 128 + i32::from(position[1])) * 4,
            (self.header.origin[2] - 128 + i32::from(position[2])) * 4,
        ])
    }
}

/// Parses one SVTX/CVTX entry item.
pub fn parse_object_frame(
    bytes: &[u8],
    kind: ObjectVertexKind,
) -> Result<ObjectFrame, FormatError> {
    let header = ObjectFrameHeader::parse(bytes)?;
    if !header.geometry_eid.is_named() {
        return Err(FormatError::at(4, "object frame TGEO is not a named EID"));
    }
    let vertex_count = usize::try_from(header.vertex_count)
        .map_err(|_| FormatError::at(0, "object frame vertex count does not fit the host"))?;
    let vertex_len = vertex_count
        .checked_mul(OBJECT_VERTEX_BYTES)
        .ok_or_else(|| FormatError::at(0, "object frame vertex byte length overflows"))?;
    let unpadded_len = ObjectFrameHeader::BYTE_LEN
        .checked_add(vertex_len)
        .ok_or_else(|| FormatError::at(0, "object frame item length overflows"))?;
    // Retail advances to the strictly next four-byte boundary, leaving four
    // bytes even when the counted payload was already aligned. With six-byte
    // vertices this is exactly two bytes for odd counts and four for even.
    let padding = 4 - (unpadded_len % 4);
    let expected_len = unpadded_len
        .checked_add(padding)
        .ok_or_else(|| FormatError::at(0, "object frame padded length overflows"))?;
    require_exact_len(bytes, expected_len, "object frame item")?;
    let payload = checked_slice(
        bytes,
        ObjectFrameHeader::BYTE_LEN,
        bytes.len() - ObjectFrameHeader::BYTE_LEN,
        "object frame vertices",
    )?;
    Ok(ObjectFrame {
        kind,
        header,
        vertex_bytes: payload[..vertex_len].to_vec(),
        trailing_bytes: payload[vertex_len..].to_vec(),
    })
}

/// One resolved model frame and its validated TGEO.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectModelFrame {
    pub vertex_eid: Eid,
    pub frame_index: u16,
    pub frame: ObjectFrame,
    pub geometry: ObjectGeometry,
}

impl ObjectModelFrame {
    /// Combines a parsed frame with geometry resolved by the caller's asset
    /// catalog and validates every serialized polygon-to-vertex reference.
    ///
    /// Retail streams can point an SVTX/CVTX frame at a TGEO stored in a
    /// different level pair. Keeping this constructor separate from
    /// [`load_object_model_frame`] lets a browser-wide catalog resolve that
    /// relationship without weakening any of the reference validation.
    pub fn validated(
        vertex_eid: Eid,
        frame_index: u16,
        frame: ObjectFrame,
        geometry: ObjectGeometry,
    ) -> Result<Self, FormatError> {
        for (polygon_index, polygon) in geometry.polygons.iter().copied().enumerate() {
            for vertex_offset in polygon.vertex_offsets {
                frame.vertex_at_offset(vertex_offset).map_err(|error| {
                    FormatError::at(
                        polygon_index * ObjectPolygon::BYTE_LEN,
                        format!("TGEO polygon has an invalid frame vertex: {error}"),
                    )
                })?;
            }
        }
        Ok(Self {
            vertex_eid,
            frame_index,
            frame,
            geometry,
        })
    }
}

/// Resolves an SVTX/CVTX frame and its TGEO without relocating either EID.
pub fn load_object_model_frame(
    metadata: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    vertex_eid: Eid,
    frame_index: u16,
) -> Result<ObjectModelFrame, FormatError> {
    let vertex_entry = nsf.resolve_entry(metadata, vertex_eid)?;
    let kind = ObjectVertexKind::from_entry_type(vertex_entry.entry_type)?;
    let frame_item = vertex_entry.item(usize::from(frame_index)).ok_or_else(|| {
        FormatError::at(
            vertex_entry.byte_range().start,
            format!("object frame {frame_index} is outside entry {vertex_eid}"),
        )
    })?;
    let frame = parse_object_frame(frame_item.bytes(nsf_bytes)?, kind)?;
    let geometry_entry = nsf.resolve_entry(metadata, frame.header.geometry_eid)?;
    if geometry_entry.entry_type != TGEO_ENTRY_TYPE {
        return Err(FormatError::at(
            geometry_entry.byte_range().start + 8,
            format!(
                "object frame TGEO {} has entry type {}",
                frame.header.geometry_eid, geometry_entry.entry_type
            ),
        ));
    }
    let header_item = geometry_entry.item(0).ok_or_else(|| {
        FormatError::at(geometry_entry.byte_range().start, "TGEO has no header item")
    })?;
    let polygon_item = geometry_entry.item(1).ok_or_else(|| {
        FormatError::at(
            geometry_entry.byte_range().start,
            "TGEO has no polygon item",
        )
    })?;
    let geometry = parse_object_geometry(
        header_item.bytes(nsf_bytes)?,
        polygon_item.bytes(nsf_bytes)?,
    )?;
    ObjectModelFrame::validated(vertex_eid, frame_index, frame, geometry)
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

    #[test]
    fn polygon_preserves_byte_offsets_and_reversed_bitfield() {
        let mut bytes = [0_u8; ObjectPolygon::BYTE_LEN];
        put_u16(&mut bytes, 0, 0);
        put_u16(&mut bytes, 2, 6);
        put_u16(&mut bytes, 4, 12);
        put_u16(&mut bytes, 6, 0x9234);
        assert_eq!(
            ObjectPolygon::parse(&bytes).unwrap(),
            ObjectPolygon {
                vertex_offsets: [0, 6, 12],
                texture_word_index: 0x1234,
                flat_shaded: true,
            }
        );
    }

    #[test]
    fn geometry_validates_variable_width_descriptor_starts() {
        let mut header = vec![0_u8; ObjectGeometryHeader::BYTE_LEN + 16];
        put_u32(&mut header, 0, 1);
        for (offset, value) in [(4, 0x1000), (8, 0x1000), (12, 0x1000), (16, 4)] {
            put_u32(&mut header, offset, value);
        }
        put_u32(&mut header, 20, 0x8000_00ff);
        put_u32(&mut header, 24, Eid::from_name("pageT").unwrap().raw());
        put_u32(&mut header, 28, 0);
        put_u32(&mut header, 32, 0x0003_0201);
        let mut polygon = [0_u8; ObjectPolygon::BYTE_LEN];
        put_u16(&mut polygon, 0, 0);
        put_u16(&mut polygon, 2, 6);
        put_u16(&mut polygon, 4, 12);
        put_u16(&mut polygon, 6, 0);

        let geometry = parse_object_geometry(&header, &polygon).unwrap();
        assert!(matches!(
            geometry.material_for_polygon(geometry.polygons[0]),
            Ok(ObjectMaterial::Texture { .. })
        ));
        put_u16(&mut polygon, 6, 1);
        assert!(parse_object_geometry(&header, &polygon).is_err());
    }

    #[test]
    fn frame_uses_six_byte_vertex_offsets_and_retains_alignment_tail() {
        let mut bytes = vec![0_u8; ObjectFrameHeader::BYTE_LEN + 20];
        put_u32(&mut bytes, 0, 3);
        put_u32(&mut bytes, 4, Eid::from_name("meshG").unwrap().raw());
        put_u32(&mut bytes, 8, 128);
        put_u32(&mut bytes, 12, 127);
        put_u32(&mut bytes, 16, 126);
        bytes[56..62].copy_from_slice(&[128, 129, 130, 1, 2, 3]);
        bytes[62..68].copy_from_slice(&[127, 126, 125, 4, 5, 6]);
        bytes[68..74].copy_from_slice(&[1, 2, 3, 7, 8, 9]);
        bytes[74..76].copy_from_slice(&[0xbe, 0x40]);
        let frame = parse_object_frame(&bytes, ObjectVertexKind::Lit).unwrap();
        assert_eq!(frame.vertex_count(), 3);
        assert_eq!(frame.trailing_bytes(), [0xbe, 0x40]);
        assert_eq!(frame.local_position(0).unwrap(), [512, 512, 512]);
        assert!(frame.vertex_at_offset(1).is_err());
        assert!(frame.vertex_at_offset(18).is_err());

        bytes.pop();
        assert!(parse_object_frame(&bytes, ObjectVertexKind::Lit).is_err());
    }

    proptest! {
        #[test]
        fn malformed_object_formats_never_panic(
            header in proptest::collection::vec(any::<u8>(), 0..256),
            polygons in proptest::collection::vec(any::<u8>(), 0..512),
            frame in proptest::collection::vec(any::<u8>(), 0..1024),
        ) {
            let _ = parse_object_geometry(&header, &polygons);
            let _ = parse_object_frame(&frame, ObjectVertexKind::Lit);
            let _ = parse_object_frame(&frame, ObjectVertexKind::Colored);
        }
    }
}
