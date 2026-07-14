//! Checked adapter from validated retail PBAK assets to browser runtime state.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use core::fmt;

use crust_formats::binary::Eid;
use crust_formats::stream::{
    Nsd, Nsf, PBAK_ENTRY_TYPE, PBAK_SPAWN_WORD_COUNT, PbakHeader, PbakLayout, RetailPathId,
    RetailZoneGraph, load_pbak_entry,
};
use crust_sim::camera::{GAME_STATE_CUTSCENE, RetailCameraLocation, RetailCameraRuntime};
use crust_sim::demo::{Demo, DemoEnd, DemoError, DemoFrame, DemoPlayer, DemoStep};
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PbakInputFrame {
    pub held: u32,
    pub ticks_per_frame: Option<i32>,
    pub end: Option<DemoEnd>,
}

impl RetailPbakPlayback {
    #[must_use]
    pub const fn new(prepared: PreparedPbak) -> Self {
        Self {
            prepared,
            phase: PlaybackPhase::Armed,
        }
    }

    #[must_use]
    pub const fn is_armed(&self) -> bool {
        matches!(self.phase, PlaybackPhase::Armed)
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
        match self.prepared.player.advance(u32::from(physical_held)) {
            DemoStep::Playing {
                held,
                ticks_per_frame,
                end,
                ..
            } => {
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

    pub fn take_end(&mut self) -> Option<DemoEnd> {
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
    MultipleEntries(usize),
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
            Self::MultipleEntries(count) => {
                write!(
                    formatter,
                    "stream contains {count} PBAK entries; expected at most one"
                )
            }
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

/// Selects and prepares the sole PBAK entry present in a retail level stream.
pub(crate) fn prepare_pair_pbak(
    metadata: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    graph: &RetailZoneGraph,
) -> Result<Option<PreparedPbak>, PbakRuntimeError> {
    let entries = nsf
        .entries()
        .filter(|entry| entry.entry_type == PBAK_ENTRY_TYPE)
        .collect::<Vec<_>>();
    let Some(entry) = entries.first().copied() else {
        return Ok(None);
    };
    if entries.len() != 1 {
        return Err(PbakRuntimeError::MultipleEntries(entries.len()));
    }
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
    })
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
        KNOWN_LEVELS, LevelId, PBAK_ENTRY_TYPE, PbakBound, PbakFrame, PbakLayout, PbakLevelState,
        StreamKind, StreamName, parse_nsd, parse_nsf,
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
        assert_eq!(
            playback.advance_input(0x0800),
            PbakInputFrame {
                held: 0x1000,
                ticks_per_frame: Some(34),
                end: Some(DemoEnd::Interrupted),
            }
        );
        assert_eq!(playback.take_end(), Some(DemoEnd::Interrupted));
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
    #[ignore = "set C1_STREAM_DIR to legally local extracted NTSC-U streams"]
    fn prepares_every_legally_local_recording_without_copying_game_data() {
        let root = PathBuf::from(
            std::env::var_os("C1_STREAM_DIR")
                .expect("C1_STREAM_DIR must name legally local extracted streams"),
        );
        let mut recordings = 0_usize;
        let mut frames = 0_usize;
        for known in KNOWN_LEVELS {
            let nsd_path = root.join(StreamName::new(known.id, StreamKind::Nsd).filename());
            let nsf_path = root.join(StreamName::new(known.id, StreamKind::Nsf).filename());
            let nsd_bytes = std::fs::read(&nsd_path)
                .unwrap_or_else(|error| panic!("{}: {error}", nsd_path.display()));
            let nsf_bytes = std::fs::read(&nsf_path)
                .unwrap_or_else(|error| panic!("{}: {error}", nsf_path.display()));
            let metadata = parse_nsd(&nsd_bytes, known.id).unwrap();
            let nsf = parse_nsf(&nsf_bytes, &metadata).unwrap();
            let count = nsf
                .entries()
                .filter(|entry| entry.entry_type == PBAK_ENTRY_TYPE)
                .count();
            if count == 0 {
                continue;
            }
            let graph = RetailZoneGraph::from_pair(&metadata, &nsf, &nsf_bytes).unwrap();
            let prepared = prepare_pair_pbak(&metadata, &nsf, &nsf_bytes, &graph)
                .unwrap()
                .expect("counted PBAK entry must prepare");
            assert_eq!(prepared.snapshot.level, known.id);
            recordings += 1;
            frames += prepared.frame_count();
        }
        assert_eq!(recordings, 9);
        assert_eq!(frames, 10_966);
    }
}
