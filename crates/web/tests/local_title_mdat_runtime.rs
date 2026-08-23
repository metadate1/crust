//! Opt-in title MDAT runtime characterization against legally local retail data.

use std::{collections::BTreeMap, path::PathBuf};

use crust_formats::binary::{Eid, EntryRef};
use crust_formats::disc::DiscImage;
use crust_formats::stream::{
    LevelId, Nsd, Nsf, ObjectVertexKind, RetailPathId, RetailZoneGraph, StreamKind, StreamName,
    ZoneEntity, ZoneEntityPathPoint, ZoneHeader, load_title_mdat, parse_nsd, parse_nsf,
};
use crust_platform::input::{
    PAD_CIRCLE, PAD_CROSS, PAD_DOWN, PAD_SQUARE, PAD_START, PAD_TRIANGLE, PadState,
};
use crust_sim::camera::{
    RetailCameraEffect, RetailCameraInput, RetailCameraLocation, RetailCameraOutcome,
    RetailCameraRuntime, RetailIslandCameraInput,
};
use crust_sim::card::{CardOperation, CardOutcome, CardPayload, SaveData, Slot, VirtualCard};
use crust_sim::flow::{TitlePhase, TitleScreen};
use crust_sim::gool::{
    CURRENT_MAP_LEVEL_GLOBAL, CardHostRequest, GAME_STATE_GLOBAL, INITIAL_LIFE_COUNT_GLOBAL,
    LEVELS_UNLOCKED_GLOBAL, LIFE_COUNT_GLOBAL, ModelVertexSource, NEXT_DISPLAY_GLOBAL,
    RetailPadSnapshot, RetailSolidEnvironment, SAVED_TITLE_STATE_GLOBAL, TITLE_STATE_GLOBAL,
    VmEffect, VmObject, VmStateProgram,
};
use crust_sim::object_arena::NeighborZone;
use crust_sim::object_arena::SpawnError;
use crust_sim::object_bounds::AnimationBoundSource;
use crust_sim::retail_frame::PathProgress;
use crust_sim::retail_runtime::{
    AnimationBoundBinding, CardHostResponse, ISLAND_CAMERA_ROTATION_GLOBAL,
    ISLAND_CAMERA_STATE_GLOBAL, ModelVertexBinding, NsfProgramError, NsfProgramHost,
    ProgramBinding, ProgramHost, ProgramOrigin, RetailLevelStateContext, RetailRuntime,
    RetailTitleAction, RetailZoneEnvironment, RuntimeError, StateProgramBinding,
};
use crust_sim::zone_lifecycle::{OrderedZoneLoadList, ZoneLifecycle, ZoneLifecycleZone};

const RETAIL_GLOBAL_WORDS: usize = 256;
const RETAIL_INSTRUCTION_BUDGET: usize = 67;
const CARD_SCREEN_MODE_GLOBAL: usize = 1;
const CARD_SCREEN_SAVE_MODE: u32 = 2;
const ACTIVE_ZONE_DISPLAY_BIT: u32 = 2;
// TitleLoadScreen(type = 0) preserves the category tail from the preceding
// all-enabled word and adds the image/title-loaded bits. TitleUpdate then adds
// the global display/animate pair before the GLUpdate latch.
const IMAGE_ONLY_TITLE_LOAD_MASK: u32 = 0x22_3ff0;
const IMAGE_ONLY_TITLE_ACTIVE_MASK: u32 = IMAGE_ONLY_TITLE_LOAD_MASK | 0x0c;
const SYNTHETIC_REQUESTER_ID: u16 = 303;
const RETURN: u32 = 0x8289_4000;
const TITLE_DIRECT_ZONES: [&str; 10] = [
    "0a_pZ", "0b_pZ", "0c_pZ", "0d_pZ", "0e_pZ", "0f_pZ", "1a_pZ", "1e_pZ", "2b_pZ", "3a_pZ",
];

const DISPLAY_IMAGES: u32 = 0x2_0000;
const DISPLAY_TITLE_LOADED: u32 = 0x20_0000;
const DISPLAY_WORLD: u32 = 0x0001;
const DISPLAY_OBJECTS_AND_ANIMATION: u32 = 0xfffc;
const DISPLAY_CAMERA_UPDATE: u32 = 0x0002;

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

struct TitleFlowHost<'assets, 'card> {
    inner: NsfProgramHost<'assets>,
    card: &'card mut VirtualCard,
    trace: Option<&'card mut Vec<CardRequestTrace>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CardRequestTrace {
    request: CardHostRequest,
    current: SaveData,
    response: CardHostResponse,
}

impl<'assets, 'card> TitleFlowHost<'assets, 'card> {
    fn new(
        metadata: &'assets Nsd,
        nsf: &'assets Nsf,
        nsf_bytes: &'assets [u8],
        card: &'card mut VirtualCard,
    ) -> Self {
        Self {
            inner: NsfProgramHost::new(metadata, nsf, nsf_bytes),
            card,
            trace: None,
        }
    }

    fn traced(
        metadata: &'assets Nsd,
        nsf: &'assets Nsf,
        nsf_bytes: &'assets [u8],
        card: &'card mut VirtualCard,
        trace: &'card mut Vec<CardRequestTrace>,
    ) -> Self {
        Self {
            inner: NsfProgramHost::new(metadata, nsf, nsf_bytes),
            card,
            trace: Some(trace),
        }
    }
}

impl ProgramHost for TitleFlowHost<'_, '_> {
    type Error = NsfProgramError;

    fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
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
        self.inner.zone_environment(zone)
    }

    fn solid_environment(
        &mut self,
        zone: Eid,
    ) -> Result<Option<RetailSolidEnvironment>, Self::Error> {
        self.inner.solid_environment(zone)
    }

    fn find_neighbor_zone(
        &mut self,
        current_zone: Eid,
        point: [i32; 3],
    ) -> Result<Option<Eid>, Self::Error> {
        self.inner.find_neighbor_zone(current_zone, point)
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

    fn animation_display_vertex_kind(
        &mut self,
        binding: AnimationBoundBinding,
    ) -> Result<Option<ObjectVertexKind>, Self::Error> {
        self.inner.animation_display_vertex_kind(binding)
    }

    fn model_vertex_source(
        &mut self,
        binding: ModelVertexBinding,
    ) -> Result<Option<ModelVertexSource>, Self::Error> {
        self.inner.model_vertex_source(binding)
    }

    fn handle_card_request(
        &mut self,
        request: CardHostRequest,
        current: SaveData,
    ) -> Result<CardHostResponse, Self::Error> {
        let operation = CardOperation::from_retail(request.operation);
        let part_index = usize::try_from(request.part_index).unwrap_or(usize::MAX);
        let outcome = self.card.control(operation, part_index, Some(current));
        let loaded = match outcome {
            Ok(CardOutcome::Loaded(save)) => Some(save),
            Ok(CardOutcome::Complete) | Err(_) => None,
        };
        let response = CardHostResponse {
            result: i32::from(outcome.is_err()),
            loaded,
            published: self.card.published_state(),
        };
        if let Some(trace) = &mut self.trace {
            trace.push(CardRequestTrace {
                request,
                current,
                response,
            });
        }
        Ok(response)
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

#[derive(Clone, Debug)]
struct OwnedTitleZone {
    eid: Eid,
    header: ZoneHeader,
    entities: Vec<ZoneEntity>,
}

fn load_legally_local_title_pair() -> (Nsd, Nsf, Vec<u8>) {
    let level = LevelId::TITLE;
    let (nsd_bytes, nsf_bytes) = if let Some(root) = std::env::var_os("C1_STREAM_DIR") {
        let root = PathBuf::from(root);
        let nsd_path = root.join(StreamName::new(level, StreamKind::Nsd).filename());
        let nsf_path = root.join(StreamName::new(level, StreamKind::Nsf).filename());
        let nsd = std::fs::read(&nsd_path)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", nsd_path.display()));
        let nsf = std::fs::read(&nsf_path)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", nsf_path.display()));
        (nsd, nsf)
    } else {
        let disc_path = PathBuf::from(
            std::env::var_os("C1_DISC_IMAGE")
                .expect("set C1_STREAM_DIR or C1_DISC_IMAGE to legally local NTSC-U game data"),
        );
        let disc_bytes = std::fs::read(&disc_path)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", disc_path.display()));
        let disc = DiscImage::open(&disc_bytes)
            .unwrap_or_else(|error| panic!("{}: {error}", disc_path.display()));
        let streams = disc.discover_streams().expect("could not discover streams");
        let nsd = disc
            .read_stream(
                streams
                    .get(StreamName::new(level, StreamKind::Nsd))
                    .expect("disc is missing title NSD"),
            )
            .expect("could not read title NSD");
        let nsf = disc
            .read_stream(
                streams
                    .get(StreamName::new(level, StreamKind::Nsf))
                    .expect("disc is missing title NSF"),
            )
            .expect("could not read title NSF");
        (nsd, nsf)
    };
    let nsd = parse_nsd(&nsd_bytes, level).expect("invalid title NSD");
    let nsf = parse_nsf(&nsf_bytes, &nsd).expect("invalid title NSF");
    (nsd, nsf, nsf_bytes)
}

fn title_zone_catalog(
    nsd: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
) -> (
    RetailZoneGraph,
    BTreeMap<Eid, OwnedTitleZone>,
    ZoneLifecycle,
) {
    let roots = TITLE_DIRECT_ZONES.map(|name| Eid::from_name(name).unwrap());
    let graph = RetailZoneGraph::from_pair_with_roots(nsd, nsf, nsf_bytes, roots)
        .expect("title zone graph must parse");
    let mut zones = BTreeMap::new();
    let mut lifecycle_zones = Vec::with_capacity(graph.zone_count());
    for node in graph.zones() {
        let entry = nsf
            .resolve_entry(nsd, node.eid)
            .unwrap_or_else(|error| panic!("title ZDAT {}: {error}", node.eid));
        let header = ZoneHeader::parse(
            entry
                .item(0)
                .unwrap_or_else(|| panic!("title ZDAT {} has no header", node.eid))
                .bytes(nsf_bytes)
                .unwrap(),
        )
        .unwrap();
        let entities = (0..header.entity_count)
            .map(|entity_index| {
                let item_index = usize::try_from(
                    header
                        .entity_item_index(entity_index)
                        .expect("title entity item must exist"),
                )
                .unwrap();
                ZoneEntity::parse(entry.item(item_index).unwrap().bytes(nsf_bytes).unwrap())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        lifecycle_zones.push(ZoneLifecycleZone::new(
            node.eid,
            header.display_flags,
            header.neighbors.iter().copied(),
            OrderedZoneLoadList::from(&header.load_list),
        ));
        zones.insert(
            node.eid,
            OwnedTitleZone {
                eid: node.eid,
                header,
                entities,
            },
        );
    }
    let lifecycle = ZoneLifecycle::new(lifecycle_zones).expect("title lifecycle must parse");
    (graph, zones, lifecycle)
}

fn title_screen_fixture(screen: TitleScreen) -> (&'static str, u32, bool) {
    match screen {
        TitleScreen::MainMenu => (
            "0c_pZ",
            DISPLAY_OBJECTS_AND_ANIMATION | DISPLAY_IMAGES | DISPLAY_TITLE_LOADED,
            true,
        ),
        TitleScreen::Options => (
            "0f_pZ",
            DISPLAY_WORLD
                | DISPLAY_OBJECTS_AND_ANIMATION
                | DISPLAY_CAMERA_UPDATE
                | DISPLAY_TITLE_LOADED,
            false,
        ),
        TitleScreen::GameOver => (
            "0b_pZ",
            DISPLAY_WORLD
                | DISPLAY_OBJECTS_AND_ANIMATION
                | DISPLAY_CAMERA_UPDATE
                | DISPLAY_TITLE_LOADED,
            false,
        ),
        TitleScreen::Map => (
            "1a_pZ",
            DISPLAY_WORLD
                | DISPLAY_OBJECTS_AND_ANIMATION
                | DISPLAY_CAMERA_UPDATE
                | DISPLAY_TITLE_LOADED,
            false,
        ),
        TitleScreen::Password | TitleScreen::Load => (
            "0e_pZ",
            DISPLAY_WORLD
                | DISPLAY_OBJECTS_AND_ANIMATION
                | DISPLAY_CAMERA_UPDATE
                | DISPLAY_TITLE_LOADED,
            false,
        ),
        _ => panic!("the authored menu harness does not mount {screen:?}"),
    }
}

#[test]
#[ignore = "set C1_STREAM_DIR or C1_DISC_IMAGE to legally local NTSC-U game data"]
fn authored_carried_game_over_routes_are_characterized() {
    let (nsd, nsf, nsf_bytes) = load_legally_local_title_pair();

    let mut neutral = AuthoredTitleHarness::carried_game_over(&nsd, &nsf, &nsf_bytes);
    assert_eq!(neutral.loaded, [(0, TitleScreen::GameOver)]);
    assert_eq!(
        neutral.runtime.level_state_context().unwrap().location.path,
        (RetailPathId {
            zone: Eid::from_name("0b_pZ").unwrap(),
            index: 0,
        }),
        "the carried fatal-life state must mount the authored 0b_pZ world"
    );
    assert_eq!(neutral.runtime.global_word(GAME_STATE_GLOBAL), Ok(0x200));
    assert_eq!(
        neutral.runtime.global_word(TITLE_STATE_GLOBAL),
        Ok(TitleScreen::GameOver.raw())
    );
    assert_eq!(neutral.runtime.global_word(LIFE_COUNT_GLOBAL), Ok(0));
    assert_eq!(
        neutral.runtime.global_word(INITIAL_LIFE_COUNT_GLOBAL),
        Ok(4 << 8)
    );
    assert_eq!(
        neutral
            .runtime
            .retail_title_presentation()
            .unwrap()
            .unwrap()
            .phase,
        TitlePhase::Blank
    );
    neutral.wait_until_ready(32);
    assert_eq!(neutral.frame, 10, "authored Game Over ready-frame drift");
    assert_eq!(neutral.runtime.arena().len(), 10);
    while neutral.frame < 157 {
        neutral.step(0);
    }
    assert_eq!(
        neutral.runtime.arena().len(),
        11,
        "the authored Game Over dwell object must appear before pre-frame 158"
    );
    while neutral.frame < 277 {
        neutral.step(0);
    }
    assert_eq!(
        neutral.runtime.arena().len(),
        10,
        "the authored Game Over dwell object must retire before pre-frame 278"
    );
    while neutral.frame < 400 {
        neutral.step(0);
    }
    let neutral_presentation = neutral
        .runtime
        .retail_title_presentation()
        .unwrap()
        .unwrap();
    assert_eq!(neutral_presentation.screen, TitleScreen::GameOver);
    assert_eq!(neutral_presentation.next_screen, TitleScreen::GameOver);
    assert_eq!(neutral_presentation.phase, TitlePhase::Ready);
    assert_eq!(neutral.runtime.global_word(GAME_STATE_GLOBAL), Ok(0x200));
    assert_eq!(neutral.runtime.global_word(LIFE_COUNT_GLOBAL), Ok(0));
    assert!(neutral.transitions.is_empty());

    let mut map = AuthoredTitleHarness::carried_game_over(&nsd, &nsf, &nsf_bytes);
    map.wait_until_ready(32);
    map.step(PAD_CROSS);
    assert_eq!(map.frame, 11);
    let map_fade = map.runtime.retail_title_presentation().unwrap().unwrap();
    assert_eq!(map_fade.screen, TitleScreen::GameOver);
    assert_eq!(map_fade.next_screen, TitleScreen::Map);
    assert_eq!(map_fade.phase, TitlePhase::FadingOut);
    assert_eq!(
        map.runtime.global_word(TITLE_STATE_GLOBAL),
        Ok(TitleScreen::Map.raw())
    );
    assert_eq!(
        map.runtime.global_word(LIFE_COUNT_GLOBAL),
        Ok(4 << 8),
        "the default Game Over choice restores the configured life stock immediately"
    );
    while map.frame < 19 {
        map.step(0);
    }
    assert_eq!(
        map.runtime
            .retail_title_presentation()
            .unwrap()
            .unwrap()
            .phase,
        TitlePhase::FinishedFadingOut
    );
    map.step(0);
    assert_eq!(map.frame, 20);
    assert_eq!(map.loaded.last(), Some(&(20, TitleScreen::Map)));
    let map_blank = map.runtime.retail_title_presentation().unwrap().unwrap();
    assert_eq!(map_blank.screen, TitleScreen::Map);
    assert_eq!(map_blank.phase, TitlePhase::Blank);
    while map.frame < 22 {
        map.step(0);
    }
    assert_eq!(
        map.runtime.global_word(GAME_STATE_GLOBAL),
        Ok(0),
        "the atomic Map controller clears the carried Game Over state on frame 22"
    );
    map.step(0);
    assert_eq!(map.frame, 23);
    assert_eq!(
        map.runtime.global_word(GAME_STATE_GLOBAL),
        Ok(0),
        "the cleared Map game state remains stable on frame 23"
    );
    map.wait_until_ready(32);
    assert_eq!(map.frame, 30, "post-Game-Over Map ready-frame drift");
    assert_eq!(map.runtime.global_word(LIFE_COUNT_GLOBAL), Ok(4 << 8));
    assert!(map.transitions.is_empty());

    let mut menu = AuthoredTitleHarness::carried_game_over(&nsd, &nsf, &nsf_bytes);
    menu.wait_until_ready(32);
    menu.step(PAD_DOWN);
    menu.step(0);
    menu.step(PAD_CROSS);
    assert_eq!(menu.frame, 13);
    assert_eq!(
        menu.runtime.global_word(TITLE_STATE_GLOBAL),
        Ok(TitleScreen::MainMenu.raw()),
        "the atomic alternate-selection tail requests Main Menu on its confirmation frame"
    );
    menu.step(0);
    menu.step(0);
    assert_eq!(menu.frame, 15);
    let menu_fade = menu.runtime.retail_title_presentation().unwrap().unwrap();
    assert_eq!(menu_fade.screen, TitleScreen::GameOver);
    assert_eq!(menu_fade.next_screen, TitleScreen::MainMenu);
    assert_eq!(menu_fade.phase, TitlePhase::FadingOut);
    assert_eq!(
        menu.runtime.global_word(TITLE_STATE_GLOBAL),
        Ok(TitleScreen::MainMenu.raw())
    );
    while menu.frame < 21 {
        menu.step(0);
    }
    assert_eq!(
        menu.runtime
            .retail_title_presentation()
            .unwrap()
            .unwrap()
            .phase,
        TitlePhase::FinishedFadingOut
    );
    menu.step(0);
    assert_eq!(menu.frame, 22);
    assert_eq!(menu.loaded.last(), Some(&(22, TitleScreen::MainMenu)));
    let menu_blank = menu.runtime.retail_title_presentation().unwrap().unwrap();
    assert_eq!(menu_blank.screen, TitleScreen::MainMenu);
    assert_eq!(menu_blank.phase, TitlePhase::Blank);
    menu.wait_until_ready(32);
    assert_eq!(menu.frame, 32, "post-Game-Over MainMenu ready-frame drift");
    assert_eq!(menu.runtime.global_word(GAME_STATE_GLOBAL), Ok(0x200));
    assert_eq!(menu.runtime.global_word(LIFE_COUNT_GLOBAL), Ok(4 << 8));
    assert_eq!(
        menu.runtime.global_word(INITIAL_LIFE_COUNT_GLOBAL),
        Ok(4 << 8)
    );
    assert!(menu.transitions.is_empty());
}

#[test]
#[ignore = "set C1_STREAM_DIR or C1_DISC_IMAGE to legally local NTSC-U game data"]
fn authored_main_menu_routes_and_password_card_handshake_are_characterized() {
    let (nsd, nsf, nsf_bytes) = load_legally_local_title_pair();

    let mut map = AuthoredTitleHarness::main_menu(&nsd, &nsf, &nsf_bytes);
    map.wait_until_ready(32);
    assert_eq!(map.frame, 10, "authored MainMenu ready-frame drift");
    map.tap(PAD_CROSS);
    assert_eq!(
        map.runtime.global_word(TITLE_STATE_GLOBAL).unwrap(),
        TitleScreen::Map.raw(),
        "default MainMenu selection must request Map"
    );

    let mut password_selection = AuthoredTitleHarness::main_menu(&nsd, &nsf, &nsf_bytes);
    password_selection.wait_until_ready(32);
    // Each tap includes a released frame so the authored edge-triggered menu
    // receives two distinct Down pulses before Cross.
    password_selection.tap(PAD_DOWN);
    password_selection.tap(PAD_DOWN);
    password_selection.tap(PAD_CROSS);
    assert_eq!(
        password_selection
            .runtime
            .global_word(TITLE_STATE_GLOBAL)
            .unwrap(),
        TitleScreen::Load.raw(),
        "two Down pulses request the shared Load/Password screen"
    );
    assert_eq!(
        password_selection.wait_for_loaded(TitleScreen::Load, 32),
        Some(24),
        "two-Down shared-screen mount-frame drift"
    );
    password_selection.wait_until_ready(32);
    assert_eq!(
        password_selection.wait_for_loaded(TitleScreen::Password, 64),
        None,
        "the two-Down selection must not be reported as an independent Password mount"
    );
    assert_eq!(
        password_selection
            .runtime
            .global_word(TITLE_STATE_GLOBAL)
            .unwrap(),
        TitleScreen::Load.raw(),
        "the shared screen retains its numeric Load state while handling Password internally"
    );
    let password_flags = password_selection.card.published_state().flags;
    assert_eq!(
        password_flags.bits(),
        0,
        "the Password selection must bypass the Load card-rescan latch"
    );

    let mut load = AuthoredTitleHarness::main_menu(&nsd, &nsf, &nsf_bytes);
    load.wait_until_ready(32);
    load.tap(PAD_DOWN);
    load.tap(PAD_CROSS);
    assert_eq!(
        load.runtime.global_word(TITLE_STATE_GLOBAL).unwrap(),
        TitleScreen::Load.raw(),
        "one Down pulse must select Load"
    );
    assert_eq!(
        load.wait_for_loaded(TitleScreen::Load, 32),
        Some(22),
        "Load screen mount-frame drift"
    );
    load.wait_until_ready(32);
    for _ in 0..64 {
        load.step(0);
    }
    let card_flags = load.card.published_state().flags;
    assert_eq!(
        card_flags.bits(),
        0,
        "CardC must observe CHECKING, issue ClearFlag6, and let the following card update finish the empty-card rescan"
    );
    assert_eq!(
        load.card.published_state().part_count,
        0,
        "the completed empty-card rescan must publish no parts"
    );
    assert_eq!(
        load.runtime.global_word(TITLE_STATE_GLOBAL).unwrap(),
        TitleScreen::Load.raw(),
        "the completed card handshake must remain on the authored empty-card Load screen"
    );

    let mut options = AuthoredTitleHarness::main_menu(&nsd, &nsf, &nsf_bytes);
    options.wait_until_ready(32);
    for _ in 0..3 {
        options.tap(PAD_DOWN);
    }
    options.tap(PAD_CROSS);
    assert_eq!(
        options.runtime.global_word(TITLE_STATE_GLOBAL).unwrap(),
        TitleScreen::Options.raw(),
        "three Down pulses must select Options"
    );
    assert_eq!(
        options.wait_for_loaded(TitleScreen::Options, 32),
        Some(26),
        "Options screen mount-frame drift"
    );
    // The first authored update reads the null interrupter bootstrap word;
    // the following all-root event installs the controller link. Keep the
    // screen alive long enough to cover both that handoff and steady state.
    for _ in 0..128 {
        options.step(0);
    }
}

#[test]
#[ignore = "set C1_STREAM_DIR or C1_DISC_IMAGE to legally local NTSC-U game data"]
fn authored_card_save_then_later_title_load_round_trip_reaches_gameplay() {
    let (nsd, nsf, nsf_bytes) = load_legally_local_title_pair();
    let expected_save = SaveData {
        level_count: 8,
        initial_lives: 7 << 8,
        unknown_6190c: 0x1234_5678,
        mono: true,
        sfx_volume: 239,
        music_volume: 223,
        item_pool_1: 0x2000_0000,
        gem_count: 1,
        ..SaveData::default()
    };

    // State 13 with CardC mode two is the exact authored save screen entered
    // from a gameplay carry. Start at that boundary, but let CardC perform its
    // own rescan, acknowledgement, slot selection, and physical save.
    let mut saving = AuthoredTitleHarness::main_menu(&nsd, &nsf, &nsf_bytes);
    saving.wait_until_ready(32);
    saving
        .runtime
        .restore_card_save_data(expected_save)
        .unwrap();
    saving
        .runtime
        .set_global_word(CARD_SCREEN_MODE_GLOBAL, CARD_SCREEN_SAVE_MODE)
        .unwrap();
    saving
        .runtime
        .set_global_word(TITLE_STATE_GLOBAL, TitleScreen::Password.raw())
        .unwrap();
    saving.step(0);
    assert_eq!(
        saving.wait_for_loaded(TitleScreen::Password, 64),
        Some(20),
        "carried retail save-screen mount-frame drift"
    );
    saving.wait_until_ready(64);
    for _ in 0..64 {
        if saving.card.published_state().flags.bits() == 0
            && saving
                .card_requests
                .iter()
                .any(|trace| trace.request.operation == 2)
        {
            break;
        }
        saving.step(0);
    }
    assert_eq!(
        saving
            .card_requests
            .iter()
            .map(|trace| (trace.request.operation, trace.request.part_index))
            .collect::<Vec<_>>(),
        [(10, 0), (10, 0), (2, 0)],
        "CardC must complete its authored pre-save rescan handshake"
    );
    assert_eq!(saving.card.published_state().part_count, 0);
    // CardC retains its native empty-card presentation dwell after the host
    // handshake becomes idle; input during that authored interval is ignored.
    for _ in 0..96 {
        saving.step(0);
    }
    let card_controller = saving
        .runtime
        .arena()
        .main_object()
        .and_then(|arena| saving.runtime.object_for_arena(arena))
        .and_then(|handle| saving.runtime.machine().object(handle.vm()).ok())
        .expect("CardC must own the save screen's main-object slot");
    assert_eq!(
        (card_controller.state(), card_controller.pc()),
        (24, 2_273),
        "mode two must be executing CardC's authored save-selection state"
    );

    saving.tap(PAD_CROSS);
    for _ in 0..192 {
        if saving
            .card_requests
            .iter()
            .any(|trace| trace.request.operation == 3)
        {
            break;
        }
        saving.step(0);
    }
    let save_request = saving
        .card_requests
        .iter()
        .find(|trace| trace.request.operation == 3)
        .copied()
        .expect("CardC must issue a save request on Cross");
    assert_eq!(
        (
            save_request.request.operation,
            save_request.request.part_index
        ),
        (3, 0),
        "CardC must use retail SaveSelected for the empty card"
    );
    assert_eq!(save_request.current, expected_save);
    assert_eq!(save_request.response.result, 0);
    assert_eq!(save_request.response.loaded, None);

    let saved_payload = match saving.card.slots()[0] {
        Slot::Valid(payload) => payload,
        slot => panic!("CardC must author slot zero, got {slot:?}"),
    };
    assert_eq!(saved_payload.as_bytes().len(), 128);
    assert!(saved_payload.is_valid());
    assert_eq!(saved_payload, CardPayload::encode(expected_save));
    assert_eq!(saved_payload.decode(), Ok(expected_save));
    assert_eq!(saving.card.current_slot(), Some(0));
    assert_eq!(saving.card.published_state().part_count, 1);
    assert!(
        saving.card.slots()[1..]
            .iter()
            .all(|slot| *slot == Slot::Empty)
    );

    // This carried save presentation has no authored return-to-menu branch:
    // the ordinary cancel/menu/face inputs all leave CardC in state 13 after
    // the successful write. A later title session is therefore the honest
    // persistence boundary for the load half of this round trip.
    let card_after_save = saving.card.clone();
    for button in [PAD_TRIANGLE, PAD_START, PAD_CIRCLE, PAD_SQUARE, PAD_DOWN] {
        saving.tap(button);
        for _ in 0..64 {
            saving.step(0);
        }
        assert_eq!(
            saving.runtime.global_word(TITLE_STATE_GLOBAL),
            Ok(TitleScreen::Password.raw())
        );
        assert_eq!(saving.card, card_after_save);
    }

    // A later browser/title session receives the card object produced above;
    // no test-side payload injection or high-level restore occurs on this leg.
    let saved_card = std::mem::take(&mut saving.card);
    let mut loading = AuthoredTitleHarness::main_menu_with_card(&nsd, &nsf, &nsf_bytes, saved_card);
    assert_eq!(loading.runtime.card_save_data(), Ok(default_title_save()));
    loading.wait_until_ready(32);
    loading.tap(PAD_DOWN);
    loading.tap(PAD_CROSS);
    assert_eq!(loading.wait_for_loaded(TitleScreen::Load, 32), Some(22));
    loading.wait_until_ready(32);
    for _ in 0..64 {
        if loading.card.published_state().flags.bits() == 0
            && loading.card.published_state().part_count == 1
        {
            break;
        }
        loading.step(0);
    }
    assert_eq!(loading.card.published_state().flags.bits(), 0);
    assert_eq!(loading.card.published_state().part_count, 1);
    assert_eq!(
        loading
            .card_requests
            .iter()
            .map(|trace| (trace.request.operation, trace.request.part_index))
            .collect::<Vec<_>>(),
        [(10, 0), (2, 0)],
        "fresh-session CardC must rescan the persisted slot before accepting input"
    );
    assert!(loading.card_loads.is_empty());
    assert_eq!(
        loading.runtime.card_save_data(),
        Ok(SaveData {
            gem_count: 1,
            ..default_title_save()
        }),
        "CardC may preview the selected part's packed gem count, but must not restore the payload before confirmation"
    );

    loading.tap(PAD_CROSS);
    for _ in 0..64 {
        if !loading.card_loads.is_empty()
            && loading.runtime.global_word(TITLE_STATE_GLOBAL) == Ok(TitleScreen::Map.raw())
        {
            break;
        }
        loading.step(0);
    }
    assert_eq!(loading.card_loads.len(), 1);
    assert_eq!(loading.card_loads[0].1, expected_save);
    let load_request = loading
        .card_requests
        .iter()
        .find(|trace| trace.request.operation == 4)
        .copied()
        .expect("CardC must issue retail LoadSelected");
    assert_eq!(load_request.request.part_index, 0);
    assert_eq!(load_request.response.result, 0);
    assert_eq!(load_request.response.loaded, Some(expected_save));
    assert_eq!(
        loading
            .card_requests
            .iter()
            .map(|trace| (trace.request.operation, trace.request.part_index))
            .collect::<Vec<_>>(),
        [(10, 0), (2, 0), (4, 0)],
        "only the authored rescan and confirmed LoadSelected handshake may touch the card"
    );
    assert_eq!(loading.runtime.card_save_data(), Ok(expected_save));
    assert_eq!(
        CardPayload::encode(loading.runtime.card_save_data().unwrap()),
        saved_payload,
        "the authored load must restore every represented payload byte"
    );
    assert_eq!(
        loading.runtime.global_word(GAME_STATE_GLOBAL),
        Ok(0x100),
        "CardC's successful-load branch must enter gameplay state"
    );
    assert_eq!(
        loading.runtime.global_word(TITLE_STATE_GLOBAL),
        Ok(TitleScreen::Map.raw())
    );

    assert!(loading.wait_for_loaded(TitleScreen::Map, 64).is_some());
    loading.wait_until_ready(64);
    for _ in 0..120 {
        loading.step(0);
    }
    loading.step(PAD_CROSS);
    assert_eq!(
        loading.transitions.last(),
        Some(&(loading.frame, 0x11)),
        "the restored eighth map node must request Hog Wild"
    );

    let report = {
        let mut host = TitleFlowHost::new(&nsd, &nsf, &nsf_bytes, &mut loading.card);
        loading
            .runtime
            .finish_level_transition(&mut host, 0x11)
            .expect("the loaded map session must export a gameplay carry")
    };
    assert!(report.event_failures.is_empty());
    assert_eq!(report.resolved.level, LevelId::new_const(0x11));
    let gameplay = RetailRuntime::new_from_session(
        RETAIL_GLOBAL_WORDS,
        LevelId::new_const(0x11),
        report.carry,
    )
    .expect("Hog Wild must import the card-restored session carry");
    assert_eq!(gameplay.card_save_data(), Ok(expected_save));
}

#[test]
#[ignore = "set C1_STREAM_DIR or C1_DISC_IMAGE to legally local NTSC-U game data"]
fn authored_valid_and_invalid_retail_password_routes_are_characterized() {
    let (nsd, nsf, nsf_bytes) = load_legally_local_title_pair();
    let initial_save = default_title_save();

    let mut valid = AuthoredTitleHarness::main_menu(&nsd, &nsf, &nsf_bytes);
    let valid_card_before = valid.card.clone();
    valid.wait_until_ready(32);
    valid.tap(PAD_DOWN);
    valid.tap(PAD_DOWN);
    valid.tap(PAD_CROSS);
    assert_eq!(valid.wait_for_loaded(TitleScreen::Load, 32), Some(24));
    valid.wait_until_ready(32);
    assert_eq!(valid.frame, 34, "authored Password ready-frame drift");

    // Eight-symbol retail password for the first 2%-progress save point.
    for button in [
        PAD_CIRCLE,
        PAD_SQUARE,
        PAD_CIRCLE,
        PAD_SQUARE,
        PAD_CIRCLE,
        PAD_CIRCLE,
        PAD_TRIANGLE,
        PAD_SQUARE,
    ] {
        valid.tap(button);
    }
    let restored = SaveData {
        level_count: 2,
        ..initial_save
    };
    assert_eq!(
        valid.frame, 50,
        "eight password pulses must consume 16 frames"
    );
    assert_eq!(valid.runtime.card_save_data(), Ok(restored));
    assert_eq!(
        valid.runtime.global_word(LEVELS_UNLOCKED_GLOBAL),
        Ok(2),
        "the retail decoder must publish the restored unlock count"
    );
    assert_eq!(
        valid.runtime.global_word(CURRENT_MAP_LEVEL_GLOBAL),
        Ok(2),
        "the retail decoder must select the restored map position"
    );
    assert_eq!(
        valid.card, valid_card_before,
        "password restore must not manufacture a virtual-card write"
    );

    valid.tap(PAD_CROSS);
    assert_eq!(valid.frame, 52);
    assert_eq!(
        valid.runtime.global_word(TITLE_STATE_GLOBAL),
        Ok(TitleScreen::Map.raw()),
        "Cross must accept a decoded password and request the world map"
    );
    assert_eq!(valid.wait_for_loaded(TitleScreen::Map, 32), Some(60));
    valid.wait_until_ready(32);
    assert_eq!(valid.frame, 70, "restored Map ready-frame drift");
    assert_eq!(valid.runtime.card_save_data(), Ok(restored));
    assert_eq!(valid.card, valid_card_before);
    assert!(valid.transitions.is_empty());

    let mut invalid = AuthoredTitleHarness::main_menu(&nsd, &nsf, &nsf_bytes);
    let invalid_card_before = invalid.card.clone();
    let invalid_resume_payload_before = CardPayload::encode(initial_save);
    invalid.wait_until_ready(32);
    invalid.tap(PAD_DOWN);
    invalid.tap(PAD_DOWN);
    invalid.tap(PAD_CROSS);
    assert_eq!(invalid.wait_for_loaded(TitleScreen::Load, 32), Some(24));
    invalid.wait_until_ready(32);
    for _ in 0..8 {
        invalid.tap(PAD_CIRCLE);
    }
    assert_eq!(invalid.frame, 50);
    assert_eq!(
        invalid.runtime.card_save_data(),
        Ok(initial_save),
        "the rejected code must not mutate browser-resumable progression"
    );
    assert_eq!(invalid.card, invalid_card_before);
    assert_eq!(
        CardPayload::encode(invalid.runtime.card_save_data().unwrap()),
        invalid_resume_payload_before,
        "the invalid code must not dirty the browser-resume payload"
    );

    // The first acknowledgement falls inside the authored error dwell and is
    // deliberately ignored. This proves the invalid path rather than merely
    // entering eight symbols and observing that no save decoded.
    invalid.tap(PAD_CROSS);
    assert_eq!(invalid.frame, 52);
    assert_eq!(
        invalid.runtime.global_word(TITLE_STATE_GLOBAL),
        Ok(TitleScreen::Load.raw())
    );
    assert_eq!(invalid.runtime.card_save_data(), Ok(initial_save));
    assert_eq!(invalid.card, invalid_card_before);
    assert_eq!(
        CardPayload::encode(invalid.runtime.card_save_data().unwrap()),
        invalid_resume_payload_before
    );
    for _ in 0..38 {
        invalid.step(0);
    }
    assert_eq!(invalid.frame, 90);
    assert_eq!(
        invalid.runtime.global_word(TITLE_STATE_GLOBAL),
        Ok(TitleScreen::Load.raw()),
        "the invalid-password error dwell must remain on the shared screen"
    );
    assert_eq!(invalid.runtime.card_save_data(), Ok(initial_save));
    assert_eq!(invalid.card, invalid_card_before);
    assert_eq!(
        CardPayload::encode(invalid.runtime.card_save_data().unwrap()),
        invalid_resume_payload_before
    );

    invalid.step(PAD_CROSS);
    assert_eq!(invalid.frame, 91);
    assert_eq!(
        invalid.runtime.global_word(TITLE_STATE_GLOBAL),
        Ok(TitleScreen::MainMenu.raw()),
        "the acknowledged invalid-password error must request MainMenu"
    );
    assert_eq!(invalid.runtime.card_save_data(), Ok(initial_save));
    invalid.step(0);
    assert_eq!(
        invalid.wait_for_loaded(TitleScreen::MainMenu, 32),
        Some(100)
    );
    invalid.wait_until_ready(32);
    assert_eq!(invalid.frame, 110, "returned MainMenu ready-frame drift");
    assert_eq!(invalid.runtime.card_save_data(), Ok(initial_save));
    assert_eq!(invalid.card, invalid_card_before);
    assert_eq!(
        CardPayload::encode(invalid.runtime.card_save_data().unwrap()),
        invalid_resume_payload_before,
        "the MainMenu reset/restore must retain the unmodified resume payload"
    );
    assert!(invalid.transitions.is_empty());
}

#[test]
#[ignore = "set C1_STREAM_DIR or C1_DISC_IMAGE to legally local NTSC-U game data"]
fn authored_damaged_slot_and_unreadable_card_routes_fail_closed() {
    let (nsd, nsf, nsf_bytes) = load_legally_local_title_pair();
    let mut title = AuthoredTitleHarness::main_menu(&nsd, &nsf, &nsf_bytes);
    title
        .card
        .set_slot(0, Slot::Corrupt)
        .expect("damaged fixture must fit virtual-card slot zero");

    title.wait_until_ready(32);
    title.tap(PAD_DOWN);
    title.tap(PAD_CROSS);
    assert_eq!(title.wait_for_loaded(TitleScreen::Load, 32), Some(22));
    title.wait_until_ready(32);
    for _ in 0..64 {
        title.step(0);
    }
    assert_eq!(title.card.published_state().flags.bits(), 0);
    assert_eq!(title.card.published_state().part_count, 1);
    assert_eq!(
        title.card.published_state().partinfos[0],
        3,
        "a damaged physical slot must remain visible as the retail damaged-part word"
    );

    let damaged_before = title.card.clone();
    title.tap(PAD_CROSS);
    for _ in 0..32 {
        title.step(0);
    }
    assert_eq!(title.card, damaged_before);
    assert_eq!(
        title.runtime.global_word(TITLE_STATE_GLOBAL),
        Ok(TitleScreen::Load.raw()),
        "a damaged individual save is visible but cannot be loaded"
    );
    title.tap(PAD_TRIANGLE);
    assert_eq!(
        title.runtime.global_word(TITLE_STATE_GLOBAL),
        Ok(TitleScreen::MainMenu.raw()),
        "Triangle must leave the damaged-slot list without altering the card"
    );
    assert_eq!(title.card, damaged_before);

    let mut unreadable = AuthoredTitleHarness::main_menu(&nsd, &nsf, &nsf_bytes);
    unreadable.card.set_storage_available(false);
    assert!(
        unreadable
            .card
            .control(CardOperation::Rescan, 0, None)
            .is_err(),
        "a malformed storage envelope must reject its initial browser rescan"
    );
    assert_eq!(unreadable.card.published_state().flags.bits(), 0x06);
    unreadable.wait_until_ready(32);
    unreadable.tap(PAD_DOWN);
    unreadable.tap(PAD_CROSS);
    assert_eq!(unreadable.wait_for_loaded(TitleScreen::Load, 32), Some(22));
    unreadable.wait_until_ready(32);
    for _ in 0..64 {
        unreadable.step(0);
    }
    assert_eq!(unreadable.card.published_state().flags.bits(), 0x06);
    assert_eq!(unreadable.card.published_state().part_count, 0);
    let unreadable_before = unreadable.card.clone();
    for button in [PAD_CROSS, PAD_SQUARE, PAD_CIRCLE, PAD_START] {
        unreadable.tap(button);
        assert_eq!(
            unreadable.runtime.global_word(TITLE_STATE_GLOBAL),
            Ok(TitleScreen::Load.raw())
        );
        assert_eq!(
            unreadable.card, unreadable_before,
            "ordinary Load-screen inputs must not mutate an unreadable card"
        );
    }
    unreadable.tap(PAD_TRIANGLE);
    assert_eq!(
        unreadable.runtime.global_word(TITLE_STATE_GLOBAL),
        Ok(TitleScreen::MainMenu.raw()),
        "the authored UI must let the player leave an unreadable-card error"
    );
    assert_eq!(unreadable.card, unreadable_before);
}

#[test]
#[ignore = "set C1_STREAM_DIR or C1_DISC_IMAGE to legally local NTSC-U game data"]
fn authored_main_menu_map_to_n_sanity_handoff_preserves_session_carry() {
    let (nsd, nsf, nsf_bytes) = load_legally_local_title_pair();
    let mut title = AuthoredTitleHarness::main_menu(&nsd, &nsf, &nsf_bytes);

    title.wait_until_ready(32);
    assert_eq!(title.frame, 10, "authored MainMenu ready-frame drift");
    title.tap(PAD_CROSS);
    assert_eq!(
        title.runtime.global_word(TITLE_STATE_GLOBAL).unwrap(),
        TitleScreen::Map.raw(),
        "default MainMenu selection must request the world map"
    );
    let map_loaded_at = title
        .wait_for_loaded(TitleScreen::Map, 32)
        .expect("MainMenu Cross must reach the source TitleLoadState map boundary");
    assert_eq!(map_loaded_at, 20, "authored Map load-frame drift");
    title.wait_until_ready(32);
    let map_ready_at = title.frame;
    assert_eq!(map_ready_at, 30, "authored Map ready-frame drift");
    assert!(
        title.runtime.arena().main_object().is_some(),
        "the authored map controller must own the live main-object slot"
    );

    // CoreFrame consumes the request before another title GOOL frame. Do not
    // execute a synthetic release frame after the authored write.
    title.step(PAD_CROSS);
    assert_eq!(
        title.transitions,
        [(31, 0x09)],
        "the first unlocked map node must request N. Sanity Beach on Cross"
    );
    assert_eq!(
        title.island_level_updates.first(),
        Some(&(22, 7, -1, 1, 1, true)),
        "mode-seven state must be visible before its first cross-zone LevelUpdate"
    );
    assert!(
        title
            .island_level_updates
            .iter()
            .all(|&(_, mode, _, observed, state_after, _)| {
                mode == 7 && observed == state_after
            }),
        "every exercised mode-seven LevelUpdate must observe the prior writeback: {:?}",
        title.island_level_updates,
    );

    let report = {
        let mut host = TitleFlowHost::new(&nsd, &nsf, &nsf_bytes, &mut title.card);
        title
            .runtime
            .finish_level_transition(&mut host, 0x09)
            .expect("the map LEVEL_END phase must export a checked session carry")
    };
    assert!(
        report.event_failures.is_empty(),
        "map LEVEL_END handlers must complete cleanly: {:?}",
        report.event_failures
    );
    assert_eq!(report.requested_lid, 0x09);
    assert_eq!(report.next_lid_after_event, 0x09);
    assert_eq!(report.resolved.level, LevelId::N_SANITY_BEACH);
    assert!(!report.resolved.bonus_return);

    let title_draw_count = report.carry.draw_count;
    assert_eq!(title_draw_count, 31, "title/map draw-count drift");
    let mounted =
        RetailRuntime::new_from_session(RETAIL_GLOBAL_WORDS, LevelId::N_SANITY_BEACH, report.carry)
            .expect("N. Sanity must import the title/map session carry");
    assert_eq!(mounted.level(), Some(LevelId::N_SANITY_BEACH));
    assert_eq!(mounted.draw_count(), title_draw_count);
    assert_eq!(
        mounted.global_word(LEVELS_UNLOCKED_GLOBAL).unwrap(),
        1,
        "the first-map handoff must retain fresh-game progression"
    );
    eprintln!(
        "authored title route: MainMenu ready=10, Map loaded={map_loaded_at}, Map ready={map_ready_at}, N. Sanity request={}, carry draw_count={title_draw_count}",
        map_ready_at + 1,
    );
}

#[test]
#[ignore = "set C1_STREAM_DIR or C1_DISC_IMAGE to legally local NTSC-U game data"]
fn map_island_one_point_trailing_path_alias_is_characterized() {
    let (nsd, nsf, nsf_bytes) = load_legally_local_title_pair();
    let zone = Eid::from_name("1a_pZ").unwrap();
    let entry = nsf.resolve_entry(&nsd, zone).unwrap();
    let header = ZoneHeader::parse(entry.item(0).unwrap().bytes(&nsf_bytes).unwrap()).unwrap();
    let entry_range = entry.byte_range();
    let mut matches = 0;
    for entity_index in 0..header.entity_count {
        let item_index = usize::try_from(header.entity_item_index(entity_index).unwrap()).unwrap();
        let item = entry.item(item_index).unwrap();
        let entity = ZoneEntity::parse(item.bytes(&nsf_bytes).unwrap()).unwrap();
        if entity.executable != 59 || entity.path_points.len() != 1 {
            continue;
        }
        matches += 1;
        let item_range = item.byte_range();
        assert_eq!(entity.id, 1);
        assert_eq!(item_range.len(), 28);
        let point = entity.path_points.first().unwrap();
        assert_eq!([point.x, point.y, point.z], [99, 200, 200]);
        let alias_start =
            item_range.start + ZoneEntity::HEADER_BYTE_LEN + ZoneEntityPathPoint::BYTE_LEN;
        let alias_end = alias_start + ZoneEntityPathPoint::BYTE_LEN;
        assert!(alias_end <= entry_range.end);
        let next_range = entry.item(item_index + 1).unwrap().byte_range();
        assert_eq!(next_range.start, item_range.end);
        assert_eq!(alias_start + 2, next_range.start);
        assert_eq!(&nsf_bytes[alias_start..alias_end], &[0; 6]);
        eprintln!(
            "IsldC entity={entity_index} id={} item={item_range:?} len={} entry={entry_range:?} alias={:02x?} next={:?}",
            entity.id,
            item_range.len(),
            &nsf_bytes[alias_start..alias_end],
            Some(next_range),
        );
    }
    assert_eq!(
        matches, 1,
        "the retail title pair must contain exactly one IsldC one-point alias"
    );
}

fn default_title_save() -> SaveData {
    SaveData {
        level_count: 1,
        initial_lives: 4 << 8,
        sfx_volume: 255,
        music_volume: 255,
        ..SaveData::default()
    }
}

struct AuthoredTitleHarness<'assets> {
    nsd: &'assets Nsd,
    nsf: &'assets Nsf,
    nsf_bytes: &'assets [u8],
    graph: RetailZoneGraph,
    zones: BTreeMap<Eid, OwnedTitleZone>,
    lifecycle: ZoneLifecycle,
    camera: RetailCameraRuntime,
    runtime: RetailRuntime,
    card: VirtualCard,
    pad: PadState,
    frame: u32,
    loaded: Vec<(u32, TitleScreen)>,
    transitions: Vec<(u32, i32)>,
    card_requests: Vec<CardRequestTrace>,
    card_loads: Vec<(u32, SaveData)>,
    /// Frame, mode, state before, state at the `LevelUpdate` boundary, state
    /// after, and whether the effect crosses a zone boundary. A production
    /// cross-zone `LevelUpdate` exposes the boundary value to synchronous TERM.
    island_level_updates: Vec<(u32, u16, i32, i32, i32, bool)>,
}

impl<'assets> AuthoredTitleHarness<'assets> {
    fn main_menu(nsd: &'assets Nsd, nsf: &'assets Nsf, nsf_bytes: &'assets [u8]) -> Self {
        Self::main_menu_with_card(nsd, nsf, nsf_bytes, VirtualCard::new())
    }

    fn main_menu_with_card(
        nsd: &'assets Nsd,
        nsf: &'assets Nsf,
        nsf_bytes: &'assets [u8],
        card: VirtualCard,
    ) -> Self {
        let (graph, zones, lifecycle) = title_zone_catalog(nsd, nsf, nsf_bytes);
        let camera = RetailCameraRuntime::new(&graph).expect("title camera must initialize");
        let mut harness = Self {
            nsd,
            nsf,
            nsf_bytes,
            graph,
            zones,
            lifecycle,
            camera,
            runtime: RetailRuntime::new_for_level(RETAIL_GLOBAL_WORDS, LevelId::TITLE),
            card,
            pad: PadState::default(),
            frame: 0,
            loaded: Vec::new(),
            transitions: Vec::new(),
            card_requests: Vec::new(),
            card_loads: Vec::new(),
            island_level_updates: Vec::new(),
        };
        harness
            .runtime
            .restore_card_save_data(default_title_save())
            .unwrap();
        harness
            .runtime
            .configure_retail_title(TitleScreen::MainMenu, false)
            .unwrap();
        harness.mount(TitleScreen::MainMenu);
        harness
    }

    fn carried_game_over(nsd: &'assets Nsd, nsf: &'assets Nsf, nsf_bytes: &'assets [u8]) -> Self {
        let (graph, zones, lifecycle) = title_zone_catalog(nsd, nsf, nsf_bytes);
        let camera = RetailCameraRuntime::new(&graph).expect("title camera must initialize");
        let mut harness = Self {
            nsd,
            nsf,
            nsf_bytes,
            graph,
            zones,
            lifecycle,
            camera,
            runtime: RetailRuntime::new_for_level(RETAIL_GLOBAL_WORDS, LevelId::TITLE),
            card: VirtualCard::new(),
            pad: PadState::default(),
            frame: 0,
            loaded: Vec::new(),
            transitions: Vec::new(),
            card_requests: Vec::new(),
            card_loads: Vec::new(),
            island_level_updates: Vec::new(),
        };
        harness
            .runtime
            .restore_card_save_data(default_title_save())
            .unwrap();
        for (index, value) in [
            (GAME_STATE_GLOBAL, 0x200),
            (TITLE_STATE_GLOBAL, TitleScreen::GameOver.raw()),
            (SAVED_TITLE_STATE_GLOBAL, u32::MAX),
            (LIFE_COUNT_GLOBAL, 0),
        ] {
            harness.runtime.set_global_word(index, value).unwrap();
        }
        harness
            .runtime
            .configure_retail_title(TitleScreen::GameOver, false)
            .unwrap();
        harness.mount(TitleScreen::GameOver);
        harness
    }

    fn mount(&mut self, screen: TitleScreen) {
        let mut host = NsfProgramHost::new(self.nsd, self.nsf, self.nsf_bytes);
        let teardown = self.runtime.terminate_all_objects(&mut host).unwrap();
        assert!(
            teardown.event_failures.is_empty(),
            "title screen {screen:?} teardown handler mismatch: {:?}",
            teardown.event_failures
        );
        if screen == TitleScreen::MainMenu {
            self.runtime.reset_level_globals().unwrap();
            self.runtime
                .restore_resume_after_title_reset(default_title_save())
                .unwrap();
        }
        let (zone_name, display_mask, uses_image) = title_screen_fixture(screen);
        self.runtime
            .set_global_word(NEXT_DISPLAY_GLOBAL, display_mask)
            .unwrap();
        let zone = Eid::from_name(zone_name).unwrap();
        let path = RetailPathId { zone, index: 0 };
        self.lifecycle
            .transition_with_marker(zone, true)
            .unwrap_or_else(|error| panic!("title {screen:?} LevelUpdate: {error}"));
        let camera_step = self
            .camera
            .level_update(&self.graph, path, 0, 2)
            .unwrap_or_else(|error| panic!("title {screen:?} camera LevelUpdate: {error}"));
        assert_eq!(camera_step.after.path, path);
        let owned = self.zones.get(&zone).unwrap();
        self.runtime
            .set_level_state_context(RetailLevelStateContext {
                location: camera_step.after,
                graphics_flags: owned.header.graphics.flags,
                box_count: 0,
                checkpoint_id: -1,
                checkpoint_translation: [0; 3],
                first_spawn: false,
                active_neighbor_zones: self.lifecycle.active_neighbor_zones(),
            });
        if uses_image {
            let mdat = load_title_mdat(self.nsd, self.nsf, self.nsf_bytes, screen as u8)
                .unwrap_or_else(|error| panic!("title {screen:?} MDAT: {error}"));
            let unlocked = self
                .runtime
                .global_word(LEVELS_UNLOCKED_GLOBAL)
                .unwrap()
                .cast_signed();
            let eligible = mdat
                .entities
                .iter()
                .filter(|entity| {
                    entity
                        .path_points
                        .first()
                        .is_some_and(|point| i32::from(point.z) <= unlocked)
                })
                .cloned()
                .collect::<Vec<_>>();
            let neighbors = [NeighborZone {
                eid: zone,
                display_flags: ACTIVE_ZONE_DISPLAY_BIT,
                entities: &eligible,
            }];
            let mut host = TitleMdatHost::new(self.nsd, self.nsf, self.nsf_bytes, mdat.eid);
            let attempts = self
                .runtime
                .spawn_current_zone_neighbors(&neighbors, &mut host);
            assert!(
                attempts.iter().all(|attempt| attempt.result.is_ok()),
                "title {screen:?} MDAT spawn mismatch: {attempts:?}"
            );
        }
        self.loaded.push((self.frame, screen));
    }

    fn step(&mut self, held: u16) {
        self.frame += 1;
        self.runtime.set_frame_timing(34, 34);
        self.card.update();
        self.runtime
            .publish_card_state(self.card.published_state())
            .unwrap();
        let neighbors = self
            .lifecycle
            .next_frame_spawn_scan()
            .iter()
            .map(|candidate| {
                let zone = self.zones.get(&candidate.zone).unwrap();
                NeighborZone {
                    eid: zone.eid,
                    display_flags: candidate.display_flags,
                    entities: &zone.entities,
                }
            })
            .collect::<Vec<_>>();
        let attempts = {
            let mut host = TitleFlowHost::traced(
                self.nsd,
                self.nsf,
                self.nsf_bytes,
                &mut self.card,
                &mut self.card_requests,
            );
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
            "title frame {} spawn mismatch: {attempts:?}",
            self.frame
        );
        self.update_title_world_camera();
        let mut host = TitleFlowHost::traced(
            self.nsd,
            self.nsf,
            self.nsf_bytes,
            &mut self.card,
            &mut self.card_requests,
        );
        let pad = &mut self.pad;
        let report = self
            .runtime
            .run_frame_before_display_with_traversal_hook(
                &mut host,
                RETAIL_INSTRUCTION_BUDGET,
                |runtime, _host, _point| {
                    pad.update(held, 0, None);
                    let snapshot = pad.snapshot();
                    runtime
                        .set_pad_snapshot(
                            0,
                            RetailPadSnapshot {
                                tapped: snapshot.tapped,
                                held: snapshot.held,
                                held_previous: snapshot.held_previous,
                                tapped_previous: snapshot.tapped_previous,
                                held_previous_2: snapshot.held_previous_2,
                            },
                        )
                        .map_err(RuntimeError::Vm)
                },
            )
            .unwrap_or_else(|error| panic!("title frame {} runtime: {error:?}", self.frame));
        assert!(
            report
                .executions
                .iter()
                .all(|execution| execution.result.is_ok()),
            "title frame {} execution mismatch: {:?}",
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
        if let Some(save) = self.runtime.take_card_load() {
            self.card_loads.push((self.frame, save));
        }
        let action = self.runtime.begin_retail_title_update().unwrap();
        if let Some(RetailTitleAction::LoadScreen { screen, .. }) = action {
            self.mount(screen);
            self.runtime
                .continue_retail_title_update_after_load()
                .unwrap();
        }
        self.runtime.finish_retail_title_update().unwrap();
        self.runtime.finish_deferred_display_frame().unwrap();
    }

    fn update_title_world_camera(&mut self) {
        let Some(presentation) = self.runtime.retail_title_presentation().unwrap() else {
            return;
        };
        if !matches!(
            presentation.screen,
            TitleScreen::GameOver | TitleScreen::Map
        ) || self.runtime.arena().main_object().is_none()
        {
            return;
        }
        let snapshot = self.pad.snapshot();
        self.camera.synchronize_game_state(
            self.runtime
                .global_word(GAME_STATE_GLOBAL)
                .unwrap()
                .cast_signed(),
        );
        let island = (presentation.screen == TitleScreen::Map).then(|| RetailIslandCameraInput {
            island_cam_state: self
                .runtime
                .global_word(ISLAND_CAMERA_STATE_GLOBAL)
                .unwrap()
                .cast_signed(),
            island_cam_rot_x: self
                .runtime
                .global_word(ISLAND_CAMERA_ROTATION_GLOBAL)
                .unwrap()
                .cast_signed(),
        });
        let step = self
            .camera
            .update_with_island(
                &self.graph,
                RetailCameraInput {
                    tapped: snapshot.tapped,
                },
                island,
            )
            .expect("authored title-world camera update must execute");
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
            // Native mode seven updates `island_cam_state` before its optional
            // LevelUpdate, so departing TERM handlers observe the new value.
            self.runtime
                .set_global_word(ISLAND_CAMERA_STATE_GLOBAL, state_after.cast_unsigned())
                .unwrap();
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
            if let Some((mode, state_before, state_after)) = island_writeback {
                let observed = self
                    .runtime
                    .global_word(ISLAND_CAMERA_STATE_GLOBAL)
                    .unwrap()
                    .cast_signed();
                self.island_level_updates.push((
                    self.frame,
                    mode,
                    state_before,
                    observed,
                    state_after,
                    before.path.zone != after.path.zone,
                ));
            }
            if before.path.zone != after.path.zone {
                self.lifecycle
                    .transition_with_marker(after.path.zone, flags & 2 != 0)
                    .expect("world-map zone transition must remain valid");
            }
            let existing = self.runtime.level_state_context().unwrap().clone();
            let zone = self.zones.get(&after.path.zone).unwrap();
            self.runtime
                .set_level_state_context(RetailLevelStateContext {
                    location: after,
                    graphics_flags: zone.header.graphics.flags,
                    box_count: existing.box_count,
                    checkpoint_id: existing.checkpoint_id,
                    checkpoint_translation: existing.checkpoint_translation,
                    first_spawn: existing.first_spawn,
                    active_neighbor_zones: self.lifecycle.active_neighbor_zones(),
                });
        }
        if let Some((8, _, state_after)) = island_writeback {
            // Native mode eight performs LevelUpdate first and only then
            // resets `island_cam_state` when the destination leaves mode eight.
            self.runtime
                .set_global_word(ISLAND_CAMERA_STATE_GLOBAL, state_after.cast_unsigned())
                .unwrap();
        }
        let live_game_state = self
            .runtime
            .global_word(GAME_STATE_GLOBAL)
            .expect("title game-state global must remain readable")
            .cast_signed();
        self.camera.synchronize_game_state(live_game_state);
        self.runtime.latch_frame_context(
            live_game_state,
            self.camera.rotation_xz(&self.graph).unwrap(),
        );
    }

    fn wait_until_ready(&mut self, limit: u32) {
        for _ in 0..limit {
            if self
                .runtime
                .retail_title_presentation()
                .unwrap()
                .is_some_and(|title| title.phase == TitlePhase::Ready)
            {
                return;
            }
            self.step(0);
        }
        panic!(
            "title did not become ready by frame {}: {:?}",
            self.frame,
            self.runtime.retail_title_presentation()
        );
    }

    fn tap(&mut self, button: u16) {
        self.step(button);
        self.step(0);
    }

    fn wait_for_loaded(&mut self, screen: TitleScreen, limit: u32) -> Option<u32> {
        for _ in 0..limit {
            if self
                .loaded
                .last()
                .is_some_and(|(_, loaded)| *loaded == screen)
            {
                return self.loaded.last().map(|(frame, _)| *frame);
            }
            self.step(0);
        }
        None
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

            // MOVC arms the authored global transition routine in process.tp.
            // Its time gate opens on frame 64, where opcode 0x1a's two-frame
            // tapped mode must retain the coherent Start edge from frame 63.
            let mut transition_frame = None;
            for frame in 1..=64 {
                let start_pressed = frame == 63;
                runtime
                    .set_pad_snapshot(
                        0,
                        RetailPadSnapshot {
                            tapped: u32::from(start_pressed) * u32::from(PAD_START),
                            held: u32::from(start_pressed) * u32::from(PAD_START),
                            held_previous: u32::from(frame == 64) * u32::from(PAD_START),
                            tapped_previous: u32::from(frame == 64) * u32::from(PAD_START),
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
                "the authored state-ten controller must retain MOVC's transition pointer and consume the preceding Start edge through its two-frame gate"
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
