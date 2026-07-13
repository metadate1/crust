//! Owned, pointer-free camera-path graph assembled from retail ZDAT entries.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::binary::{Eid, FormatError};

use super::{Nsd, Nsf, ZoneHeader, ZoneNeighborPath, ZonePath, ZoneRect};

const ZDAT_ENTRY_TYPE: u32 = 7;

/// Stable value handle for one camera path in one ZDAT zone.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RetailPathId {
    pub zone: Eid,
    pub index: u32,
}

/// All camera data retained from one reachable ZDAT zone.
///
/// This is an owned value. It contains no offsets or borrows into the source
/// NSD/NSF byte buffers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetailZoneNode {
    pub eid: Eid,
    pub origin: [i32; 3],
    pub graphics_flags: u32,
    pub display_flags: u32,
    pub neighbors: Vec<Eid>,
    pub paths: Vec<ZonePath>,
}

impl RetailZoneNode {
    /// Creates an owned zone node. [`RetailZoneGraph::new`] validates all of
    /// its cross-zone path links before admitting it to a graph.
    #[must_use]
    pub fn new(
        eid: Eid,
        origin: [i32; 3],
        graphics_flags: u32,
        display_flags: u32,
        neighbors: Vec<Eid>,
        paths: Vec<ZonePath>,
    ) -> Self {
        Self {
            eid,
            origin,
            graphics_flags,
            display_flags,
            neighbors,
            paths,
        }
    }
}

/// Validated graph of every ZDAT zone reachable from a playable LDAT spawn.
///
/// Zone and path identities remain explicit `(EID, index)` values. Retail
/// neighbor records are resolved through validated vectors rather than being
/// overwritten with native pointers as they were in the original 32-bit C
/// runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetailZoneGraph {
    spawn_path: RetailPathId,
    zones: BTreeMap<Eid, RetailZoneNode>,
    path_count: usize,
}

impl RetailZoneGraph {
    /// Validates and owns a collection of already decoded zone nodes.
    pub fn new(
        spawn_path: RetailPathId,
        nodes: impl IntoIterator<Item = RetailZoneNode>,
    ) -> Result<Self, FormatError> {
        let mut zones = BTreeMap::new();
        for node in nodes {
            let eid = node.eid;
            if zones.insert(eid, node).is_some() {
                return Err(FormatError::global(format!(
                    "zone graph contains duplicate ZDAT {eid}"
                )));
            }
        }

        let spawn_zone = zones.get(&spawn_path.zone).ok_or_else(|| {
            FormatError::global(format!(
                "zone graph spawn ZDAT {} is absent",
                spawn_path.zone
            ))
        })?;
        if usize::try_from(spawn_path.index)
            .ok()
            .and_then(|index| spawn_zone.paths.get(index))
            .is_none()
        {
            return Err(FormatError::global(format!(
                "zone graph spawn path {}:{} is absent",
                spawn_path.zone, spawn_path.index
            )));
        }

        let mut path_count = 0_usize;
        for node in zones.values() {
            path_count = path_count.checked_add(node.paths.len()).ok_or_else(|| {
                FormatError::global("zone graph path count overflows the host size")
            })?;
            for (path_index, path) in node.paths.iter().enumerate() {
                if path.points.is_empty() {
                    return Err(path_error(
                        node.eid,
                        path_index,
                        "contains no camera points",
                    ));
                }
                if path.points.len() > usize::from(u16::MAX) {
                    return Err(path_error(
                        node.eid,
                        path_index,
                        "contains more than 65535 camera points",
                    ));
                }
                for (link_index, link) in path.neighbors.iter().copied().enumerate() {
                    let target_eid = node
                        .neighbors
                        .get(usize::from(link.neighbor_zone_index))
                        .copied()
                        .ok_or_else(|| {
                            path_link_error(
                                node.eid,
                                path_index,
                                link_index,
                                "zone-neighbor index is outside the source ZDAT",
                            )
                        })?;
                    let target = zones.get(&target_eid).ok_or_else(|| {
                        path_link_error(
                            node.eid,
                            path_index,
                            link_index,
                            format!("target ZDAT {target_eid} is absent"),
                        )
                    })?;
                    if target.paths.get(usize::from(link.path_index)).is_none() {
                        return Err(path_link_error(
                            node.eid,
                            path_index,
                            link_index,
                            format!(
                                "target path {target_eid}:{} is outside the target ZDAT",
                                link.path_index
                            ),
                        ));
                    }
                }
            }
        }

        Ok(Self {
            spawn_path,
            zones,
            path_count,
        })
    }

