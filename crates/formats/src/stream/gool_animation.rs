//! Bounds-checked GOOL item-five animation descriptors.
//!
//! GOOL item five is a packed byte string containing animation descriptors.
//! Every descriptor begins with the same four-byte header. The five retail
//! payloads remain pointer-free: EIDs stay tagged values, sprite/fragment
//! arrays are length-checked, text terms retain raw bytes, and font references
//! are validated as byte offsets into this same item.

use crate::binary::{Eid, FormatError, Reader, checked_slice};

use super::structs::{ColorInfo, RegionInfo};

/// Size of the header shared by all known GOOL animation descriptors.
pub const GOOL_ANIMATION_HEADER_LEN: usize = 4;

/// Exact serialized size of a type-one vertex animation descriptor.
pub const GOOL_VERTEX_ANIMATION_LEN: usize = 8;
/// Exact serialized size of one sprite texture descriptor.
pub const GOOL_TEXTURE_INFO_LEN: usize = 8;
/// Exact serialized size of one font glyph.
pub const GOOL_GLYPH_LEN: usize = 12;
/// Exact serialized size of one textured fragment.
pub const GOOL_FRAGMENT_LEN: usize = 16;
/// Number of conventional printable slots before retail's backdrop alias.
///
/// Type-three descriptors serialize `header.length` glyphs. Retail's C view
/// names only the first 63 (`0x20..=0x5e`) and pointer-indexes later records
/// for controller icons such as `c`, `s`, `t`, and `x`. The fragment-shaped
/// backdrop aliases the bytes beginning at glyph index 63.
pub const GOOL_FONT_GLYPH_COUNT: usize = 63;
/// Maximum serialized size of a type-three font descriptor.
pub const GOOL_MAX_FONT_ANIMATION_LEN: usize = 8 + u8::MAX as usize * GOOL_GLYPH_LEN;

/// Animation kinds named by the retail GOOL format.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum GoolAnimationKind {
    /// Per-frame SVTX/CVTX model entries.
    Vertex = 1,
    /// Screen-aligned textured sprites.
    Sprite = 2,
    /// Glyph and backdrop definitions.
    Font = 3,
    /// Text-string definitions.
    Text = 4,
    /// Per-frame textured fragments.
    Fragment = 5,
}

impl GoolAnimationKind {
    fn parse(raw: u8, offset: usize) -> Result<Self, FormatError> {
        match raw {
            1 => Ok(Self::Vertex),
            2 => Ok(Self::Sprite),
            3 => Ok(Self::Font),
            4 => Ok(Self::Text),
            5 => Ok(Self::Fragment),
            _ => Err(FormatError::at(
                offset,
                format!("unknown GOOL animation type {raw}"),
            )),
        }
    }
}

/// Four bytes common to every known animation descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GoolAnimationHeader {
    /// Descriptor payload kind.
    pub kind: GoolAnimationKind,
    /// First byte retained from the source's currently-unknown field.
    pub reserved_1: u8,
    /// Retail animation length, normally used as a frame count.
    pub length: u8,
    /// Second byte retained from the source's currently-unknown field.
    pub reserved_3: u8,
}

/// Fully validated type-one vertex animation descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GoolVertexAnimation {
    /// Common descriptor header. Its kind is always
    /// [`GoolAnimationKind::Vertex`].
    pub header: GoolAnimationHeader,
    /// Named SVTX/CVTX entry supplying the animation's frames.
    pub model_eid: Eid,
}

/// Color/region pair used by sprites, glyphs, and fragments.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GoolTextureInfo {
    pub color: ColorInfo,
    pub region: RegionInfo,
}

/// Fully validated type-two screen-aligned sprite animation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoolSpriteAnimation {
    pub header: GoolAnimationHeader,
    pub texture_page: Eid,
    pub frames: Vec<GoolTextureInfo>,
}

/// One exact twelve-byte font glyph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GoolGlyph {
    pub texture: GoolTextureInfo,
    pub width: u16,
    pub height: u16,
}

impl GoolGlyph {
    /// Retail aliases the first descriptor word as `has_texture`.
    #[must_use]
    pub const fn has_texture(self) -> bool {
        self.texture.color.raw() != 0
    }
}

/// One exact sixteen-byte rectangle/texture fragment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GoolFragment {
    pub texture: GoolTextureInfo,
    pub bounds: [i16; 4],
}

