//! ISO9660 and raw PlayStation data-track helpers.
//!
//! `SectorLayout::request` and `SectorRequest::decode` deliberately separate
//! physical range selection from sector validation. A browser can request only
//! that range from a `Blob`, then pass the returned bytes to `decode`; native
//! tests and tools can use [`DiscImage`] over a complete borrowed slice.

use core::ops::Range;
use core::str::FromStr;
use std::collections::{HashMap, HashSet};

use crate::binary::{FormatError, Reader, checked_slice};
use crate::stream::{KNOWN_LEVELS, LevelId, StreamKind, StreamName, known_level};

/// ISO9660 logical-block size required by the game disc.
pub const LOGICAL_SECTOR_SIZE: usize = 2_048;
/// Physical sector size of the NTSC-U raw BIN data track.
pub const RAW_SECTOR_SIZE: usize = 2_352;
/// Start of Mode 2 Form 1 user bytes in a raw sector.
pub const RAW_USER_DATA_OFFSET: usize = 24;
const VOLUME_DESCRIPTOR_START: u32 = 16;
const MAX_VOLUME_DESCRIPTORS: u32 = 64;
const MAX_DIRECTORY_BYTES: usize = 16 * 1024 * 1024;

/// Checked byte range suitable for `Blob.slice(start, end)`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ByteRange {
    pub start: u64,
    pub length: u32,
}

impl ByteRange {
    /// Exclusive range end with overflow detection.
    pub fn end(self) -> Result<u64, FormatError> {
        self.start
            .checked_add(u64::from(self.length))
            .ok_or_else(|| FormatError::global("physical byte range overflows u64"))
    }

    /// Converts to a host range after checking the host's address width.
    pub fn as_usize_range(self) -> Result<Range<usize>, FormatError> {
        let start = usize::try_from(self.start)
            .map_err(|_| FormatError::global("physical byte offset does not fit the host"))?;
        let end = usize::try_from(self.end()?)
            .map_err(|_| FormatError::global("physical byte range does not fit the host"))?;
        Ok(start..end)
    }
}

/// Supported physical layouts for the ISO9660 logical sectors.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SectorLayout {
    /// 2,048-byte cooked ISO sectors.
    Cooked2048,
    /// 2,352-byte PlayStation Mode 2 Form 1 sectors.
    RawMode2_2352,
}

impl SectorLayout {
    /// Human-readable stable label used by the loader.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Cooked2048 => "ISO 2048",
            Self::RawMode2_2352 => "MODE2/2352",
        }
    }

    /// Physical bytes per sector.
    #[must_use]
    pub const fn physical_sector_size(self) -> usize {
        match self {
            Self::Cooked2048 => LOGICAL_SECTOR_SIZE,
            Self::RawMode2_2352 => RAW_SECTOR_SIZE,
        }
    }

    /// Number of complete physical sectors in an image of `file_len` bytes.
    #[must_use]
    pub fn sector_count(self, file_len: u64) -> u64 {
        file_len / self.physical_sector_size() as u64
    }

    /// Computes one minimal physical range request for a logical sector.
    pub fn request(self, lba: u32, file_len: u64) -> Result<SectorRequest, FormatError> {
        let sector_size = self.physical_sector_size() as u64;
        if u64::from(lba) >= self.sector_count(file_len) {
            return Err(FormatError::global(format!(
                "logical sector {lba} is outside the selected image"
            )));
        }
        let start = u64::from(lba)
            .checked_mul(sector_size)
            .ok_or_else(|| FormatError::global("physical sector offset overflows"))?;
        Ok(SectorRequest {
            lba,
            layout: self,
            range: ByteRange {
                start,
                length: u32::try_from(self.physical_sector_size())
                    .expect("supported physical sector sizes fit u32"),
            },
        })
    }

    /// Builds browser-friendly physical pieces for an ISO extent.
    pub fn extent_parts(
        self,
        extent_lba: u32,
        byte_len: u32,
        file_len: u64,
    ) -> Result<Vec<ExtentPart>, FormatError> {
        let mut remaining = usize::try_from(byte_len)
            .map_err(|_| FormatError::global("extent length does not fit the host"))?;
        let mut lba = extent_lba;
        let mut parts = Vec::with_capacity(remaining.div_ceil(LOGICAL_SECTOR_SIZE));
        while remaining > 0 {
            let request = self.request(lba, file_len)?;
            let logical_bytes = remaining.min(LOGICAL_SECTOR_SIZE);
            let physical_start = request
                .range
                .start
                .checked_add(match self {
                    Self::Cooked2048 => 0,
                    Self::RawMode2_2352 => RAW_USER_DATA_OFFSET as u64,
                })
                .ok_or_else(|| FormatError::global("extent piece offset overflows"))?;
            parts.push(ExtentPart {
                lba,
                range: ByteRange {
                    start: physical_start,
                    length: u32::try_from(logical_bytes).expect("logical sector bytes fit u32"),
                },
            });
            remaining -= logical_bytes;
            lba = lba
                .checked_add(1)
                .ok_or_else(|| FormatError::global("extent LBA overflows"))?;
        }
        Ok(parts)
    }

    /// Requests the first and last physical sectors of an extent for header validation.
    ///
    /// The raw-disc importer can decode these before retaining zero-copy user-data
    /// slices from [`Self::extent_parts`], matching the source loader's endpoint
    /// integrity check without reading the entire disc into memory.
    pub fn extent_endpoint_requests(
        self,
        extent_lba: u32,
        byte_len: u32,
        file_len: u64,
    ) -> Result<Vec<SectorRequest>, FormatError> {
        if byte_len == 0 {
            return Ok(Vec::new());
        }
        let block_count = u64::from(byte_len).div_ceil(LOGICAL_SECTOR_SIZE as u64);
        let last_delta = u32::try_from(block_count - 1)
            .map_err(|_| FormatError::global("extent block count exceeds u32"))?;
        let last_lba = extent_lba
            .checked_add(last_delta)
            .ok_or_else(|| FormatError::global("extent endpoint LBA overflows"))?;
        let first = self.request(extent_lba, file_len)?;
        if last_lba == extent_lba {
            Ok(vec![first])
        } else {
            Ok(vec![first, self.request(last_lba, file_len)?])
        }
    }
}

