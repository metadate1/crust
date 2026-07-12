//! Versioned local-storage envelopes compatible with the browser C1 format.

use std::array;
use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CARD_STORAGE_KEY: &str = "c1.virtual-memory-card.v1";
pub const CARD_SCHEMA: &str = "c1-virtual-memory-card";
pub const RESUME_STORAGE_KEY: &str = "c1.browser-resume.v1";
pub const RESUME_SCHEMA: &str = "c1-browser-resume";
pub const STORAGE_VERSION: u32 = 1;
pub const SLOT_COUNT: usize = 15;
pub const PAYLOAD_BYTES: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PersistenceError {
    Json(String),
    Schema {
        expected: &'static str,
        actual: String,
    },
    UnsupportedVersion(u32),
    OutdatedVersion(u32),
    MissingPayload,
    InvalidBase64,
    InvalidPayloadLength(usize),
}

impl fmt::Display for PersistenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(message) => write!(formatter, "invalid persistence JSON: {message}"),
            Self::Schema { expected, actual } => {
                write!(formatter, "expected schema {expected}, found {actual}")
            }
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported persistence version {version}")
            }
            Self::OutdatedVersion(version) => {
                write!(formatter, "outdated persistence version {version}")
            }
            Self::MissingPayload => formatter.write_str("persistence record has no payload"),
            Self::InvalidBase64 => formatter.write_str("persistence payload is not base64"),
            Self::InvalidPayloadLength(length) => {
                write!(
                    formatter,
                    "persistence payload has {length} bytes, expected 128"
                )
            }
        }
    }
}

