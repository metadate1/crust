//! Opt-in binding check against GOOL entries on a legally local disc.

use std::path::PathBuf;

use crust_formats::binary::Eid;
use crust_formats::disc::DiscImage;
use crust_formats::stream::{
    LevelId, NsdKind, StreamKind, StreamName, load_gool_program, parse_nsd, parse_nsf,
    structs::GoolState,
};
use crust_sim::gool::{
    AnimationReference, CodeAddress, CodeSegment, Execution, HaltReason, Machine, ObjectHandle,
    VmEffect, VmObject, VmStateProgram,
};

fn words(bytes: &[u8]) -> Vec<u32> {
    assert!(bytes.len().is_multiple_of(4));
    bytes
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
        .collect()
}

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
        boot_machine
            .run_with_host_effects(boot_handle, 67, |_machine, effect| {
                assert!(matches!(effect, VmEffect::SpawnChildren { .. }));
                Ok(())
            })
            .unwrap(),
        Execution {
            reason: HaltReason::BudgetExhausted,
            steps: 67,
        }
    );
    assert_eq!(
        boot_machine.object(boot_handle).unwrap().code_address(),
        CodeAddress {
            segment: CodeSegment::External,
            pc: 99,
        }
    );
    let boot_stack = boot_machine.object(boot_handle).unwrap().stack();
    assert_eq!(boot_stack.len(), 3);
    assert_eq!(boot_stack[0], 0xffff);
    assert_eq!(boot_stack[1] & 0xff00_0000, 0xa600_0000);
    assert_eq!(boot_stack[2], program.header().initial_stack_pointer * 4);
    assert_eq!(
        boot_machine.effects(),
        &[
            VmEffect::SpawnChildren {
                parent: boot_handle,
                executable: 5,
                subtype: 0,
                count: 1,
                allow_reclaim: false,
                arguments: vec![0],
            },
            VmEffect::SpawnChildren {
                parent: boot_handle,
                executable: 29,
                subtype: 0,
                count: 1,
                allow_reclaim: false,
                arguments: vec![0, 4096, 0],
            },
        ]
    );

    // The second retail child is ShadC. Its first instruction reads the
    // third spawn argument through fp[-1]; that init block also contains the
    // packed writes for the final three halfwords of its color matrix.
    let shadow_eid = metadata.ldat().unwrap().executable_map[29];
    let shadow_program = load_gool_program(&metadata, &nsf, &nsf_bytes, shadow_eid, 0).unwrap();
    assert_eq!(shadow_program.code()[0], 0x11b7_fe4d);
    assert_eq!(shadow_program.code()[17], 0x240a_8802);
    let shadow_handle = ObjectHandle::new(2).unwrap();
    let mut shadow = VmObject::from_gool_program(shadow_handle, &shadow_program).unwrap();
    shadow.initialize_arguments(&[0, 4096, 0]).unwrap();
    shadow.set_link(1, Some(boot_handle)).unwrap();
    shadow.set_link(4, Some(boot_handle)).unwrap();
    shadow.set_link(5, Some(boot_handle)).unwrap();
    let shadow_global = nsf.resolve_entry(&metadata, shadow_eid).unwrap();
    let animation_data = shadow_global.item(5).unwrap().bytes(&nsf_bytes).unwrap();
    shadow.bind_animation_data(animation_data);
    boot_machine.insert_object(shadow).unwrap();
    assert_eq!(
        boot_machine.run(shadow_handle, 64).unwrap().reason,
        HaltReason::StateChanged(1)
    );
    assert_eq!(
        boot_machine.object(shadow_handle).unwrap().register(0x4d),
        Ok(0)
    );

    let states = shadow_global.item(4).unwrap().bytes(&nsf_bytes).unwrap();
    let state_one = GoolState::parse(&states[16..32]).unwrap();
    let external_eid =
        Eid::from_raw(shadow_program.internal_words()[usize::from(state_one.external_index)]);
    let external = nsf.resolve_entry(&metadata, external_eid).unwrap();
    let state_one_program = VmStateProgram::new(
        1,
        state_one,
        words(external.item(1).unwrap().bytes(&nsf_bytes).unwrap()),
        words(external.item(2).unwrap().bytes(&nsf_bytes).unwrap()),
    )
    .unwrap();
    assert_eq!(state_one.code_pc, 22);
    boot_machine
        .rebind_state_program(shadow_handle, &state_one_program, &[])
        .unwrap();
    boot_machine.set_frames_elapsed(1);
    let execution = boot_machine.run(shadow_handle, 64).unwrap();
    assert_eq!(
        execution.reason,
        HaltReason::AnimationChanged { frame: 0, wait: 1 }
    );
    assert!(execution.steps > 2);
    let animation_word = boot_machine
        .object(shadow_handle)
        .unwrap()
        .register(0x2a)
        .unwrap();
    let animation = AnimationReference::from_word(animation_word).unwrap();
    assert_eq!(animation.offset(), 0);
    assert_eq!(
        boot_machine
            .object(shadow_handle)
            .unwrap()
            .animation_data(animation)
            .unwrap(),
        animation_data
    );
    assert!(boot_machine.effects().iter().any(|effect| matches!(
        effect,
        VmEffect::AnimationFrameChanged {
            object,
            frame: 0,
            ..
        } if *object == shadow_handle
    )));
    assert_eq!(
        boot_machine.run(shadow_handle, 64).unwrap(),
        Execution {
            reason: HaltReason::AnimationWaiting { remaining: 1 },
            steps: 0,
        }
    );
    boot_machine.set_frames_elapsed(2);
    let resumed = boot_machine.run(shadow_handle, 64).unwrap();
    assert!(matches!(
        resumed.reason,
        HaltReason::AnimationChanged { frame: 0, .. }
    ));
    assert!(resumed.steps > 0);
}
