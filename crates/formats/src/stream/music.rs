//! Bounds-checked retail INST/VAB and PlayStation SEP sequence formats.
//!
//! The two families deliberately use different byte orders. C1's INST and
//! VAB records are native little-endian PlayStation structs. SEP retains the
//! big-endian sequence fields and MIDI variable-length timing used by Sony's
//! sequence tools. Keeping the parsers separate prevents host endianness or a
//! packed C bitfield layout from leaking into the runtime.

use crate::binary::{FormatError, Reader};

use super::{Entry, structs::MidiHeader};

/// Retail subsystem index for a MIDI entry.
pub const MIDI_ENTRY_TYPE: u32 = 13;
/// Retail subsystem index for an instrument-body fragment.
pub const INST_ENTRY_TYPE: u32 = 14;
/// Native `EID_NONE`, used in unused slots of a MIDI instrument list.
pub const EID_NONE_RAW: u32 = 0x6396_347f;

const INST_HEADER_LEN: usize = 8;
const VAB_HEADER_LEN: usize = 32;
const VAB_PROGRAM_COUNT: usize = 128;
const VAB_PROGRAM_LEN: usize = 16;
const VAB_TONES_PER_PROGRAM: usize = 16;
const VAB_TONE_LEN: usize = 32;
const VAB_WAVE_CAPACITY: usize = 255;
const VAB_WAVE_TABLE_LEN: usize = 2 + VAB_WAVE_CAPACITY * 2;
const VAB_MAGIC: &[u8; 4] = b"pBAV";
const SEP_MAGIC: &[u8; 4] = b"pQES";
const SEP_HEADER_LEN: usize = 6;
const SEQUENCE_HEADER_LEN: usize = 13;
const MAX_SEQUENCES: usize = 7;
const MAX_VLQ_BYTES: usize = 4;

/// One validated, borrowed body fragment from an INST entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InstrumentFragment<'a> {
    pub part_index: u8,
    pub body: &'a [u8],
}

impl<'a> InstrumentFragment<'a> {
    /// Parses the exact two-item INST contract without retaining a copy of the
    /// caller-owned ADPCM body.
    pub fn parse(header: &[u8], body: &'a [u8]) -> Result<Self, FormatError> {
        require_exact_len(header, INST_HEADER_LEN, "INST header")?;
        let mut reader = Reader::new(header);
        let raw_part_index = reader.i32_le()?;
        let part_index = u8::try_from(raw_part_index).map_err(|_| {
            FormatError::at(0, "INST part index is negative or does not fit eight bits")
        })?;
        if usize::from(part_index) >= MAX_SEQUENCES {
            return Err(FormatError::at(
                0,
                "INST part index is outside the seven MIDI instrument slots",
            ));
        }
        let raw_body_len = reader.i32_le()?;
        let body_len = usize::try_from(raw_body_len)
            .map_err(|_| FormatError::at(4, "INST body length is negative"))?;
        if body_len != body.len() {
            return Err(FormatError::at(
                4,
                format!(
                    "INST declares {body_len} body bytes but its item contains {}",
                    body.len()
                ),
            ));
        }
        if body_len % 16 != 0 {
            return Err(FormatError::at(
                4,
                "INST body is not aligned to 16-byte PSX ADPCM blocks",
            ));
        }
        Ok(Self { part_index, body })
    }
}

/// One active VAB program. Tone records are packed by active-program order,
/// not by the sparse MIDI program number.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VabProgram {
    pub index: u8,
    pub volume: u8,
    pub priority: u8,
    pub mode: u8,
    pub pan: u8,
    pub attribute: i16,
    pub tones: Vec<VabTone>,
}

/// One effective 32-byte VAB tone record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VabTone {
    pub priority: u8,
    pub mode: u8,
    pub volume: u8,
    pub pan: u8,
    pub center_note: u8,
    pub pitch_shift: u8,
    pub note_min: u8,
    pub note_max: u8,
    pub vibrato_width: u8,
    pub vibrato_time: u8,
    pub portamento_width: u8,
    pub portamento_time: u8,
    pub pitch_bend_min: u8,
    pub pitch_bend_max: u8,
    pub adsr1: u16,
    pub adsr2: u16,
    pub program: u8,
    /// One-based waveform number used by the VAB tone format.
    pub wave_index: u16,
}

