//! Safe decoding of SLST raw and delta visibility lists.

use crate::binary::{FormatError, Reader, checked_slice};

const DELTA_HEADER_WORDS: usize = 2;
const NULL_INDEX: i64 = 0x00ff_ff00;

/// One packed world/polygon reference from a visibility list.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PolygonId {
    pub world_index: u8,
    pub polygon_index: u16,
    pub flag: bool,
}

impl PolygonId {
    pub const BYTE_LEN: usize = 2;

    #[must_use]
    pub const fn from_raw(raw: u16) -> Self {
        Self {
            world_index: ((raw >> 12) & 7) as u8,
            polygon_index: raw & 0x0fff,
            flag: raw & 0x8000 != 0,
        }
    }

    #[must_use]
    pub const fn raw(self) -> u16 {
        self.polygon_index | ((self.world_index as u16) << 12) | ((self.flag as u16) << 15)
    }

    #[must_use]
    const fn without_flag(self) -> Self {
        Self {
            flag: false,
            ..self
        }
    }
}

/// Direction used when applying a delta between adjacent path points.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SlstDirection {
    Forward,
    Backward,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SwapOperation {
    left: usize,
    offset: usize,
}

/// Parsed delta payload. Word indices remain relative to the serialized delta
/// header, exactly as in the source format.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlstDelta {
    pub split_index: u16,
    pub swaps_index: u16,
    pub words: Vec<u16>,
    swaps: Vec<SwapOperation>,
    removal_items: usize,
    addition_items: usize,
}

/// One SLST entry item, either a complete polygon list or an adjacent-point delta.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SlstItem {
    Raw {
        item_type: u16,
        polygons: Vec<PolygonId>,
    },
    Delta {
        item_type: u16,
        delta: SlstDelta,
    },
}

impl SlstItem {
    pub const HEADER_BYTE_LEN: usize = 4;

    /// Parses one item. The serialized `length` is a polygon count for raw
    /// items and a 16-bit word count for delta items.
    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        let header = checked_slice(bytes, 0, Self::HEADER_BYTE_LEN, "SLST item header")?;
        let mut reader = Reader::new(header);
        let length = usize::from(reader.u16_le()?);
        let item_type = reader.u16_le()?;
        let payload_len = length
            .checked_mul(2)
            .ok_or_else(|| FormatError::at(0, "SLST payload length overflows"))?;
        let payload = checked_slice(
            bytes,
            Self::HEADER_BYTE_LEN,
            payload_len,
            "SLST item payload",
        )?;

        if item_type & 1 == 0 {
            let mut polygons = Vec::with_capacity(length);
            let mut payload_reader = Reader::new(payload);
            for _ in 0..length {
                polygons.push(PolygonId::from_raw(payload_reader.u16_le()?));
            }
            return Ok(Self::Raw {
                item_type,
                polygons,
            });
        }

        if length < DELTA_HEADER_WORDS {
            return Err(FormatError::at(
                0,
                "SLST delta is shorter than its split/swap header",
            ));
        }
        let mut payload_reader = Reader::new(payload);
        let mut words = Vec::with_capacity(length);
        for _ in 0..length {
            words.push(payload_reader.u16_le()?);
        }
        let split_index = usize::from(words[0]);
        let swaps_index = usize::from(words[1]);
        if !(DELTA_HEADER_WORDS <= split_index
            && split_index <= swaps_index
            && swaps_index <= words.len())
        {
            return Err(FormatError::at(
                Self::HEADER_BYTE_LEN,
                "SLST delta indices do not partition removal, addition and swap data",
            ));
        }
        let removal_items = validate_edit_segment(&words, DELTA_HEADER_WORDS, split_index)?;
        let addition_items = validate_edit_segment(&words, split_index, swaps_index)?;
        let swaps = parse_swaps(&words, swaps_index)?;
        Ok(Self::Delta {
            item_type,
            delta: SlstDelta {
                split_index: words[0],
                swaps_index: words[1],
                words,
                swaps,
                removal_items,
                addition_items,
            },
        })
    }

    /// Produces the visible polygon list represented by this item.
    ///
    /// Raw items replace the source in either direction. Delta items mirror
    /// `SlstDecodeForward`/`SlstDecodeBackward` while rejecting every malformed
    /// index that the C code would otherwise access unchecked.
    pub fn apply(
        &self,
        source: &[PolygonId],
        direction: SlstDirection,
    ) -> Result<Vec<PolygonId>, FormatError> {
        if source.len() > usize::from(u16::MAX) {
            return Err(FormatError::global(
                "SLST source list exceeds the 16-bit retail length",
            ));
        }
        match self {
            Self::Raw { polygons, .. } => Ok(polygons.clone()),
            Self::Delta { delta, .. } => match direction {
                SlstDirection::Forward => delta.apply_forward(source),
                SlstDirection::Backward => delta.apply_backward(source),
            },
        }
    }
}

