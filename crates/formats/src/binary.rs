//! Small endian-explicit primitives shared by the disc and stream readers.

use core::fmt;
use core::ops::Range;

/// Error returned when user-supplied bytes do not satisfy a format contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormatError {
    offset: Option<usize>,
    message: String,
}

impl FormatError {
    /// Creates an error associated with an absolute byte offset.
    #[must_use]
    pub fn at(offset: usize, message: impl Into<String>) -> Self {
        Self {
            offset: Some(offset),
            message: message.into(),
        }
    }

    /// Creates an error that applies to a whole input rather than one byte.
    #[must_use]
    pub fn global(message: impl Into<String>) -> Self {
        Self {
            offset: None,
            message: message.into(),
        }
    }

    /// Absolute input offset associated with this failure, when available.
    #[must_use]
    pub const fn offset(&self) -> Option<usize> {
        self.offset
    }

    /// Human-readable failure reason without the offset prefix.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for FormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(offset) = self.offset {
            write!(formatter, "at byte 0x{offset:x}: {}", self.message)
        } else {
            formatter.write_str(&self.message)
        }
    }
}

impl std::error::Error for FormatError {}

/// Bounds-checked cursor over a borrowed byte slice.
#[derive(Clone, Debug)]
pub struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    /// Creates a reader positioned at byte zero.
    #[must_use]
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    /// Creates a reader positioned at `position`.
    pub fn with_position(bytes: &'a [u8], position: usize) -> Result<Self, FormatError> {
        if position > bytes.len() {
            return Err(FormatError::at(position, "cursor is outside the input"));
        }
        Ok(Self { bytes, position })
    }

    /// Total input length.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether the input contains no bytes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Current absolute input position.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.position
    }

    /// Bytes remaining after the cursor.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }

    /// Moves the cursor to an absolute byte offset.
    pub fn seek(&mut self, position: usize) -> Result<(), FormatError> {
        if position > self.bytes.len() {
            return Err(FormatError::at(position, "cursor is outside the input"));
        }
        self.position = position;
        Ok(())
    }

    /// Returns exactly `length` bytes and advances the cursor.
    pub fn take(&mut self, length: usize) -> Result<&'a [u8], FormatError> {
        let start = self.position;
        let result = checked_slice(self.bytes, start, length, "field")?;
        self.position = start + length;
        Ok(result)
    }

    /// Reads one byte.
    pub fn u8(&mut self) -> Result<u8, FormatError> {
        Ok(self.take(1)?[0])
    }

    /// Reads a little-endian unsigned 16-bit integer.
    pub fn u16_le(&mut self) -> Result<u16, FormatError> {
        let bytes: [u8; 2] = self.take(2)?.try_into().expect("slice length was checked");
        Ok(u16::from_le_bytes(bytes))
    }

    /// Reads a big-endian unsigned 16-bit integer.
    pub fn u16_be(&mut self) -> Result<u16, FormatError> {
        let bytes: [u8; 2] = self.take(2)?.try_into().expect("slice length was checked");
        Ok(u16::from_be_bytes(bytes))
    }

    /// Reads a little-endian signed 16-bit integer.
    pub fn i16_le(&mut self) -> Result<i16, FormatError> {
        let bytes: [u8; 2] = self.take(2)?.try_into().expect("slice length was checked");
        Ok(i16::from_le_bytes(bytes))
    }

    /// Reads a little-endian unsigned 32-bit integer.
    pub fn u32_le(&mut self) -> Result<u32, FormatError> {
        let bytes: [u8; 4] = self.take(4)?.try_into().expect("slice length was checked");
        Ok(u32::from_le_bytes(bytes))
    }

    /// Reads a big-endian unsigned 32-bit integer.
    pub fn u32_be(&mut self) -> Result<u32, FormatError> {
        let bytes: [u8; 4] = self.take(4)?.try_into().expect("slice length was checked");
        Ok(u32::from_be_bytes(bytes))
    }

    /// Reads a little-endian signed 32-bit integer.
    pub fn i32_le(&mut self) -> Result<i32, FormatError> {
        let bytes: [u8; 4] = self.take(4)?.try_into().expect("slice length was checked");
        Ok(i32::from_le_bytes(bytes))
    }

    /// Reads an ISO9660 16-bit value stored in both byte orders.
    pub fn both_endian_u16(&mut self, field: &'static str) -> Result<u16, FormatError> {
        let offset = self.position;
        let little = self.u16_le()?;
        let big = self.u16_be()?;
        if little != big {
            return Err(FormatError::at(
                offset,
                format!("{field} has inconsistent little- and big-endian values"),
            ));
        }
        Ok(little)
    }

    /// Reads an ISO9660 32-bit value stored in both byte orders.
    pub fn both_endian_u32(&mut self, field: &'static str) -> Result<u32, FormatError> {
        let offset = self.position;
        let little = self.u32_le()?;
        let big = self.u32_be()?;
        if little != big {
            return Err(FormatError::at(
                offset,
                format!("{field} has inconsistent little- and big-endian values"),
            ));
        }
        Ok(little)
    }
}

