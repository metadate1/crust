//! Explicit page, entry, and load-list ownership.

use std::collections::{BTreeMap, BTreeSet};

use crust_formats::binary::{Eid, EntryHandle, PageIndex};

pub const MAX_PHYSICAL_PAGES: usize = 128;
/// Retail's eight usable lower-VRAM slots. Rust slots `0..=7` correspond to
/// native physical slots `8..=15`; native slots `0..=7` hold frame buffers.
pub const TEXTURE_SLOT_COUNT: usize = 8;

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
    pub evicted: Option<TextureSlotBinding>,
}

/// Shared page-reference change produced by one native-idempotent close.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PagerCloseOutcome {
    pub page: PageIndex,
    pub decremented: bool,
}

/// Source page-state values, represented without pointer tagging.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum PageState {
    Free = 1,
    Raw = 3,
    Translated = 4,
    Resident = 20,
    Stale = 21,
    Inaccessible = 30,
}

/// Runtime metadata for a validated NSF page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageRecord {
    pub index: PageIndex,
    pub state: PageState,
    pub generation: u32,
    pub references: u32,
    entries: BTreeSet<EntryHandle>,
}

impl PageRecord {
    #[must_use]
    pub fn entries(&self) -> impl ExactSizeIterator<Item = EntryHandle> + '_ {
        self.entries.iter().copied()
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
    NoFreeTextureSlot(Eid),
}

/// Bounds-checked page registry replacing NS pointer relocation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Pager {
    pages: BTreeMap<PageIndex, PageRecord>,
    entries: BTreeMap<EntryHandle, u32>,
    eids: BTreeMap<Eid, EntryHandle>,
    page_eids: BTreeMap<Eid, PageIndex>,
    texture_page_eids: BTreeMap<PageIndex, Eid>,
    active: LoadList,
    texture_slots: [Option<PageIndex>; TEXTURE_SLOT_COUNT],
    texture_slot_states: [TextureSlotState; TEXTURE_SLOT_COUNT],
    texture_generations: [u32; TEXTURE_SLOT_COUNT],
    /// EIDs protected by native's current-zone load-list allocation rule.
    /// `None` represents a null `cur_zone`, which has a distinct fallback.
    current_texture_load_eids: Option<BTreeSet<Eid>>,
}

impl Pager {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
        if let Some(entry) = self.eids.get(&eid).copied() {
            return self.open_entry_with_outcome(entry);
        }
        let page = self
            .page_eids
            .get(&eid)
            .copied()
            .ok_or(PagingError::UnknownEid(eid))?;
        self.open_page_with_outcome(page)
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
    /// idempotence.
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
            return self.close_page_retail_with_outcome(entry.page());
        }
        let page = self
            .page_eids
            .get(&eid)
            .copied()
            .ok_or(PagingError::UnknownEid(eid))?;
        self.close_page_retail_with_outcome(page)
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

    pub fn set_page_inaccessible(&mut self, page: PageIndex) -> Result<(), PagingError> {
        let record = self
            .pages
            .get_mut(&page)
            .ok_or(PagingError::UnknownPage(page))?;
        if record.references != 0 {
            return Err(PagingError::InaccessiblePage(page));
        }
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
        let state = self
            .pages
            .get(&page)
            .ok_or(PagingError::UnknownPage(page))?
            .state;
        if state == PageState::Inaccessible {
            return Err(PagingError::InaccessiblePage(page));
        }
        // A type-one physical page is copied into a texture slot as part of
        // the same synchronous native `NSPageOpen` operation. Ordinary pages
        // have no reverse texture-EID binding and skip this branch.
        let evicted = if self.texture_page_eids.contains_key(&page) {
            self.materialize_texture_page(page)?
                .replaced
                .filter(|binding| binding.state == TextureSlotState::Resident)
        } else {
            None
        };
        let record = self
            .pages
            .get_mut(&page)
            .ok_or(PagingError::UnknownPage(page))?;
        record.references = record.references.saturating_add(1);
        if record.state == PageState::Raw {
            record.state = PageState::Translated;
        }
        Ok(PagerOpenOutcome { page, evicted })
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
            });
        }
        self.close_page(page)?;
        Ok(PagerCloseOutcome {
            page,
            decremented: true,
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
        let page = entry.page();
        let record = self
            .pages
            .get(&page)
            .ok_or(PagingError::UnknownPage(page))?;
        if !record.entries.contains(&entry) {
            return Err(PagingError::UnknownEntry(entry));
        }
        let outcome = self.open_page_with_outcome(page)?;
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
            self.open_page(page)?;
        }
        for entry in entries_to_open {
            self.open_entry(entry)?;
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
    fn load_list_texture_capacity_failure_is_transactional() {
        let mut pager = Pager::new();
        let eids = (0..=8)
            .map(|index| register_texture(&mut pager, index))
            .collect::<Vec<_>>();
        pager.set_current_texture_load_eids(eids.iter().copied());
        let before = pager.clone();

        assert_eq!(
            pager.apply_load_list(LoadList::new([], (0..=8).map(PageIndex::new))),
            Err(PagingError::NoFreeTextureSlot(eids[8]))
        );
        assert_eq!(pager, before);
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
            assert_eq!(pager.open_eid_with_outcome(eid).unwrap().evicted, None);
        }
        pager.set_current_texture_load_eids(eids.iter().skip(1).copied());

        let replacement = pager.open_eid_with_outcome(eids[8]).unwrap();

        assert_eq!(replacement.page, PageIndex::new(8));
        let evicted = replacement.evicted.unwrap();
        assert_eq!(evicted.page, PageIndex::new(0));
        assert_eq!(evicted.eid, eids[0]);
        assert_eq!(evicted.state, TextureSlotState::Resident);
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
            }
        );
        pager.close_eid_retail(eid_b).unwrap();
        assert_eq!(pager.entry_references(entry_b), Some(0));
        assert_eq!(pager.page(page).unwrap().references, 0);
    }
}
