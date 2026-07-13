//! Exact WGEO/GOOL texture references and safe retail TPAG resolution.
//!
//! Retail `TextureInfo2` values are not C structs in memory here. They remain
//! two explicit little-endian words: a [`ColorInfo`] word followed by one
//! selected [`RegionInfo`] word. WGEO animation stores several region words
//! after a shared color word, so [`TextureInfo2::parse_wgeo_table`] accepts the
//! color-word index and selected animation frame separately.
//!
//! A TPAG is a complete 64 KiB PSX VRAM page. Its 16-byte NSF header is also
//! part of the pixel address space used by the original renderer, so page
//! resolution deliberately returns the whole page rather than only the bytes
//! after that header.

use core::fmt;

use crust_formats::binary::{Eid, FormatError, PageIndex, Reader, checked_slice};
use crust_formats::stream::structs::{ColorInfo, RegionInfo};
use crust_formats::stream::{NSF_PAGE_SIZE, Nsf, NsfPage};

use crate::cache::{TextureRequest, TextureUvBounds};
use crate::command::BlendMode;
use crate::texture::{
    ClutLocation, ColorMode, DecodedTexture, Palette, TEXTURE_PAGE_HEIGHT, TEXTURE_PAGE_WORD_WIDTH,
    TextureError, TextureRegion, decode_region,
};

/// Number of texture-coordinate entries in C1's exact retail UV map.
pub const UV_MAP_ENTRY_COUNT: u16 = 600;
/// Retail subsystem id stored in a TPAG page header.
pub const TPAG_ENTRY_TYPE: u32 = 5;

const WORD_BYTES: usize = 4;
const TEXTURE_INFO2_BYTES: usize = 8;
const UV_EXTENTS: [u8; 5] = [3, 7, 15, 31, 63];

/// Exact eight-byte `TextureInfo2` disk representation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TextureInfo2 {
    /// Primitive RGB, render flags, blend mode, and CLUT X block.
    pub color: ColorInfo,
    /// UV-map index, pixel mode, page segment/offsets, and CLUT Y row.
    pub region: RegionInfo,
}

impl TextureInfo2 {
    /// Serialized length of a non-animated color/region pair.
    pub const BYTE_LEN: usize = TEXTURE_INFO2_BYTES;

    /// Parses a color word and its immediately following region word.
    ///
    /// # Errors
    ///
    /// Returns a format error when fewer than eight bytes are available.
    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let mut reader = Reader::new(bytes);
        Ok(Self {
            color: ColorInfo::from_raw(reader.u32_le()?),
            region: RegionInfo::from_raw(reader.u32_le()?),
        })
    }

    /// Selects one entry from a WGEO's word-addressed texture-info table.
    ///
    /// `color_word_index` is the polygon's `tinf_idx`. `animation_frame` is
    /// zero for a static polygon and selects `rgninfos[animation_frame]` for an
    /// animated polygon. The region therefore lives at word
    /// `color_word_index + 1 + animation_frame`.
    ///
    /// # Errors
    ///
    /// Returns a format error if index arithmetic overflows or either selected
    /// word is outside `table`.
    pub fn parse_wgeo_table(
        table: &[u8],
        color_word_index: usize,
        animation_frame: usize,
    ) -> Result<Self, FormatError> {
        let color_offset = word_offset(color_word_index)?;
        let region_word_index = color_word_index
            .checked_add(1)
            .and_then(|index| index.checked_add(animation_frame))
            .ok_or_else(|| FormatError::global("WGEO texture-info word index overflows"))?;
        let region_offset = word_offset(region_word_index)?;
        Ok(Self {
            color: ColorInfo::from_raw(read_word_at(table, color_offset)?),
            region: RegionInfo::from_raw(read_word_at(table, region_offset)?),
        })
    }

    /// Whether the color word selects textured rather than flat-color drawing.
    #[must_use]
    pub const fn is_textured(self) -> bool {
        self.color.color_type() == 1
    }
}

/// Exact EID reference to a retail TPAG page.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TpagReference(Eid);

impl TpagReference {
    /// Retains an already decoded EID without relocating it to a pointer.
    #[must_use]
    pub const fn new(eid: Eid) -> Self {
        Self(eid)
    }

    /// Retains the exact 32-bit tagged representation.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self(Eid::from_raw(raw))
    }

    /// Referenced texture-page EID.
    #[must_use]
    pub const fn eid(self) -> Eid {
        self.0
    }

    /// Exact 32-bit tagged representation.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0.raw()
    }
}

