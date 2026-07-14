//! Opt-in static instruction census over legally local retail GOOL programs.
//!
//! This never writes program bytes. It reports only selector sets and counts,
//! which make dormant interpreter branches auditable without requiring a
//! particular playthrough to reach every state.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use crust_formats::{
    binary::Eid,
    stream::{
        KNOWN_LEVELS, NsdKind, StreamKind, StreamName, load_gool_program, load_gool_state_program,
        parse_nsd, parse_nsf,
    },
};

fn record_words(
    words: &[u32],
    opcodes: &mut BTreeMap<u8, usize>,
    misc: &mut BTreeSet<(u8, i8)>,
    transform: &mut BTreeSet<u8>,
    solid: &mut BTreeSet<u8>,
) {
    for word in words {
        let opcode = (word >> 24) as u8;
        *opcodes.entry(opcode).or_default() += 1;
        if opcode == 0x1c {
            let primary = ((word >> 20) & 0x0f) as u8;
            let secondary = (((word >> 15) & 0x1f) as i8) << 3 >> 3;
            misc.insert((primary, secondary));
        } else if opcode == 0x85 {
            transform.insert(((word >> 18) & 7) as u8);
        } else if opcode == 0x8e {
            solid.insert(((word >> 18) & 7) as u8);
        }
    }
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn census_every_resolvable_retail_state_program() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let mut opcodes = BTreeMap::new();
    let mut misc = BTreeSet::new();
    let mut transform = BTreeSet::new();
    let mut solid = BTreeSet::new();
    let mut global_programs = 0_usize;
    let mut state_programs = 0_usize;

    for known in KNOWN_LEVELS {
        let nsd_bytes =
            std::fs::read(root.join(StreamName::new(known.id, StreamKind::Nsd).filename()))
                .unwrap();
        let nsf_bytes =
            std::fs::read(root.join(StreamName::new(known.id, StreamKind::Nsf).filename()))
                .unwrap();
        let nsd = parse_nsd(&nsd_bytes, known.id).unwrap();
        let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
        let NsdKind::Playable(ldat) = &nsd.kind else {
            continue;
        };
        let mut seen = BTreeSet::new();
        for global_eid in ldat.executable_map {
            if global_eid == Eid::NONE || !seen.insert(global_eid) {
                continue;
            }
            let Some(seed) = (0..=u8::MAX).find_map(|subtype| {
                load_gool_program(&nsd, &nsf, &nsf_bytes, global_eid, u16::from(subtype)).ok()
            }) else {
                continue;
            };
            global_programs += 1;
            record_words(
                seed.global_code(),
                &mut opcodes,
                &mut misc,
                &mut transform,
                &mut solid,
            );
            for state in 0..seed.states().len() {
                let state_index = u16::try_from(state).expect("retail state table fits u16");
                let Ok(state) =
                    load_gool_state_program(&nsd, &nsf, &nsf_bytes, global_eid, state_index)
                else {
                    continue;
                };
                state_programs += 1;
                record_words(
                    state.code(),
                    &mut opcodes,
                    &mut misc,
                    &mut transform,
                    &mut solid,
                );
            }
        }
    }

    eprintln!("global programs: {global_programs}; state programs: {state_programs}");
    eprintln!("opcodes: {opcodes:?}");
    eprintln!("misc selectors: {misc:?}");
    eprintln!("transform-vector selectors: {transform:?}");
    eprintln!("solid selectors: {solid:?}");
    assert!(global_programs > 400);
    assert!(state_programs > 7_000);
    assert!(opcodes.contains_key(&0x8e));
}
