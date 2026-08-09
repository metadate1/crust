#![forbid(unsafe_code)]

//! Browser host for the Rust runtime.

use wasm_bindgen::prelude::*;

#[cfg(any(target_arch = "wasm32", test))]
use crust_formats::binary::Eid;
#[cfg(any(target_arch = "wasm32", test))]
use crust_formats::stream::{LevelId as FormatLevelId, RetailPathId};
#[cfg(any(target_arch = "wasm32", test))]
use crust_sim::card::SaveData;
#[cfg(any(target_arch = "wasm32", test))]
use crust_sim::gool::{
    INITIAL_LIFE_COUNT_GLOBAL, ITEM_POOL_2_GLOBAL, LEVELS_UNLOCKED_GLOBAL, LIFE_COUNT_GLOBAL,
    ObjectHandle, SendEventTarget, TITLE_STATE_GLOBAL, VmError,
};
#[cfg(any(target_arch = "wasm32", test))]
use crust_sim::retail_runtime::{RenderObjectsError, RetailRenderObject, RetailRuntime};

#[cfg(target_arch = "wasm32")]
mod app;
#[cfg(target_arch = "wasm32")]
mod assets;
#[cfg(any(target_arch = "wasm32", test))]
mod audio_output_metrics;
#[cfg(any(target_arch = "wasm32", test))]
mod browser_spawn;
#[cfg(any(target_arch = "wasm32", test))]
mod card_persistence;
#[cfg(target_arch = "wasm32")]
mod disc_import;
#[cfg(any(target_arch = "wasm32", test))]
mod display;
#[cfg(target_arch = "wasm32")]
mod dom;
#[cfg(any(target_arch = "wasm32", test))]
mod paging_boundary;
#[cfg(any(target_arch = "wasm32", test))]
mod pbak_runtime;
#[cfg(any(target_arch = "wasm32", test))]
pub mod renderer_backend;
#[cfg(any(target_arch = "wasm32", test))]
mod restart_transaction;
#[cfg(any(target_arch = "wasm32", test))]
mod retail_clock;
pub mod retail_scene;
#[cfg(target_arch = "wasm32")]
mod storage;
#[cfg(any(target_arch = "wasm32", test))]
mod title_runtime;
#[cfg(target_arch = "wasm32")]
mod webaudio;
#[cfg(any(target_arch = "wasm32", test))]
mod webgl;

/// Deterministic clock used only by the opt-in browser test harness.
///
/// Thirty-four milliseconds matches the fixed timing used by the legally local
/// native route characterizations. The ordinary browser build remains driven by
/// `requestAnimationFrame` and never compiles this clock into its application.
#[cfg(any(test, all(target_arch = "wasm32", feature = "browser-test-harness")))]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct BrowserTestClock {
    next_timestamp_ms: f64,
}

/// Cumulative recovery diagnostics exposed only by the deterministic browser
/// harness. Ordinary per-level runtime metrics reset on each stream mount;
/// these counters instead span the complete browser session.
#[cfg(any(test, all(target_arch = "wasm32", feature = "browser-test-harness")))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct BrowserTestCumulativeMetrics {
    /// Legacy schema-one counter: every observed `LevelRestart` call,
    /// including a call that names a different stream or later fails.
    pub(crate) hard_restarts: u64,
    /// Successful same-level restart transactions. Kept separate so adding a
    /// commit-boundary diagnostic cannot change schema-one checkpoints.
    pub(crate) completed_same_level_hard_restarts: u64,
    pub(crate) load_states: u64,
    pub(crate) death_camera_frames: u64,
}

#[cfg(any(test, all(target_arch = "wasm32", feature = "browser-test-harness")))]
impl BrowserTestCumulativeMetrics {
    pub(crate) fn record_hard_restart_call(&mut self) {
        self.hard_restarts = self.hard_restarts.saturating_add(1);
    }

    pub(crate) fn record_completed_same_level_hard_restart(&mut self) {
        self.completed_same_level_hard_restarts =
            self.completed_same_level_hard_restarts.saturating_add(1);
    }
}

/// Browser-side progress through the source `CoreFrame` tail for the currently
/// mounted stream. Kept outside `app` so the asynchronous mount ordering is
/// directly testable on native targets.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetailTickState {
    NeedsSpawn,
    /// A synchronous `LevelRestart` completed during the preceding source
    /// frame. The next `CoreFrame` must inspect the restored game state before
    /// respawning, while retaining the ordinary spawn/camera/GOOL tail when no
    /// transition is requested.
    RestartedNeedsSpawn,
    PausedBeforeSpawn,
    Running,
    Paused,
}

/// Source position at which a synchronous `LevelRestart` returns to
/// `CoreFrame`.
///
/// An ordinary GOOL or PBAK restart reaches the transition gate next. A
/// bonus-return restart is different: it runs inside the already-consumed
/// cross-stream transition branch, so native continues with that same
/// frame's outer spawn/camera/GOOL tail before it may inspect game state at
/// the following transition gate.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetailRestartResume {
    NextTransitionGate,
    SuspendedFrameTail,
}

#[cfg(any(target_arch = "wasm32", test))]
impl RetailRestartResume {
    pub(crate) const fn tick_state(self) -> RetailTickState {
        match self {
            Self::NextTransitionGate => RetailTickState::RestartedNeedsSpawn,
            Self::SuspendedFrameTail => RetailTickState::NeedsSpawn,
        }
    }
}

