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
use std::collections::HashMap;
use std::rc::Rc;

use crust_audio::output::OutputOptions;
use crust_formats::binary::Eid;
use crust_formats::stream::{
    KNOWN_LEVELS, LevelId as FormatLevelId, RetailZoneGraph, ZoneEntity, ZoneHeader,
};
use crust_platform::input::{
    PAD_CIRCLE, PAD_CROSS, PAD_DOWN, PAD_LEFT, PAD_RIGHT, PAD_SQUARE, PAD_START, PAD_UP,
    PadSnapshot as PlatformPadSnapshot, PadState as PlatformPadState, keyboard_code,
    standard_gamepad,
};
use crust_renderer::texture::{DecodedTexture, decode_loading_image};
use crust_renderer::title::decode_title_card;
use crust_sim::Vec3;
use crust_sim::camera::{
    RetailCameraFollowInput, RetailCameraInput, RetailCameraLocation, RetailCameraRuntime,
    RetailCameraStep,
};
use crust_sim::card::{
    CardOperation, CardOutcome, ResumeLoadResult, ResumeManager, SaveData, VirtualCard,
};
use crust_sim::flow::{
    FlowCommand, FlowEvent, FlowState, GameFlow, GameOptions, LevelId, MenuChoice, TitlePhase,
    TitleScreen,
};
use crust_sim::gool::{RetailPadSnapshot, process_register};
use crust_sim::object_arena::NeighborZone;
use crust_sim::player::PadState as SimPadState;
use crust_sim::retail_frame::{PresentedFrame, RetailFrameState};
use crust_sim::retail_runtime::{NsfProgramHost, RetailRuntime, RuntimeFrame};
use crust_sim::scheduler::{FrameDecision, FrameScheduler};
use js_sys::{Object, Reflect};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;
use wasm_bindgen_futures::spawn_local;
use web_sys::{
    DragEvent, Event, FileList, Gamepad, HtmlElement, HtmlOptionElement, KeyboardEvent,
    PointerEvent,
};

use crate::assets::{AssetStore, ValidatedPair};
use crate::disc_import::discover_disc;
use crate::dom::{Dom, window};
use crate::retail_scene::{RetailSceneBuilder, RetailSceneProgressLocation};
use crate::storage::StorageState;
use crate::webaudio::WebAudio;
use crate::webgl::{GlStage, VisualState};

const ZDAT_ENTRY_TYPE: u32 = 7;
const RETAIL_GLOBAL_WORDS: usize = 256;
const RETAIL_INSTRUCTION_BUDGET: usize = 67;

pub fn boot() -> Result<(), JsValue> {
    let dom = Dom::find()?;
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
        active_touches: HashMap::new(),
        busy: false,
        locked: false,
        muted: false,
        debug,
    }));
    bind_events(&app)?;
    app.borrow_mut().refresh_assets()?;
    start_animation_loop(&app)?;
    Ok(())
}

struct App {
    dom: Dom,
    assets: AssetStore,
    storage: Option<StorageState>,
    runtime: Option<Runtime>,
    keyboard_bits: u16,
    active_touches: HashMap<i32, u16>,
    busy: bool,
    locked: bool,
    muted: bool,
    debug: Object,
}

impl App {
    fn touch_bits(&self) -> u16 {
        self.active_touches
            .values()
            .copied()
            .fold(0_u16, |value, bit| value | bit)
    }

