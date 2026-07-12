//! Camera state and characterized map/death-camera helpers.

use crate::math::{Angle12, Angles, Vec3, seek};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CameraMode {
    Follow,
    Path,
    Fixed,
    Orbit,
    Death,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CameraState {
    pub translation: Vec3,
    pub rotation: Angles,
    pub mode: CameraMode,
    pub offset: Vec3,
    pub zoom: i32,
    pub death_acceleration: i32,
    pub death_orbit: i32,
    pub death_flip_velocity: i32,
}

impl Default for CameraState {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Angles::default(),
            mode: CameraMode::Follow,
            offset: Vec3 {
                x: 0,
                y: 0x3e800,
                z: -0x12c00,
            },
            zoom: 0x6a400,
            death_acceleration: 0,
            death_orbit: 0,
            death_flip_velocity: 0,
        }
    }
}

impl CameraState {
    /// Deterministically follows a target using per-axis seek limits.
    pub fn follow(&mut self, target: Vec3, speed: i32) {
        self.mode = CameraMode::Follow;
        let desired = target.wrapping_add(self.offset);
        self.translation.x = seek(self.translation.x, desired.x, speed);
        self.translation.y = seek(self.translation.y, desired.y, speed);
        self.translation.z = seek(self.translation.z, desired.z, speed);
    }

    /// Advances the characterized orbit acceleration used by death cameras.
    pub fn death_step(&mut self, focus: Vec3, flip_speed: i32, zoom_speed: i32, accelerate: bool) {
        self.mode = CameraMode::Death;
        self.death_acceleration = 22;
        self.death_flip_velocity = flip_speed;
        if accelerate {
            self.death_orbit = self.death_orbit.wrapping_add(self.death_acceleration);
            self.rotation.x = self.rotation.x.wrapping_add(self.death_acceleration);
        }
        self.translation.y = seek(self.translation.y, focus.y.saturating_add(120_000), 102_400);
        self.zoom = seek(self.zoom, 175_000, zoom_speed);
        let sin = i32::from(Angle12::new(self.death_orbit).sin_q12());
        let cos = i32::from(Angle12::new(self.death_orbit).cos_q12());
        self.translation.x = focus
            .x
            .wrapping_add(((i64::from(self.zoom) * i64::from(sin)) >> 12) as i32);
        self.translation.z = focus
            .z
            .wrapping_add(((i64::from(self.zoom) * i64::from(cos)) >> 12) as i32);
    }
}

/// One world-map path neighbor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MapNeighbor {
    pub goal: u8,
}

/// Retail preference: the last exact/direction-compatible match, then the first
/// goal without orbit bit 4, then no route.
#[must_use]
pub fn select_island_neighbor(neighbors: &[MapNeighbor], state: u8) -> Option<usize> {
    let mut selected = None;
    for (index, neighbor) in neighbors.iter().copied().enumerate() {
        if neighbor.goal == state
            || (selected.is_none() && (neighbors.len() == 1 || (neighbor.goal & 3) == (state & 3)))
        {
            selected = Some(index);
        }
    }
    selected.or_else(|| neighbors.iter().position(|neighbor| neighbor.goal & 4 == 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn island_neighbor_selection_matches_source_golden() {
        assert_eq!(
            select_island_neighbor(
                &[
                    MapNeighbor { goal: 6 },
                    MapNeighbor { goal: 1 },
                    MapNeighbor { goal: 5 }
                ],
                5,
            ),
            Some(2)
        );
        assert_eq!(
            select_island_neighbor(
                &[
                    MapNeighbor { goal: 6 },
                    MapNeighbor { goal: 2 },
                    MapNeighbor { goal: 7 }
                ],
                1,
            ),
            Some(1)
        );
        assert_eq!(
            select_island_neighbor(&[MapNeighbor { goal: 4 }, MapNeighbor { goal: 4 }], 2),
            None
        );
    }

    #[test]
    fn death_camera_uses_characterized_acceleration() {
        let mut camera = CameraState {
            translation: Vec3 {
                x: 4_000,
                y: 5_000,
                z: 6_000,
            },
            ..CameraState::default()
        };
        camera.death_step(
            Vec3 {
                x: 1_000,
                y: 2_000,
                z: 3_000,
            },
            100,
            1_000,
            true,
        );
        assert_eq!(camera.death_acceleration, 22);
        assert_eq!(camera.death_orbit, 22);
        assert_eq!(camera.death_flip_velocity, 100);
        assert!(camera.translation.x.unsigned_abs() < 10_000_000);
        assert!(camera.translation.y.unsigned_abs() < 10_000_000);
        assert!(camera.translation.z.unsigned_abs() < 10_000_000);
    }
}
