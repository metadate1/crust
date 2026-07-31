use crust_platform::persistence::{
    CARD_STORAGE_KEY, CardSlot, RESUME_STORAGE_KEY, ResumeRecord, encode_resume,
    encode_virtual_card, invalid_resume_key,
};
use crust_sim::card::{
    CardOperation, CardPayload, ResumeLoadResult, ResumeManager, SaveData, Slot, VirtualCard,
};
use wasm_bindgen::JsValue;
use web_sys::Storage;

use crate::card_persistence::{
    CardPersistIntent, CardRecordState, ResumeRecordDisposition, load_resume_record,
};

#[derive(Clone, Debug)]
pub struct StorageState {
    storage: Storage,
    card_record: CardRecordState,
    writes_enabled: bool,
    card_writes_deferred: bool,
    deferred_card_dirty: bool,
}

impl StorageState {
    pub fn open() -> Result<Self, JsValue> {
        let storage = web_sys::window()
            .ok_or_else(|| JsValue::from_str("browser window is unavailable"))?
            .local_storage()?
            .ok_or_else(|| JsValue::from_str("localStorage is unavailable"))?;
        let stored_card = storage.get_item(CARD_STORAGE_KEY)?;
        let card_record = CardRecordState::load(stored_card.as_deref());
        Ok(Self {
            storage,
            card_record,
            writes_enabled: true,
            card_writes_deferred: false,
            deferred_card_dirty: false,
        })
    }

    /// Forks the card persistence state for a checked runtime transaction.
    /// Mutations update only the fork until [`Self::commit_card_transaction`]
    /// succeeds, so a rejected GOOL/restart transaction cannot leak a partial
    /// localStorage write.
    #[must_use]
    pub fn begin_card_transaction(&self) -> Self {
        let mut transaction = self.clone();
        transaction.card_writes_deferred = true;
        transaction.deferred_card_dirty = false;
        transaction
    }

    /// Publishes a previously deferred card record atomically at the host
    /// boundary and converts this fork back into ordinary storage state.
    pub fn commit_card_transaction(&mut self) -> Result<(), JsValue> {
        if self.card_writes_deferred && self.deferred_card_dirty && self.writes_enabled {
            let json = encode_virtual_card(self.card_record.record())
                .map_err(|error| JsValue::from_str(&error.to_string()))?;
            self.storage.set_item(CARD_STORAGE_KEY, &json)?;
        }
        self.card_writes_deferred = false;
        self.deferred_card_dirty = false;
        Ok(())
    }

    /// Keeps the mounted browser card readable while making every subsequent
    /// resume/card write ephemeral for the lifetime of this runtime.
    pub const fn keep_writes_in_memory(&mut self) {
        self.writes_enabled = false;
    }

    pub fn virtual_card(&self) -> VirtualCard {
        let mut card = VirtualCard::new();
        if !self.card_record.is_readable() {
            card.set_storage_available(false);
            let _ = card.control(CardOperation::Rescan, 0, None);
            return card;
        }
        for (index, slot) in self.card_record.record().slots.iter().enumerate() {
            let value = match slot {
                CardSlot::Empty => Slot::Empty,
                CardSlot::Valid { payload, .. } => {
                    CardPayload::from_bytes(**payload).map_or(Slot::Corrupt, Slot::Valid)
                }
                CardSlot::Damaged { .. } => Slot::Corrupt,
            };
            let _ = card.set_slot(index, value);
        }
        let _ = card.control(CardOperation::Rescan, 0, None);
        card.update();
        let _ = card.control(CardOperation::ClearFlag6, 0, None);
        card.update();
        card
    }

    #[must_use]
    pub const fn card_operation_available(&self, operation: CardOperation) -> bool {
        self.card_record.can_service(operation)
    }

    pub fn persist_card(
        &mut self,
        card: &VirtualCard,
        intent: CardPersistIntent,
    ) -> Result<(), JsValue> {
        let timestamp = now_timestamp();
        let next_record = self
            .card_record
            .merged(card, timestamp, intent)
            .ok_or_else(|| {
                JsValue::from_str("virtual card record is unreadable; format required")
            })?;
        if self.writes_enabled {
            let json = encode_virtual_card(&next_record)
                .map_err(|error| JsValue::from_str(&error.to_string()))?;
            if !self.card_writes_deferred {
                self.storage.set_item(CARD_STORAGE_KEY, &json)?;
            }
        }
        self.card_record.replace(next_record);
        self.deferred_card_dirty |= self.card_writes_deferred;
        Ok(())
    }

    pub fn load_resume(
        &self,
        current: SaveData,
    ) -> Result<(ResumeManager, ResumeLoadResult), JsValue> {
        let Some(json) = self.storage.get_item(RESUME_STORAGE_KEY)? else {
            return Ok(ResumeManager::load(None, current));
        };
        let (manager, result, disposition) = load_resume_record(&json, current);
        if disposition == ResumeRecordDisposition::Quarantine && self.writes_enabled {
            let key = invalid_resume_key(now_timestamp());
            self.storage.set_item(&key, &json)?;
            self.storage.remove_item(RESUME_STORAGE_KEY)?;
        }
        Ok((manager, result))
    }

    pub fn persist_resume(&self, payload: CardPayload) -> Result<(), JsValue> {
        if !self.writes_enabled {
            return Ok(());
        }
        let record = ResumeRecord {
            payload: Box::new(payload.into_bytes()),
            updated_at: now_timestamp(),
        };
        let json = encode_resume(&record).map_err(|error| JsValue::from_str(&error.to_string()))?;
        self.storage.set_item(RESUME_STORAGE_KEY, &json)
    }
}

#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn now_timestamp() -> u64 {
    // ECMAScript TimeClip constrains a valid Date value to +/-8.64e15 milliseconds, well within
    // `u64`; clamping negatives and truncating the sub-millisecond fraction is intentional here.
    js_sys::Date::now().max(0.0) as u64
}
