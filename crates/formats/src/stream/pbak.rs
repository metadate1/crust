//! Bounds-checked retail PBAK demo recordings.
//!
//! Legal NTSC-U item-zero payloads use two explicit native layouts. Eight
//! recordings contain 304 spawn words and begin frames at byte 1,324. The
//! Upstream recording contains 511 spawn words and begins frames at byte
//! 2,152. The extra words are part of that authored structure, not padding:
//! its draw stamp, bound, 34-tick cadence, and monotonic frames all occur at
//! the latter offsets. No legal entry contains the 3,592-word `level_spawns`
//! array found in the reconstructed PC header; `LevelSaveState` does not copy
//! that process-lifetime registry either.

use crate::binary::{Eid, FormatError, Reader};

use super::{Entry, LevelId};

/// NSF subsystem type used by retail PBAK entries.
pub const PBAK_ENTRY_TYPE: u32 = 19;
/// Spawn-word count in eight of the nine legal NTSC-U recordings.
pub const PBAK_SPAWN_WORD_COUNT: usize = 304;
/// Spawn-word count in the legal Upstream recording.
pub const PBAK_EXTENDED_SPAWN_WORD_COUNT: usize = 511;
/// Exact serialized size of the ordinary PBAK `level_state`.
pub const PBAK_LEVEL_STATE_LEN: usize = 1_276;
/// Exact serialized size of the 511-word PBAK `level_state`.
pub const PBAK_EXTENDED_LEVEL_STATE_LEN: usize = 2_104;
/// Exact ordinary offset of `pbak_header.frames` within item zero.
pub const PBAK_FRAMES_OFFSET: usize = 1_324;
/// Exact frame offset in the 511-word recording.
pub const PBAK_EXTENDED_FRAMES_OFFSET: usize = 2_152;
/// Exact size of one `pbak_frame`.
pub const PBAK_FRAME_LEN: usize = 8;

const PBAK_LEVEL_STATE_OFFSET: usize = 16;

/// The two exact PBAK layouts present on the legal NTSC-U disc.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PbakLayout {
    SpawnWords304,
    SpawnWords511,
}

impl PbakLayout {
    #[must_use]
    pub const fn spawn_word_count(self) -> usize {
        match self {
            Self::SpawnWords304 => PBAK_SPAWN_WORD_COUNT,
            Self::SpawnWords511 => PBAK_EXTENDED_SPAWN_WORD_COUNT,
        }
    }

    #[must_use]
    pub const fn level_state_len(self) -> usize {
        match self {
            Self::SpawnWords304 => PBAK_LEVEL_STATE_LEN,
            Self::SpawnWords511 => PBAK_EXTENDED_LEVEL_STATE_LEN,
        }
    }

    #[must_use]
    pub const fn frames_offset(self) -> usize {
        match self {
            Self::SpawnWords304 => PBAK_FRAMES_OFFSET,
            Self::SpawnWords511 => PBAK_EXTENDED_FRAMES_OFFSET,
        }
    }
}

/// Pointer-free copy of the native `level_state` stored in a demo.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PbakLevelState {
    pub player_translation: [i32; 3],
    /// Native field order is X, Y, Z. Runtime consumers translate it to the
    /// process register's documented Y/X/Z order explicitly.
    pub player_rotation: [i32; 3],
    pub player_scale: [i32; 3],
    pub zone: Eid,
    pub path_index: u32,
    pub progress: u32,
    pub level: LevelId,
    pub flag: i32,
    /// Exact authored width: 304 words normally and 511 for Upstream.
    pub spawn_words: Box<[u32]>,
    pub box_count: i32,
}

/// Native two-point 3D bound recorded for Crash.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PbakBound {
    pub min: [i32; 3],
    pub max: [i32; 3],
}

/// One recorded controller/time sample.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PbakFrame {
    pub ticks_elapsed: i32,
    pub held: u32,
}

/// One fully validated PBAK item-zero payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PbakHeader {
    pub layout: PbakLayout,
    pub seed: u32,
    pub unknown: i32,
    pub draw_count: i32,
    pub save_state: PbakLevelState,
    pub draw_stamp: u32,
    pub crash_bound: PbakBound,
    pub ticks_per_frame: i32,
    pub frames: Vec<PbakFrame>,
}

