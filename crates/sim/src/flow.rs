//! Passive high-level mirror of the mounted retail-authored runtime.
//!
//! This module deliberately owns no simulation clock, player movement, camera
//! follow, menu progression, or level-transition policy. Those behaviors are
//! driven by [`crate::retail_runtime::RetailRuntime`] and mirrored here only
//! after the corresponding retail stream has been committed.

pub const TITLE_FADE_START: i32 = 288;
pub const TITLE_FADE_STEP: i32 = 32;
const MAX_RETAIL_FLOW_EVENTS: usize = 256;

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

/// The sole request emitted by the passive mirror.
///
/// Subsequent pair changes are sourced from retail GOOL and mounted directly;
/// they must not be re-emitted from this mirror.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetailFlowEvent {
    Booted(LevelId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlowError {
    InvalidState,
    NotPlayable(LevelId),
    EventQueueFull,
}

/// Passive reporting state for the currently mounted retail stream.
///
/// `RetailFlowMirror` cannot tick. The browser may boot its first validated
/// pair, mirror a committed pair, or mirror an authored title screen; all
/// gameplay, title timing, and transition decisions remain in retail GOOL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetailFlowMirror {
    state: FlowState,
    title_screen: TitleScreen,
    pub options: GameOptions,
    pub progress: ProgressState,
    events: Vec<RetailFlowEvent>,
}

impl RetailFlowMirror {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: FlowState::Boot,
            title_screen: TitleScreen::PublisherFirst,
            options: GameOptions::default(),
            progress: ProgressState::default(),
            events: Vec::new(),
        }
    }

    #[must_use]
    pub fn state(&self) -> &FlowState {
        &self.state
    }

    #[must_use]
    pub const fn title_screen(&self) -> TitleScreen {
        self.title_screen
    }

    #[must_use]
    pub fn events(&self) -> &[RetailFlowEvent] {
        &self.events
    }

    #[must_use]
    pub fn take_events(&mut self) -> Vec<RetailFlowEvent> {
        core::mem::take(&mut self.events)
    }

    /// Records the first already-validated pair and emits its one asset event.
    pub fn boot(&mut self, level: LevelId) -> Result<(), FlowError> {
        if !level.is_playable() {
            return Err(FlowError::NotPlayable(level));
        }
        self.state = state_for_level(level, level);
        if level.kind() == LevelKind::Title {
            self.title_screen = TitleScreen::PublisherFirst;
        }
        self.emit(RetailFlowEvent::Booted(level))
    }

    /// Mirrors a screen that the mounted retail title runtime already loaded.
    ///
    /// This emits no event and owns no fade state: native title teardown/load
    /// has already happened at the source `TitleUpdate` boundary.
    pub fn mirror_retail_title_screen(&mut self, screen: TitleScreen) -> Result<bool, FlowError> {
        if !matches!(self.state, FlowState::Title) {
            return Err(FlowError::InvalidState);
        }
        let changed = self.title_screen != screen;
        self.title_screen = screen;
        Ok(changed)
    }

    /// Mirrors a stream committed by the authoritative retail runtime.
    ///
    /// This deliberately emits no level-change request: the pair is already
    /// mounted, so re-emitting it would form an asset-transition loop.
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
        self.state = state_for_level(level, completion_source);
        if level.kind() == LevelKind::Title {
            self.title_screen = title_screen.unwrap_or(TitleScreen::MainMenu);
        }
        Ok(())
    }

    fn emit(&mut self, event: RetailFlowEvent) -> Result<(), FlowError> {
        if self.events.len() == MAX_RETAIL_FLOW_EVENTS {
            return Err(FlowError::EventQueueFull);
        }
        self.events.push(event);
        Ok(())
    }
}

impl Default for RetailFlowMirror {
    fn default() -> Self {
        Self::new()
    }
}

fn state_for_level(level: LevelId, completion_source: LevelId) -> FlowState {
    match level.kind() {
        LevelKind::Title => FlowState::Title,
        LevelKind::Gameplay => FlowState::Gameplay(level),
        LevelKind::Bonus => FlowState::Bonus(level),
        LevelKind::Boss => FlowState::Boss(level),
        LevelKind::Completion => FlowState::LevelComplete {
            source: completion_source,
            missed_boxes: 0,
        },
        LevelKind::Intro => FlowState::Intro,
        LevelKind::Ending => FlowState::Ending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn boot_reports_only_the_validated_initial_pair() {
        let beach = LevelId::new(0x09).unwrap();
        let mut flow = RetailFlowMirror::new();

        flow.boot(beach).unwrap();

        assert_eq!(flow.state(), &FlowState::Gameplay(beach));
        assert_eq!(flow.take_events(), vec![RetailFlowEvent::Booted(beach)]);
        assert!(flow.events().is_empty());
    }

    #[test]
    fn boot_rejects_the_non_playable_cave_pair_without_mutation() {
        let mut flow = RetailFlowMirror::new();

        assert_eq!(
            flow.boot(LevelId::CAVE),
            Err(FlowError::NotPlayable(LevelId::CAVE))
        );
        assert_eq!(flow.state(), &FlowState::Boot);
        assert!(flow.events().is_empty());
    }

    #[test]
    fn committed_mount_is_passive_and_preserves_completion_source() {
        let beach = LevelId::new(0x09).unwrap();
        let mut flow = RetailFlowMirror::new();
        flow.boot(beach).unwrap();
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
        assert_eq!(flow.title_screen(), TitleScreen::Load);
        assert!(flow.events().is_empty());
    }

    #[test]
    fn authored_title_screen_is_mirrored_without_a_second_clock_or_event() {
        let mut flow = RetailFlowMirror::new();
        flow.boot(LevelId::TITLE).unwrap();
        let _ = flow.take_events();

        assert_eq!(flow.title_screen(), TitleScreen::PublisherFirst);
        assert_eq!(
            flow.mirror_retail_title_screen(TitleScreen::Options),
            Ok(true)
        );
        assert_eq!(flow.title_screen(), TitleScreen::Options);
        assert_eq!(
            flow.mirror_retail_title_screen(TitleScreen::Options),
            Ok(false)
        );
        assert!(flow.events().is_empty());
    }

    #[test]
    fn title_screen_cannot_be_mirrored_outside_the_title_stream() {
        let beach = LevelId::new(0x09).unwrap();
        let mut flow = RetailFlowMirror::new();
        flow.boot(beach).unwrap();

        assert_eq!(
            flow.mirror_retail_title_screen(TitleScreen::MainMenu),
            Err(FlowError::InvalidState)
        );
    }
}
