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
        GoolAnimationDescriptor, Nsd, Nsf, ObjectVertexKind, ZoneEntity, ZoneHeader, ZoneRect,
        load_gool_program, load_gool_state_program, parse_gool_animation_descriptor,
        parse_object_frame,
    },
};

use crate::{
    gool::{
        AnimationReference, COLOR_COUNT, Execution, GoolProgramIdentity, HaltReason, MAX_OBJECTS,
        Machine, ObjectHandle as VmObjectHandle, RetailPadSnapshot, RetailSolidEnvironment,
        RetailSolidZone, RetailTransform, VmEffect, VmError, VmObject, VmStateProgram,
        process_register,
    },
    math::{Angle12, Angles, Bounds3, Vec3},
    object_arena::{
        EntitySpawnDescriptor, NeighborZone, ObjectArena, ObjectHandle as ArenaObjectHandle,
        ROOT_HANDLE_COUNT, RootHandle, RuntimeCreateError, SpawnError, SpawnedObject, TreeError,
        TreeParent,
    },
    object_bounds::{
        AnimationBoundSource, BoundTransform, calculate_local_bound, calculate_world_bound,
    },
};

/// A malformed transition graph must not monopolize the browser's
/// cooperative frame. Retail follows state links synchronously; this bound
/// preserves that ordering while reporting cycles as a typed VM failure.
const MAX_SYNCHRONOUS_STATE_CHANGES: usize = 64;
const COLLIDABLE_STATUS_B: u32 = 0x10;

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

/// Immutable, pointer-free render state captured from one live arena/VM pair.
///
/// `program` is present for objects materialized from a parsed retail
/// [`crust_formats::stream::GoolProgram`]. Authored objects created directly
/// with [`VmObject::new`] retain `None`; their process state is still exposed
/// for deterministic tests and non-retail hosts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailRenderObject {
    pub object: RuntimeObjectHandle,
    pub zone: Eid,
    pub executable: u8,
    pub subtype: u8,
    pub program: Option<GoolProgramIdentity>,
    pub animation_reference: Option<AnimationReference>,
    pub animation_frame: u32,
    pub transform: RetailTransform,
    pub status_a: u32,
    pub status_b: u32,
    pub status_c: u32,
    pub state_flags: u32,
    pub size: i32,
    pub colors: [u16; COLOR_COUNT],
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
pub struct AnimationBoundBinding {
    pub object: RuntimeObjectHandle,
    pub zone: Eid,
    pub executable: u8,
    pub reference: AnimationReference,
    /// Integer frame selected by the process's 24.8 animation counter.
    pub frame_index: u32,
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

    /// Optionally resolves the current item-five animation and frame to the
    /// fields needed by retail local/world AABB calculation.
    ///
    /// The runtime calls this only for a live object whose `status_b & 0x10`
    /// collidable gate is armed and whose animation reference is non-null.
    /// Authored hosts may omit the callback; no synthetic bound is invented.
    fn animation_bound_source(
        &mut self,
        _binding: AnimationBoundBinding,
    ) -> Result<Option<AnimationBoundSource>, Self::Error> {
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

    fn animation_bound_source(
        &mut self,
        binding: AnimationBoundBinding,
    ) -> Result<Option<AnimationBoundSource>, Self::Error> {
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
            usize::try_from(binding.reference.offset()).map_err(|_| {
                NsfProgramError::Format(FormatError::global(
                    "GOOL animation offset does not fit the host",
                ))
            })?,
        )
        .map_err(NsfProgramError::Format)?;

        let GoolAnimationDescriptor::Vertex(vertex) = descriptor else {
            return Ok(Some(AnimationBoundSource::NonVertex));
        };
        let Ok(frame_index) = u16::try_from(binding.frame_index) else {
            return Ok(None);
        };

        // Retail assets occasionally name a model held by another stream pair.
        // A single-pair host cannot page that dormant reference, so absence
        // from this NSD is controlled `None`; a present but malformed
        // declaration remains a format error.
        if self.metadata.pte(vertex.model_eid).is_none() {
            return Ok(None);
        }
        let vertex_entry = self
            .nsf
            .resolve_entry(self.metadata, vertex.model_eid)
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

    /// Captures every live object in the source runtime's eight-root preorder.
    ///
    /// The returned values own all scalar render state; no arena, VM, entry,
    /// or animation-data references escape this call. Both directions of the
    /// arena/VM handle map and every VM object are validated before collection,
    /// so a stale arena generation cannot silently render a recycled VM slot.
    pub fn render_objects(&self) -> Result<Vec<RetailRenderObject>, RenderObjectsError> {
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
                objects.push(RetailRenderObject {
                    object,
                    zone: spawned.zone(),
                    executable: origin.executable(),
                    subtype: origin.subtype(),
                    program: vm_object.program_identity(),
                    animation_reference: vm_object
                        .animation_reference()
                        .map_err(RenderObjectsError::Vm)?,
                    animation_frame: vm_object.animation_frame(),
                    transform: vm_object
                        .retail_transform()
                        .map_err(RenderObjectsError::Vm)?,
                    status_a: vm_object
                        .register(process_register::STATUS_A)
                        .map_err(RenderObjectsError::Vm)?,
                    status_b: vm_object
                        .register(process_register::STATUS_B)
                        .map_err(RenderObjectsError::Vm)?,
                    status_c: vm_object.status_c(),
                    state_flags: vm_object.state_flags(),
                    size: vm_object
                        .register(process_register::SIZE)
                        .map_err(RenderObjectsError::Vm)? as i32,
                    colors: *vm_object.retail_colors(),
                });
            }
        }

