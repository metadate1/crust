//! Bounded object allocation and retail-compatible spawn-tree ordering.
//!
//! The original runtime represented objects and the eight logical roots with
//! host pointers. This module keeps the same allocation and tree behavior with
//! validated, generational handles. It intentionally owns no GOOL execution
//! state; callers can use [`ObjectHandle`] as the key for that state.

use crust_formats::{binary::Eid, stream::ZoneEntity};

/// Number of ordinary GOOL objects in the retail allocation table.
pub const OBJECT_POOL_CAPACITY: usize = 96;
/// Total live-object capacity: the ordinary pool plus retail's separately
/// allocated player/main object.
pub const OBJECT_ARENA_CAPACITY: usize = OBJECT_POOL_CAPACITY + 1;
/// Number of persistent entity spawn-flag entries.
pub const SPAWN_TABLE_CAPACITY: usize = 304;
/// Number of logical GOOL tree roots.
pub const ROOT_HANDLE_COUNT: usize = 8;

const TOTAL_SLOT_COUNT: usize = OBJECT_ARENA_CAPACITY;
const DEDICATED_MAIN_SLOT: usize = OBJECT_POOL_CAPACITY;
const ACTIVE_ZONE_DISPLAY_BIT: u32 = 1 << 1;
const SPAWNABLE_ENTITY_GROUP: u16 = 3;
const SPAWN_ACTIVE_BIT: u32 = 1;
const SPAWN_BLOCKED_BIT: u32 = 2;
const RECLAIMABLE_STATE_FLAG: u32 = 0x0008_0000;

/// Ordinary zone-spawned objects are inserted under retail root three.
pub const ZONE_OBJECT_ROOT: RootHandle = RootHandle(3);
/// Enemy-category objects can subsequently be reparented under root four.
pub const ENEMY_OBJECT_ROOT: RootHandle = RootHandle(4);
/// The dedicated main object is inserted under retail root six.
pub const MAIN_OBJECT_ROOT: RootHandle = RootHandle(6);

/// A validated logical root replacing one of the eight `gool_handle` pointers.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RootHandle(u8);

impl RootHandle {
    /// Validates a root index in the retail `0..8` range.
    #[must_use]
    pub const fn new(index: u8) -> Option<Self> {
        if index < ROOT_HANDLE_COUNT as u8 {
            Some(Self(index))
        } else {
            None
        }
    }

    /// Returns the root's retail index.
    #[must_use]
    pub const fn index(self) -> u8 {
        self.0
    }
}

/// A stable object identity consisting of an arena slot and generation.
///
/// Moving an object within the tree does not change this handle. Despawning an
/// object increments the slot generation so a stale handle cannot name the
/// next object allocated into the same slot.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ObjectHandle {
    slot: u8,
    generation: u32,
}

impl ObjectHandle {
    /// Returns the stable slot number. Slot 96 is the dedicated main object.
    #[must_use]
    pub const fn slot(self) -> u8 {
        self.slot
    }

    /// Returns the generation used to reject stale handles.
    #[must_use]
    pub const fn generation(self) -> u32 {
        self.generation
    }

    /// Whether this handle names the separately allocated main-object slot.
    #[must_use]
    pub const fn is_dedicated_main(self) -> bool {
        self.slot as usize == DEDICATED_MAIN_SLOT
    }
}

/// Either a logical root or another object can own an object's tree link.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TreeParent {
    Root(RootHandle),
    Object(ObjectHandle),
}

/// The narrow, pointer-free subset of [`ZoneEntity`] needed for spawning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntitySpawnDescriptor {
    pub id: u16,
    pub group: u16,
    pub executable: u8,
    pub subtype: u8,
}

impl EntitySpawnDescriptor {
    /// Exact `GoolObjectSpawn` predicate for selecting the dedicated main slot.
    ///
    /// Group-three executable zero is the usual Crash entity. IDs one through
    /// four and subtype-zero executables `0x2c`/`0x30` are retail special cases.
    #[must_use]
    pub const fn selects_main_object(self) -> bool {
        self.group == SPAWNABLE_ENTITY_GROUP
            && (self.executable == 0
                || (self.id > 0 && self.id < 5)
                || (self.executable == 0x2c && self.subtype == 0)
                || (self.executable == 0x30 && self.subtype == 0))
    }

    /// Exact `GoolObjectCreate` executable/subtype predicate for Crash's slot.
    #[must_use]
    pub const fn is_crash_program(self) -> bool {
        self.executable == 0 && self.subtype == 0
    }
}

impl From<&ZoneEntity> for EntitySpawnDescriptor {
    fn from(entity: &ZoneEntity) -> Self {
        Self {
            id: entity.id,
            group: entity.group,
            executable: entity.executable,
            subtype: entity.subtype,
        }
    }
}

impl From<&Self> for EntitySpawnDescriptor {
    fn from(entity: &Self) -> Self {
        *entity
    }
}

/// One already-resolved neighbor in the current zone header's original order.
///
/// Callers must retain the current zone's neighbor ordering when constructing
/// this slice. The scanner deliberately performs no sorting or deduplication.
#[derive(Clone, Copy, Debug)]
pub struct NeighborZone<'a, E = EntitySpawnDescriptor> {
    pub eid: Eid,
    pub display_flags: u32,
    pub entities: &'a [E],
}

/// One group-three call that the retail neighbor scan makes to object spawn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpawnAttempt {
    pub neighbor_index: usize,
    pub entity_index: usize,
    pub zone: Eid,
    pub descriptor: EntitySpawnDescriptor,
    pub result: Result<ObjectHandle, SpawnError>,
}

/// Expected, non-panicking failures from bounded entity spawning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpawnError {
    InvalidSpawnId(u16),
    SpawnBlocked { id: u16, flags: u32 },
    MainObjectAlreadyActive,
    ObjectPoolFull,
}

/// Expected failures while creating a GOOL runtime child.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeCreateError {
    InvalidParent(ObjectHandle),
    MainObjectUnavailable,
    ObjectPoolFull,
    /// Native `GoolObjectAlloc(1)` selected this root-three preorder object
    /// for synchronous TERM/release before the allocation is retried.
    ///
    /// The arena deliberately does not release it here: the runtime must first
    /// deliver TERM and remove paired VM/audio state.
    ReclaimRequired(ObjectHandle),
    BrokenTree(TreeError),
}

/// Errors from validated tree operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TreeError {
    InvalidObject(ObjectHandle),
    WouldCreateCycle,
    BrokenTreeLink,
}

/// Persistent, exact-width flags for the 304 retail entity IDs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpawnTable {
    flags: [u32; SPAWN_TABLE_CAPACITY],
}

impl Default for SpawnTable {
    fn default() -> Self {
        Self {
            flags: [0; SPAWN_TABLE_CAPACITY],
        }
    }
}

impl SpawnTable {
    /// Returns exact flags, or `None` for an out-of-range entity ID.
    #[must_use]
    pub fn flags(&self, id: u16) -> Option<u32> {
        self.flags.get(usize::from(id)).copied()
    }

    /// Replaces exact flags, including save/checkpoint bits owned elsewhere.
    pub fn set_flags(&mut self, id: u16, flags: u32) -> Result<(), SpawnError> {
        let slot = self
            .flags
            .get_mut(usize::from(id))
            .ok_or(SpawnError::InvalidSpawnId(id))?;
        *slot = flags;
        Ok(())
    }

    /// Copies the exact 304 native spawn words used by `level_state`.
    #[must_use]
    pub const fn snapshot(&self) -> [u32; SPAWN_TABLE_CAPACITY] {
        self.flags
    }

