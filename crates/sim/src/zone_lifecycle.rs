//! Deterministic, pointer-free retail zone activation and load-list planning.
//!
//! The retail engine mutates `display_flags` in each loaded ZDAT header and
//! performs zone transitions in a strict order. This module owns equivalent
//! mutable values behind stable [`Eid`] identities. It deliberately does not
//! open pages, terminate GOOL objects, or retain pointers into an NSF. Instead,
//! [`ZoneLifecycle::plan_transition`] produces an ordered action trace that a
//! host can validate against its object and paging registries before committing
//! the lifecycle state with [`ZoneLifecycle::commit_transition`].

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

use crust_formats::{
    binary::{Eid, PageIndex},
    stream::ZoneLoadList,
};

/// ZDAT bit zero: objects for the zone have been activated.
pub const ZONE_OBJECTS_ACTIVE: u32 = 0x1;
/// ZDAT bit one: the next `LevelSpawnObjects` neighbor scan visits the zone.
pub const ZONE_SPAWN_SCAN_ELIGIBLE: u32 = 0x2;
/// ZDAT bit two: activation happened during the initial level update.
pub const ZONE_INITIAL_ACTIVATION: u32 = 0x4;
/// Low flags produced when a zeroed neighbor is activated at level start.
pub const INITIAL_ACTIVATION_FLAGS: u32 = 0x7;
/// Low flags produced when a zeroed neighbor is activated by a normal move.
pub const TRANSITION_ACTIVATION_FLAGS: u32 = 0x3;

const DEPARTED_ZONE_CLEAR_MASK: u32 = ZONE_OBJECTS_ACTIVE | ZONE_SPAWN_SCAN_ELIGIBLE;

/// An ordered copy of the entry and page arrays embedded in a ZDAT header.
///
/// This is intentionally not a set. The native transition closes every old
/// element and then opens every new element, including resources present in
/// both lists and any repeated serialized values.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OrderedZoneLoadList {
    entries: Vec<Eid>,
    pages: Vec<PageIndex>,
}

impl OrderedZoneLoadList {
    /// Owns load-list resources without sorting or deduplicating them.
    #[must_use]
    pub fn new(
        entries: impl IntoIterator<Item = Eid>,
        pages: impl IntoIterator<Item = PageIndex>,
    ) -> Self {
        Self {
            entries: entries.into_iter().collect(),
            pages: pages.into_iter().collect(),
        }
    }

    /// Entry EIDs in their exact ZDAT order.
    #[must_use]
    pub fn entries(&self) -> &[Eid] {
        &self.entries
    }

    /// Page indices in their exact ZDAT order.
    #[must_use]
    pub fn pages(&self) -> &[PageIndex] {
        &self.pages
    }
}

impl From<&ZoneLoadList> for OrderedZoneLoadList {
    fn from(load_list: &ZoneLoadList) -> Self {
        Self::new(
            load_list.entries.iter().copied(),
            load_list.pages.iter().copied(),
        )
    }
}

/// Owned runtime inputs for one ZDAT zone.
///
/// `neighbors` retains header order because both departure termination and the
/// next-frame entity scan are order-sensitive. `display_flags` may include
/// unrelated high bits; lifecycle operations modify only the low three bits
/// used by `LevelUpdate`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZoneLifecycleZone {
    eid: Eid,
    display_flags: u32,
    neighbors: Vec<Eid>,
    load_list: OrderedZoneLoadList,
}

impl ZoneLifecycleZone {
    /// Creates one owned zone record without sorting its neighbors or loads.
    #[must_use]
    pub fn new(
        eid: Eid,
        display_flags: u32,
        neighbors: impl IntoIterator<Item = Eid>,
        load_list: OrderedZoneLoadList,
    ) -> Self {
        Self {
            eid,
            display_flags,
            neighbors: neighbors.into_iter().collect(),
            load_list,
        }
    }

    /// Stable ZDAT identity.
    #[must_use]
    pub const fn eid(&self) -> Eid {
        self.eid
    }

    /// Current mutable ZDAT display flags.
    #[must_use]
    pub const fn display_flags(&self) -> u32 {
        self.display_flags
    }

    /// Neighbor EIDs in exact header order.
    #[must_use]
    pub fn neighbors(&self) -> &[Eid] {
        &self.neighbors
    }

    /// Exact ordered load list for this zone.
    #[must_use]
    pub const fn load_list(&self) -> &OrderedZoneLoadList {
        &self.load_list
    }
}

/// One externally observable operation in exact native transition order.
///
/// A host should execute object termination before acknowledging its following
/// [`SetDisplayFlags`](Self::SetDisplayFlags), and should execute every close
/// and open even when the old and new lists overlap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ZoneTransitionAction {
    /// Terminate the active GOOL objects belonging to a departed neighbor.
    TerminateZoneObjects(Eid),
    /// Apply one lifecycle-owned display flag mutation.
    SetDisplayFlags { zone: Eid, before: u32, after: u32 },
    /// Close one old load-list entry.
    CloseEntry(Eid),
    /// Close one old load-list page.
    ClosePage(PageIndex),
    /// Open one new load-list entry.
    OpenEntry(Eid),
    /// Open one new load-list page.
    OpenPage(PageIndex),
}

