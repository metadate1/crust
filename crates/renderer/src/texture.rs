//! Bounds-checked decoding of PSX texture pages.
//!
//! A C1 texture page is 256 little-endian 16-bit words wide and 128 rows
//! high.  The same 64 KiB storage is viewed as 1024 4-bit pixels, 512 8-bit
//! pixels, or 256 15-bit pixels per row.

use core::fmt;

use crate::command::BlendMode;

/// Size of one decompressed texture page.
pub const TEXTURE_PAGE_BYTES: usize = 0x1_0000;
/// Width of a texture page in 16-bit VRAM words.
pub const TEXTURE_PAGE_WORD_WIDTH: u32 = 256;
/// Height of a texture page in pixels.
pub const TEXTURE_PAGE_HEIGHT: u32 = 128;

/// PSX texture pixel encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ColorMode {
    /// Four-bit palette indices, low nibble first.
    Indexed4 = 0,
    /// Eight-bit palette indices.
    Indexed8 = 1,
    /// Direct BGR555/STP pixels.
    Direct15 = 2,
}

impl ColorMode {
    /// Pixel width of a page when viewed in this mode.
    #[must_use]
    pub const fn page_width(self) -> u32 {
        1024 >> (self as u8)
    }

    /// Number of colors required by an indexed mode.
    #[must_use]
    pub const fn palette_len(self) -> Option<usize> {
        match self {
            Self::Indexed4 => Some(16),
            Self::Indexed8 => Some(256),
            Self::Direct15 => None,
        }
    }
}

/// An integer texture rectangle in decoded-pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl TextureRegion {
    /// Construct a nonempty region.
    ///
    /// # Errors
    ///
    /// Returns [`TextureError::EmptyRegion`] when either dimension is zero.
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Result<Self, TextureError> {
        let region = Self {
            x,
            y,
            width,
            height,
        };
        if width == 0 || height == 0 {
            return Err(TextureError::EmptyRegion);
        }
        Ok(region)
    }

    fn checked_right(self) -> Option<u32> {
        self.x.checked_add(self.width)
    }

    fn checked_bottom(self) -> Option<u32> {
        self.y.checked_add(self.height)
    }
}

/// Location of a CLUT inside the same texture page.
///
/// `block_x` is measured in groups of 16 16-bit colors, matching the packed
/// C1 texture metadata. `row` is a VRAM row within the 128-row page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClutLocation {
    pub block_x: u8,
    pub row: u8,
}

/// Palette source used while decoding an indexed texture.
#[derive(Debug, Clone, Copy)]
pub enum Palette<'a> {
    /// Read the CLUT from the same 64 KiB texture page.
    Page(ClutLocation),
    /// Use caller-provided little-endian-decoded BGR555/STP colors.
    External(&'a [u16]),
}

/// One RGBA8 pixel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Rgba8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba8 {
    /// Pack to the legacy `0xAABBGGRR` integer representation.
    #[must_use]
    pub const fn to_legacy_u32(self) -> u32 {
        u32::from_le_bytes([self.r, self.g, self.b, self.a])
    }
}

