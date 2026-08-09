//! Opt-in characterization of Stormy Ascent's unreachable Cortex-token exit.
//!
//! The test reads a legally owned disc locally and retains only structural
//! metadata and exact GOOL instruction words. It never writes or prints game
//! data.

use std::path::PathBuf;

use crust_formats::{
    binary::Eid,
    disc::DiscImage,
    stream::{
        LevelId, RetailZoneGraph, StreamKind, StreamName, ZoneEntity, ZoneHeader,
        load_gool_state_program, parse_nsd, parse_nsf,
    },
};

#[test]
#[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
fn stormy_cortex_tokens_retain_the_authored_missing_destination_fallthrough() {
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
    let stormy = LevelId::new_const(0x22);
    let nsd_bytes = disc
        .read_stream(
            streams
                .get(StreamName::new(stormy, StreamKind::Nsd))
                .expect("Stormy NSD is present"),
        )
        .expect("Stormy NSD must extract");
    let nsf_bytes = disc
        .read_stream(
            streams
                .get(StreamName::new(stormy, StreamKind::Nsf))
                .expect("Stormy NSF is present"),
        )
        .expect("Stormy NSF must extract");
    let nsd = parse_nsd(&nsd_bytes, stormy).expect("Stormy NSD must parse");
    let nsf = parse_nsf(&nsf_bytes, &nsd).expect("Stormy NSF must parse");

    let boxs = Eid::from_name("BoxsC").expect("fixed retail box EID is valid");
    assert_eq!(nsd.ldat().unwrap().executable_map[0x22], boxs);
    let graph =
        RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes).expect("Stormy zone graph must parse");
    let mut tokens = Vec::new();
    for zone in graph.zones() {
        let entry = nsf
            .resolve_entry(&nsd, zone.eid)
            .expect("Stormy ZDAT must resolve");
        let header = ZoneHeader::parse(
            entry
                .item(0)
                .expect("Stormy ZDAT header item is present")
                .bytes(&nsf_bytes)
                .expect("Stormy ZDAT header bytes are bounded"),
        )
        .expect("Stormy ZDAT header must parse");
        for entity_index in 0..header.entity_count {
            let item = header
                .entity_item_index(entity_index)
                .expect("Stormy entity index must be bounded") as usize;
            let entity = ZoneEntity::parse(
                entry
                    .item(item)
                    .expect("Stormy entity item is present")
                    .bytes(&nsf_bytes)
                    .expect("Stormy entity bytes are bounded"),
            )
            .expect("Stormy entity must parse");
            if entity.group == 3
                && entity.executable == 0x22
                && entity.subtype == 10
                && entity.initializer.as_slice() == [0x67, 0, 0]
            {
                tokens.push((zone.eid, entity.id, entity.spawn_flags));
            }
        }
    }
    tokens.sort_unstable_by_key(|(_, id, _)| *id);
    assert_eq!(
        tokens,
        [
            (Eid::from_name("a4_yZ").unwrap(), 29, 0x1999),
            (Eid::from_name("o1_yZ").unwrap(), 60, 0x19),
            (Eid::from_name("j3_yZ").unwrap(), 81, 0x1999),
        ],
        "Stormy has exactly three serialized Cortex-token boxes"
    );

    let dispc = Eid::from_name("DispC").expect("fixed retail pickup EID is valid");
    let state = load_gool_state_program(&nsd, &nsf, &nsf_bytes, dispc, 13)
        .expect("Stormy DispC state 13 must parse");
    assert_eq!(state.code_pc(), Some(521));
    assert_eq!(
        &state.global_code()[480..495],
        &[
            0x0486_7b03,
            0x8227_c00d,
            0x1fbe_0800,
            0x16be_0e1f,
            0x0482_3b04, // LID 0x23
            0x8227_c003,
            0x2001_d83c,
            0x1183_4e4e, // destination 0x34
            0x8209_4004,
            0x0481_db04, // LID 0x1d
            0x8227_c002,
            0x2001_e83c,
            0x1183_4e4e, // destination 0x34
            0x8209_4400,
            0x8209_4018, // all other LIDs fall through without a write
        ]
    );
    assert_eq!(
        &state.global_code()[559..568],
        &[
            0x0480_3e1f, // three tokens
            0x8227_c007,
            0x1cc2_dbe0,
            0x84cf_0e49,
            0x1180_0e1f,
            0x1c40_5e4a, // clear Cortex counter
            0x16be_0805, // completion argument 0x500
            0x87a4_080f, // direct event 0xf00 to the player
            0x1cc4_de4e, // LLEV from destination register 78
        ]
    );
}

