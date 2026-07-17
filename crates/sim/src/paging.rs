//! Explicit page, entry, and load-list ownership.

use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU8,
};

use crust_formats::binary::{Eid, EntryHandle, PageIndex};
use crust_formats::stream::{LevelId, Nsd, Nsf, NsfPage};

pub const MAX_PHYSICAL_PAGES: usize = 128;
/// Retail Crash Bandicoot keeps exactly twenty-two ordinary NSF pages in RAM.
///
/// The mounted stream may describe many more pages; this is the size of the
/// replaceable physical working set, not the catalog limit above.
pub const PHYSICAL_SLOT_COUNT: usize = 22;

/// Physical ordinary-page pool produced by retail's descending 64 KiB heap
/// probe for each stream class.
///
/// PCSX-Redux traces of the legally local NTSC-U executable establish twenty
/// pages for Title, twenty-one for Intro, and the full twenty-two for every
/// gameplay, bonus, and ending stream. Keeping this profile explicit avoids
/// pretending the browser has an unbounded catalog-sized resident set while
/// preserving the retail heap result without emulating its C allocator.
#[must_use]
pub const fn retail_physical_slot_count(level: LevelId) -> usize {
    match level.get() {
        0x19 => 20,
        0x38 => 21,
        _ => PHYSICAL_SLOT_COUNT,
    }
}
/// Retail's eight usable lower-VRAM slots. Rust slots `0..=7` correspond to
/// native physical slots `8..=15`; native slots `0..=7` hold frame buffers.
pub const TEXTURE_SLOT_COUNT: usize = 8;
const AUDIO_PAGE_TYPES: [u16; 2] = [3, 4];

/// Allocation state of one of retail's eight usable texture-page slots.
///
/// `Free` deliberately does not imply that the previous identity was erased.
/// Native `NSTexturePageFree` re-arms the source page but leaves the slot's EID
/// and copied bytes intact until a later allocation overwrites them. Keeping
/// state separate from identity lets a frame snapshot preserve that behavior.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TextureSlotState {
    #[default]
    Free,
    Resident,
    /// A page retained across a stream mount but preferred for replacement.
    Stale,
}

/// Immutable identity of one physical texture slot generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextureSlotBinding {
    pub page: PageIndex,
    pub eid: Eid,
    pub generation: u32,
    pub state: TextureSlotState,
}

/// Eight-slot identity snapshot consumed by one renderer frame.
///
/// The snapshot owns only validated page handles and scalar generations. It
/// can therefore survive later pager mutation without retaining page bytes or
/// recreating native pointers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TextureFrameSnapshot {
    slots: [Option<TextureSlotBinding>; TEXTURE_SLOT_COUNT],
}

impl TextureFrameSnapshot {
    #[must_use]
    pub fn slot(&self, slot: usize) -> Option<TextureSlotBinding> {
        self.slots.get(slot).copied().flatten()
    }

    #[must_use]
    pub fn slots(&self) -> &[Option<TextureSlotBinding>; TEXTURE_SLOT_COUNT] {
        &self.slots
    }

    #[must_use]
    pub fn find_eid(&self, eid: Eid) -> Option<(usize, TextureSlotBinding)> {
        self.slots.iter().enumerate().find_map(|(slot, binding)| {
            binding
                .filter(|binding| binding.eid == eid)
                .map(|binding| (slot, binding))
        })
    }
}

/// Result of making one named texture page resident.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextureSlotAssignment {
    pub slot: usize,
    pub binding: TextureSlotBinding,
    /// Previous slot identity when this request overwrote one.
    pub replaced: Option<TextureSlotBinding>,
    /// False when the requested EID was already present in a slot.
    pub changed: bool,
}

/// Resolution change produced by one source-compatible page open.
///
/// Ordinary pages resolve without replacing a texture slot. A texture open
/// reports the previous resident binding only when native would re-arm that
/// page's PTE before overwriting the slot; retained Free/Stale identities were
/// already nonresident and therefore are not evictions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PagerOpenOutcome {
    pub page: PageIndex,
    /// Whether this call returned with the page's PTE offsets resolved.
    ///
    /// A flag-zero open may retain a native state-two virtual request when no
    /// ordinary RAM slot is replaceable. Such an open still owns its reference
    /// and is retried by [`Pager::update_pending_virtual_page`].
    pub resolved: bool,
    /// Complete fixed-capacity set of PTEs re-armed by this open. A texture
    /// transfer may displace one ordinary RAM page and one resident texture
    /// page in the same operation.
    pub invalidated: PageInvalidations,
    /// Full texture-slot identity retained for texture-cache diagnostics.
    pub evicted: Option<TextureSlotBinding>,
}

impl PagerOpenOutcome {
    /// Single ordinary eviction produced by count-zero GOOL-global
    /// materialization. Program globals cannot resolve to texture pages, so
    /// this narrower helper is intentionally unavailable to normal opens.
    #[must_use]
    pub fn single_program_eviction(self) -> Option<PageIndex> {
        debug_assert!(self.evicted.is_none());
        debug_assert!(self.invalidated.second().is_none());
        self.invalidated.first()
    }
}

/// At most two distinct PTEs invalidated by one native page allocation.
///
/// Texture materialization first reserves ordinary transfer RAM and then
/// reserves a VRAM slot, so both allocators may evict in one synchronous open.
/// Keeping the pair typed prevents host adapters from silently projecting the
/// result down to one page.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PageInvalidations([Option<PageIndex>; 2]);

impl PageInvalidations {
    pub const NONE: Self = Self([None, None]);

    #[must_use]
    pub const fn one(page: PageIndex) -> Self {
        Self([Some(page), None])
    }

    #[must_use]
    pub fn new(first: Option<PageIndex>, second: Option<PageIndex>) -> Self {
        match (first, second) {
            (None, None) => Self::NONE,
            (Some(page), None) | (None, Some(page)) => Self::one(page),
            (Some(first), Some(second)) if first == second => Self::one(first),
            (Some(first), Some(second)) => Self([Some(first), Some(second)]),
        }
    }

    #[must_use]
    pub const fn first(self) -> Option<PageIndex> {
        self.0[0]
    }

    #[must_use]
    pub const fn second(self) -> Option<PageIndex> {
        self.0[1]
    }

    pub fn iter(self) -> impl Iterator<Item = PageIndex> {
        self.0.into_iter().flatten()
    }
}

/// Shared page-reference change produced by one native-idempotent close.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PagerCloseOutcome {
    pub page: PageIndex,
    pub decremented: bool,
    /// The close targeted a state-two virtual request rather than a resolved
    /// PTE. A final close cancels that pending `NSUpdate` request.
    pub unresolved: bool,
}

/// Source page-state values, represented without pointer tagging.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum PageState {
    Free = 1,
    /// Native state-two virtual request waiting for `NSUpdate(-1)`.
    Queued = 2,
    Raw = 3,
    Translated = 4,
    Resident = 20,
    Stale = 21,
    Inaccessible = 30,
}

/// Allocation path currently backing one ordinary entry page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryPageKind {
    /// A translated type-one page, retained at reference count zero. Both
    /// synchronous flag-zero and flag-one opens reach this promoted state.
    Physical,
}

/// Runtime metadata for a validated NSF page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageRecord {
    pub index: PageIndex,
    pub state: PageState,
    pub generation: u32,
    pub references: u32,
    physical_slot: Option<u8>,
    physical_timestamp: u64,
    ordinary_kind: Option<OrdinaryPageKind>,
    entries: BTreeSet<EntryHandle>,
}

impl PageRecord {
    #[must_use]
    pub fn entries(&self) -> impl ExactSizeIterator<Item = EntryHandle> + '_ {
        self.entries.iter().copied()
    }

    /// Retail RAM slot currently backing this ordinary page.
    #[must_use]
    pub fn physical_slot(&self) -> Option<usize> {
        self.physical_slot.map(usize::from)
    }

    #[must_use]
    pub const fn ordinary_kind(&self) -> Option<OrdinaryPageKind> {
        self.ordinary_kind
    }
}

/// Entries and pages required by one zone/path.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LoadList {
    entries: BTreeSet<EntryHandle>,
    pages: BTreeSet<PageIndex>,
}

impl LoadList {
    #[must_use]
    pub fn new(
        entries: impl IntoIterator<Item = EntryHandle>,
        pages: impl IntoIterator<Item = PageIndex>,
    ) -> Self {
        Self {
            entries: entries.into_iter().collect(),
            pages: pages.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn entries(&self) -> impl ExactSizeIterator<Item = EntryHandle> + '_ {
        self.entries.iter().copied()
    }

    #[must_use]
    pub fn pages(&self) -> impl ExactSizeIterator<Item = PageIndex> + '_ {
        self.pages.iter().copied()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PagingError {
    MissingLdat,
    ExecutableOutsideLdat(usize),
    InvalidExecutableEid { index: usize, eid: Eid },
    TooManyPages,
    DuplicatePage(PageIndex),
    UnknownPage(PageIndex),
    UnknownEntry(EntryHandle),
    EntryPageMismatch(EntryHandle),
    InaccessiblePage(PageIndex),
    ReferenceUnderflow(PageIndex),
    InvalidTextureSlot(usize),
    UnknownEid(Eid),
    DuplicateEid(Eid),
    DuplicateTexturePageEid(PageIndex),
    UnnamedTexturePage(PageIndex),
    InvalidPhysicalSlotCount(usize),
    NoFreePhysicalSlot(PageIndex),
    NoFreeTextureSlot(Eid),
    PendingUpdateStalled(PageIndex),
}

/// Bounds-checked page registry replacing NS pointer relocation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Pager {
    pages: BTreeMap<PageIndex, PageRecord>,
    entries: BTreeMap<EntryHandle, u32>,
    eids: BTreeMap<Eid, EntryHandle>,
    page_eids: BTreeMap<Eid, PageIndex>,
    texture_page_eids: BTreeMap<PageIndex, Eid>,
    /// Type-three/four pages are staged through ordinary RAM and then copied
    /// to SPU RAM. Their resolved entries and reference counts survive, but
    /// the temporary twenty-two-slot allocation does not.
    audio_pages: BTreeSet<PageIndex>,
    active: LoadList,
    texture_slots: [Option<PageIndex>; TEXTURE_SLOT_COUNT],
    texture_slot_states: [TextureSlotState; TEXTURE_SLOT_COUNT],
    texture_generations: [u32; TEXTURE_SLOT_COUNT],
    physical_slots: [Option<PageIndex>; PHYSICAL_SLOT_COUNT],
    /// Runtime count returned by retail's descending 64 KiB heap probe.
    /// `None` is the nominal twenty-two-page maximum.
    physical_slot_count: Option<NonZeroU8>,
    physical_clock: u64,
    /// EIDs protected by native's current-zone load-list allocation rule.
    /// `None` represents a null `cur_zone`, which has a distinct fallback.
    current_texture_load_eids: Option<BTreeSet<Eid>>,
}

impl Pager {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Configures the runtime PS1 ordinary-page pool before any page opens.
    ///
    /// Retail starts at twenty-two 64 KiB pages and retries with a smaller
    /// allocation when the largest remaining heap block cannot satisfy it.
    /// Keeping the backing array at the maximum preserves stable slot IDs;
    /// only the configured prefix participates in allocation/accounting.
    pub fn set_physical_slot_count(&mut self, count: usize) -> Result<(), PagingError> {
        if count == 0 || count > PHYSICAL_SLOT_COUNT {
            return Err(PagingError::InvalidPhysicalSlotCount(count));
        }
        if self.physical_slots[count..].iter().any(Option::is_some) {
            return Err(PagingError::InvalidPhysicalSlotCount(count));
        }
        self.physical_slot_count = if count == PHYSICAL_SLOT_COUNT {
            None
        } else {
            NonZeroU8::new(
                u8::try_from(count).map_err(|_| PagingError::InvalidPhysicalSlotCount(count))?,
            )
        };
        Ok(())
    }

