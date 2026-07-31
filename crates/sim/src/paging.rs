//! Explicit page, entry, and load-list ownership.

use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU8,
};

use crust_formats::binary::{Eid, EntryHandle, PageIndex};
use crust_formats::stream::{
    LevelId, NSF_PAGE_SECTOR_COUNT, Nsd, Nsf, NsfPage, NsfPageSectorCount,
};

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
/// Retail's double-speed CD transfers 150 sectors per second. The cooperative
/// simulation observes five sectors during each 30 Hz game frame.
const RETAIL_CD_SECTORS_PER_FRAME: u16 = 5;
/// Characterized CdlSeekL/read setup before the first page in a new group.
/// Contiguous pages share this cost and retain only their own transfer time.
const RETAIL_CD_SEEK_SETUP_FRAMES: u16 = 10;

#[derive(Clone, Debug, Eq, PartialEq)]
struct RetailCdTransfer {
    pages: Vec<RetailCdPage>,
    next: usize,
    frames_remaining: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RetailCdPage {
    page: PageIndex,
    reservation: Option<RetailCdReservation>,
    /// False after native-style cancellation or synchronous materialization;
    /// the combined read continues, but this cloned member is not published.
    cloned: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RetailCdReservation {
    slot: u8,
}

enum PendingPageUpdate {
    Idle,
    Waiting,
    Stalled,
    Invalidated(Vec<PageIndex>),
    Resolved(PagerOpenOutcome),
}

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
    /// Native texture-page state 30: VRAM is owned by title-card CLUT data.
    ///
    /// The last copied page identity remains available to frame diagnostics,
    /// but neither the free/stale passes nor the ordinary replacement pass may
    /// allocate this slot until the title runtime explicitly releases it.
    Reserved,
}

/// Immutable identity of one physical texture slot generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextureSlotBinding {
    pub page: PageIndex,
    pub eid: Eid,
    pub generation: u32,
    pub state: TextureSlotState,
}

/// Pointer-free texture identity retained while native changes NSF streams.
///
/// A destination stream has its own page table, so a carried texture must not
/// retain the source stream's [`PageIndex`]. The destination pager resolves
/// this EID again and either binds the same VRAM slot to its local page or
/// keeps the identity stale until that slot is replaced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextureSlotCarryBinding {
    pub eid: Eid,
    pub generation: u32,
    pub state: TextureSlotState,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct TextureSlotCarryRecord {
    eid: Option<Eid>,
    generation: u32,
    state: TextureSlotState,
}

/// Exact eight-slot state passed from `NSKill` to `NSInitTexturePages`.
///
/// The representation deliberately contains no page indices, offsets, or
/// borrowed stream data. It is therefore safe to carry between independently
/// parsed streams without recreating native's cross-NSF pointers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TextureSlotCarrySnapshot {
    slots: [TextureSlotCarryRecord; TEXTURE_SLOT_COUNT],
}

impl TextureSlotCarrySnapshot {
    /// Constructs a validated carry snapshot for tests and non-stream hosts.
    /// Empty slots start as native state 1 (`Free`) at generation zero.
    pub fn try_from_bindings(
        slots: [Option<TextureSlotCarryBinding>; TEXTURE_SLOT_COUNT],
    ) -> Result<Self, PagingError> {
        let snapshot = Self {
            slots: std::array::from_fn(|slot| match slots[slot] {
                Some(binding) => TextureSlotCarryRecord {
                    eid: Some(binding.eid),
                    generation: binding.generation,
                    state: binding.state,
                },
                None => TextureSlotCarryRecord::default(),
            }),
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Resident/stale identity retained in one native slot.
    #[must_use]
    pub fn binding(&self, slot: usize) -> Option<TextureSlotCarryBinding> {
        let record = self.slots.get(slot)?;
        record.eid.map(|eid| TextureSlotCarryBinding {
            eid,
            generation: record.generation,
            state: record.state,
        })
    }

    /// Source slot state before destination initialization resets it.
    #[must_use]
    pub fn state(&self, slot: usize) -> Option<TextureSlotState> {
        self.slots.get(slot).map(|record| record.state)
    }

    /// Monotonic renderer generation retained for this physical VRAM slot.
    #[must_use]
    pub fn generation(&self, slot: usize) -> Option<u32> {
        self.slots.get(slot).map(|record| record.generation)
    }

    fn validate(&self) -> Result<(), PagingError> {
        let mut eids = BTreeSet::new();
        for (slot, record) in self.slots.iter().copied().enumerate() {
            match record.state {
                TextureSlotState::Resident | TextureSlotState::Stale => {
                    let eid = record
                        .eid
                        .ok_or(PagingError::MissingTextureCarryEid(slot))?;
                    if eid == Eid::NONE || !eid.is_named() {
                        return Err(PagingError::InvalidTextureCarryEid { slot, eid });
                    }
                    if !eids.insert(eid) {
                        return Err(PagingError::DuplicateTextureCarryEid(eid));
                    }
                }
                TextureSlotState::Free | TextureSlotState::Reserved => {
                    if record.eid.is_some() {
                        return Err(PagingError::InvalidTextureCarryState(slot));
                    }
                }
            }
        }
        Ok(())
    }
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

/// One ordered pager effect performed by native `CoreObjectsCreate`.
///
/// Count-zero materialization changes resolution/eviction state without
/// acquiring ownership. Flag-zero open acquires one reference even when the
/// page remains queued, so hosts must publish the two cases differently.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetailCorePagePreloadOutcome {
    Materialize(PagerOpenOutcome),
    Open(PagerOpenOutcome),
}

/// One observable frame-boundary change produced by `NSUpdate(-1)`.
///
/// A CD group re-arms every zero-reference victim PTE when its complete
/// physical run is reserved, before any member finishes reading. The victim
/// list is distinct from [`PageInvalidations`]: one group may reserve and
/// invalidate all twenty-two ordinary slots at once.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PagerUpdateOutcome {
    /// Transactional reservation-start invalidations, in physical-slot order.
    Invalidated(Vec<PageIndex>),
    /// One progressively published page from the active read group.
    Resolved(PagerOpenOutcome),
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

/// Whether the initial `LevelUpdate` drains its flag-zero virtual opens.
///
/// Native derives this choice from a local activation flag. In particular,
/// `LdatInit` calls `LevelUpdate(..., flags = 0)` while title/PBAK state is
/// still active, which clears that flag and skips the PSX `NSUpdate2` drain.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RetailLevelMountPageUpdate {
    /// Run the initial synchronous `NSUpdate2` before core-object preloads.
    #[default]
    Drain,
    /// Leave the initial load-list pages queued for following `NSUpdate(-1)`s.
    Defer,
}

/// Host-selected policy for reconstructing the initial retail level mount.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailLevelMountOptions {
    physical_slot_count: usize,
    page_update: RetailLevelMountPageUpdate,
    initial_visibility_list: Option<Eid>,
    core_page_preloads: bool,
    texture_slot_carry: Option<TextureSlotCarrySnapshot>,
}

impl RetailLevelMountOptions {
    /// Uses the characterized heap-page count and ordinary synchronous drain.
    #[must_use]
    pub const fn new(level: LevelId) -> Self {
        Self {
            physical_slot_count: retail_physical_slot_count(level),
            page_update: RetailLevelMountPageUpdate::Drain,
            initial_visibility_list: None,
            core_page_preloads: true,
            texture_slot_carry: None,
        }
    }

    /// Overrides the characterized PS1 heap-page count.
    #[must_use]
    pub const fn with_physical_slot_count(mut self, physical_slot_count: usize) -> Self {
        self.physical_slot_count = physical_slot_count;
        self
    }

    /// Selects whether the initial `LevelUpdate` performs `NSUpdate2`.
    #[must_use]
    pub const fn with_page_update(mut self, page_update: RetailLevelMountPageUpdate) -> Self {
        self.page_update = page_update;
        self
    }

    /// Supplies the initial camera path's SLST when the spawn zone owns world
    /// geometry. Native opens and closes this entry physically before it
    /// installs the zone load list, even when the following `NSUpdate2` is
    /// suppressed by title/PBAK state.
    #[must_use]
    pub const fn with_initial_visibility_list(mut self, visibility_list: Option<Eid>) -> Self {
        self.initial_visibility_list = visibility_list;
        self
    }

    /// Selects whether `CoreObjectsCreate` page opens run inside the mount.
    ///
    /// Browser title mounts defer these until `TitleLoadNextState` has loaded
    /// its MDAT graph, matching the subsystem init2 order before the external
    /// `CoreObjectsCreate` call.
    #[must_use]
    pub const fn with_core_page_preloads(mut self, enabled: bool) -> Self {
        self.core_page_preloads = enabled;
        self
    }

    /// Imports the previous stream's eight lower-VRAM texture identities
    /// before any destination page is opened.
    #[must_use]
    pub const fn with_texture_slot_carry(mut self, carry: TextureSlotCarrySnapshot) -> Self {
        self.texture_slot_carry = Some(carry);
        self
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
    ReservedTextureSlot(usize),
    MissingTextureCarryEid(usize),
    InvalidTextureCarryEid { slot: usize, eid: Eid },
    InvalidTextureCarryState(usize),
    DuplicateTextureCarryEid(Eid),
    TextureCarryEidIsNotTexture(Eid),
    TextureCarryDestinationNotFresh(usize),
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
    /// Slot identity is independent of the current stream's page indices.
    /// A stale texture absent from the destination has an EID but no page.
    texture_slot_eids: [Option<Eid>; TEXTURE_SLOT_COUNT],
    texture_slot_states: [TextureSlotState; TEXTURE_SLOT_COUNT],
    texture_generations: [u32; TEXTURE_SLOT_COUNT],
    physical_slots: [Option<PageIndex>; PHYSICAL_SLOT_COUNT],
    /// Runtime count returned by retail's descending 64 KiB heap probe.
    /// `None` is the nominal twenty-two-page maximum.
    physical_slot_count: Option<NonZeroU8>,
    physical_clock: u64,
    /// Validated per-page CD lengths. `None` keeps authored/synthetic pagers
    /// synchronous; stream-backed production pagers use retail transfer time.
    cd_page_sectors: Option<Vec<NsfPageSectorCount>>,
    cd_transfer: Option<RetailCdTransfer>,
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
        let mut page_sectors = Vec::with_capacity(metadata.header.page_count as usize);
        for index in 0..metadata.header.page_count {
            let page = PageIndex::new(index);
            page_sectors.push(
                metadata
                    .header
                    .page_sector_count(page)
                    .ok_or(PagingError::UnknownPage(page))?,
            );
        }
        pager.cd_page_sectors = Some(page_sectors);
        Ok(pager)
    }