/// One neighbor visited by the following frame's ordered spawn scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpawnScanZone {
    /// Position in the new current zone's neighbor array.
    pub neighbor_index: usize,
    /// Neighbor ZDAT identity.
    pub zone: Eid,
    /// Post-transition flags used by the `display_flags & 2` predicate.
    pub display_flags: u32,
}

/// Immutable, fully validated transition proposal.
///
/// Planning never mutates [`ZoneLifecycle`]. A plan includes the resulting
/// complete display-flag vector internally, so commit can validate the entire
/// proposal before making any state change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZoneTransitionPlan {
    base_revision: u64,
    next_revision: u64,
    previous_zone: Option<Eid>,
    next_zone: Eid,
    activation_marker: bool,
    actions: Vec<ZoneTransitionAction>,
    next_frame_spawn_scan: Vec<SpawnScanZone>,
    resulting_display_flags: Vec<u32>,
}

/// Fully validated `LevelRestart` teardown followed by its null-origin
/// `LevelUpdate`. Unlike an ordinary transition, restart terminates every
/// active neighbor of the old current zone even when it is also present in the
/// restored band.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZoneHardRestartPlan {
    base_revision: u64,
    next_revision: u64,
    previous_zone: Option<Eid>,
    next_zone: Eid,
    activation_marker: bool,
    actions: Vec<ZoneTransitionAction>,
    next_frame_spawn_scan: Vec<SpawnScanZone>,
    resulting_display_flags: Vec<u32>,
}

impl ZoneHardRestartPlan {
    #[must_use]
    pub const fn previous_zone(&self) -> Option<Eid> {
        self.previous_zone
    }

    #[must_use]
    pub const fn next_zone(&self) -> Eid {
        self.next_zone
    }

    #[must_use]
    pub const fn activation_marker(&self) -> bool {
        self.activation_marker
    }

    #[must_use]
    pub fn actions(&self) -> &[ZoneTransitionAction] {
        &self.actions
    }

    #[must_use]
    pub fn next_frame_spawn_scan(&self) -> &[SpawnScanZone] {
        &self.next_frame_spawn_scan
    }
}

impl ZoneTransitionPlan {
    /// Zone active when the plan was created, or `None` for initial activation.
    #[must_use]
    pub const fn previous_zone(&self) -> Option<Eid> {
        self.previous_zone
    }

    /// Zone that becomes current after commit.
    #[must_use]
    pub const fn next_zone(&self) -> Eid {
        self.next_zone
    }

    /// Whether `LevelUpdate` applies ZDAT marker bit four to the new band.
    ///
    /// Retail enables this for the initial gameplay update and for explicit
    /// flag-two/title updates. Ordinary camera crossings clear the marker.
    #[must_use]
    pub const fn activation_marker(&self) -> bool {
        self.activation_marker
    }

    /// Exact object, flag, close, and open action sequence.
    #[must_use]
    pub fn actions(&self) -> &[ZoneTransitionAction] {
        &self.actions
    }

    /// Ordered zones eligible for the following frame's entity spawn scan.
    #[must_use]
    pub fn next_frame_spawn_scan(&self) -> &[SpawnScanZone] {
        &self.next_frame_spawn_scan
    }

    /// Whether the requested zone was already current.
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.previous_zone == Some(self.next_zone)
    }
}

/// Checked failures while constructing, planning, or committing lifecycle data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ZoneLifecycleError {
    /// Two input records claim the same ZDAT identity.
    DuplicateZone(Eid),
    /// A header neighbor does not resolve to an owned zone record.
    UnknownNeighbor {
        zone: Eid,
        neighbor_index: usize,
        neighbor: Eid,
    },
    /// A requested transition target does not exist.
    UnknownZone(Eid),
    /// The lifecycle changed since this plan was produced.
    StalePlan {
        expected_revision: u64,
        plan_revision: u64,
    },
    /// A plan was not produced from the exact current topology and flags.
    InvalidPlan(Eid),
    /// The monotonic plan revision cannot be advanced.
    RevisionOverflow,
}

impl fmt::Display for ZoneLifecycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateZone(zone) => write!(formatter, "duplicate lifecycle zone {zone}"),
            Self::UnknownNeighbor {
                zone,
                neighbor_index,
                neighbor,
            } => write!(
                formatter,
                "lifecycle zone {zone} neighbor {neighbor_index} references absent zone {neighbor}"
            ),
            Self::UnknownZone(zone) => write!(formatter, "unknown lifecycle zone {zone}"),
            Self::StalePlan {
                expected_revision,
                plan_revision,
            } => write!(
                formatter,
                "stale zone plan revision {plan_revision}; current revision is {expected_revision}"
            ),
            Self::InvalidPlan(zone) => {
                write!(
                    formatter,
                    "zone plan for {zone} does not match current state"
                )
            }
            Self::RevisionOverflow => formatter.write_str("zone lifecycle revision overflow"),
        }
    }
}

impl Error for ZoneLifecycleError {}

/// Ordered mutable ZDAT display state and current-zone identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZoneLifecycle {
    zones: Vec<ZoneLifecycleZone>,
    zone_indices: BTreeMap<Eid, usize>,
    current_zone: Option<Eid>,
    revision: u64,
}