/// A physical range request whose returned bytes can be validated and decoded.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SectorRequest {
    pub lba: u32,
    pub layout: SectorLayout,
    pub range: ByteRange,
}

impl SectorRequest {
    /// Validates the response length/header and returns exactly 2,048 user bytes.
    pub fn decode(self, physical_bytes: &[u8]) -> Result<&[u8], FormatError> {
        if physical_bytes.len() != self.layout.physical_sector_size() {
            return Err(FormatError::global(format!(
                "sector {} response has {} bytes; expected {}",
                self.lba,
                physical_bytes.len(),
                self.layout.physical_sector_size()
            )));
        }
        match self.layout {
            SectorLayout::Cooked2048 => Ok(physical_bytes),
            SectorLayout::RawMode2_2352 => {
                validate_raw_mode2_header(physical_bytes, self.lba)?;
                Ok(&physical_bytes
                    [RAW_USER_DATA_OFFSET..RAW_USER_DATA_OFFSET + LOGICAL_SECTOR_SIZE])
            }
        }
    }
}

/// One user-data part of a file extent. Raw BIN pieces skip their 24-byte header.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ExtentPart {
    pub lba: u32,
    pub range: ByteRange,
}

/// Decoded ISO9660 directory record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryRecord {
    pub extent_lba: u32,
    pub data_length: u32,
    pub flags: u8,
    pub volume_sequence: u16,
    pub identifier: String,
}

impl DirectoryRecord {
    #[must_use]
    pub const fn is_directory(&self) -> bool {
        self.flags & 2 != 0
    }

    /// Identifier with a standard `;1` version suffix removed.
    #[must_use]
    pub fn unversioned_identifier(&self) -> &str {
        strip_iso_version(&self.identifier)
    }
}

/// Parsed primary-volume descriptor fields needed for bounded traversal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrimaryVolumeDescriptor {
    pub descriptor_lba: u32,
    pub volume_space_size: u32,
    pub logical_block_size: u16,
    pub root: DirectoryRecord,
}

impl PrimaryVolumeDescriptor {
    /// Validates that a record's complete logical extent stays inside this volume.
    pub fn validate_extent(
        &self,
        record: &DirectoryRecord,
        context: &str,
    ) -> Result<(), FormatError> {
        validate_extent_bounds(record, self.volume_space_size, context)
    }
}