impl SlstDelta {
    fn apply_forward(&self, source: &[PolygonId]) -> Result<Vec<PolygonId>, FormatError> {
        let split = usize::from(self.split_index);
        let swaps = usize::from(self.swaps_index);
        let removal = EditCursor::new(&self.words, DELTA_HEADER_WORDS, split)?;
        let addition = EditCursor::new(&self.words, split, swaps)?;
        let mut output = merge_forward(
            source,
            removal,
            addition,
            self.removal_items,
            self.addition_items,
        )?;
        apply_swaps(&mut output, self.swaps.iter().copied())?;
        Ok(output)
    }

    fn apply_backward(&self, source: &[PolygonId]) -> Result<Vec<PolygonId>, FormatError> {
        let split = usize::from(self.split_index);
        let swaps = usize::from(self.swaps_index);
        let mut unswapped = source.to_vec();
        apply_swaps(&mut unswapped, self.swaps.iter().rev().copied())?;
        // Forward removals become backward additions and vice versa.
        let addition = EditCursor::new(&self.words, DELTA_HEADER_WORDS, split)?;
        let removal = EditCursor::new(&self.words, split, swaps)?;
        merge_backward(
            &unswapped,
            removal,
            addition,
            self.addition_items,
            self.removal_items,
        )
    }
}

fn validate_edit_segment(words: &[u16], start: usize, end: usize) -> Result<usize, FormatError> {
    let mut position = start;
    let mut item_count = 0_usize;
    while position < end {
        let node = words[position];
        position += 1;
        if node == u16::MAX {
            return Ok(item_count);
        }
        let count = usize::from((node >> 12) + 1);
        let payload_end = position
            .checked_add(count)
            .ok_or_else(|| FormatError::at(position * 2, "SLST edit run overflows"))?;
        if payload_end > end {
            return Err(FormatError::at(
                position * 2,
                "SLST edit run is truncated at its partition boundary",
            ));
        }
        item_count = item_count
            .checked_add(count)
            .ok_or_else(|| FormatError::at(position * 2, "SLST edit count overflows"))?;
        position = payload_end;
    }
    Ok(item_count)
}

fn parse_swaps(words: &[u16], start: usize) -> Result<Vec<SwapOperation>, FormatError> {
    let mut result = Vec::new();
    let mut position = start;
    let mut previous_absolute_index: Option<usize> = None;
    while position < words.len() {
        let node = words[position];
        if node == u16::MAX {
            break;
        }
        let (left, offset, consumed) = if node & 0x8000 != 0 {
            (
                usize::from(node & 0x07ff),
                usize::from((node >> 11) & 0x0f),
                1,
            )
        } else if node & 0xc000 == 0x4000 {
            let base = previous_absolute_index.ok_or_else(|| {
                FormatError::at(
                    position * 2,
                    "SLST format-C swap has no preceding absolute swap",
                )
            })?;
            let delta = usize::from((node >> 5) & 0x01ff);
            let left = base.checked_add(delta).ok_or_else(|| {
                FormatError::at(position * 2, "SLST format-C swap index overflows")
            })?;
            (left, usize::from(node & 0x1f) + 16, 1)
        } else {
            let offset_word = *words
                .get(position + 1)
                .ok_or_else(|| FormatError::at(position * 2, "SLST format-B swap is truncated"))?;
            if node & 0xf000 != 0 || offset_word & 0xf000 != 0 {
                return Err(FormatError::at(
                    position * 2,
                    "SLST format-B swap contains nonzero reserved high bits",
                ));
            }
            (
                usize::from(node & 0x0fff),
                usize::from(offset_word & 0x0fff),
                2,
            )
        };
        left.checked_add(offset)
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| FormatError::at(position * 2, "SLST swap target overflows"))?;
        result.push(SwapOperation { left, offset });
        previous_absolute_index = if node & 0xc000 == 0x4000 {
            None
        } else {
            Some(left)
        };
        position += consumed;
    }
    Ok(result)
}

