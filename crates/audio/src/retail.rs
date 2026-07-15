//! Deterministic high-level model of the retail 24-slot SFX voice engine.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "retail audio fields deliberately retain their 8/16/32-bit wrapping representations"
)]

use std::collections::HashMap;
use std::fmt;

use crust_formats::binary::Eid;
use crust_sim::gool::{
    AudioControlArgument, AudioControlOperation, AudioControlRequest, AudioHostRequest,
    AudioHostResponse, AudioScalarArgument, AudioVoiceCreateRequest, ObjectHandle,
};

use crate::mixer::{AudioMetrics, Mixer, Sample, VOICE_COUNT};

pub const DEFAULT_MAX_MIDI_VOICES: usize = 8;
pub const RETAIL_MAX_VOLUME: u16 = 0x3fff;
pub const RETAIL_BASE_PITCH: i16 = 0x1000;

const FLAG_FORCE_OFF: u32 = 0x001;
const FLAG_STOP_AFTER_RAMP: u32 = 0x002;
const FLAG_RAMP_OR_GLIDE: u32 = 0x004;
const FLAG_USED: u32 = 0x008;
const FLAG_DELAYED_KEY: u32 = 0x010;
const FLAG_RAMPING: u32 = 0x040;
const FLAG_GLIDING: u32 = 0x080;
const FLAG_SPATIALIZE: u32 = 0x200;
const FLAG_REVERB: u32 = 0x400;
const FLAG_UNKNOWN_800: u32 = 0x800;
const DEFAULT_TEMPLATE_FLAGS: u32 = FLAG_SPATIALIZE | FLAG_REVERB;
const DEFAULT_RAMP_RATE: i32 = 30;
const OWNER_FREE_RAMP_RATE: i32 = 9;
const STEAL_LOUDNESS_THRESHOLD: u16 = 0x800;

/// Failures that cannot be represented by retail's ordinary voice-id return.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetailAudioError {
    InvalidMaxMidiVoices(usize),
    MissingSample(Eid),
    ArgumentTypeMismatch { suboperation: u8 },
}

impl fmt::Display for RetailAudioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMaxMidiVoices(value) => {
                write!(formatter, "MIDI voice boundary {value} exceeds 24 slots")
            }
            Self::MissingSample(eid) => {
                write!(
                    formatter,
                    "ADIO sample 0x{:08x} is not registered",
                    eid.raw()
                )
            }
            Self::ArgumentTypeMismatch { suboperation } => write!(
                formatter,
                "audio control suboperation {suboperation} has the wrong generic-union argument"
            ),
        }
    }
}

impl std::error::Error for RetailAudioError {}

/// Exact high-level parameters copied from the next-voice template.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailVoiceParameters {
    pub flags: u32,
    pub owner: Option<ObjectHandle>,
    pub delay_counter: u8,
    pub sustain_counter: u8,
    pub amplitude: i16,
    pub delayed_key_counter: u16,
    pub pitch: i16,
    pub location: [i32; 3],
    pub target_amplitude: i16,
    pub target_pitch: i16,
    pub ramp_counter: i32,
    pub ramp_step: i32,
    pub glide_counter: i32,
    pub glide_step: i32,
}

impl Default for RetailVoiceParameters {
    fn default() -> Self {
        Self {
            flags: DEFAULT_TEMPLATE_FLAGS,
            owner: None,
            delay_counter: 1,
            sustain_counter: 128,
            amplitude: RETAIL_MAX_VOLUME as i16,
            delayed_key_counter: 0,
            pitch: RETAIL_BASE_PITCH,
            location: [0; 3],
            target_amplitude: 0,
            target_pitch: 0,
            ramp_counter: 0,
            ramp_step: 0,
            glide_counter: 0,
            glide_step: 0,
        }
    }
}

impl RetailVoiceParameters {
    fn blank_slot() -> Self {
        Self {
            flags: 0,
            owner: None,
            delay_counter: 0,
            sustain_counter: 0,
            amplitude: 0,
            delayed_key_counter: 0,
            pitch: 0,
            location: [0; 3],
            target_amplitude: 0,
            target_pitch: 0,
            ramp_counter: 0,
            ramp_step: 0,
            glide_counter: 0,
            glide_step: 0,
        }
    }

    fn reset_after_create(&mut self) {
        self.delay_counter = 1;
        self.sustain_counter = 128;
        self.amplitude = RETAIL_MAX_VOLUME as i16;
        self.pitch = RETAIL_BASE_PITCH;
        self.owner = None;
        self.delayed_key_counter = 0;
        self.location = [0; 3];
        // The source resets only the low twelve flag bits and the fields
        // above. Targets/counters/steps remain available to later template
        // controls, while private high bits survive the reset.
        self.flags = (self.flags & 0xffff_f000) | DEFAULT_TEMPLATE_FLAGS;
    }
}

#[derive(Clone, Debug)]
struct RetailVoice {
    id: i32,
    parameters: RetailVoiceParameters,
    adio: Option<Eid>,
    sample: Option<Sample>,
    keyed: bool,
    volume_left: u16,
    volume_right: u16,
}

impl Default for RetailVoice {
    fn default() -> Self {
        Self {
            id: 0,
            parameters: RetailVoiceParameters::blank_slot(),
            adio: None,
            sample: None,
            keyed: false,
            volume_left: 0,
            volume_right: 0,
        }
    }
}

impl RetailVoice {
    const fn active(&self) -> bool {
        self.parameters.flags & FLAG_USED != 0
    }
}

