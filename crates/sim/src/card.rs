//! Retail-compatible virtual memory-card payloads and handshakes.

use core::fmt;

pub const CARD_SLOT_COUNT: usize = 15;
pub const CARD_PAYLOAD_SIZE: usize = 128;
pub const CARD_STORAGE_KEY: &str = "c1.virtual-memory-card.v1";
pub const CARD_SCHEMA: &str = "c1-virtual-memory-card";
pub const CARD_VERSION: u32 = 1;
pub const RESUME_STORAGE_KEY: &str = "c1.browser-resume.v1";
pub const RESUME_SCHEMA: &str = "c1-browser-resume";
pub const RESUME_VERSION: u32 = 1;

const PROGRESS_OFFSET: usize = 0;
const LEVEL_COUNT_OFFSET: usize = 4;
const INITIAL_LIVES_OFFSET: usize = 8;
const UNKNOWN_OFFSET: usize = 12;
const MONO_OFFSET: usize = 16;
const SFX_VOLUME_OFFSET: usize = 20;
const MUSIC_VOLUME_OFFSET: usize = 24;
const ITEM_POOL_1_OFFSET: usize = 28;
const ITEM_POOL_2_OFFSET: usize = 32;
const CHECKSUM_OFFSET: usize = 124;

/// Progression and options represented by the retail 128-byte payload.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SaveData {
    pub level_count: u32,
    pub initial_lives: u32,
    pub unknown_6190c: u32,
    pub mono: bool,
    pub sfx_volume: u32,
    pub music_volume: u32,
    pub item_pool_1: u32,
    pub item_pool_2: u32,
    pub gem_count: u8,
    pub key_count: u32,
}

impl SaveData {
    #[must_use]
    pub const fn packed_progress(self) -> u32 {
        self.key_count.wrapping_shl(10)
            | (self.gem_count as u32).wrapping_shl(5)
            | (self.level_count & 0x1f)
    }
}

/// Opaque retail save bytes with explicit endian access.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct CardPayload([u8; CARD_PAYLOAD_SIZE]);

impl fmt::Debug for CardPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CardPayload")
            .field("valid", &self.is_valid())
            .field("progress", &read_u32(&self.0, PROGRESS_OFFSET))
            .finish_non_exhaustive()
    }
}

impl CardPayload {
    /// Encodes a fresh payload; bytes not used by the retail fields are zero.
    #[must_use]
    pub fn encode(data: SaveData) -> Self {
        let mut bytes = [0_u8; CARD_PAYLOAD_SIZE];
        write_u32(&mut bytes, PROGRESS_OFFSET, data.packed_progress());
        write_u32(&mut bytes, LEVEL_COUNT_OFFSET, data.level_count);
        write_u32(&mut bytes, INITIAL_LIVES_OFFSET, data.initial_lives);
        write_u32(&mut bytes, UNKNOWN_OFFSET, data.unknown_6190c);
        write_u32(&mut bytes, MONO_OFFSET, u32::from(data.mono));
        write_u32(&mut bytes, SFX_VOLUME_OFFSET, data.sfx_volume);
        write_u32(&mut bytes, MUSIC_VOLUME_OFFSET, data.music_volume);
        write_u32(&mut bytes, ITEM_POOL_1_OFFSET, data.item_pool_1);
        write_u32(&mut bytes, ITEM_POOL_2_OFFSET, data.item_pool_2);
        let checksum = checksum_bytes(&bytes);
        write_u32(&mut bytes, CHECKSUM_OFFSET, checksum);
        Self(bytes)
    }

    /// Accepts bytes only when their retail checksum is valid.
    pub fn from_bytes(bytes: [u8; CARD_PAYLOAD_SIZE]) -> Result<Self, PayloadError> {
        let payload = Self(bytes);
        if payload.is_valid() {
            Ok(payload)
        } else {
            Err(PayloadError::ChecksumMismatch)
        }
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; CARD_PAYLOAD_SIZE] {
        &self.0
    }

    #[must_use]
    pub fn into_bytes(self) -> [u8; CARD_PAYLOAD_SIZE] {
        self.0
    }

    #[must_use]
    pub fn checksum(&self) -> u32 {
        read_u32(&self.0, CHECKSUM_OFFSET)
    }

