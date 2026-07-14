//! Deterministic 44.1 kHz stereo voice mixer.

use std::collections::HashMap;
use std::sync::Arc;

use crate::adpcm;

pub const SAMPLE_RATE: u32 = 44_100;
pub const VOICE_COUNT: usize = 24;
pub const MUSIC_VOICE: usize = 0;
pub const SAMPLE_CACHE_ENTRIES: usize = 128;
pub const SAMPLE_CACHE_BYTES: usize = 8 * 1024 * 1024;
const VOLUME_BASE: f64 = 16_383.0;
const PITCH_BASE: f64 = 4_096.0;
const PITCH_UNITS: u64 = 4_096;
const MIX_DIVISOR: f64 = 12.0;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sample {
    pcm: Arc<[i16]>,
    loop_start: Option<usize>,
}

impl Sample {
    #[must_use]
    pub fn new(pcm: impl Into<Arc<[i16]>>, loop_start: Option<usize>) -> Self {
        let pcm = pcm.into();
        let loop_start = loop_start.filter(|index| *index < pcm.len());
        Self { pcm, loop_start }
    }

    #[must_use]
    pub fn from_adpcm(bytes: &[u8]) -> Self {
        let decoded = adpcm::decode(bytes);
        Self::new(decoded.samples, decoded.loop_start)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.pcm.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pcm.is_empty()
    }

    pub(crate) fn sample(&self, index: usize) -> Option<i16> {
        self.pcm.get(index).copied()
    }

    pub(crate) const fn loop_start(&self) -> Option<usize> {
        self.loop_start
    }
}

