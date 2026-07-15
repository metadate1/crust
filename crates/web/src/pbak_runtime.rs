//! Checked adapter from validated retail PBAK assets to browser runtime state.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use core::fmt;

use crust_formats::binary::{EID_ALPHABET, Eid};
use crust_formats::stream::{
    Nsd, Nsf, PBAK_SPAWN_WORD_COUNT, PbakHeader, PbakLayout, RetailPathId, RetailZoneGraph,
    load_pbak_entry,
};
use crust_sim::camera::{GAME_STATE_CUTSCENE, RetailCameraLocation, RetailCameraRuntime};
use crust_sim::demo::{Demo, DemoEnd, DemoError, DemoFrame, DemoPlayer, DemoStep};
use crust_sim::gool::{RetailPadSnapshot, retail_random};
use crust_sim::math::{Bounds3, Vec3};
use crust_sim::object_arena::SPAWN_TABLE_CAPACITY;
use crust_sim::retail_runtime::RetailLevelSnapshot;

/// One selected recording with every pair-owned reference resolved to values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreparedPbak {
    pub eid: Eid,
    pub layout: PbakLayout,
    pub snapshot: RetailLevelSnapshot,
    pub crash_bound: Bounds3,
    pub player: DemoPlayer,
    recorded_ticks_per_frame: Box<[i32]>,
    recorded_ticks_elapsed: Box<[u32]>,
}

impl PreparedPbak {
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.player.frame_count()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlaybackPhase {
    Armed,
    Playing,
    Ending(DemoEnd),
    Returning,
}

/// Process-lifetime PBAK ownership around one mounted browser level.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RetailPbakPlayback {
    prepared: PreparedPbak,
    phase: PlaybackPhase,
    frame_cursor: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PbakInputFrame {
    pub held: u32,
    pub ticks_per_frame: Option<i32>,
    pub end: Option<DemoEnd>,
}

/// Timing visible on either side of Crash's source `PadUpdatePbak` call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PbakFrameTiming {
    pub prior: PbakPublishedTiming,
    pub crash: PbakPublishedTiming,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PbakPublishedTiming {
    pub current: i32,
    pub period: i32,
}

/// Pad words visible to the synchronous completion event sent from inside
/// `PadUpdatePbak`. Native has shifted history and replaced `held`, but does
/// not calculate the current `tapped` word until the nested event returns.
pub(crate) fn pbak_event_pad_snapshot(
    previous: RetailPadSnapshot,
    mut updated: RetailPadSnapshot,
) -> RetailPadSnapshot {
    updated.tapped = previous.tapped;
    updated
}

impl RetailPbakPlayback {
    #[must_use]
    pub const fn new(prepared: PreparedPbak) -> Self {
        Self {
            prepared,
            phase: PlaybackPhase::Armed,
            frame_cursor: 0,
        }
    }

    #[must_use]
    pub const fn is_armed(&self) -> bool {
        matches!(self.phase, PlaybackPhase::Armed)
    }

    /// Whether native would next service this playback from Crash's
    /// `PadUpdatePbak` traversal boundary.
    #[must_use]
    pub const fn uses_crash_boundary(&self) -> bool {
        matches!(
            self.phase,
            PlaybackPhase::Playing | PlaybackPhase::Returning
        )
    }

    /// Recorded TPF Crash will consume at the next `PadUpdatePbak` call.
    /// Cursor zero is the header TPF installed by Crash itself; later values
    /// were published by the preceding native `GLUpdate`. Reading it does not
    /// move either the browser cursor or `DemoPlayer`.
    #[must_use]
    pub fn pending_recorded_ticks_per_frame(&self) -> Option<i32> {
        matches!(self.phase, PlaybackPhase::Playing)
            .then(|| {
                self.prepared
                    .recorded_ticks_per_frame
                    .get(self.frame_cursor)
                    .copied()
            })
            .flatten()
    }

    /// Absolute source clock installed by the preceding PBAK `GLUpdate` and
    /// visible to the current pre-camera shader update.
    ///
    /// Crash consumes frame `n` at `PadUpdatePbak` and advances native's
    /// `cur_pbak_frame` before that same frame's `GLUpdate`. The clock written
    /// there is consequently frame `n + 1`, which is also the next unconsumed
    /// `frame_cursor` here. Cursor zero has not passed a PBAK `GLUpdate` yet,
    /// while an ending/returning playback no longer satisfies its state-two
    /// clock gate.
    #[must_use]
    pub fn pre_shader_ticks_elapsed(&self) -> Option<u32> {
        if !matches!(self.phase, PlaybackPhase::Playing) || self.frame_cursor == 0 {
            return None;
        }
        self.prepared
            .recorded_ticks_elapsed
            .get(self.frame_cursor)
            .copied()
    }