impl ZoneLifecycle {
    /// Validates every zone identity and neighbor before admitting any state.
    ///
    /// Duplicate neighbor values are accepted and preserved because the source
    /// iterates the fixed header array literally.
    pub fn new(
        zones: impl IntoIterator<Item = ZoneLifecycleZone>,
    ) -> Result<Self, ZoneLifecycleError> {
        let zones: Vec<_> = zones.into_iter().collect();
        let mut zone_indices = BTreeMap::new();
        for (index, zone) in zones.iter().enumerate() {
            if zone_indices.insert(zone.eid, index).is_some() {
                return Err(ZoneLifecycleError::DuplicateZone(zone.eid));
            }
        }
        for zone in &zones {
            for (neighbor_index, neighbor) in zone.neighbors.iter().copied().enumerate() {
                if !zone_indices.contains_key(&neighbor) {
                    return Err(ZoneLifecycleError::UnknownNeighbor {
                        zone: zone.eid,
                        neighbor_index,
                        neighbor,
                    });
                }
            }
        }
        Ok(Self {
            zones,
            zone_indices,
            current_zone: None,
            revision: 0,
        })
    }

    /// Current zone, or `None` before initial activation.
    #[must_use]
    pub const fn current_zone(&self) -> Option<Eid> {
        self.current_zone
    }

    /// Monotonic state revision used to reject stale plans.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Looks up one zone by stable EID.
    #[must_use]
    pub fn zone(&self, eid: Eid) -> Option<&ZoneLifecycleZone> {
        self.zone_indices
            .get(&eid)
            .and_then(|index| self.zones.get(*index))
    }

    /// Iterates zones in their original input order.
    #[must_use]
    pub fn zones(&self) -> impl ExactSizeIterator<Item = &ZoneLifecycleZone> {
        self.zones.iter()
    }

    /// Produces a complete transition trace without changing lifecycle state.
    ///
    /// The trace order is:
    ///
    /// 1. terminate and clear each departed active neighbor in the old header's
    ///    order;
    /// 2. close every old entry, then every old page;
    /// 3. open every new entry, then every new page;
    /// 4. activate/update each new neighbor in the new header's order.
    pub fn plan_transition(
        &self,
        next_zone: Eid,
    ) -> Result<ZoneTransitionPlan, ZoneLifecycleError> {
        self.plan_transition_with_marker(next_zone, self.current_zone.is_none())
    }

    /// Produces a transition trace with the source `LevelUpdate` marker flag.
    ///
    /// The native flag is not solely an initial/non-initial distinction:
    /// title and explicit flag-two updates can set bit four after startup.
    /// Hosts that model those calls must pass their computed `flag != 0` here.
    pub fn plan_transition_with_marker(
        &self,
        next_zone: Eid,
        activation_marker: bool,
    ) -> Result<ZoneTransitionPlan, ZoneLifecycleError> {
        let next_index = self
            .zone_indices
            .get(&next_zone)
            .copied()
            .ok_or(ZoneLifecycleError::UnknownZone(next_zone))?;
        let next = &self.zones[next_index];

        if self.current_zone == Some(next_zone) {
            return Ok(ZoneTransitionPlan {
                base_revision: self.revision,
                next_revision: self.revision,
                previous_zone: self.current_zone,
                next_zone,
                activation_marker,
                actions: Vec::new(),
                next_frame_spawn_scan: spawn_scan(
                    next,
                    &self.current_display_flags(),
                    &self.zone_indices,
                ),
                resulting_display_flags: self.current_display_flags(),
            });
        }

        let next_revision = self
            .revision
            .checked_add(1)
            .ok_or(ZoneLifecycleError::RevisionOverflow)?;
        let mut resulting_display_flags = self.current_display_flags();
        let mut actions = Vec::new();

        if let Some(previous_eid) = self.current_zone {
            let previous = self
                .zone(previous_eid)
                .ok_or(ZoneLifecycleError::UnknownZone(previous_eid))?;
            for departed in previous.neighbors.iter().copied() {
                if next.neighbors.contains(&departed) {
                    continue;
                }
                let departed_index = self.zone_indices[&departed];
                let before = resulting_display_flags[departed_index];
                if before & ZONE_OBJECTS_ACTIVE == 0 {
                    continue;
                }
                let after = before & !DEPARTED_ZONE_CLEAR_MASK;
                actions.push(ZoneTransitionAction::TerminateZoneObjects(departed));
                actions.push(ZoneTransitionAction::SetDisplayFlags {
                    zone: departed,
                    before,
                    after,
                });
                resulting_display_flags[departed_index] = after;
            }

            for entry in previous.load_list.entries.iter().copied() {
                actions.push(ZoneTransitionAction::CloseEntry(entry));
            }
            for page in previous.load_list.pages.iter().copied() {
                actions.push(ZoneTransitionAction::ClosePage(page));
            }
        }

        for entry in next.load_list.entries.iter().copied() {
            actions.push(ZoneTransitionAction::OpenEntry(entry));
        }
        for page in next.load_list.pages.iter().copied() {
            actions.push(ZoneTransitionAction::OpenPage(page));
        }

        for neighbor in next.neighbors.iter().copied() {
            let neighbor_index = self.zone_indices[&neighbor];
            let before = resulting_display_flags[neighbor_index];
            let mut after = before;
            if after & ZONE_OBJECTS_ACTIVE == 0 {
                after |= TRANSITION_ACTIVATION_FLAGS;
            }
            if activation_marker {
                after |= ZONE_INITIAL_ACTIVATION;
            } else {
                after &= !ZONE_INITIAL_ACTIVATION;
            }
            if after != before {
                actions.push(ZoneTransitionAction::SetDisplayFlags {
                    zone: neighbor,
                    before,
                    after,
                });
                resulting_display_flags[neighbor_index] = after;
            }
        }

        let next_frame_spawn_scan = spawn_scan(next, &resulting_display_flags, &self.zone_indices);
        Ok(ZoneTransitionPlan {
            base_revision: self.revision,
            next_revision,
            previous_zone: self.current_zone,
            next_zone,
            activation_marker,
            actions,
            next_frame_spawn_scan,
            resulting_display_flags,
        })
    }