#[derive(Clone, Debug)]
struct EditCursor<'a> {
    words: &'a [u16],
    position: usize,
    end: usize,
    index: i64,
    remaining: usize,
    polygon: PolygonId,
}

impl<'a> EditCursor<'a> {
    fn new(words: &'a [u16], start: usize, end: usize) -> Result<Self, FormatError> {
        let mut cursor = Self {
            words,
            position: start,
            end,
            index: NULL_INDEX,
            remaining: 0,
            polygon: PolygonId::from_raw(0),
        };
        cursor.load_run()?;
        Ok(cursor)
    }

    fn active(&self) -> bool {
        self.remaining != 0
    }

    fn consume(&mut self) -> Result<(), FormatError> {
        if self.remaining == 0 {
            return Err(FormatError::global(
                "SLST decoder attempted to consume an exhausted edit run",
            ));
        }
        self.remaining -= 1;
        if self.remaining == 0 {
            self.load_run()
        } else {
            self.polygon = self.read_word_as_polygon()?;
            self.index += i64::from(self.polygon.flag);
            Ok(())
        }
    }

    fn load_run(&mut self) -> Result<(), FormatError> {
        if self.position >= self.end {
            self.index = NULL_INDEX;
            self.remaining = 0;
            return Ok(());
        }
        let node = self.read_word()?;
        if node == u16::MAX {
            self.index = NULL_INDEX;
            self.remaining = 0;
            return Ok(());
        }
        self.index = i64::from(node & 0x0fff);
        self.remaining = usize::from((node >> 12) + 1);
        self.polygon = self.read_word_as_polygon()?;
        Ok(())
    }

    fn read_word_as_polygon(&mut self) -> Result<PolygonId, FormatError> {
        self.read_word().map(PolygonId::from_raw)
    }

    fn read_word(&mut self) -> Result<u16, FormatError> {
        if self.position >= self.end {
            return Err(FormatError::at(
                self.position * 2,
                "SLST edit cursor crossed its segment boundary",
            ));
        }
        let word = self.words[self.position];
        self.position += 1;
        Ok(word)
    }
}

fn merge_forward(
    source: &[PolygonId],
    mut removal: EditCursor<'_>,
    mut addition: EditCursor<'_>,
    removal_items: usize,
    addition_items: usize,
) -> Result<Vec<PolygonId>, FormatError> {
    let capacity = source
        .len()
        .checked_sub(removal_items)
        .and_then(|value| value.checked_add(addition_items))
        .ok_or_else(|| FormatError::global("SLST forward result length is invalid"))?;
    if capacity > usize::from(u16::MAX) {
        return Err(FormatError::global(
            "SLST forward result exceeds the 16-bit retail length",
        ));
    }
    let mut output = Vec::with_capacity(capacity);
    let mut source_index = 0_usize;
    let mut removed = 0_i64;
    while source_index < source.len() || removal.active() || addition.active() {
        let source_index_i64 = i64::try_from(source_index)
            .map_err(|_| FormatError::global("SLST source index does not fit i64"))?;
        if source_index < source.len() && removal.active() && removal.index - 1 == source_index_i64
        {
            source_index += 1;
            removed += 1;
            removal.consume()?;
        } else if addition.active() && addition.index + removed == source_index_i64 {
            output.push(addition.polygon.without_flag());
            addition.consume()?;
        } else if source_index < source.len() {
            let remaining_after_one = source.len() - (source_index + 1);
            let copy_count = (addition.index + removed - source_index_i64)
                .min(removal.index - (source_index_i64 + 1))
                .min(i64::try_from(remaining_after_one).expect("u16-sized list fits i64"));
            let copy_count = if copy_count == 0 { 1 } else { copy_count };
            if copy_count < 0 {
                return Err(FormatError::global(
                    "SLST forward edit indices move behind the source cursor",
                ));
            }
            let count = usize::try_from(copy_count)
                .map_err(|_| FormatError::global("SLST copy count does not fit usize"))?;
            let end = source_index
                .checked_add(count)
                .filter(|end| *end <= source.len())
                .ok_or_else(|| FormatError::global("SLST forward copy exceeds source list"))?;
            output.extend_from_slice(&source[source_index..end]);
            source_index = end;
        } else {
            return Err(FormatError::global(
                "SLST forward edits cannot be applied at their encoded indices",
            ));
        }
    }
    if output.len() != capacity {
        return Err(FormatError::global(
            "SLST forward result length disagrees with its edit runs",
        ));
    }
    Ok(output)
}