    #[must_use]
    pub fn physical_slot_count(&self) -> usize {
        self.physical_slot_count
            .map_or(PHYSICAL_SLOT_COUNT, |count| usize::from(count.get()))
    }

    /// Builds the pointer-free page/EID catalog from one validated stream
    /// pair. No page is made resident until an explicit native-style open.
    pub fn from_stream(metadata: &Nsd, nsf: &Nsf) -> Result<Self, PagingError> {
        let mut pager = Self::new();
        for page in &nsf.pages {
            let entries = match page {
                NsfPage::Texture(_) => Vec::new(),
                NsfPage::Entries(page) => page
                    .entries
                    .iter()
                    .map(|entry| entry.handle)
                    .collect::<Vec<_>>(),
            };
            pager.register_page(page.index(), entries)?;
            if let NsfPage::Entries(page) = page {
                if AUDIO_PAGE_TYPES.contains(&page.header.page_type) {
                    pager.audio_pages.insert(page.index);
                }
                for entry in &page.entries {
                    pager.bind_eid(entry.eid, entry.handle)?;
                }
            }
        }
        for pte in &metadata.page_table {
            let page = pte.page_index();
            if matches!(
                nsf.pages.get(page.get() as usize),
                Some(NsfPage::Texture(_))
            ) {
                pager.bind_page_eid(pte.eid, page)?;
            }
        }
        Ok(pager)
    }

    /// Reconstructs the initial retail mount through `LdatInit`,
    /// `LevelUpdate`/`NSUpdate2`, and `CoreObjectsCreate` page ownership.
    pub fn mount_retail_level(
        metadata: &Nsd,
        nsf: &Nsf,
        level: LevelId,
        initial_zone: Eid,
        load_entry_eids: impl IntoIterator<Item = Eid>,
        load_pages: impl IntoIterator<Item = PageIndex>,
    ) -> Result<Self, PagingError> {
        Self::mount_retail_level_with_physical_slot_count(
            metadata,
            nsf,
            level,
            initial_zone,
            load_entry_eids,
            load_pages,
            retail_physical_slot_count(level),
        )
    }

    /// Retail mount with an explicitly characterized PS1 heap-page count.
    pub fn mount_retail_level_with_physical_slot_count(
        metadata: &Nsd,
        nsf: &Nsf,
        level: LevelId,
        initial_zone: Eid,
        load_entry_eids: impl IntoIterator<Item = Eid>,
        load_pages: impl IntoIterator<Item = PageIndex>,
        physical_slot_count: usize,
    ) -> Result<Self, PagingError> {
        let mut pager = Self::from_stream(metadata, nsf)?;
        pager.set_physical_slot_count(physical_slot_count)?;
        let load_entry_eids = load_entry_eids.into_iter().collect::<Vec<_>>();
        pager.set_current_texture_load_eids(load_entry_eids.iter().copied());

        // LdatInit physically opens the spawn ZDAT before LevelUpdate. The two
        // hog streams intentionally acquire a second reference and retain one
        // after the matching single close below.
        let spawn_references = usize::from(matches!(level.get(), 0x11 | 0x1e)) + 1;
        for _ in 0..spawn_references {
            pager.open_eid(initial_zone)?;
        }
        for eid in load_entry_eids {
            pager.open_eid_virtual_with_outcome(eid)?;
        }
        for page in load_pages {
            pager.open_page_virtual_with_outcome(page)?;
        }
        // Initial gameplay `LevelUpdate` opens the complete destination load
        // list virtually, then its nonzero native marker calls `NSUpdate2`.
        // Drain only that queue here. `CoreObjectsCreate` runs afterward, so
        // its flag-zero executable preloads remain queued for the ordinary
        // one-page `NSUpdate(-1)` at the start of following CoreFrames.
        pager.update_all_pending_virtual_pages()?;
        pager.close_eid_retail(initial_zone)?;
        pager.materialize_core_pages(metadata, level)?;
        Ok(pager)
    }

    fn executable_map_eid(metadata: &Nsd, index: usize) -> Result<Eid, PagingError> {
        let ldat = metadata.ldat().ok_or(PagingError::MissingLdat)?;
        ldat.executable_map
            .get(index)
            .copied()
            .ok_or(PagingError::ExecutableOutsideLdat(index))
    }

    fn validate_executable_eid(index: usize, eid: Eid) -> Result<Eid, PagingError> {
        if eid == Eid::NONE || !eid.is_named() {
            return Err(PagingError::InvalidExecutableEid { index, eid });
        }
        Ok(eid)
    }

    fn executable_eid(metadata: &Nsd, index: usize) -> Result<Eid, PagingError> {
        Self::validate_executable_eid(index, Self::executable_map_eid(metadata, index)?)
    }

    /// Resolves one ignored `CoreObjectsCreate` preload.
    ///
    /// `NSOpen` returns `ERROR_INVALID_REF` immediately for `EID_NONE`, and
    /// `CoreObjectsCreate` deliberately ignores that return value. Level
    /// Complete uses exactly that representation for executable slot 30, so
    /// the pointer-free equivalent is an absent optional preload rather than
    /// a malformed stream. Other untagged values remain rejected.
    fn preload_executable_eid(metadata: &Nsd, index: usize) -> Result<Option<Eid>, PagingError> {
        Self::validate_preload_executable_eid(index, Self::executable_map_eid(metadata, index)?)
    }

    fn validate_preload_executable_eid(index: usize, eid: Eid) -> Result<Option<Eid>, PagingError> {
        if eid == Eid::NONE {
            return Ok(None);
        }
        Self::validate_executable_eid(index, eid).map(Some)
    }

    fn materialize_core_pages(
        &mut self,
        metadata: &Nsd,
        level: LevelId,
    ) -> Result<(), PagingError> {
        let materialize = |pager: &mut Self, index| {
            pager.materialize_eid_with_outcome(Self::executable_eid(metadata, index)?)?;
            Ok::<(), PagingError>(())
        };
        let open = |pager: &mut Self, index| {
            if let Some(eid) = Self::preload_executable_eid(metadata, index)? {
                pager.open_eid_virtual_with_outcome(eid)?;
            }
            Ok::<(), PagingError>(())
        };

        if level == LevelId::TITLE {
            for index in [4, 52] {
                open(self, index)?;
            }
            return Ok(());
        }
        if level == LevelId::LEVEL_COMPLETE {
            for index in [29, 30, 3] {
                open(self, index)?;
            }
            return Ok(());
        }
        if level == LevelId::INTRO || level == LevelId::ENDING {
            return Ok(());
        }

        materialize(self, 4)?;
        for index in [0, 5, 29] {
            open(self, index)?;
        }
        if level != LevelId::new_const(0x2c) {
            open(self, 34)?;
        }
        for index in [3, 4] {
            open(self, index)?;
        }
        if let Some(index) = match level.get() {
            0x05 => Some(9),
            0x14 | 0x16 => Some(23),
            0x17 => Some(39),
            0x22 | 0x2e => Some(53),
            _ => None,
        } {
            materialize(self, index)?;
        }
        Ok(())
    }

    pub fn register_page(
        &mut self,
        index: PageIndex,
        entries: impl IntoIterator<Item = EntryHandle>,
    ) -> Result<(), PagingError> {
        if self.pages.len() == MAX_PHYSICAL_PAGES {
            return Err(PagingError::TooManyPages);
        }
        if self.pages.contains_key(&index) {
            return Err(PagingError::DuplicatePage(index));
        }
        let entries: BTreeSet<_> = entries.into_iter().collect();
        if let Some(entry) = entries.iter().find(|entry| entry.page() != index) {
            return Err(PagingError::EntryPageMismatch(*entry));
        }
        for entry in &entries {
            self.entries.insert(*entry, 0);
        }
        self.pages.insert(
            index,
            PageRecord {
                index,
                state: PageState::Raw,
                generation: 0,
                references: 0,
                physical_slot: None,
                physical_timestamp: 0,
                ordinary_kind: None,
                entries,
            },
        );
        Ok(())
    }

    pub fn bind_eid(&mut self, eid: Eid, entry: EntryHandle) -> Result<(), PagingError> {
        if !self.entries.contains_key(&entry) {
            return Err(PagingError::UnknownEntry(entry));
        }
        if self.eids.contains_key(&eid) || self.page_eids.contains_key(&eid) {
            return Err(PagingError::DuplicateEid(eid));
        }
        self.eids.insert(eid, entry);
        Ok(())
    }

    /// Binds a named NSD record whose target is a type-one texture page.
    ///
    /// Retail zone load lists store both ordinary entry EIDs and TPAG-page
    /// EIDs in the same serialized array. Keeping the page target typed avoids
    /// fabricating an [`EntryHandle`] for a texture page that has no entry
    /// offset table.
    pub fn bind_page_eid(&mut self, eid: Eid, page: PageIndex) -> Result<(), PagingError> {
        if !self.pages.contains_key(&page) {
            return Err(PagingError::UnknownPage(page));
        }
        if self.eids.contains_key(&eid) || self.page_eids.contains_key(&eid) {
            return Err(PagingError::DuplicateEid(eid));
        }
        if self.texture_page_eids.contains_key(&page) {
            return Err(PagingError::DuplicateTexturePageEid(page));
        }
        self.page_eids.insert(eid, page);
        self.texture_page_eids.insert(page, eid);
        Ok(())
    }

    /// Publishes the current zone's serialized entry-EID load list to the
    /// texture allocator.
    ///
    /// Native installs `cur_zone` before opening the destination load list.
    /// Once all eight slots are occupied, `NSTexturePageAllocate` may replace
    /// only an EID absent from this exact set. Page-index load-list members are
    /// intentionally not folded into it because the source allocator scans
    /// only `loadlist.entries`.
    pub fn set_current_texture_load_eids(&mut self, eids: impl IntoIterator<Item = Eid>) {
        self.current_texture_load_eids = Some(eids.into_iter().collect());
    }

    /// Represents native's null `cur_zone` texture-allocation fallback.
    pub fn clear_current_texture_zone(&mut self) {
        self.current_texture_load_eids = None;
    }

    #[must_use]
    pub fn current_texture_load_eids(&self) -> Option<&BTreeSet<Eid>> {
        self.current_texture_load_eids.as_ref()
    }

    pub fn resolve_eid(&self, eid: Eid) -> Result<EntryHandle, PagingError> {
        self.eids
            .get(&eid)
            .copied()
            .ok_or(PagingError::UnknownEid(eid))
    }

    /// Opens either an ordinary entry or a named texture page through the
    /// same EID namespace used by native `NSOpen`.
    pub fn open_eid(&mut self, eid: Eid) -> Result<(), PagingError> {
        self.open_eid_with_outcome(eid).map(|_| ())
    }