/// Validated metadata from a MIDI entry's VAB-header item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VabBank {
    pub version: u32,
    pub bank_id: i32,
    pub file_size: u32,
    pub master_volume: u8,
    pub master_pan: u8,
    pub attribute_1: u8,
    pub attribute_2: u8,
    pub programs: Vec<VabProgram>,
    /// Actual waveform byte lengths. Retail's serialized unit is eight bytes,
    /// despite an inaccurate 16-byte comment in the upstream PC header.
    pub wave_sizes: Vec<usize>,
    pub wave_table_unknown: u16,
    serialized_header_len: usize,
    total_wave_bytes: usize,
}

impl VabBank {
    /// Parses a complete VAB header/tone/wave-size item. Waveform bodies live
    /// in separate INST entries and are validated by [`Self::validate_fragments`].
    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        if bytes.len() < VAB_HEADER_LEN {
            return Err(FormatError::at(0, "VAB header is truncated"));
        }
        if bytes.get(..4) != Some(VAB_MAGIC.as_slice()) {
            return Err(FormatError::at(0, "VAB magic is not `pBAV`"));
        }
        let mut reader = Reader::with_position(bytes, 4)?;
        let version = reader.u32_le()?;
        let bank_id = reader.i32_le()?;
        let file_size = reader.u32_le()?;
        let _reserved_0 = reader.u16_le()?;
        let program_count = usize::from(reader.u16_le()?);
        let tone_count = usize::from(reader.u16_le()?);
        let wave_count = usize::from(reader.u16_le()?);
        let master_volume = reader.u8()?;
        let master_pan = reader.u8()?;
        let attribute_1 = reader.u8()?;
        let attribute_2 = reader.u8()?;
        let _reserved_1 = reader.u32_le()?;

        if program_count > VAB_PROGRAM_COUNT {
            return Err(FormatError::at(18, "VAB has more than 128 programs"));
        }
        if tone_count > program_count.saturating_mul(VAB_TONES_PER_PROGRAM) {
            return Err(FormatError::at(
                20,
                "VAB tone count exceeds its program capacity",
            ));
        }
        if wave_count > VAB_WAVE_CAPACITY {
            return Err(FormatError::at(22, "VAB has more than 255 waveforms"));
        }
        validate_midi_value(master_volume, 24, "VAB master volume")?;
        validate_midi_value(master_pan, 25, "VAB master pan")?;

        let program_table_len = VAB_PROGRAM_COUNT
            .checked_mul(VAB_PROGRAM_LEN)
            .ok_or_else(|| FormatError::global("VAB program table length overflows"))?;
        let tone_table_len = program_count
            .checked_mul(VAB_TONES_PER_PROGRAM)
            .and_then(|value| value.checked_mul(VAB_TONE_LEN))
            .ok_or_else(|| FormatError::global("VAB tone table length overflows"))?;
        let serialized_header_len = VAB_HEADER_LEN
            .checked_add(program_table_len)
            .and_then(|value| value.checked_add(tone_table_len))
            .and_then(|value| value.checked_add(VAB_WAVE_TABLE_LEN))
            .ok_or_else(|| FormatError::global("VAB serialized header length overflows"))?;
        require_exact_len(bytes, serialized_header_len, "VAB header item")?;

        let mut active_programs = Vec::with_capacity(program_count);
        let mut program_reader = Reader::with_position(bytes, VAB_HEADER_LEN)?;
        for program_index in 0..VAB_PROGRAM_COUNT {
            let tone_slots = program_reader.u8()?;
            let volume = program_reader.u8()?;
            let priority = program_reader.u8()?;
            let mode = program_reader.u8()?;
            let pan = program_reader.u8()?;
            let _reserved_0 = program_reader.u8()?;
            let attribute = program_reader.i16_le()?;
            let _reserved_1 = program_reader.u32_le()?;
            let _reserved_2 = program_reader.u32_le()?;
            if usize::from(tone_slots) > VAB_TONES_PER_PROGRAM {
                return Err(FormatError::at(
                    VAB_HEADER_LEN + program_index * VAB_PROGRAM_LEN,
                    "VAB program has more than 16 tones",
                ));
            }
            if tone_slots == 0 {
                continue;
            }
            validate_midi_value(
                volume,
                VAB_HEADER_LEN + program_index * VAB_PROGRAM_LEN + 1,
                "VAB program volume",
            )?;
            validate_midi_value(
                pan,
                VAB_HEADER_LEN + program_index * VAB_PROGRAM_LEN + 4,
                "VAB program pan",
            )?;
            active_programs.push((
                u8::try_from(program_index).expect("program index is below 128"),
                tone_slots,
                volume,
                priority,
                mode,
                pan,
                attribute,
            ));
        }
        if active_programs.len() != program_count {
            return Err(FormatError::at(
                18,
                format!(
                    "VAB declares {program_count} programs but {} program records are active",
                    active_programs.len()
                ),
            ));
        }
        let effective_tones = active_programs
            .iter()
            .map(|(_, count, ..)| usize::from(*count))
            .sum::<usize>();
        if effective_tones != tone_count {
            return Err(FormatError::at(
                20,
                format!(
                    "VAB declares {tone_count} tones but active programs reference {effective_tones}"
                ),
            ));
        }

