//! High-level title, menu, gameplay, bonus, boss, and ending flow.

use crate::camera::CameraState;
use crate::card::SaveData;
use crate::demo::{DemoPlayer, DemoStep};
use crate::player::{PAD_CROSS, PAD_START, PadState, PlayerMode, PlayerState};

pub const TITLE_IDLE_INTRO_FRAMES: u32 = 30 * 30;
pub const TITLE_FADE_START: i32 = 288;
pub const TITLE_FADE_STEP: i32 = 32;
pub const BONUS_RETURN_SENTINEL: i32 = -2;
const MAX_FLOW_EVENTS: usize = 256;

/// Numeric stream/level identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LevelId(u8);

impl LevelId {
    pub const CAVE: Self = Self(0x04);
    pub const TITLE: Self = Self(0x19);
    pub const LEVEL_COMPLETE: Self = Self(0x2d);
    pub const INTRO: Self = Self(0x38);
    pub const ENDING: Self = Self(0x39);

    #[must_use]
    pub const fn new(raw: u8) -> Option<Self> {
        if is_known_level(raw) {
            Some(Self(raw))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn raw(self) -> u8 {
        self.0
    }

    #[must_use]
    pub const fn is_playable(self) -> bool {
        self.0 != Self::CAVE.0
    }

    #[must_use]
    pub const fn kind(self) -> LevelKind {
        match self.0 {
            0x19 => LevelKind::Title,
            0x2d => LevelKind::Completion,
            0x38 => LevelKind::Intro,
            0x39 => LevelKind::Ending,
            0x24 | 0x25 | 0x26 | 0x33 | 0x34 => LevelKind::Bonus,
            0x08 | 0x0a | 0x17 | 0x1b | 0x1f | 0x21 => LevelKind::Boss,
            _ => LevelKind::Gameplay,
        }
    }
}

const fn is_known_level(raw: u8) -> bool {
    matches!(
        raw,
        0x03 | 0x04
            | 0x05
            | 0x06
            | 0x07
            | 0x08
            | 0x09
            | 0x0a
            | 0x0c
            | 0x0e
            | 0x0f
            | 0x11
            | 0x12
            | 0x13
            | 0x14
            | 0x15
            | 0x16
            | 0x17
            | 0x18
            | 0x19
            | 0x1a
            | 0x1b
            | 0x1c
            | 0x1d
            | 0x1e
            | 0x1f
            | 0x20
            | 0x21
            | 0x22
            | 0x23
            | 0x24
            | 0x25
            | 0x26
            | 0x28
            | 0x29
            | 0x2a
            | 0x2c
            | 0x2d
            | 0x2e
            | 0x33
            | 0x34
            | 0x37
            | 0x38
            | 0x39
    )
}

pub const KNOWN_LEVELS: [LevelId; 44] = [
    LevelId(0x03),
    LevelId(0x04),
    LevelId(0x05),
    LevelId(0x06),
    LevelId(0x07),
    LevelId(0x08),
    LevelId(0x09),
    LevelId(0x0a),
    LevelId(0x0c),
    LevelId(0x0e),
    LevelId(0x0f),
    LevelId(0x11),
    LevelId(0x12),
    LevelId(0x13),
    LevelId(0x14),
    LevelId(0x15),
    LevelId(0x16),
    LevelId(0x17),
    LevelId(0x18),
    LevelId(0x19),
    LevelId(0x1a),
    LevelId(0x1b),
    LevelId(0x1c),
    LevelId(0x1d),
    LevelId(0x1e),
    LevelId(0x1f),
    LevelId(0x20),
    LevelId(0x21),
    LevelId(0x22),
    LevelId(0x23),
    LevelId(0x24),
    LevelId(0x25),
    LevelId(0x26),
    LevelId(0x28),
    LevelId(0x29),
    LevelId(0x2a),
    LevelId(0x2c),
    LevelId(0x2d),
    LevelId(0x2e),
    LevelId(0x33),
    LevelId(0x34),
    LevelId(0x37),
    LevelId(0x38),
    LevelId(0x39),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LevelKind {
    Title,
    Gameplay,
    Bonus,
    Boss,
    Completion,
    Intro,
    Ending,
}

/// Stable title-state numbers used by data-authored GOOL.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum TitleScreen {
    MainMenu = 5,
    Options = 6,
    PublisherSecond = 7,
    NaughtyDog = 8,
    PublisherFirst = 10,
    GameOver = 12,
    Password = 13,
    Load = 14,
    Map = 15,
}

impl TitleScreen {
    /// Converts the checked 32-bit value stored in the retail GOOL global.
    ///
    /// The source engine represented this value as a signed `int`. Keeping the
    /// boundary as a full word means negative or malformed values cannot be
    /// truncated into a valid title state accidentally.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            5 => Some(Self::MainMenu),
            6 => Some(Self::Options),
            7 => Some(Self::PublisherSecond),
            8 => Some(Self::NaughtyDog),
            10 => Some(Self::PublisherFirst),
            12 => Some(Self::GameOver),
            13 => Some(Self::Password),
            14 => Some(Self::Load),
            15 => Some(Self::Map),
            _ => None,
        }
    }

