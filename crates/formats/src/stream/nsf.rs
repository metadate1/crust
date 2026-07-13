use core::ops::Range;

use crate::binary::{
    Eid, EntryHandle, FormatError, Offset, PageIndex, PageRef, Reader, checked_slice,
};

use super::Nsd;

/// Exact size of an uncompressed C1 NSF page.
pub const NSF_PAGE_SIZE: usize = 0x1_0000;
const PAGE_HEADER_SIZE: usize = 16;
const PAGE_MAGIC: u16 = 0x1234;
const TEXTURE_PAGE_TYPE: u16 = 1;
const ENTRY_COUNT_MAX: u32 = 256;
// Upstream declared a 128-item convenience bound, but retail entries can carry
// at least 131 items. The 64 KiB page itself is the authoritative safe bound.
const ENTRY_ITEM_COUNT_MAX: u32 = ((NSF_PAGE_SIZE - ENTRY_HEADER_SIZE) / 4 - 1) as u32;
const ENTRY_HEADER_SIZE: usize = 16;
const ENTRY_TYPE_MAX: u32 = 20;

/// Exact magic at the start of a normal entry.
pub const ENTRY_MAGIC: u32 = 0x0100_ffff;

/// Common 16-byte header of a non-texture NSF page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageHeader {
    pub magic: u16,
    pub page_type: u16,
    pub page_id: PageRef,
    pub entry_count: u32,
    pub checksum: u32,
}

/// One validated variable-size entry item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntryItem {
    pub index: u16,
    pub relative_offset: Offset,
    byte_range: Range<usize>,
}

impl EntryItem {
    /// Absolute range within the NSF bytes.
    #[must_use]
    pub fn byte_range(&self) -> Range<usize> {
        self.byte_range.clone()
    }

    /// Borrows this item from the original NSF.
    pub fn bytes<'a>(&self, nsf: &'a [u8]) -> Result<&'a [u8], FormatError> {
        checked_slice(
            nsf,
            self.byte_range.start,
            self.byte_range.len(),
            "NSF entry item",
        )
    }
}

/// A normal page entry with relocated references represented as logical handles.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub handle: EntryHandle,
    pub page_relative_offset: Offset,
    pub magic: u32,
    pub eid: Eid,
    pub entry_type: u32,
    pub items: Vec<EntryItem>,
    byte_range: Range<usize>,
}

impl Entry {
    /// Absolute range occupied by the entry, including its offset table.
    #[must_use]
    pub fn byte_range(&self) -> Range<usize> {
        self.byte_range.clone()
    }

    /// Looks up one validated item without pointer arithmetic.
    #[must_use]
    pub fn item(&self, index: usize) -> Option<&EntryItem> {
        self.items.get(index)
    }
}

/// A normal entry-bearing NSF page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Page {
    pub index: PageIndex,
    pub header: PageHeader,
    pub entries: Vec<Entry>,
    byte_range: Range<usize>,
}

impl Page {
    /// Absolute page range in the NSF.
    #[must_use]
    pub fn byte_range(&self) -> Range<usize> {
        self.byte_range.clone()
    }
}

/// Texture page header. Its word at offset four is an EID, not a pgid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TexturePage {
    pub index: PageIndex,
    pub magic: u16,
    pub page_type: u16,
    pub eid: Eid,
    pub entry_type: u32,
    pub checksum: u32,
    data_range: Range<usize>,
}

impl TexturePage {
    /// Raw texture payload after the 16-byte header.
    pub fn data<'a>(&self, nsf: &'a [u8]) -> Result<&'a [u8], FormatError> {
        checked_slice(
            nsf,
            self.data_range.start,
            self.data_range.len(),
            "texture-page data",
        )
    }
}

/// The two exact interpretations of a 64 KiB page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NsfPage {
    /// Type one uses the texture-page header and has no entry-offset table.
    Texture(TexturePage),
    /// All other supported page types use an entry-offset table.
    Entries(Page),
}

impl NsfPage {
    /// Logical page index assigned by stream order.
    #[must_use]
    pub const fn index(&self) -> PageIndex {
        match self {
            Self::Texture(page) => page.index,
            Self::Entries(page) => page.index,
        }
    }
}

