use core::fmt;
use core::str::FromStr;

use crate::binary::FormatError;

/// Numeric level identifier embedded in stream filenames and LDAT metadata.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LevelId(u32);

impl LevelId {
    /// Cave's index-only archive.
    pub const CAVE: Self = Self(0x04);
    /// Title screens and island map.
    pub const TITLE: Self = Self(0x19);
    /// N. Sanity Beach.
    pub const N_SANITY_BEACH: Self = Self(0x09);
    /// Level-complete tally.
    pub const LEVEL_COMPLETE: Self = Self(0x2d);
    /// Cortex laboratory Intro attract sequence.
    pub const INTRO: Self = Self(0x38);
    /// Ending sequence.
    pub const ENDING: Self = Self(0x39);

    /// Creates an identifier. The on-disc filename field has 28 value bits.
    pub fn new(value: u32) -> Result<Self, FormatError> {
        if value > 0x0fff_ffff {
            return Err(FormatError::global(
                "level id does not fit the seven-hex-digit filename field",
            ));
        }
        Ok(Self(value))
    }

    /// Creates an identifier in a constant context.
    #[must_use]
    pub const fn new_const(value: u32) -> Self {
        assert!(value <= 0x0fff_ffff);
        Self(value)
    }

    /// Numeric identifier.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Retail `S0` through `S3` directory for known C1 levels.
    #[must_use]
    pub const fn stream_directory_index(self) -> Option<u8> {
        let index = self.0 >> 4;
        if index <= 3 { Some(index as u8) } else { None }
    }

    /// Catalog metadata when this is one of the 44 recognized pairs.
    #[must_use]
    pub fn known(self) -> Option<&'static KnownLevel> {
        known_level(self)
    }
}

impl fmt::Display for LevelId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "0x{:02X}", self.0)
    }
}

/// A stream's file extension.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum StreamKind {
    /// Level metadata and page table.
    Nsd,
    /// Page payload stream.
    Nsf,
}

impl StreamKind {
    /// Canonical lowercase extension without a leading dot.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Nsd => "nsd",
            Self::Nsf => "nsf",
        }
    }
}

/// Parsed canonical `s1234567.nsd` or `.nsf` filename.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StreamName {
    level: LevelId,
    kind: StreamKind,
}

impl StreamName {
    /// Constructs a stream filename descriptor.
    #[must_use]
    pub const fn new(level: LevelId, kind: StreamKind) -> Self {
        Self { level, kind }
    }

    /// Level id encoded in the seven hexadecimal digits.
    #[must_use]
    pub const fn level(self) -> LevelId {
        self.level
    }

    /// Metadata or page-data extension.
    #[must_use]
    pub const fn kind(self) -> StreamKind {
        self.kind
    }

    /// Canonical lowercase filename used by the browser mount.
    #[must_use]
    pub fn filename(self) -> String {
        format!("s{:07x}.{}", self.level.get(), self.kind.extension())
    }
}

impl fmt::Display for StreamName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.filename())
    }
}

impl FromStr for StreamName {
    type Err = FormatError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let bytes = value.as_bytes();
        if bytes.len() != 12 || !matches!(bytes[0], b's' | b'S') || bytes[8] != b'.' {
            return Err(FormatError::global(
                "stream name must match s0000000.nsd or s0000000.nsf",
            ));
        }
        let digits = value
            .get(1..8)
            .ok_or_else(|| FormatError::global("stream name is not ASCII"))?;
        if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(FormatError::at(
                1,
                "stream level id contains a non-hexadecimal digit",
            ));
        }
        let level = u32::from_str_radix(digits, 16)
            .map_err(|_| FormatError::at(1, "stream level id is invalid"))?;
        let extension = value
            .get(9..12)
            .ok_or_else(|| FormatError::global("stream extension is not ASCII"))?;
        let kind = if extension.eq_ignore_ascii_case("nsd") {
            StreamKind::Nsd
        } else if extension.eq_ignore_ascii_case("nsf") {
            StreamKind::Nsf
        } else {
            return Err(FormatError::at(9, "stream extension must be NSD or NSF"));
        };
        Ok(Self::new(LevelId::new(level)?, kind))
    }
}

/// User-facing metadata for one recognized retail stream pair.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct KnownLevel {
    /// Numeric stream id.
    pub id: LevelId,
    /// Retail/user-facing name.
    pub name: &'static str,
    /// Whether the pair has LDAT metadata and may be selected as a boot target.
    pub bootable: bool,
}

impl KnownLevel {
    /// Canonical lowercase metadata filename.
    #[must_use]
    pub fn nsd_filename(self) -> String {
        StreamName::new(self.id, StreamKind::Nsd).filename()
    }

    /// Canonical lowercase page-data filename.
    #[must_use]
    pub fn nsf_filename(self) -> String {
        StreamName::new(self.id, StreamKind::Nsf).filename()
    }
}