    /// Returns the stable title-state value consumed and authored by GOOL.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self as u32
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum TitlePhase {
    Start = 0,
    Blank = 1,
    Ready = 3,
    FadingOut = 5,
    FadingIn = 6,
    FinishedFadingOut = 7,
}

/// Fade state machine corresponding to `TitleUpdate`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TitleMachine {
    screen: TitleScreen,
    next_screen: TitleScreen,
    phase: TitlePhase,
    fade_counter: i32,
}

impl TitleMachine {
    #[must_use]
    pub const fn first_boot() -> Self {
        Self {
            screen: TitleScreen::PublisherFirst,
            next_screen: TitleScreen::PublisherFirst,
            phase: TitlePhase::Start,
            fade_counter: TITLE_FADE_START,
        }
    }

    #[must_use]
    pub const fn resumed(screen: TitleScreen) -> Self {
        Self {
            screen,
            next_screen: screen,
            phase: TitlePhase::Start,
            fade_counter: TITLE_FADE_START,
        }
    }

    /// Creates a non-authoritative title mirror for a mounted retail runtime.
    ///
    /// The browser uses this only for high-level flow/progress reporting. The
    /// source-ordered fade, pending screen, and swap boundary live in
    /// `RetailRuntime`, so keeping this mirror ready prevents a future caller
    /// from accidentally advancing a second transition clock.
    #[must_use]
    pub const fn passive(screen: TitleScreen) -> Self {
        Self {
            screen,
            next_screen: screen,
            phase: TitlePhase::Ready,
            fade_counter: 0,
        }
    }

    #[must_use]
    pub const fn screen(self) -> TitleScreen {
        self.screen
    }

    /// Returns the target already latched for the current or next screen.
    ///
    /// This differs from [`Self::screen`] while a fade-out is pending and is
    /// the value corresponding to native `title->next_state`.
    #[must_use]
    pub const fn next_screen(self) -> TitleScreen {
        self.next_screen
    }

    #[must_use]
    pub const fn phase(self) -> TitlePhase {
        self.phase
    }

    #[must_use]
    pub const fn fade_counter(self) -> i32 {
        self.fade_counter
    }

    pub fn request(&mut self, screen: TitleScreen) {
        self.next_screen = screen;
        self.phase = TitlePhase::FadingOut;
        self.fade_counter = -256;
    }