impl PbakHeader {
    /// Number of validated frames declared by the serialized header.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }
}

/// Parses one exact PBAK item-zero payload.
pub fn parse_pbak_header(bytes: &[u8]) -> Result<PbakHeader, FormatError> {
    let mut reader = Reader::new(bytes);
    let raw_frame_count = reader.i32_le()?;
    let frame_count = usize::try_from(raw_frame_count)
        .map_err(|_| FormatError::at(0, "PBAK frame count is negative"))?;
    let frames_len = frame_count
        .checked_mul(PBAK_FRAME_LEN)
        .ok_or_else(|| FormatError::at(0, "PBAK frame range overflows"))?;
    let ordinary_len = PBAK_FRAMES_OFFSET
        .checked_add(frames_len)
        .ok_or_else(|| FormatError::at(0, "ordinary PBAK length overflows"))?;
    let extended_len = PBAK_EXTENDED_FRAMES_OFFSET
        .checked_add(frames_len)
        .ok_or_else(|| FormatError::at(0, "extended PBAK length overflows"))?;
    let layout = if bytes.len() == ordinary_len {
        PbakLayout::SpawnWords304
    } else if bytes.len() == extended_len {
        PbakLayout::SpawnWords511
    } else {
        return Err(FormatError::at(
            0,
            format!(
                "PBAK declares {frame_count} frames; expected exact item length {ordinary_len} (304 spawn words) or {extended_len} (511 spawn words), found {}",
                bytes.len()
            ),
        ));
    };

    let seed = reader.u32_le()?;
    let unknown = reader.i32_le()?;
    let draw_count = reader.i32_le()?;
    debug_assert_eq!(reader.position(), PBAK_LEVEL_STATE_OFFSET);
    let save_state = parse_level_state(&mut reader, layout.spawn_word_count())?;
    debug_assert_eq!(
        reader.position(),
        PBAK_LEVEL_STATE_OFFSET + layout.level_state_len()
    );
    let draw_stamp = reader.u32_le()?;
    let crash_bound = PbakBound {
        min: read_i32_vec3(&mut reader)?,
        max: read_i32_vec3(&mut reader)?,
    };
    let ticks_offset = reader.position();
    let ticks_per_frame = reader.i32_le()?;
    if ticks_per_frame <= 0 {
        return Err(FormatError::at(
            ticks_offset,
            "PBAK ticks per frame must be positive",
        ));
    }
    debug_assert_eq!(reader.position(), layout.frames_offset());

    let mut frames = Vec::with_capacity(frame_count);
    for _ in 0..frame_count {
        frames.push(PbakFrame {
            ticks_elapsed: reader.i32_le()?,
            held: reader.u32_le()?,
        });
    }
    debug_assert_eq!(reader.position(), bytes.len());

    Ok(PbakHeader {
        layout,
        seed,
        unknown,
        draw_count,
        save_state,
        draw_stamp,
        crash_bound,
        ticks_per_frame,
        frames,
    })
}

/// Validates an NSF entry's type/item contract and parses its PBAK payload.
pub fn load_pbak_entry(entry: &Entry, nsf_bytes: &[u8]) -> Result<PbakHeader, FormatError> {
    if entry.entry_type != PBAK_ENTRY_TYPE {
        return Err(FormatError::global(format!(
            "entry {} has type {}; expected PBAK type {PBAK_ENTRY_TYPE}",
            entry.eid, entry.entry_type
        )));
    }
    let item = entry.item(0).ok_or_else(|| {
        FormatError::global(format!("PBAK entry {} is missing item zero", entry.eid))
    })?;
    parse_pbak_header(item.bytes(nsf_bytes)?)
}