    /// Restores all native spawn words as one validated, fixed-size value.
    pub fn restore(&mut self, flags: [u32; SPAWN_TABLE_CAPACITY]) {
        self.flags = flags;
    }

    fn mark_active(&mut self, id: u16) {
        self.flags[usize::from(id)] |= SPAWN_ACTIVE_BIT;
    }

    fn clear_active(&mut self, id: u16) {
        self.flags[usize::from(id)] &= !SPAWN_ACTIVE_BIT;
    }
}

/// How a live slot was selected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectAllocation {
    Pool,
    DedicatedMain,
}

/// Whether a live object came from persistent zone data or a GOOL opcode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectOrigin {
    Entity(EntitySpawnDescriptor),
    Runtime { executable: u8, subtype: u8 },
}

impl ObjectOrigin {
    #[must_use]
    pub const fn executable(self) -> u8 {
        match self {
            Self::Entity(descriptor) => descriptor.executable,
            Self::Runtime { executable, .. } => executable,
        }
    }

    #[must_use]
    pub const fn subtype(self) -> u8 {
        match self {
            Self::Entity(descriptor) => descriptor.subtype,
            Self::Runtime { subtype, .. } => subtype,
        }
    }
}

/// Spawn metadata attached to one live logical object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpawnedObject {
    zone: Eid,
    origin: ObjectOrigin,
    allocation: ObjectAllocation,
    state_flags: u32,
    parent: TreeParent,
    first_child: Option<ObjectHandle>,
    next_sibling: Option<ObjectHandle>,
}

impl SpawnedObject {
    #[must_use]
    pub const fn zone(&self) -> Eid {
        self.zone
    }

    #[must_use]
    pub const fn origin(&self) -> ObjectOrigin {
        self.origin
    }

    #[must_use]
    pub const fn entity_descriptor(&self) -> Option<EntitySpawnDescriptor> {
        match self.origin {
            ObjectOrigin::Entity(descriptor) => Some(descriptor),
            ObjectOrigin::Runtime { .. } => None,
        }
    }

    #[must_use]
    pub const fn allocation(&self) -> ObjectAllocation {
        self.allocation
    }

    #[must_use]
    pub const fn state_flags(&self) -> u32 {
        self.state_flags
    }

    #[must_use]
    pub const fn parent(&self) -> TreeParent {
        self.parent
    }

    #[must_use]
    pub const fn first_child(&self) -> Option<ObjectHandle> {
        self.first_child
    }

    #[must_use]
    pub const fn next_sibling(&self) -> Option<ObjectHandle> {
        self.next_sibling
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ObjectSlot {
    generation: u32,
    object: Option<SpawnedObject>,
}

/// Fixed-capacity allocation table and eight-root object forest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectArena {
    slots: [ObjectSlot; TOTAL_SLOT_COUNT],
    roots: [Option<ObjectHandle>; ROOT_HANDLE_COUNT],
    // A bounded LIFO model of the C free-object child list. The initial
    // reversed contents make the first allocation use pool slot zero.
    free_slots: [u8; OBJECT_POOL_CAPACITY],
    free_len: usize,
    spawn_table: SpawnTable,
    object_count: usize,
}

impl Default for ObjectArena {
    fn default() -> Self {
        let mut free_slots = [0_u8; OBJECT_POOL_CAPACITY];
        for (index, slot) in free_slots.iter_mut().enumerate() {
            *slot = u8::try_from(OBJECT_POOL_CAPACITY - 1 - index)
                .expect("the retail object pool fits in u8");
        }
        Self {
            slots: std::array::from_fn(|_| ObjectSlot {
                generation: 1,
                object: None,
            }),
            roots: [None; ROOT_HANDLE_COUNT],
            free_slots,
            free_len: OBJECT_POOL_CAPACITY,
            spawn_table: SpawnTable::default(),
            object_count: 0,
        }
    }
}

impl ObjectArena {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.object_count
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.object_count == 0
    }

    #[must_use]
    pub const fn remaining_pool_capacity(&self) -> usize {
        self.free_len
    }

    #[must_use]
    pub const fn spawn_table(&self) -> &SpawnTable {
        &self.spawn_table
    }

    pub const fn spawn_table_mut(&mut self) -> &mut SpawnTable {
        &mut self.spawn_table
    }

    /// Resolves a live handle; stale generations and invalid slots return none.
    #[must_use]
    pub fn get(&self, handle: ObjectHandle) -> Option<&SpawnedObject> {
        let slot = self.slots.get(usize::from(handle.slot))?;
        (slot.generation == handle.generation)
            .then_some(slot.object.as_ref())
            .flatten()
    }

    /// Returns the live dedicated main object, if one has been selected.
    #[must_use]
    pub fn main_object(&self) -> Option<ObjectHandle> {
        self.handle_for_slot(DEDICATED_MAIN_SLOT)
    }

    /// Spawns one descriptor exactly as `GoolObjectSpawn` allocates it.
    pub fn spawn_entity(
        &mut self,
        zone: Eid,
        descriptor: EntitySpawnDescriptor,
    ) -> Result<ObjectHandle, SpawnError> {
        let is_main = descriptor.selects_main_object();
        // Retail checks the existing main pointer before consulting spawn bits.
        if is_main && self.main_object().is_some() {
            return Err(SpawnError::MainObjectAlreadyActive);
        }

        let flags = self
            .spawn_table
            .flags(descriptor.id)
            .ok_or(SpawnError::InvalidSpawnId(descriptor.id))?;
        if flags & (SPAWN_ACTIVE_BIT | SPAWN_BLOCKED_BIT) != 0 {
            return Err(SpawnError::SpawnBlocked {
                id: descriptor.id,
                flags,
            });
        }

        let allocation = if is_main {
            ObjectAllocation::DedicatedMain
        } else {
            ObjectAllocation::Pool
        };
        let parent = TreeParent::Root(if is_main {
            MAIN_OBJECT_ROOT
        } else {
            ZONE_OBJECT_ROOT
        });
        let handle = self.allocate(zone, ObjectOrigin::Entity(descriptor), allocation, parent)?;
        self.insert_at_head(parent, handle);
        self.spawn_table.mark_active(descriptor.id);
        Ok(handle)
    }

