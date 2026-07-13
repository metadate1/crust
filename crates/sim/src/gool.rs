//! Bounded, word-addressed GOOL virtual machine.
//!
//! Instructions retain the retail `opcode:8 | operand-a:12 | operand-b:12`
//! layout. Native pointers are never represented; objects, registers, pages,
//! events, and call targets are checked logical indices.

use std::collections::BTreeMap;

use crust_formats::stream::GoolProgram;

use crate::math::{Angle12, seek};

pub const MAX_OBJECTS: usize = 96;
/// Exact `gool_object.regs[0x1FC]` word span from the retail 32-bit layout.
pub const REGISTER_COUNT: usize = 0x1fc;
pub const TABLE_WORD_COUNT: usize = 1024;
pub const MAX_STACK_WORDS: usize = 256;
pub const MAX_CALL_DEPTH: usize = 64;
pub const MAX_EFFECTS: usize = 256;
/// Fourteen-bit retail code/PC address space.
pub const MAX_CODE_WORDS: usize = 1 << 14;
pub const NULL_INPUT_VALUE: u32 = 3;

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
        alternate_parent: bool,
        arguments: Vec<u32>,
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
    ProgramCounterOutOfBounds { object: ObjectHandle, pc: usize },
    InvalidOperand(u16),
    InvalidRegister(usize),
    MissingLink { object: ObjectHandle, link: u8 },
    StackUnderflow(ObjectHandle),
    StackOverflow(ObjectHandle),
    CallStackOverflow(ObjectHandle),
    InvalidJump { object: ObjectHandle, target: i64 },
    DivisionByZero,
    ArithmeticOverflow,
    InvalidShift(i32),
    SpawnCountTooLarge(u32),
    UnknownOpcode(u8),
    UnknownControl(u8),
    EffectQueueFull,
}

/// Why an interpreter invocation yielded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HaltReason {
    Halted,
    Yielded,
    StateChanged(u16),
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
    stack: Vec<u32>,
    call_stack: Vec<CallFrame>,
    links: [Option<ObjectHandle>; 8],
    state: u16,
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
            initial_stack_pointer: 0,
            frame_base: 0,
            internal: vec![0; TABLE_WORD_COUNT],
            external: vec![0; TABLE_WORD_COUNT],
            registers: vec![0; REGISTER_COUNT],
            stack: Vec::with_capacity(MAX_STACK_WORDS),
            call_stack: Vec::with_capacity(MAX_CALL_DEPTH),
            links: [None; 8],
            state: 0,
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
        if usize::try_from(initial_stack_pointer).map_or(true, |value| value >= REGISTER_COUNT) {
            return Err(VmError::InvalidInitialStackPointer(initial_stack_pointer));
        }

        let mut object = Self::new(handle, program.code().to_vec())?;
        object.global_code = program.global_code().to_vec();
        object.initial_stack_pointer = initial_stack_pointer;
        object.internal[..program.internal_words().len()].copy_from_slice(program.internal_words());
        object.external[..program.external_words().len()].copy_from_slice(program.external_words());
        object.state = program.state_index();
        object.state_flags = program.state().flags;
        object.status_c = program.state().status_c;
        object.event_pc = program.event_pc();
        object.transition_pc = program.transition_pc();
        if let Some(pc) = program.code_pc() {
            object.pc = pc;
        } else {
            object.halted = true;
        }
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
        Ok(())
    }

    pub fn register(&self, index: usize) -> Result<u32, VmError> {
        self.registers
            .get(index)
            .copied()
            .ok_or(VmError::InvalidRegister(index))
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
}