/// Fully validated uncompressed page region of a companion NSF.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Nsf {
    pub page_data_offset: usize,
    pub pages: Vec<NsfPage>,
}

impl Nsf {
    /// Iterates every normal entry in stream order, excluding texture pages.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.pages.iter().flat_map(|page| match page {
            NsfPage::Texture(_) => [].iter(),
            NsfPage::Entries(page) => page.entries.iter(),
        })
    }

    /// Finds a normal entry through its stable logical handle.
    #[must_use]
    pub fn entry(&self, handle: EntryHandle) -> Option<&Entry> {
        let page = self.pages.get(handle.page().get() as usize)?;
        let NsfPage::Entries(page) = page else {
            return None;
        };
        page.entries.get(usize::from(handle.entry()))
    }

    /// Resolves a named NSD page-table record to the matching entry in its
    /// validated NSF page.
    ///
    /// This is the safe equivalent of the original `NSProbe` + `NSResolve`
    /// path: the serialized page id is never overwritten with a native
    /// pointer, and a malformed page cannot redirect the lookup elsewhere.
    pub fn resolve_entry<'a>(&'a self, metadata: &Nsd, eid: Eid) -> Result<&'a Entry, FormatError> {
        let pte = metadata
            .pte(eid)
            .ok_or_else(|| FormatError::global(format!("EID {eid} is absent from the NSD")))?;
        let page_index = usize::try_from(pte.page_index().get())
            .map_err(|_| FormatError::global(format!("EID {eid} page index does not fit host")))?;
        let page = self
            .pages
            .get(page_index)
            .ok_or_else(|| FormatError::global(format!("EID {eid} page is absent from the NSF")))?;
        let NsfPage::Entries(page) = page else {
            return Err(FormatError::global(format!(
                "EID {eid} resolves to a texture page, not an entry"
            )));
        };
        page.entries
            .iter()
            .find(|entry| entry.eid == eid)
            .ok_or_else(|| {
                FormatError::global(format!(
                    "EID {eid} page does not contain its declared entry"
                ))
            })
    }
}

/// Parses all uncompressed pages selected by a previously validated NSD.
pub fn parse_nsf(bytes: &[u8], metadata: &Nsd) -> Result<Nsf, FormatError> {
    metadata.validate_nsf_len(bytes.len())?;
    let page_data_offset = metadata.header.nsf_page_data_offset()?;
    let page_count = usize::try_from(metadata.header.page_count)
        .map_err(|_| FormatError::global("page count does not fit the host size"))?;
    let mut pages = Vec::with_capacity(page_count);
    for page_number in 0..page_count {
        let relative = page_number
            .checked_mul(NSF_PAGE_SIZE)
            .ok_or_else(|| FormatError::global("NSF page offset overflows"))?;
        let page_start = page_data_offset
            .checked_add(relative)
            .ok_or_else(|| FormatError::global("NSF page offset overflows"))?;
        let page_bytes = checked_slice(bytes, page_start, NSF_PAGE_SIZE, "NSF page")?;
        let page_index = PageIndex::new(
            u32::try_from(page_number)
                .map_err(|_| FormatError::at(page_start, "page index exceeds 32 bits"))?,
        );
        pages.push(parse_page(page_bytes, page_start, page_index)?);
    }
    Ok(Nsf {
        page_data_offset,
        pages,
    })
}

