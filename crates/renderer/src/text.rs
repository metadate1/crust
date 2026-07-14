//! Safe formatting, layout, and projection for retail GOOL type-four text.
//!
//! Native GOOL feeds four negative stack arguments to `snprintf`, then
//! interprets a compact `~` control language while indexing a type-three font.
//! This module preserves the authored integer formatting and layout rules but
//! makes every stack access, glyph index, shift, and coordinate operation
//! explicit and checked.

use core::fmt;

use crust_formats::stream::{GoolFontAnimation, GoolGlyph, GoolTextureInfo};

use crate::sprite::{ProjectedSpriteQuad, RetailSpriteTransform, project_retail_fragment};
use crate::texture::Rgba8;

const RETAIL_FORMAT_ARGUMENTS: usize = 4;
const RETAIL_FORMAT_PAYLOAD_LEN: usize = 255;
const RETAIL_SCALE_BASE: i32 = 400;
const RETAIL_BACKDROP_X_MARGIN: i32 = 100;

/// Per-corner modulation values aliased by native `gool_colors.vert_colors`.
pub type RetailTextVertexColors = [[u16; 3]; 4];

/// All validated inputs needed to render one text term.
#[derive(Clone, Copy, Debug)]
pub struct RetailTextProjection<'a> {
    pub term: &'a [u8],
    pub font: &'a GoolFontAnimation,
    /// Native `sp[-2]`, `sp[-3]`, ... in that order. Formatting reads at most
    /// four values; `~pN` can address the first ten.
    pub negative_stack_arguments: &'a [Option<u32>],
    pub transform: RetailSpriteTransform,
    pub shrink: u8,
    pub projection_distance: u32,
    pub object_size: i32,
    /// Status-B bit `0x400`: measure the widest line and offset every line by
    /// half that width before drawing.
    pub center_by_width: bool,
    /// `GOOL_FLAG_STRING_CENTER` (`0x0400_0000`): track textured glyph bounds
    /// and emit the optional font backdrop.
    pub center_backdrop: bool,
    pub vertex_colors: RetailTextVertexColors,
}

/// Provenance for one projected type-four quad.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetailTextQuadKind {
    Glyph { character: u8, glyph_index: u8 },
    Backdrop,
}

/// One projected glyph or backdrop, retaining texture and per-corner color.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectedRetailTextQuad {
    pub source_part: u16,
    pub kind: RetailTextQuadKind,
    pub texture: GoolTextureInfo,
    pub projected: ProjectedSpriteQuad,
    pub colors: [Rgba8; 4],
}

/// Deterministic result of formatting and projecting one retail text term.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedRetailText {
    /// Exact post-`snprintf`, post-trailing-space-trim payload consumed by the
    /// control-language pass. It never exceeds 255 bytes.
    pub formatted: Vec<u8>,
    /// Maximum X2 measured from an initial X of zero, before optional centering.
    pub measured_width: i32,
    pub quads: Vec<ProjectedRetailTextQuad>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RetailTextError {
    MissingFormatSpecifier(usize),
    UnsupportedFormatSpecifier {
        offset: usize,
        specifier: u8,
    },
    FormatWidthOverflow(usize),
    MissingStackArgument(usize),
    TruncatedControl {
        offset: usize,
        command: u8,
    },
    InvalidPluralArgument {
        offset: usize,
        selector: u8,
    },
    MissingPluralArgument(usize),
    UnterminatedScale(usize),
    ScaleValueOverflow(usize),
    ShiftOutOfRange(u8),
    MissingLineHeight,
    InvalidGlyph {
        offset: usize,
        character: u8,
        glyph_count: usize,
    },
    CoordinateOverflow,
    SourcePartOverflow(usize),
}

