#![forbid(unsafe_code)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "the VM and fixed-point formats intentionally preserve low-bit two's-complement semantics"
)]
#![allow(
    clippy::missing_errors_doc,
    reason = "fallible APIs return small, exhaustive subsystem error enums"
)]

//! Deterministic 30 Hz simulation, GOOL execution, collision, and game flow.

pub mod camera;
pub mod card;
pub mod collision;
pub mod demo;
pub mod flow;
pub mod gool;
pub mod math;
pub mod object_arena;
pub mod paging;
pub mod player;
pub mod retail_frame;
pub mod retail_runtime;
pub mod scheduler;

pub use math::{Angle12, Bounds3, Vec2, Vec3};
pub use scheduler::{FrameDecision, FrameScheduler, SIMULATION_HZ};
