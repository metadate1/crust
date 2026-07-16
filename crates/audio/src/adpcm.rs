//! Bounds-checked PSX SPU ADPCM decoding.

use std::sync::Arc;

/// Encoded bytes per SPU ADPCM block.
pub const BLOCK_BYTES: usize = 16;
/// Decoded mono samples per SPU ADPCM block.
pub const SAMPLES_PER_BLOCK: usize = 28;

const FILTER_0: [i32; 16] = [0, 60, 115, 98, 122, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const FILTER_1: [i32; 16] = [0, 0, -52, -55, -60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AdpcmBlock([u8; BLOCK_BYTES]);

impl AdpcmBlock {
    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        Some(Self(bytes.try_into().ok()?))
    }

    const fn flags(self) -> u8 {
        self.0[1]
    }

    const fn loop_end(self) -> bool {
        self.flags() & 1 != 0
    }

    const fn loop_repeat(self) -> bool {
        self.flags() & 2 != 0
    }

    const fn loop_start(self) -> bool {
        self.flags() & 4 != 0
    }

    fn decode(self, history: &mut [i16; 2]) -> [i16; SAMPLES_PER_BLOCK] {
        let shift = usize::from(self.0[0] & 0x0f).min(15);
        let predictor = usize::from(self.0[0] >> 4);
        let mut samples = [0_i16; SAMPLES_PER_BLOCK];
        let mut output = 0;
        for packed in &self.0[2..] {
            for nibble in [packed & 0x0f, packed >> 4] {
                let signed = if nibble & 8 == 0 {
                    i32::from(nibble)
                } else {
                    i32::from(nibble) - 16
                };
                let filtered = (i32::from(history[0]) * FILTER_0[predictor]
                    + i32::from(history[1]) * FILTER_1[predictor]
                    + 32)
                    >> 6;
                let decoded = ((signed * 4096) >> shift) + filtered;
                let saturated = decoded.clamp(i32::from(i16::MIN), i32::from(i16::MAX));
                history[1] = history[0];
                history[0] = i16::try_from(saturated).unwrap_or(if saturated.is_negative() {
                    i16::MIN
                } else {
                    i16::MAX
                });
                samples[output] = history[0];
                output += 1;
            }
        }
        samples
    }
}

/// Parsed blocks retained by a runtime voice so predictor history can remain
/// continuous when a repeating end flag jumps back to the loop address.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AdpcmSample {
    blocks: Arc<[AdpcmBlock]>,
    loop_start_block: Option<usize>,
}

impl AdpcmSample {
    pub(crate) fn parse(encoded: &[u8]) -> Self {
        let block_capacity = encoded.len() / BLOCK_BYTES;
        let mut blocks = Vec::new();
        if blocks.try_reserve_exact(block_capacity).is_err() {
            return Self::default();
        }
        let mut loop_candidate = None;
        let mut loop_start_block = None;
        for bytes in encoded.chunks_exact(BLOCK_BYTES) {
            let Some(block) = AdpcmBlock::from_bytes(bytes) else {
                break;
            };
            if block.loop_start() {
                loop_candidate = Some(blocks.len());
            }
            blocks.push(block);
            if block.loop_end() {
                if block.loop_repeat() {
                    loop_start_block = loop_candidate;
                }
                break;
            }
        }
        Self {
            blocks: blocks.into(),
            loop_start_block,
        }
    }

    pub(crate) fn decoded_len(&self) -> Option<usize> {
        self.blocks.len().checked_mul(SAMPLES_PER_BLOCK)
    }

    pub(crate) fn encoded_len(&self) -> Option<usize> {
        self.blocks.len().checked_mul(BLOCK_BYTES)
    }

    pub(crate) fn loop_start_sample(&self) -> Option<usize> {
        self.loop_start_block?.checked_mul(SAMPLES_PER_BLOCK)
    }

    fn block(&self, index: usize) -> Option<AdpcmBlock> {
        self.blocks.get(index).copied()
    }

    pub(crate) fn decode_first_pass(&self) -> DecodedAdpcm {
        let Some(capacity) = self.decoded_len() else {
            return DecodedAdpcm {
                samples: Vec::new(),
                loop_start: None,
            };
        };
        let mut samples = Vec::new();
        if samples.try_reserve_exact(capacity).is_err() {
            return DecodedAdpcm {
                samples,
                loop_start: None,
            };
        }
        let mut history = [0_i16; 2];
        for block in self.blocks.iter().copied() {
            samples.extend(block.decode(&mut history));
        }
        DecodedAdpcm {
            samples,
            loop_start: self.loop_start_sample(),
        }
    }
}

