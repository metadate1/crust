//! Opt-in characterization of the legally local Hog Wild player program.
//!
//! The test retains only structural assertions and never writes or prints
//! stream bytes, derived instruction words, or game assets.

use std::path::PathBuf;

use crust_formats::stream::{
    LevelId, StreamKind, StreamName, load_gool_state_program, parse_nsd, parse_nsf,
};

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn hog_wild_fall_kill_targets_authored_death_state() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let level = LevelId::new_const(0x11);
    let nsd_bytes = std::fs::read(root.join(StreamName::new(level, StreamKind::Nsd).filename()))
        .expect("read Hog Wild NSD");
    let nsf_bytes = std::fs::read(root.join(StreamName::new(level, StreamKind::Nsf).filename()))
        .expect("read Hog Wild NSF");
    let nsd = parse_nsd(&nsd_bytes, level).expect("parse Hog Wild NSD");
    let nsf = parse_nsf(&nsf_bytes, &nsd).expect("parse Hog Wild NSF");
    let player = nsd.ldat().expect("Hog Wild has LDAT").executable_map[0];
    let mounted = load_gool_state_program(&nsd, &nsf, &nsf_bytes, player, 37)
        .expect("load mounted player state 37");
    let death = load_gool_state_program(&nsd, &nsf, &nsf_bytes, player, 22)
        .expect("load fall-kill state 22");

    assert_eq!(mounted.event_map().get(9), Some(&22));
    assert_eq!(mounted.external_eid().name().as_deref(), Some("WilhC"));
    assert_eq!(death.external_eid().name().as_deref(), Some("WillC"));
    assert_eq!(death.state().flags, 0x4009);
    assert!(mounted.code_pc().is_some());
    assert!(mounted.event_pc().is_some());
    assert!(mounted.transition_pc().is_some());
    assert!(death.code_pc().is_some());
    assert!(death.transition_pc().is_some());
    assert!(death.event_pc().is_none());
}
