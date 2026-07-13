//! Retail title-card composition from MDAT, IPAL, and IMAG entries.
//!
//! The C runtime assembled each 512×240 title card from 16×16 indexed tiles.
//! Every tile selects one of the CLUT items referenced by its MDAT header. This
//! module performs the same lookup through validated EIDs and entry-item ranges;
//! it never relocates serialized references into native pointers.

use core::fmt;

use crust_formats::binary::{EID_ALPHABET, Eid, FormatError, Reader};
use crust_formats::stream::structs::MdatHeader;
use crust_formats::stream::{Nsd, Nsf};

use crate::command::BlendMode;
use crate::texture::{DecodedTexture, decode_bgr555_stp};

pub const TITLE_WIDTH: u32 = 512;
pub const TITLE_HEIGHT: u32 = 240;
const TITLE_WIDTH_USIZE: usize = 512;
const TITLE_HEIGHT_USIZE: usize = 240;
const TITLE_RGBA_BYTES: usize = TITLE_WIDTH_USIZE * TITLE_HEIGHT_USIZE * 4;
const TILE_SIDE: usize = 16;
const TILE_PIXELS: usize = TILE_SIDE * TILE_SIDE;
const IMAG_TILE_BYTES: usize = 4 + TILE_PIXELS;
const CLUT_COLORS: usize = 256;
const CLUT_BYTES: usize = CLUT_COLORS * 2;
const CLUTS_PER_IPAL: usize = 120;

/// One fully composed retail title card plus its source identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleCard {
    pub state: u8,
    pub mdat: Eid,
    pub width_tiles: u8,
    pub height_tiles: u8,
    pub image: DecodedTexture,
}

/// Malformed or incomplete title-card asset graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TitleError {
    InvalidState(u8),
    Format(FormatError),
    WrongEntryType {
        eid: Eid,
        expected: u32,
        actual: u32,
    },
    MissingItem {
        eid: Eid,
        index: usize,
    },
    InvalidDimensions {
        width_tiles: i32,
        height_tiles: i32,
    },
    InvalidPaletteIndex {
        index: u32,
        count: u32,
    },
}

impl fmt::Display for TitleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidState(state) => {
                write!(formatter, "title state {state} exceeds the EID alphabet")
            }
            Self::Format(error) => error.fmt(formatter),
            Self::WrongEntryType {
                eid,
                expected,
                actual,
            } => write!(
                formatter,
                "title asset {eid} has entry type {actual}; expected {expected}"
            ),
            Self::MissingItem { eid, index } => {
                write!(formatter, "title asset {eid} is missing item {index}")
            }
            Self::InvalidDimensions {
                width_tiles,
                height_tiles,
            } => write!(
                formatter,
                "title card is {width_tiles}x{height_tiles} tiles; expected at most 32x15"
            ),
            Self::InvalidPaletteIndex { index, count } => write!(
                formatter,
                "title tile references CLUT {index}, but MDAT declares {count} CLUTs"
            ),
        }
    }
}

impl std::error::Error for TitleError {}

impl From<FormatError> for TitleError {
    fn from(error: FormatError) -> Self {
        Self::Format(error)
    }
}

/// Compose the exact MDAT title state from one validated stream pair.
///
/// # Errors
///
/// Returns [`TitleError`] when the state has no valid MDAT EID, a referenced
/// entry/item is absent or mistyped, dimensions or palette indices are outside
/// their retail bounds, or any source bytes are malformed.
pub fn decode_title_card(
    metadata: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    state: u8,
) -> Result<TitleCard, TitleError> {
    let mdat = title_mdat_eid(state)?;
    let mdat_entry = nsf.resolve_entry(metadata, mdat)?;
    require_type(mdat_entry.eid, mdat_entry.entry_type, 17)?;
    let header_item = mdat_entry.item(0).ok_or(TitleError::MissingItem {
        eid: mdat,
        index: 0,
    })?;
    let header = MdatHeader::parse(header_item.bytes(nsf_bytes)?)?;
    if !(0..=32).contains(&header.width_tiles) || !(0..=15).contains(&header.height_tiles) {
        return Err(TitleError::InvalidDimensions {
            width_tiles: header.width_tiles,
            height_tiles: header.height_tiles,
        });
    }

    let width_tiles =
        usize::try_from(header.width_tiles).map_err(|_| TitleError::InvalidDimensions {
            width_tiles: header.width_tiles,
            height_tiles: header.height_tiles,
        })?;
    let height_tiles =
        usize::try_from(header.height_tiles).map_err(|_| TitleError::InvalidDimensions {
            width_tiles: header.width_tiles,
            height_tiles: header.height_tiles,
        })?;
    let palette_count = u32::try_from(header.palette_count)
        .map_err(|_| TitleError::InvalidPaletteIndex { index: 0, count: 0 })?;
    let mut rgba = vec![0_u8; TITLE_RGBA_BYTES];
    for alpha in rgba.iter_mut().skip(3).step_by(4) {
        *alpha = u8::MAX;
    }

    for tile_x in 0..width_tiles {
        let imag_eid = header.images[tile_x];
        let imag = nsf.resolve_entry(metadata, imag_eid)?;
        require_type(imag.eid, imag.entry_type, 15)?;
        for tile_y in 0..height_tiles {
            let item = imag.item(tile_y).ok_or(TitleError::MissingItem {
                eid: imag_eid,
                index: tile_y,
            })?;
            let bytes = item.bytes(nsf_bytes)?;
            if bytes.len() < IMAG_TILE_BYTES {
                return Err(FormatError::global(format!(
                    "IMAG {imag_eid} item {tile_y} is shorter than {IMAG_TILE_BYTES} bytes"
                ))
                .into());
            }
            let mut reader = Reader::new(bytes);
            let clut_index = reader.u32_le()?;
            if clut_index >= palette_count {
                return Err(TitleError::InvalidPaletteIndex {
                    index: clut_index,
                    count: palette_count,
                });
            }
            let indices = reader.take(TILE_PIXELS)?;
            let palette = read_clut(metadata, nsf, nsf_bytes, &header, clut_index)?;
            composite_tile(&mut rgba, tile_x, tile_y, indices, &palette);
        }
    }

    Ok(TitleCard {
        state,
        mdat,
        width_tiles: u8::try_from(width_tiles).map_err(|_| TitleError::InvalidDimensions {
            width_tiles: header.width_tiles,
            height_tiles: header.height_tiles,
        })?,
        height_tiles: u8::try_from(height_tiles).map_err(|_| TitleError::InvalidDimensions {
            width_tiles: header.width_tiles,
            height_tiles: header.height_tiles,
        })?,
        image: DecodedTexture::from_rgba(TITLE_WIDTH, TITLE_HEIGHT, rgba),
    })
}