/// Per-key-on decoder state. Only key-on/reset clears `history`; a repeat
/// changes the block address while retaining the terminal predictor pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AdpcmPlayback {
    history: [i16; 2],
    decoded_block: [i16; SAMPLES_PER_BLOCK],
    block_index: usize,
    sample_index: usize,
    initialized: bool,
    finished: bool,
}

impl Default for AdpcmPlayback {
    fn default() -> Self {
        Self {
            history: [0; 2],
            decoded_block: [0; SAMPLES_PER_BLOCK],
            block_index: 0,
            sample_index: 0,
            initialized: false,
            finished: false,
        }
    }
}

impl AdpcmPlayback {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn next(&mut self, sample: &AdpcmSample) -> Option<i16> {
        if self.finished {
            return None;
        }
        if !self.initialized {
            self.load_block(sample, 0)?;
            self.initialized = true;
            return self.decoded_block.first().copied();
        }
        if self.sample_index + 1 < SAMPLES_PER_BLOCK {
            self.sample_index += 1;
            return self.decoded_block.get(self.sample_index).copied();
        }

        let Some(block) = sample.block(self.block_index) else {
            self.finished = true;
            return None;
        };
        let next_block = if block.loop_end() {
            if !block.loop_repeat() {
                self.finished = true;
                return None;
            }
            let Some(loop_start) = sample.loop_start_block else {
                self.finished = true;
                return None;
            };
            loop_start
        } else {
            let Some(next) = self.block_index.checked_add(1) else {
                self.finished = true;
                return None;
            };
            next
        };
        self.load_block(sample, next_block)?;
        self.decoded_block.first().copied()
    }

    fn load_block(&mut self, sample: &AdpcmSample, index: usize) -> Option<()> {
        let Some(block) = sample.block(index) else {
            self.finished = true;
            return None;
        };
        self.decoded_block = block.decode(&mut self.history);
        self.block_index = index;
        self.sample_index = 0;
        Some(())
    }
}

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
    AdpcmSample::parse(encoded).decode_first_pass()
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

    fn filtered_loop() -> Vec<u8> {
        let mut encoded = Vec::new();
        encoded.extend(block(0x00, 0, 0x11));
        encoded.extend(block(0x10, 4, 0x00));
        encoded.extend(block(0x10, 3, 0x00));
        encoded
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

    #[test]
    fn repeating_filtered_loop_keeps_terminal_predictor_history() {
        let sample = AdpcmSample::parse(&filtered_loop());
        let first_pass = sample.decode_first_pass();
        assert_eq!(first_pass.loop_start, Some(28));
        let mut playback = AdpcmPlayback::default();
        let output = (0..140)
            .map(|_| playback.next(&sample).expect("the sample repeats"))
            .collect::<Vec<_>>();
        assert_eq!(&output[..84], first_pass.samples);
        assert_eq!(
            &output[28..36],
            &[3_840, 3_600, 3_375, 3_164, 2_966, 2_781, 2_607, 2_444]
        );
        assert_eq!(&output[84..92], &[104, 98, 92, 86, 81, 76, 71, 67]);
        playback.reset();
        assert_eq!(playback.next(&sample), Some(4_096));
    }

    #[test]
    fn one_shot_end_finishes_without_decoding_an_extra_block() {
        let sample = AdpcmSample::parse(&block(0, 1, 0x11));
        let mut playback = AdpcmPlayback::default();
        assert_eq!(
            (0..SAMPLES_PER_BLOCK)
                .map(|_| playback.next(&sample))
                .collect::<Vec<_>>(),
            vec![Some(4_096); SAMPLES_PER_BLOCK]
        );
        assert_eq!(playback.next(&sample), None);
        assert_eq!(playback.next(&sample), None);
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

        #[test]
        fn arbitrary_first_pass_matches_the_streaming_decoder(
            encoded in proptest::collection::vec(any::<u8>(), 0..512)
        ) {
            let sample = AdpcmSample::parse(&encoded);
            let decoded = sample.decode_first_pass();
            let mut playback = AdpcmPlayback::default();
            let streamed = (0..decoded.samples.len())
                .map(|_| playback.next(&sample))
                .collect::<Option<Vec<_>>>();
            prop_assert_eq!(streamed.as_deref(), Some(decoded.samples.as_slice()));
            if decoded.loop_start.is_none() {
                prop_assert_eq!(playback.next(&sample), None);
            }
        }
    }
}
