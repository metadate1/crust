//! Opt-in title MDAT runtime characterization against legally local retail data.

use std::path::PathBuf;

use crust_formats::binary::{Eid, EntryRef};
use crust_formats::disc::DiscImage;
use crust_formats::stream::{
    LevelId, RetailPathId, StreamKind, StreamName, ZoneEntity, ZoneEntityPathPoint, ZoneHeader,
    load_title_mdat, parse_nsd, parse_nsf,
};
use crust_sim::camera::RetailCameraLocation;
use crust_sim::flow::{TitlePhase, TitleScreen};
use crust_sim::gool::{
    ModelVertexSource, NEXT_DISPLAY_GLOBAL, RetailPadSnapshot, RetailSolidEnvironment,
    TITLE_STATE_GLOBAL, VmEffect, VmObject, VmStateProgram,
};
use crust_sim::object_arena::NeighborZone;
use crust_sim::object_bounds::AnimationBoundSource;
use crust_sim::player::PAD_START;
use crust_sim::retail_frame::PathProgress;
use crust_sim::retail_runtime::{
    AnimationBoundBinding, ModelVertexBinding, NsfProgramError, NsfProgramHost, ProgramBinding,
    ProgramHost, ProgramOrigin, RetailLevelStateContext, RetailRuntime, RetailTitleAction,
    RetailZoneEnvironment, StateProgramBinding,
};

const RETAIL_GLOBAL_WORDS: usize = 256;
const RETAIL_INSTRUCTION_BUDGET: usize = 67;
const ACTIVE_ZONE_DISPLAY_BIT: u32 = 2;
// TitleLoadScreen(type = 0) preserves the category tail from the preceding
// all-enabled word and adds the image/title-loaded bits. TitleUpdate then adds
// the global display/animate pair before the GLUpdate latch.
const IMAGE_ONLY_TITLE_LOAD_MASK: u32 = 0x22_3ff0;
const IMAGE_ONLY_TITLE_ACTIVE_MASK: u32 = IMAGE_ONLY_TITLE_LOAD_MASK | 0x0c;
const SYNTHETIC_REQUESTER_ID: u16 = 303;
const RETURN: u32 = 0x8289_4000;

const fn misc(primary: u32, secondary: i32, operand: u16) -> u32 {
    (0x1c_u32 << 24)
        | ((primary & 0x0f) << 20)
        | ((secondary.cast_unsigned() & 0x1f) << 15)
        | (operand as u32 & 0x0fff)
}

struct TitleMdatHost<'a> {
    inner: NsfProgramHost<'a>,
    mdat: Eid,
}

impl<'a> TitleMdatHost<'a> {
    const fn new(
        metadata: &'a crust_formats::stream::Nsd,
        nsf: &'a crust_formats::stream::Nsf,
        nsf_bytes: &'a [u8],
        mdat: Eid,
    ) -> Self {
        Self {
            inner: NsfProgramHost::new(metadata, nsf, nsf_bytes),
            mdat,
        }
    }
}

impl ProgramHost for TitleMdatHost<'_> {
    type Error = NsfProgramError;

    fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
        if matches!(binding.origin, ProgramOrigin::Entity(entity) if entity.id == SYNTHETIC_REQUESTER_ID)
        {
            return VmObject::new(binding.object.vm(), vec![misc(12, 7, 0x0be0), RETURN])
                .map_err(NsfProgramError::Vm);
        }
        self.inner.bind_program(binding)
    }

    fn bind_state_program(
        &mut self,
        binding: StateProgramBinding,
    ) -> Result<VmStateProgram, Self::Error> {
        self.inner.bind_state_program(binding)
    }

    fn zone_environment(
        &mut self,
        zone: Eid,
    ) -> Result<Option<RetailZoneEnvironment>, Self::Error> {
        if zone == self.mdat {
            Ok(None)
        } else {
            self.inner.zone_environment(zone)
        }
    }

    fn solid_environment(
        &mut self,
        zone: Eid,
    ) -> Result<Option<RetailSolidEnvironment>, Self::Error> {
        if zone == self.mdat {
            Ok(None)
        } else {
            self.inner.solid_environment(zone)
        }
    }

    fn current_zone_neighbors(&mut self, current_zone: Eid) -> Result<Vec<Eid>, Self::Error> {
        self.inner.current_zone_neighbors(current_zone)
    }

    fn animation_bound_source(
        &mut self,
        binding: AnimationBoundBinding,
    ) -> Result<Option<AnimationBoundSource>, Self::Error> {
        self.inner.animation_bound_source(binding)
    }

    fn model_vertex_source(
        &mut self,
        binding: ModelVertexBinding,
    ) -> Result<Option<ModelVertexSource>, Self::Error> {
        self.inner.model_vertex_source(binding)
    }
}

