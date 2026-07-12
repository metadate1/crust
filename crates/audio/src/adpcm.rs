//! Bounds-checked PSX SPU ADPCM decoding.

/// Encoded bytes per SPU ADPCM block.
pub const BLOCK_BYTES: usize = 16;
/// Decoded mono samples per SPU ADPCM block.
pub const SAMPLES_PER_BLOCK: usize = 28;

const FILTER_0: [i32; 16] = [0, 60, 115, 98, 122, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const FILTER_1: [i32; 16] = [0, 0, -52, -55, -60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// Fully decoded sample plus an optional loop point, expressed in samples.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedAdpcm {
    pub samples: Vec<i16>,
    pub loop_start: Option<usize>,
}

/// Return the maximum decoded sample count. Incomplete trailing blocks are ignored, matching the
/// retail stream reader.
#[must_use]
pub const fn decoded_sample_capacity(encoded_len: usize) -> Option<usize> {
    (encoded_len / BLOCK_BYTES).checked_mul(SAMPLES_PER_BLOCK)
}

/// Decode a sequence of 16-byte PSX SPU blocks.
///
/// Predictor history is updated with the saturated sample. Bit 2 marks a candidate loop start;
/// bit 0 ends the sample, and bit 1 makes that end repeat from the latest candidate.
#[must_use]
pub fn decode(encoded: &[u8]) -> DecodedAdpcm {
    let Some(capacity) = decoded_sample_capacity(encoded.len()) else {
        return DecodedAdpcm {
            samples: Vec::new(),
            loop_start: None,
        };
    };
    let mut samples = Vec::new();
    // A maliciously large slice can have a mathematically valid expansion that exceeds a
    // platform's maximum `Vec` capacity. Decoding has no fallible public API, so represent an
    // unallocatable payload as an empty sample instead of panicking on capacity overflow.
    if samples.try_reserve_exact(capacity).is_err() {
        return DecodedAdpcm {
            samples,
            loop_start: None,
        };
    }
    let mut previous = 0_i32;
    let mut previous_2 = 0_i32;
    let mut loop_candidate = None;
    let mut loop_start = None;

    for block in encoded.chunks_exact(BLOCK_BYTES) {
        let shift = usize::from(block[0] & 0x0f).min(15);
        let predictor = usize::from(block[0] >> 4);
        let flags = block[1];
        if flags & 4 != 0 {
            loop_candidate = Some(samples.len());
        }

        for packed in &block[2..] {
            for nibble in [packed & 0x0f, packed >> 4] {
                let signed = if nibble & 8 == 0 {
                    i32::from(nibble)
                } else {
                    i32::from(nibble) - 16
                };
                let filtered =
                    (previous * FILTER_0[predictor] + previous_2 * FILTER_1[predictor] + 32) >> 6;
                let decoded = ((signed * 4096) >> shift) + filtered;
                let saturated = decoded.clamp(i32::from(i16::MIN), i32::from(i16::MAX));
                previous_2 = previous;
                previous = saturated;
                let saturated = i16::try_from(saturated).unwrap_or(if saturated.is_negative() {
                    i16::MIN
                } else {
                    i16::MAX
                });
                samples.push(saturated);
            }
        }

        if flags & 1 != 0 {
            if flags & 2 != 0 {
                loop_start = loop_candidate;
            }
            break;
        }
    }

    DecodedAdpcm {
        samples,
        loop_start,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn block(header: u8, flags: u8, packed: u8) -> [u8; BLOCK_BYTES] {
        let mut block = [0; BLOCK_BYTES];
        block[0] = header;
        block[1] = flags;
        block[2..].fill(packed);
        block
    }

    #[test]
    fn decodes_signed_nibbles_and_clamps() {
        let mut encoded = block(0, 1, 0);
        encoded[2] = 0x87;
        let decoded = decode(&encoded);
        assert_eq!(decoded.samples.len(), 28);
        assert_eq!(&decoded.samples[..2], &[28_672, -32_768]);
        assert_eq!(decoded.loop_start, None);
    }

    #[test]
    fn predictor_uses_saturated_history() {
        let mut encoded = Vec::new();
        encoded.extend(block(0x40, 0, 0x77));
        encoded.extend(block(0x40, 1, 0x88));
        let decoded = decode(&encoded);
        assert_eq!(decoded.samples[0], 28_672);
        assert_eq!(decoded.samples[1], 32_767);
        assert_eq!(decoded.samples[27], 32_767);
        assert_eq!(decoded.samples[28], -1_025);
        assert_eq!(decoded.samples[29], -32_768);
        assert_eq!(decoded.samples[55], -32_768);
    }

    #[test]
    fn loops_only_on_repeating_end_marker() {
        let mut encoded = Vec::new();
        encoded.extend(block(0, 4, 0));
        encoded.extend(block(0, 0, 0));
        encoded.extend(block(0, 1, 0));
        encoded.extend(block(0, 7, 0));
        let one_shot = decode(&encoded);
        assert_eq!(one_shot.samples.len(), 84);
        assert_eq!(one_shot.loop_start, None);

        encoded[2 * BLOCK_BYTES + 1] = 3;
        let looped = decode(&encoded);
        assert_eq!(looped.loop_start, Some(0));
    }

    #[test]
    fn latest_loop_marker_wins() {
        let mut encoded = Vec::new();
        encoded.extend(block(0, 6, 0));
        encoded.extend(block(0, 6, 0));
        encoded.extend(block(0, 0, 0));
        encoded.extend(block(0, 3, 0));
        let decoded = decode(&encoded);
        assert_eq!(decoded.samples.len(), 112);
        assert_eq!(decoded.loop_start, Some(28));
    }

    #[test]
    fn ignores_incomplete_tail() {
        assert_eq!(decode(&[0; 15]).samples.len(), 0);
        assert_eq!(decode(&[0; 17]).samples.len(), 28);
    }

    proptest! {
        #[test]
        fn arbitrary_payloads_remain_bounded(encoded in proptest::collection::vec(any::<u8>(), 0..512)) {
            let decoded = decode(&encoded);
            let capacity = decoded_sample_capacity(encoded.len()).unwrap();
            prop_assert!(decoded.samples.len() <= capacity);
            prop_assert_eq!(decoded.samples.len() % SAMPLES_PER_BLOCK, 0);
            if let Some(loop_start) = decoded.loop_start {
                prop_assert!(loop_start < decoded.samples.len());
                prop_assert_eq!(loop_start % SAMPLES_PER_BLOCK, 0);
            }
        }
    }
}
