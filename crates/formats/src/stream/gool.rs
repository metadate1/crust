//! Validated materialization of one retail GOOL object's initial state.
//!
//! A global GOOL entry uses item 0 for its header, item 1 for shared code,
//! item 2 for internal words and external-entry EIDs, item 3 for the event and
//! subtype maps, and item 4 for sixteen-byte state descriptors. A selected
//! state's `external_index` names an EID in item 2; that entry supplies code in
//! item 1 and external words in item 2. This module resolves that graph with
//! logical EIDs and owned words, never relocated native pointers.

use crate::binary::{Eid, FormatError, Reader};

use super::nsd::Nsd;
use super::nsf::{Entry, Nsf};
use super::structs::{GoolHeader, GoolState};

/// Retail sentinel for an absent state code, event, or transition program.
pub const GOOL_PC_NONE: u16 = 0x3fff;

const GLOBAL_HEADER_ITEM: usize = 0;
const GLOBAL_CODE_ITEM: usize = 1;
const GLOBAL_DATA_ITEM: usize = 2;
const GLOBAL_MAP_ITEM: usize = 3;
const GLOBAL_STATE_ITEM: usize = 4;
const GLOBAL_ANIMATION_ITEM: usize = 5;
const EXTERNAL_CODE_ITEM: usize = 1;
const EXTERNAL_DATA_ITEM: usize = 2;

/// One state-specific, pointer-free GOOL program ready for a VM to bind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoolProgram {
    global_eid: Eid,
    external_eid: Eid,
    header: GoolHeader,
    state_index: u16,
    state: GoolState,
    states: Vec<GoolState>,
    global_code: Vec<u32>,
    code: Vec<u32>,
    internal_words: Vec<u32>,
    animation_data: Vec<u8>,
    external_words: Vec<u32>,
    code_pc: Option<usize>,
    event_pc: Option<usize>,
    transition_pc: Option<usize>,
}

impl GoolProgram {
    #[must_use]
    pub const fn global_eid(&self) -> Eid {
        self.global_eid
    }

    #[must_use]
    pub const fn external_eid(&self) -> Eid {
        self.external_eid
    }

    #[must_use]
    pub const fn header(&self) -> GoolHeader {
        self.header
    }

    #[must_use]
    pub const fn state_index(&self) -> u16 {
        self.state_index
    }

    #[must_use]
    pub const fn state(&self) -> GoolState {
        self.state
    }

    /// All descriptors from global item four. The VM uses their flags to
    /// apply retail's guarded state-link behavior before asking the host to
    /// bind a target state's external entry.
    #[must_use]
    pub fn states(&self) -> &[GoolState] {
        &self.states
    }

    #[must_use]
    pub fn global_code(&self) -> &[u32] {
        &self.global_code
    }

    #[must_use]
    pub fn code(&self) -> &[u32] {
        &self.code
    }

    #[must_use]
    pub fn internal_words(&self) -> &[u32] {
        &self.internal_words
    }

    /// Raw global item-five bytes addressed by GOOL animation references.
    #[must_use]
    pub fn animation_data(&self) -> &[u8] {
        &self.animation_data
    }

    #[must_use]
    pub fn external_words(&self) -> &[u32] {
        &self.external_words
    }

    #[must_use]
    pub const fn code_pc(&self) -> Option<usize> {
        self.code_pc
    }

    #[must_use]
    pub const fn event_pc(&self) -> Option<usize> {
        self.event_pc
    }

    #[must_use]
    pub const fn transition_pc(&self) -> Option<usize> {
        self.transition_pc
    }
}

