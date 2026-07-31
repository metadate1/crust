use crust_platform::persistence::{
    CardSlot, PersistenceError, ResumeRecord, VirtualCardRecord, decode_resume, decode_virtual_card,
};
use crust_sim::card::{
    CardOperation, ResumeLoadResult, ResumeManager, SaveData, Slot, StoredResume, VirtualCard,
};

/// Decoded browser-card state plus whether the complete storage envelope was
/// readable.
///
/// A malformed individual slot remains a readable [`VirtualCardRecord`] and is
/// represented as a damaged part. A malformed envelope is different: ordinary
/// card operations must report a storage error, while retail's explicit format
/// operation must still be able to replace it with an empty valid card.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CardRecordState {
    record: VirtualCardRecord,
    readable: bool,
}

impl CardRecordState {
    #[must_use]
    pub(crate) fn load(json: Option<&str>) -> Self {
        match json {
            None => Self {
                record: VirtualCardRecord::default(),
                readable: true,
            },
            Some(json) => match decode_virtual_card(json) {
                Ok(record) => Self {
                    record,
                    readable: true,
                },
                Err(_) => Self {
                    record: VirtualCardRecord::default(),
                    readable: false,
                },
            },
        }
    }

    #[must_use]
    pub(crate) const fn record(&self) -> &VirtualCardRecord {
        &self.record
    }

    #[must_use]
    pub(crate) const fn is_readable(&self) -> bool {
        self.readable
    }

    /// Mirrors the source browser backend: a broken card record rejects reads,
    /// scans, and writes, but FORMAT bypasses parsing and may repair the record.
    #[must_use]
    pub(crate) const fn can_service(&self, operation: CardOperation) -> bool {
        self.readable || matches!(operation, CardOperation::Format)
    }

    #[must_use]
    pub(crate) fn merged(
        &self,
        card: &VirtualCard,
        timestamp: u64,
        intent: CardPersistIntent,
    ) -> Option<VirtualCardRecord> {
        if !self.readable && intent != CardPersistIntent::Format {
            return None;
        }
        Some(merge_card_record(&self.record, card, timestamp, intent))
    }