/// Fully validated variable-size type-three font descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoolFontAnimation {
    pub header: GoolAnimationHeader,
    pub texture_page: Eid,
    pub glyphs: Vec<GoolGlyph>,
    pub backdrop: Option<GoolFragment>,
}

/// Fully validated type-four text terms and their item-five font reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoolTextAnimation {
    pub header: GoolAnimationHeader,
    pub unknown_word: u32,
    /// Word offset from the start of item five, as serialized by retail.
    pub font_word_offset: u32,
    /// Raw NUL-delimited format terms, without their terminators.
    pub terms: Vec<Vec<u8>>,
    serialized_len: usize,
}

/// Fully validated type-five fragment animation, stored frame-major.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoolFragmentAnimation {
    pub header: GoolAnimationHeader,
    pub texture_page: Eid,
    pub fragments_per_frame: u32,
    pub fragments: Vec<GoolFragment>,
}

impl GoolFragmentAnimation {
    /// Returns one frame's exact contiguous fragment slice.
    #[must_use]
    pub fn frame(&self, index: usize) -> Option<&[GoolFragment]> {
        let per_frame = usize::try_from(self.fragments_per_frame).ok()?;
        let start = index.checked_mul(per_frame)?;
        self.fragments.get(start..start.checked_add(per_frame)?)
    }
}

/// A fully validated GOOL item-five animation descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GoolAnimationDescriptor {
    /// Exact type-one vertex-animation payload.
    Vertex(GoolVertexAnimation),
    Sprite(GoolSpriteAnimation),
    Font(GoolFontAnimation),
    Text(GoolTextAnimation),
    Fragment(GoolFragmentAnimation),
}

impl GoolAnimationDescriptor {
    /// Common header shared by this descriptor.
    #[must_use]
    pub const fn header(&self) -> GoolAnimationHeader {
        match self {
            Self::Vertex(value) => value.header,
            Self::Sprite(value) => value.header,
            Self::Font(value) => value.header,
            Self::Text(value) => value.header,
            Self::Fragment(value) => value.header,
        }
    }

    /// Number of serialized bytes consumed by this descriptor.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        match self {
            Self::Vertex(_) => GOOL_VERTEX_ANIMATION_LEN,
            Self::Sprite(value) => 8 + value.frames.len() * GOOL_TEXTURE_INFO_LEN,
            Self::Font(value) => 8 + value.glyphs.len() * GOOL_GLYPH_LEN,
            Self::Text(value) => value.serialized_len,
            Self::Fragment(value) => 12 + value.fragments.len() * GOOL_FRAGMENT_LEN,
        }
    }
}

/// Parses the common header at one byte offset within a GOOL item-five slice.
///
/// Unknown type tags are rejected. Known types two through five are accepted
/// here because parsing their fixed common header does not claim that their
/// variable payloads have been validated.
pub fn parse_gool_animation_header(
    item_five: &[u8],
    descriptor_offset: usize,
) -> Result<GoolAnimationHeader, FormatError> {
    checked_slice(
        item_five,
        descriptor_offset,
        GOOL_ANIMATION_HEADER_LEN,
        "GOOL animation header",
    )?;

    let mut reader = Reader::with_position(item_five, descriptor_offset)?;
    let kind_offset = reader.position();
    let kind = GoolAnimationKind::parse(reader.u8()?, kind_offset)?;
    let reserved_1 = reader.u8()?;
    let length = reader.u8()?;
    let reserved_3 = reader.u8()?;
    Ok(GoolAnimationHeader {
        kind,
        reserved_1,
        length,
        reserved_3,
    })
}

