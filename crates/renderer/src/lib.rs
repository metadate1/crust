#![forbid(unsafe_code)]

//! Safe, renderer-neutral C1 command generation and texture decoding.
//!
//! The crate deliberately stops at immutable RGBA uploads and interleaved
//! triangle batches so the browser crate can own all WebGL2 state.

pub mod cache;
pub mod command;
pub mod projection;
pub mod texture;
pub mod timing;

pub use cache::{CachedTexture, TextureCache, TextureHandle, TextureRequest};
pub use command::{GeneratedFrame, OrderingTable, PrimitiveCommand};
pub use projection::{Matrix3, Viewport};
