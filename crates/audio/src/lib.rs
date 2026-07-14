#![forbid(unsafe_code)]

//! PSX ADPCM decoding and deterministic software music/SFX mixing.

pub mod adpcm;
pub mod mixer;
pub mod output;
pub mod retail;
pub mod retail_music;
pub mod retail_player;
pub mod sequencer;