impl Machine {
    #[must_use]
    pub fn new(global_words: usize) -> Self {
        Self {
            objects: BTreeMap::new(),
            globals: vec![0; global_words],
            effects: Vec::new(),
        }
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

    #[must_use]
    pub fn effects(&self) -> &[VmEffect] {
        &self.effects
    }

    #[must_use]
    pub fn take_effects(&mut self) -> Vec<VmEffect> {
        core::mem::take(&mut self.effects)
    }

    pub fn run(&mut self, handle: ObjectHandle, budget: usize) -> Result<Execution, VmError> {
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
            0x11 => {
                let value = self.read_operand(handle, a)?;
                self.write_operand(handle, b, value)?;
            }
            0x12 => {
                let value = u32::from(self.read_operand(handle, a)? == 0);
                self.write_operand(handle, b, value)?;
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
            0x25 => {
                let target = Angle12::new(self.read_operand(handle, a)? as i32);
                let current = Angle12::new(self.read_operand(handle, b)? as i32);
                let difference = i32::from(current.difference_to(target));
                let delta = difference.clamp(-0x100, 0x100);
                self.push(handle, u32::from(current.wrapping_add(delta).raw()))?;
            }
            0x82 => {
                return self.control_flow(handle, word, condition);
            }
            0x86 => {
                let argument_count = ((word >> 20) & 0x0f) as usize;
                let target = (word & 0x3fff) as usize;
                self.call_global(handle, target, argument_count)?;
            }
            0x87 | 0x90 => {
                let link_index = (instruction.operand_a & 7) as usize;
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
                self.spawn_children(handle, word, instruction.opcode == 0x91)?;
                return Ok(Some(HaltReason::Yielded));
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
                self.object(handle)?
                    .registers
                    .get(index)
                    .copied()
                    .ok_or(VmError::InvalidRegister(index))
            }
            Operand::Null => Ok(NULL_INPUT_VALUE),
            Operand::StackDouble => Err(VmError::InvalidOperand(0x0bf0)),
            Operand::ObjectRegister(index) => self.object(handle)?.register(usize::from(index)),
            Operand::Stack => self.pop(handle),
            Operand::LinkRegister { link, register } => {
                let target =
                    self.object(handle)?.links[usize::from(link)].ok_or(VmError::MissingLink {
                        object: handle,
                        link,
                    })?;
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
                let target =
                    self.object(handle)?.links[usize::from(link)].ok_or(VmError::MissingLink {
                        object: handle,
                        link,
                    })?;
                self.object_mut(target)?
                    .set_register(usize::from(register), value)
            }
            Operand::Immediate(_) | Operand::Null | Operand::StackDouble => {
                Err(VmError::InvalidOperand(0))
            }
        }
    }

    fn push(&mut self, handle: ObjectHandle, value: u32) -> Result<(), VmError> {
        let stack = &mut self.object_mut(handle)?.stack;
        if stack.len() == MAX_STACK_WORDS {
            return Err(VmError::StackOverflow(handle));
        }
        stack.push(value);
        Ok(())
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
        let frame = CallFrame {
            return_address: object.code_address(),
            argument_base: object.stack.len() - argument_count,
            previous_frame_base: object.frame_base,
        };
        let object = self.object_mut(handle)?;
        object.frame_base = object.stack.len();
        object.call_stack.push(frame);
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
        alternate_parent: bool,
    ) -> Result<(), VmError> {
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
        if count > MAX_OBJECTS as u32 {
            return Err(VmError::SpawnCountTooLarge(count));
        }

        let argument_start = stack_len - encoded_argument_count;
        let argument_end = argument_start + argument_count;
        let arguments = self.object(handle)?.stack[argument_start..argument_end].to_vec();
        self.object_mut(handle)?.stack.truncate(argument_start);
        if (count as i32) > 0 {
            self.emit(VmEffect::SpawnChildren {
                parent: handle,
                executable,
                subtype,
                count,
                alternate_parent,
                arguments,
            })?;
        }
        Ok(())
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
    fn exact_crash_child_spawn_yields_a_pointer_free_host_request() {
        let h = handle(0);
        let mut object =
            VmObject::new(h, vec![Instruction::encode(0x00, REG0, REG1), 0x8a10_5001]).unwrap();
        object.set_register(0, 0).unwrap();
        object.set_register(1, 0).unwrap();
        let mut machine = Machine::new(0);
        machine.insert_object(object).unwrap();

        assert_eq!(
            machine.run(h, 2).unwrap(),
            Execution {
                reason: HaltReason::Yielded,
                steps: 2,
            }
        );
        assert!(machine.object(h).unwrap().stack().is_empty());
        assert_eq!(
            machine.effects(),
            &[VmEffect::SpawnChildren {
                parent: h,
                executable: 5,
                subtype: 0,
                count: 1,
                alternate_parent: false,
                arguments: vec![0],
            }]
        );
    }

    #[test]
    fn events_audio_paging_state_and_transitions_are_effects() {
        let a = handle(0);
        let b = handle(1);
        let code = vec![
            Instruction::encode(0x87, 0, REG0),
            Instruction::encode(0x8c, REG1, REG0),
            Instruction::encode(0x8b, REG1, REG0),
            Instruction::encode(0x1c, REG1, REG0),
            Instruction::encode(0x88, 0, REG0),
        ];
        let mut object = VmObject::new(a, code).unwrap();
        object.set_link(0, Some(b)).unwrap();
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
