//! Deterministic player state updated at one tick per simulation frame.

use crate::collision::move_with;
use crate::math::{Angle12, Angles, Vec3};

pub const PAD_SELECT: u32 = 0x0100;
pub const PAD_START: u32 = 0x0800;
pub const PAD_UP: u32 = 0x1000;
pub const PAD_RIGHT: u32 = 0x2000;
pub const PAD_DOWN: u32 = 0x4000;
pub const PAD_LEFT: u32 = 0x8000;
pub const PAD_TRIANGLE: u32 = 0x0010;
pub const PAD_CIRCLE: u32 = 0x0020;
pub const PAD_CROSS: u32 = 0x0040;
pub const PAD_SQUARE: u32 = 0x0080;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PadState {
    pub held: u32,
    pub tapped: u32,
}

impl PadState {
    #[must_use]
    pub const fn from_frames(previous: u32, held: u32) -> Self {
        let mut normalized = held;
        if normalized & PAD_UP != 0 {
            normalized &= !PAD_DOWN;
        }
        if normalized & PAD_LEFT != 0 {
            normalized &= !PAD_RIGHT;
        }
        Self {
            held: normalized,
            tapped: (!previous & normalized) & 0xf9ff,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlayerMode {
    Cutscene,
    Grounded,
    Airborne,
    Spinning,
    Dead,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerState {
    pub translation: Vec3,
    pub rotation: Angles,
    pub scale: Vec3,
    pub velocity: Vec3,
    pub mode: PlayerMode,
    pub lives: i32,
    pub health: u8,
    pub fruit: u16,
    pub boxes: u16,
    pub checkpoint: Option<u16>,
    pub spin_frames: u8,
    pub grounded: bool,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Angles::default(),
            scale: Vec3 {
                x: 0x1000,
                y: 0x1000,
                z: 0x1000,
            },
            velocity: Vec3::ZERO,
            mode: PlayerMode::Cutscene,
            lives: 4,
            health: 0,
            fruit: 0,
            boxes: 0,
            checkpoint: None,
            spin_frames: 0,
            grounded: false,
        }
    }
}

impl PlayerState {
    /// Updates input, gravity, jump/spin timers, and collision-resolved movement.
    pub fn tick<F>(&mut self, pad: PadState, mut resolve: F)
    where
        F: FnMut(Vec3, Vec3) -> Vec3,
    {
        const ACCELERATION: i32 = 4_000;
        const MAX_SPEED: i32 = 64_000;

        if self.mode == PlayerMode::Dead || self.mode == PlayerMode::Cutscene {
            return;
        }
        if pad.held & PAD_LEFT != 0 {
            self.velocity.x = self.velocity.x.saturating_sub(ACCELERATION).max(-MAX_SPEED);
            self.rotation.x = Angle12::new(0xc00);
        } else if pad.held & PAD_RIGHT != 0 {
            self.velocity.x = self.velocity.x.saturating_add(ACCELERATION).min(MAX_SPEED);
            self.rotation.x = Angle12::new(0x400);
        } else {
            self.velocity.x -= self.velocity.x / 4;
        }
        if pad.held & PAD_UP != 0 {
            self.velocity.z = self.velocity.z.saturating_sub(ACCELERATION).max(-MAX_SPEED);
            self.rotation.x = Angle12::new(0x800);
        } else if pad.held & PAD_DOWN != 0 {
            self.velocity.z = self.velocity.z.saturating_add(ACCELERATION).min(MAX_SPEED);
            self.rotation.x = Angle12::new(0);
        } else {
            self.velocity.z -= self.velocity.z / 4;
        }

        if pad.tapped & PAD_CROSS != 0 && self.grounded {
            self.velocity.y = 180_000;
            self.grounded = false;
            self.mode = PlayerMode::Airborne;
        }
        if pad.tapped & PAD_SQUARE != 0 {
            self.spin_frames = 18;
            self.mode = PlayerMode::Spinning;
        }
        if !self.grounded {
            self.velocity.y = self.velocity.y.saturating_sub(4_000).max(-0x002e_e000);
        }
        if self.spin_frames > 0 {
            self.spin_frames -= 1;
            if self.spin_frames == 0 {
                self.mode = if self.grounded {
                    PlayerMode::Grounded
                } else {
                    PlayerMode::Airborne
                };
            }
        }
        move_with(&mut self.translation, self.velocity, &mut resolve);
    }

    pub fn land(&mut self, floor_y: i32) {
        self.translation.y = floor_y;
        self.velocity.y = 0;
        self.grounded = true;
        if self.spin_frames == 0 {
            self.mode = PlayerMode::Grounded;
        }
    }

    pub fn die(&mut self) {
        self.mode = PlayerMode::Dead;
        self.lives = self.lives.saturating_sub(1);
        self.velocity = Vec3::ZERO;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opposite_directions_follow_retail_priority() {
        let state = PadState::from_frames(0, PAD_UP | PAD_DOWN | PAD_LEFT | PAD_RIGHT);
        assert_eq!(state.held & (PAD_UP | PAD_DOWN), PAD_UP);
        assert_eq!(state.held & (PAD_LEFT | PAD_RIGHT), PAD_LEFT);
    }

    #[test]
    fn jump_spin_and_gravity_are_deterministic() {
        let mut player = PlayerState {
            mode: PlayerMode::Grounded,
            grounded: true,
            ..PlayerState::default()
        };
        player.tick(
            PadState {
                held: PAD_CROSS | PAD_SQUARE,
                tapped: PAD_CROSS | PAD_SQUARE,
            },
            |position, delta| position + delta,
        );
        assert!(!player.grounded);
        assert_eq!(player.spin_frames, 17);
        assert_eq!(player.velocity.y, 176_000);
        assert_eq!(player.translation.y, 176_000);
    }
}