#[test]
#[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
fn lights_out_tawna_selector_is_authored_but_has_no_retail_token_sources() {
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
    let lights_out = LevelId::new_const(0x28);
    let nsd_bytes = disc
        .read_stream(
            streams
                .get(StreamName::new(lights_out, StreamKind::Nsd))
                .expect("Lights Out NSD is present"),
        )
        .expect("Lights Out NSD must extract");
    let nsf_bytes = disc
        .read_stream(
            streams
                .get(StreamName::new(lights_out, StreamKind::Nsf))
                .expect("Lights Out NSF is present"),
        )
        .expect("Lights Out NSF must extract");
    let nsd = parse_nsd(&nsd_bytes, lights_out).expect("Lights Out NSD must parse");
    let nsf = parse_nsf(&nsf_bytes, &nsd).expect("Lights Out NSF must parse");

    let boxs = Eid::from_name("BoxsC").expect("fixed retail box EID is valid");
    assert_eq!(nsd.ldat().unwrap().executable_map[0x22], boxs);
    let graph = RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes)
        .expect("Lights Out zone graph must parse");
    let mut tawna_tokens = Vec::new();
    for zone in graph.zones() {
        let entry = nsf
            .resolve_entry(&nsd, zone.eid)
            .expect("Lights Out ZDAT must resolve");
        let header = ZoneHeader::parse(
            entry
                .item(0)
                .expect("Lights Out ZDAT header item is present")
                .bytes(&nsf_bytes)
                .expect("Lights Out ZDAT header bytes are bounded"),
        )
        .expect("Lights Out ZDAT header must parse");
        for entity_index in 0..header.entity_count {
            let item = header
                .entity_item_index(entity_index)
                .expect("Lights Out entity index must be bounded") as usize;
            let entity = ZoneEntity::parse(
                entry
                    .item(item)
                    .expect("Lights Out entity item is present")
                    .bytes(&nsf_bytes)
                    .expect("Lights Out entity bytes are bounded"),
            )
            .expect("Lights Out entity must parse");
            if entity.group == 3
                && entity.executable == 0x22
                && entity.subtype == 10
                && entity.initializer.as_slice() == [0x69, 0, 0]
            {
                tawna_tokens.push((zone.eid, entity.id, entity.spawn_flags));
            }
        }
    }
    assert!(
        tawna_tokens.is_empty(),
        "Lights Out's retail zones must not grow invented Tawna-token sources"
    );

    // DispC nevertheless retains a dead retail selector branch for Lights
    // Out: parent LID 0x28 would choose Bonus 2 (0x33) with layout selector
    // 16. With no 0x69 crates above, normal gameplay cannot reach it.
    let dispc = Eid::from_name("DispC").expect("fixed retail pickup EID is valid");
    let state = load_gool_state_program(&nsd, &nsf, &nsf_bytes, dispc, 13)
        .expect("Lights Out DispC state 13 must parse");
    assert_eq!(state.code_pc(), Some(521));
    assert_eq!(
        &state.global_code()[461..466],
        &[
            0x0482_8b04, // parent LID 0x28
            0x8227_c003,
            0x20a0_183c, // internal constant 16 -> global 60
            0x1183_3e4e, // destination LID 0x33
            0x8209_400c,
        ]
    );
}
