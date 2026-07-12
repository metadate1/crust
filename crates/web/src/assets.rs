//! Local-only stream registry and validated pair materialization.
// WebIDL exposes Blob lengths as integral `f64` values. Every intentional cast
// below follows a finite/integer/2^53 bound check.
#![allow(dead_code)] // Public loader wiring is added by the web application slice.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::missing_errors_doc
)]

use std::collections::BTreeMap;
use std::str::FromStr as _;

use crust_formats::stream::{
    KNOWN_LEVELS, LevelId, Nsd, Nsf, StreamKind, StreamName, parse_nsd, parse_nsf,
};
use js_sys::{ArrayBuffer, Uint8Array};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, File, FileList};

use crust_formats::disc::{DiscStreamSet, SectorLayout};

use crate::disc_import::read_disc_extent;

const MAX_SAFE_JS_BYTES: u64 = 9_007_199_254_740_991;
// Retail's largest stream is under 10 MiB. Reading a Blob briefly holds both
// its JS ArrayBuffer and the Rust copy, so keep a conservative upper bound.
const MAX_STREAM_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub enum MountedSource {
    Blob(Blob),
    Disc {
        file: File,
        layout: SectorLayout,
        extent_lba: u32,
        byte_len: u32,
    },
}

#[derive(Clone, Debug)]
pub struct MountedBlob {
    pub source: MountedSource,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default)]
pub struct AssetStore {
    streams: BTreeMap<StreamName, MountedBlob>,
}

#[derive(Debug)]
pub struct ValidatedPair {
    pub level: LevelId,
    pub nsd_bytes: Vec<u8>,
    pub nsf_bytes: Vec<u8>,
    pub nsd: Nsd,
    pub nsf: Nsf,
}

impl AssetStore {
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.streams.len()
    }

    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.streams.values().map(|stream| stream.bytes).sum()
    }

    #[must_use]
    pub fn pair_count(&self) -> usize {
        KNOWN_LEVELS
            .iter()
            .filter(|level| self.has_pair(level.id))
            .count()
    }

    #[must_use]
    pub fn playable_levels(&self) -> Vec<(LevelId, &'static str)> {
        KNOWN_LEVELS
            .iter()
            .filter(|level| level.bootable && self.has_pair(level.id))
            .map(|level| (level.id, level.name))
            .collect()
    }

    #[must_use]
    pub fn has_pair(&self, level: LevelId) -> bool {
        self.streams
            .contains_key(&StreamName::new(level, StreamKind::Nsd))
            && self
                .streams
                .contains_key(&StreamName::new(level, StreamKind::Nsf))
    }

    pub fn clear(&mut self) {
        self.streams.clear();
    }

    pub fn insert_blob(&mut self, name: StreamName, blob: Blob) -> Result<(), JsValue> {
        if name.level().known().is_none() {
            return Ok(());
        }
        let bytes = checked_blob_length(&blob, &name.filename())?;
        self.streams.insert(
            name,
            MountedBlob {
                source: MountedSource::Blob(blob),
                bytes,
            },
        );
        Ok(())
    }

    pub fn insert_disc_streams(
        &mut self,
        file: &File,
        layout: SectorLayout,
        streams: &DiscStreamSet,
    ) -> Result<usize, JsValue> {
        streams
            .validate_complete_retail()
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        let mut mounted = BTreeMap::new();
        for stream in streams.files() {
            if u64::from(stream.byte_len) > MAX_STREAM_BYTES {
                return Err(JsValue::from_str(&format!(
                    "{} is too large for the browser runtime",
                    stream.name
                )));
            }
            mounted.insert(
                stream.name,
                MountedBlob {
                    source: MountedSource::Disc {
                        file: file.clone(),
                        layout,
                        extent_lba: stream.extent_lba,
                        byte_len: stream.byte_len,
                    },
                    bytes: u64::from(stream.byte_len),
                },
            );
        }
        let count = mounted.len();
        self.streams.extend(mounted);
        Ok(count)
    }

    pub fn insert_file(&mut self, file: File) -> Result<bool, JsValue> {
        let Ok(name) = StreamName::from_str(&file.name()) else {
            return Ok(false);
        };
        if name.level().known().is_none() {
            return Ok(false);
        }
        let blob: Blob = file.unchecked_into();
        self.insert_blob(name, blob)?;
        Ok(true)
    }

    pub fn insert_file_list(&mut self, files: &FileList) -> Result<usize, JsValue> {
        let mut accepted = 0;
        for index in 0..files.length() {
            if let Some(file) = files.get(index)
                && self.insert_file(file)?
            {
                accepted += 1;
            }
        }
        Ok(accepted)
    }

    #[must_use]
    pub fn blob(&self, name: StreamName) -> Option<&MountedBlob> {
        self.streams.get(&name)
    }

    pub async fn validate_pair(&self, level: LevelId) -> Result<ValidatedPair, JsValue> {
        let known = level
            .known()
            .ok_or_else(|| JsValue::from_str(&format!("unrecognized level {level}")))?;
        if !known.bootable {
            return Err(JsValue::from_str(
                "the Cave pair is an index-only archive and cannot be booted",
            ));
        }
        let nsd_name = StreamName::new(level, StreamKind::Nsd);
        let nsf_name = StreamName::new(level, StreamKind::Nsf);
        let nsd_blob = self
            .blob(nsd_name)
            .ok_or_else(|| JsValue::from_str(&format!("missing {nsd_name}")))?;
        let nsf_blob = self
            .blob(nsf_name)
            .ok_or_else(|| JsValue::from_str(&format!("missing {nsf_name}")))?;
        let nsd_bytes = read_mounted(nsd_blob, nsd_name).await?;
        let nsd = parse_nsd(&nsd_bytes, level)
            .map_err(|error| JsValue::from_str(&format!("{nsd_name}: {error}")))?;
        let declared_nsf_len = usize::try_from(nsf_blob.bytes)
            .map_err(|_| JsValue::from_str(&format!("{nsf_name} is too large for this browser")))?;
        nsd.validate_nsf_len(declared_nsf_len)
            .map_err(|error| JsValue::from_str(&format!("{nsf_name}: {error}")))?;
        let nsf_bytes = read_mounted(nsf_blob, nsf_name).await?;
        let nsf = parse_nsf(&nsf_bytes, &nsd)
            .map_err(|error| JsValue::from_str(&format!("{nsf_name}: {error}")))?;
        Ok(ValidatedPair {
            level,
            nsd_bytes,
            nsf_bytes,
            nsd,
            nsf,
        })
    }
}