/// Parses one already-decoded 2,048-byte volume-descriptor sector.
///
/// This is the range-reader counterpart to [`DiscImage::open`]. It returns
/// `Ok(None)` for valid non-primary descriptors (including the terminator), so
/// browser code can fetch LBAs 16 onward one at a time until it finds a PVD or
/// reaches type 255. Raw headers must first be checked with
/// [`SectorRequest::decode`].
pub fn parse_primary_volume_descriptor(
    logical_sector: &[u8],
    descriptor_lba: u32,
    layout: SectorLayout,
    file_len: u64,
) -> Result<Option<PrimaryVolumeDescriptor>, FormatError> {
    if logical_sector.len() != LOGICAL_SECTOR_SIZE {
        return Err(FormatError::global(format!(
            "volume descriptor sector has {} bytes; expected 2048",
            logical_sector.len()
        )));
    }
    if !has_iso_signature(logical_sector) {
        return Err(FormatError::global(format!(
            "{} sector {descriptor_lba} is not an ISO9660 volume descriptor",
            layout.label()
        )));
    }
    if logical_sector[0] != 1 {
        return Ok(None);
    }
    let mut block_reader = Reader::with_position(logical_sector, 128)?;
    let logical_block_size = block_reader.both_endian_u16("logical block size")?;
    if usize::from(logical_block_size) != LOGICAL_SECTOR_SIZE {
        return Err(FormatError::at(
            128,
            format!("unsupported ISO9660 logical block size {logical_block_size}"),
        ));
    }
    let mut volume_reader = Reader::with_position(logical_sector, 80)?;
    let volume_space_size = volume_reader.both_endian_u32("volume size")?;
    if volume_space_size == 0 || u64::from(volume_space_size) > layout.sector_count(file_len) {
        return Err(FormatError::at(
            80,
            "ISO9660 volume size exceeds the selected image",
        ));
    }
    let root_length = usize::from(logical_sector[156]);
    let root_bytes = checked_slice(logical_sector, 156, root_length, "root directory record")?;
    let root = parse_directory_record(root_bytes, 156)?;
    if !root.is_directory() || root.identifier != "." {
        return Err(FormatError::at(
            156,
            "primary descriptor has no valid root directory",
        ));
    }
    validate_extent_bounds(&root, volume_space_size, "root directory")?;
    Ok(Some(PrimaryVolumeDescriptor {
        descriptor_lba,
        volume_space_size,
        logical_block_size,
        root,
    }))
}

/// Read-only view of a detected disc image.
#[derive(Clone, Debug)]
pub struct DiscImage<'a> {
    bytes: &'a [u8],
    layout: SectorLayout,
    descriptor: PrimaryVolumeDescriptor,
}

impl<'a> DiscImage<'a> {
    /// Detects raw/cooked layout and validates the primary descriptor and root.
    pub fn open(bytes: &'a [u8]) -> Result<Self, FormatError> {
        let mut failures = Vec::new();
        for layout in [SectorLayout::RawMode2_2352, SectorLayout::Cooked2048] {
            match parse_primary_descriptor(bytes, layout) {
                Ok(descriptor) => {
                    return Ok(Self {
                        bytes,
                        layout,
                        descriptor,
                    });
                }
                Err(error) => failures.push(format!("{}: {error}", layout.label())),
            }
        }
        Err(FormatError::global(format!(
            "image is neither raw MODE2/2352 nor cooked ISO 2048 ({})",
            failures.join("; ")
        )))
    }

    /// Detected physical layout.
    #[must_use]
    pub const fn layout(&self) -> SectorLayout {
        self.layout
    }

    /// Validated primary descriptor.
    #[must_use]
    pub const fn descriptor(&self) -> &PrimaryVolumeDescriptor {
        &self.descriptor
    }

    /// Reads and validates one logical sector without allocation.
    pub fn logical_sector(&self, lba: u32) -> Result<&'a [u8], FormatError> {
        let request = self.layout.request(
            lba,
            u64::try_from(self.bytes.len()).expect("usize fits u64"),
        )?;
        let range = request.range.as_usize_range()?;
        let physical = self
            .bytes
            .get(range)
            .ok_or_else(|| FormatError::global("physical sector range is outside the image"))?;
        request.decode(physical)
    }

    /// Copies exactly the meaningful bytes of a validated ISO extent.
    pub fn read_extent(&self, record: &DirectoryRecord) -> Result<Vec<u8>, FormatError> {
        self.validate_extent(record, "ISO extent")?;
        let length = usize::try_from(record.data_length)
            .map_err(|_| FormatError::global("extent length does not fit the host"))?;
        if length > MAX_DIRECTORY_BYTES && record.is_directory() {
            return Err(FormatError::global("ISO directory is unreasonably large"));
        }
        let mut output = Vec::with_capacity(length);
        let mut lba = record.extent_lba;
        while output.len() < length {
            let sector = self.logical_sector(lba)?;
            let count = (length - output.len()).min(LOGICAL_SECTOR_SIZE);
            output.extend_from_slice(&sector[..count]);
            lba = lba
                .checked_add(1)
                .ok_or_else(|| FormatError::global("extent LBA overflows"))?;
        }
        Ok(output)
    }

    /// Traverses `/S0` through `/S3` and validates every discovered stream.
    pub fn discover_streams(&self) -> Result<DiscStreamSet, FormatError> {
        let root_bytes = self.read_extent(&self.descriptor.root)?;
        let root_records = parse_directory(&root_bytes)?;
        let mut files = Vec::new();
        for (directory, directory_record) in
            find_stream_directories(&self.descriptor, &root_records)?
        {
            let directory_bytes = self.read_extent(&directory_record)?;
            let records = parse_directory(&directory_bytes)?;
            files.extend(discover_stream_directory(
                &self.descriptor,
                directory,
                &records,
            )?);
        }
        DiscStreamSet::from_files(files)
    }

    /// Extracts one discovered stream into owned bytes.
    pub fn read_stream(&self, stream: &DiscStream) -> Result<Vec<u8>, FormatError> {
        self.read_extent(&DirectoryRecord {
            extent_lba: stream.extent_lba,
            data_length: stream.byte_len,
            flags: 0,
            volume_sequence: 1,
            identifier: stream.name.filename(),
        })
    }

    fn validate_extent(&self, record: &DirectoryRecord, context: &str) -> Result<(), FormatError> {
        let blocks = u64::from(record.data_length).div_ceil(LOGICAL_SECTOR_SIZE as u64);
        let start = u64::from(record.extent_lba);
        let end = start
            .checked_add(blocks)
            .ok_or_else(|| FormatError::global(format!("{context} extent overflows")))?;
        if start >= u64::from(self.descriptor.volume_space_size)
            || end > u64::from(self.descriptor.volume_space_size)
        {
            return Err(FormatError::global(format!(
                "{context} extent points outside the ISO9660 volume"
            )));
        }
        Ok(())
    }
}