/// Public immutable view of one retail hardware slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetailVoiceState {
    pub index: usize,
    pub id: i32,
    pub active: bool,
    pub keyed: bool,
    pub adio: Option<Eid>,
    pub volume_left: u16,
    pub volume_right: u16,
    pub parameters: RetailVoiceParameters,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ValidatedControlArgument {
    Signed(i32),
    Unsigned(u32),
    SignedByte(i8),
    Vector([i32; 3]),
    Object(Option<ObjectHandle>),
    Unused,
}

/// Browser-independent 30 Hz retail SFX controller backed by [`Mixer`].
#[derive(Debug)]
pub struct RetailAudioEngine {
    mixer: Mixer,
    voices: [RetailVoice; VOICE_COUNT],
    template: RetailVoiceParameters,
    samples: HashMap<Eid, Sample>,
    max_midi_voices: usize,
    voice_id_counter: i32,
    sfx_volume: u8,
    voice_master_volume: u16,
    ramp_rate: i32,
    random_seed: u32,
    completed_sample_rekey_count: u32,
}

impl Default for RetailAudioEngine {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_MIDI_VOICES).expect("the default MIDI boundary fits 24 slots")
    }
}

impl RetailAudioEngine {
    /// Creates an empty deterministic retail voice table.
    ///
    /// # Errors
    ///
    /// Returns [`RetailAudioError::InvalidMaxMidiVoices`] when the MIDI/SFX
    /// boundary lies beyond the 24 hardware slots.
    pub fn new(max_midi_voices: usize) -> Result<Self, RetailAudioError> {
        if max_midi_voices > VOICE_COUNT {
            return Err(RetailAudioError::InvalidMaxMidiVoices(max_midi_voices));
        }
        let sfx_volume = u8::MAX;
        Ok(Self {
            mixer: Mixer::new(),
            voices: std::array::from_fn(|_| RetailVoice::default()),
            template: RetailVoiceParameters::default(),
            samples: HashMap::new(),
            max_midi_voices,
            voice_id_counter: 0,
            sfx_volume,
            voice_master_volume: scaled_option_volume(sfx_volume),
            ramp_rate: DEFAULT_RAMP_RATE,
            // Native `randb` uses zero-initialized process BSS. Browser hosts
            // synchronize this stream with level shaders at source-order
            // subsystem boundaries.
            random_seed: 0,
            completed_sample_rekey_count: 0,
        })
    }

    #[must_use]
    pub const fn max_midi_voices(&self) -> usize {
        self.max_midi_voices
    }

    /// Changes the MIDI/SFX slot boundary and clears all SFX slots. Retail
    /// performs this change as part of an audio-bank reinitialization.
    ///
    /// # Errors
    ///
    /// Returns [`RetailAudioError::InvalidMaxMidiVoices`] when `value` lies
    /// beyond the 24 hardware slots.
    pub fn set_max_midi_voices(&mut self, value: usize) -> Result<(), RetailAudioError> {
        if value > VOICE_COUNT {
            return Err(RetailAudioError::InvalidMaxMidiVoices(value));
        }
        self.stop_all_sfx();
        self.max_midi_voices = value;
        Ok(())
    }

    #[must_use]
    pub const fn sfx_volume(&self) -> u8 {
        self.sfx_volume
    }

    pub fn set_sfx_volume(&mut self, value: u8) {
        self.sfx_volume = value;
        self.voice_master_volume = scaled_option_volume(value);
    }

    /// Restores native's process-global RNG-B word before audio allocation.
    /// The browser host reconciles this same stream with dynamic lighting and
    /// PBAK selection at their source-ordered subsystem boundaries.
    pub const fn set_random_seed(&mut self, seed: u32) {
        self.random_seed = seed;
    }

    /// Current native RNG-B word after any voice-allocation draws.
    #[must_use]
    pub const fn random_seed(&self) -> u32 {
        self.random_seed
    }

    /// Restores the 32-bit voice-id counter for save-state or wrap testing.
    pub const fn set_voice_id_counter(&mut self, value: i32) {
        self.voice_id_counter = value;
    }

    #[must_use]
    pub const fn voice_id_counter(&self) -> i32 {
        self.voice_id_counter
    }

    #[must_use]
    pub const fn completed_sample_rekey_count(&self) -> u32 {
        self.completed_sample_rekey_count
    }

    #[must_use]
    pub const fn ramp_rate(&self) -> i32 {
        self.ramp_rate
    }

    #[must_use]
    pub const fn next_voice_template(&self) -> RetailVoiceParameters {
        self.template
    }

    #[must_use]
    pub fn voice(&self, index: usize) -> Option<RetailVoiceState> {
        self.voices.get(index).map(|voice| RetailVoiceState {
            index,
            id: voice.id,
            active: voice.active(),
            keyed: voice.keyed,
            adio: voice.adio,
            volume_left: voice.volume_left,
            volume_right: voice.volume_right,
            parameters: voice.parameters,
        })
    }

    #[must_use]
    pub fn active_sfx_count(&self) -> usize {
        self.sfx_range()
            .filter(|index| self.voices[*index].active())
            .count()
    }

    #[must_use]
    pub const fn metrics(&self) -> AudioMetrics {
        self.mixer.metrics()
    }

    /// Registers an already decoded sample without retaining proprietary
    /// source bytes.
    pub fn register_sample(&mut self, eid: Eid, sample: Sample) {
        self.samples.insert(eid, sample);
    }

    /// Reports whether this mounted engine already owns decoded PCM for an
    /// ADIO entry. Hosts can use this to avoid borrowing local stream bytes on
    /// every repeated create request.
    #[must_use]
    pub fn has_sample(&self, eid: Eid) -> bool {
        self.samples.contains_key(&eid)
    }

