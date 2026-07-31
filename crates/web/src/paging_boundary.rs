//! Host-side synchronous paging boundaries shared by the Wasm application
//! and native unit tests.

use crust_formats::binary::{Eid, PageIndex};
use crust_formats::stream::TitleMdat;
use crust_sim::camera::RetailCameraLocation;
use crust_sim::paging::{
    Pager, PagerCloseOutcome, PagerOpenOutcome, PagerUpdateOutcome, PagingError, TextureSlotState,
};

/// One externally visible page-generation change produced by host-side
/// retail paging. The browser applies these to the GOOL paging mirror in this
/// exact order after the matching [`Pager`] operation succeeds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RetailPagingPublication {
    Open(PagerOpenOutcome),
    Close(PagerCloseOutcome),
    SynchronousUpdate(PagerUpdateOutcome),
    /// Resident texture pages displaced when title-card CLUTs claim native
    /// VRAM slots in state 30.
    TitleTextureReservations(Vec<PageIndex>),
}

/// Ordered, bounds-checked counterpart of one source `TitleLoadEntries` graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RetailTitleEntryPlan {
    mdat: Eid,
    ipal_count: u32,
    transient_ipals: Vec<Eid>,
    retained_images: Vec<Eid>,
    retained_tgeos: Vec<Eid>,
}

impl RetailTitleEntryPlan {
    /// Extracts exactly the serialized ranges traversed by `TitleLoadEntries`.
    pub(crate) fn from_mdat(mdat: &TitleMdat) -> Result<Self, String> {
        let ipal_count = u32::try_from(mdat.header.palette_count)
            .map_err(|_| "title MDAT has a negative IPAL count".to_owned())?;
        let ipal_entries = usize::try_from(ipal_count.div_ceil(120))
            .map_err(|_| "title MDAT IPAL table length does not fit this host".to_owned())?;
        if ipal_entries > mdat.header.palettes.len() {
            return Err(format!(
                "title MDAT requires {ipal_entries} IPAL entries but stores only {}",
                mdat.header.palettes.len()
            ));
        }
        let image_count = usize::try_from(mdat.header.width_tiles)
            .map_err(|_| "title MDAT has a negative image-column count".to_owned())?;
        if image_count > mdat.header.images.len() {
            return Err(format!(
                "title MDAT requires {image_count} IMAG entries but stores only {}",
                mdat.header.images.len()
            ));
        }
        let tgeo_count = usize::try_from(mdat.header.geometry_count)
            .map_err(|_| "title MDAT has a negative TGEO count".to_owned())?;
        if tgeo_count > mdat.header.geometries.len() {
            return Err(format!(
                "title MDAT requires {tgeo_count} TGEO entries but stores only {}",
                mdat.header.geometries.len()
            ));
        }
        Ok(Self {
            mdat: mdat.eid,
            ipal_count,
            transient_ipals: mdat.header.palettes[..ipal_entries]
                .iter()
                .copied()
                .filter(|eid| *eid != Eid::NONE)
                .collect(),
            retained_images: mdat.header.images[..image_count]
                .iter()
                .copied()
                .filter(|eid| *eid != Eid::NONE)
                .collect(),
            retained_tgeos: mdat.header.geometries[..tgeo_count]
                .iter()
                .copied()
                .filter(|eid| *eid != Eid::NONE)
                .collect(),
        })
    }

    fn retained_entries(&self) -> impl Iterator<Item = Eid> + '_ {
        self.retained_images
            .iter()
            .chain(&self.retained_tgeos)
            .copied()
    }
}

fn reserve_retail_title_texture_slots(
    pager: &mut Pager,
    ipal_count: u32,
    mut publish: impl FnMut(RetailPagingPublication) -> Result<(), String>,
) -> Result<(), String> {
    // Keep collection separate from mutation: the pager deliberately retains
    // old slot identities after `NSTexturePageFree`.
    let mut invalidated = Vec::new();
    for slot in [1_usize, 2, 5]
        .into_iter()
        .chain((ipal_count > 160).then_some(0))
        .chain((ipal_count > 288).then_some(4))
        .chain((ipal_count >= 417).then_some(6))
    {
        if let Some(binding) = pager.texture_slot_binding(slot)
            && binding.state == TextureSlotState::Resident
        {
            invalidated.push(binding.page);
        }
    }
    pager
        .reserve_title_clut_texture_slots(ipal_count)
        .map_err(|error| format!("could not reserve title CLUT texture slots: {error:?}"))?;
    if invalidated.is_empty() {
        Ok(())
    } else {
        publish(RetailPagingPublication::TitleTextureReservations(
            invalidated,
        ))
    }
}