    /// Source timing on either side of the next Crash pad boundary.
    ///
    /// `PbakStart` occurs after the preceding `GLUpdate`, so earlier roots on
    /// its first frame retain ordinary wall timing. Crash then installs only
    /// the header TPF. Later recorded frames have already passed `GLUpdate`
    /// with state two and therefore expose `(17, recorded TPF)` to every root.
    /// Returning state three exposes `(17, rounded wall TPF)` throughout.
    #[must_use]
    pub fn frame_timing(
        &self,
        wall_ticks_current_frame: i32,
        wall_ticks_per_frame: i32,
    ) -> Option<PbakFrameTiming> {
        match self.phase {
            PlaybackPhase::Playing => {
                let recorded = self.pending_recorded_ticks_per_frame()?;
                if self.frame_cursor == 0 {
                    Some(PbakFrameTiming {
                        prior: PbakPublishedTiming {
                            current: wall_ticks_current_frame,
                            period: wall_ticks_per_frame,
                        },
                        crash: PbakPublishedTiming {
                            current: wall_ticks_current_frame,
                            period: recorded,
                        },
                    })
                } else {
                    Some(PbakFrameTiming {
                        prior: PbakPublishedTiming {
                            current: 17,
                            period: recorded,
                        },
                        crash: PbakPublishedTiming {
                            current: 17,
                            period: recorded,
                        },
                    })
                }
            }
            PlaybackPhase::Returning => Some(PbakFrameTiming {
                prior: PbakPublishedTiming {
                    current: 17,
                    period: wall_ticks_per_frame,
                },
                crash: PbakPublishedTiming {
                    current: 17,
                    period: wall_ticks_per_frame,
                },
            }),
            PlaybackPhase::Armed | PlaybackPhase::Ending(_) => None,
        }
    }

    #[must_use]
    pub const fn is_returning(&self) -> bool {
        matches!(self.phase, PlaybackPhase::Returning)
    }

    #[must_use]
    pub fn start_payload(&self) -> (RetailLevelSnapshot, u32, Bounds3) {
        (
            self.prepared.snapshot.clone(),
            self.prepared.player.seed(),
            self.prepared.crash_bound,
        )
    }

    pub fn mark_started(&mut self) {
        debug_assert!(self.is_armed());
        debug_assert_eq!(self.frame_cursor, 0);
        self.phase = PlaybackPhase::Playing;
    }

    /// Returns the pad override for this source `PadUpdatePbak` boundary.
    pub fn advance_input(&mut self, physical_held: u16) -> PbakInputFrame {
        if !matches!(self.phase, PlaybackPhase::Playing) {
            return PbakInputFrame {
                held: 0,
                ticks_per_frame: None,
                end: None,
            };
        }
        let pending_ticks_per_frame = self.pending_recorded_ticks_per_frame();
        match self.prepared.player.advance(u32::from(physical_held)) {
            DemoStep::Playing {
                held,
                ticks_per_frame,
                end,
                ..
            } => {
                debug_assert_eq!(pending_ticks_per_frame, Some(ticks_per_frame));
                self.frame_cursor = self.frame_cursor.saturating_add(1);
                if let Some(reason) = end {
                    self.phase = PlaybackPhase::Ending(reason);
                }
                PbakInputFrame {
                    held,
                    ticks_per_frame: Some(ticks_per_frame),
                    end,
                }
            }
            DemoStep::Finished => {
                self.phase = PlaybackPhase::Ending(DemoEnd::Finished);
                PbakInputFrame {
                    held: 0,
                    ticks_per_frame: None,
                    end: Some(DemoEnd::Finished),
                }
            }
        }
    }

    /// Advances and consumes an ending recording at one `PadUpdatePbak`
    /// boundary.
    ///
    /// Native reaches this from Crash's `GoolObjectUpdate`, after earlier
    /// roots (including the caption controller) and before Crash or later
    /// roots run. Returning the end reason alongside the recorded input lets
    /// the browser keep the synchronous event and state latch at that hook.
    pub fn advance_pad_boundary(
        &mut self,
        physical_held: u16,
    ) -> (PbakInputFrame, Option<DemoEnd>) {
        let input = self.advance_input(physical_held);
        let end = input.end.and_then(|_| self.take_end());
        (input, end)
    }