/// Indexed-color palette reference inside the same TPAG as the texels.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ClutReference {
    /// TPAG containing the palette.
    pub tpag: TpagReference,
    /// X is a 16-word block and Y is a 128-row page coordinate.
    pub location: ClutLocation,
}

/// One exact integer coordinate from the 600-entry retail UV map.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct TextureCoordinate {
    pub x: u8,
    pub y: u8,
}

/// Triangle or quad coordinates selected by a region's ten-bit UV index.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TextureCoordinates {
    /// Exact table points. A triangle retains the retail `(99, 99)` sentinel
    /// as its fourth point.
    pub points: [TextureCoordinate; 4],
    vertex_count: u8,
}

impl TextureCoordinates {
    /// Number of active vertices: three for a sentinel entry, otherwise four.
    #[must_use]
    pub const fn vertex_count(self) -> usize {
        self.vertex_count as usize
    }

    /// Width and height of the inclusive source rectangle in pixels.
    #[must_use]
    pub fn pixel_dimensions(self) -> (u32, u32) {
        let (minimum, maximum) = self.bounds();
        (
            u32::from(maximum.x - minimum.x) + 1,
            u32::from(maximum.y - minimum.y) + 1,
        )
    }

    /// Maps exact integer coordinates onto a cache lease's padded UV bounds.
    ///
    /// The inactive fourth UV of a triangle duplicates the first, matching the
    /// original cache contract after it consumes the `(99, 99)` sentinel.
    #[must_use]
    pub fn cache_uvs(self, bounds: TextureUvBounds) -> [[f32; 2]; 4] {
        let (minimum, maximum) = self.bounds();
        let span_x = f32::from(maximum.x - minimum.x);
        let span_y = f32::from(maximum.y - minimum.y);
        let mut result = [[0.0; 2]; 4];
        for (destination, point) in result
            .iter_mut()
            .zip(self.points.iter())
            .take(self.vertex_count())
        {
            let factor_x = f32::from(point.x - minimum.x) / span_x;
            let factor_y = f32::from(point.y - minimum.y) / span_y;
            destination[0] = bounds.left + factor_x * (bounds.right - bounds.left);
            destination[1] = bounds.top + factor_y * (bounds.bottom - bounds.top);
        }
        if self.vertex_count == 3 {
            result[3] = result[0];
        }
        result
    }

    fn bounds(self) -> (TextureCoordinate, TextureCoordinate) {
        let first = self.points[0];
        let mut minimum = first;
        let mut maximum = first;
        for point in self.points.iter().take(self.vertex_count()).skip(1) {
            minimum.x = minimum.x.min(point.x);
            minimum.y = minimum.y.min(point.y);
            maximum.x = maximum.x.max(point.x);
            maximum.y = maximum.y.max(point.y);
        }
        (minimum, maximum)
    }
}

/// A polygon-ready retail texture reference before page resolution.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RetailTextureReference {
    /// TPAG selected by the WGEO polygon's `tpag_idx`.
    pub tpag: TpagReference,
    /// Color word and selected static/animated region word.
    pub info: TextureInfo2,
}

impl RetailTextureReference {
    /// Creates an unresolved retail reference without pointer relocation.
    #[must_use]
    pub const fn new(tpag: TpagReference, info: TextureInfo2) -> Self {
        Self { tpag, info }
    }