#[cfg(any(target_arch = "wasm32", test))]
impl RetailTickState {
    /// A synchronous source mount completes spawn, camera, and GOOL work in
    /// the preceding `CoreFrame`. An asynchronous browser destination may not
    /// consume a carried game-state transition before that tail has run.
    pub(crate) const fn can_resolve_core_frame_transition(self) -> bool {
        matches!(self, Self::RestartedNeedsSpawn | Self::Running)
    }

    /// Returns an explicit transition only at a source `CoreFrame` gate.
    ///
    /// The request remains owned by the runtime while an asynchronous mount is
    /// completing the suspended spawn/camera/GOOL tail. This applies equally
    /// to a concrete LID and native's `-2` saved-level sentinel.
    pub(crate) const fn explicit_core_frame_transition(self, next_lid: i32) -> Option<i32> {
        if self.can_resolve_core_frame_transition() && next_lid != -1 {
            Some(next_lid)
        } else {
            None
        }
    }
}

/// Reports whether one scheduler callback completed a source `CoreFrame` and
/// may therefore consume one browser-replay input sample.
///
/// Destination validation and pager work are asynchronous in the browser.
/// Those callbacks can advance host-side loading while the mounted retail
/// runtime still has no live main object; native blocks inside that same
/// source frame and does not consume another pad sample. Likewise, the
/// callback that consumes a transition requested by the preceding GOOL frame
/// only mounts the destination. A live retail main-frame tail, or an ordinary
/// non-retail flow frame, is the first completed source frame that may advance
/// replay input.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) const fn retail_source_frame_completed(
    retail_state: bool,
    transition_queued: bool,
    retail_pad_boundary_completed: bool,
) -> bool {
    if transition_queued {
        false
    } else if retail_state {
        retail_pad_boundary_completed
    } else {
        true
    }
}

/// Resolves the transition consumed at native's pre-spawn/pre-camera
/// `CoreFrame` gate.
///
/// A fatal same-level restart returns with no explicit `next_lid`, but its
/// restored game state requests Title. Keeping that fallback in the same
/// decision as explicit requests prevents the following camera update from
/// overwriting the carried Game Over/continue state first.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn retail_core_frame_transition_request(
    tick_state: RetailTickState,
    next_lid: i32,
    current_level: FormatLevelId,
    game_state: i32,
) -> Option<i32> {
    let next_lid = if next_lid == -1
        && current_level != FormatLevelId::TITLE
        && matches!(game_state, 0x200 | 0x300 | 0x400)
    {
        i32::try_from(FormatLevelId::TITLE.get()).expect("retail title level fits signed 32-bit")
    } else {
        next_lid
    };
    tick_state.explicit_core_frame_transition(next_lid)
}

/// Browser-visible half of the Stormy Ascent cut-content recovery gate.
///
/// The runtime performs the stronger direct-boot, object-program, state,
/// checkpoint, counter, and saved-level validation. This host-side check
/// preserves the one piece of provenance available only in the ordered frame
/// effects: `DispC` sent its exact completion event directly to the player in
/// the same frame that requested the invalid LID-zero fallthrough.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn stormy_cut_content_completion_recipient(
    current_level: FormatLevelId,
    requested_level: i32,
    target: SendEventTarget,
    event: u32,
    arguments: &[u32],
) -> Option<ObjectHandle> {
    if current_level.get() != 0x22
        || requested_level != 0
        || event != 0x0f00
        || arguments != [0x500]
    {
        return None;
    }
    let SendEventTarget::Direct { recipient } = target else {
        return None;
    };
    Some(recipient)
}

/// Ordered browser-side provenance for Stormy Ascent's missing-destination
/// recovery.
///
/// GOOL services a direct event synchronously before the sending object can
/// execute its following `LLEV`. Recipient-side work may therefore emit other
/// effects between the two instructions. Retain the most recent matching send
/// that the host has actually processed, then consume it at the next
/// transition. This deliberately cannot see a matching send that appears
/// later in the same frame.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct StormyCutContentEffectGate {
    pending: Option<(ObjectHandle, ObjectHandle)>,
}

#[cfg(any(target_arch = "wasm32", test))]
impl StormyCutContentEffectGate {
    pub(crate) fn observe_send(
        &mut self,
        current_level: FormatLevelId,
        sender: ObjectHandle,
        target: SendEventTarget,
        event: u32,
        arguments: &[u32],
    ) {
        if let Some(recipient) =
            stormy_cut_content_completion_recipient(current_level, 0, target, event, arguments)
        {
            self.pending = Some((sender, recipient));
        }
    }

    pub(crate) fn take_for_transition(
        &mut self,
        current_level: FormatLevelId,
        requested_level: i32,
    ) -> Option<(ObjectHandle, ObjectHandle)> {
        let pending = self.pending.take();
        if current_level.get() == 0x22 && requested_level == 0 {
            pending
        } else {
            None
        }
    }
}

/// Clear diagnostic shown when the exact host-only Stormy boundary is
/// recovered without inventing a missing Cortex bonus stream.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) const STORMY_CUT_CONTENT_RECOVERY_DIAGNOSTIC: &str = "Stormy Ascent's retail three-Cortex-token path requests nonexistent LID 0; returned to the Main Menu without inventing a bonus layout.";

