#![forbid(unsafe_code)]

//! Browser host for the Rust runtime.

use wasm_bindgen::prelude::*;

#[cfg(any(target_arch = "wasm32", test))]
use crust_sim::card::SaveData;
#[cfg(any(target_arch = "wasm32", test))]
use crust_sim::retail_runtime::{RenderObjectsError, RetailRenderObject};

#[cfg(target_arch = "wasm32")]
mod app;
#[cfg(target_arch = "wasm32")]
mod assets;
#[cfg(any(target_arch = "wasm32", test))]
mod card_persistence;
#[cfg(target_arch = "wasm32")]
mod disc_import;
#[cfg(target_arch = "wasm32")]
mod dom;
#[cfg(any(target_arch = "wasm32", test))]
mod pbak_runtime;
#[cfg(any(target_arch = "wasm32", test))]
pub mod renderer_backend;
#[cfg(any(target_arch = "wasm32", test))]
mod retail_clock;
pub mod retail_scene;
#[cfg(target_arch = "wasm32")]
mod storage;
#[cfg(any(target_arch = "wasm32", test))]
mod title_runtime;
#[cfg(target_arch = "wasm32")]
mod webaudio;
#[cfg(target_arch = "wasm32")]
mod webgl;

#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn initial_presented_path_point(
    point_count: core::num::NonZeroU16,
    after_loading_image: bool,
) -> usize {
    let desired = if after_loading_image { 2 } else { 1 };
    desired.min(usize::from(point_count.get() - 1))
}

/// Retail's first mount starts without any checkpoint or collected boxes.
///
/// The mounted GOOL globals and level-state snapshot are authoritative as soon
/// as the runtime is constructed; the high-level flow mirror has no player
/// state of its own.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InitialRetailLevelState {
    pub box_count: i32,
    pub checkpoint_id: i32,
    pub checkpoint_translation: [i32; 3],
}

#[cfg(any(target_arch = "wasm32", test))]
pub(crate) const fn initial_retail_level_state() -> InitialRetailLevelState {
    InitialRetailLevelState {
        box_count: 0,
        checkpoint_id: -1,
        checkpoint_translation: [0; 3],
    }
}

/// Returns the live retail payload when it is readable, otherwise retaining
/// the most recent payload that was read successfully from the same globals.
///
/// The fallback is deliberately exact save data rather than a reconstruction
/// from the high-level display mirror.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn authoritative_save_or_last<E>(
    current: Result<SaveData, E>,
    last: SaveData,
) -> SaveData {
    current.unwrap_or(last)
}

/// Performs the `PadUpdate` that native calls at the start of
/// `CoreObjectsCreate` on every initial boot and stream remount.
///
/// The browser's one-frame `pending` latch preserves a complete press that
/// began and ended between cooperative samples. Folding it into this boundary
/// makes the first destination `CoreFrame` observe the same shifted history as
/// the source while a physically held button remains edge-free. An armed
/// attract recording supplies `Some(0)`, matching `PadUpdatePbak` state three:
/// history shifts normally, but the new held/tapped words stay zero.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn core_objects_pad_update(
    pad: &mut crust_platform::input::PadState,
    physical: u16,
    pending: u16,
    demo_override: Option<u32>,
) -> crust_platform::input::PadSnapshot {
    pad.update(physical | pending, 0, demo_override);
    pad.snapshot()
}

/// Side of native `LevelUpdate` on which `CamUpdate` publishes the external
/// island-camera state.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetailIslandWritebackPhase {
    BeforeLevelUpdate,
    AfterLevelUpdate,
}

/// Selects the source-compatible island-state writeback boundary.
///
/// Mode seven updates the global before its optional `LevelUpdate`; mode eight
/// calls `LevelUpdate` first and only then publishes the directed-camera state.
/// Keeping this decision outside the browser host makes the otherwise subtle
/// synchronous TERM-handler ordering directly testable on native targets.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) const fn retail_island_state_writeback(
    outcome: crust_sim::camera::RetailCameraOutcome,
) -> Option<(RetailIslandWritebackPhase, i32)> {
    use crust_sim::camera::RetailCameraOutcome;

    match outcome {
        RetailCameraOutcome::IslandAdvanced {
            mode: 7,
            state_after,
            ..
        } => Some((RetailIslandWritebackPhase::BeforeLevelUpdate, state_after)),
        RetailCameraOutcome::IslandAdvanced {
            mode: 8,
            state_after,
            ..
        } => Some((RetailIslandWritebackPhase::AfterLevelUpdate, state_after)),
        _ => None,
    }
}

