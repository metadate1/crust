//! Bounded, word-addressed GOOL virtual machine.
//!
//! Instructions retain the retail `opcode:8 | operand-a:12 | operand-b:12`
//! layout. Native pointers are never represented; objects, registers, pages,
//! events, and call targets are checked logical indices.

use std::collections::{BTreeMap, BTreeSet};

use crust_formats::binary::{Eid, PageIndex};
use crust_formats::stream::{
    GOOL_PC_NONE, GoolProgram, ZoneEntity, ZoneEntityPathPoint, structs::GoolState,
};

use crate::math::{Angle12, Bounds3, integer_sqrt, seek};
use crate::object_bounds::{FrameBound, FrameBounds, FrameBoundsError};

pub const MAX_OBJECTS: usize = 96;
/// Exact `gool_object.regs[0x1FC]` word span from the retail 32-bit layout.
pub const REGISTER_COUNT: usize = 0x1fc;
pub const TABLE_WORD_COUNT: usize = 1024;
pub const MAX_STACK_WORDS: usize = 256;
pub const MAX_CALL_DEPTH: usize = 64;
pub const MAX_EFFECTS: usize = 256;
/// Defensive host bound for one retail `once_p` invocation. Retail runs the
/// block synchronously until its return link; the bound turns malformed or
/// recursive input into a typed failure instead of hanging the browser.
pub const MAX_ONCE_INSTRUCTIONS: usize = 16_384;
/// Defensive host bound for one synchronous retail transition-block
/// invocation. Retail has no native instruction limit; malformed local data
/// becomes a typed failure here instead of hanging the browser's 30 Hz loop.
pub const MAX_TRANSITION_INSTRUCTIONS: usize = 16_384;
pub const RETAIL_PAD_COUNT: usize = 2;
/// Halfword count in the retail `gool_colors` union.
pub const COLOR_COUNT: usize = 24;
/// Fourteen-bit retail code/PC address space.
pub const MAX_CODE_WORDS: usize = 1 << 14;
pub const NULL_INPUT_VALUE: u32 = 3;
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
const ENTRY_REFERENCE_TAG: u32 = 0xa400_0000;
const ENTRY_REFERENCE_SLOT_BITS: u32 = 0x003f_ffff;
const ENTRY_REFERENCE_SLOT_SHIFT: u32 = 2;
const ENTRY_REFERENCE_PAYLOAD_MASK: u32 = ENTRY_REFERENCE_SLOT_BITS << ENTRY_REFERENCE_SLOT_SHIFT;
const COLLISION_OBJECT_REFERENCE_TAG: u32 = 0xa300_0000;
const COLLISION_OBJECT_REFERENCE_BITS: u32 = 0x7f;
const COLLISION_OBJECT_REFERENCE_SHIFT: u32 = 2;
const COLLISION_OBJECT_REFERENCE_MASK: u32 =
    COLLISION_OBJECT_REFERENCE_BITS << COLLISION_OBJECT_REFERENCE_SHIFT;
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

/// Immutable identity of the global GOOL program that owns an object's
/// animation item and retail display category.
///
/// Keeping both fields on the VM object prevents hosts from reconstructing
/// render metadata through an arena slot or executable number after either
/// handle has been recycled. Objects authored directly with [`VmObject::new`]
/// intentionally have no parsed-program identity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GoolProgramIdentity {
    global_eid: Eid,
    category: u32,
}

impl GoolProgramIdentity {
    #[must_use]
    pub const fn global_eid(self) -> Eid {
        self.global_eid
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

/// Pointer-free encoding of a translated GOOL input operand.
///
/// Retail pushes native addresses from opcode `0x26`. The Rust VM instead
/// packs an object handle, storage region and checked word index under a tag
/// disjoint from code and animation references. Its low two bits remain zero
/// like the source word pointer, so it cannot be mistaken for a named EID.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StorageReference {
    object: ObjectHandle,
    region: StorageRegion,
    index: u16,
}

impl StorageReference {
    fn checked(object: ObjectHandle, region: StorageRegion, index: usize) -> Result<Self, VmError> {
        if index > STORAGE_REFERENCE_INDEX_BITS as usize {
            return Err(VmError::InvalidStorageReference(STORAGE_REFERENCE_TAG));
        }
        Ok(Self {
            object,
            region,
            index: index as u16,
        })
    }

    #[must_use]
    pub const fn object(self) -> ObjectHandle {
        self.object
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
        STORAGE_REFERENCE_TAG
            | ((self.region as u32) << STORAGE_REFERENCE_REGION_SHIFT)
            | ((self.object.get() as u32) << STORAGE_REFERENCE_OBJECT_SHIFT)
            | ((self.index as u32) << STORAGE_REFERENCE_INDEX_SHIFT)
    }

    #[must_use]
    pub const fn from_word(word: u32) -> Option<Self> {
        if word & !STORAGE_REFERENCE_PAYLOAD_MASK != STORAGE_REFERENCE_TAG {
            return None;
        }
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
        Some(Self {
            object,
            region,
            index: ((word & STORAGE_REFERENCE_INDEX_MASK) >> STORAGE_REFERENCE_INDEX_SHIFT) as u16,
        })
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
/// shifts the validated 96-slot handle above two zero alignment bits. Reserved
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
    origin: [i32; 3],
    dimensions: [u32; 3],
    root: u16,
    max_depth: [u16; 3],
    bytes: Vec<u8>,
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
            origin,
            dimensions,
            root,
            max_depth,
            bytes,
        })
    }
}

/// Pointer-free zone state needed by `ZoneFindNearestObjectNode3`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetailSolidEnvironment {
    graphics_flags: u32,
    object_colors: [u16; COLOR_COUNT],
    player_colors: [u16; COLOR_COUNT],
    neighbors: Vec<RetailSolidZone>,
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
    fn from_zone(zone: &RetailSolidZone) -> Result<Self, VmError> {
        let mut origin = [0_i32; 3];
        let mut dimensions = [0_i32; 3];
        for axis in 0..3 {
            origin[axis] = zone.origin[axis]
                .checked_mul(0x100)
                .ok_or(VmError::ArithmeticOverflow)?;
            dimensions[axis] = i32::try_from(zone.dimensions[axis])
                .ok()
                .and_then(|value| value.checked_mul(0x100))
                .ok_or(VmError::ArithmeticOverflow)?;
            origin[axis]
                .checked_add(dimensions[axis])
                .ok_or(VmError::ArithmeticOverflow)?;
        }
        Ok(Self { origin, dimensions })
    }

    fn contains_unscaled_zone_point(
        zone: &RetailSolidZone,
        point: [i32; 3],
    ) -> Result<bool, VmError> {
        let rect = Self::from_zone(zone)?;
        for (axis, coordinate) in point.into_iter().enumerate() {
            let end = rect.origin[axis]
                .checked_add(rect.dimensions[axis])
                .ok_or(VmError::ArithmeticOverflow)?;
            if coordinate < rect.origin[axis] || coordinate > end {
                return Ok(false);
            }
        }
        Ok(true)
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
            if RetailSolidRect::contains_unscaled_zone_point(zone, point)? {
                containing = Some(zone);
                break;
            }
        }
        let Some(zone) = containing else {
            return Ok((None, point));
        };
        let mut rect = RetailSolidRect::from_zone(zone)?;
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