/// Parses one complete descriptor at a byte offset within GOOL item five.
///
/// The returned descriptor owns all variable data and records the exact byte
/// extent selected by its header counts. Extra bytes belonging to following
/// packed descriptors are never consumed.
pub fn parse_gool_animation_descriptor(
    item_five: &[u8],
    descriptor_offset: usize,
) -> Result<GoolAnimationDescriptor, FormatError> {
    let header = parse_gool_animation_header(item_five, descriptor_offset)?;
    let payload_offset = descriptor_offset
        .checked_add(GOOL_ANIMATION_HEADER_LEN)
        .ok_or_else(|| FormatError::at(descriptor_offset, "GOOL animation offset overflows"))?;
    let mut reader = Reader::with_position(item_five, payload_offset)?;
    match header.kind {
        GoolAnimationKind::Vertex => {
            let model_eid = read_named_eid(&mut reader, "GOOL vertex animation model")?;
            Ok(GoolAnimationDescriptor::Vertex(GoolVertexAnimation {
                header,
                model_eid,
            }))
        }
        GoolAnimationKind::Sprite => {
            let texture_page = read_named_eid(&mut reader, "GOOL sprite texture page")?;
            let frames = (0..usize::from(header.length))
                .map(|_| parse_texture_info(&mut reader))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(GoolAnimationDescriptor::Sprite(GoolSpriteAnimation {
                header,
                texture_page,
                frames,
            }))
        }
        GoolAnimationKind::Font => {
            let texture_page = read_named_eid(&mut reader, "GOOL font texture page")?;
            // The source C declaration names 63 conventional printable slots,
            // but retail serializes `length` records and deliberately indexes
            // beyond that declaration for controller-icon glyphs. The
            // fragment-shaped backdrop aliases glyph 63; only its texture
            // words are consumed by GfxTransformFragment because text layout
            // supplies the bounds separately.
            let glyphs = (0..usize::from(header.length))
                .map(|_| {
                    Ok(GoolGlyph {
                        texture: parse_texture_info(&mut reader)?,
                        width: reader.u16_le()?,
                        height: reader.u16_le()?,
                    })
                })
                .collect::<Result<Vec<_>, FormatError>>()?;
            let backdrop = glyphs
                .get(GOOL_FONT_GLYPH_COUNT)
                .copied()
                .filter(|glyph| glyph.has_texture())
                .map(|glyph| GoolFragment {
                    texture: glyph.texture,
                    bounds: [0; 4],
                });
            Ok(GoolAnimationDescriptor::Font(GoolFontAnimation {
                header,
                texture_page,
                glyphs,
                backdrop,
            }))
        }
        GoolAnimationKind::Text => {
            let unknown_word = reader.u32_le()?;
            let font_word_offset = reader.u32_le()?;
            let font_offset = usize::try_from(font_word_offset)
                .ok()
                .and_then(|offset| offset.checked_mul(4))
                .ok_or_else(|| {
                    FormatError::at(reader.position() - 4, "GOOL text font offset overflows")
                })?;
            let font_header = parse_gool_animation_header(item_five, font_offset)?;
            if font_header.kind != GoolAnimationKind::Font {
                return Err(FormatError::at(
                    font_offset,
                    "GOOL text font offset does not reference a font descriptor",
                ));
            }
            let mut terms = Vec::with_capacity(usize::from(header.length));
            for _ in 0..header.length {
                let start = reader.position();
                let remaining =
                    checked_slice(item_five, start, reader.remaining(), "GOOL text terms")?;
                let length = remaining
                    .iter()
                    .position(|byte| *byte == 0)
                    .ok_or_else(|| {
                        FormatError::at(start, "GOOL text term has no NUL terminator")
                    })?;
                terms.push(reader.take(length)?.to_vec());
                reader.u8()?;
            }
            Ok(GoolAnimationDescriptor::Text(GoolTextAnimation {
                header,
                unknown_word,
                font_word_offset,
                terms,
                serialized_len: reader.position() - descriptor_offset,
            }))
        }
        GoolAnimationKind::Fragment => {
            let texture_page = read_named_eid(&mut reader, "GOOL fragment texture page")?;
            let fragments_per_frame = reader.u32_le()?;
            let fragment_count = usize::try_from(fragments_per_frame)
                .ok()
                .and_then(|count| count.checked_mul(usize::from(header.length)))
                .ok_or_else(|| {
                    FormatError::at(
                        reader.position() - 4,
                        "GOOL fragment animation count overflows",
                    )
                })?;
            let fragments = (0..fragment_count)
                .map(|_| parse_fragment(&mut reader))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(GoolAnimationDescriptor::Fragment(GoolFragmentAnimation {
                header,
                texture_page,
                fragments_per_frame,
                fragments,
            }))
        }
    }
}

fn read_named_eid(reader: &mut Reader<'_>, context: &str) -> Result<Eid, FormatError> {
    let offset = reader.position();
    let eid = Eid::from_raw(reader.u32_le()?);
    if !eid.is_named() {
        return Err(FormatError::at(
            offset,
            format!("{context} is not a named EID"),
        ));
    }
    Ok(eid)
}

