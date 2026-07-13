//! Bounded object allocation and retail-compatible spawn-tree ordering.
//!
//! The original runtime represented objects and the eight logical roots with
//! host pointers. This module keeps the same allocation and tree behavior with
//! validated, generational handles. It intentionally owns no GOOL execution
//! state; callers can use [`ObjectHandle`] as the key for that state.

use crust_formats::{binary::Eid, stream::ZoneEntity};

/// Number of ordinary GOOL objects in the retail allocation table.
pub const OBJECT_POOL_CAPACITY: usize = 96;
/// Number of persistent entity spawn-flag entries.
pub const SPAWN_TABLE_CAPACITY: usize = 304;
/// Number of logical GOOL tree roots.
pub const ROOT_HANDLE_COUNT: usize = 8;

const TOTAL_SLOT_COUNT: usize = OBJECT_POOL_CAPACITY + 1;
const DEDICATED_MAIN_SLOT: usize = OBJECT_POOL_CAPACITY;
const ACTIVE_ZONE_DISPLAY_BIT: u32 = 1 << 1;
const SPAWNABLE_ENTITY_GROUP: u16 = 3;
const SPAWN_ACTIVE_BIT: u32 = 1;
const SPAWN_BLOCKED_BIT: u32 = 2;

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

/// Spawn metadata attached to one live logical object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpawnedObject {
    zone: Eid,
    descriptor: EntitySpawnDescriptor,
    allocation: ObjectAllocation,
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
    pub const fn descriptor(&self) -> EntitySpawnDescriptor {
        self.descriptor
    }

    #[must_use]
    pub const fn allocation(&self) -> ObjectAllocation {
        self.allocation
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
        let handle = self.allocate(zone, descriptor, allocation, parent)?;
        self.insert_at_head(parent, handle);
        self.spawn_table.mark_active(descriptor.id);
        Ok(handle)
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

    /// Despawns an object and all descendants, clearing entity active bits.
    pub fn despawn_subtree(&mut self, root: ObjectHandle) -> Result<(), TreeError> {
        self.object(root)?;
        let mut preorder = [None; TOTAL_SLOT_COUNT];
        let mut len = 0_usize;
        self.collect_subtree(root, &mut preorder, &mut len)?;
        self.detach(root)?;

        // Reverse preorder guarantees every child is released before its parent.
        for handle in preorder[..len].iter().rev().copied().flatten() {
            self.release(handle)?;
        }
        Ok(())
    }

    fn allocate(
        &mut self,
        zone: Eid,
        descriptor: EntitySpawnDescriptor,
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
            descriptor,
            allocation,
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

    fn collect_subtree(
        &self,
        root: ObjectHandle,
        output: &mut [Option<ObjectHandle>; TOTAL_SLOT_COUNT],
        len: &mut usize,
    ) -> Result<(), TreeError> {
        if *len == output.len() {
            return Err(TreeError::WouldCreateCycle);
        }
        output[*len] = Some(root);
        *len += 1;
        let mut child = self.object(root)?.first_child;
        while let Some(current) = child {
            let sibling = self.object(current)?.next_sibling;
            self.collect_subtree(current, output, len)?;
            child = sibling;
        }
        Ok(())
    }

    fn release(&mut self, handle: ObjectHandle) -> Result<(), TreeError> {
        let slot_index = usize::from(handle.slot);
        let object = self.object(handle)?.clone();
        self.spawn_table.clear_active(object.descriptor.id);
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
            .map(|handle| arena.get(handle).unwrap().descriptor().id)
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
        assert_eq!(arena.len(), OBJECT_POOL_CAPACITY + 1);
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