    fn frame(&mut self, timestamp_ms: f64) -> Result<Option<FormatLevelId>, JsValue> {
        let held = self.keyboard_bits | self.touch_bits() | poll_gamepad()?;
        if let Some(runtime) = &mut self.runtime {
            runtime.frame(timestamp_ms, held, &self.dom)?;
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
            option.set_text(&format!("{level} — {name}"));
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

    fn start_runtime(&mut self, pair: ValidatedPair) -> Result<(), JsValue> {
        let available = self
            .assets
            .playable_levels()
            .into_iter()
            .filter_map(|(level, _)| u8::try_from(level.get()).ok().and_then(LevelId::new))
            .collect();
        let storage = self.storage.take().or_else(|| StorageState::open().ok());
        let mut runtime = Runtime::new(pair, available, storage, &self.dom)?;
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
struct OwnedNeighborZone {
    eid: Eid,
    display_flags: u32,
    entities: Vec<ZoneEntity>,
}

#[derive(Debug, Default)]
struct RetailRuntimeMetrics {
    spawn_attempts: u64,
    successful_spawns: u64,
    failed_spawns: u64,
    executions: u64,
    execution_errors: u64,
    spawned_children: u64,
    effects: u64,
}

impl RetailRuntimeMetrics {
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

struct Runtime {
    flow: GameFlow,
    scheduler: FrameScheduler,
    pad: PlatformPadState,
    stage: GlStage,
    retail_frame: RetailFrameState,
    retail_objects: RetailRuntime,
    retail_neighbors: Vec<OwnedNeighborZone>,
    retail_tick_state: RetailTickState,
    retail_metrics: RetailRuntimeMetrics,
    retail_runtime_error: Option<String>,
    retail_runtime_warning: Option<String>,
    retail_scene_builder: RetailSceneBuilder,
    retail_zone_graph: RetailZoneGraph,
    retail_camera: RetailCameraRuntime,
    show_loading_image: bool,
    level_assets: ValidatedPair,
    audio: Option<WebAudio>,
    storage: Option<StorageState>,
    card: VirtualCard,
    resume: ResumeManager,
    available_levels: Vec<LevelId>,
    menu_index: usize,
    password_digits: [u8; 8],
    password_cursor: usize,
    last_title_state: Option<u8>,
    pending_buttons: u16,
    pending_asset_level: Option<FormatLevelId>,
    loading_asset_level: Option<FormatLevelId>,
    asset_load_error: Option<String>,
    muted: bool,
    last_gl_error: u32,
}

impl Runtime {
    fn new(
        pair: ValidatedPair,
        available_levels: Vec<LevelId>,
        mut storage: Option<StorageState>,
        dom: &Dom,
    ) -> Result<Self, JsValue> {
        let raw_level = u8::try_from(pair.level.get())
            .map_err(|_| JsValue::from_str("selected level does not fit the retail id"))?;
        let boot_level = LevelId::new(raw_level)
            .ok_or_else(|| JsValue::from_str("selected level is not in the retail catalog"))?;
        let seed = hash_pair(&pair);
        let mut flow = GameFlow::new();
        flow.command(FlowCommand::Boot(boot_level))
            .map_err(|error| JsValue::from_str(&format!("could not boot level: {error:?}")))?;
        let retail_neighbors = parse_retail_neighbors(&pair)?;
        dom.log(
            &format!(
                "Parsed {} current-zone neighbors with {} owned retail entity descriptors.",
                retail_neighbors.len(),
                retail_entity_count(&retail_neighbors),
            ),
            false,
        );

        let mut save = default_save();
        let card = storage
            .as_ref()
            .map_or_else(VirtualCard::new, StorageState::virtual_card);
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
            dom.log("Quarantined an invalid browser resume record.", true);
        }

        let audio = match WebAudio::new(seed) {
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
        let retail_zone_graph = RetailZoneGraph::from_pair(&pair.nsd, &pair.nsf, &pair.nsf_bytes)
            .map_err(|error| {
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
        let retail_point_count = retail_spawn_point_count(&retail_zone_graph)?;
        dom.log(
            &format!(
                "Validated a pointer-free camera graph with {} zones and {} paths.",
                retail_zone_graph.zone_count(),
                retail_zone_graph.path_count(),
            ),
            false,
        );
        let mut retail_scene_builder = RetailSceneBuilder::new();
        install_retail_scene_for_pair(
            &pair,
            &mut retail_scene_builder,
            &mut stage,
            dom,
            after_loading_image,
            retail_point_count,
        )?;
        let retail_frame = if after_loading_image {
            RetailFrameState::after_loading_image(retail_point_count, 0)
        } else {
            RetailFrameState::ready(retail_point_count, 0)
        };
        let mut last_title_state = None;
        if boot_level == LevelId::TITLE {
            let state = flow.title().screen() as u8;
            if title_state_uses_image(flow.title().screen()) {
                let card = decode_title_card(&pair.nsd, &pair.nsf, &pair.nsf_bytes, state)
                    .map_err(|error| {
                        JsValue::from_str(&format!("retail title state {state}: {error}"))
                    })?;
                stage.install_title_image(&card.image)?;
                dom.log(
                    &format!(
                        "Composed retail title state {state} from {}x{} MDAT tiles.",
                        card.width_tiles, card.height_tiles
                    ),
                    false,
                );
            }
            last_title_state = Some(state);
        }
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
        let _ = save;
        Ok(Self {
            flow,
            scheduler: FrameScheduler::new(),
            pad: PlatformPadState::default(),
            stage,
            retail_frame,
            retail_objects: RetailRuntime::new(RETAIL_GLOBAL_WORDS),
            retail_neighbors,
            retail_tick_state: RetailTickState::NeedsSpawn,
            retail_metrics: RetailRuntimeMetrics::default(),
            retail_runtime_error: None,
            retail_runtime_warning: None,
            retail_scene_builder,
            retail_zone_graph,
            retail_camera,
            show_loading_image: after_loading_image,
            level_assets: pair,
            audio,
            storage: storage.take(),
            card,
            resume,
            available_levels,
            menu_index: 0,
            password_digits: [0; 8],
            password_cursor: 0,
            last_title_state,
            pending_buttons: 0,
            pending_asset_level: None,
            loading_asset_level: None,
            asset_load_error: None,
            muted: false,
            last_gl_error: 0,
        })
    }

    fn take_asset_request(&mut self) -> Option<FormatLevelId> {
        let level = self.pending_asset_level.take()?;
        if level == self.level_assets.level {
            return None;
        }
        self.loading_asset_level = Some(level);
        self.asset_load_error = None;
        self.scheduler.set_paused(true);
        Some(level)
    }

    fn install_level_assets(&mut self, pair: ValidatedPair, dom: &Dom) -> Result<(), JsValue> {
        if self.loading_asset_level != Some(pair.level) {
            return Err(JsValue::from_str(
                "validated stream pair does not match the pending transition",
            ));
        }
        let retail_neighbors = parse_retail_neighbors(&pair)?;
        dom.log(
            &format!(
                "Parsed {} destination-zone neighbors with {} owned retail entity descriptors.",
                retail_neighbors.len(),
                retail_entity_count(&retail_neighbors),
            ),
            false,
        );
        let retail_zone_graph = RetailZoneGraph::from_pair(&pair.nsd, &pair.nsf, &pair.nsf_bytes)
            .map_err(|error| {
            JsValue::from_str(&format!(
                "destination camera graph for {} is invalid: {error}",
                pair.level
            ))
        })?;
        let retail_camera = RetailCameraRuntime::new(&retail_zone_graph).map_err(|error| {
            JsValue::from_str(&format!(
                "destination camera state for {} is invalid: {error}",
                pair.level
            ))
        })?;
        let retail_point_count = retail_spawn_point_count(&retail_zone_graph)?;
        let after_loading_image = if let Some(image) = decode_pair_loading_image(&pair)? {
            self.stage.install_loading_image(&image)?;
            dom.log(
                &format!(
                    "Decoded and uploaded the {}x{} destination loading image.",
                    image.width(),
                    image.height()
                ),
                false,
            );
            true
        } else {
            false
        };
        // A validated pair transition gets a fresh owner so parsed graph data,
        // TPAG mappings, and decoded pixels cannot cross the mount boundary.
        let mut retail_scene_builder = RetailSceneBuilder::new();
        install_retail_scene_for_pair(
            &pair,
            &mut retail_scene_builder,
            &mut self.stage,
            dom,
            after_loading_image,
            retail_point_count,
        )?;
        self.retail_frame = if after_loading_image {
            RetailFrameState::after_loading_image(retail_point_count, 0)
        } else {
            RetailFrameState::ready(retail_point_count, 0)
        };
        self.show_loading_image = after_loading_image;
        let pages = pair.nsf.pages.len();
        let entries = pair_entry_count(&pair);
        let level = pair.level;
        self.level_assets = pair;
        self.retail_scene_builder = retail_scene_builder;
        self.retail_zone_graph = retail_zone_graph;
        self.retail_camera = retail_camera;
        self.retail_objects = RetailRuntime::new(RETAIL_GLOBAL_WORDS);
        self.retail_neighbors = retail_neighbors;
        self.retail_tick_state = RetailTickState::NeedsSpawn;
        self.retail_metrics = RetailRuntimeMetrics::default();
        self.retail_runtime_error = None;
        self.retail_runtime_warning = None;
        self.last_title_state = None;
        self.loading_asset_level = None;
        self.asset_load_error = None;
        self.scheduler.set_paused(false);
        self.scheduler.reset_deadline();
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
        self.loading_asset_level = None;
        self.asset_load_error = Some(format!("Could not mount {level}: {message}"));
        self.scheduler.set_paused(true);
    }

    fn asset_transition_level(&self) -> Option<FormatLevelId> {
        self.loading_asset_level.or(self.pending_asset_level)
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
            && self.loading_asset_level != Some(level)
            && self.pending_asset_level != Some(level)
        {
            self.pending_asset_level = Some(level);
        }
    }

    fn frame(&mut self, timestamp_ms: f64, held: u16, dom: &Dom) -> Result<(), JsValue> {
        let now_us = (timestamp_ms.max(0.0) * 1_000.0).round() as u64;
        if !self.assets_stalled() && self.scheduler.sample(now_us) == FrameDecision::Step {
            self.pad.update(held | self.pending_buttons, 0, None);
            self.pending_buttons = 0;
            let snapshot = self.pad.snapshot();
            self.retail_objects
                .set_pad_snapshot(0, retail_pad_snapshot(snapshot))
                .map_err(|error| {
                    JsValue::from_str(&format!("could not bind retail pad state: {error:?}"))
                })?;
            let sim_pad = SimPadState {
                held: u32::from(snapshot.held),
                tapped: u32::from(snapshot.tapped),
            };

            let retail_state = is_retail_runtime_state(self.flow.state());
            if retail_state
                && self.retail_runtime_error.is_none()
                && snapshot.tapped & PAD_START != 0
            {
                self.retail_tick_state = match self.retail_tick_state {
                    RetailTickState::NeedsSpawn => RetailTickState::PausedBeforeSpawn,
                    RetailTickState::PausedBeforeSpawn => RetailTickState::NeedsSpawn,
                    RetailTickState::Running => RetailTickState::Paused,
                    RetailTickState::Paused => RetailTickState::Running,
                };
                dom.log(
                    if self.paused() {
                        "Retail object simulation paused."
                    } else {
                        "Retail object simulation resumed."
                    },
                    false,
                );
            }

            if retail_state && !self.paused() && self.retail_runtime_error.is_none() {
                // Retail orders initial entity spawning before camera/world
                // work, and GOOL after it. Keep all three stages in this one
                // cooperative tick so pause cannot split their state.
                if self.retail_tick_state == RetailTickState::NeedsSpawn {
                    self.spawn_retail_objects(dom);
                }
                let mut scene_location = None;
                let camera_location = match self.update_retail_camera(snapshot) {
                    Ok(step) => Some(step.after),
                    Err(error) => {
                        let message = format!("retail camera update failed: {error}");
                        dom.log(&message, true);
                        self.retail_runtime_error = Some(message);
                        self.retail_tick_state = RetailTickState::Paused;
                        None
                    }
                };
                if let Some(camera_location) = camera_location {
                    let trace = self.retail_frame.tick();
                    self.show_loading_image =
                        matches!(trace.presented(), PresentedFrame::LoadingImage);
                    if let PresentedFrame::Gameplay { draw_count, .. } = trace.presented()
                        && is_retail_runtime_state(self.flow.state())
                    {
                        scene_location = Some((camera_location, draw_count));
                    }
                }
                if self.retail_runtime_error.is_none()
                    && self.retail_tick_state == RetailTickState::Running
                {
                    self.tick_retail_runtime(dom);
                }
                if let Some((camera_location, draw_count)) = scene_location
                    && let Err(error) = self.update_retail_scene(camera_location, draw_count, dom)
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

            self.handle_menu_input(sim_pad, dom)?;
            // Menu commands can synchronously enter a different level. Drain
            // that event first so the confirming button cannot advance the
            // destination simulation against the previous stream pair.
            self.handle_events(dom)?;
            if self.pending_asset_level.is_none() {
                if !is_retail_runtime_state(self.flow.state()) {
                    self.flow.tick(sim_pad).map_err(|error| {
                        JsValue::from_str(&format!("simulation flow failed: {error:?}"))
                    })?;
                }
                self.handle_events(dom)?;
            }
            if !is_retail_runtime_state(self.flow.state())
                && snapshot.tapped & (PAD_CROSS | PAD_SQUARE) != 0
                && let Some(audio) = &mut self.audio
            {
                audio.trigger_sfx((self.scheduler.frame_count() & 0xff) as u8);
            }
            if let Some(audio) = &mut self.audio {
                audio.tick_30_hz();
            }
            if let Some(payload) = self.resume.update(self.save_data())
                && let Some(storage) = &self.storage
            {
                storage.persist_resume(payload)?;
            }
        }
        if let Some(audio) = &mut self.audio {
            audio.schedule()?;
        }
        self.sync_title_card(dom)?;
        let assets_stalled = self.assets_stalled();
        let show_title_image = !assets_stalled
            && matches!(self.flow.state(), FlowState::Title)
            && title_state_uses_image(self.flow.title().screen());
        self.stage.render(VisualState {
            show_title_image,
            show_retail_scene: !assets_stalled && !show_title_image,
            show_loading_image: !assets_stalled && self.show_loading_image,
        })?;
        self.last_gl_error = self.stage.error();
        self.render_ui(dom)?;
        Ok(())
    }

    fn spawn_retail_objects(&mut self, dom: &Dom) {
        let attempts = {
            let neighbors = self
                .retail_neighbors
                .iter()
                .map(|neighbor| NeighborZone {
                    eid: neighbor.eid,
                    display_flags: neighbor.display_flags,
                    entities: neighbor.entities.as_slice(),
                })
                .collect::<Vec<_>>();
            let mut host = NsfProgramHost::new(
                &self.level_assets.nsd,
                &self.level_assets.nsf,
                &self.level_assets.nsf_bytes,
            );
            self.retail_objects
                .spawn_current_zone_neighbors(&neighbors, &mut host)
        };
        let attempt_count = attempts.len() as u64;
        let successful = attempts
            .iter()
            .filter(|attempt| attempt.result.is_ok())
            .count() as u64;
        let failed = attempt_count.saturating_sub(successful);
        self.retail_metrics.spawn_attempts = attempt_count;
        self.retail_metrics.successful_spawns = successful;
        self.retail_metrics.failed_spawns = failed;
        self.retail_tick_state = RetailTickState::Running;
        if let Some(error) = attempts
            .iter()
            .find_map(|attempt| attempt.result.as_ref().err())
        {
            self.retail_runtime_warning = Some(format!(
                "Retail spawn scan rejected {failed} object(s); first error: {error:?}"
            ));
        }
        dom.log(
            &format!(
                "Retail spawn scan covered {} displayed neighbor zones: {successful}/{attempt_count} group-3 entities bound.",
                self.retail_neighbors.len(),
            ),
            failed != 0,
        );
    }

    fn tick_retail_runtime(&mut self, dom: &Dom) {
        let result = {
            let mut host = NsfProgramHost::new(
                &self.level_assets.nsd,
                &self.level_assets.nsf,
                &self.level_assets.nsf_bytes,
            );
            self.retail_objects
                .run_frame(&mut host, RETAIL_INSTRUCTION_BUDGET)
        };
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
            }
            Err(error) => {
                let message = format!("retail GOOL frame failed: {error:?}");
                dom.log(&message, true);
                self.retail_runtime_error = Some(message);
            }
        }
    }

    fn update_retail_scene(
        &mut self,
        location: RetailCameraLocation,
        draw_count: u32,
        dom: &Dom,
    ) -> Result<(), JsValue> {
        let path_progress = location.progress.raw();
        let objects = match self.retail_objects.render_objects() {
            Ok(objects) => objects,
            Err(error) => {
                let warning = format!(
                    "retail render-object snapshot was rejected; presenting world only: {error:?}"
                );
                if self.retail_runtime_warning.as_deref() != Some(&warning) {
                    dom.log(&warning, true);
                    self.retail_runtime_warning = Some(warning);
                }
                Vec::new()
            }
        };
        let main_object = self
            .retail_objects
            .arena()
            .main_object()
            .and_then(|arena| self.retail_objects.object_for_arena(arena));
        let scene = self
            .retail_scene_builder
            .build_at_progress_with_objects(
                &self.level_assets.nsd,
                &self.level_assets.nsf,
                &self.level_assets.nsf_bytes,
                RetailSceneProgressLocation {
                    zone: location.path.zone,
                    path_index: location.path.index,
                    path_progress,
                    draw_count,
                },
                &objects,
                main_object,
            )
            .map_err(|error| {
                JsValue::from_str(&format!(
                    "retail scene update at progress {path_progress:#x}: {error}"
                ))
            })?;
        self.stage.update_retail_scene(scene)?;
        Ok(())
    }

    fn update_retail_camera(
        &mut self,
        snapshot: PlatformPadSnapshot,
    ) -> Result<RetailCameraStep, String> {
        let location = self.retail_camera.location();
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
            return self
                .retail_camera
                .update_follow(&self.retail_zone_graph, input)
                .map_err(|error| error.to_string());
        }
        self.retail_camera
            .update(
                &self.retail_zone_graph,
                RetailCameraInput {
                    tapped: u32::from(snapshot.tapped),
                },
            )
            .map_err(|error| error.to_string())
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
            held_buttons: u32::from(snapshot.held),
            level_id,
            // CamUpdate precedes GoolUpdate, so it observes the stamp installed
            // by the previous retail object frame.
            frames_elapsed: machine.frames_elapsed(),
            // Gem events are still a typed host boundary. Zero is the exact
            // LevelInit value and remains correct until that event is hosted.
            gem_stamp: 0,
        }))
    }

    fn paused(&self) -> bool {
        if is_retail_runtime_state(self.flow.state()) {
            self.retail_runtime_error.is_some()
                || matches!(
                    self.retail_tick_state,
                    RetailTickState::PausedBeforeSpawn | RetailTickState::Paused
                )
        } else {
            self.flow.paused()
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
        let screen = self.flow.title().screen();
        let state = screen as u8;
        if self.last_title_state == Some(state) {
            return Ok(());
        }
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

    fn handle_menu_input(&mut self, pad: SimPadState, dom: &Dom) -> Result<(), JsValue> {
        if !matches!(self.flow.state(), FlowState::Title)
            || self.flow.title().phase() != TitlePhase::Ready
        {
            self.menu_index = 0;
            return Ok(());
        }
        let screen = self.flow.title().screen();
        let item_count = match screen {
            TitleScreen::MainMenu | TitleScreen::Options => 4,
            TitleScreen::Password => 9,
            TitleScreen::Load => self.card.part_count().saturating_add(1).max(1),
            TitleScreen::Map => self.available_levels.len().max(1),
            _ => 1,
        };
        if pad.tapped & u32::from(PAD_UP) != 0 {
            self.menu_index = self.menu_index.checked_sub(1).unwrap_or(item_count - 1);
        }
        if pad.tapped & u32::from(PAD_DOWN) != 0 {
            self.menu_index = (self.menu_index + 1) % item_count;
        }
        if screen == TitleScreen::Password {
            if pad.tapped & u32::from(PAD_LEFT) != 0 {
                self.password_cursor = self.password_cursor.checked_sub(1).unwrap_or(7);
            }
            if pad.tapped & u32::from(PAD_RIGHT) != 0 {
                self.password_cursor = (self.password_cursor + 1) % 8;
            }
            if pad.tapped & u32::from(PAD_UP) != 0 {
                self.password_digits[self.password_cursor] =
                    (self.password_digits[self.password_cursor] + 1) % 10;
            }
            if pad.tapped & u32::from(PAD_DOWN) != 0 {
                self.password_digits[self.password_cursor] =
                    (self.password_digits[self.password_cursor] + 9) % 10;
            }
            if pad.tapped & u32::from(PAD_START) != 0 {
                let level_count = self
                    .password_digits
                    .iter()
                    .fold(0_u32, |sum, digit| sum + u32::from(*digit))
                    .clamp(1, 32);
                self.flow.progress.level_count = level_count;
                self.flow.progress.levels_unlocked = level_count;
                self.flow
                    .command(FlowCommand::Menu(MenuChoice::Back))
                    .map_err(flow_error)?;
                dom.log(
                    "Applied local password progression and returned to the menu.",
                    false,
                );
                return Ok(());
            }
        }

        if pad.tapped & u32::from(PAD_CIRCLE) != 0 {
            if matches!(
                screen,
                TitleScreen::Options | TitleScreen::Password | TitleScreen::Load
            ) {
                self.flow
                    .command(FlowCommand::Menu(MenuChoice::Back))
                    .map_err(flow_error)?;
            }
            return Ok(());
        }
        if pad.tapped & u32::from(PAD_CROSS) == 0 {
            if screen == TitleScreen::Options && pad.tapped & u32::from(PAD_LEFT | PAD_RIGHT) != 0 {
                self.adjust_option(pad.tapped & u32::from(PAD_RIGHT) != 0)?;
            }
            return Ok(());
        }

        match screen {
            TitleScreen::MainMenu => {
                let choice = [
                    MenuChoice::Start,
                    MenuChoice::Password,
                    MenuChoice::Load,
                    MenuChoice::Options,
                ][self.menu_index];
                self.flow
                    .command(FlowCommand::Menu(choice))
                    .map_err(flow_error)?;
            }
            TitleScreen::Options => {
                if self.menu_index == 3 {
                    self.flow
                        .command(FlowCommand::Menu(MenuChoice::Back))
                        .map_err(flow_error)?;
                } else {
                    self.adjust_option(true)?;
                }
            }
            TitleScreen::Password => {
                self.password_cursor = self.menu_index.min(7);
                self.password_digits[self.password_cursor] =
                    (self.password_digits[self.password_cursor] + 1) % 10;
            }
            TitleScreen::Load => {
                if self.menu_index >= self.card.part_count() {
                    self.flow
                        .command(FlowCommand::Menu(MenuChoice::Back))
                        .map_err(flow_error)?;
                } else if let Ok(CardOutcome::Loaded(save)) =
                    self.card
                        .control(CardOperation::LoadSelected, self.menu_index, None)
                {
                    self.flow
                        .command(FlowCommand::LoadProgress(save))
                        .map_err(flow_error)?;
                    dom.log("Loaded a checksummed virtual-card slot.", false);
                }
            }
            TitleScreen::Map => {
                if let Some(level) = self.available_levels.get(self.menu_index).copied() {
                    self.flow
                        .command(FlowCommand::SelectMapLevel(level))
                        .map_err(flow_error)?;
                }
            }
            TitleScreen::GameOver => {
                self.flow
                    .command(FlowCommand::Menu(MenuChoice::Back))
                    .map_err(flow_error)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn adjust_option(&mut self, increase: bool) -> Result<(), JsValue> {
        let mut options = self.flow.options;
        match self.menu_index {
            0 => {
                options.sfx_volume = if increase {
                    options.sfx_volume.saturating_add(16)
                } else {
                    options.sfx_volume.saturating_sub(16)
                };
            }
            1 => {
                options.music_volume = if increase {
                    options.music_volume.saturating_add(16)
                } else {
                    options.music_volume.saturating_sub(16)
                };
            }
            2 => options.mono = !options.mono,
            _ => return Ok(()),
        }
        self.flow
            .command(FlowCommand::SetOptions(options))
            .map_err(flow_error)
    }

    fn handle_events(&mut self, dom: &Dom) -> Result<(), JsValue> {
        for event in self.flow.take_events() {
            dom.log(&format!("flow: {event:?}"), false);
            match &event {
                FlowEvent::OptionsChanged(options) => {
                    if let Some(audio) = &mut self.audio {
                        audio.set_output_options(output_options(*options));
                    }
                }
                FlowEvent::ProgressLoaded => {
                    if let Some(audio) = &mut self.audio {
                        audio.set_output_options(output_options(self.flow.options));
                    }
                }
                _ => {}
            }
            let asset_level = match &event {
                FlowEvent::LevelChanged(level)
                | FlowEvent::Booted(level)
                | FlowEvent::BonusReturned(level) => Some(*level),
                _ => None,
            };
            if let Some(level) = asset_level {
                self.queue_asset_level(level);
            }
            if matches!(event, FlowEvent::Completed(_)) {
                let operation = if self.card.current_slot().is_some() {
                    CardOperation::SaveCurrent
                } else {
                    CardOperation::SaveSelected
                };
                self.card
                    .control(operation, 0, Some(self.save_data()))
                    .map_err(|error| {
                        JsValue::from_str(&format!(
                            "could not update the virtual-card completion slot: {error:?}"
                        ))
                    })?;
                if let Some(storage) = &mut self.storage {
                    storage.persist_card(&self.card)?;
                }
            }
        }
        Ok(())
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

        match self.flow.state() {
            FlowState::Boot => {
                dom.set_overlay(true, "RUST / WASM", "Booting", "Validating streams");
            }
            FlowState::Title => self.render_title_ui(dom)?,
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
                    "Level complete",
                    &format!("Missed boxes: {missed_boxes} · press Z"),
                );
                dom.set_menu(&[])?;
            }
            FlowState::Intro => {
                dom.set_overlay(
                    true,
                    "ATTRACT SEQUENCE",
                    "Intro",
                    "Press a face button to return",
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

    fn render_title_ui(&self, dom: &Dom) -> Result<(), JsValue> {
        let screen = self.flow.title().screen();
        let subtitle = match self.flow.title().phase() {
            TitlePhase::Ready => "Ready",
            TitlePhase::FadingIn => "Fading in",
            TitlePhase::FadingOut | TitlePhase::FinishedFadingOut => "Transitioning",
            TitlePhase::Start | TitlePhase::Blank => "Loading title state",
        };
        let title = match screen {
            TitleScreen::PublisherFirst => "Publisher",
            TitleScreen::PublisherSecond => "Production",
            TitleScreen::NaughtyDog => "Developer",
            TitleScreen::MainMenu => "Main menu",
            TitleScreen::Options => "Options",
            TitleScreen::GameOver => "Game over",
            TitleScreen::Password => "Password",
            TitleScreen::Load => "Load game",
            TitleScreen::Map => "Island map",
        };
        dom.set_overlay(
            true,
            &format!("TITLE STATE {}", screen as u8),
            title,
            subtitle,
        );
        let entries: Vec<(String, bool)> = match screen {
            TitleScreen::MainMenu => ["Start", "Password", "Load", "Options"]
                .into_iter()
                .enumerate()
                .map(|(index, label)| (label.to_owned(), index == self.menu_index))
                .collect(),
            TitleScreen::Options => [
                format!("SFX volume {:03}", self.flow.options.sfx_volume),
                format!("Music volume {:03}", self.flow.options.music_volume),
                if self.flow.options.mono {
                    "Mono".to_owned()
                } else {
                    "Stereo".to_owned()
                },
                "Exit".to_owned(),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, label)| (label, index == self.menu_index))
            .collect(),
            TitleScreen::Password => self
                .password_digits
                .iter()
                .enumerate()
                .map(|(index, digit)| {
                    (
                        format!("Digit {}: {digit}", index + 1),
                        index == self.password_cursor,
                    )
                })
                .chain(std::iter::once((
                    "Start: apply · C: back".to_owned(),
                    false,
                )))
                .collect(),
            TitleScreen::Load => {
                let mut values: Vec<_> = (0..self.card.part_count())
                    .map(|index| (format!("Card save {}", index + 1), index == self.menu_index))
                    .collect();
                values.push(("Back".to_owned(), self.menu_index >= self.card.part_count()));
                values
            }
            TitleScreen::Map => self
                .available_levels
                .iter()
                .enumerate()
                .map(|(index, level)| (level_name(*level).to_owned(), index == self.menu_index))
                .collect(),
            TitleScreen::GameOver => vec![("Return to menu".to_owned(), true)],
            _ => vec![("Press Z, X, C, V, Enter, or Space".to_owned(), true)],
        };
        let borrowed: Vec<_> = entries
            .iter()
            .map(|(label, selected)| (label.as_str(), *selected))
            .collect();
        dom.set_menu(&borrowed)
    }

    fn request_pause(&mut self) {
        self.pending_buttons |= PAD_START;
    }

    fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        if let Some(audio) = &self.audio {
            audio.set_muted(muted);
        }
    }

    fn resume_audio(&self) {
        if let Some(audio) = &self.audio {
            audio.resume();
        }
    }

    fn save_data(&self) -> SaveData {
        SaveData {
            level_count: self.flow.progress.level_count,
            initial_lives: u32::try_from(self.flow.player.lives.max(0)).unwrap_or_default() << 8,
            unknown_6190c: 0,
            mono: self.flow.options.mono,
            sfx_volume: u32::from(self.flow.options.sfx_volume),
            music_volume: u32::from(self.flow.options.music_volume),
            item_pool_1: self.flow.progress.item_pool_1,
            item_pool_2: self.flow.progress.item_pool_2,
            gem_count: self.flow.progress.gem_count,
            key_count: self.flow.progress.key_count,
        }
    }

    fn flush(&mut self) {
        let save = self.save_data();
        if let Some(payload) = self.resume.flush(save)
            && let Some(storage) = &self.storage
        {
            let _ = storage.persist_resume(payload);
        }
        if let Some(storage) = &mut self.storage {
            let _ = storage.persist_card(&self.card);
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
        let screen = dom.screen.clone();
        bind_click(&dom.fullscreen, move || {
            let _ = screen.request_fullscreen();
        })?;
    }

    {
        let app = Rc::clone(app);
        let callback = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
            if let Some(bit) = keyboard_code(&event.code()) {
                event.prevent_default();
                let mut app = app.borrow_mut();
                app.keyboard_bits |= bit;
                if let Some(runtime) = &app.runtime {
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
            if let Some(bit) = keyboard_code(&event.code()) {
                event.prevent_default();
                app.borrow_mut().keyboard_bits &= !bit;
            }
        });
        dom.document
            .add_event_listener_with_callback("keyup", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }
    {
        let app = Rc::clone(app);
        let callback = Closure::<dyn FnMut(Event)>::new(move |_| {
            let mut app = app.borrow_mut();
            app.keyboard_bits = 0;
            app.active_touches.clear();
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
                if let Some(runtime) = &app.runtime {
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
    let (store, level) = {
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
        (app_ref.assets.clone(), level)
    };
    spawn_local(async move {
        match store.validate_pair(level).await {
            Ok(pair) => {
                let mut app = app.borrow_mut();
                if let Err(error) = app.start_runtime(pair) {
                    app.fail(&js_message(&error));
                }
            }
            Err(error) => app.borrow_mut().fail(&js_message(&error)),
        }
    });
}

type AnimationFrameCallback = Rc<RefCell<Option<Closure<dyn FnMut(f64)>>>>;

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
        tapped: u32::from(snapshot.tapped),
        held: u32::from(snapshot.held),
        held_previous: u32::from(snapshot.held_previous),
        tapped_previous: u32::from(snapshot.tapped_previous),
        held_previous_2: u32::from(snapshot.held_previous_2),
    }
}

fn update_debug(debug: &Object, runtime: &Runtime, assets: &AssetStore) -> Result<(), JsValue> {
    let level = current_level(&runtime.flow);
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
        &JsValue::from_f64(f64::from(runtime.flow.title().screen() as u8)),
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
        &JsValue::from_f64(runtime.retail_neighbors.len() as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailEntityDescriptors"),
        &JsValue::from_f64(retail_entity_count(&runtime.retail_neighbors) as f64),
    )?;
    Reflect::set(
        debug,
        &JsValue::from_str("retailLiveObjects"),
        &JsValue::from_f64(runtime.retail_objects.arena().len() as f64),
    )?;
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
    }
    Ok(())
}

fn current_level(flow: &GameFlow) -> LevelId {
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
        FlowState::Gameplay(_) | FlowState::Bonus(_) | FlowState::Boss(_) | FlowState::Ending
    )
}

fn level_name(level: LevelId) -> &'static str {
    KNOWN_LEVELS
        .iter()
        .find(|known| known.id.get() == u32::from(level.raw()))
        .map_or("Unknown level", |known| known.name)
}

const fn title_state_uses_image(screen: TitleScreen) -> bool {
    matches!(
        screen,
        TitleScreen::MainMenu
            | TitleScreen::PublisherSecond
            | TitleScreen::NaughtyDog
            | TitleScreen::PublisherFirst
    )
}

fn apply_save(flow: &mut GameFlow, save: SaveData) {
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
    flow.player.lives = i32::try_from(save.initial_lives >> 8).unwrap_or(4);
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

fn retail_entity_count(neighbors: &[OwnedNeighborZone]) -> usize {
    neighbors
        .iter()
        .map(|neighbor| neighbor.entities.len())
        .sum()
}

fn parse_retail_neighbors(pair: &ValidatedPair) -> Result<Vec<OwnedNeighborZone>, JsValue> {
    let ldat = pair
        .nsd
        .ldat()
        .ok_or_else(|| JsValue::from_str("index-only NSD has no retail spawn zone"))?;
    let (_, current_header) = parse_zone_entry(pair, ldat.spawn_zone, "current spawn ZDAT")?;
    let mut neighbors = Vec::with_capacity(current_header.neighbors.len());
    for eid in current_header.neighbors {
        let (entry, header) = parse_zone_entry(pair, eid, "spawn-neighbor ZDAT")?;
        let mut entities = Vec::with_capacity(header.entity_count as usize);
        for entity_index in 0..header.entity_count {
            let item_index = header.entity_item_index(entity_index).ok_or_else(|| {
                JsValue::from_str(&format!(
                    "spawn-neighbor ZDAT {eid} entity {entity_index} is outside its item range"
                ))
            })?;
            let item_index = usize::try_from(item_index).map_err(|_| {
                JsValue::from_str(&format!(
                    "spawn-neighbor ZDAT {eid} entity item does not fit this host"
                ))
            })?;
            let item = entry.item(item_index).ok_or_else(|| {
                JsValue::from_str(&format!(
                    "spawn-neighbor ZDAT {eid} entity item {item_index} is absent"
                ))
            })?;
            let bytes = item.bytes(&pair.nsf_bytes).map_err(|error| {
                JsValue::from_str(&format!(
                    "spawn-neighbor ZDAT {eid} entity item {item_index}: {error}"
                ))
            })?;
            entities.push(ZoneEntity::parse(bytes).map_err(|error| {
                JsValue::from_str(&format!(
                    "spawn-neighbor ZDAT {eid} entity item {item_index}: {error}"
                ))
            })?);
        }
        // The first retail LevelUpdate marks each current-zone neighbor
        // loaded and displayed immediately before LevelSpawnObjects scans it.
        neighbors.push(OwnedNeighborZone {
            eid,
            display_flags: header.display_flags | 3,
            entities,
        });
    }
    Ok(neighbors)
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

fn install_retail_scene_for_pair(
    pair: &ValidatedPair,
    builder: &mut RetailSceneBuilder,
    stage: &mut GlStage,
    dom: &Dom,
    after_loading_image: bool,
    point_count: NonZeroU16,
) -> Result<NonZeroU16, JsValue> {
    let draw_count = u32::from(after_loading_image);
    // Title and external-transition dummy zones legally have one-point paths.
    // The gameplay loading contract asks for point one/two, so clamp only that
    // presentation selection to the validated final point.
    let path_point = crate::initial_presented_path_point(point_count, after_loading_image);
    let scene = builder
        .build_at_path_point(
            &pair.nsd,
            &pair.nsf,
            &pair.nsf_bytes,
            path_point,
            draw_count,
        )
        .map_err(|error| {
            JsValue::from_str(&format!(
                "retail first-presented snapshot for {} is invalid: {error}",
                pair.level
            ))
        })?;
    let stats = scene.stats;
    debug_assert_eq!(scene.path_point_count, point_count.get());
    stage.install_retail_scene(scene)?;
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

fn hash_pair(pair: &ValidatedPair) -> u32 {
    pair.nsd_bytes
        .iter()
        .chain(pair.nsf_bytes.iter().step_by(4096))
        .fold(0x811c_9dc5_u32 ^ pair.level.get(), |hash, byte| {
            (hash ^ u32::from(*byte)).wrapping_mul(0x0100_0193)
        })
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

fn flow_error(error: crust_sim::flow::FlowError) -> JsValue {
    JsValue::from_str(&format!("flow command failed: {error:?}"))
}

fn js_message(error: &JsValue) -> String {
    error
        .as_string()
        .unwrap_or_else(|| "browser operation failed".to_owned())
}