    /// Creates a child requested by GOOL opcode `0x8a` or `0x91`.
    ///
    /// Runtime children do not mark an entity spawn-table ID when created.
    /// Their retail PID word is zero, however, so teardown clears the active
    /// bit of spawn slot zero exactly like `GoolObjectKill`. If the ordinary
    /// pool is full, `allow_reclaim` mirrors opcode `0x91` by reporting the
    /// first root-three preorder object whose state has retail flag `0x80000`.
    /// The runtime owns TERM delivery and paired VM/audio cleanup, so no arena
    /// slot is released until that synchronous lifecycle has completed.
    pub fn create_child(
        &mut self,
        parent: ObjectHandle,
        zone: Eid,
        executable: u8,
        subtype: u8,
        allow_reclaim: bool,
    ) -> Result<ObjectHandle, RuntimeCreateError> {
        self.object(parent)
            .map_err(|_| RuntimeCreateError::InvalidParent(parent))?;

        // Retail reuses the separately allocated player/Crash object for
        // executable/subtype zero rather than consuming the ordinary pool.
        if executable == 0 && subtype == 0 {
            let tree_parent = TreeParent::Object(parent);
            let main = if let Some(main) = self.main_object() {
                self.add_child(tree_parent, main)
                    .map_err(RuntimeCreateError::BrokenTree)?;
                let object = self
                    .object_mut(main)
                    .map_err(RuntimeCreateError::BrokenTree)?;
                object.zone = zone;
                object.origin = ObjectOrigin::Runtime {
                    executable,
                    subtype,
                };
                object.state_flags = 0;
                main
            } else {
                let main = self
                    .allocate(
                        zone,
                        ObjectOrigin::Runtime {
                            executable,
                            subtype,
                        },
                        ObjectAllocation::DedicatedMain,
                        tree_parent,
                    )
                    .map_err(|_| RuntimeCreateError::MainObjectUnavailable)?;
                self.insert_at_head(tree_parent, main);
                main
            };
            return Ok(main);
        }

        if self.free_len == 0 {
            if !allow_reclaim {
                return Err(RuntimeCreateError::ObjectPoolFull);
            }
            let candidate = self
                .first_reclaimable()
                .map_err(RuntimeCreateError::BrokenTree)?
                .ok_or(RuntimeCreateError::ObjectPoolFull)?;
            return Err(RuntimeCreateError::ReclaimRequired(candidate));
        }

        let tree_parent = TreeParent::Object(parent);
        let handle = self
            .allocate(
                zone,
                ObjectOrigin::Runtime {
                    executable,
                    subtype,
                },
                ObjectAllocation::Pool,
                tree_parent,
            )
            .map_err(|error| match error {
                SpawnError::ObjectPoolFull => RuntimeCreateError::ObjectPoolFull,
                _ => unreachable!("pool allocation has no entity validation"),
            })?;
        self.insert_at_head(tree_parent, handle);
        Ok(handle)
    }

    /// Creates an object directly beneath one of retail's eight logical roots.
    ///
    /// Native host code uses `GoolObjectCreate(&handles[n], ...)` for HUD,
    /// pause, and PBAK caption objects, while opcodes `0x8a`/`0x91` use an
    /// ordinary object parent. Allocation and optional root-three reclamation
    /// are otherwise identical to [`Self::create_child`].
    pub fn create_root_object(
        &mut self,
        root: RootHandle,
        zone: Eid,
        executable: u8,
        subtype: u8,
        allow_reclaim: bool,
    ) -> Result<ObjectHandle, RuntimeCreateError> {
        let tree_parent = TreeParent::Root(root);

        // `GoolObjectCreate` reuses the separately allocated player/Crash
        // object for executable/subtype zero regardless of whether its parent
        // is an object or a logical handle.
        if executable == 0 && subtype == 0 {
            let main = if let Some(main) = self.main_object() {
                self.add_child(tree_parent, main)
                    .map_err(RuntimeCreateError::BrokenTree)?;
                let object = self
                    .object_mut(main)
                    .map_err(RuntimeCreateError::BrokenTree)?;
                object.zone = zone;
                object.origin = ObjectOrigin::Runtime {
                    executable,
                    subtype,
                };
                object.state_flags = 0;
                main
            } else {
                let main = self
                    .allocate(
                        zone,
                        ObjectOrigin::Runtime {
                            executable,
                            subtype,
                        },
                        ObjectAllocation::DedicatedMain,
                        tree_parent,
                    )
                    .map_err(|_| RuntimeCreateError::MainObjectUnavailable)?;
                self.insert_at_head(tree_parent, main);
                main
            };
            return Ok(main);
        }

        if self.free_len == 0 {
            if !allow_reclaim {
                return Err(RuntimeCreateError::ObjectPoolFull);
            }
            let candidate = self
                .first_reclaimable()
                .map_err(RuntimeCreateError::BrokenTree)?
                .ok_or(RuntimeCreateError::ObjectPoolFull)?;
            return Err(RuntimeCreateError::ReclaimRequired(candidate));
        }

        let handle = self
            .allocate(
                zone,
                ObjectOrigin::Runtime {
                    executable,
                    subtype,
                },
                ObjectAllocation::Pool,
                tree_parent,
            )
            .map_err(|error| match error {
                SpawnError::ObjectPoolFull => RuntimeCreateError::ObjectPoolFull,
                _ => unreachable!("pool allocation has no entity validation"),
            })?;
        self.insert_at_head(tree_parent, handle);
        Ok(handle)
    }

    /// Synchronizes the VM state flags used by opcode `0x91` reclamation.
    pub fn set_state_flags(
        &mut self,
        handle: ObjectHandle,
        state_flags: u32,
    ) -> Result<(), TreeError> {
        self.object_mut(handle)?.state_flags = state_flags;
        Ok(())
    }

    /// Changes the current zone attached to one live object.
    ///
    /// The object's allocation, tree position, and persistent entity spawn
    /// flags are not changed. A stale generation is rejected before mutation.
    pub fn set_zone(&mut self, handle: ObjectHandle, zone: Eid) -> Result<(), TreeError> {
        self.object_mut(handle)?.zone = zone;
        Ok(())
    }

