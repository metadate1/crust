//! Browser range reader for raw Mode 2/2352 and cooked ISO9660 images.
// Blob sizes/offsets are WebIDL `f64` values. We validate them as finite integral
// values no larger than 2^53-1 before the intentional exact integer casts.
#![allow(dead_code)] // Public loader wiring is added by the web application slice.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::missing_errors_doc
)]

use crust_formats::binary::FormatError;
use crust_formats::disc::{
    ByteRange, DirectoryRecord, DiscStreamSet, LOGICAL_SECTOR_SIZE, PrimaryVolumeDescriptor,
    RAW_SECTOR_SIZE, SectorLayout, discover_stream_directory, find_stream_directories,
    parse_directory, parse_primary_volume_descriptor,
};
use wasm_bindgen::JsValue;
use web_sys::File;

use crate::assets::read_blob;

const VOLUME_DESCRIPTOR_START: u32 = 16;
const MAX_VOLUME_DESCRIPTORS: u32 = 64;
const MAX_SAFE_FILE_BYTES: u64 = 9_007_199_254_740_991;
// Retail's largest stream is under 10 MiB. A 16 MiB ceiling leaves headroom
// while bounding the simultaneous JS ArrayBuffer + Rust Vec allocation.
const MAX_BROWSER_EXTENT_BYTES: u32 = 16 * 1024 * 1024;
const RAW_BATCH_SECTORS: usize = 256;
const COOKED_BATCH_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct DiscDiscovery {
    pub layout: SectorLayout,
    pub descriptor: PrimaryVolumeDescriptor,
    pub streams: DiscStreamSet,
}

/// Detect and index a local disc using small `Blob.slice()` requests. No complete-disc buffer is
/// allocated and no request leaves the browser origin.
pub async fn discover_disc(file: &File) -> Result<DiscDiscovery, JsValue> {
    let file_len = file_length(file)?;
    let mut failures = Vec::new();
    for layout in [SectorLayout::RawMode2_2352, SectorLayout::Cooked2048] {
        match discover_layout(file, file_len, layout).await {
            Ok(discovery) => return Ok(discovery),
            Err(error) => failures.push(format!("{}: {error}", layout.label())),
        }
    }
    Err(JsValue::from_str(&format!(
        "the selected file is not a supported Crash data track ({})",
        failures.join("; ")
    )))
}

async fn discover_layout(
    file: &File,
    file_len: u64,
    layout: SectorLayout,
) -> Result<DiscDiscovery, FormatError> {
    let mut descriptor = None;
    for index in 0..MAX_VOLUME_DESCRIPTORS {
        let lba = VOLUME_DESCRIPTOR_START + index;
        let logical = read_logical_sector_format(file, layout, lba, file_len).await?;
        let descriptor_type = logical.first().copied().unwrap_or_default();
        if let Some(parsed) = parse_primary_volume_descriptor(&logical, lba, layout, file_len)? {
            descriptor = Some(parsed);
            break;
        }
        if descriptor_type == 255 {
            break;
        }
    }
    let descriptor = descriptor
        .ok_or_else(|| FormatError::global("ISO9660 image has no primary volume descriptor"))?;
    let root = read_directory_extent(file, layout, &descriptor, &descriptor.root, file_len).await?;
    let root_records = parse_directory(&root)?;
    let directories = find_stream_directories(&descriptor, &root_records)?;
    let mut files = Vec::new();
    for (directory, record) in directories {
        let bytes = read_directory_extent(file, layout, &descriptor, &record, file_len).await?;
        let records = parse_directory(&bytes)?;
        files.extend(discover_stream_directory(&descriptor, directory, &records)?);
    }
    let streams = DiscStreamSet::from_files(files)?;
    streams.validate_complete_retail()?;
    if let Some(stream) = streams
        .files()
        .iter()
        .find(|stream| stream.byte_len > MAX_BROWSER_EXTENT_BYTES)
    {
        return Err(FormatError::global(format!(
            "{} is too large for the browser runtime",
            stream.name
        )));
    }
    Ok(DiscDiscovery {
        layout,
        descriptor,
        streams,
    })
}

async fn read_directory_extent(
    file: &File,
    layout: SectorLayout,
    descriptor: &PrimaryVolumeDescriptor,
    record: &DirectoryRecord,
    file_len: u64,
) -> Result<Vec<u8>, FormatError> {
    descriptor.validate_extent(record, &record.identifier)?;
    if record.data_length > 16 * 1024 * 1024 {
        return Err(FormatError::global("ISO directory is unreasonably large"));
    }
    read_disc_extent_format(
        file,
        layout,
        record.extent_lba,
        record.data_length,
        file_len,
    )
    .await
}

pub async fn read_disc_extent(
    file: &File,
    layout: SectorLayout,
    extent_lba: u32,
    byte_len: u32,
) -> Result<Vec<u8>, JsValue> {
    let file_len = file_length(file)?;
    read_disc_extent_format(file, layout, extent_lba, byte_len, file_len)
        .await
        .map_err(|error| JsValue::from_str(&error.to_string()))
}