        let tone_table_start = VAB_HEADER_LEN + program_table_len;
        let mut programs = Vec::with_capacity(program_count);
        for (packed_index, (index, tone_slots, volume, priority, mode, pan, attribute)) in
            active_programs.into_iter().enumerate()
        {
            let block_start =
                tone_table_start + packed_index * VAB_TONES_PER_PROGRAM * VAB_TONE_LEN;
            let mut tones = Vec::with_capacity(usize::from(tone_slots));
            for tone_index in 0..usize::from(tone_slots) {
                let offset = block_start + tone_index * VAB_TONE_LEN;
                tones.push(parse_vab_tone(bytes, offset, index, wave_count)?);
            }
            programs.push(VabProgram {
                index,
                volume,
                priority,
                mode,
                pan,
                attribute,
                tones,
            });
        }

        let wave_table_start = tone_table_start + tone_table_len;
        let mut wave_reader = Reader::with_position(bytes, wave_table_start)?;
        let wave_table_unknown = wave_reader.u16_le()?;
        let mut wave_sizes = Vec::with_capacity(wave_count);
        let mut total_wave_bytes = 0_usize;
        for wave_index in 0..VAB_WAVE_CAPACITY {
            let units = wave_reader.u16_le()?;
            if wave_index >= wave_count {
                continue;
            }
            let byte_len = usize::from(units).checked_mul(8).ok_or_else(|| {
                FormatError::at(wave_reader.position() - 2, "VAB wave size overflows")
            })?;
            if byte_len == 0 || byte_len % 16 != 0 {
                return Err(FormatError::at(
                    wave_reader.position() - 2,
                    "VAB waveform is empty or not PSX ADPCM-block aligned",
                ));
            }
            total_wave_bytes = total_wave_bytes.checked_add(byte_len).ok_or_else(|| {
                FormatError::at(wave_reader.position() - 2, "VAB waveform total overflows")
            })?;
            wave_sizes.push(byte_len);
        }
        let expected_file_size = serialized_header_len
            .checked_add(total_wave_bytes)
            .ok_or_else(|| FormatError::global("VAB full size overflows"))?;
        if usize::try_from(file_size).ok() != Some(expected_file_size) {
            return Err(FormatError::at(
                12,
                format!(
                    "VAB declares {file_size} total bytes but header and wave table describe {expected_file_size}"
                ),
            ));
        }

        Ok(Self {
            version,
            bank_id,
            file_size,
            master_volume,
            master_pan,
            attribute_1,
            attribute_2,
            programs,
            wave_sizes,
            wave_table_unknown,
            serialized_header_len,
            total_wave_bytes,
        })
    }

    /// Serialized header bytes before the external waveform bodies.
    #[must_use]
    pub const fn serialized_header_len(&self) -> usize {
        self.serialized_header_len
    }

    /// Sum of the waveform lengths described by the 255-entry size table.
    #[must_use]
    pub const fn total_wave_bytes(&self) -> usize {
        self.total_wave_bytes
    }

    /// Verifies that borrowed INST parts are unique, contiguous and exactly
    /// cover the VAB waveform body. Part order supplied by the caller is not
    /// significant.
    pub fn validate_fragments(
        &self,
        fragments: &[InstrumentFragment<'_>],
    ) -> Result<(), FormatError> {
        if fragments.len() > MAX_SEQUENCES {
            return Err(FormatError::global(
                "VAB references more than seven INST fragments",
            ));
        }
        let mut seen = [false; MAX_SEQUENCES];
        let mut total = 0_usize;
        for fragment in fragments {
            let index = usize::from(fragment.part_index);
            if index >= MAX_SEQUENCES {
                return Err(FormatError::global(format!(
                    "INST part {} is outside the seven MIDI instrument slots",
                    fragment.part_index
                )));
            }
            if seen[index] {
                return Err(FormatError::global(format!(
                    "VAB contains duplicate INST part {}",
                    fragment.part_index
                )));
            }
            seen[index] = true;
            total = total
                .checked_add(fragment.body.len())
                .ok_or_else(|| FormatError::global("INST body total overflows"))?;
        }
        for (expected, present) in seen.iter().enumerate().take(fragments.len()) {
            if !present {
                return Err(FormatError::global(format!(
                    "VAB is missing contiguous INST part {expected}"
                )));
            }
        }
        if total != self.total_wave_bytes {
            return Err(FormatError::global(format!(
                "INST fragments contain {total} bytes but VAB requires {}",
                self.total_wave_bytes
            )));
        }
        Ok(())
    }
}

