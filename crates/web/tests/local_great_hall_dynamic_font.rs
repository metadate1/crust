//! Opt-in Great Hall ending-font golden against legally local retail streams.
//!
//! The retail ZDATs do not contain a subtype-three `WinGC` entity. The ending
//! route creates it synchronously from authored GOOL, so this fixture uses a
//! minimal synthetic parent solely to reproduce that exact child request. The
//! child program, state transitions, animation data, and fonts all come from
//! the mounted `S2` pair.

use std::path::PathBuf;

use crust_formats::binary::{Eid, EntryRef};
use crust_formats::stream::{
    Entry, GoolAnimationDescriptor, LevelId, ObjectVertexKind, RetailPathId, RetailZoneGraph,
    StreamKind, StreamName, ZoneEntity, ZoneEntityPathPoint, ZoneHeader, load_gool_state_program,
    parse_gool_animation_descriptor, parse_nsd, parse_nsf,
};
use crust_sim::camera::{RetailCameraLocation, RetailCameraRuntime};
use crust_sim::gool::{
    GAME_STATE_GLOBAL, ModelVertexSource, RetailSolidEnvironment, RetailTransformVectorsCamera,
    VmObject, VmStateProgram,
};
use crust_sim::object_arena::NeighborZone;
use crust_sim::object_bounds::AnimationBoundSource;
use crust_sim::retail_runtime::{
    AnimationBoundBinding, ModelVertexBinding, NsfProgramError, NsfProgramHost, ProgramBinding,
    ProgramHost, ProgramOrigin, RetailLevelStateContext, RetailRenderObject, RetailRuntime,
    RetailZoneEnvironment, StateProgramBinding,
};

const RETAIL_GLOBAL_WORDS: usize = 256;
const RETAIL_INSTRUCTION_BUDGET: usize = 67;
const SYNTHETIC_PARENT_ID: u16 = 303;
const WIN_EXECUTABLE: u8 = 61;
const WIN_TEXT_SUBTYPE: u8 = 3;
const WIN_TEXT_STATE: u16 = 9;
const TEXT_TERM_INDEX: u32 = 101;
const TEXT_FRAME: u32 = TEXT_TERM_INDEX << 8;
const TEXT_DESCRIPTOR_OFFSET: usize = 0x820;
const DEFAULT_FONT_WORD_OFFSET: u32 = 0x84;
const OVERRIDE_FONT_WORD_OFFSET: u32 = 0x146;

// Three exact internal-to-stack moves, retail child creation, then return.
const SYNTHETIC_PARENT_CODE: [u32; 5] = [
    0x1100_0e1f,
    0x1100_1e1f,
    0x1100_2e1f,
    0x8a33_d0c1,
    0x8289_4000,
];
const SYNTHETIC_CHILD_ARGUMENTS: [u32; 3] = [TEXT_FRAME, 0xfffe_e800, 1];

fn entry_item<'a>(entry: &Entry, bytes: &'a [u8], index: usize) -> &'a [u8] {
    entry
        .item(index)
        .unwrap_or_else(|| panic!("entry {} has no item {index}", entry.eid))
        .bytes(bytes)
        .unwrap_or_else(|error| panic!("entry {} item {index}: {error}", entry.eid))
}

struct GreatHallFontHost<'a> {
    inner: NsfProgramHost<'a>,
}

impl<'a> GreatHallFontHost<'a> {
    const fn new(
        metadata: &'a crust_formats::stream::Nsd,
        nsf: &'a crust_formats::stream::Nsf,
        nsf_bytes: &'a [u8],
    ) -> Self {
        Self {
            inner: NsfProgramHost::new(metadata, nsf, nsf_bytes),
        }
    }
}