/// One of the four retail stream directories.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum StreamDirectory {
    S0,
    S1,
    S2,
    S3,
}

impl StreamDirectory {
    pub const ALL: [Self; 4] = [Self::S0, Self::S1, Self::S2, Self::S3];

    #[must_use]
    pub const fn index(self) -> u8 {
        match self {
            Self::S0 => 0,
            Self::S1 => 1,
            Self::S2 => 2,
            Self::S3 => 3,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::S0 => "S0",
            Self::S1 => "S1",
            Self::S2 => "S2",
            Self::S3 => "S3",
        }
    }
}

/// Validated location of one stream in the local disc image.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DiscStream {
    pub directory: StreamDirectory,
    pub name: StreamName,
    pub extent_lba: u32,
    pub byte_len: u32,
}

impl DiscStream {
    /// Minimal physical slices needed to reconstruct this stream in a browser.
    pub fn extent_parts(
        self,
        layout: SectorLayout,
        file_len: u64,
    ) -> Result<Vec<ExtentPart>, FormatError> {
        layout.extent_parts(self.extent_lba, self.byte_len, file_len)
    }

    /// First/last full-sector requests used to validate a zero-copy raw extent.
    pub fn endpoint_sector_requests(
        self,
        layout: SectorLayout,
        file_len: u64,
    ) -> Result<Vec<SectorRequest>, FormatError> {
        layout.extent_endpoint_requests(self.extent_lba, self.byte_len, file_len)
    }
}

/// Sorted, duplicate-free stream discovery result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscStreamSet {
    files: Vec<DiscStream>,
}

impl DiscStreamSet {
    /// Validates, de-duplicates, and sorts staged browser discovery results.
    pub fn from_files(mut files: Vec<DiscStream>) -> Result<Self, FormatError> {
        if files.is_empty() {
            return Err(FormatError::global(
                "the /S0 through /S3 directories contain no NSD/NSF streams",
            ));
        }
        let mut names = HashSet::with_capacity(files.len());
        for file in &files {
            if file.byte_len == 0 {
                return Err(FormatError::global(format!("{} is empty", file.name)));
            }
            if file.name.level().stream_directory_index() != Some(file.directory.index()) {
                return Err(FormatError::global(format!(
                    "{} is stored in /{} instead of its level-id directory",
                    file.name,
                    file.directory.name()
                )));
            }
            if !names.insert(file.name) {
                return Err(FormatError::global(format!(
                    "duplicate stream filename {}",
                    file.name
                )));
            }
        }
        files.sort_by_key(|file| file.name);
        Ok(Self { files })
    }

    /// All discovered files in canonical filename order.
    #[must_use]
    pub fn files(&self) -> &[DiscStream] {
        &self.files
    }

    /// Finds an exact canonical stream.
    #[must_use]
    pub fn get(&self, name: StreamName) -> Option<&DiscStream> {
        self.files
            .binary_search_by_key(&name, |stream| stream.name)
            .ok()
            .map(|index| &self.files[index])
    }