/// An owned RGBA8 texture ready for upload to WebGL2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedTexture {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl DecodedTexture {
    fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        Self {
            width,
            height,
            rgba,
        }
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    #[must_use]
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.rgba.len()
    }

    /// Read one decoded pixel.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<Rgba8> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let pixel_index = usize::try_from(y)
            .ok()?
            .checked_mul(usize::try_from(self.width).ok()?)?
            .checked_add(usize::try_from(x).ok()?)?;
        let offset = pixel_index.checked_mul(4)?;
        let bytes = self.rgba.get(offset..offset + 4)?;
        Some(Rgba8 {
            r: bytes[0],
            g: bytes[1],
            b: bytes[2],
            a: bytes[3],
        })
    }

    /// Duplicate edge texels into a border suitable for atlas sampling.
    ///
    /// # Errors
    ///
    /// Returns [`TextureError::DimensionsOverflow`] if the padded allocation
    /// cannot be represented on this target.
    pub fn with_edge_padding(&self, padding: u32) -> Result<Self, TextureError> {
        if padding == 0 {
            return Ok(self.clone());
        }
        let doubled = padding
            .checked_mul(2)
            .ok_or(TextureError::DimensionsOverflow)?;
        let width = self
            .width
            .checked_add(doubled)
            .ok_or(TextureError::DimensionsOverflow)?;
        let height = self
            .height
            .checked_add(doubled)
            .ok_or(TextureError::DimensionsOverflow)?;
        let len = rgba_len(width, height)?;
        let mut rgba = vec![0; len];

        let destination_width =
            usize::try_from(width).map_err(|_| TextureError::DimensionsOverflow)?;
        for destination_y in 0..height {
            let source_y = destination_y.saturating_sub(padding).min(self.height - 1);
            for destination_x in 0..width {
                let source_x = destination_x.saturating_sub(padding).min(self.width - 1);
                let source = self
                    .pixel(source_x, source_y)
                    .ok_or(TextureError::DimensionsOverflow)?;
                let pixel_index = usize::try_from(destination_y)
                    .map_err(|_| TextureError::DimensionsOverflow)?
                    .checked_mul(destination_width)
                    .and_then(|row| row.checked_add(usize::try_from(destination_x).ok()?))
                    .ok_or(TextureError::DimensionsOverflow)?;
                let offset = pixel_index
                    .checked_mul(4)
                    .ok_or(TextureError::DimensionsOverflow)?;
                rgba[offset..offset + 4].copy_from_slice(&[source.r, source.g, source.b, source.a]);
            }
        }
        Ok(Self::from_rgba(width, height, rgba))
    }
}

/// Texture decoding failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextureError {
    InvalidPageLength {
        actual: usize,
    },
    EmptyRegion,
    RegionOutOfBounds {
        region: TextureRegion,
        page_width: u32,
        page_height: u32,
    },
    PaletteRequired {
        mode: ColorMode,
    },
    PaletteTooShort {
        required: usize,
        actual: usize,
    },
    ClutOutOfBounds {
        location: ClutLocation,
        colors: usize,
    },
    DimensionsOverflow,
}

impl fmt::Display for TextureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPageLength { actual } => write!(
                formatter,
                "texture page is {actual} bytes; expected {TEXTURE_PAGE_BYTES}"
            ),
            Self::EmptyRegion => formatter.write_str("texture region is empty"),
            Self::RegionOutOfBounds {
                region,
                page_width,
                page_height,
            } => write!(
                formatter,
                "texture region {region:?} exceeds {page_width}x{page_height} page view"
            ),
            Self::PaletteRequired { mode } => {
                write!(formatter, "{mode:?} texture requires a palette")
            }
            Self::PaletteTooShort { required, actual } => write!(
                formatter,
                "palette has {actual} colors; {required} are required"
            ),
            Self::ClutOutOfBounds { location, colors } => write!(
                formatter,
                "CLUT {location:?} with {colors} colors exceeds texture page"
            ),
            Self::DimensionsOverflow => {
                formatter.write_str("texture dimensions overflow address space")
            }
        }
    }
}

impl std::error::Error for TextureError {}

/// Expand a PSX five-bit color component with the exact legacy rounding.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub const fn expand_five_bit(value: u8) -> u8 {
    let value = value & 0x1f;
    (((value as u16 * 510) + 31) / 62) as u8
}

/// Decode one PSX BGR555/STP word with C1's blend-dependent alpha semantics.
#[must_use]
pub const fn decode_bgr555_stp(pixel: u16, blend: BlendMode) -> Rgba8 {
    let r = expand_five_bit((pixel & 0x1f) as u8);
    let g = expand_five_bit(((pixel >> 5) & 0x1f) as u8);
    let b = expand_five_bit(((pixel >> 10) & 0x1f) as u8);
    let stp = pixel & 0x8000 != 0;
    let black = r == 0 && g == 0 && b == 0;
    let a = match blend {
        BlendMode::Average | BlendMode::Subtractive => {
            if stp {
                0x7f
            } else if black {
                0
            } else {
                0xff
            }
        }
        BlendMode::Additive => {
            if stp || black {
                0
            } else {
                0xff
            }
        }
        BlendMode::Opaque => {
            if !stp && black {
                0
            } else {
                0xff
            }
        }
    };
    Rgba8 { r, g, b, a }
}

