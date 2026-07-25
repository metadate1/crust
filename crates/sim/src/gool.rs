//! Bounded, word-addressed GOOL virtual machine.
//!
//! Instructions retain the retail `opcode:8 | operand-a:12 | operand-b:12`
//! layout. Native pointers are never represented; objects, registers, pages,
//! events, and call targets are checked logical indices.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crust_formats::binary::{Eid, PageIndex};
use crust_formats::stream::{
    GOOL_PC_NONE, GoolAnimationDescriptor, GoolAnimationHeader, GoolFragmentAnimation, GoolProgram,
    GoolSpriteAnimation, GoolVertexAnimation, LevelId, ZoneEntity, ZoneEntityPathPoint,
    parse_gool_animation_descriptor, parse_gool_animation_header, structs::GoolState,
};

use crate::card::{CARD_SLOT_COUNT, CardPublishedState, SaveData};
use crate::math::{
    Angle12, Angles, Bounds3, Vec2, Vec3, approximate_distance, euclidean_distance, integer_sqrt,
    seek,
};
use crate::object_arena::{OBJECT_ARENA_CAPACITY, OBJECT_POOL_CAPACITY, SPAWN_TABLE_CAPACITY};
use crate::object_bounds::{
    BoundTransform, FrameBound, FrameBounds, FrameBoundsError, bounds_intersect_asymmetric,
    point_in_bound, retail_yxy_transform,
};
use crate::paging::{PHYSICAL_SLOT_COUNT, PageInvalidations};
use crate::retail_physics::{
    RetailAngles, RetailPhysicsContext, RetailPhysicsPlan, RetailPhysicsResult, RetailPhysicsState,
    RetailTranslationMode, apply_free_movement, apply_path_orientation, begin_retail_physics,
    finalize_retail_physics, path_orientation_requested, rotate_toward,
};
use crate::retail_solid_motion::{
    ObjectCollisionLinks, ObjectCollisionState, STATUS_HOTSPOT_COLLISION, SmoothStopMemory,
    SolidColliderState, SolidEffect, SolidLevelQuirks, SolidMotionContext, SolidMotionError,
    SolidMotionState, SolidObjectCandidate, SolidObjectZone, SolidQuery, SolidZoneBoundary,
    SolidZoneView, resolve_object_collision, solve_retail_solid_motion_with_event_handler,
};

/// Maximum simultaneous VM identities: the 96-object retail pool plus its
/// separately allocated player/main object.
pub const MAX_OBJECTS: usize = OBJECT_ARENA_CAPACITY;
/// Exact `gool_object.regs[0x1FC]` word span from the retail 32-bit layout.
pub const REGISTER_COUNT: usize = 0x1fc;
pub const TABLE_WORD_COUNT: usize = 1024;
pub const MAX_STACK_WORDS: usize = 256;
pub const MAX_CALL_DEPTH: usize = 64;
/// Native send-event instructions carry at most three argument-count bits.
pub const MAX_EVENT_ARGUMENTS: usize = 7;
pub const MAX_EFFECTS: usize = 256;
/// Defensive host bound for one retail `once_p` invocation. Retail runs the
/// block synchronously until its return link; the bound turns malformed or
/// recursive input into a typed failure instead of hanging the browser.
pub const MAX_ONCE_INSTRUCTIONS: usize = 16_384;
/// Defensive bound for one synchronous event-service or mapped-interrupt
/// interpreter invocation.
pub const MAX_EVENT_SERVICE_INSTRUCTIONS: usize = 16_384;
/// Defensive host bound for one synchronous retail transition-block
/// invocation. Retail has no native instruction limit; malformed local data
/// becomes a typed failure here instead of hanging the browser's 30 Hz loop.
pub const MAX_TRANSITION_INSTRUCTIONS: usize = 16_384;
pub const RETAIL_PAD_COUNT: usize = 2;
/// Exact halfword capacity of native `level_spawns`, the process-lifetime
/// encountered-object registry. This is distinct from the 304-word active
/// spawn table mirrored by [`Machine::spawn_flags`].
pub const RETAIL_LEVEL_SPAWN_CAPACITY: usize = 3_592;
/// GOOL global written by title/pause scripts and latched at frame end.
pub const NEXT_DISPLAY_GLOBAL: usize = 4;
/// Frozen display/camera/animation mask consumed during the current frame.
pub const CURRENT_DISPLAY_GLOBAL: usize = 9;
pub const INITIAL_DISPLAY_MASK: u32 = 0xffff;
pub const CURRENT_LEVEL_GLOBAL: usize = 0;
pub const CAMERA_ROTATION_GLOBAL: usize = 15;
pub const GAME_STATE_GLOBAL: usize = 17;
pub const TITLE_STATE_GLOBAL: usize = 18;
pub const SAVED_TITLE_STATE_GLOBAL: usize = 19;
pub const CURRENT_MAP_LEVEL_GLOBAL: usize = 20;
pub const LIFE_COUNT_GLOBAL: usize = 24;
pub const INITIAL_LIFE_COUNT_GLOBAL: usize = 31;
pub const MONO_GLOBAL: usize = 33;
pub const SFX_VOLUME_GLOBAL: usize = 34;
pub const MUSIC_VOLUME_GLOBAL: usize = 35;
pub const CAMERA_TRANSLATION_GLOBAL: usize = 37;
pub const CAMERA_ROTATION_YXZ_GLOBAL: usize = 40;
pub const TICKS_CURRENT_FRAME_GLOBAL: usize = 43;
pub const LEVEL_COUNT_GLOBAL: usize = 46;
pub const LEVELS_UNLOCKED_GLOBAL: usize = 47;
pub const DRAW_COUNT_GLOBAL: usize = 79;
const CHECKPOINT_ID_GLOBAL: usize = 69;
pub const CARD_FLAGS_GLOBAL: usize = 59;
pub const CARD_PART_COUNT_GLOBAL: usize = 61;
pub const CARD_PARTINFOS_GLOBAL: usize = 82;
pub const UNKNOWN_6190C_GLOBAL: usize = 32;
pub const ITEM_POOL_1_GLOBAL: usize = 63;
pub const ITEM_POOL_2_GLOBAL: usize = 72;
pub const GEM_COUNT_GLOBAL: usize = 97;
pub const KEY_COUNT_GLOBAL: usize = 98;
/// Halfword count in the retail `gool_colors` union.
pub const COLOR_COUNT: usize = 24;
const COLOR_INTENSITY_START: usize = 21;
const COLOR_INTENSITY_END: usize = 24;
const HIT_EVENT: u32 = 0x0300;
const HIT_INVINCIBLE_EVENT: u32 = 0x0a00;
const STATUS_EVENT: u32 = 0x0f00;
const WIN_BOSS_EVENT: u32 = 0x1d00;
const EVENT_CLEAR_GUARD_STATUS: u32 = 0x1800;
const SQUASH_EVENT: u32 = 0x1900;
const BOULDER_SQUASH_EVENT: u32 = 0x2500;
const EVENT_MAP_NULL_STATE: u16 = 0x00ff;
const STATUS_A_EVENT_SQUASHED: u32 = 0x0001_0000;
const STATUS_A_KEEP_EVENT_STACK: u32 = 0x0002_0000;
const STATUS_B_DPAD_CONTROL: u32 = 0x0000_0080;
const STATUS_B_MAIN_COLOR_BY_ZONE: u32 = 0x0400_0000;
/// Fourteen-bit retail code/PC address space.
pub const MAX_CODE_WORDS: usize = 1 << 14;
pub const NULL_INPUT_VALUE: u32 = 3;
/// `OptionsC` state two reads interrupter register `0x40` through this exact
/// word once before its controller's first all-root event supplies link seven.
/// The NTSC-U PS1 low-memory read at that null-derived address observes zero.
const OPTIONS_NULL_INTERRUPTER_LOAD: u32 = 0x1c30_7840;
const ANIMATION_REFERENCE_TAG: u32 = 0xa700_0000;
const ANIMATION_REFERENCE_MASK: u32 = 0x00ff_ffff;
const CODE_REFERENCE_TAG: u32 = 0xa600_0000;
const CODE_REFERENCE_GLOBAL: u32 = 0x0080_0000;
const CODE_REFERENCE_PC_BITS: u32 = 0x0000_3fff;
const CODE_REFERENCE_PC_SHIFT: u32 = 2;
const CODE_REFERENCE_PC_MASK: u32 = CODE_REFERENCE_PC_BITS << CODE_REFERENCE_PC_SHIFT;
const CODE_REFERENCE_PAYLOAD_MASK: u32 = CODE_REFERENCE_GLOBAL | CODE_REFERENCE_PC_MASK;
const STORAGE_REFERENCE_TAG: u32 = 0xa500_0000;
const STORAGE_REFERENCE_REGION_SHIFT: u32 = 22;
const STORAGE_REFERENCE_REGION_MASK: u32 = 3 << STORAGE_REFERENCE_REGION_SHIFT;
const STORAGE_REFERENCE_OBJECT_SHIFT: u32 = 15;
const STORAGE_REFERENCE_OBJECT_BITS: u32 = 0x7f;
const STORAGE_REFERENCE_OBJECT_MASK: u32 =
    STORAGE_REFERENCE_OBJECT_BITS << STORAGE_REFERENCE_OBJECT_SHIFT;
const STORAGE_REFERENCE_INDEX_BITS: u32 = 0x1fff;
const STORAGE_REFERENCE_INDEX_SHIFT: u32 = 2;
const STORAGE_REFERENCE_INDEX_MASK: u32 =
    STORAGE_REFERENCE_INDEX_BITS << STORAGE_REFERENCE_INDEX_SHIFT;
const STORAGE_REFERENCE_PAYLOAD_MASK: u32 =
    STORAGE_REFERENCE_REGION_MASK | STORAGE_REFERENCE_OBJECT_MASK | STORAGE_REFERENCE_INDEX_MASK;
/// Checked stand-in for an address into one physical retail object-pool slot.
///
/// Native linked GOP translation returns `&link->regs[index]` even after the
/// linked object has been reclaimed. Keeping this tag separate from logical
/// object storage makes that static-pool lifetime explicit without exposing a
/// host pointer. Seven slot bits cover the 96 ordinary allocations plus the
/// separately allocated main object; nine register bits cover all 508 words.
const RETAIL_POOL_STORAGE_REFERENCE_TAG: u32 = 0xa100_0000;
const RETAIL_POOL_STORAGE_REFERENCE_SLOT_SHIFT: u32 = 11;
const RETAIL_POOL_STORAGE_REFERENCE_SLOT_BITS: u32 = 0x7f;
const RETAIL_POOL_STORAGE_REFERENCE_SLOT_MASK: u32 =
    RETAIL_POOL_STORAGE_REFERENCE_SLOT_BITS << RETAIL_POOL_STORAGE_REFERENCE_SLOT_SHIFT;
const RETAIL_POOL_STORAGE_REFERENCE_REGISTER_SHIFT: u32 = 2;
const RETAIL_POOL_STORAGE_REFERENCE_REGISTER_BITS: u32 = 0x1ff;
const RETAIL_POOL_STORAGE_REFERENCE_REGISTER_MASK: u32 =
    RETAIL_POOL_STORAGE_REFERENCE_REGISTER_BITS << RETAIL_POOL_STORAGE_REFERENCE_REGISTER_SHIFT;
const RETAIL_POOL_STORAGE_REFERENCE_PAYLOAD_MASK: u32 =
    RETAIL_POOL_STORAGE_REFERENCE_SLOT_MASK | RETAIL_POOL_STORAGE_REFERENCE_REGISTER_MASK;
/// Checked replacement for a relocated `zone_entity *`/`mdat_entity *`.
///
/// Entity pointers are copied by ordinary GOOL MOV instructions (Ripper
/// Roo's falling TNT objects are one retail example), so the identity must
/// live in the 32-bit process word rather than only beside the object that was
/// originally spawned from the entity. The machine-owned table keeps the
/// validated path alive after the authored parent or a physical pool slot is
/// reclaimed, without retaining a native pointer into user-supplied bytes.
const ENTITY_REFERENCE_TAG: u32 = 0xa000_0000;
const ENTITY_REFERENCE_SLOT_BITS: u32 = 0x003f_ffff;
const ENTITY_REFERENCE_SLOT_SHIFT: u32 = 2;
const ENTITY_REFERENCE_PAYLOAD_MASK: u32 =
    ENTITY_REFERENCE_SLOT_BITS << ENTITY_REFERENCE_SLOT_SHIFT;
const ENTRY_REFERENCE_TAG: u32 = 0xa400_0000;
const ENTRY_REFERENCE_SLOT_BITS: u32 = 0x003f_ffff;
const ENTRY_REFERENCE_SLOT_SHIFT: u32 = 2;
const ENTRY_REFERENCE_PAYLOAD_MASK: u32 = ENTRY_REFERENCE_SLOT_BITS << ENTRY_REFERENCE_SLOT_SHIFT;
const COLLISION_OBJECT_REFERENCE_TAG: u32 = 0xa300_0000;
const COLLISION_OBJECT_REFERENCE_BITS: u32 = 0x7f;
const COLLISION_OBJECT_REFERENCE_SHIFT: u32 = 2;
const COLLISION_OBJECT_REFERENCE_MASK: u32 =
    COLLISION_OBJECT_REFERENCE_BITS << COLLISION_OBJECT_REFERENCE_SHIFT;
/// Checked aligned stand-in for native `&free_objects`.
///
/// This deliberately is not an object reference: retail's free-list parent is
/// a `gool_handle *`, not a `gool_object *`. Keeping a distinct tag preserves
/// raw word identity without allowing ordinary object resolution to treat the
/// allocator root as a live process.
const RETAIL_FREE_LIST_ROOT_REFERENCE: u32 = 0xa200_0000;
const PROCESS_LINK_PARENT: usize = 1;
const PROCESS_LINK_SIBLING: usize = 2;
const PROCESS_LINK_CHILDREN: usize = 3;
const PROCESS_LINK_COLLIDER: usize = 6;
const EVENT_ARGUMENT_REFERENCE_TAG: u32 = 0xc000_0000;
const EVENT_ARGUMENT_REFERENCE_GENERATION_BITS: u32 = 0x0fff_ffff;
const EVENT_ARGUMENT_REFERENCE_GENERATION_SHIFT: u32 = 2;
const EVENT_ARGUMENT_REFERENCE_PAYLOAD_MASK: u32 =
    EVENT_ARGUMENT_REFERENCE_GENERATION_BITS << EVENT_ARGUMENT_REFERENCE_GENERATION_SHIFT;
const MAX_EVENT_ARGUMENT_SCOPES: usize = MAX_CALL_DEPTH;
const INITIAL_FRAME_FLAGS: u32 = 0xffff;
const STATE_FRAME_WORDS: usize = 3;
const INITIAL_FRAME_WORDS: usize = 4;
const ONCE_FRAME_WORDS: usize = 3;
const SYNTHETIC_STACK_POINTER: usize = REGISTER_COUNT - MAX_STACK_WORDS;
const NORMAL_INTERPRETER_FLAGS: u32 = 4;
const MOVE_DIRECTIONS: [u32; 16] = [8, 0, 2, 1, 4, 8, 3, 8, 6, 7, 8, 8, 5, 8, 8, 8];
const PROCESS_VECTOR_COUNT: usize = 6;
const PROCESS_VECTOR_WORDS: usize = 3;
const PROCESS_VECTOR_BASE: usize = process_register::TRANSLATION_X;
const STATUS_A_TOWARD_GOAL: u32 = 0x4;
const STATUS_A_CHANGE_PATH_DIRECTION: u32 = 0x10;
const STATUS_A_INVALID_PATH: u32 = 0x200;
const STATUS_B_TRACK_PATH_ROTATION: u32 = 0x2;
const STATUS_B_TRACK_PATH_SIGN: u32 = 0x4;
const STATUS_B_TRACK_PATH_PITCH: u32 = 0x800;
const STATUS_B_ORIENT_ON_PATH: u32 = 0x8000;
/// Delta-bit encoding of the source PC `atan_table[1024]`. Every table step
/// is zero or one, so prefix popcount reproduces all exact 12-bit values in
/// 128 bytes instead of carrying a second 2 KiB integer table.
const RETAIL_ATAN_INCREMENTS: [u64; 16] = [
    0x6d6d_6dad_b5b6_b6d6,
    0xd6d6_dada_db5b_5b6b,
    0x5ada_dad6_d6d6_d6d6,
    0x5ad6_d6b5_ad6d_6b5b,
    0x6b56_b5ab_5ad6_b56b,
    0xd5aa_d5ab_56ad_5ab5,
    0xd556_aad5_5aab_55aa,
    0xaaaa_aaad_5555_aaaa,
    0xaaaa_aa55_5555_55aa,
    0xa555_2aaa_5555_4aaa,
    0x4a95_2a95_4aa9_54aa,
    0x4a54_a52a_54a5_4a95,
    0xa525_294a_4a52_9529,
    0x2524_a494_9494_94a4,
    0x9249_24a4_924a_4929,
    0x8924_9249_2492_4924,
];

/// Exact five-word input history consumed by retail GOOL control tests.
///
/// The browser platform owns physical/demo input normalization. Keeping this
/// snapshot in the VM makes opcode `0x1a` deterministic and prevents GOOL
/// from depending on browser APIs directly.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetailPadSnapshot {
    pub tapped: u32,
    pub held: u32,
    pub held_previous: u32,
    pub tapped_previous: u32,
    pub held_previous_2: u32,
}

/// Exact word indices in retail `gool_process`, relative to the start of the
/// process union. Pointer-bearing fields stay represented by checked handles
/// elsewhere; scalar/vector words retain their original indices so GOOL
/// object-register operands observe the retail layout.
pub mod process_register {
    pub const TRANSLATION_X: usize = 8;
    pub const TRANSLATION_Y: usize = 9;
    pub const TRANSLATION_Z: usize = 10;
    /// Retail `ang` order is Y, X, Z.
    pub const ROTATION_Y: usize = 11;
    pub const ROTATION_X: usize = 12;
    pub const ROTATION_Z: usize = 13;
    pub const SCALE_X: usize = 14;
    pub const SCALE_Y: usize = 15;
    pub const SCALE_Z: usize = 16;
    pub const MISC_A_X: usize = 17;
    pub const MISC_A_Y: usize = 18;
    pub const MISC_A_Z: usize = 19;
    pub const MISC_B_Y: usize = 20;
    pub const MISC_B_X: usize = 21;
    pub const MISC_B_Z: usize = 22;
    pub const MODE_FLAGS_A: usize = 23;
    pub const MODE_FLAGS_B: usize = 24;
    pub const MODE_FLAGS_C: usize = 25;
    pub const STATUS_A: usize = 26;
    pub const STATUS_B: usize = 27;
    pub const STATUS_C: usize = 28;
    pub const SUBTYPE: usize = 29;
    pub const PID_FLAGS: usize = 30;
    /// Native process pointers. The checked VM keeps their authoritative
    /// values in typed interpreter state, so their raw union words must never
    /// inherit pointer bits from a previous physical-pool occupant.
    pub const STACK_POINTER: usize = 31;
    pub const PROGRAM_COUNTER: usize = 32;
    pub const FRAME_POINTER: usize = 33;
    pub const TRANSITION_POINTER: usize = 34;
    pub const EVENT_POINTER: usize = 35;
    pub const ONCE_POINTER: usize = 36;
    pub const MISC_VALUE: usize = 37;
    pub const ACK: usize = 38;
    pub const ANIMATION_STAMP: usize = 39;
    pub const STATE_STAMP: usize = 40;
    pub const ANIMATION_COUNTER: usize = 41;
    pub const ANIMATION_SEQUENCE: usize = 42;
    pub const ANIMATION_FRAME: usize = 43;
    pub const ENTITY_REFERENCE: usize = 44;
    pub const PATH_PROGRESS: usize = 45;
    pub const PATH_LENGTH: usize = 46;
    pub const FLOOR_Y: usize = 47;
    pub const STATE_FLAGS: usize = 48;
    pub const SPEED: usize = 49;
    pub const INVINCIBILITY_STATE: usize = 50;
    pub const INVINCIBILITY_STAMP: usize = 51;
    pub const FLOOR_IMPACT_STAMP: usize = 52;
    pub const FLOOR_IMPACT_VELOCITY: usize = 53;
    pub const SIZE: usize = 54;
    pub const EVENT: usize = 55;
    pub const CAMERA_ZOOM: usize = 56;
    pub const ANGULAR_VELOCITY_Y: usize = 57;
    pub const HOTSPOT_SIZE: usize = 58;
    pub const VOICE_ID: usize = 59;
    pub const UNKNOWN_150: usize = 60;
    pub const UNKNOWN_154: usize = 61;
    pub const NODE: usize = 62;
}

const SCALE_X_REGISTER: usize = process_register::SCALE_X;
const INITIAL_SCALE: i32 = 0x1000;
const INITIAL_STATUS_A: u32 = 0x0002_0020;
const INITIAL_NODE: u32 = 0xffff;
const INITIAL_VOICE_ID: i32 = -2;

/// A checked GOOL instruction-space selector. Retail executable code lives in
/// the external entry while opcode `0x86` calls absolute offsets in the
/// executable's shared/global code item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodeSegment {
    External,
    Global,
}

/// Logical instruction address used instead of storing native code pointers.
/// The packed representation stores its 14-bit word PC above two zero low
/// bits, preserving the alignment discriminator of a retail code pointer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodeAddress {
    pub segment: CodeSegment,
    pub pc: usize,
}

impl CodeAddress {
    /// Decodes one pointer-free GOOL code word. Reserved payload bits must be
    /// zero so arbitrary process values can never become executable offsets.
    #[must_use]
    pub const fn from_word(word: u32) -> Option<Self> {
        if word & !CODE_REFERENCE_PAYLOAD_MASK != CODE_REFERENCE_TAG {
            return None;
        }
        Some(Self {
            segment: if word & CODE_REFERENCE_GLOBAL == 0 {
                CodeSegment::External
            } else {
                CodeSegment::Global
            },
            pc: ((word & CODE_REFERENCE_PC_MASK) >> CODE_REFERENCE_PC_SHIFT) as usize,
        })
    }

    #[must_use]
    pub const fn to_word(self) -> u32 {
        CODE_REFERENCE_TAG
            | match self.segment {
                CodeSegment::External => 0,
                CodeSegment::Global => CODE_REFERENCE_GLOBAL,
            }
            | (((self.pc as u32) & CODE_REFERENCE_PC_BITS) << CODE_REFERENCE_PC_SHIFT)
    }
}

/// Pointer-free reference to a byte offset in one object's global animation
/// item. The packed word remains 32-bit for GOOL register compatibility.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AnimationReference {
    offset: u32,
}

impl AnimationReference {
    fn checked(offset: usize, animation_len: usize) -> Result<Self, VmError> {
        if offset >= animation_len || offset > ANIMATION_REFERENCE_MASK as usize {
            return Err(VmError::InvalidAnimationOffset(offset));
        }
        Ok(Self {
            offset: offset as u32,
        })
    }

    #[must_use]
    pub const fn offset(self) -> u32 {
        self.offset
    }

    #[must_use]
    pub const fn to_word(self) -> u32 {
        ANIMATION_REFERENCE_TAG | self.offset
    }

    #[must_use]
    pub const fn from_word(word: u32) -> Option<Self> {
        if word & !ANIMATION_REFERENCE_MASK == ANIMATION_REFERENCE_TAG {
            Some(Self {
                offset: word & ANIMATION_REFERENCE_MASK,
            })
        } else {
            None
        }
    }
}

/// A checked type-four descriptor stored outside global animation item five.
///
/// Native permits LEA to point `anim_seq` into any object-owned word region.
/// Text still resolves its font word offset against global item five, but its
/// NUL-delimited terms remain in the aliased region represented here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessTextAnimation {
    pub header: GoolAnimationHeader,
    pub unknown_word: u32,
    pub font_word_offset: u32,
    pub terms: Vec<Vec<u8>>,
}

/// Descriptor kind read through a checked LEA-created animation pointer.
///
/// Retail GOOL can use opcode `0x14` (LEA) to point `anim_seq` at words in
/// the object's internal table or process image instead of global item five.
/// Known types retain their fully validated owned payload so a render
/// snapshot cannot observe later linked-register mutation. Native's transform
/// switch has no default body, so every other type byte is an intentional
/// no-draw descriptor with the standard non-vertex collision bound.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProcessAnimationKind {
    /// Native's transform-switch default: a non-null animation that draws no
    /// primitives and uses the standard non-vertex collision box. This covers
    /// type zero plus unknown bytes used by retail timer/data aliases.
    NoDraw,
    Vertex(GoolVertexAnimation),
    Sprite(GoolSpriteAnimation),
    /// Type three is itself a packed font resource. Selecting it as the live
    /// animation emits no primitives, so only the consumed common header must
    /// be present in the aliased region.
    Font(GoolAnimationHeader),
    Text(ProcessTextAnimation),
    Fragment(GoolFragmentAnimation),
}

/// One live, bounds-checked animation descriptor selected through GOOL
/// storage. The token preserves a same-object internal/register alias, a
/// process-global rotating-constant slot, or a physical retail-pool register
/// address without exposing a native pointer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessAnimationReference {
    storage: StorageReference,
    kind: ProcessAnimationKind,
}

impl ProcessAnimationReference {
    #[must_use]
    pub const fn storage(&self) -> StorageReference {
        self.storage
    }

    #[must_use]
    pub const fn kind(&self) -> &ProcessAnimationKind {
        &self.kind
    }
}

/// Checked replacement for retail's `gool_anim *` union of pointer sources.
///
/// Item-five descriptors retain their compact byte offset. LEA-created
/// process descriptors retain a checked [`StorageReference`] plus the source
/// words needed by downstream simulation. Both variants fit in owned render
/// snapshots and neither can outlive or dereference native memory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnimationSource {
    ItemFive(AnimationReference),
    Process(ProcessAnimationReference),
}

impl AnimationSource {
    #[must_use]
    pub const fn item_five_reference(&self) -> Option<AnimationReference> {
        match self {
            Self::ItemFive(reference) => Some(*reference),
            Self::Process(_) => None,
        }
    }
}

fn animation_words_as_bytes(words: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(words.len().saturating_mul(4));
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes
}

fn parse_process_text_animation(words: &[u32]) -> Result<ProcessTextAnimation, ()> {
    let bytes = animation_words_as_bytes(words);
    let header = parse_gool_animation_header(&bytes, 0).map_err(|_| ())?;
    if bytes.len() < 12 {
        return Err(());
    }
    let unknown_word = u32::from_le_bytes(bytes[4..8].try_into().map_err(|_| ())?);
    let font_word_offset = u32::from_le_bytes(bytes[8..12].try_into().map_err(|_| ())?);
    let mut cursor = 12_usize;
    let mut terms = Vec::with_capacity(usize::from(header.length));
    for _ in 0..header.length {
        let remaining = bytes.get(cursor..).ok_or(())?;
        let length = remaining.iter().position(|byte| *byte == 0).ok_or(())?;
        let end = cursor.checked_add(length).ok_or(())?;
        terms.push(bytes.get(cursor..end).ok_or(())?.to_vec());
        cursor = end.checked_add(1).ok_or(())?;
    }
    Ok(ProcessTextAnimation {
        header,
        unknown_word,
        font_word_offset,
        terms,
    })
}

fn parse_process_animation_reference(
    storage: StorageReference,
    words: &[u32],
) -> Result<ProcessAnimationReference, VmError> {
    let word = storage.to_word();
    let header_word = words
        .first()
        .copied()
        .ok_or(VmError::InvalidAnimationReference(word))?;
    let raw_type = header_word.to_le_bytes()[0];
    let kind = match raw_type {
        1 | 2 | 5 => {
            let bytes = animation_words_as_bytes(words);
            match parse_gool_animation_descriptor(&bytes, 0)
                .map_err(|_| VmError::InvalidAnimationReference(word))?
            {
                GoolAnimationDescriptor::Vertex(value) => ProcessAnimationKind::Vertex(value),
                GoolAnimationDescriptor::Sprite(value) => ProcessAnimationKind::Sprite(value),
                GoolAnimationDescriptor::Fragment(value) => ProcessAnimationKind::Fragment(value),
                GoolAnimationDescriptor::Font(_) | GoolAnimationDescriptor::Text(_) => {
                    return Err(VmError::InvalidAnimationReference(word));
                }
            }
        }
        3 => {
            let bytes = header_word.to_le_bytes();
            let header = parse_gool_animation_header(&bytes, 0)
                .map_err(|_| VmError::InvalidAnimationReference(word))?;
            ProcessAnimationKind::Font(header)
        }
        4 => ProcessAnimationKind::Text(
            parse_process_text_animation(words)
                .map_err(|()| VmError::InvalidAnimationReference(word))?,
        ),
        // Native's transform switch has no default body. Its local-bound
        // path tests only `type == 1`, so every other byte is a live,
        // non-vertex, no-draw animation rather than malformed input.
        _ => ProcessAnimationKind::NoDraw,
    };
    Ok(ProcessAnimationReference { storage, kind })
}

/// Immutable identity of the global GOOL program that owns an object's
/// animation item and retail display category.
///
/// Keeping these fields on the VM object prevents hosts from reconstructing
/// render metadata through an arena slot or executable number after either
/// handle has been recycled. Objects authored directly with [`VmObject::new`]
/// intentionally have no parsed-program identity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GoolProgramIdentity {
    global_eid: Eid,
    object_type: u32,
    category: u32,
}

impl GoolProgramIdentity {
    #[must_use]
    pub const fn global_eid(self) -> Eid {
        self.global_eid
    }

    /// Retail GOOL header type used by native update/physics special cases.
    #[must_use]
    pub const fn object_type(self) -> u32 {
        self.object_type
    }

    #[must_use]
    pub const fn category(self) -> u32 {
        self.category
    }
}

/// One bounded storage region addressable by opcode `0x26`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum StorageRegion {
    Internal = 0,
    External = 1,
    Register = 2,
    Constant = 3,
}

/// Backing storage retained by one translated GOOL operand.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum StorageBacking {
    /// Storage owned by one currently live logical VM object.
    Object(ObjectHandle),
    /// Register storage at one physical native object-pool address.
    ///
    /// The slot can be live or reclaimed. Reuse of the same slot therefore
    /// retargets the reference exactly like a pointer into native's static
    /// object array.
    RetailPool(u8),
}

/// Pointer-free encoding of a translated GOOL input operand.
///
/// Retail pushes native addresses from opcode `0x26`. The Rust VM instead
/// packs either an object handle or physical retail-pool slot, storage region,
/// and checked word index under tags disjoint from code and animation
/// references. Its low two bits remain zero like the source word pointer, so
/// it cannot be mistaken for a named EID.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StorageReference {
    backing: StorageBacking,
    region: StorageRegion,
    index: u16,
}

impl StorageReference {
    pub fn checked(
        object: ObjectHandle,
        region: StorageRegion,
        index: usize,
    ) -> Result<Self, VmError> {
        if index > STORAGE_REFERENCE_INDEX_BITS as usize {
            return Err(VmError::InvalidStorageReference(STORAGE_REFERENCE_TAG));
        }
        Ok(Self {
            backing: StorageBacking::Object(object),
            region,
            index: index as u16,
        })
    }

    fn retail_pool_register(pool_slot: u8, register: usize) -> Result<Self, VmError> {
        if usize::from(pool_slot) >= MAX_OBJECTS || register >= REGISTER_COUNT {
            return Err(VmError::InvalidStorageReference(
                RETAIL_POOL_STORAGE_REFERENCE_TAG,
            ));
        }
        Ok(Self {
            backing: StorageBacking::RetailPool(pool_slot),
            region: StorageRegion::Register,
            index: register as u16,
        })
    }

    fn checked_offset(self, offset: usize) -> Result<Self, VmError> {
        let index = usize::from(self.index)
            .checked_add(offset)
            .ok_or(VmError::InvalidStorageReference(self.to_word()))?;
        match self.backing {
            StorageBacking::Object(object) => Self::checked(object, self.region, index),
            StorageBacking::RetailPool(pool_slot) => Self::retail_pool_register(pool_slot, index),
        }
    }

    #[must_use]
    pub const fn object(self) -> Option<ObjectHandle> {
        match self.backing {
            StorageBacking::Object(object) => Some(object),
            StorageBacking::RetailPool(_) => None,
        }
    }

    #[must_use]
    pub const fn retail_pool_slot(self) -> Option<u8> {
        match self.backing {
            StorageBacking::Object(_) => None,
            StorageBacking::RetailPool(pool_slot) => Some(pool_slot),
        }
    }

    #[must_use]
    pub const fn region(self) -> StorageRegion {
        self.region
    }

    #[must_use]
    pub const fn index(self) -> u16 {
        self.index
    }

    #[must_use]
    pub const fn to_word(self) -> u32 {
        match self.backing {
            StorageBacking::Object(object) => {
                STORAGE_REFERENCE_TAG
                    | ((self.region as u32) << STORAGE_REFERENCE_REGION_SHIFT)
                    | ((object.get() as u32) << STORAGE_REFERENCE_OBJECT_SHIFT)
                    | ((self.index as u32) << STORAGE_REFERENCE_INDEX_SHIFT)
            }
            StorageBacking::RetailPool(pool_slot) => {
                RETAIL_POOL_STORAGE_REFERENCE_TAG
                    | ((pool_slot as u32) << RETAIL_POOL_STORAGE_REFERENCE_SLOT_SHIFT)
                    | ((self.index as u32) << RETAIL_POOL_STORAGE_REFERENCE_REGISTER_SHIFT)
            }
        }
    }

    #[must_use]
    pub const fn from_word(word: u32) -> Option<Self> {
        if word & !STORAGE_REFERENCE_PAYLOAD_MASK == STORAGE_REFERENCE_TAG {
            let region = match (word >> STORAGE_REFERENCE_REGION_SHIFT) & 3 {
                0 => StorageRegion::Internal,
                1 => StorageRegion::External,
                2 => StorageRegion::Register,
                3 => StorageRegion::Constant,
                _ => unreachable!(),
            };
            let Some(object) = ObjectHandle::new(
                ((word >> STORAGE_REFERENCE_OBJECT_SHIFT) & STORAGE_REFERENCE_OBJECT_BITS) as u16,
            ) else {
                return None;
            };
            return Some(Self {
                backing: StorageBacking::Object(object),
                region,
                index: ((word & STORAGE_REFERENCE_INDEX_MASK) >> STORAGE_REFERENCE_INDEX_SHIFT)
                    as u16,
            });
        }

        if word & !RETAIL_POOL_STORAGE_REFERENCE_PAYLOAD_MASK != RETAIL_POOL_STORAGE_REFERENCE_TAG {
            return None;
        }
        let pool_slot = ((word >> RETAIL_POOL_STORAGE_REFERENCE_SLOT_SHIFT)
            & RETAIL_POOL_STORAGE_REFERENCE_SLOT_BITS) as u8;
        let register = ((word & RETAIL_POOL_STORAGE_REFERENCE_REGISTER_MASK)
            >> RETAIL_POOL_STORAGE_REFERENCE_REGISTER_SHIFT) as u16;
        if pool_slot as usize >= MAX_OBJECTS || register as usize >= REGISTER_COUNT {
            return None;
        }
        Some(Self {
            backing: StorageBacking::RetailPool(pool_slot),
            region: StorageRegion::Register,
            index: register,
        })
    }
}

/// Stable index into [`Machine`]'s validated retail-entity table.
///
/// Low bits remain clear like the source pointer, and reserved payload bits
/// are rejected so arbitrary scalar process values cannot select a path.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct EntityReference {
    slot: u32,
}

impl EntityReference {
    #[must_use]
    const fn to_word(self) -> u32 {
        ENTITY_REFERENCE_TAG | (self.slot << ENTITY_REFERENCE_SLOT_SHIFT)
    }

    #[must_use]
    const fn from_word(word: u32) -> Option<Self> {
        if word & !ENTITY_REFERENCE_PAYLOAD_MASK == ENTITY_REFERENCE_TAG {
            Some(Self {
                slot: (word & ENTITY_REFERENCE_PAYLOAD_MASK) >> ENTITY_REFERENCE_SLOT_SHIFT,
            })
        } else {
            None
        }
    }
}

/// Stable, checked replacement for one relocated retail `entry *`.
///
/// The packed payload indexes a machine-owned `(EID, page)` record. This is
/// deliberately distinct from [`StorageReference`]: after `NSOpen` resolves
/// an EID, retail's `misc_entry` remains bound to that entry even if GOOL
/// overwrites the cell that originally held the EID. Slots are shifted above
/// two zero low bits to retain the aligned-pointer/EID discriminator.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EntryReference {
    slot: u32,
}

impl EntryReference {
    #[must_use]
    pub const fn to_word(self) -> u32 {
        ENTRY_REFERENCE_TAG | (self.slot << ENTRY_REFERENCE_SLOT_SHIFT)
    }

    #[must_use]
    pub const fn from_word(word: u32) -> Option<Self> {
        if word & !ENTRY_REFERENCE_PAYLOAD_MASK == ENTRY_REFERENCE_TAG {
            Some(Self {
                slot: (word & ENTRY_REFERENCE_PAYLOAD_MASK) >> ENTRY_REFERENCE_SLOT_SHIFT,
            })
        } else {
            None
        }
    }
}

/// Checked 32-bit replacement for an object pointer returned by a solid query.
///
/// Retail stores either a 16-bit octree node or an aligned `gool_object *` in
/// the same process word. Rust uses a tag dedicated to collision results and
/// shifts the validated live-object handle above two zero alignment bits.
/// Seven payload bits cover the pool and dedicated main identity; reserved
/// payload bits and out-of-range handles are rejected during decoding.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CollisionObjectReference {
    object: ObjectHandle,
}

impl CollisionObjectReference {
    #[must_use]
    pub const fn new(object: ObjectHandle) -> Self {
        Self { object }
    }

    #[must_use]
    pub const fn object(self) -> ObjectHandle {
        self.object
    }

    #[must_use]
    pub const fn to_word(self) -> u32 {
        COLLISION_OBJECT_REFERENCE_TAG
            | ((self.object.get() as u32) << COLLISION_OBJECT_REFERENCE_SHIFT)
    }

    #[must_use]
    pub const fn from_word(word: u32) -> Option<Self> {
        if word & !COLLISION_OBJECT_REFERENCE_MASK != COLLISION_OBJECT_REFERENCE_TAG {
            return None;
        }
        let Some(object) = ObjectHandle::new(
            ((word & COLLISION_OBJECT_REFERENCE_MASK) >> COLLISION_OBJECT_REFERENCE_SHIFT) as u16,
        ) else {
            return None;
        };
        Some(Self { object })
    }
}

/// Opaque 32-bit replacement for the native `uint32_t *argv` stored at
/// `fp[-1]` while an event-service routine runs.
///
/// The generation identifies one bounded, machine-owned scope and makes a word
/// stale as soon as that scope is left. Low alignment bits stay clear so the
/// token retains the native pointer/EID discriminator without exposing a host
/// address.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EventArgumentsReference {
    generation: u32,
}

impl EventArgumentsReference {
    fn checked(generation: u32) -> Result<Self, VmError> {
        if generation == 0 || generation > EVENT_ARGUMENT_REFERENCE_GENERATION_BITS {
            return Err(VmError::EventArgumentReferenceCapacityExceeded);
        }
        Ok(Self { generation })
    }

    #[must_use]
    pub const fn to_word(self) -> u32 {
        EVENT_ARGUMENT_REFERENCE_TAG
            | (self.generation << EVENT_ARGUMENT_REFERENCE_GENERATION_SHIFT)
    }

    #[must_use]
    const fn from_word(word: u32) -> Option<Self> {
        if word & !EVENT_ARGUMENT_REFERENCE_PAYLOAD_MASK != EVENT_ARGUMENT_REFERENCE_TAG {
            return None;
        }
        let generation = (word >> EVENT_ARGUMENT_REFERENCE_GENERATION_SHIFT)
            & EVENT_ARGUMENT_REFERENCE_GENERATION_BITS;
        if generation == 0 {
            return None;
        }
        Some(Self { generation })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EventArgumentsScope {
    reference: EventArgumentsReference,
    arguments: [u32; MAX_EVENT_ARGUMENTS],
    pool_slots: [Option<u8>; MAX_EVENT_ARGUMENTS],
    len: u8,
}

#[derive(Clone, Copy)]
struct EventArgumentSlices<'a> {
    arguments: Option<&'a [u32]>,
    pool_slots: Option<&'a [Option<u8>]>,
}

impl<'a> EventArgumentSlices<'a> {
    const fn new(arguments: Option<&'a [u32]>, pool_slots: Option<&'a [Option<u8>]>) -> Self {
        Self {
            arguments,
            pool_slots,
        }
    }
}

/// Pointer-free view of the three retail transform vectors.
///
/// Rotation preserves the serialized/runtime `ang` component order (Y, X,
/// Z), while translation and scale use X, Y, Z.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailTransform {
    pub translation: [i32; 3],
    pub rotation_yxz: [i32; 3],
    pub scale: [i32; 3],
}

impl Default for RetailTransform {
    fn default() -> Self {
        Self {
            translation: [0; 3],
            rotation_yxz: [0; 3],
            scale: [INITIAL_SCALE; 3],
        }
    }
}

/// Camera state consumed by GOOL transform-vector suboperations one and
/// seven. The matrix is the source `ms_cam_rot` Q12 matrix after its 5/8 Y
/// aspect adjustment and Z-axis flip; retaining it as signed halfwords keeps
/// the VM independent from renderer-owned matrices and native pointers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailTransformVectorsCamera {
    pub translation: [i32; 3],
    /// Camera angles in the retail serialized/runtime order (`y`, `x`, `z`).
    pub rotation_yxz: [i32; 3],
    pub rotation_matrix: [[i16; 3]; 3],
    pub screen_projection: u32,
}

impl RetailTransformVectorsCamera {
    /// Builds the exact `ms_cam_rot` matrix produced by `GfxUpdateMatrices`
    /// from the camera's unusual serialized/runtime Y-X-Z angle order.
    #[must_use]
    pub fn from_retail_pose(
        translation: [i32; 3],
        rotation_yxz: [i32; 3],
        screen_projection: u32,
    ) -> Self {
        let z = Angle12::new(rotation_yxz[2].wrapping_neg());
        let stored_y = Angle12::new(rotation_yxz[0].wrapping_neg());
        let stored_x = Angle12::new(rotation_yxz[1].wrapping_neg());
        let z_rotation = [
            [z.cos_q12(), z.sin_q12().wrapping_neg(), 0],
            [z.sin_q12(), z.cos_q12(), 0],
            [0, 0, 0x1000],
        ];
        let stored_y_rotation = [
            [0x1000, 0, 0],
            [0, stored_y.cos_q12(), stored_y.sin_q12().wrapping_neg()],
            [0, stored_y.sin_q12(), stored_y.cos_q12()],
        ];
        let stored_x_rotation = [
            [stored_x.cos_q12(), 0, stored_x.sin_q12()],
            [0, 0x1000, 0],
            [stored_x.sin_q12().wrapping_neg(), 0, stored_x.cos_q12()],
        ];
        let mut rotation_matrix = multiply_q12_matrices(
            multiply_q12_matrices(z_rotation, stored_y_rotation),
            stored_x_rotation,
        );
        for value in &mut rotation_matrix[1] {
            *value = ((-5 * i32::from(*value)) >> 3) as i16;
        }
        for value in &mut rotation_matrix[2] {
            *value = value.wrapping_neg();
        }
        Self {
            translation,
            rotation_yxz,
            rotation_matrix,
            screen_projection,
        }
    }

    /// Transforms one native Q24.8 world point into the camera-space integer
    /// coordinates consumed by object visibility and zone-shader checks.
    #[must_use]
    pub fn camera_space_point(self, point: [i32; 3]) -> [i32; 3] {
        camera_space_point(point, self)
    }

    /// Applies native's object-only camera substitution for ZDAT graphics
    /// flag `0x1000`. World rendering and the ordinary camera remain intact;
    /// displayed objects use fixed X/Z, a 128-frame triangular Y bob, and the
    /// authored 125-angle pitch.
    #[must_use]
    pub fn for_object_display(self, graphics_flags: u32, frame_stamp: u32) -> Self {
        if graphics_flags & 0x1000 == 0 {
            return self;
        }
        let phase = i32::try_from(frame_stamp % 128).unwrap_or_default();
        let y = 901_600 + (phase - 64).abs() * 800;
        // `from_retail_pose` negates authored camera angles while assembling
        // the ordinary camera matrix. Native's special branch instead writes
        // a positive-125 matrix directly, so feed the inverse stored angle.
        Self::from_retail_pose([0, y, 6_144_000], [-125, 0, 0], self.screen_projection)
    }
}

fn multiply_q12_matrices(left: [[i16; 3]; 3], right: [[i16; 3]; 3]) -> [[i16; 3]; 3] {
    let mut output = [[0_i16; 3]; 3];
    for (row_index, row) in output.iter_mut().enumerate() {
        for (column_index, value) in row.iter_mut().enumerate() {
            let dot = (0..3).fold(0_i64, |sum, index| {
                sum + i64::from(left[row_index][index]) * i64::from(right[index][column_index])
            });
            // `mat16` coefficients are signed halfwords. The source software
            // matrix path retains the low halfword after the Q12 shift.
            *value = (dot >> 12) as i16;
        }
    }
    output
}

fn transform_q12_point(point: [i32; 3], matrix: [[i16; 3]; 3]) -> [i32; 3] {
    [0_usize, 1, 2].map(|row| {
        let dot = i64::from(matrix[row][0]) * i64::from(point[0])
            + i64::from(matrix[row][1]) * i64::from(point[1])
            + i64::from(matrix[row][2]) * i64::from(point[2]);
        (dot >> 12) as i32
    })
}

fn camera_space_point(point: [i32; 3], camera: RetailTransformVectorsCamera) -> [i32; 3] {
    transform_q12_point(
        [
            point[0].wrapping_sub(camera.translation[0]) >> 8,
            point[1].wrapping_sub(camera.translation[1]) >> 8,
            point[2].wrapping_sub(camera.translation[2]) >> 8,
        ],
        camera.rotation_matrix,
    )
}

fn project_gte_axis(value: i32, z: i32, projection: u32) -> i32 {
    let value = value.clamp(-0x8000, 0x7fff);
    let projected = if (z as u32).wrapping_mul(2) <= projection {
        (i128::from(value) * 0x1_ffff) >> 16
    } else {
        (i128::from(value) * i128::from(projection) * 0x1_0000 / i128::from(z)) >> 16
    };
    projected.clamp(-0x400, 0x3ff) as i32
}

fn project_retail_point(point: [i32; 3], camera: RetailTransformVectorsCamera) -> [i32; 3] {
    let transformed = camera_space_point(point, camera);
    let z = transformed[2].clamp(0, 0xffff);
    let x = project_gte_axis(transformed[0], z, camera.screen_projection);
    let y = project_gte_axis(transformed[1], z, camera.screen_projection);
    [
        x.wrapping_shl(8),
        y.wrapping_neg().wrapping_shl(8),
        z.wrapping_shl(8),
    ]
}

fn transform_retail_audio_point(
    point: [i32; 3],
    prior_output: [i32; 3],
    camera: RetailTransformVectorsCamera,
) -> [i32; 3] {
    let transformed = camera_space_point(point, camera);
    [
        transformed[0].wrapping_shl(8),
        prior_output[1].wrapping_shl(8).wrapping_neg(),
        transformed[2].wrapping_shl(8),
    ]
}

/// Coordinate space carried by a retail entity's parent entry.
///
/// ZDAT (type 7) paths store quarter-scale points relative to the zone
/// rectangle. MDAT (type 17) paths store unscaled points relative to zero.
/// Keeping that distinction typed prevents the VM from retaining the C
/// engine's relocated `parent_zone` pointer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetailEntityPathSpace {
    Zone { origin: [i32; 3] },
    Model,
}

/// One owned ZDAT rectangle/octree used by synchronous GOOL solid queries.
/// Child links remain serialized byte offsets and are validated at each
/// traversal rather than becoming native pointers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetailSolidZone {
    eid: Eid,
    origin: [i32; 3],
    dimensions: [u32; 3],
    root: u16,
    max_depth: [u16; 3],
    bytes: Vec<u8>,
    graphics_flags: u32,
    water_y: i32,
}

impl RetailSolidZone {
    pub fn new(
        origin: [i32; 3],
        dimensions: [u32; 3],
        root: u16,
        max_depth: [u16; 3],
        bytes: Vec<u8>,
    ) -> Result<Self, VmError> {
        if max_depth.iter().any(|depth| *depth > 31) {
            return Err(VmError::MalformedSolidOctree { offset: 0 });
        }
        if root != 0 && root & 1 == 0 && usize::from(root) >= bytes.len() {
            return Err(VmError::MalformedSolidOctree {
                offset: usize::from(root),
            });
        }
        Ok(Self {
            eid: Eid::NONE,
            origin,
            dimensions,
            root,
            max_depth,
            bytes,
            graphics_flags: 0,
            water_y: i32::MIN,
        })
    }

    /// Assigns the stable stream identity represented by this rectangle.
    /// Authored tests may retain [`Eid::NONE`], but retail runtime hosts bind
    /// every current-zone neighbor explicitly so indices are never reused as
    /// zone identities when the camera crosses a ZDAT boundary.
    #[must_use]
    pub const fn with_eid(mut self, eid: Eid) -> Self {
        self.eid = eid;
        self
    }

    /// Adds the runtime ZDAT header fields consumed by water and zone-boundary
    /// collision rules. Keeping them beside the owned rectangle avoids ever
    /// retaining a relocated header pointer.
    #[must_use]
    pub const fn with_graphics(mut self, graphics_flags: u32, water_y: i32) -> Self {
        self.graphics_flags = graphics_flags;
        self.water_y = water_y;
        self
    }
}

/// Pointer-free zone state needed by `ZoneFindNearestObjectNode3`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetailSolidEnvironment {
    graphics_flags: u32,
    object_shader_mode: u32,
    object_shader_depth_anchor: i32,
    object_colors: [u16; COLOR_COUNT],
    player_colors: [u16; COLOR_COUNT],
    neighbors: Vec<RetailSolidZone>,
    object_zone: Option<Eid>,
    level_quirks: SolidLevelQuirks,
}

const RETAIL_SOLID_RECT_BYTES: usize = 36;
const MAX_SOLID_QUERY_STEPS: usize = 128;
const RETAIL_SIZE_MAP: [i32; 16] = [
    0, -256, -128, -64, -48, -40, -32, -26, -20, -14, -8, 8, 16, 24, 32, 64,
];
const RETAIL_SOLID_INITIAL_Y_MAX: i32 = -999_999_999;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetailSolidHit {
    None,
    Node(u16),
    Object(ObjectHandle),
}

impl RetailSolidHit {
    const fn to_word(self) -> u32 {
        match self {
            Self::None => 0,
            Self::Node(node) => node as u32,
            Self::Object(object) => CollisionObjectReference::new(object).to_word(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RetailSolidRect {
    origin: [i32; 3],
    dimensions: [i32; 3],
}

impl RetailSolidRect {
    fn from_zone(zone: &RetailSolidZone) -> Self {
        let mut origin = [0_i32; 3];
        let mut dimensions = [0_i32; 3];
        for axis in 0..3 {
            // The PSX executes both rectangle shifts as 32-bit SLL
            // instructions. Great Hall's terminal `y__IZ` deliberately uses
            // `[i32::MAX; 3] + [1; 3]` as an out-of-world sentinel, producing
            // the wrapped inclusive interval `-256..=0`. Treating that
            // authored rectangle as checked host arithmetic faults every
            // active ShadC when the 100% ending reaches `x__IZ`.
            origin[axis] = zone.origin[axis].wrapping_shl(8);
            dimensions[axis] = zone.dimensions[axis].cast_signed().wrapping_shl(8);
        }
        Self { origin, dimensions }
    }

    fn contains_unscaled_zone_point(zone: &RetailSolidZone, point: [i32; 3]) -> bool {
        let rect = Self::from_zone(zone);
        for (axis, coordinate) in point.into_iter().enumerate() {
            let end = rect.origin[axis].wrapping_add(rect.dimensions[axis]);
            if coordinate < rect.origin[axis] || coordinate > end {
                return false;
            }
        }
        true
    }
}

fn retail_solid_child(
    zone: &RetailSolidZone,
    node: u16,
    rect: &mut RetailSolidRect,
    point: &mut [i32; 3],
    level: usize,
    flags: u8,
) -> Result<u16, VmError> {
    if level > 64 {
        return Err(VmError::MalformedSolidOctree {
            offset: usize::from(node),
        });
    }
    if node == 0 || node & 1 != 0 {
        let node_type = (node & 0x000e) >> 1;
        let subtype = (node & 0x03f0) >> 4;
        let valid =
            node & 1 != 0 && (flags & 8 == 0 || node_type != 3) && node_type != 4 && subtype != 11;
        let axis = if flags & 1 != 0 {
            Some(1)
        } else if flags & 2 != 0 {
            Some(2)
        } else {
            None
        };
        if let Some(axis) = axis {
            point[axis] = if valid {
                rect.origin[axis]
                    .checked_add(rect.dimensions[axis])
                    .ok_or(VmError::ArithmeticOverflow)?
            } else {
                rect.origin[axis]
                    .checked_sub(1)
                    .ok_or(VmError::ArithmeticOverflow)?
            };
        }
        return Ok(if valid { node } else { 0 });
    }

    let offset = usize::from(node);
    if offset < RETAIL_SOLID_RECT_BYTES {
        return Err(VmError::MalformedSolidOctree { offset });
    }
    let mut selected = [0_usize; 3];
    let mut counts = [1_usize; 3];
    let level = u16::try_from(level).map_err(|_| VmError::MalformedSolidOctree { offset })?;
    for axis in 0..3 {
        if level < zone.max_depth[axis] {
            counts[axis] = 2;
            rect.dimensions[axis] /= 2;
            let split = rect.origin[axis]
                .checked_add(rect.dimensions[axis])
                .ok_or(VmError::ArithmeticOverflow)?;
            if point[axis] >= split {
                selected[axis] = 1;
                rect.origin[axis] = split;
            }
        }
    }
    let index = (selected[0] * counts[1] + selected[1]) * counts[2] + selected[2];
    let child_offset = offset
        .checked_add(
            index
                .checked_mul(2)
                .ok_or(VmError::MalformedSolidOctree { offset })?,
        )
        .ok_or(VmError::MalformedSolidOctree { offset })?;
    let child =
        zone.bytes
            .get(child_offset..child_offset + 2)
            .ok_or(VmError::MalformedSolidOctree {
                offset: child_offset,
            })?;
    retail_solid_child(
        zone,
        u16::from_le_bytes([child[0], child[1]]),
        rect,
        point,
        usize::from(level) + 1,
        flags,
    )
}

fn find_retail_solid_node(
    environment: &RetailSolidEnvironment,
    translation: [i32; 3],
    flags: u8,
    y_offset: i32,
) -> Result<(Option<u16>, [i32; 3]), VmError> {
    let mut point = translation;
    point[1] = point[1]
        .checked_add(y_offset)
        .ok_or(VmError::ArithmeticOverflow)?;
    for _ in 0..MAX_SOLID_QUERY_STEPS {
        let mut containing = None;
        for zone in &environment.neighbors {
            if RetailSolidRect::contains_unscaled_zone_point(zone, point) {
                containing = Some(zone);
                break;
            }
        }
        let Some(zone) = containing else {
            return Ok((None, point));
        };
        let mut rect = RetailSolidRect::from_zone(zone);
        let node = retail_solid_child(zone, zone.root, &mut rect, &mut point, 0, flags)?;
        if node != 0 {
            return Ok((Some(node), point));
        }
        // The C loop rescans the same ordered neighbor list after a zero leaf.
        // `retail_solid_child` has moved the local query coordinate just past
        // that leaf, so a valid tree eventually resolves another leaf or exits.
    }
    Err(VmError::MalformedSolidOctree { offset: 0 })
}

fn retail_rebound_leaf(
    node: u16,
    rect: RetailSolidRect,
    point: &mut [i32; 3],
    direction: &mut [i32; 3],
) -> u16 {
    let corner = [
        rect.origin[0].wrapping_add(if direction[0] < 0 {
            rect.dimensions[0]
        } else {
            0
        }),
        rect.origin[1].wrapping_add(if direction[1] < 0 {
            rect.dimensions[1]
        } else {
            0
        }),
        rect.origin[2].wrapping_add(if direction[2] < 0 {
            rect.dimensions[2]
        } else {
            0
        }),
    ];
    let mut distance = i32::MIN;
    let mut axis = 0_usize;
    if direction[0] != 0 {
        let candidate = corner[0]
            .wrapping_sub(point[0].wrapping_sub(direction[0]))
            .wrapping_shl(8)
            .wrapping_div(direction[0]);
        if candidate > distance {
            distance = candidate;
            axis = 0;
        }
    }
    if direction[2] != 0 {
        let candidate = corner[2]
            .wrapping_sub(point[2].wrapping_sub(direction[2]))
            .wrapping_shl(8)
            .wrapping_div(direction[2]);
        if candidate > distance || direction[0].wrapping_abs() < direction[2].wrapping_abs() {
            distance = candidate;
            axis = 2;
        }
    }
    if direction[1] != 0 {
        let candidate = corner[1]
            .wrapping_sub(point[1].wrapping_sub(direction[1]))
            .wrapping_shl(8)
            .wrapping_div(direction[1]);
        if candidate > distance
            || (axis == 0 && direction[0].wrapping_abs() < direction[1].wrapping_abs())
            || (axis == 2 && direction[2].wrapping_abs() < direction[1].wrapping_abs())
        {
            axis = 1;
        }
    }
    direction[axis] = direction[axis].wrapping_neg();
    point[axis] = corner[axis];
    node
}

fn retail_rebound_child(
    zone: &RetailSolidZone,
    node: u16,
    rect: RetailSolidRect,
    point: &mut [i32; 3],
    direction: &mut [i32; 3],
    level: usize,
) -> Result<u16, VmError> {
    if level > 64 {
        return Err(VmError::MalformedSolidOctree {
            offset: usize::from(node),
        });
    }
    if node & 1 != 0 {
        return Ok(retail_rebound_leaf(node, rect, point, direction));
    }
    if node == 0 {
        let end = [
            rect.origin[0].wrapping_add(rect.dimensions[0]),
            rect.origin[1].wrapping_add(rect.dimensions[1]),
            rect.origin[2].wrapping_add(rect.dimensions[2]),
        ];
        let corner = [
            if direction[0] > 0 {
                end[0]
            } else {
                rect.origin[0]
            },
            if direction[1] > 0 {
                end[1]
            } else {
                rect.origin[1]
            },
            if direction[2] > 0 {
                end[2]
            } else {
                rect.origin[2]
            },
        ];
        let mut distance = i32::MAX;
        for axis in 0..3 {
            if direction[axis] == 0 {
                continue;
            }
            let candidate = corner[axis]
                .wrapping_sub(point[axis])
                .wrapping_shl(8)
                .wrapping_div(direction[axis]);
            if candidate < distance {
                distance = candidate;
            }
        }
        distance = distance.wrapping_add(1 << 8);
        for axis in 0..3 {
            point[axis] = point[axis].wrapping_add(distance.wrapping_mul(direction[axis]) >> 8);
        }
        return Ok(0);
    }

    let offset = usize::from(node);
    if offset < RETAIL_SOLID_RECT_BYTES {
        return Err(VmError::MalformedSolidOctree { offset });
    }
    let level_u16 = u16::try_from(level).map_err(|_| VmError::MalformedSolidOctree { offset })?;
    let mut child_dimensions = rect.dimensions;
    let mut counts = [1_usize; 3];
    for axis in 0..3 {
        if level_u16 < zone.max_depth[axis] {
            counts[axis] = 2;
            child_dimensions[axis] /= 2;
        }
    }
    let child_count = counts[0]
        .checked_mul(counts[1])
        .and_then(|count| count.checked_mul(counts[2]))
        .ok_or(VmError::MalformedSolidOctree { offset })?;
    let child_bytes = zone
        .bytes
        .get(offset..offset.saturating_add(child_count.saturating_mul(2)))
        .ok_or(VmError::MalformedSolidOctree { offset })?;
    let mut child_index = 0_usize;
    for x in 0..counts[0] {
        for y in 0..counts[1] {
            for z in 0..counts[2] {
                let mut child_rect = RetailSolidRect {
                    origin: rect.origin,
                    dimensions: child_dimensions,
                };
                for (axis, upper) in [x, y, z].into_iter().enumerate() {
                    if upper != 0 {
                        child_rect.origin[axis] =
                            child_rect.origin[axis].wrapping_add(child_rect.dimensions[axis]);
                    }
                }
                let contains = (0..3).all(|axis| {
                    point[axis] >= child_rect.origin[axis]
                        && point[axis]
                            <= child_rect.origin[axis].wrapping_add(child_rect.dimensions[axis])
                });
                if contains {
                    let byte_offset = child_index * 2;
                    let child = u16::from_le_bytes([
                        child_bytes[byte_offset],
                        child_bytes[byte_offset + 1],
                    ]);
                    let result =
                        retail_rebound_child(zone, child, child_rect, point, direction, level + 1)?;
                    if result != 0 {
                        return Ok(result);
                    }
                }
                child_index += 1;
            }
        }
    }
    Ok(0)
}

fn retail_rebound_vector(
    environment: &RetailSolidEnvironment,
    mut point: [i32; 3],
    mut direction: [i32; 3],
) -> Result<(u16, [i32; 3], [i32; 3]), VmError> {
    if direction == [0; 3] {
        return Ok((0, point, direction));
    }
    for zone in &environment.neighbors {
        if !RetailSolidRect::contains_unscaled_zone_point(zone, point) {
            continue;
        }
        let rect = RetailSolidRect::from_zone(zone);
        let node = retail_rebound_child(zone, zone.root, rect, &mut point, &mut direction, 0)?;
        if node != 0 {
            return Ok((node, point, direction));
        }
    }
    Ok((1, point, direction))
}

fn scale_retail_colors_rgb(
    source: &[u16; COLOR_COUNT],
    percentages: [u32; 3],
) -> [u16; COLOR_COUNT] {
    let factors = percentages.map(|percentage| (percentage << 12) / 100);
    let mut scaled = *source;
    for (index, (destination, source)) in scaled[..9].iter_mut().zip(&source[..9]).enumerate() {
        let value = i64::from(*source as i16) * i64::from(factors[index % 3]);
        *destination = ((value >> 12) as i16) as u16;
    }
    for (index, (destination, source)) in scaled[9..12].iter_mut().zip(&source[9..12]).enumerate() {
        *destination = ((u32::from(*source) * factors[index]) >> 12) as u16;
    }
    scaled
}

fn scaled_retail_colors(
    source: &[u16; COLOR_COUNT],
    subtype: i32,
    level: Option<u32>,
) -> Result<[u16; COLOR_COUNT], VmError> {
    if subtype <= 39 {
        return Ok(scale_retail_colors_rgb(source, [100; 3]));
    }
    if subtype >= 64 {
        // Retail indexes a sixteen-byte percentage table without a bound for
        // these corrupt object NODE values. Reject them instead of importing
        // that undefined read into the checked VM.
        return Err(VmError::InvalidColorSubtype(subtype));
    }

    // Native starts every selector from an all-black light/color result while
    // preserving the color matrix and intensity. Level-specific selectors
    // below overwrite that baseline; 45..47 intentionally leave it intact.
    let mut scaled = scale_retail_colors_rgb(source, [0; 3]);
    match (level, subtype) {
        (Some(0x03), 40) => {
            // Cortex Power.
            scaled[..12].copy_from_slice(&[
                0,
                (-8_601_i16) as u16,
                0,
                (-3_809_i16) as u16,
                (-1_679_i16) as u16,
                2_621,
                3_563,
                4_915,
                (-286_i16) as u16,
                0,
                255,
                255,
            ]);
            scaled[12..].copy_from_slice(&[0, 255, 0, 88, 637, 90, 284, 128, 128, 255, 255, 255]);
        }
        (Some(0x07), 40) => {
            // Toxic Waste.
            scaled[..12].copy_from_slice(&[
                0,
                (-8_601_i16) as u16,
                0,
                (-3_809_i16) as u16,
                (-1_679_i16) as u16,
                2_621,
                3_563,
                4_915,
                (-286_i16) as u16,
                0,
                255,
                255,
            ]);
            scaled[12..]
                .copy_from_slice(&[192, 255, 192, 224, 400, 224, 260, 240, 240, 255, 255, 255]);
        }
        (Some(0x13), 40) => {
            // Boulder Dash retains the source light matrix and object color,
            // then replaces only the color matrix and intensity.
            scaled = scale_retail_colors_rgb(source, [100; 3]);
            scaled[12..].copy_from_slice(&[0, 944, 944, 0, 249, 255, 0, 100, 255, 0, 255, 255]);
        }
        (Some(0x1c | 0x1d), 40..=44) => {
            // Temple Ruins and Jaws of Darkness tint only the red channel.
            let red = [50, 75, 100, 125, 150]
                [usize::try_from(subtype - 40).map_err(|_| VmError::ArithmeticOverflow)?];
            scaled = scale_retail_colors_rgb(source, [red, 100, 100]);
        }
        _ if subtype >= 48 => {
            const PERCENTAGES: [u32; 16] = [
                2, 16, 30, 44, 58, 72, 86, 100, 112, 124, 136, 148, 160, 172, 184, 196,
            ];
            let percentage = PERCENTAGES
                [usize::try_from(subtype - 48).map_err(|_| VmError::ArithmeticOverflow)?];
            scaled = scale_retail_colors_rgb(source, [percentage; 3]);
        }
        _ => {}
    }
    Ok(scaled)
}

fn seek_retail_colors(current: &mut [u16; COLOR_COUNT], target: [u16; COLOR_COUNT], step: u16) {
    for (current, mut target) in current.iter_mut().zip(target) {
        if step != 0 {
            let delta = target.wrapping_sub(*current) as i16;
            let step_signed = step as i16;
            if delta > step_signed {
                target = current.wrapping_add(step);
            } else if delta < -step_signed {
                target = current.wrapping_sub(step);
            }
        }
        *current = target;
    }
}

impl RetailSolidEnvironment {
    #[must_use]
    pub fn new(
        graphics_flags: u32,
        object_colors: [u16; COLOR_COUNT],
        player_colors: [u16; COLOR_COUNT],
        neighbors: Vec<RetailSolidZone>,
    ) -> Self {
        Self {
            graphics_flags,
            object_shader_mode: 0,
            object_shader_depth_anchor: 0,
            object_colors,
            player_colors,
            neighbors,
            object_zone: None,
            level_quirks: SolidLevelQuirks::default(),
        }
    }

    /// Records which ordered neighbor is the object's current zone and the
    /// small set of retail level-dependent collision rules.
    #[must_use]
    pub const fn with_runtime_context(
        mut self,
        object_zone: Option<Eid>,
        level_quirks: SolidLevelQuirks,
    ) -> Self {
        self.object_zone = object_zone;
        self.level_quirks = level_quirks;
        self
    }

    /// Adds the current ZDAT object-shader selector used while displaying
    /// vertex animations. Zero retains the native no-shader/default branch.
    #[must_use]
    pub const fn with_object_shader(
        mut self,
        object_shader_mode: u32,
        object_shader_depth_anchor: i32,
    ) -> Self {
        self.object_shader_mode = object_shader_mode;
        self.object_shader_depth_anchor = object_shader_depth_anchor;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RetailEntityPath {
    /// Retail's level-wide entity/spawn identity. Including it prevents two
    /// distinct entities with coincident paths from being interned as one
    /// native pointer while allowing a later respawn to recover the same
    /// stable identity.
    entity_id: u16,
    space: RetailEntityPathSpace,
    points: Vec<ZoneEntityPathPoint>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PathOrientation {
    location: [i32; 3],
    status_a: u32,
    misc_c_y: i32,
    rotation_z: i32,
    target_rotation_x: i32,
    target_rotation_y: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PathOrientationInputs {
    location: [i32; 3],
    status_a: u32,
    status_b: u32,
    object_progress: i32,
    inertia_limit: i32,
    misc_c_y: i32,
    rotation_z: i32,
    target_rotation_x: i32,
    target_rotation_y: i32,
}

/// Checked external code/data binding for a state selected by GOOL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmStateProgram {
    state_index: u16,
    state: GoolState,
    code: Vec<u32>,
    external: Vec<u32>,
    code_pc: Option<usize>,
    event_pc: Option<usize>,
    transition_pc: Option<usize>,
    page_count: u32,
    resident_pages: Vec<PageIndex>,
    entry_pages: Vec<(Eid, PageIndex)>,
}

impl VmStateProgram {
    pub fn new(
        state_index: u16,
        state: GoolState,
        code: Vec<u32>,
        external: Vec<u32>,
    ) -> Result<Self, VmError> {
        if code.len() > MAX_CODE_WORDS {
            return Err(VmError::CodeTooLarge);
        }
        if external.len() > TABLE_WORD_COUNT {
            return Err(VmError::ExternalTableTooLarge(external.len()));
        }
        let code_pc = validate_state_pc(state_index, state.code_pc, code.len())?;
        let event_pc = validate_state_pc(state_index, state.event_pc, code.len())?;
        let transition_pc = validate_state_pc(state_index, state.transition_pc, code.len())?;
        Ok(Self {
            state_index,
            state,
            code,
            external,
            code_pc,
            event_pc,
            transition_pc,
            page_count: 0,
            resident_pages: Vec::new(),
            entry_pages: Vec::new(),
        })
    }

    /// Attaches validated stream paging metadata to a rebound state's code
    /// and external table. Authored unit programs may omit it; the NSF host
    /// supplies it so EIDs first referenced by later states remain resolvable.
    #[must_use]
    pub fn with_paging_metadata(
        mut self,
        page_count: u32,
        resident_pages: impl IntoIterator<Item = PageIndex>,
        entry_pages: impl IntoIterator<Item = (Eid, PageIndex)>,
    ) -> Self {
        self.page_count = page_count;
        self.resident_pages = resident_pages.into_iter().collect();
        self.entry_pages = entry_pages.into_iter().collect();
        self
    }

    #[must_use]
    pub const fn state_index(&self) -> u16 {
        self.state_index
    }
}

fn validate_state_pc(state: u16, raw: u16, code_len: usize) -> Result<Option<usize>, VmError> {
    if raw == GOOL_PC_NONE {
        return Ok(None);
    }
    let pc = usize::from(raw);
    if pc >= code_len {
        return Err(VmError::InvalidStateProgramCounter { state, pc });
    }
    Ok(Some(pc))
}

fn retail_path_coordinate(point: i16, space: RetailEntityPathSpace, axis: usize) -> i32 {
    match space {
        RetailEntityPathSpace::Zone { origin } => i32::from(point)
            .wrapping_mul(4)
            .wrapping_add(origin[axis])
            .wrapping_mul(0x100),
        RetailEntityPathSpace::Model => i32::from(point).wrapping_mul(0x100),
    }
}

fn checked_path_coordinate(
    point: i16,
    space: RetailEntityPathSpace,
    axis: usize,
) -> Result<i32, VmError> {
    let (scale, origin) = match space {
        RetailEntityPathSpace::Zone { origin } => (4_i64, i64::from(origin[axis])),
        RetailEntityPathSpace::Model => (1, 0),
    };
    let coordinate = (i64::from(point) * scale + origin) * 0x100;
    i32::try_from(coordinate).map_err(|_| VmError::ArithmeticOverflow)
}

fn checked_i32(value: i64) -> Result<i32, VmError> {
    i32::try_from(value).map_err(|_| VmError::ArithmeticOverflow)
}

fn checked_abs(value: i32) -> Result<i32, VmError> {
    value.checked_abs().ok_or(VmError::ArithmeticOverflow)
}

fn path_point_location(path: &RetailEntityPath, index: usize) -> Result<[i32; 3], VmError> {
    let point = path
        .points
        .get(index)
        .ok_or(VmError::EntityPathProgressOutOfBounds {
            progress: i32::try_from(index)
                .unwrap_or(i32::MAX)
                .saturating_mul(0x100),
            point_count: path.points.len(),
        })?;
    Ok([
        checked_path_coordinate(point.x, path.space, 0)?,
        checked_path_coordinate(point.y, path.space, 1)?,
        checked_path_coordinate(point.z, path.space, 2)?,
    ])
}

fn orient_retail_path(
    path: &RetailEntityPath,
    progress: i32,
    inputs: PathOrientationInputs,
) -> Result<PathOrientation, VmError> {
    // Native computes `&path_points[abs(progress) >> 8]` before its explicit
    // one-point early return. The title island controller reaches progress
    // 0x110 with a declared one-point path, so that source order aliases the
    // following entity's relocated parent pointer and is address-dependent
    // undefined C behavior. Preserve the authored one-point intent instead:
    // its sole point is stationary for every progress value. Multi-point
    // paths retain the exact checked index rules below.
    if path.points.len() == 1 {
        let location = path_point_location(path, 0)?;
        return Ok(PathOrientation {
            location,
            status_a: inputs.status_a,
            misc_c_y: inputs.misc_c_y,
            rotation_z: inputs.rotation_z,
            target_rotation_x: inputs.target_rotation_x,
            target_rotation_y: inputs.target_rotation_y,
        });
    }
    let absolute = progress.checked_abs().ok_or(VmError::ArithmeticOverflow)?;
    let index = usize::try_from(absolute >> 8).map_err(|_| VmError::ArithmeticOverflow)?;
    if index >= path.points.len() {
        return Err(VmError::EntityPathProgressOutOfBounds {
            progress,
            point_count: path.points.len(),
        });
    }
    let mut segment = index;
    let mut fractional = (progress as u32 & 0xff) as i32;
    if index == path.points.len() - 1 && index != 0 {
        segment -= 1;
        fractional += 0x100;
    }
    let location = path_point_location(path, segment)?;
    let mut output = PathOrientation {
        location,
        status_a: inputs.status_a,
        misc_c_y: inputs.misc_c_y,
        rotation_z: inputs.rotation_z,
        target_rotation_x: inputs.target_rotation_x,
        target_rotation_y: inputs.target_rotation_y,
    };
    let next = path_point_location(path, segment + 1)?;
    let direction = [
        next[0]
            .checked_sub(location[0])
            .ok_or(VmError::ArithmeticOverflow)?,
        next[1]
            .checked_sub(location[1])
            .ok_or(VmError::ArithmeticOverflow)?,
        next[2]
            .checked_sub(location[2])
            .ok_or(VmError::ArithmeticOverflow)?,
    ];

    if inputs.status_b & STATUS_B_ORIENT_ON_PATH != 0 {
        let dx = i64::from(direction[0] >> 8);
        let dz = i64::from(direction[2] >> 8);
        let squared = dx
            .checked_mul(dx)
            .and_then(|value| value.checked_add(dz.checked_mul(dz)?))
            .ok_or(VmError::ArithmeticOverflow)?;
        let distance = i64::from(retail_sqrt(squared)?);
        // The source helper returns CODE_ERROR here and its caller ignores
        // that result, retaining the already-computed segment origin.
        if distance == 0 {
            return Ok(output);
        }
        let object_distance = [
            inputs.location[0]
                .checked_sub(location[0])
                .ok_or(VmError::ArithmeticOverflow)?,
            inputs.location[2]
                .checked_sub(location[2])
                .ok_or(VmError::ArithmeticOverflow)?,
        ];
        let dot = i64::from(object_distance[0] >> 4) * i64::from(direction[0] >> 4)
            + i64::from(object_distance[1] >> 4) * i64::from(direction[2] >> 4);
        let projection = checked_i32(dot / (distance * distance))?;
        if projection >= 0x100 && index < path.points.len() - 1 {
            let next_progress = i32::try_from(index + 1)
                .ok()
                .and_then(|value| value.checked_mul(0x100))
                .ok_or(VmError::ArithmeticOverflow)?;
            return orient_retail_path(path, next_progress, inputs);
        }
        let projection_part = dot / distance;
        let projected_x =
            checked_i32(((projection_part >> 4) * i64::from(direction[0] >> 4)) / distance)?;
        let projected_z =
            checked_i32(((projection_part >> 4) * i64::from(direction[2] >> 4)) / distance)?;
        let x = checked_abs(
            projected_x
                .checked_sub(object_distance[0])
                .ok_or(VmError::ArithmeticOverflow)?,
        )?;
        let z = checked_abs(
            projected_z
                .checked_sub(object_distance[1])
                .ok_or(VmError::ArithmeticOverflow)?,
        )?;
        output.misc_c_y = if z < x {
            x.checked_add(z / 2).ok_or(VmError::ArithmeticOverflow)?
        } else {
            z.checked_add(x / 2).ok_or(VmError::ArithmeticOverflow)?
        };
        if output.misc_c_y > inputs.inertia_limit
            || projection >= 0x100
            || (projection < 0 && index == 0)
        {
            output.status_a |= STATUS_A_INVALID_PATH;
        }
        let cross = i64::from(direction[2]) * i64::from(inputs.location[0] >> 8)
            - i64::from(direction[0]) * i64::from(inputs.location[2] >> 8)
            - i64::from(location[0] >> 8) * i64::from(next[2])
            + i64::from(location[2] >> 8) * i64::from(next[0]);
        if cross < 0 {
            output.misc_c_y = output
                .misc_c_y
                .checked_neg()
                .ok_or(VmError::ArithmeticOverflow)?;
        }
    }

    let previous_status_a = output.status_a;
    if inputs.object_progress >= 0 && index < path.points.len() - 1 {
        output.status_a &= !STATUS_A_TOWARD_GOAL;
    } else {
        output.status_a |= STATUS_A_TOWARD_GOAL;
    }
    if (previous_status_a & STATUS_A_TOWARD_GOAL) == (output.status_a & STATUS_A_TOWARD_GOAL)
        || output.status_a & STATUS_A_CHANGE_PATH_DIRECTION != 0
    {
        output.status_a &= !STATUS_A_CHANGE_PATH_DIRECTION;
    } else {
        output.status_a |= STATUS_A_CHANGE_PATH_DIRECTION;
    }

    if inputs.status_b & STATUS_B_TRACK_PATH_SIGN != 0 {
        output.target_rotation_x = if inputs.status_b & STATUS_B_TRACK_PATH_ROTATION != 0
            && output.status_a & STATUS_A_TOWARD_GOAL != 0
        {
            retail_atan2(
                direction[0]
                    .checked_neg()
                    .ok_or(VmError::ArithmeticOverflow)?,
                direction[2]
                    .checked_neg()
                    .ok_or(VmError::ArithmeticOverflow)?,
            )
        } else {
            retail_atan2(direction[0], direction[2])
        };
    }
    if inputs.status_b & STATUS_B_TRACK_PATH_PITCH != 0 {
        let x = checked_abs(direction[0])?;
        let z = checked_abs(direction[2])?;
        let horizontal = if z < x {
            x.checked_add(z / 2).ok_or(VmError::ArithmeticOverflow)?
        } else {
            z.checked_add(x / 2).ok_or(VmError::ArithmeticOverflow)?
        };
        if inputs.status_b & STATUS_B_TRACK_PATH_SIGN != 0 {
            output.rotation_z = retail_atan2(direction[0], direction[2]);
            output.target_rotation_y = retail_atan2(direction[1], horizontal)
                .checked_neg()
                .ok_or(VmError::ArithmeticOverflow)?;
        } else {
            output.rotation_z = retail_atan2(
                direction[0]
                    .checked_neg()
                    .ok_or(VmError::ArithmeticOverflow)?,
                direction[2]
                    .checked_neg()
                    .ok_or(VmError::ArithmeticOverflow)?,
            );
            output.target_rotation_y = retail_atan2(direction[1], horizontal);
        }
    }
    for axis in 0..3 {
        let delta = (i64::from(direction[axis]) * i64::from(fractional)) >> 8;
        output.location[axis] = checked_i32(i64::from(location[axis]) + delta)?;
    }
    Ok(output)
}

fn retail_atan2(y: i32, x: i32) -> i32 {
    let negative_y = y < 0;
    let negative_x = x < 0;
    let y = i64::from(y).unsigned_abs();
    let x = i64::from(x).unsigned_abs();
    if y | x == 0 {
        return 0;
    }
    let mut angle = if y >= x {
        0x400 - retail_atan_ratio(x, y)
    } else {
        retail_atan_ratio(y, x)
    };
    if negative_x {
        angle = 0x800 - angle;
    }
    if negative_y { -angle } else { angle }
}

fn retail_sqrt(value: i64) -> Result<i32, VmError> {
    let value = i32::try_from(value).map_err(|_| VmError::ArithmeticOverflow)?;
    if value == 0 {
        return Ok(0);
    }
    if value < 0 {
        return Err(VmError::ArithmeticOverflow);
    }
    let leading = (value.leading_zeros() & !1) as usize;
    let index = if leading < 24 {
        (value as u32) >> (24 - leading)
    } else {
        (value as u32) << (leading - 24)
    };
    if !(64..=255).contains(&index) {
        return Err(VmError::ArithmeticOverflow);
    }
    // Source sqrt_table[i - 64] is exactly floor(sqrt(i) * 512).
    let table = integer_sqrt(u64::from(index) << 18);
    let scaled = table
        .checked_shl(((31 - leading) / 2) as u32)
        .ok_or(VmError::ArithmeticOverflow)?;
    i32::try_from(scaled >> 12).map_err(|_| VmError::ArithmeticOverflow)
}

fn retail_atan_ratio(numerator: u64, denominator: u64) -> i32 {
    let ratio = if numerator >> 21 != 0 {
        numerator / (denominator >> 10).max(1)
    } else {
        (numerator << 10) / denominator
    }
    .min(0x3ff) as usize;
    let block = ratio / 64;
    let bit = ratio % 64;
    let prior = RETAIL_ATAN_INCREMENTS[..block]
        .iter()
        .map(|word| word.count_ones())
        .sum::<u32>();
    let mask = if bit == 63 {
        u64::MAX
    } else {
        (1_u64 << (bit + 1)) - 1
    };
    i32::try_from(prior + (RETAIL_ATAN_INCREMENTS[block] & mask).count_ones())
        .expect("retail atan table values fit i32")
}

fn encode_code_reference(address: CodeAddress) -> u32 {
    address.to_word()
}

/// Decoded instruction word.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Instruction {
    pub opcode: u8,
    pub operand_a: u16,
    pub operand_b: u16,
}

impl Instruction {
    #[must_use]
    pub const fn decode(word: u32) -> Self {
        Self {
            opcode: (word >> 24) as u8,
            operand_a: ((word >> 12) & 0x0fff) as u16,
            operand_b: (word & 0x0fff) as u16,
        }
    }

    #[must_use]
    pub const fn encode(opcode: u8, operand_a: u16, operand_b: u16) -> u32 {
        ((opcode as u32) << 24)
            | (((operand_a as u32) & 0x0fff) << 12)
            | ((operand_b as u32) & 0x0fff)
    }
}

/// Retail operand classes after removing pointer interpretation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operand {
    Internal(u16),
    External(u16),
    Immediate(i32),
    FrameRelative(i8),
    Null,
    StackDouble,
    LinkRegister { link: u8, register: u8 },
    ObjectRegister(u16),
    Stack,
}

impl Operand {
    #[must_use]
    pub const fn decode(raw: u16) -> Self {
        let raw = raw & 0x0fff;
        if raw & 0x0800 == 0 {
            if raw & 0x0400 == 0 {
                return Self::Internal(raw & 0x03ff);
            }
            return Self::External(raw & 0x03ff);
        }
        if raw & 0x0400 == 0 {
            if raw & 0x0200 == 0 {
                return Self::Immediate(sign_extend((raw & 0x01ff) as u32, 9) << 8);
            }
            if raw & 0x0100 == 0 {
                return Self::Immediate(sign_extend((raw & 0x00ff) as u32, 8) << 4);
            }
            if raw & 0x0080 == 0 {
                return Self::FrameRelative(sign_extend((raw & 0x003f) as u32, 6) as i8);
            }
            if raw == 0x0be0 {
                return Self::Null;
            }
            if raw == 0x0bf0 {
                return Self::StackDouble;
            }
            return Self::Null;
        }
        if raw & 0x0e00 == 0x0e00 {
            if raw == 0x0e1f {
                Self::Stack
            } else {
                Self::ObjectRegister(raw & 0x01ff)
            }
        } else {
            Self::LinkRegister {
                link: ((raw >> 6) & 7) as u8,
                register: (raw & 0x003f) as u8,
            }
        }
    }
}

const fn sign_extend(value: u32, bits: u32) -> i32 {
    ((value << (32 - bits)) as i32) >> (32 - bits)
}

/// Advances either native 32-bit retail random stream and reduces the result
/// with the engine's exact signed-folding algorithm.
///
/// The explicit seed lets browser-owned subsystems preserve source ordering
/// for process-global RNG-B without coupling their otherwise independent
/// audio, PBAK, and lighting state. A zero maximum returns zero without
/// advancing the stream, matching `_rand`.
pub fn retail_random(maximum: u32, seed: &mut u32) -> u32 {
    if maximum == 0 {
        return 0;
    }
    let generated = 0x41c6_4e6d_u32.wrapping_mul(*seed).wrapping_add(12_345);
    *seed = generated;
    let divided = generated / 15;
    let correction = (((u64::from(divided) * 33) >> 32) as u32).wrapping_add(divided) >> 1;
    let folded = ((correction & 0x7c00_0000) << 1).wrapping_sub(correction >> 26);
    divided
        .wrapping_sub(folded)
        .cast_signed()
        .wrapping_abs()
        .cast_unsigned()
        % maximum
}

/// Stable index into the full VM object table, including the dedicated main.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ObjectHandle(u16);

impl ObjectHandle {
    #[must_use]
    pub const fn new(index: u16) -> Option<Self> {
        if (index as usize) < MAX_OBJECTS {
            Some(Self(index))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

fn solid_effect_handle(raw: u32) -> Result<ObjectHandle, VmError> {
    let index = u16::try_from(raw).map_err(|_| VmError::UnknownObject(ObjectHandle(u16::MAX)))?;
    ObjectHandle::new(index).ok_or(VmError::UnknownObject(ObjectHandle(index)))
}

/// Native recipient selection performed by GOOL opcodes `0x87`, `0x8f`, and
/// `0x90` before synchronous event delivery begins.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SendEventTarget {
    /// Opcode `0x87`: deliver directly to one linked object.
    Direct { recipient: ObjectHandle },
    /// Opcode `0x8f`: traverse all eight retail roots in live postorder.
    AllRoots { mode: u8 },
    /// Opcode `0x90`: traverse the linked object's children, excluding the
    /// linked object itself, in live postorder.
    LinkedChildren { root: ObjectHandle, mode: u8 },
}

/// Fully decoded synchronous GOOL send-event request.
///
/// The fixed argument buffer is the safe equivalent of native
/// `GoolOpSendEvent`'s local `uint32_t argv[64]`. Only the prefix returned by
/// [`Self::arguments`] is initialized from the sender's stack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SendEventRequest {
    pub sender: ObjectHandle,
    pub target: SendEventTarget,
    pub event: u32,
    arguments: [u32; MAX_EVENT_ARGUMENTS],
    argument_pool_slots: [Option<u8>; MAX_EVENT_ARGUMENTS],
    argument_count: u8,
}

impl SendEventRequest {
    #[must_use]
    pub fn arguments(&self) -> &[u32] {
        &self.arguments[..usize::from(self.argument_count)]
    }

    #[must_use]
    pub const fn argument_count(&self) -> u8 {
        self.argument_count
    }

    /// Native physical-pool provenance captured beside each argument word.
    ///
    /// The slice has exactly the same length and ordering as
    /// [`Self::arguments`]. A `None` entry is an ordinary scalar or a pointer
    /// whose pool identity was not captured.
    #[must_use]
    pub fn argument_pool_slots(&self) -> &[Option<u8>] {
        &self.argument_pool_slots[..usize::from(self.argument_count)]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingSendEvent {
    id: u64,
    request: SendEventRequest,
    sender_incarnation: u64,
    return_link_halt: Option<HaltReason>,
    servicing: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SendEventService {
    None,
    Continue,
    Halt(HaltReason),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HostRunOptions {
    suspend_on_animation: bool,
    apply_animation_gate: bool,
    service_audio: bool,
    return_link_halt: Option<HaltReason>,
}

/// Native local-bound refresh policy attached to animation opcode `0x83` or
/// `0x84`. Asset resolution remains synchronous at the runtime host boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnimationLocalBoundRefresh {
    /// `0x83`: refresh only for solid/collidable objects that are near Crash,
    /// unless status B's source override bit is set.
    Conditional,
    /// `0x84`: refresh the selected frame without a status/range gate.
    Unconditional,
}

/// Host-visible, deterministic effect emitted by GOOL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VmEffect {
    Event {
        sender: ObjectHandle,
        recipient: Option<ObjectHandle>,
        event: u32,
    },
    /// Exact typed observation of a synchronous `0x87`/`0x8f`/`0x90`
    /// instruction. The corresponding host request must finish before the
    /// sender advances to its following instruction.
    SendEvent(SendEventRequest),
    /// Collision/event work whose native source target is synchronous GOOL.
    /// The pure solid solver retains the exact argument/reason payload here so
    /// the event-service host can consume it without reconstructing C scratch.
    Solid {
        object: ObjectHandle,
        effect: SolidEffect,
    },
    StateChanged {
        object: ObjectHandle,
        state: u16,
    },
    AudioStart {
        object: ObjectHandle,
        voice: u32,
        sound: u32,
    },
    AudioControl {
        object: ObjectHandle,
        command: u32,
        value: u32,
    },
    MidiTogglePlayback {
        object: ObjectHandle,
        value: u32,
    },
    /// Misc 12/5 resets the retail MIDI master-volume fade step. The fade
    /// itself belongs to the audio host, while GOOL retains the originating
    /// object for deterministic diagnostics and effect ordering.
    ResetMasterFadeStep {
        object: ObjectHandle,
    },
    /// Misc 12/11 invokes native `LevelResetGlobals(1)` synchronously. The
    /// runtime applies the exact scalar/encounter-registry transaction before
    /// the following GOOL instruction executes.
    ResetLevelGlobals {
        object: ObjectHandle,
    },
    Paging {
        object: ObjectHandle,
        operation: PagingHostOperation,
        /// Native `NSOpen` allocation flag: opcode case six physically pins
        /// an ordinary page, while case one uses the releasable virtual path.
        physical: bool,
        reference: u32,
        eid: Eid,
        page: PageIndex,
        was_resolved: bool,
    },
    SpawnChildren {
        parent: ObjectHandle,
        executable: u8,
        subtype: u8,
        count: u32,
        allow_reclaim: bool,
        arguments: Vec<u32>,
        argument_pool_slots: Vec<Option<u8>>,
    },
    /// Misc 7 asks the runtime's exact handle-three/handle-four preorder for
    /// an active entity whose `pid_flags` word matches this value.
    FindSpawnedObject {
        requester: ObjectHandle,
        pid_flags: u32,
    },
    /// Misc 13 asks the runtime to search native logical root four in
    /// preorder. The packed five-bit category mask and raw event are retained
    /// exactly; the VM resolves the selected origin link before yielding so a
    /// recycled or missing link can never become a host pointer.
    FindNearestObject {
        requester: ObjectHandle,
        origin: ObjectHandle,
        categories: u8,
        event: u32,
    },
    /// Misc 8/10 writes one word in the shared 304-entry spawn table. The VM
    /// applies the value before yielding; the runtime mirrors it into the
    /// arena before the following instruction executes.
    SpawnFlagsChanged {
        object: ObjectHandle,
        id: u16,
        flags: u32,
    },
    /// Transform-vectors suboperation six needs one legally mounted model
    /// frame. The VM retains all live-object transforms while the asset host
    /// resolves only this validated vertex request.
    TransformModelVertex {
        requester: ObjectHandle,
        link: ObjectHandle,
        output_vector: u8,
        model_eid: Eid,
        frame_index: u32,
        vertex_index: u32,
    },
    /// Misc 12/4 requests that the runtime assign `obj_zone` to an object.
    /// The stream-owning lifecycle supplies either a validated destination EID
    /// or the hard-restart sentinel; the VM never stores a native zone pointer.
    SetObjectZoneToTransitionTarget {
        object: ObjectHandle,
    },
    /// Misc 12/7 synchronously visits the current ZDAT header's neighbors in
    /// serialized order and terminates matching objects beneath roots zero
    /// through seven. The VM owns only the requester identity; header lookup,
    /// mutable tree traversal, TERM delivery, and cleanup belong to the host.
    TerminateCurrentZoneNeighbors {
        requester: ObjectHandle,
    },
    /// Misc primary nine (`SZON`) assigns one linked object to the first
    /// current-header neighbor containing an optional Q24.8 point. Pair-owned
    /// ZDAT lookup remains a synchronous runtime/asset-host responsibility;
    /// `None` selects the current zone directly.
    SetLinkZoneFromPoint {
        requester: ObjectHandle,
        target: ObjectHandle,
        point: Option<[i32; 3]>,
    },
    /// Misc 12/2 moves the current object beneath one of the eight native
    /// logical handles. The runtime applies the tree mutation synchronously.
    ReparentToRoot {
        object: ObjectHandle,
        root: u8,
    },
    AnimationSelected {
        object: ObjectHandle,
        reference: AnimationReference,
    },
    AnimationFrameChanged {
        object: ObjectHandle,
        frame: u32,
        scale_x: i32,
        local_bound_refresh: AnimationLocalBoundRefresh,
    },
    Transition(i32),
    SaveState(ObjectHandle),
    /// Misc 12/1 requests native `LevelRestart` at this exact instruction
    /// boundary. The pure VM emits `saved_level: None`; the stream-owning
    /// runtime resolves it synchronously from the protected save snapshot so
    /// later GOOL cannot retroactively change the restart kind.
    LoadState {
        object: ObjectHandle,
        saved_level: Option<LevelId>,
    },
}

/// Asset-only input returned for transform-vectors suboperation six.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelVertexSource {
    /// Frame origin plus signed packed vertex, in the source's `<< 10`
    /// coordinate domain.
    pub local_position: [i32; 3],
    /// TGEO header scale applied before the linked object's process scale.
    pub geometry_scale: [i32; 3],
}

/// State rebind requested synchronously by [`Machine::send_event`].
///
/// The caller must resolve and bind `state` with `arguments` before executing
/// the recipient again. This is the pointer-free boundary corresponding to
/// native `GoolObjectChangeState`, whose external entry lookup belongs to the
/// stream-owning runtime rather than the VM.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventStateChange {
    pub recipient: ObjectHandle,
    pub state: u16,
    pub event: u32,
    pub arguments: Vec<u32>,
    pub argument_pool_slots: Vec<Option<u8>>,
}

/// Complete synchronous result of one checked event delivery.
///
/// Event-service and mapped-interrupt code has already run when this value is
/// returned. `state_change` is the only remaining host action; it must be
/// rebound immediately rather than queued as an asynchronous event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventDispatchOutcome {
    pub acknowledged: bool,
    pub state_change: Option<EventStateChange>,
}

/// Pure candidate result for the root-four search requested by misc 13.
///
/// Distance/category/event-map inspection stays in the VM, where process and
/// descriptor words are bounds checked. Only tree order remains the runtime's
/// responsibility. A status interrupt is separated because it must execute
/// synchronously before the candidate can be accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NearestObjectCandidate {
    Ineligible,
    Eligible { distance: i32 },
    StatusInterrupt { distance: i32, offset: usize },
}

/// Checked source operands and values for retail opcode `0x8c`.
///
/// Both source references are retained because the native interpreter passes
/// pointers returned by GOP translation to the audio subsystem. The copied
/// values are the pointer-free inputs a host needs after the VM suspends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioVoiceCreateRequest {
    pub object: ObjectHandle,
    pub volume_source: StorageReference,
    pub volume: i32,
    pub adio_source: StorageReference,
    pub adio: Eid,
}

/// Retail voice-id source encoded in bits 12 through 17 of opcode `0x8d`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioVoiceSelector {
    /// Selector zero controls the template copied into the next voice.
    Template,
    /// Selector `0x1f` pops the voice id after operand B is translated.
    Stack { voice_id: i32 },
    /// Every other selector reads one word from the process register file.
    ProcessRegister { register: u8, voice_id: i32 },
}

impl AudioVoiceSelector {
    /// Effective `AudioControl` id used by the retail audio subsystem.
    #[must_use]
    pub const fn voice_id(self) -> i32 {
        match self {
            Self::Template => 0,
            Self::Stack { voice_id } | Self::ProcessRegister { voice_id, .. } => voice_id,
        }
    }
}

/// High control bits synthesized by the packed `0x8d` instruction.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AudioControlFlags {
    /// Raw suboperation 15 maps to the native force-key-off bit.
    pub force_off: bool,
    /// Packed flag bit zero requests key-off after a ramp or glide.
    pub stop_after_ramp: bool,
    /// Packed flag bit one enables an amplitude ramp or pitch glide.
    pub ramp_or_glide: bool,
}

/// Exact operation and flags decoded from retail opcode `0x8d`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioControlOperation {
    /// Original four-bit suboperation. Value 15 remains visible even though
    /// the C boundary maps it to operation zero plus `force_off`.
    pub suboperation: u8,
    pub flags: AudioControlFlags,
}

impl AudioControlOperation {
    #[must_use]
    pub const fn decode(instruction: u32) -> Self {
        let suboperation = ((instruction >> 20) & 0x0f) as u8;
        let packed_flags = ((instruction >> 18) & 3) as u8;
        Self {
            suboperation,
            flags: AudioControlFlags {
                force_off: suboperation == 15,
                stop_after_ramp: packed_flags & 1 != 0,
                ramp_or_glide: packed_flags & 2 != 0,
            },
        }
    }

    /// Low operation passed to `AudioControl` after suboperation 15 is
    /// converted into force-off plus the amplitude operation.
    #[must_use]
    pub const fn effective_suboperation(self) -> u8 {
        if self.suboperation == 15 {
            0
        } else {
            self.suboperation
        }
    }

    /// Exact native control word, useful to adapters that retain the original
    /// audio engine's bit-oriented operation representation.
    #[must_use]
    pub const fn native_control_word(self) -> u32 {
        (if self.flags.force_off { 0x8000_0000 } else { 0 })
            | (if self.flags.stop_after_ramp {
                0x4000_0000
            } else {
                0
            })
            | (if self.flags.ramp_or_glide {
                0x2000_0000
            } else {
                0
            })
            | self.effective_suboperation() as u32
    }
}

/// Scalar widths read by the native `generic` audio-control union.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioScalarArgument {
    Signed(i32),
    Unsigned(u32),
    SignedByte(i8),
}

/// Typed copy of the argument behind opcode `0x8d` operand B.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioControlArgument {
    Scalar(AudioScalarArgument),
    Vector([i32; 3]),
    Object(Option<ObjectHandle>),
    /// Operations 8, 9, 13 and 14 translate B but never dereference it.
    Unused,
}

impl AudioControlArgument {
    const fn compatibility_word(self) -> u32 {
        match self {
            Self::Scalar(AudioScalarArgument::Signed(value)) => value as u32,
            Self::Scalar(AudioScalarArgument::Unsigned(value)) => value,
            Self::Scalar(AudioScalarArgument::SignedByte(value)) => value as i32 as u32,
            Self::Vector(vector) => vector[0] as u32,
            Self::Object(Some(object)) => CollisionObjectReference::new(object).to_word(),
            Self::Object(None) | Self::Unused => 0,
        }
    }
}

/// Checked source and decoded values for retail opcode `0x8d`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioControlRequest {
    pub object: ObjectHandle,
    pub voice: AudioVoiceSelector,
    pub operation: AudioControlOperation,
    /// Present even for no-argument operations when B translated to a valid
    /// address, preserving its stack/constant/register translation contract.
    pub argument_source: Option<StorageReference>,
    pub argument: AudioControlArgument,
}

/// Typed synchronous audio work exposed when execution halts with
/// [`HaltReason::HostEffect`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioHostRequest {
    CreateVoice(AudioVoiceCreateRequest),
    Control(AudioControlRequest),
}

impl AudioHostRequest {
    #[must_use]
    pub const fn object(self) -> ObjectHandle {
        match self {
            Self::CreateVoice(request) => request.object,
            Self::Control(request) => request.object,
        }
    }
}

/// Host acknowledgement required before the suspended object may resume.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioHostResponse {
    VoiceCreated { voice_id: i32 },
    ControlApplied,
}

/// Exact native operation performed by GOOL opcode `0x8b`.
///
/// `Probe` is source case three's `NSClose(ref, 0)`: it observes whether the
/// reference is resolved but must not decrement the page reference count.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PagingHostOperation {
    Open,
    Close,
    Probe,
}

/// Pointer-free paging request exposed at the opcode's synchronous host
/// boundary. The logical `reference` remains useful to deterministic VM
/// traces, while `eid` and `page` let a platform pager operate without
/// decoding a machine-private tagged handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PagingHostRequest {
    pub object: ObjectHandle,
    pub operation: PagingHostOperation,
    /// Selects native `NSOpen(..., flag = 1)` for an open request. False is
    /// the virtual type-zero path; close/probe requests always carry false.
    pub physical: bool,
    pub reference: u32,
    pub eid: Eid,
    pub page: PageIndex,
    /// Resolution state immediately before this opcode. A platform allocation
    /// failure uses it to roll back the VM's optimistic logical handle.
    pub was_resolved: bool,
}

/// Platform result for a synchronous GOOL paging request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PagingHostResponse {
    Applied {
        /// Complete fixed-capacity set of PTEs re-armed by this operation. A
        /// texture `Open` may displace one ordinary page and one texture page;
        /// a queued virtual `Close` may report the requested page itself.
        invalidated: PageInvalidations,
    },
    /// A flag-zero open retained its reference in native's state-two virtual
    /// queue and will be promoted by a later `NSUpdate(-1)` frame boundary.
    Queued,
    /// Native `NSOpen` returned null because no physical/texture slot could be
    /// materialized. This is an authored branch result, not a VM fault.
    Unavailable,
}

/// Exact signed arguments passed by retail misc primary fifteen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CardHostRequest {
    pub object: ObjectHandle,
    pub operation: i32,
    pub part_index: i32,
}

/// One synchronous host boundary reached by an interpreter runner.
///
/// [`Self::Effect`] covers object/tree work already represented by
/// [`VmEffect`]. [`Self::Audio`] is separate because the native audio calls
/// must return a value or acknowledgement before the following GOOL
/// instruction can execute.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VmHostRequest {
    Effect(VmEffect),
    SendEvent(SendEventRequest),
    Audio(AudioHostRequest),
    Card(CardHostRequest),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VmError {
    TooManyObjects,
    DuplicateObject(ObjectHandle),
    UnknownObject(ObjectHandle),
    ActiveEventInvocation(ObjectHandle),
    CodeTooLarge,
    GlobalCodeTooLarge,
    InternalTableTooLarge(usize),
    ExternalTableTooLarge(usize),
    InvalidInitialStackPointer(u32),
    InvalidPadPort(usize),
    InvalidSpawnId(u16),
    InvalidRetailPoolSlot(u8),
    RetailPoolSlotOccupied {
        slot: u8,
        object: ObjectHandle,
    },
    RetailPoolSlotMismatch {
        object: ObjectHandle,
        bound: Option<u8>,
        requested: u8,
    },
    RetailPoolSlotUnavailable(u8),
    RetailFreePoolLinkMutation {
        slot: u8,
        register: usize,
    },
    InvalidEntityReference(u32),
    EntityReferenceTableFull,
    EntityPathTooLong(usize),
    EntityPathProgressOutOfBounds {
        progress: i32,
        point_count: usize,
    },
    InvalidProcessVector(u8),
    MalformedSolidOctree {
        offset: usize,
    },
    RetailSolidMotion(SolidMotionError),
    ProgramCounterOutOfBounds {
        object: ObjectHandle,
        pc: usize,
    },
    InvalidOperand(u16),
    InvalidRegister(usize),
    InvalidColor(usize),
    InvalidAnimationOffset(usize),
    InvalidAnimationReference(u32),
    InvalidCodeReference(u32),
    InvalidOnceCodeSegment(CodeSegment),
    InvalidStorageReference(u32),
    InvalidObjectReference(u32),
    InvalidEventArgumentsReference(u32),
    EventArgumentsTooLong(usize),
    EventArgumentPoolSlotsLengthMismatch {
        arguments: usize,
        pool_slots: usize,
    },
    EventArgumentReferenceCapacityExceeded,
    EventArgumentOutOfBounds {
        reference: u32,
        index: i8,
        len: u8,
    },
    EventArgumentScopeMismatch(u32),
    EventServiceBudgetExhausted(ObjectHandle),
    InterruptBudgetExhausted(ObjectHandle),
    UnexpectedEventServiceHalt {
        object: ObjectHandle,
        reason: HaltReason,
    },
    UnexpectedInterruptHalt {
        object: ObjectHandle,
        reason: HaltReason,
    },
    InvalidEntryReference(u32),
    EntryReferenceTableFull,
    MissingAudioVoiceCreateVolume,
    MissingAudioVoiceCreateAdio,
    MissingAudioControlArgument(u8),
    InvalidAudioEntryReference(u32),
    InvalidAudioObjectReference(u32),
    AudioHostRequestPending(ObjectHandle),
    MissingAudioHostRequest,
    MismatchedAudioHostResponse,
    MismatchedPagingHostResponse,
    InvalidPlatformPagingPage(PageIndex),
    InvalidPlatformPagingCapacity(u32),
    UnsupportedReferenceOperand(u16),
    AnimationDataUnbound,
    InvalidStateProgramCounter {
        state: u16,
        pc: usize,
    },
    InvalidStateDescriptor(u16),
    StateProgramMismatch {
        requested: u16,
        provided: u16,
    },
    MissingLink {
        object: ObjectHandle,
        link: u8,
    },
    StackUnderflow(ObjectHandle),
    StackOverflow(ObjectHandle),
    CallStackOverflow(ObjectHandle),
    InvalidJump {
        object: ObjectHandle,
        target: i64,
    },
    DivisionByZero,
    ArithmeticOverflow,
    InvalidShift(i32),
    SpawnCountTooLarge(u32),
    MissingEntryReferencePage(Eid),
    ConflictingEntryPage {
        eid: Eid,
        first: PageIndex,
        second: PageIndex,
    },
    PagingReferenceUnderflow(PageIndex),
    InvalidPagingOperation(u32),
    /// An object supplied a color selector outside the serialized six-bit
    /// node-subtype range. Retail reads beyond `percent_map` for this corrupt
    /// value; the checked VM rejects it explicitly.
    InvalidColorSubtype(i32),
    MissingSolidEnvironment(ObjectHandle),
    /// An object-bound environment does not contain the exact typed ZDAT
    /// identity it claims to own. A detached zone must retain its own checked
    /// rectangle/header rather than borrowing a numeric current-zone slot.
    SolidObjectZoneMissingFromBoundEnvironment {
        object: ObjectHandle,
        zone: Eid,
    },
    /// More than the retail object-pool maximum of 96 ordered AABB snapshots
    /// were registered for one frame.
    FrameBoundsCapacityExceeded,
    /// Projection/audio transforms cannot fabricate the renderer's current
    /// camera matrix. Hosts bind one checked, pointer-free frame snapshot.
    TransformVectorsCameraUnbound,
    /// Opcodes `0x88`/`0x89` are event-service returns, not ordinary state
    /// changes. Keep their packed contract visible until nested event-service
    /// invocation is implemented.
    UnsupportedEventServiceReturn {
        opcode: u8,
        condition_type: u8,
        return_type: u8,
        register: u8,
    },
    OnceBudgetExhausted(ObjectHandle),
    UnexpectedOnceHalt {
        object: ObjectHandle,
        reason: HaltReason,
    },
    TransitionBudgetExhausted(ObjectHandle),
    UnexpectedTransitionHalt {
        object: ObjectHandle,
        reason: HaltReason,
    },
    SynchronousStateChangeBudgetExhausted(ObjectHandle),
    MissingMiscOperand {
        primary: u8,
        secondary: i8,
        operand: u16,
    },
    UnsupportedMiscOperation {
        primary: u8,
        secondary: i8,
        operand: u16,
    },
    UnknownOpcode(u8),
    UnknownControl(u8),
    EffectQueueFull,
    MissingHostEffect,
}

fn validate_argument_pool_slots(
    argument_count: usize,
    pool_slots: Option<&[Option<u8>]>,
) -> Result<(), VmError> {
    let Some(pool_slots) = pool_slots else {
        return Ok(());
    };
    if pool_slots.len() != argument_count {
        return Err(VmError::EventArgumentPoolSlotsLengthMismatch {
            arguments: argument_count,
            pool_slots: pool_slots.len(),
        });
    }
    if let Some(pool_slot) = pool_slots
        .iter()
        .flatten()
        .copied()
        .find(|pool_slot| usize::from(*pool_slot) >= MAX_OBJECTS)
    {
        return Err(VmError::InvalidRetailPoolSlot(pool_slot));
    }
    Ok(())
}

/// Why an interpreter invocation stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HaltReason {
    Halted,
    /// Retail attempted to return through its initial stack frame, whose
    /// saved frame pointer is zero. Native reports `ERROR_INVALID_RETURN` so
    /// preorder traversal can reclaim the object without sending TERM.
    InvalidInitialReturn,
    /// A synchronous host effect must be applied before interpretation resumes.
    HostEffect,
    /// The synchronous host effect removed the object whose invocation emitted
    /// it. No later instruction, return-link unwind, or animation gate may
    /// dereference that stale compact handle.
    ObjectTerminated,
    StateChanged(u16),
    AnimationChanged {
        frame: u32,
        wait: u8,
    },
    AnimationWaiting {
        remaining: u8,
    },
    /// Native `GOOL_FLAG_STALL` countdown skipped transition, interpreter,
    /// colors, and physics for this object update.
    NativeStall {
        remaining: u32,
    },
    /// A state-change `once_p` block returned through its suspend link. The
    /// production runtime consumes this internal synchronous boundary before
    /// exposing the rebound state to the following frame.
    OnceCompleted,
    /// A state transition block returned through the nested frame installed
    /// by `GoolObjectChangeState`.
    TransitionCompleted,
    /// Internal synchronous boundary produced by a valid `0x88`/`0x89`
    /// event-service response or a later ordinary return after return mode 0.
    EventServiceReturned {
        state: u16,
        guard: bool,
    },
    /// The event routine returned without first executing a successful event
    /// return opcode, so delivery must use the retained event map.
    EventServiceInvalidReturn,
    /// A high-bit event-map entry returned from its shared-code interrupt.
    InterruptCompleted,
    BudgetExhausted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Execution {
    pub reason: HaltReason,
    pub steps: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CallFrame {
    return_address: CodeAddress,
    return_halted: bool,
    argument_base: usize,
    previous_frame_base: usize,
    behavior: ReturnBehavior,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReturnBehavior {
    Continue,
    SuspendOnce {
        state_stamp: u32,
    },
    SuspendTransition {
        previous_animation_wait: Option<AnimationWait>,
    },
    EventService {
        condition: bool,
        return_event: bool,
        guard: bool,
        previous_animation_wait: Option<AnimationWait>,
    },
    Interrupt {
        previous_animation_wait: Option<AnimationWait>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingOnce {
    address: CodeAddress,
    state_stamp: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AnimationWait {
    stamp: u32,
    frames: u8,
}

/// One VM object. All word arrays have explicit limits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmObject {
    handle: ObjectHandle,
    program_identity: Option<GoolProgramIdentity>,
    retail_initial_frame_return_is_invalid: bool,
    event_map: Vec<u16>,
    global_code: Vec<u32>,
    code: Vec<u32>,
    code_segment: CodeSegment,
    pc: usize,
    initial_stack_pointer: u32,
    frame_base: usize,
    internal: Vec<u32>,
    external: Vec<u32>,
    /// Native object pointers copied into GOOL data tables retain the same
    /// physical-pool identity as pointers copied into process registers.
    internal_pool_slots: Vec<Option<u8>>,
    external_pool_slots: Vec<Option<u8>>,
    registers: Vec<u32>,
    /// Physical retail-pool provenance for pointer-shaped process words.
    ///
    /// Compact VM handles are an implementation detail and may be reused in
    /// a different order from native's static object pool. Keeping provenance
    /// beside the raw word lets a copied pointer continue to name its native
    /// storage slot after the logical object is killed.
    register_pool_slots: Vec<Option<u8>>,
    colors: [u16; COLOR_COUNT],
    base_colors: [u16; COLOR_COUNT],
    entity_spawn_flags: Option<u16>,
    /// Validated entity awaiting registration in a [`Machine`]. Once the
    /// object is inserted, register 44 contains a checked [`EntityReference`]
    /// and the machine owns the corresponding path lifetime.
    pending_entity_path: Option<Arc<RetailEntityPath>>,
    solid_environment: Option<RetailSolidEnvironment>,
    local_bound: Bounds3,
    solid_zone_eid: Option<Eid>,
    is_main_player: bool,
    page_count: u32,
    resident_pages: Vec<PageIndex>,
    entry_pages: Vec<(Eid, PageIndex)>,
    animation_data: Vec<u8>,
    animation_frame: u32,
    animation_wait: Option<AnimationWait>,
    stack: Vec<u32>,
    state_argument_count: usize,
    call_stack: Vec<CallFrame>,
    pending_once: Option<PendingOnce>,
    links: [Option<ObjectHandle>; 8],
    state: u16,
    state_flags_by_index: Vec<u32>,
    state_flags: u32,
    status_c: u32,
    event_pc: Option<usize>,
    /// Authoritative checked counterpart of native `gool_process.tp`.
    /// GOOL may rewrite it to the live post-fetch PC for one-time prologues.
    transition_address: Option<CodeAddress>,
    halted: bool,
}

impl VmObject {
    pub fn new(handle: ObjectHandle, code: Vec<u32>) -> Result<Self, VmError> {
        if code.len() > MAX_CODE_WORDS {
            return Err(VmError::CodeTooLarge);
        }
        let mut registers = vec![0; REGISTER_COUNT];
        registers[0] = CollisionObjectReference::new(handle).to_word();
        Ok(Self {
            handle,
            program_identity: None,
            retail_initial_frame_return_is_invalid: false,
            event_map: Vec::new(),
            global_code: Vec::new(),
            code,
            code_segment: CodeSegment::External,
            pc: 0,
            initial_stack_pointer: SYNTHETIC_STACK_POINTER as u32,
            frame_base: SYNTHETIC_STACK_POINTER,
            internal: vec![0; TABLE_WORD_COUNT],
            external: vec![0; TABLE_WORD_COUNT],
            internal_pool_slots: vec![None; TABLE_WORD_COUNT],
            external_pool_slots: vec![None; TABLE_WORD_COUNT],
            registers,
            register_pool_slots: vec![None; REGISTER_COUNT],
            colors: [0; COLOR_COUNT],
            base_colors: [0; COLOR_COUNT],
            entity_spawn_flags: None,
            pending_entity_path: None,
            solid_environment: None,
            local_bound: Bounds3::default(),
            solid_zone_eid: None,
            is_main_player: false,
            page_count: 0,
            resident_pages: Vec::new(),
            entry_pages: Vec::new(),
            animation_data: Vec::new(),
            animation_frame: 0,
            animation_wait: None,
            stack: Vec::with_capacity(MAX_STACK_WORDS),
            state_argument_count: 0,
            call_stack: Vec::with_capacity(MAX_CALL_DEPTH),
            pending_once: None,
            links: [Some(handle), None, None, None, None, None, None, None],
            state: 0,
            state_flags_by_index: Vec::new(),
            state_flags: 0,
            status_c: 0,
            event_pc: None,
            transition_address: None,
            halted: false,
        })
    }

    /// Binds the state-specific code and data resolved from retail NSF entries.
    pub fn from_gool_program(handle: ObjectHandle, program: &GoolProgram) -> Result<Self, VmError> {
        if program.global_code().len() > MAX_CODE_WORDS {
            return Err(VmError::GlobalCodeTooLarge);
        }
        if program.internal_words().len() > TABLE_WORD_COUNT {
            return Err(VmError::InternalTableTooLarge(
                program.internal_words().len(),
            ));
        }
        if program.external_words().len() > TABLE_WORD_COUNT {
            return Err(VmError::ExternalTableTooLarge(
                program.external_words().len(),
            ));
        }
        let initial_stack_pointer = program.header().initial_stack_pointer;
        if usize::try_from(initial_stack_pointer).map_or(true, |value| {
            value
                .checked_add(INITIAL_FRAME_WORDS)
                .is_none_or(|end| end > REGISTER_COUNT)
        }) {
            return Err(VmError::InvalidInitialStackPointer(initial_stack_pointer));
        }

        let mut object = Self::new(handle, program.code().to_vec())?;
        object.program_identity = Some(GoolProgramIdentity {
            global_eid: program.global_eid(),
            object_type: program.header().object_type,
            category: program.header().category,
        });
        object.retail_initial_frame_return_is_invalid = true;
        object.event_map = program.event_map().to_vec();
        object.global_code = program.global_code().to_vec();
        object.initial_stack_pointer = initial_stack_pointer;
        object.internal[..program.internal_words().len()].copy_from_slice(program.internal_words());
        object.external[..program.external_words().len()].copy_from_slice(program.external_words());
        object.bind_animation_data(program.animation_data());
        object.page_count = program.page_count();
        object.resident_pages = program.resident_pages().to_vec();
        object.entry_pages = program.entry_pages().to_vec();
        object.state = program.state_index();
        object.state_flags_by_index = program.states().iter().map(|state| state.flags).collect();
        object.set_register(process_register::STATE_FLAGS, program.state().flags)?;
        object.set_register(process_register::STATUS_C, program.state().status_c)?;
        object.event_pc = program.event_pc();
        object.transition_address = program.transition_pc().map(|pc| CodeAddress {
            segment: CodeSegment::External,
            pc,
        });
        if let Some(pc) = program.code_pc() {
            object.pc = pc;
        } else {
            object.halted = true;
        }
        object.initialize_arguments(&[])?;
        Ok(object)
    }

    #[must_use]
    pub const fn handle(&self) -> ObjectHandle {
        self.handle
    }

    /// Parsed global-program identity paired with this VM object.
    ///
    /// Synthetic objects built with [`VmObject::new`] return `None`; objects
    /// built from a validated [`GoolProgram`] always return the global EID,
    /// object type, and retail category together.
    #[must_use]
    pub const fn program_identity(&self) -> Option<GoolProgramIdentity> {
        self.program_identity
    }

    /// Exact owned prefix of global item three before the subtype-map
    /// boundary. `0x00ff` is the null-state sentinel; high-bit values are
    /// shared-code interrupt offsets and remain unmodified here.
    #[must_use]
    pub fn event_map(&self) -> &[u16] {
        &self.event_map
    }

    #[must_use]
    pub const fn pc(&self) -> usize {
        self.pc
    }

    #[must_use]
    pub const fn code_address(&self) -> CodeAddress {
        CodeAddress {
            segment: self.code_segment,
            pc: self.pc,
        }
    }

    fn checked_code_address(&self, word: u32) -> Result<CodeAddress, VmError> {
        let address = CodeAddress::from_word(word).ok_or(VmError::InvalidCodeReference(word))?;
        let code_len = match address.segment {
            CodeSegment::External => self.code.len(),
            CodeSegment::Global => self.global_code.len(),
        };
        if address.pc >= code_len {
            return Err(VmError::InvalidCodeReference(word));
        }
        Ok(address)
    }

    #[must_use]
    pub const fn state(&self) -> u16 {
        self.state
    }

    #[must_use]
    pub const fn initial_stack_pointer(&self) -> u32 {
        self.initial_stack_pointer
    }

    #[must_use]
    pub const fn state_flags(&self) -> u32 {
        self.state_flags
    }

    #[must_use]
    pub const fn status_c(&self) -> u32 {
        self.status_c
    }

    fn state_link_blocked(&self, state: u16) -> Result<bool, VmError> {
        // Small authored programs created directly with `VmObject::new` do
        // not carry a retail descriptor table. Their zero target flags retain
        // the unconditional behavior used by deterministic VM tests. Parsed
        // retail objects always bind the full checked item-four table.
        let target_flags = if self.state_flags_by_index.is_empty() {
            0
        } else {
            *self
                .state_flags_by_index
                .get(usize::from(state))
                .ok_or(VmError::InvalidStateDescriptor(state))?
        };
        let invincibility = self.register(process_register::INVINCIBILITY_STATE)?;
        let status = if matches!(invincibility, 2..=4) {
            self.status_c | 0x1002
        } else {
            self.status_c
        };
        Ok(status & target_flags != 0)
    }

    fn event_state_blocked(&self, state: u16, event: u32) -> Result<bool, VmError> {
        let target_flags = if self.state_flags_by_index.is_empty() {
            0
        } else {
            *self
                .state_flags_by_index
                .get(usize::from(state))
                .ok_or(VmError::InvalidStateDescriptor(state))?
        };
        let mut status = self.status_c;
        if matches!(
            event,
            EVENT_CLEAR_GUARD_STATUS | SQUASH_EVENT | BOULDER_SQUASH_EVENT
        ) {
            status &= !2;
        }
        let invincibility = self.register(process_register::INVINCIBILITY_STATE)?;
        if self.is_main_player && matches!(invincibility, 2..=4) {
            status |= 0x1002;
        }
        Ok(status & target_flags != 0)
    }

    #[must_use]
    pub const fn event_pc(&self) -> Option<usize> {
        self.event_pc
    }

    #[must_use]
    pub const fn transition_pc(&self) -> Option<usize> {
        match self.transition_address {
            Some(address) => Some(address.pc),
            None => None,
        }
    }

    #[must_use]
    pub fn global_code(&self) -> &[u32] {
        &self.global_code
    }

    #[must_use]
    pub fn stack(&self) -> &[u32] {
        &self.stack
    }

    pub fn set_register(&mut self, index: usize, value: u32) -> Result<(), VmError> {
        self.set_register_with_pool_slot(index, value, None)
    }

    fn set_register_with_pool_slot(
        &mut self,
        index: usize,
        value: u32,
        pool_slot: Option<u8>,
    ) -> Result<(), VmError> {
        if let Some(slot) = pool_slot
            && usize::from(slot) >= MAX_OBJECTS
        {
            return Err(VmError::InvalidRetailPoolSlot(slot));
        }
        let pool_slot = pool_slot.filter(|_| CollisionObjectReference::from_word(value).is_some());
        *self
            .registers
            .get_mut(index)
            .ok_or(VmError::InvalidRegister(index))? = value;
        *self
            .register_pool_slots
            .get_mut(index)
            .ok_or(VmError::InvalidRegister(index))? = pool_slot;
        if index < self.links.len() {
            self.links[index] =
                CollisionObjectReference::from_word(value).map(CollisionObjectReference::object);
        }
        let stack_origin = self.initial_stack_pointer as usize;
        if let Some(stack_index) = index.checked_sub(stack_origin)
            && let Some(stack_word) = self.stack.get_mut(stack_index)
        {
            *stack_word = value;
        }
        match index {
            process_register::STATUS_C => self.status_c = value,
            process_register::STATE_FLAGS => self.state_flags = value,
            process_register::ANIMATION_FRAME => self.animation_frame = value,
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn register_pool_slot(&self, index: usize) -> Result<Option<u8>, VmError> {
        self.register_pool_slots
            .get(index)
            .copied()
            .ok_or(VmError::InvalidRegister(index))
    }

    /// Captures the native static-pool identity behind every live
    /// pointer-shaped process word that does not already carry one.
    ///
    /// Compact VM handles can be reused independently from retail's object
    /// pool. This pass runs before a pool occupant is reclaimed, while every
    /// live word can still be mapped unambiguously to its physical slot.
    /// Existing provenance always wins: the same compact tag may already be
    /// a dangling pointer to a different slot from an earlier incarnation.
    fn capture_live_retail_pool_slots(&mut self, pool_slots_by_object: &[Option<u8>; MAX_OBJECTS]) {
        for (words, pool_slots) in [
            (&self.internal, &mut self.internal_pool_slots),
            (&self.external, &mut self.external_pool_slots),
            (&self.registers, &mut self.register_pool_slots),
        ] {
            for (value, retained_pool_slot) in words.iter().copied().zip(pool_slots.iter_mut()) {
                if retained_pool_slot.is_some() {
                    continue;
                }
                let Some(reference) = CollisionObjectReference::from_word(value) else {
                    continue;
                };
                *retained_pool_slot = pool_slots_by_object
                    .get(usize::from(reference.object().get()))
                    .copied()
                    .flatten();
            }
        }
    }

    /// Restores the process words that physically remain in a reclaimed
    /// native object-pool slot before `GoolObjectInit` selectively overwrites
    /// them. Program/state metadata lives outside this byte-level storage and
    /// deliberately keeps the values parsed for the replacement object.
    fn inherit_retail_process_storage(&mut self, storage: &RetiredRetailProcessStorage) {
        for (index, initialized) in storage.initialized_registers.iter().copied().enumerate() {
            if !initialized {
                continue;
            }
            self.registers[index] = storage.registers[index];
            self.register_pool_slots[index] = storage.register_pool_slots[index];
        }
        for (index, link) in self.links.iter_mut().enumerate() {
            *link = self
                .registers
                .get(index)
                .copied()
                .and_then(CollisionObjectReference::from_word)
                .map(CollisionObjectReference::object);
        }
    }

    pub fn register(&self, index: usize) -> Result<u32, VmError> {
        self.registers
            .get(index)
            .copied()
            .ok_or(VmError::InvalidRegister(index))
    }

    /// Initializes the scalar/vector process state established by
    /// `GoolObjectInit` followed by the first `GoolObjectChangeState`.
    /// Pointer fields remain zero because links, code, entities and animation
    /// references have typed representations in this VM.
    pub fn initialize_retail_process(
        &mut self,
        subtype: u8,
        frame_stamp: u32,
    ) -> Result<(), VmError> {
        let stack_origin = usize::try_from(self.initial_stack_pointer)
            .map_err(|_| VmError::InvalidInitialStackPointer(self.initial_stack_pointer))?;
        let arguments = self
            .stack
            .get(..self.state_argument_count)
            .ok_or(VmError::InvalidInitialStackPointer(
                self.initial_stack_pointer,
            ))?
            .to_vec();
        let argument_pool_slots = self
            .register_pool_slots
            .get(
                stack_origin
                    ..stack_origin.checked_add(self.state_argument_count).ok_or(
                        VmError::InvalidInitialStackPointer(self.initial_stack_pointer),
                    )?,
            )
            .ok_or(VmError::InvalidInitialStackPointer(
                self.initial_stack_pointer,
            ))?
            .to_vec();
        // `GoolObjectInit` precedes `GoolObjectChangeState`. Clear the active
        // stack view while process fields are initialized, then reconstruct
        // the state frame so overlapping process words follow that order.
        self.stack.clear();
        self.animation_wait = None;
        // `GoolObjectInit` is an in-place selective initializer. Fields not
        // listed here retain the last word stored in this physical pool slot.
        // Parent/root transform initialization is applied by the runtime,
        // which owns the checked intrusive-tree context.
        self.pending_entity_path = None;
        for register in [
            process_register::MISC_A_X,
            process_register::MISC_A_Y,
            process_register::MISC_A_Z,
            process_register::MISC_B_Y,
            process_register::MISC_B_X,
            process_register::MISC_B_Z,
            process_register::MODE_FLAGS_A,
            process_register::MODE_FLAGS_B,
            process_register::MODE_FLAGS_C,
            process_register::STATUS_B,
            process_register::PID_FLAGS,
            process_register::STACK_POINTER,
            process_register::PROGRAM_COUNTER,
            process_register::FRAME_POINTER,
            process_register::TRANSITION_POINTER,
            process_register::EVENT_POINTER,
            process_register::ONCE_POINTER,
            process_register::ACK,
            process_register::ANIMATION_SEQUENCE,
            process_register::ANIMATION_FRAME,
            process_register::ENTITY_REFERENCE,
            process_register::PATH_PROGRESS,
            process_register::PATH_LENGTH,
            process_register::SPEED,
            process_register::INVINCIBILITY_STATE,
            process_register::FLOOR_IMPACT_STAMP,
            process_register::SIZE,
            process_register::HOTSPOT_SIZE,
        ] {
            self.set_register(register, 0)?;
        }
        self.set_register(process_register::STATUS_A, INITIAL_STATUS_A)?;
        self.set_register(process_register::STATUS_C, self.status_c)?;
        self.set_register(process_register::SUBTYPE, u32::from(subtype))?;
        self.set_register(process_register::STATE_STAMP, frame_stamp)?;
        self.set_register(process_register::STATE_FLAGS, self.state_flags)?;
        self.set_register(process_register::VOICE_ID, INITIAL_VOICE_ID as u32)?;
        self.set_register(process_register::NODE, INITIAL_NODE)?;
        self.initialize_arguments_with_pool_slots(&arguments, &argument_pool_slots)?;
        self.set_register(process_register::STATE_STAMP, frame_stamp)
    }

    fn mark_retail_state_change(&mut self) -> Result<(), VmError> {
        let status_a = self.register(process_register::STATUS_A)? | INITIAL_STATUS_A;
        self.set_register(process_register::STATUS_A, status_a)
    }

    /// Applies the descriptor-owned fields written by `GoolObjectSpawn` and
    /// positions the object at progress zero on its entity path.
    pub fn initialize_retail_entity(
        &mut self,
        entity: &ZoneEntity,
        zone_origin: [i32; 3],
    ) -> Result<(), VmError> {
        self.initialize_retail_entity_path(
            entity,
            RetailEntityPathSpace::Zone {
                origin: zone_origin,
            },
        )
    }

    /// Owns the validated entity path and its parent-entry coordinate space.
    /// No relocated ZDAT/MDAT pointers cross into the VM.
    pub fn initialize_retail_entity_path(
        &mut self,
        entity: &ZoneEntity,
        path_space: RetailEntityPathSpace,
    ) -> Result<(), VmError> {
        self.entity_spawn_flags = Some(entity.spawn_flags);
        let path_length = u32::try_from(entity.path_points.len())
            .map_err(|_| VmError::EntityPathTooLong(entity.path_points.len()))?;
        let path_length = path_length
            .checked_mul(0x100)
            .ok_or(VmError::EntityPathTooLong(entity.path_points.len()))?;
        let first = entity
            .path_points
            .first()
            .ok_or(VmError::EntityPathTooLong(0))?;
        self.pending_entity_path = Some(Arc::new(RetailEntityPath {
            entity_id: entity.id,
            space: path_space,
            points: entity.path_points.clone(),
        }));

        self.set_register(process_register::PID_FLAGS, u32::from(entity.id) << 8)?;
        self.set_register(process_register::PATH_PROGRESS, 0)?;
        self.set_register(process_register::PATH_LENGTH, path_length)?;
        self.set_register(
            process_register::MODE_FLAGS_A,
            i32::from(entity.initializer[0]).wrapping_mul(0x100) as u32,
        )?;
        self.set_register(
            process_register::MODE_FLAGS_B,
            i32::from(entity.initializer[1]).wrapping_mul(0x100) as u32,
        )?;
        self.set_register(
            process_register::MODE_FLAGS_C,
            i32::from(entity.initializer[2]).wrapping_mul(0x100) as u32,
        )?;

        let mut transform = self.retail_transform()?;
        transform.translation = [
            retail_path_coordinate(first.x, path_space, 0),
            retail_path_coordinate(first.y, path_space, 1),
            retail_path_coordinate(first.z, path_space, 2),
        ];
        if entity.spawn_flags & 1 == 0 {
            transform.rotation_yxz = entity.initializer.map(i32::from);
        }
        self.set_retail_transform(transform)
    }

    pub fn set_retail_transform(&mut self, transform: RetailTransform) -> Result<(), VmError> {
        for (register, value) in [
            (process_register::TRANSLATION_X, transform.translation[0]),
            (process_register::TRANSLATION_Y, transform.translation[1]),
            (process_register::TRANSLATION_Z, transform.translation[2]),
            (process_register::ROTATION_Y, transform.rotation_yxz[0]),
            (process_register::ROTATION_X, transform.rotation_yxz[1]),
            (process_register::ROTATION_Z, transform.rotation_yxz[2]),
            (process_register::SCALE_X, transform.scale[0]),
            (process_register::SCALE_Y, transform.scale[1]),
            (process_register::SCALE_Z, transform.scale[2]),
        ] {
            self.set_register(register, value as u32)?;
        }
        Ok(())
    }

    pub fn retail_transform(&self) -> Result<RetailTransform, VmError> {
        Ok(RetailTransform {
            translation: [
                self.register(process_register::TRANSLATION_X)? as i32,
                self.register(process_register::TRANSLATION_Y)? as i32,
                self.register(process_register::TRANSLATION_Z)? as i32,
            ],
            rotation_yxz: [
                self.register(process_register::ROTATION_Y)? as i32,
                self.register(process_register::ROTATION_X)? as i32,
                self.register(process_register::ROTATION_Z)? as i32,
            ],
            scale: [
                self.register(process_register::SCALE_X)? as i32,
                self.register(process_register::SCALE_Y)? as i32,
                self.register(process_register::SCALE_Z)? as i32,
            ],
        })
    }

    fn retail_physics_state(&self) -> Result<RetailPhysicsState, VmError> {
        Ok(RetailPhysicsState {
            translation: Vec3 {
                x: self.register(process_register::TRANSLATION_X)? as i32,
                y: self.register(process_register::TRANSLATION_Y)? as i32,
                z: self.register(process_register::TRANSLATION_Z)? as i32,
            },
            rotation: RetailAngles {
                y: self.register(process_register::ROTATION_Y)? as i32,
                x: self.register(process_register::ROTATION_X)? as i32,
                z: self.register(process_register::ROTATION_Z)? as i32,
            },
            velocity: Vec3 {
                x: self.register(process_register::MISC_A_X)? as i32,
                y: self.register(process_register::MISC_A_Y)? as i32,
                z: self.register(process_register::MISC_A_Z)? as i32,
            },
            angular_velocity_x: self.register(process_register::MISC_B_Y)? as i32,
            target_rotation: Vec2 {
                x: self.register(process_register::MISC_B_X)? as i32,
                y: self.register(process_register::MISC_B_Z)? as i32,
            },
            status_a: self.register(process_register::STATUS_A)?,
            status_b: self.register(process_register::STATUS_B)?,
            state_flags: self.register(process_register::STATE_FLAGS)?,
            speed: self.register(process_register::SPEED)? as i32,
            invincibility_state: self.register(process_register::INVINCIBILITY_STATE)?,
            floor_y: self.register(process_register::FLOOR_Y)? as i32,
            floor_impact_stamp: self.register(process_register::FLOOR_IMPACT_STAMP)?,
            floor_impact_velocity: self.register(process_register::FLOOR_IMPACT_VELOCITY)? as i32,
            event: self.register(process_register::EVENT)?,
            angular_velocity_y: self.register(process_register::ANGULAR_VELOCITY_Y)? as i32,
        })
    }

    fn set_retail_physics_state(&mut self, state: RetailPhysicsState) -> Result<(), VmError> {
        for (register, value) in [
            (process_register::TRANSLATION_X, state.translation.x),
            (process_register::TRANSLATION_Y, state.translation.y),
            (process_register::TRANSLATION_Z, state.translation.z),
            (process_register::ROTATION_Y, state.rotation.y),
            (process_register::ROTATION_X, state.rotation.x),
            (process_register::ROTATION_Z, state.rotation.z),
            (process_register::MISC_A_X, state.velocity.x),
            (process_register::MISC_A_Y, state.velocity.y),
            (process_register::MISC_A_Z, state.velocity.z),
            (process_register::MISC_B_Y, state.angular_velocity_x),
            (process_register::MISC_B_X, state.target_rotation.x),
            (process_register::MISC_B_Z, state.target_rotation.y),
            (process_register::STATUS_A, state.status_a as i32),
            (process_register::STATUS_B, state.status_b as i32),
            (process_register::STATE_FLAGS, state.state_flags as i32),
            (process_register::SPEED, state.speed),
            (
                process_register::INVINCIBILITY_STATE,
                state.invincibility_state as i32,
            ),
            (process_register::FLOOR_Y, state.floor_y),
            (
                process_register::FLOOR_IMPACT_STAMP,
                state.floor_impact_stamp as i32,
            ),
            (
                process_register::FLOOR_IMPACT_VELOCITY,
                state.floor_impact_velocity,
            ),
            (process_register::EVENT, state.event as i32),
            (
                process_register::ANGULAR_VELOCITY_Y,
                state.angular_velocity_y,
            ),
        ] {
            self.set_register(register, value as u32)?;
        }
        Ok(())
    }

    fn orient_retail_physics_on_path(
        &mut self,
        path: Option<&RetailEntityPath>,
        state: &mut RetailPhysicsState,
    ) -> Result<(), VmError> {
        let Some(path) = path else {
            // The source caller zero-initializes its out vector, ignores the
            // missing-entity error, and still copies that Y into `floor_y`.
            apply_path_orientation(state, Vec3::ZERO);
            return Ok(());
        };
        let object_progress = self.register(process_register::PATH_PROGRESS)? as i32;
        let oriented = orient_retail_path(
            path,
            0,
            PathOrientationInputs {
                location: [
                    state.translation.x,
                    state.translation.y,
                    state.translation.z,
                ],
                status_a: state.status_a,
                status_b: state.status_b,
                object_progress,
                inertia_limit: self.register(process_register::UNKNOWN_154)? as i32,
                misc_c_y: self.register(process_register::MODE_FLAGS_B)? as i32,
                rotation_z: state.rotation.z,
                target_rotation_x: state.target_rotation.x,
                target_rotation_y: state.target_rotation.y,
            },
        )?;
        state.status_a = oriented.status_a;
        state.rotation.z = oriented.rotation_z;
        state.target_rotation.x = oriented.target_rotation_x;
        state.target_rotation.y = oriented.target_rotation_y;
        self.set_register(process_register::MODE_FLAGS_B, oriented.misc_c_y as u32)?;
        apply_path_orientation(
            state,
            Vec3 {
                x: oriented.location[0],
                y: oriented.location[1],
                z: oriented.location[2],
            },
        );
        Ok(())
    }

    fn process_vector(&self, index: u8) -> Result<[i32; 3], VmError> {
        let index = usize::from(index);
        if index >= PROCESS_VECTOR_COUNT {
            return Err(VmError::InvalidProcessVector(index as u8));
        }
        let base = PROCESS_VECTOR_BASE + index * PROCESS_VECTOR_WORDS;
        Ok([
            self.register(base)? as i32,
            self.register(base + 1)? as i32,
            self.register(base + 2)? as i32,
        ])
    }

    fn set_process_vector(&mut self, index: u8, vector: [i32; 3]) -> Result<(), VmError> {
        let index = usize::from(index);
        if index >= PROCESS_VECTOR_COUNT {
            return Err(VmError::InvalidProcessVector(index as u8));
        }
        let base = PROCESS_VECTOR_BASE + index * PROCESS_VECTOR_WORDS;
        for (component, value) in vector.into_iter().enumerate() {
            self.set_register(base + component, value as u32)?;
        }
        Ok(())
    }

    fn orient_process_vector_on_path(
        &mut self,
        path: Option<&RetailEntityPath>,
        progress: i32,
        vector_index: u8,
    ) -> Result<(), VmError> {
        // GoolOpTransformVectors checks `process.entity`; a null reference
        // therefore retains the vector after B has been translated.
        let Some(path) = path else {
            return Ok(());
        };
        let location = self.process_vector(vector_index)?;
        let status_a = self.register(process_register::STATUS_A)?;
        let status_b = self.register(process_register::STATUS_B)?;
        let object_progress = self.register(process_register::PATH_PROGRESS)? as i32;
        let inertia_limit = self.register(process_register::UNKNOWN_154)? as i32;
        let misc_c_y = self.register(process_register::MODE_FLAGS_B)? as i32;
        let rotation_z = self.register(process_register::ROTATION_Z)? as i32;
        // `misc_b` is an `ang` in Y,X,Z memory order. Its final two words are
        // also `target_rot.x` and `target_rot.y` in the overlapping union.
        let target_rotation_x = self.register(process_register::MISC_B_X)? as i32;
        let target_rotation_y = self.register(process_register::MISC_B_Z)? as i32;
        let oriented = orient_retail_path(
            path,
            progress,
            PathOrientationInputs {
                location,
                status_a,
                status_b,
                object_progress,
                inertia_limit,
                misc_c_y,
                rotation_z,
                target_rotation_x,
                target_rotation_y,
            },
        )?;

        // `GoolObjectOrientOnPath` mutates these process fields while its
        // caller holds the transformed location in a stack-local `trans_new`.
        // Native copies `trans_new` back into the selected process vector
        // only after the helper returns. Preserve that order because vectors
        // one, four, and five alias rotation/target-rotation/misc-C fields.
        self.set_register(process_register::STATUS_A, oriented.status_a)?;
        self.set_register(process_register::MODE_FLAGS_B, oriented.misc_c_y as u32)?;
        self.set_register(process_register::ROTATION_Z, oriented.rotation_z as u32)?;
        self.set_register(
            process_register::MISC_B_X,
            oriented.target_rotation_x as u32,
        )?;
        self.set_register(
            process_register::MISC_B_Z,
            oriented.target_rotation_y as u32,
        )?;
        self.set_process_vector(vector_index, oriented.location)?;
        self.set_register(process_register::FLOOR_Y, oriented.location[1] as u32)
    }

    pub fn set_retail_colors(&mut self, colors: [u16; COLOR_COUNT]) {
        self.base_colors = colors;
        self.colors = colors;
    }

    /// Applies a display-time mutation to the live GOOL color words while
    /// preserving the zone-color source used by later authored scale seeks.
    pub(crate) fn set_retail_display_colors(&mut self, colors: [u16; COLOR_COUNT]) {
        self.colors = colors;
    }

    /// Owns the current ZDAT solid-query inputs without retaining relocated
    /// entry or octree pointers from the source runtime.
    pub fn bind_retail_solid_environment(&mut self, environment: RetailSolidEnvironment) {
        self.solid_zone_eid = environment.object_zone;
        self.solid_environment = Some(environment);
    }

    /// Refreshes the object-zone header/rectangle owner after native
    /// `StopAtZone` selected a new EID. The global current-zone octree owner
    /// lives on [`Machine`], so replacing colors here must not reset the
    /// source's smooth-stop memory.
    pub fn refresh_retail_object_zone_environment(&mut self, environment: RetailSolidEnvironment) {
        self.solid_zone_eid = environment.object_zone;
        self.solid_environment = Some(environment);
    }

    #[must_use]
    pub const fn retail_solid_zone_eid(&self) -> Option<Eid> {
        self.solid_zone_eid
    }

    /// Synchronizes the pointer-free VM mirror after a runtime-owned `SZON`
    /// mutation. The arena remains the generational authority; this value is
    /// consumed only by the in-flight solid solver and its final checked
    /// commit.
    pub(crate) fn set_retail_solid_zone_eid(&mut self, zone: Option<Eid>) {
        self.solid_zone_eid = zone;
    }

    /// Updates the persistent object-local AABB calculated from the current
    /// retail animation. Native solid motion consumes this after the
    /// interpreter, while the frame-bound list keeps the separate world AABB.
    pub fn set_retail_local_bound(&mut self, bound: Bounds3) {
        self.local_bound = bound;
    }

    /// Returns the persistent object-local AABB consumed by retail solid motion.
    #[must_use]
    pub const fn retail_local_bound(&self) -> Bounds3 {
        self.local_bound
    }

    /// Records the runtime identity represented by retail's global `crash`
    /// pointer without placing a native pointer in the VM process image.
    pub fn set_main_player_identity(&mut self, is_main_player: bool) {
        self.is_main_player = is_main_player;
    }

    #[must_use]
    pub const fn retail_colors(&self) -> &[u16; COLOR_COUNT] {
        &self.colors
    }

    fn scale_colors_for_entity_node(&mut self, level: Option<u32>) -> Result<(), VmError> {
        let Some(spawn_flags) = self.entity_spawn_flags else {
            // `ZoneColorsScaleSeekByEntityNode` returns immediately for
            // runtime children and every other object without a ZDAT entity.
            return Ok(());
        };
        let node = (spawn_flags >> 3) as i16;
        if node == -1 {
            return Ok(());
        }
        let subtype = if self.is_main_player && self.state_flags & 0x20 != 0 {
            // `ZoneColorsScaleSeek` forces subtype 0x37 (100 percent) for
            // Crash while this state flag is armed.
            0x37
        } else {
            (((node as u16) & 0x03f0) >> 4) as u8
        };
        self.colors = scaled_retail_colors(&self.base_colors, i32::from(subtype), level)?;
        Ok(())
    }

    /// Installs arguments exactly where retail frame-relative operands expect
    /// them: immediately below the initial frame pointer. The runtime calls
    /// this once after binding a newly spawned object and before interpreting
    /// its state code.
    pub fn initialize_arguments(&mut self, arguments: &[u32]) -> Result<(), VmError> {
        self.initialize_state_frame(arguments, None, true)
    }

    /// Installs creation/state arguments together with the native static-pool
    /// identity captured for pointer-shaped words.
    pub(crate) fn initialize_arguments_with_pool_slots(
        &mut self,
        arguments: &[u32],
        pool_slots: &[Option<u8>],
    ) -> Result<(), VmError> {
        self.initialize_state_frame(arguments, Some(pool_slots), true)
    }

    fn initialize_state_frame(
        &mut self,
        arguments: &[u32],
        argument_pool_slots: Option<&[Option<u8>]>,
        push_initial_wait: bool,
    ) -> Result<(), VmError> {
        validate_argument_pool_slots(arguments.len(), argument_pool_slots)?;
        let stack_origin = usize::try_from(self.initial_stack_pointer)
            .map_err(|_| VmError::InvalidInitialStackPointer(self.initial_stack_pointer))?;
        let required = arguments
            .len()
            .checked_add(STATE_FRAME_WORDS + usize::from(push_initial_wait))
            .ok_or(VmError::StackOverflow(self.handle))?;
        if required > MAX_STACK_WORDS
            || stack_origin
                .checked_add(required)
                .is_none_or(|end| end > REGISTER_COUNT)
        {
            return Err(VmError::StackOverflow(self.handle));
        }
        self.stack.clear();
        self.state_argument_count = arguments.len();
        self.call_stack.clear();
        self.pending_once = None;
        for (index, argument) in arguments.iter().copied().enumerate() {
            let pool_slot = argument_pool_slots
                .and_then(|pool_slots| pool_slots.get(index))
                .copied()
                .flatten();
            self.push_stack_word_with_pool_slot(argument, pool_slot)?;
        }
        self.frame_base = stack_origin + arguments.len();

        // Exact initial frame produced by GoolObjectChangeState followed by
        // GoolObjectPushFrame(argc, 0xffff). Native code pointers become a
        // validated tagged code address; the packed prior fp/rsp word keeps
        // retail byte offsets and therefore has a zero initial fp halfword.
        let return_pc = encode_code_reference(self.code_address());
        let prior_rsp = u32::try_from(stack_origin * 4)
            .map_err(|_| VmError::InvalidInitialStackPointer(self.initial_stack_pointer))?;
        self.push_stack_word(INITIAL_FRAME_FLAGS)?;
        self.push_stack_word(return_pc)?;
        self.push_stack_word(prior_rsp)?;
        if push_initial_wait {
            self.push_stack_word(0)?;
            self.animation_wait = Some(AnimationWait {
                stamp: 0,
                frames: 0,
            });
        } else {
            self.animation_wait = None;
        }
        Ok(())
    }

    fn push_stack_word(&mut self, value: u32) -> Result<(), VmError> {
        self.push_stack_word_with_pool_slot(value, None)
    }

    fn push_stack_word_with_pool_slot(
        &mut self,
        value: u32,
        pool_slot: Option<u8>,
    ) -> Result<(), VmError> {
        if let Some(slot) = pool_slot
            && usize::from(slot) >= MAX_OBJECTS
        {
            return Err(VmError::InvalidRetailPoolSlot(slot));
        }
        let pool_slot = pool_slot.filter(|_| CollisionObjectReference::from_word(value).is_some());
        if self.stack.len() == MAX_STACK_WORDS {
            return Err(VmError::StackOverflow(self.handle));
        }
        let index = (self.initial_stack_pointer as usize)
            .checked_add(self.stack.len())
            .ok_or(VmError::StackOverflow(self.handle))?;
        *self
            .registers
            .get_mut(index)
            .ok_or(VmError::StackOverflow(self.handle))? = value;
        *self
            .register_pool_slots
            .get_mut(index)
            .ok_or(VmError::StackOverflow(self.handle))? = pool_slot;
        self.stack.push(value);
        Ok(())
    }

    pub fn color(&self, index: usize) -> Result<u16, VmError> {
        self.colors
            .get(index)
            .copied()
            .ok_or(VmError::InvalidColor(index))
    }

    pub fn set_color(&mut self, index: usize, value: u16) -> Result<(), VmError> {
        *self
            .colors
            .get_mut(index)
            .ok_or(VmError::InvalidColor(index))? = value;
        Ok(())
    }

    /// Binds the global GOOL animation item used by opcode `0x27`.
    pub fn bind_animation_data(&mut self, bytes: &[u8]) {
        self.animation_data.clear();
        self.animation_data.extend_from_slice(bytes);
    }

    /// Resolves a tagged animation reference back into bounded local data.
    pub fn animation_data(&self, reference: AnimationReference) -> Result<&[u8], VmError> {
        let offset = reference.offset as usize;
        if offset >= self.animation_data.len() {
            return Err(VmError::InvalidAnimationReference(reference.to_word()));
        }
        Ok(&self.animation_data[offset..])
    }

    /// Current checked replacement for retail's `gool_anim *`.
    ///
    /// Opcode `0x27` and animation opcodes select a descriptor in global item
    /// five. Opcode `0x14` may instead install a process-word address. This
    /// object-local view resolves the same-object internal/register subset;
    /// [`Machine::animation_source`] additionally owns the rotating-constant
    /// and physical-pool lifetimes needed by cross-object aliases.
    pub fn animation_source(&self) -> Result<Option<AnimationSource>, VmError> {
        let word = self.register(process_register::ANIMATION_SEQUENCE)?;
        if word == 0 {
            return Ok(None);
        }
        if let Some(reference) = AnimationReference::from_word(word) {
            let _validated_data = self.animation_data(reference)?;
            return Ok(Some(AnimationSource::ItemFive(reference)));
        }

        let storage = StorageReference::from_word(word)
            .filter(|reference| reference.object() == Some(self.handle))
            .ok_or(VmError::InvalidAnimationReference(word))?;
        let words = self
            .animation_storage_words(storage)
            .ok_or(VmError::InvalidAnimationReference(word))?;
        parse_process_animation_reference(storage, words)
            .map(|source| Some(AnimationSource::Process(source)))
    }

    fn animation_storage_words(&self, storage: StorageReference) -> Option<&[u32]> {
        let index = usize::from(storage.index());
        match storage.region() {
            StorageRegion::Internal => self.internal.get(index..),
            StorageRegion::Register => self.registers.get(index..),
            // State changes replace the current external entry while a native
            // LEA pointer retains the prior entry's identity. The serialized
            // token has no generation for that backing, so retargeting it to
            // the new external table would be wrong. Rotating constants are
            // process-global Machine storage and therefore require the
            // machine-owned resolver rather than this object-local view.
            StorageRegion::External | StorageRegion::Constant => None,
        }
    }

    /// Current item-five animation reference, when that is the active source.
    ///
    /// Prefer [`Self::animation_source`] for display, collision, or any other
    /// pointer-presence decision. This compatibility view intentionally
    /// returns `None` for a valid LEA-created process descriptor.
    pub fn animation_reference(&self) -> Result<Option<AnimationReference>, VmError> {
        Ok(self
            .animation_source()?
            .as_ref()
            .and_then(AnimationSource::item_five_reference))
    }

    #[must_use]
    pub const fn animation_frame(&self) -> u32 {
        self.animation_frame
    }

    /// Applies the external code/data selected after a state-change yield.
    /// Object registers, links, colors, and global animation data persist.
    pub fn rebind_state_program(
        &mut self,
        program: &VmStateProgram,
        arguments: &[u32],
        frame_stamp: u32,
    ) -> Result<(), VmError> {
        self.rebind_state_program_inner(program, arguments, None, frame_stamp)
    }

    pub(crate) fn rebind_state_program_with_pool_slots(
        &mut self,
        program: &VmStateProgram,
        arguments: &[u32],
        argument_pool_slots: &[Option<u8>],
        frame_stamp: u32,
    ) -> Result<(), VmError> {
        self.rebind_state_program_inner(program, arguments, Some(argument_pool_slots), frame_stamp)
    }

    fn rebind_state_program_inner(
        &mut self,
        program: &VmStateProgram,
        arguments: &[u32],
        argument_pool_slots: Option<&[Option<u8>]>,
        frame_stamp: u32,
    ) -> Result<(), VmError> {
        let once = self.preflight_state_program_rebind(
            program,
            arguments,
            argument_pool_slots,
            frame_stamp,
        )?;

        self.code.clone_from(&program.code);
        self.external.fill(0);
        self.external[..program.external.len()].copy_from_slice(&program.external);
        // A state rebind selects a different native external entry. Its data
        // words cannot inherit pointer provenance captured for the previous
        // entry merely because this checked VM reuses one backing vector.
        self.external_pool_slots.fill(None);
        self.page_count = self.page_count.max(program.page_count);
        for page in &program.resident_pages {
            if !self.resident_pages.contains(page) {
                self.resident_pages.push(*page);
            }
        }
        for (eid, page) in &program.entry_pages {
            if let Some((_, known_page)) = self
                .entry_pages
                .iter_mut()
                .find(|(known_eid, _)| known_eid == eid)
            {
                *known_page = *page;
            } else {
                self.entry_pages.push((*eid, *page));
            }
        }
        self.set_register(process_register::STATE_FLAGS, program.state.flags)?;
        self.set_register(process_register::STATUS_C, program.state.status_c)?;
        self.event_pc = program.event_pc;
        self.transition_address = program.transition_pc.map(|pc| CodeAddress {
            segment: CodeSegment::External,
            pc,
        });
        self.code_segment = CodeSegment::External;
        self.pc = program.code_pc.unwrap_or(0);
        self.halted = program.code_pc.is_none();
        self.animation_wait = None;
        // Retail clears `once_p` only after the target external program and
        // state PCs have been rebound, but before replacing fp/sp.
        self.set_register(process_register::ONCE_POINTER, 0)?;
        self.initialize_state_frame(arguments, argument_pool_slots, once.is_none())?;
        self.mark_retail_state_change()?;
        if let Some(once) = once {
            self.pending_once = Some(once);
        } else {
            self.set_register(process_register::STATE_STAMP, frame_stamp)?;
        }
        Ok(())
    }

    fn preflight_state_program_rebind(
        &self,
        program: &VmStateProgram,
        arguments: &[u32],
        argument_pool_slots: Option<&[Option<u8>]>,
        frame_stamp: u32,
    ) -> Result<Option<PendingOnce>, VmError> {
        validate_argument_pool_slots(arguments.len(), argument_pool_slots)?;
        if self.state != program.state_index {
            return Err(VmError::StateProgramMismatch {
                requested: self.state,
                provided: program.state_index,
            });
        }
        let once_word = self.register(process_register::ONCE_POINTER)?;
        let once = if once_word == 0 {
            None
        } else {
            let address = self.checked_code_address(once_word)?;
            if address.segment != CodeSegment::Global {
                return Err(VmError::InvalidOnceCodeSegment(address.segment));
            }
            Some(PendingOnce {
                address,
                state_stamp: frame_stamp,
            })
        };
        let required = arguments
            .len()
            .checked_add(if once.is_some() {
                STATE_FRAME_WORDS + ONCE_FRAME_WORDS
            } else {
                INITIAL_FRAME_WORDS
            })
            .ok_or(VmError::StackOverflow(self.handle))?;
        let stack_origin = usize::try_from(self.initial_stack_pointer)
            .map_err(|_| VmError::InvalidInitialStackPointer(self.initial_stack_pointer))?;
        if required > MAX_STACK_WORDS
            || stack_origin
                .checked_add(required)
                .is_none_or(|end| end > REGISTER_COUNT)
        {
            return Err(VmError::StackOverflow(self.handle));
        }
        Ok(once)
    }

    pub fn set_internal(&mut self, index: usize, value: u32) -> Result<(), VmError> {
        *self
            .internal
            .get_mut(index)
            .ok_or(VmError::InvalidRegister(index))? = value;
        self.internal_pool_slots[index] = None;
        Ok(())
    }

    pub fn set_external(&mut self, index: usize, value: u32) -> Result<(), VmError> {
        *self
            .external
            .get_mut(index)
            .ok_or(VmError::InvalidRegister(index))? = value;
        self.external_pool_slots[index] = None;
        Ok(())
    }

    pub fn set_link(&mut self, index: usize, target: Option<ObjectHandle>) -> Result<(), VmError> {
        *self
            .links
            .get_mut(index)
            .ok_or(VmError::InvalidRegister(index))? = target;
        *self
            .registers
            .get_mut(index)
            .ok_or(VmError::InvalidRegister(index))? = target
            .map(CollisionObjectReference::new)
            .map_or(0, CollisionObjectReference::to_word);
        *self
            .register_pool_slots
            .get_mut(index)
            .ok_or(VmError::InvalidRegister(index))? = None;
        Ok(())
    }

    /// Installs a typed process link backed by one physical retail pool slot.
    ///
    /// This is used for native pointers whose allocation outlives its current
    /// logical occupant, notably the separately allocated `player` object.
    /// The raw compact token remains serializable while the sidecar controls
    /// free-slot reads and same-slot retargeting.
    pub(crate) fn set_retail_pool_link(
        &mut self,
        index: usize,
        target_token: ObjectHandle,
        pool_slot: u8,
    ) -> Result<(), VmError> {
        if usize::from(pool_slot) >= MAX_OBJECTS {
            return Err(VmError::InvalidRetailPoolSlot(pool_slot));
        }
        *self
            .links
            .get_mut(index)
            .ok_or(VmError::InvalidRegister(index))? = Some(target_token);
        *self
            .registers
            .get_mut(index)
            .ok_or(VmError::InvalidRegister(index))? =
            CollisionObjectReference::new(target_token).to_word();
        *self
            .register_pool_slots
            .get_mut(index)
            .ok_or(VmError::InvalidRegister(index))? = Some(pool_slot);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn configure_test_event_interrupt(
        &mut self,
        event: u32,
        global_code: Vec<u32>,
    ) -> Result<(), VmError> {
        if global_code.len() > MAX_CODE_WORDS {
            return Err(VmError::GlobalCodeTooLarge);
        }
        let event_index = (event >> 8) as usize;
        self.event_map.resize(event_index + 1, EVENT_MAP_NULL_STATE);
        self.event_map[event_index] = 0x8000;
        self.global_code = global_code;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn configure_test_event_state(&mut self, event: u32, state: u16) {
        let event_index = (event >> 8) as usize;
        self.event_map.resize(event_index + 1, EVENT_MAP_NULL_STATE);
        self.event_map[event_index] = state;
    }

    #[cfg(test)]
    pub(crate) fn configure_test_event_service(
        &mut self,
        code: Vec<u32>,
        event_pc: usize,
    ) -> Result<(), VmError> {
        if code.len() > MAX_CODE_WORDS {
            return Err(VmError::CodeTooLarge);
        }
        if event_pc >= code.len() {
            return Err(VmError::InvalidJump {
                object: self.handle,
                target: event_pc as i64,
            });
        }
        self.code = code;
        self.event_pc = Some(event_pc);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn configure_test_once(
        &mut self,
        global_code: Vec<u32>,
        once_pc: usize,
    ) -> Result<(), VmError> {
        if global_code.len() > MAX_CODE_WORDS {
            return Err(VmError::GlobalCodeTooLarge);
        }
        if once_pc >= global_code.len() {
            return Err(VmError::InvalidJump {
                object: self.handle,
                target: once_pc as i64,
            });
        }
        self.global_code = global_code;
        self.set_register(
            process_register::ONCE_POINTER,
            CodeAddress {
                segment: CodeSegment::Global,
                pc: once_pc,
            }
            .to_word(),
        )
    }

    #[cfg(test)]
    pub(crate) fn configure_test_state(&mut self, state: u16) {
        self.state = state;
    }

    #[cfg(test)]
    pub(crate) fn configure_test_program_identity(&mut self, category: u32) {
        self.configure_test_program_identity_with_type(category, 0);
    }

    #[cfg(test)]
    pub(crate) fn configure_test_program_identity_with_type(
        &mut self,
        category: u32,
        object_type: u32,
    ) {
        self.program_identity = Some(GoolProgramIdentity {
            global_eid: Eid::from_raw(0),
            object_type,
            category,
        });
    }

    #[cfg(test)]
    pub(crate) fn configure_test_retail_initial_frame_return(&mut self) {
        self.retail_initial_frame_return_is_invalid = true;
    }

    pub fn restart(&mut self, pc: usize) -> Result<(), VmError> {
        if pc >= self.code.len() {
            return Err(VmError::InvalidJump {
                object: self.handle,
                target: pc as i64,
            });
        }
        self.code_segment = CodeSegment::External;
        self.pc = pc;
        self.halted = false;
        self.animation_wait = None;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RetiredRetailProcessStorage {
    registers: Box<[u32]>,
    register_pool_slots: Box<[Option<u8>]>,
    /// Words with deterministic native contents that allocation must inherit.
    /// Initial malloc storage only has allocator links; reclaimed process
    /// storage and authored free-slot writes initialize their complete cells.
    initialized_registers: Box<[bool]>,
}

impl RetiredRetailProcessStorage {
    fn initial_free_pool_slot(next: Option<u8>) -> Self {
        let mut registers = vec![0; REGISTER_COUNT].into_boxed_slice();
        let mut register_pool_slots = vec![None; REGISTER_COUNT].into_boxed_slice();
        let mut initialized_registers = vec![false; REGISTER_COUNT].into_boxed_slice();
        registers[PROCESS_LINK_PARENT] = RETAIL_FREE_LIST_ROOT_REFERENCE;
        initialized_registers[PROCESS_LINK_PARENT] = true;
        initialized_registers[PROCESS_LINK_SIBLING] = true;
        initialized_registers[PROCESS_LINK_CHILDREN] = true;
        if let Some(next) = next {
            registers[PROCESS_LINK_SIBLING] = retail_pool_slot_reference_word(next);
            register_pool_slots[PROCESS_LINK_SIBLING] = Some(next);
        }
        Self {
            registers,
            register_pool_slots,
            initialized_registers,
        }
    }

    fn initial_dedicated_player() -> Self {
        let registers = vec![0; REGISTER_COUNT].into_boxed_slice();
        let register_pool_slots = vec![None; REGISTER_COUNT].into_boxed_slice();
        let mut initialized_registers = vec![false; REGISTER_COUNT].into_boxed_slice();
        // GoolInitAllocTable initializes exactly these three links after the
        // separate player malloc. Other malloc bytes are indeterminate and
        // therefore must not seed a later logical main object.
        for link in [
            PROCESS_LINK_PARENT,
            PROCESS_LINK_SIBLING,
            PROCESS_LINK_CHILDREN,
        ] {
            initialized_registers[link] = true;
        }
        Self {
            registers,
            register_pool_slots,
            initialized_registers,
        }
    }

    fn set_free_pool_sibling(&mut self, next: Option<u8>) {
        self.registers[PROCESS_LINK_SIBLING] = next.map_or(0, retail_pool_slot_reference_word);
        self.register_pool_slots[PROCESS_LINK_SIBLING] = next;
    }
}

const fn retail_pool_slot_reference_word(pool_slot: u8) -> u32 {
    COLLISION_OBJECT_REFERENCE_TAG | ((pool_slot as u32) << COLLISION_OBJECT_REFERENCE_SHIFT)
}

fn initial_retail_pool_registers() -> Vec<Option<RetiredRetailProcessStorage>> {
    (0..MAX_OBJECTS)
        .map(|slot| {
            if slot == OBJECT_POOL_CAPACITY {
                return Some(RetiredRetailProcessStorage::initial_dedicated_player());
            }
            let next = (slot + 1 < OBJECT_POOL_CAPACITY).then_some((slot + 1) as u8);
            Some(RetiredRetailProcessStorage::initial_free_pool_slot(next))
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum PagingCapacityAuthority {
    #[default]
    ProgramMetadata,
    PlatformHeap,
}

/// Re-entrant GOOL machine. Branch state belongs to each invocation, never a
/// process-global static as in the C interpreter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Machine {
    objects: BTreeMap<ObjectHandle, VmObject>,
    /// Interned, owned replacements for relocated retail entity pointers.
    /// GOOL copies the corresponding checked word through ordinary registers,
    /// stacks, tables, event arguments, and reclaimed pool storage; keeping
    /// the target here gives every such copy the source asset lifetime.
    entity_paths: Vec<Arc<RetailEntityPath>>,
    /// Per-slot identity epoch. A host callback may reclaim and immediately
    /// reuse the same compact VM handle, so bare map membership cannot prove
    /// that the interpreter's original object still exists.
    object_incarnations: [u64; MAX_OBJECTS],
    /// Physical native pool slot currently paired with each compact VM
    /// handle. The runtime installs this after binding a VM object and clears
    /// it on removal. Pointer-valued global writes snapshot this storage
    /// identity before either allocator can reuse its independent handle.
    retail_pool_slots_by_object: [Option<u8>; MAX_OBJECTS],
    /// Last translation retained in each native object-pool slot after its
    /// logical object is killed. Retail globals are raw pointers into a
    /// statically allocated pool; the Dark2 level shader can therefore read a
    /// killed doctor's final translation until that slot is reused. Keeping
    /// only this bounded, initialized field models that defined storage
    /// lifetime without exposing a stale Rust object or pointer.
    retired_retail_translations: [Option<[i32; 3]>; MAX_OBJECTS],
    /// Last initialized translation retained by each physical native object
    /// pool slot. Compact VM handles and arena slots have different reuse
    /// orders, so pointer-valued retail globals must resolve through this
    /// storage identity rather than assuming both handles keep matching.
    retired_retail_pool_translations: [Option<[i32; 3]>; MAX_OBJECTS],
    /// Complete initialized process words retained in each now-free native
    /// pool slot. C keeps these words in static storage after `handle.type`
    /// is cleared, so an authored dangling pointer may still read them until
    /// the physical slot is allocated again.
    retired_retail_pool_registers: Vec<Option<RetiredRetailProcessStorage>>,
    /// Head-to-tail physical slots beneath native `free_objects`.
    ///
    /// `GoolInitAllocTable` links ordinary slots `0..96` in ascending order.
    /// Allocation unlinks one slot and kill inserts it at the head. The
    /// separately allocated main/player slot never participates.
    retail_free_pool_slots: Vec<u8>,
    globals: Vec<u32>,
    /// Physical pool slot captured when a checked global write receives a
    /// live tagged object reference. Unchanged dangling words retain this
    /// metadata across compact VM reuse just as native raw pointers retain
    /// their static-pool address.
    retail_pool_slots_by_global: Vec<Option<u8>>,
    /// Monotonic write epochs distinguish a retained pointer from a later
    /// assignment that happens to encode the same compact tagged reference.
    global_write_epochs: Vec<u64>,
    effects: Vec<VmEffect>,
    /// Start of the current uninterrupted effect-producing transaction.
    /// Retail hosts checkpoint between synchronous broadcast recipients while
    /// retaining every observation in `effects` until its caller drains it.
    effect_checkpoint: usize,
    pending_send_events: Vec<PendingSendEvent>,
    next_send_event_id: u64,
    pending_audio_host_request: Option<AudioHostRequest>,
    pending_card_host_request: Option<CardHostRequest>,
    completed_card_load: Option<SaveData>,
    level_restart_requested: bool,
    checkpoint_globals_changed_since_context: bool,
    spawn_flags: [u32; SPAWN_TABLE_CAPACITY],
    level_spawn_tags: Box<[u16]>,
    random_seed: u32,
    ticks_per_frame: i32,
    draw_count: u32,
    frames_elapsed: u32,
    retail_game_state_playing: bool,
    camera_rotation_xz: i32,
    pads: [RetailPadSnapshot; RETAIL_PAD_COUNT],
    operand_constants: [u32; 2],
    operand_constant_pool_slots: [Option<u8>; 2],
    input_constant_index: usize,
    output_constant_index: usize,
    event_argument_scopes: Vec<EventArgumentsScope>,
    next_event_argument_generation: u32,
    // Function-static vectors from `GoolOpReactSolidSurfaces`. They are shared
    // across objects exactly like the source globals, but remain ordinary
    // deterministic machine state rather than hidden Rust statics.
    solid_trans3: [i32; 3],
    solid_trans4: [i32; 3],
    /// Native `being_stopped` and `prev_velocity` are process globals shared
    /// by every `TransSmoothStopAtSolid` caller, not per-object fields.
    solid_smooth_stop: SmoothStopMemory,
    /// Owned form of native BSS `cur_zone_query`. Its `once` lifetime spans
    /// objects, frames, and current-zone replacement until `LevelInitMisc`
    /// explicitly invalidates it or a strict event-bound escape rebuilds it.
    solid_query_cache: Option<SolidQuery>,
    /// Octree neighborhood owned by native global `cur_zone`. Per-object
    /// solid environments remain separate because their headers supply
    /// object-zone colors and boundary/water behavior.
    current_solid_environment: Option<RetailSolidEnvironment>,
    solid_frame_bounds: FrameBounds<ObjectHandle>,
    /// Incarnation captured beside each frame-owned AABB. Native bounds keep
    /// a raw object pointer; this map prevents a compact VM slot reused later
    /// in the traversal from inheriting that earlier generation's rectangle.
    solid_frame_bound_incarnations: Vec<u64>,
    camera_translation: [i32; 3],
    transform_vectors_camera: Option<RetailTransformVectorsCamera>,
    paging_page_capacity: u32,
    /// Whether the browser pager has replaced the nominal NSF-derived page
    /// capacity with the physical pool size obtained from retail's heap
    /// probe. Later object/state metadata may extend the catalog, but cannot
    /// change this authoritative allocator capacity.
    paging_page_capacity_authority: PagingCapacityAuthority,
    entry_pages: BTreeMap<Eid, PageIndex>,
    paging_page_references: BTreeMap<PageIndex, u32>,
    paging_baseline_pages: BTreeSet<PageIndex>,
    paging_loaded_pages: BTreeSet<PageIndex>,
    paging_resolved_pages: BTreeSet<PageIndex>,
    /// State-two flag-zero opens awaiting a platform `NSUpdate(-1)` retry.
    paging_pending_pages: BTreeSet<PageIndex>,
    /// Texture-cache pages are not members of native's twenty-two-slot
    /// ordinary/virtual page accounting pool.
    paging_uncounted_pages: BTreeSet<PageIndex>,
    paging_entry_references: Vec<(Eid, PageIndex)>,
}

impl Machine {
    #[must_use]
    pub fn new(global_words: usize) -> Self {
        Self {
            objects: BTreeMap::new(),
            entity_paths: Vec::new(),
            object_incarnations: [0; MAX_OBJECTS],
            retail_pool_slots_by_object: [None; MAX_OBJECTS],
            retired_retail_translations: [None; MAX_OBJECTS],
            retired_retail_pool_translations: [None; MAX_OBJECTS],
            retired_retail_pool_registers: initial_retail_pool_registers(),
            retail_free_pool_slots: (0..OBJECT_POOL_CAPACITY as u8).collect(),
            globals: vec![0; global_words],
            retail_pool_slots_by_global: vec![None; global_words],
            global_write_epochs: vec![0; global_words],
            effects: Vec::new(),
            effect_checkpoint: 0,
            pending_send_events: Vec::with_capacity(MAX_CALL_DEPTH),
            next_send_event_id: 1,
            pending_audio_host_request: None,
            pending_card_host_request: None,
            completed_card_load: None,
            level_restart_requested: false,
            checkpoint_globals_changed_since_context: false,
            spawn_flags: [0; SPAWN_TABLE_CAPACITY],
            level_spawn_tags: vec![0; RETAIL_LEVEL_SPAWN_CAPACITY].into_boxed_slice(),
            random_seed: 12_345,
            ticks_per_frame: 34,
            draw_count: 0,
            frames_elapsed: 0,
            retail_game_state_playing: false,
            camera_rotation_xz: 0,
            pads: [RetailPadSnapshot::default(); RETAIL_PAD_COUNT],
            operand_constants: [0; 2],
            operand_constant_pool_slots: [None; 2],
            input_constant_index: 0,
            output_constant_index: 0,
            event_argument_scopes: Vec::with_capacity(MAX_CALL_DEPTH),
            next_event_argument_generation: 1,
            solid_trans3: [0; 3],
            solid_trans4: [0; 3],
            solid_smooth_stop: SmoothStopMemory::default(),
            solid_query_cache: None,
            current_solid_environment: None,
            solid_frame_bounds: FrameBounds::new(),
            solid_frame_bound_incarnations: Vec::new(),
            camera_translation: [0; 3],
            transform_vectors_camera: None,
            paging_page_capacity: 0,
            paging_page_capacity_authority: PagingCapacityAuthority::ProgramMetadata,
            entry_pages: BTreeMap::new(),
            paging_page_references: BTreeMap::new(),
            paging_baseline_pages: BTreeSet::new(),
            paging_loaded_pages: BTreeSet::new(),
            paging_resolved_pages: BTreeSet::new(),
            paging_pending_pages: BTreeSet::new(),
            paging_uncounted_pages: BTreeSet::new(),
            paging_entry_references: Vec::new(),
        }
    }

    /// Restores the retail gameplay RNG stream used by opcode `0x10`.
    pub fn set_random_seed(&mut self, seed: u32) {
        self.random_seed = seed;
    }

    /// Current retail gameplay RNG state carried across stream mounts.
    #[must_use]
    pub const fn random_seed(&self) -> u32 {
        self.random_seed
    }

    /// Complete checked scalar GOOL-global allocation.
    ///
    /// Stream mounts retain this process-lifetime allocation while rebuilding
    /// every pointer-bearing object and pair-owned subsystem around it.
    #[must_use]
    pub fn global_words(&self) -> &[u32] {
        &self.globals
    }

    /// Replaces the complete scalar allocation during a checked stream mount.
    ///
    /// The runtime validates the allocation length against the destination
    /// pair before moving it here; keeping this crate-private prevents normal
    /// interpreter code from resizing the retail global table.
    pub(crate) fn restore_global_words(&mut self, globals: Box<[u32]>) {
        self.globals = globals.into_vec();
    }

    /// Mirrors the retail persistent spawn word table at the interpreter
    /// boundary. The runtime refreshes these words from its owning arena
    /// before GOOL runs so misc reads never dereference native globals.
    pub fn set_spawn_flags(&mut self, id: u16, flags: u32) -> Result<(), VmError> {
        *self
            .spawn_flags
            .get_mut(usize::from(id))
            .ok_or(VmError::InvalidSpawnId(id))? = flags;
        Ok(())
    }

    pub fn spawn_flags(&self, id: u16) -> Result<u32, VmError> {
        self.spawn_flags
            .get(usize::from(id))
            .copied()
            .ok_or(VmError::InvalidSpawnId(id))
    }

    #[must_use]
    pub(crate) const fn retail_spawn_flags_snapshot(&self) -> [u32; SPAWN_TABLE_CAPACITY] {
        self.spawn_flags
    }

    /// Exact process-lifetime `level_spawns` contents. A zero word terminates
    /// the live prefix and a one word is a reusable hole, matching retail.
    #[must_use]
    pub fn retail_level_spawn_tags(&self) -> &[u16] {
        &self.level_spawn_tags
    }

    pub(crate) fn restore_retail_level_spawn_tags(&mut self, tags: Box<[u16]>) {
        debug_assert_eq!(tags.len(), RETAIL_LEVEL_SPAWN_CAPACITY);
        self.level_spawn_tags = tags;
    }

    #[cfg(test)]
    pub(crate) fn set_retail_level_spawn_tag(&mut self, index: usize, tag: u16) {
        self.level_spawn_tags[index] = tag;
    }

    /// Applies source `GoolInitLevelSpawns` to the separate active table after
    /// a stream mount has cleared it. Corrupt tags whose nine-bit object id is
    /// outside the retail 304-word allocation are ignored rather than
    /// reproducing the native out-of-bounds write.
    pub(crate) fn initialize_retail_level_spawn_flags(&mut self, level: LevelId) {
        self.spawn_flags.fill(0);
        for &tag in self.level_spawn_tags.iter().take_while(|&&tag| tag != 0) {
            if tag == 1 || u32::from(tag >> 9) != level.get() {
                continue;
            }
            let id = usize::from(tag & 0x01ff);
            if let Some(flags) = self.spawn_flags.get_mut(id) {
                *flags |= 8;
            }
        }
    }

    fn free_retail_level_spawn_tag(&mut self, id: u16) {
        let Some(level) = self.globals.get(CURRENT_LEVEL_GLOBAL).map(|word| word >> 8) else {
            return;
        };
        let Ok(tag) = u16::try_from((level << 9) | u32::from(id)) else {
            return;
        };
        for index in 0..self.level_spawn_tags.len() {
            let entry = self.level_spawn_tags[index];
            if entry == 0 {
                break;
            }
            if entry == tag {
                self.level_spawn_tags[index] = u16::from(
                    self.level_spawn_tags
                        .get(index + 1)
                        .is_some_and(|next| *next != 0),
                );
                break;
            }
        }
    }

    fn allocate_retail_level_spawn_tag(&mut self, id: u16) {
        // Current-zone graphics flags are published at global word 30 by
        // `LevelUpdateMisc`; restricted zones deliberately do not record an
        // encountered-object tag.
        if self
            .globals
            .get(30)
            .is_some_and(|flags| flags & 0x2000 != 0)
        {
            return;
        }
        let Some(level) = self.globals.get(CURRENT_LEVEL_GLOBAL).map(|word| word >> 8) else {
            return;
        };
        let Ok(tag) = u16::try_from((level << 9) | u32::from(id)) else {
            return;
        };
        let mut reusable = None;
        let mut tail = None;
        for (index, &entry) in self.level_spawn_tags.iter().enumerate() {
            if entry == tag {
                return;
            }
            if entry == 1 {
                reusable = Some(index);
            }
            if entry == 0 {
                tail = Some(index);
                break;
            }
        }
        // The legal game never exhausts this allocation. Malformed input is
        // bounded here instead of reproducing the native tail write past the
        // 3,592-halfword array.
        if let Some(index) = reusable.or(tail) {
            self.level_spawn_tags[index] = tag;
        }
    }

    /// Exact browser-relevant body of `LevelResetGlobals(1)`.
    ///
    /// The write list follows source order. It deliberately does not touch
    /// objects, savestate ownership, card metadata/options, or the separate
    /// 304-word active spawn table. The PSX-only trailing callback has no
    /// browser counterpart.
    pub fn reset_retail_level_globals(&mut self) -> Result<(), VmError> {
        // Preflight the highest touched word so malformed authored machines
        // cannot observe a partially applied reset transaction.
        if self.globals.len() <= 113 {
            return Err(VmError::InvalidRegister(113));
        }
        let initial_lives = self.global_word(INITIAL_LIFE_COUNT_GLOBAL)?;
        for (index, value) in [
            (69, u32::MAX), // checkpoint_id = -1
            (108, 0),       // death_count
            (5, 0),         // respawn_count
            (25, 0),        // health
            (26, 0),        // fruit_count
            (27, 0),        // cortex_count
            (28, 0),        // brio_count
            (29, 0),        // tawna_count
            (47, 1),        // levels_unlocked
            (63, 0),        // item_pool1
            (72, 0),        // item_pool2
            (67, 1),        // is_first_zone
            (20, 99),       // cur_map_level
            (46, 1),        // level_count
            (100, 0),       // saved_item_pool1
            (101, 0),       // saved_item_pool2
            (113, 1),       // saved_level_count
            (LIFE_COUNT_GLOBAL, initial_lives),
        ] {
            self.set_global_word(index, value)?;
        }
        self.level_spawn_tags.fill(0);
        self.checkpoint_globals_changed_since_context = true;
        Ok(())
    }

    #[must_use]
    pub(crate) const fn checkpoint_globals_changed_since_context(&self) -> bool {
        self.checkpoint_globals_changed_since_context
    }

    pub(crate) fn acknowledge_level_state_context(&mut self) {
        self.checkpoint_globals_changed_since_context = false;
    }

    /// Clears the modeled solid BSS state used by `LevelInitMisc(0)`.
    ///
    /// The latch/prior displacement and `cur_zone_query.once` are shared by
    /// interleaved object updates. Current-zone replacement intentionally does
    /// not invalidate the query; native keeps it until the next strict-bound
    /// escape.
    pub(crate) fn reset_retail_solid_smoothing(&mut self) {
        self.solid_smooth_stop = SmoothStopMemory::default();
        self.solid_query_cache = None;
    }

    /// Whether the host must stop for a deferred same-level restart.
    ///
    /// Native different-level misc 12/1 only schedules `next_lid = -2` and
    /// returns to GOOL. The stream-owning runtime therefore leaves this false
    /// for that case and retains the emitted [`VmEffect::LoadState`].
    #[must_use]
    pub const fn level_restart_requested(&self) -> bool {
        self.level_restart_requested
    }

    pub fn request_level_restart(&mut self) {
        self.level_restart_requested = true;
    }

    pub fn clear_level_restart_request(&mut self) {
        self.level_restart_requested = false;
    }

    /// Reads one checked logical GOOL global word.
    pub fn global_word(&self, index: usize) -> Result<u32, VmError> {
        self.globals
            .get(index)
            .copied()
            .ok_or(VmError::InvalidRegister(index))
    }

    /// Writes one checked logical GOOL global word.
    pub fn set_global_word(&mut self, index: usize, value: u32) -> Result<(), VmError> {
        let previous = self
            .globals
            .get(index)
            .copied()
            .ok_or(VmError::InvalidRegister(index))?;
        self.global_write_epochs
            .get(index)
            .ok_or(VmError::InvalidRegister(index))?;
        self.retail_pool_slots_by_global
            .get(index)
            .ok_or(VmError::InvalidRegister(index))?;
        let referenced_pool_slot =
            CollisionObjectReference::from_word(value).and_then(|reference| {
                self.retail_pool_slots_by_object
                    .get(usize::from(reference.object().get()))
                    .copied()
                    .flatten()
            });

        self.globals[index] = value;
        let epoch = &mut self.global_write_epochs[index];
        *epoch = epoch.wrapping_add(1);
        let retained_pool_slot = &mut self.retail_pool_slots_by_global[index];
        if value == 0 {
            *retained_pool_slot = None;
        } else if let Some(pool_slot) = referenced_pool_slot {
            *retained_pool_slot = Some(pool_slot);
        } else if value != previous {
            // Copying the same dangling native pointer retains its storage
            // identity. A distinct unresolved word cannot inherit the prior
            // pool slot merely because both values are malformed or stale.
            *retained_pool_slot = None;
        }
        if matches!(index, 69 | 102 | 103 | 104) {
            self.checkpoint_globals_changed_since_context = true;
        }
        Ok(())
    }

    fn set_global_word_with_pool_slot(
        &mut self,
        index: usize,
        value: u32,
        pool_slot: Option<u8>,
    ) -> Result<(), VmError> {
        if let Some(pool_slot) = pool_slot {
            if usize::from(pool_slot) >= MAX_OBJECTS {
                return Err(VmError::InvalidRetailPoolSlot(pool_slot));
            }
            if CollisionObjectReference::from_word(value).is_none() {
                return Err(VmError::InvalidObjectReference(value));
            }
        }
        self.set_global_word(index, value)?;
        if let Some(pool_slot) = pool_slot {
            self.retail_pool_slots_by_global[index] = Some(pool_slot);
        }
        Ok(())
    }

    /// Returns the number of checked writes observed for one global word.
    pub(crate) fn global_word_write_epoch(&self, index: usize) -> Result<u64, VmError> {
        self.global_write_epochs
            .get(index)
            .copied()
            .ok_or(VmError::InvalidRegister(index))
    }

    /// Returns the physical pool-storage identity captured by the latest
    /// checked tagged-reference write to one global word.
    pub(crate) fn retail_global_pool_slot(&self, index: usize) -> Result<Option<u8>, VmError> {
        self.retail_pool_slots_by_global
            .get(index)
            .copied()
            .ok_or(VmError::InvalidRegister(index))
    }

    /// Captures the exact persistent scalar globals serialized by the retail
    /// 128-byte card payload.
    pub fn retail_card_save_data(&self) -> Result<SaveData, VmError> {
        Ok(SaveData {
            level_count: self.global_word(LEVEL_COUNT_GLOBAL)?,
            initial_lives: self.global_word(INITIAL_LIFE_COUNT_GLOBAL)?,
            unknown_6190c: self.global_word(UNKNOWN_6190C_GLOBAL)?,
            mono: self.global_word(MONO_GLOBAL)? != 0,
            sfx_volume: self.global_word(SFX_VOLUME_GLOBAL)?,
            music_volume: self.global_word(MUSIC_VOLUME_GLOBAL)?,
            item_pool_1: self.global_word(ITEM_POOL_1_GLOBAL)?,
            item_pool_2: self.global_word(ITEM_POOL_2_GLOBAL)?,
            gem_count: self.global_word(GEM_COUNT_GLOBAL)? as u8,
            key_count: self.global_word(KEY_COUNT_GLOBAL)?,
        })
    }

    /// Applies `CardRestorePayload`, including the source-ordered
    /// `init_life_count` write followed by `LevelResetGlobals(1)`. Native does
    /// not clear the distinct 304-word active spawn table here.
    pub fn restore_retail_card_save_data(&mut self, save: SaveData) -> Result<(), VmError> {
        self.restore_retail_card_payload_globals(save)
    }

    /// Reapplies a browser-resume payload around a title reset exactly like
    /// `CardBrowserResumeAfterTitleReset`: savestate and active spawn words
    /// remain owned by the runtime and untouched.
    pub(crate) fn restore_retail_resume_save_data(
        &mut self,
        save: SaveData,
    ) -> Result<(), VmError> {
        self.restore_retail_card_payload_globals(save)
    }

    fn restore_retail_card_payload_globals(&mut self, save: SaveData) -> Result<(), VmError> {
        self.set_global_word(INITIAL_LIFE_COUNT_GLOBAL, save.initial_lives)?;
        self.reset_retail_level_globals()?;
        for (index, value) in [
            (LEVEL_COUNT_GLOBAL, save.level_count),
            (UNKNOWN_6190C_GLOBAL, save.unknown_6190c),
            (MONO_GLOBAL, u32::from(save.mono)),
            (SFX_VOLUME_GLOBAL, save.sfx_volume),
            (MUSIC_VOLUME_GLOBAL, save.music_volume),
            (ITEM_POOL_1_GLOBAL, save.item_pool_1),
            (ITEM_POOL_2_GLOBAL, save.item_pool_2),
            (GEM_COUNT_GLOBAL, u32::from(save.gem_count)),
            (KEY_COUNT_GLOBAL, save.key_count),
            (LEVELS_UNLOCKED_GLOBAL, save.level_count),
            (CURRENT_MAP_LEVEL_GLOBAL, save.level_count),
        ] {
            self.set_global_word(index, value)?;
        }
        Ok(())
    }

    pub(crate) fn record_completed_card_load(&mut self, save: SaveData) {
        self.completed_card_load = Some(save);
    }

    pub(crate) fn take_completed_card_load(&mut self) -> Option<SaveData> {
        self.completed_card_load.take()
    }

    /// Publishes one coherent card snapshot in native readiness order.
    pub fn publish_retail_card_state(&mut self, state: CardPublishedState) -> Result<(), VmError> {
        for (offset, value) in state.partinfos.into_iter().enumerate() {
            self.set_global_word(CARD_PARTINFOS_GLOBAL + offset, value)?;
        }
        debug_assert_eq!(CARD_SLOT_COUNT, state.partinfos.len());
        self.set_global_word(CARD_PART_COUNT_GLOBAL, state.part_count)?;
        self.set_global_word(CARD_FLAGS_GLOBAL, u32::from(state.flags.bits()))
    }

    /// Seeds the pointer-free scalar subset written by `LevelInitGlobals`,
    /// `LevelResetGlobals`, `LdatInit`, and `GoolInitLid` before any GOOL code
    /// executes. Pointer-valued globals remain null checked handles.
    pub fn initialize_retail_level_globals(&mut self, level: LevelId) {
        let initial_lives = 4_u32 << 8;
        for (index, value) in [
            (CURRENT_LEVEL_GLOBAL, level.get() << 8),
            (NEXT_DISPLAY_GLOBAL, INITIAL_DISPLAY_MASK),
            (CURRENT_DISPLAY_GLOBAL, INITIAL_DISPLAY_MASK),
            (GAME_STATE_GLOBAL, 0),
            (TITLE_STATE_GLOBAL, 7),
            (SAVED_TITLE_STATE_GLOBAL, u32::MAX),
            (CURRENT_MAP_LEVEL_GLOBAL, 99),
            (LIFE_COUNT_GLOBAL, initial_lives),
            (INITIAL_LIFE_COUNT_GLOBAL, initial_lives),
            (MONO_GLOBAL, 0),
            (SFX_VOLUME_GLOBAL, 255),
            (MUSIC_VOLUME_GLOBAL, 255),
            (LEVEL_COUNT_GLOBAL, 1),
            (LEVELS_UNLOCKED_GLOBAL, 1),
            (CHECKPOINT_ID_GLOBAL, u32::MAX),
        ] {
            if let Some(global) = self.globals.get_mut(index) {
                *global = value;
            }
        }
    }

    /// Supplies the cooperative host timing consumed by opcode `0x1b`.
    pub fn set_ticks_per_frame(&mut self, ticks_per_frame: i32) {
        self.set_frame_timing(ticks_per_frame, ticks_per_frame);
    }

    /// Supplies the source browser's unrounded global tick delta and rounded
    /// physics/GOOL scaling value for one cooperative frame.
    pub fn set_frame_timing(&mut self, ticks_current_frame: i32, ticks_per_frame: i32) {
        self.ticks_per_frame = ticks_per_frame;
        if let Some(global) = self.globals.get_mut(TICKS_CURRENT_FRAME_GLOBAL) {
            *global = ticks_current_frame as u32;
        }
    }

    /// Current rounded cooperative timing used by retail movement and
    /// `GoolObjectRotate`. Negative host deltas are clamped at the same safe
    /// boundary used by physics contexts.
    #[must_use]
    pub fn ticks_per_frame(&self) -> u32 {
        u32::try_from(self.ticks_per_frame).unwrap_or(0)
    }

    /// Supplies the presentation counter consumed by opcode `0x1e`.
    pub fn set_draw_count(&mut self, draw_count: u32) {
        self.draw_count = draw_count;
        if let Some(global) = self.globals.get_mut(DRAW_COUNT_GLOBAL) {
            *global = draw_count;
        }
    }

    /// Supplies the retail simulation-frame stamp used by animation waits.
    pub fn set_frames_elapsed(&mut self, frames_elapsed: u32) {
        self.frames_elapsed = frames_elapsed;
    }

    /// Supplies the camera/game-state globals read by native object physics.
    /// They are frozen for the complete retail preorder traversal, just like
    /// the pad snapshot and cooperative tick duration.
    pub fn set_retail_physics_frame_context(
        &mut self,
        game_state_playing: bool,
        camera_rotation_xz: i32,
    ) {
        self.set_retail_frame_context(
            if game_state_playing { 0x100 } else { 0 },
            camera_rotation_xz,
        );
    }

    /// Supplies the exact retail game-state word and camera-relative heading
    /// frozen for one source-ordered object traversal.
    pub fn set_retail_frame_context(&mut self, game_state: i32, camera_rotation_xz: i32) {
        if let Some(global) = self.globals.get_mut(GAME_STATE_GLOBAL) {
            *global = game_state as u32;
        }
        self.latch_retail_frame_context(game_state, camera_rotation_xz);
    }

    /// Freezes the live post-camera-effect context for object physics without
    /// rewriting the shared `game_state` global.
    ///
    /// `CamUpdate` writes that global before a synchronous `LevelUpdate`.
    /// Departing TERM handlers may then replace it, so browser-ordered hosts
    /// must read the final live word and latch it here rather than replaying a
    /// precomputed camera result after those handlers return.
    pub fn latch_retail_frame_context(&mut self, game_state: i32, camera_rotation_xz: i32) {
        self.retail_game_state_playing = game_state == 0x100;
        self.camera_rotation_xz = i32::from(Angle12::new(camera_rotation_xz).raw());
        if let Some(global) = self.globals.get_mut(CAMERA_ROTATION_GLOBAL) {
            *global = self.camera_rotation_xz as u32;
        }
    }

    /// Replaces the native global `cur_zone` octree/query neighborhood.
    /// Object-owned environments are deliberately unaffected: their ZDAT
    /// identity and color header follow `obj->zone`, not the camera.
    pub fn set_current_retail_solid_environment(
        &mut self,
        environment: Option<RetailSolidEnvironment>,
    ) {
        self.current_solid_environment = environment;
    }

    /// Returns the active `cur_zone` object-shader selector, when a retail
    /// ZDAT environment has been installed for this traversal.
    #[must_use]
    pub fn current_retail_object_shader(&self) -> Option<(u32, [u16; COLOR_COUNT], i32)> {
        self.current_solid_environment.as_ref().map(|environment| {
            (
                environment.object_shader_mode,
                environment.object_colors,
                environment.object_shader_depth_anchor,
            )
        })
    }

    /// Returns the frozen camera snapshot shared by GOOL projection and the
    /// source-ordered object display boundary.
    #[must_use]
    pub const fn transform_vectors_camera(&self) -> Option<RetailTransformVectorsCamera> {
        self.transform_vectors_camera
    }

    #[must_use]
    pub const fn frames_elapsed(&self) -> u32 {
        self.frames_elapsed
    }

    /// Returns the owned mirror of native process-global `cur_zone_query`.
    /// This is exposed read-only so legal-data characterization tests can
    /// distinguish cache reuse from a collision or handler mutation.
    #[must_use]
    pub fn retail_solid_query_cache(&self) -> Option<&SolidQuery> {
        self.solid_query_cache.as_ref()
    }

    /// Clears the animation-derived AABB snapshots at the start of a frame.
    /// The retail-sized backing allocation is retained for the next traversal.
    pub fn clear_frame_bounds(&mut self) {
        self.solid_frame_bounds.clear();
        self.solid_frame_bound_incarnations.clear();
    }

    /// Appends one world-space object AABB in host traversal order.
    ///
    /// The AABB is a per-frame snapshot. Solid-query eligibility and object
    /// size remain live VM fields and are read only when the query executes.
    pub fn register_frame_bound(
        &mut self,
        object: ObjectHandle,
        bound: Bounds3,
    ) -> Result<(), VmError> {
        let incarnation = self.object_incarnation(object)?;
        self.solid_frame_bounds
            .push(FrameBound { bound, object })
            .map_err(|error| match error {
                FrameBoundsError::CapacityExceeded => VmError::FrameBoundsCapacityExceeded,
            })?;
        self.solid_frame_bound_incarnations.push(incarnation);
        Ok(())
    }

    /// Snapshots every live collider field read by native solid motion.
    ///
    /// Link six is independent of the frame-bound list, so the linked object
    /// must be resolved through the checked machine table instead of inferred
    /// from collision candidates.
    fn solid_collider_state(&self, collider: ObjectHandle) -> Result<SolidColliderState, VmError> {
        let object = self.object(collider)?;
        let translation = object.retail_transform()?.translation;
        Ok(SolidColliderState {
            id: u32::from(collider.get()),
            translation: Vec3 {
                x: translation[0],
                y: translation[1],
                z: translation[2],
            },
            status_b: object.register(process_register::STATUS_B)?,
            state_flags: object.register(process_register::STATE_FLAGS)?,
            object_type: object
                .program_identity
                .map_or(0, GoolProgramIdentity::object_type),
            hotspot_size: object.register(process_register::HOTSPOT_SIZE)? as i32,
        })
    }

    /// Applies one checked, pointer-free `GoolCollide` call.
    ///
    /// All process fields are snapshotted before either object is mutated.
    /// Link and status writes then retain native source-before-target ordering,
    /// including when `target == source` or the current collider aliases one
    /// of the participants.
    pub(crate) fn collide_retail_objects(
        &mut self,
        target: ObjectHandle,
        target_bound: Bounds3,
        source: ObjectHandle,
        source_bound: Bounds3,
    ) -> Result<bool, VmError> {
        let snapshot = |object: &VmObject| -> Result<ObjectCollisionState, VmError> {
            let translation = object.retail_transform()?.translation;
            Ok(ObjectCollisionState {
                translation: Vec3 {
                    x: translation[0],
                    y: translation[1],
                    z: translation[2],
                },
                state_flags: object.register(process_register::STATE_FLAGS)?,
                hotspot_size: object.register(process_register::HOTSPOT_SIZE)? as i32,
            })
        };
        let target_object = self.object(target)?;
        let target_state = snapshot(target_object)?;
        let target_collider = self.resolve_process_link(target, 6)?;
        let source_state = snapshot(self.object(source)?)?;
        let current = if target_collider.is_some_and(|current| current != source)
            && source_state.state_flags & 0x800 == 0
        {
            Some(snapshot(
                self.object(target_collider.expect("checked some"))?,
            )?)
        } else {
            None
        };
        let resolution = resolve_object_collision(
            target_state,
            target_collider.map(|object| u32::from(object.get())),
            target_bound,
            u32::from(source.get()),
            source_state,
            source_bound,
            current,
        )
        .map_err(VmError::RetailSolidMotion)?;

        if resolution.links != ObjectCollisionLinks::Unchanged {
            self.object_mut(source)?.set_link(6, Some(target))?;
        }
        if resolution.links == ObjectCollisionLinks::Both {
            self.object_mut(target)?.set_link(6, Some(source))?;
        }
        if resolution.target_hotspot {
            let status_a = self.object(target)?.register(process_register::STATUS_A)?;
            self.object_mut(target)?.set_register(
                process_register::STATUS_A,
                status_a | STATUS_HOTSPOT_COLLISION,
            )?;
        }
        if resolution.source_hotspot {
            let status_a = self.object(source)?.register(process_register::STATUS_A)?;
            self.object_mut(source)?.set_register(
                process_register::STATUS_A,
                status_a | STATUS_HOTSPOT_COLLISION,
            )?;
        }
        Ok(resolution.links == ObjectCollisionLinks::Both)
    }

    /// Returns this frame's AABB snapshots in their exact registration order.
    #[must_use]
    pub fn frame_bounds(&self) -> &[FrameBound<ObjectHandle>] {
        self.solid_frame_bounds.as_slice()
    }

    /// Evaluates one candidate using the exact `GoolSendIfColliding` mode
    /// predicates. Traversal order and mode-five throttling remain the
    /// runtime's responsibility because their query counter spans candidates.
    pub fn send_event_candidate_matches(
        &self,
        sender: ObjectHandle,
        recipient: ObjectHandle,
        mode: u8,
    ) -> Result<bool, VmError> {
        let sender_object = self.object(sender)?;
        let recipient_object = self.object(recipient)?;
        let category = recipient_object
            .program_identity
            .map_or(0, GoolProgramIdentity::category);
        if matches!(mode, 3..=5) && !matches!(category, 0x300 | 0x400) {
            return Ok(false);
        }

        let translation = |object: &VmObject| -> Result<Vec3, VmError> {
            Ok(Vec3 {
                x: object.register(process_register::TRANSLATION_X)? as i32,
                y: object.register(process_register::TRANSLATION_Y)? as i32,
                z: object.register(process_register::TRANSLATION_Z)? as i32,
            })
        };
        let translated_bound = |object: &VmObject| -> Result<Bounds3, VmError> {
            Ok(object.local_bound.translated(translation(object)?))
        };

        Ok(match mode {
            0 | 6 | 7 => true,
            1 | 4 => bounds_intersect_asymmetric(
                translated_bound(recipient_object)?,
                translated_bound(sender_object)?,
            ),
            2 | 3 => point_in_bound(
                translation(recipient_object)?,
                translated_bound(sender_object)?,
            ),
            5 => {
                let center = translation(sender_object)?;
                let extent_x = sender_object.register(process_register::MISC_B_Y)? as i32;
                let extent_y = sender_object.register(process_register::MISC_B_X)? as i32;
                let extent_z = sender_object.register(process_register::MISC_B_Z)? as i32;
                let sender_bound = Bounds3 {
                    min: Vec3 {
                        x: center.x.wrapping_sub(extent_x),
                        y: center.y.wrapping_sub(extent_y),
                        z: center.z.wrapping_sub(extent_z),
                    },
                    max: Vec3 {
                        x: center.x.wrapping_add(extent_x),
                        y: center.y.wrapping_add(extent_y),
                        z: center.z.wrapping_add(extent_z),
                    },
                };
                bounds_intersect_asymmetric(translated_bound(recipient_object)?, sender_bound)
            }
            _ => unreachable!("send-event mode is three bits"),
        })
    }

    /// Supplies the current retail camera translation used by shadow sizing.
    pub fn set_camera_translation(&mut self, translation: [i32; 3]) {
        self.camera_translation = translation;
        for (offset, value) in translation.into_iter().enumerate() {
            if let Some(global) = self
                .globals
                .get_mut(CAMERA_TRANSLATION_GLOBAL.saturating_add(offset))
            {
                *global = value as u32;
            }
        }
        if let Some(camera) = &mut self.transform_vectors_camera {
            camera.translation = translation;
        }
    }

    /// Supplies the complete frozen camera snapshot used by transform-vector
    /// projection and audio-space operations during this cooperative frame.
    pub fn set_transform_vectors_camera(&mut self, camera: RetailTransformVectorsCamera) {
        self.set_camera_translation(camera.translation);
        for (offset, value) in camera.rotation_yxz.into_iter().enumerate() {
            if let Some(global) = self
                .globals
                .get_mut(CAMERA_ROTATION_YXZ_GLOBAL.saturating_add(offset))
            {
                *global = value as u32;
            }
        }
        self.transform_vectors_camera = Some(camera);
    }

    /// Replaces one complete retail pad history snapshot.
    ///
    /// `crust-platform` advances the five history words once per simulation
    /// frame; the VM merely consumes that already-normalized state.
    pub fn set_pad_snapshot(
        &mut self,
        port: usize,
        snapshot: RetailPadSnapshot,
    ) -> Result<(), VmError> {
        *self
            .pads
            .get_mut(port)
            .ok_or(VmError::InvalidPadPort(port))? = snapshot;
        Ok(())
    }

    /// Returns one complete retail pad history snapshot.
    pub fn pad_snapshot(&self, port: usize) -> Result<RetailPadSnapshot, VmError> {
        self.pads
            .get(port)
            .copied()
            .ok_or(VmError::InvalidPadPort(port))
    }

    fn restore_retail_intensity_from_zone(&mut self, handle: ObjectHandle) -> Result<(), VmError> {
        let intensity = {
            let object = self.object(handle)?;
            let environment = object
                .solid_environment
                .as_ref()
                .ok_or(VmError::MissingSolidEnvironment(handle))?;
            let source = if object.is_main_player {
                &environment.player_colors
            } else {
                &environment.object_colors
            };
            [
                source[COLOR_INTENSITY_START],
                source[COLOR_INTENSITY_START + 1],
                source[COLOR_INTENSITY_START + 2],
            ]
        };
        let object = self.object_mut(handle)?;
        object.set_register(process_register::INVINCIBILITY_STATE, 0)?;
        object.colors[COLOR_INTENSITY_START..COLOR_INTENSITY_END].copy_from_slice(&intensity);
        Ok(())
    }

    /// Executes source `GoolObjectColors` for one live object.
    ///
    /// This preserves the intentional switch fallthrough for invincibility
    /// states two through five. The runtime-facing variant below additionally
    /// hosts the category-`0x300` collider interrupt at its source call site.
    pub fn run_retail_object_colors(&mut self, handle: ObjectHandle) -> Result<(), VmError> {
        self.run_retail_object_colors_impl(handle, true, |_, _, _, _| {})?;
        Ok(())
    }

    /// Executes retail object colors while synchronously hosting the
    /// invincibility-hit event at the exact point of source dispatch.
    ///
    /// The returned flag is false when nested delivery removes or reuses the
    /// sender's VM slot. In that case the old pass must not write cyclic color
    /// intensity into the replacement object or continue into physics.
    pub(crate) fn run_retail_object_colors_with_event_handler(
        &mut self,
        handle: ObjectHandle,
        event_handler: impl FnMut(&mut Self, ObjectHandle, ObjectHandle, u32),
    ) -> Result<bool, VmError> {
        self.run_retail_object_colors_impl(handle, false, event_handler)
    }

    fn run_retail_object_colors_impl(
        &mut self,
        handle: ObjectHandle,
        emit_event_effect: bool,
        mut event_handler: impl FnMut(&mut Self, ObjectHandle, ObjectHandle, u32),
    ) -> Result<bool, VmError> {
        let incarnation = self.object_incarnation(handle)?;
        let collider = self.resolve_process_link(handle, 6)?;
        let (invincibility_state, invincibility_stamp, status_a, status_b, is_main_player) = {
            let object = self.object(handle)?;
            (
                object.register(process_register::INVINCIBILITY_STATE)?,
                object.register(process_register::INVINCIBILITY_STAMP)?,
                object.register(process_register::STATUS_A)?,
                object.register(process_register::STATUS_B)?,
                object.is_main_player,
            )
        };
        // Both source operands are unsigned words. Assignment to its signed
        // local retains the low 32 bits on the characterized two's-complement
        // target, so a future stamp does not spuriously expire the state.
        let elapsed_since = self.frames_elapsed.wrapping_sub(invincibility_stamp) as i32;

        match invincibility_state {
            2..=5 => {
                let expired = match invincibility_state {
                    3 => elapsed_since > 451,
                    4 => elapsed_since > 60,
                    5 => elapsed_since > 602,
                    _ => false,
                };
                if expired {
                    self.restore_retail_intensity_from_zone(handle)?;
                }

                // Case four performs this branch after its timeout reset and
                // before falling through to the shared cyclic intensity.
                if invincibility_state == 4
                    && let Some(collider) = collider
                    && self
                        .object(collider)?
                        .program_identity
                        .is_some_and(|identity| identity.category() == 0x300)
                {
                    if emit_event_effect {
                        self.emit(VmEffect::Event {
                            sender: handle,
                            recipient: Some(collider),
                            event: HIT_INVINCIBLE_EVENT,
                        })?;
                    }
                    event_handler(self, handle, collider, HIT_INVINCIBLE_EVENT);
                    if !self.incarnation_is_live(handle, incarnation) {
                        return Ok(false);
                    }
                }

                let modulus = (self.draw_count % 4) << 8;
                let value = (if modulus < 0x100 {
                    modulus + 0x7f
                } else {
                    0x47f - modulus
                }) as u16;
                self.object_mut(handle)?.colors[COLOR_INTENSITY_START..COLOR_INTENSITY_END]
                    .fill(value);
            }
            6 => {
                if elapsed_since > 15 {
                    let object = self.object_mut(handle)?;
                    object.set_register(process_register::INVINCIBILITY_STATE, 0)?;
                    object.set_register(
                        process_register::STATUS_B,
                        status_b | STATUS_B_DPAD_CONTROL,
                    )?;
                }
            }
            7 => {
                if elapsed_since > 15 || status_a & 1 != 0 {
                    let object = self.object_mut(handle)?;
                    object.set_register(process_register::INVINCIBILITY_STATE, 0)?;
                    object.set_register(
                        process_register::STATUS_B,
                        status_b | STATUS_B_DPAD_CONTROL,
                    )?;
                }
            }
            _ => {
                if is_main_player && status_b & STATUS_B_MAIN_COLOR_BY_ZONE != 0 {
                    self.restore_retail_intensity_from_zone(handle)?;
                }
            }
        }
        Ok(true)
    }

    /// Executes the native post-interpreter color and physics phases for one
    /// live GOOL object, in source order. Static ZDAT floor response is
    /// resolved here against the machine's global current-zone collision
    /// environment; collision-generated event effects are committed at their
    /// source call sites through the hosted variant below.
    pub fn run_retail_object_physics(
        &mut self,
        handle: ObjectHandle,
    ) -> Result<RetailPhysicsResult, VmError> {
        self.run_retail_object_physics_with_solid_event_handler(handle, |_, _, _, _| true)
    }

    /// Executes retail physics while hosting each collision-generated GOOL
    /// event at its native call site.
    ///
    /// Returning `false` from `solid_event_handler` stops the pass before any
    /// later collision work. This is used when synchronous delivery kills the
    /// mover or requests a level restart. The callback runs after all earlier
    /// collision-link/status effects have been committed and after the mover's
    /// current process fields have been made visible in the VM.
    pub fn run_retail_object_physics_with_solid_event_handler(
        &mut self,
        handle: ObjectHandle,
        solid_event_handler: impl FnMut(
            &mut Self,
            ObjectHandle,
            &mut [SolidObjectCandidate],
            SolidEffect,
        ) -> bool,
    ) -> Result<RetailPhysicsResult, VmError> {
        self.run_retail_object_colors(handle)?;
        self.run_retail_object_physics_after_colors_with_solid_event_handler(
            handle,
            solid_event_handler,
        )
    }

    /// Executes the physics portion of the native post-interpreter pass after
    /// its caller has completed the source-ordered color phase.
    pub(crate) fn run_retail_object_physics_after_colors_with_solid_event_handler(
        &mut self,
        handle: ObjectHandle,
        mut solid_event_handler: impl FnMut(
            &mut Self,
            ObjectHandle,
            &mut [SolidObjectCandidate],
            SolidEffect,
        ) -> bool,
    ) -> Result<RetailPhysicsResult, VmError> {
        let object_type = self
            .object(handle)?
            .program_identity
            .map_or(0, GoolProgramIdentity::object_type);
        let context = RetailPhysicsContext {
            ticks_per_frame: self.ticks_per_frame.max(0) as u32,
            game_state_playing: self.retail_game_state_playing,
            camera_rotation_xz: self.camera_rotation_xz,
            pad_held: self.pad_snapshot(0)?.held,
            frame_stamp: self.frames_elapsed,
            object_type,
        };
        let mut state = self.object(handle)?.retail_physics_state()?;
        let plan = begin_retail_physics(&mut state, context);
        if plan.clear_collider {
            self.object_mut(handle)?.set_link(6, None)?;
        }
        match plan.translation_mode {
            RetailTranslationMode::None => {}
            RetailTranslationMode::Free => {
                let _changed = apply_free_movement(&mut state, plan);
            }
            RetailTranslationMode::StoppedBySolid => {
                if self.resolve_retail_static_solid_motion(
                    handle,
                    &mut state,
                    plan,
                    &mut solid_event_handler,
                )? {
                    return Ok(RetailPhysicsResult {
                        register_collision_bound: false,
                    });
                }
            }
        }
        if path_orientation_requested(&state) {
            let entity_path = self.entity_path(handle)?;
            self.object_mut(handle)?
                .orient_retail_physics_on_path(entity_path.as_deref(), &mut state)?;
        }
        let result = finalize_retail_physics(&mut state, context);
        self.object_mut(handle)?.set_retail_physics_state(state)?;
        Ok(result)
    }

    fn resolve_retail_static_solid_motion(
        &mut self,
        handle: ObjectHandle,
        state: &mut RetailPhysicsState,
        plan: RetailPhysicsPlan,
        solid_event_handler: &mut impl FnMut(
            &mut Self,
            ObjectHandle,
            &mut [SolidObjectCandidate],
            SolidEffect,
        ) -> bool,
    ) -> Result<bool, VmError> {
        let collider_handle = self.resolve_process_link(handle, 6)?;
        let (
            environment,
            local_bound,
            object_zone_context,
            smooth_stop,
            status_c,
            animation_stamp,
            hotspot_size,
        ) = {
            let object = self.object(handle)?;
            let object_zone_context = object
                .solid_zone_eid
                .map(|eid| {
                    let bound_environment = object
                        .solid_environment
                        .as_ref()
                        .ok_or(VmError::MissingSolidEnvironment(handle))?;
                    let zone = bound_environment
                        .neighbors
                        .iter()
                        .find(|zone| zone.eid == eid)
                        .ok_or(VmError::SolidObjectZoneMissingFromBoundEnvironment {
                            object: handle,
                            zone: eid,
                        })?;
                    Ok((
                        eid,
                        SolidZoneBoundary {
                            origin: zone.origin,
                            dimensions: zone.dimensions,
                            graphics_flags: zone.graphics_flags,
                            water_y: zone.water_y,
                        },
                    ))
                })
                .transpose()?;
            (
                self.current_solid_environment
                    .clone()
                    .ok_or(VmError::MissingSolidEnvironment(handle))?,
                object.local_bound,
                object_zone_context,
                self.solid_smooth_stop,
                object.register(process_register::STATUS_C)?,
                object.register(process_register::ANIMATION_STAMP)? as i32,
                object.register(process_register::HOTSPOT_SIZE)? as i32,
            )
        };
        let collider = collider_handle
            .map(|collider| self.solid_collider_state(collider))
            .transpose()?;

        let object_zone =
            object_zone_context.map_or(SolidObjectZone::Missing, |(eid, boundary)| {
                environment
                    .neighbors
                    .iter()
                    .position(|zone| zone.eid == eid)
                    .map_or(
                        SolidObjectZone::Detached { eid, boundary },
                        SolidObjectZone::CurrentNeighbor,
                    )
            });

        let mut zones = Vec::with_capacity(environment.neighbors.len());
        for zone in &environment.neighbors {
            zones.push(
                SolidZoneView::new(
                    zone.origin,
                    zone.dimensions,
                    zone.root,
                    zone.max_depth,
                    &zone.bytes,
                )
                .map_err(VmError::RetailSolidMotion)?
                .with_graphics(zone.graphics_flags, zone.water_y),
            );
        }

        // The source reads the frame-bound list in traversal order. Each
        // snapshot owns only its world AABB; all gates and metadata are live
        // object fields at the exact moment this mover reaches physics.
        let mut candidates = Vec::with_capacity(self.solid_frame_bounds.len());
        for (bound_index, snapshot) in self.solid_frame_bounds.iter().enumerate() {
            let registered_incarnation = self
                .solid_frame_bound_incarnations
                .get(bound_index)
                .copied();
            let active = registered_incarnation
                .is_some_and(|incarnation| self.incarnation_is_live(snapshot.object, incarnation));
            if !active {
                candidates.push(SolidObjectCandidate {
                    id: u32::from(snapshot.object.get()),
                    active: false,
                    translation: Vec3::ZERO,
                    bounds: snapshot.bound,
                    status_b: 0,
                    status_c: 0,
                    state_flags: 0,
                    category: 0,
                    object_type: 0,
                    hotspot_size: 0,
                });
                continue;
            }
            let candidate = self.object(snapshot.object)?;
            let identity = candidate.program_identity;
            candidates.push(SolidObjectCandidate {
                id: u32::from(snapshot.object.get()),
                active,
                translation: Vec3 {
                    x: candidate.register(process_register::TRANSLATION_X)? as i32,
                    y: candidate.register(process_register::TRANSLATION_Y)? as i32,
                    z: candidate.register(process_register::TRANSLATION_Z)? as i32,
                },
                bounds: snapshot.bound,
                status_b: candidate.register(process_register::STATUS_B)?,
                status_c: candidate.register(process_register::STATUS_C)?,
                state_flags: candidate.register(process_register::STATE_FLAGS)?,
                category: identity.map_or(0, GoolProgramIdentity::category),
                object_type: identity.map_or(0, GoolProgramIdentity::object_type),
                hotspot_size: candidate.register(process_register::HOTSPOT_SIZE)? as i32,
            });
        }

        let solid_state = SolidMotionState {
            object_id: Some(u32::from(handle.get())),
            translation: state.translation,
            velocity: state.velocity,
            local_bound,
            status_a: state.status_a,
            status_b: state.status_b,
            status_c,
            state_flags: state.state_flags,
            invincibility_state: state.invincibility_state as i32,
            animation_stamp,
            floor_impact_stamp: state.floor_impact_stamp as i32,
            floor_impact_velocity: state.floor_impact_velocity,
            event: state.event,
            hotspot_size,
            collider,
        };
        // `begin_retail_physics` has already changed fields outside the solid
        // solver's narrower state (notably speed and rotations). Carry that
        // complete snapshot through synchronous event boundaries so a handler
        // observes every earlier native physics mutation and its own changes
        // survive any later collision event in the same pull loop.
        let mut live_physics_state = *state;
        let mut applied_effects = 0_usize;
        let mut hook_error = None;
        let mut interrupted = false;
        // The event hook needs `&mut Machine`, so temporarily move the owned
        // BSS query out instead of borrowing one field across nested GOOL
        // dispatch. Restore it before inspecting every solver/error outcome.
        let mut query_cache = self.solid_query_cache.take();
        let outcome = solve_retail_solid_motion_with_event_handler(
            &zones,
            &mut candidates,
            solid_state,
            plan.displacement,
            SolidMotionContext {
                frame_stamp: self.frames_elapsed as i32,
                object_zone,
                current_world_graphics_flags: environment.graphics_flags,
                quirks: environment.level_quirks,
            },
            smooth_stop,
            &mut query_cache,
            |solid_state, object_zone, candidates, effects, event| {
                if let Err(error) =
                    self.commit_live_solid_motion_state(handle, live_physics_state, solid_state)
                {
                    hook_error = Some(error);
                    interrupted = true;
                    return false;
                }
                if let Err(error) =
                    self.commit_live_solid_object_zone(handle, &environment, *object_zone)
                {
                    hook_error = Some(error);
                    interrupted = true;
                    return false;
                }
                if let Err(error) =
                    self.apply_retail_solid_effects(handle, &effects[applied_effects..])
                {
                    hook_error = Some(error);
                    interrupted = true;
                    return false;
                }
                applied_effects = effects.len();
                if !solid_event_handler(self, handle, candidates, event) {
                    interrupted = true;
                    return false;
                }
                match self.object(handle).and_then(VmObject::retail_physics_state) {
                    Ok(refreshed) => live_physics_state = refreshed,
                    Err(error) => {
                        hook_error = Some(error);
                        interrupted = true;
                        return false;
                    }
                }
                if let Err(error) = self.refresh_live_solid_motion_state(handle, solid_state) {
                    hook_error = Some(error);
                    interrupted = true;
                    return false;
                }
                if let Err(error) =
                    self.refresh_live_solid_object_zone(handle, &environment, object_zone)
                {
                    hook_error = Some(error);
                    interrupted = true;
                    return false;
                }
                true
            },
        );
        self.solid_query_cache = query_cache;
        let outcome = outcome.map_err(VmError::RetailSolidMotion)?;
        if let Some(error) = hook_error {
            return Err(error);
        }
        if interrupted {
            // Native computes `being_stopped`/`prev_velocity` after
            // TransPullStopAtSolid returns. A safely modeled recipient kill
            // stops our remaining pointer-backed work, but the completed
            // partial solver outcome still owns the process-global update.
            self.solid_smooth_stop = outcome.smooth_stop;
            return Ok(true);
        }

        // A synchronous handler may update any process register. Preserve
        // those live values as the base for the remaining native physics
        // phases, then overlay fields changed by collision after the handler.
        let mut live_state = live_physics_state;
        live_state.translation = outcome.state.translation;
        live_state.velocity = outcome.state.velocity;
        live_state.status_a = outcome.state.status_a;
        live_state.status_b = outcome.state.status_b;
        live_state.state_flags = outcome.state.state_flags;
        live_state.invincibility_state = outcome.state.invincibility_state as u32;
        live_state.floor_impact_stamp = outcome.state.floor_impact_stamp as u32;
        live_state.floor_impact_velocity = outcome.state.floor_impact_velocity;
        live_state.event = outcome.state.event;
        *state = live_state;

        {
            let object = self.object_mut(handle)?;
            object.solid_zone_eid = match outcome.object_zone {
                SolidObjectZone::Missing => None,
                SolidObjectZone::CurrentNeighbor(index) => Some(
                    environment
                        .neighbors
                        .get(index)
                        .ok_or(VmError::RetailSolidMotion(
                            SolidMotionError::InvalidObjectZoneIndex {
                                index,
                                zone_count: environment.neighbors.len(),
                            },
                        ))?
                        .eid,
                ),
                SolidObjectZone::Detached { eid, .. } => Some(eid),
            };
            let collider = outcome
                .state
                .collider
                .and_then(|candidate| u16::try_from(candidate.id).ok())
                .and_then(ObjectHandle::new);
            object.set_link(6, collider)?;
        }
        self.solid_smooth_stop = outcome.smooth_stop;
        self.apply_retail_solid_effects(handle, &outcome.effects[applied_effects..])?;
        Ok(false)
    }

    fn commit_live_solid_motion_state(
        &mut self,
        handle: ObjectHandle,
        mut physics_state: RetailPhysicsState,
        state: &SolidMotionState,
    ) -> Result<(), VmError> {
        physics_state.translation = state.translation;
        physics_state.velocity = state.velocity;
        physics_state.status_a = state.status_a;
        physics_state.status_b = state.status_b;
        physics_state.state_flags = state.state_flags;
        physics_state.invincibility_state = state.invincibility_state as u32;
        physics_state.floor_impact_stamp = state.floor_impact_stamp as u32;
        physics_state.floor_impact_velocity = state.floor_impact_velocity;
        physics_state.event = state.event;
        let object = self.object_mut(handle)?;
        object.set_retail_physics_state(physics_state)?;
        for (register, value) in [
            (process_register::STATUS_C, state.status_c as i32),
            (process_register::ANIMATION_STAMP, state.animation_stamp),
            (process_register::HOTSPOT_SIZE, state.hotspot_size),
        ] {
            object.set_register(register, value as u32)?;
        }
        let collider = state
            .collider
            .and_then(|candidate| u16::try_from(candidate.id).ok())
            .and_then(ObjectHandle::new);
        object.set_link(6, collider)?;
        Ok(())
    }

    fn refresh_live_solid_motion_state(
        &self,
        handle: ObjectHandle,
        state: &mut SolidMotionState,
    ) -> Result<(), VmError> {
        let object = self.object(handle)?;
        state.translation = Vec3 {
            x: object.register(process_register::TRANSLATION_X)? as i32,
            y: object.register(process_register::TRANSLATION_Y)? as i32,
            z: object.register(process_register::TRANSLATION_Z)? as i32,
        };
        state.velocity = Vec3 {
            x: object.register(process_register::MISC_A_X)? as i32,
            y: object.register(process_register::MISC_A_Y)? as i32,
            z: object.register(process_register::MISC_A_Z)? as i32,
        };
        state.status_a = object.register(process_register::STATUS_A)?;
        state.status_b = object.register(process_register::STATUS_B)?;
        state.status_c = object.register(process_register::STATUS_C)?;
        state.state_flags = object.register(process_register::STATE_FLAGS)?;
        state.invincibility_state = object.register(process_register::INVINCIBILITY_STATE)? as i32;
        state.animation_stamp = object.register(process_register::ANIMATION_STAMP)? as i32;
        state.floor_impact_stamp = object.register(process_register::FLOOR_IMPACT_STAMP)? as i32;
        state.floor_impact_velocity =
            object.register(process_register::FLOOR_IMPACT_VELOCITY)? as i32;
        state.event = object.register(process_register::EVENT)?;
        state.hotspot_size = object.register(process_register::HOTSPOT_SIZE)? as i32;
        let collider = self.resolve_process_link(handle, 6)?;
        state.collider = collider
            .map(|collider| self.solid_collider_state(collider))
            .transpose()?;
        Ok(())
    }

    fn commit_live_solid_object_zone(
        &mut self,
        handle: ObjectHandle,
        environment: &RetailSolidEnvironment,
        object_zone: SolidObjectZone,
    ) -> Result<(), VmError> {
        let zone = match object_zone {
            SolidObjectZone::Missing => None,
            SolidObjectZone::CurrentNeighbor(index) => Some(
                environment
                    .neighbors
                    .get(index)
                    .ok_or(VmError::RetailSolidMotion(
                        SolidMotionError::InvalidObjectZoneIndex {
                            index,
                            zone_count: environment.neighbors.len(),
                        },
                    ))?
                    .eid,
            ),
            SolidObjectZone::Detached { eid, .. } => Some(eid),
        };
        self.object_mut(handle)?.set_retail_solid_zone_eid(zone);
        Ok(())
    }

    fn refresh_live_solid_object_zone(
        &self,
        handle: ObjectHandle,
        environment: &RetailSolidEnvironment,
        object_zone: &mut SolidObjectZone,
    ) -> Result<(), VmError> {
        let object = self.object(handle)?;
        let Some(eid) = object.solid_zone_eid else {
            *object_zone = SolidObjectZone::Missing;
            return Ok(());
        };
        if let Some(index) = environment
            .neighbors
            .iter()
            .position(|zone| zone.eid == eid)
        {
            *object_zone = SolidObjectZone::CurrentNeighbor(index);
            return Ok(());
        }
        let bound_environment = object
            .solid_environment
            .as_ref()
            .ok_or(VmError::MissingSolidEnvironment(handle))?;
        let zone = bound_environment
            .neighbors
            .iter()
            .find(|zone| zone.eid == eid)
            .ok_or(VmError::SolidObjectZoneMissingFromBoundEnvironment {
                object: handle,
                zone: eid,
            })?;
        *object_zone = SolidObjectZone::Detached {
            eid,
            boundary: SolidZoneBoundary {
                origin: zone.origin,
                dimensions: zone.dimensions,
                graphics_flags: zone.graphics_flags,
                water_y: zone.water_y,
            },
        };
        Ok(())
    }

    fn apply_retail_solid_effects(
        &mut self,
        moving: ObjectHandle,
        effects: &[SolidEffect],
    ) -> Result<(), VmError> {
        for effect in effects.iter().copied() {
            match effect {
                SolidEffect::SetCandidateCollider { candidate } => {
                    let candidate = solid_effect_handle(candidate)?;
                    self.object_mut(candidate)?.set_link(6, Some(moving))?;
                }
                SolidEffect::SetCandidateStatus {
                    candidate,
                    status_bits,
                } => {
                    let candidate = solid_effect_handle(candidate)?;
                    let status = self
                        .object(candidate)?
                        .register(process_register::STATUS_A)?;
                    self.object_mut(candidate)?
                        .set_register(process_register::STATUS_A, status | status_bits)?;
                }
                SolidEffect::ObjectCollision { .. } | SolidEffect::SendEvent { .. } => {
                    // `GoolCollide` and `GoolSendEvent` are synchronous in the
                    // source. Retain their complete typed payload until the
                    // nested event-service runner consumes it in this frame.
                    self.emit(VmEffect::Solid {
                        object: moving,
                        effect,
                    })?;
                }
                SolidEffect::NodeContact { .. }
                | SolidEffect::ZoneChanged { .. }
                | SolidEffect::MissingZone => {
                    // The pure solver has already committed the corresponding
                    // state, zone, or preserve-previous-zone result.
                }
            }
        }
        Ok(())
    }

    fn intern_entity_path(
        &mut self,
        path: Arc<RetailEntityPath>,
    ) -> Result<EntityReference, VmError> {
        if let Some(slot) = self
            .entity_paths
            .iter()
            .position(|candidate| candidate.as_ref() == path.as_ref())
        {
            return Ok(EntityReference { slot: slot as u32 });
        }
        if self.entity_paths.len() > ENTITY_REFERENCE_SLOT_BITS as usize {
            return Err(VmError::EntityReferenceTableFull);
        }
        let slot = self.entity_paths.len() as u32;
        self.entity_paths.push(path);
        Ok(EntityReference { slot })
    }

    fn bind_pending_entity_path(&mut self, object: &mut VmObject) -> Result<(), VmError> {
        let Some(path) = object.pending_entity_path.clone() else {
            return Ok(());
        };
        let reference = self.intern_entity_path(path)?;
        object.set_register(process_register::ENTITY_REFERENCE, reference.to_word())?;
        object.pending_entity_path = None;
        Ok(())
    }

    fn entity_path(&self, handle: ObjectHandle) -> Result<Option<Arc<RetailEntityPath>>, VmError> {
        let word = self
            .object(handle)?
            .register(process_register::ENTITY_REFERENCE)?;
        if word == 0 {
            return Ok(None);
        }
        let reference =
            EntityReference::from_word(word).ok_or(VmError::InvalidEntityReference(word))?;
        self.entity_paths
            .get(reference.slot as usize)
            .cloned()
            .map(Some)
            .ok_or(VmError::InvalidEntityReference(word))
    }

    pub fn insert_object(&mut self, mut object: VmObject) -> Result<(), VmError> {
        let handle = object.handle;
        if self.objects.contains_key(&handle) {
            return Err(VmError::DuplicateObject(handle));
        }
        if self.objects.len() == MAX_OBJECTS {
            return Err(VmError::TooManyObjects);
        }
        if self.current_solid_environment.is_none() {
            self.current_solid_environment
                .clone_from(&object.solid_environment);
        }
        self.register_paging_metadata(
            object.page_count,
            &object.resident_pages,
            &object.entry_pages,
        )?;
        self.bind_pending_entity_path(&mut object)?;
        self.objects.insert(handle, object);
        self.advance_object_incarnation(handle);
        Ok(())
    }

    /// Installs or replaces one validated VM object while preserving all
    /// stream-level paging metadata. Runtime pool reuse must take this path;
    /// assigning through `object_mut` would bypass EID/page registration.
    pub fn upsert_object(&mut self, mut object: VmObject) -> Result<(), VmError> {
        let handle = object.handle;
        if !self.objects.contains_key(&handle) && self.objects.len() == MAX_OBJECTS {
            return Err(VmError::TooManyObjects);
        }
        if self.current_solid_environment.is_none() {
            self.current_solid_environment
                .clone_from(&object.solid_environment);
        }
        self.register_paging_metadata(
            object.page_count,
            &object.resident_pages,
            &object.entry_pages,
        )?;
        self.bind_pending_entity_path(&mut object)?;
        self.objects.insert(handle, object);
        self.advance_object_incarnation(handle);
        Ok(())
    }

    fn register_paging_metadata(
        &mut self,
        page_count: u32,
        resident_pages: &[PageIndex],
        entry_pages: &[(Eid, PageIndex)],
    ) -> Result<(), VmError> {
        for (eid, page) in entry_pages {
            if let Some(first) = self.entry_pages.get(eid).copied()
                && first != *page
            {
                return Err(VmError::ConflictingEntryPage {
                    eid: *eid,
                    first,
                    second: *page,
                });
            }
        }
        if self.paging_page_capacity_authority == PagingCapacityAuthority::ProgramMetadata {
            self.paging_page_capacity = self
                .paging_page_capacity
                .max(page_count.min(PHYSICAL_SLOT_COUNT as u32));
        }
        for (eid, page) in entry_pages {
            self.entry_pages.insert(*eid, *page);
        }
        // The browser source loads every NSF page into a physical type-1
        // page at NSInit. Residency is therefore independent of reference
        // count; count zero makes a slot available but does not unload it.
        for index in 0..page_count {
            let page = PageIndex::new(index);
            self.paging_baseline_pages.insert(page);
            self.paging_loaded_pages.insert(page);
        }
        for page in resident_pages {
            self.paging_baseline_pages.insert(*page);
            self.paging_loaded_pages.insert(*page);
            // Global/external pages are translated as part of binding. Other
            // physical type-1 pages stay resident but their entry offsets are
            // not resolved until NSOpen translates that page.
            self.paging_resolved_pages.insert(*page);
        }
        Ok(())
    }

    pub fn object(&self, handle: ObjectHandle) -> Result<&VmObject, VmError> {
        self.objects
            .get(&handle)
            .ok_or(VmError::UnknownObject(handle))
    }

    pub fn object_mut(&mut self, handle: ObjectHandle) -> Result<&mut VmObject, VmError> {
        self.objects
            .get_mut(&handle)
            .ok_or(VmError::UnknownObject(handle))
    }

    /// Resolves the live animation pointer with every represented native
    /// storage lifetime in scope.
    ///
    /// Same-object internal/register references stay on the logical object.
    /// Immediate GOPs point into native's shared two-word rotating buffers, so
    /// their checked token deliberately follows later input/output cursor
    /// overwrites. Linked GOPs point into the static retail object pool; that
    /// token keeps reading retained free-slot words and retargets only when
    /// the same physical slot is reused. Bare foreign logical-object tokens
    /// remain rejected because compact handle reuse is not a native lifetime.
    /// External-state tokens also remain rejected because a state rebind
    /// replaces the current vector without preserving the prior entry's
    /// identity in the 32-bit token.
    pub fn animation_source(
        &self,
        handle: ObjectHandle,
    ) -> Result<Option<AnimationSource>, VmError> {
        let object = self.object(handle)?;
        let word = object.register(process_register::ANIMATION_SEQUENCE)?;
        if word == 0 {
            return Ok(None);
        }
        if let Some(reference) = AnimationReference::from_word(word) {
            let _validated_data = object.animation_data(reference)?;
            return Ok(Some(AnimationSource::ItemFive(reference)));
        }

        let storage =
            StorageReference::from_word(word).ok_or(VmError::InvalidAnimationReference(word))?;
        let index = usize::from(storage.index());
        let words = match storage.backing {
            StorageBacking::RetailPool(pool_slot) => self
                .retail_pool_animation_words(pool_slot, index)
                .map_err(|_| VmError::InvalidAnimationReference(word))?,
            StorageBacking::Object(owner) => match storage.region() {
                StorageRegion::Internal if owner == handle => object
                    .internal
                    .get(index..)
                    .ok_or(VmError::InvalidAnimationReference(word))?,
                StorageRegion::Register if owner == handle => object
                    .registers
                    .get(index..)
                    .ok_or(VmError::InvalidAnimationReference(word))?,
                StorageRegion::Constant => self
                    .operand_constants
                    .get(index..)
                    .ok_or(VmError::InvalidAnimationReference(word))?,
                StorageRegion::External | StorageRegion::Internal | StorageRegion::Register => {
                    return Err(VmError::InvalidAnimationReference(word));
                }
            },
        };
        parse_process_animation_reference(storage, words)
            .map(|source| Some(AnimationSource::Process(source)))
    }

    /// Validates a proposed physical-pool association without requiring the
    /// replacement VM object to be installed first.
    pub(crate) fn preflight_retail_pool_slot_binding(
        &self,
        handle: ObjectHandle,
        pool_slot: u8,
    ) -> Result<(), VmError> {
        if usize::from(pool_slot) >= MAX_OBJECTS {
            return Err(VmError::InvalidRetailPoolSlot(pool_slot));
        }
        let bound = self.retail_pool_slots_by_object[usize::from(handle.get())];
        if bound.is_some() && bound != Some(pool_slot) {
            return Err(VmError::RetailPoolSlotMismatch {
                object: handle,
                bound,
                requested: pool_slot,
            });
        }
        if let Some(object) = self.live_object_in_retail_pool_slot(pool_slot)
            && object != handle
        {
            return Err(VmError::RetailPoolSlotOccupied {
                slot: pool_slot,
                object,
            });
        }
        if usize::from(pool_slot) < OBJECT_POOL_CAPACITY
            && bound != Some(pool_slot)
            && !self.retail_free_pool_slots.contains(&pool_slot)
        {
            return Err(VmError::RetailPoolSlotUnavailable(pool_slot));
        }
        Ok(())
    }

    /// Commits a pool-slot association after a successful preflight and
    /// object installation. The shared validation keeps runtime replacement
    /// transactional when a malformed binding names an occupied slot.
    pub(crate) fn bind_retail_pool_slot(
        &mut self,
        handle: ObjectHandle,
        pool_slot: u8,
    ) -> Result<(), VmError> {
        self.object(handle)?;
        self.preflight_retail_pool_slot_binding(handle, pool_slot)?;
        let bound = self.retail_pool_slots_by_object[usize::from(handle.get())];
        if usize::from(pool_slot) < OBJECT_POOL_CAPACITY && bound != Some(pool_slot) {
            let position = self
                .retail_free_pool_slots
                .iter()
                .position(|candidate| *candidate == pool_slot)
                .expect("preflight proved the ordinary retail pool slot is free");
            let predecessor = position
                .checked_sub(1)
                .and_then(|index| self.retail_free_pool_slots.get(index))
                .copied();
            let successor = self.retail_free_pool_slots.get(position + 1).copied();
            if let Some(predecessor) = predecessor {
                self.retired_retail_pool_registers[usize::from(predecessor)]
                    .as_mut()
                    .expect("every free ordinary slot retains its process storage")
                    .set_free_pool_sibling(successor);
            }
            self.retail_free_pool_slots.remove(position);
        }
        self.retail_pool_slots_by_object[usize::from(handle.get())] = Some(pool_slot);
        Ok(())
    }

    /// Seeds a replacement object from the initialized process words retained
    /// by its physical native pool slot. Never-used ordinary slots expose the
    /// deterministic allocator links written by `GoolInitAllocTable`. Words
    /// that native allocation left indeterminate retain the replacement's
    /// authored or checked-default values until selective initialization.
    pub(crate) fn seed_retail_pool_slot_storage(
        &self,
        pool_slot: u8,
        object: &mut VmObject,
    ) -> Result<(), VmError> {
        if usize::from(pool_slot) >= MAX_OBJECTS {
            return Err(VmError::InvalidRetailPoolSlot(pool_slot));
        }
        if let Some(storage) = self.retired_retail_pool_registers[usize::from(pool_slot)].as_ref() {
            object.inherit_retail_process_storage(storage);
        }
        Ok(())
    }

    /// Returns the last initialized translation stored in a now-free native
    /// object-pool slot. Live-object lookup must always take precedence: pool
    /// reuse makes the same native pointer refer to the replacement object.
    #[must_use]
    pub fn retired_retail_translation(&self, handle: ObjectHandle) -> Option<[i32; 3]> {
        self.retired_retail_translations[usize::from(handle.get())]
    }

    /// Returns initialized storage retained by one physical native pool slot.
    /// A live arena occupant must take precedence over this tombstone.
    #[must_use]
    pub(crate) fn retired_retail_pool_translation(&self, pool_slot: u8) -> Option<[i32; 3]> {
        self.retired_retail_pool_translations
            .get(usize::from(pool_slot))
            .copied()
            .flatten()
    }

    fn live_object_in_retail_pool_slot(&self, pool_slot: u8) -> Option<ObjectHandle> {
        self.retail_pool_slots_by_object
            .iter()
            .position(|candidate| *candidate == Some(pool_slot))
            .and_then(|index| u16::try_from(index).ok())
            .and_then(ObjectHandle::new)
            .filter(|handle| self.objects.contains_key(handle))
    }

    fn live_pool_slot_for_word(&self, value: u32, retained: Option<u8>) -> Option<u8> {
        retained.or_else(|| {
            CollisionObjectReference::from_word(value).and_then(|reference| {
                self.retail_pool_slots_by_object
                    .get(usize::from(reference.object().get()))
                    .copied()
                    .flatten()
            })
        })
    }

    /// Resolves a checked object token through its captured native pool
    /// identity. A dangling pool pointer must never fall back to the compact
    /// handle encoded in its raw word: that handle may already name an
    /// unrelated object in another physical slot.
    fn resolve_pool_backed_object_reference(
        &self,
        reference: CollisionObjectReference,
        pool_slot: Option<u8>,
    ) -> Result<ObjectHandle, VmError> {
        let object = match pool_slot {
            Some(pool_slot) => self
                .live_object_in_retail_pool_slot(pool_slot)
                .ok_or(VmError::UnknownObject(reference.object()))?,
            None => reference.object(),
        };
        self.object(object)?;
        Ok(object)
    }

    /// Resolves one process link through its native physical-pool identity.
    ///
    /// A provenance-bearing word may outlive its original logical object.
    /// While the slot is free, callers that require a live object see no
    /// target; after native LIFO reuse, the same pointer names the replacement
    /// occupant even when Rust assigned that object a different compact VM
    /// handle. Link-register reads have a separate retained-storage path for
    /// the free-slot interval.
    fn resolve_process_link(
        &self,
        handle: ObjectHandle,
        link: usize,
    ) -> Result<Option<ObjectHandle>, VmError> {
        let object = self.object(handle)?;
        let cached = *object
            .links
            .get(link)
            .ok_or(VmError::InvalidRegister(link))?;
        match object.register_pool_slot(link)? {
            Some(pool_slot) => Ok(self.live_object_in_retail_pool_slot(pool_slot)),
            None => Ok(cached),
        }
    }

    /// Clears the collider pair named by `object`, in retail source order.
    ///
    /// `LevelRestart` does not clear link six on every surviving object. It
    /// snapshots Crash's current collider, clears that object's collider, and
    /// then clears Crash's. Other asymmetric collider links remain live; the
    /// Doctor object relies on its retained Crash link to accept a mask after
    /// a death restart. A native collider pointer can still name initialized
    /// physical storage after its logical pool object has been reclaimed, so
    /// that retained slot must be cleared before the object's own link.
    pub(crate) fn clear_retail_collider_pair(
        &mut self,
        object: ObjectHandle,
    ) -> Result<(), VmError> {
        let (word, pool_slot, cached) = {
            let process = self.object(object)?;
            (
                process.register(PROCESS_LINK_COLLIDER)?,
                process.register_pool_slot(PROCESS_LINK_COLLIDER)?,
                process.links[PROCESS_LINK_COLLIDER],
            )
        };
        if word == 0 {
            return Ok(());
        }

        if let Some(pool_slot) = pool_slot {
            self.write_retail_pool_register_word(pool_slot, PROCESS_LINK_COLLIDER, 0, None)?;
        } else if let Some(collider) = cached {
            self.object_mut(collider)?
                .set_link(PROCESS_LINK_COLLIDER, None)?;
        }
        self.object_mut(object)?
            .set_link(PROCESS_LINK_COLLIDER, None)?;
        Ok(())
    }

    /// Reads the live occupant or the initialized static storage retained in
    /// one physical native object-pool slot.
    fn retail_pool_register_word(
        &self,
        pool_slot: u8,
        register: usize,
    ) -> Result<(u32, Option<u8>), VmError> {
        if let Some(handle) = self.live_object_in_retail_pool_slot(pool_slot) {
            return self.read_aliased_process_register_with_pool_slot(handle, register);
        }
        let storage = self
            .retired_retail_pool_registers
            .get(usize::from(pool_slot))
            .and_then(Option::as_ref)
            .ok_or(VmError::InvalidRetailPoolSlot(pool_slot))?;
        let value = storage
            .registers
            .get(register)
            .copied()
            .ok_or(VmError::InvalidRegister(register))?;
        let provenance = storage
            .register_pool_slots
            .get(register)
            .copied()
            .ok_or(VmError::InvalidRegister(register))?;
        Ok((value, self.live_pool_slot_for_word(value, provenance)))
    }

    /// Returns only process words with defined native contents for animation
    /// decoding. A reclaimed object initializes its complete register block;
    /// a never-used free slot contains only allocator-written cells and must
    /// not turn indeterminate malloc bytes into deterministic descriptors.
    fn retail_pool_animation_words(
        &self,
        pool_slot: u8,
        register: usize,
    ) -> Result<&[u32], VmError> {
        if let Some(handle) = self.live_object_in_retail_pool_slot(pool_slot) {
            return self
                .object(handle)?
                .registers
                .get(register..)
                .ok_or(VmError::InvalidRegister(register));
        }
        let storage = self
            .retired_retail_pool_registers
            .get(usize::from(pool_slot))
            .and_then(Option::as_ref)
            .ok_or(VmError::InvalidRetailPoolSlot(pool_slot))?;
        let registers = storage
            .registers
            .get(register..)
            .ok_or(VmError::InvalidRegister(register))?;
        let initialized = storage
            .initialized_registers
            .get(register..)
            .ok_or(VmError::InvalidRegister(register))?;
        let initialized_len = initialized.iter().take_while(|value| **value).count();
        registers
            .get(..initialized_len)
            .ok_or(VmError::InvalidRegister(register))
    }

    /// Writes one register through a native static-pool pointer. Free slots
    /// remain ordinary initialized storage, so authored SR/MOV operations can
    /// mutate a reclaimed object before the slot is reused. Mutating the
    /// three allocator-owned link words is rejected: the bounded arena owns
    /// allocation order and cannot safely reproduce a corrupted C free list.
    fn write_retail_pool_register_word(
        &mut self,
        pool_slot: u8,
        register: usize,
        value: u32,
        provenance: Option<u8>,
    ) -> Result<(), VmError> {
        let provenance =
            self.preflight_retail_pool_register_write(pool_slot, register, value, provenance)?;
        if let Some(handle) = self.live_object_in_retail_pool_slot(pool_slot) {
            return self.write_aliased_process_register_with_pool_slot(
                handle, register, value, provenance,
            );
        }

        let storage = self
            .retired_retail_pool_registers
            .get_mut(usize::from(pool_slot))
            .and_then(Option::as_mut)
            .ok_or(VmError::InvalidRetailPoolSlot(pool_slot))?;
        *storage
            .registers
            .get_mut(register)
            .ok_or(VmError::InvalidRegister(register))? = value;
        *storage
            .register_pool_slots
            .get_mut(register)
            .ok_or(VmError::InvalidRegister(register))? = provenance;
        *storage
            .initialized_registers
            .get_mut(register)
            .ok_or(VmError::InvalidRegister(register))? = true;

        let updated_translation = register
            .checked_sub(process_register::TRANSLATION_X)
            .filter(|axis| *axis < 3)
            .map(|_| {
                [
                    storage.registers[process_register::TRANSLATION_X] as i32,
                    storage.registers[process_register::TRANSLATION_Y] as i32,
                    storage.registers[process_register::TRANSLATION_Z] as i32,
                ]
            });
        if let Some(translation) = updated_translation {
            self.retired_retail_pool_translations[usize::from(pool_slot)] = Some(translation);
        }
        Ok(())
    }

    /// Validates a static-pool write without mutating either the live object
    /// or retained slot. Vector stores use this for their full span before
    /// committing the first word, so a later protected free-list link cannot
    /// leave a partial write behind.
    fn preflight_retail_pool_register_write(
        &self,
        pool_slot: u8,
        register: usize,
        value: u32,
        provenance: Option<u8>,
    ) -> Result<Option<u8>, VmError> {
        if usize::from(pool_slot) >= MAX_OBJECTS {
            return Err(VmError::InvalidRetailPoolSlot(pool_slot));
        }
        if let Some(provenance) = provenance
            && usize::from(provenance) >= MAX_OBJECTS
        {
            return Err(VmError::InvalidRetailPoolSlot(provenance));
        }
        let provenance = self
            .live_pool_slot_for_word(value, provenance)
            .filter(|_| CollisionObjectReference::from_word(value).is_some());
        if let Some(handle) = self.live_object_in_retail_pool_slot(pool_slot) {
            self.object(handle)?.register(register)?;
            if matches!(
                register,
                process_register::PROGRAM_COUNTER | process_register::TRANSITION_POINTER
            ) && value != 0
            {
                self.object(handle)?.checked_code_address(value)?;
            }
            return Ok(provenance);
        }

        let storage = self
            .retired_retail_pool_registers
            .get(usize::from(pool_slot))
            .and_then(Option::as_ref)
            .ok_or(VmError::InvalidRetailPoolSlot(pool_slot))?;
        let current_value = storage
            .registers
            .get(register)
            .copied()
            .ok_or(VmError::InvalidRegister(register))?;
        let current_provenance = storage
            .register_pool_slots
            .get(register)
            .copied()
            .ok_or(VmError::InvalidRegister(register))?;
        storage
            .initialized_registers
            .get(register)
            .ok_or(VmError::InvalidRegister(register))?;
        if self.retail_free_pool_slots.contains(&pool_slot)
            && matches!(
                register,
                PROCESS_LINK_PARENT | PROCESS_LINK_SIBLING | PROCESS_LINK_CHILDREN
            )
            && (current_value, current_provenance) != (value, provenance)
        {
            return Err(VmError::RetailFreePoolLinkMutation {
                slot: pool_slot,
                register,
            });
        }
        Ok(provenance)
    }

    /// Removes one checked VM object and nulls every inbound process link.
    /// Active synchronous event/interrupt frames cannot be removed midway;
    /// this keeps their stack-scoped argument tokens and return addresses from
    /// becoming detached from an owning object.
    pub fn remove_object(&mut self, handle: ObjectHandle) -> Result<VmObject, VmError> {
        let object = self.object(handle)?;
        if object.call_stack.iter().any(|frame| {
            matches!(
                frame.behavior,
                ReturnBehavior::EventService { .. } | ReturnBehavior::Interrupt { .. }
            )
        }) {
            return Err(VmError::ActiveEventInvocation(handle));
        }
        self.remove_object_unchecked(handle, None)
    }

    /// Removes a checked VM object while retaining its initialized transform
    /// in the matching physical native pool slot.
    pub(crate) fn remove_object_from_retail_pool_slot(
        &mut self,
        handle: ObjectHandle,
        pool_slot: u8,
    ) -> Result<VmObject, VmError> {
        if usize::from(pool_slot) >= MAX_OBJECTS {
            return Err(VmError::InvalidRetailPoolSlot(pool_slot));
        }
        let object = self.object(handle)?;
        if object.call_stack.iter().any(|frame| {
            matches!(
                frame.behavior,
                ReturnBehavior::EventService { .. } | ReturnBehavior::Interrupt { .. }
            )
        }) {
            return Err(VmError::ActiveEventInvocation(handle));
        }
        self.remove_object_unchecked(handle, Some(pool_slot))
    }

    /// Removes an object during a synchronous host effect, including when that
    /// object owns the active event/interrupt frame which emitted the effect.
    ///
    /// This is intentionally narrower than [`Self::remove_object`]. The caller
    /// must return control directly to a host-aware runner, which observes the
    /// missing handle and yields [`HaltReason::ObjectTerminated`] instead of
    /// attempting to unwind or execute the reclaimed frame.
    pub fn remove_object_for_host_termination(
        &mut self,
        handle: ObjectHandle,
    ) -> Result<VmObject, VmError> {
        self.object(handle)?;
        self.remove_object_unchecked(handle, None)
    }

    /// Host-termination form that also preserves physical pool-slot storage.
    pub(crate) fn remove_object_for_host_termination_from_retail_pool_slot(
        &mut self,
        handle: ObjectHandle,
        pool_slot: u8,
    ) -> Result<VmObject, VmError> {
        if usize::from(pool_slot) >= MAX_OBJECTS {
            return Err(VmError::InvalidRetailPoolSlot(pool_slot));
        }
        self.object(handle)?;
        self.remove_object_unchecked(handle, Some(pool_slot))
    }

    fn remove_object_unchecked(
        &mut self,
        handle: ObjectHandle,
        retail_pool_slot: Option<u8>,
    ) -> Result<VmObject, VmError> {
        if let Some(requested) = retail_pool_slot {
            let bound = self.retail_pool_slots_by_object[usize::from(handle.get())];
            if bound != Some(requested) {
                return Err(VmError::RetailPoolSlotMismatch {
                    object: handle,
                    bound,
                    requested,
                });
            }
        }
        let mut removed = self
            .objects
            .remove(&handle)
            .ok_or(VmError::UnknownObject(handle))?;
        if let Some(pool_slot) = retail_pool_slot {
            // Capture every live pointer before clearing this compact
            // handle's physical identity. This includes pointers already
            // stored in other objects and nested pointer words retained in
            // the object that is about to become static free-slot storage.
            let mut live_pool_slots = self.retail_pool_slots_by_object;
            live_pool_slots[usize::from(handle.get())] = Some(pool_slot);
            removed.capture_live_retail_pool_slots(&live_pool_slots);
            for object in self.objects.values_mut() {
                object.capture_live_retail_pool_slots(&live_pool_slots);
            }

            if usize::from(pool_slot) < OBJECT_POOL_CAPACITY {
                debug_assert!(
                    !self.retail_free_pool_slots.contains(&pool_slot),
                    "a bound ordinary pool slot cannot already be free"
                );
                let previous_head = self.retail_free_pool_slots.first().copied();
                removed.set_register_with_pool_slot(
                    PROCESS_LINK_PARENT,
                    RETAIL_FREE_LIST_ROOT_REFERENCE,
                    None,
                )?;
                removed.set_register_with_pool_slot(
                    PROCESS_LINK_SIBLING,
                    previous_head.map_or(0, retail_pool_slot_reference_word),
                    previous_head,
                )?;
                // Recursive kill has already removed every child from this
                // object's intrusive list before the object itself is linked
                // beneath `free_objects`.
                removed.set_register_with_pool_slot(PROCESS_LINK_CHILDREN, 0, None)?;
                self.retail_free_pool_slots.insert(0, pool_slot);
            } else {
                // The separately allocated player/main object is temporarily
                // passed through AddChild in native kill, then explicitly
                // detached without changing the ordinary free-list head.
                for link in [
                    PROCESS_LINK_PARENT,
                    PROCESS_LINK_SIBLING,
                    PROCESS_LINK_CHILDREN,
                ] {
                    removed.set_register_with_pool_slot(link, 0, None)?;
                }
            }
        }
        let retired_translation = removed
            .retail_transform()
            .ok()
            .map(|transform| transform.translation);
        self.retired_retail_translations[usize::from(handle.get())] = retired_translation;
        if let Some(pool_slot) = retail_pool_slot {
            self.retired_retail_pool_translations[usize::from(pool_slot)] = retired_translation;
            self.retired_retail_pool_registers[usize::from(pool_slot)] =
                Some(RetiredRetailProcessStorage {
                    registers: removed.registers.clone().into_boxed_slice(),
                    register_pool_slots: removed.register_pool_slots.clone().into_boxed_slice(),
                    initialized_registers: vec![true; REGISTER_COUNT].into_boxed_slice(),
                });
        }
        self.retail_pool_slots_by_object[usize::from(handle.get())] = None;
        self.advance_object_incarnation(handle);
        // A request currently inside its host callback must survive until the
        // runner observes the incarnation change and abandons it. An
        // unserviced request can never be delivered after its sender dies.
        self.pending_send_events
            .retain(|pending| pending.request.sender != handle || pending.servicing);
        if retail_pool_slot.is_none() {
            // Synthetic/non-retail machines have no physical storage
            // identity through which a stale link could be represented.
            // Preserve their checked teardown contract.
            for object in self.objects.values_mut() {
                for index in 0..object.links.len() {
                    if object.links[index] == Some(handle) {
                        object.set_link(index, None)?;
                    }
                }
            }
        }
        Ok(removed)
    }

    fn advance_object_incarnation(&mut self, handle: ObjectHandle) {
        let incarnation = &mut self.object_incarnations[usize::from(handle.get())];
        *incarnation = incarnation.wrapping_add(1);
    }

    fn object_incarnation(&self, handle: ObjectHandle) -> Result<u64, VmError> {
        self.object(handle)?;
        Ok(self.object_incarnations[usize::from(handle.get())])
    }

    fn incarnation_is_live(&self, handle: ObjectHandle, incarnation: u64) -> bool {
        self.objects.contains_key(&handle)
            && self.object_incarnations[usize::from(handle.get())] == incarnation
    }

    /// Enters one synchronous event-argument scope. `None` is the native null
    /// `argv` pointer, while `Some(&[])` deliberately creates a non-null owned
    /// reference with zero readable elements.
    pub fn enter_event_arguments_scope(
        &mut self,
        arguments: Option<&[u32]>,
    ) -> Result<Option<EventArgumentsReference>, VmError> {
        self.enter_event_arguments_scope_inner(arguments, None)
    }

    fn enter_event_arguments_scope_with_pool_slots(
        &mut self,
        arguments: Option<&[u32]>,
        argument_pool_slots: Option<&[Option<u8>]>,
    ) -> Result<Option<EventArgumentsReference>, VmError> {
        self.enter_event_arguments_scope_inner(arguments, argument_pool_slots)
    }

    fn enter_event_arguments_scope_inner(
        &mut self,
        arguments: Option<&[u32]>,
        argument_pool_slots: Option<&[Option<u8>]>,
    ) -> Result<Option<EventArgumentsReference>, VmError> {
        let argument_count = arguments.map_or(0, <[u32]>::len);
        validate_argument_pool_slots(argument_count, argument_pool_slots)?;
        let Some(arguments) = arguments else {
            return Ok(None);
        };
        if arguments.len() > MAX_EVENT_ARGUMENTS {
            return Err(VmError::EventArgumentsTooLong(arguments.len()));
        }
        if self.event_argument_scopes.len() == MAX_EVENT_ARGUMENT_SCOPES {
            return Err(VmError::EventArgumentReferenceCapacityExceeded);
        }
        let reference = EventArgumentsReference::checked(self.next_event_argument_generation)?;
        self.next_event_argument_generation = self
            .next_event_argument_generation
            .checked_add(1)
            .ok_or(VmError::EventArgumentReferenceCapacityExceeded)?;
        let mut owned = [0; MAX_EVENT_ARGUMENTS];
        owned[..arguments.len()].copy_from_slice(arguments);
        let mut pool_slots = [None; MAX_EVENT_ARGUMENTS];
        if let Some(argument_pool_slots) = argument_pool_slots {
            pool_slots[..arguments.len()].copy_from_slice(argument_pool_slots);
        }
        self.event_argument_scopes.push(EventArgumentsScope {
            reference,
            arguments: owned,
            pool_slots,
            len: arguments.len() as u8,
        });
        Ok(Some(reference))
    }

    /// Leaves the most recently entered event-argument scope. Requiring LIFO
    /// release mirrors the synchronous interpreter stack and prevents a token
    /// from outliving the frame whose `fp[-1]` contains it.
    pub fn leave_event_arguments_scope(
        &mut self,
        reference: Option<EventArgumentsReference>,
    ) -> Result<(), VmError> {
        let Some(reference) = reference else {
            return Ok(());
        };
        if self
            .event_argument_scopes
            .last()
            .is_none_or(|scope| scope.reference != reference)
        {
            return Err(VmError::EventArgumentScopeMismatch(reference.to_word()));
        }
        self.event_argument_scopes.pop();
        Ok(())
    }

    #[cfg(test)]
    fn event_argument(
        &self,
        reference: EventArgumentsReference,
        index: i8,
    ) -> Result<u32, VmError> {
        self.event_argument_with_pool_slot(reference, index)
            .map(|(value, _)| value)
    }

    fn event_argument_with_pool_slot(
        &self,
        reference: EventArgumentsReference,
        index: i8,
    ) -> Result<(u32, Option<u8>), VmError> {
        let scope = self
            .event_argument_scopes
            .iter()
            .find(|scope| scope.reference == reference)
            .ok_or(VmError::InvalidEventArgumentsReference(reference.to_word()))?;
        let index = usize::try_from(index).map_err(|_| VmError::EventArgumentOutOfBounds {
            reference: reference.to_word(),
            index,
            len: scope.len,
        })?;
        let value = scope
            .arguments
            .get(index)
            .copied()
            .filter(|_| index < usize::from(scope.len))
            .ok_or(VmError::EventArgumentOutOfBounds {
                reference: reference.to_word(),
                index: index as i8,
                len: scope.len,
            })?;
        let pool_slot = scope
            .pool_slots
            .get(index)
            .copied()
            .flatten()
            .filter(|_| CollisionObjectReference::from_word(value).is_some());
        Ok((value, pool_slot))
    }

    fn begin_synchronous_event_frame(
        &mut self,
        handle: ObjectHandle,
        target: CodeAddress,
        arguments: &[u32],
        argument_pool_slots: Option<&[Option<u8>]>,
        behavior: ReturnBehavior,
    ) -> Result<usize, VmError> {
        validate_argument_pool_slots(arguments.len(), argument_pool_slots)?;
        let object = self.object(handle)?;
        let code_len = match target.segment {
            CodeSegment::External => object.code.len(),
            CodeSegment::Global => object.global_code.len(),
        };
        if target.pc >= code_len {
            return Err(VmError::InvalidJump {
                object: handle,
                target: target.pc as i64,
            });
        }
        if object.call_stack.len() == MAX_CALL_DEPTH {
            return Err(VmError::CallStackOverflow(handle));
        }
        let required = arguments
            .len()
            .checked_add(3)
            .ok_or(VmError::StackOverflow(handle))?;
        if object.stack.len() + required > MAX_STACK_WORDS {
            return Err(VmError::StackOverflow(handle));
        }

        let stack_origin = object.initial_stack_pointer as usize;
        let argument_base = object.stack.len();
        let frame_base = stack_origin
            .checked_add(argument_base)
            .and_then(|base| base.checked_add(arguments.len()))
            .ok_or(VmError::StackOverflow(handle))?;
        if frame_base + 3 > REGISTER_COUNT {
            return Err(VmError::StackOverflow(handle));
        }
        let return_address = object.code_address();
        let return_halted = object.halted;
        let previous_frame_base = object.frame_base;
        let prior_rsp_bytes = stack_origin
            .checked_add(argument_base)
            .and_then(|word| word.checked_mul(4))
            .and_then(|bytes| u16::try_from(bytes).ok())
            .ok_or(VmError::StackOverflow(handle))?;
        let prior_rfp_bytes = previous_frame_base
            .checked_mul(4)
            .and_then(|bytes| u16::try_from(bytes).ok())
            .ok_or(VmError::StackOverflow(handle))?;
        let frame_depth = object.call_stack.len();

        for (index, argument) in arguments.iter().copied().enumerate() {
            let pool_slot = argument_pool_slots
                .and_then(|pool_slots| pool_slots.get(index))
                .copied()
                .flatten();
            self.push_with_pool_slot(handle, argument, pool_slot)?;
        }
        {
            let object = self.object_mut(handle)?;
            object.frame_base = frame_base;
            object.call_stack.push(CallFrame {
                return_address,
                return_halted,
                argument_base,
                previous_frame_base,
                behavior,
            });
        }
        self.push(handle, INITIAL_FRAME_FLAGS)?;
        self.push(handle, return_address.to_word())?;
        self.push(
            handle,
            (u32::from(prior_rfp_bytes) << 16) | u32::from(prior_rsp_bytes),
        )?;
        let object = self.object_mut(handle)?;
        object.code_segment = target.segment;
        object.pc = target.pc;
        object.halted = false;
        Ok(frame_depth)
    }

    fn unwind_synchronous_event_frame(
        &mut self,
        handle: ObjectHandle,
        frame_depth: usize,
    ) -> Result<(), VmError> {
        let Some(frame) = self.object(handle)?.call_stack.get(frame_depth).copied() else {
            return Ok(());
        };
        let (ReturnBehavior::EventService {
            previous_animation_wait,
            ..
        }
        | ReturnBehavior::Interrupt {
            previous_animation_wait,
        }) = frame.behavior
        else {
            return Err(VmError::UnexpectedEventServiceHalt {
                object: handle,
                reason: HaltReason::Halted,
            });
        };
        let object = self.object_mut(handle)?;
        object.call_stack.truncate(frame_depth);
        object.stack.truncate(frame.argument_base);
        object.frame_base = frame.previous_frame_base;
        object.code_segment = frame.return_address.segment;
        object.pc = frame.return_address.pc;
        object.halted = frame.return_halted;
        object.animation_wait = previous_animation_wait;
        Ok(())
    }

    fn run_synchronous_event_code_mode<F>(
        &mut self,
        handle: ObjectHandle,
        host: &mut F,
        service_audio: bool,
        return_link_halt: Option<HaltReason>,
    ) -> Result<Execution, VmError>
    where
        F: FnMut(&mut Self, VmHostRequest) -> Result<(), VmError>,
    {
        let incarnation = self.object_incarnation(handle)?;
        if self.level_restart_requested {
            return Ok(Execution {
                reason: HaltReason::HostEffect,
                steps: 0,
            });
        }
        match self.service_pending_send_event(handle, host)? {
            SendEventService::Halt(reason) => {
                return Ok(Execution { reason, steps: 0 });
            }
            SendEventService::Continue | SendEventService::None => {}
        }
        if let Some(request) = self.pending_audio_host_request {
            if !service_audio {
                return Ok(Execution {
                    reason: HaltReason::HostEffect,
                    steps: 0,
                });
            }
            host(self, VmHostRequest::Audio(request))?;
            if !self.incarnation_is_live(handle, incarnation) {
                return Ok(Execution {
                    reason: HaltReason::ObjectTerminated,
                    steps: 0,
                });
            }
            if self.pending_audio_host_request.is_some() {
                return Ok(Execution {
                    reason: HaltReason::HostEffect,
                    steps: 0,
                });
            }
        }
        if let Some(request) = self.pending_card_host_request {
            if !service_audio {
                return Ok(Execution {
                    reason: HaltReason::HostEffect,
                    steps: 0,
                });
            }
            host(self, VmHostRequest::Card(request))?;
            if !self.incarnation_is_live(handle, incarnation) {
                return Ok(Execution {
                    reason: HaltReason::ObjectTerminated,
                    steps: 0,
                });
            }
            if self.pending_card_host_request.is_some() {
                return Ok(Execution {
                    reason: HaltReason::HostEffect,
                    steps: 0,
                });
            }
        }
        let mut condition = false;
        for steps in 0..MAX_EVENT_SERVICE_INSTRUCTIONS {
            match self.step(handle, &mut condition, return_link_halt)? {
                Some(HaltReason::AnimationChanged { .. }) if service_audio => {
                    let effect = self
                        .effects
                        .last()
                        .cloned()
                        .ok_or(VmError::MissingHostEffect)?;
                    if !matches!(effect, VmEffect::AnimationFrameChanged { .. }) {
                        return Err(VmError::MissingHostEffect);
                    }
                    host(self, VmHostRequest::Effect(effect))?;
                    if !self.incarnation_is_live(handle, incarnation) {
                        return Ok(Execution {
                            reason: HaltReason::ObjectTerminated,
                            steps: steps + 1,
                        });
                    }
                    if self.level_restart_requested {
                        return Ok(Execution {
                            reason: HaltReason::HostEffect,
                            steps: steps + 1,
                        });
                    }
                }
                None | Some(HaltReason::AnimationChanged { .. }) => {}
                Some(HaltReason::HostEffect) if self.pending_send_event_index(handle).is_some() => {
                    match self.service_pending_send_event(handle, host)? {
                        SendEventService::Halt(reason) => {
                            return Ok(Execution {
                                reason,
                                steps: steps + 1,
                            });
                        }
                        SendEventService::Continue => {
                            if self.level_restart_requested {
                                return Ok(Execution {
                                    reason: HaltReason::HostEffect,
                                    steps: steps + 1,
                                });
                            }
                        }
                        SendEventService::None => return Err(VmError::MissingHostEffect),
                    }
                }
                Some(HaltReason::HostEffect)
                    if self.pending_audio_host_request.is_some() && service_audio =>
                {
                    let request = self
                        .pending_audio_host_request
                        .expect("matched pending audio request");
                    host(self, VmHostRequest::Audio(request))?;
                    if !self.incarnation_is_live(handle, incarnation) {
                        return Ok(Execution {
                            reason: HaltReason::ObjectTerminated,
                            steps: steps + 1,
                        });
                    }
                    if self.pending_audio_host_request.is_some() {
                        return Ok(Execution {
                            reason: HaltReason::HostEffect,
                            steps: steps + 1,
                        });
                    }
                }
                Some(HaltReason::HostEffect)
                    if self.pending_card_host_request.is_some() && service_audio =>
                {
                    let request = self
                        .pending_card_host_request
                        .expect("matched pending card request");
                    host(self, VmHostRequest::Card(request))?;
                    if !self.incarnation_is_live(handle, incarnation) {
                        return Ok(Execution {
                            reason: HaltReason::ObjectTerminated,
                            steps: steps + 1,
                        });
                    }
                    if self.pending_card_host_request.is_some() {
                        return Ok(Execution {
                            reason: HaltReason::HostEffect,
                            steps: steps + 1,
                        });
                    }
                }
                Some(HaltReason::HostEffect) if service_audio => {
                    let effect = self
                        .effects
                        .last()
                        .cloned()
                        .ok_or(VmError::MissingHostEffect)?;
                    if !matches!(
                        effect,
                        VmEffect::SpawnChildren { .. }
                            | VmEffect::Event { .. }
                            | VmEffect::SetObjectZoneToTransitionTarget { .. }
                            | VmEffect::TerminateCurrentZoneNeighbors { .. }
                            | VmEffect::SetLinkZoneFromPoint { .. }
                            | VmEffect::ReparentToRoot { .. }
                            | VmEffect::FindSpawnedObject { .. }
                            | VmEffect::SpawnFlagsChanged { .. }
                            | VmEffect::TransformModelVertex { .. }
                            | VmEffect::SaveState(_)
                            | VmEffect::LoadState { .. }
                            | VmEffect::ResetLevelGlobals { .. }
                            | VmEffect::Paging { .. }
                    ) {
                        return Err(VmError::MissingHostEffect);
                    }
                    host(self, VmHostRequest::Effect(effect))?;
                    if !self.incarnation_is_live(handle, incarnation) {
                        return Ok(Execution {
                            reason: HaltReason::ObjectTerminated,
                            steps: steps + 1,
                        });
                    }
                    if self.level_restart_requested {
                        return Ok(Execution {
                            reason: HaltReason::HostEffect,
                            steps: steps + 1,
                        });
                    }
                }
                Some(reason) => {
                    return Ok(Execution {
                        reason,
                        steps: steps + 1,
                    });
                }
            }
        }
        Ok(Execution {
            reason: HaltReason::BudgetExhausted,
            steps: MAX_EVENT_SERVICE_INSTRUCTIONS,
        })
    }

    fn set_event_acknowledgement(
        &mut self,
        sender: Option<ObjectHandle>,
        acknowledged: bool,
    ) -> Result<(), VmError> {
        if let Some(sender) = sender {
            self.object_mut(sender)?
                .set_register(process_register::MISC_VALUE, u32::from(acknowledged))?;
        }
        Ok(())
    }

    fn invoke_event_service<F>(
        &mut self,
        recipient: ObjectHandle,
        event: u32,
        arguments: EventArgumentSlices<'_>,
        event_pc: usize,
        host: &mut F,
        service_audio: bool,
    ) -> Result<Execution, VmError>
    where
        F: FnMut(&mut Self, VmHostRequest) -> Result<(), VmError>,
    {
        let reference = self.enter_event_arguments_scope_with_pool_slots(
            arguments.arguments,
            arguments.pool_slots,
        )?;
        let argv_word = reference.map_or(0, EventArgumentsReference::to_word);
        let previous_animation_wait = self.object(recipient)?.animation_wait;
        let behavior = ReturnBehavior::EventService {
            condition: false,
            return_event: false,
            guard: false,
            previous_animation_wait,
        };
        let frame_depth = match self.begin_synchronous_event_frame(
            recipient,
            CodeAddress {
                segment: CodeSegment::External,
                pc: event_pc,
            },
            &[event, argv_word],
            None,
            behavior,
        ) {
            Ok(depth) => depth,
            Err(error) => {
                self.leave_event_arguments_scope(reference)?;
                return Err(error);
            }
        };

        let execution = self.run_synchronous_event_code_mode(recipient, host, service_audio, None);
        let preserve_for_rebind = matches!(
            execution,
            Ok(Execution {
                reason: HaltReason::StateChanged(_),
                ..
            })
        );
        if !preserve_for_rebind
            && (execution.is_err()
                || !matches!(
                    execution,
                    Ok(Execution {
                        reason: HaltReason::EventServiceReturned { .. }
                            | HaltReason::EventServiceInvalidReturn
                            | HaltReason::ObjectTerminated,
                        ..
                    })
                ))
        {
            self.unwind_synchronous_event_frame(recipient, frame_depth)?;
        }
        self.leave_event_arguments_scope(reference)?;

        let execution = execution?;
        match execution.reason {
            HaltReason::EventServiceReturned { .. }
            | HaltReason::EventServiceInvalidReturn
            | HaltReason::StateChanged(_)
            | HaltReason::ObjectTerminated => Ok(execution),
            HaltReason::HostEffect if self.level_restart_requested => Ok(execution),
            HaltReason::BudgetExhausted => Err(VmError::EventServiceBudgetExhausted(recipient)),
            reason => Err(VmError::UnexpectedEventServiceHalt {
                object: recipient,
                reason,
            }),
        }
    }

    fn invoke_event_interrupt<F>(
        &mut self,
        recipient: ObjectHandle,
        offset: usize,
        arguments: &[u32],
        argument_pool_slots: Option<&[Option<u8>]>,
        host: &mut F,
        service_audio: bool,
    ) -> Result<Execution, VmError>
    where
        F: FnMut(&mut Self, VmHostRequest) -> Result<(), VmError>,
    {
        let previous_animation_wait = self.object(recipient)?.animation_wait;
        let frame_depth = self.begin_synchronous_event_frame(
            recipient,
            CodeAddress {
                segment: CodeSegment::Global,
                pc: offset,
            },
            arguments,
            argument_pool_slots,
            ReturnBehavior::Interrupt {
                previous_animation_wait,
            },
        )?;
        let execution = self.run_synchronous_event_code_mode(
            recipient,
            host,
            service_audio,
            Some(HaltReason::InterruptCompleted),
        );
        let preserve_for_rebind = matches!(
            execution,
            Ok(Execution {
                reason: HaltReason::StateChanged(_),
                ..
            })
        );
        if !preserve_for_rebind
            && (execution.is_err()
                || !matches!(
                    execution,
                    Ok(Execution {
                        reason: HaltReason::InterruptCompleted | HaltReason::ObjectTerminated,
                        ..
                    })
                ))
        {
            self.unwind_synchronous_event_frame(recipient, frame_depth)?;
        }
        let execution = execution?;
        match execution.reason {
            HaltReason::InterruptCompleted
            | HaltReason::StateChanged(_)
            | HaltReason::ObjectTerminated => Ok(execution),
            HaltReason::HostEffect if self.level_restart_requested => Ok(execution),
            HaltReason::BudgetExhausted => Err(VmError::InterruptBudgetExhausted(recipient)),
            reason => Err(VmError::UnexpectedInterruptHalt {
                object: recipient,
                reason,
            }),
        }
    }

    fn request_event_state_change(
        &mut self,
        sender: Option<ObjectHandle>,
        recipient: ObjectHandle,
        event: u32,
        state: u16,
        arguments: (&[u32], &[Option<u8>]),
        acknowledged: bool,
    ) -> Result<EventDispatchOutcome, VmError> {
        let (arguments, argument_pool_slots) = arguments;
        validate_argument_pool_slots(arguments.len(), Some(argument_pool_slots))?;
        if self.object(recipient)?.event_state_blocked(state, event)? {
            self.set_event_acknowledgement(sender, false)?;
            return Ok(EventDispatchOutcome {
                acknowledged: false,
                state_change: None,
            });
        }
        {
            let object = self.object_mut(recipient)?;
            object.set_register(process_register::EVENT, event)?;
            if matches!(event, EVENT_CLEAR_GUARD_STATUS | SQUASH_EVENT) {
                let status = object.register(process_register::STATUS_A)?;
                object
                    .set_register(process_register::STATUS_A, status | STATUS_A_EVENT_SQUASHED)?;
            }
            object.state = state;
        }
        Ok(EventDispatchOutcome {
            acknowledged,
            state_change: Some(EventStateChange {
                recipient,
                state,
                event,
                arguments: arguments.to_vec(),
                argument_pool_slots: argument_pool_slots.to_vec(),
            }),
        })
    }

    /// Classifies one misc-13 candidate without consulting host tree state.
    ///
    /// The runtime calls this in root-four preorder and applies the strict
    /// distance comparison before invoking a returned status interrupt. That
    /// ordering is observable: an equally distant candidate must neither win
    /// the tie nor run its interrupt.
    pub fn classify_nearest_object_candidate(
        &self,
        origin: ObjectHandle,
        candidate: ObjectHandle,
        categories: u8,
        event: u32,
    ) -> Result<NearestObjectCandidate, VmError> {
        let origin_object = self.object(origin)?;
        let candidate_object = self.object(candidate)?;
        if candidate == origin {
            return Ok(NearestObjectCandidate::Ineligible);
        }

        let Some(identity) = candidate_object.program_identity() else {
            // Synthetic host objects have no serialized GOOL header and
            // therefore no native category bit to test.
            return Ok(NearestObjectCandidate::Ineligible);
        };
        let category = identity.category() >> 8;
        let category_bit = 1_u32.checked_shl(category).unwrap_or(0);
        if u32::from(categories) & category_bit == 0 {
            return Ok(NearestObjectCandidate::Ineligible);
        }

        let origin_translation = origin_object.process_vector(0)?;
        let candidate_translation = candidate_object.process_vector(0)?;
        let distance = approximate_distance(
            Vec3 {
                x: candidate_translation[0],
                y: candidate_translation[1],
                z: candidate_translation[2],
            },
            Vec3 {
                x: origin_translation[0],
                y: origin_translation[1],
                z: origin_translation[2],
            },
        );

        if event == 0xff {
            return Ok(NearestObjectCandidate::Eligible { distance });
        }
        let state = candidate_object
            .event_map
            .get((event >> 8) as usize)
            .copied()
            .unwrap_or(EVENT_MAP_NULL_STATE);
        if state == EVENT_MAP_NULL_STATE {
            let invincibility = candidate_object.register(process_register::INVINCIBILITY_STATE)?;
            let eligible = match event {
                HIT_EVENT => {
                    !matches!(invincibility, 2..=4) && candidate_object.status_c() & 2 == 0
                }
                HIT_INVINCIBLE_EVENT => candidate_object.state_flags() & 0x800 == 0,
                WIN_BOSS_EVENT => true,
                _ => false,
            };
            return Ok(if eligible {
                NearestObjectCandidate::Eligible { distance }
            } else {
                NearestObjectCandidate::Ineligible
            });
        }
        if state & 0x8000 != 0 {
            return Ok(if event == STATUS_EVENT {
                NearestObjectCandidate::StatusInterrupt {
                    distance,
                    offset: usize::from(state & 0x7fff),
                }
            } else {
                NearestObjectCandidate::Eligible { distance }
            });
        }
        Ok(if candidate_object.state_link_blocked(state)? {
            NearestObjectCandidate::Ineligible
        } else {
            NearestObjectCandidate::Eligible { distance }
        })
    }

    /// Executes the high-bit status-map branch used only by misc 13.
    ///
    /// Unlike ordinary event delivery, this bypasses the event-service
    /// routine, leaves the process event word untouched, installs the query
    /// origin in link seven, and supplies the single native `0x100` argument.
    /// A returned state change must be rebound synchronously before reading
    /// the candidate's ACK register.
    pub fn run_nearest_status_interrupt_with_host_requests<F>(
        &mut self,
        origin: ObjectHandle,
        candidate: ObjectHandle,
        offset: usize,
        mut host: F,
    ) -> Result<Option<EventStateChange>, VmError>
    where
        F: FnMut(&mut Self, VmHostRequest) -> Result<(), VmError>,
    {
        self.object(origin)?;
        self.object(candidate)?;
        self.object_mut(candidate)?.set_link(7, Some(origin))?;
        let execution =
            self.invoke_event_interrupt(candidate, offset, &[0x100], None, &mut host, true)?;
        match execution.reason {
            HaltReason::InterruptCompleted | HaltReason::ObjectTerminated => Ok(None),
            HaltReason::StateChanged(state) => Ok(Some(EventStateChange {
                recipient: candidate,
                state,
                event: STATUS_EVENT,
                arguments: Vec::new(),
                argument_pool_slots: Vec::new(),
            })),
            HaltReason::HostEffect if self.level_restart_requested => Ok(None),
            _ => unreachable!("invoke_event_interrupt validates its halt reason"),
        }
    }

    /// Delivers one event synchronously through the recipient's current ESR,
    /// exact event-map fallback, and high-bit shared-code interrupt path.
    ///
    /// `None` preserves a native null `argv`; `Some(&[])` is a distinct owned,
    /// non-null empty argument list. At most [`MAX_EVENT_ARGUMENTS`] words are
    /// accepted. A returned [`EventStateChange`] must be rebound immediately
    /// by the stream-owning runtime before either object executes again.
    pub fn send_event(
        &mut self,
        sender: Option<ObjectHandle>,
        recipient: Option<ObjectHandle>,
        event: u32,
        arguments: Option<&[u32]>,
    ) -> Result<EventDispatchOutcome, VmError> {
        let mut host = |_machine: &mut Self, _request: VmHostRequest| -> Result<(), VmError> {
            unreachable!("legacy event delivery suspends before typed audio")
        };
        self.send_event_mode(
            sender,
            recipient,
            event,
            EventArgumentSlices::new(arguments, None),
            &mut host,
            false,
        )
    }

    /// Audio-aware event delivery used by the stream-owning runtime.
    ///
    /// Event-service and high-bit interrupt code remain inside the same
    /// bounded synchronous frame while [`VmHostRequest::Audio`] is completed.
    /// The legacy [`Self::send_event`] behavior remains unchanged for callers
    /// that explicitly own the typed `HostEffect` handshake.
    pub fn send_event_with_host_requests<F>(
        &mut self,
        sender: Option<ObjectHandle>,
        recipient: Option<ObjectHandle>,
        event: u32,
        arguments: Option<&[u32]>,
        mut host: F,
    ) -> Result<EventDispatchOutcome, VmError>
    where
        F: FnMut(&mut Self, VmHostRequest) -> Result<(), VmError>,
    {
        self.send_event_mode(
            sender,
            recipient,
            event,
            EventArgumentSlices::new(arguments, None),
            &mut host,
            true,
        )
    }

    pub(crate) fn send_event_with_host_requests_and_pool_slots<F>(
        &mut self,
        sender: Option<ObjectHandle>,
        recipient: Option<ObjectHandle>,
        event: u32,
        arguments: Option<&[u32]>,
        argument_pool_slots: Option<&[Option<u8>]>,
        mut host: F,
    ) -> Result<EventDispatchOutcome, VmError>
    where
        F: FnMut(&mut Self, VmHostRequest) -> Result<(), VmError>,
    {
        self.send_event_mode(
            sender,
            recipient,
            event,
            EventArgumentSlices::new(arguments, argument_pool_slots),
            &mut host,
            true,
        )
    }

    fn send_event_mode<F>(
        &mut self,
        sender: Option<ObjectHandle>,
        recipient: Option<ObjectHandle>,
        event: u32,
        arguments: EventArgumentSlices<'_>,
        host: &mut F,
        service_audio: bool,
    ) -> Result<EventDispatchOutcome, VmError>
    where
        F: FnMut(&mut Self, VmHostRequest) -> Result<(), VmError>,
    {
        let argument_count = arguments.arguments.map_or(0, <[u32]>::len);
        if argument_count > MAX_EVENT_ARGUMENTS {
            return Err(VmError::EventArgumentsTooLong(argument_count));
        }
        validate_argument_pool_slots(argument_count, arguments.pool_slots)?;
        if let Some(sender) = sender {
            self.object(sender)?;
        }
        let Some(recipient) = recipient else {
            self.set_event_acknowledgement(sender, false)?;
            return Ok(EventDispatchOutcome {
                acknowledged: false,
                state_change: None,
            });
        };
        self.object(recipient)?;
        self.set_event_acknowledgement(sender, true)?;
        self.object_mut(recipient)?.set_link(7, sender)?;
        let argument_words = arguments.arguments.unwrap_or(&[]);
        // Public/runtime callers supply raw native-shaped words rather than a
        // pre-captured sidecar. Snapshot every currently live pool pointer at
        // this ownership boundary; an explicit sidecar instead represents
        // exact earlier provenance and must never be re-derived after ABA.
        let mut inferred_pool_slots = [None; MAX_EVENT_ARGUMENTS];
        let argument_pool_slots = if let Some(pool_slots) = arguments.pool_slots {
            pool_slots
        } else {
            for (pool_slot, argument) in inferred_pool_slots[..argument_count]
                .iter_mut()
                .zip(argument_words)
            {
                *pool_slot = self.live_pool_slot_for_word(*argument, None);
            }
            &inferred_pool_slots[..argument_count]
        };

        if let Some(event_pc) = self.object(recipient)?.event_pc {
            let execution = self.invoke_event_service(
                recipient,
                event,
                EventArgumentSlices::new(arguments.arguments, Some(argument_pool_slots)),
                event_pc,
                host,
                service_audio,
            )?;
            match execution.reason {
                HaltReason::EventServiceReturned { state, guard } => {
                    self.set_event_acknowledgement(sender, guard)?;
                    if state == EVENT_MAP_NULL_STATE {
                        return Ok(EventDispatchOutcome {
                            acknowledged: guard,
                            state_change: None,
                        });
                    }
                    return self.request_event_state_change(
                        sender,
                        recipient,
                        event,
                        state,
                        (argument_words, argument_pool_slots),
                        guard,
                    );
                }
                HaltReason::StateChanged(state) => {
                    return Ok(EventDispatchOutcome {
                        acknowledged: true,
                        state_change: Some(EventStateChange {
                            recipient,
                            state,
                            event,
                            arguments: Vec::new(),
                            argument_pool_slots: Vec::new(),
                        }),
                    });
                }
                HaltReason::HostEffect if self.level_restart_requested => {
                    return Ok(EventDispatchOutcome {
                        acknowledged: true,
                        state_change: None,
                    });
                }
                HaltReason::ObjectTerminated => {
                    return Ok(EventDispatchOutcome {
                        acknowledged: true,
                        state_change: None,
                    });
                }
                HaltReason::EventServiceInvalidReturn => {}
                _ => unreachable!("invoke_event_service validates its halt reason"),
            }
        }

        let event_index = (event >> 8) as usize;
        let state = self
            .object(recipient)?
            .event_map
            .get(event_index)
            .copied()
            .unwrap_or(EVENT_MAP_NULL_STATE);
        let acknowledged = state != EVENT_MAP_NULL_STATE;
        self.set_event_acknowledgement(sender, acknowledged)?;
        if !acknowledged {
            return Ok(EventDispatchOutcome {
                acknowledged: false,
                state_change: None,
            });
        }

        if state & 0x8000 != 0 {
            self.object_mut(recipient)?
                .set_register(process_register::EVENT, event)?;
            let execution = self.invoke_event_interrupt(
                recipient,
                usize::from(state & 0x7fff),
                argument_words,
                Some(argument_pool_slots),
                host,
                service_audio,
            )?;
            return match execution.reason {
                HaltReason::InterruptCompleted | HaltReason::ObjectTerminated => {
                    Ok(EventDispatchOutcome {
                        acknowledged: true,
                        state_change: None,
                    })
                }
                HaltReason::StateChanged(state) => Ok(EventDispatchOutcome {
                    acknowledged: true,
                    state_change: Some(EventStateChange {
                        recipient,
                        state,
                        event,
                        arguments: Vec::new(),
                        argument_pool_slots: Vec::new(),
                    }),
                }),
                HaltReason::HostEffect if self.level_restart_requested => {
                    Ok(EventDispatchOutcome {
                        acknowledged: true,
                        state_change: None,
                    })
                }
                _ => unreachable!("invoke_event_interrupt validates its halt reason"),
            };
        }

        self.request_event_state_change(
            sender,
            recipient,
            event,
            state,
            (argument_words, argument_pool_slots),
            acknowledged,
        )
    }

    /// Rebinds an object after [`HaltReason::StateChanged`].
    pub fn rebind_state_program(
        &mut self,
        handle: ObjectHandle,
        program: &VmStateProgram,
        arguments: &[u32],
    ) -> Result<(), VmError> {
        self.rebind_state_program_inner(handle, program, arguments, None)
    }

    pub(crate) fn rebind_state_program_with_pool_slots(
        &mut self,
        handle: ObjectHandle,
        program: &VmStateProgram,
        arguments: &[u32],
        argument_pool_slots: &[Option<u8>],
    ) -> Result<(), VmError> {
        self.rebind_state_program_inner(handle, program, arguments, Some(argument_pool_slots))
    }

    fn rebind_state_program_inner(
        &mut self,
        handle: ObjectHandle,
        program: &VmStateProgram,
        arguments: &[u32],
        argument_pool_slots: Option<&[Option<u8>]>,
    ) -> Result<(), VmError> {
        validate_argument_pool_slots(arguments.len(), argument_pool_slots)?;
        let frame_stamp = self.frames_elapsed;
        self.object(handle)?.preflight_state_program_rebind(
            program,
            arguments,
            argument_pool_slots,
            frame_stamp,
        )?;
        self.register_paging_metadata(
            program.page_count,
            &program.resident_pages,
            &program.entry_pages,
        )?;
        let object = self.object_mut(handle)?;
        match argument_pool_slots {
            Some(pool_slots) => object.rebind_state_program_with_pool_slots(
                program,
                arguments,
                pool_slots,
                frame_stamp,
            ),
            None => object.rebind_state_program(program, arguments, frame_stamp),
        }
    }

    /// Starts the `once_p` block captured by the most recent state rebind.
    /// The nested frame is byte-for-byte equivalent to
    /// `GoolObjectPushFrame(obj, 0, 0xffff)` while its return behavior models
    /// `SUSPEND_ON_RET | SUSPEND_ON_RETLNK | STATUS_PRESERVE`.
    fn begin_pending_once(&mut self, handle: ObjectHandle) -> Result<bool, VmError> {
        let Some(pending) = self.object_mut(handle)?.pending_once.take() else {
            return Ok(false);
        };
        if pending.address.segment != CodeSegment::Global {
            return Err(VmError::InvalidOnceCodeSegment(pending.address.segment));
        }
        let object = self.object(handle)?;
        if pending.address.pc >= object.global_code.len() {
            return Err(VmError::InvalidCodeReference(pending.address.to_word()));
        }
        if object.call_stack.len() == MAX_CALL_DEPTH {
            return Err(VmError::CallStackOverflow(handle));
        }
        if object.stack.len() + ONCE_FRAME_WORDS > MAX_STACK_WORDS {
            return Err(VmError::StackOverflow(handle));
        }

        let stack_origin = object.initial_stack_pointer as usize;
        let stack_len = object.stack.len();
        let stack_pointer = stack_origin
            .checked_add(stack_len)
            .ok_or(VmError::StackOverflow(handle))?;
        if stack_pointer + ONCE_FRAME_WORDS > REGISTER_COUNT {
            return Err(VmError::StackOverflow(handle));
        }
        let return_address = object.code_address();
        let return_halted = object.halted;
        let previous_frame_base = object.frame_base;
        let prior_rsp_bytes = stack_pointer
            .checked_mul(4)
            .and_then(|bytes| u16::try_from(bytes).ok())
            .ok_or(VmError::StackOverflow(handle))?;
        let prior_rfp_bytes = previous_frame_base
            .checked_mul(4)
            .and_then(|bytes| u16::try_from(bytes).ok())
            .ok_or(VmError::StackOverflow(handle))?;
        let frame = CallFrame {
            return_address,
            return_halted,
            argument_base: stack_len,
            previous_frame_base,
            behavior: ReturnBehavior::SuspendOnce {
                state_stamp: pending.state_stamp,
            },
        };
        {
            let object = self.object_mut(handle)?;
            object.frame_base = stack_pointer;
            object.call_stack.push(frame);
        }
        self.push(handle, INITIAL_FRAME_FLAGS)?;
        self.push(handle, return_address.to_word())?;
        self.push(
            handle,
            (u32::from(prior_rfp_bytes) << 16) | u32::from(prior_rsp_bytes),
        )?;
        let object = self.object_mut(handle)?;
        object.code_segment = pending.address.segment;
        object.pc = pending.address.pc;
        object.halted = false;
        Ok(true)
    }

    /// Executes a captured state-change once block synchronously, including
    /// any already-supported host effects. Returns `Ok(None)` when the state
    /// change did not carry an armed `once_p` pointer.
    pub fn run_pending_once_with_host_effects<F>(
        &mut self,
        handle: ObjectHandle,
        mut host: F,
    ) -> Result<Option<Execution>, VmError>
    where
        F: FnMut(&mut Self, &VmEffect) -> Result<(), VmError>,
    {
        if !self.begin_pending_once(handle)? {
            return Ok(None);
        }
        let execution = self.run_with_host_effects_mode(
            handle,
            MAX_ONCE_INSTRUCTIONS,
            &mut host,
            false,
            false,
            Some(HaltReason::OnceCompleted),
        )?;
        match execution.reason {
            HaltReason::OnceCompleted
            | HaltReason::StateChanged(_)
            | HaltReason::ObjectTerminated => Ok(Some(execution)),
            HaltReason::BudgetExhausted => Err(VmError::OnceBudgetExhausted(handle)),
            reason => Err(VmError::UnexpectedOnceHalt {
                object: handle,
                reason,
            }),
        }
    }

    /// Audio-aware counterpart of [`Self::run_pending_once_with_host_effects`].
    /// The callback must complete each [`VmHostRequest::Audio`] before the
    /// once block can advance past that opcode.
    pub fn run_pending_once_with_host_requests<F>(
        &mut self,
        handle: ObjectHandle,
        host: F,
    ) -> Result<Option<Execution>, VmError>
    where
        F: FnMut(&mut Self, VmHostRequest) -> Result<(), VmError>,
    {
        if !self.begin_pending_once(handle)? {
            return Ok(None);
        }
        let execution = self.run_with_host_requests_mode(
            handle,
            MAX_ONCE_INSTRUCTIONS,
            host,
            HostRunOptions {
                suspend_on_animation: false,
                apply_animation_gate: false,
                service_audio: true,
                return_link_halt: Some(HaltReason::OnceCompleted),
            },
        )?;
        match execution.reason {
            HaltReason::OnceCompleted
            | HaltReason::StateChanged(_)
            | HaltReason::ObjectTerminated => Ok(Some(execution)),
            HaltReason::BudgetExhausted => Err(VmError::OnceBudgetExhausted(handle)),
            reason => Err(VmError::UnexpectedOnceHalt {
                object: handle,
                reason,
            }),
        }
    }

    /// Pushes and enters the target state's external transition block exactly
    /// as `GoolObjectChangeState` does after installing the initial wait word
    /// and recording `state_stamp`.
    fn begin_transition_block(&mut self, handle: ObjectHandle) -> Result<bool, VmError> {
        let object = self.object(handle)?;
        let Some(transition_address) = object.transition_address else {
            return Ok(false);
        };
        let code_len = match transition_address.segment {
            CodeSegment::External => object.code.len(),
            CodeSegment::Global => object.global_code.len(),
        };
        if transition_address.pc >= code_len {
            return Err(VmError::InvalidJump {
                object: handle,
                target: transition_address.pc as i64,
            });
        }
        if object.call_stack.len() == MAX_CALL_DEPTH {
            return Err(VmError::CallStackOverflow(handle));
        }
        if object.stack.len() + ONCE_FRAME_WORDS > MAX_STACK_WORDS {
            return Err(VmError::StackOverflow(handle));
        }

        let stack_origin = object.initial_stack_pointer as usize;
        let stack_len = object.stack.len();
        let stack_pointer = stack_origin
            .checked_add(stack_len)
            .ok_or(VmError::StackOverflow(handle))?;
        if stack_pointer + ONCE_FRAME_WORDS > REGISTER_COUNT {
            return Err(VmError::StackOverflow(handle));
        }
        let return_address = object.code_address();
        let return_halted = object.halted;
        let previous_frame_base = object.frame_base;
        let previous_animation_wait = object.animation_wait;
        let prior_rsp_bytes = stack_pointer
            .checked_mul(4)
            .and_then(|bytes| u16::try_from(bytes).ok())
            .ok_or(VmError::StackOverflow(handle))?;
        let prior_rfp_bytes = previous_frame_base
            .checked_mul(4)
            .and_then(|bytes| u16::try_from(bytes).ok())
            .ok_or(VmError::StackOverflow(handle))?;
        let frame = CallFrame {
            return_address,
            return_halted,
            argument_base: stack_len,
            previous_frame_base,
            behavior: ReturnBehavior::SuspendTransition {
                previous_animation_wait,
            },
        };
        {
            let object = self.object_mut(handle)?;
            object.frame_base = stack_pointer;
            object.call_stack.push(frame);
        }
        self.push(handle, INITIAL_FRAME_FLAGS)?;
        self.push(handle, return_address.to_word())?;
        self.push(
            handle,
            (u32::from(prior_rfp_bytes) << 16) | u32::from(prior_rsp_bytes),
        )?;
        let object = self.object_mut(handle)?;
        object.code_segment = transition_address.segment;
        object.pc = transition_address.pc;
        object.halted = false;
        Ok(true)
    }

    /// Executes the rebound state's transition block synchronously, including
    /// supported host effects. Animation opcodes mutate animation state but do
    /// not suspend this invocation because retail supplies only
    /// `SUSPEND_ON_RET | SUSPEND_ON_RETLNK` here.
    ///
    /// A returned [`HaltReason::StateChanged`] is intentional: the production
    /// runtime must immediately bind that state before exposing the object to
    /// the next cooperative frame.
    pub fn run_transition_with_host_effects<F>(
        &mut self,
        handle: ObjectHandle,
        mut host: F,
    ) -> Result<Option<Execution>, VmError>
    where
        F: FnMut(&mut Self, &VmEffect) -> Result<(), VmError>,
    {
        if !self.begin_transition_block(handle)? {
            return Ok(None);
        }
        let execution = self.run_with_host_effects_mode(
            handle,
            MAX_TRANSITION_INSTRUCTIONS,
            &mut host,
            false,
            false,
            Some(HaltReason::TransitionCompleted),
        )?;
        match execution.reason {
            HaltReason::TransitionCompleted
            | HaltReason::StateChanged(_)
            | HaltReason::ObjectTerminated => Ok(Some(execution)),
            HaltReason::BudgetExhausted => Err(VmError::TransitionBudgetExhausted(handle)),
            reason => Err(VmError::UnexpectedTransitionHalt {
                object: handle,
                reason,
            }),
        }
    }

    /// Audio-aware counterpart of [`Self::run_transition_with_host_effects`].
    /// The transition remains one bounded synchronous invocation even when it
    /// contains multiple voice-create or voice-control calls.
    pub fn run_transition_with_host_requests<F>(
        &mut self,
        handle: ObjectHandle,
        host: F,
    ) -> Result<Option<Execution>, VmError>
    where
        F: FnMut(&mut Self, VmHostRequest) -> Result<(), VmError>,
    {
        if !self.begin_transition_block(handle)? {
            return Ok(None);
        }
        let execution = self.run_with_host_requests_mode(
            handle,
            MAX_TRANSITION_INSTRUCTIONS,
            host,
            HostRunOptions {
                suspend_on_animation: false,
                apply_animation_gate: false,
                service_audio: true,
                return_link_halt: Some(HaltReason::TransitionCompleted),
            },
        )?;
        match execution.reason {
            HaltReason::TransitionCompleted
            | HaltReason::StateChanged(_)
            | HaltReason::ObjectTerminated => Ok(Some(execution)),
            HaltReason::BudgetExhausted => Err(VmError::TransitionBudgetExhausted(handle)),
            reason => Err(VmError::UnexpectedTransitionHalt {
                object: handle,
                reason,
            }),
        }
    }

    #[must_use]
    pub fn effects(&self) -> &[VmEffect] {
        &self.effects
    }

    /// Resolves the protected save level for the `LoadState` effect that just
    /// crossed the synchronous stream-host boundary.
    ///
    /// The effect must still be the newest observation and must belong to the
    /// same caller. This keeps the VM independent of mounted stream state
    /// while preventing a later `SaveState` from changing the earlier request's
    /// restart kind.
    pub(crate) fn resolve_load_state_effect(
        &mut self,
        object: ObjectHandle,
        saved_level: LevelId,
    ) -> Result<(), VmError> {
        let Some(VmEffect::LoadState {
            object: pending,
            saved_level: slot,
        }) = self.effects.last_mut()
        else {
            return Err(VmError::MissingHostEffect);
        };
        if *pending != object || slot.is_some() {
            return Err(VmError::MissingHostEffect);
        }
        *slot = Some(saved_level);
        Ok(())
    }

    fn enqueue_send_event(
        &mut self,
        request: SendEventRequest,
        return_link_halt: Option<HaltReason>,
    ) -> Result<(), VmError> {
        let sender_incarnation = self.object_incarnation(request.sender)?;
        let id = self.next_send_event_id;
        self.next_send_event_id = self.next_send_event_id.wrapping_add(1).max(1);
        self.emit(VmEffect::SendEvent(request))?;
        self.pending_send_events.push(PendingSendEvent {
            id,
            request,
            sender_incarnation,
            return_link_halt,
            servicing: false,
        });
        Ok(())
    }

    fn pending_send_event_index(&mut self, handle: ObjectHandle) -> Option<usize> {
        loop {
            let index = self
                .pending_send_events
                .iter()
                .rposition(|pending| pending.request.sender == handle && !pending.servicing)?;
            let pending = self.pending_send_events[index];
            if self.incarnation_is_live(handle, pending.sender_incarnation) {
                return Some(index);
            }
            self.pending_send_events.remove(index);
        }
    }

    fn service_pending_send_event<F>(
        &mut self,
        handle: ObjectHandle,
        host: &mut F,
    ) -> Result<SendEventService, VmError>
    where
        F: FnMut(&mut Self, VmHostRequest) -> Result<(), VmError>,
    {
        let Some(index) = self.pending_send_event_index(handle) else {
            return Ok(SendEventService::None);
        };
        let pending = self.pending_send_events[index];
        self.pending_send_events[index].servicing = true;
        if let Err(error) = host(self, VmHostRequest::SendEvent(pending.request)) {
            if let Some(pending) = self
                .pending_send_events
                .iter_mut()
                .find(|candidate| candidate.id == pending.id)
            {
                pending.servicing = false;
            }
            return Err(error);
        }

        let index = self
            .pending_send_events
            .iter()
            .position(|candidate| candidate.id == pending.id)
            .ok_or(VmError::MissingHostEffect)?;
        let pending = self.pending_send_events.remove(index);
        if !self.incarnation_is_live(handle, pending.sender_incarnation) {
            return Ok(SendEventService::Halt(HaltReason::ObjectTerminated));
        }

        let keep_event_stack = self.object(handle)?.register(process_register::STATUS_A)?
            & STATUS_A_KEEP_EVENT_STACK
            != 0;
        if keep_event_stack {
            if let Some(reason) = pending.return_link_halt {
                return Ok(SendEventService::Halt(reason));
            }
            // `GoolObjectChangeState` replaced the old fp/sp and installed the
            // new state's initial animation word. Native pops exactly that one
            // word before continuing at the rebound PC.
            self.object_mut(handle)?.animation_wait = None;
            self.pop(handle)?;
        } else {
            let argument_count = usize::from(pending.request.argument_count);
            let object = self.object_mut(handle)?;
            let new_len = object
                .stack
                .len()
                .checked_sub(argument_count)
                .ok_or(VmError::StackUnderflow(handle))?;
            object.stack.truncate(new_len);
        }
        Ok(SendEventService::Continue)
    }

    /// Returns the synchronous audio request that caused
    /// [`HaltReason::HostEffect`]. The request remains pending until a
    /// matching [`AudioHostResponse`] is supplied.
    #[must_use]
    pub const fn pending_audio_host_request(&self) -> Option<AudioHostRequest> {
        self.pending_audio_host_request
    }

    #[must_use]
    pub const fn pending_card_host_request(&self) -> Option<CardHostRequest> {
        self.pending_card_host_request
    }

    /// Completes the pending retail audio call. Voice creation writes its
    /// signed result bits to `gool_process.voice_id` before interpretation can
    /// resume; control calls require an explicit acknowledgement so template
    /// changes cannot be reordered past a following create instruction.
    pub fn complete_audio_host_request(
        &mut self,
        response: AudioHostResponse,
    ) -> Result<(), VmError> {
        let request = self
            .pending_audio_host_request
            .ok_or(VmError::MissingAudioHostRequest)?;
        match (request, response) {
            (
                AudioHostRequest::CreateVoice(request),
                AudioHostResponse::VoiceCreated { voice_id },
            ) => {
                self.object_mut(request.object)?
                    .set_register(process_register::VOICE_ID, voice_id as u32)?;
            }
            (AudioHostRequest::Control(_), AudioHostResponse::ControlApplied) => {}
            _ => return Err(VmError::MismatchedAudioHostResponse),
        }
        self.pending_audio_host_request = None;
        Ok(())
    }

    /// Reconciles the VM's optimistic, pointer-free paging bookkeeping with
    /// the platform allocator result before the next GOOL instruction runs.
    pub fn complete_paging_host_request(
        &mut self,
        request: PagingHostRequest,
        response: PagingHostResponse,
    ) -> Result<(), VmError> {
        match response {
            PagingHostResponse::Applied { invalidated } => {
                for page in invalidated.iter() {
                    let valid = match request.operation {
                        PagingHostOperation::Open => page != request.page,
                        PagingHostOperation::Close => page == request.page,
                        PagingHostOperation::Probe => false,
                    };
                    if !valid || !self.paging_loaded_pages.contains(&page) {
                        return Err(VmError::MismatchedPagingHostResponse);
                    }
                }
                for page in invalidated.iter() {
                    self.paging_resolved_pages.remove(&page);
                }
                if request.operation == PagingHostOperation::Open {
                    // The VM resolves an entry optimistically before yielding
                    // to the host. Reassert it after removing the displaced
                    // page so a successful open can never invalidate itself.
                    self.paging_resolved_pages.insert(request.page);
                    self.paging_pending_pages.remove(&request.page);
                }
                Ok(())
            }
            PagingHostResponse::Queued => {
                if request.operation != PagingHostOperation::Open
                    || request.physical
                    || request.was_resolved
                {
                    return Err(VmError::MismatchedPagingHostResponse);
                }
                // The optimistic open already acquired the page reference.
                // Native returns null until NSUpdate promotes the state-two
                // virtual record, so retain ownership but re-arm the PTE view.
                self.paging_resolved_pages.remove(&request.page);
                self.paging_pending_pages.insert(request.page);
                self.object_mut(request.object)?
                    .set_register(process_register::MISC_VALUE, 0)
            }
            PagingHostResponse::Unavailable => {
                if request.operation != PagingHostOperation::Open {
                    return Err(VmError::MismatchedPagingHostResponse);
                }
                let references = self
                    .paging_page_references
                    .get_mut(&request.page)
                    .ok_or(VmError::MismatchedPagingHostResponse)?;
                *references = references
                    .checked_sub(1)
                    .ok_or(VmError::MismatchedPagingHostResponse)?;
                if !request.was_resolved {
                    self.paging_resolved_pages.remove(&request.page);
                }
                self.object_mut(request.object)?
                    .set_register(process_register::MISC_VALUE, 0)
            }
        }
    }

    /// Seeds browser-owned load-list references after the mounted program
    /// graph has registered the NSF's page metadata.
    ///
    /// Program/global pages already marked resolved remain so; the supplied
    /// page counts replace the otherwise-empty platform reference view at the
    /// pre-frame mount boundary.
    pub fn seed_platform_paging_state(
        &mut self,
        page_count: u32,
        resolved_pages: impl IntoIterator<Item = PageIndex>,
        page_references: impl IntoIterator<Item = (PageIndex, u32)>,
    ) -> Result<(), VmError> {
        self.seed_platform_paging_state_with_uncounted_pages(
            page_count,
            resolved_pages,
            page_references,
            std::iter::empty(),
        )
    }

    /// Seeds platform paging state while excluding texture-cache pages from
    /// retail's `NSCountAvailablePages` result.
    pub fn seed_platform_paging_state_with_uncounted_pages(
        &mut self,
        page_count: u32,
        resolved_pages: impl IntoIterator<Item = PageIndex>,
        page_references: impl IntoIterator<Item = (PageIndex, u32)>,
        uncounted_pages: impl IntoIterator<Item = PageIndex>,
    ) -> Result<(), VmError> {
        self.seed_platform_paging_state_with_capacity(
            page_count,
            page_count.min(PHYSICAL_SLOT_COUNT as u32),
            resolved_pages,
            page_references,
            uncounted_pages,
        )
    }

    /// Seeds the catalog separately from the heap-derived PS1 physical pool.
    ///
    /// Retail catalogs up to 128 NSF pages, but its descending 64 KiB malloc
    /// probe can expose fewer than the nominal twenty-two ordinary slots.
    pub fn seed_platform_paging_state_with_capacity(
        &mut self,
        page_count: u32,
        physical_page_capacity: u32,
        resolved_pages: impl IntoIterator<Item = PageIndex>,
        page_references: impl IntoIterator<Item = (PageIndex, u32)>,
        uncounted_pages: impl IntoIterator<Item = PageIndex>,
    ) -> Result<(), VmError> {
        if physical_page_capacity == 0 || physical_page_capacity > PHYSICAL_SLOT_COUNT as u32 {
            return Err(VmError::InvalidPlatformPagingCapacity(
                physical_page_capacity,
            ));
        }
        let resolved_pages = resolved_pages.into_iter().collect::<BTreeSet<_>>();
        let page_references = page_references.into_iter().collect::<BTreeMap<_, _>>();
        let uncounted_pages = uncounted_pages.into_iter().collect::<BTreeSet<_>>();
        for page in resolved_pages
            .iter()
            .copied()
            .chain(page_references.keys().copied())
            .chain(uncounted_pages.iter().copied())
        {
            if page.get() >= page_count {
                return Err(VmError::InvalidPlatformPagingPage(page));
            }
        }
        self.paging_page_capacity = physical_page_capacity;
        self.paging_page_capacity_authority = PagingCapacityAuthority::PlatformHeap;
        for index in 0..page_count {
            let page = PageIndex::new(index);
            self.paging_baseline_pages.insert(page);
            self.paging_loaded_pages.insert(page);
        }
        self.paging_resolved_pages.extend(resolved_pages);
        self.paging_pending_pages.clear();
        self.paging_uncounted_pages = uncounted_pages;
        self.paging_page_references = page_references
            .into_iter()
            .filter(|(_, references)| *references != 0)
            .collect();
        Ok(())
    }

    /// Seeds state-two virtual requests retained by the mounted platform
    /// pager before the first browser frame runs.
    pub fn seed_platform_pending_pages(
        &mut self,
        pages: impl IntoIterator<Item = PageIndex>,
    ) -> Result<(), VmError> {
        let pages = pages.into_iter().collect::<BTreeSet<_>>();
        for page in &pages {
            if !self.paging_loaded_pages.contains(page)
                || self.paging_resolved_pages.contains(page)
                || self
                    .paging_page_references
                    .get(page)
                    .copied()
                    .unwrap_or_default()
                    == 0
            {
                return Err(VmError::InvalidPlatformPagingPage(*page));
            }
        }
        self.paging_pending_pages = pages;
        Ok(())
    }

    /// Applies one browser lifecycle page open outside a GOOL instruction.
    pub fn apply_platform_paging_open(
        &mut self,
        page: PageIndex,
        invalidated: PageInvalidations,
    ) -> Result<(), VmError> {
        if !self.paging_loaded_pages.contains(&page) {
            return Err(VmError::InvalidPlatformPagingPage(page));
        }
        for invalidated_page in invalidated.iter() {
            if invalidated_page == page || !self.paging_loaded_pages.contains(&invalidated_page) {
                return Err(VmError::InvalidPlatformPagingPage(invalidated_page));
            }
        }
        for invalidated_page in invalidated.iter() {
            self.paging_resolved_pages.remove(&invalidated_page);
        }
        self.paging_resolved_pages.insert(page);
        self.paging_pending_pages.remove(&page);
        let references = self.paging_page_references.entry(page).or_default();
        *references = references
            .checked_add(1)
            .ok_or(VmError::MismatchedPagingHostResponse)?;
        Ok(())
    }

    /// Retains one browser lifecycle reference in native's virtual queue.
    pub fn apply_platform_paging_queued_open(&mut self, page: PageIndex) -> Result<(), VmError> {
        if !self.paging_loaded_pages.contains(&page) || self.paging_resolved_pages.contains(&page) {
            return Err(VmError::InvalidPlatformPagingPage(page));
        }
        self.paging_pending_pages.insert(page);
        let references = self.paging_page_references.entry(page).or_default();
        *references = references
            .checked_add(1)
            .ok_or(VmError::MismatchedPagingHostResponse)?;
        Ok(())
    }

    /// Publishes one successful platform `NSUpdate(-1)` promotion without
    /// acquiring another reference for the already-owned virtual request.
    pub fn apply_platform_paging_resolution(
        &mut self,
        page: PageIndex,
        invalidated: PageInvalidations,
    ) -> Result<(), VmError> {
        if !self.paging_loaded_pages.contains(&page) || !self.paging_pending_pages.contains(&page) {
            return Err(VmError::InvalidPlatformPagingPage(page));
        }
        for invalidated_page in invalidated.iter() {
            if invalidated_page == page || !self.paging_loaded_pages.contains(&invalidated_page) {
                return Err(VmError::InvalidPlatformPagingPage(invalidated_page));
            }
        }
        for invalidated_page in invalidated.iter() {
            self.paging_resolved_pages.remove(&invalidated_page);
        }
        self.paging_pending_pages.remove(&page);
        self.paging_resolved_pages.insert(page);
        Ok(())
    }

    /// Publishes a count-zero global-program materialization.
    ///
    /// Program binding registers the target's catalog metadata separately
    /// from the pager allocation that made its PTE usable. This operation
    /// reconciles those views without acquiring another reference: an
    /// already-owned queued request keeps its count, while any displaced
    /// zero-reference PTEs become unresolved.
    pub fn apply_platform_program_materialization(
        &mut self,
        page: PageIndex,
        invalidated: PageInvalidations,
    ) -> Result<(), VmError> {
        if !self.paging_loaded_pages.contains(&page) {
            return Err(VmError::InvalidPlatformPagingPage(page));
        }
        for invalidated_page in invalidated.iter() {
            if invalidated_page == page
                || !self.paging_loaded_pages.contains(&invalidated_page)
                || self
                    .paging_page_references
                    .get(&invalidated_page)
                    .is_some_and(|references| *references != 0)
            {
                return Err(VmError::InvalidPlatformPagingPage(invalidated_page));
            }
        }
        for invalidated_page in invalidated.iter() {
            self.paging_resolved_pages.remove(&invalidated_page);
            self.paging_pending_pages.remove(&invalidated_page);
        }
        self.paging_resolved_pages.insert(page);
        self.paging_pending_pages.remove(&page);
        Ok(())
    }

    /// Invalidates a zero-reference page displaced by a count-zero global
    /// program materialization. No reference is acquired for the new global
    /// page; its program metadata marks that page resolved during insertion.
    pub fn apply_platform_paging_eviction(&mut self, page: PageIndex) -> Result<(), VmError> {
        if !self.paging_loaded_pages.contains(&page)
            || self
                .paging_page_references
                .get(&page)
                .is_some_and(|references| *references != 0)
        {
            return Err(VmError::InvalidPlatformPagingPage(page));
        }
        self.paging_resolved_pages.remove(&page);
        Ok(())
    }

    /// Applies one transactional CD-group reservation invalidation batch.
    ///
    /// Native re-arms every victim PTE before starting the asynchronous read.
    /// Validate the complete physical run first so a malformed platform event
    /// cannot leave the VM mirror partially invalidated.
    pub fn apply_platform_paging_evictions(&mut self, pages: &[PageIndex]) -> Result<(), VmError> {
        let mut unique = BTreeSet::new();
        for &page in pages {
            if !unique.insert(page)
                || !self.paging_loaded_pages.contains(&page)
                || !self.paging_resolved_pages.contains(&page)
                || self.paging_pending_pages.contains(&page)
                || self
                    .paging_page_references
                    .get(&page)
                    .is_some_and(|references| *references != 0)
            {
                return Err(VmError::InvalidPlatformPagingPage(page));
            }
        }
        for page in unique {
            self.paging_resolved_pages.remove(&page);
        }
        Ok(())
    }

    /// Applies one browser lifecycle page close outside a GOOL instruction.
    pub fn apply_platform_paging_close(
        &mut self,
        page: PageIndex,
        decremented: bool,
        unresolved: bool,
    ) -> Result<(), VmError> {
        if !self.paging_loaded_pages.contains(&page) {
            return Err(VmError::InvalidPlatformPagingPage(page));
        }
        if unresolved && !decremented {
            return Err(VmError::MismatchedPagingHostResponse);
        }
        if decremented {
            let references = self.paging_page_references.entry(page).or_default();
            *references = references
                .checked_sub(1)
                .ok_or(VmError::MismatchedPagingHostResponse)?;
        }
        if unresolved {
            self.paging_resolved_pages.remove(&page);
            let references = self
                .paging_page_references
                .get(&page)
                .copied()
                .unwrap_or_default();
            if references == 0 {
                self.paging_pending_pages.remove(&page);
            } else {
                self.paging_pending_pages.insert(page);
            }
        }
        Ok(())
    }

    /// Completes misc primary fifteen before interpretation advances.
    ///
    /// `CardControl` returns a signed C `int` directly into process register
    /// 37 and never pushes a stack value.
    pub fn complete_card_host_request(
        &mut self,
        response_for: CardHostRequest,
        result: i32,
    ) -> Result<(), VmError> {
        let request = self
            .pending_card_host_request
            .ok_or(VmError::MissingHostEffect)?;
        if request != response_for {
            return Err(VmError::MissingHostEffect);
        }
        self.object_mut(request.object)?
            .set_register(process_register::MISC_VALUE, result as u32)?;
        self.pending_card_host_request = None;
        Ok(())
    }

    /// Completes misc seven after the runtime has searched native logical
    /// handles three and four in preorder. The returned object becomes the
    /// same checked 32-bit token used by collision/object operands.
    pub fn complete_find_spawned_object(
        &mut self,
        requester: ObjectHandle,
        found: Option<ObjectHandle>,
    ) -> Result<(), VmError> {
        self.object(requester)?;
        let value = if let Some(found) = found {
            self.object(found)?;
            CollisionObjectReference::new(found).to_word()
        } else {
            0
        };
        self.push(requester, value)
    }

    /// Completes misc 13 with a checked object token or native null.
    pub fn complete_find_nearest_object(
        &mut self,
        requester: ObjectHandle,
        found: Option<ObjectHandle>,
    ) -> Result<(), VmError> {
        self.complete_find_spawned_object(requester, found)
    }

    /// Applies one host-resolved model vertex through the linked object's
    /// exact retail scale/Y-X-Y/translation transform.
    pub fn complete_model_vertex_transform(
        &mut self,
        requester: ObjectHandle,
        link: ObjectHandle,
        output_vector: u8,
        source: Option<ModelVertexSource>,
    ) -> Result<(), VmError> {
        self.object(requester)?;
        let Some(source) = source else {
            return Ok(());
        };
        let (translation, rotation, object_scale) = {
            let link = self.object(link)?;
            (
                link.process_vector(0)?,
                link.process_vector(1)?,
                link.process_vector(2)?,
            )
        };
        let scale = [0_usize, 1, 2]
            .map(|axis| source.geometry_scale[axis].wrapping_mul(object_scale[axis]) >> 12);
        let transformed = retail_yxy_transform(
            Vec3 {
                x: source.local_position[0],
                y: source.local_position[1],
                z: source.local_position[2],
            },
            BoundTransform {
                translation: Vec3 {
                    x: translation[0],
                    y: translation[1],
                    z: translation[2],
                },
                rotation: Angles {
                    y: Angle12::new(rotation[0]),
                    x: Angle12::new(rotation[1]),
                    z: Angle12::new(rotation[2]),
                },
                scale: Vec3 {
                    x: scale[0],
                    y: scale[1],
                    z: scale[2],
                },
            },
        );
        self.object_mut(requester)?
            .set_process_vector(output_vector, [transformed.x, transformed.y, transformed.z])
    }

    #[must_use]
    pub fn take_effects(&mut self) -> Vec<VmEffect> {
        self.effect_checkpoint = 0;
        core::mem::take(&mut self.effects)
    }

    pub(crate) fn drain_effects_into(&mut self, destination: &mut Vec<VmEffect>) {
        destination.append(&mut self.effects);
        self.effect_checkpoint = 0;
    }

    pub(crate) fn clear_effects(&mut self) {
        self.effects.clear();
        self.effect_checkpoint = 0;
    }

    /// Starts a new bounded synchronous effect segment without discarding the
    /// ordered observations accumulated for the current caller. Native
    /// broadcasts apply each recipient before visiting the next one, so one
    /// recipient—not the full 96-object traversal—is the uninterrupted unit.
    pub(crate) fn checkpoint_effects(&mut self) {
        self.effect_checkpoint = self.effects.len();
    }

    pub fn run(&mut self, handle: ObjectHandle, budget: usize) -> Result<Execution, VmError> {
        if self.pending_send_event_index(handle).is_some()
            || self.pending_audio_host_request.is_some()
            || self.pending_card_host_request.is_some()
        {
            return Ok(Execution {
                reason: HaltReason::HostEffect,
                steps: 0,
            });
        }
        if let Some(execution) = self.animation_gate(handle)? {
            return Ok(execution);
        }
        let mut condition = false;
        for steps in 0..budget {
            if let Some(reason) = self.step(handle, &mut condition, None)? {
                return Ok(Execution {
                    reason,
                    steps: steps + 1,
                });
            }
        }
        Ok(Execution {
            reason: HaltReason::BudgetExhausted,
            steps: budget,
        })
    }

    /// Runs one interpreter invocation while applying spawn/event effects
    /// before the following instruction, matching their synchronous retail
    /// host calls. Typed audio work is returned as [`HaltReason::HostEffect`]
    /// and must be completed through [`Self::complete_audio_host_request`].
    ///
    /// The callback may update the machine (for example by installing a child
    /// object or link) and external host state. Returning an error aborts the
    /// invocation without executing the next instruction.
    pub fn run_with_host_effects<F>(
        &mut self,
        handle: ObjectHandle,
        budget: usize,
        host: F,
    ) -> Result<Execution, VmError>
    where
        F: FnMut(&mut Self, &VmEffect) -> Result<(), VmError>,
    {
        self.run_with_host_effects_mode(handle, budget, host, true, true, None)
    }

    /// Runs one interpreter invocation while synchronously servicing both
    /// ordinary VM effects and typed audio calls.
    ///
    /// An [`VmHostRequest::Audio`] callback must complete the request through
    /// [`Self::complete_audio_host_request`]. If it deliberately leaves the
    /// request pending, execution returns [`HaltReason::HostEffect`] without
    /// advancing to the following instruction.
    pub fn run_with_host_requests<F>(
        &mut self,
        handle: ObjectHandle,
        budget: usize,
        host: F,
    ) -> Result<Execution, VmError>
    where
        F: FnMut(&mut Self, VmHostRequest) -> Result<(), VmError>,
    {
        self.run_with_host_requests_mode(
            handle,
            budget,
            host,
            HostRunOptions {
                suspend_on_animation: true,
                apply_animation_gate: true,
                service_audio: true,
                return_link_halt: None,
            },
        )
    }

    fn run_with_host_effects_mode<F>(
        &mut self,
        handle: ObjectHandle,
        budget: usize,
        mut host: F,
        suspend_on_animation: bool,
        apply_animation_gate: bool,
        return_link_halt: Option<HaltReason>,
    ) -> Result<Execution, VmError>
    where
        F: FnMut(&mut Self, &VmEffect) -> Result<(), VmError>,
    {
        self.run_with_host_requests_mode(
            handle,
            budget,
            |machine, request| match request {
                VmHostRequest::Effect(effect) => host(machine, &effect),
                VmHostRequest::SendEvent(request) => host(machine, &VmEffect::SendEvent(request)),
                VmHostRequest::Audio(_) => {
                    unreachable!("legacy host-effect runner suspends before typed audio")
                }
                VmHostRequest::Card(_) => {
                    unreachable!("legacy host-effect runner suspends before typed card control")
                }
            },
            HostRunOptions {
                suspend_on_animation,
                apply_animation_gate,
                service_audio: false,
                return_link_halt,
            },
        )
    }

    fn run_with_host_requests_mode<F>(
        &mut self,
        handle: ObjectHandle,
        budget: usize,
        mut host: F,
        options: HostRunOptions,
    ) -> Result<Execution, VmError>
    where
        F: FnMut(&mut Self, VmHostRequest) -> Result<(), VmError>,
    {
        let HostRunOptions {
            suspend_on_animation,
            apply_animation_gate,
            service_audio,
            return_link_halt,
        } = options;
        let incarnation = self.object_incarnation(handle)?;
        if self.level_restart_requested {
            return Ok(Execution {
                reason: HaltReason::HostEffect,
                steps: 0,
            });
        }
        match self.service_pending_send_event(handle, &mut host)? {
            SendEventService::Halt(reason) => {
                return Ok(Execution { reason, steps: 0 });
            }
            SendEventService::Continue | SendEventService::None => {}
        }
        if let Some(request) = self.pending_audio_host_request {
            if !service_audio {
                return Ok(Execution {
                    reason: HaltReason::HostEffect,
                    steps: 0,
                });
            }
            host(self, VmHostRequest::Audio(request))?;
            if !self.incarnation_is_live(handle, incarnation) {
                return Ok(Execution {
                    reason: HaltReason::ObjectTerminated,
                    steps: 0,
                });
            }
            if self.pending_audio_host_request.is_some() {
                return Ok(Execution {
                    reason: HaltReason::HostEffect,
                    steps: 0,
                });
            }
        }
        if let Some(request) = self.pending_card_host_request {
            if !service_audio {
                return Ok(Execution {
                    reason: HaltReason::HostEffect,
                    steps: 0,
                });
            }
            host(self, VmHostRequest::Card(request))?;
            if !self.incarnation_is_live(handle, incarnation) {
                return Ok(Execution {
                    reason: HaltReason::ObjectTerminated,
                    steps: 0,
                });
            }
            if self.pending_card_host_request.is_some() {
                return Ok(Execution {
                    reason: HaltReason::HostEffect,
                    steps: 0,
                });
            }
        }
        if apply_animation_gate && let Some(execution) = self.animation_gate(handle)? {
            return Ok(execution);
        }
        let mut condition = false;
        for steps in 0..budget {
            if let Some(reason) = self.step(handle, &mut condition, return_link_halt)? {
                if reason == HaltReason::HostEffect {
                    if self.pending_send_event_index(handle).is_some() {
                        match self.service_pending_send_event(handle, &mut host)? {
                            SendEventService::Halt(reason) => {
                                return Ok(Execution {
                                    reason,
                                    steps: steps + 1,
                                });
                            }
                            SendEventService::Continue => {
                                if self.level_restart_requested {
                                    return Ok(Execution {
                                        reason: HaltReason::HostEffect,
                                        steps: steps + 1,
                                    });
                                }
                                continue;
                            }
                            SendEventService::None => return Err(VmError::MissingHostEffect),
                        }
                    }
                    if let Some(request) = self.pending_audio_host_request {
                        if !service_audio {
                            return Ok(Execution {
                                reason,
                                steps: steps + 1,
                            });
                        }
                        host(self, VmHostRequest::Audio(request))?;
                        if !self.incarnation_is_live(handle, incarnation) {
                            return Ok(Execution {
                                reason: HaltReason::ObjectTerminated,
                                steps: steps + 1,
                            });
                        }
                        if self.pending_audio_host_request.is_some() {
                            return Ok(Execution {
                                reason,
                                steps: steps + 1,
                            });
                        }
                        continue;
                    }
                    if let Some(request) = self.pending_card_host_request {
                        if !service_audio {
                            return Ok(Execution {
                                reason,
                                steps: steps + 1,
                            });
                        }
                        host(self, VmHostRequest::Card(request))?;
                        if !self.incarnation_is_live(handle, incarnation) {
                            return Ok(Execution {
                                reason: HaltReason::ObjectTerminated,
                                steps: steps + 1,
                            });
                        }
                        if self.pending_card_host_request.is_some() {
                            return Ok(Execution {
                                reason,
                                steps: steps + 1,
                            });
                        }
                        continue;
                    }
                    let effect = self
                        .effects
                        .last()
                        .cloned()
                        .ok_or(VmError::MissingHostEffect)?;
                    if !matches!(
                        effect,
                        VmEffect::SpawnChildren { .. }
                            | VmEffect::Event { .. }
                            | VmEffect::SetObjectZoneToTransitionTarget { .. }
                            | VmEffect::TerminateCurrentZoneNeighbors { .. }
                            | VmEffect::SetLinkZoneFromPoint { .. }
                            | VmEffect::ReparentToRoot { .. }
                            | VmEffect::FindSpawnedObject { .. }
                            | VmEffect::FindNearestObject { .. }
                            | VmEffect::SpawnFlagsChanged { .. }
                            | VmEffect::TransformModelVertex { .. }
                            | VmEffect::SaveState(_)
                            | VmEffect::LoadState { .. }
                            | VmEffect::ResetLevelGlobals { .. }
                            | VmEffect::Paging { .. }
                    ) {
                        return Err(VmError::MissingHostEffect);
                    }
                    host(self, VmHostRequest::Effect(effect))?;
                    if !self.incarnation_is_live(handle, incarnation) {
                        return Ok(Execution {
                            reason: HaltReason::ObjectTerminated,
                            steps: steps + 1,
                        });
                    }
                    if self.level_restart_requested {
                        return Ok(Execution {
                            reason: HaltReason::HostEffect,
                            steps: steps + 1,
                        });
                    }
                    continue;
                }
                if matches!(reason, HaltReason::AnimationChanged { .. }) {
                    if service_audio {
                        let effect = self
                            .effects
                            .last()
                            .cloned()
                            .ok_or(VmError::MissingHostEffect)?;
                        if !matches!(effect, VmEffect::AnimationFrameChanged { .. }) {
                            return Err(VmError::MissingHostEffect);
                        }
                        host(self, VmHostRequest::Effect(effect))?;
                        if !self.incarnation_is_live(handle, incarnation) {
                            return Ok(Execution {
                                reason: HaltReason::ObjectTerminated,
                                steps: steps + 1,
                            });
                        }
                        if self.level_restart_requested {
                            return Ok(Execution {
                                reason: HaltReason::HostEffect,
                                steps: steps + 1,
                            });
                        }
                    }
                    if !suspend_on_animation {
                        continue;
                    }
                }
                return Ok(Execution {
                    reason,
                    steps: steps + 1,
                });
            }
        }
        Ok(Execution {
            reason: HaltReason::BudgetExhausted,
            steps: budget,
        })
    }

    fn animation_gate(&mut self, handle: ObjectHandle) -> Result<Option<Execution>, VmError> {
        let Some(wait) = self.object(handle)?.animation_wait else {
            return Ok(None);
        };
        let elapsed = self.frames_elapsed.wrapping_sub(wait.stamp);
        if elapsed < u32::from(wait.frames) {
            return Ok(Some(Execution {
                reason: HaltReason::AnimationWaiting {
                    remaining: (u32::from(wait.frames) - elapsed) as u8,
                },
                steps: 0,
            }));
        }
        self.object_mut(handle)?.animation_wait = None;
        self.pop(handle)?;
        Ok(None)
    }

    fn execute_send_event_opcode(
        &mut self,
        handle: ObjectHandle,
        word: u32,
        opcode: u8,
        event_operand: Operand,
        return_link_halt: Option<HaltReason>,
    ) -> Result<Option<HaltReason>, VmError> {
        let mode = ((word >> 21) & 7) as u8;
        // Opcodes 0x87/0x90 read their link before entering
        // GoolOpSendEvent. Keep that value stable across B/condition stack
        // translation just as the native local `recipient` does.
        let linked_recipient = if matches!(opcode, 0x87 | 0x90) {
            self.resolve_process_link(handle, usize::from(mode))?
        } else {
            None
        };
        // Native retains the pointer produced by GOP-B translation, then
        // clears KEEP before it dereferences the event word.
        let event_source = self.input_reference(handle, event_operand)?;
        let status_a = self.object(handle)?.register(process_register::STATUS_A)?;
        self.object_mut(handle)?.set_register(
            process_register::STATUS_A,
            status_a & !STATUS_A_KEEP_EVENT_STACK,
        )?;
        let condition_register = ((word >> 12) & 0x3f) as usize;
        let condition = self.read_process_register_reference(handle, condition_register)?;
        let argument_count = ((word >> 18) & 7) as usize;
        let eligible = event_source.is_some()
            && condition != 0
            && (linked_recipient.is_some() || opcode == 0x8f);
        if !eligible {
            self.object_mut(handle)?
                .set_register(process_register::MISC_VALUE, 0)?;
            let object = self.object_mut(handle)?;
            let new_len = object
                .stack
                .len()
                .checked_sub(argument_count)
                .ok_or(VmError::StackUnderflow(handle))?;
            object.stack.truncate(new_len);
            return Ok(None);
        }

        let mut arguments = [0; MAX_EVENT_ARGUMENTS];
        let mut argument_pool_slots = [None; MAX_EVENT_ARGUMENTS];
        {
            let object = self.object(handle)?;
            let first = object
                .stack
                .len()
                .checked_sub(argument_count)
                .ok_or(VmError::StackUnderflow(handle))?;
            arguments[..argument_count].copy_from_slice(&object.stack[first..]);
            let stack_origin = usize::try_from(object.initial_stack_pointer)
                .map_err(|_| VmError::InvalidInitialStackPointer(object.initial_stack_pointer))?;
            for (offset, pool_slot) in argument_pool_slots[..argument_count].iter_mut().enumerate()
            {
                let register = stack_origin
                    .checked_add(first)
                    .and_then(|index| index.checked_add(offset))
                    .ok_or(VmError::StackOverflow(handle))?;
                *pool_slot = self.live_pool_slot_for_word(
                    arguments[offset],
                    object.register_pool_slot(register)?,
                );
            }
        }
        let event =
            self.read_storage_reference(event_source.expect("eligible source is present"))?;
        let target = match opcode {
            0x87 => SendEventTarget::Direct {
                recipient: linked_recipient.expect("eligible direct recipient is present"),
            },
            0x8f => SendEventTarget::AllRoots { mode },
            0x90 => SendEventTarget::LinkedChildren {
                root: linked_recipient.expect("eligible linked root is present"),
                mode,
            },
            _ => unreachable!("send-event helper is called only for event opcodes"),
        };
        let request = SendEventRequest {
            sender: handle,
            target,
            event,
            arguments,
            argument_pool_slots,
            argument_count: argument_count as u8,
        };
        self.enqueue_send_event(request, return_link_halt)?;
        Ok(Some(HaltReason::HostEffect))
    }

    fn step(
        &mut self,
        handle: ObjectHandle,
        condition: &mut bool,
        return_link_halt: Option<HaltReason>,
    ) -> Result<Option<HaltReason>, VmError> {
        if self.pending_send_event_index(handle).is_some()
            || self.pending_audio_host_request.is_some()
            || self.pending_card_host_request.is_some()
        {
            return Ok(Some(HaltReason::HostEffect));
        }
        let word = {
            let object = self.object_mut(handle)?;
            if object.halted {
                return Ok(Some(HaltReason::Halted));
            }
            let code = match object.code_segment {
                CodeSegment::External => &object.code,
                CodeSegment::Global => &object.global_code,
            };
            let word = code
                .get(object.pc)
                .copied()
                .ok_or(VmError::ProgramCounterOutOfBounds {
                    object: handle,
                    pc: object.pc,
                })?;
            object.pc += 1;
            word
        };
        let instruction = Instruction::decode(word);
        let a = Operand::decode(instruction.operand_a);
        let b = Operand::decode(instruction.operand_b);

        match instruction.opcode {
            0x00 => self.binary_push(handle, a, b, u32::wrapping_add)?,
            0x01 => self.binary_push(handle, a, b, u32::wrapping_sub)?,
            0x02 => self.binary_push(handle, a, b, |left, right| {
                (left as i32).wrapping_mul(right as i32) as u32
            })?,
            0x03 => {
                let divisor = self.read_operand(handle, a)? as i32;
                let dividend = self.read_operand(handle, b)? as i32;
                if divisor == 0 {
                    return Err(VmError::DivisionByZero);
                }
                let value = dividend
                    .checked_div(divisor)
                    .ok_or(VmError::ArithmeticOverflow)?;
                self.push(handle, value as u32)?;
            }
            0x04 => {
                // Native compares physical pointer words. Compact VM handles
                // can be reused independently, so provenance-bearing object
                // references compare by their stable pool-slot identity.
                let (right, right_pool_slot) = self.read_operand_with_pool_slot(handle, a)?;
                let (left, left_pool_slot) = self.read_operand_with_pool_slot(handle, b)?;
                let equal = match (left_pool_slot, right_pool_slot) {
                    (Some(left), Some(right)) => left == right,
                    (None, None) => left == right,
                    _ => false,
                };
                self.push(handle, u32::from(equal))?;
            }
            0x05 => self.binary_push(handle, a, b, |left, right| {
                u32::from(right != 0 && left != 0)
            })?,
            0x06 => self.binary_push(handle, a, b, |left, right| {
                u32::from(left != 0 || right != 0)
            })?,
            0x07 => self.binary_push(handle, a, b, |left, right| left & right)?,
            0x08 => self.binary_push(handle, a, b, |left, right| left | right)?,
            0x09 => self.compare_push(handle, a, b, |left, right| left > right)?,
            0x0a => self.compare_push(handle, a, b, |left, right| left >= right)?,
            0x0b => self.compare_push(handle, a, b, |left, right| left < right)?,
            0x0c => self.compare_push(handle, a, b, |left, right| left <= right)?,
            0x0d => {
                let divisor = self.read_operand(handle, a)? as i32;
                let dividend = self.read_operand(handle, b)? as i32;
                if divisor == 0 {
                    return Err(VmError::DivisionByZero);
                }
                let value = dividend
                    .checked_rem(divisor)
                    .ok_or(VmError::ArithmeticOverflow)?;
                self.push(handle, value as u32)?;
            }
            0x0e => self.binary_push(handle, a, b, |left, right| left ^ right)?,
            0x0f => {
                // `binary_push` names B `left` and A `right` to preserve the
                // arithmetic opcodes' retail ordering. TST is specifically
                // `(A & B) == A`, so the subset is the right-hand argument.
                self.binary_push(handle, a, b, |left, right| u32::from(left & right == right))?;
            }
            0x10 => {
                let upper = self.read_operand(handle, a)?;
                let lower = self.read_operand(handle, b)?;
                let value = if upper == lower {
                    lower
                } else {
                    lower.wrapping_add(retail_random(
                        upper.wrapping_sub(lower),
                        &mut self.random_seed,
                    ))
                };
                self.push(handle, value)?;
            }
            0x11 => {
                let (value, pool_slot) = self.read_operand_with_pool_slot(handle, a)?;
                self.write_operand_with_pool_slot(handle, b, value, pool_slot)?;
            }
            0x12 => {
                let value = u32::from(self.read_operand(handle, a)? == 0);
                self.write_operand(handle, b, value)?;
            }
            0x13 => {
                let (step, target) = if a == Operand::StackDouble {
                    (self.pop(handle)? as i32, self.pop(handle)? as i32)
                } else {
                    (0x100, self.read_operand(handle, a)? as i32)
                };
                if b != Operand::Null {
                    let rate = self.read_operand(handle, b)? as i32;
                    let progress = if target >= 0 {
                        if rate.wrapping_add(step) < target {
                            rate.wrapping_add(step)
                        } else {
                            step.wrapping_mul(2).wrapping_sub(target)
                        }
                    } else if target >= rate.wrapping_sub(step) {
                        target.wrapping_neg().wrapping_sub(step.wrapping_mul(2))
                    } else {
                        rate.wrapping_sub(step)
                    };
                    let absolute_progress = if target >= 0 {
                        progress.wrapping_abs()
                    } else {
                        progress.wrapping_abs().wrapping_neg()
                    };
                    self.write_operand(handle, b, progress as u32)?;
                    self.push(handle, absolute_progress as u32)?;
                }
            }
            0x14 => {
                // Native LEA translates A through the input cursor before it
                // translates B through the independent output cursor, then
                // stores A's address rather than the value behind it. A null
                // source is therefore a literal null word; a null destination
                // still preserves every A-side stack/constant side effect.
                let source = self.input_reference(handle, a)?;
                let destination = self.output_reference(handle, b)?;
                if let Some(destination) = destination {
                    self.write_storage_reference(
                        destination,
                        source.map_or(0, StorageReference::to_word),
                    )?;
                }
            }
            0x15 => {
                let shift = self.read_operand(handle, a)? as i32;
                let value = self.read_operand(handle, b)? as i32;
                // Retail emits MIPS `sllv`/`srav`: variable shifts consume
                // only the low five bits even though the decompiled C form
                // has undefined behavior for magnitudes of 32 or greater.
                let magnitude = if shift < 0 {
                    shift.wrapping_neg() as u32
                } else {
                    shift as u32
                } & 31;
                let shifted = if shift < 0 {
                    value >> magnitude
                } else {
                    value.wrapping_shl(magnitude)
                };
                self.push(handle, shifted as u32)?;
            }
            0x16 => {
                // Retail translates A before B. Stack inputs therefore pop in
                // that order, and null is an absent pointer rather than the
                // PS1 compatibility value used by ordinary GOP reads.
                let source = self.read_optional_input(handle, a)?;
                let destination = self.read_optional_input(handle, b)?;
                if let Some(destination) = destination {
                    self.push(handle, destination)?;
                    if let Some(source) = source {
                        self.push(handle, source)?;
                    }
                }
            }
            0x17 => {
                let value = !self.read_operand(handle, a)?;
                self.write_operand(handle, b, value)?;
            }
            0x18 => {
                let offset = (word & 0x3fff) as usize;
                if offset >= self.object(handle)?.global_code.len() {
                    return Err(VmError::InvalidJump {
                        object: handle,
                        target: offset as i64,
                    });
                }
                let reference = encode_code_reference(CodeAddress {
                    segment: CodeSegment::Global,
                    pc: offset,
                });
                let register = ((word >> 14) & 0x3f) as usize;
                if register == 0x1f {
                    self.push(handle, reference)?;
                } else {
                    // `MOVC` writes through the process-register union. Code
                    // pointer aliases such as `tp` and `pc` must therefore
                    // update their validated typed counterparts as well as
                    // retaining the exact raw retail word.
                    self.write_aliased_process_register_with_pool_slot(
                        handle, register, reference, None,
                    )?;
                }
            }
            0x19 => {
                let value = self.read_operand(handle, a)? as i32;
                self.write_operand(
                    handle,
                    b,
                    value.checked_abs().ok_or(VmError::ArithmeticOverflow)? as u32,
                )?;
            }
            0x1a => {
                let result = self.test_controls(word, 0)?;
                self.push(handle, result)?;
            }
            0x1b => {
                let acceleration = self.read_operand(handle, a)? as i32;
                let velocity = self.read_operand(handle, b)? as i32;
                let frame_ticks = self.ticks_per_frame.min(0x66);
                let delta = acceleration.wrapping_mul(frame_ticks) / 1024;
                self.push(handle, velocity.wrapping_add(delta) as u32)?;
            }
            0x1c => {
                if self.misc_operation(handle, word, b)? {
                    return Ok(Some(HaltReason::HostEffect));
                }
            }
            0x1d => {
                let scale = self.read_operand(handle, a)? as i32;
                let phase = self.read_operand(handle, b)? as i32;
                if scale == 0 {
                    return Err(VmError::DivisionByZero);
                }
                let angle = phase
                    .wrapping_shl(11)
                    .checked_div(scale)
                    .ok_or(VmError::ArithmeticOverflow)?
                    .wrapping_sub(0x400);
                let sine_adjusted = i32::from(Angle12::new(angle).sin_q12()).wrapping_add(0x1000);
                let shift = if scale > 0xffff { 2 } else { 0 };
                let product = (sine_adjusted >> shift).wrapping_mul(scale);
                self.push(handle, (product >> (13 - shift)) as u32)?;
            }
            0x1e => {
                let value = self.read_operand(handle, a)?;
                let modulus = self.read_operand(handle, b)?.max(1);
                self.push(handle, value.wrapping_add(self.draw_count) % modulus)?;
            }
            0x1f => {
                let index = (self.read_operand(handle, b)? >> 8) as usize;
                let value = self
                    .globals
                    .get(index)
                    .copied()
                    .ok_or(VmError::InvalidRegister(index))?;
                let pool_slot = self
                    .retail_pool_slots_by_global
                    .get(index)
                    .copied()
                    .flatten();
                self.push_with_pool_slot(handle, value, pool_slot)?;
            }
            0x20 => {
                let (value, pool_slot) = self.read_operand_with_pool_slot(handle, a)?;
                let index = (self.read_operand(handle, b)? >> 8) as usize;
                self.set_global_word_with_pool_slot(index, value, pool_slot)?;
            }
            0x21 => {
                let target = Angle12::new(self.read_operand(handle, a)? as i32);
                let current = Angle12::new(self.read_operand(handle, b)? as i32);
                self.push(handle, i32::from(current.difference_to(target)) as u32)?;
            }
            0x22 => {
                // GOP 0xbf0 is the authored two-pop form: speed is on top of
                // target, and B is translated only after both are consumed.
                let (speed, target) = if a == Operand::StackDouble {
                    (self.pop(handle)? as i32, self.pop(handle)? as i32)
                } else {
                    (0x100, self.read_operand(handle, a)? as i32)
                };
                let current = self.read_operand(handle, b)? as i32;
                let speed = speed.checked_abs().ok_or(VmError::ArithmeticOverflow)?;
                self.push(handle, seek(current, target, speed) as u32)?;
            }
            0x23 => {
                let link = ((word >> 12) & 7) as usize;
                let color = ((word >> 15) & 0x3f) as usize;
                let value = if let Some(target) = self.resolve_process_link(handle, link)? {
                    u32::from(self.object(target)?.color(color)?)
                } else {
                    NULL_INPUT_VALUE
                };
                self.push(handle, value)?;
            }
            0x24 => {
                // Retail translates the value before resolving the packed
                // link/color destination. A null pool link is therefore a
                // silent no-op, like all ordinary GOOL output operands.
                let value = self.read_operand(handle, b)? as u16;
                let link = ((word >> 12) & 7) as usize;
                let color = ((word >> 15) & 0x3f) as usize;
                if let Some(target) = self.resolve_process_link(handle, link)? {
                    self.object_mut(target)?.set_color(color, value)?;
                }
            }
            0x25 => {
                // This shares opcode 0x22's authored two-pop form, but uses
                // GoolObjectRotate with the live cooperative frame scale.
                // Speed is on top of target and B translates only afterward.
                let (speed, target) = if a == Operand::StackDouble {
                    (self.pop(handle)? as i32, self.pop(handle)? as i32)
                } else {
                    (0x100, self.read_operand(handle, a)? as i32)
                };
                let current = self.read_operand(handle, b)? as i32;
                let ticks = u32::try_from(self.ticks_per_frame).unwrap_or(0);
                self.push(
                    handle,
                    rotate_toward(current, target, speed, ticks, false, None) as u32,
                )?;
            }
            0x26 => {
                // Retail translates A before B, then pushes B's address and
                // (when non-null) A's address. Tagged storage references keep
                // the same aliasing and stack-pop behavior without exposing
                // native pointers.
                let source = self.input_reference(handle, a)?;
                let destination = self.input_reference(handle, b)?;
                if let Some(destination) = destination {
                    self.push(handle, destination.to_word())?;
                    if let Some(source) = source {
                        self.push(handle, source.to_word())?;
                    }
                }
            }
            0x27 => {
                if b != Operand::Null {
                    let encoded_offset = self.read_operand(handle, a)?;
                    let offset = usize::try_from(encoded_offset >> 6)
                        .map_err(|_| VmError::InvalidAnimationOffset(usize::MAX))?;
                    let animation_len = self.object(handle)?.animation_data.len();
                    if animation_len == 0 {
                        return Err(VmError::AnimationDataUnbound);
                    }
                    let reference = AnimationReference::checked(offset, animation_len)?;
                    self.write_operand(handle, b, reference.to_word())?;
                    self.emit(VmEffect::AnimationSelected {
                        object: handle,
                        reference,
                    })?;
                }
            }
            0x80 | 0x81 => {
                // Retail's interpreter has neither switch case nor a default,
                // so both fetched opcodes are intentional one-cycle no-ops.
                // Authored programs contain 0x80000000 as well as 0x81 words.
            }
            0x82 => {
                return self.control_flow(handle, word, condition);
            }
            0x83 => {
                // `GoolOpChangeAnim` addresses item five as a u32 array: the
                // packed nine-bit selector is therefore a word offset, while
                // AnimationReference stores a checked byte offset.
                let frame = (word & 0x7f) << 8;
                let offset = (((word >> 7) & 0x01ff) as usize)
                    .checked_mul(core::mem::size_of::<u32>())
                    .ok_or(VmError::InvalidAnimationOffset(usize::MAX))?;
                let animation_len = self.object(handle)?.animation_data.len();
                if animation_len == 0 {
                    return Err(VmError::AnimationDataUnbound);
                }
                let reference = AnimationReference::checked(offset, animation_len)?;
                let wait = (word >> 16) & 0x3f;
                let flip = (word >> 22) & 3;
                let frames_elapsed = self.frames_elapsed;

                self.object_mut(handle)?
                    .set_register(process_register::ANIMATION_FRAME, frame)?;
                self.object_mut(handle)?
                    .set_register(process_register::ANIMATION_SEQUENCE, reference.to_word())?;
                self.push(handle, (wait << 24) | frames_elapsed)?;
                self.object_mut(handle)?.animation_wait = Some(AnimationWait {
                    stamp: frames_elapsed,
                    frames: wait as u8,
                });

                let scale_x = self.object(handle)?.register(SCALE_X_REGISTER)? as i32;
                let scale_x = match flip {
                    0 => scale_x.wrapping_abs().wrapping_neg(),
                    1 => scale_x.wrapping_abs(),
                    2 => scale_x.wrapping_neg(),
                    _ => scale_x,
                };
                self.object_mut(handle)?
                    .set_register(SCALE_X_REGISTER, scale_x as u32)?;
                self.emit(VmEffect::AnimationSelected {
                    object: handle,
                    reference,
                })?;
                self.emit(VmEffect::AnimationFrameChanged {
                    object: handle,
                    frame,
                    scale_x,
                    local_bound_refresh: AnimationLocalBoundRefresh::Conditional,
                })?;
                return Ok(Some(HaltReason::AnimationChanged {
                    frame,
                    wait: wait as u8,
                }));
            }
            0x84 => {
                let frame = self.read_operand(handle, b)?;
                let wait = (word >> 16) & 0x3f;
                let flip = (word >> 22) & 3;
                let frames_elapsed = self.frames_elapsed;
                self.object_mut(handle)?
                    .set_register(process_register::ANIMATION_FRAME, frame)?;
                self.push(handle, (wait << 24) | frames_elapsed)?;
                self.object_mut(handle)?.animation_wait = Some(AnimationWait {
                    stamp: frames_elapsed,
                    frames: wait as u8,
                });

                let scale_x = self.object(handle)?.register(SCALE_X_REGISTER)? as i32;
                let scale_x = match flip {
                    0 => scale_x.wrapping_abs().wrapping_neg(),
                    1 => scale_x.wrapping_abs(),
                    2 => scale_x.wrapping_neg(),
                    _ => scale_x,
                };
                self.object_mut(handle)?
                    .set_register(SCALE_X_REGISTER, scale_x as u32)?;
                self.emit(VmEffect::AnimationFrameChanged {
                    object: handle,
                    frame,
                    scale_x,
                    local_bound_refresh: AnimationLocalBoundRefresh::Unconditional,
                })?;
                return Ok(Some(HaltReason::AnimationChanged {
                    frame,
                    wait: wait as u8,
                }));
            }
            0x85 => {
                // Retail translates B once before dispatching the packed
                // transform selector. Retaining its checked address matters:
                // subop one treats the operand as a three-word vector, while
                // stack operands must pop even in subops that ignore B.
                let input = self.input_reference(handle, b)?;
                let suboperation = ((word >> 18) & 7) as u8;
                let input_vector = ((word >> 12) & 7) as u8;
                let output_vector = ((word >> 15) & 7) as u8;
                match suboperation {
                    0 => {
                        if let Some(input) = input {
                            let progress = self.read_storage_reference(input)?;
                            let entity_path = self.entity_path(handle)?;
                            self.object_mut(handle)?.orient_process_vector_on_path(
                                entity_path.as_deref(),
                                progress as i32,
                                input_vector,
                            )?;
                        }
                    }
                    1 => {
                        let input = input.ok_or(VmError::InvalidOperand(instruction.operand_b))?;
                        let camera = self
                            .transform_vectors_camera
                            .ok_or(VmError::TransformVectorsCameraUnbound)?;
                        let perspective = self.object(handle)?.process_vector(input_vector)?;
                        let projected = project_retail_point(perspective, camera);
                        self.object_mut(handle)?
                            .set_process_vector(output_vector, projected)?;

                        // The in/out pointer may alias the output vector, so
                        // read it only after GoolProject has stored `ortho`.
                        let inout_x = self.read_storage_reference(input)? as i32;
                        let z = self.object(handle)?.process_vector(output_vector)?[2] >> 8;
                        if inout_x != 0 && z != 0 {
                            let inout = self.read_storage_span3(input)?.map(|value| value as i32);
                            let scaled = inout.map(|value| value.wrapping_mul(280) / z);
                            self.write_storage_span3(input, scaled.map(|value| value as u32))?;
                        }
                    }
                    2 => {
                        let input = input.ok_or(VmError::InvalidOperand(instruction.operand_b))?;
                        let speed = self.read_storage_reference(input)? as i32;
                        let (mut velocity, target_rotation_x, status_b) = {
                            let object = self.object(handle)?;
                            (
                                object.process_vector(input_vector)?,
                                object.register(process_register::MISC_B_X)? as i32,
                                object.register(process_register::STATUS_B)?,
                            )
                        };
                        let angle = Angle12::new(target_rotation_x);
                        velocity[0] = (i32::from(angle.sin_q12()) / 16).wrapping_mul(speed) >> 8;
                        if status_b & 0x0020_0200 != 0 {
                            velocity[1] =
                                (i32::from(angle.cos_q12()) / 16).wrapping_mul(speed) >> 8;
                        } else {
                            velocity[2] =
                                (i32::from(angle.cos_q12()) / 16).wrapping_mul(speed) >> 8;
                        }
                        let object = self.object_mut(handle)?;
                        object.set_process_vector(input_vector, velocity)?;
                        object.set_register(process_register::SPEED, speed as u32)?;
                    }
                    // Source has no case three; translating B is its only
                    // observable behavior.
                    3 => {}
                    4 | 5 => {
                        let input = input.ok_or(VmError::InvalidOperand(instruction.operand_b))?;
                        let input_z = self.read_storage_reference(input)? as i32;
                        let input_y = self.pop(handle)? as i32;
                        let input_x = self.pop(handle)? as i32;
                        let (translation, rotation, scale) = {
                            let object = self.object(handle)?;
                            (
                                object.process_vector(input_vector)?,
                                object.process_vector(if suboperation == 4 { 1 } else { 4 })?,
                                if suboperation == 4 {
                                    object.process_vector(2)?
                                } else {
                                    [INITIAL_SCALE; 3]
                                },
                            )
                        };
                        let transformed = retail_yxy_transform(
                            Vec3 {
                                x: input_x,
                                y: input_y,
                                z: input_z,
                            },
                            BoundTransform {
                                translation: Vec3 {
                                    x: translation[0],
                                    y: translation[1],
                                    z: translation[2],
                                },
                                rotation: Angles {
                                    y: Angle12::new(rotation[0]),
                                    x: Angle12::new(rotation[1]),
                                    z: Angle12::new(rotation[2]),
                                },
                                scale: Vec3 {
                                    x: scale[0],
                                    y: scale[1],
                                    z: scale[2],
                                },
                            },
                        );
                        self.object_mut(handle)?.set_process_vector(
                            output_vector,
                            [transformed.x, transformed.y, transformed.z],
                        )?;
                    }
                    6 => {
                        let input = input.ok_or(VmError::InvalidOperand(instruction.operand_b))?;
                        let vertex_index = self.read_storage_reference(input)? >> 8;
                        let link_index = ((word >> 21) & 7) as u8;
                        let link = self
                            .resolve_process_link(handle, usize::from(link_index))?
                            .ok_or(VmError::MissingLink {
                                object: handle,
                                link: link_index,
                            })?;
                        let Some(source) = self.animation_source(link)? else {
                            return Ok(None);
                        };
                        let model_eid = match source {
                            AnimationSource::ItemFive(reference) => {
                                let descriptor = parse_gool_animation_descriptor(
                                    &self.object(link)?.animation_data,
                                    reference.offset() as usize,
                                )
                                .map_err(|_| {
                                    VmError::InvalidAnimationReference(reference.to_word())
                                })?;
                                let GoolAnimationDescriptor::Vertex(vertex) = descriptor else {
                                    return Ok(None);
                                };
                                vertex.model_eid
                            }
                            AnimationSource::Process(reference) => match reference.kind() {
                                ProcessAnimationKind::Vertex(vertex) => vertex.model_eid,
                                ProcessAnimationKind::NoDraw
                                | ProcessAnimationKind::Sprite(_)
                                | ProcessAnimationKind::Font(_)
                                | ProcessAnimationKind::Text(_)
                                | ProcessAnimationKind::Fragment(_) => return Ok(None),
                            },
                        };
                        let frame_index = self.object(link)?.animation_frame() >> 8;
                        self.emit(VmEffect::TransformModelVertex {
                            requester: handle,
                            link,
                            // Suboperation six is the one transform form whose
                            // destination occupies bits 12..14 (`trans_idx` in
                            // the shared decode), not bits 15..17.
                            output_vector: input_vector,
                            model_eid,
                            frame_index,
                            vertex_index,
                        })?;
                        return Ok(Some(HaltReason::HostEffect));
                    }
                    7 => {
                        let camera = self
                            .transform_vectors_camera
                            .ok_or(VmError::TransformVectorsCameraUnbound)?;
                        let (vector, prior_output) = {
                            let object = self.object(handle)?;
                            (
                                object.process_vector(input_vector)?,
                                object.process_vector(output_vector)?,
                            )
                        };
                        let transformed =
                            transform_retail_audio_point(vector, prior_output, camera);
                        self.object_mut(handle)?
                            .set_process_vector(output_vector, transformed)?;
                    }
                    _ => unreachable!(),
                }
            }
            0x86 => {
                let argument_count = ((word >> 20) & 0x0f) as usize;
                let target = (word & 0x3fff) as usize;
                self.call_global(handle, target, argument_count)?;
            }
            0x87 | 0x8f | 0x90 => {
                return self.execute_send_event_opcode(
                    handle,
                    word,
                    instruction.opcode,
                    b,
                    return_link_halt,
                );
            }
            0x88 | 0x89 => {
                return self.event_service_return(handle, word, instruction.opcode);
            }
            0x8a | 0x91 => {
                if self.spawn_children(handle, word, instruction.opcode == 0x91)? {
                    return Ok(Some(HaltReason::HostEffect));
                }
            }
            0x8b => {
                if self.paging_operation(handle, a, b)? {
                    return Ok(Some(HaltReason::HostEffect));
                }
            }
            0x8c => {
                // Native code dereferences A before translating B, then
                // synchronously stores AudioVoiceCreate's signed result in
                // process.voice_id. Retain both checked source addresses and
                // stop before the following GOOL instruction.
                let volume_source = self
                    .input_reference(handle, a)?
                    .ok_or(VmError::MissingAudioVoiceCreateVolume)?;
                let volume = self.read_storage_reference(volume_source)? as i32;
                let adio_source = self
                    .input_reference(handle, b)?
                    .ok_or(VmError::MissingAudioVoiceCreateAdio)?;
                let adio = self.resolve_audio_entry_argument(adio_source)?;
                let request = AudioVoiceCreateRequest {
                    object: handle,
                    volume_source,
                    volume,
                    adio_source,
                    adio,
                };
                // Keep the legacy observation queue populated while the
                // typed synchronous request becomes the authoritative path.
                self.emit(VmEffect::AudioStart {
                    object: handle,
                    voice: volume as u32,
                    sound: adio.raw(),
                })?;
                self.begin_audio_host_request(AudioHostRequest::CreateVoice(request))?;
                return Ok(Some(HaltReason::HostEffect));
            }
            0x8d => {
                // B is a generic-union pointer and is translated before the
                // packed voice selector. In particular, B=stack must pop
                // before selector 0x1f performs its independent voice-id pop.
                let argument_source = self.input_reference(handle, b)?;
                let operation = AudioControlOperation::decode(word);
                let voice_selector = ((word >> 12) & 0x3f) as u8;
                let voice = self.decode_audio_voice_selector(handle, voice_selector)?;
                let argument = self.decode_audio_control_argument(operation, argument_source)?;
                let request = AudioControlRequest {
                    object: handle,
                    voice,
                    operation,
                    argument_source,
                    argument,
                };
                self.emit(VmEffect::AudioControl {
                    object: handle,
                    command: operation.native_control_word(),
                    value: argument.compatibility_word(),
                })?;
                self.begin_audio_host_request(AudioHostRequest::Control(request))?;
                return Ok(Some(HaltReason::HostEffect));
            }
            0x8e => {
                let suboperation = ((word >> 18) & 7) as u8;
                let input_vector = ((word >> 12) & 7) as u8;
                let output_vector = ((word >> 15) & 7) as u8;
                // The source interpreter translates B once through each of
                // the independent input/output constant cursors before the
                // solid-surface switch, even for color-scale suboperation 6.
                // Preserve those shared-buffer alias and stack-SP effects.
                let _ = self.input_reference(handle, b)?;
                let output_reference = self.output_reference(handle, b)?;
                if suboperation == 6 {
                    let level = self.globals.get(CURRENT_LEVEL_GLOBAL).map(|word| word >> 8);
                    self.object_mut(handle)?
                        .scale_colors_for_entity_node(level)?;
                } else if suboperation == 0 {
                    self.react_solid_surface_suboperation_zero(
                        handle,
                        input_vector,
                        output_vector,
                        output_reference,
                    )?;
                } else if suboperation == 1 {
                    self.react_solid_surface_suboperation_one(
                        handle,
                        input_vector,
                        output_vector,
                        output_reference,
                    )?;
                } else if (2..=5).contains(&suboperation) {
                    self.react_solid_surface_directional(
                        handle,
                        suboperation,
                        input_vector,
                        output_vector,
                        output_reference,
                    )?;
                } else {
                    // Source has no case seven. Translating B through both
                    // independent cursors above is its only observable work.
                }
            }
            opcode => return Err(VmError::UnknownOpcode(opcode)),
        }
        Ok(None)
    }

    fn find_retail_solid_object_node<F>(
        &self,
        environment: &RetailSolidEnvironment,
        translation: [i32; 3],
        flags: u8,
        padding: i32,
        predicate: F,
    ) -> Result<(RetailSolidHit, [i32; 3]), VmError>
    where
        F: Fn(u32) -> bool,
    {
        let original = translation;
        let (node, mut nearest) = find_retail_solid_node(environment, translation, flags, 0)?;
        let mut highest_object = None;
        let mut highest_y = RETAIL_SOLID_INITIAL_Y_MAX;

        for (bound_index, snapshot) in self.solid_frame_bounds.iter().enumerate() {
            let Some(incarnation) = self
                .solid_frame_bound_incarnations
                .get(bound_index)
                .copied()
            else {
                continue;
            };
            if !self.incarnation_is_live(snapshot.object, incarnation) {
                continue;
            }
            let candidate = self.object(snapshot.object)?;
            if !predicate(candidate.register(process_register::STATUS_B)?) {
                continue;
            }
            let bound = snapshot.bound;
            let within_padded_xz = original[0] >= bound.min.x.wrapping_sub(padding)
                && original[0] <= bound.max.x.wrapping_add(padding)
                && original[2] >= bound.min.z.wrapping_sub(padding)
                && original[2] <= bound.max.z.wrapping_add(padding);
            if !within_padded_xz {
                continue;
            }
            if original[1] >= bound.min.y && original[1] <= bound.max.y {
                nearest[1] = bound.max.y;
                return Ok((RetailSolidHit::Object(snapshot.object), nearest));
            }
            if bound.max.y > highest_y && original[1] >= bound.max.y {
                highest_object = Some(snapshot.object);
                highest_y = bound.max.y;
            }
        }

        if let Some(object) = highest_object
            && (node.is_none() || highest_y >= nearest[1])
        {
            nearest[1] = highest_y;
            return Ok((RetailSolidHit::Object(object), nearest));
        }
        Ok((
            node.map_or(RetailSolidHit::None, RetailSolidHit::Node),
            nearest,
        ))
    }

    fn find_retail_solid_object_node_three(
        &self,
        query: ObjectHandle,
        environment: &RetailSolidEnvironment,
        translation: [i32; 3],
        flags: u8,
    ) -> Result<RetailSolidHit, VmError> {
        let mut original = translation;
        original[1] = original[1]
            .checked_add(25_000)
            .ok_or(VmError::ArithmeticOverflow)?;
        let (node, nearest) = find_retail_solid_node(environment, translation, flags & 3, 25_000)?;
        let mut nearest_object = None;
        let mut nearest_axis = RETAIL_SOLID_INITIAL_Y_MAX;

        for (bound_index, snapshot) in self.solid_frame_bounds.iter().enumerate() {
            let Some(incarnation) = self
                .solid_frame_bound_incarnations
                .get(bound_index)
                .copied()
            else {
                continue;
            };
            if !self.incarnation_is_live(snapshot.object, incarnation) {
                continue;
            }
            if snapshot.object == query {
                continue;
            }
            let candidate = self.object(snapshot.object)?;
            // This is the exact source sentinel. Negative NODE values carry
            // a valid subtype plus the no-seek bit and must remain eligible.
            if candidate.register(process_register::NODE)? == 0xffff {
                continue;
            }
            let bound = snapshot.bound;
            if flags & 1 != 0 {
                if original[0] < bound.min.x
                    || original[0] > bound.max.x
                    || original[2] < bound.min.z
                    || original[2] > bound.max.z
                {
                    continue;
                }
                if original[1] >= bound.min.y && original[1] <= bound.max.y {
                    // Ordered direct overlaps win immediately, even when an
                    // octree leaf is closer on the selected axis.
                    return Ok(RetailSolidHit::Object(snapshot.object));
                }
                if bound.max.y > nearest_axis && original[1] >= bound.max.y {
                    nearest_object = Some(snapshot.object);
                    nearest_axis = bound.max.y;
                }
            } else if flags & 2 != 0 {
                if original[0] < bound.min.x
                    || original[0] > bound.max.x
                    || original[1] < bound.min.y
                    || original[1] > bound.max.y
                {
                    continue;
                }
                // Native's upper Z comparison is uniquely exclusive here.
                if original[2] >= bound.min.z && original[2] < bound.max.z {
                    return Ok(RetailSolidHit::Object(snapshot.object));
                }
                if bound.max.z > nearest_axis && original[2] >= bound.max.z {
                    nearest_object = Some(snapshot.object);
                    nearest_axis = bound.max.z;
                }
            }
        }

        if let Some(candidate) = nearest_object {
            let axis = if flags & 1 != 0 { 1 } else { 2 };
            if node.is_none() || nearest_axis >= nearest[axis] {
                return Ok(RetailSolidHit::Object(candidate));
            }
        }
        Ok(node.map_or(RetailSolidHit::None, RetailSolidHit::Node))
    }

    fn apply_retail_shadow_parent_size(
        &mut self,
        query: ObjectHandle,
        parent: ObjectHandle,
        hit: RetailSolidHit,
    ) -> Result<(), VmError> {
        let increment = if self.object(parent)?.state_flags & 0x10 != 0 {
            0
        } else {
            0x18_u32
        };
        let size = match hit {
            RetailSolidHit::None => increment,
            RetailSolidHit::Node(node) => {
                let index = usize::from((node & 0x3c00) >> 10);
                increment.wrapping_add(RETAIL_SIZE_MAP[index] as u32)
            }
            RetailSolidHit::Object(candidate) => {
                let mut candidate_size =
                    self.object(candidate)?.register(process_register::SIZE)?;
                let query_y =
                    self.object(query)?
                        .register(process_register::TRANSLATION_Y)? as i32;
                let camera_y = self.camera_translation[1];
                if query_y > camera_y {
                    candidate_size =
                        candidate_size.wrapping_sub((query_y.wrapping_sub(camera_y) >> 12) as u32);
                }
                increment.wrapping_add(candidate_size)
            }
        };
        self.object_mut(parent)?
            .set_register(process_register::SIZE, size)
    }

    fn react_solid_surface_suboperation_one(
        &mut self,
        handle: ObjectHandle,
        input_vector: u8,
        output_vector: u8,
        output_reference: Option<StorageReference>,
    ) -> Result<(), VmError> {
        // Like every source case, suboperation one materializes this copy even
        // though it never subsequently consumes it.
        let _input = self.object(handle)?.process_vector(input_vector)?;
        let translation = self.object(handle)?.process_vector(0)?;
        let environment = self
            .current_solid_environment
            .clone()
            .ok_or(VmError::MissingSolidEnvironment(handle))?;
        let parent = self.resolve_process_link(handle, 1)?;
        let player_related = if self.object(handle)?.is_main_player {
            true
        } else if let Some(parent) = parent {
            self.object(parent)?.is_main_player
        } else {
            false
        };

        // Crash and its direct children first run the alternate retail query
        // on a throwaway transform. Its ordered AABB scan updates only the
        // parent's shadow size; suboperation one discards the hit and vector.
        if player_related
            && self.object(handle)?.register(process_register::STATUS_B)? & 0x0400_0000 != 0
        {
            let (hit, _) = self.find_retail_solid_object_node(
                &environment,
                translation,
                1,
                35_000,
                |status| status & 0x4002_0000 == 0x0002_0000,
            )?;
            let parent = parent.ok_or(VmError::MissingLink {
                object: handle,
                link: 1,
            })?;
            self.apply_retail_shadow_parent_size(handle, parent, hit)?;
        }

        // The second query is unconditional, writes its selected Y back into
        // `trans`, and excludes type-three leaves through flag 8. Its result is
        // stored in the overlapping `misc_node` word before either vector
        // output. Object hits use a collision-only checked handle tag.
        let (hit, nearest) =
            self.find_retail_solid_object_node(&environment, translation, 9, 20_000, |status| {
                status & 0x0002_0000 != 0
            })?;
        self.object_mut(handle)?
            .set_register(process_register::MISC_VALUE, hit.to_word())?;
        if let Some(reference) = output_reference {
            self.write_storage_span3(reference, self.solid_trans3.map(|value| value as u32))?;
        }
        self.object_mut(handle)?
            .set_process_vector(output_vector, nearest)
    }

    fn react_solid_surface_suboperation_zero(
        &mut self,
        handle: ObjectHandle,
        input_vector: u8,
        output_vector: u8,
        output_reference: Option<StorageReference>,
    ) -> Result<(), VmError> {
        let direction = self.object(handle)?.process_vector(input_vector)?;
        let mut translation = self.object(handle)?.process_vector(0)?;
        translation[1] = translation[1].wrapping_add(self.object(handle)?.local_bound.max.y / 2);
        let environment = self
            .current_solid_environment
            .clone()
            .ok_or(VmError::MissingSolidEnvironment(handle))?;
        let (node, translation, direction) =
            retail_rebound_vector(&environment, translation, direction)?;
        self.object_mut(handle)?
            .set_register(process_register::MISC_VALUE, u32::from(node))?;
        if let Some(reference) = output_reference {
            self.write_storage_span3(reference, direction.map(|value| value as u32))?;
        }
        self.object_mut(handle)?
            .set_process_vector(output_vector, translation)
    }

    fn react_solid_surface_directional(
        &mut self,
        handle: ObjectHandle,
        suboperation: u8,
        input_vector: u8,
        output_vector: u8,
        output_reference: Option<StorageReference>,
    ) -> Result<(), VmError> {
        // The source copies this vector before entering the switch even though
        // directional suboperations two through five do not consume it.
        let _input = self.object(handle)?.process_vector(input_vector)?;
        let translation = self.object(handle)?.process_vector(0)?;
        // `trans3` is static in C, but this case overwrites it on every call.
        // `ZoneFindNearestObjectNode3` mutates only a local copy and never
        // writes through its vector pointer, so the final vector is exactly
        // the current translation.
        self.solid_trans3 = translation;

        if self.object(handle)?.register(process_register::STATUS_B)? & 0x0400_0000 != 0 {
            let environment = self
                .current_solid_environment
                .clone()
                .ok_or(VmError::MissingSolidEnvironment(handle))?;
            let parent_colors = matches!(suboperation, 3 | 5);
            let seek_colors = matches!(suboperation, 2 | 3);
            let flags = if environment.graphics_flags & 1 != 0 {
                if parent_colors { 6 } else { 2 }
            } else {
                if parent_colors { 5 } else { 1 }
            };
            let active_parent = if parent_colors {
                let parent = self
                    .resolve_process_link(handle, 1)?
                    .ok_or(VmError::MissingLink {
                        object: handle,
                        link: 1,
                    })?;
                (self.object(parent)?.register(process_register::STATUS_B)? & 0x0400_0000 != 0)
                    .then_some(parent)
            } else {
                None
            };

            // Native helper three gates parent-color selectors on the
            // parent's active bit, while the two non-color selectors always
            // perform their directional query for an active child.
            if !parent_colors || active_parent.is_some() {
                let hit = self.find_retail_solid_object_node_three(
                    handle,
                    &environment,
                    translation,
                    flags,
                )?;

                if let Some(parent) = active_parent {
                    let color_environment = self
                        .object(parent)?
                        .solid_environment
                        .clone()
                        .ok_or(VmError::MissingSolidEnvironment(parent))?;
                    let use_player_colors =
                        self.object(handle)?.is_main_player || self.object(parent)?.is_main_player;
                    let source = if use_player_colors {
                        color_environment.player_colors
                    } else {
                        color_environment.object_colors
                    };
                    let (mut subtype, object_allows_seek) = match hit {
                        RetailSolidHit::None => (-1, true),
                        RetailSolidHit::Node(node) => {
                            let subtype = i32::from((node & 0x03f0) >> 4);
                            (if subtype < 39 { -1 } else { subtype }, true)
                        }
                        RetailSolidHit::Object(candidate) => {
                            let node =
                                self.object(candidate)?.register(process_register::NODE)? as i32;
                            if node < 0 {
                                (
                                    node.checked_neg()
                                        .ok_or(VmError::InvalidColorSubtype(node))?,
                                    false,
                                )
                            } else {
                                (node, true)
                            }
                        }
                    };
                    if self.object(parent)?.is_main_player
                        && self.object(parent)?.state_flags & 0x20 != 0
                    {
                        subtype = 0x37;
                        // The Crash override changes only the subtype. A
                        // negative object NODE has already cleared seek.
                    }
                    let level = self.globals.get(CURRENT_LEVEL_GLOBAL).map(|word| word >> 8);
                    let target = scaled_retail_colors(&source, subtype, level)?;
                    let step = if seek_colors && object_allows_seek {
                        0x015e
                    } else {
                        0
                    };
                    seek_retail_colors(&mut self.object_mut(parent)?.colors, target, step);
                }
            }
        }

        // `trans4` is the other C static. Nothing assigns it anywhere, so its
        // language-defined static initialization remains a zero vector. The
        // source stores it before storing `trans3`; preserve that overlap order.
        if let Some(reference) = output_reference {
            self.write_storage_span3(reference, self.solid_trans4.map(|value| value as u32))?;
        }
        let trans3 = self.solid_trans3;
        self.object_mut(handle)?
            .set_process_vector(output_vector, trans3)
    }

    fn test_controls(&self, instruction: u32, port: usize) -> Result<u32, VmError> {
        let pad = self.pad_snapshot(port)?;
        let button_mask = instruction & 0x0fff;
        let test_type = (instruction >> 12) & 3;
        let direction_test_type = (instruction >> 14) & 3;
        let direction_test = (instruction >> 16) & 0x0f;
        let negate = (instruction >> 20) & 1;

        // Preserve the integer mask returned by the C interpreter. A button
        // success is not normalized to one: CROSS, for example, returns 0x40.
        let mut condition = match test_type {
            0 => 1,
            1 => pad.tapped & 0x0fff & button_mask,
            2 => pad.held & 0x0fff & button_mask,
            // The second `test_type == 3` branch in the retail source is
            // unreachable. The shipped behavior is the two-frame tapped
            // test represented here, not a two-frame held test.
            3 => button_mask & (pad.tapped | pad.tapped_previous),
            _ => unreachable!(),
        };
        if condition == 0 {
            return Ok(negate);
        }
        if direction_test_type == 0 {
            return Ok(condition ^ negate);
        }

        condition = match direction_test_type {
            1 if (9..=12).contains(&direction_test) => pad.tapped & (1_u32 << (direction_test + 3)),
            1 => {
                let direction = MOVE_DIRECTIONS[((pad.held >> 12) & 0x0f) as usize];
                let previous = MOVE_DIRECTIONS[((pad.held_previous >> 12) & 0x0f) as usize];
                u32::from(direction == direction_test && direction != previous)
            }
            2 if (9..=12).contains(&direction_test) => pad.held & (1_u32 << (direction_test + 3)),
            2 => {
                let direction = MOVE_DIRECTIONS[((pad.held >> 12) & 0x0f) as usize];
                u32::from(direction == direction_test)
            }
            3 => {
                let direction = MOVE_DIRECTIONS[((pad.held >> 12) & 0x0f) as usize];
                let previous = MOVE_DIRECTIONS[((pad.held_previous >> 12) & 0x0f) as usize];
                u32::from(direction == direction_test || previous == direction_test)
            }
            _ => unreachable!(),
        };
        Ok(condition ^ negate)
    }

    fn begin_audio_host_request(&mut self, request: AudioHostRequest) -> Result<(), VmError> {
        if let Some(pending) = self.pending_audio_host_request {
            return Err(VmError::AudioHostRequestPending(pending.object()));
        }
        self.pending_audio_host_request = Some(request);
        Ok(())
    }

    fn resolve_audio_entry_argument(&self, reference: StorageReference) -> Result<Eid, VmError> {
        let raw = self.read_storage_reference(reference)?;
        let eid = Eid::from_raw(raw);
        if eid.is_named() {
            return Ok(eid);
        }
        let entry =
            EntryReference::from_word(raw).ok_or(VmError::InvalidAudioEntryReference(raw))?;
        self.paging_entry_references
            .get(entry.slot as usize)
            .map(|(eid, _)| *eid)
            .ok_or(VmError::InvalidAudioEntryReference(raw))
    }

    fn decode_audio_voice_selector(
        &mut self,
        handle: ObjectHandle,
        selector: u8,
    ) -> Result<AudioVoiceSelector, VmError> {
        match selector {
            0 => Ok(AudioVoiceSelector::Template),
            0x1f => Ok(AudioVoiceSelector::Stack {
                voice_id: self.pop(handle)? as i32,
            }),
            register => Ok(AudioVoiceSelector::ProcessRegister {
                register,
                voice_id: self.read_process_register_reference(handle, usize::from(register))?
                    as i32,
            }),
        }
    }

    fn decode_audio_object_argument(
        &self,
        reference: StorageReference,
    ) -> Result<Option<ObjectHandle>, VmError> {
        if reference.region == StorageRegion::Register && reference.index < 8 {
            let (raw, pool_slot) = self.read_storage_reference_with_pool_slot(reference)?;
            if raw == 0 {
                return Ok(None);
            }
            let target = CollisionObjectReference::from_word(raw)
                .ok_or(VmError::InvalidAudioObjectReference(raw))?;
            return self
                .resolve_pool_backed_object_reference(target, pool_slot)
                .map(Some);
        }

        let (raw, pool_slot) = self.read_storage_reference_with_pool_slot(reference)?;
        if raw == 0 {
            return Ok(None);
        }
        let reference = CollisionObjectReference::from_word(raw)
            .ok_or(VmError::InvalidAudioObjectReference(raw))?;
        self.resolve_pool_backed_object_reference(reference, pool_slot)
            .map(Some)
    }

    fn decode_audio_control_argument(
        &self,
        operation: AudioControlOperation,
        source: Option<StorageReference>,
    ) -> Result<AudioControlArgument, VmError> {
        let required_source =
            || source.ok_or(VmError::MissingAudioControlArgument(operation.suboperation));
        match operation.effective_suboperation() {
            0 | 1 | 6 => {
                let value = self.read_storage_reference(required_source()?)? as i32;
                Ok(AudioControlArgument::Scalar(AudioScalarArgument::Signed(
                    value,
                )))
            }
            2 | 3 => {
                let vector = self
                    .read_storage_span3(required_source()?)?
                    .map(|word| word as i32);
                Ok(AudioControlArgument::Vector(vector))
            }
            4 | 12 => {
                let value = self.read_storage_reference(required_source()?)? as u8 as i8;
                Ok(AudioControlArgument::Scalar(
                    AudioScalarArgument::SignedByte(value),
                ))
            }
            5 => Ok(AudioControlArgument::Object(
                self.decode_audio_object_argument(required_source()?)?,
            )),
            7 | 10 | 11 => {
                let value = self.read_storage_reference(required_source()?)?;
                Ok(AudioControlArgument::Scalar(AudioScalarArgument::Unsigned(
                    value,
                )))
            }
            8 | 9 | 13 | 14 => Ok(AudioControlArgument::Unused),
            _ => unreachable!("effective audio suboperation is four bits and never fifteen"),
        }
    }

    fn paging_operation(
        &mut self,
        handle: ObjectHandle,
        operation_operand: Operand,
        argument_operand: Operand,
    ) -> Result<bool, VmError> {
        let operation = self.read_operand(handle, operation_operand)?;
        // `GoolOpPaging` retains the address produced by GOP translation. It
        // does not first dereference B like an ordinary scalar opcode.
        let argument = self.input_reference(handle, argument_operand)?;
        match operation {
            1 | 6 => {
                let reference = argument.ok_or(VmError::InvalidPagingOperation(operation))?;
                let (entry, page) = self.resolve_entry_argument(reference)?;
                let (eid, _) = self.entry_reference_identity(entry)?;
                let was_resolved = self.paging_resolved_pages.contains(&page);
                self.paging_loaded_pages.insert(page);
                self.paging_resolved_pages.insert(page);
                let count = self.paging_page_references.entry(page).or_default();
                *count = count.saturating_add(1);
                // Native `misc_entry` is represented by the checked logical
                // entry token rather than a relocated host pointer or a
                // reference back to the mutable source EID cell.
                self.object_mut(handle)?
                    .set_register(process_register::MISC_VALUE, entry.to_word())?;
                self.emit(VmEffect::Paging {
                    object: handle,
                    operation: PagingHostOperation::Open,
                    physical: operation == 6,
                    reference: entry.to_word(),
                    eid,
                    page,
                    was_resolved,
                })?;
                return Ok(true);
            }
            2 => {
                let reference = argument.ok_or(VmError::InvalidPagingOperation(operation))?;
                let (entry, page) = self.resolve_entry_argument(reference)?;
                let (eid, _) = self.entry_reference_identity(entry)?;
                let was_resolved = self.paging_resolved_pages.contains(&page);
                let result = self.close_paging_page(page, true);
                self.object_mut(handle)?
                    .set_register(process_register::MISC_VALUE, result)?;
                self.emit(VmEffect::Paging {
                    object: handle,
                    operation: PagingHostOperation::Close,
                    physical: false,
                    reference: entry.to_word(),
                    eid,
                    page,
                    was_resolved,
                })?;
                return Ok(true);
            }
            3 => {
                let reference = argument.ok_or(VmError::InvalidPagingOperation(operation))?;
                let (entry, page) = self.resolve_entry_argument(reference)?;
                let (eid, _) = self.entry_reference_identity(entry)?;
                let was_resolved = self.paging_resolved_pages.contains(&page);
                self.emit(VmEffect::Paging {
                    object: handle,
                    operation: PagingHostOperation::Probe,
                    physical: false,
                    reference: entry.to_word(),
                    eid,
                    page,
                    was_resolved,
                })?;
                // `NSClose(ref, 0)` is a query: resolved PC PTEs return
                // literal one; unresolved pages return zero. It does not
                // decrement the count.
                let result = self.close_paging_page(page, false);
                self.push(handle, result)?;
                return Ok(true);
            }
            4 => self.push(handle, self.available_page_count())?,
            5 => {
                let reference = argument.ok_or(VmError::InvalidPagingOperation(operation))?;
                let count = usize::try_from(self.read_storage_reference(reference)?)
                    .map_err(|_| VmError::InvalidPagingOperation(operation))?;
                let stack_len = self.object(handle)?.stack.len();
                let start = stack_len
                    .checked_sub(count)
                    .ok_or(VmError::StackUnderflow(handle))?;
                let words = self.object(handle)?.stack[start..].to_vec();
                let mut required_entries = 0_u32;
                for word in words {
                    let storage = StorageReference::from_word(word)
                        .ok_or(VmError::InvalidStorageReference(word))?;
                    let entry_word = self.read_storage_reference(storage)?;
                    let (page, resolved) =
                        if let Some(entry) = EntryReference::from_word(entry_word) {
                            let page = self.entry_reference_page(entry)?;
                            (page, self.paging_resolved_pages.contains(&page))
                        } else {
                            if StorageReference::from_word(entry_word).is_some() {
                                return Err(VmError::InvalidStorageReference(entry_word));
                            }
                            let eid = Eid::from_raw(entry_word);
                            let page = self
                                .entry_pages
                                .get(&eid)
                                .copied()
                                .ok_or(VmError::MissingEntryReferencePage(eid))?;
                            (page, self.paging_resolved_pages.contains(&page))
                        };
                    // The resolved-PTE/type-1 branch counts every requested
                    // entry, with no page deduplication. Physically resident
                    // but untranslated pages remain in the raw-EID branch and
                    // contribute zero until NSOpen resolves their offsets.
                    if resolved {
                        debug_assert!(self.paging_loaded_pages.contains(&page));
                        required_entries = required_entries.saturating_add(1);
                    }
                }
                self.object_mut(handle)?.stack.truncate(start);
                self.push(handle, required_entries)?;
            }
            _ => return Err(VmError::InvalidPagingOperation(operation)),
        }
        Ok(false)
    }

    fn close_paging_page(&mut self, page: PageIndex, decrement: bool) -> u32 {
        if self.paging_pending_pages.contains(&page) {
            if decrement {
                let count = self.paging_page_references.entry(page).or_default();
                if *count != 0 {
                    *count -= 1;
                }
                if *count == 0 {
                    self.paging_pending_pages.remove(&page);
                }
            }
            // A state-two virtual page still presents an unresolved odd pgid.
            return 0;
        }
        // `NSClose(ref, 0)` observes the PTE, not the immutable stream bytes.
        // A catalog page may be known/loaded by the browser while its entry
        // offsets are re-armed after physical eviction; that unresolved PTE
        // must return zero until another `NSOpen` translates it.
        if !self.paging_resolved_pages.contains(&page) {
            return 0;
        }
        // Copied texture/audio PTEs carry native bit two. Count-zero probes
        // return one before the tag test, but a positive close returns zero and
        // must not consume the copied page's stranded reference count.
        if decrement && self.paging_uncounted_pages.contains(&page) {
            return 0;
        }
        let count = self.paging_page_references.entry(page).or_default();
        if decrement && *count != 0 {
            *count -= 1;
        }
        // Resolved PC-source PTEs return literal one for count=0; type-1 NSF
        // pages also remain resident when count=1 decrements to zero.
        1
    }

    fn available_page_count(&self) -> u32 {
        let referenced = self
            .paging_page_references
            .iter()
            .filter(|(page, references)| {
                **references != 0 && !self.paging_uncounted_pages.contains(page)
            })
            .count() as u32;
        self.paging_page_capacity.saturating_sub(referenced)
    }

    fn peek_process_register_reference(
        &self,
        handle: ObjectHandle,
        register: usize,
    ) -> Result<u32, VmError> {
        // `gool_process.regs` aliases the eight leading link pointers. Their
        // checked tags are ordinary raw register words here, so VM slot zero
        // is nonzero while scalar union values stay intact.
        if register == 0x1f {
            self.object(handle)?
                .stack
                .last()
                .copied()
                .ok_or(VmError::StackUnderflow(handle))
        } else {
            self.read_aliased_process_register(handle, register)
        }
    }

    fn read_aliased_process_register_with_pool_slot(
        &self,
        handle: ObjectHandle,
        register: usize,
    ) -> Result<(u32, Option<u8>), VmError> {
        let object = self.object(handle)?;
        let raw = object.register(register)?;
        let (value, retained) = match register {
            // Instruction fetch advances the typed PC before opcode operands
            // are translated, matching native `*pc++` visibility to GOOL.
            process_register::PROGRAM_COUNTER => (
                if object.halted {
                    0
                } else {
                    object.code_address().to_word()
                },
                None,
            ),
            process_register::TRANSITION_POINTER => (
                object.transition_address.map_or(0, CodeAddress::to_word),
                None,
            ),
            _ => (raw, object.register_pool_slot(register)?),
        };
        Ok((value, self.live_pool_slot_for_word(value, retained)))
    }

    fn read_aliased_process_register(
        &self,
        handle: ObjectHandle,
        register: usize,
    ) -> Result<u32, VmError> {
        self.read_aliased_process_register_with_pool_slot(handle, register)
            .map(|(value, _)| value)
    }

    fn write_aliased_process_register_with_pool_slot(
        &mut self,
        handle: ObjectHandle,
        register: usize,
        value: u32,
        pool_slot: Option<u8>,
    ) -> Result<(), VmError> {
        let code_address = match register {
            process_register::PROGRAM_COUNTER | process_register::TRANSITION_POINTER
                if value != 0 =>
            {
                Some(self.object(handle)?.checked_code_address(value)?)
            }
            _ => None,
        };
        let object = self.object_mut(handle)?;
        object.set_register_with_pool_slot(register, value, pool_slot)?;
        match register {
            process_register::PROGRAM_COUNTER => {
                if let Some(address) = code_address {
                    object.code_segment = address.segment;
                    object.pc = address.pc;
                    object.halted = false;
                } else {
                    object.halted = true;
                }
            }
            process_register::TRANSITION_POINTER => {
                object.transition_address = code_address;
            }
            _ => {}
        }
        Ok(())
    }

    fn read_process_register_reference(
        &mut self,
        handle: ObjectHandle,
        register: usize,
    ) -> Result<u32, VmError> {
        if register == 0x1f {
            self.pop(handle)
        } else {
            self.peek_process_register_reference(handle, register)
        }
    }

    fn event_service_return(
        &mut self,
        handle: ObjectHandle,
        instruction: u32,
        opcode: u8,
    ) -> Result<Option<HaltReason>, VmError> {
        let condition_type = ((instruction >> 20) & 3) as u8;
        let return_type = ((instruction >> 22) & 3) as u8;
        let register = ((instruction >> 14) & 0x3f) as usize;
        let previous_condition = match self.object(handle)?.call_stack.last() {
            Some(CallFrame {
                behavior: ReturnBehavior::EventService { condition, .. },
                ..
            }) => *condition,
            _ => {
                return Err(VmError::UnsupportedEventServiceReturn {
                    opcode,
                    condition_type,
                    return_type,
                    register: register as u8,
                });
            }
        };

        let pops_condition = matches!(condition_type, 1 | 2) && register == 0x1f;
        let tested = match condition_type {
            0 => true,
            1 | 2 => {
                let value = self.peek_process_register_reference(handle, register)?;
                if condition_type == 1 {
                    value != 0
                } else {
                    value == 0
                }
            }
            3 => previous_condition,
            _ => unreachable!(),
        };
        let branch_argument_count = if !tested && return_type == 0 {
            ((instruction >> 10) & 0x0f) as usize
        } else {
            0
        };
        let required = branch_argument_count + usize::from(pops_condition);
        let object = self.object(handle)?;
        let stack_origin = object.initial_stack_pointer as usize;
        let frame_floor = object
            .frame_base
            .checked_sub(stack_origin)
            .and_then(|frame| frame.checked_add(3))
            .ok_or(VmError::StackUnderflow(handle))?;
        let available = object
            .stack
            .len()
            .checked_sub(frame_floor)
            .ok_or(VmError::StackUnderflow(handle))?;
        if available < required {
            return Err(VmError::StackUnderflow(handle));
        }
        if matches!(condition_type, 1 | 2) {
            let value = self.read_process_register_reference(handle, register)?;
            debug_assert_eq!(
                tested,
                if condition_type == 1 {
                    value != 0
                } else {
                    value == 0
                }
            );
        }
        {
            let frame = self
                .object_mut(handle)?
                .call_stack
                .last_mut()
                .expect("validated event-service frame remains present");
            let ReturnBehavior::EventService {
                condition,
                return_event,
                guard,
                ..
            } = &mut frame.behavior
            else {
                unreachable!("validated event-service frame remains on top");
            };
            *condition = tested;
            if tested {
                *return_event = true;
                *guard = opcode == 0x89;
            }
        }

        if tested && matches!(return_type, 1 | 2) {
            let state = if return_type == 1 {
                (instruction & 0x3fff) as u16
            } else {
                EVENT_MAP_NULL_STATE
            };
            let reason = self
                .return_from_call(handle)?
                .expect("event-service return always crosses a frame boundary");
            let HaltReason::EventServiceReturned { guard, .. } = reason else {
                unreachable!("validated event-service frame returns an event response");
            };
            return Ok(Some(HaltReason::EventServiceReturned { state, guard }));
        }
        if !tested && return_type == 0 {
            let offset = i64::from(sign_extend(instruction & 0x03ff, 10));
            self.jump_relative(handle, offset)?;
            let stack = &mut self.object_mut(handle)?.stack;
            stack.truncate(stack.len() - branch_argument_count);
        }
        Ok(None)
    }

    fn control_flow(
        &mut self,
        handle: ObjectHandle,
        instruction: u32,
        condition: &mut bool,
    ) -> Result<Option<HaltReason>, VmError> {
        let condition_type = (instruction >> 20) & 3;
        let register = ((instruction >> 14) & 0x3f) as usize;
        let pops_condition = matches!(condition_type, 1 | 2) && register == 0x1f;
        let tested = match condition_type {
            0 => true,
            1 | 2 => {
                let value = self.peek_process_register_reference(handle, register)?;
                if condition_type == 1 {
                    value != 0
                } else {
                    value == 0
                }
            }
            3 => *condition,
            _ => unreachable!(),
        };
        *condition = tested;

        let operation = (instruction >> 22) & 3;
        let argument_count = if tested && operation == 0 {
            ((instruction >> 10) & 0xf) as usize
        } else {
            0
        };
        let required = argument_count + usize::from(pops_condition);
        if self.object(handle)?.stack.len() < required {
            return Err(VmError::StackUnderflow(handle));
        }

        if matches!(condition_type, 1 | 2) {
            let value = self.read_process_register_reference(handle, register)?;
            debug_assert_eq!(
                tested,
                if condition_type == 1 {
                    value != 0
                } else {
                    value == 0
                }
            );
        }

        if tested && operation == 0 {
            let offset = i64::from(sign_extend(instruction & 0x03ff, 10));
            self.jump_relative(handle, offset)?;
        }
        if !tested || operation == 3 {
            return Ok(None);
        }
        match operation {
            0 => {
                let stack = &mut self.object_mut(handle)?.stack;
                stack.truncate(stack.len() - argument_count);
                Ok(None)
            }
            1 => {
                let state = (instruction & 0x3fff) as u16;
                if self.object(handle)?.state_link_blocked(state)? {
                    return Ok(None);
                }
                self.object_mut(handle)?.state = state;
                self.emit(VmEffect::StateChanged {
                    object: handle,
                    state,
                })?;
                Ok(Some(HaltReason::StateChanged(state)))
            }
            2 => self.return_from_call(handle),
            _ => unreachable!(),
        }
    }

    fn binary_push<F>(
        &mut self,
        handle: ObjectHandle,
        a: Operand,
        b: Operand,
        operation: F,
    ) -> Result<(), VmError>
    where
        F: FnOnce(u32, u32) -> u32,
    {
        let right = self.read_operand(handle, a)?;
        let left = self.read_operand(handle, b)?;
        self.push(handle, operation(left, right))
    }

    fn compare_push<F>(
        &mut self,
        handle: ObjectHandle,
        a: Operand,
        b: Operand,
        operation: F,
    ) -> Result<(), VmError>
    where
        F: FnOnce(i32, i32) -> bool,
    {
        // `G_TRANS_GOPS` translates A before B, then opcodes 0x09..0x0c
        // evaluate the signed predicate as A op B. Keep the read order exact
        // because either operand can pop the retail stack.
        let left = self.read_operand(handle, a)? as i32;
        let right = self.read_operand(handle, b)? as i32;
        self.push(handle, u32::from(operation(left, right)))
    }

    fn store_input_constant(&mut self, value: u32) -> usize {
        self.input_constant_index ^= 1;
        self.operand_constants[self.input_constant_index] = value;
        self.operand_constant_pool_slots[self.input_constant_index] = None;
        self.input_constant_index
    }

    fn store_output_constant(&mut self, value: u32) -> usize {
        self.output_constant_index ^= 1;
        self.operand_constants[self.output_constant_index] = value;
        self.operand_constant_pool_slots[self.output_constant_index] = None;
        self.output_constant_index
    }

    fn misc_operation(
        &mut self,
        handle: ObjectHandle,
        instruction: u32,
        operand: Operand,
    ) -> Result<bool, VmError> {
        // Native `GoolOpMisc` translates GOP B before inspecting either
        // packed selector. This matters for stack GOPs and the shared
        // immediate-constant cursor even when the selected operation is not
        // implemented yet.
        let input = self.input_reference(handle, operand)?;
        let primary = ((instruction >> 20) & 0x0f) as u8;
        let secondary = sign_extend((instruction >> 15) & 0x1f, 5) as i8;
        match primary {
            0 => {
                // Event-service `fp[-1]` is itself a pointer-valued word. A
                // null GOP means no destination/push; a non-null GOP holding
                // zero is the distinct native null-argv case and pushes zero.
                let Some(input) = input else {
                    return Ok(false);
                };
                let argv_word = self.read_storage_reference(input)?;
                if argv_word == 0 {
                    self.push(handle, 0)?;
                    return Ok(false);
                }
                let reference = EventArgumentsReference::from_word(argv_word)
                    .ok_or(VmError::InvalidEventArgumentsReference(argv_word))?;
                let (argument, pool_slot) =
                    self.event_argument_with_pool_slot(reference, secondary)?;
                self.push_with_pool_slot(handle, argument, pool_slot)?;
                Ok(false)
            }
            1 | 6 => {
                let link_index = ((instruction >> 12) & 7) as u8;
                let target = self
                    .resolve_process_link(handle, usize::from(link_index))?
                    .ok_or(VmError::MissingLink {
                        object: handle,
                        link: link_index,
                    })?;
                let mut source = if primary == 1 {
                    self.object(handle)?.process_vector(0)?
                } else {
                    let input = input.ok_or(VmError::MissingMiscOperand {
                        primary,
                        secondary,
                        operand: instruction as u16 & 0x0fff,
                    })?;
                    self.read_storage_span3(input)?.map(|value| value as i32)
                };
                let mut destination = self.object(target)?.process_vector(0)?;
                if secondary & 2 != 0 {
                    source[1] = 0;
                    destination[1] = 0;
                }
                let source = Vec3 {
                    x: source[0],
                    y: source[1],
                    z: source[2],
                };
                let destination = Vec3 {
                    x: destination[0],
                    y: destination[1],
                    z: destination[2],
                };
                let distance = if secondary & 1 != 0 {
                    euclidean_distance(source, destination)
                } else {
                    approximate_distance(source, destination)
                };
                self.push(handle, distance as u32)?;
                Ok(false)
            }
            2 => {
                let input = input.ok_or(VmError::MissingMiscOperand {
                    primary,
                    secondary,
                    operand: instruction as u16 & 0x0fff,
                })?;
                let link_index = ((instruction >> 12) & 7) as u8;
                let target = self
                    .resolve_process_link(handle, usize::from(link_index))?
                    .ok_or(VmError::MissingLink {
                        object: handle,
                        link: link_index,
                    })?;
                let source = self.object(target)?.process_vector(0)?;
                let destination = self.read_storage_span3(input)?.map(|value| value as i32);
                let status_b = self.object(handle)?.register(process_register::STATUS_B)?;
                let denominator = if status_b & 0x0020_0200 != 0 {
                    destination[1].wrapping_sub(source[1])
                } else {
                    destination[2].wrapping_sub(source[2])
                };
                let angle = retail_atan2(destination[0].wrapping_sub(source[0]), denominator);
                self.push(handle, u32::from(Angle12::new(angle).raw()))?;
                Ok(false)
            }
            3 | 4 => {
                let input = input.ok_or(VmError::MissingMiscOperand {
                    primary,
                    secondary,
                    operand: instruction as u16 & 0x0fff,
                })?;
                let link_index = ((instruction >> 12) & 7) as u8;
                let register = (self.read_storage_reference(input)? >> 8) as usize;
                let retained_pool_slot = self
                    .object(handle)?
                    .register_pool_slot(usize::from(link_index))?;
                let target = self.resolve_process_link(handle, usize::from(link_index))?;
                if retained_pool_slot.is_none() && target.is_none() {
                    // Keep this compatibility read instruction-exact. Stores,
                    // other registers, and every other absent process link
                    // remain checked failures instead of inheriting C's null
                    // pointer arithmetic.
                    if primary == 3 && instruction == OPTIONS_NULL_INTERRUPTER_LOAD {
                        self.push(handle, 0)?;
                        return Ok(false);
                    }
                    return Err(VmError::MissingLink {
                        object: handle,
                        link: link_index,
                    });
                }
                if primary == 3 {
                    let (value, provenance) = if let Some(pool_slot) = retained_pool_slot {
                        self.retail_pool_register_word(pool_slot, register)?
                    } else {
                        self.read_aliased_process_register_with_pool_slot(
                            target.expect("checked live target"),
                            register,
                        )?
                    };
                    self.push_with_pool_slot(handle, value, provenance)?;
                } else {
                    // Translation of GOP B happens before this pop, matching
                    // the native two-stack-word set-register form.
                    let (value, provenance) = self.pop_with_pool_slot(handle)?;
                    if let Some(pool_slot) = retained_pool_slot {
                        self.write_retail_pool_register_word(
                            pool_slot, register, value, provenance,
                        )?;
                    } else {
                        self.write_aliased_process_register_with_pool_slot(
                            target.expect("checked live target"),
                            register,
                            value,
                            provenance,
                        )?;
                    }
                }
                Ok(false)
            }
            5 => {
                let link_index = ((instruction >> 12) & 7) as u8;
                let target = self
                    .resolve_process_link(handle, usize::from(link_index))?
                    .ok_or(VmError::MissingLink {
                        object: handle,
                        link: link_index,
                    })?;
                let source = self.object(handle)?.process_vector(0)?;
                let destination = self.object(target)?.process_vector(0)?;
                let angle = i32::from(
                    Angle12::new(retail_atan2(
                        destination[0].wrapping_sub(source[0]),
                        destination[2].wrapping_sub(source[2]),
                    ))
                    .raw(),
                );
                let rotation = i32::from(
                    Angle12::new(
                        self.object(handle)?
                            .register(process_register::ROTATION_X)? as i32,
                    )
                    .raw(),
                );
                let mut difference = angle.wrapping_sub(rotation);
                if difference.wrapping_abs() >= 0x800 {
                    difference = i32::from(Angle12::new(difference.wrapping_neg()).raw());
                }
                self.push(handle, difference as u32)?;
                Ok(false)
            }
            7 => {
                let Some(input) = input else {
                    return Ok(false);
                };
                let pid_flags = self.read_storage_reference(input)?;
                if pid_flags == 0 {
                    self.push(handle, 0)?;
                    return Ok(false);
                }
                let raw_id = pid_flags >> 8;
                let id = u16::try_from(raw_id).map_err(|_| VmError::InvalidSpawnId(u16::MAX))?;
                if self.spawn_flags(id)? & 1 == 0 {
                    self.push(handle, 0)?;
                    return Ok(false);
                }
                self.emit(VmEffect::FindSpawnedObject {
                    requester: handle,
                    pid_flags,
                })?;
                Ok(true)
            }
            8 => {
                let Some(input) = input else {
                    return Ok(false);
                };
                let raw_id = self.read_storage_reference(input)? >> 8;
                let id = u16::try_from(raw_id).map_err(|_| VmError::InvalidSpawnId(u16::MAX))?;
                let before = self.spawn_flags(id)?;
                let flags = if secondary != 0 {
                    before & !2
                } else {
                    before | 2
                };
                self.set_spawn_flags(id, flags)?;
                self.emit(VmEffect::SpawnFlagsChanged {
                    object: handle,
                    id,
                    flags,
                })?;
                Ok(true)
            }
            9 => {
                let link_index = ((instruction >> 12) & 7) as u8;
                let target = self
                    .resolve_process_link(handle, usize::from(link_index))?
                    .ok_or(VmError::MissingLink {
                        object: handle,
                        link: link_index,
                    })?;
                let point = input
                    .map(|input| {
                        self.read_storage_span3(input)
                            .map(|point| point.map(|coordinate| coordinate as i32))
                    })
                    .transpose()?;
                self.emit(VmEffect::SetLinkZoneFromPoint {
                    requester: handle,
                    target,
                    point,
                })?;
                Ok(true)
            }
            10 => {
                let Some(input) = input else {
                    return Ok(false);
                };
                let raw_id = self.read_storage_reference(input)? >> 8;
                let id = u16::try_from(raw_id).map_err(|_| VmError::InvalidSpawnId(u16::MAX))?;
                let before = self.spawn_flags(id)?;
                if secondary == 4 {
                    self.free_retail_level_spawn_tag(id);
                } else if secondary == 5 {
                    self.allocate_retail_level_spawn_tag(id);
                }
                let Some(flags) = (match secondary {
                    0 => Some(before & !4),
                    1 => Some(before | 4),
                    // Native cases four and five update `level_spawns` and
                    // intentionally fall through to clear/set spawn bit four.
                    2 | 4 => Some(before & !8),
                    3 | 5 => Some(before | 8),
                    8 => Some(before & !1),
                    9 => Some(before | 1),
                    // Cases six/seven and all unknown selectors are no-ops.
                    _ => None,
                }) else {
                    return Ok(false);
                };
                self.set_spawn_flags(id, flags)?;
                self.emit(VmEffect::SpawnFlagsChanged {
                    object: handle,
                    id,
                    flags,
                })?;
                Ok(true)
            }
            11 => {
                if !(1..=3).contains(&secondary) {
                    return Ok(false);
                }
                let Some(input) = input else {
                    self.push(handle, 0)?;
                    return Ok(false);
                };
                let raw_id = self.read_storage_reference(input)? >> 8;
                let id = u16::try_from(raw_id).map_err(|_| VmError::InvalidSpawnId(u16::MAX))?;
                let flags = self.spawn_flags(id)?;
                let value = match secondary {
                    1 => u32::from(flags & 2 == 0),
                    2 => flags & 4,
                    3 => flags & 8,
                    _ => unreachable!(),
                };
                self.push(handle, value)?;
                Ok(false)
            }
            12 => match secondary {
                // These are the three native suboperations that correspond to
                // the VM's existing host effects. The remaining misc cases
                // stay explicit until their object/audio/global bindings are
                // available.
                0 => {
                    self.emit(VmEffect::SaveState(handle))?;
                    Ok(true)
                }
                1 => {
                    self.emit(VmEffect::LoadState {
                        object: handle,
                        saved_level: None,
                    })?;
                    Ok(true)
                }
                2 => {
                    let input = input.ok_or(VmError::MissingMiscOperand {
                        primary,
                        secondary,
                        operand: instruction as u16 & 0x0fff,
                    })?;
                    let raw_root = self.read_storage_reference(input)? >> 8;
                    let root = u8::try_from(raw_root).map_err(|_| {
                        VmError::InvalidRegister(usize::try_from(raw_root).unwrap_or(usize::MAX))
                    })?;
                    self.emit(VmEffect::ReparentToRoot {
                        object: handle,
                        root,
                    })?;
                    Ok(true)
                }
                4 => {
                    let input = input.ok_or(VmError::MissingMiscOperand {
                        primary,
                        secondary,
                        operand: instruction as u16 & 0x0fff,
                    })?;
                    let object = self.decode_misc_object_argument(input)?;
                    self.emit(VmEffect::SetObjectZoneToTransitionTarget { object })?;
                    Ok(true)
                }
                5 => {
                    self.emit(VmEffect::ResetMasterFadeStep { object: handle })?;
                    Ok(false)
                }
                6 => {
                    let input = input.ok_or(VmError::MissingMiscOperand {
                        primary,
                        secondary,
                        operand: instruction as u16 & 0x0fff,
                    })?;
                    let value = self.read_storage_reference(input)?;
                    self.emit(VmEffect::MidiTogglePlayback {
                        object: handle,
                        value,
                    })?;
                    Ok(false)
                }
                7 => {
                    self.emit(VmEffect::TerminateCurrentZoneNeighbors { requester: handle })?;
                    Ok(true)
                }
                8 => {
                    let input = input.ok_or(VmError::MissingMiscOperand {
                        primary,
                        secondary,
                        operand: instruction as u16 & 0x0fff,
                    })?;
                    let link_index = ((instruction >> 12) & 7) as u8;
                    let target = self
                        .resolve_process_link(handle, usize::from(link_index))?
                        .ok_or(VmError::MissingLink {
                            object: handle,
                            link: link_index,
                        })?;
                    let source = self.read_storage_span3(input)?.map(|value| value as i32);
                    let destination = self.object(target)?.process_vector(0)?;
                    let horizontal = euclidean_distance(
                        Vec3 {
                            x: destination[0],
                            y: source[1],
                            z: destination[2],
                        },
                        Vec3 {
                            x: source[0],
                            y: source[1],
                            z: source[2],
                        },
                    );
                    let angle = retail_atan2(destination[1].wrapping_sub(source[1]), horizontal);
                    self.push(handle, angle as u32)?;
                    Ok(false)
                }
                9 => {
                    let input = input.ok_or(VmError::MissingMiscOperand {
                        primary,
                        secondary,
                        operand: instruction as u16 & 0x0fff,
                    })?;
                    let level = (self.read_storage_reference(input)? >> 8) as i32;
                    self.emit(VmEffect::Transition(level))?;
                    Ok(false)
                }
                11 => {
                    self.emit(VmEffect::ResetLevelGlobals { object: handle })?;
                    Ok(true)
                }
                // Native cases three/ten intentionally do nothing, and its
                // inner `switch (sop2)` has no default arm. Every other
                // signed selector likewise performs only the GOP-B
                // translation that happened before the switch. Preserve its
                // stack/immediate cursor effects without quarantining the
                // object when a dormant or later retail path selects one.
                _ => Ok(false),
            },
            13 => {
                let input = input.ok_or(VmError::MissingMiscOperand {
                    primary,
                    secondary,
                    operand: instruction as u16 & 0x0fff,
                })?;
                let link_index = ((instruction >> 12) & 7) as u8;
                let origin = self
                    .resolve_process_link(handle, usize::from(link_index))?
                    .ok_or(VmError::MissingLink {
                        object: handle,
                        link: link_index,
                    })?;
                let event = self.read_storage_reference(input)?;
                self.emit(VmEffect::FindNearestObject {
                    requester: handle,
                    origin,
                    categories: ((instruction >> 15) & 0x1f) as u8,
                    event,
                })?;
                Ok(true)
            }
            14 => {
                let input = input.ok_or(VmError::MissingMiscOperand {
                    primary,
                    secondary,
                    operand: instruction as u16 & 0x0fff,
                })?;
                let link_index = ((instruction >> 12) & 7) as u8;
                let target = self
                    .resolve_process_link(handle, usize::from(link_index))?
                    .ok_or(VmError::MissingLink {
                        object: handle,
                        link: link_index,
                    })?;
                let point = self.read_storage_span3(input)?.map(|value| value as i32);
                let target_object = self.object(target)?;
                let translation = target_object.process_vector(0)?;
                let bound = Bounds3 {
                    min: Vec3 {
                        x: target_object.local_bound.min.x.wrapping_add(translation[0]),
                        y: target_object.local_bound.min.y.wrapping_add(translation[1]),
                        z: target_object.local_bound.min.z.wrapping_add(translation[2]),
                    },
                    max: Vec3 {
                        x: target_object.local_bound.max.x.wrapping_add(translation[0]),
                        y: target_object.local_bound.max.y.wrapping_add(translation[1]),
                        z: target_object.local_bound.max.z.wrapping_add(translation[2]),
                    },
                };
                self.push(
                    handle,
                    u32::from(point_in_bound(
                        Vec3 {
                            x: point[0],
                            y: point[1],
                            z: point[2],
                        },
                        bound,
                    )),
                )?;
                Ok(false)
            }
            15 => {
                let input = input.ok_or(VmError::MissingMiscOperand {
                    primary,
                    secondary,
                    operand: instruction as u16 & 0x0fff,
                })?;
                let part_index = self.read_storage_reference(input)? as i32;
                self.pending_card_host_request = Some(CardHostRequest {
                    object: handle,
                    operation: i32::from(secondary),
                    part_index,
                });
                Ok(true)
            }
            _ => Err(VmError::UnsupportedMiscOperation {
                primary,
                secondary,
                operand: instruction as u16 & 0x0fff,
            }),
        }
    }

    fn decode_misc_object_argument(
        &self,
        reference: StorageReference,
    ) -> Result<ObjectHandle, VmError> {
        if reference.region == StorageRegion::Register && reference.index < 8 {
            let (raw, pool_slot) = self.read_storage_reference_with_pool_slot(reference)?;
            let target = CollisionObjectReference::from_word(raw)
                .ok_or(VmError::InvalidObjectReference(raw))?;
            return self.resolve_pool_backed_object_reference(target, pool_slot);
        }
        let (raw, pool_slot) = self.read_storage_reference_with_pool_slot(reference)?;
        let reference =
            CollisionObjectReference::from_word(raw).ok_or(VmError::InvalidObjectReference(raw))?;
        self.resolve_pool_backed_object_reference(reference, pool_slot)
    }

    fn input_reference(
        &mut self,
        handle: ObjectHandle,
        operand: Operand,
    ) -> Result<Option<StorageReference>, VmError> {
        let reference = match operand {
            Operand::Internal(index) => {
                self.object(handle)?
                    .internal
                    .get(usize::from(index))
                    .ok_or(VmError::InvalidRegister(usize::from(index)))?;
                StorageReference::checked(handle, StorageRegion::Internal, usize::from(index))?
            }
            Operand::External(index) => {
                self.object(handle)?
                    .external
                    .get(usize::from(index))
                    .ok_or(VmError::InvalidRegister(usize::from(index)))?;
                StorageReference::checked(handle, StorageRegion::External, usize::from(index))?
            }
            Operand::Immediate(value) => {
                let index = self.store_input_constant(value as u32);
                StorageReference::checked(handle, StorageRegion::Constant, index)?
            }
            Operand::FrameRelative(offset) => {
                let base = self.object(handle)?.frame_base;
                let index = base.checked_add_signed(isize::from(offset)).ok_or(
                    VmError::UnsupportedReferenceOperand(0x0b00 | (u16::from(offset as u8) & 0x3f)),
                )?;
                self.read_aliased_process_register(handle, index)?;
                StorageReference::checked(handle, StorageRegion::Register, index)?
            }
            Operand::Null => return Ok(None),
            Operand::StackDouble => return Err(VmError::UnsupportedReferenceOperand(0x0bf0)),
            Operand::LinkRegister { link, register } => {
                let Some(reference) = self.link_register_reference(handle, link, register)? else {
                    return Ok(None);
                };
                reference
            }
            Operand::ObjectRegister(index) => {
                self.read_aliased_process_register(handle, usize::from(index))?;
                StorageReference::checked(handle, StorageRegion::Register, usize::from(index))?
            }
            Operand::Stack => {
                let object = self.object(handle)?;
                let stack_index = object
                    .stack
                    .len()
                    .checked_sub(1)
                    .ok_or(VmError::StackUnderflow(handle))?;
                let register_index = (object.initial_stack_pointer as usize)
                    .checked_add(stack_index)
                    .ok_or(VmError::StackOverflow(handle))?;
                self.pop(handle)?;
                StorageReference::checked(handle, StorageRegion::Register, register_index)?
            }
        };
        Ok(Some(reference))
    }

    fn output_reference(
        &mut self,
        handle: ObjectHandle,
        operand: Operand,
    ) -> Result<Option<StorageReference>, VmError> {
        let reference = match operand {
            Operand::Internal(index) => {
                self.object(handle)?
                    .internal
                    .get(usize::from(index))
                    .ok_or(VmError::InvalidRegister(usize::from(index)))?;
                StorageReference::checked(handle, StorageRegion::Internal, usize::from(index))?
            }
            Operand::External(index) => {
                self.object(handle)?
                    .external
                    .get(usize::from(index))
                    .ok_or(VmError::InvalidRegister(usize::from(index)))?;
                StorageReference::checked(handle, StorageRegion::External, usize::from(index))?
            }
            Operand::Immediate(value) => {
                // Retail has independent input/output cursors over one shared
                // two-word constant buffer. Merely translating an output
                // immediate writes it, even if the caller never stores.
                let index = self.store_output_constant(value as u32);
                StorageReference::checked(handle, StorageRegion::Constant, index)?
            }
            Operand::FrameRelative(offset) => {
                let base = self.object(handle)?.frame_base;
                let index = base.checked_add_signed(isize::from(offset)).ok_or(
                    VmError::UnsupportedReferenceOperand(0x0b00 | (u16::from(offset as u8) & 0x3f)),
                )?;
                self.read_aliased_process_register(handle, index)?;
                StorageReference::checked(handle, StorageRegion::Register, index)?
            }
            Operand::Null => return Ok(None),
            Operand::StackDouble => return Err(VmError::UnsupportedReferenceOperand(0x0bf0)),
            Operand::LinkRegister { link, register } => {
                let Some(reference) = self.link_register_reference(handle, link, register)? else {
                    return Ok(None);
                };
                reference
            }
            Operand::ObjectRegister(index) => {
                self.read_aliased_process_register(handle, usize::from(index))?;
                StorageReference::checked(handle, StorageRegion::Register, usize::from(index))?
            }
            Operand::Stack => {
                let register_index = {
                    let object = self.object(handle)?;
                    (object.initial_stack_pointer as usize)
                        .checked_add(object.stack.len())
                        .ok_or(VmError::StackOverflow(handle))?
                };
                let previous = self.read_aliased_process_register(handle, register_index)?;
                // Translating an output stack GOP advances SP but does not
                // itself write the pointed-to word. Retain the stale bounded
                // register value until a caller stores through the reference.
                self.push(handle, previous)?;
                StorageReference::checked(handle, StorageRegion::Register, register_index)?
            }
        };
        Ok(Some(reference))
    }

    /// Translates a linked register to the physical static-pool address when
    /// one is known. Native performs no live/type check here: a non-null link
    /// continues to address initialized storage while its slot is free and
    /// automatically names the replacement after that exact slot is reused.
    fn link_register_reference(
        &self,
        handle: ObjectHandle,
        link: u8,
        register: u8,
    ) -> Result<Option<StorageReference>, VmError> {
        let (cached, retained_pool_slot) = {
            let object = self.object(handle)?;
            let link_index = usize::from(link);
            (
                *object
                    .links
                    .get(link_index)
                    .ok_or(VmError::InvalidRegister(link_index))?,
                object.register_pool_slot(link_index)?,
            )
        };
        if let Some(pool_slot) = retained_pool_slot {
            return StorageReference::retail_pool_register(pool_slot, usize::from(register))
                .map(Some);
        }
        let Some(target) = cached else {
            return Ok(None);
        };
        if let Some(pool_slot) = self
            .retail_pool_slots_by_object
            .get(usize::from(target.get()))
            .copied()
            .flatten()
        {
            return StorageReference::retail_pool_register(pool_slot, usize::from(register))
                .map(Some);
        }
        self.read_aliased_process_register(target, usize::from(register))?;
        StorageReference::checked(target, StorageRegion::Register, usize::from(register)).map(Some)
    }

    /// Resolves one tagged storage word through bounded VM-owned arrays.
    pub fn read_storage_reference(&self, reference: StorageReference) -> Result<u32, VmError> {
        let index = usize::from(reference.index);
        match reference.backing {
            StorageBacking::RetailPool(pool_slot) => self
                .retail_pool_register_word(pool_slot, index)
                .map(|(value, _)| value)
                .map_err(|_| VmError::InvalidStorageReference(reference.to_word())),
            StorageBacking::Object(object) => match reference.region {
                StorageRegion::Internal => self
                    .object(object)?
                    .internal
                    .get(index)
                    .copied()
                    .ok_or(VmError::InvalidStorageReference(reference.to_word())),
                StorageRegion::External => self
                    .object(object)?
                    .external
                    .get(index)
                    .copied()
                    .ok_or(VmError::InvalidStorageReference(reference.to_word())),
                StorageRegion::Register => self
                    .read_aliased_process_register(object, index)
                    .map_err(|_| VmError::InvalidStorageReference(reference.to_word())),
                StorageRegion::Constant => self
                    .operand_constants
                    .get(index)
                    .copied()
                    .ok_or(VmError::InvalidStorageReference(reference.to_word())),
            },
        }
    }

    fn read_storage_reference_with_pool_slot(
        &self,
        reference: StorageReference,
    ) -> Result<(u32, Option<u8>), VmError> {
        match (reference.backing, reference.region) {
            (StorageBacking::RetailPool(pool_slot), _) => {
                return self
                    .retail_pool_register_word(pool_slot, usize::from(reference.index))
                    .map_err(|_| VmError::InvalidStorageReference(reference.to_word()));
            }
            (StorageBacking::Object(object), StorageRegion::Register) => {
                return self
                    .read_aliased_process_register_with_pool_slot(
                        object,
                        usize::from(reference.index),
                    )
                    .map_err(|_| VmError::InvalidStorageReference(reference.to_word()));
            }
            (StorageBacking::Object(_), _) => {}
        }
        let value = self.read_storage_reference(reference)?;
        let index = usize::from(reference.index);
        let StorageBacking::Object(object) = reference.backing else {
            unreachable!("retail-pool storage returned above")
        };
        let retained = match reference.region {
            StorageRegion::Internal => self
                .object(object)?
                .internal_pool_slots
                .get(index)
                .copied()
                .ok_or(VmError::InvalidStorageReference(reference.to_word()))?,
            StorageRegion::External => self
                .object(object)?
                .external_pool_slots
                .get(index)
                .copied()
                .ok_or(VmError::InvalidStorageReference(reference.to_word()))?,
            StorageRegion::Register => unreachable!("live register storage returned above"),
            StorageRegion::Constant => self
                .operand_constant_pool_slots
                .get(index)
                .copied()
                .ok_or(VmError::InvalidStorageReference(reference.to_word()))?,
        };
        Ok((value, self.live_pool_slot_for_word(value, retained)))
    }

    fn write_storage_reference(
        &mut self,
        reference: StorageReference,
        value: u32,
    ) -> Result<(), VmError> {
        self.write_storage_reference_with_pool_slot(reference, value, None)
    }

    fn write_storage_reference_with_pool_slot(
        &mut self,
        reference: StorageReference,
        value: u32,
        pool_slot: Option<u8>,
    ) -> Result<(), VmError> {
        if let Some(pool_slot) = pool_slot
            && usize::from(pool_slot) >= MAX_OBJECTS
        {
            return Err(VmError::InvalidRetailPoolSlot(pool_slot));
        }
        let pool_slot = self
            .live_pool_slot_for_word(value, pool_slot)
            .filter(|_| CollisionObjectReference::from_word(value).is_some());
        let index = usize::from(reference.index);
        match reference.backing {
            StorageBacking::RetailPool(retail_pool_slot) => {
                self.write_retail_pool_register_word(retail_pool_slot, index, value, pool_slot)
            }
            StorageBacking::Object(object_handle) => match reference.region {
                StorageRegion::Internal => {
                    let object = self.object_mut(object_handle)?;
                    *object
                        .internal
                        .get_mut(index)
                        .ok_or(VmError::InvalidStorageReference(reference.to_word()))? = value;
                    *object
                        .internal_pool_slots
                        .get_mut(index)
                        .ok_or(VmError::InvalidStorageReference(reference.to_word()))? = pool_slot;
                    Ok(())
                }
                StorageRegion::External => {
                    let object = self.object_mut(object_handle)?;
                    *object
                        .external
                        .get_mut(index)
                        .ok_or(VmError::InvalidStorageReference(reference.to_word()))? = value;
                    *object
                        .external_pool_slots
                        .get_mut(index)
                        .ok_or(VmError::InvalidStorageReference(reference.to_word()))? = pool_slot;
                    Ok(())
                }
                StorageRegion::Register => self
                    .write_aliased_process_register_with_pool_slot(
                        object_handle,
                        index,
                        value,
                        pool_slot,
                    )
                    .map_err(|_| VmError::InvalidStorageReference(reference.to_word())),
                StorageRegion::Constant => {
                    *self
                        .operand_constants
                        .get_mut(index)
                        .ok_or(VmError::InvalidStorageReference(reference.to_word()))? = value;
                    *self
                        .operand_constant_pool_slots
                        .get_mut(index)
                        .ok_or(VmError::InvalidStorageReference(reference.to_word()))? = pool_slot;
                    Ok(())
                }
            },
        }
    }

    fn write_storage_span3(
        &mut self,
        reference: StorageReference,
        values: [u32; 3],
    ) -> Result<(), VmError> {
        let references = [0_usize, 1, 2].map(|offset| reference.checked_offset(offset));
        let references = [references[0]?, references[1]?, references[2]?];
        // Validate the complete C `vec` span before mutating so malformed
        // tagged storage cannot leave a partial triple behind.
        for (reference, value) in references.into_iter().zip(values) {
            self.read_storage_reference(reference)?;
            if let StorageBacking::RetailPool(pool_slot) = reference.backing {
                self.preflight_retail_pool_register_write(
                    pool_slot,
                    usize::from(reference.index),
                    value,
                    None,
                )?;
            }
        }
        for (reference, value) in references.into_iter().zip(values) {
            self.write_storage_reference(reference, value)?;
        }
        Ok(())
    }

    fn read_storage_span3(&self, reference: StorageReference) -> Result<[u32; 3], VmError> {
        let references = [0_usize, 1, 2].map(|offset| reference.checked_offset(offset));
        let references = [references[0]?, references[1]?, references[2]?];
        Ok([
            self.read_storage_reference(references[0])?,
            self.read_storage_reference(references[1])?,
            self.read_storage_reference(references[2])?,
        ])
    }

    fn intern_entry_reference(
        &mut self,
        eid: Eid,
        page: PageIndex,
    ) -> Result<EntryReference, VmError> {
        if let Some(slot) = self
            .paging_entry_references
            .iter()
            .position(|candidate| *candidate == (eid, page))
        {
            return Ok(EntryReference { slot: slot as u32 });
        }
        let slot = u32::try_from(self.paging_entry_references.len())
            .map_err(|_| VmError::EntryReferenceTableFull)?;
        if slot > ENTRY_REFERENCE_SLOT_BITS {
            return Err(VmError::EntryReferenceTableFull);
        }
        self.paging_entry_references.push((eid, page));
        Ok(EntryReference { slot })
    }

    fn entry_reference_page(&self, reference: EntryReference) -> Result<PageIndex, VmError> {
        self.entry_reference_identity(reference)
            .map(|(_, page)| page)
    }

    fn entry_reference_identity(
        &self,
        reference: EntryReference,
    ) -> Result<(Eid, PageIndex), VmError> {
        self.paging_entry_references
            .get(reference.slot as usize)
            .copied()
            .ok_or(VmError::InvalidEntryReference(reference.to_word()))
    }

    fn resolve_entry_argument(
        &mut self,
        reference: StorageReference,
    ) -> Result<(EntryReference, PageIndex), VmError> {
        let word = self.read_storage_reference(reference)?;
        if let Some(entry) = EntryReference::from_word(word) {
            let page = self.entry_reference_page(entry)?;
            return Ok((entry, page));
        }
        // A relocated entry token is immutable. Never retain or recursively
        // follow a mutable storage-cell reference; doing so would change the
        // entry identity after GOOL overwrites that cell and permits cycles.
        if StorageReference::from_word(word).is_some() {
            return Err(VmError::InvalidStorageReference(word));
        }
        let eid = Eid::from_raw(word);
        let page = self
            .entry_pages
            .get(&eid)
            .copied()
            .ok_or(VmError::MissingEntryReferencePage(eid))?;
        let entry = self.intern_entry_reference(eid, page)?;
        Ok((entry, page))
    }

    fn read_operand(&mut self, handle: ObjectHandle, operand: Operand) -> Result<u32, VmError> {
        self.read_operand_with_pool_slot(handle, operand)
            .map(|(value, _)| value)
    }

    fn read_operand_with_pool_slot(
        &mut self,
        handle: ObjectHandle,
        operand: Operand,
    ) -> Result<(u32, Option<u8>), VmError> {
        match operand {
            Operand::Internal(index) => self.read_storage_reference_with_pool_slot(
                StorageReference::checked(handle, StorageRegion::Internal, usize::from(index))?,
            ),
            Operand::External(index) => self.read_storage_reference_with_pool_slot(
                StorageReference::checked(handle, StorageRegion::External, usize::from(index))?,
            ),
            Operand::Immediate(value) => {
                self.store_input_constant(value as u32);
                Ok((value as u32, None))
            }
            Operand::FrameRelative(offset) => {
                let base = self.object(handle)?.frame_base;
                let index = base
                    .checked_add_signed(isize::from(offset))
                    .ok_or(VmError::InvalidOperand(0))?;
                self.read_aliased_process_register_with_pool_slot(handle, index)
            }
            Operand::Null => Ok((NULL_INPUT_VALUE, None)),
            Operand::StackDouble => Err(VmError::InvalidOperand(0x0bf0)),
            Operand::ObjectRegister(index) => {
                let index = usize::from(index);
                self.read_aliased_process_register_with_pool_slot(handle, index)
            }
            Operand::Stack => {
                let (value, retained) = self.pop_with_pool_slot(handle)?;
                Ok((value, self.live_pool_slot_for_word(value, retained)))
            }
            Operand::LinkRegister { link, register } => {
                let (target, pool_slot) = {
                    let object = self.object(handle)?;
                    (
                        object.links[usize::from(link)],
                        object.register_pool_slot(usize::from(link))?,
                    )
                };
                if let Some(pool_slot) = pool_slot {
                    return self.retail_pool_register_word(pool_slot, usize::from(register));
                }
                let Some(target) = self
                    .resolve_process_link(handle, usize::from(link))?
                    .or(target)
                else {
                    return Ok((NULL_INPUT_VALUE, None));
                };
                let register = usize::from(register);
                self.read_aliased_process_register_with_pool_slot(target, register)
            }
        }
    }

    fn read_optional_input(
        &mut self,
        handle: ObjectHandle,
        operand: Operand,
    ) -> Result<Option<u32>, VmError> {
        match operand {
            Operand::Null => Ok(None),
            Operand::StackDouble => Err(VmError::InvalidOperand(0x0bf0)),
            operand => self.read_operand(handle, operand).map(Some),
        }
    }

    fn write_operand(
        &mut self,
        handle: ObjectHandle,
        operand: Operand,
        value: u32,
    ) -> Result<(), VmError> {
        self.write_operand_with_pool_slot(handle, operand, value, None)
    }

    fn write_operand_with_pool_slot(
        &mut self,
        handle: ObjectHandle,
        operand: Operand,
        value: u32,
        pool_slot: Option<u8>,
    ) -> Result<(), VmError> {
        if let Operand::LinkRegister { link, register } = operand {
            let retained = self.object(handle)?.register_pool_slot(usize::from(link))?;
            if let Some(target_pool_slot) = retained {
                return self.write_retail_pool_register_word(
                    target_pool_slot,
                    usize::from(register),
                    value,
                    pool_slot,
                );
            }
            if let Some(target) = self.resolve_process_link(handle, usize::from(link))? {
                self.write_aliased_process_register_with_pool_slot(
                    target,
                    usize::from(register),
                    value,
                    pool_slot,
                )?;
            }
            return Ok(());
        }
        if let Some(reference) = self.output_reference(handle, operand)? {
            self.write_storage_reference_with_pool_slot(reference, value, pool_slot)?;
        }
        Ok(())
    }

    fn push(&mut self, handle: ObjectHandle, value: u32) -> Result<(), VmError> {
        self.object_mut(handle)?.push_stack_word(value)
    }

    fn push_with_pool_slot(
        &mut self,
        handle: ObjectHandle,
        value: u32,
        pool_slot: Option<u8>,
    ) -> Result<(), VmError> {
        self.object_mut(handle)?
            .push_stack_word_with_pool_slot(value, pool_slot)
    }

    fn pop(&mut self, handle: ObjectHandle) -> Result<u32, VmError> {
        self.pop_with_pool_slot(handle).map(|(value, _)| value)
    }

    fn pop_with_pool_slot(&mut self, handle: ObjectHandle) -> Result<(u32, Option<u8>), VmError> {
        let object = self.object_mut(handle)?;
        let stack_index = object
            .stack
            .len()
            .checked_sub(1)
            .ok_or(VmError::StackUnderflow(handle))?;
        let register_index = (object.initial_stack_pointer as usize)
            .checked_add(stack_index)
            .ok_or(VmError::StackOverflow(handle))?;
        let pool_slot = object.register_pool_slot(register_index)?;
        let value = object.stack.pop().ok_or(VmError::StackUnderflow(handle))?;
        // Native only moves SP. The popped process word and its pointer bits
        // remain in static storage until a later push overwrites that slot.
        Ok((value, pool_slot))
    }

    fn jump_relative(&mut self, handle: ObjectHandle, offset: i64) -> Result<(), VmError> {
        let object = self.object(handle)?;
        let target = i64::try_from(object.pc)
            .unwrap_or(i64::MAX)
            .saturating_add(offset);
        let code_len = match object.code_segment {
            CodeSegment::External => object.code.len(),
            CodeSegment::Global => object.global_code.len(),
        };
        if target < 0 || usize::try_from(target).map_or(true, |target| target >= code_len) {
            return Err(VmError::InvalidJump {
                object: handle,
                target,
            });
        }
        self.object_mut(handle)?.pc = target as usize;
        Ok(())
    }

    fn call_global(
        &mut self,
        handle: ObjectHandle,
        target: usize,
        argument_count: usize,
    ) -> Result<(), VmError> {
        let object = self.object(handle)?;
        if object.call_stack.len() == MAX_CALL_DEPTH {
            return Err(VmError::CallStackOverflow(handle));
        }
        if object.stack.len() < argument_count {
            return Err(VmError::StackUnderflow(handle));
        }
        if target == 0x3fff {
            self.object_mut(handle)?.halted = true;
            return Ok(());
        }
        if target >= object.global_code.len() {
            return Err(VmError::InvalidJump {
                object: handle,
                target: target as i64,
            });
        }
        let stack_origin = object.initial_stack_pointer as usize;
        let stack_pointer = stack_origin
            .checked_add(object.stack.len())
            .ok_or(VmError::StackOverflow(handle))?;
        let argument_base = object.stack.len() - argument_count;
        let previous_frame_base = object.frame_base;
        let return_address = object.code_address();
        let prior_rsp_bytes = stack_origin
            .checked_add(argument_base)
            .and_then(|word| word.checked_mul(4))
            .and_then(|bytes| u16::try_from(bytes).ok())
            .ok_or(VmError::StackOverflow(handle))?;
        let prior_rfp_bytes = previous_frame_base
            .checked_mul(4)
            .and_then(|bytes| u16::try_from(bytes).ok())
            .ok_or(VmError::StackOverflow(handle))?;
        if object.stack.len() + 3 > MAX_STACK_WORDS || stack_pointer + 3 > REGISTER_COUNT {
            return Err(VmError::StackOverflow(handle));
        }
        let frame = CallFrame {
            return_address,
            return_halted: object.halted,
            argument_base,
            previous_frame_base,
            behavior: ReturnBehavior::Continue,
        };
        {
            let object = self.object_mut(handle)?;
            object.frame_base = stack_pointer;
            object.call_stack.push(frame);
        }
        self.push(handle, NORMAL_INTERPRETER_FLAGS)?;
        self.push(handle, encode_code_reference(return_address))?;
        self.push(
            handle,
            (u32::from(prior_rfp_bytes) << 16) | u32::from(prior_rsp_bytes),
        )?;
        let object = self.object_mut(handle)?;
        object.code_segment = CodeSegment::Global;
        object.pc = target;
        Ok(())
    }

    fn return_from_call(&mut self, handle: ObjectHandle) -> Result<Option<HaltReason>, VmError> {
        let Some(frame) = self.object_mut(handle)?.call_stack.pop() else {
            let object = self.object_mut(handle)?;
            if object.retail_initial_frame_return_is_invalid {
                return Ok(Some(HaltReason::InvalidInitialReturn));
            }
            object.halted = true;
            return Ok(Some(HaltReason::Halted));
        };
        let object = self.object_mut(handle)?;
        object.stack.truncate(frame.argument_base);
        object.frame_base = frame.previous_frame_base;
        object.code_segment = frame.return_address.segment;
        object.pc = frame.return_address.pc;
        object.halted = frame.return_halted;
        match frame.behavior {
            ReturnBehavior::Continue => Ok(None),
            ReturnBehavior::SuspendOnce { state_stamp } => {
                self.push(handle, 0)?;
                let object = self.object_mut(handle)?;
                object.animation_wait = Some(AnimationWait {
                    stamp: 0,
                    frames: 0,
                });
                object.set_register(process_register::STATE_STAMP, state_stamp)?;
                Ok(Some(HaltReason::OnceCompleted))
            }
            ReturnBehavior::SuspendTransition {
                previous_animation_wait,
            } => {
                // Any animation wait word pushed inside the transition frame
                // was discarded with that frame. Restore the state-code wait
                // represented by the stack beneath it; retail does not carry
                // a separate transition animation suspension.
                self.object_mut(handle)?.animation_wait = previous_animation_wait;
                Ok(Some(HaltReason::TransitionCompleted))
            }
            ReturnBehavior::EventService {
                return_event,
                guard,
                previous_animation_wait,
                ..
            } => {
                self.object_mut(handle)?.animation_wait = previous_animation_wait;
                if return_event {
                    Ok(Some(HaltReason::EventServiceReturned {
                        state: EVENT_MAP_NULL_STATE,
                        guard,
                    }))
                } else {
                    Ok(Some(HaltReason::EventServiceInvalidReturn))
                }
            }
            ReturnBehavior::Interrupt {
                previous_animation_wait,
            } => {
                self.object_mut(handle)?.animation_wait = previous_animation_wait;
                Ok(Some(HaltReason::InterruptCompleted))
            }
        }
    }

    fn spawn_children(
        &mut self,
        handle: ObjectHandle,
        instruction: u32,
        allow_reclaim: bool,
    ) -> Result<bool, VmError> {
        let encoded_argument_count = ((instruction >> 20) & 0x0f) as usize;
        let executable = ((instruction >> 12) & 0xff) as u8;
        let subtype = ((instruction >> 6) & 0x3f) as u8;
        let encoded_count = instruction & 0x3f;
        let stack_len = self.object(handle)?.stack.len();
        if stack_len < encoded_argument_count {
            return Err(VmError::StackUnderflow(handle));
        }

        let (count, argument_count) = if encoded_count == 0 {
            let Some(argument_count) = encoded_argument_count.checked_sub(1) else {
                return Err(VmError::StackUnderflow(handle));
            };
            (self.object(handle)?.stack[stack_len - 1], argument_count)
        } else {
            (encoded_count, encoded_argument_count)
        };
        let signed_count = count as i32;
        if signed_count > MAX_OBJECTS as i32 {
            return Err(VmError::SpawnCountTooLarge(count));
        }

        let argument_start = stack_len - encoded_argument_count;
        let argument_end = argument_start + argument_count;
        let (arguments, argument_pool_slots) = {
            let object = self.object(handle)?;
            let stack_origin = usize::try_from(object.initial_stack_pointer)
                .map_err(|_| VmError::InvalidInitialStackPointer(object.initial_stack_pointer))?;
            let arguments = object.stack[argument_start..argument_end].to_vec();
            let mut pool_slots = Vec::with_capacity(argument_count);
            for stack_index in argument_start..argument_end {
                let register = stack_origin
                    .checked_add(stack_index)
                    .ok_or(VmError::StackOverflow(handle))?;
                let argument = object.stack[stack_index];
                pool_slots.push(
                    self.live_pool_slot_for_word(argument, object.register_pool_slot(register)?),
                );
            }
            (arguments, pool_slots)
        };
        self.object_mut(handle)?.stack.truncate(argument_start);
        if signed_count > 0 {
            self.emit(VmEffect::SpawnChildren {
                parent: handle,
                executable,
                subtype,
                count,
                allow_reclaim,
                arguments,
                argument_pool_slots,
            })?;
            return Ok(true);
        }
        Ok(false)
    }

    fn emit(&mut self, effect: VmEffect) -> Result<(), VmError> {
        if self.effects.len().saturating_sub(self.effect_checkpoint) >= MAX_EFFECTS {
            return Err(VmError::EffectQueueFull);
        }
        self.effects.push(effect);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::Vec3;
    use crate::paging::Pager;
    use proptest::prelude::*;

    const REG0: u16 = 0x0e00;
    const REG1: u16 = 0x0e01;
    const REG2: u16 = 0x0e02;
    const STACK: u16 = 0x0e1f;

    fn handle(index: u16) -> ObjectHandle {
        ObjectHandle::new(index).unwrap()
    }

    fn write_animation_bytes(
        object: &mut VmObject,
        region: StorageRegion,
        index: usize,
        bytes: &[u8],
    ) {
        let mut padded = bytes.to_vec();
        let padded_len = padded.len().next_multiple_of(4);
        padded.resize(padded_len, 0);
        for (offset, bytes) in padded.chunks_exact(4).enumerate() {
            let value = u32::from_le_bytes(bytes.try_into().unwrap());
            match region {
                StorageRegion::Internal => object.set_internal(index + offset, value).unwrap(),
                StorageRegion::External => object.set_external(index + offset, value).unwrap(),
                StorageRegion::Register => object.set_register(index + offset, value).unwrap(),
                StorageRegion::Constant => panic!("constants are machine-owned"),
            }
        }
        let reference = StorageReference::checked(object.handle(), region, index).unwrap();
        object
            .set_register(process_register::ANIMATION_SEQUENCE, reference.to_word())
            .unwrap();
    }

    fn control_flow(
        operation: u32,
        condition: u32,
        register: u32,
        argument_count: u32,
        target: u32,
    ) -> u32 {
        (0x82_u32 << 24)
            | ((operation & 3) << 22)
            | ((condition & 3) << 20)
            | ((register & 0x3f) << 14)
            | ((argument_count & 0xf) << 10)
            | (target & 0x03ff)
    }

    fn misc(primary: u32, secondary: i32, operand: u16) -> u32 {
        (0x1c_u32 << 24)
            | ((primary & 0x0f) << 20)
            | (((secondary as u32) & 0x1f) << 15)
            | u32::from(operand & 0x0fff)
    }

    fn audio_control(suboperation: u8, flags: u8, voice_selector: u8, operand: u16) -> u32 {
        (0x8d_u32 << 24)
            | (u32::from(suboperation & 0x0f) << 20)
            | (u32::from(flags & 3) << 18)
            | (u32::from(voice_selector & 0x3f) << 12)
            | u32::from(operand & 0x0fff)
    }

    const fn send_event_instruction(
        opcode: u8,
        condition_register: u8,
        argument_count: u8,
        mode: u8,
        event_operand: u16,
    ) -> u32 {
        (opcode as u32) << 24
            | ((mode as u32 & 7) << 21)
            | ((argument_count as u32 & 7) << 18)
            | ((condition_register as u32 & 0x3f) << 12)
            | (event_operand as u32 & 0x0fff)
    }

    fn event_return(
        opcode: u8,
        return_type: u32,
        condition_type: u32,
        register: u32,
        argument_count: u32,
        target: u32,
    ) -> u32 {
        (u32::from(opcode) << 24)
            | ((return_type & 3) << 22)
            | ((condition_type & 3) << 20)
            | ((register & 0x3f) << 14)
            | ((argument_count & 0x0f) << 10)
            | (target & 0x3fff)
    }

    #[test]
    fn instruction_and_operand_decoding_preserve_words() {
        let word = Instruction::encode(0x8d, 0xabc, 0x123);
        assert_eq!(
            Instruction::decode(word),
            Instruction {
                opcode: 0x8d,
                operand_a: 0xabc,
                operand_b: 0x123
            }
        );
        assert_eq!(Operand::decode(0x800), Operand::Immediate(0));
        assert_eq!(Operand::decode(0x9ff), Operand::Immediate(-256));
        assert_eq!(Operand::decode(0xbe0), Operand::Null);
        assert_eq!(Operand::decode(STACK), Operand::Stack);
    }

    #[test]
    fn authored_opcodes_80_and_81_are_retail_noops() {
        let h = handle(0);
        let mut machine = Machine::new(0);
        machine
            .insert_object(
                VmObject::new(
                    h,
                    vec![
                        0x8000_0000,
                        0x8100_0000,
                        Instruction::encode(0x11, 0x0805, 0x0e08),
                    ],
                )
                .unwrap(),
            )
            .unwrap();

        assert_eq!(
            machine.run(h, 3),
            Ok(Execution {
                reason: HaltReason::BudgetExhausted,
                steps: 3,
            })
        );
        assert_eq!(machine.object(h).unwrap().register(8), Ok(0x500));
    }

    #[test]
    fn aligned_tagged_references_round_trip_without_eid_low_bits() {
        assert_eq!(
            MAX_OBJECTS,
            crate::object_arena::OBJECT_POOL_CAPACITY + 1,
            "VM identities include retail's dedicated main allocation"
        );
        assert!(ObjectHandle::new(crate::object_arena::OBJECT_POOL_CAPACITY as u16).is_some());
        assert!(ObjectHandle::new(MAX_OBJECTS as u16).is_none());

        let code = CodeAddress {
            segment: CodeSegment::Global,
            pc: CODE_REFERENCE_PC_BITS as usize,
        };
        let code_word = code.to_word();
        assert_eq!(
            code_word,
            CODE_REFERENCE_TAG
                | CODE_REFERENCE_GLOBAL
                | (CODE_REFERENCE_PC_BITS << CODE_REFERENCE_PC_SHIFT)
        );
        assert_eq!(CodeAddress::from_word(code_word), Some(code));

        let storage = StorageReference::checked(
            handle((MAX_OBJECTS - 1) as u16),
            StorageRegion::Constant,
            STORAGE_REFERENCE_INDEX_BITS as usize,
        )
        .unwrap();
        let storage_word = storage.to_word();
        assert_eq!(
            storage_word,
            STORAGE_REFERENCE_TAG
                | ((StorageRegion::Constant as u32) << STORAGE_REFERENCE_REGION_SHIFT)
                | (((MAX_OBJECTS - 1) as u32) << STORAGE_REFERENCE_OBJECT_SHIFT)
                | (STORAGE_REFERENCE_INDEX_BITS << STORAGE_REFERENCE_INDEX_SHIFT)
        );
        assert_eq!(StorageReference::from_word(storage_word), Some(storage));

        let pool_storage =
            StorageReference::retail_pool_register((MAX_OBJECTS - 1) as u8, REGISTER_COUNT - 1)
                .unwrap();
        let pool_storage_word = pool_storage.to_word();
        assert_eq!(
            pool_storage_word,
            RETAIL_POOL_STORAGE_REFERENCE_TAG
                | (((MAX_OBJECTS - 1) as u32) << RETAIL_POOL_STORAGE_REFERENCE_SLOT_SHIFT)
                | (((REGISTER_COUNT - 1) as u32) << RETAIL_POOL_STORAGE_REFERENCE_REGISTER_SHIFT)
        );
        assert_eq!(
            StorageReference::from_word(pool_storage_word),
            Some(pool_storage)
        );
        assert_eq!(pool_storage.object(), None);
        assert_eq!(
            pool_storage.retail_pool_slot(),
            Some((MAX_OBJECTS - 1) as u8)
        );
        assert_eq!(CodeAddress::from_word(pool_storage_word), None);
        assert_eq!(AnimationReference::from_word(pool_storage_word), None);
        assert_eq!(EntityReference::from_word(pool_storage_word), None);
        assert_eq!(EntryReference::from_word(pool_storage_word), None);
        assert_eq!(CollisionObjectReference::from_word(pool_storage_word), None);
        assert_eq!(EventArgumentsReference::from_word(pool_storage_word), None);

        let entity = EntityReference {
            slot: ENTITY_REFERENCE_SLOT_BITS,
        };
        let entity_word = entity.to_word();
        assert_eq!(
            entity_word,
            ENTITY_REFERENCE_TAG | ENTITY_REFERENCE_PAYLOAD_MASK
        );
        assert_eq!(EntityReference::from_word(entity_word), Some(entity));

        let entry = EntryReference {
            slot: ENTRY_REFERENCE_SLOT_BITS,
        };
        let entry_word = entry.to_word();
        assert_eq!(
            entry_word,
            ENTRY_REFERENCE_TAG | ENTRY_REFERENCE_PAYLOAD_MASK
        );
        assert_eq!(EntryReference::from_word(entry_word), Some(entry));

        for word in [
            code_word,
            storage_word,
            pool_storage_word,
            entity_word,
            entry_word,
        ] {
            assert_eq!(word & 3, 0, "logical pointer tokens remain word-aligned");
            assert!(!Eid::from_raw(word).is_named());
            assert!(matches!(
                crust_formats::binary::EntryRef::from_raw(word),
                crust_formats::binary::EntryRef::Offset(_)
            ));
        }
    }

    #[test]
    fn aligned_tagged_references_reject_low_bits_and_reserved_payloads() {
        let code_word = CodeAddress {
            segment: CodeSegment::External,
            pc: 7,
        }
        .to_word();
        let storage_word = StorageReference::checked(
            handle(3),
            StorageRegion::Register,
            process_register::STATE_STAMP,
        )
        .unwrap()
        .to_word();
        let pool_storage_word = StorageReference::retail_pool_register(3, 7)
            .unwrap()
            .to_word();
        let entity_word = EntityReference { slot: 7 }.to_word();
        let entry_word = EntryReference { slot: 7 }.to_word();

        for low_bits in 1..=3 {
            assert_eq!(CodeAddress::from_word(code_word | low_bits), None);
            assert_eq!(StorageReference::from_word(storage_word | low_bits), None);
            assert_eq!(
                StorageReference::from_word(pool_storage_word | low_bits),
                None
            );
            assert_eq!(EntityReference::from_word(entity_word | low_bits), None);
            assert_eq!(EntryReference::from_word(entry_word | low_bits), None);
        }
        assert_eq!(
            CodeAddress::from_word(CODE_REFERENCE_TAG | 0x0001_0000),
            None
        );
        assert_eq!(
            StorageReference::checked(
                handle(0),
                StorageRegion::Internal,
                (STORAGE_REFERENCE_INDEX_BITS + 1) as usize,
            ),
            Err(VmError::InvalidStorageReference(STORAGE_REFERENCE_TAG))
        );
        assert_eq!(
            StorageReference::retail_pool_register(MAX_OBJECTS as u8, 0),
            Err(VmError::InvalidStorageReference(
                RETAIL_POOL_STORAGE_REFERENCE_TAG
            ))
        );
        assert_eq!(
            StorageReference::retail_pool_register(0, REGISTER_COUNT),
            Err(VmError::InvalidStorageReference(
                RETAIL_POOL_STORAGE_REFERENCE_TAG
            ))
        );
        assert_eq!(
            StorageReference::from_word(
                RETAIL_POOL_STORAGE_REFERENCE_TAG
                    | ((MAX_OBJECTS as u32) << RETAIL_POOL_STORAGE_REFERENCE_SLOT_SHIFT)
            ),
            None
        );
        assert_eq!(
            StorageReference::from_word(
                RETAIL_POOL_STORAGE_REFERENCE_TAG
                    | ((REGISTER_COUNT as u32) << RETAIL_POOL_STORAGE_REFERENCE_REGISTER_SHIFT)
            ),
            None
        );
        assert_eq!(
            StorageReference::from_word(pool_storage_word | 0x0080_0000),
            None,
            "reserved pool-reference payload bits must remain zero"
        );

        let named_eid_word = entry_word | 1;
        assert!(Eid::from_raw(named_eid_word).is_named());
        assert_eq!(EntryReference::from_word(named_eid_word), None);
    }

    #[test]
    fn collision_object_references_validate_alignment_reserved_bits_and_runtime_range() {
        let object = handle((MAX_OBJECTS - 1) as u16);
        let reference = CollisionObjectReference::new(object);
        let word = reference.to_word();
        assert_eq!(
            word,
            COLLISION_OBJECT_REFERENCE_TAG
                | (((MAX_OBJECTS - 1) as u32) << COLLISION_OBJECT_REFERENCE_SHIFT)
        );
        assert_eq!(CollisionObjectReference::from_word(word), Some(reference));
        assert_eq!(reference.object(), object);

        for low_bits in 1..=3 {
            assert_eq!(CollisionObjectReference::from_word(word | low_bits), None);
        }
        assert_eq!(
            CollisionObjectReference::from_word(
                COLLISION_OBJECT_REFERENCE_TAG | ((MAX_OBJECTS as u32) << 2)
            ),
            None,
            "the first identity beyond the pool plus dedicated main is rejected"
        );
        assert_eq!(
            CollisionObjectReference::from_word(COLLISION_OBJECT_REFERENCE_TAG | (1 << 9)),
            None,
            "bits outside the seven-bit shifted handle are reserved"
        );
    }

    #[test]
    fn event_argument_references_are_aligned_generation_checked_scopes() {
        let mut machine = Machine::new(0);
        assert_eq!(machine.enter_event_arguments_scope(None), Ok(None));

        let empty = machine
            .enter_event_arguments_scope(Some(&[]))
            .unwrap()
            .unwrap();
        assert_ne!(empty.to_word(), 0, "owned empty argv is not a null pointer");
        assert_eq!(empty.to_word() & 3, 0);
        for low_bits in 1..=3 {
            assert_eq!(
                EventArgumentsReference::from_word(empty.to_word() | low_bits),
                None
            );
        }

        let nested = machine
            .enter_event_arguments_scope(Some(&[10, 20]))
            .unwrap()
            .unwrap();
        assert_eq!(machine.event_argument(nested, 0), Ok(10));
        assert_eq!(machine.event_argument(nested, 1), Ok(20));
        assert_eq!(
            machine.event_argument(nested, 2),
            Err(VmError::EventArgumentOutOfBounds {
                reference: nested.to_word(),
                index: 2,
                len: 2,
            })
        );
        assert_eq!(
            machine.leave_event_arguments_scope(Some(empty)),
            Err(VmError::EventArgumentScopeMismatch(empty.to_word()))
        );
        machine.leave_event_arguments_scope(Some(nested)).unwrap();
        machine.leave_event_arguments_scope(Some(empty)).unwrap();
        assert_eq!(
            machine.event_argument(nested, 0),
            Err(VmError::InvalidEventArgumentsReference(nested.to_word()))
        );
        assert_eq!(
            machine.enter_event_arguments_scope(Some(&[0; MAX_EVENT_ARGUMENTS + 1])),
            Err(VmError::EventArgumentsTooLong(MAX_EVENT_ARGUMENTS + 1))
        );
    }

    #[test]
    fn packed_misc_earg_reads_real_fp_minus_one_and_checks_signed_indices() {
        let h = handle(0);
        let mut machine = Machine::new(0);
        let reference = machine
            .enter_event_arguments_scope(Some(&[0x1122_3344, 0x5566_7788]))
            .unwrap()
            .unwrap();
        // Legal N. Sanity Crash/WillC word: misc primary zero, argv[0],
        // frame-relative GOP B `fp[-1]`.
        let mut object = VmObject::new(h, vec![0x1c00_5b7f]).unwrap();
        object
            .initialize_arguments(&[0x1500, reference.to_word()])
            .unwrap();
        machine.insert_object(object).unwrap();
        assert_eq!(
            machine.run(h, 1).unwrap(),
            Execution {
                reason: HaltReason::BudgetExhausted,
                steps: 1,
            }
        );
        assert_eq!(
            machine.object(h).unwrap().stack().last(),
            Some(&0x1122_3344)
        );

        let negative = handle(1);
        let mut object = VmObject::new(negative, vec![misc(0, -1, REG1)]).unwrap();
        object.set_register(1, reference.to_word()).unwrap();
        machine.insert_object(object).unwrap();
        assert_eq!(
            machine.run(negative, 1),
            Err(VmError::EventArgumentOutOfBounds {
                reference: reference.to_word(),
                index: -1,
                len: 2,
            })
        );
    }

    #[test]
    fn copied_event_argument_keeps_physical_pool_identity_across_compact_aba() {
        let original = handle(0);
        let actor = handle(1);
        let replacement = handle(2);
        let pointer = CollisionObjectReference::new(original).to_word();
        let mut original_object = VmObject::new(original, vec![0]).unwrap();
        original_object.set_register(8, 0x1111_1100).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(original_object).unwrap();
        machine.bind_retail_pool_slot(original, 5).unwrap();
        let reference = machine
            .enter_event_arguments_scope_with_pool_slots(Some(&[pointer]), Some(&[Some(5)]))
            .unwrap()
            .unwrap();
        let mut actor_object = VmObject::new(
            actor,
            vec![
                misc(0, 0, REG1),
                Instruction::encode(0x11, STACK, 0x0e04),
                Instruction::encode(0x11, 0x0d08, 0x0e17),
            ],
        )
        .unwrap();
        actor_object.set_register(1, reference.to_word()).unwrap();
        machine.insert_object(actor_object).unwrap();

        machine.run(actor, 1).unwrap();
        assert_eq!(
            machine
                .object(actor)
                .unwrap()
                .register_pool_slot(SYNTHETIC_STACK_POINTER),
            Ok(Some(5))
        );
        machine
            .leave_event_arguments_scope(Some(reference))
            .unwrap();
        machine
            .remove_object_from_retail_pool_slot(original, 5)
            .unwrap();

        let mut compact_reuse = VmObject::new(original, vec![0]).unwrap();
        compact_reuse.set_register(8, 0x2222_2200).unwrap();
        machine.insert_object(compact_reuse).unwrap();
        machine.bind_retail_pool_slot(original, 6).unwrap();
        let mut same_slot_replacement = VmObject::new(replacement, vec![0]).unwrap();
        same_slot_replacement.set_register(8, 0x3333_3300).unwrap();
        machine.insert_object(same_slot_replacement).unwrap();
        machine.bind_retail_pool_slot(replacement, 5).unwrap();

        machine.run(actor, 2).unwrap();
        assert_eq!(
            machine.object(actor).unwrap().register(23),
            Ok(0x3333_3300),
            "the copied native argv pointer follows its physical slot, not a reused compact handle"
        );
    }

    #[test]
    fn packed_misc_earg_preserves_null_gop_and_null_argv_distinction() {
        let null_gop = handle(0);
        let null_argv = handle(1);
        let empty_argv = handle(2);
        let mut machine = Machine::new(0);

        machine
            .insert_object(VmObject::new(null_gop, vec![misc(0, 0, 0x0be0)]).unwrap())
            .unwrap();
        assert_eq!(machine.run(null_gop, 1).unwrap().steps, 1);
        assert!(machine.object(null_gop).unwrap().stack().is_empty());

        let mut object = VmObject::new(null_argv, vec![misc(0, 0, REG1)]).unwrap();
        object.set_register(1, 0).unwrap();
        machine.insert_object(object).unwrap();
        assert_eq!(machine.run(null_argv, 1).unwrap().steps, 1);
        assert_eq!(machine.object(null_argv).unwrap().stack(), &[0]);

        let empty = machine
            .enter_event_arguments_scope(Some(&[]))
            .unwrap()
            .unwrap();
        let mut object = VmObject::new(empty_argv, vec![misc(0, 0, REG1)]).unwrap();
        object.set_register(1, empty.to_word()).unwrap();
        machine.insert_object(object).unwrap();
        assert_eq!(
            machine.run(empty_argv, 1),
            Err(VmError::EventArgumentOutOfBounds {
                reference: empty.to_word(),
                index: 0,
                len: 0,
            })
        );
    }

    #[test]
    fn stale_event_argument_generation_cannot_read_reused_stack_slot() {
        let h = handle(0);
        let mut machine = Machine::new(0);
        let stale = machine
            .enter_event_arguments_scope(Some(&[1]))
            .unwrap()
            .unwrap();
        machine.leave_event_arguments_scope(Some(stale)).unwrap();
        let current = machine
            .enter_event_arguments_scope(Some(&[2]))
            .unwrap()
            .unwrap();
        assert_ne!(stale.to_word(), current.to_word());

        let mut object = VmObject::new(h, vec![misc(0, 0, REG1)]).unwrap();
        object.set_register(1, stale.to_word()).unwrap();
        machine.insert_object(object).unwrap();
        assert_eq!(
            machine.run(h, 1),
            Err(VmError::InvalidEventArgumentsReference(stale.to_word()))
        );
    }

    #[test]
    fn process_register_references_alias_links_and_pop_only_selector_31() {
        let h = handle(0);
        let other = handle(1);
        let mut object = VmObject::new(h, vec![control_flow(1, 1, 0, 0, 7)]).unwrap();
        object.set_register(8, 0xfeed_beef).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine
            .insert_object(VmObject::new(other, vec![0]).unwrap())
            .unwrap();

        assert_eq!(
            machine.read_process_register_reference(h, 0),
            Ok(CollisionObjectReference::new(h).to_word())
        );
        assert_eq!(machine.read_process_register_reference(h, 6), Ok(0));
        machine
            .object_mut(h)
            .unwrap()
            .set_link(6, Some(other))
            .unwrap();
        assert_eq!(
            machine.read_process_register_reference(h, 6),
            Ok(CollisionObjectReference::new(other).to_word())
        );
        assert_eq!(
            machine.read_process_register_reference(h, 8),
            Ok(0xfeed_beef)
        );
        machine.push(h, 0x1234).unwrap();
        assert_eq!(machine.read_process_register_reference(h, 0x1f), Ok(0x1234));
        assert!(machine.object(h).unwrap().stack().is_empty());

        // The encoded state-link condition names register zero. Native sees
        // the non-null self pointer stored in that union word.
        assert_eq!(
            machine.run(h, 1).unwrap().reason,
            HaltReason::StateChanged(7)
        );
    }

    #[test]
    fn ordinary_input_gops_observe_typed_process_link_aliases() {
        let h = handle(0);
        let collider = handle(1);
        // Exact Crash shared pc 872 word: `ANDL pop(), collider`. Retail's
        // `obj->regs[6]` aliases the collider pointer, while Rust owns that
        // pointer as a checked link instead of a scalar register word.
        let mut object = VmObject::new(h, vec![0x05e1_fe06]).unwrap();
        object.set_link(6, Some(collider)).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine
            .insert_object(VmObject::new(collider, vec![0x8289_4000]).unwrap())
            .unwrap();
        machine.push(h, 1).unwrap();

        machine.run(h, 1).unwrap();

        assert_eq!(machine.object(h).unwrap().stack(), &[1]);
    }

    #[test]
    fn exact_boxs_link_copy_targets_the_found_object() {
        let requester = handle(0);
        let old_interrupter = handle(1);
        let found = handle(2);
        // Exact BoxsC pc 222 and 224: copy misc-seven's checked result into
        // the interrupter word, then send event 0x800 through that link.
        let mut object = VmObject::new(requester, vec![0x11e1_fe07, 0x87e0_0808]).unwrap();
        object.set_link(7, Some(old_interrupter)).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine
            .insert_object(VmObject::new(old_interrupter, vec![0x8289_4000]).unwrap())
            .unwrap();
        machine
            .insert_object(VmObject::new(found, vec![0x8289_4000]).unwrap())
            .unwrap();
        machine
            .push(requester, CollisionObjectReference::new(found).to_word())
            .unwrap();
        let mut delivered = None;

        machine
            .run_with_host_requests(requester, 2, |_machine, request| {
                let VmHostRequest::SendEvent(request) = request else {
                    return Err(VmError::MissingHostEffect);
                };
                delivered = Some(request);
                Ok(())
            })
            .unwrap();

        assert_eq!(machine.object(requester).unwrap().links[7], Some(found));
        let delivered = delivered.expect("BoxsC sends its chain event");
        assert_eq!(
            delivered.target,
            SendEventTarget::Direct { recipient: found }
        );
        assert_eq!(delivered.event, 0x800);
    }

    #[test]
    fn process_link_output_preserves_non_pointer_union_words() {
        let requester = handle(0);
        let existing = handle(1);
        let mut object = VmObject::new(requester, vec![0x11e1_fe07]).unwrap();
        object.set_link(7, Some(existing)).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine
            .insert_object(VmObject::new(existing, vec![0x8289_4000]).unwrap())
            .unwrap();
        machine.push(requester, 1).unwrap();

        machine.run(requester, 1).unwrap();

        assert_eq!(machine.object(requester).unwrap().links[7], None);
        assert_eq!(machine.object(requester).unwrap().register(7), Ok(1));
    }

    #[test]
    fn animation_references_preserve_unaligned_byte_offsets() {
        // Opcode 0x27 forms a `uint8_t *` into animation item five. Unlike
        // code, storage and entry pointers, that source address has no
        // word-alignment contract, so its exact byte offset remains payload.
        let reference = AnimationReference::checked(1, 4).unwrap();
        assert_eq!(reference.to_word(), ANIMATION_REFERENCE_TAG | 1);
        assert_eq!(
            AnimationReference::from_word(reference.to_word()),
            Some(reference)
        );
        assert_eq!(reference.offset(), 1);
    }

    #[test]
    fn current_animation_reference_is_exact_null_aware_and_bounds_checked() {
        let h = handle(0);
        let mut object = VmObject::new(h, vec![0]).unwrap();
        object.bind_animation_data(&[0; 7]);
        assert_eq!(object.animation_reference(), Ok(None));

        let unaligned = AnimationReference::checked(3, 7).unwrap();
        object
            .set_register(process_register::ANIMATION_SEQUENCE, unaligned.to_word())
            .unwrap();
        assert_eq!(object.animation_reference(), Ok(Some(unaligned)));
        assert_eq!(object.animation_data(unaligned), Ok(&[0; 4][..]));

        let at_end = AnimationReference::from_word(ANIMATION_REFERENCE_TAG | 7).unwrap();
        object
            .set_register(process_register::ANIMATION_SEQUENCE, at_end.to_word())
            .unwrap();
        assert_eq!(
            object.animation_reference(),
            Err(VmError::InvalidAnimationReference(at_end.to_word()))
        );

        object
            .set_register(process_register::ANIMATION_SEQUENCE, 0x1234_5678)
            .unwrap();
        assert_eq!(
            object.animation_reference(),
            Err(VmError::InvalidAnimationReference(0x1234_5678))
        );
    }

    #[test]
    fn lea_created_type_zero_process_animation_is_live_aliased_and_checked() {
        let h = handle(0);
        let descriptor_index = 65;
        let descriptor =
            StorageReference::checked(h, StorageRegion::Register, descriptor_index).unwrap();
        let mut object = VmObject::new(h, vec![0]).unwrap();
        object.set_register(descriptor_index, 0).unwrap();
        object
            .set_register(process_register::ANIMATION_SEQUENCE, descriptor.to_word())
            .unwrap();

        assert_eq!(
            object.animation_source(),
            Ok(Some(AnimationSource::Process(ProcessAnimationReference {
                storage: descriptor,
                kind: ProcessAnimationKind::NoDraw,
            })))
        );
        assert_eq!(
            object.animation_reference(),
            Ok(None),
            "the compatibility item-five view must not mislabel process words as an asset offset"
        );

        object.set_register(descriptor_index, 1).unwrap();
        assert_eq!(
            object.animation_source(),
            Err(VmError::InvalidAnimationReference(descriptor.to_word())),
            "a live mutation to a malformed known payload is revalidated instead of silently hidden"
        );
    }

    #[test]
    fn lea_created_known_process_animations_retain_complete_bounded_payloads() {
        let h = handle(0);
        let page = Eid::from_name("pageT").unwrap();
        let model = Eid::from_name("model").unwrap();
        let mut object = VmObject::new(h, vec![0]).unwrap();

        let mut vertex = vec![1, 0x12, 7, 0x34];
        vertex.extend_from_slice(&model.raw().to_le_bytes());
        write_animation_bytes(&mut object, StorageRegion::Internal, 80, &vertex);
        let AnimationSource::Process(source) = object.animation_source().unwrap().unwrap() else {
            panic!("expected process vertex animation");
        };
        let ProcessAnimationKind::Vertex(animation) = source.kind() else {
            panic!("expected type-one descriptor");
        };
        assert_eq!(animation.header.length, 7);
        assert_eq!(animation.model_eid, model);

        let mut sprite = vec![2, 0, 2, 0];
        sprite.extend_from_slice(&page.raw().to_le_bytes());
        for raw in [0x1111_0001_u32, 0x2222_0002, 0x3333_0003, 0x4444_0004] {
            sprite.extend_from_slice(&raw.to_le_bytes());
        }
        write_animation_bytes(&mut object, StorageRegion::Internal, 90, &sprite);
        let AnimationSource::Process(source) = object.animation_source().unwrap().unwrap() else {
            panic!("expected process sprite animation");
        };
        let ProcessAnimationKind::Sprite(animation) = source.kind() else {
            panic!("expected type-two descriptor");
        };
        assert_eq!(animation.texture_page, page);
        assert_eq!(animation.frames.len(), 2);
        assert_eq!(animation.frames[1].color.raw(), 0x3333_0003);
        assert_eq!(animation.frames[1].region.raw(), 0x4444_0004);

        write_animation_bytes(
            &mut object,
            StorageRegion::Register,
            100,
            &[3, 0xaa, 95, 0xbb],
        );
        let AnimationSource::Process(source) = object.animation_source().unwrap().unwrap() else {
            panic!("expected process font animation");
        };
        let ProcessAnimationKind::Font(header) = source.kind() else {
            panic!("expected type-three descriptor");
        };
        assert_eq!(header.length, 95);
        assert_eq!(header.reserved_1, 0xaa);
        assert_eq!(header.reserved_3, 0xbb);

        let mut text = vec![4, 1, 2, 3];
        text.extend_from_slice(&0x1234_5678_u32.to_le_bytes());
        text.extend_from_slice(&7_u32.to_le_bytes());
        text.extend_from_slice(b"ONE\0TWO\0");
        write_animation_bytes(&mut object, StorageRegion::Register, 120, &text);
        let AnimationSource::Process(source) = object.animation_source().unwrap().unwrap() else {
            panic!("expected process text animation");
        };
        let ProcessAnimationKind::Text(animation) = source.kind() else {
            panic!("expected type-four descriptor");
        };
        assert_eq!(animation.unknown_word, 0x1234_5678);
        assert_eq!(animation.font_word_offset, 7);
        assert_eq!(animation.terms, [b"ONE".to_vec(), b"TWO".to_vec()]);

        let mut fragments = vec![5, 0, 2, 0];
        fragments.extend_from_slice(&page.raw().to_le_bytes());
        fragments.extend_from_slice(&1_u32.to_le_bytes());
        for frame in 0..2_u32 {
            fragments.extend_from_slice(&(0x0102_0000 | frame).to_le_bytes());
            fragments.extend_from_slice(&(0x0304_0000 | frame).to_le_bytes());
            for bound in [-(frame as i16) - 1, 2, 3, 4] {
                fragments.extend_from_slice(&bound.to_le_bytes());
            }
        }
        write_animation_bytes(&mut object, StorageRegion::Register, 140, &fragments);
        let AnimationSource::Process(source) = object.animation_source().unwrap().unwrap() else {
            panic!("expected process fragment animation");
        };
        let ProcessAnimationKind::Fragment(animation) = source.kind() else {
            panic!("expected type-five descriptor");
        };
        assert_eq!(animation.texture_page, page);
        assert_eq!(animation.fragments_per_frame, 1);
        assert_eq!(animation.frame(1).unwrap()[0].bounds, [-2, 2, 3, 4]);
    }

    #[test]
    fn process_animation_accepts_owned_tables_and_rejects_unstable_aliases() {
        let h = handle(0);
        let foreign = handle(1);
        let foreign_reference =
            StorageReference::checked(foreign, StorageRegion::Register, 65).unwrap();
        let table_reference = StorageReference::checked(h, StorageRegion::Internal, 65).unwrap();
        let external_reference = StorageReference::checked(h, StorageRegion::External, 65).unwrap();
        let constant_reference = StorageReference::checked(h, StorageRegion::Constant, 0).unwrap();
        let mut object = VmObject::new(h, vec![0]).unwrap();

        object.set_internal(65, 0x4ac8_2073).unwrap();
        object
            .set_register(
                process_register::ANIMATION_SEQUENCE,
                table_reference.to_word(),
            )
            .unwrap();
        let AnimationSource::Process(source) = object.animation_source().unwrap().unwrap() else {
            panic!("expected owned internal-table animation");
        };
        assert_eq!(source.storage(), table_reference);
        assert_eq!(*source.kind(), ProcessAnimationKind::NoDraw);

        for invalid in [foreign_reference, external_reference, constant_reference] {
            object
                .set_register(process_register::ANIMATION_SEQUENCE, invalid.to_word())
                .unwrap();
            assert_eq!(
                object.animation_source(),
                Err(VmError::InvalidAnimationReference(invalid.to_word()))
            );
        }
    }

    #[test]
    fn machine_animation_alias_follows_the_shared_rotating_constant_slot() {
        let h = handle(0);
        let mut machine = Machine::new(0);
        machine
            .insert_object(
                VmObject::new(
                    h,
                    vec![
                        // LEA fractional immediate 0x10 into anim_seq. The
                        // input cursor starts at zero and selects slot one.
                        Instruction::encode(0x14, 0x0a01, 0x0e2a),
                        // Solid subop six translates immediate 0x200 through
                        // input slot zero and then output slot one, replacing
                        // the exact pointee through the other cursor.
                        0x8e18_0802,
                    ],
                )
                .unwrap(),
            )
            .unwrap();

        machine.run(h, 1).unwrap();
        let reference = StorageReference::from_word(
            machine
                .object(h)
                .unwrap()
                .register(process_register::ANIMATION_SEQUENCE)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(reference.region(), StorageRegion::Constant);
        assert_eq!(reference.index(), 1);
        assert_eq!(machine.read_storage_reference(reference), Ok(0x10));
        let AnimationSource::Process(first) = machine.animation_source(h).unwrap().unwrap() else {
            panic!("the immediate alias must remain a process animation source");
        };
        assert_eq!(first.storage(), reference);
        assert_eq!(*first.kind(), ProcessAnimationKind::NoDraw);

        machine.run(h, 1).unwrap();
        assert_eq!(machine.read_storage_reference(reference), Ok(0x200));
        let AnimationSource::Process(second) = machine.animation_source(h).unwrap().unwrap() else {
            panic!("the rotated immediate alias must remain a process animation source");
        };
        assert_eq!(second.storage(), reference);
        assert_eq!(*second.kind(), ProcessAnimationKind::NoDraw);
    }

    #[test]
    fn machine_animation_alias_tracks_foreign_physical_pool_storage_lifetime() {
        let original = handle(0);
        let actor = handle(1);
        let replacement = handle(2);
        let original_header = u32::from_le_bytes([3, 0xaa, 7, 0xbb]);
        let replacement_header = u32::from_le_bytes([3, 0xcc, 9, 0xdd]);

        let mut original_object = VmObject::new(original, vec![0]).unwrap();
        original_object.set_register(8, original_header).unwrap();
        let mut actor_object =
            VmObject::new(actor, vec![Instruction::encode(0x14, 0x0d08, 0x0e2a)]).unwrap();
        actor_object.set_link(4, Some(original)).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(original_object).unwrap();
        machine.bind_retail_pool_slot(original, 7).unwrap();
        machine.insert_object(actor_object).unwrap();
        machine.run(actor, 1).unwrap();

        let reference = StorageReference::from_word(
            machine
                .object(actor)
                .unwrap()
                .register(process_register::ANIMATION_SEQUENCE)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(reference.retail_pool_slot(), Some(7));
        let AnimationSource::Process(live) = machine.animation_source(actor).unwrap().unwrap()
        else {
            panic!("the linked pool alias must resolve while its object is live");
        };
        let ProcessAnimationKind::Font(live_header) = live.kind() else {
            panic!("the original pool words must decode as a font descriptor");
        };
        assert_eq!(live.storage(), reference);
        assert_eq!(live_header.length, 7);
        assert_eq!(live_header.reserved_1, 0xaa);
        assert_eq!(live_header.reserved_3, 0xbb);

        machine
            .remove_object_from_retail_pool_slot(original, 7)
            .unwrap();
        let AnimationSource::Process(retained) = machine.animation_source(actor).unwrap().unwrap()
        else {
            panic!("the linked pool alias must resolve retained free-slot storage");
        };
        let ProcessAnimationKind::Font(retained_header) = retained.kind() else {
            panic!("the retained pool words must preserve the descriptor");
        };
        assert_eq!(retained.storage(), reference);
        assert_eq!(retained_header.length, 7);
        assert_eq!(retained_header.reserved_1, 0xaa);
        assert_eq!(retained_header.reserved_3, 0xbb);

        let mut replacement_object = VmObject::new(replacement, vec![0]).unwrap();
        replacement_object
            .set_register(8, replacement_header)
            .unwrap();
        machine.insert_object(replacement_object).unwrap();
        machine.bind_retail_pool_slot(replacement, 7).unwrap();
        let AnimationSource::Process(reused) = machine.animation_source(actor).unwrap().unwrap()
        else {
            panic!("the linked pool alias must follow exact-slot reuse");
        };
        let ProcessAnimationKind::Font(reused_header) = reused.kind() else {
            panic!("the replacement pool words must decode as a font descriptor");
        };
        assert_eq!(reused.storage(), reference);
        assert_eq!(reused_header.length, 9);
        assert_eq!(reused_header.reserved_1, 0xcc);
        assert_eq!(reused_header.reserved_3, 0xdd);
    }

    #[test]
    fn unrepresented_and_malformed_animation_aliases_fail_without_mutation() {
        let actor = handle(0);
        let foreign = handle(1);

        let external = StorageReference::checked(actor, StorageRegion::External, 8).unwrap();
        let mut external_object = VmObject::new(actor, vec![0]).unwrap();
        external_object.set_external(8, 0x0007_aa03).unwrap();
        external_object
            .set_register(process_register::ANIMATION_SEQUENCE, external.to_word())
            .unwrap();
        let mut external_machine = Machine::new(0);
        external_machine.insert_object(external_object).unwrap();
        let external_snapshot = external_machine.clone();
        assert_eq!(
            external_machine.animation_source(actor),
            Err(VmError::InvalidAnimationReference(external.to_word()))
        );
        assert_eq!(external_machine, external_snapshot);

        let foreign_reference =
            StorageReference::checked(foreign, StorageRegion::Register, 8).unwrap();
        let mut foreign_object = VmObject::new(foreign, vec![0]).unwrap();
        foreign_object.set_register(8, 0x0007_aa03).unwrap();
        let mut actor_object = VmObject::new(actor, vec![0]).unwrap();
        actor_object
            .set_register(
                process_register::ANIMATION_SEQUENCE,
                foreign_reference.to_word(),
            )
            .unwrap();
        let mut foreign_machine = Machine::new(0);
        foreign_machine.insert_object(foreign_object).unwrap();
        foreign_machine.insert_object(actor_object).unwrap();
        let foreign_snapshot = foreign_machine.clone();
        assert_eq!(
            foreign_machine.animation_source(actor),
            Err(VmError::InvalidAnimationReference(
                foreign_reference.to_word()
            ))
        );
        assert_eq!(foreign_machine, foreign_snapshot);

        let uninitialized_pool = StorageReference::retail_pool_register(7, 8).unwrap();
        let mut uninitialized_object = VmObject::new(actor, vec![0]).unwrap();
        uninitialized_object
            .set_register(
                process_register::ANIMATION_SEQUENCE,
                uninitialized_pool.to_word(),
            )
            .unwrap();
        let mut uninitialized_machine = Machine::new(0);
        uninitialized_machine
            .insert_object(uninitialized_object)
            .unwrap();
        let uninitialized_snapshot = uninitialized_machine.clone();
        assert_eq!(
            uninitialized_machine.animation_source(actor),
            Err(VmError::InvalidAnimationReference(
                uninitialized_pool.to_word()
            ))
        );
        assert_eq!(uninitialized_machine, uninitialized_snapshot);

        let constant = StorageReference::checked(actor, StorageRegion::Constant, 1).unwrap();
        let mut malformed_object = VmObject::new(actor, vec![0]).unwrap();
        malformed_object
            .set_register(process_register::ANIMATION_SEQUENCE, constant.to_word())
            .unwrap();
        let mut malformed_machine = Machine::new(0);
        malformed_machine.insert_object(malformed_object).unwrap();
        malformed_machine.operand_constants[1] = 1;
        let malformed_snapshot = malformed_machine.clone();
        assert_eq!(
            malformed_machine.animation_source(actor),
            Err(VmError::InvalidAnimationReference(constant.to_word()))
        );
        assert_eq!(malformed_machine, malformed_snapshot);
    }

    #[test]
    fn arithmetic_wraps_and_division_errors_are_defined() {
        let h = handle(0);
        let code = vec![Instruction::encode(0x00, REG0, REG1)];
        let mut object = VmObject::new(h, code).unwrap();
        object.set_register(0, 1).unwrap();
        object.set_register(1, u32::MAX).unwrap();
        let mut machine = Machine::new(16);
        machine.insert_object(object).unwrap();
        assert_eq!(
            machine.run(h, 1).unwrap().reason,
            HaltReason::BudgetExhausted
        );
        assert_eq!(machine.object(h).unwrap().stack(), &[0]);

        let h = handle(1);
        let mut object = VmObject::new(h, vec![Instruction::encode(0x03, REG0, REG1)]).unwrap();
        object.set_register(0, 0).unwrap();
        object.set_register(1, 10).unwrap();
        machine.insert_object(object).unwrap();
        assert_eq!(machine.run(h, 1), Err(VmError::DivisionByZero));
    }

    #[test]
    fn signed_comparisons_evaluate_operand_a_against_operand_b() {
        let h = handle(0);
        let code = (0x09..=0x0c)
            .map(|opcode| Instruction::encode(opcode, REG0, REG1))
            .collect();
        let mut object = VmObject::new(h, code).unwrap();
        object.set_register(0, (-2_i32) as u32).unwrap();
        object.set_register(1, 1).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        machine.run(h, 4).unwrap();
        assert_eq!(machine.object(h).unwrap().stack(), &[0, 0, 1, 1]);
    }

    #[test]
    fn exact_crash_exit_shift_masks_the_mips_variable_count() {
        let h = handle(0);
        // Exact Crash shared pc 2461 word reached by state 32 during N.
        // Sanity Beach's warp: `SHA pop(), ireg[0x170]` with a count of 67.
        let mut object = VmObject::new(h, vec![0x15e1_f05c]).unwrap();
        object.set_internal(0x5c, 0x123).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.push(h, 67).unwrap();

        machine.run(h, 1).unwrap();

        assert_eq!(machine.object(h).unwrap().stack(), &[0x918]);
    }

    #[test]
    fn state_34_animation_bound_keeps_looping_below_the_limit() {
        let h = handle(0);
        // Exact Wil9C pc 16 word: signed `0x1a00 >= pop(animation_frame)`.
        let object = VmObject::new(h, vec![0x0a81_ae1f]).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.push(h, 0x100).unwrap();

        machine.run(h, 1).unwrap();
        assert_eq!(machine.object(h).unwrap().stack(), &[1]);
    }

    #[test]
    fn state_34_status_mask_tests_operand_a_as_the_subset() {
        let h = handle(0);
        // Exact Wil9C pc 106 word: `(0x20 & status_a) == 0x20`.
        let mut object = VmObject::new(h, vec![0x0fa0_2e1a]).unwrap();
        object
            .set_register(process_register::STATUS_A, 0x0006_0821)
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        machine.run(h, 1).unwrap();
        assert_eq!(machine.object(h).unwrap().stack(), &[1]);
    }

    #[test]
    fn shadow_seek_stack_double_pops_speed_then_target() {
        let h = handle(0);
        // Exact ShadC state-1 pc 51 word: seek R74 toward the stacked target
        // using the absolute value of the stacked speed.
        let mut object = VmObject::new(h, vec![0x22bf_0e4a]).unwrap();
        object.set_register(74, 100).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.push(h, 300).unwrap();
        machine.push(h, (-50_i32) as u32).unwrap();

        machine.run(h, 1).unwrap();
        assert_eq!(machine.object(h).unwrap().stack(), &[150]);
    }

    #[test]
    fn dynamic_rotate_stack_double_pops_speed_then_target() {
        let h = handle(0);
        // Exact GOOL form reached by the Generator Room machinery scripts.
        let mut object = VmObject::new(h, vec![0x25bf_0e0b]).unwrap();
        object
            .set_register(process_register::ROTATION_Y, 0)
            .unwrap();
        let mut machine = Machine::new(0);
        machine.set_ticks_per_frame(34);
        machine.insert_object(object).unwrap();
        machine.push(h, 0x400).unwrap();
        machine.push(h, 0x300).unwrap();

        machine.run(h, 1).unwrap();
        assert_eq!(machine.object(h).unwrap().stack(), &[25]);
    }

    #[test]
    fn signed_comparisons_translate_stack_operands_a_then_b() {
        let h = handle(0);
        let object = VmObject::new(h, vec![Instruction::encode(0x0b, STACK, STACK)]).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.push(h, 3).unwrap();
        machine.push(h, 7).unwrap();

        machine.run(h, 1).unwrap();
        assert_eq!(machine.object(h).unwrap().stack(), &[0]);
    }

    #[test]
    fn retail_scalar_opcodes_use_deterministic_frame_context() {
        let h = handle(0);
        let code = vec![
            Instruction::encode(0x10, REG0, REG1),
            Instruction::encode(0x10, REG0, REG1),
            Instruction::encode(0x13, REG0 + 2, REG0 + 3),
            Instruction::encode(0x13, REG0 + 2, REG0 + 3),
            Instruction::encode(0x1b, REG0 + 4, REG0 + 5),
            Instruction::encode(0x1d, REG0 + 6, REG0 + 7),
            Instruction::encode(0x1e, REG0 + 8, REG0 + 9),
        ];
        let mut object = VmObject::new(h, code).unwrap();
        for (index, value) in [100, 0, 0x300, 0, 1024, 100, 0x1000, 0x1000, 4, 5]
            .into_iter()
            .enumerate()
        {
            object.set_register(index, value).unwrap();
        }
        let mut machine = Machine::new(0);
        machine.set_random_seed(12_345);
        machine.set_ticks_per_frame(34);
        machine.set_draw_count(3);
        machine.insert_object(object).unwrap();

        machine.run(h, 7).unwrap();
        assert_eq!(machine.object(h).unwrap().register(3), Ok(0x200));
        assert_eq!(
            machine.object(h).unwrap().stack(),
            &[56, 67, 0x100, 0x200, 134, 0x1000, 2]
        );
    }

    #[test]
    fn retail_frame_context_keeps_scalar_globals_and_camera_pose_synchronized() {
        let mut machine = Machine::new(256);
        machine.initialize_retail_level_globals(LevelId::N_SANITY_BEACH);
        machine.set_frame_timing(33, 34);
        machine.set_draw_count(19);
        machine.set_retail_frame_context(0x500, 0x1234);
        machine.set_transform_vectors_camera(RetailTransformVectorsCamera::from_retail_pose(
            [-256, 512, 768],
            [0x111, -0x222, 0x333],
            500,
        ));

        assert_eq!(machine.ticks_per_frame, 34);
        assert_eq!(
            machine.global_word(CURRENT_LEVEL_GLOBAL),
            Ok(LevelId::N_SANITY_BEACH.get() << 8)
        );
        assert_eq!(machine.global_word(TICKS_CURRENT_FRAME_GLOBAL), Ok(33));
        assert_eq!(machine.global_word(DRAW_COUNT_GLOBAL), Ok(19));
        assert_eq!(machine.global_word(GAME_STATE_GLOBAL), Ok(0x500));
        assert_eq!(machine.global_word(CHECKPOINT_ID_GLOBAL), Ok(u32::MAX));
        assert_eq!(machine.global_word(CAMERA_ROTATION_GLOBAL), Ok(0x234));
        assert_eq!(
            [0_usize, 1, 2].map(|axis| machine
                .global_word(CAMERA_TRANSLATION_GLOBAL + axis)
                .unwrap()),
            [(-256_i32) as u32, 512, 768]
        );
        assert_eq!(
            [0_usize, 1, 2].map(|axis| machine
                .global_word(CAMERA_ROTATION_YXZ_GLOBAL + axis)
                .unwrap()),
            [0x111, (-0x222_i32) as u32, 0x333]
        );
    }

    #[test]
    fn retail_frame_latch_preserves_a_term_game_state_write_for_physics() {
        const TERM_GAME_STATE: i32 = 0x500;
        let mut machine = Machine::new(256);
        machine.set_retail_frame_context(0, 0x123);

        // CamUpdate's ordered LevelUpdate may synchronously run a departing
        // TERM program after the camera has written the cutscene state.
        machine
            .set_global_word(GAME_STATE_GLOBAL, TERM_GAME_STATE as u32)
            .unwrap();
        machine.latch_retail_frame_context(TERM_GAME_STATE, 0x1456);

        assert_eq!(
            machine.global_word(GAME_STATE_GLOBAL),
            Ok(TERM_GAME_STATE as u32),
            "latching post-camera physics context must not replay the earlier camera write"
        );
        assert!(!machine.retail_game_state_playing);
        assert_eq!(machine.camera_rotation_xz, 0x456);
        assert_eq!(machine.global_word(CAMERA_ROTATION_GLOBAL), Ok(0x456));
    }

    #[test]
    fn retail_branch_uses_post_fetch_pc_and_cleans_arguments() {
        let h = handle(0);
        let code = vec![
            Instruction::encode(0x00, REG0, REG1),
            Instruction::encode(0x00, REG0, REG1),
            control_flow(0, 0, 0, 1, 1),
            Instruction::encode(0xff, 0, 0),
            Instruction::encode(0x00, REG0, REG1),
        ];
        let mut object = VmObject::new(h, code).unwrap();
        object.set_register(0, 2).unwrap();
        object.set_register(1, 3).unwrap();
        let mut machine = Machine::new(1);
        machine.insert_object(object).unwrap();
        let execution = machine.run(h, 4).unwrap();
        assert_eq!(execution.reason, HaltReason::BudgetExhausted);
        assert_eq!(machine.object(h).unwrap().pc(), 5);
        assert_eq!(machine.object(h).unwrap().stack(), &[5, 5]);
    }

    #[test]
    fn retail_condition_can_pop_and_reuse_within_one_invocation() {
        let h = handle(0);
        let code = vec![
            Instruction::encode(0x00, REG0, REG1),
            control_flow(3, 1, 0x1f, 0, 0),
            control_flow(0, 3, 0, 0, 1),
            Instruction::encode(0xff, 0, 0),
            Instruction::encode(0x00, REG0, REG1),
        ];
        let mut object = VmObject::new(h, code).unwrap();
        object.set_register(0, 2).unwrap();
        object.set_register(1, 3).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        assert_eq!(
            machine.run(h, 4).unwrap().reason,
            HaltReason::BudgetExhausted
        );
        assert_eq!(machine.object(h).unwrap().stack(), &[5]);
    }

    #[test]
    fn reused_condition_does_not_leak_between_invocations() {
        let h = handle(0);
        let code = vec![
            control_flow(3, 1, 0, 0, 0),
            control_flow(0, 3, 0, 0, 1),
            Instruction::encode(0x00, REG0, REG1),
            Instruction::encode(0xff, 0, 0),
        ];
        let mut object = VmObject::new(h, code).unwrap();
        object.set_register(0, 2).unwrap();
        object.set_register(1, 3).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        machine.run(h, 1).unwrap();
        machine.run(h, 2).unwrap();
        assert_eq!(machine.object(h).unwrap().stack(), &[5]);
    }

    #[test]
    fn packed_retail_state_change_yields_for_host_rebinding_and_terminal_return_halts() {
        let state_object = handle(0);
        let return_object = handle(1);
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(state_object, vec![control_flow(1, 0, 0, 0, 7)]).unwrap())
            .unwrap();
        machine
            .insert_object(VmObject::new(return_object, vec![control_flow(2, 0, 0, 0, 0)]).unwrap())
            .unwrap();

        assert_eq!(
            machine.run(state_object, 1),
            Ok(Execution {
                reason: HaltReason::StateChanged(7),
                steps: 1,
            })
        );
        assert_eq!(machine.object(state_object).unwrap().state(), 7);
        assert_eq!(
            machine.effects(),
            &[VmEffect::StateChanged {
                object: state_object,
                state: 7,
            }]
        );
        assert_eq!(
            machine.run(return_object, 1),
            Ok(Execution {
                reason: HaltReason::Halted,
                steps: 1,
            })
        );
    }

    #[test]
    fn retail_state_link_guard_uses_status_target_flags_and_invincibility_augmentation() {
        let ordinary = handle(0);
        let invincible = handle(1);
        let allowed = handle(2);
        let state_link = control_flow(1, 0, 0, 0, 1);
        let mut ordinary_object = VmObject::new(ordinary, vec![state_link]).unwrap();
        ordinary_object.state_flags_by_index = vec![0, 0x20];
        ordinary_object
            .set_register(process_register::STATUS_C, 0x20)
            .unwrap();
        let mut invincible_object = VmObject::new(invincible, vec![state_link]).unwrap();
        invincible_object.state_flags_by_index = vec![0, 0x1000];
        invincible_object
            .set_register(process_register::INVINCIBILITY_STATE, 3)
            .unwrap();
        let mut allowed_object = VmObject::new(allowed, vec![state_link]).unwrap();
        allowed_object.state_flags_by_index = vec![0, 4];
        allowed_object
            .set_register(process_register::INVINCIBILITY_STATE, 3)
            .unwrap();

        let mut machine = Machine::new(0);
        machine.insert_object(ordinary_object).unwrap();
        machine.insert_object(invincible_object).unwrap();
        machine.insert_object(allowed_object).unwrap();

        assert_eq!(
            machine.run(ordinary, 1).unwrap().reason,
            HaltReason::BudgetExhausted
        );
        assert_eq!(machine.object(ordinary).unwrap().state(), 0);
        assert_eq!(
            machine.run(invincible, 1).unwrap().reason,
            HaltReason::BudgetExhausted
        );
        assert_eq!(machine.object(invincible).unwrap().state(), 0);
        assert_eq!(
            machine.run(allowed, 1).unwrap().reason,
            HaltReason::StateChanged(1)
        );
        assert_eq!(machine.object(allowed).unwrap().state(), 1);
        assert_eq!(
            machine.effects(),
            &[VmEffect::StateChanged {
                object: allowed,
                state: 1,
            }]
        );
    }

    #[test]
    fn state_rebind_preserves_object_data_and_runs_tagged_animation_ops() {
        let h = handle(0);
        let mut object = VmObject::new(h, vec![control_flow(1, 0, 0, 0, 7)]).unwrap();
        object
            .set_register(SCALE_X_REGISTER, (-0x1000_i32) as u32)
            .unwrap();
        object.set_register(process_register::STATUS_A, 4).unwrap();
        object.bind_animation_data(&[1, 2, 3, 4]);
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        assert_eq!(
            machine.run(h, 1).unwrap().reason,
            HaltReason::StateChanged(7)
        );

        let animation_register = 0x0e2a;
        let select_animation = Instruction::encode(0x27, 0x0800, animation_register);
        let change_frame = (0x84_u32 << 24) | (1 << 22) | (3 << 16) | u32::from(REG0);
        let state = GoolState {
            flags: 0x1234,
            status_c: 0x5678,
            external_index: 0,
            event_pc: GOOL_PC_NONE,
            transition_pc: GOOL_PC_NONE,
            code_pc: 0,
        };
        let program =
            VmStateProgram::new(7, state, vec![select_animation, change_frame], vec![0xaa55])
                .unwrap();
        machine.set_frames_elapsed(9);
        machine.rebind_state_program(h, &program, &[]).unwrap();
        assert_eq!(
            machine
                .object(h)
                .unwrap()
                .register(process_register::STATUS_A),
            Ok(0x0002_0024)
        );
        assert_eq!(
            machine
                .object(h)
                .unwrap()
                .register(process_register::STATE_STAMP),
            Ok(9)
        );
        machine
            .object_mut(h)
            .unwrap()
            .set_register(0, 0x200)
            .unwrap();
        assert_eq!(
            machine.run(h, 2).unwrap(),
            Execution {
                reason: HaltReason::AnimationChanged {
                    frame: 0x200,
                    wait: 3,
                },
                steps: 2,
            }
        );

        let reference_word = machine.object(h).unwrap().register(42).unwrap();
        let reference = AnimationReference::from_word(reference_word).unwrap();
        assert_eq!(reference.offset(), 0);
        assert_eq!(
            machine.object(h).unwrap().animation_data(reference),
            Ok(&[1, 2, 3, 4][..])
        );
        assert_eq!(machine.object(h).unwrap().animation_frame(), 0x200);
        assert_eq!(
            machine.object(h).unwrap().register(SCALE_X_REGISTER),
            Ok(0x1000)
        );
        assert_eq!(
            machine.object(h).unwrap().stack(),
            &[
                INITIAL_FRAME_FLAGS,
                CODE_REFERENCE_TAG,
                (SYNTHETIC_STACK_POINTER * 4) as u32,
                (3 << 24) | 9,
            ]
        );
        machine.set_frames_elapsed(10);
        assert_eq!(
            machine.run(h, 0).unwrap(),
            Execution {
                reason: HaltReason::AnimationWaiting { remaining: 2 },
                steps: 0,
            }
        );
        machine.set_frames_elapsed(12);
        assert_eq!(
            machine.run(h, 0).unwrap(),
            Execution {
                reason: HaltReason::BudgetExhausted,
                steps: 0,
            }
        );
        assert_eq!(
            machine.object(h).unwrap().stack(),
            &[
                INITIAL_FRAME_FLAGS,
                CODE_REFERENCE_TAG,
                (SYNTHETIC_STACK_POINTER * 4) as u32,
            ]
        );
        assert_eq!(machine.object(h).unwrap().state_flags(), 0x1234);
        assert_eq!(machine.object(h).unwrap().status_c(), 0x5678);
        assert!(machine.effects().contains(&VmEffect::AnimationSelected {
            object: h,
            reference,
        }));
        assert!(
            machine
                .effects()
                .contains(&VmEffect::AnimationFrameChanged {
                    object: h,
                    frame: 0x200,
                    scale_x: 0x1000,
                    local_bound_refresh: AnimationLocalBoundRefresh::Unconditional,
                })
        );
    }

    #[test]
    fn state_rebind_captures_clears_and_synchronously_returns_from_once_pointer() {
        let h = handle(0);
        let once_pc = 0x6e_usize;
        // Exact N. Sanity Crash MOVC word at external pc 945: arm global
        // offset 0x6e in process word 36 (`once_p`).
        let mut object = VmObject::new(h, vec![0x1809_006e, control_flow(1, 0, 0, 0, 7)]).unwrap();
        object.global_code.resize(once_pc + 4, 0);
        // Exercise an ordinary nested JAL inside the once invocation. Only
        // the outer retail frame has suspend/status-preserve behavior.
        object.global_code[once_pc] = (0x86_u32 << 24) | ((once_pc + 2) as u32);
        object.global_code[once_pc + 1] = control_flow(2, 0, 0, 0, 0);
        object.global_code[once_pc + 2] = Instruction::encode(0x11, 0x0805, REG0);
        object.global_code[once_pc + 3] = control_flow(2, 0, 0, 0, 0);

        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        assert_eq!(
            machine.run(h, 2).unwrap().reason,
            HaltReason::StateChanged(7)
        );
        assert_eq!(
            machine
                .object(h)
                .unwrap()
                .register(process_register::ONCE_POINTER),
            Ok(CodeAddress {
                segment: CodeSegment::Global,
                pc: once_pc,
            }
            .to_word())
        );

        let target = VmStateProgram::new(
            7,
            GoolState {
                flags: 0x20,
                status_c: 0x40,
                external_index: 0,
                event_pc: GOOL_PC_NONE,
                transition_pc: GOOL_PC_NONE,
                code_pc: 0,
            },
            vec![control_flow(2, 0, 0, 0, 0)],
            Vec::new(),
        )
        .unwrap();
        machine.set_frames_elapsed(9);
        machine.rebind_state_program(h, &target, &[]).unwrap();

        let rebound = machine.object(h).unwrap();
        assert_eq!(rebound.register(process_register::ONCE_POINTER), Ok(0));
        // Retail writes state_stamp only after the once interpreter returns.
        assert_eq!(rebound.register(process_register::STATE_STAMP), Ok(0));
        assert_eq!(rebound.stack().len(), STATE_FRAME_WORDS);
        assert_eq!(
            rebound.stack(),
            &[
                INITIAL_FRAME_FLAGS,
                CodeAddress {
                    segment: CodeSegment::External,
                    pc: 0,
                }
                .to_word(),
                (SYNTHETIC_STACK_POINTER * 4) as u32,
            ]
        );

        assert_eq!(
            machine
                .run_pending_once_with_host_effects(h, |_machine, _effect| Ok(()))
                .unwrap(),
            Some(Execution {
                reason: HaltReason::OnceCompleted,
                steps: 4,
            })
        );
        let completed = machine.object(h).unwrap();
        assert_eq!(completed.register(0), Ok(0x500));
        assert_eq!(completed.register(process_register::STATE_STAMP), Ok(9));
        assert_eq!(completed.frame_base, SYNTHETIC_STACK_POINTER);
        assert!(completed.call_stack.is_empty());
        assert_eq!(
            completed.stack(),
            &[
                INITIAL_FRAME_FLAGS,
                CodeAddress {
                    segment: CodeSegment::External,
                    pc: 0,
                }
                .to_word(),
                (SYNTHETIC_STACK_POINTER * 4) as u32,
                0,
            ]
        );
        assert_eq!(
            machine
                .run_pending_once_with_host_effects(h, |_machine, _effect| Ok(()))
                .unwrap(),
            None
        );
    }

    #[test]
    fn once_pointer_requires_a_checked_global_code_tag_before_rebind_mutates_state() {
        let h = handle(0);
        let mut object = VmObject::new(h, vec![control_flow(1, 0, 0, 0, 1)]).unwrap();
        object.global_code = vec![control_flow(2, 0, 0, 0, 0)];
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        assert_eq!(
            machine.run(h, 1).unwrap().reason,
            HaltReason::StateChanged(1)
        );
        let invalid = CODE_REFERENCE_TAG | 1;
        machine
            .object_mut(h)
            .unwrap()
            .set_register(process_register::ONCE_POINTER, invalid)
            .unwrap();
        let target = VmStateProgram::new(
            1,
            GoolState {
                flags: 0,
                status_c: 0,
                external_index: 0,
                event_pc: GOOL_PC_NONE,
                transition_pc: GOOL_PC_NONE,
                code_pc: 0,
            },
            vec![control_flow(2, 0, 0, 0, 0)],
            Vec::new(),
        )
        .unwrap();
        assert_eq!(
            machine.rebind_state_program(h, &target, &[]),
            Err(VmError::InvalidCodeReference(invalid))
        );
        assert_eq!(
            machine
                .object(h)
                .unwrap()
                .register(process_register::ONCE_POINTER),
            Ok(invalid)
        );
        assert_eq!(CodeAddress::from_word(invalid), None);
    }

    #[test]
    fn once_mode_records_animation_but_does_not_suspend_before_return_link() {
        let h = handle(0);
        let mut object = VmObject::new(h, vec![0x1809_0000, control_flow(1, 0, 0, 0, 1)]).unwrap();
        object.bind_animation_data(&[0; 4]);
        object.global_code = vec![
            (0x83_u32 << 24) | (1 << 22) | (1 << 16),
            control_flow(2, 0, 0, 0, 0),
        ];
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        assert_eq!(
            machine.run(h, 2).unwrap().reason,
            HaltReason::StateChanged(1)
        );
        let target = VmStateProgram::new(
            1,
            GoolState {
                flags: 0,
                status_c: 0,
                external_index: 0,
                event_pc: GOOL_PC_NONE,
                transition_pc: GOOL_PC_NONE,
                code_pc: 0,
            },
            vec![control_flow(2, 0, 0, 0, 0)],
            Vec::new(),
        )
        .unwrap();
        machine.rebind_state_program(h, &target, &[]).unwrap();
        assert_eq!(
            machine
                .run_pending_once_with_host_effects(h, |_machine, _effect| Ok(()))
                .unwrap(),
            Some(Execution {
                reason: HaltReason::OnceCompleted,
                steps: 2,
            })
        );
        assert!(machine.effects().iter().any(|effect| matches!(
            effect,
            VmEffect::AnimationFrameChanged { object, frame: 0, .. } if *object == h
        )));
        assert_eq!(machine.object(h).unwrap().stack().last(), Some(&0));
    }

    #[test]
    fn transition_block_observes_state_stamp_and_returns_to_state_code() {
        let h = handle(0);
        let mut machine = Machine::new(0);
        let mut object = VmObject::new(h, vec![control_flow(1, 0, 0, 0, 1)]).unwrap();
        object.global_code = vec![
            Instruction::encode(0x11, 0x0801, REG1),
            control_flow(2, 0, 0, 0, 0),
        ];
        machine.insert_object(object).unwrap();
        assert_eq!(
            machine.run(h, 1).unwrap().reason,
            HaltReason::StateChanged(1)
        );

        let state_code_pc = 3;
        let target = VmStateProgram::new(
            1,
            GoolState {
                flags: 0,
                status_c: 0,
                external_index: 0,
                event_pc: GOOL_PC_NONE,
                transition_pc: 0,
                code_pc: state_code_pc as u16,
            },
            vec![
                0x8600_0000,
                Instruction::encode(0x11, REG0 + process_register::STATE_STAMP as u16, REG0),
                control_flow(2, 0, 0, 0, 0),
                control_flow(2, 0, 0, 0, 0),
            ],
            Vec::new(),
        )
        .unwrap();
        machine.set_frames_elapsed(9);
        machine.rebind_state_program(h, &target, &[]).unwrap();
        let state_stack = machine.object(h).unwrap().stack().to_vec();

        assert_eq!(
            machine
                .run_transition_with_host_effects(h, |_machine, _effect| Ok(()))
                .unwrap(),
            Some(Execution {
                reason: HaltReason::TransitionCompleted,
                steps: 5,
            })
        );
        let object = machine.object(h).unwrap();
        assert_eq!(object.register(0), Ok(9));
        assert_eq!(object.register(1), Ok(0x100));
        assert_eq!(object.register(process_register::STATE_STAMP), Ok(9));
        assert_eq!(object.code_address().segment, CodeSegment::External);
        assert_eq!(object.pc(), state_code_pc);
        assert_eq!(object.frame_base, SYNTHETIC_STACK_POINTER);
        assert!(object.call_stack.is_empty());
        assert_eq!(object.stack(), state_stack);
    }

    #[test]
    fn transition_pointer_can_capture_the_post_fetch_program_counter() {
        let h = handle(0);
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(h, vec![control_flow(2, 0, 0, 0, 0)]).unwrap())
            .unwrap();

        let state_code_pc = 4;
        let target = VmStateProgram::new(
            0,
            GoolState {
                flags: 0,
                status_c: 0,
                external_index: 0,
                event_pc: GOOL_PC_NONE,
                transition_pc: 0,
                code_pc: state_code_pc as u16,
            },
            vec![
                Instruction::encode(0x11, 0x0801, REG0 + 8),
                // MOVE object[pc] -> object[tp], as used by PinOC state zero.
                0x11e2_0e22,
                Instruction::encode(0x11, 0x0802, REG0 + 9),
                control_flow(2, 0, 0, 0, 0),
                control_flow(2, 0, 0, 0, 0),
            ],
            Vec::new(),
        )
        .unwrap();
        machine.rebind_state_program(h, &target, &[]).unwrap();

        assert_eq!(
            machine
                .run_transition_with_host_effects(h, |_machine, _effect| Ok(()))
                .unwrap(),
            Some(Execution {
                reason: HaltReason::TransitionCompleted,
                steps: 4,
            })
        );
        let captured = CodeAddress {
            segment: CodeSegment::External,
            pc: 2,
        };
        let object = machine.object(h).unwrap();
        assert_eq!(object.transition_pc(), Some(2));
        assert_eq!(
            object.register(process_register::TRANSITION_POINTER),
            Ok(captured.to_word())
        );
        assert_eq!(object.register(8), Ok(0x100));
        assert_eq!(object.register(9), Ok(0x200));
        assert_eq!(object.pc(), state_code_pc);

        machine
            .object_mut(h)
            .unwrap()
            .set_register(8, 0xdead_beef)
            .unwrap();
        assert_eq!(
            machine
                .run_transition_with_host_effects(h, |_machine, _effect| Ok(()))
                .unwrap(),
            Some(Execution {
                reason: HaltReason::TransitionCompleted,
                steps: 2,
            })
        );
        let object = machine.object(h).unwrap();
        assert_eq!(object.register(8), Ok(0xdead_beef));
        assert_eq!(object.register(9), Ok(0x200));
        assert_eq!(object.transition_pc(), Some(2));
        assert_eq!(object.pc(), state_code_pc);
    }

    #[test]
    fn transition_animation_and_spawn_effect_do_not_suspend_or_replace_state_wait() {
        let h = handle(0);
        let mut object = VmObject::new(h, vec![control_flow(1, 0, 0, 0, 1)]).unwrap();
        object.bind_animation_data(&[0; 4]);
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        assert_eq!(
            machine.run(h, 1).unwrap().reason,
            HaltReason::StateChanged(1)
        );

        let state_code_pc = 4;
        let target = VmStateProgram::new(
            1,
            GoolState {
                flags: 0,
                status_c: 0,
                external_index: 0,
                event_pc: GOOL_PC_NONE,
                transition_pc: 0,
                code_pc: state_code_pc as u16,
            },
            vec![
                (0x83_u32 << 24) | (1 << 22) | (5 << 16),
                Instruction::encode(0x11, 0x0801, STACK),
                0x8a10_5001,
                control_flow(2, 0, 0, 0, 0),
                control_flow(2, 0, 0, 0, 0),
            ],
            Vec::new(),
        )
        .unwrap();
        machine.set_frames_elapsed(12);
        machine.rebind_state_program(h, &target, &[]).unwrap();
        let state_stack = machine.object(h).unwrap().stack().to_vec();
        let mut callback_count = 0;

        assert_eq!(
            machine
                .run_transition_with_host_effects(h, |_machine, effect| {
                    assert!(matches!(effect, VmEffect::SpawnChildren { .. }));
                    callback_count += 1;
                    Ok(())
                })
                .unwrap(),
            Some(Execution {
                reason: HaltReason::TransitionCompleted,
                steps: 4,
            })
        );
        assert_eq!(callback_count, 1);
        let object = machine.object(h).unwrap();
        assert_eq!(object.pc(), state_code_pc);
        assert_eq!(object.stack(), state_stack);
        assert_eq!(
            object.animation_wait,
            Some(AnimationWait {
                stamp: 0,
                frames: 0,
            })
        );
        assert!(machine.effects().iter().any(|effect| matches!(
            effect,
            VmEffect::AnimationFrameChanged { object, frame: 0, .. } if *object == h
        )));
        assert!(machine.effects().iter().any(|effect| matches!(
            effect,
            VmEffect::SpawnChildren {
                parent,
                executable: 5,
                arguments,
                ..
            } if *parent == h && arguments == &[0x100]
        )));
    }

    #[test]
    fn packed_animation_change_uses_checked_word_offset_frame_wait_and_flip() {
        let h = handle(0);
        let instruction = (0x83_u32 << 24) | (2 << 22) | (3 << 16) | (2 << 7) | 5;
        let mut object = VmObject::new(h, vec![instruction]).unwrap();
        object.bind_animation_data(&[0; 16]);
        object
            .set_register(SCALE_X_REGISTER, (-0x1000_i32) as u32)
            .unwrap();
        let mut machine = Machine::new(0);
        machine.set_frames_elapsed(9);
        machine.insert_object(object).unwrap();

        assert_eq!(
            machine.run(h, 1),
            Ok(Execution {
                reason: HaltReason::AnimationChanged {
                    frame: 5 << 8,
                    wait: 3,
                },
                steps: 1,
            })
        );
        let object = machine.object(h).unwrap();
        let reference_word = object
            .register(process_register::ANIMATION_SEQUENCE)
            .unwrap();
        let reference = AnimationReference::from_word(reference_word).unwrap();
        assert_eq!(reference.offset(), 8);
        assert_eq!(object.animation_frame(), 5 << 8);
        assert_eq!(object.register(SCALE_X_REGISTER), Ok(0x1000));
        assert_eq!(object.stack(), &[(3 << 24) | 9]);
        assert_eq!(
            machine.effects(),
            &[
                VmEffect::AnimationSelected {
                    object: h,
                    reference,
                },
                VmEffect::AnimationFrameChanged {
                    object: h,
                    frame: 5 << 8,
                    scale_x: 0x1000,
                    local_bound_refresh: AnimationLocalBoundRefresh::Conditional,
                },
            ]
        );
    }

    #[test]
    fn dormant_solid_directional_selectors_copy_statics_without_a_parent() {
        let translation = [12_345, 67_890, -12_000];
        let output = 5_u32;
        let input = 3_u32;
        let output_words = 0x0e00 | process_register::ACK as u32;
        let environment = RetailSolidEnvironment::new(0, [0; 24], [0; 24], Vec::new());

        for suboperation in [2_u32, 4] {
            let h = handle(suboperation as u16);
            let instruction = (0x8e_u32 << 24)
                | (suboperation << 18)
                | (output << 15)
                | (input << 12)
                | output_words;
            let mut object = VmObject::new(h, vec![instruction]).unwrap();
            object
                .set_register(process_register::STATUS_B, 0x0400_0000)
                .unwrap();
            object.set_process_vector(0, translation).unwrap();
            object.set_process_vector(3, [1, 2, 3]).unwrap();
            object.set_process_vector(5, [-1, -2, -3]).unwrap();
            object.set_register(process_register::ACK, 11).unwrap();
            object
                .set_register(process_register::ANIMATION_STAMP, 12)
                .unwrap();
            object
                .set_register(process_register::STATE_STAMP, 13)
                .unwrap();
            object.bind_retail_solid_environment(environment.clone());
            let mut machine = Machine::new(0);
            machine.insert_object(object).unwrap();

            machine.run(h, 1).unwrap();
            let object = machine.object(h).unwrap();
            assert_eq!(object.process_vector(5), Ok(translation));
            assert_eq!(
                [
                    object.register(process_register::ACK).unwrap(),
                    object.register(process_register::ANIMATION_STAMP).unwrap(),
                    object.register(process_register::STATE_STAMP).unwrap(),
                ],
                [0; 3],
                "selector {suboperation} must write static trans4 before trans3"
            );
        }
    }

    #[test]
    fn solid_suboperation_five_copies_parent_colors_without_seeking() {
        let child = handle(0);
        let parent = handle(1);
        let instruction = (0x8e_u32 << 24) | (5 << 18) | (5 << 15) | 0x0be0;
        // Odd leaf, type zero, subtype 48 (two-percent color scale).
        let zone = RetailSolidZone::new(
            [0; 3],
            [1_000; 3],
            0x0301,
            [0; 3],
            vec![0; RETAIL_SOLID_RECT_BYTES],
        )
        .unwrap();
        let environment = RetailSolidEnvironment::new(0, [2_000; 24], [1_000; 24], vec![zone]);
        let translation = [25_600; 3];

        let mut parent_object = VmObject::new(parent, vec![0]).unwrap();
        parent_object
            .set_register(process_register::STATUS_B, 0x0400_0000)
            .unwrap();
        parent_object.set_retail_colors([0; COLOR_COUNT]);
        parent_object.bind_retail_solid_environment(environment.clone());
        let mut child_object = VmObject::new(child, vec![instruction]).unwrap();
        child_object
            .set_register(process_register::STATUS_B, 0x0400_0000)
            .unwrap();
        child_object.set_process_vector(0, translation).unwrap();
        child_object.set_process_vector(5, [-1; 3]).unwrap();
        child_object.set_link(1, Some(parent)).unwrap();
        child_object.bind_retail_solid_environment(environment);

        let mut machine = Machine::new(0);
        machine.insert_object(parent_object).unwrap();
        machine.insert_object(child_object).unwrap();
        machine.run(child, 1).unwrap();

        assert_eq!(
            machine.object(child).unwrap().process_vector(5),
            Ok(translation)
        );
        let colors = machine.object(parent).unwrap().retail_colors();
        assert_eq!(colors[..12], [39; 12]);
        assert_eq!(colors[12..], [2_000; 12]);
    }

    #[test]
    fn solid_suboperation_seven_only_translates_its_stack_operand() {
        let h = handle(0);
        let instruction = (0x8e_u32 << 24) | (7 << 18) | u32::from(STACK);
        let object = VmObject::new(h, vec![instruction]).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.push(h, 0x1122_3344).unwrap();

        machine.run(h, 1).unwrap();
        assert_eq!(machine.object(h).unwrap().stack(), &[0x1122_3344]);
    }

    #[test]
    fn solid_suboperation_zero_rebounds_and_clamps_against_a_leaf() {
        let h = handle(0);
        let vector_three = 0x0e00 | process_register::MISC_A_X as u16;
        let instruction = (0x8e_u32 << 24) | (5 << 15) | (3 << 12) | u32::from(vector_three);
        let zone = RetailSolidZone::new(
            [0; 3],
            [100; 3],
            0x0301,
            [0; 3],
            vec![0; RETAIL_SOLID_RECT_BYTES],
        )
        .unwrap();
        let environment = RetailSolidEnvironment::new(0, [0; 24], [0; 24], vec![zone]);
        let mut object = VmObject::new(h, vec![instruction]).unwrap();
        object.set_process_vector(0, [12_800; 3]).unwrap();
        object.set_process_vector(3, [100, 0, 0]).unwrap();
        object.set_process_vector(5, [-1; 3]).unwrap();
        object.bind_retail_solid_environment(environment);
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        machine.run(h, 1).unwrap();
        let object = machine.object(h).unwrap();
        assert_eq!(object.register(process_register::MISC_VALUE), Ok(0x0301));
        assert_eq!(object.process_vector(3), Ok([-100, 0, 0]));
        assert_eq!(object.process_vector(5), Ok([0, 12_800, 12_800]));
    }

    #[test]
    fn current_zone_solid_queries_do_not_replace_per_object_zone_colors() {
        let object_zones = [
            Eid::from_name("oa_9Z").unwrap(),
            Eid::from_name("ob_9Z").unwrap(),
        ];
        let current_zone = Eid::from_name("cq_9Z").unwrap();
        let query_zone = RetailSolidZone::new(
            [0; 3],
            [100; 3],
            0x0301,
            [0; 3],
            vec![0; RETAIL_SOLID_RECT_BYTES],
        )
        .unwrap()
        .with_eid(current_zone);
        let current_environment =
            RetailSolidEnvironment::new(0, [0x999; 24], [0xaaa; 24], vec![query_zone])
                .with_runtime_context(Some(current_zone), SolidLevelQuirks::default());
        let vector_three = 0x0e00 | process_register::MISC_A_X as u16;
        let instruction = (0x8e_u32 << 24) | (5 << 15) | (3 << 12) | u32::from(vector_three);
        let mut machine = Machine::new(0);

        for (index, zone) in object_zones.into_iter().enumerate() {
            let handle = handle(index as u16);
            let intensity = u16::try_from(0x110 + index).unwrap();
            let mut colors = retail_color_environment([0x220 + intensity; 3], [intensity; 3])
                .with_runtime_context(Some(zone), SolidLevelQuirks::default());
            colors.graphics_flags = 0xdead_beef;
            let mut object = VmObject::new(handle, vec![instruction]).unwrap();
            object.set_main_player_identity(true);
            object.set_retail_colors([0x777; COLOR_COUNT]);
            object
                .set_register(process_register::STATUS_B, STATUS_B_MAIN_COLOR_BY_ZONE)
                .unwrap();
            object
                .set_register(process_register::INVINCIBILITY_STATE, 1)
                .unwrap();
            object.set_process_vector(0, [12_800; 3]).unwrap();
            object.set_process_vector(3, [100, 0, 0]).unwrap();
            object.bind_retail_solid_environment(colors);
            machine.insert_object(object).unwrap();
        }
        machine.set_current_retail_solid_environment(Some(current_environment));

        for (index, _) in object_zones.into_iter().enumerate() {
            let handle = handle(index as u16);
            machine.run(handle, 1).unwrap();
            machine.run_retail_object_colors(handle).unwrap();
            let object = machine.object(handle).unwrap();
            assert_eq!(
                object.register(process_register::MISC_VALUE),
                Ok(0x0301),
                "both objects must query global cur_zone geometry"
            );
            let intensity = u16::try_from(0x110 + index).unwrap();
            assert_eq!(
                &object.retail_colors()[COLOR_INTENSITY_START..COLOR_INTENSITY_END],
                &[intensity; 3],
                "each object must retain its own obj->zone color header"
            );
            assert_eq!(object.retail_solid_zone_eid(), Some(object_zones[index]));
        }
    }

    #[test]
    fn smooth_stop_latch_is_shared_across_interleaved_objects() {
        let zone_eid = Eid::from_name("sm_9Z").unwrap();
        let mut bytes = vec![0_u8; 44];
        for (index, child) in [0x0003_u16, 0, 0x0003, 0x0001].into_iter().enumerate() {
            let offset = RETAIL_SOLID_RECT_BYTES + index * 2;
            bytes[offset..offset + 2].copy_from_slice(&child.to_le_bytes());
        }
        let zone = RetailSolidZone::new(
            [-100; 3],
            [200; 3],
            RETAIL_SOLID_RECT_BYTES as u16,
            [1, 1, 0],
            bytes,
        )
        .unwrap()
        .with_eid(zone_eid);
        let environment = RetailSolidEnvironment::new(0, [0; 24], [0; 24], vec![zone])
            .with_runtime_context(Some(zone_eid), SolidLevelQuirks::default());
        let first = handle(0);
        let second = handle(1);
        let mut machine = Machine::new(0);
        for object_handle in [first, second] {
            let mut object = VmObject::new(object_handle, Vec::new()).unwrap();
            object.bind_retail_solid_environment(environment.clone());
            object
                .set_register(
                    process_register::STATUS_B,
                    crate::retail_physics::STATUS_B_TRANSLATION_MOTION
                        | crate::retail_physics::STATUS_B_STOPPED_BY_SOLID,
                )
                .unwrap();
            object
                .set_register(process_register::TRANSLATION_X, (-3_856_i32) as u32)
                .unwrap();
            object
                .set_register(process_register::TRANSLATION_Y, 1)
                .unwrap();
            object
                .set_register(process_register::MISC_A_X, 602_353)
                .unwrap();
            machine.insert_object(object).unwrap();
        }

        machine.run_retail_object_physics(first).unwrap();
        assert!(
            machine.solid_smooth_stop.being_stopped,
            "the first blocked mover must arm native being_stopped: smooth={:?}, translation={:?}",
            machine.solid_smooth_stop,
            machine.object(first).unwrap().process_vector(0)
        );
        assert_ne!(machine.solid_smooth_stop.previous_displacement, Vec3::ZERO);

        machine.run_retail_object_physics(second).unwrap();
        assert_eq!(
            machine.solid_smooth_stop,
            SmoothStopMemory::default(),
            "the second mover consumes the process-global latch instead of using a private copy"
        );
    }

    #[test]
    fn stopped_by_solid_without_event_preserves_pre_solver_control_state() {
        let zone_eid = Eid::from_name("np_9Z").unwrap();
        let zone = RetailSolidZone::new(
            [-100_000; 3],
            [200_000; 3],
            0,
            [0; 3],
            vec![0; RETAIL_SOLID_RECT_BYTES],
        )
        .unwrap()
        .with_eid(zone_eid);
        let environment = RetailSolidEnvironment::new(0, [0; 24], [0; 24], vec![zone])
            .with_runtime_context(Some(zone_eid), SolidLevelQuirks::default());
        let object_handle = handle(0);
        let mut object = VmObject::new(object_handle, Vec::new()).unwrap();
        object.bind_retail_solid_environment(environment);
        object
            .set_register(
                process_register::STATUS_B,
                STATUS_B_DPAD_CONTROL
                    | crate::retail_physics::STATUS_B_TRANSLATION_MOTION
                    | crate::retail_physics::STATUS_B_STOPPED_BY_SOLID,
            )
            .unwrap();
        object
            .set_register(
                process_register::STATUS_A,
                crate::retail_physics::STATUS_A_MOVEMENT_ACTIVE,
            )
            .unwrap();
        object
            .set_register(
                process_register::STATE_FLAGS,
                crate::retail_physics::STATE_FLAG_GROUND,
            )
            .unwrap();
        let mut expected = object.retail_physics_state().unwrap();
        begin_retail_physics(
            &mut expected,
            RetailPhysicsContext {
                ticks_per_frame: 34,
                game_state_playing: true,
                pad_held: 4 << 12,
                ..RetailPhysicsContext::default()
            },
        );
        assert!(expected.speed > 0);

        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.set_retail_physics_frame_context(true, 0);
        machine
            .set_pad_snapshot(
                0,
                RetailPadSnapshot {
                    held: 4 << 12,
                    ..RetailPadSnapshot::default()
                },
            )
            .unwrap();
        let mut event_count = 0;
        machine
            .run_retail_object_physics_with_solid_event_handler(object_handle, |_, _, _, _| {
                event_count += 1;
                true
            })
            .unwrap();

        assert_eq!(event_count, 0);
        let actual = machine.object(object_handle).unwrap();
        assert_eq!(
            actual.register(process_register::SPEED),
            Ok(expected.speed as u32)
        );
        assert_eq!(
            actual.register(process_register::MISC_B_X),
            Ok(expected.target_rotation.x as u32)
        );
    }

    #[test]
    fn inline_solid_handler_observes_complete_pre_solver_control_state() {
        let zone_eid = Eid::from_name("hp_9Z").unwrap();
        let zone = RetailSolidZone::new(
            [0; 3],
            [100; 3],
            0,
            [0; 3],
            vec![0; RETAIL_SOLID_RECT_BYTES],
        )
        .unwrap()
        .with_eid(zone_eid)
        .with_graphics(2, i32::MIN);
        let environment = RetailSolidEnvironment::new(0, [0; 24], [0; 24], vec![zone])
            .with_runtime_context(Some(zone_eid), SolidLevelQuirks::default());
        let object_handle = handle(0);
        let mut object = VmObject::new(object_handle, Vec::new()).unwrap();
        object.bind_retail_solid_environment(environment);
        object
            .set_register(
                process_register::STATUS_B,
                STATUS_B_DPAD_CONTROL
                    | crate::retail_physics::STATUS_B_TRANSLATION_MOTION
                    | crate::retail_physics::STATUS_B_STOPPED_BY_SOLID,
            )
            .unwrap();
        object
            .set_register(
                process_register::STATUS_A,
                crate::retail_physics::STATUS_A_MOVEMENT_ACTIVE,
            )
            .unwrap();
        object
            .set_register(
                process_register::STATE_FLAGS,
                crate::retail_physics::STATE_FLAG_GROUND,
            )
            .unwrap();
        object
            .set_register(process_register::TRANSLATION_Y, 100)
            .unwrap();
        object
            .set_register(process_register::MISC_A_Y, (-4_000_i32) as u32)
            .unwrap();
        let mut expected = object.retail_physics_state().unwrap();
        begin_retail_physics(
            &mut expected,
            RetailPhysicsContext {
                ticks_per_frame: 34,
                game_state_playing: true,
                pad_held: 4 << 12,
                ..RetailPhysicsContext::default()
            },
        );

        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.set_retail_physics_frame_context(true, 0);
        machine
            .set_pad_snapshot(
                0,
                RetailPadSnapshot {
                    held: 4 << 12,
                    ..RetailPadSnapshot::default()
                },
            )
            .unwrap();
        let mut observed = None;
        machine
            .run_retail_object_physics_with_solid_event_handler(
                object_handle,
                |machine, moving, _, effect| {
                    if matches!(effect, SolidEffect::SendEvent { .. }) {
                        let object = machine.object(moving).unwrap();
                        observed = Some((
                            object.register(process_register::SPEED).unwrap(),
                            object.register(process_register::MISC_B_X).unwrap(),
                        ));
                        return false;
                    }
                    true
                },
            )
            .unwrap();

        assert_eq!(
            observed,
            Some((expected.speed as u32, expected.target_rotation.x as u32))
        );
    }

    #[test]
    fn solid_query_cache_spans_objects_frames_and_current_zone_replacement() {
        fn environment(eid: Eid, raw_node: u16) -> RetailSolidEnvironment {
            let zone = RetailSolidZone::new(
                [-2_000; 3],
                [4_000; 3],
                raw_node,
                [0; 3],
                vec![0; RETAIL_SOLID_RECT_BYTES],
            )
            .unwrap()
            .with_eid(eid);
            RetailSolidEnvironment::new(0, [0; 24], [0; 24], vec![zone])
                .with_runtime_context(Some(eid), SolidLevelQuirks::default())
        }

        fn object(
            handle: ObjectHandle,
            environment: RetailSolidEnvironment,
            translation_x: i32,
        ) -> VmObject {
            let mut object = VmObject::new(handle, Vec::new()).unwrap();
            object.bind_retail_solid_environment(environment);
            object
                .set_register(
                    process_register::STATUS_B,
                    crate::retail_physics::STATUS_B_TRANSLATION_MOTION
                        | crate::retail_physics::STATUS_B_STOPPED_BY_SOLID,
                )
                .unwrap();
            object
                .set_register(process_register::TRANSLATION_X, translation_x as u32)
                .unwrap();
            object.set_register(process_register::MISC_A_X, 31).unwrap();
            object
        }

        let first_zone = Eid::from_name("qa_9Z").unwrap();
        let second_zone = Eid::from_name("qb_9Z").unwrap();
        let first_environment = environment(first_zone, 0x0013);
        let second_environment = environment(second_zone, 0x0023);
        let first = handle(0);
        let second = handle(1);
        let mut machine = Machine::new(0);
        machine
            .insert_object(object(first, first_environment.clone(), 0))
            .unwrap();
        machine
            .insert_object(object(second, first_environment, 100))
            .unwrap();

        machine.run_retail_object_physics(first).unwrap();
        let initial = machine.solid_query_cache.clone().unwrap();
        assert_eq!(initial.nodes()[0].raw_node, 0x0013);

        machine.set_frames_elapsed(1);
        machine.set_current_retail_solid_environment(Some(second_environment));
        assert_eq!(machine.solid_query_cache, Some(initial.clone()));
        machine.run_retail_object_physics(second).unwrap();
        assert_eq!(
            machine.solid_query_cache,
            Some(initial.clone()),
            "a nearby object in the next frame keeps native cur_zone_query even after cur_zone changes"
        );

        {
            let object = machine.object_mut(second).unwrap();
            object
                .set_register(process_register::TRANSLATION_X, 200_000)
                .unwrap();
            object
                .set_register(
                    process_register::STATUS_B,
                    crate::retail_physics::STATUS_B_TRANSLATION_MOTION
                        | crate::retail_physics::STATUS_B_STOPPED_BY_SOLID,
                )
                .unwrap();
            object.set_register(process_register::MISC_A_X, 31).unwrap();
        }
        machine.run_retail_object_physics(second).unwrap();
        let rebuilt = machine.solid_query_cache.as_ref().unwrap();
        assert_ne!(rebuilt.nodes_bound, initial.nodes_bound);
        assert_eq!(rebuilt.nodes()[0].raw_node, 0x0023);

        machine.reset_retail_solid_smoothing();
        assert!(machine.solid_query_cache.is_none());
    }

    #[test]
    fn interrupted_solid_event_commits_process_global_smoothing() {
        let zone_eid = Eid::from_name("si_9Z").unwrap();
        let zone = RetailSolidZone::new(
            [0; 3],
            [100; 3],
            0,
            [0; 3],
            vec![0; RETAIL_SOLID_RECT_BYTES],
        )
        .unwrap()
        .with_eid(zone_eid)
        .with_graphics(2, i32::MIN);
        let environment = RetailSolidEnvironment::new(0, [0; 24], [0; 24], vec![zone])
            .with_runtime_context(Some(zone_eid), SolidLevelQuirks::default());
        let object_handle = handle(0);
        let mut object = VmObject::new(object_handle, Vec::new()).unwrap();
        object.bind_retail_solid_environment(environment.clone());
        object
            .set_register(
                process_register::STATUS_B,
                crate::retail_physics::STATUS_B_TRANSLATION_MOTION
                    | crate::retail_physics::STATUS_B_STOPPED_BY_SOLID,
            )
            .unwrap();
        object
            .set_register(process_register::TRANSLATION_Y, 100)
            .unwrap();
        object
            .set_register(process_register::MISC_A_Y, (-4_000_i32) as u32)
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.solid_smooth_stop = SmoothStopMemory {
            being_stopped: true,
            previous_displacement: Vec3::ZERO,
        };

        machine
            .run_retail_object_physics_with_solid_event_handler(object_handle, |_, _, _, effect| {
                !matches!(effect, SolidEffect::SendEvent { .. })
            })
            .unwrap();

        assert_eq!(
            machine.solid_smooth_stop,
            SmoothStopMemory::default(),
            "native updates its process-global latch after the pull loop"
        );
        let interrupted_cache = machine.solid_query_cache.clone().unwrap();

        let next_handle = handle(1);
        let mut next = VmObject::new(next_handle, Vec::new()).unwrap();
        next.bind_retail_solid_environment(environment);
        next.set_register(
            process_register::STATUS_B,
            crate::retail_physics::STATUS_B_TRANSLATION_MOTION
                | crate::retail_physics::STATUS_B_STOPPED_BY_SOLID,
        )
        .unwrap();
        next.set_register(process_register::TRANSLATION_Y, 100)
            .unwrap();
        next.set_register(process_register::MISC_A_Y, (-4_000_i32) as u32)
            .unwrap();
        machine.insert_object(next).unwrap();
        machine.run_retail_object_physics(next_handle).unwrap();
        assert_eq!(
            machine.solid_query_cache,
            Some(interrupted_cache),
            "a callback-false early return restores cur_zone_query for the next mover"
        );
    }

    #[test]
    fn solid_event_hook_error_restores_process_global_query_cache() {
        let zone_eid = Eid::from_name("qe_9Z").unwrap();
        let zone = RetailSolidZone::new(
            [0; 3],
            [100; 3],
            0,
            [0; 3],
            vec![0; RETAIL_SOLID_RECT_BYTES],
        )
        .unwrap()
        .with_eid(zone_eid)
        .with_graphics(2, i32::MIN);
        let environment = RetailSolidEnvironment::new(0, [0; 24], [0; 24], vec![zone])
            .with_runtime_context(Some(zone_eid), SolidLevelQuirks::default());
        let make_object = |object_handle| {
            let mut object = VmObject::new(object_handle, Vec::new()).unwrap();
            object.bind_retail_solid_environment(environment.clone());
            object
                .set_register(
                    process_register::STATUS_B,
                    crate::retail_physics::STATUS_B_TRANSLATION_MOTION
                        | crate::retail_physics::STATUS_B_STOPPED_BY_SOLID,
                )
                .unwrap();
            object
                .set_register(process_register::TRANSLATION_Y, 100)
                .unwrap();
            object
                .set_register(process_register::MISC_A_Y, (-4_000_i32) as u32)
                .unwrap();
            object
        };

        let seed = handle(0);
        let failing = handle(1);
        let mut machine = Machine::new(0);
        machine.insert_object(make_object(seed)).unwrap();
        machine.run_retail_object_physics(seed).unwrap();
        let cached = machine.solid_query_cache.clone().unwrap();
        machine.insert_object(make_object(failing)).unwrap();

        let result = machine.run_retail_object_physics_with_solid_event_handler(
            failing,
            |machine, moving, _, effect| {
                if matches!(effect, SolidEffect::SendEvent { .. }) {
                    machine.remove_object_for_host_termination(moving).unwrap();
                }
                true
            },
        );

        assert_eq!(result, Err(VmError::UnknownObject(failing)));
        assert_eq!(
            machine.solid_query_cache,
            Some(cached),
            "an error while refreshing callback-mutated live state must not strand cur_zone_query"
        );
    }

    #[test]
    fn detached_object_zone_fallback_does_not_alias_current_neighbor_slot() {
        let object_zone = Eid::from_name("oz_9Z").unwrap();
        let current_zone = Eid::from_name("cz_9Z").unwrap();
        let object_environment = RetailSolidEnvironment::new(
            0,
            [0; 24],
            [0; 24],
            vec![
                RetailSolidZone::new(
                    [0; 3],
                    [100; 3],
                    0,
                    [0; 3],
                    vec![0; RETAIL_SOLID_RECT_BYTES],
                )
                .unwrap()
                .with_eid(object_zone),
            ],
        )
        .with_runtime_context(Some(object_zone), SolidLevelQuirks::default());
        let current_environment = RetailSolidEnvironment::new(
            0,
            [0; 24],
            [0; 24],
            vec![
                RetailSolidZone::new(
                    [1_000; 3],
                    [100; 3],
                    0,
                    [0; 3],
                    vec![0; RETAIL_SOLID_RECT_BYTES],
                )
                .unwrap()
                .with_eid(current_zone),
            ],
        )
        .with_runtime_context(Some(current_zone), SolidLevelQuirks::default());
        let h = handle(0);
        let mut object = VmObject::new(h, Vec::new()).unwrap();
        object.bind_retail_solid_environment(object_environment);
        object
            .set_register(
                process_register::STATUS_B,
                crate::retail_physics::STATUS_B_TRANSLATION_MOTION
                    | crate::retail_physics::STATUS_B_STOPPED_BY_SOLID,
            )
            .unwrap();
        object
            .set_register(process_register::TRANSLATION_Y, 100)
            .unwrap();
        object
            .set_register(process_register::MISC_A_Y, (-200_000_i32) as u32)
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.set_current_retail_solid_environment(Some(current_environment));

        machine.run_retail_object_physics(h).unwrap();
        let object = machine.object(h).unwrap();
        assert_eq!(
            object.register(process_register::TRANSLATION_Y),
            Ok(0),
            "the detached object's own bottom must clamp its motion"
        );
        assert_eq!(object.retail_solid_zone_eid(), Some(object_zone));
    }

    #[test]
    fn detached_object_zone_requires_matching_bound_geometry() {
        let object_zone = Eid::from_name("oz_9Z").unwrap();
        let current_zone = Eid::from_name("cz_9Z").unwrap();
        let object_environment = RetailSolidEnvironment::new(0, [0; 24], [0; 24], Vec::new())
            .with_runtime_context(Some(object_zone), SolidLevelQuirks::default());
        let current_environment = RetailSolidEnvironment::new(
            0,
            [0; 24],
            [0; 24],
            vec![
                RetailSolidZone::new(
                    [1_000; 3],
                    [100; 3],
                    0,
                    [0; 3],
                    vec![0; RETAIL_SOLID_RECT_BYTES],
                )
                .unwrap()
                .with_eid(current_zone),
            ],
        )
        .with_runtime_context(Some(current_zone), SolidLevelQuirks::default());
        let h = handle(0);
        let mut object = VmObject::new(h, Vec::new()).unwrap();
        object.bind_retail_solid_environment(object_environment);
        object
            .set_register(
                process_register::STATUS_B,
                crate::retail_physics::STATUS_B_TRANSLATION_MOTION
                    | crate::retail_physics::STATUS_B_STOPPED_BY_SOLID,
            )
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.set_current_retail_solid_environment(Some(current_environment));

        assert_eq!(
            machine.run_retail_object_physics(h),
            Err(VmError::SolidObjectZoneMissingFromBoundEnvironment {
                object: h,
                zone: object_zone,
            })
        );
    }

    #[test]
    fn solid_neighbor_sentinel_rect_uses_psx_wrapping_bounds() {
        let sentinel = RetailSolidZone::new(
            [i32::MAX; 3],
            [1; 3],
            0,
            [0; 3],
            vec![0; RETAIL_SOLID_RECT_BYTES],
        )
        .unwrap();
        let rect = RetailSolidRect::from_zone(&sentinel);
        assert_eq!(rect.origin, [-256; 3]);
        assert_eq!(rect.dimensions, [256; 3]);
        assert!(RetailSolidRect::contains_unscaled_zone_point(
            &sentinel, [-256; 3]
        ));
        assert!(RetailSolidRect::contains_unscaled_zone_point(
            &sentinel, [0; 3]
        ));
        assert!(!RetailSolidRect::contains_unscaled_zone_point(
            &sentinel, [1; 3]
        ));

        let environment =
            RetailSolidEnvironment::new(0, [0; COLOR_COUNT], [0; COLOR_COUNT], vec![sentinel]);
        assert_eq!(
            find_retail_solid_node(&environment, [8_279_948, 1_031_168, 24_872_448], 1, 25_000)
                .unwrap(),
            (None, [8_279_948, 1_056_168, 24_872_448]),
            "Great Hall's out-of-world y__ sentinel must be skipped, not fault ShadC"
        );
    }

    #[test]
    fn legal_solid_suboperation_three_gated_path_clears_b_and_copies_translation() {
        let h = handle(0);
        let instruction = 0x8e0e_de26;
        assert_eq!(instruction >> 24, 0x8e);
        let mut object = VmObject::new(h, vec![instruction]).unwrap();
        object
            .set_process_vector(0, [2_073_344, 1_371_648, 34_188_544])
            .unwrap();
        object.set_process_vector(5, [-1, -2, -3]).unwrap();
        object.set_register(process_register::ACK, 1).unwrap();
        object
            .set_register(process_register::ANIMATION_STAMP, 2)
            .unwrap();
        object
            .set_register(process_register::STATE_STAMP, 3)
            .unwrap();
        // The child status gate short-circuits before the parent gate. No
        // environment or parent link is required to preserve the C no-query.
        object
            .set_register(process_register::STATUS_B, 0x0000_0400)
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        machine.run(h, 1).unwrap();
        let object = machine.object(h).unwrap();
        assert_eq!(
            [
                object.register(process_register::ACK).unwrap(),
                object.register(process_register::ANIMATION_STAMP).unwrap(),
                object.register(process_register::STATE_STAMP).unwrap(),
            ],
            [0; 3]
        );
        assert_eq!(
            object.process_vector(5).unwrap(),
            [2_073_344, 1_371_648, 34_188_544]
        );
    }

    #[test]
    fn legal_solid_suboperation_three_active_octree_seeks_parent_colors() {
        let child = handle(0);
        let parent = handle(1);
        let instruction = 0x8e0e_de26;
        // Odd leaf, type zero, subtype 48 (two-percent color scale).
        let zone = RetailSolidZone::new(
            [0; 3],
            [1_000; 3],
            0x0301,
            [0; 3],
            vec![0; RETAIL_SOLID_RECT_BYTES],
        )
        .unwrap();
        let object_colors = [2_000_u16; COLOR_COUNT];
        let player_colors = [1_000_u16; COLOR_COUNT];
        let environment = RetailSolidEnvironment::new(0, object_colors, player_colors, vec![zone]);

        let mut parent_object = VmObject::new(parent, vec![0]).unwrap();
        parent_object
            .set_register(process_register::STATUS_B, 0x0400_0000)
            .unwrap();
        parent_object
            .set_register(process_register::NODE, 0xffff)
            .unwrap();
        parent_object.set_main_player_identity(true);
        parent_object.set_retail_colors([30; COLOR_COUNT]);
        parent_object.bind_retail_solid_environment(environment.clone());

        let translation = [25_600, 25_600, 25_600];
        let mut child_object = VmObject::new(child, vec![instruction]).unwrap();
        child_object
            .set_register(process_register::STATUS_B, 0x0400_0000)
            .unwrap();
        child_object.set_process_vector(0, translation).unwrap();
        child_object.set_process_vector(5, [-1, -2, -3]).unwrap();
        child_object
            .set_register(process_register::ACK, 11)
            .unwrap();
        child_object
            .set_register(process_register::ANIMATION_STAMP, 12)
            .unwrap();
        child_object
            .set_register(process_register::STATE_STAMP, 13)
            .unwrap();
        child_object.set_link(1, Some(parent)).unwrap();
        child_object.bind_retail_solid_environment(environment);

        let mut machine = Machine::new(0);
        machine.insert_object(parent_object).unwrap();
        machine.insert_object(child_object).unwrap();
        machine.run(child, 1).unwrap();

        let child_object = machine.object(child).unwrap();
        assert_eq!(child_object.process_vector(5).unwrap(), translation);
        assert_eq!(
            [
                child_object.register(process_register::ACK).unwrap(),
                child_object
                    .register(process_register::ANIMATION_STAMP)
                    .unwrap(),
                child_object
                    .register(process_register::STATE_STAMP)
                    .unwrap(),
            ],
            [0; 3]
        );
        let colors = machine.object(parent).unwrap().retail_colors();
        // Two percent of 1000 is 19 with the source's 12-bit factor. All
        // first twelve components are close enough to copy directly; the
        // unchanged color matrix/intensity seeks upward by exactly 350.
        assert_eq!(colors[..12], [19; 12]);
        assert_eq!(colors[12..], [380; 12]);
    }

    #[test]
    fn machine_frame_bound_api_preserves_order_capacity_and_clear_reuse() {
        let candidate = handle(0);
        let missing = handle(1);
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(candidate, vec![0]).unwrap())
            .unwrap();
        let bound = Bounds3 {
            min: Vec3 { x: 1, y: 2, z: 3 },
            max: Vec3 { x: 4, y: 5, z: 6 },
        };

        assert_eq!(
            machine.register_frame_bound(missing, bound),
            Err(VmError::UnknownObject(missing))
        );
        for _ in 0..crate::object_bounds::MAX_FRAME_BOUNDS {
            machine.register_frame_bound(candidate, bound).unwrap();
        }
        assert_eq!(
            machine.register_frame_bound(candidate, bound),
            Err(VmError::FrameBoundsCapacityExceeded)
        );
        assert_eq!(
            machine.frame_bounds().first(),
            Some(&FrameBound {
                bound,
                object: candidate,
            })
        );

        machine.clear_frame_bounds();
        assert!(machine.frame_bounds().is_empty());
        machine.register_frame_bound(candidate, bound).unwrap();
        assert_eq!(machine.frame_bounds().len(), 1);
    }

    #[test]
    fn stale_frame_bound_cannot_alias_a_reused_slot_before_the_first_solid_event() {
        let candidate = handle(0);
        let mover = handle(1);
        let zone_eid = Eid::from_name("gi_9Z").unwrap();
        let zone = RetailSolidZone::new(
            [-2_000; 3],
            [4_000; 3],
            0,
            [0; 3],
            vec![0; RETAIL_SOLID_RECT_BYTES],
        )
        .unwrap()
        .with_eid(zone_eid);
        let environment = RetailSolidEnvironment::new(0, [0; 24], [0; 24], vec![zone])
            .with_runtime_context(Some(zone_eid), SolidLevelQuirks::default());
        let mut original = VmObject::new(candidate, Vec::new()).unwrap();
        original
            .set_register(
                process_register::STATUS_B,
                crate::retail_solid_motion::SOLID_BOTTOM,
            )
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(original).unwrap();
        machine
            .register_frame_bound(
                candidate,
                Bounds3 {
                    min: Vec3 {
                        x: -20_000,
                        y: 250_000,
                        z: -20_000,
                    },
                    max: Vec3 {
                        x: 20_000,
                        y: 500_000,
                        z: 20_000,
                    },
                },
            )
            .unwrap();
        let registered_incarnation = machine.solid_frame_bound_incarnations[0];

        machine.remove_object(candidate).unwrap();
        let mut replacement = VmObject::new(candidate, Vec::new()).unwrap();
        replacement
            .set_register(
                process_register::STATUS_B,
                crate::retail_solid_motion::SOLID_BOTTOM,
            )
            .unwrap();
        machine.insert_object(replacement).unwrap();
        assert!(!machine.incarnation_is_live(candidate, registered_incarnation));
        assert_eq!(
            machine
                .find_retail_solid_object_node(&environment, [0, 300_000, 0], 9, 0, |_| true)
                .unwrap()
                .0,
            RetailSolidHit::None
        );

        let mut moving = VmObject::new(mover, Vec::new()).unwrap();
        moving.bind_retail_solid_environment(environment);
        moving
            .set_register(
                process_register::STATUS_B,
                crate::retail_physics::STATUS_B_TRANSLATION_MOTION
                    | crate::retail_physics::STATUS_B_STOPPED_BY_SOLID,
            )
            .unwrap();
        moving
            .set_register(process_register::MISC_A_Y, 6_000_000)
            .unwrap();
        machine.insert_object(moving).unwrap();
        let mut events = 0;

        machine
            .run_retail_object_physics_with_solid_event_handler(mover, |_, _, candidates, _| {
                assert!(candidates.iter().all(|candidate| !candidate.active));
                events += 1;
                true
            })
            .unwrap();

        assert_eq!(events, 0);
        assert_eq!(machine.object(candidate).unwrap().state_flags(), 0);
    }

    #[test]
    fn ordered_solid_bounds_use_live_status_first_hit_and_inclusive_padding() {
        let first = handle(1);
        let second = handle(2);
        let mut machine = Machine::new(0);
        for candidate in [first, second] {
            let mut object = VmObject::new(candidate, vec![0]).unwrap();
            object
                .set_register(process_register::STATUS_B, 0x0002_0000)
                .unwrap();
            machine.insert_object(object).unwrap();
        }
        let first_bound = Bounds3 {
            min: Vec3 {
                x: 100_000,
                y: -10,
                z: 0,
            },
            max: Vec3 {
                x: 110_000,
                y: 20,
                z: 10,
            },
        };
        let second_bound = Bounds3 {
            min: Vec3 {
                x: 100_000,
                y: -10,
                z: 0,
            },
            max: Vec3 {
                x: 110_000,
                y: 30,
                z: 10,
            },
        };
        machine.register_frame_bound(first, first_bound).unwrap();
        machine.register_frame_bound(second, second_bound).unwrap();
        let environment = RetailSolidEnvironment::new(0, [0; 24], [0; 24], Vec::new());
        let translation = [80_000, 0, -20_000];

        assert_eq!(
            machine
                .find_retail_solid_object_node(
                    &environment,
                    translation,
                    9,
                    20_000,
                    |status| status & 0x0002_0000 != 0,
                )
                .unwrap(),
            (RetailSolidHit::Object(first), [80_000, 20, -20_000])
        );

        machine
            .object_mut(first)
            .unwrap()
            .set_register(process_register::STATUS_B, 0)
            .unwrap();
        assert_eq!(
            machine
                .find_retail_solid_object_node(
                    &environment,
                    translation,
                    9,
                    20_000,
                    |status| status & 0x0002_0000 != 0,
                )
                .unwrap(),
            (RetailSolidHit::Object(second), [80_000, 30, -20_000]),
            "the AABB is snapshotted but candidate status remains live"
        );
    }

    #[test]
    fn checked_object_collision_writes_reciprocal_links_and_hotspot_bits() {
        let target = handle(0);
        let source = handle(1);
        let mut target_object = VmObject::new(target, Vec::new()).unwrap();
        let mut source_object = VmObject::new(source, Vec::new()).unwrap();
        target_object
            .set_register(process_register::HOTSPOT_SIZE, 10)
            .unwrap();
        source_object
            .set_register(process_register::HOTSPOT_SIZE, 5)
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(target_object).unwrap();
        machine.insert_object(source_object).unwrap();
        let target_bound = Bounds3 {
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
        };
        let source_bound = Bounds3 {
            min: Vec3 {
                x: -50,
                y: -50,
                z: -50,
            },
            max: Vec3 {
                x: 50,
                y: 50,
                z: 50,
            },
        };

        assert!(
            machine
                .collide_retail_objects(target, target_bound, source, source_bound)
                .unwrap()
        );

        assert_eq!(machine.object(target).unwrap().links[6], Some(source));
        assert_eq!(machine.object(source).unwrap().links[6], Some(target));
        assert_ne!(
            machine
                .object(target)
                .unwrap()
                .register(process_register::STATUS_A)
                .unwrap()
                & STATUS_HOTSPOT_COLLISION,
            0
        );
        assert_ne!(
            machine
                .object(source)
                .unwrap()
                .register(process_register::STATUS_A)
                .unwrap()
                & STATUS_HOTSPOT_COLLISION,
            0
        );
    }

    #[test]
    fn restart_clears_only_crash_current_collider_pair() {
        let crash = handle(0);
        let current = handle(1);
        let doctor = handle(2);
        let mut crash_object = VmObject::new(crash, Vec::new()).unwrap();
        crash_object.set_link(6, Some(current)).unwrap();
        let mut current_object = VmObject::new(current, Vec::new()).unwrap();
        current_object.set_link(6, Some(crash)).unwrap();
        let mut doctor_object = VmObject::new(doctor, Vec::new()).unwrap();
        doctor_object.set_link(6, Some(crash)).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(crash_object).unwrap();
        machine.insert_object(current_object).unwrap();
        machine.insert_object(doctor_object).unwrap();

        machine.clear_retail_collider_pair(crash).unwrap();

        assert_eq!(machine.object(crash).unwrap().links[6], None);
        assert_eq!(machine.object(current).unwrap().links[6], None);
        assert_eq!(
            machine.object(doctor).unwrap().links[6],
            Some(crash),
            "native LevelRestart leaves unrelated asymmetric collider links intact"
        );
    }

    #[test]
    fn restart_clears_crash_collider_storage_after_pool_reclamation() {
        let crash = handle(0);
        let collider = handle(1);
        let doctor = handle(2);
        let mut crash_object = VmObject::new(crash, Vec::new()).unwrap();
        crash_object
            .set_link(PROCESS_LINK_COLLIDER, Some(collider))
            .unwrap();
        let mut collider_object = VmObject::new(collider, Vec::new()).unwrap();
        collider_object
            .set_link(PROCESS_LINK_COLLIDER, Some(crash))
            .unwrap();
        let mut doctor_object = VmObject::new(doctor, Vec::new()).unwrap();
        doctor_object
            .set_link(PROCESS_LINK_COLLIDER, Some(crash))
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(crash_object).unwrap();
        machine.insert_object(collider_object).unwrap();
        machine.insert_object(doctor_object).unwrap();
        machine.bind_retail_pool_slot(collider, 0).unwrap();
        machine
            .remove_object_from_retail_pool_slot(collider, 0)
            .unwrap();

        assert_eq!(
            machine
                .object(crash)
                .unwrap()
                .register_pool_slot(PROCESS_LINK_COLLIDER),
            Ok(Some(0))
        );
        assert_eq!(
            machine.resolve_process_link(crash, PROCESS_LINK_COLLIDER),
            Ok(None)
        );
        assert_ne!(
            machine
                .retail_pool_register_word(0, PROCESS_LINK_COLLIDER)
                .unwrap()
                .0,
            0
        );

        machine.clear_retail_collider_pair(crash).unwrap();

        assert_eq!(
            machine
                .object(crash)
                .unwrap()
                .register(PROCESS_LINK_COLLIDER),
            Ok(0)
        );
        assert_eq!(
            machine.retail_pool_register_word(0, PROCESS_LINK_COLLIDER),
            Ok((0, None))
        );
        assert_eq!(
            machine.object(doctor).unwrap().links[PROCESS_LINK_COLLIDER],
            Some(crash),
            "clearing a reclaimed collider must not sever the Doctor's asymmetric Crash link"
        );
    }

    #[test]
    fn checked_object_collision_preserves_native_priority_override_branches() {
        let target = handle(0);
        let current = handle(1);
        let source = handle(2);
        let bound = Bounds3 {
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
        };
        let build = |source_flags: u32, current_flags: u32| {
            let mut target_object = VmObject::new(target, Vec::new()).unwrap();
            target_object.set_link(6, Some(current)).unwrap();
            let mut current_object = VmObject::new(current, Vec::new()).unwrap();
            current_object
                .set_retail_transform(RetailTransform {
                    translation: [100, 0, 0],
                    ..RetailTransform::default()
                })
                .unwrap();
            current_object
                .set_register(process_register::STATE_FLAGS, current_flags)
                .unwrap();
            let mut source_object = VmObject::new(source, Vec::new()).unwrap();
            source_object
                .set_retail_transform(RetailTransform {
                    translation: [200, 0, 0],
                    ..RetailTransform::default()
                })
                .unwrap();
            source_object
                .set_register(process_register::STATE_FLAGS, source_flags)
                .unwrap();
            let mut machine = Machine::new(0);
            machine.insert_object(target_object).unwrap();
            machine.insert_object(current_object).unwrap();
            machine.insert_object(source_object).unwrap();
            machine
        };

        let mut farther = build(0, 0);
        assert!(
            !farther
                .collide_retail_objects(target, bound, source, bound)
                .unwrap()
        );
        assert_eq!(farther.object(target).unwrap().links[6], Some(current));
        assert_eq!(farther.object(source).unwrap().links[6], None);

        let mut source_override = build(0x800, 0);
        assert!(
            !source_override
                .collide_retail_objects(target, bound, source, bound)
                .unwrap()
        );
        assert_eq!(
            source_override.object(target).unwrap().links[6],
            Some(current)
        );
        assert_eq!(
            source_override.object(source).unwrap().links[6],
            Some(target)
        );

        let mut current_override = build(0, 0x800);
        assert!(
            current_override
                .collide_retail_objects(target, bound, source, bound)
                .unwrap()
        );
        assert_eq!(
            current_override.object(target).unwrap().links[6],
            Some(source)
        );
        assert_eq!(
            current_override.object(source).unwrap().links[6],
            Some(target)
        );
    }

    #[test]
    fn solid_motion_refresh_resolves_live_collider_outside_frame_bounds() {
        let mover = handle(0);
        let first = handle(1);
        let second = handle(2);
        let mut mover_object = VmObject::new(mover, Vec::new()).unwrap();
        mover_object.set_link(6, Some(first)).unwrap();

        let mut first_object = VmObject::new(first, Vec::new()).unwrap();
        first_object
            .set_retail_transform(RetailTransform {
                translation: [100, 200, 300],
                ..RetailTransform::default()
            })
            .unwrap();
        first_object
            .set_register(
                process_register::STATUS_B,
                crate::retail_solid_motion::BOX_OBJECT,
            )
            .unwrap();
        first_object
            .set_register(process_register::STATE_FLAGS, 0x800)
            .unwrap();
        first_object
            .set_register(process_register::HOTSPOT_SIZE, 17)
            .unwrap();
        first_object.configure_test_program_identity_with_type(0, 0x22);

        let mut second_object = VmObject::new(second, Vec::new()).unwrap();
        second_object
            .set_retail_transform(RetailTransform {
                translation: [-400, -500, -600],
                ..RetailTransform::default()
            })
            .unwrap();
        second_object
            .set_register(process_register::STATUS_B, 0x1234)
            .unwrap();
        second_object
            .set_register(process_register::STATE_FLAGS, 0x5678)
            .unwrap();
        second_object
            .set_register(process_register::HOTSPOT_SIZE, 23)
            .unwrap();
        second_object.configure_test_program_identity_with_type(0, 11);

        let mut machine = Machine::new(0);
        machine.insert_object(mover_object).unwrap();
        machine.insert_object(first_object).unwrap();
        machine.insert_object(second_object).unwrap();
        assert!(machine.frame_bounds().is_empty());

        let mut state = SolidMotionState::default();
        machine
            .refresh_live_solid_motion_state(mover, &mut state)
            .unwrap();
        assert_eq!(
            state.collider,
            Some(SolidColliderState {
                id: u32::from(first.get()),
                translation: Vec3 {
                    x: 100,
                    y: 200,
                    z: 300,
                },
                status_b: crate::retail_solid_motion::BOX_OBJECT,
                state_flags: 0x800,
                object_type: 0x22,
                hotspot_size: 17,
            })
        );

        // A synchronous handler may replace link six. The refresh must follow
        // that post-handler link instead of retaining the pre-dispatch object.
        machine
            .object_mut(mover)
            .unwrap()
            .set_link(6, Some(second))
            .unwrap();
        machine
            .refresh_live_solid_motion_state(mover, &mut state)
            .unwrap();
        assert_eq!(
            state.collider,
            Some(SolidColliderState {
                id: u32::from(second.get()),
                translation: Vec3 {
                    x: -400,
                    y: -500,
                    z: -600,
                },
                status_b: 0x1234,
                state_flags: 0x5678,
                object_type: 11,
                hotspot_size: 23,
            })
        );
    }

    #[test]
    fn checked_object_collision_handles_target_source_self_alias() {
        let object = handle(0);
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(object, Vec::new()).unwrap())
            .unwrap();
        let bound = Bounds3 {
            min: Vec3 {
                x: -10,
                y: -10,
                z: -10,
            },
            max: Vec3 {
                x: 10,
                y: 10,
                z: 10,
            },
        };

        assert!(
            machine
                .collide_retail_objects(object, bound, object, bound)
                .unwrap()
        );
        assert_eq!(machine.object(object).unwrap().links[6], Some(object));
    }

    #[test]
    fn ordered_solid_bounds_keep_first_highest_tie_and_node_priority() {
        let low = handle(1);
        let first_high = handle(2);
        let tied_high = handle(3);
        let mut machine = Machine::new(0);
        for candidate in [low, first_high, tied_high] {
            let mut object = VmObject::new(candidate, vec![0]).unwrap();
            object
                .set_register(process_register::STATUS_B, 0x0002_0000)
                .unwrap();
            machine.insert_object(object).unwrap();
        }
        for (candidate, maximum_y) in [(low, 100), (first_high, 200), (tied_high, 200)] {
            machine
                .register_frame_bound(
                    candidate,
                    Bounds3 {
                        min: Vec3 {
                            x: -10,
                            y: 0,
                            z: -10,
                        },
                        max: Vec3 {
                            x: 10,
                            y: maximum_y,
                            z: 10,
                        },
                    },
                )
                .unwrap();
        }
        let empty = RetailSolidEnvironment::new(0, [0; 24], [0; 24], Vec::new());
        assert_eq!(
            machine
                .find_retail_solid_object_node(&empty, [0, 300, 0], 9, 20_000, |status| {
                    status & 0x0002_0000 != 0
                })
                .unwrap(),
            (RetailSolidHit::Object(first_high), [0, 200, 0]),
            "strictly-greater replacement keeps the first equal-height bound"
        );

        machine.clear_frame_bounds();
        machine
            .register_frame_bound(
                low,
                Bounds3 {
                    min: Vec3 { x: 0, y: 0, z: 0 },
                    max: Vec3 {
                        x: 100_000,
                        y: 100_000,
                        z: 100_000,
                    },
                },
            )
            .unwrap();
        let zone = RetailSolidZone::new(
            [0; 3],
            [1_000; 3],
            0x0301,
            [0; 3],
            vec![0; RETAIL_SOLID_RECT_BYTES],
        )
        .unwrap();
        let environment = RetailSolidEnvironment::new(0, [0; 24], [0; 24], vec![zone]);
        assert_eq!(
            machine
                .find_retail_solid_object_node(
                    &environment,
                    [25_600, 200_000, 25_600],
                    9,
                    20_000,
                    |status| status & 0x0002_0000 != 0,
                )
                .unwrap(),
            (RetailSolidHit::Node(0x0301), [25_600, 256_000, 25_600]),
            "a lower object does not override a nearer octree surface"
        );

        machine.clear_frame_bounds();
        machine
            .register_frame_bound(
                low,
                Bounds3 {
                    // Inverted Y can result from retail's signed scale path.
                    // It avoids the direct-overlap branch while exercising
                    // the exact `highest >= node_y` override tie.
                    min: Vec3 {
                        x: 0,
                        y: 300_000,
                        z: 0,
                    },
                    max: Vec3 {
                        x: 100_000,
                        y: 256_000,
                        z: 100_000,
                    },
                },
            )
            .unwrap();
        assert_eq!(
            machine
                .find_retail_solid_object_node(
                    &environment,
                    [25_600, 256_000, 25_600],
                    9,
                    20_000,
                    |status| status & 0x0002_0000 != 0,
                )
                .unwrap(),
            (RetailSolidHit::Object(low), [25_600, 256_000, 25_600])
        );
    }

    #[test]
    fn solid_suboperation_one_reuses_static_trans3_and_writes_nearest_node() {
        let child = handle(0);
        let parent = handle(1);
        let zone = RetailSolidZone::new(
            [0; 3],
            [1_000; 3],
            0x0301,
            [0; 3],
            vec![0; RETAIL_SOLID_RECT_BYTES],
        )
        .unwrap();
        let environment = RetailSolidEnvironment::new(0, [0; 24], [0; 24], vec![zone]);
        let translation = [25_600, 25_600, 25_600];
        let mut child_object = VmObject::new(child, vec![0x8e0e_de26, 0x8e06_de26]).unwrap();
        child_object.set_process_vector(0, translation).unwrap();
        child_object.set_process_vector(5, [-1, -2, -3]).unwrap();
        child_object.set_link(1, Some(parent)).unwrap();
        child_object.bind_retail_solid_environment(environment);
        let mut parent_object = VmObject::new(parent, vec![0]).unwrap();
        parent_object.set_main_player_identity(true);

        let mut machine = Machine::new(0);
        machine.insert_object(parent_object).unwrap();
        machine.insert_object(child_object).unwrap();
        machine.run(child, 2).unwrap();

        let child_object = machine.object(child).unwrap();
        assert_eq!(
            child_object.register(process_register::MISC_VALUE),
            Ok(0x0301)
        );
        assert_eq!(
            [
                child_object.register(process_register::ACK).unwrap(),
                child_object
                    .register(process_register::ANIMATION_STAMP)
                    .unwrap(),
                child_object
                    .register(process_register::STATE_STAMP)
                    .unwrap(),
            ],
            translation.map(|value| value as u32),
            "suboperation one must output the prior function-static trans3"
        );
        assert_eq!(
            child_object.process_vector(5).unwrap(),
            [translation[0], 256_000, translation[2]]
        );
    }

    #[test]
    fn solid_suboperation_one_active_shadow_query_updates_parent_size_without_bounds() {
        let child = handle(0);
        let parent = handle(1);
        // Size selector three lives in bits 10..13 independently of the
        // nearest-node subtype bits.
        let zone = RetailSolidZone::new(
            [0; 3],
            [1_000; 3],
            0x0c01,
            [0; 3],
            vec![0; RETAIL_SOLID_RECT_BYTES],
        )
        .unwrap();
        let environment = RetailSolidEnvironment::new(0, [0; 24], [0; 24], vec![zone]);
        let mut child_object = VmObject::new(child, vec![0x8e06_de26]).unwrap();
        child_object
            .set_register(process_register::STATUS_B, 0x0400_0000)
            .unwrap();
        child_object
            .set_process_vector(0, [25_600, 25_600, 25_600])
            .unwrap();
        child_object.set_link(1, Some(parent)).unwrap();
        child_object.bind_retail_solid_environment(environment);
        let mut parent_object = VmObject::new(parent, vec![0]).unwrap();
        parent_object.set_main_player_identity(true);

        let mut machine = Machine::new(0);
        machine.insert_object(parent_object).unwrap();
        machine.insert_object(child_object).unwrap();
        machine.run(child, 1).unwrap();

        assert_eq!(
            machine
                .object(parent)
                .unwrap()
                .register(process_register::SIZE),
            Ok((-40_i32) as u32),
            "size-map selector three is -64 plus the default 0x18 increment"
        );
        assert_eq!(
            machine
                .object(child)
                .unwrap()
                .register(process_register::MISC_VALUE),
            Ok(0x0c01)
        );
    }

    #[test]
    fn solid_suboperation_one_ignores_live_objects_without_frame_snapshots() {
        let child = handle(0);
        let parent = handle(1);
        let candidate = handle(2);
        let zone = RetailSolidZone::new(
            [0; 3],
            [1_000; 3],
            0x0301,
            [0; 3],
            vec![0; RETAIL_SOLID_RECT_BYTES],
        )
        .unwrap();
        let environment = RetailSolidEnvironment::new(0, [0; 24], [0; 24], vec![zone]);
        let mut child_object = VmObject::new(child, vec![0x8e06_de26]).unwrap();
        child_object
            .set_register(process_register::STATUS_B, 0x0400_0000)
            .unwrap();
        child_object
            .set_process_vector(0, [25_600, 25_600, 25_600])
            .unwrap();
        child_object.set_link(1, Some(parent)).unwrap();
        child_object.bind_retail_solid_environment(environment);
        let mut parent_object = VmObject::new(parent, vec![0]).unwrap();
        parent_object.set_main_player_identity(true);
        let mut candidate_object = VmObject::new(candidate, vec![0]).unwrap();
        candidate_object
            .set_register(process_register::STATUS_B, 0x0002_0000)
            .unwrap();

        let mut machine = Machine::new(0);
        machine.insert_object(parent_object).unwrap();
        machine.insert_object(candidate_object).unwrap();
        machine.insert_object(child_object).unwrap();
        machine.run(child, 1).unwrap();
        assert_eq!(
            machine
                .object(parent)
                .unwrap()
                .register(process_register::SIZE),
            Ok(0x18),
            "a live status bit alone does not fabricate a frame AABB"
        );
        assert_eq!(
            machine
                .object(child)
                .unwrap()
                .register(process_register::MISC_VALUE),
            Ok(0x0301)
        );
    }

    #[test]
    fn solid_suboperation_one_uses_both_retail_paddings_and_live_shadow_size() {
        let child = handle(0);
        let parent = handle(1);
        let shadow_candidate = handle(2);
        let floor_candidate = handle(3);
        let environment = RetailSolidEnvironment::new(0, [0; 24], [0; 24], Vec::new());
        let translation = [65_000, 100_000, 0];

        let mut child_object = VmObject::new(child, vec![0x8e06_de26]).unwrap();
        child_object
            .set_register(process_register::STATUS_B, 0x0400_0000)
            .unwrap();
        child_object.set_process_vector(0, translation).unwrap();
        child_object.set_link(1, Some(parent)).unwrap();
        child_object.bind_retail_solid_environment(environment);

        let mut parent_object = VmObject::new(parent, vec![0]).unwrap();
        parent_object.set_main_player_identity(true);
        parent_object
            .set_register(process_register::STATE_FLAGS, 0x08)
            .unwrap();

        let mut shadow_object = VmObject::new(shadow_candidate, vec![0]).unwrap();
        shadow_object
            .set_register(process_register::STATUS_B, 0x0002_0000)
            .unwrap();
        shadow_object
            .set_register(process_register::SIZE, 1)
            .unwrap();
        let mut floor_object = VmObject::new(floor_candidate, vec![0]).unwrap();
        floor_object
            .set_register(process_register::STATUS_B, 0x0002_0000)
            .unwrap();

        let mut machine = Machine::new(0);
        machine.insert_object(parent_object).unwrap();
        machine.insert_object(shadow_object).unwrap();
        machine.insert_object(floor_object).unwrap();
        machine.insert_object(child_object).unwrap();
        machine
            .register_frame_bound(
                shadow_candidate,
                Bounds3 {
                    min: Vec3 {
                        x: 100_000,
                        y: 90_000,
                        z: -1,
                    },
                    max: Vec3 {
                        x: 110_000,
                        y: 110_000,
                        z: 1,
                    },
                },
            )
            .unwrap();
        machine
            .register_frame_bound(
                floor_candidate,
                Bounds3 {
                    min: Vec3 {
                        x: 80_000,
                        y: 80_000,
                        z: -1,
                    },
                    max: Vec3 {
                        x: 90_000,
                        y: 120_000,
                        z: 1,
                    },
                },
            )
            .unwrap();
        // Size is intentionally changed after the AABB snapshot: helper two
        // must read this process field at query time.
        machine
            .object_mut(shadow_candidate)
            .unwrap()
            .set_register(process_register::SIZE, 100)
            .unwrap();
        machine.set_camera_translation([0, 59_040, 0]);
        machine.run(child, 1).unwrap();

        assert_eq!(
            machine
                .object(parent)
                .unwrap()
                .register(process_register::SIZE),
            Ok(114),
            "35k padding selects size 100, subtracts 10 camera units, and adds 0x18"
        );
        let misc = machine
            .object(child)
            .unwrap()
            .register(process_register::MISC_VALUE)
            .unwrap();
        assert_eq!(
            CollisionObjectReference::from_word(misc),
            Some(CollisionObjectReference::new(floor_candidate)),
            "20k padding skips the first AABB and selects the second"
        );
        assert_eq!(
            machine.object(child).unwrap().process_vector(5),
            Ok([65_000, 120_000, 0])
        );

        machine.object_mut(child).unwrap().restart(0).unwrap();
        machine
            .object_mut(parent)
            .unwrap()
            .set_register(process_register::STATE_FLAGS, 0x10)
            .unwrap();
        machine.run(child, 1).unwrap();
        assert_eq!(
            machine
                .object(parent)
                .unwrap()
                .register(process_register::SIZE),
            Ok(90),
            "state flag 0x10 alone suppresses the 0x18 parent increment"
        );

        machine.object_mut(child).unwrap().restart(0).unwrap();
        machine
            .object_mut(parent)
            .unwrap()
            .set_register(process_register::STATE_FLAGS, 0x08)
            .unwrap();
        machine
            .object_mut(shadow_candidate)
            .unwrap()
            .set_register(process_register::STATUS_B, 0x4002_0000)
            .unwrap();
        machine
            .object_mut(floor_candidate)
            .unwrap()
            .set_register(process_register::STATUS_B, 0)
            .unwrap();
        machine.run(child, 1).unwrap();
        assert_eq!(
            machine
                .object(parent)
                .unwrap()
                .register(process_register::SIZE),
            Ok(0x18),
            "helper two excludes candidates carrying status bit 0x40000000"
        );
    }

    #[test]
    fn solid_suboperation_three_object_node_can_disable_color_seek() {
        let child = handle(0);
        let parent = handle(1);
        let first = handle(2);
        let second = handle(3);
        let environment = RetailSolidEnvironment::new(0, [2_000; 24], [1_000; 24], Vec::new());

        let mut child_object = VmObject::new(child, vec![0x8e0e_de26]).unwrap();
        child_object
            .set_register(process_register::STATUS_B, 0x0400_0000)
            .unwrap();
        child_object.set_process_vector(0, [0; 3]).unwrap();
        child_object.set_link(1, Some(parent)).unwrap();
        child_object.bind_retail_solid_environment(environment.clone());
        let mut parent_object = VmObject::new(parent, vec![0]).unwrap();
        parent_object
            .set_register(process_register::STATUS_B, 0x0400_0000)
            .unwrap();
        parent_object
            .set_register(process_register::NODE, 0xffff)
            .unwrap();
        parent_object.set_retail_colors([0; COLOR_COUNT]);
        parent_object.bind_retail_solid_environment(environment);
        let mut first_object = VmObject::new(first, vec![0]).unwrap();
        first_object
            .set_register(process_register::NODE, (-55_i32) as u32)
            .unwrap();
        let mut second_object = VmObject::new(second, vec![0]).unwrap();
        second_object
            .set_register(process_register::NODE, (-48_i32) as u32)
            .unwrap();

        let mut machine = Machine::new(0);
        machine.insert_object(parent_object).unwrap();
        machine.insert_object(first_object).unwrap();
        machine.insert_object(second_object).unwrap();
        machine.insert_object(child_object).unwrap();
        let direct_bound = Bounds3 {
            min: Vec3 {
                x: -10,
                y: 20_000,
                z: -10,
            },
            max: Vec3 {
                x: 10,
                y: 30_000,
                z: 10,
            },
        };
        machine.register_frame_bound(first, direct_bound).unwrap();
        machine.register_frame_bound(second, direct_bound).unwrap();

        machine.run(child, 1).unwrap();

        assert_eq!(
            machine.object(parent).unwrap().retail_colors(),
            &[2_000; COLOR_COUNT],
            "the first direct bound wins and NODE -55 clears the 350-unit seek step"
        );
    }

    #[test]
    fn solid_suboperation_three_keeps_first_highest_y_bound_below_query() {
        let query = handle(0);
        let low = handle(1);
        let first_high = handle(2);
        let tied_high = handle(3);
        let mut machine = Machine::new(0);
        for candidate in [query, low, first_high, tied_high] {
            let mut object = VmObject::new(candidate, vec![0]).unwrap();
            object
                .set_register(process_register::NODE, u32::from(candidate.get()))
                .unwrap();
            machine.insert_object(object).unwrap();
        }
        for (candidate, maximum_y) in [(low, 10_000), (first_high, 20_000), (tied_high, 20_000)] {
            machine
                .register_frame_bound(
                    candidate,
                    Bounds3 {
                        min: Vec3 {
                            x: -10,
                            y: 0,
                            z: -10,
                        },
                        max: Vec3 {
                            x: 10,
                            y: maximum_y,
                            z: 10,
                        },
                    },
                )
                .unwrap();
        }
        let environment = RetailSolidEnvironment::new(0, [0; 24], [0; 24], Vec::new());

        assert_eq!(
            machine
                .find_retail_solid_object_node_three(query, &environment, [0; 3], 5)
                .unwrap(),
            RetailSolidHit::Object(first_high),
            "strictly-greater replacement keeps the first equal-height frame bound"
        );
    }

    #[test]
    fn solid_suboperation_three_uses_exclusive_z_max_and_ordered_nearest_z() {
        let query = handle(0);
        let edge = handle(1);
        let direct = handle(2);
        let low = handle(3);
        let first_high = handle(4);
        let tied_high = handle(5);
        let mut machine = Machine::new(0);
        for candidate in [query, edge, direct, low, first_high, tied_high] {
            let mut object = VmObject::new(candidate, vec![0]).unwrap();
            object.set_register(process_register::NODE, 48).unwrap();
            machine.insert_object(object).unwrap();
        }
        let z_bound = |minimum_z, maximum_z| Bounds3 {
            min: Vec3 {
                x: -10,
                y: 20_000,
                z: minimum_z,
            },
            max: Vec3 {
                x: 10,
                y: 30_000,
                z: maximum_z,
            },
        };
        machine
            .register_frame_bound(edge, z_bound(200, 300))
            .unwrap();
        machine
            .register_frame_bound(direct, z_bound(300, 400))
            .unwrap();
        let environment = RetailSolidEnvironment::new(1, [0; 24], [0; 24], Vec::new());

        assert_eq!(
            machine
                .find_retail_solid_object_node_three(query, &environment, [0, 0, 300], 6)
                .unwrap(),
            RetailSolidHit::Object(direct),
            "z == max is not a direct hit, allowing the next ordered bound to contain the point"
        );

        machine.clear_frame_bounds();
        for (candidate, maximum_z) in [(low, 100), (first_high, 200), (tied_high, 200)] {
            machine
                .register_frame_bound(candidate, z_bound(0, maximum_z))
                .unwrap();
        }
        assert_eq!(
            machine
                .find_retail_solid_object_node_three(query, &environment, [0, 0, 300], 6)
                .unwrap(),
            RetailSolidHit::Object(first_high),
            "the nearest behind candidate uses strict replacement and preserves traversal order"
        );
    }

    #[test]
    fn entity_node_color_opcode_applies_retail_uniform_scale_layout() {
        let h = handle(0);
        let instruction = (0x8e_u32 << 24) | (6 << 18);
        let mut object = VmObject::new(h, vec![instruction]).unwrap();
        let mut colors = [0_u16; COLOR_COUNT];
        colors[0] = (-1000_i16) as u16;
        colors[9] = 1000;
        colors[12] = 777;
        colors[21] = 888;
        object.set_retail_colors(colors);
        // Node subtype 53 selects percent_map[5] = 72 percent.
        object.entity_spawn_flags = Some(0x0350 << 3);
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        machine.run(h, 1).unwrap();
        let colors = machine.object(h).unwrap().retail_colors();
        assert_eq!(colors[0] as i16, -720);
        assert_eq!(colors[9], 719);
        assert_eq!(colors[12], 777);
        assert_eq!(colors[21], 888);
    }

    #[test]
    fn main_player_state_flag_forces_retail_full_color_scale() {
        let h = handle(0);
        let instruction = (0x8e_u32 << 24) | (6 << 18);
        let mut object = VmObject::new(h, vec![instruction]).unwrap();
        let mut colors = [0_u16; COLOR_COUNT];
        colors[0] = (-1000_i16) as u16;
        colors[9] = 1000;
        object.set_retail_colors(colors);
        // Node subtype 53 normally selects 72 percent, but retail forces
        // subtype 0x37 (100 percent) for Crash while state flag 0x20 is set.
        object.entity_spawn_flags = Some(0x0350 << 3);
        object.set_main_player_identity(true);
        object
            .set_register(process_register::STATE_FLAGS, 0x20)
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        machine.run(h, 1).unwrap();
        let colors = machine.object(h).unwrap().retail_colors();
        assert_eq!(colors[0] as i16, -1000);
        assert_eq!(colors[9], 1000);
    }

    #[test]
    fn entity_node_color_opcode_uses_current_level_for_hard_coded_selector() {
        let h = handle(0);
        let instruction = (0x8e_u32 << 24) | (6 << 18);
        let mut object = VmObject::new(h, vec![instruction]).unwrap();
        object.set_retail_colors([777; COLOR_COUNT]);
        object.entity_spawn_flags = Some(40 << 7);
        let mut machine = Machine::new(1);
        machine.initialize_retail_level_globals(LevelId::new_const(0x03));
        machine.insert_object(object).unwrap();

        machine.run(h, 1).unwrap();
        assert_eq!(
            machine.object(h).unwrap().retail_colors(),
            &[
                0,
                (-8_601_i16) as u16,
                0,
                (-3_809_i16) as u16,
                (-1_679_i16) as u16,
                2_621,
                3_563,
                4_915,
                (-286_i16) as u16,
                0,
                255,
                255,
                0,
                255,
                0,
                88,
                637,
                90,
                284,
                128,
                128,
                255,
                255,
                255,
            ]
        );
    }

    #[test]
    fn hard_coded_retail_color_selectors_match_all_five_levels() {
        let source = [777_u16; COLOR_COUNT];
        let common_light_and_color = [
            0,
            (-8_601_i16) as u16,
            0,
            (-3_809_i16) as u16,
            (-1_679_i16) as u16,
            2_621,
            3_563,
            4_915,
            (-286_i16) as u16,
            0,
            255,
            255,
        ];

        let cortex = scaled_retail_colors(&source, 40, Some(0x03)).unwrap();
        assert_eq!(cortex[..12], common_light_and_color);
        assert_eq!(
            cortex[12..],
            [0, 255, 0, 88, 637, 90, 284, 128, 128, 255, 255, 255]
        );

        let toxic = scaled_retail_colors(&source, 40, Some(0x07)).unwrap();
        assert_eq!(toxic[..12], common_light_and_color);
        assert_eq!(
            toxic[12..],
            [192, 255, 192, 224, 400, 224, 260, 240, 240, 255, 255, 255]
        );

        let boulder = scaled_retail_colors(&source, 40, Some(0x13)).unwrap();
        assert_eq!(boulder[..12], [777; 12]);
        assert_eq!(
            boulder[12..],
            [0, 944, 944, 0, 249, 255, 0, 100, 255, 0, 255, 255]
        );

        for level in [0x03, 0x07, 0x13] {
            for subtype in 41..=44 {
                let colors = scaled_retail_colors(&source, subtype, Some(level)).unwrap();
                assert_eq!(colors[..12], [0; 12]);
                assert_eq!(colors[12..], [777; 12]);
            }
        }

        let mut tinted_source = [0_u16; COLOR_COUNT];
        for row in 0..3 {
            tinted_source[row * 3] = (-1_000_i16) as u16;
            tinted_source[row * 3 + 1] = 1_000;
            tinted_source[row * 3 + 2] = 2_000;
        }
        tinted_source[9..12].copy_from_slice(&[100, 200, 300]);
        tinted_source[12..].fill(555);
        for level in [0x1c, 0x1d] {
            for (subtype, red_percentage) in (40..=44).zip([50_i32, 75, 100, 125, 150]) {
                let colors = scaled_retail_colors(&tinted_source, subtype, Some(level)).unwrap();
                let factor = (red_percentage << 12) / 100;
                let expected_signed = ((-1_000_i64 * i64::from(factor)) >> 12) as i16;
                let expected_color = ((100 * factor) >> 12) as u16;
                for row in 0..3 {
                    assert_eq!(colors[row * 3] as i16, expected_signed);
                    assert_eq!(colors[row * 3 + 1], 1_000);
                    assert_eq!(colors[row * 3 + 2], 2_000);
                }
                assert_eq!(colors[9..12], [expected_color, 200, 300]);
                assert_eq!(colors[12..], [555; 12]);
            }
        }

        assert_eq!(
            scaled_retail_colors(&source, 64, Some(0x1c)),
            Err(VmError::InvalidColorSubtype(64))
        );
    }

    #[test]
    fn animation_reference_opcode_rejects_unbound_global_item() {
        let h = handle(0);
        let instruction = Instruction::encode(0x27, 0x0800, REG0);
        let object = VmObject::new(h, vec![instruction]).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        assert_eq!(machine.run(h, 1), Err(VmError::AnimationDataUnbound));
        machine.object_mut(h).unwrap().bind_animation_data(&[0]);
        machine.object_mut(h).unwrap().restart(0).unwrap();
        machine.run(h, 1).unwrap();
        assert!(
            AnimationReference::from_word(machine.object(h).unwrap().register(0).unwrap())
                .is_some()
        );
    }

    #[test]
    fn exact_crash_jal_calls_absolute_global_word_and_returns_to_external_code() {
        let h = handle(0);
        let mut object =
            VmObject::new(h, vec![0x8609_806e, Instruction::encode(0x00, REG0, REG1)]).unwrap();
        object.set_register(0, 2).unwrap();
        object.set_register(1, 3).unwrap();
        object.global_code = vec![0; 132];
        object.global_code[110] = control_flow(2, 0, 0, 0, 0);

        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        assert_eq!(
            machine.run(h, 2).unwrap(),
            Execution {
                reason: HaltReason::BudgetExhausted,
                steps: 2,
            }
        );
        assert_eq!(
            machine.object(h).unwrap().code_address(),
            CodeAddress {
                segment: CodeSegment::External,
                pc: 1,
            }
        );

        machine.run(h, 1).unwrap();
        assert_eq!(machine.object(h).unwrap().stack(), &[5]);
    }

    #[test]
    fn movc_writes_checked_global_code_references_to_register_or_stack() {
        let h = handle(0);
        let register_reference = (0x18_u32 << 24) | (3 << 14) | 2;
        let transition_reference =
            (0x18_u32 << 24) | ((process_register::TRANSITION_POINTER as u32) << 14) | 2;
        let stack_reference = (0x18_u32 << 24) | (0x1f << 14) | 1;
        let mut object = VmObject::new(
            h,
            vec![register_reference, transition_reference, stack_reference],
        )
        .unwrap();
        object.global_code = vec![0; 3];
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        machine.run(h, 3).unwrap();
        assert_eq!(
            machine.object(h).unwrap().register(3),
            Ok(encode_code_reference(CodeAddress {
                segment: CodeSegment::Global,
                pc: 2,
            }))
        );
        assert_eq!(machine.object(h).unwrap().transition_pc(), Some(2));
        assert_eq!(
            machine
                .object(h)
                .unwrap()
                .register(process_register::TRANSITION_POINTER),
            Ok(encode_code_reference(CodeAddress {
                segment: CodeSegment::Global,
                pc: 2,
            }))
        );
        assert_eq!(
            machine.object(h).unwrap().stack(),
            &[encode_code_reference(CodeAddress {
                segment: CodeSegment::Global,
                pc: 1,
            })]
        );
    }

    #[test]
    fn movc_rejects_a_global_code_offset_outside_item_one() {
        let h = handle(0);
        let mut object = VmObject::new(h, vec![(0x18_u32 << 24) | 3]).unwrap();
        object.global_code = vec![0; 3];
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        assert_eq!(
            machine.run(h, 1),
            Err(VmError::InvalidJump {
                object: h,
                target: 3,
            })
        );
    }

    #[test]
    fn nested_global_calls_restore_their_code_segments() {
        let h = handle(0);
        let mut object = VmObject::new(h, vec![(0x86_u32 << 24) | 2]).unwrap();
        object.global_code = vec![0; 9];
        object.global_code[2] = (0x86_u32 << 24) | 8;
        object.global_code[3] = control_flow(2, 0, 0, 0, 0);
        object.global_code[8] = control_flow(2, 0, 0, 0, 0);

        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.run(h, 3).unwrap();
        assert_eq!(
            machine.object(h).unwrap().code_address(),
            CodeAddress {
                segment: CodeSegment::Global,
                pc: 3,
            }
        );
        machine.run(h, 1).unwrap();
        assert_eq!(
            machine.object(h).unwrap().code_address(),
            CodeAddress {
                segment: CodeSegment::External,
                pc: 1,
            }
        );
    }

    #[test]
    fn global_return_discards_declared_call_arguments() {
        let h = handle(0);
        let call_with_two_arguments = (0x86_u32 << 24) | (2 << 20);
        let mut object = VmObject::new(
            h,
            vec![
                Instruction::encode(0x00, REG0, REG1),
                Instruction::encode(0x00, REG0, REG1),
                call_with_two_arguments,
            ],
        )
        .unwrap();
        object.set_register(0, 2).unwrap();
        object.set_register(1, 3).unwrap();
        object.global_code = vec![control_flow(2, 0, 0, 0, 0)];

        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.run(h, 4).unwrap();
        assert!(machine.object(h).unwrap().stack().is_empty());
        assert_eq!(
            machine.object(h).unwrap().code_address(),
            CodeAddress {
                segment: CodeSegment::External,
                pc: 3,
            }
        );
    }

    #[test]
    fn exact_crash_child_spawn_continues_after_pointer_free_host_request() {
        let h = handle(0);
        let make_object = || {
            let mut object = VmObject::new(
                h,
                vec![
                    Instruction::encode(0x00, REG0, REG1),
                    0x8a10_5001,
                    Instruction::encode(0x00, REG0, REG1),
                ],
            )
            .unwrap();
            object.set_register(0, 0).unwrap();
            object.set_register(1, 0).unwrap();
            object
        };
        let mut machine = Machine::new(0);
        machine.insert_object(make_object()).unwrap();

        assert_eq!(
            machine.run(h, 3).unwrap(),
            Execution {
                reason: HaltReason::HostEffect,
                steps: 2,
            }
        );
        assert!(machine.object(h).unwrap().stack().is_empty());
        assert_eq!(machine.object(h).unwrap().code_address().pc, 2);
        assert_eq!(
            machine.effects(),
            &[VmEffect::SpawnChildren {
                parent: h,
                executable: 5,
                subtype: 0,
                count: 1,
                allow_reclaim: false,
                arguments: vec![0],
                argument_pool_slots: vec![None],
            }]
        );

        let mut hosted = Machine::new(0);
        hosted.insert_object(make_object()).unwrap();
        let mut callback_count = 0;
        assert_eq!(
            hosted
                .run_with_host_effects(h, 3, |machine, effect| {
                    assert!(matches!(effect, VmEffect::SpawnChildren { .. }));
                    callback_count += 1;
                    machine.object_mut(h)?.set_register(0, 7)?;
                    Ok(())
                })
                .unwrap(),
            Execution {
                reason: HaltReason::BudgetExhausted,
                steps: 3,
            }
        );
        assert_eq!(callback_count, 1);
        assert_eq!(hosted.object(h).unwrap().stack(), &[7]);
    }

    #[test]
    fn negative_dynamic_child_count_pops_arguments_without_spawning() {
        let h = handle(0);
        let object = VmObject::new(h, vec![0x8a20_5000]).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.push(h, 0x1234).unwrap();
        machine.push(h, u32::MAX).unwrap();

        assert_eq!(
            machine.run(h, 1).unwrap(),
            Execution {
                reason: HaltReason::BudgetExhausted,
                steps: 1,
            }
        );
        assert!(machine.object(h).unwrap().stack().is_empty());
        assert!(machine.effects().is_empty());
    }

    #[test]
    fn audio_voice_create_translates_a_then_b_and_writes_the_synchronous_result() {
        let h = handle(0);
        let adio = Eid::from_name("audio").unwrap();
        let object = VmObject::new(h, vec![Instruction::encode(0x8c, STACK, STACK)]).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.push(h, adio.raw()).unwrap();
        machine.push(h, (-0x1234_i32) as u32).unwrap();

        assert_eq!(
            machine.run(h, 1).unwrap(),
            Execution {
                reason: HaltReason::HostEffect,
                steps: 1,
            }
        );
        assert!(machine.object(h).unwrap().stack().is_empty());
        let request = AudioVoiceCreateRequest {
            object: h,
            volume_source: StorageReference::checked(
                h,
                StorageRegion::Register,
                SYNTHETIC_STACK_POINTER + 1,
            )
            .unwrap(),
            volume: -0x1234,
            adio_source: StorageReference::checked(
                h,
                StorageRegion::Register,
                SYNTHETIC_STACK_POINTER,
            )
            .unwrap(),
            adio,
        };
        assert_eq!(
            machine.pending_audio_host_request(),
            Some(AudioHostRequest::CreateVoice(request))
        );
        assert_eq!(
            machine.run(h, 1).unwrap(),
            Execution {
                reason: HaltReason::HostEffect,
                steps: 0,
            },
            "an unresolved native audio call cannot execute the next word"
        );
        assert_eq!(
            machine.complete_audio_host_request(AudioHostResponse::ControlApplied),
            Err(VmError::MismatchedAudioHostResponse)
        );
        assert_eq!(
            machine.pending_audio_host_request(),
            Some(AudioHostRequest::CreateVoice(request))
        );

        machine
            .complete_audio_host_request(AudioHostResponse::VoiceCreated { voice_id: -17 })
            .unwrap();
        assert_eq!(machine.pending_audio_host_request(), None);
        assert_eq!(
            machine
                .object(h)
                .unwrap()
                .register(process_register::VOICE_ID),
            Ok((-17_i32) as u32)
        );
        assert_eq!(
            machine.effects(),
            &[VmEffect::AudioStart {
                object: h,
                voice: (-0x1234_i32) as u32,
                sound: adio.raw(),
            }]
        );
    }

    #[test]
    fn typed_audio_request_escapes_the_legacy_effect_runner_without_advancing() {
        let h = handle(0);
        let adio = Eid::from_name("audio").unwrap();
        let mut object = VmObject::new(h, vec![Instruction::encode(0x8c, REG1, REG2)]).unwrap();
        object.set_register(1, 0x3fff).unwrap();
        object.set_register(2, adio.raw()).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        assert_eq!(
            machine
                .run_with_host_effects(h, 1, |_machine, _effect| {
                    panic!("typed audio requests do not use the legacy effect callback")
                })
                .unwrap(),
            Execution {
                reason: HaltReason::HostEffect,
                steps: 1,
            }
        );
        assert!(matches!(
            machine.pending_audio_host_request(),
            Some(AudioHostRequest::CreateVoice(_))
        ));
        assert_eq!(
            machine.run(h, 1).unwrap(),
            Execution {
                reason: HaltReason::HostEffect,
                steps: 0,
            }
        );
    }

    #[test]
    fn audio_control_decodes_every_operation_and_generic_argument_width() {
        let target = handle(1);
        for suboperation in 0_u8..16 {
            let h = handle(0);
            let operand = if suboperation == 5 { 0x0e03 } else { 0 };
            let mut object =
                VmObject::new(h, vec![audio_control(suboperation, 0, 0, operand)]).unwrap();
            object.set_internal(0, 0xffff_ff80).unwrap();
            object.set_internal(1, 2).unwrap();
            object.set_internal(2, 3).unwrap();
            object.set_link(3, Some(target)).unwrap();
            let mut machine = Machine::new(0);
            machine.insert_object(object).unwrap();
            machine
                .insert_object(VmObject::new(target, Vec::new()).unwrap())
                .unwrap();

            assert_eq!(machine.run(h, 1).unwrap().reason, HaltReason::HostEffect);
            let Some(AudioHostRequest::Control(request)) = machine.pending_audio_host_request()
            else {
                panic!("0x8d must expose a typed control request");
            };
            assert_eq!(request.operation.suboperation, suboperation);
            assert_eq!(
                request.operation.effective_suboperation(),
                suboperation % 15
            );
            assert_eq!(request.voice, AudioVoiceSelector::Template);
            let expected = match request.operation.effective_suboperation() {
                0 | 1 | 6 => AudioControlArgument::Scalar(AudioScalarArgument::Signed(-128)),
                2 | 3 => AudioControlArgument::Vector([-128, 2, 3]),
                4 | 12 => AudioControlArgument::Scalar(AudioScalarArgument::SignedByte(-128)),
                5 => AudioControlArgument::Object(Some(target)),
                7 | 10 | 11 => {
                    AudioControlArgument::Scalar(AudioScalarArgument::Unsigned(0xffff_ff80))
                }
                8 | 9 | 13 | 14 => AudioControlArgument::Unused,
                _ => unreachable!(),
            };
            assert_eq!(
                request.argument, expected,
                "raw suboperation {suboperation}"
            );
            machine
                .complete_audio_host_request(AudioHostResponse::ControlApplied)
                .unwrap();
        }
    }

    #[test]
    fn audio_control_decodes_all_flag_bits_and_force_off_mapping() {
        for suboperation in 0_u8..16 {
            for packed_flags in 0_u8..4 {
                let operation = AudioControlOperation::decode(audio_control(
                    suboperation,
                    packed_flags,
                    0x3f,
                    0x0fff,
                ));
                assert_eq!(operation.suboperation, suboperation);
                assert_eq!(operation.flags.force_off, suboperation == 15);
                assert_eq!(operation.flags.stop_after_ramp, packed_flags & 1 != 0);
                assert_eq!(operation.flags.ramp_or_glide, packed_flags & 2 != 0);
                assert_eq!(operation.effective_suboperation(), suboperation % 15);
                let expected = (if suboperation == 15 { 0x8000_0000 } else { 0 })
                    | (if packed_flags & 1 != 0 {
                        0x4000_0000
                    } else {
                        0
                    })
                    | (if packed_flags & 2 != 0 {
                        0x2000_0000
                    } else {
                        0
                    })
                    | u32::from(suboperation % 15);
                assert_eq!(operation.native_control_word(), expected);
            }
        }
    }

    #[test]
    fn audio_control_translates_b_before_popping_the_stack_voice_selector() {
        let h = handle(0);
        let object = VmObject::new(h, vec![audio_control(15, 3, 0x1f, STACK)]).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.push(h, (-7_i32) as u32).unwrap();
        machine.push(h, (-123_i32) as u32).unwrap();

        assert_eq!(machine.run(h, 1).unwrap().reason, HaltReason::HostEffect);
        assert!(machine.object(h).unwrap().stack().is_empty());
        let Some(AudioHostRequest::Control(request)) = machine.pending_audio_host_request() else {
            panic!("0x8d must expose a typed control request");
        };
        assert_eq!(request.voice, AudioVoiceSelector::Stack { voice_id: -7 });
        assert_eq!(request.voice.voice_id(), -7);
        assert_eq!(
            request.argument,
            AudioControlArgument::Scalar(AudioScalarArgument::Signed(-123))
        );
        assert_eq!(
            request.argument_source,
            Some(
                StorageReference::checked(h, StorageRegion::Register, SYNTHETIC_STACK_POINTER + 1,)
                    .unwrap()
            )
        );
        assert_eq!(request.operation.native_control_word(), 0xe000_0000);
    }

    #[test]
    fn audio_control_distinguishes_template_and_process_register_voice_selectors() {
        let h = handle(0);
        let mut object = VmObject::new(
            h,
            vec![
                audio_control(8, 0, 0, 0x0be0),
                audio_control(9, 0, process_register::VOICE_ID as u8, 0x0be0),
            ],
        )
        .unwrap();
        object
            .set_register(process_register::VOICE_ID, (-21_i32) as u32)
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        assert_eq!(machine.run(h, 1).unwrap().reason, HaltReason::HostEffect);
        let Some(AudioHostRequest::Control(template)) = machine.pending_audio_host_request() else {
            panic!("expected template request");
        };
        assert_eq!(template.voice, AudioVoiceSelector::Template);
        assert_eq!(template.argument_source, None);
        assert_eq!(template.argument, AudioControlArgument::Unused);
        machine
            .complete_audio_host_request(AudioHostResponse::ControlApplied)
            .unwrap();

        assert_eq!(machine.run(h, 1).unwrap().reason, HaltReason::HostEffect);
        let Some(AudioHostRequest::Control(process)) = machine.pending_audio_host_request() else {
            panic!("expected process-register request");
        };
        assert_eq!(
            process.voice,
            AudioVoiceSelector::ProcessRegister {
                register: process_register::VOICE_ID as u8,
                voice_id: -21,
            }
        );
    }

    #[test]
    fn events_audio_and_misc_transitions_are_effects() {
        let a = handle(0);
        let b = handle(1);
        let adio = Eid::from_name("audio").unwrap();
        let code = vec![
            Instruction::encode(0x87, 3 << 9, REG0),
            Instruction::encode(0x8c, REG1, REG2),
            misc(12, 9, REG0),
        ];
        let mut object = VmObject::new(a, code).unwrap();
        object.set_link(3, Some(b)).unwrap();
        object.set_register(0, 0x900).unwrap();
        object.set_register(1, 9).unwrap();
        object.set_register(2, adio.raw()).unwrap();
        let mut machine = Machine::new(1);
        machine.insert_object(object).unwrap();
        machine
            .insert_object(VmObject::new(b, vec![Instruction::encode(0x82, 0, 0)]).unwrap())
            .unwrap();
        assert_eq!(
            machine.run(a, 3).unwrap(),
            Execution {
                reason: HaltReason::HostEffect,
                steps: 1,
            }
        );
        assert_eq!(
            machine.run(a, 2).unwrap(),
            Execution {
                reason: HaltReason::HostEffect,
                steps: 0,
            }
        );
        let mut delivered = None;
        assert_eq!(
            machine
                .run_with_host_requests(a, 0, |_machine, request| {
                    let VmHostRequest::SendEvent(request) = request else {
                        return Err(VmError::MissingHostEffect);
                    };
                    delivered = Some(request);
                    Ok(())
                })
                .unwrap(),
            Execution {
                reason: HaltReason::BudgetExhausted,
                steps: 0,
            }
        );
        let delivered = delivered.expect("typed event request was serviced");
        assert_eq!(delivered.sender, a);
        assert_eq!(delivered.target, SendEventTarget::Direct { recipient: b });
        assert_eq!(delivered.event, 0x900);
        assert!(delivered.arguments().is_empty());
        assert_eq!(machine.run(a, 1).unwrap().reason, HaltReason::HostEffect);
        machine
            .complete_audio_host_request(AudioHostResponse::VoiceCreated { voice_id: 41 })
            .unwrap();
        assert_eq!(
            machine.run(a, 1).unwrap().reason,
            HaltReason::BudgetExhausted
        );
        assert!(machine.effects().contains(&VmEffect::SendEvent(delivered)));
        assert!(machine.effects().contains(&VmEffect::AudioStart {
            object: a,
            voice: 9,
            sound: adio.raw(),
        }));
        assert!(machine.effects().contains(&VmEffect::Transition(9)));
        assert_eq!(
            machine
                .object(a)
                .unwrap()
                .register(process_register::VOICE_ID),
            Ok(41)
        );
    }

    #[test]
    fn exact_crash_8784080f_reuses_its_one_argument_without_stack_growth() {
        const REPETITIONS: usize = 300;

        let sender = handle(0);
        let recipient = handle(1);
        let mut code = Vec::with_capacity(REPETITIONS * 2);
        for _ in 0..REPETITIONS {
            // Exact Crash pair from the legal corpus: push 0x400, then send
            // event 0xf00 through link four with argc one and condition link0.
            code.extend([0x16be_0804, 0x8784_080f]);
        }
        let mut object = VmObject::new(sender, code).unwrap();
        object.set_link(4, Some(recipient)).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine
            .insert_object(VmObject::new(recipient, vec![0]).unwrap())
            .unwrap();
        let mut deliveries = 0;

        assert_eq!(
            machine
                .run_with_host_requests(sender, REPETITIONS * 2, |machine, request| {
                    let VmHostRequest::SendEvent(request) = request else {
                        return Err(VmError::MissingHostEffect);
                    };
                    assert_eq!(request.sender, sender);
                    assert_eq!(request.target, SendEventTarget::Direct { recipient });
                    assert_eq!(request.event, 0x0f00);
                    assert_eq!(request.arguments(), &[0x400]);
                    assert_eq!(machine.object(sender)?.stack(), &[0x400]);
                    assert_eq!(
                        machine.take_effects(),
                        [VmEffect::SendEvent(request)],
                        "drain the bounded observable queue between repetitions"
                    );
                    deliveries += 1;
                    Ok(())
                })
                .unwrap(),
            Execution {
                reason: HaltReason::BudgetExhausted,
                steps: REPETITIONS * 2,
            }
        );
        assert_eq!(deliveries, REPETITIONS);
        assert!(machine.object(sender).unwrap().stack().is_empty());
        assert!(machine.pending_send_events.is_empty());
    }

    #[test]
    fn send_and_spawn_owned_arguments_capture_live_physical_pool_slots() {
        let target = handle(0);
        let sender = handle(1);
        let recipient = handle(2);
        let pointer = CollisionObjectReference::new(target).to_word();

        let mut sender_object = VmObject::new(
            sender,
            vec![send_event_instruction(0x87, 0x1f, 1, 1, 0x080f)],
        )
        .unwrap();
        sender_object.set_link(1, Some(recipient)).unwrap();
        let mut send_machine = Machine::new(0);
        send_machine
            .insert_object(VmObject::new(target, vec![0]).unwrap())
            .unwrap();
        send_machine.bind_retail_pool_slot(target, 5).unwrap();
        send_machine.insert_object(sender_object).unwrap();
        send_machine
            .insert_object(VmObject::new(recipient, vec![0]).unwrap())
            .unwrap();
        // Deliberately push without an attached sidecar. The owned host
        // request must enrich the live compact token at the capture boundary.
        send_machine.push(sender, pointer).unwrap();
        send_machine.push(sender, 1).unwrap();
        let mut captured_send = false;
        send_machine
            .run_with_host_requests(sender, 1, |_machine, request| {
                let VmHostRequest::SendEvent(request) = request else {
                    return Err(VmError::MissingHostEffect);
                };
                assert_eq!(request.arguments(), &[pointer]);
                assert_eq!(request.argument_pool_slots(), &[Some(5)]);
                captured_send = true;
                Ok(())
            })
            .unwrap();
        assert!(captured_send);

        let spawner = handle(1);
        let mut spawn_machine = Machine::new(0);
        spawn_machine
            .insert_object(VmObject::new(target, vec![0]).unwrap())
            .unwrap();
        spawn_machine.bind_retail_pool_slot(target, 5).unwrap();
        spawn_machine
            .insert_object(VmObject::new(spawner, vec![0x8a10_5001]).unwrap())
            .unwrap();
        spawn_machine.push(spawner, pointer).unwrap();
        assert_eq!(
            spawn_machine.run(spawner, 1).unwrap().reason,
            HaltReason::HostEffect
        );
        assert!(matches!(
            spawn_machine.effects().last(),
            Some(VmEffect::SpawnChildren {
                arguments,
                argument_pool_slots,
                ..
            }) if arguments == &[pointer] && argument_pool_slots == &[Some(5)]
        ));
    }

    #[test]
    fn send_event_condition_pop_precedes_ordered_argv_and_link_selectors_are_typed() {
        let sender = handle(0);
        let recipient = handle(1);
        let mut object = VmObject::new(
            sender,
            vec![send_event_instruction(0x87, 0x1f, 3, 1, 0x080f)],
        )
        .unwrap();
        object.set_link(1, Some(recipient)).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine
            .insert_object(VmObject::new(recipient, vec![0]).unwrap())
            .unwrap();
        for word in [0x11, 0x22, 0x33, 1] {
            machine.push(sender, word).unwrap();
        }
        let mut delivered = false;

        machine
            .run_with_host_requests(sender, 1, |machine, request| {
                let VmHostRequest::SendEvent(request) = request else {
                    return Err(VmError::MissingHostEffect);
                };
                assert_eq!(request.arguments(), &[0x11, 0x22, 0x33]);
                assert_eq!(machine.object(sender)?.stack(), &[0x11, 0x22, 0x33]);
                delivered = true;
                Ok(())
            })
            .unwrap();
        assert!(delivered);
        assert!(machine.object(sender).unwrap().stack().is_empty());

        for (selector, link, expected) in [
            (0, None, true),
            (6, Some(recipient), true),
            (6, None, false),
            (7, Some(recipient), true),
            (7, None, false),
        ] {
            let mut object = VmObject::new(
                sender,
                vec![send_event_instruction(0x87, selector, 0, 1, 0x080f)],
            )
            .unwrap();
            object.set_link(1, Some(recipient)).unwrap();
            if selector != 0 {
                object.set_link(usize::from(selector), link).unwrap();
            }
            let mut machine = Machine::new(0);
            machine.insert_object(object).unwrap();
            machine
                .insert_object(VmObject::new(recipient, vec![0]).unwrap())
                .unwrap();
            let mut count = 0;
            machine
                .run_with_host_requests(sender, 1, |_machine, request| {
                    assert!(matches!(request, VmHostRequest::SendEvent(_)));
                    count += 1;
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                count,
                usize::from(expected),
                "condition selector {selector}"
            );
        }
    }

    #[test]
    fn skipped_send_event_paths_clear_misc_and_drop_argv() {
        let sender = handle(0);
        let recipient = handle(1);
        for (condition, event_operand, linked) in
            [(8, 0x080f, true), (0, 0x0be0, true), (0, 0x080f, false)]
        {
            let mut object = VmObject::new(
                sender,
                vec![send_event_instruction(0x87, condition, 1, 1, event_operand)],
            )
            .unwrap();
            object.set_link(1, linked.then_some(recipient)).unwrap();
            object
                .set_register(process_register::MISC_VALUE, 0xdead_beef)
                .unwrap();
            let mut machine = Machine::new(0);
            machine.insert_object(object).unwrap();
            machine
                .insert_object(VmObject::new(recipient, vec![0]).unwrap())
                .unwrap();
            machine.push(sender, 0x1234).unwrap();

            assert_eq!(
                machine.run_with_host_requests(sender, 1, |_machine, _request| {
                    Err(VmError::MissingHostEffect)
                }),
                Ok(Execution {
                    reason: HaltReason::BudgetExhausted,
                    steps: 1,
                })
            );
            let object = machine.object(sender).unwrap();
            assert!(object.stack().is_empty());
            assert_eq!(object.register(process_register::MISC_VALUE), Ok(0));
        }

        let mut object = VmObject::new(
            sender,
            vec![send_event_instruction(0x87, 0x1f, 1, 1, 0x080f)],
        )
        .unwrap();
        object.set_link(1, Some(recipient)).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine
            .insert_object(VmObject::new(recipient, vec![0]).unwrap())
            .unwrap();
        machine.push(sender, 0x1234).unwrap();
        machine.push(sender, 0).unwrap();
        machine
            .run_with_host_requests(sender, 1, |_machine, _request| {
                Err(VmError::MissingHostEffect)
            })
            .unwrap();
        assert!(machine.object(sender).unwrap().stack().is_empty());
    }

    #[test]
    fn keep_rebind_pops_only_normal_code_and_preserves_every_retlk_context() {
        let sender = handle(0);
        let recipient = handle(1);
        for return_link_halt in [
            None,
            Some(HaltReason::OnceCompleted),
            Some(HaltReason::TransitionCompleted),
            Some(HaltReason::InterruptCompleted),
        ] {
            let mut object =
                VmObject::new(sender, vec![send_event_instruction(0x87, 0, 1, 1, 0x080f)]).unwrap();
            object.set_link(1, Some(recipient)).unwrap();
            let mut machine = Machine::new(0);
            machine.insert_object(object).unwrap();
            machine
                .insert_object(VmObject::new(recipient, vec![0]).unwrap())
                .unwrap();
            machine.push(sender, 0xaaaa).unwrap();
            let mut rebound_len = 0;

            let execution = machine
                .run_with_host_requests_mode(
                    sender,
                    1,
                    |machine, request| {
                        assert!(matches!(request, VmHostRequest::SendEvent(_)));
                        let object = machine.object_mut(sender)?;
                        object.initialize_arguments(&[0x1111, 0x2222])?;
                        let status_a = object.register(process_register::STATUS_A)?;
                        object.set_register(
                            process_register::STATUS_A,
                            status_a | STATUS_A_KEEP_EVENT_STACK,
                        )?;
                        rebound_len = object.stack().len();
                        Ok(())
                    },
                    HostRunOptions {
                        suspend_on_animation: true,
                        apply_animation_gate: true,
                        service_audio: true,
                        return_link_halt,
                    },
                )
                .unwrap();

            if let Some(reason) = return_link_halt {
                assert_eq!(execution.reason, reason);
                assert_eq!(machine.object(sender).unwrap().stack().len(), rebound_len);
            } else {
                assert_eq!(execution.reason, HaltReason::BudgetExhausted);
                assert_eq!(
                    machine.object(sender).unwrap().stack().len(),
                    rebound_len - 1
                );
                assert_eq!(machine.object(sender).unwrap().animation_wait, None);
            }
        }
    }

    #[test]
    fn native_misc_twelve_relocates_save_load_and_neighbor_termination_effects() {
        let h = handle(0);
        let object = VmObject::new(h, vec![misc(12, 0, 0x0be0), misc(12, 1, 0x0be0)]).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        assert_eq!(
            machine
                .run_with_host_effects(h, 2, |machine, effect| {
                    if matches!(effect, VmEffect::LoadState { .. }) {
                        machine.request_level_restart();
                    }
                    Ok(())
                })
                .unwrap(),
            Execution {
                reason: HaltReason::HostEffect,
                steps: 2,
            }
        );
        assert_eq!(
            machine.effects(),
            &[
                VmEffect::SaveState(h),
                VmEffect::LoadState {
                    object: h,
                    saved_level: None,
                },
            ]
        );
        machine.clear_level_restart_request();

        let requester = handle(1);
        machine
            .insert_object(
                VmObject::new(requester, vec![misc(12, 7, STACK), misc(12, 5, 0x0be0)]).unwrap(),
            )
            .unwrap();
        machine.push(requester, 0xfeed_beef).unwrap();
        let mut host_effect_count = 0;
        assert_eq!(
            machine
                .run_with_host_effects(requester, 2, |machine, effect| {
                    host_effect_count += 1;
                    assert_eq!(
                        effect,
                        &VmEffect::TerminateCurrentZoneNeighbors { requester }
                    );
                    assert!(machine.object(requester)?.stack().is_empty());
                    assert_eq!(machine.effects().last(), Some(effect));
                    Ok(())
                })
                .unwrap(),
            Execution {
                reason: HaltReason::BudgetExhausted,
                steps: 2,
            }
        );
        assert_eq!(host_effect_count, 1);
        assert_eq!(
            &machine.effects()[2..],
            &[
                VmEffect::TerminateCurrentZoneNeighbors { requester },
                VmEffect::ResetMasterFadeStep { object: requester },
            ]
        );
    }

    #[test]
    fn host_termination_and_same_slot_reuse_cannot_resume_the_replacement() {
        let requester = handle(0);
        let mut machine = Machine::new(0);
        machine
            .insert_object(
                VmObject::new(requester, vec![misc(12, 7, 0x0be0), misc(12, 5, 0x0be0)]).unwrap(),
            )
            .unwrap();

        assert_eq!(
            machine
                .run_with_host_effects(requester, 2, |machine, effect| {
                    assert_eq!(
                        effect,
                        &VmEffect::TerminateCurrentZoneNeighbors { requester }
                    );
                    machine.remove_object_for_host_termination(requester)?;
                    let mut replacement =
                        VmObject::new(requester, vec![Instruction::encode(0x11, 0x0807, 0x0e0a)])?;
                    replacement.set_register(10, 0xfeed_beef)?;
                    machine.insert_object(replacement)?;
                    Ok(())
                })
                .unwrap(),
            Execution {
                reason: HaltReason::ObjectTerminated,
                steps: 1,
            }
        );
        let replacement = machine.object(requester).unwrap();
        assert_eq!(replacement.pc(), 0);
        assert_eq!(replacement.register(10), Ok(0xfeed_beef));
        assert_eq!(
            machine.effects(),
            &[VmEffect::TerminateCurrentZoneNeighbors { requester }]
        );
    }

    #[test]
    fn synchronous_event_same_slot_reuse_cannot_unwind_or_resume_the_replacement() {
        const EVENT: u32 = 0x1500;

        let requester = handle(0);
        let mut object = VmObject::new(requester, vec![0]).unwrap();
        object
            .configure_test_event_interrupt(
                EVENT,
                vec![
                    misc(12, 7, 0x0be0),
                    Instruction::encode(0x11, 0x0807, 0x0e0a),
                    0x8280_0000,
                ],
            )
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        let outcome = machine
            .send_event_with_host_requests(
                None,
                Some(requester),
                EVENT,
                None,
                |machine, request| {
                    assert_eq!(
                        request,
                        VmHostRequest::Effect(VmEffect::TerminateCurrentZoneNeighbors {
                            requester
                        })
                    );
                    machine.remove_object_for_host_termination(requester)?;
                    let mut replacement =
                        VmObject::new(requester, vec![Instruction::encode(0x11, 0x0809, 0x0e0a)])?;
                    replacement.set_register(10, 0xcafe_babe)?;
                    machine.insert_object(replacement)
                },
            )
            .unwrap();

        assert_eq!(
            outcome,
            EventDispatchOutcome {
                acknowledged: true,
                state_change: None,
            }
        );
        let replacement = machine.object(requester).unwrap();
        assert_eq!(replacement.pc(), 0);
        assert_eq!(replacement.register(10), Ok(0xcafe_babe));
    }

    #[test]
    fn native_misc_master_fade_and_noops_translate_and_pop_gop_b_once() {
        let object = handle(0);
        let vm_object = VmObject::new(
            object,
            vec![misc(12, 3, STACK), misc(12, 5, STACK), misc(12, 10, STACK)],
        )
        .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(vm_object).unwrap();
        machine.push(object, 0x1111_1111).unwrap();
        machine.push(object, 0x2222_2222).unwrap();
        machine.push(object, 0x3333_3333).unwrap();

        assert_eq!(
            machine.run(object, 3).unwrap(),
            Execution {
                reason: HaltReason::BudgetExhausted,
                steps: 3,
            }
        );
        assert!(machine.object(object).unwrap().stack().is_empty());
        assert_eq!(
            machine.effects(),
            &[VmEffect::ResetMasterFadeStep { object }]
        );
    }

    #[test]
    fn native_misc_unknown_suboperation_is_translation_only_noop() {
        let object = handle(0);
        let vm_object =
            VmObject::new(object, vec![misc(12, -1, STACK), misc(12, 5, STACK)]).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(vm_object).unwrap();
        machine.push(object, 0x1111_1111).unwrap();
        machine.push(object, 0x2222_2222).unwrap();

        assert_eq!(
            machine.run(object, 2).unwrap(),
            Execution {
                reason: HaltReason::BudgetExhausted,
                steps: 2,
            }
        );
        assert!(machine.object(object).unwrap().stack().is_empty());
        assert_eq!(
            machine.effects(),
            &[VmEffect::ResetMasterFadeStep { object }]
        );
    }

    #[test]
    fn native_misc_level_reset_is_synchronous_exact_and_preserves_active_spawns() {
        let object = handle(0);
        let vm_object =
            VmObject::new(object, vec![misc(12, 11, STACK), misc(12, 5, STACK)]).unwrap();
        let mut machine = Machine::new(119);
        machine.insert_object(vm_object).unwrap();
        machine.push(object, 0x1111_1111).unwrap();
        machine.push(object, 0x2222_2222).unwrap();
        machine
            .set_global_word(INITIAL_LIFE_COUNT_GLOBAL, 7 << 8)
            .unwrap();
        for index in [
            5, 20, 24, 25, 26, 27, 28, 29, 46, 47, 63, 67, 69, 72, 100, 101, 108, 113,
        ] {
            machine.set_global_word(index, 0xdead_beef).unwrap();
        }
        machine.set_global_word(GAME_STATE_GLOBAL, 0x600).unwrap();
        machine.set_spawn_flags(42, 0xab).unwrap();
        machine.set_retail_level_spawn_tag(0, 0x1234);

        assert_eq!(
            machine
                .run_with_host_effects(object, 2, |machine, effect| {
                    if matches!(effect, VmEffect::ResetLevelGlobals { .. }) {
                        machine.reset_retail_level_globals()?;
                    }
                    Ok(())
                })
                .unwrap(),
            Execution {
                reason: HaltReason::BudgetExhausted,
                steps: 2,
            }
        );

        assert!(machine.object(object).unwrap().stack().is_empty());
        for (index, expected) in [
            (69, u32::MAX),
            (108, 0),
            (5, 0),
            (25, 0),
            (26, 0),
            (27, 0),
            (28, 0),
            (29, 0),
            (47, 1),
            (63, 0),
            (72, 0),
            (67, 1),
            (20, 99),
            (46, 1),
            (100, 0),
            (101, 0),
            (113, 1),
            (LIFE_COUNT_GLOBAL, 7 << 8),
        ] {
            assert_eq!(machine.global_word(index), Ok(expected), "global {index}");
        }
        assert_eq!(machine.global_word(GAME_STATE_GLOBAL), Ok(0x600));
        assert_eq!(machine.spawn_flags(42), Ok(0xab));
        assert!(
            machine
                .retail_level_spawn_tags()
                .iter()
                .all(|tag| *tag == 0)
        );
        assert_eq!(
            machine.effects(),
            &[
                VmEffect::ResetLevelGlobals { object },
                VmEffect::ResetMasterFadeStep { object },
            ]
        );
    }

    #[test]
    fn native_misc_link_register_load_and_store_use_checked_links() {
        let source = handle(0);
        let target = handle(1);
        let mut source_object = VmObject::new(
            source,
            vec![misc(4, 0, REG0) | (3 << 12), misc(3, 0, REG0) | (3 << 12)],
        )
        .unwrap();
        source_object.set_link(3, Some(target)).unwrap();
        source_object.set_register(0, 5 << 8).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(source_object).unwrap();
        machine
            .insert_object(VmObject::new(target, Vec::new()).unwrap())
            .unwrap();
        machine.push(source, 0x1234_5678).unwrap();

        assert_eq!(
            machine.run(source, 2).unwrap(),
            Execution {
                reason: HaltReason::BudgetExhausted,
                steps: 2,
            }
        );
        assert_eq!(machine.object(target).unwrap().register(5), Ok(0x1234_5678));
        assert_eq!(machine.object(source).unwrap().stack(), &[0x1234_5678]);
    }

    #[test]
    fn options_null_interrupter_bootstrap_is_instruction_exact() {
        let options = handle(0);
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(options, vec![OPTIONS_NULL_INTERRUPTER_LOAD]).unwrap())
            .unwrap();

        assert_eq!(machine.run(options, 1).unwrap().steps, 1);
        assert_eq!(machine.object(options).unwrap().stack(), &[0]);

        for (object, instruction) in [
            (handle(1), misc(3, 0, 0x0841) | (7 << 12)),
            (handle(2), misc(4, 0, 0x0840) | (7 << 12)),
        ] {
            let mut candidate = VmObject::new(object, vec![instruction]).unwrap();
            candidate.set_register(0, 0x1234_5600).unwrap();
            let mut checked = Machine::new(0);
            checked.insert_object(candidate).unwrap();
            checked.push(object, 0x1234_5678).unwrap();
            assert_eq!(
                checked.run(object, 1),
                Err(VmError::MissingLink { object, link: 7 })
            );
        }
    }

    #[test]
    fn native_misc_szon_translates_optional_point_and_checked_link() {
        let source = handle(0);
        let target = handle(1);
        let mut source_object = VmObject::new(
            source,
            vec![misc(9, 0, REG0) | (3 << 12), misc(9, 0, 0x0be0) | (3 << 12)],
        )
        .unwrap();
        source_object.set_link(3, Some(target)).unwrap();
        for (register, value) in [i32::MIN, -1, i32::MAX].into_iter().enumerate() {
            source_object
                .set_register(register, value.cast_unsigned())
                .unwrap();
        }
        let mut machine = Machine::new(0);
        machine.insert_object(source_object).unwrap();
        machine
            .insert_object(VmObject::new(target, Vec::new()).unwrap())
            .unwrap();

        assert_eq!(
            machine
                .run_with_host_effects(source, 2, |_machine, _effect| Ok(()))
                .unwrap(),
            Execution {
                reason: HaltReason::BudgetExhausted,
                steps: 2,
            }
        );
        assert_eq!(
            machine.effects(),
            &[
                VmEffect::SetLinkZoneFromPoint {
                    requester: source,
                    target,
                    point: Some([i32::MIN, -1, i32::MAX]),
                },
                VmEffect::SetLinkZoneFromPoint {
                    requester: source,
                    target,
                    point: None,
                },
            ]
        );

        let missing = handle(2);
        machine
            .insert_object(VmObject::new(missing, vec![misc(9, 0, 0x0be0) | (2 << 12)]).unwrap())
            .unwrap();
        assert_eq!(
            machine.run(missing, 1),
            Err(VmError::MissingLink {
                object: missing,
                link: 2,
            })
        );
    }

    #[test]
    fn native_misc_distance_and_angle_operations_use_checked_vectors_and_links() {
        let source = handle(0);
        let target = handle(1);
        let vector_three = 0x0e00 | process_register::MISC_A_X as u16;
        let mut source_object = VmObject::new(
            source,
            vec![
                misc(1, 0, REG0) | (3 << 12),
                misc(1, 1, REG0) | (3 << 12),
                misc(1, 3, REG0) | (3 << 12),
                misc(2, 0, vector_three) | (3 << 12),
                misc(6, 1, vector_three) | (3 << 12),
                misc(5, 0, REG0) | (3 << 12),
            ],
        )
        .unwrap();
        source_object.set_link(3, Some(target)).unwrap();
        source_object.set_process_vector(0, [0, 0, 0]).unwrap();
        source_object.set_process_vector(3, [4_096, 0, 0]).unwrap();
        source_object
            .set_register(process_register::ROTATION_X, 0x100)
            .unwrap();
        let mut target_object = VmObject::new(target, Vec::new()).unwrap();
        target_object
            .set_process_vector(0, [3 * 256, 4 * 256, 12 * 256])
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(source_object).unwrap();
        machine.insert_object(target_object).unwrap();

        assert_eq!(machine.run(source, 6).unwrap().steps, 6);
        let linked = [3 * 256, 4 * 256, 12 * 256];
        let vector = [4_096, 0, 0];
        let point_angle = u32::from(
            Angle12::new(retail_atan2(vector[0] - linked[0], vector[2] - linked[2])).raw(),
        );
        let facing_angle =
            i32::from(Angle12::new(retail_atan2(linked[0], linked[2])).raw()) - 0x100;
        assert_eq!(
            machine.object(source).unwrap().stack(),
            &[3_520, 3_328, 3_072, point_angle, 4_608, facing_angle as u32]
        );
    }

    #[test]
    fn native_misc_vertical_angle_and_linked_point_bound_are_synchronous() {
        let source = handle(0);
        let target = handle(1);
        let vector_three = 0x0e00 | process_register::MISC_A_X as u16;
        let mut source_object = VmObject::new(
            source,
            vec![
                misc(12, 8, vector_three) | (3 << 12),
                misc(14, 0, vector_three) | (3 << 12),
            ],
        )
        .unwrap();
        source_object.set_link(3, Some(target)).unwrap();
        source_object.set_process_vector(3, [0, 0, 0]).unwrap();
        let mut target_object = VmObject::new(target, Vec::new()).unwrap();
        target_object
            .set_process_vector(0, [3 * 256, 5 * 256, 4 * 256])
            .unwrap();
        target_object.set_retail_local_bound(Bounds3 {
            min: Vec3 {
                x: -2_000,
                y: -2_000,
                z: -2_000,
            },
            max: Vec3 {
                x: 2_000,
                y: 2_000,
                z: 2_000,
            },
        });
        let mut machine = Machine::new(0);
        machine.insert_object(source_object).unwrap();
        machine.insert_object(target_object).unwrap();

        assert_eq!(machine.run(source, 2).unwrap().steps, 2);
        assert_eq!(machine.object(source).unwrap().stack(), &[0x200, 1]);
    }

    #[test]
    fn native_misc_spawn_reads_preserve_retail_bit_values() {
        let object = handle(0);
        let mut vm_object = VmObject::new(
            object,
            vec![misc(11, 1, REG0), misc(11, 2, REG0), misc(11, 3, REG0)],
        )
        .unwrap();
        vm_object.set_register(0, 17 << 8).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(vm_object).unwrap();
        machine.set_spawn_flags(17, 0b1100).unwrap();

        assert_eq!(machine.run(object, 3).unwrap().steps, 3);
        assert_eq!(machine.object(object).unwrap().stack(), &[1, 4, 8]);
    }

    #[test]
    fn native_misc_spawn_writes_are_synchronous_shared_table_effects() {
        let object = handle(0);
        let mut vm_object = VmObject::new(
            object,
            vec![misc(8, 0, REG0), misc(10, 1, REG0), misc(10, 8, REG0)],
        )
        .unwrap();
        vm_object.set_register(0, 17 << 8).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(vm_object).unwrap();
        machine.set_spawn_flags(17, 1).unwrap();
        let mut observed = Vec::new();

        assert_eq!(
            machine
                .run_with_host_effects(object, 3, |_machine, effect| {
                    observed.push(effect.clone());
                    Ok(())
                })
                .unwrap()
                .steps,
            3
        );
        assert_eq!(machine.spawn_flags(17), Ok(6));
        assert_eq!(
            observed,
            [
                VmEffect::SpawnFlagsChanged {
                    object,
                    id: 17,
                    flags: 3,
                },
                VmEffect::SpawnFlagsChanged {
                    object,
                    id: 17,
                    flags: 7,
                },
                VmEffect::SpawnFlagsChanged {
                    object,
                    id: 17,
                    flags: 6,
                },
            ]
        );
    }

    #[test]
    fn native_misc_spawn_encounter_registry_is_distinct_from_active_words() {
        let object = handle(0);
        let mut vm_object =
            VmObject::new(object, vec![misc(10, 5, REG0), misc(10, 4, REG0)]).unwrap();
        vm_object.set_register(0, 17 << 8).unwrap();
        let mut machine = Machine::new(119);
        machine.insert_object(vm_object).unwrap();
        machine
            .set_global_word(CURRENT_LEVEL_GLOBAL, 3 << 8)
            .unwrap();
        let expected_tag = u16::try_from((3 << 9) | 0x11).unwrap();
        let mut observed = Vec::new();

        machine
            .run_with_host_effects(object, 2, |machine, effect| {
                observed.push((effect.clone(), machine.retail_level_spawn_tags()[0]));
                Ok(())
            })
            .unwrap();

        assert_eq!(
            observed,
            [
                (
                    VmEffect::SpawnFlagsChanged {
                        object,
                        id: 17,
                        flags: 8,
                    },
                    expected_tag,
                ),
                (
                    VmEffect::SpawnFlagsChanged {
                        object,
                        id: 17,
                        flags: 0,
                    },
                    0,
                ),
            ]
        );
        assert_eq!(machine.spawn_flags(17), Ok(0));

        let restricted = handle(1);
        let mut restricted_object = VmObject::new(restricted, vec![misc(10, 5, REG0)]).unwrap();
        restricted_object.set_register(0, 17 << 8).unwrap();
        machine.insert_object(restricted_object).unwrap();
        machine.set_global_word(30, 0x2000).unwrap();
        machine
            .run_with_host_effects(restricted, 1, |_machine, _effect| Ok(()))
            .unwrap();
        assert_eq!(machine.spawn_flags(17), Ok(8));
        assert!(
            machine
                .retail_level_spawn_tags()
                .iter()
                .all(|tag| *tag == 0)
        );
    }

    #[test]
    fn native_misc_find_spawned_object_returns_a_checked_reference() {
        let requester = handle(0);
        let found = handle(1);
        let mut requester_object = VmObject::new(requester, vec![misc(7, 0, REG0)]).unwrap();
        requester_object.set_register(0, 33 << 8).unwrap();
        let mut found_object = VmObject::new(found, Vec::new()).unwrap();
        found_object
            .set_register(process_register::PID_FLAGS, 33 << 8)
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(requester_object).unwrap();
        machine.insert_object(found_object).unwrap();
        machine.set_spawn_flags(33, 1).unwrap();

        assert_eq!(
            machine
                .run_with_host_effects(requester, 1, |machine, effect| {
                    assert_eq!(
                        effect,
                        &VmEffect::FindSpawnedObject {
                            requester,
                            pid_flags: 33 << 8,
                        }
                    );
                    machine.complete_find_spawned_object(requester, Some(found))
                })
                .unwrap()
                .steps,
            1
        );
        assert_eq!(
            machine.object(requester).unwrap().stack(),
            &[CollisionObjectReference::new(found).to_word()]
        );
    }

    #[test]
    fn native_misc_find_nearest_preserves_link_mask_and_raw_event() {
        let requester = handle(0);
        let origin = handle(1);
        let instruction = misc(13, 0b1_1000, REG0) | (3 << 12);
        let mut requester_object = VmObject::new(requester, vec![instruction]).unwrap();
        requester_object.set_link(3, Some(origin)).unwrap();
        requester_object.set_register(0, HIT_EVENT).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(requester_object).unwrap();
        machine
            .insert_object(VmObject::new(origin, Vec::new()).unwrap())
            .unwrap();

        assert_eq!(
            machine
                .run_with_host_effects(requester, 1, |machine, effect| {
                    assert_eq!(
                        effect,
                        &VmEffect::FindNearestObject {
                            requester,
                            origin,
                            categories: 0b1_1000,
                            event: HIT_EVENT,
                        }
                    );
                    machine.complete_find_nearest_object(requester, None)
                })
                .unwrap()
                .steps,
            1
        );
        assert_eq!(machine.object(requester).unwrap().stack(), &[0]);
    }

    #[test]
    fn nearest_candidate_filters_category_and_uses_native_approximate_distance() {
        let origin = handle(0);
        let candidate = handle(1);
        let mut origin_object = VmObject::new(origin, Vec::new()).unwrap();
        origin_object.set_process_vector(0, [0; 3]).unwrap();
        let mut candidate_object = VmObject::new(candidate, Vec::new()).unwrap();
        candidate_object.configure_test_program_identity(0x300);
        candidate_object
            .set_process_vector(0, [100, 40, 20])
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(origin_object).unwrap();
        machine.insert_object(candidate_object).unwrap();

        assert_eq!(
            machine.classify_nearest_object_candidate(origin, candidate, 1 << 2, 0xff),
            Ok(NearestObjectCandidate::Ineligible)
        );
        assert_eq!(
            machine.classify_nearest_object_candidate(origin, candidate, 1 << 3, 0xff),
            Ok(NearestObjectCandidate::Eligible { distance: 115 })
        );
        assert_eq!(
            machine.classify_nearest_object_candidate(origin, origin, 1 << 3, 0xff),
            Ok(NearestObjectCandidate::Ineligible)
        );
    }

    #[test]
    fn nearest_candidate_event_contract_covers_fallback_interrupt_and_state_flags() {
        let origin = handle(0);
        let candidate = handle(1);
        let origin_object = VmObject::new(origin, Vec::new()).unwrap();
        let mut candidate_object = VmObject::new(candidate, Vec::new()).unwrap();
        candidate_object.configure_test_program_identity(0x300);
        candidate_object.set_process_vector(0, [16, 0, 0]).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(origin_object).unwrap();
        machine.insert_object(candidate_object).unwrap();

        let classify = |machine: &Machine, event| {
            machine
                .classify_nearest_object_candidate(origin, candidate, 1 << 3, event)
                .unwrap()
        };
        assert_eq!(
            classify(&machine, HIT_EVENT),
            NearestObjectCandidate::Eligible { distance: 16 }
        );
        machine
            .object_mut(candidate)
            .unwrap()
            .set_register(process_register::INVINCIBILITY_STATE, 2)
            .unwrap();
        assert_eq!(
            classify(&machine, HIT_EVENT),
            NearestObjectCandidate::Ineligible
        );
        machine
            .object_mut(candidate)
            .unwrap()
            .set_register(process_register::INVINCIBILITY_STATE, 0)
            .unwrap();
        machine
            .object_mut(candidate)
            .unwrap()
            .set_register(process_register::STATUS_C, 2)
            .unwrap();
        assert_eq!(
            classify(&machine, HIT_EVENT),
            NearestObjectCandidate::Ineligible
        );

        machine
            .object_mut(candidate)
            .unwrap()
            .set_register(process_register::STATE_FLAGS, 0x800)
            .unwrap();
        assert_eq!(
            classify(&machine, HIT_INVINCIBLE_EVENT),
            NearestObjectCandidate::Ineligible
        );
        assert_eq!(
            classify(&machine, WIN_BOSS_EVENT),
            NearestObjectCandidate::Eligible { distance: 16 }
        );

        {
            let object = machine.object_mut(candidate).unwrap();
            object.event_map.resize(0x10, EVENT_MAP_NULL_STATE);
            object.event_map[4] = 1;
            object.state_flags_by_index = vec![0, 2];
        }
        assert_eq!(
            classify(&machine, 0x0400),
            NearestObjectCandidate::Ineligible
        );
        machine
            .object_mut(candidate)
            .unwrap()
            .set_register(process_register::STATUS_C, 0)
            .unwrap();
        assert_eq!(
            classify(&machine, 0x0400),
            NearestObjectCandidate::Eligible { distance: 16 }
        );
        machine.object_mut(candidate).unwrap().state_flags_by_index[1] = 0x1000;
        machine
            .object_mut(candidate)
            .unwrap()
            .set_register(process_register::INVINCIBILITY_STATE, 2)
            .unwrap();
        assert_eq!(
            classify(&machine, 0x0400),
            NearestObjectCandidate::Ineligible
        );
        machine
            .object_mut(candidate)
            .unwrap()
            .set_register(process_register::INVINCIBILITY_STATE, 0)
            .unwrap();
        {
            let object = machine.object_mut(candidate).unwrap();
            object.event_map[4] = 0x8003;
            object.event_map[usize::try_from(STATUS_EVENT >> 8).unwrap()] = 0x8007;
        }
        assert_eq!(
            classify(&machine, 0x0400),
            NearestObjectCandidate::Eligible { distance: 16 }
        );
        assert_eq!(
            classify(&machine, STATUS_EVENT),
            NearestObjectCandidate::StatusInterrupt {
                distance: 16,
                offset: 7,
            }
        );
    }

    #[test]
    fn nearest_status_interrupt_sets_link_seven_and_supplies_0x100_argument() {
        let origin = handle(0);
        let candidate = handle(1);
        let mut candidate_object = VmObject::new(candidate, Vec::new()).unwrap();
        candidate_object.global_code = vec![
            Instruction::encode(0x11, 0x0b7f, 0x0e00 | process_register::ACK as u16),
            control_flow(2, 0, 0, 0, 0),
        ];
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(origin, Vec::new()).unwrap())
            .unwrap();
        machine.insert_object(candidate_object).unwrap();

        assert_eq!(
            machine.run_nearest_status_interrupt_with_host_requests(
                origin,
                candidate,
                0,
                |_machine, _request| unreachable!(),
            ),
            Ok(None)
        );
        let candidate_object = machine.object(candidate).unwrap();
        assert_eq!(candidate_object.links[7], Some(origin));
        assert_eq!(candidate_object.register(process_register::ACK), Ok(0x100));
        assert_eq!(candidate_object.register(process_register::EVENT), Ok(0));
        assert!(candidate_object.stack().is_empty());
    }

    #[test]
    fn transform_model_vertex_uses_a_synchronous_asset_effect() {
        let requester = handle(0);
        let link = handle(1);
        let model_eid = Eid::from_name("model").unwrap();
        let instruction = (0x85_u32 << 24) | (3 << 21) | (6 << 18) | (5 << 12) | u32::from(REG0);
        let mut requester_object = VmObject::new(requester, vec![instruction]).unwrap();
        requester_object.set_link(3, Some(link)).unwrap();
        requester_object.set_register(0, 2 << 8).unwrap();
        let mut link_object = VmObject::new(link, Vec::new()).unwrap();
        let mut animation = vec![1, 0, 1, 0];
        animation.extend_from_slice(&model_eid.raw().to_le_bytes());
        link_object.bind_animation_data(&animation);
        let reference = AnimationReference::checked(0, animation.len()).unwrap();
        link_object
            .set_register(process_register::ANIMATION_SEQUENCE, reference.to_word())
            .unwrap();
        link_object
            .set_register(process_register::ANIMATION_FRAME, 1 << 8)
            .unwrap();
        link_object.set_process_vector(0, [10, 20, 30]).unwrap();
        link_object.set_process_vector(1, [0; 3]).unwrap();
        link_object
            .set_process_vector(2, [INITIAL_SCALE; 3])
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(requester_object).unwrap();
        machine.insert_object(link_object).unwrap();

        assert_eq!(
            machine
                .run_with_host_effects(requester, 1, |machine, effect| {
                    assert_eq!(
                        effect,
                        &VmEffect::TransformModelVertex {
                            requester,
                            link,
                            output_vector: 5,
                            model_eid,
                            frame_index: 1,
                            vertex_index: 2,
                        }
                    );
                    machine.complete_model_vertex_transform(
                        requester,
                        link,
                        5,
                        Some(ModelVertexSource {
                            local_position: [100, 200, 300],
                            geometry_scale: [INITIAL_SCALE; 3],
                        }),
                    )
                })
                .unwrap()
                .steps,
            1
        );
        assert_eq!(
            machine.object(requester).unwrap().process_vector(5),
            // Retail's Q12 table has cos(0) == 4095, so the three fixed-point
            // matrix stages deliberately retain their native truncation.
            Ok([106, 212, 318])
        );
    }

    #[test]
    fn native_misc_midi_toggle_retains_the_translated_value() {
        let object = handle(0);
        let mut vm_object = VmObject::new(object, vec![misc(12, 6, REG0)]).unwrap();
        vm_object.set_register(0, 0x1234).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(vm_object).unwrap();

        assert_eq!(machine.run(object, 1).unwrap().steps, 1);
        assert_eq!(
            machine.effects(),
            &[VmEffect::MidiTogglePlayback {
                object,
                value: 0x1234,
            }]
        );
    }

    #[test]
    fn native_misc_card_control_is_signed_synchronous_and_never_pushes() {
        let object = handle(0);
        let mut vm_object = VmObject::new(object, vec![misc(15, -3, REG0)]).unwrap();
        vm_object.set_register(0, (-7_i32) as u32).unwrap();
        vm_object
            .set_register(process_register::MISC_VALUE, 0x55)
            .unwrap();
        let mut machine = Machine::new(119);
        machine.insert_object(vm_object).unwrap();

        let execution = machine
            .run_with_host_requests(object, 1, |machine, request| {
                let VmHostRequest::Card(request) = request else {
                    panic!("expected typed card request");
                };
                assert_eq!(
                    request,
                    CardHostRequest {
                        object,
                        operation: -3,
                        part_index: -7,
                    }
                );
                machine.complete_card_host_request(request, 1)
            })
            .unwrap();

        assert_eq!(execution.reason, HaltReason::BudgetExhausted);
        assert!(machine.object(object).unwrap().stack().is_empty());
        assert_eq!(
            machine
                .object(object)
                .unwrap()
                .register(process_register::MISC_VALUE),
            Ok(1)
        );
        assert_eq!(machine.pending_card_host_request(), None);
    }

    #[test]
    fn native_misc_add_to_handle_is_a_synchronous_checked_effect() {
        let object = handle(0);
        let mut vm_object = VmObject::new(object, vec![misc(12, 2, REG0)]).unwrap();
        vm_object.set_register(0, 4 << 8).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(vm_object).unwrap();

        assert_eq!(
            machine.run(object, 1).unwrap(),
            Execution {
                reason: HaltReason::HostEffect,
                steps: 1,
            }
        );
        assert_eq!(
            machine.effects(),
            &[VmEffect::ReparentToRoot { object, root: 4 }]
        );
    }

    #[test]
    fn synchronous_event_service_returns_state_and_preserves_guard_bit() {
        let sender = handle(0);
        let recipient = handle(1);
        let mut object = VmObject::new(recipient, vec![event_return(0x88, 1, 0, 0, 0, 5)]).unwrap();
        object.event_pc = Some(0);
        object.state_flags_by_index = vec![0; 6];
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(sender, vec![0]).unwrap())
            .unwrap();
        machine.insert_object(object).unwrap();

        assert_eq!(
            machine.send_event(Some(sender), Some(recipient), 0x1500, Some(&[11, 22])),
            Ok(EventDispatchOutcome {
                acknowledged: false,
                state_change: Some(EventStateChange {
                    recipient,
                    state: 5,
                    event: 0x1500,
                    arguments: vec![11, 22],
                    argument_pool_slots: vec![None, None],
                }),
            })
        );
        let recipient = machine.object(recipient).unwrap();
        assert_eq!(recipient.links[7], Some(sender));
        assert_eq!(recipient.register(process_register::EVENT), Ok(0x1500));
        assert_eq!(recipient.state(), 5);
        assert!(recipient.stack().is_empty());
        assert!(recipient.call_stack.is_empty());
        assert_eq!(
            machine
                .object(sender)
                .unwrap()
                .register(process_register::MISC_VALUE),
            Ok(0),
            "opcode 0x88 returns a false sender guard"
        );
    }

    #[test]
    fn checked_send_event_rejects_oversized_arguments_before_mutation() {
        let sender = handle(0);
        let recipient = handle(1);
        let mut sender_object = VmObject::new(sender, vec![0]).unwrap();
        sender_object
            .set_register(process_register::MISC_VALUE, 0x55)
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(sender_object).unwrap();
        machine
            .insert_object(VmObject::new(recipient, vec![0]).unwrap())
            .unwrap();

        assert_eq!(
            machine.send_event(
                Some(sender),
                Some(recipient),
                0,
                Some(&[0; MAX_EVENT_ARGUMENTS + 1]),
            ),
            Err(VmError::EventArgumentsTooLong(MAX_EVENT_ARGUMENTS + 1))
        );
        assert_eq!(
            machine
                .object(sender)
                .unwrap()
                .register(process_register::MISC_VALUE),
            Ok(0x55)
        );
        assert_eq!(machine.object(recipient).unwrap().links[7], None);

        assert_eq!(
            machine.send_event(Some(sender), None, 0, None),
            Ok(EventDispatchOutcome {
                acknowledged: false,
                state_change: None,
            })
        );
        assert_eq!(
            machine
                .object(sender)
                .unwrap()
                .register(process_register::MISC_VALUE),
            Ok(0)
        );
    }

    #[test]
    fn event_state_change_rebind_preserves_argument_pool_identity() {
        let target = handle(0);
        let recipient = handle(1);
        let pointer = CollisionObjectReference::new(target).to_word();
        let mut recipient_object = VmObject::new(recipient, vec![0]).unwrap();
        recipient_object.event_map = vec![1];
        recipient_object.state_flags_by_index = vec![0; 2];
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(target, vec![0]).unwrap())
            .unwrap();
        machine.bind_retail_pool_slot(target, 5).unwrap();
        machine.insert_object(recipient_object).unwrap();

        let outcome = machine
            .send_event(None, Some(recipient), 0, Some(&[pointer]))
            .unwrap();
        let change = outcome.state_change.unwrap();
        assert_eq!(change.arguments, [pointer]);
        assert_eq!(change.argument_pool_slots, [Some(5)]);
        let state = VmStateProgram::new(
            1,
            GoolState {
                flags: 0,
                status_c: 0,
                external_index: 0,
                event_pc: GOOL_PC_NONE,
                transition_pc: GOOL_PC_NONE,
                code_pc: GOOL_PC_NONE,
            },
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        machine
            .rebind_state_program_with_pool_slots(
                recipient,
                &state,
                &change.arguments,
                &change.argument_pool_slots,
            )
            .unwrap();
        let object = machine.object(recipient).unwrap();
        assert_eq!(
            object.register_pool_slot(object.initial_stack_pointer() as usize),
            Ok(Some(5))
        );
    }

    #[test]
    fn malformed_argument_pool_sidecars_are_rejected_transactionally() {
        let h = handle(0);
        let pointer = CollisionObjectReference::new(h).to_word();
        let invalid_slot = MAX_OBJECTS as u8;
        let mut object = VmObject::new(h, vec![0]).unwrap();
        let object_snapshot = object.clone();
        assert_eq!(
            object.initialize_arguments_with_pool_slots(&[pointer], &[]),
            Err(VmError::EventArgumentPoolSlotsLengthMismatch {
                arguments: 1,
                pool_slots: 0,
            })
        );
        assert_eq!(object, object_snapshot);
        assert_eq!(
            object.initialize_arguments_with_pool_slots(&[pointer], &[Some(invalid_slot)]),
            Err(VmError::InvalidRetailPoolSlot(invalid_slot))
        );
        assert_eq!(object, object_snapshot);

        let recipient = handle(1);
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine
            .insert_object(VmObject::new(recipient, vec![0]).unwrap())
            .unwrap();
        let machine_snapshot = machine.clone();
        assert_eq!(
            machine.send_event_with_host_requests_and_pool_slots(
                Some(h),
                Some(recipient),
                0,
                Some(&[pointer]),
                Some(&[]),
                |_machine, _request| Ok(()),
            ),
            Err(VmError::EventArgumentPoolSlotsLengthMismatch {
                arguments: 1,
                pool_slots: 0,
            })
        );
        assert_eq!(machine, machine_snapshot);

        let eid = Eid::from_raw(0x7500_2055);
        let state = VmStateProgram::new(
            0,
            GoolState {
                flags: 0,
                status_c: 0,
                external_index: 0,
                event_pc: GOOL_PC_NONE,
                transition_pc: GOOL_PC_NONE,
                code_pc: GOOL_PC_NONE,
            },
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
        .with_paging_metadata(5, [PageIndex::new(0)], [(eid, PageIndex::new(4))]);
        assert_eq!(
            machine.rebind_state_program_with_pool_slots(h, &state, &[pointer], &[]),
            Err(VmError::EventArgumentPoolSlotsLengthMismatch {
                arguments: 1,
                pool_slots: 0,
            })
        );
        assert_eq!(machine, machine_snapshot);
        assert_eq!(
            machine.rebind_state_program_with_pool_slots(
                h,
                &state,
                &[pointer],
                &[Some(invalid_slot)],
            ),
            Err(VmError::InvalidRetailPoolSlot(invalid_slot))
        );
        assert_eq!(machine, machine_snapshot);

        let unknown = handle(2);
        assert_eq!(
            machine.rebind_state_program(unknown, &state, &[]),
            Err(VmError::UnknownObject(unknown))
        );
        assert_eq!(machine, machine_snapshot);

        let mismatch_eid = Eid::from_raw(0x7500_2455);
        let mismatch = VmStateProgram::new(
            1,
            GoolState {
                flags: 0,
                status_c: 0,
                external_index: 0,
                event_pc: GOOL_PC_NONE,
                transition_pc: GOOL_PC_NONE,
                code_pc: GOOL_PC_NONE,
            },
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
        .with_paging_metadata(9, [PageIndex::new(1)], [(mismatch_eid, PageIndex::new(8))]);
        assert_eq!(
            machine.rebind_state_program(h, &mismatch, &[]),
            Err(VmError::StateProgramMismatch {
                requested: 0,
                provided: 1,
            })
        );
        assert_eq!(machine, machine_snapshot);

        let conflict_eid = Eid::from_raw(0x7500_2855);
        machine
            .register_paging_metadata(
                3,
                &[PageIndex::new(1)],
                &[(conflict_eid, PageIndex::new(2))],
            )
            .unwrap();
        let conflict_snapshot = machine.clone();
        let conflicting = VmStateProgram::new(
            0,
            GoolState {
                flags: 0,
                status_c: 0,
                external_index: 0,
                event_pc: GOOL_PC_NONE,
                transition_pc: GOOL_PC_NONE,
                code_pc: GOOL_PC_NONE,
            },
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
        .with_paging_metadata(4, [PageIndex::new(1)], [(conflict_eid, PageIndex::new(3))]);
        assert_eq!(
            machine.rebind_state_program(h, &conflicting, &[]),
            Err(VmError::ConflictingEntryPage {
                eid: conflict_eid,
                first: PageIndex::new(2),
                second: PageIndex::new(3),
            })
        );
        assert_eq!(machine, conflict_snapshot);
    }

    #[test]
    fn event_service_earg_and_guarded_null_return_are_one_synchronous_scope() {
        let sender = handle(0);
        let recipient = handle(1);
        let mut object = VmObject::new(
            recipient,
            vec![0x1c00_5b7f, event_return(0x89, 2, 1, 0x1f, 0, 0)],
        )
        .unwrap();
        object.event_pc = Some(0);
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(sender, vec![0]).unwrap())
            .unwrap();
        machine.insert_object(object).unwrap();

        assert_eq!(
            machine.send_event(Some(sender), Some(recipient), 0x1500, Some(&[7])),
            Ok(EventDispatchOutcome {
                acknowledged: true,
                state_change: None,
            })
        );
        let recipient = machine.object(recipient).unwrap();
        assert!(recipient.stack().is_empty());
        assert!(recipient.call_stack.is_empty());
        assert!(machine.event_argument_scopes.is_empty());
        assert_eq!(
            machine
                .object(sender)
                .unwrap()
                .register(process_register::MISC_VALUE),
            Ok(1)
        );
    }

    #[test]
    fn event_return_mode_zero_defers_guarded_null_response_to_frame_return() {
        let recipient = handle(0);
        let mut object = VmObject::new(
            recipient,
            vec![
                event_return(0x89, 0, 0, 0, 0, 0),
                control_flow(2, 0, 0, 0, 0),
            ],
        )
        .unwrap();
        object.event_pc = Some(0);
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        assert_eq!(
            machine.send_event(None, Some(recipient), 0, Some(&[])),
            Ok(EventDispatchOutcome {
                acknowledged: true,
                state_change: None,
            })
        );
        assert!(machine.object(recipient).unwrap().stack().is_empty());
    }

    #[test]
    fn false_event_return_mode_zero_branches_from_the_post_fetch_pc() {
        let recipient = handle(0);
        let mut object = VmObject::new(
            recipient,
            vec![
                event_return(0x89, 0, 1, 6, 0, 1),
                Instruction::encode(0xff, 0, 0),
                event_return(0x89, 2, 0, 0, 0, 0),
            ],
        )
        .unwrap();
        object.event_pc = Some(0);
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        assert_eq!(
            machine.send_event(None, Some(recipient), 0, None),
            Ok(EventDispatchOutcome {
                acknowledged: true,
                state_change: None,
            })
        );
    }

    #[test]
    fn invalid_event_service_return_falls_back_to_exact_event_map_state() {
        let sender = handle(0);
        let recipient = handle(1);
        let mut object = VmObject::new(recipient, vec![control_flow(2, 0, 0, 0, 0)]).unwrap();
        object.event_pc = Some(0);
        object.event_map = vec![EVENT_MAP_NULL_STATE; 4];
        object.event_map[3] = 2;
        object.state_flags_by_index = vec![0; 3];
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(sender, vec![0]).unwrap())
            .unwrap();
        machine.insert_object(object).unwrap();

        assert_eq!(
            machine.send_event(Some(sender), Some(recipient), 0x0300, None),
            Ok(EventDispatchOutcome {
                acknowledged: true,
                state_change: Some(EventStateChange {
                    recipient,
                    state: 2,
                    event: 0x0300,
                    arguments: Vec::new(),
                    argument_pool_slots: Vec::new(),
                }),
            })
        );
        assert_eq!(machine.object(recipient).unwrap().state(), 2);
    }

    #[test]
    fn event_map_distinguishes_null_state_from_every_high_bit_interrupt() {
        let null_recipient = handle(0);
        let interrupt_recipient = handle(1);
        let invalid_interrupt = handle(2);

        let mut null = VmObject::new(null_recipient, vec![0]).unwrap();
        null.event_map = vec![EVENT_MAP_NULL_STATE];
        let mut interrupt = VmObject::new(interrupt_recipient, vec![0]).unwrap();
        interrupt.event_map = vec![0x8000];
        interrupt.global_code = vec![control_flow(2, 0, 0, 0, 0)];
        let mut invalid = VmObject::new(invalid_interrupt, vec![0]).unwrap();
        invalid.event_map = vec![0xffff];
        invalid.global_code = vec![control_flow(2, 0, 0, 0, 0)];

        let mut machine = Machine::new(0);
        machine.insert_object(null).unwrap();
        machine.insert_object(interrupt).unwrap();
        machine.insert_object(invalid).unwrap();
        assert_eq!(
            machine.send_event(None, Some(null_recipient), 0, Some(&[])),
            Ok(EventDispatchOutcome {
                acknowledged: false,
                state_change: None,
            })
        );
        assert_eq!(
            machine.send_event(None, Some(interrupt_recipient), 1, Some(&[0x1234])),
            Ok(EventDispatchOutcome {
                acknowledged: true,
                state_change: None,
            })
        );
        let interrupt = machine.object(interrupt_recipient).unwrap();
        assert_eq!(interrupt.register(process_register::EVENT), Ok(1));
        assert!(interrupt.stack().is_empty());
        assert_eq!(
            machine.send_event(None, Some(invalid_interrupt), 0, None),
            Err(VmError::InvalidJump {
                object: invalid_interrupt,
                target: 0x7fff,
            })
        );
    }

    #[test]
    fn event_state_guards_apply_squash_exception_and_status_update() {
        let sender = handle(0);
        let recipient = handle(1);
        let mut object = VmObject::new(recipient, vec![0]).unwrap();
        object.event_map = vec![EVENT_MAP_NULL_STATE; 26];
        object.event_map[3] = 1;
        object.event_map[0x19] = 1;
        object.state_flags_by_index = vec![0, 2];
        object.set_register(process_register::STATUS_C, 2).unwrap();
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(sender, vec![0]).unwrap())
            .unwrap();
        machine.insert_object(object).unwrap();

        assert_eq!(
            machine.send_event(Some(sender), Some(recipient), 0x0300, None),
            Ok(EventDispatchOutcome {
                acknowledged: false,
                state_change: None,
            })
        );
        let outcome = machine
            .send_event(Some(sender), Some(recipient), SQUASH_EVENT, None)
            .unwrap();
        assert_eq!(
            outcome.state_change.as_ref().map(|change| change.state),
            Some(1)
        );
        let recipient = machine.object(recipient).unwrap();
        assert_eq!(
            recipient.register(process_register::EVENT),
            Ok(SQUASH_EVENT)
        );
        assert_ne!(
            recipient.register(process_register::STATUS_A).unwrap() & STATUS_A_EVENT_SQUASHED,
            0
        );
    }

    #[test]
    fn event_service_budget_failure_unwinds_frame_and_argument_scope() {
        let recipient = handle(0);
        let mut object = VmObject::new(recipient, vec![control_flow(0, 0, 0, 0, 0x3ff)]).unwrap();
        object.event_pc = Some(0);
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        assert_eq!(
            machine.send_event(None, Some(recipient), 0, Some(&[1])),
            Err(VmError::EventServiceBudgetExhausted(recipient))
        );
        let recipient = machine.object(recipient).unwrap();
        assert!(recipient.stack().is_empty());
        assert!(recipient.call_stack.is_empty());
        assert!(machine.event_argument_scopes.is_empty());
    }

    #[test]
    fn event_service_returns_remain_an_explicit_nested_interpreter_boundary() {
        let h = handle(0);
        // Real Crash word: conditional guarded return to state 0x1e. It is
        // valid only inside `GOOL_FLAG_EVENT_SERVICE`, so ordinary execution
        // must not misinterpret it as a direct state change.
        let object = VmObject::new(h, vec![0x8957_c01e]).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        assert_eq!(
            machine.run(h, 1),
            Err(VmError::UnsupportedEventServiceReturn {
                opcode: 0x89,
                condition_type: 1,
                return_type: 1,
                register: 0x1f,
            })
        );
    }

    #[test]
    fn null_input_uses_documented_psx_compatibility_value() {
        let h = handle(0);
        let code = vec![Instruction::encode(0x00, 0xbe0, REG0)];
        let mut object = VmObject::new(h, code).unwrap();
        object.set_register(0, 4).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.run(h, 1).unwrap();
        assert_eq!(machine.object(h).unwrap().stack(), &[7]);
    }

    #[test]
    fn retail_pad_snapshot_and_control_opcode_preserve_mask_results() {
        let h = handle(0);
        let cross_tapped = (0x1a_u32 << 24) | (1 << 12) | 0x40;
        let previous_square_tapped = (0x1a_u32 << 24) | (3 << 12) | 0x80;
        let up_tapped = (0x1a_u32 << 24) | (1 << 14) | (9 << 16);
        let diagonal_held = (0x1a_u32 << 24) | (2 << 14) | (1 << 16);
        let object = VmObject::new(
            h,
            vec![
                cross_tapped,
                previous_square_tapped,
                up_tapped,
                diagonal_held,
            ],
        )
        .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        let snapshot = RetailPadSnapshot {
            tapped: 0x1040,
            held: 0x3000,
            held_previous: 0,
            tapped_previous: 0x80,
            held_previous_2: 0x4000,
        };
        machine.set_pad_snapshot(0, snapshot).unwrap();
        assert_eq!(machine.pad_snapshot(0), Ok(snapshot));
        assert_eq!(
            machine.set_pad_snapshot(RETAIL_PAD_COUNT, snapshot),
            Err(VmError::InvalidPadPort(RETAIL_PAD_COUNT))
        );

        machine.run(h, 4).unwrap();
        assert_eq!(machine.object(h).unwrap().stack(), &[0x40, 0x80, 0x1000, 1]);
    }

    #[test]
    fn control_opcode_applies_two_frame_taps_and_exact_not_bit() {
        let h = handle(0);
        let previous_cross_tapped = (0x1a_u32 << 24) | (3 << 12) | 0x40;
        let missing_cross_negated = (0x1a_u32 << 24) | (1 << 20) | (1 << 12) | 0x40;
        let object = VmObject::new(h, vec![previous_cross_tapped, missing_cross_negated]).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine
            .set_pad_snapshot(
                0,
                RetailPadSnapshot {
                    tapped_previous: 0x40,
                    ..RetailPadSnapshot::default()
                },
            )
            .unwrap();

        machine.run(h, 2).unwrap();
        assert_eq!(machine.object(h).unwrap().stack(), &[0x40, 1]);
    }

    #[test]
    fn null_output_discards_a_value_after_preserving_input_pop() {
        let h = handle(0);
        let code = vec![
            Instruction::encode(0x00, REG0, REG1),
            Instruction::encode(0x11, STACK, 0x0be0),
        ];
        let mut object = VmObject::new(h, code).unwrap();
        object.set_register(0, 2).unwrap();
        object.set_register(1, 3).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        machine.run(h, 2).unwrap();
        assert!(machine.object(h).unwrap().stack().is_empty());
    }

    #[test]
    fn spawn_arguments_are_addressed_below_the_initial_frame_pointer() {
        let h = handle(0);
        let code = vec![
            Instruction::encode(0x11, 0x0b7f, REG0),
            Instruction::encode(0x11, 0x0801, 0x0b7f),
        ];
        let mut object = VmObject::new(h, code).unwrap();
        object.initialize_arguments(&[7, 11]).unwrap();
        assert_eq!(object.register(SYNTHETIC_STACK_POINTER), Ok(7));
        assert_eq!(object.register(SYNTHETIC_STACK_POINTER + 1), Ok(11));
        assert_eq!(
            object.register(SYNTHETIC_STACK_POINTER + 2),
            Ok(INITIAL_FRAME_FLAGS)
        );
        assert_eq!(object.register(SYNTHETIC_STACK_POINTER + 5), Ok(0));
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        machine.run(h, 2).unwrap();
        assert_eq!(machine.object(h).unwrap().register(0), Ok(11));
        assert_eq!(
            machine
                .object(h)
                .unwrap()
                .register(SYNTHETIC_STACK_POINTER + 1),
            Ok(0x100)
        );
        assert_eq!(
            machine.object(h).unwrap().stack(),
            &[
                7,
                0x100,
                INITIAL_FRAME_FLAGS,
                CODE_REFERENCE_TAG,
                (SYNTHETIC_STACK_POINTER * 4) as u32,
            ]
        );
    }

    #[test]
    fn unbound_pool_operands_read_psx_null_value_and_ignore_writes() {
        let h = handle(0);
        let missing_link_two_register_zero = 0x0c80;
        let code = vec![
            Instruction::encode(0x11, missing_link_two_register_zero, REG1),
            Instruction::encode(0x11, 0x0801, missing_link_two_register_zero),
        ];
        let object = VmObject::new(h, code).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        machine.run(h, 2).unwrap();
        assert_eq!(machine.object(h).unwrap().register(1), Ok(NULL_INPUT_VALUE));
    }

    #[test]
    fn packed_color_opcodes_use_self_link_and_retail_halfwords() {
        let h = handle(0);
        let color_index = 21_u32;
        let write = (0x24_u32 << 24) | (color_index << 15) | 0x0802;
        let read = (0x23_u32 << 24) | (color_index << 15);
        let object = VmObject::new(h, vec![write, read]).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        machine.run(h, 2).unwrap();
        assert_eq!(machine.object(h).unwrap().color(21), Ok(0x200));
        assert_eq!(machine.object(h).unwrap().stack(), &[0x200]);
    }

    #[test]
    fn retail_dual_input_pop_and_repush_preserves_the_crash_stack_word() {
        let h = handle(0);
        let code = vec![Instruction::encode(0x00, REG0, REG1), 0x16be_0e1f];
        let mut object = VmObject::new(h, code).unwrap();
        object.set_register(0, 2).unwrap();
        object.set_register(1, 3).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        machine.run(h, 2).unwrap();
        assert_eq!(machine.object(h).unwrap().stack(), &[5]);
    }

    #[test]
    fn retail_dual_input_pushes_destination_then_source_and_honors_null_destination() {
        let h = handle(0);
        let code = vec![
            Instruction::encode(0x16, REG0, REG1),
            Instruction::encode(0x16, REG0, 0x0be0),
        ];
        let mut object = VmObject::new(h, code).unwrap();
        object.set_register(0, 2).unwrap();
        object.set_register(1, 3).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        machine.run(h, 2).unwrap();
        assert_eq!(machine.object(h).unwrap().stack(), &[3, 2]);
    }

    #[test]
    fn pointer_exposing_dual_input_pushes_checked_storage_references() {
        let h = handle(0);
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(h, vec![0x2604_d04c]).unwrap())
            .unwrap();

        machine.run(h, 1).unwrap();
        let stack = machine.object(h).unwrap().stack();
        assert_eq!(stack.len(), 2);
        let destination = StorageReference::from_word(stack[0]).unwrap();
        let source = StorageReference::from_word(stack[1]).unwrap();
        assert_eq!(destination.object(), Some(h));
        assert_eq!(destination.region(), StorageRegion::Internal);
        assert_eq!(destination.index(), 0x04c);
        assert_eq!(source.index(), 0x04d);
        assert_eq!(machine.read_storage_reference(destination), Ok(0));
    }

    #[test]
    fn lea_stores_a_checked_address_after_source_then_destination_translation() {
        let h = handle(0);
        let mut object = VmObject::new(
            h,
            vec![
                Instruction::encode(0x00, REG0, REG1),
                Instruction::encode(0x14, STACK, STACK),
            ],
        )
        .unwrap();
        object.set_register(0, 2).unwrap();
        object.set_register(1, 3).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        machine.run(h, 2).unwrap();

        let stack = machine.object(h).unwrap().stack();
        assert_eq!(stack.len(), 1);
        let reference = StorageReference::from_word(stack[0]).unwrap();
        assert_eq!(reference.object(), Some(h));
        assert_eq!(reference.region(), StorageRegion::Register);
        assert_eq!(reference.index(), SYNTHETIC_STACK_POINTER as u16);
        assert_eq!(
            machine.read_storage_reference(reference),
            Ok(reference.to_word()),
            "input pop must precede output push, so LEA can point at its reoccupied stack cell"
        );
    }

    #[test]
    fn lea_preserves_null_and_missing_pool_pointer_semantics() {
        let h = handle(0);
        let missing_link_two_register_zero = 0x0c80;
        let mut object = VmObject::new(
            h,
            vec![
                Instruction::encode(0x14, 0x0be0, REG0),
                Instruction::encode(0x00, REG1, REG2),
                Instruction::encode(0x14, STACK, missing_link_two_register_zero),
                Instruction::encode(0x14, missing_link_two_register_zero, 0x0e03),
            ],
        )
        .unwrap();
        object.set_register(0, 0xdead_beef).unwrap();
        object.set_register(1, 4).unwrap();
        object.set_register(2, 5).unwrap();
        object.set_register(3, 0xdead_beef).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        machine.run(h, 4).unwrap();

        let object = machine.object(h).unwrap();
        assert_eq!(object.register(0), Ok(0), "null A stores a null word");
        assert_eq!(
            object.register(3),
            Ok(0),
            "a missing input link translates to a null source address"
        );
        assert!(
            object.stack().is_empty(),
            "A's stack pop remains observable when missing B has no output address"
        );
    }

    #[test]
    fn input_and_output_immediates_use_independent_cursors_over_shared_constants() {
        let h = handle(0);
        let mut machine = Machine::new(0);
        machine
            .insert_object(
                VmObject::new(
                    h,
                    vec![
                        // Capture input immediate 0x100 in shared slot one.
                        0x2680_1000,
                        // Solid subop six still translates B=0x200 through
                        // input and output cursors before its entity-less no-op.
                        0x8e18_0802,
                    ],
                )
                .unwrap(),
            )
            .unwrap();

        machine.run(h, 1).unwrap();
        let source = StorageReference::from_word(machine.object(h).unwrap().stack()[1]).unwrap();
        assert_eq!(source.region(), StorageRegion::Constant);
        assert_eq!(source.index(), 1);
        assert_eq!(machine.read_storage_reference(source), Ok(0x100));

        machine.run(h, 1).unwrap();
        assert_eq!(
            machine.read_storage_reference(source),
            Ok(0x200),
            "output cursor 0->1 must overwrite the live slot-one input pointer"
        );
    }

    #[test]
    fn tagged_pointer_list_feeds_paging_count_and_available_page_operations() {
        let h = handle(0);
        let eid_a = Eid::from_raw(0x7500_2055);
        let eid_b = Eid::from_raw(0x7500_2073);
        let mut object = VmObject::new(
            h,
            vec![
                Instruction::encode(0x26, 1, 0),
                Instruction::encode(0x8b, 2, 3),
                Instruction::encode(0x8b, 4, 0x0be0),
            ],
        )
        .unwrap();
        object.internal[0] = eid_a.raw();
        object.internal[1] = eid_b.raw();
        object.internal[2] = 5;
        object.internal[3] = 2;
        object.internal[4] = 4;
        object.page_count = 10;
        object.resident_pages = vec![PageIndex::new(0)];
        object.entry_pages = vec![(eid_a, PageIndex::new(2)), (eid_b, PageIndex::new(3))];
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        machine.run(h, 1).unwrap();
        let references = machine.object(h).unwrap().stack();
        assert_eq!(references.len(), 2);
        assert_eq!(
            StorageReference::from_word(references[0]).unwrap().index(),
            0
        );
        assert_eq!(
            StorageReference::from_word(references[1]).unwrap().index(),
            1
        );
        machine.run(h, 2).unwrap();
        // Pages two and three are physically resident type-1 pages but have
        // not had entry offsets translated. Raw EIDs therefore require no
        // additional resolved entries yet, and all slots remain available.
        assert_eq!(machine.object(h).unwrap().stack(), &[0, 10]);
    }

    #[test]
    fn paging_open_misc_probe_and_explicit_available_follow_retail_counts() {
        let h = handle(0);
        let eid = Eid::from_raw(0x7500_2055);
        let other_eid = Eid::from_raw(0x7500_2073);
        let misc = 0x0e00 | process_register::MISC_VALUE as u16;
        let mut object = VmObject::new(
            h,
            vec![
                Instruction::encode(0x8b, 1, 0),
                Instruction::encode(0x8b, 2, misc),
                Instruction::encode(0x8b, 3, misc),
                Instruction::encode(0x8b, 4, misc),
                Instruction::encode(0x8b, 4, 7),
                Instruction::encode(0x8b, 4, 7),
                Instruction::encode(0x26, 0x0be0, 7),
                Instruction::encode(0x8b, 5, 8),
                Instruction::encode(0x8b, 6, 0x0be0),
            ],
        )
        .unwrap();
        object.internal[0] = eid.raw();
        object.internal[1] = 1;
        object.internal[2] = 6;
        object.internal[3] = 3;
        object.internal[4] = 2;
        object.internal[5] = 5;
        object.internal[6] = 4;
        object.internal[7] = eid.raw();
        object.internal[8] = 1;
        object.page_count = 4;
        object.resident_pages = vec![PageIndex::new(0)];
        object.entry_pages = vec![(eid, PageIndex::new(2)), (other_eid, PageIndex::new(3))];
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        machine.run(h, 1).unwrap();
        let entry_token = machine
            .object(h)
            .unwrap()
            .register(process_register::MISC_VALUE)
            .unwrap();
        assert!(EntryReference::from_word(entry_token).is_some());
        assert!(StorageReference::from_word(entry_token).is_none());
        assert_eq!(machine.paging_page_references[&PageIndex::new(2)], 1);

        // The resolved token owns entry identity. Mutating the original EID
        // cell cannot redirect op6(misc) to another page.
        machine.object_mut(h).unwrap().internal[0] = other_eid.raw();
        machine.run(h, 1).unwrap();
        assert_eq!(machine.paging_page_references[&PageIndex::new(2)], 2);
        assert_eq!(
            machine
                .object(h)
                .unwrap()
                .register(process_register::MISC_VALUE),
            Ok(entry_token)
        );
        assert_eq!(machine.paging_page_references.get(&PageIndex::new(3)), None);

        // Case 3 uses retail NSClose(count=0): a resolved type-1 page returns
        // literal one. The retail binary then jumps to the common one-word
        // push and cannot fall through to case four; the decompiled C source
        // omitted that control-flow edge.
        machine.run(h, 1).unwrap();
        assert_eq!(machine.object(h).unwrap().stack(), &[1]);

        // B points at misc_entry, which contains the logical token written by
        // open. The first close decrements two references to one and writes
        // the resolved type-1 result to misc_flag.
        machine.run(h, 1).unwrap();
        assert_eq!(
            machine
                .object(h)
                .unwrap()
                .register(process_register::MISC_VALUE),
            Ok(1)
        );
        assert!(machine.paging_loaded_pages.contains(&PageIndex::new(2)));
        assert_eq!(machine.paging_page_references[&PageIndex::new(2)], 1);

        // Close through a fresh EID cell to reach zero; PC type-1 pages remain
        // resident at ref0 and still return literal one.
        machine.run(h, 1).unwrap();
        assert_eq!(machine.paging_page_references[&PageIndex::new(2)], 0);
        assert!(machine.paging_loaded_pages.contains(&PageIndex::new(2)));

        // NSPageDecRef is idempotent at zero.
        machine.run(h, 1).unwrap();
        assert_eq!(
            machine
                .object(h)
                .unwrap()
                .register(process_register::MISC_VALUE),
            Ok(1)
        );
        machine.run(h, 3).unwrap();
        // Case five replaces the tagged EID list with its required-page
        // count; the final explicit case four pushes the available count.
        assert_eq!(machine.object(h).unwrap().stack(), &[1, 1, 4]);
    }

    #[test]
    fn copied_texture_audio_close_returns_zero_but_count_zero_probe_returns_one() {
        let pages = [PageIndex::new(2), PageIndex::new(3)];
        let mut machine = Machine::new(0);
        machine
            .seed_platform_paging_state_with_uncounted_pages(
                4,
                pages,
                pages.into_iter().map(|page| (page, 1)),
                pages,
            )
            .unwrap();

        for page in pages {
            assert_eq!(machine.close_paging_page(page, true), 0);
            assert_eq!(machine.paging_page_references[&page], 1);
            assert_eq!(machine.close_paging_page(page, false), 1);
            assert_eq!(machine.paging_page_references[&page], 1);
        }
    }

    #[test]
    fn queued_virtual_host_response_retains_reference_until_async_resolution() {
        let h = handle(0);
        let eid = Eid::from_raw(0x7500_2055);
        let page = PageIndex::new(2);
        let mut object = VmObject::new(h, vec![Instruction::encode(0x8b, 1, 0)]).unwrap();
        object.internal[0] = eid.raw();
        object.internal[1] = 1;
        object.page_count = 3;
        object.entry_pages = vec![(eid, page)];
        let mut machine = Machine::new(0);
        machine
            .seed_platform_paging_state(3, std::iter::empty(), std::iter::empty())
            .unwrap();
        machine.insert_object(object).unwrap();

        machine
            .run_with_host_requests(h, 1, |machine, request| {
                let VmHostRequest::Effect(VmEffect::Paging {
                    object,
                    operation,
                    physical,
                    reference,
                    eid,
                    page,
                    was_resolved,
                }) = request
                else {
                    return Err(VmError::MissingHostEffect);
                };
                machine.complete_paging_host_request(
                    PagingHostRequest {
                        object,
                        operation,
                        physical,
                        reference,
                        eid,
                        page,
                        was_resolved,
                    },
                    PagingHostResponse::Queued,
                )
            })
            .unwrap();

        assert!(machine.paging_pending_pages.contains(&page));
        assert!(!machine.paging_resolved_pages.contains(&page));
        assert_eq!(machine.paging_page_references[&page], 1);
        assert_eq!(
            machine
                .object(h)
                .unwrap()
                .register(process_register::MISC_VALUE),
            Ok(0)
        );

        machine
            .apply_platform_paging_resolution(page, PageInvalidations::NONE)
            .unwrap();
        assert!(!machine.paging_pending_pages.contains(&page));
        assert!(machine.paging_resolved_pages.contains(&page));
        assert_eq!(machine.paging_page_references[&page], 1);
    }

    #[test]
    fn brio_page_probe_gate_preserves_the_zero_sentinel_and_takes_retail_branch() {
        let h = handle(0);
        let eids = [
            Eid::from_raw(0x7500_2055),
            Eid::from_raw(0x7500_2073),
            Eid::from_raw(0x7500_2091),
            Eid::from_raw(0x7500_20af),
            Eid::from_raw(0x7500_20cd),
            Eid::from_raw(0x7500_20eb),
        ];
        let pages = [
            PageIndex::new(2),
            PageIndex::new(3),
            PageIndex::new(4),
            PageIndex::new(5),
            PageIndex::new(6),
            PageIndex::new(7),
        ];
        let mut code = Vec::new();
        for eid_index in 0..eids.len() {
            code.push(Instruction::encode(0x8b, 6, eid_index as u16));
        }
        for _ in 0..7 {
            code.push(Instruction::encode(0x05, STACK, STACK));
        }
        // Mirrors Brio PC 80: operation zero, condition type two (`!pop()`),
        // no arguments, relative target +22. PC is post-fetch, so local PC
        // 14 branches to 36 exactly when the zero sentinel survives all six
        // one-word probes and seven logical-AND reductions.
        code.push(control_flow(0, 2, 0x1f, 0, 22));
        code.resize(37, Instruction::encode(0xff, 0, 0));

        let mut object = VmObject::new(h, code).unwrap();
        for (index, eid) in eids.into_iter().enumerate() {
            object.internal[index] = eid.raw();
        }
        object.internal[6] = 3;
        object.page_count = 8;
        object.resident_pages = vec![PageIndex::new(0)];
        object.entry_pages = eids.into_iter().zip(pages).collect();

        let mut machine = Machine::new(0);
        machine
            .seed_platform_paging_state(8, pages, std::iter::empty())
            .unwrap();
        machine.insert_object(object).unwrap();
        machine.push(h, 0).unwrap();
        machine.push(h, 1).unwrap();

        for probe_index in 0..6 {
            assert_eq!(machine.run(h, 1).unwrap().reason, HaltReason::HostEffect);
            let expected = [0, 1]
                .into_iter()
                .chain(std::iter::repeat_n(1, probe_index + 1))
                .collect::<Vec<_>>();
            assert_eq!(machine.object(h).unwrap().stack(), expected);
        }
        assert_eq!(
            machine.run(h, 8).unwrap().reason,
            HaltReason::BudgetExhausted
        );
        assert_eq!(machine.object(h).unwrap().pc(), 36);
        assert!(machine.object(h).unwrap().stack().is_empty());
    }

    #[test]
    fn paging_host_unavailable_rolls_back_before_the_following_instruction() {
        let h = handle(0);
        let eid = Eid::from_raw(0x7500_2055);
        let mut object = VmObject::new(
            h,
            vec![
                Instruction::encode(0x8b, 1, 0),
                Instruction::encode(0x8b, 4, 0),
            ],
        )
        .unwrap();
        object.internal[0] = eid.raw();
        object.internal[1] = 1;
        object.internal[4] = 4;
        object.page_count = 4;
        object.resident_pages = vec![PageIndex::new(0)];
        object.entry_pages = vec![(eid, PageIndex::new(2))];
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        let mut requests = Vec::new();

        let execution = machine
            .run_with_host_requests(h, 2, |machine, request| {
                let VmHostRequest::Effect(VmEffect::Paging {
                    object,
                    operation,
                    physical,
                    reference,
                    eid,
                    page,
                    was_resolved,
                }) = request
                else {
                    return Err(VmError::MissingHostEffect);
                };
                let request = PagingHostRequest {
                    object,
                    operation,
                    physical,
                    reference,
                    eid,
                    page,
                    was_resolved,
                };
                requests.push(request);
                machine.complete_paging_host_request(request, PagingHostResponse::Unavailable)
            })
            .unwrap();

        assert_eq!(execution.reason, HaltReason::BudgetExhausted);
        assert_eq!(execution.steps, 2);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].operation, PagingHostOperation::Open);
        assert_eq!(requests[0].eid, eid);
        assert_eq!(requests[0].page, PageIndex::new(2));
        assert!(!requests[0].was_resolved);
        assert_eq!(
            machine
                .object(h)
                .unwrap()
                .register(process_register::MISC_VALUE),
            Ok(0)
        );
        assert_eq!(machine.paging_page_references[&PageIndex::new(2)], 0);
        assert!(!machine.paging_resolved_pages.contains(&PageIndex::new(2)));
        assert_eq!(
            machine.object(h).unwrap().stack(),
            &[4],
            "case four must observe the rolled-back count in the next instruction"
        );
    }

    #[test]
    fn paging_host_close_keeps_vm_and_pager_counts_after_texture_pte_eviction() {
        const TEXTURE_SLOT_COUNT: usize = 8;
        let mut pager = Pager::new();
        let texture_eids = (0..=TEXTURE_SLOT_COUNT)
            .map(|index| {
                let page = PageIndex::new(index as u32);
                let eid = Eid::from_raw(0x7500_2055 + (index as u32 * 0x1e));
                pager.register_page(page, []).unwrap();
                pager.bind_page_eid(eid, page).unwrap();
                eid
            })
            .collect::<Vec<_>>();
        let evicted_eid = texture_eids[0];
        let evicted_page = PageIndex::new(0);

        pager.set_current_texture_load_eids(texture_eids.iter().take(TEXTURE_SLOT_COUNT).copied());
        pager.open_eid(evicted_eid).unwrap();
        pager.open_eid(evicted_eid).unwrap();
        for eid in texture_eids
            .iter()
            .take(TEXTURE_SLOT_COUNT)
            .skip(1)
            .copied()
        {
            pager.open_eid(eid).unwrap();
        }

        let resolved_pages = pager.resolved_pages().collect::<Vec<_>>();
        let page_references = pager.page_reference_counts().collect::<Vec<_>>();
        let uncounted_pages = pager.uncounted_pages().collect::<Vec<_>>();
        let mut machine = Machine::new(0);
        machine
            .seed_platform_paging_state_with_uncounted_pages(
                (TEXTURE_SLOT_COUNT + 1) as u32,
                resolved_pages,
                page_references,
                uncounted_pages,
            )
            .unwrap();

        let h = handle(0);
        let mut object = VmObject::new(h, vec![Instruction::encode(0x8b, 1, 0)]).unwrap();
        object.internal[0] = evicted_eid.raw();
        object.internal[1] = 2;
        object.page_count = (TEXTURE_SLOT_COUNT + 1) as u32;
        object.entry_pages = vec![(evicted_eid, evicted_page)];
        machine.insert_object(object).unwrap();

        pager.set_current_texture_load_eids(
            texture_eids
                .iter()
                .skip(1)
                .take(TEXTURE_SLOT_COUNT - 1)
                .copied(),
        );
        let replacement = pager
            .open_eid_with_outcome(texture_eids[TEXTURE_SLOT_COUNT])
            .unwrap();
        assert!(
            replacement
                .invalidated
                .iter()
                .any(|page| page == evicted_page)
        );
        machine
            .apply_platform_paging_open(replacement.page, replacement.invalidated)
            .unwrap();
        assert!(!machine.paging_resolved_pages.contains(&evicted_page));
        assert_eq!(machine.paging_page_references[&evicted_page], 2);

        let execution = machine
            .run_with_host_requests(h, 1, |machine, request| {
                let VmHostRequest::Effect(VmEffect::Paging {
                    object,
                    operation,
                    physical,
                    reference,
                    eid,
                    page,
                    was_resolved,
                }) = request
                else {
                    return Err(VmError::MissingHostEffect);
                };
                let request = PagingHostRequest {
                    object,
                    operation,
                    physical,
                    reference,
                    eid,
                    page,
                    was_resolved,
                };
                assert_eq!(request.operation, PagingHostOperation::Close);
                assert_eq!(request.eid, evicted_eid);
                assert_eq!(request.page, evicted_page);
                assert!(!request.was_resolved);
                assert_eq!(machine.paging_page_references[&evicted_page], 2);

                let outcome = pager.close_eid_retail_with_outcome(request.eid).unwrap();
                assert_eq!(outcome.page, evicted_page);
                assert!(!outcome.decremented);
                assert!(!outcome.unresolved);
                machine.complete_paging_host_request(
                    request,
                    PagingHostResponse::Applied {
                        invalidated: PageInvalidations::NONE,
                    },
                )
            })
            .unwrap();

        assert_eq!(execution.reason, HaltReason::BudgetExhausted);
        assert_eq!(execution.steps, 1);
        assert_eq!(pager.page(evicted_page).unwrap().references, 2);
        assert_eq!(machine.paging_page_references[&evicted_page], 2);
        assert!(!machine.paging_resolved_pages.contains(&evicted_page));
        assert_eq!(
            machine
                .object(h)
                .unwrap()
                .register(process_register::MISC_VALUE),
            Ok(0)
        );
    }

    #[test]
    fn paging_host_open_eviction_rearms_the_displaced_entry_before_case_five() {
        let h = handle(0);
        let eid_a = Eid::from_raw(0x7500_2055);
        let eid_b = Eid::from_raw(0x7500_2073);
        let mut object = VmObject::new(
            h,
            vec![
                // Resolve B and retain its logical entry token.
                Instruction::encode(0x8b, 1, 0),
                // Resolving A replaces B's platform texture slot.
                Instruction::encode(0x8b, 1, 2),
                // Count the retained B token after that replacement.
                Instruction::encode(0x8b, 5, 8),
            ],
        )
        .unwrap();
        object.internal[0] = eid_b.raw();
        object.internal[1] = 1;
        object.internal[2] = eid_a.raw();
        object.internal[5] = 5;
        object.internal[8] = 1;
        object.page_count = 4;
        object.resident_pages = vec![PageIndex::new(0)];
        object.entry_pages = vec![(eid_a, PageIndex::new(2)), (eid_b, PageIndex::new(3))];
        let retained_b = StorageReference::checked(h, StorageRegion::Internal, 7).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        let mut request_count = 0;

        let execution = machine
            .run_with_host_requests(h, 3, |machine, request| {
                let VmHostRequest::Effect(VmEffect::Paging {
                    object,
                    operation,
                    physical,
                    reference,
                    eid,
                    page,
                    was_resolved,
                }) = request
                else {
                    return Err(VmError::MissingHostEffect);
                };
                let request = PagingHostRequest {
                    object,
                    operation,
                    physical,
                    reference,
                    eid,
                    page,
                    was_resolved,
                };
                let response = match request_count {
                    0 => {
                        assert_eq!(request.eid, eid_b);
                        assert_eq!(request.page, PageIndex::new(3));
                        let entry_b = machine.object(h)?.register(process_register::MISC_VALUE)?;
                        machine.object_mut(h)?.internal[7] = entry_b;
                        machine.push(h, retained_b.to_word())?;
                        PagingHostResponse::Applied {
                            invalidated: PageInvalidations::NONE,
                        }
                    }
                    1 => {
                        assert_eq!(request.eid, eid_a);
                        assert_eq!(request.page, PageIndex::new(2));
                        PagingHostResponse::Applied {
                            invalidated: PageInvalidations::one(PageIndex::new(3)),
                        }
                    }
                    _ => return Err(VmError::MismatchedPagingHostResponse),
                };
                request_count += 1;
                machine.complete_paging_host_request(request, response)
            })
            .unwrap();

        assert_eq!(execution.reason, HaltReason::BudgetExhausted);
        assert_eq!(execution.steps, 3);
        assert_eq!(request_count, 2);
        assert!(machine.paging_resolved_pages.contains(&PageIndex::new(2)));
        assert!(!machine.paging_resolved_pages.contains(&PageIndex::new(3)));
        assert_eq!(machine.paging_page_references[&PageIndex::new(2)], 1);
        assert_eq!(machine.paging_page_references[&PageIndex::new(3)], 1);
        assert_eq!(
            machine.object(h).unwrap().stack(),
            &[0],
            "case five must consult live resolution even for a tagged entry token"
        );
    }

    #[test]
    fn paging_host_rejects_non_open_self_and_unknown_evictions() {
        let h = handle(0);
        let eid = Eid::from_raw(0x7500_2055);
        let mut object = VmObject::new(h, vec![control_flow(3, 0, 0, 0, 0)]).unwrap();
        object.page_count = 4;
        object.entry_pages = vec![(eid, PageIndex::new(2))];
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        let mut request = PagingHostRequest {
            object: h,
            operation: PagingHostOperation::Close,
            physical: false,
            reference: 0,
            eid,
            page: PageIndex::new(2),
            was_resolved: false,
        };

        assert_eq!(
            machine.complete_paging_host_request(
                request,
                PagingHostResponse::Applied {
                    invalidated: PageInvalidations::one(PageIndex::new(3)),
                },
            ),
            Err(VmError::MismatchedPagingHostResponse)
        );

        request.operation = PagingHostOperation::Open;
        machine.paging_resolved_pages.insert(request.page);
        assert_eq!(
            machine.complete_paging_host_request(
                request,
                PagingHostResponse::Applied {
                    invalidated: PageInvalidations::one(request.page),
                },
            ),
            Err(VmError::MismatchedPagingHostResponse)
        );
        assert!(machine.paging_resolved_pages.contains(&request.page));

        assert_eq!(
            machine.complete_paging_host_request(
                request,
                PagingHostResponse::Applied {
                    invalidated: PageInvalidations::one(PageIndex::new(9)),
                },
            ),
            Err(VmError::MismatchedPagingHostResponse)
        );
        assert!(machine.paging_resolved_pages.contains(&request.page));
    }

    #[test]
    fn platform_lifecycle_seed_open_and_close_share_vm_reference_state() {
        let h = handle(0);
        let eid_a = Eid::from_raw(0x7500_2055);
        let eid_b = Eid::from_raw(0x7500_2073);
        let mut object = VmObject::new(
            h,
            vec![
                Instruction::encode(0x8b, 4, 0),
                Instruction::encode(0x8b, 4, 0),
            ],
        )
        .unwrap();
        object.internal[4] = 4;
        object.page_count = 4;
        object.resident_pages = vec![PageIndex::new(0)];
        object.entry_pages = vec![(eid_a, PageIndex::new(2)), (eid_b, PageIndex::new(3))];
        let mut machine = Machine::new(0);
        machine
            .seed_platform_paging_state(
                4,
                [PageIndex::new(2)],
                [(PageIndex::new(2), 1), (PageIndex::new(3), 0)],
            )
            .unwrap();
        machine.insert_object(object).unwrap();
        machine.run(h, 1).unwrap();
        assert_eq!(machine.object(h).unwrap().stack(), &[3]);

        machine
            .apply_platform_paging_open(
                PageIndex::new(3),
                PageInvalidations::one(PageIndex::new(2)),
            )
            .unwrap();
        assert!(!machine.paging_resolved_pages.contains(&PageIndex::new(2)));
        assert!(machine.paging_resolved_pages.contains(&PageIndex::new(3)));
        assert_eq!(machine.paging_page_references[&PageIndex::new(2)], 1);
        assert_eq!(machine.paging_page_references[&PageIndex::new(3)], 1);

        machine
            .apply_platform_paging_close(PageIndex::new(2), true, false)
            .unwrap();
        machine.run(h, 1).unwrap();
        assert_eq!(machine.object(h).unwrap().stack(), &[3, 3]);
        assert_eq!(machine.paging_page_references[&PageIndex::new(2)], 0);
        assert_eq!(machine.paging_page_references[&PageIndex::new(3)], 1);
    }

    #[test]
    fn platform_open_applies_both_pager_invalidations_atomically() {
        let first = PageIndex::new(1);
        let second = PageIndex::new(2);
        let requested = PageIndex::new(3);
        let mut machine = Machine::new(0);
        machine
            .seed_platform_paging_state(4, [first, second], [])
            .unwrap();

        machine
            .apply_platform_paging_open(
                requested,
                PageInvalidations::new(Some(first), Some(second)),
            )
            .unwrap();

        assert!(!machine.paging_resolved_pages.contains(&first));
        assert!(!machine.paging_resolved_pages.contains(&second));
        assert!(machine.paging_resolved_pages.contains(&requested));
        assert_eq!(machine.paging_page_references[&requested], 1);

        machine.paging_resolved_pages.insert(first);
        assert_eq!(
            machine.apply_platform_paging_open(
                requested,
                PageInvalidations::new(Some(first), Some(PageIndex::new(9))),
            ),
            Err(VmError::InvalidPlatformPagingPage(PageIndex::new(9)))
        );
        assert!(
            machine.paging_resolved_pages.contains(&first),
            "validation must finish before either invalidation is committed"
        );
    }

    #[test]
    fn platform_cd_reservation_evictions_support_full_runs_and_are_transactional() {
        let victims = (0..4).map(PageIndex::new).collect::<Vec<_>>();
        let mut machine = Machine::new(0);
        machine
            .seed_platform_paging_state(6, victims.iter().copied(), [])
            .unwrap();

        machine.apply_platform_paging_evictions(&victims).unwrap();
        assert!(
            victims
                .iter()
                .all(|page| !machine.paging_resolved_pages.contains(page)),
            "reservation batches are not limited to the two legacy invalidation fields"
        );

        let mut machine = Machine::new(0);
        machine
            .seed_platform_paging_state(6, victims.iter().copied(), [])
            .unwrap();
        let unresolved = PageIndex::new(5);
        let before = machine.clone();
        assert_eq!(
            machine.apply_platform_paging_evictions(&[
                victims[0], victims[1], unresolved, victims[2],
            ]),
            Err(VmError::InvalidPlatformPagingPage(unresolved))
        );
        assert_eq!(
            machine, before,
            "the complete reservation batch must validate before any PTE is re-armed"
        );
    }

    #[test]
    fn program_materialization_resolves_a_queued_page_without_changing_references() {
        let target = PageIndex::new(2);
        let victim = PageIndex::new(3);
        let referenced_victim = PageIndex::new(4);
        let mut machine = Machine::new(0);
        machine
            .seed_platform_paging_state(
                5,
                [victim, referenced_victim],
                [(target, 1), (referenced_victim, 1)],
            )
            .unwrap();
        machine.seed_platform_pending_pages([target]).unwrap();

        let before_rejected_materialization = machine.clone();
        assert_eq!(
            machine.apply_platform_program_materialization(
                target,
                PageInvalidations::one(referenced_victim),
            ),
            Err(VmError::InvalidPlatformPagingPage(referenced_victim))
        );
        assert_eq!(machine, before_rejected_materialization);

        // A stale pending marker on an evicted count-zero PTE must not survive
        // the platform's authoritative materialization result.
        machine.paging_pending_pages.insert(victim);
        machine
            .apply_platform_program_materialization(target, PageInvalidations::one(victim))
            .unwrap();

        assert!(machine.paging_resolved_pages.contains(&target));
        assert!(!machine.paging_pending_pages.contains(&target));
        assert!(!machine.paging_resolved_pages.contains(&victim));
        assert!(!machine.paging_pending_pages.contains(&victim));
        assert_eq!(machine.paging_page_references.get(&target), Some(&1));
        assert_eq!(
            machine.paging_page_references.get(&referenced_victim),
            Some(&1)
        );
        assert_eq!(machine.paging_page_references.get(&victim), None);
        assert_eq!(machine.close_paging_page(target, false), 1);
        assert_eq!(machine.paging_page_references.get(&target), Some(&1));
    }

    #[test]
    fn gool_open_response_applies_both_pager_invalidations() {
        let first = PageIndex::new(1);
        let second = PageIndex::new(2);
        let requested = PageIndex::new(3);
        let mut machine = Machine::new(0);
        machine
            .seed_platform_paging_state(4, [first, second], [])
            .unwrap();
        let request = PagingHostRequest {
            object: handle(0),
            operation: PagingHostOperation::Open,
            physical: false,
            reference: 0,
            eid: Eid::from_raw(0x7500_2055),
            page: requested,
            was_resolved: false,
        };

        machine
            .complete_paging_host_request(
                request,
                PagingHostResponse::Applied {
                    invalidated: PageInvalidations::new(Some(first), Some(second)),
                },
            )
            .unwrap();

        assert!(!machine.paging_resolved_pages.contains(&first));
        assert!(!machine.paging_resolved_pages.contains(&second));
        assert!(machine.paging_resolved_pages.contains(&requested));
    }

    #[test]
    fn paging_rejects_a_self_referential_storage_cell_instead_of_following_it() {
        let h = handle(0);
        let mut object = VmObject::new(h, vec![Instruction::encode(0x8b, 1, 0)]).unwrap();
        let self_reference = StorageReference::checked(h, StorageRegion::Internal, 0)
            .unwrap()
            .to_word();
        object.internal[0] = self_reference;
        object.internal[1] = 1;
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        assert_eq!(
            machine.run(h, 1),
            Err(VmError::InvalidStorageReference(self_reference))
        );
    }

    #[test]
    fn checked_upsert_registers_replacement_paging_metadata() {
        let h = handle(0);
        let eid = Eid::from_raw(0x7500_2055);
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(h, vec![control_flow(3, 0, 0, 0, 0)]).unwrap())
            .unwrap();
        let mut replacement = VmObject::new(h, vec![control_flow(3, 0, 0, 0, 0)]).unwrap();
        replacement.page_count = 7;
        replacement.resident_pages = vec![PageIndex::new(1)];
        replacement.entry_pages = vec![(eid, PageIndex::new(3))];
        machine.upsert_object(replacement).unwrap();

        assert_eq!(machine.paging_page_capacity, 7);
        assert_eq!(machine.entry_pages.get(&eid), Some(&PageIndex::new(3)));
        assert!(machine.paging_baseline_pages.contains(&PageIndex::new(1)));
        assert!(machine.paging_loaded_pages.contains(&PageIndex::new(1)));
        assert!(machine.paging_resolved_pages.contains(&PageIndex::new(1)));
        assert!(!machine.paging_resolved_pages.contains(&PageIndex::new(3)));
        assert_eq!(machine.available_page_count(), 7);
    }

    #[test]
    fn platform_heap_capacity_survives_later_object_and_state_metadata_binds() {
        for physical_page_capacity in [20, 21] {
            let h = handle(0);
            let mut machine = Machine::new(0);
            machine
                .seed_platform_paging_state_with_capacity(
                    PHYSICAL_SLOT_COUNT as u32,
                    physical_page_capacity,
                    std::iter::empty(),
                    std::iter::empty(),
                    std::iter::empty(),
                )
                .unwrap();

            let mut object = VmObject::new(h, vec![control_flow(2, 0, 0, 0, 0)]).unwrap();
            object.page_count = PHYSICAL_SLOT_COUNT as u32;
            object.resident_pages = vec![PageIndex::new(0)];
            machine.insert_object(object).unwrap();
            assert_eq!(machine.paging_page_capacity, physical_page_capacity);

            let state = VmStateProgram::new(
                0,
                GoolState {
                    flags: 0,
                    status_c: 0,
                    external_index: 0,
                    event_pc: GOOL_PC_NONE,
                    transition_pc: GOOL_PC_NONE,
                    code_pc: 0,
                },
                vec![control_flow(2, 0, 0, 0, 0)],
                Vec::new(),
            )
            .unwrap()
            .with_paging_metadata(
                PHYSICAL_SLOT_COUNT as u32,
                [PageIndex::new(1)],
                std::iter::empty(),
            );
            machine.rebind_state_program(h, &state, &[]).unwrap();

            assert_eq!(machine.paging_page_capacity, physical_page_capacity);
            assert_eq!(
                machine.paging_page_capacity_authority,
                PagingCapacityAuthority::PlatformHeap
            );
        }
    }

    #[test]
    fn unseeded_metadata_grows_nominal_capacity_to_physical_slot_limit() {
        let h = handle(0);
        let mut object = VmObject::new(h, vec![control_flow(2, 0, 0, 0, 0)]).unwrap();
        object.page_count = 7;
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        assert_eq!(machine.paging_page_capacity, 7);
        assert_eq!(
            machine.paging_page_capacity_authority,
            PagingCapacityAuthority::ProgramMetadata
        );

        let state = VmStateProgram::new(
            0,
            GoolState {
                flags: 0,
                status_c: 0,
                external_index: 0,
                event_pc: GOOL_PC_NONE,
                transition_pc: GOOL_PC_NONE,
                code_pc: 0,
            },
            vec![control_flow(2, 0, 0, 0, 0)],
            Vec::new(),
        )
        .unwrap()
        .with_paging_metadata(
            PHYSICAL_SLOT_COUNT as u32 + 4,
            std::iter::empty(),
            std::iter::empty(),
        );
        machine.rebind_state_program(h, &state, &[]).unwrap();

        assert_eq!(machine.paging_page_capacity, PHYSICAL_SLOT_COUNT as u32);
        assert_eq!(
            machine.paging_page_capacity_authority,
            PagingCapacityAuthority::ProgramMetadata
        );
    }

    #[test]
    fn rebound_state_registers_later_entry_paging_metadata_at_ref_zero() {
        let h = handle(0);
        let eid = Eid::from_raw(0x7500_2055);
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(h, vec![control_flow(2, 0, 0, 0, 0)]).unwrap())
            .unwrap();
        let target = VmStateProgram::new(
            0,
            GoolState {
                flags: 0,
                status_c: 0,
                external_index: 0,
                event_pc: GOOL_PC_NONE,
                transition_pc: GOOL_PC_NONE,
                code_pc: 0,
            },
            vec![control_flow(2, 0, 0, 0, 0)],
            Vec::new(),
        )
        .unwrap()
        .with_paging_metadata(5, [PageIndex::new(0)], [(eid, PageIndex::new(4))]);

        machine.rebind_state_program(h, &target, &[]).unwrap();

        assert_eq!(machine.entry_pages.get(&eid), Some(&PageIndex::new(4)));
        assert!(machine.paging_loaded_pages.contains(&PageIndex::new(4)));
        assert_eq!(machine.paging_page_references.get(&PageIndex::new(4)), None);
        assert_eq!(machine.available_page_count(), 5);
    }

    #[test]
    fn duplicate_insert_does_not_replace_the_live_object() {
        let h = handle(0);
        let original = VmObject::new(h, vec![control_flow(3, 0, 0, 0, 0)]).unwrap();
        let replacement = VmObject::new(h, vec![Instruction::encode(0xff, 0, 0)]).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(original).unwrap();
        assert_eq!(
            machine.insert_object(replacement),
            Err(VmError::DuplicateObject(h))
        );
        assert_eq!(
            machine.run(h, 1).unwrap().reason,
            HaltReason::BudgetExhausted
        );
    }

    #[test]
    fn checked_remove_object_clears_every_inbound_process_link() {
        let target = handle(0);
        let first = handle(1);
        let second = handle(2);
        let mut first_object = VmObject::new(first, vec![0]).unwrap();
        first_object.set_link(6, Some(target)).unwrap();
        first_object.set_link(7, Some(target)).unwrap();
        let mut second_object = VmObject::new(second, vec![0]).unwrap();
        second_object.set_link(1, Some(target)).unwrap();
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(target, vec![0]).unwrap())
            .unwrap();
        machine.insert_object(first_object).unwrap();
        machine.insert_object(second_object).unwrap();

        assert_eq!(machine.remove_object(target).unwrap().handle(), target);
        assert_eq!(machine.object(target), Err(VmError::UnknownObject(target)));
        assert_eq!(machine.object(first).unwrap().links[6], None);
        assert_eq!(machine.object(first).unwrap().links[7], None);
        assert_eq!(machine.object(second).unwrap().links[1], None);
        assert_eq!(machine.object(first).unwrap().register(6).unwrap(), 0);
        assert_eq!(machine.object(first).unwrap().register(7).unwrap(), 0);
        assert_eq!(machine.object(second).unwrap().register(1).unwrap(), 0);
        assert_eq!(
            machine.remove_object(target),
            Err(VmError::UnknownObject(target))
        );
    }

    #[test]
    fn retail_pool_slot_removal_rejects_invalid_storage_identity_transactionally() {
        let object = handle(0);
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(object, vec![0]).unwrap())
            .unwrap();

        assert_eq!(
            machine.bind_retail_pool_slot(object, u8::MAX),
            Err(VmError::InvalidRetailPoolSlot(u8::MAX))
        );
        assert_eq!(
            machine.remove_object_from_retail_pool_slot(object, u8::MAX),
            Err(VmError::InvalidRetailPoolSlot(u8::MAX))
        );
        assert_eq!(machine.object(object).unwrap().handle(), object);
    }

    #[test]
    fn ordinary_retail_pool_starts_as_the_native_free_chain_and_unlinks_a_binding() {
        let mut machine = Machine::new(0);
        let initial_slots = (0..OBJECT_POOL_CAPACITY)
            .map(|slot| u8::try_from(slot).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(machine.retail_free_pool_slots, initial_slots);
        assert_eq!(
            machine.retail_pool_register_word(0, PROCESS_LINK_PARENT),
            Ok((RETAIL_FREE_LIST_ROOT_REFERENCE, None))
        );
        assert_eq!(
            machine.retail_pool_register_word(0, PROCESS_LINK_SIBLING),
            Ok((retail_pool_slot_reference_word(1), Some(1)))
        );
        assert_eq!(
            machine.retail_pool_register_word(95, PROCESS_LINK_SIBLING),
            Ok((0, None))
        );

        let object = handle(7);
        machine
            .insert_object(VmObject::new(object, vec![0]).unwrap())
            .unwrap();
        machine.bind_retail_pool_slot(object, 7).unwrap();

        assert_eq!(
            machine.retail_free_pool_slots,
            initial_slots
                .into_iter()
                .filter(|slot| *slot != 7)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            machine.retail_pool_register_word(6, PROCESS_LINK_SIBLING),
            Ok((retail_pool_slot_reference_word(8), Some(8))),
            "unlinking an arbitrary free slot must reconnect its predecessor"
        );
    }

    #[test]
    fn ordinary_retail_pool_removal_is_lifo_and_head_binding_reuses_the_slot() {
        let first = handle(0);
        let second = handle(1);
        let replacement = handle(2);
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(first, vec![0]).unwrap())
            .unwrap();
        machine
            .insert_object(VmObject::new(second, vec![0]).unwrap())
            .unwrap();
        machine.bind_retail_pool_slot(first, 0).unwrap();
        machine.bind_retail_pool_slot(second, 1).unwrap();

        machine
            .remove_object_from_retail_pool_slot(first, 0)
            .unwrap();
        assert_eq!(&machine.retail_free_pool_slots[..3], &[0, 2, 3]);
        assert_eq!(
            machine.retail_pool_register_word(0, PROCESS_LINK_PARENT),
            Ok((RETAIL_FREE_LIST_ROOT_REFERENCE, None))
        );
        assert_eq!(
            machine.retail_pool_register_word(0, PROCESS_LINK_SIBLING),
            Ok((retail_pool_slot_reference_word(2), Some(2)))
        );
        assert_eq!(
            machine.retail_pool_register_word(0, PROCESS_LINK_CHILDREN),
            Ok((0, None))
        );

        machine
            .remove_object_from_retail_pool_slot(second, 1)
            .unwrap();
        assert_eq!(&machine.retail_free_pool_slots[..4], &[1, 0, 2, 3]);
        assert_eq!(
            machine.retail_pool_register_word(1, PROCESS_LINK_SIBLING),
            Ok((retail_pool_slot_reference_word(0), Some(0)))
        );

        machine
            .insert_object(VmObject::new(replacement, vec![0]).unwrap())
            .unwrap();
        machine.bind_retail_pool_slot(replacement, 1).unwrap();
        assert_eq!(&machine.retail_free_pool_slots[..3], &[0, 2, 3]);
        assert_eq!(
            machine.live_object_in_retail_pool_slot(1),
            Some(replacement)
        );

        let after_first_binding = machine.clone();
        machine.bind_retail_pool_slot(replacement, 1).unwrap();
        assert_eq!(
            machine, after_first_binding,
            "repeating an identical binding must not pop another free slot"
        );
    }

    #[test]
    fn free_pool_allocator_links_reject_authored_mutation_without_diverging() {
        let target = handle(0);
        let writer = handle(1);
        let dynamic_writer = handle(2);
        let mut writer_object =
            VmObject::new(writer, vec![Instruction::encode(0x11, 0x0e0a, 0x0d02)]).unwrap();
        writer_object.set_link(4, Some(target)).unwrap();
        writer_object.set_register(10, 0).unwrap();
        let mut dynamic_writer_object =
            VmObject::new(dynamic_writer, vec![misc(4, 0, REG0) | (4 << 12)]).unwrap();
        dynamic_writer_object.set_link(4, Some(target)).unwrap();
        dynamic_writer_object
            .set_register(0, (PROCESS_LINK_CHILDREN as u32) << 8)
            .unwrap();
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(target, vec![0]).unwrap())
            .unwrap();
        machine.bind_retail_pool_slot(target, 1).unwrap();
        machine.insert_object(writer_object).unwrap();
        machine.insert_object(dynamic_writer_object).unwrap();
        machine
            .remove_object_from_retail_pool_slot(target, 1)
            .unwrap();
        let free_slots = machine.retail_free_pool_slots.clone();
        let sibling = machine
            .retail_pool_register_word(1, PROCESS_LINK_SIBLING)
            .unwrap();

        assert_eq!(
            machine.run(writer, 1),
            Err(VmError::RetailFreePoolLinkMutation {
                slot: 1,
                register: PROCESS_LINK_SIBLING,
            })
        );
        assert_eq!(machine.retail_free_pool_slots, free_slots);
        assert_eq!(
            machine.retail_pool_register_word(1, PROCESS_LINK_SIBLING),
            Ok(sibling),
            "a rejected stale write must leave both visible storage and allocator order intact"
        );

        machine
            .write_retail_pool_register_word(1, PROCESS_LINK_SIBLING, sibling.0, sibling.1)
            .unwrap();
        assert_eq!(machine.retail_free_pool_slots, free_slots);

        machine.push(dynamic_writer, 0x100).unwrap();
        assert_eq!(
            machine.run(dynamic_writer, 1),
            Err(VmError::RetailFreePoolLinkMutation {
                slot: 1,
                register: PROCESS_LINK_CHILDREN,
            })
        );
        assert_eq!(machine.retail_free_pool_slots, free_slots);
        assert_eq!(
            machine.retail_pool_register_word(1, PROCESS_LINK_CHILDREN),
            Ok((0, None))
        );

        for register in [
            PROCESS_LINK_PARENT,
            PROCESS_LINK_SIBLING,
            PROCESS_LINK_CHILDREN,
        ] {
            let retained = machine.retail_pool_register_word(1, register).unwrap();
            assert_eq!(
                machine.write_retail_pool_register_word(
                    1,
                    register,
                    retained.0 ^ 0x0100,
                    retained.1,
                ),
                Err(VmError::RetailFreePoolLinkMutation { slot: 1, register })
            );
            assert_eq!(
                machine.retail_pool_register_word(1, register),
                Ok(retained),
                "free-list link register {register} must reject mutation atomically"
            );
        }
        assert_eq!(machine.retail_free_pool_slots, free_slots);
    }

    #[test]
    fn rejected_free_pool_vector_write_is_transactional_across_earlier_words() {
        let target = handle(0);
        let mut target_object = VmObject::new(target, vec![0]).unwrap();
        target_object.set_register(0, 0x1111_1100).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(target_object).unwrap();
        machine.bind_retail_pool_slot(target, 1).unwrap();
        machine
            .remove_object_from_retail_pool_slot(target, 1)
            .unwrap();
        let before =
            [0, 1, 2].map(|register| machine.retail_pool_register_word(1, register).unwrap().0);
        let reference = StorageReference::retail_pool_register(1, 0).unwrap();

        assert_eq!(
            machine.write_storage_span3(reference, [0x2222_2200, before[1] ^ 0x100, 0x3333_3300],),
            Err(VmError::RetailFreePoolLinkMutation {
                slot: 1,
                register: PROCESS_LINK_PARENT,
            })
        );
        assert_eq!(
            [0, 1, 2].map(|register| { machine.retail_pool_register_word(1, register).unwrap().0 }),
            before,
            "full vector validation must precede its first retained-slot write"
        );
    }

    #[test]
    fn first_dedicated_player_vector_write_initializes_both_translation_views() {
        let mut machine = Machine::new(0);
        let pool_slot = OBJECT_POOL_CAPACITY as u8;
        let reference =
            StorageReference::retail_pool_register(pool_slot, process_register::TRANSLATION_X)
                .unwrap();
        let translation = [0x1234_5600, 0xfedc_ba00, 0x0102_0300];

        assert_eq!(machine.retired_retail_pool_translation(pool_slot), None);
        machine.write_storage_span3(reference, translation).unwrap();

        assert_eq!(
            machine.read_storage_span3(reference),
            Ok(translation),
            "the persistent player allocation must retain all written process words"
        );
        assert_eq!(
            machine.retired_retail_pool_translation(pool_slot),
            Some(translation.map(u32::cast_signed)),
            "the runtime translation view must be created by the first pre-main write"
        );
    }

    #[test]
    fn rejected_scalar_free_pool_link_write_keeps_the_specific_error_and_value() {
        let target = handle(0);
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(target, vec![0]).unwrap())
            .unwrap();
        machine.bind_retail_pool_slot(target, 1).unwrap();
        machine
            .remove_object_from_retail_pool_slot(target, 1)
            .unwrap();
        let reference = StorageReference::retail_pool_register(1, PROCESS_LINK_PARENT).unwrap();
        let before = machine.read_storage_reference(reference).unwrap();

        assert_eq!(
            machine.write_storage_reference(reference, before ^ 0x100),
            Err(VmError::RetailFreePoolLinkMutation {
                slot: 1,
                register: PROCESS_LINK_PARENT,
            })
        );
        assert_eq!(machine.read_storage_reference(reference), Ok(before));
    }

    #[test]
    fn dedicated_main_pool_slot_never_enters_the_ordinary_free_chain() {
        let main = handle(OBJECT_POOL_CAPACITY as u16);
        let initial_free_slots = (0..OBJECT_POOL_CAPACITY)
            .map(|slot| u8::try_from(slot).unwrap())
            .collect::<Vec<_>>();
        let mut main_object = VmObject::new(main, vec![0]).unwrap();
        main_object
            .set_link(PROCESS_LINK_PARENT, Some(handle(0)))
            .unwrap();
        main_object
            .set_link(PROCESS_LINK_SIBLING, Some(handle(1)))
            .unwrap();
        main_object
            .set_link(PROCESS_LINK_CHILDREN, Some(handle(2)))
            .unwrap();
        let mut machine = Machine::new(0);
        assert!(
            machine.retired_retail_pool_registers[OBJECT_POOL_CAPACITY].is_some(),
            "the separately allocated player has its own initialized link storage"
        );
        machine.insert_object(main_object).unwrap();
        machine
            .bind_retail_pool_slot(main, OBJECT_POOL_CAPACITY as u8)
            .unwrap();
        assert_eq!(machine.retail_free_pool_slots, initial_free_slots);

        let after_first_binding = machine.clone();
        machine
            .bind_retail_pool_slot(main, OBJECT_POOL_CAPACITY as u8)
            .unwrap();
        assert_eq!(machine, after_first_binding);
        machine
            .remove_object_from_retail_pool_slot(main, OBJECT_POOL_CAPACITY as u8)
            .unwrap();

        assert_eq!(machine.retail_free_pool_slots, initial_free_slots);
        for link in [
            PROCESS_LINK_PARENT,
            PROCESS_LINK_SIBLING,
            PROCESS_LINK_CHILDREN,
        ] {
            assert_eq!(
                machine.retail_pool_register_word(OBJECT_POOL_CAPACITY as u8, link),
                Ok((0, None)),
                "native kill explicitly detaches main link {link}"
            );
        }
    }

    #[test]
    fn linked_lea_source_keeps_free_pool_address_and_retargets_on_slot_reuse() {
        let original = handle(0);
        let actor = handle(1);
        let replacement = handle(2);
        let original_value = 0x1111_1100;
        let replacement_value = 0x2222_2200;
        let mut original_object = VmObject::new(original, vec![0]).unwrap();
        original_object.set_register(8, original_value).unwrap();
        let mut actor_object =
            VmObject::new(actor, vec![Instruction::encode(0x14, 0x0d08, 0x0e0a)]).unwrap();
        actor_object.set_link(4, Some(original)).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(original_object).unwrap();
        machine.bind_retail_pool_slot(original, 1).unwrap();
        machine.insert_object(actor_object).unwrap();
        machine
            .remove_object_from_retail_pool_slot(original, 1)
            .unwrap();

        machine.run(actor, 1).unwrap();
        let word = machine.object(actor).unwrap().register(10).unwrap();
        let reference = StorageReference::from_word(word).unwrap();
        assert_eq!(reference.object(), None);
        assert_eq!(reference.retail_pool_slot(), Some(1));
        assert_eq!(reference.region(), StorageRegion::Register);
        assert_eq!(reference.index(), 8);
        assert_eq!(
            machine.read_storage_reference(reference),
            Ok(original_value)
        );
        assert_eq!(
            StorageReference::from_word(reference.to_word()),
            Some(reference)
        );

        let mut replacement_object = VmObject::new(replacement, vec![0]).unwrap();
        replacement_object
            .set_register(8, replacement_value)
            .unwrap();
        machine.insert_object(replacement_object).unwrap();
        machine.bind_retail_pool_slot(replacement, 1).unwrap();
        assert_eq!(
            machine.read_storage_reference(reference),
            Ok(replacement_value),
            "a stored native address follows reuse of its physical static-pool slot"
        );
    }

    #[test]
    fn inactive_dedicated_player_link_keeps_slot_ninety_six_address() {
        let main = handle(0);
        let actor = handle(1);
        let replacement = handle(3);
        let main_slot = OBJECT_POOL_CAPACITY as u8;
        let inactive_token = handle(OBJECT_POOL_CAPACITY as u16);
        let mut main_object = VmObject::new(main, vec![0]).unwrap();
        main_object.set_register(8, 0x1111_1100).unwrap();
        let mut actor_object =
            VmObject::new(actor, vec![Instruction::encode(0x14, 0x0d48, 0x0e0a)]).unwrap();
        actor_object
            .set_retail_pool_link(5, inactive_token, main_slot)
            .unwrap();
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(inactive_token, vec![0]).unwrap())
            .unwrap();
        machine.bind_retail_pool_slot(inactive_token, 7).unwrap();
        machine.insert_object(actor_object).unwrap();

        assert_eq!(machine.resolve_process_link(actor, 5), Ok(None));
        machine.run(actor, 1).unwrap();
        let reference =
            StorageReference::from_word(machine.object(actor).unwrap().register(10).unwrap())
                .unwrap();
        assert_eq!(reference.retail_pool_slot(), Some(main_slot));
        assert_eq!(
            machine.read_storage_reference(reference),
            Ok(0),
            "the player allocation is addressable before its first logical main object"
        );

        machine.insert_object(main_object).unwrap();
        machine.bind_retail_pool_slot(main, main_slot).unwrap();
        assert_eq!(machine.resolve_process_link(actor, 5), Ok(Some(main)));
        assert_eq!(machine.read_storage_reference(reference), Ok(0x1111_1100));
        machine
            .remove_object_from_retail_pool_slot(main, main_slot)
            .unwrap();
        assert_eq!(machine.resolve_process_link(actor, 5), Ok(None));
        assert_eq!(machine.read_storage_reference(reference), Ok(0x1111_1100));

        let mut replacement_object = VmObject::new(replacement, vec![0]).unwrap();
        replacement_object.set_register(8, 0x2222_2200).unwrap();
        machine.insert_object(replacement_object).unwrap();
        machine
            .bind_retail_pool_slot(replacement, main_slot)
            .unwrap();
        assert_eq!(
            machine.resolve_process_link(actor, 5),
            Ok(Some(replacement))
        );
        assert_eq!(machine.read_storage_reference(reference), Ok(0x2222_2200));
    }

    #[test]
    fn linked_lea_live_source_uses_pool_address_before_reclamation() {
        let target = handle(0);
        let actor = handle(1);
        let value = 0x3456_7800;
        let mut target_object = VmObject::new(target, vec![0]).unwrap();
        target_object.set_register(9, value).unwrap();
        let mut actor_object =
            VmObject::new(actor, vec![Instruction::encode(0x14, 0x0d09, 0x0e0a)]).unwrap();
        // Deliberately establish the typed link before either object is bound;
        // LEA must still discover the target's current physical allocation.
        actor_object.set_link(4, Some(target)).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(target_object).unwrap();
        machine.bind_retail_pool_slot(target, 7).unwrap();
        machine.insert_object(actor_object).unwrap();

        machine.run(actor, 1).unwrap();
        let reference =
            StorageReference::from_word(machine.object(actor).unwrap().register(10).unwrap())
                .unwrap();
        assert_eq!(reference.retail_pool_slot(), Some(7));
        assert_eq!(machine.read_storage_reference(reference), Ok(value));
        machine
            .remove_object_from_retail_pool_slot(target, 7)
            .unwrap();
        assert_eq!(
            machine.read_storage_reference(reference),
            Ok(value),
            "reclamation leaves the native address bound to initialized slot storage"
        );
    }

    #[test]
    fn linked_lea_destination_writes_reclaimed_pool_storage() {
        let target = handle(0);
        let actor = handle(1);
        let mut actor_object =
            VmObject::new(actor, vec![Instruction::encode(0x14, 0x0e0a, 0x0d08)]).unwrap();
        actor_object.set_link(4, Some(target)).unwrap();
        actor_object.set_register(10, 0x1234_5600).unwrap();
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(target, vec![0]).unwrap())
            .unwrap();
        machine.bind_retail_pool_slot(target, 3).unwrap();
        machine.insert_object(actor_object).unwrap();
        machine
            .remove_object_from_retail_pool_slot(target, 3)
            .unwrap();

        machine.run(actor, 1).unwrap();
        let stored_word = machine.retail_pool_register_word(3, 8).unwrap().0;
        let stored_reference = StorageReference::from_word(stored_word).unwrap();
        assert_eq!(stored_reference.object(), Some(actor));
        assert_eq!(stored_reference.region(), StorageRegion::Register);
        assert_eq!(stored_reference.index(), 10);
        assert_eq!(
            machine.read_storage_reference(stored_reference),
            Ok(0x1234_5600)
        );
    }

    #[test]
    fn retail_pool_slot_binding_and_removal_reject_identity_aliases_transactionally() {
        let first = handle(0);
        let second = handle(1);
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(first, vec![0]).unwrap())
            .unwrap();
        machine
            .insert_object(VmObject::new(second, vec![0]).unwrap())
            .unwrap();
        machine.bind_retail_pool_slot(first, 7).unwrap();
        let bound_snapshot = machine.clone();

        assert_eq!(
            machine.bind_retail_pool_slot(second, 7),
            Err(VmError::RetailPoolSlotOccupied {
                slot: 7,
                object: first,
            })
        );
        assert_eq!(machine, bound_snapshot);
        assert_eq!(
            machine.bind_retail_pool_slot(first, 8),
            Err(VmError::RetailPoolSlotMismatch {
                object: first,
                bound: Some(7),
                requested: 8,
            })
        );
        assert_eq!(machine, bound_snapshot);
        assert_eq!(
            machine.remove_object_from_retail_pool_slot(first, 8),
            Err(VmError::RetailPoolSlotMismatch {
                object: first,
                bound: Some(7),
                requested: 8,
            })
        );
        assert_eq!(machine, bound_snapshot);
        machine.bind_retail_pool_slot(first, 7).unwrap();
        assert_eq!(machine, bound_snapshot);
        assert_eq!(machine.object(first).unwrap().handle(), first);
        assert_eq!(machine.object(second).unwrap().handle(), second);
        assert_eq!(
            machine.retail_pool_slots_by_object[usize::from(first.get())],
            Some(7)
        );
        assert_eq!(
            machine.retail_pool_slots_by_object[usize::from(second.get())],
            None
        );
    }

    #[test]
    fn preexisting_link_keeps_physical_pool_identity_across_compact_and_slot_reuse() {
        let original = handle(0);
        let holder = handle(1);
        let replacement = handle(2);
        let mut original_object = VmObject::new(original, vec![0]).unwrap();
        original_object.set_register(8, 0x1111_1100).unwrap();
        let mut holder_object = VmObject::new(holder, vec![0]).unwrap();
        holder_object.set_link(4, Some(original)).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(original_object).unwrap();
        machine.bind_retail_pool_slot(original, 1).unwrap();
        machine.insert_object(holder_object).unwrap();
        machine
            .remove_object_from_retail_pool_slot(original, 1)
            .unwrap();

        assert_eq!(machine.object(holder).unwrap().links[4], Some(original));
        assert_eq!(
            machine.object(holder).unwrap().register_pool_slot(4),
            Ok(Some(1))
        );
        assert_eq!(
            machine
                .read_operand(
                    holder,
                    Operand::LinkRegister {
                        link: 4,
                        register: 8,
                    },
                )
                .unwrap(),
            0x1111_1100
        );

        let mut unrelated_reuse = VmObject::new(original, vec![0]).unwrap();
        unrelated_reuse.set_register(8, 0x4444_4400).unwrap();
        machine.insert_object(unrelated_reuse).unwrap();
        machine.bind_retail_pool_slot(original, 4).unwrap();
        assert_eq!(
            machine
                .read_operand(
                    holder,
                    Operand::LinkRegister {
                        link: 4,
                        register: 8,
                    },
                )
                .unwrap(),
            0x1111_1100,
            "compact-handle reuse in another slot must not retarget the old pointer"
        );

        let mut replacement_object = VmObject::new(replacement, vec![0]).unwrap();
        replacement_object.set_register(8, 0x2222_2200).unwrap();
        machine.insert_object(replacement_object).unwrap();
        machine.bind_retail_pool_slot(replacement, 1).unwrap();
        assert_eq!(
            machine.resolve_process_link(holder, 4),
            Ok(Some(replacement))
        );
        assert_eq!(
            machine
                .read_operand(
                    holder,
                    Operand::LinkRegister {
                        link: 4,
                        register: 8,
                    },
                )
                .unwrap(),
            0x2222_2200,
            "reuse of the same physical slot must retarget the old pointer"
        );
    }

    #[test]
    fn null_link_read_and_killed_slot_zero_are_distinct() {
        let target = handle(0);
        let holder = handle(1);
        let mut holder_object = VmObject::new(holder, vec![0]).unwrap();
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(target, vec![0]).unwrap())
            .unwrap();
        machine.bind_retail_pool_slot(target, 1).unwrap();
        machine.insert_object(holder_object.clone()).unwrap();
        assert_eq!(
            machine
                .read_operand(
                    holder,
                    Operand::LinkRegister {
                        link: 4,
                        register: 8,
                    },
                )
                .unwrap(),
            NULL_INPUT_VALUE
        );

        holder_object.set_link(4, Some(target)).unwrap();
        *machine.object_mut(holder).unwrap() = holder_object;
        machine
            .remove_object_from_retail_pool_slot(target, 1)
            .unwrap();
        assert_eq!(
            machine
                .read_operand(
                    holder,
                    Operand::LinkRegister {
                        link: 4,
                        register: 8,
                    },
                )
                .unwrap(),
            0,
            "a non-null free-slot pointer reads retained zero, not the null compatibility word"
        );
    }

    #[test]
    fn linked_mov_updates_retired_register_and_translation_storage() {
        let target = handle(0);
        let writer = handle(1);
        let value = 0x8765_4300;
        let mut writer_object =
            VmObject::new(writer, vec![Instruction::encode(0x11, 0x0e0a, 0x0d08)]).unwrap();
        writer_object.set_link(4, Some(target)).unwrap();
        writer_object.set_register(10, value).unwrap();
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(target, vec![0]).unwrap())
            .unwrap();
        machine.bind_retail_pool_slot(target, 1).unwrap();
        machine.insert_object(writer_object).unwrap();
        machine
            .remove_object_from_retail_pool_slot(target, 1)
            .unwrap();

        machine.run(writer, 1).unwrap();
        assert_eq!(machine.retail_pool_register_word(1, 8), Ok((value, None)));
        assert_eq!(
            machine.retired_retail_pool_translation(1),
            Some([value as i32, 0, 0])
        );
    }

    #[test]
    fn dynamic_retired_register_store_and_load_preserve_nested_pool_pointer() {
        let retired = handle(0);
        let pointee = handle(1);
        let actor = handle(2);
        let pointee_word = CollisionObjectReference::new(pointee).to_word();
        let old_pointee_value = 0x1111_1100;
        let new_pointee_value = 0x3333_3300;
        let mut pointee_object = VmObject::new(pointee, vec![0]).unwrap();
        pointee_object.set_register(8, old_pointee_value).unwrap();
        let mut actor_object = VmObject::new(
            actor,
            vec![
                misc(4, 0, REG0) | (4 << 12),
                misc(3, 0, REG0) | (4 << 12),
                Instruction::encode(0x11, STACK, 0x0e06),
                Instruction::encode(0x11, 0x0d88, 0x0e17),
            ],
        )
        .unwrap();
        actor_object.set_link(4, Some(retired)).unwrap();
        actor_object.set_register(0, 5 << 8).unwrap();
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(retired, vec![0]).unwrap())
            .unwrap();
        machine.bind_retail_pool_slot(retired, 1).unwrap();
        machine.insert_object(pointee_object).unwrap();
        machine.bind_retail_pool_slot(pointee, 2).unwrap();
        machine.insert_object(actor_object).unwrap();
        machine
            .push_with_pool_slot(actor, pointee_word, Some(2))
            .unwrap();
        machine
            .remove_object_from_retail_pool_slot(retired, 1)
            .unwrap();

        machine.run(actor, 1).unwrap();
        assert_eq!(
            machine.retail_pool_register_word(1, 5),
            Ok((pointee_word, Some(2)))
        );

        machine
            .remove_object_from_retail_pool_slot(pointee, 2)
            .unwrap();
        let mut compact_reuse = VmObject::new(pointee, vec![0]).unwrap();
        compact_reuse.set_register(8, new_pointee_value).unwrap();
        machine.insert_object(compact_reuse).unwrap();
        machine.bind_retail_pool_slot(pointee, 3).unwrap();
        machine.run(actor, 3).unwrap();

        let actor_object = machine.object(actor).unwrap();
        assert_eq!(actor_object.register_pool_slot(6), Ok(Some(2)));
        assert_eq!(actor_object.register(23), Ok(old_pointee_value));
    }

    #[test]
    fn object_pointer_equality_uses_physical_pool_identity() {
        let original = handle(0);
        let actor = handle(1);
        let replacement = handle(2);
        let original_word = CollisionObjectReference::new(original).to_word();
        let replacement_word = CollisionObjectReference::new(replacement).to_word();
        let mut actor_object = VmObject::new(
            actor,
            vec![
                Instruction::encode(0x04, 0x0e0a, 0x0e0b),
                Instruction::encode(0x04, 0x0e0a, 0x0e0c),
            ],
        )
        .unwrap();
        actor_object.set_register(10, original_word).unwrap();
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(original, vec![0]).unwrap())
            .unwrap();
        machine.bind_retail_pool_slot(original, 1).unwrap();
        machine.insert_object(actor_object).unwrap();
        machine
            .remove_object_from_retail_pool_slot(original, 1)
            .unwrap();

        machine
            .insert_object(VmObject::new(original, vec![0]).unwrap())
            .unwrap();
        machine.bind_retail_pool_slot(original, 4).unwrap();
        machine
            .insert_object(VmObject::new(replacement, vec![0]).unwrap())
            .unwrap();
        machine.bind_retail_pool_slot(replacement, 1).unwrap();
        machine
            .object_mut(actor)
            .unwrap()
            .set_register(11, original_word)
            .unwrap();
        machine
            .object_mut(actor)
            .unwrap()
            .set_register(12, replacement_word)
            .unwrap();

        machine.run(actor, 2).unwrap();
        assert_eq!(
            machine.object(actor).unwrap().stack(),
            &[0, 1],
            "the same compact token in another slot differs, while different tokens in one slot compare equal"
        );
    }

    #[test]
    fn mov_through_internal_storage_preserves_pool_pointer_identity() {
        let target = handle(0);
        let actor = handle(1);
        let pointer = CollisionObjectReference::new(target).to_word();
        let mut actor_object = VmObject::new(
            actor,
            vec![
                Instruction::encode(0x11, 0x0e0a, 7),
                Instruction::encode(0x04, 7, 0x0e0a),
            ],
        )
        .unwrap();
        actor_object.set_register(10, pointer).unwrap();
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(target, vec![0]).unwrap())
            .unwrap();
        machine.bind_retail_pool_slot(target, 5).unwrap();
        machine.insert_object(actor_object).unwrap();
        machine
            .remove_object_from_retail_pool_slot(target, 5)
            .unwrap();

        machine.run(actor, 2).unwrap();
        let actor = machine.object(actor).unwrap();
        assert_eq!(actor.internal[7], pointer);
        assert_eq!(actor.internal_pool_slots[7], Some(5));
        assert_eq!(actor.stack(), &[1]);
    }

    #[test]
    fn direct_table_overwrite_and_state_rebind_clear_pointer_provenance() {
        let h = handle(0);
        let mut object = VmObject::new(h, vec![0]).unwrap();
        object.internal_pool_slots[0] = Some(5);
        object.external_pool_slots[0] = Some(5);

        object.set_internal(0, 0x1111_1100).unwrap();
        object.set_external(0, 0x2222_2200).unwrap();
        assert_eq!(object.internal_pool_slots[0], None);
        assert_eq!(object.external_pool_slots[0], None);

        object.external_pool_slots[0] = Some(5);
        let target = VmStateProgram::new(
            0,
            GoolState {
                flags: 0,
                status_c: 0,
                external_index: 0,
                event_pc: GOOL_PC_NONE,
                transition_pc: GOOL_PC_NONE,
                code_pc: GOOL_PC_NONE,
            },
            Vec::new(),
            vec![0x3333_3300],
        )
        .unwrap();
        object.rebind_state_program(&target, &[], 0).unwrap();

        assert_eq!(object.external[0], 0x3333_3300);
        assert!(object.external_pool_slots.iter().all(Option::is_none));
    }

    #[test]
    fn object_argument_decoders_follow_pool_identity_after_compact_reuse() {
        let original = handle(0);
        let actor = handle(1);
        let replacement = handle(2);
        let pointer = CollisionObjectReference::new(original).to_word();
        let mut actor_object = VmObject::new(actor, vec![0]).unwrap();
        actor_object.set_register(10, pointer).unwrap();
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(original, vec![0]).unwrap())
            .unwrap();
        machine.bind_retail_pool_slot(original, 5).unwrap();
        machine.insert_object(actor_object).unwrap();
        machine
            .remove_object_from_retail_pool_slot(original, 5)
            .unwrap();

        machine
            .insert_object(VmObject::new(original, vec![0]).unwrap())
            .unwrap();
        machine.bind_retail_pool_slot(original, 6).unwrap();
        machine
            .insert_object(VmObject::new(replacement, vec![0]).unwrap())
            .unwrap();
        machine.bind_retail_pool_slot(replacement, 5).unwrap();
        let reference = StorageReference::checked(actor, StorageRegion::Register, 10).unwrap();

        assert_eq!(
            machine.decode_misc_object_argument(reference),
            Ok(replacement)
        );
        assert_eq!(
            machine.decode_audio_object_argument(reference),
            Ok(Some(replacement))
        );
    }

    #[test]
    fn tagged_global_captures_physical_pool_slot_when_written_not_when_read() {
        let object = handle(0);
        let tagged = CollisionObjectReference::new(object).to_word();
        let mut machine = Machine::new(1);
        machine
            .insert_object(VmObject::new(object, vec![0]).unwrap())
            .unwrap();
        machine.bind_retail_pool_slot(object, 7).unwrap();
        machine.set_global_word(0, tagged).unwrap();
        assert_eq!(machine.retail_global_pool_slot(0), Ok(Some(7)));

        machine
            .remove_object_from_retail_pool_slot(object, 7)
            .unwrap();
        machine
            .insert_object(VmObject::new(object, vec![0]).unwrap())
            .unwrap();
        machine.bind_retail_pool_slot(object, 8).unwrap();
        assert_eq!(
            machine.retail_global_pool_slot(0),
            Ok(Some(7)),
            "compact-handle reuse must not mutate an unchanged native pointer"
        );

        machine.set_global_word(0, tagged).unwrap();
        assert_eq!(machine.retail_global_pool_slot(0), Ok(Some(8)));
        machine.set_global_word(0, 0).unwrap();
        assert_eq!(machine.retail_global_pool_slot(0), Ok(None));
    }

    fn global_pool_pointer_reader(handle: ObjectHandle) -> VmObject {
        VmObject::new(
            handle,
            vec![
                // GLBR global six (`fruit_hud`), then the exact Jaws FruiC
                // state-12 pointer copy and linked trans.x read.
                0x1fbe_0806,
                Instruction::encode(0x11, STACK, 0x0e04),
                Instruction::encode(0x11, 0x0d08, 0x0e17),
            ],
        )
        .unwrap()
    }

    #[test]
    fn global_pool_pointer_reads_retired_static_process_storage_after_compact_reuse() {
        let fruit = handle(1);
        let reader = handle(7);
        let mut fruit_object = VmObject::new(fruit, vec![0]).unwrap();
        fruit_object.set_register(8, 0xffff_3800).unwrap();
        let mut machine = Machine::new(7);
        machine.insert_object(fruit_object).unwrap();
        machine.bind_retail_pool_slot(fruit, 1).unwrap();
        machine
            .set_global_word(6, CollisionObjectReference::new(fruit).to_word())
            .unwrap();
        machine
            .remove_object_from_retail_pool_slot(fruit, 1)
            .unwrap();

        // Compact handle one is independently reused twice in the legal Jaws
        // route. Its later pool identity and register contents must not alter
        // the unchanged native pointer stored in global six.
        let mut unrelated = VmObject::new(fruit, vec![0]).unwrap();
        unrelated.set_register(8, 0x00e8_0700).unwrap();
        machine.insert_object(unrelated).unwrap();
        machine.bind_retail_pool_slot(fruit, 4).unwrap();
        machine
            .remove_object_from_retail_pool_slot(fruit, 4)
            .unwrap();

        machine
            .insert_object(global_pool_pointer_reader(reader))
            .unwrap();
        machine.run(reader, 3).unwrap();

        let reader = machine.object(reader).unwrap();
        assert_eq!(reader.register(23), Ok(0xffff_3800));
        assert_eq!(reader.register_pool_slot(4), Ok(Some(1)));
    }

    #[test]
    fn global_pool_pointer_observes_a_replacement_in_the_same_physical_slot() {
        let fruit = handle(1);
        let replacement = handle(2);
        let reader = handle(7);
        let mut fruit_object = VmObject::new(fruit, vec![0]).unwrap();
        fruit_object.set_register(8, 0xffff_3800).unwrap();
        let mut machine = Machine::new(7);
        machine.insert_object(fruit_object).unwrap();
        machine.bind_retail_pool_slot(fruit, 1).unwrap();
        machine
            .set_global_word(6, CollisionObjectReference::new(fruit).to_word())
            .unwrap();
        machine
            .remove_object_from_retail_pool_slot(fruit, 1)
            .unwrap();

        let mut replacement_object = VmObject::new(replacement, vec![0]).unwrap();
        replacement_object.set_register(8, 0x1234_5600).unwrap();
        machine.insert_object(replacement_object).unwrap();
        machine.bind_retail_pool_slot(replacement, 1).unwrap();
        machine
            .insert_object(global_pool_pointer_reader(reader))
            .unwrap();
        machine.run(reader, 3).unwrap();

        assert_eq!(
            machine.object(reader).unwrap().register(23),
            Ok(0x1234_5600)
        );
    }

    #[test]
    fn reclaimed_pool_storage_is_selectively_initialized_in_place() {
        let original = handle(0);
        let replacement = handle(1);
        let retained = [
            (process_register::TRANSLATION_X, 0x1111_1100),
            (process_register::MISC_VALUE, 0x2222_2200),
            (process_register::ANIMATION_STAMP, 0x3333_3300),
            (process_register::ANIMATION_COUNTER, 0x4444_4400),
            (process_register::FLOOR_Y, 0x5555_5500),
            (process_register::INVINCIBILITY_STAMP, 0x6666_6600),
            (process_register::FLOOR_IMPACT_VELOCITY, 0x7777_7700),
            (process_register::EVENT, 0x8888_8800),
            (process_register::CAMERA_ZOOM, 0x9999_9900),
            (process_register::ANGULAR_VELOCITY_Y, 0xaaaa_aa00),
            (process_register::UNKNOWN_150, 0xbbbb_bb00),
            (process_register::UNKNOWN_154, 0xcccc_cc00),
        ];
        let reset = [
            process_register::MISC_A_X,
            process_register::STATUS_B,
            process_register::PID_FLAGS,
            process_register::STACK_POINTER,
            process_register::PROGRAM_COUNTER,
            process_register::FRAME_POINTER,
            process_register::TRANSITION_POINTER,
            process_register::EVENT_POINTER,
            process_register::ACK,
            process_register::ANIMATION_SEQUENCE,
            process_register::ANIMATION_FRAME,
            process_register::PATH_LENGTH,
            process_register::SPEED,
            process_register::INVINCIBILITY_STATE,
            process_register::FLOOR_IMPACT_STAMP,
            process_register::SIZE,
            process_register::HOTSPOT_SIZE,
        ];
        let mut original_object = VmObject::new(original, vec![0]).unwrap();
        for (register, value) in retained {
            original_object.set_register(register, value).unwrap();
        }
        for register in reset {
            original_object.set_register(register, 0xdead_beef).unwrap();
        }
        let mut machine = Machine::new(0);
        machine.insert_object(original_object).unwrap();
        machine.bind_retail_pool_slot(original, 1).unwrap();
        machine
            .remove_object_from_retail_pool_slot(original, 1)
            .unwrap();

        let mut replacement_object = VmObject::new(replacement, vec![0]).unwrap();
        replacement_object
            .set_register(process_register::STATUS_C, 0x1234)
            .unwrap();
        replacement_object
            .set_register(process_register::STATE_FLAGS, 0x5678)
            .unwrap();
        machine
            .seed_retail_pool_slot_storage(1, &mut replacement_object)
            .unwrap();
        replacement_object.initialize_retail_process(7, 99).unwrap();

        for (register, value) in retained {
            assert_eq!(
                replacement_object.register(register),
                Ok(value),
                "register {register} is not written by native Init"
            );
        }
        for register in reset {
            assert_eq!(
                replacement_object.register(register),
                Ok(0),
                "register {register} must be cleared by native Init"
            );
        }
        assert_eq!(
            replacement_object.register(process_register::STATUS_C),
            Ok(0x1234)
        );
        assert_eq!(
            replacement_object.register(process_register::STATE_FLAGS),
            Ok(0x5678)
        );
        assert_eq!(
            replacement_object.register(process_register::SUBTYPE),
            Ok(7)
        );
        assert_eq!(
            replacement_object.register(process_register::STATE_STAMP),
            Ok(99)
        );
        assert_eq!(
            replacement_object.register(process_register::VOICE_ID),
            Ok((-2_i32) as u32)
        );
    }

    #[test]
    fn checked_remove_rejects_an_active_synchronous_event_frame() {
        let h = handle(0);
        let mut object = VmObject::new(h, vec![0]).unwrap();
        object.global_code = vec![control_flow(2, 0, 0, 0, 0)];
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        let depth = machine
            .begin_synchronous_event_frame(
                h,
                CodeAddress {
                    segment: CodeSegment::Global,
                    pc: 0,
                },
                &[],
                None,
                ReturnBehavior::Interrupt {
                    previous_animation_wait: None,
                },
            )
            .unwrap();

        assert_eq!(
            machine.remove_object(h),
            Err(VmError::ActiveEventInvocation(h))
        );
        machine.unwind_synchronous_event_frame(h, depth).unwrap();
        assert_eq!(machine.remove_object(h).unwrap().handle(), h);
    }

    fn entity_with_path(points: Vec<ZoneEntityPathPoint>) -> ZoneEntity {
        ZoneEntity {
            serialized_parent: crust_formats::binary::EntryRef::from_raw(0),
            spawn_flags: 0,
            group: 0,
            id: 1,
            initializer: [0; 3],
            executable: 0,
            subtype: 0,
            path_points: points,
        }
    }

    fn transform_vectors_instruction(
        suboperation: u8,
        input_vector: u8,
        output_vector: u8,
        operand: u16,
    ) -> u32 {
        let packed = u16::from(input_vector)
            | (u16::from(output_vector) << 3)
            | (u16::from(suboperation) << 6);
        Instruction::encode(0x85, packed, operand)
    }

    #[test]
    fn special_object_camera_matches_native_direct_matrix_and_bob() {
        let ordinary = RetailTransformVectorsCamera::from_retail_pose([1, 2, 3], [17, 29, 41], 500);
        assert_eq!(ordinary.for_object_display(0, 0), ordinary);

        let special = ordinary.for_object_display(0x1000, 0);
        let sine = Angle12::new(125).sin_q12();
        let cosine = Angle12::new(125).cos_q12();
        assert_eq!(special.translation, [0, 952_800, 6_144_000]);
        assert_eq!(special.rotation_yxz, [-125, 0, 0]);
        assert_eq!(
            special.rotation_matrix,
            [
                [0x1000, 0, 0],
                [
                    0,
                    ((-5 * i32::from(cosine)) >> 3) as i16,
                    ((5 * i32::from(sine)) >> 3) as i16,
                ],
                [0, sine.wrapping_neg(), cosine.wrapping_neg()],
            ]
        );
        assert_eq!(
            ordinary.for_object_display(0x1000, 64).translation,
            [0, 901_600, 6_144_000]
        );
        assert_eq!(
            ordinary.for_object_display(0x1000, 128),
            special,
            "the native triangular bob repeats every 128 frame stamps"
        );
    }

    #[test]
    fn transform_vectors_projects_and_scales_the_translated_operand_vector() {
        let h = handle(0);
        let mut object =
            VmObject::new(h, vec![transform_vectors_instruction(1, 0, 1, 20)]).unwrap();
        object.initialize_retail_process(0, 0).unwrap();
        object
            .set_process_vector(0, [25_600, 12_800, -256_000])
            .unwrap();
        object.internal[20..23].copy_from_slice(&[1_000, 2_000, 3_000]);
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        let camera = RetailTransformVectorsCamera::from_retail_pose([0; 3], [0; 3], 500);
        assert_eq!(
            camera.rotation_matrix,
            [[0x1000, 0, 0], [0, -0x0a00, 0], [0, 0, -0x1000]]
        );
        machine.set_transform_vectors_camera(camera);

        assert_eq!(
            machine.run(h, 1).unwrap().reason,
            HaltReason::BudgetExhausted
        );
        let object = machine.object(h).unwrap();
        assert_eq!(object.process_vector(1), Ok([12_800, 4_096, 256_000]));
        assert_eq!(&object.internal[20..23], &[280, 560, 840]);
    }

    #[test]
    fn transform_vectors_aims_velocity_in_target_rotation_plane() {
        let h = handle(0);
        let mut object =
            VmObject::new(h, vec![transform_vectors_instruction(2, 3, 0, 0x804)]).unwrap();
        object.initialize_retail_process(0, 0).unwrap();
        object.set_process_vector(3, [99, 77, 55]).unwrap();
        object
            .set_register(process_register::MISC_B_X, 0x400)
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.run(h, 1).unwrap();
        let object = machine.object(h).unwrap();
        assert_eq!(object.process_vector(3), Ok([1_024, 77, 0]));
        assert_eq!(object.register(process_register::SPEED), Ok(1_024));

        let h = handle(1);
        let mut object =
            VmObject::new(h, vec![transform_vectors_instruction(2, 3, 0, 0x804)]).unwrap();
        object.initialize_retail_process(0, 0).unwrap();
        object.set_process_vector(3, [99, 77, 55]).unwrap();
        object
            .set_register(process_register::STATUS_B, 0x0020_0200)
            .unwrap();
        machine.insert_object(object).unwrap();
        machine.run(h, 1).unwrap();
        assert_eq!(
            machine.object(h).unwrap().process_vector(3),
            Ok([0, 1_024, 55])
        );
    }

    #[test]
    fn transform_vectors_subop_four_applies_translation_rotation_and_scale() {
        let h = handle(0);
        let mut object =
            VmObject::new(h, vec![transform_vectors_instruction(4, 0, 5, 0x0a03)]).unwrap();
        object.initialize_retail_process(0, 0).unwrap();
        object
            .set_retail_transform(RetailTransform {
                translation: [100, 200, 300],
                rotation_yxz: [0, 0x400, 0],
                scale: [0x2000, 0x1000, 0x1000],
            })
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.run(h, 0).unwrap();
        let base_stack_len = machine.object(h).unwrap().stack().len();
        machine.push(h, 16).unwrap();
        machine.push(h, 32).unwrap();

        machine.run(h, 1).unwrap();
        let object = machine.object(h).unwrap();
        assert_eq!(object.process_vector(5), Ok([148, 232, 268]));
        assert_eq!(object.stack().len(), base_stack_len);
    }

    #[test]
    fn transform_vectors_subop_five_matches_n_sanity_stack_contract() {
        let h = handle(0);
        // Legal N. Sanity executes subop 5 with input vec 5, output vec 0,
        // and B=0x800 (the immediate Q8 value zero).
        let mut object =
            VmObject::new(h, vec![transform_vectors_instruction(5, 5, 0, 0x800)]).unwrap();
        object.initialize_retail_process(0, 0).unwrap();
        object.set_process_vector(4, [0; 3]).unwrap();
        object.set_process_vector(5, [1_000, 2_000, 3_000]).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.run(h, 0).unwrap();
        let base_stack_len = machine.object(h).unwrap().stack().len();
        machine.push(h, 160).unwrap();
        machine.push(h, (-320_i32) as u32).unwrap();

        machine.run(h, 1).unwrap();
        let object = machine.object(h).unwrap();
        assert_eq!(object.process_vector(0), Ok([1_160, 1_680, 3_000]));
        assert_eq!(object.stack().len(), base_stack_len);
    }

    #[test]
    fn transform_vectors_audio_transform_consumes_b_and_preserves_source_y_quirk() {
        let h = handle(0);
        let mut object =
            VmObject::new(h, vec![transform_vectors_instruction(7, 0, 1, STACK)]).unwrap();
        object.initialize_retail_process(0, 0).unwrap();
        object.set_process_vector(0, [256, 512, -768]).unwrap();
        object.set_process_vector(1, [10, 7, 30]).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.set_transform_vectors_camera(RetailTransformVectorsCamera::from_retail_pose(
            [0; 3], [0; 3], 500,
        ));
        machine.run(h, 0).unwrap();
        let base_stack_len = machine.object(h).unwrap().stack().len();
        machine.push(h, 0xdead_beef).unwrap();

        machine.run(h, 1).unwrap();
        let object = machine.object(h).unwrap();
        assert_eq!(object.process_vector(1), Ok([256, -1_792, 768]));
        assert_eq!(object.stack().len(), base_stack_len);
    }

    #[test]
    fn transform_vectors_orients_zone_translation_from_stack_progress() {
        let h = handle(0);
        let mut object = VmObject::new(h, vec![0x8502_8e1f]).unwrap();
        object.initialize_retail_process(0, 0).unwrap();
        object
            .initialize_retail_entity(
                &entity_with_path(vec![
                    ZoneEntityPathPoint {
                        x: 10,
                        y: 20,
                        z: 30,
                    },
                    ZoneEntityPathPoint {
                        x: 35,
                        y: 19,
                        z: 30,
                    },
                ]),
                [100, 200, 300],
            )
            .unwrap();
        object
            .set_register(
                process_register::STATUS_B,
                STATUS_B_TRACK_PATH_SIGN | STATUS_B_TRACK_PATH_PITCH,
            )
            .unwrap();
        object
            .set_register(process_register::PATH_PROGRESS, 34)
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.run(h, 0).unwrap();
        machine.push(h, 34).unwrap();

        assert_eq!(
            machine.run(h, 1).unwrap().reason,
            HaltReason::BudgetExhausted
        );
        let object = machine.object(h).unwrap();
        assert_eq!(object.process_vector(0), Ok([39_240, 71_544, 107_520]));
        assert_eq!(object.register(process_register::FLOOR_Y), Ok(71_544));
        assert_eq!(object.register(process_register::ROTATION_Z), Ok(0x400));
        assert_eq!(object.register(process_register::MISC_B_X), Ok(0x400));
        // Source atan_table[40] == 0x1a for the -4:100 path pitch.
        assert_eq!(object.register(process_register::MISC_B_Z), Ok(0x1a));
        assert_eq!(object.stack().len(), 3, "stack progress must be consumed");
    }

    #[test]
    fn transform_vectors_path_copy_wins_over_aliased_misc_c_side_effects() {
        let h = handle(0);
        let mut object =
            VmObject::new(h, vec![transform_vectors_instruction(0, 5, 0, 0x800)]).unwrap();
        object.initialize_retail_process(0, 0).unwrap();
        object
            .initialize_retail_entity_path(
                &entity_with_path(vec![
                    ZoneEntityPathPoint {
                        x: 10,
                        y: 20,
                        z: 30,
                    },
                    ZoneEntityPathPoint {
                        x: 35,
                        y: 19,
                        z: 30,
                    },
                ]),
                RetailEntityPathSpace::Model,
            )
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        machine.run(h, 1).unwrap();

        let object = machine.object(h).unwrap();
        assert_eq!(object.process_vector(5), Ok([2_560, 5_120, 7_680]));
        assert_eq!(object.register(process_register::FLOOR_Y), Ok(5_120));
    }

    #[test]
    fn mov_copies_entity_path_reference_beyond_authored_parent_lifetime() {
        let parent = handle(0);
        let child = handle(1);
        let mut parent_object = VmObject::new(parent, vec![0]).unwrap();
        parent_object.initialize_retail_process(0, 0).unwrap();
        parent_object
            .initialize_retail_entity(
                &entity_with_path(vec![
                    ZoneEntityPathPoint {
                        x: 10,
                        y: 20,
                        z: 30,
                    },
                    ZoneEntityPathPoint {
                        x: 20,
                        y: 20,
                        z: 30,
                    },
                ]),
                [100, 200, 300],
            )
            .unwrap();

        let mut child_object = VmObject::new(
            child,
            vec![
                // RooOC's state-three pointer copy: process.entity from the
                // parent link into this runtime-created child.
                0x11c6_ce2c,
                transform_vectors_instruction(0, 0, 0, 0x0e2d),
            ],
        )
        .unwrap();
        child_object.initialize_retail_process(1, 0).unwrap();
        child_object.set_link(1, Some(parent)).unwrap();
        child_object.set_process_vector(0, [-1, -2, -3]).unwrap();

        let mut machine = Machine::new(0);
        machine.insert_object(parent_object).unwrap();
        machine.insert_object(child_object).unwrap();
        assert_eq!(
            machine.run(child, 1).unwrap().reason,
            HaltReason::BudgetExhausted
        );

        let parent_reference = machine
            .object(parent)
            .unwrap()
            .register(process_register::ENTITY_REFERENCE)
            .unwrap();
        assert!(EntityReference::from_word(parent_reference).is_some());
        assert_eq!(
            machine
                .object(child)
                .unwrap()
                .register(process_register::ENTITY_REFERENCE),
            Ok(parent_reference)
        );

        // The native pointer targets immutable ZDAT storage, not the parent
        // GOOL object. Removing the parent must therefore leave the copied
        // reference usable by the child.
        machine.remove_object(parent).unwrap();
        assert_eq!(
            machine.run(child, 1).unwrap().reason,
            HaltReason::BudgetExhausted
        );
        assert_eq!(
            machine.object(child).unwrap().process_vector(0),
            Ok([35_840, 71_680, 107_520])
        );
    }

    #[test]
    fn transform_vectors_uses_model_space_and_last_point_rule() {
        let h = handle(0);
        let mut object = VmObject::new(h, vec![0x8502_8e1f]).unwrap();
        object.initialize_retail_process(0, 0).unwrap();
        object
            .initialize_retail_entity_path(
                &entity_with_path(vec![
                    ZoneEntityPathPoint { x: 0, y: 0, z: 0 },
                    ZoneEntityPathPoint {
                        x: 10,
                        y: 20,
                        z: -10,
                    },
                ]),
                RetailEntityPathSpace::Model,
            )
            .unwrap();
        object
            .set_register(process_register::PATH_PROGRESS, (-0x100_i32) as u32)
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.run(h, 0).unwrap();
        machine.push(h, 0x100).unwrap();

        machine.run(h, 1).unwrap();
        let object = machine.object(h).unwrap();
        assert_eq!(object.process_vector(0), Ok([2_560, 5_120, -2_560]));
        assert_eq!(object.register(process_register::FLOOR_Y), Ok(5_120));
        assert_ne!(
            object.register(process_register::STATUS_A).unwrap() & STATUS_A_TOWARD_GOAL,
            0
        );
    }

    #[test]
    fn transform_vectors_rejects_malformed_path_progress_and_vector_index() {
        let entity = entity_with_path(vec![
            ZoneEntityPathPoint { x: 0, y: 0, z: 0 },
            ZoneEntityPathPoint { x: 1, y: 0, z: 0 },
        ]);
        let h = handle(0);
        let mut object = VmObject::new(h, vec![0x8502_8e1f]).unwrap();
        object.initialize_retail_process(0, 0).unwrap();
        object.initialize_retail_entity(&entity, [0; 3]).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.run(h, 0).unwrap();
        machine.push(h, 0x200).unwrap();
        assert_eq!(
            machine.run(h, 1),
            Err(VmError::EntityPathProgressOutOfBounds {
                progress: 0x200,
                point_count: 2,
            })
        );

        let h = handle(1);
        let mut object = VmObject::new(h, vec![0x8502_ee1f]).unwrap();
        object.initialize_retail_process(0, 0).unwrap();
        object.initialize_retail_entity(&entity, [0; 3]).unwrap();
        machine.insert_object(object).unwrap();
        machine.run(h, 0).unwrap();
        machine.push(h, 0).unwrap();
        assert_eq!(machine.run(h, 1), Err(VmError::InvalidProcessVector(6)));
    }

    #[test]
    fn transform_vectors_rejects_unbound_entity_reference() {
        let h = handle(0);
        let mut object =
            VmObject::new(h, vec![transform_vectors_instruction(0, 0, 0, 0x0e2d)]).unwrap();
        object.initialize_retail_process(0, 0).unwrap();
        let word = EntityReference { slot: 7 }.to_word();
        object
            .set_register(process_register::ENTITY_REFERENCE, word)
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        assert_eq!(
            machine.run(h, 1),
            Err(VmError::InvalidEntityReference(word))
        );
    }

    #[test]
    fn single_point_path_is_stationary_for_fractional_and_extreme_progress() {
        let path = RetailEntityPath {
            entity_id: 0,
            space: RetailEntityPathSpace::Model,
            points: vec![ZoneEntityPathPoint {
                x: 99,
                y: 200,
                z: 200,
            }],
        };
        let inputs = PathOrientationInputs {
            location: [1, 2, 3],
            status_a: 0x1234,
            status_b: STATUS_B_ORIENT_ON_PATH,
            object_progress: 0x110,
            inertia_limit: 77,
            misc_c_y: 88,
            rotation_z: 99,
            target_rotation_x: 111,
            target_rotation_y: 222,
        };

        for progress in [0, 0x110, -0x110, i32::MIN, i32::MAX] {
            assert_eq!(
                orient_retail_path(&path, progress, inputs),
                Ok(PathOrientation {
                    location: [99 * 0x100, 200 * 0x100, 200 * 0x100],
                    status_a: inputs.status_a,
                    misc_c_y: inputs.misc_c_y,
                    rotation_z: inputs.rotation_z,
                    target_rotation_x: inputs.target_rotation_x,
                    target_rotation_y: inputs.target_rotation_y,
                }),
                "one declared point must never index into adjacent serialized storage"
            );
        }
    }

    #[test]
    fn path_projection_sets_inertia_and_direction_flags_with_source_math() {
        let path = RetailEntityPath {
            entity_id: 0,
            space: RetailEntityPathSpace::Model,
            points: vec![
                ZoneEntityPathPoint { x: 0, y: 0, z: 0 },
                ZoneEntityPathPoint { x: 0, y: 0, z: 100 },
            ],
        };
        let result = orient_retail_path(
            &path,
            0,
            PathOrientationInputs {
                location: [0x100, 0, 50 * 0x100],
                status_a: STATUS_A_TOWARD_GOAL,
                status_b: STATUS_B_ORIENT_ON_PATH,
                object_progress: 0,
                inertia_limit: 100,
                misc_c_y: 0,
                rotation_z: 0,
                target_rotation_x: 0,
                target_rotation_y: 0,
            },
        )
        .unwrap();

        assert_eq!(result.location, [0; 3]);
        assert_eq!(result.misc_c_y, 386);
        assert_eq!(
            result.status_a,
            STATUS_A_INVALID_PATH | STATUS_A_CHANGE_PATH_DIRECTION
        );
        assert_eq!(retail_sqrt(10_000), Ok(99));
        assert_eq!(retail_atan2(-1_024, 25_600), -0x1a);
    }

    fn retail_color_environment(
        object_intensity: [u16; 3],
        player_intensity: [u16; 3],
    ) -> RetailSolidEnvironment {
        let mut object_colors = [0_u16; COLOR_COUNT];
        let mut player_colors = [0_u16; COLOR_COUNT];
        object_colors[COLOR_INTENSITY_START..COLOR_INTENSITY_END]
            .copy_from_slice(&object_intensity);
        player_colors[COLOR_INTENSITY_START..COLOR_INTENSITY_END]
            .copy_from_slice(&player_intensity);
        RetailSolidEnvironment::new(0, object_colors, player_colors, Vec::new())
    }

    #[test]
    fn native_colors_cycle_only_the_three_intensity_halfwords() {
        for (draw_count, expected) in [(0, 0x07f), (1, 0x37f), (2, 0x27f), (3, 0x17f)] {
            let h = handle(0);
            let mut object = VmObject::new(h, Vec::new()).unwrap();
            let mut original = [0_u16; COLOR_COUNT];
            for (index, color) in original.iter_mut().enumerate() {
                *color = u16::try_from(index * 17).unwrap();
            }
            object.set_retail_colors(original);
            object
                .set_register(process_register::INVINCIBILITY_STATE, 2)
                .unwrap();
            let mut machine = Machine::new(0);
            machine.insert_object(object).unwrap();
            machine.set_draw_count(draw_count);

            machine.run_retail_object_colors(h).unwrap();

            let colors = machine.object(h).unwrap().retail_colors();
            assert_eq!(
                &colors[..COLOR_INTENSITY_START],
                &original[..COLOR_INTENSITY_START]
            );
            assert_eq!(
                &colors[COLOR_INTENSITY_START..COLOR_INTENSITY_END],
                &[expected; 3]
            );
        }
    }

    #[test]
    fn native_color_expiry_thresholds_are_strict_and_keep_fallthrough_flash() {
        for (invincibility_state, threshold) in [(3, 451), (4, 60), (5, 602)] {
            let h = handle(0);
            let mut object = VmObject::new(h, Vec::new()).unwrap();
            object.bind_retail_solid_environment(retail_color_environment(
                [0x111, 0x222, 0x333],
                [0x444, 0x555, 0x666],
            ));
            object
                .set_register(process_register::INVINCIBILITY_STATE, invincibility_state)
                .unwrap();
            object
                .set_register(process_register::INVINCIBILITY_STAMP, 100)
                .unwrap();
            let mut machine = Machine::new(0);
            machine.insert_object(object).unwrap();
            machine.set_draw_count(2);
            machine.set_frames_elapsed(100 + threshold);

            machine.run_retail_object_colors(h).unwrap();
            assert_eq!(
                machine
                    .object(h)
                    .unwrap()
                    .register(process_register::INVINCIBILITY_STATE),
                Ok(invincibility_state)
            );

            machine.set_frames_elapsed(101 + threshold);
            machine.run_retail_object_colors(h).unwrap();
            let object = machine.object(h).unwrap();
            assert_eq!(
                object.register(process_register::INVINCIBILITY_STATE),
                Ok(0)
            );
            assert_eq!(
                &object.retail_colors()[COLOR_INTENSITY_START..COLOR_INTENSITY_END],
                &[0x27f; 3],
                "zone reset is intentionally overwritten by case-two fallthrough"
            );
        }
    }

    #[test]
    fn state_four_emits_checked_category_three_hundred_collider_event() {
        let sender = handle(0);
        let collider = handle(1);
        let mut sender_object = VmObject::new(sender, Vec::new()).unwrap();
        sender_object.bind_retail_solid_environment(retail_color_environment([1, 2, 3], [4, 5, 6]));
        sender_object
            .set_register(process_register::INVINCIBILITY_STATE, 4)
            .unwrap();
        sender_object.set_link(6, Some(collider)).unwrap();
        let mut collider_object = VmObject::new(collider, Vec::new()).unwrap();
        collider_object.program_identity = Some(GoolProgramIdentity {
            global_eid: Eid::from_raw(0x7500_2055),
            object_type: 0,
            category: 0x300,
        });
        let mut machine = Machine::new(0);
        machine.insert_object(sender_object).unwrap();
        machine.insert_object(collider_object).unwrap();
        machine.set_frames_elapsed(61);

        machine.run_retail_object_colors(sender).unwrap();

        assert_eq!(
            machine.effects(),
            &[VmEffect::Event {
                sender,
                recipient: Some(collider),
                event: HIT_INVINCIBLE_EVENT,
            }]
        );
        assert_eq!(
            machine
                .object(sender)
                .unwrap()
                .register(process_register::INVINCIBILITY_STATE),
            Ok(0)
        );
    }

    #[test]
    fn hosted_invincibility_hit_stops_before_writing_a_reused_sender_slot() {
        let sender = handle(0);
        let collider = handle(1);
        let mut sender_object = VmObject::new(sender, Vec::new()).unwrap();
        sender_object
            .set_register(process_register::INVINCIBILITY_STATE, 4)
            .unwrap();
        sender_object.set_link(6, Some(collider)).unwrap();
        let mut collider_object = VmObject::new(collider, Vec::new()).unwrap();
        collider_object.configure_test_program_identity(0x300);
        let mut machine = Machine::new(0);
        machine.insert_object(sender_object).unwrap();
        machine.insert_object(collider_object).unwrap();
        machine.set_draw_count(2);
        let original_incarnation = machine.object_incarnation(sender).unwrap();
        let replacement_colors = [0x0555; COLOR_COUNT];
        let mut handled = false;

        let completed = machine
            .run_retail_object_colors_with_event_handler(
                sender,
                |machine, callback_sender, callback_recipient, event| {
                    handled = true;
                    assert_eq!(callback_sender, sender);
                    assert_eq!(callback_recipient, collider);
                    assert_eq!(event, HIT_INVINCIBLE_EVENT);
                    machine
                        .remove_object_for_host_termination(callback_sender)
                        .unwrap();
                    let mut replacement = VmObject::new(callback_sender, Vec::new()).unwrap();
                    replacement.set_retail_colors(replacement_colors);
                    replacement
                        .set_register(process_register::INVINCIBILITY_STATE, 0xfeed)
                        .unwrap();
                    machine.insert_object(replacement).unwrap();
                },
            )
            .unwrap();

        assert!(handled);
        assert!(!completed);
        assert_ne!(
            machine.object_incarnation(sender).unwrap(),
            original_incarnation
        );
        let replacement = machine.object(sender).unwrap();
        assert_eq!(replacement.retail_colors(), &replacement_colors);
        assert_eq!(
            replacement.register(process_register::INVINCIBILITY_STATE),
            Ok(0xfeed)
        );
        assert!(machine.effects().is_empty());
    }

    #[test]
    fn recovery_states_restore_dpad_on_strict_timeout_or_floor_contact() {
        let timed = handle(0);
        let grounded = handle(1);
        let future = handle(2);
        let mut timed_object = VmObject::new(timed, Vec::new()).unwrap();
        timed_object
            .set_register(process_register::INVINCIBILITY_STATE, 6)
            .unwrap();
        let mut grounded_object = VmObject::new(grounded, Vec::new()).unwrap();
        grounded_object
            .set_register(process_register::INVINCIBILITY_STATE, 7)
            .unwrap();
        grounded_object
            .set_register(process_register::STATUS_A, 1)
            .unwrap();
        let mut future_object = VmObject::new(future, Vec::new()).unwrap();
        future_object
            .set_register(process_register::INVINCIBILITY_STATE, 6)
            .unwrap();
        future_object
            .set_register(process_register::INVINCIBILITY_STAMP, 17)
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(timed_object).unwrap();
        machine.insert_object(grounded_object).unwrap();
        machine.insert_object(future_object).unwrap();
        machine.set_frames_elapsed(16);

        for object in [timed, grounded, future] {
            machine.run_retail_object_colors(object).unwrap();
        }

        for object in [timed, grounded] {
            let object = machine.object(object).unwrap();
            assert_eq!(
                object.register(process_register::INVINCIBILITY_STATE),
                Ok(0)
            );
            assert_ne!(
                object.register(process_register::STATUS_B).unwrap() & STATUS_B_DPAD_CONTROL,
                0
            );
        }
        assert_eq!(
            machine
                .object(future)
                .unwrap()
                .register(process_register::INVINCIBILITY_STATE),
            Ok(6),
            "wrapped unsigned subtraction becomes signed -1, not an expiry"
        );
    }

    #[test]
    fn main_player_default_branch_restores_player_zone_intensity() {
        let h = handle(0);
        let mut object = VmObject::new(h, Vec::new()).unwrap();
        object.set_main_player_identity(true);
        object.set_retail_colors([0x777; COLOR_COUNT]);
        object.bind_retail_solid_environment(retail_color_environment(
            [0x111, 0x222, 0x333],
            [0x444, 0x555, 0x666],
        ));
        object
            .set_register(process_register::INVINCIBILITY_STATE, 1)
            .unwrap();
        object
            .set_register(process_register::STATUS_B, STATUS_B_MAIN_COLOR_BY_ZONE)
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        machine.run_retail_object_colors(h).unwrap();

        let object = machine.object(h).unwrap();
        assert_eq!(
            &object.retail_colors()[COLOR_INTENSITY_START..COLOR_INTENSITY_END],
            &[0x444, 0x555, 0x666]
        );
        assert_eq!(
            object.register(process_register::INVINCIBILITY_STATE),
            Ok(0)
        );
        assert!(
            object.retail_colors()[..COLOR_INTENSITY_START]
                .iter()
                .all(|color| *color == 0x777)
        );
    }

    #[test]
    fn native_color_recovery_enables_controller_before_same_frame_physics() {
        let h = handle(0);
        let mut object = VmObject::new(h, Vec::new()).unwrap();
        object
            .set_register(process_register::INVINCIBILITY_STATE, 6)
            .unwrap();
        object
            .set_register(process_register::STATUS_A, 0x2000)
            .unwrap();
        object
            .set_register(process_register::STATUS_B, 0x40)
            .unwrap();
        object
            .set_register(process_register::STATE_FLAGS, 0x4)
            .unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();
        machine.set_frames_elapsed(16);
        machine.set_retail_physics_frame_context(true, 0);
        machine
            .set_pad_snapshot(
                0,
                RetailPadSnapshot {
                    held: 4 << 12,
                    ..RetailPadSnapshot::default()
                },
            )
            .unwrap();

        machine.run_retail_object_physics(h).unwrap();

        let expected_speed = (0x001a_0aaa_i32 * 34) / 1024;
        let expected_translation = (expected_speed * 34) / 1024;
        let object = machine.object(h).unwrap();
        assert_eq!(
            object.register(process_register::SPEED),
            Ok(expected_speed as u32)
        );
        assert_eq!(
            object.register(process_register::TRANSLATION_Z),
            Ok(expected_translation as u32)
        );
        assert_ne!(
            object.register(process_register::STATUS_B).unwrap() & STATUS_B_DPAD_CONTROL,
            0
        );
    }

    proptest! {
        #[test]
        fn instruction_fields_round_trip(opcode in any::<u8>(), a in 0_u16..0x1000, b in 0_u16..0x1000) {
            let decoded = Instruction::decode(Instruction::encode(opcode, a, b));
            prop_assert_eq!(decoded, Instruction { opcode, operand_a: a, operand_b: b });
        }

        #[test]
        fn straight_model_path_interpolation_matches_signed_eight_eight_progress(
            fraction in 0_i32..=255,
        ) {
            let path = RetailEntityPath {
                entity_id: 0,
                space: RetailEntityPathSpace::Model,
                points: vec![
                    ZoneEntityPathPoint { x: -20, y: 4, z: 9 },
                    ZoneEntityPathPoint { x: 44, y: -12, z: 25 },
                ],
            };
            let result = orient_retail_path(
                &path,
                fraction,
                PathOrientationInputs {
                    location: [0; 3],
                    status_a: 0,
                    status_b: 0,
                    object_progress: fraction,
                    inertia_limit: 0,
                    misc_c_y: 0,
                    rotation_z: 0,
                    target_rotation_x: 0,
                    target_rotation_y: 0,
                },
            ).unwrap();
            prop_assert_eq!(
                result.location,
                [
                    -20 * 0x100 + 64 * fraction,
                    4 * 0x100 - 16 * fraction,
                    9 * 0x100 + 16 * fraction,
                ],
            );
        }
    }
}