fn title_current_zone(state: u8) -> Eid {
    let name = match state {
        5 => "0c_pZ",
        8 => "0d_pZ",
        7 | 10 => "0a_pZ",
        _ => panic!("state {state} has no image-backed title zone fixture"),
    };
    Eid::from_name(name).unwrap()
}

fn title_level_context(current_zone: Eid, header: &ZoneHeader) -> RetailLevelStateContext {
    RetailLevelStateContext {
        location: RetailCameraLocation {
            path: RetailPathId {
                zone: current_zone,
                index: 0,
            },
            progress: PathProgress::ZERO,
        },
        graphics_flags: header.graphics.flags,
        box_count: 0,
        checkpoint_id: -1,
        checkpoint_translation: [0; 3],
        first_spawn: false,
        active_neighbor_zones: header.neighbors.clone(),
    }
}

fn synthetic_neighbor_requester() -> ZoneEntity {
    ZoneEntity {
        serialized_parent: EntryRef::from_raw(0),
        spawn_flags: 0,
        group: 3,
        id: SYNTHETIC_REQUESTER_ID,
        initializer: [0; 3],
        executable: 2,
        subtype: 0,
        path_points: vec![ZoneEntityPathPoint { x: 0, y: 0, z: 0 }],
    }
}

#[test]
#[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
fn image_title_mdat_objects_use_current_zdat_zone_colors_and_neighbor_termination() {
    let disc_path = PathBuf::from(
        std::env::var_os("C1_DISC_IMAGE")
            .expect("C1_DISC_IMAGE must name a legally local NTSC-U raw BIN"),
    );
    let disc_bytes = std::fs::read(&disc_path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", disc_path.display()));
    let disc = DiscImage::open(&disc_bytes)
        .unwrap_or_else(|error| panic!("{}: {error}", disc_path.display()));
    let streams = disc.discover_streams().expect("could not discover streams");
    let nsd_bytes = disc
        .read_stream(
            streams
                .get(StreamName::new(LevelId::TITLE, StreamKind::Nsd))
                .expect("disc is missing title NSD"),
        )
        .expect("could not read title NSD");
    let nsf_bytes = disc
        .read_stream(
            streams
                .get(StreamName::new(LevelId::TITLE, StreamKind::Nsf))
                .expect("disc is missing title NSF"),
        )
        .expect("could not read title NSF");
    let nsd = parse_nsd(&nsd_bytes, LevelId::TITLE).expect("invalid title NSD");
    let nsf = parse_nsf(&nsf_bytes, &nsd).expect("invalid title NSF");

    for (state, expected_spawnable) in [(5_u8, 0_usize), (7, 1), (8, 0), (10, 1)] {
        let mdat = load_title_mdat(&nsd, &nsf, &nsf_bytes, state)
            .unwrap_or_else(|error| panic!("title state {state}: {error}"));
        let eligible = mdat
            .entities
            .iter()
            .filter(|entity| {
                entity
                    .path_points
                    .first()
                    .is_some_and(|point| i64::from(point.z) <= 32)
            })
            .cloned()
            .collect::<Vec<_>>();
        let current_zone = title_current_zone(state);
        let current_entry = nsf
            .resolve_entry(&nsd, current_zone)
            .unwrap_or_else(|error| {
                panic!("title state {state} current zone {current_zone}: {error}")
            });
        let current_header = ZoneHeader::parse(
            current_entry
                .item(0)
                .expect("title ZDAT has no header")
                .bytes(&nsf_bytes)
                .expect("title ZDAT header bytes are invalid"),
        )
        .expect("title ZDAT header is invalid");
        let neighbors = [NeighborZone {
            eid: current_zone,
            display_flags: ACTIVE_ZONE_DISPLAY_BIT,
            entities: &eligible,
        }];
        let mut runtime = RetailRuntime::new_for_level(RETAIL_GLOBAL_WORDS, LevelId::TITLE);
        runtime.set_level_state_context(title_level_context(current_zone, &current_header));
        if state == 10 {
            runtime
                .configure_retail_title(TitleScreen::PublisherFirst, true)
                .expect("title runtime must be configurable");
            runtime
                .set_global_word(NEXT_DISPLAY_GLOBAL, IMAGE_ONLY_TITLE_LOAD_MASK)
                .expect("next-display global must exist");
        }
        let attempts = {
            let mut host = TitleMdatHost::new(&nsd, &nsf, &nsf_bytes, mdat.eid);
            runtime.spawn_current_zone_neighbors(&neighbors, &mut host)
        };
        assert_eq!(attempts.len(), expected_spawnable, "title state {state}");
        assert!(
            attempts.iter().all(|attempt| attempt.result.is_ok()),
            "title state {state}: {attempts:?}"
        );
        let spawned = attempts
            .iter()
            .map(|attempt| {
                let object = *attempt.result.as_ref().unwrap();
                assert_eq!(attempt.zone, current_zone, "title state {state}");
                assert_eq!(
                    runtime.arena().get(object.arena()).unwrap().zone(),
                    current_zone,
                    "title state {state} must rewrite the MDAT provenance to cur_zone"
                );
                let expected_colors = if object.arena().is_dedicated_main() {
                    current_header.graphics.player_colors.words
                } else {
                    current_header.graphics.object_colors.words
                };
                assert_eq!(
                    runtime
                        .machine()
                        .object(object.vm())
                        .unwrap()
                        .retail_colors(),
                    &expected_colors,
                    "title state {state} must initialize colors from cur_zone"
                );
                object
            })
            .collect::<Vec<_>>();

        if state == 7
            && let Some(&mdat_object) = spawned.first()
        {
            assert!(
                current_header.neighbors.contains(&current_zone),
                "the authored current-header sweep must include the current title zone"
            );
            let requester = synthetic_neighbor_requester();
            let requester_attempt = {
                let requester_zone = [NeighborZone {
                    eid: mdat.eid,
                    display_flags: ACTIVE_ZONE_DISPLAY_BIT,
                    entities: std::slice::from_ref(&requester),
                }];
                let mut host = TitleMdatHost::new(&nsd, &nsf, &nsf_bytes, mdat.eid);
                runtime.spawn_current_zone_neighbors(&requester_zone, &mut host)
            };
            let requester_object = *requester_attempt[0].result.as_ref().unwrap();
            let frame = {
                let mut host = TitleMdatHost::new(&nsd, &nsf, &nsf_bytes, mdat.eid);
                runtime
                    .run_frame(&mut host, RETAIL_INSTRUCTION_BUDGET)
                    .unwrap_or_else(|error| panic!("title state {state} frame: {error:?}"))
            };
            assert_eq!(runtime.object_for_vm(mdat_object.vm()), None);
            assert_eq!(
                runtime.object_for_vm(requester_object.vm()),
                Some(requester_object)
            );
            assert!(frame.effects.iter().any(|effect| {
                matches!(
                    effect,
                    VmEffect::TerminateCurrentZoneNeighbors { requester }
                        if *requester == requester_object.vm()
                )
            }));
            continue;
        }

        let frame = {
            let mut host = TitleMdatHost::new(&nsd, &nsf, &nsf_bytes, mdat.eid);
            if state == 10 {
                runtime
                    .run_frame_before_display(&mut host, RETAIL_INSTRUCTION_BUDGET)
                    .unwrap_or_else(|error| panic!("title state {state} frame: {error:?}"))
            } else {
                runtime
                    .run_frame(&mut host, RETAIL_INSTRUCTION_BUDGET)
                    .unwrap_or_else(|error| panic!("title state {state} frame: {error:?}"))
            }
        };
        if state == 10 {
            assert_eq!(runtime.begin_retail_title_update(), Ok(None));
            runtime.finish_retail_title_update().unwrap();
            runtime.finish_deferred_display_frame().unwrap();
        }
        assert_eq!(
            frame.executions.len(),
            expected_spawnable,
            "title state {state}"
        );
        assert!(
            frame
                .executions
                .iter()
                .all(|execution| execution.result.is_ok()),
            "title state {state}: {:?}",
            frame.executions
        );

        if state == 10 {
            assert_eq!(
                runtime.global_word(TITLE_STATE_GLOBAL),
                Ok(u32::from(state)),
                "the controller must not skip the first card without input"
            );

            assert_eq!(
                runtime.current_display_mask(),
                IMAGE_ONLY_TITLE_ACTIVE_MASK,
                "TitleUpdate must add display/animate before GLUpdate latches the word"
            );

            // State ten opens its authored skip gate on frame 64. Opcode
            // 0x1a's two-frame tapped mode must therefore retain the coherent
            // Start edge from frame 63 for that exact transition frame.
            let mut transition_frame = None;
            for frame in 1..=64 {
                let start_pressed = frame == 63;
                runtime
                    .set_pad_snapshot(
                        0,
                        RetailPadSnapshot {
                            tapped: u32::from(start_pressed) * PAD_START,
                            held: u32::from(start_pressed) * PAD_START,
                            held_previous: u32::from(frame == 64) * PAD_START,
                            tapped_previous: u32::from(frame == 64) * PAD_START,
                            ..RetailPadSnapshot::default()
                        },
                    )
                    .expect("retail pad zero must exist");
                let input_frame = {
                    let mut host = TitleMdatHost::new(&nsd, &nsf, &nsf_bytes, mdat.eid);
                    runtime
                        .run_frame_before_display(&mut host, RETAIL_INSTRUCTION_BUDGET)
                        .unwrap_or_else(|error| {
                            panic!("title state {state} input frame {frame}: {error:?}")
                        })
                };
                assert_eq!(runtime.begin_retail_title_update(), Ok(None));
                runtime.finish_retail_title_update().unwrap();
                runtime.finish_deferred_display_frame().unwrap();
                assert!(
                    input_frame
                        .executions
                        .iter()
                        .all(|execution| execution.result.is_ok()),
                    "title state {state} input frame {frame}: {:?}",
                    input_frame.executions
                );
                if runtime.global_word(TITLE_STATE_GLOBAL) == Ok(7) {
                    transition_frame = Some(frame);
                    break;
                }
            }
            assert_eq!(
                transition_frame,
                Some(64),
                "the authored state-ten controller must consume the preceding Start edge through its two-frame tapped-button gate"
            );
            assert_eq!(
                runtime.retail_title_presentation().unwrap().unwrap(),
                crust_sim::retail_runtime::RetailTitlePresentation {
                    screen: TitleScreen::PublisherFirst,
                    next_screen: TitleScreen::PublisherSecond,
                    phase: TitlePhase::FadingOut,
                    opaque_swap_overlay: false,
                    fade_counter: -224,
                },
                "the authored request must seed global fade before the same frame's GLUpdate"
            );

            let mut load_action = None;
            for frame in 1..=10 {
                let transition = {
                    let mut host = TitleMdatHost::new(&nsd, &nsf, &nsf_bytes, mdat.eid);
                    runtime
                        .run_frame_before_display(&mut host, RETAIL_INSTRUCTION_BUDGET)
                        .unwrap_or_else(|error| {
                            panic!("title state {state} fade frame {frame}: {error:?}")
                        })
                };
                assert!(
                    transition
                        .executions
                        .iter()
                        .all(|execution| execution.result.is_ok()),
                    "title state {state} fade frame {frame}: {:?}",
                    transition.executions
                );
                let action = runtime.begin_retail_title_update().unwrap();
                runtime.finish_retail_title_update().unwrap();
                runtime.finish_deferred_display_frame().unwrap();
                if action.is_some() {
                    load_action = action;
                    break;
                }
            }
            assert_eq!(
                load_action,
                Some(RetailTitleAction::LoadScreen {
                    previous: TitleScreen::PublisherFirst,
                    screen: TitleScreen::PublisherSecond,
                }),
                "the legal publisher controller must reach the source TitleLoadState boundary"
            );
        }
    }
}