/// Decode a validated region of one 64 KiB PSX texture page.
///
/// # Errors
///
/// Returns an error for a non-64 KiB page, empty/out-of-range region, missing
/// or short indexed palette, invalid page CLUT, or allocation-size overflow.
pub fn decode_region(
    page: &[u8],
    mode: ColorMode,
    blend: BlendMode,
    region: TextureRegion,
    palette: Option<Palette<'_>>,
) -> Result<DecodedTexture, TextureError> {
    validate_page_and_region(page, mode, region)?;
    let palette = read_palette(page, mode, palette)?;
    let capacity = rgba_len(region.width, region.height)?;
    let mut rgba = Vec::with_capacity(capacity);

    for y in region.y..region.y + region.height {
        for x in region.x..region.x + region.width {
            let pixel = match mode {
                ColorMode::Indexed4 => {
                    let row_offset =
                        usize::try_from(y).map_err(|_| TextureError::DimensionsOverflow)? * 512;
                    let byte = page[row_offset
                        + usize::try_from(x / 2).map_err(|_| TextureError::DimensionsOverflow)?];
                    let index = if x & 1 == 0 { byte & 0x0f } else { byte >> 4 };
                    palette[usize::from(index)]
                }
                ColorMode::Indexed8 => {
                    let row_offset =
                        usize::try_from(y).map_err(|_| TextureError::DimensionsOverflow)? * 512;
                    let index = page[row_offset
                        + usize::try_from(x).map_err(|_| TextureError::DimensionsOverflow)?];
                    palette[usize::from(index)]
                }
                ColorMode::Direct15 => {
                    let word_index = usize::try_from(y)
                        .map_err(|_| TextureError::DimensionsOverflow)?
                        * usize::try_from(TEXTURE_PAGE_WORD_WIDTH)
                            .map_err(|_| TextureError::DimensionsOverflow)?
                        + usize::try_from(x).map_err(|_| TextureError::DimensionsOverflow)?;
                    read_word(page, word_index)
                }
            };
            let pixel = decode_bgr555_stp(pixel, blend);
            rgba.extend_from_slice(&[pixel.r, pixel.g, pixel.b, pixel.a]);
        }
    }

    debug_assert_eq!(rgba.len(), capacity);
    Ok(DecodedTexture::from_rgba(region.width, region.height, rgba))
}

/// Decode a standalone 8-bit indexed image, as used by loading screens.
///
/// # Errors
///
/// Returns an error for empty/overflowing dimensions, a mismatched index
/// buffer length, or a palette shorter than 256 colors.
pub fn decode_indexed8(
    indices: &[u8],
    width: u32,
    height: u32,
    palette: &[u16],
    blend: BlendMode,
) -> Result<DecodedTexture, TextureError> {
    let pixel_count = pixel_count(width, height)?;
    if width == 0 || height == 0 {
        return Err(TextureError::EmptyRegion);
    }
    if indices.len() != pixel_count {
        return Err(TextureError::InvalidPageLength {
            actual: indices.len(),
        });
    }
    if palette.len() < 256 {
        return Err(TextureError::PaletteTooShort {
            required: 256,
            actual: palette.len(),
        });
    }
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for &index in indices {
        let pixel = decode_bgr555_stp(palette[usize::from(index)], blend);
        rgba.extend_from_slice(&[pixel.r, pixel.g, pixel.b, pixel.a]);
    }
    Ok(DecodedTexture::from_rgba(width, height, rgba))
}

fn validate_page_and_region(
    page: &[u8],
    mode: ColorMode,
    region: TextureRegion,
) -> Result<(), TextureError> {
    if page.len() != TEXTURE_PAGE_BYTES {
        return Err(TextureError::InvalidPageLength { actual: page.len() });
    }
    if region.width == 0 || region.height == 0 {
        return Err(TextureError::EmptyRegion);
    }
    let page_width = mode.page_width();
    if region
        .checked_right()
        .is_none_or(|right| right > page_width)
        || region
            .checked_bottom()
            .is_none_or(|bottom| bottom > TEXTURE_PAGE_HEIGHT)
    {
        return Err(TextureError::RegionOutOfBounds {
            region,
            page_width,
            page_height: TEXTURE_PAGE_HEIGHT,
        });
    }
    Ok(())
}

