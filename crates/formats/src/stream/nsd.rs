use core::ops::Range;

use crate::binary::{Eid, FormatError, Offset, PageIndex, PageRef, Reader, checked_slice};

use super::LevelId;

const BUCKET_COUNT: usize = 256;
const LEGACY_HEADER_SIZE: usize = 0x408;
const MODERN_HEADER_SIZE: usize = 0x520;
const PAGE_COUNT_OFFSET: usize = 0x400;
const PAGE_TABLE_SIZE_OFFSET: usize = 0x404;
const LDAT_EID_OFFSET: usize = 0x408;
const HAS_LOADING_IMAGE_OFFSET: usize = 0x40c;
const LOADING_IMAGE_WIDTH_OFFSET: usize = 0x410;
const LOADING_IMAGE_HEIGHT_OFFSET: usize = 0x414;
const PAGES_SECTOR_OFFSET: usize = 0x418;
const COMPRESSED_PAGE_COUNT_OFFSET: usize = 0x41c;
const COMPRESSED_PAGE_OFFSETS_OFFSET: usize = 0x420;
const COMPRESSED_PAGE_CAPACITY: usize = 64;
const PTE_SIZE: usize = 8;
const MAX_PAGE_COUNT: u32 = 128;
const LOGICAL_SECTOR_SIZE: usize = 0x800;

/// Bytes before `nsd_ldat.image_data` in the exact 32-bit disk layout.
pub const LDAT_PREFIX_SIZE: usize = 0x118;
/// Capacity of the retail loading-image field after the LDAT prefix.
pub const LDAT_IMAGE_CAPACITY: usize = 0xf5f8;
const LOADING_IMAGE_PALETTE_SIZE: usize = 512;
const LOADING_IMAGE_MAX_WIDTH: u32 = 512;
const LOADING_IMAGE_MAX_HEIGHT: u32 = 240;

/// Fixed NSD header values shared by playable and index-only streams.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NsdHeader {
    pub bucket_offsets: [u32; BUCKET_COUNT],
    pub page_count: u32,
    pub page_table_len: u32,
    pub page_table_offset: Offset,
    pub ldat_eid: Option<Eid>,
    pub loading_image_flag: u32,
    pub loading_image_width: u32,
    pub loading_image_height: u32,
    pub pages_sector_offset: Option<u32>,
    pub compressed_page_offsets: Vec<u32>,
}

impl NsdHeader {
    /// Byte offset in the NSF at which the uncompressed 64 KiB pages begin.
    pub fn nsf_page_data_offset(&self) -> Result<usize, FormatError> {
        let sectors = self.pages_sector_offset.unwrap_or(0);
        usize::try_from(sectors)
            .ok()
            .and_then(|value| value.checked_mul(LOGICAL_SECTOR_SIZE))
            .ok_or_else(|| FormatError::global("NSF page-data offset overflows the host size"))
    }
}

/// One unresolved eight-byte NSD page-table entry.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NsdPte {
    pub page: PageRef,
    pub eid: Eid,
}

impl NsdPte {
    /// Untagged page index. Parsing guarantees this variant is present.
    #[must_use]
    pub const fn page_index(self) -> PageIndex {
        match self.page {
            PageRef::Page(index) => index,
            PageRef::Offset(_) => unreachable!(),
        }
    }
}

/// Decoded playable LDAT prefix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ldat {
    pub level: LevelId,
    pub spawn_zone: Eid,
    pub spawn_path_index: i32,
    pub unknown_10: u32,
    pub executable_map: [Eid; 64],
    pub field_of_view: u32,
    image_data: Range<usize>,
}

impl Ldat {
    /// Absolute byte range of the fixed-capacity loading-image field.
    #[must_use]
    pub fn image_data_range(&self) -> Range<usize> {
        self.image_data.clone()
    }
}

/// Whether an NSD contains a bootable LDAT or Cave's legacy index-only table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NsdKind {
    /// Exact legacy `0x408 + table_len * 8` Cave archive.
    IndexOnlyCave,
    /// Modern NSD followed by a validated LDAT prefix.
    Playable(Box<Ldat>),
}

/// Validated metadata stream. It contains only values and offsets, never pointers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Nsd {
    pub header: NsdHeader,
    pub page_table: Vec<NsdPte>,
    pub kind: NsdKind,
}

