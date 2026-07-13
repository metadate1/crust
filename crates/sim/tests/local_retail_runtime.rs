//! Opt-in end-to-end runtime bridge check against the user's own retail BIN.

use std::path::PathBuf;

use crust_formats::disc::DiscImage;
use crust_formats::stream::{
    LevelId, StreamKind, StreamName, ZoneEntity, ZoneHeader, parse_nsd, parse_nsf,
};
use crust_sim::gool::VmEffect;
use crust_sim::object_arena::{NeighborZone, ObjectOrigin};
use crust_sim::retail_runtime::{NsfProgramHost, RetailRuntime};

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
    let shadow_execution = first
        .frame
        .executions
        .iter()
        .find(|execution| execution.object == shadow)
        .expect("Crash's shadow child must execute in frame one");
    assert!(
        shadow_execution.result.is_ok(),
        "solid suboperation one must use only registered frame snapshots: {:?}",
        shadow_execution.result
    );
    for execution in &first.frame.executions {
        assert!(
            execution.result.is_ok(),
            "unexpected frame-one fault for {:?}: {:?}",
            execution.object,
            execution.result
        );
    }
    for frame in 2_u32..=300 {
        let report = runtime.run_frame(&mut host, 256).unwrap();
        for execution in &report.executions {
            assert!(
                execution.result.is_ok(),
                "unexpected frame-{frame} fault for {:?}: {:?}",
                execution.object,
                execution.result
            );
        }
    }
}