impl ProgramHost for GreatHallFontHost<'_> {
    type Error = NsfProgramError;

    fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
        if matches!(binding.origin, ProgramOrigin::Entity(entity) if entity.id == SYNTHETIC_PARENT_ID)
        {
            let mut object = VmObject::new(binding.object.vm(), SYNTHETIC_PARENT_CODE.to_vec())
                .map_err(NsfProgramError::Vm)?;
            for (index, value) in SYNTHETIC_CHILD_ARGUMENTS.into_iter().enumerate() {
                object
                    .set_internal(index, value)
                    .map_err(NsfProgramError::Vm)?;
            }
            return Ok(object);
        }
        if binding.executable == WIN_EXECUTABLE
            && binding.subtype == WIN_TEXT_SUBTYPE
            && let ProgramOrigin::RuntimeChild { arguments } = binding.origin
        {
            assert_eq!(arguments, SYNTHETIC_CHILD_ARGUMENTS);
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
}

fn synthetic_parent(path_y: i16) -> ZoneEntity {
    ZoneEntity {
        serialized_parent: EntryRef::from_raw(0),
        spawn_flags: 0,
        group: 3,
        id: SYNTHETIC_PARENT_ID,
        initializer: [0; 3],
        executable: WIN_EXECUTABLE,
        subtype: 4,
        path_points: vec![ZoneEntityPathPoint {
            x: 0,
            y: path_y,
            z: 0,
        }],
    }
}

fn level_context(
    graph: &RetailZoneGraph,
    location: RetailCameraLocation,
) -> RetailLevelStateContext {
    RetailLevelStateContext {
        location,
        graphics_flags: graph
            .zone(location.path.zone)
            .expect("camera zone must exist")
            .graphics_flags,
        box_count: 0,
        checkpoint_id: -1,
        checkpoint_translation: [0; 3],
        first_spawn: false,
        active_neighbor_zones: vec![location.path.zone],
    }
}

fn win_text_object<'a>(
    runtime: &RetailRuntime,
    objects: &'a [RetailRenderObject],
    win: Eid,
) -> Option<&'a RetailRenderObject> {
    objects.iter().find(|object| {
        object
            .program
            .is_some_and(|program| program.global_eid() == win)
            && runtime
                .machine()
                .object(object.object.vm())
                .is_ok_and(|vm| vm.state() == WIN_TEXT_STATE)
    })
}