    /// Number of levels with both NSD and NSF present.
    #[must_use]
    pub fn complete_pair_count(&self) -> usize {
        let mut by_level: HashMap<LevelId, u8> = HashMap::new();
        for file in &self.files {
            let bit = match file.name.kind() {
                StreamKind::Nsd => 1,
                StreamKind::Nsf => 2,
            };
            *by_level.entry(file.name.level()).or_default() |= bit;
        }
        by_level.values().filter(|mask| **mask == 3).count()
    }

    /// Requires the exact 88 files / 44 known pairs from the NTSC-U catalog.
    pub fn validate_complete_retail(&self) -> Result<(), FormatError> {
        if self.files.len() != KNOWN_LEVELS.len() * 2 {
            return Err(FormatError::global(format!(
                "retail stream set has {} files; expected 88",
                self.files.len()
            )));
        }
        for file in &self.files {
            if known_level(file.name.level()).is_none() {
                return Err(FormatError::global(format!(
                    "unrecognized stream pair {}",
                    file.name.level()
                )));
            }
        }
        for level in KNOWN_LEVELS {
            for kind in [StreamKind::Nsd, StreamKind::Nsf] {
                let name = StreamName::new(level.id, kind);
                if self.get(name).is_none() {
                    return Err(FormatError::global(format!("missing retail stream {name}")));
                }
            }
        }
        Ok(())
    }
}

/// Locates exactly one `S0`, `S1`, `S2`, and `S3` record in a parsed root.
pub fn find_stream_directories(
    descriptor: &PrimaryVolumeDescriptor,
    root_records: &[DirectoryRecord],
) -> Result<Vec<(StreamDirectory, DirectoryRecord)>, FormatError> {
    let mut result = Vec::with_capacity(4);
    for directory in StreamDirectory::ALL {
        let matches: Vec<_> = root_records
            .iter()
            .filter(|record| {
                record.is_directory()
                    && record
                        .unversioned_identifier()
                        .eq_ignore_ascii_case(directory.name())
            })
            .collect();
        if matches.len() != 1 {
            return Err(FormatError::global(format!(
                "expected exactly one /{} directory, found {}",
                directory.name(),
                matches.len()
            )));
        }
        descriptor.validate_extent(matches[0], directory.name())?;
        result.push((directory, matches[0].clone()));
    }
    Ok(result)
}

/// Validates stream records from one fetched `S0` through `S3` extent.
pub fn discover_stream_directory(
    descriptor: &PrimaryVolumeDescriptor,
    directory: StreamDirectory,
    records: &[DirectoryRecord],
) -> Result<Vec<DiscStream>, FormatError> {
    let mut files = Vec::new();
    let mut names = HashSet::new();
    for record in records {
        if matches!(record.identifier.as_str(), "." | "..") {
            continue;
        }
        let identifier = record.unversioned_identifier();
        let Ok(name) = StreamName::from_str(identifier) else {
            continue;
        };
        if record.is_directory() {
            return Err(FormatError::global(format!(
                "/{}/{} is unexpectedly a directory",
                directory.name(),
                record.identifier
            )));
        }
        if record.data_length == 0 {
            return Err(FormatError::global(format!(
                "/{}/{} is empty",
                directory.name(),
                record.identifier
            )));
        }
        if name.level().stream_directory_index() != Some(directory.index()) {
            return Err(FormatError::global(format!(
                "{} is stored in /{} instead of its level-id directory",
                name,
                directory.name()
            )));
        }
        descriptor.validate_extent(record, identifier)?;
        if !names.insert(name) {
            return Err(FormatError::global(format!(
                "duplicate stream filename {name} within /{}",
                directory.name()
            )));
        }
        files.push(DiscStream {
            directory,
            name,
            extent_lba: record.extent_lba,
            byte_len: record.data_length,
        });
    }
    Ok(files)
}

/// Parses one directory extent, respecting sector-end zero padding.
pub fn parse_directory(bytes: &[u8]) -> Result<Vec<DirectoryRecord>, FormatError> {
    if bytes.len() > MAX_DIRECTORY_BYTES {
        return Err(FormatError::global("ISO directory is unreasonably large"));
    }
    let mut records = Vec::new();
    let mut offset = 0_usize;
    while offset < bytes.len() {
        let length = bytes[offset] as usize;
        if length == 0 {
            let next_sector = (offset / LOGICAL_SECTOR_SIZE + 1)
                .checked_mul(LOGICAL_SECTOR_SIZE)
                .ok_or_else(|| FormatError::at(offset, "directory sector offset overflows"))?;
            offset = next_sector.min(bytes.len());
            continue;
        }
        let bytes_left_in_sector = LOGICAL_SECTOR_SIZE - offset % LOGICAL_SECTOR_SIZE;
        if length > bytes_left_in_sector {
            return Err(FormatError::at(
                offset,
                "ISO directory record crosses a logical-sector boundary",
            ));
        }
        let record_bytes = checked_slice(bytes, offset, length, "ISO directory record")?;
        records.push(parse_directory_record(record_bytes, offset)?);
        offset += length;
    }
    Ok(records)
}