impl fmt::Display for RetailTextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingFormatSpecifier(offset) => {
                write!(
                    formatter,
                    "GOOL text format at byte {offset} has no conversion"
                )
            }
            Self::UnsupportedFormatSpecifier { offset, specifier } => write!(
                formatter,
                "GOOL text format at byte {offset} uses unsupported conversion {:?}",
                char::from(*specifier)
            ),
            Self::FormatWidthOverflow(offset) => {
                write!(
                    formatter,
                    "GOOL text format width at byte {offset} overflows"
                )
            }
            Self::MissingStackArgument(index) => write!(
                formatter,
                "GOOL text requires missing native sp[-{}] argument",
                index + 2
            ),
            Self::TruncatedControl { offset, command } => write!(
                formatter,
                "GOOL text control {:?} at byte {offset} is truncated",
                char::from(*command)
            ),
            Self::InvalidPluralArgument { offset, selector } => write!(
                formatter,
                "GOOL text plural selector {:?} at byte {offset} is not decimal",
                char::from(*selector)
            ),
            Self::MissingPluralArgument(index) => write!(
                formatter,
                "GOOL text plural command requires missing native sp[-{}] argument",
                index + 2
            ),
            Self::UnterminatedScale(offset) => {
                write!(
                    formatter,
                    "GOOL text scale at byte {offset} has no closing '~'"
                )
            }
            Self::ScaleValueOverflow(offset) => {
                write!(formatter, "GOOL text scale at byte {offset} exceeds i32")
            }
            Self::ShiftOutOfRange(shift) => {
                write!(
                    formatter,
                    "GOOL text scale shift {shift} exceeds signed coordinates"
                )
            }
            Self::MissingLineHeight => {
                formatter.write_str("GOOL text newline requires font glyph zero")
            }
            Self::InvalidGlyph {
                offset,
                character,
                glyph_count,
            } => write!(
                formatter,
                "GOOL text byte {offset} character 0x{character:02x} is outside {glyph_count} glyphs"
            ),
            Self::CoordinateOverflow => {
                formatter.write_str("GOOL text layout exceeds signed coordinates")
            }
            Self::SourcePartOverflow(part) => {
                write!(formatter, "GOOL text source part {part} exceeds u16")
            }
        }
    }
}

impl std::error::Error for RetailTextError {}

/// Applies retail's four-argument `snprintf` subset and 256-byte buffer cap.
///
/// The legally characterized corpus uses `%d` and `%02d`; `%u`, `%x`, `%X`,
/// `%c`, and `%%` are also defined without accepting native pointer formats.
/// The returned payload is truncated to 255 bytes, stops at an embedded NUL,
/// and has trailing ASCII spaces removed exactly like `GoolTextObjectTransform`.
///
/// # Errors
///
/// Returns a controlled error for malformed/unsupported conversions, widths
/// that overflow the host, or a required negative-stack word that is absent.
pub fn format_retail_text(
    term: &[u8],
    negative_stack_arguments: &[Option<u32>],
) -> Result<Vec<u8>, RetailTextError> {
    let mut output = Vec::with_capacity(term.len().min(RETAIL_FORMAT_PAYLOAD_LEN));
    let mut cursor = 0_usize;
    let mut argument_index = 0_usize;
    while cursor < term.len() {
        let byte = term[cursor];
        if byte != b'%' {
            push_limited(&mut output, byte);
            cursor += 1;
            continue;
        }

        let format_offset = cursor;
        cursor += 1;
        let Some(&first) = term.get(cursor) else {
            return Err(RetailTextError::MissingFormatSpecifier(format_offset));
        };
        if first == b'%' {
            push_limited(&mut output, b'%');
            cursor += 1;
            continue;
        }

        let zero_pad = first == b'0';
        if zero_pad {
            cursor += 1;
        }
        let mut width = 0_usize;
        while let Some(&digit) = term.get(cursor).filter(|digit| digit.is_ascii_digit()) {
            width = width
                .checked_mul(10)
                .and_then(|value| value.checked_add(usize::from(digit - b'0')))
                .ok_or(RetailTextError::FormatWidthOverflow(format_offset))?;
            cursor += 1;
        }
        let Some(&specifier) = term.get(cursor) else {
            return Err(RetailTextError::MissingFormatSpecifier(format_offset));
        };
        cursor += 1;
        if !matches!(specifier, b'd' | b'u' | b'x' | b'X' | b'c') {
            return Err(RetailTextError::UnsupportedFormatSpecifier {
                offset: format_offset,
                specifier,
            });
        }
        if argument_index >= RETAIL_FORMAT_ARGUMENTS {
            return Err(RetailTextError::MissingStackArgument(argument_index));
        }
        let argument = negative_stack_arguments
            .get(argument_index)
            .copied()
            .flatten()
            .ok_or(RetailTextError::MissingStackArgument(argument_index))?;
        argument_index += 1;
        let rendered = match specifier {
            b'd' => argument.cast_signed().to_string().into_bytes(),
            b'u' => argument.to_string().into_bytes(),
            b'x' => format!("{argument:x}").into_bytes(),
            b'X' => format!("{argument:X}").into_bytes(),
            b'c' => vec![argument.to_le_bytes()[0]],
            _ => unreachable!("validated conversion"),
        };
        append_padded(&mut output, &rendered, width, zero_pad && specifier != b'c');
    }

    if let Some(nul) = output.iter().position(|byte| *byte == 0) {
        output.truncate(nul);
    }
    while output.last() == Some(&b' ') {
        output.pop();
    }
    Ok(output)
}