        Ok(objects)
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
    ///
    /// Animation bounds are currently captured immediately before each
    /// collidable object's interpreter invocation. The source can instead
    /// defer a second bound calculation until after physics when its private
    /// animation stamp differs from the render stamp; those display/stamp and
    /// late-physics branches are not yet represented by this host boundary.
    pub fn run_frame<H: ProgramHost>(
        &mut self,
        host: &mut H,
        instruction_budget_per_object: usize,
    ) -> Result<RuntimeFrame<H::Error>, RuntimeError<H::Error>> {
        self.machine.clear_frame_bounds();
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
                self.register_animation_bound(object, host).and_then(|()| {
                    self.run_object(
                        object,
                        host,
                        instruction_budget_per_object,
                        &mut work.spawned_children,
                    )
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

    fn register_animation_bound<H: ProgramHost>(
        &mut self,
        object: RuntimeObjectHandle,
        host: &mut H,
    ) -> Result<(), RuntimeError<H::Error>> {
        let (zone, executable) = {
            let spawned = self
                .arena
                .get(object.arena)
                .ok_or(RuntimeError::UnknownArenaObject(object.arena))?;
            (spawned.zone(), spawned.origin().executable())
        };
        let (reference, frame_index, transform) = {
            let vm_object = self.machine.object(object.vm).map_err(RuntimeError::Vm)?;
            let status_b = vm_object
                .register(process_register::STATUS_B)
                .map_err(RuntimeError::Vm)?;
            if status_b & COLLIDABLE_STATUS_B == 0 {
                return Ok(());
            }
            let Some(reference) = vm_object.animation_reference().map_err(RuntimeError::Vm)? else {
                return Ok(());
            };
            (
                reference,
                vm_object.animation_frame() >> 8,
                vm_object.retail_transform().map_err(RuntimeError::Vm)?,
            )
        };
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
            return Ok(());
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
        let local_bound = calculate_local_bound(source, scale, object.arena.is_dedicated_main());
        let world_bound = calculate_world_bound(local_bound, source, bound_transform);
        self.machine
            .register_frame_bound(object.vm, world_bound)
            .map_err(RuntimeError::Vm)
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

#[cfg(test)]
mod tests {
    use crust_formats::{
        binary::EntryRef,
        stream::{ENTRY_MAGIC, LevelId, NSF_PAGE_SIZE, ZoneEntityPathPoint, parse_nsd, parse_nsf},
    };

    use super::*;
    use crate::object_bounds::MAX_FRAME_BOUNDS;

    const ZONE: Eid = Eid::from_raw(0x1234_5679);
    const RETURN: u32 = 0x8289_4000;
    const MODERN_NSD_HEADER_SIZE: usize = 0x520;

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

    fn configure_render_state(runtime: &mut RetailRuntime, object: RuntimeObjectHandle, seed: u8) {
        let vm_object = runtime.machine.object_mut(object.vm()).unwrap();
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
        vm_object.set_retail_colors([u16::from(seed); COLOR_COUNT]);
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
        vm_object.set_retail_transform(transform).unwrap();
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
                    call.reference.offset(),
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

        assert_eq!(frame.executions.len(), MAX_FRAME_BOUNDS);
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
            reference: AnimationReference::from_word(0xa700_0001).unwrap(),
            frame_index: 0,
        };

        let (nsd_bytes, nsf_bytes) = object_bound_stream_fixture(true, false);
        let metadata = parse_nsd(&nsd_bytes, LevelId::TITLE).unwrap();
        let nsf = parse_nsf(&nsf_bytes, &metadata).unwrap();
        let mut host = NsfProgramHost::new(&metadata, &nsf, &nsf_bytes);
        assert_eq!(
            host.animation_bound_source(binding).unwrap(),
            Some(AnimationBoundSource::Vertex {
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
}
