//! Opt-in first-frame boot sweep across every legally local retail pair.
//!
//! This test never copies retail bytes into the repository. It exists to make
//! broad runtime parity failures actionable instead of hiding behind the one
//! deeply characterized N. Sanity Beach trace.

use std::{collections::BTreeSet, path::PathBuf};

use crust_formats::stream::{
    KNOWN_LEVELS, RetailZoneGraph, ZoneEntity, ZoneHeader, parse_nsd, parse_nsf,
};
use crust_sim::camera::RetailCameraLocation;
use crust_sim::object_arena::{NeighborZone, SpawnError};
use crust_sim::retail_frame::PathProgress;
use crust_sim::retail_runtime::{
    NsfProgramError, NsfProgramHost, RetailLevelStateContext, RetailRuntime, RuntimeError,
};

fn is_native_rejected_spawn(
    result: &Result<crust_sim::retail_runtime::RuntimeObjectHandle, RuntimeError<NsfProgramError>>,
) -> bool {
    match result {
        Err(RuntimeError::Spawn(
            SpawnError::SpawnBlocked { .. } | SpawnError::MainObjectAlreadyActive,
        )) => true,
        Err(RuntimeError::Program(NsfProgramError::Format(error))) => error
            .message()
            .contains("maps to the invalid-state sentinel"),
        _ => false,
    }
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn every_bootable_retail_pair_crosses_its_first_runtime_frame() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let expected_pairs = KNOWN_LEVELS.iter().filter(|known| known.bootable).count();
    let mut attempted_pairs = 0_usize;
    let mut booted_pairs = 0_usize;
    let mut failures = Vec::new();

    for known in KNOWN_LEVELS.iter().filter(|known| known.bootable) {
        attempted_pairs += 1;
        let nsd_path = root.join(known.nsd_filename());
        let nsf_path = root.join(known.nsf_filename());
        let nsd_bytes = match std::fs::read(&nsd_path) {
            Ok(bytes) => bytes,
            Err(error) => {
                failures.push(format!(
                    "{} ({}) NSD {}: {error}",
                    known.name,
                    known.id,
                    nsd_path.display()
                ));
                continue;
            }
        };
        let nsf_bytes = match std::fs::read(&nsf_path) {
            Ok(bytes) => bytes,
            Err(error) => {
                failures.push(format!(
                    "{} ({}) NSF {}: {error}",
                    known.name,
                    known.id,
                    nsf_path.display()
                ));
                continue;
            }
        };
        let nsd = match parse_nsd(&nsd_bytes, known.id) {
            Ok(nsd) => nsd,
            Err(error) => {
                failures.push(format!("{} ({}) NSD parse: {error}", known.name, known.id));
                continue;
            }
        };
        let nsf = match parse_nsf(&nsf_bytes, &nsd) {
            Ok(nsf) => nsf,
            Err(error) => {
                failures.push(format!("{} ({}) NSF parse: {error}", known.name, known.id));
                continue;
            }
        };
        let graph = match RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes) {
            Ok(graph) => graph,
            Err(error) => {
                failures.push(format!(
                    "{} ({}) reachable zone graph: {error}",
                    known.name, known.id
                ));
                continue;
            }
        };
        let spawn_zone = graph.spawn_path().zone;
        let current_entry = match nsf.resolve_entry(&nsd, spawn_zone) {
            Ok(entry) => entry,
            Err(error) => {
                failures.push(format!(
                    "{} ({}) spawn ZDAT {spawn_zone}: {error}",
                    known.name, known.id
                ));
                continue;
            }
        };
        let current_header = match current_entry
            .item(0)
            .ok_or_else(|| "header item is absent".to_owned())
            .and_then(|item| item.bytes(&nsf_bytes).map_err(|error| error.to_string()))
            .and_then(|bytes| ZoneHeader::parse(bytes).map_err(|error| error.to_string()))
        {
            Ok(header) => header,
            Err(error) => {
                failures.push(format!(
                    "{} ({}) spawn ZDAT {spawn_zone} header: {error}",
                    known.name, known.id
                ));
                continue;
            }
        };

        let mut owned_neighbors = Vec::with_capacity(current_header.neighbors.len());
        let mut pair_failed = false;
        for eid in current_header.neighbors.iter().copied() {
            let entry = match nsf.resolve_entry(&nsd, eid) {
                Ok(entry) => entry,
                Err(error) => {
                    failures.push(format!(
                        "{} ({}) neighbor ZDAT {eid}: {error}",
                        known.name, known.id
                    ));
                    pair_failed = true;
                    break;
                }
            };
            let header = match entry
                .item(0)
                .ok_or_else(|| "header item is absent".to_owned())
                .and_then(|item| item.bytes(&nsf_bytes).map_err(|error| error.to_string()))
                .and_then(|bytes| ZoneHeader::parse(bytes).map_err(|error| error.to_string()))
            {
                Ok(header) => header,
                Err(error) => {
                    failures.push(format!(
                        "{} ({}) neighbor ZDAT {eid} header: {error}",
                        known.name, known.id
                    ));
                    pair_failed = true;
                    break;
                }
            };
            let mut entities = Vec::with_capacity(header.entity_count as usize);
            for entity_index in 0..header.entity_count {
                let Some(item_index) = header.entity_item_index(entity_index) else {
                    failures.push(format!(
                        "{} ({}) neighbor ZDAT {eid} entity {entity_index}: item index is absent",
                        known.name, known.id
                    ));
                    pair_failed = true;
                    break;
                };
                let Ok(item_index) = usize::try_from(item_index) else {
                    failures.push(format!(
                        "{} ({}) neighbor ZDAT {eid} entity {entity_index}: item index does not fit this host",
                        known.name, known.id
                    ));
                    pair_failed = true;
                    break;
                };
                let entity = match entry
                    .item(item_index)
                    .ok_or_else(|| format!("item {item_index} is absent"))
                    .and_then(|item| item.bytes(&nsf_bytes).map_err(|error| error.to_string()))
                    .and_then(|bytes| ZoneEntity::parse(bytes).map_err(|error| error.to_string()))
                {
                    Ok(entity) => entity,
                    Err(error) => {
                        failures.push(format!(
                            "{} ({}) neighbor ZDAT {eid} entity {entity_index}: {error}",
                            known.name, known.id
                        ));
                        pair_failed = true;
                        break;
                    }
                };
                entities.push(entity);
            }
            if pair_failed {
                break;
            }
            // Native's first LevelUpdate marks the current band loaded and
            // displayed immediately before LevelSpawnObjects scans it.
            owned_neighbors.push((eid, header.display_flags | 3, entities));
        }
        if pair_failed {
            continue;
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
        let mut runtime = RetailRuntime::new_for_level(256, known.id);
        // Native LevelInit publishes cur_zone through LevelUpdate before the
        // first CoreFrame; the browser installs this same mount context before
        // creating roots, so the direct runtime harness must do so as well.
        let mut seen_active_neighbors = BTreeSet::new();
        runtime.set_level_state_context(RetailLevelStateContext {
            location: RetailCameraLocation {
                path: graph.spawn_path(),
                progress: PathProgress::ZERO,
            },
            graphics_flags: current_header.graphics.flags,
            box_count: 0,
            checkpoint_id: -1,
            checkpoint_translation: [0; 3],
            first_spawn: false,
            active_neighbor_zones: neighbors
                .iter()
                .filter(|neighbor| {
                    neighbor.display_flags & 1 != 0 && seen_active_neighbors.insert(neighbor.eid)
                })
                .map(|neighbor| neighbor.eid)
                .collect(),
        });
        let first = match runtime.spawn_and_run_frame(&neighbors, &mut host, 1_024) {
            Ok(first) => first,
            Err(error) => {
                failures.push(format!(
                    "{} ({}) first runtime frame aborted: {error:?}",
                    known.name, known.id
                ));
                continue;
            }
        };
        let spawn_failures = first
            .spawn_attempts
            .iter()
            .filter(|attempt| attempt.result.is_err() && !is_native_rejected_spawn(&attempt.result))
            .collect::<Vec<_>>();
        let execution_failures = first
            .frame
            .executions
            .iter()
            .filter(|execution| execution.result.is_err())
            .collect::<Vec<_>>();
        if spawn_failures.is_empty() && execution_failures.is_empty() {
            booted_pairs += 1;
        } else {
            failures.push(format!(
                "{} ({}) first frame: {} spawn failure(s) {spawn_failures:?}; {} execution failure(s) {execution_failures:?}",
                known.name,
                known.id,
                spawn_failures.len(),
                execution_failures.len()
            ));
        }
    }

    assert_eq!(attempted_pairs, expected_pairs);
    assert_eq!(expected_pairs, 43, "retail bootable-pair count changed");
    assert!(
        failures.is_empty(),
        "{booted_pairs}/{expected_pairs} bootable retail pairs crossed frame one; failures:\n{}",
        failures.join("\n")
    );
}
