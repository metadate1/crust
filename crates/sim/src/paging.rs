//! Explicit page, entry, and load-list ownership.

use std::collections::{BTreeMap, BTreeSet};

use crust_formats::binary::{Eid, EntryHandle, PageIndex};

pub const MAX_PHYSICAL_PAGES: usize = 128;
pub const TEXTURE_SLOT_COUNT: usize = 8;

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
}

/// Bounds-checked page registry replacing NS pointer relocation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Pager {
    pages: BTreeMap<PageIndex, PageRecord>,
    entries: BTreeMap<EntryHandle, u32>,
    eids: BTreeMap<Eid, EntryHandle>,
    active: LoadList,
    texture_slots: [Option<PageIndex>; TEXTURE_SLOT_COUNT],
    texture_generations: [u32; TEXTURE_SLOT_COUNT],
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
        self.eids.insert(eid, entry);
        Ok(())
    }

    pub fn resolve_eid(&self, eid: Eid) -> Result<EntryHandle, PagingError> {
        self.eids
            .get(&eid)
            .copied()
            .ok_or(PagingError::UnknownEid(eid))
    }

    #[must_use]
    pub fn page(&self, page: PageIndex) -> Option<&PageRecord> {
        self.pages.get(&page)
    }

    #[must_use]
    pub fn active_load_list(&self) -> &LoadList {
        &self.active
    }

    #[must_use]
    pub fn entry_references(&self, entry: EntryHandle) -> Option<u32> {
        self.entries.get(&entry).copied()
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
        let record = self
            .pages
            .get_mut(&page)
            .ok_or(PagingError::UnknownPage(page))?;
        if record.state == PageState::Inaccessible {
            return Err(PagingError::InaccessiblePage(page));
        }
        record.references = record.references.saturating_add(1);
        if record.state == PageState::Raw {
            record.state = PageState::Translated;
        }
        Ok(())
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

    pub fn open_entry(&mut self, entry: EntryHandle) -> Result<(), PagingError> {
        let page = entry.page();
        let record = self
            .pages
            .get(&page)
            .ok_or(PagingError::UnknownPage(page))?;
        if !record.entries.contains(&entry) {
            return Err(PagingError::UnknownEntry(entry));
        }
        self.open_page(page)?;
        let references = self
            .entries
            .get_mut(&entry)
            .ok_or(PagingError::UnknownEntry(entry))?;
        *references = references.saturating_add(1);
        Ok(())
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

    /// Assigns a physical page to one of the eight texture slots.
    pub fn materialize_texture(
        &mut self,
        slot: usize,
        page: PageIndex,
    ) -> Result<u32, PagingError> {
        let previous = *self
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
        if let Some(previous) = previous
            && previous != page
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
        Ok(record.generation)
    }

    #[must_use]
    pub fn texture_slot(&self, slot: usize) -> Option<(PageIndex, u32)> {
        self.texture_slots
            .get(slot)
            .copied()
            .flatten()
            .map(|page| (page, self.texture_generations[slot]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(page: u32, index: u16) -> EntryHandle {
        EntryHandle::new(PageIndex::new(page), index)
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

        pager
            .apply_load_list(LoadList::new([entry(0, 1), entry(1, 0)], []))
            .unwrap();
        assert_eq!(pager.entry_references(entry(0, 0)), Some(0));
        assert_eq!(pager.entry_references(entry(0, 1)), Some(1));
        assert_eq!(pager.entry_references(entry(1, 0)), Some(1));
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
        pager
            .register_page(PageIndex::new(0), [entry(0, 0)])
            .unwrap();
        pager
            .register_page(PageIndex::new(1), [entry(1, 0)])
            .unwrap();
        assert_eq!(pager.materialize_texture(7, PageIndex::new(0)), Ok(1));
        assert_eq!(pager.materialize_texture(7, PageIndex::new(1)), Ok(2));
        assert_eq!(
            pager.page(PageIndex::new(0)).unwrap().state,
            PageState::Stale
        );
        assert_eq!(pager.texture_slot(7), Some((PageIndex::new(1), 2)));
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
}