    /// Decodes and caches one caller-owned ADIO payload. The bytes are not
    /// retained after this call.
    pub fn register_adpcm(&mut self, eid: Eid, bytes: &[u8]) -> bool {
        let Some(sample) = self.mixer.cache_adpcm(eid.raw(), bytes) else {
            return false;
        };
        self.samples.insert(eid, sample);
        true
    }

    /// Applies one synchronous typed GOOL audio request.
    ///
    /// # Errors
    ///
    /// Returns an error when a create request names an unregistered sample or
    /// a control request carries the wrong tagged argument kind.
    pub fn handle_request(
        &mut self,
        request: AudioHostRequest,
    ) -> Result<AudioHostResponse, RetailAudioError> {
        match request {
            AudioHostRequest::CreateVoice(request) => {
                let voice_id = self.create_voice(request)?;
                Ok(AudioHostResponse::VoiceCreated { voice_id })
            }
            AudioHostRequest::Control(request) => {
                self.control(request)?;
                Ok(AudioHostResponse::ControlApplied)
            }
        }
    }

    /// Applies the non-GOOL thunder transaction emitted by
    /// `ShaderParamsUpdate`: set next-voice pitch, arm delayed key-on, then
    /// create an ownerless SFX voice. The registered sample must already be
    /// present, exactly like the ordinary synchronous create boundary.
    ///
    /// # Errors
    ///
    /// Returns [`RetailAudioError::MissingSample`] when the mounted audio
    /// cache does not contain `adio`.
    pub fn create_unowned_delayed_voice(
        &mut self,
        adio: Eid,
        volume: i32,
        pitch: u32,
        delayed_key_counter: u32,
    ) -> Result<i32, RetailAudioError> {
        self.template.pitch = pitch as i16;
        self.template.delayed_key_counter = delayed_key_counter as u16;
        self.template.flags |= FLAG_DELAYED_KEY;
        self.create_voice_with_owner(adio, volume, None)
    }

    /// Mixes interleaved stereo output through the existing deterministic
    /// software mixer.
    pub fn mix(&mut self, destination: &mut [i16]) {
        self.mixer.mix(destination);
    }

    /// Advances the source high-level voice controller once at 30 Hz.
    pub fn tick_30_hz(&mut self) {
        let mut prior_ramp_or_glide_continues = false;
        for index in self.sfx_range().collect::<Vec<_>>() {
            if !self.voices[index].active() {
                continue;
            }

            if self.voices[index].parameters.flags & FLAG_DELAYED_KEY != 0 {
                let counter = self.voices[index]
                    .parameters
                    .delayed_key_counter
                    .wrapping_sub(1);
                self.voices[index].parameters.delayed_key_counter = counter;
                if counter != 0 {
                    continue;
                }
                self.voices[index].parameters.flags &= !FLAG_DELAYED_KEY;
                self.key_on(index);
            }

            let completed = self.voices[index].keyed && !self.mixer.is_active(index);
            if completed {
                let repeats = self.voices[index].parameters.delay_counter.wrapping_sub(1);
                self.voices[index].parameters.delay_counter = repeats;
                if repeats == 0 {
                    self.deactivate(index);
                    continue;
                }
                self.completed_sample_rekey_count =
                    self.completed_sample_rekey_count.wrapping_add(1);
                self.key_on(index);
            }

            let mut volume_changed = false;
            let mut pitch_changed = false;
            {
                let parameters = &mut self.voices[index].parameters;
                if parameters.flags & FLAG_RAMPING != 0 {
                    parameters.amplitude = parameters
                        .amplitude
                        .wrapping_add(parameters.ramp_step as i16);
                    parameters.ramp_counter = parameters.ramp_counter.wrapping_sub(1);
                    if parameters.ramp_counter > 0 {
                        prior_ramp_or_glide_continues = true;
                    } else {
                        parameters.flags &= !FLAG_RAMPING;
                    }
                    volume_changed = true;
                }
                if parameters.flags & FLAG_GLIDING != 0 {
                    parameters.pitch = parameters.pitch.wrapping_add(parameters.glide_step as i16);
                    parameters.glide_counter = parameters.glide_counter.wrapping_sub(1);
                    if parameters.glide_counter > 0 {
                        prior_ramp_or_glide_continues = true;
                    } else {
                        parameters.flags &= !FLAG_GLIDING;
                    }
                    pitch_changed = true;
                }
            }
            if volume_changed {
                self.refresh_volume(index);
            }
            if pitch_changed {
                self.refresh_pitch(index);
            }

            let flags = self.voices[index].parameters.flags;
            if flags & FLAG_FORCE_OFF != 0
                || (!prior_ramp_or_glide_continues && flags & FLAG_STOP_AFTER_RAMP != 0)
            {
                self.deactivate(index);
            }
        }
    }

    /// Stops and releases every active voice associated with an object.
    pub fn free_owner(&mut self, owner: ObjectHandle) -> usize {
        self.ramp_rate = OWNER_FREE_RAMP_RATE;
        let mut freed = 0;
        for index in self.sfx_range().collect::<Vec<_>>() {
            if self.voices[index].parameters.owner != Some(owner) {
                continue;
            }
            if self.voices[index].active() {
                self.deactivate(index);
                freed += 1;
            }
            self.voices[index].parameters.owner = None;
            self.voices[index].parameters.sustain_counter = 0;
        }
        freed
    }

    pub fn stop_all_sfx(&mut self) {
        for index in 1..VOICE_COUNT {
            self.deactivate(index);
        }
    }

    fn sfx_range(&self) -> impl Iterator<Item = usize> {
        self.max_midi_voices..VOICE_COUNT
    }

    fn create_voice(&mut self, request: AudioVoiceCreateRequest) -> Result<i32, RetailAudioError> {
        self.create_voice_with_owner(request.adio, request.volume, Some(request.object))
    }