fn merge_backward(
    source: &[PolygonId],
    mut removal: EditCursor<'_>,
    mut addition: EditCursor<'_>,
    removal_items: usize,
    addition_items: usize,
) -> Result<Vec<PolygonId>, FormatError> {
    let capacity = source
        .len()
        .checked_sub(removal_items)
        .and_then(|value| value.checked_add(addition_items))
        .ok_or_else(|| FormatError::global("SLST backward result length is invalid"))?;
    if capacity > usize::from(u16::MAX) {
        return Err(FormatError::global(
            "SLST backward result exceeds the 16-bit retail length",
        ));
    }
    let mut output = Vec::with_capacity(capacity);
    let mut source_index = 0_usize;
    let mut removed = 0_i64;
    let mut added = 0_i64;
    while source_index < source.len() || removal.active() || addition.active() {
        let source_index_i64 = i64::try_from(source_index)
            .map_err(|_| FormatError::global("SLST source index does not fit i64"))?;
        if source_index < source.len()
            && removal.active()
            && removal.index + removed == source_index_i64
        {
            source_index += 1;
            removed += 1;
            removal.consume()?;
        } else if addition.active() && addition.index + removed - (added + 1) == source_index_i64 {
            output.push(addition.polygon.without_flag());
            added += 1;
            addition.consume()?;
        } else if source_index < source.len() {
            let remaining_after_one = source.len() - (source_index + 1);
            let copy_count = (addition.index + (removed - added) - (source_index_i64 + 1))
                .min(removal.index + removed - source_index_i64)
                .min(i64::try_from(remaining_after_one).expect("u16-sized list fits i64"));
            let count = usize::try_from(copy_count.max(1))
                .map_err(|_| FormatError::global("SLST copy count does not fit usize"))?;
            let end = source_index
                .checked_add(count)
                .filter(|end| *end <= source.len())
                .ok_or_else(|| FormatError::global("SLST backward copy exceeds source list"))?;
            output.extend_from_slice(&source[source_index..end]);
            source_index = end;
        } else {
            return Err(FormatError::global(
                "SLST backward edits cannot be applied at their encoded indices",
            ));
        }
    }
    if output.len() != capacity {
        return Err(FormatError::global(
            "SLST backward result length disagrees with its edit runs",
        ));
    }
    Ok(output)
}

