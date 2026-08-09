//! Opt-in characterization of direct-bonus completion against a legally owned disc.
//!
//! The assertions retain only structural metadata and never write, print, or
//! commit stream bytes or proprietary assets.

use std::path::PathBuf;

use crust_formats::{
    binary::Eid,
    disc::DiscImage,
    stream::{
        LevelId, RetailZoneGraph, StreamKind, StreamName, load_gool_state_program, parse_nsd,
        parse_nsf,
    },
};

fn is_load_state_misc(word: u32) -> bool {
    let opcode = (word >> 24) as u8;
    let primary = ((word >> 20) & 0x0f) as u8;
    let secondary = ((((word >> 15) & 0x1f) as i8) << 3) >> 3;
    opcode == 0x1c && primary == 12 && secondary == 1
}

#[test]
#[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
fn every_direct_bonus_uses_the_restricted_willc_completion_boundary() {
    let path = PathBuf::from(
        std::env::var_os("C1_DISC_IMAGE")
            .expect("C1_DISC_IMAGE must name a legally local NTSC-U raw BIN"),
    );
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()));
    let disc = DiscImage::open(&bytes)
        .unwrap_or_else(|error| panic!("could not open {}: {error}", path.display()));
    let streams = disc
        .discover_streams()
        .expect("the legally local disc stream catalog must parse");
    let will = Eid::from_name("WillC").expect("fixed retail player EID is valid");

    for bonus in [0x24, 0x25, 0x26, 0x33, 0x34].map(LevelId::new_const) {
        let nsd_name = StreamName::new(bonus, StreamKind::Nsd);
        let nsf_name = StreamName::new(bonus, StreamKind::Nsf);
        let nsd_bytes = disc
            .read_stream(streams.get(nsd_name).expect("bonus NSD is present"))
            .expect("bonus NSD must extract");
        let nsf_bytes = disc
            .read_stream(streams.get(nsf_name).expect("bonus NSF is present"))
            .expect("bonus NSF must extract");
        let nsd = parse_nsd(&nsd_bytes, bonus).expect("bonus NSD must parse");
        let nsf = parse_nsf(&nsf_bytes, &nsd).expect("bonus NSF must parse");
        let graph = RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes)
            .expect("bonus zone graph must parse");
        let spawn = graph
            .zone(graph.spawn_path().zone)
            .expect("bonus spawn zone must be reachable");
        assert_eq!(spawn.graphics_flags, 0x2002, "bonus LID {bonus}");

        let death = load_gool_state_program(&nsd, &nsf, &nsf_bytes, will, 22)
            .unwrap_or_else(|error| panic!("bonus LID {bonus} WillC state 22: {error}"));
        if bonus == LevelId::new_const(0x26) {
            assert!(
                load_gool_state_program(&nsd, &nsf, &nsf_bytes, will, 32).is_err(),
                "dormant bonus 0x26 must not invent the absent authored completion entry"
            );
            continue;
        }
        let completion = load_gool_state_program(&nsd, &nsf, &nsf_bytes, will, 32)
            .unwrap_or_else(|error| panic!("bonus LID {bonus} WillC state 32: {error}"));
        assert_eq!(completion.global_eid(), will, "bonus LID {bonus}");
        assert_eq!(completion.state_index(), 32, "bonus LID {bonus}");
        assert_eq!(death.state_index(), 22, "bonus LID {bonus}");
        assert_eq!(
            completion.event_map().get(22),
            Some(&32),
            "bonus LID {bonus}"
        );
        assert_eq!(death.event_map().get(9), Some(&22), "bonus LID {bonus}");
        assert!(
            completion.code().iter().copied().any(is_load_state_misc),
            "bonus LID {bonus} WillC state table must retain misc 12/1"
        );
    }
}