/// Opens one title-state MDAT graph in native order.
///
/// MDAT, IPAL, IMAG, and TGEO entries use physical `NSOpen`. Each IPAL is
/// closed immediately after its CLUTs are consumed; IMAG/TGEO references stay
/// owned until the state fades out.
pub(crate) fn stage_retail_title_entries_open(
    pager: &mut Pager,
    plan: &RetailTitleEntryPlan,
    mut publish: impl FnMut(RetailPagingPublication) -> Result<(), String>,
) -> Result<(), String> {
    publish(open_retail_physical_eid(pager, plan.mdat).map_err(|error| {
        format!(
            "could not physically open title MDAT {}: {error:?}",
            plan.mdat
        )
    })?)?;
    for ipal in &plan.transient_ipals {
        publish(
            open_retail_physical_eid(pager, *ipal).map_err(|error| {
                format!("could not physically open title IPAL {ipal}: {error:?}")
            })?,
        )?;
        publish(
            close_retail_physical_eid(pager, *ipal).map_err(|error| {
                format!("could not close transient title IPAL {ipal}: {error:?}")
            })?,
        )?;
    }
    reserve_retail_title_texture_slots(pager, plan.ipal_count, &mut publish)?;
    for eid in plan.retained_entries() {
        publish(open_retail_physical_eid(pager, eid).map_err(|error| {
            format!("could not physically open retained title entry {eid}: {error:?}")
        })?)?;
    }
    Ok(())
}

/// Closes one fading-out title-state MDAT graph in source order.
pub(crate) fn stage_retail_title_entries_close(
    pager: &mut Pager,
    plan: &RetailTitleEntryPlan,
    mut publish: impl FnMut(RetailPagingPublication) -> Result<(), String>,
) -> Result<(), String> {
    publish(
        close_retail_physical_eid(pager, plan.mdat)
            .map_err(|error| format!("could not close title MDAT {}: {error:?}", plan.mdat))?,
    )?;
    // The PSX block runs for both positive and negative TitleLoadEntries
    // counts, before the retained IMAG/TGEO loop.
    reserve_retail_title_texture_slots(pager, plan.ipal_count, &mut publish)?;
    for eid in plan.retained_entries() {
        publish(
            close_retail_physical_eid(pager, eid).map_err(|error| {
                format!("could not close retained title entry {eid}: {error:?}")
            })?,
        )?;
    }
    Ok(())
}

/// Native reopens the target path's SLST only when the ZDAT owns worlds and
/// `LevelUpdate` changes zone, path, or integer path point. A fractional-only
/// progress change deliberately reuses the current polygon list.
#[must_use]
pub(crate) const fn retail_level_update_opens_visibility(
    before: RetailCameraLocation,
    after: RetailCameraLocation,
    target_world_count: usize,
) -> bool {
    target_world_count != 0
        && (before.path.zone.raw() != after.path.zone.raw()
            || before.path.index != after.path.index
            || before.progress.point_index() != after.progress.point_index())
}

/// Physically opens an EID through native `NSOpen(ref, 1, 1)` semantics.
pub(crate) fn open_retail_physical_eid(
    pager: &mut Pager,
    eid: Eid,
) -> Result<RetailPagingPublication, PagingError> {
    pager
        .open_eid_with_outcome(eid)
        .map(RetailPagingPublication::Open)
}

/// Closes an EID through native `NSClose(ref, 1)` semantics.
pub(crate) fn close_retail_physical_eid(
    pager: &mut Pager,
    eid: Eid,
) -> Result<RetailPagingPublication, PagingError> {
    pager
        .close_eid_retail_with_outcome(eid)
        .map(RetailPagingPublication::Close)
}