impl Nsd {
    /// Expected level id for this metadata object.
    #[must_use]
    pub fn level(&self) -> LevelId {
        match &self.kind {
            NsdKind::IndexOnlyCave => LevelId::CAVE,
            NsdKind::Playable(ldat) => ldat.level,
        }
    }

    /// Whether this pair may be passed to the game boot flow.
    #[must_use]
    pub const fn is_bootable(&self) -> bool {
        matches!(self.kind, NsdKind::Playable(_))
    }

    /// Returns the validated playable-level descriptor.
    #[must_use]
    pub fn ldat(&self) -> Option<&Ldat> {
        match &self.kind {
            NsdKind::Playable(ldat) => Some(ldat),
            NsdKind::IndexOnlyCave => None,
        }
    }

    /// Looks up an EID through the exact 256-bucket retail page table.
    ///
    /// The original runtime stored a relocated pointer at each bucket head and
    /// scanned forward until the matching EID. Empty retail buckets can share
    /// a head, so the safe equivalent scans from that validated head to the end
    /// of the table and returns the still-unrelocated record.
    #[must_use]
    pub fn pte(&self, eid: Eid) -> Option<&NsdPte> {
        let bucket = usize::from(((eid.raw() >> 15) & 0xff) as u8);
        let start = usize::try_from(self.header.bucket_offsets[bucket]).ok()?;
        self.page_table
            .get(start..)?
            .iter()
            .find(|pte| pte.eid == eid)
    }

    /// Validates the companion NSF's total length without reading its contents.
    pub fn validate_nsf_len(&self, nsf_len: usize) -> Result<(), FormatError> {
        let prefix = self.header.nsf_page_data_offset()?;
        let pages = usize::try_from(self.header.page_count)
            .map_err(|_| FormatError::global("page count does not fit the host size"))?;
        let page_bytes = pages
            .checked_mul(super::NSF_PAGE_SIZE)
            .ok_or_else(|| FormatError::global("NSF page byte count overflows"))?;
        let expected = prefix
            .checked_add(page_bytes)
            .ok_or_else(|| FormatError::global("NSF expected length overflows"))?;
        if nsf_len != expected {
            return Err(FormatError::global(format!(
                "NSF length is {nsf_len} bytes; expected exactly {expected}"
            )));
        }
        Ok(())
    }

    /// Borrows the validated loading-image field from the original NSD bytes.
    pub fn image_data<'a>(&self, bytes: &'a [u8]) -> Result<Option<&'a [u8]>, FormatError> {
        let NsdKind::Playable(ldat) = &self.kind else {
            return Ok(None);
        };
        if self.header.loading_image_flag == 0 {
            return Ok(None);
        }
        let range = ldat.image_data_range();
        checked_slice(bytes, range.start, range.len(), "LDAT image data").map(Some)
    }
}

