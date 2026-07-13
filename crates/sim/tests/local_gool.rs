//! Opt-in binding check against GOOL entries on a legally local disc.

use std::path::PathBuf;

use crust_formats::binary::Eid;
use crust_formats::disc::DiscImage;
use crust_formats::stream::{
    LevelId, NsdKind, StreamKind, StreamName, load_gool_program, parse_nsd, parse_nsf,
};
use crust_sim::gool::{
    CodeAddress, CodeSegment, Execution, HaltReason, Machine, ObjectHandle, VmEffect, VmObject,
};

#[test]
#[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
fn binds_every_mounted_title_executable_to_a_vm_object() {
    let disc_path = PathBuf::from(
        std::env::var_os("C1_DISC_IMAGE")
            .expect("C1_DISC_IMAGE must name a legally local NTSC-U raw BIN"),
    );
    let disc_bytes = std::fs::read(&disc_path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", disc_path.display()));
    let image = DiscImage::open(&disc_bytes).unwrap();
    let streams = image.discover_streams().unwrap();
    let nsd_bytes = image
        .read_stream(
            streams
                .get(StreamName::new(LevelId::TITLE, StreamKind::Nsd))
                .unwrap(),
        )
        .unwrap();
    let nsf_bytes = image
        .read_stream(
            streams
                .get(StreamName::new(LevelId::TITLE, StreamKind::Nsf))
                .unwrap(),
        )
        .unwrap();
    let metadata = parse_nsd(&nsd_bytes, LevelId::TITLE).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &metadata).unwrap();
    let NsdKind::Playable(ldat) = &metadata.kind else {
        panic!("title stream must have playable LDAT metadata");
    };

    let mut object_index = 0_u16;
    for eid in ldat.executable_map {
        if eid == Eid::NONE {
            continue;
        }
        let program = (0..=u8::MAX)
            .find_map(|subtype| {
                load_gool_program(&metadata, &nsf, &nsf_bytes, eid, u16::from(subtype)).ok()
            })
            .unwrap_or_else(|| panic!("mounted title executable {eid} has no valid subtype"));
        let handle = ObjectHandle::new(object_index).unwrap();
        VmObject::from_gool_program(handle, &program)
            .unwrap_or_else(|error| panic!("could not bind title executable {eid}: {error:?}"));
        object_index += 1;
    }
    assert!(object_index > 0);
}

#[test]
#[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
fn n_sanity_crash_uses_absolute_shared_code_addressing() {
    let disc_path = PathBuf::from(
        std::env::var_os("C1_DISC_IMAGE")
            .expect("C1_DISC_IMAGE must name a legally local NTSC-U raw BIN"),
    );
    let disc_bytes = std::fs::read(&disc_path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", disc_path.display()));
    let image = DiscImage::open(&disc_bytes).unwrap();
    let streams = image.discover_streams().unwrap();
    let level = LevelId::N_SANITY_BEACH;
    let nsd_bytes = image
        .read_stream(
            streams
                .get(StreamName::new(level, StreamKind::Nsd))
                .unwrap(),
        )
        .unwrap();
    let nsf_bytes = image
        .read_stream(
            streams
                .get(StreamName::new(level, StreamKind::Nsf))
                .unwrap(),
        )
        .unwrap();
    let metadata = parse_nsd(&nsd_bytes, level).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &metadata).unwrap();
    let crash_eid = metadata.ldat().unwrap().executable_map[0];
    let program = load_gool_program(&metadata, &nsf, &nsf_bytes, crash_eid, 0).unwrap();
    assert_eq!(program.code()[70], 0x8609_806e);
    assert_eq!(program.global_code()[131], 0x8289_4000);
    assert_eq!(program.code()[72], 0x16be_0e1f);

    let handle = ObjectHandle::new(0).unwrap();
    let mut object = VmObject::from_gool_program(handle, &program).unwrap();
    object.restart(70).unwrap();
    let mut machine = Machine::new(0);
    machine.insert_object(object).unwrap();
    machine.run(handle, 1).unwrap();
    assert_eq!(
        machine.object(handle).unwrap().code_address(),
        CodeAddress {
            segment: CodeSegment::Global,
            pc: 110,
        }
    );

    let boot_handle = ObjectHandle::new(1).unwrap();
    let boot_object = VmObject::from_gool_program(boot_handle, &program).unwrap();
    let mut boot_machine = Machine::new(256);
    boot_machine.insert_object(boot_object).unwrap();
    assert_eq!(
        boot_machine.run(boot_handle, 256).unwrap(),
        Execution {
            reason: HaltReason::Yielded,
            steps: 64,
        }
    );
    assert_eq!(
        boot_machine.object(boot_handle).unwrap().code_address(),
        CodeAddress {
            segment: CodeSegment::External,
            pc: 96,
        }
    );
    assert!(boot_machine.object(boot_handle).unwrap().stack().is_empty());
    assert_eq!(
        boot_machine.effects(),
        &[VmEffect::SpawnChildren {
            parent: boot_handle,
            executable: 5,
            subtype: 0,
            count: 1,
            alternate_parent: false,
            arguments: vec![0],
        }]
    );
    let _ = boot_machine.take_effects();
    assert_eq!(
        boot_machine.run(boot_handle, 256).unwrap(),
        Execution {
            reason: HaltReason::Yielded,
            steps: 3,
        }
    );
    assert_eq!(
        boot_machine.object(boot_handle).unwrap().code_address(),
        CodeAddress {
            segment: CodeSegment::External,
            pc: 99,
        }
    );
    assert!(boot_machine.object(boot_handle).unwrap().stack().is_empty());
    assert_eq!(
        boot_machine.effects(),
        &[VmEffect::SpawnChildren {
            parent: boot_handle,
            executable: 29,
            subtype: 0,
            count: 1,
            alternate_parent: false,
            arguments: vec![0, 4096, 0],
        }]
    );
}
