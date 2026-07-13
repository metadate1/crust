//! Bounded collision queries and deterministic movement subdivision.

use crate::math::{Bounds3, Vec3};

pub const MAX_QUERY_RESULTS: usize = 512;
pub const MAX_HORIZONTAL_STEP: i32 = 25_600;
pub const MAX_VERTICAL_STEP: i32 = 153_600;

/// A decoded octree leaf used by collision consumers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CollisionNode {
    pub level: u8,
    pub raw_node: u16,
    pub origin: Vec3,
}

impl CollisionNode {
    #[must_use]
    pub const fn kind(self) -> u8 {
        (((self.raw_node << 1) | 1) & 0x000e) as u8 >> 1
    }

    #[must_use]
    pub const fn subtype(self) -> u8 {
        ((((self.raw_node << 1) | 1) & 0x03f0) >> 4) as u8
    }
}

/// Header that applies to following octree nodes in a flattened query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueryHeader {
    pub cell_size: Vec3,
    pub max_depth: Vec3,
}

/// Safe flattened counterpart to the source query's overlayed records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryRecord {
    Header(QueryHeader),
    Node(CollisionNode),
    End,
}

/// Fixed-capacity collision query result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollisionQuery {
    records: Vec<QueryRecord>,
}

impl CollisionQuery {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            records: Vec::new(),
        }
    }

    pub fn push(&mut self, record: QueryRecord) -> Result<(), CollisionError> {
        if self.records.len() == MAX_QUERY_RESULTS {
            return Err(CollisionError::ResultCapacityExceeded);
        }
        self.records.push(record);
        Ok(())
    }

    #[must_use]
    pub fn records(&self) -> &[QueryRecord] {
        &self.records
    }
}

impl Default for CollisionQuery {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollisionError {
    ResultCapacityExceeded,
    MissingHeader,
    InvalidDepth,
}

fn node_bounds(
    node: CollisionNode,
    header: QueryHeader,
    base: Vec3,
) -> Result<Bounds3, CollisionError> {
    let depth = Vec3 {
        x: i32::from(node.level).min(header.max_depth.x),
        y: i32::from(node.level).min(header.max_depth.y),
        z: i32::from(node.level).min(header.max_depth.z),
    };
    if depth.x < 0 || depth.y < 0 || depth.z < 0 || depth.x >= 31 || depth.y >= 31 || depth.z >= 31
    {
        return Err(CollisionError::InvalidDepth);
    }
    let min = base.wrapping_add(node.origin.wrapping_scale(16));
    let max = Vec3 {
        x: min.x.wrapping_add(header.cell_size.x >> depth.x),
        y: min.y.wrapping_add(header.cell_size.y >> depth.y),
        z: min.z.wrapping_add(header.cell_size.z >> depth.z),
    };
    Bounds3::new(min, max).ok_or(CollisionError::InvalidDepth)
}

/// Averages the lower face of all overlapping nodes of either requested type.
pub fn find_ceiling_y(
    query: &CollisionQuery,
    nodes_origin: Vec3,
    collider: Bounds3,
    type_a: u8,
    type_b: u8,
    default_y: i32,
) -> Result<i32, CollisionError> {
    let mut header = None;
    let mut sum = 0_i64;
    let mut count = 0_i64;
    for record in query.records() {
        match *record {
            QueryRecord::Header(value) => header = Some(value),
            QueryRecord::End => break,
            QueryRecord::Node(node) => {
                let current = header.ok_or(CollisionError::MissingHeader)?;
                let kind = node.kind();
                if kind != type_a.saturating_sub(1) && kind != type_b.saturating_sub(1) {
                    continue;
                }
                let bounds = node_bounds(node, current, nodes_origin)?;
                if bounds.intersects(collider) {
                    sum += i64::from(bounds.min.y);
                    count += 1;
                }
            }
        }
    }
    if count == 0 {
        Ok(default_y)
    } else {
        Ok((sum / count) as i32)
    }
}

/// Splits a velocity into the exact number of source-sized collision steps.
#[must_use]
pub fn movement_steps(velocity: Vec3) -> Vec<Vec3> {
    let horizontal_x = velocity.x.unsigned_abs() / MAX_HORIZONTAL_STEP as u32;
    let vertical = velocity.y.unsigned_abs() / MAX_VERTICAL_STEP as u32;
    let horizontal_z = velocity.z.unsigned_abs() / MAX_HORIZONTAL_STEP as u32;
    let count = horizontal_x
        .max(vertical)
        .max(horizontal_z)
        .saturating_add(1);
    let count_i32 = i32::try_from(count).unwrap_or(i32::MAX);
    let maximum = Vec3 {
        x: velocity.x / count_i32,
        y: velocity.y / count_i32,
        z: velocity.z / count_i32,
    };
    let mut remaining = velocity;
    let mut output = Vec::with_capacity(count as usize);
    while remaining != Vec3::ZERO {
        let step = Vec3 {
            x: clamp_step(remaining.x, maximum.x),
            y: clamp_step(remaining.y, maximum.y),
            z: clamp_step(remaining.z, maximum.z),
        };
        if step == Vec3::ZERO {
            break;
        }
        output.push(step);
        remaining -= step;
    }
    output
}

const fn clamp_step(remaining: i32, maximum: i32) -> i32 {
    // Integer division can truncate a small component to zero when another
    // axis determines the split count. Consume that component in one step so
    // it is not silently discarded (or left behind by the zero-step guard).
    if maximum == 0 {
        return remaining;
    }
    if remaining.unsigned_abs() >= maximum.unsigned_abs() {
        maximum
    } else {
        remaining
    }
}

/// Applies subdivided movement through a caller-owned collision resolver.
pub fn move_with<F>(position: &mut Vec3, velocity: Vec3, mut resolve: F)
where
    F: FnMut(Vec3, Vec3) -> Vec3,
{
    for delta in movement_steps(velocity) {
        *position = resolve(*position, delta);
    }
}

/// Small deterministic wall mask equivalent to the source 32-word bitmap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WallBitmap {
    words: [u32; 32],
}

