//! Browser application controller. All engine decisions remain in Rust.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::missing_errors_doc,
    clippy::too_many_lines
)]

use core::num::NonZeroU16;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::rc::Rc;

use crust_audio::output::{OutputOptions, RetailMasterFade};
use crust_audio::retail::{RetailAudioEngine, retail_max_midi_voices};
use crust_audio::retail_music::RetailMusic;
use crust_audio::retail_player::RetailMusicChange;
use crust_formats::binary::{Eid, FormatError};
use crust_formats::stream::{
    KNOWN_LEVELS, LevelId as FormatLevelId, Nsd, Nsf, ObjectVertexKind, RetailPathId,
    RetailZoneGraph, ZoneEntity, ZoneGraphics, ZoneHeader, load_title_mdat, parse_instrument_entry,
    parse_retail_midi, title_mdat_eid,
};
use crust_platform::input::{
    PAD_START, PadSnapshot as PlatformPadSnapshot, PadState as PlatformPadState, keyboard_code,
    mouse_button, standard_gamepad,
};
use crust_renderer::texture::{DecodedTexture, decode_loading_image};
use crust_renderer::title::decode_title_card;
use crust_sim::Vec3;
use crust_sim::camera::{
    RetailCameraEffect, RetailCameraFollowInput, RetailCameraInput, RetailCameraLocation,
    RetailCameraPose, RetailCameraRuntime, RetailCameraStep, RetailDeathCameraInput,
    RetailDeathCameraState, RetailIslandCameraInput,
};
use crust_sim::card::{
    CardOperation, CardOutcome, ResumeLoadResult, ResumeManager, SaveData, VirtualCard,
};
use crust_sim::demo::DemoEnd;
use crust_sim::flow::{
    FlowState, GameOptions, LevelId, RetailFlowEvent, RetailFlowMirror, TitleScreen,
};
use crust_sim::gool::{
    AudioHostRequest, AudioHostResponse, CURRENT_MAP_LEVEL_GLOBAL, CardHostRequest,
    GAME_STATE_GLOBAL, GEM_COUNT_GLOBAL, INITIAL_DISPLAY_MASK, ITEM_POOL_1_GLOBAL,
    ITEM_POOL_2_GLOBAL, KEY_COUNT_GLOBAL, LEVEL_COUNT_GLOBAL, LEVELS_UNLOCKED_GLOBAL, MONO_GLOBAL,
    MUSIC_VOLUME_GLOBAL, ModelVertexSource, NEXT_DISPLAY_GLOBAL, PagingHostOperation,
    PagingHostRequest, PagingHostResponse, RetailPadSnapshot, RetailSolidEnvironment,
    RetailTransformVectorsCamera, SFX_VOLUME_GLOBAL, TITLE_STATE_GLOBAL, VmEffect, VmObject,
    VmStateProgram, process_register,
};
use crust_sim::object_arena::NeighborZone;
use crust_sim::object_bounds::AnimationBoundSource;
use crust_sim::paging::{
    PageInvalidations, Pager, PagerCloseOutcome, PagerOpenOutcome, PagerUpdateOutcome, PagingError,
    TextureFrameSnapshot,
};
use crust_sim::retail_frame::{PresentedFrame, RetailFrameState};
use crust_sim::retail_runtime::{
    AnimationBoundBinding, CardHostResponse, ISLAND_CAMERA_ROTATION_GLOBAL,
    ISLAND_CAMERA_STATE_GLOBAL, ModelVertexBinding, NsfProgramError, NsfProgramHost,
    ProgramBinding, ProgramHost, RetailCoreObjects, RetailDemoFinishOutcome,
    RetailLevelStateContext, RetailPauseUpdate, RetailRestartOutcome, RetailRuntime,
    RetailSaveStateOutcome, RetailSessionCarry, RetailThunderCue, RetailTitleAction,
    RetailTraversalBoundary, RetailZoneEnvironment, RuntimeCleanupAction, RuntimeError,
    RuntimeFrame, RuntimeObjectHandle, StateProgramBinding, ZoneTerminationMode,
};
use crust_sim::scheduler::{FRAME_US, FrameDecision, FrameScheduler};
use crust_sim::zone_lifecycle::{
    OrderedZoneLoadList, ZONE_OBJECTS_ACTIVE, ZoneLifecycle, ZoneLifecycleZone,
    ZoneTransitionAction,
};
use js_sys::{Function, Object, Promise, Reflect};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    DragEvent, Element, Event, FileList, Gamepad, HtmlElement, HtmlOptionElement, KeyboardEvent,
    MouseEvent, PointerEvent,
};

use crate::assets::{AssetStore, ValidatedPair};
use crate::browser_spawn::{
    BrowserProgramError, BrowserSpawnCounts, browser_spawn_counts, first_unexpected_browser_spawn,
};
use crate::card_persistence::CardPersistIntent;
use crate::disc_import::discover_disc;
use crate::display::DisplaySettings;
use crate::dom::{Dom, window};
use crate::pbak_runtime::{
    PbakFrameTiming, RetailPbakPlayback, pbak_event_pad_snapshot, prepare_pair_pbak,
};
use crate::retail_clock::advance_retail_shader_clock;
use crate::retail_scene::{
    RetailMapPathAnimation, RetailSceneBuilder, RetailScenePresentation,
    RetailSceneProgressLocation, RetailWorldShaderRenderState,
};
use crate::storage::StorageState;
use crate::title_runtime::{
    RETAIL_CAMERA_UPDATE, RETAIL_ZONE_OBJECTS_ACTIVE, RetailTitleScreenProfile,
    RetailTitleScreenType, retail_title_mdat_binding, retail_title_overlay_alpha,
    retail_title_screen_profile, title_mdat_entity_is_unlocked, title_state_number_uses_image,
};
use crate::webaudio::WebAudio;
use crate::webgl::{GlStage, RetailBackgroundFill, VisualState};
#[cfg(feature = "browser-test-harness")]
use crate::{BrowserTestClock, BrowserTestPadInput};
use crate::{
    RetailIslandWritebackPhase, apply_all_levels_override, authoritative_save_or_last,
    core_objects_pad_update, initial_retail_level_state, require_render_object_snapshot,
    retail_island_state_writeback,
};
#[cfg(feature = "browser-test-harness")]
use crust_sim::gool::{INITIAL_LIFE_COUNT_GLOBAL, LIFE_COUNT_GLOBAL};
#[cfg(feature = "browser-test-harness")]
use crust_sim::retail_runtime::BrowserTestLiveObject;

// `web-sys` 0.3 exposes `Element::request_fullscreen` as a caught void
// method, even though current browsers return a Promise. Read that return
// value explicitly so a platform or readiness rejection is observed instead
// of surfacing as an unhandled browser-console failure.
fn request_fullscreen_promise(element: &Element) -> Result<Promise, JsValue> {
    let method = Reflect::get(element.as_ref(), &JsValue::from_str("requestFullscreen"))?
        .dyn_into::<Function>()?;
    method.call0(element.as_ref())?.dyn_into::<Promise>()
}

const ZDAT_ENTRY_TYPE: u32 = 7;
const ADIO_ENTRY_TYPE: u32 = 12;
const RETAIL_GLOBAL_WORDS: usize = 256;
const RETAIL_INSTRUCTION_BUDGET: usize = 67;
const BOX_COUNT_GLOBAL: usize = 62;
const CHECKPOINT_ID_GLOBAL: usize = 69;
const CHECKPOINT_TRANSLATION_GLOBALS: [usize; 3] = [102, 103, 104];
const MAP_LEVEL_LINKS_GLOBAL: usize = 73;
const MAP_KEY_LINKS_GLOBAL: usize = 75;
const PBAK_STATE_GLOBAL: usize = 105;
const TITLE_DIRECT_ZONE_NAMES: [&str; 10] = [
    "0a_pZ", "0b_pZ", "0c_pZ", "0d_pZ", "0e_pZ", "0f_pZ", "1a_pZ", "1e_pZ", "2b_pZ", "3a_pZ",
];

fn retail_zone_graph(pair: &ValidatedPair) -> Result<RetailZoneGraph, FormatError> {
    let direct_roots = if pair.level == FormatLevelId::TITLE {
        TITLE_DIRECT_ZONE_NAMES
            .iter()
            .map(|name| Eid::from_name(name).expect("fixed title zone EID is valid"))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    RetailZoneGraph::from_pair_with_roots(&pair.nsd, &pair.nsf, &pair.nsf_bytes, direct_roots)
}

fn retail_screen_projection(field_of_view: u32) -> Option<u32> {
    match field_of_view {
        30 => Some(960),
        37 => Some(800),
        55 => Some(500),
        60 => Some(460),
        90 => Some(288),
        _ => None,
    }
}

fn round_retail_ticks(ticks: i32) -> i32 {
    match ticks {
        0..=18 => 17,
        ..0 | 19..=35 => 34,
        36..=52 => 51,
        _ => ticks,
    }
}

pub fn boot() -> Result<(), JsValue> {
    let dom = Dom::find()?;
    let display_settings = dom.display_settings();
    dom.apply_display_settings(display_settings)?;
    dom.sync_canvas_resolution(display_settings);
    let debug = Object::new();
    let browser_window = window()?;
    Reflect::set(
        browser_window.as_ref(),
        &JsValue::from_str("__crustDebug"),
        &debug,
    )?;
    let storage = StorageState::open().ok();
    let app = Rc::new(RefCell::new(App {
        dom,
        assets: AssetStore::default(),
        storage,
        runtime: None,
        keyboard_bits: 0,
        active_keys: HashMap::new(),
        active_mouse_buttons: HashMap::new(),
        active_touches: HashMap::new(),
        busy: false,
        locked: false,
        muted: false,
        display_settings,
        debug,
        #[cfg(feature = "browser-test-harness")]
        browser_test_input: BrowserTestPadInput::default(),
    }));
    bind_events(&app)?;
    app.borrow_mut().refresh_assets()?;
    #[cfg(feature = "browser-test-harness")]
    install_browser_test_harness(&app, &browser_window)?;
    #[cfg(not(feature = "browser-test-harness"))]
    start_animation_loop(&app)?;
    Ok(())
}

struct App {
    dom: Dom,
    assets: AssetStore,
    storage: Option<StorageState>,
    runtime: Option<Runtime>,
    keyboard_bits: u16,
    active_keys: HashMap<String, u16>,
    active_mouse_buttons: HashMap<i16, u16>,
    active_touches: HashMap<i32, u16>,
    busy: bool,
    locked: bool,
    muted: bool,
    display_settings: DisplaySettings,
    debug: Object,
    #[cfg(feature = "browser-test-harness")]
    browser_test_input: BrowserTestPadInput,
}

impl App {
    fn rebuild_keyboard_bits(&mut self) {
        self.keyboard_bits = self
            .active_keys
            .values()
            .copied()
            .fold(0_u16, |value, bits| value | bits);
    }

    fn mouse_bits(&self) -> u16 {
        self.active_mouse_buttons
            .values()
            .copied()
            .fold(0_u16, |value, bit| value | bit)
    }

    fn touch_bits(&self) -> u16 {
        self.active_touches
            .values()
            .copied()
            .fold(0_u16, |value, bit| value | bit)
    }

    fn frame(&mut self, timestamp_ms: f64) -> Result<Option<FormatLevelId>, JsValue> {
        let display_settings = self.dom.display_settings();
        if display_settings != self.display_settings {
            self.display_settings = display_settings;
            self.dom.apply_display_settings(display_settings)?;
            if let Some(runtime) = &mut self.runtime {
                runtime.set_display_settings(display_settings);
            }
        }
        self.dom.sync_canvas_resolution(display_settings);
        #[cfg(not(feature = "browser-test-harness"))]
        let held = self.keyboard_bits | self.mouse_bits() | self.touch_bits() | poll_gamepad()?;
        #[cfg(feature = "browser-test-harness")]
        let (held, recorded_override) = self.browser_test_input.frame_input();
        if let Some(runtime) = &mut self.runtime {
            #[cfg(not(feature = "browser-test-harness"))]
            runtime.frame(timestamp_ms, held, &self.dom)?;
            #[cfg(feature = "browser-test-harness")]
            runtime.frame(timestamp_ms, held, recorded_override, &self.dom)?;
            update_debug(&self.debug, runtime, &self.assets)?;
            return Ok(runtime.take_asset_request());
        }
        Reflect::set(
            &self.debug,
            &JsValue::from_str("pairs"),
            &JsValue::from_f64(self.assets.pair_count() as f64),
        )?;
        Ok(None)
    }

    fn refresh_assets(&mut self) -> Result<(), JsValue> {
        self.dom
            .file_count
            .set_text_content(Some(&self.assets.file_count().to_string()));
        self.dom
            .pair_count
            .set_text_content(Some(&self.assets.pair_count().to_string()));
        self.dom
            .byte_count
            .set_text_content(Some(&format_bytes(self.assets.total_bytes())));
        let playable = self.assets.playable_levels();
        let previous = self.dom.boot_level.value();
        self.dom.boot_level.set_inner_html("");
        for (level, name) in &playable {
            let option: HtmlOptionElement =
                self.dom.document.create_element("option")?.dyn_into()?;
            option.set_value(&level.get().to_string());
            let label = if *level == FormatLevelId::TITLE {
                "Full game — from the beginning".to_owned()
            } else {
                format!("{name} — {level}")
            };
            option.set_text(&label);
            self.dom.boot_level.append_child(&option)?;
        }
        if playable.is_empty() {
            let option: HtmlOptionElement =
                self.dom.document.create_element("option")?.dyn_into()?;
            option.set_text("Select game data first");
            self.dom.boot_level.append_child(&option)?;
        } else if playable
            .iter()
            .any(|(level, _)| level.get().to_string() == previous)
        {
            self.dom.boot_level.set_value(&previous);
        } else if playable
            .iter()
            .any(|(level, _)| *level == FormatLevelId::TITLE)
        {
            self.dom
                .boot_level
                .set_value(&FormatLevelId::TITLE.get().to_string());
        } else {
            self.dom
                .boot_level
                .set_value(&playable[0].0.get().to_string());
        }

        let disabled = playable.is_empty() || self.busy || self.locked;
        self.dom.boot_level.set_disabled(disabled);
        self.dom.unlock_all.set_disabled(disabled);
        self.dom.launch.set_disabled(disabled);
        self.dom
            .clear
            .set_disabled(self.assets.file_count() == 0 || self.busy || self.locked);
        self.dom.choose_files.set_disabled(self.busy || self.locked);
        self.dom
            .choose_folder
            .set_disabled(self.busy || self.locked);
        self.dom.game_files.set_disabled(self.busy || self.locked);
        self.dom.game_folder.set_disabled(self.busy || self.locked);
        self.dom
            .dropzone
            .set_attribute("aria-disabled", &(self.busy || self.locked).to_string())?;

        let (class_name, message) = match (self.assets.file_count(), self.assets.pair_count()) {
            (0, _) => ("asset-message", "No game files selected.".to_owned()),
            (_, 44) => (
                "asset-message is-ready",
                "Full set mounted: 43 playable pairs plus the Cave archive.".to_owned(),
            ),
            (_, 0) => (
                "asset-message is-warning",
                "Recognized files do not form a playable NSD/NSF pair.".to_owned(),
            ),
            (_, pairs) => (
                "asset-message is-warning",
                format!("{pairs} bootable pair(s). Missing transition data can stop later flows."),
            ),
        };
        self.dom.asset_message.set_class_name(class_name);
        self.dom.asset_message.set_text_content(Some(&message));
        Ok(())
    }

    fn begin_import(&mut self, label: &str) -> Result<(), JsValue> {
        if self.locked {
            return Err(JsValue::from_str(
                "reload the page before replacing data in a running session",
            ));
        }
        self.busy = true;
        self.dom.set_runtime_state("loading", label)?;
        self.dom.set_progress(true, 0.08, label)?;
        self.refresh_assets()
    }

    fn finish_import(&mut self, message: &str) -> Result<(), JsValue> {
        self.busy = false;
        self.dom.set_progress(false, 1.0, "Complete")?;
        self.dom.set_runtime_state("idle", "Local media ready")?;
        self.dom.log(message, false);
        self.refresh_assets()
    }

    fn fail(&mut self, message: &str) {
        self.busy = false;
        let _ = self.dom.set_progress(false, 0.0, "Failed");
        let _ = self.dom.set_runtime_state("error", "Local media rejected");
        // Refresh control availability before installing the specific error. The
        // generic asset summary must not overwrite the actionable rejection.
        let _ = self.refresh_assets();
        self.dom
            .asset_message
            .set_class_name("asset-message is-error");
        self.dom.asset_message.set_text_content(Some(message));
        self.dom.log(message, true);
    }

    fn start_runtime(
        &mut self,
        pair: ValidatedPair,
        unlock_all_levels: bool,
    ) -> Result<(), JsValue> {
        let physical_held =
            self.keyboard_bits | self.mouse_bits() | self.touch_bits() | poll_gamepad()?;
        let storage = self.storage.take().or_else(|| StorageState::open().ok());
        let level_access = if unlock_all_levels {
            LevelAccessMode::AllLevels
        } else {
            LevelAccessMode::Progression
        };
        let mut runtime = Runtime::new(
            pair,
            storage,
            physical_held,
            level_access,
            self.display_settings,
            &self.dom,
        )?;
        runtime.set_muted(self.muted);
        self.runtime = Some(runtime);
        self.busy = false;
        self.locked = true;
        self.dom.set_progress(false, 1.0, "Ready")?;
        self.dom
            .set_runtime_state("running", "Rust runtime active")?;
        self.dom.enable_runtime_controls(true);
        self.dom.screen.focus()?;
        self.refresh_assets()?;
        Ok(())
    }

    fn flush(&mut self) {
        if let Some(runtime) = &mut self.runtime {
            runtime.flush();
        }
    }
}

#[derive(Debug)]
struct OwnedRetailZone {
    eid: Eid,
    graphics: ZoneGraphics,
    entities: Vec<ZoneEntity>,
}

#[derive(Debug, Default)]
struct RetailRuntimeMetrics {
    spawn_attempts: u64,
    successful_spawns: u64,
    already_active_spawn_skips: u64,
    authored_spawn_rejections: u64,
    failed_spawns: u64,
    executions: u64,
    execution_errors: u64,
    spawned_children: u64,
    effects: u64,
    zone_transitions: u64,
    zone_terminated_objects: u64,
    zone_event_failures: u64,
    camera_save_handshakes: u64,
}

#[cfg(feature = "browser-test-harness")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct BrowserTestCumulativeMetrics {
    hard_restarts: u64,
    load_states: u64,
    death_camera_frames: u64,
}

impl RetailRuntimeMetrics {
    fn record_spawns(&mut self, counts: BrowserSpawnCounts) {
        self.spawn_attempts = self.spawn_attempts.saturating_add(counts.attempts);
        self.successful_spawns = self.successful_spawns.saturating_add(counts.successful);
        self.already_active_spawn_skips = self
            .already_active_spawn_skips
            .saturating_add(counts.already_active);
        self.authored_spawn_rejections = self
            .authored_spawn_rejections
            .saturating_add(counts.authored_rejections);
        self.failed_spawns = self.failed_spawns.saturating_add(counts.failed);
    }