/// Reconstructs the temporary physical SLST lifetime at the start of every
/// changed, world-owning `LevelUpdate`.
///
/// The open and close must remain two separately published operations: the
/// open may displace a zero-reference page, while the close releases the SLST
/// before the old zone's TERM/load-list work starts.
pub(crate) fn stage_retail_visibility_list(
    pager: &mut Pager,
    visibility_list: Eid,
    mut publish: impl FnMut(RetailPagingPublication) -> Result<(), String>,
) -> Result<(), String> {
    let opened = open_retail_physical_eid(pager, visibility_list)
        .map_err(|error| format!("could not physically open SLST {visibility_list}: {error:?}"))?;
    publish(opened)?;
    let closed = close_retail_physical_eid(pager, visibility_list)
        .map_err(|error| format!("could not close physical SLST {visibility_list}: {error:?}"))?;
    publish(closed)
}

/// Applies the physical prefix shared by ordinary changed-world updates and
/// `TitleLoadScreen`. The optional title ZDAT stays referenced after this
/// function returns so the caller can run the zone load-list/`NSUpdate2`
/// suffix before closing it.
pub(crate) fn stage_retail_level_update_physical_prefix(
    pager: &mut Pager,
    physical_zdat: Option<Eid>,
    visibility_list: Option<Eid>,
    mut publish: impl FnMut(RetailPagingPublication) -> Result<(), String>,
) -> Result<(), String> {
    if let Some(zone) = physical_zdat {
        let publication = open_retail_physical_eid(pager, zone)
            .map_err(|error| format!("could not physically open title ZDAT {zone}: {error:?}"))?;
        publish(publication)?;
    }
    if let Some(slst) = visibility_list {
        stage_retail_visibility_list(pager, slst, &mut publish)?;
    }
    Ok(())
}

/// Closes the optional title ZDAT after the enclosed `LevelUpdate` completes.
pub(crate) fn stage_retail_level_update_physical_suffix(
    pager: &mut Pager,
    physical_zdat: Option<Eid>,
    mut publish: impl FnMut(RetailPagingPublication) -> Result<(), String>,
) -> Result<(), String> {
    if let Some(zone) = physical_zdat {
        let publication = close_retail_physical_eid(pager, zone)
            .map_err(|error| format!("could not close physical title ZDAT {zone}: {error:?}"))?;
        publish(publication)?;
    }
    Ok(())
}