    /// Validates packed fields and derives the source rectangle/cache request.
    ///
    /// # Errors
    ///
    /// Rejects flat-color descriptors, color mode three, UV indices outside
    /// the exact 600-entry retail map, overflowing/out-of-page regions, and
    /// CLUTs which exceed the complete 64 KiB page.
    pub fn layout(self) -> Result<RetailTextureLayout, RetailTextureError> {
        if !self.info.is_textured() {
            return Err(RetailTextureError::UntexturedDescriptor);
        }
        let raw_mode = self.info.region.color_mode();
        let color_mode = match raw_mode {
            0 => ColorMode::Indexed4,
            1 => ColorMode::Indexed8,
            2 => ColorMode::Direct15,
            value => return Err(RetailTextureError::InvalidColorMode(value)),
        };
        let coordinates = texture_coordinates(self.info.region.uv_index())?;
        let (minimum, maximum) = coordinates.bounds();
        let pixels_per_offset = 2_u32 << (2 - u32::from(raw_mode));
        let page_x = u32::from(self.info.region.segment())
            .checked_mul(32)
            .and_then(|value| value.checked_add(u32::from(self.info.region.offset_x())))
            .and_then(|value| value.checked_mul(pixels_per_offset))
            .and_then(|value| value.checked_add(u32::from(minimum.x)))
            .ok_or(TextureError::DimensionsOverflow)?;
        let page_y = u32::from(self.info.region.offset_y())
            .checked_mul(4)
            .and_then(|value| value.checked_add(u32::from(minimum.y)))
            .ok_or(TextureError::DimensionsOverflow)?;
        let width = u32::from(maximum.x - minimum.x) + 1;
        let height = u32::from(maximum.y - minimum.y) + 1;
        let region = TextureRegion::new(page_x, page_y, width, height)?;
        if page_x
            .checked_add(width)
            .is_none_or(|right| right > color_mode.page_width())
            || page_y
                .checked_add(height)
                .is_none_or(|bottom| bottom > TEXTURE_PAGE_HEIGHT)
        {
            return Err(TextureError::RegionOutOfBounds {
                region,
                page_width: color_mode.page_width(),
                page_height: TEXTURE_PAGE_HEIGHT,
            }
            .into());
        }

        let clut = if let Some(colors) = color_mode.palette_len() {
            let location = ClutLocation {
                block_x: self.info.color.palette_x(),
                row: self.info.region.palette_y(),
            };
            validate_clut(location, colors)?;
            Some(ClutReference {
                tpag: self.tpag,
                location,
            })
        } else {
            None
        };
        let blend_mode = match self.info.color.semi_transparency() {
            0 => BlendMode::Average,
            1 => BlendMode::Additive,
            2 => BlendMode::Subtractive,
            3 => BlendMode::Opaque,
            value => return Err(RetailTextureError::InvalidBlendMode(value)),
        };
        Ok(RetailTextureLayout {
            request: TextureRequest {
                page_id: self.tpag.raw(),
                region,
                color_mode,
                blend_mode,
                clut: clut.map(|reference| reference.location),
            },
            coordinates,
            clut,
        })
    }

    /// Resolves the exact TPAG and decodes this region to upload-ready RGBA8.
    ///
    /// # Errors
    ///
    /// Returns any packed-field/layout, TPAG-resolution, or pixel-decoding
    /// failure. User bytes are always bounds checked.
    pub fn decode(self, nsf: &Nsf, nsf_bytes: &[u8]) -> Result<DecodedTexture, RetailTextureError> {
        let layout = self.layout()?;
        let page = resolve_texture_page(nsf, nsf_bytes, self.tpag)?;
        let palette = layout.request.clut.map(Palette::Page);
        Ok(decode_region(
            page.bytes(),
            layout.request.color_mode,
            layout.request.blend_mode,
            layout.request.region,
            palette,
        )?)
    }
}

/// Validated renderer/cache inputs derived from one packed retail reference.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RetailTextureLayout {
    /// Complete cache key, including the exact TPAG EID and CLUT location.
    pub request: TextureRequest,
    /// Exact triangle/quad orientation and per-vertex texture coordinates.
    pub coordinates: TextureCoordinates,
    /// Explicit same-page palette reference for indexed modes.
    pub clut: Option<ClutReference>,
}

/// Borrowed complete TPAG page, checked against the parsed NSF snapshot.
#[derive(Clone, Copy, Debug)]
pub struct ResolvedTexturePage<'a> {
    /// Logical page index within the validated NSF stream.
    pub page_index: PageIndex,
    /// Exact unresolved EID used by cache keys.
    pub tpag: TpagReference,
    bytes: &'a [u8],
}

impl<'a> ResolvedTexturePage<'a> {
    /// The complete 64 KiB page, including its 16-byte TPAG header.
    #[must_use]
    pub const fn bytes(self) -> &'a [u8] {
        self.bytes
    }

    /// Exact page id used by [`crate::TextureCache::install_page`].
    #[must_use]
    pub const fn cache_page_id(self) -> u32 {
        self.tpag.raw()
    }
}

