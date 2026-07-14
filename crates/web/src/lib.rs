#![forbid(unsafe_code)]

//! Browser host for the Rust runtime.

use wasm_bindgen::prelude::*;

#[cfg(any(target_arch = "wasm32", test))]
use crust_sim::flow::FlowState;

#[cfg(target_arch = "wasm32")]
mod app;
#[cfg(target_arch = "wasm32")]
mod assets;
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

/// The browser may advance the high-level flow mirror only for the authored
/// title presentation. Gameplay, completion, bonus, boss, intro, and ending
/// progression are owned exclusively by the mounted retail runtime.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BrowserFlowMirrorAdvance {
    HoldForRetailRuntime,
    TickAuthoredTitle,
}

#[cfg(any(target_arch = "wasm32", test))]
pub(crate) const fn browser_flow_mirror_advance(
    state: &FlowState,
    authored_title_runtime_active: bool,
) -> BrowserFlowMirrorAdvance {
    if matches!(state, FlowState::Title) && authored_title_runtime_active {
        BrowserFlowMirrorAdvance::TickAuthoredTitle
    } else {
        BrowserFlowMirrorAdvance::HoldForRetailRuntime
    }
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
    use crust_sim::flow::LevelId;

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
    fn browser_flow_mirror_has_no_synthetic_gameplay_or_title_fallback() {
        let level = LevelId::new(0x03).unwrap();
        let states = [
            FlowState::Boot,
            FlowState::Gameplay(level),
            FlowState::Bonus(level),
            FlowState::Boss(level),
            FlowState::LevelComplete {
                source: level,
                missed_boxes: 7,
            },
            FlowState::Intro,
            FlowState::Ending,
        ];
        for state in states {
            assert_eq!(
                browser_flow_mirror_advance(&state, false),
                BrowserFlowMirrorAdvance::HoldForRetailRuntime
            );
            assert_eq!(
                browser_flow_mirror_advance(&state, true),
                BrowserFlowMirrorAdvance::HoldForRetailRuntime
            );
        }
        assert_eq!(
            browser_flow_mirror_advance(&FlowState::Title, false),
            BrowserFlowMirrorAdvance::HoldForRetailRuntime
        );
        assert_eq!(
            browser_flow_mirror_advance(&FlowState::Title, true),
            BrowserFlowMirrorAdvance::TickAuthoredTitle
        );
    }
}
