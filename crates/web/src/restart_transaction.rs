//! Clone-owned browser restart transaction state.
//!
//! Native `LevelRestart` synchronously crosses object, audio, paging, and
//! memory-card host boundaries. The browser must therefore stage all of those
//! owners together and publish them only after every fallible boundary has
//! succeeded. Keeping the bundle target-independent lets native tests inject
//! failures at the same boundaries as the Wasm controller.

use crust_sim::camera::RetailCameraLocation;
use crust_sim::retail_runtime::RetailRestartLevelUpdateBoundary;

/// Records the last successfully published `LevelUpdate` boundary in a
/// possibly recursive `LevelRestart` transaction.
///
/// RESPAWN and TERM handlers may synchronously enter another restart. Native
/// samples `first_spawn` only after those handlers return, so neither the
/// final flags nor the final saved location can be locked before the runtime
/// reaches this boundary. The outer restart is always the last callback; that
/// is the one its returned report must describe.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RestartLevelUpdateAudit {
    last: Option<RetailRestartLevelUpdateBoundary>,
}

impl RestartLevelUpdateAudit {
    pub(crate) fn record(&mut self, boundary: RetailRestartLevelUpdateBoundary) {
        self.last = Some(boundary);
    }

    pub(crate) fn validate_final(
        self,
        location: RetailCameraLocation,
        flags: u8,
    ) -> Result<(), String> {
        let boundary = self
            .last
            .ok_or_else(|| "hard restart completed without a LevelUpdate boundary".to_owned())?;
        if boundary.location != location {
            return Err(format!(
                "hard-restart report location {:?} differs from its final LevelUpdate {:?}",
                location, boundary.location,
            ));
        }
        if boundary.flags != flags {
            return Err(format!(
                "hard-restart report flags {flags} differ from its final LevelUpdate {}",
                boundary.flags,
            ));
        }
        Ok(())
    }
}

/// Every mutable host owner reachable while a browser hard restart is staged.
///
/// The type parameters keep browser-only persistence out of native tests while
/// production instantiates the exact Rust runtime, audio, pager, card, and
/// `localStorage` owner types.
#[derive(Clone, Debug)]
pub(crate) struct RestartTransactionOwners<Runtime, Audio, Paging, Card, Storage> {
    pub(crate) retail_objects: Runtime,
    pub(crate) retail_audio: Audio,
    pub(crate) retail_zone_pager: Paging,
    pub(crate) card: Card,
    pub(crate) storage: Storage,
    pub(crate) pbak_play: bool,
}

impl<Runtime, Audio, Paging, Card, Storage>
    RestartTransactionOwners<Runtime, Audio, Paging, Card, Storage>