fn parse_texture_info(reader: &mut Reader<'_>) -> Result<GoolTextureInfo, FormatError> {
    Ok(GoolTextureInfo {
        color: ColorInfo::from_raw(reader.u32_le()?),
        region: RegionInfo::from_raw(reader.u32_le()?),
    })
}

fn parse_fragment(reader: &mut Reader<'_>) -> Result<GoolFragment, FormatError> {
    Ok(GoolFragment {
        texture: parse_texture_info(reader)?,
        bounds: [
            reader.i16_le()?,
            reader.i16_le()?,
            reader.i16_le()?,
            reader.i16_le()?,
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parses_exact_vertex_descriptor_at_an_unaligned_byte_offset() {
        let model_eid = Eid::from_name("model").unwrap();
        let mut item = vec![0xaa, 1, 0x12, 7, 0x34];
        item.extend_from_slice(&model_eid.raw().to_le_bytes());
        item.extend_from_slice(&[0xee, 0xff]);

        let descriptor = parse_gool_animation_descriptor(&item, 1).unwrap();
        assert_eq!(descriptor.byte_len(), GOOL_VERTEX_ANIMATION_LEN);
        assert_eq!(
            descriptor,
            GoolAnimationDescriptor::Vertex(GoolVertexAnimation {
                header: GoolAnimationHeader {
                    kind: GoolAnimationKind::Vertex,
                    reserved_1: 0x12,
                    length: 7,
                    reserved_3: 0x34,
                },
                model_eid,
            })
        );
    }

    #[test]
    fn parses_headers_for_all_named_kinds() {
        for (raw_kind, kind) in [
            (1, GoolAnimationKind::Vertex),
            (2, GoolAnimationKind::Sprite),
            (3, GoolAnimationKind::Font),
            (4, GoolAnimationKind::Text),
            (5, GoolAnimationKind::Fragment),
        ] {
            let bytes = [raw_kind, 0x56, 0x78, 0x9a];
            assert_eq!(
                parse_gool_animation_header(&bytes, 0).unwrap(),
                GoolAnimationHeader {
                    kind,
                    reserved_1: 0x56,
                    length: 0x78,
                    reserved_3: 0x9a,
                }
            );
        }
    }

    #[test]
    fn parses_sprite_font_text_and_fragment_payload_extents() {
        let page = Eid::from_name("pageT").unwrap();

        let mut sprite = vec![2, 0, 2, 0];
        sprite.extend_from_slice(&page.raw().to_le_bytes());
        sprite.extend_from_slice(&[0; GOOL_TEXTURE_INFO_LEN * 2]);
        let sprite = parse_gool_animation_descriptor(&sprite, 0).unwrap();
        let GoolAnimationDescriptor::Sprite(sprite) = sprite else {
            panic!("expected sprite descriptor");
        };
        assert_eq!(sprite.frames.len(), 2);
        assert_eq!(
            GoolAnimationDescriptor::Sprite(sprite).byte_len(),
            8 + GOOL_TEXTURE_INFO_LEN * 2
        );

        let glyph_count = GOOL_FONT_GLYPH_COUNT + 1;
        let font_len = 8 + glyph_count * GOOL_GLYPH_LEN;
        let mut font_and_text = vec![0_u8; font_len];
        font_and_text[0..4].copy_from_slice(&[3, 0, glyph_count as u8, 0]);
        font_and_text[4..8].copy_from_slice(&page.raw().to_le_bytes());
        let text_offset = font_and_text.len();
        font_and_text.extend_from_slice(&[4, 0, 2, 0]);
        font_and_text.extend_from_slice(&0x1234_5678_u32.to_le_bytes());
        font_and_text.extend_from_slice(&0_u32.to_le_bytes());
        font_and_text.extend_from_slice(b"one\0two\0");
        let font = parse_gool_animation_descriptor(&font_and_text, 0).unwrap();
        let GoolAnimationDescriptor::Font(font) = font else {
            panic!("expected font descriptor");
        };
        assert_eq!(font.glyphs.len(), glyph_count);
        assert!(font.backdrop.is_none());
        assert_eq!(GoolAnimationDescriptor::Font(font).byte_len(), font_len);
        let text = parse_gool_animation_descriptor(&font_and_text, text_offset).unwrap();
        let GoolAnimationDescriptor::Text(text) = text else {
            panic!("expected text descriptor");
        };
        assert_eq!(text.terms, [b"one".to_vec(), b"two".to_vec()]);
        assert_eq!(
            GoolAnimationDescriptor::Text(text).byte_len(),
            12 + b"one\0two\0".len()
        );

        let mut fragments = vec![5, 0, 2, 0];
        fragments.extend_from_slice(&page.raw().to_le_bytes());
        fragments.extend_from_slice(&1_u32.to_le_bytes());
        fragments.extend_from_slice(&[0; GOOL_FRAGMENT_LEN * 2]);
        let fragments = parse_gool_animation_descriptor(&fragments, 0).unwrap();
        let GoolAnimationDescriptor::Fragment(fragments) = fragments else {
            panic!("expected fragment descriptor");
        };
        assert_eq!(fragments.fragments.len(), 2);
        assert_eq!(fragments.frame(1).unwrap().len(), 1);
    }

    #[test]
    fn font_length_bounds_the_variable_glyph_table_and_backdrop_alias() {
        let page = Eid::from_name("pageT").unwrap();
        for length in [64, 90, 95] {
            let byte_len = 8 + usize::from(length) * GOOL_GLYPH_LEN;
            let mut bytes = vec![0_u8; byte_len];
            bytes[0..4].copy_from_slice(&[3, 0, length, 0]);
            bytes[4..8].copy_from_slice(&page.raw().to_le_bytes());
            let backdrop_offset = 8 + GOOL_FONT_GLYPH_COUNT * GOOL_GLYPH_LEN;
            bytes[backdrop_offset..backdrop_offset + 4]
                .copy_from_slice(&0x8123_4567_u32.to_le_bytes());

            let descriptor = parse_gool_animation_descriptor(&bytes, 0).unwrap();
            assert_eq!(descriptor.byte_len(), byte_len);
            let GoolAnimationDescriptor::Font(font) = descriptor else {
                panic!("expected font descriptor");
            };
            assert_eq!(font.header.length, length);
            assert_eq!(font.glyphs.len(), usize::from(length));
            assert_eq!(
                font.backdrop.map(|backdrop| backdrop.texture),
                Some(font.glyphs[GOOL_FONT_GLYPH_COUNT].texture)
            );
            assert_eq!(font.backdrop.unwrap().bounds, [0; 4]);
        }
    }

    #[test]
    fn variable_font_rejects_a_truncated_extended_glyph() {
        let page = Eid::from_name("pageT").unwrap();
        let mut bytes = vec![0_u8; 8 + 90 * GOOL_GLYPH_LEN - 1];
        bytes[0..4].copy_from_slice(&[3, 0, 90, 0]);
        bytes[4..8].copy_from_slice(&page.raw().to_le_bytes());

        let error = parse_gool_animation_descriptor(&bytes, 0).unwrap_err();
        assert!(error.message().contains("field is truncated"));
    }

    #[test]
    fn rejects_truncation_unknown_types_and_untagged_eids() {
        let error = parse_gool_animation_header(&[1, 0, 1], 0).unwrap_err();
        assert_eq!(error.offset(), Some(0));
        assert!(error.message().contains("header is truncated"));

        let error = parse_gool_animation_header(&[9, 0, 1, 0], 0).unwrap_err();
        assert_eq!(error.offset(), Some(0));
        assert!(error.message().contains("unknown GOOL animation type 9"));

        let error = parse_gool_animation_descriptor(&[1, 0, 1, 0], 0).unwrap_err();
        assert_eq!(error.offset(), Some(4));
        assert!(error.message().contains("field is truncated"));

        let error =
            parse_gool_animation_descriptor(&[1, 0, 1, 0, 0x78, 0x56, 0x34, 0x12], 0).unwrap_err();
        assert_eq!(error.offset(), Some(4));
        assert!(error.message().contains("not a named EID"));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn arbitrary_item_bytes_and_offsets_never_panic(
            bytes in proptest::collection::vec(any::<u8>(), 0..512),
            offset in any::<usize>(),
        ) {
            let _ = parse_gool_animation_header(&bytes, offset);
            let _ = parse_gool_animation_descriptor(&bytes, offset);
        }
    }
}
