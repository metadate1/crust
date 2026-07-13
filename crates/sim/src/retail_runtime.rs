//! Safe coordination between retail zone spawns, the object forest, and GOOL.
//!
//! [`ObjectArena`] owns allocation, generations, persistent spawn flags, and
//! retail tree order. [`Machine`] owns executable GOOL state and uses a
//! separate compact handle space. This module is the only place that pairs the
//! two identities, so a stale arena generation can never be mistaken for a
//! live VM object.

use std::collections::{BTreeMap, BTreeSet};

use crust_formats::{
    binary::{Eid, FormatError},
    stream::{
        Nsd, Nsf, ZoneEntity, ZoneHeader, ZoneRect, load_gool_program, load_gool_state_program,
    },
};

use crate::{
    gool::{
        COLOR_COUNT, Execution, HaltReason, MAX_OBJECTS, Machine, ObjectHandle as VmObjectHandle,
        RetailPadSnapshot, RetailSolidEnvironment, RetailSolidZone, VmEffect, VmError, VmObject,
        VmStateProgram,
    },
    object_arena::{
        EntitySpawnDescriptor, NeighborZone, ObjectArena, ObjectHandle as ArenaObjectHandle,
        ROOT_HANDLE_COUNT, RootHandle, RuntimeCreateError, SpawnError, SpawnedObject, TreeError,
        TreeParent,
    },
};

/// A malformed transition graph must not monopolize the browser's
/// cooperative frame. Retail follows state links synchronously; this bound
/// preserves that ordering while reporting cycles as a typed VM failure.
const MAX_SYNCHRONOUS_STATE_CHANGES: usize = 64;

/// One live object identity at the arena/VM boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RuntimeObjectHandle {
    arena: ArenaObjectHandle,
    vm: VmObjectHandle,
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

/// Fully typed request to materialize code/data for a changed GOOL state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StateProgramBinding {
    pub object: RuntimeObjectHandle,
    pub zone: Eid,
    pub executable: u8,
    pub state: u16,
}

/// Immutable zone inputs needed to reproduce `GoolObjectSpawn` without
/// retaining native pointers into a ZDAT entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailZoneEnvironment {
    pub origin: [i32; 3],
    pub object_colors: [u16; COLOR_COUNT],
    pub player_colors: [u16; COLOR_COUNT],
}

/// Supplies the initial GOOL object for an entity or runtime child.
///
/// The returned object's handle must equal `binding.object.vm()`. Keeping the
/// constructor on this boundary lets a browser asset host page entries before
/// binding, while deterministic tests can provide small authored programs.
pub trait ProgramHost {
    type Error;

    fn bind_program(&mut self, binding: ProgramBinding<'_>) -> Result<VmObject, Self::Error>;

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
    MissingExecutable { executable: u8, eid: Eid },
    Format(FormatError),
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
        }))
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
                .map_err(NsfProgramError::Vm)?,
            );
        }
        Ok(Some(RetailSolidEnvironment::new(
            header.graphics.flags,
            header.graphics.object_colors.words,
            header.graphics.player_colors.words,
            neighbors,
        )))
    }
}