/// Formats, interprets, and projects one type-four term with no raw pointers.
///
/// # Errors
///
/// Returns a controlled error for malformed controls, absent stack aliases,
/// invalid font indices, oversized shifts, or signed coordinate overflow.
pub fn project_retail_text(
    input: RetailTextProjection<'_>,
) -> Result<ProjectedRetailText, RetailTextError> {
    let formatted = format_retail_text(input.term, input.negative_stack_arguments)?;
    let layout = layout_text(
        &formatted,
        input.font,
        input.negative_stack_arguments,
        input.shrink,
    )?;
    let center_offset = if input.center_by_width {
        layout
            .measured_width
            .checked_neg()
            .ok_or(RetailTextError::CoordinateOverflow)?
            / 2
    } else {
        0
    };
    let mut quads = Vec::new();
    let mut backdrop_bounds: Option<[i32; 4]> = None;
    let mut source_part = 0_usize;

    for glyph in layout.glyphs {
        if !glyph.glyph.has_texture() {
            continue;
        }
        let bounds = offset_x(glyph.bounds, center_offset)?;
        if input.center_backdrop {
            backdrop_bounds = Some(expand_bounds(backdrop_bounds, bounds));
        }
        let part = u16::try_from(source_part)
            .map_err(|_| RetailTextError::SourcePartOverflow(source_part))?;
        source_part += 1;
        let Some(projected) = project_retail_fragment(
            input.transform,
            bounds,
            input.projection_distance,
            input.object_size,
        ) else {
            continue;
        };
        quads.push(ProjectedRetailTextQuad {
            source_part: part,
            kind: RetailTextQuadKind::Glyph {
                character: glyph.character,
                glyph_index: u8::try_from(glyph.glyph_index).map_err(|_| {
                    RetailTextError::InvalidGlyph {
                        offset: glyph.offset,
                        character: glyph.character,
                        glyph_count: input.font.glyphs.len(),
                    }
                })?,
            },
            texture: glyph.glyph.texture,
            projected,
            colors: glyph_colors(glyph.glyph.texture, glyph.gouraud, input.vertex_colors),
        });
    }

    if input.center_backdrop
        && let (Some(backdrop), Some(bounds)) = (input.font.backdrop, backdrop_bounds)
    {
        let bounds = [
            bounds[0]
                .checked_sub(RETAIL_BACKDROP_X_MARGIN)
                .ok_or(RetailTextError::CoordinateOverflow)?,
            bounds[1],
            bounds[2]
                .checked_add(RETAIL_BACKDROP_X_MARGIN)
                .ok_or(RetailTextError::CoordinateOverflow)?,
            bounds[3],
        ];
        let part = u16::try_from(source_part)
            .map_err(|_| RetailTextError::SourcePartOverflow(source_part))?;
        let backdrop_size = input
            .object_size
            .checked_sub(10)
            .ok_or(RetailTextError::CoordinateOverflow)?;
        if let Some(projected) = project_retail_fragment(
            input.transform,
            bounds,
            input.projection_distance,
            backdrop_size,
        ) {
            quads.push(ProjectedRetailTextQuad {
                source_part: part,
                kind: RetailTextQuadKind::Backdrop,
                texture: backdrop.texture,
                projected,
                colors: [flat_color(backdrop.texture); 4],
            });
        }
    }

    Ok(ProjectedRetailText {
        formatted,
        measured_width: layout.measured_width,
        quads,
    })
}