fn parse_level_state(
    reader: &mut Reader<'_>,
    spawn_word_count: usize,
) -> Result<PbakLevelState, FormatError> {
    let player_translation = read_i32_vec3(reader)?;
    let player_rotation = read_i32_vec3(reader)?;
    let player_scale = read_i32_vec3(reader)?;
    let zone = Eid::from_raw(reader.u32_le()?);
    let path_offset = reader.position();
    let raw_path_index = reader.i32_le()?;
    let path_index = u32::try_from(raw_path_index)
        .map_err(|_| FormatError::at(path_offset, "PBAK save-state path index is negative"))?;
    let progress = reader.u32_le()?;
    let level_offset = reader.position();
    let raw_level = reader.i32_le()?;
    let raw_level = u32::try_from(raw_level)
        .map_err(|_| FormatError::at(level_offset, "PBAK save-state level is negative"))?;
    let level = LevelId::new(raw_level)
        .map_err(|error| FormatError::at(level_offset, error.message().to_owned()))?;
    let flag = reader.i32_le()?;

    let mut spawn_words = Vec::with_capacity(spawn_word_count);
    for _ in 0..spawn_word_count {
        spawn_words.push(reader.u32_le()?);
    }
    let box_count = reader.i32_le()?;

    Ok(PbakLevelState {
        player_translation,
        player_rotation,
        player_scale,
        zone,
        path_index,
        progress,
        level,
        flag,
        spawn_words: spawn_words.into_boxed_slice(),
        box_count,
    })
}

