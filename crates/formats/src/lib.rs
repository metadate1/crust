#![forbid(unsafe_code)]
// Binary readers intentionally expose many fallible leaf methods and unpack
// masked 32-bit fields into narrower components. Repeating identical Errors/
// Panics sections or checked conversions for values already masked to fit would
// obscure the actual format invariants documented at the module/type level.
#![allow(
    clippy::cast_possible_truncation,
    clippy::doc_markdown,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc
)]

//! Bounds-checked, endian-explicit readers for user-supplied C1 data.

pub mod binary;
pub mod disc;
pub mod stream;