/// Converts the checked object snapshot into the only scene input the browser
/// may accept. A rejected snapshot must stop the runtime instead of silently
/// degrading the frame to world-only rendering.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn require_render_object_snapshot(
    snapshot: Result<Vec<RetailRenderObject>, RenderObjectsError>,
) -> Result<Vec<RetailRenderObject>, String> {
    snapshot.map_err(|error| format!("retail render-object snapshot failed: {error:?}"))
}

#[wasm_bindgen]
/// Starts the browser application after the generated Wasm module is initialized.
///
/// # Errors
///
/// Returns a JavaScript exception when required DOM, WebGL2, storage, or event bindings cannot be
/// initialized. Native builds use a no-op implementation so the workspace remains testable.
pub fn boot() -> Result<(), JsValue> {
    #[cfg(target_arch = "wasm32")]
    {
        app::boot()
    }
    #[cfg(not(target_arch = "wasm32"))]
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_presentation_clamps_one_point_title_and_transition_paths() {
        let one = core::num::NonZeroU16::new(1).unwrap();
        assert_eq!(initial_presented_path_point(one, false), 0);
        assert_eq!(initial_presented_path_point(one, true), 0);
        assert_eq!(
            initial_presented_path_point(core::num::NonZeroU16::new(3).unwrap(), false),
            1
        );
        assert_eq!(
            initial_presented_path_point(core::num::NonZeroU16::new(3).unwrap(), true),
            2
        );
    }

    #[test]
    fn first_retail_mount_uses_the_authored_empty_level_state_seed() {
        assert_eq!(
            initial_retail_level_state(),
            InitialRetailLevelState {
                box_count: 0,
                checkpoint_id: -1,
                checkpoint_translation: [0; 3],
            }
        );
    }

    #[test]
    fn live_retail_save_wins_and_failure_retains_last_exact_payload() {
        let last = SaveData {
            level_count: 7,
            initial_lives: 3 << 8,
            item_pool_1: 0x1234,
            ..SaveData::default()
        };
        let live = SaveData {
            level_count: 19,
            initial_lives: 8 << 8,
            item_pool_1: 0xabcd,
            ..SaveData::default()
        };

        assert_eq!(
            authoritative_save_or_last::<()>(Ok(live), last),
            live,
            "a readable retail payload must always take precedence"
        );
        assert_eq!(
            authoritative_save_or_last::<()>(Err(()), last),
            last,
            "an unreadable VM must retain the last exact retail payload"
        );
    }

    #[test]
    fn rejected_render_object_snapshot_is_not_replaced_by_an_empty_scene() {
        let error = require_render_object_snapshot(Err(RenderObjectsError::InvalidRootIndex(8)))
            .unwrap_err();

        assert_eq!(
            error,
            "retail render-object snapshot failed: InvalidRootIndex(8)"
        );
    }

    #[test]
    fn core_objects_pad_boundary_exposes_a_new_mount_press() {
        use crust_platform::input::{PAD_CROSS, PAD_START, PadState};

        let mut pad = PadState::default();
        pad.update(PAD_CROSS, 0, None);
        pad.update(0, 0, None);

        let mounted = core_objects_pad_update(&mut pad, PAD_START, 0, None);
        assert_eq!(mounted.held, u32::from(PAD_START));
        assert_eq!(mounted.tapped, u32::from(PAD_START));
        assert_eq!(mounted.held_previous, 0);
        assert_eq!(mounted.held_previous_2, u32::from(PAD_CROSS));
    }

    #[test]
    fn core_objects_pad_boundary_does_not_retap_a_held_button() {
        use crust_platform::input::{PAD_START, PadState};

        let mut pad = PadState::default();
        pad.update(PAD_START, 0, None);
        let mounted = core_objects_pad_update(&mut pad, PAD_START, 0, None);

        assert_eq!(mounted.held, u32::from(PAD_START));
        assert_eq!(mounted.held_previous, u32::from(PAD_START));
        assert_eq!(mounted.tapped, 0);
        assert_eq!(mounted.tapped_previous, u32::from(PAD_START));
    }

    #[test]
    fn core_objects_pad_boundary_consumes_a_between_frame_latch() {
        use crust_platform::input::{PAD_START, PadState};

        let mut pad = PadState::default();
        let mounted = core_objects_pad_update(&mut pad, 0, PAD_START, None);

        assert_eq!(mounted.held, u32::from(PAD_START));
        assert_eq!(mounted.tapped, u32::from(PAD_START));

        // Crash's later in-frame `PadUpdate` sees the already-shifted mount
        // sample, so the same latched press cannot create a second edge.
        pad.update(PAD_START, 0, None);
        assert_eq!(pad.snapshot().tapped, 0);
        assert_eq!(pad.snapshot().held_previous, u32::from(PAD_START));
    }

    #[test]
    fn core_objects_pad_boundary_suppresses_current_input_while_attract_is_armed() {
        use crust_platform::input::{PAD_CROSS, PAD_START, PadState};

        let mut pad = PadState::default();
        pad.update(PAD_CROSS, 0, None);
        let mounted = core_objects_pad_update(&mut pad, PAD_START, PAD_CROSS, Some(0));

        // PbakChoose sets state three before CoreObjectsCreate. Native's
        // PadUpdatePbak then forces only the new held word to zero; the normal
        // PadUpdate history shift still happened.
        assert_eq!(mounted.held, 0);
        assert_eq!(mounted.tapped, 0);
        assert_eq!(mounted.held_previous, u32::from(PAD_CROSS));
        assert_eq!(mounted.tapped_previous, u32::from(PAD_CROSS));
    }

    #[test]
    fn island_state_writeback_uses_the_source_side_of_level_update_for_every_mode() {
        use crust_sim::camera::RetailCameraOutcome;

        for mode in 0..=u16::MAX {
            let outcome = RetailCameraOutcome::IslandAdvanced {
                mode,
                state_before: 3,
                state_after: -7,
                path_crossings: 1,
                moved: true,
            };
            let expected = match mode {
                7 => Some((RetailIslandWritebackPhase::BeforeLevelUpdate, -7)),
                8 => Some((RetailIslandWritebackPhase::AfterLevelUpdate, -7)),
                _ => None,
            };
            assert_eq!(
                retail_island_state_writeback(outcome),
                expected,
                "mode {mode}"
            );
        }
    }

    #[test]
    fn island_state_writeback_preserves_all_signed_states_and_rejects_other_outcomes() {
        use crust_sim::camera::RetailCameraOutcome;

        for state_after in [i32::MIN, -1, 0, 1, i32::MAX] {
            for (mode, phase) in [
                (7, RetailIslandWritebackPhase::BeforeLevelUpdate),
                (8, RetailIslandWritebackPhase::AfterLevelUpdate),
            ] {
                assert_eq!(
                    retail_island_state_writeback(RetailCameraOutcome::IslandAdvanced {
                        mode,
                        state_before: state_after.wrapping_sub(1),
                        state_after,
                        path_crossings: u32::MAX,
                        moved: false,
                    }),
                    Some((phase, state_after)),
                );
            }
        }

        for outcome in [
            RetailCameraOutcome::Stationary,
            RetailCameraOutcome::AutoAdvanced {
                skipped: true,
                path_crossings: 2,
            },
            RetailCameraOutcome::FollowBoundary { mode: 5 },
            RetailCameraOutcome::FollowEvaluated {
                mode: 6,
                candidate_count: u8::MAX,
                moved: true,
                crossed_path: true,
            },
            RetailCameraOutcome::IslandBoundary { mode: 7 },
            RetailCameraOutcome::IslandBoundary { mode: 8 },
        ] {
            assert_eq!(retail_island_state_writeback(outcome), None);
        }
    }

    #[test]
    fn island_writeback_phase_controls_what_synchronous_term_observes() {
        use crust_sim::camera::RetailCameraOutcome;

        let observe_level_update = |outcome, initial_state| {
            let writeback = retail_island_state_writeback(outcome).unwrap();
            let mut live_state = initial_state;
            if let (RetailIslandWritebackPhase::BeforeLevelUpdate, state_after) = writeback {
                live_state = state_after;
            }
            // A cross-zone LevelUpdate synchronously runs departing TERM
            // handlers here, before returning to CamUpdate.
            let term_observed = live_state;
            if let (RetailIslandWritebackPhase::AfterLevelUpdate, state_after) = writeback {
                live_state = state_after;
            }
            (term_observed, live_state)
        };

        let mode_seven = RetailCameraOutcome::IslandAdvanced {
            mode: 7,
            state_before: -1,
            state_after: 1,
            path_crossings: 1,
            moved: true,
        };
        assert_eq!(observe_level_update(mode_seven, -1), (1, 1));

        let mode_eight_exit = RetailCameraOutcome::IslandAdvanced {
            mode: 8,
            state_before: 3,
            state_after: 1,
            path_crossings: 1,
            moved: true,
        };
        assert_eq!(observe_level_update(mode_eight_exit, 3), (3, 1));
    }
}