    /// Runs the current-zone neighbor scan without reordering any input.
    ///
    /// Only displayed neighbors (`display_flags & 2`) and group-three entities
    /// generate attempts. Failures are retained and scanning continues, as in
    /// `LevelSpawnObjects`, so the report itself is a characterization trace.
    pub fn spawn_current_zone_neighbors<E>(
        &mut self,
        neighbors: &[NeighborZone<'_, E>],
    ) -> Vec<SpawnAttempt>
    where
        for<'a> EntitySpawnDescriptor: From<&'a E>,
    {
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
                attempts.push(SpawnAttempt {
                    neighbor_index,
                    entity_index,
                    zone: neighbor.eid,
                    descriptor,
                    result: self.spawn_entity(neighbor.eid, descriptor),
                });
            }
        }
        attempts
    }

    /// Reparents an existing object, inserting it at the new parent's head.
    pub fn add_child(&mut self, parent: TreeParent, child: ObjectHandle) -> Result<(), TreeError> {
        self.object(child)?;
        self.first_child_of(parent)?;
        if let TreeParent::Object(mut ancestor) = parent {
            loop {
                if ancestor == child {
                    return Err(TreeError::WouldCreateCycle);
                }
                let ancestor_parent = self.object(ancestor)?.parent;
                let TreeParent::Object(next) = ancestor_parent else {
                    break;
                };
                ancestor = next;
            }
        }

        self.detach(child)?;
        self.insert_at_head(parent, child);
        Ok(())
    }

    /// Moves a live object to the head of one of the eight logical roots.
    ///
    /// Descendants remain attached to the object. The operation first
    /// validates the live generation and then uses the same checked detach and
    /// head insertion as object-to-object reparenting.
    pub fn reparent_to_root(
        &mut self,
        child: ObjectHandle,
        root: RootHandle,
    ) -> Result<(), TreeError> {
        self.add_child(TreeParent::Root(root), child)
    }

    /// Iterates all descendants of a root or object in retail preorder.
    pub fn preorder(&self, parent: TreeParent) -> Result<Preorder<'_>, TreeError> {
        let next = self.first_child_of(parent)?;
        Ok(Preorder {
            arena: self,
            next,
            deferred_siblings: [None; TOTAL_SLOT_COUNT],
            deferred_len: 0,
        })
    }

    /// Returns the current head child of one logical root.
    ///
    /// Mutation-aware host traversals use this narrow accessor to begin each
    /// native root walk live, then retain checked sibling handles across
    /// synchronous callbacks without exposing the arena's root table.
    #[must_use]
    pub fn root_first_child(&self, root: RootHandle) -> Option<ObjectHandle> {
        self.roots[usize::from(root.index())]
    }

    /// Takes a deterministic, checked postorder snapshot of the whole forest.
    ///
    /// Roots are visited in retail index order `0..8`; siblings retain their
    /// current head-to-tail order. Every descendant therefore appears before
    /// its parent, which lets callers terminate selected zone objects without
    /// borrowing the arena during traversal. Invalid, cyclic, duplicated, or
    /// unreachable tree links are reported instead of producing a partial
    /// snapshot.
    pub fn postorder_snapshot(&self) -> Result<Vec<ObjectHandle>, TreeError> {
        let mut output = Vec::with_capacity(self.object_count);
        let mut visited = [false; TOTAL_SLOT_COUNT];
        for root_index in 0..ROOT_HANDLE_COUNT as u8 {
            let root = RootHandle(root_index);
            self.collect_children_postorder(TreeParent::Root(root), &mut visited, &mut output)?;
        }
        if output.len() != self.object_count {
            return Err(TreeError::BrokenTreeLink);
        }
        Ok(output)
    }

    /// Takes a deterministic, checked postorder snapshot of one subtree.
    ///
    /// Unlike [`Self::despawn_subtree`], this does not mutate allocation or
    /// spawn state. Paired runtimes use the snapshot to validate live process
    /// PIDs before releasing children and then their parent.
    pub fn subtree_postorder_snapshot(
        &self,
        root: ObjectHandle,
    ) -> Result<Vec<ObjectHandle>, TreeError> {
        let parent = self.object(root)?.parent;
        let mut output = Vec::new();
        let mut visited = [false; TOTAL_SLOT_COUNT];
        self.collect_object_postorder(root, parent, &mut visited, &mut output)?;
        Ok(output)
    }

    /// Despawns an object and all descendants in retail release order.
    ///
    /// The returned stale handles are ordered children-before-parent, with
    /// every sibling list visited from head to tail. This matches the release
    /// order of recursive `GoolObjectKill` and lets the runtime remove paired
    /// VM/audio state deterministically after the arena generations advance.
    /// Entity objects clear their own active spawn bit; runtime children clear
    /// slot zero, matching their zero-initialized retail PID word.
    pub fn despawn_subtree(&mut self, root: ObjectHandle) -> Result<Vec<ObjectHandle>, TreeError> {
        let postorder = self.subtree_postorder_snapshot(root)?;
        self.detach(root)?;

        for handle in postorder.iter().copied() {
            self.release(handle)?;
        }
        Ok(postorder)
    }

    /// Releases one already-childless object.
    ///
    /// This is the checked final step of native recursive `GoolObjectKill`:
    /// callers deliver TERM and release every child before invoking it. The
    /// ordinary subtree API remains preferable for non-signalling teardown.
    pub fn despawn_leaf(&mut self, object: ObjectHandle) -> Result<ObjectHandle, TreeError> {
        if self.object(object)?.first_child.is_some() {
            return Err(TreeError::BrokenTreeLink);
        }
        self.detach(object)?;
        self.release(object)?;
        Ok(object)
    }

    /// Releases one already-childless object using its live GOOL PID.
    ///
    /// Entity initialization seeds `pid_flags` from the descriptor, while a
    /// runtime program may later replace that word. Native teardown reads the
    /// live process word, so the paired runtime supplies its validated ID here
    /// instead of relying on immutable arena provenance.
    pub fn despawn_leaf_with_spawn_id(
        &mut self,
        object: ObjectHandle,
        spawn_id: u16,
    ) -> Result<ObjectHandle, TreeError> {
        if self.object(object)?.first_child.is_some() {
            return Err(TreeError::BrokenTreeLink);
        }
        self.detach(object)?;
        self.release_with_spawn_id(object, spawn_id)?;
        Ok(object)
    }

    /// Returns native `GoolObjectAlloc(1)`'s expendable-object choice.
    ///
    /// The source searches only handle/root three, visiting its head-to-tail
    /// children and every descendant in preorder. It does not exclude the
    /// current creator or any ancestor; legal programs are responsible for
    /// not marking an active creator expendable.
    pub fn first_reclaimable(&self) -> Result<Option<ObjectHandle>, TreeError> {
        Ok(self
            .preorder(TreeParent::Root(ZONE_OBJECT_ROOT))?
            .find(|candidate| {
                self.get(*candidate)
                    .is_some_and(|object| object.state_flags & RECLAIMABLE_STATE_FLAG != 0)
            }))
    }

    fn allocate(
        &mut self,
        zone: Eid,
        origin: ObjectOrigin,
        allocation: ObjectAllocation,
        parent: TreeParent,
    ) -> Result<ObjectHandle, SpawnError> {
        let slot_index = match allocation {
            ObjectAllocation::Pool => {
                let next_len = self
                    .free_len
                    .checked_sub(1)
                    .ok_or(SpawnError::ObjectPoolFull)?;
                self.free_len = next_len;
                usize::from(self.free_slots[next_len])
            }
            ObjectAllocation::DedicatedMain => DEDICATED_MAIN_SLOT,
        };
        let slot = &mut self.slots[slot_index];
        debug_assert!(slot.object.is_none());
        let handle = ObjectHandle {
            slot: u8::try_from(slot_index).expect("all object slots fit in u8"),
            generation: slot.generation,
        };
        slot.object = Some(SpawnedObject {
            zone,
            origin,
            allocation,
            state_flags: 0,
            parent,
            first_child: None,
            next_sibling: None,
        });
        self.object_count += 1;
        Ok(handle)
    }

    fn object(&self, handle: ObjectHandle) -> Result<&SpawnedObject, TreeError> {
        self.get(handle).ok_or(TreeError::InvalidObject(handle))
    }

    fn object_mut(&mut self, handle: ObjectHandle) -> Result<&mut SpawnedObject, TreeError> {
        let slot = self
            .slots
            .get_mut(usize::from(handle.slot))
            .ok_or(TreeError::InvalidObject(handle))?;
        if slot.generation != handle.generation {
            return Err(TreeError::InvalidObject(handle));
        }
        slot.object.as_mut().ok_or(TreeError::InvalidObject(handle))
    }

    fn handle_for_slot(&self, slot_index: usize) -> Option<ObjectHandle> {
        let slot = self.slots.get(slot_index)?;
        slot.object.as_ref()?;
        Some(ObjectHandle {
            slot: u8::try_from(slot_index).ok()?,
            generation: slot.generation,
        })
    }

    fn first_child_of(&self, parent: TreeParent) -> Result<Option<ObjectHandle>, TreeError> {
        match parent {
            TreeParent::Root(root) => Ok(self.roots[usize::from(root.index())]),
            TreeParent::Object(object) => Ok(self.object(object)?.first_child),
        }
    }

    fn set_first_child(
        &mut self,
        parent: TreeParent,
        child: Option<ObjectHandle>,
    ) -> Result<(), TreeError> {
        match parent {
            TreeParent::Root(root) => self.roots[usize::from(root.index())] = child,
            TreeParent::Object(object) => self.object_mut(object)?.first_child = child,
        }
        Ok(())
    }

    fn insert_at_head(&mut self, parent: TreeParent, child: ObjectHandle) {
        let old_head = self
            .first_child_of(parent)
            .expect("allocation and reparenting validate parents");
        let object = self
            .object_mut(child)
            .expect("allocation and reparenting validate children");
        object.parent = parent;
        object.next_sibling = old_head;
        self.set_first_child(parent, Some(child))
            .expect("allocation and reparenting validate parents");
    }

    fn detach(&mut self, child: ObjectHandle) -> Result<(), TreeError> {
        let child_object = self.object(child)?;
        let parent = child_object.parent;
        let next = child_object.next_sibling;
        let first = self.first_child_of(parent)?;
        if first == Some(child) {
            self.set_first_child(parent, next)?;
        } else {
            let mut cursor = first;
            loop {
                let current = cursor.ok_or(TreeError::BrokenTreeLink)?;
                let current_next = self.object(current)?.next_sibling;
                if current_next == Some(child) {
                    self.object_mut(current)?.next_sibling = next;
                    break;
                }
                cursor = current_next;
            }
        }
        self.object_mut(child)?.next_sibling = None;
        Ok(())
    }

    fn collect_children_postorder(
        &self,
        parent: TreeParent,
        visited: &mut [bool; TOTAL_SLOT_COUNT],
        output: &mut Vec<ObjectHandle>,
    ) -> Result<(), TreeError> {
        let mut child = self.first_child_of(parent)?;
        while let Some(current) = child {
            let sibling = self.object(current)?.next_sibling;
            self.collect_object_postorder(current, parent, visited, output)?;
            child = sibling;
        }
        Ok(())
    }

    fn collect_object_postorder(
        &self,
        current: ObjectHandle,
        parent: TreeParent,
        visited: &mut [bool; TOTAL_SLOT_COUNT],
        output: &mut Vec<ObjectHandle>,
    ) -> Result<(), TreeError> {
        let was_visited = visited
            .get_mut(usize::from(current.slot))
            .ok_or(TreeError::InvalidObject(current))?;
        if *was_visited {
            return Err(TreeError::WouldCreateCycle);
        }
        let object = self.object(current)?;
        if object.parent != parent {
            return Err(TreeError::BrokenTreeLink);
        }
        *was_visited = true;
        self.collect_children_postorder(TreeParent::Object(current), visited, output)?;
        output.push(current);
        Ok(())
    }

    fn release(&mut self, handle: ObjectHandle) -> Result<(), TreeError> {
        let object = self.object(handle)?;
        let spawn_id = match object.origin {
            ObjectOrigin::Entity(descriptor) => descriptor.id,
            ObjectOrigin::Runtime { .. } => 0,
        };
        self.release_with_spawn_id(handle, spawn_id)
    }

    fn release_with_spawn_id(
        &mut self,
        handle: ObjectHandle,
        spawn_id: u16,
    ) -> Result<(), TreeError> {
        let slot_index = usize::from(handle.slot);
        let object = self.object(handle)?.clone();
        self.spawn_table.clear_active(spawn_id);
        let slot = &mut self.slots[slot_index];
        slot.object = None;
        slot.generation = slot.generation.wrapping_add(1).max(1);
        if object.allocation == ObjectAllocation::Pool {
            debug_assert!(self.free_len < OBJECT_POOL_CAPACITY);
            self.free_slots[self.free_len] = handle.slot;
            self.free_len += 1;
        }
        self.object_count -= 1;
        Ok(())
    }
}

