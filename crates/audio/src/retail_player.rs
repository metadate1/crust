//! Browser-independent ownership and transition rules for retail music.
//!
//! The original runtime owns one looping primary SEP track and an optional
//! second track selected by GOOL misc 12/6. Zone MIDI changes fade over thirty
//! cooperative simulation ticks; a change is deferred while the second track
//! is active. Keeping those rules outside `web-sys` makes them deterministic
//! and testable on the host.

use std::fmt;

use crust_formats::binary::Eid;

use crate::{retail_music::RetailMusic, sequencer::Sequencer};

/// Duration of each retail zone-music fade at the 30 Hz simulation rate.
pub const RETAIL_MUSIC_FADE_TICKS: u8 = 30;

/// Observable playback phase, exposed to browser diagnostics and tests.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RetailMusicState {
    #[default]
    Stopped,
    Primary,
    Secondary,
    FadingOut,
    FadingIn,
}

/// Result of a zone or GOOL music request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetailMusicChange {
    Unchanged,
    Started,
    FadeStarted,
    TargetUpdated,
    Deferred,
    SecondaryStarted,
    PrimaryResumed,
    Stopped,
}

/// A decoded MIDI entry did not contain the primary SEP sequence required by
/// retail playback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailMusicPlayerError;

impl fmt::Display for RetailMusicPlayerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("retail MIDI entry contains no primary SEP sequence")
    }
}

impl std::error::Error for RetailMusicPlayerError {}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PendingTarget {
    Music(Eid, Box<RetailMusic>),
    Silence,
}

impl PendingTarget {
    fn checked(target: Option<(Eid, RetailMusic)>) -> Result<Self, RetailMusicPlayerError> {
        match target {
            Some((eid, music)) if music.sequences.is_empty() => {
                let _ = eid;
                Err(RetailMusicPlayerError)
            }
            Some((eid, music)) => Ok(Self::Music(eid, Box::new(music))),
            None => Ok(Self::Silence),
        }
    }

    const fn eid(&self) -> Option<Eid> {
        match self {
            Self::Music(eid, _) => Some(*eid),
            Self::Silence => None,
        }
    }
}

/// Owns decoded retail sequences and applies source-compatible transition
/// timing without retaining serialized stream bytes.
#[derive(Debug)]
pub struct RetailMusicPlayer {
    primary: Sequencer,
    secondary: Sequencer,
    state: RetailMusicState,
    current_eid: Option<Eid>,
    pending: Option<PendingTarget>,
    primary_gain: f32,
    secondary_gain: f32,
    fade_start_gain: f32,
    fade_tick: u8,
    has_secondary: bool,
    secondary_scratch: Vec<f32>,
}