    /// Builds the reachable zone graph with a breadth-first traversal starting
    /// at the playable LDAT spawn zone.
    pub fn from_pair(metadata: &Nsd, nsf: &Nsf, nsf_bytes: &[u8]) -> Result<Self, FormatError> {
        let ldat = metadata
            .ldat()
            .ok_or_else(|| FormatError::global("index-only NSD has no playable zone graph"))?;
        let spawn_path_index = u32::try_from(ldat.spawn_path_index).map_err(|_| {
            FormatError::global(format!(
                "LDAT spawn path index {} is negative",
                ldat.spawn_path_index
            ))
        })?;
        let spawn_path = RetailPathId {
            zone: ldat.spawn_zone,
            index: spawn_path_index,
        };

        let mut queue = VecDeque::from([ldat.spawn_zone]);
        let mut queued = BTreeSet::from([ldat.spawn_zone]);
        let mut nodes = Vec::new();
        while let Some(eid) = queue.pop_front() {
            let entry = nsf.resolve_entry(metadata, eid)?;
            if entry.entry_type != ZDAT_ENTRY_TYPE {
                return Err(FormatError::global(format!(
                    "zone graph EID {eid} has entry type {}; expected ZDAT type {ZDAT_ENTRY_TYPE}",
                    entry.entry_type
                )));
            }
            let header_bytes = entry
                .item(0)
                .ok_or_else(|| FormatError::global(format!("ZDAT {eid} has no header item")))?
                .bytes(nsf_bytes)?;
            let rect_bytes = entry
                .item(1)
                .ok_or_else(|| FormatError::global(format!("ZDAT {eid} has no rectangle item")))?
                .bytes(nsf_bytes)?;
            let header = ZoneHeader::parse(header_bytes).map_err(|error| {
                FormatError::global(format!("ZDAT {eid} header is malformed: {error}"))
            })?;
            let rect = ZoneRect::parse(rect_bytes).map_err(|error| {
                FormatError::global(format!("ZDAT {eid} rectangle is malformed: {error}"))
            })?;

            let path_capacity = usize::try_from(header.path_count).map_err(|_| {
                FormatError::global(format!("ZDAT {eid} path count does not fit the host"))
            })?;
            let mut paths = Vec::with_capacity(path_capacity);
            for path_index in 0..header.path_count {
                let item_index = header.path_item_index(path_index).ok_or_else(|| {
                    FormatError::global(format!(
                        "ZDAT {eid} path {path_index} is outside its declared item range"
                    ))
                })?;
                let item_index = usize::try_from(item_index).map_err(|_| {
                    FormatError::global(format!(
                        "ZDAT {eid} path {path_index} item index does not fit the host"
                    ))
                })?;
                let path_bytes = entry
                    .item(item_index)
                    .ok_or_else(|| {
                        FormatError::global(format!(
                            "ZDAT {eid} path {path_index} item {item_index} is absent"
                        ))
                    })?
                    .bytes(nsf_bytes)?;
                paths.push(ZonePath::parse(path_bytes).map_err(|error| {
                    FormatError::global(format!(
                        "ZDAT {eid} path {path_index} is malformed: {error}"
                    ))
                })?);
            }

            for neighbor in header.neighbors.iter().copied() {
                if queued.insert(neighbor) {
                    queue.push_back(neighbor);
                }
            }
            nodes.push(RetailZoneNode::new(
                eid,
                rect.origin,
                header.graphics.flags,
                header.display_flags,
                header.neighbors,
                paths,
            ));
        }

        Self::new(spawn_path, nodes)
    }