/// One decoded Sony sequence event with its absolute musical tick.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SepEvent {
    pub tick: u64,
    pub kind: SepEventKind,
}

/// Endian-neutral event forms preserved from a Sony SEQ stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SepEventKind {
    NoteOff {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    NoteOn {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    PolyphonicPressure {
        channel: u8,
        note: u8,
        pressure: u8,
    },
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
    },
    ProgramChange {
        channel: u8,
        program: u8,
    },
    ChannelPressure {
        channel: u8,
        pressure: u8,
    },
    PitchBend {
        channel: u8,
        value: u16,
    },
    Tempo {
        micros_per_quarter: u32,
    },
    End,
}

/// One bounded sequence embedded after a SEP header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SepSequence {
    pub number: u16,
    pub ticks_per_quarter: u16,
    pub initial_tempo: u32,
    pub time_signature_numerator: u8,
    pub time_signature_denominator: u8,
    pub unused: u16,
    pub events: Vec<SepEvent>,
    pub end_tick: u64,
}

/// A validated `pQES` container. Retail entries can contain up to seven
/// sequence headers; some shipped headers declare a dormant second track but
/// serialize only one, so parsing is driven by the bounded SEP item itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sep {
    pub version: u16,
    pub sequences: Vec<SepSequence>,
}

impl Sep {
    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        if bytes.len() < SEP_HEADER_LEN {
            return Err(FormatError::at(0, "SEP header is truncated"));
        }
        if bytes.get(..4) != Some(SEP_MAGIC.as_slice()) {
            return Err(FormatError::at(0, "SEP magic is not `pQES`"));
        }
        let mut reader = Reader::with_position(bytes, 4)?;
        let version = reader.u16_be()?;
        let mut sequences = Vec::new();
        loop {
            let remaining = reader.remaining();
            if remaining == 0 {
                break;
            }
            let tail = bytes
                .get(reader.position()..)
                .expect("reader position is bounds checked");
            if remaining <= 3 && tail.iter().all(|byte| *byte == 0) {
                break;
            }
            if sequences.len() >= MAX_SEQUENCES {
                return Err(FormatError::at(
                    reader.position(),
                    "SEP contains more than seven sequences",
                ));
            }
            let header_offset = reader.position();
            if remaining < SEQUENCE_HEADER_LEN {
                return Err(FormatError::at(
                    header_offset,
                    "SEP sequence header is truncated",
                ));
            }
            let number = reader.u16_be()?;
            let ticks_per_quarter = reader.u16_be()?;
            let tempo_bytes = reader.take(3)?;
            let initial_tempo = u32::from(tempo_bytes[0]) << 16
                | u32::from(tempo_bytes[1]) << 8
                | u32::from(tempo_bytes[2]);
            let time_signature_numerator = reader.u8()?;
            let time_signature_denominator = reader.u8()?;
            let unused = reader.u16_be()?;
            let event_len = usize::from(reader.u16_be()?);
            if ticks_per_quarter == 0 {
                return Err(FormatError::at(
                    header_offset + 2,
                    "SEQ ticks-per-quarter is zero",
                ));
            }
            if initial_tempo == 0 {
                return Err(FormatError::at(
                    header_offset + 4,
                    "SEQ initial tempo is zero",
                ));
            }
            if time_signature_numerator == 0 {
                return Err(FormatError::at(
                    header_offset + 7,
                    "SEQ time-signature numerator is zero",
                ));
            }
            if time_signature_denominator > 7 {
                return Err(FormatError::at(
                    header_offset + 8,
                    "SEQ time-signature denominator exponent exceeds seven",
                ));
            }
            let event_offset = reader.position();
            let event_bytes = reader.take(event_len)?;
            let (events, end_tick) = parse_sequence_events(event_bytes, event_offset)?;
            sequences.push(SepSequence {
                number,
                ticks_per_quarter,
                initial_tempo,
                time_signature_numerator,
                time_signature_denominator,
                unused,
                events,
                end_tick,
            });
        }
        if sequences.is_empty() {
            return Err(FormatError::at(
                SEP_HEADER_LEN,
                "SEP contains no complete sequences",
            ));
        }
        Ok(Self { version, sequences })
    }
}