/// `PbakChoose` runs before the destination's initial `LevelUpdate`. When a
/// title-attract destination has no usable recording it changes native's game
/// state from title (`0x600`) to cutscene (`0`) immediately; that makes the
/// subsequent null-origin `LevelUpdate` active and drains pending PSX pages.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) const fn retail_game_state_after_pbak_choose(
    game_state: i32,
    title_attract_mount: bool,
    playback_selected: bool,
) -> i32 {
    if title_attract_mount && !playback_selected {
        0
    } else {
        game_state
    }
}

/// Native `LevelUpdate` starts a null-origin zone change with local flag one,
/// except while the shared game state still denotes the title/attract flow.
/// Its raw `LdatInit` flags are zero there, so both initial neighbor activation
/// and the PSX `NSUpdate2` drain are suppressed.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) const fn retail_initial_level_update_is_active(game_state: i32) -> bool {
    game_state != 0x600
}

/// Reconstructs the path selected by `LdatInit` before its initial
/// null-origin `LevelUpdate`.
///
/// The Title stream normally uses its LDAT spawn (`0a_pZ`), but retail
/// substitutes the Game Over world (`0b_pZ:0`) when the carried game state is
/// `0x200`. A bonus return is applied afterward in native and therefore wins
/// over both ordinary choices.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn retail_ldat_initial_path(
    level: FormatLevelId,
    game_state: i32,
    ldat_spawn: RetailPathId,
    bonus_return: Option<RetailPathId>,
) -> RetailPathId {
    bonus_return.unwrap_or_else(|| {
        if level == FormatLevelId::TITLE && game_state == 0x200 {
            RetailPathId {
                zone: Eid::from_name("0b_pZ").expect("fixed Title Game Over EID is valid"),
                index: 0,
            }
        } else {
            ldat_spawn
        }
    })
}

#[cfg(any(test, all(target_arch = "wasm32", feature = "browser-test-harness")))]
impl BrowserTestClock {
    pub(crate) const FRAME_DURATION_MS: f64 = 34.0;

    pub(crate) fn take_timestamp_ms(&mut self) -> f64 {
        let timestamp = self.next_timestamp_ms;
        self.next_timestamp_ms += Self::FRAME_DURATION_MS;
        timestamp
    }
}

/// Input source accepted by the feature-only manually stepped browser harness.
///
/// Physical input deliberately remains a 16-bit console-controller mask.
/// Recorded input is a separate diagnostic path because native
/// `PadUpdatePbak` copies the complete 32-bit frame word into the pad state.
#[cfg(any(test, all(target_arch = "wasm32", feature = "browser-test-harness")))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BrowserTestPadInput {
    Physical(u16),
    Recorded(u32),
}

#[cfg(any(test, all(target_arch = "wasm32", feature = "browser-test-harness")))]
impl Default for BrowserTestPadInput {
    fn default() -> Self {
        Self::Physical(0)
    }
}

#[cfg(any(test, all(target_arch = "wasm32", feature = "browser-test-harness")))]
impl BrowserTestPadInput {
    pub(crate) fn physical(raw: u32) -> Result<Self, &'static str> {
        u16::try_from(raw)
            .map(Self::Physical)
            .map_err(|_| "held physical pad mask exceeds 16 bits")
    }

    #[must_use]
    pub(crate) const fn recorded(raw: u32) -> Self {
        Self::Recorded(raw)
    }

    #[must_use]
    pub(crate) const fn frame_input(self) -> (u16, Option<u32>) {
        match self {
            Self::Physical(held) => (held, None),
            Self::Recorded(held) => (0, Some(held)),
        }
    }

    #[must_use]
    pub(crate) const fn held_word(self) -> u32 {
        match self {
            Self::Physical(held) => held as u32,
            Self::Recorded(held) => held,
        }
    }

    #[must_use]
    pub(crate) const fn input_kind(self) -> &'static str {
        match self {
            Self::Physical(_) => "physical",
            Self::Recorded(_) => "recorded",
        }
    }
}

#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn initial_presented_path_point(
    point_count: core::num::NonZeroU16,
    after_loading_image: bool,
) -> usize {
    let desired = if after_loading_image { 2 } else { 1 };
    desired.min(usize::from(point_count.get() - 1))
}

/// Builds the presentation clock for an initial or destination stream mount.
///
/// `CoreFrame` arms two skipped draws for every authored stream transition.
/// A loading image changes the retained framebuffer contents, not that timing.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn retail_frame_for_mount(
    point_count: core::num::NonZeroU16,
    initial_progress: i32,
    draw_count: u32,
    core_transition: bool,
    loading_image_written: bool,
) -> crust_sim::retail_frame::RetailFrameState {
    use crust_sim::retail_frame::RetailFrameState;

    if core_transition {
        RetailFrameState::after_core_transition_with_draw_count(
            point_count,
            initial_progress,
            draw_count,
            loading_image_written,
        )
    } else if loading_image_written {
        RetailFrameState::after_loading_image_with_draw_count(
            point_count,
            initial_progress,
            draw_count,
        )
    } else {
        RetailFrameState::ready_with_draw_count(point_count, initial_progress, draw_count)
    }
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

/// Builds the browser launcher's direct-boot label without changing the
/// canonical retail catalog name used by runtime diagnostics and game state.
///
/// LID `0x26` is a valid mounted pair, but the owned retail data has no parent
/// selector or natural completion path. Keep it available for inspection while
/// making that limitation explicit before a user launches it.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn boot_level_option_label(level: FormatLevelId, name: &str) -> String {
    if level == FormatLevelId::TITLE {
        "Full game — from the beginning".to_owned()
    } else if level == FormatLevelId::new_const(0x26) {
        format!("{name} — {level} · dormant/unused retail bonus data; no natural completion")
    } else {
        format!("{name} — {level}")
    }
}