async fn read_mounted(mounted: &MountedBlob, name: StreamName) -> Result<Vec<u8>, JsValue> {
    let bytes = read_source(&mounted.source).await?;
    let actual = u64::try_from(bytes.len())
        .map_err(|_| JsValue::from_str(&format!("{name} length exceeds u64")))?;
    if actual != mounted.bytes {
        return Err(JsValue::from_str(&format!(
            "{name} read {actual} bytes; expected {}",
            mounted.bytes
        )));
    }
    Ok(bytes)
}

pub async fn read_source(source: &MountedSource) -> Result<Vec<u8>, JsValue> {
    match source {
        MountedSource::Blob(blob) => read_blob(blob).await,
        MountedSource::Disc {
            file,
            layout,
            extent_lba,
            byte_len,
        } => read_disc_extent(file, *layout, *extent_lba, *byte_len).await,
    }
}

pub async fn read_blob(blob: &Blob) -> Result<Vec<u8>, JsValue> {
    let expected = checked_blob_length(blob, "selected stream")?;
    let array_buffer = JsFuture::from(blob.array_buffer()).await?;
    let buffer: ArrayBuffer = array_buffer.dyn_into()?;
    let bytes = Uint8Array::new(&buffer);
    let mut output = vec![0; bytes.length() as usize];
    bytes.copy_to(&mut output);
    if output.len() as u64 != expected {
        return Err(JsValue::from_str(
            "browser Blob read returned an unexpected byte length",
        ));
    }
    Ok(output)
}

fn checked_blob_length(blob: &Blob, context: &str) -> Result<u64, JsValue> {
    let size = blob.size();
    if !size.is_finite() || size <= 0.0 || size.fract() != 0.0 || size > MAX_SAFE_JS_BYTES as f64 {
        return Err(JsValue::from_str(&format!(
            "{context} has an invalid byte length"
        )));
    }
    let length = size as u64;
    if length > MAX_STREAM_BYTES {
        return Err(JsValue::from_str(&format!(
            "{context} is {length} bytes; browser limit is {MAX_STREAM_BYTES}"
        )));
    }
    Ok(length)
}