fn parse_primary_descriptor(
    bytes: &[u8],
    layout: SectorLayout,
) -> Result<PrimaryVolumeDescriptor, FormatError> {
    let file_len = u64::try_from(bytes.len()).expect("usize fits u64");
    let mut primary = None;
    for index in 0..MAX_VOLUME_DESCRIPTORS {
        let lba = VOLUME_DESCRIPTOR_START + index;
        let request = layout.request(lba, file_len)?;
        let range = request.range.as_usize_range()?;
        let physical = bytes
            .get(range)
            .ok_or_else(|| FormatError::global("volume descriptor range is outside the image"))?;
        let descriptor = request.decode(physical)?;
        if let Some(parsed) = parse_primary_volume_descriptor(descriptor, lba, layout, file_len)? {
            primary = Some(parsed);
            break;
        }
        if descriptor[0] == 255 {
            break;
        }
    }
    primary.ok_or_else(|| FormatError::global("ISO9660 image has no primary volume descriptor"))
}

fn parse_directory_record(
    bytes: &[u8],
    absolute_offset: usize,
) -> Result<DirectoryRecord, FormatError> {
    if bytes.len() < 34 {
        return Err(FormatError::at(
            absolute_offset,
            "ISO directory record is shorter than 34 bytes",
        ));
    }
    let declared_length = usize::from(bytes[0]);
    if declared_length != bytes.len() {
        return Err(FormatError::at(
            absolute_offset,
            "ISO directory record length is inconsistent",
        ));
    }
    if bytes[1] != 0 {
        return Err(FormatError::at(
            absolute_offset + 1,
            "extended-attribute records are unsupported",
        ));
    }
    if bytes[26] != 0 || bytes[27] != 0 {
        return Err(FormatError::at(
            absolute_offset + 26,
            "interleaved ISO files are unsupported",
        ));
    }
    let flags = bytes[25];
    if flags & 0x80 != 0 {
        return Err(FormatError::at(
            absolute_offset + 25,
            "multi-extent ISO files are unsupported",
        ));
    }
    let identifier_length = usize::from(bytes[32]);
    let minimum_length = 33_usize
        .checked_add(identifier_length)
        .and_then(|length| length.checked_add(usize::from(identifier_length % 2 == 0)))
        .ok_or_else(|| FormatError::at(absolute_offset + 32, "identifier length overflows"))?;
    if identifier_length == 0 || minimum_length > bytes.len() {
        return Err(FormatError::at(
            absolute_offset + 32,
            "ISO directory identifier is truncated",
        ));
    }
    let mut extent_reader = Reader::with_position(bytes, 2)?;
    let extent_lba = extent_reader.both_endian_u32("extent LBA")?;
    let mut length_reader = Reader::with_position(bytes, 10)?;
    let data_length = length_reader.both_endian_u32("extent byte length")?;
    let mut sequence_reader = Reader::with_position(bytes, 28)?;
    let volume_sequence = sequence_reader.both_endian_u16("volume sequence")?;
    if volume_sequence == 0 {
        return Err(FormatError::at(
            absolute_offset + 28,
            "ISO volume sequence is zero",
        ));
    }
    let identifier_bytes = &bytes[33..33 + identifier_length];
    let identifier = if identifier_bytes == [0] {
        ".".to_owned()
    } else if identifier_bytes == [1] {
        "..".to_owned()
    } else {
        if !identifier_bytes
            .iter()
            .all(|byte| (0x20..=0x7e).contains(byte))
        {
            return Err(FormatError::at(
                absolute_offset + 33,
                "ISO filename contains unsupported non-ASCII bytes",
            ));
        }
        String::from_utf8(identifier_bytes.to_vec())
            .map_err(|_| FormatError::at(absolute_offset + 33, "ISO filename is not UTF-8"))?
    };
    Ok(DirectoryRecord {
        extent_lba,
        data_length,
        flags,
        volume_sequence,
        identifier,
    })
}