    fn record_frame<E>(&mut self, frame: &RuntimeFrame<E>) {
        self.executions = self
            .executions
            .saturating_add(frame.executions.len() as u64);
        self.execution_errors = self.execution_errors.saturating_add(
            frame
                .executions
                .iter()
                .filter(|execution| execution.result.is_err())
                .count() as u64,
        );
        self.spawned_children = self
            .spawned_children
            .saturating_add(frame.spawned_children.len() as u64);
        self.effects = self.effects.saturating_add(frame.effects.len() as u64);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetailTickState {
    NeedsSpawn,
    PausedBeforeSpawn,
    Running,
    Paused,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RetailPbakPadBoundary {
    physical_held: u16,
    timing: PbakFrameTiming,
}

/// Short-lived stream borrows around the persistent audio engine owned by a
/// mounted [`Runtime`]. GOOL can therefore suspend at an audio opcode and
/// receive the engine's real stateful response before its next instruction.
struct BrowserProgramHost<'assets, 'runtime> {
    program: NsfProgramHost<'assets>,
    metadata: &'assets Nsd,
    nsf: &'assets Nsf,
    nsf_bytes: &'assets [u8],
    audio: &'runtime mut RetailAudioEngine,
    pager: &'runtime mut Pager,
    card: &'runtime mut VirtualCard,
    storage: &'runtime mut Option<StorageState>,
    environmentless_mdat: Option<Eid>,
}

impl<'assets, 'runtime> BrowserProgramHost<'assets, 'runtime> {
    fn new(
        metadata: &'assets Nsd,
        nsf: &'assets Nsf,
        nsf_bytes: &'assets [u8],
        audio: &'runtime mut RetailAudioEngine,
        pager: &'runtime mut Pager,
        card: &'runtime mut VirtualCard,
        storage: &'runtime mut Option<StorageState>,
    ) -> Self {
        Self {
            program: NsfProgramHost::new(metadata, nsf, nsf_bytes),
            metadata,
            nsf,
            nsf_bytes,
            audio,
            pager,
            card,
            storage,
            environmentless_mdat: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn for_title_mdat(
        metadata: &'assets Nsd,
        nsf: &'assets Nsf,
        nsf_bytes: &'assets [u8],
        audio: &'runtime mut RetailAudioEngine,
        pager: &'runtime mut Pager,
        card: &'runtime mut VirtualCard,
        storage: &'runtime mut Option<StorageState>,
        mdat: Eid,
    ) -> Self {
        let mut host = Self::new(metadata, nsf, nsf_bytes, audio, pager, card, storage);
        host.environmentless_mdat = Some(mdat);
        host
    }

    fn ensure_audio_sample(&mut self, eid: Eid) -> Result<(), BrowserProgramError> {
        if self.audio.has_sample(eid) || self.audio.sfx_volume() == 0 {
            return Ok(());
        }
        let entry = self
            .nsf
            .resolve_entry(self.metadata, eid)
            .map_err(NsfProgramError::Format)
            .map_err(BrowserProgramError::Program)?;
        if entry.entry_type != ADIO_ENTRY_TYPE {
            return Err(BrowserProgramError::AudioAsset(format!(
                "audio EID {eid} resolves to entry type {}, expected ADIO type {ADIO_ENTRY_TYPE}",
                entry.entry_type
            )));
        }
        let item = entry.item(0).ok_or_else(|| {
            BrowserProgramError::AudioAsset(format!("ADIO entry {eid} has no sample item"))
        })?;
        let bytes = item
            .bytes(self.nsf_bytes)
            .map_err(NsfProgramError::Format)
            .map_err(BrowserProgramError::Program)?;
        if !self.audio.register_adpcm(eid, bytes) {
            return Err(BrowserProgramError::AudioAsset(format!(
                "ADIO entry {eid} decoded to an empty or oversized sample"
            )));
        }
        Ok(())
    }
}

impl ProgramHost for BrowserProgramHost<'_, '_> {
    type Error = BrowserProgramError;

    fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
        self.program
            .bind_program(binding)
            .map_err(BrowserProgramError::Program)
    }

    fn materialize_program_page(
        &mut self,
        binding: ProgramBinding<'_>,
    ) -> Result<Option<PagerOpenOutcome>, Self::Error> {
        let global = self
            .program
            .global_eid(binding.executable)
            .map_err(BrowserProgramError::Program)?;
        self.pager
            .materialize_eid_with_outcome(global)
            .map(Some)
            .map_err(BrowserProgramError::Paging)
    }

    fn bind_state_program(
        &mut self,
        binding: StateProgramBinding,
    ) -> Result<VmStateProgram, Self::Error> {
        self.program
            .bind_state_program(binding)
            .map_err(BrowserProgramError::Program)
    }

    fn zone_environment(
        &mut self,
        zone: Eid,
    ) -> Result<Option<RetailZoneEnvironment>, Self::Error> {
        if self.environmentless_mdat == Some(zone) {
            // A type-17 entry is entity provenance, not a ZDAT environment.
            // Native GoolObjectSpawn rewrites its object zone to `cur_zone`
            // before reading colors, so the authored title path normally
            // reaches the current ZDAT branch below. Keep this guard for any
            // environmentless MDAT-origin binding that has not been rewritten.
            return Ok(None);
        }
        self.program
            .zone_environment(zone)
            .map_err(BrowserProgramError::Program)
    }

    fn find_neighbor_zone(
        &mut self,
        current_zone: Eid,
        point: [i32; 3],
    ) -> Result<Option<Eid>, Self::Error> {
        self.program
            .find_neighbor_zone(current_zone, point)
            .map_err(BrowserProgramError::Program)
    }

    fn current_zone_neighbors(&mut self, current_zone: Eid) -> Result<Vec<Eid>, Self::Error> {
        self.program
            .current_zone_neighbors(current_zone)
            .map_err(BrowserProgramError::Program)
    }

    fn solid_environment(
        &mut self,
        zone: Eid,
    ) -> Result<Option<RetailSolidEnvironment>, Self::Error> {
        if self.environmentless_mdat == Some(zone) {
            return Ok(None);
        }
        self.program
            .solid_environment(zone)
            .map_err(BrowserProgramError::Program)
    }

    fn animation_bound_source(
        &mut self,
        binding: AnimationBoundBinding,
    ) -> Result<Option<AnimationBoundSource>, Self::Error> {
        self.program
            .animation_bound_source(binding)
            .map_err(BrowserProgramError::Program)
    }

    fn animation_display_vertex_kind(
        &mut self,
        binding: AnimationBoundBinding,
    ) -> Result<Option<ObjectVertexKind>, Self::Error> {
        self.program
            .animation_display_vertex_kind(binding)
            .map_err(BrowserProgramError::Program)
    }

    fn model_vertex_source(
        &mut self,
        binding: ModelVertexBinding,
    ) -> Result<Option<ModelVertexSource>, Self::Error> {
        self.program
            .model_vertex_source(binding)
            .map_err(BrowserProgramError::Program)
    }

    fn handle_audio_request(
        &mut self,
        request: AudioHostRequest,
    ) -> Result<AudioHostResponse, Self::Error> {
        if let AudioHostRequest::CreateVoice(request) = request {
            self.ensure_audio_sample(request.adio)?;
        }
        self.audio
            .handle_request(request)
            .map_err(BrowserProgramError::Audio)
    }

    fn handle_paging_request(
        &mut self,
        request: PagingHostRequest,
    ) -> Result<PagingHostResponse, Self::Error> {
        match request.operation {
            PagingHostOperation::Open => match if request.physical {
                self.pager.open_eid_with_outcome(request.eid)
            } else {
                self.pager.open_eid_virtual_with_outcome(request.eid)
            } {
                Ok(outcome) => {
                    if outcome.page != request.page {
                        return Err(BrowserProgramError::PagingPageMismatch {
                            requested: request.page,
                            resolved: outcome.page,
                        });
                    }
                    if outcome.resolved {
                        Ok(PagingHostResponse::Applied {
                            invalidated: outcome.invalidated,
                        })
                    } else {
                        Ok(PagingHostResponse::Queued)
                    }
                }
                Err(PagingError::NoFreePhysicalSlot(_) | PagingError::NoFreeTextureSlot(_)) => {
                    Ok(PagingHostResponse::Unavailable)
                }
                Err(error) => Err(BrowserProgramError::Paging(error)),
            },
            PagingHostOperation::Close => {
                let outcome = self
                    .pager
                    .close_eid_retail_with_outcome(request.eid)
                    .map_err(BrowserProgramError::Paging)?;
                if outcome.page != request.page {
                    return Err(BrowserProgramError::PagingPageMismatch {
                        requested: request.page,
                        resolved: outcome.page,
                    });
                }
                Ok(PagingHostResponse::Applied {
                    invalidated: if outcome.unresolved {
                        PageInvalidations::one(outcome.page)
                    } else {
                        PageInvalidations::NONE
                    },
                })
            }
            // `NSClose(ref, 0)` only observes the already-resolved reference.
            // The VM owns the native binary's exact one-word result; the
            // platform pager must remain unchanged.
            PagingHostOperation::Probe => Ok(PagingHostResponse::Applied {
                invalidated: PageInvalidations::NONE,
            }),
        }
    }

    fn texture_frame_snapshot(&self) -> Option<TextureFrameSnapshot> {
        Some(self.pager.texture_frame_snapshot())
    }

    fn free_object_audio(&mut self, object: RuntimeObjectHandle) -> bool {
        self.audio.free_owner(object.vm());
        true
    }

    fn handle_card_request(
        &mut self,
        request: CardHostRequest,
        current: SaveData,
    ) -> Result<CardHostResponse, Self::Error> {
        let operation = CardOperation::from_retail(request.operation);
        let part_index = usize::try_from(request.part_index).unwrap_or(usize::MAX);
        let before = self.card.clone();
        let mut candidate = before.clone();
        candidate.set_storage_available(self.storage.is_some());
        let outcome = candidate.control(operation, part_index, Some(current));

        if outcome.is_ok() && operation.mutates_storage() {
            let intent = match operation {
                CardOperation::Format => CardPersistIntent::Format,
                CardOperation::SaveSelected | CardOperation::SaveCurrent => candidate
                    .current_slot()
                    .map_or(CardPersistIntent::Snapshot, CardPersistIntent::WriteSlot),
                _ => CardPersistIntent::Snapshot,
            };
            let persisted = self
                .storage
                .as_mut()
                .is_some_and(|storage| storage.persist_card(&candidate, intent).is_ok());
            if !persisted {
                let mut failed = before;
                failed.set_storage_available(false);
                let _ = failed.control(operation, part_index, Some(current));
                *self.card = failed;
                return Ok(CardHostResponse {
                    result: 1,
                    loaded: None,
                    published: self.card.published_state(),
                });
            }
        }

        let loaded = match outcome {
            Ok(CardOutcome::Loaded(save)) => Some(save),
            Ok(CardOutcome::Complete) | Err(_) => None,
        };
        let result = i32::from(outcome.is_err());
        *self.card = candidate;
        Ok(CardHostResponse {
            result,
            loaded,
            published: self.card.published_state(),
        })
    }
}

enum PreparedRetailMusic {
    Unchanged,
    Silence,
    Music(Eid, Box<RetailMusic>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PbakArmedPresentation {
    PreviousScene,
    LoadingImage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RetailPairMount {
    target: FormatLevelId,
    carry: RetailSessionCarry,
    bonus_return: bool,
    core_transition: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LevelAccessMode {
    Progression,
    AllLevels,
}

impl LevelAccessMode {
    const fn all_levels(self) -> bool {
        matches!(self, Self::AllLevels)
    }

    const fn persists(self) -> bool {
        matches!(self, Self::Progression)
    }
}

fn apply_level_access(
    mode: LevelAccessMode,
    runtime: &mut RetailRuntime,
    flow: &mut RetailFlowMirror,
) -> Result<(), crust_sim::gool::VmError> {
    if mode.all_levels() {
        apply_all_levels_override(runtime)?;
        mirror_level_access(mode, runtime, flow)?;
    }
    Ok(())
}

fn mirror_level_access(
    mode: LevelAccessMode,
    runtime: &RetailRuntime,
    flow: &mut RetailFlowMirror,
) -> Result<(), crust_sim::gool::VmError> {
    if mode.all_levels() {
        flow.progress.levels_unlocked = runtime.global_word(LEVELS_UNLOCKED_GLOBAL)?;
        flow.progress.item_pool_2 = runtime.global_word(ITEM_POOL_2_GLOBAL)?;
    }
    Ok(())
}

struct Runtime {
    flow: RetailFlowMirror,
    scheduler: FrameScheduler,
    previous_step_us: Option<u64>,
    previous_shader_wall_ticks: Option<u32>,
    retail_shader_ticks_elapsed: u32,
    pad: PlatformPadState,
    /// Most recent physical browser sample, refreshed on every RAF even while
    /// an asynchronous pair validation freezes cooperative simulation.
    latest_physical_held: u16,
    stage: GlStage,
    retail_frame: RetailFrameState,
    retail_objects: RetailRuntime,
    retail_zones: BTreeMap<Eid, OwnedRetailZone>,
    retail_zone_lifecycle: ZoneLifecycle,
    retail_zone_pager: Pager,
    retail_tick_state: RetailTickState,
    retail_metrics: RetailRuntimeMetrics,
    #[cfg(feature = "browser-test-harness")]
    browser_test_cumulative_metrics: BrowserTestCumulativeMetrics,
    retail_runtime_error: Option<String>,
    retail_runtime_warning: Option<String>,
    retail_scene_builder: RetailSceneBuilder,
    retail_world_shader_render_state: RetailWorldShaderRenderState,
    retail_zone_graph: RetailZoneGraph,
    retail_camera: RetailCameraRuntime,
    retail_death_camera: RetailDeathCameraState,
    retail_camera_pose_override: Option<RetailCameraPose>,
    show_loading_image: bool,
    pbak_armed_presentation: Option<PbakArmedPresentation>,
    level_assets: ValidatedPair,
    retail_audio: RetailAudioEngine,
    retail_master_fade: RetailMasterFade,
    retail_pbak: Option<RetailPbakPlayback>,
    audio: Option<WebAudio>,
    storage: Option<StorageState>,
    card: VirtualCard,
    resume: ResumeManager,
    level_access: LevelAccessMode,
    display_settings: DisplaySettings,
    /// Last payload successfully read from retail GOOL globals. This is the
    /// only persistence fallback used if an impossible fixed-allocation VM
    /// read fails; no high-level mirror state is serialized.
    last_authoritative_save: SaveData,
    last_title_state: Option<u8>,
    pending_buttons: u16,
    pending_mount: Option<RetailPairMount>,
    loading_mount: Option<RetailPairMount>,
    next_lid: i32,
    title_seen: bool,
    asset_load_error: Option<String>,
    muted: bool,
    last_gl_error: u32,
}

impl Runtime {
    fn new(
        pair: ValidatedPair,
        mut storage: Option<StorageState>,
        physical_held: u16,
        level_access: LevelAccessMode,
        display_settings: DisplaySettings,
        dom: &Dom,
    ) -> Result<Self, JsValue> {
        let raw_level = u8::try_from(pair.level.get())
            .map_err(|_| JsValue::from_str("selected level does not fit the retail id"))?;
        let boot_level = LevelId::new(raw_level)
            .ok_or_else(|| JsValue::from_str("selected level is not in the retail catalog"))?;
        let mut flow = RetailFlowMirror::new();
        flow.boot(boot_level)
            .map_err(|error| JsValue::from_str(&format!("could not boot level: {error:?}")))?;
        let mut save = default_save();
        let card = storage
            .as_ref()
            .map_or_else(VirtualCard::new, StorageState::virtual_card);
        if level_access.all_levels()
            && let Some(storage) = &mut storage
        {
            storage.keep_writes_in_memory();
        }
        let (resume, resume_result) = if let Some(storage) = &storage {
            storage.load_resume(save)?
        } else {
            ResumeManager::load(None, save)
        };
        if let ResumeLoadResult::Loaded(restored) = resume_result {
            save = restored;
            apply_save(&mut flow, restored);
            dom.log("Restored checksummed browser resume data.", false);
        } else if resume_result == ResumeLoadResult::Corrupt {
            dom.log(
                if level_access.persists() {
                    "Quarantined an invalid browser resume record."
                } else {
                    "Ignored an invalid browser resume record without modifying storage."
                },
                true,
            );
        }

        let audio = match WebAudio::new() {
            Ok(mut audio) => {
                audio.set_output_options(output_options(flow.options));
                Some(audio)
            }
            Err(error) => {
                dom.log(
                    &format!("Audio initialization deferred: {}", js_message(&error)),
                    true,
                );
                None
            }
        };
        // MidiInit partitions the shared 24-voice table on every NSInit. The
        // boundary depends on the mounted level and restored option volumes;
        // defaulting to eight would let SFX steal retail music slots in most
        // levels.
        let mut retail_audio = RetailAudioEngine::new(retail_max_midi_voices(
            pair.level,
            flow.options.music_volume,
            flow.options.sfx_volume,
        ))
        .map_err(|error| {
            JsValue::from_str(&format!(
                "could not initialize the retail MIDI voice boundary: {error}"
            ))
        })?;
        retail_audio.set_sfx_volume(flow.options.sfx_volume);
        let mut stage = GlStage::new(&dom.canvas)?;
        let after_loading_image = if let Some(image) = decode_pair_loading_image(&pair)? {
            stage.install_loading_image(&image)?;
            dom.log(
                &format!(
                    "Decoded and uploaded the {}x{} retail loading image.",
                    image.width(),
                    image.height()
                ),
                false,
            );
            true
        } else {
            false
        };
        let retail_zone_graph = retail_zone_graph(&pair).map_err(|error| {
            JsValue::from_str(&format!(
                "retail camera graph for {} is invalid: {error}",
                pair.level
            ))
        })?;
        let retail_camera = RetailCameraRuntime::new(&retail_zone_graph).map_err(|error| {
            JsValue::from_str(&format!(
                "retail camera state for {} is invalid: {error}",
                pair.level
            ))
        })?;
        let (retail_zones, retail_zone_lifecycle) = parse_retail_zone_catalog(
            &pair,
            &retail_zone_graph,
            retail_camera.location().path.zone,
        )?;
        let initial_zone = retail_camera.location().path.zone;
        let initial_background_fill = retail_background_fill_for_zone(
            &retail_zones,
            initial_zone,
            INITIAL_DISPLAY_MASK,
            [0; 3],
        )
        .map_err(|error| JsValue::from_str(&error))?;
        let initial_next_vram_fill_color =
            retail_zone_transition_color(&retail_zones, initial_zone)
                .map_err(|error| JsValue::from_str(&error))?;
        let retail_zone_pager = build_retail_zone_pager(&pair, &retail_zone_lifecycle)?;
        dom.log(
            &format!(
                "Parsed {} reachable zones with {} owned retail entity descriptors; {} zones are in the initial spawn band.",
                retail_zones.len(),
                retail_entity_count(&retail_zones),
                retail_zone_lifecycle.next_frame_spawn_scan().len(),
            ),
            false,
        );
        let retail_point_count = retail_spawn_point_count(&retail_zone_graph)?;
        dom.log(
            &format!(
                "Validated a pointer-free camera graph with {} zones and {} paths.",
                retail_zone_graph.zone_count(),
                retail_zone_graph.path_count(),
            ),
            false,
        );
        let retail_scene_builder = RetailSceneBuilder::new();
        let retail_world_shader_render_state = RetailWorldShaderRenderState::default();
        install_retail_scene_for_pair(
            &pair,
            &mut stage,
            dom,
            after_loading_image,
            true,
            0,
            retail_point_count,
            retail_zone_pager.texture_frame_snapshot(),
            crust_sim::retail_runtime::RetailWorldShaderSnapshot::initialized_for_level(pair.level),
            retail_world_shader_render_state,
            initial_background_fill,
        )?;
        let retail_frame =
            crate::retail_frame_for_mount(retail_point_count, 0, 0, false, after_loading_image);
        dom.log(
            &format!(
                "Validated {} pages and {} entries for {}.",
                pair.nsf.pages.len(),
                pair.nsf
                    .pages
                    .iter()
                    .map(|page| match page {
                        crust_formats::stream::NsfPage::Texture(_) => 0,
                        crust_formats::stream::NsfPage::Entries(page) => page.entries.len(),
                    })
                    .sum::<usize>(),
                level_name(boot_level)
            ),
            false,
        );
        // Keep the checksum baseline synchronized even when the restored value was unchanged.
        let mut retail_objects = RetailRuntime::new_for_level(RETAIL_GLOBAL_WORDS, pair.level);
        retail_objects.set_next_vram_fill_color(initial_next_vram_fill_color);
        retail_objects
            .restore_card_save_data(save)
            .and_then(|()| retail_objects.publish_card_state(card.published_state()))
            .map_err(|error| {
                JsValue::from_str(&format!(
                    "could not initialize retail card globals: {error:?}"
                ))
            })?;
        let initial_level_state = initial_retail_level_state();
        retail_objects.set_level_state_context(build_retail_level_state_context(
            &retail_zone_graph,
            retail_camera.location(),
            &retail_zone_lifecycle,
            initial_level_state.box_count,
            initial_level_state.checkpoint_id,
            initial_level_state.checkpoint_translation,
            false,
        )?);
        // Native CoreObjectsCreate begins with PadUpdate. Publish that shifted
        // history before any HUD/root once handler can observe the destination
        // pad, and retain the same snapshot for the first CoreFrame gate.
        let mut pad = PlatformPadState::default();
        let initial_pad = core_objects_pad_update(&mut pad, physical_held, 0, None);
        retail_objects
            .set_pad_snapshot(0, retail_pad_snapshot(initial_pad))
            .map_err(|error| {
                JsValue::from_str(&format!(
                    "could not initialize the CoreObjectsCreate pad boundary: {error:?}"
                ))
            })?;
        let retail_core_objects = create_retail_core_objects_for_pair(
            &mut retail_objects,
            &pair,
            retail_camera.location().path.zone,
        )?;
        let retail_level_misc_object = create_retail_level_misc_object_for_pair(
            &mut retail_objects,
            &pair,
            retail_camera.location().path.zone,
        )?;
        apply_level_access(level_access, &mut retail_objects, &mut flow).map_err(|error| {
            JsValue::from_str(&format!(
                "could not apply temporary all-level access: {error:?}"
            ))
        })?;
        if level_access.all_levels() {
            dom.log(
                "Unlocked every island-map node and both key paths with 999 starting lives for this session; browser resume and memory-card writes remain untouched.",
                false,
            );
        }
        let last_authoritative_save = retail_objects.card_save_data().map_err(|error| {
            JsValue::from_str(&format!(
                "could not snapshot initial retail save globals: {error:?}"
            ))
        })?;
        let retail_pager_page_count = u32::try_from(retail_zone_pager.page_count())
            .map_err(|_| JsValue::from_str("initial retail pager page count exceeds u32"))?;
        retail_objects
            .seed_platform_paging_state_with_capacity(
                retail_pager_page_count,
                u32::try_from(retail_zone_pager.physical_slot_count()).map_err(|_| {
                    JsValue::from_str("initial retail physical page count exceeds u32")
                })?,
                retail_zone_pager.resolved_pages(),
                retail_zone_pager.page_reference_counts(),
                retail_zone_pager.uncounted_pages(),
            )
            .map_err(|error| {
                JsValue::from_str(&format!(
                    "could not seed initial retail page resolutions: {error:?}"
                ))
            })?;
        retail_objects
            .seed_platform_pending_pages(retail_zone_pager.pending_virtual_pages())
            .map_err(|error| {
                JsValue::from_str(&format!(
                    "could not seed initial virtual page requests: {error:?}"
                ))
            })?;
        let title_seen = pair.level == FormatLevelId::TITLE;
        let mut runtime = Self {
            flow,
            scheduler: FrameScheduler::new(),
            previous_step_us: None,
            previous_shader_wall_ticks: None,
            retail_shader_ticks_elapsed: 0,
            pad,
            latest_physical_held: physical_held,
            stage,
            retail_frame,
            retail_objects,
            retail_zones,
            retail_zone_lifecycle,
            retail_zone_pager,
            retail_tick_state: RetailTickState::NeedsSpawn,
            retail_metrics: RetailRuntimeMetrics::default(),
            #[cfg(feature = "browser-test-harness")]
            browser_test_cumulative_metrics: BrowserTestCumulativeMetrics::default(),
            retail_runtime_error: None,
            retail_runtime_warning: None,
            retail_scene_builder,
            retail_world_shader_render_state,
            retail_zone_graph,
            retail_camera,
            retail_death_camera: RetailDeathCameraState::default(),
            retail_camera_pose_override: None,
            show_loading_image: after_loading_image,
            pbak_armed_presentation: None,
            level_assets: pair,
            retail_audio,
            retail_master_fade: RetailMasterFade::new(),
            retail_pbak: None,
            audio,
            storage: storage.take(),
            card,
            resume,
            level_access,
            display_settings,
            last_authoritative_save,
            // Even the first authored title screen enters through
            // TitleLoadScreen's flag-two LevelUpdate. `sync_title_card` below
            // applies that transaction before the runtime is returned.
            last_title_state: None,
            pending_buttons: 0,
            pending_mount: None,
            loading_mount: None,
            next_lid: -1,
            title_seen,
            asset_load_error: None,
            muted: false,
            last_gl_error: 0,
        };
        runtime.configure_retail_title_authority(true)?;
        log_retail_core_objects(dom, retail_core_objects);
        log_retail_level_misc_object(dom, retail_level_misc_object);
        runtime.sync_title_card(dom)?;
        let initial_zone = runtime.retail_camera.location().path.zone;
        let initial_music = runtime
            .prepare_retail_music(&runtime.level_assets, initial_zone, false)
            .map_err(|error| JsValue::from_str(&error))?;
        runtime
            .apply_prepared_retail_music(initial_music, true, initial_zone, dom)
            .map_err(|error| JsValue::from_str(&error))?;
        Ok(runtime)
    }

    fn set_display_settings(&mut self, settings: DisplaySettings) {
        self.display_settings = settings;
    }

    fn take_asset_request(&mut self) -> Option<FormatLevelId> {
        let mount = self.pending_mount.take()?;
        if !mount.core_transition && mount.target == self.level_assets.level {
            return None;
        }
        let level = mount.target;
        self.loading_mount = Some(mount);
        self.asset_load_error = None;
        self.scheduler.set_paused(true);
        Some(level)
    }

    fn install_level_assets(&mut self, pair: ValidatedPair, dom: &Dom) -> Result<(), JsValue> {
        let mut mount = self
            .loading_mount
            .clone()
            .ok_or_else(|| JsValue::from_str("validated stream pair has no pending transition"))?;
        if mount.target != pair.level {
            return Err(JsValue::from_str(
                "validated stream pair does not match the pending transition",
            ));
        }
        let retail_zone_graph = retail_zone_graph(&pair).map_err(|error| {
            JsValue::from_str(&format!(
                "destination camera graph for {} is invalid: {error}",
                pair.level
            ))
        })?;
        let mount_game_state = mount
            .carry
            .globals
            .get(crust_sim::gool::GAME_STATE_GLOBAL)
            .copied()
            .ok_or_else(|| JsValue::from_str("retail session has no game-state global"))?
            .cast_signed();
        let title_attract_mount = mount.core_transition
            && mount_game_state == 0x600
            && pair.level != FormatLevelId::TITLE;
        let prepared_pbak = if title_attract_mount {
            let mut random_seed_b = mount.carry.random_seed_b();
            let prepared = prepare_pair_pbak(
                &pair.nsd,
                &pair.nsf,
                &pair.nsf_bytes,
                &retail_zone_graph,
                &mut random_seed_b,
            )
            .map_err(|error| {
                JsValue::from_str(&format!(
                    "could not prepare retail PBAK for {}: {error}",
                    pair.level
                ))
            })?;
            // `PbakChoose` precedes destination LevelUpdate and shader
            // initialization, and consumes randb even when exactly one
            // recording exists.
            mount.carry.set_random_seed_b(random_seed_b);
            prepared
        } else {
            None
        };
        let retail_camera = if mount.bonus_return {
            let snapshot = mount.carry.saved_level_state.as_ref().ok_or_else(|| {
                JsValue::from_str("bonus return has no carried retail level snapshot")
            })?;
            if snapshot.level != pair.level {
                return Err(JsValue::from_str(&format!(
                    "bonus return snapshot targets {}, not mounted {}",
                    snapshot.level, pair.level
                )));
            }
            RetailCameraRuntime::at_path(
                &retail_zone_graph,
                snapshot.location.path,
                snapshot.location.progress.raw(),
                mount_game_state,
            )
        } else {
            RetailCameraRuntime::at_path(
                &retail_zone_graph,
                retail_zone_graph.spawn_path(),
                0,
                mount_game_state,
            )
        }
        .map_err(|error| {
            JsValue::from_str(&format!(
                "destination camera state for {} is invalid: {error}",
                pair.level
            ))
        })?;
        let (retail_zones, retail_zone_lifecycle) = parse_retail_zone_catalog(
            &pair,
            &retail_zone_graph,
            retail_camera.location().path.zone,
        )?;
        let retail_zone_pager = build_retail_zone_pager(&pair, &retail_zone_lifecycle)?;
        let destination_zone = retail_camera.location().path.zone;
        let destination_background_fill = retail_background_fill_for_zone(
            &retail_zones,
            destination_zone,
            INITIAL_DISPLAY_MASK,
            [0; 3],
        )
        .map_err(|error| JsValue::from_str(&error))?;
        let destination_next_vram_fill_color =
            retail_zone_transition_color(&retail_zones, destination_zone)
                .map_err(|error| JsValue::from_str(&error))?;
        let destination_music = self
            .prepare_retail_music(&pair, destination_zone, true)
            .map_err(|error| JsValue::from_str(&error))?;
        dom.log(
            &format!(
                "Parsed {} destination zones with {} owned retail entity descriptors.",
                retail_zones.len(),
                retail_entity_count(&retail_zones),
            ),
            false,
        );
        let retail_point_count = retail_spawn_point_count(&retail_zone_graph)?;
        let loading_image = decode_pair_loading_image(&pair)?;
        let after_loading_image = loading_image.is_some();
        let pages = pair.nsf.pages.len();
        let entries = pair_entry_count(&pair);
        let level = pair.level;
        let flow_level = u8::try_from(level.get())
            .ok()
            .and_then(LevelId::new)
            .ok_or_else(|| JsValue::from_str("mounted retail level is not playable"))?;
        let title_screen = (flow_level == LevelId::TITLE)
            .then(|| self.title_screen_for_mount(&mount))
            .transpose()?;
        let mut retail_objects =
            RetailRuntime::new_from_session(RETAIL_GLOBAL_WORDS, level, mount.carry.clone())
                .map_err(|error| {
                    JsValue::from_str(&format!(
                        "could not import retail session across mount: {error:?}"
                    ))
                })?;
        retail_objects.set_next_vram_fill_color(destination_next_vram_fill_color);
        retail_objects
            .publish_card_state(self.card.published_state())
            .map_err(|error| {
                JsValue::from_str(&format!(
                    "could not carry retail card globals across mount: {error:?}"
                ))
            })?;
        if title_attract_mount {
            retail_objects
                .set_global_word(
                    GAME_STATE_GLOBAL,
                    if prepared_pbak.is_some() { 0x600 } else { 0 },
                )
                .and_then(|()| {
                    retail_objects.set_global_word(
                        PBAK_STATE_GLOBAL,
                        if prepared_pbak.is_some() { 3 } else { 0 },
                    )
                })
                .map_err(|error| {
                    JsValue::from_str(&format!(
                        "could not initialize retail PBAK globals: {error:?}"
                    ))
                })?;
        }
        let read_mount_global = |index| {
            retail_objects
                .global_word(index)
                .map(u32::cast_signed)
                .map_err(|error| {
                    JsValue::from_str(&format!(
                        "could not import retail mount global {index}: {error:?}"
                    ))
                })
        };
        let mounted_box_count = read_mount_global(BOX_COUNT_GLOBAL)?;
        let mounted_checkpoint_id = read_mount_global(CHECKPOINT_ID_GLOBAL)?;
        let mounted_checkpoint_translation = [
            read_mount_global(CHECKPOINT_TRANSLATION_GLOBALS[0])?,
            read_mount_global(CHECKPOINT_TRANSLATION_GLOBALS[1])?,
            read_mount_global(CHECKPOINT_TRANSLATION_GLOBALS[2])?,
        ];
        let mounted_music_volume = retail_objects
            .global_word(MUSIC_VOLUME_GLOBAL)
            .map_err(|error| {
                JsValue::from_str(&format!(
                    "could not import destination music volume: {error:?}"
                ))
            })?
            .min(u32::from(u8::MAX)) as u8;
        let mounted_sfx_volume = retail_objects
            .global_word(SFX_VOLUME_GLOBAL)
            .map_err(|error| {
                JsValue::from_str(&format!(
                    "could not import destination SFX volume: {error:?}"
                ))
            })?
            .min(u32::from(u8::MAX)) as u8;
        let mut destination_retail_audio = RetailAudioEngine::new(retail_max_midi_voices(
            level,
            mounted_music_volume,
            mounted_sfx_volume,
        ))
        .map_err(|error| {
            JsValue::from_str(&format!(
                "could not initialize destination retail MIDI voice boundary: {error}"
            ))
        })?;
        destination_retail_audio.set_sfx_volume(mounted_sfx_volume);
        retail_objects.set_level_state_context(build_retail_level_state_context(
            &retail_zone_graph,
            retail_camera.location(),
            &retail_zone_lifecycle,
            mounted_box_count,
            mounted_checkpoint_id,
            mounted_checkpoint_translation,
            false,
        )?);
        // Build the pad boundary transactionally alongside the candidate
        // runtime. If validation fails, neither the live history nor the live
        // object graph advances. On commit this is the PadUpdate at the start
        // of native CoreObjectsCreate after NSInit.
        let mut destination_pad = self.pad;
        let destination_pad_snapshot = core_objects_pad_update(
            &mut destination_pad,
            self.latest_physical_held,
            self.pending_buttons,
            prepared_pbak.as_ref().map(|_| 0),
        );
        retail_objects
            .set_pad_snapshot(0, retail_pad_snapshot(destination_pad_snapshot))
            .map_err(|error| {
                JsValue::from_str(&format!(
                    "could not initialize destination CoreObjectsCreate pad boundary: {error:?}"
                ))
            })?;
        // Materialize the destination's process-lifetime roots against the
        // candidate runtime and candidate stream host. A malformed DispC or
        // ZDAT therefore fails before the active flow/runtime mount is
        // committed below.
        let retail_core_objects = create_retail_core_objects_for_pair(
            &mut retail_objects,
            &pair,
            retail_camera.location().path.zone,
        )?;
        let retail_level_misc_object = create_retail_level_misc_object_for_pair(
            &mut retail_objects,
            &pair,
            retail_camera.location().path.zone,
        )?;
        if self.level_access.all_levels() {
            apply_all_levels_override(&mut retail_objects).map_err(|error| {
                JsValue::from_str(&format!(
                    "could not carry temporary all-level access into the destination: {error:?}"
                ))
            })?;
        }
        let retail_pager_page_count = u32::try_from(retail_zone_pager.page_count())
            .map_err(|_| JsValue::from_str("destination retail pager page count exceeds u32"))?;
        retail_objects
            .seed_platform_paging_state_with_capacity(
                retail_pager_page_count,
                u32::try_from(retail_zone_pager.physical_slot_count()).map_err(|_| {
                    JsValue::from_str("destination retail physical page count exceeds u32")
                })?,
                retail_zone_pager.resolved_pages(),
                retail_zone_pager.page_reference_counts(),
                retail_zone_pager.uncounted_pages(),
            )
            .map_err(|error| {
                JsValue::from_str(&format!(
                    "could not seed destination retail page resolutions: {error:?}"
                ))
            })?;
        retail_objects
            .seed_platform_pending_pages(retail_zone_pager.pending_virtual_pages())
            .map_err(|error| {
                JsValue::from_str(&format!(
                    "could not seed destination virtual page requests: {error:?}"
                ))
            })?;
        let destination_authoritative_save = retail_objects.card_save_data().map_err(|error| {
            JsValue::from_str(&format!(
                "could not snapshot destination retail save globals: {error:?}"
            ))
        })?;
        if let Some(image) = loading_image {
            self.stage.install_loading_image(&image)?;
            dom.log(
                &format!(
                    "Decoded and uploaded the {}x{} destination loading image.",
                    image.width(),
                    image.height()
                ),
                false,
            );
        }
        // A validated pair transition gets a fresh owner so parsed graph data,
        // TPAG mappings, and decoded pixels cannot cross the mount boundary.
        let retail_scene_builder = RetailSceneBuilder::new();
        let retail_world_shader_render_state = self.retail_world_shader_render_state;
        let preserve_framebuffer_for_pbak = prepared_pbak.is_some();
        install_retail_scene_for_pair(
            &pair,
            &mut self.stage,
            dom,
            after_loading_image,
            !preserve_framebuffer_for_pbak,
            retail_objects.draw_count(),
            retail_point_count,
            retail_zone_pager.texture_frame_snapshot(),
            retail_objects.world_shader_snapshot(),
            retail_world_shader_render_state,
            destination_background_fill,
        )?;
        let retail_frame = crate::retail_frame_for_mount(
            retail_point_count,
            0,
            retail_objects.draw_count(),
            mount.core_transition,
            after_loading_image,
        );
        let first_title_boot = flow_level == LevelId::TITLE && !self.title_seen;
        self.flow
            .mount_retail_level(flow_level, title_screen)
            .map_err(|error| {
                JsValue::from_str(&format!("could not mirror mounted retail flow: {error:?}"))
            })?;
        mirror_level_access(self.level_access, &retail_objects, &mut self.flow).map_err(
            |error| {
                JsValue::from_str(&format!(
                    "could not mirror temporary all-level access after the destination mount: {error:?}"
                ))
            },
        )?;
        if flow_level == LevelId::TITLE {
            self.title_seen = true;
        }
        self.retail_frame = retail_frame;
        self.show_loading_image = after_loading_image;
        self.level_assets = pair;
        self.retail_scene_builder = retail_scene_builder;
        self.retail_world_shader_render_state = retail_world_shader_render_state;
        self.retail_zone_graph = retail_zone_graph;
        self.retail_camera = retail_camera;
        self.retail_camera_pose_override = None;
        self.pad = destination_pad;
        self.retail_objects = retail_objects;
        self.last_authoritative_save = destination_authoritative_save;
        self.retail_zones = retail_zones;
        self.retail_zone_lifecycle = retail_zone_lifecycle;
        self.retail_zone_pager = retail_zone_pager;
        self.retail_pbak = prepared_pbak.map(RetailPbakPlayback::new);
        self.pbak_armed_presentation = self.retail_pbak.as_ref().map(|_| {
            if after_loading_image {
                PbakArmedPresentation::LoadingImage
            } else {
                PbakArmedPresentation::PreviousScene
            }
        });
        self.configure_retail_title_authority(first_title_boot)?;
        self.retail_tick_state = RetailTickState::NeedsSpawn;
        self.retail_metrics = RetailRuntimeMetrics::default();
        self.retail_runtime_error = None;
        self.retail_runtime_warning = None;
        destination_retail_audio.set_random_seed(self.retail_objects.random_seed_b());
        self.retail_audio = destination_retail_audio;
        // AudioInit/MidiInit restore the whole-output fade to full scale on
        // every NSInit. Never carry a level-end fade into the destination.
        self.retail_master_fade = RetailMasterFade::new();
        if let Some(audio) = &mut self.audio {
            audio.set_retail_master_gain(self.retail_master_fade.normalized_gain());
        }
        self.last_title_state = None;
        log_retail_core_objects(dom, retail_core_objects);
        log_retail_level_misc_object(dom, retail_level_misc_object);
        self.sync_title_card(dom)?;
        self.apply_prepared_retail_music(destination_music, true, destination_zone, dom)
            .map_err(|error| JsValue::from_str(&error))?;
        if let Some(pbak) = &self.retail_pbak {
            dom.log(
                &format!(
                    "Armed retail PBAK {} ({:?}, {} recorded frames); input remains locked until Crash is live.",
                    pbak.eid(),
                    pbak.layout(),
                    pbak.frame_count(),
                ),
                false,
            );
        } else if title_attract_mount {
            dom.log(
                "The title attract transition has no PBAK entry; continuing as a cutscene.",
                false,
            );
        }
        if mount.bonus_return {
            // Native creates the destination object infrastructure, performs
            // one protected Crash spawn, then executes LevelRestart against
            // the carried snapshot before the outer frame's normal scan.
            self.retail_objects.set_initial_crash_save_suppressed(true);
            let protected_spawn = self.spawn_retail_objects(dom, true);
            self.retail_objects.set_initial_crash_save_suppressed(false);
            protected_spawn.map_err(|error| JsValue::from_str(&error))?;
            self.apply_retail_hard_restart(dom)
                .map_err(|error| JsValue::from_str(&error))?;
        }
        self.loading_mount = None;
        self.asset_load_error = None;
        self.scheduler.set_paused(false);
        self.scheduler.reset_deadline();
        self.previous_step_us = None;
        dom.log(
            &format!(
                "Mounted destination {level}: validated {pages} pages, {entries} entries, {} camera zones and {} paths.",
                self.retail_zone_graph.zone_count(),
                self.retail_zone_graph.path_count(),
            ),
            false,
        );
        Ok(())
    }

    fn fail_level_assets(&mut self, level: FormatLevelId, message: &str) {
        if self.loading_mount.as_ref().map(|mount| mount.target) == Some(level) {
            self.loading_mount = None;
        }
        self.asset_load_error = Some(format!("Could not mount {level}: {message}"));
        self.scheduler.set_paused(true);
    }

    fn asset_transition_level(&self) -> Option<FormatLevelId> {
        self.loading_mount
            .as_ref()
            .or(self.pending_mount.as_ref())
            .map(|mount| mount.target)
    }

    fn assets_stalled(&self) -> bool {
        self.asset_transition_level().is_some() || self.asset_load_error.is_some()
    }

    fn queue_asset_level(&mut self, level: LevelId) {
        let Ok(level) = FormatLevelId::new(u32::from(level.raw())) else {
            self.asset_load_error = Some(format!(
                "flow requested an unknown stream id 0x{:02x}",
                level.raw()
            ));
            self.scheduler.set_paused(true);
            return;
        };
        if level != self.level_assets.level
            && self.loading_mount.as_ref().map(|mount| mount.target) != Some(level)
            && self.pending_mount.as_ref().map(|mount| mount.target) != Some(level)
        {
            // Audio and ShaderParamsUpdate share native's process-global
            // `seed_b`; publish any voice-allocation draws before exporting
            // the pointer-free session carry.
            self.retail_objects
                .set_random_seed_b(self.retail_audio.random_seed());
            self.pending_mount = Some(RetailPairMount {
                target: level,
                carry: self.retail_objects.export_session_carry(),
                bonus_return: false,
                core_transition: false,
            });
        }
    }

    fn title_screen_profile(&self) -> Option<RetailTitleScreenProfile> {
        if !matches!(self.flow.state(), FlowState::Title) {
            return None;
        }
        self.retail_objects
            .retail_title_presentation()
            .ok()
            .flatten()
            .map(|title| {
                retail_title_screen_profile(title.screen, self.flow.progress.current_map_level)
            })
    }

    fn authored_title_runtime_active(&self) -> bool {
        matches!(self.flow.state(), FlowState::Title)
            && self.level_assets.level == FormatLevelId::TITLE
            && self
                .retail_objects
                .retail_title_presentation()
                .is_ok_and(|title| title.is_some())
    }

    fn authored_scene_runtime_active(&self) -> bool {
        !matches!(self.flow.state(), FlowState::Boot)
            && !self.retail_objects.arena().is_empty()
            && self.retail_runtime_error.is_none()
    }

    fn title_screen_for_mount(&self, mount: &RetailPairMount) -> Result<TitleScreen, JsValue> {
        if !mount.core_transition && matches!(self.flow.state(), FlowState::Title) {
            return self
                .retail_objects
                .retail_title_presentation()
                .map_err(|error| {
                    JsValue::from_str(&format!(
                        "could not read mounted retail title state: {error:?}"
                    ))
                })?
                .map(|title| title.next_screen)
                .ok_or_else(|| JsValue::from_str("mounted title runtime is not configured"));
        }
        if !self.title_seen {
            return Ok(TitleScreen::PublisherFirst);
        }
        let game_state = mount
            .carry
            .globals
            .get(crust_sim::gool::GAME_STATE_GLOBAL)
            .copied()
            .ok_or_else(|| JsValue::from_str("retail session has no game-state global"))?;
        match game_state {
            0x200 => Ok(TitleScreen::GameOver),
            0x600 => Ok(TitleScreen::MainMenu),
            _ => {
                let raw = mount
                    .carry
                    .globals
                    .get(TITLE_STATE_GLOBAL)
                    .copied()
                    .ok_or_else(|| JsValue::from_str("retail session has no title-state global"))?;
                TitleScreen::from_raw(raw).ok_or_else(|| {
                    JsValue::from_str(&format!(
                        "retail session requested invalid title state {raw:#x}"
                    ))
                })
            }
        }
    }

    fn configure_retail_title_authority(&mut self, first_boot: bool) -> Result<(), JsValue> {
        if !matches!(self.flow.state(), FlowState::Title)
            || self.level_assets.level != FormatLevelId::TITLE
        {
            return Ok(());
        }
        let screen = self.flow.title_screen();
        self.retail_objects
            .configure_retail_title(screen, first_boot)
            .map_err(|error| {
                JsValue::from_str(&format!(
                    "could not configure retail title authority: {error:?}"
                ))
            })?;
        self.flow
            .mirror_retail_title_screen(screen)
            .map_err(|error| {
                JsValue::from_str(&format!(
                    "could not initialize passive title flow mirror: {error:?}"
                ))
            })?;
        Ok(())
    }

    fn live_title_mdat_eid(&self) -> Option<Eid> {
        self.title_screen_profile()
            .filter(|profile| profile.uses_image())
            .and_then(|_| {
                self.retail_objects
                    .retail_title_presentation()
                    .ok()
                    .flatten()
            })
            .and_then(|title| title_mdat_eid(title.screen as u8).ok())
    }

    fn effective_retail_display_mask(&self) -> u32 {
        // TitleLoadState seeds NEXT_DISPLAY_GLOBAL from the screen profile,
        // but subsequent GOOL writes and the one-frame GLUpdate latch remain
        // authoritative just as they are in gameplay.
        self.retail_objects.current_display_mask()
    }

    fn pre_gool_retail_map_path_animation(&self) -> Result<Option<RetailMapPathAnimation>, String> {
        if self.level_assets.level != FormatLevelId::TITLE {
            return Ok(None);
        }
        let title_state = self
            .retail_objects
            .global_word(TITLE_STATE_GLOBAL)
            .map_err(|error| format!("retail title-state global is unavailable: {error:?}"))?;
        if title_state != TitleScreen::Map.raw() {
            return Ok(None);
        }
        let read = |index| {
            self.retail_objects.global_word(index).map_err(|error| {
                format!("retail map-path global {index} is unavailable: {error:?}")
            })
        };
        Ok(Some(RetailMapPathAnimation {
            title_state,
            map_level_links: read(MAP_LEVEL_LINKS_GLOBAL)?,
            map_key_links: read(MAP_KEY_LINKS_GLOBAL)?,
        }))
    }

    fn effective_retail_field_of_view(&self) -> Result<u32, String> {
        if let Some(profile) = self.title_screen_profile() {
            return Ok(profile.field_of_view);
        }
        self.level_assets
            .nsd
            .ldat()
            .map(|ldat| ldat.field_of_view)
            .ok_or_else(|| "playable level has no LDAT camera projection".to_owned())
    }

    fn start_armed_retail_pbak(&mut self, dom: &Dom) -> Result<(), JsValue> {
        let Some(pbak) = self.retail_pbak.as_ref() else {
            return Ok(());
        };
        if !pbak.is_armed() || self.retail_objects.arena().main_object().is_none() {
            return Ok(());
        }
        let eid = pbak.eid();
        let (snapshot, seed, crash_bound) = pbak.start_payload();
        let caption = {
            let current_zone = self.retail_camera.location().path.zone;
            let mut host = BrowserProgramHost::new(
                &self.level_assets.nsd,
                &self.level_assets.nsf,
                &self.level_assets.nsf_bytes,
                &mut self.retail_audio,
                &mut self.retail_zone_pager,
                &mut self.card,
                &mut self.storage,
            );
            self.retail_objects
                .create_retail_demo_caption(current_zone, &mut host)
        }
        .map_err(|error| {
            JsValue::from_str(&format!(
                "could not create retail PBAK {eid} caption controller: {error:?}"
            ))
        })?;
        self.drain_retail_reclaim_diagnostics(dom);
        self.retail_objects
            .install_retail_demo_start(snapshot, seed, crash_bound)
            .map_err(|error| {
                JsValue::from_str(&format!("could not install retail PBAK {eid}: {error:?}"))
            })?;
        self.apply_retail_hard_restart(dom).map_err(|error| {
            JsValue::from_str(&format!("could not start retail PBAK {eid}: {error}"))
        })?;
        self.retail_pbak
            .as_mut()
            .expect("PBAK remained mounted through its same-level restart")
            .mark_started();
        dom.log(
            &format!(
                "Started retail PBAK {eid}; created caption controller {caption:?} and restored its checked camera/player snapshot and gameplay RNG."
            ),
            false,
        );
        Ok(())
    }

    fn record_retail_pbak_finish(
        &mut self,
        reason: DemoEnd,
        outcome: RetailDemoFinishOutcome,
        dom: &Dom,
    ) {
        match outcome {
            RetailDemoFinishOutcome::Released => {
                self.retail_pbak = None;
                dom.log(
                    &format!(
                        "Retail PBAK input ended ({reason:?}); no caption controller retained the authored return lock."
                    ),
                    false,
                );
            }
            RetailDemoFinishOutcome::CaptionEvent {
                recipient,
                dispatch,
                effects: _,
            } => {
                dom.log(
                    &format!(
                        "Retail PBAK input ended ({reason:?}); caption {recipient:?} received event 0xE00 (acknowledged: {}) and retained the authored return lock.",
                        dispatch.acknowledged,
                    ),
                    false,
                );
            }
            RetailDemoFinishOutcome::CaptionEventFault {
                recipient,
                effects: _,
            } => {
                let message = format!(
                    "Retail PBAK input ended ({reason:?}); native ignored caption {recipient:?}'s faulted event 0xE00 handler and retained the authored return lock."
                );
                dom.log(&message, true);
                self.retail_runtime_warning = Some(message);
            }
        }
    }

    fn frame(
        &mut self,
        timestamp_ms: f64,
        held: u16,
        #[cfg(feature = "browser-test-harness")] recorded_override: Option<u32>,
        dom: &Dom,
    ) -> Result<(), JsValue> {
        #[cfg(not(feature = "browser-test-harness"))]
        let recorded_override = None;
        debug_assert!(
            recorded_override.is_none() || held == 0,
            "recorded browser-test input must not be mixed with physical input"
        );
        // Pair validation is asynchronous, but browser input is not. Retain
        // each RAF sample so the destination CoreObjectsCreate PadUpdate uses
        // the current physical state instead of the source level's last tick.
        self.latest_physical_held = held;
        let now_us = (timestamp_ms.max(0.0) * 1_000.0).round() as u64;
        let shader_wall_ticks = (timestamp_ms.max(0.0) / 0.963_765).trunc() as u32;
        let assets_stalled = self.assets_stalled();
        // Native GetTicksElapsed continues through synchronous NSKill/NSInit.
        // Browser validation is asynchronous, so sample its wall-clock gap on
        // every callback even while the cooperative scheduler is frozen.
        // Authored pause remains the sole ordinary wall-time exclusion.
        let shader_clock_was_paused =
            !assets_stalled && self.retail_objects.retail_pause_state().paused();
        advance_retail_shader_clock(
            &mut self.retail_shader_ticks_elapsed,
            &mut self.previous_shader_wall_ticks,
            shader_wall_ticks,
            shader_clock_was_paused,
        );
        #[cfg(feature = "browser-test-harness")]
        let mut simulation_stepped = false;
        if !assets_stalled && self.scheduler.sample(now_us) == FrameDecision::Step {
            #[cfg(feature = "browser-test-harness")]
            {
                simulation_stepped = true;
            }
            let wall_ticks_current_frame = self.previous_step_us.map_or(34, |previous| {
                i32::try_from(now_us.saturating_sub(previous) / 1_000).unwrap_or(i32::MAX)
            });
            self.previous_step_us = Some(now_us);
            let wall_ticks_per_frame = round_retail_ticks(wall_ticks_current_frame);
            // CoreFrame's pause event sees the current wall timing. PBAK may
            // replace it with its prior/Crash boundary later in this frame.
            self.retail_objects
                .set_frame_timing(wall_ticks_current_frame, wall_ticks_per_frame);
            let pending_buttons = std::mem::take(&mut self.pending_buttons);
            let physical_held = if recorded_override.is_some() {
                0
            } else {
                held | pending_buttons
            };
            self.card.update();
            self.retail_objects
                .publish_card_state(self.card.published_state())
                .map_err(|error| {
                    JsValue::from_str(&format!(
                        "could not publish retail card frame state: {error:?}"
                    ))
                })?;
            // CoreFrame reads the snapshot published at Crash's preceding
            // traversal. The physical/demonstration update is deferred to the
            // next BeforeMainObjectUpdate hook below, matching native order.
            let snapshot = self.pad.snapshot();
            let retail_state = is_retail_runtime_state(self.flow.state());
            if retail_state && self.retail_runtime_error.is_none() {
                let title_mdat = self.live_title_mdat_eid();
                let current_zone = self.retail_camera.location().path.zone;
                let pause_update = {
                    let mut host = if let Some(mdat) = title_mdat {
                        BrowserProgramHost::for_title_mdat(
                            &self.level_assets.nsd,
                            &self.level_assets.nsf,
                            &self.level_assets.nsf_bytes,
                            &mut self.retail_audio,
                            &mut self.retail_zone_pager,
                            &mut self.card,
                            &mut self.storage,
                            mdat,
                        )
                    } else {
                        BrowserProgramHost::new(
                            &self.level_assets.nsd,
                            &self.level_assets.nsf,
                            &self.level_assets.nsf_bytes,
                            &mut self.retail_audio,
                            &mut self.retail_zone_pager,
                            &mut self.card,
                            &mut self.storage,
                        )
                    };
                    self.retail_objects.update_retail_pause(
                        snapshot.tapped & u32::from(PAD_START) != 0,
                        current_zone,
                        &mut host,
                    )
                };
                match pause_update {
                    Ok(RetailPauseUpdate::Paused { .. }) => {
                        dom.log("Authored retail pause controller opened.", false);
                    }
                    Ok(RetailPauseUpdate::Failed) => {
                        dom.log(
                            "Authored retail pause controller was unavailable; gameplay continues.",
                            true,
                        );
                    }
                    Ok(RetailPauseUpdate::Resumed {
                        event_faulted: false,
                        ..
                    }) => {
                        dom.log("Authored retail pause controller resumed.", false);
                    }
                    // The typed diagnostic queue below owns the single C00
                    // fault report while native still completes the resume.
                    Ok(
                        RetailPauseUpdate::Resumed {
                            event_faulted: true,
                            ..
                        }
                        | RetailPauseUpdate::Unchanged
                        | RetailPauseUpdate::Blocked,
                    ) => {}
                    Err(error) => {
                        let message = format!("retail pause handshake failed: {error:?}");
                        dom.log(&message, true);
                        self.retail_runtime_error = Some(message);
                        self.retail_tick_state = RetailTickState::Paused;
                    }
                }
                self.drain_retail_reclaim_diagnostics(dom);
            }

            // Native PbakPlay follows the CoreFrame pause gate. Starting an
            // armed recording before that gate would incorrectly publish a
            // nonzero PBAK state one boundary too early.
            self.start_armed_retail_pbak(dom)?;
            // Pause/caption/restart handlers above can allocate voices. Native
            // exposes those draws to the same RNG-B word used by lighting and
            // by a following cross-stream transition.
            self.retail_objects
                .set_random_seed_b(self.retail_audio.random_seed());
            if let Some(recorded_ticks) = self
                .retail_pbak
                .as_ref()
                .and_then(RetailPbakPlayback::pre_shader_ticks_elapsed)
            {
                self.retail_shader_ticks_elapsed = recorded_ticks;
            }
            let pbak_boundary = self
                .retail_pbak
                .as_ref()
                .filter(|playback| playback.uses_crash_boundary())
                .and_then(|playback| {
                    playback
                        .frame_timing(wall_ticks_current_frame, wall_ticks_per_frame)
                        .map(|timing| RetailPbakPadBoundary {
                            physical_held,
                            timing,
                        })
                });
            let pbak_input = if pbak_boundary.is_some() {
                None
            } else {
                self.retail_pbak
                    .as_mut()
                    .map(|pbak| pbak.advance_input(physical_held))
            };
            let (ticks_current_frame, ticks_per_frame, demo_override) =
                if let Some(boundary) = pbak_boundary {
                    (
                        boundary.timing.prior.current,
                        boundary.timing.prior.period,
                        None,
                    )
                } else {
                    pbak_input.map_or_else(
                        || {
                            (
                                wall_ticks_current_frame,
                                wall_ticks_per_frame,
                                recorded_override,
                            )
                        },
                        |input| {
                            (
                                17,
                                input.ticks_per_frame.unwrap_or(wall_ticks_per_frame),
                                Some(input.held),
                            )
                        },
                    )
                };
            self.retail_objects
                .set_frame_timing(ticks_current_frame, ticks_per_frame);

            // CoreFrame consumes a level requested by the preceding GOOL
            // frame before any destination spawn, camera, or object work.
            // Browser pair validation is asynchronous, so this boundary queues
            // an owned session carry and freezes the rest of this 30 Hz step.
            let transition_queued = retail_state
                && self.retail_runtime_error.is_none()
                && self.process_retail_level_transition(dom)?;

            if retail_state
                && !transition_queued
                && self.retail_runtime_error.is_none()
                && let Err(error) = self.update_pending_retail_page()
            {
                let message = format!("retail NSUpdate paging retry failed: {error}");
                dom.log(&message, true);
                self.retail_runtime_error = Some(message);
                self.retail_tick_state = RetailTickState::Paused;
            }

            if retail_state && !transition_queued && self.retail_runtime_error.is_none() {
                let log_spawn_scan = matches!(
                    self.retail_tick_state,
                    RetailTickState::NeedsSpawn | RetailTickState::PausedBeforeSpawn
                );
                if let Err(error) = self.spawn_retail_objects(dom, log_spawn_scan) {
                    let message = format!("retail spawn scan failed: {error}");
                    dom.log(&message, true);
                    self.retail_runtime_error = Some(message);
                    self.retail_tick_state = RetailTickState::Paused;
                }
                // Spawn/once handlers execute through BrowserProgramHost and
                // can allocate voices before the pre-camera shader boundary.
                // Preserve those source-ordered RNG-B draws even when a later
                // descriptor reports a checked failure.
                self.retail_objects
                    .set_random_seed_b(self.retail_audio.random_seed());
            }

            if retail_state && !transition_queued && self.retail_runtime_error.is_none() {
                let mut scene_location = None;
                let native_paused = self.retail_objects.retail_pause_state().paused();
                // Native submits worlds before GOOL. Preserve that exact mask
                // separately from the per-object values latched later during
                // the live preorder traversal.
                let world_display_mask = self.effective_retail_display_mask();
                let vram_fill_color = self.retail_objects.current_vram_fill_color();
                let map_path_animation = match self.pre_gool_retail_map_path_animation() {
                    Ok(animation) => animation,
                    Err(error) => {
                        let message = format!("retail map-path state failed: {error}");
                        dom.log(&message, true);
                        self.retail_runtime_error = Some(message);
                        self.retail_tick_state = RetailTickState::Paused;
                        None
                    }
                };
                let frame_draw_count = self.retail_objects.draw_count();
                let frame_stamp = self.retail_objects.next_frame_stamp();
                if !native_paused {
                    match self
                        .retail_objects
                        .advance_level_shader_at(self.retail_shader_ticks_elapsed)
                    {
                        Ok(cue) => {
                            // ShaderParamsUpdate may have consumed `randb`;
                            // publish its exact resulting word to audio before
                            // the optional thunder voice allocation.
                            self.retail_audio
                                .set_random_seed(self.retail_objects.random_seed_b());
                            if let Some(cue) = cue
                                && let Err(error) = self.play_retail_thunder(cue)
                            {
                                let message = format!(
                                    "retail thunder audio was skipped after shader update: {error}"
                                );
                                dom.log(&message, true);
                                self.retail_runtime_warning = Some(message);
                            }
                            self.retail_objects
                                .set_random_seed_b(self.retail_audio.random_seed());
                        }
                        Err(error) => {
                            let message = format!("retail level shader update failed: {error:?}");
                            dom.log(&message, true);
                            self.retail_runtime_error = Some(message);
                            self.retail_tick_state = RetailTickState::Paused;
                        }
                    }
                }
                let world_shader_snapshot = self.retail_objects.world_shader_snapshot();
                let camera_location = if self.retail_runtime_error.is_none() {
                    if native_paused {
                        Some(self.retail_camera.location())
                    } else {
                        match self.update_retail_camera(snapshot, dom) {
                            Ok(step) => Some(step.after),
                            Err(error) => {
                                let message = format!("retail camera update failed: {error}");
                                dom.log(&message, true);
                                self.retail_runtime_error = Some(message);
                                self.retail_tick_state = RetailTickState::Paused;
                                None
                            }
                        }
                    }
                } else {
                    None
                };
                // GfxUpdateMatrices consumes the CamUpdate/CamDeath pose
                // before native enters GoolUpdateObjects. Keep that exact
                // frame snapshot: a same-frame LoadState may legitimately
                // replace the persistent camera for the following frame, but
                // it cannot retroactively change the worlds already submitted
                // with this one.
                let camera_pose_override = self.retail_camera_pose_override;
                let texture_frame_snapshot = self.retail_zone_pager.texture_frame_snapshot();
                if self.retail_runtime_error.is_none()
                    && self.retail_tick_state == RetailTickState::Running
                {
                    self.tick_retail_runtime(dom, pbak_boundary, physical_held, demo_override);
                    // GOOL audio commands run through the external browser
                    // host; reconcile their voice-allocation draws before
                    // any carry can be captured on the following frame.
                    self.retail_objects
                        .set_random_seed_b(self.retail_audio.random_seed());
                }
                if let Some(camera_location) = camera_location {
                    // Native submits worlds before GOOL but resolves
                    // `GLClear`'s zone afterward. A synchronous LoadState can
                    // therefore replace `cur_zone` without retroactively
                    // changing the already-submitted world camera.
                    let background_zone = self.retail_camera.location().path.zone;
                    let count_draws =
                        !native_paused && self.effective_retail_display_mask() & 0x1000 != 0;
                    let trace = self.retail_frame.tick_with_draw_count_enabled(count_draws);
                    self.show_loading_image =
                        matches!(trace.presented(), PresentedFrame::LoadingImage);
                    // World transforms precede GL's later draw-skip gate.
                    // Rebuild hidden frames too so ripple and persistent
                    // world-shader scratch advance from the actual camera,
                    // visibility, and display mask rather than a speculative
                    // first-presented snapshot.
                    if is_retail_runtime_state(self.flow.state()) {
                        scene_location = Some((
                            camera_location,
                            background_zone,
                            frame_draw_count,
                            frame_stamp,
                            !native_paused,
                            world_display_mask,
                            vram_fill_color,
                            map_path_animation,
                            camera_pose_override,
                            texture_frame_snapshot,
                            world_shader_snapshot,
                        ));
                    }
                }
                if let Some((
                    camera_location,
                    background_zone,
                    draw_count,
                    frame_stamp,
                    advance_world_ripple,
                    world_display_mask,
                    vram_fill_color,
                    map_path_animation,
                    camera_pose_override,
                    texture_frame_snapshot,
                    world_shader_snapshot,
                )) = scene_location
                    && let Err(error) = self.update_retail_scene(
                        camera_location,
                        background_zone,
                        draw_count,
                        frame_stamp,
                        advance_world_ripple,
                        world_display_mask,
                        vram_fill_color,
                        map_path_animation,
                        camera_pose_override,
                        texture_frame_snapshot,
                        world_shader_snapshot,
                    )
                {
                    let message = format!("retail scene update failed: {}", js_message(&error));
                    dom.log(&message, true);
                    self.retail_runtime_error = Some(message);
                    self.retail_tick_state = match self.retail_tick_state {
                        RetailTickState::NeedsSpawn | RetailTickState::PausedBeforeSpawn => {
                            RetailTickState::PausedBeforeSpawn
                        }
                        RetailTickState::Running | RetailTickState::Paused => {
                            RetailTickState::Paused
                        }
                    };
                }
            } else if !retail_state {
                let trace = self.retail_frame.tick();
                self.show_loading_image = matches!(trace.presented(), PresentedFrame::LoadingImage);
            }

            if !transition_queued {
                self.handle_events(dom);
                if self.pending_mount.is_none() {
                    self.handle_events(dom);
                }
                if let Some(audio) = &mut self.audio {
                    audio.tick_30_hz();
                }
                self.retail_audio.tick_30_hz();
                self.retail_master_fade.tick_30_hz();
                if let Some(audio) = &mut self.audio {
                    audio.set_retail_master_gain(self.retail_master_fade.normalized_gain());
                }
                let resume_payload = self.resume.update(self.save_data());
                if self.level_access.persists()
                    && let Some(payload) = resume_payload
                    && let Some(storage) = &self.storage
                {
                    storage.persist_resume(payload)?;
                }
            }
        }
        if let Some(audio) = &mut self.audio {
            #[cfg(feature = "browser-test-harness")]
            if simulation_stepped {
                audio.schedule_browser_test_frame(&mut self.retail_audio);
            }
            #[cfg(not(feature = "browser-test-harness"))]
            audio.schedule(&mut self.retail_audio)?;
        }
        self.sync_title_card(dom)?;
        let assets_stalled = self.assets_stalled();
        let show_title_image = !assets_stalled
            && matches!(self.flow.state(), FlowState::Title)
            && self
                .title_screen_profile()
                .is_some_and(RetailTitleScreenProfile::uses_image);
        let show_title_objects = self
            .title_screen_profile()
            .is_some_and(|profile| profile.screen_type == RetailTitleScreenType::ImageAndObjects);
        // PbakChoose arms native `ns.draw_skip_counter = -1`; PbakPlay clears
        // it only after Crash exists. Geometry still executes while armed,
        // but GLShouldRenderFrame suppresses submission indefinitely.
        let pbak_draw_suppressed = self
            .retail_pbak
            .as_ref()
            .is_some_and(RetailPbakPlayback::is_armed);
        let pbak_holds_loading_image =
            self.pbak_armed_presentation == Some(PbakArmedPresentation::LoadingImage);
        let title_overlay_alpha = self
            .retail_objects
            .retail_title_presentation()
            .ok()
            .flatten()
            .map(|title| {
                retail_title_overlay_alpha(
                    title.phase,
                    title.fade_counter,
                    title.opaque_swap_overlay,
                )
            })
            .unwrap_or_default();
        let interpolation_alpha = if self.display_settings.smooth_motion {
            self.previous_step_us.map_or(u16::MAX, |previous| {
                let elapsed = now_us.saturating_sub(previous).min(FRAME_US);
                let scaled = elapsed.saturating_mul(u64::from(u16::MAX)) / FRAME_US;
                u16::try_from(scaled).unwrap_or(u16::MAX)
            })
        } else {
            u16::MAX
        };
        self.stage.render(
            VisualState {
                show_title_image,
                // Type-three title screens composite animated GOOL objects over
                // their MDAT image. Type-zero screens suppress even a scene still
                // resident from the preceding state before the next 30 Hz step.
                show_retail_scene: !assets_stalled
                    && (!pbak_draw_suppressed || !pbak_holds_loading_image)
                    && (!show_title_image || show_title_objects),
                show_loading_image: !assets_stalled
                    && (self.show_loading_image
                        || (pbak_draw_suppressed && pbak_holds_loading_image)),
                title_overlay_alpha,
                interpolation_alpha,
            },
            self.display_settings.aspect.logical_viewport(),
        )?;
        self.last_gl_error = self.stage.error();
        self.render_ui(dom)?;
        Ok(())
    }

    fn spawn_retail_objects(&mut self, dom: &Dom, log_scan: bool) -> Result<(), String> {
        let title_mdat = self.live_title_mdat_eid();
        let attempts = {
            let spawn_scan = self.retail_zone_lifecycle.next_frame_spawn_scan();
            let neighbors = spawn_scan
                .iter()
                .map(|candidate| {
                    self.retail_zones
                        .get(&candidate.zone)
                        .ok_or_else(|| {
                            format!(
                                "active lifecycle zone {} is absent from the owned descriptor catalog",
                                candidate.zone
                            )
                        })
                        .map(|zone| NeighborZone {
                            eid: zone.eid,
                            display_flags: candidate.display_flags,
                            entities: zone.entities.as_slice(),
                        })
                })
                .collect::<Result<Vec<_>, String>>()?;
            let mut host = if let Some(mdat) = title_mdat {
                BrowserProgramHost::for_title_mdat(
                    &self.level_assets.nsd,
                    &self.level_assets.nsf,
                    &self.level_assets.nsf_bytes,
                    &mut self.retail_audio,
                    &mut self.retail_zone_pager,
                    &mut self.card,
                    &mut self.storage,
                    mdat,
                )
            } else {
                BrowserProgramHost::new(
                    &self.level_assets.nsd,
                    &self.level_assets.nsf,
                    &self.level_assets.nsf_bytes,
                    &mut self.retail_audio,
                    &mut self.retail_zone_pager,
                    &mut self.card,
                    &mut self.storage,
                )
            };
            self.retail_objects
                .spawn_current_zone_neighbors(&neighbors, &mut host)
        };
        self.drain_retail_reclaim_diagnostics(dom);
        let counts = browser_spawn_counts(&attempts);
        self.retail_metrics.record_spawns(counts);
        self.retail_tick_state = match self.retail_tick_state {
            RetailTickState::NeedsSpawn | RetailTickState::Running => RetailTickState::Running,
            RetailTickState::PausedBeforeSpawn | RetailTickState::Paused => RetailTickState::Paused,
        };
        let unexpected = first_unexpected_browser_spawn(&attempts);
        if let Some(error) = unexpected {
            self.retail_runtime_warning = Some(format!(
                "Retail spawn scan reached {} unexpected failure(s); first error: {error:?}",
                counts.failed,
            ));
        }
        if log_scan || counts.successful != 0 || unexpected.is_some() {
            dom.log(
                &format!(
                    "Retail spawn scan covered {} active neighbor zones: {} new bindings, {} already active, {} authored sentinel rejection(s), {} unexpected failures from {} group-3 entities.",
                    self.retail_zone_lifecycle.next_frame_spawn_scan().len(),
                    counts.successful,
                    counts.already_active,
                    counts.authored_rejections,
                    counts.failed,
                    counts.attempts,
                ),
                unexpected.is_some(),
            );
        }
        self.sync_completed_card_load(dom);
        Ok(())
    }

    fn tick_retail_runtime(
        &mut self,
        dom: &Dom,
        pbak_boundary: Option<RetailPbakPadBoundary>,
        physical_held: u16,
        demo_override: Option<u32>,
    ) {
        let authored_title = self.authored_title_runtime_active();
        let title_mdat = self.live_title_mdat_eid();
        let mut pbak_finish = None;
        let mut pbak_finish_effects_applied = false;
        let result = {
            let mut host = if let Some(mdat) = title_mdat {
                BrowserProgramHost::for_title_mdat(
                    &self.level_assets.nsd,
                    &self.level_assets.nsf,
                    &self.level_assets.nsf_bytes,
                    &mut self.retail_audio,
                    &mut self.retail_zone_pager,
                    &mut self.card,
                    &mut self.storage,
                    mdat,
                )
            } else {
                BrowserProgramHost::new(
                    &self.level_assets.nsd,
                    &self.level_assets.nsf,
                    &self.level_assets.nsf_bytes,
                    &mut self.retail_audio,
                    &mut self.retail_zone_pager,
                    &mut self.card,
                    &mut self.storage,
                )
            };
            if let Some(boundary) = pbak_boundary {
                let playback = &mut self.retail_pbak;
                let pad = &mut self.pad;
                self.retail_objects
                    .run_frame_before_display_with_traversal_hook(
                        &mut host,
                        RETAIL_INSTRUCTION_BUDGET,
                        |runtime, host, point| {
                            let RetailTraversalBoundary::BeforeMainObjectUpdate { .. } = point;
                            let released_return = playback
                                .as_ref()
                                .is_some_and(RetailPbakPlayback::is_returning)
                                && runtime
                                    .global_word(PBAK_STATE_GLOBAL)
                                    .map_err(RuntimeError::Vm)?
                                    != 3;
                            if released_return {
                                *playback = None;
                                pad.update(boundary.physical_held, 0, None);
                                runtime
                                    .set_pad_snapshot(0, retail_pad_snapshot(pad.snapshot()))
                                    .map_err(RuntimeError::Vm)?;
                                return Ok(());
                            }
                            let Some(playback) = playback.as_mut() else {
                                return Ok(());
                            };
                            let (input, end) =
                                playback.advance_pad_boundary(boundary.physical_held);
                            debug_assert!(
                                input.ticks_per_frame.is_none()
                                    || input.ticks_per_frame == Some(boundary.timing.crash.period),
                                "pre-Crash timing must match the frame consumed by PadUpdatePbak"
                            );
                            runtime.set_frame_timing(
                                boundary.timing.crash.current,
                                boundary.timing.crash.period,
                            );
                            let previous_pad = pad.snapshot();
                            pad.update(boundary.physical_held, 0, Some(input.held));
                            let updated_pad = pad.snapshot();
                            if let Some(reason) = end {
                                runtime
                                    .set_pad_snapshot(
                                        0,
                                        pbak_event_pad_snapshot(
                                            retail_pad_snapshot(previous_pad),
                                            retail_pad_snapshot(updated_pad),
                                        ),
                                    )
                                    .map_err(RuntimeError::Vm)?;
                                pbak_finish = Some((reason, runtime.finish_retail_demo(host)?));
                            }
                            runtime
                                .set_pad_snapshot(0, retail_pad_snapshot(updated_pad))
                                .map_err(RuntimeError::Vm)?;
                            Ok(())
                        },
                    )
            } else {
                let pad = &mut self.pad;
                self.retail_objects
                    .run_frame_before_display_with_traversal_hook(
                        &mut host,
                        RETAIL_INSTRUCTION_BUDGET,
                        |runtime, _host, point| {
                            let RetailTraversalBoundary::BeforeMainObjectUpdate { .. } = point;
                            pad.update(physical_held, 0, demo_override);
                            runtime
                                .set_pad_snapshot(0, retail_pad_snapshot(pad.snapshot()))
                                .map_err(RuntimeError::Vm)
                        },
                    )
            }
        };
        let restart_before_display = result.as_ref().is_ok_and(|frame| {
            frame
                .effects
                .iter()
                .any(|effect| matches!(effect, VmEffect::LoadState { .. }))
        });
        // Native LevelRestart is synchronous inside GOOL. The pointer-free
        // host applies its structural effect after traversal, so defer
        // TitleUpdate/GLUpdate only for that frame and complete them as soon
        // as the restart transaction has committed.
        let boundary_error = result.as_ref().ok().and_then(|_| {
            (!restart_before_display)
                .then(|| self.complete_retail_source_frame(dom, authored_title).err())
                .flatten()
        });
        self.drain_retail_reclaim_diagnostics(dom);
        match result {
            Ok(frame) => {
                let frame_executions = frame.executions.len();
                let frame_execution_errors = frame
                    .executions
                    .iter()
                    .filter(|execution| execution.result.is_err())
                    .count();
                let errors_before = self.retail_metrics.execution_errors;
                let first_error = frame
                    .executions
                    .iter()
                    .find_map(|execution| execution.result.as_ref().err())
                    .map(|error| format!("{error:?}"));
                self.retail_metrics.record_frame(&frame);
                if errors_before == 0
                    && self.retail_metrics.execution_errors != 0
                    && let Some(error) = first_error
                {
                    let message = format!(
                        "Retail GOOL reached a checked object execution boundary on frame {}; first error: {error}",
                        frame.frame_index
                    );
                    dom.log(&message, true);
                    self.retail_runtime_warning = Some(message);
                }
                if frame.frame_index == 0 {
                    dom.log(
                        &format!(
                            "Retail GOOL frame 0 ran {frame_executions} objects ({frame_execution_errors} errors), created {} runtime children and emitted {} effects.",
                            frame.spawned_children.len(),
                            frame.effects.len(),
                        ),
                        frame_execution_errors != 0,
                    );
                }
                if let Some(error) = boundary_error {
                    let message = format!("retail title/display boundary failed: {error}");
                    dom.log(&message, true);
                    self.retail_runtime_error = Some(message);
                    self.retail_tick_state = RetailTickState::Paused;
                } else {
                    match self.apply_retail_gool_level_effects(&frame.effects, dom) {
                        Ok(()) => {
                            let boundary_result = if restart_before_display {
                                self.complete_retail_source_frame(dom, authored_title)
                            } else {
                                Ok(())
                            };
                            match boundary_result {
                                Ok(()) => {
                                    pbak_finish_effects_applied = pbak_finish.is_some();
                                }
                                Err(error) => {
                                    let message =
                                        format!("retail title/display boundary failed: {error}");
                                    dom.log(&message, true);
                                    self.retail_runtime_error = Some(message);
                                    self.retail_tick_state = RetailTickState::Paused;
                                }
                            }
                        }
                        Err(error) => {
                            let message = format!("retail save/restart effect failed: {error}");
                            dom.log(&message, true);
                            self.retail_runtime_error = Some(message);
                            self.retail_tick_state = RetailTickState::Paused;
                        }
                    }
                }
            }
            Err(error) => {
                let mut message = format!("retail GOOL frame failed: {error:?}");
                if let Some((_, outcome)) = pbak_finish.as_ref() {
                    // A failure after the Crash traversal hook returns no
                    // RuntimeFrame, and the machine discards its pending
                    // effects at the next frame boundary. Recover only the
                    // caption event's captured prefix before recording the
                    // handoff; successful frames apply the same effects from
                    // `frame.effects` above.
                    match self.apply_retail_gool_level_effects(outcome.effects(), dom) {
                        Ok(()) => pbak_finish_effects_applied = true,
                        Err(effect_error) => {
                            let _ = write!(
                                message,
                                "; PBAK completion effect recovery failed: {effect_error}"
                            );
                            self.retail_tick_state = RetailTickState::Paused;
                        }
                    }
                }
                dom.log(&message, true);
                self.retail_runtime_error = Some(message);
            }
        }
        if pbak_finish_effects_applied && let Some((reason, outcome)) = pbak_finish {
            self.record_retail_pbak_finish(reason, outcome, dom);
        }
        self.sync_completed_card_load(dom);
        if self.retail_runtime_error.is_none()
            && let Err(error) = self.sync_retail_globals_to_flow(dom)
        {
            dom.log(&error, true);
            self.retail_runtime_error = Some(error);
            self.retail_tick_state = RetailTickState::Paused;
        }
    }

    fn update_pending_retail_page(&mut self) -> Result<(), String> {
        let Some(outcome) = self
            .retail_zone_pager
            .update_pending_virtual_page()
            .map_err(|error| format!("pager update failed: {error:?}"))?
        else {
            return Ok(());
        };
        match outcome {
            PagerUpdateOutcome::Invalidated(pages) => self
                .retail_objects
                .apply_platform_paging_evictions(&pages)
                .map_err(|error| format!("could not publish reserved-page evictions: {error:?}")),
            PagerUpdateOutcome::Resolved(outcome) => {
                debug_assert!(outcome.resolved);
                self.retail_objects
                    .apply_platform_paging_resolution(outcome.page, outcome.invalidated)
                    .map_err(|error| format!("could not publish promoted page: {error:?}"))
            }
        }
    }

    fn complete_retail_source_frame(
        &mut self,
        dom: &Dom,
        authored_title: bool,
    ) -> Result<(), String> {
        if authored_title {
            let previous = self
                .retail_objects
                .retail_title_presentation()
                .map_err(|error| format!("could not read title state: {error:?}"))?
                .ok_or_else(|| "authored title runtime is not configured".to_owned())?;
            let action = self
                .retail_objects
                .begin_retail_title_update()
                .map_err(|error| format!("TitleUpdate start failed: {error:?}"))?;
            if let Some(RetailTitleAction::LoadScreen { screen, .. }) = action {
                self.flow
                    .mirror_retail_title_screen(screen)
                    .map_err(|error| format!("passive title mirror failed: {error:?}"))?;
                self.sync_title_card(dom)
                    .map_err(|error| format!("TitleLoadState failed: {}", js_message(&error)))?;
            }
            self.retail_objects
                .finish_retail_title_update()
                .map_err(|error| format!("TitleUpdate request compare failed: {error:?}"))?;
            let current = self
                .retail_objects
                .retail_title_presentation()
                .map_err(|error| format!("could not read updated title state: {error:?}"))?
                .ok_or_else(|| "authored title runtime disappeared".to_owned())?;
            if current.next_screen != previous.next_screen {
                dom.log(
                    &format!(
                        "Retail GOOL requested title state {}.",
                        current.next_screen.raw()
                    ),
                    false,
                );
            }
        }
        self.retail_objects
            .finish_deferred_display_frame()
            .map_err(|error| format!("GLUpdate failed: {error:?}"))?
            .map(|_| ())
            .ok_or_else(|| "GLUpdate remained deferred by an unapplied level restart".to_owned())
    }

    fn sync_completed_card_load(&mut self, dom: &Dom) {
        let Some(save) = self.retail_objects.take_card_load() else {
            return;
        };
        apply_save(&mut self.flow, save);
        self.last_authoritative_save = save;
        if let Err(error) =
            apply_level_access(self.level_access, &mut self.retail_objects, &mut self.flow)
        {
            let message =
                format!("Could not restore temporary all-level access after card load: {error:?}");
            dom.log(&message, true);
            self.retail_runtime_error = Some(message);
            self.retail_tick_state = RetailTickState::Paused;
            return;
        }
        self.last_authoritative_save = authoritative_save_or_last(
            self.retail_objects.card_save_data(),
            self.last_authoritative_save,
        );
        self.retail_audio
            .set_sfx_volume(self.flow.options.sfx_volume);
        if let Some(audio) = &mut self.audio {
            audio.set_output_options(output_options(self.flow.options));
        }
        dom.log(
            "Restored retail progression and audio options from the selected virtual-card slot.",
            false,
        );
    }

    fn sync_retail_globals_to_flow(&mut self, dom: &Dom) -> Result<(), String> {
        if self.level_access.all_levels() {
            apply_all_levels_override(&mut self.retail_objects).map_err(|error| {
                format!("could not enforce temporary all-level access: {error:?}")
            })?;
        }
        let read = |index| {
            self.retail_objects
                .global_word(index)
                .map_err(|error| format!("retail global {index} is unavailable: {error:?}"))
        };
        let level_count = read(LEVEL_COUNT_GLOBAL)?;
        let levels_unlocked = read(LEVELS_UNLOCKED_GLOBAL)?;
        let current_map_level = read(CURRENT_MAP_LEVEL_GLOBAL)?;
        let gem_count = read(GEM_COUNT_GLOBAL)?;
        let key_count = read(KEY_COUNT_GLOBAL)?;
        let item_pool_1 = read(ITEM_POOL_1_GLOBAL)?;
        let item_pool_2 = read(ITEM_POOL_2_GLOBAL)?;
        let box_count = read(BOX_COUNT_GLOBAL)?.cast_signed();
        let checkpoint_id = read(CHECKPOINT_ID_GLOBAL)?.cast_signed();
        let checkpoint_translation = [
            read(CHECKPOINT_TRANSLATION_GLOBALS[0])?.cast_signed(),
            read(CHECKPOINT_TRANSLATION_GLOBALS[1])?.cast_signed(),
            read(CHECKPOINT_TRANSLATION_GLOBALS[2])?.cast_signed(),
        ];
        let options = GameOptions {
            mono: read(MONO_GLOBAL)? != 0,
            sfx_volume: read(SFX_VOLUME_GLOBAL)?.min(u32::from(u8::MAX)) as u8,
            music_volume: read(MUSIC_VOLUME_GLOBAL)?.min(u32::from(u8::MAX)) as u8,
        };

        self.flow.progress.level_count = level_count;
        self.flow.progress.levels_unlocked = levels_unlocked;
        self.flow.progress.current_map_level = current_map_level;
        self.flow.progress.gem_count = gem_count.min(u32::from(u8::MAX)) as u8;
        self.flow.progress.key_count = key_count;
        self.flow.progress.item_pool_1 = item_pool_1;
        self.flow.progress.item_pool_2 = item_pool_2;
        if self.flow.options != options {
            self.flow.options = options;
            self.retail_audio.set_sfx_volume(options.sfx_volume);
            if let Some(audio) = &mut self.audio {
                audio.set_output_options(output_options(options));
            }
            dom.log("Applied authored retail audio options.", false);
        }
        if let Some(mut context) = self.retail_objects.level_state_context().cloned() {
            context.box_count = box_count;
            context.checkpoint_id = checkpoint_id;
            context.checkpoint_translation = checkpoint_translation;
            self.retail_objects.set_level_state_context(context);
        }
        self.last_authoritative_save = self
            .retail_objects
            .card_save_data()
            .map_err(|error| format!("retail save globals are unavailable: {error:?}"))?;
        Ok(())
    }

    fn drain_retail_reclaim_diagnostics(&mut self, dom: &Dom) {
        for cleanup in self.retail_objects.take_cleanup_actions() {
            let RuntimeCleanupAction::FreeObjectAudio(object) = cleanup;
            self.retail_audio.free_owner(object.vm());
        }
        let faults = self.retail_objects.take_reclaim_event_faults();
        if !faults.is_empty() {
            let message = format!(
                "Native object-pool reclaim ignored {} faulted TERM handler(s); first object: {:?}.",
                faults.len(),
                faults[0].object,
            );
            dom.log(&message, true);
            self.retail_runtime_warning = Some(message);
        }
        let pause_faults = self.retail_objects.take_pause_event_faults();
        if !pause_faults.is_empty() {
            let message = format!(
                "Native pause resume ignored {} faulted C00 handler(s); first controller: {:?}.",
                pause_faults.len(),
                pause_faults[0].object,
            );
            dom.log(&message, true);
            self.retail_runtime_warning = Some(message);
        }
        let invincibility_faults = self.retail_objects.take_invincibility_event_faults();
        if !invincibility_faults.is_empty() {
            let first = invincibility_faults[0];
            let message = format!(
                "Native invincibility collision ignored {} faulted GOOL handler(s); first sender: {:?}, recipient: {:?}, event: 0x{:X}.",
                invincibility_faults.len(),
                first.sender,
                first.recipient,
                first.event,
            );
            dom.log(&message, true);
            self.retail_runtime_warning = Some(message);
        }
        let solid_faults = self.retail_objects.take_solid_event_faults();
        if !solid_faults.is_empty() {
            let first = solid_faults[0];
            let message = format!(
                "Native solid motion ignored {} faulted GOOL event handler(s); first mover: {:?}, recipient: {:?}, event: 0x{:X}, reason: {:?}.",
                solid_faults.len(),
                first.moving_object,
                first.recipient,
                first.event,
                first.reason,
            );
            dom.log(&message, true);
            self.retail_runtime_warning = Some(message);
        }
    }

    fn process_retail_level_transition(&mut self, dom: &Dom) -> Result<bool, JsValue> {
        if self.next_lid == -1 && self.level_assets.level != FormatLevelId::TITLE {
            let game_state = self
                .retail_objects
                .global_word(crust_sim::gool::GAME_STATE_GLOBAL)
                .map_err(|error| {
                    JsValue::from_str(&format!(
                        "could not read retail transition game state: {error:?}"
                    ))
                })?;
            if matches!(game_state, 0x200 | 0x300 | 0x400) {
                self.next_lid = i32::try_from(FormatLevelId::TITLE.get())
                    .expect("retail title level fits signed 32-bit");
            }
        }
        if self.next_lid == -1 {
            return Ok(false);
        }

        let requested_lid = std::mem::replace(&mut self.next_lid, -1);
        let title_mdat = self.live_title_mdat_eid();
        let report = {
            let mut host = if let Some(mdat) = title_mdat {
                BrowserProgramHost::for_title_mdat(
                    &self.level_assets.nsd,
                    &self.level_assets.nsf,
                    &self.level_assets.nsf_bytes,
                    &mut self.retail_audio,
                    &mut self.retail_zone_pager,
                    &mut self.card,
                    &mut self.storage,
                    mdat,
                )
            } else {
                BrowserProgramHost::new(
                    &self.level_assets.nsd,
                    &self.level_assets.nsf,
                    &self.level_assets.nsf_bytes,
                    &mut self.retail_audio,
                    &mut self.retail_zone_pager,
                    &mut self.card,
                    &mut self.storage,
                )
            };
            self.retail_objects
                .finish_level_transition(&mut host, requested_lid)
        };
        // LEVEL_END handlers can allocate voices. Native carries their shared
        // RNG-B result through NSKill/NSInit even though the Rust audio owner
        // itself is replaced for the destination pair.
        self.retail_objects
            .set_random_seed_b(self.retail_audio.random_seed());
        let mut report = report.map_err(|error| {
            JsValue::from_str(&format!("retail LEVEL_END phase failed: {error:?}"))
        })?;
        report
            .carry
            .set_random_seed_b(self.retail_audio.random_seed());
        self.retail_objects
            .reset_retail_pause_for_screen_load()
            .map_err(|error| {
                JsValue::from_str(&format!(
                    "could not reset retail pause state for the destination mount: {error:?}"
                ))
            })?;
        self.drain_retail_reclaim_diagnostics(dom);

        let residual_effects = report
            .effects
            .iter()
            .filter(|effect| {
                !matches!(effect, VmEffect::Transition(_) | VmEffect::LoadState { .. })
            })
            .cloned()
            .collect::<Vec<_>>();
        self.apply_retail_gool_level_effects(&residual_effects, dom)
            .map_err(|error| JsValue::from_str(&error))?;
        if !report.event_failures.is_empty() {
            let message = format!(
                "Retail LEVEL_END reached {} checked handler failure(s); first object: {:?}.",
                report.event_failures.len(),
                report.event_failures[0].object,
            );
            dom.log(&message, true);
            self.retail_runtime_warning = Some(message);
        }

        let target = report.resolved.level;
        let raw = u8::try_from(target.get()).map_err(|_| {
            JsValue::from_str(&format!("retail transition target {target} is too large"))
        })?;
        let flow_target = LevelId::new(raw).ok_or_else(|| {
            JsValue::from_str(&format!(
                "retail transition target {target} is not playable"
            ))
        })?;
        if !flow_target.is_playable() {
            return Err(JsValue::from_str(&format!(
                "retail transition target {target} is the non-playable Cave archive"
            )));
        }
        self.pending_mount = Some(RetailPairMount {
            target,
            carry: report.carry,
            bonus_return: report.resolved.bonus_return,
            core_transition: true,
        });
        dom.log(
            &format!(
                "Retail LEVEL_END resolved {requested_lid} to {target} (bonus return: {}).",
                report.resolved.bonus_return,
            ),
            !report.event_failures.is_empty(),
        );
        Ok(true)
    }

    fn apply_retail_gool_level_effects(
        &mut self,
        effects: &[VmEffect],
        dom: &Dom,
    ) -> Result<(), String> {
        for effect in effects {
            match *effect {
                VmEffect::MidiTogglePlayback { object, value } => {
                    if let Some(audio) = &mut self.audio {
                        let change = audio.toggle_retail_music(value);
                        if change != RetailMusicChange::Unchanged {
                            dom.log(
                                &format!(
                                    "Retail GOOL object {object:?} toggled MIDI playback with {value:#x} ({change:?})."
                                ),
                                false,
                            );
                        }
                    }
                }
                VmEffect::ResetMasterFadeStep { object } => {
                    self.retail_master_fade.reset_step();
                    dom.log(
                        &format!(
                            "Retail GOOL object {object:?} started the native master-volume fade."
                        ),
                        false,
                    );
                }
                VmEffect::LoadState {
                    saved_level: Some(saved_level),
                    ..
                } => {
                    #[cfg(feature = "browser-test-harness")]
                    {
                        self.browser_test_cumulative_metrics.load_states = self
                            .browser_test_cumulative_metrics
                            .load_states
                            .saturating_add(1);
                    }
                    let replaces_live_objects = saved_level == self.level_assets.level;
                    self.apply_retail_hard_restart_from_effect(dom, saved_level)?;
                    if replaces_live_objects {
                        // A same-level restart invalidates ordinary object
                        // identities and resumes from the restored band. A
                        // different-level LoadState only schedules `-2`, so
                        // its authored post-load effect tail remains valid.
                        break;
                    }
                }
                VmEffect::LoadState {
                    saved_level: None, ..
                } => return Err("GOOL LoadState reached the browser without a saved level".into()),
                VmEffect::Transition(level) => {
                    self.next_lid = level;
                }
                _ => {
                    // The runtime applies SaveState misc 12/0 inside the
                    // interpreter host, before the following GOOL
                    // instruction. Re-saving here would incorrectly capture
                    // end-of-frame state. Other effects have their own typed
                    // host/lifecycle owners.
                }
            }
        }
        Ok(())
    }

    fn apply_retail_hard_restart(&mut self, dom: &Dom) -> Result<(), String> {
        let saved_level = self
            .retail_objects
            .saved_level_state()
            .map(|snapshot| snapshot.level)
            .ok_or_else(|| "GOOL misc 12/1 has no saved level state".to_owned())?;
        self.apply_retail_hard_restart_from_effect(dom, saved_level)
    }

    fn apply_retail_hard_restart_from_effect(
        &mut self,
        dom: &Dom,
        captured_saved_level: FormatLevelId,
    ) -> Result<(), String> {
        #[cfg(feature = "browser-test-harness")]
        {
            self.browser_test_cumulative_metrics.hard_restarts = self
                .browser_test_cumulative_metrics
                .hard_restarts
                .saturating_add(1);
        }
        let snapshot = self
            .retail_objects
            .saved_level_state()
            .cloned()
            .ok_or_else(|| "GOOL misc 12/1 has no saved level state".to_owned())?;
        if captured_saved_level != self.level_assets.level {
            let outcome = {
                let mut host = BrowserProgramHost::new(
                    &self.level_assets.nsd,
                    &self.level_assets.nsf,
                    &self.level_assets.nsf_bytes,
                    &mut self.retail_audio,
                    &mut self.retail_zone_pager,
                    &mut self.card,
                    &mut self.storage,
                );
                self.retail_objects
                    .restart_saved_level_from_effect(&mut host, captured_saved_level)
            }
            .map_err(|error| format!("different-level restart failed: {error:?}"))?;
            let RetailRestartOutcome::DifferentLevel { .. } = outcome else {
                return Err("different-level restart unexpectedly stayed in this stream".to_owned());
            };
            self.next_lid = -2;
            return Ok(());
        }
        let restored_music =
            self.prepare_retail_music(&self.level_assets, snapshot.location.path.zone, false)?;
        let activation_marker = self.level_assets.level != FormatLevelId::TITLE;
        let plan = self
            .retail_zone_lifecycle
            .plan_hard_restart(snapshot.location.path.zone, activation_marker)
            .map_err(|error| format!("could not plan hard restart: {error}"))?;
        let mut pager_validation = self.retail_zone_pager.clone();
        let restored_load_list = self
            .retail_zone_lifecycle
            .zone(snapshot.location.path.zone)
            .ok_or_else(|| "restored texture zone is absent from the lifecycle".to_owned())?
            .load_list();
        pager_validation
            .set_current_texture_load_eids(restored_load_list.entries().iter().copied());
        for action in plan.actions().iter().copied() {
            apply_retail_zone_paging_action(&mut pager_validation, action)?;
        }
        let mut lifecycle_preview = self.retail_zone_lifecycle.clone();
        lifecycle_preview
            .commit_hard_restart(&plan)
            .map_err(|error| format!("could not preflight hard restart lifecycle: {error}"))?;
        let expected_level_update_flags = u8::from(
            !self
                .retail_objects
                .level_state_context()
                .ok_or_else(|| "hard restart has no level-state context".to_owned())?
                .first_spawn,
        );
        let mut camera_preview = self.retail_camera;
        let camera_step = camera_preview
            .level_update(
                &self.retail_zone_graph,
                snapshot.location.path,
                snapshot.location.progress.raw(),
                expected_level_update_flags,
            )
            .map_err(|error| format!("could not preflight restored camera location: {error}"))?;
        debug_assert_eq!(camera_step.after, snapshot.location);
        let restored_path = self
            .retail_zone_graph
            .path(snapshot.location.path)
            .ok_or_else(|| "restored camera path disappeared after validation".to_owned())?;
        let restored_point_count = NonZeroU16::new(
            u16::try_from(restored_path.points.len())
                .map_err(|_| "restored camera path has too many points".to_owned())?,
        )
        .ok_or_else(|| "restored camera path is empty".to_owned())?;

        // RESPAWN and TERM execute while native still exposes the old current
        // zone to texture replacement. Lifecycle close/open actions follow
        // those handlers and switch to destination protection at the same
        // source boundary where LevelUpdate assigns cur_zone.
        let mut pager_commit = self.retail_zone_pager.clone();
        let outcome = {
            let mut host = BrowserProgramHost::new(
                &self.level_assets.nsd,
                &self.level_assets.nsf,
                &self.level_assets.nsf_bytes,
                &mut self.retail_audio,
                &mut pager_commit,
                &mut self.card,
                &mut self.storage,
            );
            self.retail_objects
                .restart_saved_level_from_effect(&mut host, captured_saved_level)
        }
        .map_err(|error| format!("object hard restart failed: {error:?}"))?;

        let report = match outcome {
            RetailRestartOutcome::Restarted(report) => report,
            RetailRestartOutcome::DifferentLevel { .. } => {
                self.next_lid = -2;
                return Ok(());
            }
        };

        let mut destination_selected = false;
        let mut unavailable_opens = 0_usize;
        for action in plan.actions().iter().copied() {
            if !matches!(
                action,
                ZoneTransitionAction::CloseEntry(_)
                    | ZoneTransitionAction::ClosePage(_)
                    | ZoneTransitionAction::OpenEntry(_)
                    | ZoneTransitionAction::OpenPage(_)
            ) {
                continue;
            }
            let is_open = matches!(
                action,
                ZoneTransitionAction::OpenEntry(_) | ZoneTransitionAction::OpenPage(_)
            );
            if is_open && !destination_selected {
                pager_commit
                    .set_current_texture_load_eids(restored_load_list.entries().iter().copied());
                destination_selected = true;
            }
            match apply_retail_zone_paging_action(&mut pager_commit, action)? {
                RetailZonePagingOutcome::Open(outcome) => {
                    let published = if outcome.resolved {
                        self.retail_objects
                            .apply_platform_paging_open(outcome.page, outcome.invalidated)
                    } else {
                        self.retail_objects
                            .apply_platform_paging_queued_open(outcome.page)
                    };
                    published.map_err(|error| {
                        format!("could not publish hard-restart page resolution: {error:?}")
                    })?;
                }
                RetailZonePagingOutcome::Close(outcome) => self
                    .retail_objects
                    .apply_platform_paging_close(
                        outcome.page,
                        outcome.decremented,
                        outcome.unresolved,
                    )
                    .map_err(|error| {
                        format!("could not publish hard-restart page close: {error:?}")
                    })?,
                RetailZonePagingOutcome::OpenUnavailable => {
                    unavailable_opens = unavailable_opens.saturating_add(1);
                }
                RetailZonePagingOutcome::None => {}
            }
        }

        for (_, zone_report) in &report.zone_reports {
            for cleanup in &zone_report.cleanup_actions {
                let RuntimeCleanupAction::FreeObjectAudio(object) = *cleanup;
                self.retail_audio.free_owner(object.vm());
            }
        }
        let termination_failures = report
            .zone_reports
            .iter()
            .map(|(_, report)| report.event_failures.len())
            .sum::<usize>();
        let respawn_failures = report.respawn_event_failures.len();
        if report.level_update_flags != expected_level_update_flags {
            return Err(format!(
                "hard restart LevelUpdate flags changed after preflight: expected {expected_level_update_flags}, got {}",
                report.level_update_flags
            ));
        }
        let restored_vram_fill_color =
            retail_zone_transition_color(&self.retail_zones, report.snapshot.location.path.zone)?;
        self.retail_zone_pager = pager_commit;
        self.retail_zone_lifecycle = lifecycle_preview;
        self.retail_camera = camera_preview;
        self.retail_camera_pose_override = None;
        self.retail_objects
            .set_next_vram_fill_color(restored_vram_fill_color);
        self.refresh_retail_level_state_context(report.snapshot.location)?;
        self.retail_frame = RetailFrameState::ready(
            restored_point_count,
            report.snapshot.location.progress.raw(),
        );
        self.retail_tick_state = RetailTickState::NeedsSpawn;
        self.apply_prepared_retail_music(
            restored_music,
            false,
            report.snapshot.location.path.zone,
            dom,
        )?;
        dom.log(
            &format!(
                "Hard restart restored {}:{} progress {:#x}; {} objects terminated, {respawn_failures} RESPAWN and {termination_failures} TERM handler failures, {unavailable_opens} ignored native load-list opens.",
                report.snapshot.location.path.zone,
                report.snapshot.location.path.index,
                report.snapshot.location.progress.raw(),
                report
                    .zone_reports
                    .iter()
                    .map(|(_, report)| report.terminated.len())
                    .sum::<usize>(),
            ),
            respawn_failures != 0 || termination_failures != 0,
        );
        self.sync_completed_card_load(dom);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn update_retail_scene(
        &mut self,
        location: RetailCameraLocation,
        background_zone: Eid,
        draw_count: u32,
        frame_stamp: u32,
        advance_world_ripple: bool,
        world_display_mask: u32,
        vram_fill_color: [u8; 3],
        map_path_animation: Option<RetailMapPathAnimation>,
        camera_pose_override: Option<RetailCameraPose>,
        texture_frame_snapshot: TextureFrameSnapshot,
        world_shader_snapshot: crust_sim::retail_runtime::RetailWorldShaderSnapshot,
    ) -> Result<(), JsValue> {
        let path_progress = location.progress.raw();
        let render_title_objects = !self
            .title_screen_profile()
            .is_some_and(|profile| profile.screen_type == RetailTitleScreenType::ImageOnly);
        let objects = if render_title_objects {
            require_render_object_snapshot(self.retail_objects.render_objects())
                .map_err(|error| JsValue::from_str(&error))?
        } else {
            Vec::new()
        };
        let main_object = self
            .retail_objects
            .arena()
            .main_object()
            .and_then(|arena| self.retail_objects.object_for_arena(arena));
        let field_of_view = self
            .effective_retail_field_of_view()
            .map_err(|error| JsValue::from_str(&error))?;
        let authored_projection = retail_screen_projection(field_of_view).ok_or_else(|| {
            JsValue::from_str(&format!(
                "retail field of view {field_of_view} has no projection constant"
            ))
        })?;
        let presentation = RetailScenePresentation {
            projection_distance: Some(
                self.display_settings
                    .projection_distance(authored_projection),
            ),
            extended_world: self.display_settings.extended_world,
        };
        let background_fill = retail_background_fill_for_zone(
            &self.retail_zones,
            background_zone,
            world_display_mask,
            vram_fill_color,
        )
        .map_err(|error| JsValue::from_str(&error))?;
        let scene = self
            .retail_scene_builder
            .build_at_progress_with_runtime_snapshots_and_presentation(
                &self.level_assets.nsd,
                &self.level_assets.nsf,
                &self.level_assets.nsf_bytes,
                RetailSceneProgressLocation {
                    zone: location.path.zone,
                    path_index: location.path.index,
                    path_progress,
                    frame_stamp,
                    draw_count,
                },
                advance_world_ripple,
                &objects,
                main_object,
                world_display_mask,
                field_of_view,
                map_path_animation,
                camera_pose_override.map(Into::into),
                texture_frame_snapshot,
                world_shader_snapshot,
                &mut self.retail_world_shader_render_state,
                presentation,
            )
            .map_err(|error| {
                JsValue::from_str(&format!(
                    "retail scene update at progress {path_progress:#x}: {error}"
                ))
            })?;
        if !self
            .retail_pbak
            .as_ref()
            .is_some_and(RetailPbakPlayback::is_armed)
        {
            self.stage.update_retail_scene(scene, background_fill)?;
        }
        Ok(())
    }

    fn update_retail_camera(
        &mut self,
        snapshot: PlatformPadSnapshot,
        dom: &Dom,
    ) -> Result<RetailCameraStep, String> {
        let shared_game_state = self
            .retail_objects
            .global_word(GAME_STATE_GLOBAL)
            .map_err(|error| format!("retail camera game-state synchronization failed: {error:?}"))?
            .cast_signed();
        self.retail_camera.synchronize_game_state(shared_game_state);
        let location = self.retail_camera.location();
        let display_mask = self.effective_retail_display_mask();
        let spin_death = display_mask & 0x1_0000 != 0;
        let step = if self.retail_objects.arena().main_object().is_none() {
            // Native CamUpdate returns before either ordinary or death-camera
            // work until Crash has spawned.
            Ok(self.retail_camera.stationary_step())
        } else if spin_death {
            #[cfg(feature = "browser-test-harness")]
            {
                self.browser_test_cumulative_metrics.death_camera_frames = self
                    .browser_test_cumulative_metrics
                    .death_camera_frames
                    .saturating_add(1);
            }
            let inputs = {
                let mut host = NsfProgramHost::new(
                    &self.level_assets.nsd,
                    &self.level_assets.nsf,
                    &self.level_assets.nsf_bytes,
                );
                self.retail_objects
                    .resolve_spin_death_camera_inputs(&mut host)
                    .map_err(|error| {
                        format!("retail spin-death focus resolution failed: {error:?}")
                    })?
            };
            let mut pose = match self.retail_camera_pose_override {
                Some(pose) => pose,
                None => self
                    .retail_camera
                    .pose(&self.retail_zone_graph)
                    .map_err(|error| error.to_string())?,
            };
            self.retail_death_camera.i_death_cam = inputs.count;
            self.retail_death_camera
                .step(
                    &mut pose,
                    RetailDeathCameraInput {
                        transformed_focus: inputs.focus,
                        flip_speed: inputs.flip_speed,
                        zoom_speed: inputs.zoom_speed,
                        spin_accel: inputs.spin_accel,
                        ticks_per_frame: inputs.ticks_per_frame,
                    },
                )
                .map_err(|error| format!("retail spin-death camera failed: {error:?}"))?;
            self.retail_objects
                .set_spin_death_camera_count(self.retail_death_camera.i_death_cam)
                .map_err(|error| format!("retail spin-death count writeback failed: {error:?}"))?;
            self.retail_camera_pose_override = Some(pose);
            Ok(self.retail_camera.stationary_step())
        } else if display_mask & RETAIL_CAMERA_UPDATE == 0 {
            // Disabling bit two leaves the native camera transform untouched.
            Ok(self.retail_camera.stationary_step())
        } else if let Some(profile) = self.title_screen_profile() {
            if profile.updates_camera() {
                // Modes seven/eight consume title-owned globals that ordinary
                // CamUpdate deliberately cannot invent. Supplying them here
                // is the authored island-map path from menu selection to the
                // first gameplay `next_lid` request.
                let island_cam_state = self
                    .retail_objects
                    .global_word(ISLAND_CAMERA_STATE_GLOBAL)
                    .map_err(|error| {
                        format!("retail island-camera state is unavailable: {error:?}")
                    })?
                    .cast_signed();
                let island_cam_rot_x = self
                    .retail_objects
                    .global_word(ISLAND_CAMERA_ROTATION_GLOBAL)
                    .map_err(|error| {
                        format!("retail island-camera rotation is unavailable: {error:?}")
                    })?
                    .cast_signed();
                self.retail_camera.update_with_island(
                    &self.retail_zone_graph,
                    RetailCameraInput {
                        tapped: snapshot.tapped,
                    },
                    Some(RetailIslandCameraInput {
                        island_cam_state,
                        island_cam_rot_x,
                    }),
                )
            } else {
                Ok(self.retail_camera.stationary_step())
            }
        } else {
            let mode = self
                .retail_zone_graph
                .path(location.path)
                .ok_or_else(|| {
                    format!(
                        "camera graph has no active path {}:{}",
                        location.path.zone, location.path.index
                    )
                })?
                .camera_mode;
            if matches!(mode, 5 | 6)
                && let Some(input) = self.retail_follow_input(snapshot)?
            {
                self.retail_camera
                    .update_follow(&self.retail_zone_graph, input)
            } else {
                self.retail_camera.update(
                    &self.retail_zone_graph,
                    RetailCameraInput {
                        tapped: snapshot.tapped,
                    },
                )
            }
        }
        .map_err(|error| error.to_string())?;
        let island_state_writeback = retail_island_state_writeback(step.outcome);
        if let Some((RetailIslandWritebackPhase::BeforeLevelUpdate, state_after)) =
            island_state_writeback
        {
            // Native mode seven publishes the orbit state before its optional
            // LevelUpdate, so departing-zone TERM handlers observe the new
            // value just as they do in CamUpdate.
            self.write_retail_island_camera_state(state_after)?;
        }
        if !spin_death && step.before != step.after {
            // Every modeled path movement corresponds to the native
            // LevelUpdate/CamFollow write that replaces a prior death pose.
            self.retail_camera_pose_override = None;
        }
        self.apply_retail_camera_effects(&step, dom)?;
        if let Some((RetailIslandWritebackPhase::AfterLevelUpdate, state_after)) =
            island_state_writeback
        {
            // Native mode eight performs LevelUpdate first. That call may run
            // TERM handlers synchronously; only after it returns does CamUpdate
            // publish state one when the destination leaves mode eight.
            self.write_retail_island_camera_state(state_after)?;
        }
        let rotation_xz = self
            .retail_camera
            .rotation_xz(&self.retail_zone_graph)
            .map_err(|error| error.to_string())?;
        let pose = match self.retail_camera_pose_override {
            Some(pose) => pose,
            None => self
                .retail_camera
                .pose(&self.retail_zone_graph)
                .map_err(|error| error.to_string())?,
        };
        let field_of_view = self.effective_retail_field_of_view()?;
        let screen_projection = retail_screen_projection(field_of_view).ok_or_else(|| {
            format!("retail field of view {field_of_view} has no projection constant")
        })?;
        let live_game_state = self
            .retail_objects
            .global_word(GAME_STATE_GLOBAL)
            .map_err(|error| format!("retail post-camera game state is unavailable: {error:?}"))?
            .cast_signed();
        self.retail_camera.synchronize_game_state(live_game_state);
        self.retail_objects
            .latch_frame_context(live_game_state, rotation_xz);
        self.retail_objects.set_transform_vectors_camera(
            RetailTransformVectorsCamera::from_retail_pose(
                pose.translation,
                pose.rotation_yxz,
                screen_projection,
            ),
        );
        Ok(step)
    }

    fn write_retail_island_camera_state(&mut self, state: i32) -> Result<(), String> {
        self.retail_objects
            .set_global_word(ISLAND_CAMERA_STATE_GLOBAL, state.cast_unsigned())
            .map_err(|error| format!("retail island-camera state writeback failed: {error:?}"))
    }

    fn apply_retail_camera_effects(
        &mut self,
        step: &RetailCameraStep,
        dom: &Dom,
    ) -> Result<(), String> {
        for effect in &step.effects {
            match *effect {
                RetailCameraEffect::GameStateWrite { value } => self
                    .retail_objects
                    .set_global_word(GAME_STATE_GLOBAL, value.cast_unsigned())
                    .map_err(|error| format!("retail camera game-state write failed: {error:?}"))?,
                RetailCameraEffect::LevelUpdate {
                    before,
                    after,
                    flags,
                } => {
                    if before.path.zone != after.path.zone {
                        self.apply_retail_zone_transition(after.path.zone, flags, dom)?;
                    }
                    self.refresh_retail_level_state_context(after)?;
                }
                RetailCameraEffect::SaveStateHandshake { location } => {
                    self.retail_metrics.camera_save_handshakes =
                        self.retail_metrics.camera_save_handshakes.saturating_add(1);
                    self.refresh_retail_level_state_context(location)?;
                    let main = self
                        .retail_objects
                        .arena()
                        .main_object()
                        .and_then(|arena| self.retail_objects.object_for_arena(arena))
                        .ok_or_else(|| {
                            "camera save-state handshake has no live main object".to_owned()
                        })?;
                    match self
                        .retail_objects
                        .save_level_state(main, true)
                        .map_err(|error| format!("camera save-state capture failed: {error:?}"))?
                    {
                        RetailSaveStateOutcome::Saved(_) => {
                            if self.retail_metrics.camera_save_handshakes == 1 {
                                dom.log(
                                    &format!(
                                        "Persisted the retail in-level snapshot at {}:{} progress {:#x}.",
                                        location.path.zone,
                                        location.path.index,
                                        location.progress.raw(),
                                    ),
                                    false,
                                );
                            }
                        }
                        RetailSaveStateOutcome::RestrictedByZone => {}
                    }
                }
            }
        }
        Ok(())
    }

    fn refresh_retail_level_state_context(
        &mut self,
        location: RetailCameraLocation,
    ) -> Result<(), String> {
        let existing = self
            .retail_objects
            .level_state_context()
            .cloned()
            .ok_or_else(|| {
                "retail camera update has no authoritative level-state context".to_owned()
            })?;
        let context = build_retail_level_state_context(
            &self.retail_zone_graph,
            location,
            &self.retail_zone_lifecycle,
            existing.box_count,
            existing.checkpoint_id,
            existing.checkpoint_translation,
            existing.first_spawn,
        )?;
        self.retail_objects.set_level_state_context(context);
        Ok(())
    }

    fn apply_retail_zone_transition(
        &mut self,
        next_zone: Eid,
        flags: u8,
        dom: &Dom,
    ) -> Result<(), String> {
        let activation_marker = (self.retail_zone_lifecycle.current_zone().is_none()
            && self.level_assets.level != FormatLevelId::TITLE)
            || flags & 2 != 0;
        let plan = self
            .retail_zone_lifecycle
            .plan_transition_with_marker(next_zone, activation_marker)
            .map_err(|error| format!("could not plan retail zone transition: {error}"))?;
        if plan.is_noop() {
            return Ok(());
        }
        let next_vram_fill_color = retail_zone_transition_color(&self.retail_zones, next_zone)?;
        let prepared_music = self.prepare_retail_music(&self.level_assets, next_zone, false)?;

        // Validate every fallible page/entry operation before the first TERM
        // event can irreversibly mutate the live object forest.
        let mut pager_validation = self.retail_zone_pager.clone();
        let destination_load_list = self
            .retail_zone_lifecycle
            .zone(next_zone)
            .ok_or_else(|| "destination texture zone is absent from the lifecycle".to_owned())?
            .load_list();
        pager_validation
            .set_current_texture_load_eids(destination_load_list.entries().iter().copied());
        for action in plan.actions().iter().copied() {
            apply_retail_zone_paging_action(&mut pager_validation, action)?;
        }
        let mut lifecycle_preview = self.retail_zone_lifecycle.clone();
        lifecycle_preview
            .commit_transition(&plan)
            .map_err(|error| format!("could not preflight retail zone transition: {error}"))?;

        // Replay the checked plan against a separate commit candidate. TERM
        // handlers share this pager while native still exposes the old
        // current zone; the first lifecycle paging action then installs the
        // destination protection set before any destination open can allocate
        // a texture slot.
        let mut pager_commit = self.retail_zone_pager.clone();

        let previous_zone = plan.previous_zone();
        let mut terminated = 0_usize;
        let mut cleanup_actions = 0_usize;
        let mut destination_selected = false;
        let mut unavailable_opens = 0_usize;
        let mut event_failures = Vec::new();
        for action in plan.actions().iter().copied() {
            match action {
                ZoneTransitionAction::TerminateZoneObjects(zone) => {
                    let report = {
                        let mut host = BrowserProgramHost::new(
                            &self.level_assets.nsd,
                            &self.level_assets.nsf,
                            &self.level_assets.nsf_bytes,
                            &mut self.retail_audio,
                            &mut pager_commit,
                            &mut self.card,
                            &mut self.storage,
                        );
                        self.retail_objects.terminate_zone_objects(
                            zone,
                            ZoneTerminationMode::Departure { target: next_zone },
                            &mut host,
                        )
                    }
                    .map_err(|error| format!("retail zone {zone} termination failed: {error:?}"))?;
                    terminated = terminated.saturating_add(report.terminated.len());
                    cleanup_actions = cleanup_actions.saturating_add(report.cleanup_actions.len());
                    for cleanup in &report.cleanup_actions {
                        let RuntimeCleanupAction::FreeObjectAudio(object) = *cleanup;
                        self.retail_audio.free_owner(object.vm());
                    }
                    event_failures.extend(
                        report
                            .event_failures
                            .into_iter()
                            .map(|failure| format!("{:?}: {:?}", failure.object, failure.error)),
                    );
                }
                ZoneTransitionAction::SetDisplayFlags { before, after, .. } => {
                    if before & ZONE_OBJECTS_ACTIVE == 0 && after & ZONE_OBJECTS_ACTIVE != 0 {
                        self.retail_objects.reset_retail_box_spawn_state();
                    }
                }
                ZoneTransitionAction::CloseEntry(_)
                | ZoneTransitionAction::ClosePage(_)
                | ZoneTransitionAction::OpenEntry(_)
                | ZoneTransitionAction::OpenPage(_) => {
                    if !destination_selected {
                        pager_commit.set_current_texture_load_eids(
                            destination_load_list.entries().iter().copied(),
                        );
                        destination_selected = true;
                    }
                    match apply_retail_zone_paging_action(&mut pager_commit, action)? {
                        RetailZonePagingOutcome::Open(outcome) => {
                            let published = if outcome.resolved {
                                self.retail_objects
                                    .apply_platform_paging_open(outcome.page, outcome.invalidated)
                            } else {
                                self.retail_objects
                                    .apply_platform_paging_queued_open(outcome.page)
                            };
                            published.map_err(|error| {
                                format!(
                                    "could not publish zone-transition page resolution: {error:?}"
                                )
                            })?;
                        }
                        RetailZonePagingOutcome::Close(outcome) => self
                            .retail_objects
                            .apply_platform_paging_close(
                                outcome.page,
                                outcome.decremented,
                                outcome.unresolved,
                            )
                            .map_err(|error| {
                                format!("could not publish zone-transition page close: {error:?}")
                            })?,
                        RetailZonePagingOutcome::OpenUnavailable => {
                            unavailable_opens = unavailable_opens.saturating_add(1);
                        }
                        RetailZonePagingOutcome::None => {}
                    }
                }
            }
        }
        // Publish the candidate only after the final TERM delivery and checked
        // lifecycle action succeeds. Until here, the live pager is untouched.
        self.retail_zone_pager = pager_commit;
        self.retail_zone_lifecycle = lifecycle_preview;
        self.retail_objects
            .set_next_vram_fill_color(next_vram_fill_color);
        self.apply_prepared_retail_music(prepared_music, false, next_zone, dom)?;

        self.retail_metrics.zone_transitions =
            self.retail_metrics.zone_transitions.saturating_add(1);
        self.retail_metrics.zone_terminated_objects = self
            .retail_metrics
            .zone_terminated_objects
            .saturating_add(terminated as u64);
        self.retail_metrics.zone_event_failures = self
            .retail_metrics
            .zone_event_failures
            .saturating_add(event_failures.len() as u64);
        dom.log(
            &format!(
                "Retail LevelUpdate moved {:?} -> {next_zone}; terminated {terminated} objects, applied {cleanup_actions} audio-owner cleanups, ignored {unavailable_opens} native load-list opens and activated {} next-frame spawn zones.",
                previous_zone,
                plan.next_frame_spawn_scan().len(),
            ),
            !event_failures.is_empty(),
        );
        if let Some(first) = event_failures.first() {
            self.retail_runtime_warning = Some(format!(
                "{} terminate handler(s) reached checked failures; first: {first}",
                event_failures.len()
            ));
        }
        self.sync_completed_card_load(dom);
        Ok(())
    }

    fn retail_follow_input(
        &self,
        snapshot: PlatformPadSnapshot,
    ) -> Result<Option<RetailCameraFollowInput>, String> {
        let Some(arena_handle) = self.retail_objects.arena().main_object() else {
            return Ok(None);
        };
        let object_handle = self
            .retail_objects
            .object_for_arena(arena_handle)
            .ok_or_else(|| "retail main object has no paired VM handle".to_owned())?;
        let machine = self.retail_objects.machine();
        let player = machine
            .object(object_handle.vm())
            .map_err(|error| format!("retail main object is unavailable: {error:?}"))?;
        let register = |index| {
            player
                .register(index)
                .map_err(|error| format!("retail main object register {index}: {error:?}"))
        };
        let level_id = i32::try_from(self.level_assets.level.get())
            .map_err(|_| "mounted level identifier does not fit the camera input".to_owned())?;
        Ok(Some(RetailCameraFollowInput {
            player_translation: Vec3 {
                x: register(process_register::TRANSLATION_X)?.cast_signed(),
                y: register(process_register::TRANSLATION_Y)?.cast_signed(),
                z: register(process_register::TRANSLATION_Z)?.cast_signed(),
            },
            player_cam_zoom: register(process_register::CAMERA_ZOOM)?.cast_signed(),
            held_buttons: snapshot.held,
            level_id,
            // CamUpdate precedes GoolUpdate, so it observes the stamp installed
            // by the previous retail object frame.
            frames_elapsed: machine.frames_elapsed(),
            gem_stamp: self
                .retail_objects
                .gem_stamp()
                .map_err(|error| format!("retail gem stamp is unavailable: {error:?}"))?,
        }))
    }

    fn paused(&self) -> bool {
        if is_retail_runtime_state(self.flow.state()) {
            self.retail_runtime_error.is_some() || self.retail_objects.retail_pause_state().paused()
        } else {
            false
        }
    }

    fn retail_runtime_message<'a>(&'a self, normal: &'static str) -> &'a str {
        self.retail_runtime_error
            .as_deref()
            .or(self.retail_runtime_warning.as_deref())
            .unwrap_or(normal)
    }

    fn sync_title_card(&mut self, dom: &Dom) -> Result<(), JsValue> {
        if !matches!(self.flow.state(), FlowState::Title)
            || self.level_assets.level.get() != u32::from(LevelId::TITLE.raw())
        {
            return Ok(());
        }
        let screen = self
            .retail_objects
            .retail_title_presentation()
            .map_err(|error| {
                JsValue::from_str(&format!(
                    "could not read authoritative retail title screen: {error:?}"
                ))
            })?
            .map(|title| title.screen)
            .ok_or_else(|| JsValue::from_str("retail title runtime is not configured"))?;
        let state = screen as u8;
        if self.last_title_state == Some(state) {
            return Ok(());
        }
        self.apply_retail_title_level_update(screen, dom)?;
        self.last_title_state = Some(state);
        if !title_state_uses_image(screen) {
            return Ok(());
        }
        let card = decode_title_card(
            &self.level_assets.nsd,
            &self.level_assets.nsf,
            &self.level_assets.nsf_bytes,
            state,
        )
        .map_err(|error| JsValue::from_str(&format!("retail title state {state}: {error}")))?;
        self.stage.install_title_image(&card.image)?;
        dom.log(
            &format!(
                "Composed retail title state {state} from {}x{} MDAT tiles.",
                card.width_tiles, card.height_tiles
            ),
            false,
        );
        Ok(())
    }

    fn apply_protected_title_reset(&mut self, dom: &Dom) -> Result<(), JsValue> {
        let current = self.save_data();
        let resume_payload = self.resume.before_title_reset(current);
        if self.level_access.persists()
            && let Some(payload) = resume_payload
            && let Some(storage) = &self.storage
            && let Err(error) = storage.persist_resume(payload)
        {
            // The native browser hook still restores the protected in-memory
            // payload when its eager persistence write fails.
            dom.log(
                &format!(
                    "Could not flush browser resume before the title reset: {}",
                    js_message(&error)
                ),
                true,
            );
        }
        self.retail_objects.reset_level_globals().map_err(|error| {
            JsValue::from_str(&format!("retail main-menu global reset failed: {error:?}"))
        })?;
        let protected = self.resume.after_title_reset().ok_or_else(|| {
            JsValue::from_str("retail main-menu reset lost its protected resume payload")
        })?;
        self.retail_objects
            .restore_resume_after_title_reset(protected)
            .map_err(|error| {
                JsValue::from_str(&format!(
                    "retail main-menu resume restoration failed: {error:?}"
                ))
            })?;
        apply_save(&mut self.flow, protected);
        apply_level_access(self.level_access, &mut self.retail_objects, &mut self.flow).map_err(
            |error| {
                JsValue::from_str(&format!(
                    "could not restore temporary all-level access after title reset: {error:?}"
                ))
            },
        )?;
        self.last_authoritative_save =
            authoritative_save_or_last(self.retail_objects.card_save_data(), protected);
        self.retail_audio
            .set_sfx_volume(self.flow.options.sfx_volume);
        if let Some(audio) = &mut self.audio {
            audio.set_output_options(output_options(self.flow.options));
        }
        dom.log(
            "Applied the native main-menu reset with browser-resume protection.",
            false,
        );
        Ok(())
    }

    fn apply_retail_title_level_update(
        &mut self,
        screen: TitleScreen,
        dom: &Dom,
    ) -> Result<(), JsValue> {
        let profile = retail_title_screen_profile(screen, self.flow.progress.current_map_level);
        self.retail_objects
            .set_global_word(NEXT_DISPLAY_GLOBAL, profile.display_mask())
            .map_err(|error| {
                JsValue::from_str(&format!("could not publish title display mask: {error:?}"))
            })?;
        let zone_name = profile.zone_name;
        let zone = Eid::from_name(zone_name).map_err(|error| {
            JsValue::from_str(&format!("retail title zone {zone_name}: {error}"))
        })?;
        let path = RetailPathId { zone, index: 0 };
        if self.retail_zone_graph.path(path).is_none() {
            return Err(JsValue::from_str(&format!(
                "retail title state {} references absent path {zone}:0",
                screen as u8
            )));
        }
        let previous_title_mdat = self
            .last_title_state
            .filter(|state| title_state_number_uses_image(*state))
            .and_then(|state| title_mdat_eid(state).ok());

        // TitleLoadScreen kills every old title/MDAT object before its
        // explicit flag-two LevelUpdate, without zone immunity gates.
        self.retail_objects
            .reset_retail_pause_for_screen_load()
            .map_err(|error| {
                JsValue::from_str(&format!(
                    "could not reset retail pause state for the title screen: {error:?}"
                ))
            })?;
        let report = {
            let mut host = if let Some(mdat) = previous_title_mdat {
                BrowserProgramHost::for_title_mdat(
                    &self.level_assets.nsd,
                    &self.level_assets.nsf,
                    &self.level_assets.nsf_bytes,
                    &mut self.retail_audio,
                    &mut self.retail_zone_pager,
                    &mut self.card,
                    &mut self.storage,
                    mdat,
                )
            } else {
                BrowserProgramHost::new(
                    &self.level_assets.nsd,
                    &self.level_assets.nsf,
                    &self.level_assets.nsf_bytes,
                    &mut self.retail_audio,
                    &mut self.retail_zone_pager,
                    &mut self.card,
                    &mut self.storage,
                )
            };
            self.retail_objects.terminate_all_objects(&mut host)
        }
        .map_err(|error| {
            JsValue::from_str(&format!("retail title object teardown failed: {error:?}"))
        })?;
        for cleanup in &report.cleanup_actions {
            let RuntimeCleanupAction::FreeObjectAudio(object) = *cleanup;
            self.retail_audio.free_owner(object.vm());
        }
        if screen == TitleScreen::MainMenu {
            self.apply_protected_title_reset(dom)?;
        }
        let step = self
            .retail_camera
            .level_update(&self.retail_zone_graph, path, 0, 2)
            .map_err(|error| {
                JsValue::from_str(&format!("retail title LevelUpdate failed: {error}"))
            })?;
        self.apply_retail_camera_effects(&step, dom)
            .map_err(|error| JsValue::from_str(&error))?;
        if profile.uses_image() {
            self.spawn_retail_title_mdat(screen as u8, dom)?;
        }
        self.retail_tick_state = RetailTickState::NeedsSpawn;
        dom.log(
            &format!(
                "Title state {} applied a type-{:?} flag-two LevelUpdate to {zone}:0 after terminating {} objects.",
                screen as u8,
                profile.screen_type,
                report.terminated.len(),
            ),
            !report.event_failures.is_empty(),
        );
        self.sync_completed_card_load(dom);
        Ok(())
    }

    fn spawn_retail_title_mdat(&mut self, state: u8, dom: &Dom) -> Result<(), JsValue> {
        let mdat = load_title_mdat(
            &self.level_assets.nsd,
            &self.level_assets.nsf,
            &self.level_assets.nsf_bytes,
            state,
        )
        .map_err(|error| JsValue::from_str(&format!("retail title MDAT state {state}: {error}")))?;
        let total_entities = mdat.entities.len();
        let eligible_entities = mdat
            .entities
            .into_iter()
            .filter(|entity| {
                title_mdat_entity_is_unlocked(entity, self.flow.progress.levels_unlocked)
            })
            .collect::<Vec<_>>();
        // Native GoolObjectSpawn reads the entity from the type-17 MDAT, then
        // rewrites `zone` to `cur_zone` before assigning obj_zone and reading
        // the ZDAT colors. The live level-state context is also the zone used
        // by misc 12/7, so sharing it keeps title objects in that TERM domain.
        let current_zone = self
            .retail_objects
            .level_state_context()
            .map(|context| context.location.path.zone)
            .ok_or_else(|| {
                JsValue::from_str(&format!(
                    "retail title MDAT state {state} has no current zone context"
                ))
            })?;
        let binding = retail_title_mdat_binding(mdat.eid, current_zone);
        let attempts = {
            let neighbors = [NeighborZone {
                eid: binding.object_zone,
                display_flags: RETAIL_ZONE_OBJECTS_ACTIVE,
                entities: eligible_entities.as_slice(),
            }];
            let mut host = BrowserProgramHost::for_title_mdat(
                &self.level_assets.nsd,
                &self.level_assets.nsf,
                &self.level_assets.nsf_bytes,
                &mut self.retail_audio,
                &mut self.retail_zone_pager,
                &mut self.card,
                &mut self.storage,
                binding.source,
            );
            self.retail_objects
                .spawn_current_zone_neighbors(&neighbors, &mut host)
        };
        self.drain_retail_reclaim_diagnostics(dom);

        let counts = browser_spawn_counts(&attempts);
        self.retail_metrics.record_spawns(counts);
        let unexpected = first_unexpected_browser_spawn(&attempts);
        if let Some(error) = unexpected {
            let message = format!(
                "Title MDAT state {state} reached {} unexpected spawn failure(s); first error: {error:?}",
                counts.failed,
            );
            dom.log(&message, true);
            self.retail_runtime_warning = Some(message);
        }
        dom.log(
            &format!(
                "Title MDAT state {state} filtered {total_entities} descriptors to {} unlocked entries and immediately bound {}/{} group-3 objects to current zone {current_zone} ({} already active, {} authored sentinel rejection(s)).",
                eligible_entities.len(),
                counts.successful,
                counts.attempts,
                counts.already_active,
                counts.authored_rejections,
            ),
            unexpected.is_some(),
        );
        self.sync_completed_card_load(dom);
        Ok(())
    }

    fn handle_events(&mut self, dom: &Dom) {
        for event in self.flow.take_events() {
            dom.log(&format!("flow: {event:?}"), false);
            let RetailFlowEvent::Booted(level) = event;
            self.queue_asset_level(level);
        }
    }

    fn render_ui(&self, dom: &Dom) -> Result<(), JsValue> {
        let simulation_state = if self.asset_load_error.is_some() {
            "BLOCKED"
        } else if self.asset_transition_level().is_some() {
            "LOADING"
        } else if self.retail_runtime_error.is_some() {
            "RUNTIME ERROR"
        } else if self.retail_runtime_warning.is_some() {
            "RUNTIME WARN"
        } else if self.paused() {
            "PAUSED"
        } else {
            "RUNNING"
        };
        dom.sim_state.set_text_content(Some(simulation_state));
        dom.current_level
            .set_text_content(Some(&format!("0x{:02X}", current_level(&self.flow).raw())));
        dom.audio_state.set_text_content(Some(if self.muted {
            "MUTED"
        } else if self.audio.is_some() {
            "SYNTH ACTIVE"
        } else {
            "UNAVAILABLE"
        }));
        dom.card_state
            .set_text_content(Some(&format!("{} / 15", self.card.part_count())));
        dom.pause
            .set_attribute("aria-pressed", if self.paused() { "true" } else { "false" })?;

        if let Some(message) = &self.asset_load_error {
            dom.set_overlay(
                true,
                "LOCAL STREAM TRANSITION BLOCKED",
                "Destination data unavailable",
                message,
            );
            dom.set_menu(&[])?;
            return Ok(());
        }
        if let Some(level) = self.asset_transition_level() {
            dom.set_overlay(
                true,
                &format!("MOUNTING LID 0x{:02X}", level.get()),
                "Reading local NSD/NSF pair",
                "No game data is uploaded",
            );
            dom.set_menu(&[])?;
            return Ok(());
        }

        // Once the authored GOOL scene is presenting, keep the diagnostic DOM
        // chrome off the 4:3 output. The state and warning indicators remain in
        // the monitor panel outside the game canvas.
        if self.authored_scene_runtime_active() {
            dom.set_overlay(false, "", "", "");
            dom.set_menu(&[])?;
            return Ok(());
        }

        match self.flow.state() {
            FlowState::Boot => {
                dom.set_overlay(true, "RUST / WASM", "Booting", "Validating streams");
            }
            FlowState::Title => {
                let (overline, title) = if self.retail_runtime_error.is_some() {
                    ("AUTHORED TITLE ERROR", "Retail title runtime stopped")
                } else {
                    ("AUTHORED TITLE LOADING", "Waiting for retail title objects")
                };
                dom.set_overlay(
                    true,
                    overline,
                    title,
                    self.retail_runtime_message(
                        "Mounted title data has not produced an authored scene yet",
                    ),
                );
                dom.set_menu(&[])?;
            }
            FlowState::Gameplay(level) => {
                dom.set_overlay(
                    true,
                    &format!("LID 0x{:02X} / GAMEPLAY", level.raw()),
                    level_name(*level),
                    self.retail_runtime_message(
                        "Keyboard, gamepad, and touch controls feed the retail GOOL pad state",
                    ),
                );
                dom.set_menu(&[])?;
            }
            FlowState::Bonus(level) => {
                dom.set_overlay(
                    true,
                    "BONUS PATH",
                    level_name(*level),
                    self.retail_runtime_message(
                        "Retail bonus GOOL is ticking · transition effects are pending",
                    ),
                );
                dom.set_menu(&[])?;
            }
            FlowState::Boss(level) => {
                dom.set_overlay(
                    true,
                    "BOSS PATH",
                    level_name(*level),
                    self.retail_runtime_message(
                        "Retail boss GOOL is ticking · transition effects are pending",
                    ),
                );
                dom.set_menu(&[])?;
            }
            FlowState::LevelComplete { missed_boxes, .. } => {
                dom.set_overlay(
                    true,
                    "LEVEL TRANSITION",
                    &format!("Level complete · missed boxes: {missed_boxes}"),
                    self.retail_runtime_message(
                        "Waiting for authored level-complete objects to present",
                    ),
                );
                dom.set_menu(&[])?;
            }
            FlowState::Intro => {
                dom.set_overlay(
                    true,
                    "ATTRACT SEQUENCE",
                    "Intro",
                    self.retail_runtime_message(
                        "Retail intro GOOL is ticking · face buttons feed the authored pad state",
                    ),
                );
                dom.set_menu(&[])?;
            }
            FlowState::Ending => {
                dom.set_overlay(
                    true,
                    "COMPLETION FLOW",
                    "Ending",
                    self.retail_runtime_message(
                        "Retail ending GOOL is ticking · presentation effects are pending",
                    ),
                );
                dom.set_menu(&[])?;
            }
        }
        Ok(())
    }

    fn request_pause(&mut self) {
        self.pending_buttons |= PAD_START;
    }

    fn prepare_retail_music(
        &self,
        pair: &ValidatedPair,
        zone: Eid,
        force_owner_change: bool,
    ) -> Result<PreparedRetailMusic, String> {
        let Some(audio) = &self.audio else {
            return Ok(PreparedRetailMusic::Unchanged);
        };
        let midi = zone_retail_music_eid(pair, zone)?;
        if !force_owner_change && audio.requested_retail_music_eid() == midi {
            return Ok(PreparedRetailMusic::Unchanged);
        }
        midi.map_or(Ok(PreparedRetailMusic::Silence), |eid| {
            decode_retail_music(pair, eid)
                .map(|music| PreparedRetailMusic::Music(eid, Box::new(music)))
        })
    }

    fn apply_prepared_retail_music(
        &mut self,
        music: PreparedRetailMusic,
        immediate: bool,
        zone: Eid,
        dom: &Dom,
    ) -> Result<(), String> {
        let Some(audio) = &mut self.audio else {
            return Ok(());
        };
        let (eid, change) = match music {
            PreparedRetailMusic::Unchanged => return Ok(()),
            PreparedRetailMusic::Silence if immediate => (None, audio.clear_retail_music()),
            PreparedRetailMusic::Silence => (
                None,
                audio
                    .request_retail_music(None)
                    .map_err(|error| error.to_string())?,
            ),
            PreparedRetailMusic::Music(eid, music) if immediate => (
                Some(eid),
                audio
                    .start_retail_music(eid, *music)
                    .map_err(|error| error.to_string())?,
            ),
            PreparedRetailMusic::Music(eid, music) => (
                Some(eid),
                audio
                    .request_retail_music(Some((eid, *music)))
                    .map_err(|error| error.to_string())?,
            ),
        };
        if change != RetailMusicChange::Unchanged {
            dom.log(
                &format!(
                    "Retail music for zone {zone} requested {} ({change:?}).",
                    eid.map_or_else(|| "silence".to_owned(), |eid| eid.to_string()),
                ),
                false,
            );
        }
        Ok(())
    }

    fn play_retail_thunder(&mut self, cue: RetailThunderCue) -> Result<(), String> {
        let mut host = BrowserProgramHost::new(
            &self.level_assets.nsd,
            &self.level_assets.nsf,
            &self.level_assets.nsf_bytes,
            &mut self.retail_audio,
            &mut self.retail_zone_pager,
            &mut self.card,
            &mut self.storage,
        );
        host.ensure_audio_sample(cue.adio)
            .map_err(|error| error.to_string())?;
        host.audio
            .create_unowned_delayed_voice(cue.adio, cue.amplitude, cue.pitch, cue.trigger)
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        if let Some(audio) = &mut self.audio {
            audio.set_muted(muted);
        }
    }

    fn resume_audio(&self) {
        if let Some(audio) = &self.audio {
            audio.resume();
        }
    }

    fn save_data(&self) -> SaveData {
        authoritative_save_or_last(
            self.retail_objects.card_save_data(),
            self.last_authoritative_save,
        )
    }

    fn flush(&mut self) {
        let save = self.save_data();
        let resume_payload = self.resume.flush(save);
        if self.level_access.persists()
            && let Some(payload) = resume_payload
            && let Some(storage) = &self.storage
        {
            let _ = storage.persist_resume(payload);
        }
        if let Some(storage) = &mut self.storage {
            let _ = storage.persist_card(&self.card, CardPersistIntent::Snapshot);
        }
    }
}

fn bind_events(app: &Rc<RefCell<App>>) -> Result<(), JsValue> {
    let dom = app.borrow().dom.clone();

    {
        let input = dom.game_files.clone();
        let app = Rc::clone(app);
        let callback = Closure::<dyn FnMut(Event)>::new(move |_| {
            if let Some(files) = input.files() {
                import_files(Rc::clone(&app), &files);
            }
        });
        dom.game_files
            .add_event_listener_with_callback("change", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }
    {
        let input = dom.game_folder.clone();
        let app = Rc::clone(app);
        let callback = Closure::<dyn FnMut(Event)>::new(move |_| {
            if let Some(files) = input.files() {
                import_files(Rc::clone(&app), &files);
            }
        });
        dom.game_folder
            .add_event_listener_with_callback("change", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }
    bind_click(&dom.choose_files, {
        let input = dom.game_files.clone();
        move || input.click()
    })?;
    bind_click(&dom.choose_folder, {
        let input = dom.game_folder.clone();
        move || input.click()
    })?;
    bind_click(&dom.dropzone, {
        let input = dom.game_files.clone();
        move || input.click()
    })?;

    {
        let callback =
            Closure::<dyn FnMut(DragEvent)>::new(move |event: DragEvent| event.prevent_default());
        dom.dropzone
            .add_event_listener_with_callback("dragover", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }
    {
        let app = Rc::clone(app);
        let callback = Closure::<dyn FnMut(DragEvent)>::new(move |event: DragEvent| {
            event.prevent_default();
            if let Some(files) = event.data_transfer().and_then(|transfer| transfer.files()) {
                import_files(Rc::clone(&app), &files);
            }
        });
        dom.dropzone
            .add_event_listener_with_callback("drop", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }

    {
        let app = Rc::clone(app);
        bind_click(&dom.launch, move || launch(Rc::clone(&app)))?;
    }
    {
        let app = Rc::clone(app);
        bind_click(&dom.clear, move || {
            let mut app = app.borrow_mut();
            if !app.locked && !app.busy {
                app.assets.clear();
                app.dom.log("Released selected local file handles.", false);
                let _ = app.refresh_assets();
            }
        })?;
    }
    {
        let app = Rc::clone(app);
        bind_click(&dom.pause, move || {
            if let Some(runtime) = &mut app.borrow_mut().runtime {
                runtime.request_pause();
            }
        })?;
    }
    {
        let app = Rc::clone(app);
        let mute = dom.mute.clone();
        bind_click(&dom.mute, move || {
            let mut app = app.borrow_mut();
            app.muted = !app.muted;
            let muted = app.muted;
            mute.set_text_content(Some(if muted { "◖ Unmute" } else { "◖ Mute" }));
            let _ = mute.set_attribute("aria-pressed", if muted { "true" } else { "false" });
            if let Some(runtime) = &mut app.runtime {
                runtime.set_muted(muted);
            }
        })?;
    }
    {
        let document = dom.document.clone();
        let fullscreen = dom.fullscreen.clone();
        let screen: Element = dom.screen.clone().into();
        bind_click(&dom.fullscreen, move || {
            if document.fullscreen_enabled() {
                let Ok(promise) = request_fullscreen_promise(&screen) else {
                    fullscreen.set_disabled(true);
                    let _ = fullscreen
                        .set_attribute("title", "Fullscreen was denied by this browser context");
                    return;
                };
                let fullscreen = fullscreen.clone();
                spawn_local(async move {
                    if JsFuture::from(promise).await.is_err() {
                        fullscreen.set_disabled(true);
                        let _ = fullscreen.set_attribute(
                            "title",
                            "Fullscreen was denied by this browser context",
                        );
                    }
                });
            }
        })?;
    }

    {
        let app = Rc::clone(app);
        let callback = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
            if let Some(bit) = keyboard_code(&event.code()) {
                event.prevent_default();
                let mut app = app.borrow_mut();
                let newly_pressed = app.active_keys.insert(event.code(), bit).is_none();
                app.rebuild_keyboard_bits();
                if let Some(runtime) = &mut app.runtime {
                    // Preserve a complete key press that begins and ends
                    // between two 30 Hz simulation samples. Held input still
                    // comes from `keyboard_bits`; this one-frame latch only
                    // guarantees the authored tapped edge is observable.
                    if newly_pressed && !event.repeat() {
                        runtime.pending_buttons |= bit;
                    }
                    runtime.resume_audio();
                }
            }
        });
        dom.document
            .add_event_listener_with_callback("keydown", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }
    {
        let app = Rc::clone(app);
        let callback = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
            if keyboard_code(&event.code()).is_some() {
                event.prevent_default();
                let mut app = app.borrow_mut();
                app.active_keys.remove(&event.code());
                app.rebuild_keyboard_bits();
            }
        });
        dom.document
            .add_event_listener_with_callback("keyup", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }
    {
        let app = Rc::clone(app);
        let callback = Closure::<dyn FnMut(MouseEvent)>::new(move |event: MouseEvent| {
            if let Some(bit) = mouse_button(event.button()) {
                event.prevent_default();
                let mut app = app.borrow_mut();
                let newly_pressed = app
                    .active_mouse_buttons
                    .insert(event.button(), bit)
                    .is_none();
                if newly_pressed && let Some(runtime) = &mut app.runtime {
                    runtime.pending_buttons |= bit;
                    runtime.resume_audio();
                }
            }
        });
        dom.screen
            .add_event_listener_with_callback("mousedown", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }
    {
        let app = Rc::clone(app);
        let callback = Closure::<dyn FnMut(MouseEvent)>::new(move |event: MouseEvent| {
            if mouse_button(event.button()).is_some() {
                event.prevent_default();
                app.borrow_mut()
                    .active_mouse_buttons
                    .remove(&event.button());
            }
        });
        window()?.add_event_listener_with_callback("mouseup", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }
    {
        let app = Rc::clone(app);
        let callback = Closure::<dyn FnMut(Event)>::new(move |_| {
            let mut app = app.borrow_mut();
            app.keyboard_bits = 0;
            app.active_keys.clear();
            app.active_mouse_buttons.clear();
            app.active_touches.clear();
            if let Some(runtime) = &mut app.runtime {
                runtime.pending_buttons = 0;
            }
        });
        window()?.add_event_listener_with_callback("blur", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }

    bind_touch_controls(app, &dom)?;
    {
        let app = Rc::clone(app);
        let callback = Closure::<dyn FnMut(Event)>::new(move |_| app.borrow_mut().flush());
        window()?
            .add_event_listener_with_callback("pagehide", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }
    Ok(())
}

fn bind_touch_controls(app: &Rc<RefCell<App>>, dom: &Dom) -> Result<(), JsValue> {
    let controls = dom.document.query_selector_all("[data-pad]")?;
    for index in 0..controls.length() {
        let Some(node) = controls.get(index) else {
            continue;
        };
        let element: HtmlElement = node.dyn_into()?;
        let Some(bit) = element
            .get_attribute("data-pad")
            .and_then(|value| value.parse::<u16>().ok())
        else {
            continue;
        };
        {
            let app = Rc::clone(app);
            let target = element.clone();
            let visual = element.clone();
            let callback = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
                event.prevent_default();
                let _ = visual.class_list().add_1("is-held");
                let mut app = app.borrow_mut();
                app.active_touches.insert(event.pointer_id(), bit);
                if let Some(runtime) = &mut app.runtime {
                    runtime.pending_buttons |= bit;
                    runtime.resume_audio();
                }
            });
            target.add_event_listener_with_callback(
                "pointerdown",
                callback.as_ref().unchecked_ref(),
            )?;
            callback.forget();
        }
        for event_name in ["pointerup", "pointercancel", "lostpointercapture"] {
            let app = Rc::clone(app);
            let target = element.clone();
            let visual = element.clone();
            let callback = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
                event.prevent_default();
                let _ = visual.class_list().remove_1("is-held");
                app.borrow_mut().active_touches.remove(&event.pointer_id());
            });
            target
                .add_event_listener_with_callback(event_name, callback.as_ref().unchecked_ref())?;
            callback.forget();
        }
    }
    Ok(())
}

fn bind_click(
    target: &web_sys::EventTarget,
    mut action: impl FnMut() + 'static,
) -> Result<(), JsValue> {
    let callback = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        event.prevent_default();
        action();
    });
    target.add_event_listener_with_callback("click", callback.as_ref().unchecked_ref())?;
    callback.forget();
    Ok(())
}

fn import_files(app: Rc<RefCell<App>>, files: &FileList) {
    let mut disc = None;
    let mut extracted = Vec::new();
    for index in 0..files.length() {
        let Some(file) = files.get(index) else {
            continue;
        };
        if disc.is_none() && is_disc_image_name(&file.name()) {
            disc = Some(file);
        } else {
            extracted.push(file);
        }
    }
    spawn_local(async move {
        {
            let mut app = app.borrow_mut();
            if let Err(error) = app.begin_import(if disc.is_some() {
                "Reading local disc index"
            } else {
                "Indexing extracted streams"
            }) {
                app.fail(&js_message(&error));
                return;
            }
            let mut accepted = 0;
            for file in extracted {
                match app.assets.insert_file(file) {
                    Ok(true) => accepted += 1,
                    Ok(false) => {}
                    Err(error) => {
                        app.fail(&js_message(&error));
                        return;
                    }
                }
            }
            if accepted > 0 {
                app.dom.log(
                    &format!("Indexed {accepted} extracted stream files."),
                    false,
                );
            }
        }
        if let Some(file) = disc {
            match discover_disc(&file).await {
                Ok(discovery) => {
                    let mut app = app.borrow_mut();
                    match app.assets.insert_disc_streams(
                        &file,
                        discovery.layout,
                        &discovery.streams,
                    ) {
                        Ok(count) => app.dom.log(
                            &format!(
                                "Mounted {count} streams from {} without uploading it.",
                                discovery.layout.label()
                            ),
                            false,
                        ),
                        Err(error) => {
                            app.fail(&js_message(&error));
                            return;
                        }
                    }
                }
                Err(error) => {
                    app.borrow_mut().fail(&js_message(&error));
                    return;
                }
            }
        }
        let mut app = app.borrow_mut();
        if app.assets.pair_count() == 0 {
            app.fail("No playable NSD/NSF pair was found in the selected files.");
            return;
        }
        if let Err(error) = app.finish_import("Local game data is ready.") {
            app.fail(&js_message(&error));
        }
    });
}

fn is_disc_image_name(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        extension.eq_ignore_ascii_case("bin") || extension.eq_ignore_ascii_case("iso")
    })
}

fn launch(app: Rc<RefCell<App>>) {
    let (store, level, unlock_all_levels) = {
        let mut app_ref = app.borrow_mut();
        if app_ref.busy || app_ref.locked {
            return;
        }
        let Ok(raw) = app_ref.dom.boot_level.value().parse::<u32>() else {
            app_ref.fail("Choose a valid boot target.");
            return;
        };
        let Ok(level) = FormatLevelId::new(raw) else {
            app_ref.fail("Choose a valid boot target.");
            return;
        };
        if let Err(error) = app_ref.begin_import("Validating selected stream pair") {
            app_ref.fail(&js_message(&error));
            return;
        }
        let _ = app_ref
            .dom
            .set_progress(true, 0.2, "Parsing NSD and NSF pages");
        (
            app_ref.assets.clone(),
            level,
            app_ref.dom.unlock_all.checked(),
        )
    };
    spawn_local(async move {
        match store.validate_pair(level).await {
            Ok(pair) => {
                let mut app = app.borrow_mut();
                if let Err(error) = app.start_runtime(pair, unlock_all_levels) {
                    app.fail(&js_message(&error));
                }
            }
            Err(error) => app.borrow_mut().fail(&js_message(&error)),
        }
    });
}

#[cfg(feature = "browser-test-harness")]
fn step_browser_test_frame(
    app: &Rc<RefCell<App>>,
    harness: &Object,
    clock_and_count: &Rc<RefCell<(BrowserTestClock, u64)>>,
    input: BrowserTestPadInput,
) {
    let (timestamp_ms, step_count) = {
        let mut state = clock_and_count.borrow_mut();
        let timestamp_ms = state.0.take_timestamp_ms();
        state.1 = state.1.wrapping_add(1);
        (timestamp_ms, state.1)
    };
    let (frame_result, effective_held) = {
        let mut app = app.borrow_mut();
        app.browser_test_input = input;
        let frame_result = app.frame(timestamp_ms);
        let effective_held = app
            .runtime
            .as_ref()
            .map_or_else(|| input.held_word(), |runtime| runtime.pad.snapshot().held);
        (frame_result, effective_held)
    };
    let requested_lid = match frame_result {
        Ok(Some(level)) => {
            load_level_pair(Rc::clone(app), level);
            JsValue::from_f64(f64::from(level.get()))
        }
        Ok(None) => JsValue::NULL,
        Err(error) => {
            let message = js_message(&error);
            let _ = Reflect::set(
                harness.as_ref(),
                &JsValue::from_str("lastError"),
                &JsValue::from_str(&message),
            );
            app.borrow_mut().fail(&message);
            JsValue::NULL
        }
    };
    for (name, value) in [
        ("lastHeld", JsValue::from_f64(f64::from(effective_held))),
        ("lastInputKind", JsValue::from_str(input.input_kind())),
        ("lastTimestampMs", JsValue::from_f64(timestamp_ms)),
        ("stepCount", JsValue::from_f64(step_count as f64)),
        ("lastRequestedLid", requested_lid),
    ] {
        let _ = Reflect::set(harness.as_ref(), &JsValue::from_str(name), &value);
    }
}

#[cfg(feature = "browser-test-harness")]
fn install_browser_test_harness(
    app: &Rc<RefCell<App>>,
    browser_window: &web_sys::Window,
) -> Result<(), JsValue> {
    let harness = Object::new();
    Reflect::set(
        harness.as_ref(),
        &JsValue::from_str("mode"),
        &JsValue::from_str("manual-34ms"),
    )?;
    Reflect::set(
        harness.as_ref(),
        &JsValue::from_str("frameDurationMs"),
        &JsValue::from_f64(BrowserTestClock::FRAME_DURATION_MS),
    )?;
    Reflect::set(
        harness.as_ref(),
        &JsValue::from_str("stepCount"),
        &JsValue::from_f64(0.0),
    )?;
    Reflect::set(
        harness.as_ref(),
        &JsValue::from_str("lastError"),
        &JsValue::NULL,
    )?;

    let debug = app.borrow().debug.clone();
    let app_for_live_objects = Rc::clone(app);
    let harness_for_live_objects = harness.clone();
    let snapshot_retail_objects = Closure::<dyn FnMut() -> JsValue>::new(move || {
        let result = app_for_live_objects
            .try_borrow()
            .map_err(|_| JsValue::from_str("runtime is busy while observing live objects"))
            .and_then(|app| {
                app.runtime.as_ref().map_or_else(
                    || Ok(JsValue::from(js_sys::Array::new())),
                    |runtime| browser_test_live_objects_value(&runtime.retail_objects),
                )
            });
        match result {
            Ok(value) => value,
            Err(error) => {
                let message = js_message(&error);
                let _ = Reflect::set(
                    harness_for_live_objects.as_ref(),
                    &JsValue::from_str("lastError"),
                    &JsValue::from_str(&message),
                );
                JsValue::NULL
            }
        }
    });
    Reflect::set(
        debug.as_ref(),
        &JsValue::from_str("snapshotRetailObjects"),
        snapshot_retail_objects.as_ref().unchecked_ref(),
    )?;

    let clock_and_count = Rc::new(RefCell::new((BrowserTestClock::default(), 0_u64)));
    let app_for_step = Rc::clone(app);
    let harness_for_step = harness.clone();
    let clock_and_count_for_step = Rc::clone(&clock_and_count);
    let step = Closure::<dyn FnMut(u32)>::new(move |raw_held| {
        let input = match BrowserTestPadInput::physical(raw_held) {
            Ok(input) => input,
            Err(message) => {
                let _ = Reflect::set(
                    harness_for_step.as_ref(),
                    &JsValue::from_str("lastError"),
                    &JsValue::from_str(message),
                );
                return;
            }
        };
        step_browser_test_frame(
            &app_for_step,
            &harness_for_step,
            &clock_and_count_for_step,
            input,
        );
    });
    let app_for_recorded_step = Rc::clone(app);
    let harness_for_recorded_step = harness.clone();
    let clock_and_count_for_recorded_step = Rc::clone(&clock_and_count);
    let step_recorded = Closure::<dyn FnMut(u32)>::new(move |held| {
        step_browser_test_frame(
            &app_for_recorded_step,
            &harness_for_recorded_step,
            &clock_and_count_for_recorded_step,
            BrowserTestPadInput::recorded(held),
        );
    });
    Reflect::set(
        harness.as_ref(),
        &JsValue::from_str("step"),
        step.as_ref().unchecked_ref(),
    )?;
    Reflect::set(
        harness.as_ref(),
        &JsValue::from_str("stepRecorded"),
        step_recorded.as_ref().unchecked_ref(),
    )?;
    Reflect::set(
        browser_window.as_ref(),
        &JsValue::from_str("__crustTest"),
        harness.as_ref(),
    )?;
    step.forget();
    step_recorded.forget();
    snapshot_retail_objects.forget();
    Ok(())
}

#[cfg(feature = "browser-test-harness")]
fn browser_test_live_objects_value(runtime: &RetailRuntime) -> Result<JsValue, JsValue> {
    let objects = js_sys::Array::new();
    for snapshot in runtime.browser_test_live_objects().map_err(|error| {
        JsValue::from_str(&format!(
            "browser-test live object snapshot failed: {error:?}"
        ))
    })? {
        objects.push(&browser_test_live_object_value(&snapshot)?);
    }
    Ok(objects.into())
}

#[cfg(feature = "browser-test-harness")]
fn browser_test_live_object_value(snapshot: &BrowserTestLiveObject) -> Result<JsValue, JsValue> {
    let object = Object::new();
    Reflect::set(
        object.as_ref(),
        &JsValue::from_str("handle"),
        &browser_test_object_handle_value(snapshot.handle)?,
    )?;
    for (name, value) in [
        ("entityId", snapshot.entity_id.map(u32::from)),
        ("entityGroup", snapshot.entity_group.map(u32::from)),
        ("programEid", snapshot.program_eid.map(Eid::raw)),
    ] {
        Reflect::set(
            object.as_ref(),
            &JsValue::from_str(name),
            &value.map_or(JsValue::NULL, |value| JsValue::from_f64(f64::from(value))),
        )?;
    }
    for (name, value) in [
        ("executable", u32::from(snapshot.executable)),
        ("spawnSubtype", u32::from(snapshot.spawn_subtype)),
        ("subtype", snapshot.subtype),
        ("state", u32::from(snapshot.state)),
        (
            "pc",
            u32::try_from(snapshot.pc)
                .map_err(|_| JsValue::from_str("retail object PC exceeds 32 bits"))?,
        ),
        ("zoneEid", snapshot.zone.raw()),
    ] {
        Reflect::set(
            object.as_ref(),
            &JsValue::from_str(name),
            &JsValue::from_f64(f64::from(value)),
        )?;
    }
    Reflect::set(
        object.as_ref(),
        &JsValue::from_str("zone"),
        &JsValue::from_str(&snapshot.zone.to_string()),
    )?;
    Reflect::set(
        object.as_ref(),
        &JsValue::from_str("origin"),
        &JsValue::from_str(if snapshot.entity_id.is_some() {
            "entity"
        } else {
            "runtime"
        }),
    )?;

    Reflect::set(
        object.as_ref(),
        &JsValue::from_str("translation"),
        &browser_test_vector_value(snapshot.translation, ["x", "y", "z"])?,
    )?;
    Reflect::set(
        object.as_ref(),
        &JsValue::from_str("rotationYxz"),
        &browser_test_vector_value(snapshot.rotation_yxz, ["y", "x", "z"])?,
    )?;
    Reflect::set(
        object.as_ref(),
        &JsValue::from_str("velocity"),
        &browser_test_vector_value(snapshot.velocity, ["x", "y", "z"])?,
    )?;
    let frame_bound = if let Some(bound) = snapshot.frame_bound {
        let value = Object::new();
        Reflect::set(
            value.as_ref(),
            &JsValue::from_str("min"),
            &browser_test_vector_value([bound.min.x, bound.min.y, bound.min.z], ["x", "y", "z"])?,
        )?;
        Reflect::set(
            value.as_ref(),
            &JsValue::from_str("max"),
            &browser_test_vector_value([bound.max.x, bound.max.y, bound.max.z], ["x", "y", "z"])?,
        )?;
        JsValue::from(value)
    } else {
        JsValue::NULL
    };
    Reflect::set(
        object.as_ref(),
        &JsValue::from_str("frameBound"),
        &frame_bound,
    )?;

    let status = Object::new();
    for (name, value) in [
        ("a", snapshot.status_a),
        ("b", snapshot.status_b),
        ("c", snapshot.status_c),
        ("stateFlags", snapshot.state_flags),
    ] {
        Reflect::set(
            status.as_ref(),
            &JsValue::from_str(name),
            &JsValue::from_f64(f64::from(value)),
        )?;
    }
    Reflect::set(
        object.as_ref(),
        &JsValue::from_str("status"),
        status.as_ref(),
    )?;
    Reflect::set(
        object.as_ref(),
        &JsValue::from_str("player"),
        &JsValue::from_bool(snapshot.is_player),
    )?;
    Reflect::set(
        object.as_ref(),
        &JsValue::from_str("collider"),
        &snapshot.collider.map_or(Ok(JsValue::NULL), |collider| {
            browser_test_object_handle_value(collider)
        })?,
    )?;
    Reflect::set(
        object.as_ref(),
        &JsValue::from_str("faulted"),
        &JsValue::from_bool(snapshot.faulted),
    )?;
    Ok(object.into())
}

#[cfg(feature = "browser-test-harness")]
fn browser_test_vector_value(coordinates: [i32; 3], names: [&str; 3]) -> Result<JsValue, JsValue> {
    let value = Object::new();
    for (name, coordinate) in names.into_iter().zip(coordinates) {
        Reflect::set(
            value.as_ref(),
            &JsValue::from_str(name),
            &JsValue::from_f64(f64::from(coordinate)),
        )?;
    }
    Ok(value.into())
}

#[cfg(feature = "browser-test-harness")]
fn browser_test_object_handle_value(handle: RuntimeObjectHandle) -> Result<JsValue, JsValue> {
    let value = Object::new();
    for (name, word) in [
        ("arenaSlot", u32::from(handle.arena().slot())),
        ("arenaGeneration", handle.arena().generation()),
        ("vm", u32::from(handle.vm().get())),
    ] {
        Reflect::set(
            value.as_ref(),
            &JsValue::from_str(name),
            &JsValue::from_f64(f64::from(word)),
        )?;
    }
    Ok(value.into())
}

#[cfg(not(feature = "browser-test-harness"))]
type AnimationFrameCallback = Rc<RefCell<Option<Closure<dyn FnMut(f64)>>>>;

#[cfg(not(feature = "browser-test-harness"))]
fn start_animation_loop(app: &Rc<RefCell<App>>) -> Result<(), JsValue> {
    let callback_slot: AnimationFrameCallback = Rc::new(RefCell::new(None));
    let callback_slot_inner = Rc::clone(&callback_slot);
    let app_inner = Rc::clone(app);
    *callback_slot.borrow_mut() = Some(Closure::new(move |timestamp| {
        let frame_result = app_inner.borrow_mut().frame(timestamp);
        match frame_result {
            Ok(Some(level)) => load_level_pair(Rc::clone(&app_inner), level),
            Ok(None) => {}
            Err(error) => app_inner.borrow_mut().fail(&js_message(&error)),
        }
        if let Some(callback) = callback_slot_inner.borrow().as_ref() {
            let _ = window().and_then(|window| {
                window
                    .request_animation_frame(callback.as_ref().unchecked_ref())
                    .map(|_| ())
            });
        }
    }));
    let borrowed = callback_slot.borrow();
    let callback = borrowed
        .as_ref()
        .ok_or_else(|| JsValue::from_str("animation loop did not initialize"))?;
    window()?.request_animation_frame(callback.as_ref().unchecked_ref())?;
    Ok(())
}

fn load_level_pair(app: Rc<RefCell<App>>, level: FormatLevelId) {
    let store = {
        let mut app = app.borrow_mut();
        app.busy = true;
        let label = format!("Reading destination stream {level}");
        let _ = app.dom.set_runtime_state("loading", &label);
        let _ = app
            .dom
            .set_progress(true, 0.35, "Validating destination NSD/NSF");
        app.assets.clone()
    };
    spawn_local(async move {
        match store.validate_pair(level).await {
            Ok(pair) => {
                let mut app = app.borrow_mut();
                let dom = app.dom.clone();
                let result = app
                    .runtime
                    .as_mut()
                    .ok_or_else(|| JsValue::from_str("runtime ended during a level transition"))
                    .and_then(|runtime| runtime.install_level_assets(pair, &dom));
                let result = result.and_then(|()| {
                    let runtime = app.runtime.as_ref().ok_or_else(|| {
                        JsValue::from_str("runtime ended while publishing the mounted destination")
                    })?;
                    // The asynchronous mount completes between cooperative
                    // frames. Publish its new pair immediately so diagnostics
                    // and the browser campaign harness cannot mistake the
                    // previous level for the active destination or spend an
                    // extra simulation frame merely to refresh observation.
                    update_debug(&app.debug, runtime, &app.assets)
                });
                match result {
                    Ok(()) => {
                        app.busy = false;
                        let _ = app.dom.set_progress(false, 1.0, "Destination mounted");
                        let _ = app.dom.set_runtime_state("running", "Rust runtime active");
                    }
                    Err(error) => fail_level_pair(&mut app, level, &js_message(&error)),
                }
            }
            Err(error) => {
                let mut app = app.borrow_mut();
                fail_level_pair(&mut app, level, &js_message(&error));
            }
        }
    });
}

fn fail_level_pair(app: &mut App, level: FormatLevelId, message: &str) {
    if let Some(runtime) = &mut app.runtime {
        runtime.fail_level_assets(level, message);
    }
    app.fail(&format!("Could not mount destination {level}: {message}"));
}

fn poll_gamepad() -> Result<u16, JsValue> {
    let gamepads = window()?.navigator().get_gamepads()?;
    let Some(gamepad) = gamepads
        .iter()
        .find_map(|value| value.dyn_into::<Gamepad>().ok())
    else {
        return Ok(0);
    };
    let buttons = gamepad
        .buttons()
        .iter()
        .map(|value| {
            value
                .dyn_into::<web_sys::GamepadButton>()
                .is_ok_and(|button| button.pressed())
        })
        .collect::<Vec<_>>();
    let axes = gamepad
        .axes()
        .iter()
        .map(|value| value.as_f64().unwrap_or_default() as f32)
        .collect::<Vec<_>>();
    Ok(standard_gamepad(&buttons, &axes))
}

fn retail_pad_snapshot(snapshot: PlatformPadSnapshot) -> RetailPadSnapshot {
    RetailPadSnapshot {
        tapped: snapshot.tapped,
        held: snapshot.held,
        held_previous: snapshot.held_previous,
        tapped_previous: snapshot.tapped_previous,
        held_previous_2: snapshot.held_previous_2,
    }
}

fn update_debug(debug: &Object, runtime: &Runtime, assets: &AssetStore) -> Result<(), JsValue> {
    let level = current_level(&runtime.flow);
    let title_state = runtime
        .retail_objects
        .retail_title_presentation()
        .ok()
        .flatten()
        .map_or(runtime.flow.title_screen(), |title| title.screen);
    Reflect::set(
        debug,
        &JsValue::from_str("frame"),
        &JsValue::from_f64(runtime.scheduler.frame_count() as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("currentLid"),
        &JsValue::from_f64(f64::from(level.raw())),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("titleState"),
        &JsValue::from_f64(f64::from(title_state as u8)),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("pairs"),
        &JsValue::from_f64(assets.pair_count() as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("mountedLid"),
        &JsValue::from_f64(f64::from(runtime.level_assets.level.get())),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("mountedPages"),
        &JsValue::from_f64(runtime.level_assets.nsf.pages.len() as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("mountedEntries"),
        &JsValue::from_f64(pair_entry_count(&runtime.level_assets) as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("glError"),
        &JsValue::from_f64(f64::from(runtime.last_gl_error)),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("paused"),
        &JsValue::from_bool(runtime.paused()),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailFrame"),
        &JsValue::from_f64(runtime.retail_objects.frame_index() as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailPathProgress"),
        &JsValue::from_f64(f64::from(runtime.retail_camera.location().progress.raw())),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailCameraZone"),
        &JsValue::from_f64(f64::from(runtime.retail_camera.location().path.zone.raw())),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailCameraPath"),
        &JsValue::from_f64(f64::from(runtime.retail_camera.location().path.index)),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailCameraGameState"),
        &JsValue::from_f64(f64::from(runtime.retail_camera.game_state())),
    )?;
    let camera_location = runtime.retail_camera.location();
    let follow_active = runtime.retail_objects.arena().main_object().is_some()
        && runtime
            .retail_zone_graph
            .path(camera_location.path)
            .is_some_and(|path| matches!(path.camera_mode, 5 | 6));
    Reflect::set(
        debug,
        &JsValue::from_str("retailCameraFollowActive"),
        &JsValue::from_bool(follow_active),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailCameraFollowSpeed"),
        &JsValue::from_f64(f64::from(runtime.retail_camera.follow_state().speed)),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailCameraFollowZoom"),
        &JsValue::from_f64(f64::from(runtime.retail_camera.follow_state().zoom)),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailDrawCount"),
        &JsValue::from_f64(f64::from(runtime.retail_frame.draw_count())),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailProcessDrawCount"),
        &JsValue::from_f64(f64::from(runtime.retail_objects.draw_count())),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailRandomSeed"),
        &JsValue::from_f64(f64::from(runtime.retail_objects.machine().random_seed())),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailRandomSeedB"),
        &JsValue::from_f64(f64::from(runtime.retail_objects.random_seed_b())),
    )?;
    let scene_cache = runtime.retail_scene_builder.diagnostics();
    Reflect::set(
        debug,
        &JsValue::from_str("retailSceneGraphBuilds"),
        &JsValue::from_f64(scene_cache.graph_builds as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailSceneGraphReuses"),
        &JsValue::from_f64(scene_cache.graph_reuses as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailSceneTexturePageInstalls"),
        &JsValue::from_f64(scene_cache.texture_page_installs as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailSceneTextureHits"),
        &JsValue::from_f64(scene_cache.texture_hits as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailSceneTextureMisses"),
        &JsValue::from_f64(scene_cache.texture_misses as f64),
    )?;
    let pad = runtime.pad.snapshot();
    Reflect::set(
        debug,
        &JsValue::from_str("retailPadHeld"),
        &JsValue::from_f64(f64::from(pad.held)),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailPadTapped"),
        &JsValue::from_f64(f64::from(pad.tapped)),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailNeighborZones"),
        &JsValue::from_f64(runtime.retail_zone_lifecycle.next_frame_spawn_scan().len() as f64),
    )?;
    let current_zone = runtime
        .retail_zone_lifecycle
        .current_zone()
        .map_or(JsValue::NULL, |zone| JsValue::from_str(&zone.to_string()));
    Reflect::set(
        debug,
        &JsValue::from_str("retailCurrentZone"),
        &current_zone,
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailZoneRevision"),
        &JsValue::from_f64(runtime.retail_zone_lifecycle.revision() as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailLoadEntryReferences"),
        &JsValue::from_f64(runtime.retail_zone_pager.total_entry_references() as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailLoadPageReferences"),
        &JsValue::from_f64(runtime.retail_zone_pager.total_page_references() as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailEntityDescriptors"),
        &JsValue::from_f64(retail_entity_count(&runtime.retail_zones) as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailLiveObjects"),
        &JsValue::from_f64(runtime.retail_objects.arena().len() as f64),
    )?;
    let retail_main = runtime
        .retail_objects
        .arena()
        .main_object()
        .and_then(|arena| runtime.retail_objects.object_for_arena(arena))
        .and_then(|handle| runtime.retail_objects.machine().object(handle.vm()).ok());
    let retail_main_debug = if let Some(object) = retail_main {
        let state = Object::new();
        let read_register = |register| {
            object.register(register).map_err(|error| {
                JsValue::from_str(&format!(
                    "retail debug register {register} is unavailable: {error:?}"
                ))
            })
        };
        for (name, value) in [
            ("state", u32::from(object.state())),
            ("pc", object.pc() as u32),
            ("statusA", read_register(process_register::STATUS_A)?),
            ("statusB", read_register(process_register::STATUS_B)?),
        ] {
            Reflect::set(
                state.as_ref(),
                &JsValue::from_str(name),
                &JsValue::from_f64(f64::from(value)),
            )?;
        }
        for (name, register) in [
            ("x", process_register::TRANSLATION_X),
            ("y", process_register::TRANSLATION_Y),
            ("z", process_register::TRANSLATION_Z),
            ("vx", process_register::MISC_A_X),
            ("vy", process_register::MISC_A_Y),
            ("vz", process_register::MISC_A_Z),
        ] {
            Reflect::set(
                state.as_ref(),
                &JsValue::from_str(name),
                &JsValue::from_f64(f64::from(read_register(register)?.cast_signed())),
            )?;
        }
        JsValue::from(state)
    } else {
        JsValue::NULL
    };
    Reflect::set(debug, &JsValue::from_str("retailMain"), &retail_main_debug)?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailFaultedObjects"),
        &JsValue::from_f64(runtime.retail_objects.faulted_object_count() as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailSpawnAttempts"),
        &JsValue::from_f64(runtime.retail_metrics.spawn_attempts as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailSuccessfulSpawns"),
        &JsValue::from_f64(runtime.retail_metrics.successful_spawns as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailAlreadyActiveSpawnSkips"),
        &JsValue::from_f64(runtime.retail_metrics.already_active_spawn_skips as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailAuthoredSpawnRejections"),
        &JsValue::from_f64(runtime.retail_metrics.authored_spawn_rejections as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailFailedSpawns"),
        &JsValue::from_f64(runtime.retail_metrics.failed_spawns as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailExecutions"),
        &JsValue::from_f64(runtime.retail_metrics.executions as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailExecutionErrors"),
        &JsValue::from_f64(runtime.retail_metrics.execution_errors as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailSpawnedChildren"),
        &JsValue::from_f64(runtime.retail_metrics.spawned_children as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailEffects"),
        &JsValue::from_f64(runtime.retail_metrics.effects as f64),
    )?;
    for (name, value) in [
        (
            "retailZoneTransitions",
            runtime.retail_metrics.zone_transitions,
        ),
        (
            "retailZoneTerminatedObjects",
            runtime.retail_metrics.zone_terminated_objects,
        ),
        (
            "retailZoneEventFailures",
            runtime.retail_metrics.zone_event_failures,
        ),
        (
            "retailCameraSaveHandshakes",
            runtime.retail_metrics.camera_save_handshakes,
        ),
    ] {
        Reflect::set(
            debug,
            &JsValue::from_str(name),
            &JsValue::from_f64(value as f64),
        )?;
    }
    #[cfg(feature = "browser-test-harness")]
    for (name, value) in [
        (
            "retailHardRestarts",
            runtime.browser_test_cumulative_metrics.hard_restarts,
        ),
        (
            "retailLoadStates",
            runtime.browser_test_cumulative_metrics.load_states,
        ),
        (
            "retailDeathCameraFrames",
            runtime.browser_test_cumulative_metrics.death_camera_frames,
        ),
    ] {
        Reflect::set(
            debug,
            &JsValue::from_str(name),
            &JsValue::from_f64(value as f64),
        )?;
    }
    Reflect::set(
        debug,
        &JsValue::from_str("retailRuntimeError"),
        &runtime
            .retail_runtime_error
            .as_deref()
            .map_or(JsValue::NULL, JsValue::from_str),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailRuntimeWarning"),
        &runtime
            .retail_runtime_warning
            .as_deref()
            .map_or(JsValue::NULL, JsValue::from_str),
    )?;
    let retail_audio_metrics = runtime.retail_audio.metrics();
    for (name, value) in [
        (
            "retailAudioActiveVoices",
            runtime.retail_audio.active_sfx_count() as u64,
        ),
        (
            "retailAudioCompletedRekeys",
            u64::from(runtime.retail_audio.completed_sample_rekey_count()),
        ),
        ("retailAudioCallbacks", retail_audio_metrics.callbacks),
        ("retailAudioCacheHits", retail_audio_metrics.cache_hits),
        ("retailAudioCacheMisses", retail_audio_metrics.cache_misses),
    ] {
        Reflect::set(
            debug,
            &JsValue::from_str(name),
            &JsValue::from_f64(value as f64),
        )?;
    }
    if let Some(audio) = &runtime.audio {
        let metrics = audio.metrics();
        let output = audio.output_options();
        Reflect::set(
            debug,
            &JsValue::from_str("audioCallbacks"),
            &JsValue::from_f64(metrics.callbacks as f64),
        )?;
        Reflect::set(
            debug,
            &JsValue::from_str("audioPeak"),
            &JsValue::from_f64(f64::from(metrics.peak)),
        )?;
        Reflect::set(
            debug,
            &JsValue::from_str("sfxVolume"),
            &JsValue::from_f64(f64::from(output.sfx_volume())),
        )?;
        Reflect::set(
            debug,
            &JsValue::from_str("musicVolume"),
            &JsValue::from_f64(f64::from(output.music_volume())),
        )?;
        Reflect::set(
            debug,
            &JsValue::from_str("mono"),
            &JsValue::from_bool(output.mono()),
        )?;
        Reflect::set(
            debug,
            &JsValue::from_str("retailMusicState"),
            &JsValue::from_str(&format!("{:?}", audio.retail_music_state())),
        )?;
        Reflect::set(
            debug,
            &JsValue::from_str("retailMusicEid"),
            &audio
                .requested_retail_music_eid()
                .map_or(JsValue::NULL, |eid| JsValue::from_f64(f64::from(eid.raw()))),
        )?;
    }
    #[cfg(feature = "browser-test-harness")]
    update_browser_test_globals(debug, runtime)?;
    Ok(())
}

#[cfg(feature = "browser-test-harness")]
fn update_browser_test_globals(debug: &Object, runtime: &Runtime) -> Result<(), JsValue> {
    let globals = Object::new();
    Reflect::set(
        globals.as_ref(),
        &JsValue::from_str("allLevels"),
        &JsValue::from_bool(runtime.level_access.all_levels()),
    )?;
    let read = |index| {
        runtime.retail_objects.global_word(index).map_err(|error| {
            JsValue::from_str(&format!(
                "browser-test global {index} is unavailable: {error:?}"
            ))
        })
    };
    for (name, index) in [
        ("lifeCount", LIFE_COUNT_GLOBAL),
        ("initialLifeCount", INITIAL_LIFE_COUNT_GLOBAL),
        ("levelsUnlocked", LEVELS_UNLOCKED_GLOBAL),
        ("itemPool2", ITEM_POOL_2_GLOBAL),
    ] {
        Reflect::set(
            globals.as_ref(),
            &JsValue::from_str(name),
            &JsValue::from_f64(f64::from(read(index)?)),
        )?;
    }
    Reflect::set(
        debug,
        &JsValue::from_str("browserTestGlobals"),
        globals.as_ref(),
    )?;
    Ok(())
}

fn current_level(flow: &RetailFlowMirror) -> LevelId {
    match flow.state() {
        FlowState::Gameplay(level) | FlowState::Bonus(level) | FlowState::Boss(level) => *level,
        FlowState::LevelComplete { .. } => LevelId::LEVEL_COMPLETE,
        FlowState::Intro => LevelId::INTRO,
        FlowState::Ending => LevelId::ENDING,
        FlowState::Boot | FlowState::Title => LevelId::TITLE,
    }
}

const fn is_retail_runtime_state(state: &FlowState) -> bool {
    matches!(
        state,
        FlowState::Title
            | FlowState::Gameplay(_)
            | FlowState::Bonus(_)
            | FlowState::Boss(_)
            | FlowState::LevelComplete { .. }
            | FlowState::Intro
            | FlowState::Ending
    )
}

fn level_name(level: LevelId) -> &'static str {
    KNOWN_LEVELS
        .iter()
        .find(|known| known.id.get() == u32::from(level.raw()))
        .map_or("Unknown level", |known| known.name)
}

const fn title_state_uses_image(screen: TitleScreen) -> bool {
    retail_title_screen_profile(screen, 0).uses_image()
}

fn apply_save(flow: &mut RetailFlowMirror, save: SaveData) {
    flow.progress.level_count = save.level_count;
    flow.progress.levels_unlocked = save.level_count;
    flow.progress.current_map_level = save.level_count;
    flow.progress.gem_count = save.gem_count;
    flow.progress.key_count = save.key_count;
    flow.progress.item_pool_1 = save.item_pool_1;
    flow.progress.item_pool_2 = save.item_pool_2;
    flow.options = GameOptions {
        sfx_volume: save.sfx_volume.min(u32::from(u8::MAX)) as u8,
        music_volume: save.music_volume.min(u32::from(u8::MAX)) as u8,
        mono: save.mono,
    };
}

fn default_save() -> SaveData {
    SaveData {
        level_count: 1,
        initial_lives: 4 << 8,
        sfx_volume: u32::from(u8::MAX),
        music_volume: u32::from(u8::MAX),
        ..SaveData::default()
    }
}

const fn output_options(options: GameOptions) -> OutputOptions {
    OutputOptions::new(options.sfx_volume, options.music_volume, options.mono)
}

fn retail_entity_count(zones: &BTreeMap<Eid, OwnedRetailZone>) -> usize {
    zones.values().map(|zone| zone.entities.len()).sum()
}

fn retail_zone_transition_color(
    zones: &BTreeMap<Eid, OwnedRetailZone>,
    zone: Eid,
) -> Result<[u8; 3], String> {
    zones
        .get(&zone)
        .map(|zone| zone.graphics.transition_color)
        .ok_or_else(|| format!("retail background zone {zone} is absent"))
}

fn retail_background_fill_for_zone(
    zones: &BTreeMap<Eid, OwnedRetailZone>,
    zone: Eid,
    display_mask: u32,
    vram_fill_color: [u8; 3],
) -> Result<Option<RetailBackgroundFill>, String> {
    let graphics = zones
        .get(&zone)
        .map(|zone| zone.graphics)
        .ok_or_else(|| format!("retail background zone {zone} is absent"))?;
    RetailBackgroundFill::for_zone(
        display_mask,
        graphics.vram_fill_height,
        vram_fill_color,
        graphics.vram_fill,
    )
    .map_err(|error| format!("retail background zone {zone}: {error}"))
}

fn parse_retail_zone_catalog(
    pair: &ValidatedPair,
    graph: &RetailZoneGraph,
    initial_zone: Eid,
) -> Result<(BTreeMap<Eid, OwnedRetailZone>, ZoneLifecycle), JsValue> {
    let mut zones = BTreeMap::new();
    let mut lifecycle_zones = Vec::with_capacity(graph.zone_count());
    for node in graph.zones() {
        let eid = node.eid;
        let (entry, header) = parse_zone_entry(pair, eid, "reachable ZDAT")?;
        let mut entities = Vec::with_capacity(header.entity_count as usize);
        for entity_index in 0..header.entity_count {
            let item_index = header.entity_item_index(entity_index).ok_or_else(|| {
                JsValue::from_str(&format!(
                    "reachable ZDAT {eid} entity {entity_index} is outside its item range"
                ))
            })?;
            let item_index = usize::try_from(item_index).map_err(|_| {
                JsValue::from_str(&format!(
                    "reachable ZDAT {eid} entity item does not fit this host"
                ))
            })?;
            let item = entry.item(item_index).ok_or_else(|| {
                JsValue::from_str(&format!(
                    "reachable ZDAT {eid} entity item {item_index} is absent"
                ))
            })?;
            let bytes = item.bytes(&pair.nsf_bytes).map_err(|error| {
                JsValue::from_str(&format!(
                    "reachable ZDAT {eid} entity item {item_index}: {error}"
                ))
            })?;
            entities.push(ZoneEntity::parse(bytes).map_err(|error| {
                JsValue::from_str(&format!(
                    "reachable ZDAT {eid} entity item {item_index}: {error}"
                ))
            })?);
        }
        lifecycle_zones.push(ZoneLifecycleZone::new(
            eid,
            header.display_flags,
            header.neighbors.iter().copied(),
            OrderedZoneLoadList::from(&header.load_list),
        ));
        if zones
            .insert(
                eid,
                OwnedRetailZone {
                    eid,
                    graphics: header.graphics,
                    entities,
                },
            )
            .is_some()
        {
            return Err(JsValue::from_str(&format!(
                "reachable zone catalog contains duplicate ZDAT {eid}"
            )));
        }
    }
    let mut lifecycle = ZoneLifecycle::new(lifecycle_zones)
        .map_err(|error| JsValue::from_str(&format!("retail zone lifecycle: {error}")))?;
    lifecycle
        .transition_with_marker(initial_zone, pair.level != FormatLevelId::TITLE)
        .map_err(|error| JsValue::from_str(&format!("initial retail zone activation: {error}")))?;
    Ok((zones, lifecycle))
}

fn build_retail_level_state_context(
    graph: &RetailZoneGraph,
    location: RetailCameraLocation,
    lifecycle: &ZoneLifecycle,
    box_count: i32,
    checkpoint_id: i32,
    checkpoint_translation: [i32; 3],
    first_spawn: bool,
) -> Result<RetailLevelStateContext, String> {
    let graphics_flags = graph
        .zone(location.path.zone)
        .ok_or_else(|| {
            format!(
                "retail save-state location references absent zone {}",
                location.path.zone
            )
        })?
        .graphics_flags;
    Ok(RetailLevelStateContext {
        location,
        graphics_flags,
        box_count,
        checkpoint_id,
        checkpoint_translation,
        first_spawn,
        active_neighbor_zones: lifecycle.active_neighbor_zones(),
    })
}

fn build_retail_zone_pager(
    pair: &ValidatedPair,
    lifecycle: &ZoneLifecycle,
) -> Result<Pager, JsValue> {
    let current_zone = lifecycle
        .current_zone()
        .ok_or_else(|| JsValue::from_str("retail zone lifecycle has no initial current zone"))?;
    let load_list = lifecycle
        .zone(current_zone)
        .ok_or_else(|| JsValue::from_str("retail lifecycle current zone is absent"))?
        .load_list();
    Pager::mount_retail_level(
        &pair.nsd,
        &pair.nsf,
        pair.level,
        current_zone,
        load_list.entries().iter().copied(),
        load_list.pages().iter().copied(),
    )
    .map_err(|error| {
        JsValue::from_str(&format!(
            "could not mount initial retail page ownership: {error:?}"
        ))
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetailZonePagingOutcome {
    None,
    Open(PagerOpenOutcome),
    Close(PagerCloseOutcome),
    OpenUnavailable,
}

fn apply_retail_zone_paging_action(
    pager: &mut Pager,
    action: ZoneTransitionAction,
) -> Result<RetailZonePagingOutcome, String> {
    match action {
        ZoneTransitionAction::CloseEntry(eid) => pager
            .close_eid_retail_with_outcome(eid)
            .map(RetailZonePagingOutcome::Close)
            .map_err(|error| format!("could not close retail transition EID {eid}: {error:?}")),
        ZoneTransitionAction::ClosePage(page) => pager
            .close_page_retail_with_outcome(page)
            .map(RetailZonePagingOutcome::Close)
            .map_err(|error| {
                format!(
                    "could not close retail transition page {}: {error:?}",
                    page.get()
                )
            }),
        ZoneTransitionAction::OpenEntry(eid) => match pager.open_eid_virtual_with_outcome(eid) {
            Ok(outcome) => Ok(RetailZonePagingOutcome::Open(outcome)),
            Err(PagingError::NoFreePhysicalSlot(_) | PagingError::NoFreeTextureSlot(_)) => {
                Ok(RetailZonePagingOutcome::OpenUnavailable)
            }
            Err(error) => Err(format!(
                "could not open retail transition EID {eid}: {error:?}"
            )),
        },
        ZoneTransitionAction::OpenPage(page) => match pager.open_page_virtual_with_outcome(page) {
            Ok(outcome) => Ok(RetailZonePagingOutcome::Open(outcome)),
            Err(PagingError::NoFreePhysicalSlot(_) | PagingError::NoFreeTextureSlot(_)) => {
                Ok(RetailZonePagingOutcome::OpenUnavailable)
            }
            Err(error) => Err(format!(
                "could not open retail transition page {}: {error:?}",
                page.get()
            )),
        },
        ZoneTransitionAction::TerminateZoneObjects(_)
        | ZoneTransitionAction::SetDisplayFlags { .. } => Ok(RetailZonePagingOutcome::None),
    }
}

fn parse_zone_entry<'a>(
    pair: &'a ValidatedPair,
    eid: Eid,
    context: &str,
) -> Result<(&'a crust_formats::stream::Entry, ZoneHeader), JsValue> {
    let entry = pair
        .nsf
        .resolve_entry(&pair.nsd, eid)
        .map_err(|error| JsValue::from_str(&format!("{context} {eid}: {error}")))?;
    if entry.entry_type != ZDAT_ENTRY_TYPE {
        return Err(JsValue::from_str(&format!(
            "{context} {eid} has type {}; expected {ZDAT_ENTRY_TYPE}",
            entry.entry_type
        )));
    }
    let header_item = entry
        .item(0)
        .ok_or_else(|| JsValue::from_str(&format!("{context} {eid} header item is absent")))?;
    let header_bytes = header_item
        .bytes(&pair.nsf_bytes)
        .map_err(|error| JsValue::from_str(&format!("{context} {eid} header: {error}")))?;
    let header = ZoneHeader::parse(header_bytes)
        .map_err(|error| JsValue::from_str(&format!("{context} {eid} header: {error}")))?;
    Ok((entry, header))
}

fn zone_retail_music_eid(pair: &ValidatedPair, zone: Eid) -> Result<Option<Eid>, String> {
    let (_, header) =
        parse_zone_entry(pair, zone, "retail zone music").map_err(|error| js_message(&error))?;
    Ok((header.graphics.midi != Eid::NONE).then_some(header.graphics.midi))
}

fn decode_retail_music(pair: &ValidatedPair, midi: Eid) -> Result<RetailMusic, String> {
    let entry = pair.nsf.resolve_entry(&pair.nsd, midi).map_err(|error| {
        format!(
            "retail MIDI {midi} in level {} does not resolve: {error}",
            pair.level
        )
    })?;
    let asset = parse_retail_midi(entry, &pair.nsf_bytes)
        .map_err(|error| format!("retail MIDI {midi}: {error}"))?;
    let mut fragments = Vec::new();
    for instrument in asset
        .header
        .instruments
        .into_iter()
        .filter(|eid| *eid != Eid::NONE)
    {
        let entry = pair
            .nsf
            .resolve_entry(&pair.nsd, instrument)
            .map_err(|error| {
                format!("retail MIDI {midi} instrument {instrument} does not resolve: {error}")
            })?;
        fragments.push(
            parse_instrument_entry(entry, &pair.nsf_bytes)
                .map_err(|error| format!("retail MIDI {midi} instrument {instrument}: {error}"))?,
        );
    }
    RetailMusic::decode(&asset, &fragments)
        .map_err(|error| format!("retail MIDI {midi} decode failed: {error}"))
}

fn decode_pair_loading_image(pair: &ValidatedPair) -> Result<Option<DecodedTexture>, JsValue> {
    let Some(payload) = pair
        .nsd
        .image_data(&pair.nsd_bytes)
        .map_err(|error| JsValue::from_str(&format!("{} loading image: {error}", pair.level)))?
    else {
        return Ok(None);
    };
    decode_loading_image(
        payload,
        pair.nsd.header.loading_image_width,
        pair.nsd.header.loading_image_height,
    )
    .map(Some)
    .map_err(|error| JsValue::from_str(&format!("{} loading image: {error}", pair.level)))
}

fn create_retail_core_objects_for_pair(
    runtime: &mut RetailRuntime,
    pair: &ValidatedPair,
    current_zone: Eid,
) -> Result<Option<RetailCoreObjects>, JsValue> {
    let mut host = NsfProgramHost::new(&pair.nsd, &pair.nsf, &pair.nsf_bytes);
    runtime
        .create_retail_core_objects(current_zone, &mut host)
        .map_err(|error| {
            JsValue::from_str(&format!(
                "could not create retail core HUD objects: {error:?}"
            ))
        })
}

fn log_retail_core_objects(dom: &Dom, created: Option<RetailCoreObjects>) {
    if let Some(objects) = created {
        dom.log(
            &format!(
                "Created retail life, fruit, and pickup HUD roots: {:?}, {:?}, {:?}.",
                objects.life, objects.fruit, objects.pickup
            ),
            false,
        );
    }
}

fn create_retail_level_misc_object_for_pair(
    runtime: &mut RetailRuntime,
    pair: &ValidatedPair,
    current_zone: Eid,
) -> Result<Option<RuntimeObjectHandle>, JsValue> {
    let mut host = NsfProgramHost::new(&pair.nsd, &pair.nsf, &pair.nsf_bytes);
    runtime
        .create_retail_level_misc_object(current_zone, &mut host)
        .map_err(|error| {
            JsValue::from_str(&format!(
                "could not create retail level-misc controller: {error:?}"
            ))
        })
}

fn log_retail_level_misc_object(dom: &Dom, created: Option<RuntimeObjectHandle>) {
    if let Some(object) = created {
        dom.log(
            &format!("Created retail LevelInitMisc root controller: {object:?}."),
            false,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn install_retail_scene_for_pair(
    pair: &ValidatedPair,
    stage: &mut GlStage,
    dom: &Dom,
    after_loading_image: bool,
    install_preview: bool,
    initial_draw_count: u32,
    point_count: NonZeroU16,
    texture_frame_snapshot: TextureFrameSnapshot,
    world_shader_snapshot: crust_sim::retail_runtime::RetailWorldShaderSnapshot,
    world_shader_render_state: RetailWorldShaderRenderState,
    background_fill: Option<RetailBackgroundFill>,
) -> Result<NonZeroU16, JsValue> {
    let draw_count = initial_draw_count.wrapping_add(u32::from(after_loading_image));
    // Title and external-transition dummy zones legally have one-point paths.
    // The gameplay loading contract asks for point one/two, so clamp only that
    // presentation selection to the validated final point.
    let path_point = crate::initial_presented_path_point(point_count, after_loading_image);
    // This build is a fallible mount preview only. The source performs world
    // transforms on actual hidden and presented frames; retaining preview
    // ripple/scratch state would add a phantom step and use the wrong camera.
    let mut preview_builder = RetailSceneBuilder::new();
    let mut preview_shader_render_state = world_shader_render_state;
    let scene = preview_builder
        .build_at_path_point_with_texture_frame(
            &pair.nsd,
            &pair.nsf,
            &pair.nsf_bytes,
            path_point,
            draw_count,
            texture_frame_snapshot,
            world_shader_snapshot,
            &mut preview_shader_render_state,
        )
        .map_err(|error| {
            JsValue::from_str(&format!(
                "retail first-presented snapshot for {} is invalid: {error}",
                pair.level
            ))
        })?;
    let stats = scene.stats;
    debug_assert_eq!(scene.path_point_count, point_count.get());
    if install_preview {
        stage.install_retail_scene(scene, background_fill)?;
    }
    if stats.worlds == 0 {
        dom.log(
            "Mounted the retail zero-world transition/title spawn zone.",
            false,
        );
    } else {
        dom.log(
            &format!(
                "Built retail first-presented snapshot: {} worlds, {}/{} polygons, {} decoded textures{}.",
                stats.worlds,
                stats.submitted_polygons,
                stats.visible_polygons,
                stats.unique_textures,
                if stats.skipped_textured_polygons == 0 {
                    String::new()
                } else {
                    format!(
                        ", {} safely skipped texture references",
                        stats.skipped_textured_polygons
                    )
                }
            ),
            false,
        );
    }
    Ok(point_count)
}

fn retail_spawn_point_count(graph: &RetailZoneGraph) -> Result<NonZeroU16, JsValue> {
    let spawn = graph.spawn_path();
    let path = graph.path(spawn).ok_or_else(|| {
        JsValue::from_str(&format!(
            "retail camera graph has no spawn path {}:{}",
            spawn.zone, spawn.index
        ))
    })?;
    let point_count = u16::try_from(path.points.len())
        .ok()
        .and_then(NonZeroU16::new)
        .ok_or_else(|| {
            JsValue::from_str(&format!(
                "retail spawn path {}:{} has invalid point count {}",
                spawn.zone,
                spawn.index,
                path.points.len()
            ))
        })?;
    Ok(point_count)
}

fn pair_entry_count(pair: &ValidatedPair) -> usize {
    pair.nsf
        .pages
        .iter()
        .map(|page| match page {
            crust_formats::stream::NsfPage::Texture(_) => 0,
            crust_formats::stream::NsfPage::Entries(page) => page.entries.len(),
        })
        .sum()
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 || value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn js_message(error: &JsValue) -> String {
    error
        .as_string()
        .unwrap_or_else(|| "browser operation failed".to_owned())
}
