//! Retail title-state MDAT lookup and entity parsing.
//!
//! MDAT entries share the exact serialized entity layout used by ZDAT, but
//! keep those descriptors after the fixed header and one reserved item.  The
//! source runtime rewrote their parent words to pointers; this module retains
//! the entry EID and parses every descriptor without relocation.

use crate::binary::{EID_ALPHABET, Eid, FormatError};

use super::{Nsd, Nsf, ZoneEntity, structs::MdatHeader};

/// NSF entry type used by retail title-state MDAT assets.
pub const MDAT_ENTRY_TYPE: u32 = 17;

/// One validated title-state MDAT and its pointer-free entity descriptors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleMdat {
    /// Stable entry identity (`0MapP` through `_MapP`).
    pub eid: Eid,
    /// Exact fixed MDAT metadata.
    pub header: MdatHeader,
    /// Entities in serialized item order.
    pub entities: Vec<ZoneEntity>,
}

/// Returns the retail `{state-character}MapP` entry identity.
///
/// The state byte is an index into the same 64-character alphabet used by all
/// EIDs.  It is not interpreted as ASCII or formatted as decimal.
pub fn title_mdat_eid(state: u8) -> Result<Eid, FormatError> {
    let first = *EID_ALPHABET.get(usize::from(state)).ok_or_else(|| {
        FormatError::global(format!(
            "title state {state} exceeds the 64-entry EID alphabet"
        ))
    })?;
    let name = [first, b'M', b'a', b'p', b'P'];
    let name = core::str::from_utf8(&name)
        .map_err(|_| FormatError::global("EID alphabet is not ASCII"))?;
    Eid::from_name(name)
}

/// Resolves and parses one retail title-state MDAT.
///
/// Entity item zero in the source is the fixed header, item one is reserved,
/// and entity `i` is item `2 + i`.  Every range and count is validated before
/// bytes reach [`ZoneEntity::parse`].
pub fn load_title_mdat(
    metadata: &Nsd,
    nsf: &Nsf,
    nsf_bytes: &[u8],
    state: u8,
) -> Result<TitleMdat, FormatError> {
    let eid = title_mdat_eid(state)?;
    let entry = nsf.resolve_entry(metadata, eid)?;
    if entry.entry_type != MDAT_ENTRY_TYPE {
        return Err(FormatError::global(format!(
            "title asset {eid} has entry type {}; expected {MDAT_ENTRY_TYPE}",
            entry.entry_type
        )));
    }
    let header_item = entry
        .item(0)
        .ok_or_else(|| FormatError::global(format!("title asset {eid} is missing item 0")))?;
    let header = MdatHeader::parse(header_item.bytes(nsf_bytes)?)?;
    let entity_count = usize::try_from(header.entity_count)
        .map_err(|_| FormatError::global("MDAT entity count does not fit this host"))?;
    let required_items = 2_usize
        .checked_add(entity_count)
        .ok_or_else(|| FormatError::global("MDAT entity item range overflows"))?;
    if entry.items.len() < required_items {
        return Err(FormatError::global(format!(
            "title asset {eid} declares {entity_count} entities but has only {} items",
            entry.items.len()
        )));
    }

    let mut entities = Vec::with_capacity(entity_count);
    for entity_index in 0..entity_count {
        let item_index = 2 + entity_index;
        let item = entry.item(item_index).ok_or_else(|| {
            FormatError::global(format!("title asset {eid} is missing item {item_index}"))
        })?;
        entities.push(ZoneEntity::parse(item.bytes(nsf_bytes)?)?);
    }
    Ok(TitleMdat {
        eid,
        header,
        entities,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_state_uses_the_eid_alphabet_not_decimal_formatting() {
        assert_eq!(title_mdat_eid(5).unwrap().name().as_deref(), Some("5MapP"));
        assert_eq!(title_mdat_eid(10).unwrap().name().as_deref(), Some("aMapP"));
        assert_eq!(title_mdat_eid(15).unwrap().name().as_deref(), Some("fMapP"));
        assert_eq!(title_mdat_eid(63).unwrap().name().as_deref(), Some("!MapP"));
        assert!(title_mdat_eid(64).is_err());
    }
}