fn validate_raw_mode2_header(bytes: &[u8], lba: u32) -> Result<(), FormatError> {
    if bytes.len() != RAW_SECTOR_SIZE {
        return Err(FormatError::global(format!(
            "raw sector {lba} is truncated"
        )));
    }
    if bytes[0] != 0 || bytes[11] != 0 || bytes[1..11].iter().any(|byte| *byte != 0xff) {
        return Err(FormatError::global(format!(
            "raw sector {lba} has an invalid CD sync header"
        )));
    }
    if bytes[15] != 2 {
        return Err(FormatError::global(format!(
            "raw sector {lba} is not Mode 2"
        )));
    }
    if bytes[16..20] != bytes[20..24] {
        return Err(FormatError::global(format!(
            "raw sector {lba} has mismatched Mode 2 subheaders"
        )));
    }
    if bytes[18] & 0x20 != 0 {
        return Err(FormatError::global(format!(
            "raw sector {lba} is Mode 2 Form 2 instead of Form 1"
        )));
    }
    Ok(())
}

fn validate_extent_bounds(
    record: &DirectoryRecord,
    volume_space_size: u32,
    context: &str,
) -> Result<(), FormatError> {
    let blocks = u64::from(record.data_length).div_ceil(LOGICAL_SECTOR_SIZE as u64);
    let start = u64::from(record.extent_lba);
    let end = start
        .checked_add(blocks)
        .ok_or_else(|| FormatError::global(format!("{context} extent overflows")))?;
    if start >= u64::from(volume_space_size) || end > u64::from(volume_space_size) {
        return Err(FormatError::global(format!(
            "{context} extent points outside the ISO9660 volume"
        )));
    }
    Ok(())
}

fn has_iso_signature(bytes: &[u8]) -> bool {
    bytes.len() >= 7 && &bytes[1..6] == b"CD001" && bytes[6] == 1
}

