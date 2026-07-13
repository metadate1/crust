//! Bounded, word-addressed GOOL virtual machine.
//!
//! Instructions retain the retail `opcode:8 | operand-a:12 | operand-b:12`
//! layout. Native pointers are never represented; objects, registers, pages,
//! events, and call targets are checked logical indices.

use std::collections::BTreeMap;

use crust_formats::stream::{GOOL_PC_NONE, GoolProgram, ZoneEntity, structs::GoolState};

use crate::math::{Angle12, seek};

pub const MAX_OBJECTS: usize = 96;
/// Exact `gool_object.regs[0x1FC]` word span from the retail 32-bit layout.
pub const REGISTER_COUNT: usize = 0x1fc;
pub const TABLE_WORD_COUNT: usize = 1024;
pub const MAX_STACK_WORDS: usize = 256;
pub const MAX_CALL_DEPTH: usize = 64;
pub const MAX_EFFECTS: usize = 256;
/// Halfword count in the retail `gool_colors` union.
pub const COLOR_COUNT: usize = 24;
/// Fourteen-bit retail code/PC address space.
pub const MAX_CODE_WORDS: usize = 1 << 14;
pub const NULL_INPUT_VALUE: u32 = 3;
const ANIMATION_REFERENCE_TAG: u32 = 0xa700_0000;
const ANIMATION_REFERENCE_MASK: u32 = 0x00ff_ffff;
const CODE_REFERENCE_TAG: u32 = 0xa600_0000;
const CODE_REFERENCE_GLOBAL: u32 = 0x0080_0000;
const CODE_REFERENCE_PC_MASK: u32 = 0x0000_3fff;
const INITIAL_FRAME_FLAGS: u32 = 0xffff;
const INITIAL_FRAME_WORDS: usize = 4;
const SYNTHETIC_STACK_POINTER: usize = REGISTER_COUNT - MAX_STACK_WORDS;
const NORMAL_INTERPRETER_FLAGS: u32 = 4;

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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodeAddress {
    pub segment: CodeSegment,
    pub pc: usize,
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
        })
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

fn retail_entity_coordinate(point: i16, zone_origin: i32) -> i32 {
    i32::from(point)
        .wrapping_mul(4)
        .wrapping_add(zone_origin)
        .wrapping_mul(0x100)
}