impl WallBitmap {
    #[must_use]
    pub const fn empty() -> Self {
        Self { words: [0; 32] }
    }

    #[must_use]
    pub const fn full() -> Self {
        Self {
            words: [u32::MAX; 32],
        }
    }

    pub fn set(&mut self, x: u8, z: u8, blocked: bool) {
        let x = usize::from(x.min(31));
        let z = u32::from(z.min(31));
        let mask = 1_u32 << z;
        if blocked {
            self.words[x] |= mask;
        } else {
            self.words[x] &= !mask;
        }
    }

    #[must_use]
    pub fn is_set(&self, x: u8, z: u8) -> bool {
        self.words[usize::from(x.min(31))] & (1_u32 << u32::from(z.min(31))) != 0
    }

    #[must_use]
    pub const fn words(&self) -> &[u32; 32] {
        &self.words
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn source_query(kind: u16, level: u8, node_z: i32) -> CollisionQuery {
        let mut query = CollisionQuery::new();
        query
            .push(QueryRecord::Header(QueryHeader {
                cell_size: Vec3 {
                    x: 256,
                    y: 256,
                    z: 256,
                },
                max_depth: Vec3 { x: 2, y: 2, z: 2 },
            }))
            .unwrap();
        query
            .push(QueryRecord::Node(CollisionNode {
                level,
                raw_node: kind | 8,
                origin: Vec3 {
                    x: 0,
                    y: 10,
                    z: node_z,
                },
            }))
            .unwrap();
        query.push(QueryRecord::End).unwrap();
        query
    }

    #[test]
    fn ceiling_accepts_either_type_and_partial_z_overlap() {
        let collider = Bounds3::new(
            Vec3 {
                x: 0,
                y: 100,
                z: 200,
            },
            Vec3 {
                x: 300,
                y: 500,
                z: 300,
            },
        )
        .unwrap();
        assert_eq!(
            find_ceiling_y(&source_query(0, 2, 10), Vec3::ZERO, collider, 2, 1, -999),
            Ok(160)
        );
        assert_eq!(
            find_ceiling_y(&source_query(1, 0, 0), Vec3::ZERO, collider, 2, 1, -999),
            Ok(160)
        );
        assert_eq!(
            find_ceiling_y(&source_query(2, 0, 0), Vec3::ZERO, collider, 2, 1, -999),
            Ok(-999)
        );
    }

    #[test]
    fn movement_uses_source_caps_and_preserves_total() {
        let velocity = Vec3 {
            x: 51_200,
            y: -307_200,
            z: 25_600,
        };
        let steps = movement_steps(velocity);
        // Integer division leaves a two-unit x remainder after the three
        // source-sized steps, so the original loop performs one final call.
        assert_eq!(steps.len(), 4);
        assert_eq!(
            steps.iter().copied().fold(Vec3::ZERO, Vec3::wrapping_add),
            velocity
        );
        assert!(
            steps
                .iter()
                .all(|step| step.x.unsigned_abs() <= MAX_HORIZONTAL_STEP as u32)
        );
        assert!(
            steps
                .iter()
                .all(|step| step.y.unsigned_abs() <= MAX_VERTICAL_STEP as u32)
        );

        let truncated_axis = Vec3 {
            x: 0,
            y: -1,
            z: -MAX_HORIZONTAL_STEP,
        };
        assert_eq!(
            movement_steps(truncated_axis)
                .into_iter()
                .fold(Vec3::ZERO, Vec3::wrapping_add),
            truncated_axis
        );
    }

    proptest! {
        #[test]
        fn split_steps_sum_to_velocity(x in -500_000_i32..500_000, y in -500_000_i32..500_000, z in -500_000_i32..500_000) {
            let velocity = Vec3 { x, y, z };
            let sum = movement_steps(velocity).into_iter().fold(Vec3::ZERO, Vec3::wrapping_add);
            prop_assert_eq!(sum, velocity);
        }
    }
}