fn strip_iso_version(identifier: &str) -> &str {
    let Some((base, version)) = identifier.rsplit_once(';') else {
        return identifier;
    };
    if !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit()) {
        base
    } else {
        identifier
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn write_both_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        bytes[offset + 2..offset + 4].copy_from_slice(&value.to_be_bytes());
    }

    fn write_both_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        bytes[offset + 4..offset + 8].copy_from_slice(&value.to_be_bytes());
    }

    fn directory_record(identifier: &[u8], extent: u32, size: u32, directory: bool) -> Vec<u8> {
        let length = 33 + identifier.len() + usize::from(identifier.len().is_multiple_of(2));
        let mut bytes = vec![0_u8; length];
        bytes[0] = u8::try_from(length).unwrap();
        write_both_u32(&mut bytes, 2, extent);
        write_both_u32(&mut bytes, 10, size);
        bytes[25] = if directory { 2 } else { 0 };
        write_both_u16(&mut bytes, 28, 1);
        bytes[32] = u8::try_from(identifier.len()).unwrap();
        bytes[33..33 + identifier.len()].copy_from_slice(identifier);
        bytes
    }

    fn append_record(sector: &mut [u8], cursor: &mut usize, record: &[u8]) {
        sector[*cursor..*cursor + record.len()].copy_from_slice(record);
        *cursor += record.len();
    }

    fn cooked_retail_iso() -> Vec<u8> {
        const SECTORS: usize = 40;
        let mut iso = vec![0_u8; SECTORS * LOGICAL_SECTOR_SIZE];
        let pvd = &mut iso[16 * LOGICAL_SECTOR_SIZE..17 * LOGICAL_SECTOR_SIZE];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        pvd[6] = 1;
        write_both_u32(pvd, 80, SECTORS as u32);
        write_both_u16(pvd, 128, LOGICAL_SECTOR_SIZE as u16);
        let root_record = directory_record(&[0], 20, LOGICAL_SECTOR_SIZE as u32, true);
        pvd[156..156 + root_record.len()].copy_from_slice(&root_record);

        let terminator = &mut iso[17 * LOGICAL_SECTOR_SIZE..18 * LOGICAL_SECTOR_SIZE];
        terminator[0] = 255;
        terminator[1..6].copy_from_slice(b"CD001");
        terminator[6] = 1;

        let root = &mut iso[20 * LOGICAL_SECTOR_SIZE..21 * LOGICAL_SECTOR_SIZE];
        let mut root_cursor = 0;
        append_record(
            root,
            &mut root_cursor,
            &directory_record(&[0], 20, 2048, true),
        );
        append_record(
            root,
            &mut root_cursor,
            &directory_record(&[1], 20, 2048, true),
        );
        for directory in StreamDirectory::ALL {
            append_record(
                root,
                &mut root_cursor,
                &directory_record(
                    directory.name().as_bytes(),
                    21 + u32::from(directory.index()),
                    2048,
                    true,
                ),
            );
        }

        for directory in StreamDirectory::ALL {
            let start = (21 + usize::from(directory.index())) * LOGICAL_SECTOR_SIZE;
            let sector = &mut iso[start..start + LOGICAL_SECTOR_SIZE];
            let mut cursor = 0;
            append_record(sector, &mut cursor, &directory_record(&[0], 21, 2048, true));
            append_record(sector, &mut cursor, &directory_record(&[1], 20, 2048, true));
            for level in KNOWN_LEVELS
                .iter()
                .filter(|level| level.id.stream_directory_index() == Some(directory.index()))
            {
                for kind in [StreamKind::Nsd, StreamKind::Nsf] {
                    let name = StreamName::new(level.id, kind).filename().to_uppercase() + ";1";
                    append_record(
                        sector,
                        &mut cursor,
                        &directory_record(name.as_bytes(), 30, 1, false),
                    );
                }
            }
        }
        iso
    }

    fn raw_from_cooked(cooked: &[u8]) -> Vec<u8> {
        let mut raw = vec![0_u8; cooked.len() / LOGICAL_SECTOR_SIZE * RAW_SECTOR_SIZE];
        for (lba, sector) in cooked.chunks_exact(LOGICAL_SECTOR_SIZE).enumerate() {
            let output = &mut raw[lba * RAW_SECTOR_SIZE..(lba + 1) * RAW_SECTOR_SIZE];
            output[1..11].fill(0xff);
            output[15] = 2;
            output[16..20].copy_from_slice(&[0, 0, 0, 0]);
            output[20..24].copy_from_slice(&[0, 0, 0, 0]);
            output[24..24 + LOGICAL_SECTOR_SIZE].copy_from_slice(sector);
        }
        raw
    }

    #[test]
    fn detects_and_discovers_exact_retail_catalog() {
        let cooked = cooked_retail_iso();
        let image = DiscImage::open(&cooked).unwrap();
        assert_eq!(image.layout(), SectorLayout::Cooked2048);
        let streams = image.discover_streams().unwrap();
        assert_eq!(streams.files().len(), 88);
        assert_eq!(streams.complete_pair_count(), 44);
        streams.validate_complete_retail().unwrap();

        let raw = raw_from_cooked(&cooked);
        let image = DiscImage::open(&raw).unwrap();
        assert_eq!(image.layout(), SectorLayout::RawMode2_2352);
        image
            .discover_streams()
            .unwrap()
            .validate_complete_retail()
            .unwrap();
    }

    #[test]
    fn range_requests_decode_raw_without_exposing_headers() {
        let cooked = cooked_retail_iso();
        let raw = raw_from_cooked(&cooked);
        let request = SectorLayout::RawMode2_2352
            .request(16, raw.len() as u64)
            .unwrap();
        let range = request.range.as_usize_range().unwrap();
        assert_eq!(
            request.decode(&raw[range]).unwrap(),
            &cooked[16 * 2048..17 * 2048]
        );

        let parts = SectorLayout::RawMode2_2352
            .extent_parts(20, 2050, raw.len() as u64)
            .unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].range.start, 20 * RAW_SECTOR_SIZE as u64 + 24);
        assert_eq!(parts[0].range.length, 2048);
        assert_eq!(parts[1].range.length, 2);

        let endpoints = SectorLayout::RawMode2_2352
            .extent_endpoint_requests(20, 2050, raw.len() as u64)
            .unwrap();
        assert_eq!(
            endpoints
                .iter()
                .map(|request| request.lba)
                .collect::<Vec<_>>(),
            [20, 21]
        );
    }

    #[test]
    fn malformed_raw_and_iso_metadata_are_rejected() {
        let cooked = cooked_retail_iso();
        let mut raw = raw_from_cooked(&cooked);
        raw[16 * RAW_SECTOR_SIZE + 18] = 0x20;
        assert!(DiscImage::open(&raw).is_err());

        let mut cooked = cooked;
        cooked[16 * LOGICAL_SECTOR_SIZE + 84] ^= 1;
        assert!(DiscImage::open(&cooked).is_err());
    }

    #[test]
    fn directory_parser_rejects_records_crossing_sectors() {
        let mut bytes = vec![0_u8; LOGICAL_SECTOR_SIZE + 64];
        let record = directory_record(&[0], 1, 1, true);
        for offset in (0..2_040).step_by(record.len()) {
            bytes[offset..offset + record.len()].copy_from_slice(&record);
        }
        bytes[2_040] = 34;
        assert!(parse_directory(&bytes).is_err());
    }

    proptest! {
        #[test]
        fn arbitrary_directory_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..8192)) {
            let _ = parse_directory(&bytes);
        }

        #[test]
        fn sector_range_math_never_wraps(lba in any::<u32>(), file_len in any::<u64>()) {
            if let Ok(request) = SectorLayout::RawMode2_2352.request(lba, file_len) {
                prop_assert!(request.range.end().unwrap() <= file_len);
            }
        }
    }
}