fn scaled_retail_colors(
    source: &[u16; COLOR_COUNT],
    subtype: i32,
) -> Result<[u16; COLOR_COUNT], VmError> {
    let percentage = match subtype {
        i32::MIN..=39 => 100,
        // These selectors have level-specific hard-coded matrices in five
        // retail levels. Until a level selector is owned here, do not silently
        // substitute the generic zero matrix.
        40..=44 | 64..=i32::MAX => {
            return Err(VmError::LevelDependentColorSubtype(subtype as u8));
        }
        45..=47 => 0,
        48..=63 => {
            const PERCENTAGES: [u32; 16] = [
                2, 16, 30, 44, 58, 72, 86, 100, 112, 124, 136, 148, 160, 172, 184, 196,
            ];
            PERCENTAGES[usize::try_from(subtype - 48).map_err(|_| VmError::ArithmeticOverflow)?]
        }
    };
    let factor = (percentage << 12) / 100;
    let mut scaled = *source;
    for (destination, source) in scaled[..9].iter_mut().zip(&source[..9]) {
        let value = i64::from(*source as i16) * i64::from(factor);
        *destination = ((value >> 12) as i16) as u16;
    }
    for (destination, source) in scaled[9..12].iter_mut().zip(&source[9..12]) {
        *destination = ((u32::from(*source) * factor) >> 12) as u16;
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
            object_colors,
            player_colors,
            neighbors,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RetailEntityPath {
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
    if path.points.len() == 1 {
        return Ok(output);
    }
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

fn retail_random(maximum: u32, seed: &mut u32) -> u32 {
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

/// Stable index into the 96-object pool.
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

/// Host-visible, deterministic effect emitted by GOOL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VmEffect {
    Event {
        sender: ObjectHandle,
        recipient: Option<ObjectHandle>,
        event: u32,
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
    Paging {
        object: ObjectHandle,
        open: bool,
        reference: u32,
    },
    SpawnChildren {
        parent: ObjectHandle,
        executable: u8,
        subtype: u8,
        count: u32,
        allow_reclaim: bool,
        arguments: Vec<u32>,
    },
    AnimationSelected {
        object: ObjectHandle,
        reference: AnimationReference,
    },
    AnimationFrameChanged {
        object: ObjectHandle,
        frame: u32,
        scale_x: i32,
    },
    Transition(i32),
    SaveState(ObjectHandle),
    LoadState(ObjectHandle),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VmError {
    TooManyObjects,
    DuplicateObject(ObjectHandle),
    UnknownObject(ObjectHandle),
    CodeTooLarge,
    GlobalCodeTooLarge,
    InternalTableTooLarge(usize),
    ExternalTableTooLarge(usize),
    InvalidInitialStackPointer(u32),
    InvalidPadPort(usize),
    EntityPathTooLong(usize),
    EntityPathProgressOutOfBounds {
        progress: i32,
        point_count: usize,
    },
    InvalidProcessVector(u8),
    MalformedSolidOctree {
        offset: usize,
    },
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
    InvalidEntryReference(u32),
    EntryReferenceTableFull,
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
    LevelDependentColorSubtype(u8),
    /// Opcode `0x8e` delegates to zone-solid queries whose checked Rust host
    /// is not wired yet. Preserve the packed selector fields so callers can
    /// characterize the exact retail boundary without treating collision as
    /// a successful no-op.
    UnsupportedSolidSurface {
        suboperation: u8,
        input_vector: u8,
        output_vector: u8,
        operand: u16,
    },
    MissingSolidEnvironment(ObjectHandle),
    /// More than the retail object-pool maximum of 96 ordered AABB snapshots
    /// were registered for one frame.
    FrameBoundsCapacityExceeded,
    /// Suboperation three still requires its separate transformed-bound and
    /// color-selection path; suboperation one's ordered queries are hosted.
    UnsupportedSolidObjectBounds(ObjectHandle),
    /// Transform-vector suboperations whose required camera, animation, or
    /// matrix host state has not yet been made pointer-free remain explicit.
    UnsupportedTransformVectors {
        suboperation: u8,
        input_vector: u8,
        output_vector: u8,
        operand: u16,
    },
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
    UnknownOpcode(u8),
    UnknownControl(u8),
    EffectQueueFull,
    MissingHostEffect,
}

/// Why an interpreter invocation stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HaltReason {
    Halted,
    /// A synchronous host effect must be applied before interpretation resumes.
    HostEffect,
    StateChanged(u16),
    AnimationChanged {
        frame: u32,
        wait: u8,
    },
    AnimationWaiting {
        remaining: u8,
    },
    /// A state-change `once_p` block returned through its suspend link. The
    /// production runtime consumes this internal synchronous boundary before
    /// exposing the rebound state to the following frame.
    OnceCompleted,
    /// A state transition block returned through the nested frame installed
    /// by `GoolObjectChangeState`.
    TransitionCompleted,
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
    global_code: Vec<u32>,
    code: Vec<u32>,
    code_segment: CodeSegment,
    pc: usize,
    initial_stack_pointer: u32,
    frame_base: usize,
    internal: Vec<u32>,
    external: Vec<u32>,
    registers: Vec<u32>,
    colors: [u16; COLOR_COUNT],
    base_colors: [u16; COLOR_COUNT],
    entity_spawn_flags: Option<u16>,
    entity_path: Option<RetailEntityPath>,
    solid_environment: Option<RetailSolidEnvironment>,
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
    transition_pc: Option<usize>,
    halted: bool,
}

impl VmObject {
    pub fn new(handle: ObjectHandle, code: Vec<u32>) -> Result<Self, VmError> {
        if code.len() > MAX_CODE_WORDS {
            return Err(VmError::CodeTooLarge);
        }
        Ok(Self {
            handle,
            program_identity: None,
            global_code: Vec::new(),
            code,
            code_segment: CodeSegment::External,
            pc: 0,
            initial_stack_pointer: SYNTHETIC_STACK_POINTER as u32,
            frame_base: SYNTHETIC_STACK_POINTER,
            internal: vec![0; TABLE_WORD_COUNT],
            external: vec![0; TABLE_WORD_COUNT],
            registers: vec![0; REGISTER_COUNT],
            colors: [0; COLOR_COUNT],
            base_colors: [0; COLOR_COUNT],
            entity_spawn_flags: None,
            entity_path: None,
            solid_environment: None,
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
            transition_pc: None,
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
            category: program.header().category,
        });
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
        object.transition_pc = program.transition_pc();
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
    /// built from a validated [`GoolProgram`] always return both the global
    /// EID and its retail category together.
    #[must_use]
    pub const fn program_identity(&self) -> Option<GoolProgramIdentity> {
        self.program_identity
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

    #[must_use]
    pub const fn event_pc(&self) -> Option<usize> {
        self.event_pc
    }

    #[must_use]
    pub const fn transition_pc(&self) -> Option<usize> {
        self.transition_pc
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
        *self
            .registers
            .get_mut(index)
            .ok_or(VmError::InvalidRegister(index))? = value;
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
        let arguments = self
            .stack
            .get(..self.state_argument_count)
            .ok_or(VmError::InvalidInitialStackPointer(
                self.initial_stack_pointer,
            ))?
            .to_vec();
        // `GoolObjectInit` precedes `GoolObjectChangeState`. Clear the active
        // stack view while process fields are initialized, then reconstruct
        // the state frame so overlapping process words follow that order.
        self.stack.clear();
        self.animation_wait = None;
        self.set_retail_transform(RetailTransform::default())?;
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
            process_register::ONCE_POINTER,
            process_register::MISC_VALUE,
            process_register::ACK,
            process_register::ANIMATION_STAMP,
            process_register::ANIMATION_COUNTER,
            process_register::ANIMATION_SEQUENCE,
            process_register::ANIMATION_FRAME,
            process_register::ENTITY_REFERENCE,
            process_register::PATH_PROGRESS,
            process_register::PATH_LENGTH,
            process_register::FLOOR_Y,
            process_register::SPEED,
            process_register::INVINCIBILITY_STATE,
            process_register::INVINCIBILITY_STAMP,
            process_register::FLOOR_IMPACT_STAMP,
            process_register::FLOOR_IMPACT_VELOCITY,
            process_register::SIZE,
            process_register::EVENT,
            process_register::CAMERA_ZOOM,
            process_register::ANGULAR_VELOCITY_Y,
            process_register::HOTSPOT_SIZE,
            process_register::UNKNOWN_150,
            process_register::UNKNOWN_154,
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
        self.initialize_arguments(&arguments)?;
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
        self.entity_path = Some(RetailEntityPath {
            space: path_space,
            points: entity.path_points.clone(),
        });

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
        progress: i32,
        vector_index: u8,
    ) -> Result<(), VmError> {
        // GoolOpTransformVectors checks `process.entity`; runtime children
        // therefore retain their vector unchanged after B has been translated.
        let Some(path) = self.entity_path.as_ref() else {
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

        self.set_process_vector(vector_index, oriented.location)?;
        self.set_register(process_register::FLOOR_Y, oriented.location[1] as u32)?;
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
        )
    }

    pub fn set_retail_colors(&mut self, colors: [u16; COLOR_COUNT]) {
        self.base_colors = colors;
        self.colors = colors;
    }

    /// Owns the current ZDAT solid-query inputs without retaining relocated
    /// entry or octree pointers from the source runtime.
    pub fn bind_retail_solid_environment(&mut self, environment: RetailSolidEnvironment) {
        self.solid_environment = Some(environment);
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

    fn scale_colors_for_entity_node(&mut self) -> Result<(), VmError> {
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
        self.colors = scaled_retail_colors(&self.base_colors, i32::from(subtype))?;
        Ok(())
    }

    /// Installs arguments exactly where retail frame-relative operands expect
    /// them: immediately below the initial frame pointer. The runtime calls
    /// this once after binding a newly spawned object and before interpreting
    /// its state code.
    pub fn initialize_arguments(&mut self, arguments: &[u32]) -> Result<(), VmError> {
        self.initialize_state_frame(arguments, true)
    }

    fn initialize_state_frame(
        &mut self,
        arguments: &[u32],
        push_initial_wait: bool,
    ) -> Result<(), VmError> {
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
        for argument in arguments {
            self.push_stack_word(*argument)?;
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

    /// Current checked byte reference into the owning global GOOL animation
    /// item. Zero is retail's null animation pointer.
    pub fn animation_reference(&self) -> Result<Option<AnimationReference>, VmError> {
        let word = self.register(process_register::ANIMATION_SEQUENCE)?;
        if word == 0 {
            return Ok(None);
        }
        let reference =
            AnimationReference::from_word(word).ok_or(VmError::InvalidAnimationReference(word))?;
        let _validated_data = self.animation_data(reference)?;
        Ok(Some(reference))
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
        if required > MAX_STACK_WORDS {
            return Err(VmError::StackOverflow(self.handle));
        }

        self.code.clone_from(&program.code);
        self.external.fill(0);
        self.external[..program.external.len()].copy_from_slice(&program.external);
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
        self.transition_pc = program.transition_pc;
        self.code_segment = CodeSegment::External;
        self.pc = program.code_pc.unwrap_or(0);
        self.halted = program.code_pc.is_none();
        self.animation_wait = None;
        // Retail clears `once_p` only after the target external program and
        // state PCs have been rebound, but before replacing fp/sp.
        self.set_register(process_register::ONCE_POINTER, 0)?;
        self.initialize_state_frame(arguments, once.is_none())?;
        self.mark_retail_state_change()?;
        if let Some(once) = once {
            self.pending_once = Some(once);
        } else {
            self.set_register(process_register::STATE_STAMP, frame_stamp)?;
        }
        Ok(())
    }

    pub fn set_internal(&mut self, index: usize, value: u32) -> Result<(), VmError> {
        *self
            .internal
            .get_mut(index)
            .ok_or(VmError::InvalidRegister(index))? = value;
        Ok(())
    }

    pub fn set_external(&mut self, index: usize, value: u32) -> Result<(), VmError> {
        *self
            .external
            .get_mut(index)
            .ok_or(VmError::InvalidRegister(index))? = value;
        Ok(())
    }

    pub fn set_link(&mut self, index: usize, target: Option<ObjectHandle>) -> Result<(), VmError> {
        *self
            .links
            .get_mut(index)
            .ok_or(VmError::InvalidRegister(index))? = target;
        Ok(())
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

/// Re-entrant GOOL machine. Branch state belongs to each invocation, never a
/// process-global static as in the C interpreter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Machine {
    objects: BTreeMap<ObjectHandle, VmObject>,
    globals: Vec<u32>,
    effects: Vec<VmEffect>,
    random_seed: u32,
    ticks_per_frame: i32,
    draw_count: u32,
    frames_elapsed: u32,
    pads: [RetailPadSnapshot; RETAIL_PAD_COUNT],
    operand_constants: [u32; 2],
    input_constant_index: usize,
    output_constant_index: usize,
    // Function-static vectors from `GoolOpReactSolidSurfaces`. They are shared
    // across objects exactly like the source globals, but remain ordinary
    // deterministic machine state rather than hidden Rust statics.
    solid_trans3: [i32; 3],
    solid_trans4: [i32; 3],
    solid_frame_bounds: FrameBounds<ObjectHandle>,
    camera_translation: [i32; 3],
    paging_page_capacity: u32,
    entry_pages: BTreeMap<Eid, PageIndex>,
    paging_page_references: BTreeMap<PageIndex, u32>,
    paging_baseline_pages: BTreeSet<PageIndex>,
    paging_loaded_pages: BTreeSet<PageIndex>,
    paging_resolved_pages: BTreeSet<PageIndex>,
    paging_entry_references: Vec<(Eid, PageIndex)>,
}

impl Machine {
    #[must_use]
    pub fn new(global_words: usize) -> Self {
        Self {
            objects: BTreeMap::new(),
            globals: vec![0; global_words],
            effects: Vec::new(),
            random_seed: 12_345,
            ticks_per_frame: 34,
            draw_count: 0,
            frames_elapsed: 0,
            pads: [RetailPadSnapshot::default(); RETAIL_PAD_COUNT],
            operand_constants: [0; 2],
            input_constant_index: 0,
            output_constant_index: 0,
            solid_trans3: [0; 3],
            solid_trans4: [0; 3],
            solid_frame_bounds: FrameBounds::new(),
            camera_translation: [0; 3],
            paging_page_capacity: 0,
            entry_pages: BTreeMap::new(),
            paging_page_references: BTreeMap::new(),
            paging_baseline_pages: BTreeSet::new(),
            paging_loaded_pages: BTreeSet::new(),
            paging_resolved_pages: BTreeSet::new(),
            paging_entry_references: Vec::new(),
        }
    }

    /// Restores the retail gameplay RNG stream used by opcode `0x10`.
    pub fn set_random_seed(&mut self, seed: u32) {
        self.random_seed = seed;
    }

    /// Supplies the cooperative host timing consumed by opcode `0x1b`.
    pub fn set_ticks_per_frame(&mut self, ticks_per_frame: i32) {
        self.ticks_per_frame = ticks_per_frame;
    }

    /// Supplies the presentation counter consumed by opcode `0x1e`.
    pub fn set_draw_count(&mut self, draw_count: u32) {
        self.draw_count = draw_count;
    }

    /// Supplies the retail simulation-frame stamp used by animation waits.
    pub fn set_frames_elapsed(&mut self, frames_elapsed: u32) {
        self.frames_elapsed = frames_elapsed;
    }

    #[must_use]
    pub const fn frames_elapsed(&self) -> u32 {
        self.frames_elapsed
    }

    /// Clears the animation-derived AABB snapshots at the start of a frame.
    /// The retail-sized backing allocation is retained for the next traversal.
    pub fn clear_frame_bounds(&mut self) {
        self.solid_frame_bounds.clear();
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
        self.object(object)?;
        self.solid_frame_bounds
            .push(FrameBound { bound, object })
            .map_err(|error| match error {
                FrameBoundsError::CapacityExceeded => VmError::FrameBoundsCapacityExceeded,
            })
    }

    /// Returns this frame's AABB snapshots in their exact registration order.
    #[must_use]
    pub fn frame_bounds(&self) -> &[FrameBound<ObjectHandle>] {
        self.solid_frame_bounds.as_slice()
    }

    /// Supplies the current retail camera translation used by shadow sizing.
    pub fn set_camera_translation(&mut self, translation: [i32; 3]) {
        self.camera_translation = translation;
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

    pub fn insert_object(&mut self, object: VmObject) -> Result<(), VmError> {
        let handle = object.handle;
        if self.objects.contains_key(&handle) {
            return Err(VmError::DuplicateObject(handle));
        }
        if self.objects.len() == MAX_OBJECTS {
            return Err(VmError::TooManyObjects);
        }
        self.register_paging_metadata(
            object.page_count,
            &object.resident_pages,
            &object.entry_pages,
        )?;
        self.objects.insert(handle, object);
        Ok(())
    }

    /// Installs or replaces one validated VM object while preserving all
    /// stream-level paging metadata. Runtime pool reuse must take this path;
    /// assigning through `object_mut` would bypass EID/page registration.
    pub fn upsert_object(&mut self, object: VmObject) -> Result<(), VmError> {
        let handle = object.handle;
        if !self.objects.contains_key(&handle) && self.objects.len() == MAX_OBJECTS {
            return Err(VmError::TooManyObjects);
        }
        self.register_paging_metadata(
            object.page_count,
            &object.resident_pages,
            &object.entry_pages,
        )?;
        self.objects.insert(handle, object);
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
        self.paging_page_capacity = self.paging_page_capacity.max(page_count);
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

    /// Rebinds an object after [`HaltReason::StateChanged`].
    pub fn rebind_state_program(
        &mut self,
        handle: ObjectHandle,
        program: &VmStateProgram,
        arguments: &[u32],
    ) -> Result<(), VmError> {
        let frame_stamp = self.frames_elapsed;
        self.register_paging_metadata(
            program.page_count,
            &program.resident_pages,
            &program.entry_pages,
        )?;
        let object = self.object_mut(handle)?;
        object.rebind_state_program(program, arguments, frame_stamp)
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
        )?;
        match execution.reason {
            HaltReason::OnceCompleted => Ok(Some(execution)),
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
        let Some(transition_pc) = object.transition_pc else {
            return Ok(false);
        };
        if transition_pc >= object.code.len() {
            return Err(VmError::InvalidJump {
                object: handle,
                target: transition_pc as i64,
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
        object.code_segment = CodeSegment::External;
        object.pc = transition_pc;
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
        )?;
        match execution.reason {
            HaltReason::TransitionCompleted | HaltReason::StateChanged(_) => Ok(Some(execution)),
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

    #[must_use]
    pub fn take_effects(&mut self) -> Vec<VmEffect> {
        core::mem::take(&mut self.effects)
    }

    pub fn run(&mut self, handle: ObjectHandle, budget: usize) -> Result<Execution, VmError> {
        if let Some(execution) = self.animation_gate(handle)? {
            return Ok(execution);
        }
        let mut condition = false;
        for steps in 0..budget {
            if let Some(reason) = self.step(handle, &mut condition)? {
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

    /// Runs one interpreter invocation while applying spawn effects before the
    /// following instruction, matching retail's synchronous host call.
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
        self.run_with_host_effects_mode(handle, budget, host, true, true)
    }

    fn run_with_host_effects_mode<F>(
        &mut self,
        handle: ObjectHandle,
        budget: usize,
        mut host: F,
        suspend_on_animation: bool,
        apply_animation_gate: bool,
    ) -> Result<Execution, VmError>
    where
        F: FnMut(&mut Self, &VmEffect) -> Result<(), VmError>,
    {
        if apply_animation_gate && let Some(execution) = self.animation_gate(handle)? {
            return Ok(execution);
        }
        let mut condition = false;
        for steps in 0..budget {
            if let Some(reason) = self.step(handle, &mut condition)? {
                if reason == HaltReason::HostEffect {
                    let effect = self
                        .effects
                        .last()
                        .cloned()
                        .ok_or(VmError::MissingHostEffect)?;
                    if !matches!(effect, VmEffect::SpawnChildren { .. }) {
                        return Err(VmError::MissingHostEffect);
                    }
                    host(self, &effect)?;
                    continue;
                }
                if !suspend_on_animation && matches!(reason, HaltReason::AnimationChanged { .. }) {
                    continue;
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

    fn step(
        &mut self,
        handle: ObjectHandle,
        condition: &mut bool,
    ) -> Result<Option<HaltReason>, VmError> {
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
            0x04 => self.binary_push(handle, a, b, |left, right| u32::from(left == right))?,
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
                self.binary_push(handle, a, b, |left, right| u32::from(left & right == left))?;
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
                let value = self.read_operand(handle, a)?;
                self.write_operand(handle, b, value)?;
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
            0x15 => {
                let shift = self.read_operand(handle, a)? as i32;
                let value = self.read_operand(handle, b)? as i32;
                let magnitude = shift.unsigned_abs();
                if magnitude >= 32 {
                    return Err(VmError::InvalidShift(shift));
                }
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
                    self.object_mut(handle)?.set_register(register, reference)?;
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
                let command = self.read_operand(handle, a)? as u8;
                match command {
                    0 => self.emit(VmEffect::SaveState(handle))?,
                    1 => self.emit(VmEffect::LoadState(handle))?,
                    9 => {
                        let level = self.read_operand(handle, b)? as i32;
                        self.emit(VmEffect::Transition(level))?;
                    }
                    _ => return Err(VmError::UnknownControl(command)),
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
                self.push(handle, value)?;
            }
            0x20 => {
                let value = self.read_operand(handle, a)?;
                let index = (self.read_operand(handle, b)? >> 8) as usize;
                *self
                    .globals
                    .get_mut(index)
                    .ok_or(VmError::InvalidRegister(index))? = value;
            }
            0x21 => {
                let target = Angle12::new(self.read_operand(handle, a)? as i32);
                let current = Angle12::new(self.read_operand(handle, b)? as i32);
                self.push(handle, i32::from(current.difference_to(target)) as u32)?;
            }
            0x22 => {
                let target = self.read_operand(handle, a)? as i32;
                let current = self.read_operand(handle, b)? as i32;
                self.push(handle, seek(current, target, 0x100) as u32)?;
            }
            0x23 => {
                let link = ((word >> 12) & 7) as usize;
                let color = ((word >> 15) & 0x3f) as usize;
                let value = if let Some(target) = self.object(handle)?.links[link] {
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
                if let Some(target) = self.object(handle)?.links[link] {
                    self.object_mut(target)?.set_color(color, value)?;
                }
            }
            0x25 => {
                let target = Angle12::new(self.read_operand(handle, a)? as i32);
                let current = Angle12::new(self.read_operand(handle, b)? as i32);
                let difference = i32::from(current.difference_to(target));
                let delta = difference.clamp(-0x100, 0x100);
                self.push(handle, u32::from(current.wrapping_add(delta).raw()))?;
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
                })?;
                return Ok(Some(HaltReason::AnimationChanged {
                    frame,
                    wait: wait as u8,
                }));
            }
            0x85 => {
                // Retail translates B once before dispatching the packed
                // transform selector. For stack operands that pop must occur
                // even when the object has no entity path.
                let input = self.read_optional_input(handle, b)?;
                let suboperation = ((word >> 18) & 7) as u8;
                let input_vector = ((word >> 12) & 7) as u8;
                let output_vector = ((word >> 15) & 7) as u8;
                match suboperation {
                    0 => {
                        if let Some(progress) = input {
                            self.object_mut(handle)?
                                .orient_process_vector_on_path(progress as i32, input_vector)?;
                        }
                    }
                    _ => {
                        return Err(VmError::UnsupportedTransformVectors {
                            suboperation,
                            input_vector,
                            output_vector,
                            operand: instruction.operand_b,
                        });
                    }
                }
            }
            0x86 => {
                let argument_count = ((word >> 20) & 0x0f) as usize;
                let target = (word & 0x3fff) as usize;
                self.call_global(handle, target, argument_count)?;
            }
            0x87 | 0x90 => {
                let link_index = ((word >> 21) & 7) as usize;
                let recipient =
                    self.object(handle)?.links[link_index].ok_or(VmError::MissingLink {
                        object: handle,
                        link: link_index as u8,
                    })?;
                let event = self.read_operand(handle, b)?;
                self.emit(VmEffect::Event {
                    sender: handle,
                    recipient: Some(recipient),
                    event,
                })?;
            }
            0x8f => {
                let event = self.read_operand(handle, b)?;
                self.emit(VmEffect::Event {
                    sender: handle,
                    recipient: None,
                    event,
                })?;
            }
            0x88 | 0x89 => {
                return Err(VmError::UnsupportedEventServiceReturn {
                    opcode: instruction.opcode,
                    condition_type: ((word >> 20) & 3) as u8,
                    return_type: ((word >> 22) & 3) as u8,
                    register: ((word >> 14) & 0x3f) as u8,
                });
            }
            0x8a | 0x91 => {
                if self.spawn_children(handle, word, instruction.opcode == 0x91)? {
                    return Ok(Some(HaltReason::HostEffect));
                }
            }
            0x8b => {
                self.paging_operation(handle, a, b)?;
            }
            0x8c => {
                let voice = self.read_operand(handle, a)?;
                let sound = self.read_operand(handle, b)?;
                self.emit(VmEffect::AudioStart {
                    object: handle,
                    voice,
                    sound,
                })?;
            }
            0x8d => {
                let command = self.read_operand(handle, a)?;
                let value = self.read_operand(handle, b)?;
                self.emit(VmEffect::AudioControl {
                    object: handle,
                    command,
                    value,
                })?;
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
                    self.object_mut(handle)?.scale_colors_for_entity_node()?;
                } else if suboperation == 1 {
                    self.react_solid_surface_suboperation_one(
                        handle,
                        input_vector,
                        output_vector,
                        output_reference,
                    )?;
                } else if suboperation == 3 {
                    self.react_solid_surface_suboperation_three(
                        handle,
                        input_vector,
                        output_vector,
                        output_reference,
                    )?;
                } else {
                    return Err(VmError::UnsupportedSolidSurface {
                        suboperation,
                        input_vector,
                        output_vector,
                        operand: instruction.operand_b,
                    });
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

        for snapshot in &self.solid_frame_bounds {
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
            .object(handle)?
            .solid_environment
            .clone()
            .ok_or(VmError::MissingSolidEnvironment(handle))?;
        let parent = self.object(handle)?.links[1];
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

    fn react_solid_surface_suboperation_three(
        &mut self,
        handle: ObjectHandle,
        input_vector: u8,
        output_vector: u8,
        output_reference: Option<StorageReference>,
    ) -> Result<(), VmError> {
        // The source copies this vector before entering the switch even though
        // suboperation three does not subsequently consume the copy.
        let _input = self.object(handle)?.process_vector(input_vector)?;
        let translation = self.object(handle)?.process_vector(0)?;
        // `trans3` is static in C, but this case overwrites it on every call.
        // `ZoneFindNearestObjectNode3` mutates only a local copy and never
        // writes through its vector pointer, so the final vector is exactly
        // the current translation.
        self.solid_trans3 = translation;

        let child_status = self.object(handle)?.register(process_register::STATUS_B)?;
        let mut active_parent = None;
        if child_status & 0x0400_0000 != 0 {
            let parent = self.object(handle)?.links[1].ok_or(VmError::MissingLink {
                object: handle,
                link: 1,
            })?;
            if self.object(parent)?.register(process_register::STATUS_B)? & 0x0400_0000 != 0 {
                active_parent = Some(parent);
            }
        }

        if let Some(parent) = active_parent {
            let environment = self
                .object(handle)?
                .solid_environment
                .clone()
                .ok_or(VmError::MissingSolidEnvironment(handle))?;
            let flags = if environment.graphics_flags & 1 != 0 {
                6
            } else {
                5
            };
            let (node, _) = find_retail_solid_node(&environment, translation, flags & 3, 25_000)?;

            // Source collision bounds are a per-frame traversal-order list of
            // animation-derived AABBs, not all live objects. NODE==0xffff is
            // the exact source skip gate and covers the characterized legal
            // path. Any other candidate remains an honest typed host boundary
            // until that ordered bounds list is owned by Rust.
            if let Some(candidate) = self.objects.iter().find_map(|(candidate, object)| {
                if *candidate == handle {
                    return None;
                }
                object
                    .register(process_register::NODE)
                    .ok()
                    .filter(|node| *node != 0xffff)
                    .map(|_| *candidate)
            }) {
                return Err(VmError::UnsupportedSolidObjectBounds(candidate));
            }

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
            let mut subtype = node.map_or(-1, |node| i32::from((node & 0x03f0) >> 4));
            if subtype < 39 {
                subtype = -1;
            }
            if self.object(parent)?.is_main_player && self.object(parent)?.state_flags & 0x20 != 0 {
                subtype = 0x37;
            }
            let target = scaled_retail_colors(&source, subtype)?;
            seek_retail_colors(&mut self.object_mut(parent)?.colors, target, 0x015e);
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

    fn paging_operation(
        &mut self,
        handle: ObjectHandle,
        operation_operand: Operand,
        argument_operand: Operand,
    ) -> Result<(), VmError> {
        let operation = self.read_operand(handle, operation_operand)?;
        // `GoolOpPaging` retains the address produced by GOP translation. It
        // does not first dereference B like an ordinary scalar opcode.
        let argument = self.input_reference(handle, argument_operand)?;
        match operation {
            1 | 6 => {
                let reference = argument.ok_or(VmError::InvalidPagingOperation(operation))?;
                let (entry, page) = self.resolve_entry_argument(reference)?;
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
                    open: true,
                    reference: entry.to_word(),
                })?;
            }
            2 => {
                let reference = argument.ok_or(VmError::InvalidPagingOperation(operation))?;
                let (entry, page) = self.resolve_entry_argument(reference)?;
                let result = self.close_paging_page(page, true);
                self.object_mut(handle)?
                    .set_register(process_register::MISC_VALUE, result)?;
                self.emit(VmEffect::Paging {
                    object: handle,
                    open: false,
                    reference: entry.to_word(),
                })?;
            }
            3 => {
                let reference = argument.ok_or(VmError::InvalidPagingOperation(operation))?;
                let (entry, page) = self.resolve_entry_argument(reference)?;
                self.emit(VmEffect::Paging {
                    object: handle,
                    open: false,
                    reference: entry.to_word(),
                })?;
                // `NSClose(ref, 0)` is a query: resolved PC PTEs return
                // literal one; unresolved pages return zero. It does not
                // decrement the count.
                let result = self.close_paging_page(page, false);
                self.push(handle, result)?;
                // Retail case three deliberately falls through to case four.
                self.push(handle, self.available_page_count())?;
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
                            (self.entry_reference_page(entry)?, true)
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
        Ok(())
    }

    fn close_paging_page(&mut self, page: PageIndex, decrement: bool) -> u32 {
        if !self.paging_loaded_pages.contains(&page) {
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
            .values()
            .filter(|references| **references != 0)
            .count() as u32;
        self.paging_page_capacity.saturating_sub(referenced)
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
                let value = if pops_condition {
                    self.object(handle)?
                        .stack
                        .last()
                        .copied()
                        .ok_or(VmError::StackUnderflow(handle))?
                } else {
                    self.object(handle)?.register(register)?
                };
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

        if tested && operation == 0 {
            let offset = i64::from(sign_extend(instruction & 0x03ff, 10));
            self.jump_relative(handle, offset)?;
        }
        if pops_condition {
            self.pop(handle)?;
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
        let right = self.read_operand(handle, a)? as i32;
        let left = self.read_operand(handle, b)? as i32;
        self.push(handle, u32::from(operation(left, right)))
    }

    fn store_input_constant(&mut self, value: u32) -> usize {
        self.input_constant_index ^= 1;
        self.operand_constants[self.input_constant_index] = value;
        self.input_constant_index
    }

    fn store_output_constant(&mut self, value: u32) -> usize {
        self.output_constant_index ^= 1;
        self.operand_constants[self.output_constant_index] = value;
        self.output_constant_index
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
                self.object(handle)?.register(index)?;
                StorageReference::checked(handle, StorageRegion::Register, index)?
            }
            Operand::Null => return Ok(None),
            Operand::StackDouble => return Err(VmError::UnsupportedReferenceOperand(0x0bf0)),
            Operand::LinkRegister { link, register } => {
                let Some(target) = self.object(handle)?.links[usize::from(link)] else {
                    return Ok(None);
                };
                self.object(target)?.register(usize::from(register))?;
                StorageReference::checked(target, StorageRegion::Register, usize::from(register))?
            }
            Operand::ObjectRegister(index) => {
                self.object(handle)?.register(usize::from(index))?;
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
                self.object(handle)?.register(index)?;
                StorageReference::checked(handle, StorageRegion::Register, index)?
            }
            Operand::Null => return Ok(None),
            Operand::StackDouble => return Err(VmError::UnsupportedReferenceOperand(0x0bf0)),
            Operand::LinkRegister { link, register } => {
                let Some(target) = self.object(handle)?.links[usize::from(link)] else {
                    return Ok(None);
                };
                self.object(target)?.register(usize::from(register))?;
                StorageReference::checked(target, StorageRegion::Register, usize::from(register))?
            }
            Operand::ObjectRegister(index) => {
                self.object(handle)?.register(usize::from(index))?;
                StorageReference::checked(handle, StorageRegion::Register, usize::from(index))?
            }
            Operand::Stack => {
                let (register_index, previous) = {
                    let object = self.object(handle)?;
                    let register_index = (object.initial_stack_pointer as usize)
                        .checked_add(object.stack.len())
                        .ok_or(VmError::StackOverflow(handle))?;
                    (register_index, object.register(register_index)?)
                };
                // Translating an output stack GOP advances SP but does not
                // itself write the pointed-to word. Retain the stale bounded
                // register value until a caller stores through the reference.
                self.push(handle, previous)?;
                StorageReference::checked(handle, StorageRegion::Register, register_index)?
            }
        };
        Ok(Some(reference))
    }

    /// Resolves one tagged storage word through bounded VM-owned arrays.
    pub fn read_storage_reference(&self, reference: StorageReference) -> Result<u32, VmError> {
        let index = usize::from(reference.index);
        match reference.region {
            StorageRegion::Internal => self
                .object(reference.object)?
                .internal
                .get(index)
                .copied()
                .ok_or(VmError::InvalidStorageReference(reference.to_word())),
            StorageRegion::External => self
                .object(reference.object)?
                .external
                .get(index)
                .copied()
                .ok_or(VmError::InvalidStorageReference(reference.to_word())),
            StorageRegion::Register => self
                .object(reference.object)?
                .register(index)
                .map_err(|_| VmError::InvalidStorageReference(reference.to_word())),
            StorageRegion::Constant => self
                .operand_constants
                .get(index)
                .copied()
                .ok_or(VmError::InvalidStorageReference(reference.to_word())),
        }
    }

    fn write_storage_reference(
        &mut self,
        reference: StorageReference,
        value: u32,
    ) -> Result<(), VmError> {
        let index = usize::from(reference.index);
        match reference.region {
            StorageRegion::Internal => {
                *self
                    .object_mut(reference.object)?
                    .internal
                    .get_mut(index)
                    .ok_or(VmError::InvalidStorageReference(reference.to_word()))? = value;
                Ok(())
            }
            StorageRegion::External => {
                *self
                    .object_mut(reference.object)?
                    .external
                    .get_mut(index)
                    .ok_or(VmError::InvalidStorageReference(reference.to_word()))? = value;
                Ok(())
            }
            StorageRegion::Register => self
                .object_mut(reference.object)?
                .set_register(index, value)
                .map_err(|_| VmError::InvalidStorageReference(reference.to_word())),
            StorageRegion::Constant => {
                *self
                    .operand_constants
                    .get_mut(index)
                    .ok_or(VmError::InvalidStorageReference(reference.to_word()))? = value;
                Ok(())
            }
        }
    }

    fn write_storage_span3(
        &mut self,
        reference: StorageReference,
        values: [u32; 3],
    ) -> Result<(), VmError> {
        let base = usize::from(reference.index);
        let references = [0_usize, 1, 2].map(|offset| {
            let index = base
                .checked_add(offset)
                .ok_or(VmError::InvalidStorageReference(reference.to_word()))?;
            StorageReference::checked(reference.object, reference.region, index)
        });
        let references = [references[0]?, references[1]?, references[2]?];
        // Validate the complete C `vec` span before mutating so malformed
        // tagged storage cannot leave a partial triple behind.
        for reference in references {
            self.read_storage_reference(reference)?;
        }
        for (reference, value) in references.into_iter().zip(values) {
            self.write_storage_reference(reference, value)?;
        }
        Ok(())
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
        self.paging_entry_references
            .get(reference.slot as usize)
            .map(|(_, page)| *page)
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
        match operand {
            Operand::Internal(index) => self
                .object(handle)?
                .internal
                .get(usize::from(index))
                .copied()
                .ok_or(VmError::InvalidRegister(usize::from(index))),
            Operand::External(index) => self
                .object(handle)?
                .external
                .get(usize::from(index))
                .copied()
                .ok_or(VmError::InvalidRegister(usize::from(index))),
            Operand::Immediate(value) => {
                self.store_input_constant(value as u32);
                Ok(value as u32)
            }
            Operand::FrameRelative(offset) => {
                let base = self.object(handle)?.frame_base;
                let index = base
                    .checked_add_signed(isize::from(offset))
                    .ok_or(VmError::InvalidOperand(0))?;
                self.object(handle)?.register(index)
            }
            Operand::Null => Ok(NULL_INPUT_VALUE),
            Operand::StackDouble => Err(VmError::InvalidOperand(0x0bf0)),
            Operand::ObjectRegister(index) => self.object(handle)?.register(usize::from(index)),
            Operand::Stack => self.pop(handle),
            Operand::LinkRegister { link, register } => {
                let Some(target) = self.object(handle)?.links[usize::from(link)] else {
                    return Ok(NULL_INPUT_VALUE);
                };
                self.object(target)?.register(usize::from(register))
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
        if let Some(reference) = self.output_reference(handle, operand)? {
            self.write_storage_reference(reference, value)?;
        }
        Ok(())
    }

    fn push(&mut self, handle: ObjectHandle, value: u32) -> Result<(), VmError> {
        self.object_mut(handle)?.push_stack_word(value)
    }

    fn pop(&mut self, handle: ObjectHandle) -> Result<u32, VmError> {
        self.object_mut(handle)?
            .stack
            .pop()
            .ok_or(VmError::StackUnderflow(handle))
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
            self.object_mut(handle)?.halted = true;
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
        let arguments = self.object(handle)?.stack[argument_start..argument_end].to_vec();
        self.object_mut(handle)?.stack.truncate(argument_start);
        if signed_count > 0 {
            self.emit(VmEffect::SpawnChildren {
                parent: handle,
                executable,
                subtype,
                count,
                allow_reclaim,
                arguments,
            })?;
            return Ok(true);
        }
        Ok(false)
    }

    fn emit(&mut self, effect: VmEffect) -> Result<(), VmError> {
        if self.effects.len() == MAX_EFFECTS {
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
    use proptest::prelude::*;

    const REG0: u16 = 0x0e00;
    const REG1: u16 = 0x0e01;
    const STACK: u16 = 0x0e1f;

    fn handle(index: u16) -> ObjectHandle {
        ObjectHandle::new(index).unwrap()
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
    fn aligned_tagged_references_round_trip_without_eid_low_bits() {
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

        let entry = EntryReference {
            slot: ENTRY_REFERENCE_SLOT_BITS,
        };
        let entry_word = entry.to_word();
        assert_eq!(
            entry_word,
            ENTRY_REFERENCE_TAG | ENTRY_REFERENCE_PAYLOAD_MASK
        );
        assert_eq!(EntryReference::from_word(entry_word), Some(entry));

        for word in [code_word, storage_word, entry_word] {
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
        let entry_word = EntryReference { slot: 7 }.to_word();

        for low_bits in 1..=3 {
            assert_eq!(CodeAddress::from_word(code_word | low_bits), None);
            assert_eq!(StorageReference::from_word(storage_word | low_bits), None);
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

        let named_eid_word = entry_word | 1;
        assert!(Eid::from_raw(named_eid_word).is_named());
        assert_eq!(EntryReference::from_word(named_eid_word), None);
    }

    #[test]
    fn collision_object_references_validate_alignment_reserved_bits_and_pool_range() {
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
            CollisionObjectReference::from_word(COLLISION_OBJECT_REFERENCE_TAG | (96 << 2)),
            None,
            "slot 96 is outside the retail object pool"
        );
        assert_eq!(
            CollisionObjectReference::from_word(COLLISION_OBJECT_REFERENCE_TAG | (1 << 9)),
            None,
            "bits outside the seven-bit shifted handle are reserved"
        );
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
                },
            ]
        );
    }

    #[test]
    fn solid_surface_opcode_reports_its_exact_unimplemented_selector() {
        let h = handle(0);
        let instruction = (0x8e_u32 << 24) | (5 << 18) | (3 << 15) | (2 << 12) | 0x0be0;
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(h, vec![instruction]).unwrap())
            .unwrap();

        assert_eq!(
            machine.run(h, 1),
            Err(VmError::UnsupportedSolidSurface {
                suboperation: 5,
                input_vector: 2,
                output_vector: 3,
                operand: 0x0be0,
            })
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
    fn active_solid_object_bounds_remain_an_explicit_host_boundary() {
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

        let mut child_object = VmObject::new(child, vec![0x8e0e_de26]).unwrap();
        child_object
            .set_register(process_register::STATUS_B, 0x0400_0000)
            .unwrap();
        child_object
            .set_process_vector(0, [25_600, 25_600, 25_600])
            .unwrap();
        child_object.set_link(1, Some(parent)).unwrap();
        child_object.bind_retail_solid_environment(environment.clone());
        let mut parent_object = VmObject::new(parent, vec![0]).unwrap();
        parent_object
            .set_register(process_register::STATUS_B, 0x0400_0000)
            .unwrap();
        parent_object
            .set_register(process_register::NODE, 0xffff)
            .unwrap();
        parent_object.bind_retail_solid_environment(environment);
        let mut candidate_object = VmObject::new(candidate, vec![0]).unwrap();
        candidate_object
            .set_register(process_register::NODE, 0x0301)
            .unwrap();

        let mut machine = Machine::new(0);
        machine.insert_object(parent_object).unwrap();
        machine.insert_object(candidate_object).unwrap();
        machine.insert_object(child_object).unwrap();
        assert_eq!(
            machine.run(child, 1),
            Err(VmError::UnsupportedSolidObjectBounds(candidate))
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
    fn entity_node_color_opcode_keeps_level_dependent_selectors_explicit() {
        let h = handle(0);
        let instruction = (0x8e_u32 << 24) | (6 << 18);
        let mut object = VmObject::new(h, vec![instruction]).unwrap();
        object.entity_spawn_flags = Some(40 << 7);
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        assert_eq!(
            machine.run(h, 1),
            Err(VmError::LevelDependentColorSubtype(40))
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
        let stack_reference = (0x18_u32 << 24) | (0x1f << 14) | 1;
        let mut object = VmObject::new(h, vec![register_reference, stack_reference]).unwrap();
        object.global_code = vec![0; 3];
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        machine.run(h, 2).unwrap();
        assert_eq!(
            machine.object(h).unwrap().register(3),
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
    fn events_audio_and_misc_transitions_are_effects() {
        let a = handle(0);
        let b = handle(1);
        let code = vec![
            Instruction::encode(0x87, 3 << 9, REG0),
            Instruction::encode(0x8c, REG1, REG0),
            Instruction::encode(0x1c, REG1, REG0),
        ];
        let mut object = VmObject::new(a, code).unwrap();
        object.set_link(3, Some(b)).unwrap();
        object.set_register(0, 0x900).unwrap();
        object.set_register(1, 9).unwrap();
        let mut machine = Machine::new(1);
        machine.insert_object(object).unwrap();
        machine
            .insert_object(VmObject::new(b, vec![Instruction::encode(0x82, 0, 0)]).unwrap())
            .unwrap();
        assert_eq!(
            machine.run(a, 3).unwrap().reason,
            HaltReason::BudgetExhausted
        );
        assert!(machine.effects().contains(&VmEffect::Event {
            sender: a,
            recipient: Some(b),
            event: 0x900
        }));
        assert!(machine.effects().contains(&VmEffect::AudioStart {
            object: a,
            voice: 9,
            sound: 0x900
        }));
        assert!(machine.effects().contains(&VmEffect::Transition(0x900)));
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
        assert_eq!(destination.object(), h);
        assert_eq!(destination.region(), StorageRegion::Internal);
        assert_eq!(destination.index(), 0x04c);
        assert_eq!(source.index(), 0x04d);
        assert_eq!(machine.read_storage_reference(destination), Ok(0));
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
    fn paging_open_misc_close_query_and_zero_close_follow_source_counts() {
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

        // Case 3 uses source PC NSClose(count=0): a resolved type-1 page
        // returns literal one, then deliberately falls through to available
        // pages. Only one distinct page has a nonzero reference count.
        machine.run(h, 1).unwrap();
        assert_eq!(machine.object(h).unwrap().stack(), &[1, 3]);

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
        assert_eq!(machine.object(h).unwrap().stack(), &[1, 3, 1, 4]);
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
    fn path_projection_sets_inertia_and_direction_flags_with_source_math() {
        let path = RetailEntityPath {
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