    fn create_voice_with_owner(
        &mut self,
        adio: Eid,
        volume: i32,
        owner: Option<ObjectHandle>,
    ) -> Result<i32, RetailAudioError> {
        if self.sfx_volume == 0 {
            return Ok(0);
        }
        let Some(index) = self.allocate_voice() else {
            return Ok(-1);
        };
        // Source creation treats hardware slot zero as allocation failure even
        // if a caller configured the MIDI boundary to zero.
        if index == 0 {
            return Ok(-1);
        }
        let sample = self
            .samples
            .get(&adio)
            .cloned()
            .ok_or(RetailAudioError::MissingSample(adio))?;

        let mut parameters = self.template;
        let scaled = volume.wrapping_mul(i32::from(self.voice_master_volume)) >> 14;
        parameters.amplitude = scaled as i16;
        parameters.owner = owner;
        if parameters.flags & FLAG_RAMPING != 0 {
            parameters.ramp_step = wrapping_div(
                i32::from(parameters.target_amplitude)
                    .wrapping_sub(i32::from(parameters.amplitude)),
                parameters.ramp_counter,
            );
        }
        self.template.reset_after_create();

        self.mixer.stop(index);
        self.voice_id_counter = self.voice_id_counter.wrapping_add(1);
        let id = self.voice_id_counter;
        parameters.flags |= FLAG_USED;
        let volume = amplitude_volume(parameters.amplitude);
        self.voices[index] = RetailVoice {
            id,
            parameters,
            adio: Some(adio),
            sample: Some(sample),
            keyed: false,
            volume_left: volume,
            volume_right: volume,
        };
        if parameters.flags & FLAG_DELAYED_KEY == 0 {
            self.key_on(index);
        }
        Ok(id)
    }

    fn allocate_voice(&mut self) -> Option<usize> {
        let mut minimum_sustain = u8::MAX;
        for index in self.sfx_range() {
            minimum_sustain = minimum_sustain.min(self.voices[index].parameters.sustain_counter);
            if !self.voices[index].active() {
                return Some(index);
            }
        }

        if minimum_sustain > self.template.sustain_counter {
            return None;
        }
        let mut quietest = None;
        for index in self.sfx_range() {
            let voice = &self.voices[index];
            if voice.parameters.sustain_counter != minimum_sustain {
                continue;
            }
            let volume = voice.volume_left.min(voice.volume_right);
            if quietest.is_none_or(|(_, known_volume)| volume < known_volume) {
                quietest = Some((index, volume));
            }
        }
        let (index, minimum_volume) = quietest?;
        if minimum_sustain == self.template.sustain_counter
            && minimum_volume >= STEAL_LOUDNESS_THRESHOLD
            && retail_random(100, &mut self.random_seed) >= 50
        {
            return None;
        }
        Some(index)
    }

    fn control(&mut self, request: AudioControlRequest) -> Result<(), RetailAudioError> {
        let argument = validate_argument(request.operation, request.argument)?;
        let voice_id = request.voice.voice_id();
        if voice_id == 0 {
            let mut parameters = self.template;
            apply_control_parameters(
                &mut parameters,
                request.operation,
                argument,
                true,
                &mut self.ramp_rate,
            );
            self.template = parameters;
            return Ok(());
        }

        if voice_id == -1 {
            for index in self.sfx_range().collect::<Vec<_>>() {
                if self.voices[index].parameters.owner == Some(request.object) {
                    self.control_slot(index, request.operation, argument, false);
                }
            }
            return Ok(());
        }

        let first_sfx_slot = self.max_midi_voices;
        let index = (first_sfx_slot..VOICE_COUNT).find(|index| self.voices[*index].id == voice_id);
        if let Some(index) = index {
            self.control_slot(index, request.operation, argument, false);
        }
        Ok(())
    }

    fn control_slot(
        &mut self,
        index: usize,
        operation: AudioControlOperation,
        argument: ValidatedControlArgument,
        template: bool,
    ) {
        let mut parameters = self.voices[index].parameters;
        let changes = apply_control_parameters(
            &mut parameters,
            operation,
            argument,
            template,
            &mut self.ramp_rate,
        );
        self.voices[index].parameters = parameters;
        if changes.volume {
            self.refresh_volume(index);
        }
        if changes.pitch {
            self.refresh_pitch(index);
        }
    }

    fn key_on(&mut self, index: usize) {
        let voice = &self.voices[index];
        let Some(sample) = voice.sample.clone() else {
            return;
        };
        let played = self.mixer.play(
            index,
            sample,
            voice.volume_left,
            voice.volume_right,
            voice.parameters.pitch as u16,
            0,
            1,
        );
        self.voices[index].keyed = played;
    }

    fn deactivate(&mut self, index: usize) {
        if let Some(voice) = self.voices.get_mut(index) {
            voice.parameters.flags &= !FLAG_USED;
            voice.keyed = false;
            self.mixer.stop(index);
        }
    }

    fn refresh_volume(&mut self, index: usize) {
        let volume = amplitude_volume(self.voices[index].parameters.amplitude);
        self.voices[index].volume_left = volume;
        self.voices[index].volume_right = volume;
        if self.voices[index].keyed {
            self.mixer.set_voice_volume(index, volume, volume);
        }
    }

