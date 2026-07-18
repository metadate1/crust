//! Opt-in retail CD paging cadence checks against the user's own streams.
//!
//! This test reads the legally local Native Fortress pair in place. It never
//! copies game data into the repository.
//!
//! Independent oracle: PCSX-Redux instrumentation booted the user's legally
//! owned NTSC-U BIN directly and sampled the retail PTEs once per coherent
//! game tick while `WillC` waited at external PC 2695. Page 23 remained tagged
//! through tick 14 and published at tick 15; page 24 published at tick 21 as
//! `WillC` advanced to PC 2700. PCSX-Redux is a test oracle only, not a Crust
//! runtime dependency.

use std::path::PathBuf;

use crust_formats::binary::PageIndex;
use crust_formats::stream::{
    LevelId, NsfPageSectorCount, StreamKind, StreamName, parse_nsd, parse_nsf,
};
use crust_sim::paging::{PageState, Pager, PagerUpdateOutcome};

fn resolved_page(update: Option<PagerUpdateOutcome>) -> PageIndex {
    match update {
        Some(PagerUpdateOutcome::Resolved(outcome)) => outcome.page,
        other => panic!("expected one page publication, got {other:?}"),
    }
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn native_fortress_publishes_willc_pages_on_retail_cd_ticks() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let level = LevelId::new_const(0x1a);
    let nsd_bytes =
        std::fs::read(root.join(StreamName::new(level, StreamKind::Nsd).filename())).unwrap();
    let nsf_bytes =
        std::fs::read(root.join(StreamName::new(level, StreamKind::Nsf).filename())).unwrap();
    let nsd = parse_nsd(&nsd_bytes, level).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
    let page_23 = PageIndex::new(23);
    let page_24 = PageIndex::new(24);

    assert_eq!(
        nsd.header
            .page_sector_count(page_23)
            .map(NsfPageSectorCount::get),
        Some(20)
    );
    assert_eq!(
        nsd.header
            .page_sector_count(page_24)
            .map(NsfPageSectorCount::get),
        Some(31)
    );

    let mut pager = Pager::from_stream(&nsd, &nsf).unwrap();
    // WillC requests the V and G pages in this order. Reverse the host calls
    // deliberately to prove that NSUpdate follows source pgid order rather
    // than insertion order.
    pager.open_page_virtual_with_outcome(page_24).unwrap();
    pager.open_page_virtual_with_outcome(page_23).unwrap();
    assert_eq!(pager.page(page_23).unwrap().state, PageState::Queued);
    assert_eq!(pager.page(page_24).unwrap().state, PageState::Queued);

    // The first call is the source clone/seek-start NSUpdate. It reserves the
    // physical group but does not consume a transfer frame, so these API calls
    // map directly to the oracle's still-tagged ticks 1 through 14.
    for update_tick in 1..=14 {
        assert_eq!(
            pager.update_pending_virtual_page().unwrap(),
            None,
            "page 23 published before PCSX oracle tick {update_tick}"
        );
        assert_eq!(pager.page(page_23).unwrap().state, PageState::Queued);
        assert_eq!(pager.page(page_24).unwrap().state, PageState::Queued);
    }
    assert_eq!(
        resolved_page(pager.update_pending_virtual_page().unwrap()),
        page_23,
        "page 23 must publish on PCSX oracle tick 15"
    );
    assert_eq!(pager.page(page_23).unwrap().state, PageState::Translated);
    assert_eq!(pager.page(page_24).unwrap().state, PageState::Queued);

    for update_tick in 16..=20 {
        assert_eq!(
            pager.update_pending_virtual_page().unwrap(),
            None,
            "page 24 published before PCSX oracle tick {update_tick}"
        );
        assert_eq!(pager.page(page_24).unwrap().state, PageState::Queued);
    }
    assert_eq!(
        resolved_page(pager.update_pending_virtual_page().unwrap()),
        page_24,
        "page 24 must publish on PCSX oracle tick 21"
    );
    assert_eq!(pager.page(page_24).unwrap().state, PageState::Translated);
    assert!(pager.pending_virtual_pages().next().is_none());
}