/// Resolves one EID to exactly one validated TPAG page.
///
/// The supplied byte slice is checked against the parsed NSF's page index and
/// immutable header fields before any pixels are borrowed. This prevents a
/// caller from accidentally pairing an `Nsf` parsed from one stream with the
/// bytes of another stream.
///
/// # Errors
///
/// Returns an error for a missing/duplicate EID, non-TPAG entry type, offset or
/// truncation failure, or parsed/raw page-header mismatch.
pub fn resolve_texture_page<'a>(
    nsf: &Nsf,
    nsf_bytes: &'a [u8],
    tpag: TpagReference,
) -> Result<ResolvedTexturePage<'a>, RetailTextureError> {
    let mut found = None;
    for candidate in &nsf.pages {
        let NsfPage::Texture(page) = candidate else {
            continue;
        };
        if page.eid != tpag.eid() {
            continue;
        }
        if found.replace(page).is_some() {
            return Err(RetailTextureError::DuplicateTexturePage(tpag.eid()));
        }
    }
    let page = found.ok_or(RetailTextureError::MissingTexturePage(tpag.eid()))?;
    if page.entry_type != TPAG_ENTRY_TYPE {
        return Err(RetailTextureError::InvalidTexturePageEntryType {
            eid: tpag.eid(),
            actual: page.entry_type,
        });
    }

    let page_number = usize::try_from(page.index.get())
        .map_err(|_| FormatError::global("texture-page index does not fit the host"))?;
    let relative = page_number
        .checked_mul(NSF_PAGE_SIZE)
        .ok_or_else(|| FormatError::global("texture-page offset overflows"))?;
    let start = nsf
        .page_data_offset
        .checked_add(relative)
        .ok_or_else(|| FormatError::global("texture-page offset overflows"))?;
    let bytes = checked_slice(nsf_bytes, start, NSF_PAGE_SIZE, "texture page")?;

    let mut reader = Reader::new(bytes);
    let raw_magic = reader.u16_le()?;
    let raw_page_type = reader.u16_le()?;
    let raw_eid = Eid::from_raw(reader.u32_le()?);
    let raw_entry_type = reader.u32_le()?;
    let raw_checksum = reader.u32_le()?;
    if raw_magic != page.magic
        || raw_page_type != page.page_type
        || raw_eid != page.eid
        || raw_entry_type != page.entry_type
        || raw_checksum != page.checksum
    {
        return Err(RetailTextureError::TexturePageSnapshotMismatch(tpag.eid()));
    }
    Ok(ResolvedTexturePage {
        page_index: page.index,
        tpag,
        bytes,
    })
}

/// Produces one exact entry from the compact 24-orientation retail UV map.
///
/// # Errors
///
/// Returns [`RetailTextureError::InvalidUvIndex`] for indices `600..=1023`.
pub fn texture_coordinates(index: u16) -> Result<TextureCoordinates, RetailTextureError> {
    if index >= UV_MAP_ENTRY_COUNT {
        return Err(RetailTextureError::InvalidUvIndex(index));
    }
    let index = usize::from(index);
    let template = UV_TEMPLATES[index / 25];
    let dimensions = index % 25;
    let width = UV_EXTENTS[dimensions % 5];
    let height = UV_EXTENTS[dimensions / 5];
    let mut points = template.map(|corner| corner.coordinate(width, height));
    // The shipped table has one non-templated, degenerate coordinate: entry
    // 372 repeats top-left for both vertices two and three. Preserve that
    // observable byte contract rather than silently "repairing" retail data.
    if index == 372 {
        points[2] = Corner::TopLeft.coordinate(width, height);
    }
    let vertex_count = if template[3] == Corner::Sentinel {
        3
    } else {
        4
    };
    Ok(TextureCoordinates {
        points,
        vertex_count,
    })
}

/// Retail texture-reference failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RetailTextureError {
    /// A selected word or page slice is malformed/truncated.
    Format(FormatError),
    /// The descriptor is a flat-color primitive and has no texture region.
    UntexturedDescriptor,
    /// Packed color mode three has no C1 texture-decoder interpretation.
    InvalidColorMode(u8),
    /// Defensive rejection if a future packed-color reader exposes more than two blend bits.
    InvalidBlendMode(u8),
    /// UV indices `600..=1023` would read beyond the retail map in C.
    InvalidUvIndex(u16),
    /// No texture page in the validated NSF has this EID.
    MissingTexturePage(Eid),
    /// More than one texture page has the requested EID.
    DuplicateTexturePage(Eid),
    /// A type-one NSF page did not identify itself as subsystem TPAG/type five.
    InvalidTexturePageEntryType { eid: Eid, actual: u32 },
    /// Raw bytes no longer match the parsed NSF snapshot supplied by the caller.
    TexturePageSnapshotMismatch(Eid),
    /// Region, CLUT, page length, or pixel conversion failure.
    Texture(TextureError),
}