fn apply_swaps(
    polygons: &mut [PolygonId],
    operations: impl IntoIterator<Item = SwapOperation>,
) -> Result<(), FormatError> {
    for operation in operations {
        let right = operation
            .left
            .checked_add(operation.offset)
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| FormatError::global("SLST swap target overflows"))?;
        if operation.left >= polygons.len() || right >= polygons.len() {
            return Err(FormatError::global(
                "SLST swap references a polygon outside the visible list",
            ));
        }
        polygons.swap(operation.left, right);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn item_bytes(item_type: u16, words: &[u16]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(4 + words.len() * 2);
        bytes.extend_from_slice(&(words.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&item_type.to_le_bytes());
        for word in words {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    fn polygon(world: u8, index: u16) -> PolygonId {
        PolygonId {
            world_index: world,
            polygon_index: index,
            flag: false,
        }
    }

    #[test]
    fn polygon_id_matches_reversed_c_bitfield_order() {
        let id = PolygonId {
            world_index: 5,
            polygon_index: 0xabc,
            flag: true,
        };
        assert_eq!(id.raw(), 0xdabc);
        assert_eq!(PolygonId::from_raw(0xdabc), id);
    }

    #[test]
    fn raw_items_replace_the_visibility_list() {
        let words = [polygon(1, 2).raw(), polygon(7, 0xfff).raw()];
        let item = SlstItem::parse(&item_bytes(2, &words)).unwrap();
        assert_eq!(
            item.apply(&[polygon(0, 9)], SlstDirection::Forward)
                .unwrap(),
            [polygon(1, 2), polygon(7, 0xfff)]
        );
        assert!(SlstItem::parse(&item_bytes(2, &words)[..7]).is_err());
    }

    #[test]
    fn delta_remove_add_round_trips_in_both_directions() {
        let a = polygon(0, 1);
        let b = polygon(0, 2);
        let c = polygon(0, 3);
        let d = polygon(1, 9);
        // nodes[0..2] are split/swap indices. The forward removal run drops
        // source index one (header index two); the addition run inserts D at
        // logical index one after accounting for that removal.
        let words = [5, 8, 2, b.raw(), u16::MAX, 1, d.raw(), u16::MAX];
        let item = SlstItem::parse(&item_bytes(1, &words)).unwrap();
        let forward = item.apply(&[a, b, c], SlstDirection::Forward).unwrap();
        assert_eq!(forward, [a, d, c]);
        assert_eq!(
            item.apply(&forward, SlstDirection::Backward).unwrap(),
            [a, b, c]
        );
    }

    #[test]
    fn delta_partitions_may_end_after_payload_without_sentinels() {
        let source = [polygon(0, 0x00a), polygon(0, 0x00b), polygon(0, 0x00c)];
        // Removal [2, 0x000b] and addition [1, 0x1005] both end exactly at
        // their partition boundary, matching retail SLST delta encoding.
        let words = [4, 6, 2, 0x000b, 1, 0x1005];
        let item = SlstItem::parse(&item_bytes(1, &words)).unwrap();
        let expected = [polygon(0, 0x00a), polygon(1, 0x005), polygon(0, 0x00c)];
        assert_eq!(
            item.apply(&source, SlstDirection::Forward).unwrap(),
            expected
        );
        assert_eq!(
            item.apply(&expected, SlstDirection::Backward).unwrap(),
            source
        );
    }

    #[test]
    fn empty_delta_partitions_preserve_the_source() {
        let source = [polygon(2, 7), polygon(3, 9)];
        let item = SlstItem::parse(&item_bytes(1, &[2, 2])).unwrap();
        assert_eq!(item.apply(&source, SlstDirection::Forward).unwrap(), source);
        assert_eq!(
            item.apply(&source, SlstDirection::Backward).unwrap(),
            source
        );
    }

    #[test]
    fn swap_formats_are_checked_and_reversible() {
        let source: Vec<_> = (0..19).map(|index| polygon(0, index)).collect();
        // Empty edit sections each contain their terminator. A format-B swap
        // exchanges 0/17, followed by format-C exchanging 1/18.
        let words = [3, 4, u16::MAX, u16::MAX, 0, 16, 0x4020, u16::MAX];
        let item = SlstItem::parse(&item_bytes(1, &words)).unwrap();
        let forward = item.apply(&source, SlstDirection::Forward).unwrap();
        assert_eq!(forward[0], source[17]);
        assert_eq!(forward[1], source[18]);
        assert_eq!(
            item.apply(&forward, SlstDirection::Backward).unwrap(),
            source
        );

        let malformed = [3, 4, u16::MAX, u16::MAX, 0x4000];
        assert!(SlstItem::parse(&item_bytes(1, &malformed)).is_err());

        let chained_format_c = [2, 2, 0x8000, 0x4000, 0x4000];
        assert!(SlstItem::parse(&item_bytes(1, &chained_format_c)).is_err());

        let reserved_format_b = [2, 2, 0x1000, 0];
        assert!(SlstItem::parse(&item_bytes(1, &reserved_format_b)).is_err());
    }

    proptest! {
        #[test]
        fn raw_round_trip_preserves_all_packed_ids(words in proptest::collection::vec(any::<u16>(), 0..256)) {
            let parsed = SlstItem::parse(&item_bytes(0, &words)).unwrap();
            let expected: Vec<_> = words.into_iter().map(PolygonId::from_raw).collect();
            prop_assert_eq!(parsed.apply(&[], SlstDirection::Forward).unwrap(), expected);
        }

        #[test]
        fn malformed_slst_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..1024)) {
            if let Ok(item) = SlstItem::parse(&bytes) {
                let _ = item.apply(&[], SlstDirection::Forward);
                let _ = item.apply(&[], SlstDirection::Backward);
            }
        }
    }
}
