//! Compatibility names for the retail controller bitfield.
//!
//! New platform-facing code should import these values from
//! `crust_platform::input`. This module intentionally contains no player state
//! or gameplay simulation; local simulation characterization tests retain the
//! historical path while they are migrated independently.

pub const PAD_START: u32 = 0x0800;
pub const PAD_UP: u32 = 0x1000;
pub const PAD_RIGHT: u32 = 0x2000;
pub const PAD_DOWN: u32 = 0x4000;
pub const PAD_LEFT: u32 = 0x8000;
pub const PAD_CROSS: u32 = 0x0040;
pub const PAD_SQUARE: u32 = 0x0080;