#[derive(Clone, Copy, Debug)]
struct GlyphLayout {
    offset: usize,
    character: u8,
    glyph_index: usize,
    glyph: GoolGlyph,
    bounds: [i32; 4],
    gouraud: bool,
}

#[derive(Debug)]
struct TextLayout {
    measured_width: i32,
    glyphs: Vec<GlyphLayout>,
}

fn layout_text(
    text: &[u8],
    font: &GoolFontAnimation,
    arguments: &[Option<u32>],
    shrink: u8,
) -> Result<TextLayout, RetailTextError> {
    let base_scale = shift_i32(RETAIL_SCALE_BASE, shrink)?;
    let mut cursor = 0_usize;
    let mut x = 0_i32;
    let mut y_offset = 0_i32;
    let mut measured_width = 0_i32;
    let mut gouraud = true;
    let mut glyphs = Vec::with_capacity(text.len());

    while cursor < text.len() {
        let control_offset = cursor;
        let mut character = text[cursor];
        cursor += 1;
        let mut scale_x = base_scale;
        let mut scale_y = base_scale;
        if character == b'~' {
            let Some(&command) = text.get(cursor) else {
                return Err(RetailTextError::TruncatedControl {
                    offset: control_offset,
                    command: 0,
                });
            };
            cursor += 1;
            character = command;
            match command {
                b'n' | b'%' => {
                    character = take_control_byte(text, &mut cursor, control_offset, command)?;
                    x = 0;
                    let line_height = font
                        .glyphs
                        .first()
                        .ok_or(RetailTextError::MissingLineHeight)?
                        .height;
                    y_offset = y_offset
                        .checked_sub(shift_i32(i32::from(line_height), shrink)?)
                        .ok_or(RetailTextError::CoordinateOverflow)?;
                }
                b'p' => {
                    let selector = take_control_byte(text, &mut cursor, control_offset, command)?;
                    if !selector.is_ascii_digit() {
                        return Err(RetailTextError::InvalidPluralArgument {
                            offset: control_offset,
                            selector,
                        });
                    }
                    let argument_index = usize::from(selector - b'0');
                    let count = arguments
                        .get(argument_index)
                        .copied()
                        .flatten()
                        .ok_or(RetailTextError::MissingPluralArgument(argument_index))?;
                    if count == 1 {
                        if cursor >= text.len() {
                            return Err(RetailTextError::TruncatedControl {
                                offset: control_offset,
                                command,
                            });
                        }
                        cursor += 1;
                    }
                    let Some(&next) = text.get(cursor) else {
                        break;
                    };
                    cursor += 1;
                    character = next;
                    if character == b'~' {
                        cursor -= 1;
                        continue;
                    }
                }
                b's' => {
                    let axis = take_control_byte(text, &mut cursor, control_offset, command)?;
                    let value_start = cursor;
                    let delimiter = text[value_start..]
                        .iter()
                        .position(|byte| *byte == b'~')
                        .and_then(|relative| value_start.checked_add(relative))
                        .ok_or(RetailTextError::UnterminatedScale(control_offset))?;
                    let value = parse_atoi(&text[value_start..delimiter], control_offset)?;
                    cursor = delimiter + 1;
                    if axis == b'x' {
                        scale_x = shift_i32(value, shrink)?;
                    } else {
                        scale_y = shift_i32(value, shrink)?;
                    }
                    character = take_control_byte(text, &mut cursor, control_offset, command)?;
                    if character == b'~' {
                        cursor -= 1;
                        continue;
                    }
                }
                b'c' => {
                    let enabled = take_control_byte(text, &mut cursor, control_offset, command)?;
                    gouraud = enabled == b'1';
                    character = take_control_byte(text, &mut cursor, control_offset, command)?;
                    if character == b'~' {
                        cursor -= 1;
                        continue;
                    }
                }
                _ => {}
            }
        }

        let glyph_index = usize::from(character.checked_sub(0x20).ok_or(
            RetailTextError::InvalidGlyph {
                offset: control_offset,
                character,
                glyph_count: font.glyphs.len(),
            },
        )?);
        let glyph = font
            .glyphs
            .get(glyph_index)
            .copied()
            .ok_or(RetailTextError::InvalidGlyph {
                offset: control_offset,
                character,
                glyph_count: font.glyphs.len(),
            })?;
        let x2 = x
            .checked_add(scale_x)
            .ok_or(RetailTextError::CoordinateOverflow)?;
        let y2 = y_offset
            .checked_add(shift_i32(i32::from(glyph.height), shrink)?)
            .ok_or(RetailTextError::CoordinateOverflow)?;
        let y1 = y2
            .checked_sub(scale_y)
            .ok_or(RetailTextError::CoordinateOverflow)?;
        measured_width = measured_width.max(x2);
        glyphs.push(GlyphLayout {
            offset: control_offset,
            character,
            glyph_index,
            glyph,
            bounds: [x, y1, x2, y2],
            gouraud,
        });
        x = x
            .checked_add(shift_i32(i32::from(glyph.width), shrink)?)
            .ok_or(RetailTextError::CoordinateOverflow)?;
    }

    Ok(TextLayout {
        measured_width,
        glyphs,
    })
}