/// Resolves a global GOOL EID and subtype into its initial external program.
pub fn load_gool_program(
    metadata: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    global_eid: Eid,
    subtype: u16,
) -> Result<GoolProgram, FormatError> {
    let global = unique_entry(metadata, nsf, global_eid, "global GOOL")?;
    let header = parse_global_header(global, nsf_bytes)?;
    let internal_words = parse_words(global, GLOBAL_DATA_ITEM, nsf_bytes, "GOOL internal data")?;
    let map_bytes = item_bytes(global, GLOBAL_MAP_ITEM, nsf_bytes, "GOOL state maps")?;
    let state_map = parse_halfwords(
        map_bytes,
        global.items[GLOBAL_MAP_ITEM].byte_range().start,
        "GOOL state maps",
    )?;
    let subtype_base = usize::try_from(header.subtype_map_index).map_err(|_| {
        FormatError::at(
            global.items[GLOBAL_HEADER_ITEM].byte_range().start + 16,
            "GOOL subtype-map index does not fit the host",
        )
    })?;
    if subtype_base > state_map.len() {
        return Err(FormatError::at(
            global.items[GLOBAL_HEADER_ITEM].byte_range().start + 16,
            "GOOL subtype-map index is outside the map item",
        ));
    }
    let subtype_index = subtype_base
        .checked_add(usize::from(subtype))
        .ok_or_else(|| FormatError::global("GOOL subtype-map lookup overflows"))?;
    let state_index = state_map.get(subtype_index).copied().ok_or_else(|| {
        FormatError::at(
            global.items[GLOBAL_MAP_ITEM].byte_range().start,
            format!("GOOL subtype {subtype} is outside the subtype map"),
        )
    })?;
    if state_index == 0x00ff {
        return Err(FormatError::at(
            global.items[GLOBAL_MAP_ITEM].byte_range().start + subtype_index * 2,
            format!("GOOL subtype {subtype} maps to the invalid-state sentinel"),
        ));
    }

    load_resolved_state_program(
        metadata,
        nsf,
        nsf_bytes,
        global_eid,
        global,
        header,
        internal_words,
        state_index,
    )
}

/// Resolves one explicit state index for an already selected global GOOL.
///
/// This is the pointer-free host operation used after opcode `0x82` changes
/// an object's state. Unlike [`load_gool_program`], it does not consult the
/// subtype map.
pub fn load_gool_state_program(
    metadata: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    global_eid: Eid,
    state_index: u16,
) -> Result<GoolProgram, FormatError> {
    let global = unique_entry(metadata, nsf, global_eid, "global GOOL")?;
    let header = parse_global_header(global, nsf_bytes)?;
    let internal_words = parse_words(global, GLOBAL_DATA_ITEM, nsf_bytes, "GOOL internal data")?;
    load_resolved_state_program(
        metadata,
        nsf,
        nsf_bytes,
        global_eid,
        global,
        header,
        internal_words,
        state_index,
    )
}

fn parse_global_header(global: &Entry, nsf_bytes: &[u8]) -> Result<GoolHeader, FormatError> {
    let header_bytes = item_bytes(global, GLOBAL_HEADER_ITEM, nsf_bytes, "GOOL header")?;
    if header_bytes.len() < GoolHeader::BYTE_LEN {
        return Err(FormatError::at(
            global.items[GLOBAL_HEADER_ITEM].byte_range().start,
            "GOOL header is shorter than 24 bytes",
        ));
    }
    GoolHeader::parse(header_bytes)
}