#[derive(Clone, Debug)]
struct Voice {
    sample: Option<Sample>,
    /// Playback position in 1/4096-sample units, matching the SPU pitch base exactly.
    position: u64,
    gain: f64,
    left: f64,
    right: f64,
    pitch: u16,
    delayed_frames: u16,
    repeats_left: u8,
    active: bool,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            sample: None,
            position: 0,
            gain: 1.0,
            left: 1.0,
            right: 1.0,
            pitch: 4_096,
            delayed_frames: 0,
            repeats_left: 0,
            active: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AudioMetrics {
    pub callbacks: u64,
    pub peak: i32,
    pub music_peak: i32,
    pub sfx_peak: i32,
    pub music_rms: i32,
    pub sfx_rms: i32,
    pub clips: u64,
    pub active_sfx: u8,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub cache_bytes: usize,
}

#[derive(Clone, Debug)]
struct CacheEntry {
    sample: Sample,
    bytes: usize,
    last_used: u64,
}

#[derive(Debug)]
pub struct Mixer {
    voices: [Voice; VOICE_COUNT],
    master: f64,
    muted: bool,
    metrics: AudioMetrics,
    cache: HashMap<u32, CacheEntry>,
    cache_clock: u64,
    music_scratch: Vec<f64>,
    sfx_scratch: Vec<f64>,
}

impl Default for Mixer {
    fn default() -> Self {
        Self::new()
    }
}

impl Mixer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            voices: std::array::from_fn(|_| Voice::default()),
            master: 1.0,
            muted: false,
            metrics: AudioMetrics::default(),
            cache: HashMap::new(),
            cache_clock: 0,
            music_scratch: Vec::new(),
            sfx_scratch: Vec::new(),
        }
    }

    pub fn set_master_volume(&mut self, value: u16) {
        self.master = f64::from(value.min(16_383)) / VOLUME_BASE;
    }

    pub const fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    /// Advance delay counters once per 30 Hz simulation frame.
    pub fn tick_30_hz(&mut self) {
        for voice in &mut self.voices {
            if voice.active && voice.delayed_frames > 0 {
                voice.delayed_frames -= 1;
            }
        }
    }

    pub fn cache_adpcm(&mut self, eid: u32, bytes: &[u8]) -> Option<Sample> {
        self.cache_clock = self.cache_clock.wrapping_add(1);
        if let Some(entry) = self.cache.get_mut(&eid) {
            entry.last_used = self.cache_clock;
            self.metrics.cache_hits = self.metrics.cache_hits.saturating_add(1);
            return Some(entry.sample.clone());
        }
        self.metrics.cache_misses = self.metrics.cache_misses.saturating_add(1);
        let sample = Sample::from_adpcm(bytes);
        let byte_len = sample.len().checked_mul(size_of::<i16>())?;
        if sample.is_empty() || byte_len > SAMPLE_CACHE_BYTES {
            return None;
        }
        while self.metrics.cache_bytes.saturating_add(byte_len) > SAMPLE_CACHE_BYTES
            || self.cache.len() >= SAMPLE_CACHE_ENTRIES
        {
            let oldest = self
                .cache
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| *key)?;
            if let Some(removed) = self.cache.remove(&oldest) {
                self.metrics.cache_bytes = self.metrics.cache_bytes.saturating_sub(removed.bytes);
            }
        }
        self.metrics.cache_bytes += byte_len;
        self.cache.insert(
            eid,
            CacheEntry {
                sample: sample.clone(),
                bytes: byte_len,
                last_used: self.cache_clock,
            },
        );
        Some(sample)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn play(
        &mut self,
        voice_index: usize,
        sample: Sample,
        volume_left: u16,
        volume_right: u16,
        pitch: u16,
        delay_frames: u16,
        repeats: u8,
    ) -> bool {
        let Some(voice) = self.voices.get_mut(voice_index) else {
            return false;
        };
        *voice = Voice {
            sample: Some(sample),
            position: 0,
            gain: 1.0,
            left: f64::from(volume_left.min(16_383)) / VOLUME_BASE,
            right: f64::from(volume_right.min(16_383)) / VOLUME_BASE,
            pitch,
            delayed_frames: delay_frames,
            repeats_left: repeats.max(1),
            active: true,
        };
        true
    }

    pub fn stop(&mut self, voice_index: usize) {
        if let Some(voice) = self.voices.get_mut(voice_index) {
            *voice = Voice::default();
        }
    }

    /// Updates one active voice's channel volumes without restarting its
    /// sample cursor.
    pub fn set_voice_volume(&mut self, voice_index: usize, left: u16, right: u16) -> bool {
        let Some(voice) = self.voices.get_mut(voice_index) else {
            return false;
        };
        voice.left = f64::from(left.min(16_383)) / VOLUME_BASE;
        voice.right = f64::from(right.min(16_383)) / VOLUME_BASE;
        true
    }

    /// Updates one active voice's SPU pitch without restarting its sample
    /// cursor.
    pub fn set_voice_pitch(&mut self, voice_index: usize, pitch: u16) -> bool {
        let Some(voice) = self.voices.get_mut(voice_index) else {
            return false;
        };
        voice.pitch = pitch;
        true
    }

    #[must_use]
    pub fn is_active(&self, voice_index: usize) -> bool {
        self.voices
            .get(voice_index)
            .is_some_and(|voice| voice.active)
    }

    #[must_use]
    pub const fn metrics(&self) -> AudioMetrics {
        self.metrics
    }

    /// Mix interleaved stereo frames. The destination is always completely initialized.
    pub fn mix(&mut self, destination: &mut [i16]) {
        destination.fill(0);
        let frame_count = destination.len() / 2;
        if frame_count == 0 {
            return;
        }
        let mixed_len = frame_count * 2;
        self.music_scratch.resize(mixed_len, 0.0);
        self.music_scratch.fill(0.0);
        self.sfx_scratch.resize(mixed_len, 0.0);
        self.sfx_scratch.fill(0.0);
        let mut active_sfx = 0_u8;

        for (voice_index, voice) in self.voices.iter_mut().enumerate() {
            if !voice.active || voice.delayed_frames > 0 {
                continue;
            }
            if voice_index != MUSIC_VOICE {
                active_sfx = active_sfx.saturating_add(1);
            }
            let target = if voice_index == MUSIC_VOICE {
                &mut self.music_scratch
            } else {
                &mut self.sfx_scratch
            };
            for frame in 0..frame_count {
                let Some(value) = voice_next(voice) else {
                    break;
                };
                target[frame * 2] += value * voice.left * voice.gain;
                target[frame * 2 + 1] += value * voice.right * voice.gain;
            }
        }

        let mut music_square = 0.0_f64;
        let mut sfx_square = 0.0_f64;
        let mut peak = 0_i32;
        let mut music_peak = 0_i32;
        let mut sfx_peak = 0_i32;
        for (index, output) in destination[..mixed_len].iter_mut().enumerate() {
            let music_sample = self.music_scratch[index] / MIX_DIVISOR;
            let sfx_sample = self.sfx_scratch[index] / MIX_DIVISOR;
            music_square += music_sample * music_sample;
            sfx_square += sfx_sample * sfx_sample;
            music_peak = music_peak.max(float_magnitude_to_i32(music_sample));
            sfx_peak = sfx_peak.max(float_magnitude_to_i32(sfx_sample));
            let mixed = if self.muted {
                0.0
            } else {
                (music_sample + sfx_sample) * self.master
            };
            let clipped = mixed.clamp(f64::from(i16::MIN), f64::from(i16::MAX));
            if mixed < f64::from(i16::MIN) || mixed > f64::from(i16::MAX) {
                self.metrics.clips = self.metrics.clips.saturating_add(1);
            }
            *output = float_to_i16(clipped);
            peak = peak.max(i32::from(*output).abs());
        }
        let divisor = usize_to_f64(mixed_len);
        self.metrics.callbacks = self.metrics.callbacks.saturating_add(1);
        self.metrics.peak = peak;
        self.metrics.music_peak = music_peak;
        self.metrics.sfx_peak = sfx_peak;
        self.metrics.music_rms = float_magnitude_to_i32((music_square / divisor).sqrt());
        self.metrics.sfx_rms = float_magnitude_to_i32((sfx_square / divisor).sqrt());
        self.metrics.active_sfx = active_sfx;
    }
}