/// Borrowing preorder iterator over a validated, immutable object forest.
#[derive(Debug)]
pub struct Preorder<'a> {
    arena: &'a ObjectArena,
    next: Option<ObjectHandle>,
    deferred_siblings: [Option<ObjectHandle>; TOTAL_SLOT_COUNT],
    deferred_len: usize,
}

impl Iterator for Preorder<'_> {
    type Item = ObjectHandle;

    fn next(&mut self) -> Option<Self::Item> {
        let current = self.next?;
        let object = self.arena.get(current)?;
        if let Some(child) = object.first_child {
            if let Some(sibling) = object.next_sibling {
                debug_assert!(self.deferred_len < self.deferred_siblings.len());
                self.deferred_siblings[self.deferred_len] = Some(sibling);
                self.deferred_len += 1;
            }
            self.next = Some(child);
        } else if object.next_sibling.is_some() {
            self.next = object.next_sibling;
        } else if self.deferred_len == 0 {
            self.next = None;
        } else {
            self.deferred_len -= 1;
            self.next = self.deferred_siblings[self.deferred_len].take();
        }
        Some(current)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(TOTAL_SLOT_COUNT))
    }
}

impl std::iter::FusedIterator for Preorder<'_> {}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const ZONE_A: Eid = Eid::from_raw(0x1111_1111);
    const ZONE_B: Eid = Eid::from_raw(0x2222_2223);

    const fn entity(id: u16, group: u16, executable: u8, subtype: u8) -> EntitySpawnDescriptor {
        EntitySpawnDescriptor {
            id,
            group,
            executable,
            subtype,
        }
    }

    fn object_ids(
        arena: &ObjectArena,
        handles: impl IntoIterator<Item = ObjectHandle>,
    ) -> Vec<u16> {
        handles
            .into_iter()
            .map(|handle| arena.get(handle).unwrap().entity_descriptor().unwrap().id)
            .collect()
    }

    #[test]
    fn main_and_crash_selection_match_distinct_retail_predicates() {
        assert!(entity(9, 3, 0, 7).selects_main_object());
        assert!(entity(1, 3, 19, 8).selects_main_object());
        assert!(entity(4, 3, 19, 8).selects_main_object());
        assert!(!entity(5, 3, 19, 8).selects_main_object());
        assert!(entity(9, 3, 0x2c, 0).selects_main_object());
        assert!(!entity(9, 3, 0x2c, 1).selects_main_object());
        assert!(entity(9, 3, 0x30, 0).selects_main_object());
        assert!(!entity(9, 3, 0x30, 1).selects_main_object());
        assert!(!entity(1, 2, 0, 0).selects_main_object());

        assert!(entity(9, 2, 0, 0).is_crash_program());
        assert!(!entity(9, 3, 0, 1).is_crash_program());
    }

    #[test]
    fn scan_keeps_neighbor_and_entity_order_before_head_insertion() {
        let hidden = [entity(10, 3, 1, 0)];
        let first = [
            entity(11, 3, 1, 0),
            entity(12, 2, 1, 0),
            entity(13, 3, 1, 0),
        ];
        let second = [entity(14, 3, 1, 0)];
        let neighbors = [
            NeighborZone {
                eid: ZONE_A,
                display_flags: 0,
                entities: &hidden,
            },
            NeighborZone {
                eid: ZONE_A,
                display_flags: 2,
                entities: &first,
            },
            NeighborZone {
                eid: ZONE_B,
                display_flags: 6,
                entities: &second,
            },
        ];

        let mut arena = ObjectArena::new();
        let attempts = arena.spawn_current_zone_neighbors(&neighbors);
        assert_eq!(
            attempts
                .iter()
                .map(|attempt| (
                    attempt.neighbor_index,
                    attempt.entity_index,
                    attempt.zone,
                    attempt.descriptor.id
                ))
                .collect::<Vec<_>>(),
            [(1, 0, ZONE_A, 11), (1, 2, ZONE_A, 13), (2, 0, ZONE_B, 14)]
        );
        assert!(attempts.iter().all(|attempt| attempt.result.is_ok()));

        let preorder = arena.preorder(TreeParent::Root(ZONE_OBJECT_ROOT)).unwrap();
        assert_eq!(object_ids(&arena, preorder), [14, 13, 11]);
    }

    #[test]
    fn scan_records_failures_and_continues() {
        let descriptors = [
            entity(20, 3, 1, 0),
            entity(20, 3, 1, 0),
            entity(21, 3, 0, 0),
            entity(22, 3, 0x2c, 0),
            entity(23, 3, 1, 0),
        ];
        let neighbors = [NeighborZone {
            eid: ZONE_A,
            display_flags: 2,
            entities: &descriptors,
        }];
        let mut arena = ObjectArena::new();
        let attempts = arena.spawn_current_zone_neighbors(&neighbors);

        assert!(attempts[0].result.is_ok());
        assert_eq!(
            attempts[1].result,
            Err(SpawnError::SpawnBlocked { id: 20, flags: 1 })
        );
        assert!(
            attempts[2]
                .result
                .is_ok_and(ObjectHandle::is_dedicated_main)
        );
        assert_eq!(attempts[3].result, Err(SpawnError::MainObjectAlreadyActive));
        assert!(attempts[4].result.is_ok());
    }

    #[test]
    fn spawn_table_is_exactly_bounded_and_preserves_other_bits() {
        let mut table = SpawnTable::default();
        assert_eq!(table.flags(303), Some(0));
        assert_eq!(table.flags(304), None);
        table.set_flags(303, 0x8000_0002).unwrap();
        assert_eq!(table.flags(303), Some(0x8000_0002));
        assert_eq!(
            table.set_flags(304, 0),
            Err(SpawnError::InvalidSpawnId(304))
        );

        let mut arena = ObjectArena::new();
        arena.spawn_table_mut().set_flags(33, 0x102).unwrap();
        assert_eq!(
            arena.spawn_entity(ZONE_A, entity(33, 3, 1, 0)),
            Err(SpawnError::SpawnBlocked {
                id: 33,
                flags: 0x102
            })
        );
        assert_eq!(
            arena.spawn_entity(ZONE_A, entity(304, 3, 1, 0)),
            Err(SpawnError::InvalidSpawnId(304))
        );
    }

    #[test]
    fn child_insertion_is_at_head_and_preorder_is_depth_first() {
        let mut arena = ObjectArena::new();
        let parent = arena.spawn_entity(ZONE_A, entity(30, 3, 1, 0)).unwrap();
        let sibling = arena.spawn_entity(ZONE_A, entity(31, 3, 1, 0)).unwrap();
        let first_child = arena.spawn_entity(ZONE_A, entity(32, 3, 1, 0)).unwrap();
        let second_child = arena.spawn_entity(ZONE_A, entity(33, 3, 1, 0)).unwrap();
        let grandchild = arena.spawn_entity(ZONE_A, entity(34, 3, 1, 0)).unwrap();

        arena
            .add_child(TreeParent::Object(parent), first_child)
            .unwrap();
        arena
            .add_child(TreeParent::Object(parent), second_child)
            .unwrap();
        arena
            .add_child(TreeParent::Object(first_child), grandchild)
            .unwrap();
        assert_eq!(arena.get(parent).unwrap().first_child(), Some(second_child));

        let preorder = arena.preorder(TreeParent::Root(ZONE_OBJECT_ROOT)).unwrap();
        assert_eq!(
            object_ids(&arena, preorder),
            [31, 30, 33, 32, 34],
            "root and child lists both retain head-insertion order"
        );
        assert_eq!(
            arena.get(sibling).unwrap().parent(),
            TreeParent::Root(ZONE_OBJECT_ROOT)
        );
    }

    #[test]
    fn cycle_rejection_leaves_the_tree_unchanged() {
        let mut arena = ObjectArena::new();
        let parent = arena.spawn_entity(ZONE_A, entity(40, 3, 1, 0)).unwrap();
        let child = arena.spawn_entity(ZONE_A, entity(41, 3, 1, 0)).unwrap();
        arena.add_child(TreeParent::Object(parent), child).unwrap();
        let before = arena.clone();
        assert_eq!(
            arena.add_child(TreeParent::Object(child), parent),
            Err(TreeError::WouldCreateCycle)
        );
        assert_eq!(arena, before);
    }

    #[test]
    fn moving_a_live_object_to_another_zone_is_checked_and_preserves_spawn_state() {
        let mut arena = ObjectArena::new();
        let object = arena.spawn_entity(ZONE_A, entity(42, 3, 1, 0)).unwrap();

        arena.set_zone(object, ZONE_B).unwrap();
        assert_eq!(arena.get(object).unwrap().zone(), ZONE_B);
        assert_eq!(arena.spawn_table().flags(42), Some(SPAWN_ACTIVE_BIT));

        arena.despawn_subtree(object).unwrap();
        assert_eq!(arena.spawn_table().flags(42), Some(0));
        assert_eq!(
            arena.set_zone(object, ZONE_A),
            Err(TreeError::InvalidObject(object))
        );
    }

    #[test]
    fn category_reparenting_moves_root_three_objects_to_root_four_at_head() {
        let mut arena = ObjectArena::new();
        let first = arena.spawn_entity(ZONE_A, entity(43, 3, 1, 0)).unwrap();
        let second = arena.spawn_entity(ZONE_A, entity(44, 3, 1, 0)).unwrap();
        let third = arena.spawn_entity(ZONE_A, entity(45, 3, 1, 0)).unwrap();

        arena.reparent_to_root(second, ENEMY_OBJECT_ROOT).unwrap();
        arena.reparent_to_root(first, ENEMY_OBJECT_ROOT).unwrap();

        assert_eq!(
            object_ids(
                &arena,
                arena.preorder(TreeParent::Root(ZONE_OBJECT_ROOT)).unwrap()
            ),
            [45]
        );
        assert_eq!(
            object_ids(
                &arena,
                arena.preorder(TreeParent::Root(ENEMY_OBJECT_ROOT)).unwrap()
            ),
            [43, 44],
            "each category move inserts at the destination root's head"
        );
        assert_eq!(
            arena.get(first).unwrap().parent(),
            TreeParent::Root(ENEMY_OBJECT_ROOT)
        );
        assert_eq!(
            arena.get(second).unwrap().parent(),
            TreeParent::Root(ENEMY_OBJECT_ROOT)
        );
        assert_eq!(arena.get(third).unwrap().next_sibling(), None);
    }

    #[test]
    fn whole_forest_postorder_is_children_first_and_root_ordered() {
        let mut arena = ObjectArena::new();
        let root_zero_parent = arena.spawn_entity(ZONE_A, entity(46, 3, 1, 0)).unwrap();
        let root_zero_child = arena.spawn_entity(ZONE_A, entity(47, 3, 1, 0)).unwrap();
        let root_zero_grandchild = arena.spawn_entity(ZONE_A, entity(48, 3, 1, 0)).unwrap();
        let root_zero_head = arena.spawn_entity(ZONE_A, entity(49, 3, 1, 0)).unwrap();
        let root_two = arena.spawn_entity(ZONE_A, entity(50, 3, 1, 0)).unwrap();
        let root_seven_parent = arena.spawn_entity(ZONE_A, entity(51, 3, 1, 0)).unwrap();
        let root_seven_child = arena.spawn_entity(ZONE_A, entity(52, 3, 1, 0)).unwrap();

        arena
            .add_child(TreeParent::Object(root_zero_parent), root_zero_child)
            .unwrap();
        arena
            .add_child(TreeParent::Object(root_zero_child), root_zero_grandchild)
            .unwrap();
        arena
            .reparent_to_root(root_zero_parent, RootHandle::new(0).unwrap())
            .unwrap();
        arena
            .reparent_to_root(root_zero_head, RootHandle::new(0).unwrap())
            .unwrap();
        arena
            .reparent_to_root(root_two, RootHandle::new(2).unwrap())
            .unwrap();
        arena
            .add_child(TreeParent::Object(root_seven_parent), root_seven_child)
            .unwrap();
        arena
            .reparent_to_root(root_seven_parent, RootHandle::new(7).unwrap())
            .unwrap();

        assert_eq!(
            arena.postorder_snapshot().unwrap(),
            [
                root_zero_head,
                root_zero_grandchild,
                root_zero_child,
                root_zero_parent,
                root_two,
                root_seven_child,
                root_seven_parent,
            ]
        );
    }

    #[test]
    fn stale_lifecycle_handles_and_cycle_requests_do_not_mutate_live_objects() {
        let mut arena = ObjectArena::new();
        let parent = arena.spawn_entity(ZONE_A, entity(53, 3, 1, 0)).unwrap();
        let child = arena.spawn_entity(ZONE_A, entity(54, 3, 1, 0)).unwrap();
        arena.add_child(TreeParent::Object(parent), child).unwrap();

        let before_cycle = arena.clone();
        assert_eq!(
            arena.add_child(TreeParent::Object(child), parent),
            Err(TreeError::WouldCreateCycle)
        );
        assert_eq!(arena, before_cycle);

        let snapshot = arena.postorder_snapshot().unwrap();
        assert_eq!(snapshot, [child, parent]);
        arena.despawn_subtree(parent).unwrap();
        let replacement = arena.spawn_entity(ZONE_B, entity(55, 3, 1, 0)).unwrap();
        assert_eq!(replacement.slot(), parent.slot());
        assert_ne!(replacement.generation(), parent.generation());

        assert_eq!(
            arena.reparent_to_root(parent, ENEMY_OBJECT_ROOT),
            Err(TreeError::InvalidObject(parent))
        );
        assert_eq!(
            arena.despawn_subtree(snapshot[0]),
            Err(TreeError::InvalidObject(child))
        );
        assert_eq!(arena.get(replacement).unwrap().zone(), ZONE_B);
        assert_eq!(arena.len(), 1);
    }

    #[test]
    fn subtree_despawn_returns_and_reuses_exact_head_to_tail_postorder() {
        let mut arena = ObjectArena::new();
        let parent = arena.spawn_entity(ZONE_A, entity(56, 3, 1, 0)).unwrap();
        let first_child = arena.spawn_entity(ZONE_A, entity(57, 3, 1, 0)).unwrap();
        let second_child = arena.spawn_entity(ZONE_A, entity(58, 3, 1, 0)).unwrap();
        let grandchild = arena.spawn_entity(ZONE_A, entity(59, 3, 1, 0)).unwrap();
        arena
            .add_child(TreeParent::Object(parent), second_child)
            .unwrap();
        arena
            .add_child(TreeParent::Object(parent), first_child)
            .unwrap();
        arena
            .add_child(TreeParent::Object(first_child), grandchild)
            .unwrap();

        let removed = arena.despawn_subtree(parent).unwrap();
        assert_eq!(removed, [grandchild, first_child, second_child, parent]);
        assert!(removed.iter().all(|handle| arena.get(*handle).is_none()));
        for id in 56..=59 {
            assert_eq!(arena.spawn_table().flags(id), Some(0));
        }

        let replacements = (60..=63)
            .map(|id| arena.spawn_entity(ZONE_B, entity(id, 3, 1, 0)).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            replacements
                .iter()
                .map(|handle| handle.slot())
                .collect::<Vec<_>>(),
            [
                parent.slot(),
                second_child.slot(),
                first_child.slot(),
                grandchild.slot(),
            ],
            "head insertion into the free list makes the parent reusable first"
        );
        assert!(
            replacements
                .iter()
                .zip([parent, second_child, first_child, grandchild])
                .all(|(replacement, stale)| replacement.generation() != stale.generation())
        );
    }

    #[test]
    fn runtime_child_teardown_clears_only_spawn_slot_zero_active_bit() {
        let mut arena = ObjectArena::new();
        let parent = arena.spawn_entity(ZONE_A, entity(64, 3, 1, 0)).unwrap();
        arena.spawn_table_mut().set_flags(0, 0x8000_000f).unwrap();
        let child = arena.create_child(parent, ZONE_A, 5, 2, false).unwrap();
        assert_eq!(arena.spawn_table().flags(0), Some(0x8000_000f));

        assert_eq!(arena.despawn_subtree(child).unwrap(), [child]);
        assert_eq!(arena.spawn_table().flags(0), Some(0x8000_000e));
        assert_eq!(
            arena.spawn_table().flags(64),
            Some(SPAWN_ACTIVE_BIT),
            "runtime teardown must not clear its live parent's entity bit"
        );
    }

    #[test]
    fn paired_runtime_teardown_uses_the_live_pid_spawn_slot() {
        let mut arena = ObjectArena::new();
        let parent = arena.spawn_entity(ZONE_A, entity(64, 3, 1, 0)).unwrap();
        let child = arena.create_child(parent, ZONE_A, 5, 2, false).unwrap();
        arena.spawn_table_mut().set_flags(0, 0x8000_000f).unwrap();
        arena.spawn_table_mut().set_flags(131, 0x4000_000f).unwrap();

        assert_eq!(arena.despawn_leaf_with_spawn_id(child, 131).unwrap(), child);
        assert_eq!(arena.spawn_table().flags(0), Some(0x8000_000f));
        assert_eq!(arena.spawn_table().flags(131), Some(0x4000_000e));
    }

    #[test]
    fn pool_and_dedicated_main_have_independent_bounds() {
        let mut arena = ObjectArena::new();
        for offset in 0..OBJECT_POOL_CAPACITY {
            let id = 10 + u16::try_from(offset).unwrap();
            let handle = arena.spawn_entity(ZONE_A, entity(id, 3, 1, 0)).unwrap();
            assert!(!handle.is_dedicated_main());
        }
        assert_eq!(arena.remaining_pool_capacity(), 0);
        assert_eq!(
            arena.spawn_entity(ZONE_A, entity(200, 3, 1, 0)),
            Err(SpawnError::ObjectPoolFull)
        );

        let main = arena.spawn_entity(ZONE_A, entity(201, 3, 0, 0)).unwrap();
        assert!(main.is_dedicated_main());
        assert_eq!(arena.len(), OBJECT_ARENA_CAPACITY);
    }

    #[test]
    fn native_capacity_keeps_all_96_pool_objects_alongside_main() {
        let mut arena = ObjectArena::new();
        let main = arena.spawn_entity(ZONE_A, entity(200, 3, 0, 0)).unwrap();
        let parent = arena.spawn_entity(ZONE_A, entity(201, 3, 1, 0)).unwrap();

        for _ in 1..OBJECT_POOL_CAPACITY {
            arena.create_child(parent, ZONE_A, 39, 1, false).unwrap();
        }

        assert!(main.is_dedicated_main());
        assert_eq!(arena.len(), OBJECT_ARENA_CAPACITY);
        assert_eq!(arena.remaining_pool_capacity(), 0);
        assert_eq!(
            arena.create_child(parent, ZONE_A, 39, 1, false),
            Err(RuntimeCreateError::ObjectPoolFull),
            "opcode 0x8a cannot reclaim after the 96-slot ordinary pool fills"
        );
    }

    #[test]
    fn runtime_children_use_object_parent_without_touching_spawn_flags() {
        let mut arena = ObjectArena::new();
        let parent = arena.spawn_entity(ZONE_A, entity(60, 3, 1, 0)).unwrap();
        let child = arena.create_child(parent, ZONE_A, 5, 2, false).unwrap();

        assert_eq!(
            arena.get(child).unwrap().origin(),
            ObjectOrigin::Runtime {
                executable: 5,
                subtype: 2
            }
        );
        assert_eq!(
            arena.get(child).unwrap().parent(),
            TreeParent::Object(parent)
        );
        assert_eq!(arena.spawn_table().flags(60), Some(1));
        arena.despawn_subtree(child).unwrap();
        assert_eq!(arena.spawn_table().flags(60), Some(1));
    }

    #[test]
    fn host_created_objects_bind_directly_to_the_requested_retail_root() {
        let mut arena = ObjectArena::new();
        let root = RootHandle::new(1).unwrap();
        let first = arena.create_root_object(root, ZONE_A, 4, 8, true).unwrap();
        let second = arena.create_root_object(root, ZONE_A, 4, 2, true).unwrap();

        assert_eq!(arena.get(first).unwrap().parent(), TreeParent::Root(root));
        assert_eq!(arena.get(second).unwrap().parent(), TreeParent::Root(root));
        assert_eq!(
            arena
                .preorder(TreeParent::Root(root))
                .unwrap()
                .collect::<Vec<_>>(),
            [second, first],
            "GoolObjectAddChild inserts host-created objects at the root head"
        );
        assert_eq!(arena.spawn_table().flags(0), Some(0));
    }

    #[test]
    fn runtime_reclaim_reports_first_flagged_preorder_candidate_without_releasing_it() {
        let mut arena = ObjectArena::new();
        let parent = arena.spawn_entity(ZONE_A, entity(100, 3, 1, 0)).unwrap();
        let candidate = arena.spawn_entity(ZONE_A, entity(101, 3, 1, 0)).unwrap();
        for id in 102..196 {
            arena.spawn_entity(ZONE_A, entity(id, 3, 1, 0)).unwrap();
        }
        assert_eq!(arena.remaining_pool_capacity(), 0);
        assert_eq!(
            arena.create_child(parent, ZONE_A, 5, 0, false),
            Err(RuntimeCreateError::ObjectPoolFull)
        );

        arena
            .set_state_flags(candidate, RECLAIMABLE_STATE_FLAG)
            .unwrap();
        assert_eq!(arena.first_reclaimable(), Ok(Some(candidate)));
        assert_eq!(
            arena.create_child(parent, ZONE_A, 5, 0, true),
            Err(RuntimeCreateError::ReclaimRequired(candidate))
        );
        assert!(arena.get(candidate).is_some());
        assert_eq!(arena.spawn_table().flags(101), Some(1));
        assert_eq!(arena.remaining_pool_capacity(), 0);
    }

    #[test]
    fn native_reclaim_search_does_not_exclude_the_active_parent() {
        let mut arena = ObjectArena::new();
        for id in 100..196 {
            arena.spawn_entity(ZONE_A, entity(id, 3, 1, 0)).unwrap();
        }
        let parent = arena
            .preorder(TreeParent::Root(ZONE_OBJECT_ROOT))
            .unwrap()
            .next()
            .unwrap();
        arena
            .set_state_flags(parent, RECLAIMABLE_STATE_FLAG)
            .unwrap();

        assert_eq!(arena.first_reclaimable(), Ok(Some(parent)));
        assert_eq!(
            arena.create_child(parent, ZONE_A, 5, 0, true),
            Err(RuntimeCreateError::ReclaimRequired(parent))
        );
    }

    #[test]
    fn runtime_crash_creation_reuses_dedicated_main_object() {
        let mut arena = ObjectArena::new();
        let parent = arena.spawn_entity(ZONE_A, entity(200, 3, 1, 0)).unwrap();
        let main = arena.spawn_entity(ZONE_A, entity(201, 3, 0, 0)).unwrap();
        arena.set_state_flags(main, u32::MAX).unwrap();
        let free_before = arena.remaining_pool_capacity();

        assert_eq!(arena.create_child(parent, ZONE_A, 0, 0, false), Ok(main));
        assert_eq!(arena.remaining_pool_capacity(), free_before);
        assert_eq!(
            arena.get(main).unwrap().parent(),
            TreeParent::Object(parent)
        );
        assert_eq!(
            arena.get(main).unwrap().origin(),
            ObjectOrigin::Runtime {
                executable: 0,
                subtype: 0
            }
        );
        assert_eq!(arena.get(main).unwrap().state_flags(), 0);
        assert_eq!(arena.spawn_table().flags(201), Some(1));
        arena.despawn_subtree(main).unwrap();
        assert_eq!(arena.spawn_table().flags(201), Some(1));
    }

    #[test]
    fn runtime_crash_creation_activates_empty_dedicated_slot() {
        let mut arena = ObjectArena::new();
        let parent = arena.spawn_entity(ZONE_A, entity(202, 3, 1, 0)).unwrap();
        let free_before = arena.remaining_pool_capacity();
        let main = arena.create_child(parent, ZONE_A, 0, 0, false).unwrap();

        assert!(main.is_dedicated_main());
        assert_eq!(arena.main_object(), Some(main));
        assert_eq!(arena.remaining_pool_capacity(), free_before);
        assert_eq!(
            arena.get(main).unwrap().origin(),
            ObjectOrigin::Runtime {
                executable: 0,
                subtype: 0
            }
        );
        assert_eq!(
            arena.get(main).unwrap().parent(),
            TreeParent::Object(parent)
        );
    }

    #[test]
    fn despawn_invalidates_handles_clears_active_bits_and_reuses_pool_head() {
        let mut arena = ObjectArena::new();
        let parent = arena.spawn_entity(ZONE_A, entity(50, 3, 1, 0)).unwrap();
        let child = arena.spawn_entity(ZONE_A, entity(51, 3, 1, 0)).unwrap();
        arena.add_child(TreeParent::Object(parent), child).unwrap();
        arena.despawn_subtree(parent).unwrap();

        assert!(arena.get(parent).is_none());
        assert!(arena.get(child).is_none());
        assert_eq!(arena.spawn_table().flags(50), Some(0));
        assert_eq!(arena.spawn_table().flags(51), Some(0));
        let replacement = arena.spawn_entity(ZONE_B, entity(52, 3, 1, 0)).unwrap();
        assert_eq!(replacement.slot(), parent.slot());
        assert_ne!(replacement.generation(), parent.generation());
        assert_eq!(
            arena.add_child(TreeParent::Root(ENEMY_OBJECT_ROOT), parent),
            Err(TreeError::InvalidObject(parent))
        );
    }

    proptest! {
        #[test]
        fn display_and_group_filters_are_exact(
            display_flags in any::<u32>(),
            group in any::<u16>(),
        ) {
            let descriptors = [entity(70, group, 1, 0)];
            let neighbors = [NeighborZone {
                eid: ZONE_A,
                display_flags,
                entities: &descriptors,
            }];
            let mut arena = ObjectArena::new();
            let attempts = arena.spawn_current_zone_neighbors(&neighbors);
            let expected = usize::from(display_flags & 2 != 0 && group == 3);
            prop_assert_eq!(attempts.len(), expected);
            prop_assert_eq!(arena.len(), expected);
        }

        #[test]
        fn repeated_head_insertion_reverses_spawn_order(count in 1_usize..64) {
            let descriptors = (0..count)
                .map(|index| entity(100 + u16::try_from(index).unwrap(), 3, 1, 0))
                .collect::<Vec<_>>();
            let neighbors = [NeighborZone {
                eid: ZONE_A,
                display_flags: 2,
                entities: &descriptors,
            }];
            let mut arena = ObjectArena::new();
            let attempts = arena.spawn_current_zone_neighbors(&neighbors);
            prop_assert!(attempts.iter().all(|attempt| attempt.result.is_ok()));
            let actual = object_ids(
                &arena,
                arena.preorder(TreeParent::Root(ZONE_OBJECT_ROOT)).unwrap(),
            );
            let expected = descriptors.iter().rev().map(|descriptor| descriptor.id).collect::<Vec<_>>();
            prop_assert_eq!(actual, expected);
        }
    }
}
