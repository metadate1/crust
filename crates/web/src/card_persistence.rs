use crust_platform::persistence::{CardSlot, VirtualCardRecord};
use crust_sim::card::{Slot, VirtualCard};

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
}
