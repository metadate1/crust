//! Opt-in binding check against GOOL entries on a legally local disc.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crust_formats::binary::Eid;
use crust_formats::disc::DiscImage;
use crust_formats::stream::{
    GoolAnimationDescriptor, GoolAnimationKind, KNOWN_LEVELS, LevelId, NsdKind, StreamKind,
    StreamName, load_gool_program, load_gool_state_program, load_object_model_frame,
    parse_gool_animation_descriptor, parse_nsd, parse_nsf, structs::GoolState,
};
use crust_sim::gool::{
    AnimationReference, AnimationSource, CodeAddress, CodeSegment, Execution, HaltReason, Machine,
    ObjectHandle, ProcessAnimationKind, REGISTER_COUNT, StorageReference, StorageRegion, VmEffect,
    VmObject, VmStateProgram, process_register,
};

fn words(bytes: &[u8]) -> Vec<u32> {
    assert!(bytes.len().is_multiple_of(4));
    bytes
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
        .collect()
}

fn collect_direct_animation_leas(code: &[u32], sources: &mut BTreeSet<(u16, u16)>) {
    for word in code {
        let instruction = crust_sim::gool::Instruction::decode(*word);
        let destination = instruction.operand_b;
        let writes_animation_sequence =
            destination == 0x0e2a || (destination < 0x0200 && destination & 0x003f == 42);
        if instruction.opcode == 0x14 && writes_animation_sequence {
            sources.insert((instruction.operand_a, destination));
        }
    }
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn every_retail_direct_animation_lea_has_the_characterized_safe_source_kind() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let mut source_count = 0_usize;
    let mut static_header_types = BTreeMap::new();
    let mut static_program_names = BTreeSet::new();
    let mut dynamic_sources = BTreeSet::new();
    let mut destinations = BTreeSet::new();
    for known in KNOWN_LEVELS {
        let nsd_bytes = std::fs::read(root.join(known.nsd_filename())).unwrap();
        let nsf_bytes = std::fs::read(root.join(known.nsf_filename())).unwrap();
        let metadata = parse_nsd(&nsd_bytes, known.id).unwrap();
        let nsf = parse_nsf(&nsf_bytes, &metadata).unwrap();
        for global in nsf
            .entries()
            .filter(|entry| entry.entry_type == 11 && entry.items.len() >= 6)
        {
            let Ok(_header) = crust_formats::stream::structs::GoolHeader::parse(
                global.item(0).unwrap().bytes(&nsf_bytes).unwrap(),
            ) else {
                continue;
            };
            let shared = words(global.item(1).unwrap().bytes(&nsf_bytes).unwrap());
            let internal = words(global.item(2).unwrap().bytes(&nsf_bytes).unwrap());
            let mut sources = BTreeSet::new();
            collect_direct_animation_leas(&shared, &mut sources);
            let states = global.item(4).unwrap().bytes(&nsf_bytes).unwrap();
            for state_index in 0..states.len() / GoolState::BYTE_LEN {
                let Ok(program) = load_gool_state_program(
                    &metadata,
                    &nsf,
                    &nsf_bytes,
                    global.eid,
                    u16::try_from(state_index).unwrap(),
                ) else {
                    continue;
                };
                collect_direct_animation_leas(program.code(), &mut sources);
            }
            source_count += sources.len();
            for (source, destination) in sources {
                destinations.insert(destination);
                if source < 0x0400 {
                    let raw_type = internal[usize::from(source)].to_le_bytes()[0];
                    *static_header_types.entry(raw_type).or_insert(0_usize) += 1;
                    static_program_names.insert(global.eid.name().unwrap());
                    assert!(
                        !(1..=5).contains(&raw_type),
                        "{} {} has an unexpected static type-{raw_type} LEA source",
                        known.name,
                        global.eid
                    );
                } else {
                    dynamic_sources.insert((known.id.get(), global.eid, source));
                }
            }
        }
    }
    assert_eq!(source_count, 31);
    assert_eq!(destinations, BTreeSet::from([0x0e2a]));
    assert_eq!(
        static_header_types,
        BTreeMap::from([(0x73, 18), (0xef, 12)])
    );
    assert_eq!(
        static_program_names,
        BTreeSet::from([
            "BoxsC".to_owned(),
            "DispC".to_owned(),
            "DoctC".to_owned(),
            "FruiC".to_owned(),
            "ShadC".to_owned(),
        ])
    );
    assert_eq!(
        dynamic_sources,
        BTreeSet::from([(0x07, Eid::from_name("BaraC").unwrap(), 0x0b7c,)])
    );
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn retail_payloads_of_all_five_kinds_survive_a_process_storage_alias() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let descriptor_index = 64_usize;
    let available_bytes = (REGISTER_COUNT - descriptor_index) * 4;
    let mut found = BTreeSet::new();

    'levels: for known in KNOWN_LEVELS {
        let nsd_bytes = std::fs::read(root.join(known.nsd_filename())).unwrap();
        let nsf_bytes = std::fs::read(root.join(known.nsf_filename())).unwrap();
        let metadata = parse_nsd(&nsd_bytes, known.id).unwrap();
        let nsf = parse_nsf(&nsf_bytes, &metadata).unwrap();
        for entry in nsf
            .entries()
            .filter(|entry| entry.entry_type == 11 && entry.items.len() >= 6)
        {
            let animation_bytes = entry.item(5).unwrap().bytes(&nsf_bytes).unwrap();
            for offset in 0..animation_bytes.len() {
                let Ok(descriptor) = parse_gool_animation_descriptor(animation_bytes, offset)
                else {
                    continue;
                };
                let tag = descriptor.header().kind as u8;
                let byte_len = descriptor.byte_len();
                if found.contains(&tag) || byte_len > available_bytes {
                    continue;
                }
                let Some(bytes) = animation_bytes.get(offset..offset + byte_len) else {
                    continue;
                };
                let handle = ObjectHandle::new(0).unwrap();
                let mut object = VmObject::new(handle, vec![0]).unwrap();
                object.bind_animation_data(animation_bytes);
                let mut padded = bytes.to_vec();
                padded.resize(padded.len().next_multiple_of(4), 0);
                for (word_index, bytes) in padded.chunks_exact(4).enumerate() {
                    object
                        .set_register(
                            descriptor_index + word_index,
                            u32::from_le_bytes(bytes.try_into().unwrap()),
                        )
                        .unwrap();
                }
                let reference =
                    StorageReference::checked(handle, StorageRegion::Register, descriptor_index)
                        .unwrap();
                object
                    .set_register(process_register::ANIMATION_SEQUENCE, reference.to_word())
                    .unwrap();
                let AnimationSource::Process(process) = object.animation_source().unwrap().unwrap()
                else {
                    panic!(
                        "{} {} type {tag} lost its process alias",
                        known.name, entry.eid
                    );
                };
                match (&descriptor, process.kind()) {
                    (
                        GoolAnimationDescriptor::Vertex(expected),
                        ProcessAnimationKind::Vertex(actual),
                    ) => assert_eq!(actual, expected),
                    (
                        GoolAnimationDescriptor::Sprite(expected),
                        ProcessAnimationKind::Sprite(actual),
                    ) => assert_eq!(actual, expected),
                    (
                        GoolAnimationDescriptor::Font(expected),
                        ProcessAnimationKind::Font(actual),
                    ) => assert_eq!(*actual, expected.header),
                    (
                        GoolAnimationDescriptor::Text(expected),
                        ProcessAnimationKind::Text(actual),
                    ) => {
                        assert_eq!(actual.header, expected.header);
                        assert_eq!(actual.unknown_word, expected.unknown_word);
                        assert_eq!(actual.font_word_offset, expected.font_word_offset);
                        assert_eq!(actual.terms, expected.terms);
                    }
                    (
                        GoolAnimationDescriptor::Fragment(expected),
                        ProcessAnimationKind::Fragment(actual),
                    ) => assert_eq!(actual, expected),
                    _ => panic!(
                        "{} {} type {tag} changed kind through its process alias",
                        known.name, entry.eid
                    ),
                }
                found.insert(tag);
                if found.len() == GoolAnimationKind::Fragment as usize {
                    break 'levels;
                }
            }
        }
    }

    assert_eq!(found, BTreeSet::from([1, 2, 3, 4, 5]));
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn toxic_waste_barrel_lea_installs_and_resolves_its_process_animation() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let level = LevelId::new_const(0x07);
    let nsd_bytes =
        std::fs::read(root.join(StreamName::new(level, StreamKind::Nsd).filename())).unwrap();
    let nsf_bytes =
        std::fs::read(root.join(StreamName::new(level, StreamKind::Nsf).filename())).unwrap();
    let metadata = parse_nsd(&nsd_bytes, level).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &metadata).unwrap();
    let barrel_eid = metadata.ldat().unwrap().executable_map[0x10];
    assert_eq!(barrel_eid, Eid::from_name("BaraC").unwrap());
    let program = load_gool_program(&metadata, &nsf, &nsf_bytes, barrel_eid, 0).unwrap();
    assert_eq!(program.code()[0x9f], 0x14b7_ce2a);

    let handle = ObjectHandle::new(0).unwrap();
    let mut object = VmObject::from_gool_program(handle, &program).unwrap();
    object.initialize_retail_process(0, 0).unwrap();
    let expected_index = object
        .initial_stack_pointer()
        .checked_sub(4)
        .expect("BaraC frame has the authored fp[-4] cell");
    let mut machine = Machine::new(0);
    machine.insert_object(object).unwrap();

    // BaraC's init code writes process memory word two (fp[-4]) as exact
    // type zero and word three as its authored X displacement. State three
    // later aliases those live words instead of selecting global item five.
    machine.object_mut(handle).unwrap().restart(0).unwrap();
    assert_eq!(
        machine.run(handle, 6),
        Ok(Execution {
            reason: HaltReason::BudgetExhausted,
            steps: 6,
        })
    );
    assert_eq!(
        machine
            .object(handle)
            .unwrap()
            .register(expected_index as usize),
        Ok(0)
    );
    assert_eq!(
        machine
            .object(handle)
            .unwrap()
            .register(expected_index as usize + 1),
        Ok(0x600)
    );

    machine.object_mut(handle).unwrap().restart(0x98).unwrap();
    assert_eq!(
        machine.run(handle, 8),
        Ok(Execution {
            reason: HaltReason::BudgetExhausted,
            steps: 8,
        })
    );
    let word = machine
        .object(handle)
        .unwrap()
        .register(process_register::ANIMATION_SEQUENCE)
        .unwrap();
    let reference = StorageReference::from_word(word).unwrap();
    assert_eq!(reference.object(), handle);
    assert_eq!(reference.region(), StorageRegion::Register);
    assert_eq!(u32::from(reference.index()), expected_index);
    assert_eq!(machine.read_storage_reference(reference), Ok(0));
    let source = machine
        .object(handle)
        .unwrap()
        .animation_source()
        .unwrap()
        .expect("BaraC's non-null process descriptor is a live animation source");
    let AnimationSource::Process(process) = source else {
        panic!("BaraC must retain the LEA process address, not an item-five offset");
    };
    assert_eq!(process.storage(), reference);
    assert_eq!(*process.kind(), ProcessAnimationKind::NoDraw);
    assert_eq!(
        machine
            .object(handle)
            .unwrap()
            .retail_transform()
            .unwrap()
            .translation,
        [0x600, 0, 0],
        "the descriptor's following process words remain the authored state-three displacement inputs"
    );
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn title_boxs_lea_resolves_its_internal_table_as_native_no_draw() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let level = LevelId::new_const(0x04);
    let nsd_bytes =
        std::fs::read(root.join(StreamName::new(level, StreamKind::Nsd).filename())).unwrap();
    let nsf_bytes =
        std::fs::read(root.join(StreamName::new(level, StreamKind::Nsf).filename())).unwrap();
    let metadata = parse_nsd(&nsd_bytes, level).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &metadata).unwrap();
    let boxs = Eid::from_name("BoxsC").unwrap();
    let program = load_gool_state_program(&metadata, &nsf, &nsf_bytes, boxs, 0).unwrap();
    assert_eq!(program.code()[0xfd], 0x1400_2e2a);

    let handle = ObjectHandle::new(0).unwrap();
    let mut object = VmObject::from_gool_program(handle, &program).unwrap();
    object.restart(0xfd).unwrap();
    let mut machine = Machine::new(0);
    machine.insert_object(object).unwrap();
    assert_eq!(
        machine.run(handle, 1),
        Ok(Execution {
            reason: HaltReason::BudgetExhausted,
            steps: 1,
        })
    );

    let source = machine
        .object(handle)
        .unwrap()
        .animation_source()
        .unwrap()
        .expect("BoxsC's internal word is a live no-draw animation source");
    let AnimationSource::Process(process) = source else {
        panic!("BoxsC must retain its LEA-created storage reference");
    };
    assert_eq!(process.storage().region(), StorageRegion::Internal);
    assert_eq!(process.storage().index(), 2);
    assert_eq!(*process.kind(), ProcessAnimationKind::NoDraw);
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
    assert_eq!(program.event_map().len(), 45);
    assert_eq!(program.event_map()[2], 0x84ed);
    assert_eq!(program.event_map()[3], 23);
    assert_eq!(program.event_map()[5], 0x84fa);
    assert_eq!(program.event_map()[6], 0x8500);

    let state_one = load_gool_state_program(&metadata, &nsf, &nsf_bytes, crash_eid, 1).unwrap();
    assert_eq!(state_one.event_pc(), Some(420));
    assert_eq!(state_one.code()[431], 0x8897_c000);
    let state_seven = load_gool_state_program(&metadata, &nsf, &nsf_bytes, crash_eid, 7).unwrap();
    assert_eq!(state_seven.event_pc(), Some(731));
    assert_eq!(state_seven.code()[731], 0x0481_5b7e);
    assert_eq!(state_seven.code()[732], 0x8957_c001);
    assert_eq!(program.page_count(), metadata.header.page_count);
    assert_eq!(
        program.resident_pages(),
        [
            metadata.pte(crash_eid).unwrap().page_index(),
            metadata.pte(program.external_eid()).unwrap().page_index(),
        ]
    );
    for index in [0x4c, 0x4d] {
        let eid = Eid::from_raw(program.internal_words()[index]);
        assert!(
            program
                .entry_pages()
                .contains(&(eid, metadata.pte(eid).unwrap().page_index(),))
        );
    }
    assert_eq!(program.code()[70], 0x8609_806e);
    assert_eq!(program.global_code()[131], 0x8289_4000);
    assert_eq!(program.code()[72], 0x16be_0e1f);

    let handle = ObjectHandle::new(0).unwrap();
    let mut object = VmObject::from_gool_program(handle, &program).unwrap();
    assert_eq!(object.event_map(), program.event_map());
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
    let descriptor = parse_gool_animation_descriptor(
        animation_data,
        usize::try_from(animation.offset()).unwrap(),
    )
    .unwrap();
    let GoolAnimationDescriptor::Vertex(vertex_animation) = descriptor else {
        panic!("ShadC must select a vertex animation");
    };
    let shadow_model = load_object_model_frame(
        &metadata,
        &nsf,
        &nsf_bytes,
        vertex_animation.model_eid,
        u16::try_from(
            boot_machine
                .object(shadow_handle)
                .unwrap()
                .animation_frame()
                >> 8,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        usize::from(vertex_animation.header.length),
        nsf.resolve_entry(&metadata, vertex_animation.model_eid)
            .unwrap()
            .items
            .len()
    );
    assert!(!shadow_model.geometry.polygons.is_empty());
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
