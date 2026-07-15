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
/// Keep this seed independent from the legacy `GameFlow::player` mirror: the
/// mounted GOOL globals and level-state snapshot become authoritative as soon
/// as the runtime is constructed.
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
/// from the legacy high-level flow mirror.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn authoritative_save_or_last<E>(
    current: Result<SaveData, E>,
    last: SaveData,
) -> SaveData {
    current.unwrap_or(last)
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
    fn first_retail_mount_does_not_inherit_synthetic_player_state() {
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
}
