//! Opt-in characterization of a legally local NTSC-U disc image.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr as _;

use crust_formats::disc::{DiscImage, SectorLayout};
use crust_formats::stream::StreamName;

/// Reads only directory metadata for canonical NSD/NSF filenames.
fn stream_metadata(root: &Path) -> BTreeMap<StreamName, u64> {
    let entries = std::fs::read_dir(root)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", root.display()));
    let mut files = BTreeMap::new();
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| {
            panic!("could not read an entry in {}: {error}", root.display())
        });
        let metadata = entry.metadata().unwrap_or_else(|error| {
            panic!(
                "could not read metadata for {}: {error}",
                entry.path().display()
            )
        });
        if !metadata.is_file() {
            continue;
        }
        let Some(filename) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(name) = StreamName::from_str(&filename) else {
            continue;
        };
        assert!(
            files.insert(name, metadata.len()).is_none(),
            "duplicate local stream filename {name}"
        );
    }
    files
}

/// Characterizes the user's local retail BIN without extracting or copying any game data.
#[test]
#[ignore = "set C1_DISC_IMAGE to a legally local NTSC-U raw BIN"]
fn opens_local_raw_disc_and_discovers_exact_retail_stream_set() {
    let disc_path = PathBuf::from(
        std::env::var_os("C1_DISC_IMAGE")
            .expect("C1_DISC_IMAGE must name a legally local NTSC-U raw BIN"),
    );
    let disc_bytes = std::fs::read(&disc_path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", disc_path.display()));
    let image = DiscImage::open(&disc_bytes)
        .unwrap_or_else(|error| panic!("{}: {error}", disc_path.display()));
    assert_eq!(image.layout(), SectorLayout::RawMode2_2352);

    let streams = image
        .discover_streams()
        .unwrap_or_else(|error| panic!("{}: {error}", disc_path.display()));
    assert_eq!(streams.files().len(), 88);
    assert_eq!(streams.complete_pair_count(), 44);
    streams
        .validate_complete_retail()
        .unwrap_or_else(|error| panic!("{}: {error}", disc_path.display()));

    if let Some(stream_root) = std::env::var_os("C1_STREAM_DIR") {
        let stream_root = PathBuf::from(stream_root);
        let discovered = streams
            .files()
            .iter()
            .map(|stream| (stream.name, u64::from(stream.byte_len)))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(stream_metadata(&stream_root), discovered);
    }
}