#[allow(clippy::too_many_arguments)]
fn load_resolved_state_program(
    metadata: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    global_eid: Eid,
    global: &Entry,
    header: GoolHeader,
    internal_words: Vec<u32>,
    state_index: u16,
) -> Result<GoolProgram, FormatError> {
    let global_code = parse_words(global, GLOBAL_CODE_ITEM, nsf_bytes, "GOOL shared code")?;

    let states_bytes = item_bytes(global, GLOBAL_STATE_ITEM, nsf_bytes, "GOOL states")?;
    if states_bytes.len() % GoolState::BYTE_LEN != 0 {
        return Err(FormatError::at(
            global.items[GLOBAL_STATE_ITEM].byte_range().start,
            "GOOL state item length is not a multiple of 16 bytes",
        ));
    }
    let states = states_bytes
        .chunks_exact(GoolState::BYTE_LEN)
        .map(GoolState::parse)
        .collect::<Result<Vec<_>, _>>()?;
    let state_offset = usize::from(state_index)
        .checked_mul(GoolState::BYTE_LEN)
        .ok_or_else(|| FormatError::global("GOOL state offset overflows"))?;
    let state = states
        .get(usize::from(state_index))
        .copied()
        .ok_or_else(|| {
            FormatError::at(
                global.items[GLOBAL_STATE_ITEM].byte_range().start,
                format!("GOOL state {state_index} is outside the state item"),
            )
        })?;
    let animation_data = global
        .item(GLOBAL_ANIMATION_ITEM)
        .map(|item| item.bytes(nsf_bytes))
        .transpose()?
        .unwrap_or_default()
        .to_vec();

    let external_eid = internal_words
        .get(usize::from(state.external_index))
        .copied()
        .map(Eid::from_raw)
        .ok_or_else(|| {
            FormatError::at(
                global.items[GLOBAL_DATA_ITEM].byte_range().start,
                format!(
                    "GOOL state {state_index} external index {} is outside internal data",
                    state.external_index
                ),
            )
        })?;
    if external_eid == Eid::NONE || !external_eid.is_named() {
        return Err(FormatError::at(
            global.items[GLOBAL_DATA_ITEM].byte_range().start
                + usize::from(state.external_index) * 4,
            "GOOL state external reference is not a named EID",
        ));
    }
    let external = unique_entry(metadata, nsf, external_eid, "external GOOL")?;
    let code = parse_words(
        external,
        EXTERNAL_CODE_ITEM,
        nsf_bytes,
        "GOOL external code",
    )?;
    let external_words = parse_words(
        external,
        EXTERNAL_DATA_ITEM,
        nsf_bytes,
        "GOOL external data",
    )?;
    let state_absolute = global.items[GLOBAL_STATE_ITEM].byte_range().start + state_offset;
    let event_pc = validate_pc(state.event_pc, code.len(), state_absolute + 10, "event")?;
    let transition_pc = validate_pc(
        state.transition_pc,
        code.len(),
        state_absolute + 12,
        "transition",
    )?;
    let code_pc = validate_pc(state.code_pc, code.len(), state_absolute + 14, "code")?;

    Ok(GoolProgram {
        global_eid,
        external_eid,
        header,
        state_index,
        state,
        states,
        global_code,
        code,
        internal_words,
        animation_data,
        external_words,
        code_pc,
        event_pc,
        transition_pc,
    })
}

fn unique_entry<'a>(
    metadata: &Nsd,
    nsf: &'a Nsf,
    eid: Eid,
    context: &str,
) -> Result<&'a Entry, FormatError> {
    let mut matches = nsf.entries().filter(|entry| entry.eid == eid);
    let entry = nsf.resolve_entry(metadata, eid).map_err(|error| {
        FormatError::global(format!("could not resolve {context} entry {eid}: {error}"))
    })?;
    let _ = matches.next();
    if matches.next().is_some() {
        return Err(FormatError::global(format!(
            "{context} EID {eid} is ambiguous in the NSF"
        )));
    }
    Ok(entry)
}

fn item_bytes<'a>(
    entry: &Entry,
    index: usize,
    nsf_bytes: &'a [u8],
    context: &str,
) -> Result<&'a [u8], FormatError> {
    entry
        .item(index)
        .ok_or_else(|| {
            FormatError::at(
                entry.byte_range().start,
                format!("{context} item {index} is missing"),
            )
        })?
        .bytes(nsf_bytes)
}

fn parse_words(
    entry: &Entry,
    index: usize,
    nsf_bytes: &[u8],
    context: &str,
) -> Result<Vec<u32>, FormatError> {
    let bytes = item_bytes(entry, index, nsf_bytes, context)?;
    let absolute = entry.items[index].byte_range().start;
    if !bytes.len().is_multiple_of(4) {
        return Err(FormatError::at(
            absolute,
            format!("{context} length is not a multiple of four bytes"),
        ));
    }
    let mut reader = Reader::new(bytes);
    let mut words = Vec::with_capacity(bytes.len() / 4);
    while reader.remaining() != 0 {
        words.push(reader.u32_le()?);
    }
    Ok(words)
}

fn parse_halfwords(bytes: &[u8], absolute: usize, context: &str) -> Result<Vec<u16>, FormatError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(FormatError::at(
            absolute,
            format!("{context} length is not a multiple of two bytes"),
        ));
    }
    let mut reader = Reader::new(bytes);
    let mut values = Vec::with_capacity(bytes.len() / 2);
    while reader.remaining() != 0 {
        values.push(reader.u16_le()?);
    }
    Ok(values)
}