/// Parses one NSD and verifies that its embedded level matches the filename.
pub fn parse_nsd(bytes: &[u8], expected_level: LevelId) -> Result<Nsd, FormatError> {
    if bytes.len() < LEGACY_HEADER_SIZE {
        return Err(FormatError::global(
            "NSD is shorter than its 0x408-byte base header",
        ));
    }
    let bucket_offsets = parse_bucket_offsets(bytes)?;
    let page_count = read_u32_at(bytes, PAGE_COUNT_OFFSET, "NSD page count")?;
    let page_table_len = read_u32_at(bytes, PAGE_TABLE_SIZE_OFFSET, "NSD page-table length")?;
    if page_count == 0 || page_count > MAX_PAGE_COUNT {
        return Err(FormatError::at(
            PAGE_COUNT_OFFSET,
            "NSD page count is outside 1..=128",
        ));
    }
    if page_table_len == 0 {
        return Err(FormatError::at(
            PAGE_TABLE_SIZE_OFFSET,
            "NSD page table is empty",
        ));
    }
    for (index, offset) in bucket_offsets.iter().copied().enumerate() {
        if offset >= page_table_len {
            return Err(FormatError::at(
                index * 4,
                "NSD bucket offset is outside the page table",
            ));
        }
    }
    if !bucket_offsets.windows(2).all(|pair| pair[0] <= pair[1]) {
        return Err(FormatError::global("NSD bucket offsets are not monotonic"));
    }

    let table_bytes = usize::try_from(page_table_len)
        .ok()
        .and_then(|count| count.checked_mul(PTE_SIZE))
        .ok_or_else(|| FormatError::at(PAGE_TABLE_SIZE_OFFSET, "NSD page table size overflows"))?;
    let legacy_end = LEGACY_HEADER_SIZE
        .checked_add(table_bytes)
        .ok_or_else(|| FormatError::global("legacy NSD table end overflows"))?;
    if expected_level == LevelId::CAVE && bytes.len() == legacy_end {
        let page_table = parse_page_table(bytes, LEGACY_HEADER_SIZE, page_table_len, page_count)?;
        return Ok(Nsd {
            header: NsdHeader {
                bucket_offsets,
                page_count,
                page_table_len,
                page_table_offset: Offset::new(LEGACY_HEADER_SIZE as u32),
                ldat_eid: None,
                loading_image_flag: 0,
                loading_image_width: 0,
                loading_image_height: 0,
                pages_sector_offset: None,
                compressed_page_offsets: Vec::new(),
            },
            page_table,
            kind: NsdKind::IndexOnlyCave,
        });
    }
    if expected_level == LevelId::CAVE {
        return Err(FormatError::global(
            "Cave NSD is not the exact legacy index-only layout",
        ));
    }
    if bytes.len() < MODERN_HEADER_SIZE {
        return Err(FormatError::global(
            "playable NSD is shorter than its 0x520-byte header",
        ));
    }

    let ldat_eid = Eid::from_raw(read_u32_at(bytes, LDAT_EID_OFFSET, "LDAT EID")?);
    let loading_image_flag = read_u32_at(bytes, HAS_LOADING_IMAGE_OFFSET, "loading-image flag")?;
    let loading_image_width =
        read_u32_at(bytes, LOADING_IMAGE_WIDTH_OFFSET, "loading-image width")?;
    let loading_image_height =
        read_u32_at(bytes, LOADING_IMAGE_HEIGHT_OFFSET, "loading-image height")?;
    let pages_sector_offset = read_u32_at(bytes, PAGES_SECTOR_OFFSET, "NSF page sector offset")?;
    let compressed_page_count =
        read_u32_at(bytes, COMPRESSED_PAGE_COUNT_OFFSET, "compressed-page count")?;
    if compressed_page_count as usize > COMPRESSED_PAGE_CAPACITY {
        return Err(FormatError::at(
            COMPRESSED_PAGE_COUNT_OFFSET,
            "compressed-page count exceeds its 64-entry table",
        ));
    }
    let mut compressed_page_offsets = Vec::with_capacity(compressed_page_count as usize);
    for index in 0..compressed_page_count as usize {
        compressed_page_offsets.push(read_u32_at(
            bytes,
            COMPRESSED_PAGE_OFFSETS_OFFSET + index * 4,
            "compressed-page offset",
        )?);
    }
    if !compressed_page_offsets
        .windows(2)
        .all(|pair| pair[0] <= pair[1])
    {
        return Err(FormatError::at(
            COMPRESSED_PAGE_OFFSETS_OFFSET,
            "compressed-page offsets are not monotonic",
        ));
    }

    let page_table = parse_page_table(bytes, MODERN_HEADER_SIZE, page_table_len, page_count)?;
    let ldat_offset = MODERN_HEADER_SIZE
        .checked_add(table_bytes)
        .ok_or_else(|| FormatError::global("LDAT offset overflows"))?;
    let ldat = parse_ldat(
        bytes,
        ldat_offset,
        expected_level,
        loading_image_flag,
        loading_image_width,
        loading_image_height,
    )?;
    Ok(Nsd {
        header: NsdHeader {
            bucket_offsets,
            page_count,
            page_table_len,
            page_table_offset: Offset::new(MODERN_HEADER_SIZE as u32),
            ldat_eid: Some(ldat_eid),
            loading_image_flag,
            loading_image_width,
            loading_image_height,
            pages_sector_offset: Some(pages_sector_offset),
            compressed_page_offsets,
        },
        page_table,
        kind: NsdKind::Playable(Box::new(ldat)),
    })
}

fn parse_bucket_offsets(bytes: &[u8]) -> Result<[u32; BUCKET_COUNT], FormatError> {
    let mut reader = Reader::new(checked_slice(bytes, 0, BUCKET_COUNT * 4, "NSD buckets")?);
    let mut result = [0_u32; BUCKET_COUNT];
    for offset in &mut result {
        *offset = reader.u32_le()?;
    }
    Ok(result)
}