    fn take_end(&mut self) -> Option<DemoEnd> {
        let PlaybackPhase::Ending(reason) = self.phase else {
            return None;
        };
        self.phase = PlaybackPhase::Returning;
        Some(reason)
    }

    #[must_use]
    pub const fn eid(&self) -> Eid {
        self.prepared.eid
    }

    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.prepared.frame_count()
    }

    #[must_use]
    pub const fn layout(&self) -> PbakLayout {
        self.prepared.layout
    }
}

/// A checked incompatibility at the parser/runtime handoff.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PbakRuntimeError {
    EntryCountOverflow(usize),
    MissingSelectedEntry { eid: Eid, index: u32, count: usize },
    InvalidLevelAlphabet(u32),
    Format(String),
    EmptyRecording,
    LevelMismatch { expected: u32, recorded: u32 },
    MissingPath { zone: Eid, index: u32 },
    SpawnWordCount(usize),
    NonzeroExtendedTail { index: usize, value: u32 },
    Demo(DemoError),
}

impl fmt::Display for PbakRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EntryCountOverflow(count) => write!(
                formatter,
                "stream contains {count} PBAK entries, beyond retail's 32-bit count"
            ),
            Self::MissingSelectedEntry { eid, index, count } => write!(
                formatter,
                "retail PBAK choice {index} of {count} names absent entry {eid}"
            ),
            Self::InvalidLevelAlphabet(level) => write!(
                formatter,
                "retail PBAK level {level:#x} is outside the 64-character EID alphabet"
            ),
            Self::Format(error) => formatter.write_str(error),
            Self::EmptyRecording => formatter.write_str("PBAK recording has no pad frames"),
            Self::LevelMismatch { expected, recorded } => write!(
                formatter,
                "PBAK snapshot targets level {recorded:#x}, not mounted level {expected:#x}"
            ),
            Self::MissingPath { zone, index } => {
                write!(formatter, "PBAK snapshot path {zone}:{index} is absent")
            }
            Self::SpawnWordCount(count) => write!(
                formatter,
                "PBAK snapshot contains {count} spawn words; expected 304 or the legal 511-word layout"
            ),
            Self::NonzeroExtendedTail { index, value } => write!(
                formatter,
                "PBAK extended spawn tail word {index} is {value:#x}; refusing to discard it"
            ),
            Self::Demo(error) => write!(formatter, "PBAK playback metadata is invalid: {error:?}"),
        }
    }
}

/// Runs native `PbakChoose` against the mounted pair and prepares its selected
/// recording. Every nonempty choice consumes the caller's process-global
/// RNG-B stream, including the retail corpus's one-entry levels.
pub(crate) fn prepare_pair_pbak(
    metadata: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    graph: &RetailZoneGraph,
    random_seed_b: &mut u32,
) -> Result<Option<PreparedPbak>, PbakRuntimeError> {
    // NSCountEntries examines still-unrelocated NSD PTE names through
    // NSEIDType, where a trailing `B` denotes type 19. Do not substitute an
    // NSF iteration: malformed metadata must affect the choice exactly where
    // it did natively, then fail safely when the selected entry is resolved.
    let entry_count = pbak_entry_count(metadata);
    if entry_count == 0 {
        return Ok(None);
    }
    let count = u32::try_from(entry_count)
        .map_err(|_| PbakRuntimeError::EntryCountOverflow(entry_count))?;
    let selected_index = retail_random(count, random_seed_b);
    let selected_eid = pbak_choice_eid(selected_index, metadata.level())?;
    let entry = metadata
        .pte(selected_eid)
        .and_then(|_| nsf.resolve_entry(metadata, selected_eid).ok())
        .ok_or(PbakRuntimeError::MissingSelectedEntry {
            eid: selected_eid,
            index: selected_index,
            count: usize::try_from(count).expect("u32 count fits usize"),
        })?;
    let header = load_pbak_entry(entry, nsf_bytes)
        .map_err(|error| PbakRuntimeError::Format(error.to_string()))?;
    let path = RetailPathId {
        zone: header.save_state.zone,
        index: header.save_state.path_index,
    };
    let camera = RetailCameraRuntime::at_path(
        graph,
        path,
        header.save_state.progress.cast_signed(),
        GAME_STATE_CUTSCENE,
    )
    .map_err(|_| PbakRuntimeError::MissingPath {
        zone: path.zone,
        index: path.index,
    })?;
    prepare_header(entry.eid, &header, metadata.level(), camera.location()).map(Some)
}