    /// Opens one EID and reports the exact page-resolution change.
    pub fn open_eid_with_outcome(&mut self, eid: Eid) -> Result<PagerOpenOutcome, PagingError> {
        self.open_eid_with_kind(eid, OrdinaryPageKind::Physical)
    }

    /// Opens one EID through native's flag-zero virtual path.
    ///
    /// For every newly unresolved PTE, PSX `NSPageVirtual` retains a state-two
    /// request and its reference rather than attempting physical allocation.
    /// The browser exposes that queued/null result for a later
    /// [`Self::update_pending_virtual_page`] call. Already-resolved PTEs keep
    /// the native immediate reference-increment fast path.
    pub fn open_eid_virtual_with_outcome(
        &mut self,
        eid: Eid,
    ) -> Result<PagerOpenOutcome, PagingError> {
        if let Some(entry) = self.eids.get(&eid).copied() {
            let outcome = self.open_entry_virtual_with_outcome(entry)?;
            return Ok(outcome);
        }
        let page = self
            .page_eids
            .get(&eid)
            .copied()
            .ok_or(PagingError::UnknownEid(eid))?;
        self.open_page_virtual_with_outcome(page)
    }

    fn open_eid_with_kind(
        &mut self,
        eid: Eid,
        kind: OrdinaryPageKind,
    ) -> Result<PagerOpenOutcome, PagingError> {
        if let Some(entry) = self.eids.get(&eid).copied() {
            return self.open_entry_with_kind(entry, kind);
        }
        let page = self
            .page_eids
            .get(&eid)
            .copied()
            .ok_or(PagingError::UnknownEid(eid))?;
        self.open_page_with_kind(page, kind)
    }

    /// Resolves one EID exactly like `NSOpen(..., count = 0)`: its page must
    /// occupy RAM, but the page and entry reference counts do not change.
    pub fn materialize_eid_with_outcome(
        &mut self,
        eid: Eid,
    ) -> Result<PagerOpenOutcome, PagingError> {
        let page = if let Some(entry) = self.eids.get(&eid).copied() {
            entry.page()
        } else {
            self.page_eids
                .get(&eid)
                .copied()
                .ok_or(PagingError::UnknownEid(eid))?
        };
        self.materialize_page_with_outcome(page)
    }

    /// Closes a previously opened entry or named texture page EID.
    pub fn close_eid(&mut self, eid: Eid) -> Result<(), PagingError> {
        if let Some(entry) = self.eids.get(&eid).copied() {
            return self.close_entry(entry);
        }
        let page = self
            .page_eids
            .get(&eid)
            .copied()
            .ok_or(PagingError::UnknownEid(eid))?;
        self.close_page(page)
    }

    /// Source-compatible `NSClose(ref, 1)` with native's zero-count
    /// idempotence and copied-PTE tag handling.
    ///
    /// Strict lifecycle preflight may keep using [`Self::close_eid`] to expose
    /// an unbalanced owned plan. Authored GOOL and the ordered lifecycle
    /// commit after TERM handlers use this operation because retail leaves an
    /// already-zero reference at zero.
    pub fn close_eid_retail(&mut self, eid: Eid) -> Result<(), PagingError> {
        self.close_eid_retail_with_outcome(eid).map(|_| ())
    }

    /// Closes one EID with native page-level semantics and reports whether
    /// the shared physical-page reference count changed.
    pub fn close_eid_retail_with_outcome(
        &mut self,
        eid: Eid,
    ) -> Result<PagerCloseOutcome, PagingError> {
        if let Some(entry) = self.eids.get(&eid).copied() {
            let page = entry.page();
            let state = self
                .pages
                .get(&page)
                .ok_or(PagingError::UnknownPage(page))?
                .state;
            if state != PageState::Queued && !self.page_offsets_resolved(page)? {
                // `NSClose` follows the re-armed PTE to `NSPageClose`, which
                // returns zero for a null page-map entry without touching the
                // old physical/texture page's stranded reference count.
                return Ok(PagerCloseOutcome {
                    page,
                    decremented: false,
                    unresolved: false,
                });
            }
            if self.copied_page_pte(page) {
                // Resolved texture/audio PTEs carry native bit two. NSClose
                // tests that tag before locating a containing ordinary page,
                // returns zero, and leaves both counts untouched.
                return Ok(PagerCloseOutcome {
                    page,
                    decremented: false,
                    unresolved: false,
                });
            }
            let references = self
                .entries
                .get_mut(&entry)
                .ok_or(PagingError::UnknownEntry(entry))?;
            if *references != 0 {
                *references -= 1;
            }
            // Native tracks only the containing page's ref_count. Closing EID
            // A can therefore consume a reference originally opened through
            // EID B on that same page. Per-entry counts are retained only as
            // advisory Rust ownership diagnostics and converge as their own
            // closes arrive.
            return self.close_page_retail_with_outcome(page);
        }
        let page = self
            .page_eids
            .get(&eid)
            .copied()
            .ok_or(PagingError::UnknownEid(eid))?;
        self.close_page_retail_with_outcome(page)
    }

    fn copied_page_pte(&self, page: PageIndex) -> bool {
        self.page_offsets_resolved(page).unwrap_or(false)
            && (self.texture_page_eids.contains_key(&page) || self.audio_pages.contains(&page))
    }

    #[must_use]
    pub fn page(&self, page: PageIndex) -> Option<&PageRecord> {
        self.pages.get(&page)
    }

    #[must_use]
    pub fn active_load_list(&self) -> &LoadList {
        &self.active
    }

    /// Number of validated pages in the mounted NSF pager catalog.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Number of occupied ordinary-page RAM slots.
    #[must_use]
    pub fn resident_physical_page_count(&self) -> usize {
        self.physical_slots[..self.physical_slot_count()]
            .iter()
            .flatten()
            .count()
    }

    /// Native `NSCountAvailablePages`: physical capacity minus every
    /// referenced ordinary page, including a queued/nonresident one.
    #[must_use]
    pub fn available_physical_page_count(&self) -> usize {
        let referenced = self
            .pages
            .values()
            .filter(|record| {
                record.references != 0
                    && !self.texture_page_eids.contains_key(&record.index)
                    && !self.audio_pages.contains(&record.index)
            })
            .count();
        self.physical_slot_count().saturating_sub(referenced)
    }

    #[must_use]
    pub fn entry_references(&self, entry: EntryHandle) -> Option<u32> {
        self.entries.get(&entry).copied()
    }

    /// Sum of all explicit page references, including references contributed
    /// by opened entries. This is a diagnostics value; reference mutation
    /// remains checked per page.
    #[must_use]
    pub fn total_page_references(&self) -> u64 {
        self.pages
            .values()
            .map(|page| u64::from(page.references))
            .sum()
    }

    /// Sum of all named-entry references currently owned by the pager.
    #[must_use]
    pub fn total_entry_references(&self) -> u64 {
        self.entries.values().copied().map(u64::from).sum()
    }

    /// Current shared physical-page reference counts for host/VM seeding.
    pub fn page_reference_counts(&self) -> impl Iterator<Item = (PageIndex, u32)> + '_ {
        self.pages
            .values()
            .map(|record| (record.index, record.references))
    }