impl Default for RetailMusicPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl RetailMusicPlayer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            primary: Sequencer::new(),
            secondary: Sequencer::new(),
            state: RetailMusicState::Stopped,
            current_eid: None,
            pending: None,
            primary_gain: 0.0,
            secondary_gain: 0.0,
            fade_start_gain: 0.0,
            fade_tick: 0,
            has_secondary: false,
            secondary_scratch: Vec::new(),
        }
    }

    /// Starts one decoded MIDI entry without a cross-fade. This is used only
    /// after a fresh stream-pair mount, where no previous owner may survive.
    ///
    /// # Errors
    ///
    /// Returns an error when `music` has no primary sequence.
    pub fn start_immediate(
        &mut self,
        eid: Eid,
        music: RetailMusic,
    ) -> Result<RetailMusicChange, RetailMusicPlayerError> {
        if music.sequences.is_empty() {
            return Err(RetailMusicPlayerError);
        }
        self.install_music(eid, music, 1.0);
        self.state = RetailMusicState::Primary;
        Ok(RetailMusicChange::Started)
    }

    /// Requests the MIDI entry associated with the latest zone, or silence
    /// for `None`. An initial request starts immediately; later changes use
    /// the retail thirty-tick fade and keep only the newest pending target.
    ///
    /// # Errors
    ///
    /// Returns an error when a requested MIDI entry has no primary sequence.
    pub fn request(
        &mut self,
        target: Option<(Eid, RetailMusic)>,
    ) -> Result<RetailMusicChange, RetailMusicPlayerError> {
        let target = PendingTarget::checked(target)?;
        if self.requested_eid() == target.eid() {
            return Ok(RetailMusicChange::Unchanged);
        }

        match self.state {
            RetailMusicState::Stopped => match target {
                PendingTarget::Music(eid, music) => self.start_immediate(eid, *music),
                PendingTarget::Silence => Ok(RetailMusicChange::Unchanged),
            },
            RetailMusicState::Primary | RetailMusicState::FadingIn => {
                self.pending = Some(target);
                self.begin_fade_out();
                Ok(RetailMusicChange::FadeStarted)
            }
            RetailMusicState::FadingOut => {
                self.pending = Some(target);
                Ok(RetailMusicChange::TargetUpdated)
            }
            RetailMusicState::Secondary => {
                self.pending = Some(target);
                Ok(RetailMusicChange::Deferred)
            }
        }
    }

    /// Applies GOOL misc 12/6. Values whose high byte is three select the
    /// secondary SEP track; every other value resumes the paused primary.
    pub fn toggle_secondary(&mut self, value: u32) -> RetailMusicChange {
        if value >> 8 == 3 {
            if !self.has_secondary
                || !matches!(
                    self.state,
                    RetailMusicState::Primary | RetailMusicState::FadingIn
                )
            {
                return RetailMusicChange::Unchanged;
            }
            self.primary.set_playing(false);
            self.secondary.rewind();
            self.secondary.set_playing(true);
            self.primary_gain = 0.0;
            self.secondary_gain = 1.0;
            self.state = RetailMusicState::Secondary;
            return RetailMusicChange::SecondaryStarted;
        }

        if self.state != RetailMusicState::Secondary {
            return RetailMusicChange::Unchanged;
        }
        self.secondary.set_playing(false);
        self.secondary.rewind();
        self.secondary_gain = 0.0;
        self.primary.set_playing(true);
        self.primary_gain = 1.0;
        self.state = RetailMusicState::Primary;
        if self.pending.is_some() {
            self.begin_fade_out();
        }
        RetailMusicChange::PrimaryResumed
    }

    /// Advances retail fade state once per cooperative 30 Hz simulation tick.
    pub fn tick_30_hz(&mut self) {
        match self.state {
            RetailMusicState::FadingOut => {
                self.fade_tick = self
                    .fade_tick
                    .saturating_add(1)
                    .min(RETAIL_MUSIC_FADE_TICKS);
                let progress = f32::from(self.fade_tick) / f32::from(RETAIL_MUSIC_FADE_TICKS);
                self.primary_gain = self.fade_start_gain * (1.0 - progress);
                if self.fade_tick == RETAIL_MUSIC_FADE_TICKS {
                    let target = self.pending.take().unwrap_or(PendingTarget::Silence);
                    match target {
                        PendingTarget::Music(eid, music) => {
                            self.install_music(eid, *music, 0.0);
                            self.state = RetailMusicState::FadingIn;
                        }
                        PendingTarget::Silence => {
                            self.stop_immediate();
                        }
                    }
                }
            }
            RetailMusicState::FadingIn => {
                self.fade_tick = self
                    .fade_tick
                    .saturating_add(1)
                    .min(RETAIL_MUSIC_FADE_TICKS);
                self.primary_gain = f32::from(self.fade_tick) / f32::from(RETAIL_MUSIC_FADE_TICKS);
                if self.fade_tick == RETAIL_MUSIC_FADE_TICKS {
                    self.primary_gain = 1.0;
                    self.state = RetailMusicState::Primary;
                }
            }
            RetailMusicState::Stopped | RetailMusicState::Primary | RetailMusicState::Secondary => {
            }
        }
    }

    /// Renders additive stereo samples and applies the independent primary and
    /// secondary sequence gains before clamping the combined bus.
    pub fn render(&mut self, destination: &mut [f32]) {
        self.primary.render(destination);
        for sample in destination.iter_mut() {
            *sample *= self.primary_gain;
        }
        self.secondary_scratch.resize(destination.len(), 0.0);
        self.secondary.render(&mut self.secondary_scratch);
        for (sample, secondary) in destination.iter_mut().zip(&self.secondary_scratch) {
            *sample = (*sample + *secondary * self.secondary_gain).clamp(-1.0, 1.0);
        }
    }

    /// Drops both decoded banks, both sequences, all voices, and transition
    /// state immediately at an owning level boundary.
    pub fn stop_immediate(&mut self) -> RetailMusicChange {
        self.primary.clear();
        self.secondary.clear();
        self.state = RetailMusicState::Stopped;
        self.current_eid = None;
        self.pending = None;
        self.primary_gain = 0.0;
        self.secondary_gain = 0.0;
        self.fade_start_gain = 0.0;
        self.fade_tick = 0;
        self.has_secondary = false;
        self.secondary_scratch.clear();
        RetailMusicChange::Stopped
    }

    #[must_use]
    pub const fn state(&self) -> RetailMusicState {
        self.state
    }

    #[must_use]
    pub const fn current_eid(&self) -> Option<Eid> {
        self.current_eid
    }

    /// Returns the newest requested EID, including a deferred or fading
    /// target. `None` means that silence is requested.
    #[must_use]
    pub fn requested_eid(&self) -> Option<Eid> {
        self.pending
            .as_ref()
            .map_or(self.current_eid, PendingTarget::eid)
    }

    #[must_use]
    pub const fn has_secondary(&self) -> bool {
        self.has_secondary
    }

    #[must_use]
    pub const fn primary_gain(&self) -> f32 {
        self.primary_gain
    }

    #[must_use]
    pub const fn secondary_gain(&self) -> f32 {
        self.secondary_gain
    }

    fn begin_fade_out(&mut self) {
        self.fade_start_gain = self.primary_gain;
        self.fade_tick = 0;
        self.state = RetailMusicState::FadingOut;
    }

    fn install_music(&mut self, eid: Eid, music: RetailMusic, initial_gain: f32) {
        let RetailMusic {
            bank,
            mut sequences,
        } = music;
        let primary = sequences.remove(0);
        let secondary = sequences.into_iter().next();

        self.primary.clear();
        self.primary.set_sample_bank(Some(bank.clone()));
        self.primary.load(primary);
        self.primary.set_playing(true);

        self.secondary.clear();
        self.has_secondary = secondary.is_some();
        if let Some(secondary) = secondary {
            self.secondary.set_sample_bank(Some(bank));
            self.secondary.load(secondary);
            self.secondary.set_playing(false);
        }

        self.current_eid = Some(eid);
        self.pending = None;
        self.primary_gain = initial_gain.clamp(0.0, 1.0);
        self.secondary_gain = 0.0;
        self.fade_start_gain = self.primary_gain;
        self.fade_tick = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        mixer::Sample,
        sequencer::{EventKind, SampleBank, SampleProgram, SampleTone, Sequence, SequenceEvent},
    };

    const FIRST: Eid = Eid::from_raw(1);
    const SECOND: Eid = Eid::from_raw(3);
    const THIRD: Eid = Eid::from_raw(5);

    fn music(track_count: usize) -> RetailMusic {
        let mut bank = SampleBank::new(127, 64);
        assert!(bank.set_program(
            0,
            SampleProgram {
                volume: 127,
                priority: 10,
                mode: 0,
                pan: 64,
                attribute: 0,
                tones: vec![SampleTone {
                    sample: Sample::new(vec![16_000_i16; 128], Some(0)),
                    priority: 10,
                    mode: 0,
                    volume: 127,
                    pan: 64,
                    center_note: 60,
                    pitch_shift: 0,
                    note_min: 0,
                    note_max: 127,
                    vibrato_width: 0,
                    vibrato_time: 0,
                    portamento_width: 0,
                    portamento_time: 0,
                    pitch_bend_min: 2,
                    pitch_bend_max: 2,
                    adsr1: 0,
                    adsr2: 0,
                }],
            }
        ));
        let mut sequence = Sequence::new(
            60,
            vec![
                SequenceEvent {
                    tick: 0,
                    kind: EventKind::Program {
                        channel: 0,
                        program: 0,
                    },
                },
                SequenceEvent {
                    tick: 0,
                    kind: EventKind::NoteOn {
                        channel: 0,
                        note: 60,
                        velocity: 127,
                    },
                },
                SequenceEvent {
                    tick: 60,
                    kind: EventKind::Marker,
                },
            ],
        );
        sequence.loop_tick = Some(0);
        RetailMusic {
            bank,
            sequences: vec![sequence; track_count],
        }
    }

    #[test]
    fn initial_request_starts_and_renders_primary() {
        let mut player = RetailMusicPlayer::new();
        assert_eq!(
            player.request(Some((FIRST, music(1)))),
            Ok(RetailMusicChange::Started)
        );
        assert_eq!(player.state(), RetailMusicState::Primary);
        assert_eq!(player.current_eid(), Some(FIRST));
        let mut output = [0.0_f32; 128];
        player.render(&mut output);
        assert!(output.iter().any(|sample| sample.abs() > 0.01));
        assert!(output.iter().all(|sample| (-1.0..=1.0).contains(sample)));
    }

    #[test]
    fn zone_change_fades_out_swaps_and_fades_in_over_thirty_ticks_each() {
        let mut player = RetailMusicPlayer::new();
        player.start_immediate(FIRST, music(1)).unwrap();
        assert_eq!(
            player.request(Some((SECOND, music(1)))),
            Ok(RetailMusicChange::FadeStarted)
        );
        for _ in 0..29 {
            player.tick_30_hz();
        }
        assert_eq!(player.current_eid(), Some(FIRST));
        assert_eq!(player.state(), RetailMusicState::FadingOut);
        player.tick_30_hz();
        assert_eq!(player.current_eid(), Some(SECOND));
        assert_eq!(player.state(), RetailMusicState::FadingIn);
        assert!(player.primary_gain().abs() < f32::EPSILON);
        for _ in 0..30 {
            player.tick_30_hz();
        }
        assert_eq!(player.state(), RetailMusicState::Primary);
        assert!((player.primary_gain() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn newest_zone_request_replaces_the_target_during_fade() {
        let mut player = RetailMusicPlayer::new();
        player.start_immediate(FIRST, music(1)).unwrap();
        player.request(Some((SECOND, music(1)))).unwrap();
        player.tick_30_hz();
        assert_eq!(
            player.request(Some((THIRD, music(1)))),
            Ok(RetailMusicChange::TargetUpdated)
        );
        for _ in 1..RETAIL_MUSIC_FADE_TICKS {
            player.tick_30_hz();
        }
        assert_eq!(player.current_eid(), Some(THIRD));
    }

    #[test]
    fn silence_request_releases_both_sequence_owners_after_fade() {
        let mut player = RetailMusicPlayer::new();
        player.start_immediate(FIRST, music(2)).unwrap();
        assert_eq!(player.request(None), Ok(RetailMusicChange::FadeStarted));
        for _ in 0..RETAIL_MUSIC_FADE_TICKS {
            player.tick_30_hz();
        }
        assert_eq!(player.state(), RetailMusicState::Stopped);
        assert_eq!(player.current_eid(), None);
        assert!(!player.has_secondary());
    }

    #[test]
    fn secondary_track_defers_zone_change_until_primary_resumes() {
        let mut player = RetailMusicPlayer::new();
        player.start_immediate(FIRST, music(2)).unwrap();
        assert_eq!(
            player.toggle_secondary(0x300),
            RetailMusicChange::SecondaryStarted
        );
        assert_eq!(player.state(), RetailMusicState::Secondary);
        assert!((player.secondary_gain() - 1.0).abs() < f32::EPSILON);
        assert_eq!(
            player.request(Some((SECOND, music(1)))),
            Ok(RetailMusicChange::Deferred)
        );
        for _ in 0..(RETAIL_MUSIC_FADE_TICKS * 2) {
            player.tick_30_hz();
        }
        assert_eq!(player.current_eid(), Some(FIRST));
        assert_eq!(player.requested_eid(), Some(SECOND));
        assert_eq!(
            player.toggle_secondary(0),
            RetailMusicChange::PrimaryResumed
        );
        assert_eq!(player.state(), RetailMusicState::FadingOut);
    }

    #[test]
    fn malformed_decoded_music_does_not_disturb_current_playback() {
        let mut player = RetailMusicPlayer::new();
        player.start_immediate(FIRST, music(1)).unwrap();
        let empty = RetailMusic {
            bank: SampleBank::new(127, 64),
            sequences: Vec::new(),
        };
        assert_eq!(
            player.request(Some((SECOND, empty))),
            Err(RetailMusicPlayerError)
        );
        assert_eq!(player.current_eid(), Some(FIRST));
        assert_eq!(player.state(), RetailMusicState::Primary);
    }
}