fn read_i32_vec3(reader: &mut Reader<'_>) -> Result<[i32; 3], FormatError> {
    Ok([reader.i32_le()?, reader.i32_le()?, reader.i32_le()?])
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn write_i32(bytes: &mut [u8], offset: usize, value: i32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn fixture(layout: PbakLayout, frame_count: usize) -> Vec<u8> {
        let mut bytes = vec![0_u8; layout.frames_offset() + frame_count * PBAK_FRAME_LEN];
        write_i32(&mut bytes, 0, i32::try_from(frame_count).unwrap());
        write_u32(&mut bytes, 4, 0x1234_5678);
        write_i32(&mut bytes, 8, -7);
        write_i32(&mut bytes, 12, 99);
        for (index, value) in [11, 22, 33].into_iter().enumerate() {
            write_i32(&mut bytes, 16 + index * 4, value);
        }
        for (index, value) in [44, 55, 66].into_iter().enumerate() {
            write_i32(&mut bytes, 28 + index * 4, value);
        }
        for (index, value) in [0x1000, 0x1100, 0x1200].into_iter().enumerate() {
            write_i32(&mut bytes, 40 + index * 4, value);
        }
        write_u32(&mut bytes, 52, Eid::from_name("zoneP").unwrap().raw());
        write_i32(&mut bytes, 56, 3);
        write_u32(&mut bytes, 60, 0x345);
        write_i32(&mut bytes, 64, 9);
        write_i32(&mut bytes, 68, 1);
        let spawn_words_offset = 72;
        write_u32(&mut bytes, spawn_words_offset, 0x3333_4444);
        write_u32(
            &mut bytes,
            spawn_words_offset + (layout.spawn_word_count() - 1) * 4,
            0x5555_6666,
        );
        let box_count_offset = spawn_words_offset + layout.spawn_word_count() * 4;
        write_i32(&mut bytes, box_count_offset, 0x700);
        let draw_stamp_offset = box_count_offset + 4;
        write_u32(&mut bytes, draw_stamp_offset, 0x89ab_cdef);
        for (index, value) in [-10, -20, -30, 40, 50, 60].into_iter().enumerate() {
            write_i32(&mut bytes, draw_stamp_offset + 4 + index * 4, value);
        }
        write_i32(&mut bytes, draw_stamp_offset + 28, 17);
        for index in 0..frame_count {
            let offset = layout.frames_offset() + index * PBAK_FRAME_LEN;
            write_i32(&mut bytes, offset, 1_000 + i32::try_from(index).unwrap());
            write_u32(&mut bytes, offset + 4, 1_u32 << index);
        }
        bytes
    }

    #[test]
    fn both_legal_native_layouts_and_frames_are_preserved() {
        assert_eq!(PBAK_LEVEL_STATE_LEN, 1_276);
        assert_eq!(PBAK_FRAMES_OFFSET, 1_324);
        assert_eq!(PBAK_EXTENDED_LEVEL_STATE_LEN, 2_104);
        assert_eq!(PBAK_EXTENDED_FRAMES_OFFSET, 2_152);

        for layout in [PbakLayout::SpawnWords304, PbakLayout::SpawnWords511] {
            let parsed = parse_pbak_header(&fixture(layout, 2)).unwrap();
            assert_eq!(parsed.layout, layout);
            assert_eq!(parsed.seed, 0x1234_5678);
            assert_eq!(parsed.unknown, -7);
            assert_eq!(parsed.draw_count, 99);
            assert_eq!(parsed.save_state.player_translation, [11, 22, 33]);
            assert_eq!(parsed.save_state.player_rotation, [44, 55, 66]);
            assert_eq!(parsed.save_state.player_scale, [0x1000, 0x1100, 0x1200]);
            assert_eq!(parsed.save_state.zone.name().as_deref(), Some("zoneP"));
            assert_eq!(parsed.save_state.path_index, 3);
            assert_eq!(parsed.save_state.progress, 0x345);
            assert_eq!(parsed.save_state.level, LevelId::N_SANITY_BEACH);
            assert_eq!(parsed.save_state.flag, 1);
            assert_eq!(
                parsed.save_state.spawn_words.len(),
                layout.spawn_word_count()
            );
            assert_eq!(parsed.save_state.spawn_words[0], 0x3333_4444);
            assert_eq!(
                parsed.save_state.spawn_words[layout.spawn_word_count() - 1],
                0x5555_6666
            );
            assert_eq!(parsed.save_state.box_count, 0x700);
            assert_eq!(parsed.draw_stamp, 0x89ab_cdef);
            assert_eq!(parsed.crash_bound.min, [-10, -20, -30]);
            assert_eq!(parsed.crash_bound.max, [40, 50, 60]);
            assert_eq!(parsed.ticks_per_frame, 17);
            assert_eq!(
                parsed.frames,
                [
                    PbakFrame {
                        ticks_elapsed: 1_000,
                        held: 1,
                    },
                    PbakFrame {
                        ticks_elapsed: 1_001,
                        held: 2,
                    },
                ]
            );
        }
    }

    #[test]
    fn zero_frames_is_a_valid_inactive_recording() {
        for layout in [PbakLayout::SpawnWords304, PbakLayout::SpawnWords511] {
            let parsed = parse_pbak_header(&fixture(layout, 0)).unwrap();
            assert_eq!(parsed.frame_count(), 0);
            assert!(parsed.frames.is_empty());
        }
    }

    #[test]
    fn negative_overstated_truncated_trailing_and_unknown_layouts_are_rejected() {
        let mut negative = fixture(PbakLayout::SpawnWords304, 0);
        write_i32(&mut negative, 0, -1);
        assert_eq!(parse_pbak_header(&negative).unwrap_err().offset(), Some(0));

        let mut overstated = fixture(PbakLayout::SpawnWords304, 1);
        write_i32(&mut overstated, 0, i32::MAX);
        assert!(parse_pbak_header(&overstated).is_err());

        let mut truncated = fixture(PbakLayout::SpawnWords304, 1);
        truncated.pop();
        assert!(parse_pbak_header(&truncated).is_err());

        let mut trailing = fixture(PbakLayout::SpawnWords511, 1);
        trailing.push(0);
        assert!(parse_pbak_header(&trailing).is_err());

        let unknown = vec![0_u8; 1_500];
        assert!(parse_pbak_header(&unknown).is_err());
    }

    #[test]
    fn signed_indices_and_nonpositive_timing_are_checked_before_runtime_use() {
        let mut negative_path = fixture(PbakLayout::SpawnWords304, 0);
        write_i32(&mut negative_path, 56, -1);
        assert_eq!(
            parse_pbak_header(&negative_path).unwrap_err().offset(),
            Some(56)
        );

        let mut negative_level = fixture(PbakLayout::SpawnWords304, 0);
        write_i32(&mut negative_level, 64, -1);
        assert_eq!(
            parse_pbak_header(&negative_level).unwrap_err().offset(),
            Some(64)
        );

        let mut zero_ticks = fixture(PbakLayout::SpawnWords304, 0);
        write_i32(&mut zero_ticks, PBAK_FRAMES_OFFSET - 4, 0);
        assert_eq!(
            parse_pbak_header(&zero_ticks).unwrap_err().offset(),
            Some(PBAK_FRAMES_OFFSET - 4)
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn arbitrary_pbak_bytes_never_panic(
            bytes in prop::collection::vec(any::<u8>(), 0..20_000),
        ) {
            let _ = parse_pbak_header(&bytes);
        }
    }
}
