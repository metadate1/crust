//! Opt-in end-to-end runtime bridge check against the user's own retail BIN.

use std::path::PathBuf;

use crust_formats::disc::DiscImage;
use crust_formats::stream::{
    LevelId, StreamKind, StreamName, ZoneEntity, ZoneHeader, parse_nsd, parse_nsf,
};
use crust_sim::gool::{HaltReason, VmEffect, VmError};
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
        .spawn_and_run_frame(&neighbors, &mut host, 67)
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

    let shadow = first
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
    assert!(first.frame.executions.iter().any(|execution| {
        execution.object == shadow
            && execution
                .result
                .as_ref()
                .is_ok_and(|result| result.reason == HaltReason::StateChanged(1))
    }));

    // Production state rebinding must resolve state one through the same NSF
    // host and carry global animation item five into the following frames.
    let second = runtime.run_frame(&mut host, 67).unwrap();
    let shadow_second = second
        .executions
        .iter()
        .find(|execution| execution.object == shadow)
        .unwrap()
        .result
        .as_ref()
        .unwrap();
    assert_eq!(
        shadow_second.reason,
        HaltReason::AnimationChanged { frame: 0, wait: 1 }
    );
    assert!(shadow_second.steps > 2);

    let third = runtime.run_frame(&mut host, 67).unwrap();
    let shadow_third = third
        .executions
        .iter()
        .find(|execution| execution.object == shadow)
        .unwrap()
        .result
        .as_ref()
        .unwrap();
    assert!(matches!(
        shadow_third.reason,
        HaltReason::AnimationChanged { frame: 0, .. }
    ));
    assert!(shadow_third.steps > 0);

    let errors_by_frame = [
        ("initial", first.frame.executions.as_slice()),
        ("rebound", second.executions.as_slice()),
        ("resumed", third.executions.as_slice()),
    ]
    .map(|(label, executions)| {
        (
            label,
            executions
                .iter()
                .filter_map(|execution| execution.result.as_ref().err())
                .collect::<Vec<_>>(),
        )
    });

    // The remaining boundaries are explicit and data-derived. All four
    // initial exe-34 objects request 0x8e suboperation six, whose entity-node
    // color-seek host state is not available yet. Once 0x83 resumes, the
    // former InvalidOperand(0) is gone: opcode 0x11 correctly discards a
    // popped value through its null output. Crash then reaches the exact 0x26
    // address-tagging boundary on the third frame.
    assert_eq!(errors_by_frame[0].0, "initial");
    assert_eq!(errors_by_frame[0].1.len(), 4);
    assert!(
        errors_by_frame[0].1.iter().all(|error| matches!(
            error,
            RuntimeError::Vm(VmError::UnsupportedSolidSurface {
                suboperation: 6,
                input_vector: 5,
                output_vector: 4,
                operand: 0x0e26,
            })
        )),
        "unexpected initial retail boundaries: {errors_by_frame:?}"
    );
    assert_eq!(errors_by_frame[1].0, "rebound");
    assert!(errors_by_frame[1].1.is_empty());
    assert_eq!(errors_by_frame[2].0, "resumed");
    assert!(matches!(
        errors_by_frame[2].1.as_slice(),
        [RuntimeError::Vm(VmError::UnsupportedInputReference {
            source: 0x04d,
            destination: 0x04c,
        })]
    ));
}
