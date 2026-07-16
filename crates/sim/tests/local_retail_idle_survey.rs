//! Opt-in, legally local long-window survey of every retail stream pair.
//!
//! No game bytes or derived assets are written by these tests. The runtime
//! survey mirrors the browser's spawn -> camera -> GOOL order for a bounded
//! window and prints deterministic diagnostics instead of stopping at the
//! first level. The separate N. Sanity progression test drives an observable
//! camera/player-state route using only retail directional, jump, and spin pad
//! input for a default 18,000-frame window selected by `C1_PROGRESSION_FRAMES`.
//! A separate vertical-flow test retains the authored session carry across
//! N. Sanity Beach, Jungle Rollers, both Level Complete screens, the Title map,
//! and The Great Gate's normal main route through its end `WarpC` transition
//! without writing any game data.
//! Set `C1_SURVEY_REQUIRE_CLEAN=1` to turn a characterized runtime boundary into
//! a failing assertion. Set `C1_SURVEY_LEVEL` to a
//! hexadecimal retail level ID (for example `05` or `0x05`) to reproduce only
//! one level's trace. `C1_SURVEY_FRAMES` selects a bounded 1..=108,000 frame
//! window; the default remains 360 frames.

#![allow(clippy::too_many_arguments, clippy::too_many_lines)]

use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU16,
    path::{Path, PathBuf},
};

use crust_formats::{
    binary::{Eid, PageIndex},
    disc::{DiscImage, SectorLayout},
    stream::{
        KNOWN_LEVELS, LevelId, Nsd, Nsf, PBAK_ENTRY_TYPE, RetailPathId, RetailZoneGraph,
        StreamKind, StreamName, ZoneEntity, ZoneHeader, load_gool_state_program, load_pbak_entry,
        parse_nsd, parse_nsf,
    },
};
use crust_sim::{
    camera::{
        RetailCameraEffect, RetailCameraFollowInput, RetailCameraInput, RetailCameraLocation,
        RetailCameraOutcome, RetailCameraPose, RetailCameraRuntime, RetailDeathCameraInput,
        RetailDeathCameraState, RetailIslandCameraInput,
    },
    card::{CardPayload, SaveData, VirtualCard},
    flow::{TitlePhase, TitleScreen},
    gool::{
        CURRENT_MAP_LEVEL_GLOBAL, CodeAddress, CodeSegment, CollisionObjectReference,
        GAME_STATE_GLOBAL, GEM_COUNT_GLOBAL, GoolProgramIdentity, ITEM_POOL_1_GLOBAL,
        ITEM_POOL_2_GLOBAL, LEVEL_COUNT_GLOBAL, LEVELS_UNLOCKED_GLOBAL, NEXT_DISPLAY_GLOBAL,
        ObjectHandle as VmObjectHandle, PagingHostOperation, PagingHostRequest, PagingHostResponse,
        RetailPadSnapshot, RetailTransformVectorsCamera, SAVED_TITLE_STATE_GLOBAL, SendEventTarget,
        TITLE_STATE_GLOBAL, VmEffect, process_register,
    },
    object_arena::{NeighborZone, SpawnError},
    player::{PAD_CROSS, PAD_DOWN, PAD_LEFT, PAD_RIGHT, PAD_SQUARE, PAD_UP},
    retail_frame::RetailFrameState,
    retail_runtime::{
        CURRENT_ZONE_FLAGS_GLOBAL, ISLAND_CAMERA_ROTATION_GLOBAL, ISLAND_CAMERA_STATE_GLOBAL,
        NsfProgramError, NsfProgramHost, ProgramHost, RetailLevelStateContext,
        RetailRestartOutcome, RetailRuntime, RetailSessionCarry, RuntimeError, RuntimeObjectHandle,
        ZoneTerminationMode,
    },
    zone_lifecycle::{OrderedZoneLoadList, ZoneLifecycle, ZoneLifecycleZone, ZoneTransitionAction},
};

const GLOBAL_WORDS: usize = 256;
const DOCTOR_OBJECT_GLOBAL: usize = 16;
const LIFE_COUNT_GLOBAL: usize = 24;
const HEALTH_GLOBAL: usize = 25;
const FRUIT_COUNT_GLOBAL: usize = 26;
const BOX_COUNT_GLOBAL: usize = 62;
const CHECKPOINT_ID_GLOBAL: usize = 69;
const CHECKPOINT_TRANSLATION_GLOBALS: [usize; 3] = [102, 103, 104];
const INSTRUCTION_BUDGET: usize = 67;
const DEFAULT_SURVEY_FRAMES: u32 = 360;
const DEFAULT_PROGRESSION_FRAMES: u32 = 18_000;
const MAX_SURVEY_FRAMES: u32 = 108_000;
const EMPTY_TERMINAL_WINDOW: u32 = 8;
const TITLE_MAP_DISPLAY_MASK: u32 = 0x20_ffff;
const TITLE_DIRECT_ZONES: [&str; 10] = [
    "0a_pZ", "0b_pZ", "0c_pZ", "0d_pZ", "0e_pZ", "0f_pZ", "1a_pZ", "1e_pZ", "2b_pZ", "3a_pZ",
];

fn survey_frame_count() -> u32 {
    std::env::var("C1_SURVEY_FRAMES")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|frames| (1..=MAX_SURVEY_FRAMES).contains(frames))
        .unwrap_or(DEFAULT_SURVEY_FRAMES)
}

fn progression_frame_count() -> u32 {
    std::env::var("C1_PROGRESSION_FRAMES")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|frames| (1..=MAX_SURVEY_FRAMES).contains(frames))
        .unwrap_or(DEFAULT_PROGRESSION_FRAMES)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SurveyInputProfile {
    Idle,
    DirectionAndButtonSweep,
    DirectionAndButtonSweepToTransition,
    ForwardWithActions,
    ForwardThroughCheckpointThenA8Hit,
    JunglePhaseRobust,
    GreatGatePhaseRobust,
    GreatGateTawnaBonus,
    GreatGateYellowGemExactCarry,
    LocalPbakPrefix,
    BouldersCompletionRoute,
    UpstreamCarriedRecovery,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LevelContextSource {
    FreshBoot,
    SessionGlobals,
}

impl SurveyInputProfile {
    const fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::DirectionAndButtonSweep => "direction-and-button-sweep",
            Self::DirectionAndButtonSweepToTransition => "direction-and-button-sweep-to-transition",
            Self::ForwardWithActions => "forward-with-actions",
            Self::ForwardThroughCheckpointThenA8Hit => "forward-through-checkpoint-then-a8-hit",
            Self::JunglePhaseRobust => "jungle-phase-robust",
            Self::GreatGatePhaseRobust => "great-gate-phase-robust",
            Self::GreatGateTawnaBonus => "great-gate-tawna-bonus",
            Self::GreatGateYellowGemExactCarry => "great-gate-yellow-gem-exact-carry",
            Self::LocalPbakPrefix => "legally-local-pbak-prefix",
            Self::BouldersCompletionRoute => "boulders-completion-route",
            Self::UpstreamCarriedRecovery => "upstream-carried-recovery",
        }
    }

    const fn stops_at_transition(self) -> bool {
        matches!(
            self,
            Self::DirectionAndButtonSweepToTransition
                | Self::ForwardWithActions
                | Self::JunglePhaseRobust
                | Self::GreatGatePhaseRobust
                | Self::GreatGateTawnaBonus
                | Self::GreatGateYellowGemExactCarry
                | Self::BouldersCompletionRoute
                | Self::UpstreamCarriedRecovery
        )
    }
}

/// Grounded, path-relative route for Jungle Rollers' authentic first-completion
/// RNG phase. Square taps clear authored hazards; Cross actions traverse gaps.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct JungleRouteController {
    stage: u8,
    active: Option<RouteAction>,
    active_uses_forward: bool,
    action_tick: u8,
}

impl JungleRouteController {
    fn held(&mut self, camera: RetailCameraLocation, player: Option<PlayerTrace>) -> u32 {
        if let Some(action) = self.active {
            let held = action.held(self.action_tick);
            self.action_tick = self.action_tick.saturating_add(1);
            if self.action_tick >= action.total_frames() {
                self.active = None;
                self.action_tick = 0;
                self.stage = self.stage.saturating_add(1);
            }
            return if self.active_uses_forward {
                PAD_UP | held
            } else {
                held
            };
        }

        let zone_0b = Eid::from_name("0b_cZ").expect("fixed Jungle route EID is valid");
        let zone_0d = Eid::from_name("0d_cZ").expect("fixed Jungle route EID is valid");
        let zone_0e = Eid::from_name("0e_cZ").expect("fixed Jungle route EID is valid");
        let zone_0f = Eid::from_name("0f_cZ").expect("fixed Jungle route EID is valid");
        let zone_0g = Eid::from_name("0g_cZ").expect("fixed Jungle route EID is valid");
        let zone_0h = Eid::from_name("0h_cZ").expect("fixed Jungle route EID is valid");
        let zone_0i = Eid::from_name("0i_cZ").expect("fixed Jungle route EID is valid");
        let zone_0n = Eid::from_name("0n_cZ").expect("fixed Jungle route EID is valid");
        let zone_0o = Eid::from_name("0o_cZ").expect("fixed Jungle route EID is valid");
        let zone_0p = Eid::from_name("0p_cZ").expect("fixed Jungle route EID is valid");
        let zone_0q = Eid::from_name("0q_cZ").expect("fixed Jungle route EID is valid");
        let zone_0r = Eid::from_name("0r_cZ").expect("fixed Jungle route EID is valid");
        let zone_0s = Eid::from_name("0s_cZ").expect("fixed Jungle route EID is valid");
        let zone_0u = Eid::from_name("0u_cZ").expect("fixed Jungle route EID is valid");
        let zone_0w = Eid::from_name("0w_cZ").expect("fixed Jungle route EID is valid");
        let zone_0x = Eid::from_name("0x_cZ").expect("fixed Jungle route EID is valid");
        let zone_0y = Eid::from_name("0y_cZ").expect("fixed Jungle route EID is valid");
        let zone_0a_upper = Eid::from_name("0A_cZ").expect("fixed Jungle route EID is valid");
        let zone_0b_upper = Eid::from_name("0B_cZ").expect("fixed Jungle route EID is valid");
        let zone_0c_upper = Eid::from_name("0C_cZ").expect("fixed Jungle route EID is valid");
        let zone_0f_upper = Eid::from_name("0F_cZ").expect("fixed Jungle route EID is valid");
        let zone_0g_upper = Eid::from_name("0G_cZ").expect("fixed Jungle route EID is valid");
        let zone_0i_upper = Eid::from_name("0I_cZ").expect("fixed Jungle route EID is valid");
        let zone_0j_upper = Eid::from_name("0J_cZ").expect("fixed Jungle route EID is valid");
        let zone_0k_upper = Eid::from_name("0K_cZ").expect("fixed Jungle route EID is valid");
        let zone_0m_upper = Eid::from_name("0M_cZ").expect("fixed Jungle route EID is valid");
        let zone_0o_upper = Eid::from_name("0O_cZ").expect("fixed Jungle route EID is valid");
        let grounded = player.is_some_and(|player| player.status_a & 1 != 0);
        let progress = camera.progress.raw();
        if camera.path.zone == zone_0d
            && camera.path.index == 1
            && (2_500..5_000).contains(&progress)
        {
            return PAD_UP | PAD_CROSS;
        }
        if self.stage == 28
            && camera.path.zone == zone_0g_upper
            && camera.path.index == 0
            && progress < 4_000
        {
            return PAD_UP | PAD_SQUARE;
        }
        if self.stage == 13
            && camera.path.zone == zone_0u
            && camera.path.index == 0
            && progress < 1_000
        {
            return PAD_UP | PAD_RIGHT;
        }
        let triggered = grounded
            && match self.stage {
                0 => camera.path.zone == zone_0b && camera.path.index == 1 && progress >= 4_000,
                1 => camera.path.zone == zone_0e && camera.path.index == 1 && progress >= 1_000,
                2 => camera.path.zone == zone_0f && camera.path.index == 0 && progress >= 8_000,
                3 => camera.path.zone == zone_0g && camera.path.index == 1 && progress >= 3_000,
                4 => camera.path.zone == zone_0h && camera.path.index == 0 && progress >= 5_000,
                5 => camera.path.zone == zone_0i && camera.path.index == 0 && progress >= 10_000,
                6 => camera.path.zone == zone_0n && camera.path.index == 0 && progress >= 500,
                7 => camera.path.zone == zone_0o && camera.path.index == 0 && progress >= 6_000,
                8 => camera.path.zone == zone_0o && camera.path.index == 0 && progress >= 14_500,
                9 => camera.path.zone == zone_0p && camera.path.index == 0 && progress >= 21_000,
                10 => camera.path.zone == zone_0q && camera.path.index == 0 && progress >= 10_500,
                11 => camera.path.zone == zone_0r && camera.path.index == 0 && progress >= 18_000,
                12 => camera.path.zone == zone_0s && camera.path.index == 0 && progress >= 19_900,
                13 => camera.path.zone == zone_0u && camera.path.index == 0 && progress >= 12_000,
                14 => camera.path.zone == zone_0u && camera.path.index == 1 && progress >= 5_000,
                15 => camera.path.zone == zone_0u && camera.path.index == 1 && progress >= 18_500,
                16 => camera.path.zone == zone_0w && camera.path.index == 0 && progress >= 8_000,
                17 => camera.path.zone == zone_0x && camera.path.index == 0 && progress >= 4_000,
                18 => camera.path.zone == zone_0x && camera.path.index == 1 && progress >= 7_000,
                19 => camera.path.zone == zone_0x && camera.path.index == 1 && progress >= 18_000,
                20 => camera.path.zone == zone_0y && camera.path.index == 0 && progress >= 8_000,
                21 => {
                    camera.path.zone == zone_0a_upper && camera.path.index == 0 && progress >= 6_000
                }
                22 => {
                    camera.path.zone == zone_0b_upper
                        && camera.path.index == 0
                        && progress >= 15_500
                }
                23 => {
                    camera.path.zone == zone_0c_upper && camera.path.index == 0 && progress >= 5_000
                }
                24 => {
                    camera.path.zone == zone_0c_upper && camera.path.index == 1 && progress >= 1_500
                }
                25 => {
                    camera.path.zone == zone_0c_upper && camera.path.index == 1 && progress >= 3_800
                }
                26 => {
                    camera.path.zone == zone_0c_upper
                        && camera.path.index == 1
                        && progress >= 16_000
                }
                27 => {
                    camera.path.zone == zone_0f_upper && camera.path.index == 0 && progress >= 5_000
                }
                28 => {
                    camera.path.zone == zone_0g_upper
                        && camera.path.index == 0
                        && progress >= 16_000
                }
                29 => {
                    camera.path.zone == zone_0g_upper && camera.path.index == 1 && progress >= 7_500
                }
                30 | 31 => {
                    camera.path.zone == zone_0i_upper && camera.path.index == 1 && progress >= 2_500
                }
                32 | 33 => {
                    (camera.path.zone == zone_0i_upper
                        && camera.path.index == 1
                        && progress >= 5_000)
                        || (camera.path.zone == zone_0j_upper
                            && camera.path.index == 0
                            && progress >= 2_000)
                }
                34 | 35 => {
                    camera.path.zone == zone_0j_upper
                        && camera.path.index == 0
                        && progress >= 16_000
                }
                36 => {
                    camera.path.zone == zone_0k_upper && camera.path.index == 0 && progress >= 500
                }
                37 => {
                    camera.path.zone == zone_0k_upper && camera.path.index == 1 && progress >= 500
                }
                38 => {
                    camera.path.zone == zone_0k_upper
                        && camera.path.index == 1
                        && progress >= 12_000
                }
                39 => camera.path.zone == zone_0m_upper && camera.path.index == 1,
                40 => {
                    camera.path.zone == zone_0o_upper && camera.path.index == 0 && progress >= 1_000
                }
                41 => {
                    camera.path.zone == zone_0o_upper
                        && camera.path.index == 0
                        && progress >= 10_000
                }
                _ => false,
            };
        if !triggered {
            return PAD_UP;
        }
        // The upper 0J correction must be purely lateral; adding the usual
        // forward hold misses the narrow authored path alignment.
        self.active_uses_forward = !(self.stage == 32 && camera.path.zone == zone_0j_upper);
        self.active = Some(match self.stage {
            3 | 5 | 7 | 21 => RouteAction {
                button: PAD_SQUARE,
                button_frames: 1,
                ..RouteAction::default()
            },
            10 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 32,
                ..RouteAction::default()
            },
            11 => RouteAction {
                button: PAD_CROSS,
                button_frames: 32,
                ..RouteAction::default()
            },
            12 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 28,
                button: PAD_CROSS,
                button_frames: 32,
                ..RouteAction::default()
            },
            14 => RouteAction {
                button: PAD_SQUARE,
                button_frames: 8,
                ..RouteAction::default()
            },
            17 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 48,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            24 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                ..RouteAction::default()
            },
            32 if camera.path.zone == zone_0j_upper => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                ..RouteAction::default()
            },
            28 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 40,
                button: PAD_CROSS,
                button_frames: 32,
                ..RouteAction::default()
            },
            30 => RouteAction {
                direction_frames: 48,
                ..RouteAction::default()
            },
            31 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 32,
                button: PAD_CROSS,
                button_frames: 32,
                ..RouteAction::default()
            },
            33 if camera.path.zone == zone_0j_upper => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 32,
                button: PAD_CROSS,
                button_frames: 32,
                ..RouteAction::default()
            },
            34 => RouteAction {
                direction_frames: 1,
                ..RouteAction::default()
            },
            37 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 4,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            39 => RouteAction {
                button: PAD_SQUARE,
                button_frames: 32,
                ..RouteAction::default()
            },
            41 => RouteAction {
                // An eight-frame tap reaches WarpC's exact proximity gate
                // without carrying the jump beyond the transition window.
                button: PAD_CROSS,
                button_frames: 8,
                ..RouteAction::default()
            },
            _ => RouteAction {
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
        });
        self.held(camera, player)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RouteAction {
    direction: u32,
    direction_frames: u8,
    button: u32,
    button_start: u8,
    button_frames: u8,
}

impl RouteAction {
    fn total_frames(self) -> u8 {
        self.direction_frames
            .max(self.button_start.saturating_add(self.button_frames))
    }

    fn held(self, tick: u8) -> u32 {
        let direction = if tick < self.direction_frames {
            self.direction
        } else {
            0
        };
        let button = if tick >= self.button_start
            && tick < self.button_start.saturating_add(self.button_frames)
        {
            self.button
        } else {
            0
        };
        direction | button
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct NSanityRouteController {
    opening_stage: u8,
    stage: u8,
    active: Option<RouteAction>,
    active_is_opening: bool,
    action_tick: u8,
}

impl NSanityRouteController {
    fn held(&mut self, camera: RetailCameraLocation, player: Option<PlayerTrace>) -> u32 {
        if let Some(action) = self.active {
            let held = action.held(self.action_tick);
            self.action_tick = self.action_tick.saturating_add(1);
            if self.action_tick >= action.total_frames() {
                self.active = None;
                self.action_tick = 0;
                if self.active_is_opening {
                    self.opening_stage = self.opening_stage.saturating_add(1);
                    self.active_is_opening = false;
                } else {
                    self.stage = self.stage.saturating_add(1);
                }
            }
            return PAD_UP | held;
        }

        let Some(player) = player else {
            return PAD_UP;
        };
        let a0 = Eid::from_name("a0_9Z").expect("fixed N. Sanity route EID is valid");
        let a1 = Eid::from_name("a1_9Z").expect("fixed N. Sanity route EID is valid");
        let a2 = Eid::from_name("a2_9Z").expect("fixed N. Sanity route EID is valid");
        let a3 = Eid::from_name("a3_9Z").expect("fixed N. Sanity route EID is valid");
        let a4 = Eid::from_name("a4_9Z").expect("fixed N. Sanity route EID is valid");
        let a5 = Eid::from_name("a5_9Z").expect("fixed N. Sanity route EID is valid");
        let a6 = Eid::from_name("a6_9Z").expect("fixed N. Sanity route EID is valid");
        let a7 = Eid::from_name("a7_9Z").expect("fixed N. Sanity route EID is valid");
        let a8 = Eid::from_name("a8_9Z").expect("fixed N. Sanity route EID is valid");
        let a9 = Eid::from_name("a9_9Z").expect("fixed N. Sanity route EID is valid");
        let b0 = Eid::from_name("b0_9Z").expect("fixed N. Sanity route EID is valid");
        let b1 = Eid::from_name("b1_9Z").expect("fixed N. Sanity route EID is valid");
        let zone_2b = Eid::from_name("2b_9Z").expect("fixed N. Sanity route EID is valid");
        let zone_3b = Eid::from_name("3b_9Z").expect("fixed N. Sanity route EID is valid");
        let zone_4b = Eid::from_name("4b_9Z").expect("fixed N. Sanity route EID is valid");
        let b5 = Eid::from_name("b5_9Z").expect("fixed N. Sanity route EID is valid");
        let b6 = Eid::from_name("b6_9Z").expect("fixed N. Sanity route EID is valid");
        let b7 = Eid::from_name("b7_9Z").expect("fixed N. Sanity route EID is valid");
        let progress = camera.progress.raw();
        let grounded = player.status_a & 1 != 0;
        if self.opening_stage < 2 {
            let opening_action = match self.opening_stage {
                0 if camera.path.zone == a0 && camera.path.index == 0 && progress >= 200 => {
                    RouteAction {
                        button: PAD_CROSS,
                        button_start: 4,
                        button_frames: 1,
                        ..RouteAction::default()
                    }
                }
                1 if camera.path.zone == a0 && camera.path.index == 1 && progress >= 5_000 => {
                    RouteAction {
                        button: PAD_SQUARE,
                        button_start: 1,
                        button_frames: 1,
                        ..RouteAction::default()
                    }
                }
                _ => return PAD_UP,
            };
            self.active = Some(opening_action);
            self.active_is_opening = true;
            self.action_tick = 0;
            return self.held(camera, Some(player));
        }
        let action = match self.stage {
            0 if camera.path.zone == a1 && camera.path.index == 0 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 6,
                button: PAD_CROSS,
                button_start: 6,
                button_frames: 1,
            },
            1 if camera.path.zone == a1 && camera.path.index == 0 && progress >= 17_000 => {
                RouteAction {
                    button: PAD_SQUARE,
                    button_frames: 1,
                    ..RouteAction::default()
                }
            }
            2 if camera.path.zone == a1
                && camera.path.index == 0
                && progress >= 17_000
                && grounded =>
            {
                RouteAction {
                    button: PAD_CROSS,
                    button_frames: 1,
                    ..RouteAction::default()
                }
            }
            3 if camera.path.zone == a1 && camera.path.index == 1 && progress >= 16_000 => {
                RouteAction {
                    button: PAD_SQUARE,
                    button_frames: 1,
                    ..RouteAction::default()
                }
            }
            4 if camera.path.zone == a2 && progress >= 5_000 && grounded => RouteAction {
                button: PAD_CROSS,
                button_frames: 11,
                ..RouteAction::default()
            },
            5 if camera.path.zone == a2 && progress >= 14_000 && grounded => RouteAction {
                button: PAD_CROSS,
                button_frames: 1,
                ..RouteAction::default()
            },
            6 if camera.path.zone == a2 && progress >= 19_000 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 11,
                ..RouteAction::default()
            },
            7 if camera.path.zone == a3 && camera.path.index == 0 && progress >= 3_500 => {
                RouteAction {
                    button: PAD_SQUARE,
                    button_frames: 1,
                    ..RouteAction::default()
                }
            }
            8 if camera.path.zone == a3
                && camera.path.index == 0
                // Native local-bound refresh exposes the crate face before
                // the camera reaches 7,000; jump at the last stable 6,400
                // sample instead of relying on the former stale bound.
                && progress >= 6_000
                && grounded =>
            {
                RouteAction {
                    button: PAD_CROSS,
                    button_frames: 1,
                    ..RouteAction::default()
                }
            }
            9 if camera.path.zone == a3 && camera.path.index == 0 && progress >= 9_000 => {
                RouteAction {
                    button: PAD_SQUARE,
                    button_frames: 1,
                    ..RouteAction::default()
                }
            }
            10 if camera.path.zone == a3
                && camera.path.index == 1
                && progress >= 3_500
                && grounded =>
            {
                RouteAction {
                    button: PAD_CROSS,
                    button_frames: 11,
                    ..RouteAction::default()
                }
            }
            11 if camera.path.zone == a3 && camera.path.index == 1 && progress >= 9_000 => {
                RouteAction {
                    button: PAD_SQUARE,
                    button_frames: 1,
                    ..RouteAction::default()
                }
            }
            12 if camera.path.zone == a4 && progress <= 1_000 && grounded => RouteAction {
                button: PAD_CROSS,
                button_frames: 11,
                ..RouteAction::default()
            },
            13 if camera.path.zone == a4 && progress >= 12_000 && grounded => RouteAction {
                button: PAD_CROSS,
                button_frames: 1,
                ..RouteAction::default()
            },
            14 if camera.path.zone == a4 && progress >= 20_000 => RouteAction {
                button: PAD_SQUARE,
                button_frames: 1,
                ..RouteAction::default()
            },
            // The native same-stamp collision tail exposes the next face at
            // 31,941 and closes the lateral route. Start this spin/jump/left
            // sequence early enough to establish the sidestep before contact.
            15 if camera.path.zone == a4 && progress >= 25_000 => RouteAction {
                button: PAD_SQUARE,
                button_frames: 1,
                ..RouteAction::default()
            },
            16 if camera.path.zone == a4 && progress >= 26_000 && grounded => RouteAction {
                button: PAD_CROSS,
                button_frames: 1,
                ..RouteAction::default()
            },
            17 if camera.path.zone == a4 && progress >= 27_000 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 11,
                ..RouteAction::default()
            },
            18 if camera.path.zone == a4 && player.zone == a5 && grounded => RouteAction {
                button: PAD_SQUARE,
                button_frames: 1,
                ..RouteAction::default()
            },
            19 if camera.path.zone == a4 && player.zone == a5 && progress >= 33_500 && grounded => {
                // Delay the lateral jump until the a5 octree step is in
                // range; an immediate forward jump lands before its face.
                RouteAction {
                    direction: PAD_LEFT,
                    direction_frames: 11,
                    button: PAD_CROSS,
                    button_frames: 11,
                    ..RouteAction::default()
                }
            }
            20 if camera.path.zone == a5 && progress >= 1_000 && grounded => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 11,
                button: PAD_CROSS,
                button_frames: 11,
                ..RouteAction::default()
            },
            // The corrected a5 collision path presents the same authored
            // obstacles earlier: the first terrain face is stable at 3,205.
            21 if camera.path.zone == a5 && progress >= 3_200 => RouteAction {
                button: PAD_SQUARE,
                button_frames: 1,
                ..RouteAction::default()
            },
            22 if camera.path.zone == a5 && progress >= 3_200 && grounded => RouteAction {
                button: PAD_CROSS,
                button_frames: 1,
                ..RouteAction::default()
            },
            23 if camera.path.zone == a5 && progress >= 3_200 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            // This lands on entity 32 at 8,064; jump before forward input
            // remains pinned to the crate top.
            24 if camera.path.zone == a5 && progress >= 8_000 => RouteAction {
                button: PAD_SQUARE,
                button_frames: 1,
                ..RouteAction::default()
            },
            25 if camera.path.zone == a5 && progress >= 8_000 && grounded => RouteAction {
                button: PAD_CROSS,
                button_frames: 1,
                ..RouteAction::default()
            },
            // At the next stable terrain face (11,036), the right-hand lane
            // is open; the former left jump lands back against the face.
            26 if camera.path.zone == a5 && progress >= 11_000 && grounded => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 11,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            27 if camera.path.zone == a5 && progress >= 16_000 => RouteAction {
                button: PAD_SQUARE,
                button_frames: 1,
                ..RouteAction::default()
            },
            28 if camera.path.zone == a5 && progress >= 16_000 && grounded => RouteAction {
                // Move onto the subtype-10 bounce crate and keep Cross held
                // through its delayed rebound so Crash carries into a6.
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 32,
                ..RouteAction::default()
            },
            29 if camera.path.zone == a6 && progress >= 2_000 && grounded => RouteAction {
                button: PAD_CROSS,
                button_frames: 11,
                ..RouteAction::default()
            },
            30 if camera.path.zone == a6 && progress >= 7_000 && grounded => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            31 if camera.path.zone == a6 && progress >= 10_000 && grounded => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            32 if camera.path.zone == a6 && progress >= 15_000 && grounded => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            33 if camera.path.zone == a7 && progress >= 2_500 && grounded => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            34 if camera.path.zone == a7 && progress >= 9_000 && grounded => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            35 if camera.path.zone == a7
                && camera.path.index == 1
                && progress >= 2_900
                && grounded =>
            {
                RouteAction {
                    button: PAD_CROSS,
                    button_frames: 16,
                    ..RouteAction::default()
                }
            }
            36 if camera.path.zone == a7
                && camera.path.index == 1
                && progress >= 18_500
                && grounded =>
            {
                RouteAction {
                    button: PAD_CROSS,
                    button_frames: 16,
                    ..RouteAction::default()
                }
            }
            37 if camera.path.zone == a7
                && camera.path.index == 2
                && progress >= 13_000
                && grounded =>
            {
                RouteAction {
                    button: PAD_CROSS,
                    button_frames: 16,
                    ..RouteAction::default()
                }
            }
            38 if camera.path.zone == a8
                && camera.path.index == 0
                // The preceding jump lands into entity 39 at 10,407; its
                // accepted solid contact sends Crash's death event. Jump
                // again from the stable 6,400 sample to clear that contact.
                && progress >= 6_000
                && grounded =>
            {
                RouteAction {
                    button: PAD_CROSS,
                    button_frames: 16,
                    ..RouteAction::default()
                }
            }
            39 if camera.path.zone == a8
                && camera.path.index == 1
                // This path loses its grounded status after 13,056, so the
                // former 16,500 guard could never start the required jump.
                && progress >= 13_000
                && grounded =>
            {
                RouteAction {
                    button: PAD_CROSS,
                    button_frames: 16,
                    ..RouteAction::default()
                }
            }
            40 if camera.path.zone == a9
                && camera.path.index == 0
                && progress >= 11_000
                && grounded =>
            {
                RouteAction {
                    button: PAD_CROSS,
                    button_frames: 16,
                    ..RouteAction::default()
                }
            }
            41 if camera.path.zone == a9
                && camera.path.index == 2
                // The last grounded sample is 8,355; at 8,874 Crash has
                // already left the face, making the former 9,000 guard late.
                && progress >= 8_000
                && grounded =>
            {
                RouteAction {
                    button: PAD_CROSS,
                    button_frames: 16,
                    ..RouteAction::default()
                }
            }
            42 if camera.path.zone == a9
                && camera.path.index == 1
                && progress >= 6_000
                && grounded =>
            {
                RouteAction {
                    button: PAD_CROSS,
                    button_frames: 16,
                    ..RouteAction::default()
                }
            }
            43 if camera.path.zone == b0
                && camera.path.index == 0
                && progress >= 2_000
                && grounded =>
            {
                RouteAction {
                    button: PAD_CROSS,
                    button_frames: 16,
                    ..RouteAction::default()
                }
            }
            44 if camera.path.zone == b0
                && camera.path.index == 0
                // The camera parks at 16,384 on this grounded face, so the
                // former 17,000 guard could never advance the route.
                && progress >= 16_000
                && grounded =>
            {
                RouteAction {
                    button: PAD_CROSS,
                    button_frames: 16,
                    ..RouteAction::default()
                }
            }
            // b0 path 1 ends against stacked static cells (no dynamic link).
            // A straight jump returns to the same face; left clears into b1.
            45 if camera.path.zone == b0 && camera.path.index == 1 && grounded => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            46 if camera.path.zone == b1 && camera.path.index == 0 && grounded => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 8,
                button: PAD_CROSS,
                button_start: 0,
                button_frames: 16,
            },
            47 if camera.path.zone == zone_2b
                && camera.path.index == 1
                && progress >= 10_000
                && grounded =>
            {
                RouteAction {
                    direction: PAD_LEFT,
                    direction_frames: 12,
                    button: PAD_CROSS,
                    button_frames: 16,
                    ..RouteAction::default()
                }
            }
            48 if camera.path.zone == zone_3b
                && camera.path.index == 0
                && progress >= 9_000
                && grounded =>
            {
                RouteAction {
                    direction: PAD_DOWN,
                    direction_frames: 30,
                    button: PAD_CROSS,
                    button_frames: 1,
                    ..RouteAction::default()
                }
            }
            49 if camera.path.zone == zone_3b && grounded => RouteAction {
                direction: PAD_DOWN,
                direction_frames: 80,
                ..RouteAction::default()
            },
            50 if camera.path.zone == zone_3b && grounded => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 18,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            // 4b path 0 parks against static cells with no dynamic link.
            // Jump from its grounded entry sample to clear into retail b5.
            51 if camera.path.zone == zone_4b && camera.path.index == 0 && grounded => {
                RouteAction {
                    button: PAD_CROSS,
                    button_frames: 16,
                    ..RouteAction::default()
                }
            }
            // The authored camera topology is b5:p4 -> b5:p1 -> b6:p0.
            // Two grounded jumps clear the static steps while staying inside
            // the center/right lane selected by the retail collision bitmap.
            52 if camera.path.zone == b5
                && camera.path.index == 4
                && progress >= 10_000
                && grounded =>
            {
                RouteAction {
                    button: PAD_CROSS,
                    button_frames: 16,
                    ..RouteAction::default()
                }
            }
            53 if camera.path.zone == b5
                && camera.path.index == 1
                && progress >= 5_500
                && grounded =>
            {
                RouteAction {
                    direction: PAD_RIGHT,
                    direction_frames: 16,
                    button: PAD_CROSS,
                    button_frames: 16,
                    ..RouteAction::default()
                }
            }
            // b6:p1 has one last static rise before the terminal b7 zone.
            54 if camera.path.zone == b6
                && camera.path.index == 1
                && progress >= 6_500
                && grounded =>
            {
                RouteAction {
                    button: PAD_CROSS,
                    button_frames: 16,
                    ..RouteAction::default()
                }
            }
            // Stay in the live WarpC portal lane; the previous left jump
            // bypassed the portal and parked against b7's static boundary.
            55 if camera.path.zone == b7 && camera.path.index == 0 && grounded => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 8,
                ..RouteAction::default()
            },
            _ => return PAD_UP,
        };
        self.active = Some(action);
        self.action_tick = 0;
        self.held(camera, Some(player))
    }
}

/// State-anchored route through The Great Gate's normal end `WarpC` or the
/// Yellow Gem platform path. The opening sequence is anchored to the end of
/// Crash's authored spawn animation and the first climb waits for grounded
/// camera locations. Past that anchor, short pad windows preserve the
/// authentic carried hazard phase through the checkpoint and later gaps.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct GreatGateRouteController {
    yellow_gem_route: bool,
    opening_stage: u8,
    opening_ready_frames: u8,
    stage: u8,
    active: Option<RouteAction>,
    action_tick: u8,
    pickup_wait_frames: u16,
}