impl fmt::Display for RetailTextureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format(error) => error.fmt(formatter),
            Self::UntexturedDescriptor => {
                formatter.write_str("flat-color descriptor has no retail texture")
            }
            Self::InvalidColorMode(mode) => {
                write!(formatter, "packed C1 texture color mode {mode} is invalid")
            }
            Self::InvalidBlendMode(mode) => {
                write!(formatter, "packed C1 texture blend mode {mode} is invalid")
            }
            Self::InvalidUvIndex(index) => {
                write!(formatter, "C1 UV-map index {index} is outside 0..600")
            }
            Self::MissingTexturePage(eid) => {
                write!(formatter, "TPAG {eid} is absent from the validated NSF")
            }
            Self::DuplicateTexturePage(eid) => {
                write!(formatter, "TPAG {eid} appears more than once in the NSF")
            }
            Self::InvalidTexturePageEntryType { eid, actual } => write!(
                formatter,
                "texture page {eid} has entry type {actual}; expected TPAG type {TPAG_ENTRY_TYPE}"
            ),
            Self::TexturePageSnapshotMismatch(eid) => {
                write!(
                    formatter,
                    "raw TPAG {eid} bytes do not match parsed metadata"
                )
            }
            Self::Texture(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for RetailTextureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Format(error) => Some(error),
            Self::Texture(error) => Some(error),
            _ => None,
        }
    }
}

impl From<FormatError> for RetailTextureError {
    fn from(error: FormatError) -> Self {
        Self::Format(error)
    }
}

impl From<TextureError> for RetailTextureError {
    fn from(error: TextureError) -> Self {
        Self::Texture(error)
    }
}

fn word_offset(index: usize) -> Result<usize, FormatError> {
    index
        .checked_mul(WORD_BYTES)
        .ok_or_else(|| FormatError::global("WGEO texture-info byte offset overflows"))
}

fn read_word_at(bytes: &[u8], offset: usize) -> Result<u32, FormatError> {
    let mut reader = Reader::with_position(bytes, offset)?;
    reader.u32_le()
}