/// Native `GOD_MODE` access gate, deliberately separate from saved progress.
#[cfg(any(target_arch = "wasm32", test))]
const ALL_LEVELS_UNLOCK_GATE: u32 = 99;

/// The source project's native `GOD_MODE` initial-life value in retail 24.8
/// fixed-point units.
#[cfg(any(target_arch = "wasm32", test))]
const ALL_LEVELS_MAX_LIVES: u32 = 999 << 8;

/// The two native `item_pool2` flags that expose the key-gated secret paths.
#[cfg(any(target_arch = "wasm32", test))]
const ALL_LEVELS_SECRET_PATH_BITS: u32 = (1 << 10) | (1 << 20);

/// Applies the source project's `GOD_MODE` access gates without moving the
/// saved island-map cursor or fabricating gems, keys, and options.
///
/// The secret-path flags are `ORed` into the live pool so collected state is not
/// discarded. The first application after boot or card restore also installs
/// `GOD_MODE`'s 999-life starting count. Later applications recognize that the
/// initial count is already armed and do not replenish lives lost in play. The
/// browser host keeps the resulting card/resume writes in-memory for this
/// launch.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn apply_all_levels_override(runtime: &mut RetailRuntime) -> Result<(), VmError> {
    let item_pool_2 = runtime.global_word(ITEM_POOL_2_GLOBAL)?;
    let initial_lives = runtime.global_word(INITIAL_LIFE_COUNT_GLOBAL)?;
    runtime.set_global_word(LEVELS_UNLOCKED_GLOBAL, ALL_LEVELS_UNLOCK_GATE)?;
    runtime.set_global_word(
        ITEM_POOL_2_GLOBAL,
        item_pool_2 | ALL_LEVELS_SECRET_PATH_BITS,
    )?;
    if initial_lives != ALL_LEVELS_MAX_LIVES {
        runtime.set_global_word(INITIAL_LIFE_COUNT_GLOBAL, ALL_LEVELS_MAX_LIVES)?;
        runtime.set_global_word(LIFE_COUNT_GLOBAL, ALL_LEVELS_MAX_LIVES)?;
    }
    Ok(())
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