    /// Catalog pages copied into the separate texture cache and therefore not
    /// charged against `NSCountAvailablePages`.
    pub fn texture_pages(&self) -> impl Iterator<Item = PageIndex> + '_ {
        self.texture_page_eids.keys().copied()
    }

    /// Type-three/four NSF pages whose payload is resident in SPU RAM rather
    /// than one of the twenty-two ordinary page slots.
    pub fn audio_pages(&self) -> impl Iterator<Item = PageIndex> + '_ {
        self.audio_pages.iter().copied()
    }

    /// Pages resolved through a dedicated texture/audio destination and thus
    /// excluded from native `NSCountAvailablePages` accounting.
    pub fn uncounted_pages(&self) -> impl Iterator<Item = PageIndex> + '_ {
        self.texture_pages().chain(self.audio_pages())
    }

    /// Pages whose NSD entry offsets are currently resolved by this pager.
    ///
    /// Ordinary translated pages remain resolved in the browser's mounted
    /// NSF. Texture pages are resolved only while their copied slot is live;
    /// stale/free retained identities are deliberately excluded.
    pub fn resolved_pages(&self) -> impl Iterator<Item = PageIndex> + '_ {
        self.pages.values().filter_map(|record| {
            let is_texture = self.texture_page_eids.contains_key(&record.index);
            ((!is_texture && record.state == PageState::Translated)
                || (is_texture && record.state == PageState::Resident))
                .then_some(record.index)
        })
    }

    /// State-two virtual requests retained for a later `NSUpdate(-1)` retry.
    pub fn pending_virtual_pages(&self) -> impl Iterator<Item = PageIndex> + '_ {
        self.pages
            .values()
            .filter(|record| record.state == PageState::Queued)
            .map(|record| record.index)
    }

    fn page_offsets_resolved(&self, page: PageIndex) -> Result<bool, PagingError> {
        let record = self
            .pages
            .get(&page)
            .ok_or(PagingError::UnknownPage(page))?;
        Ok(if self.texture_page_eids.contains_key(&page) {
            record.state == PageState::Resident
        } else {
            record.state == PageState::Translated
        })
    }

    pub fn set_page_inaccessible(&mut self, page: PageIndex) -> Result<(), PagingError> {
        let physical_slot = self
            .pages
            .get(&page)
            .ok_or(PagingError::UnknownPage(page))?
            .physical_slot;
        if self.pages[&page].references != 0 {
            return Err(PagingError::InaccessiblePage(page));
        }
        if let Some(slot) = physical_slot {
            self.physical_slots[usize::from(slot)] = None;
        }
        let record = self
            .pages
            .get_mut(&page)
            .ok_or(PagingError::UnknownPage(page))?;
        record.physical_slot = None;
        record.ordinary_kind = None;
        record.state = PageState::Inaccessible;
        Ok(())
    }

    pub fn open_page(&mut self, page: PageIndex) -> Result<(), PagingError> {
        self.open_page_with_outcome(page).map(|_| ())
    }

    /// Opens one page and reports a resident texture binding displaced by the
    /// same native allocation operation.
    pub fn open_page_with_outcome(
        &mut self,
        page: PageIndex,
    ) -> Result<PagerOpenOutcome, PagingError> {
        self.open_page_with_kind(page, OrdinaryPageKind::Physical)
    }

    /// Opens one page through native's flag-zero virtual path.
    ///
    /// A newly unresolved page is always queued, even when ordinary RAM is
    /// currently free. `NSPageVirtual` never promotes it synchronously; the
    /// lowest queued pgid is copied only by a later `NSUpdate(-1)`. An already
    /// resolved PTE takes the ordinary fast path and increments immediately.
    pub fn open_page_virtual_with_outcome(
        &mut self,
        page: PageIndex,
    ) -> Result<PagerOpenOutcome, PagingError> {
        let record = self
            .pages
            .get(&page)
            .ok_or(PagingError::UnknownPage(page))?;
        if record.state == PageState::Inaccessible {
            return Err(PagingError::InaccessiblePage(page));
        }
        if self.page_offsets_resolved(page)? {
            self.open_page_with_kind(page, OrdinaryPageKind::Physical)
        } else {
            self.queue_virtual_page_reference(page)
        }
    }

    fn queue_virtual_page_reference(
        &mut self,
        page: PageIndex,
    ) -> Result<PagerOpenOutcome, PagingError> {
        let record = self
            .pages
            .get_mut(&page)
            .ok_or(PagingError::UnknownPage(page))?;
        if record.state == PageState::Inaccessible {
            return Err(PagingError::InaccessiblePage(page));
        }
        debug_assert!(record.physical_slot.is_none());
        record.state = PageState::Queued;
        record.ordinary_kind = None;
        record.references = record.references.saturating_add(1);
        Ok(PagerOpenOutcome {
            page,
            resolved: false,
            invalidated: PageInvalidations::NONE,
            evicted: None,
        })
    }

    /// Performs one native `NSUpdate(-1)` virtual-page promotion.
    ///
    /// Retail chooses the lowest pending pgid and leaves it queued when no
    /// physical resource is replaceable. At most one request is promoted by a
    /// frame update; callers can loop this operation for `NSUpdate2` behavior.
    pub fn update_pending_virtual_page(&mut self) -> Result<Option<PagerOpenOutcome>, PagingError> {
        let Some(page) = self
            .pages
            .values()
            .filter(|record| record.state == PageState::Queued)
            .map(|record| record.index)
            .min()
        else {
            return Ok(None);
        };
        match self.open_page_with_reference_outcome(page, false, OrdinaryPageKind::Physical) {
            Ok(outcome) => Ok(Some(outcome)),
            Err(PagingError::NoFreePhysicalSlot(_) | PagingError::NoFreeTextureSlot(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Drains native's virtual queue using `NSUpdate2` ordering.
    ///
    /// The synchronous model rejects a permanently stalled queue instead of
    /// reproducing retail's unbounded `while (page_count)` loop.
    pub fn update_all_pending_virtual_pages(
        &mut self,
    ) -> Result<Vec<PagerOpenOutcome>, PagingError> {
        let mut outcomes = Vec::new();
        loop {
            let Some(next) = self.pending_virtual_pages().next() else {
                return Ok(outcomes);
            };
            let Some(outcome) = self.update_pending_virtual_page()? else {
                return Err(PagingError::PendingUpdateStalled(next));
            };
            outcomes.push(outcome);
        }
    }

    fn open_page_with_kind(
        &mut self,
        page: PageIndex,
        kind: OrdinaryPageKind,
    ) -> Result<PagerOpenOutcome, PagingError> {
        self.open_page_with_reference_outcome(page, true, kind)
    }

    /// Physical page materialization without a reference increment.
    pub fn materialize_page_with_outcome(
        &mut self,
        page: PageIndex,
    ) -> Result<PagerOpenOutcome, PagingError> {
        self.open_page_with_reference_outcome(page, false, OrdinaryPageKind::Physical)
    }

    fn open_page_with_reference_outcome(
        &mut self,
        page: PageIndex,
        increment_reference: bool,
        kind: OrdinaryPageKind,
    ) -> Result<PagerOpenOutcome, PagingError> {
        // Texture allocation has two bounded resources (ordinary transfer RAM
        // and a texture slot). Previewing keeps a failure in either allocator
        // from committing an eviction in the other.
        let mut preview = self.clone();
        let outcome = preview.open_page_with_outcome_in_place(page, increment_reference, kind)?;
        *self = preview;
        Ok(outcome)
    }

    fn open_page_with_outcome_in_place(
        &mut self,
        page: PageIndex,
        increment_reference: bool,
        kind: OrdinaryPageKind,
    ) -> Result<PagerOpenOutcome, PagingError> {
        let state = self
            .pages
            .get(&page)
            .ok_or(PagingError::UnknownPage(page))?
            .state;
        if state == PageState::Inaccessible {
            return Err(PagingError::InaccessiblePage(page));
        }
        let is_texture = self.texture_page_eids.contains_key(&page);
        let is_audio = self.audio_pages.contains(&page);
        // A type-one texture page first consumes ordinary transfer RAM and is
        // then copied into VRAM, releasing that ordinary slot immediately.
        let (physical_evicted, evicted) = if is_texture {
            let physical_evicted = (state != PageState::Resident)
                .then(|| self.reserve_temporary_physical_slot(page))
                .transpose()?
                .flatten();
            let evicted = self
                .materialize_texture_page(page)?
                .replaced
                .filter(|binding| binding.state == TextureSlotState::Resident);
            (physical_evicted, evicted)
        } else if is_audio {
            let physical_evicted = (state != PageState::Translated)
                .then(|| self.reserve_temporary_physical_slot(page))
                .transpose()?
                .flatten();
            self.pages
                .get_mut(&page)
                .ok_or(PagingError::UnknownPage(page))?
                .state = PageState::Translated;
            (physical_evicted, None)
        } else {
            (self.materialize_ordinary_page(page, kind)?, None)
        };
        let record = self
            .pages
            .get_mut(&page)
            .ok_or(PagingError::UnknownPage(page))?;
        if increment_reference {
            record.references = record.references.saturating_add(1);
        }
        debug_assert!(is_texture || record.state == PageState::Translated);
        Ok(PagerOpenOutcome {
            page,
            resolved: true,
            invalidated: PageInvalidations::new(
                physical_evicted,
                evicted.map(|binding| binding.page),
            ),
            evicted,
        })
    }

    fn materialize_ordinary_page(
        &mut self,
        page: PageIndex,
        kind: OrdinaryPageKind,
    ) -> Result<Option<PageIndex>, PagingError> {
        if self.pages[&page].physical_slot.is_some() {
            if kind == OrdinaryPageKind::Physical {
                self.pages
                    .get_mut(&page)
                    .ok_or(PagingError::UnknownPage(page))?
                    .ordinary_kind = Some(OrdinaryPageKind::Physical);
            }
            return Ok(None);
        }
        let (slot, evicted) = self.take_replaceable_physical_slot(page)?;
        self.physical_clock = self.physical_clock.wrapping_add(1);
        self.physical_slots[slot] = Some(page);
        let record = self
            .pages
            .get_mut(&page)
            .ok_or(PagingError::UnknownPage(page))?;
        record.state = PageState::Translated;
        record.physical_slot = Some(u8::try_from(slot).expect("twenty-two slots fit u8"));
        record.physical_timestamp = self.physical_clock;
        record.ordinary_kind = Some(kind);
        Ok(evicted)
    }

    fn reserve_temporary_physical_slot(
        &mut self,
        page: PageIndex,
    ) -> Result<Option<PageIndex>, PagingError> {
        let (slot, evicted) = self.take_replaceable_physical_slot(page)?;
        // Native releases the transfer page as soon as the texture copy and
        // PTE rewrite complete. The slot therefore remains immediately
        // available to the next ordinary request.
        self.physical_slots[slot] = None;
        Ok(evicted)
    }

    fn take_replaceable_physical_slot(
        &mut self,
        requested: PageIndex,
    ) -> Result<(usize, Option<PageIndex>), PagingError> {
        let physical_slot_count = self.physical_slot_count();
        if let Some(slot) = self.physical_slots[..physical_slot_count]
            .iter()
            .position(Option::is_none)
        {
            return Ok((slot, None));
        }
        let (slot, evicted) = self.physical_slots[..physical_slot_count]
            .iter()
            .enumerate()
            .filter_map(|(slot, page)| {
                let page = page.as_ref().copied()?;
                let record = self.pages.get(&page)?;
                (record.references == 0).then_some((slot, page, record.physical_timestamp))
            })
            .min_by_key(|(slot, _, timestamp)| (*timestamp, *slot))
            .map(|(slot, page, _)| (slot, page))
            .ok_or(PagingError::NoFreePhysicalSlot(requested))?;
        let record = self
            .pages
            .get_mut(&evicted)
            .ok_or(PagingError::UnknownPage(evicted))?;
        record.state = PageState::Raw;
        record.physical_slot = None;
        record.ordinary_kind = None;
        self.physical_slots[slot] = None;
        Ok((slot, Some(evicted)))
    }

    pub fn close_page(&mut self, page: PageIndex) -> Result<(), PagingError> {
        let record = self
            .pages
            .get_mut(&page)
            .ok_or(PagingError::UnknownPage(page))?;
        record.references = record
            .references
            .checked_sub(1)
            .ok_or(PagingError::ReferenceUnderflow(page))?;
        if record.references == 0 && record.state == PageState::Queued {
            record.state = PageState::Raw;
        }
        Ok(())
    }

    /// Source-compatible page close used by `NSZoneUnload`.
    ///
    /// Native `NSPageDecRef` returns immediately when the count is already
    /// zero. This matters when a synchronous RESPAWN or TERM handler has
    /// closed a reference that the following lifecycle unload also names.
    pub fn close_page_retail(&mut self, page: PageIndex) -> Result<(), PagingError> {
        self.close_page_retail_with_outcome(page).map(|_| ())
    }

    /// Native-idempotent page close with an explicit reference-count delta.
    pub fn close_page_retail_with_outcome(
        &mut self,
        page: PageIndex,
    ) -> Result<PagerCloseOutcome, PagingError> {
        let state = self
            .pages
            .get(&page)
            .ok_or(PagingError::UnknownPage(page))?
            .state;
        if state == PageState::Queued {
            let record = self
                .pages
                .get_mut(&page)
                .ok_or(PagingError::UnknownPage(page))?;
            if record.references == 0 {
                return Ok(PagerCloseOutcome {
                    page,
                    decremented: false,
                    unresolved: true,
                });
            }
            record.references -= 1;
            if record.references == 0 {
                // Native NSPageDecRef deletes a zero-reference type-zero page,
                // canceling its pending NSUpdate request.
                record.state = PageState::Raw;
            }
            return Ok(PagerCloseOutcome {
                page,
                decremented: true,
                unresolved: true,
            });
        }
        if !self.page_offsets_resolved(page)? {
            return Ok(PagerCloseOutcome {
                page,
                decremented: false,
                unresolved: false,
            });
        }
        if self.copied_page_pte(page) {
            // Direct NSPageClose also treats copied texture/audio page structs
            // as nonordinary and returns without consuming their references.
            return Ok(PagerCloseOutcome {
                page,
                decremented: false,
                unresolved: false,
            });
        }
        if self
            .pages
            .get(&page)
            .ok_or(PagingError::UnknownPage(page))?
            .references
            == 0
        {
            return Ok(PagerCloseOutcome {
                page,
                decremented: false,
                unresolved: false,
            });
        }
        let record = self
            .pages
            .get_mut(&page)
            .ok_or(PagingError::UnknownPage(page))?;
        record.references -= 1;
        Ok(PagerCloseOutcome {
            page,
            decremented: true,
            unresolved: false,
        })
    }

    pub fn open_entry(&mut self, entry: EntryHandle) -> Result<(), PagingError> {
        self.open_entry_with_outcome(entry).map(|_| ())
    }

    /// Opens one entry and reports the containing page's resolution change.
    pub fn open_entry_with_outcome(
        &mut self,
        entry: EntryHandle,
    ) -> Result<PagerOpenOutcome, PagingError> {
        self.open_entry_with_kind(entry, OrdinaryPageKind::Physical)
    }

    fn open_entry_with_kind(
        &mut self,
        entry: EntryHandle,
        kind: OrdinaryPageKind,
    ) -> Result<PagerOpenOutcome, PagingError> {
        let page = entry.page();
        let record = self
            .pages
            .get(&page)
            .ok_or(PagingError::UnknownPage(page))?;
        if !record.entries.contains(&entry) {
            return Err(PagingError::UnknownEntry(entry));
        }
        let outcome = self.open_page_with_kind(page, kind)?;
        let references = self
            .entries
            .get_mut(&entry)
            .ok_or(PagingError::UnknownEntry(entry))?;
        *references = references.saturating_add(1);
        Ok(outcome)
    }

    fn open_entry_virtual_with_outcome(
        &mut self,
        entry: EntryHandle,
    ) -> Result<PagerOpenOutcome, PagingError> {
        let page = entry.page();
        let record = self
            .pages
            .get(&page)
            .ok_or(PagingError::UnknownPage(page))?;
        if !record.entries.contains(&entry) {
            return Err(PagingError::UnknownEntry(entry));
        }
        let outcome = self.open_page_virtual_with_outcome(page)?;
        let references = self
            .entries
            .get_mut(&entry)
            .ok_or(PagingError::UnknownEntry(entry))?;
        *references = references.saturating_add(1);
        Ok(outcome)
    }

    pub fn close_entry(&mut self, entry: EntryHandle) -> Result<(), PagingError> {
        let references = self
            .entries
            .get_mut(&entry)
            .ok_or(PagingError::UnknownEntry(entry))?;
        *references = references
            .checked_sub(1)
            .ok_or(PagingError::ReferenceUnderflow(entry.page()))?;
        self.close_page(entry.page())
    }

    /// Atomically validates, then applies the difference between zone lists.
    pub fn apply_load_list(&mut self, next: LoadList) -> Result<(), PagingError> {
        // Texture materialization adds a capacity failure that cannot be
        // proven from page metadata alone. Apply to an owned preview so a
        // ninth protected TPAG cannot leave earlier closes/opens committed.
        let mut preview = self.clone();
        preview.apply_load_list_in_place(next)?;
        *self = preview;
        Ok(())
    }

    fn apply_load_list_in_place(&mut self, next: LoadList) -> Result<(), PagingError> {
        for page in next.pages() {
            let record = self
                .pages
                .get(&page)
                .ok_or(PagingError::UnknownPage(page))?;
            if record.state == PageState::Inaccessible {
                return Err(PagingError::InaccessiblePage(page));
            }
        }
        for entry in next.entries() {
            if !self.entries.contains_key(&entry) {
                return Err(PagingError::UnknownEntry(entry));
            }
        }

        let entries_to_close: Vec<_> = self
            .active
            .entries
            .difference(&next.entries)
            .copied()
            .collect();
        let pages_to_close: Vec<_> = self.active.pages.difference(&next.pages).copied().collect();
        let entries_to_open: Vec<_> = next
            .entries
            .difference(&self.active.entries)
            .copied()
            .collect();
        let pages_to_open: Vec<_> = next.pages.difference(&self.active.pages).copied().collect();

        for entry in entries_to_close {
            self.close_entry(entry)?;
        }
        for page in pages_to_close {
            self.close_page(page)?;
        }
        for page in pages_to_open {
            self.open_page_virtual_with_outcome(page)?;
        }
        for entry in entries_to_open {
            self.open_entry_with_kind(entry, OrdinaryPageKind::Physical)?;
        }
        self.active = next;
        Ok(())
    }

    /// Assigns a named physical page to one of the eight usable texture slots.
    ///
    /// Reinstalling the same resident EID in the same slot is idempotent. A
    /// freed slot retains its old identity only for frame snapshots; reopening
    /// that EID must run allocation and create a new slot generation.
    pub fn materialize_texture(
        &mut self,
        slot: usize,
        page: PageIndex,
    ) -> Result<u32, PagingError> {
        let eid = self
            .texture_page_eids
            .get(&page)
            .copied()
            .ok_or(PagingError::UnnamedTexturePage(page))?;
        let previous_page = *self
            .texture_slots
            .get(slot)
            .ok_or(PagingError::InvalidTextureSlot(slot))?;
        let state = self
            .pages
            .get(&page)
            .ok_or(PagingError::UnknownPage(page))?
            .state;
        if state == PageState::Inaccessible {
            return Err(PagingError::InaccessiblePage(page));
        }
        if previous_page == Some(page)
            && self.texture_slot_states[slot] == TextureSlotState::Resident
        {
            self.texture_slot_states[slot] = TextureSlotState::Resident;
            if let Some(record) = self.pages.get_mut(&page) {
                record.state = PageState::Resident;
            }
            return Ok(self.texture_generations[slot]);
        }
        if let Some(previous) = previous_page
            && let Some(previous_record) = self.pages.get_mut(&previous)
        {
            previous_record.state = PageState::Stale;
        }
        self.texture_generations[slot] = self.texture_generations[slot].wrapping_add(1);
        let record = self
            .pages
            .get_mut(&page)
            .ok_or(PagingError::UnknownPage(page))?;
        record.state = PageState::Resident;
        record.generation = self.texture_generations[slot];
        self.texture_slots[slot] = Some(page);
        self.texture_slot_states[slot] = TextureSlotState::Resident;
        debug_assert_eq!(self.texture_page_eids.get(&page), Some(&eid));
        Ok(record.generation)
    }

    /// Makes one named texture page resident using native's high-to-low slot
    /// selection order: free, stale, null-zone fallback, then the first page
    /// not protected by the current zone's entry-EID load list.
    pub fn materialize_texture_eid(
        &mut self,
        eid: Eid,
    ) -> Result<TextureSlotAssignment, PagingError> {
        let page = self
            .page_eids
            .get(&eid)
            .copied()
            .ok_or(PagingError::UnknownEid(eid))?;

        if let Some(slot) = self
            .texture_slots
            .iter()
            .enumerate()
            .position(|(slot, candidate)| {
                *candidate == Some(page)
                    && self.texture_slot_states[slot] == TextureSlotState::Resident
            })
        {
            self.materialize_texture(slot, page)?;
            let binding = self
                .texture_slot_binding(slot)
                .ok_or(PagingError::UnnamedTexturePage(page))?;
            return Ok(TextureSlotAssignment {
                slot,
                binding,
                replaced: None,
                changed: false,
            });
        }

        let free = (0..TEXTURE_SLOT_COUNT)
            .rev()
            .find(|slot| self.texture_slot_states[*slot] == TextureSlotState::Free);
        let stale = (0..TEXTURE_SLOT_COUNT)
            .rev()
            .find(|slot| self.texture_slot_states[*slot] == TextureSlotState::Stale);
        let replaceable = match self.current_texture_load_eids.as_ref() {
            None => Some(TEXTURE_SLOT_COUNT - 1),
            Some(protected) => (0..TEXTURE_SLOT_COUNT).rev().find(|slot| {
                self.texture_slot_binding(*slot)
                    .is_some_and(|binding| !protected.contains(&binding.eid))
            }),
        };
        let slot = free
            .or(stale)
            .or(replaceable)
            .ok_or(PagingError::NoFreeTextureSlot(eid))?;
        let replaced = self.texture_slot_binding(slot);
        self.materialize_texture(slot, page)?;
        let binding = self
            .texture_slot_binding(slot)
            .ok_or(PagingError::UnnamedTexturePage(page))?;
        Ok(TextureSlotAssignment {
            slot,
            binding,
            replaced,
            changed: true,
        })
    }

    /// Page-index counterpart used by serialized load-list `pgids`.
    pub fn materialize_texture_page(
        &mut self,
        page: PageIndex,
    ) -> Result<TextureSlotAssignment, PagingError> {
        let eid = self
            .texture_page_eids
            .get(&page)
            .copied()
            .ok_or(PagingError::UnnamedTexturePage(page))?;
        self.materialize_texture_eid(eid)
    }

    /// Marks a slot as preferred replacement while retaining its identity.
    /// This is the pointer-free counterpart of stream-mount state 21.
    pub fn mark_texture_slot_stale(&mut self, slot: usize) -> Result<(), PagingError> {
        let state = self
            .texture_slot_states
            .get_mut(slot)
            .ok_or(PagingError::InvalidTextureSlot(slot))?;
        *state = TextureSlotState::Stale;
        if let Some(page) = self.texture_slots[slot]
            && let Some(record) = self.pages.get_mut(&page)
        {
            record.state = PageState::Stale;
        }
        Ok(())
    }

    /// Frees a slot for allocation without erasing its last EID/generation.
    /// Renderer frame snapshots may still address a cached old-generation
    /// texture until the next frame boundary.
    pub fn free_texture_slot(&mut self, slot: usize) -> Result<(), PagingError> {
        let state = self
            .texture_slot_states
            .get_mut(slot)
            .ok_or(PagingError::InvalidTextureSlot(slot))?;
        *state = TextureSlotState::Free;
        if let Some(page) = self.texture_slots[slot]
            && let Some(record) = self.pages.get_mut(&page)
        {
            record.state = PageState::Translated;
        }
        Ok(())
    }

    #[must_use]
    pub fn texture_slot(&self, slot: usize) -> Option<(PageIndex, u32)> {
        self.texture_slots
            .get(slot)
            .copied()
            .flatten()
            .map(|page| (page, self.texture_generations[slot]))
    }

    #[must_use]
    pub fn texture_slot_binding(&self, slot: usize) -> Option<TextureSlotBinding> {
        self.texture_slots
            .get(slot)
            .copied()
            .flatten()
            .and_then(|page| {
                self.texture_page_eids
                    .get(&page)
                    .copied()
                    .map(|eid| TextureSlotBinding {
                        page,
                        eid,
                        generation: self.texture_generations[slot],
                        state: self.texture_slot_states[slot],
                    })
            })
    }

    /// Captures the exact slot identities consumed by `TexturesBeginFrame`.
    #[must_use]
    pub fn texture_frame_snapshot(&self) -> TextureFrameSnapshot {
        TextureFrameSnapshot {
            slots: std::array::from_fn(|slot| self.texture_slot_binding(slot)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retail_heap_probe_profile_matches_characterized_stream_classes() {
        assert_eq!(retail_physical_slot_count(LevelId::new_const(0x19)), 20);
        assert_eq!(retail_physical_slot_count(LevelId::new_const(0x38)), 21);
        for level in [0x00, 0x01, 0x1b, 0x24, 0x25, 0x2f, 0x34, 0x35] {
            assert_eq!(
                retail_physical_slot_count(LevelId::new_const(level)),
                PHYSICAL_SLOT_COUNT,
                "level {level:#04x}"
            );
        }
    }

    #[test]
    fn core_preload_treats_none_as_an_ignored_invalid_reference() {
        let named = Eid::from_name("sHADc").unwrap();
        let malformed = Eid::from_raw(0x1234_5678);

        assert_eq!(
            Pager::validate_executable_eid(29, named),
            Ok(named),
            "required executable slots remain strict"
        );
        assert_eq!(
            Pager::validate_executable_eid(30, Eid::NONE),
            Err(PagingError::InvalidExecutableEid {
                index: 30,
                eid: Eid::NONE,
            }),
            "required executable materialization must not accept the sentinel"
        );
        assert_eq!(
            Pager::validate_preload_executable_eid(30, Eid::NONE),
            Ok(None),
            "retail NSOpen(EID_NONE) fails locally and CoreObjectsCreate ignores it"
        );
        assert_eq!(
            Pager::validate_preload_executable_eid(29, named),
            Ok(Some(named))
        );
        assert_eq!(
            Pager::validate_preload_executable_eid(30, malformed),
            Err(PagingError::InvalidExecutableEid {
                index: 30,
                eid: malformed,
            })
        );
    }

    fn entry(page: u32, index: u16) -> EntryHandle {
        EntryHandle::new(PageIndex::new(page), index)
    }

    fn texture_eid(index: usize) -> Eid {
        const NAMES: [&str; 10] = [
            "Tex0T", "Tex1T", "Tex2T", "Tex3T", "Tex4T", "Tex5T", "Tex6T", "Tex7T", "Tex8T",
            "Tex9T",
        ];
        Eid::from_name(NAMES[index]).unwrap()
    }

    fn register_texture(pager: &mut Pager, index: u32) -> Eid {
        let page = PageIndex::new(index);
        let eid = texture_eid(index as usize);
        pager.register_page(page, []).unwrap();
        pager.bind_page_eid(eid, page).unwrap();
        eid
    }

    #[test]
    fn load_list_differences_keep_balanced_references() {
        let mut pager = Pager::new();
        pager
            .register_page(PageIndex::new(0), [entry(0, 0), entry(0, 1)])
            .unwrap();
        pager
            .register_page(PageIndex::new(1), [entry(1, 0)])
            .unwrap();
        pager
            .apply_load_list(LoadList::new([entry(0, 0)], [PageIndex::new(1)]))
            .unwrap();
        assert_eq!(pager.page(PageIndex::new(0)).unwrap().references, 1);
        assert_eq!(pager.page(PageIndex::new(1)).unwrap().references, 1);
        assert_eq!(pager.entry_references(entry(0, 0)), Some(1));
        assert_eq!(pager.total_entry_references(), 1);
        assert_eq!(pager.total_page_references(), 2);

        pager
            .apply_load_list(LoadList::new([entry(0, 1), entry(1, 0)], []))
            .unwrap();
        assert_eq!(pager.entry_references(entry(0, 0)), Some(0));
        assert_eq!(pager.entry_references(entry(0, 1)), Some(1));
        assert_eq!(pager.entry_references(entry(1, 0)), Some(1));
        assert_eq!(pager.total_entry_references(), 2);
        assert_eq!(pager.total_page_references(), 2);
    }

    #[test]
    fn invalid_list_is_rejected_before_mutation() {
        let mut pager = Pager::new();
        pager
            .register_page(PageIndex::new(0), [entry(0, 0)])
            .unwrap();
        let previous = pager.clone();
        assert_eq!(
            pager.apply_load_list(LoadList::new([entry(5, 0)], [])),
            Err(PagingError::UnknownEntry(entry(5, 0)))
        );
        assert_eq!(pager, previous);
    }

    #[test]
    fn texture_reuse_advances_generation_and_stales_previous_page() {
        let mut pager = Pager::new();
        register_texture(&mut pager, 0);
        register_texture(&mut pager, 1);
        assert_eq!(pager.materialize_texture(7, PageIndex::new(0)), Ok(1));
        assert_eq!(pager.materialize_texture(7, PageIndex::new(1)), Ok(2));
        assert_eq!(
            pager.page(PageIndex::new(0)).unwrap().state,
            PageState::Stale
        );
        assert_eq!(pager.texture_slot(7), Some((PageIndex::new(1), 2)));
    }

    #[test]
    fn texture_allocator_uses_free_slots_high_to_low_and_reuses_an_eid() {
        let mut pager = Pager::new();
        let first = register_texture(&mut pager, 0);
        let second = register_texture(&mut pager, 1);
        pager.set_current_texture_load_eids([first, second]);

        let first_assignment = pager.materialize_texture_eid(first).unwrap();
        let second_assignment = pager.materialize_texture_eid(second).unwrap();
        let repeated = pager.materialize_texture_eid(first).unwrap();

        assert_eq!(first_assignment.slot, 7);
        assert_eq!(second_assignment.slot, 6);
        assert!(first_assignment.changed);
        assert!(!repeated.changed);
        assert_eq!(repeated.slot, 7);
        assert_eq!(
            repeated.binding.generation,
            first_assignment.binding.generation
        );
    }

    #[test]
    fn stale_slot_is_preferred_before_an_unprotected_resident_slot() {
        let mut pager = Pager::new();
        let eids = (0..=8)
            .map(|index| register_texture(&mut pager, index))
            .collect::<Vec<_>>();
        pager.set_current_texture_load_eids(eids.iter().take(8).copied());
        for eid in eids.iter().take(8).copied() {
            pager.materialize_texture_eid(eid).unwrap();
        }
        pager.mark_texture_slot_stale(2).unwrap();
        // A resident slot is also unprotected, but native's stale pass comes
        // first regardless of its lower slot index.
        pager.set_current_texture_load_eids(eids.iter().take(7).copied());

        let assignment = pager.materialize_texture_eid(eids[8]).unwrap();

        assert_eq!(assignment.slot, 2);
        assert_eq!(assignment.replaced.unwrap().eid, eids[5]);
    }

    #[test]
    fn replacement_skips_current_zone_eids_and_scans_high_to_low() {
        let mut pager = Pager::new();
        let eids = (0..=8)
            .map(|index| register_texture(&mut pager, index))
            .collect::<Vec<_>>();
        for eid in eids.iter().take(8).copied() {
            pager.materialize_texture_eid(eid).unwrap();
        }
        // The first materialization occupies slot 7 and the last slot 0.
        // Protect every page except the one in slot 6.
        pager.set_current_texture_load_eids(
            eids.iter().take(8).copied().filter(|eid| *eid != eids[1]),
        );

        let assignment = pager.materialize_texture_eid(eids[8]).unwrap();

        assert_eq!(assignment.slot, 6);
        assert_eq!(assignment.replaced.unwrap().eid, eids[1]);
    }

    #[test]
    fn fully_protected_slots_reject_a_ninth_texture_page() {
        let mut pager = Pager::new();
        let eids = (0..=8)
            .map(|index| register_texture(&mut pager, index))
            .collect::<Vec<_>>();
        pager.set_current_texture_load_eids(eids.iter().take(8).copied());
        for eid in eids.iter().take(8).copied() {
            pager.materialize_texture_eid(eid).unwrap();
        }

        assert_eq!(
            pager.materialize_texture_eid(eids[8]),
            Err(PagingError::NoFreeTextureSlot(eids[8]))
        );
    }

    #[test]
    fn load_list_retains_a_protected_ninth_texture_as_a_virtual_request() {
        let mut pager = Pager::new();
        let eids = (0..=8)
            .map(|index| register_texture(&mut pager, index))
            .collect::<Vec<_>>();
        pager.set_current_texture_load_eids(eids.iter().copied());
        pager
            .apply_load_list(LoadList::new([], (0..=8).map(PageIndex::new)))
            .unwrap();

        for index in 0..=8 {
            let page = PageIndex::new(index);
            assert_eq!(pager.page(page).unwrap().state, PageState::Queued);
            assert_eq!(pager.page(page).unwrap().references, 1);
        }
        for expected in 0..8 {
            let promoted = pager
                .update_pending_virtual_page()
                .unwrap()
                .expect("the first eight protected textures have slots");
            assert_eq!(promoted.page, PageIndex::new(expected));
        }
        let queued = PageIndex::new(8);
        assert_eq!(pager.update_pending_virtual_page(), Ok(None));
        assert_eq!(pager.page(queued).unwrap().state, PageState::Queued);
        assert!(!pager.resolved_pages().any(|page| page == queued));
    }

    #[test]
    fn null_zone_fallback_replaces_highest_slot_after_free_and_stale_passes() {
        let mut pager = Pager::new();
        let eids = (0..=8)
            .map(|index| register_texture(&mut pager, index))
            .collect::<Vec<_>>();
        pager.set_current_texture_load_eids(eids.iter().take(8).copied());
        for eid in eids.iter().take(8).copied() {
            pager.materialize_texture_eid(eid).unwrap();
        }
        pager.clear_current_texture_zone();

        let assignment = pager.materialize_texture_eid(eids[8]).unwrap();

        assert_eq!(assignment.slot, 7);
        assert_eq!(assignment.replaced.unwrap().eid, eids[0]);
    }

    #[test]
    fn frame_snapshot_remains_stable_after_mid_frame_replacement() {
        let mut pager = Pager::new();
        let eids = (0..=8)
            .map(|index| register_texture(&mut pager, index))
            .collect::<Vec<_>>();
        pager.set_current_texture_load_eids(eids.iter().take(8).copied());
        for eid in eids.iter().take(8).copied() {
            pager.materialize_texture_eid(eid).unwrap();
        }
        let frame = pager.texture_frame_snapshot();
        pager.set_current_texture_load_eids(eids.iter().skip(1).copied());

        let replacement = pager.materialize_texture_eid(eids[8]).unwrap();

        assert_eq!(replacement.slot, 7);
        assert_eq!(frame.slot(7).unwrap().eid, eids[0]);
        assert_eq!(pager.texture_frame_snapshot().slot(7).unwrap().eid, eids[8]);
        assert_ne!(
            frame.slot(7).unwrap().generation,
            pager.texture_frame_snapshot().slot(7).unwrap().generation
        );
        assert_eq!(frame.find_eid(eids[0]).unwrap().0, 7);
        assert!(pager.texture_frame_snapshot().find_eid(eids[0]).is_none());
    }

    #[test]
    fn freeing_a_slot_keeps_its_identity_until_overwrite() {
        let mut pager = Pager::new();
        let eid = register_texture(&mut pager, 0);
        let assignment = pager.materialize_texture_eid(eid).unwrap();

        pager.free_texture_slot(assignment.slot).unwrap();

        let binding = pager
            .texture_frame_snapshot()
            .slot(assignment.slot)
            .unwrap();
        assert_eq!(binding.eid, eid);
        assert_eq!(binding.state, TextureSlotState::Free);
        assert_eq!(binding.generation, assignment.binding.generation);
    }

    #[test]
    fn reopening_a_freed_identity_reallocates_and_advances_its_generation() {
        let mut pager = Pager::new();
        let eids = (0..8)
            .map(|index| register_texture(&mut pager, index))
            .collect::<Vec<_>>();
        pager.set_current_texture_load_eids(eids.iter().copied());
        let first = pager.materialize_texture_eid(eids[0]).unwrap();
        for eid in eids.iter().skip(1).copied() {
            pager.materialize_texture_eid(eid).unwrap();
        }
        pager.free_texture_slot(first.slot).unwrap();

        let reopened = pager.materialize_texture_eid(eids[0]).unwrap();

        assert!(reopened.changed);
        assert_eq!(reopened.slot, 7);
        assert_eq!(reopened.replaced.unwrap().state, TextureSlotState::Free);
        assert_eq!(reopened.binding.generation, first.binding.generation + 1);
        assert_eq!(reopened.binding.state, TextureSlotState::Resident);
    }

    #[test]
    fn reopening_a_stale_identity_reallocates_and_advances_its_generation() {
        let mut pager = Pager::new();
        let eids = (0..8)
            .map(|index| register_texture(&mut pager, index))
            .collect::<Vec<_>>();
        pager.set_current_texture_load_eids(eids.iter().copied());
        let first = pager.materialize_texture_eid(eids[0]).unwrap();
        for eid in eids.iter().skip(1).copied() {
            pager.materialize_texture_eid(eid).unwrap();
        }
        pager.mark_texture_slot_stale(first.slot).unwrap();

        let reopened = pager.materialize_texture_eid(eids[0]).unwrap();

        assert!(reopened.changed);
        assert_eq!(reopened.slot, 7);
        assert_eq!(reopened.replaced.unwrap().state, TextureSlotState::Stale);
        assert_eq!(reopened.binding.generation, first.binding.generation + 1);
        assert_eq!(reopened.binding.state, TextureSlotState::Resident);
    }

    #[test]
    fn eid_binding_uses_typed_format_handles() {
        let mut pager = Pager::new();
        let handle = entry(0, 4);
        let eid = Eid::from_name("0c_pZ").unwrap();
        pager.register_page(PageIndex::new(0), [handle]).unwrap();
        pager.bind_eid(eid, handle).unwrap();
        assert_eq!(pager.resolve_eid(eid), Ok(handle));
    }

    #[test]
    fn texture_page_eids_share_the_native_open_close_namespace() {
        let mut pager = Pager::new();
        let page = PageIndex::new(7);
        let eid = Eid::from_name("WillT").unwrap();
        pager.register_page(page, []).unwrap();
        pager.bind_page_eid(eid, page).unwrap();

        pager.open_eid(eid).unwrap();
        assert_eq!(pager.page(page).unwrap().references, 1);
        assert_eq!(pager.page(page).unwrap().state, PageState::Resident);
        assert_eq!(pager.texture_slot_binding(7).unwrap().eid, eid);
        assert_eq!(pager.resolve_eid(eid), Err(PagingError::UnknownEid(eid)));

        pager.close_eid(eid).unwrap();
        assert_eq!(pager.page(page).unwrap().references, 0);
        assert_eq!(
            pager.bind_page_eid(eid, page),
            Err(PagingError::DuplicateEid(eid))
        );
    }

    #[test]
    fn open_outcome_reports_only_a_displaced_resident_texture_page() {
        let mut pager = Pager::new();
        let eids = (0..=8)
            .map(|index| register_texture(&mut pager, index))
            .collect::<Vec<_>>();
        pager.set_current_texture_load_eids(eids.iter().take(8).copied());
        for eid in eids.iter().take(8).copied() {
            let outcome = pager.open_eid_with_outcome(eid).unwrap();
            assert_eq!(outcome.evicted, None);
            assert_eq!(outcome.invalidated, PageInvalidations::NONE);
        }
        pager.set_current_texture_load_eids(eids.iter().skip(1).copied());

        let replacement = pager.open_eid_with_outcome(eids[8]).unwrap();

        assert_eq!(replacement.page, PageIndex::new(8));
        let evicted = replacement.evicted.unwrap();
        assert_eq!(evicted.page, PageIndex::new(0));
        assert_eq!(evicted.eid, eids[0]);
        assert_eq!(evicted.state, TextureSlotState::Resident);
        assert_eq!(
            replacement.invalidated,
            PageInvalidations::one(evicted.page)
        );
        assert!(!pager.resolved_pages().any(|page| page == evicted.page));
        assert!(pager.resolved_pages().any(|page| page == replacement.page));
    }

    #[test]
    fn free_and_stale_texture_identities_are_not_reported_as_new_evictions() {
        for retained_state in [TextureSlotState::Free, TextureSlotState::Stale] {
            let mut pager = Pager::new();
            let first = register_texture(&mut pager, 0);
            let second = register_texture(&mut pager, 1);
            pager.set_current_texture_load_eids([second]);
            let opened = pager.open_eid_with_outcome(first).unwrap();
            let slot = pager.texture_frame_snapshot().find_eid(first).unwrap().0;
            match retained_state {
                TextureSlotState::Free => pager.free_texture_slot(slot).unwrap(),
                TextureSlotState::Stale => pager.mark_texture_slot_stale(slot).unwrap(),
                TextureSlotState::Resident => unreachable!(),
            }

            let replacement = pager.open_eid_with_outcome(second).unwrap();

            assert_eq!(replacement.evicted, None, "{retained_state:?}");
            assert_eq!(
                replacement.invalidated,
                PageInvalidations::NONE,
                "{retained_state:?}"
            );
            assert!(!pager.resolved_pages().any(|page| page == opened.page));
            assert!(pager.resolved_pages().any(|page| page == replacement.page));
        }
    }

    #[test]
    fn gool_close_is_idempotent_without_weakening_strict_lifecycle_close() {
        let mut pager = Pager::new();
        let page = PageIndex::new(2);
        let handle = entry(2, 0);
        let eid = Eid::from_name("WillG").unwrap();
        pager.register_page(page, [handle]).unwrap();
        pager.bind_eid(eid, handle).unwrap();

        pager.close_eid_retail(eid).unwrap();
        assert_eq!(pager.entry_references(handle), Some(0));
        assert_eq!(pager.page(page).unwrap().references, 0);
        assert_eq!(
            pager.close_eid(eid),
            Err(PagingError::ReferenceUnderflow(page))
        );

        pager.open_eid(eid).unwrap();
        pager.close_eid_retail(eid).unwrap();
        pager.close_eid_retail(eid).unwrap();
        assert_eq!(pager.entry_references(handle), Some(0));
        assert_eq!(pager.page(page).unwrap().references, 0);

        pager.close_page_retail(page).unwrap();
        assert_eq!(pager.page(page).unwrap().references, 0);
        assert_eq!(
            pager.close_page(page),
            Err(PagingError::ReferenceUnderflow(page))
        );
    }

    #[test]
    fn retail_close_decrements_the_shared_page_even_when_that_eid_was_not_opened() {
        let mut pager = Pager::new();
        let page = PageIndex::new(3);
        let entry_a = entry(3, 0);
        let entry_b = entry(3, 1);
        let eid_a = Eid::from_name("WillG").unwrap();
        let eid_b = Eid::from_name("WiI1V").unwrap();
        pager.register_page(page, [entry_a, entry_b]).unwrap();
        pager.bind_eid(eid_a, entry_a).unwrap();
        pager.bind_eid(eid_b, entry_b).unwrap();

        pager.open_eid(eid_b).unwrap();
        assert_eq!(pager.entry_references(entry_a), Some(0));
        assert_eq!(pager.entry_references(entry_b), Some(1));
        assert_eq!(pager.page(page).unwrap().references, 1);

        assert_eq!(
            pager.close_eid_retail_with_outcome(eid_a).unwrap(),
            PagerCloseOutcome {
                page,
                decremented: true,
                unresolved: false,
            }
        );
        assert_eq!(pager.entry_references(entry_a), Some(0));
        assert_eq!(pager.entry_references(entry_b), Some(1));
        assert_eq!(pager.page(page).unwrap().references, 0);

        assert_eq!(
            pager.close_eid_retail_with_outcome(eid_b).unwrap(),
            PagerCloseOutcome {
                page,
                decremented: false,
                unresolved: false,
            }
        );
        pager.close_eid_retail(eid_b).unwrap();
        assert_eq!(pager.entry_references(entry_b), Some(0));
        assert_eq!(pager.page(page).unwrap().references, 0);
    }

    #[test]
    fn retail_eid_close_does_not_decrement_resolved_copied_texture_or_audio_ptes() {
        let mut pager = Pager::new();
        let texture_eid = register_texture(&mut pager, 0);
        let texture_page = PageIndex::new(0);
        pager.open_eid(texture_eid).unwrap();

        let audio_page = PageIndex::new(1);
        let audio_entry = entry(1, 0);
        let audio_eid = Eid::from_name("WiI1V").unwrap();
        pager.register_page(audio_page, [audio_entry]).unwrap();
        pager.bind_eid(audio_eid, audio_entry).unwrap();
        pager.audio_pages.insert(audio_page);
        pager.open_eid(audio_eid).unwrap();

        for (eid, page, entry) in [
            (texture_eid, texture_page, None),
            (audio_eid, audio_page, Some(audio_entry)),
        ] {
            assert_eq!(
                pager.close_eid_retail_with_outcome(eid).unwrap(),
                PagerCloseOutcome {
                    page,
                    decremented: false,
                    unresolved: false,
                }
            );
            assert_eq!(pager.page(page).unwrap().references, 1);
            if let Some(entry) = entry {
                assert_eq!(pager.entry_references(entry), Some(1));
            }
        }
    }

    #[test]
    fn free_ram_virtual_open_returns_queued_until_nsupdate_promotes_it() {
        let mut pager = Pager::new();
        let page = PageIndex::new(3);
        let entry = entry(3, 0);
        let eid = Eid::from_name("WillG").unwrap();
        pager.register_page(page, [entry]).unwrap();
        pager.bind_eid(eid, entry).unwrap();

        let opened = pager.open_eid_virtual_with_outcome(eid).unwrap();
        assert!(!opened.resolved, "fresh flag-zero NSOpen returns null");
        assert_eq!(opened.invalidated, PageInvalidations::NONE);
        assert_eq!(pager.page(page).unwrap().ordinary_kind(), None);
        assert_eq!(pager.page(page).unwrap().state, PageState::Queued);
        assert_eq!(pager.page(page).unwrap().physical_slot(), None);
        assert!(!pager.resolved_pages().any(|resolved| resolved == page));

        let promoted = pager
            .update_pending_virtual_page()
            .unwrap()
            .expect("free ordinary RAM promotes the queued page on NSUpdate");
        let slot = pager.page(page).unwrap().physical_slot();
        assert!(promoted.resolved);
        assert_eq!(promoted.page, page);
        assert_eq!(
            pager.page(page).unwrap().ordinary_kind(),
            Some(OrdinaryPageKind::Physical)
        );
        assert_eq!(pager.page(page).unwrap().state, PageState::Translated);
        assert!(slot.is_some());
        assert!(pager.resolved_pages().any(|resolved| resolved == page));

        let reopened = pager.open_eid_virtual_with_outcome(eid).unwrap();
        assert!(
            reopened.resolved,
            "an already-resolved PTE opens immediately"
        );
        assert_eq!(pager.page(page).unwrap().references, 2);
        assert_eq!(
            pager.close_eid_retail_with_outcome(eid).unwrap(),
            PagerCloseOutcome {
                page,
                decremented: true,
                unresolved: false,
            }
        );
        assert_eq!(pager.page(page).unwrap().references, 1);
        pager.close_eid_retail(eid).unwrap();
        assert_eq!(pager.page(page).unwrap().references, 0);
        assert_eq!(pager.page(page).unwrap().state, PageState::Translated);
        assert_eq!(pager.page(page).unwrap().physical_slot(), slot);
        assert!(pager.resolved_pages().any(|resolved| resolved == page));
    }

    #[test]
    fn nsupdate_serializes_pending_pages_by_lowest_pgid_not_open_order() {
        let mut pager = Pager::new();
        let pages = [PageIndex::new(7), PageIndex::new(5), PageIndex::new(2)];
        for page in pages {
            pager.register_page(page, []).unwrap();
            let opened = pager.open_page_virtual_with_outcome(page).unwrap();
            assert!(!opened.resolved);
        }

        let mut promoted = Vec::new();
        for _ in pages {
            let outcome = pager
                .update_pending_virtual_page()
                .unwrap()
                .expect("free RAM promotes exactly one queued page");
            promoted.push(outcome.page);
        }

        assert_eq!(
            promoted,
            [PageIndex::new(2), PageIndex::new(5), PageIndex::new(7)]
        );
        assert_eq!(pager.update_pending_virtual_page(), Ok(None));
    }

    #[test]
    fn twenty_two_referenced_pages_block_a_physical_open_transactionally() {
        let mut pager = Pager::new();
        for index in 0..=PHYSICAL_SLOT_COUNT {
            pager
                .register_page(PageIndex::new(index as u32), [])
                .unwrap();
        }
        for index in 0..PHYSICAL_SLOT_COUNT {
            pager.open_page(PageIndex::new(index as u32)).unwrap();
        }
        let before = pager.clone();
        let requested = PageIndex::new(PHYSICAL_SLOT_COUNT as u32);

        assert_eq!(
            pager.open_page(requested),
            Err(PagingError::NoFreePhysicalSlot(requested))
        );
        assert_eq!(pager, before);
        assert_eq!(pager.resident_physical_page_count(), PHYSICAL_SLOT_COUNT);
        assert_eq!(pager.available_physical_page_count(), 0);
    }

    #[test]
    fn saturated_virtual_open_retains_ownership_until_nsupdate_can_promote_it() {
        let mut pager = Pager::new();
        pager.set_physical_slot_count(2).unwrap();
        let eids = (0..3)
            .map(|index| {
                let page = PageIndex::new(index);
                let entry = entry(index, 0);
                let eid = Eid::from_raw(0x7500_2055 + index * 0x1e);
                pager.register_page(page, [entry]).unwrap();
                pager.bind_eid(eid, entry).unwrap();
                eid
            })
            .collect::<Vec<_>>();
        pager.open_eid(eids[0]).unwrap();
        pager.open_eid(eids[1]).unwrap();

        let queued = pager.open_eid_virtual_with_outcome(eids[2]).unwrap();
        assert!(!queued.resolved);
        assert_eq!(queued.invalidated, PageInvalidations::NONE);
        assert_eq!(pager.page(queued.page).unwrap().state, PageState::Queued);
        assert_eq!(pager.page(queued.page).unwrap().references, 1);
        assert_eq!(pager.entry_references(entry(2, 0)), Some(1));
        assert_eq!(pager.update_pending_virtual_page(), Ok(None));

        let released = pager.close_eid_retail_with_outcome(eids[0]).unwrap();
        assert!(released.decremented);
        let promoted = pager
            .update_pending_virtual_page()
            .unwrap()
            .expect("a zero-reference ordinary slot permits the next NSUpdate");
        assert!(promoted.resolved);
        assert_eq!(promoted.page, queued.page);
        assert_eq!(promoted.invalidated, PageInvalidations::one(released.page));
        assert_eq!(
            pager.page(promoted.page).unwrap().state,
            PageState::Translated
        );
        assert_eq!(pager.page(promoted.page).unwrap().references, 1);
        assert!(pager.resolved_pages().any(|page| page == promoted.page));
        assert!(!pager.resolved_pages().any(|page| page == released.page));
    }

    #[test]
    fn final_close_cancels_a_saturated_virtual_request_before_nsupdate() {
        let mut pager = Pager::new();
        pager.set_physical_slot_count(1).unwrap();
        let resident = PageIndex::new(0);
        let queued = PageIndex::new(1);
        let queued_entry = entry(1, 0);
        let queued_eid = Eid::from_name("WillG").unwrap();
        pager.register_page(resident, []).unwrap();
        pager.register_page(queued, [queued_entry]).unwrap();
        pager.bind_eid(queued_eid, queued_entry).unwrap();
        pager.open_page(resident).unwrap();

        assert!(
            !pager
                .open_eid_virtual_with_outcome(queued_eid)
                .unwrap()
                .resolved
        );
        assert_eq!(
            pager.close_eid_retail_with_outcome(queued_eid).unwrap(),
            PagerCloseOutcome {
                page: queued,
                decremented: true,
                unresolved: true,
            }
        );
        assert_eq!(pager.page(queued).unwrap().state, PageState::Raw);
        assert_eq!(pager.page(queued).unwrap().references, 0);
        assert_eq!(pager.entry_references(queued_entry), Some(0));
        assert_eq!(pager.update_pending_virtual_page(), Ok(None));
    }

    #[test]
    fn oldest_zero_reference_page_is_evicted_and_its_pte_is_rearmed() {
        let mut pager = Pager::new();
        for index in 0..=PHYSICAL_SLOT_COUNT {
            pager
                .register_page(PageIndex::new(index as u32), [])
                .unwrap();
        }
        for index in 0..PHYSICAL_SLOT_COUNT {
            let page = PageIndex::new(index as u32);
            pager.open_page(page).unwrap();
            pager.close_page(page).unwrap();
        }
        let evicted = PageIndex::new(0);
        let evicted_slot = pager.page(evicted).unwrap().physical_slot();
        let requested = PageIndex::new(PHYSICAL_SLOT_COUNT as u32);

        let outcome = pager.open_page_with_outcome(requested).unwrap();

        assert_eq!(outcome.invalidated, PageInvalidations::one(evicted));
        assert_eq!(pager.page(evicted).unwrap().state, PageState::Raw);
        assert_eq!(pager.page(evicted).unwrap().physical_slot(), None);
        assert!(!pager.resolved_pages().any(|page| page == evicted));
        assert_eq!(pager.page(requested).unwrap().physical_slot(), evicted_slot);
        assert!(pager.resolved_pages().any(|page| page == requested));
    }

    #[test]
    fn texture_open_reports_both_transfer_ram_and_texture_invalidations() {
        let mut pager = Pager::new();
        let texture_eids = (0..=8)
            .map(|index| register_texture(&mut pager, index))
            .collect::<Vec<_>>();
        for index in 10..=32 {
            pager.register_page(PageIndex::new(index), []).unwrap();
        }
        for index in 10..32 {
            let page = PageIndex::new(index);
            pager.open_page(page).unwrap();
            pager.close_page(page).unwrap();
        }
        pager.set_current_texture_load_eids(texture_eids.iter().take(8).copied());
        for eid in texture_eids.iter().take(8).copied() {
            pager.open_eid(eid).unwrap();
        }
        pager.close_eid(texture_eids[0]).unwrap();

        // The first texture transfer evicted ordinary page 10 and released
        // its staging slot. Refill it so the ninth transfer must evict from
        // both bounded allocators.
        let refill = PageIndex::new(32);
        pager.open_page(refill).unwrap();
        pager.close_page(refill).unwrap();
        pager.set_current_texture_load_eids(texture_eids.iter().skip(1).copied());

        let outcome = pager.open_eid_with_outcome(texture_eids[8]).unwrap();
        let physical_evicted = PageIndex::new(11);
        let texture_evicted = PageIndex::new(0);

        assert_eq!(
            outcome.invalidated,
            PageInvalidations::new(Some(physical_evicted), Some(texture_evicted))
        );
        assert_eq!(outcome.evicted.unwrap().page, texture_evicted);
        assert!(
            !pager
                .resolved_pages()
                .any(|page| page == physical_evicted || page == texture_evicted)
        );
        assert!(pager.resolved_pages().any(|page| page == outcome.page));
    }

    #[test]
    fn close_of_an_evicted_texture_pte_does_not_mutate_its_stranded_references() {
        let mut pager = Pager::new();
        let texture_eids = (0..=TEXTURE_SLOT_COUNT as u32)
            .map(|index| register_texture(&mut pager, index))
            .collect::<Vec<_>>();
        pager.set_current_texture_load_eids(texture_eids.iter().take(TEXTURE_SLOT_COUNT).copied());
        pager.open_eid(texture_eids[0]).unwrap();
        pager.open_eid(texture_eids[0]).unwrap();
        for eid in texture_eids
            .iter()
            .take(TEXTURE_SLOT_COUNT)
            .skip(1)
            .copied()
        {
            pager.open_eid(eid).unwrap();
        }
        let evicted_page = pager.page_eids[&texture_eids[0]];
        assert_eq!(pager.page(evicted_page).unwrap().references, 2);

        pager.set_current_texture_load_eids(
            texture_eids
                .iter()
                .skip(1)
                .take(TEXTURE_SLOT_COUNT - 1)
                .copied(),
        );
        let replacement = pager
            .open_eid_with_outcome(texture_eids[TEXTURE_SLOT_COUNT])
            .unwrap();
        assert!(
            replacement
                .invalidated
                .iter()
                .any(|page| page == evicted_page)
        );
        assert!(!pager.resolved_pages().any(|page| page == evicted_page));

        assert_eq!(
            pager
                .close_eid_retail_with_outcome(texture_eids[0])
                .unwrap(),
            PagerCloseOutcome {
                page: evicted_page,
                decremented: false,
                unresolved: false,
            }
        );
        assert_eq!(pager.page(evicted_page).unwrap().references, 2);
    }

    #[test]
    fn texture_transfer_also_requires_an_ordinary_physical_slot() {
        let mut pager = Pager::new();
        for index in 0..PHYSICAL_SLOT_COUNT {
            let page = PageIndex::new(index as u32);
            pager.register_page(page, []).unwrap();
            pager.open_page(page).unwrap();
        }
        let texture_page = PageIndex::new(PHYSICAL_SLOT_COUNT as u32);
        let texture_eid = texture_eid(0);
        pager.register_page(texture_page, []).unwrap();
        pager.bind_page_eid(texture_eid, texture_page).unwrap();
        let before = pager.clone();

        assert_eq!(
            pager.open_eid(texture_eid),
            Err(PagingError::NoFreePhysicalSlot(texture_page))
        );
        assert_eq!(pager, before);
    }

    #[test]
    fn texture_references_do_not_consume_ns_available_page_count() {
        let mut pager = Pager::new();
        let texture_eid = register_texture(&mut pager, 0);

        pager.open_eid(texture_eid).unwrap();

        assert_eq!(pager.available_physical_page_count(), PHYSICAL_SLOT_COUNT);
        assert_eq!(pager.resident_physical_page_count(), 0);
    }

    #[test]
    fn audio_transfer_releases_ordinary_ram_and_is_not_counted_as_referenced() {
        let mut pager = Pager::new();
        let audio_page = PageIndex::new(0);
        pager.register_page(audio_page, []).unwrap();
        pager.audio_pages.insert(audio_page);

        let outcome = pager.open_page_with_outcome(audio_page).unwrap();

        assert_eq!(outcome.invalidated, PageInvalidations::NONE);
        assert_eq!(pager.page(audio_page).unwrap().state, PageState::Translated);
        assert_eq!(pager.page(audio_page).unwrap().references, 1);
        assert_eq!(pager.page(audio_page).unwrap().physical_slot(), None);
        assert_eq!(pager.resident_physical_page_count(), 0);
        assert_eq!(pager.available_physical_page_count(), PHYSICAL_SLOT_COUNT);
        assert!(pager.uncounted_pages().any(|page| page == audio_page));
    }
}