    /// Reconstructs the ordinary initial retail mount through `LdatInit`,
    /// `LevelUpdate`/`NSUpdate2`, and `CoreObjectsCreate` page ownership.
    ///
    /// This retains the historical synchronous-drain behavior. Hosts mounting
    /// title/PBAK attract gameplay should use
    /// [`Self::mount_retail_level_with_options`] to defer the initial drain.
    pub fn mount_retail_level(
        metadata: &Nsd,
        nsf: &Nsf,
        level: LevelId,
        initial_zone: Eid,
        load_entry_eids: impl IntoIterator<Item = Eid>,
        load_pages: impl IntoIterator<Item = PageIndex>,
    ) -> Result<Self, PagingError> {
        Self::mount_retail_level_with_options(
            metadata,
            nsf,
            level,
            initial_zone,
            load_entry_eids,
            load_pages,
            RetailLevelMountOptions::new(level),
        )
    }

    /// Retail mount with an explicitly characterized PS1 heap-page count and
    /// the historical synchronous initial drain.
    pub fn mount_retail_level_with_physical_slot_count(
        metadata: &Nsd,
        nsf: &Nsf,
        level: LevelId,
        initial_zone: Eid,
        load_entry_eids: impl IntoIterator<Item = Eid>,
        load_pages: impl IntoIterator<Item = PageIndex>,
        physical_slot_count: usize,
    ) -> Result<Self, PagingError> {
        Self::mount_retail_level_with_options(
            metadata,
            nsf,
            level,
            initial_zone,
            load_entry_eids,
            load_pages,
            RetailLevelMountOptions::new(level).with_physical_slot_count(physical_slot_count),
        )
    }