fn take_control_byte(
    text: &[u8],
    cursor: &mut usize,
    offset: usize,
    command: u8,
) -> Result<u8, RetailTextError> {
    let byte = text
        .get(*cursor)
        .copied()
        .ok_or(RetailTextError::TruncatedControl { offset, command })?;
    *cursor += 1;
    Ok(byte)
}

fn parse_atoi(bytes: &[u8], offset: usize) -> Result<i32, RetailTextError> {
    let mut cursor = 0_usize;
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    let negative = match bytes.get(cursor) {
        Some(b'-') => {
            cursor += 1;
            true
        }
        Some(b'+') => {
            cursor += 1;
            false
        }
        _ => false,
    };
    let mut value = 0_i64;
    let mut found_digit = false;
    while let Some(&digit) = bytes.get(cursor).filter(|digit| digit.is_ascii_digit()) {
        found_digit = true;
        value = value
            .checked_mul(10)
            .and_then(|current| current.checked_add(i64::from(digit - b'0')))
            .ok_or(RetailTextError::ScaleValueOverflow(offset))?;
        cursor += 1;
    }
    if !found_digit {
        return Ok(0);
    }
    if negative {
        value = value
            .checked_neg()
            .ok_or(RetailTextError::ScaleValueOverflow(offset))?;
    }
    i32::try_from(value).map_err(|_| RetailTextError::ScaleValueOverflow(offset))
}

fn shift_i32(value: i32, shift: u8) -> Result<i32, RetailTextError> {
    let shifted = i64::from(value)
        .checked_shl(u32::from(shift))
        .ok_or(RetailTextError::ShiftOutOfRange(shift))?;
    i32::try_from(shifted).map_err(|_| RetailTextError::ShiftOutOfRange(shift))
}

fn offset_x(mut bounds: [i32; 4], offset: i32) -> Result<[i32; 4], RetailTextError> {
    bounds[0] = bounds[0]
        .checked_add(offset)
        .ok_or(RetailTextError::CoordinateOverflow)?;
    bounds[2] = bounds[2]
        .checked_add(offset)
        .ok_or(RetailTextError::CoordinateOverflow)?;
    Ok(bounds)
}