fn parse_page(
    bytes: &[u8],
    absolute_start: usize,
    index: PageIndex,
) -> Result<NsfPage, FormatError> {
    let mut reader = Reader::new(bytes);
    let magic = reader.u16_le()?;
    let page_type = reader.u16_le()?;
    if magic != PAGE_MAGIC {
        return Err(FormatError::at(
            absolute_start,
            format!("NSF page magic is 0x{magic:04x}, expected 0x1234"),
        ));
    }
    if page_type == TEXTURE_PAGE_TYPE {
        let eid = Eid::from_raw(reader.u32_le()?);
        let entry_type = reader.u32_le()?;
        let checksum = reader.u32_le()?;
        return Ok(NsfPage::Texture(TexturePage {
            index,
            magic,
            page_type,
            eid,
            entry_type,
            checksum,
            data_range: absolute_start + PAGE_HEADER_SIZE..absolute_start + NSF_PAGE_SIZE,
        }));
    }

    let raw_page_id = reader.u32_le()?;
    let page_id = PageRef::from_raw(raw_page_id);
    let PageRef::Page(encoded_index) = page_id else {
        return Err(FormatError::at(
            absolute_start + 4,
            "normal NSF page contains an even relocated page id",
        ));
    };
    if encoded_index != index {
        return Err(FormatError::at(
            absolute_start + 4,
            format!(
                "NSF page id {} does not match stream index {}",
                encoded_index.get(),
                index.get()
            ),
        ));
    }
    let entry_count = reader.u32_le()?;
    let checksum = reader.u32_le()?;
    if entry_count > ENTRY_COUNT_MAX {
        return Err(FormatError::at(
            absolute_start + 8,
            "NSF page has more than 256 entries",
        ));
    }
    let count = usize::try_from(entry_count)
        .map_err(|_| FormatError::at(absolute_start + 8, "entry count does not fit the host"))?;
    let offset_count = count
        .checked_add(1)
        .ok_or_else(|| FormatError::at(absolute_start + 8, "entry-offset count overflows"))?;
    let offset_table_bytes = offset_count.checked_mul(4).ok_or_else(|| {
        FormatError::at(
            absolute_start + PAGE_HEADER_SIZE,
            "entry-offset table overflows",
        )
    })?;
    let minimum_entry_offset = PAGE_HEADER_SIZE
        .checked_add(offset_table_bytes)
        .ok_or_else(|| FormatError::at(absolute_start, "entry-data offset overflows"))?;
    let mut offsets_reader = Reader::new(checked_slice(
        bytes,
        PAGE_HEADER_SIZE,
        offset_table_bytes,
        "page entry-offset table",
    )?);
    let mut offsets = Vec::with_capacity(offset_count);
    for _ in 0..offset_count {
        let raw = offsets_reader.u32_le()?;
        let offset = usize::try_from(raw)
            .map_err(|_| FormatError::at(absolute_start, "entry offset does not fit the host"))?;
        offsets.push(offset);
    }
    validate_offsets(
        &offsets,
        minimum_entry_offset,
        NSF_PAGE_SIZE,
        absolute_start + PAGE_HEADER_SIZE,
        "page entry",
        true,
    )?;

    let mut entries = Vec::with_capacity(count);
    for entry_index in 0..count {
        let start = offsets[entry_index];
        let end = offsets[entry_index + 1];
        let entry_bytes = checked_slice(bytes, start, end - start, "page entry")?;
        entries.push(parse_entry(
            entry_bytes,
            absolute_start + start,
            index,
            entry_index,
            start,
        )?);
    }
    Ok(NsfPage::Entries(Page {
        index,
        header: PageHeader {
            magic,
            page_type,
            page_id,
            entry_count,
            checksum,
        },
        entries,
        byte_range: absolute_start..absolute_start + NSF_PAGE_SIZE,
    }))
}