macro_rules! level {
    ($id:literal, $name:literal) => {
        KnownLevel {
            id: LevelId::new_const($id),
            name: $name,
            bootable: $id != 0x04,
        }
    };
}

/// Exact 44-pair catalog from the NTSC-U retail disc.
pub const KNOWN_LEVELS: [KnownLevel; 44] = [
    level!(0x03, "Cortex Power"),
    level!(0x04, "Cave"),
    level!(0x05, "Generator Room"),
    level!(0x06, "Heavy Machinery"),
    level!(0x07, "Toxic Waste"),
    level!(0x08, "Pinstripe"),
    level!(0x09, "N. Sanity Beach"),
    level!(0x0a, "Papu Papu"),
    level!(0x0c, "Jungle Rollers"),
    level!(0x0e, "Boulders"),
    level!(0x0f, "Upstream"),
    level!(0x11, "Hog Wild"),
    level!(0x12, "The Great Gate"),
    level!(0x13, "Boulder Dash"),
    level!(0x14, "Road to Nowhere"),
    level!(0x15, "Rolling Stones"),
    level!(0x16, "The High Road"),
    level!(0x17, "Ripper Roo"),
    level!(0x18, "Up the Creek"),
    level!(0x19, "Title / Island Map"),
    level!(0x1a, "Native Fortress"),
    level!(0x1b, "Dr. N. Brio"),
    level!(0x1c, "Temple Ruins"),
    level!(0x1d, "Jaws of Darkness"),
    level!(0x1e, "Whole Hog"),
    level!(0x1f, "Dr. Neo Cortex"),
    level!(0x20, "The Lost City"),
    level!(0x21, "Koala Kong"),
    level!(0x22, "Stormy Ascent"),
    level!(0x23, "Sunset Vista"),
    level!(0x24, "Tawna Bonus 1"),
    level!(0x25, "Brio Bonus"),
    level!(0x26, "Bonus"),
    level!(0x28, "Lights Out"),
    level!(0x29, "The Lab"),
    level!(0x2a, "Fumbling in the Dark"),
    level!(0x2c, "The Great Hall"),
    level!(0x2d, "Level Complete"),
    level!(0x2e, "Slippery Climb"),
    level!(0x33, "Tawna Bonus 2"),
    level!(0x34, "Cortex Bonus"),
    level!(0x37, "Castle Machinery"),
    level!(0x38, "Intro"),
    level!(0x39, "Ending"),
];

/// Looks up one of the exact recognized NTSC-U pairs.
#[must_use]
pub fn known_level(id: LevelId) -> Option<&'static KnownLevel> {
    KNOWN_LEVELS
        .binary_search_by_key(&id, |level| level.id)
        .ok()
        .map(|index| &KNOWN_LEVELS[index])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn catalog_is_exact_unique_and_sorted() {
        assert_eq!(KNOWN_LEVELS.len(), 44);
        assert_eq!(
            KNOWN_LEVELS.iter().filter(|level| level.bootable).count(),
            43
        );
        assert!(!known_level(LevelId::CAVE).unwrap().bootable);
        assert_eq!(
            known_level(LevelId::TITLE).unwrap().name,
            "Title / Island Map"
        );

        let ids: Vec<_> = KNOWN_LEVELS.iter().map(|level| level.id).collect();
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(ids.iter().copied().collect::<HashSet<_>>().len(), 44);
    }

    #[test]
    fn canonical_filenames_round_trip() {
        let mut names = HashSet::new();
        for level in KNOWN_LEVELS {
            for kind in [StreamKind::Nsd, StreamKind::Nsf] {
                let stream = StreamName::new(level.id, kind);
                let filename = stream.filename();
                assert_eq!(filename.parse::<StreamName>().unwrap(), stream);
                assert!(names.insert(filename));
            }
            assert_eq!(
                level.id.stream_directory_index(),
                Some((level.id.get() >> 4) as u8)
            );
        }
        assert_eq!(names.len(), 88);
        assert_eq!(
            StreamName::new(LevelId::INTRO, StreamKind::Nsf).filename(),
            "s0000038.nsf"
        );
    }

    #[test]
    fn filename_parser_is_strict_but_case_insensitive() {
        let parsed: StreamName = "S0000019.NSD".parse().unwrap();
        assert_eq!(parsed.level(), LevelId::TITLE);
        assert_eq!(parsed.kind(), StreamKind::Nsd);
        for malformed in [
            "s000019.nsd",
            "x0000019.nsd",
            "s00000zz.nsd",
            "s0000019.bin",
            "s0000019.nsd;1",
        ] {
            assert!(
                malformed.parse::<StreamName>().is_err(),
                "accepted {malformed}"
            );
        }
    }
}