async fn read_disc_extent_format(
    file: &File,
    layout: SectorLayout,
    extent_lba: u32,
    byte_len: u32,
    file_len: u64,
) -> Result<Vec<u8>, FormatError> {
    if byte_len > MAX_BROWSER_EXTENT_BYTES {
        return Err(FormatError::global(format!(
            "disc extent is {byte_len} bytes; browser limit is {MAX_BROWSER_EXTENT_BYTES}"
        )));
    }
    let length = usize::try_from(byte_len)
        .map_err(|_| FormatError::global("disc extent does not fit browser memory"))?;
    if length == 0 {
        return Ok(Vec::new());
    }
    // Checking both endpoint requests proves that every physical byte range
    // below fits in the selected file before any allocation or Blob read.
    layout.extent_endpoint_requests(extent_lba, byte_len, file_len)?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(length)
        .map_err(|error| FormatError::global(format!("could not allocate disc extent: {error}")))?;
    match layout {
        SectorLayout::Cooked2048 => {
            read_cooked_extent(file, extent_lba, length, file_len, &mut output).await?;
        }
        SectorLayout::RawMode2_2352 => {
            read_raw_extent(file, extent_lba, length, file_len, &mut output).await?;
        }
    }
    if output.len() != length {
        return Err(FormatError::global(
            "disc extent was assembled at the wrong size",
        ));
    }
    Ok(output)
}

async fn read_cooked_extent(
    file: &File,
    extent_lba: u32,
    length: usize,
    file_len: u64,
    output: &mut Vec<u8>,
) -> Result<(), FormatError> {
    let first = SectorLayout::Cooked2048.request(extent_lba, file_len)?;
    let mut physical_offset = first.range.start;
    while output.len() < length {
        let count = (length - output.len()).min(COOKED_BATCH_BYTES);
        let range = ByteRange {
            start: physical_offset,
            length: u32::try_from(count)
                .map_err(|_| FormatError::global("cooked batch length exceeds u32"))?,
        };
        let bytes = read_range_format(file, range).await?;
        if bytes.len() != count {
            return Err(FormatError::global("cooked ISO range read was truncated"));
        }
        output.extend_from_slice(&bytes);
        let count_u64 = u64::try_from(count)
            .map_err(|_| FormatError::global("cooked batch length exceeds u64"))?;
        physical_offset = physical_offset
            .checked_add(count_u64)
            .ok_or_else(|| FormatError::global("cooked ISO range offset overflows"))?;
    }
    Ok(())
}

async fn read_raw_extent(
    file: &File,
    extent_lba: u32,
    length: usize,
    file_len: u64,
    output: &mut Vec<u8>,
) -> Result<(), FormatError> {
    let mut lba = extent_lba;
    while output.len() < length {
        let logical_remaining = length - output.len();
        let sector_count = logical_remaining
            .div_ceil(LOGICAL_SECTOR_SIZE)
            .min(RAW_BATCH_SECTORS);
        let first = SectorLayout::RawMode2_2352.request(lba, file_len)?;
        let physical_length = sector_count
            .checked_mul(RAW_SECTOR_SIZE)
            .ok_or_else(|| FormatError::global("raw-sector batch length overflows"))?;
        let range = ByteRange {
            start: first.range.start,
            length: u32::try_from(physical_length)
                .map_err(|_| FormatError::global("raw-sector batch exceeds u32"))?,
        };
        let physical = read_range_format(file, range).await?;
        if physical.len() != physical_length {
            return Err(FormatError::global("raw-sector batch read was truncated"));
        }
        for sector in physical.chunks_exact(RAW_SECTOR_SIZE) {
            let request = SectorLayout::RawMode2_2352.request(lba, file_len)?;
            // Decode validates sync, mode, duplicate subheaders, and Form 1 for
            // every sector, not merely the first/last extent endpoints.
            let logical = request.decode(sector)?;
            let count = (length - output.len()).min(LOGICAL_SECTOR_SIZE);
            output.extend_from_slice(&logical[..count]);
            lba = lba
                .checked_add(1)
                .ok_or_else(|| FormatError::global("disc extent LBA overflows"))?;
            if output.len() == length {
                break;
            }
        }
    }
    Ok(())
}

async fn read_logical_sector_format(
    file: &File,
    layout: SectorLayout,
    lba: u32,
    file_len: u64,
) -> Result<Vec<u8>, FormatError> {
    let request = layout.request(lba, file_len)?;
    let physical = read_range(file, request.range)
        .await
        .map_err(|error| FormatError::global(js_error(&error)))?;
    Ok(request.decode(&physical)?.to_vec())
}

async fn read_range(file: &File, range: ByteRange) -> Result<Vec<u8>, JsValue> {
    let end = range
        .end()
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    let blob = file.slice_with_f64_and_f64(range.start as f64, end as f64)?;
    let bytes = read_blob(&blob).await?;
    if bytes.len() != range.length as usize {
        return Err(JsValue::from_str("browser Blob range read was truncated"));
    }
    Ok(bytes)
}

async fn read_range_format(file: &File, range: ByteRange) -> Result<Vec<u8>, FormatError> {
    read_range(file, range)
        .await
        .map_err(|error| FormatError::global(js_error(&error)))
}

fn file_length(file: &File) -> Result<u64, JsValue> {
    let size = file.size();
    if !size.is_finite() || size <= 0.0 || size > MAX_SAFE_FILE_BYTES as f64 || size.fract() != 0.0
    {
        return Err(JsValue::from_str(
            "the selected disc has an invalid byte length",
        ));
    }
    Ok(size as u64)
}

fn js_error(value: &JsValue) -> String {
    value
        .as_string()
        .unwrap_or_else(|| "browser Blob range read failed".to_owned())
}