/// Parsed metadata owned by one type-13 retail entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetailMidiAsset {
    pub header: MidiHeader,
    pub vab: VabBank,
    pub sep: Sep,
}

/// Parses all three MIDI entry items without retaining any serialized bytes.
pub fn parse_retail_midi(entry: &Entry, nsf_bytes: &[u8]) -> Result<RetailMidiAsset, FormatError> {
    if entry.entry_type != MIDI_ENTRY_TYPE {
        return Err(FormatError::global(format!(
            "entry {} has type {}, expected MIDI type {MIDI_ENTRY_TYPE}",
            entry.eid, entry.entry_type
        )));
    }
    if entry.items.len() != 3 {
        return Err(FormatError::global(format!(
            "MIDI entry {} has {} items, expected three",
            entry.eid,
            entry.items.len()
        )));
    }
    let header_bytes = entry.items[0].bytes(nsf_bytes)?;
    require_exact_len(header_bytes, MidiHeader::BYTE_LEN, "MIDI header item")?;
    let header = MidiHeader::parse(header_bytes)?;
    let vab = VabBank::parse(entry.items[1].bytes(nsf_bytes)?)?;
    let sep = Sep::parse(entry.items[2].bytes(nsf_bytes)?)?;
    let declared = usize::try_from(header.track_count)
        .map_err(|_| FormatError::global("validated MIDI track count became negative"))?;
    if sep.sequences.len() > declared {
        return Err(FormatError::global(format!(
            "MIDI declares {declared} tracks but SEP contains {}",
            sep.sequences.len()
        )));
    }
    Ok(RetailMidiAsset { header, vab, sep })
}

/// Parses one exact two-item type-14 entry as a borrowed INST fragment.
pub fn parse_instrument_entry<'a>(
    entry: &Entry,
    nsf_bytes: &'a [u8],
) -> Result<InstrumentFragment<'a>, FormatError> {
    if entry.entry_type != INST_ENTRY_TYPE {
        return Err(FormatError::global(format!(
            "entry {} has type {}, expected INST type {INST_ENTRY_TYPE}",
            entry.eid, entry.entry_type
        )));
    }
    if entry.items.len() != 2 {
        return Err(FormatError::global(format!(
            "INST entry {} has {} items, expected two",
            entry.eid,
            entry.items.len()
        )));
    }
    InstrumentFragment::parse(
        entry.items[0].bytes(nsf_bytes)?,
        entry.items[1].bytes(nsf_bytes)?,
    )
}

fn parse_vab_tone(
    bytes: &[u8],
    offset: usize,
    expected_program: u8,
    wave_count: usize,
) -> Result<VabTone, FormatError> {
    let mut reader = Reader::with_position(bytes, offset)?;
    let priority = reader.u8()?;
    let mode = reader.u8()?;
    let volume = reader.u8()?;
    let pan = reader.u8()?;
    let center_note = reader.u8()?;
    let pitch_shift = reader.u8()?;
    let note_min = reader.u8()?;
    let note_max = reader.u8()?;
    let vibrato_width = reader.u8()?;
    let vibrato_time = reader.u8()?;
    let portamento_width = reader.u8()?;
    let portamento_time = reader.u8()?;
    let pitch_bend_min = reader.u8()?;
    let pitch_bend_max = reader.u8()?;
    let _reserved_1 = reader.u8()?;
    let _reserved_2 = reader.u8()?;
    let adsr1 = reader.u16_le()?;
    let adsr2 = reader.u16_le()?;
    let raw_program = reader.i16_le()?;
    let raw_wave_index = reader.i16_le()?;
    for (value, field_offset, field) in [
        (volume, 2, "tone volume"),
        (pan, 3, "tone pan"),
        (center_note, 4, "tone center note"),
        (pitch_shift, 5, "tone pitch shift"),
        (note_min, 6, "tone minimum note"),
        (note_max, 7, "tone maximum note"),
        (vibrato_width, 8, "tone vibrato width"),
        (vibrato_time, 9, "tone vibrato time"),
        (portamento_width, 10, "tone portamento width"),
        (portamento_time, 11, "tone portamento time"),
        (pitch_bend_min, 12, "tone negative pitch bend"),
        (pitch_bend_max, 13, "tone positive pitch bend"),
    ] {
        validate_midi_value(value, offset + field_offset, field)?;
    }
    if note_min > note_max {
        return Err(FormatError::at(
            offset + 6,
            "VAB tone minimum note exceeds its maximum",
        ));
    }
    if raw_program != i16::from(expected_program) {
        return Err(FormatError::at(
            offset + 20,
            format!("VAB tone names program {raw_program}, expected {expected_program}"),
        ));
    }
    let wave_index = u16::try_from(raw_wave_index)
        .map_err(|_| FormatError::at(offset + 22, "VAB tone waveform number is negative"))?;
    if wave_index == 0 || usize::from(wave_index) > wave_count {
        return Err(FormatError::at(
            offset + 22,
            "VAB tone waveform number is outside the one-based wave table",
        ));
    }
    // Four reserved i16 values complete the exact 32-byte record.
    for _ in 0..4 {
        let _ = reader.i16_le()?;
    }
    Ok(VabTone {
        priority,
        mode,
        volume,
        pan,
        center_note,
        pitch_shift,
        note_min,
        note_max,
        vibrato_width,
        vibrato_time,
        portamento_width,
        portamento_time,
        pitch_bend_min,
        pitch_bend_max,
        adsr1,
        adsr2,
        program: expected_program,
        wave_index,
    })
}