fn voice_next(voice: &mut Voice) -> Option<f64> {
    loop {
        let Some(sample) = voice.sample.as_ref() else {
            voice.active = false;
            return None;
        };
        if let Some(value) = sample_linear(sample, &mut voice.position) {
            voice.position = voice.position.saturating_add(u64::from(voice.pitch));
            return Some(value);
        }
        if voice.repeats_left <= 1 {
            voice.active = false;
            return None;
        }
        voice.repeats_left -= 1;
        voice.position = 0;
    }
}

fn sample_linear(sample: &Sample, position: &mut u64) -> Option<f64> {
    if sample.is_empty() {
        return None;
    }
    let mut index = usize::try_from(*position / PITCH_UNITS).ok()?;
    if index >= sample.len() {
        let start = sample.loop_start?;
        let loop_len = sample.len().checked_sub(start)?;
        if loop_len == 0 {
            return None;
        }
        let start = u64::try_from(start).ok()?.checked_mul(PITCH_UNITS)?;
        let loop_len = u64::try_from(loop_len).ok()?.checked_mul(PITCH_UNITS)?;
        *position = start + position.checked_sub(start)? % loop_len;
        index = usize::try_from(*position / PITCH_UNITS).ok()?;
    }
    let fraction = f64::from(u16::try_from(*position % PITCH_UNITS).ok()?) / PITCH_BASE;
    let current = f64::from(sample.pcm[index]);
    let next_index = if index + 1 < sample.len() {
        index + 1
    } else {
        sample.loop_start.unwrap_or(index)
    };
    let next = f64::from(sample.pcm[next_index]);
    Some(current + fraction * (next - current))
}

#[allow(clippy::cast_possible_truncation)]
fn float_magnitude_to_i32(value: f64) -> i32 {
    debug_assert!(value.is_finite() && value >= 0.0 && value <= f64::from(i32::MAX));
    value as i32
}

#[allow(clippy::cast_possible_truncation)]
fn float_to_i16(value: f64) -> i16 {
    debug_assert!(value.is_finite() && value >= f64::from(i16::MIN));
    debug_assert!(value <= f64::from(i16::MAX));
    value as i16
}

