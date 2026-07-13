//! Opt-in end-to-end runtime bridge check against the user's own retail BIN.

use std::path::PathBuf;

use crust_formats::disc::DiscImage;
use crust_formats::stream::{
    LevelId, StreamKind, StreamName, ZoneEntity, ZoneHeader, load_gool_state_program, parse_nsd,
    parse_nsf,
};
use crust_sim::gool::{
    CodeAddress, CodeSegment, ObjectHandle as VmObjectHandle, VmEffect, VmError,
};
use crust_sim::object_arena::{NeighborZone, ObjectOrigin};
use crust_sim::retail_runtime::{NsfProgramHost, RetailRuntime, RuntimeError};

#[test]
#[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
fn n_sanity_neighbors_spawn_and_crash_hosts_both_boot_children() {
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
    let nsd = parse_nsd(&nsd_bytes, level).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
    let spawn_zone = nsd.ldat().unwrap().spawn_zone;
    let current_entry = nsf.resolve_entry(&nsd, spawn_zone).unwrap();
    let current_header =
        ZoneHeader::parse(current_entry.item(0).unwrap().bytes(&nsf_bytes).unwrap()).unwrap();

    let mut owned_neighbors = Vec::new();
    for eid in current_header.neighbors {
        let entry = nsf.resolve_entry(&nsd, eid).unwrap();
        let header = ZoneHeader::parse(entry.item(0).unwrap().bytes(&nsf_bytes).unwrap()).unwrap();
        let mut entities = Vec::new();
        for entity_index in 0..header.entity_count {
            let item_index =
                usize::try_from(header.entity_item_index(entity_index).unwrap()).unwrap();
            entities.push(
                ZoneEntity::parse(entry.item(item_index).unwrap().bytes(&nsf_bytes).unwrap())
                    .unwrap(),
            );
        }
        // The first LevelUpdate marks every current-zone neighbor loaded and
        // displayed (`|= 3`) immediately before LevelSpawnObjects scans it.
        owned_neighbors.push((eid, header.display_flags | 3, entities));
    }
    let neighbors = owned_neighbors
        .iter()
        .map(|(eid, display_flags, entities)| NeighborZone {
            eid: *eid,
            display_flags: *display_flags,
            entities,
        })
        .collect::<Vec<_>>();
    let mut host = NsfProgramHost::new(&nsd, &nsf, &nsf_bytes);
    let mut runtime = RetailRuntime::new(256);
    let first = runtime
        .spawn_and_run_frame(&neighbors, &mut host, 256)
        .unwrap();
    assert_eq!(first.spawn_attempts.len(), 7);
    assert!(
        first
            .spawn_attempts
            .iter()
            .all(|attempt| attempt.result.is_ok())
    );
    let crash = first
        .spawn_attempts
        .iter()
        .find(|attempt| attempt.descriptor.executable == 0)
        .unwrap()
        .result
        .as_ref()
        .unwrap();
    let crash_spawns = first
        .frame
        .effects
        .iter()
        .filter_map(|effect| match effect {
            VmEffect::SpawnChildren {
                parent,
                executable,
                subtype,
                arguments,
                ..
            } if *parent == crash.vm() => Some((*executable, *subtype, arguments.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(crash_spawns, [(5, 0, vec![0]), (29, 0, vec![0, 4096, 0])]);

    let _shadow = first
        .frame
        .spawned_children
        .iter()
        .copied()
        .find(|child| {
            runtime.arena().get(child.arena()).is_some_and(|object| {
                object.origin()
                    == ObjectOrigin::Runtime {
                        executable: 29,
                        subtype: 0,
                    }
            })
        })
        .expect("Crash must synchronously bind ShadC");
    let path_object = first
        .frame
        .executions
        .iter()
        .find(|execution| {
            runtime
                .arena()
                .get(execution.object.arena())
                .is_some_and(|object| {
                    matches!(
                        object.origin(),
                        ObjectOrigin::Entity(descriptor)
                            if descriptor.id == 14
                                && descriptor.group == 3
                                && descriptor.executable == 31
                                && descriptor.subtype == 0
                    )
                })
        })
        .expect("N. Sanity path object must execute in frame one");
    assert!(
        path_object.result.is_ok(),
        "0x85 path orientation must cross its old boundary: {:?}",
        path_object.result
    );
    let path_vm = runtime.machine().object(path_object.object.vm()).unwrap();
    assert_eq!(
        [
            path_vm.register(8).unwrap().cast_signed(),
            path_vm.register(9).unwrap().cast_signed(),
            path_vm.register(10).unwrap().cast_signed(),
        ],
        [1_842_944, 1_076_736, 31_948_288]
    );
    assert_eq!(path_vm.register(47), Ok(1_076_736));
    assert_eq!(path_vm.register(13), Ok(0x400));
    assert_eq!(path_vm.register(21), Ok(0x400));
    let mut first_fault = None;
    for execution in &first.frame.executions {
        if let Err(error) = &execution.result {
            let origin = runtime
                .arena()
                .get(execution.object.arena())
                .expect("faulted object remains in the arena")
                .origin();
            let address = runtime
                .machine()
                .object(execution.object.vm())
                .expect("faulted object remains in the VM")
                .code_address();
            let state = runtime
                .machine()
                .object(execution.object.vm())
                .expect("faulted object remains in the VM")
                .state();
            let expected = matches!(
                error,
                RuntimeError::Vm(VmError::UnsupportedSolidObjectBounds(candidate))
                    if candidate.get() == 6
            );
            first_fault = Some((
                1_u32,
                execution.object,
                origin,
                address,
                state,
                format!("{error:?}"),
                expected,
            ));
            break;
        }
    }
    for frame in 2_u32..=300 {
        let report = runtime.run_frame(&mut host, 256).unwrap();
        for execution in &report.executions {
            if let Err(error) = &execution.result
                && first_fault.is_none()
            {
                let origin = runtime
                    .arena()
                    .get(execution.object.arena())
                    .expect("faulted object remains in the arena")
                    .origin();
                let address = runtime
                    .machine()
                    .object(execution.object.vm())
                    .expect("faulted object remains in the VM")
                    .code_address();
                let state = runtime
                    .machine()
                    .object(execution.object.vm())
                    .expect("faulted object remains in the VM")
                    .state();
                let expected = matches!(
                    error,
                    RuntimeError::Vm(VmError::UnsupportedSolidObjectBounds(candidate))
                        if candidate.get() == 6
                );
                first_fault = Some((
                    frame,
                    execution.object,
                    origin,
                    address,
                    state,
                    format!("{error:?}"),
                    expected,
                ));
            }
        }
    }
    eprintln!("first source-derived fault through 300 frames: {first_fault:?}");
    let Some((frame, object, origin, address, state, error, expected)) = first_fault else {
        panic!("the legal trace unexpectedly crossed every typed VM boundary");
    };
    assert_eq!(frame, 1);
    assert_eq!(
        origin,
        ObjectOrigin::Runtime {
            executable: 29,
            subtype: 0,
        }
    );
    assert_eq!(
        address,
        CodeAddress {
            segment: CodeSegment::External,
            // Fetch is post-incremented: after exact suboperation three,
            // ShadC's next typed collision boundary is external word 40.
            pc: 41,
        }
    );
    assert_eq!(state, 1);
    let solid_vm = runtime.machine().object(object.vm()).unwrap();
    assert_eq!(
        [
            solid_vm.register(38).unwrap(),
            solid_vm.register(39).unwrap(),
            solid_vm.register(40).unwrap(),
        ],
        [0; 3],
        "static trans4 must clear B's three process words"
    );
    assert_eq!(
        [
            solid_vm.register(23).unwrap(),
            solid_vm.register(24).unwrap(),
            solid_vm.register(25).unwrap(),
        ],
        [
            solid_vm.register(8).unwrap(),
            solid_vm.register(9).unwrap(),
            solid_vm.register(10).unwrap(),
        ],
        "ZoneFindNearestObjectNode3 never writes back its local query vector"
    );
    let executable = nsd.ldat().unwrap().executable_map[29];
    let state_program = load_gool_state_program(&nsd, &nsf, &nsf_bytes, executable, state).unwrap();
    let instruction = state_program.code()[address.pc - 1];
    assert_eq!(instruction, 0x8e06_de26);
    let bound_candidate = VmObjectHandle::new(6).unwrap();
    let candidate_execution = first
        .frame
        .executions
        .iter()
        .find(|execution| execution.object.vm() == bound_candidate)
        .unwrap();
    assert!(matches!(
        runtime
            .arena()
            .get(candidate_execution.object.arena())
            .unwrap()
            .origin(),
        ObjectOrigin::Entity(descriptor)
            if descriptor.id == 14
                && descriptor.group == 3
                && descriptor.executable == 31
                && descriptor.subtype == 0
    ));
    assert_eq!(
        runtime
            .machine()
            .object(bound_candidate)
            .unwrap()
            .register(27),
        Ok(0x0003_2814),
        "the legal candidate is collidable and passes the source shadow-bound mask"
    );
    eprintln!(
        "active object-bound boundary within solid suboperation one is state {state} external word {} = {instruction:#010x}",
        address.pc - 1
    );
    assert!(
        expected,
        "unexpected first fault for object {object:?}: {error}"
    );
}
