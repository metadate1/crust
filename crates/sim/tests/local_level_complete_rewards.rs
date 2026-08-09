//! Legally local characterization of retail's gem-award handshake.
//!
//! Gameplay `WillC` publishes the completed LID and destroyed-box count in
//! globals 71 and 70. Level Complete's `GamOC` then compares that pair against
//! its authored per-level box table and sets the map-node inventory bit. The
//! assertions below keep that data-authored contract exact without embedding
//! or writing any proprietary stream bytes.

use std::{fs, path::PathBuf};

use crust_formats::{
    binary::Eid,
    stream::{
        GoolProgram, LevelId, StreamKind, StreamName, load_gool_program, parse_nsd, parse_nsf,
    },
};

fn load_program(level: LevelId, eid: Eid) -> GoolProgram {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let nsd_name = StreamName::new(level, StreamKind::Nsd);
    let nsf_name = StreamName::new(level, StreamKind::Nsf);
    let nsd_bytes = fs::read(root.join(nsd_name.filename())).expect("local NSD must be readable");
    let nsf_bytes = fs::read(root.join(nsf_name.filename())).expect("local NSF must be readable");
    let nsd = parse_nsd(&nsd_bytes, level).expect("local NSD must parse");
    let nsf = parse_nsf(&nsf_bytes, &nsd).expect("local NSF must parse");

    (0..=u16::from(u8::MAX))
        .find_map(|subtype| load_gool_program(&nsd, &nsf, &nsf_bytes, eid, subtype).ok())
        .unwrap_or_else(|| panic!("{eid} must expose at least one executable subtype in {level}"))
}

fn immediate_operand(value: u8) -> u16 {
    0x0800 | u16::from(value)
}

fn compare_immediate_to_frame_three(value: u8) -> u32 {
    (0x04_u32 << 24) | (u32::from(immediate_operand(value)) << 12) | 0x0b03
}

fn move_immediate_to_register_69(value: u8) -> u32 {
    (0x11_u32 << 24) | (u32::from(immediate_operand(value)) << 12) | 0x0e45
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn gameplay_level_end_publishes_the_retail_gem_award_handoff() {
    let will = Eid::from_name("WillC").expect("fixed WillC EID is valid");
    let gem_levels = [
        0x09, 0x0c, 0x12, 0x0e, 0x0f, 0x15, 0x11, 0x1a, 0x18, 0x20, 0x1c, 0x14, 0x13, 0x23, 0x1e,
        0x06, 0x03, 0x05, 0x07, 0x16, 0x2e, 0x28, 0x1d, 0x37, 0x29, 0x2a,
    ];

    // The executable is shared, but resolve it through every legal parent
    // pair so a missing or mismatched page-table binding cannot hide behind a
    // single representative stream.
    for lid in gem_levels {
        let level = LevelId::new_const(lid);
        let program = load_program(level, will);
        let code = program.global_code();

        assert_eq!(
            &code[2525..=2532],
            &[
                0x1fbe_086c, // death count
                0x1fbe_081e, // current-zone flags
                0x0782_0e1f, // colored-gem no-death/route gate
                0x1fbe_0800, // mounted LID
                0x0482_ce1f, // The Great Hall is not a gem parent
                0x06e1_fe1f,
                0x06e1_fe1f,
                0x8227_c02b,
            ],
            "{level} must retain the authored reward eligibility gate"
        );
        assert_eq!(
            &code[2578..=2600],
            &[
                0x1fbe_0814,
                0x0ca0_2e1f,
                0x8227_c007,
                0x1fbe_0848,
                0x1fbe_0814,
                0x01a0_2e1f,
                0x15e1_f05c,
                0x07e1_fe1f,
                0x11e1_fe25,
                0x8209_4005,
                0x1fbe_083f,
                0x1fbe_0814,
                0x15e1_f05c,
                0x07e1_fe1f,
                0x11e1_fe25,
                0x12e2_5e1f,
                0x8227_c009,
                0x16be_0805,
                0x8619_8000,
                0x1fbe_0800,
                0x20e1_f847, // global 71 = completed LID
                0x1fbe_083e, // read destroyed-box count
                0x20e1_f846, // global 70 = destroyed-box count
            ],
            "{level} must publish the exact Level Complete handoff"
        );
    }
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn level_complete_owns_all_authored_box_totals_and_inventory_bit_writes() {
    let level = LevelId::LEVEL_COMPLETE;
    let gamoc = Eid::from_name("GamOC").expect("fixed GamOC EID is valid");
    let program = load_program(level, gamoc);
    let code = program.global_code();

    // Twenty-six obtainable gems plus Stormy Ascent's authored-but-cut entry.
    // Values are retail crate totals, not a host-side completion shortcut.
    let box_totals = [
        (0x09, 49),
        (0x0c, 38),
        (0x12, 26),
        (0x11, 24),
        (0x1e, 24),
        (0x1a, 42),
        (0x0f, 14),
        (0x15, 46),
        (0x0e, 16),
        (0x13, 35),
        (0x18, 15),
        (0x1c, 67),
        (0x20, 26),
        (0x23, 50),
        (0x1d, 65),
        (0x03, 41),
        (0x06, 33),
        (0x05, 44),
        (0x07, 26),
        (0x14, 33),
        (0x16, 18),
        (0x28, 15),
        (0x22, 34),
        (0x2a, 18),
        (0x2e, 31),
        (0x37, 24),
        (0x29, 24),
    ];
    assert_eq!(code[2382], 0x1fbe_0847, "GamOC must read completed LID");
    for (index, (lid, boxes)) in box_totals.into_iter().enumerate() {
        let pc = 2384 + index * 4;
        assert_eq!(
            code[pc],
            compare_immediate_to_frame_three(lid),
            "box-table row {index} must select LID {lid:#04x}"
        );
        assert_eq!(
            code[pc + 2],
            move_immediate_to_register_69(boxes),
            "box-table row {index} must require {boxes} crates"
        );
    }
    assert_eq!(
        code[2492], 0x1fbe_0846,
        "GamOC must read the destroyed-box total"
    );
    assert_eq!(
        code[2493], 0x04e4_5e1f,
        "GamOC must compare it with the table row"
    );

    // The award is derived from current-map ordinal: pool one owns ordinals
    // 0..=32, while later secret-level ordinals use pool two. Both branches
    // OR a single bit and preserve every previously earned item.
    assert_eq!(
        &code[2646..=2657],
        &[
            0x0ca0_2e3d,
            0x8227_c006,
            0x1fbe_0848,
            0x01a0_2e3d,
            0x15e1_f095,
            0x08e1_fe1f,
            0x20e1_f848,
            0x8209_4004,
            0x1fbe_083f,
            0x15e3_d095,
            0x08e1_fe1f,
            0x20e1_f83f,
        ]
    );
}