/// Completes the synchronous page-update boundary used by a retail
/// `LevelUpdate` whose effective local flag is nonzero. PSX reaches this state
/// through `NSUpdate2`; the non-PSX source resolves the same flag-zero opens
/// inline. Title-state attract PBAK reduces the effective flag to zero, so its
/// queued PSX work is deliberately left for ordinary per-frame updates.
///
/// Keep this policy at the browser/platform boundary: every reservation
/// invalidation and completed page is published in its source order, while a
/// permanently stalled allocator fails instead of reproducing native's
/// unbounded `NSUpdate2` loop. Validated NSF pages occupy at most 32 sectors;
/// sixty-four update attempts per page is therefore a deliberately generous
/// bound over seek setup, transfer, and shortened physical groups.
pub(crate) fn drain_retail_level_update_pages(
    pager: &mut Pager,
    mut publish: impl FnMut(PagerUpdateOutcome) -> Result<(), String>,
) -> Result<(), String> {
    let step_budget = pager.page_count().saturating_mul(64).max(64);
    for _ in 0..step_budget {
        if pager.pending_virtual_pages().next().is_none() {
            return Ok(());
        }
        if let Some(outcome) = pager
            .update_pending_virtual_page()
            .map_err(|error| format!("synchronous pager update failed: {error:?}"))?
        {
            publish(outcome)?;
        }
    }
    let page = pager
        .pending_virtual_pages()
        .next()
        .expect("an exhausted synchronous update budget retains a pending page");
    Err(format!(
        "synchronous pager update stalled with page {} still pending",
        page.get()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crust_formats::binary::{EntryHandle, PageIndex};
    use crust_formats::stream::{ZoneEntity, structs::MdatHeader};
    use crust_sim::paging::{PageInvalidations, TextureSlotState};

    fn eid(name: &str) -> Eid {
        Eid::from_name(name).unwrap()
    }

    fn register_entry(pager: &mut Pager, page: PageIndex, entry_index: u16, name: &str) -> Eid {
        let eid = eid(name);
        let handle = EntryHandle::new(page, entry_index);
        pager.register_page(page, [handle]).unwrap();
        pager.bind_eid(eid, handle).unwrap();
        eid
    }

    fn location(zone: Eid, path: u32, progress: i32) -> RetailCameraLocation {
        use core::num::NonZeroU16;
        use crust_formats::stream::RetailPathId;
        use crust_sim::retail_frame::PathProgress;

        RetailCameraLocation {
            path: RetailPathId { zone, index: path },
            progress: PathProgress::clamped(progress, NonZeroU16::new(16).unwrap()),
        }
    }

    fn mdat_fixture(
        mdat: Eid,
        palette_count: i32,
        palettes: &[Eid],
        images: &[Eid],
        tgeos: &[Eid],
    ) -> TitleMdat {
        let mut palette_table = [Eid::NONE; 46];
        palette_table[..palettes.len()].copy_from_slice(palettes);
        let mut image_table = [Eid::NONE; 32];
        image_table[..images.len()].copy_from_slice(images);
        let mut geometry_table = [Eid::NONE; 32];
        geometry_table[..tgeos.len()].copy_from_slice(tgeos);
        TitleMdat {
            eid: mdat,
            header: MdatHeader {
                width_tiles: i32::try_from(images.len()).unwrap(),
                height_tiles: 15,
                palette_count,
                entity_count: 0,
                unknown_4: 0,
                geometry_count: i32::try_from(tgeos.len()).unwrap(),
                clut_lines: [crust_formats::stream::structs::ClutLine {
                    x: 0,
                    y: 0,
                    count: 0,
                }; 8],
                palettes: palette_table,
                geometries: geometry_table,
                images: image_table,
            },
            entities: Vec::<ZoneEntity>::new(),
        }
    }

    #[test]
    fn title_entry_plan_opens_transient_ipals_and_retains_images_and_tgeos() {
        let mut pager = Pager::new();
        let mdat = register_entry(&mut pager, PageIndex::new(0), 0, "md000");
        let ipal_a = register_entry(&mut pager, PageIndex::new(1), 0, "ip000");
        let ipal_b = register_entry(&mut pager, PageIndex::new(2), 0, "ip001");
        let image_a = register_entry(&mut pager, PageIndex::new(3), 0, "im000");
        let image_b = register_entry(&mut pager, PageIndex::new(4), 0, "im001");
        let tgeo = register_entry(&mut pager, PageIndex::new(5), 0, "tg000");
        let plan = RetailTitleEntryPlan::from_mdat(&mdat_fixture(
            mdat,
            121,
            &[ipal_a, ipal_b],
            &[image_a, image_b],
            &[tgeo],
        ))
        .unwrap();

        let mut open_trace = Vec::new();
        stage_retail_title_entries_open(&mut pager, &plan, |publication| {
            open_trace.push(publication);
            Ok(())
        })
        .unwrap();

        assert_eq!(plan.mdat, mdat);
        assert_eq!(
            1 + plan.retained_images.len() + plan.retained_tgeos.len(),
            4
        );
        assert_eq!(open_trace.len(), 8);
        assert!(matches!(open_trace[0], RetailPagingPublication::Open(_)));
        assert!(matches!(open_trace[1], RetailPagingPublication::Open(_)));
        assert!(matches!(open_trace[2], RetailPagingPublication::Close(_)));
        assert!(matches!(open_trace[3], RetailPagingPublication::Open(_)));
        assert!(matches!(open_trace[4], RetailPagingPublication::Close(_)));
        for page in 0..=5 {
            let expected = u32::from(matches!(page, 0 | 3 | 4 | 5));
            assert_eq!(
                pager.page(PageIndex::new(page)).unwrap().references,
                expected,
                "page {page}"
            );
        }

        let mut close_trace = Vec::new();
        stage_retail_title_entries_close(&mut pager, &plan, |publication| {
            close_trace.push(publication);
            Ok(())
        })
        .unwrap();
        assert_eq!(close_trace.len(), 4);
        assert!(
            close_trace
                .iter()
                .all(|publication| matches!(publication, RetailPagingPublication::Close(_)))
        );
        assert_eq!(pager.total_page_references(), 0);
        assert_eq!(pager.total_entry_references(), 0);
    }

    #[test]
    fn title_reservation_publication_lists_source_order_evictions() {
        let mut pager = Pager::new();
        let mut pages = Vec::new();
        for index in 0..8_u32 {
            let page = PageIndex::new(index);
            let texture = eid(&format!("tx{index:03}"));
            pager.register_page(page, []).unwrap();
            pager.bind_page_eid(texture, page).unwrap();
            pager.materialize_texture_eid(texture).unwrap();
            pages.push(page);
        }
        let slot_fifteen = pager.texture_slot_binding(7).unwrap();
        let mut publications = Vec::new();

        reserve_retail_title_texture_slots(&mut pager, 417, |publication| {
            publications.push(publication);
            Ok(())
        })
        .unwrap();

        assert_eq!(
            publications,
            [RetailPagingPublication::TitleTextureReservations(vec![
                pages[6], pages[5], pages[2], pages[7], pages[3], pages[1]
            ])]
        );
        for slot in [0, 1, 2, 4, 5, 6] {
            assert_eq!(
                pager.texture_slot_state(slot),
                Some(TextureSlotState::Reserved)
            );
            assert_eq!(pager.texture_frame_snapshot().slot(slot), None);
        }
        assert_eq!(pager.texture_slot_binding(7), Some(slot_fifteen));
    }

    #[test]
    fn title_entry_plan_rejects_an_image_range_outside_the_fixed_table() {
        let mut malformed = mdat_fixture(eid("md000"), 0, &[], &[], &[]);
        malformed.header.width_tiles = 33;
        assert_eq!(
            RetailTitleEntryPlan::from_mdat(&malformed),
            Err("title MDAT requires 33 IMAG entries but stores only 32".to_owned())
        );
    }

    #[test]
    fn visibility_predicate_ignores_fractional_only_and_worldless_updates() {
        let zone = eid("zone0");
        let other = eid("zone1");
        let before = location(zone, 0, 0x120);

        assert!(!retail_level_update_opens_visibility(
            before,
            location(zone, 0, 0x1f0),
            1
        ));
        assert!(retail_level_update_opens_visibility(
            before,
            location(zone, 0, 0x200),
            1
        ));
        assert!(retail_level_update_opens_visibility(
            before,
            location(zone, 1, 0x120),
            1
        ));
        assert!(retail_level_update_opens_visibility(
            before,
            location(other, 0, 0x120),
            1
        ));
        assert!(!retail_level_update_opens_visibility(
            before,
            location(other, 0, 0x120),
            0
        ));
    }

    #[test]
    fn changed_world_visibility_is_a_physical_open_then_close() {
        let old_page = PageIndex::new(0);
        let slst_page = PageIndex::new(1);
        let mut pager = Pager::new();
        pager.set_physical_slot_count(1).unwrap();
        pager.register_page(old_page, []).unwrap();
        let slst = register_entry(&mut pager, slst_page, 4, "vis00");
        pager.materialize_page_with_outcome(old_page).unwrap();

        let mut trace = Vec::new();
        stage_retail_visibility_list(&mut pager, slst, |publication| {
            trace.push(publication);
            Ok(())
        })
        .unwrap();

        assert_eq!(
            trace,
            [
                RetailPagingPublication::Open(PagerOpenOutcome {
                    page: slst_page,
                    resolved: true,
                    invalidated: PageInvalidations::one(old_page),
                    evicted: None,
                }),
                RetailPagingPublication::Close(PagerCloseOutcome {
                    page: slst_page,
                    decremented: true,
                    unresolved: false,
                }),
            ]
        );
        assert_eq!(pager.resolved_pages().collect::<Vec<_>>(), [slst_page]);
        assert_eq!(
            pager
                .page_reference_counts()
                .find(|(page, _)| *page == slst_page),
            Some((slst_page, 0))
        );
    }

    #[test]
    fn title_zdat_reference_can_make_following_slst_fail_in_a_tight_heap() {
        let zdat_page = PageIndex::new(0);
        let slst_page = PageIndex::new(1);
        let mut pager = Pager::new();
        pager.set_physical_slot_count(1).unwrap();
        let zdat = register_entry(&mut pager, zdat_page, 4, "zone0");
        let slst = register_entry(&mut pager, slst_page, 4, "vis00");

        assert!(matches!(
            open_retail_physical_eid(&mut pager, zdat).unwrap(),
            RetailPagingPublication::Open(PagerOpenOutcome {
                page,
                resolved: true,
                ..
            }) if page == zdat_page
        ));
        assert_eq!(
            stage_retail_visibility_list(&mut pager, slst, |_| Ok(())),
            Err(format!(
                "could not physically open SLST {slst}: {:?}",
                PagingError::NoFreePhysicalSlot(slst_page)
            ))
        );
        assert_eq!(
            pager
                .page_reference_counts()
                .find(|(page, _)| *page == zdat_page),
            Some((zdat_page, 1))
        );
        assert_eq!(
            pager
                .page_reference_counts()
                .find(|(page, _)| *page == slst_page),
            Some((slst_page, 0))
        );
    }

    #[test]
    fn title_zdat_open_can_span_slst_and_close_when_they_share_a_page() {
        let page = PageIndex::new(0);
        let zdat = eid("zone0");
        let slst = eid("vis00");
        let zdat_handle = EntryHandle::new(page, 4);
        let slst_handle = EntryHandle::new(page, 8);
        let mut pager = Pager::new();
        pager.set_physical_slot_count(1).unwrap();
        pager
            .register_page(page, [zdat_handle, slst_handle])
            .unwrap();
        pager.bind_eid(zdat, zdat_handle).unwrap();
        pager.bind_eid(slst, slst_handle).unwrap();

        let mut trace = Vec::new();
        stage_retail_level_update_physical_prefix(
            &mut pager,
            Some(zdat),
            Some(slst),
            |publication| {
                trace.push(publication);
                Ok(())
            },
        )
        .unwrap();
        stage_retail_level_update_physical_suffix(&mut pager, Some(zdat), |publication| {
            trace.push(publication);
            Ok(())
        })
        .unwrap();

        assert!(matches!(
            trace.as_slice(),
            [
                RetailPagingPublication::Open(_),
                RetailPagingPublication::Open(_),
                RetailPagingPublication::Close(_),
                RetailPagingPublication::Close(_),
            ]
        ));
        assert_eq!(pager.page_reference_counts().next(), Some((page, 0)));
    }

    #[test]
    fn level_update_drain_reports_replacement_before_clearing_pending_pages() {
        let old = PageIndex::new(0);
        let restored = PageIndex::new(1);
        let mut pager = Pager::new();
        pager.set_physical_slot_count(1).unwrap();
        pager.register_page(old, []).unwrap();
        pager.register_page(restored, []).unwrap();
        pager.materialize_page_with_outcome(old).unwrap();
        let queued = pager.open_page_virtual_with_outcome(restored).unwrap();
        assert!(!queued.resolved);

        let mut updates = Vec::new();
        drain_retail_level_update_pages(&mut pager, |outcome| {
            updates.push(RetailPagingPublication::SynchronousUpdate(outcome));
            Ok(())
        })
        .unwrap();

        assert_eq!(
            updates,
            [RetailPagingPublication::SynchronousUpdate(
                PagerUpdateOutcome::Resolved(PagerOpenOutcome {
                    page: restored,
                    resolved: true,
                    invalidated: PageInvalidations::one(old),
                    evicted: None,
                })
            )]
        );
        assert!(pager.pending_virtual_pages().next().is_none());
        assert_eq!(pager.resolved_pages().collect::<Vec<_>>(), [restored]);
    }
}