fn pbak_entry_count(metadata: &Nsd) -> usize {
    metadata
        .page_table
        .iter()
        .filter(|pte| pte.eid.name_bytes().is_some_and(|name| name[4] == b'B'))
        .count()
}

/// Exact five-byte name built by source `PbakChoose`: `pb` + (`'0'` +
/// choice) + level alphabet character + `B`. `NSStringToEID` contributes a
/// zero digit for a byte outside its alphabet; retaining that quirk lets a
/// malformed multi-entry stream fail as a checked missing-entry error instead
/// of silently selecting a different recording.
fn pbak_choice_eid(
    index: u32,
    level: crust_formats::stream::LevelId,
) -> Result<Eid, PbakRuntimeError> {
    let mut name = *b"pb00B";
    name[2] = b'0'.wrapping_add(index.to_le_bytes()[0]);
    name[3] = EID_ALPHABET
        .get(level.get() as usize)
        .copied()
        .ok_or(PbakRuntimeError::InvalidLevelAlphabet(level.get()))?;
    let mut value = 0_u32;
    for byte in name {
        value <<= 6;
        if let Some(digit) = EID_ALPHABET.iter().position(|candidate| *candidate == byte) {
            value |= u32::try_from(digit).expect("retail alphabet has 64 entries");
        }
    }
    Ok(Eid::from_raw((value << 1) | 1))
}

fn prepare_header(
    eid: Eid,
    header: &PbakHeader,
    expected_level: crust_formats::stream::LevelId,
    location: RetailCameraLocation,
) -> Result<PreparedPbak, PbakRuntimeError> {
    if header.save_state.level != expected_level {
        return Err(PbakRuntimeError::LevelMismatch {
            expected: expected_level.get(),
            recorded: header.save_state.level.get(),
        });
    }
    if header.frames.is_empty() {
        return Err(PbakRuntimeError::EmptyRecording);
    }
    let spawn_words = fixed_spawn_words(&header.save_state.spawn_words)?;
    let frames = header
        .frames
        .iter()
        .map(|frame| DemoFrame {
            ticks_elapsed: frame.ticks_elapsed,
            held: frame.held,
        })
        .collect::<Vec<_>>();
    let demo = Demo::new(
        header.seed,
        header.draw_stamp,
        header.ticks_per_frame,
        frames,
    )
    .map_err(PbakRuntimeError::Demo)?;
    let recorded_ticks_per_frame = recorded_frame_timings(header);
    let recorded_ticks_elapsed = header
        .frames
        .iter()
        .map(|frame| frame.ticks_elapsed.cast_unsigned())
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let snapshot = RetailLevelSnapshot {
        player_translation: header.save_state.player_translation,
        // Serialized PBAK order is X/Y/Z. The process register block is the
        // engine's documented Y/X/Z rotation order.
        player_rotation_yxz: [
            header.save_state.player_rotation[1],
            header.save_state.player_rotation[0],
            header.save_state.player_rotation[2],
        ],
        player_scale: header.save_state.player_scale,
        location,
        level: header.save_state.level,
        death_resets_counter: header.save_state.flag != 0,
        spawn_words,
        box_count: header.save_state.box_count,
    };
    let crash_bound = Bounds3 {
        min: Vec3 {
            x: header.crash_bound.min[0],
            y: header.crash_bound.min[1],
            z: header.crash_bound.min[2],
        },
        max: Vec3 {
            x: header.crash_bound.max[0],
            y: header.crash_bound.max[1],
            z: header.crash_bound.max[2],
        },
    };
    Ok(PreparedPbak {
        eid,
        layout: header.layout,
        snapshot,
        crash_bound,
        player: DemoPlayer::new(demo),
        recorded_ticks_per_frame,
        recorded_ticks_elapsed,
    })
}

fn recorded_frame_timings(header: &PbakHeader) -> Box<[i32]> {
    header
        .frames
        .iter()
        .enumerate()
        .map(|(index, frame)| {
            if index == 0 {
                return header.ticks_per_frame;
            }
            let previous_stamp = if index == 1 {
                header.draw_stamp.cast_signed()
            } else {
                header.frames[index - 1].ticks_elapsed
            };
            round_pbak_ticks(frame.ticks_elapsed.wrapping_sub(previous_stamp))
        })
        .collect()
}

const fn round_pbak_ticks(ticks: i32) -> i32 {
    match ticks {
        0..=18 => 17,
        ..0 | 19..=35 => 34,
        36..=52 => 51,
        _ => ticks,
    }
}