    /// Plans the exact zone/paging half of native `LevelRestart`.
    ///
    /// The trace first visits every active neighbor in the old current
    /// header's serialized order, clears bits zero/one, unloads the old
    /// current zone's complete load list, then performs a null-origin
    /// `LevelUpdate` to the saved zone. Shared resources are deliberately
    /// closed and reopened, and shared zones are deliberately terminated and
    /// reactivated.
    pub fn plan_hard_restart(
        &self,
        next_zone: Eid,
        activation_marker: bool,
    ) -> Result<ZoneHardRestartPlan, ZoneLifecycleError> {
        let next_index = self
            .zone_indices
            .get(&next_zone)
            .copied()
            .ok_or(ZoneLifecycleError::UnknownZone(next_zone))?;
        let next = &self.zones[next_index];
        let next_revision = self
            .revision
            .checked_add(1)
            .ok_or(ZoneLifecycleError::RevisionOverflow)?;
        let mut resulting_display_flags = self.current_display_flags();
        let mut actions = Vec::new();

        if let Some(previous_eid) = self.current_zone {
            let previous = self
                .zone(previous_eid)
                .ok_or(ZoneLifecycleError::UnknownZone(previous_eid))?;
            for neighbor in previous.neighbors.iter().copied() {
                let neighbor_index = self.zone_indices[&neighbor];
                let before = resulting_display_flags[neighbor_index];
                if before & ZONE_OBJECTS_ACTIVE == 0 {
                    continue;
                }
                let after = before & !DEPARTED_ZONE_CLEAR_MASK;
                actions.push(ZoneTransitionAction::TerminateZoneObjects(neighbor));
                actions.push(ZoneTransitionAction::SetDisplayFlags {
                    zone: neighbor,
                    before,
                    after,
                });
                resulting_display_flags[neighbor_index] = after;
            }
            for entry in previous.load_list.entries.iter().copied() {
                actions.push(ZoneTransitionAction::CloseEntry(entry));
            }
            for page in previous.load_list.pages.iter().copied() {
                actions.push(ZoneTransitionAction::ClosePage(page));
            }
        }

        for entry in next.load_list.entries.iter().copied() {
            actions.push(ZoneTransitionAction::OpenEntry(entry));
        }
        for page in next.load_list.pages.iter().copied() {
            actions.push(ZoneTransitionAction::OpenPage(page));
        }
        for neighbor in next.neighbors.iter().copied() {
            let neighbor_index = self.zone_indices[&neighbor];
            let before = resulting_display_flags[neighbor_index];
            let mut after = before;
            if after & ZONE_OBJECTS_ACTIVE == 0 {
                after |= TRANSITION_ACTIVATION_FLAGS;
            }
            if activation_marker {
                after |= ZONE_INITIAL_ACTIVATION;
            } else {
                after &= !ZONE_INITIAL_ACTIVATION;
            }
            if after != before {
                actions.push(ZoneTransitionAction::SetDisplayFlags {
                    zone: neighbor,
                    before,
                    after,
                });
                resulting_display_flags[neighbor_index] = after;
            }
        }

        let next_frame_spawn_scan = spawn_scan(next, &resulting_display_flags, &self.zone_indices);
        Ok(ZoneHardRestartPlan {
            base_revision: self.revision,
            next_revision,
            previous_zone: self.current_zone,
            next_zone,
            activation_marker,
            actions,
            next_frame_spawn_scan,
            resulting_display_flags,
        })
    }

    /// Commits one preflighted hard-restart plan atomically.
    pub fn commit_hard_restart(
        &mut self,
        plan: &ZoneHardRestartPlan,
    ) -> Result<(), ZoneLifecycleError> {
        if plan.base_revision != self.revision {
            return Err(ZoneLifecycleError::StalePlan {
                expected_revision: self.revision,
                plan_revision: plan.base_revision,
            });
        }
        let expected = self.plan_hard_restart(plan.next_zone, plan.activation_marker)?;
        if expected != *plan {
            return Err(ZoneLifecycleError::InvalidPlan(plan.next_zone));
        }
        for (zone, display_flags) in self
            .zones
            .iter_mut()
            .zip(plan.resulting_display_flags.iter().copied())
        {
            zone.display_flags = display_flags;
        }
        self.current_zone = Some(plan.next_zone);
        self.revision = plan.next_revision;
        Ok(())
    }