fn title_mdat_eid(state: u8) -> Result<Eid, TitleError> {
    let first = *EID_ALPHABET
        .get(usize::from(state))
        .ok_or(TitleError::InvalidState(state))?;
    let name = [first, b'M', b'a', b'p', b'P'];
    let name = core::str::from_utf8(&name)
        .map_err(|_| TitleError::Format(FormatError::global("EID alphabet is not ASCII")))?;
    Eid::from_name(name).map_err(TitleError::Format)
}

fn require_type(eid: Eid, actual: u32, expected: u32) -> Result<(), TitleError> {
    if actual == expected {
        Ok(())
    } else {
        Err(TitleError::WrongEntryType {
            eid,
            expected,
            actual,
        })
    }
}

fn read_clut(
    metadata: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    header: &MdatHeader,
    clut_index: u32,
) -> Result<[u16; CLUT_COLORS], TitleError> {
    let clut_index = usize::try_from(clut_index)
        .map_err(|_| FormatError::global("title CLUT index does not fit host"))?;
    let ipal_index = clut_index / CLUTS_PER_IPAL;
    let item_index = clut_index % CLUTS_PER_IPAL;
    let ipal_eid = *header
        .palettes
        .get(ipal_index)
        .ok_or_else(|| FormatError::global("title CLUT exceeds MDAT IPAL table"))?;
    let ipal = nsf.resolve_entry(metadata, ipal_eid)?;
    require_type(ipal.eid, ipal.entry_type, 18)?;
    let item = ipal.item(item_index).ok_or(TitleError::MissingItem {
        eid: ipal_eid,
        index: item_index,
    })?;
    let bytes = item.bytes(nsf_bytes)?;
    if bytes.len() < CLUT_BYTES {
        return Err(FormatError::global(format!(
            "IPAL {ipal_eid} item {item_index} is shorter than {CLUT_BYTES} bytes"
        ))
        .into());
    }
    let mut reader = Reader::new(bytes);
    let mut palette = [0_u16; CLUT_COLORS];
    for color in &mut palette {
        *color = reader.u16_le()?;
    }
    Ok(palette)
}

fn composite_tile(
    output: &mut [u8],
    tile_x: usize,
    tile_y: usize,
    indices: &[u8],
    palette: &[u16; CLUT_COLORS],
) {
    let output_width = TITLE_WIDTH_USIZE;
    for y in 0..TILE_SIDE {
        for x in 0..TILE_SIDE {
            let source = y * TILE_SIDE + x;
            let destination_pixel =
                (tile_y * TILE_SIDE + y) * output_width + tile_x * TILE_SIDE + x;
            let destination = destination_pixel * 4;
            let mut color =
                decode_bgr555_stp(palette[usize::from(indices[source])], BlendMode::Opaque);
            // `GLDrawImage` disables blending for title cards.
            color.a = u8::MAX;
            output[destination..destination + 4]
                .copy_from_slice(&[color.r, color.g, color.b, color.a]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_names_match_the_retail_title_map() {
        assert_eq!(title_mdat_eid(5).unwrap().name().as_deref(), Some("5MapP"));
        assert_eq!(title_mdat_eid(10).unwrap().name().as_deref(), Some("aMapP"));
        assert_eq!(title_mdat_eid(15).unwrap().name().as_deref(), Some("fMapP"));
        assert_eq!(title_mdat_eid(64), Err(TitleError::InvalidState(64)));
    }

    #[test]
    fn tile_composition_is_opaque_and_uses_psx_color_order() {
        let mut output = vec![0_u8; usize::try_from(TITLE_WIDTH * TITLE_HEIGHT * 4).unwrap()];
        let mut palette = [0_u16; CLUT_COLORS];
        palette[1] = 0x001f;
        let indices = [1_u8; TILE_PIXELS];
        composite_tile(&mut output, 1, 2, &indices, &palette);
        let pixel = ((2 * TILE_SIDE) * usize::try_from(TITLE_WIDTH).unwrap() + TILE_SIDE) * 4;
        assert_eq!(&output[pixel..pixel + 4], &[255, 0, 0, 255]);
    }
}