    pub fn tick(&mut self) {
        match self.phase {
            TitlePhase::Start | TitlePhase::Blank => {
                self.phase = TitlePhase::FadingIn;
                self.fade_counter = TITLE_FADE_START;
            }
            TitlePhase::FadingIn => {
                self.fade_counter = (self.fade_counter - TITLE_FADE_STEP).max(0);
                if self.fade_counter == 0 {
                    self.phase = TitlePhase::Ready;
                }
            }
            TitlePhase::FadingOut => {
                self.fade_counter = (self.fade_counter + TITLE_FADE_STEP).min(0);
                if self.fade_counter == 0 {
                    self.phase = TitlePhase::FinishedFadingOut;
                }
            }
            TitlePhase::FinishedFadingOut => {
                self.screen = self.next_screen;
                self.phase = TitlePhase::Blank;
                self.fade_counter = TITLE_FADE_START;
            }
            TitlePhase::Ready => {}
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GameOptions {
    pub sfx_volume: u8,
    pub music_volume: u8,
    pub mono: bool,
}

impl Default for GameOptions {
    fn default() -> Self {
        Self {
            sfx_volume: u8::MAX,
            music_volume: u8::MAX,
            mono: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProgressState {
    pub level_count: u32,
    pub levels_unlocked: u32,
    pub current_map_level: u32,
    pub gem_count: u8,
    pub key_count: u32,
    pub item_pool_1: u32,
    pub item_pool_2: u32,
}

impl Default for ProgressState {
    fn default() -> Self {
        Self {
            level_count: 1,
            levels_unlocked: 1,
            current_map_level: 99,
            gem_count: 0,
            key_count: 0,
            item_pool_1: 0,
            item_pool_2: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LevelSnapshot {
    pub level: LevelId,
    pub player: PlayerState,
    pub path_index: u16,
    pub progress: u32,
    pub box_count: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FlowState {
    Boot,
    Title,
    Gameplay(LevelId),
    Bonus(LevelId),
    Boss(LevelId),
    LevelComplete { source: LevelId, missed_boxes: u16 },
    Intro,
    Ending,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FlowEvent {
    Booted(LevelId),
    TitleChanged(TitleScreen),
    LevelChanged(LevelId),
    PauseChanged(bool),
    OptionsChanged(GameOptions),
    ProgressLoaded,
    BonusReturned(LevelId),
    Completed(LevelId),
    DemoFinished,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MenuChoice {
    Start,
    Password,
    Load,
    Options,
    Back,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlowCommand {
    Boot(LevelId),
    Menu(MenuChoice),
    SetOptions(GameOptions),
    LoadProgress(SaveData),
    SelectMapLevel(LevelId),
    CompleteLevel { missed_boxes: u16 },
    AcknowledgeCompletion,
    EnterBonus(LevelId),
    ReturnFromBonus,
    DefeatBoss,
    GameOver,
    Continue,
    TriggerEnding,
    FinishEnding,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlowError {
    InvalidState,
    NotPlayable(LevelId),
    ExpectedBonus(LevelId),
    ExpectedBoss(LevelId),
    NoBonusSnapshot,
    EventQueueFull,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedTransition {
    pub level: LevelId,
    pub bonus_return: bool,
}

/// Preserves the originally requested target; only post-event `-2` overrides it.
pub fn resolve_level_transition(
    requested: LevelId,
    next_after_event: i32,
    saved: Option<LevelId>,
) -> Result<ResolvedTransition, FlowError> {
    if next_after_event == BONUS_RETURN_SENTINEL {
        Ok(ResolvedTransition {
            level: saved.ok_or(FlowError::NoBonusSnapshot)?,
            bonus_return: true,
        })
    } else {
        Ok(ResolvedTransition {
            level: requested,
            bonus_return: false,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GameFlow {
    state: FlowState,
    title: TitleMachine,
    title_idle_frames: u32,
    state_frames: u32,
    paused: bool,
    pub options: GameOptions,
    pub progress: ProgressState,
    pub player: PlayerState,
    pub camera: CameraState,
    saved_level: Option<LevelSnapshot>,
    demo: Option<DemoPlayer>,
    events: Vec<FlowEvent>,
}

impl GameFlow {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: FlowState::Boot,
            title: TitleMachine::first_boot(),
            title_idle_frames: 0,
            state_frames: 0,
            paused: false,
            options: GameOptions::default(),
            progress: ProgressState::default(),
            player: PlayerState::default(),
            camera: CameraState::default(),
            saved_level: None,
            demo: None,
            events: Vec::new(),
        }
    }

    #[must_use]
    pub fn state(&self) -> &FlowState {
        &self.state
    }

    #[must_use]
    pub const fn title(&self) -> TitleMachine {
        self.title
    }

    #[must_use]
    pub const fn paused(&self) -> bool {
        self.paused
    }

    #[must_use]
    pub fn events(&self) -> &[FlowEvent] {
        &self.events
    }

    #[must_use]
    pub fn take_events(&mut self) -> Vec<FlowEvent> {
        core::mem::take(&mut self.events)
    }

    pub fn set_demo(&mut self, demo: Option<DemoPlayer>) {
        self.demo = demo;
    }

    /// Mirrors a screen that the mounted retail title runtime already loaded.
    ///
    /// This deliberately emits no [`FlowEvent::TitleChanged`]: native title
    /// teardown/load has already happened at the source `TitleUpdate`
    /// boundary, so an event-driven browser load would execute it twice.
    pub fn mirror_retail_title_screen(&mut self, screen: TitleScreen) -> Result<bool, FlowError> {
        if !matches!(self.state, FlowState::Title) {
            return Err(FlowError::InvalidState);
        }
        let changed = self.title.screen() != screen;
        self.title = TitleMachine::passive(screen);
        if changed {
            self.state_frames = 0;
            self.title_idle_frames = 0;
        }
        Ok(changed)
    }

    /// Mirrors a stream that has already passed platform validation and been
    /// committed by the retail runtime.
    ///
    /// Unlike [`FlowCommand`], this does not emit a level-change request: the
    /// pair is already mounted, so feeding another request into the browser
    /// asset loader would form a transition loop. Retail GOOL globals remain
    /// authoritative for progression and the caller supplies the title state
    /// selected by the process-lifetime `TitleLoadNextState` contract.
    pub fn mount_retail_level(
        &mut self,
        level: LevelId,
        title_screen: Option<TitleScreen>,
    ) -> Result<(), FlowError> {
        if !level.is_playable() {
            return Err(FlowError::NotPlayable(level));
        }
        let completion_source = match self.state {
            FlowState::Gameplay(source)
            | FlowState::Boss(source)
            | FlowState::LevelComplete { source, .. } => source,
            _ => level,
        };
        self.state = match level.kind() {
            LevelKind::Title => {
                self.title = TitleMachine::resumed(title_screen.unwrap_or(TitleScreen::MainMenu));
                self.title_idle_frames = 0;
                FlowState::Title
            }
            LevelKind::Gameplay => FlowState::Gameplay(level),
            LevelKind::Bonus => FlowState::Bonus(level),
            LevelKind::Boss => FlowState::Boss(level),
            LevelKind::Completion => FlowState::LevelComplete {
                source: completion_source,
                missed_boxes: 0,
            },
            LevelKind::Intro => FlowState::Intro,
            LevelKind::Ending => FlowState::Ending,
        };
        if matches!(
            level.kind(),
            LevelKind::Gameplay | LevelKind::Bonus | LevelKind::Boss
        ) {
            self.player.mode = PlayerMode::Cutscene;
        }
        self.state_frames = 0;
        self.paused = false;
        Ok(())
    }

    pub fn command(&mut self, command: FlowCommand) -> Result<(), FlowError> {
        match command {
            FlowCommand::Boot(level) => self.boot(level),
            FlowCommand::Menu(choice) => self.menu(choice),
            FlowCommand::SetOptions(options) => {
                if !matches!(self.state, FlowState::Title)
                    || self.title.screen() != TitleScreen::Options
                {
                    return Err(FlowError::InvalidState);
                }
                self.options = options;
                self.emit(FlowEvent::OptionsChanged(options))
            }
            FlowCommand::LoadProgress(save) => {
                if !matches!(self.state, FlowState::Title)
                    || self.title.screen() != TitleScreen::Load
                {
                    return Err(FlowError::InvalidState);
                }
                self.apply_save(save);
                self.title.request(TitleScreen::Map);
                self.emit(FlowEvent::ProgressLoaded)
            }
            FlowCommand::SelectMapLevel(level) => {
                if !matches!(self.state, FlowState::Title)
                    || self.title.screen() != TitleScreen::Map
                {
                    return Err(FlowError::InvalidState);
                }
                self.enter_level(level)
            }
            FlowCommand::CompleteLevel { missed_boxes } => {
                let (FlowState::Gameplay(source) | FlowState::Boss(source)) = self.state else {
                    return Err(FlowError::InvalidState);
                };
                self.state = FlowState::LevelComplete {
                    source,
                    missed_boxes,
                };
                self.state_frames = 0;
                self.paused = false;
                self.emit(FlowEvent::Completed(source))?;
                self.emit(FlowEvent::LevelChanged(LevelId::LEVEL_COMPLETE))
            }
            FlowCommand::AcknowledgeCompletion => {
                let FlowState::LevelComplete { source, .. } = self.state else {
                    return Err(FlowError::InvalidState);
                };
                self.progress.level_count = self.progress.level_count.saturating_add(1);
                self.progress.levels_unlocked =
                    self.progress.levels_unlocked.max(self.progress.level_count);
                self.progress.current_map_level = self.progress.level_count;
                self.enter_title(TitleScreen::Map)?;
                self.emit(FlowEvent::Completed(source))
            }
            FlowCommand::EnterBonus(level) => {
                if level.kind() != LevelKind::Bonus {
                    return Err(FlowError::ExpectedBonus(level));
                }
                let FlowState::Gameplay(source) = self.state else {
                    return Err(FlowError::InvalidState);
                };
                self.saved_level = Some(LevelSnapshot {
                    level: source,
                    player: self.player.clone(),
                    path_index: 0,
                    progress: 0,
                    box_count: self.player.boxes,
                });
                self.state = FlowState::Bonus(level);
                self.state_frames = 0;
                self.emit(FlowEvent::LevelChanged(level))
            }
            FlowCommand::ReturnFromBonus => self.return_from_bonus(),
            FlowCommand::DefeatBoss => {
                let FlowState::Boss(level) = self.state else {
                    return Err(FlowError::InvalidState);
                };
                self.progress.level_count = self.progress.level_count.saturating_add(1);
                self.progress.levels_unlocked =
                    self.progress.levels_unlocked.max(self.progress.level_count);
                self.enter_title(TitleScreen::Map)?;
                self.emit(FlowEvent::Completed(level))
            }
            FlowCommand::GameOver => self.enter_title(TitleScreen::GameOver),
            FlowCommand::Continue => {
                let snapshot = self.saved_level.clone().ok_or(FlowError::InvalidState)?;
                self.player = snapshot.player;
                self.state = FlowState::Gameplay(snapshot.level);
                self.state_frames = 0;
                self.emit(FlowEvent::LevelChanged(snapshot.level))
            }
            FlowCommand::TriggerEnding => self.boot(LevelId::ENDING),
            FlowCommand::FinishEnding => {
                if !matches!(self.state, FlowState::Ending) {
                    return Err(FlowError::InvalidState);
                }
                self.enter_title(TitleScreen::MainMenu)
            }
        }
    }

    /// Advances one exact 30 Hz game frame.
    pub fn tick(&mut self, mut pad: PadState) -> Result<(), FlowError> {
        self.state_frames = self.state_frames.wrapping_add(1);
        let mut demo_ended = false;
        if let Some(demo) = &mut self.demo {
            match demo.advance(pad.held) {
                DemoStep::Playing {
                    held, tapped, end, ..
                } => {
                    pad = PadState { held, tapped };
                    demo_ended = end.is_some();
                }
                DemoStep::Finished => demo_ended = true,
            }
        }

        let result = match self.state.clone() {
            FlowState::Title => self.tick_title(pad),
            FlowState::Intro => {
                if pad.held & 0x09f0 != 0 || self.state_frames >= 60 * 30 {
                    self.enter_title(TitleScreen::MainMenu)?;
                }
                Ok(())
            }
            FlowState::LevelComplete { .. } => {
                if pad.tapped & PAD_CROSS != 0 {
                    self.command(FlowCommand::AcknowledgeCompletion)?;
                }
                Ok(())
            }
            FlowState::Gameplay(_)
            | FlowState::Bonus(_)
            | FlowState::Boss(_)
            | FlowState::Ending => {
                if pad.tapped & PAD_START != 0 && self.demo.is_none() {
                    self.paused = !self.paused;
                    self.emit(FlowEvent::PauseChanged(self.paused))?;
                }
                if !self.paused {
                    self.player.tick(pad, |position, delta| position + delta);
                    self.camera.follow(self.player.translation, 16_000);
                }
                Ok(())
            }
            FlowState::Boot => Ok(()),
        };
        if demo_ended {
            self.demo = None;
            self.emit(FlowEvent::DemoFinished)?;
        }
        result
    }

    fn boot(&mut self, level: LevelId) -> Result<(), FlowError> {
        if !level.is_playable() {
            return Err(FlowError::NotPlayable(level));
        }
        self.paused = false;
        self.state_frames = 0;
        match level.kind() {
            LevelKind::Title => {
                self.title = TitleMachine::first_boot();
                self.state = FlowState::Title;
            }
            LevelKind::Gameplay => self.state = FlowState::Gameplay(level),
            LevelKind::Bonus => self.state = FlowState::Bonus(level),
            LevelKind::Boss => self.state = FlowState::Boss(level),
            LevelKind::Completion => {
                self.state = FlowState::LevelComplete {
                    source: level,
                    missed_boxes: 0,
                };
            }
            LevelKind::Intro => self.state = FlowState::Intro,
            LevelKind::Ending => self.state = FlowState::Ending,
        }
        if matches!(
            level.kind(),
            LevelKind::Gameplay | LevelKind::Bonus | LevelKind::Boss
        ) {
            self.player.mode = PlayerMode::Cutscene;
        }
        self.emit(FlowEvent::Booted(level))
    }

    fn menu(&mut self, choice: MenuChoice) -> Result<(), FlowError> {
        if !matches!(self.state, FlowState::Title) || self.title.phase() != TitlePhase::Ready {
            return Err(FlowError::InvalidState);
        }
        let target = match (self.title.screen(), choice) {
            (TitleScreen::MainMenu, MenuChoice::Start) => TitleScreen::Map,
            (TitleScreen::MainMenu, MenuChoice::Password) => TitleScreen::Password,
            (TitleScreen::MainMenu, MenuChoice::Load) => TitleScreen::Load,
            (TitleScreen::MainMenu, MenuChoice::Options) => TitleScreen::Options,
            (
                TitleScreen::Options
                | TitleScreen::Password
                | TitleScreen::Load
                | TitleScreen::GameOver,
                MenuChoice::Back,
            ) => TitleScreen::MainMenu,
            _ => return Err(FlowError::InvalidState),
        };
        self.title.request(target);
        self.title_idle_frames = 0;
        Ok(())
    }

    fn tick_title(&mut self, pad: PadState) -> Result<(), FlowError> {
        self.advance_title_transition()?;
        if self.title.phase() != TitlePhase::Ready {
            return Ok(());
        }
        match self.title.screen() {
            TitleScreen::PublisherFirst if pad.held & 0x09f0 != 0 || self.state_frames >= 90 => {
                self.title.request(TitleScreen::PublisherSecond);
            }
            TitleScreen::PublisherSecond if pad.held & 0x09f0 != 0 || self.state_frames >= 90 => {
                self.title.request(TitleScreen::NaughtyDog);
            }
            TitleScreen::NaughtyDog if pad.held & 0x09f0 != 0 || self.state_frames >= 90 => {
                self.title.request(TitleScreen::MainMenu);
            }
            TitleScreen::MainMenu => {
                if pad.tapped & PAD_START != 0 {
                    self.menu(MenuChoice::Start)?;
                } else {
                    self.title_idle_frames = self.title_idle_frames.saturating_add(1);
                    if self.title_idle_frames >= TITLE_IDLE_INTRO_FRAMES {
                        self.state = FlowState::Intro;
                        self.state_frames = 0;
                        self.emit(FlowEvent::LevelChanged(LevelId::INTRO))?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn advance_title_transition(&mut self) -> Result<(), FlowError> {
        let previous = self.title.screen();
        self.title.tick();
        if self.title.screen() != previous {
            self.state_frames = 0;
            self.title_idle_frames = 0;
            self.emit(FlowEvent::TitleChanged(self.title.screen()))?;
        }
        Ok(())
    }

    fn enter_title(&mut self, screen: TitleScreen) -> Result<(), FlowError> {
        self.state = FlowState::Title;
        self.title = TitleMachine::resumed(screen);
        self.title_idle_frames = 0;
        self.state_frames = 0;
        self.paused = false;
        self.emit(FlowEvent::LevelChanged(LevelId::TITLE))
    }

    fn enter_level(&mut self, level: LevelId) -> Result<(), FlowError> {
        if !level.is_playable() {
            return Err(FlowError::NotPlayable(level));
        }
        self.state = match level.kind() {
            LevelKind::Gameplay => FlowState::Gameplay(level),
            LevelKind::Boss => FlowState::Boss(level),
            LevelKind::Bonus => FlowState::Bonus(level),
            LevelKind::Intro => FlowState::Intro,
            LevelKind::Ending => FlowState::Ending,
            LevelKind::Title => FlowState::Title,
            LevelKind::Completion => FlowState::LevelComplete {
                source: level,
                missed_boxes: 0,
            },
        };
        self.state_frames = 0;
        self.paused = false;
        self.player.mode = PlayerMode::Cutscene;
        self.emit(FlowEvent::LevelChanged(level))
    }

    fn return_from_bonus(&mut self) -> Result<(), FlowError> {
        if !matches!(self.state, FlowState::Bonus(_)) {
            return Err(FlowError::InvalidState);
        }
        let snapshot = self.saved_level.take().ok_or(FlowError::NoBonusSnapshot)?;
        let resolved =
            resolve_level_transition(LevelId::TITLE, BONUS_RETURN_SENTINEL, Some(snapshot.level))?;
        self.player = snapshot.player;
        self.state = FlowState::Gameplay(resolved.level);
        self.state_frames = 0;
        self.emit(FlowEvent::BonusReturned(resolved.level))?;
        self.emit(FlowEvent::LevelChanged(resolved.level))
    }

    fn apply_save(&mut self, save: SaveData) {
        self.progress.level_count = save.level_count;
        self.progress.levels_unlocked = save.level_count;
        self.progress.current_map_level = save.level_count;
        self.progress.gem_count = save.gem_count;
        self.progress.key_count = save.key_count;
        self.progress.item_pool_1 = save.item_pool_1;
        self.progress.item_pool_2 = save.item_pool_2;
        self.options = GameOptions {
            sfx_volume: save.sfx_volume.min(u32::from(u8::MAX)) as u8,
            music_volume: save.music_volume.min(u32::from(u8::MAX)) as u8,
            mono: save.mono,
        };
    }

    fn emit(&mut self, event: FlowEvent) -> Result<(), FlowError> {
        if self.events.len() == MAX_FLOW_EVENTS {
            return Err(FlowError::EventQueueFull);
        }
        self.events.push(event);
        Ok(())
    }
}

impl Default for GameFlow {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::PAD_SQUARE;

    fn tick_until_ready(flow: &mut GameFlow) {
        for _ in 0..32 {
            flow.tick(PadState::default()).unwrap();
            if flow.title().phase() == TitlePhase::Ready {
                break;
            }
        }
        assert_eq!(flow.title().phase(), TitlePhase::Ready);
    }

    fn skip_title_card(flow: &mut GameFlow) {
        tick_until_ready(flow);
        flow.tick(PadState {
            held: PAD_CROSS,
            tapped: PAD_CROSS,
        })
        .unwrap();
        for _ in 0..32 {
            flow.tick(PadState::default()).unwrap();
            if flow.title().phase() == TitlePhase::Ready {
                break;
            }
        }
    }

    fn ready_main_menu() -> GameFlow {
        let mut flow = GameFlow::new();
        flow.state = FlowState::Title;
        flow.title = TitleMachine::resumed(TitleScreen::MainMenu);
        tick_until_ready(&mut flow);
        let _ = flow.take_events();
        flow
    }

    #[test]
    fn catalog_has_44_pairs_and_only_cave_is_not_playable() {
        assert_eq!(KNOWN_LEVELS.len(), 44);
        assert_eq!(
            KNOWN_LEVELS
                .iter()
                .filter(|level| level.is_playable())
                .count(),
            43
        );
        assert!(!LevelId::CAVE.is_playable());
    }

    #[test]
    fn title_screen_raw_conversion_rejects_unrecognized_full_words() {
        for screen in [
            TitleScreen::MainMenu,
            TitleScreen::Options,
            TitleScreen::PublisherSecond,
            TitleScreen::NaughtyDog,
            TitleScreen::PublisherFirst,
            TitleScreen::GameOver,
            TitleScreen::Password,
            TitleScreen::Load,
            TitleScreen::Map,
        ] {
            assert_eq!(TitleScreen::from_raw(screen.raw()), Some(screen));
        }
        for raw in [0, 4, 9, 11, 16, 0x105, u32::MAX] {
            assert_eq!(TitleScreen::from_raw(raw), None);
        }
    }

    #[test]
    fn retail_title_mirror_is_passive_and_emits_no_duplicate_load_event() {
        let mut flow = ready_main_menu();
        assert_eq!(
            flow.mirror_retail_title_screen(TitleScreen::Options),
            Ok(true)
        );
        assert_eq!(flow.title().screen(), TitleScreen::Options);
        assert_eq!(flow.title().next_screen(), TitleScreen::Options);
        assert_eq!(flow.title().phase(), TitlePhase::Ready);
        assert_eq!(flow.title().fade_counter(), 0);
        assert!(flow.events().is_empty());
        assert_eq!(
            flow.mirror_retail_title_screen(TitleScreen::Options),
            Ok(false)
        );
        assert!(flow.events().is_empty());
    }

    #[test]
    fn committed_retail_mount_mirrors_state_without_requeueing_assets() {
        let mut flow = GameFlow::new();
        let beach = LevelId::new(0x09).unwrap();
        flow.command(FlowCommand::Boot(beach)).unwrap();
        let _ = flow.take_events();

        flow.mount_retail_level(LevelId::LEVEL_COMPLETE, None)
            .unwrap();
        assert_eq!(
            flow.state(),
            &FlowState::LevelComplete {
                source: beach,
                missed_boxes: 0,
            }
        );
        assert!(flow.events().is_empty());

        flow.mount_retail_level(LevelId::TITLE, Some(TitleScreen::Load))
            .unwrap();
        assert_eq!(flow.state(), &FlowState::Title);
        assert_eq!(flow.title().screen(), TitleScreen::Load);
        assert_eq!(flow.title().next_screen(), TitleScreen::Load);
        assert!(flow.events().is_empty());
    }

    #[test]
    fn fresh_title_route_reaches_main_menu_and_start_goes_to_map() {
        let mut flow = GameFlow::new();
        flow.command(FlowCommand::Boot(LevelId::TITLE)).unwrap();
        assert_eq!(flow.title().screen(), TitleScreen::PublisherFirst);
        skip_title_card(&mut flow);
        assert_eq!(flow.title().screen(), TitleScreen::PublisherSecond);
        skip_title_card(&mut flow);
        assert_eq!(flow.title().screen(), TitleScreen::NaughtyDog);
        skip_title_card(&mut flow);
        assert_eq!(flow.title().screen(), TitleScreen::MainMenu);
        flow.tick(PadState {
            held: PAD_START,
            tapped: PAD_START,
        })
        .unwrap();
        for _ in 0..32 {
            flow.tick(PadState::default()).unwrap();
        }
        assert_eq!(flow.title().screen(), TitleScreen::Map);
    }

    #[test]
    fn map_selection_emits_destination_without_advancing_its_player() {
        let beach = LevelId::new(0x09).unwrap();
        let mut flow = GameFlow::new();
        flow.state = FlowState::Title;
        flow.title = TitleMachine::resumed(TitleScreen::Map);
        tick_until_ready(&mut flow);
        let player_before = flow.player.clone();

        flow.command(FlowCommand::SelectMapLevel(beach)).unwrap();

        assert_eq!(flow.state(), &FlowState::Gameplay(beach));
        assert_eq!(flow.player, player_before);
        assert_eq!(flow.take_events(), vec![FlowEvent::LevelChanged(beach)]);
    }

    #[test]
    fn idle_menu_enters_intro_and_input_returns_to_title() {
        let mut flow = GameFlow::new();
        flow.state = FlowState::Title;
        flow.title = TitleMachine::resumed(TitleScreen::MainMenu);
        tick_until_ready(&mut flow);
        for _ in 0..TITLE_IDLE_INTRO_FRAMES {
            flow.tick(PadState::default()).unwrap();
        }
        assert_eq!(flow.state(), &FlowState::Intro);
        flow.tick(PadState {
            held: PAD_CROSS,
            tapped: PAD_CROSS,
        })
        .unwrap();
        assert_eq!(flow.state(), &FlowState::Title);
        assert_eq!(flow.title().screen(), TitleScreen::MainMenu);
    }

    #[test]
    fn beach_completion_and_bonus_return_preserve_source_level() {
        let beach = LevelId::new(0x09).unwrap();
        let bonus = LevelId::new(0x26).unwrap();
        let mut flow = GameFlow::new();
        flow.command(FlowCommand::Boot(beach)).unwrap();
        flow.player.mode = PlayerMode::Grounded;
        flow.player.boxes = 12;
        flow.command(FlowCommand::EnterBonus(bonus)).unwrap();
        assert_eq!(flow.state(), &FlowState::Bonus(bonus));
        flow.player.boxes = 0;
        flow.command(FlowCommand::ReturnFromBonus).unwrap();
        assert_eq!(flow.state(), &FlowState::Gameplay(beach));
        assert_eq!(flow.player.boxes, 12);
        flow.command(FlowCommand::CompleteLevel { missed_boxes: 3 })
            .unwrap();
        assert_eq!(
            flow.state(),
            &FlowState::LevelComplete {
                source: beach,
                missed_boxes: 3
            }
        );
    }

    #[test]
    fn every_cross_stream_flow_emits_the_destination_level() {
        let beach = LevelId::new(0x09).unwrap();
        let bonus = LevelId::new(0x26).unwrap();
        let mut flow = GameFlow::new();
        flow.command(FlowCommand::Boot(beach)).unwrap();
        let _ = flow.take_events();

        flow.command(FlowCommand::CompleteLevel { missed_boxes: 2 })
            .unwrap();
        assert_eq!(
            flow.take_events(),
            vec![
                FlowEvent::Completed(beach),
                FlowEvent::LevelChanged(LevelId::LEVEL_COMPLETE),
            ]
        );
        flow.command(FlowCommand::AcknowledgeCompletion).unwrap();
        assert_eq!(
            flow.take_events(),
            vec![
                FlowEvent::LevelChanged(LevelId::TITLE),
                FlowEvent::Completed(beach),
            ]
        );

        flow.command(FlowCommand::Boot(beach)).unwrap();
        let _ = flow.take_events();
        flow.command(FlowCommand::EnterBonus(bonus)).unwrap();
        let _ = flow.take_events();
        flow.command(FlowCommand::ReturnFromBonus).unwrap();
        assert_eq!(
            flow.take_events(),
            vec![
                FlowEvent::BonusReturned(beach),
                FlowEvent::LevelChanged(beach),
            ]
        );
    }

    #[test]
    fn requested_transition_wins_unless_event_sets_minus_two() {
        let beach = LevelId::new(0x09).unwrap();
        assert_eq!(
            resolve_level_transition(
                LevelId::LEVEL_COMPLETE,
                LevelId::TITLE.raw().into(),
                Some(beach)
            ),
            Ok(ResolvedTransition {
                level: LevelId::LEVEL_COMPLETE,
                bonus_return: false
            })
        );
        assert_eq!(
            resolve_level_transition(LevelId::LEVEL_COMPLETE, -2, Some(beach)),
            Ok(ResolvedTransition {
                level: beach,
                bonus_return: true
            })
        );
    }

    #[test]
    fn gameplay_pause_freezes_player_but_options_are_independent() {
        let beach = LevelId::new(0x09).unwrap();
        let mut flow = GameFlow::new();
        flow.command(FlowCommand::Boot(beach)).unwrap();
        flow.player.mode = PlayerMode::Grounded;
        flow.tick(PadState {
            held: PAD_START,
            tapped: PAD_START,
        })
        .unwrap();
        let before = flow.player.translation;
        flow.tick(PadState {
            held: PAD_SQUARE,
            tapped: PAD_SQUARE,
        })
        .unwrap();
        assert_eq!(flow.player.translation, before);
    }
}
