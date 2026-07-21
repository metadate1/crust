//! Safe coordination between retail zone spawns, the object forest, and GOOL.
//!
//! [`ObjectArena`] owns allocation, generations, persistent spawn flags, and
//! retail tree order. [`Machine`] owns executable GOOL state and uses a
//! separate compact handle space. This module is the only place that pairs the
//! two identities, so a stale arena generation can never be mistaken for a
//! live VM object.

use std::collections::{BTreeMap, BTreeSet};

use crust_formats::{
    binary::{Eid, FormatError, PageIndex},
    stream::{
        GoolAnimationDescriptor, LevelId, Nsd, Nsf, ObjectVertexKind, ZoneEntity,
        ZoneEntityPathPoint, ZoneHeader, ZoneRect, load_gool_program, load_gool_state_program,
        load_object_model_frame, parse_gool_animation_descriptor, parse_object_frame,
    },
};

use crate::{
    camera::{GOOL_FLAG_SPIN_ACCEL, RetailCameraLocation},
    card::{CardPublishedState, SaveData},
    flow::{TITLE_FADE_START, TITLE_FADE_STEP, TitlePhase, TitleScreen},
    gool::{
        AnimationLocalBoundRefresh, AnimationReference, AnimationSource, AudioHostRequest,
        AudioHostResponse, COLOR_COUNT, CURRENT_DISPLAY_GLOBAL, CURRENT_LEVEL_GLOBAL,
        CardHostRequest, CollisionObjectReference, EventDispatchOutcome, EventStateChange,
        Execution, GoolProgramIdentity, HaltReason, INITIAL_DISPLAY_MASK, MAX_OBJECTS, Machine,
        ModelVertexSource, NEXT_DISPLAY_GLOBAL, NearestObjectCandidate,
        ObjectHandle as VmObjectHandle, PagingHostOperation, PagingHostRequest, PagingHostResponse,
        ProcessAnimationKind, RETAIL_LEVEL_SPAWN_CAPACITY, RetailPadSnapshot,
        RetailSolidEnvironment, RetailSolidZone, RetailTransform, RetailTransformVectorsCamera,
        SendEventRequest, SendEventTarget, TITLE_STATE_GLOBAL, VmEffect, VmError, VmHostRequest,
        VmObject, VmStateProgram, process_register, retail_random,
    },
    math::{Angle12, Angles, Bounds3, Vec3},
    object_arena::{
        ENEMY_OBJECT_ROOT, EntitySpawnDescriptor, NeighborZone, OBJECT_POOL_CAPACITY, ObjectArena,
        ObjectHandle as ArenaObjectHandle, ROOT_HANDLE_COUNT, RootHandle, RuntimeCreateError,
        SPAWN_TABLE_CAPACITY, SpawnError, SpawnedObject, TreeError, TreeParent, ZONE_OBJECT_ROOT,
    },
    object_bounds::{
        AnimationBoundSource, BoundTransform, bounds_intersect_asymmetric, calculate_local_bound,
        calculate_world_bound, retail_yxy_transform,
    },
    paging::{PageInvalidations, Pager, PagerOpenOutcome, PagingError, TextureFrameSnapshot},
    retail_lighting::{
        ObjectDarkShaderInput, RetailObjectZoneShaderError, apply_retail_object_zone_shader,
    },
    retail_solid_motion::{
        HOG_LAND_OFFSET, STANDARD_LAND_OFFSET, SolidEffect, SolidEventReason, SolidEventTarget,
        SolidLevelQuirks, SolidObjectCandidate,
    },
};

/// A malformed transition graph must not monopolize the browser's
/// cooperative frame. Retail follows state links synchronously; this bound
/// preserves that ordering while reporting cycles as a typed VM failure.
const MAX_SYNCHRONOUS_STATE_CHANGES: usize = 64;
const COLLIDABLE_STATUS_B: u32 = 0x10;
const FIRST_FRAME_STATUS_A: u32 = 0x20;
const LOCAL_BOUND_INVALID_STATUS_A: u32 = 0x8000;
const LOCAL_BOUND_REFRESH_STATUS_B: u32 = 0x18;
const FORCE_LOCAL_BOUND_REFRESH_STATUS_B: u32 = 0x8000_0000;
const LATE_BOUND_RANGE: Vec3 = Vec3 {
    x: 0x7d000,
    y: 0xaf000,
    z: 0x7d000,
};
const STALL_STATUS_B: u32 = 0x1000_0000;
const BOX_EXECUTABLE: u8 = 0x22;
const BOX_OBJECT_TYPE: u32 = 0x22;
const BOX_STACK_SPACING: i32 = 0x19000;
const BOX_NEAR_TOLERANCE: u32 = 10;
const BOX_NO_STAGGER_GRAPHICS_FLAG: u32 = 4;
/// Native `cur_zone_flags_ro`, sampled by GOOL during level/bonus routing.
pub const CURRENT_ZONE_FLAGS_GLOBAL: usize = 30;
const PREVIOUS_BOX_GLOBAL: usize = 116;
const BOXES_Y_GLOBAL: usize = 117;
const PREVIOUS_BOX_ENTITY_GLOBAL: usize = 118;
const FORCE_UPDATE_STATUS_B: u32 = 0x0200_0000;
const MENU_TEXT_STATE_FLAG: u32 = 0x0002_0000;
const INVISIBLE_STATUS_B: u32 = 0x100;
const DISPLAY_OBJECTS: u32 = 0x4;
const ANIMATE_OBJECTS: u32 = 0x8;
const FORCE_DISPLAY_MENUS: u32 = 0x4000;
const FORCE_ANIMATE_MENUS: u32 = 0x8000;
const TERMINATE_EVENT: u32 = 0x1a00;
const RESPAWN_EVENT: u32 = 0x1300;
/// Native `GOOL_EVENT_LEVEL_END`, broadcast before every stream remount.
pub const LEVEL_END_EVENT: u32 = 0x2900;
const SAVE_RESTRICTED_ZONE_FLAG: u32 = 0x2000;
const SAVE_TRANSLATION_FROM_CALLER_STATUS_B: u32 = 0x200;
const SPAWN_ACTIVE_BIT: u32 = 1;
const SPAWN_CHECKPOINT_BLOCKED_BIT: u32 = 2;
const SPAWN_CHECKPOINT_SEEN_BIT: u32 = 8;
/// `LevelUpdate(..., flags = 1)` clears native spawn bits one and two.
const SPAWN_LEVEL_UPDATE_CLEAR_MASK: u32 = 0x6;
const ACTIVE_ZONE_DISPLAY_BIT: u32 = 2;
const SPAWNABLE_ENTITY_GROUP: u16 = 3;
const ZONE_TERMINATION_STATUS_B_IMMUNE: u32 = 0x0100_0000;
const ZONE_TERMINATION_STATE_IMMUNE: u32 = 0x0004_0000;
/// Native allocates `player` separately at initialization and every successful
/// `GoolObjectInit` stores that non-null address in process link five, even
/// while no logical main/Crash object occupies it.
const DEDICATED_PLAYER_POOL_SLOT: u8 = OBJECT_POOL_CAPACITY as u8;

// `gool_globals` words whose C values are native pointers. A stream remount
// destroys every pointee. Retaining compact Rust handles here could alias a
// newly allocated object, so these words are deliberately cleared rather than
// reproducing that undefined dangling-pointer behavior.
const POINTER_GLOBALS: [usize; 13] = [
    6,   // fruit_hud
    7,   // life_hud
    8,   // ambiance_obj
    12,  // pause_obj
    14,  // pickup_hud
    16,  // doctor
    36,  // cam_spin_obj
    54,  // light_src_obj
    76,  // caption_obj
    80,  // card_str
    81,  // card_icon
    116, // prev_box
    118, // prev_box_entity
];
const RESPAWN_COUNT_GLOBAL: usize = 5;
const SCREEN_SHAKE_GLOBAL: usize = 2;
const AMBIANCE_OBJECT_GLOBAL: usize = 8;
const CORTEX_COUNT_GLOBAL: usize = 27;
const BRIO_COUNT_GLOBAL: usize = 28;
const TAWNA_COUNT_GLOBAL: usize = 29;
const GAME_STATE_GLOBAL: usize = 17;
const CHECKPOINT_ID_GLOBAL: usize = 69;
const CHECKPOINT_TRANSLATION_GLOBALS: [usize; 3] = [102, 103, 104];
const DEATH_COUNT_GLOBAL: usize = 108;
const BOX_COUNT_GLOBAL: usize = 62;
const BONUS_ROUND_GLOBAL: usize = 60;
const PAUSE_OBJECT_GLOBAL: usize = 12;
const DOCTOR_OBJECT_GLOBAL: usize = 16;
const LIGHT_SOURCE_OBJECT_GLOBAL: usize = 54;
pub const ISLAND_CAMERA_ROTATION_GLOBAL: usize = 64;
const GEM_STAMP_GLOBAL: usize = 65;
pub const ISLAND_CAMERA_STATE_GLOBAL: usize = 66;
const IS_FIRST_ZONE_GLOBAL: usize = 67;
const TITLE_PAUSE_STATE_GLOBAL: usize = 74;
const CAPTION_OBJECT_GLOBAL: usize = 76;
const PBAK_STATE_GLOBAL: usize = 105;
const FADE_COUNTER_GLOBAL: usize = 106;
const FADE_STEP_GLOBAL: usize = 107;
const SPIN_DEATH_CAMERA_COUNT_GLOBAL: usize = 10;
const SPIN_DEATH_CAMERA_OBJECT_GLOBAL: usize = 36;
const SPIN_DEATH_CAMERA_VERTEX_GLOBAL: usize = 49;
const SPIN_DEATH_CAMERA_ZOOM_SPEED_GLOBAL: usize = 56;
const SPIN_DEATH_CAMERA_FLIP_SPEED_GLOBAL: usize = 57;
const PBAK_CAPTION_EVENT: u32 = 0x0e00;
const PBAK_CAPTION_EXECUTABLE: u8 = 4;
const PBAK_CAPTION_SUBTYPE: u8 = 8;
const PBAK_CAPTION_ARGUMENTS: [u32; 2] = [2_279, 19_993];
const LEVEL_MISC_CONTROLLER_ROOT: u8 = 4;
const PAUSE_RESUME_EVENT: u32 = 0x0c00;
const PAUSE_CONTROLLER_EXECUTABLE: u8 = 4;
const PAUSE_CONTROLLER_SUBTYPE: u8 = 4;
const PAUSE_CONTROLLER_ROOT: u8 = 7;

fn solid_level_quirks(level: LevelId) -> SolidLevelQuirks {
    let level = level.get();
    SolidLevelQuirks {
        land_offset: if matches!(level, 0x11 | 0x1e) {
            HOG_LAND_OFFSET
        } else {
            STANDARD_LAND_OFFSET
        },
        type_four_pits_drown: matches!(level, 0x03 | 0x07),
        drown_when_below_zone: level == 0x17,
        lethal_river_water: matches!(level, 0x0f | 0x18),
    }
}

fn retail_object_shader_depth_anchor(level: LevelId, visibility_depth: u32, fog_shift: u32) -> i32 {
    let visibility = i32::try_from(visibility_depth >> 8).unwrap_or(i32::MAX);
    if matches!(level.get(), 0x14 | 0x16) {
        // `fog_z` starts at zero in native LevelInit for both bridge levels.
        visibility.wrapping_add(400)
    } else {
        visibility.wrapping_sub(if fog_shift == 0 { 0 } else { 1_200 })
    }
}

const fn retail_animation_mask_enabled(
    display_mask: u32,
    status_b: u32,
    state_flags: u32,
    category: Option<u32>,
) -> bool {
    if display_mask & ANIMATE_OBJECTS == 0 {
        return false;
    }
    if (status_b & FORCE_UPDATE_STATUS_B != 0 || state_flags & MENU_TEXT_STATE_FLAG != 0)
        && display_mask & FORCE_ANIMATE_MENUS != 0
    {
        return true;
    }
    let Some(category) = category else {
        // Synthetic test/host objects have no retail category contract.
        return true;
    };
    let category_mask = match category {
        0x100 => 0x20,
        0x300 | 0x500 | 0x600 => 0x80,
        0x400 => 0x400,
        0x200 => 0x100,
        _ => 0,
    };
    display_mask & category_mask != 0
}

/// Applies native `GoolObjectUpdate(obj, !paused)` ordering to the animation
/// mask decision. The authored pause/options controllers are the only objects
/// allowed to execute while the host update flag is clear. The override lives
/// inside category two's ordinary branch, so force-menu animation does not
/// accidentally inherit it.
const fn retail_animation_update_enabled(
    display_mask: u32,
    status_b: u32,
    state_flags: u32,
    category: Option<u32>,
    object_type: Option<u32>,
    subtype: u32,
    paused: bool,
) -> bool {
    if !paused {
        return retail_animation_mask_enabled(display_mask, status_b, state_flags, category);
    }
    if display_mask & ANIMATE_OBJECTS == 0 {
        return false;
    }
    if (status_b & FORCE_UPDATE_STATUS_B != 0 || state_flags & MENU_TEXT_STATE_FLAG != 0)
        && display_mask & FORCE_ANIMATE_MENUS != 0
    {
        return false;
    }
    let Some(category) = category else {
        // Synthetic host objects have no native header contract. Preserve
        // their historical unpaused behavior but never grant a pause override.
        return false;
    };
    let category_enabled = match category {
        0x100 => display_mask & 0x20 != 0,
        0x300 | 0x500 | 0x600 => display_mask & 0x80 != 0,
        0x400 => display_mask & 0x400 != 0,
        0x200 => display_mask & 0x100 != 0,
        _ => false,
    };
    if !category_enabled {
        return false;
    }
    category == 0x200 && matches!(object_type, Some(4)) && matches!(subtype, 4 | 7)
}

const fn retail_display_mask_enabled(
    display_mask: u32,
    status_b: u32,
    state_flags: u32,
    category: Option<u32>,
    has_animation: bool,
) -> bool {
    if !has_animation || status_b & INVISIBLE_STATUS_B != 0 || display_mask & DISPLAY_OBJECTS == 0 {
        return false;
    }
    if (status_b & FORCE_UPDATE_STATUS_B != 0 || state_flags & MENU_TEXT_STATE_FLAG != 0)
        && display_mask & FORCE_DISPLAY_MENUS != 0
    {
        return true;
    }
    let Some(category) = category else {
        return true;
    };
    let category_mask = match category {
        0x100 => 0x10,
        0x300 | 0x500 | 0x600 => 0x40,
        0x400 => 0x800,
        0x200 => 0x200,
        _ => 0,
    };
    display_mask & category_mask != 0
}

/// One live object identity at the arena/VM boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RuntimeObjectHandle {
    arena: ArenaObjectHandle,
    vm: VmObjectHandle,
}

/// Process-lifetime HUD controllers created by native `CoreObjectsCreate`.
///
/// The three objects live beneath logical root one. Their handles are also
/// published through the exact GOOL pointer globals consumed by authored
/// display, pickup, save, and bonus code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailCoreObjects {
    pub life: RuntimeObjectHandle,
    pub fruit: RuntimeObjectHandle,
    pub pickup: RuntimeObjectHandle,
}

/// Host-owned native pause handshake retained across cooperative frames.
///
/// `controller` is the executable-four/subtype-four object beneath logical
/// root seven. `saved_frame_index` is the pointer-free equivalent of native
/// `pause_draw_stamp`: pause frames advance so the authored menu can animate,
/// then resume rewinds ordinary GOOL waits to the pre-pause timestamp.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetailPauseState {
    paused: bool,
    status: i32,
    controller: Option<RuntimeObjectHandle>,
    saved_frame_index: Option<u64>,
}

impl RetailPauseState {
    #[must_use]
    pub const fn paused(self) -> bool {
        self.paused
    }

    /// Native `pause_status`: one on creation, minus one on resume/screen
    /// teardown, and zero on an ordinary frame without a successful toggle.
    #[must_use]
    pub const fn status(self) -> i32 {
        self.status
    }

    #[must_use]
    pub const fn controller(self) -> Option<RuntimeObjectHandle> {
        self.controller
    }
}

/// Result of one source-ordered `CoreFrame` pause-input check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetailPauseUpdate {
    /// START was not tapped; `pause_status` was reset to zero.
    Unchanged,
    /// START was tapped while the level/title/PBAK gate rejected pausing.
    Blocked,
    /// The controller could not be allocated or materialized. Native treats
    /// this as a failed toggle, clears its pause globals, and keeps running.
    Failed,
    /// The authored root-seven controller was created successfully.
    Paused { controller: RuntimeObjectHandle },
    /// Resume event delivery completed or faulted as native permits. The
    /// controller remains live until its state-seven return later this frame.
    Resumed {
        controller: Option<RuntimeObjectHandle>,
        event_faulted: bool,
    },
}

/// One resume event whose checked handler faulted. Native ignores this return
/// value, clears the pause latch, and continues the same frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimePauseEventFault {
    pub object: RuntimeObjectHandle,
}

impl RuntimeObjectHandle {
    /// Generational identity used by the allocation forest.
    #[must_use]
    pub const fn arena(self) -> ArenaObjectHandle {
        self.arena
    }

    /// Compact identity used by GOOL operands and links.
    #[must_use]
    pub const fn vm(self) -> VmObjectHandle {
        self.vm
    }
}

/// Immutable, pointer-free render state captured from one live arena/VM pair.
///
/// `program` is present for objects materialized from a parsed retail
/// [`crust_formats::stream::GoolProgram`]. Authored objects created directly
/// with [`VmObject::new`] retain `None`; their process state is still exposed
/// for deterministic tests and non-retail hosts. Every remaining render field
/// is copied at native's post-update/pre-child display boundary. A descendant
/// may subsequently mutate its parent through a linked register without
/// retroactively changing the parent's already-consumed render state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetailRenderObject {
    pub object: RuntimeObjectHandle,
    pub zone: Eid,
    pub executable: u8,
    pub subtype: u8,
    pub program: Option<GoolProgramIdentity>,
    /// Authoritative checked replacement for native `process.anim_seq`.
    /// Process-local type-zero descriptors are present here even though they
    /// deliberately have no item-five offset and draw no primitives.
    pub animation_source: Option<AnimationSource>,
    /// Compatibility item-five view used by asset renderers. This is `None`
    /// for a valid process-local animation; presence decisions must use
    /// [`Self::animation_source`].
    pub animation_reference: Option<AnimationReference>,
    pub animation_frame: u32,
    pub transform: RetailTransform,
    pub status_a: u32,
    pub status_b: u32,
    pub status_c: u32,
    pub state_flags: u32,
    pub size: i32,
    pub colors: [u16; COLOR_COUNT],
    /// Type-four text's dynamic font word offset (`invincibility_state >> 8`).
    /// Zero selects the font reference serialized in the text descriptor.
    pub text_font_override_word_offset: u32,
    /// Exact bounded aliases for native `sp[-2]` through `sp[-11]`.
    ///
    /// `snprintf` consumes the first four values, while retail's `~pN`
    /// pluralization command can select any decimal `N` in `0..=9`. Missing
    /// words stay explicit instead of reproducing an out-of-bounds C read.
    pub text_arguments: [Option<u32>; 10],
    /// Live source reference for ZDAT object-shader mode four. Native uses the
    /// pause object while it exists and the dedicated player otherwise.
    pub dark_reference_translation: Option<[i32; 3]>,
    /// Source `dark_dist` at this display boundary, after the frame shader
    /// step and any mode-four clamp performed for this object.
    pub dark_distance: i32,
    /// Live global-nine display mask sampled at this object's exact
    /// post-update transform boundary.
    ///
    /// This is deliberately independent from the pre-GOOL mask consumed by
    /// world geometry: an earlier object may write global nine before this
    /// object is displayed in the same preorder traversal.
    pub display_mask: u32,
    /// Live texture-slot identities at this object's exact display boundary.
    /// Browser hosts provide this after every synchronous GOOL paging opcode;
    /// authored hosts without a platform pager retain `None`.
    pub texture_frame_snapshot: Option<TextureFrameSnapshot>,
    /// Exact per-object display decision captured after this object's update.
    pub display_eligible: bool,
}

fn retail_text_arguments(stack: &[u32]) -> [Option<u32>; 10] {
    std::array::from_fn(|index| {
        stack
            .len()
            .checked_sub(index + 2)
            .and_then(|stack_index| stack.get(stack_index))
            .copied()
    })
}

/// Checked failures while taking an immutable render-object snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderObjectsError {
    InvalidRootIndex(usize),
    Tree(TreeError),
    /// A live tree node has no arena-to-VM binding.
    UnboundArenaObject(ArenaObjectHandle),
    /// Either direction of the arena/VM map, the arena generation, or the VM
    /// object no longer agrees with this identity pair.
    StaleObjectPair(RuntimeObjectHandle),
    Vm(VmError),
}

/// Why a program is being materialized for one arena object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgramOrigin<'a> {
    /// A persistent descriptor parsed from a ZDAT entity item.
    Entity(&'a ZoneEntity),
    /// A synchronous `0x8a`/`0x91` child request from GOOL.
    RuntimeChild { arguments: &'a [u32] },
}

/// Fully typed input to a program loader/binder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProgramBinding<'a> {
    pub object: RuntimeObjectHandle,
    pub zone: Eid,
    pub executable: u8,
    pub subtype: u8,
    pub origin: ProgramOrigin<'a>,
}

/// `GoolObjectInit` clears `obj->zone` for these process-lifetime programs.
/// `GoolObjectSpawn` subsequently restores an entity's ZDAT owner, so the
/// null-zone rule applies only to objects created by `GoolObjectCreate`.
const fn runtime_program_clears_object_zone(binding: ProgramBinding<'_>) -> bool {
    matches!(binding.origin, ProgramOrigin::RuntimeChild { .. })
        && matches!(binding.executable, 4 | 5 | 29)
}

/// Fully typed request to materialize code/data for a changed GOOL state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StateProgramBinding {
    pub object: RuntimeObjectHandle,
    pub zone: Eid,
    pub executable: u8,
    pub state: u16,
}

/// Typed request for one live object's current animation-derived bound source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnimationBoundReference {
    /// Descriptor selected from the object's global item five.
    ItemFive(AnimationReference),
    /// Type-one descriptor read through a checked LEA-created storage alias.
    Model(Eid),
}

/// Typed request for one live object's current animation-derived bound source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnimationBoundBinding {
    pub object: RuntimeObjectHandle,
    pub zone: Eid,
    pub executable: u8,
    pub reference: AnimationBoundReference,
    /// Integer frame selected by the process's 24.8 animation counter.
    pub frame_index: u32,
}

fn animation_vertex_reference(source: &AnimationSource) -> Option<AnimationBoundReference> {
    match source {
        AnimationSource::ItemFive(reference) => Some(AnimationBoundReference::ItemFive(*reference)),
        AnimationSource::Process(reference) => match reference.kind() {
            ProcessAnimationKind::Vertex(vertex) => {
                Some(AnimationBoundReference::Model(vertex.model_eid))
            }
            ProcessAnimationKind::NoDraw
            | ProcessAnimationKind::Sprite(_)
            | ProcessAnimationKind::Font(_)
            | ProcessAnimationKind::Text(_)
            | ProcessAnimationKind::Fragment(_) => None,
        },
    }
}

/// Typed asset request emitted by transform-vectors suboperation six.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelVertexBinding {
    pub requester: RuntimeObjectHandle,
    pub link: RuntimeObjectHandle,
    pub model_eid: Eid,
    pub frame_index: u32,
    pub vertex_index: u32,
}

/// Fully resolved inputs consumed by retail's spinning death camera.
///
/// The focus vertex is copied into world space while the referenced object,
/// animation frame, and mounted model are all live. No pair-backed bytes or
/// compact object token escape this boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpinDeathCameraInputs {
    /// Live signed global ten, advanced by the camera core for its first nine
    /// alignment iterations.
    pub count: i32,
    pub focus: Vec3,
    pub zoom_speed: i32,
    pub flip_speed: i32,
    /// Native current-display `GOOL_FLAG_SPIN_ACCEL` (`0x40000`).
    pub spin_accel: bool,
    /// Current rounded cooperative timing used by `GoolObjectRotate`.
    pub ticks_per_frame: u32,
}

/// Checked failures while resolving the authored spinning-death focus.
#[derive(Debug, Eq, PartialEq)]
pub enum SpinDeathCameraResolveError<E> {
    Vm(VmError),
    Program(E),
    NullObjectReference,
    InvalidObjectReference(u32),
    /// The compact VM token has no complete live arena/VM generation pair.
    StaleObjectReference(CollisionObjectReference),
    MissingAnimation(RuntimeObjectHandle),
    NonVertexAnimation(RuntimeObjectHandle),
    MissingFrame {
        object: RuntimeObjectHandle,
        model_eid: Eid,
        frame_index: u32,
    },
    VertexIndexOutOfRange {
        object: RuntimeObjectHandle,
        model_eid: Eid,
        frame_index: u32,
        /// Signed result of native's `cam_spin_obj_vert >> 8` operation.
        vertex_index: i32,
    },
}

/// Synchronous result of one browser/platform memory-card operation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CardHostResponse {
    /// Native `CardControl` result written to process register 37.
    pub result: i32,
    /// Present only after a successful load operation.
    pub loaded: Option<SaveData>,
    /// Card metadata visible to GOOL immediately after the call.
    pub published: CardPublishedState,
}

/// Immutable zone inputs needed to reproduce `GoolObjectSpawn` without
/// retaining native pointers into a ZDAT entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailZoneEnvironment {
    pub origin: [i32; 3],
    pub object_colors: [u16; COLOR_COUNT],
    pub player_colors: [u16; COLOR_COUNT],
    /// Native ZDAT graphics flags, including bit two's crate-stagger bypass.
    pub graphics_flags: u32,
}

/// Returns whether one native Q24.8 point lies inside a serialized ZDAT
/// rectangle. The PSX additions and shifts are 32-bit wrapping operations;
/// both faces are inclusive in the source `TestPointInRect` predicate.
fn retail_zone_rect_contains(rect: ZoneRect, point: [i32; 3]) -> bool {
    rect.origin.into_iter().zip(rect.dimensions).zip(point).all(
        |((origin, dimension), coordinate)| {
            let lower = origin.wrapping_shl(8);
            let upper = origin.wrapping_add(dimension.cast_signed()).wrapping_shl(8);
            coordinate >= lower && coordinate <= upper
        },
    )
}

fn retail_box_points_are_adjacent(
    current: ZoneEntityPathPoint,
    previous: ZoneEntityPathPoint,
) -> bool {
    i32::from(current.x).abs_diff(i32::from(previous.x)) < BOX_NEAR_TOLERANCE
        && i32::from(current.z).abs_diff(i32::from(previous.z)) < BOX_NEAR_TOLERANCE
        && i32::from(current.y).abs_diff(i32::from(previous.y) + 100) < BOX_NEAR_TOLERANCE
}

fn retail_box_stagger_count(translation: [i32; 3]) -> u32 {
    (((translation[2] >> 4) ^ translation[0]) & 7) as u32
}

/// Mirrors `ZoneFindNeighbor`'s reverse serialized-header scan. Rectangle
/// resolution is deliberately lazy: native returns after its first match and
/// never dereferences any earlier serialized neighbor.
fn find_retail_neighbor_zone<E, F>(
    neighbors: &[Eid],
    point: [i32; 3],
    mut resolve_rect: F,
) -> Result<Option<Eid>, E>
where
    F: FnMut(Eid) -> Result<ZoneRect, E>,
{
    for zone in neighbors.iter().rev().copied() {
        if retail_zone_rect_contains(resolve_rect(zone)?, point) {
            return Ok(Some(zone));
        }
    }
    Ok(None)
}

/// Supplies the initial GOOL object for an entity or runtime child.
///
/// The returned object's handle must equal `binding.object.vm()`. Keeping the
/// constructor on this boundary lets a browser asset host page entries before
/// binding, while deterministic tests can provide small authored programs.
pub trait ProgramHost {
    type Error;

    fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error>;

    /// Mirrors `GoolObjectInit`'s physical `NSOpen(global, count = 0)` after a
    /// checked program bind. Stream-only hosts need no separate allocator;
    /// browser hosts return the resolved program page and every PTE displaced
    /// while acquiring its ordinary physical slot.
    fn materialize_program_page(
        &mut self,
        _binding: ProgramBinding<'_>,
    ) -> Result<Option<PagerOpenOutcome>, Self::Error> {
        Ok(None)
    }

    fn bind_state_program(
        &mut self,
        binding: StateProgramBinding,
    ) -> Result<VmStateProgram, Self::Error>;

    /// Optionally supplies parsed ZDAT inputs for process initialization.
    /// Existing authored/test hosts remain source-compatible and may leave
    /// colors at their program-bound values while still receiving the common
    /// process and transform defaults.
    fn zone_environment(
        &mut self,
        _zone: Eid,
    ) -> Result<Option<RetailZoneEnvironment>, Self::Error> {
        Ok(None)
    }

    /// Optionally owns the serialized octree neighborhoods needed by active
    /// solid-surface GOOL queries. Authored hosts that never arm the retail
    /// collision gate need not provide one.
    fn solid_environment(
        &mut self,
        _zone: Eid,
    ) -> Result<Option<RetailSolidEnvironment>, Self::Error> {
        Ok(None)
    }

    /// Resolves GOOL `SZON` against the current ZDAT header. The caller owns
    /// the current-zone identity, while the stream host owns and validates the
    /// serialized neighbor EIDs and rectangles. `None` means no neighbor
    /// contains the point and therefore leaves the target object's zone
    /// unchanged.
    fn find_neighbor_zone(
        &mut self,
        _current_zone: Eid,
        _point: [i32; 3],
    ) -> Result<Option<Eid>, Self::Error> {
        Ok(None)
    }

    /// Returns the current ZDAT header's neighbor EIDs in exact serialized
    /// forward order. Misc 12/7 intentionally does not filter display flags,
    /// sort, or deduplicate this list.
    fn current_zone_neighbors(&mut self, _current_zone: Eid) -> Result<Vec<Eid>, Self::Error> {
        Ok(Vec::new())
    }

    /// Resolves the current item-five animation/frame for persistent local-
    /// bound refresh or frame-bound registration. Frame registration uses the
    /// collidable gate. Opcode `0x83` requests a local refresh when
    /// `status_b & 0x18` passes its range/force rules; `0x84` requests one
    /// unconditionally when an animation reference exists. Authored hosts may
    /// omit the callback; no synthetic bound is invented.
    fn animation_bound_source(
        &mut self,
        _binding: AnimationBoundBinding,
    ) -> Result<Option<AnimationBoundSource>, Self::Error> {
        Ok(None)
    }

    /// Validates that the current item-five animation resolves to an
    /// available SVTX/CVTX frame and returns its vertex kind for native's
    /// display-time side effects. This is deliberately separate from
    /// [`Self::animation_bound_source`]: display must not manufacture a
    /// collision-bound callback or perturb its source-ordered scheduling.
    fn animation_display_vertex_kind(
        &mut self,
        _binding: AnimationBoundBinding,
    ) -> Result<Option<ObjectVertexKind>, Self::Error> {
        Ok(None)
    }

    /// Resolves one packed SVTX/CVTX vertex and its TGEO scale without
    /// retaining pointers into mounted stream bytes. Authored hosts may return
    /// `None`; the VM then preserves native's no-animation/no-model no-op.
    fn model_vertex_source(
        &mut self,
        _binding: ModelVertexBinding,
    ) -> Result<Option<ModelVertexSource>, Self::Error> {
        Ok(None)
    }

    /// Completes an exact synchronous retail audio opcode before the next
    /// GOOL instruction executes. Asset-only hosts use the native failure
    /// value for voice creation while still acknowledging control calls; a
    /// browser host can override this boundary with the retail audio engine.
    fn handle_audio_request(
        &mut self,
        request: AudioHostRequest,
    ) -> Result<AudioHostResponse, Self::Error> {
        Ok(match request {
            AudioHostRequest::CreateVoice(_) => AudioHostResponse::VoiceCreated { voice_id: -2 },
            AudioHostRequest::Control(_) => AudioHostResponse::ControlApplied,
        })
    }

    /// Applies one GOOL `NSOpen`/`NSClose` operation at the exact opcode
    /// boundary. Asset-only hosts keep the VM's deterministic paging model;
    /// browser hosts additionally mutate their mounted stream pager here.
    fn handle_paging_request(
        &mut self,
        _request: PagingHostRequest,
    ) -> Result<PagingHostResponse, Self::Error> {
        Ok(PagingHostResponse::Applied {
            invalidated: PageInvalidations::NONE,
        })
    }

    /// Returns the platform pager's live eight-slot mapping at the current
    /// source display boundary. The immutable copy carries no stream bytes or
    /// host pointers and may safely outlive later same-frame replacements.
    fn texture_frame_snapshot(&self) -> Option<TextureFrameSnapshot> {
        None
    }

    /// Completes misc primary fifteen synchronously. Asset-only hosts reject
    /// the operation while preserving an empty, deterministic card view.
    fn handle_card_request(
        &mut self,
        _request: CardHostRequest,
        _current: SaveData,
    ) -> Result<CardHostResponse, Self::Error> {
        Ok(CardHostResponse {
            result: 1,
            ..CardHostResponse::default()
        })
    }

    /// Releases platform-owned voices before a reclaimed VM handle can be
    /// rebound to a replacement object in the same cooperative frame.
    ///
    /// Return `true` when cleanup was completed synchronously. Hosts that do
    /// not own audio can retain the default; the runtime then exposes an
    /// ordered [`RuntimeCleanupAction`] through [`RetailRuntime::take_cleanup_actions`].
    fn free_object_audio(&mut self, _object: RuntimeObjectHandle) -> bool {
        false
    }
}

/// A direct host over one already-validated NSD/NSF stream pair.
#[derive(Debug)]
pub struct NsfProgramHost<'a> {
    metadata: &'a Nsd,
    nsf: &'a Nsf,
    nsf_bytes: &'a [u8],
}

impl<'a> NsfProgramHost<'a> {
    #[must_use]
    pub const fn new(metadata: &'a Nsd, nsf: &'a Nsf, nsf_bytes: &'a [u8]) -> Self {
        Self {
            metadata,
            nsf,
            nsf_bytes,
        }
    }
}

/// Exact failure from the stream-backed program host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NsfProgramError {
    MissingLdat,
    ExecutableOutsideLdat(u8),
    MissingExecutable {
        executable: u8,
        eid: Eid,
    },
    Format(FormatError),
    Paging(PagingError),
    PagingPageMismatch {
        requested: PageIndex,
        resolved: PageIndex,
    },
    Vm(VmError),
}

impl ProgramHost for NsfProgramHost<'_> {
    type Error = NsfProgramError;

    fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
        let global_eid = self.global_eid(binding.executable)?;
        let program = load_gool_program(
            self.metadata,
            self.nsf,
            self.nsf_bytes,
            global_eid,
            u16::from(binding.subtype),
        )
        .map_err(NsfProgramError::Format)?;
        let mut object = VmObject::from_gool_program(binding.object.vm(), &program)
            .map_err(NsfProgramError::Vm)?;
        if let ProgramOrigin::RuntimeChild { arguments } = binding.origin {
            object
                .initialize_arguments(arguments)
                .map_err(NsfProgramError::Vm)?;
        }
        Ok(object)
    }

    fn bind_state_program(
        &mut self,
        binding: StateProgramBinding,
    ) -> Result<VmStateProgram, Self::Error> {
        let global_eid = self.global_eid(binding.executable)?;
        let program = load_gool_state_program(
            self.metadata,
            self.nsf,
            self.nsf_bytes,
            global_eid,
            binding.state,
        )
        .map_err(NsfProgramError::Format)?;
        VmStateProgram::new(
            program.state_index(),
            program.state(),
            program.code().to_vec(),
            program.external_words().to_vec(),
        )
        .map(|state| {
            state.with_paging_metadata(
                program.page_count(),
                program.resident_pages(),
                program.entry_pages().iter().copied(),
            )
        })
        .map_err(NsfProgramError::Vm)
    }

    fn zone_environment(
        &mut self,
        zone: Eid,
    ) -> Result<Option<RetailZoneEnvironment>, Self::Error> {
        let entry = self
            .nsf
            .resolve_entry(self.metadata, zone)
            .map_err(NsfProgramError::Format)?;
        if entry.entry_type != 7 {
            return Err(NsfProgramError::Format(FormatError::global(format!(
                "zone {zone} resolves to entry type {}, expected ZDAT type 7",
                entry.entry_type
            ))));
        }
        let header_item = entry.item(0).ok_or_else(|| {
            NsfProgramError::Format(FormatError::global(format!(
                "zone {zone} has no ZDAT header item"
            )))
        })?;
        let rect_item = entry.item(1).ok_or_else(|| {
            NsfProgramError::Format(FormatError::global(format!(
                "zone {zone} has no ZDAT rectangle item"
            )))
        })?;
        let header = ZoneHeader::parse(
            header_item
                .bytes(self.nsf_bytes)
                .map_err(NsfProgramError::Format)?,
        )
        .map_err(NsfProgramError::Format)?;
        let rect = ZoneRect::parse(
            rect_item
                .bytes(self.nsf_bytes)
                .map_err(NsfProgramError::Format)?,
        )
        .map_err(NsfProgramError::Format)?;
        Ok(Some(RetailZoneEnvironment {
            origin: rect.origin,
            object_colors: header.graphics.object_colors.words,
            player_colors: header.graphics.player_colors.words,
            graphics_flags: header.graphics.flags,
        }))
    }

    fn find_neighbor_zone(
        &mut self,
        current_zone: Eid,
        point: [i32; 3],
    ) -> Result<Option<Eid>, Self::Error> {
        let entry = self
            .nsf
            .resolve_entry(self.metadata, current_zone)
            .map_err(NsfProgramError::Format)?;
        if entry.entry_type != 7 {
            return Err(NsfProgramError::Format(FormatError::global(format!(
                "zone {current_zone} resolves to entry type {}, expected ZDAT type 7",
                entry.entry_type
            ))));
        }
        let header_item = entry.item(0).ok_or_else(|| {
            NsfProgramError::Format(FormatError::global(format!(
                "zone {current_zone} has no ZDAT header item"
            )))
        })?;
        let header = ZoneHeader::parse(
            header_item
                .bytes(self.nsf_bytes)
                .map_err(NsfProgramError::Format)?,
        )
        .map_err(NsfProgramError::Format)?;
        find_retail_neighbor_zone(&header.neighbors, point, |neighbor| {
            let entry = self
                .nsf
                .resolve_entry(self.metadata, neighbor)
                .map_err(NsfProgramError::Format)?;
            if entry.entry_type != 7 {
                return Err(NsfProgramError::Format(FormatError::global(format!(
                    "SZON neighbor {neighbor} resolves to entry type {}, expected ZDAT type 7",
                    entry.entry_type
                ))));
            }
            let rect_item = entry.item(1).ok_or_else(|| {
                NsfProgramError::Format(FormatError::global(format!(
                    "SZON neighbor {neighbor} has no ZDAT rectangle item"
                )))
            })?;
            let rect = ZoneRect::parse(
                rect_item
                    .bytes(self.nsf_bytes)
                    .map_err(NsfProgramError::Format)?,
            )
            .map_err(NsfProgramError::Format)?;
            Ok(rect)
        })
    }

    fn current_zone_neighbors(&mut self, current_zone: Eid) -> Result<Vec<Eid>, Self::Error> {
        let entry = self
            .nsf
            .resolve_entry(self.metadata, current_zone)
            .map_err(NsfProgramError::Format)?;
        if entry.entry_type != 7 {
            return Err(NsfProgramError::Format(FormatError::global(format!(
                "zone {current_zone} resolves to entry type {}, expected ZDAT type 7",
                entry.entry_type
            ))));
        }
        let header_item = entry.item(0).ok_or_else(|| {
            NsfProgramError::Format(FormatError::global(format!(
                "zone {current_zone} has no ZDAT header item"
            )))
        })?;
        let header = ZoneHeader::parse(
            header_item
                .bytes(self.nsf_bytes)
                .map_err(NsfProgramError::Format)?,
        )
        .map_err(NsfProgramError::Format)?;
        for neighbor in header.neighbors.iter().copied() {
            let neighbor_entry = self
                .nsf
                .resolve_entry(self.metadata, neighbor)
                .map_err(NsfProgramError::Format)?;
            if neighbor_entry.entry_type != 7 {
                return Err(NsfProgramError::Format(FormatError::global(format!(
                    "zone {current_zone} neighbor {neighbor} resolves to entry type {}, expected ZDAT type 7",
                    neighbor_entry.entry_type
                ))));
            }
        }
        Ok(header.neighbors)
    }

    fn solid_environment(
        &mut self,
        zone: Eid,
    ) -> Result<Option<RetailSolidEnvironment>, Self::Error> {
        let entry = self
            .nsf
            .resolve_entry(self.metadata, zone)
            .map_err(NsfProgramError::Format)?;
        if entry.entry_type != 7 {
            return Err(NsfProgramError::Format(FormatError::global(format!(
                "zone {zone} resolves to entry type {}, expected ZDAT type 7",
                entry.entry_type
            ))));
        }
        let header_item = entry.item(0).ok_or_else(|| {
            NsfProgramError::Format(FormatError::global(format!(
                "zone {zone} has no ZDAT header item"
            )))
        })?;
        let header = ZoneHeader::parse(
            header_item
                .bytes(self.nsf_bytes)
                .map_err(NsfProgramError::Format)?,
        )
        .map_err(NsfProgramError::Format)?;
        let mut neighbors = Vec::with_capacity(header.neighbors.len());
        for neighbor in &header.neighbors {
            let entry = self
                .nsf
                .resolve_entry(self.metadata, *neighbor)
                .map_err(NsfProgramError::Format)?;
            if entry.entry_type != 7 {
                return Err(NsfProgramError::Format(FormatError::global(format!(
                    "solid-query neighbor {neighbor} resolves to entry type {}, expected ZDAT type 7",
                    entry.entry_type
                ))));
            }
            let neighbor_header_item = entry.item(0).ok_or_else(|| {
                NsfProgramError::Format(FormatError::global(format!(
                    "solid-query neighbor {neighbor} has no ZDAT header item"
                )))
            })?;
            let neighbor_header = ZoneHeader::parse(
                neighbor_header_item
                    .bytes(self.nsf_bytes)
                    .map_err(NsfProgramError::Format)?,
            )
            .map_err(NsfProgramError::Format)?;
            let rect_item = entry.item(1).ok_or_else(|| {
                NsfProgramError::Format(FormatError::global(format!(
                    "solid-query neighbor {neighbor} has no ZDAT rectangle item"
                )))
            })?;
            let bytes = rect_item
                .bytes(self.nsf_bytes)
                .map_err(NsfProgramError::Format)?;
            let rect = ZoneRect::parse(bytes).map_err(NsfProgramError::Format)?;
            neighbors.push(
                RetailSolidZone::new(
                    rect.origin,
                    rect.dimensions,
                    rect.octree_root,
                    rect.octree_max_depth,
                    bytes.to_vec(),
                )
                .map_err(NsfProgramError::Vm)?
                .with_eid(*neighbor)
                .with_graphics(
                    neighbor_header.graphics.flags,
                    neighbor_header.graphics.water_y,
                ),
            );
        }
        Ok(Some(
            RetailSolidEnvironment::new(
                header.graphics.flags,
                header.graphics.object_colors.words,
                header.graphics.player_colors.words,
                neighbors,
            )
            .with_object_shader(
                header.graphics.unknown_a,
                retail_object_shader_depth_anchor(
                    self.metadata.level(),
                    header.graphics.visibility_depth,
                    header.graphics.unknown_b_to_e[0],
                ),
            )
            .with_runtime_context(Some(zone), solid_level_quirks(self.metadata.level())),
        ))
    }

    fn animation_bound_source(
        &mut self,
        binding: AnimationBoundBinding,
    ) -> Result<Option<AnimationBoundSource>, Self::Error> {
        let model_eid = match binding.reference {
            AnimationBoundReference::ItemFive(reference) => {
                let global_eid = self.global_eid(binding.executable)?;
                let global = self
                    .nsf
                    .resolve_entry(self.metadata, global_eid)
                    .map_err(NsfProgramError::Format)?;
                let animation_item = global.item(5).ok_or_else(|| {
                    NsfProgramError::Format(FormatError::global(format!(
                        "global GOOL {global_eid} has no animation item five"
                    )))
                })?;
                let animation_bytes = animation_item
                    .bytes(self.nsf_bytes)
                    .map_err(NsfProgramError::Format)?;
                let descriptor = parse_gool_animation_descriptor(
                    animation_bytes,
                    usize::try_from(reference.offset()).map_err(|_| {
                        NsfProgramError::Format(FormatError::global(
                            "GOOL animation offset does not fit the host",
                        ))
                    })?,
                )
                .map_err(NsfProgramError::Format)?;
                let GoolAnimationDescriptor::Vertex(vertex) = descriptor else {
                    return Ok(Some(AnimationBoundSource::NonVertex));
                };
                vertex.model_eid
            }
            AnimationBoundReference::Model(model_eid) => model_eid,
        };
        let Ok(frame_index) = u16::try_from(binding.frame_index) else {
            return Ok(None);
        };

        // Retail assets occasionally name a model held by another stream pair.
        // A single-pair host cannot page that dormant reference, so absence
        // from this NSD is controlled `None`; a present but malformed
        // declaration remains a format error.
        if self.metadata.pte(model_eid).is_none() {
            return Ok(None);
        }
        let vertex_entry = self
            .nsf
            .resolve_entry(self.metadata, model_eid)
            .map_err(NsfProgramError::Format)?;
        let vertex_kind = ObjectVertexKind::from_entry_type(vertex_entry.entry_type)
            .map_err(NsfProgramError::Format)?;
        let Some(frame_item) = vertex_entry.item(usize::from(frame_index)) else {
            return Ok(None);
        };
        let frame = parse_object_frame(
            frame_item
                .bytes(self.nsf_bytes)
                .map_err(NsfProgramError::Format)?,
            vertex_kind,
        )
        .map_err(NsfProgramError::Format)?;
        // Collision uses only the SVTX/CVTX frame header. TGEO supplies
        // polygons and materials to rendering, but retail's local/world bound
        // calculations never resolve it. Keeping that boundary separate also
        // avoids reparsing full geometry for every collidable object/frame.
        let header = frame.header;
        Ok(Some(AnimationBoundSource::Vertex {
            vertex_kind,
            serialized_bound: Bounds3 {
                min: Vec3 {
                    x: header.local_bound_min[0],
                    y: header.local_bound_min[1],
                    z: header.local_bound_min[2],
                },
                max: Vec3 {
                    x: header.local_bound_max[0],
                    y: header.local_bound_max[1],
                    z: header.local_bound_max[2],
                },
            },
            collision_center: Vec3 {
                x: header.collision_center[0],
                y: header.collision_center[1],
                z: header.collision_center[2],
            },
        }))
    }

    fn animation_display_vertex_kind(
        &mut self,
        binding: AnimationBoundBinding,
    ) -> Result<Option<ObjectVertexKind>, Self::Error> {
        Ok(match self.animation_bound_source(binding)? {
            Some(AnimationBoundSource::Vertex { vertex_kind, .. }) => Some(vertex_kind),
            Some(AnimationBoundSource::NonVertex) | None => None,
        })
    }

    fn model_vertex_source(
        &mut self,
        binding: ModelVertexBinding,
    ) -> Result<Option<ModelVertexSource>, Self::Error> {
        // Some descriptors intentionally refer to a model resident in another
        // retail pair. A pair-scoped host cannot manufacture those bytes.
        if self.metadata.pte(binding.model_eid).is_none() {
            return Ok(None);
        }
        let frame_index = u16::try_from(binding.frame_index).map_err(|_| {
            NsfProgramError::Format(FormatError::global(format!(
                "model {} frame {} does not fit the retail frame index",
                binding.model_eid, binding.frame_index
            )))
        })?;
        let model = load_object_model_frame(
            self.metadata,
            self.nsf,
            self.nsf_bytes,
            binding.model_eid,
            frame_index,
        )
        .map_err(NsfProgramError::Format)?;
        let Some(vertex_offset) = binding
            .vertex_index
            .checked_mul(6)
            .and_then(|offset| u16::try_from(offset).ok())
        else {
            return Ok(None);
        };
        if usize::try_from(binding.vertex_index)
            .ok()
            .is_none_or(|vertex_index| vertex_index >= model.frame.vertex_count())
        {
            return Ok(None);
        }
        // `ObjectFrame::local_position` uses the renderer's quarter-scale
        // model domain. GoolOpTransformVectors uses the same packed vertex at
        // `<< 10`, exactly 256 times that value.
        let local_position = model
            .frame
            .local_position(vertex_offset)
            .map_err(NsfProgramError::Format)?
            .map(|value| value.wrapping_mul(256));
        Ok(Some(ModelVertexSource {
            local_position,
            geometry_scale: model.geometry.header.scale,
        }))
    }
}

impl NsfProgramHost<'_> {
    /// Resolves one checked LDAT executable-map slot without exposing stream
    /// pointers. Platform pagers use this to mirror `GoolObjectInit`'s physical
    /// count-zero global open before binding the Rust program.
    pub fn global_eid(&self, executable: u8) -> Result<Eid, NsfProgramError> {
        let ldat = self.metadata.ldat().ok_or(NsfProgramError::MissingLdat)?;
        let global_eid = ldat
            .executable_map
            .get(usize::from(executable))
            .copied()
            .ok_or(NsfProgramError::ExecutableOutsideLdat(executable))?;
        if global_eid == Eid::NONE || !global_eid.is_named() {
            return Err(NsfProgramError::MissingExecutable {
                executable,
                eid: global_eid,
            });
        }
        Ok(global_eid)
    }
}

/// Stream-backed GOOL host coupled to the shared retail page allocator.
///
/// This is used by native characterization runs and other non-browser hosts;
/// the browser layers audio/card ownership on the same [`Pager`] contract.
#[derive(Debug)]
pub struct PagedNsfProgramHost<'assets, 'pager> {
    program: NsfProgramHost<'assets>,
    pager: &'pager mut Pager,
}

impl<'assets, 'pager> PagedNsfProgramHost<'assets, 'pager> {
    #[must_use]
    pub fn new(
        metadata: &'assets Nsd,
        nsf: &'assets Nsf,
        nsf_bytes: &'assets [u8],
        pager: &'pager mut Pager,
    ) -> Self {
        Self {
            program: NsfProgramHost::new(metadata, nsf, nsf_bytes),
            pager,
        }
    }

    #[must_use]
    pub fn pager(&self) -> &Pager {
        self.pager
    }

    pub fn pager_mut(&mut self) -> &mut Pager {
        self.pager
    }
}

impl ProgramHost for PagedNsfProgramHost<'_, '_> {
    type Error = NsfProgramError;

    fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
        self.program.bind_program(binding)
    }

    fn materialize_program_page(
        &mut self,
        binding: ProgramBinding<'_>,
    ) -> Result<Option<PagerOpenOutcome>, Self::Error> {
        let global = self.program.global_eid(binding.executable)?;
        self.pager
            .materialize_eid_with_outcome(global)
            .map(Some)
            .map_err(NsfProgramError::Paging)
    }

    fn bind_state_program(
        &mut self,
        binding: StateProgramBinding,
    ) -> Result<VmStateProgram, Self::Error> {
        self.program.bind_state_program(binding)
    }

    fn zone_environment(
        &mut self,
        zone: Eid,
    ) -> Result<Option<RetailZoneEnvironment>, Self::Error> {
        self.program.zone_environment(zone)
    }

    fn solid_environment(
        &mut self,
        zone: Eid,
    ) -> Result<Option<RetailSolidEnvironment>, Self::Error> {
        self.program.solid_environment(zone)
    }

    fn find_neighbor_zone(
        &mut self,
        current_zone: Eid,
        point: [i32; 3],
    ) -> Result<Option<Eid>, Self::Error> {
        self.program.find_neighbor_zone(current_zone, point)
    }

    fn current_zone_neighbors(&mut self, current_zone: Eid) -> Result<Vec<Eid>, Self::Error> {
        self.program.current_zone_neighbors(current_zone)
    }

    fn animation_bound_source(
        &mut self,
        binding: AnimationBoundBinding,
    ) -> Result<Option<AnimationBoundSource>, Self::Error> {
        self.program.animation_bound_source(binding)
    }

    fn animation_display_vertex_kind(
        &mut self,
        binding: AnimationBoundBinding,
    ) -> Result<Option<ObjectVertexKind>, Self::Error> {
        self.program.animation_display_vertex_kind(binding)
    }

    fn model_vertex_source(
        &mut self,
        binding: ModelVertexBinding,
    ) -> Result<Option<ModelVertexSource>, Self::Error> {
        self.program.model_vertex_source(binding)
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
                        return Err(NsfProgramError::PagingPageMismatch {
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
                Err(error) => Err(NsfProgramError::Paging(error)),
            },
            PagingHostOperation::Close => {
                let outcome = self
                    .pager
                    .close_eid_retail_with_outcome(request.eid)
                    .map_err(NsfProgramError::Paging)?;
                if outcome.page != request.page {
                    return Err(NsfProgramError::PagingPageMismatch {
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
            PagingHostOperation::Probe => Ok(PagingHostResponse::Applied {
                invalidated: PageInvalidations::NONE,
            }),
        }
    }

    fn texture_frame_snapshot(&self) -> Option<TextureFrameSnapshot> {
        Some(self.pager.texture_frame_snapshot())
    }
}

/// A spawn attempt plus the result of binding its program into the VM.
#[derive(Debug, Eq, PartialEq)]
pub struct RuntimeSpawnAttempt<E> {
    pub neighbor_index: usize,
    pub entity_index: usize,
    pub zone: Eid,
    pub descriptor: EntitySpawnDescriptor,
    pub result: Result<RuntimeObjectHandle, RuntimeError<E>>,
}

/// One interpreter invocation made during a cooperative 30 Hz frame.
#[derive(Debug, Eq, PartialEq)]
pub struct RuntimeExecution<E> {
    pub object: RuntimeObjectHandle,
    pub result: Result<Execution, RuntimeError<E>>,
}

/// Deterministic trace from one call to [`RetailRuntime::run_frame`].
#[derive(Debug, Eq, PartialEq)]
pub struct RuntimeFrame<E> {
    pub frame_index: u64,
    pub executions: Vec<RuntimeExecution<E>>,
    pub spawned_children: Vec<RuntimeObjectHandle>,
    pub effects: Vec<VmEffect>,
}

/// Authoritative pointer-free state corresponding to native `title_struct`.
///
/// The requested screen remains the exact 32-bit GOOL global at word 18;
/// this snapshot owns only the C-side current/pending screen and transition
/// phase that are not serialized in the stream VM.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailTitlePresentation {
    pub screen: TitleScreen,
    pub next_screen: TitleScreen,
    pub phase: TitlePhase,
    /// Native `TitleUpdate` submitted an opaque overlay while swapping the
    /// screen in this source frame. This remains set through the following
    /// `GLUpdate`/browser render even if `TitleLoadState` synchronously starts
    /// another fade-out.
    pub opaque_swap_overlay: bool,
    /// Live signed global 106 after the most recent `GLUpdate` fade step.
    pub fade_counter: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RetailTitleState {
    screen: TitleScreen,
    next_screen: TitleScreen,
    phase: TitlePhase,
    opaque_swap_overlay: bool,
}

/// Host work requested from the middle of native `TitleUpdate`.
///
/// The browser applies this before [`RetailRuntime::finish_retail_title_update`]
/// because `TitleLoadState` can synchronously spawn authored objects before
/// the source's final title-state comparison.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetailTitleAction {
    LoadScreen {
        previous: TitleScreen,
        screen: TitleScreen,
    },
}

/// Checked failure at the source title/GL boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RetailTitleError {
    NotConfigured,
    InvalidTitleState(u32),
    Vm(VmError),
}

impl From<VmError> for RetailTitleError {
    fn from(error: VmError) -> Self {
        Self::Vm(error)
    }
}

/// A source-ordered boundary inside one retail object-tree traversal.
///
/// Native calls `PadUpdate` from `GoolObjectUpdate` immediately before it
/// updates the live Crash object. Keeping this boundary typed lets a platform
/// perform that one process-owned operation after earlier roots have run and
/// before Crash or any later root observes its results.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetailTraversalBoundary {
    BeforeMainObjectUpdate {
        root: RootHandle,
        object: RuntimeObjectHandle,
    },
}

/// Pointer-free host mirrors used around native level save and restart calls.
///
/// Pointer-bearing C globals are replaced by a validated camera location and
/// an ordered list of active neighbor EIDs. The browser refreshes this value
/// after every `LevelUpdate`; synchronous scalar GOOL globals remain
/// authoritative when a save or restart happens before that refresh.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetailLevelStateContext {
    pub location: RetailCameraLocation,
    pub graphics_flags: u32,
    /// Host-side mirror used by restart bookkeeping. `LevelSaveState` samples
    /// the live VM global instead because GOOL can change it synchronously.
    pub box_count: i32,
    /// Native `checkpoint_id`, including its eight fractional/tag bits. `-1`
    /// means no checkpoint; zero deliberately remains distinct.
    pub checkpoint_id: i32,
    pub checkpoint_translation: [i32; 3],
    pub first_spawn: bool,
    /// Current header neighbors with display bit one set, in header order.
    pub active_neighbor_zones: Vec<Eid>,
}

/// Pointer-free representation of retail `level_state`.
///
/// `level_spawns` is intentionally absent: although it exists in the C
/// structure, `LevelSaveState` never copies that array. All fields actually
/// written by the source are represented at their native widths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetailLevelSnapshot {
    pub player_translation: [i32; 3],
    /// Retail angle order is Y, X, Z; saving always zeroes all three words.
    pub player_rotation_yxz: [i32; 3],
    pub player_scale: [i32; 3],
    pub location: RetailCameraLocation,
    pub level: LevelId,
    pub death_resets_counter: bool,
    pub spawn_words: [u32; SPAWN_TABLE_CAPACITY],
    pub box_count: i32,
}

/// Process-lifetime scalar state retained while pair-owned runtime state is
/// destroyed and rebuilt around a newly mounted stream.
///
/// Native `NSKill` does not clear `gool_globals`, the gameplay RNG, or the
/// global `level_state` save. Rust owns those values explicitly so no native
/// pointers or pair-backed references cross the mount boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetailSessionCarry {
    pub globals: Box<[u32]>,
    /// Native process-lifetime `level_spawns` encounter registry. This is not
    /// the destination pair's fresh 304-word active spawn table.
    pub level_spawn_tags: Box<[u16]>,
    pub saved_level_state: Option<RetailLevelSnapshot>,
    pub random_seed: u32,
    /// Native renderer animation counter. `NSKill`/`NSInit` preserve this
    /// process-lifetime word; only `LevelRestart` resets it to zero.
    pub draw_count: u32,
    pub respawn_count: u32,
    pub death_count: u32,
    /// Native `first_spawn`, armed by a different-level `LevelRestart` and
    /// applied when the destination host supplies its new camera context.
    pub first_spawn: bool,
    /// Complete `ShaderParamsUpdate` data/BSS retained by `NSKill`/`NSInit`.
    /// The destination's initialization subsequently rewrites only the exact
    /// source subset, including the separate zero-seeded `randb` stream.
    level_shader: RetailLevelShaderState,
}

impl RetailSessionCarry {
    /// Native process-global `seed_b`, shared by dynamic lighting, PBAK
    /// selection, and the audio voice allocator.
    #[must_use]
    pub const fn random_seed_b(&self) -> u32 {
        self.level_shader.random_seed_b
    }

    /// Reconciles the shared RNG-B word after a pair-owned host operation.
    /// Browser audio is deliberately owned outside [`RetailRuntime`], so the
    /// web boundary publishes its latest draw before carrying the session to
    /// another stream.
    pub const fn set_random_seed_b(&mut self, seed: u32) {
        self.level_shader.random_seed_b = seed;
    }
}

/// Checked failure while rebuilding a pair-owned runtime from retained
/// process-lifetime state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetailSessionImportError {
    GlobalWordCount { expected: usize, actual: usize },
    LevelSpawnTagCount { expected: usize, actual: usize },
}

/// Result of the exact `CoreResolveLevelTransition` decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedRetailLevelTransition {
    pub level: LevelId,
    pub bonus_return: bool,
}

/// A malformed or incomplete signed native level transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetailTransitionError {
    InvalidRequestedLevel(i32),
    MissingSavedLevelState,
}

/// Preserves the originally requested target; only a post-event `-2` selects
/// the level held by the retail save snapshot.
pub fn resolve_retail_level_transition(
    requested_lid: i32,
    next_lid_after_event: i32,
    saved_level: Option<LevelId>,
) -> Result<ResolvedRetailLevelTransition, RetailTransitionError> {
    if next_lid_after_event == -2 {
        return Ok(ResolvedRetailLevelTransition {
            level: saved_level.ok_or(RetailTransitionError::MissingSavedLevelState)?,
            bonus_return: true,
        });
    }
    let requested = u32::try_from(requested_lid)
        .ok()
        .and_then(|requested| LevelId::new(requested).ok())
        .ok_or(RetailTransitionError::InvalidRequestedLevel(requested_lid))?;
    Ok(ResolvedRetailLevelTransition {
        level: requested,
        bonus_return: false,
    })
}

/// One checked event failure encountered by the native all-root level-end
/// broadcast. Delivery continues with the remaining postorder recipients.
#[derive(Debug, Eq, PartialEq)]
pub struct RetailLevelEndEventFailure<E> {
    pub object: RuntimeObjectHandle,
    pub error: RuntimeError<E>,
}

/// Source-ordered result of the pre-remount `GOOL_EVENT_LEVEL_END` phase.
#[derive(Debug, Eq, PartialEq)]
pub struct RetailLevelEndReport<E> {
    pub requested_lid: i32,
    pub next_lid_after_event: i32,
    pub resolved: ResolvedRetailLevelTransition,
    pub event_failures: Vec<RetailLevelEndEventFailure<E>>,
    /// Effects emitted by the broadcast in recipient/instruction order.
    /// Transition and different-level load effects are already folded into
    /// `next_lid_after_event` and must not be replayed by the caller.
    pub effects: Vec<VmEffect>,
    pub carry: RetailSessionCarry,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RetailSaveStateOutcome {
    Saved(Box<RetailLevelSnapshot>),
    RestrictedByZone,
}

/// A checked failure at the pointer-free `LevelSaveState` boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetailLevelStateError {
    MissingContext,
    MissingLevel,
    MissingMainObject,
    UnknownCaller(RuntimeObjectHandle),
    Vm(VmError),
}

/// Checked failure while installing a pointer-free PBAK restart snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RetailDemoStartError {
    MissingLevel,
    LevelMismatch { mounted: LevelId, recorded: LevelId },
    MissingLevelStateContext,
    MissingMainObject,
    Vm(VmError),
}

/// Source result after `PadUpdatePbak` exposes its final recorded pad word.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RetailDemoFinishOutcome {
    /// A non-island recording simply releases physical input ownership.
    Released,
    /// Island-camera recordings synchronously notify the live caption object
    /// and retain the native `pbak_state == 3` input lock until its authored
    /// GOOL flow requests the next level.
    CaptionEvent {
        recipient: RuntimeObjectHandle,
        dispatch: EventDispatchOutcome,
        effects: Vec<VmEffect>,
    },
    /// The caption handler was malformed, but native discards that event
    /// status and still retains its `pbak_state == 3` input lock.
    CaptionEventFault {
        recipient: RuntimeObjectHandle,
        effects: Vec<VmEffect>,
    },
}

impl RetailDemoFinishOutcome {
    /// VM effects emitted by the synchronous caption handoff.
    ///
    /// A failed outer frame has no [`RuntimeFrame`] from which the browser can
    /// recover these already-authored effects, so the PBAK owner may replay
    /// this exact slice once on that error path. Zero-island releases emit no
    /// caption event and therefore expose an empty slice.
    #[must_use]
    pub fn effects(&self) -> &[VmEffect] {
        match self {
            Self::Released => &[],
            Self::CaptionEvent { effects, .. } | Self::CaptionEventFault { effects, .. } => effects,
        }
    }
}

/// One failed source-order `RESPAWN` broadcast during `LevelRestart`.
#[derive(Debug, Eq, PartialEq)]
pub struct RetailRespawnEventFailure<E> {
    pub object: RuntimeObjectHandle,
    pub error: RuntimeError<E>,
}

/// Same-level restart work completed by [`RetailRuntime`]. The host must then
/// apply `LevelUpdate(snapshot.location, level_update_flags)` to its camera,
/// pager, and zone lifecycle before the next spawn scan.
#[derive(Debug, Eq, PartialEq)]
pub struct RetailRestartReport<E> {
    pub snapshot: RetailLevelSnapshot,
    pub level_update_flags: u8,
    pub respawn_event_failures: Vec<RetailRespawnEventFailure<E>>,
    pub zone_reports: Vec<(Eid, ZoneTerminationReport<E>)>,
    pub respawn_count: u32,
    pub death_count: u32,
    pub restored_box_count: i32,
}

/// Native different-level saves do not partially reload the current stream;
/// they request sentinel level `-2` and arm `first_spawn` for the next mount.
#[derive(Debug, Eq, PartialEq)]
pub enum RetailRestartOutcome<E> {
    Restarted(Box<RetailRestartReport<E>>),
    DifferentLevel {
        saved_level: LevelId,
        requested_level_sentinel: i32,
    },
}

/// Combined trace for the current-zone scan and its first simulation frame.
#[derive(Debug, Eq, PartialEq)]
pub struct SpawnedRuntimeFrame<E> {
    pub spawn_attempts: Vec<RuntimeSpawnAttempt<E>>,
    pub frame: RuntimeFrame<E>,
}

/// Native zone-termination mode selected by the level lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ZoneTerminationMode {
    /// Ordinary camera/zone departure. An object that migrates to another zone
    /// while handling the terminate event survives.
    Departure { target: Eid },
    /// `LevelRestart`'s native `obj_zone == (entry *)-1` sentinel. Migration
    /// no longer prevents teardown, but the ordinary eligibility gates and
    /// non-title Crash immunity still apply.
    HardRestart,
}

/// Pointer-free process-lifetime representation of native `obj_zone`.
///
/// The value starts null, becomes the destination on a real zone departure,
/// and becomes `(entry *)-1` for hard restart. Native does not scope or restore
/// this global around TERM delivery, so the runtime deliberately persists it
/// until the next source-ordered assignment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectZoneContext {
    Null,
    Target(Eid),
    HardRestartSentinel,
}

/// Cleanup work whose owner lives outside [`RetailRuntime`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeCleanupAction {
    /// Equivalent to native `AudioVoiceFree(object)` for a stale object pair.
    ///
    /// The simulation crate intentionally has no WebAudio/voice allocator.
    /// The platform audio owner must consume these actions before it reuses an
    /// object-associated voice. Actions retain exact child-before-parent
    /// teardown order.
    FreeObjectAudio(RuntimeObjectHandle),
}

/// One TERM delivery that faulted during native expendable-object reclaim.
///
/// Reclaim still releases the object, matching `GoolObjectKill`'s ignored
/// event return value. The host error itself cannot be retained by the
/// non-generic runtime, so this ordered queue exposes the exact recipient
/// identities for deterministic diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeReclaimEventFault {
    pub object: RuntimeObjectHandle,
}

/// One collision-generated event whose checked GOOL handler faulted.
///
/// `TransSmoothStopAtSolid` and its object/water/outside-zone branches discard
/// `GoolSendEvent`'s return value. Rust preserves that control flow while this
/// queue keeps the exact mover, recipient, event, and native call site
/// observable without retaining a generic host error in the runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeSolidEventFault {
    pub moving_object: RuntimeObjectHandle,
    pub recipient: RuntimeObjectHandle,
    pub event: u32,
    pub reason: SolidEventReason,
}

/// One invincibility-hit event whose checked GOOL handler faulted.
///
/// `GoolObjectColors` discards `GoolSendEvent`'s return value and continues
/// into its cyclic-color and physics tail. This diagnostic preserves the
/// exact sender, recipient, and event without changing that native ordering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeInvincibilityEventFault {
    pub sender: RuntimeObjectHandle,
    pub recipient: RuntimeObjectHandle,
    pub event: u32,
}

/// Deterministic result of terminating all eligible objects from one zone.
#[derive(Debug, Eq, PartialEq)]
pub struct ZoneTerminationEventFailure<E> {
    pub object: RuntimeObjectHandle,
    pub error: RuntimeError<E>,
}

/// Deterministic result of terminating all eligible objects from one zone.
#[derive(Debug, Eq, PartialEq)]
pub struct ZoneTerminationReport<E> {
    /// Removed identities in native recursive release order.
    pub terminated: Vec<RuntimeObjectHandle>,
    /// Objects whose terminate handler changed their zone during an ordinary
    /// departure. Hard restart never records migration survivors.
    pub migrated: Vec<RuntimeObjectHandle>,
    /// Platform-owned cleanup work in the same order as `terminated`.
    pub cleanup_actions: Vec<RuntimeCleanupAction>,
    /// Checked TERM-handler failures. Native teardown ignores these failures
    /// and still kills an object whose zone did not change.
    pub event_failures: Vec<ZoneTerminationEventFailure<E>>,
}

impl<E> ZoneTerminationReport<E> {
    fn new() -> Self {
        Self {
            terminated: Vec::new(),
            migrated: Vec::new(),
            cleanup_actions: Vec::new(),
            event_failures: Vec::new(),
        }
    }
}

/// Checked failures at the arena/VM/asset boundary.
#[derive(Debug, Eq, PartialEq)]
pub enum RuntimeError<E> {
    Spawn(SpawnError),
    Create(RuntimeCreateError),
    Tree(TreeError),
    Vm(VmError),
    ObjectZoneShader(RetailObjectZoneShaderError),
    Program(E),
    VmHandleCapacity,
    DuplicateArenaBinding(ArenaObjectHandle),
    UnknownArenaObject(ArenaObjectHandle),
    UnknownVmObject(VmObjectHandle),
    InvalidRootIndex(usize),
    EntityIndexUnavailable {
        neighbor_index: usize,
        entity_index: usize,
    },
    HostObjectHandleMismatch {
        expected: VmObjectHandle,
        actual: VmObjectHandle,
    },
    InvalidGlobalObjectReference {
        global: usize,
        value: u32,
    },
    MissingDemoCaptionObject,
    MissingTransitionZoneTarget,
    MissingLevelStateContext,
    MissingSavedLevelState,
    MissingMainObject,
    LevelState(RetailLevelStateError),
    Transition(RetailTransitionError),
    PendingLevelRestartAtLevelEnd,
    SameLevelRestartDuringLevelEnd(LevelId),
    SavedLevelChangedAfterLoad {
        captured: LevelId,
        current: LevelId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HandleMap {
    vm_by_arena: BTreeMap<ArenaObjectHandle, VmObjectHandle>,
    arena_by_vm: [Option<ArenaObjectHandle>; MAX_OBJECTS],
    /// Last physical native pool slot paired with each compact VM handle.
    /// This survives release so an initialized stale pointer can be resolved
    /// even before the Dark2 shader first observes its tombstone.
    retired_arena_slots_by_vm: [Option<u8>; MAX_OBJECTS],
}

struct FrameWork<E> {
    executions: Vec<RuntimeExecution<E>>,
    spawned_children: Vec<RuntimeObjectHandle>,
    display_records: Vec<RetailDisplayRecord>,
    effects: Vec<VmEffect>,
}

struct FrameTraversalHook<'hook, F> {
    callback: &'hook mut F,
    main_invoked: bool,
    paused: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EventLoadStateMode {
    RequestRestart,
    ContinueDifferentLevel,
}

/// Borrowed runtime pieces used by one native send-to-colliders traversal.
///
/// The cursor deliberately stays on the live arena instead of materializing a
/// recipient snapshot: event handlers may mutate later descendants and the C
/// traversal observes those mutations. A sibling is captured immediately
/// before descent, matching the source's only stable pointer.
struct SendEventTraversal<'a, H: ProgramHost> {
    arena: &'a mut ObjectArena,
    handles: &'a mut HandleMap,
    machine: &'a mut Machine,
    pending_states: &'a mut BTreeMap<VmObjectHandle, u16>,
    pending_cleanup_actions: &'a mut Vec<RuntimeCleanupAction>,
    reclaim_event_faults: &'a mut Vec<RuntimeReclaimEventFault>,
    level: Option<LevelId>,
    level_state_context: Option<&'a RetailLevelStateContext>,
    saved_level_state: &'a mut Option<RetailLevelSnapshot>,
    transition_zone_context: ObjectZoneContext,
    host: &'a mut H,
    spawned_children: &'a mut Vec<RuntimeObjectHandle>,
    current_object: Option<VmObjectHandle>,
    sender: RuntimeObjectHandle,
    event: u32,
    arguments: &'a [u32],
    argument_pool_slots: &'a [Option<u8>],
    mode: u8,
    count: u32,
}

impl<H: ProgramHost> SendEventTraversal<'_, H> {
    fn traverse_root(&mut self, root: RootHandle) -> Result<(), VmError> {
        let mut child = self.arena.root_first_child(root);
        while let Some(child_handle) = child {
            let Some(spawned) = self.arena.get(child_handle) else {
                break;
            };
            let sibling = spawned.next_sibling();
            self.traverse_subtree(child_handle)?;
            child = sibling;
        }
        Ok(())
    }

    fn traverse_children(&mut self, root: ArenaObjectHandle) -> Result<(), VmError> {
        let mut child = self.arena.get(root).and_then(SpawnedObject::first_child);
        while let Some(child_handle) = child {
            let Some(spawned) = self.arena.get(child_handle) else {
                break;
            };
            let sibling = spawned.next_sibling();
            self.traverse_subtree(child_handle)?;
            child = sibling;
        }
        Ok(())
    }

    fn traverse_subtree(&mut self, arena_handle: ArenaObjectHandle) -> Result<(), VmError> {
        let mut child = self
            .arena
            .get(arena_handle)
            .and_then(SpawnedObject::first_child);
        while let Some(child_handle) = child {
            let Some(spawned) = self.arena.get(child_handle) else {
                break;
            };
            let sibling = spawned.next_sibling();
            self.traverse_subtree(child_handle)?;
            child = sibling;
        }
        self.deliver_candidate(arena_handle)
    }

    fn deliver_candidate(&mut self, arena_handle: ArenaObjectHandle) -> Result<(), VmError> {
        if !self.handles.is_live_pair(self.sender) {
            return Ok(());
        }
        let Some(recipient) = self.handles.for_arena(arena_handle) else {
            return Ok(());
        };
        let matches =
            self.machine
                .send_event_candidate_matches(self.sender.vm, recipient.vm, self.mode)?;
        if !matches {
            return Ok(());
        }

        // Mode five throttles matching candidates 1,2,3,6,11,...; its count
        // advances even when delivery is suppressed or the recipient handler
        // fails. Other modes send every match and retain the same native count.
        let deliver = self.mode != 5 || self.count < 3 || self.count.is_multiple_of(5);
        self.count += 1;
        if !deliver {
            return Ok(());
        }

        // GoolSendIfColliding discards GoolSendEvent's return code. Contain a
        // malformed or terminating recipient and continue the live traversal.
        let _ = RetailRuntime::dispatch_event_parts_current(
            self.arena,
            self.handles,
            self.machine,
            self.pending_states,
            self.pending_cleanup_actions,
            self.reclaim_event_faults,
            self.level,
            self.level_state_context,
            self.saved_level_state,
            self.transition_zone_context,
            self.host,
            self.current_object,
            Some(self.sender),
            Some(recipient),
            self.event,
            Some(self.arguments),
            Some(self.argument_pool_slots),
            self.spawned_children,
        );
        Ok(())
    }
}

impl Default for HandleMap {
    fn default() -> Self {
        Self {
            vm_by_arena: BTreeMap::new(),
            arena_by_vm: [None; MAX_OBJECTS],
            retired_arena_slots_by_vm: [None; MAX_OBJECTS],
        }
    }
}

impl HandleMap {
    fn reserve<E>(
        &mut self,
        arena: ArenaObjectHandle,
    ) -> Result<RuntimeObjectHandle, RuntimeError<E>> {
        if self.vm_by_arena.contains_key(&arena) {
            return Err(RuntimeError::DuplicateArenaBinding(arena));
        }
        let index = self
            .arena_by_vm
            .iter()
            .position(Option::is_none)
            .ok_or(RuntimeError::VmHandleCapacity)?;
        let vm = VmObjectHandle::new(u16::try_from(index).expect("VM capacity fits u16"))
            .expect("index came from the VM handle array");
        self.arena_by_vm[index] = Some(arena);
        self.vm_by_arena.insert(arena, vm);
        Ok(RuntimeObjectHandle { arena, vm })
    }

    fn release(&mut self, object: RuntimeObjectHandle) {
        if self.vm_by_arena.get(&object.arena) == Some(&object.vm) {
            self.vm_by_arena.remove(&object.arena);
        }
        let index = usize::from(object.vm.get());
        if self.arena_by_vm[index] == Some(object.arena) {
            self.retired_arena_slots_by_vm[index] = Some(object.arena.slot());
            self.arena_by_vm[index] = None;
        }
    }

    fn prune_stale(&mut self, arena: &ObjectArena) {
        let stale = self
            .vm_by_arena
            .iter()
            .filter_map(|(handle, vm)| arena.get(*handle).is_none().then_some((*handle, *vm)))
            .collect::<Vec<_>>();
        for (arena, vm) in stale {
            self.release(RuntimeObjectHandle { arena, vm });
        }
    }

    fn for_arena(&self, arena: ArenaObjectHandle) -> Option<RuntimeObjectHandle> {
        self.vm_by_arena
            .get(&arena)
            .copied()
            .map(|vm| RuntimeObjectHandle { arena, vm })
    }

    fn for_vm(&self, vm: VmObjectHandle) -> Option<RuntimeObjectHandle> {
        self.arena_by_vm
            .get(usize::from(vm.get()))
            .copied()
            .flatten()
            .map(|arena| RuntimeObjectHandle { arena, vm })
    }

    fn for_retail_pool_slot(&self, pool_slot: u8) -> Option<RuntimeObjectHandle> {
        self.vm_by_arena.iter().find_map(|(&arena, &vm)| {
            (arena.slot() == pool_slot).then_some(RuntimeObjectHandle { arena, vm })
        })
    }

    fn retired_arena_slot(&self, vm: VmObjectHandle) -> Option<u8> {
        self.retired_arena_slots_by_vm[usize::from(vm.get())]
    }

    fn is_live_pair(&self, object: RuntimeObjectHandle) -> bool {
        self.for_arena(object.arena) == Some(object) && self.for_vm(object.vm) == Some(object)
    }
}

const LEVEL_SHADER_TABLE_1: [i32; 84] = [
    0, 81, 163, 245, 327, 400, 491, 573, 655, 737, 819, 900, 982, 1_064, 1_146, 1_228, 1_310,
    1_392, 1_474, 1_556, 1_638, 1_719, 1_801, 1_883, 1_965, 2_047, 2_129, 2_211, 2_293, 2_375,
    2_457, 2_538, 2_620, 2_702, 2_784, 2_866, 2_743, 2_620, 2_497, 2_375, 2_252, 2_129, 2_006,
    1_883, 1_760, 1_638, 1_760, 1_883, 2_006, 2_129, 2_252, 2_375, 2_497, 2_620, 2_743, 2_866,
    2_743, 2_620, 2_497, 2_375, 2_252, 2_129, 2_006, 1_883, 1_760, 1_638, 1_515, 1_392, 1_269,
    1_146, 1_023, 941, 859, 778, 696, 614, 532, 450, 368, 286, 204, 122, 40, -1,
];

const LEVEL_SHADER_TABLE_2_A: [i32; 14] = [
    0xfff, 0, 0x7ff, 0xfff, 0x7ff, 0, 0x7ff, 0x599, 0x666, 0x599, 0x3ff, 0x199, 0x0a3, -1,
];
const LEVEL_SHADER_TABLE_2_B: [i32; 10] = [
    0xfff, 0xccc, 0, 0xfff, 0x7ff, 0xfff, 0x666, 0x333, 0x199, -1,
];
const LEVEL_SHADER_TABLE_2_C: [i32; 11] =
    [0xfff, 0xe65, 0xccc, 0xb32, 0, 0, 0xfff, 0, 0xfff, 0x7ff, -1];
const LEVEL_SHADER_TABLE_2_D: [i32; 14] = [
    0x7ff, 0, 0xfff, 0xe65, 0xfff, 0xccc, 0x4cc, 0, 0, 0x7ff, 0x199, 0x7ff, 0x599, -1,
];
const LEVEL_SHADER_TABLE_2_E: [i32; 16] = [
    0xfff, 0, 0xe65, 0xccc, 0xb32, 0x999, 0x7ff, 0x666, 0x4cc, 0x333, 0x7ff, 0x666, 0x4cc, 0x333,
    0x199, -1,
];
const LEVEL_SHADER_TABLE_2_F: [i32; 28] = [
    0xfff, 0xf32, 0xe65, 0xd98, 0xccc, 0xbff, 0xb32, 0xa65, 0x999, 0x8cc, 0x7ff, 0x732, 0x666,
    0x599, 0x4cc, 0x3ff, 0x333, 0x7ff, 0x732, 0x666, 0x599, 0x4cc, 0x3ff, 0x333, 0x266, 0x199,
    0x0cc, -1,
];
const LEVEL_SHADER_TABLE_2: [&[i32]; 6] = [
    &LEVEL_SHADER_TABLE_2_A,
    &LEVEL_SHADER_TABLE_2_B,
    &LEVEL_SHADER_TABLE_2_C,
    &LEVEL_SHADER_TABLE_2_D,
    &LEVEL_SHADER_TABLE_2_E,
    &LEVEL_SHADER_TABLE_2_F,
];

/// Immutable pre-camera world-shader globals consumed by one world submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailWorldShaderSnapshot {
    pub clear_color: [u32; 3],
    pub clear_t: i32,
    pub effect_color: [u32; 3],
    pub effect_t: i32,
    /// Q24.8 position selected from native `doctor`, falling back to Crash.
    pub dark2_illumination: [i32; 3],
    pub dark2_shift_add: u32,
    pub dark2_shift_sub: u32,
    pub dark2_ambient_clear: i32,
    pub dark2_ambient_effect: i32,
}

impl RetailWorldShaderSnapshot {
    /// Source `ShaderParamsUpdate(1)` result for an otherwise fresh process.
    #[must_use]
    pub fn initialized_for_level(level: LevelId) -> Self {
        let mut state = RetailLevelShaderState::default();
        state.initialize(level);
        state.snapshot()
    }
}

/// One accepted source thunder transaction. Rendering state and RNG have
/// already advanced when this cue is returned; an audio failure must not roll
/// either one back.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailThunderCue {
    pub adio: Eid,
    pub pitch: u32,
    pub trigger: u32,
    pub volume_percent: u32,
    pub amplitude: i32,
}

/// Pointer-free ownership of the complete process-global `ShaderParamsUpdate`
/// state. These words survive pair teardown; initialization deliberately
/// rewrites only the subset touched by the source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RetailLevelShaderState {
    clear_color: [u32; 3],
    clear_t: i32,
    effect_color: [u32; 3],
    effect_t: i32,
    dark2_shift_add: u32,
    dark2_shift_sub: u32,
    dark2_ambient_clear: i32,
    dark2_ambient_effect: i32,
    distance: i32,
    sequence_state: i32,
    sequence_index: usize,
    effect_t_target: i32,
    effect_t_rate: i32,
    ruins_effect_color: [u32; 3],
    ruins_random_color_a: [u32; 3],
    ruins_random_color_b: [u32; 3],
    previous_light_source: u32,
    ambient_target: i32,
    ambient_step: i32,
    ambient_next: i32,
    distance_target: i32,
    distance_step: i32,
    distance_next: i32,
    lightning_stamp: u32,
    previous_lightning_stamp: u32,
    dark2_illumination: [i32; 3],
    /// Native `randb` owns a separate zero-initialized BSS stream.
    random_seed_b: u32,
}

impl Default for RetailLevelShaderState {
    fn default() -> Self {
        Self {
            clear_color: [0; 3],
            clear_t: 0x800,
            effect_color: [255; 3],
            effect_t: 0x800,
            dark2_shift_add: 0,
            dark2_shift_sub: 0,
            dark2_ambient_clear: 0,
            dark2_ambient_effect: 0,
            distance: 0,
            sequence_state: -1,
            sequence_index: 0,
            effect_t_target: 0,
            effect_t_rate: 0,
            ruins_effect_color: [0; 3],
            ruins_random_color_a: [0; 3],
            ruins_random_color_b: [0; 3],
            previous_light_source: 0,
            ambient_target: 0,
            ambient_step: 0,
            ambient_next: 0,
            distance_target: 0,
            distance_step: 0,
            distance_next: 0,
            lightning_stamp: 0,
            previous_lightning_stamp: 0,
            dark2_illumination: [0; 3],
            random_seed_b: 0,
        }
    }
}

impl RetailLevelShaderState {
    fn initialize(&mut self, level: LevelId) {
        self.clear_t = 0x800;
        self.clear_color = [0; 3];
        self.effect_t = 0x800;
        self.effect_color = [255; 3];
        self.sequence_index = 0;
        self.sequence_state = -1;
        match level.get() {
            0x03 | 0x06 | 0x07 | 0x37 => {
                self.clear_t = 0;
                self.effect_color = [255, 43, 11];
            }
            0x05 => {
                self.clear_t = 0;
                self.effect_color = [238, 255, 60];
            }
            0x0a | 0x1c | 0x1d => {
                self.clear_t = 0;
                self.effect_color = [255, 100, 0];
            }
            0x13 => {
                self.clear_t = 0;
                self.effect_color = [0, 240, 255];
            }
            0x20 | 0x23 => {
                self.clear_t = 0;
                self.effect_t = 1_600;
                self.ruins_effect_color = [165, 90, 100];
                self.ruins_random_color_a = [165, 90, 100];
                self.ruins_random_color_b = [255, 75, 0];
            }
            0x21 => {
                self.clear_t = 0;
                self.effect_color = [200, 80, 0];
            }
            0x28 | 0x2a => {
                self.effect_t = 0;
                self.dark2_shift_add = 10;
                self.dark2_ambient_clear = -14_000;
                self.previous_light_source = 0;
                self.ambient_next = 4_095;
                self.distance_next = 2_000;
            }
            _ => {}
        }
    }

    const fn snapshot(self) -> RetailWorldShaderSnapshot {
        RetailWorldShaderSnapshot {
            clear_color: self.clear_color,
            clear_t: self.clear_t,
            effect_color: self.effect_color,
            effect_t: self.effect_t,
            dark2_illumination: self.dark2_illumination,
            dark2_shift_add: self.dark2_shift_add,
            dark2_shift_sub: self.dark2_shift_sub,
            dark2_ambient_clear: self.dark2_ambient_clear,
            dark2_ambient_effect: self.dark2_ambient_effect,
        }
    }

    fn random(&mut self, maximum: u32) -> u32 {
        retail_random(maximum, &mut self.random_seed_b)
    }

    fn advance(
        &mut self,
        level: LevelId,
        ticks_elapsed: u32,
        light_source: u32,
        dark2_illumination: Option<[i32; 3]>,
    ) -> Option<RetailThunderCue> {
        match level.get() {
            0x03 | 0x06 | 0x07 | 0x37 => {
                if LEVEL_SHADER_TABLE_1
                    .get(self.sequence_index)
                    .is_none_or(|value| *value == -1)
                {
                    self.sequence_index = 0;
                }
                self.effect_t = LEVEL_SHADER_TABLE_1[self.sequence_index];
                self.sequence_index += 1;
            }
            0x05 | 0x0a | 0x1c | 0x1d => {
                if self.sequence_index == 0 {
                    self.effect_t_target = self.random(1_500).cast_signed();
                    self.effect_t += (self.effect_t_target - self.effect_t) / 2;
                } else {
                    self.effect_t = self.effect_t_target;
                }
                self.sequence_index = (self.sequence_index + 1) % 2;
            }
            0x13 => {
                if LEVEL_SHADER_TABLE_1
                    .get(self.sequence_index)
                    .is_none_or(|value| *value == -1)
                {
                    self.sequence_index = 0;
                }
                self.effect_t = LEVEL_SHADER_TABLE_1[self.sequence_index] >> 1;
                self.sequence_index += 1;
            }
            0x1b | 0x22 | 0x2e => return self.advance_lightning(level, ticks_elapsed),
            0x20 | 0x23 => {
                if self.sequence_index == 0 {
                    let t = self.random(100).cast_signed();
                    for channel in 0..3 {
                        let a = self.ruins_random_color_a[channel].cast_signed();
                        let b = self.ruins_random_color_b[channel].cast_signed();
                        let color = (a + (b - a) * t) / 100;
                        self.effect_color[channel] = ((color - 255) / 2 + 255).cast_unsigned();
                    }
                } else {
                    self.effect_color = self.ruins_effect_color;
                }
                self.sequence_index = (self.sequence_index + 1) % 2;
            }
            0x21 => {
                if self.sequence_index == 0 {
                    self.effect_t_target = self.random(1_000).cast_signed();
                    self.effect_t_rate = (self.effect_t_target - self.effect_t) / 4;
                }
                self.effect_t += self.effect_t_rate;
                self.sequence_index = (self.sequence_index + 1) % 4;
            }
            0x28 | 0x2a => {
                if let Some(illumination) = dark2_illumination {
                    self.dark2_illumination = illumination;
                }
                if light_source == 0 {
                    if self.previous_light_source != 0 {
                        self.ambient_target = 4_095;
                        self.ambient_step = 100;
                        self.distance_target = 2_000;
                        self.distance_step = 20;
                    }
                } else if light_source != self.previous_light_source {
                    self.ambient_target = -8_000;
                    self.ambient_step = -500;
                    self.distance_target = 75;
                    self.distance_step = -75;
                }
                self.previous_light_source = light_source;
                if self.ambient_next != self.ambient_target {
                    self.ambient_next = shader_step_toward(
                        self.ambient_next,
                        self.ambient_target,
                        &mut self.ambient_step,
                    );
                }
                if self.distance_next != self.distance_target {
                    self.distance_next = shader_step_toward(
                        self.distance_next,
                        self.distance_target,
                        &mut self.distance_step,
                    );
                }
                self.dark2_ambient_clear = self.ambient_next;
                self.distance = self.distance_next;
            }
            _ => {}
        }
        None
    }

    fn advance_lightning(
        &mut self,
        level: LevelId,
        ticks_elapsed: u32,
    ) -> Option<RetailThunderCue> {
        if self.sequence_state == -1 {
            self.clear_t = 0;
            self.effect_t = 0;
            if self.random(1_000) >= 25 {
                return None;
            }
            self.sequence_state = if matches!(level.get(), 0x22 | 0x2e) {
                match self.random(3) {
                    0 => 5,
                    1 => 4,
                    _ => self.random(4).cast_signed(),
                }
            } else {
                self.random(6).cast_signed()
            };
            self.sequence_index = 0;
            if ticks_elapsed.wrapping_sub(self.previous_lightning_stamp) < 6_145 {
                return None;
            }
            self.previous_lightning_stamp = self.lightning_stamp;
            self.lightning_stamp = ticks_elapsed;
            let sample = self.random(3) + 1;
            let adio = Eid::from_name(&format!("lt{sample}rA"))
                .expect("fixed lightning sample EID is valid");
            let pitch = (self.random(0x4cc) + 0xd99) >> 3;
            let trigger = self.random(15) + 1;
            let mut volume_percent = self.random(100);
            if volume_percent > 20 {
                volume_percent += self.random(50);
            }
            let amplitude = i32::try_from(0x3fff_u32.wrapping_mul(volume_percent) / 100)
                .expect("retail thunder amplitude fits i32");
            Some(RetailThunderCue {
                adio,
                pitch,
                trigger,
                volume_percent,
                amplitude,
            })
        } else {
            let table = LEVEL_SHADER_TABLE_2
                .get(usize::try_from(self.sequence_state).unwrap_or(usize::MAX));
            let value = table
                .and_then(|table| table.get(self.sequence_index))
                .copied();
            if value.is_none_or(|value| value == -1) {
                self.clear_t = 0;
                self.effect_t = 0;
                self.sequence_state = -1;
            } else if let Some(value) = value {
                self.clear_t = value;
                self.effect_t = value;
                self.sequence_index += 1;
            }
            None
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RetailDisplaySnapshot {
    /// Live global nine consumed by this object's display/transform path.
    display_mask: u32,
    texture_frame_snapshot: Option<TextureFrameSnapshot>,
    enabled: bool,
    /// Validation is retained until `render_objects()` so malformed item-five
    /// or process-local sources keep the render-snapshot error boundary.
    animation_source: Result<Option<AnimationSource>, VmError>,
    animation_frame: u32,
    transform: RetailTransform,
    status_a: u32,
    status_b: u32,
    status_c: u32,
    state_flags: u32,
    size: i32,
    text_font_override_word_offset: u32,
    text_arguments: [Option<u32>; 10],
    dark_reference_translation: Option<[i32; 3]>,
    dark_distance: i32,
    /// Colors consumed by this object's already-completed native transform.
    /// These can differ from the live VM because status-B `0x100000` resets
    /// live colors after geometry but before child traversal.
    colors: [u16; COLOR_COUNT],
}

#[derive(Clone, Copy)]
struct RetailDisplayCapture {
    display_mask: u32,
    texture_frame_snapshot: Option<TextureFrameSnapshot>,
    enabled: bool,
    dark_reference_translation: Option<[i32; 3]>,
    dark_distance: i32,
    effective_colors: Option<[u16; COLOR_COUNT]>,
}

impl RetailDisplaySnapshot {
    fn capture(
        vm_object: &VmObject,
        animation_source: Result<Option<AnimationSource>, VmError>,
        capture: RetailDisplayCapture,
    ) -> Result<Self, VmError> {
        let RetailDisplayCapture {
            display_mask,
            texture_frame_snapshot,
            enabled,
            dark_reference_translation,
            dark_distance,
            effective_colors,
        } = capture;
        Ok(Self {
            display_mask,
            texture_frame_snapshot,
            enabled,
            animation_source,
            animation_frame: vm_object.animation_frame(),
            transform: vm_object.retail_transform()?,
            status_a: vm_object.register(process_register::STATUS_A)?,
            status_b: vm_object.register(process_register::STATUS_B)?,
            status_c: vm_object.status_c(),
            state_flags: vm_object.state_flags(),
            size: vm_object.register(process_register::SIZE)? as i32,
            text_font_override_word_offset: vm_object
                .register(process_register::INVINCIBILITY_STATE)?
                >> 8,
            text_arguments: retail_text_arguments(vm_object.stack()),
            dark_reference_translation,
            dark_distance,
            colors: effective_colors.unwrap_or(*vm_object.retail_colors()),
        })
    }
}

/// One native display submission captured in exact traversal order.
///
/// Unlike the live arena, this record owns every identity needed by the
/// renderer. Later siblings may kill, recycle, or reparent an already-drawn
/// object without changing the frame that native has already submitted.
#[derive(Clone, Debug, Eq, PartialEq)]
struct RetailDisplayRecord {
    object: RuntimeObjectHandle,
    zone: Eid,
    executable: u8,
    subtype: u8,
    program: Option<GoolProgramIdentity>,
    snapshot: RetailDisplaySnapshot,
}

impl RetailDisplayRecord {
    fn render_object(&self) -> Result<RetailRenderObject, RenderObjectsError> {
        let animation_source = self
            .snapshot
            .animation_source
            .clone()
            .map_err(RenderObjectsError::Vm)?;
        let animation_reference = animation_source
            .as_ref()
            .and_then(AnimationSource::item_five_reference);
        Ok(RetailRenderObject {
            object: self.object,
            zone: self.zone,
            executable: self.executable,
            subtype: self.subtype,
            program: self.program,
            animation_source,
            animation_reference,
            animation_frame: self.snapshot.animation_frame,
            transform: self.snapshot.transform,
            status_a: self.snapshot.status_a,
            status_b: self.snapshot.status_b,
            status_c: self.snapshot.status_c,
            state_flags: self.snapshot.state_flags,
            size: self.snapshot.size,
            colors: self.snapshot.colors,
            text_font_override_word_offset: self.snapshot.text_font_override_word_offset,
            text_arguments: self.snapshot.text_arguments,
            dark_reference_translation: self.snapshot.dark_reference_translation,
            dark_distance: self.snapshot.dark_distance,
            display_mask: self.snapshot.display_mask,
            texture_frame_snapshot: self.snapshot.texture_frame_snapshot,
            display_eligible: self.snapshot.enabled,
        })
    }
}

/// Safe replacement for native's `prev_box`, `prev_box_entity`, and
/// `boxes_y` globals. Entity adjacency is retained as an owned path point and
/// a live crate is retained as a generation-checked runtime handle, so neither
/// field can become a dangling C pointer after zone teardown or pool reuse.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RetailBoxSpawnState {
    previous_entity_point: Option<ZoneEntityPathPoint>,
    previous_live_box: Option<RuntimeObjectHandle>,
    boxes_y: i32,
}

impl Default for RetailBoxSpawnState {
    fn default() -> Self {
        Self {
            previous_entity_point: None,
            previous_live_box: None,
            boxes_y: BOX_STACK_SPACING,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RetailBoxSpawnPlan {
    near_box: Option<RuntimeObjectHandle>,
    boxes_y: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MaterializedObject {
    object: RuntimeObjectHandle,
    environment: Option<RetailZoneEnvironment>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetailLevelMiscObjectState {
    Uninitialized,
    Initialized(Option<RuntimeObjectHandle>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RestrictedDirectBootSave {
    Disabled,
    Armed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RetainedDoctorPoolPointer {
    encoded_word: u32,
    global_write_epoch: u64,
    pool_slot: u8,
    translation: [i32; 3],
}

fn shader_step_toward(current: i32, target: i32, step: &mut i32) -> i32 {
    let next = current.wrapping_add(*step);
    if (target > current && next >= target) || (target < current && next <= target) {
        *step = 0;
        target
    } else {
        next
    }
}

/// Pointer-free native coordinator for the first retail runtime slice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetailRuntime {
    arena: ObjectArena,
    machine: Machine,
    handles: HandleMap,
    pending_states: BTreeMap<VmObjectHandle, u16>,
    pending_cleanup_actions: Vec<RuntimeCleanupAction>,
    reclaim_event_faults: Vec<RuntimeReclaimEventFault>,
    pause_event_faults: Vec<RuntimePauseEventFault>,
    solid_event_faults: Vec<RuntimeSolidEventFault>,
    invincibility_event_faults: Vec<RuntimeInvincibilityEventFault>,
    faulted_objects: BTreeSet<RuntimeObjectHandle>,
    /// Exact owned submissions from the last successful frame traversal.
    /// `None` retains the live-state fallback before the first frame.
    rendered_frame_objects: Option<Vec<RetailDisplayRecord>>,
    level: Option<LevelId>,
    transition_zone_context: ObjectZoneContext,
    level_state_context: Option<RetailLevelStateContext>,
    /// Last ZDAT whose ordered neighborhood was installed as native global
    /// `cur_zone` solid/query state.
    current_solid_zone: Option<Eid>,
    saved_level_state: Option<RetailLevelSnapshot>,
    pending_first_spawn: bool,
    suppress_initial_crash_save: bool,
    /// Fresh browser/CLI boots can target a save-restricted bonus stream
    /// without a parent-level snapshot. Native direct boot leaves the static
    /// save buffer invalid in that case; arm one bounded first-Crash fallback
    /// so the advertised direct-boot path remains restartable. Session mounts
    /// never enable this and retain the real parent-level bonus return.
    restricted_direct_boot_save: RestrictedDirectBootSave,
    /// Safe owned form of native's retained `doctor` pool pointer. Its tagged
    /// global word names a compact VM handle, but subsequent reads follow the
    /// captured physical arena slot across free-list and VM-handle reuse.
    retained_doctor_pool_pointer: Option<RetainedDoctorPoolPointer>,
    respawn_count: u32,
    death_count: u32,
    frame_index: u64,
    draw_count: u32,
    level_shader: RetailLevelShaderState,
    box_spawn: RetailBoxSpawnState,
    core_objects_initialized: bool,
    core_objects: Option<RetailCoreObjects>,
    level_misc_object: RetailLevelMiscObjectState,
    pause: RetailPauseState,
    title: Option<RetailTitleState>,
}

impl RetailRuntime {
    #[must_use]
    pub fn new(global_words: usize) -> Self {
        let mut machine = Machine::new(global_words);
        // Authored tests may deliberately construct a zero-global VM. Retail
        // stream hosts allocate the complete globals span and receive the two
        // exact LdatInit display words.
        if global_words > CURRENT_DISPLAY_GLOBAL {
            let next = machine.set_global_word(NEXT_DISPLAY_GLOBAL, INITIAL_DISPLAY_MASK);
            let current = machine.set_global_word(CURRENT_DISPLAY_GLOBAL, INITIAL_DISPLAY_MASK);
            debug_assert!(next.is_ok() && current.is_ok());
        }
        Self {
            arena: ObjectArena::new(),
            machine,
            handles: HandleMap::default(),
            pending_states: BTreeMap::new(),
            pending_cleanup_actions: Vec::new(),
            reclaim_event_faults: Vec::new(),
            pause_event_faults: Vec::new(),
            solid_event_faults: Vec::new(),
            invincibility_event_faults: Vec::new(),
            faulted_objects: BTreeSet::new(),
            rendered_frame_objects: None,
            level: None,
            transition_zone_context: ObjectZoneContext::Null,
            level_state_context: None,
            current_solid_zone: None,
            saved_level_state: None,
            pending_first_spawn: false,
            suppress_initial_crash_save: false,
            restricted_direct_boot_save: RestrictedDirectBootSave::Disabled,
            retained_doctor_pool_pointer: None,
            respawn_count: 0,
            death_count: 0,
            frame_index: 0,
            draw_count: 0,
            level_shader: RetailLevelShaderState::default(),
            box_spawn: RetailBoxSpawnState::default(),
            core_objects_initialized: false,
            core_objects: None,
            level_misc_object: RetailLevelMiscObjectState::Uninitialized,
            pause: RetailPauseState::default(),
            title: None,
        }
    }

    /// Creates a production retail runtime with the level/read-only GOOL
    /// globals initialized before the first entity program can execute.
    #[must_use]
    pub fn new_for_level(global_words: usize, level: LevelId) -> Self {
        let mut runtime = Self::new(global_words);
        runtime.level = Some(level);
        runtime.machine.initialize_retail_level_globals(level);
        // The first browser mount has no session-carry constructor to run the
        // source `LdatInit`/`LevelInitMisc` scalar boundary. Publish the same
        // pointer clears, display words, counters, and fade step before the
        // initial entity scan; remounts already take this path through
        // `new_from_session`.
        runtime.apply_stream_mount_globals(level);
        runtime.restricted_direct_boot_save = RestrictedDirectBootSave::Armed;
        runtime
    }

    /// Rebuilds pair-owned state around exact process-lifetime scalar state.
    ///
    /// Object identities, spawn words, paging, frame effects, and all native
    /// pointer globals start empty. The destination host subsequently installs
    /// its camera/zone context and republishes card metadata before GOOL runs.
    pub fn new_from_session(
        global_words: usize,
        level: LevelId,
        carry: RetailSessionCarry,
    ) -> Result<Self, RetailSessionImportError> {
        if carry.globals.len() != global_words {
            return Err(RetailSessionImportError::GlobalWordCount {
                expected: global_words,
                actual: carry.globals.len(),
            });
        }
        if carry.level_spawn_tags.len() != RETAIL_LEVEL_SPAWN_CAPACITY {
            return Err(RetailSessionImportError::LevelSpawnTagCount {
                expected: RETAIL_LEVEL_SPAWN_CAPACITY,
                actual: carry.level_spawn_tags.len(),
            });
        }
        let RetailSessionCarry {
            globals,
            level_spawn_tags,
            saved_level_state,
            random_seed,
            draw_count,
            respawn_count,
            death_count,
            first_spawn,
            level_shader,
        } = carry;
        let mut runtime = Self::new(global_words);
        runtime.level_shader = level_shader;
        runtime.machine.restore_global_words(globals);
        runtime
            .machine
            .restore_retail_level_spawn_tags(level_spawn_tags);
        runtime.machine.initialize_retail_level_spawn_flags(level);
        runtime
            .arena
            .spawn_table_mut()
            .restore(runtime.machine.retail_spawn_flags_snapshot());
        runtime.level = Some(level);
        runtime.saved_level_state = saved_level_state;
        runtime.pending_first_spawn = first_spawn;
        runtime.respawn_count = respawn_count;
        runtime.death_count = death_count;
        runtime.machine.set_random_seed(random_seed);
        runtime.apply_stream_mount_globals(level);
        runtime.draw_count = draw_count;
        runtime.machine.set_draw_count(draw_count);
        Ok(runtime)
    }

    /// Captures every process-lifetime value that native retains across
    /// `NSKill`/`NSInit`, without retaining an arena, VM object, or pair-owned
    /// reference.
    #[must_use]
    pub fn export_session_carry(&self) -> RetailSessionCarry {
        let globals = self.machine.global_words().to_vec();
        let respawn_count = globals
            .get(RESPAWN_COUNT_GLOBAL)
            .copied()
            .unwrap_or(self.respawn_count);
        let death_count = globals
            .get(DEATH_COUNT_GLOBAL)
            .copied()
            .unwrap_or(self.death_count);
        RetailSessionCarry {
            globals: globals.into_boxed_slice(),
            level_spawn_tags: self
                .machine
                .retail_level_spawn_tags()
                .to_vec()
                .into_boxed_slice(),
            saved_level_state: self.saved_level_state.clone(),
            random_seed: self.machine.random_seed(),
            draw_count: self.draw_count,
            respawn_count,
            death_count,
            first_spawn: self.pending_first_spawn
                || self
                    .level_state_context
                    .as_ref()
                    .is_some_and(|context| context.first_spawn),
            level_shader: self.level_shader,
        }
    }

    fn set_mount_global(&mut self, index: usize, value: u32) {
        if index < self.machine.global_words().len() {
            self.machine
                .set_global_word(index, value)
                .expect("mount global index was checked");
        }
    }

    /// Clears native's per-activation crate scan state before the next ordered
    /// neighbor/entity pass. Raw ZDAT and object pointers are represented by
    /// owned data and checked handles internally; their exposed GOOL words are
    /// cleared or published as tagged object references.
    pub fn reset_retail_box_spawn_state(&mut self) {
        self.box_spawn = RetailBoxSpawnState::default();
        self.set_mount_global(PREVIOUS_BOX_GLOBAL, 0);
        self.set_mount_global(BOXES_Y_GLOBAL, BOX_STACK_SPACING as u32);
        self.set_mount_global(PREVIOUS_BOX_ENTITY_GLOBAL, 0);
    }

    fn apply_stream_mount_globals(&mut self, level: LevelId) {
        self.level_shader.initialize(level);
        self.retained_doctor_pool_pointer = None;
        for index in POINTER_GLOBALS {
            self.set_mount_global(index, 0);
        }
        for index in [
            CURRENT_ZONE_FLAGS_GLOBAL, // republished by the destination LevelUpdate
            37,
            38,
            39, // camera translation
            40,
            41,
            42, // camera rotation
            43,
            44,
            45,  // frame ticks and screen words
            62,  // box_count
            65,  // gem_stamp
            66,  // island_cam_state
            74,  // title_pause_state
            79,  // draw_count_ro
            105, // pbak_state
        ] {
            self.set_mount_global(index, 0);
        }
        // The destination's initial LevelUpdate activates a fresh neighbor
        // band and establishes the first box stack offset before entity scan.
        self.reset_retail_box_spawn_state();
        self.set_mount_global(CURRENT_LEVEL_GLOBAL, level.get() << 8);
        self.set_mount_global(NEXT_DISPLAY_GLOBAL, INITIAL_DISPLAY_MASK);
        self.set_mount_global(CURRENT_DISPLAY_GLOBAL, INITIAL_DISPLAY_MASK);
        self.set_mount_global(RESPAWN_COUNT_GLOBAL, self.respawn_count);
        self.set_mount_global(DEATH_COUNT_GLOBAL, self.death_count);
        self.set_mount_global(67, 1); // is_first_zone
        self.set_mount_global(107, 32); // fade_step
        if self
            .machine
            .global_word(GAME_STATE_GLOBAL)
            .is_ok_and(|state| state != 0x600)
        {
            self.set_mount_global(106, 288); // fade_counter
        }
        if level == LevelId::TITLE {
            self.respawn_count = 0;
            self.death_count = 0;
            for (index, value) in [
                (RESPAWN_COUNT_GLOBAL, 0),
                (DEATH_COUNT_GLOBAL, 0),
                (CORTEX_COUNT_GLOBAL, 0),
                (BRIO_COUNT_GLOBAL, 0),
                (TAWNA_COUNT_GLOBAL, 0),
                (CHECKPOINT_ID_GLOBAL, u32::MAX),
            ] {
                self.set_mount_global(index, value);
            }
        }
    }

    #[must_use]
    pub const fn arena(&self) -> &ObjectArena {
        &self.arena
    }

    #[must_use]
    pub const fn machine(&self) -> &Machine {
        &self.machine
    }

    /// Checked access to one pointer-free retail global word.
    pub fn global_word(&self, index: usize) -> Result<u32, VmError> {
        self.machine.global_word(index)
    }

    /// Checked mutation of one pointer-free retail global word.
    pub fn set_global_word(&mut self, index: usize, value: u32) -> Result<(), VmError> {
        self.machine.set_global_word(index, value)?;
        match index {
            RESPAWN_COUNT_GLOBAL => self.respawn_count = value,
            DEATH_COUNT_GLOBAL => self.death_count = value,
            _ => {}
        }
        Ok(())
    }

    /// Seeds mounted load-list resolution and shared page-reference state
    /// before the first browser GOOL frame.
    pub fn seed_platform_paging_state(
        &mut self,
        page_count: u32,
        resolved_pages: impl IntoIterator<Item = PageIndex>,
        page_references: impl IntoIterator<Item = (PageIndex, u32)>,
    ) -> Result<(), VmError> {
        self.machine
            .seed_platform_paging_state(page_count, resolved_pages, page_references)
    }

    /// Browser seed that keeps texture-cache references outside native's
    /// twenty-two ordinary-page availability count.
    pub fn seed_platform_paging_state_with_uncounted_pages(
        &mut self,
        page_count: u32,
        resolved_pages: impl IntoIterator<Item = PageIndex>,
        page_references: impl IntoIterator<Item = (PageIndex, u32)>,
        uncounted_pages: impl IntoIterator<Item = PageIndex>,
    ) -> Result<(), VmError> {
        self.machine
            .seed_platform_paging_state_with_uncounted_pages(
                page_count,
                resolved_pages,
                page_references,
                uncounted_pages,
            )
    }

    /// Seeds both the NSF catalog and the smaller heap-derived PS1 page pool.
    pub fn seed_platform_paging_state_with_capacity(
        &mut self,
        page_count: u32,
        physical_page_capacity: u32,
        resolved_pages: impl IntoIterator<Item = PageIndex>,
        page_references: impl IntoIterator<Item = (PageIndex, u32)>,
        uncounted_pages: impl IntoIterator<Item = PageIndex>,
    ) -> Result<(), VmError> {
        self.machine.seed_platform_paging_state_with_capacity(
            page_count,
            physical_page_capacity,
            resolved_pages,
            page_references,
            uncounted_pages,
        )
    }

    /// Seeds virtual requests retained by the platform pager at mount time.
    pub fn seed_platform_pending_pages(
        &mut self,
        pages: impl IntoIterator<Item = PageIndex>,
    ) -> Result<(), VmError> {
        self.machine.seed_platform_pending_pages(pages)
    }

    /// Applies one browser lifecycle open outside a GOOL instruction.
    pub fn apply_platform_paging_open(
        &mut self,
        page: PageIndex,
        invalidated: PageInvalidations,
    ) -> Result<(), VmError> {
        self.machine.apply_platform_paging_open(page, invalidated)
    }

    /// Retains one lifecycle-owned flag-zero page in the virtual queue.
    pub fn apply_platform_paging_queued_open(&mut self, page: PageIndex) -> Result<(), VmError> {
        self.machine.apply_platform_paging_queued_open(page)
    }

    /// Publishes one successful frame-boundary `NSUpdate(-1)` promotion.
    pub fn apply_platform_paging_resolution(
        &mut self,
        page: PageIndex,
        invalidated: PageInvalidations,
    ) -> Result<(), VmError> {
        self.machine
            .apply_platform_paging_resolution(page, invalidated)
    }

    /// Publishes every zero-reference PTE re-armed by one CD-group reservation.
    pub fn apply_platform_paging_evictions(&mut self, pages: &[PageIndex]) -> Result<(), VmError> {
        self.machine.apply_platform_paging_evictions(pages)
    }

    /// Applies one browser lifecycle close outside a GOOL instruction.
    pub fn apply_platform_paging_close(
        &mut self,
        page: PageIndex,
        decremented: bool,
        unresolved: bool,
    ) -> Result<(), VmError> {
        self.machine
            .apply_platform_paging_close(page, decremented, unresolved)
    }

    /// Advances every source `ShaderParamsUpdate(0)` case at the unpaused
    /// pre-camera boundary. Zones without Dark2/Lightning flags deliberately
    /// leave the process-global state and its separate RNG untouched.
    pub fn advance_level_shader(&mut self) -> Result<(), VmError> {
        self.advance_level_shader_at(0).map(|_| ())
    }

    /// Timestamp-aware form used by the browser for the two-stamp thunder
    /// cooldown. `ticks_elapsed` is the pause-adjusted native clock.
    pub fn advance_level_shader_at(
        &mut self,
        ticks_elapsed: u32,
    ) -> Result<Option<RetailThunderCue>, VmError> {
        let Some(level) = self.level else {
            return Ok(None);
        };
        let graphics_flags = self
            .level_state_context
            .as_ref()
            .map_or(0, |context| context.graphics_flags);
        if graphics_flags & 0x600 == 0 {
            return Ok(None);
        }
        let light_source = self.machine.global_word(LIGHT_SOURCE_OBJECT_GLOBAL)?;
        let dark2_illumination = if matches!(level.get(), 0x28 | 0x2a) {
            self.current_world_dark2_illumination()?
        } else {
            None
        };
        Ok(self
            .level_shader
            .advance(level, ticks_elapsed, light_source, dark2_illumination))
    }

    /// Frozen shader globals to capture immediately after the pre-camera
    /// update and before current-frame GOOL can mutate object globals.
    #[must_use]
    pub const fn world_shader_snapshot(&self) -> RetailWorldShaderSnapshot {
        self.level_shader.snapshot()
    }

    /// Native process-global `seed_b`, shared by dynamic lighting, PBAK
    /// selection, and audio voice allocation.
    #[must_use]
    pub const fn random_seed_b(&self) -> u32 {
        self.level_shader.random_seed_b
    }

    /// Publishes draws performed by an external process-lifetime subsystem
    /// back into the shader/session owner before the next native boundary.
    pub const fn set_random_seed_b(&mut self, seed: u32) {
        self.level_shader.random_seed_b = seed;
    }

    /// Captures the exact persistent globals serialized by memory-card saves.
    pub fn card_save_data(&self) -> Result<SaveData, VmError> {
        self.machine.retail_card_save_data()
    }

    /// Publishes card metadata before the next GOOL traversal.
    pub fn publish_card_state(&mut self, state: CardPublishedState) -> Result<(), VmError> {
        self.machine.publish_retail_card_state(state)
    }

    /// Applies a restored retail payload to a newly mounted or active runtime.
    pub fn restore_card_save_data(&mut self, save: SaveData) -> Result<(), VmError> {
        self.apply_loaded_card_save(save)
    }

    /// Applies the exact browser-relevant `LevelResetGlobals(1)` transaction.
    ///
    /// The retained savestate and 304-word active spawn table are deliberately
    /// preserved. Native clears only its separate 3,592-halfword encountered-
    /// object registry plus the documented scalar globals.
    pub fn reset_level_globals(&mut self) -> Result<(), VmError> {
        self.machine.reset_retail_level_globals()?;
        self.sync_level_reset_mirrors();
        Ok(())
    }

    /// Reapplies the protected browser-resume payload after title state five
    /// has run [`Self::reset_level_globals`]. This matches
    /// `CardBrowserResumeAfterTitleReset`: initial lives are installed, the
    /// exact globals reset runs a second time, then only payload progression
    /// and options are restored. Savestate and active spawn words survive.
    pub fn restore_resume_after_title_reset(&mut self, save: SaveData) -> Result<(), VmError> {
        self.machine.restore_retail_resume_save_data(save)?;
        self.sync_level_reset_mirrors();
        Ok(())
    }

    fn sync_level_reset_mirrors(&mut self) {
        self.respawn_count = 0;
        self.death_count = 0;
        if let Some(context) = &mut self.level_state_context {
            context.checkpoint_id = -1;
        }
        self.machine.acknowledge_level_state_context();
    }

    /// Exact representable scalar/object-latch body of `LevelInitMisc(0)`.
    ///
    /// The machine-owned, pointer-free `cur_zone_query` cache is invalidated
    /// here beside native smooth-stop history, rebuilt by the next solid-floor
    /// query, and thereafter retained until a strict-bound escape.
    /// Only levels 0x28/0x2a call `ShaderParamsUpdate(1)` at this boundary.
    /// Its targets, steps, current distance, timestamps, and RNG retain their
    /// process-global values; other levels retain the whole shader sequence.
    fn apply_level_init_misc_zero(&mut self, level: LevelId) {
        if matches!(level.get(), 0x28 | 0x2a) {
            self.level_shader.initialize(level);
        }
        // Cases with a level-specific branch do not execute the default
        // `ambiance_obj = 0` assignment when flag is zero.
        if !matches!(
            level.get(),
            0x05 | 0x0e | 0x14 | 0x16 | 0x17 | 0x22 | 0x28 | 0x2a | 0x2e
        ) {
            self.set_mount_global(AMBIANCE_OBJECT_GLOBAL, 0);
        }
        for (index, value) in [
            (IS_FIRST_ZONE_GLOBAL, 1),
            (BOX_COUNT_GLOBAL, 0),
            (GEM_STAMP_GLOBAL, 0),
            (ISLAND_CAMERA_STATE_GLOBAL, 0),
            (TITLE_PAUSE_STATE_GLOBAL, 0),
            (FADE_STEP_GLOBAL, 32),
        ] {
            self.set_mount_global(index, value);
        }
        if !self
            .machine
            .global_word(PBAK_STATE_GLOBAL)
            .is_ok_and(|state| state == 2)
        {
            self.set_mount_global(CAPTION_OBJECT_GLOBAL, 0);
        }
        self.machine.reset_retail_solid_smoothing();
        if !self
            .machine
            .global_word(GAME_STATE_GLOBAL)
            .is_ok_and(|state| state == 0x600)
        {
            self.set_mount_global(FADE_COUNTER_GLOBAL, 288);
        }
    }

    /// Takes a successful authored card load exactly once so the browser flow
    /// and audio mirrors can synchronize with the already-restored VM.
    pub fn take_card_load(&mut self) -> Option<SaveData> {
        let loaded = self.machine.take_completed_card_load();
        if loaded.is_some() {
            self.sync_level_reset_mirrors();
        }
        loaded
    }

    fn apply_loaded_card_save(&mut self, save: SaveData) -> Result<(), VmError> {
        self.machine.restore_retail_card_save_data(save)?;
        self.sync_level_reset_mirrors();
        Ok(())
    }

    /// Level identity used by lifecycle-only contracts such as Crash's title
    /// teardown exception. Authored runtimes made with [`Self::new`] retain
    /// `None`, which is treated as non-title.
    #[must_use]
    pub const fn level(&self) -> Option<LevelId> {
        self.level
    }

    /// Frame stamp written by authored gem pickup logic and consumed by the
    /// following native `CamUpdate` gem-path gate.
    pub fn gem_stamp(&self) -> Result<u32, VmError> {
        self.machine.global_word(GEM_STAMP_GLOBAL)
    }

    /// Installs the C-side title state for a newly mounted title stream.
    ///
    /// `first_boot` selects native transition state zero; subsequent title
    /// mounts enter through blank state one. Both paths seed the exact live
    /// fade globals and leave screen-specific display flags to `TitleLoadState`.
    pub fn configure_retail_title(
        &mut self,
        screen: TitleScreen,
        first_boot: bool,
    ) -> Result<(), RetailTitleError> {
        self.machine
            .set_global_word(TITLE_STATE_GLOBAL, screen.raw())?;
        self.machine
            .set_global_word(FADE_COUNTER_GLOBAL, TITLE_FADE_START as u32)?;
        self.machine
            .set_global_word(FADE_STEP_GLOBAL, TITLE_FADE_STEP as u32)?;
        self.title = Some(RetailTitleState {
            screen,
            next_screen: screen,
            phase: if first_boot {
                TitlePhase::Start
            } else {
                TitlePhase::Blank
            },
            opaque_swap_overlay: false,
        });
        Ok(())
    }

    /// Returns the authoritative title presentation after the latest display
    /// boundary, or `None` for non-title runtimes.
    pub fn retail_title_presentation(
        &self,
    ) -> Result<Option<RetailTitlePresentation>, RetailTitleError> {
        let Some(title) = self.title else {
            return Ok(None);
        };
        let fade_counter = self.machine.global_word(FADE_COUNTER_GLOBAL)? as i32;
        Ok(Some(RetailTitlePresentation {
            screen: title.screen,
            next_screen: title.next_screen,
            phase: title.phase,
            opaque_swap_overlay: title.opaque_swap_overlay,
            fade_counter,
        }))
    }

    /// Runs native `TitleUpdate` through the optional `TitleLoadState` call.
    ///
    /// Call this after GOOL traversal and before `GLUpdate`. If it returns a
    /// load action, the host must apply it before calling
    /// [`Self::finish_retail_title_update`].
    pub fn begin_retail_title_update(
        &mut self,
    ) -> Result<Option<RetailTitleAction>, RetailTitleError> {
        let mut title = self.title.ok_or(RetailTitleError::NotConfigured)?;
        let fade_counter = self.machine.global_word(FADE_COUNTER_GLOBAL)? as i32;
        // The previous frame's immediate GL draw remains visible until this
        // source TitleUpdate boundary. A swap below republishes it for the
        // new frame before the final authored-state comparison can change the
        // transition phase again.
        title.opaque_swap_overlay = false;
        let mut action = None;
        match title.phase {
            TitlePhase::Start | TitlePhase::Blank => {
                let display = self.machine.global_word(NEXT_DISPLAY_GLOBAL)?;
                self.machine.set_global_word(
                    NEXT_DISPLAY_GLOBAL,
                    display | DISPLAY_OBJECTS | ANIMATE_OBJECTS,
                )?;
                self.machine
                    .set_global_word(FADE_COUNTER_GLOBAL, TITLE_FADE_START as u32)?;
                title.phase = TitlePhase::FadingIn;
            }
            TitlePhase::FadingOut if fade_counter == 0 => {
                let display = self.machine.global_word(NEXT_DISPLAY_GLOBAL)?;
                self.machine.set_global_word(
                    NEXT_DISPLAY_GLOBAL,
                    display & !(DISPLAY_OBJECTS | ANIMATE_OBJECTS),
                )?;
                title.phase = TitlePhase::FinishedFadingOut;
                title.opaque_swap_overlay = true;
            }
            TitlePhase::FadingIn if fade_counter == 0 => {
                title.phase = TitlePhase::Ready;
            }
            TitlePhase::FinishedFadingOut => {
                let previous = title.screen;
                title.screen = title.next_screen;
                title.phase = TitlePhase::Blank;
                title.opaque_swap_overlay = true;
                action = Some(RetailTitleAction::LoadScreen {
                    previous,
                    screen: title.screen,
                });
            }
            TitlePhase::Ready | TitlePhase::FadingIn | TitlePhase::FadingOut => {}
        }
        self.title = Some(title);
        Ok(action)
    }

    /// Completes native `TitleUpdate` after any synchronous screen load.
    ///
    /// This final comparison is intentionally separate: an authored object
    /// initialized by `TitleLoadState` may write global 18 before the source
    /// checks it and starts another fade.
    pub fn finish_retail_title_update(&mut self) -> Result<(), RetailTitleError> {
        let mut title = self.title.ok_or(RetailTitleError::NotConfigured)?;
        let raw = self.machine.global_word(TITLE_STATE_GLOBAL)?;
        let requested =
            TitleScreen::from_raw(raw).ok_or(RetailTitleError::InvalidTitleState(raw))?;
        if title.next_screen != requested {
            self.machine
                .set_global_word(FADE_COUNTER_GLOBAL, (-256_i32) as u32)?;
            title.next_screen = requested;
            title.phase = TitlePhase::FadingOut;
        }
        self.title = Some(title);
        Ok(())
    }

    /// GOOL display/animation word currently consumed by object/render logic.
    ///
    /// Authored zero-global runtimes retain the historical all-enabled
    /// fallback; stream-backed runtimes always contain the exact global.
    #[must_use]
    pub fn current_display_mask(&self) -> u32 {
        self.machine
            .global_word(CURRENT_DISPLAY_GLOBAL)
            .unwrap_or(INITIAL_DISPLAY_MASK)
    }

    /// Counter used by world textures and GOOL during the current frame.
    #[must_use]
    pub const fn draw_count(&self) -> u32 {
        self.draw_count
    }

    /// Completes the source `GLUpdate` display/draw-count boundary.
    ///
    /// Normal [`Self::run_frame`] calls finish this automatically. A paused
    /// host that deliberately skips GOOL can call it with `paused = true` to
    /// keep title/pause-authored mask writes synchronized without incrementing
    /// the draw counter.
    pub fn finish_display_frame(&mut self, paused: bool) -> Result<u32, VmError> {
        let display_mask = if self.machine.global_word(CURRENT_DISPLAY_GLOBAL).is_ok() {
            let next = self.machine.global_word(NEXT_DISPLAY_GLOBAL)?;
            self.machine.set_global_word(CURRENT_DISPLAY_GLOBAL, next)?;
            next
        } else {
            INITIAL_DISPLAY_MASK
        };
        if !paused && display_mask & 0x1000 != 0 {
            self.draw_count = self.draw_count.wrapping_add(1);
        }
        self.machine.set_draw_count(self.draw_count);
        self.advance_native_display_fade(display_mask)?;
        Ok(display_mask)
    }

    /// Completes the deferred native `GLUpdate` boundary after a host has run
    /// title work between GOOL and display latching.
    ///
    /// A level-restart request suppresses this boundary exactly as the ordinary
    /// one-shot frame runner does.
    pub fn finish_deferred_display_frame(&mut self) -> Result<Option<u32>, VmError> {
        if self.machine.level_restart_requested() {
            return Ok(None);
        }
        self.finish_display_frame(self.pause.paused).map(Some)
    }

    /// Advances the signed global brightness state at the same end-of-frame
    /// boundary as native `GLUpdate`.
    ///
    /// GOOL owns both words and waits on the two negative terminal sentinels,
    /// so this cannot be replaced by a renderer-only interpolation. The
    /// renderer may consume the corresponding brightness independently; the
    /// simulation must first publish the exact next value to authored code.
    fn advance_native_display_fade(&mut self, display_mask: u32) -> Result<(), VmError> {
        if self.machine.global_words().len() <= FADE_STEP_GLOBAL {
            return Ok(());
        }
        let counter = self.machine.global_word(FADE_COUNTER_GLOBAL)? as i32;
        if counter == 0 {
            return Ok(());
        }
        let step = self.machine.global_word(FADE_STEP_GLOBAL)? as i32;
        let next = if counter < -2 {
            let advanced = counter.wrapping_add(step);
            if advanced == 0 && display_mask & 0x20_0000 == 0 {
                -2
            } else {
                advanced
            }
        } else if counter < 0 {
            -1
        } else {
            counter.wrapping_sub(step)
        };
        self.machine
            .set_global_word(FADE_COUNTER_GLOBAL, next as u32)
    }

    #[must_use]
    pub const fn frame_index(&self) -> u64 {
        self.frame_index
    }

    /// Low native word that the next cooperative GOOL frame publishes as
    /// `frames_elapsed`. Unlike [`Self::draw_count`], this advances while the
    /// authored display-count bit is clear and is rewound by pause resume.
    #[must_use]
    pub fn next_frame_stamp(&self) -> u32 {
        wrapping_frame_stamp(self.frame_index)
    }

    /// Current host-side pause latch. The authored controller is still a
    /// normal arena/VM object and can be inspected through render snapshots.
    #[must_use]
    pub const fn retail_pause_state(&self) -> RetailPauseState {
        self.pause
    }

    /// Exact `CoreFrame` level/title/PBAK gate for a START pause toggle.
    pub fn can_retail_pause(&self) -> Result<bool, VmError> {
        let Some(level) = self.level else {
            return Ok(false);
        };
        let title_pause_state = self.machine.global_word(TITLE_PAUSE_STATE_GLOBAL)? as i32;
        let pbak_state = self.machine.global_word(PBAK_STATE_GLOBAL)?;
        let ordinary_pause_level = !matches!(
            level,
            LevelId::TITLE | LevelId::LEVEL_COMPLETE | LevelId::INTRO
        );
        Ok(pbak_state == 0
            && ((ordinary_pause_level && title_pause_state != -1) || title_pause_state > 0))
    }

    /// Performs native `CoreFrame`'s pause check using the pad snapshot from
    /// the previous Crash traversal.
    ///
    /// Call this before level-transition handling, spawning, camera work, and
    /// [`Self::run_frame`]. A successful resume clears global word twelve and
    /// rewinds the GOOL frame clock immediately; the state-seven controller is
    /// killed when its ordinary top-level return is observed later in the same
    /// traversal. Checked event faults are recorded but do not prevent resume,
    /// matching the source's ignored `GoolSendEvent` result.
    pub fn update_retail_pause<H: ProgramHost>(
        &mut self,
        start_tapped: bool,
        current_zone: Eid,
        host: &mut H,
    ) -> Result<RetailPauseUpdate, RuntimeError<H::Error>> {
        if !start_tapped {
            self.pause.status = 0;
            return Ok(RetailPauseUpdate::Unchanged);
        }
        if !self.can_retail_pause().map_err(RuntimeError::Vm)? {
            self.pause.status = 0;
            return Ok(RetailPauseUpdate::Blocked);
        }

        if !self.pause.paused {
            // Preflight the pointer-global slot so a deliberately undersized
            // runtime cannot allocate a controller it is unable to publish.
            self.machine
                .global_word(PAUSE_OBJECT_GLOBAL)
                .map_err(RuntimeError::Vm)?;
            let controller = match self.create_root_program(
                PAUSE_CONTROLLER_ROOT,
                current_zone,
                PAUSE_CONTROLLER_EXECUTABLE,
                PAUSE_CONTROLLER_SUBTYPE,
                &[],
                false,
                host,
            ) {
                Ok(controller) => controller,
                Err(_error) => {
                    self.pause = RetailPauseState::default();
                    self.machine
                        .set_global_word(PAUSE_OBJECT_GLOBAL, 0)
                        .map_err(RuntimeError::Vm)?;
                    // Native discards the controller-creation error and
                    // continues the frame with pause_status/paused cleared.
                    return Ok(RetailPauseUpdate::Failed);
                }
            };
            self.machine
                .set_global_word(
                    PAUSE_OBJECT_GLOBAL,
                    CollisionObjectReference::new(controller.vm).to_word(),
                )
                .map_err(RuntimeError::Vm)?;
            self.pause = RetailPauseState {
                paused: true,
                status: 1,
                controller: Some(controller),
                saved_frame_index: Some(self.frame_index),
            };
            return Ok(RetailPauseUpdate::Paused { controller });
        }

        // Native flips `paused` before delivering C00, so event-authored work
        // observes the unpaused host state and the following traversal uses the
        // ordinary update flag.
        self.pause.paused = false;
        let controller = self
            .pause
            .controller
            .filter(|controller| self.handles.is_live_pair(*controller));
        let event_faulted = controller.is_some_and(|controller| {
            if self
                .dispatch_event(host, None, Some(controller), PAUSE_RESUME_EVENT, Some(&[0]))
                .is_err()
            {
                self.pause_event_faults
                    .push(RuntimePauseEventFault { object: controller });
                true
            } else {
                false
            }
        });
        self.machine
            .set_global_word(PAUSE_OBJECT_GLOBAL, 0)
            .map_err(RuntimeError::Vm)?;
        self.pause.status = -1;
        if let Some(saved_frame_index) = self.pause.saved_frame_index.take() {
            self.frame_index = saved_frame_index;
        }
        Ok(RetailPauseUpdate::Resumed {
            controller,
            event_faulted,
        })
    }

    /// Clears the host latch at native screen-load/remount boundaries. The
    /// caller must immediately perform its ordinary all-object teardown; this
    /// method deliberately does not invent a TERM event for the controller.
    pub fn reset_retail_pause_for_screen_load(&mut self) -> Result<(), VmError> {
        self.pause = RetailPauseState {
            status: -1,
            ..RetailPauseState::default()
        };
        if self.machine.global_words().len() > PAUSE_OBJECT_GLOBAL {
            self.machine.set_global_word(PAUSE_OBJECT_GLOBAL, 0)?;
        }
        Ok(())
    }

    /// Refreshes the pointer-free globals consumed by retail save/restart.
    pub fn set_level_state_context(&mut self, mut context: RetailLevelStateContext) {
        if self.pending_first_spawn {
            context.first_spawn = true;
            self.pending_first_spawn = false;
        }
        // Native LevelUpdate publishes this scalar before the following GOOL
        // spawn/update pass. Bonus WARP state 32 reads bit 0x2000 to select
        // LoadState instead of the ordinary Title transition.
        self.set_mount_global(CURRENT_ZONE_FLAGS_GLOBAL, context.graphics_flags);
        self.level_state_context = Some(context);
        self.machine.acknowledge_level_state_context();
    }

    #[must_use]
    pub const fn level_state_context(&self) -> Option<&RetailLevelStateContext> {
        self.level_state_context.as_ref()
    }

    #[must_use]
    pub const fn saved_level_state(&self) -> Option<&RetailLevelSnapshot> {
        self.saved_level_state.as_ref()
    }

    /// Creates the process-lifetime HUD roots installed by native
    /// `CoreObjectsCreate` before the first ZDAT spawn scan.
    ///
    /// Gameplay, boss, bonus, and map streams receive executable-four life,
    /// fruit, and pickup controllers in that exact order. Title, level-end,
    /// intro, and ending streams only pre-open programs in native code and do
    /// not create these objects. Repeated calls on one mounted runtime are
    /// idempotent so a same-level restart cannot duplicate the roots.
    pub fn create_retail_core_objects<H: ProgramHost>(
        &mut self,
        current_zone: Eid,
        host: &mut H,
    ) -> Result<Option<RetailCoreObjects>, RuntimeError<H::Error>> {
        if self.core_objects_initialized {
            return Ok(self.core_objects);
        }
        let Some(level) = self.level else {
            self.core_objects_initialized = true;
            return Ok(None);
        };
        if matches!(
            level,
            LevelId::TITLE | LevelId::LEVEL_COMPLETE | LevelId::INTRO | LevelId::ENDING
        ) {
            self.core_objects_initialized = true;
            return Ok(None);
        }

        // Preflight the three pointer-global slots before allocating anything
        // so a deliberately undersized test VM stays transactionally empty.
        for global in [7, 6, 14] {
            self.machine.global_word(global).map_err(RuntimeError::Vm)?;
        }

        let mut created = Vec::with_capacity(3);
        for subtype in [0, 1, 5] {
            match self.create_root_program(1, current_zone, 4, subtype, &[], false, host) {
                Ok(object) => created.push(object),
                Err(error) => {
                    self.discard_unstarted_runtime_roots::<H::Error>(&created)?;
                    return Err(error);
                }
            }
        }
        let objects = RetailCoreObjects {
            life: created[0],
            fruit: created[1],
            pickup: created[2],
        };
        for (global, object) in [(7, objects.life), (6, objects.fruit), (14, objects.pickup)] {
            self.machine
                .set_global_word(global, CollisionObjectReference::new(object.vm).to_word())
                .map_err(RuntimeError::Vm)?;
        }
        self.core_objects = Some(objects);
        self.core_objects_initialized = true;
        Ok(Some(objects))
    }

    /// Creates the optional root-four controller installed by native
    /// `LevelInitMisc(1)` after [`Self::create_retail_core_objects`].
    ///
    /// Only six retail levels take an object-creating branch. The runtime
    /// child has no lifecycle ZDAT, receives the current zone only as its
    /// initialization environment, carries no arguments, and uses native's
    /// reclaiming allocation flag. Ripper Roo additionally publishes the
    /// controller through `ambiance_obj` (GOOL global eight). Repeated calls
    /// on one mounted pair are idempotent, matching the one mount-time call in
    /// `CoreObjectsCreate`; same-level restarts use `LevelInitMisc(0)` and do
    /// not create another controller.
    pub fn create_retail_level_misc_object<H: ProgramHost>(
        &mut self,
        current_zone: Eid,
        host: &mut H,
    ) -> Result<Option<RuntimeObjectHandle>, RuntimeError<H::Error>> {
        if let RetailLevelMiscObjectState::Initialized(object) = self.level_misc_object {
            return Ok(object);
        }
        let Some(level) = self.level else {
            self.level_misc_object = RetailLevelMiscObjectState::Initialized(None);
            return Ok(None);
        };
        let (executable, subtype, publish_ambiance) = match level.get() {
            0x05 => (9, 4, false),
            0x14 | 0x16 => (23, 6, false),
            0x17 => (39, 4, true),
            0x22 | 0x2e => (53, 13, false),
            _ => {
                self.level_misc_object = RetailLevelMiscObjectState::Initialized(None);
                return Ok(None);
            }
        };

        // Preflight Ripper Roo's pointer slot before allocation so a
        // deliberately undersized checked VM cannot leak a root object.
        if publish_ambiance {
            self.machine
                .global_word(AMBIANCE_OBJECT_GLOBAL)
                .map_err(RuntimeError::Vm)?;
        }
        let object = self.create_root_program(
            LEVEL_MISC_CONTROLLER_ROOT,
            current_zone,
            executable,
            subtype,
            &[],
            true,
            host,
        )?;
        if publish_ambiance {
            self.machine
                .set_global_word(
                    AMBIANCE_OBJECT_GLOBAL,
                    CollisionObjectReference::new(object.vm).to_word(),
                )
                .map_err(RuntimeError::Vm)?;
        }
        self.level_misc_object = RetailLevelMiscObjectState::Initialized(Some(object));
        Ok(Some(object))
    }

    fn discard_unstarted_runtime_roots<E>(
        &mut self,
        objects: &[RuntimeObjectHandle],
    ) -> Result<(), RuntimeError<E>> {
        let mut report = ZoneTerminationReport::new();
        for object in objects.iter().rev() {
            if self.handles.is_live_pair(*object) {
                self.remove_runtime_subtree(object.arena, &mut report)?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn create_root_program<H: ProgramHost>(
        &mut self,
        root_index: u8,
        binding_zone: Eid,
        executable: u8,
        subtype: u8,
        arguments: &[u32],
        allow_reclaim: bool,
        host: &mut H,
    ) -> Result<RuntimeObjectHandle, RuntimeError<H::Error>> {
        let root = RootHandle::new(root_index)
            .ok_or(RuntimeError::InvalidRootIndex(usize::from(root_index)))?;
        let arena_handle = loop {
            match self
                .arena
                .create_root_object(root, Eid::NONE, executable, subtype, allow_reclaim)
            {
                Ok(arena_handle) => break arena_handle,
                Err(RuntimeCreateError::ReclaimRequired(candidate)) => {
                    let mut spawned_children = Vec::new();
                    self.reclaim_runtime_subtree(candidate, host, &mut spawned_children)?;
                }
                Err(error) => return Err(RuntimeError::Create(error)),
            }
        };

        self.handles.prune_stale(&self.arena);
        let object = match self.handles.reserve(arena_handle) {
            Ok(object) => object,
            Err(error) => {
                self.arena
                    .despawn_subtree(arena_handle)
                    .map_err(RuntimeError::Tree)?;
                return Err(error);
            }
        };
        let binding = ProgramBinding {
            object,
            zone: binding_zone,
            executable,
            subtype,
            origin: ProgramOrigin::RuntimeChild { arguments },
        };
        if let Err(error) = self.materialize(binding, host) {
            self.handles.release(object);
            self.arena
                .despawn_subtree(arena_handle)
                .map_err(RuntimeError::Tree)?;
            return Err(error);
        }
        Ok(object)
    }

    /// Creates the process-lifetime caption controller installed by native
    /// `PbakPlay` before its same-level `LevelRestart`.
    ///
    /// The object is executable four, subtype eight, receives the exact two
    /// caption arguments, and is inserted beneath logical root one with the
    /// reclaiming `GoolObjectAlloc(1)` policy. Executable four clears its
    /// native `obj->zone` pointer during initialization, so the arena retains
    /// [`Eid::NONE`] for lifecycle decisions while the supplied current ZDAT
    /// still provides its initial colors and solid environment.
    pub fn create_retail_demo_caption<H: ProgramHost>(
        &mut self,
        current_zone: Eid,
        host: &mut H,
    ) -> Result<RuntimeObjectHandle, RuntimeError<H::Error>> {
        self.create_root_program(
            1,
            current_zone,
            PBAK_CAPTION_EXECUTABLE,
            PBAK_CAPTION_SUBTYPE,
            &PBAK_CAPTION_ARGUMENTS,
            true,
            host,
        )
    }

    /// Installs the process-lifetime state written by native `PbakStart`.
    ///
    /// Pair parsing and path resolution happen in the browser host before this
    /// call. The runtime still validates the mounted level and live Crash
    /// handle before publishing any mutation. `restart_saved_level` performs
    /// the following source-ordered `LevelRestart` transaction.
    pub fn install_retail_demo_start(
        &mut self,
        snapshot: RetailLevelSnapshot,
        random_seed: u32,
        crash_bound: Bounds3,
    ) -> Result<(), RetailDemoStartError> {
        let mounted = self.level.ok_or(RetailDemoStartError::MissingLevel)?;
        if snapshot.level != mounted {
            return Err(RetailDemoStartError::LevelMismatch {
                mounted,
                recorded: snapshot.level,
            });
        }
        if self.level_state_context.is_none() {
            return Err(RetailDemoStartError::MissingLevelStateContext);
        }
        let main_arena = self
            .arena
            .main_object()
            .ok_or(RetailDemoStartError::MissingMainObject)?;
        let main = self
            .handles
            .for_arena(main_arena)
            .filter(|handle| self.handles.is_live_pair(*handle))
            .ok_or(RetailDemoStartError::MissingMainObject)?;
        for index in [CHECKPOINT_ID_GLOBAL, PBAK_STATE_GLOBAL] {
            self.machine
                .global_word(index)
                .map_err(RetailDemoStartError::Vm)?;
        }
        self.machine
            .object(main.vm)
            .map_err(RetailDemoStartError::Vm)?;

        self.saved_level_state = Some(snapshot);
        self.machine.set_random_seed(random_seed);
        self.machine
            .object_mut(main.vm)
            .map_err(RetailDemoStartError::Vm)?
            .set_retail_local_bound(crash_bound);
        self.machine
            .set_global_word(CHECKPOINT_ID_GLOBAL, u32::MAX)
            .map_err(RetailDemoStartError::Vm)?;
        self.machine
            .set_global_word(PBAK_STATE_GLOBAL, 2)
            .map_err(RetailDemoStartError::Vm)?;
        if let Some(context) = self.level_state_context.as_mut() {
            context.checkpoint_id = -1;
        }
        self.machine.acknowledge_level_state_context();
        Ok(())
    }

    /// Completes native `PadUpdatePbak` after the final recorded word or a
    /// physical interruption has already been made observable to GOOL.
    ///
    /// A zero island-camera target releases playback immediately. Otherwise
    /// global `caption_obj` must decode to a live generational Rust handle;
    /// event `0xE00` with one zero argument is delivered synchronously before
    /// `pbak_state` is latched to three. Partial/native pointer corruption is
    /// rejected rather than dereferenced.
    pub fn finish_retail_demo<H: ProgramHost>(
        &mut self,
        host: &mut H,
    ) -> Result<RetailDemoFinishOutcome, RuntimeError<H::Error>> {
        let island_rotation = self
            .machine
            .global_word(ISLAND_CAMERA_ROTATION_GLOBAL)
            .map_err(RuntimeError::Vm)?;
        self.machine
            .set_global_word(PBAK_STATE_GLOBAL, 0)
            .map_err(RuntimeError::Vm)?;
        if island_rotation == 0 {
            return Ok(RetailDemoFinishOutcome::Released);
        }

        let caption_word = self
            .machine
            .global_word(CAPTION_OBJECT_GLOBAL)
            .map_err(RuntimeError::Vm)?;
        if caption_word == 0 {
            return Err(RuntimeError::MissingDemoCaptionObject);
        }
        let reference = CollisionObjectReference::from_word(caption_word).ok_or(
            RuntimeError::InvalidGlobalObjectReference {
                global: CAPTION_OBJECT_GLOBAL,
                value: caption_word,
            },
        )?;
        let recipient = self
            .handles
            .for_vm(reference.object())
            .ok_or(RuntimeError::UnknownVmObject(reference.object()))?;
        Self::validate_runtime_object(&self.arena, &self.handles, &self.machine, recipient)?;

        let effects_start = self.machine.effects().len();
        let dispatch =
            self.dispatch_event(host, None, Some(recipient), PBAK_CAPTION_EVENT, Some(&[0]));
        // The source ignores GoolSendEvent's status and always enters state
        // three after a nonzero island target. Preserve that latch even when
        // checked Rust event execution reports a malformed program.
        self.machine
            .set_global_word(PBAK_STATE_GLOBAL, 3)
            .map_err(RuntimeError::Vm)?;
        let effects = self.machine.effects()[effects_start..].to_vec();
        Ok(match dispatch {
            Ok(dispatch) => RetailDemoFinishOutcome::CaptionEvent {
                recipient,
                dispatch,
                effects,
            },
            Err(_) => RetailDemoFinishOutcome::CaptionEventFault { recipient, effects },
        })
    }

    /// Mirrors the temporary `next_lid != -1` guard around Crash's initial
    /// `LevelSaveState` call in `GoolObjectSpawn`.
    ///
    /// Ordinary destination mounts leave this disabled so their first Crash
    /// spawn replaces the previous level's snapshot. Bonus returns enable it
    /// only for the protected pre-restart spawn scan, preserving the snapshot
    /// that `LevelRestart` immediately consumes.
    pub fn set_initial_crash_save_suppressed(&mut self, suppressed: bool) {
        self.suppress_initial_crash_save = suppressed;
    }

    /// Captures the exact fields written by native `LevelSaveState`.
    ///
    /// The zone's `0x2000` restriction is checked before dereferencing Crash,
    /// matching the source early return. `caller` supplies the optional
    /// status-`0x200` translation override used by checkpoint objects.
    pub fn save_level_state(
        &mut self,
        caller: RuntimeObjectHandle,
        death_resets_counter: bool,
    ) -> Result<RetailSaveStateOutcome, RetailLevelStateError> {
        let outcome = Self::capture_level_state(
            &self.arena,
            &self.handles,
            &self.machine,
            self.level,
            self.level_state_context.as_ref(),
            caller,
            death_resets_counter,
        )?;
        if let RetailSaveStateOutcome::Saved(snapshot) = &outcome {
            self.saved_level_state = Some(snapshot.as_ref().clone());
        }
        Ok(outcome)
    }

    /// Seeds a restartable snapshot for the browser's non-native direct-boot
    /// affordance when the selected stream begins in a save-restricted zone.
    /// This never runs on a session remount, so ordinary bonus entry continues
    /// to retain and return to the parent level exactly as native does.
    fn save_restricted_direct_boot_state(
        &mut self,
        caller: RuntimeObjectHandle,
    ) -> Result<(), RetailLevelStateError> {
        let mut context = self
            .level_state_context
            .clone()
            .ok_or(RetailLevelStateError::MissingContext)?;
        context.graphics_flags &= !SAVE_RESTRICTED_ZONE_FLAG;
        let outcome = Self::capture_level_state(
            &self.arena,
            &self.handles,
            &self.machine,
            self.level,
            Some(&context),
            caller,
            true,
        )?;
        if let RetailSaveStateOutcome::Saved(snapshot) = outcome {
            self.saved_level_state = Some(*snapshot);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn capture_level_state(
        arena: &ObjectArena,
        handles: &HandleMap,
        machine: &Machine,
        level: Option<LevelId>,
        context: Option<&RetailLevelStateContext>,
        caller: RuntimeObjectHandle,
        death_resets_counter: bool,
    ) -> Result<RetailSaveStateOutcome, RetailLevelStateError> {
        let context = context.ok_or(RetailLevelStateError::MissingContext)?;
        if context.graphics_flags & SAVE_RESTRICTED_ZONE_FLAG != 0 {
            return Ok(RetailSaveStateOutcome::RestrictedByZone);
        }
        if !handles.is_live_pair(caller) {
            return Err(RetailLevelStateError::UnknownCaller(caller));
        }
        let level = level.ok_or(RetailLevelStateError::MissingLevel)?;
        let main_arena = arena
            .main_object()
            .ok_or(RetailLevelStateError::MissingMainObject)?;
        let main = handles
            .for_arena(main_arena)
            .ok_or(RetailLevelStateError::MissingMainObject)?;
        let player = machine.object(main.vm).map_err(RetailLevelStateError::Vm)?;
        let caller_object = machine
            .object(caller.vm)
            .map_err(RetailLevelStateError::Vm)?;
        let read_vec = |object: &VmObject, indices: [usize; 3]| {
            Ok::<[i32; 3], VmError>([
                object.register(indices[0])?.cast_signed(),
                object.register(indices[1])?.cast_signed(),
                object.register(indices[2])?.cast_signed(),
            ])
        };
        let mut player_translation = read_vec(
            player,
            [
                process_register::TRANSLATION_X,
                process_register::TRANSLATION_Y,
                process_register::TRANSLATION_Z,
            ],
        )
        .map_err(RetailLevelStateError::Vm)?;
        if caller_object
            .register(process_register::STATUS_B)
            .map_err(RetailLevelStateError::Vm)?
            & SAVE_TRANSLATION_FROM_CALLER_STATUS_B
            != 0
        {
            player_translation = read_vec(
                caller_object,
                [
                    process_register::TRANSLATION_X,
                    process_register::TRANSLATION_Y,
                    process_register::TRANSLATION_Z,
                ],
            )
            .map_err(RetailLevelStateError::Vm)?;
        }
        // Checkpoint GOOL writes and misc 12/11 can precede a save in the same
        // interpreter invocation, before the browser can publish a refreshed
        // pointer-free context. All four native globals are authoritative in
        // that synchronous window; ordinary frames retain the supplied host
        // contract.
        let (checkpoint_id, checkpoint_translation) =
            if machine.checkpoint_globals_changed_since_context() {
                (
                    machine
                        .global_word(CHECKPOINT_ID_GLOBAL)
                        .map_err(RetailLevelStateError::Vm)?
                        .cast_signed(),
                    [
                        machine
                            .global_word(CHECKPOINT_TRANSLATION_GLOBALS[0])
                            .map_err(RetailLevelStateError::Vm)?
                            .cast_signed(),
                        machine
                            .global_word(CHECKPOINT_TRANSLATION_GLOBALS[1])
                            .map_err(RetailLevelStateError::Vm)?
                            .cast_signed(),
                        machine
                            .global_word(CHECKPOINT_TRANSLATION_GLOBALS[2])
                            .map_err(RetailLevelStateError::Vm)?
                            .cast_signed(),
                    ],
                )
            } else {
                (context.checkpoint_id, context.checkpoint_translation)
            };
        if checkpoint_id != -1 && checkpoint_id != 0 {
            player_translation = checkpoint_translation;
        }
        let snapshot = RetailLevelSnapshot {
            player_translation,
            player_rotation_yxz: [0; 3],
            player_scale: read_vec(
                player,
                [
                    process_register::SCALE_X,
                    process_register::SCALE_Y,
                    process_register::SCALE_Z,
                ],
            )
            .map_err(RetailLevelStateError::Vm)?,
            location: context.location,
            level,
            death_resets_counter,
            spawn_words: arena.spawn_table().snapshot(),
            // Native LevelSaveState samples the process-global word at this
            // exact synchronous boundary. The calling GOOL handler can mutate
            // that word on either side of the call, while the host's
            // pointer-free camera/lifecycle mirror is refreshed only between
            // cooperative frames.
            box_count: machine
                .global_word(BOX_COUNT_GLOBAL)
                .map_err(RetailLevelStateError::Vm)?
                .cast_signed(),
        };
        Ok(RetailSaveStateOutcome::Saved(Box::new(snapshot)))
    }

    /// Reproduces the object/spawn half of native `LevelRestart`.
    ///
    /// The caller must preflight its pager/lifecycle `LevelUpdate` before this
    /// irreversible method, then commit the returned location/flags before the
    /// next spawn scan. Broadcast, zone teardown, spawn restore, and Crash
    /// reset retain their source order here.
    pub fn restart_saved_level<H: ProgramHost>(
        &mut self,
        host: &mut H,
    ) -> Result<RetailRestartOutcome<H::Error>, RuntimeError<H::Error>> {
        let saved_level = self
            .saved_level_state
            .as_ref()
            .ok_or(RuntimeError::MissingSavedLevelState)?
            .level;
        self.restart_saved_level_from_effect(host, saved_level)
    }

    /// Completes a `LoadState` whose save level was captured at the exact VM
    /// host boundary.
    ///
    /// Different-level GOOL may continue after misc 12/1 and can legally emit
    /// another `SaveState` before the browser consumes the effect. Restart kind
    /// therefore comes from `captured_saved_level`, while a later snapshot is
    /// still available to the eventual `-2` `LEVEL_END` resolution just as it
    /// is in native process state.
    pub fn restart_saved_level_from_effect<H: ProgramHost>(
        &mut self,
        host: &mut H,
        captured_saved_level: LevelId,
    ) -> Result<RetailRestartOutcome<H::Error>, RuntimeError<H::Error>> {
        let snapshot = self
            .saved_level_state
            .clone()
            .ok_or(RuntimeError::MissingSavedLevelState)?;
        // Native clears bonus mode before even checking whether the saved
        // level differs and returning the `-2` remount sentinel.
        self.set_mount_global(BONUS_ROUND_GLOBAL, 0);
        // Same-level misc 12/1 stops the pointer-free traversal before this
        // deferred structural phase. A different-level request has already
        // completed its source interpreter/traversal and carries no live-tree
        // mutation, but clearing the shared latch remains harmless.
        self.machine.clear_level_restart_request();
        if let Ok(value) = self.machine.global_word(RESPAWN_COUNT_GLOBAL) {
            self.respawn_count = value;
        }
        if let Ok(value) = self.machine.global_word(DEATH_COUNT_GLOBAL) {
            self.death_count = value;
        }
        let current_level = self.level.ok_or(RuntimeError::MissingLevelStateContext)?;
        if captured_saved_level != current_level {
            let context = self
                .level_state_context
                .as_mut()
                .ok_or(RuntimeError::MissingLevelStateContext)?;
            context.first_spawn = true;
            return Ok(RetailRestartOutcome::DifferentLevel {
                saved_level: captured_saved_level,
                requested_level_sentinel: -2,
            });
        }
        if snapshot.level != captured_saved_level {
            return Err(RuntimeError::SavedLevelChangedAfterLoad {
                captured: captured_saved_level,
                current: snapshot.level,
            });
        }
        let mut context = self
            .level_state_context
            .clone()
            .ok_or(RuntimeError::MissingLevelStateContext)?;

        // `GoolSendToColliders(..., type=0)` is an all-root postorder
        // broadcast despite its name. Checked failures are retained while the
        // source-order restart continues.
        let recipients = self
            .arena
            .postorder_snapshot()
            .map_err(RuntimeError::Tree)?
            .into_iter()
            .map(|arena| {
                self.handles
                    .for_arena(arena)
                    .ok_or(RuntimeError::UnknownArenaObject(arena))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut respawn_event_failures = Vec::new();
        for object in recipients {
            if let Err(error) = self.dispatch_event(host, None, Some(object), RESPAWN_EVENT, None) {
                respawn_event_failures.push(RetailRespawnEventFailure { object, error });
            }
        }

        // `obj_zone = (entry *)-1` makes every eligible object die even when
        // a TERM handler tries to migrate it. Zones retain current-header
        // neighbor order, and each report retains native postorder.
        let mut zone_reports = Vec::with_capacity(context.active_neighbor_zones.len());
        for zone in context.active_neighbor_zones.iter().copied() {
            let report =
                self.terminate_zone_objects(zone, ZoneTerminationMode::HardRestart, host)?;
            zone_reports.push((zone, report));
        }

        // Native samples the checkpoint globals only after the RESPAWN and
        // TERM broadcasts. A synchronous handler may move the checkpoint in
        // the same restart transaction, so the live words must win over the
        // cloned browser context before spawn restoration and box accounting.
        if self.machine.checkpoint_globals_changed_since_context() {
            context.checkpoint_id = self
                .machine
                .global_word(CHECKPOINT_ID_GLOBAL)
                .map_err(RuntimeError::Vm)?
                .cast_signed();
            context.checkpoint_translation = [
                self.machine
                    .global_word(CHECKPOINT_TRANSLATION_GLOBALS[0])
                    .map_err(RuntimeError::Vm)?
                    .cast_signed(),
                self.machine
                    .global_word(CHECKPOINT_TRANSLATION_GLOBALS[1])
                    .map_err(RuntimeError::Vm)?
                    .cast_signed(),
                self.machine
                    .global_word(CHECKPOINT_TRANSLATION_GLOBALS[2])
                    .map_err(RuntimeError::Vm)?
                    .cast_signed(),
            ];
        }

        let first_spawn = context.first_spawn;
        if first_spawn {
            let mut words = snapshot.spawn_words;
            if context.checkpoint_id != -1 {
                let raw_index = context.checkpoint_id >> 8;
                if let Ok(index) = usize::try_from(raw_index)
                    && let Some(word) = words.get_mut(index)
                {
                    *word &= !SPAWN_CHECKPOINT_BLOCKED_BIT;
                    *word |= SPAWN_CHECKPOINT_SEEN_BIT;
                }
            }
            for word in &mut words {
                *word &= !SPAWN_ACTIVE_BIT;
            }
            self.arena.spawn_table_mut().restore(words);
        } else {
            // Ordinary same-level death restart calls LevelUpdate with flag
            // one. Before the following spawn scan it clears both transient
            // checkpoint/load bits from every exact-width spawn word.
            let mut words = self.arena.spawn_table().snapshot();
            for word in &mut words {
                *word &= !SPAWN_LEVEL_UPDATE_CLEAR_MASK;
            }
            self.arena.spawn_table_mut().restore(words);
        }

        // Hard restart preserves non-title Crash. It is an explicit checked
        // boundary if no main object exists; fabricating a program without the
        // source's handle-six create path would corrupt state silently.
        let main_arena = self
            .arena
            .main_object()
            .ok_or(RuntimeError::MissingMainObject)?;
        let main = self
            .handles
            .for_arena(main_arena)
            .ok_or(RuntimeError::MissingMainObject)?;
        self.arena
            .set_zone(main_arena, snapshot.location.path.zone)
            .map_err(RuntimeError::Tree)?;
        // Native clears only Crash's current collider pair. Other surviving
        // objects can retain asymmetric collider links to Crash; DoctC uses
        // exactly that link to accept a mask after a death restart.
        self.machine
            .clear_retail_collider_pair(main.vm)
            .map_err(RuntimeError::Vm)?;
        let player = self.machine.object_mut(main.vm).map_err(RuntimeError::Vm)?;
        for (register, value) in [
            (
                process_register::TRANSLATION_X,
                snapshot.player_translation[0],
            ),
            (
                process_register::TRANSLATION_Y,
                snapshot.player_translation[1],
            ),
            (
                process_register::TRANSLATION_Z,
                snapshot.player_translation[2],
            ),
            (
                process_register::ROTATION_Y,
                snapshot.player_rotation_yxz[0],
            ),
            (
                process_register::ROTATION_X,
                snapshot.player_rotation_yxz[1],
            ),
            (
                process_register::ROTATION_Z,
                snapshot.player_rotation_yxz[2],
            ),
            (process_register::SCALE_X, snapshot.player_scale[0]),
            (process_register::SCALE_Y, snapshot.player_scale[1]),
            (process_register::SCALE_Z, snapshot.player_scale[2]),
            (process_register::MISC_A_X, 0),
            (process_register::MISC_A_Y, 0),
            (process_register::MISC_A_Z, 0),
            (process_register::SPEED, 0),
            (process_register::FLOOR_IMPACT_STAMP, 0),
            // `target_rot.x = rot.x`; target X aliases misc-B X.
            (process_register::MISC_B_X, snapshot.player_rotation_yxz[1]),
        ] {
            player
                .set_register(register, value.cast_unsigned())
                .map_err(RuntimeError::Vm)?;
        }
        self.draw_count = 0;
        self.machine.set_draw_count(0);
        self.set_mount_global(SCREEN_SHAKE_GLOBAL, 0);
        if self.machine.global_word(NEXT_DISPLAY_GLOBAL).is_ok() {
            self.machine
                .set_global_word(NEXT_DISPLAY_GLOBAL, INITIAL_DISPLAY_MASK)
                .map_err(RuntimeError::Vm)?;
        }
        if !first_spawn {
            self.respawn_count = self.respawn_count.wrapping_add(0x100);
            if snapshot.death_resets_counter {
                self.death_count = 0;
            } else {
                self.death_count = self.death_count.wrapping_add(0x100);
            }
        }
        self.set_mount_global(RESPAWN_COUNT_GLOBAL, self.respawn_count);
        self.set_mount_global(DEATH_COUNT_GLOBAL, self.death_count);
        self.apply_level_init_misc_zero(current_level);
        let restored_box_count = if first_spawn && context.checkpoint_id != -1 {
            snapshot.box_count.wrapping_sub(0x100)
        } else if first_spawn {
            snapshot.box_count
        } else {
            0
        };
        self.set_mount_global(BOX_COUNT_GLOBAL, restored_box_count.cast_unsigned());
        if let Some(live_context) = self.level_state_context.as_mut() {
            live_context.location = snapshot.location;
            live_context.box_count = restored_box_count;
            live_context.checkpoint_id = context.checkpoint_id;
            live_context.checkpoint_translation = context.checkpoint_translation;
            live_context.first_spawn = false;
        }
        self.machine.acknowledge_level_state_context();
        // The following native LevelUpdate activates a fresh neighbor band
        // before its first post-restart entity scan.
        self.reset_retail_box_spawn_state();
        Self::refresh_tree_links(&self.arena, &self.handles, &mut self.machine)?;

        Ok(RetailRestartOutcome::Restarted(Box::new(
            RetailRestartReport {
                snapshot,
                level_update_flags: u8::from(!first_spawn),
                respawn_event_failures,
                zone_reports,
                respawn_count: self.respawn_count,
                death_count: self.death_count,
                restored_box_count,
            },
        )))
    }

    /// Installs one port's complete retail pad history before object
    /// interpretation. `PadUpdate` computes these five words once per
    /// cooperative tick; GOOL opcode `0x1a` and native-style physics must
    /// observe the same immutable snapshot for the whole frame.
    ///
    /// # Errors
    ///
    /// Returns a VM error when `port` is outside the two retail controller
    /// slots.
    pub fn set_pad_snapshot(
        &mut self,
        port: usize,
        snapshot: RetailPadSnapshot,
    ) -> Result<(), VmError> {
        self.machine.set_pad_snapshot(port, snapshot)
    }

    /// Freezes the camera-relative heading and retail gameplay-input gate for
    /// the next source-ordered object traversal.
    pub fn set_physics_frame_context(
        &mut self,
        game_state_playing: bool,
        camera_rotation_xz: Angle12,
    ) {
        self.machine.set_retail_physics_frame_context(
            game_state_playing,
            i32::from(camera_rotation_xz.raw()),
        );
    }

    /// Freezes the complete retail game-state word and camera-relative
    /// heading for the next source-ordered object traversal.
    pub fn set_frame_context(&mut self, game_state: i32, camera_rotation_xz: Angle12) {
        self.machine
            .set_retail_frame_context(game_state, i32::from(camera_rotation_xz.raw()));
    }

    /// Latches a live post-effect game-state word for physics without writing
    /// that word back over any synchronous camera/TERM handler mutation.
    pub fn latch_frame_context(&mut self, game_state: i32, camera_rotation_xz: Angle12) {
        self.machine
            .latch_retail_frame_context(game_state, i32::from(camera_rotation_xz.raw()));
    }

    /// Freezes the camera pose and projection used by GOOL transform-vector
    /// projection/audio operations for the next cooperative frame.
    pub fn set_transform_vectors_camera(&mut self, camera: RetailTransformVectorsCamera) {
        self.machine.set_transform_vectors_camera(camera);
    }

    /// Freezes the browser frame's unrounded and source-rounded millisecond
    /// tick deltas before the next cooperative object traversal.
    pub fn set_frame_timing(&mut self, ticks_current_frame: i32, ticks_per_frame: i32) {
        self.machine
            .set_frame_timing(ticks_current_frame, ticks_per_frame);
    }

    /// Resolves the live model vertex and scalar globals consumed by
    /// `CamDeath` without retaining a native object or asset pointer.
    pub fn resolve_spin_death_camera_inputs<H: ProgramHost>(
        &self,
        host: &mut H,
    ) -> Result<SpinDeathCameraInputs, SpinDeathCameraResolveError<H::Error>> {
        let object_word = self
            .machine
            .global_word(SPIN_DEATH_CAMERA_OBJECT_GLOBAL)
            .map_err(SpinDeathCameraResolveError::Vm)?;
        if object_word == 0 {
            return Err(SpinDeathCameraResolveError::NullObjectReference);
        }
        let reference = CollisionObjectReference::from_word(object_word).ok_or(
            SpinDeathCameraResolveError::InvalidObjectReference(object_word),
        )?;
        let object = self
            .handles
            .for_vm(reference.object())
            .ok_or(SpinDeathCameraResolveError::StaleObjectReference(reference))?;
        if !self.handles.is_live_pair(object) || self.arena.get(object.arena).is_none() {
            return Err(SpinDeathCameraResolveError::StaleObjectReference(reference));
        }
        let vm_object = self
            .machine
            .object(object.vm)
            .map_err(|_| SpinDeathCameraResolveError::StaleObjectReference(reference))?;
        let spawned = self
            .arena
            .get(object.arena)
            .ok_or(SpinDeathCameraResolveError::StaleObjectReference(reference))?;
        let animation = self
            .machine
            .animation_source(object.vm)
            .map_err(SpinDeathCameraResolveError::Vm)?
            .ok_or(SpinDeathCameraResolveError::MissingAnimation(object))?;
        let (model_eid, frame_count, bound_reference) = match &animation {
            AnimationSource::ItemFive(animation_reference) => {
                let bytes = vm_object
                    .animation_data(*animation_reference)
                    .map_err(SpinDeathCameraResolveError::Vm)?;
                let descriptor = parse_gool_animation_descriptor(bytes, 0).map_err(|_| {
                    SpinDeathCameraResolveError::Vm(VmError::InvalidAnimationReference(
                        animation_reference.to_word(),
                    ))
                })?;
                let GoolAnimationDescriptor::Vertex(vertex) = descriptor else {
                    return Err(SpinDeathCameraResolveError::NonVertexAnimation(object));
                };
                (
                    vertex.model_eid,
                    u32::from(vertex.header.length),
                    AnimationBoundReference::ItemFive(*animation_reference),
                )
            }
            AnimationSource::Process(animation_reference) => {
                let ProcessAnimationKind::Vertex(vertex) = animation_reference.kind() else {
                    return Err(SpinDeathCameraResolveError::NonVertexAnimation(object));
                };
                (
                    vertex.model_eid,
                    u32::from(vertex.header.length),
                    AnimationBoundReference::Model(vertex.model_eid),
                )
            }
        };
        let frame_index = vm_object.animation_frame() >> 8;
        if frame_index >= frame_count {
            return Err(SpinDeathCameraResolveError::MissingFrame {
                object,
                model_eid,
                frame_index,
            });
        }
        let raw_vertex_index = self
            .machine
            .global_word(SPIN_DEATH_CAMERA_VERTEX_GLOBAL)
            .map_err(SpinDeathCameraResolveError::Vm)? as i32
            >> 8;
        let vertex_index = u32::try_from(raw_vertex_index).map_err(|_| {
            SpinDeathCameraResolveError::VertexIndexOutOfRange {
                object,
                model_eid,
                frame_index,
                vertex_index: raw_vertex_index,
            }
        })?;

        let bound = host
            .animation_bound_source(AnimationBoundBinding {
                object,
                zone: spawned.zone(),
                executable: spawned.origin().executable(),
                reference: bound_reference,
                frame_index,
            })
            .map_err(SpinDeathCameraResolveError::Program)?;
        match bound {
            Some(AnimationBoundSource::Vertex { .. }) => {}
            Some(AnimationBoundSource::NonVertex) => {
                return Err(SpinDeathCameraResolveError::NonVertexAnimation(object));
            }
            None => {
                return Err(SpinDeathCameraResolveError::MissingFrame {
                    object,
                    model_eid,
                    frame_index,
                });
            }
        }

        let source = host
            .model_vertex_source(ModelVertexBinding {
                requester: object,
                link: object,
                model_eid,
                frame_index,
                vertex_index,
            })
            .map_err(SpinDeathCameraResolveError::Program)?
            .ok_or(SpinDeathCameraResolveError::VertexIndexOutOfRange {
                object,
                model_eid,
                frame_index,
                vertex_index: raw_vertex_index,
            })?;
        let transform = vm_object
            .retail_transform()
            .map_err(SpinDeathCameraResolveError::Vm)?;
        let scale = [0_usize, 1, 2]
            .map(|axis| source.geometry_scale[axis].wrapping_mul(transform.scale[axis]) >> 12);
        let focus = retail_yxy_transform(
            Vec3 {
                x: source.local_position[0],
                y: source.local_position[1],
                z: source.local_position[2],
            },
            BoundTransform {
                translation: Vec3 {
                    x: transform.translation[0],
                    y: transform.translation[1],
                    z: transform.translation[2],
                },
                rotation: Angles {
                    y: Angle12::new(transform.rotation_yxz[0]),
                    x: Angle12::new(transform.rotation_yxz[1]),
                    z: Angle12::new(transform.rotation_yxz[2]),
                },
                scale: Vec3 {
                    x: scale[0],
                    y: scale[1],
                    z: scale[2],
                },
            },
        );

        Ok(SpinDeathCameraInputs {
            count: self
                .machine
                .global_word(SPIN_DEATH_CAMERA_COUNT_GLOBAL)
                .map_err(SpinDeathCameraResolveError::Vm)? as i32,
            focus,
            zoom_speed: self
                .machine
                .global_word(SPIN_DEATH_CAMERA_ZOOM_SPEED_GLOBAL)
                .map_err(SpinDeathCameraResolveError::Vm)? as i32,
            flip_speed: self
                .machine
                .global_word(SPIN_DEATH_CAMERA_FLIP_SPEED_GLOBAL)
                .map_err(SpinDeathCameraResolveError::Vm)? as i32,
            spin_accel: self
                .machine
                .global_word(CURRENT_DISPLAY_GLOBAL)
                .map_err(SpinDeathCameraResolveError::Vm)?
                & GOOL_FLAG_SPIN_ACCEL
                != 0,
            ticks_per_frame: self.machine.ticks_per_frame(),
        })
    }

    /// Writes back the first-nine-frame alignment counter advanced by the
    /// death-camera core without exposing its raw GOOL global index.
    pub fn set_spin_death_camera_count(&mut self, count: i32) -> Result<(), VmError> {
        self.machine
            .set_global_word(SPIN_DEATH_CAMERA_COUNT_GLOBAL, count as u32)
    }

    #[must_use]
    pub fn object_for_arena(&self, arena: ArenaObjectHandle) -> Option<RuntimeObjectHandle> {
        self.handles.for_arena(arena)
    }

    #[must_use]
    pub fn object_for_vm(&self, vm: VmObjectHandle) -> Option<RuntimeObjectHandle> {
        self.handles.for_vm(vm)
    }

    /// Drains platform-owned cleanup emitted by synchronous object reclaim.
    ///
    /// Actions retain native recursive release order (children before parent)
    /// and must be consumed before advancing another browser simulation
    /// boundary that could associate audio with a recycled VM handle.
    pub fn take_cleanup_actions(&mut self) -> Vec<RuntimeCleanupAction> {
        std::mem::take(&mut self.pending_cleanup_actions)
    }

    /// Drains ordered TERM recipients whose reclaim handler faulted.
    ///
    /// Native ignores the handler's return value and still releases the
    /// object. Keeping this diagnostic separate preserves that lifecycle while
    /// making every checked fault observable.
    pub fn take_reclaim_event_faults(&mut self) -> Vec<RuntimeReclaimEventFault> {
        std::mem::take(&mut self.reclaim_event_faults)
    }

    /// Drains C00 resume deliveries whose checked handler faulted. Resume
    /// still completes in source order; this queue is diagnostics only.
    pub fn take_pause_event_faults(&mut self) -> Vec<RuntimePauseEventFault> {
        std::mem::take(&mut self.pause_event_faults)
    }

    /// Drains collision-generated event deliveries whose GOOL handlers
    /// faulted. Native ignores these return codes and continues the mover's
    /// update; the ordered queue makes the checked Rust failures observable.
    pub fn take_solid_event_faults(&mut self) -> Vec<RuntimeSolidEventFault> {
        std::mem::take(&mut self.solid_event_faults)
    }

    /// Drains invincibility-hit deliveries whose GOOL handlers faulted.
    /// Native ignores these return codes and continues through colors and
    /// physics; the ordered queue makes checked Rust failures observable.
    pub fn take_invincibility_event_faults(&mut self) -> Vec<RuntimeInvincibilityEventFault> {
        std::mem::take(&mut self.invincibility_event_faults)
    }

    /// Completes native `CoreFrame`'s pre-remount level transition phase.
    ///
    /// `requested_lid` is the signed value retained before event delivery.
    /// Every live object is then sent `GOOL_EVENT_LEVEL_END` in eight-root
    /// postorder. Ordinary level writes made by handlers remain observable in
    /// `next_lid_after_event`, but only a final `-2` overrides the requested
    /// destination with the saved snapshot's level.
    ///
    /// A same-level load request is returned as a checked restart boundary:
    /// it needs the full in-stream RESPAWN/zone/Crash transaction and cannot
    /// be represented by the remount carry used for a different-level load.
    pub fn finish_level_transition<H: ProgramHost>(
        &mut self,
        host: &mut H,
        requested_lid: i32,
    ) -> Result<RetailLevelEndReport<H::Error>, RuntimeError<H::Error>> {
        if self.machine.level_restart_requested() {
            return Err(RuntimeError::PendingLevelRestartAtLevelEnd);
        }
        let effects_start = self.machine.effects().len();
        let recipients = self
            .arena
            .postorder_snapshot()
            .map_err(RuntimeError::Tree)?
            .into_iter()
            .map(|arena| {
                self.handles
                    .for_arena(arena)
                    .ok_or(RuntimeError::UnknownArenaObject(arena))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut next_lid_after_event = requested_lid;
        let mut event_failures = Vec::new();

        for object in recipients {
            let effect_cursor = self.machine.effects().len();
            let result = self.dispatch_event_mode(
                host,
                None,
                Some(object),
                LEVEL_END_EVENT,
                None,
                EventLoadStateMode::ContinueDifferentLevel,
            );
            let emitted = self.machine.effects()[effect_cursor..].to_vec();
            let mut consumed_restart = false;
            for effect in &emitted {
                match effect {
                    VmEffect::Transition(level) => next_lid_after_event = *level,
                    VmEffect::LoadState {
                        saved_level: Some(saved_level),
                        ..
                    } => {
                        self.consume_different_level_restart_at_level_end(*saved_level)?;
                        next_lid_after_event = -2;
                        consumed_restart = true;
                    }
                    VmEffect::LoadState {
                        saved_level: None, ..
                    } => return Err(RuntimeError::Vm(VmError::MissingHostEffect)),
                    _ => {}
                }
            }
            if self.machine.level_restart_requested() && !consumed_restart {
                return Err(RuntimeError::PendingLevelRestartAtLevelEnd);
            }
            if let Err(error) = result {
                event_failures.push(RetailLevelEndEventFailure { object, error });
            }
        }

        let resolved = resolve_retail_level_transition(
            requested_lid,
            next_lid_after_event,
            self.saved_level_state
                .as_ref()
                .map(|snapshot| snapshot.level),
        )
        .map_err(RuntimeError::Transition)?;
        let effects = self.machine.effects()[effects_start..].to_vec();
        let carry = self.export_session_carry();
        Ok(RetailLevelEndReport {
            requested_lid,
            next_lid_after_event,
            resolved,
            event_failures,
            effects,
            carry,
        })
    }

    fn consume_different_level_restart_at_level_end<E>(
        &mut self,
        captured_saved_level: LevelId,
    ) -> Result<(), RuntimeError<E>> {
        let current_level = self.level.ok_or(RuntimeError::MissingLevelStateContext)?;
        if captured_saved_level == current_level {
            return Err(RuntimeError::SameLevelRestartDuringLevelEnd(current_level));
        }
        self.pending_first_spawn = true;
        if let Some(context) = &mut self.level_state_context {
            context.first_spawn = true;
        }
        self.machine.clear_level_restart_request();
        Ok(())
    }

    /// Delivers one event through the checked VM and resolves any returned
    /// state program before control is returned to the caller.
    ///
    /// This is the stream-owning half of [`Machine::send_event`]. It validates
    /// both arena/VM handle directions, binds the target state through
    /// [`ProgramHost`], preserves the event argument payload, and runs an armed
    /// state `once` block synchronously. Spawn effects from that once block are
    /// also materialized before this method returns.
    pub fn dispatch_event<H: ProgramHost>(
        &mut self,
        host: &mut H,
        sender: Option<RuntimeObjectHandle>,
        recipient: Option<RuntimeObjectHandle>,
        event: u32,
        arguments: Option<&[u32]>,
    ) -> Result<EventDispatchOutcome, RuntimeError<H::Error>> {
        self.dispatch_event_mode(
            host,
            sender,
            recipient,
            event,
            arguments,
            EventLoadStateMode::RequestRestart,
        )
    }

    fn dispatch_event_mode<H: ProgramHost>(
        &mut self,
        host: &mut H,
        sender: Option<RuntimeObjectHandle>,
        recipient: Option<RuntimeObjectHandle>,
        event: u32,
        arguments: Option<&[u32]>,
        load_state_mode: EventLoadStateMode,
    ) -> Result<EventDispatchOutcome, RuntimeError<H::Error>> {
        let mut spawned_children = Vec::new();
        let Self {
            arena,
            machine,
            handles,
            pending_states,
            pending_cleanup_actions,
            reclaim_event_faults,
            level,
            level_state_context,
            saved_level_state,
            transition_zone_context,
            ..
        } = self;
        Self::dispatch_event_parts_mode(
            arena,
            handles,
            machine,
            pending_states,
            pending_cleanup_actions,
            reclaim_event_faults,
            *level,
            level_state_context.as_ref(),
            saved_level_state,
            *transition_zone_context,
            host,
            sender,
            recipient,
            event,
            arguments,
            None,
            &mut spawned_children,
            load_state_mode,
            None,
        )
    }

    /// Sends the native terminate event to every eligible live object from
    /// `zone`, then tears down objects that did not migrate away.
    ///
    /// Every root uses a live, mutation-aware postorder cursor. Siblings are
    /// captured before descent exactly as the source does, while generational
    /// handles reject any freed-pointer ABA instead of reproducing C undefined
    /// behavior. Platform audio is released before compact handles are reused.
    pub fn terminate_zone_objects<H: ProgramHost>(
        &mut self,
        zone: Eid,
        mode: ZoneTerminationMode,
        host: &mut H,
    ) -> Result<ZoneTerminationReport<H::Error>, RuntimeError<H::Error>> {
        let context = match mode {
            ZoneTerminationMode::Departure { target } => ObjectZoneContext::Target(target),
            ZoneTerminationMode::HardRestart => ObjectZoneContext::HardRestartSentinel,
        };
        self.transition_zone_context = context;
        let mut spawned_children = Vec::new();
        let Self {
            arena,
            machine,
            handles,
            pending_states,
            pending_cleanup_actions,
            reclaim_event_faults,
            faulted_objects,
            level,
            level_state_context,
            saved_level_state,
            transition_zone_context,
            ..
        } = self;
        let result = Self::terminate_zone_roots_live_parts(
            arena,
            handles,
            machine,
            pending_states,
            pending_cleanup_actions,
            reclaim_event_faults,
            *level,
            level_state_context.as_ref(),
            saved_level_state,
            *transition_zone_context,
            host,
            zone,
            &mut spawned_children,
            false,
        );
        faulted_objects.retain(|object| handles.is_live_pair(*object));
        match result {
            Ok(report) => {
                self.clear_stale_retail_box_links()?;
                Ok(report)
            }
            Err(error) => {
                let _ = self.clear_stale_retail_box_links::<H::Error>();
                Err(error)
            }
        }
    }

    /// Implements title `TitleTerminateObjects`: signal and kill every object
    /// in eight-root postorder without zone/immunity gates.
    pub fn terminate_all_objects<H: ProgramHost>(
        &mut self,
        host: &mut H,
    ) -> Result<ZoneTerminationReport<H::Error>, RuntimeError<H::Error>> {
        let snapshot = self
            .arena
            .postorder_snapshot()
            .map_err(RuntimeError::Tree)?;
        let mut report = ZoneTerminationReport::new();
        for arena_handle in snapshot {
            let Some(object) = self.handles.for_arena(arena_handle) else {
                continue;
            };
            if let Err(error) = self.dispatch_event(host, None, Some(object), TERMINATE_EVENT, None)
            {
                report
                    .event_failures
                    .push(ZoneTerminationEventFailure { object, error });
            }
            if self.arena.get(arena_handle).is_some() {
                self.remove_runtime_subtree(arena_handle, &mut report)?;
            }
        }
        if !report.terminated.is_empty() {
            self.machine.clear_frame_bounds();
            Self::refresh_tree_links(&self.arena, &self.handles, &mut self.machine)?;
        }
        Ok(report)
    }

    /// Whether an object has failed during a program/VM invocation and is no
    /// longer eligible to execute.
    ///
    /// The VM fetches and advances its program counter before interpreting an
    /// instruction. Retrying after an error would therefore skip the failed
    /// instruction and continue from a state retail never reached. Faulting
    /// the complete generational arena/VM pair also prevents a recycled VM
    /// slot from inheriting an earlier object's quarantine.
    #[must_use]
    pub fn is_object_faulted(&self, object: RuntimeObjectHandle) -> bool {
        self.faulted_objects.contains(&object)
    }

    /// Number of live objects permanently excluded from GOOL execution after
    /// a checked runtime failure.
    #[must_use]
    pub fn faulted_object_count(&self) -> usize {
        self.faulted_objects.len()
    }

    /// Live faulted identities in deterministic arena/VM order.
    pub fn faulted_objects(&self) -> impl Iterator<Item = RuntimeObjectHandle> + '_ {
        self.faulted_objects.iter().copied()
    }

    /// Returns the last frame's owned display submissions in source order.
    ///
    /// Once a frame has traversed successfully, this never reconstructs the
    /// output from final arena liveness or tree shape: a later sibling may
    /// legitimately kill or reparent an object after native already displayed
    /// it. Before the first frame, deliberately constructed runtimes retain a
    /// checked live-state fallback in eight-root preorder.
    pub fn render_objects(&self) -> Result<Vec<RetailRenderObject>, RenderObjectsError> {
        if let Some(records) = &self.rendered_frame_objects {
            return records
                .iter()
                .map(RetailDisplayRecord::render_object)
                .collect();
        }

        self.validate_render_object_pairs()?;
        let mut objects = Vec::with_capacity(self.arena.len());

        for root_index in 0..ROOT_HANDLE_COUNT {
            let root_index_u8 = u8::try_from(root_index)
                .map_err(|_| RenderObjectsError::InvalidRootIndex(root_index))?;
            let root = RootHandle::new(root_index_u8)
                .ok_or(RenderObjectsError::InvalidRootIndex(root_index))?;
            let preorder = self
                .arena
                .preorder(TreeParent::Root(root))
                .map_err(RenderObjectsError::Tree)?;
            for arena_handle in preorder {
                let spawned = self
                    .arena
                    .get(arena_handle)
                    .ok_or(RenderObjectsError::UnboundArenaObject(arena_handle))?;
                let object = self
                    .handles
                    .for_arena(arena_handle)
                    .ok_or(RenderObjectsError::UnboundArenaObject(arena_handle))?;
                if !self.handles.is_live_pair(object) {
                    return Err(RenderObjectsError::StaleObjectPair(object));
                }
                let vm_object = self
                    .machine
                    .object(object.vm)
                    .map_err(RenderObjectsError::Vm)?;
                if vm_object.handle() != object.vm {
                    return Err(RenderObjectsError::StaleObjectPair(object));
                }
                let origin = spawned.origin();
                let display_mask = self.current_display_mask();
                let display_eligible = self
                    .retail_display_enabled_at(object, display_mask)
                    .map_err(RenderObjectsError::Vm)?;
                let current_dark_reference_translation = self
                    .current_dark_reference_translation()
                    .map_err(RenderObjectsError::Vm)?;
                let animation_source = self.machine.animation_source(object.vm);
                let display_snapshot = RetailDisplaySnapshot::capture(
                    vm_object,
                    animation_source,
                    RetailDisplayCapture {
                        display_mask,
                        texture_frame_snapshot: None,
                        enabled: display_eligible,
                        dark_reference_translation: current_dark_reference_translation,
                        dark_distance: self.level_shader.distance,
                        effective_colors: None,
                    },
                )
                .map_err(RenderObjectsError::Vm)?;
                let animation_source = display_snapshot
                    .animation_source
                    .map_err(RenderObjectsError::Vm)?;
                let animation_reference = animation_source
                    .as_ref()
                    .and_then(AnimationSource::item_five_reference);
                objects.push(RetailRenderObject {
                    object,
                    zone: spawned.zone(),
                    executable: origin.executable(),
                    subtype: origin.subtype(),
                    program: vm_object.program_identity(),
                    animation_source,
                    animation_reference,
                    animation_frame: display_snapshot.animation_frame,
                    transform: display_snapshot.transform,
                    status_a: display_snapshot.status_a,
                    status_b: display_snapshot.status_b,
                    status_c: display_snapshot.status_c,
                    state_flags: display_snapshot.state_flags,
                    size: display_snapshot.size,
                    colors: display_snapshot.colors,
                    text_font_override_word_offset: display_snapshot.text_font_override_word_offset,
                    text_arguments: display_snapshot.text_arguments,
                    dark_reference_translation: display_snapshot.dark_reference_translation,
                    dark_distance: display_snapshot.dark_distance,
                    display_mask: display_snapshot.display_mask,
                    texture_frame_snapshot: display_snapshot.texture_frame_snapshot,
                    display_eligible: display_snapshot.enabled,
                });
            }
        }

        Ok(objects)
    }

    fn current_dark_reference_translation(&self) -> Result<Option<[i32; 3]>, VmError> {
        let pause = self
            .machine
            .global_word(PAUSE_OBJECT_GLOBAL)
            .ok()
            .and_then(CollisionObjectReference::from_word)
            .and_then(|reference| self.handles.for_vm(reference.object()))
            .filter(|object| self.handles.is_live_pair(*object));
        let main = self
            .arena
            .main_object()
            .and_then(|arena| self.handles.for_arena(arena))
            .filter(|object| self.handles.is_live_pair(*object));
        let Some(reference) = pause.or(main) else {
            return Ok(None);
        };
        let transform = self.machine.object(reference.vm)?.retail_transform()?;
        Ok(Some(transform.translation))
    }

    fn retail_pool_slot_translation(&self, pool_slot: u8) -> Result<Option<[i32; 3]>, VmError> {
        if let Some(object) = self
            .handles
            .for_retail_pool_slot(pool_slot)
            .filter(|object| self.handles.is_live_pair(*object))
        {
            return Ok(Some(
                self.machine
                    .object(object.vm)?
                    .retail_transform()?
                    .translation,
            ));
        }
        Ok(self.machine.retired_retail_pool_translation(pool_slot))
    }

    fn current_world_dark2_illumination(&mut self) -> Result<Option<[i32; 3]>, VmError> {
        let doctor_word = self.machine.global_word(DOCTOR_OBJECT_GLOBAL)?;
        if doctor_word != 0 {
            let doctor = CollisionObjectReference::from_word(doctor_word)
                .ok_or(VmError::InvalidObjectReference(doctor_word))?;
            let global_write_epoch = self.machine.global_word_write_epoch(DOCTOR_OBJECT_GLOBAL)?;
            let mut retained = if let Some(retained) =
                self.retained_doctor_pool_pointer.filter(|retained| {
                    retained.encoded_word == doctor_word
                        && retained.global_write_epoch == global_write_epoch
                }) {
                retained
            } else {
                let captured_pool_slot =
                    self.machine.retail_global_pool_slot(DOCTOR_OBJECT_GLOBAL)?;
                let live = self.handles.for_vm(doctor.object()).filter(|object| {
                    self.handles.is_live_pair(*object)
                        && captured_pool_slot.is_none_or(|slot| object.arena.slot() == slot)
                });
                let (pool_slot, translation) = if let Some(pool_slot) = captured_pool_slot {
                    let translation = self
                        .retail_pool_slot_translation(pool_slot)?
                        .or_else(|| self.machine.retired_retail_translation(doctor.object()))
                        .ok_or(VmError::UnknownObject(doctor.object()))?;
                    (pool_slot, translation)
                } else if let Some(object) = live {
                    (
                        object.arena.slot(),
                        self.machine
                            .object(object.vm)?
                            .retail_transform()?
                            .translation,
                    )
                } else {
                    let pool_slot = self
                        .handles
                        .retired_arena_slot(doctor.object())
                        .ok_or(VmError::UnknownObject(doctor.object()))?;
                    let translation = self
                        .machine
                        .retired_retail_pool_translation(pool_slot)
                        .or_else(|| self.machine.retired_retail_translation(doctor.object()))
                        .ok_or(VmError::UnknownObject(doctor.object()))?;
                    (pool_slot, translation)
                };
                RetainedDoctorPoolPointer {
                    encoded_word: doctor_word,
                    global_write_epoch,
                    pool_slot,
                    translation,
                }
            };

            // Native `doctor` is a raw pointer into a static object pool.
            // `GoolObjectKill` leaves that slot's initialized transform in
            // place, and a later allocation through the physical free list
            // makes the pointer observe the replacement even when compact VM
            // handles are reused in a different order.
            retained.translation = self
                .retail_pool_slot_translation(retained.pool_slot)?
                .unwrap_or(retained.translation);
            self.retained_doctor_pool_pointer = Some(retained);
            return Ok(Some(retained.translation));
        }

        self.retained_doctor_pool_pointer = None;

        let reference = self
            .arena
            .main_object()
            .and_then(|arena| self.handles.for_arena(arena))
            .filter(|object| self.handles.is_live_pair(*object));
        let Some(reference) = reference else {
            // Synthetic pre-spawn tests may initialize Dark2 before a main
            // object exists. The native retail path has Crash by the first
            // eligible frame; retain the previous BSS value until then.
            return Ok(None);
        };
        Ok(Some(
            self.machine
                .object(reference.vm)?
                .retail_transform()?
                .translation,
        ))
    }

    fn validate_render_object_pairs(&self) -> Result<(), RenderObjectsError> {
        for (&arena, &vm) in &self.handles.vm_by_arena {
            let object = RuntimeObjectHandle { arena, vm };
            if self.arena.get(arena).is_none()
                || self.handles.for_vm(vm) != Some(object)
                || self.machine.object(vm).is_err()
            {
                return Err(RenderObjectsError::StaleObjectPair(object));
            }
        }
        for (index, arena) in self.handles.arena_by_vm.iter().copied().enumerate() {
            let Some(arena) = arena else {
                continue;
            };
            let vm = VmObjectHandle::new(
                u16::try_from(index).expect("VM handle map capacity fits in u16"),
            )
            .expect("index came from the VM handle map");
            let object = RuntimeObjectHandle { arena, vm };
            if self.arena.get(arena).is_none()
                || self.handles.for_arena(arena) != Some(object)
                || self.machine.object(vm).is_err()
            {
                return Err(RenderObjectsError::StaleObjectPair(object));
            }
        }
        Ok(())
    }

    /// Applies the exact displayed-neighbor/group-three scan and binds every
    /// successful ZDAT entity. A program failure rolls back that one arena
    /// object so no live tree node exists without executable state.
    pub fn spawn_current_zone_neighbors<H: ProgramHost>(
        &mut self,
        neighbors: &[NeighborZone<'_, ZoneEntity>],
        host: &mut H,
    ) -> Vec<RuntimeSpawnAttempt<H::Error>> {
        let mut attempts = Vec::new();
        for (neighbor_index, neighbor) in neighbors.iter().enumerate() {
            if neighbor.display_flags & ACTIVE_ZONE_DISPLAY_BIT == 0 {
                continue;
            }
            for (entity_index, entity) in neighbor.entities.iter().enumerate() {
                let descriptor = EntitySpawnDescriptor::from(entity);
                if descriptor.group != SPAWNABLE_ENTITY_GROUP {
                    continue;
                }
                let result = self.bind_entity_with_native_reclaim(neighbor.eid, entity, host);
                attempts.push(RuntimeSpawnAttempt {
                    neighbor_index,
                    entity_index,
                    zone: neighbor.eid,
                    descriptor,
                    result,
                });
            }
        }
        attempts
    }

    /// Spawns the current zone and executes its initially bound objects once.
    pub fn spawn_and_run_frame<H: ProgramHost>(
        &mut self,
        neighbors: &[NeighborZone<'_, ZoneEntity>],
        host: &mut H,
        instruction_budget_per_object: usize,
    ) -> Result<SpawnedRuntimeFrame<H::Error>, RuntimeError<H::Error>> {
        let spawn_attempts = self.spawn_current_zone_neighbors(neighbors, host);
        let frame = self.run_frame(host, instruction_budget_per_object)?;
        Ok(SpawnedRuntimeFrame {
            spawn_attempts,
            frame,
        })
    }

    /// Executes one cooperative 30 Hz frame in retail root/preorder order.
    ///
    /// Spawn effects are applied synchronously and their children are fully
    /// arena/VM-bound before the parent resumes. As in the source's mutation-
    /// aware preorder walk, children added by a parent are visited later in
    /// this same frame.
    ///
    /// Collidable animation bounds follow the source's Crash-stamp ordering.
    /// Objects whose animation stamp matches Crash publish before GOOL and
    /// physics; an object visited before Crash instead publishes after physics
    /// when it remains inside the exact retail proximity box.
    pub fn run_frame<H: ProgramHost>(
        &mut self,
        host: &mut H,
        instruction_budget_per_object: usize,
    ) -> Result<RuntimeFrame<H::Error>, RuntimeError<H::Error>> {
        self.run_frame_with_traversal_hook(host, instruction_budget_per_object, |_, _, _| Ok(()))
    }

    /// Executes one cooperative GOOL frame while deferring the display latch.
    ///
    /// Title hosts use this to insert source `TitleUpdate` work before calling
    /// [`Self::finish_deferred_display_frame`].
    pub fn run_frame_before_display<H: ProgramHost>(
        &mut self,
        host: &mut H,
        instruction_budget_per_object: usize,
    ) -> Result<RuntimeFrame<H::Error>, RuntimeError<H::Error>> {
        self.run_frame_before_display_with_traversal_hook(
            host,
            instruction_budget_per_object,
            |_, _, _| Ok(()),
        )
    }

    /// Executes one cooperative frame and invokes `hook` immediately before
    /// the live main/Crash object is updated.
    ///
    /// The hook runs at most once, at the object's actual position in the
    /// mutation-aware eight-root preorder. It may synchronously mutate the
    /// runtime (including dispatching GOOL events), and Crash plus every later
    /// object observes those mutations in the same frame. Earlier roots have
    /// already completed. A frame without a live main object never invokes
    /// the hook because native has no `PadUpdate` call site when `crash` is
    /// null.
    pub fn run_frame_with_traversal_hook<H, F>(
        &mut self,
        host: &mut H,
        instruction_budget_per_object: usize,
        hook: F,
    ) -> Result<RuntimeFrame<H::Error>, RuntimeError<H::Error>>
    where
        H: ProgramHost,
        F: FnMut(&mut Self, &mut H, RetailTraversalBoundary) -> Result<(), RuntimeError<H::Error>>,
    {
        self.run_frame_with_traversal_hook_inner(host, instruction_budget_per_object, true, hook)
    }

    /// Executes GOOL traversal but defers native `GLUpdate` so the platform can
    /// run `TitleUpdate` and any synchronous screen load at the source boundary.
    ///
    /// The caller must complete a successful frame with
    /// [`Self::finish_deferred_display_frame`]. Other callers should continue
    /// using [`Self::run_frame_with_traversal_hook`].
    pub fn run_frame_before_display_with_traversal_hook<H, F>(
        &mut self,
        host: &mut H,
        instruction_budget_per_object: usize,
        hook: F,
    ) -> Result<RuntimeFrame<H::Error>, RuntimeError<H::Error>>
    where
        H: ProgramHost,
        F: FnMut(&mut Self, &mut H, RetailTraversalBoundary) -> Result<(), RuntimeError<H::Error>>,
    {
        self.run_frame_with_traversal_hook_inner(host, instruction_budget_per_object, false, hook)
    }

    fn run_frame_with_traversal_hook_inner<H, F>(
        &mut self,
        host: &mut H,
        instruction_budget_per_object: usize,
        finish_display: bool,
        mut hook: F,
    ) -> Result<RuntimeFrame<H::Error>, RuntimeError<H::Error>>
    where
        H: ProgramHost,
        F: FnMut(&mut Self, &mut H, RetailTraversalBoundary) -> Result<(), RuntimeError<H::Error>>,
    {
        self.refresh_current_solid_environment(host)?;
        for index in 0..SPAWN_TABLE_CAPACITY {
            let id = u16::try_from(index)
                .map_err(|_| RuntimeError::Spawn(SpawnError::InvalidSpawnId(u16::MAX)))?;
            let flags = self
                .arena
                .spawn_table()
                .flags(id)
                .ok_or(RuntimeError::Spawn(SpawnError::InvalidSpawnId(id)))?;
            self.machine
                .set_spawn_flags(id, flags)
                .map_err(RuntimeError::Vm)?;
        }
        self.machine.clear_frame_bounds();
        self.machine.clear_effects();
        let frame_stamp = wrapping_frame_stamp(self.frame_index);
        self.machine.set_frames_elapsed(frame_stamp);
        self.machine.set_draw_count(self.draw_count);
        let handles = &self.handles;
        self.faulted_objects
            .retain(|object| handles.is_live_pair(*object));
        let mut work = FrameWork {
            executions: Vec::with_capacity(self.handles.vm_by_arena.len()),
            spawned_children: Vec::new(),
            display_records: Vec::with_capacity(self.handles.vm_by_arena.len()),
            effects: Vec::with_capacity(self.handles.vm_by_arena.len()),
        };
        let paused = self.pause.paused;
        let mut traversal_hook = FrameTraversalHook {
            callback: &mut hook,
            main_invoked: false,
            paused,
        };

        'roots: for root_index in 0..ROOT_HANDLE_COUNT {
            let root_index_u8 =
                u8::try_from(root_index).map_err(|_| RuntimeError::InvalidRootIndex(root_index))?;
            let root =
                RootHandle::new(root_index_u8).ok_or(RuntimeError::InvalidRootIndex(root_index))?;
            let mut child = self
                .arena
                .preorder(TreeParent::Root(root))
                .map_err(RuntimeError::Tree)?
                .next();
            while let Some(arena_handle) = child {
                let Some(spawned) = self.arena.get(arena_handle) else {
                    break;
                };
                let sibling = spawned.next_sibling();
                self.visit_object(
                    root,
                    arena_handle,
                    host,
                    instruction_budget_per_object,
                    &mut traversal_hook,
                    &mut work,
                )?;
                if self.machine.level_restart_requested() {
                    break 'roots;
                }
                child = sibling;
            }
        }

        let handles = &self.handles;
        self.faulted_objects
            .retain(|object| handles.is_live_pair(*object));

        let frame_index = self.frame_index;
        self.frame_index = self.frame_index.wrapping_add(1);
        if finish_display && !self.machine.level_restart_requested() {
            self.finish_display_frame(paused)
                .map_err(RuntimeError::Vm)?;
        }
        // Publish atomically only after traversal (and the ordinary display
        // latch, when requested) succeeds. A failed later object must not
        // expose a partially reconstructed frame to the renderer.
        self.rendered_frame_objects = Some(std::mem::take(&mut work.display_records));
        self.machine.drain_effects_into(&mut work.effects);
        let effects = std::mem::take(&mut work.effects);
        if effects
            .iter()
            .any(|effect| matches!(effect, VmEffect::ResetLevelGlobals { .. }))
        {
            self.sync_level_reset_mirrors();
        }
        Ok(RuntimeFrame {
            frame_index,
            executions: work.executions,
            spawned_children: work.spawned_children,
            effects,
        })
    }

    fn visit_object<H, F>(
        &mut self,
        root: RootHandle,
        arena_handle: ArenaObjectHandle,
        host: &mut H,
        instruction_budget_per_object: usize,
        traversal_hook: &mut FrameTraversalHook<'_, F>,
        work: &mut FrameWork<H::Error>,
    ) -> Result<(), RuntimeError<H::Error>>
    where
        H: ProgramHost,
        F: FnMut(&mut Self, &mut H, RetailTraversalBoundary) -> Result<(), RuntimeError<H::Error>>,
    {
        let paused = traversal_hook.paused;
        if !traversal_hook.main_invoked
            && self.arena.main_object() == Some(arena_handle)
            && let Some(object) = self.handles.for_arena(arena_handle)
            && self.handles.is_live_pair(object)
        {
            traversal_hook.main_invoked = true;
            (traversal_hook.callback)(
                self,
                host,
                RetailTraversalBoundary::BeforeMainObjectUpdate { root, object },
            )?;
            if self.machine.level_restart_requested() {
                self.machine.drain_effects_into(&mut work.effects);
                return Ok(());
            }
        }
        if let Some(object) = self.handles.for_arena(arena_handle)
            && !self.faulted_objects.contains(&object)
            && self.retail_animation_enabled(object, paused)?
        {
            let result = if self.handles.is_live_pair(object) {
                self.begin_native_object_update(object).and_then(|stalled| {
                    if let Some(execution) = stalled {
                        Ok(execution)
                    } else {
                        let pre_bound = self.animation_stamp_matches_main(object)?;
                        if pre_bound {
                            self.register_animation_bound(object, host)?;
                        }
                        (|| {
                            let execution = self.run_object(
                                object,
                                host,
                                instruction_budget_per_object,
                                &mut work.spawned_children,
                            )?;
                            if !self.machine.level_restart_requested()
                                && self.handles.is_live_pair(object)
                                && execution.reason != HaltReason::InvalidInitialReturn
                            {
                                self.finish_native_object_update(
                                    object,
                                    host,
                                    &mut work.spawned_children,
                                )?;
                            }
                            Ok(execution)
                        })()
                    }
                })
            } else {
                Err(RuntimeError::UnknownArenaObject(arena_handle))
            };
            if result.is_err() {
                // Any error returned from the execution path can follow a
                // fetched instruction or a partial host handshake. Continuing
                // would turn that checked failure into an implicit opcode
                // skip, so quarantine the exact object identity permanently.
                self.pending_states.remove(&object.vm);
                self.faulted_objects.insert(object);
            }
            let invalid_initial_return = matches!(
                &result,
                Ok(Execution {
                    reason: HaltReason::InvalidInitialReturn,
                    ..
                })
            );
            if invalid_initial_return {
                // `GoolObjectTraverseTreePreorder` consumes
                // `ERROR_INVALID_RETURN` immediately as `GoolObjectKill(0)`.
                // This is not a VM fault and must happen before display or
                // child traversal, without dispatching TERM.
                self.kill_invalid_initial_return(object, host, &mut work.spawned_children)?;
                work.executions.push(RuntimeExecution { object, result });
                self.machine.drain_effects_into(&mut work.effects);
                return Ok(());
            }
            if let Ok(vm_object) = self.machine.object(object.vm)
                && self.arena.get(arena_handle).is_some()
            {
                self.arena
                    .set_state_flags(arena_handle, vm_object.state_flags())
                    .map_err(RuntimeError::Tree)?;
            }
            work.executions.push(RuntimeExecution { object, result });
            if !paused
                && self.pause.controller == Some(object)
                && work.executions.last().is_some_and(|execution| {
                    matches!(
                        &execution.result,
                        Ok(Execution {
                            reason: HaltReason::Halted,
                            ..
                        })
                    )
                })
            {
                self.kill_resumed_pause_controller(object, host, &mut work.spawned_children)?;
            }
        }
        if self.machine.level_restart_requested() {
            self.machine.drain_effects_into(&mut work.effects);
            return Ok(());
        }
        if let Some(object) = self.handles.for_arena(arena_handle)
            && self.handles.is_live_pair(object)
        {
            // Native reads global nine after this object's transition/code and
            // consumes that same live value throughout its display transform.
            // Capture it once: earlier/later objects in this preorder frame may
            // legitimately observe different authored values.
            let display_mask = self.current_display_mask();
            let texture_frame_snapshot = host.texture_frame_snapshot();
            let displayed = self
                .retail_display_enabled_at(object, display_mask)
                .map_err(RuntimeError::Vm)?;
            let dark_reference_translation = self
                .current_dark_reference_translation()
                .map_err(RuntimeError::Vm)?;
            let effective_colors = self.apply_native_vertex_display_side_effects(
                object,
                displayed,
                display_mask,
                dark_reference_translation,
                host,
            )?;
            let animation_source = self.machine.animation_source(object.vm);
            let display_snapshot = RetailDisplaySnapshot::capture(
                self.machine.object(object.vm).map_err(RuntimeError::Vm)?,
                animation_source,
                RetailDisplayCapture {
                    display_mask,
                    texture_frame_snapshot,
                    enabled: displayed,
                    dark_reference_translation,
                    dark_distance: self.level_shader.distance,
                    effective_colors,
                },
            )
            .map_err(RuntimeError::Vm)?;
            let (zone, origin) = {
                let spawned = self
                    .arena
                    .get(object.arena)
                    .ok_or(RuntimeError::UnknownArenaObject(object.arena))?;
                (spawned.zone(), spawned.origin())
            };
            let program = self
                .machine
                .object(object.vm)
                .map_err(RuntimeError::Vm)?
                .program_identity();
            work.display_records.push(RetailDisplayRecord {
                object,
                zone,
                executable: origin.executable(),
                subtype: origin.subtype(),
                program,
                snapshot: display_snapshot,
            });
        }

        // `Machine` keeps a small bounded effect queue for standalone
        // interpreter callers. A retail frame can legitimately visit 97
        // animation/audio-producing objects, including one deep subtree, so
        // publish this object's ordered observations before descending.
        self.machine.drain_effects_into(&mut work.effects);

        let mut child = self
            .arena
            .get(arena_handle)
            .and_then(SpawnedObject::first_child);
        while let Some(child_handle) = child {
            let Some(spawned) = self.arena.get(child_handle) else {
                break;
            };
            let sibling = spawned.next_sibling();
            self.visit_object(
                root,
                child_handle,
                host,
                instruction_budget_per_object,
                traversal_hook,
                work,
            )?;
            if self.machine.level_restart_requested() {
                return Ok(());
            }
            child = sibling;
        }
        Ok(())
    }

    fn kill_invalid_initial_return<H: ProgramHost>(
        &mut self,
        object: RuntimeObjectHandle,
        host: &mut H,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
    ) -> Result<(), RuntimeError<H::Error>> {
        // `GoolObjectKill` protects the dedicated Crash/main allocation in
        // every level except Title, even when traversal requested a no-signal
        // kill after an invalid initial-frame return.
        if object.arena.is_dedicated_main() && self.level != Some(LevelId::TITLE) {
            return Ok(());
        }
        if !self.handles.is_live_pair(object) {
            return Ok(());
        }

        let mut report = ZoneTerminationReport::new();
        {
            let Self {
                arena,
                machine,
                handles,
                pending_states,
                pending_cleanup_actions,
                ..
            } = self;
            Self::kill_runtime_subtree_with_host_parts(
                arena,
                handles,
                machine,
                pending_states,
                pending_cleanup_actions,
                host,
                object.arena,
                spawned_children,
                true,
                &mut report,
            )?;
            Self::refresh_tree_links(arena, handles, machine)?;
        }
        self.faulted_objects
            .retain(|candidate| self.handles.is_live_pair(*candidate));
        self.clear_stale_retail_box_links()?;
        if self.pause.controller == Some(object) {
            self.pause.controller = None;
        }
        Ok(())
    }

    fn kill_resumed_pause_controller<H: ProgramHost>(
        &mut self,
        object: RuntimeObjectHandle,
        host: &mut H,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
    ) -> Result<(), RuntimeError<H::Error>> {
        let mut report = ZoneTerminationReport::new();
        {
            let Self {
                arena,
                machine,
                handles,
                pending_states,
                pending_cleanup_actions,
                ..
            } = self;
            Self::kill_runtime_subtree_with_host_parts(
                arena,
                handles,
                machine,
                pending_states,
                pending_cleanup_actions,
                host,
                object.arena,
                spawned_children,
                true,
                &mut report,
            )?;
            Self::refresh_tree_links(arena, handles, machine)?;
        }
        self.faulted_objects
            .retain(|candidate| self.handles.is_live_pair(*candidate));
        self.clear_stale_retail_box_links()?;
        self.pause.controller = None;
        Ok(())
    }

    fn retail_animation_enabled<E>(
        &self,
        object: RuntimeObjectHandle,
        paused: bool,
    ) -> Result<bool, RuntimeError<E>> {
        let Ok(display_mask) = self.machine.global_word(CURRENT_DISPLAY_GLOBAL) else {
            return Ok(true);
        };
        let vm_object = self.machine.object(object.vm).map_err(RuntimeError::Vm)?;
        let status_b = vm_object
            .register(process_register::STATUS_B)
            .map_err(RuntimeError::Vm)?;
        // GOOL register 29 is mutable authored process state. Native reads
        // obj->process.subtype here, not the immutable entity/root origin.
        let subtype = vm_object
            .register(process_register::SUBTYPE)
            .map_err(RuntimeError::Vm)?;
        Ok(retail_animation_update_enabled(
            display_mask,
            status_b,
            vm_object.state_flags(),
            vm_object
                .program_identity()
                .map(GoolProgramIdentity::category),
            vm_object
                .program_identity()
                .map(GoolProgramIdentity::object_type),
            subtype,
            paused,
        ))
    }

    /// Applies the color mutations performed by native `GoolObjectTransform`
    /// at this object's exact post-update/pre-child display boundary.
    ///
    /// The returned colors are the values consumed by geometry. Live VM
    /// colors are committed immediately so a child opcode `0x23` observes its
    /// parent's display side effects in the same preorder frame. Status-B
    /// `0x100000` then performs its separate post-transform zone-color reset,
    /// while the returned snapshot retains the already-rendered values.
    fn apply_native_vertex_display_side_effects<H: ProgramHost>(
        &mut self,
        object: RuntimeObjectHandle,
        displayed: bool,
        display_mask: u32,
        dark_reference_translation: Option<[i32; 3]>,
        host: &mut H,
    ) -> Result<Option<[u16; COLOR_COUNT]>, RuntimeError<H::Error>> {
        if !displayed {
            return Ok(None);
        }
        let (zone, executable) = {
            let spawned = self
                .arena
                .get(object.arena)
                .ok_or(RuntimeError::UnknownArenaObject(object.arena))?;
            (spawned.zone(), spawned.origin().executable())
        };
        let (reference, frame_index, status_b, transform, original_colors) = {
            let Some(source) = self
                .machine
                .animation_source(object.vm)
                .map_err(RuntimeError::Vm)?
            else {
                return Ok(None);
            };
            let vm_object = self.machine.object(object.vm).map_err(RuntimeError::Vm)?;
            let Some(reference) = animation_vertex_reference(&source) else {
                // Non-vertex and native no-draw process descriptors have no
                // vertex color or geometry side effects.
                return Ok(None);
            };
            (
                reference,
                vm_object.animation_frame() >> 8,
                vm_object
                    .register(process_register::STATUS_B)
                    .map_err(RuntimeError::Vm)?,
                vm_object.retail_transform().map_err(RuntimeError::Vm)?,
                *vm_object.retail_colors(),
            )
        };
        let Some(vertex_kind) = host
            .animation_display_vertex_kind(AnimationBoundBinding {
                object,
                zone,
                executable,
                reference,
                frame_index,
            })
            .map_err(RuntimeError::Program)?
        else {
            return Ok(None);
        };

        let is_main = object.arena.is_dedicated_main();
        let mut effective_colors = original_colors;
        let two_dimensional_cvtx =
            vertex_kind == ObjectVertexKind::Colored && status_b & 0x200 != 0;
        if display_mask & 0x1_0000 == 0
            && !is_main
            && status_b & 0x400 == 0
            && !two_dimensional_cvtx
            && let Some((mode, zone_colors, depth_anchor)) =
                self.machine.current_retail_object_shader()
            && let Some(camera) = self.machine.transform_vectors_camera()
        {
            let graphics_flags = self
                .level_state_context
                .as_ref()
                .map_or(0, |context| context.graphics_flags);
            let camera = camera.for_object_display(graphics_flags, self.machine.frames_elapsed());
            let camera_depth = camera.camera_space_point(transform.translation)[2];
            let projection = i32::try_from(camera.screen_projection).unwrap_or(i32::MAX);
            if status_b & 0x4_0000 != 0 || projection < camera_depth {
                if mode == 4 {
                    // Native assigns this clamp into renderer BSS only after
                    // the mode-four object reaches the shader.
                    self.level_shader.distance = self.level_shader.distance.max(1);
                }
                let dark =
                    dark_reference_translation.map(|reference_translation| ObjectDarkShaderInput {
                        reference_translation,
                        object_translation: transform.translation,
                        dark_distance: self.level_shader.distance,
                    });
                if let Some(shading) = apply_retail_object_zone_shader(
                    mode,
                    vertex_kind,
                    original_colors,
                    zone_colors,
                    camera_depth,
                    depth_anchor,
                    dark,
                )
                .map_err(RuntimeError::ObjectZoneShader)?
                {
                    effective_colors = shading.colors;
                    self.machine
                        .object_mut(object.vm)
                        .map_err(RuntimeError::Vm)?
                        .set_retail_display_colors(effective_colors);
                }
            }
        }

        let reset_zone = if zone == Eid::NONE {
            self.level_state_context
                .as_ref()
                .map(|context| context.location.path.zone)
                .filter(|zone| *zone != Eid::NONE)
        } else {
            Some(zone)
        };
        if status_b & 0x10_0000 != 0
            && let Some(reset_zone) = reset_zone
            && let Some(environment) = host
                .zone_environment(reset_zone)
                .map_err(RuntimeError::Program)?
        {
            let reset = if is_main {
                environment.player_colors
            } else {
                environment.object_colors
            };
            self.machine
                .object_mut(object.vm)
                .map_err(RuntimeError::Vm)?
                .set_retail_display_colors(reset);
        }
        Ok(Some(effective_colors))
    }

    fn retail_display_enabled_at(
        &self,
        object: RuntimeObjectHandle,
        display_mask: u32,
    ) -> Result<bool, VmError> {
        let vm_object = self.machine.object(object.vm)?;
        let status_b = vm_object.register(process_register::STATUS_B)?;
        Ok(retail_display_mask_enabled(
            display_mask,
            status_b,
            vm_object.state_flags(),
            vm_object
                .program_identity()
                .map(GoolProgramIdentity::category),
            self.machine.animation_source(object.vm)?.is_some(),
        ))
    }

    fn begin_native_object_update<E>(
        &mut self,
        object: RuntimeObjectHandle,
    ) -> Result<Option<Execution>, RuntimeError<E>> {
        let frame_stamp = self.machine.frames_elapsed();
        let vm_object = self
            .machine
            .object_mut(object.vm)
            .map_err(RuntimeError::Vm)?;
        let status_b = vm_object
            .register(process_register::STATUS_B)
            .map_err(RuntimeError::Vm)?;
        let counter = vm_object
            .register(process_register::ANIMATION_COUNTER)
            .map_err(RuntimeError::Vm)?;
        if status_b & STALL_STATUS_B != 0 && counter != 0 {
            let remaining = counter - 1;
            vm_object
                .set_register(process_register::ANIMATION_COUNTER, remaining)
                .map_err(RuntimeError::Vm)?;
            if remaining == 0 {
                vm_object
                    .set_register(process_register::STATUS_B, status_b & !STALL_STATUS_B)
                    .map_err(RuntimeError::Vm)?;
            }
            return Ok(Some(Execution {
                reason: HaltReason::NativeStall { remaining },
                steps: 0,
            }));
        }
        vm_object
            .set_register(process_register::ANIMATION_STAMP, frame_stamp)
            .map_err(RuntimeError::Vm)?;
        Ok(None)
    }

    fn finish_native_object_update<H: ProgramHost>(
        &mut self,
        object: RuntimeObjectHandle,
        host: &mut H,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
    ) -> Result<(), RuntimeError<H::Error>> {
        if !self.handles.is_live_pair(object) {
            return Ok(());
        }
        let mut hook_error = None;
        let mut candidate_generations = BTreeMap::new();
        let mut candidate_generations_captured = false;
        let register_collision_bound = {
            let Self {
                arena,
                machine,
                handles,
                pending_states,
                pending_cleanup_actions,
                reclaim_event_faults,
                solid_event_faults,
                invincibility_event_faults,
                level,
                level_state_context,
                saved_level_state,
                transition_zone_context,
                ..
            } = self;
            let colors_completed = machine
                .run_retail_object_colors_with_event_handler(
                    object.vm,
                    |machine, sender_vm, recipient_vm, event| {
                        let Some(sender) = handles.for_vm(sender_vm) else {
                            hook_error = Some(RuntimeError::UnknownVmObject(sender_vm));
                            return;
                        };
                        let Some(recipient) = handles.for_vm(recipient_vm) else {
                            hook_error = Some(RuntimeError::UnknownVmObject(recipient_vm));
                            return;
                        };
                        if sender != object || !handles.is_live_pair(sender) {
                            hook_error = Some(RuntimeError::UnknownVmObject(sender_vm));
                            return;
                        }
                        if !handles.is_live_pair(recipient) {
                            hook_error = Some(RuntimeError::UnknownVmObject(recipient_vm));
                            return;
                        }
                        // Source passes argc=1 with a null argv pointer here.
                        // Preserve the observable zero word without reproducing
                        // that undefined C dereference in the checked VM.
                        let dispatch = Self::dispatch_event_parts_current(
                            arena,
                            handles,
                            machine,
                            pending_states,
                            pending_cleanup_actions,
                            reclaim_event_faults,
                            *level,
                            level_state_context.as_ref(),
                            saved_level_state,
                            *transition_zone_context,
                            host,
                            Some(object.vm),
                            Some(sender),
                            Some(recipient),
                            event,
                            Some(&[0]),
                            None,
                            spawned_children,
                        );
                        if dispatch.is_err() {
                            invincibility_event_faults.push(RuntimeInvincibilityEventFault {
                                sender,
                                recipient,
                                event,
                            });
                        }
                    },
                )
                .map_err(RuntimeError::Vm)?;
            if let Some(error) = hook_error.take() {
                return Err(error);
            }
            if !colors_completed || !handles.is_live_pair(object) {
                return Ok(());
            }
            let physics = machine
                .run_retail_object_physics_after_colors_with_solid_event_handler(
                    object.vm,
                    |machine, _moving_vm, candidates, effect| {
                        if !candidate_generations_captured {
                            candidate_generations_captured = true;
                            for candidate in candidates.iter_mut() {
                                if !candidate.active {
                                    continue;
                                }
                                let Ok(index) = u16::try_from(candidate.id) else {
                                    hook_error =
                                        Some(RuntimeError::Vm(VmError::ArithmeticOverflow));
                                    return false;
                                };
                                let Some(vm) = VmObjectHandle::new(index) else {
                                    hook_error =
                                        Some(RuntimeError::Vm(VmError::ArithmeticOverflow));
                                    return false;
                                };
                                let Some(generation) = handles.for_vm(vm) else {
                                    hook_error = Some(RuntimeError::UnknownVmObject(vm));
                                    return false;
                                };
                                candidate_generations.insert(candidate.id, generation);
                            }
                        }
                        let SolidEffect::SendEvent {
                            target,
                            event,
                            argument,
                            reason,
                        } = effect
                        else {
                            return true;
                        };
                        let recipient = match target {
                            SolidEventTarget::MovingObject => object,
                            SolidEventTarget::Candidate(candidate) => {
                                let Some(recipient) =
                                    candidate_generations.get(&candidate).copied()
                                else {
                                    hook_error =
                                        Some(RuntimeError::Vm(VmError::ArithmeticOverflow));
                                    return false;
                                };
                                if !handles.is_live_pair(recipient) {
                                    hook_error = Some(RuntimeError::UnknownVmObject(recipient.vm));
                                    return false;
                                }
                                recipient
                            }
                        };
                        let zone_before_dispatch = match Self::publish_inline_solid_mover_zone(
                            arena, machine, object, host,
                        ) {
                            Ok(zone) => zone,
                            Err(error) => {
                                hook_error = Some(error);
                                return false;
                            }
                        };
                        let sender = matches!(reason, SolidEventReason::ObjectHitFromBelow)
                            .then_some(object);
                        let dispatch = Self::dispatch_event_parts_current(
                            arena,
                            handles,
                            machine,
                            pending_states,
                            pending_cleanup_actions,
                            reclaim_event_faults,
                            *level,
                            level_state_context.as_ref(),
                            saved_level_state,
                            *transition_zone_context,
                            host,
                            Some(object.vm),
                            sender,
                            Some(recipient),
                            event,
                            Some(&[argument]),
                            None,
                            spawned_children,
                        );
                        // Native solid callers discard `GoolSendEvent`'s
                        // status. Preserve the fault for diagnostics, then
                        // continue from the now-live process state.
                        if dispatch.is_err() {
                            solid_event_faults.push(RuntimeSolidEventFault {
                                moving_object: object,
                                recipient,
                                event,
                                reason,
                            });
                        }
                        let mover_live = handles.is_live_pair(object);
                        if mover_live {
                            let Some(spawned) = arena.get(object.arena) else {
                                hook_error = Some(RuntimeError::UnknownArenaObject(object.arena));
                                return false;
                            };
                            let live_zone = spawned.zone();
                            let environment =
                                if live_zone != zone_before_dispatch && live_zone != Eid::NONE {
                                    match host.solid_environment(live_zone) {
                                        Ok(environment) => environment,
                                        Err(error) => {
                                            hook_error = Some(RuntimeError::Program(error));
                                            return false;
                                        }
                                    }
                                } else {
                                    None
                                };
                            let zone = (live_zone != Eid::NONE).then_some(live_zone);
                            let Ok(vm_object) = machine.object_mut(object.vm) else {
                                hook_error = Some(RuntimeError::UnknownVmObject(object.vm));
                                return false;
                            };
                            if let Some(environment) = environment {
                                vm_object.refresh_retail_object_zone_environment(environment);
                            }
                            vm_object.set_retail_solid_zone_eid(zone);
                        }
                        if let Err(error) = Self::refresh_inline_solid_candidates::<H::Error>(
                            handles,
                            machine,
                            &candidate_generations,
                            candidates,
                        ) {
                            hook_error = Some(error);
                            return false;
                        }
                        // Native ignores GoolSendEvent's result and finishes
                        // TransSmoothStopAtSolid even when the handler has
                        // requested a level restart. The outer frame observes
                        // that request immediately after physics. Only a
                        // released mover makes further checked work unsafe.
                        mover_live
                    },
                )
                .map_err(RuntimeError::Vm)?;
            physics.register_collision_bound
        };
        if let Some(error) = hook_error {
            return Err(error);
        }
        if !self.handles.is_live_pair(object) {
            return Ok(());
        }
        if self.machine.level_restart_requested() {
            // `GoolObjectPhysics` finishes its collidable stamp/range tail
            // before the outer update observes a synchronous LoadState.
            // The destination mount does not begin until the frame returns,
            // so the still-live object must append/invalidate its late bound
            // first even though the remaining update phases are skipped.
            if register_collision_bound {
                self.register_late_animation_bound(object, host)?;
            }
            return Ok(());
        }
        if let Some(zone) = self
            .machine
            .object(object.vm)
            .map_err(RuntimeError::Vm)?
            .retail_solid_zone_eid()
            && zone != Eid::NONE
            && self
                .arena
                .get(object.arena)
                .ok_or(RuntimeError::UnknownArenaObject(object.arena))?
                .zone()
                != zone
        {
            let environment = host
                .solid_environment(zone)
                .map_err(RuntimeError::Program)?;
            self.arena
                .set_zone(object.arena, zone)
                .map_err(RuntimeError::Tree)?;
            if let Some(environment) = environment {
                self.machine
                    .object_mut(object.vm)
                    .map_err(RuntimeError::Vm)?
                    .refresh_retail_object_zone_environment(environment);
            }
        }
        if register_collision_bound {
            self.register_late_animation_bound(object, host)?;
        }
        let vm_object = self
            .machine
            .object_mut(object.vm)
            .map_err(RuntimeError::Vm)?;
        let status_a = vm_object
            .register(process_register::STATUS_A)
            .map_err(RuntimeError::Vm)?;
        vm_object
            .set_register(process_register::STATUS_A, status_a & !FIRST_FRAME_STATUS_A)
            .map_err(RuntimeError::Vm)
    }

    fn refresh_current_solid_environment<H: ProgramHost>(
        &mut self,
        host: &mut H,
    ) -> Result<(), RuntimeError<H::Error>> {
        let Some(zone) = self
            .level_state_context
            .as_ref()
            .map(|context| context.location.path.zone)
        else {
            return Ok(());
        };
        if self.current_solid_zone == Some(zone) {
            return Ok(());
        }
        let environment = host
            .solid_environment(zone)
            .map_err(RuntimeError::Program)?;
        self.machine
            .set_current_retail_solid_environment(environment);
        self.current_solid_zone = Some(zone);
        Ok(())
    }

    fn refresh_inline_solid_candidates<E>(
        handles: &HandleMap,
        machine: &Machine,
        generations: &BTreeMap<u32, RuntimeObjectHandle>,
        candidates: &mut [SolidObjectCandidate],
    ) -> Result<(), RuntimeError<E>> {
        for candidate in candidates {
            let Some(generation) = generations.get(&candidate.id).copied() else {
                candidate.active = false;
                continue;
            };
            if !handles.is_live_pair(generation) {
                candidate.active = false;
                continue;
            }
            let object = machine.object(generation.vm).map_err(RuntimeError::Vm)?;
            let identity = object.program_identity();
            candidate.active = true;
            candidate.translation = Vec3 {
                x: object
                    .register(process_register::TRANSLATION_X)
                    .map_err(RuntimeError::Vm)? as i32,
                y: object
                    .register(process_register::TRANSLATION_Y)
                    .map_err(RuntimeError::Vm)? as i32,
                z: object
                    .register(process_register::TRANSLATION_Z)
                    .map_err(RuntimeError::Vm)? as i32,
            };
            candidate.status_b = object
                .register(process_register::STATUS_B)
                .map_err(RuntimeError::Vm)?;
            candidate.status_c = object
                .register(process_register::STATUS_C)
                .map_err(RuntimeError::Vm)?;
            candidate.state_flags = object
                .register(process_register::STATE_FLAGS)
                .map_err(RuntimeError::Vm)?;
            candidate.category = identity.map_or(0, GoolProgramIdentity::category);
            candidate.object_type = identity.map_or(0, GoolProgramIdentity::object_type);
            candidate.hotspot_size = object
                .register(process_register::HOTSPOT_SIZE)
                .map_err(RuntimeError::Vm)? as i32;
        }
        Ok(())
    }

    fn publish_inline_solid_mover_zone<H: ProgramHost>(
        arena: &mut ObjectArena,
        machine: &mut Machine,
        object: RuntimeObjectHandle,
        host: &mut H,
    ) -> Result<Eid, RuntimeError<H::Error>> {
        let zone = machine
            .object(object.vm)
            .map_err(RuntimeError::Vm)?
            .retail_solid_zone_eid()
            .unwrap_or(Eid::NONE);
        let current = arena
            .get(object.arena)
            .ok_or(RuntimeError::UnknownArenaObject(object.arena))?
            .zone();
        if zone == current {
            return Ok(zone);
        }
        let environment = if zone == Eid::NONE {
            None
        } else {
            host.solid_environment(zone)
                .map_err(RuntimeError::Program)?
        };
        arena
            .set_zone(object.arena, zone)
            .map_err(RuntimeError::Tree)?;
        if let Some(environment) = environment {
            let vm_object = machine.object_mut(object.vm).map_err(RuntimeError::Vm)?;
            vm_object.refresh_retail_object_zone_environment(environment);
            vm_object.set_retail_solid_zone_eid(Some(zone));
        }
        Ok(zone)
    }

    fn live_main_object<E>(&self) -> Result<Option<RuntimeObjectHandle>, RuntimeError<E>> {
        let Some(arena) = self.arena.main_object() else {
            return Ok(None);
        };
        let object = self
            .handles
            .for_arena(arena)
            .filter(|object| self.handles.is_live_pair(*object))
            .ok_or(RuntimeError::UnknownArenaObject(arena))?;
        self.machine.object(object.vm).map_err(RuntimeError::Vm)?;
        Ok(Some(object))
    }

    fn animation_stamp_matches_main<E>(
        &self,
        object: RuntimeObjectHandle,
    ) -> Result<bool, RuntimeError<E>> {
        let Some(main) = self.live_main_object()? else {
            return Ok(false);
        };
        let object_stamp = self
            .machine
            .object(object.vm)
            .map_err(RuntimeError::Vm)?
            .register(process_register::ANIMATION_STAMP)
            .map_err(RuntimeError::Vm)?;
        let main_stamp = self
            .machine
            .object(main.vm)
            .map_err(RuntimeError::Vm)?
            .register(process_register::ANIMATION_STAMP)
            .map_err(RuntimeError::Vm)?;
        Ok(object_stamp == main_stamp)
    }

    fn register_late_animation_bound<H: ProgramHost>(
        &mut self,
        object: RuntimeObjectHandle,
        host: &mut H,
    ) -> Result<(), RuntimeError<H::Error>> {
        let Some(main) = self.live_main_object()? else {
            return Ok(());
        };
        let (object_stamp, object_translation) = {
            let vm_object = self.machine.object(object.vm).map_err(RuntimeError::Vm)?;
            (
                vm_object
                    .register(process_register::ANIMATION_STAMP)
                    .map_err(RuntimeError::Vm)?,
                vm_object
                    .retail_transform()
                    .map_err(RuntimeError::Vm)?
                    .translation,
            )
        };
        let (main_stamp, main_translation) = {
            let vm_object = self.machine.object(main.vm).map_err(RuntimeError::Vm)?;
            (
                vm_object
                    .register(process_register::ANIMATION_STAMP)
                    .map_err(RuntimeError::Vm)?,
                vm_object
                    .retail_transform()
                    .map_err(RuntimeError::Vm)?
                    .translation,
            )
        };
        if object_stamp == main_stamp {
            return Ok(());
        }
        if translation_outside_bound_range(object_translation, main_translation, LATE_BOUND_RANGE) {
            let vm_object = self
                .machine
                .object_mut(object.vm)
                .map_err(RuntimeError::Vm)?;
            let status_a = vm_object
                .register(process_register::STATUS_A)
                .map_err(RuntimeError::Vm)?;
            vm_object
                .set_register(
                    process_register::STATUS_A,
                    status_a | LOCAL_BOUND_INVALID_STATUS_A,
                )
                .map_err(RuntimeError::Vm)?;
            return Ok(());
        }
        self.register_animation_bound(object, host).map(|_| ())
    }

    fn register_animation_bound<H: ProgramHost>(
        &mut self,
        object: RuntimeObjectHandle,
        host: &mut H,
    ) -> Result<bool, RuntimeError<H::Error>> {
        let (zone, executable) = {
            let spawned = self
                .arena
                .get(object.arena)
                .ok_or(RuntimeError::UnknownArenaObject(object.arena))?;
            (spawned.zone(), spawned.origin().executable())
        };
        let (animation, frame_index, transform, status_a, cached_local_bound) = {
            let vm_object = self.machine.object(object.vm).map_err(RuntimeError::Vm)?;
            let status_b = vm_object
                .register(process_register::STATUS_B)
                .map_err(RuntimeError::Vm)?;
            if status_b & COLLIDABLE_STATUS_B == 0 {
                return Ok(false);
            }
            let Some(animation) = self
                .machine
                .animation_source(object.vm)
                .map_err(RuntimeError::Vm)?
            else {
                return Ok(false);
            };
            (
                animation,
                vm_object.animation_frame() >> 8,
                vm_object.retail_transform().map_err(RuntimeError::Vm)?,
                vm_object
                    .register(process_register::STATUS_A)
                    .map_err(RuntimeError::Vm)?,
                vm_object.retail_local_bound(),
            )
        };
        let source = match animation_vertex_reference(&animation) {
            Some(reference) => {
                let Some(source) = host
                    .animation_bound_source(AnimationBoundBinding {
                        object,
                        zone,
                        executable,
                        reference,
                        frame_index,
                    })
                    .map_err(RuntimeError::Program)?
                else {
                    return Ok(false);
                };
                source
            }
            None => AnimationBoundSource::NonVertex,
        };

        let scale = Vec3 {
            x: transform.scale[0],
            y: transform.scale[1],
            z: transform.scale[2],
        };
        let bound_transform = BoundTransform {
            translation: Vec3 {
                x: transform.translation[0],
                y: transform.translation[1],
                z: transform.translation[2],
            },
            rotation: Angles {
                y: Angle12::new(transform.rotation_yxz[0]),
                x: Angle12::new(transform.rotation_yxz[1]),
                z: Angle12::new(transform.rotation_yxz[2]),
            },
            scale,
        };
        let local_bound = if status_a & LOCAL_BOUND_INVALID_STATUS_A != 0 {
            calculate_local_bound(source, scale, object.arena.is_dedicated_main())
        } else {
            cached_local_bound
        };
        let world_bound = calculate_world_bound(local_bound, source, bound_transform);
        if status_a & LOCAL_BOUND_INVALID_STATUS_A != 0 {
            self.machine
                .object_mut(object.vm)
                .map_err(RuntimeError::Vm)?
                .set_retail_local_bound(local_bound);
        }
        self.machine
            .register_frame_bound(object.vm, world_bound)
            .map_err(RuntimeError::Vm)?;
        let vm_object = self
            .machine
            .object_mut(object.vm)
            .map_err(RuntimeError::Vm)?;
        let status_a = vm_object
            .register(process_register::STATUS_A)
            .map_err(RuntimeError::Vm)?;
        vm_object
            .set_register(
                process_register::STATUS_A,
                status_a & !LOCAL_BOUND_INVALID_STATUS_A,
            )
            .map_err(RuntimeError::Vm)?;
        if let Some(main) = self.live_main_object()? {
            let (object_stamp, main_stamp, crash_bound) = {
                let object_vm = self.machine.object(object.vm).map_err(RuntimeError::Vm)?;
                let main_vm = self.machine.object(main.vm).map_err(RuntimeError::Vm)?;
                let main_translation = main_vm
                    .retail_transform()
                    .map_err(RuntimeError::Vm)?
                    .translation;
                (
                    object_vm
                        .register(process_register::ANIMATION_STAMP)
                        .map_err(RuntimeError::Vm)?,
                    main_vm
                        .register(process_register::ANIMATION_STAMP)
                        .map_err(RuntimeError::Vm)?,
                    main_vm.retail_local_bound().translated(Vec3 {
                        x: main_translation[0],
                        y: main_translation[1],
                        z: main_translation[2],
                    }),
                )
            };
            if object_stamp == main_stamp {
                if bounds_intersect_asymmetric(crash_bound, world_bound) {
                    self.machine
                        .collide_retail_objects(object.vm, world_bound, main.vm, crash_bound)
                        .map_err(RuntimeError::Vm)?;
                } else {
                    self.machine
                        .object_mut(object.vm)
                        .map_err(RuntimeError::Vm)?
                        .set_link(6, None)
                        .map_err(RuntimeError::Vm)?;
                }
            }
        }
        Ok(true)
    }

    fn bind_entity_with_native_reclaim<H: ProgramHost>(
        &mut self,
        zone: Eid,
        entity: &ZoneEntity,
        host: &mut H,
    ) -> Result<RuntimeObjectHandle, RuntimeError<H::Error>> {
        let descriptor = EntitySpawnDescriptor::from(entity);
        // Native performs crate adjacency bookkeeping before consulting the
        // persistent spawn bits. Keep this outside the allocation retry loop:
        // reclaiming a full pool must not advance the ordered entity scan.
        let box_plan = self.begin_retail_box_spawn(entity);
        loop {
            match self.arena.spawn_entity(zone, descriptor) {
                Ok(arena_handle) => {
                    let materialized = self.bind_new_entity(arena_handle, zone, entity, host)?;
                    if let Some(plan) = box_plan {
                        self.finish_retail_box_spawn(materialized, plan)?;
                    }
                    return Ok(materialized.object);
                }
                Err(SpawnError::ObjectPoolFull) => {
                    let candidate = self
                        .arena
                        .first_reclaimable()
                        .map_err(RuntimeError::Tree)?
                        .ok_or(RuntimeError::Spawn(SpawnError::ObjectPoolFull))?;
                    let mut spawned_children = Vec::new();
                    self.reclaim_runtime_subtree(candidate, host, &mut spawned_children)?;
                }
                Err(error @ SpawnError::SpawnBlocked { .. }) => {
                    if box_plan.is_some() {
                        self.box_spawn.boxes_y =
                            self.box_spawn.boxes_y.wrapping_add(BOX_STACK_SPACING);
                        self.set_mount_global(
                            BOXES_Y_GLOBAL,
                            self.box_spawn.boxes_y.cast_unsigned(),
                        );
                    }
                    return Err(RuntimeError::Spawn(error));
                }
                Err(error) => return Err(RuntimeError::Spawn(error)),
            }
        }
    }

    fn begin_retail_box_spawn(&mut self, entity: &ZoneEntity) -> Option<RetailBoxSpawnPlan> {
        if entity.executable != BOX_EXECUTABLE {
            return None;
        }
        let current_point = entity.path_points.first().copied();
        let adjacent = current_point
            .zip(self.box_spawn.previous_entity_point)
            .is_some_and(|(current, previous)| retail_box_points_are_adjacent(current, previous));
        if !adjacent {
            self.reset_retail_box_spawn_state();
        }

        // A terminated/reclaimed predecessor is a defined null link in Rust,
        // never a recycled pointer alias. Preserve the native stack offset and
        // entity adjacency while dropping only the stale object identity.
        let near_box = self.box_spawn.previous_live_box.filter(|object| {
            self.handles.is_live_pair(*object) && self.machine.object(object.vm).is_ok()
        });
        if near_box != self.box_spawn.previous_live_box {
            self.box_spawn.previous_live_box = near_box;
            self.set_mount_global(PREVIOUS_BOX_GLOBAL, 0);
        }
        self.box_spawn.previous_entity_point = current_point;
        Some(RetailBoxSpawnPlan {
            near_box,
            boxes_y: self.box_spawn.boxes_y,
        })
    }

    fn finish_retail_box_spawn<E>(
        &mut self,
        materialized: MaterializedObject,
        plan: RetailBoxSpawnPlan,
    ) -> Result<(), RuntimeError<E>> {
        let object = materialized.object;
        let is_retail_box = self
            .machine
            .object(object.vm)
            .map_err(RuntimeError::Vm)?
            .program_identity()
            .is_some_and(|identity| identity.object_type() == BOX_OBJECT_TYPE);
        if !is_retail_box {
            return Ok(());
        }

        let near_box = plan.near_box.filter(|candidate| {
            self.handles.is_live_pair(*candidate) && self.machine.object(candidate.vm).is_ok()
        });
        let previous_word = near_box.map_or(0, |previous| {
            CollisionObjectReference::new(previous.vm).to_word()
        });
        let current_word = CollisionObjectReference::new(object.vm).to_word();
        if let Some(previous) = near_box {
            self.machine
                .object_mut(previous.vm)
                .map_err(RuntimeError::Vm)?
                .set_register(process_register::MISC_A_Y, current_word)
                .map_err(RuntimeError::Vm)?;
        }

        let y_adjustment = BOX_STACK_SPACING.wrapping_sub(plan.boxes_y);
        let object_vm = self
            .machine
            .object_mut(object.vm)
            .map_err(RuntimeError::Vm)?;
        object_vm
            .set_register(process_register::MISC_A_X, previous_word)
            .map_err(RuntimeError::Vm)?;
        object_vm
            .set_register(process_register::MISC_A_Y, 0)
            .map_err(RuntimeError::Vm)?;
        let translation_y = object_vm
            .register(process_register::TRANSLATION_Y)
            .map_err(RuntimeError::Vm)?
            .cast_signed()
            .wrapping_add(y_adjustment);
        object_vm
            .set_register(
                process_register::TRANSLATION_Y,
                translation_y.cast_unsigned(),
            )
            .map_err(RuntimeError::Vm)?;

        if materialized.environment.is_none_or(|environment| {
            environment.graphics_flags & BOX_NO_STAGGER_GRAPHICS_FLAG == 0
        }) {
            let translation = object_vm
                .retail_transform()
                .map_err(RuntimeError::Vm)?
                .translation;
            let stagger = retail_box_stagger_count(translation);
            object_vm
                .set_register(process_register::ANIMATION_COUNTER, stagger)
                .map_err(RuntimeError::Vm)?;
            if stagger != 0 {
                let status_b = object_vm
                    .register(process_register::STATUS_B)
                    .map_err(RuntimeError::Vm)?;
                object_vm
                    .set_register(process_register::STATUS_B, status_b | STALL_STATUS_B)
                    .map_err(RuntimeError::Vm)?;
            }
        }

        self.box_spawn.previous_live_box = Some(object);
        self.set_mount_global(PREVIOUS_BOX_GLOBAL, current_word);
        self.set_mount_global(BOXES_Y_GLOBAL, self.box_spawn.boxes_y.cast_unsigned());
        // Native stores a transient `zone_entity *` here. The owned path point
        // above is its only required runtime information, so no raw-address
        // surrogate is exposed to GOOL.
        self.set_mount_global(PREVIOUS_BOX_ENTITY_GLOBAL, 0);
        Ok(())
    }

    fn clear_stale_retail_box_links<E>(&mut self) -> Result<(), RuntimeError<E>> {
        if self.box_spawn.previous_live_box.is_some_and(|object| {
            !self.handles.is_live_pair(object) || self.machine.object(object.vm).is_err()
        }) {
            self.box_spawn.previous_live_box = None;
        }
        let previous_word = self.box_spawn.previous_live_box.map_or(0, |object| {
            CollisionObjectReference::new(object.vm).to_word()
        });
        self.set_mount_global(PREVIOUS_BOX_GLOBAL, previous_word);

        let live = self
            .handles
            .vm_by_arena
            .values()
            .copied()
            .collect::<Vec<_>>();
        for vm in live {
            let is_box = self
                .machine
                .object(vm)
                .map_err(RuntimeError::Vm)?
                .program_identity()
                .is_some_and(|identity| identity.object_type() == BOX_OBJECT_TYPE);
            if !is_box {
                continue;
            }
            for register in [process_register::MISC_A_X, process_register::MISC_A_Y] {
                let word = self
                    .machine
                    .object(vm)
                    .map_err(RuntimeError::Vm)?
                    .register(register)
                    .map_err(RuntimeError::Vm)?;
                let stale = CollisionObjectReference::from_word(word)
                    .is_some_and(|reference| self.handles.for_vm(reference.object()).is_none());
                if stale {
                    self.machine
                        .object_mut(vm)
                        .map_err(RuntimeError::Vm)?
                        .set_register(register, 0)
                        .map_err(RuntimeError::Vm)?;
                }
            }
        }
        Ok(())
    }

    fn clear_removed_retail_box_word_references(
        machine: &mut Machine,
        handles: &HandleMap,
        removed: VmObjectHandle,
    ) -> Result<(), VmError> {
        let removed_word = CollisionObjectReference::new(removed).to_word();
        if machine
            .global_word(PREVIOUS_BOX_GLOBAL)
            .is_ok_and(|word| word == removed_word)
        {
            machine.set_global_word(PREVIOUS_BOX_GLOBAL, 0)?;
        }

        // Clear the register aliases before the compact VM handle can be
        // returned to an allocator. This closes the nested TERM/reclaim ABA
        // window even when another object is created before the outer runtime
        // operation regains control and prunes its typed BoxSpawnState.
        let live = handles
            .vm_by_arena
            .values()
            .copied()
            .filter(|vm| *vm != removed)
            .collect::<Vec<_>>();
        for vm in live {
            let Ok(object) = machine.object(vm) else {
                continue;
            };
            if object
                .program_identity()
                .map(GoolProgramIdentity::object_type)
                != Some(BOX_OBJECT_TYPE)
            {
                continue;
            }
            for register in [process_register::MISC_A_X, process_register::MISC_A_Y] {
                if machine.object(vm)?.register(register)? == removed_word {
                    machine.object_mut(vm)?.set_register(register, 0)?;
                }
            }
        }
        Ok(())
    }

    fn reclaim_runtime_subtree<H: ProgramHost>(
        &mut self,
        root: ArenaObjectHandle,
        host: &mut H,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
    ) -> Result<(), RuntimeError<H::Error>> {
        let Self {
            arena,
            machine,
            handles,
            pending_states,
            pending_cleanup_actions,
            reclaim_event_faults,
            level,
            level_state_context,
            saved_level_state,
            transition_zone_context,
            faulted_objects,
            ..
        } = self;
        Self::reclaim_runtime_subtree_parts(
            arena,
            handles,
            machine,
            pending_states,
            pending_cleanup_actions,
            reclaim_event_faults,
            *level,
            level_state_context.as_ref(),
            saved_level_state,
            *transition_zone_context,
            host,
            root,
            spawned_children,
        )?;
        faulted_objects.retain(|object| handles.is_live_pair(*object));
        self.clear_stale_retail_box_links()?;
        Ok(())
    }

    fn bind_new_entity<H: ProgramHost>(
        &mut self,
        arena_handle: ArenaObjectHandle,
        zone: Eid,
        entity: &ZoneEntity,
        host: &mut H,
    ) -> Result<MaterializedObject, RuntimeError<H::Error>> {
        self.handles.prune_stale(&self.arena);
        let object = match self.handles.reserve(arena_handle) {
            Ok(object) => object,
            Err(error) => {
                self.arena
                    .despawn_subtree(arena_handle)
                    .map_err(RuntimeError::Tree)?;
                return Err(error);
            }
        };
        let binding = ProgramBinding {
            object,
            zone,
            executable: entity.executable,
            subtype: entity.subtype,
            origin: ProgramOrigin::Entity(entity),
        };
        let result = self.materialize(binding, host);
        if let Ok(materialized) = result.as_ref().copied()
            && materialized.object.arena.is_dedicated_main()
            && !self.suppress_initial_crash_save
            && self.level_state_context.is_some()
        {
            // `GoolObjectSpawn` establishes the initial death checkpoint as
            // soon as the dedicated `crash`/`main_obj` allocation is bound,
            // including IDs 1..4 and executable 0x2c/0x30 subtype-zero
            // specials. Native's temporary transition guard suppresses only
            // the bonus-return pre-restart scan.
            let initial_save = self.save_level_state(materialized.object, true);
            if self.restricted_direct_boot_save == RestrictedDirectBootSave::Armed {
                if matches!(initial_save, Ok(RetailSaveStateOutcome::RestrictedByZone))
                    && self.saved_level_state.is_none()
                {
                    let _direct_boot_save =
                        self.save_restricted_direct_boot_state(materialized.object);
                }
                self.restricted_direct_boot_save = RestrictedDirectBootSave::Disabled;
            }
        }
        let preserve_spawned_bit = matches!(&result, Err(RuntimeError::Program(_)));
        if result.is_err() {
            self.handles.release(object);
            self.arena
                .despawn_subtree(arena_handle)
                .map_err(RuntimeError::Tree)?;
            if preserve_spawned_bit {
                // GoolObjectSpawn marks a rejected retail program as spawned
                // after returning its pool object. Preserve that persistent
                // handshake without retaining a tree node that has no VM.
                let flags = self
                    .arena
                    .spawn_table()
                    .flags(entity.id)
                    .ok_or(RuntimeError::Spawn(SpawnError::InvalidSpawnId(entity.id)))?;
                self.arena
                    .spawn_table_mut()
                    .set_flags(entity.id, flags | 1)
                    .map_err(RuntimeError::Spawn)?;
            }
        }
        result
    }

    fn run_object<H: ProgramHost>(
        &mut self,
        object: RuntimeObjectHandle,
        host: &mut H,
        budget: usize,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
    ) -> Result<Execution, RuntimeError<H::Error>> {
        let rebound_at_frame_start = self.pending_states.contains_key(&object.vm);
        self.rebind_pending_state(object, host, spawned_children)?;
        if !rebound_at_frame_start {
            self.run_frame_transition(object, host, spawned_children)?;
        }
        if !self.handles.is_live_pair(object) {
            return Ok(Execution {
                reason: HaltReason::ObjectTerminated,
                steps: 0,
            });
        }
        let mut remaining = budget;
        let mut total_steps = 0_usize;
        loop {
            let mut callback_error = None;
            let execution = {
                let Self {
                    arena,
                    machine,
                    handles,
                    pending_states,
                    pending_cleanup_actions,
                    reclaim_event_faults,
                    level,
                    level_state_context,
                    saved_level_state,
                    transition_zone_context,
                    ..
                } = self;
                machine.run_with_host_requests(object.vm, remaining, |machine, request| {
                    let result = Self::apply_host_request(
                        arena,
                        handles,
                        machine,
                        pending_states,
                        pending_cleanup_actions,
                        reclaim_event_faults,
                        *level,
                        level_state_context.as_ref(),
                        saved_level_state,
                        *transition_zone_context,
                        host,
                        Some(object.vm),
                        request,
                        spawned_children,
                    );
                    if let Err(error) = result {
                        callback_error = Some(error);
                        return Err(VmError::MissingHostEffect);
                    }
                    Ok(())
                })
            };
            if let Some(error) = callback_error {
                return Err(error);
            }
            let execution = execution.map_err(RuntimeError::Vm)?;
            total_steps = total_steps.saturating_add(execution.steps);
            let HaltReason::StateChanged(state) = execution.reason else {
                return Ok(Execution {
                    reason: execution.reason,
                    steps: total_steps,
                });
            };

            // Normal GoolObjectUpdate code carries SUSPEND_ON_ANIM but not
            // SUSPEND_ON_RETLNK. A successful state link therefore binds the
            // target (including its transition block), pops its zero wait tag
            // through the next animation gate, and continues interpreting in
            // this same native update.
            self.pending_states.insert(object.vm, state);
            self.rebind_pending_state(object, host, spawned_children)?;
            if !self.handles.is_live_pair(object) {
                return Ok(Execution {
                    reason: HaltReason::ObjectTerminated,
                    steps: total_steps,
                });
            }
            if total_steps >= budget {
                return Ok(Execution {
                    reason: HaltReason::StateChanged(state),
                    steps: total_steps,
                });
            }
            remaining = budget - total_steps;
        }
    }

    /// Runs the current state's transition block at the start of every native
    /// object update. `GoolObjectUpdate` does this before consulting the
    /// animation wait tag; it is not limited to the instant a state is bound.
    fn run_frame_transition<H: ProgramHost>(
        &mut self,
        object: RuntimeObjectHandle,
        host: &mut H,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
    ) -> Result<(), RuntimeError<H::Error>> {
        let mut callback_error = None;
        let execution = {
            let Self {
                arena,
                machine,
                handles,
                pending_states,
                pending_cleanup_actions,
                reclaim_event_faults,
                level,
                level_state_context,
                saved_level_state,
                transition_zone_context,
                ..
            } = self;
            machine.run_transition_with_host_requests(object.vm, |machine, request| {
                let result = Self::apply_host_request(
                    arena,
                    handles,
                    machine,
                    pending_states,
                    pending_cleanup_actions,
                    reclaim_event_faults,
                    *level,
                    level_state_context.as_ref(),
                    saved_level_state,
                    *transition_zone_context,
                    host,
                    Some(object.vm),
                    request,
                    spawned_children,
                );
                if let Err(error) = result {
                    callback_error = Some(error);
                    return Err(VmError::MissingHostEffect);
                }
                Ok(())
            })
        };
        if let Some(error) = callback_error {
            return Err(error);
        }
        if let Some(Execution {
            reason: HaltReason::StateChanged(state),
            ..
        }) = execution.map_err(RuntimeError::Vm)?
        {
            self.pending_states.insert(object.vm, state);
            self.rebind_pending_state(object, host, spawned_children)?;
        }
        Ok(())
    }

    fn rebind_pending_state<H: ProgramHost>(
        &mut self,
        object: RuntimeObjectHandle,
        host: &mut H,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
    ) -> Result<(), RuntimeError<H::Error>> {
        for _ in 0..MAX_SYNCHRONOUS_STATE_CHANGES {
            let Some(state) = self.pending_states.get(&object.vm).copied() else {
                return Ok(());
            };
            let spawned = self
                .arena
                .get(object.arena)
                .ok_or(RuntimeError::UnknownArenaObject(object.arena))?;
            let program = host
                .bind_state_program(StateProgramBinding {
                    object,
                    zone: spawned.zone(),
                    executable: spawned.origin().executable(),
                    state,
                })
                .map_err(RuntimeError::Program)?;
            self.machine
                .rebind_state_program(object.vm, &program, &[])
                .map_err(RuntimeError::Vm)?;
            self.pending_states.remove(&object.vm);

            // `GoolObjectChangeState` executes an armed once block before it
            // writes `state_stamp`, then enters the target state's transition
            // block. Both interpreter invocations stay inside this host
            // boundary so child creation and further state links remain
            // synchronous.
            let mut callback_error = None;
            let once_execution = {
                let Self {
                    arena,
                    machine,
                    handles,
                    pending_states,
                    pending_cleanup_actions,
                    reclaim_event_faults,
                    level,
                    level_state_context,
                    saved_level_state,
                    transition_zone_context,
                    ..
                } = self;
                machine.run_pending_once_with_host_requests(object.vm, |machine, request| {
                    let result = Self::apply_host_request(
                        arena,
                        handles,
                        machine,
                        pending_states,
                        pending_cleanup_actions,
                        reclaim_event_faults,
                        *level,
                        level_state_context.as_ref(),
                        saved_level_state,
                        *transition_zone_context,
                        host,
                        Some(object.vm),
                        request,
                        spawned_children,
                    );
                    if let Err(error) = result {
                        callback_error = Some(error);
                        return Err(VmError::MissingHostEffect);
                    }
                    Ok(())
                })
            };
            if let Some(error) = callback_error {
                return Err(error);
            }
            let once_execution = once_execution.map_err(RuntimeError::Vm)?;
            if !self.handles.is_live_pair(object) {
                return Ok(());
            }
            if self.machine.level_restart_requested() {
                return Ok(());
            }
            if let Some(Execution {
                reason: HaltReason::StateChanged(state),
                ..
            }) = once_execution
            {
                self.pending_states.insert(object.vm, state);
                continue;
            }

            let mut callback_error = None;
            let transition_execution = {
                let Self {
                    arena,
                    machine,
                    handles,
                    pending_states,
                    pending_cleanup_actions,
                    reclaim_event_faults,
                    level,
                    level_state_context,
                    saved_level_state,
                    transition_zone_context,
                    ..
                } = self;
                machine.run_transition_with_host_requests(object.vm, |machine, request| {
                    let result = Self::apply_host_request(
                        arena,
                        handles,
                        machine,
                        pending_states,
                        pending_cleanup_actions,
                        reclaim_event_faults,
                        *level,
                        level_state_context.as_ref(),
                        saved_level_state,
                        *transition_zone_context,
                        host,
                        Some(object.vm),
                        request,
                        spawned_children,
                    );
                    if let Err(error) = result {
                        callback_error = Some(error);
                        return Err(VmError::MissingHostEffect);
                    }
                    Ok(())
                })
            };
            if let Some(error) = callback_error {
                return Err(error);
            }
            let transition_execution = transition_execution.map_err(RuntimeError::Vm)?;
            if !self.handles.is_live_pair(object) {
                return Ok(());
            }
            if self.machine.level_restart_requested() {
                return Ok(());
            }
            if let Some(Execution {
                reason: HaltReason::StateChanged(state),
                ..
            }) = transition_execution
            {
                self.pending_states.insert(object.vm, state);
                continue;
            }
            return Ok(());
        }
        Err(RuntimeError::Vm(
            VmError::SynchronousStateChangeBudgetExhausted(object.vm),
        ))
    }

    fn validate_runtime_object<E>(
        arena: &ObjectArena,
        handles: &HandleMap,
        machine: &Machine,
        object: RuntimeObjectHandle,
    ) -> Result<(), RuntimeError<E>> {
        if arena.get(object.arena).is_none() || handles.for_arena(object.arena) != Some(object) {
            return Err(RuntimeError::UnknownArenaObject(object.arena));
        }
        if handles.for_vm(object.vm) != Some(object) {
            return Err(RuntimeError::UnknownVmObject(object.vm));
        }
        machine.object(object.vm).map_err(RuntimeError::Vm)?;
        Ok(())
    }

    fn validate_different_level_load_state<E>(
        arena: &ObjectArena,
        handles: &HandleMap,
        machine: &mut Machine,
        level: Option<LevelId>,
        saved_level_state: Option<&RetailLevelSnapshot>,
        vm: VmObjectHandle,
    ) -> Result<(), RuntimeError<E>> {
        let caller = handles
            .for_vm(vm)
            .ok_or(RuntimeError::UnknownVmObject(vm))?;
        Self::validate_runtime_object(arena, handles, machine, caller)?;
        let saved_level = saved_level_state
            .ok_or(RuntimeError::MissingSavedLevelState)?
            .level;
        machine
            .resolve_load_state_effect(vm, saved_level)
            .map_err(RuntimeError::Vm)?;
        if BONUS_ROUND_GLOBAL < machine.global_words().len() {
            machine
                .set_global_word(BONUS_ROUND_GLOBAL, 0)
                .map_err(RuntimeError::Vm)?;
        }
        let current_level = level.ok_or(RuntimeError::MissingLevelStateContext)?;
        if saved_level == current_level {
            return Err(RuntimeError::SameLevelRestartDuringLevelEnd(current_level));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_event_parts<H: ProgramHost>(
        arena: &mut ObjectArena,
        handles: &mut HandleMap,
        machine: &mut Machine,
        pending_states: &mut BTreeMap<VmObjectHandle, u16>,
        pending_cleanup_actions: &mut Vec<RuntimeCleanupAction>,
        reclaim_event_faults: &mut Vec<RuntimeReclaimEventFault>,
        level: Option<LevelId>,
        level_state_context: Option<&RetailLevelStateContext>,
        saved_level_state: &mut Option<RetailLevelSnapshot>,
        transition_zone_context: ObjectZoneContext,
        host: &mut H,
        sender: Option<RuntimeObjectHandle>,
        recipient: Option<RuntimeObjectHandle>,
        event: u32,
        arguments: Option<&[u32]>,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
    ) -> Result<EventDispatchOutcome, RuntimeError<H::Error>> {
        Self::dispatch_event_parts_mode(
            arena,
            handles,
            machine,
            pending_states,
            pending_cleanup_actions,
            reclaim_event_faults,
            level,
            level_state_context,
            saved_level_state,
            transition_zone_context,
            host,
            sender,
            recipient,
            event,
            arguments,
            None,
            spawned_children,
            EventLoadStateMode::RequestRestart,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_event_parts_current<H: ProgramHost>(
        arena: &mut ObjectArena,
        handles: &mut HandleMap,
        machine: &mut Machine,
        pending_states: &mut BTreeMap<VmObjectHandle, u16>,
        pending_cleanup_actions: &mut Vec<RuntimeCleanupAction>,
        reclaim_event_faults: &mut Vec<RuntimeReclaimEventFault>,
        level: Option<LevelId>,
        level_state_context: Option<&RetailLevelStateContext>,
        saved_level_state: &mut Option<RetailLevelSnapshot>,
        transition_zone_context: ObjectZoneContext,
        host: &mut H,
        current_object: Option<VmObjectHandle>,
        sender: Option<RuntimeObjectHandle>,
        recipient: Option<RuntimeObjectHandle>,
        event: u32,
        arguments: Option<&[u32]>,
        argument_pool_slots: Option<&[Option<u8>]>,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
    ) -> Result<EventDispatchOutcome, RuntimeError<H::Error>> {
        Self::dispatch_event_parts_mode(
            arena,
            handles,
            machine,
            pending_states,
            pending_cleanup_actions,
            reclaim_event_faults,
            level,
            level_state_context,
            saved_level_state,
            transition_zone_context,
            host,
            sender,
            recipient,
            event,
            arguments,
            argument_pool_slots,
            spawned_children,
            EventLoadStateMode::RequestRestart,
            current_object,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_event_parts_mode<H: ProgramHost>(
        arena: &mut ObjectArena,
        handles: &mut HandleMap,
        machine: &mut Machine,
        pending_states: &mut BTreeMap<VmObjectHandle, u16>,
        pending_cleanup_actions: &mut Vec<RuntimeCleanupAction>,
        reclaim_event_faults: &mut Vec<RuntimeReclaimEventFault>,
        level: Option<LevelId>,
        level_state_context: Option<&RetailLevelStateContext>,
        saved_level_state: &mut Option<RetailLevelSnapshot>,
        transition_zone_context: ObjectZoneContext,
        host: &mut H,
        sender: Option<RuntimeObjectHandle>,
        recipient: Option<RuntimeObjectHandle>,
        event: u32,
        arguments: Option<&[u32]>,
        argument_pool_slots: Option<&[Option<u8>]>,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
        load_state_mode: EventLoadStateMode,
        current_object: Option<VmObjectHandle>,
    ) -> Result<EventDispatchOutcome, RuntimeError<H::Error>> {
        // Native GoolSendEvent is one immediate recipient transaction. Keep
        // every effect in caller-visible order, but reset the bounded burst
        // counter at both sides so a 96-object TERM or broadcast traversal is
        // not mistaken for one uninterrupted interpreter transaction.
        machine.checkpoint_effects();
        let result = Self::dispatch_event_parts_mode_inner(
            arena,
            handles,
            machine,
            pending_states,
            pending_cleanup_actions,
            reclaim_event_faults,
            level,
            level_state_context,
            saved_level_state,
            transition_zone_context,
            host,
            sender,
            recipient,
            event,
            arguments,
            argument_pool_slots,
            spawned_children,
            load_state_mode,
            current_object,
        );
        machine.checkpoint_effects();
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_event_parts_mode_inner<H: ProgramHost>(
        arena: &mut ObjectArena,
        handles: &mut HandleMap,
        machine: &mut Machine,
        pending_states: &mut BTreeMap<VmObjectHandle, u16>,
        pending_cleanup_actions: &mut Vec<RuntimeCleanupAction>,
        reclaim_event_faults: &mut Vec<RuntimeReclaimEventFault>,
        level: Option<LevelId>,
        level_state_context: Option<&RetailLevelStateContext>,
        saved_level_state: &mut Option<RetailLevelSnapshot>,
        transition_zone_context: ObjectZoneContext,
        host: &mut H,
        sender: Option<RuntimeObjectHandle>,
        recipient: Option<RuntimeObjectHandle>,
        event: u32,
        arguments: Option<&[u32]>,
        argument_pool_slots: Option<&[Option<u8>]>,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
        load_state_mode: EventLoadStateMode,
        current_object: Option<VmObjectHandle>,
    ) -> Result<EventDispatchOutcome, RuntimeError<H::Error>> {
        if let Some(sender) = sender {
            Self::validate_runtime_object(arena, handles, machine, sender)?;
        }
        if let Some(recipient) = recipient {
            Self::validate_runtime_object(arena, handles, machine, recipient)?;
        }

        let mut callback_error = None;
        let outcome = machine.send_event_with_host_requests_and_pool_slots(
            sender.map(RuntimeObjectHandle::vm),
            recipient.map(RuntimeObjectHandle::vm),
            event,
            arguments,
            argument_pool_slots,
            |machine, request| {
                let result = match request {
                    VmHostRequest::Effect(VmEffect::LoadState { object: vm, .. })
                        if load_state_mode == EventLoadStateMode::ContinueDifferentLevel =>
                    {
                        Self::validate_different_level_load_state(
                            arena,
                            handles,
                            machine,
                            level,
                            saved_level_state.as_ref(),
                            vm,
                        )
                    }
                    request => Self::apply_host_request(
                        arena,
                        handles,
                        machine,
                        pending_states,
                        pending_cleanup_actions,
                        reclaim_event_faults,
                        level,
                        level_state_context,
                        saved_level_state,
                        transition_zone_context,
                        host,
                        current_object,
                        request,
                        spawned_children,
                    ),
                };
                if let Err(error) = result {
                    callback_error = Some(error);
                    return Err(VmError::MissingHostEffect);
                }
                Ok(())
            },
        );
        if let Some(error) = callback_error {
            return Err(error);
        }
        let outcome = outcome.map_err(RuntimeError::Vm)?;
        if machine.level_restart_requested() {
            return Ok(outcome);
        }
        if let Some(change) = &outcome.state_change {
            Self::rebind_event_state_change_parts(
                arena,
                handles,
                machine,
                pending_states,
                pending_cleanup_actions,
                reclaim_event_faults,
                level,
                level_state_context,
                saved_level_state,
                transition_zone_context,
                host,
                change,
                spawned_children,
                current_object,
            )?;
        }
        Ok(outcome)
    }

    #[allow(clippy::too_many_arguments)]
    fn rebind_event_state_change_parts<H: ProgramHost>(
        arena: &mut ObjectArena,
        handles: &mut HandleMap,
        machine: &mut Machine,
        pending_states: &mut BTreeMap<VmObjectHandle, u16>,
        pending_cleanup_actions: &mut Vec<RuntimeCleanupAction>,
        reclaim_event_faults: &mut Vec<RuntimeReclaimEventFault>,
        level: Option<LevelId>,
        level_state_context: Option<&RetailLevelStateContext>,
        saved_level_state: &mut Option<RetailLevelSnapshot>,
        transition_zone_context: ObjectZoneContext,
        host: &mut H,
        change: &EventStateChange,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
        current_object: Option<VmObjectHandle>,
    ) -> Result<(), RuntimeError<H::Error>> {
        let object = handles
            .for_vm(change.recipient)
            .ok_or(RuntimeError::UnknownVmObject(change.recipient))?;
        Self::validate_runtime_object(arena, handles, machine, object)?;
        let mut state = change.state;
        let mut use_event_arguments = true;

        for _ in 0..MAX_SYNCHRONOUS_STATE_CHANGES {
            let spawned = arena
                .get(object.arena)
                .ok_or(RuntimeError::UnknownArenaObject(object.arena))?;
            let program = host
                .bind_state_program(StateProgramBinding {
                    object,
                    zone: spawned.zone(),
                    executable: spawned.origin().executable(),
                    state,
                })
                .map_err(RuntimeError::Program)?;
            let arguments = if use_event_arguments {
                change.arguments.as_slice()
            } else {
                &[]
            };
            let argument_pool_slots = if use_event_arguments {
                change.argument_pool_slots.as_slice()
            } else {
                &[]
            };
            machine
                .rebind_state_program_with_pool_slots(
                    object.vm,
                    &program,
                    arguments,
                    argument_pool_slots,
                )
                .map_err(RuntimeError::Vm)?;
            pending_states.remove(&object.vm);
            arena
                .set_state_flags(
                    object.arena,
                    machine
                        .object(object.vm)
                        .map_err(RuntimeError::Vm)?
                        .state_flags(),
                )
                .map_err(RuntimeError::Tree)?;

            // An armed once block always runs inside GoolObjectChangeState.
            // A state link from that block immediately replaces the just-bound
            // state and restarts this synchronous transaction with no argv.
            let mut callback_error = None;
            let once =
                machine.run_pending_once_with_host_requests(object.vm, |machine, request| {
                    let result = Self::apply_host_request(
                        arena,
                        handles,
                        machine,
                        pending_states,
                        pending_cleanup_actions,
                        reclaim_event_faults,
                        level,
                        level_state_context,
                        saved_level_state,
                        transition_zone_context,
                        host,
                        current_object,
                        request,
                        spawned_children,
                    );
                    if let Err(error) = result {
                        callback_error = Some(error);
                        return Err(VmError::MissingHostEffect);
                    }
                    Ok(())
                });
            if let Some(error) = callback_error {
                return Err(error);
            }
            let once = once.map_err(RuntimeError::Vm)?;
            if !handles.is_live_pair(object) || machine.level_restart_requested() {
                return Ok(());
            }
            if let Some(Execution {
                reason: HaltReason::StateChanged(next_state),
                ..
            }) = once
            {
                state = next_state;
                use_event_arguments = false;
                continue;
            }

            // `cur_obj` remains the outer frame object while nested event
            // interpreters run. Only a state change targeting that identity
            // enters the new transition block before event-stack cleanup.
            if current_object == Some(object.vm) {
                let mut callback_error = None;
                let transition =
                    machine.run_transition_with_host_requests(object.vm, |machine, request| {
                        let result = Self::apply_host_request(
                            arena,
                            handles,
                            machine,
                            pending_states,
                            pending_cleanup_actions,
                            reclaim_event_faults,
                            level,
                            level_state_context,
                            saved_level_state,
                            transition_zone_context,
                            host,
                            current_object,
                            request,
                            spawned_children,
                        );
                        if let Err(error) = result {
                            callback_error = Some(error);
                            return Err(VmError::MissingHostEffect);
                        }
                        Ok(())
                    });
                if let Some(error) = callback_error {
                    return Err(error);
                }
                let transition = transition.map_err(RuntimeError::Vm)?;
                if !handles.is_live_pair(object) || machine.level_restart_requested() {
                    return Ok(());
                }
                if let Some(Execution {
                    reason: HaltReason::StateChanged(next_state),
                    ..
                }) = transition
                {
                    state = next_state;
                    use_event_arguments = false;
                    continue;
                }
            }

            return arena
                .set_state_flags(
                    object.arena,
                    machine
                        .object(object.vm)
                        .map_err(RuntimeError::Vm)?
                        .state_flags(),
                )
                .map_err(RuntimeError::Tree);
        }

        Err(RuntimeError::Vm(
            VmError::SynchronousStateChangeBudgetExhausted(object.vm),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn terminate_zone_roots_live_parts<H: ProgramHost>(
        arena: &mut ObjectArena,
        handles: &mut HandleMap,
        machine: &mut Machine,
        pending_states: &mut BTreeMap<VmObjectHandle, u16>,
        pending_cleanup_actions: &mut Vec<RuntimeCleanupAction>,
        reclaim_event_faults: &mut Vec<RuntimeReclaimEventFault>,
        level: Option<LevelId>,
        level_state_context: Option<&RetailLevelStateContext>,
        saved_level_state: &mut Option<RetailLevelSnapshot>,
        object_zone_context: ObjectZoneContext,
        host: &mut H,
        zone: Eid,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
        queue_cleanup_actions: bool,
    ) -> Result<ZoneTerminationReport<H::Error>, RuntimeError<H::Error>> {
        let mut report = ZoneTerminationReport::new();
        for root_index in 0..ROOT_HANDLE_COUNT {
            let root_index =
                u8::try_from(root_index).map_err(|_| RuntimeError::InvalidRootIndex(root_index))?;
            let root = RootHandle::new(root_index)
                .ok_or(RuntimeError::InvalidRootIndex(usize::from(root_index)))?;
            let mut child = arena.root_first_child(root);
            while let Some(arena_handle) = child {
                let Some(spawned) = arena.get(arena_handle) else {
                    // A previously captured raw C sibling may have been freed
                    // by a TERM handler. Generational Rust handles reject that
                    // ABA case instead of following its free-list links.
                    break;
                };
                let sibling = spawned.next_sibling();
                Self::terminate_zone_subtree_live_parts(
                    arena,
                    handles,
                    machine,
                    pending_states,
                    pending_cleanup_actions,
                    reclaim_event_faults,
                    level,
                    level_state_context,
                    saved_level_state,
                    object_zone_context,
                    host,
                    zone,
                    arena_handle,
                    spawned_children,
                    queue_cleanup_actions,
                    &mut report,
                )?;
                child = sibling;
            }
        }
        if !report.terminated.is_empty() {
            machine.clear_frame_bounds();
        }
        Ok(report)
    }

    #[allow(clippy::too_many_arguments)]
    fn terminate_zone_subtree_live_parts<H: ProgramHost>(
        arena: &mut ObjectArena,
        handles: &mut HandleMap,
        machine: &mut Machine,
        pending_states: &mut BTreeMap<VmObjectHandle, u16>,
        pending_cleanup_actions: &mut Vec<RuntimeCleanupAction>,
        reclaim_event_faults: &mut Vec<RuntimeReclaimEventFault>,
        level: Option<LevelId>,
        level_state_context: Option<&RetailLevelStateContext>,
        saved_level_state: &mut Option<RetailLevelSnapshot>,
        object_zone_context: ObjectZoneContext,
        host: &mut H,
        zone: Eid,
        arena_handle: ArenaObjectHandle,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
        queue_cleanup_actions: bool,
        report: &mut ZoneTerminationReport<H::Error>,
    ) -> Result<(), RuntimeError<H::Error>> {
        let Some(spawned) = arena.get(arena_handle) else {
            return Ok(());
        };
        // Native reads the first child at recursive entry, then captures each
        // sibling immediately before descending. Borrows end before TERM so a
        // later sibling's current descendants remain observable.
        let mut child = spawned.first_child();
        while let Some(child_handle) = child {
            let Some(spawned_child) = arena.get(child_handle) else {
                break;
            };
            let sibling = spawned_child.next_sibling();
            Self::terminate_zone_subtree_live_parts(
                arena,
                handles,
                machine,
                pending_states,
                pending_cleanup_actions,
                reclaim_event_faults,
                level,
                level_state_context,
                saved_level_state,
                object_zone_context,
                host,
                zone,
                child_handle,
                spawned_children,
                queue_cleanup_actions,
                report,
            )?;
            child = sibling;
        }

        Self::terminate_zone_candidate_parts(
            arena,
            handles,
            machine,
            pending_states,
            pending_cleanup_actions,
            reclaim_event_faults,
            level,
            level_state_context,
            saved_level_state,
            object_zone_context,
            host,
            zone,
            arena_handle,
            spawned_children,
            queue_cleanup_actions,
            report,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn terminate_zone_candidate_parts<H: ProgramHost>(
        arena: &mut ObjectArena,
        handles: &mut HandleMap,
        machine: &mut Machine,
        pending_states: &mut BTreeMap<VmObjectHandle, u16>,
        pending_cleanup_actions: &mut Vec<RuntimeCleanupAction>,
        reclaim_event_faults: &mut Vec<RuntimeReclaimEventFault>,
        level: Option<LevelId>,
        level_state_context: Option<&RetailLevelStateContext>,
        saved_level_state: &mut Option<RetailLevelSnapshot>,
        object_zone_context: ObjectZoneContext,
        host: &mut H,
        zone: Eid,
        arena_handle: ArenaObjectHandle,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
        queue_cleanup_actions: bool,
        report: &mut ZoneTerminationReport<H::Error>,
    ) -> Result<(), RuntimeError<H::Error>> {
        let Some(spawned) = arena.get(arena_handle) else {
            return Ok(());
        };
        if spawned.zone() != zone {
            return Ok(());
        }
        let original_zone = spawned.zone();
        // Retail's `crash` pointer names the dedicated main allocation for
        // every main-selecting entity (including IDs 1..4 and the 0x2c/0x30
        // subtype-zero specials), not only executable-zero Crash.
        let is_crash = arena_handle.is_dedicated_main();
        let object = handles
            .for_arena(arena_handle)
            .ok_or(RuntimeError::UnknownArenaObject(arena_handle))?;
        Self::validate_runtime_object(arena, handles, machine, object)?;
        let vm_object = machine.object(object.vm).map_err(RuntimeError::Vm)?;
        let status_b = vm_object
            .register(process_register::STATUS_B)
            .map_err(RuntimeError::Vm)?;
        if status_b & ZONE_TERMINATION_STATUS_B_IMMUNE != 0
            || vm_object.state_flags() & ZONE_TERMINATION_STATE_IMMUNE != 0
        {
            return Ok(());
        }

        let event_failure = Self::dispatch_event_parts(
            arena,
            handles,
            machine,
            pending_states,
            pending_cleanup_actions,
            reclaim_event_faults,
            level,
            level_state_context,
            saved_level_state,
            object_zone_context,
            host,
            None,
            Some(object),
            TERMINATE_EVENT,
            None,
            spawned_children,
        )
        .err();

        if let Some(error) = event_failure {
            report
                .event_failures
                .push(ZoneTerminationEventFailure { object, error });
        }
        // Nested TERM/misc work may already have reclaimed this exact
        // generation. Native then has no remaining lifecycle work for it.
        if handles.for_arena(arena_handle) != Some(object) || arena.get(arena_handle).is_none() {
            return Ok(());
        }
        let current_zone = arena
            .get(arena_handle)
            .ok_or(RuntimeError::UnknownArenaObject(arena_handle))?
            .zone();
        if object_zone_context != ObjectZoneContext::HardRestartSentinel
            && current_zone != original_zone
        {
            report.migrated.push(object);
            return Ok(());
        }
        if is_crash && level != Some(LevelId::TITLE) {
            return Ok(());
        }

        Self::kill_runtime_subtree_with_host_parts(
            arena,
            handles,
            machine,
            pending_states,
            pending_cleanup_actions,
            host,
            arena_handle,
            spawned_children,
            queue_cleanup_actions,
            report,
        )?;
        Self::refresh_tree_links(arena, handles, machine)
    }

    #[allow(clippy::too_many_arguments)]
    fn kill_runtime_subtree_with_host_parts<H: ProgramHost>(
        arena: &mut ObjectArena,
        handles: &mut HandleMap,
        machine: &mut Machine,
        pending_states: &mut BTreeMap<VmObjectHandle, u16>,
        pending_cleanup_actions: &mut Vec<RuntimeCleanupAction>,
        host: &mut H,
        arena_handle: ArenaObjectHandle,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
        queue_cleanup_actions: bool,
        report: &mut ZoneTerminationReport<H::Error>,
    ) -> Result<(), RuntimeError<H::Error>> {
        let Some(spawned) = arena.get(arena_handle) else {
            return Ok(());
        };
        let mut child = spawned.first_child();
        while let Some(child_handle) = child {
            let sibling = arena
                .get(child_handle)
                .and_then(SpawnedObject::next_sibling);
            Self::kill_runtime_subtree_with_host_parts(
                arena,
                handles,
                machine,
                pending_states,
                pending_cleanup_actions,
                host,
                child_handle,
                spawned_children,
                queue_cleanup_actions,
                report,
            )?;
            child = sibling;
        }

        let Some(object) = handles.for_arena(arena_handle) else {
            return Ok(());
        };
        let spawn_id = Self::live_spawn_id(machine, object.vm)?;
        machine
            .remove_object_for_host_termination_from_retail_pool_slot(
                object.vm,
                object.arena.slot(),
            )
            .map_err(RuntimeError::Vm)?;
        Self::clear_removed_retail_box_word_references(machine, handles, object.vm)
            .map_err(RuntimeError::Vm)?;
        pending_states.remove(&object.vm);
        let audio_freed = host.free_object_audio(object);
        handles.release(object);
        arena
            .despawn_leaf_with_spawn_id(arena_handle, spawn_id)
            .map_err(RuntimeError::Tree)?;
        let spawn_flags = arena
            .spawn_table()
            .flags(spawn_id)
            .ok_or(RuntimeError::Spawn(SpawnError::InvalidSpawnId(spawn_id)))?;
        machine
            .set_spawn_flags(spawn_id, spawn_flags)
            .map_err(RuntimeError::Vm)?;
        spawned_children.retain(|spawned| *spawned != object);
        report.terminated.push(object);
        if !audio_freed {
            let action = RuntimeCleanupAction::FreeObjectAudio(object);
            report.cleanup_actions.push(action);
            if queue_cleanup_actions {
                pending_cleanup_actions.push(action);
            }
        }
        Ok(())
    }

    fn remove_runtime_subtree<E>(
        &mut self,
        root: ArenaObjectHandle,
        report: &mut ZoneTerminationReport<E>,
    ) -> Result<(), RuntimeError<E>> {
        let removed = self
            .arena
            .subtree_postorder_snapshot(root)
            .map_err(RuntimeError::Tree)?;
        let objects = removed
            .into_iter()
            .map(|arena_handle| {
                let object = self
                    .handles
                    .for_arena(arena_handle)
                    .ok_or(RuntimeError::UnknownArenaObject(arena_handle))?;
                let spawn_id = Self::live_spawn_id(&self.machine, object.vm)?;
                Ok((arena_handle, object, spawn_id))
            })
            .collect::<Result<Vec<_>, RuntimeError<E>>>()?;

        for (arena_handle, object, spawn_id) in objects {
            self.machine
                .remove_object_from_retail_pool_slot(object.vm, object.arena.slot())
                .map_err(RuntimeError::Vm)?;
            Self::clear_removed_retail_box_word_references(
                &mut self.machine,
                &self.handles,
                object.vm,
            )
            .map_err(RuntimeError::Vm)?;
            self.pending_states.remove(&object.vm);
            self.faulted_objects.remove(&object);
            self.handles.release(object);
            self.arena
                .despawn_leaf_with_spawn_id(arena_handle, spawn_id)
                .map_err(RuntimeError::Tree)?;
            let spawn_flags = self
                .arena
                .spawn_table()
                .flags(spawn_id)
                .ok_or(RuntimeError::Spawn(SpawnError::InvalidSpawnId(spawn_id)))?;
            self.machine
                .set_spawn_flags(spawn_id, spawn_flags)
                .map_err(RuntimeError::Vm)?;
            report.terminated.push(object);
            report
                .cleanup_actions
                .push(RuntimeCleanupAction::FreeObjectAudio(object));
        }
        self.clear_stale_retail_box_links()?;
        Ok(())
    }

    fn live_spawn_id<E>(machine: &Machine, object: VmObjectHandle) -> Result<u16, RuntimeError<E>> {
        let raw_spawn_id = machine
            .object(object)
            .map_err(RuntimeError::Vm)?
            .register(process_register::PID_FLAGS)
            .map_err(RuntimeError::Vm)?
            >> 8;
        u16::try_from(raw_spawn_id)
            .ok()
            .filter(|id| usize::from(*id) < SPAWN_TABLE_CAPACITY)
            .ok_or(RuntimeError::Spawn(SpawnError::InvalidSpawnId(
                u16::try_from(raw_spawn_id).unwrap_or(u16::MAX),
            )))
    }

    fn refresh_tree_links<E>(
        arena: &ObjectArena,
        handles: &HandleMap,
        machine: &mut Machine,
    ) -> Result<(), RuntimeError<E>> {
        let player = arena
            .main_object()
            .and_then(|arena| handles.for_arena(arena))
            .map(RuntimeObjectHandle::vm);
        let objects = handles
            .vm_by_arena
            .iter()
            .map(|(arena, vm)| RuntimeObjectHandle {
                arena: *arena,
                vm: *vm,
            })
            .collect::<Vec<_>>();
        for object in objects {
            Self::validate_runtime_object(arena, handles, machine, object)?;
            let spawned = arena
                .get(object.arena)
                .ok_or(RuntimeError::UnknownArenaObject(object.arena))?;
            let parent = match spawned.parent() {
                TreeParent::Root(_) => None,
                TreeParent::Object(parent) => Some(
                    handles
                        .for_arena(parent)
                        .ok_or(RuntimeError::UnknownArenaObject(parent))?
                        .vm,
                ),
            };
            let sibling = spawned
                .next_sibling()
                .map(|sibling| {
                    handles
                        .for_arena(sibling)
                        .ok_or(RuntimeError::UnknownArenaObject(sibling))
                        .map(RuntimeObjectHandle::vm)
                })
                .transpose()?;
            let child = spawned
                .first_child()
                .map(|child| {
                    handles
                        .for_arena(child)
                        .ok_or(RuntimeError::UnknownArenaObject(child))
                        .map(RuntimeObjectHandle::vm)
                })
                .transpose()?;
            let vm_object = machine.object_mut(object.vm).map_err(RuntimeError::Vm)?;
            vm_object
                .set_link(0, Some(object.vm))
                .map_err(RuntimeError::Vm)?;
            vm_object.set_link(1, parent).map_err(RuntimeError::Vm)?;
            vm_object.set_link(2, sibling).map_err(RuntimeError::Vm)?;
            vm_object.set_link(3, child).map_err(RuntimeError::Vm)?;
            Self::set_player_link(vm_object, player).map_err(RuntimeError::Vm)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn reclaim_runtime_subtree_parts<H: ProgramHost>(
        arena: &mut ObjectArena,
        handles: &mut HandleMap,
        machine: &mut Machine,
        pending_states: &mut BTreeMap<VmObjectHandle, u16>,
        pending_cleanup_actions: &mut Vec<RuntimeCleanupAction>,
        reclaim_event_faults: &mut Vec<RuntimeReclaimEventFault>,
        level: Option<LevelId>,
        level_state_context: Option<&RetailLevelStateContext>,
        saved_level_state: &mut Option<RetailLevelSnapshot>,
        transition_zone_context: ObjectZoneContext,
        host: &mut H,
        root: ArenaObjectHandle,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
    ) -> Result<(), RuntimeError<H::Error>> {
        Self::reclaim_runtime_object_parts(
            arena,
            handles,
            machine,
            pending_states,
            pending_cleanup_actions,
            reclaim_event_faults,
            level,
            level_state_context,
            saved_level_state,
            transition_zone_context,
            host,
            root,
            spawned_children,
        )?;

        // Bounds are keyed by compact VM handle. Native frees every voice and
        // object field before returning the slot to the free-list; clearing
        // this bounded snapshot prevents the replacement from inheriting the
        // prior identity's collision record.
        machine.clear_frame_bounds();
        Self::refresh_tree_links(arena, handles, machine)
    }

    #[allow(clippy::too_many_arguments)]
    fn reclaim_runtime_object_parts<H: ProgramHost>(
        arena: &mut ObjectArena,
        handles: &mut HandleMap,
        machine: &mut Machine,
        pending_states: &mut BTreeMap<VmObjectHandle, u16>,
        pending_cleanup_actions: &mut Vec<RuntimeCleanupAction>,
        reclaim_event_faults: &mut Vec<RuntimeReclaimEventFault>,
        level: Option<LevelId>,
        level_state_context: Option<&RetailLevelStateContext>,
        saved_level_state: &mut Option<RetailLevelSnapshot>,
        transition_zone_context: ObjectZoneContext,
        host: &mut H,
        arena_handle: ArenaObjectHandle,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
    ) -> Result<(), RuntimeError<H::Error>> {
        let Some(object) = handles.for_arena(arena_handle) else {
            // A TERM handler may synchronously trigger another reclaim. If it
            // already removed this exact generation, its lifecycle is done.
            return Ok(());
        };
        Self::validate_runtime_object(arena, handles, machine, object)?;

        // `GoolObjectKill(sig=1)` signals the current object before reading
        // its child pointer, then recurses head-to-tail. The event return is
        // ignored; retain only the ordered recipient identity for diagnostics.
        if Self::dispatch_event_parts(
            arena,
            handles,
            machine,
            pending_states,
            pending_cleanup_actions,
            reclaim_event_faults,
            level,
            level_state_context,
            saved_level_state,
            transition_zone_context,
            host,
            None,
            Some(object),
            TERMINATE_EVENT,
            None,
            spawned_children,
        )
        .is_err()
        {
            reclaim_event_faults.push(RuntimeReclaimEventFault { object });
        }

        let Some(spawned) = arena.get(arena_handle) else {
            return Ok(());
        };
        let mut child = spawned.first_child();
        while let Some(child_handle) = child {
            let sibling = arena
                .get(child_handle)
                .and_then(SpawnedObject::next_sibling);
            Self::reclaim_runtime_object_parts(
                arena,
                handles,
                machine,
                pending_states,
                pending_cleanup_actions,
                reclaim_event_faults,
                level,
                level_state_context,
                saved_level_state,
                transition_zone_context,
                host,
                child_handle,
                spawned_children,
            )?;
            child = sibling;
        }

        let Some(object) = handles.for_arena(arena_handle) else {
            return Ok(());
        };
        let spawn_id = Self::live_spawn_id(machine, object.vm)?;
        machine
            .remove_object_from_retail_pool_slot(object.vm, object.arena.slot())
            .map_err(RuntimeError::Vm)?;
        Self::clear_removed_retail_box_word_references(machine, handles, object.vm)
            .map_err(RuntimeError::Vm)?;
        pending_states.remove(&object.vm);
        let audio_freed = host.free_object_audio(object);
        handles.release(object);
        arena
            .despawn_leaf_with_spawn_id(arena_handle, spawn_id)
            .map_err(RuntimeError::Tree)?;
        let spawn_flags = arena
            .spawn_table()
            .flags(spawn_id)
            .ok_or(RuntimeError::Spawn(SpawnError::InvalidSpawnId(spawn_id)))?;
        machine
            .set_spawn_flags(spawn_id, spawn_flags)
            .map_err(RuntimeError::Vm)?;
        spawned_children.retain(|spawned| *spawned != object);
        if !audio_freed {
            pending_cleanup_actions.push(RuntimeCleanupAction::FreeObjectAudio(object));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_send_event_request_parts<H: ProgramHost>(
        arena: &mut ObjectArena,
        handles: &mut HandleMap,
        machine: &mut Machine,
        pending_states: &mut BTreeMap<VmObjectHandle, u16>,
        pending_cleanup_actions: &mut Vec<RuntimeCleanupAction>,
        reclaim_event_faults: &mut Vec<RuntimeReclaimEventFault>,
        level: Option<LevelId>,
        level_state_context: Option<&RetailLevelStateContext>,
        saved_level_state: &mut Option<RetailLevelSnapshot>,
        transition_zone_context: ObjectZoneContext,
        host: &mut H,
        current_object: Option<VmObjectHandle>,
        request: SendEventRequest,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
    ) -> Result<(), RuntimeError<H::Error>> {
        let Some(sender) = handles.for_vm(request.sender) else {
            // A servicing request whose sender was reclaimed is completed by
            // the VM's incarnation guard after this no-op host boundary.
            return Ok(());
        };
        if !handles.is_live_pair(sender)
            || arena.get(sender.arena).is_none()
            || machine.object(sender.vm).is_err()
        {
            return Ok(());
        }

        match request.target {
            SendEventTarget::Direct { recipient } => {
                let Some(recipient) = handles.for_vm(recipient) else {
                    return Ok(());
                };
                // Native GoolOpSendEvent ignores the per-recipient return
                // value, but its non-null argv pointer is retained at argc 0.
                let _ = Self::dispatch_event_parts_current(
                    arena,
                    handles,
                    machine,
                    pending_states,
                    pending_cleanup_actions,
                    reclaim_event_faults,
                    level,
                    level_state_context,
                    saved_level_state,
                    transition_zone_context,
                    host,
                    current_object,
                    Some(sender),
                    Some(recipient),
                    request.event,
                    Some(request.arguments()),
                    Some(request.argument_pool_slots()),
                    spawned_children,
                );
            }
            SendEventTarget::AllRoots { mode } => {
                let mut traversal = SendEventTraversal {
                    arena,
                    handles,
                    machine,
                    pending_states,
                    pending_cleanup_actions,
                    reclaim_event_faults,
                    level,
                    level_state_context,
                    saved_level_state,
                    transition_zone_context,
                    host,
                    spawned_children,
                    current_object,
                    sender,
                    event: request.event,
                    arguments: request.arguments(),
                    argument_pool_slots: request.argument_pool_slots(),
                    mode,
                    count: 0,
                };
                for index in 0..ROOT_HANDLE_COUNT {
                    let root = RootHandle::new(index as u8)
                        .expect("the fixed root count always fits a root handle");
                    traversal.traverse_root(root).map_err(RuntimeError::Vm)?;
                }
            }
            SendEventTarget::LinkedChildren { root, mode } => {
                let Some(root) = handles.for_vm(root) else {
                    return Ok(());
                };
                let mut traversal = SendEventTraversal {
                    arena,
                    handles,
                    machine,
                    pending_states,
                    pending_cleanup_actions,
                    reclaim_event_faults,
                    level,
                    level_state_context,
                    saved_level_state,
                    transition_zone_context,
                    host,
                    spawned_children,
                    current_object,
                    sender,
                    event: request.event,
                    arguments: request.arguments(),
                    argument_pool_slots: request.argument_pool_slots(),
                    mode,
                    count: 0,
                };
                traversal
                    .traverse_children(root.arena)
                    .map_err(RuntimeError::Vm)?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_host_request<H: ProgramHost>(
        arena: &mut ObjectArena,
        handles: &mut HandleMap,
        machine: &mut Machine,
        pending_states: &mut BTreeMap<VmObjectHandle, u16>,
        pending_cleanup_actions: &mut Vec<RuntimeCleanupAction>,
        reclaim_event_faults: &mut Vec<RuntimeReclaimEventFault>,
        level: Option<LevelId>,
        level_state_context: Option<&RetailLevelStateContext>,
        saved_level_state: &mut Option<RetailLevelSnapshot>,
        transition_zone_context: ObjectZoneContext,
        host: &mut H,
        current_object: Option<VmObjectHandle>,
        request: VmHostRequest,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
    ) -> Result<(), RuntimeError<H::Error>> {
        match request {
            VmHostRequest::SendEvent(request) => Self::apply_send_event_request_parts(
                arena,
                handles,
                machine,
                pending_states,
                pending_cleanup_actions,
                reclaim_event_faults,
                level,
                level_state_context,
                saved_level_state,
                transition_zone_context,
                host,
                current_object,
                request,
                spawned_children,
            ),
            VmHostRequest::Audio(request) => {
                let response = host
                    .handle_audio_request(request)
                    .map_err(RuntimeError::Program)?;
                machine
                    .complete_audio_host_request(response)
                    .map_err(RuntimeError::Vm)
            }
            VmHostRequest::Card(request) => {
                let current = machine.retail_card_save_data().map_err(RuntimeError::Vm)?;
                let response = host
                    .handle_card_request(request, current)
                    .map_err(RuntimeError::Program)?;
                machine
                    .complete_card_host_request(request, response.result)
                    .map_err(RuntimeError::Vm)?;
                if let Some(loaded) = response.loaded {
                    machine
                        .restore_retail_card_save_data(loaded)
                        .map_err(RuntimeError::Vm)?;
                    machine.record_completed_card_load(loaded);
                }
                machine
                    .publish_retail_card_state(response.published)
                    .map_err(RuntimeError::Vm)
            }
            VmHostRequest::Effect(effect) => Self::apply_host_effect(
                arena,
                handles,
                machine,
                pending_states,
                pending_cleanup_actions,
                reclaim_event_faults,
                level,
                level_state_context,
                saved_level_state,
                transition_zone_context,
                host,
                &effect,
                spawned_children,
            ),
        }
    }

    fn refresh_animation_local_bound_parts<H: ProgramHost>(
        arena: &ObjectArena,
        handles: &HandleMap,
        machine: &mut Machine,
        host: &mut H,
        vm: VmObjectHandle,
        refresh: AnimationLocalBoundRefresh,
    ) -> Result<(), RuntimeError<H::Error>> {
        let object = handles
            .for_vm(vm)
            .ok_or(RuntimeError::UnknownVmObject(vm))?;
        Self::validate_runtime_object(arena, handles, machine, object)?;
        let spawned = arena
            .get(object.arena)
            .ok_or(RuntimeError::UnknownArenaObject(object.arena))?;
        let status_b = machine
            .object(vm)
            .map_err(RuntimeError::Vm)?
            .register(process_register::STATUS_B)
            .map_err(RuntimeError::Vm)?;

        if refresh == AnimationLocalBoundRefresh::Conditional {
            if status_b & LOCAL_BOUND_REFRESH_STATUS_B == 0 {
                return Ok(());
            }
            let Some(main_arena) = arena.main_object() else {
                return Ok(());
            };
            let main = handles
                .for_arena(main_arena)
                .filter(|main| handles.is_live_pair(*main))
                .ok_or(RuntimeError::UnknownArenaObject(main_arena))?;
            let object_translation = machine
                .object(vm)
                .map_err(RuntimeError::Vm)?
                .retail_transform()
                .map_err(RuntimeError::Vm)?
                .translation;
            let main_translation = machine
                .object(main.vm)
                .map_err(RuntimeError::Vm)?
                .retail_transform()
                .map_err(RuntimeError::Vm)?
                .translation;
            if translation_outside_bound_range(
                object_translation,
                main_translation,
                LATE_BOUND_RANGE,
            ) {
                let vm_object = machine.object_mut(vm).map_err(RuntimeError::Vm)?;
                let status_a = vm_object
                    .register(process_register::STATUS_A)
                    .map_err(RuntimeError::Vm)?;
                vm_object
                    .set_register(
                        process_register::STATUS_A,
                        status_a | LOCAL_BOUND_INVALID_STATUS_A,
                    )
                    .map_err(RuntimeError::Vm)?;
                if status_b & FORCE_LOCAL_BOUND_REFRESH_STATUS_B == 0 {
                    return Ok(());
                }
            }
        }

        let (animation, frame_index, transform) = {
            let Some(animation) = machine.animation_source(vm).map_err(RuntimeError::Vm)? else {
                return Ok(());
            };
            let vm_object = machine.object(vm).map_err(RuntimeError::Vm)?;
            (
                animation,
                vm_object.animation_frame() >> 8,
                vm_object.retail_transform().map_err(RuntimeError::Vm)?,
            )
        };
        let source = match animation_vertex_reference(&animation) {
            Some(reference) => {
                let Some(source) = host
                    .animation_bound_source(AnimationBoundBinding {
                        object,
                        zone: spawned.zone(),
                        executable: spawned.origin().executable(),
                        reference,
                        frame_index,
                    })
                    .map_err(RuntimeError::Program)?
                else {
                    return Ok(());
                };
                source
            }
            None => AnimationBoundSource::NonVertex,
        };
        let scale = Vec3 {
            x: transform.scale[0],
            y: transform.scale[1],
            z: transform.scale[2],
        };
        let local_bound = calculate_local_bound(source, scale, object.arena.is_dedicated_main());
        machine
            .object_mut(vm)
            .map_err(RuntimeError::Vm)?
            .set_retail_local_bound(local_bound);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_host_effect<H: ProgramHost>(
        arena: &mut ObjectArena,
        handles: &mut HandleMap,
        machine: &mut Machine,
        pending_states: &mut BTreeMap<VmObjectHandle, u16>,
        pending_cleanup_actions: &mut Vec<RuntimeCleanupAction>,
        reclaim_event_faults: &mut Vec<RuntimeReclaimEventFault>,
        level: Option<LevelId>,
        level_state_context: Option<&RetailLevelStateContext>,
        saved_level_state: &mut Option<RetailLevelSnapshot>,
        transition_zone_context: ObjectZoneContext,
        host: &mut H,
        effect: &VmEffect,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
    ) -> Result<(), RuntimeError<H::Error>> {
        if let VmEffect::Paging {
            object,
            operation,
            physical,
            reference,
            eid,
            page,
            was_resolved,
        } = effect
        {
            let runtime_object = handles
                .for_vm(*object)
                .ok_or(RuntimeError::UnknownVmObject(*object))?;
            Self::validate_runtime_object(arena, handles, machine, runtime_object)?;
            let request = PagingHostRequest {
                object: runtime_object.vm(),
                operation: *operation,
                physical: *physical,
                reference: *reference,
                eid: *eid,
                page: *page,
                was_resolved: *was_resolved,
            };
            let response = host
                .handle_paging_request(request)
                .map_err(RuntimeError::Program)?;
            return machine
                .complete_paging_host_request(request, response)
                .map_err(RuntimeError::Vm);
        }

        if let VmEffect::AnimationFrameChanged {
            object,
            local_bound_refresh,
            ..
        } = effect
        {
            return Self::refresh_animation_local_bound_parts(
                arena,
                handles,
                machine,
                host,
                *object,
                *local_bound_refresh,
            );
        }

        if let VmEffect::ResetLevelGlobals { object } = effect {
            let object = handles
                .for_vm(*object)
                .ok_or(RuntimeError::UnknownVmObject(*object))?;
            Self::validate_runtime_object(arena, handles, machine, object)?;
            machine
                .reset_retail_level_globals()
                .map_err(RuntimeError::Vm)?;
            return Ok(());
        }

        if let VmEffect::SaveState(vm) = effect {
            let caller = handles
                .for_vm(*vm)
                .ok_or(RuntimeError::UnknownVmObject(*vm))?;
            Self::validate_runtime_object(arena, handles, machine, caller)?;
            let outcome = Self::capture_level_state(
                arena,
                handles,
                machine,
                level,
                level_state_context,
                caller,
                false,
            )
            .map_err(RuntimeError::LevelState)?;
            if let RetailSaveStateOutcome::Saved(snapshot) = outcome {
                *saved_level_state = Some(*snapshot);
            }
            return Ok(());
        }

        if let VmEffect::LoadState { object: vm, .. } = effect {
            let caller = handles
                .for_vm(*vm)
                .ok_or(RuntimeError::UnknownVmObject(*vm))?;
            Self::validate_runtime_object(arena, handles, machine, caller)?;
            let saved_level = saved_level_state
                .as_ref()
                .ok_or(RuntimeError::MissingSavedLevelState)?
                .level;
            machine
                .resolve_load_state_effect(*vm, saved_level)
                .map_err(RuntimeError::Vm)?;
            // Native LevelRestart clears bonus mode before comparing saved
            // and current levels. Different-level GOOL continues after this
            // host boundary, so the write must be visible immediately rather
            // than when the browser later consumes the remount effect.
            if BONUS_ROUND_GLOBAL < machine.global_words().len() {
                machine
                    .set_global_word(BONUS_ROUND_GLOBAL, 0)
                    .map_err(RuntimeError::Vm)?;
            }
            // A same-level LevelRestart synchronously replaces the active
            // object forest, so the pointer-free host must stop this walk and
            // perform that structural transaction at its checked boundary.
            // A different-level restart only writes next_lid=-2/first_spawn
            // in retail. Preserve the effect for the browser remount, but let
            // the current interpreter, later objects, and display latch run.
            if level.is_none_or(|current_level| current_level == saved_level) {
                machine.request_level_restart();
            }
            return Ok(());
        }

        if let VmEffect::ReparentToRoot { object, root } = effect {
            let object = handles
                .for_vm(*object)
                .ok_or(RuntimeError::UnknownVmObject(*object))?;
            Self::validate_runtime_object(arena, handles, machine, object)?;
            let root =
                RootHandle::new(*root).ok_or(RuntimeError::InvalidRootIndex(usize::from(*root)))?;
            arena
                .reparent_to_root(object.arena, root)
                .map_err(RuntimeError::Tree)?;
            return Self::refresh_tree_links(arena, handles, machine);
        }

        if let VmEffect::SetObjectZoneToTransitionTarget { object } = effect {
            let object = handles
                .for_vm(*object)
                .ok_or(RuntimeError::UnknownVmObject(*object))?;
            Self::validate_runtime_object(arena, handles, machine, object)?;
            return match transition_zone_context {
                ObjectZoneContext::Target(target) => arena
                    .set_zone(object.arena, target)
                    .map_err(RuntimeError::Tree),
                // Native writes the `(entry *)-1` sentinel to the object. The
                // arena admits only validated EIDs, and hard restart kills the
                // object immediately regardless, so no persistent zone value
                // is needed here.
                ObjectZoneContext::HardRestartSentinel => Ok(()),
                ObjectZoneContext::Null => arena
                    .set_zone(object.arena, Eid::NONE)
                    .map_err(RuntimeError::Tree),
            };
        }

        if let VmEffect::TerminateCurrentZoneNeighbors { requester } = effect {
            let requester = handles
                .for_vm(*requester)
                .ok_or(RuntimeError::UnknownVmObject(*requester))?;
            Self::validate_runtime_object(arena, handles, machine, requester)?;
            let Some(current_zone) = level_state_context.map(|context| context.location.path.zone)
            else {
                // Native case 12/7 is a no-op while `cur_zone` is null.
                return Ok(());
            };
            let neighbors = host
                .current_zone_neighbors(current_zone)
                .map_err(RuntimeError::Program)?;
            for zone in neighbors {
                let report = Self::terminate_zone_roots_live_parts(
                    arena,
                    handles,
                    machine,
                    pending_states,
                    pending_cleanup_actions,
                    reclaim_event_faults,
                    level,
                    level_state_context,
                    saved_level_state,
                    transition_zone_context,
                    host,
                    zone,
                    spawned_children,
                    true,
                )?;
                reclaim_event_faults.extend(report.event_failures.iter().map(|failure| {
                    RuntimeReclaimEventFault {
                        object: failure.object,
                    }
                }));
            }
            return Ok(());
        }

        if let VmEffect::SetLinkZoneFromPoint {
            requester,
            target,
            point,
        } = effect
        {
            let requester = handles
                .for_vm(*requester)
                .ok_or(RuntimeError::UnknownVmObject(*requester))?;
            Self::validate_runtime_object(arena, handles, machine, requester)?;
            let target = handles
                .for_vm(*target)
                .ok_or(RuntimeError::UnknownVmObject(*target))?;
            Self::validate_runtime_object(arena, handles, machine, target)?;
            let current_zone = level_state_context
                .ok_or(RuntimeError::MissingLevelStateContext)?
                .location
                .path
                .zone;
            let selected = match point {
                Some(point) => host
                    .find_neighbor_zone(current_zone, *point)
                    .map_err(RuntimeError::Program)?,
                None => Some(current_zone),
            };
            if let Some(zone) = selected {
                arena
                    .set_zone(target.arena, zone)
                    .map_err(RuntimeError::Tree)?;
            }
            return Ok(());
        }

        if let VmEffect::SpawnFlagsChanged { object, id, flags } = effect {
            let object = handles
                .for_vm(*object)
                .ok_or(RuntimeError::UnknownVmObject(*object))?;
            Self::validate_runtime_object(arena, handles, machine, object)?;
            arena
                .spawn_table_mut()
                .set_flags(*id, *flags)
                .map_err(RuntimeError::Spawn)?;
            return Ok(());
        }

        if let VmEffect::FindSpawnedObject {
            requester,
            pid_flags,
        } = effect
        {
            let requester = handles
                .for_vm(*requester)
                .ok_or(RuntimeError::UnknownVmObject(*requester))?;
            Self::validate_runtime_object(arena, handles, machine, requester)?;
            let mut found = None;
            for root in [ZONE_OBJECT_ROOT, ENEMY_OBJECT_ROOT] {
                for arena_handle in arena
                    .preorder(TreeParent::Root(root))
                    .map_err(RuntimeError::Tree)?
                {
                    let candidate = handles
                        .for_arena(arena_handle)
                        .ok_or(RuntimeError::UnknownArenaObject(arena_handle))?;
                    if machine
                        .object(candidate.vm)
                        .map_err(RuntimeError::Vm)?
                        .register(process_register::PID_FLAGS)
                        .map_err(RuntimeError::Vm)?
                        == *pid_flags
                    {
                        found = Some(candidate.vm);
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
            }
            return machine
                .complete_find_spawned_object(requester.vm, found)
                .map_err(RuntimeError::Vm);
        }

        if let VmEffect::FindNearestObject {
            requester,
            origin,
            categories,
            event,
        } = effect
        {
            let requester = handles
                .for_vm(*requester)
                .ok_or(RuntimeError::UnknownVmObject(*requester))?;
            let origin = handles
                .for_vm(*origin)
                .ok_or(RuntimeError::UnknownVmObject(*origin))?;
            Self::validate_runtime_object(arena, handles, machine, requester)?;
            Self::validate_runtime_object(arena, handles, machine, origin)?;

            // Status-query interrupts are allowed to run arbitrary GOOL and
            // mutate the forest, so retain the source preorder as checked
            // generational handles before entering any candidate code.
            let candidates = arena
                .preorder(TreeParent::Root(ENEMY_OBJECT_ROOT))
                .map_err(RuntimeError::Tree)?
                .collect::<Vec<_>>();
            let mut nearest = None;
            let mut nearest_distance = i32::MAX;
            for arena_handle in candidates {
                let candidate = handles
                    .for_arena(arena_handle)
                    .ok_or(RuntimeError::UnknownArenaObject(arena_handle))?;
                let classification = machine
                    .classify_nearest_object_candidate(origin.vm, candidate.vm, *categories, *event)
                    .map_err(RuntimeError::Vm)?;
                let (distance, interrupt_offset) = match classification {
                    NearestObjectCandidate::Ineligible => continue,
                    NearestObjectCandidate::Eligible { distance } => (distance, None),
                    NearestObjectCandidate::StatusInterrupt { distance, offset } => {
                        (distance, Some(offset))
                    }
                };
                // Native checks distance before consulting the event map. A
                // tie therefore retains the first preorder object and never
                // invokes the later candidate's STATUS interrupt.
                if distance >= nearest_distance {
                    continue;
                }

                let eligible = if let Some(offset) = interrupt_offset {
                    let mut callback_error = None;
                    let state_change = machine.run_nearest_status_interrupt_with_host_requests(
                        origin.vm,
                        candidate.vm,
                        offset,
                        |machine, request| {
                            let result = Self::apply_host_request(
                                arena,
                                handles,
                                machine,
                                pending_states,
                                pending_cleanup_actions,
                                reclaim_event_faults,
                                level,
                                level_state_context,
                                saved_level_state,
                                transition_zone_context,
                                host,
                                Some(origin.vm),
                                request,
                                spawned_children,
                            );
                            if let Err(error) = result {
                                callback_error = Some(error);
                                return Err(VmError::MissingHostEffect);
                            }
                            Ok(())
                        },
                    );
                    if let Some(error) = callback_error {
                        return Err(error);
                    }
                    let state_change = state_change.map_err(RuntimeError::Vm)?;
                    if machine.level_restart_requested() {
                        return Ok(());
                    }
                    if let Some(change) = state_change {
                        Self::rebind_event_state_change_parts(
                            arena,
                            handles,
                            machine,
                            pending_states,
                            pending_cleanup_actions,
                            reclaim_event_faults,
                            level,
                            level_state_context,
                            saved_level_state,
                            transition_zone_context,
                            host,
                            &change,
                            spawned_children,
                            Some(origin.vm),
                        )?;
                    }
                    if machine.level_restart_requested() {
                        return Ok(());
                    }
                    machine
                        .object(candidate.vm)
                        .map_err(RuntimeError::Vm)?
                        .register(process_register::ACK)
                        .map_err(RuntimeError::Vm)?
                        != 0
                } else {
                    true
                };
                if eligible {
                    nearest = Some(candidate.vm);
                    nearest_distance = distance;
                }
            }
            return machine
                .complete_find_nearest_object(requester.vm, nearest)
                .map_err(RuntimeError::Vm);
        }

        if let VmEffect::TransformModelVertex {
            requester,
            link,
            output_vector,
            model_eid,
            frame_index,
            vertex_index,
        } = effect
        {
            let requester = handles
                .for_vm(*requester)
                .ok_or(RuntimeError::UnknownVmObject(*requester))?;
            let link = handles
                .for_vm(*link)
                .ok_or(RuntimeError::UnknownVmObject(*link))?;
            Self::validate_runtime_object(arena, handles, machine, requester)?;
            Self::validate_runtime_object(arena, handles, machine, link)?;
            let source = host
                .model_vertex_source(ModelVertexBinding {
                    requester,
                    link,
                    model_eid: *model_eid,
                    frame_index: *frame_index,
                    vertex_index: *vertex_index,
                })
                .map_err(RuntimeError::Program)?;
            return machine
                .complete_model_vertex_transform(requester.vm, link.vm, *output_vector, source)
                .map_err(RuntimeError::Vm);
        }

        if let VmEffect::Event {
            sender,
            recipient,
            event,
        } = effect
        {
            let sender = handles
                .for_vm(*sender)
                .ok_or(RuntimeError::UnknownVmObject(*sender))?;
            if let Some(recipient) = recipient {
                let recipient = handles
                    .for_vm(*recipient)
                    .ok_or(RuntimeError::UnknownVmObject(*recipient))?;
                Self::dispatch_event_parts(
                    arena,
                    handles,
                    machine,
                    pending_states,
                    pending_cleanup_actions,
                    reclaim_event_faults,
                    level,
                    level_state_context,
                    saved_level_state,
                    transition_zone_context,
                    host,
                    Some(sender),
                    Some(recipient),
                    *event,
                    None,
                    spawned_children,
                )?;
            } else {
                // Opcode 0x8f uses a null effect recipient as its checked
                // all-root postorder broadcast token. The VM effect does not
                // yet retain nonzero collision modes or argv, so this path is
                // exact for the common mode-zero/no-argument form.
                let recipients = arena
                    .postorder_snapshot()
                    .map_err(RuntimeError::Tree)?
                    .into_iter()
                    .map(|arena| {
                        handles
                            .for_arena(arena)
                            .ok_or(RuntimeError::UnknownArenaObject(arena))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                for recipient in recipients {
                    Self::dispatch_event_parts(
                        arena,
                        handles,
                        machine,
                        pending_states,
                        pending_cleanup_actions,
                        reclaim_event_faults,
                        level,
                        level_state_context,
                        saved_level_state,
                        transition_zone_context,
                        host,
                        Some(sender),
                        Some(recipient),
                        *event,
                        None,
                        spawned_children,
                    )?;
                }
            }
            return Ok(());
        }

        let VmEffect::SpawnChildren {
            parent,
            executable,
            subtype,
            count,
            allow_reclaim,
            arguments,
            argument_pool_slots,
        } = effect
        else {
            return Err(RuntimeError::Vm(VmError::MissingHostEffect));
        };
        let parent = handles
            .for_vm(*parent)
            .ok_or(RuntimeError::UnknownVmObject(*parent))?;
        let zone = arena
            .get(parent.arena)
            .ok_or(RuntimeError::UnknownArenaObject(parent.arena))?
            .zone();
        let binding_zone = if zone == Eid::NONE {
            // Executables 4/5/29 deliberately clear native `obj->zone`.
            // Their runtime children inherit that null pointer, while
            // `GoolObjectCreate` still reads colors through global `cur_zone`.
            // Keep the null lifecycle identity and resolve only the host
            // environment through the checked current-camera context.
            level_state_context
                .ok_or(RuntimeError::MissingLevelStateContext)?
                .location
                .path
                .zone
        } else {
            zone
        };

        for _ in 0..*count {
            let arena_handle = loop {
                let allocation = match arena.create_child(
                    parent.arena,
                    zone,
                    *executable,
                    *subtype,
                    *allow_reclaim,
                ) {
                    Ok(arena_handle) => Some(arena_handle),
                    Err(RuntimeCreateError::ReclaimRequired(candidate)) => {
                        Self::reclaim_runtime_subtree_parts(
                            arena,
                            handles,
                            machine,
                            pending_states,
                            pending_cleanup_actions,
                            reclaim_event_faults,
                            level,
                            level_state_context,
                            saved_level_state,
                            transition_zone_context,
                            host,
                            candidate,
                            spawned_children,
                        )?;
                        continue;
                    }
                    Err(RuntimeCreateError::ObjectPoolFull) => {
                        // Native `GoolOpSpawnChildren` treats allocation failure
                        // as an ordinary null `misc_child`, not an interpreter
                        // error. The caller keeps executing after either 0x8a or
                        // an exhausted 0x91 reclaim search.
                        machine
                            .object_mut(parent.vm)
                            .map_err(RuntimeError::Vm)?
                            .set_register(process_register::MISC_VALUE, 0)
                            .map_err(RuntimeError::Vm)?;
                        None
                    }
                    Err(error) => return Err(RuntimeError::Create(error)),
                };
                break allocation;
            };
            let Some(arena_handle) = arena_handle else {
                continue;
            };
            handles.prune_stale(arena);
            let existing = handles.for_arena(arena_handle);
            let is_new_binding = existing.is_none();
            let object = if let Some(existing) = existing {
                existing
            } else {
                match handles.reserve(arena_handle) {
                    Ok(object) => object,
                    Err(error) => {
                        arena
                            .despawn_subtree(arena_handle)
                            .map_err(RuntimeError::Tree)?;
                        return Err(error);
                    }
                }
            };
            let binding = ProgramBinding {
                object,
                zone: binding_zone,
                executable: *executable,
                subtype: *subtype,
                origin: ProgramOrigin::RuntimeChild { arguments },
            };
            let mut vm_object = match host.bind_program(binding) {
                Ok(vm_object) => vm_object,
                Err(error) => {
                    if is_new_binding {
                        handles.release(object);
                        arena
                            .despawn_subtree(arena_handle)
                            .map_err(RuntimeError::Tree)?;
                    }
                    return Err(RuntimeError::Program(error));
                }
            };
            if vm_object.handle() != object.vm {
                if is_new_binding {
                    handles.release(object);
                    arena
                        .despawn_subtree(arena_handle)
                        .map_err(RuntimeError::Tree)?;
                }
                return Err(RuntimeError::HostObjectHandleMismatch {
                    expected: object.vm,
                    actual: vm_object.handle(),
                });
            }
            if let Err(error) = Self::apply_program_page_materialization(machine, host, binding) {
                if is_new_binding {
                    handles.release(object);
                    arena
                        .despawn_subtree(arena_handle)
                        .map_err(RuntimeError::Tree)?;
                }
                return Err(error);
            }
            let install_result = (|| {
                machine
                    .seed_retail_pool_slot_storage(object.arena.slot(), &mut vm_object)
                    .map_err(RuntimeError::Vm)?;
                vm_object
                    .initialize_arguments_with_pool_slots(arguments, argument_pool_slots)
                    .map_err(RuntimeError::Vm)?;
                let environment = host
                    .zone_environment(binding_zone)
                    .map_err(RuntimeError::Program)?;
                let solid_environment = host
                    .solid_environment(binding_zone)
                    .map_err(RuntimeError::Program)?;
                Self::initialize_vm_process(
                    arena,
                    handles,
                    machine,
                    binding,
                    environment,
                    &mut vm_object,
                )?;
                if let Some(environment) = solid_environment {
                    vm_object.bind_retail_solid_environment(environment);
                }
                if runtime_program_clears_object_zone(binding) {
                    arena
                        .set_zone(object.arena, Eid::NONE)
                        .map_err(RuntimeError::Tree)?;
                    vm_object.set_retail_solid_zone_eid(None);
                }
                Self::initialize_vm_links(arena, handles, machine, object, &mut vm_object)?;
                Self::install_vm_object(machine, vm_object, object.arena.slot())?;
                arena
                    .set_state_flags(
                        arena_handle,
                        machine
                            .object(object.vm)
                            .map_err(RuntimeError::Vm)?
                            .state_flags(),
                    )
                    .map_err(RuntimeError::Tree)?;
                let parent_vm = machine.object_mut(parent.vm).map_err(RuntimeError::Vm)?;
                parent_vm
                    .set_link(3, Some(object.vm))
                    .map_err(RuntimeError::Vm)?;
                parent_vm
                    .set_register(
                        process_register::MISC_VALUE,
                        CollisionObjectReference::new(object.vm).to_word(),
                    )
                    .map_err(RuntimeError::Vm)?;
                Self::refresh_player_links(arena, handles, machine)
            })();
            if let Err(error) = install_result {
                if is_new_binding {
                    handles.release(object);
                    arena
                        .despawn_subtree(arena_handle)
                        .map_err(RuntimeError::Tree)?;
                }
                return Err(error);
            }
            spawned_children.push(object);
        }
        Ok(())
    }

    fn materialize<H: ProgramHost>(
        &mut self,
        binding: ProgramBinding<'_>,
        host: &mut H,
    ) -> Result<MaterializedObject, RuntimeError<H::Error>> {
        let mut vm_object = host.bind_program(binding).map_err(RuntimeError::Program)?;
        if vm_object.handle() != binding.object.vm {
            return Err(RuntimeError::HostObjectHandleMismatch {
                expected: binding.object.vm,
                actual: vm_object.handle(),
            });
        }
        Self::apply_program_page_materialization(&mut self.machine, host, binding)?;
        self.machine
            .seed_retail_pool_slot_storage(binding.object.arena.slot(), &mut vm_object)
            .map_err(RuntimeError::Vm)?;
        let environment = host
            .zone_environment(binding.zone)
            .map_err(RuntimeError::Program)?;
        let solid_environment = host
            .solid_environment(binding.zone)
            .map_err(RuntimeError::Program)?;
        Self::initialize_vm_process(
            &self.arena,
            &self.handles,
            &self.machine,
            binding,
            environment,
            &mut vm_object,
        )?;
        if let Some(environment) = solid_environment {
            vm_object.bind_retail_solid_environment(environment);
        }
        if runtime_program_clears_object_zone(binding) {
            self.arena
                .set_zone(binding.object.arena, Eid::NONE)
                .map_err(RuntimeError::Tree)?;
            vm_object.set_retail_solid_zone_eid(None);
        }
        Self::initialize_vm_links(
            &self.arena,
            &self.handles,
            &self.machine,
            binding.object,
            &mut vm_object,
        )?;
        Self::install_vm_object(&mut self.machine, vm_object, binding.object.arena.slot())?;
        let is_entity_enemy = matches!(binding.origin, ProgramOrigin::Entity(_))
            && self
                .machine
                .object(binding.object.vm)
                .map_err(RuntimeError::Vm)?
                .program_identity()
                .is_some_and(|identity| identity.category() == 0x300);
        if is_entity_enemy {
            self.arena
                .reparent_to_root(binding.object.arena, ENEMY_OBJECT_ROOT)
                .map_err(RuntimeError::Tree)?;
        }
        self.arena
            .set_state_flags(
                binding.object.arena,
                self.machine
                    .object(binding.object.vm)
                    .map_err(RuntimeError::Vm)?
                    .state_flags(),
            )
            .map_err(RuntimeError::Tree)?;
        Self::refresh_player_links(&self.arena, &self.handles, &mut self.machine)?;
        Ok(MaterializedObject {
            object: binding.object,
            environment,
        })
    }

    fn apply_program_page_materialization<H: ProgramHost>(
        machine: &mut Machine,
        host: &mut H,
        binding: ProgramBinding<'_>,
    ) -> Result<(), RuntimeError<H::Error>> {
        if let Some(outcome) = host
            .materialize_program_page(binding)
            .map_err(RuntimeError::Program)?
        {
            machine
                .apply_platform_program_materialization(outcome.page, outcome.invalidated)
                .map_err(RuntimeError::Vm)?;
        }
        Ok(())
    }

    fn initialize_vm_process<E>(
        arena: &ObjectArena,
        handles: &HandleMap,
        machine: &Machine,
        binding: ProgramBinding<'_>,
        environment: Option<RetailZoneEnvironment>,
        vm_object: &mut VmObject,
    ) -> Result<(), RuntimeError<E>> {
        vm_object
            .initialize_retail_process(binding.subtype, machine.frames_elapsed())
            .map_err(RuntimeError::Vm)?;
        if binding.executable == 0 {
            vm_object
                .set_register(process_register::CAMERA_ZOOM, 0)
                .map_err(RuntimeError::Vm)?;
        }
        vm_object.set_main_player_identity(binding.object.arena.is_dedicated_main());

        let spawned = arena
            .get(binding.object.arena)
            .ok_or(RuntimeError::UnknownArenaObject(binding.object.arena))?;
        match spawned.parent() {
            TreeParent::Object(parent_arena) => {
                let parent = handles
                    .for_arena(parent_arena)
                    .ok_or(RuntimeError::UnknownArenaObject(parent_arena))?;
                let transform = machine
                    .object(parent.vm)
                    .map_err(RuntimeError::Vm)?
                    .retail_transform()
                    .map_err(RuntimeError::Vm)?;
                vm_object
                    .set_retail_transform(transform)
                    .map_err(RuntimeError::Vm)?;
            }
            TreeParent::Root(_) => {
                let mut transform = vm_object.retail_transform().map_err(RuntimeError::Vm)?;
                transform.rotation_yxz = [0; 3];
                transform.scale = [0x1000; 3];
                vm_object
                    .set_retail_transform(transform)
                    .map_err(RuntimeError::Vm)?;
            }
        }

        if let ProgramOrigin::Entity(entity) = binding.origin {
            vm_object
                .initialize_retail_entity(
                    entity,
                    environment.map_or([0; 3], |environment| environment.origin),
                )
                .map_err(RuntimeError::Vm)?;
        }

        if let Some(environment) = environment {
            let colors = if binding.object.arena.is_dedicated_main() {
                environment.player_colors
            } else {
                environment.object_colors
            };
            vm_object.set_retail_colors(colors);
        }
        Ok(())
    }

    fn initialize_vm_links<E>(
        arena: &ObjectArena,
        handles: &HandleMap,
        machine: &Machine,
        object: RuntimeObjectHandle,
        vm_object: &mut VmObject,
    ) -> Result<(), RuntimeError<E>> {
        for (link, target) in [
            (0, Some(object.vm)),
            (1, None),
            (2, None),
            (3, None),
            (4, None),
            (5, None),
            (6, None),
            (7, None),
        ] {
            vm_object.set_link(link, target).map_err(RuntimeError::Vm)?;
        }
        let spawned = arena
            .get(object.arena)
            .ok_or(RuntimeError::UnknownArenaObject(object.arena))?;
        if let TreeParent::Object(parent_arena) = spawned.parent() {
            let parent = handles
                .for_arena(parent_arena)
                .ok_or(RuntimeError::UnknownArenaObject(parent_arena))?;
            machine.object(parent.vm).map_err(RuntimeError::Vm)?;
            vm_object
                .set_link(1, Some(parent.vm))
                .map_err(RuntimeError::Vm)?;
            vm_object
                .set_link(4, Some(parent.vm))
                .map_err(RuntimeError::Vm)?;
        }
        if let Some(sibling_arena) = spawned.next_sibling()
            && let Some(sibling) = handles.for_arena(sibling_arena)
        {
            vm_object
                .set_link(2, Some(sibling.vm))
                .map_err(RuntimeError::Vm)?;
        }
        let player = arena
            .main_object()
            .and_then(|main_arena| handles.for_arena(main_arena))
            .map(|main| main.vm);
        Self::set_player_link(vm_object, player).map_err(RuntimeError::Vm)?;
        Ok(())
    }

    fn set_player_link(
        vm_object: &mut VmObject,
        live_player: Option<VmObjectHandle>,
    ) -> Result<(), VmError> {
        let target_token = live_player.unwrap_or_else(|| {
            VmObjectHandle::new(OBJECT_POOL_CAPACITY as u16)
                .expect("the dedicated retail player slot fits the VM handle range")
        });
        vm_object.set_retail_pool_link(5, target_token, DEDICATED_PLAYER_POOL_SLOT)
    }

    fn install_vm_object<E>(
        machine: &mut Machine,
        vm_object: VmObject,
        pool_slot: u8,
    ) -> Result<(), RuntimeError<E>> {
        let handle = vm_object.handle();
        machine
            .preflight_retail_pool_slot_binding(handle, pool_slot)
            .map_err(RuntimeError::Vm)?;
        machine.upsert_object(vm_object).map_err(RuntimeError::Vm)?;
        machine
            .bind_retail_pool_slot(handle, pool_slot)
            .map_err(RuntimeError::Vm)
    }

    fn refresh_player_links<E>(
        arena: &ObjectArena,
        handles: &HandleMap,
        machine: &mut Machine,
    ) -> Result<(), RuntimeError<E>> {
        let player = arena
            .main_object()
            .and_then(|arena| handles.for_arena(arena))
            .map(|object| object.vm);
        for vm_index in 0..MAX_OBJECTS {
            let vm = VmObjectHandle::new(
                u16::try_from(vm_index).expect("VM handle capacity fits in u16"),
            )
            .expect("index came from the VM handle capacity");
            if handles.for_vm(vm).is_some() {
                let vm_object = machine.object_mut(vm).map_err(RuntimeError::Vm)?;
                Self::set_player_link(vm_object, player).map_err(RuntimeError::Vm)?;
            }
        }
        Ok(())
    }
}

fn wrapping_frame_stamp(frame_index: u64) -> u32 {
    let bytes = frame_index.to_le_bytes();
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn translation_outside_bound_range(first: [i32; 3], second: [i32; 3], range: Vec3) -> bool {
    [range.x, range.y, range.z]
        .into_iter()
        .enumerate()
        .any(|(axis, limit)| {
            let delta = i64::from(first[axis]) - i64::from(second[axis]);
            delta > i64::from(limit) || delta < -i64::from(limit)
        })
}

#[cfg(test)]
mod tests {
    use crust_formats::{
        binary::EntryRef,
        stream::{
            ENTRY_MAGIC, GOOL_PC_NONE, LevelId, NSF_PAGE_SIZE, ZoneEntityPathPoint, parse_nsd,
            parse_nsf, structs::GoolState,
        },
    };

    use super::*;
    use crate::gool::Instruction;
    use crate::object_arena::OBJECT_POOL_CAPACITY;
    use crate::object_bounds::MAX_FRAME_BOUNDS;

    const ZONE: Eid = Eid::from_raw(0x1234_5679);
    const ZONE_B: Eid = Eid::from_raw(0x2234_5679);
    const ZONE_C: Eid = Eid::from_raw(0x3234_5679);
    const CURRENT_ZONE: Eid = Eid::from_raw(0x4234_5679);
    const RETURN: u32 = 0x8289_4000;
    const TEST_CONDITION_REGISTER: usize = 63;
    const TEST_SCALAR_REGISTER_A: usize = 70;
    const TEST_SCALAR_REGISTER_B: usize = 71;
    const TEST_SCALAR_REGISTER_C: usize = 72;
    const TEST_SCALAR_OPERAND_A: u16 = 0x0e46;
    const TEST_SCALAR_OPERAND_B: u16 = 0x0e47;
    const TEST_SCALAR_OPERAND_C: u16 = 0x0e48;
    const MODERN_NSD_HEADER_SIZE: usize = 0x520;

    const fn misc(primary: u32, secondary: i32, operand: u16) -> u32 {
        (0x1c_u32 << 24)
            | ((primary & 0x0f) << 20)
            | (((secondary as u32) & 0x1f) << 15)
            | (operand as u32 & 0x0fff)
    }

    const fn event_service_return() -> u32 {
        // Opcode 0x88, guarded-null return used by the synthetic ESR tests.
        0x8880_0000
    }

    fn zone_rect(origin: [i32; 3], dimensions: [u32; 3]) -> ZoneRect {
        ZoneRect {
            origin,
            dimensions,
            unknown: 0,
            octree_root: 0,
            octree_max_depth: [0; 3],
        }
    }

    #[test]
    fn szon_neighbor_selection_is_reverse_ordered_and_inclusive() {
        let first = zone_rect([1, 2, 3], [4, 5, 6]);
        let last = zone_rect([1, 2, 3], [4, 5, 6]);
        let neighbors = [ZONE, ZONE_B];
        let mut resolved = Vec::new();

        assert_eq!(
            find_retail_neighbor_zone(&neighbors, [1 << 8, 2 << 8, 3 << 8], |zone| {
                resolved.push(zone);
                Ok::<_, ()>(if zone == ZONE { first } else { last })
            },),
            Ok(Some(ZONE_B)),
            "the last serialized matching neighbor wins"
        );
        assert_eq!(
            resolved,
            [ZONE_B],
            "an earlier serialized neighbor is never resolved after a match"
        );
        assert_eq!(
            find_retail_neighbor_zone(&neighbors, [5 << 8, 7 << 8, 9 << 8], |zone| {
                Ok::<_, ()>(if zone == ZONE { first } else { last })
            }),
            Ok(Some(ZONE_B)),
            "all upper faces are inclusive"
        );
        assert_eq!(
            find_retail_neighbor_zone(
                &neighbors,
                [(5 << 8) + 1, 7 << 8, 9 << 8],
                |zone| Ok::<_, ()>(if zone == ZONE { first } else { last }),
            ),
            Ok(None)
        );
    }

    #[test]
    fn szon_bounds_use_wrapping_q24_8_arithmetic() {
        let rect = zone_rect([i32::MAX, 0, 0], [1, 0, 0]);

        assert!(retail_zone_rect_contains(rect, [-256, 0, 0]));
        assert!(retail_zone_rect_contains(rect, [0, 0, 0]));
        assert!(!retail_zone_rect_contains(rect, [1, 0, 0]));
    }

    #[test]
    fn retail_runtime_initializes_both_display_globals_when_present() {
        let runtime = RetailRuntime::new(CURRENT_DISPLAY_GLOBAL + 1);
        assert_eq!(
            runtime.machine().global_word(NEXT_DISPLAY_GLOBAL),
            Ok(INITIAL_DISPLAY_MASK)
        );
        assert_eq!(
            runtime.machine().global_word(CURRENT_DISPLAY_GLOBAL),
            Ok(INITIAL_DISPLAY_MASK)
        );

        let authored = RetailRuntime::new(0);
        assert!(
            authored
                .machine()
                .global_word(CURRENT_DISPLAY_GLOBAL)
                .is_err()
        );
    }

    #[test]
    fn invalid_initial_return_skips_colors_and_physics_before_no_term_reclaim() {
        let mut runtime = RetailRuntime::new_for_level(0, LevelId::N_SANITY_BEACH);
        let returned = spawn_test_object(&mut runtime, ZONE, 9, 2, 0);
        let missing_collider = VmObjectHandle::new(95).unwrap();
        let vm_object = runtime.machine.object_mut(returned.vm).unwrap();
        vm_object.configure_test_retail_initial_frame_return();
        vm_object
            .set_register(process_register::INVINCIBILITY_STATE, 4)
            .unwrap();
        vm_object.set_link(6, Some(missing_collider)).unwrap();

        // If the post-interpreter color phase runs, invincibility case four
        // resolves link six and faults on this deliberately absent collider.
        // Native returns to preorder traversal first and reclaims the object
        // through GoolObjectKill(0), so that lookup must never occur.
        let frame = runtime.run_frame(&mut SnapshotHost, 4).unwrap();
        assert_eq!(frame.executions.len(), 1);
        assert_eq!(
            frame.executions[0].result.as_ref().unwrap().reason,
            HaltReason::InvalidInitialReturn
        );
        assert!(runtime.arena().get(returned.arena).is_none());
        assert!(runtime.object_for_vm(returned.vm).is_none());
        assert_eq!(runtime.faulted_object_count(), 0);
        assert!(runtime.take_invincibility_event_faults().is_empty());
    }

    #[test]
    fn initial_level_mount_publishes_native_fade_words_before_entity_code() {
        let runtime = RetailRuntime::new_for_level(FADE_STEP_GLOBAL + 1, LevelId::N_SANITY_BEACH);

        assert_eq!(runtime.global_word(FADE_COUNTER_GLOBAL), Ok(288));
        assert_eq!(runtime.global_word(FADE_STEP_GLOBAL), Ok(32));
    }

    #[test]
    fn level_update_context_publishes_zone_flags_before_the_next_gool_pass() {
        let mut runtime =
            RetailRuntime::new_for_level(CURRENT_ZONE_FLAGS_GLOBAL + 1, LevelId::new_const(0x25));
        assert_eq!(runtime.global_word(CURRENT_ZONE_FLAGS_GLOBAL), Ok(0));
        let mut context = level_context(ZONE, false, vec![ZONE]);
        context.graphics_flags = 0x2002;

        runtime.set_level_state_context(context.clone());

        assert_eq!(runtime.global_word(CURRENT_ZONE_FLAGS_GLOBAL), Ok(0x2002));
        context.graphics_flags = 0x0404;
        runtime.set_level_state_context(context);
        assert_eq!(runtime.global_word(CURRENT_ZONE_FLAGS_GLOBAL), Ok(0x0404));
    }

    #[test]
    fn level_transition_resolver_preserves_requested_target_except_for_minus_two() {
        let requested = LevelId::new_const(0x2d);
        let saved = LevelId::new_const(0x09);
        assert_eq!(
            resolve_retail_level_transition(0x2d, 0x19, Some(saved)),
            Ok(ResolvedRetailLevelTransition {
                level: requested,
                bonus_return: false,
            })
        );
        assert_eq!(
            resolve_retail_level_transition(0x19, -2, Some(saved)),
            Ok(ResolvedRetailLevelTransition {
                level: saved,
                bonus_return: true,
            })
        );
        assert_eq!(
            resolve_retail_level_transition(0x19, -2, None),
            Err(RetailTransitionError::MissingSavedLevelState)
        );
        assert_eq!(
            resolve_retail_level_transition(-2, 0x19, Some(saved)),
            Err(RetailTransitionError::InvalidRequestedLevel(-2))
        );
    }

    #[test]
    fn session_mount_preserves_scalars_snapshot_rng_and_card_but_clears_pair_state() {
        let old_level = LevelId::new_const(0x03);
        let target = LevelId::new_const(0x17);
        let mut runtime = RetailRuntime::new_for_level(119, old_level);
        let old_object = spawn_test_object(&mut runtime, ZONE, 11, 2, 0);
        runtime.set_global_word(21, 0x1122_3344).unwrap();
        runtime.set_global_word(GAME_STATE_GLOBAL, 0x300).unwrap();
        runtime.set_global_word(46, 17).unwrap();
        runtime.set_global_word(59, 0x55).unwrap();
        runtime.set_global_word(61, 3).unwrap();
        runtime.set_global_word(82, 0xaabb_ccdd).unwrap();
        runtime.set_global_word(79, 77).unwrap();
        runtime.draw_count = 77;
        runtime.machine.set_draw_count(77);
        for index in POINTER_GLOBALS {
            runtime.set_global_word(index, 0x8000_0001).unwrap();
        }
        runtime.machine.set_random_seed(0xdead_beef);
        runtime
            .set_global_word(RESPAWN_COUNT_GLOBAL, 0x200)
            .unwrap();
        runtime.set_global_word(DEATH_COUNT_GLOBAL, 0x300).unwrap();
        // GOOL global stores occur inside Machine and can make the runtime's
        // operation-time mirrors stale between restart boundaries. Carry
        // export must retain the actual native words.
        runtime.respawn_count = 0x111;
        runtime.death_count = 0x222;
        runtime.saved_level_state = Some(level_snapshot(old_level));
        runtime.set_level_state_context(level_context(ZONE, true, vec![ZONE]));
        runtime.arena.spawn_table_mut().set_flags(42, 0xff).unwrap();
        runtime
            .machine
            .set_retail_level_spawn_tag(0, u16::try_from((target.get() << 9) | 0x2a).unwrap());

        let carry = runtime.export_session_carry();
        let snapshot = carry.saved_level_state.clone();
        let mut mounted = RetailRuntime::new_from_session(119, target, carry).unwrap();

        assert_eq!(mounted.level(), Some(target));
        assert_eq!(mounted.global_word(CURRENT_LEVEL_GLOBAL), Ok(0x1700));
        assert_eq!(mounted.global_word(21), Ok(0x1122_3344));
        assert_eq!(mounted.global_word(GAME_STATE_GLOBAL), Ok(0x300));
        assert_eq!(mounted.global_word(46), Ok(17));
        assert_eq!(mounted.global_word(59), Ok(0x55));
        assert_eq!(mounted.global_word(61), Ok(3));
        assert_eq!(mounted.global_word(82), Ok(0xaabb_ccdd));
        assert_eq!(
            mounted.global_word(NEXT_DISPLAY_GLOBAL),
            Ok(INITIAL_DISPLAY_MASK)
        );
        assert_eq!(
            mounted.global_word(CURRENT_DISPLAY_GLOBAL),
            Ok(INITIAL_DISPLAY_MASK)
        );
        assert_eq!(mounted.global_word(79), Ok(77));
        assert_eq!(mounted.global_word(117), Ok(0x19000));
        for index in POINTER_GLOBALS {
            assert_eq!(mounted.global_word(index), Ok(0), "global {index}");
        }
        assert_eq!(mounted.machine.random_seed(), 0xdead_beef);
        assert_eq!(mounted.saved_level_state(), snapshot.as_ref());
        assert_eq!(mounted.respawn_count, 0x200);
        assert_eq!(mounted.death_count, 0x300);
        assert!(mounted.arena().is_empty());
        assert!(mounted.object_for_vm(old_object.vm).is_none());
        assert_eq!(mounted.arena.spawn_table().flags(42), Some(8));
        assert_eq!(mounted.machine.spawn_flags(42), Ok(8));
        assert_eq!(mounted.frame_index(), 0);
        assert_eq!(mounted.draw_count(), 77);
        mounted.set_level_state_context(level_context(ZONE_B, false, vec![ZONE_B]));
        assert!(mounted.level_state_context().unwrap().first_spawn);

        let mismatch = RetailRuntime::new_from_session(118, target, mounted.export_session_carry());
        assert_eq!(
            mismatch,
            Err(RetailSessionImportError::GlobalWordCount {
                expected: 118,
                actual: 119,
            })
        );
    }

    #[test]
    fn destination_crash_spawn_replaces_snapshot_except_during_bonus_guard() {
        let old_level = LevelId::new_const(0x03);
        let target = LevelId::new_const(0x17);
        let mut source = RetailRuntime::new_for_level(119, old_level);
        source.saved_level_state = Some(level_snapshot(old_level));
        let carry = source.export_session_carry();
        let crash_entities = [entity(5, 0, 0)];
        let neighbors = [NeighborZone {
            eid: ZONE_B,
            display_flags: ACTIVE_ZONE_DISPLAY_BIT,
            entities: &crash_entities,
        }];

        let mut ordinary = RetailRuntime::new_from_session(119, target, carry.clone()).unwrap();
        ordinary.set_level_state_context(level_context(ZONE_B, false, vec![ZONE_B]));
        let attempts = ordinary.spawn_current_zone_neighbors(&neighbors, &mut SnapshotHost);
        assert!(attempts[0].result.is_ok());
        assert_eq!(ordinary.saved_level_state().unwrap().level, target);
        assert_eq!(
            ordinary.saved_level_state().unwrap().location.path.zone,
            ZONE_B
        );

        let mut bonus_return = RetailRuntime::new_from_session(119, target, carry).unwrap();
        let carried_snapshot = bonus_return.saved_level_state().cloned().unwrap();
        bonus_return.set_level_state_context(level_context(ZONE_B, false, vec![ZONE_B]));
        bonus_return.set_initial_crash_save_suppressed(true);
        let attempts = bonus_return.spawn_current_zone_neighbors(&neighbors, &mut SnapshotHost);
        bonus_return.set_initial_crash_save_suppressed(false);
        assert!(attempts[0].result.is_ok());
        assert_eq!(
            bonus_return.saved_level_state(),
            Some(&carried_snapshot),
            "the bonus pre-restart scan must not overwrite its return snapshot"
        );
    }

    #[test]
    fn every_native_main_selector_replaces_the_initial_restart_snapshot() {
        let old_level = LevelId::new_const(0x03);
        let cases = [
            (
                "id-range special",
                LevelId::new_const(0x09),
                entity(1, 2, 7),
            ),
            (
                "Great Hall executable",
                LevelId::new_const(0x2c),
                entity(5, 0x2c, 0),
            ),
            ("Ending executable", LevelId::ENDING, entity(5, 0x30, 0)),
        ];

        for (label, target, main_entity) in cases {
            assert!(
                EntitySpawnDescriptor::from(&main_entity).selects_main_object(),
                "{label} must select native's dedicated main allocation"
            );
            assert!(
                !EntitySpawnDescriptor::from(&main_entity).is_crash_program(),
                "{label} exercises a non-Crash main selector"
            );

            let mut source = RetailRuntime::new_for_level(119, old_level);
            source.saved_level_state = Some(level_snapshot(old_level));
            let mut mounted =
                RetailRuntime::new_from_session(119, target, source.export_session_carry())
                    .unwrap();
            mounted.set_level_state_context(level_context(ZONE_B, false, vec![ZONE_B]));
            let neighbors = [NeighborZone {
                eid: ZONE_B,
                display_flags: ACTIVE_ZONE_DISPLAY_BIT,
                entities: std::slice::from_ref(&main_entity),
            }];

            let attempts = mounted.spawn_current_zone_neighbors(&neighbors, &mut SnapshotHost);
            let main = *attempts[0]
                .result
                .as_ref()
                .unwrap_or_else(|error| panic!("{label} failed to bind: {error:?}"));

            assert!(main.arena.is_dedicated_main(), "{label}");
            let snapshot = mounted
                .saved_level_state()
                .unwrap_or_else(|| panic!("{label} omitted native's initial LevelSaveState"));
            assert_eq!(snapshot.level, target, "{label}");
            assert_eq!(snapshot.location.path.zone, ZONE_B, "{label}");
        }
    }

    #[test]
    fn fresh_restricted_direct_boot_seeds_restart_without_overwriting_bonus_return() {
        let parent = LevelId::new_const(0x09);
        let bonus = LevelId::new_const(0x26);
        let crash_entities = [entity(5, 0, 0)];
        let neighbors = [NeighborZone {
            eid: ZONE_B,
            display_flags: ACTIVE_ZONE_DISPLAY_BIT,
            entities: &crash_entities,
        }];
        let mut restricted_context = level_context(ZONE_B, false, vec![ZONE_B]);
        restricted_context.graphics_flags |= SAVE_RESTRICTED_ZONE_FLAG;

        let mut direct = RetailRuntime::new_for_level(119, bonus);
        direct.set_level_state_context(restricted_context.clone());
        let attempts = direct.spawn_current_zone_neighbors(&neighbors, &mut SnapshotHost);
        assert!(attempts[0].result.is_ok());
        let direct_snapshot = direct
            .saved_level_state()
            .expect("a fresh restricted direct boot must remain restartable");
        assert_eq!(direct_snapshot.level, bonus);
        assert_eq!(direct_snapshot.location.path.zone, ZONE_B);

        let mut source = RetailRuntime::new_for_level(119, parent);
        source.saved_level_state = Some(level_snapshot(parent));
        let mut entered_bonus =
            RetailRuntime::new_from_session(119, bonus, source.export_session_carry()).unwrap();
        entered_bonus.set_level_state_context(restricted_context);
        let attempts = entered_bonus.spawn_current_zone_neighbors(&neighbors, &mut SnapshotHost);
        assert!(attempts[0].result.is_ok());
        assert_eq!(
            entered_bonus
                .saved_level_state()
                .map(|snapshot| snapshot.level),
            Some(parent),
            "a real bonus entry must retain its parent-level return snapshot"
        );
    }

    #[test]
    fn title_session_mount_applies_only_core_title_counter_resets() {
        let mut runtime = RetailRuntime::new_for_level(119, LevelId::new_const(0x17));
        runtime.respawn_count = 0x300;
        runtime.death_count = 0x400;
        for (index, value) in [
            (RESPAWN_COUNT_GLOBAL, 0x300),
            (DEATH_COUNT_GLOBAL, 0x400),
            (CORTEX_COUNT_GLOBAL, 0x500),
            (BRIO_COUNT_GLOBAL, 0x600),
            (TAWNA_COUNT_GLOBAL, 0x700),
            (CHECKPOINT_ID_GLOBAL, 0x800),
            (46, 19),
            (63, 0x1234),
        ] {
            runtime.set_global_word(index, value).unwrap();
        }

        let mounted =
            RetailRuntime::new_from_session(119, LevelId::TITLE, runtime.export_session_carry())
                .unwrap();

        assert_eq!(mounted.respawn_count, 0);
        assert_eq!(mounted.death_count, 0);
        for index in [
            RESPAWN_COUNT_GLOBAL,
            DEATH_COUNT_GLOBAL,
            CORTEX_COUNT_GLOBAL,
            BRIO_COUNT_GLOBAL,
            TAWNA_COUNT_GLOBAL,
        ] {
            assert_eq!(mounted.global_word(index), Ok(0));
        }
        assert_eq!(mounted.global_word(CHECKPOINT_ID_GLOBAL), Ok(u32::MAX));
        assert_eq!(mounted.global_word(46), Ok(19));
        assert_eq!(mounted.global_word(63), Ok(0x1234));
    }

    #[test]
    fn level_end_broadcast_is_all_root_postorder_and_preserves_requested_target() {
        let mut runtime = RetailRuntime::new_for_level(119, LevelId::new_const(0x03));
        let root_zero = spawn_test_object(&mut runtime, ZONE, 10, 2, 0);
        let root_seven_parent = spawn_test_object(&mut runtime, ZONE, 11, 2, 0);
        let root_seven_child = attach_test_child(&mut runtime, root_seven_parent, ZONE, 2);
        runtime
            .arena
            .reparent_to_root(root_zero.arena, RootHandle::new(0).unwrap())
            .unwrap();
        runtime
            .arena
            .reparent_to_root(root_seven_parent.arena, RootHandle::new(7).unwrap())
            .unwrap();
        configure_level_end_transition(&mut runtime, root_zero, 0x05);
        configure_level_end_transition(&mut runtime, root_seven_child, 0x06);
        configure_level_end_transition(&mut runtime, root_seven_parent, 0x07);

        let report = runtime
            .finish_level_transition(&mut SnapshotHost, 0x09)
            .unwrap();

        assert_eq!(
            report.effects,
            [
                VmEffect::Transition(0x05),
                VmEffect::Transition(0x06),
                VmEffect::Transition(0x07),
            ]
        );
        assert_eq!(report.next_lid_after_event, 0x07);
        assert_eq!(
            report.resolved,
            ResolvedRetailLevelTransition {
                level: LevelId::new_const(0x09),
                bonus_return: false,
            }
        );
        assert!(report.event_failures.is_empty());
    }

    #[test]
    fn level_end_broadcast_continues_after_checked_event_failure() {
        let mut runtime = RetailRuntime::new_for_level(119, LevelId::new_const(0x03));
        let malformed = spawn_test_object(&mut runtime, ZONE, 12, 2, 0);
        let later = spawn_test_object(&mut runtime, ZONE, 13, 2, 0);
        runtime
            .arena
            .reparent_to_root(malformed.arena, RootHandle::new(0).unwrap())
            .unwrap();
        runtime
            .arena
            .reparent_to_root(later.arena, RootHandle::new(1).unwrap())
            .unwrap();
        runtime
            .machine
            .object_mut(malformed.vm)
            .unwrap()
            .configure_test_event_interrupt(LEVEL_END_EVENT, vec![0xff00_0000])
            .unwrap();
        configure_level_end_transition(&mut runtime, later, 0x17);

        let report = runtime
            .finish_level_transition(&mut SnapshotHost, 0x09)
            .unwrap();

        assert_eq!(report.effects, [VmEffect::Transition(0x17)]);
        assert_eq!(report.next_lid_after_event, 0x17);
        assert_eq!(report.resolved.level, LevelId::new_const(0x09));
        assert!(matches!(
            report.event_failures.as_slice(),
            [RetailLevelEndEventFailure {
                object,
                error: RuntimeError::Vm(VmError::UnknownOpcode(0xff)),
            }] if *object == malformed
        ));
    }

    #[test]
    fn level_end_load_kind_survives_a_later_save_and_keeps_broadcasting() {
        let current = LevelId::new_const(0x26);
        let saved = LevelId::new_const(0x09);
        let mut runtime = RetailRuntime::new_for_level(119, current);
        let _main = spawn_test_object(&mut runtime, ZONE, 1, 0, 0);
        let loader = spawn_test_object(&mut runtime, ZONE, 14, 2, 0);
        let later = spawn_test_object(&mut runtime, ZONE, 15, 2, 0);
        runtime
            .arena
            .reparent_to_root(loader.arena, RootHandle::new(0).unwrap())
            .unwrap();
        runtime
            .arena
            .reparent_to_root(later.arena, RootHandle::new(7).unwrap())
            .unwrap();
        runtime.saved_level_state = Some(level_snapshot(saved));
        runtime.set_level_state_context(level_context(ZONE, false, vec![ZONE]));
        runtime
            .machine
            .object_mut(loader.vm)
            .unwrap()
            .configure_test_event_interrupt(
                LEVEL_END_EVENT,
                vec![
                    misc(12, 1, 0x0be0),
                    misc(12, 0, 0x0be0),
                    misc(12, 6, 0x0e00),
                    0x8280_0000,
                ],
            )
            .unwrap();
        runtime
            .machine
            .object_mut(loader.vm)
            .unwrap()
            .set_register(0, 0x4321)
            .unwrap();
        runtime
            .machine
            .object_mut(later.vm)
            .unwrap()
            .configure_test_event_interrupt(
                LEVEL_END_EVENT,
                vec![
                    Instruction::encode(0x1f, 0x0be0, 0x083c),
                    Instruction::encode(0x11, 0x0e1f, 0x0e00),
                    misc(12, 6, 0x0e00),
                    0x8280_0000,
                ],
            )
            .unwrap();
        runtime
            .machine
            .object_mut(later.vm)
            .unwrap()
            .set_register(0, 0x1234)
            .unwrap();
        runtime
            .machine
            .set_global_word(BONUS_ROUND_GLOBAL, 0x100)
            .unwrap();

        let report = runtime
            .finish_level_transition(&mut SnapshotHost, 0x19)
            .unwrap();

        assert_eq!(
            report.effects,
            [
                VmEffect::LoadState {
                    object: loader.vm,
                    saved_level: Some(saved),
                },
                VmEffect::SaveState(loader.vm),
                VmEffect::MidiTogglePlayback {
                    object: loader.vm,
                    value: 0x4321,
                },
                VmEffect::MidiTogglePlayback {
                    object: later.vm,
                    value: 0,
                },
            ]
        );
        assert_eq!(report.next_lid_after_event, -2);
        assert_eq!(
            report.resolved,
            ResolvedRetailLevelTransition {
                level: current,
                bonus_return: true,
            }
        );
        assert_eq!(
            report
                .carry
                .saved_level_state
                .as_ref()
                .map(|snapshot| snapshot.level),
            Some(current),
            "the later SaveState mutates the eventual -2 target without changing the earlier load kind"
        );
        assert!(report.event_failures.is_empty());
        assert!(report.carry.first_spawn);
        assert!(runtime.level_state_context().unwrap().first_spawn);
        assert!(!runtime.machine.level_restart_requested());
    }

    #[test]
    fn same_level_load_during_level_end_is_a_checked_restart_boundary() {
        let level = LevelId::new_const(0x09);
        let mut runtime = RetailRuntime::new_for_level(119, level);
        let loader = spawn_test_object(&mut runtime, ZONE, 16, 2, 0);
        runtime.saved_level_state = Some(level_snapshot(level));
        runtime
            .machine
            .object_mut(loader.vm)
            .unwrap()
            .configure_test_event_interrupt(LEVEL_END_EVENT, vec![misc(12, 1, 0x0be0), 0x8280_0000])
            .unwrap();

        assert_eq!(
            runtime.finish_level_transition(&mut SnapshotHost, 0x17),
            Err(RuntimeError::SameLevelRestartDuringLevelEnd(level))
        );
    }

    #[test]
    fn display_mask_latches_after_traversal_with_one_frame_latency() {
        let entities = [entity(10, 3, 1)];
        let neighbors = [NeighborZone {
            eid: ZONE,
            display_flags: 2,
            entities: &entities,
        }];
        let mut runtime = RetailRuntime::new(CURRENT_DISPLAY_GLOBAL + 1);
        let _object = *runtime.spawn_current_zone_neighbors(&neighbors, &mut SnapshotHost)[0]
            .result
            .as_ref()
            .unwrap();

        runtime
            .machine
            .set_global_word(NEXT_DISPLAY_GLOBAL, 0)
            .unwrap();
        assert_eq!(runtime.current_display_mask(), INITIAL_DISPLAY_MASK);
        let first = runtime.run_frame(&mut SnapshotHost, 1).unwrap();
        assert_eq!(runtime.current_display_mask(), 0);
        assert_eq!(first.executions.len(), 1);

        let suppressed = runtime.run_frame(&mut SnapshotHost, 1).unwrap();
        assert!(suppressed.executions.is_empty());

        runtime
            .machine
            .set_global_word(NEXT_DISPLAY_GLOBAL, ANIMATE_OBJECTS | 0x20)
            .unwrap();
        let still_suppressed = runtime.run_frame(&mut SnapshotHost, 1).unwrap();
        assert!(still_suppressed.executions.is_empty());
        assert_eq!(runtime.current_display_mask(), ANIMATE_OBJECTS | 0x20);
        let animated = runtime.run_frame(&mut SnapshotHost, 1).unwrap();
        assert_eq!(animated.executions.len(), 1);
    }

    #[test]
    fn opcode_twenty_global_nine_write_latches_distinct_object_and_world_masks() {
        let mut runtime = RetailRuntime::new(CURRENT_DISPLAY_GLOBAL + 1);
        let before_writer = spawn_test_object(&mut runtime, ZONE, 10, 2, 0);
        let writer = spawn_test_object(&mut runtime, ZONE, 11, 2, 0);
        let main = spawn_test_object(&mut runtime, ZONE, 12, 0, 0);
        let after_writer = spawn_test_object(&mut runtime, ZONE, 13, 2, 0);
        runtime
            .arena
            .reparent_to_root(before_writer.arena, RootHandle::new(2).unwrap())
            .unwrap();
        runtime
            .arena
            .reparent_to_root(after_writer.arena, RootHandle::new(7).unwrap())
            .unwrap();

        let initial_mask = INITIAL_DISPLAY_MASK;
        let later_object_mask = initial_mask | 0x1_0000;
        let mut writer_vm =
            VmObject::new(writer.vm, vec![Instruction::encode(0x20, 0, 1), RETURN]).unwrap();
        writer_vm.configure_test_program_identity_with_type(0x100, 0);
        writer_vm.set_internal(0, later_object_mask).unwrap();
        writer_vm
            .set_internal(1, (CURRENT_DISPLAY_GLOBAL as u32) << 8)
            .unwrap();
        runtime.machine.upsert_object(writer_vm).unwrap();
        runtime
            .machine
            .set_global_word(CURRENT_DISPLAY_GLOBAL, initial_mask)
            .unwrap();
        runtime
            .machine
            .set_global_word(NEXT_DISPLAY_GLOBAL, initial_mask)
            .unwrap();
        // This is the value the browser must retain for the already-submitted
        // world even though object traversal can change global nine below.
        let world_display_mask = runtime.current_display_mask();

        let frame = runtime.run_frame(&mut SnapshotHost, 2).unwrap();
        assert!(
            frame.executions.iter().any(|execution| {
                execution.object == writer && execution.result.as_ref().is_ok()
            })
        );

        let objects = runtime.render_objects().unwrap();
        let mask = |object| {
            objects
                .iter()
                .find(|snapshot| snapshot.object == object)
                .unwrap()
                .display_mask
        };
        assert_eq!(world_display_mask, initial_mask);
        assert_eq!(mask(before_writer), world_display_mask);
        assert_eq!(mask(writer), later_object_mask);
        assert_eq!(mask(main), later_object_mask);
        assert_eq!(mask(after_writer), later_object_mask);
        assert_eq!(
            runtime.current_display_mask(),
            initial_mask,
            "the end-of-frame next-to-current latch cannot reconstruct per-object masks"
        );
    }

    #[test]
    fn retail_animation_masks_match_every_category_and_force_path() {
        assert!(!retail_animation_mask_enabled(
            0xffff & !ANIMATE_OBJECTS,
            0,
            0,
            Some(0x100)
        ));
        for (category, bit) in [
            (0x100, 0x20),
            (0x200, 0x100),
            (0x300, 0x80),
            (0x400, 0x400),
            (0x500, 0x80),
            (0x600, 0x80),
        ] {
            assert!(retail_animation_mask_enabled(
                ANIMATE_OBJECTS | bit,
                0,
                0,
                Some(category)
            ));
            assert!(!retail_animation_mask_enabled(
                ANIMATE_OBJECTS,
                0,
                0,
                Some(category)
            ));
        }
        assert!(!retail_animation_mask_enabled(
            ANIMATE_OBJECTS | 0x20,
            0,
            0,
            Some(0x700)
        ));
        assert!(retail_animation_mask_enabled(
            ANIMATE_OBJECTS | FORCE_ANIMATE_MENUS,
            FORCE_UPDATE_STATUS_B,
            0,
            Some(0x700)
        ));
        assert!(retail_animation_mask_enabled(
            ANIMATE_OBJECTS | 0x20,
            FORCE_UPDATE_STATUS_B,
            0,
            Some(0x100)
        ));
        assert!(retail_animation_mask_enabled(
            ANIMATE_OBJECTS | 0x20,
            0,
            MENU_TEXT_STATE_FLAG,
            Some(0x100)
        ));
        assert!(!retail_animation_mask_enabled(
            ANIMATE_OBJECTS,
            0,
            MENU_TEXT_STATE_FLAG,
            Some(0x100)
        ));
    }

    #[test]
    fn paused_update_override_is_exactly_authored_type_four_subtypes_four_and_seven() {
        let category_two_mask = ANIMATE_OBJECTS | 0x100;
        for subtype in [4, 7] {
            assert!(retail_animation_update_enabled(
                category_two_mask,
                0,
                0,
                Some(0x200),
                Some(4),
                subtype,
                true,
            ));
        }
        for (category, object_type, subtype) in
            [(0x100, 4, 4), (0x200, 3, 4), (0x200, 4, 3), (0x200, 4, 8)]
        {
            assert!(!retail_animation_update_enabled(
                0xffff,
                0,
                0,
                Some(category),
                Some(object_type),
                subtype,
                true,
            ));
        }
        assert!(!retail_animation_update_enabled(
            0xffff, 0, 0, None, None, 4, true,
        ));
        assert!(!retail_animation_update_enabled(
            category_two_mask | FORCE_ANIMATE_MENUS,
            FORCE_UPDATE_STATUS_B,
            0,
            Some(0x200),
            Some(4),
            4,
            true,
        ));
        assert!(retail_animation_update_enabled(
            category_two_mask,
            FORCE_UPDATE_STATUS_B,
            0,
            Some(0x200),
            Some(4),
            4,
            true,
        ));
        assert!(retail_animation_update_enabled(
            category_two_mask,
            0,
            0,
            Some(0x200),
            Some(3),
            4,
            false,
        ));
    }

    #[test]
    fn paused_update_override_reads_mutable_live_process_subtype() {
        let mut runtime =
            RetailRuntime::new_for_level(PBAK_STATE_GLOBAL + 1, LevelId::N_SANITY_BEACH);
        let origin_four_live_three = spawn_test_object(&mut runtime, ZONE, 10, 4, 4);
        let origin_three_live_four = spawn_test_object(&mut runtime, ZONE, 11, 4, 3);
        for (object, live_subtype) in [(origin_four_live_three, 3), (origin_three_live_four, 4)] {
            let vm = runtime.machine.object_mut(object.vm).unwrap();
            vm.configure_test_program_identity_with_type(0x200, 4);
            vm.set_register(process_register::SUBTYPE, live_subtype)
                .unwrap();
        }
        assert_eq!(
            runtime
                .arena
                .get(origin_four_live_three.arena)
                .unwrap()
                .origin()
                .subtype(),
            4
        );
        assert_eq!(
            runtime
                .arena
                .get(origin_three_live_four.arena)
                .unwrap()
                .origin()
                .subtype(),
            3
        );
        runtime
            .machine
            .set_global_word(CURRENT_DISPLAY_GLOBAL, ANIMATE_OBJECTS | 0x100)
            .unwrap();
        runtime.pause.paused = true;

        assert_eq!(
            runtime.retail_animation_enabled::<()>(origin_four_live_three, true),
            Ok(false)
        );
        assert_eq!(
            runtime.retail_animation_enabled::<()>(origin_three_live_four, true),
            Ok(true)
        );
        let frame = runtime.run_frame(&mut SnapshotHost, 1).unwrap();
        assert_eq!(
            frame
                .executions
                .iter()
                .map(|execution| execution.object)
                .collect::<Vec<_>>(),
            [origin_three_live_four]
        );
    }

    #[test]
    fn retail_pause_gate_matches_level_title_and_pbak_globals() {
        let cases = [
            (LevelId::N_SANITY_BEACH, -1, 0, false),
            (LevelId::N_SANITY_BEACH, 0, 0, true),
            (LevelId::N_SANITY_BEACH, 1, 2, false),
            (LevelId::TITLE, 0, 0, false),
            (LevelId::TITLE, 1, 0, true),
            (LevelId::LEVEL_COMPLETE, 0, 0, false),
            (LevelId::LEVEL_COMPLETE, 1, 0, true),
            (LevelId::INTRO, 0, 0, false),
            (LevelId::INTRO, 1, 0, true),
            (LevelId::ENDING, 0, 0, true),
        ];
        for (level, title_pause_state, pbak_state, expected) in cases {
            let mut runtime = RetailRuntime::new_for_level(PBAK_STATE_GLOBAL + 1, level);
            runtime
                .set_global_word(TITLE_PAUSE_STATE_GLOBAL, title_pause_state as u32)
                .unwrap();
            runtime
                .set_global_word(PBAK_STATE_GLOBAL, pbak_state)
                .unwrap();
            assert_eq!(
                runtime.can_retail_pause(),
                Ok(expected),
                "level {level}, title pause {title_pause_state}, PBAK {pbak_state}"
            );
        }
    }

    #[test]
    fn pause_controller_create_failure_is_a_nonfatal_failed_toggle() {
        let mut runtime =
            RetailRuntime::new_for_level(PBAK_STATE_GLOBAL + 1, LevelId::N_SANITY_BEACH);
        let main = spawn_test_object(&mut runtime, ZONE, 1, 0, 0);
        let mut host = RejectProgramHost;

        assert_eq!(
            runtime.update_retail_pause(true, ZONE, &mut host),
            Ok(RetailPauseUpdate::Failed)
        );
        assert!(!runtime.retail_pause_state().paused());
        assert_eq!(runtime.retail_pause_state().status(), 0);
        assert_eq!(runtime.retail_pause_state().controller(), None);
        assert_eq!(runtime.global_word(PAUSE_OBJECT_GLOBAL), Ok(0));
        assert!(
            runtime
                .arena
                .preorder(TreeParent::Root(RootHandle::new(7).unwrap()))
                .unwrap()
                .next()
                .is_none(),
            "failed controller materialization leaked a root-seven object"
        );

        let frame = runtime.run_frame(&mut host, 1).unwrap();
        assert_eq!(
            frame
                .executions
                .iter()
                .map(|execution| execution.object)
                .collect::<Vec<_>>(),
            [main]
        );
    }

    #[test]
    fn pause_controller_lives_under_root_seven_and_resumes_with_clock_rewind() {
        let mut runtime =
            RetailRuntime::new_for_level(PBAK_STATE_GLOBAL + 1, LevelId::N_SANITY_BEACH);
        let main = spawn_test_object(&mut runtime, ZONE, 1, 0, 0);
        runtime
            .arena
            .reparent_to_root(main.arena, RootHandle::new(6).unwrap())
            .unwrap();
        let mut host = SnapshotHost;

        let paused = runtime.update_retail_pause(true, ZONE, &mut host).unwrap();
        let RetailPauseUpdate::Paused { controller } = paused else {
            panic!("START did not create the authored pause controller");
        };
        let spawned = runtime.arena.get(controller.arena).unwrap();
        assert_eq!(
            spawned.parent(),
            TreeParent::Root(RootHandle::new(7).unwrap())
        );
        assert_eq!(spawned.zone(), Eid::NONE);
        assert_eq!(spawned.origin().executable(), PAUSE_CONTROLLER_EXECUTABLE);
        assert_eq!(spawned.origin().subtype(), PAUSE_CONTROLLER_SUBTYPE);
        assert_eq!(
            runtime.global_word(PAUSE_OBJECT_GLOBAL),
            Ok(CollisionObjectReference::new(controller.vm).to_word())
        );
        assert_eq!(runtime.retail_pause_state().status(), 1);
        assert!(runtime.retail_pause_state().paused());

        let mut hook_calls = 0;
        let paused_frame = runtime
            .run_frame_with_traversal_hook(&mut host, 1, |_, _, boundary| {
                assert_eq!(
                    boundary,
                    RetailTraversalBoundary::BeforeMainObjectUpdate {
                        root: RootHandle::new(6).unwrap(),
                        object: main,
                    }
                );
                hook_calls += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(hook_calls, 1, "PadUpdate still runs at Crash while paused");
        assert!(paused_frame.executions.is_empty());
        assert_eq!(runtime.frame_index(), 1);
        assert_eq!(runtime.draw_count(), 0);
        assert_eq!(runtime.next_frame_stamp(), 1);

        assert_eq!(
            runtime.update_retail_pause(false, ZONE, &mut host),
            Ok(RetailPauseUpdate::Unchanged)
        );
        assert_eq!(runtime.retail_pause_state().status(), 0);
        assert_eq!(
            runtime.update_retail_pause(true, ZONE, &mut host),
            Ok(RetailPauseUpdate::Resumed {
                controller: Some(controller),
                event_faulted: false,
            })
        );
        assert_eq!(runtime.frame_index(), 0, "resume restores pause_draw_stamp");
        assert_eq!(runtime.global_word(PAUSE_OBJECT_GLOBAL), Ok(0));
        assert_eq!(runtime.retail_pause_state().status(), -1);
        assert!(!runtime.retail_pause_state().paused());

        let resumed_frame = runtime.run_frame(&mut host, 1).unwrap();
        assert!(resumed_frame.executions.iter().any(|execution| {
            execution.object == controller
                && matches!(
                    execution.result,
                    Ok(Execution {
                        reason: HaltReason::Halted,
                        ..
                    })
                )
        }));
        assert!(runtime.object_for_vm(controller.vm).is_none());
        assert_eq!(runtime.retail_pause_state().controller(), None);
        assert_eq!(
            runtime.take_cleanup_actions(),
            [RuntimeCleanupAction::FreeObjectAudio(controller)]
        );
    }

    #[test]
    fn malformed_resume_event_is_diagnostic_and_does_not_keep_game_paused() {
        let mut runtime =
            RetailRuntime::new_for_level(PBAK_STATE_GLOBAL + 1, LevelId::N_SANITY_BEACH);
        let mut host = SnapshotHost;
        let RetailPauseUpdate::Paused { controller } =
            runtime.update_retail_pause(true, ZONE, &mut host).unwrap()
        else {
            unreachable!();
        };
        runtime
            .machine
            .object_mut(controller.vm)
            .unwrap()
            .configure_test_event_interrupt(PAUSE_RESUME_EVENT, vec![0xff00_0000])
            .unwrap();

        assert_eq!(
            runtime.update_retail_pause(true, ZONE, &mut host),
            Ok(RetailPauseUpdate::Resumed {
                controller: Some(controller),
                event_faulted: true,
            })
        );
        assert!(!runtime.retail_pause_state().paused());
        assert_eq!(runtime.global_word(PAUSE_OBJECT_GLOBAL), Ok(0));
        assert_eq!(
            runtime.take_pause_event_faults(),
            [RuntimePauseEventFault { object: controller }]
        );
    }

    #[test]
    fn screen_load_reset_clears_pause_latches_before_ordinary_teardown() {
        let mut runtime =
            RetailRuntime::new_for_level(PBAK_STATE_GLOBAL + 1, LevelId::N_SANITY_BEACH);
        let mut host = SnapshotHost;
        let RetailPauseUpdate::Paused { controller } =
            runtime.update_retail_pause(true, ZONE, &mut host).unwrap()
        else {
            unreachable!();
        };

        runtime.reset_retail_pause_for_screen_load().unwrap();

        assert!(!runtime.retail_pause_state().paused());
        assert_eq!(runtime.retail_pause_state().status(), -1);
        assert_eq!(runtime.retail_pause_state().controller(), None);
        assert_eq!(runtime.global_word(PAUSE_OBJECT_GLOBAL), Ok(0));
        assert!(runtime.object_for_vm(controller.vm).is_some());

        let report = runtime.terminate_all_objects(&mut host).unwrap();
        assert_eq!(report.terminated, [controller]);
        assert!(runtime.object_for_vm(controller.vm).is_none());
    }

    #[test]
    fn retail_display_masks_match_post_update_visibility_and_categories() {
        assert!(!retail_display_mask_enabled(
            0xffff,
            0,
            0,
            Some(0x100),
            false
        ));
        assert!(!retail_display_mask_enabled(
            0xffff,
            INVISIBLE_STATUS_B,
            0,
            Some(0x100),
            true
        ));
        for (category, bit) in [
            (0x100, 0x10),
            (0x200, 0x200),
            (0x300, 0x40),
            (0x400, 0x800),
            (0x500, 0x40),
            (0x600, 0x40),
        ] {
            assert!(retail_display_mask_enabled(
                DISPLAY_OBJECTS | bit,
                0,
                0,
                Some(category),
                true
            ));
            assert!(!retail_display_mask_enabled(
                DISPLAY_OBJECTS,
                0,
                0,
                Some(category),
                true
            ));
        }
        assert!(retail_display_mask_enabled(
            DISPLAY_OBJECTS | FORCE_DISPLAY_MENUS,
            FORCE_UPDATE_STATUS_B,
            0,
            Some(0x700),
            true
        ));
        assert!(retail_display_mask_enabled(
            DISPLAY_OBJECTS | 0x10,
            FORCE_UPDATE_STATUS_B,
            0,
            Some(0x100),
            true
        ));
    }

    #[test]
    fn newly_latched_count_draws_bit_controls_the_following_counter() {
        let mut runtime = RetailRuntime::new(crate::gool::DRAW_COUNT_GLOBAL + 1);
        assert_eq!(runtime.next_frame_stamp(), 0);
        runtime
            .machine
            .set_global_word(NEXT_DISPLAY_GLOBAL, INITIAL_DISPLAY_MASK & !0x1000)
            .unwrap();
        runtime.run_frame(&mut SnapshotHost, 1).unwrap();
        assert_eq!(runtime.draw_count(), 0);
        assert_eq!(
            runtime.next_frame_stamp(),
            1,
            "GOOL time advances while the independent draw counter is frozen"
        );

        runtime
            .machine
            .set_global_word(NEXT_DISPLAY_GLOBAL, INITIAL_DISPLAY_MASK)
            .unwrap();
        runtime.run_frame(&mut SnapshotHost, 1).unwrap();
        assert_eq!(runtime.draw_count(), 1);
        assert_eq!(runtime.next_frame_stamp(), 2);
        assert_eq!(
            runtime
                .machine()
                .global_word(crate::gool::DRAW_COUNT_GLOBAL),
            Ok(1)
        );

        runtime.finish_display_frame(true).unwrap();
        assert_eq!(runtime.draw_count(), 1, "paused GLUpdate never increments");
        assert_eq!(runtime.next_frame_stamp(), 2);
    }

    #[test]
    fn camera_gem_stamp_reads_the_live_authored_global() {
        let mut runtime = RetailRuntime::new_for_level(256, LevelId::N_SANITY_BEACH);
        assert_eq!(runtime.gem_stamp(), Ok(0));

        runtime.set_global_word(GEM_STAMP_GLOBAL, 85).unwrap();

        assert_eq!(runtime.gem_stamp(), Ok(85));
    }

    #[test]
    fn display_fade_publishes_retail_signed_sentinels_at_frame_end() {
        let mut runtime = RetailRuntime::new(FADE_STEP_GLOBAL + 1);
        runtime.set_global_word(FADE_STEP_GLOBAL, 32).unwrap();
        runtime
            .set_global_word(FADE_COUNTER_GLOBAL, (-256_i32) as u32)
            .unwrap();

        for expected in [-224_i32, -192, -160, -128, -96, -64, -32, -2, -1, -1] {
            runtime.finish_display_frame(true).unwrap();
            assert_eq!(
                runtime.global_word(FADE_COUNTER_GLOBAL),
                Ok(expected as u32)
            );
        }

        runtime.set_global_word(FADE_COUNTER_GLOBAL, 288).unwrap();
        for expected in [256_u32, 224, 192, 160, 128, 96, 64, 32, 0] {
            runtime.finish_display_frame(false).unwrap();
            assert_eq!(runtime.global_word(FADE_COUNTER_GLOBAL), Ok(expected));
        }
    }

    #[test]
    fn display_fade_preserves_zero_when_the_authored_hold_black_bit_is_set() {
        let mut runtime = RetailRuntime::new(FADE_STEP_GLOBAL + 1);
        runtime.set_global_word(FADE_STEP_GLOBAL, 32).unwrap();
        runtime
            .set_global_word(FADE_COUNTER_GLOBAL, (-32_i32) as u32)
            .unwrap();
        runtime
            .set_global_word(NEXT_DISPLAY_GLOBAL, INITIAL_DISPLAY_MASK | 0x20_0000)
            .unwrap();

        runtime.finish_display_frame(true).unwrap();

        assert_eq!(runtime.global_word(FADE_COUNTER_GLOBAL), Ok(0));
    }

    #[test]
    fn title_update_runs_between_gool_request_and_gl_fade_latch() {
        const IMAGE_LOAD: u32 = 0x22_3ff0;
        const IMAGE_ACTIVE: u32 = IMAGE_LOAD | DISPLAY_OBJECTS | ANIMATE_OBJECTS;

        let mut runtime = RetailRuntime::new_for_level(256, LevelId::TITLE);
        runtime
            .configure_retail_title(TitleScreen::PublisherFirst, true)
            .unwrap();
        runtime
            .set_global_word(NEXT_DISPLAY_GLOBAL, IMAGE_LOAD)
            .unwrap();

        assert_eq!(runtime.begin_retail_title_update(), Ok(None));
        assert_eq!(runtime.global_word(NEXT_DISPLAY_GLOBAL), Ok(IMAGE_ACTIVE));
        runtime.finish_retail_title_update().unwrap();
        assert_eq!(
            runtime.finish_deferred_display_frame(),
            Ok(Some(IMAGE_ACTIVE))
        );
        assert_eq!(
            runtime.retail_title_presentation(),
            Ok(Some(RetailTitlePresentation {
                screen: TitleScreen::PublisherFirst,
                next_screen: TitleScreen::PublisherFirst,
                phase: TitlePhase::FadingIn,
                opaque_swap_overlay: false,
                fade_counter: 256,
            }))
        );

        runtime.set_global_word(FADE_COUNTER_GLOBAL, 0).unwrap();
        assert_eq!(runtime.begin_retail_title_update(), Ok(None));
        runtime.finish_retail_title_update().unwrap();
        runtime.finish_deferred_display_frame().unwrap();
        assert_eq!(
            runtime.retail_title_presentation().unwrap().unwrap().phase,
            TitlePhase::Ready
        );

        // Authored GOOL writes global 18 before TitleUpdate. The same boundary
        // must seed global 106 to -256 before GLUpdate advances it to -224.
        runtime
            .set_global_word(TITLE_STATE_GLOBAL, TitleScreen::PublisherSecond.raw())
            .unwrap();
        assert_eq!(runtime.begin_retail_title_update(), Ok(None));
        runtime.finish_retail_title_update().unwrap();
        assert_eq!(
            runtime.global_word(FADE_COUNTER_GLOBAL),
            Ok((-256_i32) as u32)
        );
        runtime.finish_deferred_display_frame().unwrap();
        assert_eq!(
            runtime.retail_title_presentation(),
            Ok(Some(RetailTitlePresentation {
                screen: TitleScreen::PublisherFirst,
                next_screen: TitleScreen::PublisherSecond,
                phase: TitlePhase::FadingOut,
                opaque_swap_overlay: false,
                fade_counter: -224,
            }))
        );

        runtime.set_global_word(FADE_COUNTER_GLOBAL, 0).unwrap();
        assert_eq!(runtime.begin_retail_title_update(), Ok(None));
        assert_eq!(runtime.global_word(NEXT_DISPLAY_GLOBAL), Ok(IMAGE_LOAD));
        runtime.finish_retail_title_update().unwrap();
        assert_eq!(
            runtime.finish_deferred_display_frame(),
            Ok(Some(IMAGE_LOAD))
        );

        // The following source frame reaches TitleLoadState. A controller
        // spawned synchronously by that load is allowed to publish another
        // request before TitleUpdate performs its final compare.
        assert_eq!(
            runtime.begin_retail_title_update(),
            Ok(Some(RetailTitleAction::LoadScreen {
                previous: TitleScreen::PublisherFirst,
                screen: TitleScreen::PublisherSecond,
            }))
        );
        runtime
            .set_global_word(TITLE_STATE_GLOBAL, TitleScreen::NaughtyDog.raw())
            .unwrap();
        runtime.finish_retail_title_update().unwrap();
        assert_eq!(
            runtime.global_word(FADE_COUNTER_GLOBAL),
            Ok((-256_i32) as u32)
        );
        runtime.finish_deferred_display_frame().unwrap();
        assert_eq!(
            runtime.retail_title_presentation(),
            Ok(Some(RetailTitlePresentation {
                screen: TitleScreen::PublisherSecond,
                next_screen: TitleScreen::NaughtyDog,
                phase: TitlePhase::FadingOut,
                opaque_swap_overlay: true,
                fade_counter: -224,
            }))
        );

        // The direct TitleUpdate overlay is a source-frame latch, not a
        // transition-phase alias. It clears at the next TitleUpdate while the
        // newly requested fade continues normally.
        assert_eq!(runtime.begin_retail_title_update(), Ok(None));
        runtime.finish_retail_title_update().unwrap();
        runtime.finish_deferred_display_frame().unwrap();
        assert_eq!(
            runtime.retail_title_presentation(),
            Ok(Some(RetailTitlePresentation {
                screen: TitleScreen::PublisherSecond,
                next_screen: TitleScreen::NaughtyDog,
                phase: TitlePhase::FadingOut,
                opaque_swap_overlay: false,
                fade_counter: -192,
            }))
        );
    }

    #[test]
    fn title_fade_completion_retains_opaque_overlay_across_same_frame_retarget() {
        let mut runtime = RetailRuntime::new_for_level(256, LevelId::TITLE);
        runtime
            .configure_retail_title(TitleScreen::PublisherFirst, true)
            .unwrap();

        // Reach the ready source state, then begin an ordinary fade-out.
        assert_eq!(runtime.begin_retail_title_update(), Ok(None));
        runtime.finish_retail_title_update().unwrap();
        runtime.finish_deferred_display_frame().unwrap();
        runtime.set_global_word(FADE_COUNTER_GLOBAL, 0).unwrap();
        assert_eq!(runtime.begin_retail_title_update(), Ok(None));
        runtime.finish_retail_title_update().unwrap();
        runtime.finish_deferred_display_frame().unwrap();
        runtime
            .set_global_word(TITLE_STATE_GLOBAL, TitleScreen::PublisherSecond.raw())
            .unwrap();
        assert_eq!(runtime.begin_retail_title_update(), Ok(None));
        runtime.finish_retail_title_update().unwrap();
        runtime.finish_deferred_display_frame().unwrap();

        // Native draws opaque black when the fade reaches exact zero. GOOL
        // may have retargeted global 18 in that same frame; the final compare
        // starts another fade but cannot erase the already-submitted overlay.
        runtime.set_global_word(FADE_COUNTER_GLOBAL, 0).unwrap();
        assert_eq!(runtime.begin_retail_title_update(), Ok(None));
        runtime
            .set_global_word(TITLE_STATE_GLOBAL, TitleScreen::NaughtyDog.raw())
            .unwrap();
        runtime.finish_retail_title_update().unwrap();
        runtime.finish_deferred_display_frame().unwrap();
        assert_eq!(
            runtime.retail_title_presentation(),
            Ok(Some(RetailTitlePresentation {
                screen: TitleScreen::PublisherFirst,
                next_screen: TitleScreen::NaughtyDog,
                phase: TitlePhase::FadingOut,
                opaque_swap_overlay: true,
                fade_counter: -224,
            }))
        );

        assert_eq!(runtime.begin_retail_title_update(), Ok(None));
        runtime.finish_retail_title_update().unwrap();
        runtime.finish_deferred_display_frame().unwrap();
        assert_eq!(
            runtime.retail_title_presentation(),
            Ok(Some(RetailTitlePresentation {
                screen: TitleScreen::PublisherFirst,
                next_screen: TitleScreen::NaughtyDog,
                phase: TitlePhase::FadingOut,
                opaque_swap_overlay: false,
                fade_counter: -192,
            }))
        );
    }

    #[test]
    fn title_update_rejects_an_invalid_authored_state_word() {
        let mut runtime = RetailRuntime::new_for_level(256, LevelId::TITLE);
        runtime
            .configure_retail_title(TitleScreen::PublisherFirst, true)
            .unwrap();
        runtime
            .set_global_word(TITLE_STATE_GLOBAL, 0xffff_ffff)
            .unwrap();

        assert_eq!(
            runtime.finish_retail_title_update(),
            Err(RetailTitleError::InvalidTitleState(0xffff_ffff))
        );
    }

    #[test]
    fn stop_at_zone_propagates_typed_eid_to_arena_and_object_environment() {
        let level = LevelId::N_SANITY_BEACH;
        let mut runtime = RetailRuntime::new_for_level(256, level);
        runtime.set_level_state_context(RetailLevelStateContext {
            location: RetailCameraLocation {
                path: crust_formats::stream::RetailPathId {
                    zone: ZONE,
                    index: 0,
                },
                progress: crate::retail_frame::PathProgress::ZERO,
            },
            graphics_flags: 0,
            box_count: 0,
            checkpoint_id: -1,
            checkpoint_translation: [0; 3],
            first_spawn: false,
            active_neighbor_zones: vec![ZONE, ZONE_B],
        });
        let mut host = SolidZoneHost::default();
        let entities = [entity(1, 0, 3)];
        let neighbors = [NeighborZone {
            eid: ZONE,
            display_flags: 2,
            entities: &entities,
        }];
        let object = *runtime.spawn_current_zone_neighbors(&neighbors, &mut host)[0]
            .result
            .as_ref()
            .unwrap();
        let vm = runtime.machine.object_mut(object.vm).unwrap();
        vm.set_register(
            process_register::STATUS_B,
            crate::retail_physics::STATUS_B_TRANSLATION_MOTION
                | crate::retail_physics::STATUS_B_STOPPED_BY_SOLID,
        )
        .unwrap();
        vm.set_register(process_register::TRANSLATION_X, 150 * 0x100)
            .unwrap();
        vm.set_register(process_register::TRANSLATION_Y, 10_000)
            .unwrap();
        vm.set_register(process_register::TRANSLATION_Z, 50 * 0x100)
            .unwrap();
        vm.set_register(process_register::MISC_A_X, 1_024).unwrap();

        runtime.run_frame(&mut host, 8).unwrap();

        assert_eq!(runtime.arena.get(object.arena).unwrap().zone(), ZONE_B);
        assert_eq!(
            runtime
                .machine
                .object(object.vm)
                .unwrap()
                .retail_solid_zone_eid(),
            Some(ZONE_B)
        );
        assert_eq!(host.calls, [ZONE, ZONE, ZONE_B]);
    }

    struct SnapshotHost;

    impl ProgramHost for SnapshotHost {
        type Error = ();

        fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
            VmObject::new(binding.object.vm(), vec![RETURN]).map_err(|_| ())
        }

        fn bind_state_program(
            &mut self,
            _binding: StateProgramBinding,
        ) -> Result<VmStateProgram, Self::Error> {
            Err(())
        }
    }

    struct ArgumentProvenanceHost {
        argument_target: VmObjectHandle,
    }

    impl ProgramHost for ArgumentProvenanceHost {
        type Error = ();

        fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
            let code = match binding.executable {
                // Push the live target token without an explicit sidecar,
                // then create one executable-five child with argc one.
                42 => vec![
                    Instruction::encode(0x00, TEST_SCALAR_OPERAND_A, TEST_SCALAR_OPERAND_B),
                    0x8a10_5001,
                    RETURN,
                ],
                // Stay live for the bounded frame after the runtime installs
                // the creation argument in reused native process storage.
                5 => vec![
                    Instruction::encode(0x00, TEST_SCALAR_OPERAND_B, TEST_SCALAR_OPERAND_C,);
                    4
                ],
                _ => vec![RETURN],
            };
            let mut object = VmObject::new(binding.object.vm(), code).map_err(|_| ())?;
            if binding.executable == 42 {
                object
                    .set_register(
                        TEST_SCALAR_REGISTER_A,
                        CollisionObjectReference::new(self.argument_target).to_word(),
                    )
                    .map_err(|_| ())?;
                object
                    .set_register(TEST_SCALAR_REGISTER_B, 0)
                    .map_err(|_| ())?;
            }
            Ok(object)
        }

        fn bind_state_program(
            &mut self,
            _binding: StateProgramBinding,
        ) -> Result<VmStateProgram, Self::Error> {
            Err(())
        }
    }

    struct EffectBurstHost;

    impl ProgramHost for EffectBurstHost {
        type Error = ();

        fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
            let mut object = VmObject::new(
                binding.object.vm(),
                vec![misc(12, 9, TEST_SCALAR_OPERAND_A); 3],
            )
            .map_err(|_| ())?;
            object
                .set_register(
                    TEST_SCALAR_REGISTER_A,
                    (u32::from(binding.object.vm().get()) + 1) << 8,
                )
                .map_err(|_| ())?;
            Ok(object)
        }

        fn bind_state_program(
            &mut self,
            _binding: StateProgramBinding,
        ) -> Result<VmStateProgram, Self::Error> {
            Err(())
        }
    }

    #[test]
    fn full_retail_frame_drains_bounded_vm_effects_in_object_order() {
        let entities = [entity(0, 2, 0)];
        let neighbors = [NeighborZone {
            eid: ZONE,
            display_flags: ACTIVE_ZONE_DISPLAY_BIT,
            entities: &entities,
        }];
        let mut runtime = RetailRuntime::new(0);
        let mut host = EffectBurstHost;
        let attempts = runtime.spawn_current_zone_neighbors(&neighbors, &mut host);
        let mut parent = *attempts[0].result.as_ref().unwrap();
        for _ in 1..OBJECT_POOL_CAPACITY {
            let child = attach_test_child(&mut runtime, parent, ZONE, 2);
            let mut object =
                VmObject::new(child.vm, vec![misc(12, 9, TEST_SCALAR_OPERAND_A); 3]).unwrap();
            object
                .set_register(TEST_SCALAR_REGISTER_A, (u32::from(child.vm.get()) + 1) << 8)
                .unwrap();
            *runtime.machine.object_mut(child.vm).unwrap() = object;
            parent = child;
        }

        let frame = runtime.run_frame(&mut host, 3).unwrap();

        assert_eq!(frame.effects.len(), OBJECT_POOL_CAPACITY * 3);
        for (index, triplet) in frame.effects.chunks_exact(3).enumerate() {
            let expected = VmEffect::Transition(i32::try_from(index + 1).unwrap());
            assert!(triplet.iter().all(|effect| effect == &expected));
        }
    }

    #[test]
    fn materialize_rejects_an_occupied_pool_slot_without_leaking_a_vm_object() {
        let mut runtime = RetailRuntime::new(0);
        let entity = entity(279, 2, 0);
        let arena = runtime
            .arena
            .spawn_entity(ZONE, EntitySpawnDescriptor::from(&entity))
            .unwrap();
        let target = runtime.handles.reserve::<()>(arena).unwrap();
        let blocker = VmObjectHandle::new(95).unwrap();
        assert_ne!(target.vm, blocker);
        runtime
            .machine
            .insert_object(VmObject::new(blocker, vec![RETURN]).unwrap())
            .unwrap();
        runtime
            .machine
            .bind_retail_pool_slot(blocker, target.arena.slot())
            .unwrap();
        let before = runtime.machine.clone();

        let result = runtime.materialize(
            ProgramBinding {
                object: target,
                zone: ZONE,
                executable: entity.executable,
                subtype: entity.subtype,
                origin: ProgramOrigin::Entity(&entity),
            },
            &mut SnapshotHost,
        );

        assert_eq!(
            result,
            Err(RuntimeError::Vm(VmError::RetailPoolSlotOccupied {
                slot: target.arena.slot(),
                object: blocker,
            }))
        );
        assert_eq!(runtime.machine, before);
        assert_eq!(
            runtime.machine.object(target.vm),
            Err(VmError::UnknownObject(target.vm))
        );
    }

    #[test]
    fn vm_install_rejects_a_pool_slot_mismatch_without_replacing_the_live_object() {
        let mut machine = Machine::new(0);
        let handle = VmObjectHandle::new(0).unwrap();
        let mut original = VmObject::new(handle, vec![RETURN]).unwrap();
        original.set_register(TEST_SCALAR_REGISTER_A, 1).unwrap();
        machine.insert_object(original).unwrap();
        machine.bind_retail_pool_slot(handle, 3).unwrap();
        let before = machine.clone();

        let mut replacement = VmObject::new(handle, vec![RETURN]).unwrap();
        replacement.set_register(TEST_SCALAR_REGISTER_A, 2).unwrap();
        assert_eq!(
            RetailRuntime::install_vm_object::<()>(&mut machine, replacement, 4),
            Err(RuntimeError::Vm(VmError::RetailPoolSlotMismatch {
                object: handle,
                bound: Some(3),
                requested: 4,
            }))
        );
        assert_eq!(machine, before);
        assert_eq!(
            machine
                .object(handle)
                .unwrap()
                .register(TEST_SCALAR_REGISTER_A),
            Ok(1)
        );
    }

    struct SpinDeathHost {
        frame_available: bool,
        vertex_count: u32,
        source: ModelVertexSource,
        bound_bindings: Vec<AnimationBoundBinding>,
        vertex_bindings: Vec<ModelVertexBinding>,
    }

    impl SpinDeathHost {
        fn with_vertices(vertex_count: u32) -> Self {
            Self {
                frame_available: true,
                vertex_count,
                source: ModelVertexSource {
                    local_position: [100, 200, 300],
                    geometry_scale: [0x1000; 3],
                },
                bound_bindings: Vec::new(),
                vertex_bindings: Vec::new(),
            }
        }
    }

    impl ProgramHost for SpinDeathHost {
        type Error = ();

        fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
            VmObject::new(binding.object.vm(), vec![RETURN]).map_err(|_| ())
        }

        fn bind_state_program(
            &mut self,
            _binding: StateProgramBinding,
        ) -> Result<VmStateProgram, Self::Error> {
            Err(())
        }

        fn animation_bound_source(
            &mut self,
            binding: AnimationBoundBinding,
        ) -> Result<Option<AnimationBoundSource>, Self::Error> {
            self.bound_bindings.push(binding);
            Ok(self
                .frame_available
                .then_some(AnimationBoundSource::Vertex {
                    vertex_kind: ObjectVertexKind::Lit,
                    serialized_bound: Bounds3::default(),
                    collision_center: Vec3::default(),
                }))
        }

        fn model_vertex_source(
            &mut self,
            binding: ModelVertexBinding,
        ) -> Result<Option<ModelVertexSource>, Self::Error> {
            self.vertex_bindings.push(binding);
            Ok((binding.vertex_index < self.vertex_count).then_some(self.source))
        }
    }

    fn configure_spin_death_vertex_animation(
        runtime: &mut RetailRuntime,
        object: RuntimeObjectHandle,
        model_eid: Eid,
        frame_count: u8,
        frame_index: u32,
    ) {
        let mut animation = vec![1, 0, frame_count, 0];
        animation.extend_from_slice(&model_eid.raw().to_le_bytes());
        let reference = AnimationReference::from_word(0xa700_0000).unwrap();
        let object = runtime.machine.object_mut(object.vm).unwrap();
        object.bind_animation_data(&animation);
        object
            .set_register(process_register::ANIMATION_SEQUENCE, reference.to_word())
            .unwrap();
        object
            .set_register(process_register::ANIMATION_FRAME, frame_index << 8)
            .unwrap();
    }

    #[test]
    fn spin_death_camera_resolves_exact_live_vertex_scalars_and_timing() {
        const DISPLAY_IMAGES: u32 = 0x20000;

        let model_eid = Eid::from_name("model").unwrap();
        let mut runtime = RetailRuntime::new_for_level(
            SPIN_DEATH_CAMERA_FLIP_SPEED_GLOBAL + 1,
            LevelId::N_SANITY_BEACH,
        );
        let object = spawn_test_object(&mut runtime, ZONE, 260, 2, 0);
        configure_spin_death_vertex_animation(&mut runtime, object, model_eid, 2, 1);
        runtime
            .machine
            .object_mut(object.vm)
            .unwrap()
            .set_retail_transform(RetailTransform {
                translation: [10, 20, 30],
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            })
            .unwrap();
        runtime
            .set_global_word(
                SPIN_DEATH_CAMERA_OBJECT_GLOBAL,
                CollisionObjectReference::new(object.vm).to_word(),
            )
            .unwrap();
        runtime
            .set_global_word(SPIN_DEATH_CAMERA_VERTEX_GLOBAL, 2 << 8)
            .unwrap();
        runtime.set_spin_death_camera_count(-3).unwrap();
        runtime
            .set_global_word(SPIN_DEATH_CAMERA_ZOOM_SPEED_GLOBAL, (-1_000_i32) as u32)
            .unwrap();
        runtime
            .set_global_word(SPIN_DEATH_CAMERA_FLIP_SPEED_GLOBAL, 100)
            .unwrap();
        runtime.set_frame_timing(33, 34);
        runtime
            .set_global_word(CURRENT_DISPLAY_GLOBAL, DISPLAY_IMAGES)
            .unwrap();
        let mut host = SpinDeathHost::with_vertices(3);

        let without_acceleration = runtime.resolve_spin_death_camera_inputs(&mut host).unwrap();
        assert!(!without_acceleration.spin_accel);
        runtime
            .set_global_word(
                CURRENT_DISPLAY_GLOBAL,
                DISPLAY_IMAGES | GOOL_FLAG_SPIN_ACCEL,
            )
            .unwrap();
        let inputs = runtime.resolve_spin_death_camera_inputs(&mut host).unwrap();

        assert_eq!(
            inputs,
            SpinDeathCameraInputs {
                count: -3,
                // Retail's Q12 cosine is 4095, retaining the same three-stage
                // truncation as transform-vectors suboperation six.
                focus: Vec3 {
                    x: 106,
                    y: 212,
                    z: 318,
                },
                zoom_speed: -1_000,
                flip_speed: 100,
                spin_accel: true,
                ticks_per_frame: 34,
            }
        );
        assert_eq!(host.bound_bindings.len(), 2);
        assert_eq!(host.vertex_bindings.len(), 2);
        let binding = host.vertex_bindings[1];
        assert_eq!(binding.requester, object);
        assert_eq!(binding.link, object);
        assert_eq!(binding.model_eid, model_eid);
        assert_eq!(binding.frame_index, 1);
        assert_eq!(binding.vertex_index, 2);
        assert_eq!(
            runtime.global_word(SPIN_DEATH_CAMERA_COUNT_GLOBAL),
            Ok((-3_i32) as u32)
        );
    }

    #[test]
    fn spin_death_camera_rejects_null_invalid_and_stale_object_references() {
        let mut runtime = RetailRuntime::new_for_level(
            SPIN_DEATH_CAMERA_FLIP_SPEED_GLOBAL + 1,
            LevelId::N_SANITY_BEACH,
        );
        let mut host = SpinDeathHost::with_vertices(1);
        assert_eq!(
            runtime.resolve_spin_death_camera_inputs(&mut host),
            Err(SpinDeathCameraResolveError::NullObjectReference)
        );

        runtime
            .set_global_word(SPIN_DEATH_CAMERA_OBJECT_GLOBAL, 0x1234_5678)
            .unwrap();
        assert_eq!(
            runtime.resolve_spin_death_camera_inputs(&mut host),
            Err(SpinDeathCameraResolveError::InvalidObjectReference(
                0x1234_5678
            ))
        );

        let object = spawn_test_object(&mut runtime, ZONE, 261, 2, 0);
        let reference = CollisionObjectReference::new(object.vm);
        runtime
            .set_global_word(SPIN_DEATH_CAMERA_OBJECT_GLOBAL, reference.to_word())
            .unwrap();
        let mut report = ZoneTerminationReport::<()>::new();
        runtime
            .remove_runtime_subtree(object.arena, &mut report)
            .unwrap();
        assert_eq!(
            runtime.resolve_spin_death_camera_inputs(&mut host),
            Err(SpinDeathCameraResolveError::StaleObjectReference(reference))
        );
    }

    #[test]
    fn spin_death_camera_rejects_non_vertex_missing_frame_and_vertex_range() {
        let model_eid = Eid::from_name("model").unwrap();
        let sprite_page = Eid::from_name("pageT").unwrap();
        let mut runtime = RetailRuntime::new_for_level(
            SPIN_DEATH_CAMERA_FLIP_SPEED_GLOBAL + 1,
            LevelId::N_SANITY_BEACH,
        );
        let object = spawn_test_object(&mut runtime, ZONE, 262, 2, 0);
        runtime
            .set_global_word(
                SPIN_DEATH_CAMERA_OBJECT_GLOBAL,
                CollisionObjectReference::new(object.vm).to_word(),
            )
            .unwrap();
        let mut host = SpinDeathHost::with_vertices(1);

        assert_eq!(
            runtime.resolve_spin_death_camera_inputs(&mut host),
            Err(SpinDeathCameraResolveError::MissingAnimation(object))
        );

        let mut sprite = vec![2, 0, 0, 0];
        sprite.extend_from_slice(&sprite_page.raw().to_le_bytes());
        let sprite_reference = AnimationReference::from_word(0xa700_0000).unwrap();
        let vm_object = runtime.machine.object_mut(object.vm).unwrap();
        vm_object.bind_animation_data(&sprite);
        vm_object
            .set_register(
                process_register::ANIMATION_SEQUENCE,
                sprite_reference.to_word(),
            )
            .unwrap();
        assert_eq!(
            runtime.resolve_spin_death_camera_inputs(&mut host),
            Err(SpinDeathCameraResolveError::NonVertexAnimation(object))
        );

        configure_spin_death_vertex_animation(&mut runtime, object, model_eid, 1, 1);
        assert_eq!(
            runtime.resolve_spin_death_camera_inputs(&mut host),
            Err(SpinDeathCameraResolveError::MissingFrame {
                object,
                model_eid,
                frame_index: 1,
            })
        );

        configure_spin_death_vertex_animation(&mut runtime, object, model_eid, 2, 1);
        host.frame_available = false;
        assert_eq!(
            runtime.resolve_spin_death_camera_inputs(&mut host),
            Err(SpinDeathCameraResolveError::MissingFrame {
                object,
                model_eid,
                frame_index: 1,
            })
        );

        host.frame_available = true;
        runtime
            .set_global_word(SPIN_DEATH_CAMERA_VERTEX_GLOBAL, 1 << 8)
            .unwrap();
        assert_eq!(
            runtime.resolve_spin_death_camera_inputs(&mut host),
            Err(SpinDeathCameraResolveError::VertexIndexOutOfRange {
                object,
                model_eid,
                frame_index: 1,
                vertex_index: 1,
            })
        );

        runtime
            .set_global_word(SPIN_DEATH_CAMERA_VERTEX_GLOBAL, (-1_i32) as u32)
            .unwrap();
        assert_eq!(
            runtime.resolve_spin_death_camera_inputs(&mut host),
            Err(SpinDeathCameraResolveError::VertexIndexOutOfRange {
                object,
                model_eid,
                frame_index: 1,
                vertex_index: -1,
            })
        );
    }

    struct BoxHost {
        graphics_flags: u32,
    }

    impl ProgramHost for BoxHost {
        type Error = ();

        fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
            let mut object = VmObject::new(binding.object.vm(), vec![RETURN]).map_err(|_| ())?;
            object.configure_test_program_identity_with_type(0x200, BOX_OBJECT_TYPE);
            Ok(object)
        }

        fn bind_state_program(
            &mut self,
            _binding: StateProgramBinding,
        ) -> Result<VmStateProgram, Self::Error> {
            Err(())
        }

        fn zone_environment(
            &mut self,
            _zone: Eid,
        ) -> Result<Option<RetailZoneEnvironment>, Self::Error> {
            Ok(Some(RetailZoneEnvironment {
                origin: [0; 3],
                object_colors: [0; COLOR_COUNT],
                player_colors: [0; COLOR_COUNT],
                graphics_flags: self.graphics_flags,
            }))
        }
    }

    struct RejectProgramHost;

    impl ProgramHost for RejectProgramHost {
        type Error = ();

        fn bind_program(&mut self, _binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
            Err(())
        }

        fn bind_state_program(
            &mut self,
            _binding: StateProgramBinding,
        ) -> Result<VmStateProgram, Self::Error> {
            Err(())
        }
    }

    struct SzonHost {
        selected: Option<Eid>,
        queries: Vec<(Eid, [i32; 3])>,
    }

    struct InlineZoneHost {
        selected: Option<Eid>,
        zone_b_graphics: u32,
        zone_b_water_y: i32,
        quirks: SolidLevelQuirks,
        environment_calls: Vec<Eid>,
        state_bindings: Vec<Eid>,
    }

    impl InlineZoneHost {
        fn environment(&self, object_zone: Eid) -> RetailSolidEnvironment {
            let zone_a = RetailSolidZone::new([0; 3], [100; 3], 0, [0; 3], vec![0; 36])
                .unwrap()
                .with_eid(ZONE)
                .with_graphics(2, i32::MIN);
            let zone_b = RetailSolidZone::new([100, 0, 0], [100; 3], 0, [0; 3], vec![0; 36])
                .unwrap()
                .with_eid(ZONE_B)
                .with_graphics(self.zone_b_graphics, self.zone_b_water_y);
            RetailSolidEnvironment::new(0, [0; 24], [0; 24], vec![zone_a, zone_b])
                .with_runtime_context(Some(object_zone), self.quirks)
        }
    }

    #[derive(Default)]
    struct SolidZoneHost {
        calls: Vec<Eid>,
    }

    impl SolidZoneHost {
        fn environment(object_zone: Eid) -> RetailSolidEnvironment {
            let zone = |eid, origin| {
                RetailSolidZone::new(origin, [100; 3], 0, [0; 3], vec![0; 36])
                    .unwrap()
                    .with_eid(eid)
            };
            RetailSolidEnvironment::new(
                0,
                [u16::try_from(object_zone.raw() & 0xffff).unwrap(); 24],
                [u16::try_from(object_zone.raw() & 0xffff).unwrap(); 24],
                vec![zone(ZONE, [0, 0, 0]), zone(ZONE_B, [100, 0, 0])],
            )
            .with_runtime_context(Some(object_zone), SolidLevelQuirks::default())
        }
    }

    impl ProgramHost for SolidZoneHost {
        type Error = ();

        fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
            VmObject::new(binding.object.vm(), vec![RETURN]).map_err(|_| ())
        }

        fn bind_state_program(
            &mut self,
            _binding: StateProgramBinding,
        ) -> Result<VmStateProgram, Self::Error> {
            Err(())
        }

        fn solid_environment(
            &mut self,
            zone: Eid,
        ) -> Result<Option<RetailSolidEnvironment>, Self::Error> {
            self.calls.push(zone);
            Ok(Some(Self::environment(zone)))
        }
    }

    impl ProgramHost for SzonHost {
        type Error = ();

        fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
            VmObject::new(binding.object.vm(), vec![RETURN]).map_err(|_| ())
        }

        fn bind_state_program(
            &mut self,
            _binding: StateProgramBinding,
        ) -> Result<VmStateProgram, Self::Error> {
            Err(())
        }

        fn find_neighbor_zone(
            &mut self,
            current_zone: Eid,
            point: [i32; 3],
        ) -> Result<Option<Eid>, Self::Error> {
            self.queries.push((current_zone, point));
            Ok(self.selected)
        }
    }

    impl ProgramHost for InlineZoneHost {
        type Error = ();

        fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
            VmObject::new(binding.object.vm(), vec![RETURN]).map_err(|_| ())
        }

        fn bind_state_program(
            &mut self,
            binding: StateProgramBinding,
        ) -> Result<VmStateProgram, Self::Error> {
            self.state_bindings.push(binding.zone);
            Ok(event_transition_state())
        }

        fn solid_environment(
            &mut self,
            zone: Eid,
        ) -> Result<Option<RetailSolidEnvironment>, Self::Error> {
            self.environment_calls.push(zone);
            Ok(Some(self.environment(zone)))
        }

        fn find_neighbor_zone(
            &mut self,
            _current_zone: Eid,
            _point: [i32; 3],
        ) -> Result<Option<Eid>, Self::Error> {
            Ok(self.selected)
        }
    }

    struct CardLoadHost {
        loaded: SaveData,
        requests: Vec<(CardHostRequest, SaveData)>,
    }

    impl ProgramHost for CardLoadHost {
        type Error = ();

        fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
            let mut object = VmObject::new(
                binding.object.vm(),
                vec![misc(15, 4, TEST_SCALAR_OPERAND_A), RETURN],
            )
            .map_err(|_| ())?;
            object
                .set_register(TEST_SCALAR_REGISTER_A, 2)
                .map_err(|_| ())?;
            Ok(object)
        }

        fn bind_state_program(
            &mut self,
            _binding: StateProgramBinding,
        ) -> Result<VmStateProgram, Self::Error> {
            Err(())
        }

        fn handle_card_request(
            &mut self,
            request: CardHostRequest,
            current: SaveData,
        ) -> Result<CardHostResponse, Self::Error> {
            self.requests.push((request, current));
            Ok(CardHostResponse {
                result: 0,
                loaded: Some(self.loaded),
                published: CardPublishedState {
                    flags: crate::card::CardFlags::NEW_DEVICE,
                    part_count: 1,
                    partinfos: std::array::from_fn(
                        |index| {
                            if index == 0 { 0x1234_0009 } else { 0 }
                        },
                    ),
                },
            })
        }
    }

    #[test]
    fn card_load_is_synchronous_resets_globals_and_publishes_metadata() {
        let loaded = SaveData {
            level_count: 12,
            initial_lives: 7 << 8,
            unknown_6190c: 0x1122_3344,
            mono: true,
            sfx_volume: 87,
            music_volume: 65,
            item_pool_1: 0xa1,
            item_pool_2: 0xb2,
            gem_count: 9,
            key_count: 2,
        };
        let mut host = CardLoadHost {
            loaded,
            requests: Vec::new(),
        };
        let mut runtime = RetailRuntime::new_for_level(119, LevelId::new_const(0x03));
        let entities = [entity(5, 1, 0)];
        let neighbors = [NeighborZone {
            eid: ZONE,
            display_flags: 2,
            entities: &entities,
        }];
        let object = runtime.spawn_current_zone_neighbors(&neighbors, &mut host)[0]
            .result
            .as_ref()
            .copied()
            .unwrap();
        runtime.arena.spawn_table_mut().set_flags(42, 0xab).unwrap();
        runtime.machine.set_spawn_flags(42, 0xab).unwrap();
        runtime.machine.set_retail_level_spawn_tag(0, 0x1234);
        runtime.saved_level_state = Some(level_snapshot(LevelId::new_const(0x03)));
        let saved = runtime.saved_level_state.clone();
        runtime.transition_zone_context = ObjectZoneContext::Target(ZONE_B);
        let mut context = level_context(ZONE, false, vec![ZONE]);
        context.box_count = 0x500;
        context.checkpoint_id = 6 << 8;
        context.checkpoint_translation = [1, 2, 3];
        runtime.set_level_state_context(context);

        let frame = runtime.run_frame(&mut host, 4).unwrap();
        assert!(frame.executions[0].result.is_ok());
        assert_eq!(host.requests.len(), 1);
        assert_eq!(host.requests[0].0.operation, 4);
        assert_eq!(host.requests[0].0.part_index, 2);
        assert_eq!(
            runtime
                .machine
                .object(object.vm)
                .unwrap()
                .register(process_register::MISC_VALUE),
            Ok(0)
        );
        assert_eq!(runtime.card_save_data(), Ok(loaded));
        assert_eq!(runtime.global_word(24), Ok(loaded.initial_lives));
        assert_eq!(runtime.global_word(47), Ok(loaded.level_count));
        assert_eq!(runtime.global_word(20), Ok(loaded.level_count));
        assert_eq!(runtime.global_word(59), Ok(0x10));
        assert_eq!(runtime.global_word(61), Ok(1));
        assert_eq!(runtime.global_word(82), Ok(0x1234_0009));
        assert_eq!(runtime.arena.spawn_table().flags(42), Some(0xab));
        assert_eq!(runtime.machine.spawn_flags(42), Ok(0xab));
        assert_eq!(runtime.saved_level_state, saved);
        assert!(
            runtime
                .machine
                .retail_level_spawn_tags()
                .iter()
                .all(|tag| *tag == 0)
        );

        assert_eq!(runtime.take_card_load(), Some(loaded));
        assert_eq!(runtime.take_card_load(), None);
        let context = runtime.level_state_context().unwrap();
        assert_eq!(context.box_count, 0x500);
        assert_eq!(context.checkpoint_id, -1);
        assert_eq!(context.checkpoint_translation, [1, 2, 3]);
        assert!(!context.first_spawn);
        assert_eq!(
            runtime.transition_zone_context,
            ObjectZoneContext::Target(ZONE_B)
        );
        assert_eq!(runtime.saved_level_state, saved);
    }

    #[test]
    fn misc_level_reset_runs_inside_interpreter_and_preserves_runtime_state() {
        let level = LevelId::new_const(0x03);
        let mut runtime = RetailRuntime::new_for_level(119, level);
        let main = spawn_test_object(&mut runtime, ZONE, 5, 0, 0);
        runtime
            .machine
            .upsert_object(
                VmObject::new(
                    main.vm,
                    vec![
                        misc(12, 11, 0x0be0),
                        Instruction::encode(0x11, 0x0805, 0x0e08),
                        RETURN,
                    ],
                )
                .unwrap(),
            )
            .unwrap();
        runtime.saved_level_state = Some(level_snapshot(level));
        let saved = runtime.saved_level_state.clone();
        let mut context = level_context(ZONE, false, vec![ZONE]);
        context.checkpoint_id = 7 << 8;
        runtime.set_level_state_context(context);
        runtime.arena.spawn_table_mut().set_flags(42, 0xab).unwrap();
        runtime.machine.set_spawn_flags(42, 0xab).unwrap();
        runtime.machine.set_retail_level_spawn_tag(0, 0x1234);
        runtime
            .set_global_word(crate::gool::INITIAL_LIFE_COUNT_GLOBAL, 7 << 8)
            .unwrap();
        runtime.set_global_word(GAME_STATE_GLOBAL, 0x600).unwrap();
        runtime
            .set_global_word(RESPAWN_COUNT_GLOBAL, 0x300)
            .unwrap();
        runtime.set_global_word(DEATH_COUNT_GLOBAL, 0x400).unwrap();

        let frame = runtime.run_frame(&mut SnapshotHost, 8).unwrap();

        assert_eq!(
            frame.effects,
            [VmEffect::ResetLevelGlobals { object: main.vm }]
        );
        assert_eq!(
            runtime.machine.object(main.vm).unwrap().register(8),
            Ok(0x500),
            "the following GOOL instruction executes after the synchronous reset"
        );
        assert_eq!(runtime.global_word(RESPAWN_COUNT_GLOBAL), Ok(0));
        assert_eq!(runtime.global_word(DEATH_COUNT_GLOBAL), Ok(0));
        assert_eq!(
            runtime.global_word(crate::gool::LIFE_COUNT_GLOBAL),
            Ok(7 << 8)
        );
        assert_eq!(runtime.global_word(GAME_STATE_GLOBAL), Ok(0x600));
        assert_eq!(runtime.arena.spawn_table().flags(42), Some(0xab));
        assert_eq!(runtime.machine.spawn_flags(42), Ok(0xab));
        assert_eq!(runtime.saved_level_state, saved);
        assert!(
            runtime
                .machine
                .retail_level_spawn_tags()
                .iter()
                .all(|tag| *tag == 0)
        );
        assert_eq!(runtime.level_state_context().unwrap().checkpoint_id, -1);
        assert_eq!(runtime.respawn_count, 0);
        assert_eq!(runtime.death_count, 0);
    }

    #[test]
    fn reset_then_save_in_one_handler_uses_the_reset_checkpoint_word() {
        let level = LevelId::new_const(0x03);
        let mut runtime = RetailRuntime::new_for_level(119, level);
        let main = spawn_test_object(&mut runtime, ZONE, 5, 0, 0);
        let mut player = VmObject::new(
            main.vm,
            vec![misc(12, 11, 0x0be0), misc(12, 0, 0x0be0), RETURN],
        )
        .unwrap();
        for (register, value) in [
            (process_register::TRANSLATION_X, 111),
            (process_register::TRANSLATION_Y, 222),
            (process_register::TRANSLATION_Z, 333),
            (process_register::SCALE_X, 0x1000),
            (process_register::SCALE_Y, 0x1000),
            (process_register::SCALE_Z, 0x1000),
        ] {
            player.set_register(register, value).unwrap();
        }
        runtime.machine.upsert_object(player).unwrap();
        let mut context = level_context(ZONE, false, vec![ZONE]);
        context.checkpoint_id = 7 << 8;
        context.checkpoint_translation = [700, 701, 702];
        runtime.set_level_state_context(context);
        runtime
            .set_global_word(CHECKPOINT_ID_GLOBAL, 7 << 8)
            .unwrap();

        let frame = runtime.run_frame(&mut SnapshotHost, 8).unwrap();

        assert_eq!(
            frame.effects,
            [
                VmEffect::ResetLevelGlobals { object: main.vm },
                VmEffect::SaveState(main.vm),
            ]
        );
        assert_eq!(
            runtime.saved_level_state().unwrap().player_translation,
            [111, 222, 333],
            "the reset -1 checkpoint wins over the stale host mirror"
        );
        assert_eq!(runtime.global_word(CHECKPOINT_ID_GLOBAL), Ok(u32::MAX));
    }

    #[test]
    fn checkpoint_writes_then_save_in_one_handler_use_live_global_translation() {
        let level = LevelId::new_const(0x03);
        let mut runtime = RetailRuntime::new_for_level(119, level);
        let main = spawn_test_object(&mut runtime, ZONE, 5, 0, 0);
        let mut player = VmObject::new(
            main.vm,
            vec![
                Instruction::encode(0x20, 0x0807, 0x0845),
                Instruction::encode(0x20, 0x0804, 0x0866),
                Instruction::encode(0x20, 0x0805, 0x0867),
                Instruction::encode(0x20, 0x0806, 0x0868),
                misc(12, 0, 0x0be0),
                RETURN,
            ],
        )
        .unwrap();
        for (register, value) in [
            (process_register::TRANSLATION_X, 111),
            (process_register::TRANSLATION_Y, 222),
            (process_register::TRANSLATION_Z, 333),
            (process_register::SCALE_X, 0x1000),
            (process_register::SCALE_Y, 0x1000),
            (process_register::SCALE_Z, 0x1000),
        ] {
            player.set_register(register, value).unwrap();
        }
        runtime.machine.upsert_object(player).unwrap();
        let mut context = level_context(ZONE, false, vec![ZONE]);
        context.checkpoint_id = -1;
        context.checkpoint_translation = [700, 701, 702];
        runtime.set_level_state_context(context);

        let frame = runtime.run_frame(&mut SnapshotHost, 8).unwrap();

        assert_eq!(frame.effects, [VmEffect::SaveState(main.vm)]);
        assert_eq!(
            runtime.saved_level_state().unwrap().player_translation,
            [4 << 8, 5 << 8, 6 << 8],
            "the same-handler checkpoint globals win over player and stale host positions"
        );
        assert_eq!(runtime.global_word(CHECKPOINT_ID_GLOBAL), Ok(7 << 8));
    }

    #[test]
    fn protected_title_resume_reapplies_payload_without_destroying_level_state() {
        let level = LevelId::new_const(0x03);
        let mut runtime = RetailRuntime::new_for_level(119, level);
        runtime.saved_level_state = Some(level_snapshot(level));
        let saved = runtime.saved_level_state.clone();
        let mut context = level_context(ZONE, false, vec![ZONE]);
        context.checkpoint_id = 6 << 8;
        runtime.set_level_state_context(context);
        runtime.arena.spawn_table_mut().set_flags(42, 0xab).unwrap();
        runtime.machine.set_spawn_flags(42, 0xab).unwrap();
        runtime.machine.set_retail_level_spawn_tag(0, 0x1234);

        runtime.reset_level_globals().unwrap();
        assert_eq!(runtime.arena.spawn_table().flags(42), Some(0xab));
        assert_eq!(runtime.machine.spawn_flags(42), Ok(0xab));
        assert_eq!(runtime.saved_level_state, saved);
        assert!(
            runtime
                .machine
                .retail_level_spawn_tags()
                .iter()
                .all(|tag| *tag == 0)
        );

        // Prove the protected payload path performs its own second native
        // reset before restoring only progression/options fields.
        runtime.machine.set_retail_level_spawn_tag(0, 0x4321);
        let resume = SaveData {
            level_count: 12,
            initial_lives: 7 << 8,
            unknown_6190c: 0x1122_3344,
            mono: true,
            sfx_volume: 87,
            music_volume: 65,
            item_pool_1: 0xa1,
            item_pool_2: 0xb2,
            gem_count: 9,
            key_count: 2,
        };
        runtime.restore_resume_after_title_reset(resume).unwrap();

        assert_eq!(runtime.card_save_data(), Ok(resume));
        assert_eq!(
            runtime.global_word(crate::gool::LEVELS_UNLOCKED_GLOBAL),
            Ok(12)
        );
        assert_eq!(
            runtime.global_word(crate::gool::CURRENT_MAP_LEVEL_GLOBAL),
            Ok(12)
        );
        assert_eq!(runtime.arena.spawn_table().flags(42), Some(0xab));
        assert_eq!(runtime.machine.spawn_flags(42), Ok(0xab));
        assert_eq!(runtime.saved_level_state, saved);
        assert!(
            runtime
                .machine
                .retail_level_spawn_tags()
                .iter()
                .all(|tag| *tag == 0)
        );
        assert_eq!(runtime.level_state_context().unwrap().checkpoint_id, -1);
    }

    #[test]
    fn direct_card_restore_preserves_live_level_and_savestate() {
        let level = LevelId::new_const(0x03);
        let mut runtime = RetailRuntime::new_for_level(119, level);
        runtime.saved_level_state = Some(level_snapshot(level));
        let saved = runtime.saved_level_state.clone();
        runtime.arena.spawn_table_mut().set_flags(42, 0xab).unwrap();
        runtime.machine.set_spawn_flags(42, 0xab).unwrap();
        runtime.machine.set_retail_level_spawn_tag(0, 0x1234);
        runtime.transition_zone_context = ObjectZoneContext::Target(ZONE_B);
        let mut context = level_context(ZONE, true, vec![ZONE]);
        context.box_count = 0x500;
        context.checkpoint_id = 7 << 8;
        context.checkpoint_translation = [700, 701, 702];
        runtime.set_level_state_context(context);
        let loaded = SaveData {
            level_count: 14,
            initial_lives: 9 << 8,
            unknown_6190c: 0x1234_5678,
            mono: false,
            sfx_volume: 123,
            music_volume: 45,
            item_pool_1: 0xaa,
            item_pool_2: 0xbb,
            gem_count: 11,
            key_count: 2,
        };

        runtime.restore_card_save_data(loaded).unwrap();

        assert_eq!(runtime.card_save_data(), Ok(loaded));
        assert_eq!(runtime.arena.spawn_table().flags(42), Some(0xab));
        assert_eq!(runtime.machine.spawn_flags(42), Ok(0xab));
        assert_eq!(runtime.saved_level_state, saved);
        assert_eq!(
            runtime.transition_zone_context,
            ObjectZoneContext::Target(ZONE_B)
        );
        let context = runtime.level_state_context().unwrap();
        assert_eq!(context.box_count, 0x500);
        assert_eq!(context.checkpoint_id, -1);
        assert_eq!(context.checkpoint_translation, [700, 701, 702]);
        assert!(context.first_spawn);
        assert!(
            runtime
                .machine
                .retail_level_spawn_tags()
                .iter()
                .all(|tag| *tag == 0)
        );
    }

    #[derive(Default)]
    struct ReclaimHost {
        freed_audio: Vec<RuntimeObjectHandle>,
    }

    impl ProgramHost for ReclaimHost {
        type Error = ();

        fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
            let code = match binding.executable {
                // Establish one descendant before the ordinary pool fills.
                1 => vec![0x8a00_5001, RETURN],
                // Native reclaiming child creation with zero arguments.
                12 => vec![0x9100_5001, RETURN],
                _ => vec![RETURN],
            };
            VmObject::new(binding.object.vm(), code).map_err(|_| ())
        }

        fn bind_state_program(
            &mut self,
            _binding: StateProgramBinding,
        ) -> Result<VmStateProgram, Self::Error> {
            Err(())
        }

        fn free_object_audio(&mut self, object: RuntimeObjectHandle) -> bool {
            self.freed_audio.push(object);
            true
        }
    }

    struct NeighborTerminationHost {
        neighbors: Vec<Eid>,
        neighbor_queries: Vec<Eid>,
        freed_audio: Vec<RuntimeObjectHandle>,
    }

    impl NeighborTerminationHost {
        fn new(neighbors: Vec<Eid>) -> Self {
            Self {
                neighbors,
                neighbor_queries: Vec::new(),
                freed_audio: Vec::new(),
            }
        }
    }

    impl ProgramHost for NeighborTerminationHost {
        type Error = ();

        fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
            VmObject::new(binding.object.vm(), vec![RETURN]).map_err(|_| ())
        }

        fn bind_state_program(
            &mut self,
            _binding: StateProgramBinding,
        ) -> Result<VmStateProgram, Self::Error> {
            Err(())
        }

        fn current_zone_neighbors(&mut self, current_zone: Eid) -> Result<Vec<Eid>, Self::Error> {
            self.neighbor_queries.push(current_zone);
            Ok(self.neighbors.clone())
        }

        fn free_object_audio(&mut self, object: RuntimeObjectHandle) -> bool {
            self.freed_audio.push(object);
            true
        }
    }

    struct AudioRecordingHost {
        state_program: Option<VmStateProgram>,
        requests: Vec<AudioHostRequest>,
        next_voice_id: i32,
    }

    impl AudioRecordingHost {
        fn new(state_program: Option<VmStateProgram>) -> Self {
            Self {
                state_program,
                requests: Vec::new(),
                next_voice_id: 40,
            }
        }
    }

    impl ProgramHost for AudioRecordingHost {
        type Error = ();

        fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
            VmObject::new(binding.object.vm(), vec![RETURN]).map_err(|_| ())
        }

        fn bind_state_program(
            &mut self,
            _binding: StateProgramBinding,
        ) -> Result<VmStateProgram, Self::Error> {
            self.state_program.clone().ok_or(())
        }

        fn handle_audio_request(
            &mut self,
            request: AudioHostRequest,
        ) -> Result<AudioHostResponse, Self::Error> {
            self.requests.push(request);
            Ok(match request {
                AudioHostRequest::CreateVoice(_) => {
                    let voice_id = self.next_voice_id;
                    self.next_voice_id = self.next_voice_id.wrapping_add(1);
                    AudioHostResponse::VoiceCreated { voice_id }
                }
                AudioHostRequest::Control(_) => AudioHostResponse::ControlApplied,
            })
        }
    }

    const fn audio_create() -> u32 {
        Instruction::encode(0x8c, TEST_SCALAR_OPERAND_A, TEST_SCALAR_OPERAND_B)
    }

    fn prepare_audio_registers(runtime: &mut RetailRuntime, object: RuntimeObjectHandle) {
        let vm = runtime.machine.object_mut(object.vm).unwrap();
        vm.set_register(TEST_SCALAR_REGISTER_A, 0x3fff).unwrap();
        vm.set_register(TEST_SCALAR_REGISTER_B, Eid::from_raw(0x1234_5679).raw())
            .unwrap();
    }

    struct BoundHost {
        calls: Vec<AnimationBoundBinding>,
        source: AnimationBoundSource,
    }

    impl BoundHost {
        fn new(source: AnimationBoundSource) -> Self {
            Self {
                calls: Vec::new(),
                source,
            }
        }
    }

    impl ProgramHost for BoundHost {
        type Error = ();

        fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
            VmObject::new(binding.object.vm(), vec![RETURN]).map_err(|_| ())
        }

        fn bind_state_program(
            &mut self,
            _binding: StateProgramBinding,
        ) -> Result<VmStateProgram, Self::Error> {
            Err(())
        }

        fn animation_bound_source(
            &mut self,
            binding: AnimationBoundBinding,
        ) -> Result<Option<AnimationBoundSource>, Self::Error> {
            self.calls.push(binding);
            Ok(Some(self.source))
        }
    }

    struct DisplayShaderHost {
        source: AnimationBoundSource,
        solid: RetailSolidEnvironment,
        zone: RetailZoneEnvironment,
    }

    impl ProgramHost for DisplayShaderHost {
        type Error = ();

        fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
            let mut object = VmObject::new(binding.object.vm(), vec![RETURN]).map_err(|_| ())?;
            object.configure_test_program_identity_with_type(0x100, 0);
            Ok(object)
        }

        fn bind_state_program(
            &mut self,
            _binding: StateProgramBinding,
        ) -> Result<VmStateProgram, Self::Error> {
            Err(())
        }

        fn zone_environment(
            &mut self,
            zone: Eid,
        ) -> Result<Option<RetailZoneEnvironment>, Self::Error> {
            if zone != ZONE {
                return Err(());
            }
            Ok(Some(self.zone))
        }

        fn solid_environment(
            &mut self,
            _zone: Eid,
        ) -> Result<Option<RetailSolidEnvironment>, Self::Error> {
            Ok(Some(self.solid.clone()))
        }

        fn animation_bound_source(
            &mut self,
            _binding: AnimationBoundBinding,
        ) -> Result<Option<AnimationBoundSource>, Self::Error> {
            Ok(Some(self.source))
        }

        fn animation_display_vertex_kind(
            &mut self,
            _binding: AnimationBoundBinding,
        ) -> Result<Option<ObjectVertexKind>, Self::Error> {
            Ok(match self.source {
                AnimationBoundSource::Vertex { vertex_kind, .. } => Some(vertex_kind),
                AnimationBoundSource::NonVertex => None,
            })
        }
    }

    fn entity(id: u16, executable: u8, subtype: u8) -> ZoneEntity {
        ZoneEntity {
            serialized_parent: EntryRef::from_raw(0),
            spawn_flags: 0,
            group: 3,
            id,
            initializer: [0; 3],
            executable,
            subtype,
            path_points: vec![ZoneEntityPathPoint { x: 0, y: 0, z: 0 }],
        }
    }

    fn box_entity(id: u16, point: ZoneEntityPathPoint) -> ZoneEntity {
        let mut entity = entity(id, BOX_EXECUTABLE, 0);
        entity.path_points[0] = point;
        entity
    }

    #[test]
    fn box_adjacency_uses_native_strict_tolerance_and_vertical_spacing() {
        let previous = ZoneEntityPathPoint { x: 0, y: 0, z: 0 };

        assert!(retail_box_points_are_adjacent(
            ZoneEntityPathPoint {
                x: 9,
                y: 109,
                z: -9,
            },
            previous,
        ));
        for current in [
            ZoneEntityPathPoint {
                x: 10,
                y: 100,
                z: 0,
            },
            ZoneEntityPathPoint {
                x: 0,
                y: 100,
                z: -10,
            },
            ZoneEntityPathPoint { x: 0, y: 110, z: 0 },
            ZoneEntityPathPoint { x: 0, y: 90, z: 0 },
        ] {
            assert!(!retail_box_points_are_adjacent(current, previous));
        }
    }

    #[test]
    fn adjacent_box_entities_publish_bidirectional_checked_links() {
        let entities = [
            box_entity(
                23,
                ZoneEntityPathPoint {
                    x: 431,
                    y: 942,
                    z: -1550,
                },
            ),
            box_entity(
                24,
                ZoneEntityPathPoint {
                    x: 431,
                    y: 1042,
                    z: -1550,
                },
            ),
        ];
        let neighbors = [NeighborZone {
            eid: ZONE,
            display_flags: ACTIVE_ZONE_DISPLAY_BIT,
            entities: &entities,
        }];
        let mut runtime = RetailRuntime::new_for_level(119, LevelId::N_SANITY_BEACH);
        let attempts = runtime.spawn_current_zone_neighbors(
            &neighbors,
            &mut BoxHost {
                graphics_flags: BOX_NO_STAGGER_GRAPHICS_FLAG,
            },
        );
        let first = *attempts[0].result.as_ref().unwrap();
        let second = *attempts[1].result.as_ref().unwrap();
        let first_vm = runtime.machine.object(first.vm).unwrap();
        let second_vm = runtime.machine.object(second.vm).unwrap();

        assert_eq!(
            CollisionObjectReference::from_word(
                first_vm.register(process_register::MISC_A_Y).unwrap()
            )
            .map(CollisionObjectReference::object),
            Some(second.vm),
        );
        assert_eq!(
            CollisionObjectReference::from_word(
                second_vm.register(process_register::MISC_A_X).unwrap()
            )
            .map(CollisionObjectReference::object),
            Some(first.vm),
        );
        assert_eq!(second_vm.register(process_register::MISC_A_Y), Ok(0),);
        assert_eq!(
            runtime.global_word(PREVIOUS_BOX_GLOBAL),
            Ok(CollisionObjectReference::new(second.vm).to_word()),
        );
        assert_eq!(
            runtime.global_word(BOXES_Y_GLOBAL),
            Ok(BOX_STACK_SPACING as u32)
        );
        assert_eq!(runtime.global_word(PREVIOUS_BOX_ENTITY_GLOBAL), Ok(0));
    }

    #[test]
    fn blocked_lower_box_compacts_the_next_adjacent_box_by_one_spacing() {
        let entities = [
            box_entity(30, ZoneEntityPathPoint { x: 0, y: 0, z: 0 }),
            box_entity(31, ZoneEntityPathPoint { x: 0, y: 100, z: 0 }),
        ];
        let neighbors = [NeighborZone {
            eid: ZONE,
            display_flags: ACTIVE_ZONE_DISPLAY_BIT,
            entities: &entities,
        }];
        let mut runtime = RetailRuntime::new_for_level(119, LevelId::N_SANITY_BEACH);
        runtime
            .arena
            .spawn_table_mut()
            .set_flags(30, SPAWN_CHECKPOINT_BLOCKED_BIT)
            .unwrap();

        let attempts = runtime.spawn_current_zone_neighbors(
            &neighbors,
            &mut BoxHost {
                graphics_flags: BOX_NO_STAGGER_GRAPHICS_FLAG,
            },
        );
        assert!(matches!(
            attempts[0].result,
            Err(RuntimeError::Spawn(SpawnError::SpawnBlocked {
                id: 30,
                flags: SPAWN_CHECKPOINT_BLOCKED_BIT,
            }))
        ));
        let upper = *attempts[1].result.as_ref().unwrap();
        assert_eq!(
            runtime
                .machine
                .object(upper.vm)
                .unwrap()
                .register(process_register::TRANSLATION_Y),
            Ok(0),
        );
        assert_eq!(
            runtime.global_word(BOXES_Y_GLOBAL),
            Ok(BOX_STACK_SPACING.wrapping_mul(2) as u32),
        );
    }

    #[test]
    fn crate_stagger_formula_retains_all_three_low_bits() {
        for expected in 0..=7 {
            assert_eq!(retail_box_stagger_count([expected, 0, 0]), expected as u32);
        }
        assert_eq!(retail_box_stagger_count([-1, 0, -1]), 0);
    }

    #[test]
    fn box_state_reset_and_removal_clear_only_transient_checked_links() {
        let entities = [
            box_entity(40, ZoneEntityPathPoint { x: 0, y: 0, z: 0 }),
            box_entity(41, ZoneEntityPathPoint { x: 0, y: 100, z: 0 }),
        ];
        let neighbors = [NeighborZone {
            eid: ZONE,
            display_flags: ACTIVE_ZONE_DISPLAY_BIT,
            entities: &entities,
        }];
        let mut runtime = RetailRuntime::new_for_level(119, LevelId::N_SANITY_BEACH);
        let attempts = runtime.spawn_current_zone_neighbors(
            &neighbors,
            &mut BoxHost {
                graphics_flags: BOX_NO_STAGGER_GRAPHICS_FLAG,
            },
        );
        let first = *attempts[0].result.as_ref().unwrap();
        let second = *attempts[1].result.as_ref().unwrap();

        let mut report = ZoneTerminationReport::<()>::new();
        runtime
            .remove_runtime_subtree(second.arena, &mut report)
            .unwrap();
        assert_eq!(
            runtime
                .machine
                .object(first.vm)
                .unwrap()
                .register(process_register::MISC_A_Y),
            Ok(0),
        );
        assert_eq!(runtime.global_word(PREVIOUS_BOX_GLOBAL), Ok(0));

        runtime.reset_retail_box_spawn_state();
        assert_eq!(runtime.box_spawn, RetailBoxSpawnState::default());
        assert_eq!(runtime.global_word(PREVIOUS_BOX_GLOBAL), Ok(0));
        assert_eq!(
            runtime.global_word(BOXES_Y_GLOBAL),
            Ok(BOX_STACK_SPACING as u32)
        );
        assert_eq!(runtime.global_word(PREVIOUS_BOX_ENTITY_GLOBAL), Ok(0));
    }

    fn spawn_test_object(
        runtime: &mut RetailRuntime,
        zone: Eid,
        id: u16,
        executable: u8,
        subtype: u8,
    ) -> RuntimeObjectHandle {
        let entities = [entity(id, executable, subtype)];
        let neighbors = [NeighborZone {
            eid: zone,
            display_flags: 2,
            entities: &entities,
        }];
        *runtime.spawn_current_zone_neighbors(&neighbors, &mut SnapshotHost)[0]
            .result
            .as_ref()
            .unwrap()
    }

    #[test]
    fn player_link_keeps_dedicated_allocation_while_main_is_inactive() {
        let mut runtime = RetailRuntime::new_for_level(0, LevelId::TITLE);
        let ordinary = spawn_test_object(&mut runtime, ZONE, 270, 2, 0);
        let inactive_link = runtime.machine.object(ordinary.vm).unwrap();
        let inactive_word = inactive_link.register(5).unwrap();
        assert!(CollisionObjectReference::from_word(inactive_word).is_some());
        assert_eq!(
            inactive_link.register_pool_slot(5),
            Ok(Some(DEDICATED_PLAYER_POOL_SLOT)),
            "GoolObjectInit points at the separately allocated player even before it is active"
        );

        let main = spawn_test_object(&mut runtime, ZONE, 271, 0, 0);
        assert!(main.arena.is_dedicated_main());
        let live_link = runtime.machine.object(ordinary.vm).unwrap();
        assert_eq!(
            CollisionObjectReference::from_word(live_link.register(5).unwrap())
                .map(CollisionObjectReference::object),
            Some(main.vm)
        );
        assert_eq!(
            live_link.register_pool_slot(5),
            Ok(Some(DEDICATED_PLAYER_POOL_SLOT))
        );

        let mut report = ZoneTerminationReport::<()>::new();
        runtime
            .remove_runtime_subtree(main.arena, &mut report)
            .unwrap();
        assert_eq!(runtime.arena.main_object(), None);
        let reclaimed_link = runtime.machine.object(ordinary.vm).unwrap();
        assert!(
            CollisionObjectReference::from_word(reclaimed_link.register(5).unwrap()).is_some(),
            "title teardown must not turn the persistent player allocation into a null pointer"
        );
        assert_eq!(
            reclaimed_link.register_pool_slot(5),
            Ok(Some(DEDICATED_PLAYER_POOL_SLOT))
        );

        let replacement = spawn_test_object(&mut runtime, ZONE, 272, 0, 0);
        assert!(replacement.arena.is_dedicated_main());
        let reused_link = runtime.machine.object(ordinary.vm).unwrap();
        assert_eq!(
            CollisionObjectReference::from_word(reused_link.register(5).unwrap())
                .map(CollisionObjectReference::object),
            Some(replacement.vm)
        );
        assert_eq!(
            reused_link.register_pool_slot(5),
            Ok(Some(DEDICATED_PLAYER_POOL_SLOT))
        );
    }

    #[test]
    fn reclaimed_runtime_slot_starts_from_retained_process_storage() {
        let mut runtime = RetailRuntime::new(0);
        let original = spawn_test_object(&mut runtime, ZONE, 280, 2, 0);
        {
            let object = runtime.machine.object_mut(original.vm).unwrap();
            object
                .set_register(process_register::MISC_VALUE, 0x1234_5600)
                .unwrap();
            object
                .set_register(process_register::CAMERA_ZOOM, 0x2345_6700)
                .unwrap();
            object
                .set_register(process_register::STATUS_B, 0xffff_ffff)
                .unwrap();
        }
        let mut report = ZoneTerminationReport::<()>::new();
        runtime
            .remove_runtime_subtree(original.arena, &mut report)
            .unwrap();

        let replacement = spawn_test_object(&mut runtime, ZONE, 281, 2, 0);
        assert_eq!(replacement.arena.slot(), original.arena.slot());
        let object = runtime.machine.object(replacement.vm).unwrap();
        assert_eq!(
            object.register(process_register::MISC_VALUE),
            Ok(0x1234_5600)
        );
        assert_eq!(
            object.register(process_register::CAMERA_ZOOM),
            Ok(0x2345_6700)
        );
        assert_eq!(object.register(process_register::STATUS_B), Ok(0));
    }

    #[test]
    fn spawned_child_arguments_replace_reused_slot_pointer_provenance() {
        let mut runtime = RetailRuntime::new(0);
        let stale_target = spawn_test_object(&mut runtime, ZONE, 282, 2, 0);
        let argument_target = spawn_test_object(&mut runtime, ZONE, 283, 2, 0);
        let sacrificial = spawn_test_object(&mut runtime, ZONE, 284, 2, 0);
        let mut host = ArgumentProvenanceHost {
            argument_target: argument_target.vm,
        };
        let parent_entity = [entity(285, 42, 0)];
        let neighbors = [NeighborZone {
            eid: ZONE,
            display_flags: ACTIVE_ZONE_DISPLAY_BIT,
            entities: &parent_entity,
        }];
        let parent = *runtime.spawn_current_zone_neighbors(&neighbors, &mut host)[0]
            .result
            .as_ref()
            .unwrap();

        // Retire the slot the child will reuse with a different pointer in
        // its argument-register position. Seeding after argument injection
        // would resurrect this stale sidecar.
        let argument_register = runtime
            .machine
            .object(sacrificial.vm)
            .unwrap()
            .initial_stack_pointer() as usize;
        runtime
            .machine
            .object_mut(sacrificial.vm)
            .unwrap()
            .set_register(
                argument_register,
                CollisionObjectReference::new(stale_target.vm).to_word(),
            )
            .unwrap();
        let mut removal = ZoneTerminationReport::<()>::new();
        runtime
            .remove_runtime_subtree(sacrificial.arena, &mut removal)
            .unwrap();

        let frame = runtime.run_frame(&mut host, 2).unwrap();
        let child = *frame
            .spawned_children
            .iter()
            .find(|child| {
                runtime
                    .arena
                    .get(child.arena)
                    .is_some_and(|object| object.parent() == TreeParent::Object(parent.arena))
            })
            .expect("the parent must synchronously materialize its child");
        assert_eq!(child.arena.slot(), sacrificial.arena.slot());
        let child_object = runtime.machine.object(child.vm).unwrap();
        assert_eq!(
            child_object.register_pool_slot(child_object.initial_stack_pointer() as usize),
            Ok(Some(argument_target.arena.slot())),
            "creation argv must overwrite the reused slot's stale physical-pointer sidecar"
        );
    }

    fn install_szon_program(
        runtime: &mut RetailRuntime,
        requester: RuntimeObjectHandle,
        target: RuntimeObjectHandle,
        point: Option<[i32; 3]>,
    ) {
        let operand = point.map_or(0x0be0, |_| 0x0e00);
        let mut object =
            VmObject::new(requester.vm, vec![misc(9, 0, operand) | (3 << 12), RETURN]).unwrap();
        object.set_link(0, Some(requester.vm)).unwrap();
        object.set_link(3, Some(target.vm)).unwrap();
        if let Some(point) = point {
            for (register, coordinate) in point.into_iter().enumerate() {
                object
                    .set_register(register, coordinate.cast_unsigned())
                    .unwrap();
            }
        }
        runtime.machine.upsert_object(object).unwrap();
    }

    #[test]
    fn szon_runtime_assigns_validated_match_null_current_and_no_match() {
        let point = [0x1234, -0x5678, 0x7fff_ffff];
        let mut matched = RetailRuntime::new(0);
        let requester = spawn_test_object(&mut matched, ZONE, 210, 2, 0);
        let target = spawn_test_object(&mut matched, ZONE, 211, 2, 0);
        matched.set_level_state_context(level_context(ZONE, false, vec![ZONE, ZONE_B]));
        install_szon_program(&mut matched, requester, target, Some(point));
        let mut host = SzonHost {
            selected: Some(ZONE_B),
            queries: Vec::new(),
        };

        matched.run_frame(&mut host, 4).unwrap();

        assert_eq!(host.queries, [(ZONE, point)]);
        assert_eq!(matched.arena.get(target.arena).unwrap().zone(), ZONE_B);

        let mut unmatched = RetailRuntime::new(0);
        let requester = spawn_test_object(&mut unmatched, ZONE, 212, 2, 0);
        let target = spawn_test_object(&mut unmatched, ZONE_B, 213, 2, 0);
        unmatched.set_level_state_context(level_context(ZONE, false, vec![ZONE, ZONE_B]));
        install_szon_program(&mut unmatched, requester, target, Some(point));
        let mut host = SzonHost {
            selected: None,
            queries: Vec::new(),
        };

        unmatched.run_frame(&mut host, 4).unwrap();

        assert_eq!(host.queries, [(ZONE, point)]);
        assert_eq!(
            unmatched.arena.get(target.arena).unwrap().zone(),
            ZONE_B,
            "no containing neighbor leaves the linked object's zone unchanged"
        );

        let mut null_point = RetailRuntime::new(0);
        let requester = spawn_test_object(&mut null_point, ZONE_B, 214, 2, 0);
        let target = spawn_test_object(&mut null_point, ZONE_B, 215, 2, 0);
        null_point.set_level_state_context(level_context(ZONE, false, vec![ZONE, ZONE_B]));
        install_szon_program(&mut null_point, requester, target, None);
        let mut host = SzonHost {
            selected: Some(ZONE_B),
            queries: Vec::new(),
        };

        null_point.run_frame(&mut host, 4).unwrap();

        assert!(host.queries.is_empty(), "null never dereferences a point");
        assert_eq!(null_point.arena.get(target.arena).unwrap().zone(), ZONE);
    }

    fn mark_reclaimable(runtime: &mut RetailRuntime, object: RuntimeObjectHandle) {
        runtime
            .machine
            .object_mut(object.vm)
            .unwrap()
            .set_register(process_register::STATE_FLAGS, 0x0008_0000)
            .unwrap();
        runtime
            .arena
            .set_state_flags(object.arena, 0x0008_0000)
            .unwrap();
    }

    fn set_test_translation(object: &mut VmObject, translation: [i32; 3]) {
        for (register, value) in [
            (process_register::TRANSLATION_X, translation[0]),
            (process_register::TRANSLATION_Y, translation[1]),
            (process_register::TRANSLATION_Z, translation[2]),
        ] {
            object.set_register(register, value as u32).unwrap();
        }
    }

    #[test]
    fn misc_nearest_search_uses_root_four_preorder_category_distance_and_first_tie() {
        let mut runtime = RetailRuntime::new(0);
        let requester = spawn_test_object(&mut runtime, ZONE, 10, 2, 0);
        let origin = spawn_test_object(&mut runtime, ZONE, 11, 2, 0);
        let filtered = spawn_test_object(&mut runtime, ZONE, 12, 2, 0);
        let first_tie = spawn_test_object(&mut runtime, ZONE, 13, 2, 0);
        let later_tie = spawn_test_object(&mut runtime, ZONE, 14, 2, 0);

        let instruction = misc(13, 0b0_1000, 0x0e00) | (3 << 12);
        let mut requester_vm = VmObject::new(requester.vm, vec![instruction, RETURN]).unwrap();
        requester_vm.set_link(3, Some(origin.vm)).unwrap();
        requester_vm.set_register(0, 0xff).unwrap();
        set_test_translation(&mut requester_vm, [10_000, 0, 0]);
        runtime.machine.upsert_object(requester_vm).unwrap();
        set_test_translation(runtime.machine.object_mut(origin.vm).unwrap(), [0; 3]);

        for (object, category, translation) in [
            (filtered, 0x200, [1, 0, 0]),
            (first_tie, 0x300, [100, 0, 0]),
            (later_tie, 0x300, [-100, 0, 0]),
        ] {
            let vm = runtime.machine.object_mut(object.vm).unwrap();
            vm.configure_test_program_identity(category);
            set_test_translation(vm, translation);
        }
        // Root insertion is at the head, so reparent in reverse desired
        // order. The two matching candidates have equal ApxDist.
        for object in [later_tie, first_tie, filtered] {
            runtime
                .arena
                .reparent_to_root(object.arena, ENEMY_OBJECT_ROOT)
                .unwrap();
        }
        let root_order = runtime
            .arena
            .preorder(TreeParent::Root(ENEMY_OBJECT_ROOT))
            .unwrap()
            .collect::<Vec<_>>();
        assert_eq!(
            root_order,
            [filtered.arena, first_tie.arena, later_tie.arena]
        );

        let frame = runtime.run_frame(&mut SnapshotHost, 2).unwrap();
        assert!(
            frame
                .executions
                .iter()
                .find(|execution| execution.object == requester)
                .unwrap()
                .result
                .is_ok()
        );
        assert_eq!(
            runtime.machine.object(requester.vm).unwrap().stack(),
            &[CollisionObjectReference::new(first_tie.vm).to_word()]
        );
    }

    #[test]
    fn misc_nearest_status_interrupt_skips_rejected_candidate_and_accepts_ack() {
        const STATUS_EVENT: u32 = 0x0f00;

        let mut runtime = RetailRuntime::new(0);
        let requester = spawn_test_object(&mut runtime, ZONE, 20, 2, 0);
        let rejected = spawn_test_object(&mut runtime, ZONE, 21, 2, 0);
        let accepted = spawn_test_object(&mut runtime, ZONE, 22, 2, 0);
        let mut requester_vm = VmObject::new(
            requester.vm,
            vec![misc(13, 0b0_1000, TEST_SCALAR_OPERAND_A), RETURN],
        )
        .unwrap();
        requester_vm
            .set_register(TEST_SCALAR_REGISTER_A, STATUS_EVENT)
            .unwrap();
        set_test_translation(&mut requester_vm, [0; 3]);
        runtime.machine.upsert_object(requester_vm).unwrap();

        let ack_register = 0x0e00 | process_register::ACK as u16;
        {
            let vm = runtime.machine.object_mut(rejected.vm).unwrap();
            vm.configure_test_program_identity(0x300);
            set_test_translation(vm, [10, 0, 0]);
            vm.configure_test_event_interrupt(
                STATUS_EVENT,
                vec![Instruction::encode(0x11, 0x0800, ack_register), 0x8280_0000],
            )
            .unwrap();
        }
        {
            let vm = runtime.machine.object_mut(accepted.vm).unwrap();
            vm.configure_test_program_identity(0x300);
            set_test_translation(vm, [20, 0, 0]);
            vm.configure_test_event_interrupt(
                STATUS_EVENT,
                vec![Instruction::encode(0x11, 0x0b7f, ack_register), 0x8280_0000],
            )
            .unwrap();
        }
        for object in [accepted, rejected] {
            runtime
                .arena
                .reparent_to_root(object.arena, ENEMY_OBJECT_ROOT)
                .unwrap();
        }

        runtime.run_frame(&mut SnapshotHost, 2).unwrap();
        assert_eq!(
            runtime
                .machine
                .object(rejected.vm)
                .unwrap()
                .register(process_register::ACK),
            Ok(0)
        );
        assert_eq!(
            runtime
                .machine
                .object(accepted.vm)
                .unwrap()
                .register(process_register::ACK),
            Ok(0x100)
        );
        assert_eq!(
            runtime.machine.object(requester.vm).unwrap().stack(),
            &[CollisionObjectReference::new(accepted.vm).to_word()]
        );
    }

    #[test]
    fn opcode_91_reclaim_signals_and_cleans_descendants_before_vm_handle_reuse() {
        let mut runtime = RetailRuntime::new(0);
        let mut host = ReclaimHost::default();
        let candidate_entities = [entity(10, 1, 0)];
        let candidate_neighbors = [NeighborZone {
            eid: ZONE,
            display_flags: 2,
            entities: &candidate_entities,
        }];
        let candidate = *runtime.spawn_current_zone_neighbors(&candidate_neighbors, &mut host)[0]
            .result
            .as_ref()
            .unwrap();
        let first = runtime.run_frame(&mut host, 2).unwrap();
        let descendant = first.spawned_children[0];

        for object in [candidate, descendant] {
            runtime
                .machine
                .object_mut(object.vm)
                .unwrap()
                .configure_test_event_interrupt(TERMINATE_EVENT, vec![0x8280_0000])
                .unwrap();
            runtime.pending_states.insert(object.vm, 7);
            runtime.faulted_objects.insert(object);
        }
        mark_reclaimable(&mut runtime, candidate);

        let fillers = (0..94)
            .map(|index| {
                let executable = if index == 93 { 12 } else { 2 };
                entity(20 + index, executable, 0)
            })
            .collect::<Vec<_>>();
        let filler_neighbors = [NeighborZone {
            eid: ZONE,
            display_flags: 2,
            entities: &fillers,
        }];
        let attempts = runtime.spawn_current_zone_neighbors(&filler_neighbors, &mut host);
        assert!(attempts.iter().all(|attempt| attempt.result.is_ok()));
        assert_eq!(runtime.arena.remaining_pool_capacity(), 0);
        let parent = *attempts.last().unwrap().result.as_ref().unwrap();

        let frame = runtime.run_frame(&mut host, 2).unwrap();
        let replacement = frame.spawned_children[0];
        assert!(
            frame
                .executions
                .iter()
                .find(|execution| execution.object == parent)
                .unwrap()
                .result
                .is_ok()
        );
        assert_eq!(replacement.vm, candidate.vm);
        assert_ne!(replacement.arena, candidate.arena);
        assert!(runtime.arena.get(candidate.arena).is_none());
        assert!(runtime.arena.get(descendant.arena).is_none());
        assert_eq!(runtime.handles.for_vm(candidate.vm), Some(replacement));
        assert!(runtime.handles.for_vm(descendant.vm).is_none());
        assert!(!runtime.pending_states.contains_key(&candidate.vm));
        assert!(!runtime.pending_states.contains_key(&descendant.vm));
        assert!(!runtime.faulted_objects.contains(&candidate));
        assert!(!runtime.faulted_objects.contains(&descendant));
        assert_eq!(host.freed_audio, [descendant, candidate]);
        assert!(runtime.take_cleanup_actions().is_empty());
        assert!(runtime.take_reclaim_event_faults().is_empty());
        assert_eq!(
            runtime.arena.len(),
            crate::object_arena::OBJECT_POOL_CAPACITY - 1
        );
        assert_eq!(runtime.arena.remaining_pool_capacity(), 1);
    }

    #[test]
    fn full_pool_zone_spawn_reclaims_and_surfaces_a_faulted_term_handler() {
        let mut runtime = RetailRuntime::new(0);
        let candidate = spawn_test_object(&mut runtime, ZONE, 200, 2, 0);
        mark_reclaimable(&mut runtime, candidate);
        runtime
            .machine
            .object_mut(candidate.vm)
            .unwrap()
            .configure_test_event_interrupt(TERMINATE_EVENT, vec![0xff00_0000])
            .unwrap();

        let fillers = (100..195).map(|id| entity(id, 2, 0)).collect::<Vec<_>>();
        let filler_neighbors = [NeighborZone {
            eid: ZONE,
            display_flags: 2,
            entities: &fillers,
        }];
        assert!(
            runtime
                .spawn_current_zone_neighbors(&filler_neighbors, &mut SnapshotHost)
                .iter()
                .all(|attempt| attempt.result.is_ok())
        );
        assert_eq!(runtime.arena.remaining_pool_capacity(), 0);

        let replacement_entities = [entity(250, 2, 0)];
        let replacement_neighbors = [NeighborZone {
            eid: ZONE,
            display_flags: 2,
            entities: &replacement_entities,
        }];
        let replacement = *runtime
            .spawn_current_zone_neighbors(&replacement_neighbors, &mut SnapshotHost)[0]
            .result
            .as_ref()
            .unwrap();

        assert_eq!(replacement.vm, candidate.vm);
        assert_ne!(replacement.arena, candidate.arena);
        assert!(runtime.arena.get(candidate.arena).is_none());
        assert_eq!(runtime.arena.spawn_table().flags(200), Some(0));
        assert_eq!(runtime.arena.spawn_table().flags(250), Some(1));
        assert_eq!(
            runtime.take_cleanup_actions(),
            [RuntimeCleanupAction::FreeObjectAudio(candidate)]
        );
        assert_eq!(
            runtime.take_reclaim_event_faults(),
            [RuntimeReclaimEventFault { object: candidate }]
        );
        assert_eq!(
            runtime.arena.len(),
            crate::object_arena::OBJECT_POOL_CAPACITY
        );
    }

    fn attach_test_child(
        runtime: &mut RetailRuntime,
        parent: RuntimeObjectHandle,
        zone: Eid,
        executable: u8,
    ) -> RuntimeObjectHandle {
        let arena = runtime
            .arena
            .create_child(parent.arena, zone, executable, 0, false)
            .unwrap();
        let object = runtime.handles.reserve::<()>(arena).unwrap();
        let mut vm_object = VmObject::new(object.vm, vec![RETURN]).unwrap();
        vm_object.set_link(1, Some(parent.vm)).unwrap();
        vm_object.set_link(4, Some(parent.vm)).unwrap();
        runtime.machine.upsert_object(vm_object).unwrap();
        runtime
            .machine
            .bind_retail_pool_slot(object.vm, object.arena.slot())
            .unwrap();
        object
    }

    #[test]
    fn paired_subtree_teardown_clears_a_runtime_childs_live_pid_slot() {
        let mut runtime = RetailRuntime::new_for_level(0, LevelId::N_SANITY_BEACH);
        let parent = spawn_test_object(&mut runtime, ZONE, 64, 2, 0);
        let child = attach_test_child(&mut runtime, parent, ZONE, 5);
        runtime
            .machine
            .object_mut(child.vm)
            .unwrap()
            .set_register(process_register::PID_FLAGS, 131 << 8)
            .unwrap();
        runtime
            .arena
            .spawn_table_mut()
            .set_flags(0, 0x8000_000f)
            .unwrap();
        runtime
            .arena
            .spawn_table_mut()
            .set_flags(131, 0x4000_000f)
            .unwrap();
        runtime.machine.set_spawn_flags(0, 0x8000_000f).unwrap();
        runtime.machine.set_spawn_flags(131, 0x4000_000f).unwrap();

        let mut report = ZoneTerminationReport::<()>::new();
        runtime
            .remove_runtime_subtree(child.arena, &mut report)
            .unwrap();

        assert_eq!(report.terminated, [child]);
        assert_eq!(
            report.cleanup_actions,
            [RuntimeCleanupAction::FreeObjectAudio(child)]
        );
        assert!(runtime.arena.get(parent.arena).is_some());
        assert!(runtime.arena.get(child.arena).is_none());
        assert_eq!(runtime.arena.spawn_table().flags(0), Some(0x8000_000f));
        assert_eq!(runtime.arena.spawn_table().flags(131), Some(0x4000_000e));
        assert_eq!(runtime.machine.spawn_flags(0), Ok(0x8000_000f));
        assert_eq!(runtime.machine.spawn_flags(131), Ok(0x4000_000e));
    }

    #[test]
    fn paired_subtree_teardown_rejects_an_invalid_live_pid_atomically() {
        let mut runtime = RetailRuntime::new_for_level(0, LevelId::N_SANITY_BEACH);
        let parent = spawn_test_object(&mut runtime, ZONE, 64, 2, 0);
        let child = attach_test_child(&mut runtime, parent, ZONE, 5);
        runtime
            .machine
            .object_mut(child.vm)
            .unwrap()
            .set_register(process_register::PID_FLAGS, u32::from(u16::MAX) << 8)
            .unwrap();

        let mut report = ZoneTerminationReport::<()>::new();
        assert_eq!(
            runtime.remove_runtime_subtree(child.arena, &mut report),
            Err(RuntimeError::Spawn(SpawnError::InvalidSpawnId(u16::MAX)))
        );
        assert!(report.terminated.is_empty());
        assert!(report.cleanup_actions.is_empty());
        assert!(runtime.arena.get(parent.arena).is_some());
        assert!(runtime.arena.get(child.arena).is_some());
        assert!(runtime.machine.object(child.vm).is_ok());
    }

    fn configure_level_end_transition(
        runtime: &mut RetailRuntime,
        object: RuntimeObjectHandle,
        target: i32,
    ) {
        let vm = runtime.machine.object_mut(object.vm).unwrap();
        vm.configure_test_event_interrupt(LEVEL_END_EVENT, vec![misc(12, 9, 0x0e00), 0x8280_0000])
            .unwrap();
        vm.set_register(0, target.cast_unsigned() << 8).unwrap();
    }

    fn arm_zone_migration_terminate_handler(
        runtime: &mut RetailRuntime,
        object: RuntimeObjectHandle,
    ) {
        runtime
            .machine
            .object_mut(object.vm)
            .unwrap()
            .configure_test_event_interrupt(TERMINATE_EVENT, vec![misc(12, 4, 0x0e00), 0x8280_0000])
            .unwrap();
    }

    fn install_neighbor_termination_program(
        runtime: &mut RetailRuntime,
        requester: RuntimeObjectHandle,
        trailing_code: &[u32],
    ) {
        let pid_flags = runtime
            .machine
            .object(requester.vm)
            .unwrap()
            .register(process_register::PID_FLAGS)
            .unwrap();
        let mut code = vec![misc(12, 7, 0x0be0)];
        code.extend_from_slice(trailing_code);
        code.push(RETURN);
        let mut vm = VmObject::new(requester.vm, code).unwrap();
        vm.set_link(0, Some(requester.vm)).unwrap();
        vm.set_register(process_register::PID_FLAGS, pid_flags)
            .unwrap();
        runtime.machine.upsert_object(vm).unwrap();
    }

    fn arm_counting_terminate_handler(
        runtime: &mut RetailRuntime,
        object: RuntimeObjectHandle,
        register: u16,
    ) {
        runtime
            .machine
            .object_mut(object.vm)
            .unwrap()
            .configure_test_event_interrupt(
                TERMINATE_EVENT,
                vec![
                    Instruction::encode(0x00, 0x0801, 0x0e00 | register),
                    Instruction::encode(0x11, 0x0e1f, 0x0e00 | register),
                    0x8280_0000,
                ],
            )
            .unwrap();
    }

    #[test]
    fn misc_twelve_seven_preserves_neighbor_root_and_postorder_lifecycle() {
        let mut runtime = RetailRuntime::new_for_level(0, LevelId::N_SANITY_BEACH);
        let parent = spawn_test_object(&mut runtime, ZONE, 70, 2, 0);
        let child = attach_test_child(&mut runtime, parent, ZONE, 3);
        let root_seven = spawn_test_object(&mut runtime, ZONE, 71, 2, 0);
        let zone_b = spawn_test_object(&mut runtime, ZONE_B, 72, 2, 0);
        let migrant = spawn_test_object(&mut runtime, ZONE_B, 73, 2, 0);
        let status_immune = spawn_test_object(&mut runtime, ZONE, 74, 2, 0);
        let state_immune = spawn_test_object(&mut runtime, ZONE, 75, 2, 0);
        let crash = spawn_test_object(&mut runtime, ZONE, 76, 0, 0);
        let requester = spawn_test_object(&mut runtime, CURRENT_ZONE, 77, 2, 0);

        runtime
            .arena
            .reparent_to_root(parent.arena, RootHandle::new(0).unwrap())
            .unwrap();
        runtime
            .arena
            .reparent_to_root(requester.arena, RootHandle::new(0).unwrap())
            .unwrap();
        runtime
            .arena
            .reparent_to_root(status_immune.arena, RootHandle::new(2).unwrap())
            .unwrap();
        runtime
            .arena
            .reparent_to_root(state_immune.arena, RootHandle::new(2).unwrap())
            .unwrap();
        runtime
            .arena
            .reparent_to_root(zone_b.arena, RootHandle::new(4).unwrap())
            .unwrap();
        runtime
            .arena
            .reparent_to_root(migrant.arena, RootHandle::new(5).unwrap())
            .unwrap();
        runtime
            .arena
            .reparent_to_root(root_seven.arena, RootHandle::new(7).unwrap())
            .unwrap();
        runtime
            .machine
            .object_mut(status_immune.vm)
            .unwrap()
            .set_register(process_register::STATUS_B, ZONE_TERMINATION_STATUS_B_IMMUNE)
            .unwrap();
        runtime
            .machine
            .object_mut(state_immune.vm)
            .unwrap()
            .set_register(process_register::STATE_FLAGS, ZONE_TERMINATION_STATE_IMMUNE)
            .unwrap();
        arm_zone_migration_terminate_handler(&mut runtime, migrant);
        arm_counting_terminate_handler(&mut runtime, crash, 10);
        runtime.transition_zone_context = ObjectZoneContext::Target(ZONE_C);
        runtime.set_level_state_context(level_context(CURRENT_ZONE, false, Vec::new()));
        install_neighbor_termination_program(
            &mut runtime,
            requester,
            &[Instruction::encode(0x11, 0x0805, 0x0e0a)],
        );
        let mut host = NeighborTerminationHost::new(vec![ZONE, ZONE_B, ZONE]);

        let frame = runtime.run_frame(&mut host, 8).unwrap();

        assert_eq!(host.neighbor_queries, [CURRENT_ZONE]);
        assert_eq!(host.neighbors, [ZONE, ZONE_B, ZONE]);
        assert_eq!(host.freed_audio, [child, parent, root_seven, zone_b]);
        assert_eq!(runtime.take_cleanup_actions(), []);
        for (object, spawn_id) in [(parent, 70), (root_seven, 71), (zone_b, 72)] {
            assert_eq!(runtime.object_for_vm(object.vm), None);
            assert_eq!(runtime.arena.spawn_table().flags(spawn_id), Some(0));
        }
        for object in [status_immune, state_immune, crash, migrant, requester] {
            assert_eq!(runtime.object_for_vm(object.vm), Some(object));
        }
        assert_eq!(runtime.arena.get(migrant.arena).unwrap().zone(), ZONE_C);
        assert_eq!(
            runtime.machine.object(crash.vm).unwrap().register(10),
            Ok(0x200)
        );
        assert_eq!(
            runtime.machine.object(requester.vm).unwrap().register(10),
            Ok(0x500),
            "the requester resumes only after synchronous teardown"
        );
        assert_eq!(
            runtime.transition_zone_context,
            ObjectZoneContext::Target(ZONE_C)
        );
        assert!(frame.effects.iter().any(|effect| matches!(
            effect,
            VmEffect::TerminateCurrentZoneNeighbors { requester: effect_requester }
                if *effect_requester == requester.vm
        )));
    }

    #[test]
    fn misc_twelve_seven_rescans_objects_migrated_into_a_later_neighbor() {
        let mut runtime = RetailRuntime::new_for_level(0, LevelId::N_SANITY_BEACH);
        let migrant = spawn_test_object(&mut runtime, ZONE_B, 80, 2, 0);
        let requester = spawn_test_object(&mut runtime, CURRENT_ZONE, 81, 2, 0);
        runtime
            .arena
            .reparent_to_root(requester.arena, RootHandle::new(0).unwrap())
            .unwrap();
        arm_zone_migration_terminate_handler(&mut runtime, migrant);
        runtime.transition_zone_context = ObjectZoneContext::Target(ZONE);
        runtime.set_level_state_context(level_context(CURRENT_ZONE, false, Vec::new()));
        install_neighbor_termination_program(
            &mut runtime,
            requester,
            &[Instruction::encode(0x11, 0x0805, 0x0e0a)],
        );
        let mut host = NeighborTerminationHost::new(vec![ZONE_B, ZONE]);

        let frame = runtime.run_frame(&mut host, 8).unwrap();

        assert_eq!(host.neighbor_queries, [CURRENT_ZONE]);
        assert_eq!(host.freed_audio, [migrant]);
        assert_eq!(runtime.object_for_vm(migrant.vm), None);
        assert_eq!(runtime.arena.spawn_table().flags(80), Some(0));
        assert_eq!(
            runtime.machine.object(requester.vm).unwrap().register(10),
            Ok(0x500)
        );
        assert_eq!(
            frame
                .effects
                .iter()
                .filter(|effect| matches!(effect, VmEffect::SetObjectZoneToTransitionTarget { object } if *object == migrant.vm))
                .count(),
            2,
            "the migrated object is visited again when the later neighbor begins"
        );
    }

    #[test]
    fn misc_twelve_seven_can_reclaim_its_active_requester_cleanly() {
        let mut runtime = RetailRuntime::new_for_level(0, LevelId::N_SANITY_BEACH);
        let requester = spawn_test_object(&mut runtime, CURRENT_ZONE, 82, 2, 0);
        runtime
            .arena
            .reparent_to_root(requester.arena, RootHandle::new(0).unwrap())
            .unwrap();
        runtime.set_level_state_context(level_context(CURRENT_ZONE, false, Vec::new()));
        install_neighbor_termination_program(&mut runtime, requester, &[misc(12, 5, 0x0be0)]);
        let mut host = NeighborTerminationHost::new(vec![CURRENT_ZONE]);

        let frame = runtime.run_frame(&mut host, 8).unwrap();

        assert_eq!(host.neighbor_queries, [CURRENT_ZONE]);
        assert_eq!(host.freed_audio, [requester]);
        assert_eq!(runtime.object_for_vm(requester.vm), None);
        assert_eq!(runtime.arena.spawn_table().flags(82), Some(0));
        assert!(runtime.faulted_objects.is_empty());
        assert!(matches!(
            frame.executions.as_slice(),
            [RuntimeExecution {
                object,
                result: Ok(Execution {
                    reason: HaltReason::ObjectTerminated,
                    ..
                }),
            }] if *object == requester
        ));
        assert_eq!(
            frame.effects,
            [VmEffect::TerminateCurrentZoneNeighbors {
                requester: requester.vm,
            }],
            "the instruction after misc 12/7 must not resume a reclaimed requester"
        );
    }

    #[test]
    fn misc_twelve_seven_is_a_no_op_when_the_current_zone_is_null() {
        let mut runtime = RetailRuntime::new_for_level(0, LevelId::N_SANITY_BEACH);
        let requester = spawn_test_object(&mut runtime, CURRENT_ZONE, 83, 2, 0);
        let untouched = spawn_test_object(&mut runtime, ZONE, 84, 2, 0);
        runtime
            .arena
            .reparent_to_root(requester.arena, RootHandle::new(0).unwrap())
            .unwrap();
        runtime
            .arena
            .reparent_to_root(untouched.arena, RootHandle::new(1).unwrap())
            .unwrap();
        install_neighbor_termination_program(
            &mut runtime,
            requester,
            &[Instruction::encode(0x11, 0x0805, 0x0e0a)],
        );
        let mut host = NeighborTerminationHost::new(vec![ZONE]);

        runtime.run_frame(&mut host, 8).unwrap();

        assert!(host.neighbor_queries.is_empty());
        assert!(host.freed_audio.is_empty());
        assert_eq!(runtime.object_for_vm(untouched.vm), Some(untouched));
        assert_eq!(
            runtime.machine.object(requester.vm).unwrap().register(10),
            Ok(0x500)
        );
    }

    #[test]
    fn misc_twelve_seven_clears_the_machine_spawn_bit_before_trailing_spawn_writes() {
        let mut runtime = RetailRuntime::new_for_level(0, LevelId::N_SANITY_BEACH);
        let terminated = spawn_test_object(&mut runtime, ZONE, 70, 2, 0);
        let requester = spawn_test_object(&mut runtime, CURRENT_ZONE, 85, 2, 0);
        runtime
            .arena
            .reparent_to_root(requester.arena, RootHandle::new(0).unwrap())
            .unwrap();
        runtime.set_level_state_context(level_context(CURRENT_ZONE, false, Vec::new()));
        install_neighbor_termination_program(
            &mut runtime,
            requester,
            &[misc(10, 1, TEST_SCALAR_OPERAND_A)],
        );
        runtime
            .machine
            .object_mut(requester.vm)
            .unwrap()
            .set_register(TEST_SCALAR_REGISTER_A, 70 << 8)
            .unwrap();
        let mut host = NeighborTerminationHost::new(vec![ZONE]);

        let frame = runtime.run_frame(&mut host, 8).unwrap();

        assert_eq!(host.freed_audio, [terminated]);
        assert_eq!(runtime.object_for_vm(terminated.vm), None);
        assert_eq!(runtime.machine.spawn_flags(70), Ok(4));
        assert_eq!(runtime.arena.spawn_table().flags(70), Some(4));
        assert!(frame.effects.iter().any(|effect| matches!(
            effect,
            VmEffect::SpawnFlagsChanged {
                object,
                id: 70,
                flags: 4,
            } if *object == requester.vm
        )));
    }

    #[test]
    fn audio_host_request_completes_before_normal_code_advances() {
        let mut runtime = RetailRuntime::new(0);
        let object = spawn_test_object(&mut runtime, ZONE, 60, 2, 0);
        let mut vm = VmObject::new(
            object.vm,
            vec![
                audio_create(),
                Instruction::encode(0x11, 0x0805, 0x0e00),
                RETURN,
            ],
        )
        .unwrap();
        vm.set_link(0, Some(object.vm)).unwrap();
        runtime.machine.upsert_object(vm).unwrap();
        prepare_audio_registers(&mut runtime, object);
        let mut host = AudioRecordingHost::new(None);

        runtime.run_frame(&mut host, 3).unwrap();

        assert_eq!(host.requests.len(), 1);
        assert_eq!(runtime.machine.pending_audio_host_request(), None);
        let vm = runtime.machine.object(object.vm).unwrap();
        assert_eq!(vm.register(process_register::VOICE_ID), Ok(40));
        assert_eq!(vm.register(0), Ok(0x500));
    }

    #[test]
    fn once_and_transition_audio_share_one_synchronous_state_rebind() {
        let state = VmStateProgram::new(
            7,
            GoolState {
                flags: 0,
                status_c: 0,
                external_index: 0,
                event_pc: GOOL_PC_NONE,
                transition_pc: 0,
                code_pc: 2,
            },
            vec![audio_create(), 0x8280_0000, RETURN],
            Vec::new(),
        )
        .unwrap();
        let mut host = AudioRecordingHost::new(Some(state));
        let mut runtime = RetailRuntime::new(0);
        let object = spawn_test_object(&mut runtime, ZONE, 61, 2, 0);
        prepare_audio_registers(&mut runtime, object);
        runtime
            .machine
            .object_mut(object.vm)
            .unwrap()
            .configure_test_once(vec![audio_create(), 0x8280_0000], 0)
            .unwrap();
        runtime
            .machine
            .object_mut(object.vm)
            .unwrap()
            .configure_test_state(7);
        runtime.pending_states.insert(object.vm, 7);

        let frame = runtime.run_frame(&mut host, 1).unwrap();

        assert_eq!(host.requests.len(), 2, "{frame:?}");
        assert_eq!(runtime.machine.pending_audio_host_request(), None);
        assert_eq!(
            runtime
                .machine
                .object(object.vm)
                .unwrap()
                .register(process_register::VOICE_ID),
            Ok(41)
        );
    }

    #[test]
    fn event_service_and_interrupt_audio_complete_inside_delivery() {
        const EVENT: u32 = 0x1500;
        for event_service in [false, true] {
            let mut runtime = RetailRuntime::new(0);
            let object = spawn_test_object(
                &mut runtime,
                ZONE,
                if event_service { 62 } else { 63 },
                2,
                0,
            );
            prepare_audio_registers(&mut runtime, object);
            let vm = runtime.machine.object_mut(object.vm).unwrap();
            if event_service {
                vm.configure_test_event_service(vec![audio_create(), 0x8880_0000], 0)
                    .unwrap();
            } else {
                vm.configure_test_event_interrupt(EVENT, vec![audio_create(), 0x8280_0000])
                    .unwrap();
            }
            let mut host = AudioRecordingHost::new(None);

            runtime
                .dispatch_event(&mut host, None, Some(object), EVENT, None)
                .unwrap();

            assert_eq!(host.requests.len(), 1);
            assert_eq!(runtime.machine.pending_audio_host_request(), None);
            assert_eq!(
                runtime
                    .machine
                    .object(object.vm)
                    .unwrap()
                    .register(process_register::VOICE_ID),
                Ok(40)
            );
        }
    }

    fn install_test_event_sender(
        runtime: &mut RetailRuntime,
        sender: RuntimeObjectHandle,
        recipient: Option<RuntimeObjectHandle>,
        opcode: u8,
        event: u32,
    ) {
        let link = usize::from(recipient.is_some() && opcode != 0x90);
        let operand_a =
            (u16::try_from(link).unwrap() << 9) | u16::try_from(TEST_CONDITION_REGISTER).unwrap();
        let mut object = VmObject::new(
            sender.vm,
            vec![
                Instruction::encode(opcode, operand_a, TEST_SCALAR_OPERAND_A),
                RETURN,
            ],
        )
        .unwrap();
        object
            .set_link(
                0,
                if link == 0 {
                    recipient.map_or(Some(sender.vm), |recipient| Some(recipient.vm))
                } else {
                    Some(sender.vm)
                },
            )
            .unwrap();
        if let Some(recipient) = recipient {
            object.set_link(link, Some(recipient.vm)).unwrap();
        }
        object.set_register(TEST_CONDITION_REGISTER, 1).unwrap();
        object.set_register(TEST_SCALAR_REGISTER_A, event).unwrap();
        runtime.machine.upsert_object(object).unwrap();
    }

    #[test]
    fn event_opcodes_dispatch_synchronously_through_the_runtime_host() {
        const EVENT: u32 = 0x1500;

        let mut runtime = RetailRuntime::new(0);
        let recipient = spawn_test_object(&mut runtime, ZONE, 0x87, 2, 0);
        let sender = spawn_test_object(&mut runtime, ZONE, 0x88, 2, 0);
        runtime
            .machine
            .object_mut(recipient.vm)
            .unwrap()
            .configure_test_event_interrupt(EVENT, vec![0x8280_0000])
            .unwrap();
        install_test_event_sender(&mut runtime, sender, Some(recipient), 0x87, EVENT);

        runtime.run_frame(&mut SnapshotHost, 8).unwrap();

        assert_eq!(
            runtime
                .machine
                .object(recipient.vm)
                .unwrap()
                .register(process_register::EVENT),
            Ok(EVENT)
        );
        assert!(!runtime.faulted_objects.contains(&sender));

        let mut runtime = RetailRuntime::new(0);
        let root = spawn_test_object(&mut runtime, ZONE, 0x90, 2, 0);
        let descendant = spawn_test_object(&mut runtime, ZONE, 0x91, 2, 0);
        let sender = spawn_test_object(&mut runtime, ZONE, 0x92, 2, 0);
        for recipient in [root, descendant] {
            runtime
                .machine
                .object_mut(recipient.vm)
                .unwrap()
                .configure_test_event_interrupt(EVENT, vec![0x8280_0000])
                .unwrap();
        }
        runtime
            .arena
            .add_child(TreeParent::Object(root.arena), descendant.arena)
            .unwrap();
        RetailRuntime::refresh_tree_links::<()>(
            &runtime.arena,
            &runtime.handles,
            &mut runtime.machine,
        )
        .unwrap();
        install_test_event_sender(&mut runtime, sender, Some(root), 0x90, EVENT);

        runtime.run_frame(&mut SnapshotHost, 8).unwrap();

        assert_eq!(
            runtime
                .machine
                .object(root.vm)
                .unwrap()
                .register(process_register::EVENT),
            Ok(0),
            "opcode 0x90 must exclude its linked root"
        );
        assert_eq!(
            runtime
                .machine
                .object(descendant.vm)
                .unwrap()
                .register(process_register::EVENT),
            Ok(EVENT)
        );
        assert!(!runtime.faulted_objects.contains(&sender));

        let mut runtime = RetailRuntime::new(0);
        let first = spawn_test_object(&mut runtime, ZONE, 200, 2, 0);
        let second = spawn_test_object(&mut runtime, ZONE, 201, 2, 0);
        let sender = spawn_test_object(&mut runtime, ZONE, 202, 2, 0);
        for recipient in [first, second] {
            runtime
                .machine
                .object_mut(recipient.vm)
                .unwrap()
                .configure_test_event_interrupt(EVENT, vec![0x8280_0000])
                .unwrap();
        }
        install_test_event_sender(&mut runtime, sender, None, 0x8f, EVENT);

        runtime.run_frame(&mut SnapshotHost, 8).unwrap();

        for recipient in [first, second] {
            assert_eq!(
                runtime
                    .machine
                    .object(recipient.vm)
                    .unwrap()
                    .register(process_register::EVENT),
                Ok(EVENT)
            );
        }
        assert!(!runtime.faulted_objects.contains(&sender));
    }

    #[test]
    fn all_root_broadcast_checkpoints_each_recipient_effect_transaction() {
        const EVENT: u32 = 0x1500;

        let mut runtime = RetailRuntime::new(0);
        let recipients = (0..OBJECT_POOL_CAPACITY - 1)
            .map(|index| {
                let recipient = spawn_test_object(&mut runtime, ZONE, (index + 100) as u16, 3, 0);
                let transition_value = i32::from(recipient.vm.get()) + 1;
                let vm = runtime.machine.object_mut(recipient.vm).unwrap();
                vm.set_register(TEST_SCALAR_REGISTER_A, (transition_value as u32) << 8)
                    .unwrap();
                vm.configure_test_event_interrupt(
                    EVENT,
                    vec![
                        misc(12, 9, TEST_SCALAR_OPERAND_A),
                        misc(12, 9, TEST_SCALAR_OPERAND_A),
                        misc(12, 9, TEST_SCALAR_OPERAND_A),
                        0x8280_0000,
                    ],
                )
                .unwrap();
                recipient
            })
            .collect::<Vec<_>>();
        let sender = spawn_test_object(&mut runtime, ZONE, 300, 3, 0);
        install_test_event_sender(&mut runtime, sender, None, 0x8f, EVENT);
        let delivery_order = runtime
            .arena
            .postorder_snapshot()
            .unwrap()
            .into_iter()
            .filter_map(|arena| runtime.handles.for_arena(arena))
            .filter(|object| recipients.contains(object))
            .collect::<Vec<_>>();

        let frame = runtime.run_frame(&mut SnapshotHost, 4).unwrap();

        assert_eq!(recipients.len(), OBJECT_POOL_CAPACITY - 1);
        assert_eq!(delivery_order.len(), recipients.len());
        let transitions = frame
            .effects
            .iter()
            .filter_map(|effect| match effect {
                VmEffect::Transition(value) => Some(*value),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            transitions,
            delivery_order
                .iter()
                .flat_map(|recipient| [i32::from(recipient.vm.get()) + 1; 3])
                .collect::<Vec<_>>()
        );
        assert!(!runtime.faulted_objects.contains(&sender));
    }

    #[test]
    fn inline_solid_candidate_refresh_rejects_a_reused_vm_slot_generation() {
        let mut runtime = RetailRuntime::new(0);
        let original = spawn_test_object(&mut runtime, ZONE, 0x93, 2, 0);
        let candidate_id = u32::from(original.vm.get());
        let generations = BTreeMap::from([(candidate_id, original)]);
        let sentinel_translation = Vec3 { x: 1, y: 2, z: 3 };
        let mut candidates = [SolidObjectCandidate {
            id: candidate_id,
            active: true,
            translation: sentinel_translation,
            bounds: Bounds3::default(),
            status_b: crate::retail_solid_motion::SOLID_BOTTOM,
            status_c: 0,
            state_flags: 0,
            category: 0,
            object_type: 0,
            hotspot_size: 0,
        }];

        runtime
            .reclaim_runtime_subtree(original.arena, &mut SnapshotHost, &mut Vec::new())
            .unwrap();
        let replacement = spawn_test_object(&mut runtime, ZONE, 0x94, 2, 0);
        assert_eq!(replacement.vm, original.vm);
        assert_ne!(replacement.arena, original.arena);
        runtime
            .machine
            .object_mut(replacement.vm)
            .unwrap()
            .set_register(process_register::STATUS_B, u32::MAX)
            .unwrap();

        RetailRuntime::refresh_inline_solid_candidates::<()>(
            &runtime.handles,
            &runtime.machine,
            &generations,
            &mut candidates,
        )
        .unwrap();

        assert!(!candidates[0].active);
        assert_eq!(candidates[0].translation, sentinel_translation);
        assert_ne!(candidates[0].status_b, u32::MAX);
    }

    #[test]
    fn invincibility_hit_interrupt_runs_before_physics_with_checked_zero_argument() {
        const HIT_INVINCIBLE_EVENT: u32 = 0x0a00;
        const SENDER_VELOCITY_X: u16 = 0x0c00 | (7_u16 << 6) | process_register::MISC_A_X as u16;

        let mut runtime = RetailRuntime::new(0);
        let sender = spawn_test_object(&mut runtime, ZONE, 0x95, 2, 0);
        let collider = spawn_test_object(&mut runtime, ZONE, 0x96, 2, 0);
        {
            let sender_object = runtime.machine.object_mut(sender.vm).unwrap();
            sender_object
                .set_register(process_register::INVINCIBILITY_STATE, 4)
                .unwrap();
            sender_object
                .set_register(
                    process_register::STATUS_B,
                    crate::retail_physics::STATUS_B_TRANSLATION_MOTION,
                )
                .unwrap();
            sender_object
                .set_register(process_register::TRANSLATION_X, 0)
                .unwrap();
            sender_object
                .set_register(process_register::MISC_A_X, 0)
                .unwrap();
            sender_object.set_link(6, Some(collider.vm)).unwrap();
        }
        {
            let collider_object = runtime.machine.object_mut(collider.vm).unwrap();
            collider_object.configure_test_program_identity(0x300);
            collider_object
                .set_register(process_register::ACK, 0xfeed_beef)
                .unwrap();
            collider_object
                .configure_test_event_interrupt(
                    HIT_INVINCIBLE_EVENT,
                    vec![
                        // fp[-1] is the one checked replacement word for the
                        // source's argc=1/null-argv call.
                        Instruction::encode(0x11, 0x0b7f, 0x0e00 | process_register::ACK as u16),
                        Instruction::encode(0x11, 0x0840, SENDER_VELOCITY_X),
                        0x8280_0000,
                    ],
                )
                .unwrap();
        }
        let _discarded_spawn_effects = runtime.machine.take_effects();

        runtime
            .finish_native_object_update(sender, &mut SnapshotHost, &mut Vec::new())
            .unwrap();

        let sender_object = runtime.machine.object(sender.vm).unwrap();
        assert_eq!(
            sender_object.register(process_register::MISC_A_X),
            Ok(0x4000)
        );
        assert_eq!(
            sender_object.register(process_register::TRANSLATION_X),
            Ok(544),
            "the interrupt velocity must be visible to the same update's 34-tick physics pass"
        );
        let collider_object = runtime.machine.object(collider.vm).unwrap();
        assert_eq!(collider_object.register(process_register::ACK), Ok(0));
        assert_eq!(
            collider_object.register(process_register::EVENT),
            Ok(HIT_INVINCIBLE_EVENT)
        );
        assert!(runtime.machine.effects().is_empty());
        assert!(runtime.take_invincibility_event_faults().is_empty());
    }

    #[test]
    fn invincibility_hit_fault_is_diagnostic_and_physics_still_finishes() {
        const HIT_INVINCIBLE_EVENT: u32 = 0x0a00;

        let mut runtime = RetailRuntime::new(0);
        let sender = spawn_test_object(&mut runtime, ZONE, 0x97, 2, 0);
        let collider = spawn_test_object(&mut runtime, ZONE, 0x98, 2, 0);
        {
            let sender_object = runtime.machine.object_mut(sender.vm).unwrap();
            sender_object
                .set_register(process_register::INVINCIBILITY_STATE, 4)
                .unwrap();
            sender_object
                .set_register(
                    process_register::STATUS_B,
                    crate::retail_physics::STATUS_B_TRANSLATION_MOTION,
                )
                .unwrap();
            sender_object
                .set_register(process_register::MISC_A_X, 1_024)
                .unwrap();
            sender_object.set_link(6, Some(collider.vm)).unwrap();
        }
        {
            let collider_object = runtime.machine.object_mut(collider.vm).unwrap();
            collider_object.configure_test_program_identity(0x300);
            collider_object
                .configure_test_event_interrupt(HIT_INVINCIBLE_EVENT, vec![0xff00_0000])
                .unwrap();
        }
        let _discarded_spawn_effects = runtime.machine.take_effects();

        runtime
            .finish_native_object_update(sender, &mut SnapshotHost, &mut Vec::new())
            .unwrap();

        assert_eq!(
            runtime
                .machine
                .object(sender.vm)
                .unwrap()
                .register(process_register::TRANSLATION_X),
            Ok(34)
        );
        assert_eq!(
            runtime.take_invincibility_event_faults(),
            [RuntimeInvincibilityEventFault {
                sender,
                recipient: collider,
                event: HIT_INVINCIBLE_EVENT,
            }]
        );
        assert!(runtime.machine.effects().is_empty());
    }

    #[test]
    fn native_finish_rejects_a_reused_vm_slot_from_a_stale_arena_generation() {
        let mut runtime = RetailRuntime::new(0);
        let original = spawn_test_object(&mut runtime, ZONE, 0x99, 2, 0);
        runtime
            .reclaim_runtime_subtree(original.arena, &mut SnapshotHost, &mut Vec::new())
            .unwrap();
        let replacement = spawn_test_object(&mut runtime, ZONE, 0x9a, 2, 0);
        assert_eq!(replacement.vm, original.vm);
        assert_ne!(replacement.arena, original.arena);
        let replacement_colors = [0x0333; COLOR_COUNT];
        {
            let replacement_object = runtime.machine.object_mut(replacement.vm).unwrap();
            replacement_object.set_retail_colors(replacement_colors);
            replacement_object
                .set_register(process_register::INVINCIBILITY_STATE, 2)
                .unwrap();
        }

        runtime
            .finish_native_object_update(original, &mut SnapshotHost, &mut Vec::new())
            .unwrap();

        let replacement_object = runtime.machine.object(replacement.vm).unwrap();
        assert_eq!(replacement_object.retail_colors(), &replacement_colors);
        assert_eq!(
            replacement_object.register(process_register::INVINCIBILITY_STATE),
            Ok(2)
        );
    }

    #[test]
    fn neighboring_water_event_observes_the_new_zone_before_dispatch() {
        const DROWN_EVENT: u32 = 0x2100;
        let mut host = InlineZoneHost {
            selected: None,
            zone_b_graphics: 4,
            zone_b_water_y: 20_000,
            quirks: SolidLevelQuirks {
                lethal_river_water: true,
                ..SolidLevelQuirks::default()
            },
            environment_calls: Vec::new(),
            state_bindings: Vec::new(),
        };
        let mut runtime = RetailRuntime::new(0);
        let object = spawn_test_object(&mut runtime, ZONE, 0x95, 2, 0);
        let mut vm = VmObject::new(object.vm, vec![RETURN]).unwrap();
        vm.bind_retail_solid_environment(host.environment(ZONE));
        vm.configure_test_event_state(DROWN_EVENT, 7);
        vm.set_register(
            process_register::STATUS_B,
            crate::retail_physics::STATUS_B_TRANSLATION_MOTION
                | crate::retail_physics::STATUS_B_STOPPED_BY_SOLID,
        )
        .unwrap();
        vm.set_register(process_register::TRANSLATION_X, 150 * 0x100)
            .unwrap();
        vm.set_register(process_register::TRANSLATION_Y, 10_000)
            .unwrap();
        vm.set_register(process_register::TRANSLATION_Z, 50 * 0x100)
            .unwrap();
        vm.set_register(process_register::MISC_A_X, 1_024).unwrap();
        runtime.machine.upsert_object(vm).unwrap();

        let frame = runtime.run_frame(&mut host, 8).unwrap();

        assert!(frame.executions[0].result.is_ok(), "{frame:?}");
        assert_eq!(host.state_bindings, [ZONE_B]);
        assert_eq!(runtime.arena.get(object.arena).unwrap().zone(), ZONE_B);
        assert_eq!(
            runtime
                .machine
                .object(object.vm)
                .unwrap()
                .retail_solid_zone_eid(),
            Some(ZONE_B)
        );
    }

    #[test]
    fn inline_szon_migration_changes_the_remaining_outside_zone_branch() {
        const DROWN_EVENT: u32 = 0x2100;
        const FALL_KILL_EVENT: u32 = 0x0900;
        let mut host = InlineZoneHost {
            selected: Some(ZONE_B),
            zone_b_graphics: 0,
            zone_b_water_y: i32::MIN,
            quirks: SolidLevelQuirks {
                drown_when_below_zone: true,
                ..SolidLevelQuirks::default()
            },
            environment_calls: Vec::new(),
            state_bindings: Vec::new(),
        };
        let mut runtime = RetailRuntime::new(0);
        runtime.set_level_state_context(level_context(ZONE, false, vec![ZONE, ZONE_B]));
        let object = spawn_test_object(&mut runtime, ZONE, 0x96, 2, 0);
        let mut vm = VmObject::new(object.vm, vec![RETURN]).unwrap();
        vm.bind_retail_solid_environment(host.environment(ZONE));
        vm.configure_test_event_interrupt(
            DROWN_EVENT,
            vec![misc(9, 0, 0x0e00) | (3 << 12), 0x8280_0000],
        )
        .unwrap();
        vm.set_link(3, Some(object.vm)).unwrap();
        vm.set_register(
            process_register::STATUS_B,
            crate::retail_physics::STATUS_B_TRANSLATION_MOTION
                | crate::retail_physics::STATUS_B_STOPPED_BY_SOLID,
        )
        .unwrap();
        vm.set_register(process_register::TRANSLATION_X, 50 * 0x100)
            .unwrap();
        vm.set_register(process_register::TRANSLATION_Y, 100)
            .unwrap();
        vm.set_register(process_register::TRANSLATION_Z, 50 * 0x100)
            .unwrap();
        vm.set_register(process_register::MISC_A_Y, (-4_000_i32) as u32)
            .unwrap();
        runtime.machine.upsert_object(vm).unwrap();

        let frame = runtime.run_frame(&mut host, 8).unwrap();

        assert!(frame.executions[0].result.is_ok(), "{frame:?}");
        let delivered = frame
            .effects
            .iter()
            .filter_map(|effect| match effect {
                VmEffect::Solid {
                    object: moving,
                    effect: SolidEffect::SendEvent { event, .. },
                } if *moving == object.vm => Some(*event),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(delivered, [DROWN_EVENT]);
        assert!(!delivered.contains(&FALL_KILL_EVENT));
        assert_eq!(runtime.arena.get(object.arena).unwrap().zone(), ZONE_B);
        assert_eq!(
            runtime
                .machine
                .object(object.vm)
                .unwrap()
                .retail_solid_zone_eid(),
            Some(ZONE_B)
        );
        assert!(host.environment_calls.contains(&ZONE_B));
    }

    #[test]
    fn outside_zone_event_runs_before_the_final_translation_commit() {
        const FALL_KILL_EVENT: u32 = 0x0900;

        let zone = RetailSolidZone::new([0; 3], [100; 3], 0, [0; 3], vec![0; 36])
            .unwrap()
            .with_eid(ZONE)
            .with_graphics(2, i32::MIN);
        let environment = RetailSolidEnvironment::new(0, [0; 24], [0; 24], vec![zone])
            .with_runtime_context(Some(ZONE), SolidLevelQuirks::default());
        let mut runtime = RetailRuntime::new(0);
        let object = spawn_test_object(&mut runtime, ZONE, 0x93, 2, 0);
        let mut vm = VmObject::new(object.vm, vec![RETURN]).unwrap();
        vm.bind_retail_solid_environment(environment);
        vm.configure_test_event_state(FALL_KILL_EVENT, 7);
        vm.set_register(
            process_register::STATUS_B,
            crate::retail_physics::STATUS_B_TRANSLATION_MOTION
                | crate::retail_physics::STATUS_B_STOPPED_BY_SOLID,
        )
        .unwrap();
        vm.set_register(process_register::TRANSLATION_Y, 100)
            .unwrap();
        vm.set_register(process_register::MISC_A_Y, (-4_000_i32) as u32)
            .unwrap();
        runtime.machine.upsert_object(vm).unwrap();
        let mut host = AudioRecordingHost::new(Some(event_transition_state()));

        let frame = runtime.run_frame(&mut host, 1).unwrap();

        assert!(frame.executions[0].result.is_ok(), "{frame:?}");
        let vm = runtime.machine.object(object.vm).unwrap();
        assert_eq!(vm.state(), 7);
        assert_eq!(vm.register(process_register::EVENT), Ok(FALL_KILL_EVENT));
        assert_eq!(
            vm.register(process_register::TRANSLATION_X),
            Ok(0),
            "TransSmoothStopAtSolid commits its local next_trans after the inline handler"
        );
        assert!(frame.effects.iter().any(|effect| {
            matches!(
                effect,
                VmEffect::Solid {
                    object: effect_object,
                    effect: SolidEffect::SendEvent {
                        target: SolidEventTarget::MovingObject,
                        event: FALL_KILL_EVENT,
                        argument: 0x6400,
                        ..
                    },
                } if *effect_object == object.vm
            )
        }));
    }

    #[test]
    fn inline_drown_handler_changes_the_following_outside_zone_branch() {
        const DROWN_EVENT: u32 = 0x2100;
        const FALL_KILL_EVENT: u32 = 0x0900;

        let zone = RetailSolidZone::new([0; 3], [100; 3], 0, [0; 3], vec![0; 36])
            .unwrap()
            .with_eid(ZONE)
            .with_graphics(2, i32::MIN);
        let environment = RetailSolidEnvironment::new(0, [0; 24], [0; 24], vec![zone])
            .with_runtime_context(
                Some(ZONE),
                SolidLevelQuirks {
                    drown_when_below_zone: true,
                    ..SolidLevelQuirks::default()
                },
            );
        let mut runtime = RetailRuntime::new(0);
        let object = spawn_test_object(&mut runtime, ZONE, 0x95, 2, 0);
        let mut vm = VmObject::new(object.vm, vec![RETURN]).unwrap();
        vm.bind_retail_solid_environment(environment);
        vm.configure_test_event_state(DROWN_EVENT, 7);
        vm.set_register(
            process_register::STATUS_B,
            crate::retail_physics::STATUS_B_TRANSLATION_MOTION
                | crate::retail_physics::STATUS_B_STOPPED_BY_SOLID,
        )
        .unwrap();
        vm.set_register(process_register::TRANSLATION_Y, 100)
            .unwrap();
        vm.set_register(process_register::MISC_A_Y, (-4_000_i32) as u32)
            .unwrap();
        runtime.machine.upsert_object(vm).unwrap();
        let mut host = AudioRecordingHost::new(Some(invincibility_two_transition_state()));

        let frame = runtime.run_frame(&mut host, 8).unwrap();

        assert!(frame.executions[0].result.is_ok(), "{frame:?}");
        let vm = runtime.machine.object(object.vm).unwrap();
        assert_eq!(vm.register(process_register::INVINCIBILITY_STATE), Ok(2));
        assert_eq!(vm.register(process_register::TRANSLATION_Y), Ok(0));
        let delivered = frame
            .effects
            .iter()
            .filter_map(|effect| match effect {
                VmEffect::Solid {
                    object: moving,
                    effect: SolidEffect::SendEvent { event, .. },
                } if *moving == object.vm => Some(*event),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(delivered, [DROWN_EVENT]);
        assert!(!delivered.contains(&FALL_KILL_EVENT));
    }

    #[test]
    fn malformed_solid_event_handler_is_reported_without_faulting_the_mover() {
        const FALL_KILL_EVENT: u32 = 0x0900;

        let zone = RetailSolidZone::new([0; 3], [100; 3], 0, [0; 3], vec![0; 36])
            .unwrap()
            .with_eid(ZONE)
            .with_graphics(2, i32::MIN);
        let environment = RetailSolidEnvironment::new(0, [0; 24], [0; 24], vec![zone])
            .with_runtime_context(Some(ZONE), SolidLevelQuirks::default());
        let mut runtime = RetailRuntime::new(0);
        let object = spawn_test_object(&mut runtime, ZONE, 0x94, 2, 0);
        let mut vm = VmObject::new(object.vm, vec![RETURN]).unwrap();
        vm.bind_retail_solid_environment(environment);
        vm.configure_test_event_interrupt(FALL_KILL_EVENT, vec![0xff00_0000])
            .unwrap();
        vm.set_register(
            process_register::STATUS_B,
            crate::retail_physics::STATUS_B_TRANSLATION_MOTION
                | crate::retail_physics::STATUS_B_STOPPED_BY_SOLID,
        )
        .unwrap();
        vm.set_register(process_register::TRANSLATION_Y, 100)
            .unwrap();
        vm.set_register(process_register::MISC_A_Y, (-4_000_i32) as u32)
            .unwrap();
        runtime.machine.upsert_object(vm).unwrap();

        let frame = runtime.run_frame(&mut SnapshotHost, 1).unwrap();

        assert!(frame.executions[0].result.is_ok(), "{frame:?}");
        assert!(!runtime.is_object_faulted(object));
        assert_eq!(
            runtime.take_solid_event_faults(),
            [RuntimeSolidEventFault {
                moving_object: object,
                recipient: object,
                event: FALL_KILL_EVENT,
                reason: SolidEventReason::OutsideZone,
            }]
        );
    }

    fn event_transition_state() -> VmStateProgram {
        VmStateProgram::new(
            7,
            GoolState {
                flags: 0,
                status_c: 0,
                external_index: 0,
                event_pc: GOOL_PC_NONE,
                transition_pc: 0,
                code_pc: GOOL_PC_NONE,
            },
            vec![Instruction::encode(0x11, 0x0805, 0x0e08), 0x8280_0000],
            Vec::new(),
        )
        .unwrap()
    }

    fn invincibility_two_transition_state() -> VmStateProgram {
        let stack = 0x0e1f;
        let invincibility = 0x0e00 | process_register::INVINCIBILITY_STATE as u16;
        VmStateProgram::new(
            7,
            GoolState {
                flags: 0,
                status_c: 0,
                external_index: 0,
                event_pc: GOOL_PC_NONE,
                transition_pc: 0,
                code_pc: GOOL_PC_NONE,
            },
            vec![
                Instruction::encode(0x04, 0x0800, 0x0800),
                Instruction::encode(0x04, 0x0800, 0x0800),
                Instruction::encode(0x00, stack, stack),
                Instruction::encode(0x11, stack, invincibility),
                0x8280_0000,
            ],
            Vec::new(),
        )
        .unwrap()
    }

    #[test]
    fn self_and_nested_back_send_run_the_outer_current_objects_transition() {
        const OUTER_EVENT: u32 = 0x1500;
        const STATE_EVENT: u32 = 0x0f00;

        let mut runtime = RetailRuntime::new(0);
        let object = spawn_test_object(&mut runtime, ZONE, 220, 2, 0);
        let mut vm = VmObject::new(object.vm, vec![0x8700_080f, RETURN]).unwrap();
        vm.configure_test_event_state(STATE_EVENT, 7);
        runtime.machine.upsert_object(vm).unwrap();
        let mut host = AudioRecordingHost::new(Some(event_transition_state()));

        let frame = runtime.run_frame(&mut host, 2).unwrap();

        assert!(frame.executions[0].result.is_ok(), "{frame:?}");
        assert_eq!(
            runtime.machine.object(object.vm).unwrap().register(8),
            Ok(0x500),
            "a direct self-send must enter the rebound transition before KEEP cleanup"
        );

        let mut runtime = RetailRuntime::new(0);
        let outer = spawn_test_object(&mut runtime, ZONE, 221, 2, 0);
        let nested = spawn_test_object(&mut runtime, ZONE, 222, 2, 0);
        let mut outer_vm = VmObject::new(outer.vm, vec![0x8720_0815, RETURN]).unwrap();
        outer_vm.set_link(1, Some(nested.vm)).unwrap();
        outer_vm.configure_test_event_state(STATE_EVENT, 7);
        runtime.machine.upsert_object(outer_vm).unwrap();
        let mut nested_vm = VmObject::new(nested.vm, vec![RETURN]).unwrap();
        nested_vm.set_link(1, Some(outer.vm)).unwrap();
        nested_vm
            .configure_test_event_interrupt(OUTER_EVENT, vec![0x8720_080f, 0x8280_0000])
            .unwrap();
        runtime.machine.upsert_object(nested_vm).unwrap();
        let mut host = AudioRecordingHost::new(Some(event_transition_state()));

        let frame = runtime.run_frame(&mut host, 2).unwrap();

        assert!(frame.executions[0].result.is_ok(), "{frame:?}");
        assert_eq!(
            runtime.machine.object(outer.vm).unwrap().register(8),
            Ok(0x500),
            "cur_obj must remain the outer updater across a nested recipient back-send"
        );
        assert!(!runtime.faulted_objects.contains(&outer));
        assert!(!runtime.faulted_objects.contains(&nested));
    }

    #[test]
    fn direct_send_delivers_argv_and_empty_opcode_argv_is_non_null_and_contained() {
        const EVENT: u32 = 0x0f00;
        const EARG_ZERO: u32 = 0x1c00_5b7f;

        let mut runtime = RetailRuntime::new(0);
        let recipient = spawn_test_object(&mut runtime, ZONE, 230, 2, 0);
        let sender = spawn_test_object(&mut runtime, ZONE, 231, 2, 0);
        runtime
            .machine
            .object_mut(recipient.vm)
            .unwrap()
            .configure_test_event_interrupt(
                EVENT,
                vec![Instruction::encode(0x11, 0x0b7f, 0x0e08), 0x8280_0000],
            )
            .unwrap();
        let mut sender_vm =
            VmObject::new(sender.vm, vec![0x16be_0804, 0x8724_080f, RETURN]).unwrap();
        sender_vm.set_link(1, Some(recipient.vm)).unwrap();
        runtime.machine.upsert_object(sender_vm).unwrap();

        runtime.run_frame(&mut SnapshotHost, 3).unwrap();

        assert_eq!(
            runtime.machine.object(recipient.vm).unwrap().register(8),
            Ok(0x400)
        );
        assert!(
            runtime
                .machine
                .object(sender.vm)
                .unwrap()
                .stack()
                .is_empty()
        );

        let mut runtime = RetailRuntime::new(0);
        let recipient = spawn_test_object(&mut runtime, ZONE, 232, 2, 0);
        let mut recipient_vm = VmObject::new(
            recipient.vm,
            vec![
                EARG_ZERO,
                Instruction::encode(0x11, 0x0e1f, 0x0e08),
                event_service_return(),
                RETURN,
            ],
        )
        .unwrap();
        recipient_vm
            .configure_test_event_service(
                vec![
                    EARG_ZERO,
                    Instruction::encode(0x11, 0x0e1f, 0x0e08),
                    event_service_return(),
                    RETURN,
                ],
                0,
            )
            .unwrap();
        recipient_vm.restart(3).unwrap();
        runtime.machine.upsert_object(recipient_vm).unwrap();

        runtime
            .dispatch_event(&mut SnapshotHost, None, Some(recipient), EVENT, None)
            .unwrap();
        assert_eq!(
            runtime.machine.object(recipient.vm).unwrap().register(8),
            Ok(0)
        );
        runtime
            .machine
            .object_mut(recipient.vm)
            .unwrap()
            .set_register(8, 0xdead_beef)
            .unwrap();
        assert!(
            runtime
                .dispatch_event(&mut SnapshotHost, None, Some(recipient), EVENT, Some(&[]),)
                .is_err(),
            "Some(&[]) must install a real zero-length argv token"
        );
        assert_eq!(
            runtime.machine.object(recipient.vm).unwrap().register(8),
            Ok(0xdead_beef)
        );

        let sender = spawn_test_object(&mut runtime, ZONE, 233, 2, 0);
        let mut sender_vm = VmObject::new(
            sender.vm,
            vec![
                0x8720_080f,
                Instruction::encode(0x11, 0x0805, 0x0e09),
                RETURN,
            ],
        )
        .unwrap();
        sender_vm.set_link(1, Some(recipient.vm)).unwrap();
        runtime.machine.upsert_object(sender_vm).unwrap();
        runtime
            .arena
            .reparent_to_root(sender.arena, RootHandle::new(0).unwrap())
            .unwrap();
        runtime
            .arena
            .reparent_to_root(recipient.arena, RootHandle::new(7).unwrap())
            .unwrap();
        RetailRuntime::refresh_tree_links::<()>(
            &runtime.arena,
            &runtime.handles,
            &mut runtime.machine,
        )
        .unwrap();

        runtime.run_frame(&mut SnapshotHost, 3).unwrap();

        assert_eq!(
            runtime.machine.object(sender.vm).unwrap().register(9),
            Ok(0x500)
        );
        assert!(
            !runtime.faulted_objects.contains(&sender),
            "direct GoolSendEvent errors are ignored by opcode 0x87"
        );
    }

    fn audio_request_objects(requests: &[AudioHostRequest]) -> Vec<VmObjectHandle> {
        requests
            .iter()
            .map(|request| match request {
                AudioHostRequest::CreateVoice(request) => request.object,
                AudioHostRequest::Control(_) => panic!("event test emitted audio control"),
            })
            .collect()
    }

    #[test]
    fn send_event_mode_four_applies_category_and_asymmetric_face_bounds() {
        const EVENT: u32 = 0x0f00;
        let local_bound = Bounds3 {
            min: Vec3 { x: 0, y: 0, z: 0 },
            max: Vec3 {
                x: 10,
                y: 10,
                z: 10,
            },
        };
        let mut runtime = RetailRuntime::new(0);
        let accepted = spawn_test_object(&mut runtime, ZONE, 240, 2, 0);
        let rejected_face = spawn_test_object(&mut runtime, ZONE, 241, 2, 0);
        let wrong_category = spawn_test_object(&mut runtime, ZONE, 242, 2, 0);
        let sender = spawn_test_object(&mut runtime, ZONE, 243, 2, 0);
        let mut sender_vm = VmObject::new(sender.vm, vec![0x8f80_080f, RETURN]).unwrap();
        sender_vm.set_retail_local_bound(local_bound);
        runtime.machine.upsert_object(sender_vm).unwrap();

        for (object, category, translation) in [
            (accepted, 0x300, [10, 0, 0]),
            (rejected_face, 0x400, [-10, 0, 0]),
            (wrong_category, 0x200, [0, 0, 0]),
        ] {
            let vm = runtime.machine.object_mut(object.vm).unwrap();
            vm.configure_test_program_identity(category);
            vm.set_retail_local_bound(local_bound);
            set_test_translation(vm, translation);
            vm.configure_test_event_interrupt(EVENT, vec![audio_create(), 0x8280_0000])
                .unwrap();
            prepare_audio_registers(&mut runtime, object);
        }
        let mut host = AudioRecordingHost::new(None);

        runtime.run_frame(&mut host, 2).unwrap();

        assert_eq!(audio_request_objects(&host.requests), [accepted.vm]);
        assert!(!runtime.faulted_objects.contains(&sender));

        let mut runtime = RetailRuntime::new(0);
        let wrong_category = spawn_test_object(&mut runtime, ZONE, 244, 2, 0);
        let sender = spawn_test_object(&mut runtime, ZONE, 245, 2, 0);
        runtime
            .machine
            .object_mut(wrong_category.vm)
            .unwrap()
            .configure_test_program_identity(0x200);
        let mut sender_vm = VmObject::new(sender.vm, vec![0x8f80_080f, RETURN]).unwrap();
        sender_vm
            .set_register(process_register::MISC_VALUE, 0xdead_beef)
            .unwrap();
        runtime.machine.upsert_object(sender_vm).unwrap();

        runtime.run_frame(&mut SnapshotHost, 2).unwrap();

        assert_eq!(
            runtime
                .machine
                .object(sender.vm)
                .unwrap()
                .register(process_register::MISC_VALUE),
            Ok(0xdead_beef),
            "an eligible broadcast with no matches preserves prior ACK/MISC"
        );
    }

    #[test]
    fn send_event_mode_five_uses_global_cadence_and_continues_after_handler_error() {
        const EVENT: u32 = 0x0f00;
        let local_bound = Bounds3 {
            min: Vec3 {
                x: -1,
                y: -1,
                z: -1,
            },
            max: Vec3 { x: 1, y: 1, z: 1 },
        };
        let mut runtime = RetailRuntime::new(0);
        let candidates = (0..11)
            .map(|index| spawn_test_object(&mut runtime, ZONE, 250 + index, 2, 0))
            .collect::<Vec<_>>();
        let sender = spawn_test_object(&mut runtime, ZONE, 270, 2, 0);
        for candidate in &candidates {
            let vm = runtime.machine.object_mut(candidate.vm).unwrap();
            vm.configure_test_program_identity(0x300);
            vm.set_retail_local_bound(local_bound);
            vm.configure_test_event_interrupt(EVENT, vec![audio_create(), 0x8280_0000])
                .unwrap();
            prepare_audio_registers(&mut runtime, *candidate);
        }
        let order = runtime
            .arena
            .postorder_snapshot()
            .unwrap()
            .into_iter()
            .filter_map(|arena| runtime.handles.for_arena(arena))
            .filter(|object| candidates.contains(object))
            .collect::<Vec<_>>();
        assert_eq!(order.len(), 11);
        runtime
            .machine
            .object_mut(order[2].vm)
            .unwrap()
            .configure_test_event_interrupt(EVENT, vec![0xff00_0000])
            .unwrap();
        let mut sender_vm = VmObject::new(sender.vm, vec![0x8fa0_080f, RETURN]).unwrap();
        for register in [
            process_register::MISC_B_Y,
            process_register::MISC_B_X,
            process_register::MISC_B_Z,
        ] {
            sender_vm.set_register(register, 100).unwrap();
        }
        runtime.machine.upsert_object(sender_vm).unwrap();
        let mut host = AudioRecordingHost::new(None);

        runtime.run_frame(&mut host, 2).unwrap();

        assert_eq!(
            audio_request_objects(&host.requests),
            [order[0].vm, order[1].vm, order[5].vm, order[10].vm],
            "matching ordinals 1,2,3,6,11 are selected and failed ordinal 3 still advances count"
        );
        assert!(!runtime.faulted_objects.contains(&sender));
    }

    #[test]
    fn all_root_send_event_observes_live_reparenting_into_a_later_root() {
        const EVENT: u32 = 0x0f00;
        let mut runtime = RetailRuntime::new(0);
        let mover = spawn_test_object(&mut runtime, ZONE, 280, 2, 0);
        let sender = spawn_test_object(&mut runtime, ZONE, 281, 2, 0);
        let mover_vm = runtime.machine.object_mut(mover.vm).unwrap();
        mover_vm
            .set_register(TEST_SCALAR_REGISTER_C, 7 << 8)
            .unwrap();
        mover_vm
            .configure_test_event_interrupt(
                EVENT,
                vec![
                    audio_create(),
                    misc(12, 2, TEST_SCALAR_OPERAND_C),
                    0x8280_0000,
                ],
            )
            .unwrap();
        prepare_audio_registers(&mut runtime, mover);
        runtime
            .machine
            .upsert_object(VmObject::new(sender.vm, vec![0x8f00_080f, RETURN]).unwrap())
            .unwrap();
        runtime
            .arena
            .reparent_to_root(mover.arena, RootHandle::new(0).unwrap())
            .unwrap();
        runtime
            .arena
            .reparent_to_root(sender.arena, RootHandle::new(1).unwrap())
            .unwrap();
        RetailRuntime::refresh_tree_links::<()>(
            &runtime.arena,
            &runtime.handles,
            &mut runtime.machine,
        )
        .unwrap();
        let mut host = AudioRecordingHost::new(None);

        runtime.run_frame(&mut host, 2).unwrap();

        assert_eq!(
            audio_request_objects(&host.requests),
            [mover.vm, mover.vm],
            "root zero delivery moves the object into root seven, which the live cursor visits later"
        );
        assert_eq!(
            runtime.arena.get(mover.arena).unwrap().parent(),
            TreeParent::Root(RootHandle::new(7).unwrap())
        );
    }

    #[test]
    fn terminate_event_migration_survives_departure_but_not_hard_restart() {
        let mut runtime = RetailRuntime::new_for_level(0, LevelId::N_SANITY_BEACH);
        let object = spawn_test_object(&mut runtime, ZONE, 40, 2, 0);
        arm_zone_migration_terminate_handler(&mut runtime, object);

        let report = runtime
            .terminate_zone_objects(
                ZONE,
                ZoneTerminationMode::Departure { target: ZONE_B },
                &mut SnapshotHost,
            )
            .unwrap();
        assert!(report.terminated.is_empty(), "{report:?}");
        assert_eq!(report.migrated, [object]);
        assert!(report.event_failures.is_empty());
        assert_eq!(runtime.arena.get(object.arena).unwrap().zone(), ZONE_B);
        assert_eq!(runtime.object_for_vm(object.vm), Some(object));

        let mut runtime = RetailRuntime::new_for_level(0, LevelId::N_SANITY_BEACH);
        let object = spawn_test_object(&mut runtime, ZONE, 41, 2, 0);
        arm_zone_migration_terminate_handler(&mut runtime, object);
        let report = runtime
            .terminate_zone_objects(ZONE, ZoneTerminationMode::HardRestart, &mut SnapshotHost)
            .unwrap();
        assert_eq!(report.terminated, [object]);
        assert!(report.migrated.is_empty());
        assert!(runtime.object_for_vm(object.vm).is_none());
    }

    #[test]
    fn malformed_terminate_handler_is_reported_but_unchanged_object_is_killed() {
        let mut runtime = RetailRuntime::new(0);
        let object = spawn_test_object(&mut runtime, ZONE, 42, 2, 0);
        runtime
            .machine
            .object_mut(object.vm)
            .unwrap()
            .configure_test_event_interrupt(TERMINATE_EVENT, vec![0xff00_0000])
            .unwrap();

        let report = runtime
            .terminate_zone_objects(
                ZONE,
                ZoneTerminationMode::Departure { target: ZONE_B },
                &mut SnapshotHost,
            )
            .unwrap();
        assert_eq!(report.terminated, [object]);
        assert!(matches!(
            report.event_failures.as_slice(),
            [ZoneTerminationEventFailure {
                object: failed,
                error: RuntimeError::Vm(VmError::UnknownOpcode(0xff)),
            }] if *failed == object
        ));
        assert!(runtime.arena.get(object.arena).is_none());
        assert_eq!(
            runtime.machine.object(object.vm),
            Err(VmError::UnknownObject(object.vm))
        );
    }

    #[test]
    fn zone_termination_reads_live_vm_immunity_flags() {
        let mut runtime = RetailRuntime::new(0);
        let eligible = spawn_test_object(&mut runtime, ZONE, 43, 2, 0);
        let status_immune = spawn_test_object(&mut runtime, ZONE, 44, 2, 0);
        let state_immune = spawn_test_object(&mut runtime, ZONE, 45, 2, 0);
        runtime
            .machine
            .object_mut(status_immune.vm)
            .unwrap()
            .set_register(process_register::STATUS_B, ZONE_TERMINATION_STATUS_B_IMMUNE)
            .unwrap();
        runtime
            .machine
            .object_mut(state_immune.vm)
            .unwrap()
            .set_register(process_register::STATE_FLAGS, ZONE_TERMINATION_STATE_IMMUNE)
            .unwrap();
        assert_eq!(
            runtime.arena.get(state_immune.arena).unwrap().state_flags(),
            0
        );

        let report = runtime
            .terminate_zone_objects(
                ZONE,
                ZoneTerminationMode::Departure { target: ZONE_B },
                &mut SnapshotHost,
            )
            .unwrap();
        assert_eq!(report.terminated, [eligible]);
        assert_eq!(runtime.object_for_vm(status_immune.vm), Some(status_immune));
        assert_eq!(runtime.object_for_vm(state_immune.vm), Some(state_immune));
    }

    #[test]
    fn recursive_termination_cleans_every_runtime_registry_in_release_order() {
        let mut runtime = RetailRuntime::new(0);
        let parent = spawn_test_object(&mut runtime, ZONE, 46, 2, 0);
        let child = attach_test_child(&mut runtime, parent, ZONE, 3);
        let grandchild = attach_test_child(&mut runtime, child, ZONE, 4);
        for object in [child, grandchild] {
            runtime
                .machine
                .object_mut(object.vm)
                .unwrap()
                .set_register(process_register::STATE_FLAGS, ZONE_TERMINATION_STATE_IMMUNE)
                .unwrap();
        }
        for (index, object) in [parent, child, grandchild].into_iter().enumerate() {
            runtime.pending_states.insert(object.vm, index as u16);
            runtime.faulted_objects.insert(object);
            runtime
                .machine
                .register_frame_bound(object.vm, Bounds3::default())
                .unwrap();
        }

        let report = runtime
            .terminate_zone_objects(
                ZONE,
                ZoneTerminationMode::Departure { target: ZONE_B },
                &mut SnapshotHost,
            )
            .unwrap();
        assert_eq!(report.terminated, [grandchild, child, parent]);
        assert_eq!(
            report.cleanup_actions,
            [
                RuntimeCleanupAction::FreeObjectAudio(grandchild),
                RuntimeCleanupAction::FreeObjectAudio(child),
                RuntimeCleanupAction::FreeObjectAudio(parent),
            ]
        );
        assert!(runtime.arena.is_empty());
        assert!(runtime.pending_states.is_empty());
        assert!(runtime.faulted_objects.is_empty());
        assert!(runtime.machine.frame_bounds().is_empty());
        for object in [parent, child, grandchild] {
            assert!(runtime.object_for_arena(object.arena).is_none());
            assert!(runtime.object_for_vm(object.vm).is_none());
            assert_eq!(
                runtime.machine.object(object.vm),
                Err(VmError::UnknownObject(object.vm))
            );
        }
    }

    #[test]
    fn crash_is_immune_outside_title_and_released_on_title() {
        let mut gameplay = RetailRuntime::new_for_level(0, LevelId::N_SANITY_BEACH);
        let crash = spawn_test_object(&mut gameplay, ZONE, 47, 0, 0);
        let report = gameplay
            .terminate_zone_objects(
                ZONE,
                ZoneTerminationMode::Departure { target: ZONE_B },
                &mut SnapshotHost,
            )
            .unwrap();
        assert!(report.terminated.is_empty());
        assert_eq!(gameplay.object_for_vm(crash.vm), Some(crash));

        let mut title = RetailRuntime::new_for_level(0, LevelId::TITLE);
        let crash = spawn_test_object(&mut title, ZONE, 48, 0, 0);
        let report = title
            .terminate_zone_objects(
                ZONE,
                ZoneTerminationMode::Departure { target: ZONE_B },
                &mut SnapshotHost,
            )
            .unwrap();
        assert_eq!(report.terminated, [crash]);
        assert!(title.object_for_vm(crash.vm).is_none());
    }

    #[test]
    fn every_dedicated_main_special_is_crash_immune_outside_title() {
        let mut gameplay = RetailRuntime::new_for_level(0, LevelId::N_SANITY_BEACH);
        let special = spawn_test_object(&mut gameplay, ZONE, 1, 4, 8);
        assert!(special.arena.is_dedicated_main());

        let report = gameplay
            .terminate_zone_objects(
                ZONE,
                ZoneTerminationMode::Departure { target: ZONE_B },
                &mut SnapshotHost,
            )
            .unwrap();

        assert!(report.terminated.is_empty());
        assert_eq!(gameplay.object_for_vm(special.vm), Some(special));

        let mut title = RetailRuntime::new_for_level(0, LevelId::TITLE);
        let special = spawn_test_object(&mut title, ZONE, 1, 4, 8);
        let report = title
            .terminate_zone_objects(
                ZONE,
                ZoneTerminationMode::Departure { target: ZONE_B },
                &mut SnapshotHost,
            )
            .unwrap();
        assert_eq!(report.terminated, [special]);
    }

    fn configure_render_state(runtime: &mut RetailRuntime, object: RuntimeObjectHandle, seed: u8) {
        let vm_object = runtime.machine.object_mut(object.vm()).unwrap();
        vm_object.initialize_arguments(&[0; 12]).unwrap();
        let stack_origin = usize::try_from(vm_object.initial_stack_pointer()).unwrap();
        let stack_len = vm_object.stack().len();
        for index in 0..stack_len {
            vm_object
                .set_register(
                    stack_origin + index,
                    (u32::from(seed) << 16) | u32::try_from(index).unwrap(),
                )
                .unwrap();
        }
        vm_object.bind_animation_data(&[0; 16]);
        vm_object
            .set_register(
                process_register::ANIMATION_SEQUENCE,
                0xa700_0000 | u32::from(seed),
            )
            .unwrap();
        vm_object
            .set_register(process_register::ANIMATION_FRAME, u32::from(seed) << 8)
            .unwrap();
        vm_object
            .set_retail_transform(RetailTransform {
                translation: [i32::from(seed), -i32::from(seed), 0x1000 + i32::from(seed)],
                rotation_yxz: [0x100 + i32::from(seed), 0x200, 0x300],
                scale: [0x1000, -0x1000, 0x0800],
            })
            .unwrap();
        vm_object
            .set_register(process_register::STATUS_A, 0x1000_0000 | u32::from(seed))
            .unwrap();
        vm_object
            .set_register(process_register::STATUS_B, 0x2000_0000 | u32::from(seed))
            .unwrap();
        vm_object
            .set_register(process_register::STATUS_C, 0x3000_0000 | u32::from(seed))
            .unwrap();
        vm_object
            .set_register(process_register::STATE_FLAGS, 0x4000_0000 | u32::from(seed))
            .unwrap();
        vm_object
            .set_register(process_register::SIZE, (-i32::from(seed)) as u32)
            .unwrap();
        vm_object
            .set_register(
                process_register::INVINCIBILITY_STATE,
                0x0000_ab00 | u32::from(seed),
            )
            .unwrap();
        vm_object.set_retail_colors([u16::from(seed); COLOR_COUNT]);
    }

    struct LinkedParentRenderMutationHost;

    impl ProgramHost for LinkedParentRenderMutationHost {
        type Error = ();

        fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
            let parent_register = |register: usize| {
                0x0c40 | u16::try_from(register).expect("process register fits linked operand")
            };
            let mut child = VmObject::new(
                binding.object.vm(),
                vec![
                    Instruction::encode(
                        0x11,
                        0,
                        parent_register(process_register::INVINCIBILITY_STATE),
                    ),
                    Instruction::encode(
                        0x11,
                        1,
                        parent_register(process_register::ANIMATION_FRAME),
                    ),
                    Instruction::encode(0x11, 2, parent_register(process_register::TRANSLATION_X)),
                    Instruction::encode(0x11, 3, parent_register(process_register::STATUS_B)),
                    (0x24_u32 << 24) | (5 << 15) | (1 << 12) | 4,
                    RETURN,
                ],
            )
            .map_err(|_| ())?;
            for (index, value) in [
                0x0002_aa00,
                0x0000_9900,
                0x0000_0777,
                0x0000_0100,
                0x0000_0777,
            ]
            .into_iter()
            .enumerate()
            {
                child.set_internal(index, value).map_err(|_| ())?;
            }
            Ok(child)
        }

        fn bind_state_program(
            &mut self,
            _binding: StateProgramBinding,
        ) -> Result<VmStateProgram, Self::Error> {
            Err(())
        }
    }

    #[test]
    fn completed_frame_keeps_an_earlier_display_submission_after_later_teardown() {
        let mut runtime = RetailRuntime::new(0);
        let rendered_then_killed = spawn_test_object(&mut runtime, ZONE, 9, 2, 0);
        let main = spawn_test_object(&mut runtime, ZONE_B, 10, 0, 0);
        runtime
            .arena
            .reparent_to_root(rendered_then_killed.arena, RootHandle::new(2).unwrap())
            .unwrap();

        let mut hook_calls = 0;
        runtime
            .run_frame_with_traversal_hook(&mut SnapshotHost, 2, |runtime, host, boundary| {
                assert!(matches!(
                    boundary,
                    RetailTraversalBoundary::BeforeMainObjectUpdate { object, .. }
                        if object == main
                ));
                hook_calls += 1;
                runtime.terminate_zone_objects(
                    ZONE,
                    ZoneTerminationMode::Departure { target: ZONE_B },
                    host,
                )?;
                Ok(())
            })
            .unwrap();

        assert_eq!(hook_calls, 1);
        assert_eq!(runtime.object_for_arena(rendered_then_killed.arena), None);
        assert_eq!(runtime.object_for_vm(rendered_then_killed.vm), None);
        assert_eq!(
            runtime
                .render_objects()
                .unwrap()
                .into_iter()
                .map(|object| object.object)
                .collect::<Vec<_>>(),
            [rendered_then_killed, main],
            "a later teardown cannot retract an earlier native display submission"
        );
    }

    #[test]
    fn spawned_child_link_writes_cannot_retroactively_change_parent_render_snapshot() {
        const SPAWN_EXECUTABLE_FIVE_CHILD: u32 = 0x8a00_5001;
        let mut runtime = RetailRuntime::new(0);
        let parent = spawn_test_object(&mut runtime, ZONE, 10, 2, 0);
        let initial_transform = RetailTransform {
            translation: [0x111, 0x222, 0x333],
            rotation_yxz: [0x10, 0x20, 0x30],
            scale: [0x1000; 3],
        };
        let mut parent_vm =
            VmObject::new(parent.vm, vec![SPAWN_EXECUTABLE_FIVE_CHILD, RETURN]).unwrap();
        parent_vm.bind_animation_data(&[0; 16]);
        parent_vm
            .set_register(process_register::ANIMATION_SEQUENCE, 0xa700_0001)
            .unwrap();
        parent_vm
            .set_register(process_register::ANIMATION_FRAME, 0x2200)
            .unwrap();
        parent_vm
            .set_register(process_register::INVINCIBILITY_STATE, 0x0001_4600)
            .unwrap();
        parent_vm.set_retail_transform(initial_transform).unwrap();
        parent_vm.set_retail_colors([0x123; COLOR_COUNT]);
        runtime.machine.upsert_object(parent_vm).unwrap();

        let frame = runtime
            .run_frame(&mut LinkedParentRenderMutationHost, 6)
            .unwrap();
        assert_eq!(frame.spawned_children.len(), 1);
        let live_parent = runtime.machine.object(parent.vm).unwrap();
        assert_eq!(
            live_parent.register(process_register::INVINCIBILITY_STATE),
            Ok(0x0002_aa00)
        );
        assert_eq!(live_parent.animation_frame(), 0x9900);
        assert_eq!(
            live_parent.register(process_register::TRANSLATION_X),
            Ok(0x777)
        );
        assert_eq!(live_parent.register(process_register::STATUS_B), Ok(0x100));
        assert_eq!(live_parent.color(5), Ok(0x777));

        let parent_render = runtime
            .render_objects()
            .unwrap()
            .into_iter()
            .find(|render| render.object == parent)
            .unwrap();
        assert_eq!(parent_render.animation_reference.unwrap().offset(), 1);
        assert_eq!(parent_render.animation_frame, 0x2200);
        assert_eq!(parent_render.transform, initial_transform);
        assert_eq!(parent_render.status_b, 0);
        assert_eq!(parent_render.colors, [0x123; COLOR_COUNT]);
        assert_eq!(parent_render.text_font_override_word_offset, 0x146);
        assert!(parent_render.display_eligible);
    }

    fn arm_animation_bound(
        runtime: &mut RetailRuntime,
        object: RuntimeObjectHandle,
        frame_index: u32,
        transform: RetailTransform,
    ) {
        let vm_object = runtime.machine.object_mut(object.vm()).unwrap();
        vm_object.bind_animation_data(&[0; 16]);
        vm_object
            .set_register(process_register::ANIMATION_SEQUENCE, 0xa700_0001)
            .unwrap();
        vm_object
            .set_register(
                process_register::ANIMATION_FRAME,
                frame_index.wrapping_shl(8),
            )
            .unwrap();
        vm_object
            .set_register(process_register::STATUS_B, COLLIDABLE_STATUS_B)
            .unwrap();
        let status_a = vm_object.register(process_register::STATUS_A).unwrap();
        vm_object
            .set_register(
                process_register::STATUS_A,
                status_a | LOCAL_BOUND_INVALID_STATUS_A,
            )
            .unwrap();
        vm_object.set_retail_transform(transform).unwrap();
    }

    #[test]
    fn process_local_no_draw_animation_drives_presence_and_nonvertex_collision() {
        let mut runtime = RetailRuntime::new(0);
        let object = spawn_test_object(&mut runtime, ZONE, 10, 2, 1);
        let descriptor = crate::gool::StorageReference::checked(
            object.vm,
            crate::gool::StorageRegion::Register,
            65,
        )
        .unwrap();
        let transform = RetailTransform {
            translation: [100, 200, 300],
            rotation_yxz: [0; 3],
            scale: [0x1000; 3],
        };
        let vm_object = runtime.machine.object_mut(object.vm).unwrap();
        vm_object.set_register(65, 0).unwrap();
        vm_object
            .set_register(process_register::ANIMATION_SEQUENCE, descriptor.to_word())
            .unwrap();
        vm_object
            .set_register(process_register::STATUS_B, COLLIDABLE_STATUS_B)
            .unwrap();
        let status_a = vm_object.register(process_register::STATUS_A).unwrap();
        vm_object
            .set_register(
                process_register::STATUS_A,
                status_a | LOCAL_BOUND_INVALID_STATUS_A,
            )
            .unwrap();
        vm_object.set_retail_transform(transform).unwrap();
        let mut host = BoundHost::new(AnimationBoundSource::NonVertex);

        assert!(runtime.register_animation_bound(object, &mut host).unwrap());
        assert!(
            host.calls.is_empty(),
            "a process descriptor is consumed locally and never misread as an item-five offset"
        );
        assert_eq!(
            runtime.machine.frame_bounds(),
            [crate::object_bounds::FrameBound {
                object: object.vm,
                bound: Bounds3 {
                    min: Vec3 {
                        x: -51_100,
                        y: -51_000,
                        z: -50_900,
                    },
                    max: Vec3 {
                        x: 51_300,
                        y: 51_400,
                        z: 51_500,
                    },
                },
            }]
        );

        let render = runtime
            .render_objects()
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.object == object)
            .unwrap();
        assert!(matches!(
            render.animation_source,
            Some(AnimationSource::Process(_))
        ));
        assert_eq!(render.animation_reference, None);
        assert!(render.display_eligible);
    }

    #[test]
    fn process_local_vertex_animation_uses_its_model_for_bounds() {
        let mut runtime = RetailRuntime::new(0);
        let object = spawn_test_object(&mut runtime, ZONE, 10, 2, 1);
        let descriptor = crate::gool::StorageReference::checked(
            object.vm,
            crate::gool::StorageRegion::Register,
            65,
        )
        .unwrap();
        let model = Eid::from_name("model").unwrap();
        let transform = RetailTransform {
            translation: [100, 200, 300],
            rotation_yxz: [0; 3],
            scale: [0x1000; 3],
        };
        let vm_object = runtime.machine.object_mut(object.vm).unwrap();
        vm_object.set_register(65, 0x0001_0001).unwrap();
        vm_object.set_register(66, model.raw()).unwrap();
        vm_object
            .set_register(process_register::ANIMATION_SEQUENCE, descriptor.to_word())
            .unwrap();
        vm_object
            .set_register(process_register::ANIMATION_FRAME, 0x300)
            .unwrap();
        vm_object
            .set_register(process_register::STATUS_B, COLLIDABLE_STATUS_B)
            .unwrap();
        let status_a = vm_object.register(process_register::STATUS_A).unwrap();
        vm_object
            .set_register(
                process_register::STATUS_A,
                status_a | LOCAL_BOUND_INVALID_STATUS_A,
            )
            .unwrap();
        vm_object.set_retail_transform(transform).unwrap();
        let bound_source = AnimationBoundSource::Vertex {
            vertex_kind: ObjectVertexKind::Lit,
            serialized_bound: Bounds3 {
                min: Vec3 {
                    x: -0x1000,
                    y: -0x2000,
                    z: -0x3000,
                },
                max: Vec3 {
                    x: 0x4000,
                    y: 0x5000,
                    z: 0x6000,
                },
            },
            collision_center: Vec3 {
                x: 0x700,
                y: -0x800,
                z: 0x900,
            },
        };
        let mut host = BoundHost::new(bound_source);

        assert!(runtime.register_animation_bound(object, &mut host).unwrap());
        assert_eq!(host.calls.len(), 1);
        assert_eq!(
            host.calls[0].reference,
            AnimationBoundReference::Model(model)
        );
        assert_eq!(host.calls[0].frame_index, 3);
        assert_eq!(runtime.machine.frame_bounds().len(), 1);
        assert_eq!(
            runtime
                .machine
                .object(object.vm)
                .unwrap()
                .retail_local_bound(),
            calculate_local_bound(
                bound_source,
                Vec3 {
                    x: 0x1000,
                    y: 0x1000,
                    z: 0x1000,
                },
                false,
            )
        );
    }

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_i32(bytes: &mut [u8], offset: usize, value: i32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn fixture_entry(eid: Eid, entry_type: u32, items: &[Vec<u8>]) -> Vec<u8> {
        let table_end = 16 + (items.len() + 1) * 4;
        let total = table_end + items.iter().map(Vec::len).sum::<usize>();
        assert!(total.is_multiple_of(4));
        let mut bytes = vec![0; table_end];
        put_u32(&mut bytes, 0, ENTRY_MAGIC);
        put_u32(&mut bytes, 4, eid.raw());
        put_u32(&mut bytes, 8, entry_type);
        put_u32(&mut bytes, 12, u32::try_from(items.len()).unwrap());
        let mut cursor = table_end;
        for (index, item) in items.iter().enumerate() {
            put_u32(&mut bytes, 16 + index * 4, u32::try_from(cursor).unwrap());
            bytes.extend_from_slice(item);
            cursor += item.len();
        }
        put_u32(
            &mut bytes,
            16 + items.len() * 4,
            u32::try_from(cursor).unwrap(),
        );
        bytes
    }

    fn object_bound_stream_fixture(
        include_model_pte: bool,
        include_geometry_pte: bool,
    ) -> (Vec<u8>, Vec<u8>) {
        let global_eid = Eid::from_name("glob1").unwrap();
        let model_eid = Eid::from_name("model").unwrap();
        let geometry_eid = Eid::from_name("geo01").unwrap();
        let mut ptes = vec![global_eid];
        if include_geometry_pte {
            ptes.push(geometry_eid);
        }
        if include_model_pte {
            ptes.push(model_eid);
        }
        let ldat_offset = MODERN_NSD_HEADER_SIZE + ptes.len() * 8;
        let mut nsd_bytes = vec![0; ldat_offset + crust_formats::stream::LDAT_PREFIX_SIZE];
        put_u32(&mut nsd_bytes, 0x400, 1);
        put_u32(&mut nsd_bytes, 0x404, u32::try_from(ptes.len()).unwrap());
        for (index, eid) in ptes.into_iter().enumerate() {
            put_u32(&mut nsd_bytes, MODERN_NSD_HEADER_SIZE + index * 8, 1);
            put_u32(
                &mut nsd_bytes,
                MODERN_NSD_HEADER_SIZE + index * 8 + 4,
                eid.raw(),
            );
        }
        put_u32(&mut nsd_bytes, ldat_offset, 1);
        put_u32(&mut nsd_bytes, ldat_offset + 4, LevelId::TITLE.get());
        put_u32(&mut nsd_bytes, ldat_offset + 20 + 2 * 4, global_eid.raw());

        // Item five deliberately begins the type-one descriptor at byte one:
        // retail references are byte offsets and need not be word-aligned.
        let mut animation = vec![0xee, 1, 0, 1, 0];
        animation.extend_from_slice(&model_eid.raw().to_le_bytes());
        animation.extend_from_slice(&[0; 3]);
        let global = fixture_entry(
            global_eid,
            2,
            &[
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                animation,
            ],
        );

        let mut frame = vec![0; 76];
        put_u32(&mut frame, 0, 3);
        put_u32(&mut frame, 4, geometry_eid.raw());
        for (offset, value) in [
            (20, -0x1000),
            (24, -0x2000),
            (28, -0x3000),
            (32, 0x4000),
            (36, 0x5000),
            (40, 0x6000),
            (44, 0x0700),
            (48, -0x0800),
            (52, 0x0900),
        ] {
            put_i32(&mut frame, offset, value);
        }
        frame[56..74].copy_from_slice(&[
            128, 128, 128, 0, 0, 127, 129, 128, 128, 0, 0, 127, 128, 129, 128, 0, 0, 127,
        ]);
        let model = fixture_entry(model_eid, 1, &[frame]);

        let mut geometry_header = vec![0; 24];
        put_u32(&mut geometry_header, 0, 1);
        put_i32(&mut geometry_header, 4, 0x1000);
        put_i32(&mut geometry_header, 8, 0x1000);
        put_i32(&mut geometry_header, 12, 0x1000);
        put_u32(&mut geometry_header, 16, 1);
        let mut polygon = vec![0; 8];
        put_u16(&mut polygon, 0, 0);
        put_u16(&mut polygon, 2, 6);
        put_u16(&mut polygon, 4, 12);
        let geometry = fixture_entry(geometry_eid, 2, &[geometry_header, polygon]);

        let entries = [global, model, geometry];
        let table_end = 16 + (entries.len() + 1) * 4;
        let mut nsf_bytes = vec![0; NSF_PAGE_SIZE];
        put_u16(&mut nsf_bytes, 0, 0x1234);
        put_u32(&mut nsf_bytes, 4, 1);
        put_u32(&mut nsf_bytes, 8, u32::try_from(entries.len()).unwrap());
        let mut cursor = table_end;
        for (index, entry) in entries.iter().enumerate() {
            put_u32(
                &mut nsf_bytes,
                16 + index * 4,
                u32::try_from(cursor).unwrap(),
            );
            let end = cursor + entry.len();
            nsf_bytes[cursor..end].copy_from_slice(entry);
            cursor = end;
        }
        put_u32(
            &mut nsf_bytes,
            16 + entries.len() * 4,
            u32::try_from(cursor).unwrap(),
        );
        (nsd_bytes, nsf_bytes)
    }

    #[test]
    fn render_snapshot_is_eight_root_preorder_and_copies_exact_process_state() {
        let entities = [entity(10, 2, 1), entity(11, 3, 2), entity(12, 0, 7)];
        let neighbors = [NeighborZone {
            eid: ZONE,
            display_flags: 2,
            entities: &entities,
        }];
        let mut runtime = RetailRuntime::new(0);
        let attempts = runtime.spawn_current_zone_neighbors(&neighbors, &mut SnapshotHost);
        let first = *attempts[0].result.as_ref().unwrap();
        let second = *attempts[1].result.as_ref().unwrap();
        let main = *attempts[2].result.as_ref().unwrap();

        let child_arena = runtime
            .arena
            .create_child(second.arena(), ZONE, 7, 9, false)
            .unwrap();
        let child = runtime.handles.reserve::<()>(child_arena).unwrap();
        runtime
            .machine
            .upsert_object(VmObject::new(child.vm(), vec![RETURN]).unwrap())
            .unwrap();

        for (object, seed) in [(first, 1), (second, 2), (child, 3), (main, 4)] {
            configure_render_state(&mut runtime, object, seed);
        }

        let snapshots = runtime.render_objects().unwrap();
        assert_eq!(
            snapshots
                .iter()
                .map(|snapshot| snapshot.object)
                .collect::<Vec<_>>(),
            [second, child, first, main]
        );

        let snapshot = snapshots
            .iter()
            .find(|value| value.object == child)
            .unwrap();
        assert_eq!(snapshot.zone, ZONE);
        assert_eq!((snapshot.executable, snapshot.subtype), (7, 9));
        assert_eq!(snapshot.program, None);
        assert_eq!(snapshot.animation_reference.unwrap().offset(), 3);
        assert_eq!(snapshot.animation_frame, 3 << 8);
        assert_eq!(
            snapshot.transform,
            RetailTransform {
                translation: [3, -3, 0x1003],
                rotation_yxz: [0x103, 0x200, 0x300],
                scale: [0x1000, -0x1000, 0x0800],
            }
        );
        assert_eq!(snapshot.status_a, 0x1000_0003);
        assert_eq!(snapshot.status_b, 0x2000_0003);
        assert_eq!(snapshot.status_c, 0x3000_0003);
        assert_eq!(snapshot.state_flags, 0x4000_0003);
        assert_eq!(snapshot.size, -3);
        assert_eq!(snapshot.colors, [3; COLOR_COUNT]);
        assert_eq!(snapshot.text_font_override_word_offset, 0xab);
        assert_eq!(
            snapshot.text_arguments,
            std::array::from_fn(|index| Some((3_u32 << 16) | (14 - index) as u32))
        );
    }

    #[test]
    fn dark_reference_translation_is_captured_at_each_root_display_boundary() {
        let level = LevelId::new_const(0x28);
        let mut runtime = RetailRuntime::new_for_level(119, level);
        let before_main = spawn_test_object(&mut runtime, ZONE, 10, 2, 0);
        let main = spawn_test_object(&mut runtime, ZONE, 11, 0, 0);
        let after_main = spawn_test_object(&mut runtime, ZONE, 12, 2, 0);
        runtime
            .arena
            .reparent_to_root(after_main.arena, RootHandle::new(7).unwrap())
            .unwrap();

        let initial_translation = [0x1000, 0x2000, 0x3000];
        let updated_translation = [0x100, 0x2000, 0x3000];
        let mut main_vm = VmObject::new(
            main.vm,
            vec![
                Instruction::encode(
                    0x11,
                    0x0801,
                    0x0e00 | process_register::TRANSLATION_X as u16,
                ),
                RETURN,
            ],
        )
        .unwrap();
        main_vm
            .set_retail_transform(RetailTransform {
                translation: initial_translation,
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            })
            .unwrap();
        runtime.machine.upsert_object(main_vm).unwrap();

        runtime.run_frame(&mut SnapshotHost, 2).unwrap();

        let snapshots = runtime.render_objects().unwrap();
        let dark_reference = |object| {
            snapshots
                .iter()
                .find(|snapshot| snapshot.object == object)
                .unwrap()
                .dark_reference_translation
        };
        assert_eq!(dark_reference(before_main), Some(initial_translation));
        assert_eq!(dark_reference(main), Some(updated_translation));
        assert_eq!(dark_reference(after_main), Some(updated_translation));
    }

    fn run_vertex_display_fixture(
        shader_mode: u32,
        vertex_kind: ObjectVertexKind,
        status_b: u32,
        parent_translation: [i32; 3],
        display_mask: u32,
        object_zone: Eid,
        expect_shader: bool,
    ) -> (
        RetailRuntime,
        RuntimeObjectHandle,
        RuntimeObjectHandle,
        [u16; COLOR_COUNT],
        [u16; COLOR_COUNT],
    ) {
        let original_colors = std::array::from_fn(|index| 0x100 + index as u16);
        let zone_colors = std::array::from_fn(|index| 0x400 + index as u16);
        let player_colors = std::array::from_fn(|index| 0x700 + index as u16);
        let source = AnimationBoundSource::Vertex {
            vertex_kind,
            serialized_bound: Bounds3::default(),
            collision_center: Vec3::default(),
        };
        let mut host = DisplayShaderHost {
            source,
            solid: RetailSolidEnvironment::new(0, zone_colors, player_colors, Vec::new())
                .with_object_shader(shader_mode, 0),
            zone: RetailZoneEnvironment {
                origin: [0; 3],
                object_colors: zone_colors,
                player_colors,
                graphics_flags: 0,
            },
        };
        let entities = [entity(10, 2, 0), entity(11, 0, 0)];
        let neighbors = [NeighborZone {
            eid: ZONE,
            display_flags: 2,
            entities: &entities,
        }];
        let mut runtime = RetailRuntime::new_for_level(119, LevelId::new_const(0x28));
        let attempts = runtime.spawn_current_zone_neighbors(&neighbors, &mut host);
        let parent = *attempts[0].result.as_ref().unwrap();
        let main = *attempts[1].result.as_ref().unwrap();
        runtime.arena.set_zone(parent.arena, object_zone).unwrap();
        let child = attach_test_child(&mut runtime, parent, ZONE, 3);
        let read_parent_light_x = (0x23_u32 << 24) | (1 << 12);
        let mut child_vm = VmObject::new(child.vm, vec![read_parent_light_x, RETURN]).unwrap();
        child_vm.set_link(1, Some(parent.vm)).unwrap();
        child_vm.set_link(4, Some(parent.vm)).unwrap();
        child_vm.configure_test_program_identity_with_type(0x100, 0);
        runtime.machine.upsert_object(child_vm).unwrap();

        runtime
            .machine
            .object_mut(main.vm)
            .unwrap()
            .set_retail_transform(RetailTransform {
                translation: [0; 3],
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            })
            .unwrap();
        let parent_vm = runtime.machine.object_mut(parent.vm).unwrap();
        parent_vm.bind_animation_data(&[0; 8]);
        parent_vm
            .set_register(process_register::ANIMATION_SEQUENCE, 0xa700_0001)
            .unwrap();
        parent_vm
            .set_register(process_register::ANIMATION_FRAME, 0)
            .unwrap();
        parent_vm
            .set_register(process_register::STATUS_B, status_b)
            .unwrap();
        parent_vm
            .set_retail_transform(RetailTransform {
                translation: parent_translation,
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            })
            .unwrap();
        parent_vm.set_retail_display_colors(original_colors);

        runtime.set_level_state_context(level_context(ZONE, false, vec![ZONE]));
        runtime.set_transform_vectors_camera(RetailTransformVectorsCamera::from_retail_pose(
            [0; 3], [0; 3], 500,
        ));
        runtime
            .machine
            .set_global_word(CURRENT_DISPLAY_GLOBAL, display_mask)
            .unwrap();
        runtime.run_frame(&mut host, 4).unwrap();

        let expected = if expect_shader {
            apply_retail_object_zone_shader(
                shader_mode,
                vertex_kind,
                original_colors,
                zone_colors,
                -(parent_translation[2] >> 8),
                0,
                Some(ObjectDarkShaderInput {
                    reference_translation: [0; 3],
                    object_translation: parent_translation,
                    dark_distance: 1,
                }),
            )
            .unwrap()
            .unwrap()
            .colors
        } else {
            original_colors
        };
        (runtime, parent, child, expected, zone_colors)
    }

    #[test]
    fn mode_four_display_writeback_is_visible_to_child_in_same_preorder_frame() {
        let (runtime, parent, child, expected, _) = run_vertex_display_fixture(
            4,
            ObjectVertexKind::Lit,
            0,
            [600 << 8, 0, -600 << 8],
            INITIAL_DISPLAY_MASK,
            ZONE,
            true,
        );
        assert_eq!(runtime.level_shader.distance, 1);
        assert_eq!(
            runtime.machine.object(parent.vm).unwrap().retail_colors(),
            &expected
        );
        assert_eq!(
            runtime.machine.object(child.vm).unwrap().stack(),
            &[u32::from(expected[0])]
        );
        let snapshot = runtime
            .render_objects()
            .unwrap()
            .into_iter()
            .find(|object| object.object == parent)
            .unwrap();
        assert_eq!(snapshot.colors, expected);
    }

    #[test]
    fn inherited_zone_colors_follow_effective_render_colors_before_child_traversal() {
        let (runtime, parent, child, expected, zone_colors) = run_vertex_display_fixture(
            4,
            ObjectVertexKind::Lit,
            0x10_0000,
            [600 << 8, 0, -600 << 8],
            INITIAL_DISPLAY_MASK,
            ZONE,
            true,
        );
        assert_eq!(
            runtime.machine.object(parent.vm).unwrap().retail_colors(),
            &zone_colors
        );
        assert_eq!(
            runtime.machine.object(child.vm).unwrap().stack(),
            &[u32::from(zone_colors[0])]
        );
        let snapshot = runtime
            .render_objects()
            .unwrap()
            .into_iter()
            .find(|object| object.object == parent)
            .unwrap();
        assert_eq!(snapshot.colors, expected);
        assert_ne!(snapshot.colors, zone_colors);
    }

    #[test]
    fn inherited_colors_fall_back_from_null_object_zone_to_current_zone() {
        let (runtime, parent, child, expected, zone_colors) = run_vertex_display_fixture(
            4,
            ObjectVertexKind::Lit,
            0x10_0000,
            [600 << 8, 0, -600 << 8],
            INITIAL_DISPLAY_MASK,
            Eid::NONE,
            true,
        );
        assert_eq!(
            runtime.machine.object(parent.vm).unwrap().retail_colors(),
            &zone_colors
        );
        assert_eq!(
            runtime.machine.object(child.vm).unwrap().stack(),
            &[u32::from(zone_colors[0])]
        );
        let snapshot = runtime
            .render_objects()
            .unwrap()
            .into_iter()
            .find(|object| object.object == parent)
            .unwrap();
        assert_eq!(snapshot.colors, expected);
        assert_ne!(snapshot.colors, zone_colors);
    }

    #[test]
    fn native_vertex_display_gates_mode_four_color_side_effects() {
        let cases = [
            (
                ObjectVertexKind::Colored,
                0x200,
                -600 << 8,
                INITIAL_DISPLAY_MASK,
                false,
            ),
            (
                ObjectVertexKind::Lit,
                0x400,
                -600 << 8,
                INITIAL_DISPLAY_MASK,
                false,
            ),
            (
                ObjectVertexKind::Lit,
                0,
                -600 << 8,
                INITIAL_DISPLAY_MASK | 0x1_0000,
                false,
            ),
            (
                ObjectVertexKind::Lit,
                0,
                -400 << 8,
                INITIAL_DISPLAY_MASK,
                false,
            ),
            (
                ObjectVertexKind::Lit,
                0x4_0000,
                -400 << 8,
                INITIAL_DISPLAY_MASK,
                true,
            ),
        ];

        for (vertex_kind, status_b, translation_z, display_mask, expect_shader) in cases {
            let (runtime, parent, child, expected, _) = run_vertex_display_fixture(
                4,
                vertex_kind,
                status_b,
                [600 << 8, 0, translation_z],
                display_mask,
                ZONE,
                expect_shader,
            );
            assert_eq!(
                runtime.level_shader.distance,
                i32::from(expect_shader),
                "unexpected dark-distance side effect for {vertex_kind:?}, status {status_b:#x}"
            );
            assert_eq!(
                runtime.machine.object(parent.vm).unwrap().retail_colors(),
                &expected
            );
            assert_eq!(
                runtime.machine.object(child.vm).unwrap().stack(),
                &[u32::from(expected[0])]
            );
            let snapshot = runtime
                .render_objects()
                .unwrap()
                .into_iter()
                .find(|object| object.object == parent)
                .unwrap();
            assert_eq!(snapshot.colors, expected);
        }
    }

    #[test]
    fn modes_two_and_three_commit_native_live_color_results() {
        let original_colors = std::array::from_fn(|index| 0x100 + index as u16);
        for (mode, vertex_kind, changes_colors) in [
            (2, ObjectVertexKind::Lit, true),
            (3, ObjectVertexKind::Lit, true),
            (3, ObjectVertexKind::Colored, false),
        ] {
            let (runtime, parent, child, expected, _) = run_vertex_display_fixture(
                mode,
                vertex_kind,
                0,
                [600 << 8, 0, -600 << 8],
                INITIAL_DISPLAY_MASK,
                ZONE,
                true,
            );
            assert_eq!(runtime.level_shader.distance, 0);
            assert_eq!(expected != original_colors, changes_colors);
            assert_eq!(
                runtime.machine.object(parent.vm).unwrap().retail_colors(),
                &expected
            );
            assert_eq!(
                runtime.machine.object(child.vm).unwrap().stack(),
                &[u32::from(expected[0])]
            );
        }
    }

    #[test]
    fn fixed_level_shader_table_restarts_and_boulder_uses_half_values() {
        let fixed_level = LevelId::new_const(0x03);
        let mut fixed = RetailLevelShaderState::default();
        fixed.initialize(fixed_level);

        let fixed_values = (0..LEVEL_SHADER_TABLE_1.len() - 1)
            .map(|_| {
                assert_eq!(fixed.advance(fixed_level, 0, 0, None), None);
                fixed.effect_t
            })
            .collect::<Vec<_>>();
        assert_eq!(fixed_values, LEVEL_SHADER_TABLE_1[..83]);
        assert_eq!(fixed.sequence_index, 83);
        assert_eq!(fixed.effect_t, 40);
        assert_eq!(fixed.advance(fixed_level, 0, 0, None), None);
        assert_eq!((fixed.effect_t, fixed.sequence_index), (0, 1));

        let boulder_level = LevelId::new_const(0x13);
        let mut boulder = RetailLevelShaderState::default();
        boulder.initialize(boulder_level);
        let boulder_values = (0..LEVEL_SHADER_TABLE_1.len() - 1)
            .map(|_| {
                assert_eq!(boulder.advance(boulder_level, 0, 0, None), None);
                boulder.effect_t
            })
            .collect::<Vec<_>>();
        assert_eq!(
            boulder_values,
            LEVEL_SHADER_TABLE_1[..83]
                .iter()
                .map(|value| value >> 1)
                .collect::<Vec<_>>()
        );
        assert_eq!(&boulder_values[..6], &[0, 40, 81, 122, 163, 200]);
        assert_eq!(boulder.advance(boulder_level, 0, 0, None), None);
        assert_eq!((boulder.effect_t, boulder.sequence_index), (0, 1));
    }

    #[test]
    fn generator_shader_uses_the_seed_b_zero_two_frame_sequence() {
        let level = LevelId::new_const(0x05);
        let mut shader = RetailLevelShaderState::default();
        shader.initialize(level);
        assert_eq!(shader.random_seed_b, 0);

        let values = (0..4)
            .map(|_| {
                assert_eq!(shader.advance(level, 0, 0, None), None);
                shader.effect_t
            })
            .collect::<Vec<_>>();

        assert_eq!(values, [1_436, 823, 839, 856]);
        assert_eq!(shader.effect_t_target, 856);
        assert_eq!(shader.sequence_index, 0);
        assert_eq!(shader.random_seed_b, 0xd3dc_167e);
    }

    #[test]
    fn brio_seed_zero_triggers_on_frame_102_and_runs_pattern_one_to_sentinel() {
        let level = LevelId::new_const(0x1b);
        let mut shader = RetailLevelShaderState::default();
        shader.initialize(level);

        for frame in 1..=101 {
            assert_eq!(shader.advance(level, 6_145, 0, None), None, "frame {frame}");
            assert_eq!(shader.sequence_state, -1, "frame {frame}");
            assert_eq!((shader.clear_t, shader.effect_t), (0, 0));
        }

        let cue = shader
            .advance(level, 6_145, 0, None)
            .expect("the first seed-zero trigger at the cooldown boundary is audible");
        assert_eq!(shader.sequence_state, 1);
        assert_eq!(shader.sequence_index, 0);
        assert_eq!(shader.lightning_stamp, 6_145);
        assert_eq!(shader.previous_lightning_stamp, 0);
        assert_eq!(
            cue,
            RetailThunderCue {
                adio: Eid::from_name("lt3rA").unwrap(),
                pitch: 452,
                trigger: 4,
                volume_percent: 70,
                amplitude: 11_468,
            }
        );

        for (index, &expected) in LEVEL_SHADER_TABLE_2_B[..9].iter().enumerate() {
            assert_eq!(shader.advance(level, 6_145, 0, None), None);
            assert_eq!(shader.sequence_state, 1, "pattern frame {index}");
            assert_eq!((shader.clear_t, shader.effect_t), (expected, expected));
        }
        assert_eq!(shader.sequence_index, 9);
        assert_eq!(shader.advance(level, 6_145, 0, None), None);
        assert_eq!(shader.sequence_state, -1);
        assert_eq!((shader.clear_t, shader.effect_t), (0, 0));
    }

    #[test]
    fn storm_seed_zero_uses_weighted_pattern_four_and_cooldown_only_suppresses_cue() {
        let level = LevelId::new_const(0x22);
        let mut before_trigger = RetailLevelShaderState::default();
        before_trigger.initialize(level);
        for frame in 1..=101 {
            assert_eq!(
                before_trigger.advance(level, 6_144, 0, None),
                None,
                "frame {frame}"
            );
        }

        let mut cooled_down = before_trigger;
        let cue = cooled_down
            .advance(level, 6_145, 0, None)
            .expect("the threshold tick accepts the same visual trigger");
        assert_eq!(cooled_down.sequence_state, 4);
        assert_eq!(cue.adio, Eid::from_name("lt3rA").unwrap());

        let mut suppressed = before_trigger;
        assert_eq!(suppressed.advance(level, 6_144, 0, None), None);
        assert_eq!(suppressed.sequence_state, 4);
        assert_eq!(suppressed.sequence_index, 0);
        assert_eq!(suppressed.random_seed_b, 0x0079_7a9b);
        assert_eq!(suppressed.lightning_stamp, 0);
        assert_eq!(suppressed.previous_lightning_stamp, 0);

        for (index, &expected) in LEVEL_SHADER_TABLE_2_E[..15].iter().enumerate() {
            assert_eq!(suppressed.advance(level, 6_144, 0, None), None);
            assert_eq!(suppressed.sequence_state, 4, "pattern frame {index}");
            assert_eq!(
                (suppressed.clear_t, suppressed.effect_t),
                (expected, expected)
            );
        }
        assert_eq!(suppressed.advance(level, 6_144, 0, None), None);
        assert_eq!(suppressed.sequence_state, -1);
        assert_eq!((suppressed.clear_t, suppressed.effect_t), (0, 0));
    }

    #[test]
    fn dark2_world_illumination_prefers_doctor_then_crash_not_pause() {
        let level = LevelId::new_const(0x28);
        let mut runtime = RetailRuntime::new_for_level(119, level);
        let crash = spawn_test_object(&mut runtime, ZONE, 10, 0, 0);
        let doctor = spawn_test_object(&mut runtime, ZONE, 11, 2, 0);
        let pause = spawn_test_object(&mut runtime, ZONE, 12, 4, 4);
        let crash_translation = [0x1000, -0x2000, 0x3000];
        let doctor_translation = [-0x4000, 0x5000, -0x6000];
        let pause_translation = [0x7000, 0x8000, -0x9000];
        for (object, translation) in [
            (crash, crash_translation),
            (doctor, doctor_translation),
            (pause, pause_translation),
        ] {
            runtime
                .machine
                .object_mut(object.vm)
                .unwrap()
                .set_retail_transform(RetailTransform {
                    translation,
                    rotation_yxz: [0; 3],
                    scale: [0x1000; 3],
                })
                .unwrap();
        }
        let mut context = level_context(ZONE, false, vec![ZONE]);
        context.graphics_flags = 0x400;
        runtime.set_level_state_context(context);
        runtime
            .set_global_word(
                PAUSE_OBJECT_GLOBAL,
                CollisionObjectReference::new(pause.vm).to_word(),
            )
            .unwrap();
        runtime
            .set_global_word(
                DOCTOR_OBJECT_GLOBAL,
                CollisionObjectReference::new(doctor.vm).to_word(),
            )
            .unwrap();

        runtime.advance_level_shader_at(0).unwrap();
        assert_eq!(
            runtime.world_shader_snapshot().dark2_illumination,
            doctor_translation
        );

        let doctor_final_translation = [-0x4100, 0x5200, -0x6300];
        runtime
            .machine
            .object_mut(doctor.vm)
            .unwrap()
            .set_retail_transform(RetailTransform {
                translation: doctor_final_translation,
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            })
            .unwrap();
        runtime
            .reclaim_runtime_subtree(doctor.arena, &mut SnapshotHost, &mut Vec::new())
            .unwrap();
        assert_eq!(runtime.object_for_vm(doctor.vm), None);
        assert_eq!(
            runtime.global_word(DOCTOR_OBJECT_GLOBAL),
            Ok(CollisionObjectReference::new(doctor.vm).to_word())
        );
        runtime.advance_level_shader_at(0).unwrap();
        assert_eq!(
            runtime.world_shader_snapshot().dark2_illumination,
            doctor_final_translation,
            "a non-null native doctor pointer retains its freed pool slot's final translation"
        );

        let replacement = spawn_test_object(&mut runtime, ZONE, 13, 2, 0);
        assert_eq!(replacement.vm, doctor.vm, "the freed VM slot is reused");
        let replacement_translation = [0x1357, -0x2468, 0x369a];
        runtime
            .machine
            .object_mut(replacement.vm)
            .unwrap()
            .set_retail_transform(RetailTransform {
                translation: replacement_translation,
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            })
            .unwrap();
        runtime.advance_level_shader_at(0).unwrap();
        assert_eq!(
            runtime.world_shader_snapshot().dark2_illumination,
            replacement_translation,
            "native pool reuse makes the retained pointer observe the replacement slot"
        );

        runtime.set_global_word(DOCTOR_OBJECT_GLOBAL, 0).unwrap();
        runtime.advance_level_shader_at(0).unwrap();
        assert_eq!(
            runtime.world_shader_snapshot().dark2_illumination,
            crash_translation
        );
    }

    #[test]
    fn dark2_rejects_a_doctor_slot_that_was_never_initialized() {
        let level = LevelId::new_const(0x28);
        let mut runtime = RetailRuntime::new_for_level(119, level);
        spawn_test_object(&mut runtime, ZONE, 10, 0, 0);
        let mut context = level_context(ZONE, false, vec![ZONE]);
        context.graphics_flags = 0x400;
        runtime.set_level_state_context(context);
        let never_allocated = VmObjectHandle::new(95).unwrap();
        runtime
            .set_global_word(
                DOCTOR_OBJECT_GLOBAL,
                CollisionObjectReference::new(never_allocated).to_word(),
            )
            .unwrap();

        assert_eq!(
            runtime.advance_level_shader_at(0),
            Err(VmError::UnknownObject(never_allocated))
        );
    }

    #[test]
    fn dark2_retained_pointer_follows_physical_pool_reuse_not_vm_reuse() {
        let level = LevelId::new_const(0x28);
        let mut runtime = RetailRuntime::new_for_level(119, level);
        spawn_test_object(&mut runtime, ZONE, 20, 0, 0);
        let doctor = spawn_test_object(&mut runtime, ZONE, 21, 2, 0);
        let _spacer = spawn_test_object(&mut runtime, ZONE, 22, 2, 0);
        let later = spawn_test_object(&mut runtime, ZONE, 23, 2, 0);
        let doctor_translation = [-0x1100, 0x2200, -0x3300];
        runtime
            .machine
            .object_mut(doctor.vm)
            .unwrap()
            .set_retail_transform(RetailTransform {
                translation: doctor_translation,
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            })
            .unwrap();
        let mut context = level_context(ZONE, false, vec![ZONE]);
        context.graphics_flags = 0x400;
        runtime.set_level_state_context(context);
        runtime
            .set_global_word(
                DOCTOR_OBJECT_GLOBAL,
                CollisionObjectReference::new(doctor.vm).to_word(),
            )
            .unwrap();
        assert_eq!(runtime.retained_doctor_pool_pointer, None);

        runtime
            .reclaim_runtime_subtree(doctor.arena, &mut SnapshotHost, &mut Vec::new())
            .unwrap();
        runtime
            .reclaim_runtime_subtree(later.arena, &mut SnapshotHost, &mut Vec::new())
            .unwrap();

        let wrong_vm_reuse = spawn_test_object(&mut runtime, ZONE, 24, 2, 0);
        assert_eq!(wrong_vm_reuse.vm, doctor.vm);
        assert_eq!(wrong_vm_reuse.arena.slot(), later.arena.slot());
        assert_ne!(wrong_vm_reuse.arena.slot(), doctor.arena.slot());
        let wrong_translation = [0x4444, 0x5555, 0x6666];
        runtime
            .machine
            .object_mut(wrong_vm_reuse.vm)
            .unwrap()
            .set_retail_transform(RetailTransform {
                translation: wrong_translation,
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            })
            .unwrap();
        assert_eq!(
            runtime.retained_doctor_pool_pointer, None,
            "the VM slot must diverge before the shader ever caches the pointer"
        );
        runtime.advance_level_shader_at(0).unwrap();
        assert_eq!(
            runtime.world_shader_snapshot().dark2_illumination,
            doctor_translation,
            "compact VM reuse in another pool slot must not retarget a native pointer"
        );

        let physical_reuse = spawn_test_object(&mut runtime, ZONE, 25, 2, 0);
        assert_eq!(physical_reuse.arena.slot(), doctor.arena.slot());
        assert_ne!(physical_reuse.vm, doctor.vm);
        let physical_reuse_translation = [-0x7777, 0x8888, -0x9999];
        runtime
            .machine
            .object_mut(physical_reuse.vm)
            .unwrap()
            .set_retail_transform(RetailTransform {
                translation: physical_reuse_translation,
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            })
            .unwrap();
        runtime.advance_level_shader_at(0).unwrap();
        assert_eq!(
            runtime.world_shader_snapshot().dark2_illumination,
            physical_reuse_translation,
            "physical pool-slot reuse must retarget the retained native pointer"
        );

        runtime
            .set_global_word(
                DOCTOR_OBJECT_GLOBAL,
                CollisionObjectReference::new(wrong_vm_reuse.vm).to_word(),
            )
            .unwrap();
        runtime.advance_level_shader_at(0).unwrap();
        assert_eq!(
            runtime.world_shader_snapshot().dark2_illumination,
            wrong_translation,
            "a later assignment of the same tagged word must bind the current VM object"
        );
    }

    #[test]
    fn shader_session_carry_preserves_seed_and_bss_while_mount_reinitializes_data() {
        let level = LevelId::new_const(0x05);
        let mut runtime = RetailRuntime::new_for_level(119, level);
        let mut context = level_context(ZONE, false, vec![ZONE]);
        context.graphics_flags = 0x200;
        runtime.set_level_state_context(context.clone());
        runtime.advance_level_shader_at(0).unwrap();
        assert_eq!(runtime.level_shader.effect_t, 1_436);
        assert_eq!(runtime.level_shader.effect_t_target, 823);
        assert_eq!(runtime.level_shader.random_seed_b, 0x0000_3039);
        runtime.level_shader.dark2_shift_sub = 7;
        runtime.level_shader.ambient_target = -321;
        runtime.level_shader.lightning_stamp = 0x1234_5678;

        let mut carry = runtime.export_session_carry();
        assert_eq!(carry.random_seed_b(), 0x0000_3039);
        carry.set_random_seed_b(0x0000_3039);
        let mut mounted = RetailRuntime::new_from_session(119, level, carry).unwrap();
        assert_eq!(mounted.random_seed_b(), 0x0000_3039);
        assert_eq!(mounted.level_shader.effect_t, 0x800);
        assert_eq!(mounted.level_shader.effect_t_target, 823);
        assert_eq!(mounted.level_shader.sequence_index, 0);
        assert_eq!(mounted.level_shader.sequence_state, -1);
        assert_eq!(mounted.level_shader.random_seed_b, 0x0000_3039);
        assert_eq!(mounted.level_shader.dark2_shift_sub, 7);
        assert_eq!(mounted.level_shader.ambient_target, -321);
        assert_eq!(mounted.level_shader.lightning_stamp, 0x1234_5678);

        mounted.set_level_state_context(context);
        mounted.advance_level_shader_at(0).unwrap();
        assert_eq!(mounted.level_shader.effect_t, 1_452);
        assert_eq!(mounted.level_shader.effect_t_target, 856);
        assert_eq!(mounted.level_shader.random_seed_b, 0xd3dc_167e);
    }

    #[test]
    fn dark_level_shader_tracks_light_distance_and_pause_reference() {
        let level = LevelId::new_const(0x28);
        let mut runtime = RetailRuntime::new_for_level(119, level);
        let main = spawn_test_object(&mut runtime, ZONE, 10, 0, 0);
        let light = spawn_test_object(&mut runtime, ZONE, 11, 2, 0);
        let pause = spawn_test_object(&mut runtime, ZONE, 12, 4, 4);
        let main_translation = [0x1000, 0x2000, 0x3000];
        let pause_translation = [-0x4000, 0x5000, -0x6000];
        runtime
            .machine
            .object_mut(main.vm)
            .unwrap()
            .set_retail_transform(RetailTransform {
                translation: main_translation,
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            })
            .unwrap();
        runtime
            .machine
            .object_mut(pause.vm)
            .unwrap()
            .set_retail_transform(RetailTransform {
                translation: pause_translation,
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            })
            .unwrap();
        let mut context = level_context(ZONE, false, vec![ZONE]);
        context.graphics_flags = 0x400;
        runtime.set_level_state_context(context);

        let initial = runtime.render_objects().unwrap();
        assert!(initial.iter().all(|object| {
            object.dark_reference_translation == Some(main_translation) && object.dark_distance == 0
        }));

        runtime
            .set_global_word(
                LIGHT_SOURCE_OBJECT_GLOBAL,
                CollisionObjectReference::new(light.vm).to_word(),
            )
            .unwrap();
        runtime.advance_level_shader().unwrap();
        assert!(
            runtime
                .render_objects()
                .unwrap()
                .iter()
                .all(|object| object.dark_distance == 1_925)
        );

        runtime
            .set_global_word(LIGHT_SOURCE_OBJECT_GLOBAL, 0)
            .unwrap();
        runtime.advance_level_shader().unwrap();
        assert!(
            runtime
                .render_objects()
                .unwrap()
                .iter()
                .all(|object| object.dark_distance == 1_945)
        );

        runtime
            .set_global_word(
                PAUSE_OBJECT_GLOBAL,
                CollisionObjectReference::new(pause.vm).to_word(),
            )
            .unwrap();
        assert!(
            runtime
                .render_objects()
                .unwrap()
                .iter()
                .all(|object| { object.dark_reference_translation == Some(pause_translation) })
        );
    }

    #[test]
    fn dark_level_reinit_preserves_target_step_and_current_on_first_tick() {
        let level = LevelId::new_const(0x28);
        let mut runtime = RetailRuntime::new_for_level(119, level);
        let mut context = level_context(ZONE, false, vec![ZONE]);
        context.graphics_flags = 0x400;
        runtime.set_level_state_context(context);
        runtime
            .set_global_word(LIGHT_SOURCE_OBJECT_GLOBAL, 0x100)
            .unwrap();

        runtime.advance_level_shader().unwrap();
        assert_eq!(
            (
                runtime.level_shader.previous_light_source,
                runtime.level_shader.ambient_target,
                runtime.level_shader.ambient_step,
                runtime.level_shader.ambient_next,
                runtime.level_shader.dark2_ambient_clear,
                runtime.level_shader.distance_target,
                runtime.level_shader.distance_step,
                runtime.level_shader.distance_next,
                runtime.level_shader.distance,
            ),
            (0x100, -8_000, -500, 3_595, 3_595, 75, -75, 1_925, 1_925)
        );

        runtime
            .set_global_word(LIGHT_SOURCE_OBJECT_GLOBAL, 0)
            .unwrap();
        runtime.apply_level_init_misc_zero(level);
        assert_eq!(runtime.level_shader.previous_light_source, 0);
        assert_eq!(runtime.level_shader.ambient_target, -8_000);
        assert_eq!(runtime.level_shader.ambient_step, -500);
        assert_eq!(runtime.level_shader.ambient_next, 4_095);
        assert_eq!(runtime.level_shader.dark2_ambient_clear, -14_000);
        assert_eq!(runtime.level_shader.distance_target, 75);
        assert_eq!(runtime.level_shader.distance_step, -75);
        assert_eq!(runtime.level_shader.distance_next, 2_000);
        assert_eq!(runtime.level_shader.distance, 1_925);

        runtime.advance_level_shader().unwrap();
        assert_eq!(runtime.level_shader.ambient_next, 3_595);
        assert_eq!(runtime.level_shader.dark2_ambient_clear, 3_595);
        assert_eq!(runtime.level_shader.distance_next, 1_925);
        assert_eq!(runtime.level_shader.distance, 1_925);
    }

    #[test]
    fn dark_level_renderer_bss_survives_stream_remount_and_first_tick() {
        let level = LevelId::new_const(0x28);
        let mut runtime = RetailRuntime::new_for_level(119, level);
        let mut context = level_context(ZONE, false, vec![ZONE]);
        context.graphics_flags = 0x400;
        runtime.set_level_state_context(context.clone());
        runtime
            .set_global_word(LIGHT_SOURCE_OBJECT_GLOBAL, 0x100)
            .unwrap();
        runtime.advance_level_shader().unwrap();

        let carry = runtime.export_session_carry();
        let mut mounted = RetailRuntime::new_from_session(119, level, carry).unwrap();
        mounted.set_level_state_context(context);
        assert_eq!(mounted.global_word(LIGHT_SOURCE_OBJECT_GLOBAL), Ok(0));
        assert_eq!(mounted.level_shader.previous_light_source, 0);
        assert_eq!(mounted.level_shader.ambient_target, -8_000);
        assert_eq!(mounted.level_shader.ambient_step, -500);
        assert_eq!(mounted.level_shader.ambient_next, 4_095);
        assert_eq!(mounted.level_shader.dark2_ambient_clear, -14_000);
        assert_eq!(mounted.level_shader.distance_target, 75);
        assert_eq!(mounted.level_shader.distance_step, -75);
        assert_eq!(mounted.level_shader.distance_next, 2_000);
        assert_eq!(mounted.level_shader.distance, 1_925);

        mounted.advance_level_shader().unwrap();
        assert_eq!(mounted.level_shader.ambient_next, 3_595);
        assert_eq!(mounted.level_shader.dark2_ambient_clear, 3_595);
        assert_eq!(mounted.level_shader.distance_next, 1_925);
        assert_eq!(mounted.level_shader.distance, 1_925);
    }

    #[test]
    fn render_snapshot_rejects_a_stale_reverse_handle_binding() {
        let entities = [entity(10, 2, 1)];
        let neighbors = [NeighborZone {
            eid: ZONE,
            display_flags: 2,
            entities: &entities,
        }];
        let mut runtime = RetailRuntime::new(0);
        let attempts = runtime.spawn_current_zone_neighbors(&neighbors, &mut SnapshotHost);
        let object = *attempts[0].result.as_ref().unwrap();
        runtime.handles.arena_by_vm[usize::from(object.vm().get())] = None;

        assert_eq!(
            runtime.render_objects(),
            Err(RenderObjectsError::StaleObjectPair(object))
        );
    }

    #[test]
    fn render_snapshot_rejects_an_invalid_animation_register() {
        let entities = [entity(10, 2, 1)];
        let neighbors = [NeighborZone {
            eid: ZONE,
            display_flags: 2,
            entities: &entities,
        }];
        let mut runtime = RetailRuntime::new(0);
        let attempts = runtime.spawn_current_zone_neighbors(&neighbors, &mut SnapshotHost);
        let object = *attempts[0].result.as_ref().unwrap();
        runtime
            .machine
            .object_mut(object.vm())
            .unwrap()
            .set_register(process_register::ANIMATION_SEQUENCE, 0x1234_5678)
            .unwrap();

        assert_eq!(
            runtime.render_objects(),
            Err(RenderObjectsError::Vm(VmError::InvalidAnimationReference(
                0x1234_5678
            )))
        );
    }

    #[test]
    fn late_bound_range_is_inclusive_and_uses_non_overflowing_deltas() {
        assert!(!translation_outside_bound_range(
            [0x7d000, -0xaf000, 0x7d000],
            [0; 3],
            LATE_BOUND_RANGE,
        ));
        assert!(translation_outside_bound_range(
            [0x7d001, 0, 0],
            [0; 3],
            LATE_BOUND_RANGE,
        ));
        assert!(translation_outside_bound_range(
            [0, -0xaf001, 0],
            [0; 3],
            LATE_BOUND_RANGE,
        ));
        assert!(translation_outside_bound_range(
            [i32::MAX, 0, 0],
            [i32::MIN, 0, 0],
            LATE_BOUND_RANGE,
        ));
    }

    #[test]
    fn animation_frame_opcode_refreshes_only_the_persistent_local_bound() {
        let mut runtime = RetailRuntime::new(0);
        let object = spawn_test_object(&mut runtime, ZONE, 10, 2, 1);
        let _main = spawn_test_object(&mut runtime, ZONE, 20, 0, 0);
        let change_frame = (0x84_u32 << 24) | (1 << 22) | (3 << 16) | (0x0e00_u32 + 70);
        let mut vm_object = VmObject::new(object.vm, vec![change_frame]).unwrap();
        vm_object.bind_animation_data(&[0; 16]);
        vm_object
            .set_register(process_register::ANIMATION_SEQUENCE, 0xa700_0001)
            .unwrap();
        vm_object.set_register(70, 5 << 8).unwrap();
        vm_object
            .set_retail_transform(RetailTransform {
                translation: [100, 200, 300],
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            })
            .unwrap();
        runtime.machine.upsert_object(vm_object).unwrap();
        let mut host = BoundHost::new(AnimationBoundSource::NonVertex);

        let frame = runtime.run_frame(&mut host, 1).unwrap();

        assert!(frame.executions[0].result.is_ok());
        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].object, object);
        assert_eq!(host.calls[0].frame_index, 5);
        assert_eq!(
            runtime
                .machine
                .object(object.vm)
                .unwrap()
                .retail_local_bound(),
            Bounds3 {
                min: Vec3 {
                    x: -51_200,
                    y: -51_200,
                    z: -51_200,
                },
                max: Vec3 {
                    x: 51_200,
                    y: 51_200,
                    z: 51_200,
                },
            }
        );
        assert!(
            runtime.machine.frame_bounds().is_empty(),
            "opcode 0x84 updates obj->bound but does not allocate object_bounds"
        );
    }

    #[test]
    fn packed_animation_opcode_uses_range_and_force_local_bound_gates() {
        let stale = Bounds3 {
            min: Vec3 { x: 1, y: 2, z: 3 },
            max: Vec3 { x: 4, y: 5, z: 6 },
        };
        let refreshed = Bounds3 {
            min: Vec3 {
                x: -51_200,
                y: -51_200,
                z: -51_200,
            },
            max: Vec3 {
                x: 51_200,
                y: 51_200,
                z: 51_200,
            },
        };
        for (force, expected_calls, expected_local) in [(false, 0, stale), (true, 1, refreshed)] {
            let mut runtime = RetailRuntime::new(0);
            let object = spawn_test_object(&mut runtime, ZONE, 10, 2, 1);
            let _main = spawn_test_object(&mut runtime, ZONE, 20, 0, 0);
            let change_animation = (0x83_u32 << 24) | (1 << 22) | (1 << 16) | (2 << 7) | 5;
            let mut vm_object = VmObject::new(object.vm, vec![change_animation]).unwrap();
            vm_object.bind_animation_data(&[0; 16]);
            vm_object.set_retail_local_bound(stale);
            vm_object
                .set_retail_transform(RetailTransform {
                    translation: [0x7d001, 0, 0],
                    rotation_yxz: [0; 3],
                    scale: [0x1000; 3],
                })
                .unwrap();
            vm_object
                .set_register(
                    process_register::STATUS_B,
                    COLLIDABLE_STATUS_B
                        | if force {
                            FORCE_LOCAL_BOUND_REFRESH_STATUS_B
                        } else {
                            0
                        },
                )
                .unwrap();
            runtime.machine.upsert_object(vm_object).unwrap();
            runtime.frame_index = 7;
            let mut host = BoundHost::new(AnimationBoundSource::NonVertex);

            runtime.run_frame(&mut host, 1).unwrap();

            assert_eq!(host.calls.len(), expected_calls);
            assert!(runtime.machine.frame_bounds().is_empty());
            let vm_object = runtime.machine.object(object.vm).unwrap();
            assert_eq!(vm_object.retail_local_bound(), expected_local);
            assert_ne!(
                vm_object.register(process_register::STATUS_A).unwrap()
                    & LOCAL_BOUND_INVALID_STATUS_A,
                0,
                "OutOfRange sets the invalid bit even when status B forces a local refresh"
            );
        }
    }

    #[test]
    fn world_bound_reuses_cached_local_volume_until_invalidated() {
        let cached = Bounds3 {
            min: Vec3 {
                x: -10,
                y: -20,
                z: -30,
            },
            max: Vec3 {
                x: 40,
                y: 50,
                z: 60,
            },
        };
        let mut runtime = RetailRuntime::new(0);
        let main = spawn_test_object(&mut runtime, ZONE, 20, 0, 0);
        let object = spawn_test_object(&mut runtime, ZONE, 10, 2, 1);
        arm_animation_bound(
            &mut runtime,
            object,
            3,
            RetailTransform {
                translation: [100, 200, 300],
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            },
        );
        let vm_object = runtime.machine.object_mut(object.vm).unwrap();
        vm_object.set_retail_local_bound(cached);
        let status_a = vm_object.register(process_register::STATUS_A).unwrap();
        vm_object
            .set_register(
                process_register::STATUS_A,
                status_a & !LOCAL_BOUND_INVALID_STATUS_A,
            )
            .unwrap();
        runtime.frame_index = 7;
        let mut host = BoundHost::new(AnimationBoundSource::NonVertex);

        runtime.run_frame(&mut host, 1).unwrap();

        assert_eq!(
            runtime
                .machine
                .object(main.vm)
                .unwrap()
                .register(process_register::ANIMATION_STAMP),
            Ok(7)
        );
        assert_eq!(
            runtime.machine.frame_bounds(),
            [crate::object_bounds::FrameBound {
                object: object.vm,
                bound: Bounds3 {
                    min: Vec3 {
                        x: 90,
                        y: 180,
                        z: 270,
                    },
                    max: Vec3 {
                        x: 140,
                        y: 250,
                        z: 360,
                    },
                },
            }]
        );
        assert_eq!(
            runtime
                .machine
                .object(object.vm)
                .unwrap()
                .retail_local_bound(),
            cached
        );
    }

    #[test]
    fn object_before_main_registers_its_bound_after_physics() {
        let mut runtime = RetailRuntime::new(0);
        let object = spawn_test_object(&mut runtime, ZONE, 10, 2, 1);
        let _main = spawn_test_object(&mut runtime, ZONE, 20, 0, 0);
        arm_animation_bound(
            &mut runtime,
            object,
            3,
            RetailTransform {
                translation: [100, 200, 300],
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            },
        );
        let vm_object = runtime.machine.object_mut(object.vm).unwrap();
        vm_object
            .set_register(process_register::STATUS_B, COLLIDABLE_STATUS_B | 0x40)
            .unwrap();
        vm_object
            .set_register(process_register::MISC_A_X, 30_720)
            .unwrap();
        runtime.frame_index = 7;
        let mut host = BoundHost::new(AnimationBoundSource::NonVertex);

        runtime.run_frame(&mut host, 1).unwrap();

        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].object, object);
        assert_eq!(
            runtime
                .machine
                .object(object.vm)
                .unwrap()
                .retail_transform()
                .unwrap()
                .translation,
            [1_120, 200, 300],
        );
        assert_eq!(
            runtime.machine.frame_bounds(),
            [crate::object_bounds::FrameBound {
                object: object.vm,
                bound: Bounds3 {
                    min: Vec3 {
                        x: -50_080,
                        y: -51_000,
                        z: -50_900,
                    },
                    max: Vec3 {
                        x: 52_320,
                        y: 51_400,
                        z: 51_500,
                    },
                },
            }]
        );
    }

    #[test]
    fn restart_requested_during_physics_still_runs_the_late_bound_tail() {
        let mut runtime = RetailRuntime::new(0);
        let object = spawn_test_object(&mut runtime, ZONE, 10, 2, 1);
        let main = spawn_test_object(&mut runtime, ZONE, 20, 0, 0);
        arm_animation_bound(
            &mut runtime,
            object,
            3,
            RetailTransform {
                translation: [100, 200, 300],
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            },
        );
        runtime
            .machine
            .object_mut(object.vm)
            .unwrap()
            .set_register(process_register::ANIMATION_STAMP, 7)
            .unwrap();
        runtime
            .machine
            .object_mut(main.vm)
            .unwrap()
            .set_register(process_register::ANIMATION_STAMP, 6)
            .unwrap();
        runtime.machine.request_level_restart();
        let mut host = BoundHost::new(AnimationBoundSource::NonVertex);
        let mut spawned_children = Vec::new();

        runtime
            .finish_native_object_update(object, &mut host, &mut spawned_children)
            .unwrap();

        assert!(runtime.machine.level_restart_requested());
        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].object, object);
        assert_eq!(
            runtime.machine.frame_bounds(),
            [crate::object_bounds::FrameBound {
                object: object.vm,
                bound: Bounds3 {
                    min: Vec3 {
                        x: -51_100,
                        y: -51_000,
                        z: -50_900,
                    },
                    max: Vec3 {
                        x: 51_300,
                        y: 51_400,
                        z: 51_500,
                    },
                },
            }]
        );
    }

    #[test]
    fn object_after_main_registers_its_bound_before_physics() {
        let mut runtime = RetailRuntime::new(0);
        let main = spawn_test_object(&mut runtime, ZONE, 20, 0, 0);
        let object = spawn_test_object(&mut runtime, ZONE, 10, 2, 1);
        runtime
            .arena
            .reparent_to_root(object.arena, RootHandle::new(7).unwrap())
            .unwrap();
        arm_animation_bound(
            &mut runtime,
            object,
            3,
            RetailTransform {
                translation: [100, 200, 300],
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            },
        );
        let vm_object = runtime.machine.object_mut(object.vm).unwrap();
        vm_object
            .set_register(process_register::STATUS_B, COLLIDABLE_STATUS_B | 0x40)
            .unwrap();
        vm_object
            .set_register(process_register::MISC_A_X, 30_720)
            .unwrap();
        runtime.frame_index = 7;
        let mut host = BoundHost::new(AnimationBoundSource::NonVertex);

        runtime.run_frame(&mut host, 1).unwrap();

        assert_eq!(
            runtime
                .machine
                .object(main.vm)
                .unwrap()
                .register(process_register::ANIMATION_STAMP),
            Ok(7),
        );
        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].object, object);
        assert_eq!(
            runtime
                .machine
                .object(object.vm)
                .unwrap()
                .retail_transform()
                .unwrap()
                .translation,
            [1_120, 200, 300],
        );
        assert_eq!(
            runtime.machine.frame_bounds(),
            [crate::object_bounds::FrameBound {
                object: object.vm,
                bound: Bounds3 {
                    min: Vec3 {
                        x: -51_100,
                        y: -51_000,
                        z: -50_900,
                    },
                    max: Vec3 {
                        x: 51_300,
                        y: 51_400,
                        z: 51_500,
                    },
                },
            }]
        );
    }

    #[test]
    fn out_of_range_late_bound_sets_invalid_until_a_bound_succeeds() {
        let mut runtime = RetailRuntime::new(0);
        let object = spawn_test_object(&mut runtime, ZONE, 10, 2, 1);
        let _main = spawn_test_object(&mut runtime, ZONE, 20, 0, 0);
        arm_animation_bound(
            &mut runtime,
            object,
            3,
            RetailTransform {
                translation: [0x7d001, 0, 0],
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            },
        );
        let vm_object = runtime.machine.object_mut(object.vm).unwrap();
        let status_a = vm_object.register(process_register::STATUS_A).unwrap();
        vm_object
            .set_register(
                process_register::STATUS_A,
                status_a & !LOCAL_BOUND_INVALID_STATUS_A,
            )
            .unwrap();
        runtime.frame_index = 7;
        let mut host = BoundHost::new(AnimationBoundSource::NonVertex);

        runtime.run_frame(&mut host, 1).unwrap();

        assert!(host.calls.is_empty());
        assert!(runtime.machine.frame_bounds().is_empty());
        assert_ne!(
            runtime
                .machine
                .object(object.vm)
                .unwrap()
                .register(process_register::STATUS_A)
                .unwrap()
                & LOCAL_BOUND_INVALID_STATUS_A,
            0,
        );

        runtime
            .machine
            .object_mut(object.vm)
            .unwrap()
            .set_register(process_register::TRANSLATION_X, 0x7d000)
            .unwrap();
        runtime.run_frame(&mut host, 1).unwrap();

        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].object, object);
        assert_eq!(runtime.machine.frame_bounds().len(), 1);
        assert_eq!(runtime.machine.frame_bounds()[0].object, object.vm);
        assert_eq!(
            runtime
                .machine
                .object(object.vm)
                .unwrap()
                .register(process_register::STATUS_A)
                .unwrap()
                & LOCAL_BOUND_INVALID_STATUS_A,
            0,
        );
    }

    #[test]
    fn same_stamp_bound_tail_clears_only_target_collider_on_miss() {
        let mut runtime = RetailRuntime::new(0);
        let main = spawn_test_object(&mut runtime, ZONE, 20, 0, 0);
        let object = spawn_test_object(&mut runtime, ZONE, 10, 2, 1);
        runtime
            .arena
            .reparent_to_root(object.arena, RootHandle::new(7).unwrap())
            .unwrap();
        runtime
            .machine
            .object_mut(main.vm)
            .unwrap()
            .set_retail_local_bound(Bounds3 {
                min: Vec3 {
                    x: -100,
                    y: -100,
                    z: -100,
                },
                max: Vec3 {
                    x: 100,
                    y: 100,
                    z: 100,
                },
            });
        arm_animation_bound(
            &mut runtime,
            object,
            3,
            RetailTransform {
                translation: [1_000_000, 0, 0],
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            },
        );
        runtime
            .machine
            .object_mut(object.vm)
            .unwrap()
            .set_link(6, Some(main.vm))
            .unwrap();
        runtime
            .machine
            .object_mut(main.vm)
            .unwrap()
            .set_link(6, Some(object.vm))
            .unwrap();
        for vm in [object.vm, main.vm] {
            runtime
                .machine
                .object_mut(vm)
                .unwrap()
                .set_register(process_register::ANIMATION_STAMP, 7)
                .unwrap();
        }
        let mut host = BoundHost::new(AnimationBoundSource::NonVertex);

        assert!(runtime.register_animation_bound(object, &mut host).unwrap());

        assert_eq!(runtime.machine.frame_bounds().len(), 1);
        assert_eq!(runtime.machine.frame_bounds()[0].object, object.vm);
        assert_eq!(
            CollisionObjectReference::from_word(
                runtime
                    .machine
                    .object(object.vm)
                    .unwrap()
                    .register(6)
                    .unwrap()
            ),
            None
        );
        assert_eq!(
            CollisionObjectReference::from_word(
                runtime
                    .machine
                    .object(main.vm)
                    .unwrap()
                    .register(6)
                    .unwrap()
            )
            .map(CollisionObjectReference::object),
            Some(object.vm),
            "a GoolObjectBound miss clears only the target's collider slot"
        );
    }

    #[test]
    fn same_stamp_bound_tail_links_crash_after_appending_the_object_bound() {
        let mut runtime = RetailRuntime::new(0);
        let main = spawn_test_object(&mut runtime, ZONE, 20, 0, 0);
        let object = spawn_test_object(&mut runtime, ZONE, 10, 2, 1);
        runtime
            .machine
            .object_mut(main.vm)
            .unwrap()
            .set_retail_local_bound(Bounds3 {
                min: Vec3 {
                    x: -1_000,
                    y: -1_000,
                    z: -1_000,
                },
                max: Vec3 {
                    x: 1_000,
                    y: 1_000,
                    z: 1_000,
                },
            });
        arm_animation_bound(
            &mut runtime,
            object,
            3,
            RetailTransform {
                translation: [0; 3],
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            },
        );
        for vm in [object.vm, main.vm] {
            runtime
                .machine
                .object_mut(vm)
                .unwrap()
                .set_register(process_register::ANIMATION_STAMP, 7)
                .unwrap();
        }
        let mut host = BoundHost::new(AnimationBoundSource::NonVertex);

        assert!(runtime.register_animation_bound(object, &mut host).unwrap());

        assert_eq!(runtime.machine.frame_bounds().len(), 1);
        assert_eq!(runtime.machine.frame_bounds()[0].object, object.vm);
        assert_eq!(
            CollisionObjectReference::from_word(
                runtime
                    .machine
                    .object(object.vm)
                    .unwrap()
                    .register(6)
                    .unwrap()
            )
            .map(CollisionObjectReference::object),
            Some(main.vm)
        );
        assert_eq!(
            CollisionObjectReference::from_word(
                runtime
                    .machine
                    .object(main.vm)
                    .unwrap()
                    .register(6)
                    .unwrap()
            )
            .map(CollisionObjectReference::object),
            Some(object.vm)
        );
    }

    #[test]
    fn late_mismatched_stamp_bound_does_not_run_the_crash_collision_tail() {
        let mut runtime = RetailRuntime::new(0);
        let object = spawn_test_object(&mut runtime, ZONE, 10, 2, 1);
        let main = spawn_test_object(&mut runtime, ZONE, 20, 0, 0);
        arm_animation_bound(
            &mut runtime,
            object,
            3,
            RetailTransform {
                translation: [1_000_000, 0, 0],
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            },
        );
        runtime
            .machine
            .object_mut(object.vm)
            .unwrap()
            .set_link(6, Some(main.vm))
            .unwrap();
        runtime
            .machine
            .object_mut(main.vm)
            .unwrap()
            .set_link(6, Some(object.vm))
            .unwrap();
        runtime
            .machine
            .object_mut(object.vm)
            .unwrap()
            .set_register(process_register::ANIMATION_STAMP, 7)
            .unwrap();
        runtime
            .machine
            .object_mut(main.vm)
            .unwrap()
            .set_register(process_register::ANIMATION_STAMP, 6)
            .unwrap();
        let mut host = BoundHost::new(AnimationBoundSource::NonVertex);

        assert!(runtime.register_animation_bound(object, &mut host).unwrap());

        assert_eq!(runtime.machine.frame_bounds().len(), 1);
        assert_eq!(runtime.machine.frame_bounds()[0].object, object.vm);
        assert_eq!(
            CollisionObjectReference::from_word(
                runtime
                    .machine
                    .object(object.vm)
                    .unwrap()
                    .register(6)
                    .unwrap()
            )
            .map(CollisionObjectReference::object),
            Some(main.vm)
        );
        assert_eq!(
            CollisionObjectReference::from_word(
                runtime
                    .machine
                    .object(main.vm)
                    .unwrap()
                    .register(6)
                    .unwrap()
            )
            .map(CollisionObjectReference::object),
            Some(object.vm)
        );
    }

    #[test]
    fn frame_bounds_follow_preorder_and_are_cleared_before_the_next_frame() {
        let entities = [entity(10, 2, 1), entity(11, 2, 1)];
        let neighbors = [NeighborZone {
            eid: ZONE,
            display_flags: 2,
            entities: &entities,
        }];
        let mut host = BoundHost::new(AnimationBoundSource::NonVertex);
        let mut runtime = RetailRuntime::new(0);
        let attempts = runtime.spawn_current_zone_neighbors(&neighbors, &mut host);
        let first = *attempts[0].result.as_ref().unwrap();
        let second = *attempts[1].result.as_ref().unwrap();
        let _main = spawn_test_object(&mut runtime, ZONE, 20, 0, 0);
        arm_animation_bound(
            &mut runtime,
            first,
            7,
            RetailTransform {
                translation: [10, 20, 30],
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            },
        );
        arm_animation_bound(
            &mut runtime,
            second,
            9,
            RetailTransform {
                translation: [-10, -20, -30],
                rotation_yxz: [0; 3],
                scale: [0x1000; 3],
            },
        );

        runtime.run_frame(&mut host, 1).unwrap();

        assert_eq!(
            host.calls
                .iter()
                .map(|call| call.object)
                .collect::<Vec<_>>(),
            [second, first]
        );
        assert_eq!(
            host.calls
                .iter()
                .map(|call| (
                    call.zone,
                    call.executable,
                    match call.reference {
                        AnimationBoundReference::ItemFive(reference) => reference.offset(),
                        AnimationBoundReference::Model(model) => {
                            panic!("unexpected process model {model}")
                        }
                    },
                    call.frame_index
                ))
                .collect::<Vec<_>>(),
            [(ZONE, 2, 1, 9), (ZONE, 2, 1, 7)]
        );
        assert_eq!(
            runtime.machine.frame_bounds(),
            [
                crate::object_bounds::FrameBound {
                    object: second.vm(),
                    bound: Bounds3 {
                        min: Vec3 {
                            x: -51_210,
                            y: -51_220,
                            z: -51_230,
                        },
                        max: Vec3 {
                            x: 51_190,
                            y: 51_180,
                            z: 51_170,
                        },
                    },
                },
                crate::object_bounds::FrameBound {
                    object: first.vm(),
                    bound: Bounds3 {
                        min: Vec3 {
                            x: -51_190,
                            y: -51_180,
                            z: -51_170,
                        },
                        max: Vec3 {
                            x: 51_210,
                            y: 51_220,
                            z: 51_230,
                        },
                    },
                },
            ]
        );

        for object in [first, second] {
            runtime
                .machine
                .object_mut(object.vm())
                .unwrap()
                .set_register(process_register::STATUS_B, 0)
                .unwrap();
        }
        runtime.run_frame(&mut host, 1).unwrap();
        assert!(runtime.machine.frame_bounds().is_empty());
        assert_eq!(host.calls.len(), 2);
    }

    #[test]
    fn dedicated_main_uses_the_crash_collision_center_adjustment() {
        let entities = [entity(10, 2, 1), entity(20, 0, 0)];
        let neighbors = [NeighborZone {
            eid: ZONE,
            display_flags: 2,
            entities: &entities,
        }];
        let source = AnimationBoundSource::Vertex {
            vertex_kind: ObjectVertexKind::Lit,
            serialized_bound: Bounds3 {
                min: Vec3 {
                    x: -256,
                    y: -256,
                    z: -256,
                },
                max: Vec3 {
                    x: 256,
                    y: 256,
                    z: 256,
                },
            },
            collision_center: Vec3 {
                x: 256,
                y: 512,
                z: 768,
            },
        };
        let mut host = BoundHost::new(source);
        let mut runtime = RetailRuntime::new(0);
        let attempts = runtime.spawn_current_zone_neighbors(&neighbors, &mut host);
        let ordinary = *attempts[0].result.as_ref().unwrap();
        let main = *attempts[1].result.as_ref().unwrap();
        assert!(!ordinary.arena().is_dedicated_main());
        assert!(main.arena().is_dedicated_main());
        for object in [ordinary, main] {
            arm_animation_bound(
                &mut runtime,
                object,
                0,
                RetailTransform {
                    translation: [0; 3],
                    rotation_yxz: [0; 3],
                    scale: [0x1000; 3],
                },
            );
        }

        runtime.run_frame(&mut host, 1).unwrap();

        assert_eq!(
            runtime.machine.frame_bounds(),
            [
                crate::object_bounds::FrameBound {
                    object: ordinary.vm(),
                    bound: Bounds3 {
                        min: Vec3 {
                            x: 0,
                            y: 256,
                            z: 512,
                        },
                        max: Vec3 {
                            x: 512,
                            y: 768,
                            z: 1_024,
                        },
                    },
                },
                crate::object_bounds::FrameBound {
                    object: main.vm(),
                    bound: Bounds3 {
                        min: Vec3 {
                            x: 256,
                            y: 768,
                            z: 1_280,
                        },
                        max: Vec3 {
                            x: 768,
                            y: 1_280,
                            z: 1_792,
                        },
                    },
                },
            ]
        );
    }

    #[test]
    fn full_object_pool_fills_the_exact_frame_bound_capacity() {
        let entities = (0..MAX_FRAME_BOUNDS)
            .map(|index| entity(u16::try_from(index + 10).unwrap(), 2, 1))
            .collect::<Vec<_>>();
        let neighbors = [NeighborZone {
            eid: ZONE,
            display_flags: 2,
            entities: &entities,
        }];
        let mut host = BoundHost::new(AnimationBoundSource::NonVertex);
        let mut runtime = RetailRuntime::new(0);
        let attempts = runtime.spawn_current_zone_neighbors(&neighbors, &mut host);
        assert_eq!(attempts.len(), MAX_FRAME_BOUNDS);
        let _main = spawn_test_object(&mut runtime, ZONE, 200, 0, 0);
        let objects = attempts
            .iter()
            .map(|attempt| *attempt.result.as_ref().unwrap())
            .collect::<Vec<_>>();
        for object in objects {
            arm_animation_bound(
                &mut runtime,
                object,
                0,
                RetailTransform {
                    translation: [0; 3],
                    rotation_yxz: [0; 3],
                    scale: [0x1000; 3],
                },
            );
        }

        let frame = runtime.run_frame(&mut host, 1).unwrap();

        assert_eq!(frame.executions.len(), MAX_FRAME_BOUNDS + 1);
        assert!(
            frame
                .executions
                .iter()
                .all(|execution| execution.result.is_ok())
        );
        assert_eq!(host.calls.len(), MAX_FRAME_BOUNDS);
        assert_eq!(runtime.machine.frame_bounds().len(), MAX_FRAME_BOUNDS);
    }

    #[test]
    fn nsf_host_resolves_frame_only_bounds_and_skips_unavailable_frames() {
        let entities = [entity(10, 2, 1)];
        let neighbors = [NeighborZone {
            eid: ZONE,
            display_flags: 2,
            entities: &entities,
        }];
        let mut runtime = RetailRuntime::new(0);
        let attempts = runtime.spawn_current_zone_neighbors(&neighbors, &mut SnapshotHost);
        let object = *attempts[0].result.as_ref().unwrap();
        let binding = AnimationBoundBinding {
            object,
            zone: ZONE,
            executable: 2,
            reference: AnimationBoundReference::ItemFive(
                AnimationReference::from_word(0xa700_0001).unwrap(),
            ),
            frame_index: 0,
        };

        let (nsd_bytes, nsf_bytes) = object_bound_stream_fixture(true, false);
        let metadata = parse_nsd(&nsd_bytes, LevelId::TITLE).unwrap();
        let nsf = parse_nsf(&nsf_bytes, &metadata).unwrap();
        let mut host = NsfProgramHost::new(&metadata, &nsf, &nsf_bytes);
        assert_eq!(
            host.animation_bound_source(binding).unwrap(),
            Some(AnimationBoundSource::Vertex {
                vertex_kind: ObjectVertexKind::Lit,
                serialized_bound: Bounds3 {
                    min: Vec3 {
                        x: -0x1000,
                        y: -0x2000,
                        z: -0x3000,
                    },
                    max: Vec3 {
                        x: 0x4000,
                        y: 0x5000,
                        z: 0x6000,
                    },
                },
                collision_center: Vec3 {
                    x: 0x0700,
                    y: -0x0800,
                    z: 0x0900,
                },
            })
        );
        assert!(matches!(
            host.animation_bound_source(AnimationBoundBinding {
                reference: AnimationBoundReference::Model(Eid::from_name("model").unwrap()),
                ..binding
            }),
            Ok(Some(AnimationBoundSource::Vertex {
                vertex_kind: ObjectVertexKind::Lit,
                ..
            }))
        ));
        assert_eq!(
            host.animation_bound_source(AnimationBoundBinding {
                frame_index: 1,
                ..binding
            }),
            Ok(None),
            "an absent frame is a controlled unavailable animation"
        );
        assert_eq!(
            host.animation_bound_source(AnimationBoundBinding {
                frame_index: u32::MAX,
                ..binding
            }),
            Ok(None),
            "a frame outside the 16-bit handle is also controlled"
        );

        let (nsd_bytes, nsf_bytes) = object_bound_stream_fixture(false, true);
        let metadata = parse_nsd(&nsd_bytes, LevelId::TITLE).unwrap();
        let nsf = parse_nsf(&nsf_bytes, &metadata).unwrap();
        let mut host = NsfProgramHost::new(&metadata, &nsf, &nsf_bytes);
        assert_eq!(host.animation_bound_source(binding), Ok(None));
    }

    fn level_location(zone: Eid, path: u16, progress: i32) -> RetailCameraLocation {
        RetailCameraLocation {
            path: crust_formats::stream::RetailPathId {
                zone,
                index: u32::from(path),
            },
            progress: crate::retail_frame::PathProgress::clamped(
                progress,
                core::num::NonZeroU16::new(32).unwrap(),
            ),
        }
    }

    fn level_context(
        zone: Eid,
        first_spawn: bool,
        active_neighbor_zones: Vec<Eid>,
    ) -> RetailLevelStateContext {
        RetailLevelStateContext {
            location: level_location(zone, 2, 0x345),
            graphics_flags: 0,
            box_count: 0x900,
            checkpoint_id: -1,
            checkpoint_translation: [0; 3],
            first_spawn,
            active_neighbor_zones,
        }
    }

    fn level_snapshot(level: LevelId) -> RetailLevelSnapshot {
        RetailLevelSnapshot {
            player_translation: [11, 22, 33],
            player_rotation_yxz: [0; 3],
            player_scale: [0x1000, 0x1100, 0x1200],
            location: level_location(ZONE, 2, 0x345),
            level,
            death_resets_counter: true,
            spawn_words: std::array::from_fn(|index| u32::try_from(index).unwrap()),
            box_count: 0x900,
        }
    }

    #[derive(Default)]
    struct CaptionHost {
        bindings: Vec<(Eid, u8, u8, Vec<u32>)>,
    }

    impl ProgramHost for CaptionHost {
        type Error = ();

        fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
            let arguments = match binding.origin {
                ProgramOrigin::RuntimeChild { arguments } => arguments,
                ProgramOrigin::Entity(_) => &[],
            };
            self.bindings.push((
                binding.zone,
                binding.executable,
                binding.subtype,
                arguments.to_vec(),
            ));
            let mut object = VmObject::new(binding.object.vm(), vec![RETURN]).map_err(|_| ())?;
            object.initialize_arguments(arguments).map_err(|_| ())?;
            Ok(object)
        }

        fn bind_state_program(
            &mut self,
            _binding: StateProgramBinding,
        ) -> Result<VmStateProgram, Self::Error> {
            Err(())
        }
    }

    #[test]
    fn retail_core_objects_publish_exact_root_order_and_pointer_globals() {
        let mut runtime = RetailRuntime::new_for_level(119, LevelId::N_SANITY_BEACH);
        let mut host = CaptionHost::default();

        let objects = runtime
            .create_retail_core_objects(ZONE, &mut host)
            .unwrap()
            .unwrap();

        assert_eq!(
            host.bindings,
            [
                (ZONE, 4, 0, Vec::new()),
                (ZONE, 4, 1, Vec::new()),
                (ZONE, 4, 5, Vec::new()),
            ]
        );
        let root = RootHandle::new(1).unwrap();
        assert_eq!(
            runtime
                .arena
                .preorder(TreeParent::Root(root))
                .unwrap()
                .collect::<Vec<_>>(),
            [
                objects.pickup.arena,
                objects.fruit.arena,
                objects.life.arena
            ]
        );
        for (object, subtype) in [(objects.life, 0), (objects.fruit, 1), (objects.pickup, 5)] {
            let spawned = runtime.arena.get(object.arena).unwrap();
            assert_eq!(spawned.zone(), Eid::NONE);
            assert_eq!(spawned.parent(), TreeParent::Root(root));
            assert_eq!(
                (spawned.origin().executable(), spawned.origin().subtype()),
                (4, subtype)
            );
        }
        for (global, object) in [(7, objects.life), (6, objects.fruit), (14, objects.pickup)] {
            assert_eq!(
                runtime.global_word(global),
                Ok(CollisionObjectReference::new(object.vm).to_word())
            );
        }

        assert_eq!(
            runtime.create_retail_core_objects(ZONE_B, &mut host),
            Ok(Some(objects))
        );
        assert_eq!(host.bindings.len(), 3, "a same-level restart is idempotent");
    }

    #[test]
    fn retail_core_objects_keep_native_null_zone_with_a_solid_environment() {
        let mut runtime = RetailRuntime::new_for_level(119, LevelId::N_SANITY_BEACH);
        let mut host = SolidZoneHost::default();

        let objects = runtime
            .create_retail_core_objects(ZONE, &mut host)
            .unwrap()
            .unwrap();

        assert_eq!(host.calls, [ZONE, ZONE, ZONE]);
        for object in [objects.life, objects.fruit, objects.pickup] {
            assert_eq!(runtime.arena.get(object.arena).unwrap().zone(), Eid::NONE);
            assert_eq!(
                runtime
                    .machine
                    .object(object.vm)
                    .unwrap()
                    .retail_solid_zone_eid(),
                None,
                "runtime-created executable four must preserve GoolObjectInit's null obj->zone"
            );
        }
    }

    #[test]
    fn retail_core_objects_skip_four_native_non_gameplay_streams() {
        for level in [
            LevelId::TITLE,
            LevelId::LEVEL_COMPLETE,
            LevelId::INTRO,
            LevelId::ENDING,
        ] {
            let mut runtime = RetailRuntime::new_for_level(119, level);
            let mut host = CaptionHost::default();
            assert_eq!(
                runtime.create_retail_core_objects(ZONE, &mut host),
                Ok(None)
            );
            assert!(host.bindings.is_empty());
            assert!(
                runtime
                    .arena
                    .preorder(TreeParent::Root(RootHandle::new(1).unwrap()))
                    .unwrap()
                    .next()
                    .is_none()
            );
            assert_eq!(runtime.global_word(6), Ok(0));
            assert_eq!(runtime.global_word(7), Ok(0));
            assert_eq!(runtime.global_word(14), Ok(0));
        }
    }

    #[test]
    fn retail_core_object_preflight_keeps_an_undersized_vm_empty() {
        let mut runtime = RetailRuntime::new_for_level(14, LevelId::N_SANITY_BEACH);
        let mut host = CaptionHost::default();

        assert_eq!(
            runtime.create_retail_core_objects(ZONE, &mut host),
            Err(RuntimeError::Vm(VmError::InvalidRegister(14)))
        );
        assert!(host.bindings.is_empty());
        assert!(
            runtime
                .arena
                .preorder(TreeParent::Root(RootHandle::new(1).unwrap()))
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[test]
    fn level_init_misc_one_creates_exact_root_four_controllers() {
        let cases = [
            (0x05, Some((9, 4, false))),
            (0x0e, None),
            (0x14, Some((23, 6, false))),
            (0x16, Some((23, 6, false))),
            (0x17, Some((39, 4, true))),
            (0x22, Some((53, 13, false))),
            (0x28, None),
            (0x2a, None),
            (0x2e, Some((53, 13, false))),
            (0x09, None),
        ];

        for (level, expected) in cases {
            let level = LevelId::new_const(level);
            let mut runtime = RetailRuntime::new_for_level(119, level);
            let mut host = CaptionHost::default();
            let created = runtime
                .create_retail_level_misc_object(ZONE, &mut host)
                .unwrap();

            if let Some((executable, subtype, publishes_ambiance)) = expected {
                let object = created.expect("source branch creates one controller");
                assert_eq!(host.bindings, [(ZONE, executable, subtype, Vec::new())]);
                let spawned = runtime.arena.get(object.arena).unwrap();
                assert_eq!(spawned.zone(), Eid::NONE);
                assert_eq!(
                    spawned.parent(),
                    TreeParent::Root(RootHandle::new(LEVEL_MISC_CONTROLLER_ROOT).unwrap())
                );
                assert_eq!(
                    (spawned.origin().executable(), spawned.origin().subtype()),
                    (executable, subtype)
                );
                assert_eq!(
                    runtime.global_word(AMBIANCE_OBJECT_GLOBAL),
                    Ok(if publishes_ambiance {
                        CollisionObjectReference::new(object.vm).to_word()
                    } else {
                        0
                    })
                );
                assert_eq!(
                    runtime
                        .arena
                        .preorder(TreeParent::Root(
                            RootHandle::new(LEVEL_MISC_CONTROLLER_ROOT).unwrap()
                        ))
                        .unwrap()
                        .collect::<Vec<_>>(),
                    [object.arena]
                );
            } else {
                assert_eq!(created, None);
                assert!(host.bindings.is_empty());
                assert!(
                    runtime
                        .arena
                        .preorder(TreeParent::Root(
                            RootHandle::new(LEVEL_MISC_CONTROLLER_ROOT).unwrap()
                        ))
                        .unwrap()
                        .next()
                        .is_none()
                );
                assert_eq!(runtime.global_word(AMBIANCE_OBJECT_GLOBAL), Ok(0));
            }

            assert_eq!(
                runtime.create_retail_level_misc_object(ZONE_B, &mut host),
                Ok(created),
                "mount-time controller creation must be idempotent for {level}"
            );
            assert_eq!(host.bindings.len(), usize::from(expected.is_some()));
        }
    }

    #[test]
    fn ripper_misc_controller_preflight_keeps_an_undersized_vm_empty() {
        let mut runtime =
            RetailRuntime::new_for_level(AMBIANCE_OBJECT_GLOBAL, LevelId::new_const(0x17));
        let mut host = CaptionHost::default();

        assert_eq!(
            runtime.create_retail_level_misc_object(ZONE, &mut host),
            Err(RuntimeError::Vm(VmError::InvalidRegister(
                AMBIANCE_OBJECT_GLOBAL
            )))
        );
        assert!(host.bindings.is_empty());
        assert!(
            runtime
                .arena
                .preorder(TreeParent::Root(
                    RootHandle::new(LEVEL_MISC_CONTROLLER_ROOT).unwrap()
                ))
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[test]
    fn retail_demo_caption_uses_root_one_exact_program_and_null_lifecycle_zone() {
        let mut runtime = RetailRuntime::new(PBAK_STATE_GLOBAL + 1);
        let mut host = CaptionHost::default();

        let caption = runtime.create_retail_demo_caption(ZONE, &mut host).unwrap();

        assert_eq!(
            host.bindings,
            [(
                ZONE,
                PBAK_CAPTION_EXECUTABLE,
                PBAK_CAPTION_SUBTYPE,
                PBAK_CAPTION_ARGUMENTS.to_vec(),
            )]
        );
        let spawned = runtime.arena.get(caption.arena).unwrap();
        assert_eq!(spawned.zone(), Eid::NONE);
        assert_eq!(
            spawned.parent(),
            TreeParent::Root(RootHandle::new(1).unwrap())
        );
    }

    #[derive(Default)]
    struct NullLifecycleChildHost {
        bindings: Vec<(Eid, u8, u8, Vec<u32>)>,
        zone_environment_calls: Vec<Eid>,
        solid_environment_calls: Vec<Eid>,
    }

    impl ProgramHost for NullLifecycleChildHost {
        type Error = ();

        fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error> {
            let arguments = match binding.origin {
                ProgramOrigin::RuntimeChild { arguments } => arguments,
                ProgramOrigin::Entity(_) => &[],
            };
            self.bindings.push((
                binding.zone,
                binding.executable,
                binding.subtype,
                arguments.to_vec(),
            ));
            let mut object = VmObject::new(binding.object.vm(), vec![RETURN]).map_err(|_| ())?;
            object.initialize_arguments(arguments).map_err(|_| ())?;
            Ok(object)
        }

        fn bind_state_program(
            &mut self,
            _binding: StateProgramBinding,
        ) -> Result<VmStateProgram, Self::Error> {
            Err(())
        }

        fn zone_environment(
            &mut self,
            zone: Eid,
        ) -> Result<Option<RetailZoneEnvironment>, Self::Error> {
            self.zone_environment_calls.push(zone);
            Ok(Some(RetailZoneEnvironment {
                origin: [0; 3],
                object_colors: [0x1234; COLOR_COUNT],
                player_colors: [0x5678; COLOR_COUNT],
                graphics_flags: 0,
            }))
        }

        fn solid_environment(
            &mut self,
            zone: Eid,
        ) -> Result<Option<RetailSolidEnvironment>, Self::Error> {
            self.solid_environment_calls.push(zone);
            Ok(None)
        }
    }

    #[test]
    fn pbak_caption_null_zone_child_uses_current_zone_only_for_environment() {
        // DispC state 15 PC 617 in pb0cB executes this exact reclaiming
        // spawn: executable four, subtype nine, one child and two arguments.
        const PBAK_CAPTION_CHILD_SPAWN: u32 = 0x9120_4241;
        const CAPTION_Y_ARGUMENT: u32 = (-23_296_i32) as u32;

        let mut runtime = RetailRuntime::new(PBAK_STATE_GLOBAL + 1);
        runtime.set_level_state_context(level_context(ZONE, false, vec![ZONE]));
        let caption = runtime
            .create_retail_demo_caption(ZONE, &mut CaptionHost::default())
            .unwrap();
        let mut caption_vm = VmObject::new(
            caption.vm,
            vec![
                Instruction::encode(0x11, 0x0e00, 0x0e1f),
                Instruction::encode(0x11, 0x0e01, 0x0e1f),
                PBAK_CAPTION_CHILD_SPAWN,
                RETURN,
            ],
        )
        .unwrap();
        caption_vm.set_register(0, 0).unwrap();
        caption_vm.set_register(1, CAPTION_Y_ARGUMENT).unwrap();
        runtime.machine.upsert_object(caption_vm).unwrap();

        let mut host = NullLifecycleChildHost::default();
        let frame = runtime.run_frame(&mut host, 4).unwrap();

        assert!(
            frame
                .executions
                .iter()
                .all(|execution| execution.result.is_ok())
        );
        assert_eq!(frame.spawned_children.len(), 1);
        assert_eq!(host.bindings, [(ZONE, 4, 9, vec![0, CAPTION_Y_ARGUMENT])]);
        assert_eq!(host.zone_environment_calls, [ZONE]);
        assert_eq!(
            host.solid_environment_calls,
            [ZONE, ZONE],
            "creation and the first update both resolve cur_zone, never NONE"
        );

        let child = frame.spawned_children[0];
        let spawned = runtime.arena.get(child.arena).unwrap();
        assert_eq!(spawned.zone(), Eid::NONE);
        assert_eq!(
            spawned.origin(),
            crate::object_arena::ObjectOrigin::Runtime {
                executable: 4,
                subtype: 9,
            }
        );
        assert_eq!(
            runtime.machine.object(child.vm).unwrap().retail_colors(),
            &[0x1234; COLOR_COUNT]
        );
    }

    #[test]
    fn retail_demo_finish_releases_non_island_input_without_reading_caption() {
        let mut runtime = RetailRuntime::new(PBAK_STATE_GLOBAL + 1);
        runtime.set_global_word(PBAK_STATE_GLOBAL, 2).unwrap();
        runtime
            .set_global_word(CAPTION_OBJECT_GLOBAL, 0xdead_beef)
            .unwrap();

        assert_eq!(
            runtime.finish_retail_demo(&mut SnapshotHost),
            Ok(RetailDemoFinishOutcome::Released)
        );
        assert_eq!(runtime.global_word(PBAK_STATE_GLOBAL), Ok(0));
    }

    #[test]
    fn retail_demo_finish_dispatches_caption_event_then_latches_input_lock() {
        let mut runtime = RetailRuntime::new(PBAK_STATE_GLOBAL + 1);
        let caption = spawn_test_object(&mut runtime, ZONE, 7, 4, 2);
        runtime
            .machine
            .object_mut(caption.vm)
            .unwrap()
            .configure_test_event_interrupt(PBAK_CAPTION_EVENT, vec![RETURN])
            .unwrap();
        runtime
            .set_global_word(ISLAND_CAMERA_ROTATION_GLOBAL, 0x0f00)
            .unwrap();
        runtime
            .set_global_word(
                CAPTION_OBJECT_GLOBAL,
                CollisionObjectReference::new(caption.vm).to_word(),
            )
            .unwrap();
        runtime.set_global_word(PBAK_STATE_GLOBAL, 2).unwrap();

        assert_eq!(
            runtime.finish_retail_demo(&mut SnapshotHost),
            Ok(RetailDemoFinishOutcome::CaptionEvent {
                recipient: caption,
                dispatch: EventDispatchOutcome {
                    acknowledged: true,
                    state_change: None,
                },
                effects: Vec::new(),
            })
        );
        assert_eq!(runtime.global_word(PBAK_STATE_GLOBAL), Ok(3));
        assert_eq!(
            runtime
                .machine
                .object(caption.vm)
                .unwrap()
                .register(process_register::EVENT),
            Ok(PBAK_CAPTION_EVENT)
        );
    }

    #[test]
    fn retail_demo_finish_runs_between_earlier_caption_and_later_roots() {
        let mut runtime = RetailRuntime::new(PBAK_STATE_GLOBAL + 1);
        let caption = spawn_test_object(&mut runtime, ZONE, 70, 4, 8);
        let main = spawn_test_object(&mut runtime, ZONE, 71, 0, 0);
        let observer = spawn_test_object(&mut runtime, ZONE, 72, 2, 0);
        for (object, root) in [(caption, 1), (main, 6), (observer, 7)] {
            runtime
                .arena
                .reparent_to_root(object.arena, RootHandle::new(root).unwrap())
                .unwrap();
        }

        let mut caption_vm = VmObject::new(
            caption.vm,
            vec![Instruction::encode(0x11, 0x0801, 0x0e00), RETURN],
        )
        .unwrap();
        caption_vm.configure_test_event_state(PBAK_CAPTION_EVENT, 7);
        runtime.machine.upsert_object(caption_vm).unwrap();

        let caption_event_register = 0x0c00 | process_register::EVENT as u16;
        let mut observer_vm = VmObject::new(
            observer.vm,
            vec![
                Instruction::encode(0x11, caption_event_register, 0x0e00),
                RETURN,
            ],
        )
        .unwrap();
        observer_vm.set_link(0, Some(caption.vm)).unwrap();
        runtime.machine.upsert_object(observer_vm).unwrap();
        runtime
            .set_global_word(ISLAND_CAMERA_ROTATION_GLOBAL, 0x0f00)
            .unwrap();
        runtime
            .set_global_word(
                CAPTION_OBJECT_GLOBAL,
                CollisionObjectReference::new(caption.vm).to_word(),
            )
            .unwrap();
        runtime.set_global_word(PBAK_STATE_GLOBAL, 2).unwrap();

        let mut boundary = None;
        let mut host = AudioRecordingHost::new(Some(event_transition_state()));
        let frame = runtime
            .run_frame_with_traversal_hook(&mut host, 8, |runtime, host, point| {
                boundary = Some(point);
                assert_eq!(
                    runtime.machine.object(caption.vm).unwrap().register(0),
                    Ok(0x100),
                    "root-one caption ordinary work must precede PadUpdatePbak"
                );
                assert_eq!(runtime.machine.object(caption.vm).unwrap().state(), 0);
                let outcome = runtime.finish_retail_demo(host)?;
                assert!(matches!(
                    outcome,
                    RetailDemoFinishOutcome::CaptionEvent {
                        dispatch: EventDispatchOutcome {
                            state_change: Some(EventStateChange { state: 7, .. }),
                            ..
                        },
                        ..
                    }
                ));
                assert_eq!(runtime.machine.object(caption.vm).unwrap().state(), 7);
                Ok(())
            })
            .unwrap();

        assert_eq!(
            boundary,
            Some(RetailTraversalBoundary::BeforeMainObjectUpdate {
                root: RootHandle::new(6).unwrap(),
                object: main,
            })
        );
        assert_eq!(
            frame
                .executions
                .iter()
                .map(|execution| execution.object)
                .collect::<Vec<_>>(),
            [caption, main, observer]
        );
        assert_eq!(runtime.global_word(PBAK_STATE_GLOBAL), Ok(3));
        assert_eq!(
            runtime.machine.object(observer.vm).unwrap().register(0),
            Ok(PBAK_CAPTION_EVENT),
            "root seven must observe the caption event/rebind from Crash's boundary"
        );
    }

    #[test]
    fn failed_outer_frame_retains_caption_effects_in_the_finish_outcome() {
        let mut runtime = RetailRuntime::new(PBAK_STATE_GLOBAL + 1);
        let caption = spawn_test_object(&mut runtime, ZONE, 73, 4, 8);
        let main = spawn_test_object(&mut runtime, ZONE, 74, 0, 0);
        runtime
            .arena
            .reparent_to_root(caption.arena, RootHandle::new(1).unwrap())
            .unwrap();
        runtime
            .arena
            .reparent_to_root(main.arena, RootHandle::new(6).unwrap())
            .unwrap();
        let caption_vm = runtime.machine.object_mut(caption.vm).unwrap();
        caption_vm
            .configure_test_event_interrupt(
                PBAK_CAPTION_EVENT,
                vec![misc(12, 9, 0x0e00), 0x8280_0000],
            )
            .unwrap();
        caption_vm.set_register(0, 0x17 << 8).unwrap();
        runtime
            .set_global_word(ISLAND_CAMERA_ROTATION_GLOBAL, 0x0f00)
            .unwrap();
        runtime
            .set_global_word(
                CAPTION_OBJECT_GLOBAL,
                CollisionObjectReference::new(caption.vm).to_word(),
            )
            .unwrap();
        runtime.set_global_word(PBAK_STATE_GLOBAL, 2).unwrap();

        let mut finish = None;
        let result =
            runtime.run_frame_with_traversal_hook(&mut SnapshotHost, 8, |runtime, host, _| {
                finish = Some(runtime.finish_retail_demo(host)?);
                Err(RuntimeError::InvalidRootIndex(99))
            });

        assert!(matches!(result, Err(RuntimeError::InvalidRootIndex(99))));
        let finish = finish.expect("hook completed the PBAK handoff before failing");
        assert_eq!(finish.effects(), &[VmEffect::Transition(0x17)]);
        assert!(
            runtime
                .machine
                .effects()
                .contains(&VmEffect::Transition(0x17))
        );
    }

    #[test]
    fn retail_demo_finish_effect_accessor_covers_release_and_caption_outcomes() {
        let mut runtime = RetailRuntime::new(0);
        let caption = spawn_test_object(&mut runtime, ZONE, 8, 4, 2);
        assert!(RetailDemoFinishOutcome::Released.effects().is_empty());

        let outcome = RetailDemoFinishOutcome::CaptionEventFault {
            recipient: caption,
            effects: vec![VmEffect::Transition(7)],
        };
        assert_eq!(outcome.effects(), &[VmEffect::Transition(7)]);
    }

    #[test]
    fn retail_demo_finish_contains_faulted_caption_handler_and_keeps_input_lock() {
        let mut runtime = RetailRuntime::new(PBAK_STATE_GLOBAL + 1);
        let caption = spawn_test_object(&mut runtime, ZONE, 8, 4, 2);
        runtime
            .machine
            .object_mut(caption.vm)
            .unwrap()
            .configure_test_event_interrupt(PBAK_CAPTION_EVENT, vec![0xff00_0000])
            .unwrap();
        runtime
            .set_global_word(ISLAND_CAMERA_ROTATION_GLOBAL, 0x0f00)
            .unwrap();
        runtime
            .set_global_word(
                CAPTION_OBJECT_GLOBAL,
                CollisionObjectReference::new(caption.vm).to_word(),
            )
            .unwrap();
        runtime.set_global_word(PBAK_STATE_GLOBAL, 2).unwrap();

        assert_eq!(
            runtime.finish_retail_demo(&mut SnapshotHost),
            Ok(RetailDemoFinishOutcome::CaptionEventFault {
                recipient: caption,
                effects: Vec::new(),
            })
        );
        assert_eq!(runtime.global_word(PBAK_STATE_GLOBAL), Ok(3));
        assert_eq!(
            runtime
                .machine
                .object(caption.vm)
                .unwrap()
                .register(process_register::EVENT),
            Ok(PBAK_CAPTION_EVENT)
        );
    }

    #[test]
    fn retail_demo_finish_rejects_an_untyped_caption_global() {
        let mut runtime = RetailRuntime::new(PBAK_STATE_GLOBAL + 1);
        runtime
            .set_global_word(ISLAND_CAMERA_ROTATION_GLOBAL, 0x0100)
            .unwrap();
        runtime
            .set_global_word(CAPTION_OBJECT_GLOBAL, 0x1234_5678)
            .unwrap();

        assert_eq!(
            runtime.finish_retail_demo(&mut SnapshotHost),
            Err(RuntimeError::InvalidGlobalObjectReference {
                global: CAPTION_OBJECT_GLOBAL,
                value: 0x1234_5678,
            })
        );
        assert_eq!(runtime.global_word(PBAK_STATE_GLOBAL), Ok(0));
    }

    #[test]
    fn retail_demo_start_preflights_then_installs_seed_snapshot_bound_and_globals() {
        let level = LevelId::new(0x09).unwrap();
        let mut runtime = RetailRuntime::new_for_level(PBAK_STATE_GLOBAL + 1, level);
        let main = spawn_test_object(&mut runtime, ZONE, 5, 0, 0);
        runtime.set_level_state_context(level_context(ZONE, false, vec![ZONE]));
        runtime
            .set_global_word(CHECKPOINT_ID_GLOBAL, 7 << 8)
            .unwrap();
        runtime.set_random_seed_b(0xd3dc_167e);
        let snapshot = level_snapshot(level);
        let bound = Bounds3 {
            min: Vec3 {
                x: -10,
                y: -20,
                z: -30,
            },
            max: Vec3 {
                x: 40,
                y: 50,
                z: 60,
            },
        };

        runtime
            .install_retail_demo_start(snapshot.clone(), 0x1234_5678, bound)
            .unwrap();

        assert_eq!(runtime.saved_level_state(), Some(&snapshot));
        assert_eq!(runtime.machine.random_seed(), 0x1234_5678);
        assert_eq!(runtime.random_seed_b(), 0xd3dc_167e);
        assert_eq!(runtime.global_word(CHECKPOINT_ID_GLOBAL), Ok(u32::MAX));
        assert_eq!(runtime.global_word(PBAK_STATE_GLOBAL), Ok(2));
        assert_eq!(
            runtime
                .level_state_context()
                .expect("demo start preserves the checked context")
                .checkpoint_id,
            -1
        );
        assert_eq!(
            runtime
                .machine
                .object(main.vm)
                .unwrap()
                .retail_local_bound(),
            bound
        );
    }

    #[test]
    fn retail_demo_start_rejects_a_foreign_snapshot_before_mutation() {
        let mounted = LevelId::new(0x09).unwrap();
        let recorded = LevelId::new(0x0f).unwrap();
        let mut runtime = RetailRuntime::new_for_level(PBAK_STATE_GLOBAL + 1, mounted);
        spawn_test_object(&mut runtime, ZONE, 5, 0, 0);
        runtime.set_level_state_context(level_context(ZONE, false, vec![ZONE]));

        assert_eq!(
            runtime.install_retail_demo_start(
                level_snapshot(recorded),
                0x1234_5678,
                Bounds3::default(),
            ),
            Err(RetailDemoStartError::LevelMismatch { mounted, recorded })
        );
        assert_eq!(runtime.saved_level_state(), None);
        assert_eq!(runtime.machine.random_seed(), 12_345);
        assert_eq!(runtime.global_word(PBAK_STATE_GLOBAL), Ok(0));
    }

    #[test]
    fn level_save_captures_native_fields_and_translation_overrides() {
        let level = LevelId::new(0x03).unwrap();
        let mut runtime = RetailRuntime::new_for_level(BOX_COUNT_GLOBAL + 1, level);
        let main = spawn_test_object(&mut runtime, ZONE, 5, 0, 0);
        let caller = spawn_test_object(&mut runtime, ZONE, 6, 2, 0);
        for (register, value) in [
            (process_register::TRANSLATION_X, 11_i32),
            (process_register::TRANSLATION_Y, 22),
            (process_register::TRANSLATION_Z, 33),
            (process_register::ROTATION_Y, 0x111),
            (process_register::ROTATION_X, 0x222),
            (process_register::ROTATION_Z, 0x333),
            (process_register::SCALE_X, 0x1000),
            (process_register::SCALE_Y, 0x1100),
            (process_register::SCALE_Z, 0x1200),
        ] {
            runtime
                .machine
                .object_mut(main.vm)
                .unwrap()
                .set_register(register, value.cast_unsigned())
                .unwrap();
        }
        for (register, value) in [
            (process_register::TRANSLATION_X, -101_i32),
            (process_register::TRANSLATION_Y, -202),
            (process_register::TRANSLATION_Z, -303),
            (
                process_register::STATUS_B,
                SAVE_TRANSLATION_FROM_CALLER_STATUS_B as i32,
            ),
        ] {
            runtime
                .machine
                .object_mut(caller.vm)
                .unwrap()
                .set_register(register, value.cast_unsigned())
                .unwrap();
        }
        runtime.arena.spawn_table_mut().set_flags(42, 0xa5).unwrap();
        runtime.set_level_state_context(level_context(ZONE, false, vec![ZONE]));
        runtime.set_global_word(BOX_COUNT_GLOBAL, 0x900).unwrap();

        let RetailSaveStateOutcome::Saved(caller_save) =
            runtime.save_level_state(caller, false).unwrap()
        else {
            panic!("unrestricted zone must save");
        };
        assert_eq!(caller_save.player_translation, [-101, -202, -303]);
        assert_eq!(caller_save.player_rotation_yxz, [0; 3]);
        assert_eq!(caller_save.player_scale, [0x1000, 0x1100, 0x1200]);
        assert_eq!(caller_save.location, level_location(ZONE, 2, 0x345));
        assert_eq!(caller_save.level, level);
        assert!(!caller_save.death_resets_counter);
        assert_eq!(caller_save.spawn_words[42], 0xa5);
        assert_eq!(caller_save.box_count, 0x900);

        let context = runtime.level_state_context.as_mut().unwrap();
        context.checkpoint_id = 7 << 8;
        context.checkpoint_translation = [700, 701, 702];
        let RetailSaveStateOutcome::Saved(checkpoint_save) =
            runtime.save_level_state(caller, true).unwrap()
        else {
            panic!("checkpoint save must succeed");
        };
        assert_eq!(checkpoint_save.player_translation, [700, 701, 702]);
        assert!(checkpoint_save.death_resets_counter);

        runtime.level_state_context.as_mut().unwrap().graphics_flags = SAVE_RESTRICTED_ZONE_FLAG;
        assert_eq!(
            runtime.save_level_state(caller, false),
            Ok(RetailSaveStateOutcome::RestrictedByZone)
        );
        assert_eq!(runtime.saved_level_state(), Some(checkpoint_save.as_ref()));
    }

    #[test]
    fn level_save_reads_live_box_count_instead_of_stale_host_context() {
        let level = LevelId::new(0x09).unwrap();
        let mut runtime = RetailRuntime::new_for_level(BOX_COUNT_GLOBAL + 1, level);
        let main = spawn_test_object(&mut runtime, ZONE, 5, 0, 0);
        let caller = spawn_test_object(&mut runtime, ZONE, 6, 22, 0);
        let mut context = level_context(ZONE, false, vec![ZONE]);
        context.box_count = 0x400;
        runtime.set_level_state_context(context);

        // LevelSaveState reads the process global directly. A host mirror may
        // legitimately still contain the previous cooperative-frame value at
        // this synchronous boundary.
        runtime.set_global_word(BOX_COUNT_GLOBAL, 0x500).unwrap();
        let RetailSaveStateOutcome::Saved(snapshot) =
            runtime.save_level_state(caller, true).unwrap()
        else {
            panic!("unrestricted checkpoint zone must save");
        };

        assert_eq!(snapshot.box_count, 0x500);
        assert_eq!(runtime.level_state_context().unwrap().box_count, 0x400);
        assert_eq!(runtime.arena().main_object(), Some(main.arena));
    }

    #[test]
    fn same_level_misc_save_and_load_abort_for_the_deferred_structural_restart() {
        let level = LevelId::new(0x03).unwrap();
        let mut runtime = RetailRuntime::new_for_level(BOX_COUNT_GLOBAL + 1, level);
        let main = spawn_test_object(&mut runtime, ZONE, 5, 0, 0);
        let child = attach_test_child(&mut runtime, main, ZONE, 2);

        let mut main_vm = VmObject::new(
            main.vm,
            vec![
                misc(12, 0, 0x0be0),
                Instruction::encode(0x11, 0x0805, 0x0e08),
                misc(12, 1, 0x0be0),
                Instruction::encode(0x11, 0x0807, 0x0e09),
                RETURN,
            ],
        )
        .unwrap();
        main_vm
            .set_register(process_register::TRANSLATION_X, 0x111)
            .unwrap();
        main_vm
            .set_register(process_register::TRANSLATION_Y, 0x222)
            .unwrap();
        for register in [
            process_register::SCALE_X,
            process_register::SCALE_Y,
            process_register::SCALE_Z,
        ] {
            main_vm.set_register(register, 0x1000).unwrap();
        }
        runtime.machine.upsert_object(main_vm).unwrap();

        let mut child_vm = VmObject::new(
            child.vm,
            vec![Instruction::encode(0x11, 0x0809, 0x0e00), RETURN],
        )
        .unwrap();
        child_vm.set_register(0, 0x1234).unwrap();
        runtime.machine.upsert_object(child_vm).unwrap();
        runtime.set_level_state_context(level_context(ZONE, false, vec![ZONE]));
        runtime
            .machine
            .set_global_word(NEXT_DISPLAY_GLOBAL, 0)
            .unwrap();

        let frame = runtime.run_frame(&mut SnapshotHost, 8).unwrap();

        assert_eq!(
            frame.executions.len(),
            1,
            "children and later roots do not run"
        );
        assert_eq!(frame.executions[0].object, main);
        assert_eq!(
            frame.executions[0].result.as_ref().unwrap(),
            &Execution {
                reason: HaltReason::HostEffect,
                steps: 3,
            }
        );
        assert_eq!(
            frame.effects,
            vec![
                VmEffect::SaveState(main.vm),
                VmEffect::LoadState {
                    object: main.vm,
                    saved_level: Some(level),
                },
            ]
        );
        assert!(runtime.machine.level_restart_requested());
        assert_eq!(
            runtime
                .saved_level_state()
                .expect("misc 12/0 saved before continuing")
                .player_translation[0],
            0x111
        );
        assert_eq!(
            runtime
                .machine
                .object(main.vm)
                .unwrap()
                .register(process_register::TRANSLATION_X),
            Ok(0x500),
            "the instruction between save and load executes"
        );
        assert_eq!(
            runtime
                .machine
                .object(main.vm)
                .unwrap()
                .register(process_register::TRANSLATION_Y),
            Ok(0x222),
            "the instruction after load does not execute"
        );
        assert_eq!(
            runtime.machine.object(child.vm).unwrap().register(0),
            Ok(0x1234)
        );
        assert_eq!(
            runtime.machine.global_word(CURRENT_DISPLAY_GLOBAL),
            Ok(INITIAL_DISPLAY_MASK),
            "the aborted frame does not latch its pending display mask"
        );

        runtime.restart_saved_level(&mut SnapshotHost).unwrap();
        assert!(!runtime.machine.level_restart_requested());
    }

    #[test]
    fn different_level_misc_load_continues_the_source_frame_before_remount() {
        let current = LevelId::new_const(0x24);
        let saved = LevelId::new_const(0x0c);
        let mut runtime = RetailRuntime::new_for_level(BOX_COUNT_GLOBAL + 1, current);
        let main = spawn_test_object(&mut runtime, ZONE, 5, 0, 0);
        let child = attach_test_child(&mut runtime, main, ZONE, 2);

        runtime
            .machine
            .upsert_object(
                VmObject::new(
                    main.vm,
                    vec![
                        misc(12, 1, 0x0be0),
                        // A later save deliberately changes the protected
                        // snapshot to the mounted level. The earlier load's
                        // captured restart kind must remain different-level.
                        misc(12, 0, 0x0be0),
                        Instruction::encode(0x11, 0x0805, 0x0e08),
                    ],
                )
                .unwrap(),
            )
            .unwrap();
        let mut child_vm = VmObject::new(
            child.vm,
            vec![
                // Read global 60 after the different-level load boundary,
                // then retain it in register zero for the assertion below.
                Instruction::encode(0x1f, 0x0be0, 0x083c),
                Instruction::encode(0x11, 0x0e1f, 0x0e00),
            ],
        )
        .unwrap();
        child_vm.set_register(0, 0x1234).unwrap();
        runtime.machine.upsert_object(child_vm).unwrap();
        runtime.saved_level_state = Some(level_snapshot(saved));
        runtime.set_level_state_context(level_context(ZONE, false, vec![ZONE]));
        runtime
            .machine
            .set_global_word(BONUS_ROUND_GLOBAL, 0x100)
            .unwrap();
        runtime
            .machine
            .set_global_word(NEXT_DISPLAY_GLOBAL, 0)
            .unwrap();

        let frame = runtime.run_frame(&mut SnapshotHost, 8).unwrap();

        assert_eq!(
            frame.effects,
            vec![
                VmEffect::LoadState {
                    object: main.vm,
                    saved_level: Some(saved),
                },
                VmEffect::SaveState(main.vm),
            ],
            "the browser retains the ordered remount handshake"
        );
        assert_eq!(frame.executions.len(), 2, "later objects still run");
        assert!(!runtime.machine.level_restart_requested());
        assert_eq!(
            runtime
                .machine
                .object(main.vm)
                .unwrap()
                .register(process_register::TRANSLATION_X),
            Ok(0x500),
            "GOOL continues after the different-level LoadState"
        );
        assert_eq!(
            runtime.machine.object(child.vm).unwrap().register(0),
            Ok(0),
            "later preorder GOOL observes native's synchronous bonus clear"
        );
        assert_eq!(
            runtime.saved_level_state().map(|snapshot| snapshot.level),
            Some(current),
            "the later SaveState really changes the mutable protected snapshot"
        );
        assert_eq!(
            runtime.machine.global_word(CURRENT_DISPLAY_GLOBAL),
            Ok(0),
            "the source display latch still completes"
        );
        assert_eq!(
            runtime
                .restart_saved_level_from_effect(&mut SnapshotHost, saved)
                .unwrap(),
            RetailRestartOutcome::DifferentLevel {
                saved_level: saved,
                requested_level_sentinel: -2,
            }
        );
    }

    #[test]
    fn hard_restart_broadcasts_then_terminates_zones_and_resets_crash() {
        let level = LevelId::new(0x03).unwrap();
        let mut runtime = RetailRuntime::new_for_level(119, level);
        let main = spawn_test_object(&mut runtime, ZONE, 5, 0, 0);
        let old_a = spawn_test_object(&mut runtime, ZONE, 6, 2, 0);
        let old_b = spawn_test_object(&mut runtime, ZONE_B, 7, 3, 0);
        for (register, value) in [
            (process_register::TRANSLATION_X, 100_i32),
            (process_register::TRANSLATION_Y, 200),
            (process_register::TRANSLATION_Z, 300),
            (process_register::ROTATION_Y, 0x111),
            (process_register::ROTATION_X, 0x222),
            (process_register::ROTATION_Z, 0x333),
            (process_register::SCALE_X, 0x1000),
            (process_register::SCALE_Y, 0x1001),
            (process_register::SCALE_Z, 0x1002),
        ] {
            runtime
                .machine
                .object_mut(main.vm)
                .unwrap()
                .set_register(register, value.cast_unsigned())
                .unwrap();
        }
        runtime.set_level_state_context(level_context(ZONE_B, false, vec![ZONE_B, ZONE]));
        runtime.save_level_state(main, false).unwrap();
        runtime.arena.spawn_table_mut().set_flags(42, 0x0f).unwrap();
        for (index, value) in [
            (SCREEN_SHAKE_GLOBAL, 0x11),
            (AMBIANCE_OBJECT_GLOBAL, 0x22),
            (BONUS_ROUND_GLOBAL, 0x100),
            (BOX_COUNT_GLOBAL, 0x900),
            (GEM_STAMP_GLOBAL, 0x33),
            (ISLAND_CAMERA_STATE_GLOBAL, 0x44),
            (IS_FIRST_ZONE_GLOBAL, 0),
            (TITLE_PAUSE_STATE_GLOBAL, 0x55),
            (CAPTION_OBJECT_GLOBAL, 0x66),
            (PBAK_STATE_GLOBAL, 0),
            (FADE_COUNTER_GLOBAL, 0x77),
            (FADE_STEP_GLOBAL, 0x88),
            (GAME_STATE_GLOBAL, 0x100),
        ] {
            runtime.set_global_word(index, value).unwrap();
        }
        for register in [
            process_register::TRANSLATION_X,
            process_register::TRANSLATION_Y,
            process_register::TRANSLATION_Z,
            process_register::SCALE_X,
            process_register::SCALE_Y,
            process_register::SCALE_Z,
            process_register::MISC_A_X,
            process_register::MISC_A_Y,
            process_register::MISC_A_Z,
            process_register::SPEED,
            process_register::FLOOR_IMPACT_STAMP,
        ] {
            runtime
                .machine
                .object_mut(main.vm)
                .unwrap()
                .set_register(register, 0xdead_beef)
                .unwrap();
        }
        runtime.draw_count = 99;

        let RetailRestartOutcome::Restarted(report) =
            runtime.restart_saved_level(&mut SnapshotHost).unwrap()
        else {
            panic!("same-level save must restart locally");
        };
        assert_eq!(report.level_update_flags, 1);
        assert_eq!(
            report
                .zone_reports
                .iter()
                .map(|(zone, _)| *zone)
                .collect::<Vec<_>>(),
            [ZONE_B, ZONE]
        );
        assert!(report.respawn_event_failures.is_empty());
        assert!(runtime.object_for_arena(old_a.arena).is_none());
        assert!(runtime.object_for_arena(old_b.arena).is_none());
        assert_eq!(runtime.object_for_arena(main.arena), Some(main));
        assert_eq!(runtime.arena.get(main.arena).unwrap().zone(), ZONE_B);
        let player = runtime.machine.object(main.vm).unwrap();
        for (register, expected) in [
            (process_register::TRANSLATION_X, 100_i32),
            (process_register::TRANSLATION_Y, 200),
            (process_register::TRANSLATION_Z, 300),
            (process_register::ROTATION_Y, 0),
            (process_register::ROTATION_X, 0),
            (process_register::ROTATION_Z, 0),
            (process_register::SCALE_X, 0x1000),
            (process_register::SCALE_Y, 0x1001),
            (process_register::SCALE_Z, 0x1002),
            (process_register::MISC_A_X, 0),
            (process_register::MISC_A_Y, 0),
            (process_register::MISC_A_Z, 0),
            (process_register::SPEED, 0),
            (process_register::FLOOR_IMPACT_STAMP, 0),
            (process_register::MISC_B_X, 0),
        ] {
            assert_eq!(player.register(register).unwrap().cast_signed(), expected);
        }
        assert_eq!(runtime.draw_count(), 0);
        assert_eq!(runtime.respawn_count, 0x100);
        assert_eq!(runtime.death_count, 0x100);
        assert_eq!(report.restored_box_count, 0);
        assert_eq!(runtime.level_state_context().unwrap().box_count, 0);
        for (index, expected) in [
            (SCREEN_SHAKE_GLOBAL, 0),
            (AMBIANCE_OBJECT_GLOBAL, 0),
            (BONUS_ROUND_GLOBAL, 0),
            (BOX_COUNT_GLOBAL, 0),
            (GEM_STAMP_GLOBAL, 0),
            (ISLAND_CAMERA_STATE_GLOBAL, 0),
            (IS_FIRST_ZONE_GLOBAL, 1),
            (TITLE_PAUSE_STATE_GLOBAL, 0),
            (CAPTION_OBJECT_GLOBAL, 0),
            (FADE_COUNTER_GLOBAL, 288),
            (FADE_STEP_GLOBAL, 32),
            (NEXT_DISPLAY_GLOBAL, INITIAL_DISPLAY_MASK),
        ] {
            assert_eq!(runtime.global_word(index), Ok(expected), "global {index}");
        }
        assert_eq!(
            runtime.arena.spawn_table().flags(42),
            Some(0x09),
            "LevelUpdate flag one clears spawn bits one and two"
        );
    }

    #[test]
    fn hard_restart_preserves_a_sparse_nonzero_main_vm_handle() {
        let level = LevelId::new(0x03).unwrap();
        let mut runtime = RetailRuntime::new_for_level(119, level);
        let lower_slots = [
            spawn_test_object(&mut runtime, ZONE, 10, 2, 0),
            spawn_test_object(&mut runtime, ZONE, 11, 2, 0),
            spawn_test_object(&mut runtime, ZONE, 12, 2, 0),
        ];
        let main = spawn_test_object(&mut runtime, ZONE, 5, 0, 0);
        assert_eq!(main.vm, VmObjectHandle::new(3).unwrap());
        runtime.set_level_state_context(level_context(ZONE, false, vec![ZONE]));
        runtime.save_level_state(main, false).unwrap();

        let RetailRestartOutcome::Restarted(_) =
            runtime.restart_saved_level(&mut SnapshotHost).unwrap()
        else {
            panic!("same-level save must restart locally");
        };

        for object in lower_slots {
            assert!(runtime.object_for_arena(object.arena).is_none());
            assert!(runtime.object_for_vm(object.vm).is_none());
        }
        assert_eq!(runtime.object_for_arena(main.arena), Some(main));
        assert_eq!(runtime.object_for_vm(main.vm), Some(main));
        assert!(runtime.machine.object(main.vm).is_ok());
    }

    #[test]
    fn first_spawn_restart_restores_spawn_words_and_checkpoint_box_count() {
        let level = LevelId::new(0x03).unwrap();
        let mut runtime = RetailRuntime::new_for_level(119, level);
        let main = spawn_test_object(&mut runtime, ZONE, 5, 0, 0);
        let mut context = level_context(ZONE, true, Vec::new());
        context.checkpoint_id = 7 << 8;
        runtime.set_level_state_context(context);
        runtime.save_level_state(main, true).unwrap();
        let snapshot = runtime.saved_level_state.as_mut().unwrap();
        snapshot.spawn_words[7] = 0xffff_ffff;
        snapshot.spawn_words[8] = 0x11;
        snapshot.box_count = 0x900;
        runtime.arena.spawn_table_mut().set_flags(7, 0).unwrap();
        runtime.arena.spawn_table_mut().set_flags(8, 0).unwrap();
        runtime.set_global_word(PBAK_STATE_GLOBAL, 2).unwrap();
        runtime
            .set_global_word(CAPTION_OBJECT_GLOBAL, 0x1234)
            .unwrap();

        let RetailRestartOutcome::Restarted(report) =
            runtime.restart_saved_level(&mut SnapshotHost).unwrap()
        else {
            panic!("same-level first spawn must restart locally");
        };
        assert_eq!(report.level_update_flags, 0);
        assert_eq!(runtime.arena.spawn_table().flags(7), Some(0xffff_fffc));
        assert_eq!(runtime.arena.spawn_table().flags(8), Some(0x10));
        assert_eq!(report.restored_box_count, 0x800);
        assert_eq!(runtime.global_word(BOX_COUNT_GLOBAL), Ok(0x800));
        assert_eq!(runtime.global_word(CAPTION_OBJECT_GLOBAL), Ok(0x1234));
        assert!(!runtime.level_state_context().unwrap().first_spawn);
        assert_eq!(runtime.respawn_count, 0);
        assert_eq!(runtime.death_count, 0);
    }

    #[test]
    fn restart_samples_checkpoint_globals_after_respawn_handlers() {
        let level = LevelId::new_const(0x03);
        let mut runtime = RetailRuntime::new_for_level(119, level);
        let main = spawn_test_object(&mut runtime, ZONE, 5, 0, 0);
        let mut context = level_context(ZONE, true, Vec::new());
        context.checkpoint_id = -1;
        context.checkpoint_translation = [700, 701, 702];
        runtime.set_level_state_context(context);
        runtime.save_level_state(main, true).unwrap();
        let snapshot = runtime.saved_level_state.as_mut().unwrap();
        snapshot.spawn_words[7] = u32::MAX;
        snapshot.spawn_words[8] = 0x11;
        snapshot.box_count = 0x900;
        runtime.arena.spawn_table_mut().set_flags(7, 0).unwrap();
        runtime.arena.spawn_table_mut().set_flags(8, 0).unwrap();
        runtime
            .machine
            .object_mut(main.vm)
            .unwrap()
            .configure_test_event_interrupt(
                RESPAWN_EVENT,
                vec![
                    Instruction::encode(0x20, 0x0807, 0x0845),
                    Instruction::encode(0x20, 0x0804, 0x0866),
                    Instruction::encode(0x20, 0x0805, 0x0867),
                    Instruction::encode(0x20, 0x0806, 0x0868),
                    0x8280_0000,
                ],
            )
            .unwrap();

        let RetailRestartOutcome::Restarted(report) =
            runtime.restart_saved_level(&mut SnapshotHost).unwrap()
        else {
            panic!("same-level first spawn must restart locally");
        };

        assert!(report.respawn_event_failures.is_empty());
        assert_eq!(runtime.arena.spawn_table().flags(7), Some(0xffff_fffc));
        assert_eq!(runtime.arena.spawn_table().flags(8), Some(0x10));
        assert_eq!(report.restored_box_count, 0x800);
        let context = runtime.level_state_context().unwrap();
        assert_eq!(context.checkpoint_id, 7 << 8);
        assert_eq!(context.checkpoint_translation, [4 << 8, 5 << 8, 6 << 8]);
    }

    #[test]
    fn different_level_restart_clears_bonus_before_early_return_only() {
        let current = LevelId::new_const(0x03);
        let saved = LevelId::new_const(0x09);
        let mut runtime = RetailRuntime::new_for_level(119, current);
        runtime.saved_level_state = Some(level_snapshot(saved));
        runtime.set_level_state_context(level_context(ZONE, false, vec![ZONE]));
        runtime.set_global_word(BONUS_ROUND_GLOBAL, 0x100).unwrap();
        runtime.set_global_word(BOX_COUNT_GLOBAL, 0x900).unwrap();

        assert_eq!(
            runtime.restart_saved_level(&mut SnapshotHost),
            Ok(RetailRestartOutcome::DifferentLevel {
                saved_level: saved,
                requested_level_sentinel: -2,
            })
        );
        assert_eq!(runtime.global_word(BONUS_ROUND_GLOBAL), Ok(0));
        assert!(runtime.level_state_context().unwrap().first_spawn);
        assert_eq!(
            runtime.global_word(BOX_COUNT_GLOBAL),
            Ok(0x900),
            "LevelInitMisc(0) runs only after the same-level path"
        );
    }
}