    pub(crate) fn replace(&mut self, record: VirtualCardRecord) {
        self.record = record;
        self.readable = true;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResumeRecordDisposition {
    Preserve,
    Quarantine,
}

/// Decodes and validates both layers of a browser resume record.
///
/// `decode_resume` validates the JSON/base64 envelope, while
/// `ResumeManager::load` validates the retail payload checksum. Both kinds of
/// corruption must be quarantined; a newer version is intentionally preserved.
#[must_use]
pub(crate) fn load_resume_record(
    json: &str,
    current: SaveData,
) -> (ResumeManager, ResumeLoadResult, ResumeRecordDisposition) {
    let stored = match decode_resume(json) {
        Ok(ResumeRecord { payload, .. }) => StoredResume {
            schema: crust_sim::card::RESUME_SCHEMA.to_owned(),
            version: crust_sim::card::RESUME_VERSION,
            payload: payload.to_vec(),
        },
        Err(PersistenceError::UnsupportedVersion(version)) => StoredResume {
            schema: crust_sim::card::RESUME_SCHEMA.to_owned(),
            version,
            payload: Vec::new(),
        },
        Err(_) => StoredResume {
            schema: "invalid".to_owned(),
            version: 0,
            payload: Vec::new(),
        },
    };
    let (manager, result) = ResumeManager::load(Some(stored), current);
    let disposition = if result == ResumeLoadResult::Corrupt {
        ResumeRecordDisposition::Quarantine
    } else {
        ResumeRecordDisposition::Preserve
    };
    (manager, result, disposition)
}

/// Identifies the browser-card operation that caused a persistence snapshot.
///
/// The source browser backend writes only one physical slot for a save, so an
/// explicit write refreshes that slot's timestamp even when its payload bytes
/// are unchanged. A passive snapshot instead refreshes only slots whose
/// contents actually differ from the last record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CardPersistIntent {
    Snapshot,
    WriteSlot(usize),
    Format,
}

#[must_use]
pub(crate) fn merge_card_record(
    previous: &VirtualCardRecord,
    card: &VirtualCard,
    timestamp: u64,
    intent: CardPersistIntent,
) -> VirtualCardRecord {
    let mut next = previous.clone();
    for (index, slot) in card.slots().iter().copied().enumerate() {
        let explicitly_written = intent == CardPersistIntent::WriteSlot(index);
        next.slots[index] = match slot {
            Slot::Empty => CardSlot::Empty,
            Slot::Valid(payload) => {
                let payload = Box::new(payload.into_bytes());
                let updated_at = match &previous.slots[index] {
                    CardSlot::Valid {
                        payload: previous_payload,
                        updated_at,
                    } if !explicitly_written && previous_payload == &payload => *updated_at,
                    _ => timestamp,
                };
                CardSlot::Valid {
                    payload,
                    updated_at,
                }
            }
            Slot::Corrupt => match &previous.slots[index] {
                CardSlot::Damaged {
                    encoded_payload,
                    updated_at,
                } => CardSlot::Damaged {
                    encoded_payload: encoded_payload.clone(),
                    updated_at: *updated_at,
                },
                _ => CardSlot::Damaged {
                    encoded_payload: "damaged".to_owned(),
                    updated_at: timestamp,
                },
            },
        };
    }

    if intent != CardPersistIntent::Snapshot || next.slots != previous.slots {
        next.updated_at = timestamp;
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use crust_platform::persistence::{
        CARD_SCHEMA, STORAGE_VERSION, encode_resume, encode_virtual_card,
    };
    use crust_sim::card::{CardPayload, SaveData};

    fn payload(level_count: u32) -> CardPayload {
        CardPayload::encode(SaveData {
            level_count,
            ..SaveData::default()
        })
    }

    fn record_with_two_slots() -> VirtualCardRecord {
        let mut record = VirtualCardRecord {
            updated_at: 30,
            ..VirtualCardRecord::default()
        };
        record.slots[2] = CardSlot::Valid {
            payload: Box::new(payload(2).into_bytes()),
            updated_at: 10,
        };
        record.slots[7] = CardSlot::Valid {
            payload: Box::new(payload(7).into_bytes()),
            updated_at: 20,
        };
        record
    }

    fn card_from_record(record: &VirtualCardRecord) -> VirtualCard {
        let mut card = VirtualCard::new();
        for (index, slot) in record.slots.iter().enumerate() {
            if let CardSlot::Valid { payload, .. } = slot {
                card.set_slot(
                    index,
                    Slot::Valid(CardPayload::from_bytes(**payload).unwrap()),
                )
                .unwrap();
            }
        }
        card
    }

    #[test]
    fn writing_one_slot_preserves_other_valid_slot_timestamps() {
        let previous = record_with_two_slots();
        let mut card = card_from_record(&previous);
        card.set_slot(7, Slot::Valid(payload(8))).unwrap();

        let next = merge_card_record(&previous, &card, 99, CardPersistIntent::WriteSlot(7));

        assert_eq!(next.updated_at, 99);
        assert!(matches!(
            &next.slots[2],
            CardSlot::Valid { updated_at: 10, .. }
        ));
        assert!(matches!(
            &next.slots[7],
            CardSlot::Valid { updated_at: 99, .. }
        ));
    }

    #[test]
    fn an_explicit_write_refreshes_its_timestamp_even_when_payload_is_unchanged() {
        let previous = record_with_two_slots();
        let card = card_from_record(&previous);

        let next = merge_card_record(&previous, &card, 99, CardPersistIntent::WriteSlot(7));

        assert!(matches!(
            &next.slots[2],
            CardSlot::Valid { updated_at: 10, .. }
        ));
        assert!(matches!(
            &next.slots[7],
            CardSlot::Valid { updated_at: 99, .. }
        ));
    }

    #[test]
    fn an_unchanged_passive_snapshot_preserves_slot_and_card_timestamps() {
        let previous = record_with_two_slots();
        let card = card_from_record(&previous);

        let next = merge_card_record(&previous, &card, 99, CardPersistIntent::Snapshot);

        assert_eq!(next, previous);
    }

    #[test]
    fn a_passive_snapshot_refreshes_only_slots_whose_payload_changed() {
        let previous = record_with_two_slots();
        let mut card = card_from_record(&previous);
        card.set_slot(7, Slot::Valid(payload(8))).unwrap();

        let next = merge_card_record(&previous, &card, 99, CardPersistIntent::Snapshot);

        assert_eq!(next.updated_at, 99);
        assert!(matches!(
            &next.slots[2],
            CardSlot::Valid { updated_at: 10, .. }
        ));
        assert!(matches!(
            &next.slots[7],
            CardSlot::Valid { updated_at: 99, .. }
        ));
    }

    #[test]
    fn format_clears_slots_and_refreshes_the_card_timestamp() {
        let previous = record_with_two_slots();
        let card = VirtualCard::new();

        let next = merge_card_record(&previous, &card, 99, CardPersistIntent::Format);

        assert_eq!(next.updated_at, 99);
        assert!(next.slots.iter().all(|slot| *slot == CardSlot::Empty));
    }

    #[test]
    fn malformed_card_envelope_remains_format_repairable() {
        let mut state = CardRecordState::load(Some("not json"));

        assert!(!state.is_readable());
        assert!(!state.can_service(CardOperation::Rescan));
        assert!(!state.can_service(CardOperation::SaveSelected));
        assert!(state.can_service(CardOperation::Format));
        assert!(
            state
                .merged(&VirtualCard::new(), 99, CardPersistIntent::Snapshot)
                .is_none()
        );

        let repaired = state
            .merged(&VirtualCard::new(), 99, CardPersistIntent::Format)
            .expect("format bypasses the unreadable envelope");
        state.replace(repaired);

        assert!(state.is_readable());
        assert!(state.can_service(CardOperation::Rescan));
        assert_eq!(state.record().updated_at, 99);
        assert!(
            state
                .record()
                .slots
                .iter()
                .all(|slot| *slot == CardSlot::Empty)
        );
        let encoded = encode_virtual_card(state.record()).unwrap();
        assert_eq!(CardRecordState::load(Some(&encoded)), state);
    }

    #[test]
    fn wrong_card_schema_and_version_require_explicit_format() {
        for json in [
            format!(r#"{{"schema":"not-{CARD_SCHEMA}","version":{STORAGE_VERSION},"slots":[]}}"#),
            format!(
                r#"{{"schema":"{CARD_SCHEMA}","version":{},"slots":[]}}"#,
                STORAGE_VERSION + 1
            ),
        ] {
            let state = CardRecordState::load(Some(&json));
            assert!(!state.is_readable());
            assert!(state.can_service(CardOperation::Format));
            assert!(!state.can_service(CardOperation::LoadSelected));
        }
    }

    #[test]
    fn checksum_corrupt_resume_is_selected_for_quarantine() {
        let mut bytes = CardPayload::encode(SaveData::default()).into_bytes();
        bytes[0] ^= 0x80;
        let json = encode_resume(&ResumeRecord {
            payload: Box::new(bytes),
            updated_at: 42,
        })
        .unwrap();

        let (_, result, disposition) = load_resume_record(&json, SaveData::default());

        assert_eq!(result, ResumeLoadResult::Corrupt);
        assert_eq!(disposition, ResumeRecordDisposition::Quarantine);
    }

    #[test]
    fn newer_resume_version_is_preserved() {
        let json = format!(
            r#"{{"schema":"{}","version":{},"payload":"","updatedAt":0}}"#,
            crust_sim::card::RESUME_SCHEMA,
            crust_sim::card::RESUME_VERSION + 1
        );

        let (_, result, disposition) = load_resume_record(&json, SaveData::default());

        assert_eq!(result, ResumeLoadResult::NewerVersion);
        assert_eq!(disposition, ResumeRecordDisposition::Preserve);
    }
}