/// Exposes the VM's raw title-state global for exact replay checkpoints.
///
/// The passive browser-flow mirror remains a presentation fallback only. In
/// direct gameplay boots it can still name the publisher screen while the
/// retained process global correctly remains state seven.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn retail_debug_title_state(runtime: &RetailRuntime, fallback: u32) -> u32 {
    runtime.global_word(TITLE_STATE_GLOBAL).unwrap_or(fallback)
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
    #[allow(clippy::float_cmp)] // The deterministic test clock emits these exact integer values.
    fn browser_test_clock_issues_one_fixed_step_per_request() {
        let mut clock = BrowserTestClock::default();

        assert_eq!(clock.take_timestamp_ms(), 0.0);
        assert_eq!(clock.take_timestamp_ms(), 34.0);
        assert_eq!(clock.take_timestamp_ms(), 68.0);
    }

    #[test]
    fn browser_replay_advances_only_after_a_completed_source_core_frame() {
        assert!(!retail_source_frame_completed(true, false, false));
        assert!(retail_source_frame_completed(true, false, true));
        assert!(retail_source_frame_completed(false, false, false));

        assert!(
            !retail_source_frame_completed(true, true, true),
            "the callback that consumes a prior GOOL transition only mounts the destination"
        );
        assert!(
            !retail_source_frame_completed(false, true, false),
            "a queued transition never consumes destination input"
        );
    }

    #[test]
    fn replay_debug_prefers_the_raw_process_title_state_over_the_flow_mirror() {
        let runtime = RetailRuntime::new_for_level(256, FormatLevelId::new_const(0x22));

        assert_eq!(retail_debug_title_state(&runtime, 10), 7);
        assert_eq!(retail_debug_title_state(&RetailRuntime::new(0), 10), 10);
    }

    #[test]
    fn direct_boot_labels_only_annotate_the_dormant_retail_bonus() {
        for level in crust_formats::stream::KNOWN_LEVELS {
            let label = boot_level_option_label(level.id, level.name);
            if level.id == FormatLevelId::TITLE {
                assert_eq!(label, "Full game — from the beginning");
            } else if level.id == FormatLevelId::new_const(0x26) {
                assert_eq!(level.name, "Bonus", "the canonical catalog stays unchanged");
                assert_eq!(
                    label,
                    "Bonus — 0x26 · dormant/unused retail bonus data; no natural completion"
                );
            } else {
                assert_eq!(label, format!("{} — {}", level.name, level.id));
            }
        }
    }

    #[test]
    fn browser_hard_restart_metrics_preserve_calls_and_separate_same_level_commits() {
        let mut metrics = BrowserTestCumulativeMetrics::default();

        metrics.record_hard_restart_call();
        metrics.record_completed_same_level_hard_restart();
        assert_eq!(
            metrics.hard_restarts, 1,
            "an ordinary death contributes one LevelRestart call"
        );
        assert_eq!(
            metrics.completed_same_level_hard_restarts, 1,
            "the ordinary death completes its same-level restore"
        );

        metrics.record_hard_restart_call();
        assert_eq!(
            metrics.hard_restarts, 2,
            "a bonus's different-level LevelRestart call remains observable"
        );
        assert_eq!(
            metrics.completed_same_level_hard_restarts, 1,
            "a different-level call is not a completed same-level restore"
        );

        metrics.record_hard_restart_call();
        metrics.record_completed_same_level_hard_restart();
        assert_eq!(
            metrics.hard_restarts, 3,
            "the protected parent mount contributes the third call"
        );
        assert_eq!(
            metrics.completed_same_level_hard_restarts, 2,
            "the protected mount is the second completed same-level restore"
        );
    }

    #[test]
    fn game_state_transition_waits_for_the_post_mount_frame_tail() {
        assert!(!RetailTickState::NeedsSpawn.can_resolve_core_frame_transition());
        assert!(RetailTickState::RestartedNeedsSpawn.can_resolve_core_frame_transition());
        assert!(!RetailTickState::PausedBeforeSpawn.can_resolve_core_frame_transition());
        assert!(RetailTickState::Running.can_resolve_core_frame_transition());
        assert!(!RetailTickState::Paused.can_resolve_core_frame_transition());
    }

    #[test]
    fn bonus_return_restart_resumes_the_suspended_mount_frame_tail() {
        let ordinary = RetailRestartResume::NextTransitionGate.tick_state();
        assert_eq!(ordinary, RetailTickState::RestartedNeedsSpawn);
        assert!(ordinary.can_resolve_core_frame_transition());

        let bonus_return = RetailRestartResume::SuspendedFrameTail.tick_state();
        assert_eq!(bonus_return, RetailTickState::NeedsSpawn);
        assert!(
            !bonus_return.can_resolve_core_frame_transition(),
            "the carried 0x300 continue state must survive until the suspended parent frame's camera/GOOL tail has run"
        );
        for request in [-2, 0x09] {
            assert_eq!(bonus_return.explicit_core_frame_transition(request), None);
            assert_eq!(
                RetailTickState::Running.explicit_core_frame_transition(request),
                Some(request),
                "the same request becomes consumable at the following CoreFrame gate"
            );
        }
    }

    #[test]
    fn restarted_last_life_requests_title_before_the_camera_tail() {
        let gameplay = FormatLevelId::new_const(0x0c);
        let mut camera_tail_ran = false;
        let requested = retail_core_frame_transition_request(
            RetailTickState::RestartedNeedsSpawn,
            -1,
            gameplay,
            0x200,
        );
        if requested.is_none() {
            camera_tail_ran = true;
        }
        assert_eq!(
            requested,
            Some(i32::try_from(FormatLevelId::TITLE.get()).unwrap())
        );
        assert!(
            !camera_tail_ran,
            "the fatal restart must queue Title before spawn/camera can rewrite GAME_STATE_GAMEOVER"
        );

        assert_eq!(
            retail_core_frame_transition_request(RetailTickState::NeedsSpawn, -1, gameplay, 0x200,),
            None,
            "a bonus-return mount still owes its suspended frame tail"
        );
        assert_eq!(
            retail_core_frame_transition_request(
                RetailTickState::RestartedNeedsSpawn,
                -1,
                FormatLevelId::TITLE,
                0x200,
            ),
            None,
            "the implicit fallback is only a non-title transition"
        );
    }

    #[test]
    fn browser_stormy_recovery_requires_the_exact_completion_effect() {
        let stormy = FormatLevelId::new_const(0x22);
        let recipient = ObjectHandle::new(7).unwrap();
        let direct = SendEventTarget::Direct { recipient };

        assert_eq!(
            stormy_cut_content_completion_recipient(stormy, 0, direct, 0x0f00, &[0x500]),
            Some(recipient)
        );
        for (label, level, requested, target, event, arguments) in [
            (
                "other level",
                FormatLevelId::N_SANITY_BEACH,
                0,
                direct,
                0x0f00,
                &[0x500][..],
            ),
            ("nonzero transition", stormy, 1, direct, 0x0f00, &[0x500]),
            ("wrong event", stormy, 0, direct, 0x0e00, &[0x500]),
            ("wrong argument", stormy, 0, direct, 0x0f00, &[0x400]),
            ("extra argument", stormy, 0, direct, 0x0f00, &[0x500, 0]),
            (
                "broadcast target",
                stormy,
                0,
                SendEventTarget::AllRoots { mode: 0 },
                0x0f00,
                &[0x500],
            ),
        ] {
            assert_eq!(
                stormy_cut_content_completion_recipient(level, requested, target, event, arguments,),
                None,
                "{label} must not classify the cut-content boundary"
            );
        }
        assert!(STORMY_CUT_CONTENT_RECOVERY_DIAGNOSTIC.contains("nonexistent LID 0"));
        assert!(STORMY_CUT_CONTENT_RECOVERY_DIAGNOSTIC.contains("Main Menu"));
    }

    #[test]
    fn browser_stormy_recovery_uses_only_processed_effect_order() {
        let stormy = FormatLevelId::new_const(0x22);
        let sender = ObjectHandle::new(6).unwrap();
        let recipient = ObjectHandle::new(7).unwrap();
        let direct = SendEventTarget::Direct { recipient };
        let mut gate = StormyCutContentEffectGate::default();

        assert_eq!(
            gate.take_for_transition(stormy, 0),
            None,
            "a future same-frame send cannot classify an earlier transition"
        );
        gate.observe_send(stormy, sender, direct, 0x0f00, &[0x500]);
        assert_eq!(
            gate.take_for_transition(stormy, 0),
            Some((sender, recipient))
        );
        assert_eq!(
            gate.take_for_transition(stormy, 0),
            None,
            "one completion send can classify only its following transition"
        );
    }

    #[test]
    fn browser_stormy_recovery_prefers_the_latest_matching_send() {
        let stormy = FormatLevelId::new_const(0x22);
        let first_sender = ObjectHandle::new(4).unwrap();
        let first_recipient = ObjectHandle::new(5).unwrap();
        let authentic_sender = ObjectHandle::new(6).unwrap();
        let authentic_recipient = ObjectHandle::new(7).unwrap();
        let mut gate = StormyCutContentEffectGate::default();

        gate.observe_send(
            stormy,
            first_sender,
            SendEventTarget::Direct {
                recipient: first_recipient,
            },
            0x0f00,
            &[0x500],
        );
        // A recipient-side unrelated send may occur while the first event is
        // serviced; it must neither erase nor manufacture provenance.
        gate.observe_send(
            stormy,
            first_sender,
            SendEventTarget::AllRoots { mode: 0 },
            0x0f00,
            &[0x500],
        );
        gate.observe_send(
            stormy,
            authentic_sender,
            SendEventTarget::Direct {
                recipient: authentic_recipient,
            },
            0x0f00,
            &[0x500],
        );
        assert_eq!(
            gate.take_for_transition(stormy, 0),
            Some((authentic_sender, authentic_recipient))
        );

        gate.observe_send(
            stormy,
            authentic_sender,
            SendEventTarget::Direct {
                recipient: authentic_recipient,
            },
            0x0f00,
            &[0x500],
        );
        assert_eq!(gate.take_for_transition(stormy, 1), None);
        assert_eq!(
            gate.take_for_transition(stormy, 0),
            None,
            "a nonzero transition consumes stale completion provenance"
        );
    }

    #[test]
    fn initial_level_update_suppresses_activation_only_for_title_attract_state() {
        assert!(retail_initial_level_update_is_active(0));
        assert!(retail_initial_level_update_is_active(0x100));
        assert!(!retail_initial_level_update_is_active(0x600));
    }

    #[test]
    fn title_game_over_replaces_the_ldat_spawn_before_initial_level_update() {
        let ldat_spawn = RetailPathId {
            zone: Eid::from_name("0a_pZ").unwrap(),
            index: 3,
        };

        let selected = retail_ldat_initial_path(FormatLevelId::TITLE, 0x200, ldat_spawn, None);

        assert_eq!(selected.zone, Eid::from_name("0b_pZ").unwrap());
        assert_eq!(selected.index, 0);
        assert_eq!(
            retail_ldat_initial_path(FormatLevelId::TITLE, 0, ldat_spawn, None),
            ldat_spawn,
            "other Title states retain the serialized LDAT spawn"
        );
        assert_eq!(
            retail_ldat_initial_path(FormatLevelId::N_SANITY_BEACH, 0x200, ldat_spawn, None,),
            ldat_spawn,
            "the game-over override is scoped to the Title stream"
        );
    }

    #[test]
    fn bonus_return_path_wins_over_the_title_game_over_override() {
        let ldat_spawn = RetailPathId {
            zone: Eid::from_name("0a_pZ").unwrap(),
            index: 0,
        };
        let saved_path = RetailPathId {
            zone: Eid::from_name("saveZ").unwrap(),
            index: 7,
        };

        assert_eq!(
            retail_ldat_initial_path(FormatLevelId::TITLE, 0x200, ldat_spawn, Some(saved_path),),
            saved_path,
            "native applies bonus_return after its Title-specific LDAT write"
        );
    }

    #[test]
    fn missing_title_attract_recording_activates_the_initial_level_update() {
        let selected = retail_game_state_after_pbak_choose(0x600, true, true);
        assert_eq!(selected, 0x600);
        assert!(!retail_initial_level_update_is_active(selected));

        let unavailable = retail_game_state_after_pbak_choose(0x600, true, false);
        assert_eq!(unavailable, 0);
        assert!(retail_initial_level_update_is_active(unavailable));

        assert_eq!(
            retail_game_state_after_pbak_choose(0x300, false, false),
            0x300
        );
    }

    #[test]
    fn browser_test_recorded_input_preserves_full_words_without_widening_physical_input() {
        use crust_platform::input::{PadState, TAP_MASK};

        assert_eq!(
            BrowserTestPadInput::physical(u32::from(u16::MAX)),
            Ok(BrowserTestPadInput::Physical(u16::MAX))
        );
        assert_eq!(
            BrowserTestPadInput::physical(u32::from(u16::MAX) + 1),
            Err("held physical pad mask exceeds 16 bits")
        );

        let recorded = BrowserTestPadInput::recorded(u32::MAX);
        assert_eq!(recorded.frame_input(), (0, Some(u32::MAX)));
        assert_eq!(recorded.held_word(), u32::MAX);
        assert_eq!(recorded.input_kind(), "recorded");

        let mut pad = PadState::default();
        let (physical, demo_override) = recorded.frame_input();
        pad.update(physical, 0, demo_override);
        assert_eq!(pad.snapshot().held, u32::MAX);
        assert_eq!(pad.snapshot().tapped, u32::from(TAP_MASK));

        let released = BrowserTestPadInput::physical(0).unwrap();
        let (physical, demo_override) = released.frame_input();
        pad.update(physical, 0, demo_override);
        assert_eq!(pad.snapshot().held, 0);
        assert_eq!(
            pad.snapshot().held_previous,
            u32::MAX,
            "switching back to ordinary physical input must retain pad history"
        );
    }

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
    fn all_levels_override_preserves_progress_and_adds_secret_access_and_max_lives() {
        let original = SaveData {
            level_count: 7,
            initial_lives: 9 << 8,
            unknown_6190c: 0x1234,
            mono: true,
            sfx_volume: 173,
            music_volume: 211,
            item_pool_1: 0x1122_3344,
            item_pool_2: 0x5566_7788,
            gem_count: 4,
            key_count: 1,
        };
        let mut runtime =
            RetailRuntime::new_for_level(256, crust_formats::stream::LevelId::N_SANITY_BEACH);
        runtime.restore_card_save_data(original).unwrap();
        apply_all_levels_override(&mut runtime).unwrap();

        assert_eq!(
            runtime.global_word(LEVELS_UNLOCKED_GLOBAL),
            Ok(ALL_LEVELS_UNLOCK_GATE)
        );
        assert_eq!(
            runtime.global_word(crust_sim::gool::CURRENT_MAP_LEVEL_GLOBAL),
            Ok(original.level_count),
            "unlocking access must not move the player's island-map cursor"
        );

        let unlocked = runtime.card_save_data().unwrap();
        assert_eq!(
            unlocked.item_pool_2,
            original.item_pool_2 | ALL_LEVELS_SECRET_PATH_BITS
        );
        assert_eq!(
            SaveData {
                initial_lives: original.initial_lives,
                item_pool_2: original.item_pool_2,
                ..unlocked
            },
            original,
            "the launcher option must preserve real progress, collectibles, and options"
        );
        assert_eq!(unlocked.initial_lives, ALL_LEVELS_MAX_LIVES);
        assert_eq!(
            runtime.global_word(LIFE_COUNT_GLOBAL),
            Ok(ALL_LEVELS_MAX_LIVES)
        );

        runtime
            .set_global_word(LIFE_COUNT_GLOBAL, ALL_LEVELS_MAX_LIVES - (1 << 8))
            .unwrap();
        apply_all_levels_override(&mut runtime).unwrap();
        assert_eq!(
            runtime.global_word(LIFE_COUNT_GLOBAL),
            Ok(ALL_LEVELS_MAX_LIVES - (1 << 8)),
            "reapplying the access gate must not replenish a life lost during play"
        );

        let loaded = SaveData {
            level_count: 3,
            item_pool_2: 0,
            ..original
        };
        runtime.restore_card_save_data(loaded).unwrap();
        apply_all_levels_override(&mut runtime).unwrap();
        assert_eq!(runtime.global_word(LEVELS_UNLOCKED_GLOBAL), Ok(99));
        assert_eq!(
            runtime.global_word(crust_sim::gool::CURRENT_MAP_LEVEL_GLOBAL),
            Ok(3),
            "reapplying after a card load must retain the loaded map cursor"
        );
        assert_eq!(
            runtime.global_word(ITEM_POOL_2_GLOBAL),
            Ok(ALL_LEVELS_SECRET_PATH_BITS),
            "both key-gated paths must remain available after a card load"
        );
        assert_eq!(
            runtime.global_word(INITIAL_LIFE_COUNT_GLOBAL),
            Ok(ALL_LEVELS_MAX_LIVES)
        );
        assert_eq!(
            runtime.global_word(LIFE_COUNT_GLOBAL),
            Ok(ALL_LEVELS_MAX_LIVES),
            "a card restore starts the temporary max-lives session again"
        );

        apply_all_levels_override(&mut runtime).unwrap();
        assert_eq!(
            runtime.global_word(ITEM_POOL_2_GLOBAL),
            Ok(ALL_LEVELS_SECRET_PATH_BITS),
            "the access override must be idempotent"
        );

        let carry = runtime.export_session_carry();
        let mut mounted =
            RetailRuntime::new_from_session(256, crust_formats::stream::LevelId::TITLE, carry)
                .unwrap();
        assert_eq!(mounted.global_word(LEVELS_UNLOCKED_GLOBAL), Ok(99));
        assert_eq!(
            mounted.global_word(crust_sim::gool::CURRENT_MAP_LEVEL_GLOBAL),
            Ok(3)
        );
        assert_eq!(
            mounted.global_word(ITEM_POOL_2_GLOBAL),
            Ok(ALL_LEVELS_SECRET_PATH_BITS)
        );
        assert_eq!(
            mounted.global_word(INITIAL_LIFE_COUNT_GLOBAL),
            Ok(ALL_LEVELS_MAX_LIVES)
        );
        assert_eq!(
            mounted.global_word(LIFE_COUNT_GLOBAL),
            Ok(ALL_LEVELS_MAX_LIVES)
        );

        mounted.reset_level_globals().unwrap();
        let reset_level_count = mounted
            .global_word(crust_sim::gool::LEVEL_COUNT_GLOBAL)
            .unwrap();
        let reset_map_level = mounted
            .global_word(crust_sim::gool::CURRENT_MAP_LEVEL_GLOBAL)
            .unwrap();
        apply_all_levels_override(&mut mounted).unwrap();
        assert_eq!(
            mounted.global_word(crust_sim::gool::LEVEL_COUNT_GLOBAL),
            Ok(reset_level_count)
        );
        assert_eq!(
            mounted.global_word(crust_sim::gool::CURRENT_MAP_LEVEL_GLOBAL),
            Ok(reset_map_level)
        );
        assert_eq!(mounted.global_word(LEVELS_UNLOCKED_GLOBAL), Ok(99));
        assert_eq!(
            mounted.global_word(ITEM_POOL_2_GLOBAL),
            Ok(ALL_LEVELS_SECRET_PATH_BITS)
        );
        assert_eq!(
            mounted.global_word(LIFE_COUNT_GLOBAL),
            Ok(ALL_LEVELS_MAX_LIVES)
        );
    }

    #[test]
    fn native_fortress_to_level_complete_core_mount_skips_without_an_image() {
        use crust_sim::retail_frame::PresentedFrame;

        let mut frame =
            retail_frame_for_mount(core::num::NonZeroU16::new(72).unwrap(), 0, 41, true, false);
        assert_eq!(frame.draw_skip(), 2);

        let hidden = frame.tick();
        assert_eq!(hidden.presented(), PresentedFrame::None);
        assert_eq!(hidden.draw_skip(), 1);
        assert_eq!(hidden.draw_count(), 42);

        let visible = frame.tick();
        assert!(matches!(
            visible.presented(),
            PresentedFrame::Gameplay { .. }
        ));
        assert_eq!(visible.draw_skip(), 0);
        assert_eq!(visible.draw_count(), 43);
    }

    #[test]
    fn level_complete_to_title_core_mount_retains_its_loading_image() {
        use crust_sim::retail_frame::PresentedFrame;

        let mut frame =
            retail_frame_for_mount(core::num::NonZeroU16::new(72).unwrap(), 0, 91, true, true);
        assert_eq!(frame.draw_skip(), 2);

        let hidden = frame.tick();
        assert_eq!(hidden.presented(), PresentedFrame::LoadingImage);
        assert_eq!(hidden.draw_skip(), 1);
        assert_eq!(hidden.draw_count(), 92);

        let visible = frame.tick();
        assert!(matches!(
            visible.presented(),
            PresentedFrame::Gameplay { .. }
        ));
        assert_eq!(visible.draw_skip(), 0);
        assert_eq!(visible.draw_count(), 93);
    }

    #[test]
    #[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
    fn local_native_completion_destinations_have_the_characterized_image_contract() {
        use std::path::PathBuf;

        use crust_formats::stream::{LevelId, StreamKind, StreamName, parse_nsd};

        let root = PathBuf::from(
            std::env::var_os("C1_STREAM_DIR")
                .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
        );
        let has_loading_image = |level| {
            let path = root.join(StreamName::new(level, StreamKind::Nsd).filename());
            let bytes = std::fs::read(&path)
                .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()));
            let nsd = parse_nsd(&bytes, level)
                .unwrap_or_else(|error| panic!("could not parse {}: {error}", path.display()));
            nsd.image_data(&bytes)
                .unwrap_or_else(|error| panic!("{} loading image: {error}", path.display()))
                .is_some()
        };

        let native_to_complete = (LevelId::new_const(0x1a), LevelId::LEVEL_COMPLETE);
        let complete_to_title = (LevelId::LEVEL_COMPLETE, LevelId::TITLE);
        assert!(
            !has_loading_image(native_to_complete.1),
            "{} -> {} must use the no-image transition path",
            native_to_complete.0,
            native_to_complete.1,
        );
        assert!(
            has_loading_image(complete_to_title.1),
            "{} -> {} must retain the decoded loading image",
            complete_to_title.0,
            complete_to_title.1,
        );

        let point_count = core::num::NonZeroU16::new(72).unwrap();
        let native_frame = retail_frame_for_mount(point_count, 0, 0, true, false);
        let title_frame = retail_frame_for_mount(point_count, 0, 0, true, true);
        assert_eq!(native_frame.draw_skip(), 2);
        assert_eq!(title_frame.draw_skip(), 2);
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
    fn title_mount_keeps_the_source_pad_snapshot_until_deferred_core_objects_update() {
        use crust_platform::input::{PAD_CROSS, PAD_START, PadState};

        let mut pad = PadState::default();
        pad.update(PAD_CROSS, 0, None);
        let source_snapshot = pad.snapshot();

        // TitleLoadNextState and its MDAT initializers run before
        // CoreObjectsCreate. Importing the source snapshot must not shift the
        // history or synthesize the destination's pending START edge early.
        let title_init_snapshot = pad.snapshot();
        assert_eq!(title_init_snapshot, source_snapshot);
        assert_eq!(title_init_snapshot.held, u32::from(PAD_CROSS));
        assert_eq!(title_init_snapshot.held_previous, 0);

        let core_objects_snapshot = core_objects_pad_update(&mut pad, PAD_START, 0, None);
        assert_eq!(core_objects_snapshot.held, u32::from(PAD_START));
        assert_eq!(core_objects_snapshot.tapped, u32::from(PAD_START));
        assert_eq!(core_objects_snapshot.held_previous, u32::from(PAD_CROSS));
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