fn parse_entry(
    bytes: &[u8],
    absolute_start: usize,
    page: PageIndex,
    entry_index: usize,
    page_relative_offset: usize,
) -> Result<Entry, FormatError> {
    let mut reader = Reader::new(bytes);
    let magic = reader.u32_le()?;
    if magic != ENTRY_MAGIC {
        return Err(FormatError::at(
            absolute_start,
            format!("entry magic is 0x{magic:08x}, expected 0x0100ffff"),
        ));
    }
    let eid = Eid::from_raw(reader.u32_le()?);
    let entry_type = reader.u32_le()?;
    if entry_type > ENTRY_TYPE_MAX {
        return Err(FormatError::at(
            absolute_start + 8,
            "entry type is outside the 0..=20 subsystem table",
        ));
    }
    let item_count = reader.u32_le()?;
    if item_count > ENTRY_ITEM_COUNT_MAX {
        return Err(FormatError::at(
            absolute_start + 12,
            "entry item-offset table cannot fit in a 64 KiB page",
        ));
    }
    let count = usize::try_from(item_count)
        .map_err(|_| FormatError::at(absolute_start + 12, "item count does not fit the host"))?;
    let offset_count = count
        .checked_add(1)
        .ok_or_else(|| FormatError::at(absolute_start + 12, "item-offset count overflows"))?;
    let table_bytes = offset_count.checked_mul(4).ok_or_else(|| {
        FormatError::at(
            absolute_start + ENTRY_HEADER_SIZE,
            "item-offset table overflows",
        )
    })?;
    let minimum_item_offset = ENTRY_HEADER_SIZE
        .checked_add(table_bytes)
        .ok_or_else(|| FormatError::at(absolute_start, "item-data offset overflows"))?;
    let mut offsets_reader = Reader::new(checked_slice(
        bytes,
        ENTRY_HEADER_SIZE,
        table_bytes,
        "entry item-offset table",
    )?);
    let mut offsets = Vec::with_capacity(offset_count);
    for _ in 0..offset_count {
        offsets.push(
            usize::try_from(offsets_reader.u32_le()?).map_err(|_| {
                FormatError::at(absolute_start, "item offset does not fit the host")
            })?,
        );
    }
    validate_offsets(
        &offsets,
        minimum_item_offset,
        bytes.len(),
        absolute_start + ENTRY_HEADER_SIZE,
        "entry item",
        false,
    )?;
    let handle = EntryHandle::new(
        page,
        u16::try_from(entry_index)
            .map_err(|_| FormatError::at(absolute_start, "entry index exceeds 16 bits"))?,
    );
    let mut items = Vec::with_capacity(count);
    for item_index in 0..count {
        let start = offsets[item_index];
        let end = offsets[item_index + 1];
        items.push(EntryItem {
            index: u16::try_from(item_index)
                .map_err(|_| FormatError::at(absolute_start, "item index exceeds 16 bits"))?,
            relative_offset: Offset::new(
                u32::try_from(start)
                    .map_err(|_| FormatError::at(absolute_start, "item offset exceeds 32 bits"))?,
            ),
            byte_range: absolute_start + start..absolute_start + end,
        });
    }
    Ok(Entry {
        handle,
        page_relative_offset: Offset::new(
            u32::try_from(page_relative_offset)
                .map_err(|_| FormatError::at(absolute_start, "entry offset exceeds 32 bits"))?,
        ),
        magic,
        eid,
        entry_type,
        items,
        byte_range: absolute_start..absolute_start + bytes.len(),
    })
}