/// Returns a checked sub-slice and reports arithmetic overflow as malformed input.
pub fn checked_slice<'a>(
    bytes: &'a [u8],
    offset: usize,
    length: usize,
    context: &'static str,
) -> Result<&'a [u8], FormatError> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| FormatError::at(offset, format!("{context} range overflows")))?;
    bytes
        .get(offset..end)
        .ok_or_else(|| FormatError::at(offset, format!("{context} is truncated")))
}

/// A validated 32-bit file-relative byte offset.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Offset(u32);

impl Offset {
    /// Creates an offset without interpreting it as a host pointer.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Original 32-bit representation.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Host-sized representation. This is lossless on all supported Rust hosts.
    #[must_use]
    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }

    /// Checks that `length` bytes beginning here fit in `container_len`.
    pub fn checked_range(
        self,
        length: usize,
        container_len: usize,
    ) -> Result<Range<usize>, FormatError> {
        let start = self.as_usize();
        let end = start
            .checked_add(length)
            .ok_or_else(|| FormatError::at(start, "offset range overflows"))?;
        if end > container_len {
            return Err(FormatError::at(
                start,
                "offset range is outside its container",
            ));
        }
        Ok(start..end)
    }
}

/// Index encoded by an odd C1 page id (`index << 1 | 1`).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PageIndex(u32);

impl PageIndex {
    /// Creates an untagged page index.
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// Untagged page index.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Encodes the retail odd page-id tag.
    #[must_use]
    pub const fn tagged(self) -> u32 {
        (self.0 << 1) | 1
    }
}

/// A page reference before or after relocation.
///
/// Even values are retained as file offsets rather than being cast to pointers.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PageRef {
    /// Odd tagged page index.
    Page(PageIndex),
    /// Even serialized or relocated offset.
    Offset(Offset),
}

impl PageRef {
    /// Decodes the exact 32-bit tagged representation.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        if raw & 1 == 1 {
            Self::Page(PageIndex::new(raw >> 1))
        } else {
            Self::Offset(Offset::new(raw))
        }
    }

    /// Re-encodes the exact 32-bit representation.
    #[must_use]
    pub const fn raw(self) -> u32 {
        match self {
            Self::Page(index) => index.tagged(),
            Self::Offset(offset) => offset.raw(),
        }
    }
}

/// Five-character C1 entry identifier packed into a tagged 32-bit value.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Eid(u32);

impl Eid {
    /// Retail null/none sentinel.
    pub const NONE: Self = Self(0x6396_347f);

    /// Retains a raw identifier, including non-name sentinels.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Exact 32-bit representation.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Whether this value carries the low-bit name tag.
    #[must_use]
    pub const fn is_named(self) -> bool {
        self.0 & 1 == 1
    }

    /// Ten-bit page-table hash stored in bits 15 through 24.
    #[must_use]
    pub const fn table_hash(self) -> u16 {
        ((self.0 >> 15) & 0x03ff) as u16
    }

    /// Encodes exactly five characters from the retail 64-character alphabet.
    pub fn from_name(name: &str) -> Result<Self, FormatError> {
        let bytes = name.as_bytes();
        if bytes.len() != 5 {
            return Err(FormatError::global(
                "an EID name must contain exactly five ASCII bytes",
            ));
        }
        let mut value = 0_u32;
        for (index, byte) in bytes.iter().copied().enumerate() {
            let digit = EID_ALPHABET
                .iter()
                .position(|candidate| *candidate == byte)
                .ok_or_else(|| FormatError::at(index, "character is not in the C1 EID alphabet"))?;
            value = (value << 6) | u32::try_from(digit).expect("alphabet has 64 entries");
        }
        Ok(Self((value << 1) | 1))
    }

    /// Decodes the name tag, or returns `None` for an untagged value.
    #[must_use]
    pub fn name_bytes(self) -> Option<[u8; 5]> {
        if !self.is_named() {
            return None;
        }
        let mut value = self.0 >> 1;
        let mut name = [0_u8; 5];
        for byte in name.iter_mut().rev() {
            *byte = EID_ALPHABET[(value & 0x3f) as usize];
            value >>= 6;
        }
        Some(name)
    }

    /// Decodes a named EID into an owned ASCII string.
    #[must_use]
    pub fn name(self) -> Option<String> {
        self.name_bytes()
            .map(|bytes| String::from_utf8(bytes.to_vec()).expect("EID alphabet is ASCII"))
    }
}

impl fmt::Display for Eid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(name) = self.name() {
            formatter.write_str(&name)
        } else {
            write!(formatter, "[{:X}]", self.0)
        }
    }
}