    /// Atomically commits a plan after revalidating it against current state.
    ///
    /// Callers can first validate or execute fallible external work described
    /// by [`ZoneTransitionPlan::actions`]. A stale, foreign, or modified plan
    /// leaves this value byte-for-byte unchanged.
    pub fn commit_transition(
        &mut self,
        plan: &ZoneTransitionPlan,
    ) -> Result<(), ZoneLifecycleError> {
        if plan.base_revision != self.revision {
            return Err(ZoneLifecycleError::StalePlan {
                expected_revision: self.revision,
                plan_revision: plan.base_revision,
            });
        }
        let expected = self.plan_transition_with_marker(plan.next_zone, plan.activation_marker)?;
        if expected != *plan {
            return Err(ZoneLifecycleError::InvalidPlan(plan.next_zone));
        }

        // All validation is complete before the first mutation.
        for (zone, display_flags) in self
            .zones
            .iter_mut()
            .zip(plan.resulting_display_flags.iter().copied())
        {
            zone.display_flags = display_flags;
        }
        self.current_zone = Some(plan.next_zone);
        self.revision = plan.next_revision;
        Ok(())
    }

    /// Plans and commits a transition, returning its exact action trace.
    ///
    /// Integrations with fallible external object or paging operations should
    /// use the split plan/commit API so those operations can be validated first.
    pub fn transition(&mut self, next_zone: Eid) -> Result<ZoneTransitionPlan, ZoneLifecycleError> {
        let plan = self.plan_transition(next_zone)?;
        self.commit_transition(&plan)?;
        Ok(plan)
    }

    /// Plans and commits a transition with an explicit native marker flag.
    pub fn transition_with_marker(
        &mut self,
        next_zone: Eid,
        activation_marker: bool,
    ) -> Result<ZoneTransitionPlan, ZoneLifecycleError> {
        let plan = self.plan_transition_with_marker(next_zone, activation_marker)?;
        self.commit_transition(&plan)?;
        Ok(plan)
    }

    /// Current zone's ordered next-frame spawn scan after committed flags.
    #[must_use]
    pub fn next_frame_spawn_scan(&self) -> Vec<SpawnScanZone> {
        let Some(current) = self.current_zone.and_then(|eid| self.zone(eid)) else {
            return Vec::new();
        };
        spawn_scan(current, &self.current_display_flags(), &self.zone_indices)
    }

    /// Current header neighbors whose native display bit zero is set, in
    /// serialized first-occurrence order. Repeated EIDs appear only once
    /// because `LevelRestart` clears their low flags on the first visit.
    #[must_use]
    pub fn active_neighbor_zones(&self) -> Vec<Eid> {
        let Some(current) = self.current_zone.and_then(|eid| self.zone(eid)) else {
            return Vec::new();
        };
        let mut visited = BTreeSet::new();
        current
            .neighbors
            .iter()
            .copied()
            .filter(|neighbor| {
                // The native restart clears an active neighbor's low flags
                // immediately. A repeated serialized EID is therefore
                // dormant by the time its later occurrence is visited.
                visited.insert(*neighbor)
                    && self.zones[self.zone_indices[neighbor]].display_flags & ZONE_OBJECTS_ACTIVE
                        != 0
            })
            .collect()
    }

    fn current_display_flags(&self) -> Vec<u32> {
        self.zones
            .iter()
            .map(ZoneLifecycleZone::display_flags)
            .collect()
    }
}