where
    Runtime: Clone,
    Audio: Clone,
    Paging: Clone,
    Card: Clone,
    Storage: Clone,
{
    /// Clones one coherent live owner set for a fail-closed restart attempt.
    #[must_use]
    pub(crate) fn begin(
        retail_objects: &Runtime,
        retail_audio: &Audio,
        retail_zone_pager: &Paging,
        card: &Card,
        storage: &Storage,
    ) -> Self {
        Self {
            retail_objects: retail_objects.clone(),
            retail_audio: retail_audio.clone(),
            retail_zone_pager: retail_zone_pager.clone(),
            card: card.clone(),
            storage: storage.clone(),
            pbak_play: false,
        }
    }

    /// Runs fallible work against a newly cloned owner set.
    ///
    /// Failure returns no candidate, making partial publication impossible;
    /// success returns the still-unpublished owners for any remaining checked
    /// restart work.
    pub(crate) fn stage<Output, Error>(
        retail_objects: &Runtime,
        retail_audio: &Audio,
        retail_zone_pager: &Paging,
        card: &Card,
        storage: &Storage,
        operation: impl FnOnce(&mut Self) -> Result<Output, Error>,
    ) -> Result<(Self, Output), Error> {
        let mut transaction = Self::begin(
            retail_objects,
            retail_audio,
            retail_zone_pager,
            card,
            storage,
        );
        let output = operation(&mut transaction)?;
        Ok((transaction, output))
    }

    /// Atomically publishes a completely validated owner set.
    ///
    /// This contains no fallible operation. Browser code must finish card
    /// persistence and every restart validation before calling it.
    pub(crate) fn commit_into(
        self,
        retail_objects: &mut Runtime,
        retail_audio: &mut Audio,
        retail_zone_pager: &mut Paging,
        card: &mut Card,
        storage: &mut Storage,
    ) {
        *retail_objects = self.retail_objects;
        *retail_audio = self.retail_audio;
        *retail_zone_pager = self.retail_zone_pager;
        *card = self.card;
        *storage = self.storage;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::num::NonZeroU16;
    use crust_audio::retail::RetailAudioEngine;

    use crust_formats::binary::{Eid, EntryHandle, PageIndex};
    use crust_formats::stream::RetailPathId;
    use crust_platform::persistence::VirtualCardRecord;
    use crust_sim::card::{CardPayload, SaveData, Slot, VirtualCard};
    use crust_sim::paging::{Pager, PagingError};
    use crust_sim::retail_frame::PathProgress;
    use crust_sim::retail_runtime::RetailRuntime;

    const TEST_GLOBAL: usize = 31;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum InjectedFailure {
        PbakPhysicalOpen,
        CaptionCreation,
        RespawnOrTerm,
        TemporarySlst,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct TestStorage {
        card_record: VirtualCardRecord,
        deferred_write_count: u32,
    }

    type TestOwners =
        RestartTransactionOwners<RetailRuntime, RetailAudioEngine, Pager, VirtualCard, TestStorage>;

    fn eid(name: &str) -> Eid {
        Eid::from_name(name).expect("test EID is valid")
    }

    fn registered_pager(entries: &[(Eid, u16)]) -> Pager {
        let page = PageIndex::new(0);
        let handles = entries
            .iter()
            .map(|(_, entry)| EntryHandle::new(page, *entry))
            .collect::<Vec<_>>();
        let mut pager = Pager::new();
        pager.register_page(page, handles.iter().copied()).unwrap();
        for ((eid, _), handle) in entries.iter().zip(handles) {
            pager.bind_eid(*eid, handle).unwrap();
        }
        pager
    }

    fn live_owners(pager: &Pager) -> TestOwners {
        RestartTransactionOwners::begin(
            &RetailRuntime::new(32),
            &RetailAudioEngine::default(),
            pager,
            &VirtualCard::new(),
            &TestStorage {
                card_record: VirtualCardRecord::default(),
                deferred_write_count: 0,
            },
        )
    }

    fn assert_pristine(live: &TestOwners, pager: &Pager) {
        assert_eq!(live.retail_objects.global_word(TEST_GLOBAL), Ok(0));
        assert_eq!(live.retail_audio.sfx_volume(), u8::MAX);
        assert_eq!(live.retail_audio.random_seed(), 0);
        assert_eq!(&live.retail_zone_pager, pager);
        assert!(live.card.slots().iter().all(|slot| *slot == Slot::Empty));
        assert_eq!(live.storage.card_record, VirtualCardRecord::default());
        assert_eq!(live.storage.deferred_write_count, 0);
        assert!(!live.pbak_play);
    }

    fn location(zone: &str, progress: i32) -> RetailCameraLocation {
        RetailCameraLocation {
            path: RetailPathId {
                zone: eid(zone),
                index: 0,
            },
            progress: PathProgress::clamped(progress, NonZeroU16::new(16).unwrap()),
        }
    }

    #[test]
    fn final_level_update_audit_accepts_late_sampled_nested_restart_state() {
        let nested = RetailRestartLevelUpdateBoundary {
            location: location("nestZ", 0x100),
            flags: 1,
            effective_flag: true,
        };
        let outer = RetailRestartLevelUpdateBoundary {
            location: location("outrZ", 0x200),
            flags: 0,
            effective_flag: true,
        };
        let mut audit = RestartLevelUpdateAudit::default();
        audit.record(nested);
        audit.record(outer);

        audit
            .validate_final(outer.location, outer.flags)
            .expect("the outer callback, not stale preflight state, is authoritative");
        assert!(audit.validate_final(nested.location, nested.flags).is_err());
    }

    #[test]
    fn retained_pbak_physical_open_failure_keeps_every_live_owner_unchanged() {
        let pbak = eid("pb0aB");
        let occupied = eid("old0Z");
        let occupied_handle = EntryHandle::new(PageIndex::new(0), 0);
        let pbak_handle = EntryHandle::new(PageIndex::new(1), 0);
        let mut pager = Pager::new();
        pager.set_physical_slot_count(1).unwrap();
        pager
            .register_page(PageIndex::new(0), [occupied_handle])
            .unwrap();
        pager
            .register_page(PageIndex::new(1), [pbak_handle])
            .unwrap();
        pager.bind_eid(occupied, occupied_handle).unwrap();
        pager.bind_eid(pbak, pbak_handle).unwrap();
        pager.open_eid(occupied).unwrap();
        let original_pager = pager.clone();
        let live = live_owners(&pager);

        let error = RestartTransactionOwners::stage(
            &live.retail_objects,
            &live.retail_audio,
            &live.retail_zone_pager,
            &live.card,
            &live.storage,
            |candidate| {
                candidate.pbak_play = true;
                candidate
                    .retail_zone_pager
                    .open_eid_with_outcome(pbak)
                    .map(|_| ())
                    .map_err(|error| {
                        assert_eq!(error, PagingError::NoFreePhysicalSlot(PageIndex::new(1)));
                        InjectedFailure::PbakPhysicalOpen
                    })
            },
        )
        .unwrap_err();

        assert_eq!(error, InjectedFailure::PbakPhysicalOpen);
        assert_pristine(&live, &original_pager);
        assert_eq!(
            live.retail_zone_pager.entry_references(pbak_handle),
            Some(0)
        );
        assert_eq!(
            live.retail_zone_pager.entry_references(occupied_handle),
            Some(1)
        );
    }

    #[test]
    fn caption_failure_drops_the_successful_retained_pbak_open() {
        let pbak = eid("pb0aB");
        let pbak_handle = EntryHandle::new(PageIndex::new(0), 0);
        let pager = registered_pager(&[(pbak, 0)]);
        let original_pager = pager.clone();
        let live = live_owners(&pager);

        let error = RestartTransactionOwners::stage(
            &live.retail_objects,
            &live.retail_audio,
            &live.retail_zone_pager,
            &live.card,
            &live.storage,
            |candidate| {
                candidate.pbak_play = true;
                candidate.retail_zone_pager.open_eid(pbak).unwrap();
                assert_eq!(
                    candidate.retail_zone_pager.entry_references(pbak_handle),
                    Some(1),
                    "the staged PBAK reference exists before caption creation"
                );
                Err::<(), _>(InjectedFailure::CaptionCreation)
            },
        )
        .unwrap_err();

        assert_eq!(error, InjectedFailure::CaptionCreation);
        assert_pristine(&live, &original_pager);
        assert_eq!(
            live.retail_zone_pager.entry_references(pbak_handle),
            Some(0)
        );
    }

    #[test]
    fn respawn_or_term_failure_rolls_back_runtime_audio_card_storage_and_paging() {
        let handler_page = eid("rsp0Z");
        let handler_handle = EntryHandle::new(PageIndex::new(0), 0);
        let pager = registered_pager(&[(handler_page, 0)]);
        let original_pager = pager.clone();
        let live = live_owners(&pager);

        let error = RestartTransactionOwners::stage(
            &live.retail_objects,
            &live.retail_audio,
            &live.retail_zone_pager,
            &live.card,
            &live.storage,
            |candidate| {
                candidate
                    .retail_objects
                    .set_global_word(TEST_GLOBAL, 0xfeed_cafe)
                    .unwrap();
                candidate.retail_audio.set_sfx_volume(17);
                candidate.retail_audio.set_random_seed(0x1234_5678);
                candidate.retail_zone_pager.open_eid(handler_page).unwrap();
                candidate
                    .card
                    .set_slot(
                        3,
                        Slot::Valid(CardPayload::encode(SaveData {
                            level_count: 12,
                            ..SaveData::default()
                        })),
                    )
                    .unwrap();
                candidate.storage.card_record.updated_at = 77;
                candidate.storage.deferred_write_count = 1;
                Err::<(), _>(InjectedFailure::RespawnOrTerm)
            },
        )
        .unwrap_err();

        assert_eq!(error, InjectedFailure::RespawnOrTerm);
        assert_pristine(&live, &original_pager);
        assert_eq!(
            live.retail_zone_pager.entry_references(handler_handle),
            Some(0)
        );
    }

    #[test]
    fn temporary_zdat_slst_failure_rolls_back_preceding_handler_mutations() {
        let zdat = eid("rst0Z");
        let slst = eid("rst0S");
        let zdat_handle = EntryHandle::new(PageIndex::new(0), 0);
        let slst_handle = EntryHandle::new(PageIndex::new(1), 0);
        let mut pager = Pager::new();
        pager.set_physical_slot_count(1).unwrap();
        pager
            .register_page(PageIndex::new(0), [zdat_handle])
            .unwrap();
        pager
            .register_page(PageIndex::new(1), [slst_handle])
            .unwrap();
        pager.bind_eid(zdat, zdat_handle).unwrap();
        pager.bind_eid(slst, slst_handle).unwrap();
        let original_pager = pager.clone();
        let live = live_owners(&pager);

        let error = RestartTransactionOwners::stage(
            &live.retail_objects,
            &live.retail_audio,
            &live.retail_zone_pager,
            &live.card,
            &live.storage,
            |candidate| {
                // Model mutations already made by RESPAWN/TERM before
                // LevelUpdate reaches its temporary physical ZDAT/SLST
                // sequence.
                candidate
                    .retail_objects
                    .set_global_word(TEST_GLOBAL, 0xaaaa_5555)
                    .unwrap();
                candidate.retail_audio.set_sfx_volume(1);
                candidate.storage.deferred_write_count = 1;

                candidate.retail_zone_pager.clear_current_texture_zone();
                candidate.retail_zone_pager.open_eid(zdat).unwrap();
                assert_eq!(
                    candidate.retail_zone_pager.entry_references(zdat_handle),
                    Some(1)
                );
                candidate.retail_zone_pager.open_eid(slst).map_err(|error| {
                    assert_eq!(error, PagingError::NoFreePhysicalSlot(PageIndex::new(1)));
                    InjectedFailure::TemporarySlst
                })
            },
        )
        .unwrap_err();

        assert_eq!(error, InjectedFailure::TemporarySlst);
        assert_pristine(&live, &original_pager);
        assert_eq!(
            live.retail_zone_pager.entry_references(zdat_handle),
            Some(0)
        );
        assert_eq!(
            live.retail_zone_pager.entry_references(slst_handle),
            Some(0)
        );
    }

    #[test]
    fn successful_restart_publishes_all_staged_owners_together() {
        let pbak = eid("pb0aB");
        let handle = EntryHandle::new(PageIndex::new(0), 0);
        let pager = registered_pager(&[(pbak, 0)]);
        let mut live = live_owners(&pager);

        let (candidate, ()) = RestartTransactionOwners::stage(
            &live.retail_objects,
            &live.retail_audio,
            &live.retail_zone_pager,
            &live.card,
            &live.storage,
            |candidate| {
                candidate.pbak_play = true;
                candidate
                    .retail_objects
                    .set_global_word(TEST_GLOBAL, 42)
                    .unwrap();
                candidate.retail_audio.set_sfx_volume(42);
                candidate.retail_zone_pager.open_eid(pbak).unwrap();
                candidate.storage.deferred_write_count = 1;
                Ok::<(), InjectedFailure>(())
            },
        )
        .unwrap();
        let committed_pbak = candidate.pbak_play;
        candidate.commit_into(
            &mut live.retail_objects,
            &mut live.retail_audio,
            &mut live.retail_zone_pager,
            &mut live.card,
            &mut live.storage,
        );
        live.pbak_play = committed_pbak;

        assert!(live.pbak_play);
        assert_eq!(live.retail_objects.global_word(TEST_GLOBAL), Ok(42));
        assert_eq!(live.retail_audio.sfx_volume(), 42);
        assert_eq!(live.retail_zone_pager.entry_references(handle), Some(1));
        assert_eq!(live.storage.deferred_write_count, 1);
    }
}