/// A serialized entry reference with its retail low-bit tags decoded.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EntryRef {
    /// Low bit set: five-character EID or an odd EID sentinel.
    Eid(Eid),
    /// Bit one set while bit zero is clear: immediate/tagged value.
    Value(u32),
    /// Both low bits clear: explicit offset, never a host pointer.
    Offset(Offset),
}

impl EntryRef {
    /// Decodes an exact 32-bit entry reference.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        if raw & 1 == 1 {
            Self::Eid(Eid::from_raw(raw))
        } else if raw & 2 == 2 {
            Self::Value(raw)
        } else {
            Self::Offset(Offset::new(raw))
        }
    }

    /// Re-encodes the exact 32-bit entry reference.
    #[must_use]
    pub const fn raw(self) -> u32 {
        match self {
            Self::Eid(eid) => eid.raw(),
            Self::Value(value) => value,
            Self::Offset(offset) => offset.raw(),
        }
    }

    /// Hash bits used to select one of the 256 NSD buckets.
    #[must_use]
    pub const fn table_hash(self) -> u16 {
        ((self.raw() >> 15) & 0x03ff) as u16
    }
}

/// Stable logical handle replacing a relocated C entry pointer.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EntryHandle {
    page: PageIndex,
    entry: u16,
}

impl EntryHandle {
    /// Creates a logical `(page, entry)` handle.
    #[must_use]
    pub const fn new(page: PageIndex, entry: u16) -> Self {
        Self { page, entry }
    }

    /// Containing NSF page index.
    #[must_use]
    pub const fn page(self) -> PageIndex {
        self.page
    }

    /// Entry index within its page.
    #[must_use]
    pub const fn entry(self) -> u16 {
        self.entry
    }
}

/// Character map used by the original EID packer.
pub const EID_ALPHABET: [u8; 64] =
    *b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_!";

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn reader_is_endian_explicit() {
        let mut reader = Reader::new(&[0x34, 0x12, 0x12, 0x34, 0x78, 0x56, 0x34, 0x12]);
        assert_eq!(reader.u16_le().unwrap(), 0x1234);
        assert_eq!(reader.u16_be().unwrap(), 0x1234);
        assert_eq!(reader.u32_le().unwrap(), 0x1234_5678);
        assert_eq!(reader.remaining(), 0);
        assert!(reader.u8().is_err());
    }

    #[test]
    fn both_endian_values_must_agree() {
        let mut good = Reader::new(&[0x34, 0x12, 0x12, 0x34]);
        assert_eq!(good.both_endian_u16("size").unwrap(), 0x1234);

        let mut bad = Reader::new(&[0x34, 0x12, 0x12, 0x35]);
        assert!(bad.both_endian_u16("size").is_err());
    }

    #[test]
    fn eid_matches_the_retail_packer() {
        let eid = Eid::from_name("0c_pZ").unwrap();
        assert_eq!(eid.name().as_deref(), Some("0c_pZ"));
        assert_eq!(Eid::from_raw(eid.raw()).to_string(), "0c_pZ");
        assert_eq!(Eid::from_raw(2).to_string(), "[2]");
        assert!(Eid::from_name("shorter").is_err());
        assert!(Eid::from_name("bad-!").is_err());
    }

    #[test]
    fn tagged_reference_variants_are_unambiguous() {
        assert_eq!(PageRef::from_raw(7), PageRef::Page(PageIndex::new(3)));
        assert_eq!(
            PageRef::from_raw(0x100),
            PageRef::Offset(Offset::new(0x100))
        );
        assert!(matches!(EntryRef::from_raw(0x101), EntryRef::Eid(_)));
        assert_eq!(EntryRef::from_raw(0x102), EntryRef::Value(0x102));
        assert_eq!(
            EntryRef::from_raw(0x100),
            EntryRef::Offset(Offset::new(0x100))
        );
    }

    proptest! {
        #[test]
        fn page_refs_round_trip_every_raw_value(raw in any::<u32>()) {
            prop_assert_eq!(PageRef::from_raw(raw).raw(), raw);
        }

        #[test]
        fn entry_refs_round_trip_every_raw_value(raw in any::<u32>()) {
            prop_assert_eq!(EntryRef::from_raw(raw).raw(), raw);
        }

        #[test]
        fn checked_slices_never_panic(
            bytes in proptest::collection::vec(any::<u8>(), 0..512),
            offset in any::<usize>(),
            length in any::<usize>(),
        ) {
            let result = checked_slice(&bytes, offset, length, "property input");
            if let Ok(slice) = result {
                prop_assert_eq!(slice.len(), length);
                prop_assert!(offset <= bytes.len());
                prop_assert!(length <= bytes.len() - offset);
            }
        }
    }
}