#[test]
#[ignore = "set C1_STREAM_DIR to legally local extracted retail streams"]
fn great_hall_runtime_spawns_and_displays_authored_dynamic_font_text() {
    let root = PathBuf::from(
        std::env::var_os("C1_STREAM_DIR")
            .expect("C1_STREAM_DIR must name legally local extracted retail streams"),
    );
    let level = LevelId::new_const(0x2c);
    let nsd_path = root.join(StreamName::new(level, StreamKind::Nsd).filename());
    let nsf_path = root.join(StreamName::new(level, StreamKind::Nsf).filename());
    let nsd_bytes =
        std::fs::read(&nsd_path).unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
    let nsf_bytes =
        std::fs::read(&nsf_path).unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));
    let nsd = parse_nsd(&nsd_bytes, level).expect("invalid Great Hall NSD");
    let nsf = parse_nsf(&nsf_bytes, &nsd).expect("invalid Great Hall NSF");
    let ldat = nsd.ldat().expect("Great Hall is missing LDAT");
    let win = ldat.executable_map[usize::from(WIN_EXECUTABLE)];
    assert_eq!(win.name().as_deref(), Some("WinGC"));

    let state_nine = load_gool_state_program(&nsd, &nsf, &nsf_bytes, win, WIN_TEXT_STATE)
        .expect("WinGC state nine must load");
    let state_ten = load_gool_state_program(&nsd, &nsf, &nsf_bytes, win, 10)
        .expect("WinGC state ten must load");
    assert_eq!(state_nine.state().flags, 1);
    assert_eq!(state_nine.state().status_c, 0);
    assert_eq!(state_nine.code_pc(), Some(871));
    assert_eq!(state_nine.event_pc(), None);
    assert_eq!(state_nine.transition_pc(), Some(916));
    assert_eq!(
        [
            state_nine.code()[875],
            state_nine.code()[909],
            state_nine.code()[913],
        ],
        [0x1105_3e32, 0x2705_9e2a, 0x84ff_0b7d]
    );
    assert_eq!(
        &state_ten.code()[1516..1519],
        &[0x160c_7865, 0x16be_003e, 0x8a33_d0c1]
    );

    assert_eq!(
        state_nine.internal_words()[83],
        OVERRIDE_FONT_WORD_OFFSET << 8
    );
    assert_eq!(
        state_nine.internal_words()[89] >> 6,
        u32::try_from(TEXT_DESCRIPTOR_OFFSET).unwrap()
    );
    let GoolAnimationDescriptor::Text(text) =
        parse_gool_animation_descriptor(state_nine.animation_data(), TEXT_DESCRIPTOR_OFFSET)
            .expect("WinGC text descriptor must parse")
    else {
        panic!("WinGC offset 0x820 is not a text descriptor");
    };
    assert_eq!(text.font_word_offset, DEFAULT_FONT_WORD_OFFSET);
    assert_eq!(text.terms.len(), 167);
    assert_eq!(
        text.terms[usize::try_from(TEXT_TERM_INDEX).unwrap()],
        b"PAPU PAPU:"
    );

    let GoolAnimationDescriptor::Font(default_font) = parse_gool_animation_descriptor(
        state_nine.animation_data(),
        usize::try_from(DEFAULT_FONT_WORD_OFFSET).unwrap() * 4,
    )
    .expect("WinGC default font must parse") else {
        panic!("WinGC default font offset is not a font descriptor");
    };
    let GoolAnimationDescriptor::Font(override_font) = parse_gool_animation_descriptor(
        state_nine.animation_data(),
        usize::try_from(OVERRIDE_FONT_WORD_OFFSET).unwrap() * 4,
    )
    .expect("WinGC override font must parse") else {
        panic!("WinGC override font offset is not a font descriptor");
    };
    assert_eq!(default_font.texture_page.name().as_deref(), Some("Fon0T"));
    assert_eq!(override_font.texture_page.name().as_deref(), Some("Op2pT"));

    let graph = RetailZoneGraph::from_pair(&nsd, &nsf, &nsf_bytes)
        .expect("Great Hall zone graph must parse");
    let mut authored_win_entities = Vec::new();
    for node in graph.zones() {
        let entry = nsf
            .resolve_entry(&nsd, node.eid)
            .expect("Great Hall ZDAT must resolve");
        let header = ZoneHeader::parse(entry_item(entry, &nsf_bytes, 0))
            .expect("Great Hall ZDAT header must parse");
        for entity_index in 0..header.entity_count {
            let item_index = usize::try_from(
                header
                    .entity_item_index(entity_index)
                    .expect("entity item index must fit"),
            )
            .unwrap();
            let entity = ZoneEntity::parse(entry_item(entry, &nsf_bytes, item_index))
                .expect("Great Hall entity must parse");
            if entity.executable == WIN_EXECUTABLE {
                authored_win_entities.push((
                    node.eid.name(),
                    entity.id,
                    entity.subtype,
                    entity.spawn_flags,
                ));
            }
        }
    }
    assert_eq!(
        authored_win_entities,
        [
            (Some("a7_IZ".to_owned()), 28, 2, 24),
            (Some("a8_IZ".to_owned()), 29, 0, 24),
            (Some("x__IZ".to_owned()), 30, 1, 0),
        ]
    );
    assert!(
        authored_win_entities
            .iter()
            .all(|(_, _, subtype, _)| *subtype != WIN_TEXT_SUBTYPE),
        "subtype three is ending-route child data, not an idle ZDAT entity"
    );

    let boot_zone = Eid::from_name("a7_IZ").unwrap();
    let camera = RetailCameraRuntime::at_path(
        &graph,
        RetailPathId {
            zone: boot_zone,
            index: 0,
        },
        0,
        0x600,
    )
    .expect("Great Hall camera must initialize");
    let mut runtime = RetailRuntime::new_for_level(RETAIL_GLOBAL_WORDS, level);
    runtime
        .set_global_word(GAME_STATE_GLOBAL, 0x600)
        .expect("game-state global must exist");
    runtime.set_level_state_context(level_context(&graph, camera.location()));
    let pose = camera
        .pose(&graph)
        .expect("Great Hall camera pose must resolve");
    runtime.set_transform_vectors_camera(RetailTransformVectorsCamera::from_retail_pose(
        pose.translation,
        pose.rotation_yxz,
        500,
    ));

    // State nine adds argv[1] (-0x11800) with opposite sign to its parent's Y.
    // Put the synthetic parent at -0x11800 so the child lands at screen Y zero:
    // it clears INVISIBLE on update two without crossing the +0x8c00 state-link.
    let boot_origin_y = graph.zone(boot_zone).unwrap().origin[1];
    let parent_y_units = -0x11800_i32 / 0x100;
    let parent_path_delta = parent_y_units - boot_origin_y;
    assert_eq!(parent_path_delta % 4, 0);
    let parent_path_y = i16::try_from(parent_path_delta / 4).unwrap();
    let parent_entity = synthetic_parent(parent_path_y);
    let neighbors = [NeighborZone {
        eid: boot_zone,
        display_flags: 2,
        entities: std::slice::from_ref(&parent_entity),
    }];
    let mut host = GreatHallFontHost::new(&nsd, &nsf, &nsf_bytes);
    let attempts = runtime.spawn_current_zone_neighbors(&neighbors, &mut host);
    assert_eq!(attempts.len(), 1);
    let parent = *attempts[0]
        .result
        .as_ref()
        .expect("synthetic ending-route parent must bind");
    assert_eq!(
        runtime
            .render_objects()
            .unwrap()
            .iter()
            .find(|object| object.object == parent)
            .unwrap()
            .transform
            .translation[1],
        -0x11800
    );
    assert!(
        win_text_object(&runtime, &runtime.render_objects().unwrap(), win).is_none(),
        "the authentic child must not exist before its parent runs"
    );

    let creation_frame = runtime
        .run_frame(&mut host, RETAIL_INSTRUCTION_BUDGET)
        .expect("Great Hall dynamic-font creation frame must execute");
    assert_eq!(creation_frame.frame_index, 0);
    assert!(
        creation_frame
            .executions
            .iter()
            .all(|execution| execution.result.is_ok())
    );
    let creation_objects = runtime
        .render_objects()
        .expect("creation render objects must snapshot");
    let hidden = *win_text_object(&runtime, &creation_objects, win)
        .expect("WinGC state nine must select Papu Papu text in its creation frame");
    assert_eq!(hidden.status_b, 0x700);
    assert_eq!(hidden.animation_frame, TEXT_FRAME);
    assert_eq!(
        hidden.text_font_override_word_offset,
        OVERRIDE_FONT_WORD_OFFSET
    );
    assert!(!hidden.display_eligible);

    let visibility_frame = runtime
        .run_frame(&mut host, RETAIL_INSTRUCTION_BUDGET)
        .expect("Great Hall dynamic-font visibility frame must execute");
    assert_eq!(visibility_frame.frame_index, 1);
    assert!(
        visibility_frame
            .executions
            .iter()
            .all(|execution| execution.result.is_ok())
    );
    let visibility_objects = runtime
        .render_objects()
        .expect("visibility render objects must snapshot");
    let object = *win_text_object(&runtime, &visibility_objects, win)
        .expect("WinGC state nine must remain live after clearing invisible");
    let vm = runtime
        .machine()
        .object(object.object.vm())
        .expect("WinGC child VM object must remain live");
    assert_eq!(vm.state(), WIN_TEXT_STATE);
    assert_eq!(object.status_b, 0x600);
    assert_eq!(
        object
            .animation_reference
            .expect("WinGC state nine must select text")
            .offset(),
        u32::try_from(TEXT_DESCRIPTOR_OFFSET).unwrap()
    );
    assert_eq!(object.animation_frame, TEXT_FRAME);
    assert_eq!(
        object.text_font_override_word_offset,
        OVERRIDE_FONT_WORD_OFFSET
    );
    assert!(object.display_eligible);
}