fn validate_clut(location: ClutLocation, colors: usize) -> Result<(), RetailTextureError> {
    let start = usize::from(location.row)
        .checked_mul(usize::try_from(TEXTURE_PAGE_WORD_WIDTH).unwrap_or(usize::MAX))
        .and_then(|value| value.checked_add(usize::from(location.block_x) * 16))
        .ok_or(TextureError::DimensionsOverflow)?;
    let end = start
        .checked_add(colors)
        .ok_or(TextureError::ClutOutOfBounds { location, colors })?;
    if end > NSF_PAGE_SIZE / 2 {
        return Err(TextureError::ClutOutOfBounds { location, colors }.into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Sentinel,
}

impl Corner {
    const fn coordinate(self, width: u8, height: u8) -> TextureCoordinate {
        match self {
            Self::TopLeft => TextureCoordinate { x: 0, y: 0 },
            Self::TopRight => TextureCoordinate { x: width, y: 0 },
            Self::BottomLeft => TextureCoordinate { x: 0, y: height },
            Self::BottomRight => TextureCoordinate {
                x: width,
                y: height,
            },
            Self::Sentinel => TextureCoordinate { x: 99, y: 99 },
        }
    }
}

use Corner::{BottomLeft as Bl, BottomRight as Br, Sentinel as X};
use Corner::{TopLeft as Tl, TopRight as Tr};

// Each orientation expands over the 5x5 width/height grid. This is exactly the
// 600-entry `uv_map` while retaining its triangle sentinel and vertex order.
const UV_TEMPLATES: [[Corner; 4]; 24] = [
    [Tl, Tr, Bl, Br],
    [Tl, Bl, Tr, Br],
    [Tr, Bl, Tl, X],
    [Tr, Tl, Bl, X],
    [Bl, Tl, Tr, X],
    [Bl, Tr, Tl, X],
    [Tl, Tr, Br, X],
    [Tl, Br, Tr, X],
    [Tr, Br, Tl, Bl],
    [Tr, Tl, Br, Bl],
    [Br, Tl, Tr, X],
    [Br, Tr, Tl, X],
    [Tr, Br, Bl, X],
    [Tr, Bl, Br, X],
    [Br, Bl, Tr, Tl],
    [Br, Tr, Bl, Tl],
    [Bl, Tr, Br, X],
    [Bl, Br, Tr, X],
    [Tl, Br, Bl, X],
    [Tl, Bl, Br, X],
    [Br, Bl, Tl, X],
    [Br, Tl, Bl, X],
    [Bl, Tl, Br, Tr],
    [Bl, Br, Tl, Tr],
];

#[cfg(test)]
mod tests {
    use super::*;
    use crust_formats::stream::{LevelId, parse_nsd, parse_nsf};
    use proptest::prelude::*;

    const PAGE_MAGIC: u16 = 0x1234;
    const PAGE_TYPE_TEXTURE: u16 = 1;

    fn color_word(textured: bool, blend: u8, clut_x: u8) -> u32 {
        (u32::from(textured) << 31) | (u32::from(blend & 3) << 29) | (u32::from(clut_x & 0xf) << 24)
    }

    fn region_word(
        uv_index: u16,
        mode: u8,
        segment: u8,
        offset_x: u8,
        clut_y: u8,
        offset_y: u8,
    ) -> u32 {
        (u32::from(uv_index & 0x03ff) << 22)
            | (u32::from(mode & 3) << 20)
            | (u32::from(segment & 3) << 18)
            | (u32::from(offset_x & 0x1f) << 13)
            | (u32::from(clut_y & 0x7f) << 6)
            | u32::from(offset_y & 0x1f)
    }

    fn info(color: u32, region: u32) -> TextureInfo2 {
        TextureInfo2 {
            color: ColorInfo::from_raw(color),
            region: RegionInfo::from_raw(region),
        }
    }

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn parsed_texture_nsf(pages: &[(Eid, u32)]) -> (Nsf, Vec<u8>) {
        assert!(!pages.is_empty());
        let mut nsd_bytes = vec![0_u8; 0x410];
        put_u32(&mut nsd_bytes, 0x400, u32::try_from(pages.len()).unwrap());
        put_u32(&mut nsd_bytes, 0x404, 1);
        put_u32(&mut nsd_bytes, 0x408, 1);
        put_u32(&mut nsd_bytes, 0x40c, pages[0].0.raw());
        let metadata = parse_nsd(&nsd_bytes, LevelId::CAVE).unwrap();

        let mut nsf_bytes = vec![0_u8; pages.len() * NSF_PAGE_SIZE];
        for (index, (eid, entry_type)) in pages.iter().copied().enumerate() {
            let start = index * NSF_PAGE_SIZE;
            put_u16(&mut nsf_bytes, start, PAGE_MAGIC);
            put_u16(&mut nsf_bytes, start + 2, PAGE_TYPE_TEXTURE);
            put_u32(&mut nsf_bytes, start + 4, eid.raw());
            put_u32(&mut nsf_bytes, start + 8, entry_type);
            put_u32(&mut nsf_bytes, start + 12, u32::try_from(index).unwrap());
        }
        let nsf = parse_nsf(&nsf_bytes, &metadata).unwrap();
        (nsf, nsf_bytes)
    }

    #[test]
    fn parses_static_and_animated_word_streams_exactly() {
        let color = 0xe700_5634_u32;
        let first_region = 0x0123_4567_u32;
        let second_region = 0x89ab_cdef_u32;
        let mut table = Vec::new();
        table.extend_from_slice(&color.to_le_bytes());
        table.extend_from_slice(&first_region.to_le_bytes());
        table.extend_from_slice(&second_region.to_le_bytes());

        assert_eq!(
            TextureInfo2::parse(&table).unwrap(),
            info(color, first_region)
        );
        assert_eq!(
            TextureInfo2::parse_wgeo_table(&table, 0, 1).unwrap(),
            info(color, second_region)
        );
        assert!(TextureInfo2::parse(&table[..7]).is_err());
        assert!(TextureInfo2::parse_wgeo_table(&table, usize::MAX, 0).is_err());
        assert!(TextureInfo2::parse_wgeo_table(&table, 0, usize::MAX).is_err());
    }

    #[test]
    fn generated_uv_map_matches_full_retail_golden() {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        let mut triangles = 0;
        for index in 0..UV_MAP_ENTRY_COUNT {
            let coordinates = texture_coordinates(index).unwrap();
            triangles += usize::from(coordinates.vertex_count == 3);
            for point in coordinates.points {
                for byte in [point.x, point.y] {
                    hash ^= u64::from(byte);
                    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
                }
            }
        }
        assert_eq!(hash, 0x1aa9_ba1e_6c48_7f8a);
        assert_eq!(triangles, 400);
        assert_eq!(texture_coordinates(0).unwrap().pixel_dimensions(), (4, 4));
        assert_eq!(
            texture_coordinates(24).unwrap().pixel_dimensions(),
            (64, 64)
        );
        assert_eq!(texture_coordinates(50).unwrap().vertex_count(), 3);
        assert_eq!(
            texture_coordinates(50).unwrap().points[3],
            TextureCoordinate { x: 99, y: 99 }
        );
        assert!(matches!(
            texture_coordinates(600),
            Err(RetailTextureError::InvalidUvIndex(600))
        ));
    }

    #[test]
    fn derives_exact_region_clut_blend_and_cache_uvs() {
        let reference = RetailTextureReference::new(
            TpagReference::from_raw(0x123),
            info(color_word(true, 2, 7), region_word(0, 0, 1, 2, 11, 3)),
        );
        let layout = reference.layout().unwrap();
        assert_eq!(layout.request.page_id, 0x123);
        assert_eq!(layout.request.color_mode, ColorMode::Indexed4);
        assert_eq!(layout.request.blend_mode, BlendMode::Subtractive);
        assert_eq!(
            layout.request.region,
            TextureRegion::new(272, 12, 4, 4).unwrap()
        );
        assert_eq!(
            layout.clut,
            Some(ClutReference {
                tpag: TpagReference::from_raw(0x123),
                location: ClutLocation {
                    block_x: 7,
                    row: 11,
                },
            })
        );

        let uvs = layout.coordinates.cache_uvs(TextureUvBounds {
            left: 0.25,
            top: 0.2,
            right: 0.75,
            bottom: 0.8,
        });
        for (actual, expected) in
            uvs.into_iter()
                .zip([[0.25, 0.2], [0.75, 0.2], [0.25, 0.8], [0.75, 0.8]])
        {
            assert!((actual[0] - expected[0]).abs() < f32::EPSILON);
            assert!((actual[1] - expected[1]).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn direct_color_ignores_serialized_clut_bits() {
        let layout = RetailTextureReference::new(
            TpagReference::from_raw(0x123),
            info(color_word(true, 3, 15), region_word(0, 2, 0, 0, 127, 1)),
        )
        .layout()
        .unwrap();
        assert_eq!(layout.request.color_mode, ColorMode::Direct15);
        assert_eq!(layout.request.blend_mode, BlendMode::Opaque);
        assert_eq!(
            layout.request.region,
            TextureRegion::new(0, 4, 4, 4).unwrap()
        );
        assert_eq!(layout.request.clut, None);
        assert_eq!(layout.clut, None);
    }

    #[test]
    fn rejects_c_undefined_uv_mode_region_and_clut_cases() {
        let tpag = TpagReference::from_raw(0x123);
        assert!(matches!(
            RetailTextureReference::new(
                tpag,
                info(color_word(false, 0, 0), region_word(0, 0, 0, 0, 0, 0))
            )
            .layout(),
            Err(RetailTextureError::UntexturedDescriptor)
        ));
        assert!(matches!(
            RetailTextureReference::new(
                tpag,
                info(color_word(true, 0, 0), region_word(0, 3, 0, 0, 0, 0))
            )
            .layout(),
            Err(RetailTextureError::InvalidColorMode(3))
        ));
        assert!(matches!(
            RetailTextureReference::new(
                tpag,
                info(color_word(true, 0, 0), region_word(600, 0, 0, 0, 0, 0))
            )
            .layout(),
            Err(RetailTextureError::InvalidUvIndex(600))
        ));
        assert!(matches!(
            RetailTextureReference::new(
                tpag,
                info(color_word(true, 0, 0), region_word(24, 2, 3, 31, 0, 0))
            )
            .layout(),
            Err(RetailTextureError::Texture(
                TextureError::RegionOutOfBounds { .. }
            ))
        ));
        assert!(matches!(
            RetailTextureReference::new(
                tpag,
                info(color_word(true, 0, 15), region_word(0, 1, 0, 0, 127, 0))
            )
            .layout(),
            Err(RetailTextureError::Texture(
                TextureError::ClutOutOfBounds { .. }
            ))
        ));
    }

    #[test]
    fn resolves_exact_whole_page_and_rejects_aliases_or_changed_bytes() {
        let eid = Eid::from_name("tpage").unwrap();
        let tpag = TpagReference::new(eid);
        let (nsf, bytes) = parsed_texture_nsf(&[(eid, TPAG_ENTRY_TYPE)]);
        let resolved = resolve_texture_page(&nsf, &bytes, tpag).unwrap();
        assert_eq!(resolved.page_index, PageIndex::new(0));
        assert_eq!(resolved.cache_page_id(), eid.raw());
        assert_eq!(resolved.bytes().len(), NSF_PAGE_SIZE);
        assert_eq!(&resolved.bytes()[..4], &[0x34, 0x12, 1, 0]);

        let other = TpagReference::new(Eid::from_name("other").unwrap());
        assert!(matches!(
            resolve_texture_page(&nsf, &bytes, other),
            Err(RetailTextureError::MissingTexturePage(_))
        ));

        let (duplicates, duplicate_bytes) =
            parsed_texture_nsf(&[(eid, TPAG_ENTRY_TYPE), (eid, TPAG_ENTRY_TYPE)]);
        assert!(matches!(
            resolve_texture_page(&duplicates, &duplicate_bytes, tpag),
            Err(RetailTextureError::DuplicateTexturePage(_))
        ));

        let (wrong_type, wrong_type_bytes) = parsed_texture_nsf(&[(eid, 4)]);
        assert!(matches!(
            resolve_texture_page(&wrong_type, &wrong_type_bytes, tpag),
            Err(RetailTextureError::InvalidTexturePageEntryType { actual: 4, .. })
        ));

        let mut changed = bytes.clone();
        changed[12] ^= 1;
        assert!(matches!(
            resolve_texture_page(&nsf, &changed, tpag),
            Err(RetailTextureError::TexturePageSnapshotMismatch(_))
        ));
        assert!(matches!(
            resolve_texture_page(&nsf, &bytes[..NSF_PAGE_SIZE - 1], tpag),
            Err(RetailTextureError::Format(_))
        ));
    }

    #[test]
    fn decodes_indexed_region_through_validated_tpag() {
        let eid = Eid::from_name("tpage").unwrap();
        let (nsf, mut bytes) = parsed_texture_nsf(&[(eid, TPAG_ENTRY_TYPE)]);
        let palette_word = usize::try_from(TEXTURE_PAGE_WORD_WIDTH).unwrap() + 1;
        let palette_offset = palette_word * 2;
        bytes[palette_offset..palette_offset + 2].copy_from_slice(&0x001f_u16.to_le_bytes());
        let pixel_row = 4 * usize::try_from(TEXTURE_PAGE_WORD_WIDTH).unwrap() * 2;
        bytes[pixel_row] = 0x11;

        let reference = RetailTextureReference::new(
            TpagReference::new(eid),
            info(color_word(true, 3, 0), region_word(0, 0, 0, 0, 1, 1)),
        );
        let decoded = reference.decode(&nsf, &bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (4, 4));
        assert_eq!(decoded.pixel(0, 0).unwrap().r, 255);
        assert_eq!(decoded.pixel(0, 0).unwrap().a, 255);
    }

    proptest! {
        #[test]
        fn arbitrary_texture_word_slices_and_indices_never_panic(
            bytes in prop::collection::vec(any::<u8>(), 0..96),
            color_word_index in 0_usize..32,
            animation_frame in 0_usize..32,
        ) {
            let _ = TextureInfo2::parse(&bytes);
            let _ = TextureInfo2::parse_wgeo_table(
                &bytes,
                color_word_index,
                animation_frame,
            );
        }

        #[test]
        fn arbitrary_packed_references_produce_only_bounded_layouts(
            color in any::<u32>(),
            region in any::<u32>(),
            tpag in any::<u32>(),
        ) {
            let reference = RetailTextureReference::new(
                TpagReference::from_raw(tpag),
                info(color, region),
            );
            if let Ok(layout) = reference.layout() {
                let request = layout.request;
                prop_assert!(request.region.width > 0);
                prop_assert!(request.region.height > 0);
                prop_assert!(request.region.x + request.region.width <= request.color_mode.page_width());
                prop_assert!(request.region.y + request.region.height <= TEXTURE_PAGE_HEIGHT);
                prop_assert_eq!(request.page_id, tpag);
                prop_assert!(layout.coordinates.vertex_count() == 3 || layout.coordinates.vertex_count() == 4);
                match request.color_mode {
                    ColorMode::Indexed4 | ColorMode::Indexed8 => prop_assert!(request.clut.is_some()),
                    ColorMode::Direct15 => prop_assert!(request.clut.is_none()),
                }
            }
        }
    }
}