    /// LDAT-selected initial camera path.
    #[must_use]
    pub const fn spawn_path(&self) -> RetailPathId {
        self.spawn_path
    }

    /// Returns one validated owned zone.
    #[must_use]
    pub fn zone(&self, eid: Eid) -> Option<&RetailZoneNode> {
        self.zones.get(&eid)
    }

    /// Returns one validated path by stable value handle.
    #[must_use]
    pub fn path(&self, id: RetailPathId) -> Option<&ZonePath> {
        let index = usize::try_from(id.index).ok()?;
        self.zone(id.zone)?.paths.get(index)
    }

    /// Resolves one retail path link through the source zone's neighbor table.
    pub fn resolve_neighbor(
        &self,
        source: RetailPathId,
        link_index: usize,
    ) -> Result<(RetailPathId, ZoneNeighborPath), FormatError> {
        let source_zone = self.zone(source.zone).ok_or_else(|| {
            FormatError::global(format!("zone graph has no ZDAT {}", source.zone))
        })?;
        let source_path = self.path(source).ok_or_else(|| {
            FormatError::global(format!(
                "zone graph has no path {}:{}",
                source.zone, source.index
            ))
        })?;
        let link = source_path
            .neighbors
            .get(link_index)
            .copied()
            .ok_or_else(|| {
                FormatError::global(format!(
                    "zone graph path {}:{} has no link {link_index}",
                    source.zone, source.index
                ))
            })?;
        let target_zone = source_zone
            .neighbors
            .get(usize::from(link.neighbor_zone_index))
            .copied()
            .ok_or_else(|| {
                FormatError::global(format!(
                    "zone graph path {}:{} link {link_index} has an invalid zone-neighbor index",
                    source.zone, source.index
                ))
            })?;
        let target = RetailPathId {
            zone: target_zone,
            index: u32::from(link.path_index),
        };
        if self.path(target).is_none() {
            return Err(FormatError::global(format!(
                "zone graph path {}:{} link {link_index} has an invalid target path",
                source.zone, source.index
            )));
        }
        Ok((target, link))
    }

    /// Number of reachable ZDAT zones.
    #[must_use]
    pub fn zone_count(&self) -> usize {
        self.zones.len()
    }

    /// Total number of camera paths in reachable zones.
    #[must_use]
    pub const fn path_count(&self) -> usize {
        self.path_count
    }

    /// Iterates reachable zones in deterministic EID order.
    #[must_use]
    pub fn zones(&self) -> impl ExactSizeIterator<Item = &RetailZoneNode> {
        self.zones.values()
    }
}

fn path_error(zone: Eid, path_index: usize, message: impl AsRef<str>) -> FormatError {
    FormatError::global(format!(
        "zone graph path {zone}:{path_index} {}",
        message.as_ref()
    ))
}

fn path_link_error(
    zone: Eid,
    path_index: usize,
    link_index: usize,
    message: impl AsRef<str>,
) -> FormatError {
    FormatError::global(format!(
        "zone graph path {zone}:{path_index} link {link_index} {}",
        message.as_ref()
    ))
}

#[cfg(test)]
mod tests {
    use crate::binary::EntryRef;
    use crate::stream::structs::ZonePathPoint;

    use super::*;

    fn point() -> ZonePathPoint {
        ZonePathPoint {
            x: 0,
            y: 0,
            z: 0,
            rotation_y: 0,
            rotation_x: 0,
            rotation_z: 0,
        }
    }