impl NsfProgramHost<'_> {
    fn global_eid(&self, executable: u8) -> Result<Eid, NsfProgramError> {
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

/// Combined trace for the current-zone scan and its first simulation frame.
#[derive(Debug, Eq, PartialEq)]
pub struct SpawnedRuntimeFrame<E> {
    pub spawn_attempts: Vec<RuntimeSpawnAttempt<E>>,
    pub frame: RuntimeFrame<E>,
}

/// Checked failures at the arena/VM/asset boundary.
#[derive(Debug, Eq, PartialEq)]
pub enum RuntimeError<E> {
    Spawn(SpawnError),
    Create(RuntimeCreateError),
    Tree(TreeError),
    Vm(VmError),
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HandleMap {
    vm_by_arena: BTreeMap<ArenaObjectHandle, VmObjectHandle>,
    arena_by_vm: [Option<ArenaObjectHandle>; MAX_OBJECTS],
}

struct FrameWork<E> {
    executions: Vec<RuntimeExecution<E>>,
    spawned_children: Vec<RuntimeObjectHandle>,
}

impl Default for HandleMap {
    fn default() -> Self {
        Self {
            vm_by_arena: BTreeMap::new(),
            arena_by_vm: [None; MAX_OBJECTS],
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

    fn is_live_pair(&self, object: RuntimeObjectHandle) -> bool {
        self.for_arena(object.arena) == Some(object) && self.for_vm(object.vm) == Some(object)
    }
}

/// Pointer-free native coordinator for the first retail runtime slice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetailRuntime {
    arena: ObjectArena,
    machine: Machine,
    handles: HandleMap,
    pending_states: BTreeMap<VmObjectHandle, u16>,
    faulted_objects: BTreeSet<RuntimeObjectHandle>,
    frame_index: u64,
}

impl RetailRuntime {
    #[must_use]
    pub fn new(global_words: usize) -> Self {
        Self {
            arena: ObjectArena::new(),
            machine: Machine::new(global_words),
            handles: HandleMap::default(),
            pending_states: BTreeMap::new(),
            faulted_objects: BTreeSet::new(),
            frame_index: 0,
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

    #[must_use]
    pub const fn frame_index(&self) -> u64 {
        self.frame_index
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

    #[must_use]
    pub fn object_for_arena(&self, arena: ArenaObjectHandle) -> Option<RuntimeObjectHandle> {
        self.handles.for_arena(arena)
    }

    #[must_use]
    pub fn object_for_vm(&self, vm: VmObjectHandle) -> Option<RuntimeObjectHandle> {
        self.handles.for_vm(vm)
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

    /// Applies the exact displayed-neighbor/group-three scan and binds every
    /// successful ZDAT entity. A program failure rolls back that one arena
    /// object so no live tree node exists without executable state.
    pub fn spawn_current_zone_neighbors<H: ProgramHost>(
        &mut self,
        neighbors: &[NeighborZone<'_, ZoneEntity>],
        host: &mut H,
    ) -> Vec<RuntimeSpawnAttempt<H::Error>> {
        let attempts = self.arena.spawn_current_zone_neighbors(neighbors);
        attempts
            .into_iter()
            .map(|attempt| {
                let result = match attempt.result {
                    Err(error) => Err(RuntimeError::Spawn(error)),
                    Ok(arena_handle) => {
                        let entity = neighbors
                            .get(attempt.neighbor_index)
                            .and_then(|neighbor| neighbor.entities.get(attempt.entity_index));
                        if let Some(entity) = entity {
                            self.bind_new_entity(arena_handle, attempt.zone, entity, host)
                        } else {
                            let error = RuntimeError::EntityIndexUnavailable {
                                neighbor_index: attempt.neighbor_index,
                                entity_index: attempt.entity_index,
                            };
                            if let Err(tree_error) = self.arena.despawn_subtree(arena_handle) {
                                Err(RuntimeError::Tree(tree_error))
                            } else {
                                Err(error)
                            }
                        }
                    }
                };
                RuntimeSpawnAttempt {
                    neighbor_index: attempt.neighbor_index,
                    entity_index: attempt.entity_index,
                    zone: attempt.zone,
                    descriptor: attempt.descriptor,
                    result,
                }
            })
            .collect()
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
    pub fn run_frame<H: ProgramHost>(
        &mut self,
        host: &mut H,
        instruction_budget_per_object: usize,
    ) -> Result<RuntimeFrame<H::Error>, RuntimeError<H::Error>> {
        let _discarded_effects = self.machine.take_effects();
        let frame_stamp = wrapping_frame_stamp(self.frame_index);
        self.machine.set_frames_elapsed(frame_stamp);
        self.machine.set_draw_count(frame_stamp);
        let handles = &self.handles;
        self.faulted_objects
            .retain(|object| handles.is_live_pair(*object));
        let mut work = FrameWork {
            executions: Vec::with_capacity(self.handles.vm_by_arena.len()),
            spawned_children: Vec::new(),
        };

        for root_index in 0..ROOT_HANDLE_COUNT {
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
                self.visit_object(arena_handle, host, instruction_budget_per_object, &mut work)?;
                child = sibling;
            }
        }

        let frame_index = self.frame_index;
        self.frame_index = self.frame_index.wrapping_add(1);
        Ok(RuntimeFrame {
            frame_index,
            executions: work.executions,
            spawned_children: work.spawned_children,
            effects: self.machine.take_effects(),
        })
    }

    fn visit_object<H: ProgramHost>(
        &mut self,
        arena_handle: ArenaObjectHandle,
        host: &mut H,
        instruction_budget_per_object: usize,
        work: &mut FrameWork<H::Error>,
    ) -> Result<(), RuntimeError<H::Error>> {
        if let Some(object) = self.handles.for_arena(arena_handle)
            && !self.faulted_objects.contains(&object)
        {
            let result = if self.handles.is_live_pair(object) {
                self.run_object(
                    object,
                    host,
                    instruction_budget_per_object,
                    &mut work.spawned_children,
                )
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
            if let Ok(vm_object) = self.machine.object(object.vm)
                && self.arena.get(arena_handle).is_some()
            {
                self.arena
                    .set_state_flags(arena_handle, vm_object.state_flags())
                    .map_err(RuntimeError::Tree)?;
            }
            work.executions.push(RuntimeExecution { object, result });
        }

        let mut child = self
            .arena
            .get(arena_handle)
            .and_then(SpawnedObject::first_child);
        while let Some(child_handle) = child {
            let Some(spawned) = self.arena.get(child_handle) else {
                break;
            };
            let sibling = spawned.next_sibling();
            self.visit_object(child_handle, host, instruction_budget_per_object, work)?;
            child = sibling;
        }
        Ok(())
    }

    fn bind_new_entity<H: ProgramHost>(
        &mut self,
        arena_handle: ArenaObjectHandle,
        zone: Eid,
        entity: &ZoneEntity,
        host: &mut H,
    ) -> Result<RuntimeObjectHandle, RuntimeError<H::Error>> {
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
        self.rebind_pending_state(object, host, spawned_children)?;
        let mut callback_error = None;
        let execution = {
            let Self {
                arena,
                machine,
                handles,
                ..
            } = self;
            machine.run_with_host_effects(object.vm, budget, |machine, effect| {
                let result = Self::apply_host_effect(
                    arena,
                    handles,
                    machine,
                    host,
                    effect,
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
        if let HaltReason::StateChanged(state) = execution.reason {
            self.pending_states.insert(object.vm, state);
            self.rebind_pending_state(object, host, spawned_children)?;
        }
        Ok(execution)
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
                    ..
                } = self;
                machine.run_pending_once_with_host_effects(object.vm, |machine, effect| {
                    let result = Self::apply_host_effect(
                        arena,
                        handles,
                        machine,
                        host,
                        effect,
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
            once_execution.map_err(RuntimeError::Vm)?;

            let mut callback_error = None;
            let transition_execution = {
                let Self {
                    arena,
                    machine,
                    handles,
                    ..
                } = self;
                machine.run_transition_with_host_effects(object.vm, |machine, effect| {
                    let result = Self::apply_host_effect(
                        arena,
                        handles,
                        machine,
                        host,
                        effect,
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

    fn apply_host_effect<H: ProgramHost>(
        arena: &mut ObjectArena,
        handles: &mut HandleMap,
        machine: &mut Machine,
        host: &mut H,
        effect: &VmEffect,
        spawned_children: &mut Vec<RuntimeObjectHandle>,
    ) -> Result<(), RuntimeError<H::Error>> {
        let VmEffect::SpawnChildren {
            parent,
            executable,
            subtype,
            count,
            allow_reclaim,
            arguments,
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

        for _ in 0..*count {
            let arena_handle = arena
                .create_child(parent.arena, zone, *executable, *subtype, *allow_reclaim)
                .map_err(RuntimeError::Create)?;
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
                zone,
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
            let install_result = (|| {
                let environment = host.zone_environment(zone).map_err(RuntimeError::Program)?;
                let solid_environment = host
                    .solid_environment(zone)
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
                Self::initialize_vm_links(arena, handles, machine, object, &mut vm_object)?;
                Self::install_vm_object(machine, vm_object)?;
                arena
                    .set_state_flags(
                        arena_handle,
                        machine
                            .object(object.vm)
                            .map_err(RuntimeError::Vm)?
                            .state_flags(),
                    )
                    .map_err(RuntimeError::Tree)?;
                machine
                    .object_mut(parent.vm)
                    .map_err(RuntimeError::Vm)?
                    .set_link(3, Some(object.vm))
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
    ) -> Result<RuntimeObjectHandle, RuntimeError<H::Error>> {
        let mut vm_object = host.bind_program(binding).map_err(RuntimeError::Program)?;
        if vm_object.handle() != binding.object.vm {
            return Err(RuntimeError::HostObjectHandleMismatch {
                expected: binding.object.vm,
                actual: vm_object.handle(),
            });
        }
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
        Self::initialize_vm_links(
            &self.arena,
            &self.handles,
            &self.machine,
            binding.object,
            &mut vm_object,
        )?;
        Self::install_vm_object(&mut self.machine, vm_object)?;
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
        Ok(binding.object)
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
        vm_object.set_main_player_identity(binding.object.arena.is_dedicated_main());

        match binding.origin {
            ProgramOrigin::Entity(entity) => vm_object
                .initialize_retail_entity(
                    entity,
                    environment.map_or([0; 3], |environment| environment.origin),
                )
                .map_err(RuntimeError::Vm)?,
            ProgramOrigin::RuntimeChild { .. } => {
                let spawned = arena
                    .get(binding.object.arena)
                    .ok_or(RuntimeError::UnknownArenaObject(binding.object.arena))?;
                if let TreeParent::Object(parent_arena) = spawned.parent() {
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
            }
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
        vm_object
            .set_link(0, Some(object.vm))
            .map_err(RuntimeError::Vm)?;
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
        if let Some(main_arena) = arena.main_object()
            && let Some(main) = handles.for_arena(main_arena)
        {
            vm_object
                .set_link(5, Some(main.vm))
                .map_err(RuntimeError::Vm)?;
        }
        Ok(())
    }

    fn install_vm_object<E>(
        machine: &mut Machine,
        vm_object: VmObject,
    ) -> Result<(), RuntimeError<E>> {
        machine.upsert_object(vm_object).map_err(RuntimeError::Vm)
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
                machine
                    .object_mut(vm)
                    .map_err(RuntimeError::Vm)?
                    .set_link(5, player)
                    .map_err(RuntimeError::Vm)?;
            }
        }
        Ok(())
    }
}

fn wrapping_frame_stamp(frame_index: u64) -> u32 {
    let bytes = frame_index.to_le_bytes();
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}