fn expand_bounds(current: Option<[i32; 4]>, next: [i32; 4]) -> [i32; 4] {
    current.map_or(next, |current| {
        [
            current[0].min(next[0]),
            current[1].min(next[1]),
            current[2].max(next[2]),
            current[3].max(next[3]),
        ]
    })
}

fn flat_color(texture: GoolTextureInfo) -> Rgba8 {
    Rgba8 {
        r: texture.color.red(),
        g: texture.color.green(),
        b: texture.color.blue(),
        a: u8::MAX,
    }
}

fn glyph_colors(
    texture: GoolTextureInfo,
    gouraud: bool,
    vertex_colors: RetailTextVertexColors,
) -> [Rgba8; 4] {
    if !gouraud {
        return [flat_color(texture); 4];
    }
    std::array::from_fn(|index| Rgba8 {
        r: modulated_channel(texture.color.red(), vertex_colors[index][0]),
        g: modulated_channel(texture.color.green(), vertex_colors[index][1]),
        b: modulated_channel(texture.color.blue(), vertex_colors[index][2]),
        a: u8::MAX,
    })
}

fn modulated_channel(channel: u8, intensity: u16) -> u8 {
    ((u32::from(channel) * u32::from(intensity)) >> 8).to_le_bytes()[0]
}

fn push_limited(output: &mut Vec<u8>, byte: u8) {
    if output.len() < RETAIL_FORMAT_PAYLOAD_LEN {
        output.push(byte);
    }
}

fn append_padded(output: &mut Vec<u8>, rendered: &[u8], width: usize, zero_pad: bool) {
    let padding = width.saturating_sub(rendered.len());
    if zero_pad && rendered.first() == Some(&b'-') {
        push_limited(output, b'-');
        for _ in 0..padding {
            push_limited(output, b'0');
        }
        for &byte in &rendered[1..] {
            push_limited(output, byte);
        }
        return;
    }
    for _ in 0..padding {
        push_limited(output, if zero_pad { b'0' } else { b' ' });
    }
    for &byte in rendered {
        push_limited(output, byte);
    }
}

#[cfg(test)]
mod tests {
    use crust_formats::binary::Eid;
    use crust_formats::stream::structs::{ColorInfo, RegionInfo};
    use crust_formats::stream::{
        GoolAnimationHeader, GoolAnimationKind, GoolFontAnimation, GoolFragment, GoolGlyph,
        GoolTextureInfo,
    };
    use proptest::prelude::*;

    use super::*;
    use crate::command::ScreenPoint;
    use crate::sprite::{RetailSpriteTransform, RetailSpriteVectors};

    fn texture(raw: u32) -> GoolTextureInfo {
        GoolTextureInfo {
            color: ColorInfo::from_raw(raw),
            region: RegionInfo::from_raw(0),
        }
    }

    fn font() -> GoolFontAnimation {
        let mut glyphs = vec![
            GoolGlyph {
                texture: texture(0),
                width: 100,
                height: 100,
            };
            63
        ];
        for character in *b"AB" {
            glyphs[usize::from(character - 0x20)].texture = texture(0x0030_2010);
        }
        GoolFontAnimation {
            header: GoolAnimationHeader {
                kind: GoolAnimationKind::Font,
                reserved_1: 0,
                length: 63,
                reserved_3: 0,
            },
            texture_page: Eid::from_name("font1").unwrap(),
            glyphs,
            backdrop: Some(GoolFragment {
                texture: texture(0x0060_5040),
                bounds: [0; 4],
            }),
        }
    }