    fn path(neighbors: Vec<ZoneNeighborPath>) -> ZonePath {
        ZonePath {
            visibility_list: Eid::NONE,
            serialized_parent: EntryRef::from_raw(0),
            neighbors,
            entrance_index: 0,
            exit_index: 0,
            camera_mode: 1,
            average_node_distance: 1,
            camera_zoom: 0,
            unknown: [0; 3],
            direction: [0; 3],
            points: vec![point()],
        }
    }

    fn zone(eid: Eid, neighbors: Vec<Eid>, paths: Vec<ZonePath>) -> RetailZoneNode {
        RetailZoneNode::new(eid, [0; 3], 0, 0, neighbors, paths)
    }

    #[test]
    fn rejects_duplicate_zones_and_missing_spawn_handles() {
        let a = Eid::from_raw(1);
        let spawn = RetailPathId { zone: a, index: 0 };
        let duplicate = RetailZoneGraph::new(
            spawn,
            [
                zone(a, vec![], vec![path(vec![])]),
                zone(a, vec![], vec![path(vec![])]),
            ],
        )
        .unwrap_err();
        assert!(duplicate.message().contains("duplicate"));

        let missing_zone = RetailZoneGraph::new(spawn, []).unwrap_err();
        assert!(missing_zone.message().contains("spawn ZDAT"));

        let missing_path = RetailZoneGraph::new(
            RetailPathId { zone: a, index: 1 },
            [zone(a, vec![], vec![path(vec![])])],
        )
        .unwrap_err();
        assert!(missing_path.message().contains("spawn path"));
    }

    #[test]
    fn rejects_links_with_invalid_source_neighbor_indices() {
        let a = Eid::from_raw(1);
        let link = ZoneNeighborPath {
            relation: 0,
            neighbor_zone_index: 0,
            path_index: 0,
            goal: 1,
        };
        let error = RetailZoneGraph::new(
            RetailPathId { zone: a, index: 0 },
            [zone(a, vec![], vec![path(vec![link])])],
        )
        .unwrap_err();
        assert!(error.message().contains("zone-neighbor index"));
    }

    #[test]
    fn rejects_links_to_missing_zones_or_paths() {
        let a = Eid::from_raw(1);
        let b = Eid::from_raw(3);
        let link = ZoneNeighborPath {
            relation: 0,
            neighbor_zone_index: 0,
            path_index: 0,
            goal: 1,
        };
        let missing_zone = RetailZoneGraph::new(
            RetailPathId { zone: a, index: 0 },
            [zone(a, vec![b], vec![path(vec![link])])],
        )
        .unwrap_err();
        assert!(missing_zone.message().contains("target ZDAT"));

        let missing_path_link = ZoneNeighborPath {
            path_index: 1,
            ..link
        };
        let missing_path = RetailZoneGraph::new(
            RetailPathId { zone: a, index: 0 },
            [
                zone(a, vec![b], vec![path(vec![missing_path_link])]),
                zone(b, vec![], vec![path(vec![])]),
            ],
        )
        .unwrap_err();
        assert!(missing_path.message().contains("target path"));
    }

    #[test]
    fn resolves_valid_cross_zone_link_by_value_handle() {
        let a = Eid::from_raw(1);
        let b = Eid::from_raw(3);
        let link = ZoneNeighborPath {
            relation: 2,
            neighbor_zone_index: 0,
            path_index: 0,
            goal: 1,
        };
        let graph = RetailZoneGraph::new(
            RetailPathId { zone: a, index: 0 },
            [
                zone(a, vec![b], vec![path(vec![link])]),
                zone(b, vec![], vec![path(vec![])]),
            ],
        )
        .unwrap();
        assert_eq!(graph.zone_count(), 2);
        assert_eq!(graph.path_count(), 2);
        assert_eq!(
            graph
                .resolve_neighbor(RetailPathId { zone: a, index: 0 }, 0)
                .unwrap(),
            (RetailPathId { zone: b, index: 0 }, link)
        );
    }
}