    #[must_use]
    pub fn calculated_checksum(&self) -> u32 {
        checksum_bytes(&self.0)
    }

    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.checksum() == self.calculated_checksum()
    }

    pub fn decode(&self) -> Result<SaveData, PayloadError> {
        if !self.is_valid() {
            return Err(PayloadError::ChecksumMismatch);
        }
        let progress = read_u32(&self.0, PROGRESS_OFFSET);
        Ok(SaveData {
            level_count: read_u32(&self.0, LEVEL_COUNT_OFFSET),
            initial_lives: read_u32(&self.0, INITIAL_LIVES_OFFSET),
            unknown_6190c: read_u32(&self.0, UNKNOWN_OFFSET),
            mono: read_u32(&self.0, MONO_OFFSET) != 0,
            sfx_volume: read_u32(&self.0, SFX_VOLUME_OFFSET),
            music_volume: read_u32(&self.0, MUSIC_VOLUME_OFFSET),
            item_pool_1: read_u32(&self.0, ITEM_POOL_1_OFFSET),
            item_pool_2: read_u32(&self.0, ITEM_POOL_2_OFFSET),
            gem_count: ((progress >> 5) & 0x1f) as u8,
            key_count: progress >> 10,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PayloadError {
    ChecksumMismatch,
}

fn read_u32(bytes: &[u8; CARD_PAYLOAD_SIZE], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("fixed payload field"),
    )
}

fn write_u32(bytes: &mut [u8; CARD_PAYLOAD_SIZE], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn checksum_bytes(bytes: &[u8; CARD_PAYLOAD_SIZE]) -> u32 {
    let mut checksum = 0x1234_5678_u32;
    for (index, byte) in bytes.iter().copied().enumerate() {
        let value = if (CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4).contains(&index) {
            0
        } else {
            byte
        };
        checksum = checksum.wrapping_add(u32::from(value)).rotate_left(3);
    }
    checksum
}

/// Bitfield observed by retail card GOOL.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CardFlags(u8);

impl CardFlags {
    pub const PENDING: Self = Self(0x01);
    pub const ERROR: Self = Self(0x02);
    pub const CHECK_NEEDED: Self = Self(0x04);
    pub const CHECKING: Self = Self(0x08);
    pub const NEW_DEVICE: Self = Self(0x10);
    pub const FLAG_6: Self = Self(0x20);

    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    #[must_use]
    pub const fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 == flag.0
    }

    const fn insert(&mut self, flag: Self) {
        self.0 |= flag.0;
    }

    const fn remove(&mut self, flag: Self) {
        self.0 &= !flag.0;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CardOperation {
    /// Any operation number not otherwise represented, including retail op 0.
    Unsupported(i32),
    ClearFlag6,
    SaveSelected,
    LoadSelected,
    Format,
    SaveCurrent,
    ProbeName,
    ProbePresent,
    ForgetCurrent,
    Rescan,
}

impl CardOperation {
    #[must_use]
    pub const fn from_retail(operation: i32) -> Self {
        match operation {
            2 => Self::ClearFlag6,
            3 => Self::SaveSelected,
            4 => Self::LoadSelected,
            5 => Self::Format,
            6 => Self::SaveCurrent,
            7 => Self::ProbeName,
            8 => Self::ProbePresent,
            9 => Self::ForgetCurrent,
            10 => Self::Rescan,
            other => Self::Unsupported(other),
        }
    }

    #[must_use]
    pub const fn mutates_storage(self) -> bool {
        matches!(self, Self::SaveSelected | Self::Format | Self::SaveCurrent)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CardOutcome {
    Complete,
    Loaded(SaveData),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CardError {
    Busy,
    InvalidPart,
    NoCurrentSlot,
    MissingSaveData,
    CorruptSlot,
    StorageUnavailable,
    UnsupportedOperation,
    NameProbeUnsupported,
    CardFull,
}

/// Physical content of one virtual-card slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Slot {
    Empty,
    Valid(CardPayload),
    Corrupt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CardSnapshot {
    slot_map: [Option<usize>; CARD_SLOT_COUNT],
    slot_valid: [bool; CARD_SLOT_COUNT],
    partinfos: [u32; CARD_SLOT_COUNT],
    part_count: usize,
}

/// Immutable card metadata exposed to authored GOOL globals.
///
/// Native publishes the fifteen part words first, then the part count as the
/// readiness marker, and finally lets scripts observe the current flag word.
/// Keeping the snapshot typed prevents browser persistence details from
/// leaking into the interpreter.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CardPublishedState {
    pub flags: CardFlags,
    pub part_count: u32,
    pub partinfos: [u32; CARD_SLOT_COUNT],
}

impl CardSnapshot {
    const EMPTY: Self = Self {
        slot_map: [None; CARD_SLOT_COUNT],
        slot_valid: [false; CARD_SLOT_COUNT],
        partinfos: [0; CARD_SLOT_COUNT],
        part_count: 0,
    };
}

/// In-memory model of the 15-slot browser card and its asynchronous flags.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VirtualCard {
    slots: [Slot; CARD_SLOT_COUNT],
    published: CardSnapshot,
    staged: Option<CardSnapshot>,
    current_slot: Option<usize>,
    scan_ticks: u8,
    flags: CardFlags,
    storage_available: bool,
}

impl VirtualCard {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: [Slot::Empty; CARD_SLOT_COUNT],
            published: CardSnapshot::EMPTY,
            staged: None,
            current_slot: None,
            scan_ticks: 0,
            flags: CardFlags(0),
            storage_available: true,
        }
    }

    #[must_use]
    pub const fn flags(&self) -> CardFlags {
        self.flags
    }

    #[must_use]
    pub const fn part_count(&self) -> usize {
        self.published.part_count
    }

    /// Physical slot selected by the most recent successful load or save.
    #[must_use]
    pub const fn current_slot(&self) -> Option<usize> {
        self.current_slot
    }

    #[must_use]
    pub const fn partinfos(&self) -> &[u32; CARD_SLOT_COUNT] {
        &self.published.partinfos
    }

    /// Returns one coherent snapshot for the GOOL global publication phase.
    #[must_use]
    pub const fn published_state(&self) -> CardPublishedState {
        CardPublishedState {
            flags: self.flags,
            part_count: self.published.part_count as u32,
            partinfos: self.published.partinfos,
        }
    }

    #[must_use]
    pub const fn slots(&self) -> &[Slot; CARD_SLOT_COUNT] {
        &self.slots
    }

    /// Controls whether the modeled backing store can service card operations.
    ///
    /// Browser storage failures happen outside the retail payload format. Keeping
    /// this transport state separate lets the synchronous GOOL handshake expose
    /// the native failure flags without inventing an on-card slot encoding.
    pub fn set_storage_available(&mut self, available: bool) {
        self.storage_available = available;
    }

    /// Imports a physical slot. Call `Rescan` before expecting published parts.
    pub fn set_slot(&mut self, slot: usize, value: Slot) -> Result<(), CardError> {
        let destination = self.slots.get_mut(slot).ok_or(CardError::InvalidPart)?;
        *destination = value;
        Ok(())
    }

    pub fn control(
        &mut self,
        operation: CardOperation,
        part_index: usize,
        current: Option<SaveData>,
    ) -> Result<CardOutcome, CardError> {
        match operation {
            CardOperation::Unsupported(_) => Err(CardError::UnsupportedOperation),
            CardOperation::ClearFlag6 => {
                self.flags.remove(CardFlags::FLAG_6);
                if self.staged.is_some() && !self.flags.contains(CardFlags::CHECKING) {
                    self.finish_rescan();
                }
                Ok(CardOutcome::Complete)
            }
            CardOperation::SaveSelected => {
                self.reject_if_busy()?;
                let data = current.ok_or_else(|| {
                    self.set_failure(false);
                    CardError::MissingSaveData
                })?;
                let slot = self.resolve_write_slot(part_index)?;
                self.save_slot(slot, data)
            }
            CardOperation::LoadSelected => {
                self.reject_if_busy()?;
                let slot = self.resolve_read_slot(part_index)?;
                self.flags.insert(CardFlags::PENDING);
                if !self.storage_available {
                    self.set_failure(false);
                    return Err(CardError::StorageUnavailable);
                }
                match self.slots[slot] {
                    Slot::Valid(payload) => {
                        let Ok(data) = payload.decode() else {
                            self.set_failure(true);
                            return Err(CardError::CorruptSlot);
                        };
                        self.current_slot = Some(slot);
                        self.operation_success(false);
                        Ok(CardOutcome::Loaded(data))
                    }
                    Slot::Corrupt => {
                        self.set_failure(true);
                        Err(CardError::CorruptSlot)
                    }
                    Slot::Empty => {
                        self.set_failure(false);
                        Err(CardError::CorruptSlot)
                    }
                }
            }
            CardOperation::Format => {
                if self.flags.contains(CardFlags::PENDING)
                    || self.flags.contains(CardFlags::CHECKING)
                {
                    self.flags.insert(CardFlags::ERROR);
                    return Err(CardError::Busy);
                }
                self.flags.insert(CardFlags::PENDING);
                if !self.storage_available {
                    self.set_failure(false);
                    return Err(CardError::StorageUnavailable);
                }
                self.slots = [Slot::Empty; CARD_SLOT_COUNT];
                self.current_slot = None;
                self.staged = None;
                self.scan_ticks = 0;
                self.published = CardSnapshot::EMPTY;
                self.flags = CardFlags(self.flags.0 & CardFlags::NEW_DEVICE.0);
                self.flags.insert(CardFlags::CHECK_NEEDED);
                Ok(CardOutcome::Complete)
            }
            CardOperation::SaveCurrent => {
                self.reject_if_busy()?;
                let slot = self.current_slot.ok_or_else(|| {
                    self.set_failure(false);
                    CardError::NoCurrentSlot
                })?;
                let data = current.ok_or_else(|| {
                    self.set_failure(false);
                    CardError::MissingSaveData
                })?;
                self.save_slot(slot, data)
            }
            CardOperation::ProbeName => Err(CardError::NameProbeUnsupported),
            CardOperation::ProbePresent => Ok(CardOutcome::Complete),
            CardOperation::ForgetCurrent => {
                self.current_slot = None;
                Ok(CardOutcome::Complete)
            }
            CardOperation::Rescan => {
                self.staged = None;
                self.scan_ticks = 0;
                self.published = CardSnapshot::EMPTY;
                if !self.storage_available {
                    self.current_slot = None;
                    self.set_failure(true);
                    return Err(CardError::StorageUnavailable);
                }
                self.staged = Some(self.build_snapshot());
                self.flags = CardFlags(self.flags.0 & CardFlags::NEW_DEVICE.0);
                self.flags.insert(CardFlags::PENDING);
                self.flags.insert(CardFlags::CHECKING);
                self.flags.insert(CardFlags::FLAG_6);
                Ok(CardOutcome::Complete)
            }
        }
    }

    /// Advances the retail rescan handshake once per simulation frame.
    pub fn update(&mut self) {
        if self.staged.is_none() {
            return;
        }
        if self.flags.contains(CardFlags::CHECKING) {
            let previous = self.scan_ticks;
            self.scan_ticks = self.scan_ticks.saturating_add(1);
            if previous == 0 {
                return;
            }
            self.flags.remove(CardFlags::CHECKING);
        }
        if !self.flags.contains(CardFlags::FLAG_6) {
            self.finish_rescan();
        }
    }

    fn reject_if_busy(&mut self) -> Result<(), CardError> {
        if self.flags.contains(CardFlags::PENDING)
            || self.flags.contains(CardFlags::CHECK_NEEDED)
            || self.flags.contains(CardFlags::CHECKING)
        {
            self.flags.insert(CardFlags::ERROR);
            Err(CardError::Busy)
        } else {
            Ok(())
        }
    }

    fn resolve_read_slot(&mut self, part_index: usize) -> Result<usize, CardError> {
        self.published
            .slot_map
            .get(part_index)
            .copied()
            .flatten()
            .ok_or_else(|| {
                self.set_failure(false);
                CardError::InvalidPart
            })
    }

    fn resolve_write_slot(&mut self, part_index: usize) -> Result<usize, CardError> {
        if part_index < self.published.part_count {
            return self.published.slot_map[part_index].ok_or(CardError::InvalidPart);
        }
        if part_index >= CARD_SLOT_COUNT {
            self.set_failure(false);
            return Err(CardError::InvalidPart);
        }
        self.published
            .slot_valid
            .iter()
            .position(|valid| !valid)
            .ok_or_else(|| {
                self.set_failure(false);
                CardError::CardFull
            })
    }

    fn save_slot(&mut self, slot: usize, data: SaveData) -> Result<CardOutcome, CardError> {
        self.flags.insert(CardFlags::PENDING);
        if !self.storage_available {
            self.set_failure(false);
            return Err(CardError::StorageUnavailable);
        }
        self.slots[slot] = Slot::Valid(CardPayload::encode(data));
        self.current_slot = Some(slot);
        self.published = self.build_snapshot();
        self.operation_success(true);
        Ok(CardOutcome::Complete)
    }

    fn build_snapshot(&self) -> CardSnapshot {
        let mut snapshot = CardSnapshot::EMPTY;
        for (slot, value) in self.slots.iter().copied().enumerate() {
            match value {
                Slot::Empty => {}
                Slot::Corrupt => {
                    let part = snapshot.part_count;
                    snapshot.slot_map[part] = Some(slot);
                    snapshot.partinfos[part] = 1 | (1 << 1);
                    snapshot.part_count += 1;
                }
                Slot::Valid(payload) => {
                    let part = snapshot.part_count;
                    if !payload.is_valid() {
                        snapshot.slot_map[part] = Some(slot);
                        snapshot.partinfos[part] = 1 | (1 << 1);
                        snapshot.part_count += 1;
                        continue;
                    }
                    let progress = read_u32(payload.as_bytes(), PROGRESS_OFFSET);
                    snapshot.slot_valid[slot] = true;
                    snapshot.slot_map[part] = Some(slot);
                    snapshot.partinfos[part] = 1 | 8 | progress.wrapping_shl(5) | 0x2_0000;
                    snapshot.part_count += 1;
                }
            }
        }
        snapshot
    }

    fn operation_success(&mut self, clear_new_device: bool) {
        self.staged = None;
        self.scan_ticks = 0;
        self.flags = if clear_new_device {
            CardFlags(0)
        } else {
            CardFlags(self.flags.0 & CardFlags::NEW_DEVICE.0)
        };
    }

    fn set_failure(&mut self, check_needed: bool) {
        self.staged = None;
        self.scan_ticks = 0;
        self.flags = CardFlags(self.flags.0 & CardFlags::NEW_DEVICE.0);
        self.flags.insert(CardFlags::ERROR);
        if check_needed {
            self.flags.insert(CardFlags::CHECK_NEEDED);
        }
    }

    fn finish_rescan(&mut self) {
        if let Some(snapshot) = self.staged.take() {
            self.published = snapshot;
        }
        if self
            .current_slot
            .is_some_and(|slot| !self.published.slot_valid[slot])
        {
            self.current_slot = None;
        }
        self.scan_ticks = 0;
        self.flags = CardFlags(self.flags.0 & CardFlags::NEW_DEVICE.0);
    }
}

impl Default for VirtualCard {
    fn default() -> Self {
        Self::new()
    }
}

/// Persistence-facing resume value, before schema/length/checksum validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredResume {
    pub schema: String,
    pub version: u32,
    pub payload: Vec<u8>,
}

impl StoredResume {
    #[must_use]
    pub fn valid(payload: CardPayload) -> Self {
        Self {
            schema: RESUME_SCHEMA.to_owned(),
            version: RESUME_VERSION,
            payload: payload.as_bytes().to_vec(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResumeLoadResult {
    Empty,
    Loaded(SaveData),
    Corrupt,
    NewerVersion,
}

/// Automatic progression/options resume with exact 30-frame write throttling.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResumeManager {
    enabled: bool,
    ticks: u8,
    last_payload: CardPayload,
    title_payload: Option<CardPayload>,
    quarantined: Vec<StoredResume>,
}

impl ResumeManager {
    /// Loads a persistence record and establishes the dirty-comparison baseline.
    #[must_use]
    pub fn load(record: Option<StoredResume>, current: SaveData) -> (Self, ResumeLoadResult) {
        let fallback = CardPayload::encode(current);
        match record {
            None => (
                Self {
                    enabled: true,
                    ticks: 0,
                    last_payload: fallback,
                    title_payload: None,
                    quarantined: Vec::new(),
                },
                ResumeLoadResult::Empty,
            ),
            Some(record) if record.schema == RESUME_SCHEMA && record.version > RESUME_VERSION => (
                Self {
                    enabled: false,
                    ticks: 0,
                    last_payload: fallback,
                    title_payload: None,
                    quarantined: Vec::new(),
                },
                ResumeLoadResult::NewerVersion,
            ),
            Some(record) => {
                let parsed = <[u8; CARD_PAYLOAD_SIZE]>::try_from(record.payload.as_slice())
                    .ok()
                    .and_then(|bytes| CardPayload::from_bytes(bytes).ok());
                if record.schema == RESUME_SCHEMA
                    && record.version == RESUME_VERSION
                    && let Some(payload) = parsed
                    && let Ok(data) = payload.decode()
                {
                    return (
                        Self {
                            enabled: true,
                            ticks: 0,
                            last_payload: payload,
                            title_payload: None,
                            quarantined: Vec::new(),
                        },
                        ResumeLoadResult::Loaded(data),
                    );
                }
                (
                    Self {
                        enabled: true,
                        ticks: 0,
                        last_payload: fallback,
                        title_payload: None,
                        quarantined: vec![record],
                    },
                    ResumeLoadResult::Corrupt,
                )
            }
        }
    }

    #[must_use]
    pub fn quarantined(&self) -> &[StoredResume] {
        &self.quarantined
    }

    /// Returns a changed payload once every 30 simulation frames.
    pub fn update(&mut self, current: SaveData) -> Option<CardPayload> {
        if !self.enabled {
            return None;
        }
        self.ticks = self.ticks.saturating_add(1);
        if self.ticks < 30 {
            return None;
        }
        self.ticks = 0;
        self.flush(current)
    }

    /// Flushes immediately when the payload differs from the last write.
    pub fn flush(&mut self, current: SaveData) -> Option<CardPayload> {
        if !self.enabled {
            return None;
        }
        let payload = CardPayload::encode(current);
        if payload == self.last_payload {
            None
        } else {
            self.last_payload = payload;
            Some(payload)
        }
    }

    /// Protects progression from the main-menu global reset.
    pub fn before_title_reset(&mut self, current: SaveData) -> Option<CardPayload> {
        self.title_payload = Some(CardPayload::encode(current));
        self.flush(current)
    }

    /// Returns the protected state after the main-menu reset.
    pub fn after_title_reset(&mut self) -> Option<SaveData> {
        self.title_payload
            .take()
            .and_then(|payload| payload.decode().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn sample_data() -> SaveData {
        SaveData {
            level_count: 7,
            initial_lives: 4 << 8,
            unknown_6190c: 0x1122_3344,
            mono: true,
            sfx_volume: 211,
            music_volume: 199,
            item_pool_1: 0x1020_3040,
            item_pool_2: 0x5060_7080,
            gem_count: 3,
            key_count: 2,
        }
    }

    fn finish_rescan(card: &mut VirtualCard) {
        card.update();
        card.update();
        card.control(CardOperation::ClearFlag6, 0, None).unwrap();
    }

    fn scan_and_finish(card: &mut VirtualCard) {
        card.control(CardOperation::Rescan, 0, None).unwrap();
        finish_rescan(card);
    }

    #[test]
    fn payload_layout_and_checksum_match_source_contract() {
        let data = sample_data();
        let payload = CardPayload::encode(data);
        assert_eq!(payload.as_bytes().len(), 128);
        assert_eq!(read_u32(payload.as_bytes(), 0), (2 << 10) | (3 << 5) | 7);
        assert_eq!(read_u32(payload.as_bytes(), 4), 7);
        assert_eq!(read_u32(payload.as_bytes(), 8), 4 << 8);
        assert!(payload.is_valid());
        assert_eq!(payload.decode(), Ok(data));
    }

    #[test]
    fn storage_identity_and_retail_payload_size_are_version_stable() {
        assert_eq!(CARD_SLOT_COUNT, 15);
        assert_eq!(CARD_PAYLOAD_SIZE, 128);
        assert_eq!(CARD_STORAGE_KEY, "c1.virtual-memory-card.v1");
        assert_eq!(CARD_SCHEMA, "c1-virtual-memory-card");
        assert_eq!(CARD_VERSION, 1);
    }

    #[test]
    fn unsupported_probe_name_and_probe_present_do_not_mutate_card_state() {
        let mut card = VirtualCard::new();
        card.set_slot(3, Slot::Valid(CardPayload::encode(sample_data())))
            .unwrap();
        card.flags = CardFlags(CardFlags::NEW_DEVICE.bits() | CardFlags::ERROR.bits());
        card.control(CardOperation::Rescan, 0, None).unwrap();

        for (operation, expected) in [
            (
                CardOperation::Unsupported(0),
                Err(CardError::UnsupportedOperation),
            ),
            (
                CardOperation::Unsupported(11),
                Err(CardError::UnsupportedOperation),
            ),
            (
                CardOperation::ProbeName,
                Err(CardError::NameProbeUnsupported),
            ),
            (CardOperation::ProbePresent, Ok(CardOutcome::Complete)),
        ] {
            let before = card.clone();
            assert_eq!(card.control(operation, usize::MAX, None), expected);
            assert_eq!(card, before);
        }
    }

    #[test]
    fn clear_flag_6_only_clears_the_latch_without_an_active_scan() {
        let mut card = VirtualCard::new();
        card.flags = CardFlags(0x3f);
        assert_eq!(
            card.control(CardOperation::ClearFlag6, 0, None),
            Ok(CardOutcome::Complete)
        );
        assert_eq!(card.flags().bits(), 0x1f);
    }

    #[test]
    fn rescan_exposes_retail_flag_sequence() {
        let mut card = VirtualCard::new();
        card.set_slot(2, Slot::Valid(CardPayload::encode(sample_data())))
            .unwrap();
        card.flags.insert(CardFlags::NEW_DEVICE);
        assert_eq!(
            card.control(CardOperation::Rescan, 0, None),
            Ok(CardOutcome::Complete)
        );
        assert_eq!(card.flags().bits(), 0x39);
        assert_eq!(card.part_count(), 0);
        card.update();
        assert_eq!(card.flags().bits(), 0x39);
        assert_eq!(card.part_count(), 0);
        card.update();
        assert_eq!(card.flags().bits(), 0x31);
        assert_eq!(card.part_count(), 0);
        assert_eq!(
            card.control(CardOperation::ClearFlag6, 0, None),
            Ok(CardOutcome::Complete)
        );
        assert_eq!(card.flags().bits(), CardFlags::NEW_DEVICE.bits());
        assert_eq!(card.part_count(), 1);
    }

    #[test]
    fn clear_latch_waits_for_checking_then_publishes_in_update() {
        let mut card = VirtualCard::new();
        card.set_slot(5, Slot::Valid(CardPayload::encode(sample_data())))
            .unwrap();
        card.control(CardOperation::Rescan, 0, None).unwrap();
        card.update();
        card.control(CardOperation::ClearFlag6, 0, None).unwrap();
        assert_eq!(card.flags().bits(), 0x09);
        assert_eq!(card.part_count(), 0);
        card.update();
        assert_eq!(card.flags().bits(), 0);
        assert_eq!(card.part_count(), 1);
    }

    #[test]
    fn a_new_rescan_cancels_the_prior_snapshot_and_publishes_only_the_latest() {
        let first = sample_data();
        let mut second = first;
        second.level_count += 1;
        let mut card = VirtualCard::new();
        card.set_slot(0, Slot::Valid(CardPayload::encode(first)))
            .unwrap();
        card.control(CardOperation::Rescan, 0, None).unwrap();
        card.update();
        card.set_slot(0, Slot::Valid(CardPayload::encode(second)))
            .unwrap();
        card.control(CardOperation::Rescan, 0, None).unwrap();
        assert_eq!(card.part_count(), 0);
        finish_rescan(&mut card);
        assert_eq!(
            card.control(CardOperation::LoadSelected, 0, None),
            Ok(CardOutcome::Loaded(second))
        );
    }

    #[test]
    fn rescan_storage_failure_clears_metadata_and_sets_exact_failure_flags() {
        let data = sample_data();
        let mut card = VirtualCard::new();
        card.set_slot(0, Slot::Valid(CardPayload::encode(data)))
            .unwrap();
        scan_and_finish(&mut card);
        card.control(CardOperation::LoadSelected, 0, None).unwrap();
        assert_eq!(card.current_slot(), Some(0));
        card.flags.insert(CardFlags::NEW_DEVICE);
        card.set_storage_available(false);

        assert_eq!(
            card.control(CardOperation::Rescan, 0, None),
            Err(CardError::StorageUnavailable)
        );
        assert_eq!(card.flags().bits(), 0x16);
        assert_eq!(card.part_count(), 0);
        assert_eq!(card.partinfos(), &[0; CARD_SLOT_COUNT]);
        assert_eq!(card.current_slot(), None);
        card.update();
        assert_eq!(card.flags().bits(), 0x16);
    }

    #[test]
    fn save_and_load_busy_gates_match_retail_flags() {
        for operation in [
            CardOperation::SaveSelected,
            CardOperation::LoadSelected,
            CardOperation::SaveCurrent,
        ] {
            for blocker in [
                CardFlags::PENDING,
                CardFlags::CHECK_NEEDED,
                CardFlags::CHECKING,
            ] {
                let mut card = VirtualCard::new();
                card.flags = CardFlags(CardFlags::NEW_DEVICE.bits() | blocker.bits());
                let before_slots = card.slots;
                let current = if matches!(operation, CardOperation::LoadSelected) {
                    None
                } else {
                    Some(sample_data())
                };
                assert_eq!(card.control(operation, 0, current), Err(CardError::Busy));
                assert_eq!(
                    card.flags().bits(),
                    CardFlags::NEW_DEVICE.bits() | blocker.bits() | CardFlags::ERROR.bits()
                );
                assert_eq!(card.slots, before_slots);
                assert_eq!(card.part_count(), 0);
            }
        }
    }

    #[test]
    fn save_selected_uses_mapped_then_first_unused_slots_and_clears_all_flags() {
        let original = sample_data();
        let mut replacement = original;
        replacement.level_count = 9;
        let mut inserted = original;
        inserted.level_count = 10;
        let mut card = VirtualCard::new();
        card.set_slot(0, Slot::Corrupt).unwrap();
        card.set_slot(2, Slot::Valid(CardPayload::encode(original)))
            .unwrap();
        scan_and_finish(&mut card);
        assert_eq!(card.part_count(), 2);

        card.flags = CardFlags(CardFlags::NEW_DEVICE.bits() | CardFlags::ERROR.bits());
        card.control(CardOperation::SaveSelected, 1, Some(replacement))
            .unwrap();
        assert_eq!(card.flags().bits(), 0);
        assert_eq!(card.current_slot(), Some(2));
        assert_eq!(card.part_count(), 2);

        card.flags.insert(CardFlags::NEW_DEVICE);
        card.control(CardOperation::SaveSelected, 2, Some(inserted))
            .unwrap();
        assert_eq!(card.flags().bits(), 0);
        assert_eq!(card.current_slot(), Some(0));
        assert_eq!(card.part_count(), 2);
        assert!(matches!(card.slots()[0], Slot::Valid(_)));
        assert!(matches!(card.slots()[2], Slot::Valid(_)));
    }

    #[test]
    fn save_failures_preserve_only_new_device_and_add_error() {
        let cases = [
            (
                CardOperation::SaveSelected,
                CARD_SLOT_COUNT,
                Some(sample_data()),
                true,
                CardError::InvalidPart,
            ),
            (
                CardOperation::SaveSelected,
                0,
                None,
                true,
                CardError::MissingSaveData,
            ),
            (
                CardOperation::SaveSelected,
                0,
                Some(sample_data()),
                false,
                CardError::StorageUnavailable,
            ),
        ];

        for (operation, part, data, available, expected) in cases {
            let mut card = VirtualCard::new();
            card.flags = CardFlags(CardFlags::NEW_DEVICE.bits() | CardFlags::ERROR.bits());
            card.set_storage_available(available);
            assert_eq!(card.control(operation, part, data), Err(expected));
            assert_eq!(card.flags().bits(), 0x12);
            assert_eq!(card.current_slot(), None);
            assert_eq!(card.part_count(), 0);
            assert!(card.slots().iter().all(|slot| *slot == Slot::Empty));
        }
    }

    #[test]
    fn load_success_preserves_only_new_device() {
        let data = sample_data();
        let mut card = VirtualCard::new();
        card.set_slot(6, Slot::Valid(CardPayload::encode(data)))
            .unwrap();
        scan_and_finish(&mut card);
        card.flags = CardFlags(CardFlags::NEW_DEVICE.bits() | CardFlags::ERROR.bits());
        assert_eq!(
            card.control(CardOperation::LoadSelected, 0, None),
            Ok(CardOutcome::Loaded(data))
        );
        assert_eq!(card.flags().bits(), CardFlags::NEW_DEVICE.bits());
        assert_eq!(card.current_slot(), Some(6));
    }

    #[test]
    fn load_distinguishes_corrupt_checksum_empty_and_transport_failures() {
        let data = sample_data();

        let mut corrupt = VirtualCard::new();
        corrupt.set_slot(0, Slot::Corrupt).unwrap();
        scan_and_finish(&mut corrupt);
        corrupt.flags.insert(CardFlags::NEW_DEVICE);
        assert_eq!(
            corrupt.control(CardOperation::LoadSelected, 0, None),
            Err(CardError::CorruptSlot)
        );
        assert_eq!(corrupt.flags().bits(), 0x16);

        let mut bytes = CardPayload::encode(data).into_bytes();
        bytes[MONO_OFFSET] ^= 1;
        let mut bad_checksum = VirtualCard::new();
        bad_checksum
            .set_slot(1, Slot::Valid(CardPayload(bytes)))
            .unwrap();
        scan_and_finish(&mut bad_checksum);
        assert_eq!(bad_checksum.partinfos()[0], 3);
        bad_checksum.flags.insert(CardFlags::NEW_DEVICE);
        assert_eq!(
            bad_checksum.control(CardOperation::LoadSelected, 0, None),
            Err(CardError::CorruptSlot)
        );
        assert_eq!(bad_checksum.flags().bits(), 0x16);

        let mut disappeared = VirtualCard::new();
        disappeared
            .set_slot(3, Slot::Valid(CardPayload::encode(data)))
            .unwrap();
        scan_and_finish(&mut disappeared);
        disappeared.set_slot(3, Slot::Empty).unwrap();
        disappeared.flags.insert(CardFlags::NEW_DEVICE);
        assert_eq!(
            disappeared.control(CardOperation::LoadSelected, 0, None),
            Err(CardError::CorruptSlot)
        );
        assert_eq!(disappeared.flags().bits(), 0x12);

        let mut unavailable = VirtualCard::new();
        unavailable
            .set_slot(4, Slot::Valid(CardPayload::encode(data)))
            .unwrap();
        scan_and_finish(&mut unavailable);
        unavailable.flags.insert(CardFlags::NEW_DEVICE);
        unavailable.set_storage_available(false);
        assert_eq!(
            unavailable.control(CardOperation::LoadSelected, 0, None),
            Err(CardError::StorageUnavailable)
        );
        assert_eq!(unavailable.flags().bits(), 0x12);
    }

    #[test]
    fn format_gate_allows_check_needed_and_success_clears_card_metadata() {
        let data = sample_data();
        let mut card = VirtualCard::new();
        card.control(CardOperation::SaveSelected, 0, Some(data))
            .unwrap();
        card.flags = CardFlags(
            CardFlags::NEW_DEVICE.bits() | CardFlags::CHECK_NEEDED.bits() | CardFlags::ERROR.bits(),
        );
        assert_eq!(
            card.control(CardOperation::Format, 0, None),
            Ok(CardOutcome::Complete)
        );
        assert_eq!(card.flags().bits(), 0x14);
        assert_eq!(card.current_slot(), None);
        assert_eq!(card.part_count(), 0);
        assert_eq!(card.partinfos(), &[0; CARD_SLOT_COUNT]);
        assert!(card.slots().iter().all(|slot| *slot == Slot::Empty));
    }

    #[test]
    fn format_is_busy_only_for_pending_or_checking() {
        for blocker in [CardFlags::PENDING, CardFlags::CHECKING] {
            let mut card = VirtualCard::new();
            card.control(CardOperation::SaveSelected, 0, Some(sample_data()))
                .unwrap();
            card.flags = CardFlags(
                CardFlags::NEW_DEVICE.bits() | CardFlags::CHECK_NEEDED.bits() | blocker.bits(),
            );
            let before = card.clone();
            assert_eq!(
                card.control(CardOperation::Format, 0, None),
                Err(CardError::Busy)
            );
            assert_eq!(
                card.flags().bits(),
                before.flags().bits() | CardFlags::ERROR.bits()
            );
            assert_eq!(card.slots(), before.slots());
            assert_eq!(card.partinfos(), before.partinfos());
            assert_eq!(card.current_slot(), before.current_slot());
        }
    }

    #[test]
    fn format_storage_failure_preserves_content_and_sets_new_device_error() {
        let data = sample_data();
        let mut card = VirtualCard::new();
        card.control(CardOperation::SaveSelected, 0, Some(data))
            .unwrap();
        card.flags = CardFlags(CardFlags::NEW_DEVICE.bits() | CardFlags::CHECK_NEEDED.bits());
        card.set_storage_available(false);
        let before_slots = card.slots;
        let before_partinfos = card.published.partinfos;

        assert_eq!(
            card.control(CardOperation::Format, 0, None),
            Err(CardError::StorageUnavailable)
        );
        assert_eq!(card.flags().bits(), 0x12);
        assert_eq!(card.slots, before_slots);
        assert_eq!(card.published.partinfos, before_partinfos);
        assert_eq!(card.current_slot(), Some(0));
    }

    #[test]
    fn save_current_requires_current_then_data_and_clears_all_flags_on_success() {
        let data = sample_data();
        let mut no_current = VirtualCard::new();
        no_current.flags.insert(CardFlags::NEW_DEVICE);
        assert_eq!(
            no_current.control(CardOperation::SaveCurrent, 0, None),
            Err(CardError::NoCurrentSlot)
        );
        assert_eq!(no_current.flags().bits(), 0x12);

        let mut card = VirtualCard::new();
        card.control(CardOperation::SaveSelected, 0, Some(data))
            .unwrap();
        card.flags.insert(CardFlags::NEW_DEVICE);
        assert_eq!(
            card.control(CardOperation::SaveCurrent, 0, None),
            Err(CardError::MissingSaveData)
        );
        assert_eq!(card.flags().bits(), 0x12);

        let mut changed = data;
        changed.level_count += 1;
        card.control(CardOperation::SaveCurrent, 0, Some(changed))
            .unwrap();
        assert_eq!(card.flags().bits(), 0);
        assert_eq!(
            card.control(CardOperation::LoadSelected, 0, None),
            Ok(CardOutcome::Loaded(changed))
        );
    }

    #[test]
    fn forget_current_clears_only_the_selected_handle() {
        let mut card = VirtualCard::new();
        card.control(CardOperation::SaveSelected, 0, Some(sample_data()))
            .unwrap();
        card.flags = CardFlags(CardFlags::NEW_DEVICE.bits() | CardFlags::ERROR.bits());
        let slots = card.slots;
        let published = card.published;
        assert_eq!(
            card.control(CardOperation::ForgetCurrent, usize::MAX, None),
            Ok(CardOutcome::Complete)
        );
        assert_eq!(card.current_slot(), None);
        assert_eq!(card.flags().bits(), 0x12);
        assert_eq!(card.slots, slots);
        assert_eq!(card.published, published);
    }

    #[test]
    fn all_fifteen_physical_slots_are_scanned_in_order() {
        let mut card = VirtualCard::new();
        for slot in 0..CARD_SLOT_COUNT {
            let mut data = sample_data();
            data.level_count = u32::try_from(slot).unwrap();
            card.set_slot(slot, Slot::Valid(CardPayload::encode(data)))
                .unwrap();
        }
        scan_and_finish(&mut card);
        assert_eq!(card.part_count(), CARD_SLOT_COUNT);
        assert!(card.partinfos().iter().all(|part| *part & 9 == 9));
        assert_eq!(
            card.set_slot(CARD_SLOT_COUNT, Slot::Empty),
            Err(CardError::InvalidPart)
        );
    }

    #[test]
    fn save_load_current_and_format_match_golden_test() {
        let mut card = VirtualCard::new();
        card.control(CardOperation::Rescan, 0, None).unwrap();
        card.update();
        card.control(CardOperation::ClearFlag6, 0, None).unwrap();
        card.update();
        let data = sample_data();
        card.control(CardOperation::SaveSelected, 0, Some(data))
            .unwrap();
        assert_eq!(card.part_count(), 1);
        let progress = (2_u32 << 10) | (3 << 5) | 7;
        assert_eq!(card.partinfos()[0], 1 | 8 | (progress << 5) | 0x2_0000);
        assert_eq!(
            card.control(CardOperation::LoadSelected, 0, None),
            Ok(CardOutcome::Loaded(data))
        );

        let mut changed = data;
        changed.level_count = 9;
        card.control(CardOperation::SaveCurrent, 0, Some(changed))
            .unwrap();
        assert_eq!(
            card.control(CardOperation::LoadSelected, 0, None),
            Ok(CardOutcome::Loaded(changed))
        );

        card.control(CardOperation::Format, 0, None).unwrap();
        assert_eq!(card.part_count(), 0);
        assert_eq!(card.flags(), CardFlags::CHECK_NEEDED);
    }

    #[test]
    fn rescanned_card_can_overwrite_selected_slot_without_a_current_handle() {
        let original = sample_data();
        let mut card = VirtualCard::new();
        card.set_slot(0, Slot::Valid(CardPayload::encode(original)))
            .unwrap();
        card.control(CardOperation::Rescan, 0, None).unwrap();
        card.update();
        card.control(CardOperation::ClearFlag6, 0, None).unwrap();
        card.update();

        assert_eq!(card.part_count(), 1);
        assert_eq!(card.current_slot(), None);

        let mut completed = original;
        completed.level_count += 1;
        card.control(CardOperation::SaveSelected, 0, Some(completed))
            .unwrap();
        assert_eq!(card.current_slot(), Some(0));
        assert_eq!(
            card.control(CardOperation::LoadSelected, 0, None),
            Ok(CardOutcome::Loaded(completed))
        );
    }

    #[test]
    fn save_current_updates_the_loaded_physical_slot_in_a_multi_slot_card() {
        let first = sample_data();
        let mut second = first;
        second.level_count = 12;
        let mut card = VirtualCard::new();
        card.set_slot(2, Slot::Valid(CardPayload::encode(first)))
            .unwrap();
        card.set_slot(7, Slot::Valid(CardPayload::encode(second)))
            .unwrap();
        card.control(CardOperation::Rescan, 0, None).unwrap();
        card.update();
        card.control(CardOperation::ClearFlag6, 0, None).unwrap();
        card.update();

        assert_eq!(
            card.control(CardOperation::LoadSelected, 1, None),
            Ok(CardOutcome::Loaded(second))
        );
        assert_eq!(card.current_slot(), Some(7));

        let mut completed = second;
        completed.level_count = 13;
        card.control(CardOperation::SaveCurrent, 0, Some(completed))
            .unwrap();
        assert_eq!(
            card.control(CardOperation::LoadSelected, 0, None),
            Ok(CardOutcome::Loaded(first))
        );
        assert_eq!(
            card.control(CardOperation::LoadSelected, 1, None),
            Ok(CardOutcome::Loaded(completed))
        );
    }

    #[test]
    fn corrupt_slot_is_published_but_not_loadable() {
        let mut card = VirtualCard::new();
        card.set_slot(4, Slot::Corrupt).unwrap();
        card.control(CardOperation::Rescan, 0, None).unwrap();
        card.update();
        card.update();
        card.control(CardOperation::ClearFlag6, 0, None).unwrap();
        assert_eq!(card.part_count(), 1);
        assert_eq!(card.partinfos()[0], 3);
        assert_eq!(
            card.control(CardOperation::LoadSelected, 0, None),
            Err(CardError::CorruptSlot)
        );
        assert!(card.flags().contains(CardFlags::CHECK_NEEDED));
    }

    #[test]
    fn resume_throttles_quarantines_and_protects_title_reset() {
        let data = sample_data();
        let corrupt = StoredResume {
            schema: RESUME_SCHEMA.to_owned(),
            version: 1,
            payload: vec![1, 2, 3],
        };
        let (manager, result) = ResumeManager::load(Some(corrupt), data);
        assert_eq!(result, ResumeLoadResult::Corrupt);
        assert_eq!(manager.quarantined().len(), 1);

        let (mut manager, _) = ResumeManager::load(None, data);
        let mut changed = data;
        changed.level_count += 1;
        for _ in 0..29 {
            assert_eq!(manager.update(changed), None);
        }
        assert!(manager.update(changed).is_some());
        manager.before_title_reset(changed);
        assert_eq!(manager.after_title_reset(), Some(changed));
    }

    proptest! {
        #[test]
        fn payload_round_trip(
            level_count in any::<u32>(), initial_lives in any::<u32>(), unknown in any::<u32>(),
            mono in any::<bool>(), sfx in any::<u32>(), music in any::<u32>(),
            pool1 in any::<u32>(), pool2 in any::<u32>(), gems in 0_u8..32, keys in 0_u32..0x0040_0000,
        ) {
            let data = SaveData {
                level_count, initial_lives, unknown_6190c: unknown, mono,
                sfx_volume: sfx, music_volume: music, item_pool_1: pool1,
                item_pool_2: pool2, gem_count: gems, key_count: keys,
            };
            let decoded = CardPayload::encode(data).decode().unwrap();
            prop_assert_eq!(decoded, data);
        }

        #[test]
        fn mutating_a_non_checksum_byte_is_detected(index in 0_usize..124, value in any::<u8>()) {
            let payload = CardPayload::encode(sample_data());
            let mut bytes = payload.into_bytes();
            prop_assume!(bytes[index] != value);
            bytes[index] = value;
            prop_assert_eq!(CardPayload::from_bytes(bytes), Err(PayloadError::ChecksumMismatch));
        }
    }
}