fn parse_sequence_events(
    bytes: &[u8],
    absolute_offset: usize,
) -> Result<(Vec<SepEvent>, u64), FormatError> {
    let mut position = 0_usize;
    let mut tick = 0_u64;
    let mut running_status = None;
    let mut events = Vec::with_capacity(bytes.len().saturating_div(3).saturating_add(1));
    let mut ended = false;
    while position < bytes.len() {
        let (delta, next) = parse_vlq(bytes, position, absolute_offset)?;
        position = next;
        tick = tick.checked_add(u64::from(delta)).ok_or_else(|| {
            FormatError::at(absolute_offset + position, "SEQ absolute tick overflows")
        })?;
        let first = *bytes.get(position).ok_or_else(|| {
            FormatError::at(absolute_offset + position, "SEQ event status is truncated")
        })?;
        let status = if first & 0x80 != 0 {
            position += 1;
            if first < 0xf0 {
                running_status = Some(first);
            } else {
                running_status = None;
            }
            first
        } else {
            running_status.ok_or_else(|| {
                FormatError::at(
                    absolute_offset + position,
                    "SEQ running-status event has no prior channel status",
                )
            })?
        };
        let channel = status & 0x0f;
        let kind = match status >> 4 {
            0x8 => {
                let [note, velocity] = take_midi_data_2(bytes, &mut position, absolute_offset)?;
                SepEventKind::NoteOff {
                    channel,
                    note,
                    velocity,
                }
            }
            0x9 => {
                let [note, velocity] = take_midi_data_2(bytes, &mut position, absolute_offset)?;
                SepEventKind::NoteOn {
                    channel,
                    note,
                    velocity,
                }
            }
            0xa => {
                let [note, pressure] = take_midi_data_2(bytes, &mut position, absolute_offset)?;
                SepEventKind::PolyphonicPressure {
                    channel,
                    note,
                    pressure,
                }
            }
            0xb => {
                let [controller, value] = take_midi_data_2(bytes, &mut position, absolute_offset)?;
                SepEventKind::ControlChange {
                    channel,
                    controller,
                    value,
                }
            }
            0xc => SepEventKind::ProgramChange {
                channel,
                program: take_midi_data(bytes, &mut position, absolute_offset)?,
            },
            0xd => SepEventKind::ChannelPressure {
                channel,
                pressure: take_midi_data(bytes, &mut position, absolute_offset)?,
            },
            0xe => {
                let [least, most] = take_midi_data_2(bytes, &mut position, absolute_offset)?;
                SepEventKind::PitchBend {
                    channel,
                    value: u16::from(least) | (u16::from(most) << 7),
                }
            }
            0xf if status == 0xff => {
                let meta_type = *bytes.get(position).ok_or_else(|| {
                    FormatError::at(
                        absolute_offset + position,
                        "SEQ meta-event type is truncated",
                    )
                })?;
                position += 1;
                match meta_type {
                    0x2f => {
                        events.push(SepEvent {
                            tick,
                            kind: SepEventKind::End,
                        });
                        ended = true;
                        break;
                    }
                    0x51 => {
                        let data = take_exact(bytes, &mut position, 3, absolute_offset)?;
                        let micros_per_quarter =
                            u32::from(data[0]) << 16 | u32::from(data[1]) << 8 | u32::from(data[2]);
                        if micros_per_quarter == 0 {
                            return Err(FormatError::at(
                                absolute_offset + position - 3,
                                "SEQ tempo meta-event is zero",
                            ));
                        }
                        SepEventKind::Tempo { micros_per_quarter }
                    }
                    0x54 => {
                        let _ = take_exact(bytes, &mut position, 5, absolute_offset)?;
                        continue;
                    }
                    0x58 => {
                        let _ = take_exact(bytes, &mut position, 4, absolute_offset)?;
                        continue;
                    }
                    0x59 => {
                        let _ = take_exact(bytes, &mut position, 2, absolute_offset)?;
                        continue;
                    }
                    _ => {
                        return Err(FormatError::at(
                            absolute_offset + position - 1,
                            format!("unsupported SEP meta-event 0x{meta_type:02x}"),
                        ));
                    }
                }
            }
            _ => {
                return Err(FormatError::at(
                    absolute_offset + position.saturating_sub(1),
                    format!("unsupported SEP status 0x{status:02x}"),
                ));
            }
        };
        events.push(SepEvent { tick, kind });
    }
    if !ended {
        return Err(FormatError::at(
            absolute_offset + position,
            "SEQ event stream has no end-of-track marker",
        ));
    }
    if let Some(nonzero) = bytes[position..].iter().position(|byte| *byte != 0) {
        return Err(FormatError::at(
            absolute_offset + position + nonzero,
            "SEQ contains nonzero bytes after end-of-track",
        ));
    }
    Ok((events, tick))
}