impl GreatGateRouteController {
    // Keep equal actions as separate numbered stages so the deterministic
    // route remains auditable against its frame-by-frame pad trace.
    #[allow(clippy::match_same_arms)]
    fn held(
        &mut self,
        camera: RetailCameraLocation,
        player: Option<PlayerTrace>,
        collect_tawna_tokens: bool,
    ) -> u32 {
        if let Some(action) = self.active {
            let mut held = action.held(self.action_tick);
            if !self.yellow_gem_route && self.stage == 72 && (18..48).contains(&self.action_tick) {
                held = 0;
            }
            if self.stage == 90 && self.action_tick >= 4 {
                held &= !PAD_UP;
            }
            if self.stage == 102 && !collect_tawna_tokens && self.action_tick < 16 {
                held = 0;
            }
            if self.stage == 102
                && !collect_tawna_tokens
                && !self.yellow_gem_route
                && camera.path.zone
                    == Eid::from_name("c6_iZ").expect("fixed Great Gate route EID is valid")
            {
                held = PAD_RIGHT;
            }
            if self.stage == 102
                && self.yellow_gem_route
                && self.action_tick < 47
                && camera.path.zone
                    == Eid::from_name("c6_iZ").expect("fixed Great Gate route EID is valid")
            {
                held = PAD_RIGHT;
            }
            if self.stage == 106 && self.yellow_gem_route {
                held &= !(PAD_UP | PAD_DOWN);
                if self.action_tick < 3 {
                    held |= PAD_UP;
                }
            }
            if self.stage == 107 && self.yellow_gem_route {
                let braking = self.action_tick >= 24
                    && player.is_some_and(|player| player.translation[0] <= 3_780_000);
                if braking {
                    held = PAD_RIGHT;
                } else {
                    held = PAD_LEFT;
                    if self.action_tick < 16 || self.action_tick >= 24 {
                        held |= PAD_CROSS;
                    }
                }
            }
            self.action_tick = self.action_tick.saturating_add(1);
            if self.action_tick >= action.total_frames() {
                self.active = None;
                self.action_tick = 0;
                if self.opening_stage < 12 {
                    self.opening_stage = self.opening_stage.saturating_add(1);
                } else {
                    self.stage = self.stage.saturating_add(1);
                }
            }
            return held;
        }

        let Some(player) = player else {
            return 0;
        };
        let a1 = Eid::from_name("a1_iZ").expect("fixed Great Gate route EID is valid");
        let a2 = Eid::from_name("a2_iZ").expect("fixed Great Gate route EID is valid");
        let a4 = Eid::from_name("a4_iZ").expect("fixed Great Gate route EID is valid");
        let a5 = Eid::from_name("a5_iZ").expect("fixed Great Gate route EID is valid");
        let a6 = Eid::from_name("a6_iZ").expect("fixed Great Gate route EID is valid");
        let a7 = Eid::from_name("a7_iZ").expect("fixed Great Gate route EID is valid");
        let a8 = Eid::from_name("a8_iZ").expect("fixed Great Gate route EID is valid");
        let a9 = Eid::from_name("a9_iZ").expect("fixed Great Gate route EID is valid");
        let b0 = Eid::from_name("b0_iZ").expect("fixed Great Gate route EID is valid");
        let c7 = Eid::from_name("c7_iZ").expect("fixed Great Gate route EID is valid");
        let grounded = player.status_a & 1 != 0;
        let progress = camera.progress.raw();

        // Retain forward momentum between the two c7 platform jumps, but
        // release Cross so the grounded action below produces a fresh tap.
        if self.yellow_gem_route && self.stage == 109 && camera.path.zone == c7 && !grounded {
            return PAD_LEFT;
        }

        let pickup_target = if self.stage == 76 && player.tawna_counter < 0x200 {
            Some([15_154_944, 127_744])
        } else if collect_tawna_tokens && self.stage == 102 && player.tawna_counter < 0x300 {
            Some([5_426_944, 127_744])
        } else {
            None
        };
        if let Some([target_x, target_z]) = pickup_target {
            self.pickup_wait_frames = self.pickup_wait_frames.saturating_add(1);
            let mut held = 0;
            if player.translation[0] < target_x - 20_000 {
                held |= PAD_RIGHT;
            } else if player.translation[0] > target_x + 20_000 {
                held |= PAD_LEFT;
            }
            if player.translation[2] < target_z - 12_000 {
                held |= PAD_DOWN;
            } else if player.translation[2] > target_z + 12_000 {
                held |= PAD_UP;
            }
            if (self.pickup_wait_frames % 45) >= 14 && (self.pickup_wait_frames % 45) < 30 {
                held |= PAD_CROSS;
            }
            return held;
        }
        self.pickup_wait_frames = 0;

        if self.opening_stage < 12 {
            if self.opening_stage == 0 && self.opening_ready_frames < 4 {
                let ready = camera.path.zone == a1
                    && camera.path.index == 0
                    && progress == 0
                    && player.state == 1
                    && grounded;
                self.opening_ready_frames = if ready {
                    self.opening_ready_frames.saturating_add(1)
                } else {
                    0
                };
                return 0;
            }
            self.active = Some(match self.opening_stage {
                0 => RouteAction {
                    direction: PAD_LEFT,
                    direction_frames: 11,
                    button: PAD_CROSS,
                    button_frames: 11,
                    ..RouteAction::default()
                },
                1 => RouteAction {
                    direction: PAD_LEFT,
                    direction_frames: 6,
                    ..RouteAction::default()
                },
                2 => RouteAction {
                    direction: PAD_RIGHT,
                    direction_frames: 16,
                    ..RouteAction::default()
                },
                3 => RouteAction {
                    direction: PAD_UP | PAD_LEFT,
                    direction_frames: 19,
                    ..RouteAction::default()
                },
                4 => RouteAction {
                    direction: PAD_DOWN | PAD_RIGHT,
                    direction_frames: 13,
                    ..RouteAction::default()
                },
                5 => RouteAction {
                    direction: PAD_DOWN | PAD_RIGHT,
                    direction_frames: 8,
                    button: PAD_SQUARE,
                    button_frames: 8,
                    ..RouteAction::default()
                },
                6 => RouteAction {
                    direction: PAD_UP | PAD_RIGHT,
                    direction_frames: 22,
                    ..RouteAction::default()
                },
                7 => RouteAction {
                    button: PAD_CROSS,
                    button_frames: 24,
                    ..RouteAction::default()
                },
                8 => RouteAction {
                    direction: PAD_LEFT,
                    direction_frames: 26,
                    button: PAD_CROSS,
                    button_frames: 26,
                    ..RouteAction::default()
                },
                9 => RouteAction {
                    direction: PAD_RIGHT,
                    direction_frames: 15,
                    button: PAD_CROSS,
                    button_frames: 15,
                    ..RouteAction::default()
                },
                10 => RouteAction {
                    direction: PAD_LEFT,
                    direction_frames: 22,
                    button: PAD_CROSS,
                    button_frames: 22,
                    ..RouteAction::default()
                },
                11 => RouteAction {
                    direction: PAD_RIGHT,
                    direction_frames: 39,
                    button: PAD_CROSS,
                    button_frames: 39,
                    ..RouteAction::default()
                },
                _ => unreachable!("all Great Gate opening stages are matched"),
            });
            return self.held(camera, Some(player), collect_tawna_tokens);
        }

        let triggered = match self.stage {
            0
            | 2
            | 4
            | 6
            | 8
            | 10
            | 11
            | 13
            | 15
            | 17
            | 18
            | 19
            | 21
            | 23
            | 25
            | 27
            | 29..=39
            | 41..=106 => true,
            1 => camera.path.zone == a2 && camera.path.index == 1 && player.state == 13 && grounded,
            3 => camera.path.zone == a4 && camera.path.index == 0 && grounded,
            5 => camera.path.zone == a5 && progress >= 4_300 && grounded,
            7 => camera.path.zone == a5 && progress >= 17_700 && grounded,
            9 => camera.path.zone == a6 && progress >= 2_100 && grounded,
            12 => camera.path.zone == a6 && progress >= 12_400 && grounded,
            14 => camera.path.zone == a6 && progress >= 30_400 && grounded,
            16 => camera.path.zone == a7 && progress >= 27_200 && grounded,
            20 => camera.path.zone == a8 && progress >= 5_600 && grounded,
            22 => camera.path.zone == a8 && progress >= 24_900 && grounded,
            24 => camera.path.zone == a9 && progress >= 18_700 && grounded,
            26 => {
                camera.path.zone == b0 && camera.path.index == 0 && progress >= 22_800 && grounded
            }
            28 => {
                camera.path.zone == b0 && camera.path.index == 1 && player.state == 13 && grounded
            }
            40 => camera.path.zone == b0 && camera.path.index == 1 && grounded,
            107 if self.yellow_gem_route => true,
            108 if self.yellow_gem_route => grounded,
            109 if self.yellow_gem_route => camera.path.zone == c7 && grounded,
            110 if self.yellow_gem_route => camera.path.zone == c7 && grounded,
            _ => false,
        };
        if !triggered {
            return 0;
        }
        self.active = Some(match self.stage {
            0 => RouteAction {
                direction_frames: 17,
                ..RouteAction::default()
            },
            1 | 3 | 7 | 12 | 18 | 20 | 24 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            2 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 80,
                button: PAD_CROSS | PAD_SQUARE,
                button_start: 40,
                button_frames: 11,
            },
            4 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 60,
                button: PAD_CROSS | PAD_SQUARE,
                button_start: 4,
                button_frames: 11,
            },
            5 | 22 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 16,
                button: PAD_SQUARE,
                button_frames: 16,
                ..RouteAction::default()
            },
            6 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 11,
                ..RouteAction::default()
            },
            8 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 17,
                ..RouteAction::default()
            },
            9 => RouteAction {
                button: PAD_CROSS | PAD_SQUARE,
                button_frames: 11,
                ..RouteAction::default()
            },
            10 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 39,
                button: PAD_CROSS | PAD_SQUARE,
                button_frames: 39,
                ..RouteAction::default()
            },
            11 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 1,
                ..RouteAction::default()
            },
            13 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 23,
                ..RouteAction::default()
            },
            14 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 16,
                button: PAD_CROSS | PAD_SQUARE,
                button_frames: 16,
                ..RouteAction::default()
            },
            15 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 44,
                ..RouteAction::default()
            },
            16 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 5,
                button: PAD_SQUARE,
                button_frames: 5,
                ..RouteAction::default()
            },
            17 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                ..RouteAction::default()
            },
            19 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 28,
                ..RouteAction::default()
            },
            21 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 26,
                ..RouteAction::default()
            },
            23 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 38,
                ..RouteAction::default()
            },
            25 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 54,
                ..RouteAction::default()
            },
            26 => RouteAction {
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            27 => RouteAction {
                direction_frames: 7,
                ..RouteAction::default()
            },
            28 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 1,
                button: PAD_SQUARE,
                button_frames: 1,
                ..RouteAction::default()
            },
            29 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 2,
                button: PAD_CROSS | PAD_SQUARE,
                button_frames: 2,
                ..RouteAction::default()
            },
            30 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 4,
                button: PAD_CROSS,
                button_frames: 4,
                ..RouteAction::default()
            },
            31 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 3,
                button: PAD_CROSS | PAD_SQUARE,
                button_frames: 3,
                ..RouteAction::default()
            },
            32 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 2,
                button: PAD_CROSS,
                button_frames: 2,
                ..RouteAction::default()
            },
            33 | 37 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 5,
                button: PAD_CROSS,
                button_frames: 5,
                ..RouteAction::default()
            },
            34 | 36 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 3,
                ..RouteAction::default()
            },
            35 => RouteAction {
                direction_frames: 11,
                ..RouteAction::default()
            },
            38 => RouteAction {
                button: PAD_CROSS,
                button_frames: 11,
                ..RouteAction::default()
            },
            39 => RouteAction {
                direction_frames: 2,
                ..RouteAction::default()
            },
            40 | 43 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            // Let the rotating log settle under Crash, then use its horizontal
            // top as the takeoff point for the authored arrow-crate climb.
            41 => RouteAction {
                direction_frames: 14,
                ..RouteAction::default()
            },
            42 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 2,
                ..RouteAction::default()
            },
            44 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 30,
                ..RouteAction::default()
            },
            // One neutral frame leaves Crash flush with the arrow crate.
            45 => RouteAction {
                direction_frames: 1,
                ..RouteAction::default()
            },
            // A one-frame left bias on the next jump triggers the launch while
            // preserving the stable b1 camera phase.
            46 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 1,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            // Chain the next two arrow crates while their authored launch
            // states own vertical motion. The neutral windows wait for each
            // bounce; short right holds select the next crate's landing face.
            47 => RouteAction {
                direction_frames: 5,
                ..RouteAction::default()
            },
            48 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 16,
                ..RouteAction::default()
            },
            49 => RouteAction {
                direction_frames: 9,
                ..RouteAction::default()
            },
            50 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 12,
                ..RouteAction::default()
            },
            // Steer off the third arrow crate onto b3's left-hand ground,
            // break the authored checkpoint crate, and cross its first gap.
            51 => RouteAction {
                direction_frames: 10,
                ..RouteAction::default()
            },
            52 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 32,
                ..RouteAction::default()
            },
            53 => RouteAction {
                direction_frames: 5,
                ..RouteAction::default()
            },
            54 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 8,
                button: PAD_SQUARE,
                button_frames: 8,
                ..RouteAction::default()
            },
            55 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 15,
                ..RouteAction::default()
            },
            56 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            57 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 21,
                ..RouteAction::default()
            },
            // The preceding jump bounces off the authored enemy. Wait until
            // Crash returns to stable ground before ending this golden window.
            58 => RouteAction {
                direction_frames: 6,
                ..RouteAction::default()
            },
            59 => RouteAction {
                direction_frames: 1,
                ..RouteAction::default()
            },
            60 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 10,
                ..RouteAction::default()
            },
            61 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            62 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 11,
                ..RouteAction::default()
            },
            63 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 8,
                button: PAD_SQUARE,
                button_frames: 8,
                ..RouteAction::default()
            },
            64 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 12,
                ..RouteAction::default()
            },
            65 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            66 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 4,
                ..RouteAction::default()
            },
            // Two timed spins clear b4's authored enemies without disturbing
            // the continuous leftward gap approach.
            67 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 8,
                button: PAD_SQUARE,
                button_frames: 8,
                ..RouteAction::default()
            },
            68 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 18,
                ..RouteAction::default()
            },
            69 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            70 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 26,
                ..RouteAction::default()
            },
            71 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS | PAD_SQUARE,
                button_frames: 16,
                ..RouteAction::default()
            },
            72 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: if self.yellow_gem_route { 52 } else { 82 },
                ..RouteAction::default()
            },
            73 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS | PAD_SQUARE,
                button_frames: 16,
                ..RouteAction::default()
            },
            74 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 24,
                button: if self.yellow_gem_route {
                    0
                } else {
                    PAD_CROSS | PAD_SQUARE
                },
                button_frames: if self.yellow_gem_route { 0 } else { 16 },
                ..RouteAction::default()
            },
            75 => RouteAction {
                direction: PAD_DOWN | PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS | PAD_SQUARE,
                button_frames: 16,
                ..RouteAction::default()
            },
            76 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 96,
                ..RouteAction::default()
            },
            77 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 2,
                ..RouteAction::default()
            },
            78 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            79 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 21,
                ..RouteAction::default()
            },
            80 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_start: 8,
                button_frames: 8,
            },
            81 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 31,
                ..RouteAction::default()
            },
            82 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 24,
                button: PAD_CROSS,
                button_start: 8,
                button_frames: 16,
            },
            83 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 50,
                ..RouteAction::default()
            },
            84 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            85 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 51,
                ..RouteAction::default()
            },
            86 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 17,
                ..RouteAction::default()
            },
            87 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            88 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 72,
                button: PAD_CROSS | PAD_SQUARE,
                button_start: 32,
                button_frames: 11,
            },
            89 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            // Move into c3's safe depth lane and jump its rising log.
            90 => RouteAction {
                direction: PAD_UP | PAD_LEFT,
                direction_frames: 22,
                button: PAD_CROSS,
                button_frames: 22,
                ..RouteAction::default()
            },
            91 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            // c4's first WalOC rises before Crash can reach it directly.
            // Brake clear of its face, wait for lowered state four, then use
            // one jump onto the log and a second jump off its far edge.
            92 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 14,
                ..RouteAction::default()
            },
            93 => RouteAction {
                direction: PAD_RIGHT,
                direction_frames: 8,
                ..RouteAction::default()
            },
            94 => RouteAction {
                direction_frames: 50,
                ..RouteAction::default()
            },
            95 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS | PAD_SQUARE,
                button_frames: 16,
                ..RouteAction::default()
            },
            96 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 8,
                ..RouteAction::default()
            },
            97 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS | PAD_SQUARE,
                button_frames: 16,
                ..RouteAction::default()
            },
            // Cross the rest of c4 and jump the c5 entry gap.
            98 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 62,
                button: PAD_CROSS,
                button_start: 48,
                button_frames: 14,
            },
            99 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            // Land in front of Tawna-token crate 113 and break it before
            // continuing toward c6.
            100 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 11,
                ..RouteAction::default()
            },
            101 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 8,
                button: if collect_tawna_tokens {
                    PAD_SQUARE
                } else {
                    PAD_CROSS
                },
                button_frames: 8,
                ..RouteAction::default()
            },
            // Jump off c6's lowered WalOC, then cross the final gap into c7.
            102 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: if collect_tawna_tokens { 51 } else { 67 },
                button: if collect_tawna_tokens {
                    0
                } else {
                    PAD_CROSS | PAD_SQUARE
                },
                button_start: if collect_tawna_tokens { 0 } else { 24 },
                button_frames: if collect_tawna_tokens { 0 } else { 16 },
            },
            103 => RouteAction {
                direction: if self.yellow_gem_route {
                    PAD_RIGHT
                } else {
                    PAD_LEFT
                },
                direction_frames: 16,
                button: if self.yellow_gem_route {
                    PAD_SQUARE
                } else {
                    PAD_CROSS | PAD_SQUARE
                },
                button_frames: 16,
                ..RouteAction::default()
            },
            104 => RouteAction {
                direction: if self.yellow_gem_route {
                    PAD_DOWN | PAD_RIGHT
                } else {
                    PAD_LEFT
                },
                direction_frames: if self.yellow_gem_route || collect_tawna_tokens {
                    18
                } else {
                    12
                },
                ..RouteAction::default()
            },
            105 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: if self.yellow_gem_route { 48 } else { 16 },
                button: PAD_CROSS,
                button_start: if self.yellow_gem_route { 24 } else { 0 },
                button_frames: 16,
            },
            // The Yellow Gem's retail entitlement makes GemsC subtype five
            // solid. Enter its narrow depth lane before the staged c6 jumps.
            106 if self.yellow_gem_route => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            107 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: if self.yellow_gem_route { 38 } else { 10 },
                button: if self.yellow_gem_route { 0 } else { PAD_CROSS },
                button_frames: if self.yellow_gem_route { 0 } else { 16 },
                ..RouteAction::default()
            },
            108 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            109 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 16,
                button: PAD_CROSS,
                button_frames: 16,
                ..RouteAction::default()
            },
            110 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 48,
                ..RouteAction::default()
            },
            // Without the Yellow Gem, the held direction ends at normal WarpC
            // entity 33; its authored state owns Crash through Level Complete.
            106 => RouteAction {
                direction: PAD_LEFT,
                direction_frames: 250,
                button: PAD_CROSS,
                button_start: 18,
                button_frames: 16,
            },
            _ => unreachable!("all triggered Great Gate stages are matched"),
        });
        self.held(camera, Some(player), collect_tawna_tokens)
    }
}

/// Legally-local Boulders PBAK opening followed by a deterministic route
/// through the retail checkpoint and normal end `WarpC` transition.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct BouldersCompletionRouteController {
    zero_t_takeoff_fired: bool,
}

impl BouldersCompletionRouteController {
    fn held(
        &mut self,
        frame: u32,
        camera: RetailCameraLocation,
        player: Option<PlayerTrace>,
        local_pbak_held: Option<u32>,
    ) -> u32 {
        let second_hazard = Eid::from_name("0F_eZ").expect("fixed Boulders route EID is valid");
        let third_hazard = Eid::from_name("0E_eZ").expect("fixed Boulders route EID is valid");
        let fourth_hazard = Eid::from_name("0D_eZ").expect("fixed Boulders route EID is valid");
        let checkpoint_hazard = Eid::from_name("0C_eZ").expect("fixed Boulders route EID is valid");
        let post_checkpoint_hazard =
            Eid::from_name("0B_eZ").expect("fixed Boulders route EID is valid");
        let second_post_checkpoint_hazard =
            Eid::from_name("0A_eZ").expect("fixed Boulders route EID is valid");
        let third_post_checkpoint_hazard =
            Eid::from_name("0z_eZ").expect("fixed Boulders route EID is valid");
        let fourth_post_checkpoint_hazard =
            Eid::from_name("0v_eZ").expect("fixed Boulders route EID is valid");
        let fifth_post_checkpoint_hazard =
            Eid::from_name("0u_eZ").expect("fixed Boulders route EID is valid");
        let sixth_post_checkpoint_hazard =
            Eid::from_name("0t_eZ").expect("fixed Boulders route EID is valid");
        let seventh_post_checkpoint_hazard =
            Eid::from_name("0s_eZ").expect("fixed Boulders route EID is valid");

        let zero_t_takeoff = camera.path.zone == sixth_post_checkpoint_hazard
            && camera.path.index == 0
            && camera.progress.raw() >= 6_400
            && !self.zero_t_takeoff_fired;
        if zero_t_takeoff {
            self.zero_t_takeoff_fired = true;
        }

        let held = if frame <= 895 {
            local_pbak_held.expect("the legally local PBAK prefix is loaded before frame execution")
        } else if frame <= 911
            || (camera.path.zone == second_hazard && camera.progress.raw() <= 7_000)
        {
            PAD_DOWN | PAD_CROSS
        } else if camera.path.zone == second_hazard
            && camera.path.index == 0
            && (10_500..14_000).contains(&camera.progress.raw())
        {
            PAD_DOWN | PAD_CROSS | PAD_SQUARE
        } else if camera.path.zone == second_hazard && camera.progress.raw() >= 10_500 {
            PAD_DOWN | PAD_CROSS
        } else if camera.path.zone == second_hazard {
            PAD_DOWN
        } else if camera.path.zone == third_hazard
            && camera.path.index == 0
            && camera.progress.raw() >= 15_500
        {
            let mut held = PAD_DOWN | PAD_CROSS | PAD_SQUARE;
            if player.is_some_and(|player| player.translation[0] > 1_870_000) {
                held |= PAD_LEFT;
            }
            held
        } else if camera.path.zone == third_hazard
            && camera.path.index == 0
            && camera.progress.raw() >= 9_500
        {
            let mut held = PAD_DOWN | PAD_CROSS;
            if player.is_some_and(|player| player.translation[0] > 1_870_000) {
                held |= PAD_LEFT;
            }
            held
        } else if camera.path.zone == third_hazard
            && camera.path.index == 1
            && camera.progress.raw() < 2_000
        {
            let mut held = PAD_DOWN | PAD_SQUARE;
            if player.is_some_and(|player| player.translation[0] > 1_870_000) {
                held |= PAD_LEFT;
            }
            held
        } else if camera.path.zone == third_hazard
            && camera.path.index == 1
            && player.is_some_and(|player| player.translation[0] < 2_020_000)
        {
            PAD_DOWN | PAD_RIGHT | PAD_CROSS
        } else if camera.path.zone == third_hazard
            && ((camera.path.index == 0 && camera.progress.raw() >= 7_000)
                || (camera.path.index == 1 && camera.progress.raw() >= 6_000))
        {
            PAD_DOWN | PAD_CROSS
        } else if camera.path.zone == fourth_hazard
            && camera.path.index == 0
            && (4_000..14_000).contains(&camera.progress.raw())
        {
            let mut held = PAD_DOWN | PAD_CROSS;
            if camera.progress.raw() >= 6_000 {
                held |= PAD_RIGHT;
            }
            held
        } else if camera.path.zone == fourth_hazard && camera.path.index == 1 {
            let mut held = PAD_DOWN;
            if camera.progress.raw() < 4_000
                && player.is_some_and(|player| player.translation[0] > 2_110_000)
            {
                held |= PAD_LEFT;
            } else if camera.progress.raw() >= 4_000
                && player.is_some_and(|player| player.translation[0] < 2_210_000)
            {
                held |= PAD_RIGHT | PAD_SQUARE;
            } else {
                held |= PAD_SQUARE;
            }
            if (5_500..9_500).contains(&camera.progress.raw()) {
                held |= PAD_CROSS;
            }
            held
        } else if camera.path.zone == sixth_post_checkpoint_hazard
            && camera.path.index == 0
            && camera.progress.raw() < 2_000
        {
            // Three early steering frames align the first 0t landing. Holding
            // Right through the jump instead carries Crash past its lane.
            PAD_DOWN | PAD_RIGHT
        } else if zero_t_takeoff {
            // This must be a single button edge; retaining Square suppresses
            // the later cadence pulse needed across the second platform.
            PAD_DOWN | PAD_CROSS | PAD_SQUARE
        } else if camera.path.zone == seventh_post_checkpoint_hazard
            && camera.path.index == 1
            && camera.progress.raw() >= 9_000
        {
            // The final path bends away from WarpC. Move into its authored
            // proximity lane while preserving Cross until the landing.
            let mut held = PAD_DOWN | PAD_RIGHT;
            if camera.progress.raw() < 16_000 {
                held |= PAD_CROSS;
            }
            held
        } else if (camera.path.zone == checkpoint_hazard
            && camera.path.index == 0
            && (11_000..14_500).contains(&camera.progress.raw()))
            || (camera.path.zone == post_checkpoint_hazard
                && camera.path.index == 1
                && (9_000..19_000).contains(&camera.progress.raw()))
            || (camera.path.zone == second_post_checkpoint_hazard
                && ((camera.path.index == 0 && (5_000..19_000).contains(&camera.progress.raw()))
                    || (camera.path.index == 1
                        && (3_000..16_000).contains(&camera.progress.raw()))))
            || (camera.path.zone == third_post_checkpoint_hazard
                && camera.path.index == 0
                && (2_000..15_000).contains(&camera.progress.raw()))
            || (camera.path.zone == fourth_post_checkpoint_hazard
                && ((camera.path.index == 0 && camera.progress.raw() < 11_000)
                    || (camera.path.index == 1
                        && (9_000..19_000).contains(&camera.progress.raw()))))
            || (camera.path.zone == fifth_post_checkpoint_hazard
                && ((camera.path.index == 0 && (9_000..19_000).contains(&camera.progress.raw()))
                    || (camera.path.index == 1
                        && (7_000..17_000).contains(&camera.progress.raw()))))
            || (camera.path.zone == sixth_post_checkpoint_hazard
                && ((camera.path.index == 0 && (6_500..18_000).contains(&camera.progress.raw()))
                    || (camera.path.index == 1
                        && (2_500..16_000).contains(&camera.progress.raw()))))
            || (camera.path.zone == seventh_post_checkpoint_hazard
                && ((camera.path.index == 0 && camera.progress.raw() < 16_000)
                    || (camera.path.index == 1
                        && (4_000..16_000).contains(&camera.progress.raw()))))
        {
            PAD_DOWN | PAD_CROSS
        } else {
            PAD_DOWN
        };
        // Once JunOC's boulder starts chasing, one-frame spin taps preserve
        // enough forward speed to clear it without masking the jump windows.
        if frame >= 1_323 && (frame - 1_323).is_multiple_of(18) {
            held | PAD_SQUARE
        } else {
            held
        }
    }
}