fn encode_code_reference(address: CodeAddress) -> u32 {
    CODE_REFERENCE_TAG
        | match address.segment {
            CodeSegment::External => 0,
            CodeSegment::Global => CODE_REFERENCE_GLOBAL,
        }
        | (u32::try_from(address.pc).unwrap_or(u32::MAX) & CODE_REFERENCE_PC_MASK)
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
    EntityPathTooLong(usize),
    ProgramCounterOutOfBounds {
        object: ObjectHandle,
        pc: usize,
    },
    InvalidOperand(u16),
    InvalidRegister(usize),
    InvalidColor(usize),
    InvalidAnimationOffset(usize),
    InvalidAnimationReference(u32),
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
    /// Opcode `0x26` exposes GOOL storage addresses as scalar words. A future
    /// host must bind those addresses to validated tagged references before
    /// they can safely cross paging/audio instructions.
    UnsupportedInputReference {
        source: u16,
        destination: u16,
    },
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
    argument_base: usize,
    previous_frame_base: usize,
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
    animation_data: Vec<u8>,
    animation_frame: u32,
    animation_wait: Option<AnimationWait>,
    stack: Vec<u32>,
    state_argument_count: usize,
    call_stack: Vec<CallFrame>,
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
            animation_data: Vec::new(),
            animation_frame: 0,
            animation_wait: None,
            stack: Vec::with_capacity(MAX_STACK_WORDS),
            state_argument_count: 0,
            call_stack: Vec::with_capacity(MAX_CALL_DEPTH),
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
        object.global_code = program.global_code().to_vec();
        object.initial_stack_pointer = initial_stack_pointer;
        object.internal[..program.internal_words().len()].copy_from_slice(program.internal_words());
        object.external[..program.external_words().len()].copy_from_slice(program.external_words());
        object.bind_animation_data(program.animation_data());
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

    fn mark_retail_state_change(&mut self, frame_stamp: u32) -> Result<(), VmError> {
        let status_a = self.register(process_register::STATUS_A)? | INITIAL_STATUS_A;
        self.set_register(process_register::STATUS_A, status_a)?;
        self.set_register(process_register::STATE_STAMP, frame_stamp)
    }

    /// Applies the descriptor-owned fields written by `GoolObjectSpawn` and
    /// positions the object at progress zero on its entity path.
    pub fn initialize_retail_entity(
        &mut self,
        entity: &ZoneEntity,
        zone_origin: [i32; 3],
    ) -> Result<(), VmError> {
        let path_length = u32::try_from(entity.path_points.len())
            .map_err(|_| VmError::EntityPathTooLong(entity.path_points.len()))?;
        let path_length = path_length
            .checked_mul(0x100)
            .ok_or(VmError::EntityPathTooLong(entity.path_points.len()))?;
        let first = entity
            .path_points
            .first()
            .ok_or(VmError::EntityPathTooLong(0))?;

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
            retail_entity_coordinate(first.x, zone_origin[0]),
            retail_entity_coordinate(first.y, zone_origin[1]),
            retail_entity_coordinate(first.z, zone_origin[2]),
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

    pub fn set_retail_colors(&mut self, colors: [u16; COLOR_COUNT]) {
        self.colors = colors;
    }

    #[must_use]
    pub const fn retail_colors(&self) -> &[u16; COLOR_COUNT] {
        &self.colors
    }

    /// Installs arguments exactly where retail frame-relative operands expect
    /// them: immediately below the initial frame pointer. The runtime calls
    /// this once after binding a newly spawned object and before interpreting
    /// its state code.
    pub fn initialize_arguments(&mut self, arguments: &[u32]) -> Result<(), VmError> {
        let stack_origin = usize::try_from(self.initial_stack_pointer)
            .map_err(|_| VmError::InvalidInitialStackPointer(self.initial_stack_pointer))?;
        let required = arguments
            .len()
            .checked_add(INITIAL_FRAME_WORDS)
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
        self.push_stack_word(0)?;
        self.animation_wait = Some(AnimationWait {
            stamp: 0,
            frames: 0,
        });
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
        self.animation_data
            .get(reference.offset as usize..)
            .ok_or(VmError::InvalidAnimationReference(reference.to_word()))
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
    ) -> Result<(), VmError> {
        if self.state != program.state_index {
            return Err(VmError::StateProgramMismatch {
                requested: self.state,
                provided: program.state_index,
            });
        }
        if arguments.len() > MAX_STACK_WORDS {
            return Err(VmError::StackOverflow(self.handle));
        }

        self.code.clone_from(&program.code);
        self.external.fill(0);
        self.external[..program.external.len()].copy_from_slice(&program.external);
        self.set_register(process_register::STATE_FLAGS, program.state.flags)?;
        self.set_register(process_register::STATUS_C, program.state.status_c)?;
        self.event_pc = program.event_pc;
        self.transition_pc = program.transition_pc;
        self.code_segment = CodeSegment::External;
        self.pc = program.code_pc.unwrap_or(0);
        self.halted = program.code_pc.is_none();
        self.animation_wait = None;
        self.initialize_arguments(arguments)
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

    pub fn insert_object(&mut self, object: VmObject) -> Result<(), VmError> {
        let handle = object.handle;
        if self.objects.contains_key(&handle) {
            return Err(VmError::DuplicateObject(handle));
        }
        if self.objects.len() == MAX_OBJECTS {
            return Err(VmError::TooManyObjects);
        }
        self.objects.insert(handle, object);
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
        let object = self.object_mut(handle)?;
        object.rebind_state_program(program, arguments)?;
        object.mark_retail_state_change(frame_stamp)
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
        mut host: F,
    ) -> Result<Execution, VmError>
    where
        F: FnMut(&mut Self, &VmEffect) -> Result<(), VmError>,
    {
        if let Some(execution) = self.animation_gate(handle)? {
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
            0x19 => {
                let value = self.read_operand(handle, a)? as i32;
                self.write_operand(
                    handle,
                    b,
                    value.checked_abs().ok_or(VmError::ArithmeticOverflow)? as u32,
                )?;
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
                return Err(VmError::UnsupportedInputReference {
                    source: instruction.operand_a,
                    destination: instruction.operand_b,
                });
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
                let state = (self.read_operand(handle, b)? >> 8) as u16;
                self.object_mut(handle)?.state = state;
                self.emit(VmEffect::StateChanged {
                    object: handle,
                    state,
                })?;
                return Ok(Some(HaltReason::StateChanged(state)));
            }
            0x8a | 0x91 => {
                if self.spawn_children(handle, word, instruction.opcode == 0x91)? {
                    return Ok(Some(HaltReason::HostEffect));
                }
            }
            0x8b => {
                let open = self.read_operand(handle, a)? != 0;
                let reference = self.read_operand(handle, b)?;
                self.emit(VmEffect::Paging {
                    object: handle,
                    open,
                    reference,
                })?;
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
                return Err(VmError::UnsupportedSolidSurface {
                    suboperation: ((word >> 18) & 7) as u8,
                    input_vector: ((word >> 12) & 7) as u8,
                    output_vector: ((word >> 15) & 7) as u8,
                    operand: instruction.operand_b,
                });
            }
            opcode => return Err(VmError::UnknownOpcode(opcode)),
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
            Operand::Immediate(value) => Ok(value as u32),
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
        match operand {
            Operand::Internal(index) => {
                *self
                    .object_mut(handle)?
                    .internal
                    .get_mut(usize::from(index))
                    .ok_or(VmError::InvalidRegister(usize::from(index)))? = value;
                Ok(())
            }
            Operand::External(index) => {
                *self
                    .object_mut(handle)?
                    .external
                    .get_mut(usize::from(index))
                    .ok_or(VmError::InvalidRegister(usize::from(index)))? = value;
                Ok(())
            }
            Operand::FrameRelative(offset) => {
                let base = self.object(handle)?.frame_base;
                let index = base
                    .checked_add_signed(isize::from(offset))
                    .ok_or(VmError::InvalidOperand(0))?;
                self.object_mut(handle)?.set_register(index, value)
            }
            Operand::ObjectRegister(index) => self
                .object_mut(handle)?
                .set_register(usize::from(index), value),
            Operand::Stack => self.push(handle, value),
            Operand::LinkRegister { link, register } => {
                let Some(target) = self.object(handle)?.links[usize::from(link)] else {
                    return Ok(());
                };
                self.object_mut(target)?
                    .set_register(usize::from(register), value)
            }
            // Retail output translation returns nullptr for the null GOP and
            // every writer checks it before storing. Input translation still
            // occurred first, so stack-pop side effects are preserved.
            Operand::Null => Ok(()),
            Operand::Immediate(_) | Operand::StackDouble => Err(VmError::InvalidOperand(0)),
        }
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
            argument_base,
            previous_frame_base,
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
        Ok(None)
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
    fn events_audio_paging_state_and_transitions_are_effects() {
        let a = handle(0);
        let b = handle(1);
        let code = vec![
            Instruction::encode(0x87, 3 << 9, REG0),
            Instruction::encode(0x8c, REG1, REG0),
            Instruction::encode(0x8b, REG1, REG0),
            Instruction::encode(0x1c, REG1, REG0),
            Instruction::encode(0x88, 0, REG0),
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
            machine.run(a, 20).unwrap().reason,
            HaltReason::StateChanged(9)
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
    fn pointer_exposing_dual_input_reports_exact_tagging_boundary() {
        let h = handle(0);
        let mut machine = Machine::new(0);
        machine
            .insert_object(VmObject::new(h, vec![0x2604_d04c]).unwrap())
            .unwrap();

        assert_eq!(
            machine.run(h, 1),
            Err(VmError::UnsupportedInputReference {
                source: 0x04d,
                destination: 0x04c,
            })
        );
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

    proptest! {
        #[test]
        fn instruction_fields_round_trip(opcode in any::<u8>(), a in 0_u16..0x1000, b in 0_u16..0x1000) {
            let decoded = Instruction::decode(Instruction::encode(opcode, a, b));
            prop_assert_eq!(decoded, Instruction { opcode, operand_a: a, operand_b: b });
        }
    }
}