fn parse_vlq(
    bytes: &[u8],
    start: usize,
    absolute_offset: usize,
) -> Result<(u32, usize), FormatError> {
    let mut value = 0_u32;
    let mut position = start;
    for _ in 0..MAX_VLQ_BYTES {
        let byte = *bytes.get(position).ok_or_else(|| {
            FormatError::at(
                absolute_offset + position,
                "SEQ variable-length quantity is truncated",
            )
        })?;
        position += 1;
        value = (value << 7) | u32::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return Ok((value, position));
        }
    }
    Err(FormatError::at(
        absolute_offset + start,
        "SEQ variable-length quantity exceeds four bytes",
    ))
}

fn take_midi_data(
    bytes: &[u8],
    position: &mut usize,
    absolute_offset: usize,
) -> Result<u8, FormatError> {
    let offset = *position;
    let value = *bytes.get(offset).ok_or_else(|| {
        FormatError::at(
            absolute_offset + offset,
            "SEQ channel event data is truncated",
        )
    })?;
    if value & 0x80 != 0 {
        return Err(FormatError::at(
            absolute_offset + offset,
            "SEQ channel event data has its status bit set",
        ));
    }
    *position += 1;
    Ok(value)
}

fn take_midi_data_2(
    bytes: &[u8],
    position: &mut usize,
    absolute_offset: usize,
) -> Result<[u8; 2], FormatError> {
    Ok([
        take_midi_data(bytes, position, absolute_offset)?,
        take_midi_data(bytes, position, absolute_offset)?,
    ])
}

fn take_exact<'a>(
    bytes: &'a [u8],
    position: &mut usize,
    length: usize,
    absolute_offset: usize,
) -> Result<&'a [u8], FormatError> {
    let start = *position;
    let end = start.checked_add(length).ok_or_else(|| {
        FormatError::at(absolute_offset + start, "SEQ event data range overflows")
    })?;
    let result = bytes
        .get(start..end)
        .ok_or_else(|| FormatError::at(absolute_offset + start, "SEQ event data is truncated"))?;
    *position = end;
    Ok(result)
}

fn validate_midi_value(value: u8, offset: usize, field: &str) -> Result<(), FormatError> {
    if value > 127 {
        return Err(FormatError::at(
            offset,
            format!("{field} is outside 0..=127"),
        ));
    }
    Ok(())
}

