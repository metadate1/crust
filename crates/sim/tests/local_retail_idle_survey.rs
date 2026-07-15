//! Opt-in, legally local long-window survey of every retail stream pair.
//!
//! No game bytes or derived assets are written by these tests. The runtime
//! survey mirrors the browser's spawn -> camera -> GOOL order for a bounded
//! window and prints deterministic diagnostics instead of stopping at the
//! first level. The separate N. Sanity progression test drives an observable
//! camera/player-state route using only retail directional, jump, and spin pad
//! input for a default 18,000-frame window selected by `C1_PROGRESSION_FRAMES`.
//! A separate vertical-flow test retains the authored session carry across
//! N. Sanity Beach, Level Complete, and Title without writing any game data.
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
        KNOWN_LEVELS, LevelId, Nsd, Nsf, RetailPathId, RetailZoneGraph, StreamKind, StreamName,
        ZoneEntity, ZoneHeader, load_gool_state_program, parse_nsd, parse_nsf,
    },
};
use crust_sim::{
    camera::{
        RetailCameraEffect, RetailCameraFollowInput, RetailCameraInput, RetailCameraLocation,
        RetailCameraPose, RetailCameraRuntime, RetailDeathCameraInput, RetailDeathCameraState,
    },
    gool::{
        CodeAddress, CodeSegment, CollisionObjectReference, GoolProgramIdentity,
        ObjectHandle as VmObjectHandle, PagingHostOperation, PagingHostRequest, PagingHostResponse,
        RetailPadSnapshot, RetailTransformVectorsCamera, SendEventTarget, VmEffect,
        process_register,
    },
    object_arena::{NeighborZone, SpawnError},
    player::{PAD_CROSS, PAD_DOWN, PAD_LEFT, PAD_RIGHT, PAD_SQUARE, PAD_UP},
    retail_frame::RetailFrameState,
    retail_runtime::{
        CURRENT_ZONE_FLAGS_GLOBAL, NsfProgramError, NsfProgramHost, ProgramHost,
        RetailLevelStateContext, RetailRestartOutcome, RetailRuntime, RetailSessionCarry,
        RuntimeError, RuntimeObjectHandle, ZoneTerminationMode,
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
        }
    }

    const fn stops_at_transition(self) -> bool {
        matches!(
            self,
            Self::DirectionAndButtonSweepToTransition | Self::ForwardWithActions
        )
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SurveyInputController {
    profile: SurveyInputProfile,
    n_sanity: NSanityRouteController,
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
        }
    }

    fn held(
        &mut self,
        frame: u32,
        camera: RetailCameraLocation,
        player: Option<PlayerTrace>,
        checkpoint_id: i32,
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
        VmEffect::LoadState(_) => "load-state",
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
    if snapshot.level != level {
        let outcome = runtime
            .restart_saved_level(host)
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
        .restart_saved_level(host)
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
        let held = input_controller.held(
            frame,
            camera.location(),
            player_trace(&runtime)?,
            checkpoint_id_before_frame,
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
            && matches!(input_profile, SurveyInputProfile::ForwardWithActions)
            && frame >= 300
        {
            eprintln!(
                "route f{frame} held={held:#06x} camera={:?} player={player:?}",
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

        if report
            .effects
            .iter()
            .any(|effect| matches!(effect, VmEffect::LoadState(_)))
        {
            match apply_restart(
                frame,
                level,
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
fn lights_out_active_input_keeps_dark2_alive_across_doctor_pool_reclaim() {
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
        "the characterized input must exercise the doctor teardown path: {}",
        survey.summary()
    );
    let doctor_word = runtime
        .global_word(DOCTOR_OBJECT_GLOBAL)
        .expect("the retail global table contains doctor");
    let doctor = CollisionObjectReference::from_word(doctor_word)
        .expect("Lights Out retains the authored non-null doctor pool pointer");
    assert_eq!(
        runtime.object_for_vm(doctor.object()),
        None,
        "the doctor identity must be reclaimed at the characterized boundary"
    );
    assert!(
        runtime
            .machine()
            .retired_retail_translation(doctor.object())
            .is_some(),
        "the static native pool slot must retain its last initialized translation"
    );
    assert!(
        survey.is_clean(),
        "the stale-but-addressable native doctor slot must not fault Dark2: {}",
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
fn authored_n_sanity_completion_title_vertical_flow_preserves_session_carry() {
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

    let n_sanity = LevelId::N_SANITY_BEACH;
    let (n_sanity_nsd, n_sanity_nsf, n_sanity_nsf_bytes) =
        parse_local_pair(&root, n_sanity).expect("N. Sanity pair must parse");
    let (n_sanity_survey, mut n_sanity_runtime) = survey_pair_with_runtime(
        known_name(n_sanity),
        n_sanity,
        &n_sanity_nsd,
        &n_sanity_nsf,
        &n_sanity_nsf_bytes,
        RetailRuntime::new_for_level(GLOBAL_WORDS, n_sanity),
        LevelContextSource::FreshBoot,
        SurveyInputProfile::ForwardWithActions,
        N_SANITY_FRAMES,
    )
    .expect("N. Sanity authored route must execute");
    assert_eq!(
        n_sanity_survey.next_lid.map(|(_, level)| level),
        Some(i32::try_from(LevelId::LEVEL_COMPLETE.get()).unwrap()),
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
    assert!(
        n_sanity_draw_count > 0,
        "the completed source level must have advanced the native draw counter"
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
        completion_survey.next_lid.map(|(_, level)| level),
        Some(i32::try_from(LevelId::TITLE.get()).unwrap()),
        "authored completion input must request Title: {}",
        completion_survey.summary()
    );
    assert!(
        completion_survey.is_clean(),
        "Level Complete reached a checked runtime boundary: {}",
        completion_survey.summary()
    );

    let completion_draw_count = completion_runtime.draw_count();
    assert!(completion_draw_count > n_sanity_draw_count);
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

    let title = LevelId::TITLE;
    let (title_nsd, title_nsf, title_nsf_bytes) =
        parse_local_pair(&root, title).expect("Title pair must parse");
    let mut title_runtime = RetailRuntime::new_from_session(GLOBAL_WORDS, title, title_carry)
        .expect("Title must import Level Complete's session carry");
    assert_eq!(title_runtime.draw_count(), completion_draw_count);
    let title_graph = graph_for_pair(title, &title_nsd, &title_nsf, &title_nsf_bytes)
        .expect("Title graph must parse");
    let (_, title_lifecycle) = zone_catalog(
        &title_nsd,
        &title_nsf,
        &title_nsf_bytes,
        &title_graph,
        title,
    )
    .expect("Title zone catalog must parse");
    let title_camera =
        RetailCameraRuntime::new(&title_graph).expect("Title camera runtime must initialize");
    seed_mounted_level_context_from_globals(
        &mut title_runtime,
        &title_graph,
        &title_lifecycle,
        title_camera.location(),
    )
    .expect("Title mount context must use the carried retail globals");
    let mut title_host = NsfProgramHost::new(&title_nsd, &title_nsf, &title_nsf_bytes);
    let title_core = title_runtime
        .create_retail_core_objects(title_camera.location().path.zone, &mut title_host)
        .expect("Title core runtime must boot cleanly");
    assert_eq!(
        title_core, None,
        "Title intentionally has no gameplay HUD/core roots; its MDAT objects are host-owned"
    );
    title_runtime.set_frame_timing(34, 34);
    title_runtime
        .set_pad_snapshot(0, RetailPadSnapshot::default())
        .expect("Title must accept an idle retail pad snapshot");
    let title_frame = title_runtime
        .run_frame(&mut title_host, INSTRUCTION_BUDGET)
        .expect("Title's empty core frame must remain a valid runtime frame");
    assert_eq!(title_runtime.level(), Some(title));
    assert!(title_runtime.level_state_context().is_some());
    assert!(title_runtime.arena().is_empty());
    assert!(title_frame.executions.is_empty());
    assert!(title_frame.effects.is_empty());
    assert_eq!(title_runtime.faulted_object_count(), 0);
    assert_eq!(
        title_runtime.draw_count(),
        completion_draw_count.wrapping_add(1),
        "Title's first display frame must continue the cross-stream phase"
    );
    eprintln!(
        "vertical-flow: N. Sanity -> Level Complete at frame {} with draw count {}; Level Complete -> Title at frame {} with draw count {}; Title first frame advanced to {}",
        n_sanity_survey.next_lid.unwrap().0,
        n_sanity_draw_count,
        completion_survey.next_lid.unwrap().0,
        completion_draw_count,
        title_runtime.draw_count(),
    );
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