impl std::error::Error for PersistenceError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CardSlot {
    Empty,
    Valid {
        payload: Box<[u8; PAYLOAD_BYTES]>,
        updated_at: u64,
    },
    Damaged {
        encoded_payload: String,
        updated_at: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VirtualCardRecord {
    pub slots: [CardSlot; SLOT_COUNT],
    pub updated_at: u64,
}

impl Default for VirtualCardRecord {
    fn default() -> Self {
        Self {
            slots: array::from_fn(|_| CardSlot::Empty),
            updated_at: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResumeRecord {
    pub payload: Box<[u8; PAYLOAD_BYTES]>,
    pub updated_at: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CardEnvelope {
    schema: String,
    version: u32,
    slots: Vec<Option<SlotEnvelope>>,
    updated_at: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredCardEnvelope {
    schema: String,
    version: u32,
    slots: Vec<Option<Value>>,
    #[serde(default)]
    updated_at: Option<Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SlotEnvelope {
    payload: String,
    updated_at: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResumeEnvelope {
    schema: String,
    version: u32,
    payload: String,
    updated_at: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredResumeEnvelope {
    schema: String,
    version: u32,
    #[serde(default)]
    payload: Option<String>,
    #[serde(default)]
    updated_at: Option<Value>,
}

#[must_use]
pub fn invalid_resume_key(timestamp: u64) -> String {
    format!("{RESUME_STORAGE_KEY}.invalid.{timestamp}")
}

/// Decodes a versioned card record while retaining malformed individual slots.
///
/// # Errors
///
/// Returns an error when the envelope is not JSON, has the wrong schema, or
/// uses an unsupported card format version. Slot payload errors are represented
/// as [`CardSlot::Damaged`] instead of invalidating the other slots.
pub fn decode_virtual_card(json: &str) -> Result<VirtualCardRecord, PersistenceError> {
    let envelope: StoredCardEnvelope =
        serde_json::from_str(json).map_err(|error| PersistenceError::Json(error.to_string()))?;
    validate_header(&envelope.schema, envelope.version, CARD_SCHEMA)?;
    let mut slots = array::from_fn(|_| CardSlot::Empty);
    for (index, slot) in envelope.slots.into_iter().take(SLOT_COUNT).enumerate() {
        let Some(stored) = slot else {
            continue;
        };
        let updated_at = stored
            .get("updatedAt")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let Some(encoded_payload) = stored.get("payload").and_then(Value::as_str) else {
            slots[index] = CardSlot::Damaged {
                encoded_payload: stored.to_string(),
                updated_at,
            };
            continue;
        };
        slots[index] = match decode_payload(encoded_payload) {
            Ok(payload) => CardSlot::Valid {
                payload,
                updated_at,
            },
            Err(_) => CardSlot::Damaged {
                encoded_payload: encoded_payload.to_owned(),
                updated_at,
            },
        };
    }
    Ok(VirtualCardRecord {
        slots,
        updated_at: envelope
            .updated_at
            .as_ref()
            .and_then(Value::as_u64)
            .unwrap_or_default(),
    })
}

/// Encodes exactly 15 card slots in the established browser schema.
///
/// # Errors
///
/// Returns [`PersistenceError::Json`] if serialization fails.
pub fn encode_virtual_card(record: &VirtualCardRecord) -> Result<String, PersistenceError> {
    let slots = record
        .slots
        .iter()
        .map(|slot| match slot {
            CardSlot::Empty => None,
            CardSlot::Valid {
                payload,
                updated_at,
            } => Some(SlotEnvelope {
                payload: STANDARD.encode(payload.as_slice()),
                updated_at: *updated_at,
            }),
            CardSlot::Damaged {
                encoded_payload,
                updated_at,
            } => Some(SlotEnvelope {
                payload: encoded_payload.clone(),
                updated_at: *updated_at,
            }),
        })
        .collect();
    serde_json::to_string(&CardEnvelope {
        schema: CARD_SCHEMA.to_owned(),
        version: STORAGE_VERSION,
        slots,
        updated_at: record.updated_at,
    })
    .map_err(|error| PersistenceError::Json(error.to_string()))
}

/// Decodes a browser resume record.
///
/// # Errors
///
/// Returns an error for malformed JSON, schema/version mismatches, missing or
/// malformed base64, and payloads that are not exactly 128 bytes. Newer
/// versions use [`PersistenceError::UnsupportedVersion`], while older versions
/// use [`PersistenceError::OutdatedVersion`] so callers can quarantine them.
pub fn decode_resume(json: &str) -> Result<ResumeRecord, PersistenceError> {
    let envelope: StoredResumeEnvelope =
        serde_json::from_str(json).map_err(|error| PersistenceError::Json(error.to_string()))?;
    validate_resume_header(&envelope.schema, envelope.version)?;
    Ok(ResumeRecord {
        payload: decode_payload(
            envelope
                .payload
                .as_deref()
                .ok_or(PersistenceError::MissingPayload)?,
        )?,
        updated_at: envelope
            .updated_at
            .as_ref()
            .and_then(Value::as_u64)
            .unwrap_or_default(),
    })
}

/// Encodes a 128-byte resume payload in the established browser schema.
///
/// # Errors
///
/// Returns [`PersistenceError::Json`] if serialization fails.
pub fn encode_resume(record: &ResumeRecord) -> Result<String, PersistenceError> {
    serde_json::to_string(&ResumeEnvelope {
        schema: RESUME_SCHEMA.to_owned(),
        version: STORAGE_VERSION,
        payload: STANDARD.encode(record.payload.as_slice()),
        updated_at: record.updated_at,
    })
    .map_err(|error| PersistenceError::Json(error.to_string()))
}

fn validate_resume_header(schema: &str, version: u32) -> Result<(), PersistenceError> {
    if schema != RESUME_SCHEMA {
        return Err(PersistenceError::Schema {
            expected: RESUME_SCHEMA,
            actual: schema.to_owned(),
        });
    }
    if version > STORAGE_VERSION {
        return Err(PersistenceError::UnsupportedVersion(version));
    }
    if version < STORAGE_VERSION {
        return Err(PersistenceError::OutdatedVersion(version));
    }
    Ok(())
}

fn validate_header(
    schema: &str,
    version: u32,
    expected_schema: &'static str,
) -> Result<(), PersistenceError> {
    if schema != expected_schema {
        return Err(PersistenceError::Schema {
            expected: expected_schema,
            actual: schema.to_owned(),
        });
    }
    if version != STORAGE_VERSION {
        return Err(PersistenceError::UnsupportedVersion(version));
    }
    Ok(())
}

fn decode_payload(encoded: &str) -> Result<Box<[u8; PAYLOAD_BYTES]>, PersistenceError> {
    let decoded = STANDARD
        .decode(encoded)
        .map_err(|_| PersistenceError::InvalidBase64)?;
    let length = decoded.len();
    decoded
        .into_boxed_slice()
        .try_into()
        .map_err(|_| PersistenceError::InvalidPayloadLength(length))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn payload(value: u8) -> [u8; PAYLOAD_BYTES] {
        [value; PAYLOAD_BYTES]
    }

    #[test]
    fn formats_exactly_fifteen_slots() {
        let encoded = encode_virtual_card(&VirtualCardRecord::default()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(value["schema"], CARD_SCHEMA);
        assert_eq!(value["version"], STORAGE_VERSION);
        assert_eq!(value["slots"].as_array().unwrap().len(), SLOT_COUNT);
    }

    #[test]
    fn valid_card_round_trips_opaque_payload() {
        let mut card = VirtualCardRecord {
            updated_at: 200,
            ..VirtualCardRecord::default()
        };
        card.slots[3] = CardSlot::Valid {
            payload: Box::new(payload(0xa5)),
            updated_at: 100,
        };
        let encoded = encode_virtual_card(&card).unwrap();
        assert_eq!(decode_virtual_card(&encoded).unwrap(), card);
    }

    #[test]
    fn damaged_slot_is_preserved_not_formatted() {
        let encoded = format!(
            r#"{{"schema":"{CARD_SCHEMA}","version":1,"slots":[{{"payload":"bad!","updatedAt":9}}],"updatedAt":10}}"#
        );
        let card = decode_virtual_card(&encoded).unwrap();
        assert_eq!(
            card.slots[0],
            CardSlot::Damaged {
                encoded_payload: "bad!".to_owned(),
                updated_at: 9,
            }
        );
        let reencoded = encode_virtual_card(&card).unwrap();
        assert!(reencoded.contains("bad!"));
    }

    #[test]
    fn resume_rejects_bad_payload_for_quarantine() {
        let encoded =
            format!(r#"{{"schema":"{RESUME_SCHEMA}","version":1,"payload":"AA==","updatedAt":0}}"#);
        assert_eq!(
            decode_resume(&encoded),
            Err(PersistenceError::InvalidPayloadLength(1))
        );
        assert_eq!(invalid_resume_key(42), "c1.browser-resume.v1.invalid.42");
    }

    #[test]
    fn newer_version_is_not_downgraded() {
        let encoded =
            format!(r#"{{"schema":"{CARD_SCHEMA}","version":2,"slots":[],"updatedAt":0}}"#);
        assert_eq!(
            decode_virtual_card(&encoded),
            Err(PersistenceError::UnsupportedVersion(2))
        );
    }

    #[test]
    fn resume_round_trips() {
        let record = ResumeRecord {
            payload: Box::new(payload(0x33)),
            updated_at: 123,
        };
        let encoded = encode_resume(&record).unwrap();
        assert_eq!(decode_resume(&encoded).unwrap(), record);
    }

    #[test]
    fn malformed_slot_does_not_hide_other_card_slots() {
        let encoded_payload = STANDARD.encode(payload(0x5a));
        let encoded = format!(
            r#"{{"schema":"{CARD_SCHEMA}","version":1,"slots":[{{"payload":"{encoded_payload}"}},{{"payload":9}},42,null]}}"#
        );
        let card = decode_virtual_card(&encoded).unwrap();
        assert_eq!(
            card.slots[0],
            CardSlot::Valid {
                payload: Box::new(payload(0x5a)),
                updated_at: 0,
            }
        );
        assert!(matches!(card.slots[1], CardSlot::Damaged { .. }));
        assert!(matches!(card.slots[2], CardSlot::Damaged { .. }));
        assert_eq!(card.slots[3], CardSlot::Empty);
    }

    #[test]
    fn malformed_timestamps_do_not_destroy_opaque_payloads() {
        let encoded_payload = STANDARD.encode(payload(0x6b));
        let card_json = format!(
            r#"{{"schema":"{CARD_SCHEMA}","version":1,"slots":[{{"payload":"{encoded_payload}","updatedAt":"unknown"}}],"updatedAt":{{}}}}"#
        );
        let card = decode_virtual_card(&card_json).unwrap();
        assert_eq!(card.updated_at, 0);
        assert_eq!(
            card.slots[0],
            CardSlot::Valid {
                payload: Box::new(payload(0x6b)),
                updated_at: 0,
            }
        );

        let resume_json = format!(
            r#"{{"schema":"{RESUME_SCHEMA}","version":1,"payload":"{encoded_payload}","updatedAt":false}}"#
        );
        assert_eq!(decode_resume(&resume_json).unwrap().updated_at, 0);
    }

    #[test]
    fn outdated_resume_is_quarantinable_not_treated_as_newer() {
        let encoded =
            format!(r#"{{"schema":"{RESUME_SCHEMA}","version":0,"payload":"","updatedAt":0}}"#);
        assert_eq!(
            decode_resume(&encoded),
            Err(PersistenceError::OutdatedVersion(0))
        );
    }

    #[test]
    fn missing_resume_payload_has_a_stable_error() {
        let encoded = format!(r#"{{"schema":"{RESUME_SCHEMA}","version":1}}"#);
        assert_eq!(
            decode_resume(&encoded),
            Err(PersistenceError::MissingPayload)
        );
    }

    proptest! {
        #[test]
        fn arbitrary_resume_payloads_round_trip(bytes in prop::collection::vec(any::<u8>(), PAYLOAD_BYTES), updated_at in any::<u64>()) {
            let record = ResumeRecord {
                payload: bytes.into_boxed_slice().try_into().unwrap(),
                updated_at,
            };
            let json = encode_resume(&record).unwrap();
            prop_assert_eq!(decode_resume(&json).unwrap(), record);
        }
    }
}