    fn refresh_pitch(&mut self, index: usize) {
        if self.voices[index].keyed {
            self.mixer
                .set_voice_pitch(index, self.voices[index].parameters.pitch as u16);
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ParameterChanges {
    volume: bool,
    pitch: bool,
}

fn validate_argument(
    operation: AudioControlOperation,
    argument: AudioControlArgument,
) -> Result<ValidatedControlArgument, RetailAudioError> {
    let mismatch = || RetailAudioError::ArgumentTypeMismatch {
        suboperation: operation.suboperation,
    };
    match (operation.effective_suboperation(), argument) {
        (0 | 1 | 6, AudioControlArgument::Scalar(AudioScalarArgument::Signed(value))) => {
            Ok(ValidatedControlArgument::Signed(value))
        }
        (4 | 12, AudioControlArgument::Scalar(AudioScalarArgument::SignedByte(value))) => {
            Ok(ValidatedControlArgument::SignedByte(value))
        }
        (7 | 10 | 11, AudioControlArgument::Scalar(AudioScalarArgument::Unsigned(value))) => {
            Ok(ValidatedControlArgument::Unsigned(value))
        }
        (2 | 3, AudioControlArgument::Vector(value)) => Ok(ValidatedControlArgument::Vector(value)),
        (5, AudioControlArgument::Object(value)) => Ok(ValidatedControlArgument::Object(value)),
        (8 | 9 | 13 | 14, AudioControlArgument::Unused) => Ok(ValidatedControlArgument::Unused),
        _ => Err(mismatch()),
    }
}

fn apply_control_parameters(
    parameters: &mut RetailVoiceParameters,
    operation: AudioControlOperation,
    argument: ValidatedControlArgument,
    template: bool,
    ramp_rate: &mut i32,
) -> ParameterChanges {
    if operation.flags.force_off {
        parameters.flags |= FLAG_FORCE_OFF;
    }
    if operation.flags.stop_after_ramp {
        parameters.flags |= FLAG_STOP_AFTER_RAMP;
    }
    if operation.flags.ramp_or_glide {
        parameters.flags |= FLAG_RAMP_OR_GLIDE;
    }

    let mut changes = ParameterChanges::default();
    match (operation.effective_suboperation(), argument) {
        (0, ValidatedControlArgument::Signed(value)) => {
            let value = value as i16;
            if parameters.flags & FLAG_RAMP_OR_GLIDE != 0 {
                parameters.target_amplitude = value;
                parameters.ramp_counter = *ramp_rate;
                if !template {
                    parameters.ramp_step = wrapping_div(
                        i32::from(value).wrapping_sub(i32::from(parameters.amplitude)),
                        *ramp_rate,
                    );
                }
                parameters.flags |= FLAG_RAMPING;
            } else {
                parameters.amplitude = value;
                changes.volume = true;
            }
        }
        (1, ValidatedControlArgument::Signed(value)) => {
            let value = value as i16;
            if parameters.flags & FLAG_RAMP_OR_GLIDE != 0 {
                parameters.target_pitch = value;
                parameters.glide_counter = *ramp_rate;
                parameters.glide_step = wrapping_div(
                    i32::from(value).wrapping_sub(i32::from(parameters.pitch)),
                    *ramp_rate,
                );
                parameters.flags |= FLAG_GLIDING;
            } else {
                parameters.pitch = value;
                changes.pitch = true;
            }
        }
        (2 | 3, ValidatedControlArgument::Vector(value)) => {
            if parameters.flags & FLAG_RAMP_OR_GLIDE == 0 {
                parameters.location = value;
            }
        }
        (4, ValidatedControlArgument::SignedByte(value)) => {
            parameters.delay_counter = value as u8;
        }
        (5, ValidatedControlArgument::Object(value)) => parameters.owner = value,
        (6, ValidatedControlArgument::Signed(value)) => {
            *ramp_rate = if value == 0 { 1 } else { value }
        }
        (7, ValidatedControlArgument::Unsigned(value)) => {
            parameters.delayed_key_counter = value as u16;
            parameters.flags |= FLAG_DELAYED_KEY;
        }
        (8, ValidatedControlArgument::Unused) => parameters.flags |= FLAG_SPATIALIZE,
        (9, ValidatedControlArgument::Unused) => parameters.flags &= !FLAG_SPATIALIZE,
        (10, ValidatedControlArgument::Unsigned(value)) => {
            parameters.flags =
                (parameters.flags & !FLAG_UNKNOWN_800) | ((value << 3) & FLAG_UNKNOWN_800);
        }
        (11, ValidatedControlArgument::Unsigned(value)) => {
            parameters.flags = (parameters.flags & !FLAG_REVERB) | ((value << 2) & FLAG_REVERB);
        }
        (12, ValidatedControlArgument::SignedByte(value)) => {
            parameters.sustain_counter = value as u8;
        }
        (13 | 14, ValidatedControlArgument::Unused) => {}
        _ => unreachable!("control argument was validated before mutation"),
    }
    changes
}

fn scaled_option_volume(value: u8) -> u16 {
    ((u32::from(RETAIL_MAX_VOLUME) * u32::from(value)) >> 8) as u16
}

fn amplitude_volume(value: i16) -> u16 {
    value.unsigned_abs().min(RETAIL_MAX_VOLUME)
}

fn wrapping_div(numerator: i32, denominator: i32) -> i32 {
    if denominator == 0 {
        return 0;
    }
    numerator.checked_div(denominator).unwrap_or(i32::MIN)
}

fn retail_random(maximum: u32, seed: &mut u32) -> u32 {
    if maximum == 0 {
        return 0;
    }
    let generated = 0x41c6_4e6d_u32.wrapping_mul(*seed).wrapping_add(12_345);
    *seed = generated;
    let divided = generated / 15;
    let correction = (((u64::from(divided) * 33) >> 32) as u32).wrapping_add(divided) >> 1;
    let folded = ((correction & 0x7c00_0000) << 1).wrapping_sub(correction >> 26);
    divided
        .wrapping_sub(folded)
        .cast_signed()
        .wrapping_abs()
        .cast_unsigned()
        % maximum
}

#[cfg(test)]
mod tests {
    use crust_sim::gool::{AudioControlFlags, AudioVoiceSelector, StorageReference, StorageRegion};

    use super::*;

    fn handle(index: u16) -> ObjectHandle {
        ObjectHandle::new(index).unwrap()
    }

    fn eid(name: &str) -> Eid {
        Eid::from_name(name).unwrap()
    }

    fn source() -> StorageReference {
        // Checked register-zero reference for synthetic typed requests. The
        // retail engine consumes copied values, never this GOOL storage token.
        let word = 0xa500_0000 | ((StorageRegion::Register as u32) << 22);
        StorageReference::from_word(word).unwrap()
    }

    fn create(owner: ObjectHandle, adio: Eid, volume: i32) -> AudioHostRequest {
        AudioHostRequest::CreateVoice(AudioVoiceCreateRequest {
            object: owner,
            volume_source: source(),
            volume,
            adio_source: source(),
            adio,
        })
    }

    fn control(
        owner: ObjectHandle,
        voice: AudioVoiceSelector,
        suboperation: u8,
        flags: AudioControlFlags,
        argument: AudioControlArgument,
    ) -> AudioHostRequest {
        AudioHostRequest::Control(AudioControlRequest {
            object: owner,
            voice,
            operation: AudioControlOperation {
                suboperation,
                flags,
            },
            argument_source: None,
            argument,
        })
    }

    fn signed(value: i32) -> AudioControlArgument {
        AudioControlArgument::Scalar(AudioScalarArgument::Signed(value))
    }

    fn unsigned(value: u32) -> AudioControlArgument {
        AudioControlArgument::Scalar(AudioScalarArgument::Unsigned(value))
    }

    fn signed_byte(value: i8) -> AudioControlArgument {
        AudioControlArgument::Scalar(AudioScalarArgument::SignedByte(value))
    }

    fn voice_id(response: AudioHostResponse) -> i32 {
        let AudioHostResponse::VoiceCreated { voice_id } = response else {
            panic!("expected voice creation response");
        };
        voice_id
    }

    fn engine_with_sample(max_midi_voices: usize, sample_eid: Eid) -> RetailAudioEngine {
        let mut engine = RetailAudioEngine::new(max_midi_voices).unwrap();
        engine.register_sample(sample_eid, Sample::new(vec![12_000; 64], None));
        engine
    }

    #[test]
    fn boundary_first_free_and_monotonic_wrapping_ids_match_source_slots() {
        let sample_eid = eid("audio");
        let owner = handle(0);
        let mut engine = engine_with_sample(21, sample_eid);
        engine.set_voice_id_counter(i32::MAX - 1);

        let first = voice_id(
            engine
                .handle_request(create(owner, sample_eid, 0x3fff))
                .unwrap(),
        );
        let second = voice_id(
            engine
                .handle_request(create(owner, sample_eid, 0x3fff))
                .unwrap(),
        );
        assert_eq!(first, i32::MAX);
        assert_eq!(second, i32::MIN);
        assert!(!engine.voice(20).unwrap().active);
        assert_eq!(engine.voice(21).unwrap().id, i32::MAX);
        assert_eq!(engine.voice(22).unwrap().id, i32::MIN);

        let mut zero_boundary = engine_with_sample(0, sample_eid);
        assert_eq!(
            voice_id(
                zero_boundary
                    .handle_request(create(owner, sample_eid, 0x3fff))
                    .unwrap()
            ),
            -1
        );
    }

    #[test]
    fn muted_create_returns_zero_without_consuming_template_or_id() {
        let sample_eid = eid("audio");
        let owner = handle(0);
        let mut engine = RetailAudioEngine::new(8).unwrap();
        engine.set_sfx_volume(0);
        engine
            .handle_request(control(
                owner,
                AudioVoiceSelector::Template,
                1,
                AudioControlFlags::default(),
                signed(0x800),
            ))
            .unwrap();
        let template = engine.next_voice_template();

        assert_eq!(
            voice_id(
                engine
                    .handle_request(create(owner, sample_eid, 0x3fff))
                    .unwrap()
            ),
            0
        );
        assert_eq!(engine.voice_id_counter(), 0);
        assert_eq!(engine.next_voice_template(), template);
        assert_eq!(engine.active_sfx_count(), 0);
    }

    #[test]
    fn successful_create_copies_then_resets_the_next_voice_template() {
        let sample_eid = eid("audio");
        let owner = handle(0);
        let mut engine = engine_with_sample(23, sample_eid);
        for request in [
            control(
                owner,
                AudioVoiceSelector::Template,
                1,
                AudioControlFlags::default(),
                signed(0x800),
            ),
            control(
                owner,
                AudioVoiceSelector::Template,
                4,
                AudioControlFlags::default(),
                signed_byte(3),
            ),
            control(
                owner,
                AudioVoiceSelector::Template,
                12,
                AudioControlFlags::default(),
                signed_byte(7),
            ),
        ] {
            engine.handle_request(request).unwrap();
        }

        let id = voice_id(
            engine
                .handle_request(create(owner, sample_eid, 0x2000))
                .unwrap(),
        );
        let voice = engine.voice(23).unwrap();
        assert_eq!(voice.id, id);
        assert_eq!(voice.parameters.pitch, 0x800);
        assert_eq!(voice.parameters.delay_counter, 3);
        assert_eq!(voice.parameters.sustain_counter, 7);
        assert_eq!(voice.parameters.owner, Some(owner));
        assert_eq!(
            voice.parameters.amplitude,
            ((0x2000_i32 * i32::from(scaled_option_volume(u8::MAX))) >> 14) as i16
        );

        let template = engine.next_voice_template();
        assert_eq!(template.pitch, RETAIL_BASE_PITCH);
        assert_eq!(template.delay_counter, 1);
        assert_eq!(template.sustain_counter, 128);
        assert_eq!(template.owner, None);
    }

    #[test]
    fn shader_thunder_creates_an_ownerless_delayed_voice_and_resets_template() {
        let sample_eid = eid("lt1rA");
        let mut engine = engine_with_sample(23, sample_eid);

        let id = engine
            .create_unowned_delayed_voice(sample_eid, 0x2800, 0x0555, 3)
            .unwrap();
        let voice = engine.voice(23).unwrap();
        assert_eq!(voice.id, id);
        assert_eq!(voice.parameters.owner, None);
        assert_eq!(voice.parameters.pitch, 0x0555);
        assert_eq!(voice.parameters.delayed_key_counter, 3);
        assert!(!voice.keyed);
        assert_eq!(
            engine.next_voice_template(),
            RetailVoiceParameters::default()
        );

        engine.tick_30_hz();
        engine.tick_30_hz();
        assert!(!engine.voice(23).unwrap().keyed);
        engine.tick_30_hz();
        assert!(engine.voice(23).unwrap().keyed);
    }

    #[test]
    fn allocation_steals_shortest_sustain_then_quietest_tie() {
        let sample_eid = eid("audio");
        let owner = handle(0);
        let mut engine = engine_with_sample(22, sample_eid);

        for (sustain, amplitude) in [(5_i8, 1_000), (5, 100)] {
            engine
                .handle_request(control(
                    owner,
                    AudioVoiceSelector::Template,
                    12,
                    AudioControlFlags::default(),
                    signed_byte(sustain),
                ))
                .unwrap();
            let id = voice_id(
                engine
                    .handle_request(create(owner, sample_eid, 0x3fff))
                    .unwrap(),
            );
            engine
                .handle_request(control(
                    owner,
                    AudioVoiceSelector::ProcessRegister {
                        register: 1,
                        voice_id: id,
                    },
                    0,
                    AudioControlFlags::default(),
                    signed(amplitude),
                ))
                .unwrap();
        }
        let first_id = engine.voice(22).unwrap().id;
        let quiet_id = engine.voice(23).unwrap().id;

        engine
            .handle_request(control(
                owner,
                AudioVoiceSelector::Template,
                12,
                AudioControlFlags::default(),
                signed_byte(5),
            ))
            .unwrap();
        let replacement = voice_id(
            engine
                .handle_request(create(owner, sample_eid, 0x3fff))
                .unwrap(),
        );
        assert_eq!(engine.voice(22).unwrap().id, first_id);
        assert_ne!(engine.voice(23).unwrap().id, quiet_id);
        assert_eq!(engine.voice(23).unwrap().id, replacement);

        // A shorter priority wins even when it is louder.
        engine.voices[22].parameters.sustain_counter = 1;
        engine.voices[22].parameters.amplitude = 3_000;
        engine.refresh_volume(22);
        engine.voices[23].parameters.sustain_counter = 2;
        engine.voices[23].parameters.amplitude = 10;
        engine.refresh_volume(23);
        let replacement = voice_id(
            engine
                .handle_request(create(owner, sample_eid, 0x3fff))
                .unwrap(),
        );
        assert_eq!(engine.voice(22).unwrap().id, replacement);
    }

    #[test]
    fn delayed_key_zero_wraps_and_delayed_voices_skip_force_off() {
        let sample_eid = eid("audio");
        let owner = handle(0);
        let mut engine = engine_with_sample(23, sample_eid);
        engine
            .handle_request(control(
                owner,
                AudioVoiceSelector::Template,
                7,
                AudioControlFlags::default(),
                unsigned(0),
            ))
            .unwrap();
        let id = voice_id(
            engine
                .handle_request(create(owner, sample_eid, 0x3fff))
                .unwrap(),
        );
        engine
            .handle_request(control(
                owner,
                AudioVoiceSelector::ProcessRegister {
                    register: 1,
                    voice_id: id,
                },
                15,
                AudioControlFlags {
                    force_off: true,
                    ..AudioControlFlags::default()
                },
                signed(0),
            ))
            .unwrap();

        engine.tick_30_hz();
        let voice = engine.voice(23).unwrap();
        assert!(voice.active);
        assert!(!voice.keyed);
        assert_eq!(voice.parameters.delayed_key_counter, u16::MAX);
        assert_ne!(voice.parameters.flags & FLAG_FORCE_OFF, 0);
    }

    #[test]
    fn repeat_zero_wraps_to_255_and_rekeys_only_after_backend_completion() {
        let sample_eid = eid("audio");
        let owner = handle(0);
        let mut engine = RetailAudioEngine::new(23).unwrap();
        engine.register_sample(sample_eid, Sample::new(vec![12_000], None));
        engine
            .handle_request(control(
                owner,
                AudioVoiceSelector::Template,
                4,
                AudioControlFlags::default(),
                signed_byte(0),
            ))
            .unwrap();
        voice_id(
            engine
                .handle_request(create(owner, sample_eid, 0x3fff))
                .unwrap(),
        );

        engine.tick_30_hz();
        assert_eq!(engine.completed_sample_rekey_count(), 0);
        let mut output = [0_i16; 4];
        engine.mix(&mut output);
        assert!(!engine.mixer.is_active(23));
        engine.tick_30_hz();
        let voice = engine.voice(23).unwrap();
        assert!(voice.active);
        assert!(voice.keyed);
        assert_eq!(voice.parameters.delay_counter, u8::MAX);
        assert_eq!(engine.completed_sample_rekey_count(), 1);
        assert!(engine.mixer.is_active(23));
    }

    #[test]
    fn delayed_key_and_repeat_handshakes_follow_30_hz_order() {
        let sample_eid = eid("audio");
        let owner = handle(0);
        let mut engine = RetailAudioEngine::new(23).unwrap();
        engine.register_sample(sample_eid, Sample::new(vec![12_000], None));
        for request in [
            control(
                owner,
                AudioVoiceSelector::Template,
                7,
                AudioControlFlags::default(),
                unsigned(2),
            ),
            control(
                owner,
                AudioVoiceSelector::Template,
                4,
                AudioControlFlags::default(),
                signed_byte(2),
            ),
        ] {
            engine.handle_request(request).unwrap();
        }
        voice_id(
            engine
                .handle_request(create(owner, sample_eid, 0x3fff))
                .unwrap(),
        );

        engine.tick_30_hz();
        assert!(!engine.voice(23).unwrap().keyed);
        engine.tick_30_hz();
        assert!(engine.voice(23).unwrap().keyed);
        let mut output = [0_i16; 4];
        engine.mix(&mut output);
        engine.tick_30_hz();
        assert_eq!(engine.voice(23).unwrap().parameters.delay_counter, 1);
        assert_eq!(engine.completed_sample_rekey_count(), 1);
        engine.mix(&mut output);
        engine.tick_30_hz();
        assert!(!engine.voice(23).unwrap().active);
    }

    #[test]
    fn ramp_glide_stop_after_and_force_off_advance_with_source_flags() {
        let sample_eid = eid("audio");
        let owner = handle(0);
        let mut engine = engine_with_sample(22, sample_eid);
        engine
            .handle_request(control(
                owner,
                AudioVoiceSelector::Template,
                6,
                AudioControlFlags::default(),
                signed(2),
            ))
            .unwrap();
        let ramp_id = voice_id(
            engine
                .handle_request(create(owner, sample_eid, 0x3fff))
                .unwrap(),
        );
        let force_id = voice_id(
            engine
                .handle_request(create(owner, sample_eid, 0x3fff))
                .unwrap(),
        );

        engine
            .handle_request(control(
                owner,
                AudioVoiceSelector::ProcessRegister {
                    register: 1,
                    voice_id: ramp_id,
                },
                0,
                AudioControlFlags {
                    stop_after_ramp: true,
                    ramp_or_glide: true,
                    ..AudioControlFlags::default()
                },
                signed(1_000),
            ))
            .unwrap();
        engine
            .handle_request(control(
                owner,
                AudioVoiceSelector::ProcessRegister {
                    register: 1,
                    voice_id: ramp_id,
                },
                1,
                AudioControlFlags {
                    ramp_or_glide: true,
                    ..AudioControlFlags::default()
                },
                signed(0x800),
            ))
            .unwrap();
        engine
            .handle_request(control(
                owner,
                AudioVoiceSelector::ProcessRegister {
                    register: 1,
                    voice_id: force_id,
                },
                15,
                AudioControlFlags {
                    force_off: true,
                    ..AudioControlFlags::default()
                },
                signed(0),
            ))
            .unwrap();

        engine.tick_30_hz();
        let ramp = engine.voice(22).unwrap();
        assert!(ramp.active);
        assert_ne!(ramp.parameters.flags & FLAG_RAMPING, 0);
        assert_ne!(ramp.parameters.flags & FLAG_GLIDING, 0);
        assert!(!engine.voice(23).unwrap().active);

        engine.tick_30_hz();
        let ramp = engine.voice(22).unwrap();
        assert!(!ramp.active);
        assert_eq!(ramp.parameters.amplitude, 1_000);
        assert_eq!(ramp.parameters.pitch, 0x800);
    }

    #[test]
    fn owner_wide_control_and_teardown_are_slot_bounded() {
        let sample_eid = eid("audio");
        let first = handle(0);
        let second = handle(1);
        let mut engine = engine_with_sample(21, sample_eid);
        for owner in [first, first, second] {
            voice_id(
                engine
                    .handle_request(create(owner, sample_eid, 0x3fff))
                    .unwrap(),
            );
        }

        engine
            .handle_request(control(
                first,
                AudioVoiceSelector::ProcessRegister {
                    register: 1,
                    voice_id: -1,
                },
                0,
                AudioControlFlags::default(),
                signed(321),
            ))
            .unwrap();
        assert_eq!(engine.voice(21).unwrap().parameters.amplitude, 321);
        assert_eq!(engine.voice(22).unwrap().parameters.amplitude, 321);
        assert_ne!(engine.voice(23).unwrap().parameters.amplitude, 321);

        assert_eq!(engine.free_owner(first), 2);
        assert_eq!(engine.ramp_rate(), OWNER_FREE_RAMP_RATE);
        assert!(!engine.voice(21).unwrap().active);
        assert!(!engine.voice(22).unwrap().active);
        assert!(engine.voice(23).unwrap().active);
        assert_eq!(engine.voice(21).unwrap().parameters.owner, None);
    }

    #[test]
    fn malformed_argument_is_rejected_before_flags_mutate() {
        let owner = handle(0);
        let mut engine = RetailAudioEngine::default();
        let before = engine.next_voice_template();
        let error = engine
            .handle_request(control(
                owner,
                AudioVoiceSelector::Template,
                2,
                AudioControlFlags {
                    force_off: true,
                    ..AudioControlFlags::default()
                },
                signed(7),
            ))
            .unwrap_err();
        assert_eq!(
            error,
            RetailAudioError::ArgumentTypeMismatch { suboperation: 2 }
        );
        assert_eq!(engine.next_voice_template(), before);
    }
}