fn read_palette(
    page: &[u8],
    mode: ColorMode,
    palette: Option<Palette<'_>>,
) -> Result<Vec<u16>, TextureError> {
    let Some(required) = mode.palette_len() else {
        return Ok(Vec::new());
    };
    match palette.ok_or(TextureError::PaletteRequired { mode })? {
        Palette::External(colors) => {
            if colors.len() < required {
                return Err(TextureError::PaletteTooShort {
                    required,
                    actual: colors.len(),
                });
            }
            Ok(colors[..required].to_vec())
        }
        Palette::Page(location) => {
            let start = usize::from(location.row)
                * usize::try_from(TEXTURE_PAGE_WORD_WIDTH).unwrap()
                + usize::from(location.block_x) * 16;
            let end = start
                .checked_add(required)
                .ok_or(TextureError::ClutOutOfBounds {
                    location,
                    colors: required,
                })?;
            if end > TEXTURE_PAGE_BYTES / 2 {
                return Err(TextureError::ClutOutOfBounds {
                    location,
                    colors: required,
                });
            }
            Ok((start..end).map(|word| read_word(page, word)).collect())
        }
    }
}

fn read_word(bytes: &[u8], word_index: usize) -> u16 {
    let offset = word_index * 2;
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn pixel_count(width: u32, height: u32) -> Result<usize, TextureError> {
    usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or(TextureError::DimensionsOverflow)
}

fn rgba_len(width: u32, height: u32) -> Result<usize, TextureError> {
    pixel_count(width, height)?
        .checked_mul(4)
        .ok_or(TextureError::DimensionsOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn blank_page() -> Vec<u8> {
        vec![0; TEXTURE_PAGE_BYTES]
    }

    #[test]
    fn exact_5551_alpha_contract() {
        assert_eq!(decode_bgr555_stp(0, BlendMode::Average).to_legacy_u32(), 0);
        assert_eq!(decode_bgr555_stp(0x801f, BlendMode::Average).a, 0x7f);
        assert_eq!(decode_bgr555_stp(0x001f, BlendMode::Additive).a, 0xff);
        assert_eq!(decode_bgr555_stp(0x801f, BlendMode::Additive).a, 0);
        assert_eq!(decode_bgr555_stp(0x001f, BlendMode::Subtractive).a, 0xff);
        assert_eq!(decode_bgr555_stp(0x801f, BlendMode::Subtractive).a, 0x7f);
        assert_eq!(decode_bgr555_stp(0x801f, BlendMode::Opaque).r, 255);
        assert_eq!(decode_bgr555_stp(0x801f, BlendMode::Opaque).a, 0xff);
    }

    #[test]
    fn five_bit_expansion_matches_retail_rounding() {
        let expected = [
            0, 8, 16, 25, 33, 41, 49, 58, 66, 74, 82, 90, 99, 107, 115, 123, 132, 140, 148, 156,
            165, 173, 181, 189, 197, 206, 214, 222, 230, 239, 247, 255,
        ];
        for (input, expected) in expected.into_iter().enumerate() {
            assert_eq!(
                expand_five_bit(u8::try_from(input).unwrap_or_default()),
                expected
            );
        }
    }

    #[test]
    fn decodes_low_nibble_first_in_four_bit_mode() {
        let mut page = blank_page();
        page[0] = 0x21;
        let mut palette = [0; 16];
        palette[1] = 0x001f;
        palette[2] = 0x03e0;
        let decoded = decode_region(
            &page,
            ColorMode::Indexed4,
            BlendMode::Opaque,
            TextureRegion::new(0, 0, 2, 1).unwrap(),
            Some(Palette::External(&palette)),
        )
        .unwrap();
        assert_eq!(
            decoded.pixel(0, 0).unwrap(),
            decode_bgr555_stp(0x001f, BlendMode::Opaque)
        );
        assert_eq!(
            decoded.pixel(1, 0).unwrap(),
            decode_bgr555_stp(0x03e0, BlendMode::Opaque)
        );
    }

    #[test]
    fn decodes_page_clut_and_direct_pixels_little_endian() {
        let mut page = blank_page();
        // Palette block 1 starts at word 16.
        page[32..34].copy_from_slice(&0x7c00_u16.to_le_bytes());
        page[0] = 0;
        let indexed = decode_region(
            &page,
            ColorMode::Indexed8,
            BlendMode::Opaque,
            TextureRegion::new(0, 0, 1, 1).unwrap(),
            Some(Palette::Page(ClutLocation { block_x: 1, row: 0 })),
        )
        .unwrap();
        assert_eq!(indexed.pixel(0, 0).unwrap().b, 255);

        page[0..2].copy_from_slice(&0x03e0_u16.to_le_bytes());
        let direct = decode_region(
            &page,
            ColorMode::Direct15,
            BlendMode::Opaque,
            TextureRegion::new(0, 0, 1, 1).unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(direct.pixel(0, 0).unwrap().g, 255);
    }

    #[test]
    fn loading_screen_indexed_conversion_is_not_blank() {
        let palette = {
            let mut palette = [0; 256];
            palette[0] = 0x001f;
            palette[1] = 0x03e0;
            palette
        };
        let decoded = decode_indexed8(&[0, 1], 2, 1, &palette, BlendMode::Opaque).unwrap();
        assert_eq!(
            decoded.pixel(0, 0).unwrap(),
            decode_bgr555_stp(palette[0], BlendMode::Opaque)
        );
        assert_eq!(
            decoded.pixel(1, 0).unwrap(),
            decode_bgr555_stp(palette[1], BlendMode::Opaque)
        );
    }

    #[test]
    fn edge_padding_duplicates_all_edges_and_corners() {
        let texture = DecodedTexture::from_rgba(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 255]);
        let padded = texture.with_edge_padding(1).unwrap();
        assert_eq!((padded.width(), padded.height()), (4, 3));
        assert_eq!(padded.pixel(0, 0), texture.pixel(0, 0));
        assert_eq!(padded.pixel(1, 1), texture.pixel(0, 0));
        assert_eq!(padded.pixel(3, 2), texture.pixel(1, 0));
    }

    #[test]
    fn rejects_region_and_clut_overruns() {
        let page = blank_page();
        let region = TextureRegion::new(1023, 127, 2, 1).unwrap();
        assert!(matches!(
            decode_region(
                &page,
                ColorMode::Indexed4,
                BlendMode::Opaque,
                region,
                Some(Palette::Page(ClutLocation { block_x: 0, row: 0 }))
            ),
            Err(TextureError::RegionOutOfBounds { .. })
        ));
        assert!(matches!(
            decode_region(
                &page,
                ColorMode::Indexed8,
                BlendMode::Opaque,
                TextureRegion::new(0, 0, 1, 1).unwrap(),
                Some(Palette::Page(ClutLocation {
                    block_x: 15,
                    row: 127
                }))
            ),
            Err(TextureError::ClutOutOfBounds { .. })
        ));
    }

    proptest! {
        #[test]
        fn arbitrary_regions_never_escape_the_page(
            mode in 0_u8..3,
            x in any::<u32>(),
            y in any::<u32>(),
            width in any::<u16>(),
            height in any::<u16>(),
        ) {
            let mode = match mode {
                0 => ColorMode::Indexed4,
                1 => ColorMode::Indexed8,
                _ => ColorMode::Direct15,
            };
            let page = blank_page();
            let region = TextureRegion { x, y, width: u32::from(width), height: u32::from(height) };
            let palette = [0_u16; 256];
            let palette = mode.palette_len().map(|_| Palette::External(palette.as_slice()));
            let decoded = decode_region(&page, mode, BlendMode::Average, region, palette);
            if let Ok(decoded) = decoded {
                prop_assert!(x.checked_add(u32::from(width)).is_some_and(|right| right <= mode.page_width()));
                prop_assert!(y.checked_add(u32::from(height)).is_some_and(|bottom| bottom <= TEXTURE_PAGE_HEIGHT));
                prop_assert_eq!(decoded.byte_len(), usize::from(width) * usize::from(height) * 4);
            }
        }
    }
}