fn require_exact_len(bytes: &[u8], expected: usize, context: &str) -> Result<(), FormatError> {
    if bytes.len() != expected {
        return Err(FormatError::at(
            bytes.len().min(expected),
            format!(
                "{context} is {} bytes, expected exactly {expected}",
                bytes.len()
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn sequence_bytes(events: &[u8]) -> Vec<u8> {
        let mut bytes = b"pQES\0\0".to_vec();
        bytes.extend_from_slice(&0_u16.to_be_bytes());
        bytes.extend_from_slice(&480_u16.to_be_bytes());
        bytes.extend_from_slice(&[0x07, 0xa1, 0x20]);
        bytes.extend_from_slice(&[4, 2]);
        bytes.extend_from_slice(&0_u16.to_be_bytes());
        bytes.extend_from_slice(&u16::try_from(events.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(events);
        bytes
    }

    #[test]
    fn inst_header_is_little_endian_and_body_bounded() {
        let body = [0_u8; 32];
        let fragment = InstrumentFragment::parse(&[2, 0, 0, 0, 32, 0, 0, 0], &body).unwrap();
        assert_eq!(fragment.part_index, 2);
        assert_eq!(fragment.body, body);
        assert!(InstrumentFragment::parse(&[2, 0, 0, 0, 31, 0, 0, 0], &body).is_err());
        assert!(InstrumentFragment::parse(&[7, 0, 0, 0, 32, 0, 0, 0], &body).is_err());
    }

    #[test]
    fn sep_header_and_vlq_timing_are_big_endian() {
        let bytes = sequence_bytes(&[
            0x00, 0xc2, 0x05, // program 5
            0x81, 0x70, 0x92, 60, 100, // delta 240, note on
            0x00, 61, 0, // running note-on with velocity zero
            0x00, 0xff, 0x2f, 0, 0, // end plus retail alignment
        ]);
        let sep = Sep::parse(&bytes).unwrap();
        let sequence = &sep.sequences[0];
        assert_eq!(sequence.ticks_per_quarter, 480);
        assert_eq!(sequence.initial_tempo, 500_000);
        assert_eq!(sequence.end_tick, 240);
        assert_eq!(
            sequence.events,
            [
                SepEvent {
                    tick: 0,
                    kind: SepEventKind::ProgramChange {
                        channel: 2,
                        program: 5
                    }
                },
                SepEvent {
                    tick: 240,
                    kind: SepEventKind::NoteOn {
                        channel: 2,
                        note: 60,
                        velocity: 100
                    }
                },
                SepEvent {
                    tick: 240,
                    kind: SepEventKind::NoteOn {
                        channel: 2,
                        note: 61,
                        velocity: 0
                    }
                },
                SepEvent {
                    tick: 240,
                    kind: SepEventKind::End
                }
            ]
        );
    }

    #[test]
    fn sep_rejects_bad_running_status_and_unterminated_vlq() {
        assert!(Sep::parse(&sequence_bytes(&[0, 60, 100, 0, 0xff, 0x2f])).is_err());
        assert!(Sep::parse(&sequence_bytes(&[0x80, 0x80, 0x80, 0x80])).is_err());
    }

    #[test]
    fn sep_allows_only_zero_alignment_after_end() {
        assert!(Sep::parse(&sequence_bytes(&[0, 0xff, 0x2f, 0, 0])).is_ok());
        assert!(Sep::parse(&sequence_bytes(&[0, 0xff, 0x2f, 0, 1])).is_err());
    }

    #[test]
    fn fragment_validation_rejects_gaps_duplicates_and_wrong_total() {
        let bank = VabBank {
            version: 6,
            bank_id: 0,
            file_size: 0,
            master_volume: 127,
            master_pan: 64,
            attribute_1: 0,
            attribute_2: 0,
            programs: Vec::new(),
            wave_sizes: vec![16, 16],
            wave_table_unknown: 0,
            serialized_header_len: 0,
            total_wave_bytes: 32,
        };
        let body = [0_u8; 16];
        assert!(
            bank.validate_fragments(&[
                InstrumentFragment {
                    part_index: 0,
                    body: &body
                },
                InstrumentFragment {
                    part_index: 1,
                    body: &body
                }
            ])
            .is_ok()
        );
        assert!(
            bank.validate_fragments(&[InstrumentFragment {
                part_index: 1,
                body: &body
            }])
            .is_err()
        );
        assert!(
            bank.validate_fragments(&[InstrumentFragment {
                part_index: u8::MAX,
                body: &body
            }])
            .is_err()
        );
        assert!(
            bank.validate_fragments(&[
                InstrumentFragment {
                    part_index: 0,
                    body: &body
                },
                InstrumentFragment {
                    part_index: 0,
                    body: &body
                }
            ])
            .is_err()
        );
    }

    proptest! {
        #[test]
        fn arbitrary_sep_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
            let _ = Sep::parse(&bytes);
        }

        #[test]
        fn arbitrary_vab_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..16_384)) {
            let _ = VabBank::parse(&bytes);
        }

        #[test]
        fn arbitrary_inst_never_panics(
            header in prop::collection::vec(any::<u8>(), 0..32),
            body in prop::collection::vec(any::<u8>(), 0..4096),
        ) {
            let _ = InstrumentFragment::parse(&header, &body);
        }
    }
}