    /// Retail mount with host-selected heap and initial `LevelUpdate` policy.
    ///
    /// `CoreObjectsCreate` always follows the optional drain, so its flag-zero
    /// preloads remain queued in either mode unless an explicit count-zero
    /// materialization resolves their shared page first.
    pub fn mount_retail_level_with_options(
        metadata: &Nsd,
        nsf: &Nsf,
        level: LevelId,
        initial_zone: Eid,
        load_entry_eids: impl IntoIterator<Item = Eid>,
        load_pages: impl IntoIterator<Item = PageIndex>,
        options: RetailLevelMountOptions,
    ) -> Result<Self, PagingError> {
        let mut pager = Self::from_stream(metadata, nsf)?;
        pager.set_physical_slot_count(options.physical_slot_count)?;
        if let Some(carry) = options.texture_slot_carry {
            pager.import_texture_slot_carry(carry)?;
        }
        let load_entry_eids = load_entry_eids.into_iter().collect::<Vec<_>>();

        // LdatInit physically opens the spawn ZDAT before LevelUpdate. The two
        // hog streams intentionally acquire a second reference and retain one
        // after the matching single close below.
        let spawn_references = usize::from(matches!(level.get(), 0x11 | 0x1e)) + 1;
        for _ in 0..spawn_references {
            pager.open_eid(initial_zone)?;
        }
        // Initial `LevelUpdate` reconstructs the selected path's polygon list
        // before it unloads/opens any zone load-list owners. The SLST reference
        // is temporary, but its physical materialization and close affect the
        // exact eviction order of the PS1 heap.
        if let Some(visibility_list) = options.initial_visibility_list {
            pager.open_eid(visibility_list)?;
            pager.close_eid_retail(visibility_list)?;
        }
        // Only the following zone-change branch installs `cur_zone` and its
        // texture load-list protection. The temporary ZDAT/SLST opens above
        // run with no current texture zone, just like native's null-origin
        // `LevelUpdate`.
        pager.set_current_texture_load_eids(load_entry_eids.iter().copied());
        for eid in load_entry_eids {
            pager.open_eid_virtual_with_outcome(eid)?;
        }
        for page in load_pages {
            pager.open_page_virtual_with_outcome(page)?;
        }
        // Initial `LevelUpdate` opens the complete destination load list
        // virtually. Its effective native marker normally calls `NSUpdate2`,
        // but title/PBAK state can reduce that marker to zero even for a
        // gameplay stream. Drain only when the host reports that native did.
        if options.page_update == RetailLevelMountPageUpdate::Drain {
            pager.update_all_pending_virtual_pages()?;
        }
        pager.close_eid_retail(initial_zone)?;
        if options.core_page_preloads {
            pager.stage_retail_core_page_preloads(metadata, level)?;
        }
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

    /// Applies `CoreObjectsCreate`'s ordered count-zero materializations and
    /// flag-zero retained preloads, returning every pager publication for a
    /// host that deferred this phase past title subsystem init2.
    pub fn stage_retail_core_page_preloads(
        &mut self,
        metadata: &Nsd,
        level: LevelId,
    ) -> Result<Vec<RetailCorePagePreloadOutcome>, PagingError> {
        let mut outcomes = Vec::new();
        let materialize = |pager: &mut Self, outcomes: &mut Vec<_>, index| {
            outcomes.push(RetailCorePagePreloadOutcome::Materialize(
                pager.materialize_eid_with_outcome(Self::executable_eid(metadata, index)?)?,
            ));
            Ok::<(), PagingError>(())
        };
        let open = |pager: &mut Self, outcomes: &mut Vec<_>, index| {
            if let Some(eid) = Self::preload_executable_eid(metadata, index)? {
                outcomes.push(RetailCorePagePreloadOutcome::Open(
                    pager.open_eid_virtual_with_outcome(eid)?,
                ));
            }
            Ok::<(), PagingError>(())
        };

        if level == LevelId::TITLE {
            for index in [4, 52] {
                open(self, &mut outcomes, index)?;
            }
            return Ok(outcomes);
        }
        if level == LevelId::LEVEL_COMPLETE {
            for index in [29, 30, 3] {
                open(self, &mut outcomes, index)?;
            }
            return Ok(outcomes);
        }
        if level == LevelId::INTRO || level == LevelId::ENDING {
            return Ok(outcomes);
        }

        materialize(self, &mut outcomes, 4)?;
        for index in [0, 5, 29] {
            open(self, &mut outcomes, index)?;
        }
        if level != LevelId::new_const(0x2c) {
            open(self, &mut outcomes, 34)?;
        }
        for index in [3, 4] {
            open(self, &mut outcomes, index)?;
        }
        if let Some(index) = match level.get() {
            0x05 => Some(9),
            0x14 | 0x16 => Some(23),
            0x17 => Some(39),
            0x22 | 0x2e => Some(53),
            _ => None,
        } {
            materialize(self, &mut outcomes, index)?;
        }
        Ok(outcomes)
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
    /// Retail chooses the lowest pending pgid. Stream-backed pagers start one
    /// source-ordered contiguous CD group and expose at most one completed
    /// page per cooperative frame; synthetic pagers retain the immediate
    /// promotion used by focused allocator tests. A request remains queued
    /// when its transfer is incomplete or no physical resource is replaceable.
    /// Callers can loop this operation for `NSUpdate2` behavior.
    pub fn update_pending_virtual_page(
        &mut self,
    ) -> Result<Option<PagerUpdateOutcome>, PagingError> {
        Ok(match self.update_pending_virtual_page_step()? {
            PendingPageUpdate::Invalidated(pages) => Some(PagerUpdateOutcome::Invalidated(pages)),
            PendingPageUpdate::Resolved(outcome) => Some(PagerUpdateOutcome::Resolved(outcome)),
            PendingPageUpdate::Idle | PendingPageUpdate::Waiting | PendingPageUpdate::Stalled => {
                None
            }
        })
    }

    fn update_pending_virtual_page_step(&mut self) -> Result<PendingPageUpdate, PagingError> {
        if self.cd_page_sectors.is_some() {
            return self.update_pending_cd_page();
        }
        let Some(page) = self
            .pages
            .values()
            .filter(|record| record.state == PageState::Queued)
            .map(|record| record.index)
            .min()
        else {
            return Ok(PendingPageUpdate::Idle);
        };
        match self.open_page_with_reference_outcome(page, false, OrdinaryPageKind::Physical) {
            Ok(outcome) => Ok(PendingPageUpdate::Resolved(outcome)),
            Err(PagingError::NoFreePhysicalSlot(_) | PagingError::NoFreeTextureSlot(_)) => {
                Ok(PendingPageUpdate::Stalled)
            }
            Err(error) => Err(error),
        }
    }

    fn update_pending_cd_page(&mut self) -> Result<PendingPageUpdate, PagingError> {
        if self.cd_transfer.is_none() {
            let Some(first) = self.lowest_queued_page() else {
                return Ok(PendingPageUpdate::Idle);
            };
            let Some(invalidated) = self.start_cd_transfer(first)? else {
                return Ok(PendingPageUpdate::Stalled);
            };
            // The cloning NSUpdate starts the asynchronous seek/read. Its
            // first transfer frame is observed by the following NSUpdate,
            // matching the independent PCSX PTE tick boundary.
            return Ok(if invalidated.is_empty() {
                PendingPageUpdate::Waiting
            } else {
                PendingPageUpdate::Invalidated(invalidated)
            });
        }

        let (page, frames_remaining, cloned) = {
            let transfer = self
                .cd_transfer
                .as_ref()
                .expect("a CD transfer was started above");
            let page = transfer.pages[transfer.next];
            (page.page, transfer.frames_remaining, page.cloned)
        };
        if frames_remaining > 1 {
            self.cd_transfer
                .as_mut()
                .expect("the active transfer still exists")
                .frames_remaining -= 1;
            return Ok(PendingPageUpdate::Waiting);
        }

        if !cloned {
            self.advance_cd_transfer();
            return Ok(PendingPageUpdate::Waiting);
        }
        if self.pages[&page].state != PageState::Queued {
            self.cancel_cd_page_clone(page);
            self.advance_cd_transfer();
            return Ok(PendingPageUpdate::Waiting);
        }

        // Publication is transactional with texture/audio allocation. A
        // failed destination allocation retains the already-read physical
        // reservation and retries without consuming another transfer frame.
        let mut preview = self.clone();
        let reservation = preview
            .take_cd_page_reservation(page)
            .expect("every active cloned CD page owns one physical reservation");
        match preview.open_reserved_cd_page_in_place(
            page,
            reservation,
            false,
            OrdinaryPageKind::Physical,
        ) {
            Ok(outcome) => {
                preview.advance_cd_transfer();
                *self = preview;
                Ok(PendingPageUpdate::Resolved(outcome))
            }
            Err(PagingError::NoFreePhysicalSlot(_) | PagingError::NoFreeTextureSlot(_)) => {
                Ok(PendingPageUpdate::Stalled)
            }
            Err(error) => Err(error),
        }
    }

    fn lowest_queued_page(&self) -> Option<PageIndex> {
        self.pages
            .values()
            .filter(|record| record.state == PageState::Queued)
            .map(|record| record.index)
            .min()
    }

    fn start_cd_transfer(
        &mut self,
        first: PageIndex,
    ) -> Result<Option<Vec<PageIndex>>, PagingError> {
        let mut pages = Vec::new();
        let mut index = first.get();
        loop {
            let page = PageIndex::new(index);
            if self.pages.get(&page).map(|record| record.state) != Some(PageState::Queued) {
                break;
            }
            pages.push(page);
            let Some(next) = index.checked_add(1) else {
                break;
            };
            index = next;
        }
        debug_assert!(!pages.is_empty());
        let mut preview = self.clone();
        let (reservations, invalidated) = preview.reserve_cd_physical_group(&pages)?;
        if reservations.is_empty() {
            return Ok(None);
        }
        pages.truncate(reservations.len());
        let frames_remaining = RETAIL_CD_SEEK_SETUP_FRAMES + self.cd_page_transfer_frames(first);
        preview.cd_transfer = Some(RetailCdTransfer {
            pages: pages
                .into_iter()
                .zip(reservations)
                .map(|(page, reservation)| RetailCdPage {
                    page,
                    reservation: Some(reservation),
                    cloned: true,
                })
                .collect(),
            next: 0,
            frames_remaining,
        });
        *self = preview;
        Ok(Some(invalidated))
    }

    /// Reserves the longest contiguous replaceable/free physical run, exactly
    /// like `NSPageAllocate` shortening its requested count. Equal-length and
    /// equal-age candidates choose the later slot encountered by the scan.
    fn reserve_cd_physical_group(
        &mut self,
        pages: &[PageIndex],
    ) -> Result<(Vec<RetailCdReservation>, Vec<PageIndex>), PagingError> {
        let Some((start, count)) = self.longest_replaceable_physical_run(pages.len()) else {
            return Ok((Vec::new(), Vec::new()));
        };
        let mut reservations = Vec::with_capacity(count);
        let mut invalidated = Vec::with_capacity(count);
        for (offset, page) in pages.iter().copied().take(count).enumerate() {
            let slot = start + offset;
            let evicted = self.physical_slots[slot];
            if let Some(evicted) = evicted {
                let record = self
                    .pages
                    .get_mut(&evicted)
                    .ok_or(PagingError::UnknownPage(evicted))?;
                record.state = PageState::Raw;
                record.physical_slot = None;
                record.ordinary_kind = None;
                invalidated.push(evicted);
            }
            self.physical_slots[slot] = Some(page);
            reservations.push(RetailCdReservation {
                slot: u8::try_from(slot).expect("twenty-two physical slots fit u8"),
            });
        }
        debug_assert!(invalidated.len() <= PHYSICAL_SLOT_COUNT);
        Ok((reservations, invalidated))
    }

    fn longest_replaceable_physical_run(&self, requested: usize) -> Option<(usize, usize)> {
        let mut best = None;
        for start in 0..self.physical_slot_count() {
            let mut age = 0_u64;
            for len in 1..=requested.min(self.physical_slot_count() - start) {
                let slot = start + len - 1;
                let slot_age = match self.physical_slots[slot] {
                    None => 0,
                    Some(page) => {
                        let record = self.pages.get(&page)?;
                        if record.references != 0 {
                            break;
                        }
                        // Rust stores an install timestamp; native's candidate
                        // score is age. Convert it so larger still means older,
                        // preserving the existing allocator's LRU direction.
                        self.physical_clock
                            .saturating_sub(record.physical_timestamp)
                            .saturating_add(1)
                    }
                };
                age = age.saturating_add(slot_age);
                let candidate = (len, age, start);
                if best.is_none_or(|current| candidate > current) {
                    best = Some(candidate);
                }
            }
        }
        best.map(|(len, _, start)| (start, len))
    }

    fn take_cd_page_reservation(&mut self, page: PageIndex) -> Option<RetailCdReservation> {
        let member = self
            .cd_transfer
            .as_mut()?
            .pages
            .iter_mut()
            .find(|member| member.page == page && member.cloned)?;
        member.cloned = false;
        member.reservation.take()
    }

    fn cancel_cd_page_clone(&mut self, page: PageIndex) {
        let Some(reservation) = self.take_cd_page_reservation(page) else {
            return;
        };
        let slot = usize::from(reservation.slot);
        if self.physical_slots[slot] == Some(page) {
            self.physical_slots[slot] = None;
        }
    }

    fn advance_cd_transfer(&mut self) {
        let next_page = self
            .cd_transfer
            .as_ref()
            .and_then(|transfer| transfer.pages.get(transfer.next.saturating_add(1)))
            .map(|member| member.page);
        let Some(next_page) = next_page else {
            self.cd_transfer = None;
            return;
        };
        let frames_remaining = self.cd_page_transfer_frames(next_page);
        let transfer = self
            .cd_transfer
            .as_mut()
            .expect("the next transfer page belongs to an active group");
        transfer.next += 1;
        transfer.frames_remaining = frames_remaining;
    }

    fn cd_page_transfer_frames(&self, page: PageIndex) -> u16 {
        let sectors = self
            .cd_page_sectors
            .as_ref()
            .and_then(|counts| {
                usize::try_from(page.get())
                    .ok()
                    .and_then(|index| counts.get(index))
            })
            .copied()
            .map_or(NSF_PAGE_SECTOR_COUNT, NsfPageSectorCount::get);
        (u16::from(sectors) / RETAIL_CD_SECTORS_PER_FRAME).max(1)
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
            match self.update_pending_virtual_page_step()? {
                PendingPageUpdate::Resolved(outcome) => outcomes.push(outcome),
                PendingPageUpdate::Waiting | PendingPageUpdate::Invalidated(_) => {}
                PendingPageUpdate::Idle | PendingPageUpdate::Stalled => {
                    return Err(PagingError::PendingUpdateStalled(next));
                }
            }
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
        let outcome = if let Some(reservation) = preview.take_cd_page_reservation(page) {
            preview.open_reserved_cd_page_in_place(page, reservation, increment_reference, kind)?
        } else {
            preview.open_page_with_outcome_in_place(page, increment_reference, kind)?
        };
        *self = preview;
        Ok(outcome)
    }

    fn open_reserved_cd_page_in_place(
        &mut self,
        page: PageIndex,
        reservation: RetailCdReservation,
        increment_reference: bool,
        kind: OrdinaryPageKind,
    ) -> Result<PagerOpenOutcome, PagingError> {
        let slot = usize::from(reservation.slot);
        debug_assert_eq!(self.physical_slots[slot], Some(page));
        let is_texture = self.texture_page_eids.contains_key(&page);
        let is_audio = self.audio_pages.contains(&page);
        let evicted = if is_texture {
            self.physical_slots[slot] = None;
            self.materialize_texture_page(page)?
                .replaced
                .filter(|binding| binding.state == TextureSlotState::Resident)
        } else if is_audio {
            self.physical_slots[slot] = None;
            self.pages
                .get_mut(&page)
                .ok_or(PagingError::UnknownPage(page))?
                .state = PageState::Translated;
            None
        } else {
            self.physical_clock = self.physical_clock.wrapping_add(1);
            let record = self
                .pages
                .get_mut(&page)
                .ok_or(PagingError::UnknownPage(page))?;
            record.state = PageState::Translated;
            record.physical_slot = Some(reservation.slot);
            record.physical_timestamp = self.physical_clock;
            record.ordinary_kind = Some(kind);
            None
        };
        let record = self
            .pages
            .get_mut(&page)
            .ok_or(PagingError::UnknownPage(page))?;
        if increment_reference {
            record.references = record.references.saturating_add(1);
        }
        Ok(PagerOpenOutcome {
            page,
            resolved: true,
            invalidated: PageInvalidations::new(None, evicted.map(|binding| binding.page)),
            evicted,
        })
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
        let canceled = {
            let record = self
                .pages
                .get_mut(&page)
                .ok_or(PagingError::UnknownPage(page))?;
            record.references = record
                .references
                .checked_sub(1)
                .ok_or(PagingError::ReferenceUnderflow(page))?;
            let canceled = record.references == 0 && record.state == PageState::Queued;
            if canceled {
                record.state = PageState::Raw;
            }
            canceled
        };
        if canceled {
            self.cancel_cd_page_clone(page);
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
            let (decremented, canceled) = {
                let record = self
                    .pages
                    .get_mut(&page)
                    .ok_or(PagingError::UnknownPage(page))?;
                if record.references == 0 {
                    (false, false)
                } else {
                    record.references -= 1;
                    let canceled = record.references == 0;
                    if canceled {
                        // Native deletes one zero-reference type-zero clone
                        // immediately while its read siblings continue.
                        record.state = PageState::Raw;
                    }
                    (true, canceled)
                }
            };
            if canceled {
                self.cancel_cd_page_clone(page);
            }
            return Ok(PagerCloseOutcome {
                page,
                decremented,
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

    /// Captures native's global texture-page structs without retaining this
    /// stream's page-table handles.
    pub fn texture_slot_carry_snapshot(&self) -> Result<TextureSlotCarrySnapshot, PagingError> {
        let snapshot = TextureSlotCarrySnapshot {
            slots: std::array::from_fn(|slot| {
                let state = self.texture_slot_states[slot];
                TextureSlotCarryRecord {
                    eid: matches!(state, TextureSlotState::Resident | TextureSlotState::Stale)
                        .then_some(self.texture_slot_eids[slot])
                        .flatten(),
                    generation: self.texture_generations[slot],
                    state,
                }
            }),
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Recreates native `NSInitTexturePages` for a newly parsed stream.
    ///
    /// Resident and stale source EIDs which name a destination TPAG retain
    /// their exact physical slot and become resident against the destination
    /// page handle. Missing EIDs remain stale. Native state 1 (`Free`) and
    /// state 30 (`Reserved`) both initialize as free. Per-slot generations are
    /// retained so a later overwrite cannot alias the previous renderer cache
    /// generation even when the two streams reuse the same page index.
    ///
    /// Import is transactional and accepts only a fresh destination pager;
    /// callers must perform it before any destination texture open.
    pub fn import_texture_slot_carry(
        &mut self,
        carry: TextureSlotCarrySnapshot,
    ) -> Result<(), PagingError> {
        carry.validate()?;
        if let Some(slot) = (0..TEXTURE_SLOT_COUNT).find(|slot| {
            self.texture_slots[*slot].is_some()
                || self.texture_slot_eids[*slot].is_some()
                || self.texture_slot_states[*slot] != TextureSlotState::Free
                || self.texture_generations[*slot] != 0
        }) {
            return Err(PagingError::TextureCarryDestinationNotFresh(slot));
        }

        let mut destination_pages = [None; TEXTURE_SLOT_COUNT];
        for (slot, record) in carry.slots.iter().copied().enumerate() {
            let Some(eid) = record.eid else {
                continue;
            };
            if let Some(page) = self.page_eids.get(&eid).copied() {
                destination_pages[slot] = Some(page);
            } else if self.eids.contains_key(&eid) {
                return Err(PagingError::TextureCarryEidIsNotTexture(eid));
            }
        }

        let mut preview = self.clone();
        for (slot, record) in carry.slots.iter().copied().enumerate() {
            preview.texture_generations[slot] = record.generation;
            match record.state {
                TextureSlotState::Free | TextureSlotState::Reserved => {
                    preview.texture_slots[slot] = None;
                    preview.texture_slot_eids[slot] = None;
                    preview.texture_slot_states[slot] = TextureSlotState::Free;
                }
                TextureSlotState::Resident | TextureSlotState::Stale => {
                    let eid = record
                        .eid
                        .ok_or(PagingError::MissingTextureCarryEid(slot))?;
                    preview.texture_slot_eids[slot] = Some(eid);
                    if let Some(page) = destination_pages[slot] {
                        preview.texture_slots[slot] = Some(page);
                        preview.texture_slot_states[slot] = TextureSlotState::Resident;
                        let page_record = preview
                            .pages
                            .get_mut(&page)
                            .ok_or(PagingError::UnknownPage(page))?;
                        page_record.state = PageState::Resident;
                        page_record.generation = record.generation;
                    } else {
                        preview.texture_slots[slot] = None;
                        preview.texture_slot_states[slot] = TextureSlotState::Stale;
                    }
                }
            }
        }
        *self = preview;
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
        if self.texture_slot_states[slot] == TextureSlotState::Reserved {
            return Err(PagingError::ReservedTextureSlot(slot));
        }
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
        self.texture_slot_eids[slot] = Some(eid);
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

        if let Some(slot) =
            self.texture_slot_eids
                .iter()
                .enumerate()
                .position(|(slot, candidate)| {
                    *candidate == Some(eid)
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
                self.texture_slot_states[*slot] == TextureSlotState::Resident
                    && self
                        .texture_slot_binding(*slot)
                        .is_some_and(|binding| !protected.contains(&binding.eid))
            }),
        }
        .filter(|slot| self.texture_slot_states[*slot] != TextureSlotState::Reserved);
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
        if *state == TextureSlotState::Reserved {
            return Err(PagingError::ReservedTextureSlot(slot));
        }
        *state = TextureSlotState::Stale;
        if let Some(page) = self.texture_slots[slot]
            && let Some(record) = self.pages.get_mut(&page)
        {
            record.state = PageState::Stale;
        }
        Ok(())
    }

    /// Applies native `NSTexturePageFree` without erasing diagnostic identity.
    ///
    /// State 20 (`Resident`) rearms the source page; state 21 (`Stale`) only
    /// becomes free. Native leaves state 1 (`Free`) and state 30 (`Reserved`)
    /// untouched. In particular, that no-op prevents an old reserved binding
    /// from rearming a page which has since become resident in another slot.
    pub fn free_texture_slot(&mut self, slot: usize) -> Result<(), PagingError> {
        let state = self
            .texture_slot_states
            .get_mut(slot)
            .ok_or(PagingError::InvalidTextureSlot(slot))?;
        match *state {
            TextureSlotState::Resident => {
                *state = TextureSlotState::Free;
                if let Some(page) = self.texture_slots[slot]
                    && let Some(record) = self.pages.get_mut(&page)
                {
                    record.state = PageState::Translated;
                }
            }
            TextureSlotState::Stale => *state = TextureSlotState::Free,
            TextureSlotState::Free | TextureSlotState::Reserved => {}
        }
        Ok(())
    }

    /// Frees a copied texture identity, then marks its VRAM slot as occupied
    /// by non-page data.
    ///
    /// This is the pointer-free equivalent of `NSTexturePageFree(n)` followed
    /// by assigning native texture-page state 30. It intentionally preserves
    /// the old EID and generation for an already captured renderer frame.
    pub fn reserve_texture_slot(&mut self, slot: usize) -> Result<(), PagingError> {
        self.free_texture_slot(slot)?;
        self.texture_slot_states[slot] = TextureSlotState::Reserved;
        Ok(())
    }

    /// Changes native state 30 back to state 1 without disturbing any other
    /// slot state. Returns whether a reservation was released.
    pub fn release_reserved_texture_slot(&mut self, slot: usize) -> Result<bool, PagingError> {
        let state = self
            .texture_slot_states
            .get_mut(slot)
            .ok_or(PagingError::InvalidTextureSlot(slot))?;
        if *state != TextureSlotState::Reserved {
            return Ok(false);
        }
        *state = TextureSlotState::Free;
        Ok(true)
    }

    /// Applies the exact PSX `TitleLoadEntries` CLUT reservations.
    ///
    /// Native texture pages 8 through 14 map to Rust slots 0 through 6.
    /// Page 15 (slot 7) is deliberately untouched. Slots 9, 10, and 13 are
    /// always reserved; slots 8, 12, and 14 depend on the MDAT IPAL/CLUT
    /// count using the source's strict threshold comparisons.
    pub fn reserve_title_clut_texture_slots(&mut self, ipal_count: u32) -> Result<(), PagingError> {
        for slot in [1, 2, 5] {
            self.reserve_texture_slot(slot)?;
        }
        for (slot, needed) in [
            (0, ipal_count > 160),
            (4, ipal_count > 288),
            (6, ipal_count >= 417),
        ] {
            if needed {
                self.reserve_texture_slot(slot)?;
            } else {
                self.release_reserved_texture_slot(slot)?;
            }
        }
        Ok(())
    }

    /// Releases state-30 title reservations from native pages 8 through 14.
    /// Native page 15 / Rust slot 7 remains untouched.
    pub fn release_title_clut_texture_slots(&mut self) {
        for slot in 0..7 {
            // The range is statically inside the eight-slot table.
            let _ = self.release_reserved_texture_slot(slot);
        }
    }

    #[must_use]
    pub fn texture_slot_state(&self, slot: usize) -> Option<TextureSlotState> {
        self.texture_slot_states.get(slot).copied()
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
            // Native free/stale slots retain their old VRAM bytes until a
            // replacement and therefore remain valid cache identities. State
            // 30 is different: title CLUTs overwrite that VRAM, so its old
            // TPAG identity must not reach the renderer.
            slots: std::array::from_fn(|slot| {
                self.texture_slot_binding(slot)
                    .filter(|binding| binding.state != TextureSlotState::Reserved)
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crust_formats::stream::{NSF_PAGE_SIZE, parse_nsd, parse_nsf};

    const TEST_MODERN_NSD_HEADER_SIZE: usize = 0x520;
    const TEST_LDAT_PREFIX_SIZE: usize = 0x118;

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_test_entry_page(bytes: &mut [u8], page: PageIndex, eid: Eid) {
        let start = page.get() as usize * NSF_PAGE_SIZE;
        put_u16(bytes, start, 0x1234);
        put_u16(bytes, start + 2, 0);
        put_u32(bytes, start + 4, page.tagged());
        put_u32(bytes, start + 8, 1);
        put_u32(bytes, start + 16, 24);
        put_u32(bytes, start + 20, 44);
        put_u32(bytes, start + 24, crust_formats::stream::ENTRY_MAGIC);
        put_u32(bytes, start + 28, eid.raw());
        put_u32(bytes, start + 32, 2);
        put_u32(bytes, start + 36, 0);
        put_u32(bytes, start + 40, 20);
    }

    fn write_test_empty_page(bytes: &mut [u8], page: PageIndex) {
        let start = page.get() as usize * NSF_PAGE_SIZE;
        put_u16(bytes, start, 0x1234);
        put_u16(bytes, start + 2, 0);
        put_u32(bytes, start + 4, page.tagged());
        put_u32(bytes, start + 8, 0);
        put_u32(bytes, start + 16, 20);
    }

    fn initial_mount_test_stream() -> (Nsd, Nsf, Eid, Eid) {
        let level = LevelId::INTRO;
        let spawn = Eid::from_name("spawn").unwrap();
        let load_entry = Eid::from_name("loadE").unwrap();
        let page_count = 3_u32;
        let table_len = 1_usize;
        let ldat_offset = TEST_MODERN_NSD_HEADER_SIZE + table_len * 8;
        let mut nsd_bytes = vec![0_u8; ldat_offset + TEST_LDAT_PREFIX_SIZE];
        put_u32(&mut nsd_bytes, 0x400, page_count);
        put_u32(&mut nsd_bytes, 0x404, table_len as u32);
        put_u32(
            &mut nsd_bytes,
            TEST_MODERN_NSD_HEADER_SIZE,
            PageIndex::new(0).tagged(),
        );
        put_u32(&mut nsd_bytes, TEST_MODERN_NSD_HEADER_SIZE + 4, spawn.raw());
        put_u32(&mut nsd_bytes, ldat_offset, 1);
        put_u32(&mut nsd_bytes, ldat_offset + 4, level.get());
        put_u32(&mut nsd_bytes, ldat_offset + 8, spawn.raw());
        let metadata = parse_nsd(&nsd_bytes, level).unwrap();

        let mut nsf_bytes = vec![0_u8; page_count as usize * NSF_PAGE_SIZE];
        write_test_entry_page(&mut nsf_bytes, PageIndex::new(0), spawn);
        write_test_entry_page(&mut nsf_bytes, PageIndex::new(1), load_entry);
        write_test_empty_page(&mut nsf_bytes, PageIndex::new(2));
        let nsf = parse_nsf(&nsf_bytes, &metadata).unwrap();
        (metadata, nsf, spawn, load_entry)
    }

    fn initial_mount_visibility_test_stream() -> (Nsd, Nsf, Eid, Eid, Eid) {
        let level = LevelId::INTRO;
        let spawn = Eid::from_name("spawn").unwrap();
        let visibility = Eid::from_name("visib").unwrap();
        let load_entry = Eid::from_name("loadE").unwrap();
        let page_count = 3_u32;
        let table_len = 1_usize;
        let ldat_offset = TEST_MODERN_NSD_HEADER_SIZE + table_len * 8;
        let mut nsd_bytes = vec![0_u8; ldat_offset + TEST_LDAT_PREFIX_SIZE];
        put_u32(&mut nsd_bytes, 0x400, page_count);
        put_u32(&mut nsd_bytes, 0x404, table_len as u32);
        put_u32(
            &mut nsd_bytes,
            TEST_MODERN_NSD_HEADER_SIZE,
            PageIndex::new(0).tagged(),
        );
        put_u32(&mut nsd_bytes, TEST_MODERN_NSD_HEADER_SIZE + 4, spawn.raw());
        put_u32(&mut nsd_bytes, ldat_offset, 1);
        put_u32(&mut nsd_bytes, ldat_offset + 4, level.get());
        put_u32(&mut nsd_bytes, ldat_offset + 8, spawn.raw());
        let metadata = parse_nsd(&nsd_bytes, level).unwrap();

        let mut nsf_bytes = vec![0_u8; page_count as usize * NSF_PAGE_SIZE];
        write_test_entry_page(&mut nsf_bytes, PageIndex::new(0), spawn);
        write_test_entry_page(&mut nsf_bytes, PageIndex::new(1), visibility);
        write_test_entry_page(&mut nsf_bytes, PageIndex::new(2), load_entry);
        let nsf = parse_nsf(&nsf_bytes, &metadata).unwrap();
        (metadata, nsf, spawn, visibility, load_entry)
    }

    fn initial_title_core_preload_test_stream() -> (Nsd, Nsf, Eid) {
        let level = LevelId::TITLE;
        let spawn = Eid::from_name("spawn").unwrap();
        let exec_4 = Eid::from_name("exec4").unwrap();
        let exec_52 = Eid::from_name("ex052").unwrap();
        let page_count = 3_u32;
        let table_len = 1_usize;
        let ldat_offset = TEST_MODERN_NSD_HEADER_SIZE + table_len * 8;
        let mut nsd_bytes = vec![0_u8; ldat_offset + TEST_LDAT_PREFIX_SIZE];
        put_u32(&mut nsd_bytes, 0x400, page_count);
        put_u32(&mut nsd_bytes, 0x404, table_len as u32);
        put_u32(
            &mut nsd_bytes,
            TEST_MODERN_NSD_HEADER_SIZE,
            PageIndex::new(0).tagged(),
        );
        put_u32(&mut nsd_bytes, TEST_MODERN_NSD_HEADER_SIZE + 4, spawn.raw());
        put_u32(&mut nsd_bytes, ldat_offset, 1);
        put_u32(&mut nsd_bytes, ldat_offset + 4, level.get());
        put_u32(&mut nsd_bytes, ldat_offset + 8, spawn.raw());
        put_u32(&mut nsd_bytes, ldat_offset + 20 + 4 * 4, exec_4.raw());
        put_u32(&mut nsd_bytes, ldat_offset + 20 + 4 * 52, exec_52.raw());
        let metadata = parse_nsd(&nsd_bytes, level).unwrap();

        let mut nsf_bytes = vec![0_u8; page_count as usize * NSF_PAGE_SIZE];
        write_test_entry_page(&mut nsf_bytes, PageIndex::new(0), spawn);
        write_test_entry_page(&mut nsf_bytes, PageIndex::new(1), exec_4);
        write_test_entry_page(&mut nsf_bytes, PageIndex::new(2), exec_52);
        let nsf = parse_nsf(&nsf_bytes, &metadata).unwrap();
        (metadata, nsf, spawn)
    }

    #[test]
    fn retail_mount_default_drains_initial_level_update_pages() {
        let (metadata, nsf, spawn, load_entry) = initial_mount_test_stream();
        let pager = Pager::mount_retail_level(
            &metadata,
            &nsf,
            LevelId::INTRO,
            spawn,
            [load_entry],
            [PageIndex::new(2)],
        )
        .unwrap();

        assert_eq!(pager.pending_virtual_pages().collect::<Vec<_>>(), []);
        assert_eq!(
            pager.resolved_pages().collect::<Vec<_>>(),
            [PageIndex::new(0), PageIndex::new(1), PageIndex::new(2)]
        );
        assert_eq!(pager.page(PageIndex::new(0)).unwrap().references, 0);
        assert_eq!(pager.page(PageIndex::new(1)).unwrap().references, 1);
        assert_eq!(pager.page(PageIndex::new(2)).unwrap().references, 1);
    }

    #[test]
    fn retail_mount_can_defer_exact_initial_level_update_pages() {
        let (metadata, nsf, spawn, load_entry) = initial_mount_test_stream();
        let pager = Pager::mount_retail_level_with_options(
            &metadata,
            &nsf,
            LevelId::INTRO,
            spawn,
            [load_entry],
            [PageIndex::new(2)],
            RetailLevelMountOptions::new(LevelId::INTRO)
                .with_page_update(RetailLevelMountPageUpdate::Defer),
        )
        .unwrap();

        assert_eq!(
            pager.pending_virtual_pages().collect::<Vec<_>>(),
            [PageIndex::new(1), PageIndex::new(2)]
        );
        assert_eq!(
            pager.resolved_pages().collect::<Vec<_>>(),
            [PageIndex::new(0)]
        );
        assert_eq!(pager.page(PageIndex::new(0)).unwrap().references, 0);
        assert_eq!(pager.page(PageIndex::new(1)).unwrap().references, 1);
        assert_eq!(pager.page(PageIndex::new(2)).unwrap().references, 1);
    }

    #[test]
    fn retail_mount_drains_after_temporary_initial_slst_close() {
        let (metadata, nsf, spawn, visibility, load_entry) = initial_mount_visibility_test_stream();
        let pager = Pager::mount_retail_level_with_options(
            &metadata,
            &nsf,
            LevelId::INTRO,
            spawn,
            [load_entry],
            [],
            RetailLevelMountOptions::new(LevelId::INTRO)
                .with_physical_slot_count(2)
                .with_initial_visibility_list(Some(visibility)),
        )
        .unwrap();

        assert_eq!(pager.pending_virtual_pages().collect::<Vec<_>>(), []);
        assert_eq!(
            pager.resolved_pages().collect::<Vec<_>>(),
            [PageIndex::new(0), PageIndex::new(2)],
            "the queued load-list page replaces the already-closed SLST"
        );
        assert_eq!(pager.page(PageIndex::new(0)).unwrap().references, 0);
        assert_eq!(
            pager.page(PageIndex::new(1)).unwrap().state,
            PageState::Raw,
            "the closed SLST PTE is re-armed after its slot is reused"
        );
        assert_eq!(pager.page(PageIndex::new(1)).unwrap().references, 0);
        assert_eq!(pager.page(PageIndex::new(2)).unwrap().references, 1);
    }

    #[test]
    fn retail_mount_defer_still_materializes_and_closes_initial_slst_first() {
        let (metadata, nsf, spawn, visibility, load_entry) = initial_mount_visibility_test_stream();
        let pager = Pager::mount_retail_level_with_options(
            &metadata,
            &nsf,
            LevelId::INTRO,
            spawn,
            [load_entry],
            [],
            RetailLevelMountOptions::new(LevelId::INTRO)
                .with_physical_slot_count(2)
                .with_page_update(RetailLevelMountPageUpdate::Defer)
                .with_initial_visibility_list(Some(visibility)),
        )
        .unwrap();

        assert_eq!(
            pager.pending_virtual_pages().collect::<Vec<_>>(),
            [PageIndex::new(2)]
        );
        assert_eq!(
            pager.resolved_pages().collect::<Vec<_>>(),
            [PageIndex::new(0), PageIndex::new(1)]
        );
        assert_eq!(pager.page(PageIndex::new(0)).unwrap().references, 0);
        assert_eq!(pager.page(PageIndex::new(1)).unwrap().references, 0);
        assert_eq!(pager.page(PageIndex::new(2)).unwrap().references, 1);
    }

    #[test]
    fn title_mount_can_defer_core_preloads_then_reproduce_the_default_state() {
        let (metadata, nsf, spawn) = initial_title_core_preload_test_stream();
        let default =
            Pager::mount_retail_level(&metadata, &nsf, LevelId::TITLE, spawn, [], []).unwrap();
        let mut deferred = Pager::mount_retail_level_with_options(
            &metadata,
            &nsf,
            LevelId::TITLE,
            spawn,
            [],
            [],
            RetailLevelMountOptions::new(LevelId::TITLE).with_core_page_preloads(false),
        )
        .unwrap();

        assert_eq!(deferred.pending_virtual_pages().collect::<Vec<_>>(), []);
        assert_eq!(deferred.page(PageIndex::new(1)).unwrap().references, 0);
        assert_eq!(deferred.page(PageIndex::new(2)).unwrap().references, 0);

        let outcomes = deferred
            .stage_retail_core_page_preloads(&metadata, LevelId::TITLE)
            .unwrap();

        assert_eq!(
            outcomes,
            [
                RetailCorePagePreloadOutcome::Open(PagerOpenOutcome {
                    page: PageIndex::new(1),
                    resolved: false,
                    invalidated: PageInvalidations::NONE,
                    evicted: None,
                }),
                RetailCorePagePreloadOutcome::Open(PagerOpenOutcome {
                    page: PageIndex::new(2),
                    resolved: false,
                    invalidated: PageInvalidations::NONE,
                    evicted: None,
                }),
            ]
        );
        assert_eq!(deferred, default);
    }

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

    fn register_named_texture(pager: &mut Pager, index: u32, eid: Eid) {
        let page = PageIndex::new(index);
        pager.register_page(page, []).unwrap();
        pager.bind_page_eid(eid, page).unwrap();
    }

    fn enable_cd_transfer(
        pager: &mut Pager,
        page_count: usize,
        overrides: impl IntoIterator<Item = (u32, u8)>,
    ) {
        let full = NsfPageSectorCount::new(NSF_PAGE_SECTOR_COUNT).unwrap();
        let mut counts = vec![full; page_count];
        for (page, sectors) in overrides {
            counts[page as usize] = NsfPageSectorCount::new(sectors).unwrap();
        }
        pager.cd_page_sectors = Some(counts);
    }

    fn expect_resolution(update: PagerUpdateOutcome) -> PagerOpenOutcome {
        match update {
            PagerUpdateOutcome::Resolved(outcome) => outcome,
            PagerUpdateOutcome::Invalidated(pages) => {
                panic!("expected page resolution, got reservation invalidations {pages:?}")
            }
        }
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
    fn texture_carry_remaps_a_resident_eid_to_the_destination_page() {
        let eid = Eid::from_name("sameT").unwrap();
        let mut source = Pager::new();
        register_named_texture(&mut source, 2, eid);
        assert_eq!(source.materialize_texture(3, PageIndex::new(2)), Ok(1));
        let carry = source.texture_slot_carry_snapshot().unwrap();

        let mut destination = Pager::new();
        register_named_texture(&mut destination, 17, eid);
        destination.import_texture_slot_carry(carry).unwrap();

        assert_eq!(destination.texture_slot(3), Some((PageIndex::new(17), 1)));
        assert_eq!(
            destination.texture_slot_binding(3),
            Some(TextureSlotBinding {
                page: PageIndex::new(17),
                eid,
                generation: 1,
                state: TextureSlotState::Resident,
            })
        );
        assert_eq!(
            destination.page(PageIndex::new(17)).unwrap().state,
            PageState::Resident
        );
        assert_eq!(destination.page(PageIndex::new(17)).unwrap().generation, 1);
    }

    #[test]
    fn texture_carry_stales_missing_eids_and_resets_free_or_reserved_slots() {
        let missing = Eid::from_name("goneT").unwrap();
        let freed = Eid::from_name("freeT").unwrap();
        let reserved = Eid::from_name("clutT").unwrap();
        let mut source = Pager::new();
        for (page, eid) in [(0, missing), (1, freed), (2, reserved)] {
            register_named_texture(&mut source, page, eid);
        }
        source.materialize_texture(7, PageIndex::new(0)).unwrap();
        source.materialize_texture(6, PageIndex::new(1)).unwrap();
        source.materialize_texture(5, PageIndex::new(2)).unwrap();
        source.free_texture_slot(6).unwrap();
        source.reserve_texture_slot(5).unwrap();
        let carry = source.texture_slot_carry_snapshot().unwrap();
        assert_eq!(carry.state(7), Some(TextureSlotState::Resident));
        assert_eq!(carry.state(6), Some(TextureSlotState::Free));
        assert_eq!(carry.state(5), Some(TextureSlotState::Reserved));

        let mut destination = Pager::new();
        // Even if these ignored state-1/state-30 EIDs happen to exist in the
        // new page table, native does not remap them.
        register_named_texture(&mut destination, 9, freed);
        register_named_texture(&mut destination, 10, reserved);
        destination.import_texture_slot_carry(carry).unwrap();

        assert_eq!(
            destination.texture_slot_state(7),
            Some(TextureSlotState::Stale)
        );
        assert_eq!(destination.texture_slot(7), None);
        assert_eq!(
            destination
                .texture_slot_carry_snapshot()
                .unwrap()
                .binding(7),
            Some(TextureSlotCarryBinding {
                eid: missing,
                generation: 1,
                state: TextureSlotState::Stale,
            })
        );
        for slot in [5, 6] {
            assert_eq!(
                destination.texture_slot_state(slot),
                Some(TextureSlotState::Free)
            );
            assert_eq!(destination.texture_slot(slot), None);
            assert_eq!(destination.texture_generations[slot], 1);
        }
    }

    #[test]
    fn texture_carry_rejects_duplicate_and_invalid_eids() {
        let duplicate = Eid::from_name("dupeT").unwrap();
        let binding = |eid, state| {
            Some(TextureSlotCarryBinding {
                eid,
                generation: 7,
                state,
            })
        };
        let mut slots = [None; TEXTURE_SLOT_COUNT];
        slots[7] = binding(duplicate, TextureSlotState::Resident);
        slots[6] = binding(duplicate, TextureSlotState::Stale);
        assert_eq!(
            TextureSlotCarrySnapshot::try_from_bindings(slots),
            Err(PagingError::DuplicateTextureCarryEid(duplicate))
        );

        let invalid = Eid::from_raw(2);
        let mut slots = [None; TEXTURE_SLOT_COUNT];
        slots[4] = binding(invalid, TextureSlotState::Resident);
        assert_eq!(
            TextureSlotCarrySnapshot::try_from_bindings(slots),
            Err(PagingError::InvalidTextureCarryEid {
                slot: 4,
                eid: invalid,
            })
        );

        let mut slots = [None; TEXTURE_SLOT_COUNT];
        slots[2] = binding(duplicate, TextureSlotState::Free);
        assert_eq!(
            TextureSlotCarrySnapshot::try_from_bindings(slots),
            Err(PagingError::InvalidTextureCarryState(2))
        );
    }

    #[test]
    fn texture_carry_rejects_an_ordinary_destination_eid_transactionally() {
        let eid = Eid::from_name("kindT").unwrap();
        let carry = TextureSlotCarrySnapshot::try_from_bindings(std::array::from_fn(|slot| {
            (slot == 7).then_some(TextureSlotCarryBinding {
                eid,
                generation: 3,
                state: TextureSlotState::Resident,
            })
        }))
        .unwrap();
        let mut destination = Pager::new();
        let ordinary = entry(4, 0);
        destination
            .register_page(PageIndex::new(4), [ordinary])
            .unwrap();
        destination.bind_eid(eid, ordinary).unwrap();
        let before = destination.clone();

        assert_eq!(
            destination.import_texture_slot_carry(carry),
            Err(PagingError::TextureCarryEidIsNotTexture(eid))
        );
        assert_eq!(destination, before);
    }

    #[test]
    fn texture_carry_preserves_high_to_low_stale_allocation_order() {
        let source_eids = (0..8).map(texture_eid).collect::<Vec<_>>();
        let mut source = Pager::new();
        for (page, eid) in source_eids.iter().copied().enumerate() {
            register_named_texture(&mut source, page as u32, eid);
            source.materialize_texture_eid(eid).unwrap();
        }
        let carry = source.texture_slot_carry_snapshot().unwrap();

        let mut destination = Pager::new();
        for (offset, eid) in source_eids.iter().copied().enumerate().skip(2) {
            register_named_texture(&mut destination, 20 + offset as u32, eid);
        }
        let ninth = texture_eid(8);
        let tenth = texture_eid(9);
        register_named_texture(&mut destination, 28, ninth);
        register_named_texture(&mut destination, 29, tenth);
        destination.import_texture_slot_carry(carry).unwrap();
        assert_eq!(
            destination.texture_slot_state(7),
            Some(TextureSlotState::Stale)
        );
        assert_eq!(
            destination.texture_slot_state(6),
            Some(TextureSlotState::Stale)
        );
        assert_eq!(
            destination.texture_slot_state(5),
            Some(TextureSlotState::Resident)
        );
        destination.set_current_texture_load_eids(
            source_eids.iter().skip(2).copied().chain([ninth, tenth]),
        );

        let first = destination.materialize_texture_eid(ninth).unwrap();
        let second = destination.materialize_texture_eid(tenth).unwrap();

        assert_eq!(first.slot, 7);
        assert_eq!(second.slot, 6);
        assert_eq!(first.binding.generation, 2);
        assert_eq!(second.binding.generation, 2);
        assert_eq!(
            first.replaced, None,
            "missing stale EID has no destination page handle"
        );
        assert_eq!(
            second.replaced, None,
            "missing stale EID has no destination page handle"
        );
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
            let promoted = expect_resolution(
                pager
                    .update_pending_virtual_page()
                    .unwrap()
                    .expect("the first eight protected textures have slots"),
            );
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
        let captured = pager.texture_frame_snapshot();

        pager.free_texture_slot(assignment.slot).unwrap();

        let binding = pager.texture_slot_binding(assignment.slot).unwrap();
        assert_eq!(binding.eid, eid);
        assert_eq!(binding.state, TextureSlotState::Free);
        assert_eq!(binding.generation, assignment.binding.generation);
        assert_eq!(captured.slot(assignment.slot), Some(assignment.binding));
        assert_eq!(
            pager.texture_frame_snapshot().slot(assignment.slot),
            Some(binding)
        );
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
    fn reserved_texture_slot_keeps_identity_but_cannot_be_reallocated() {
        let mut pager = Pager::new();
        let eids = (0..=8)
            .map(|index| register_texture(&mut pager, index))
            .collect::<Vec<_>>();
        pager.set_current_texture_load_eids(eids.iter().take(8).copied());
        for eid in eids.iter().take(8).copied() {
            pager.materialize_texture_eid(eid).unwrap();
        }
        // The first texture owns slot 7 and the second owns slot 6.
        let retained = pager.texture_slot_binding(6).unwrap();
        pager.reserve_texture_slot(6).unwrap();
        pager.set_current_texture_load_eids(
            eids.iter()
                .take(8)
                .copied()
                .filter(|eid| *eid != eids[1] && *eid != eids[2]),
        );

        assert_eq!(
            pager.texture_slot_state(6),
            Some(TextureSlotState::Reserved)
        );
        assert_eq!(
            pager.texture_slot_binding(6),
            Some(TextureSlotBinding {
                state: TextureSlotState::Reserved,
                ..retained
            })
        );
        assert_eq!(pager.texture_frame_snapshot().slot(6), None);
        assert_eq!(
            pager.page(retained.page).unwrap().state,
            PageState::Translated
        );
        assert_eq!(
            pager.materialize_texture(6, PageIndex::new(8)),
            Err(PagingError::ReservedTextureSlot(6))
        );
        assert_eq!(
            pager.mark_texture_slot_stale(6),
            Err(PagingError::ReservedTextureSlot(6))
        );

        let assignment = pager.materialize_texture_eid(eids[8]).unwrap();
        assert_eq!(assignment.slot, 5, "allocator must skip reserved slot 6");
        assert_eq!(pager.texture_slot_binding(6).unwrap().eid, retained.eid);

        assert_eq!(pager.release_reserved_texture_slot(6), Ok(true));
        assert_eq!(pager.release_reserved_texture_slot(6), Ok(false));
        assert_eq!(pager.texture_slot_state(6), Some(TextureSlotState::Free));
    }

    #[test]
    fn title_clut_reservations_match_native_thresholds_and_leave_slot_fifteen() {
        for (ipal_count, optional) in [
            (160, [false, false, false]),
            (161, [true, false, false]),
            (288, [true, false, false]),
            (289, [true, true, false]),
            (416, [true, true, false]),
            (417, [true, true, true]),
        ] {
            let mut pager = Pager::new();
            let eids = (0..8)
                .map(|index| register_texture(&mut pager, index))
                .collect::<Vec<_>>();
            for eid in eids {
                pager.materialize_texture_eid(eid).unwrap();
            }
            let slot_fifteen = pager.texture_slot_binding(7).unwrap();

            pager.reserve_title_clut_texture_slots(ipal_count).unwrap();
            for slot in [1, 2, 5] {
                assert_eq!(
                    pager.texture_slot_state(slot),
                    Some(TextureSlotState::Reserved),
                    "IPAL count {ipal_count}, native slot {}",
                    slot + 8
                );
                assert_eq!(pager.texture_frame_snapshot().slot(slot), None);
            }
            for ((slot, needed), threshold) in
                [(0, optional[0]), (4, optional[1]), (6, optional[2])]
                    .into_iter()
                    .zip([161, 289, 417])
            {
                assert_eq!(
                    pager.texture_slot_state(slot),
                    Some(if needed {
                        TextureSlotState::Reserved
                    } else {
                        TextureSlotState::Resident
                    }),
                    "IPAL count {ipal_count} around threshold {threshold}"
                );
            }
            assert_eq!(
                pager.texture_slot_state(3),
                Some(TextureSlotState::Resident)
            );
            assert_eq!(pager.texture_slot_binding(7), Some(slot_fifteen));

            pager.release_title_clut_texture_slots();
            for slot in [0, 1, 2, 4, 5, 6] {
                assert_ne!(
                    pager.texture_slot_state(slot),
                    Some(TextureSlotState::Reserved)
                );
            }
            assert_eq!(pager.texture_slot_binding(7), Some(slot_fifteen));
        }
    }

    #[test]
    fn repeated_reservation_does_not_rearm_a_duplicate_live_page_binding() {
        let mut pager = Pager::new();
        let eid = register_texture(&mut pager, 0);
        let first = pager.materialize_texture_eid(eid).unwrap();
        pager.reserve_texture_slot(first.slot).unwrap();

        // Reopening A allocates another slot while the old native state-30
        // struct still retains A's diagnostic identity.
        let reopened = pager.materialize_texture_eid(eid).unwrap();
        assert_ne!(reopened.slot, first.slot);
        assert_eq!(
            pager.page(reopened.binding.page).unwrap().state,
            PageState::Resident
        );

        pager.reserve_texture_slot(first.slot).unwrap();
        assert_eq!(
            pager.texture_slot_state(first.slot),
            Some(TextureSlotState::Reserved)
        );
        assert_eq!(
            pager.texture_slot_binding(reopened.slot),
            Some(reopened.binding)
        );
        assert_eq!(
            pager.page(reopened.binding.page).unwrap().state,
            PageState::Resident
        );
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
                TextureSlotState::Resident | TextureSlotState::Reserved => unreachable!(),
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

        let promoted = expect_resolution(
            pager
                .update_pending_virtual_page()
                .unwrap()
                .expect("free ordinary RAM promotes the queued page on NSUpdate"),
        );
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
    fn retail_cd_frame_cost_uses_validated_sector_lengths_and_a_one_frame_minimum() {
        let mut pager = Pager::new();
        for page in 0..4 {
            pager.register_page(PageIndex::new(page), []).unwrap();
        }
        enable_cd_transfer(&mut pager, 4, [(0, 1), (1, 5), (2, 31)]);

        assert_eq!(pager.cd_page_transfer_frames(PageIndex::new(0)), 1);
        assert_eq!(pager.cd_page_transfer_frames(PageIndex::new(1)), 1);
        assert_eq!(pager.cd_page_transfer_frames(PageIndex::new(2)), 6);
        assert_eq!(pager.cd_page_transfer_frames(PageIndex::new(3)), 6);
    }

    #[test]
    fn retail_cd_group_resolves_source_order_after_one_shared_seek() {
        let mut pager = Pager::new();
        for page in 0..=26 {
            pager.register_page(PageIndex::new(page), []).unwrap();
        }
        enable_cd_transfer(&mut pager, 27, [(23, 20), (24, 31), (26, 1)]);

        // Request insertion order is deliberately reversed. Native chooses
        // the lowest pgid and clones its source-contiguous successor.
        pager
            .open_page_virtual_with_outcome(PageIndex::new(24))
            .unwrap();
        pager
            .open_page_virtual_with_outcome(PageIndex::new(23))
            .unwrap();
        pager
            .open_page_virtual_with_outcome(PageIndex::new(26))
            .unwrap();

        for _ in 0..14 {
            assert_eq!(pager.update_pending_virtual_page(), Ok(None));
        }
        assert_eq!(
            expect_resolution(pager.update_pending_virtual_page().unwrap().unwrap()).page,
            PageIndex::new(23),
            "ten setup frames plus floor(20 / 5) transfer frames"
        );
        for _ in 0..5 {
            assert_eq!(pager.update_pending_virtual_page(), Ok(None));
        }
        assert_eq!(
            expect_resolution(pager.update_pending_virtual_page().unwrap().unwrap()).page,
            PageIndex::new(24),
            "the contiguous page shares setup and needs floor(31 / 5) frames"
        );

        // Page 25 breaks contiguity. Page 26 therefore pays a new ten-frame
        // setup plus the one-frame minimum for its one-sector body.
        for _ in 0..11 {
            assert_eq!(pager.update_pending_virtual_page(), Ok(None));
        }
        assert_eq!(
            expect_resolution(pager.update_pending_virtual_page().unwrap().unwrap()).page,
            PageIndex::new(26)
        );
        assert_eq!(pager.update_pending_virtual_page(), Ok(None));
    }

    #[test]
    fn retail_cd_wait_does_not_start_until_a_physical_run_is_replaceable() {
        let mut pager = Pager::new();
        pager.set_physical_slot_count(1).unwrap();
        for page in 0..2 {
            pager.register_page(PageIndex::new(page), []).unwrap();
        }
        enable_cd_transfer(&mut pager, 2, [(0, 1)]);
        let requested = PageIndex::new(0);
        let blocker = PageIndex::new(1);
        pager.open_page(blocker).unwrap();
        pager.open_page_virtual_with_outcome(requested).unwrap();

        for _ in 0..20 {
            assert_eq!(pager.update_pending_virtual_page(), Ok(None));
            assert!(pager.cd_transfer.is_none(), "a blocked seek must not start");
        }
        pager.close_page(blocker).unwrap();
        assert_eq!(
            pager.update_pending_virtual_page().unwrap(),
            Some(PagerUpdateOutcome::Invalidated(vec![blocker])),
            "reservation invalidation is published at seek start"
        );
        for _ in 0..10 {
            assert_eq!(pager.update_pending_virtual_page(), Ok(None));
        }
        let outcome = expect_resolution(pager.update_pending_virtual_page().unwrap().unwrap());
        assert_eq!(outcome.page, requested);
        assert_eq!(outcome.invalidated, PageInvalidations::NONE);
    }

    #[test]
    fn retail_cd_clone_reserves_a_whole_later_tied_run_before_countdown() {
        let mut pager = Pager::new();
        pager.set_physical_slot_count(3).unwrap();
        for page in 0..2 {
            let page = PageIndex::new(page);
            pager.register_page(page, []).unwrap();
            pager.open_page_virtual_with_outcome(page).unwrap();
        }
        enable_cd_transfer(&mut pager, 2, [(0, 1), (1, 1)]);

        assert_eq!(pager.update_pending_virtual_page(), Ok(None));
        assert_eq!(
            pager.physical_slots[..3],
            [None, Some(PageIndex::new(0)), Some(PageIndex::new(1))],
            "equal free runs choose the later native scan candidate"
        );
        assert_eq!(pager.cd_transfer.as_ref().unwrap().pages.len(), 2);
        assert!(
            pager
                .cd_transfer
                .as_ref()
                .unwrap()
                .pages
                .iter()
                .all(|member| member.cloned && member.reservation.is_some())
        );
    }

    #[test]
    fn retail_cd_reservation_keeps_existing_oldest_first_eviction_direction() {
        let mut pager = Pager::new();
        pager.set_physical_slot_count(2).unwrap();
        for page in 0..3 {
            pager.register_page(PageIndex::new(page), []).unwrap();
        }
        let requested = PageIndex::new(0);
        let oldest = PageIndex::new(1);
        let newest = PageIndex::new(2);
        pager.open_page(oldest).unwrap();
        pager.open_page(newest).unwrap();
        pager.close_page(oldest).unwrap();
        pager.close_page(newest).unwrap();
        enable_cd_transfer(&mut pager, 3, [(0, 1)]);
        pager.open_page_virtual_with_outcome(requested).unwrap();

        assert_eq!(
            pager.update_pending_virtual_page().unwrap(),
            Some(PagerUpdateOutcome::Invalidated(vec![oldest]))
        );
        assert_eq!(pager.page(oldest).unwrap().state, PageState::Raw);
        assert_eq!(pager.page(newest).unwrap().state, PageState::Translated);
        assert_eq!(pager.physical_slots[..2], [Some(requested), Some(newest)]);
    }

    #[test]
    fn retail_cd_reservation_reports_every_victim_before_the_group_resolves() {
        let mut pager = Pager::new();
        pager.set_physical_slot_count(4).unwrap();
        for page in 0..8 {
            pager.register_page(PageIndex::new(page), []).unwrap();
        }
        let requested = (0..4).map(PageIndex::new).collect::<Vec<_>>();
        let victims = (4..8).map(PageIndex::new).collect::<Vec<_>>();
        for victim in &victims {
            pager.open_page(*victim).unwrap();
        }
        for victim in &victims {
            pager.close_page(*victim).unwrap();
        }
        enable_cd_transfer(&mut pager, 8, requested.iter().map(|page| (page.get(), 1)));
        for page in &requested {
            pager.open_page_virtual_with_outcome(*page).unwrap();
        }

        assert_eq!(
            pager.update_pending_virtual_page().unwrap(),
            Some(PagerUpdateOutcome::Invalidated(victims.clone())),
            "a reservation can invalidate more pages than PageInvalidations can hold"
        );
        for victim in &victims {
            assert_eq!(pager.page(*victim).unwrap().state, PageState::Raw);
            assert_eq!(pager.page(*victim).unwrap().physical_slot(), None);
        }
        for _ in 0..10 {
            assert_eq!(pager.update_pending_virtual_page(), Ok(None));
        }
        let first = expect_resolution(pager.update_pending_virtual_page().unwrap().unwrap());
        assert_eq!(first.page, requested[0]);
        assert_eq!(first.invalidated, PageInvalidations::NONE);
    }

    #[test]
    fn retail_cd_clone_shortens_to_the_longest_available_prefix() {
        let mut pager = Pager::new();
        pager.set_physical_slot_count(2).unwrap();
        for page in 0..4 {
            pager.register_page(PageIndex::new(page), []).unwrap();
        }
        enable_cd_transfer(&mut pager, 4, [(0, 1), (1, 1), (2, 1)]);
        let blocker = PageIndex::new(3);
        pager.open_page(blocker).unwrap();
        for page in 0..3 {
            pager
                .open_page_virtual_with_outcome(PageIndex::new(page))
                .unwrap();
        }

        assert_eq!(pager.update_pending_virtual_page(), Ok(None));
        assert_eq!(pager.cd_transfer.as_ref().unwrap().pages.len(), 1);
        assert_eq!(
            pager.page(PageIndex::new(1)).unwrap().state,
            PageState::Queued
        );
        assert_eq!(
            pager.page(PageIndex::new(2)).unwrap().state,
            PageState::Queued
        );
        for _ in 0..10 {
            assert_eq!(pager.update_pending_virtual_page(), Ok(None));
        }
        assert_eq!(
            expect_resolution(pager.update_pending_virtual_page().unwrap().unwrap()).page,
            PageIndex::new(0)
        );
        assert_eq!(pager.update_pending_virtual_page(), Ok(None));
        assert!(pager.cd_transfer.is_none());
        assert_eq!(pager.page(blocker).unwrap().references, 1);
    }

    #[test]
    fn canceled_cd_clone_does_not_rejoin_when_reopened_during_the_same_read() {
        let mut pager = Pager::new();
        pager.set_physical_slot_count(2).unwrap();
        for page in 0..2 {
            let page = PageIndex::new(page);
            pager.register_page(page, []).unwrap();
            pager.open_page_virtual_with_outcome(page).unwrap();
        }
        enable_cd_transfer(&mut pager, 2, [(0, 1), (1, 1)]);

        assert_eq!(pager.update_pending_virtual_page(), Ok(None));
        pager.close_page_retail(PageIndex::new(0)).unwrap();
        assert_eq!(pager.page(PageIndex::new(0)).unwrap().state, PageState::Raw);
        pager
            .open_page_virtual_with_outcome(PageIndex::new(0))
            .unwrap();
        for _ in 0..11 {
            assert_eq!(pager.update_pending_virtual_page(), Ok(None));
        }
        assert_eq!(
            expect_resolution(pager.update_pending_virtual_page().unwrap().unwrap()).page,
            PageIndex::new(1),
            "the surviving sibling publishes while the reopened page stays queued"
        );
        assert_eq!(
            pager.page(PageIndex::new(0)).unwrap().state,
            PageState::Queued
        );
        for _ in 0..11 {
            assert_eq!(pager.update_pending_virtual_page(), Ok(None));
        }
        assert_eq!(
            expect_resolution(pager.update_pending_virtual_page().unwrap().unwrap()).page,
            PageIndex::new(0),
            "the reopened page pays for a new seek/read group"
        );
    }

    #[test]
    fn synchronous_materialization_consumes_the_in_flight_reservation() {
        let mut pager = Pager::new();
        pager.set_physical_slot_count(2).unwrap();
        for page in 0..2 {
            let page = PageIndex::new(page);
            pager.register_page(page, []).unwrap();
            pager.open_page_virtual_with_outcome(page).unwrap();
        }
        enable_cd_transfer(&mut pager, 2, [(0, 1), (1, 1)]);

        assert_eq!(pager.update_pending_virtual_page(), Ok(None));
        assert_eq!(pager.resident_physical_page_count(), 2);
        let materialized = pager
            .materialize_page_with_outcome(PageIndex::new(0))
            .unwrap();
        assert!(materialized.resolved);
        assert_eq!(
            pager.page(PageIndex::new(0)).unwrap().state,
            PageState::Translated
        );
        assert_eq!(pager.resident_physical_page_count(), 2);

        let mut publications = Vec::new();
        for _ in 0..20 {
            if let Some(outcome) = pager.update_pending_virtual_page().unwrap() {
                publications.push(expect_resolution(outcome).page);
            }
        }
        assert_eq!(publications, [PageIndex::new(1)]);
        assert!(pager.pending_virtual_pages().next().is_none());
    }

    #[test]
    fn request_queued_after_clone_start_pays_for_a_later_seek_group() {
        let mut pager = Pager::new();
        pager.set_physical_slot_count(2).unwrap();
        for page in 0..2 {
            pager.register_page(PageIndex::new(page), []).unwrap();
        }
        enable_cd_transfer(&mut pager, 2, [(0, 1), (1, 1)]);
        pager
            .open_page_virtual_with_outcome(PageIndex::new(0))
            .unwrap();

        assert_eq!(pager.update_pending_virtual_page(), Ok(None));
        pager
            .open_page_virtual_with_outcome(PageIndex::new(1))
            .unwrap();
        for _ in 0..10 {
            assert_eq!(pager.update_pending_virtual_page(), Ok(None));
        }
        assert_eq!(
            expect_resolution(pager.update_pending_virtual_page().unwrap().unwrap()).page,
            PageIndex::new(0)
        );
        for _ in 0..11 {
            assert_eq!(pager.update_pending_virtual_page(), Ok(None));
        }
        assert_eq!(
            expect_resolution(pager.update_pending_virtual_page().unwrap().unwrap()).page,
            PageIndex::new(1)
        );
    }

    #[test]
    fn nsupdate2_drains_timed_groups_without_exposing_intermediate_waits() {
        let mut pager = Pager::new();
        for page in 0..3 {
            pager.register_page(PageIndex::new(page), []).unwrap();
            pager
                .open_page_virtual_with_outcome(PageIndex::new(page))
                .unwrap();
        }
        enable_cd_transfer(&mut pager, 3, [(0, 1), (1, 5), (2, 31)]);

        let outcomes = pager.update_all_pending_virtual_pages().unwrap();
        assert_eq!(
            outcomes
                .iter()
                .map(|outcome| outcome.page)
                .collect::<Vec<_>>(),
            [PageIndex::new(0), PageIndex::new(1), PageIndex::new(2)]
        );
        assert!(pager.pending_virtual_pages().next().is_none());
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
            let outcome = expect_resolution(
                pager
                    .update_pending_virtual_page()
                    .unwrap()
                    .expect("free RAM promotes exactly one queued page"),
            );
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
        let promoted = expect_resolution(
            pager
                .update_pending_virtual_page()
                .unwrap()
                .expect("a zero-reference ordinary slot permits the next NSUpdate"),
        );
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