fn validate_offsets(
    offsets: &[usize],
    minimum: usize,
    maximum: usize,
    table_start: usize,
    context: &'static str,
    require_aligned: bool,
) -> Result<(), FormatError> {
    let Some(first) = offsets.first().copied() else {
        return Err(FormatError::at(
            table_start,
            format!("{context} offset table is empty"),
        ));
    };
    if first < minimum {
        return Err(FormatError::at(
            table_start,
            format!("first {context} overlaps its offset table"),
        ));
    }
    if offsets.iter().any(|offset| *offset > maximum) {
        return Err(FormatError::at(
            table_start,
            format!("{context} offset is outside its container"),
        ));
    }
    if require_aligned && offsets.iter().any(|offset| offset % 4 != 0) {
        return Err(FormatError::at(
            table_start,
            format!("{context} offset is not four-byte aligned"),
        ));
    }
    if !offsets.windows(2).all(|pair| pair[0] <= pair[1]) {
        return Err(FormatError::at(
            table_start,
            format!("{context} offsets are not monotonic"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::{LevelId, parse_nsd};
    use proptest::prelude::*;

    const MODERN_HEADER_SIZE: usize = 0x520;
    const LDAT_PREFIX_SIZE: usize = 0x118;

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn one_page_nsd() -> Nsd {
        let ldat_offset = MODERN_HEADER_SIZE + 8;
        let mut bytes = vec![0_u8; ldat_offset + LDAT_PREFIX_SIZE];
        put_u32(&mut bytes, 0x400, 1);
        put_u32(&mut bytes, 0x404, 1);
        put_u32(&mut bytes, MODERN_HEADER_SIZE, 1);
        put_u32(
            &mut bytes,
            MODERN_HEADER_SIZE + 4,
            Eid::from_name("entry").unwrap().raw(),
        );
        put_u32(&mut bytes, ldat_offset, 1);
        put_u32(&mut bytes, ldat_offset + 4, LevelId::TITLE.get());
        parse_nsd(&bytes, LevelId::TITLE).unwrap()
    }

    fn page_with_one_entry() -> Vec<u8> {
        let mut bytes = vec![0_u8; NSF_PAGE_SIZE];
        put_u16(&mut bytes, 0, PAGE_MAGIC);
        put_u16(&mut bytes, 2, 0);
        put_u32(&mut bytes, 4, 1);
        put_u32(&mut bytes, 8, 1);
        put_u32(&mut bytes, 16, 24);
        put_u32(&mut bytes, 20, 52);
        put_u32(&mut bytes, 24, ENTRY_MAGIC);
        put_u32(&mut bytes, 28, Eid::from_name("entry").unwrap().raw());
        put_u32(&mut bytes, 32, 2);
        put_u32(&mut bytes, 36, 1);
        put_u32(&mut bytes, 40, 24);
        put_u32(&mut bytes, 44, 28);
        bytes[48..52].copy_from_slice(&[1, 2, 3, 4]);
        bytes
    }

    #[test]
    fn parses_offsets_into_handles_and_ranges() {
        let metadata = one_page_nsd();
        let bytes = page_with_one_entry();
        let nsf = parse_nsf(&bytes, &metadata).unwrap();
        let handle = EntryHandle::new(PageIndex::new(0), 0);
        let entry = nsf.entry(handle).unwrap();
        assert_eq!(entry.page_relative_offset, Offset::new(24));
        assert_eq!(entry.items[0].relative_offset, Offset::new(24));
        assert_eq!(entry.items[0].bytes(&bytes).unwrap(), &[1, 2, 3, 4]);
        assert_eq!(
            nsf.resolve_entry(&metadata, Eid::from_name("entry").unwrap())
                .unwrap()
                .handle,
            handle
        );
        assert!(
            nsf.resolve_entry(&metadata, Eid::from_name("other").unwrap())
                .is_err()
        );
    }

    #[test]
    fn texture_pages_do_not_reinterpret_eids_as_pgids() {
        let metadata = one_page_nsd();
        let mut bytes = vec![0_u8; NSF_PAGE_SIZE];
        put_u16(&mut bytes, 0, PAGE_MAGIC);
        put_u16(&mut bytes, 2, TEXTURE_PAGE_TYPE);
        put_u32(&mut bytes, 4, Eid::from_name("tpage").unwrap().raw());
        put_u32(&mut bytes, 8, 5);
        let nsf = parse_nsf(&bytes, &metadata).unwrap();
        let NsfPage::Texture(texture) = &nsf.pages[0] else {
            panic!("expected a texture page");
        };
        assert_eq!(texture.eid.name().as_deref(), Some("tpage"));
        assert_eq!(
            texture.data(&bytes).unwrap().len(),
            NSF_PAGE_SIZE - PAGE_HEADER_SIZE
        );
    }

    #[test]
    fn rejects_relocated_ids_bad_magic_and_crossing_offsets() {
        let metadata = one_page_nsd();
        let mut bytes = page_with_one_entry();
        put_u32(&mut bytes, 4, 0);
        assert!(parse_nsf(&bytes, &metadata).is_err());

        let mut bytes = page_with_one_entry();
        put_u32(&mut bytes, 24, 0);
        assert!(parse_nsf(&bytes, &metadata).is_err());

        let mut bytes = page_with_one_entry();
        put_u32(&mut bytes, 20, 20);
        assert!(parse_nsf(&bytes, &metadata).is_err());
    }

    proptest! {
        #[test]
        fn mutating_any_page_byte_never_panics(index in 0_usize..NSF_PAGE_SIZE, value in any::<u8>()) {
            let metadata = one_page_nsd();
            let mut bytes = page_with_one_entry();
            bytes[index] = value;
            let _ = parse_nsf(&bytes, &metadata);
        }
    }
}