fn validate_pc(
    raw: u16,
    code_len: usize,
    absolute: usize,
    context: &str,
) -> Result<Option<usize>, FormatError> {
    if raw == GOOL_PC_NONE {
        return Ok(None);
    }
    let pc = usize::from(raw);
    if pc >= code_len {
        return Err(FormatError::at(
            absolute,
            format!("GOOL {context} PC {pc} is outside {code_len} code words"),
        ));
    }
    Ok(Some(pc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::{ENTRY_MAGIC, LevelId, NSF_PAGE_SIZE, parse_nsd, parse_nsf};
    use proptest::prelude::*;

    const MODERN_HEADER_SIZE: usize = 0x520;
    const LDAT_PREFIX_SIZE: usize = 0x118;
    const GLOBAL_EID_NAME: &str = "glob1";
    const EXTERNAL_EID_NAME: &str = "code1";

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn push_u16(bytes: &mut Vec<u8>, value: u16) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u32(bytes: &mut Vec<u8>, value: u32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn one_page_nsd() -> super::super::Nsd {
        let ldat_offset = MODERN_HEADER_SIZE + 16;
        let mut bytes = vec![0_u8; ldat_offset + LDAT_PREFIX_SIZE];
        put_u32(&mut bytes, 0x400, 1);
        put_u32(&mut bytes, 0x404, 2);
        put_u32(&mut bytes, MODERN_HEADER_SIZE, 1);
        put_u32(
            &mut bytes,
            MODERN_HEADER_SIZE + 4,
            Eid::from_name(GLOBAL_EID_NAME).unwrap().raw(),
        );
        put_u32(&mut bytes, MODERN_HEADER_SIZE + 8, 1);
        put_u32(
            &mut bytes,
            MODERN_HEADER_SIZE + 12,
            Eid::from_name(EXTERNAL_EID_NAME).unwrap().raw(),
        );
        put_u32(&mut bytes, ldat_offset, 1);
        put_u32(&mut bytes, ldat_offset + 4, LevelId::TITLE.get());
        parse_nsd(&bytes, LevelId::TITLE).unwrap()
    }

    fn entry(eid: Eid, items: &[Vec<u8>]) -> Vec<u8> {
        let table_end = 16 + (items.len() + 1) * 4;
        let total = table_end + items.iter().map(Vec::len).sum::<usize>();
        assert!(
            total.is_multiple_of(4),
            "test entries must keep page offsets aligned"
        );
        let mut bytes = vec![0_u8; table_end];
        put_u32(&mut bytes, 0, ENTRY_MAGIC);
        put_u32(&mut bytes, 4, eid.raw());
        put_u32(&mut bytes, 8, 2);
        put_u32(&mut bytes, 12, items.len() as u32);
        let mut cursor = table_end;
        for (index, item) in items.iter().enumerate() {
            put_u32(&mut bytes, 16 + index * 4, cursor as u32);
            bytes.extend_from_slice(item);
            cursor += item.len();
        }
        put_u32(&mut bytes, 16 + items.len() * 4, cursor as u32);
        bytes
    }

    fn program_nsf(state_code_pc: u16, subtype_state: u16) -> (Vec<u8>, Eid) {
        let global_eid = Eid::from_name(GLOBAL_EID_NAME).unwrap();
        let external_eid = Eid::from_name(EXTERNAL_EID_NAME).unwrap();

        let mut header = Vec::new();
        for word in [1, 0x100, 0, 32, 1, 0] {
            push_u32(&mut header, word);
        }
        let mut shared_code = Vec::new();
        push_u32(&mut shared_code, 0x8200_0000);
        let mut internal = Vec::new();
        push_u32(&mut internal, external_eid.raw());
        let mut maps = Vec::new();
        for value in [0xffff, 0x00ff, 0x00ff, subtype_state] {
            push_u16(&mut maps, value);
        }
        let mut states = Vec::new();
        push_u32(&mut states, 0x1122_3344);
        push_u32(&mut states, 0x5566_7788);
        push_u16(&mut states, 0);
        push_u16(&mut states, GOOL_PC_NONE);
        push_u16(&mut states, 1);
        push_u16(&mut states, state_code_pc);
        let animation_data = vec![0xde, 0xad, 0xbe, 0xef];
        let global = entry(
            global_eid,
            &[header, shared_code, internal, maps, states, animation_data],
        );

        let mut code = Vec::new();
        push_u32(&mut code, 0x0000_0000);
        push_u32(&mut code, 0x8200_0000);
        let mut external_data = Vec::new();
        push_u32(&mut external_data, 0x1234_5678);
        let external = entry(external_eid, &[Vec::new(), code, external_data]);

        let table_end = 16 + 3 * 4;
        let global_start = table_end;
        let external_start = global_start + global.len();
        let end = external_start + external.len();
        let mut bytes = vec![0_u8; NSF_PAGE_SIZE];
        put_u16(&mut bytes, 0, 0x1234);
        put_u16(&mut bytes, 2, 0);
        put_u32(&mut bytes, 4, 1);
        put_u32(&mut bytes, 8, 2);
        put_u32(&mut bytes, 16, global_start as u32);
        put_u32(&mut bytes, 20, external_start as u32);
        put_u32(&mut bytes, 24, end as u32);
        bytes[global_start..external_start].copy_from_slice(&global);
        bytes[external_start..end].copy_from_slice(&external);
        (bytes, global_eid)
    }

    #[test]
    fn resolves_subtype_state_and_external_code_without_pointer_relocation() {
        let metadata = one_page_nsd();
        let (bytes, global_eid) = program_nsf(0, 0);
        let nsf = parse_nsf(&bytes, &metadata).unwrap();
        let program = load_gool_program(&metadata, &nsf, &bytes, global_eid, 2).unwrap();

        assert_eq!(program.global_eid(), global_eid);
        assert_eq!(
            program.external_eid().name().as_deref(),
            Some(EXTERNAL_EID_NAME)
        );
        assert_eq!(program.header().initial_stack_pointer, 32);
        assert_eq!(program.state_index(), 0);
        assert_eq!(program.state().flags, 0x1122_3344);
        assert_eq!(program.states(), &[program.state()]);
        assert_eq!(program.global_code(), &[0x8200_0000]);
        assert_eq!(program.code(), &[0, 0x8200_0000]);
        assert_eq!(program.internal_words(), &[program.external_eid().raw()]);
        assert_eq!(program.animation_data(), &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(program.external_words(), &[0x1234_5678]);
        assert_eq!(program.code_pc(), Some(0));
        assert_eq!(program.event_pc(), None);
        assert_eq!(program.transition_pc(), Some(1));

        let rebound = load_gool_state_program(&metadata, &nsf, &bytes, global_eid, 0).unwrap();
        assert_eq!(rebound, program);
        assert!(load_gool_state_program(&metadata, &nsf, &bytes, global_eid, 1).is_err());
    }

    #[test]
    fn rejects_invalid_subtypes_and_code_offsets() {
        let metadata = one_page_nsd();
        let (bytes, global_eid) = program_nsf(0, 0x00ff);
        let nsf = parse_nsf(&bytes, &metadata).unwrap();
        assert!(load_gool_program(&metadata, &nsf, &bytes, global_eid, 2).is_err());
        assert!(load_gool_program(&metadata, &nsf, &bytes, global_eid, 3).is_err());

        let (bytes, global_eid) = program_nsf(2, 0);
        let nsf = parse_nsf(&bytes, &metadata).unwrap();
        let error = load_gool_program(&metadata, &nsf, &bytes, global_eid, 2).unwrap_err();
        assert!(error.message().contains("code PC 2"));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn mutated_gool_entry_graph_never_panics(
            index in 0_usize..NSF_PAGE_SIZE,
            value in any::<u8>(),
            subtype in any::<u16>(),
        ) {
            let metadata = one_page_nsd();
            let (mut bytes, global_eid) = program_nsf(0, 0);
            bytes[index] = value;
            if let Ok(nsf) = parse_nsf(&bytes, &metadata) {
                let _ = load_gool_program(&metadata, &nsf, &bytes, global_eid, subtype);
            }
        }
    }
}