const UPSTREAM_PBAK_FRAMES: u32 = 934;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MovingPlatformTrace {
    translation: [i32; 3],
    state: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct UpstreamPlatformTraces {
    orbital: Option<MovingPlatformTrace>,
    first_zero_k: Option<MovingPlatformTrace>,
    second_zero_k: Option<MovingPlatformTrace>,
    zero_l: Option<MovingPlatformTrace>,
    zero_m: Option<MovingPlatformTrace>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum UpstreamRecoveryStage {
    #[default]
    SettleAtSpawn,
    CrossOpening,
    BoardLeaf,
    RideLeaf,
    TransferFromLeaf,
    WaitInZeroJ,
    CrossZeroKLeaves,
    JumpToSecondZeroKLeaf,
    WaitOnSecondZeroKLeaf,
    JumpToZeroLTerrain,
    WaitOnZeroLTerrain,
    JumpToZeroLLeaf,
    WaitOnZeroLLeaf,
    JumpToZeroLRock,
    WaitOnZeroLRock,
    SteerIntoZeroM,
    WaitInZeroM,
    CrossZeroMPathOne,
    WaitOnZeroMPathOne,
    CrossZeroMHazard,
    StableAtFirstCheckpoint,
}

/// Recovers from applying Upstream's mid-level PBAK input to the normal
/// post-Map spawn, then follows camera paths and live moving platforms into
/// 0n's first checkpoint.
///
/// Every Cross interval is bounded by an explicit release window. Platform
/// transfers are gated on Crash's landed state and live `RivOC` entities
/// rather than replaying a second absolute pad trace.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct UpstreamRecoveryRouteController {
    stage: UpstreamRecoveryStage,
    settle_frames: u8,
    opening_tick: u16,
    action_tick: u8,
    wait_tick: u8,
    leaf_reached_far_side: bool,
    leaf_completed_cycle: bool,
}

impl UpstreamRecoveryRouteController {
    const SETTLE_FRAMES: u8 = 67;
    const OPENING_ACTION_COUNT: u16 = 6;
    const OPENING_ACTION_CADENCE: u16 = 28;
    const LEAF_BANK_MINIMUM_TICK: u16 = 176;
    const ZERO_J_SETTLE_FRAMES: u8 = 22;
    const PLATFORM_LANDING_FRAMES: u8 = 22;
    const ZERO_L_LEAF_SETTLE_FRAMES: u8 = 28;
    const ZERO_L_ROCK_SETTLE_FRAMES: u8 = 20;
    const ZERO_M_SETTLE_FRAMES: u8 = 26;
    const ZERO_M_PATH_ONE_SETTLE_FRAMES: u8 = 28;

    const SHORT_JUMP: RouteAction = RouteAction {
        direction: PAD_UP,
        direction_frames: 16,
        button: PAD_CROSS,
        button_start: 2,
        button_frames: 16,
    };
    const FORWARD_JUMP: RouteAction = RouteAction {
        direction: PAD_UP,
        direction_frames: 27,
        button: PAD_CROSS,
        button_start: 13,
        button_frames: 15,
    };
    const LEAF_TRANSFER: RouteAction = RouteAction {
        direction: PAD_UP,
        direction_frames: 28,
        button: PAD_CROSS,
        button_start: 10,
        button_frames: 15,
    };
    const ZERO_J_TO_ZERO_K: RouteAction = RouteAction {
        direction: PAD_UP,
        direction_frames: 58,
        button: PAD_CROSS,
        button_start: 26,
        button_frames: 16,
    };
    const PLATFORM_JUMP: RouteAction = RouteAction {
        direction: PAD_UP,
        direction_frames: 20,
        button: PAD_CROSS,
        button_start: 6,
        button_frames: 16,
    };
    const ZERO_L_ROCK_JUMP: RouteAction = RouteAction {
        direction: PAD_UP,
        direction_frames: 28,
        button: PAD_CROSS,
        button_start: 10,
        button_frames: 16,
    };

    fn held(
        &mut self,
        camera: RetailCameraLocation,
        player: Option<PlayerTrace>,
        platforms: UpstreamPlatformTraces,
    ) -> u32 {
        match self.stage {
            UpstreamRecoveryStage::SettleAtSpawn => {
                if !Self::normal_spawn_bank_is_ready(camera, player) {
                    self.settle_frames = 0;
                    return 0;
                }
                if self.settle_frames < Self::SETTLE_FRAMES {
                    self.settle_frames += 1;
                    return 0;
                }
                self.stage = UpstreamRecoveryStage::CrossOpening;
                self.opening_held()
            }
            UpstreamRecoveryStage::CrossOpening => {
                if self.opening_tick >= Self::LEAF_BANK_MINIMUM_TICK
                    && Self::leaf_bank_is_ready(camera, player, platforms.orbital)
                {
                    self.stage = UpstreamRecoveryStage::BoardLeaf;
                    self.action_tick = 0;
                    return self.advance_boarding_action();
                }
                self.opening_held()
            }
            UpstreamRecoveryStage::BoardLeaf => self.advance_boarding_action(),
            UpstreamRecoveryStage::RideLeaf => {
                let leaf = platforms.orbital;
                if leaf.is_some_and(|leaf| leaf.state == 9 && leaf.translation[2] <= 21_220_000) {
                    self.leaf_reached_far_side = true;
                }
                if self.leaf_reached_far_side
                    && leaf.is_some_and(|leaf| leaf.state == 9 && leaf.translation[2] >= 21_800_000)
                {
                    self.leaf_completed_cycle = true;
                }
                if self.leaf_completed_cycle && Self::leaf_transfer_is_ready(camera, player, leaf) {
                    self.stage = UpstreamRecoveryStage::TransferFromLeaf;
                    self.action_tick = 0;
                    return self.advance_transfer_action();
                }
                0
            }
            UpstreamRecoveryStage::TransferFromLeaf => self.advance_transfer_action(),
            UpstreamRecoveryStage::WaitInZeroJ => {
                if self.wait_tick < Self::ZERO_J_SETTLE_FRAMES {
                    self.wait_tick += 1;
                    return 0;
                }
                if Self::zero_j_bank_is_ready(camera, player, platforms.first_zero_k) {
                    return self.start_action(
                        UpstreamRecoveryStage::CrossZeroKLeaves,
                        Self::ZERO_J_TO_ZERO_K,
                    );
                }
                0
            }
            UpstreamRecoveryStage::CrossZeroKLeaves => self.advance_action(
                Self::ZERO_J_TO_ZERO_K,
                UpstreamRecoveryStage::JumpToSecondZeroKLeaf,
            ),
            UpstreamRecoveryStage::JumpToSecondZeroKLeaf => self.advance_action(
                Self::PLATFORM_JUMP,
                UpstreamRecoveryStage::WaitOnSecondZeroKLeaf,
            ),
            UpstreamRecoveryStage::WaitOnSecondZeroKLeaf => {
                if self.wait_tick < Self::PLATFORM_LANDING_FRAMES {
                    self.wait_tick += 1;
                    return 0;
                }
                if Self::second_zero_k_leaf_is_ready(camera, player, platforms.second_zero_k) {
                    return self.start_action(
                        UpstreamRecoveryStage::JumpToZeroLTerrain,
                        Self::PLATFORM_JUMP,
                    );
                }
                0
            }
            UpstreamRecoveryStage::JumpToZeroLTerrain => self.advance_action(
                Self::PLATFORM_JUMP,
                UpstreamRecoveryStage::WaitOnZeroLTerrain,
            ),
            UpstreamRecoveryStage::WaitOnZeroLTerrain => {
                if self.wait_tick < Self::PLATFORM_LANDING_FRAMES {
                    self.wait_tick += 1;
                    return 0;
                }
                if Self::zero_l_terrain_is_ready(camera, player) {
                    return self
                        .start_action(UpstreamRecoveryStage::JumpToZeroLLeaf, Self::PLATFORM_JUMP);
                }
                0
            }
            UpstreamRecoveryStage::JumpToZeroLLeaf => {
                self.advance_action(Self::PLATFORM_JUMP, UpstreamRecoveryStage::WaitOnZeroLLeaf)
            }
            UpstreamRecoveryStage::WaitOnZeroLLeaf => {
                if self.wait_tick < Self::ZERO_L_LEAF_SETTLE_FRAMES {
                    self.wait_tick += 1;
                    return 0;
                }
                if Self::zero_l_leaf_is_ready(camera, player, platforms.zero_l) {
                    return self.start_action(
                        UpstreamRecoveryStage::JumpToZeroLRock,
                        Self::ZERO_L_ROCK_JUMP,
                    );
                }
                0
            }
            UpstreamRecoveryStage::JumpToZeroLRock => self.advance_action(
                Self::ZERO_L_ROCK_JUMP,
                UpstreamRecoveryStage::WaitOnZeroLRock,
            ),
            UpstreamRecoveryStage::WaitOnZeroLRock => {
                if self.wait_tick < Self::ZERO_L_ROCK_SETTLE_FRAMES {
                    self.wait_tick += 1;
                    return 0;
                }
                if Self::zero_l_rock_is_ready(camera, player) {
                    self.stage = UpstreamRecoveryStage::SteerIntoZeroM;
                    self.action_tick = 0;
                    return self.advance_zero_m_action(8, UpstreamRecoveryStage::WaitInZeroM);
                }
                0
            }
            UpstreamRecoveryStage::SteerIntoZeroM => {
                self.advance_zero_m_action(8, UpstreamRecoveryStage::WaitInZeroM)
            }
            UpstreamRecoveryStage::WaitInZeroM => {
                if self.wait_tick < Self::ZERO_M_SETTLE_FRAMES {
                    self.wait_tick += 1;
                    return 0;
                }
                if Self::zero_m_bank_is_ready(camera, player) {
                    self.stage = UpstreamRecoveryStage::CrossZeroMPathOne;
                    self.action_tick = 0;
                    return self
                        .advance_zero_m_action(6, UpstreamRecoveryStage::WaitOnZeroMPathOne);
                }
                0
            }
            UpstreamRecoveryStage::CrossZeroMPathOne => {
                self.advance_zero_m_action(6, UpstreamRecoveryStage::WaitOnZeroMPathOne)
            }
            UpstreamRecoveryStage::WaitOnZeroMPathOne => {
                if self.wait_tick < Self::ZERO_M_PATH_ONE_SETTLE_FRAMES {
                    self.wait_tick += 1;
                    return 0;
                }
                if Self::zero_m_hazard_bank_is_ready(camera, player, platforms.zero_m) {
                    self.stage = UpstreamRecoveryStage::CrossZeroMHazard;
                    self.action_tick = 0;
                    return self.advance_zero_m_hazard_clearance();
                }
                0
            }
            UpstreamRecoveryStage::CrossZeroMHazard => self.advance_zero_m_hazard_clearance(),
            UpstreamRecoveryStage::StableAtFirstCheckpoint => 0,
        }
    }

    fn opening_held(&mut self) -> u32 {
        let action_index = self.opening_tick / Self::OPENING_ACTION_CADENCE;
        let action_tick = u8::try_from(self.opening_tick % Self::OPENING_ACTION_CADENCE)
            .expect("Upstream action cadence fits u8");
        self.opening_tick = self.opening_tick.saturating_add(1);
        match action_index {
            0 | 1 => Self::SHORT_JUMP.held(action_tick),
            2..Self::OPENING_ACTION_COUNT => Self::FORWARD_JUMP.held(action_tick),
            _ => 0,
        }
    }

    fn advance_boarding_action(&mut self) -> u32 {
        let held = Self::SHORT_JUMP.held(self.action_tick);
        self.action_tick = self.action_tick.saturating_add(1);
        if self.action_tick >= Self::SHORT_JUMP.total_frames() {
            self.stage = UpstreamRecoveryStage::RideLeaf;
            self.action_tick = 0;
        }
        held
    }

    fn advance_transfer_action(&mut self) -> u32 {
        let held = Self::LEAF_TRANSFER.held(self.action_tick);
        self.action_tick = self.action_tick.saturating_add(1);
        if self.action_tick >= Self::LEAF_TRANSFER.total_frames() {
            self.stage = UpstreamRecoveryStage::WaitInZeroJ;
            self.action_tick = 0;
            self.wait_tick = 0;
        }
        held
    }

    fn start_action(&mut self, stage: UpstreamRecoveryStage, action: RouteAction) -> u32 {
        self.stage = stage;
        self.action_tick = 1;
        self.wait_tick = 0;
        action.held(0)
    }

    fn advance_action(&mut self, action: RouteAction, next: UpstreamRecoveryStage) -> u32 {
        let held = action.held(self.action_tick);
        self.action_tick = self.action_tick.saturating_add(1);
        if self.action_tick >= action.total_frames() {
            self.stage = next;
            self.action_tick = 0;
            self.wait_tick = 0;
        }
        held
    }

    fn advance_zero_m_action(&mut self, cross_start: u8, next: UpstreamRecoveryStage) -> u32 {
        let tick = self.action_tick;
        let mut held = 0;
        if tick < 20 {
            held |= PAD_UP;
        }
        if tick < 10 {
            held |= PAD_LEFT;
        }
        if (cross_start..cross_start + 16).contains(&tick) {
            held |= PAD_CROSS;
        }
        self.action_tick = self.action_tick.saturating_add(1);
        if self.action_tick >= cross_start.saturating_add(16).max(20) {
            self.stage = next;
            self.action_tick = 0;
            self.wait_tick = 0;
        }
        held
    }

    fn advance_zero_m_hazard_clearance(&mut self) -> u32 {
        let tick = self.action_tick;
        let mut held = 0;
        if tick < 64 {
            held |= PAD_UP;
        }
        if tick < 20 {
            held |= PAD_RIGHT;
        }
        // RivOC 55 re-enters its lethal contact cycle unless each native
        // 18-frame spin window receives a fresh Square edge. One-frame taps
        // preserve the releases required to generate those edges.
        if tick.is_multiple_of(18) {
            held |= PAD_SQUARE;
        }
        if (6..22).contains(&tick) {
            held |= PAD_CROSS;
        }
        self.action_tick = self.action_tick.saturating_add(1);
        if self.action_tick >= 64 {
            self.stage = UpstreamRecoveryStage::StableAtFirstCheckpoint;
            self.action_tick = 0;
            self.wait_tick = 0;
        }
        held
    }

    fn normal_spawn_bank_is_ready(
        camera: RetailCameraLocation,
        player: Option<PlayerTrace>,
    ) -> bool {
        let zone = Eid::from_name("0f_fZ").expect("fixed Upstream spawn-camera EID is valid");
        camera.path.zone == zone
            && camera.path.index == 0
            && camera.progress.raw() >= 7_000
            && player.is_some_and(|player| {
                Self::player_is_landed(player)
                    && player.translation[2] >= 25_000_000
                    && player.translation[1] >= 1_700_000
            })
    }

    fn leaf_bank_is_ready(
        camera: RetailCameraLocation,
        player: Option<PlayerTrace>,
        leaf: Option<MovingPlatformTrace>,
    ) -> bool {
        let zone = Eid::from_name("0i_fZ").expect("fixed Upstream leaf-camera EID is valid");
        camera.path.zone == zone
            && camera.path.index == 0
            && camera.progress.raw() >= 8_000
            && player.is_some_and(|player| {
                Self::player_is_landed(player)
                    && (22_150_000..=22_190_000).contains(&player.translation[2])
            })
            && leaf.is_some_and(|leaf| leaf.state == 9)
    }

    fn leaf_transfer_is_ready(
        camera: RetailCameraLocation,
        player: Option<PlayerTrace>,
        leaf: Option<MovingPlatformTrace>,
    ) -> bool {
        let zone = Eid::from_name("0i_fZ").expect("fixed Upstream leaf-camera EID is valid");
        let (Some(player), Some(leaf)) = (player, leaf) else {
            return false;
        };
        camera.path.zone == zone
            && camera.path.index == 1
            && camera.progress.raw() >= 11_900
            // State one is Crash's settled standing state on the leaf. Its
            // earlier pass through the same orbit still uses landing state
            // ten and must not trigger the transfer before the controller has
            // also observed the leaf's far-side and bank-side endpoints.
            && player.state == 1
            && leaf.state == 9
            && (2_250_000..=2_280_000).contains(&leaf.translation[0])
            && (21_300_000..=21_360_000).contains(&leaf.translation[2])
            && player.translation[0].abs_diff(leaf.translation[0]) <= 4_096
            && player.translation[2].abs_diff(leaf.translation[2]) <= 4_096
    }

    fn zero_j_bank_is_ready(
        camera: RetailCameraLocation,
        player: Option<PlayerTrace>,
        first_leaf: Option<MovingPlatformTrace>,
    ) -> bool {
        let zone = Eid::from_name("0j_fZ").expect("fixed Upstream 0j camera EID is valid");
        camera.path.zone == zone
            && camera.path.index == 0
            && camera.progress.raw() >= 12_000
            && player.is_some_and(|player| {
                player.state == 1 && (20_650_000..=20_720_000).contains(&player.translation[2])
            })
            && first_leaf.is_some_and(|leaf| {
                leaf.state == 11
                    && leaf.translation[0].abs_diff(2_252_800) <= 4_096
                    && leaf.translation[2].abs_diff(19_660_544) <= 4_096
            })
    }

    fn second_zero_k_leaf_is_ready(
        camera: RetailCameraLocation,
        player: Option<PlayerTrace>,
        second_leaf: Option<MovingPlatformTrace>,
    ) -> bool {
        let zone = Eid::from_name("0k_fZ").expect("fixed Upstream 0k camera EID is valid");
        let (Some(player), Some(leaf)) = (player, second_leaf) else {
            return false;
        };
        camera.path.zone == zone
            && camera.path.index == 1
            && camera.progress.raw() >= 8_500
            && player.state == 1
            && leaf.state == 12
            && player.translation[0].abs_diff(leaf.translation[0]) <= 16_000
            && player.translation[2].abs_diff(leaf.translation[2]) <= 24_000
    }

    fn zero_l_terrain_is_ready(camera: RetailCameraLocation, player: Option<PlayerTrace>) -> bool {
        let zone = Eid::from_name("0l_fZ").expect("fixed Upstream 0l camera EID is valid");
        camera.path.zone == zone
            && camera.path.index == 0
            && camera.progress.raw() >= 2_400
            && player.is_some_and(|player| {
                player.state == 1 && (18_680_000..=18_730_000).contains(&player.translation[2])
            })
    }

    fn zero_l_leaf_is_ready(
        camera: RetailCameraLocation,
        player: Option<PlayerTrace>,
        zero_l_leaf: Option<MovingPlatformTrace>,
    ) -> bool {
        let zone = Eid::from_name("0l_fZ").expect("fixed Upstream 0l camera EID is valid");
        let (Some(player), Some(leaf)) = (player, zero_l_leaf) else {
            return false;
        };
        camera.path.zone == zone
            && camera.path.index == 0
            && camera.progress.raw() >= 14_500
            && player.state == 1
            && leaf.state == 12
            && player.translation[0].abs_diff(leaf.translation[0]) <= 130_000
            && player.translation[2].abs_diff(leaf.translation[2]) <= 120_000
    }

    fn zero_l_rock_is_ready(camera: RetailCameraLocation, player: Option<PlayerTrace>) -> bool {
        let zone = Eid::from_name("0l_fZ").expect("fixed Upstream 0l camera EID is valid");
        camera.path.zone == zone
            && camera.path.index == 1
            && camera.progress.raw() >= 12_000
            && player.is_some_and(|player| {
                player.state == 1 && (17_620_000..=17_700_000).contains(&player.translation[2])
            })
    }

    fn zero_m_bank_is_ready(camera: RetailCameraLocation, player: Option<PlayerTrace>) -> bool {
        let zone = Eid::from_name("0m_fZ").expect("fixed Upstream 0m camera EID is valid");
        camera.path.zone == zone
            && camera.path.index == 0
            && camera.progress.raw() >= 9_900
            && player.is_some_and(|player| {
                player.state == 1
                    && (2_100_000..=2_150_000).contains(&player.translation[0])
                    && (17_150_000..=17_220_000).contains(&player.translation[2])
            })
    }

    fn zero_m_hazard_bank_is_ready(
        camera: RetailCameraLocation,
        player: Option<PlayerTrace>,
        hazard: Option<MovingPlatformTrace>,
    ) -> bool {
        let zone = Eid::from_name("0m_fZ").expect("fixed Upstream 0m camera EID is valid");
        camera.path.zone == zone
            && camera.path.index == 1
            && camera.progress.raw() >= 5_000
            && player.is_some_and(|player| {
                player.state == 1
                    && (1_970_000..=2_020_000).contains(&player.translation[0])
                    && (16_700_000..=16_760_000).contains(&player.translation[2])
            })
            && hazard.is_some_and(|hazard| {
                hazard.state == 14
                    && hazard.translation[0].abs_diff(2_139_136) <= 4_096
                    && hazard.translation[1].abs_diff(1_793_792) <= 4_096
                    && hazard.translation[2].abs_diff(16_699_392) <= 4_096
            })
    }

    const fn player_is_landed(player: PlayerTrace) -> bool {
        matches!(player.state, 1 | 10 | 13)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SurveyInputController {
    profile: SurveyInputProfile,
    n_sanity: NSanityRouteController,
    jungle: JungleRouteController,
    great_gate: GreatGateRouteController,
    boulders: BouldersCompletionRouteController,
    upstream: UpstreamRecoveryRouteController,
}

impl SurveyInputController {
    const fn new(profile: SurveyInputProfile) -> Self {
        Self {
            profile,
            n_sanity: NSanityRouteController {
                opening_stage: 0,
                stage: 0,
                active: None,
                active_is_opening: false,
                action_tick: 0,
            },
            jungle: JungleRouteController {
                stage: 0,
                active: None,
                active_uses_forward: false,
                action_tick: 0,
            },
            great_gate: GreatGateRouteController {
                yellow_gem_route: matches!(
                    profile,
                    SurveyInputProfile::GreatGateYellowGemExactCarry
                ),
                opening_stage: 0,
                opening_ready_frames: 0,
                stage: 0,
                active: None,
                action_tick: 0,
                pickup_wait_frames: 0,
            },
            boulders: BouldersCompletionRouteController {
                zero_t_takeoff_fired: false,
            },
            upstream: UpstreamRecoveryRouteController {
                stage: UpstreamRecoveryStage::SettleAtSpawn,
                settle_frames: 0,
                opening_tick: 0,
                action_tick: 0,
                wait_tick: 0,
                leaf_reached_far_side: false,
                leaf_completed_cycle: false,
            },
        }
    }

    fn held(
        &mut self,
        frame: u32,
        camera: RetailCameraLocation,
        player: Option<PlayerTrace>,
        checkpoint_id: i32,
        local_pbak_held: Option<u32>,
        upstream_platforms: UpstreamPlatformTraces,
    ) -> u32 {
        match self.profile {
            SurveyInputProfile::Idle => 0,
            SurveyInputProfile::DirectionAndButtonSweep
            | SurveyInputProfile::DirectionAndButtonSweepToTransition => active_survey_held(frame),
            SurveyInputProfile::ForwardWithActions => self.n_sanity.held(camera, player),
            SurveyInputProfile::ForwardThroughCheckpointThenA8Hit => {
                let a8 = Eid::from_name("a8_9Z").expect("fixed N. Sanity route EID is valid");
                if checkpoint_id > 0 && camera.path.zone == a8 {
                    PAD_UP
                } else {
                    self.n_sanity.held(camera, player)
                }
            }
            SurveyInputProfile::JunglePhaseRobust => self.jungle.held(camera, player),
            SurveyInputProfile::GreatGatePhaseRobust => self.great_gate.held(camera, player, false),
            SurveyInputProfile::GreatGateTawnaBonus => self.great_gate.held(camera, player, true),
            SurveyInputProfile::GreatGateYellowGemExactCarry => {
                self.great_gate.held(camera, player, false)
            }
            SurveyInputProfile::LocalPbakPrefix => local_pbak_held
                .expect("the legally local PBAK prefix is loaded before frame execution"),
            SurveyInputProfile::BouldersCompletionRoute => {
                self.boulders.held(frame, camera, player, local_pbak_held)
            }
            SurveyInputProfile::UpstreamCarriedRecovery => {
                if frame <= UPSTREAM_PBAK_FRAMES {
                    local_pbak_held
                        .expect("the legally local Upstream PBAK prefix covers its authored frames")
                } else {
                    self.upstream.held(camera, player, upstream_platforms)
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CameraProgressRange {
    first_frame: u32,
    last_frame: u32,
    minimum: i32,
    maximum: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PlayerTrace {
    program: Option<GoolProgramIdentity>,
    code_address: CodeAddress,
    zone: Eid,
    translation: [i32; 3],
    velocity: [i32; 3],
    state: u16,
    state_flags: u32,
    status_a: u32,
    status_b: u32,
    tawna_counter: u32,
    event: u32,
    animation_stamp: u32,
    state_stamp: u32,
    animation_counter: u32,
    animation_sequence: u32,
    animation_frame: u32,
    stack_len: usize,
    stack_top: Option<u32>,
    stack_tail: [Option<u32>; 8],
}

#[derive(Debug)]
struct OwnedZone {
    eid: Eid,
    entities: Vec<ZoneEntity>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PagingTraceEntry {
    frame: u32,
    object: VmObjectHandle,
    operation: PagingHostOperation,
    eid: Eid,
    page: PageIndex,
}

#[derive(Debug)]
struct LevelSurvey {
    level: LevelId,
    name: &'static str,
    input_profile: SurveyInputProfile,
    frames: u32,
    terminal: Option<String>,
    zone_transitions: u64,
    restarts: u64,
    restart_frames: Vec<u32>,
    save_handshakes: u64,
    spawn_attempts: u64,
    successful_spawns: u64,
    expected_spawn_rejections: u64,
    unexpected_spawn_errors: u64,
    executions: u64,
    execution_errors: u64,
    max_live_objects: usize,
    final_live_objects: usize,
    faulted_objects: usize,
    effect_counts: BTreeMap<&'static str, u64>,
    first_effect_samples: BTreeMap<&'static str, String>,
    paging_trace: Vec<PagingTraceEntry>,
    issue_counts: BTreeMap<&'static str, u64>,
    first_issue: Option<String>,
    fault_contexts: BTreeSet<String>,
    initial_camera: Option<RetailCameraLocation>,
    final_camera: Option<RetailCameraLocation>,
    camera_ranges: BTreeMap<RetailPathId, CameraProgressRange>,
    camera_path_changes: u64,
    last_camera_path_change: u32,
    last_camera_progress_change: u32,
    death_camera_frames: u64,
    death_camera_pose_changes: u64,
    death_camera_max_count: i32,
    first_death_camera_pose: Option<(u32, RetailCameraPose)>,
    last_death_camera_pose: Option<(u32, RetailCameraPose)>,
    initial_player_translation: Option<[i32; 3]>,
    final_player_translation: Option<[i32; 3]>,
    player_minimum: Option<[i32; 3]>,
    player_maximum: Option<[i32; 3]>,
    last_player_movement: u32,
    first_below_zero: Option<(u32, PlayerTrace)>,
    first_terminal_fall: Option<(u32, PlayerTrace)>,
    progression_samples: Vec<String>,
    box_count_samples: Vec<(u32, i32)>,
    checkpoint_samples: Vec<(u32, i32, [i32; 3])>,
    saved_box_count_samples: Vec<(u32, i32)>,
    spawn_flag_samples: Vec<(u32, u16, u32)>,
    observed_program_states: BTreeSet<(Eid, u16)>,
    early_direct_send_samples: Vec<(u32, u32)>,
    next_lid: Option<(u32, i32)>,
}

impl LevelSurvey {
    fn new(level: LevelId, name: &'static str, input_profile: SurveyInputProfile) -> Self {
        Self {
            level,
            name,
            input_profile,
            frames: 0,
            terminal: None,
            zone_transitions: 0,
            restarts: 0,
            restart_frames: Vec::new(),
            save_handshakes: 0,
            spawn_attempts: 0,
            successful_spawns: 0,
            expected_spawn_rejections: 0,
            unexpected_spawn_errors: 0,
            executions: 0,
            execution_errors: 0,
            max_live_objects: 0,
            final_live_objects: 0,
            faulted_objects: 0,
            effect_counts: BTreeMap::new(),
            first_effect_samples: BTreeMap::new(),
            paging_trace: Vec::new(),
            issue_counts: BTreeMap::new(),
            first_issue: None,
            fault_contexts: BTreeSet::new(),
            initial_camera: None,
            final_camera: None,
            camera_ranges: BTreeMap::new(),
            camera_path_changes: 0,
            last_camera_path_change: 0,
            last_camera_progress_change: 0,
            death_camera_frames: 0,
            death_camera_pose_changes: 0,
            death_camera_max_count: 0,
            first_death_camera_pose: None,
            last_death_camera_pose: None,
            initial_player_translation: None,
            final_player_translation: None,
            player_minimum: None,
            player_maximum: None,
            last_player_movement: 0,
            first_below_zero: None,
            first_terminal_fall: None,
            progression_samples: Vec::new(),
            box_count_samples: Vec::new(),
            checkpoint_samples: Vec::new(),
            saved_box_count_samples: Vec::new(),
            spawn_flag_samples: Vec::new(),
            observed_program_states: BTreeSet::new(),
            early_direct_send_samples: Vec::new(),
            next_lid: None,
        }
    }

    fn observe_progress(
        &mut self,
        frame: u32,
        camera: RetailCameraLocation,
        player: Option<PlayerTrace>,
    ) {
        let previous_camera = self.final_camera;
        self.initial_camera.get_or_insert(camera);
        self.final_camera = Some(camera);
        let range = self
            .camera_ranges
            .entry(camera.path)
            .or_insert(CameraProgressRange {
                first_frame: frame,
                last_frame: frame,
                minimum: camera.progress.raw(),
                maximum: camera.progress.raw(),
            });
        range.last_frame = frame;
        range.minimum = range.minimum.min(camera.progress.raw());
        range.maximum = range.maximum.max(camera.progress.raw());
        let path_changed = previous_camera.is_some_and(|before| before.path != camera.path);
        if path_changed {
            self.camera_path_changes += 1;
            self.last_camera_path_change = frame;
        }
        if previous_camera.is_none_or(|before| before != camera) {
            self.last_camera_progress_change = frame;
        }

        if let Some(player) = player {
            if player.translation[1] < 0 {
                self.first_below_zero.get_or_insert((frame, player));
            }
            if player.velocity[1] == -0x2e_e000 {
                self.first_terminal_fall.get_or_insert((frame, player));
            }
            self.initial_player_translation
                .get_or_insert(player.translation);
            if self
                .final_player_translation
                .is_none_or(|previous| previous != player.translation)
            {
                self.last_player_movement = frame;
            }
            self.final_player_translation = Some(player.translation);
            let minimum = self.player_minimum.get_or_insert(player.translation);
            let maximum = self.player_maximum.get_or_insert(player.translation);
            for axis in 0..3 {
                minimum[axis] = minimum[axis].min(player.translation[axis]);
                maximum[axis] = maximum[axis].max(player.translation[axis]);
            }
        }

        if (frame == 1 || frame.is_multiple_of(300) || path_changed)
            && self.progression_samples.len() < 64
        {
            self.progression_samples.push(format!(
                "f{frame}:{}:{}@{} player={player:?}",
                camera.path.zone,
                camera.path.index,
                camera.progress.raw(),
            ));
        }
    }

    fn record_issue(&mut self, kind: &'static str, frame: u32, detail: impl Into<String>) {
        *self.issue_counts.entry(kind).or_default() += 1;
        let detail = detail.into();
        if self.first_issue.is_none() {
            self.first_issue = Some(format!("frame {frame} {kind}: {detail}"));
        }
    }

    fn observe_death_camera(&mut self, frame: u32, pose: RetailCameraPose, count: i32) {
        self.death_camera_frames += 1;
        self.death_camera_max_count = self.death_camera_max_count.max(count);
        self.first_death_camera_pose.get_or_insert((frame, pose));
        if self
            .last_death_camera_pose
            .is_some_and(|(_, previous)| previous != pose)
        {
            self.death_camera_pose_changes += 1;
        }
        self.last_death_camera_pose = Some((frame, pose));
    }

    fn record_effect(&mut self, frame: u32, effect: &VmEffect) {
        let kind = effect_kind(effect);
        *self.effect_counts.entry(kind).or_default() += 1;
        self.first_effect_samples
            .entry(kind)
            .or_insert_with(|| format!("{effect:?}"));
        if let VmEffect::Paging {
            object,
            operation,
            eid,
            page,
            ..
        } = effect
        {
            self.paging_trace.push(PagingTraceEntry {
                frame,
                object: *object,
                operation: *operation,
                eid: *eid,
                page: *page,
            });
        }
    }

    fn summary(&self) -> String {
        format!(
            "{} ({}): input={} frames={} terminal={:?} live={}/max{} faulted={} spawns={}/{}/{} expected-reject={} executions={} errors={} zone-transitions={} restarts={:?} saves={} next-lid={:?} camera={:?}->{:?} paths={} path-changes={} last-path-change={} last-progress={} death-camera=frames{} changes{} max-count{} {:?}->{:?} player={:?}->{:?} bounds={:?}..{:?} last-movement={} first-below-zero={:?} first-terminal-fall={:?} samples={:?} boxes={:?} checkpoints={:?} saved-boxes={:?} spawn-flags={:?} early-direct-sends={:?} effects={:?} first-effects={:?} issues={:?} first={:?} fault-contexts={:?}",
            self.name,
            self.level,
            self.input_profile.label(),
            self.frames,
            self.terminal,
            self.final_live_objects,
            self.max_live_objects,
            self.faulted_objects,
            self.successful_spawns,
            self.spawn_attempts,
            self.unexpected_spawn_errors,
            self.expected_spawn_rejections,
            self.executions,
            self.execution_errors,
            self.zone_transitions,
            self.restart_frames,
            self.save_handshakes,
            self.next_lid,
            self.initial_camera,
            self.final_camera,
            self.camera_ranges.len(),
            self.camera_path_changes,
            self.last_camera_path_change,
            self.last_camera_progress_change,
            self.death_camera_frames,
            self.death_camera_pose_changes,
            self.death_camera_max_count,
            self.first_death_camera_pose,
            self.last_death_camera_pose,
            self.initial_player_translation,
            self.final_player_translation,
            self.player_minimum,
            self.player_maximum,
            self.last_player_movement,
            self.first_below_zero,
            self.first_terminal_fall,
            self.progression_samples,
            self.box_count_samples,
            self.checkpoint_samples,
            self.saved_box_count_samples,
            self.spawn_flag_samples,
            self.early_direct_send_samples,
            self.effect_counts,
            self.first_effect_samples,
            self.issue_counts,
            self.first_issue,
            self.fault_contexts,
        )
    }

    fn is_clean(&self) -> bool {
        self.issue_counts.is_empty() && self.faulted_objects == 0
    }
}

fn effect_kind(effect: &VmEffect) -> &'static str {
    match effect {
        VmEffect::Event { .. } => "event",
        VmEffect::SendEvent(_) => "send-event",
        VmEffect::Solid { .. } => "solid",
        VmEffect::StateChanged { .. } => "state-changed",
        VmEffect::AudioStart { .. } => "audio-start",
        VmEffect::AudioControl { .. } => "audio-control",
        VmEffect::MidiTogglePlayback { .. } => "midi-toggle",
        VmEffect::ResetMasterFadeStep { .. } => "master-fade-reset",
        VmEffect::ResetLevelGlobals { .. } => "level-globals-reset",
        VmEffect::Paging { .. } => "paging",
        VmEffect::SpawnChildren { .. } => "spawn-children",
        VmEffect::FindSpawnedObject { .. } => "find-spawned-object",
        VmEffect::FindNearestObject { .. } => "find-nearest-object",
        VmEffect::SpawnFlagsChanged { .. } => "spawn-flags",
        VmEffect::TransformModelVertex { .. } => "transform-model-vertex",
        VmEffect::SetObjectZoneToTransitionTarget { .. } => "set-object-zone",
        VmEffect::TerminateCurrentZoneNeighbors { .. } => "terminate-neighbors",
        VmEffect::SetLinkZoneFromPoint { .. } => "set-link-zone",
        VmEffect::ReparentToRoot { .. } => "reparent",
        VmEffect::AnimationSelected { .. } => "animation-selected",
        VmEffect::AnimationFrameChanged { .. } => "animation-frame",
        VmEffect::Transition(_) => "transition",
        VmEffect::SaveState(_) => "save-state",
        VmEffect::LoadState { .. } => "load-state",
    }
}

fn graph_for_pair(
    level: LevelId,
    nsd: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
) -> Result<RetailZoneGraph, String> {
    let direct_roots = (level == LevelId::TITLE)
        .then(|| {
            TITLE_DIRECT_ZONES
                .map(|name| Eid::from_name(name).expect("fixed retail title EID is valid"))
        })
        .into_iter()
        .flatten();
    RetailZoneGraph::from_pair_with_roots(nsd, nsf, nsf_bytes, direct_roots)
        .map_err(|error| error.to_string())
}

fn zone_catalog(
    nsd: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    graph: &RetailZoneGraph,
    level: LevelId,
) -> Result<(BTreeMap<Eid, OwnedZone>, ZoneLifecycle), String> {
    let mut zones = BTreeMap::new();
    let mut lifecycle_zones = Vec::with_capacity(graph.zone_count());
    for node in graph.zones() {
        let entry = nsf
            .resolve_entry(nsd, node.eid)
            .map_err(|error| format!("ZDAT {}: {error}", node.eid))?;
        let header = ZoneHeader::parse(
            entry
                .item(0)
                .ok_or_else(|| format!("ZDAT {} has no header", node.eid))?
                .bytes(nsf_bytes)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("ZDAT {} header: {error}", node.eid))?;
        let mut entities = Vec::with_capacity(header.entity_count as usize);
        for entity_index in 0..header.entity_count {
            let item_index = header
                .entity_item_index(entity_index)
                .and_then(|index| usize::try_from(index).ok())
                .ok_or_else(|| format!("ZDAT {} entity {entity_index} item is absent", node.eid))?;
            let entity = ZoneEntity::parse(
                entry
                    .item(item_index)
                    .ok_or_else(|| format!("ZDAT {} item {item_index} is absent", node.eid))?
                    .bytes(nsf_bytes)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| format!("ZDAT {} entity {entity_index}: {error}", node.eid))?;
            entities.push(entity);
        }
        lifecycle_zones.push(ZoneLifecycleZone::new(
            node.eid,
            header.display_flags,
            header.neighbors,
            OrderedZoneLoadList::from(&header.load_list),
        ));
        zones.insert(
            node.eid,
            OwnedZone {
                eid: node.eid,
                entities,
            },
        );
    }
    let mut lifecycle = ZoneLifecycle::new(lifecycle_zones).map_err(|error| error.to_string())?;
    lifecycle
        .transition_with_marker(graph.spawn_path().zone, level != LevelId::TITLE)
        .map_err(|error| error.to_string())?;
    Ok((zones, lifecycle))
}

fn refresh_level_context(
    runtime: &mut RetailRuntime,
    graph: &RetailZoneGraph,
    lifecycle: &ZoneLifecycle,
    location: RetailCameraLocation,
) -> Result<(), String> {
    let graphics_flags = graph
        .zone(location.path.zone)
        .ok_or_else(|| format!("camera zone {} is absent", location.path.zone))?
        .graphics_flags;
    let old = runtime.level_state_context().cloned();
    runtime.set_level_state_context(RetailLevelStateContext {
        location,
        graphics_flags,
        box_count: old.as_ref().map_or(0, |state| state.box_count),
        checkpoint_id: old.as_ref().map_or(-1, |state| state.checkpoint_id),
        checkpoint_translation: old
            .as_ref()
            .map_or([0; 3], |state| state.checkpoint_translation),
        first_spawn: old.as_ref().is_some_and(|state| state.first_spawn),
        active_neighbor_zones: lifecycle.active_neighbor_zones(),
    });
    Ok(())
}

fn seed_mounted_level_context_from_globals(
    runtime: &mut RetailRuntime,
    graph: &RetailZoneGraph,
    lifecycle: &ZoneLifecycle,
    location: RetailCameraLocation,
) -> Result<(), String> {
    let graphics_flags = graph
        .zone(location.path.zone)
        .ok_or_else(|| format!("camera zone {} is absent", location.path.zone))?
        .graphics_flags;
    let read = |index| {
        runtime
            .global_word(index)
            .map(u32::cast_signed)
            .map_err(|error| format!("mounted retail global {index}: {error:?}"))
    };
    runtime.set_level_state_context(RetailLevelStateContext {
        location,
        graphics_flags,
        box_count: read(BOX_COUNT_GLOBAL)?,
        checkpoint_id: read(CHECKPOINT_ID_GLOBAL)?,
        checkpoint_translation: [
            read(CHECKPOINT_TRANSLATION_GLOBALS[0])?,
            read(CHECKPOINT_TRANSLATION_GLOBALS[1])?,
            read(CHECKPOINT_TRANSLATION_GLOBALS[2])?,
        ],
        first_spawn: false,
        active_neighbor_zones: lifecycle.active_neighbor_zones(),
    });
    Ok(())
}

struct AuthoredTitleMapHarness<'assets> {
    nsd: &'assets Nsd,
    nsf: &'assets Nsf,
    nsf_bytes: &'assets [u8],
    graph: RetailZoneGraph,
    zones: BTreeMap<Eid, OwnedZone>,
    lifecycle: ZoneLifecycle,
    camera: RetailCameraRuntime,
    runtime: RetailRuntime,
    card: VirtualCard,
    frame: u32,
    held_previous: u32,
    held_previous_2: u32,
    tapped_previous: u32,
    transitions: Vec<(u32, i32)>,
}

impl<'assets> AuthoredTitleMapHarness<'assets> {
    fn fresh(nsd: &'assets Nsd, nsf: &'assets Nsf, nsf_bytes: &'assets [u8]) -> Self {
        let mut runtime = RetailRuntime::new_for_level(GLOBAL_WORDS, LevelId::TITLE);
        runtime
            .restore_card_save_data(SaveData {
                level_count: 1,
                initial_lives: 4 << 8,
                sfx_volume: 255,
                music_volume: 255,
                ..SaveData::default()
            })
            .expect("fresh map progression must come from the retail card payload path");
        Self::from_runtime(nsd, nsf, nsf_bytes, runtime)
    }

    fn from_session(
        nsd: &'assets Nsd,
        nsf: &'assets Nsf,
        nsf_bytes: &'assets [u8],
        carry: RetailSessionCarry,
    ) -> Self {
        let runtime = RetailRuntime::new_from_session(GLOBAL_WORDS, LevelId::TITLE, carry)
            .expect("Title Map must import the preceding authored session carry");
        Self::from_runtime(nsd, nsf, nsf_bytes, runtime)
    }

    fn from_runtime(
        nsd: &'assets Nsd,
        nsf: &'assets Nsf,
        nsf_bytes: &'assets [u8],
        mut runtime: RetailRuntime,
    ) -> Self {
        let graph =
            graph_for_pair(LevelId::TITLE, nsd, nsf, nsf_bytes).expect("Title graph must parse");
        let (zones, lifecycle) = zone_catalog(nsd, nsf, nsf_bytes, &graph, LevelId::TITLE)
            .expect("Title zone catalog must parse");
        let camera = RetailCameraRuntime::new(&graph).expect("Title camera must initialize");
        runtime
            .configure_retail_title(TitleScreen::Map, false)
            .expect("Title Map state must configure");
        let mut harness = Self {
            nsd,
            nsf,
            nsf_bytes,
            graph,
            zones,
            lifecycle,
            camera,
            runtime,
            card: VirtualCard::new(),
            frame: 0,
            held_previous: 0,
            held_previous_2: 0,
            tapped_previous: 0,
            transitions: Vec::new(),
        };
        harness.mount();
        harness
    }

    fn mount(&mut self) {
        let mut host = NsfProgramHost::new(self.nsd, self.nsf, self.nsf_bytes);
        let teardown = self
            .runtime
            .terminate_all_objects(&mut host)
            .expect("Title Map teardown must execute");
        assert!(
            teardown.event_failures.is_empty(),
            "Title Map teardown handlers must complete cleanly: {:?}",
            teardown.event_failures
        );
        self.runtime
            .set_global_word(NEXT_DISPLAY_GLOBAL, TITLE_MAP_DISPLAY_MASK)
            .expect("Title Map display mask global must exist");
        let zone = Eid::from_name("1a_pZ").expect("fixed Title Map zone EID is valid");
        let path = RetailPathId { zone, index: 0 };
        self.lifecycle
            .transition_with_marker(zone, true)
            .expect("Title Map lifecycle transition must execute");
        let camera_step = self
            .camera
            .level_update(&self.graph, path, 0, 2)
            .expect("Title Map camera LevelUpdate must execute");
        assert_eq!(camera_step.after.path, path);
        let graphics_flags = self
            .graph
            .zone(zone)
            .expect("Title Map zone must be present in the graph")
            .graphics_flags;
        self.runtime
            .set_level_state_context(RetailLevelStateContext {
                location: camera_step.after,
                graphics_flags,
                box_count: 0,
                checkpoint_id: -1,
                checkpoint_translation: [0; 3],
                first_spawn: false,
                active_neighbor_zones: self.lifecycle.active_neighbor_zones(),
            });
    }

    fn step(&mut self, held: u32) {
        self.frame += 1;
        self.runtime.set_frame_timing(34, 34);
        self.card.update();
        self.runtime
            .publish_card_state(self.card.published_state())
            .expect("Title Map must publish the card state before spawning");
        let neighbors = self
            .lifecycle
            .next_frame_spawn_scan()
            .iter()
            .map(|candidate| {
                let zone = self
                    .zones
                    .get(&candidate.zone)
                    .expect("Title Map spawn zone must be cataloged");
                NeighborZone {
                    eid: zone.eid,
                    display_flags: candidate.display_flags,
                    entities: zone.entities.as_slice(),
                }
            })
            .collect::<Vec<_>>();
        let attempts = {
            let mut host = NsfProgramHost::new(self.nsd, self.nsf, self.nsf_bytes);
            self.runtime
                .spawn_current_zone_neighbors(&neighbors, &mut host)
        };
        assert!(
            attempts.iter().all(|attempt| {
                attempt.result.is_ok()
                    || matches!(
                        attempt.result,
                        Err(RuntimeError::Spawn(
                            SpawnError::SpawnBlocked { .. } | SpawnError::MainObjectAlreadyActive
                        ))
                    )
            }),
            "Title Map frame {} spawn mismatch: {attempts:?}",
            self.frame
        );

        self.update_camera();
        let tapped = held & !self.held_previous;
        let snapshot = RetailPadSnapshot {
            tapped,
            held,
            tapped_previous: self.tapped_previous,
            held_previous: self.held_previous,
            held_previous_2: self.held_previous_2,
        };
        let mut host = NsfProgramHost::new(self.nsd, self.nsf, self.nsf_bytes);
        let report = self
            .runtime
            .run_frame_before_display_with_traversal_hook(
                &mut host,
                INSTRUCTION_BUDGET,
                |runtime, _host, _point| {
                    runtime
                        .set_pad_snapshot(0, snapshot)
                        .map_err(RuntimeError::Vm)
                },
            )
            .unwrap_or_else(|error| panic!("Title Map frame {} runtime: {error:?}", self.frame));
        self.held_previous_2 = self.held_previous;
        self.held_previous = held;
        self.tapped_previous = tapped;
        assert!(
            report
                .executions
                .iter()
                .all(|execution| execution.result.is_ok()),
            "Title Map frame {} execution mismatch: {:?}",
            self.frame,
            report
                .executions
                .iter()
                .filter(|execution| execution.result.is_err())
                .collect::<Vec<_>>()
        );
        self.transitions
            .extend(report.effects.iter().filter_map(|effect| match effect {
                VmEffect::Transition(level) => Some((self.frame, *level)),
                _ => None,
            }));
        let action = self
            .runtime
            .begin_retail_title_update()
            .expect("Title Map update must begin");
        assert_eq!(
            action, None,
            "Title Map must not request another title screen"
        );
        self.runtime
            .finish_retail_title_update()
            .expect("Title Map update must finish");
        self.runtime
            .finish_deferred_display_frame()
            .expect("Title Map display boundary must finish");
        assert_eq!(
            self.runtime.faulted_object_count(),
            0,
            "Title Map frame {} retained a faulted object",
            self.frame
        );
    }

    fn update_camera(&mut self) {
        let presentation = self
            .runtime
            .retail_title_presentation()
            .expect("Title Map presentation must be readable")
            .expect("Title Map presentation must be configured");
        if presentation.screen != TitleScreen::Map || self.runtime.arena().main_object().is_none() {
            return;
        }
        let island_cam_state = self
            .runtime
            .global_word(ISLAND_CAMERA_STATE_GLOBAL)
            .expect("island camera state global must exist")
            .cast_signed();
        let island_cam_rot_x = self
            .runtime
            .global_word(ISLAND_CAMERA_ROTATION_GLOBAL)
            .expect("island camera rotation global must exist")
            .cast_signed();
        let step = self
            .camera
            .update_with_island(
                &self.graph,
                RetailCameraInput {
                    tapped: self.tapped_previous,
                },
                Some(RetailIslandCameraInput {
                    island_cam_state,
                    island_cam_rot_x,
                }),
            )
            .expect("authored Title Map camera update must execute");
        let island_writeback = match step.outcome {
            RetailCameraOutcome::IslandAdvanced {
                mode,
                state_before,
                state_after,
                ..
            } => Some((mode, state_before, state_after)),
            _ => None,
        };
        if let Some((7, _, state_after)) = island_writeback {
            self.runtime
                .set_global_word(ISLAND_CAMERA_STATE_GLOBAL, state_after.cast_unsigned())
                .expect("mode-seven island state writeback must succeed");
        }
        for effect in &step.effects {
            let RetailCameraEffect::LevelUpdate {
                before,
                after,
                flags,
            } = *effect
            else {
                continue;
            };
            if before.path.zone != after.path.zone {
                self.lifecycle
                    .transition_with_marker(after.path.zone, flags & 2 != 0)
                    .expect("Title Map cross-zone lifecycle transition must execute");
            }
            let existing = self
                .runtime
                .level_state_context()
                .expect("Title Map level context must remain mounted")
                .clone();
            let graphics_flags = self
                .graph
                .zone(after.path.zone)
                .expect("Title Map destination zone must be present")
                .graphics_flags;
            self.runtime
                .set_level_state_context(RetailLevelStateContext {
                    location: after,
                    graphics_flags,
                    box_count: existing.box_count,
                    checkpoint_id: existing.checkpoint_id,
                    checkpoint_translation: existing.checkpoint_translation,
                    first_spawn: existing.first_spawn,
                    active_neighbor_zones: self.lifecycle.active_neighbor_zones(),
                });
        }
        if let Some((8, _, state_after)) = island_writeback {
            self.runtime
                .set_global_word(ISLAND_CAMERA_STATE_GLOBAL, state_after.cast_unsigned())
                .expect("mode-eight island state writeback must succeed");
        }
        self.runtime.set_frame_context(
            step.game_state,
            self.camera
                .rotation_xz(&self.graph)
                .expect("Title Map camera rotation must resolve"),
        );
    }

    fn wait_until_ready(&mut self, limit: u32) {
        for _ in 0..limit {
            if self
                .runtime
                .retail_title_presentation()
                .expect("Title Map presentation must be readable")
                .is_some_and(|title| title.phase == TitlePhase::Ready)
            {
                return;
            }
            self.step(0);
        }
        panic!(
            "Title Map did not become ready by frame {}: {:?}",
            self.frame,
            self.runtime.retail_title_presentation()
        );
    }

    fn tap(&mut self, button: u32) {
        self.step(button);
        self.step(0);
    }
}

fn screen_projection(field_of_view: u32) -> Result<u32, String> {
    match field_of_view {
        30 => Ok(960),
        37 => Ok(800),
        55 => Ok(500),
        60 => Ok(460),
        90 => Ok(288),
        _ => Err(format!(
            "retail field of view {field_of_view} has no projection constant"
        )),
    }
}

fn follow_input(
    runtime: &RetailRuntime,
    level: LevelId,
    held_buttons: u32,
) -> Result<Option<RetailCameraFollowInput>, String> {
    let Some(arena) = runtime.arena().main_object() else {
        return Ok(None);
    };
    let object = runtime
        .object_for_arena(arena)
        .ok_or_else(|| "main arena object has no VM binding".to_owned())?;
    let player = runtime
        .machine()
        .object(object.vm())
        .map_err(|error| format!("main VM object: {error:?}"))?;
    let register = |index| {
        player
            .register(index)
            .map(u32::cast_signed)
            .map_err(|error| format!("main register {index}: {error:?}"))
    };
    let level_id = i32::try_from(level.get())
        .map_err(|_| format!("level {level} does not fit the camera input"))?;
    Ok(Some(RetailCameraFollowInput {
        player_translation: crust_sim::Vec3 {
            x: register(process_register::TRANSLATION_X)?,
            y: register(process_register::TRANSLATION_Y)?,
            z: register(process_register::TRANSLATION_Z)?,
        },
        player_cam_zoom: register(process_register::CAMERA_ZOOM)?,
        held_buttons,
        level_id,
        frames_elapsed: runtime.machine().frames_elapsed(),
        gem_stamp: 0,
    }))
}

fn player_trace(runtime: &RetailRuntime) -> Result<Option<PlayerTrace>, String> {
    let Some(arena) = runtime.arena().main_object() else {
        return Ok(None);
    };
    let object = runtime
        .object_for_arena(arena)
        .ok_or_else(|| "main arena object has no VM binding".to_owned())?;
    let player = runtime
        .machine()
        .object(object.vm())
        .map_err(|error| format!("main VM object: {error:?}"))?;
    let register = |index| {
        player
            .register(index)
            .map_err(|error| format!("main register {index}: {error:?}"))
    };
    Ok(Some(PlayerTrace {
        program: player.program_identity(),
        code_address: player.code_address(),
        zone: runtime
            .arena()
            .get(arena)
            .ok_or_else(|| "main arena object disappeared during trace".to_owned())?
            .zone(),
        translation: [
            register(process_register::TRANSLATION_X)?.cast_signed(),
            register(process_register::TRANSLATION_Y)?.cast_signed(),
            register(process_register::TRANSLATION_Z)?.cast_signed(),
        ],
        velocity: [
            register(process_register::MISC_A_X)?.cast_signed(),
            register(process_register::MISC_A_Y)?.cast_signed(),
            register(process_register::MISC_A_Z)?.cast_signed(),
        ],
        state: player.state(),
        state_flags: register(process_register::STATE_FLAGS)?,
        status_a: register(process_register::STATUS_A)?,
        status_b: register(process_register::STATUS_B)?,
        tawna_counter: register(0x48)?,
        event: register(process_register::EVENT)?,
        animation_stamp: register(process_register::ANIMATION_STAMP)?,
        state_stamp: register(process_register::STATE_STAMP)?,
        animation_counter: register(process_register::ANIMATION_COUNTER)?,
        animation_sequence: register(process_register::ANIMATION_SEQUENCE)?,
        animation_frame: register(process_register::ANIMATION_FRAME)?,
        stack_len: player.stack().len(),
        stack_top: player.stack().last().copied(),
        stack_tail: std::array::from_fn(|index| {
            player
                .stack()
                .len()
                .checked_sub(8 - index)
                .and_then(|index| player.stack().get(index))
                .copied()
        }),
    }))
}

fn upstream_platform_traces(runtime: &RetailRuntime) -> Result<UpstreamPlatformTraces, String> {
    let mut traces = UpstreamPlatformTraces::default();
    for arena in runtime
        .arena()
        .postorder_snapshot()
        .map_err(|error| format!("Upstream object forest: {error:?}"))?
    {
        let spawned = runtime
            .arena()
            .get(arena)
            .ok_or_else(|| "Upstream object disappeared during platform trace".to_owned())?;
        let Some(descriptor) = spawned.entity_descriptor() else {
            continue;
        };
        let slot = match (descriptor.id, descriptor.executable, descriptor.subtype) {
            (23, 28, 2) => 0,
            (47, 28, 1) => 1,
            (46, 28, 1) => 2,
            (54, 28, 1) => 3,
            (55, 28, 9) => 4,
            _ => continue,
        };
        let object = runtime
            .object_for_arena(arena)
            .ok_or_else(|| "Upstream platform has no VM binding".to_owned())?;
        let vm = runtime
            .machine()
            .object(object.vm())
            .map_err(|error| format!("Upstream platform VM object: {error:?}"))?;
        let register = |index| {
            vm.register(index)
                .map(u32::cast_signed)
                .map_err(|error| format!("Upstream platform register {index}: {error:?}"))
        };
        let trace = MovingPlatformTrace {
            translation: [
                register(process_register::TRANSLATION_X)?,
                register(process_register::TRANSLATION_Y)?,
                register(process_register::TRANSLATION_Z)?,
            ],
            state: vm.state(),
        };
        let target = match slot {
            0 => &mut traces.orbital,
            1 => &mut traces.first_zero_k,
            2 => &mut traces.second_zero_k,
            3 => &mut traces.zero_l,
            4 => &mut traces.zero_m,
            _ => unreachable!("all Upstream platform slots are matched"),
        };
        if target.replace(trace).is_some() {
            return Err(format!(
                "Upstream has more than one live RivOC entity {}",
                descriptor.id
            ));
        }
    }
    Ok(traces)
}

fn update_camera(
    frame: u32,
    level: LevelId,
    nsd: &Nsd,
    graph: &RetailZoneGraph,
    camera: &mut RetailCameraRuntime,
    death_camera: &mut RetailDeathCameraState,
    death_camera_pose: &mut Option<RetailCameraPose>,
    lifecycle: &mut ZoneLifecycle,
    runtime: &mut RetailRuntime,
    host: &mut NsfProgramHost<'_>,
    survey: &mut LevelSurvey,
    held_buttons: u32,
) -> Result<(), String> {
    let location = camera.location();
    let mode = graph
        .path(location.path)
        .ok_or_else(|| {
            format!(
                "camera graph has no active path {}:{}",
                location.path.zone, location.path.index
            )
        })?
        .camera_mode;
    let display_mask = runtime.current_display_mask();
    let camera_before = *camera;
    let spin_death = display_mask & 0x1_0000 != 0;
    let step = if runtime.arena().main_object().is_none() {
        camera.stationary_step()
    } else if spin_death {
        let resolved = runtime
            .resolve_spin_death_camera_inputs(host)
            .map_err(|error| format!("spin-death camera input resolution: {error:?}"))?;
        let pose = death_camera_pose.get_or_insert(
            camera
                .pose(graph)
                .map_err(|error| format!("spin-death camera initial pose: {error}"))?,
        );
        death_camera.i_death_cam = resolved.count;
        death_camera
            .step(
                pose,
                RetailDeathCameraInput {
                    transformed_focus: resolved.focus,
                    flip_speed: resolved.flip_speed,
                    zoom_speed: resolved.zoom_speed,
                    spin_accel: resolved.spin_accel,
                    ticks_per_frame: resolved.ticks_per_frame,
                },
            )
            .map_err(|error| format!("spin-death camera step: {error:?}"))?;
        runtime
            .set_spin_death_camera_count(death_camera.i_death_cam)
            .map_err(|error| format!("spin-death camera count writeback: {error:?}"))?;
        survey.observe_death_camera(frame, *pose, death_camera.i_death_cam);
        camera.stationary_step()
    } else if display_mask & 0x2 != 0x2 {
        camera.stationary_step()
    } else if matches!(mode, 5 | 6) {
        let input = follow_input(runtime, level, held_buttons)?
            .ok_or_else(|| "follow camera has no main-object input".to_owned())?;
        camera
            .update_follow(graph, input)
            .map_err(|error| {
                format!(
                    "{error}; mode={mode} camera={camera_before:?} input={input:?} display-mask={display_mask:#010x}"
                )
            })?
    } else {
        camera
            .update(graph, RetailCameraInput::default())
            .map_err(|error| {
                format!(
                    "{error}; mode={mode} camera={camera_before:?} display-mask={display_mask:#010x}"
                )
            })?
    };
    if !spin_death && step.before != step.after {
        *death_camera_pose = None;
    }

    for effect in &step.effects {
        match *effect {
            RetailCameraEffect::LevelUpdate {
                before,
                after,
                flags,
            } => {
                if before.path.zone != after.path.zone {
                    let activation_marker = (lifecycle.current_zone().is_none()
                        && level != LevelId::TITLE)
                        || flags & 2 != 0;
                    let plan = lifecycle
                        .plan_transition_with_marker(after.path.zone, activation_marker)
                        .map_err(|error| error.to_string())?;
                    for action in plan.actions().iter().copied() {
                        if let ZoneTransitionAction::TerminateZoneObjects(zone) = action {
                            let report = runtime
                                .terminate_zone_objects(
                                    zone,
                                    ZoneTerminationMode::Departure {
                                        target: after.path.zone,
                                    },
                                    host,
                                )
                                .map_err(|error| format!("TERM {zone}: {error:?}"))?;
                            for failure in report.event_failures {
                                survey.record_issue(
                                    "terminate-event",
                                    frame,
                                    format!(
                                        "zone {zone} object {:?}: {:?}",
                                        failure.object, failure.error
                                    ),
                                );
                            }
                        }
                    }
                    lifecycle
                        .commit_transition(&plan)
                        .map_err(|error| error.to_string())?;
                    survey.zone_transitions += 1;
                }
                refresh_level_context(runtime, graph, lifecycle, after)?;
            }
            RetailCameraEffect::SaveStateHandshake { location } => {
                refresh_level_context(runtime, graph, lifecycle, location)?;
                let main = runtime
                    .arena()
                    .main_object()
                    .and_then(|arena| runtime.object_for_arena(arena))
                    .ok_or_else(|| "camera save handshake has no main object".to_owned())?;
                runtime
                    .save_level_state(main, true)
                    .map_err(|error| format!("camera save handshake: {error:?}"))?;
                survey.save_handshakes += 1;
            }
        }
    }

    let rotation_xz = camera
        .rotation_xz(graph)
        .map_err(|error| error.to_string())?;
    let pose = (*death_camera_pose)
        .map_or_else(|| camera.pose(graph), Ok)
        .map_err(|error| error.to_string())?;
    let field_of_view = nsd
        .ldat()
        .ok_or_else(|| "bootable pair has no LDAT".to_owned())?
        .field_of_view;
    runtime.set_frame_context(step.game_state, rotation_xz);
    runtime.set_transform_vectors_camera(RetailTransformVectorsCamera::from_retail_pose(
        pose.translation,
        pose.rotation_yxz,
        screen_projection(field_of_view)?,
    ));
    Ok(())
}

fn active_survey_held(frame: u32) -> u32 {
    match (frame - 1) % 120 {
        0..=31 => 0x1000,
        32..=39 => 0x1040,
        40..=55 => 0x2000,
        56..=63 => 0x2080,
        64..=71 => 0x0040,
        72..=79 => 0x4000,
        80..=87 => 0x8000,
        88..=95 => 0x0020,
        96..=103 => 0x0010,
        104 => 0x0800,
        _ => 0,
    }
}

fn apply_restart(
    frame: u32,
    level: LevelId,
    captured_saved_level: LevelId,
    graph: &RetailZoneGraph,
    camera: &mut RetailCameraRuntime,
    lifecycle: &mut ZoneLifecycle,
    runtime: &mut RetailRuntime,
    host: &mut NsfProgramHost<'_>,
    survey: &mut LevelSurvey,
) -> Result<Option<String>, String> {
    let snapshot = runtime
        .saved_level_state()
        .cloned()
        .ok_or_else(|| "load-state effect has no saved snapshot".to_owned())?;
    if captured_saved_level != level {
        let outcome = runtime
            .restart_saved_level_from_effect(host, captured_saved_level)
            .map_err(|error| format!("different-level restart: {error:?}"))?;
        return match outcome {
            RetailRestartOutcome::DifferentLevel { saved_level, .. } => Ok(Some(format!(
                "requested cross-level restart from {level} to {saved_level}"
            ))),
            RetailRestartOutcome::Restarted(_) => {
                Err("cross-level snapshot restarted in the current pair".to_owned())
            }
        };
    }

    let plan = lifecycle
        .plan_hard_restart(snapshot.location.path.zone, level != LevelId::TITLE)
        .map_err(|error| error.to_string())?;
    let mut camera_preview = *camera;
    let expected_flags = u8::from(
        !runtime
            .level_state_context()
            .ok_or_else(|| "restart has no level-state context".to_owned())?
            .first_spawn,
    );
    camera_preview
        .level_update(
            graph,
            snapshot.location.path,
            snapshot.location.progress.raw(),
            expected_flags,
        )
        .map_err(|error| error.to_string())?;
    let outcome = runtime
        .restart_saved_level_from_effect(host, captured_saved_level)
        .map_err(|error| format!("object restart: {error:?}"))?;
    let RetailRestartOutcome::Restarted(report) = outcome else {
        return Err("same-level snapshot requested a different stream".to_owned());
    };
    for failure in &report.respawn_event_failures {
        survey.record_issue(
            "respawn-event",
            frame,
            format!("object {:?}: {:?}", failure.object, failure.error),
        );
    }
    for (zone, zone_report) in &report.zone_reports {
        for failure in &zone_report.event_failures {
            survey.record_issue(
                "restart-term-event",
                frame,
                format!(
                    "zone {zone} object {:?}: {:?}",
                    failure.object, failure.error
                ),
            );
        }
    }
    lifecycle
        .commit_hard_restart(&plan)
        .map_err(|error| error.to_string())?;
    *camera = camera_preview;
    refresh_level_context(runtime, graph, lifecycle, report.snapshot.location)?;
    survey.restarts += 1;
    survey.restart_frames.push(frame);
    Ok(None)
}

fn expected_spawn_rejection(
    result: &Result<RuntimeObjectHandle, RuntimeError<NsfProgramError>>,
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

fn drain_reclaim_diagnostics(runtime: &mut RetailRuntime, survey: &mut LevelSurvey, frame: u32) {
    // The browser consumes these at the matching spawn/frame boundary. The
    // survey has no platform audio owner, but still drains the ordered work so
    // a TERM fault cannot hide behind an otherwise clean VM report.
    let _cleanup = runtime.take_cleanup_actions();
    for fault in runtime.take_reclaim_event_faults() {
        survey.record_issue(
            "reclaim-term",
            frame,
            format!("TERM fault while reclaiming {:?}", fault.object),
        );
    }
    for fault in runtime.take_solid_event_faults() {
        survey.record_issue(
            "solid-event",
            frame,
            format!(
                "mover {:?} recipient {:?} event {:#x} reason {:?}",
                fault.moving_object, fault.recipient, fault.event, fault.reason
            ),
        );
    }
}

fn fault_context(
    runtime: &RetailRuntime,
    nsd: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    object: RuntimeObjectHandle,
) -> String {
    let Some(arena) = runtime.arena().get(object.arena()) else {
        return format!("object {object:?} was released before diagnosis");
    };
    let Ok(vm) = runtime.machine().object(object.vm()) else {
        return format!(
            "object {object:?} origin={:?} has no live VM",
            arena.origin()
        );
    };
    let identity = vm.program_identity();
    let address = vm.code_address();
    let failing_pc = address.pc.saturating_sub(1);
    let opcode = identity.and_then(|identity| {
        load_gool_state_program(nsd, nsf, nsf_bytes, identity.global_eid(), vm.state())
            .ok()
            .and_then(|program| match address.segment {
                CodeSegment::External => program.code().get(failing_pc).copied(),
                CodeSegment::Global => program.global_code().get(failing_pc).copied(),
            })
    });
    let opcode = opcode.map_or_else(|| "unresolved".to_owned(), |word| format!("{word:#010x}"));
    let stack_tail_start = vm.stack().len().saturating_sub(12);
    let stack_tail = &vm.stack()[stack_tail_start..];
    format!(
        "object={object:?} zone={} origin={:?} program={identity:?} state={} post-address={address:?} failing-pc={failing_pc} opcode={opcode} initial-sp={} stack-len={} stack-tail={stack_tail:?}",
        arena.zone(),
        arena.origin(),
        vm.state(),
        vm.initial_stack_pointer(),
        vm.stack().len(),
    )
}

/// Reads a bounded controller prefix from the mounted, legally local NSF.
///
/// The source PBAK remains outside the repository: this helper neither
/// installs its restart snapshot nor serializes its bytes or pad words.
fn load_local_pbak_pad_prefix(
    nsf: &Nsf,
    nsf_bytes: &[u8],
    frame_count: u32,
) -> Result<Box<[u32]>, String> {
    let mut entries = nsf
        .entries()
        .filter(|entry| entry.entry_type == PBAK_ENTRY_TYPE);
    let entry = entries
        .next()
        .ok_or_else(|| "mounted NSF has no PBAK entry".to_owned())?;
    if let Some(extra) = entries.next() {
        return Err(format!(
            "mounted NSF has more than one PBAK entry ({} and {})",
            entry.eid, extra.eid
        ));
    }
    let header = load_pbak_entry(entry, nsf_bytes)
        .map_err(|error| format!("legally local PBAK {}: {error}", entry.eid))?;
    let frame_count = usize::try_from(frame_count)
        .map_err(|_| "PBAK prefix frame count does not fit usize".to_owned())?;
    if header.frames.len() < frame_count {
        return Err(format!(
            "PBAK {} has {} frames; requested prefix has {frame_count}",
            entry.eid,
            header.frames.len()
        ));
    }
    Ok(header
        .frames
        .into_iter()
        .take(frame_count)
        .map(|frame| frame.held)
        .collect())
}

fn survey_pair_with_runtime(
    name: &'static str,
    level: LevelId,
    nsd: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    mut runtime: RetailRuntime,
    context_source: LevelContextSource,
    input_profile: SurveyInputProfile,
    survey_frames: u32,
) -> Result<(LevelSurvey, RetailRuntime), String> {
    let graph = graph_for_pair(level, nsd, nsf, nsf_bytes)?;
    let (zones, mut lifecycle) = zone_catalog(nsd, nsf, nsf_bytes, &graph, level)?;
    let mut camera = RetailCameraRuntime::new(&graph).map_err(|error| error.to_string())?;
    let mut death_camera = RetailDeathCameraState::default();
    let mut death_camera_pose = None;
    let spawn_points = graph
        .path(graph.spawn_path())
        .and_then(|path| u16::try_from(path.points.len()).ok())
        .and_then(NonZeroU16::new)
        .ok_or_else(|| "spawn camera path has no representable points".to_owned())?;
    let mut frame_state = RetailFrameState::ready(spawn_points, 0);
    match context_source {
        LevelContextSource::FreshBoot => {
            refresh_level_context(&mut runtime, &graph, &lifecycle, camera.location())?;
        }
        LevelContextSource::SessionGlobals => seed_mounted_level_context_from_globals(
            &mut runtime,
            &graph,
            &lifecycle,
            camera.location(),
        )?,
    }
    let mut host = NsfProgramHost::new(nsd, nsf, nsf_bytes);
    runtime
        .create_retail_core_objects(camera.location().path.zone, &mut host)
        .map_err(|error| format!("core object creation: {error:?}"))?;
    runtime
        .create_retail_level_misc_object(camera.location().path.zone, &mut host)
        .map_err(|error| format!("level-misc object creation: {error:?}"))?;
    let local_pbak_frame_count = match input_profile {
        SurveyInputProfile::LocalPbakPrefix => Some(survey_frames),
        SurveyInputProfile::BouldersCompletionRoute => Some(895),
        SurveyInputProfile::UpstreamCarriedRecovery => Some(UPSTREAM_PBAK_FRAMES),
        _ => None,
    };
    let local_pbak_prefix = local_pbak_frame_count
        .map(|frame_count| load_local_pbak_pad_prefix(nsf, nsf_bytes, frame_count))
        .transpose()?;
    let mut survey = LevelSurvey::new(level, name, input_profile);
    let mut input_controller = SurveyInputController::new(input_profile);
    let mut empty_frames = 0_u32;
    let mut held_previous = 0_u32;
    let mut held_previous_2 = 0_u32;
    let mut tapped_previous = 0_u32;
    let mut last_interaction_globals = None;
    let mut previous_box_count = None;
    let mut previous_checkpoint = None;
    for frame in 1..=survey_frames {
        survey.frames = frame;
        runtime.set_frame_timing(34, 34);
        let checkpoint_id_before_frame = runtime
            .global_word(CHECKPOINT_ID_GLOBAL)
            .map(u32::cast_signed)
            .map_err(|error| format!("checkpoint input global: {error:?}"))?;
        let player_before_frame = player_trace(&runtime)?;
        let upstream_platforms =
            if matches!(input_profile, SurveyInputProfile::UpstreamCarriedRecovery)
                && frame > UPSTREAM_PBAK_FRAMES
            {
                upstream_platform_traces(&runtime)?
            } else {
                UpstreamPlatformTraces::default()
            };
        let held = input_controller.held(
            frame,
            camera.location(),
            player_before_frame,
            checkpoint_id_before_frame,
            local_pbak_prefix
                .as_deref()
                .and_then(|frames| frames.get(usize::try_from(frame - 1).ok()?))
                .copied(),
            upstream_platforms,
        );
        let tapped = held & !held_previous;
        runtime
            .set_pad_snapshot(
                0,
                RetailPadSnapshot {
                    tapped,
                    held,
                    tapped_previous,
                    held_previous,
                    held_previous_2,
                },
            )
            .map_err(|error| format!("pad snapshot: {error:?}"))?;
        held_previous_2 = held_previous;
        held_previous = held;
        tapped_previous = tapped;

        let spawn_scan = lifecycle.next_frame_spawn_scan();
        let neighbors = spawn_scan
            .iter()
            .map(|candidate| {
                let zone = zones.get(&candidate.zone).ok_or_else(|| {
                    format!("spawn zone {} is absent from the catalog", candidate.zone)
                })?;
                Ok(NeighborZone {
                    eid: zone.eid,
                    display_flags: candidate.display_flags,
                    entities: zone.entities.as_slice(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let attempts = runtime.spawn_current_zone_neighbors(&neighbors, &mut host);
        let frame_attempts = attempts.len();
        survey.spawn_attempts += frame_attempts as u64;
        for attempt in &attempts {
            match &attempt.result {
                Ok(_) => survey.successful_spawns += 1,
                Err(_) if expected_spawn_rejection(&attempt.result) => {
                    survey.expected_spawn_rejections += 1;
                }
                Err(error) => {
                    survey.unexpected_spawn_errors += 1;
                    survey.record_issue(
                        "spawn",
                        frame,
                        format!(
                            "zone {} entity {} descriptor {:?}: {error:?}",
                            attempt.zone, attempt.entity_index, attempt.descriptor
                        ),
                    );
                }
            }
        }
        drain_reclaim_diagnostics(&mut runtime, &mut survey, frame);

        runtime
            .advance_level_shader()
            .map_err(|error| format!("level shader update: {error:?}"))?;

        if let Err(error) = update_camera(
            frame,
            level,
            nsd,
            &graph,
            &mut camera,
            &mut death_camera,
            &mut death_camera_pose,
            &mut lifecycle,
            &mut runtime,
            &mut host,
            &mut survey,
            held,
        ) {
            survey.record_issue("camera", frame, error.clone());
            survey.terminal = Some(format!("camera boundary: {error}"));
            break;
        }

        let report = match runtime.run_frame(&mut host, INSTRUCTION_BUDGET) {
            Ok(report) => report,
            Err(error) => {
                survey.record_issue("runtime-frame", frame, format!("{error:?}"));
                survey.terminal = Some(format!("runtime frame aborted: {error:?}"));
                break;
            }
        };
        survey.executions += report.executions.len() as u64;
        for execution in &report.executions {
            if let Ok(vm) = runtime.machine().object(execution.object.vm())
                && let Some(identity) = vm.program_identity()
            {
                survey
                    .observed_program_states
                    .insert((identity.global_eid(), vm.state()));
            }
            if let Err(error) = &execution.result {
                survey.execution_errors += 1;
                let context = fault_context(&runtime, nsd, nsf, nsf_bytes, execution.object);
                survey.record_issue("vm-execution", frame, format!("{error:?}; {context}"));
                if survey.fault_contexts.len() < 12 {
                    survey.fault_contexts.insert(context);
                }
            }
        }
        for effect in &report.effects {
            survey.record_effect(frame, effect);
            if frame <= 360
                && survey.early_direct_send_samples.len() < 128
                && let VmEffect::SendEvent(request) = effect
                && matches!(request.target, SendEventTarget::Direct { .. })
            {
                survey
                    .early_direct_send_samples
                    .push((frame, request.event));
            }
            if let VmEffect::Transition(next_lid) = effect {
                survey.next_lid.get_or_insert((frame, *next_lid));
            }
            if let VmEffect::SaveState(_) = effect
                && let Some(snapshot) = runtime.saved_level_state()
            {
                survey
                    .saved_box_count_samples
                    .push((frame, snapshot.box_count));
            }
            if let VmEffect::SpawnFlagsChanged { id, flags, .. } = effect {
                survey.spawn_flag_samples.push((frame, *id, *flags));
            }
        }
        let box_count = runtime
            .global_word(BOX_COUNT_GLOBAL)
            .map(u32::cast_signed)
            .map_err(|error| format!("box-count trace global: {error:?}"))?;
        if previous_box_count != Some(box_count) {
            survey.box_count_samples.push((frame, box_count));
            previous_box_count = Some(box_count);
        }
        let checkpoint_id = runtime
            .global_word(CHECKPOINT_ID_GLOBAL)
            .map(u32::cast_signed)
            .map_err(|error| format!("checkpoint trace global: {error:?}"))?;
        let checkpoint_translation = [
            runtime
                .global_word(CHECKPOINT_TRANSLATION_GLOBALS[0])
                .map(u32::cast_signed)
                .map_err(|error| format!("checkpoint-X trace global: {error:?}"))?,
            runtime
                .global_word(CHECKPOINT_TRANSLATION_GLOBALS[1])
                .map(u32::cast_signed)
                .map_err(|error| format!("checkpoint-Y trace global: {error:?}"))?,
            runtime
                .global_word(CHECKPOINT_TRANSLATION_GLOBALS[2])
                .map(u32::cast_signed)
                .map_err(|error| format!("checkpoint-Z trace global: {error:?}"))?,
        ];
        let checkpoint = (checkpoint_id, checkpoint_translation);
        if previous_checkpoint != Some(checkpoint) {
            survey
                .checkpoint_samples
                .push((frame, checkpoint_id, checkpoint_translation));
            previous_checkpoint = Some(checkpoint);
        }
        if std::env::var_os("C1_INTERACTION_TRACE").is_some()
            && matches!(input_profile, SurveyInputProfile::ForwardWithActions)
        {
            let read_global = |index| {
                runtime
                    .global_word(index)
                    .map(u32::cast_signed)
                    .map_err(|error| format!("interaction global {index}: {error:?}"))
            };
            let interaction_globals = [
                read_global(LIFE_COUNT_GLOBAL)?,
                read_global(HEALTH_GLOBAL)?,
                read_global(FRUIT_COUNT_GLOBAL)?,
                read_global(BOX_COUNT_GLOBAL)?,
                read_global(CHECKPOINT_ID_GLOBAL)?,
            ];
            if last_interaction_globals != Some(interaction_globals) {
                eprintln!(
                    "interaction f{frame} held={held:#06x} lives/health/fruit/boxes/checkpoint={interaction_globals:?} camera={:?} player={:?} effects={:?}",
                    camera.location(),
                    player_trace(&runtime)?,
                    report.effects,
                );
                last_interaction_globals = Some(interaction_globals);
            }
        }
        drain_reclaim_diagnostics(&mut runtime, &mut survey, frame);

        let player = player_trace(&runtime)?;
        survey.observe_progress(frame, camera.location(), player);
        if std::env::var_os("C1_PROGRESSION_TRACE").is_some()
            && matches!(
                input_profile,
                SurveyInputProfile::ForwardWithActions
                    | SurveyInputProfile::GreatGatePhaseRobust
                    | SurveyInputProfile::GreatGateTawnaBonus
                    | SurveyInputProfile::GreatGateYellowGemExactCarry
            )
            && (matches!(
                input_profile,
                SurveyInputProfile::GreatGatePhaseRobust
                    | SurveyInputProfile::GreatGateTawnaBonus
                    | SurveyInputProfile::GreatGateYellowGemExactCarry
            ) || frame >= 300
                || frame <= 120)
        {
            eprintln!(
                "route[{}] f{frame} held={held:#06x} camera={:?} player={player:?}",
                input_profile.label(),
                camera.location()
            );
        }
        if std::env::var_os("C1_SURVEY_HOG_TRACE").is_some() && (170..=200).contains(&frame) {
            eprintln!(
                "hog f{frame} held={held:#06x} camera={:?} globals106/107={:?}/{:?} player={player:?}",
                camera.location(),
                runtime.global_word(106),
                runtime.global_word(107),
            );
        }
        if input_profile.stops_at_transition()
            && let Some((transition_frame, next_lid)) = survey.next_lid
        {
            survey.terminal = Some(format!(
                "frame {transition_frame} requested level transition to {next_lid:#04x}"
            ));
            break;
        }

        let mut load_states = report.effects.iter().filter_map(|effect| match effect {
            VmEffect::LoadState { saved_level, .. } => Some(*saved_level),
            _ => None,
        });
        let captured_load = load_states.next();
        if load_states.next().is_some() {
            return Err(format!(
                "frame {frame} emitted more than one LoadState boundary"
            ));
        }
        if let Some(captured_load) = captured_load {
            let captured_load = captured_load
                .ok_or_else(|| format!("frame {frame} emitted an unresolved LoadState boundary"))?;
            match apply_restart(
                frame,
                level,
                captured_load,
                &graph,
                &mut camera,
                &mut lifecycle,
                &mut runtime,
                &mut host,
                &mut survey,
            ) {
                Ok(Some(terminal)) => {
                    survey.terminal = Some(terminal);
                    break;
                }
                Ok(None) => {
                    death_camera = RetailDeathCameraState::default();
                    death_camera_pose = None;
                }
                Err(error) => {
                    survey.record_issue("restart", frame, error.clone());
                    survey.terminal = Some(format!("restart boundary: {error}"));
                    break;
                }
            }
        }

        let count_draws = runtime.current_display_mask() & 0x1000 != 0;
        let _ = frame_state.tick_with_draw_count_enabled(count_draws);
        survey.max_live_objects = survey.max_live_objects.max(runtime.arena().len());
        if runtime.arena().is_empty() && frame_attempts == 0 && report.effects.is_empty() {
            empty_frames += 1;
            if empty_frames >= EMPTY_TERMINAL_WINDOW {
                survey.terminal = Some(format!(
                    "no live or spawnable objects for {EMPTY_TERMINAL_WINDOW} frames"
                ));
                break;
            }
        } else {
            empty_frames = 0;
        }
    }

    survey.final_live_objects = runtime.arena().len();
    survey.faulted_objects = runtime.faulted_object_count();
    for object in runtime.faulted_objects() {
        if survey.fault_contexts.len() >= 12 {
            break;
        }
        survey
            .fault_contexts
            .insert(fault_context(&runtime, nsd, nsf, nsf_bytes, object));
    }
    Ok((survey, runtime))
}

fn survey_pair(
    name: &'static str,
    level: LevelId,
    nsd: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    input_profile: SurveyInputProfile,
    survey_frames: u32,
) -> Result<LevelSurvey, String> {
    survey_pair_with_runtime(
        name,
        level,
        nsd,
        nsf,
        nsf_bytes,
        RetailRuntime::new_for_level(GLOBAL_WORDS, level),
        LevelContextSource::FreshBoot,
        input_profile,
        survey_frames,
    )
    .map(|(survey, _)| survey)
}

fn read_pair(root: &Path, level: LevelId) -> Result<(Vec<u8>, Vec<u8>), String> {
    let nsd_path = root.join(StreamName::new(level, StreamKind::Nsd).filename());
    let nsf_path = root.join(StreamName::new(level, StreamKind::Nsf).filename());
    let nsd =
        std::fs::read(&nsd_path).map_err(|error| format!("{}: {error}", nsd_path.display()))?;
    let nsf =
        std::fs::read(&nsf_path).map_err(|error| format!("{}: {error}", nsf_path.display()))?;
    Ok((nsd, nsf))
}

fn parse_local_pair(root: &Path, level: LevelId) -> Result<(Nsd, Nsf, Vec<u8>), String> {
    let (nsd_bytes, nsf_bytes) = read_pair(root, level)?;
    let nsd = parse_nsd(&nsd_bytes, level).map_err(|error| error.to_string())?;
    let nsf = parse_nsf(&nsf_bytes, &nsd).map_err(|error| error.to_string())?;
    Ok((nsd, nsf, nsf_bytes))
}

fn requested_survey_level() -> Option<LevelId> {
    let raw = std::env::var("C1_SURVEY_LEVEL").ok()?;
    let digits = raw
        .strip_prefix("0x")
        .or_else(|| raw.strip_prefix("0X"))
        .unwrap_or(&raw);
    let value = u32::from_str_radix(digits, 16)
        .unwrap_or_else(|error| panic!("C1_SURVEY_LEVEL {raw:?} is not hexadecimal: {error}"));
    Some(LevelId::new(value).expect("C1_SURVEY_LEVEL fits the retail filename field"))
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn every_bootable_pair_runs_a_browser_ordered_idle_window() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let requested_level = requested_survey_level();
    let expected_surveys = if requested_level.is_some() { 1 } else { 43 };
    let mut surveys = Vec::new();
    let mut setup_failures = Vec::new();
    for known in KNOWN_LEVELS
        .iter()
        .filter(|known| known.bootable && requested_level.is_none_or(|level| known.id == level))
    {
        let result = read_pair(&root, known.id).and_then(|(nsd_bytes, nsf_bytes)| {
            let nsd = parse_nsd(&nsd_bytes, known.id).map_err(|error| error.to_string())?;
            let nsf = parse_nsf(&nsf_bytes, &nsd).map_err(|error| error.to_string())?;
            let input_profile = if std::env::var_os("C1_SURVEY_ACTIVE_INPUT").is_some() {
                SurveyInputProfile::DirectionAndButtonSweep
            } else {
                SurveyInputProfile::Idle
            };
            survey_pair(
                known.name,
                known.id,
                &nsd,
                &nsf,
                &nsf_bytes,
                input_profile,
                survey_frame_count(),
            )
        });
        match result {
            Ok(survey) => {
                eprintln!("{}", survey.summary());
                surveys.push(survey);
            }
            Err(error) => setup_failures.push(format!("{} ({}): {error}", known.name, known.id)),
        }
    }

    assert!(
        setup_failures.is_empty(),
        "retail idle survey setup failures:\n{}",
        setup_failures.join("\n")
    );
    assert_eq!(
        surveys.len(),
        expected_surveys,
        "requested level is not bootable or the retail bootable-pair count changed"
    );
    assert!(
        surveys
            .iter()
            .all(|survey| survey.frames >= survey_frame_count() || survey.terminal.is_some()),
        "every pair must reach the bounded idle window or a recorded deterministic terminal"
    );
    if requested_level == Some(LevelId::new_const(0x11)) && survey_frame_count() >= 190 {
        let hog_wild = &surveys[0];
        assert!(
            hog_wild.restarts >= 1,
            "Hog Wild idle must deliver its fall-kill event and complete the authored fade/load-state handshake: {}",
            hog_wild.summary()
        );
        assert!(
            hog_wild.first_terminal_fall.is_none(),
            "Hog Wild must restart before retaining a terminal fall: {}",
            hog_wild.summary()
        );
    }
    if requested_level == Some(LevelId::new_const(0x03))
        && survey_frame_count() >= 1_300
        && surveys[0].input_profile == SurveyInputProfile::DirectionAndButtonSweep
    {
        let cortex_power = &surveys[0];
        assert_eq!(cortex_power.death_camera_frames, 117);
        assert_eq!(cortex_power.death_camera_pose_changes, 116);
        assert_eq!(cortex_power.death_camera_max_count, 9);
        assert_eq!(
            cortex_power.first_death_camera_pose,
            Some((
                1_168,
                RetailCameraPose {
                    translation: [2_066_176, 1_645_056, 31_882_960],
                    rotation_yxz: [3_441, 4, 0],
                },
            ))
        );
        assert_eq!(
            cortex_power.last_death_camera_pose,
            Some((
                1_284,
                RetailCameraPose {
                    translation: [2_047_600, 1_406_464, 33_587_488],
                    rotation_yxz: [3_763, 0, 0],
                },
            ))
        );
    }
    if requested_level == Some(LevelId::new_const(0x0a))
        && survey_frame_count() >= 1_800
        && surveys[0].input_profile == SurveyInputProfile::DirectionAndButtonSweep
    {
        let papu_papu = &surveys[0];
        assert!(
            papu_papu.restarts >= 6,
            "Papu Papu's source-valid sweep must exercise authored deaths: {}",
            papu_papu.summary()
        );
        assert_eq!(
            papu_papu.death_camera_frames, 0,
            "Papu Papu's authored deaths use the ordinary fade/load-state path, not GOOL_FLAG_SPIN_DEATH"
        );
    }
    if std::env::var_os("C1_SURVEY_REQUIRE_CLEAN").is_some() {
        let dirty = surveys
            .iter()
            .filter(|survey| !survey.is_clean())
            .map(LevelSurvey::summary)
            .collect::<Vec<_>>();
        assert!(
            dirty.is_empty(),
            "{} retail pair(s) reached checked parity boundaries:\n{}",
            dirty.len(),
            dirty.join("\n")
        );
    }
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn lights_out_active_input_keeps_null_zone_doctor_alive_across_restart() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let level = LevelId::new_const(0x28);
    let known = KNOWN_LEVELS
        .iter()
        .find(|known| known.id == level)
        .expect("the retail level catalog contains Lights Out");
    let (nsd, nsf, nsf_bytes) =
        parse_local_pair(&root, level).expect("Lights Out's local stream pair must parse");
    let (survey, runtime) = survey_pair_with_runtime(
        known.name,
        level,
        &nsd,
        &nsf,
        &nsf_bytes,
        RetailRuntime::new_for_level(GLOBAL_WORDS, level),
        LevelContextSource::FreshBoot,
        SurveyInputProfile::DirectionAndButtonSweep,
        DEFAULT_SURVEY_FRAMES,
    )
    .expect("Lights Out must continue past the frame-240 Dark2 shader boundary");

    assert_eq!(survey.frames, DEFAULT_SURVEY_FRAMES);
    assert_eq!(
        survey.restarts,
        1,
        "the characterized input must exercise the same-level restart: {}",
        survey.summary()
    );
    let doctor_word = runtime
        .global_word(DOCTOR_OBJECT_GLOBAL)
        .expect("the retail global table contains doctor");
    let doctor = CollisionObjectReference::from_word(doctor_word)
        .expect("Lights Out retains the authored non-null doctor pool pointer");
    let live_doctor = runtime
        .object_for_vm(doctor.object())
        .expect("the runtime-created doctor must survive neighbor-zone teardown");
    assert_eq!(
        runtime
            .arena()
            .get(live_doctor.arena())
            .expect("the live doctor must retain arena storage")
            .zone(),
        Eid::NONE,
        "GoolObjectInit must keep executable 29 outside zone-owned restart teardown"
    );
    assert_eq!(
        runtime
            .machine()
            .object(doctor.object())
            .expect("the live doctor must retain VM storage")
            .program_identity()
            .map(GoolProgramIdentity::object_type),
        Some(5),
        "the retained runtime child must remain the authored DoctC program"
    );
    assert_eq!(
        runtime
            .machine()
            .retired_retail_translation(doctor.object()),
        None,
        "a live null-zone doctor must not be represented as reclaimed storage"
    );
    assert!(
        survey.is_clean(),
        "the live null-zone doctor must keep Dark2 clean across restart: {}",
        survey.summary()
    );
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn every_direct_bonus_boot_has_a_restartable_local_snapshot() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    for level in [0x24, 0x25, 0x26, 0x33, 0x34].map(LevelId::new_const) {
        let known = KNOWN_LEVELS
            .iter()
            .find(|known| known.id == level)
            .expect("the retail level catalog contains every bonus stream");
        let (nsd, nsf, nsf_bytes) =
            parse_local_pair(&root, level).expect("the local bonus pair must parse");
        let (survey, runtime) = survey_pair_with_runtime(
            known.name,
            level,
            &nsd,
            &nsf,
            &nsf_bytes,
            RetailRuntime::new_for_level(GLOBAL_WORDS, level),
            LevelContextSource::FreshBoot,
            SurveyInputProfile::DirectionAndButtonSweep,
            DEFAULT_SURVEY_FRAMES,
        )
        .unwrap_or_else(|error| panic!("{} direct boot failed: {error}", known.name));

        assert_eq!(survey.frames, DEFAULT_SURVEY_FRAMES, "{}", known.name);
        assert_eq!(
            runtime.saved_level_state().map(|snapshot| snapshot.level),
            Some(level),
            "{} must seed a current-level snapshot only for fresh direct boot",
            known.name
        );
        if matches!(level.get(), 0x26 | 0x33 | 0x34) {
            assert!(
                survey.restarts >= 1,
                "{} must exercise and survive a direct-boot death restart: {}",
                known.name,
                survey.summary()
            );
        }
        assert!(
            survey.is_clean(),
            "{} direct bonus boot reached a checked boundary: {}",
            known.name,
            survey.summary()
        );
    }
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn bonus_zone_flags_and_warp_program_layout_match_the_legal_corpus() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    for level in [0x24, 0x25, 0x26, 0x33, 0x34].map(LevelId::new_const) {
        let (nsd, nsf, nsf_bytes) =
            parse_local_pair(&root, level).expect("the local bonus pair must parse");
        let graph = graph_for_pair(level, &nsd, &nsf, &nsf_bytes)
            .expect("the local bonus zone graph must parse");
        let spawn = graph
            .zone(graph.spawn_path().zone)
            .expect("the bonus spawn zone must be in its graph");
        assert_eq!(spawn.graphics_flags, 0x2002, "bonus LID {level}");

        let mut runtime = RetailRuntime::new_for_level(GLOBAL_WORDS, level);
        runtime.set_level_state_context(RetailLevelStateContext {
            location: RetailCameraLocation {
                path: graph.spawn_path(),
                progress: crust_sim::retail_frame::PathProgress::ZERO,
            },
            graphics_flags: spawn.graphics_flags,
            box_count: 0,
            checkpoint_id: -1,
            checkpoint_translation: [0; 3],
            first_spawn: false,
            active_neighbor_zones: vec![spawn.eid],
        });
        assert_eq!(
            runtime.global_word(CURRENT_ZONE_FLAGS_GLOBAL),
            Ok(0x2002),
            "bonus LID {level} must publish cur_zone_flags_ro before GOOL"
        );
    }

    let level = LevelId::new_const(0x24);
    let (nsd, nsf, nsf_bytes) =
        parse_local_pair(&root, level).expect("the local Tawna bonus pair must parse");
    let will = Eid::from_name("WillC").expect("fixed retail player EID is valid");
    let warp = Eid::from_name("WarpC").expect("fixed retail portal EID is valid");
    let warp_state = load_gool_state_program(&nsd, &nsf, &nsf_bytes, warp, 0)
        .expect("load the authored WarpC initial state");
    let player_warp = load_gool_state_program(&nsd, &nsf, &nsf_bytes, will, 32)
        .expect("load the authored WillC WARP state");
    let player_death = load_gool_state_program(&nsd, &nsf, &nsf_bytes, will, 22)
        .expect("load the authored WillC fall-kill state");

    assert_eq!(warp_state.code().get(0x2e), Some(&0x87a4_0816));
    assert_eq!(warp_state.code().get(0x42), Some(&0x87a4_0816));
    assert_eq!(player_warp.event_map().get(22), Some(&32));
    assert_eq!(player_warp.code_pc(), Some(0x9b6));
    assert_eq!(player_warp.code().get(0xa2f), Some(&0x1fbe_081e));
    assert_eq!(player_warp.code().get(0xa30), Some(&0x0782_0e1f));
    assert_eq!(player_warp.code().get(0xa31), Some(&0x8227_c002));
    assert_eq!(player_warp.code().get(0xa32), Some(&0x1cc0_dbe0));
    assert_eq!(player_warp.code().get(0xa34), Some(&0x1cc4_d819));
    assert_eq!(player_death.event_map().get(9), Some(&22));
    assert_eq!(player_death.code().get(0x5fc), Some(&0x1cc0_dbe0));
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn jungle_rollers_tawna_bonus_warp_loads_the_carried_parent_snapshot() {
    // This is a deliberately cross-stream characterization, not a synthetic
    // save-state unit test.
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let parent = LevelId::new_const(0x0c);
    let bonus = LevelId::new_const(0x24);
    let known_name = |level| {
        KNOWN_LEVELS
            .iter()
            .find(|known| known.id == level)
            .map(|known| known.name)
            .expect("the vertical-flow level is present in the retail catalog")
    };

    let (parent_nsd, parent_nsf, parent_nsf_bytes) =
        parse_local_pair(&root, parent).expect("Jungle Rollers pair must parse");
    let (_, mut parent_runtime) = survey_pair_with_runtime(
        known_name(parent),
        parent,
        &parent_nsd,
        &parent_nsf,
        &parent_nsf_bytes,
        RetailRuntime::new_for_level(GLOBAL_WORDS, parent),
        LevelContextSource::FreshBoot,
        SurveyInputProfile::Idle,
        1,
    )
    .expect("Jungle Rollers must establish its initial retail save snapshot");
    assert_eq!(
        parent_runtime
            .saved_level_state()
            .map(|snapshot| snapshot.level),
        Some(parent)
    );
    let original_parent_snapshot = parent_runtime
        .saved_level_state()
        .cloned()
        .expect("Jungle Rollers must retain the complete parent snapshot");
    let parent_transition = {
        let mut host = NsfProgramHost::new(&parent_nsd, &parent_nsf, &parent_nsf_bytes);
        parent_runtime
            .finish_level_transition(
                &mut host,
                i32::try_from(bonus.get()).expect("bonus LID fits i32"),
            )
            .expect("the parent LEVEL_END phase must preserve the bonus target")
    };
    assert!(parent_transition.event_failures.is_empty());
    assert_eq!(parent_transition.resolved.level, bonus);
    assert!(!parent_transition.resolved.bonus_return);
    assert_eq!(
        parent_transition
            .carry
            .saved_level_state
            .as_ref()
            .map(|snapshot| snapshot.level),
        Some(parent)
    );

    let (bonus_nsd, bonus_nsf, bonus_nsf_bytes) =
        parse_local_pair(&root, bonus).expect("Tawna bonus pair must parse");
    let bonus_graph = graph_for_pair(bonus, &bonus_nsd, &bonus_nsf, &bonus_nsf_bytes)
        .expect("Tawna bonus zone graph must parse");
    let (bonus_zones, _) = zone_catalog(
        &bonus_nsd,
        &bonus_nsf,
        &bonus_nsf_bytes,
        &bonus_graph,
        bonus,
    )
    .expect("Tawna bonus zones must parse");
    let portal_zone = Eid::from_name("1__AZ").expect("fixed Tawna portal zone EID is valid");
    let portal_entity = bonus_zones[&portal_zone]
        .entities
        .iter()
        .find(|entity| entity.id == 15)
        .expect("Tawna's authored return portal must be present");
    assert_eq!(
        (
            portal_entity.group,
            portal_entity.executable,
            portal_entity.subtype,
            portal_entity.spawn_flags,
            portal_entity.initializer,
        ),
        (3, 0x20, 1, 0x0008, [0; 3])
    );
    assert_eq!(
        portal_entity
            .path_points
            .first()
            .map(|point| (point.x, point.y, point.z)),
        Some((1479, 310, 160))
    );
    let bonus_runtime =
        RetailRuntime::new_from_session(GLOBAL_WORDS, bonus, parent_transition.carry)
            .expect("the bonus stream must import the parent session carry");
    let (_, mut bonus_runtime) = survey_pair_with_runtime(
        known_name(bonus),
        bonus,
        &bonus_nsd,
        &bonus_nsf,
        &bonus_nsf_bytes,
        bonus_runtime,
        LevelContextSource::SessionGlobals,
        SurveyInputProfile::Idle,
        1,
    )
    .expect("the bonus stream must mount with the carried parent snapshot");
    assert_eq!(
        bonus_runtime.saved_level_state(),
        Some(&original_parent_snapshot),
        "the save-restricted bonus spawn must not overwrite the parent return"
    );

    let find_program = |runtime: &RetailRuntime, eid: Eid| {
        runtime
            .arena()
            .postorder_snapshot()
            .expect("the bonus object forest must remain valid")
            .into_iter()
            .filter_map(|arena| runtime.object_for_arena(arena))
            .find(|object| {
                runtime
                    .machine()
                    .object(object.vm())
                    .ok()
                    .and_then(crust_sim::gool::VmObject::program_identity)
                    .is_some_and(|identity| identity.global_eid() == eid)
            })
    };
    let player = bonus_runtime
        .arena()
        .main_object()
        .and_then(|arena| bonus_runtime.object_for_arena(arena))
        .expect("the mounted bonus stream must have Crash");
    let warp = find_program(
        &bonus_runtime,
        Eid::from_name("WarpC").expect("fixed retail portal EID is valid"),
    )
    .expect("the Tawna bonus spawn band must contain WarpC");
    let mut bonus_host = NsfProgramHost::new(&bonus_nsd, &bonus_nsf, &bonus_nsf_bytes);
    // The legal Machine regression exercises WarpC's parsed transition-level
    // proximity/status polling gate and proves that this is the exact handoff
    // it produces. Start here at that synchronous handoff so this separate
    // cross-stream test isolates CardC, LoadState, LEVEL_END, and remounting.
    // WarpC's authored sequence is `PSHV stack, 0` followed by an EVNT with
    // argc one (`0x87a40816`), so WillC receives the literal zero argument.
    let dispatch = bonus_runtime
        .dispatch_event(
            &mut bonus_host,
            Some(warp),
            Some(player),
            22 << 8,
            Some(&[0]),
        )
        .expect("WarpC must synchronously select WillC's authored WARP state");
    assert_eq!(
        dispatch.state_change.as_ref().map(|change| change.state),
        Some(32)
    );

    let mut load_state = None;
    for frame in 1..=5_400_u32 {
        bonus_runtime.set_frame_timing(34, 34);
        let pad = if frame == 300 {
            // CardC's authored completion prompt requires a CROSS tap before
            // its state 63 clears global one and releases WillC's WARP loop.
            RetailPadSnapshot {
                tapped: PAD_CROSS,
                held: PAD_CROSS,
                ..RetailPadSnapshot::default()
            }
        } else {
            RetailPadSnapshot::default()
        };
        bonus_runtime
            .set_pad_snapshot(0, pad)
            .expect("the bonus runtime must accept idle input");
        let report = bonus_runtime
            .run_frame(&mut bonus_host, INSTRUCTION_BUDGET)
            .unwrap_or_else(|error| panic!("bonus WARP frame {frame} failed: {error:?}"));
        assert!(
            report
                .executions
                .iter()
                .all(|execution| execution.result.is_ok()),
            "bonus WARP frame {frame} faulted: {:?}",
            report.executions
        );
        let load_states = report
            .effects
            .iter()
            .filter_map(|effect| match effect {
                VmEffect::LoadState { saved_level, .. } => Some(*saved_level),
                _ => None,
            })
            .collect::<Vec<_>>();
        if !load_states.is_empty() {
            assert_eq!(
                load_states.len(),
                1,
                "one authored WARP frame must emit one LoadState"
            );
            let captured = load_states[0]
                .expect("the retail runtime must resolve the WARP save level synchronously");
            load_state = Some((frame, captured));
            break;
        }
    }
    let (load_frame, captured_saved_level) =
        load_state.expect("WillC WARP must reach its authored LoadState branch");
    assert_eq!(load_frame, 301);
    assert_eq!(captured_saved_level, parent);
    assert!(
        !bonus_runtime.machine().level_restart_requested(),
        "different-level LoadState must finish the source GOOL traversal"
    );
    assert_eq!(
        bonus_runtime.restart_saved_level_from_effect(&mut bonus_host, captured_saved_level),
        Ok(RetailRestartOutcome::DifferentLevel {
            saved_level: parent,
            requested_level_sentinel: -2,
        })
    );
    let return_transition = bonus_runtime
        .finish_level_transition(&mut bonus_host, -2)
        .expect("the bonus LEVEL_END phase must resolve the carried parent snapshot");
    assert!(return_transition.event_failures.is_empty());
    assert_eq!(return_transition.next_lid_after_event, -2);
    assert_eq!(return_transition.resolved.level, parent);
    assert!(return_transition.resolved.bonus_return);

    // Reproduce the browser's protected destination mount: place its camera
    // at the carried path/progress, suppress the one initial Crash auto-save,
    // then perform native's same-level LevelRestart against the parent data.
    let expected_snapshot = return_transition
        .carry
        .saved_level_state
        .clone()
        .expect("the resolved parent mount must retain its complete snapshot");
    assert_eq!(
        expected_snapshot, original_parent_snapshot,
        "the complete parent snapshot must survive bonus execution and LEVEL_END"
    );
    let parent_graph = graph_for_pair(parent, &parent_nsd, &parent_nsf, &parent_nsf_bytes)
        .expect("the returned parent camera graph must parse");
    let (parent_zones, parent_lifecycle) = zone_catalog(
        &parent_nsd,
        &parent_nsf,
        &parent_nsf_bytes,
        &parent_graph,
        parent,
    )
    .expect("the returned parent zone catalog must parse");
    let game_state = return_transition
        .carry
        .globals
        .get(crust_sim::gool::GAME_STATE_GLOBAL)
        .copied()
        .expect("the session carry contains the game-state global")
        .cast_signed();
    let parent_camera = RetailCameraRuntime::at_path(
        &parent_graph,
        expected_snapshot.location.path,
        expected_snapshot.location.progress.raw(),
        game_state,
    )
    .expect("the returned parent camera must accept the saved location");
    assert_eq!(parent_camera.location(), expected_snapshot.location);

    let mut returned_runtime =
        RetailRuntime::new_from_session(GLOBAL_WORDS, parent, return_transition.carry)
            .expect("the parent stream must import the bonus return carry");
    seed_mounted_level_context_from_globals(
        &mut returned_runtime,
        &parent_graph,
        &parent_lifecycle,
        parent_camera.location(),
    )
    .expect("the returned parent must publish the saved camera context");
    let mut parent_host = NsfProgramHost::new(&parent_nsd, &parent_nsf, &parent_nsf_bytes);
    returned_runtime
        .create_retail_core_objects(parent_camera.location().path.zone, &mut parent_host)
        .expect("the returned parent core objects must materialize");
    returned_runtime
        .create_retail_level_misc_object(parent_camera.location().path.zone, &mut parent_host)
        .expect("the returned parent level-misc object must materialize");
    let returned_neighbors = parent_lifecycle
        .next_frame_spawn_scan()
        .iter()
        .map(|candidate| {
            let zone = parent_zones
                .get(&candidate.zone)
                .expect("the returned lifecycle zone exists in the parsed catalog");
            NeighborZone {
                eid: zone.eid,
                display_flags: candidate.display_flags,
                entities: zone.entities.as_slice(),
            }
        })
        .collect::<Vec<_>>();
    returned_runtime.set_initial_crash_save_suppressed(true);
    let protected_spawn =
        returned_runtime.spawn_current_zone_neighbors(&returned_neighbors, &mut parent_host);
    returned_runtime.set_initial_crash_save_suppressed(false);
    assert!(
        protected_spawn
            .iter()
            .all(|attempt| { attempt.result.is_ok() || expected_spawn_rejection(&attempt.result) })
    );
    assert_eq!(
        returned_runtime.saved_level_state(),
        Some(&expected_snapshot),
        "the protected Crash spawn must not replace the parent snapshot"
    );

    let RetailRestartOutcome::Restarted(restart) = returned_runtime
        .restart_saved_level(&mut parent_host)
        .expect("the protected parent restart must complete")
    else {
        panic!("the returned parent snapshot unexpectedly requested another remount");
    };
    assert_eq!(restart.snapshot, expected_snapshot);
    assert!(restart.respawn_event_failures.is_empty());
    assert!(
        restart
            .zone_reports
            .iter()
            .all(|(_, report)| report.event_failures.is_empty())
    );
    assert_eq!(restart.restored_box_count, expected_snapshot.box_count);
    assert_eq!(
        returned_runtime
            .level_state_context()
            .expect("the restarted parent retains a camera context")
            .location,
        expected_snapshot.location
    );
    assert_eq!(
        returned_runtime.arena().spawn_table().snapshot(),
        expected_snapshot.spawn_words.map(|word| word & !1),
        "first-spawn restoration keeps all 304 saved words and clears only the active bit"
    );
    let returned_player = returned_runtime
        .arena()
        .main_object()
        .and_then(|arena| returned_runtime.object_for_arena(arena))
        .and_then(|object| returned_runtime.machine().object(object.vm()).ok())
        .expect("the restarted parent must retain Crash");
    for (register, expected) in [
        (
            process_register::TRANSLATION_X,
            expected_snapshot.player_translation[0],
        ),
        (
            process_register::TRANSLATION_Y,
            expected_snapshot.player_translation[1],
        ),
        (
            process_register::TRANSLATION_Z,
            expected_snapshot.player_translation[2],
        ),
        (
            process_register::ROTATION_Y,
            expected_snapshot.player_rotation_yxz[0],
        ),
        (
            process_register::ROTATION_X,
            expected_snapshot.player_rotation_yxz[1],
        ),
        (
            process_register::ROTATION_Z,
            expected_snapshot.player_rotation_yxz[2],
        ),
        (process_register::SCALE_X, expected_snapshot.player_scale[0]),
        (process_register::SCALE_Y, expected_snapshot.player_scale[1]),
        (process_register::SCALE_Z, expected_snapshot.player_scale[2]),
    ] {
        assert_eq!(
            returned_player.register(register),
            Ok(expected.cast_unsigned())
        );
    }
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn jungle_rollers_three_tawna_crates_enter_the_authored_bonus() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let level = LevelId::new_const(0x0c);
    let (nsd, nsf, nsf_bytes) =
        parse_local_pair(&root, level).expect("Jungle Rollers pair must parse");
    let name = KNOWN_LEVELS
        .iter()
        .find(|known| known.id == level)
        .map(|known| known.name)
        .expect("Jungle Rollers must be in the retail catalog");
    let (_, mut runtime) = survey_pair_with_runtime(
        name,
        level,
        &nsd,
        &nsf,
        &nsf_bytes,
        RetailRuntime::new_for_level(GLOBAL_WORDS, level),
        LevelContextSource::FreshBoot,
        SurveyInputProfile::Idle,
        1,
    )
    .expect("Jungle Rollers must mount its authentic core objects");
    let player = runtime
        .arena()
        .main_object()
        .and_then(|arena| runtime.object_for_arena(arena))
        .expect("Jungle Rollers must spawn Crash");
    assert_eq!(
        runtime
            .machine()
            .object(player.vm())
            .unwrap()
            .register(0x48),
        Ok(0),
        "Tawna's process counter starts empty"
    );

    let graph = graph_for_pair(level, &nsd, &nsf, &nsf_bytes)
        .expect("Jungle Rollers zone graph must parse");
    let (zones, _) = zone_catalog(&nsd, &nsf, &nsf_bytes, &graph, level).expect("zones must parse");
    let boxs = Eid::from_name("BoxsC").expect("fixed retail crate EID is valid");
    let fruic = Eid::from_name("FruiC").expect("fixed retail pickup EID is valid");
    let token_break = load_gool_state_program(&nsd, &nsf, &nsf_bytes, boxs, 24)
        .expect("BoxsC token-break state must load");
    let token_pickup = load_gool_state_program(&nsd, &nsf, &nsf_bytes, fruic, 11)
        .expect("FruiC token-pickup state must load");
    assert_eq!(token_break.code_pc(), Some(0x437));
    assert_eq!(token_break.code().get(0x466), Some(&0x9120_3341));
    assert_eq!(token_pickup.code_pc(), Some(0x1f9));
    assert_eq!(token_pickup.code().get(0x3c4), Some(&0x87a4_0e47));

    let token_crates = [("0h_cZ", 22_u16), ("0w_cZ", 59), ("0G_cZ", 79)];
    let expected_counter_frames = [6_u32, 7, 7];
    let mut host = NsfProgramHost::new(&nsd, &nsf, &nsf_bytes);
    let pickup_hud = CollisionObjectReference::from_word(runtime.global_word(14).unwrap())
        .and_then(|reference| runtime.object_for_vm(reference.object()))
        .expect("global 14 must retain the live DispC pickup HUD");
    assert_eq!(
        runtime.machine().object(pickup_hud.vm()).unwrap().state(),
        11,
        "DispC subtype five starts in its authored pickup-HUD initialization state"
    );
    let mut pickup_hud_ready = None;
    for frame in 1..=180_u32 {
        runtime.set_frame_timing(34, 34);
        let report = runtime
            .run_frame(&mut host, INSTRUCTION_BUDGET)
            .unwrap_or_else(|error| panic!("pickup HUD warm-up frame {frame}: {error:?}"));
        assert!(
            report
                .executions
                .iter()
                .all(|execution| execution.result.is_ok())
        );
        if runtime.machine().object(pickup_hud.vm()).unwrap().state() == 12 {
            pickup_hud_ready = Some(frame);
            break;
        }
    }
    assert_eq!(
        pickup_hud_ready,
        Some(5),
        "the core pickup HUD must reach its accepting state before gameplay tokens arrive"
    );
    let initial_parent_snapshot = runtime
        .saved_level_state()
        .cloned()
        .expect("fresh Jungle Rollers must retain its authentic initial snapshot");
    let mut third_token_snapshot = None;

    for (token_index, (zone_name, entity_id)) in token_crates.into_iter().enumerate() {
        let zone_eid = Eid::from_name(zone_name).expect("fixed retail zone EID is valid");
        let zone = zones.get(&zone_eid).expect("token zone must be parsed");
        let entity = zone
            .entities
            .iter()
            .find(|entity| entity.id == entity_id)
            .unwrap_or_else(|| panic!("token entity {entity_id} must exist in {zone_name}"))
            .clone();
        assert_eq!(
            (
                entity.group,
                entity.executable,
                entity.subtype,
                entity.initializer[0]
            ),
            (3, 0x22, 10, 0x69),
            "the local descriptor must be an authored Tawna token crate"
        );
        let display_flags = graph
            .zone(zone_eid)
            .expect("token zone must exist in its graph")
            .display_flags
            | 2;
        let token_entities = [entity];
        let attempts = runtime.spawn_current_zone_neighbors(
            &[NeighborZone {
                eid: zone_eid,
                display_flags,
                entities: &token_entities,
            }],
            &mut host,
        );
        assert_eq!(attempts.len(), 1);
        let token_box = *attempts[0]
            .result
            .as_ref()
            .unwrap_or_else(|error| panic!("token crate {entity_id} must spawn: {error:?}"));

        // Entity binding selects BoxsC state zero, but the native object gets
        // one cooperative visit before an interaction can break it. Preserve
        // that initialization visit so state 24 reads the initializer-derived
        // token kind rather than an uninitialized process register.
        runtime.set_frame_timing(34, 34);
        let initialized = runtime
            .run_frame(&mut host, INSTRUCTION_BUDGET)
            .unwrap_or_else(|error| panic!("token crate {entity_id} init frame: {error:?}"));
        assert!(
            initialized
                .executions
                .iter()
                .all(|execution| execution.result.is_ok()),
            "token crate {entity_id} initialization faulted: {:?}",
            initialized.executions
        );

        let dispatch = runtime
            .dispatch_event(&mut host, Some(player), Some(token_box), 0x0300, Some(&[0]))
            .unwrap_or_else(|error| panic!("token crate {entity_id} break event: {error:?}"));
        assert_eq!(
            dispatch.state_change.as_ref().map(|change| change.state),
            Some(24),
            "the authored player-hit event enters BoxsC state 24"
        );

        let previous_counter = u32::try_from(token_index).unwrap() << 8;
        let expected_counter = u32::try_from(token_index + 1).unwrap() << 8;
        assert_eq!(
            runtime
                .machine()
                .object(player.vm())
                .unwrap()
                .register(0x48),
            Ok(previous_counter)
        );
        let mut reached_counter = None;
        let mut token_spawns = Vec::new();
        let mut token_pickup = None;
        let mut hud_deliveries = Vec::new();
        let mut combo_deliveries = Vec::new();
        let mut compact_trace = Vec::new();
        let mut save_state_frames = Vec::new();
        for frame in 1..=64_u32 {
            runtime.set_frame_timing(34, 34);
            runtime
                .set_pad_snapshot(0, RetailPadSnapshot::default())
                .expect("idle pad snapshot must be valid");
            let report = runtime
                .run_frame(&mut host, INSTRUCTION_BUDGET)
                .unwrap_or_else(|error| {
                    panic!("token crate {entity_id} frame {frame} failed: {error:?}")
                });
            assert!(
                report
                    .executions
                    .iter()
                    .all(|execution| execution.result.is_ok()),
                "token crate {entity_id} frame {frame} faulted: {:?}",
                report.executions
            );
            for effect in &report.effects {
                if matches!(effect, VmEffect::SaveState(object) if *object == pickup_hud.vm()) {
                    save_state_frames.push(frame);
                    let saved = runtime
                        .saved_level_state()
                        .cloned()
                        .expect("DispC SaveState must install a complete parent snapshot");
                    assert!(
                        third_token_snapshot.replace(saved).is_none(),
                        "the authentic route must install exactly one third-token snapshot"
                    );
                }
                match effect {
                    VmEffect::SpawnChildren {
                        parent,
                        executable: 3,
                        subtype: 13,
                        count: 1,
                        arguments,
                        ..
                    } if *parent == token_box.vm() => {
                        token_spawns.push((frame, arguments.clone()));
                        let direct_children = report
                            .spawned_children
                            .iter()
                            .copied()
                            .filter(|child| {
                                runtime
                                    .machine()
                                    .object(child.vm())
                                    .ok()
                                    .and_then(crust_sim::gool::VmObject::program_identity)
                                    .is_some_and(|identity| identity.global_eid() == fruic)
                            })
                            .collect::<Vec<_>>();
                        assert_eq!(
                            direct_children.len(),
                            1,
                            "BoxsC's one-child effect must materialize one FruiC program"
                        );
                        assert!(
                            token_pickup.replace(direct_children[0]).is_none(),
                            "BoxsC must emit exactly one matching token-child effect"
                        );
                    }
                    VmEffect::SendEvent(request)
                        if request.target
                            == SendEventTarget::Direct {
                                recipient: player.vm(),
                            }
                            && request.event == 0x1000 =>
                    {
                        combo_deliveries.push((
                            frame,
                            request.sender,
                            request.arguments().to_vec(),
                        ));
                    }
                    VmEffect::SendEvent(request)
                        if request.target
                            == SendEventTarget::Direct {
                                recipient: pickup_hud.vm(),
                            }
                            && request.event == 0x2000 =>
                    {
                        hud_deliveries.push((frame, request.sender, request.arguments().to_vec()));
                    }
                    _ => {}
                }
                if compact_trace.len() < 32
                    && matches!(
                        effect,
                        VmEffect::SpawnChildren { .. } | VmEffect::SendEvent(_)
                    )
                {
                    compact_trace.push(format!("frame {frame}: {effect:?}"));
                }
            }
            let counter = runtime
                .machine()
                .object(player.vm())
                .expect("Crash must remain live")
                .register(0x48)
                .expect("Tawna counter register must remain valid");
            assert!(
                counter == previous_counter || counter == expected_counter,
                "token crate {entity_id} skipped a retail counter step: {counter:#x}"
            );
            if counter == expected_counter {
                reached_counter = Some(frame);
                break;
            }
        }
        let counter_frame = reached_counter.unwrap_or_else(|| {
            let counter = runtime
                .machine()
                .object(player.vm())
                .and_then(|object| object.register(0x48))
                .unwrap_or(u32::MAX);
            panic!(
                "token crate {entity_id} never advanced Tawna's counter to {expected_counter:#x}; final={counter:#x}; trace={compact_trace:#?}"
            )
        });
        let token_pickup = token_pickup.expect("BoxsC must materialize its subtype-13 FruiC child");
        assert_eq!(
            token_spawns,
            vec![(1, vec![0x6900, u32::from(entity_id) << 8])],
            "BoxsC must spawn the authored subtype-13 token pickup"
        );
        assert_eq!(
            hud_deliveries,
            vec![(
                3,
                token_pickup.vm(),
                vec![0x6900, u32::from(entity_id) << 8]
            )],
            "FruiC must deliver the token kind and crate PID to DispC"
        );
        assert_eq!(
            combo_deliveries,
            vec![(counter_frame, token_pickup.vm(), vec![0x6900])],
            "FruiC must deliver Tawna's token kind through Crash's combo event"
        );
        assert_eq!(
            counter_frame, expected_counter_frames[token_index],
            "the cooperative token path must retain its deterministic timing"
        );
        assert_eq!(
            save_state_frames,
            if token_index == 2 { vec![4] } else { vec![] },
            "only DispC's third-token handshake performs the parent-level save"
        );
        assert_eq!(
            runtime.machine().object(pickup_hud.vm()).unwrap().state(),
            13,
            "DispC must wait in its counter-synchronization state"
        );

        let mut hud_settled_frame = None;
        let mut completion_delivery = None;
        let mut reset_master_fade_frame = None;
        let mut completion_status_delivery = None;
        let mut transition = None;
        let mut post_counter_save = false;
        let mut saw_hud_state_13 = runtime.machine().object(pickup_hud.vm()).unwrap().state() == 13;
        for frame in 1..=180_u32 {
            runtime.set_frame_timing(34, 34);
            let report = runtime
                .run_frame(&mut host, INSTRUCTION_BUDGET)
                .unwrap_or_else(|error| panic!("Tawna HUD frame {frame}: {error:?}"));
            assert!(
                report
                    .executions
                    .iter()
                    .all(|execution| execution.result.is_ok()),
                "Tawna HUD frame {frame} faulted: {:?}",
                report.executions
            );
            for effect in &report.effects {
                match effect {
                    VmEffect::SaveState(object) if *object == pickup_hud.vm() => {
                        post_counter_save = true;
                    }
                    VmEffect::SendEvent(request)
                        if request.sender == pickup_hud.vm()
                            && request.target
                                == SendEventTarget::Direct {
                                    recipient: player.vm(),
                                }
                            && request.event == 0x2700 =>
                    {
                        completion_delivery = Some((frame, request.arguments().to_vec()));
                    }
                    VmEffect::ResetMasterFadeStep { object } if *object == pickup_hud.vm() => {
                        reset_master_fade_frame = Some(frame);
                    }
                    VmEffect::SendEvent(request)
                        if request.sender == pickup_hud.vm()
                            && request.target
                                == SendEventTarget::Direct {
                                    recipient: player.vm(),
                                }
                            && request.event == 0x0f00
                            && request.arguments() == [0x500] =>
                    {
                        completion_status_delivery = Some((frame, request.arguments().to_vec()));
                    }
                    VmEffect::Transition(destination) => {
                        transition = Some((frame, *destination));
                    }
                    _ => {}
                }
            }
            let hud_state = runtime.machine().object(pickup_hud.vm()).unwrap().state();
            saw_hud_state_13 |= hud_state == 13;
            if token_index < 2 && saw_hud_state_13 && hud_state == 12 {
                hud_settled_frame = Some(frame);
            }
            if hud_settled_frame.is_some() || transition.is_some() {
                break;
            }
        }

        assert!(
            !post_counter_save,
            "the third-token save must precede its counter increment"
        );
        if token_index < 2 {
            assert_eq!(
                runtime.saved_level_state(),
                Some(&initial_parent_snapshot),
                "the first two tokens must not replace Jungle Rollers' saved state"
            );
            assert_eq!(hud_settled_frame, Some(60));
            assert_eq!(completion_delivery, None);
            assert_eq!(reset_master_fade_frame, None);
            assert_eq!(completion_status_delivery, None);
            assert_eq!(transition, None);
            assert_eq!(runtime.global_word(CHECKPOINT_ID_GLOBAL), Ok(u32::MAX));
            assert_eq!(
                runtime
                    .machine()
                    .object(player.vm())
                    .unwrap()
                    .register(0x48),
                Ok(expected_counter)
            );
        } else {
            assert_eq!(hud_settled_frame, None);
            assert_eq!(completion_delivery, Some((1, vec![0])));
            assert_eq!(reset_master_fade_frame, Some(38));
            assert_eq!(completion_status_delivery, Some((53, vec![0x500])));
            assert_eq!(transition, Some((53, 0x24)));
            assert_eq!(
                runtime.global_word(CHECKPOINT_ID_GLOBAL),
                Ok(u32::from(entity_id) << 8)
            );
            assert_eq!(
                runtime
                    .machine()
                    .object(player.vm())
                    .unwrap()
                    .register(0x48),
                Ok(0),
                "DispC clears Tawna's counter immediately before the bonus transition"
            );
            assert_eq!(
                runtime.saved_level_state().map(|snapshot| snapshot.level),
                Some(level)
            );
        }
    }

    let third_token_snapshot = third_token_snapshot
        .expect("the third token must replace Jungle Rollers' initial saved state");
    assert_ne!(
        third_token_snapshot.spawn_words, initial_parent_snapshot.spawn_words,
        "the third-token save must capture the route's changed persistent crate state"
    );
    assert_eq!(
        runtime.saved_level_state(),
        Some(&third_token_snapshot),
        "the exact third-token snapshot must remain live until LEVEL_END"
    );

    let transition = runtime
        .finish_level_transition(&mut host, 0x24)
        .expect("the authentic third-token destination must complete LEVEL_END");
    assert!(transition.event_failures.is_empty());
    assert_eq!(transition.next_lid_after_event, 0x24);
    assert_eq!(transition.resolved.level, LevelId::new_const(0x24));
    assert!(!transition.resolved.bonus_return);
    assert_eq!(
        transition.carry.saved_level_state.as_ref(),
        Some(&third_token_snapshot),
        "the exact third-token SaveState must carry Jungle Rollers into Tawna Bonus"
    );
    let bonus_runtime =
        RetailRuntime::new_from_session(GLOBAL_WORDS, LevelId::new_const(0x24), transition.carry)
            .expect("Tawna Bonus must import the exact third-token session carry");
    assert_eq!(
        bonus_runtime.saved_level_state(),
        Some(&third_token_snapshot),
        "the fresh Tawna Bonus runtime must retain Jungle Rollers' third-token snapshot"
    );
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn n_sanity_idle_paging_matches_the_legal_360_frame_trace() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let level = LevelId::N_SANITY_BEACH;
    let known = KNOWN_LEVELS
        .iter()
        .find(|known| known.id == level)
        .expect("the retail level catalog contains N. Sanity Beach");
    let (nsd, nsf, nsf_bytes) =
        parse_local_pair(&root, level).expect("N. Sanity's local stream pair must parse");
    let survey = survey_pair(
        known.name,
        level,
        &nsd,
        &nsf,
        &nsf_bytes,
        SurveyInputProfile::Idle,
        DEFAULT_SURVEY_FRAMES,
    )
    .expect("N. Sanity's 360-frame idle characterization must execute");

    let entry = |frame, operation, name, page| PagingTraceEntry {
        frame,
        object: VmObjectHandle::new(6).expect("retail paging requester handle is valid"),
        operation,
        eid: Eid::from_name(name).expect("characterized paging EID is valid"),
        page: PageIndex::new(page),
    };
    let expected = [
        entry(2, PagingHostOperation::Open, "WiI1V", 63),
        entry(2, PagingHostOperation::Open, "WillG", 16),
        entry(3, PagingHostOperation::Open, "WiI2V", 69),
        entry(3, PagingHostOperation::Open, "WillG", 16),
        entry(30, PagingHostOperation::Close, "WiI1V", 63),
        entry(30, PagingHostOperation::Close, "WillG", 16),
        entry(30, PagingHostOperation::Open, "WiI3V", 75),
        entry(30, PagingHostOperation::Open, "WillG", 16),
        entry(46, PagingHostOperation::Close, "WiI2V", 69),
        entry(46, PagingHostOperation::Close, "WillG", 16),
        entry(46, PagingHostOperation::Open, "WiI4V", 65),
        entry(46, PagingHostOperation::Open, "WillG", 16),
        entry(83, PagingHostOperation::Close, "WiI3V", 75),
        entry(83, PagingHostOperation::Close, "WillG", 16),
        entry(83, PagingHostOperation::Open, "WiI5V", 66),
        entry(83, PagingHostOperation::Open, "WillG", 16),
        entry(120, PagingHostOperation::Close, "WiI4V", 65),
        entry(120, PagingHostOperation::Close, "WillG", 16),
        entry(120, PagingHostOperation::Open, "WiI6V", 70),
        entry(120, PagingHostOperation::Open, "WillG", 16),
        entry(145, PagingHostOperation::Close, "WiI5V", 66),
        entry(145, PagingHostOperation::Close, "WillG", 16),
        entry(194, PagingHostOperation::Close, "WiI6V", 70),
        entry(194, PagingHostOperation::Close, "WillG", 16),
    ];
    assert_eq!(survey.frames, DEFAULT_SURVEY_FRAMES);
    assert_eq!(survey.paging_trace, expected);

    let mut opens = 0_u32;
    let mut closes = 0_u32;
    let mut probes = 0_u32;
    let mut per_eid_delta = BTreeMap::<Eid, i32>::new();
    for request in &survey.paging_trace {
        match request.operation {
            PagingHostOperation::Open => {
                opens += 1;
                *per_eid_delta.entry(request.eid).or_default() += 1;
            }
            PagingHostOperation::Close => {
                closes += 1;
                *per_eid_delta.entry(request.eid).or_default() -= 1;
            }
            PagingHostOperation::Probe => probes += 1,
        }
    }
    assert_eq!((opens, closes, probes), (12, 12, 0));
    assert!(
        per_eid_delta.values().all(|delta| *delta == 0),
        "every characterized paging EID must finish with a zero open/close delta: {per_eid_delta:?}"
    );

    // This asset-only host intentionally uses ProgramHost's deterministic
    // acknowledgement. Browser tests separately characterize slot exhaustion.
    let mut host = NsfProgramHost::new(&nsd, &nsf, &nsf_bytes);
    for request in expected {
        let response = host
            .handle_paging_request(PagingHostRequest {
                object: request.object,
                operation: request.operation,
                reference: request.page.tagged(),
                eid: request.eid,
                page: request.page,
                was_resolved: false,
            })
            .expect("the asset-only paging host is infallible");
        assert_eq!(response, PagingHostResponse::Applied { evicted: None });
    }
    assert!(
        survey.is_clean(),
        "paging characterization reached a checked runtime boundary: {}",
        survey.summary()
    );
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn n_sanity_goal_directed_input_characterizes_progression() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let known = KNOWN_LEVELS
        .iter()
        .find(|known| known.id == LevelId::N_SANITY_BEACH)
        .expect("the retail level catalog contains N. Sanity Beach");
    let (nsd_bytes, nsf_bytes) = read_pair(&root, known.id).unwrap();
    let nsd = parse_nsd(&nsd_bytes, known.id).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
    let frames = progression_frame_count();
    let survey = survey_pair(
        known.name,
        known.id,
        &nsd,
        &nsf,
        &nsf_bytes,
        SurveyInputProfile::ForwardWithActions,
        frames,
    )
    .unwrap();
    eprintln!("{}", survey.summary());

    assert!(
        survey.frames == frames || survey.next_lid.is_some(),
        "the progression survey must reach its bounded window or a real transition"
    );
    assert!(survey.successful_spawns > 0, "retail entities must spawn");
    assert!(
        survey.initial_player_translation.is_some() && survey.final_player_translation.is_some(),
        "the real retail Crash object must become observable"
    );
    assert!(
        survey.first_below_zero.is_none() && survey.first_terminal_fall.is_none(),
        "the authored route must not cross either observed fall boundary: {}",
        survey.summary()
    );
    if frames >= 320 {
        let crab_contact_sends = survey
            .early_direct_send_samples
            .iter()
            .copied()
            .filter(|(frame, _)| (300..=320).contains(frame))
            .collect::<Vec<_>>();
        assert_eq!(
            crab_contact_sends,
            [(311, 0x400), (312, 0x1000), (312, 0x400)],
            "the first Crab contact window must retain its source-ordered spin/defeat events: {}",
            survey.summary()
        );
        assert!(
            !survey
                .early_direct_send_samples
                .iter()
                .any(|(frame, event)| (300..=320).contains(frame) && *event == 0x300),
            "CrabC's grounded-contact gate must not emit its direct 0x300 event before the authored defeat: {}",
            survey.summary()
        );
    }
    if frames >= 900 {
        assert!(
            survey.box_count_samples.starts_with(&[
                (1, 0),
                (207, 0x100),
                (334, 0x200),
                (512, 0x300),
                (644, 0x400),
                (651, 0x500),
                (683, 0x600),
                (685, 0x700),
                (762, 0x800),
                (787, 0x900),
                (861, 0xa00),
            ]),
            "the authored route must break the first nine counted boxes and checkpoint at its deterministic source-ordered boundaries: {}",
            survey.summary()
        );
        assert_eq!(
            survey.checkpoint_samples,
            [
                (1, -1, [0, 0, 0]),
                (861, 19 << 8, [1_945_600, 4_135_168, 24_165_632]),
            ],
            "entity 19 must capture the first retail checkpoint: {}",
            survey.summary()
        );
        assert!(
            survey.spawn_flag_samples.contains(&(312, 14, 3)),
            "the first CrabC defeat must publish entity 14's native spawn flags: {}",
            survey.summary()
        );
        assert!(
            survey.spawn_flag_samples.contains(&(334, 12, 3)),
            "the first BoxsC break must publish entity 12's native spawn flags: {}",
            survey.summary()
        );
        assert_eq!(
            survey.saved_box_count_samples,
            [(861, 0x900)],
            "the checkpoint must save the live pre-increment box count at its synchronous source boundary: {}",
            survey.summary()
        );
        for zone_name in [
            "a1_9Z", "a2_9Z", "a3_9Z", "a4_9Z", "a5_9Z", "a6_9Z", "a7_9Z",
        ] {
            let zone = Eid::from_name(zone_name).unwrap();
            assert!(
                survey.camera_ranges.keys().any(|path| path.zone == zone),
                "the authored controller did not reach {zone_name}: {}",
                survey.summary()
            );
        }
    }
    if frames >= 1_400 {
        for zone_name in ["a8_9Z", "a9_9Z", "b0_9Z", "b1_9Z"] {
            let zone = Eid::from_name(zone_name).unwrap();
            assert!(
                survey.camera_ranges.keys().any(|path| path.zone == zone),
                "the authored controller did not reach {zone_name}: {}",
                survey.summary()
            );
        }
    }
    if frames >= 1_700 {
        for zone_name in ["2b_9Z", "3b_9Z", "4b_9Z"] {
            let zone = Eid::from_name(zone_name).unwrap();
            assert!(
                survey.camera_ranges.keys().any(|path| path.zone == zone),
                "the authored controller did not reach {zone_name}: {}",
                survey.summary()
            );
        }
    }
    if frames >= 1_850 {
        for zone_name in ["b5_9Z", "b6_9Z"] {
            let zone = Eid::from_name(zone_name).unwrap();
            assert!(
                survey.camera_ranges.keys().any(|path| path.zone == zone),
                "the authored controller did not reach {zone_name}: {}",
                survey.summary()
            );
        }
    }
    if frames >= 1_900 {
        let b7 = Eid::from_name("b7_9Z").unwrap();
        assert!(
            survey.camera_ranges.keys().any(|path| path.zone == b7),
            "the authored controller did not reach b7_9Z: {}",
            survey.summary()
        );
    }
    if frames >= 1_900 {
        assert_eq!(
            survey.next_lid,
            Some((1_900, 0x2d)),
            "the authored end warp must request Level Complete: {}",
            survey.summary()
        );
        assert_eq!(
            survey.restarts,
            0,
            "the complete route must not require a death/restart: {}",
            survey.summary()
        );
    }
    assert!(
        survey.is_clean(),
        "goal-directed progression reached a checked runtime boundary: {}",
        survey.summary()
    );
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn n_sanity_checkpoint_survives_an_authored_death_restart() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let known = KNOWN_LEVELS
        .iter()
        .find(|known| known.id == LevelId::N_SANITY_BEACH)
        .expect("the retail level catalog contains N. Sanity Beach");
    let (nsd_bytes, nsf_bytes) = read_pair(&root, known.id).unwrap();
    let nsd = parse_nsd(&nsd_bytes, known.id).unwrap();
    let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
    let survey = survey_pair(
        known.name,
        known.id,
        &nsd,
        &nsf,
        &nsf_bytes,
        SurveyInputProfile::ForwardThroughCheckpointThenA8Hit,
        1_400,
    )
    .unwrap();
    eprintln!("{}", survey.summary());

    assert_eq!(
        survey.box_count_samples,
        [
            (1, 0),
            (207, 0x100),
            (334, 0x200),
            (512, 0x300),
            (644, 0x400),
            (651, 0x500),
            (683, 0x600),
            (685, 0x700),
            (762, 0x800),
            (787, 0x900),
            (861, 0xa00),
            (1_151, 0),
            (1_152, 0x100),
        ],
        "same-level restart must reproduce native LevelInitMisc box reset and checkpoint respawn accounting"
    );
    assert_eq!(
        survey.checkpoint_samples,
        [
            (1, -1, [0, 0, 0]),
            (861, 19 << 8, [1_945_600, 4_135_168, 24_165_632]),
        ],
        "the checkpoint identity and spawn translation must survive the death restart"
    );
    assert_eq!(survey.saved_box_count_samples, [(861, 0x900)]);
    assert!(survey.spawn_flag_samples.contains(&(312, 14, 3)));
    assert!(survey.spawn_flag_samples.contains(&(334, 12, 3)));
    assert_eq!(survey.restart_frames, [1_150]);
    assert_eq!(survey.effect_counts.get("save-state"), Some(&1));
    assert_eq!(survey.effect_counts.get("load-state"), Some(&1));
    assert_eq!(survey.death_camera_frames, 117);
    assert_eq!(survey.death_camera_pose_changes, 116);
    assert_eq!(survey.death_camera_max_count, 9);
    assert_eq!(
        survey.first_death_camera_pose.map(|(frame, _pose)| frame),
        Some(1_035)
    );
    assert!(survey.first_below_zero.is_none());
    assert!(survey.first_terminal_fall.is_none());
    assert!(survey.next_lid.is_none());
    assert!(
        survey.is_clean(),
        "checkpoint/death route reached a checked runtime boundary: {}",
        survey.summary()
    );
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn authored_first_four_levels_reach_upstream_with_session_carry() {
    const N_SANITY_FRAMES: u32 = 2_100;
    const COMPLETION_FRAMES: u32 = 600;

    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let known_name = |level| {
        KNOWN_LEVELS
            .iter()
            .find(|known| known.id == level)
            .map(|known| known.name)
            .expect("vertical-flow level is present in the retail catalog")
    };

    let title = LevelId::TITLE;
    let (title_nsd, title_nsf, title_nsf_bytes) =
        parse_local_pair(&root, title).expect("Title pair must parse");
    let mut initial_map = AuthoredTitleMapHarness::fresh(&title_nsd, &title_nsf, &title_nsf_bytes);
    initial_map.wait_until_ready(64);
    assert_eq!(initial_map.frame, 10, "initial Title Map ready-frame drift");
    initial_map.step(PAD_CROSS);
    assert_eq!(
        initial_map.transitions,
        [(11, i32::try_from(LevelId::N_SANITY_BEACH.get()).unwrap())],
        "the first unlocked map node must request N. Sanity Beach"
    );
    let initial_map_carry = {
        let mut host = NsfProgramHost::new(&title_nsd, &title_nsf, &title_nsf_bytes);
        let report = initial_map
            .runtime
            .finish_level_transition(
                &mut host,
                i32::try_from(LevelId::N_SANITY_BEACH.get()).unwrap(),
            )
            .expect("initial Title Map LEVEL_END must export a session carry");
        assert!(
            report.event_failures.is_empty(),
            "initial Title Map LEVEL_END handlers must complete cleanly: {:?}",
            report.event_failures
        );
        assert_eq!(report.resolved.level, LevelId::N_SANITY_BEACH);
        assert!(!report.resolved.bonus_return);
        report.carry
    };

    let n_sanity = LevelId::N_SANITY_BEACH;
    let (n_sanity_nsd, n_sanity_nsf, n_sanity_nsf_bytes) =
        parse_local_pair(&root, n_sanity).expect("N. Sanity pair must parse");
    let n_sanity_runtime =
        RetailRuntime::new_from_session(GLOBAL_WORDS, n_sanity, initial_map_carry)
            .expect("N. Sanity must import the authored initial-map carry");
    assert_eq!(n_sanity_runtime.global_word(GAME_STATE_GLOBAL), Ok(0));
    assert_eq!(
        n_sanity_runtime.global_word(TITLE_STATE_GLOBAL),
        Ok(TitleScreen::Map.raw())
    );
    assert_eq!(
        n_sanity_runtime.global_word(SAVED_TITLE_STATE_GLOBAL),
        Ok(TitleScreen::Map.raw())
    );
    assert_eq!(
        n_sanity_runtime.global_word(CURRENT_MAP_LEVEL_GLOBAL),
        Ok(1)
    );
    assert_eq!(n_sanity_runtime.global_word(LEVEL_COUNT_GLOBAL), Ok(1));
    assert_eq!(n_sanity_runtime.global_word(LEVELS_UNLOCKED_GLOBAL), Ok(1));
    let (n_sanity_survey, mut n_sanity_runtime) = survey_pair_with_runtime(
        known_name(n_sanity),
        n_sanity,
        &n_sanity_nsd,
        &n_sanity_nsf,
        &n_sanity_nsf_bytes,
        n_sanity_runtime,
        LevelContextSource::SessionGlobals,
        SurveyInputProfile::ForwardWithActions,
        N_SANITY_FRAMES,
    )
    .expect("N. Sanity authored route must execute");
    assert_eq!(
        n_sanity_survey.next_lid,
        Some((1_900, i32::try_from(LevelId::LEVEL_COMPLETE.get()).unwrap())),
        "N. Sanity's authored end warp must request Level Complete: {}",
        n_sanity_survey.summary()
    );
    assert_eq!(
        n_sanity_survey.restarts,
        0,
        "the authored route must not require a restart: {}",
        n_sanity_survey.summary()
    );
    assert!(
        n_sanity_survey.is_clean(),
        "N. Sanity reached a checked runtime boundary: {}",
        n_sanity_survey.summary()
    );

    let n_sanity_draw_count = n_sanity_runtime.draw_count();
    assert_eq!(
        n_sanity_draw_count, 1_911,
        "N. Sanity completion draw-count drift"
    );
    let completion_carry: RetailSessionCarry = {
        let mut host = NsfProgramHost::new(&n_sanity_nsd, &n_sanity_nsf, &n_sanity_nsf_bytes);
        let report = n_sanity_runtime
            .finish_level_transition(
                &mut host,
                i32::try_from(LevelId::LEVEL_COMPLETE.get()).unwrap(),
            )
            .expect("N. Sanity LEVEL_END must export a session carry");
        assert!(
            report.event_failures.is_empty(),
            "N. Sanity LEVEL_END handlers must complete cleanly: {:?}",
            report.event_failures
        );
        assert_eq!(report.resolved.level, LevelId::LEVEL_COMPLETE);
        assert!(!report.resolved.bonus_return);
        assert_eq!(report.carry.draw_count, n_sanity_draw_count);
        report.carry
    };

    let completion = LevelId::LEVEL_COMPLETE;
    let (completion_nsd, completion_nsf, completion_nsf_bytes) =
        parse_local_pair(&root, completion).expect("Level Complete pair must parse");
    let completion_runtime =
        RetailRuntime::new_from_session(GLOBAL_WORDS, completion, completion_carry)
            .expect("Level Complete must import N. Sanity's session carry");
    assert_eq!(completion_runtime.draw_count(), n_sanity_draw_count);
    let (completion_survey, mut completion_runtime) = survey_pair_with_runtime(
        known_name(completion),
        completion,
        &completion_nsd,
        &completion_nsf,
        &completion_nsf_bytes,
        completion_runtime,
        LevelContextSource::SessionGlobals,
        SurveyInputProfile::DirectionAndButtonSweepToTransition,
        COMPLETION_FRAMES,
    )
    .expect("Level Complete authored runtime must execute");
    assert_eq!(
        completion_survey.next_lid,
        Some((513, i32::try_from(LevelId::TITLE.get()).unwrap())),
        "authored completion input must request Title: {}",
        completion_survey.summary()
    );
    assert!(
        completion_survey.is_clean(),
        "Level Complete reached a checked runtime boundary: {}",
        completion_survey.summary()
    );

    let completion_draw_count = completion_runtime.draw_count();
    assert_eq!(
        completion_draw_count, 2_424,
        "Level Complete draw-count drift"
    );
    let title_carry: RetailSessionCarry = {
        let mut host = NsfProgramHost::new(&completion_nsd, &completion_nsf, &completion_nsf_bytes);
        let report = completion_runtime
            .finish_level_transition(&mut host, i32::try_from(LevelId::TITLE.get()).unwrap())
            .expect("Level Complete LEVEL_END must export a session carry");
        assert!(
            report.event_failures.is_empty(),
            "Level Complete LEVEL_END handlers must complete cleanly: {:?}",
            report.event_failures
        );
        assert_eq!(report.resolved.level, LevelId::TITLE);
        assert!(!report.resolved.bonus_return);
        assert_eq!(report.carry.draw_count, completion_draw_count);
        report.carry
    };

    assert_eq!(title_carry.globals[GAME_STATE_GLOBAL], 0x300);
    assert_eq!(
        title_carry.globals[TITLE_STATE_GLOBAL],
        TitleScreen::Map.raw()
    );
    assert_eq!(
        title_carry.globals[SAVED_TITLE_STATE_GLOBAL],
        TitleScreen::Map.raw()
    );
    assert_eq!(title_carry.globals[CURRENT_MAP_LEVEL_GLOBAL], 1);
    assert_eq!(title_carry.globals[LEVEL_COUNT_GLOBAL], 1);
    assert_eq!(title_carry.globals[LEVELS_UNLOCKED_GLOBAL], 2);

    let mut post_completion_map = AuthoredTitleMapHarness::from_session(
        &title_nsd,
        &title_nsf,
        &title_nsf_bytes,
        title_carry,
    );
    assert_eq!(
        post_completion_map.runtime.draw_count(),
        completion_draw_count
    );
    post_completion_map.wait_until_ready(64);
    assert_eq!(
        post_completion_map.frame, 10,
        "post-completion Title Map ready-frame drift"
    );
    for _ in 0..120 {
        post_completion_map.step(0);
    }
    post_completion_map.tap(PAD_UP);
    for _ in 0..120 {
        post_completion_map.step(0);
    }
    post_completion_map.step(PAD_CROSS);
    assert_eq!(
        post_completion_map.frame, 253,
        "post-completion Map input-frame drift"
    );
    assert_eq!(
        post_completion_map.transitions,
        [(253, 0x0c)],
        "Up then Cross must request Jungle Rollers"
    );
    let post_map_location = post_completion_map.camera.location();
    assert_eq!(
        post_map_location.path,
        RetailPathId {
            zone: Eid::from_name("1b_pZ").expect("fixed second map-zone EID is valid"),
            index: 0,
        }
    );
    assert_eq!(post_map_location.progress.raw(), 0x0b00);
    assert_eq!(
        post_completion_map
            .runtime
            .global_word(CURRENT_MAP_LEVEL_GLOBAL),
        Ok(2)
    );
    assert_eq!(
        post_completion_map.runtime.global_word(LEVEL_COUNT_GLOBAL),
        Ok(1)
    );
    assert_eq!(
        post_completion_map
            .runtime
            .global_word(LEVELS_UNLOCKED_GLOBAL),
        Ok(2)
    );
    assert_eq!(
        post_completion_map
            .runtime
            .global_word(ISLAND_CAMERA_STATE_GLOBAL),
        Ok(1)
    );
    assert_eq!(
        post_completion_map.runtime.faulted_object_count(),
        0,
        "post-completion Map must retain no faulted authored object"
    );
    let jungle_rollers_carry = {
        let mut host = NsfProgramHost::new(&title_nsd, &title_nsf, &title_nsf_bytes);
        let report = post_completion_map
            .runtime
            .finish_level_transition(&mut host, 0x0c)
            .expect("Title Map LEVEL_END must export the Jungle Rollers carry");
        assert!(
            report.event_failures.is_empty(),
            "post-completion Map LEVEL_END handlers must complete cleanly: {:?}",
            report.event_failures
        );
        assert_eq!(report.requested_lid, 0x0c);
        assert_eq!(report.next_lid_after_event, 0x0c);
        assert_eq!(report.resolved.level, LevelId::new_const(0x0c));
        assert!(!report.resolved.bonus_return);
        report.carry
    };
    assert_eq!(jungle_rollers_carry.globals[CURRENT_MAP_LEVEL_GLOBAL], 2);
    assert_eq!(jungle_rollers_carry.globals[LEVEL_COUNT_GLOBAL], 1);
    assert_eq!(jungle_rollers_carry.globals[LEVELS_UNLOCKED_GLOBAL], 2);
    assert_eq!(
        jungle_rollers_carry.draw_count, 2_677,
        "post-completion Map draw-count drift"
    );
    let jungle = LevelId::new_const(0x0c);
    let (jungle_nsd, jungle_nsf, jungle_nsf_bytes) =
        parse_local_pair(&root, jungle).expect("Jungle Rollers pair must parse");
    let jungle_runtime =
        RetailRuntime::new_from_session(GLOBAL_WORDS, jungle, jungle_rollers_carry)
            .expect("Jungle Rollers must import the authentic post-completion carry");
    assert_eq!(
        jungle_runtime.machine().random_seed(),
        0xc5f2_4260,
        "Jungle Rollers must inherit the authentic Map RNG-A phase"
    );
    assert_eq!(
        jungle_runtime.draw_count(),
        2_677,
        "Jungle Rollers must inherit the authentic Map draw count"
    );
    let (jungle_survey, mut jungle_runtime) = survey_pair_with_runtime(
        known_name(jungle),
        jungle,
        &jungle_nsd,
        &jungle_nsf,
        &jungle_nsf_bytes,
        jungle_runtime,
        LevelContextSource::SessionGlobals,
        SurveyInputProfile::JunglePhaseRobust,
        3_000,
    )
    .expect("Jungle Rollers authentic-phase route must execute");
    assert_eq!(jungle_survey.frames, 2_602);
    assert_eq!(jungle_survey.zone_transitions, 30);
    assert_eq!(jungle_survey.restarts, 0);
    assert!(jungle_survey.restart_frames.is_empty());
    assert_eq!(jungle_survey.death_camera_frames, 0);
    assert!(jungle_survey.first_below_zero.is_none());
    assert!(jungle_survey.first_terminal_fall.is_none());
    assert_eq!(
        jungle_survey.next_lid,
        Some((2_602, i32::try_from(LevelId::LEVEL_COMPLETE.get()).unwrap()))
    );
    assert_eq!(jungle_survey.faulted_objects, 0);
    assert_eq!(jungle_survey.execution_errors, 0);
    assert!(
        jungle_survey.is_clean(),
        "Jungle Rollers completion route reached a checked runtime boundary: {}",
        jungle_survey.summary()
    );
    assert_eq!(
        jungle_survey.checkpoint_samples,
        [
            (1, -1, [1_945_600, 4_135_168, 24_165_632]),
            (1_117, 46 << 8, [-563_968, 2_236_928, 15_717_376]),
        ]
    );
    assert_eq!(
        jungle_survey.box_count_samples,
        [
            (1, 0),
            (533, 0x100),
            (534, 0x200),
            (1_117, 0x300),
            (1_148, 0x400),
            (1_149, 0x500),
            (1_152, 0x600),
            (1_153, 0x700),
            (1_154, 0x800),
            (1_162, 0x900),
            (1_168, 0xa00),
            (1_558, 0xb00),
            (1_559, 0xc00),
            (1_560, 0xd00),
        ]
    );
    assert_eq!(jungle_survey.saved_box_count_samples, [(1_117, 0x200)]);
    assert_eq!(
        jungle_survey.effect_counts.get("save-state").copied(),
        Some(1)
    );
    assert_eq!(
        jungle_survey.effect_counts.get("transition").copied(),
        Some(1)
    );
    for expected in [(531, 16, 3), (654, 25, 3), (1_117, 46, 9)] {
        assert!(jungle_survey.spawn_flag_samples.contains(&expected));
    }
    assert_eq!(
        jungle_runtime.global_word(CHECKPOINT_ID_GLOBAL),
        Ok(46 << 8)
    );
    assert_eq!(
        CHECKPOINT_TRANSLATION_GLOBALS.map(|index| {
            jungle_runtime
                .global_word(index)
                .expect("checkpoint translation global is readable")
                .cast_signed()
        }),
        [-563_968, 2_236_928, 15_717_376]
    );
    assert_eq!(jungle_runtime.global_word(BOX_COUNT_GLOBAL), Ok(0xd00));
    let jungle_final_camera = jungle_survey
        .final_camera
        .expect("Jungle Rollers route retains a camera location");
    assert_eq!(
        jungle_final_camera.path,
        RetailPathId {
            zone: Eid::from_name("0O_cZ").expect("fixed Jungle end-warp route EID is valid"),
            index: 0,
        }
    );
    assert_eq!(jungle_final_camera.progress.raw(), 17_836);
    assert_eq!(
        jungle_survey.final_player_translation,
        Some([2_193_152, 7_732_265, -2_147_072])
    );
    assert_eq!(jungle_runtime.machine().random_seed(), 0x10f8_41ad);
    assert_eq!(jungle_runtime.draw_count(), 5_279);

    let jungle_completion_carry: RetailSessionCarry = {
        let mut host = NsfProgramHost::new(&jungle_nsd, &jungle_nsf, &jungle_nsf_bytes);
        let report = jungle_runtime
            .finish_level_transition(
                &mut host,
                i32::try_from(LevelId::LEVEL_COMPLETE.get()).unwrap(),
            )
            .expect("Jungle Rollers LEVEL_END must export a session carry");
        assert!(
            report.event_failures.is_empty(),
            "Jungle Rollers LEVEL_END handlers must complete cleanly: {:?}",
            report.event_failures
        );
        assert_eq!(
            report.requested_lid,
            i32::try_from(LevelId::LEVEL_COMPLETE.get()).unwrap()
        );
        assert_eq!(report.next_lid_after_event, report.requested_lid);
        assert_eq!(report.resolved.level, LevelId::LEVEL_COMPLETE);
        assert!(!report.resolved.bonus_return);
        assert!(report.effects.is_empty());
        report.carry
    };
    assert_eq!(
        [
            GAME_STATE_GLOBAL,
            TITLE_STATE_GLOBAL,
            SAVED_TITLE_STATE_GLOBAL,
            CURRENT_MAP_LEVEL_GLOBAL,
            LEVEL_COUNT_GLOBAL,
            LEVELS_UNLOCKED_GLOBAL,
            ISLAND_CAMERA_STATE_GLOBAL,
        ]
        .map(|index| jungle_completion_carry.globals[index]),
        [
            0x500,
            TitleScreen::Map.raw(),
            TitleScreen::Map.raw(),
            2,
            1,
            3,
            0,
        ]
    );
    assert_eq!(jungle_completion_carry.random_seed, 0x10f8_41ad);
    assert_eq!(jungle_completion_carry.draw_count, 5_279);
    let jungle_completion_runtime = RetailRuntime::new_from_session(
        GLOBAL_WORDS,
        LevelId::LEVEL_COMPLETE,
        jungle_completion_carry,
    )
    .expect("Level Complete must import Jungle Rollers' session carry");
    let (jungle_completion_survey, mut jungle_completion_runtime) = survey_pair_with_runtime(
        known_name(LevelId::LEVEL_COMPLETE),
        LevelId::LEVEL_COMPLETE,
        &completion_nsd,
        &completion_nsf,
        &completion_nsf_bytes,
        jungle_completion_runtime,
        LevelContextSource::SessionGlobals,
        SurveyInputProfile::DirectionAndButtonSweepToTransition,
        COMPLETION_FRAMES,
    )
    .expect("Jungle Rollers' Level Complete runtime must execute");
    assert_eq!(jungle_completion_survey.frames, 393);
    assert_eq!(jungle_completion_survey.zone_transitions, 0);
    assert_eq!(jungle_completion_survey.restarts, 0);
    assert!(jungle_completion_survey.restart_frames.is_empty());
    assert_eq!(jungle_completion_survey.death_camera_frames, 0);
    assert!(jungle_completion_survey.first_below_zero.is_none());
    assert!(jungle_completion_survey.first_terminal_fall.is_none());
    assert_eq!(
        jungle_completion_survey.next_lid,
        Some((393, i32::try_from(LevelId::TITLE.get()).unwrap()))
    );
    assert_eq!(jungle_completion_survey.faulted_objects, 0);
    assert_eq!(jungle_completion_survey.execution_errors, 0);
    assert_eq!(
        jungle_completion_survey
            .effect_counts
            .get("transition")
            .copied(),
        Some(1)
    );
    assert!(
        jungle_completion_survey.is_clean(),
        "Jungle Rollers' Level Complete screen reached a checked boundary: {}",
        jungle_completion_survey.summary()
    );
    assert_eq!(
        [
            GAME_STATE_GLOBAL,
            TITLE_STATE_GLOBAL,
            SAVED_TITLE_STATE_GLOBAL,
            CURRENT_MAP_LEVEL_GLOBAL,
            LEVEL_COUNT_GLOBAL,
            LEVELS_UNLOCKED_GLOBAL,
            ISLAND_CAMERA_STATE_GLOBAL,
        ]
        .map(|index| jungle_completion_runtime.global_word(index).unwrap()),
        [
            0x300,
            TitleScreen::Map.raw(),
            TitleScreen::Map.raw(),
            2,
            1,
            3,
            0,
        ]
    );
    assert_eq!(
        jungle_completion_runtime.machine().random_seed(),
        0xbeee_4520
    );
    assert_eq!(jungle_completion_runtime.draw_count(), 5_672);

    let post_jungle_title_carry: RetailSessionCarry = {
        let mut host = NsfProgramHost::new(&completion_nsd, &completion_nsf, &completion_nsf_bytes);
        let report = jungle_completion_runtime
            .finish_level_transition(&mut host, i32::try_from(LevelId::TITLE.get()).unwrap())
            .expect("second Level Complete LEVEL_END must export a Title carry");
        assert!(
            report.event_failures.is_empty(),
            "second Level Complete LEVEL_END handlers must complete cleanly: {:?}",
            report.event_failures
        );
        assert_eq!(
            report.requested_lid,
            i32::try_from(LevelId::TITLE.get()).unwrap()
        );
        assert_eq!(report.next_lid_after_event, report.requested_lid);
        assert_eq!(report.resolved.level, LevelId::TITLE);
        assert!(!report.resolved.bonus_return);
        assert!(report.effects.is_empty());
        report.carry
    };
    assert_eq!(
        [
            GAME_STATE_GLOBAL,
            TITLE_STATE_GLOBAL,
            SAVED_TITLE_STATE_GLOBAL,
            CURRENT_MAP_LEVEL_GLOBAL,
            LEVEL_COUNT_GLOBAL,
            LEVELS_UNLOCKED_GLOBAL,
            ISLAND_CAMERA_STATE_GLOBAL,
        ]
        .map(|index| post_jungle_title_carry.globals[index]),
        [
            0x300,
            TitleScreen::Map.raw(),
            TitleScreen::Map.raw(),
            2,
            1,
            3,
            0,
        ]
    );
    assert_eq!(post_jungle_title_carry.random_seed, 0xbeee_4520);
    assert_eq!(post_jungle_title_carry.draw_count, 5_672);
    let mut post_jungle_map = AuthoredTitleMapHarness::from_session(
        &title_nsd,
        &title_nsf,
        &title_nsf_bytes,
        post_jungle_title_carry,
    );
    assert_eq!(post_jungle_map.runtime.draw_count(), 5_672);
    assert_eq!(post_jungle_map.runtime.machine().random_seed(), 0xbeee_4520);
    post_jungle_map.wait_until_ready(64);
    assert_eq!(post_jungle_map.frame, 10);
    for _ in 0..120 {
        post_jungle_map.step(0);
    }
    post_jungle_map.tap(PAD_UP);
    for _ in 0..120 {
        post_jungle_map.step(0);
    }
    post_jungle_map.step(PAD_CROSS);
    let great_gate = LevelId::new_const(0x12);
    let great_gate_lid = i32::try_from(great_gate.get()).unwrap();
    assert_eq!(post_jungle_map.frame, 253);
    assert_eq!(post_jungle_map.transitions, [(253, great_gate_lid)]);
    assert_eq!(
        post_jungle_map.camera.location().path,
        RetailPathId {
            zone: Eid::from_name("1c_pZ").expect("fixed third map-zone EID is valid"),
            index: 0,
        }
    );
    assert_eq!(post_jungle_map.camera.location().progress.raw(), 0x0200);
    assert_eq!(
        [
            GAME_STATE_GLOBAL,
            TITLE_STATE_GLOBAL,
            SAVED_TITLE_STATE_GLOBAL,
            CURRENT_MAP_LEVEL_GLOBAL,
            LEVEL_COUNT_GLOBAL,
            LEVELS_UNLOCKED_GLOBAL,
            ISLAND_CAMERA_STATE_GLOBAL,
        ]
        .map(|index| post_jungle_map.runtime.global_word(index).unwrap()),
        [
            0,
            TitleScreen::Map.raw(),
            TitleScreen::Map.raw(),
            3,
            1,
            3,
            1,
        ]
    );
    assert_eq!(post_jungle_map.runtime.faulted_object_count(), 0);
    assert_eq!(post_jungle_map.runtime.machine().random_seed(), 0x679d_ffe4);
    assert_eq!(post_jungle_map.runtime.draw_count(), 5_925);

    let great_gate_carry: RetailSessionCarry = {
        let mut host = NsfProgramHost::new(&title_nsd, &title_nsf, &title_nsf_bytes);
        let report = post_jungle_map
            .runtime
            .finish_level_transition(&mut host, great_gate_lid)
            .expect("post-Jungle Title Map must export The Great Gate carry");
        assert!(
            report.event_failures.is_empty(),
            "post-Jungle Map LEVEL_END handlers must complete cleanly: {:?}",
            report.event_failures
        );
        assert_eq!(report.requested_lid, great_gate_lid);
        assert_eq!(report.next_lid_after_event, great_gate_lid);
        assert_eq!(report.resolved.level, great_gate);
        assert!(!report.resolved.bonus_return);
        assert!(report.effects.is_empty());
        report.carry
    };
    assert_eq!(
        [
            GAME_STATE_GLOBAL,
            TITLE_STATE_GLOBAL,
            SAVED_TITLE_STATE_GLOBAL,
            CURRENT_MAP_LEVEL_GLOBAL,
            LEVEL_COUNT_GLOBAL,
            LEVELS_UNLOCKED_GLOBAL,
            ISLAND_CAMERA_STATE_GLOBAL,
        ]
        .map(|index| great_gate_carry.globals[index]),
        [
            0,
            TitleScreen::Map.raw(),
            TitleScreen::Map.raw(),
            3,
            1,
            3,
            1,
        ]
    );
    assert_eq!(great_gate_carry.random_seed, 0x679d_ffe4);
    assert_eq!(great_gate_carry.draw_count, 5_925);
    let (great_gate_nsd, great_gate_nsf, great_gate_nsf_bytes) =
        parse_local_pair(&root, great_gate).expect("The Great Gate pair must parse");
    let great_gate_runtime =
        RetailRuntime::new_from_session(GLOBAL_WORDS, great_gate, great_gate_carry.clone())
            .expect("The Great Gate must import the second-completion map carry");
    let (great_gate_survey, mut great_gate_runtime) = survey_pair_with_runtime(
        known_name(great_gate),
        great_gate,
        &great_gate_nsd,
        &great_gate_nsf,
        &great_gate_nsf_bytes,
        great_gate_runtime,
        LevelContextSource::SessionGlobals,
        SurveyInputProfile::GreatGateTawnaBonus,
        4_000,
    )
    .expect("The Great Gate must execute through its Tawna bonus transition");
    assert_eq!(great_gate_survey.frames, 2_321);
    assert_eq!(great_gate_survey.successful_spawns, 85);
    assert_eq!(great_gate_survey.executions, 43_706);
    assert_eq!(great_gate_survey.zone_transitions, 32);
    assert_eq!(great_gate_survey.camera_ranges.len(), 28);
    assert_eq!(great_gate_survey.camera_path_changes, 35);
    assert_eq!(great_gate_survey.last_camera_path_change, 2_175);
    assert_eq!(great_gate_survey.restarts, 0);
    assert!(great_gate_survey.restart_frames.is_empty());
    assert_eq!(great_gate_survey.death_camera_frames, 0);
    assert!(great_gate_survey.first_terminal_fall.is_none());
    assert_eq!(
        great_gate_survey.terminal.as_deref(),
        Some("frame 2321 requested level transition to 0x33")
    );
    assert_eq!(great_gate_survey.next_lid, Some((2_321, 0x33)));
    assert_eq!(great_gate_survey.faulted_objects, 0);
    assert_eq!(great_gate_survey.execution_errors, 0);
    assert_eq!(
        great_gate_survey.box_count_samples,
        [
            (1, 0),
            (58, 0x100),
            (78, 0x200),
            (92, 0x300),
            (112, 0x400),
            (299, 0x500),
            (300, 0x600),
            (514, 0x700),
            (769, 0x800),
            (779, 0x900),
            (1_152, 0xa00),
            (1_502, 0xb00),
            (1_503, 0xc00),
            (1_504, 0xd00),
            (1_817, 0xe00),
            (2_222, 0xf00),
        ]
    );
    assert_eq!(
        great_gate_survey.checkpoint_samples,
        [
            (1, -1, [-563_968, 2_236_928, 15_717_376]),
            (515, -1, [15_871_744, -10_670_848, 127_744]),
            (1_152, 76 << 8, [20_991_488, -8_397_312, 127_744]),
            (1_532, 76 << 8, [15_154_944, -8_104_292, 127_744]),
            (2_263, 76 << 8, [5_426_944, -8_332_092, 127_744]),
            (2_265, 113 << 8, [5_426_944, -8_332_092, 127_744]),
        ]
    );
    assert_eq!(
        great_gate_survey.saved_box_count_samples,
        [(1_152, 0x900), (2_265, 0xf00)]
    );
    assert!(
        great_gate_survey
            .spawn_flag_samples
            .contains(&(1_054, 63, 3)),
        "the carried route must trigger the first vertical arrow crate"
    );
    assert!(
        great_gate_survey
            .spawn_flag_samples
            .contains(&(1_152, 76, 9)),
        "the carried route must break checkpoint crate 76"
    );
    assert_eq!(
        great_gate_survey.effect_counts.get("send-event").copied(),
        Some(177)
    );
    assert_eq!(
        great_gate_survey.effect_counts.get("transition").copied(),
        Some(1)
    );
    assert_eq!(
        great_gate_survey.effect_counts.get("save-state").copied(),
        Some(2)
    );
    assert!(
        great_gate_survey.is_clean(),
        "The Great Gate Tawna route must remain clean: {}",
        great_gate_survey.summary()
    );
    let great_gate_camera = great_gate_survey
        .final_camera
        .expect("The Great Gate Tawna route retains a camera location");
    assert_eq!(
        great_gate_camera.path,
        RetailPathId {
            zone: Eid::from_name("c5_iZ").expect("fixed Great Gate route EID is valid"),
            index: 0,
        }
    );
    assert_eq!(great_gate_camera.progress.raw(), 16_550);
    assert_eq!(
        great_gate_survey.final_player_translation,
        Some([5_408_512, -8_168_640, 116_480])
    );
    assert_eq!(great_gate_runtime.global_word(BOX_COUNT_GLOBAL), Ok(0xf00));
    assert_eq!(
        great_gate_runtime.global_word(CHECKPOINT_ID_GLOBAL),
        Ok(113 << 8)
    );
    assert_eq!(
        CHECKPOINT_TRANSLATION_GLOBALS.map(|index| {
            great_gate_runtime
                .global_word(index)
                .expect("checkpoint translation global is readable")
                .cast_signed()
        }),
        [5_426_944, -8_332_092, 127_744]
    );

    let waloc = Eid::from_name("WalOC").expect("fixed rotating-log EID is valid");
    assert!(
        great_gate_survey
            .observed_program_states
            .contains(&(waloc, 2)),
        "the route must flip the rotating log into its horizontal state before climbing"
    );
    for token in [(514, 27, 7), (1_504, 89, 7), (2_263, 113, 10)] {
        assert!(
            great_gate_survey.spawn_flag_samples.contains(&token),
            "all three authored Tawna crates must break on the carried route: {token:?}"
        );
    }
    assert_eq!(great_gate_runtime.global_word(60), Ok(4));
    assert_eq!(great_gate_runtime.machine().random_seed(), 0x4ca9_620f);
    assert_eq!(great_gate_runtime.draw_count(), 8_246);
    assert_eq!(
        player_trace(&great_gate_runtime)
            .unwrap()
            .expect("Crash remains live through the bonus transition")
            .tawna_counter,
        0,
        "DispC clears the three-token counter after selecting Bonus 2"
    );
    let expected_parent_snapshot = great_gate_runtime
        .saved_level_state()
        .cloned()
        .expect("the third Tawna pickup must save the complete Great Gate return state");
    assert_eq!(expected_parent_snapshot.level, great_gate);
    assert_eq!(expected_parent_snapshot.box_count, 0xf00);
    assert_eq!(
        expected_parent_snapshot.player_translation,
        [5_426_944, -8_332_092, 127_744]
    );
    assert_eq!(expected_parent_snapshot.location.progress.raw(), 293);

    let bonus = LevelId::new_const(0x33);
    let bonus_transition = {
        let mut host = NsfProgramHost::new(&great_gate_nsd, &great_gate_nsf, &great_gate_nsf_bytes);
        great_gate_runtime
            .finish_level_transition(
                &mut host,
                i32::try_from(bonus.get()).expect("bonus LID fits i32"),
            )
            .expect("The Great Gate LEVEL_END phase must preserve Bonus 2")
    };
    assert!(bonus_transition.event_failures.is_empty());
    assert_eq!(bonus_transition.resolved.level, bonus);
    assert!(!bonus_transition.resolved.bonus_return);
    assert_eq!(
        bonus_transition.carry.saved_level_state.as_ref(),
        Some(&expected_parent_snapshot)
    );

    let (bonus_nsd, bonus_nsf, bonus_nsf_bytes) =
        parse_local_pair(&root, bonus).expect("Bonus 2 pair must parse");
    let bonus_graph = graph_for_pair(bonus, &bonus_nsd, &bonus_nsf, &bonus_nsf_bytes)
        .expect("Bonus 2 zone graph must parse");
    let (bonus_zones, _) = zone_catalog(
        &bonus_nsd,
        &bonus_nsf,
        &bonus_nsf_bytes,
        &bonus_graph,
        bonus,
    )
    .expect("Bonus 2 zones must parse");
    let bonus_spawn = bonus_graph
        .zone(bonus_graph.spawn_path().zone)
        .expect("Bonus 2 spawn zone must be present");
    assert_eq!(bonus_spawn.graphics_flags, 0x2002);
    let portal_zone = Eid::from_name("2__PZ").expect("fixed Bonus 2 portal zone EID is valid");
    let portal_entity = bonus_zones[&portal_zone]
        .entities
        .iter()
        .find(|entity| entity.id == 14)
        .expect("Bonus 2's authored return portal must be present");
    assert_eq!(
        (
            portal_entity.group,
            portal_entity.executable,
            portal_entity.subtype,
            portal_entity.spawn_flags,
        ),
        (3, 0x20, 1, 0x0008)
    );

    let bonus_runtime =
        RetailRuntime::new_from_session(GLOBAL_WORDS, bonus, bonus_transition.carry)
            .expect("Bonus 2 must import the Great Gate session carry");
    let (_, mut bonus_runtime) = survey_pair_with_runtime(
        known_name(bonus),
        bonus,
        &bonus_nsd,
        &bonus_nsf,
        &bonus_nsf_bytes,
        bonus_runtime,
        LevelContextSource::SessionGlobals,
        SurveyInputProfile::Idle,
        1,
    )
    .expect("Bonus 2 must mount with the carried Great Gate snapshot");
    assert_eq!(
        bonus_runtime.saved_level_state(),
        Some(&expected_parent_snapshot),
        "the save-restricted bonus spawn must not replace its parent return"
    );

    let player = bonus_runtime
        .arena()
        .main_object()
        .and_then(|arena| bonus_runtime.object_for_arena(arena))
        .expect("mounted Bonus 2 must have Crash");
    let mut bonus_host = NsfProgramHost::new(&bonus_nsd, &bonus_nsf, &bonus_nsf_bytes);
    let portal_entities = [portal_entity.clone()];
    let portal_display_flags = bonus_graph
        .zone(portal_zone)
        .expect("Bonus 2 portal zone must be in its graph")
        .display_flags
        | 2;
    let portal_attempts = bonus_runtime.spawn_current_zone_neighbors(
        &[NeighborZone {
            eid: portal_zone,
            display_flags: portal_display_flags,
            entities: &portal_entities,
        }],
        &mut bonus_host,
    );
    assert_eq!(portal_attempts.len(), 1);
    let warp = *portal_attempts[0]
        .result
        .as_ref()
        .expect("Bonus 2's authored WarpC portal must materialize");
    assert_eq!(
        bonus_runtime
            .machine()
            .object(warp.vm())
            .ok()
            .and_then(crust_sim::gool::VmObject::program_identity)
            .map(GoolProgramIdentity::global_eid),
        Some(Eid::from_name("WarpC").expect("fixed retail portal EID is valid"))
    );
    let dispatch = bonus_runtime
        .dispatch_event(
            &mut bonus_host,
            Some(warp),
            Some(player),
            22 << 8,
            Some(&[0]),
        )
        .expect("WarpC must synchronously select WillC's authored WARP state");
    assert_eq!(
        dispatch.state_change.as_ref().map(|change| change.state),
        Some(32)
    );

    let mut load_state = None;
    for frame in 1..=5_400_u32 {
        bonus_runtime.set_frame_timing(34, 34);
        let pad = if frame == 300 {
            RetailPadSnapshot {
                tapped: PAD_CROSS,
                held: PAD_CROSS,
                ..RetailPadSnapshot::default()
            }
        } else {
            RetailPadSnapshot::default()
        };
        bonus_runtime
            .set_pad_snapshot(0, pad)
            .expect("Bonus 2 must accept the return prompt input");
        let report = bonus_runtime
            .run_frame(&mut bonus_host, INSTRUCTION_BUDGET)
            .unwrap_or_else(|error| panic!("Bonus 2 WARP frame {frame} failed: {error:?}"));
        assert!(
            report
                .executions
                .iter()
                .all(|execution| execution.result.is_ok()),
            "Bonus 2 WARP frame {frame} faulted: {:?}",
            report.executions
        );
        let load_states = report
            .effects
            .iter()
            .filter_map(|effect| match effect {
                VmEffect::LoadState { saved_level, .. } => Some(*saved_level),
                _ => None,
            })
            .collect::<Vec<_>>();
        if !load_states.is_empty() {
            assert_eq!(load_states.len(), 1);
            load_state = Some((
                frame,
                load_states[0].expect("Bonus 2 must resolve its parent save level"),
            ));
            break;
        }
    }
    let (load_frame, captured_saved_level) =
        load_state.expect("Bonus 2 WillC WARP must reach LoadState");
    assert_eq!(load_frame, 301);
    assert_eq!(captured_saved_level, great_gate);
    assert_eq!(
        bonus_runtime.restart_saved_level_from_effect(&mut bonus_host, captured_saved_level),
        Ok(RetailRestartOutcome::DifferentLevel {
            saved_level: great_gate,
            requested_level_sentinel: -2,
        })
    );
    let return_transition = bonus_runtime
        .finish_level_transition(&mut bonus_host, -2)
        .expect("Bonus 2 LEVEL_END must resolve the carried Great Gate snapshot");
    assert!(return_transition.event_failures.is_empty());
    assert_eq!(return_transition.next_lid_after_event, -2);
    assert_eq!(return_transition.resolved.level, great_gate);
    assert!(return_transition.resolved.bonus_return);
    assert_eq!(
        return_transition.carry.saved_level_state.as_ref(),
        Some(&expected_parent_snapshot)
    );
    assert_eq!(
        return_transition.carry.globals[CHECKPOINT_ID_GLOBAL],
        113 << 8
    );

    let parent_graph = graph_for_pair(
        great_gate,
        &great_gate_nsd,
        &great_gate_nsf,
        &great_gate_nsf_bytes,
    )
    .expect("the returned Great Gate camera graph must parse");
    let (parent_zones, parent_lifecycle) = zone_catalog(
        &great_gate_nsd,
        &great_gate_nsf,
        &great_gate_nsf_bytes,
        &parent_graph,
        great_gate,
    )
    .expect("the returned Great Gate zone catalog must parse");
    let game_state =
        return_transition.carry.globals[crust_sim::gool::GAME_STATE_GLOBAL].cast_signed();
    let parent_camera = RetailCameraRuntime::at_path(
        &parent_graph,
        expected_parent_snapshot.location.path,
        expected_parent_snapshot.location.progress.raw(),
        game_state,
    )
    .expect("the returned Great Gate camera must accept the saved location");
    assert_eq!(parent_camera.location(), expected_parent_snapshot.location);

    let mut returned_runtime =
        RetailRuntime::new_from_session(GLOBAL_WORDS, great_gate, return_transition.carry)
            .expect("Great Gate must import the Bonus 2 return carry");
    seed_mounted_level_context_from_globals(
        &mut returned_runtime,
        &parent_graph,
        &parent_lifecycle,
        parent_camera.location(),
    )
    .expect("returned Great Gate must publish its saved camera context");
    let mut parent_host =
        NsfProgramHost::new(&great_gate_nsd, &great_gate_nsf, &great_gate_nsf_bytes);
    returned_runtime
        .create_retail_core_objects(parent_camera.location().path.zone, &mut parent_host)
        .expect("returned Great Gate core objects must materialize");
    returned_runtime
        .create_retail_level_misc_object(parent_camera.location().path.zone, &mut parent_host)
        .expect("returned Great Gate level-misc object must materialize");
    let returned_neighbors = parent_lifecycle
        .next_frame_spawn_scan()
        .iter()
        .map(|candidate| {
            let zone = parent_zones
                .get(&candidate.zone)
                .expect("returned lifecycle zone exists in the catalog");
            NeighborZone {
                eid: zone.eid,
                display_flags: candidate.display_flags,
                entities: zone.entities.as_slice(),
            }
        })
        .collect::<Vec<_>>();
    returned_runtime.set_initial_crash_save_suppressed(true);
    let protected_spawn =
        returned_runtime.spawn_current_zone_neighbors(&returned_neighbors, &mut parent_host);
    returned_runtime.set_initial_crash_save_suppressed(false);
    assert!(
        protected_spawn
            .iter()
            .all(|attempt| attempt.result.is_ok() || expected_spawn_rejection(&attempt.result))
    );
    assert_eq!(
        returned_runtime.saved_level_state(),
        Some(&expected_parent_snapshot)
    );

    let RetailRestartOutcome::Restarted(restart) = returned_runtime
        .restart_saved_level(&mut parent_host)
        .expect("the protected Great Gate restart must complete")
    else {
        panic!("the returned Great Gate snapshot requested another remount");
    };
    assert_eq!(restart.snapshot, expected_parent_snapshot);
    assert!(restart.respawn_event_failures.is_empty());
    assert!(
        restart
            .zone_reports
            .iter()
            .all(|(_, report)| report.event_failures.is_empty())
    );
    assert_eq!(restart.restored_box_count, 0xe00);
    assert_eq!(returned_runtime.global_word(BOX_COUNT_GLOBAL), Ok(0xe00));
    assert_eq!(
        returned_runtime
            .level_state_context()
            .expect("restarted Great Gate retains camera context")
            .location,
        expected_parent_snapshot.location
    );
    let mut expected_spawn_words = expected_parent_snapshot.spawn_words.map(|word| word & !1);
    expected_spawn_words[113] = (expected_spawn_words[113] & !2) | 8;
    assert_eq!(
        returned_runtime.arena().spawn_table().snapshot(),
        expected_spawn_words,
        "first-spawn restoration preserves the carried table, marks checkpoint 113 seen, and clears transient active/blocked bits"
    );
    let returned_player = returned_runtime
        .arena()
        .main_object()
        .and_then(|arena| returned_runtime.object_for_arena(arena))
        .and_then(|object| returned_runtime.machine().object(object.vm()).ok())
        .expect("restarted Great Gate must retain Crash");
    assert_eq!(
        [
            process_register::TRANSLATION_X,
            process_register::TRANSLATION_Y,
            process_register::TRANSLATION_Z,
        ]
        .map(|register| returned_player.register(register).unwrap().cast_signed()),
        expected_parent_snapshot.player_translation
    );

    // Fork the same authentic post-Jungle carry through the ordinary end warp
    // and into Boulders. The bonus branch above consumes only its cloned carry.
    {
        let great_gate_runtime =
            RetailRuntime::new_from_session(GLOBAL_WORDS, great_gate, great_gate_carry)
                .expect("The Great Gate must import the second-completion map carry");
        let (great_gate_survey, mut great_gate_runtime) = survey_pair_with_runtime(
            known_name(great_gate),
            great_gate,
            &great_gate_nsd,
            &great_gate_nsf,
            &great_gate_nsf_bytes,
            great_gate_runtime,
            LevelContextSource::SessionGlobals,
            SurveyInputProfile::GreatGatePhaseRobust,
            2_600,
        )
        .expect("The Great Gate must execute through its end WarpC transition");
        assert_eq!(great_gate_survey.frames, 2_471);
        assert_eq!(great_gate_survey.successful_spawns, 111);
        assert_eq!(great_gate_survey.executions, 47_371);
        assert_eq!(great_gate_survey.zone_transitions, 38);
        assert_eq!(great_gate_survey.camera_ranges.len(), 30);
        assert_eq!(great_gate_survey.camera_path_changes, 41);
        assert_eq!(great_gate_survey.last_camera_path_change, 2_377);
        assert_eq!(great_gate_survey.restarts, 0);
        assert!(great_gate_survey.restart_frames.is_empty());
        assert_eq!(great_gate_survey.death_camera_frames, 0);
        assert!(great_gate_survey.first_terminal_fall.is_none());
        assert_eq!(
            great_gate_survey.terminal.as_deref(),
            Some("frame 2471 requested level transition to 0x2d")
        );
        assert_eq!(
            great_gate_survey.next_lid,
            Some((2_471, i32::try_from(LevelId::LEVEL_COMPLETE.get()).unwrap()))
        );
        assert_eq!(great_gate_survey.faulted_objects, 0);
        assert_eq!(great_gate_survey.execution_errors, 0);
        assert_eq!(
            great_gate_survey.box_count_samples,
            [
                (1, 0),
                (58, 0x100),
                (78, 0x200),
                (92, 0x300),
                (112, 0x400),
                (299, 0x500),
                (300, 0x600),
                (514, 0x700),
                (769, 0x800),
                (779, 0x900),
                (1_152, 0xa00),
                (1_502, 0xb00),
                (1_503, 0xc00),
                (1_504, 0xd00),
                (1_817, 0xe00),
            ]
        );
        assert_eq!(
            great_gate_survey.checkpoint_samples,
            [
                (1, -1, [-563_968, 2_236_928, 15_717_376]),
                (515, -1, [15_871_744, -10_670_848, 127_744]),
                (1_152, 76 << 8, [20_991_488, -8_397_312, 127_744]),
                (1_532, 76 << 8, [15_154_944, -8_104_292, 127_744]),
            ]
        );
        assert_eq!(great_gate_survey.saved_box_count_samples, [(1_152, 0x900)]);
        assert!(
            great_gate_survey
                .spawn_flag_samples
                .contains(&(1_054, 63, 3)),
            "the carried route must trigger the first vertical arrow crate"
        );
        assert!(
            great_gate_survey
                .spawn_flag_samples
                .contains(&(1_152, 76, 9)),
            "the carried route must break checkpoint crate 76"
        );
        assert_eq!(
            great_gate_survey.effect_counts.get("send-event").copied(),
            Some(206)
        );
        assert_eq!(
            great_gate_survey.effect_counts.get("transition").copied(),
            Some(1)
        );
        assert_eq!(
            great_gate_survey.effect_counts.get("save-state").copied(),
            Some(1)
        );
        assert!(
            great_gate_survey.is_clean(),
            "The Great Gate end-Warp route must remain clean: {}",
            great_gate_survey.summary()
        );
        let great_gate_camera = great_gate_survey
            .final_camera
            .expect("The Great Gate end-Warp route retains a camera location");
        assert_eq!(
            great_gate_camera.path,
            RetailPathId {
                zone: Eid::from_name("c7_iZ").expect("fixed Great Gate route EID is valid"),
                index: 0,
            }
        );
        assert_eq!(great_gate_camera.progress.raw(), 2_528);
        assert_eq!(
            great_gate_survey.final_player_translation,
            Some([3_593_984, -4_780_682, 83_712])
        );
        assert_eq!(great_gate_runtime.global_word(BOX_COUNT_GLOBAL), Ok(0xe00));
        assert_eq!(
            great_gate_runtime.global_word(CHECKPOINT_ID_GLOBAL),
            Ok(76 << 8)
        );
        assert_eq!(
            CHECKPOINT_TRANSLATION_GLOBALS.map(|index| {
                great_gate_runtime
                    .global_word(index)
                    .expect("checkpoint translation global is readable")
                    .cast_signed()
            }),
            [15_154_944, -8_104_292, 127_744]
        );

        let waloc = Eid::from_name("WalOC").expect("fixed rotating-log EID is valid");
        assert!(
            great_gate_survey
                .observed_program_states
                .contains(&(waloc, 2)),
            "the route must flip the rotating log into its horizontal state before climbing"
        );
        let warp = Eid::from_name("WarpC").expect("fixed warp EID is valid");
        let crash = Eid::from_name("WillC").expect("fixed player EID is valid");
        assert!(
            great_gate_survey
                .observed_program_states
                .contains(&(warp, 1)),
            "the route must activate the normal end WarpC"
        );
        assert!(
            great_gate_survey
                .observed_program_states
                .contains(&(crash, 32)),
            "WarpC must hand Crash to authored warp state 32"
        );
        assert_eq!(
            [
                GAME_STATE_GLOBAL,
                TITLE_STATE_GLOBAL,
                SAVED_TITLE_STATE_GLOBAL,
                CURRENT_MAP_LEVEL_GLOBAL,
                LEVEL_COUNT_GLOBAL,
                LEVELS_UNLOCKED_GLOBAL,
                ISLAND_CAMERA_STATE_GLOBAL,
            ]
            .map(|index| great_gate_runtime.global_word(index).unwrap()),
            [
                0x500,
                TitleScreen::Map.raw(),
                TitleScreen::Map.raw(),
                3,
                1,
                4,
                0,
            ]
        );
        assert_eq!(great_gate_runtime.machine().random_seed(), 0x6a21_9f2c);
        assert_eq!(great_gate_runtime.draw_count(), 8_396);

        let great_gate_completion_carry: RetailSessionCarry = {
            let mut host =
                NsfProgramHost::new(&great_gate_nsd, &great_gate_nsf, &great_gate_nsf_bytes);
            let report = great_gate_runtime
                .finish_level_transition(
                    &mut host,
                    i32::try_from(LevelId::LEVEL_COMPLETE.get()).unwrap(),
                )
                .expect("The Great Gate LEVEL_END must export a Level Complete carry");
            assert!(
                report.event_failures.is_empty(),
                "The Great Gate LEVEL_END handlers must complete cleanly: {:?}",
                report.event_failures
            );
            assert_eq!(report.resolved.level, LevelId::LEVEL_COMPLETE);
            assert!(!report.resolved.bonus_return);
            assert!(report.effects.is_empty());
            report.carry
        };
        assert_eq!(
            [
                GAME_STATE_GLOBAL,
                TITLE_STATE_GLOBAL,
                SAVED_TITLE_STATE_GLOBAL,
                CURRENT_MAP_LEVEL_GLOBAL,
                LEVEL_COUNT_GLOBAL,
                LEVELS_UNLOCKED_GLOBAL,
                ISLAND_CAMERA_STATE_GLOBAL,
            ]
            .map(|index| great_gate_completion_carry.globals[index]),
            [
                0x500,
                TitleScreen::Map.raw(),
                TitleScreen::Map.raw(),
                3,
                1,
                4,
                0,
            ]
        );
        assert_eq!(great_gate_completion_carry.random_seed, 0x6a21_9f2c);
        assert_eq!(great_gate_completion_carry.draw_count, 8_396);
        let great_gate_completion_runtime = RetailRuntime::new_from_session(
            GLOBAL_WORDS,
            LevelId::LEVEL_COMPLETE,
            great_gate_completion_carry,
        )
        .expect("Level Complete must import The Great Gate's session carry");
        let (great_gate_completion_survey, mut great_gate_completion_runtime) =
            survey_pair_with_runtime(
                known_name(LevelId::LEVEL_COMPLETE),
                LevelId::LEVEL_COMPLETE,
                &completion_nsd,
                &completion_nsf,
                &completion_nsf_bytes,
                great_gate_completion_runtime,
                LevelContextSource::SessionGlobals,
                SurveyInputProfile::DirectionAndButtonSweepToTransition,
                COMPLETION_FRAMES,
            )
            .expect("The Great Gate's Level Complete runtime must execute");
        assert_eq!(great_gate_completion_survey.frames, 225);
        assert_eq!(great_gate_completion_survey.zone_transitions, 0);
        assert_eq!(great_gate_completion_survey.restarts, 0);
        assert!(great_gate_completion_survey.restart_frames.is_empty());
        assert_eq!(great_gate_completion_survey.death_camera_frames, 0);
        assert!(great_gate_completion_survey.first_below_zero.is_none());
        assert!(great_gate_completion_survey.first_terminal_fall.is_none());
        assert_eq!(
            great_gate_completion_survey.next_lid,
            Some((225, i32::try_from(LevelId::TITLE.get()).unwrap()))
        );
        assert_eq!(great_gate_completion_survey.faulted_objects, 0);
        assert_eq!(great_gate_completion_survey.execution_errors, 0);
        assert_eq!(
            great_gate_completion_survey
                .effect_counts
                .get("transition")
                .copied(),
            Some(1)
        );
        assert!(
            great_gate_completion_survey.is_clean(),
            "The Great Gate's Level Complete screen reached a checked boundary: {}",
            great_gate_completion_survey.summary()
        );
        assert_eq!(
            [
                GAME_STATE_GLOBAL,
                TITLE_STATE_GLOBAL,
                SAVED_TITLE_STATE_GLOBAL,
                CURRENT_MAP_LEVEL_GLOBAL,
                LEVEL_COUNT_GLOBAL,
                LEVELS_UNLOCKED_GLOBAL,
                ISLAND_CAMERA_STATE_GLOBAL,
            ]
            .map(|index| great_gate_completion_runtime.global_word(index).unwrap()),
            [
                0x300,
                TitleScreen::Map.raw(),
                TitleScreen::Map.raw(),
                3,
                1,
                4,
                0,
            ]
        );
        assert_eq!(
            great_gate_completion_runtime.machine().random_seed(),
            0x2875_d290
        );
        assert_eq!(great_gate_completion_runtime.draw_count(), 8_621);
        let post_great_gate_title_carry: RetailSessionCarry = {
            let mut host =
                NsfProgramHost::new(&completion_nsd, &completion_nsf, &completion_nsf_bytes);
            let report = great_gate_completion_runtime
                .finish_level_transition(&mut host, i32::try_from(LevelId::TITLE.get()).unwrap())
                .expect("third Level Complete LEVEL_END must export a Title carry");
            assert!(
                report.event_failures.is_empty(),
                "third Level Complete LEVEL_END handlers must complete cleanly: {:?}",
                report.event_failures
            );
            assert_eq!(
                report.requested_lid,
                i32::try_from(LevelId::TITLE.get()).unwrap()
            );
            assert_eq!(report.next_lid_after_event, report.requested_lid);
            assert_eq!(report.resolved.level, LevelId::TITLE);
            assert!(!report.resolved.bonus_return);
            assert!(report.effects.is_empty());
            report.carry
        };
        assert_eq!(post_great_gate_title_carry.random_seed, 0x2875_d290);
        assert_eq!(post_great_gate_title_carry.draw_count, 8_621);
        let mut post_great_gate_map = AuthoredTitleMapHarness::from_session(
            &title_nsd,
            &title_nsf,
            &title_nsf_bytes,
            post_great_gate_title_carry,
        );
        post_great_gate_map.wait_until_ready(64);
        assert_eq!(post_great_gate_map.frame, 10);
        for _ in 0..120 {
            post_great_gate_map.step(0);
        }
        post_great_gate_map.tap(PAD_UP);
        for _ in 0..120 {
            post_great_gate_map.step(0);
        }
        post_great_gate_map.step(PAD_CROSS);
        let boulders = LevelId::new_const(0x0e);
        let boulders_lid = i32::try_from(boulders.get()).unwrap();
        assert_eq!(post_great_gate_map.frame, 253);
        assert_eq!(post_great_gate_map.transitions, [(253, boulders_lid)]);
        assert_eq!(
            post_great_gate_map.camera.location().path,
            RetailPathId {
                zone: Eid::from_name("1c_pZ").expect("fixed fourth map-zone EID is valid"),
                index: 0,
            }
        );
        assert_eq!(post_great_gate_map.camera.location().progress.raw(), 0x0f00);
        assert_eq!(
            [
                GAME_STATE_GLOBAL,
                TITLE_STATE_GLOBAL,
                SAVED_TITLE_STATE_GLOBAL,
                CURRENT_MAP_LEVEL_GLOBAL,
                LEVEL_COUNT_GLOBAL,
                LEVELS_UNLOCKED_GLOBAL,
                ISLAND_CAMERA_STATE_GLOBAL,
            ]
            .map(|index| post_great_gate_map.runtime.global_word(index).unwrap()),
            [
                0,
                TitleScreen::Map.raw(),
                TitleScreen::Map.raw(),
                4,
                1,
                4,
                1,
            ]
        );
        assert_eq!(
            post_great_gate_map.runtime.machine().random_seed(),
            0x4196_95fd
        );
        assert_eq!(post_great_gate_map.runtime.draw_count(), 8_874);
        let boulders_carry: RetailSessionCarry = {
            let mut host = NsfProgramHost::new(&title_nsd, &title_nsf, &title_nsf_bytes);
            let report = post_great_gate_map
                .runtime
                .finish_level_transition(&mut host, boulders_lid)
                .expect("post-Great-Gate Map must export the Boulders carry");
            assert!(
                report.event_failures.is_empty(),
                "post-Great-Gate Map LEVEL_END handlers must complete cleanly: {:?}",
                report.event_failures
            );
            assert_eq!(report.requested_lid, boulders_lid);
            assert_eq!(report.next_lid_after_event, boulders_lid);
            assert_eq!(report.resolved.level, boulders);
            assert!(!report.resolved.bonus_return);
            assert!(report.effects.is_empty());
            report.carry
        };
        assert_eq!(
            [
                GAME_STATE_GLOBAL,
                TITLE_STATE_GLOBAL,
                SAVED_TITLE_STATE_GLOBAL,
                CURRENT_MAP_LEVEL_GLOBAL,
                LEVEL_COUNT_GLOBAL,
                LEVELS_UNLOCKED_GLOBAL,
                ISLAND_CAMERA_STATE_GLOBAL,
            ]
            .map(|index| boulders_carry.globals[index]),
            [
                0,
                TitleScreen::Map.raw(),
                TitleScreen::Map.raw(),
                4,
                1,
                4,
                1,
            ]
        );
        assert_eq!(boulders_carry.random_seed, 0x4196_95fd);
        assert_eq!(boulders_carry.draw_count, 8_874);
        let (boulders_nsd, boulders_nsf, boulders_nsf_bytes) =
            parse_local_pair(&root, boulders).expect("Boulders pair must parse");
        let boulders_pbak_entry = boulders_nsf
            .entries()
            .find(|entry| entry.entry_type == PBAK_ENTRY_TYPE)
            .expect("legally local Boulders pair must contain its authored PBAK");
        assert_eq!(
            boulders_pbak_entry.eid,
            Eid::from_name("pb0eB").expect("fixed Boulders PBAK EID is valid")
        );
        let boulders_pbak = load_pbak_entry(boulders_pbak_entry, &boulders_nsf_bytes)
            .expect("legally local Boulders PBAK must parse");
        assert_eq!(boulders_pbak.frames.len(), 990);
        assert_eq!(boulders_pbak.ticks_per_frame, 34);
        let boulders_runtime =
            RetailRuntime::new_from_session(GLOBAL_WORDS, boulders, boulders_carry.clone())
                .expect("Boulders must import the third-completion map carry");
        let (boulders_survey, boulders_runtime) = survey_pair_with_runtime(
            known_name(boulders),
            boulders,
            &boulders_nsd,
            &boulders_nsf,
            &boulders_nsf_bytes,
            boulders_runtime,
            LevelContextSource::SessionGlobals,
            SurveyInputProfile::LocalPbakPrefix,
            990,
        )
        .expect("Boulders must execute the legally local authored pad prefix");
        assert_eq!(boulders_survey.frames, 990);
        assert!(boulders_survey.terminal.is_none());
        assert_eq!(boulders_survey.successful_spawns, 37);
        assert_eq!(boulders_survey.unexpected_spawn_errors, 0);
        assert_eq!(boulders_survey.executions, 20_692);
        assert_eq!(boulders_survey.zone_transitions, 10);
        assert_eq!(boulders_survey.camera_ranges.len(), 16);
        assert_eq!(boulders_survey.camera_path_changes, 21);
        assert_eq!(boulders_survey.last_camera_path_change, 884);
        assert_eq!(boulders_survey.restarts, 0);
        assert!(boulders_survey.restart_frames.is_empty());
        assert_eq!(boulders_survey.save_handshakes, 0);
        assert_eq!(boulders_survey.death_camera_frames, 0);
        assert!(boulders_survey.first_below_zero.is_none());
        assert!(boulders_survey.first_terminal_fall.is_none());
        assert!(boulders_survey.next_lid.is_none());
        assert_eq!(boulders_survey.faulted_objects, 0);
        assert_eq!(boulders_survey.execution_errors, 0);
        assert!(!boulders_survey.effect_counts.contains_key("transition"));
        assert!(!boulders_survey.effect_counts.contains_key("save-state"));
        assert!(boulders_survey.issue_counts.is_empty());
        assert_eq!(
            boulders_survey.box_count_samples,
            [
                (1, 0),
                (71, 0x100),
                (173, 0x200),
                (174, 0x300),
                (197, 0x400),
                (232, 0x500),
                (633, 0x600),
                (636, 0x700),
                (695, 0x800),
            ]
        );
        assert_eq!(
            boulders_survey.checkpoint_samples,
            [(1, -1, [15_154_944, -8_104_292, 127_744])]
        );
        assert!(boulders_survey.saved_box_count_samples.is_empty());
        assert!(
            boulders_survey.is_clean(),
            "Boulders carried authored prefix must remain clean: {}",
            boulders_survey.summary()
        );
        let boulders_initial_camera = boulders_survey
            .initial_camera
            .expect("Boulders authored prefix starts with a camera location");
        assert_eq!(
            boulders_initial_camera.path,
            RetailPathId {
                zone: Eid::from_name("0Q_eZ").expect("fixed Boulders spawn-zone EID is valid"),
                index: 0,
            }
        );
        assert_eq!(boulders_initial_camera.progress.raw(), 0);
        let boulders_camera = boulders_survey
            .final_camera
            .expect("Boulders authored prefix retains a camera location");
        assert_eq!(
            boulders_camera.path,
            RetailPathId {
                zone: Eid::from_name("0I_eZ").expect("fixed Boulders route-zone EID is valid"),
                index: 1,
            }
        );
        assert_eq!(boulders_camera.progress.raw(), 3_840);
        assert_eq!(
            boulders_survey.final_player_translation,
            Some([2_377_472, 7_550_502, -12_157_440])
        );
        assert_eq!(boulders_runtime.machine().random_seed(), 0xb4e7_0e26);
        assert_eq!(boulders_runtime.draw_count(), 9_864);

        let completion_route_runtime =
            RetailRuntime::new_from_session(GLOBAL_WORDS, boulders, boulders_carry)
                .expect("Boulders completion route must import the map carry");
        let (completion_route_survey, mut completion_route_runtime) = survey_pair_with_runtime(
            known_name(boulders),
            boulders,
            &boulders_nsd,
            &boulders_nsf,
            &boulders_nsf_bytes,
            completion_route_runtime,
            LevelContextSource::SessionGlobals,
            SurveyInputProfile::BouldersCompletionRoute,
            2_300,
        )
        .expect("Boulders completion route must execute cleanly");
        assert_eq!(completion_route_survey.frames, 2_210);
        assert_eq!(
            completion_route_survey.terminal.as_deref(),
            Some("frame 2210 requested level transition to 0x2d")
        );
        assert_eq!(completion_route_survey.final_live_objects, 18);
        assert_eq!(completion_route_survey.max_live_objects, 43);
        assert_eq!(completion_route_survey.successful_spawns, 97);
        assert_eq!(completion_route_survey.spawn_attempts, 28_426);
        assert_eq!(completion_route_survey.expected_spawn_rejections, 28_329);
        assert_eq!(completion_route_survey.unexpected_spawn_errors, 0);
        assert_eq!(completion_route_survey.executions, 53_886);
        assert_eq!(completion_route_survey.zone_transitions, 26);
        assert_eq!(completion_route_survey.camera_ranges.len(), 48);
        assert_eq!(completion_route_survey.camera_path_changes, 53);
        assert_eq!(completion_route_survey.last_camera_path_change, 2_087);
        assert_eq!(completion_route_survey.last_camera_progress_change, 2_114);
        assert_eq!(completion_route_survey.last_player_movement, 2_206);
        assert_eq!(completion_route_survey.restarts, 0);
        assert!(completion_route_survey.restart_frames.is_empty());
        assert_eq!(completion_route_survey.save_handshakes, 0);
        assert_eq!(completion_route_survey.death_camera_frames, 0);
        assert_eq!(completion_route_survey.faulted_objects, 0);
        assert_eq!(completion_route_survey.execution_errors, 0);
        assert!(completion_route_survey.first_below_zero.is_none());
        assert!(completion_route_survey.first_terminal_fall.is_none());
        assert_eq!(completion_route_survey.next_lid, Some((2_210, 0x2d)));
        assert_eq!(
            completion_route_survey.effect_counts.get("save-state"),
            Some(&1)
        );
        assert_eq!(
            completion_route_survey
                .effect_counts
                .get("master-fade-reset"),
            Some(&1)
        );
        assert_eq!(
            completion_route_survey.effect_counts.get("transition"),
            Some(&1)
        );
        assert!(completion_route_survey.issue_counts.is_empty());
        assert_eq!(
            completion_route_survey.checkpoint_samples,
            [
                (1, -1, [15_154_944, -8_104_292, 127_744]),
                (1_277, 15_104, [2_303_232, 6_860_544, -5_172_480]),
            ]
        );
        assert_eq!(
            completion_route_survey.saved_box_count_samples,
            [(1_277, 0x0c00)]
        );
        assert_eq!(
            completion_route_survey.box_count_samples,
            [
                (1, 0),
                (71, 0x0100),
                (173, 0x0200),
                (174, 0x0300),
                (197, 0x0400),
                (232, 0x0500),
                (633, 0x0600),
                (636, 0x0700),
                (695, 0x0800),
                (1_105, 0x0900),
                (1_190, 0x0a00),
                (1_275, 0x0b00),
                (1_276, 0x0c00),
                (1_277, 0x0d00),
                (1_279, 0x0e00),
                (1_757, 0x0f00),
            ]
        );
        let completion_route_camera = completion_route_survey
            .final_camera
            .expect("Boulders completion route retains a camera location");
        assert_eq!(
            completion_route_camera.path,
            RetailPathId {
                zone: Eid::from_name("0s_eZ").expect("fixed Boulders end-zone EID is valid"),
                index: 1,
            }
        );
        assert_eq!(completion_route_camera.progress.raw(), 12_799);
        assert_eq!(
            completion_route_survey.final_player_translation,
            Some([2_391_808, 7_835_422, 10_507_776])
        );
        assert_eq!(
            completion_route_runtime.machine().random_seed(),
            0x5def_7434
        );
        assert_eq!(completion_route_runtime.draw_count(), 11_084);
        assert!(
            completion_route_survey.is_clean(),
            "Boulders completion route must remain clean: {}",
            completion_route_survey.summary()
        );

        let boulders_completion_carry: RetailSessionCarry = {
            let mut host = NsfProgramHost::new(&boulders_nsd, &boulders_nsf, &boulders_nsf_bytes);
            let report = completion_route_runtime
                .finish_level_transition(
                    &mut host,
                    i32::try_from(LevelId::LEVEL_COMPLETE.get()).unwrap(),
                )
                .expect("Boulders LEVEL_END must export a Level Complete carry");
            assert!(
                report.event_failures.is_empty(),
                "Boulders LEVEL_END handlers must complete cleanly: {:?}",
                report.event_failures
            );
            assert_eq!(report.resolved.level, LevelId::LEVEL_COMPLETE);
            assert!(!report.resolved.bonus_return);
            assert!(report.effects.is_empty());
            report.carry
        };
        assert_eq!(boulders_completion_carry.random_seed, 0x5def_7434);
        assert_eq!(boulders_completion_carry.draw_count, 11_084);
        assert_eq!(
            [
                GAME_STATE_GLOBAL,
                TITLE_STATE_GLOBAL,
                SAVED_TITLE_STATE_GLOBAL,
                CURRENT_MAP_LEVEL_GLOBAL,
                LEVEL_COUNT_GLOBAL,
                LEVELS_UNLOCKED_GLOBAL,
                ISLAND_CAMERA_STATE_GLOBAL,
            ]
            .map(|index| boulders_completion_carry.globals[index]),
            [
                0x500,
                TitleScreen::Map.raw(),
                TitleScreen::Map.raw(),
                4,
                1,
                5,
                0,
            ]
        );

        let boulders_completion_runtime = RetailRuntime::new_from_session(
            GLOBAL_WORDS,
            LevelId::LEVEL_COMPLETE,
            boulders_completion_carry,
        )
        .expect("Level Complete must import Boulders' session carry");
        let (boulders_completion_survey, mut boulders_completion_runtime) =
            survey_pair_with_runtime(
                known_name(LevelId::LEVEL_COMPLETE),
                LevelId::LEVEL_COMPLETE,
                &completion_nsd,
                &completion_nsf,
                &completion_nsf_bytes,
                boulders_completion_runtime,
                LevelContextSource::SessionGlobals,
                SurveyInputProfile::DirectionAndButtonSweepToTransition,
                COMPLETION_FRAMES,
            )
            .expect("Boulders' Level Complete runtime must execute");
        assert_eq!(boulders_completion_survey.frames, 105);
        assert_eq!(boulders_completion_survey.successful_spawns, 2);
        assert_eq!(boulders_completion_survey.spawn_attempts, 210);
        assert_eq!(boulders_completion_survey.expected_spawn_rejections, 208);
        assert_eq!(boulders_completion_survey.unexpected_spawn_errors, 0);
        assert_eq!(boulders_completion_survey.executions, 435);
        assert_eq!(boulders_completion_survey.restarts, 0);
        assert!(boulders_completion_survey.restart_frames.is_empty());
        assert_eq!(boulders_completion_survey.death_camera_frames, 0);
        assert!(boulders_completion_survey.first_below_zero.is_none());
        assert!(boulders_completion_survey.first_terminal_fall.is_none());
        assert_eq!(boulders_completion_survey.faulted_objects, 0);
        assert_eq!(boulders_completion_survey.execution_errors, 0);
        assert_eq!(
            boulders_completion_survey.next_lid,
            Some((105, i32::try_from(LevelId::TITLE.get()).unwrap()))
        );
        assert_eq!(
            boulders_completion_survey.terminal.as_deref(),
            Some("frame 105 requested level transition to 0x19")
        );
        assert_eq!(
            boulders_completion_survey
                .effect_counts
                .get("transition")
                .copied(),
            Some(1)
        );
        assert!(boulders_completion_survey.issue_counts.is_empty());
        assert!(
            boulders_completion_survey.is_clean(),
            "Boulders' Level Complete screen must remain clean: {}",
            boulders_completion_survey.summary()
        );
        assert_eq!(
            [
                GAME_STATE_GLOBAL,
                TITLE_STATE_GLOBAL,
                SAVED_TITLE_STATE_GLOBAL,
                CURRENT_MAP_LEVEL_GLOBAL,
                LEVEL_COUNT_GLOBAL,
                LEVELS_UNLOCKED_GLOBAL,
                ISLAND_CAMERA_STATE_GLOBAL,
            ]
            .map(|index| boulders_completion_runtime.global_word(index).unwrap()),
            [
                0x300,
                TitleScreen::Map.raw(),
                TitleScreen::Map.raw(),
                4,
                1,
                5,
                0,
            ]
        );
        assert_eq!(
            boulders_completion_runtime.machine().random_seed(),
            0x031a_a015
        );
        assert_eq!(boulders_completion_runtime.draw_count(), 11_189);

        let post_boulders_title_carry: RetailSessionCarry = {
            let mut host =
                NsfProgramHost::new(&completion_nsd, &completion_nsf, &completion_nsf_bytes);
            let report = boulders_completion_runtime
                .finish_level_transition(&mut host, i32::try_from(LevelId::TITLE.get()).unwrap())
                .expect("fourth Level Complete LEVEL_END must export a Title carry");
            assert!(
                report.event_failures.is_empty(),
                "fourth Level Complete LEVEL_END handlers must complete cleanly: {:?}",
                report.event_failures
            );
            assert_eq!(
                report.requested_lid,
                i32::try_from(LevelId::TITLE.get()).unwrap()
            );
            assert_eq!(report.next_lid_after_event, report.requested_lid);
            assert_eq!(report.resolved.level, LevelId::TITLE);
            assert!(!report.resolved.bonus_return);
            assert!(report.effects.is_empty());
            report.carry
        };
        assert_eq!(post_boulders_title_carry.random_seed, 0x031a_a015);
        assert_eq!(post_boulders_title_carry.draw_count, 11_189);

        let mut post_boulders_map = AuthoredTitleMapHarness::from_session(
            &title_nsd,
            &title_nsf,
            &title_nsf_bytes,
            post_boulders_title_carry,
        );
        post_boulders_map.wait_until_ready(64);
        assert_eq!(post_boulders_map.frame, 10);
        for _ in 0..120 {
            post_boulders_map.step(0);
        }
        post_boulders_map.tap(PAD_UP);
        for _ in 0..120 {
            post_boulders_map.step(0);
        }
        post_boulders_map.step(PAD_CROSS);
        let upstream = LevelId::new_const(0x0f);
        let upstream_lid = i32::try_from(upstream.get()).unwrap();
        assert_eq!(post_boulders_map.frame, 253);
        assert_eq!(post_boulders_map.transitions, [(253, upstream_lid)]);
        assert_eq!(
            post_boulders_map.camera.location().path,
            RetailPathId {
                zone: Eid::from_name("1c_pZ").expect("fixed fifth map-zone EID is valid"),
                index: 1,
            }
        );
        assert_eq!(post_boulders_map.camera.location().progress.raw(), 2_304);
        assert_eq!(
            [
                GAME_STATE_GLOBAL,
                TITLE_STATE_GLOBAL,
                SAVED_TITLE_STATE_GLOBAL,
                CURRENT_MAP_LEVEL_GLOBAL,
                LEVEL_COUNT_GLOBAL,
                LEVELS_UNLOCKED_GLOBAL,
                ISLAND_CAMERA_STATE_GLOBAL,
            ]
            .map(|index| post_boulders_map.runtime.global_word(index).unwrap()),
            [
                0,
                TitleScreen::Map.raw(),
                TitleScreen::Map.raw(),
                5,
                1,
                5,
                1,
            ]
        );
        assert_eq!(
            post_boulders_map.runtime.machine().random_seed(),
            0xae2d_d893
        );
        assert_eq!(post_boulders_map.runtime.draw_count(), 11_442);

        let upstream_carry: RetailSessionCarry = {
            let mut host = NsfProgramHost::new(&title_nsd, &title_nsf, &title_nsf_bytes);
            let report = post_boulders_map
                .runtime
                .finish_level_transition(&mut host, upstream_lid)
                .expect("post-Boulders Map must export the Upstream carry");
            assert!(
                report.event_failures.is_empty(),
                "post-Boulders Map LEVEL_END handlers must complete cleanly: {:?}",
                report.event_failures
            );
            assert_eq!(report.requested_lid, upstream_lid);
            assert_eq!(report.next_lid_after_event, upstream_lid);
            assert_eq!(report.resolved.level, upstream);
            assert!(!report.resolved.bonus_return);
            assert!(report.effects.is_empty());
            report.carry
        };
        assert_eq!(upstream_carry.random_seed, 0xae2d_d893);
        assert_eq!(upstream_carry.draw_count, 11_442);
        assert_eq!(
            [
                GAME_STATE_GLOBAL,
                TITLE_STATE_GLOBAL,
                SAVED_TITLE_STATE_GLOBAL,
                CURRENT_MAP_LEVEL_GLOBAL,
                LEVEL_COUNT_GLOBAL,
                LEVELS_UNLOCKED_GLOBAL,
                ISLAND_CAMERA_STATE_GLOBAL,
            ]
            .map(|index| upstream_carry.globals[index]),
            [
                0,
                TitleScreen::Map.raw(),
                TitleScreen::Map.raw(),
                5,
                1,
                5,
                1,
            ]
        );

        let (upstream_nsd, upstream_nsf, upstream_nsf_bytes) =
            parse_local_pair(&root, upstream).expect("Upstream pair must parse");
        let upstream_pbak_entry = upstream_nsf
            .entries()
            .find(|entry| entry.entry_type == PBAK_ENTRY_TYPE)
            .expect("legally local Upstream pair must contain its authored PBAK");
        assert_eq!(
            upstream_pbak_entry.eid,
            Eid::from_name("pb0fB").expect("fixed Upstream PBAK EID is valid")
        );
        let upstream_pbak = load_pbak_entry(upstream_pbak_entry, &upstream_nsf_bytes)
            .expect("legally local Upstream PBAK must parse");
        assert_eq!(upstream_pbak.frames.len(), UPSTREAM_PBAK_FRAMES as usize);
        assert_eq!(upstream_pbak.ticks_per_frame, 34);

        let upstream_runtime =
            RetailRuntime::new_from_session(GLOBAL_WORDS, upstream, upstream_carry)
                .expect("Upstream must import the fourth-completion map carry");
        let (upstream_survey, upstream_runtime) = survey_pair_with_runtime(
            known_name(upstream),
            upstream,
            &upstream_nsd,
            &upstream_nsf,
            &upstream_nsf_bytes,
            upstream_runtime,
            LevelContextSource::SessionGlobals,
            SurveyInputProfile::UpstreamCarriedRecovery,
            2_300,
        )
        .expect("Upstream must recover from the carried-spawn PBAK phase mismatch");
        assert_eq!(upstream_survey.frames, 2_300);
        assert!(upstream_survey.terminal.is_none());
        assert_eq!(upstream_survey.final_live_objects, 30);
        assert_eq!(upstream_survey.max_live_objects, 97);
        assert_eq!(upstream_survey.successful_spawns, 90);
        assert_eq!(upstream_survey.spawn_attempts, 30_585);
        assert_eq!(upstream_survey.expected_spawn_rejections, 30_495);
        assert_eq!(upstream_survey.unexpected_spawn_errors, 0);
        assert_eq!(upstream_survey.executions, 89_957);
        assert_eq!(upstream_survey.zone_transitions, 11);
        assert_eq!(upstream_survey.camera_ranges.len(), 14);
        assert_eq!(upstream_survey.camera_path_changes, 19);
        assert_eq!(upstream_survey.last_camera_path_change, 1_895);
        assert_eq!(upstream_survey.last_camera_progress_change, 1_941);
        assert_eq!(upstream_survey.last_player_movement, 1_952);
        assert_eq!(upstream_survey.restarts, 3);
        assert_eq!(upstream_survey.restart_frames, [104, 231, 816]);
        assert_eq!(
            upstream_survey.effect_counts.get("load-state").copied(),
            Some(3)
        );
        assert!(!upstream_survey.effect_counts.contains_key("transition"));
        assert_eq!(
            upstream_survey.effect_counts.get("save-state").copied(),
            Some(1)
        );
        assert_eq!(upstream_survey.save_handshakes, 0);
        assert_eq!(upstream_survey.box_count_samples, [(1, 0), (1_935, 0x100)]);
        assert_eq!(
            upstream_survey.checkpoint_samples,
            [
                (1, -1, [2_303_232, 6_860_544, -5_172_480]),
                (1_935, 57 << 8, [2_252_800, 2_350_080, 15_564_288]),
            ]
        );
        assert_eq!(upstream_survey.saved_box_count_samples, [(1_935, 0)]);
        assert!(
            upstream_survey.spawn_flag_samples.contains(&(1_935, 57, 9)),
            "BoxsC subtype-four entity 57 must publish its native seen flags: {}",
            upstream_survey.summary()
        );
        assert_eq!(upstream_survey.death_camera_frames, 0);
        assert!(upstream_survey.first_below_zero.is_none());
        assert!(upstream_survey.first_terminal_fall.is_none());
        assert!(upstream_survey.next_lid.is_none());
        assert_eq!(upstream_survey.faulted_objects, 0);
        assert_eq!(upstream_survey.execution_errors, 0);
        assert!(upstream_survey.issue_counts.is_empty());
        let upstream_initial_camera = upstream_survey
            .initial_camera
            .expect("Upstream normal spawn begins on a camera path");
        let upstream_final_camera = upstream_survey
            .final_camera
            .expect("Upstream recovery retains a camera path");
        assert_eq!(
            upstream_initial_camera.path,
            RetailPathId {
                zone: Eid::from_name("0f_fZ").expect("fixed Upstream spawn-zone EID is valid"),
                index: 0,
            }
        );
        assert_eq!(upstream_initial_camera.progress.raw(), 256);
        assert_eq!(
            upstream_final_camera.path,
            RetailPathId {
                zone: Eid::from_name("0n_fZ").expect("fixed Upstream checkpoint-zone EID is valid"),
                index: 0,
            }
        );
        assert_eq!(upstream_final_camera.progress.raw(), 16_371);
        assert_eq!(
            upstream_survey.final_player_translation,
            Some([2_236_476, 2_380_788, 15_601_332])
        );
        assert_eq!(
            upstream_survey.player_minimum,
            Some([1_993_028, 1_580_414, 15_601_332])
        );
        assert_eq!(
            upstream_survey.player_maximum,
            Some([2_277_856, 2_676_538, 25_025_792])
        );
        assert_eq!(
            upstream_runtime.global_word(CHECKPOINT_ID_GLOBAL),
            Ok(57 << 8)
        );
        assert_eq!(upstream_runtime.global_word(BOX_COUNT_GLOBAL), Ok(0x100));
        let upstream_checkpoint = upstream_runtime
            .saved_level_state()
            .expect("BoxsC entity 57 must install a saved checkpoint snapshot");
        assert_eq!(upstream_checkpoint.box_count, 0);
        assert_eq!(
            upstream_checkpoint.player_translation,
            [2_252_800, 2_350_080, 15_564_288]
        );
        assert_eq!(upstream_runtime.machine().random_seed(), 0x526e_3d90);
        assert_eq!(upstream_runtime.draw_count(), 1_484);
        assert!(
            upstream_survey.is_clean(),
            "Upstream carried-spawn recovery must remain clean: {}",
            upstream_survey.summary()
        );
        eprintln!(
            concat!(
                "vertical-flow: Map -> N. Sanity at frame 11; N. Sanity -> Level Complete ",
                "at frame {} (draw {}); first Level Complete -> Title at frame {} (draw {}); ",
                "Map -> Jungle Rollers at frame 253 (draw {}); Jungle Rollers -> Level Complete ",
                "at frame {} (draw {}); second Level Complete -> Title at frame {} (draw {}); ",
                "Map -> The Great Gate at frame 253 (draw {}); Great Gate -> Level Complete at ",
                "frame {} (draw {}); third Level Complete -> Title at frame {} (draw {}); Map -> ",
                "Boulders at frame 253 (draw {}); Boulders legally local authored PBAK: 990 frames, ",
                "0Q_eZ:0@0 -> 0I_eZ:1@3840, 16 paths/21 changes, 10 zone transitions, 8 boxes, ",
                "RNG {:#010x}, draw {}; completion route: {} frames -> Level Complete, 48 paths/53 ",
                "changes, 26 zone transitions, 15 boxes, RNG {:#010x}, draw {}; fourth Level ",
                "Complete -> Title at frame {} (draw {}); Map -> Upstream at frame 253 (draw {}); ",
                "Upstream carried-spawn PBAK phase mismatch and recovery: {} frames, 3 prefix ",
                "restarts, first 0n checkpoint, RNG {:#010x}, draw {}",
            ),
            n_sanity_survey.next_lid.unwrap().0,
            n_sanity_draw_count,
            completion_survey.next_lid.unwrap().0,
            completion_draw_count,
            2_677,
            jungle_survey.next_lid.unwrap().0,
            5_223,
            jungle_completion_survey.next_lid.unwrap().0,
            5_529,
            5_782,
            great_gate_survey.frames,
            great_gate_runtime.draw_count(),
            great_gate_completion_survey.frames,
            great_gate_completion_runtime.draw_count(),
            post_great_gate_map.runtime.draw_count(),
            boulders_runtime.machine().random_seed(),
            boulders_runtime.draw_count(),
            completion_route_survey.frames,
            completion_route_runtime.machine().random_seed(),
            completion_route_runtime.draw_count(),
            boulders_completion_survey.frames,
            boulders_completion_runtime.draw_count(),
            post_boulders_map.runtime.draw_count(),
            upstream_survey.frames,
            upstream_runtime.machine().random_seed(),
            upstream_runtime.draw_count(),
        );
    }
}

#[test]
#[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
fn raw_bin_extraction_matches_every_local_pair_and_bootable_graph() {
    let disc_path = PathBuf::from(
        std::env::var_os("C1_DISC_IMAGE")
            .expect("C1_DISC_IMAGE must name a legally local NTSC-U raw BIN"),
    );
    let disc_bytes = std::fs::read(&disc_path)
        .unwrap_or_else(|error| panic!("{}: {error}", disc_path.display()));
    let disc = DiscImage::open(&disc_bytes)
        .unwrap_or_else(|error| panic!("{}: {error}", disc_path.display()));
    assert_eq!(disc.layout(), SectorLayout::RawMode2_2352);
    let streams = disc
        .discover_streams()
        .unwrap_or_else(|error| panic!("{}: {error}", disc_path.display()));
    streams.validate_complete_retail().unwrap();
    assert_eq!(streams.files().len(), 88);
    assert_eq!(streams.complete_pair_count(), 44);
    let extracted_root = std::env::var_os("C1_STREAM_DIR").map(PathBuf::from);
    let mut extracted_bytes = 0_u64;

    for known in KNOWN_LEVELS {
        let nsd_name = StreamName::new(known.id, StreamKind::Nsd);
        let nsf_name = StreamName::new(known.id, StreamKind::Nsf);
        let nsd_bytes = disc
            .read_stream(streams.get(nsd_name).expect("validated NSD is present"))
            .unwrap();
        let nsf_bytes = disc
            .read_stream(streams.get(nsf_name).expect("validated NSF is present"))
            .unwrap();
        extracted_bytes = extracted_bytes
            .checked_add(u64::try_from(nsd_bytes.len()).unwrap())
            .and_then(|total| total.checked_add(u64::try_from(nsf_bytes.len()).unwrap()))
            .expect("retail extraction byte count fits u64");
        if let Some(root) = &extracted_root {
            assert_eq!(
                nsd_bytes,
                std::fs::read(root.join(nsd_name.filename())).unwrap(),
                "raw-disc NSD extraction differs for {} ({})",
                known.name,
                known.id
            );
            assert_eq!(
                nsf_bytes,
                std::fs::read(root.join(nsf_name.filename())).unwrap(),
                "raw-disc NSF extraction differs for {} ({})",
                known.name,
                known.id
            );
        }
        let nsd = parse_nsd(&nsd_bytes, known.id).unwrap();
        let nsf = parse_nsf(&nsf_bytes, &nsd).unwrap();
        if known.bootable {
            graph_for_pair(known.id, &nsd, &nsf, &nsf_bytes).unwrap();
        } else {
            assert_eq!(known.id, LevelId::CAVE);
            assert!(nsd.ldat().is_none(), "Cave must remain index-only");
        }
    }
    eprintln!(
        "raw BIN {} yielded 88 exact streams across 44 pairs ({} bytes)",
        disc_path.display(),
        extracted_bytes
    );
}

#[test]
#[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
fn great_gate_yellow_gem_card_route_reaches_c8() {
    const YELLOW_GEM_BIT: u32 = 1 << 29;
    const ROUTE_FRAMES: u32 = 2_600;

    let disc_path = PathBuf::from(
        std::env::var_os("C1_DISC_IMAGE")
            .expect("C1_DISC_IMAGE must name a legally local NTSC-U raw BIN"),
    );
    let disc_bytes = std::fs::read(&disc_path)
        .unwrap_or_else(|error| panic!("{}: {error}", disc_path.display()));
    let disc = DiscImage::open(&disc_bytes)
        .unwrap_or_else(|error| panic!("{}: {error}", disc_path.display()));
    assert_eq!(disc.layout(), SectorLayout::RawMode2_2352);
    let streams = disc.discover_streams().expect("disc streams must parse");
    streams.validate_complete_retail().unwrap();

    let level = LevelId::new_const(0x12);
    let nsd_bytes = disc
        .read_stream(
            streams
                .get(StreamName::new(level, StreamKind::Nsd))
                .expect("Great Gate NSD is present"),
        )
        .expect("Great Gate NSD is readable");
    let nsf_bytes = disc
        .read_stream(
            streams
                .get(StreamName::new(level, StreamKind::Nsf))
                .expect("Great Gate NSF is present"),
        )
        .expect("Great Gate NSF is readable");
    let nsd = parse_nsd(&nsd_bytes, level).expect("Great Gate NSD must parse");
    let nsf = parse_nsf(&nsf_bytes, &nsd).expect("Great Gate NSF must parse");

    let save = SaveData {
        level_count: 1,
        initial_lives: 4 << 8,
        sfx_volume: 255,
        music_volume: 255,
        item_pool_1: YELLOW_GEM_BIT,
        gem_count: 1,
        ..SaveData::default()
    };
    let payload = CardPayload::encode(save);
    assert!(payload.is_valid());
    assert_eq!(&payload.as_bytes()[28..32], &YELLOW_GEM_BIT.to_le_bytes());
    let loaded = payload.decode().expect("retail card payload must decode");
    assert_eq!(loaded, save);

    let mut title_runtime = RetailRuntime::new_for_level(GLOBAL_WORDS, LevelId::TITLE);
    title_runtime
        .restore_card_save_data(loaded)
        .expect("Yellow Gem progression must restore through the card path");
    for (index, value) in [
        (GAME_STATE_GLOBAL, 0),
        (TITLE_STATE_GLOBAL, TitleScreen::Map.raw()),
        (SAVED_TITLE_STATE_GLOBAL, TitleScreen::Map.raw()),
        (CURRENT_MAP_LEVEL_GLOBAL, 3),
        (LEVELS_UNLOCKED_GLOBAL, 3),
        (ISLAND_CAMERA_STATE_GLOBAL, 1),
    ] {
        title_runtime
            .set_global_word(index, value)
            .expect("pre-Great-Gate map global must exist");
    }
    assert_eq!(
        [
            LEVEL_COUNT_GLOBAL,
            ITEM_POOL_1_GLOBAL,
            ITEM_POOL_2_GLOBAL,
            GEM_COUNT_GLOBAL,
        ]
        .map(|index| title_runtime.global_word(index).unwrap()),
        [1, YELLOW_GEM_BIT, 0, 1]
    );

    // This is the exact post-Jungle Title Map phase characterized by the
    // vertical-flow test. Only the legally local card entitlement differs.
    let mut carry = title_runtime.export_session_carry();
    carry.random_seed = 0x4a04_f4bf;
    carry.draw_count = 5_782;
    let runtime = RetailRuntime::new_from_session(GLOBAL_WORDS, level, carry)
        .expect("Great Gate must import the card-backed map carry");
    let (survey, runtime) = survey_pair_with_runtime(
        "The Great Gate Yellow Gem route",
        level,
        &nsd,
        &nsf,
        &nsf_bytes,
        runtime,
        LevelContextSource::SessionGlobals,
        SurveyInputProfile::GreatGateYellowGemExactCarry,
        ROUTE_FRAMES,
    )
    .expect("Great Gate Yellow Gem route must reach c8");

    let c8_path = RetailPathId {
        zone: Eid::from_name("c8_iZ").expect("fixed Great Gate route EID is valid"),
        index: 0,
    };
    let c8_range = survey
        .camera_ranges
        .get(&c8_path)
        .expect("Yellow Gem platforms must route the camera into c8");
    assert_eq!(
        *c8_range,
        CameraProgressRange {
            first_frame: 2_479,
            last_frame: ROUTE_FRAMES,
            minimum: 120,
            maximum: 14_067,
        }
    );
    let final_camera = survey
        .final_camera
        .expect("Yellow Gem route must retain its c8 camera");
    assert_eq!(final_camera.path, c8_path);
    assert_eq!(final_camera.progress.raw(), 14_067);
    assert_eq!(
        survey.final_player_translation,
        Some([1_836_800, -8_385_264, 132_864])
    );
    assert_eq!(survey.frames, ROUTE_FRAMES);
    assert_eq!(survey.restarts, 0);
    assert!(survey.restart_frames.is_empty());
    assert_eq!(survey.death_camera_frames, 0);
    assert!(survey.first_terminal_fall.is_none());
    assert!(survey.next_lid.is_none());
    assert!(survey.terminal.is_none());
    assert_eq!(survey.faulted_objects, 0);
    assert_eq!(survey.execution_errors, 0);
    assert!(survey.is_clean(), "{}", survey.summary());
    assert!(
        survey.observed_program_states.contains(&(
            Eid::from_name("GemsC").expect("fixed gem-platform EID is valid"),
            2,
        )),
        "card-backed Yellow Gem must activate subtype-five GemsC platforms"
    );
    assert!(
        !survey.observed_program_states.contains(&(
            Eid::from_name("WillC").expect("fixed player EID is valid"),
            32,
        )),
        "the alternate route must avoid normal WarpC"
    );
    assert_eq!(runtime.global_word(ITEM_POOL_1_GLOBAL), Ok(YELLOW_GEM_BIT));
    assert_eq!(runtime.global_word(ITEM_POOL_2_GLOBAL), Ok(0));
    assert_eq!(runtime.global_word(GEM_COUNT_GLOBAL), Ok(1));
}