    fn projection<'a>(term: &'a [u8], font: &'a GoolFontAnimation) -> RetailTextProjection<'a> {
        RetailTextProjection {
            term,
            font,
            negative_stack_arguments: &[Some(2); 10],
            transform: RetailSpriteTransform::screen_2d(
                RetailSpriteVectors {
                    translation: [0, 0, 0],
                    rotation_yxz: [0, 0, 0],
                    scale: [0x1000; 3],
                },
                0,
                500,
            )
            .unwrap(),
            shrink: 0,
            projection_distance: 500,
            object_size: 0,
            center_by_width: false,
            center_backdrop: false,
            vertex_colors: [
                [256, 128, 64],
                [128, 256, 64],
                [64, 128, 256],
                [256, 256, 256],
            ],
        }
    }

    #[test]
    fn formats_signed_width_plural_source_and_trims_like_retail() {
        let arguments = [Some(7), Some((-7_i32).cast_unsigned()), Some(0), Some(0)];
        assert_eq!(
            format_retail_text(b"%02d/%03d %%   ", &arguments).unwrap(),
            b"07/-07 %"
        );
    }

    #[test]
    fn formatting_is_capped_at_the_native_payload_and_rejects_pointer_formats() {
        assert_eq!(
            format_retail_text(&vec![b'A'; 300], &[]).unwrap().len(),
            RETAIL_FORMAT_PAYLOAD_LEN
        );
        assert_eq!(
            format_retail_text(b"%s", &[Some(1)]),
            Err(RetailTextError::UnsupportedFormatSpecifier {
                offset: 0,
                specifier: b's'
            })
        );
    }

    #[test]
    fn glyph_projection_uses_center_offset_gouraud_alias_and_backdrop_depth() {
        let font = font();
        let mut input = projection(b"AB", &font);
        input.center_by_width = true;
        input.center_backdrop = true;
        let rendered = project_retail_text(input).unwrap();
        assert_eq!(rendered.formatted, b"AB");
        assert_eq!(rendered.measured_width, 500);
        assert_eq!(rendered.quads.len(), 3);
        assert_eq!(
            rendered.quads[0].kind,
            RetailTextQuadKind::Glyph {
                character: b'A',
                glyph_index: 33
            }
        );
        assert_eq!(
            rendered.quads[0].colors[0],
            Rgba8 {
                r: 16,
                g: 16,
                b: 12,
                a: 255,
            }
        );
        assert_eq!(
            rendered.quads[0].colors[3],
            Rgba8 {
                r: 16,
                g: 32,
                b: 48,
                a: 255,
            }
        );
        assert!(matches!(
            rendered.quads[2].kind,
            RetailTextQuadKind::Backdrop
        ));
        assert_eq!(
            rendered.quads[0].projected.vertices[0],
            ScreenPoint {
                x: -250,
                y: -63,
                z: 500
            }
        );
        assert_eq!(
            rendered.quads[2].projected.ordering_depth + 10,
            rendered.quads[0].projected.ordering_depth
        );
    }

    #[test]
    fn controls_scale_toggle_color_newline_and_pluralize_without_oob_reads() {
        let font = font();
        let mut singular = projection(b"~sx800~A~c0B~nA~p0Z", &font);
        singular.negative_stack_arguments = &[Some(1); 10];
        let rendered = project_retail_text(singular).unwrap();
        assert_eq!(rendered.quads.len(), 3);
        assert_eq!(
            rendered.quads[1].colors,
            [flat_color(texture(0x0030_2010)); 4]
        );

        let invalid = projection(b"~p9A", &font);
        assert_eq!(
            project_retail_text(RetailTextProjection {
                negative_stack_arguments: &[None; 10],
                ..invalid
            }),
            Err(RetailTextError::MissingPluralArgument(9))
        );
    }

    proptest! {
        #[test]
        fn arbitrary_format_and_control_bytes_never_escape_checked_results(
            term in prop::collection::vec(any::<u8>(), 0..300),
            argument in any::<u32>(),
        ) {
            let font = font();
            let arguments = [Some(argument); 10];
            let _ = format_retail_text(&term, &arguments);
            let input = RetailTextProjection {
                negative_stack_arguments: &arguments,
                ..projection(&term, &font)
            };
            let _ = project_retail_text(input);
        }
    }
}