fn parse_page_table(
    bytes: &[u8],
    offset: usize,
    length: u32,
    page_count: u32,
) -> Result<Vec<NsdPte>, FormatError> {
    let count = usize::try_from(length)
        .map_err(|_| FormatError::at(offset, "page-table length does not fit the host size"))?;
    let table_len = count
        .checked_mul(PTE_SIZE)
        .ok_or_else(|| FormatError::at(offset, "page-table byte length overflows"))?;
    let mut reader = Reader::new(checked_slice(bytes, offset, table_len, "NSD page table")?);
    let mut result = Vec::with_capacity(count);
    for index in 0..count {
        let raw_page = reader.u32_le()?;
        let page = PageRef::from_raw(raw_page);
        let PageRef::Page(page_index) = page else {
            return Err(FormatError::at(
                offset + index * PTE_SIZE,
                "on-disk NSD PTE contains an even relocated page reference",
            ));
        };
        if page_index.get() >= page_count {
            return Err(FormatError::at(
                offset + index * PTE_SIZE,
                "NSD PTE page index is outside the level page count",
            ));
        }
        result.push(NsdPte {
            page,
            eid: Eid::from_raw(reader.u32_le()?),
        });
    }
    Ok(result)
}

fn parse_ldat(
    bytes: &[u8],
    offset: usize,
    expected_level: LevelId,
    loading_image_flag: u32,
    loading_image_width: u32,
    loading_image_height: u32,
) -> Result<Ldat, FormatError> {
    let prefix = checked_slice(bytes, offset, LDAT_PREFIX_SIZE, "LDAT prefix")?;
    let mut reader = Reader::new(prefix);
    let magic = reader.u32_le()?;
    if magic != 1 {
        return Err(FormatError::at(offset, "LDAT magic is not 1"));
    }
    let level_raw = reader.u32_le()?;
    let level = LevelId::new(level_raw)?;
    if level != expected_level {
        return Err(FormatError::at(
            offset + 4,
            format!("LDAT level {level} does not match expected {expected_level}"),
        ));
    }
    let spawn_zone = Eid::from_raw(reader.u32_le()?);
    let spawn_path_index = reader.i32_le()?;
    let unknown_10 = reader.u32_le()?;
    let mut executable_map = [Eid::from_raw(0); 64];
    for eid in &mut executable_map {
        *eid = Eid::from_raw(reader.u32_le()?);
    }
    let field_of_view = reader.u32_le()?;
    let image_start = offset + LDAT_PREFIX_SIZE;
    let image_data = if loading_image_flag == 0 {
        image_start..image_start
    } else {
        if loading_image_width == 0
            || loading_image_height == 0
            || loading_image_width > LOADING_IMAGE_MAX_WIDTH
            || loading_image_height > LOADING_IMAGE_MAX_HEIGHT
        {
            return Err(FormatError::at(
                HAS_LOADING_IMAGE_OFFSET,
                "loading-image dimensions are zero or exceed 512x240",
            ));
        }
        let pixels = usize::try_from(loading_image_width)
            .ok()
            .and_then(|width| {
                usize::try_from(loading_image_height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .ok_or_else(|| {
                FormatError::at(LOADING_IMAGE_WIDTH_OFFSET, "loading-image size overflows")
            })?;
        if pixels > LDAT_IMAGE_CAPACITY - LOADING_IMAGE_PALETTE_SIZE {
            return Err(FormatError::at(
                LOADING_IMAGE_WIDTH_OFFSET,
                "loading image and palette exceed the retail LDAT field",
            ));
        }
        checked_slice(bytes, image_start, LDAT_IMAGE_CAPACITY, "LDAT image data")?;
        image_start..image_start + LDAT_IMAGE_CAPACITY
    };
    Ok(Ldat {
        level,
        spawn_zone,
        spawn_path_index,
        unknown_10,
        executable_map,
        field_of_view,
        image_data,
    })
}

fn read_u32_at(bytes: &[u8], offset: usize, context: &'static str) -> Result<u32, FormatError> {
    let raw: [u8; 4] = checked_slice(bytes, offset, 4, context)?
        .try_into()
        .expect("slice length was checked");
    Ok(u32::from_le_bytes(raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::NSF_PAGE_SIZE;
    use proptest::prelude::*;

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn modern_nsd(with_image: bool) -> Vec<u8> {
        let table_len = 1_usize;
        let ldat_offset = MODERN_HEADER_SIZE + table_len * PTE_SIZE;
        let image_len = if with_image { LDAT_IMAGE_CAPACITY } else { 0 };
        let mut bytes = vec![0_u8; ldat_offset + LDAT_PREFIX_SIZE + image_len];
        put_u32(&mut bytes, PAGE_COUNT_OFFSET, 1);
        put_u32(&mut bytes, PAGE_TABLE_SIZE_OFFSET, table_len as u32);
        put_u32(
            &mut bytes,
            LDAT_EID_OFFSET,
            Eid::from_name("0MapP").unwrap().raw(),
        );
        put_u32(&mut bytes, HAS_LOADING_IMAGE_OFFSET, u32::from(with_image));
        put_u32(
            &mut bytes,
            LOADING_IMAGE_WIDTH_OFFSET,
            if with_image { 16 } else { 0 },
        );
        put_u32(
            &mut bytes,
            LOADING_IMAGE_HEIGHT_OFFSET,
            if with_image { 16 } else { 0 },
        );
        put_u32(&mut bytes, PAGES_SECTOR_OFFSET, 0);
        put_u32(&mut bytes, MODERN_HEADER_SIZE, PageIndex::new(0).tagged());
        put_u32(
            &mut bytes,
            MODERN_HEADER_SIZE + 4,
            Eid::from_name("testZ").unwrap().raw(),
        );
        put_u32(&mut bytes, ldat_offset, 1);
        put_u32(&mut bytes, ldat_offset + 4, LevelId::TITLE.get());
        put_u32(
            &mut bytes,
            ldat_offset + 8,
            Eid::from_name("zoneZ").unwrap().raw(),
        );
        put_u32(&mut bytes, ldat_offset + 0x114, 55);
        bytes
    }

    #[test]
    fn parses_playable_short_and_image_ldat() {
        let short = modern_nsd(false);
        let nsd = parse_nsd(&short, LevelId::TITLE).unwrap();
        assert!(nsd.is_bootable());
        assert_eq!(nsd.header.page_count, 1);
        assert_eq!(nsd.page_table[0].page_index(), PageIndex::new(0));
        assert!(nsd.image_data(&short).unwrap().is_none());
        assert!(nsd.validate_nsf_len(NSF_PAGE_SIZE).is_ok());

        let with_image = modern_nsd(true);
        let nsd = parse_nsd(&with_image, LevelId::TITLE).unwrap();
        assert_eq!(
            nsd.image_data(&with_image).unwrap().unwrap().len(),
            LDAT_IMAGE_CAPACITY
        );
    }

    #[test]
    fn parses_exact_cave_legacy_layout() {
        let mut bytes = vec![0_u8; LEGACY_HEADER_SIZE + PTE_SIZE];
        put_u32(&mut bytes, PAGE_COUNT_OFFSET, 1);
        put_u32(&mut bytes, PAGE_TABLE_SIZE_OFFSET, 1);
        put_u32(&mut bytes, LEGACY_HEADER_SIZE, 1);
        put_u32(
            &mut bytes,
            LEGACY_HEADER_SIZE + 4,
            Eid::from_name("caveZ").unwrap().raw(),
        );
        let nsd = parse_nsd(&bytes, LevelId::CAVE).unwrap();
        assert_eq!(nsd.kind, NsdKind::IndexOnlyCave);
        assert!(!nsd.is_bootable());
        assert!(nsd.validate_nsf_len(NSF_PAGE_SIZE).is_ok());

        bytes.push(0);
        assert!(parse_nsd(&bytes, LevelId::CAVE).is_err());
    }

    #[test]
    fn rejects_bad_tables_and_embedded_level() {
        let mut bytes = modern_nsd(false);
        put_u32(&mut bytes, 0, 1);
        assert!(parse_nsd(&bytes, LevelId::TITLE).is_err());

        let mut bytes = modern_nsd(false);
        put_u32(&mut bytes, MODERN_HEADER_SIZE, 3);
        assert!(parse_nsd(&bytes, LevelId::TITLE).is_err());

        let mut bytes = modern_nsd(false);
        let ldat_offset = MODERN_HEADER_SIZE + PTE_SIZE;
        put_u32(&mut bytes, ldat_offset + 4, LevelId::INTRO.get());
        assert!(parse_nsd(&bytes, LevelId::TITLE).is_err());
    }

    proptest! {
        #[test]
        fn arbitrary_nsd_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
            let _ = parse_nsd(&bytes, LevelId::TITLE);
        }
    }
}