#[allow(clippy::cast_precision_loss)]
fn usize_to_f64(value: usize) -> f64 {
    value as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolation_wraps_across_loop_boundary() {
        let sample = Sample::new(vec![0, 1_000, 2_000, 3_000], Some(1));
        let mut position = 3 * 4_096 + 2_048;
        assert_eq!(sample_linear(&sample, &mut position), Some(2_000.0));
        position += 4_096;
        assert_eq!(sample_linear(&sample, &mut position), Some(1_500.0));
    }

    #[test]
    fn one_shot_holds_terminal_sample_during_fraction() {
        let sample = Sample::new(vec![0, 1_000], None);
        let mut position = 4_096 + 2_048;
        assert_eq!(sample_linear(&sample, &mut position), Some(1_000.0));
        position += 4_096;
        assert_eq!(sample_linear(&sample, &mut position), None);
    }

    #[test]
    fn repeats_restart_without_a_silent_frame() {
        let sample = Sample::new(vec![12_000], None);
        let mut mixer = Mixer::new();
        assert!(mixer.play(1, sample, 16_383, 16_383, 4_096, 0, 2));
        let mut output = [0_i16; 6];
        mixer.mix(&mut output);
        assert_eq!(output, [1_000, 1_000, 1_000, 1_000, 0, 0]);
        assert!(!mixer.is_active(1));
    }

    #[test]
    fn odd_destination_keeps_unpaired_sample_silent() {
        let sample = Sample::new(vec![12_000; 4], None);
        let mut mixer = Mixer::new();
        assert!(mixer.play(1, sample, 16_383, 16_383, 4_096, 0, 1));
        let mut output = [7_i16; 3];
        mixer.mix(&mut output);
        assert_eq!(output, [1_000, 1_000, 0]);
    }

    #[test]
    fn delayed_voice_starts_on_requested_tick() {
        let sample = Sample::new(vec![12_000; 64], None);
        let mut mixer = Mixer::new();
        assert!(mixer.play(1, sample, 16_383, 16_383, 4_096, 3, 1));
        let mut output = [0_i16; 8];
        for _ in 0..2 {
            mixer.tick_30_hz();
            mixer.mix(&mut output);
            assert_eq!(output, [0; 8]);
        }
        mixer.tick_30_hz();
        mixer.mix(&mut output);
        assert!(output.iter().any(|sample| *sample != 0));
    }

    #[test]
    fn mute_keeps_voice_advancing() {
        let sample = Sample::new(vec![12_000; 8], None);
        let mut mixer = Mixer::new();
        mixer.play(0, sample, 16_383, 16_383, 4_096, 0, 1);
        mixer.set_muted(true);
        let mut output = [1_i16; 8];
        mixer.mix(&mut output);
        assert_eq!(output, [0; 8]);
        mixer.set_muted(false);
        mixer.mix(&mut output);
        assert!(output.iter().any(|sample| *sample != 0));
    }

    #[test]
    fn cache_reuses_decoded_sample() {
        let mut encoded = [0_u8; 16];
        encoded[1] = 1;
        encoded[2] = 1;
        let mut mixer = Mixer::new();
        let first = mixer.cache_adpcm(7, &encoded).unwrap();
        let second = mixer.cache_adpcm(7, &encoded).unwrap();
        assert_eq!(first, second);
        assert_eq!(mixer.metrics().cache_misses, 1);
        assert_eq!(mixer.metrics().cache_hits, 1);
    }

    #[test]
    fn cache_pressure_evicts_the_least_recently_used_entry() {
        let mut encoded = [0_u8; 16];
        encoded[1] = 1;
        encoded[2] = 1;
        let mut mixer = Mixer::new();
        for eid in 0..u32::try_from(SAMPLE_CACHE_ENTRIES).unwrap() {
            assert!(mixer.cache_adpcm(eid, &encoded).is_some());
        }
        assert!(mixer.cache_adpcm(0, &encoded).is_some());
        assert!(mixer.cache_adpcm(128, &encoded).is_some());
        let before = mixer.metrics();
        assert!(mixer.cache_adpcm(0, &encoded).is_some());
        assert!(mixer.cache_adpcm(1, &encoded).is_some());
        let after = mixer.metrics();
        assert_eq!(after.cache_hits, before.cache_hits + 1);
        assert_eq!(after.cache_misses, before.cache_misses + 1);
        assert!(after.cache_bytes <= SAMPLE_CACHE_BYTES);
    }

    #[test]
    fn cache_eviction_does_not_invalidate_an_active_voice() {
        let mut encoded = [0_u8; 16];
        encoded[1] = 1;
        encoded[2] = 1;
        let mut mixer = Mixer::new();
        let playing = mixer.cache_adpcm(0, &encoded).unwrap();
        assert!(mixer.play(1, playing, 16_383, 16_383, 4_096, 0, 1));
        for eid in 1..=u32::try_from(SAMPLE_CACHE_ENTRIES).unwrap() {
            assert!(mixer.cache_adpcm(eid, &encoded).is_some());
        }

        let misses = mixer.metrics().cache_misses;
        assert!(mixer.cache_adpcm(0, &encoded).is_some());
        assert_eq!(mixer.metrics().cache_misses, misses + 1);
        let mut output = [0_i16; 2];
        mixer.mix(&mut output);
        assert!(output.iter().any(|sample| *sample != 0));
    }
}