fn spawn_scan(
    zone: &ZoneLifecycleZone,
    display_flags: &[u32],
    zone_indices: &BTreeMap<Eid, usize>,
) -> Vec<SpawnScanZone> {
    zone.neighbors
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(neighbor_index, neighbor)| {
            let flags = display_flags[zone_indices[&neighbor]];
            (flags & ZONE_SPAWN_SCAN_ELIGIBLE != 0).then_some(SpawnScanZone {
                neighbor_index,
                zone: neighbor,
                display_flags: flags,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eid(name: &str) -> Eid {
        Eid::from_name(name).expect("test EID uses the retail alphabet")
    }

    fn loads(entries: &[&str], pages: &[u32]) -> OrderedZoneLoadList {
        OrderedZoneLoadList::new(
            entries.iter().map(|name| eid(name)),
            pages.iter().copied().map(PageIndex::new),
        )
    }

    fn zone(
        name: &str,
        flags: u32,
        neighbors: &[&str],
        entries: &[&str],
        pages: &[u32],
    ) -> ZoneLifecycleZone {
        ZoneLifecycleZone::new(
            eid(name),
            flags,
            neighbors.iter().map(|name| eid(name)),
            loads(entries, pages),
        )
    }

    fn flags(lifecycle: &ZoneLifecycle, zone: &str) -> u32 {
        lifecycle.zone(eid(zone)).unwrap().display_flags()
    }

    #[test]
    fn catalog_validation_is_ordered_and_transactional() {
        let duplicate = ZoneLifecycle::new([
            zone("zone0", 0, &[], &[], &[]),
            zone("zone0", 0, &[], &[], &[]),
        ]);
        assert_eq!(
            duplicate,
            Err(ZoneLifecycleError::DuplicateZone(eid("zone0")))
        );

        let unknown = ZoneLifecycle::new([zone("zone0", 0, &["zone1"], &[], &[])]);
        assert_eq!(
            unknown,
            Err(ZoneLifecycleError::UnknownNeighbor {
                zone: eid("zone0"),
                neighbor_index: 0,
                neighbor: eid("zone1"),
            })
        );

        let lifecycle = ZoneLifecycle::new([
            zone("zone1", 1, &[], &[], &[]),
            zone("zone0", 2, &["zone1"], &[], &[]),
        ])
        .unwrap();
        assert_eq!(
            lifecycle
                .zones()
                .map(ZoneLifecycleZone::eid)
                .collect::<Vec<_>>(),
            [eid("zone1"), eid("zone0")]
        );
    }

    #[test]
    fn initial_activation_opens_first_then_sets_low_flags_to_seven() {
        let mut lifecycle = ZoneLifecycle::new([
            zone(
                "start",
                0,
                &["start", "near0"],
                &["ent00", "ent01"],
                &[4, 2],
            ),
            zone("near0", 0x80, &[], &[], &[]),
        ])
        .unwrap();
        let before = lifecycle.clone();
        let plan = lifecycle.plan_transition(eid("start")).unwrap();

        assert_eq!(
            lifecycle, before,
            "planning must not mutate lifecycle state"
        );
        assert_eq!(
            plan.actions(),
            [
                ZoneTransitionAction::OpenEntry(eid("ent00")),
                ZoneTransitionAction::OpenEntry(eid("ent01")),
                ZoneTransitionAction::OpenPage(PageIndex::new(4)),
                ZoneTransitionAction::OpenPage(PageIndex::new(2)),
                ZoneTransitionAction::SetDisplayFlags {
                    zone: eid("start"),
                    before: 0,
                    after: INITIAL_ACTIVATION_FLAGS,
                },
                ZoneTransitionAction::SetDisplayFlags {
                    zone: eid("near0"),
                    before: 0x80,
                    after: 0x80 | INITIAL_ACTIVATION_FLAGS,
                },
            ]
        );
        assert_eq!(
            plan.next_frame_spawn_scan(),
            [
                SpawnScanZone {
                    neighbor_index: 0,
                    zone: eid("start"),
                    display_flags: 7,
                },
                SpawnScanZone {
                    neighbor_index: 1,
                    zone: eid("near0"),
                    display_flags: 0x87,
                },
            ]
        );

        lifecycle.commit_transition(&plan).unwrap();
        assert_eq!(lifecycle.current_zone(), Some(eid("start")));
        assert_eq!(flags(&lifecycle, "start"), 7);
        assert_eq!(flags(&lifecycle, "near0"), 0x87);
        assert_eq!(
            lifecycle.next_frame_spawn_scan(),
            plan.next_frame_spawn_scan()
        );
    }

    #[test]
    fn full_old_list_closes_before_full_new_list_opens_even_with_overlap() {
        let mut lifecycle = ZoneLifecycle::new([
            zone("old00", 0, &[], &["only0", "share", "only0"], &[1, 2, 1]),
            zone("new00", 0, &[], &["share", "only1"], &[2, 3]),
        ])
        .unwrap();
        lifecycle.transition(eid("old00")).unwrap();
        let plan = lifecycle.plan_transition(eid("new00")).unwrap();
        assert_eq!(
            plan.actions(),
            [
                ZoneTransitionAction::CloseEntry(eid("only0")),
                ZoneTransitionAction::CloseEntry(eid("share")),
                ZoneTransitionAction::CloseEntry(eid("only0")),
                ZoneTransitionAction::ClosePage(PageIndex::new(1)),
                ZoneTransitionAction::ClosePage(PageIndex::new(2)),
                ZoneTransitionAction::ClosePage(PageIndex::new(1)),
                ZoneTransitionAction::OpenEntry(eid("share")),
                ZoneTransitionAction::OpenEntry(eid("only1")),
                ZoneTransitionAction::OpenPage(PageIndex::new(2)),
                ZoneTransitionAction::OpenPage(PageIndex::new(3)),
            ]
        );
    }

    #[test]
    fn hard_restart_terminates_shared_band_before_close_reopen_and_reactivation() {
        let mut lifecycle = ZoneLifecycle::new([
            zone(
                "old00",
                0,
                &["shar0", "old00", "dorm0"],
                &["share", "oldld"],
                &[2, 3],
            ),
            zone(
                "new00",
                0,
                &["shar0", "new00"],
                &["share", "newld"],
                &[2, 4],
            ),
            zone("shar0", 0, &[], &[], &[]),
            zone("dorm0", 0, &[], &[], &[]),
        ])
        .unwrap();
        lifecycle.transition(eid("old00")).unwrap();
        // Make one serialized old neighbor dormant; restart must skip it.
        let dormant_index = lifecycle.zone_indices[&eid("dorm0")];
        lifecycle.zones[dormant_index].display_flags &= !ZONE_OBJECTS_ACTIVE;

        let before = lifecycle.clone();
        let plan = lifecycle.plan_hard_restart(eid("new00"), true).unwrap();
        assert_eq!(lifecycle, before, "planning remains transactional");
        assert_eq!(
            plan.actions(),
            [
                ZoneTransitionAction::TerminateZoneObjects(eid("shar0")),
                ZoneTransitionAction::SetDisplayFlags {
                    zone: eid("shar0"),
                    before: 7,
                    after: 4,
                },
                ZoneTransitionAction::TerminateZoneObjects(eid("old00")),
                ZoneTransitionAction::SetDisplayFlags {
                    zone: eid("old00"),
                    before: 7,
                    after: 4,
                },
                ZoneTransitionAction::CloseEntry(eid("share")),
                ZoneTransitionAction::CloseEntry(eid("oldld")),
                ZoneTransitionAction::ClosePage(PageIndex::new(2)),
                ZoneTransitionAction::ClosePage(PageIndex::new(3)),
                ZoneTransitionAction::OpenEntry(eid("share")),
                ZoneTransitionAction::OpenEntry(eid("newld")),
                ZoneTransitionAction::OpenPage(PageIndex::new(2)),
                ZoneTransitionAction::OpenPage(PageIndex::new(4)),
                ZoneTransitionAction::SetDisplayFlags {
                    zone: eid("shar0"),
                    before: 4,
                    after: 7,
                },
                ZoneTransitionAction::SetDisplayFlags {
                    zone: eid("new00"),
                    before: 0,
                    after: 7,
                },
            ]
        );
        assert_eq!(
            plan.next_frame_spawn_scan(),
            [
                SpawnScanZone {
                    neighbor_index: 0,
                    zone: eid("shar0"),
                    display_flags: 7,
                },
                SpawnScanZone {
                    neighbor_index: 1,
                    zone: eid("new00"),
                    display_flags: 7,
                },
            ]
        );
        assert_eq!(
            lifecycle.active_neighbor_zones(),
            [eid("shar0"), eid("old00")]
        );

        lifecycle.commit_hard_restart(&plan).unwrap();
        assert_eq!(lifecycle.current_zone(), Some(eid("new00")));
        assert_eq!(
            lifecycle.active_neighbor_zones(),
            [eid("shar0"), eid("new00")]
        );
    }

    #[test]
    fn departed_active_neighbors_terminate_in_old_header_order_and_retain_bit_four() {
        let mut lifecycle = ZoneLifecycle::new([
            zone("first", 0, &["gone0", "stay0", "gone1"], &["load0"], &[0]),
            zone("next0", 0, &["stay0", "next0"], &["load1"], &[1]),
            zone("gone0", 0, &[], &[], &[]),
            zone("stay0", 0, &[], &[], &[]),
            zone("gone1", 0, &[], &[], &[]),
        ])
        .unwrap();
        lifecycle.transition(eid("first")).unwrap();
        let plan = lifecycle.plan_transition(eid("next0")).unwrap();

        assert_eq!(
            plan.actions(),
            [
                ZoneTransitionAction::TerminateZoneObjects(eid("gone0")),
                ZoneTransitionAction::SetDisplayFlags {
                    zone: eid("gone0"),
                    before: 7,
                    after: 4,
                },
                ZoneTransitionAction::TerminateZoneObjects(eid("gone1")),
                ZoneTransitionAction::SetDisplayFlags {
                    zone: eid("gone1"),
                    before: 7,
                    after: 4,
                },
                ZoneTransitionAction::CloseEntry(eid("load0")),
                ZoneTransitionAction::ClosePage(PageIndex::new(0)),
                ZoneTransitionAction::OpenEntry(eid("load1")),
                ZoneTransitionAction::OpenPage(PageIndex::new(1)),
                ZoneTransitionAction::SetDisplayFlags {
                    zone: eid("stay0"),
                    before: 7,
                    after: 3,
                },
                ZoneTransitionAction::SetDisplayFlags {
                    zone: eid("next0"),
                    before: 0,
                    after: 3,
                },
            ]
        );
        lifecycle.commit_transition(&plan).unwrap();
        assert_eq!(flags(&lifecycle, "gone0"), ZONE_INITIAL_ACTIVATION);
        assert_eq!(flags(&lifecycle, "gone1"), ZONE_INITIAL_ACTIVATION);
        assert_eq!(flags(&lifecycle, "stay0"), TRANSITION_ACTIVATION_FLAGS);
        assert_eq!(flags(&lifecycle, "next0"), TRANSITION_ACTIVATION_FLAGS);
    }

    #[test]
    fn n_sanity_like_neighbor_walk_activates_the_next_band_for_the_next_scan() {
        let mut lifecycle = ZoneLifecycle::new([
            zone("ns000", 0, &["ns000", "ns001"], &[], &[]),
            zone("ns001", 0, &["ns000", "ns001", "ns002"], &[], &[]),
            zone("ns002", 0, &["ns001", "ns002"], &[], &[]),
        ])
        .unwrap();

        lifecycle.transition(eid("ns000")).unwrap();
        assert_eq!(flags(&lifecycle, "ns000"), 7);
        assert_eq!(flags(&lifecycle, "ns001"), 7);
        assert_eq!(flags(&lifecycle, "ns002"), 0);

        let middle = lifecycle.transition(eid("ns001")).unwrap();
        assert_eq!(flags(&lifecycle, "ns000"), 3);
        assert_eq!(flags(&lifecycle, "ns001"), 3);
        assert_eq!(flags(&lifecycle, "ns002"), 3);
        assert_eq!(
            middle.next_frame_spawn_scan(),
            [
                SpawnScanZone {
                    neighbor_index: 0,
                    zone: eid("ns000"),
                    display_flags: 3,
                },
                SpawnScanZone {
                    neighbor_index: 1,
                    zone: eid("ns001"),
                    display_flags: 3,
                },
                SpawnScanZone {
                    neighbor_index: 2,
                    zone: eid("ns002"),
                    display_flags: 3,
                },
            ]
        );

        let last = lifecycle.transition(eid("ns002")).unwrap();
        assert_eq!(
            last.actions().first(),
            Some(&ZoneTransitionAction::TerminateZoneObjects(eid("ns000")))
        );
        assert_eq!(flags(&lifecycle, "ns000"), 0);
        assert_eq!(flags(&lifecycle, "ns001"), 3);
        assert_eq!(flags(&lifecycle, "ns002"), 3);
    }

    #[test]
    fn explicit_marker_updates_match_title_and_flag_two_level_updates() {
        let mut lifecycle = ZoneLifecycle::new([
            zone("zone0", 0, &["zone0", "zone1"], &[], &[]),
            zone("zone1", 0, &["zone0", "zone1"], &[], &[]),
        ])
        .unwrap();

        lifecycle.transition(eid("zone0")).unwrap();
        let ordinary = lifecycle
            .transition_with_marker(eid("zone1"), false)
            .unwrap();
        assert!(!ordinary.activation_marker());
        assert_eq!(flags(&lifecycle, "zone0"), 3);
        assert_eq!(flags(&lifecycle, "zone1"), 3);

        let marked = lifecycle
            .transition_with_marker(eid("zone0"), true)
            .unwrap();
        assert!(marked.activation_marker());
        assert_eq!(flags(&lifecycle, "zone0"), 7);
        assert_eq!(flags(&lifecycle, "zone1"), 7);
    }

    #[test]
    fn inactive_departed_neighbors_are_not_terminated_or_rewritten() {
        let mut lifecycle = ZoneLifecycle::new([
            zone("old00", 0, &["odd00"], &[], &[]),
            zone("new00", 0, &[], &[], &[]),
            // Spawn eligibility without object-active bit is intentionally
            // preserved: ZoneTerminateDifference gates on bit zero only.
            zone("odd00", ZONE_SPAWN_SCAN_ELIGIBLE | 0x40, &[], &[], &[]),
        ])
        .unwrap();
        // Model the legal source state directly: going through initial
        // activation would intentionally set the missing object-active bit.
        lifecycle.current_zone = Some(eid("old00"));
        let plan = lifecycle.plan_transition(eid("new00")).unwrap();
        assert!(plan.actions().is_empty());
        lifecycle.commit_transition(&plan).unwrap();
        assert_eq!(flags(&lifecycle, "odd00"), 0x42);
    }

    #[test]
    fn stale_and_modified_plans_leave_state_unchanged() {
        let mut lifecycle = ZoneLifecycle::new([
            zone("zone0", 0, &[], &[], &[]),
            zone("zone1", 0, &[], &[], &[]),
            zone("zone2", 0, &[], &[], &[]),
        ])
        .unwrap();
        lifecycle.transition(eid("zone0")).unwrap();
        let stale = lifecycle.plan_transition(eid("zone1")).unwrap();
        lifecycle.transition(eid("zone2")).unwrap();
        let before_stale_commit = lifecycle.clone();
        assert_eq!(
            lifecycle.commit_transition(&stale),
            Err(ZoneLifecycleError::StalePlan {
                expected_revision: 2,
                plan_revision: 1,
            })
        );
        assert_eq!(lifecycle, before_stale_commit);

        let mut modified = lifecycle.plan_transition(eid("zone1")).unwrap();
        modified
            .actions
            .push(ZoneTransitionAction::OpenPage(PageIndex::new(99)));
        let before_modified_commit = lifecycle.clone();
        assert_eq!(
            lifecycle.commit_transition(&modified),
            Err(ZoneLifecycleError::InvalidPlan(eid("zone1")))
        );
        assert_eq!(lifecycle, before_modified_commit);
    }

    #[test]
    fn unknown_target_and_same_zone_requests_do_not_mutate_or_reload() {
        let mut lifecycle = ZoneLifecycle::new([zone("zone0", 0, &[], &["entry"], &[7])]).unwrap();
        let pristine = lifecycle.clone();
        assert_eq!(
            lifecycle.plan_transition(eid("nope0")),
            Err(ZoneLifecycleError::UnknownZone(eid("nope0")))
        );
        assert_eq!(lifecycle, pristine);

        lifecycle.transition(eid("zone0")).unwrap();
        let revision = lifecycle.revision();
        let noop = lifecycle.transition(eid("zone0")).unwrap();
        assert!(noop.is_noop());
        assert!(noop.actions().is_empty());
        assert_eq!(lifecycle.revision(), revision);
    }

    #[test]
    fn duplicate_neighbors_remain_visible_in_spawn_scan_order() {
        let mut lifecycle = ZoneLifecycle::new([
            zone("zone0", 0, &["near0", "near0", "zone0"], &[], &[]),
            zone("near0", 0, &[], &[], &[]),
        ])
        .unwrap();
        let plan = lifecycle.transition(eid("zone0")).unwrap();
        assert_eq!(
            plan.next_frame_spawn_scan()
                .iter()
                .map(|candidate| (candidate.neighbor_index, candidate.zone))
                .collect::<Vec<_>>(),
            [(0, eid("near0")), (1, eid("near0")), (2, eid("zone0")),]
        );
        assert_eq!(
            plan.actions()
                .iter()
                .filter(|action| matches!(action, ZoneTransitionAction::SetDisplayFlags { .. }))
                .count(),
            2,
            "the duplicate scan entry must not duplicate the flag mutation"
        );
        assert_eq!(
            lifecycle.active_neighbor_zones(),
            [eid("near0"), eid("zone0")],
            "restart visits the repeated active zone only before its first low-flag clear"
        );
    }
}