fn fixed_spawn_words(words: &[u32]) -> Result<[u32; SPAWN_TABLE_CAPACITY], PbakRuntimeError> {
    if words.len() != PBAK_SPAWN_WORD_COUNT
        && words.len() != PbakLayout::SpawnWords511.spawn_word_count()
    {
        return Err(PbakRuntimeError::SpawnWordCount(words.len()));
    }
    if let Some((offset, value)) = words[PBAK_SPAWN_WORD_COUNT..]
        .iter()
        .copied()
        .enumerate()
        .find(|(_, value)| *value != 0)
    {
        return Err(PbakRuntimeError::NonzeroExtendedTail {
            index: PBAK_SPAWN_WORD_COUNT + offset,
            value,
        });
    }
    let mut fixed = [0_u32; SPAWN_TABLE_CAPACITY];
    fixed.copy_from_slice(&words[..PBAK_SPAWN_WORD_COUNT]);
    Ok(fixed)
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU16;
    use std::path::PathBuf;

    use crust_formats::stream::{
        KNOWN_LEVELS, LevelId, PbakBound, PbakFrame, PbakLayout, PbakLevelState, StreamKind,
        StreamName, parse_nsd, parse_nsf,
    };
    use crust_sim::demo::{DemoEnd, DemoStep};
    use crust_sim::retail_frame::PathProgress;

    use super::*;

    fn header(layout: PbakLayout) -> PbakHeader {
        let mut spawn_words = vec![0_u32; layout.spawn_word_count()];
        spawn_words[11] = 9;
        PbakHeader {
            layout,
            seed: 0x1234_5678,
            unknown: -1,
            draw_count: 77,
            save_state: PbakLevelState {
                player_translation: [1, 2, 3],
                player_rotation: [4, 5, 6],
                player_scale: [7, 8, 9],
                zone: Eid::from_name("zoneP").unwrap(),
                path_index: 2,
                progress: 0x180,
                level: LevelId::N_SANITY_BEACH,
                flag: 1,
                spawn_words: spawn_words.into_boxed_slice(),
                box_count: 0x700,
            },
            draw_stamp: 100,
            crash_bound: PbakBound {
                min: [-10, -20, -30],
                max: [40, 50, 60],
            },
            ticks_per_frame: 34,
            frames: vec![PbakFrame {
                ticks_elapsed: 134,
                held: 0x1000,
            }],
        }
    }

    fn location() -> RetailCameraLocation {
        RetailCameraLocation {
            path: RetailPathId {
                zone: Eid::from_name("zoneP").unwrap(),
                index: 2,
            },
            progress: PathProgress::clamped(0x180, NonZeroU16::new(5).unwrap()),
        }
    }

    const fn timing(
        before_current: i32,
        before_per_frame: i32,
        at_current: i32,
        at_per_frame: i32,
    ) -> PbakFrameTiming {
        PbakFrameTiming {
            prior: PbakPublishedTiming {
                current: before_current,
                period: before_per_frame,
            },
            crash: PbakPublishedTiming {
                current: at_current,
                period: at_per_frame,
            },
        }
    }

    #[test]
    fn pbak_choose_uses_source_name_and_advances_single_entry_rng_b() {
        let level = LevelId::new_const(0x0a);
        assert_eq!(
            pbak_choice_eid(0, level).unwrap(),
            Eid::from_name("pb0aB").unwrap()
        );
        assert_eq!(
            pbak_choice_eid(1, level).unwrap(),
            Eid::from_name("pb1aB").unwrap()
        );
        // `':'` is outside alpha_map, so NSStringToEID contributes digit zero.
        assert_eq!(
            pbak_choice_eid(10, level).unwrap(),
            pbak_choice_eid(0, level).unwrap()
        );
        assert_eq!(
            pbak_choice_eid(0, LevelId::new_const(64)),
            Err(PbakRuntimeError::InvalidLevelAlphabet(64))
        );

        let mut seed_b = 0;
        assert_eq!(retail_random(1, &mut seed_b), 0);
        assert_eq!(seed_b, 12_345);
        assert_eq!(retail_random(1, &mut seed_b), 0);
        assert_eq!(seed_b, 0xd3dc_167e);

        let mut many_seed = 0;
        assert_eq!(retail_random(9, &mut many_seed), 4);
    }

    #[test]
    fn ordinary_header_maps_rotation_bounds_snapshot_and_pad() {
        let eid = Eid::from_name("pb0aB").unwrap();
        let mut prepared = prepare_header(
            eid,
            &header(PbakLayout::SpawnWords304),
            LevelId::N_SANITY_BEACH,
            location(),
        )
        .unwrap();

        assert_eq!(prepared.eid, eid);
        assert_eq!(prepared.snapshot.player_translation, [1, 2, 3]);
        assert_eq!(prepared.snapshot.player_rotation_yxz, [5, 4, 6]);
        assert_eq!(prepared.snapshot.spawn_words[11], 9);
        assert!(prepared.snapshot.death_resets_counter);
        assert_eq!(
            prepared.crash_bound.min,
            Vec3 {
                x: -10,
                y: -20,
                z: -30,
            }
        );
        assert_eq!(
            prepared.crash_bound.max,
            Vec3 {
                x: 40,
                y: 50,
                z: 60,
            }
        );
        assert_eq!(
            prepared.player.advance(0),
            DemoStep::Playing {
                held: 0x1000,
                tapped: 0x1000,
                ticks_elapsed: 134,
                ticks_per_frame: 34,
                first_frame: true,
                end: Some(DemoEnd::Finished),
            }
        );
    }

    #[test]
    fn legal_extended_zero_tail_maps_the_proven_304_word_prefix() {
        let mut header = header(PbakLayout::SpawnWords511);
        header.save_state.spawn_words[303] = 0x8000_0008;
        let prepared = prepare_header(
            Eid::from_name("pb0fB").unwrap(),
            &header,
            LevelId::N_SANITY_BEACH,
            location(),
        )
        .unwrap();

        assert_eq!(prepared.snapshot.spawn_words[303], 0x8000_0008);
    }

    #[test]
    fn extended_nonzero_tail_is_never_silently_truncated() {
        let mut header = header(PbakLayout::SpawnWords511);
        header.save_state.spawn_words[304] = 7;
        assert_eq!(
            prepare_header(
                Eid::from_name("pb0fB").unwrap(),
                &header,
                LevelId::N_SANITY_BEACH,
                location(),
            ),
            Err(PbakRuntimeError::NonzeroExtendedTail {
                index: 304,
                value: 7,
            })
        );
    }

    #[test]
    fn mismatched_level_is_checked_and_full_pad_words_are_preserved() {
        let upstream = LevelId::new_const(0x0f);
        let mismatched = prepare_header(
            Eid::from_name("pb0aB").unwrap(),
            &header(PbakLayout::SpawnWords304),
            upstream,
            location(),
        );
        assert_eq!(
            mismatched,
            Err(PbakRuntimeError::LevelMismatch {
                expected: upstream.get(),
                recorded: LevelId::N_SANITY_BEACH.get(),
            })
        );

        let mut wide = header(PbakLayout::SpawnWords304);
        wide.frames[0].held = 0x0010_0040;
        let mut prepared = prepare_header(
            Eid::from_name("pb0aB").unwrap(),
            &wide,
            LevelId::N_SANITY_BEACH,
            location(),
        )
        .unwrap();
        assert_eq!(
            prepared.player.advance(0),
            DemoStep::Playing {
                held: 0x0010_0040,
                tapped: 0x0040,
                ticks_elapsed: 134,
                ticks_per_frame: 34,
                first_frame: true,
                end: Some(DemoEnd::Finished),
            }
        );
    }

    #[test]
    fn playback_locks_armed_and_returning_input_and_preserves_interrupt_frame() {
        let prepared = prepare_header(
            Eid::from_name("pb0aB").unwrap(),
            &header(PbakLayout::SpawnWords304),
            LevelId::N_SANITY_BEACH,
            location(),
        )
        .unwrap();
        let mut playback = RetailPbakPlayback::new(prepared);

        assert!(playback.is_armed());
        assert!(!playback.uses_crash_boundary());
        assert_eq!(playback.pending_recorded_ticks_per_frame(), None);
        assert_eq!(playback.pre_shader_ticks_elapsed(), None);
        assert_eq!(playback.frame_timing(40, 51), None);
        assert_eq!(
            playback.advance_input(0x1000),
            PbakInputFrame {
                held: 0,
                ticks_per_frame: None,
                end: None,
            }
        );
        let (_, seed, _) = playback.start_payload();
        assert_eq!(seed, 0x1234_5678);
        playback.mark_started();
        assert!(playback.uses_crash_boundary());
        assert_eq!(playback.pending_recorded_ticks_per_frame(), Some(34));
        assert_eq!(playback.pre_shader_ticks_elapsed(), None);
        assert_eq!(playback.frame_timing(40, 51), Some(timing(40, 51, 40, 34)));
        assert_eq!(
            playback.advance_pad_boundary(0x0800),
            (
                PbakInputFrame {
                    held: 0x1000,
                    ticks_per_frame: Some(34),
                    end: Some(DemoEnd::Interrupted),
                },
                Some(DemoEnd::Interrupted),
            )
        );
        assert!(playback.is_returning());
        assert!(playback.uses_crash_boundary());
        assert_eq!(playback.pending_recorded_ticks_per_frame(), None);
        assert_eq!(playback.pre_shader_ticks_elapsed(), None);
        assert_eq!(playback.frame_timing(40, 51), Some(timing(17, 51, 17, 51)));
        assert_eq!(playback.take_end(), None);
        assert_eq!(
            playback.advance_input(0xffff),
            PbakInputFrame {
                held: 0,
                ticks_per_frame: None,
                end: None,
            }
        );
    }

    #[test]
    fn start_frame_switches_tpf_then_gl_prepublishes_the_next_frames_clock() {
        let mut timeline = header(PbakLayout::SpawnWords304);
        timeline.draw_stamp = 100;
        timeline.ticks_per_frame = 34;
        timeline.frames = vec![
            PbakFrame {
                ticks_elapsed: 120,
                held: 1,
            },
            PbakFrame {
                ticks_elapsed: 134,
                held: 2,
            },
            PbakFrame {
                ticks_elapsed: 185,
                held: 3,
            },
        ];
        let prepared = prepare_header(
            Eid::from_name("pb0aB").unwrap(),
            &timeline,
            LevelId::N_SANITY_BEACH,
            location(),
        )
        .unwrap();
        let mut playback = RetailPbakPlayback::new(prepared);
        playback.mark_started();

        for (index, expected) in [34, 34, 51].into_iter().enumerate() {
            assert_eq!(
                playback.pre_shader_ticks_elapsed(),
                match index {
                    0 => None,
                    1 => Some(134),
                    2 => Some(185),
                    _ => unreachable!(),
                }
            );
            let pending = playback.pending_recorded_ticks_per_frame();
            assert_eq!(pending, Some(expected));
            assert_eq!(
                playback.pending_recorded_ticks_per_frame(),
                pending,
                "reading prepublished timing must not consume the recorded frame"
            );
            assert_eq!(
                playback.frame_timing(40, 17),
                Some(if index == 0 {
                    timing(40, 17, 40, expected)
                } else {
                    timing(17, expected, 17, expected)
                })
            );
            let (input, end) = playback.advance_pad_boundary(0);
            assert_eq!(input.ticks_per_frame, Some(expected));
            assert_eq!(end.is_some(), index == 2);
            assert_eq!(
                playback.pre_shader_ticks_elapsed(),
                match index {
                    0 => Some(134),
                    1 => Some(185),
                    2 => None,
                    _ => unreachable!(),
                },
                "PadUpdate advances the source cursor before the same-frame GLUpdate"
            );
        }

        assert!(playback.is_returning());
        assert_eq!(playback.pending_recorded_ticks_per_frame(), None);
        assert_eq!(playback.pre_shader_ticks_elapsed(), None);
        assert_eq!(playback.frame_timing(40, 34), Some(timing(17, 34, 17, 34)));
    }

    #[test]
    fn interrupted_pad_frame_advances_the_cursor_but_suppresses_gl_clock_publish() {
        let mut timeline = header(PbakLayout::SpawnWords304);
        timeline.frames = vec![
            PbakFrame {
                ticks_elapsed: 120,
                held: 1,
            },
            PbakFrame {
                ticks_elapsed: 134,
                held: 2,
            },
        ];
        let prepared = prepare_header(
            Eid::from_name("pb0aB").unwrap(),
            &timeline,
            LevelId::N_SANITY_BEACH,
            location(),
        )
        .unwrap();
        let mut playback = RetailPbakPlayback::new(prepared);
        playback.mark_started();

        assert_eq!(playback.pre_shader_ticks_elapsed(), None);
        let (input, end) = playback.advance_pad_boundary(0x0800);
        assert_eq!(input.held, 1);
        assert_eq!(end, Some(DemoEnd::Interrupted));
        assert_eq!(playback.frame_cursor, 1);
        assert!(playback.is_returning());
        assert_eq!(
            playback.pre_shader_ticks_elapsed(),
            None,
            "native changes PBAK state before GLUpdate, so frame one is not published"
        );
    }

    #[test]
    fn completion_event_sees_new_held_and_previous_tapped_word() {
        let previous = RetailPadSnapshot {
            held: 0x1000,
            tapped: 0x1000,
            held_previous: 0x0040,
            held_previous_2: 0x0080,
            tapped_previous: 0x0040,
        };
        let updated = RetailPadSnapshot {
            held: 0x2000,
            tapped: 0x2000,
            held_previous: 0x1000,
            held_previous_2: 0x0040,
            tapped_previous: 0x1000,
        };

        assert_eq!(
            pbak_event_pad_snapshot(previous, updated),
            RetailPadSnapshot {
                held: 0x2000,
                tapped: 0x1000,
                held_previous: 0x1000,
                held_previous_2: 0x0040,
                tapped_previous: 0x1000,
            }
        );
    }

    #[test]
    #[ignore = "set C1_STREAM_DIR to legally local extracted NTSC-U streams"]
    fn prepares_every_legally_local_recording_without_copying_game_data() {
        let root = PathBuf::from(
            std::env::var_os("C1_STREAM_DIR")
                .expect("C1_STREAM_DIR must name legally local extracted streams"),
        );
        let mut recordings = 0_usize;
        let mut frames = 0_usize;
        let mut random_seed_b = 0_u32;
        for known in KNOWN_LEVELS {
            let nsd_path = root.join(StreamName::new(known.id, StreamKind::Nsd).filename());
            let nsf_path = root.join(StreamName::new(known.id, StreamKind::Nsf).filename());
            let nsd_bytes = std::fs::read(&nsd_path)
                .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
            let nsf_bytes = std::fs::read(&nsf_path)
                .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));
            let metadata = parse_nsd(&nsd_bytes, known.id).unwrap();
            let nsf = parse_nsf(&nsf_bytes, &metadata).unwrap();
            let count = pbak_entry_count(&metadata);
            if !metadata.is_bootable() {
                assert_eq!(count, 0, "{} index-only PBAK count", known.id);
                continue;
            }
            let graph = RetailZoneGraph::from_pair(&metadata, &nsf, &nsf_bytes).unwrap();
            let seed_before = random_seed_b;
            let prepared =
                prepare_pair_pbak(&metadata, &nsf, &nsf_bytes, &graph, &mut random_seed_b).unwrap();
            if count == 0 {
                assert!(prepared.is_none(), "{} zero-entry choice", known.id);
                assert_eq!(random_seed_b, seed_before, "{} zero-entry seed", known.id);
                continue;
            }
            assert_eq!(count, 1, "{} PbakChoose entry count", known.id);
            let prepared = prepared.expect("counted PBAK entry must prepare");
            let mut expected_seed = seed_before;
            assert_eq!(retail_random(1, &mut expected_seed), 0);
            assert_eq!(random_seed_b, expected_seed);
            assert_eq!(
                prepared.eid,
                pbak_choice_eid(0, known.id).unwrap(),
                "{} selected PBAK EID",
                known.id
            );
            assert_eq!(prepared.snapshot.level, known.id);
            let frame_count = prepared.frame_count();
            let mut playback = RetailPbakPlayback::new(prepared);
            playback.mark_started();
            for frame_index in 0..frame_count {
                assert_eq!(
                    playback.pre_shader_ticks_elapsed(),
                    (frame_index != 0)
                        .then(|| playback.prepared.recorded_ticks_elapsed[frame_index]),
                    "{} frame {frame_index} pre-shader clock",
                    known.id
                );
                let pending = playback
                    .pending_recorded_ticks_per_frame()
                    .expect("each legal recorded frame has pending timing");
                assert_eq!(
                    playback.frame_timing(40, 51),
                    Some(if frame_index == 0 {
                        timing(40, 51, 40, pending)
                    } else {
                        timing(17, pending, 17, pending)
                    })
                );
                let (input, end) = playback.advance_pad_boundary(0);
                assert_eq!(input.ticks_per_frame, Some(pending));
                assert_eq!(end.is_some(), frame_index + 1 == frame_count);
                assert_eq!(
                    playback.pre_shader_ticks_elapsed(),
                    playback
                        .prepared
                        .recorded_ticks_elapsed
                        .get(frame_index + 1)
                        .copied(),
                    "{} frame {frame_index} post-pad GL clock",
                    known.id
                );
            }
            assert!(playback.is_returning());
            assert_eq!(playback.frame_timing(40, 51), Some(timing(17, 51, 17, 51)));
            recordings += 1;
            frames += frame_count;
        }
        assert_eq!(recordings, 9);
        assert_eq!(frames, 10_966);
        assert_eq!(random_seed_b, 0xaf5a_ad71);
    }
}
