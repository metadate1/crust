//! Exact fixed-point PS1 SPU Gaussian sample interpolation.
//!
//! The hardware keeps four decoded ADPCM samples, selects four coefficients
//! from its 512-word ROM with pitch-counter bits 4..11, shifts each product
//! independently, and then adds the four signed terms. Keeping that ordering
//! avoids accidentally replacing the SPU's integer rounding with a host
//! floating-point approximation.

use crate::{adpcm::AdpcmPlayback, mixer::Sample};

pub(crate) const PITCH_UNITS: u64 = 0x1000;
const MAX_PITCH_STEP: u64 = 0x4000;

/// Per-key-on sample decoder, interpolation history, and pitch counter.
/// Repeating ADPCM changes only the source block address: both predictor and
/// Gaussian history continue across that jump until the voice is re-keyed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SpuSampleCursor {
    fraction: u16,
    history: [i16; 4],
    pcm_index: usize,
    adpcm: AdpcmPlayback,
    initialized: bool,
    finished: bool,
}

impl SpuSampleCursor {
    pub(crate) fn new() -> Self {
        Self {
            fraction: 0,
            history: [0; 4],
            pcm_index: 0,
            adpcm: AdpcmPlayback::default(),
            initialized: false,
            finished: false,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.fraction = 0;
        self.history = [0; 4];
        self.pcm_index = 0;
        self.adpcm.reset();
        self.initialized = false;
        self.finished = false;
    }

    /// Produces one 44.1 kHz sample and advances by one SPU pitch step.
    ///
    /// Hardware clamps unmodulated pitch steps above `0x3fff` to `0x4000`.
    /// A zero step intentionally keeps returning the same interpolated value.
    pub(crate) fn next(&mut self, sample: &Sample, step: u64) -> Option<i16> {
        if self.finished {
            return None;
        }
        if !self.initialized {
            self.history[0] = self.next_source_sample(sample)?;
            self.initialized = true;
        }

        let output = interpolate(self.history, self.fraction);
        let counter = u64::from(self.fraction) + step.min(MAX_PITCH_STEP);
        self.fraction =
            u16::try_from(counter % PITCH_UNITS).expect("an SPU pitch-counter fraction fits u16");
        let advance = usize::try_from(counter / PITCH_UNITS)
            .expect("the clamped SPU pitch step advances at most four samples");
        for _ in 0..advance {
            let Some(next) = self.next_source_sample(sample) else {
                self.finished = true;
                break;
            };
            self.history[3] = self.history[2];
            self.history[2] = self.history[1];
            self.history[1] = self.history[0];
            self.history[0] = next;
        }
        Some(output)
    }

    fn next_source_sample(&mut self, sample: &Sample) -> Option<i16> {
        if let Some(adpcm) = sample.adpcm() {
            return self.adpcm.next(adpcm);
        }
        if self.initialized {
            self.pcm_index = match self.pcm_index.checked_add(1) {
                Some(next) if next < sample.len() => next,
                _ => sample.loop_start()?,
            };
        } else {
            self.pcm_index = 0;
        }
        sample.sample(self.pcm_index)
    }
}

/// Interpolates `[new, old, older, oldest]` decoded samples with the exact SPU
/// coefficient addressing and per-product signed arithmetic shifts.
fn interpolate(samples: [i16; 4], counter_fraction: u16) -> i16 {
    let index = usize::from((counter_fraction >> 4) & 0xff);
    let [new, old, older, oldest] = samples.map(i32::from);
    let mut output = (i32::from(GAUSSIAN_COEFFICIENTS[0x0ff - index]) * oldest) >> 15;
    output += (i32::from(GAUSSIAN_COEFFICIENTS[0x1ff - index]) * older) >> 15;
    output += (i32::from(GAUSSIAN_COEFFICIENTS[0x100 + index]) * old) >> 15;
    output += (i32::from(GAUSSIAN_COEFFICIENTS[index]) * new) >> 15;
    i16::try_from(output.clamp(i32::from(i16::MIN), i32::from(i16::MAX)))
        .expect("the clamped SPU interpolation result fits i16")
}

// Fixed 16-bit coefficient ROM exposed by the original SPU's interpolation
// behavior. These are hardware constants, not game data.
#[rustfmt::skip]
const GAUSSIAN_COEFFICIENTS: [i16; 512] = [
    -0x001, -0x001, -0x001, -0x001, -0x001, -0x001, -0x001, -0x001,
    -0x001, -0x001, -0x001, -0x001, -0x001, -0x001, -0x001, -0x001,
     0x0000,  0x0000,  0x0000,  0x0000,  0x0000,  0x0000,  0x0000,  0x0001,
     0x0001,  0x0001,  0x0001,  0x0002,  0x0002,  0x0002,  0x0003,  0x0003,
     0x0003,  0x0004,  0x0004,  0x0005,  0x0005,  0x0006,  0x0007,  0x0007,
     0x0008,  0x0009,  0x0009,  0x000a,  0x000b,  0x000c,  0x000d,  0x000e,
     0x000f,  0x0010,  0x0011,  0x0012,  0x0013,  0x0015,  0x0016,  0x0018,
     0x0019,  0x001b,  0x001c,  0x001e,  0x0020,  0x0021,  0x0023,  0x0025,
     0x0027,  0x0029,  0x002c,  0x002e,  0x0030,  0x0033,  0x0035,  0x0038,
     0x003a,  0x003d,  0x0040,  0x0043,  0x0046,  0x0049,  0x004d,  0x0050,
     0x0054,  0x0057,  0x005b,  0x005f,  0x0063,  0x0067,  0x006b,  0x006f,
     0x0074,  0x0078,  0x007d,  0x0082,  0x0087,  0x008c,  0x0091,  0x0096,
     0x009c,  0x00a1,  0x00a7,  0x00ad,  0x00b3,  0x00ba,  0x00c0,  0x00c7,
     0x00cd,  0x00d4,  0x00db,  0x00e3,  0x00ea,  0x00f2,  0x00fa,  0x0101,
     0x010a,  0x0112,  0x011b,  0x0123,  0x012c,  0x0135,  0x013f,  0x0148,
     0x0152,  0x015c,  0x0166,  0x0171,  0x017b,  0x0186,  0x0191,  0x019c,
     0x01a8,  0x01b4,  0x01c0,  0x01cc,  0x01d9,  0x01e5,  0x01f2,  0x0200,
     0x020d,  0x021b,  0x0229,  0x0237,  0x0246,  0x0255,  0x0264,  0x0273,
     0x0283,  0x0293,  0x02a3,  0x02b4,  0x02c4,  0x02d6,  0x02e7,  0x02f9,
     0x030b,  0x031d,  0x0330,  0x0343,  0x0356,  0x036a,  0x037e,  0x0392,
     0x03a7,  0x03bc,  0x03d1,  0x03e7,  0x03fc,  0x0413,  0x042a,  0x0441,
     0x0458,  0x0470,  0x0488,  0x04a0,  0x04b9,  0x04d2,  0x04ec,  0x0506,
     0x0520,  0x053b,  0x0556,  0x0572,  0x058e,  0x05aa,  0x05c7,  0x05e4,
     0x0601,  0x061f,  0x063e,  0x065c,  0x067c,  0x069b,  0x06bb,  0x06dc,
     0x06fd,  0x071e,  0x0740,  0x0762,  0x0784,  0x07a7,  0x07cb,  0x07ef,
     0x0813,  0x0838,  0x085d,  0x0883,  0x08a9,  0x08d0,  0x08f7,  0x091e,
     0x0946,  0x096f,  0x0998,  0x09c1,  0x09eb,  0x0a16,  0x0a40,  0x0a6c,
     0x0a98,  0x0ac4,  0x0af1,  0x0b1e,  0x0b4c,  0x0b7a,  0x0ba9,  0x0bd8,
     0x0c07,  0x0c38,  0x0c68,  0x0c99,  0x0ccb,  0x0cfd,  0x0d30,  0x0d63,
     0x0d97,  0x0dcb,  0x0e00,  0x0e35,  0x0e6b,  0x0ea1,  0x0ed7,  0x0f0f,
     0x0f46,  0x0f7f,  0x0fb7,  0x0ff1,  0x102a,  0x1065,  0x109f,  0x10db,
     0x1116,  0x1153,  0x118f,  0x11cd,  0x120b,  0x1249,  0x1288,  0x12c7,
     0x1307,  0x1347,  0x1388,  0x13c9,  0x140b,  0x144d,  0x1490,  0x14d4,
     0x1517,  0x155c,  0x15a0,  0x15e6,  0x162c,  0x1672,  0x16b9,  0x1700,
     0x1747,  0x1790,  0x17d8,  0x1821,  0x186b,  0x18b5,  0x1900,  0x194b,
     0x1996,  0x19e2,  0x1a2e,  0x1a7b,  0x1ac8,  0x1b16,  0x1b64,  0x1bb3,
     0x1c02,  0x1c51,  0x1ca1,  0x1cf1,  0x1d42,  0x1d93,  0x1de5,  0x1e37,
     0x1e89,  0x1edc,  0x1f2f,  0x1f82,  0x1fd6,  0x202a,  0x207f,  0x20d4,
     0x2129,  0x217f,  0x21d5,  0x222c,  0x2282,  0x22da,  0x2331,  0x2389,
     0x23e1,  0x2439,  0x2492,  0x24eb,  0x2545,  0x259e,  0x25f8,  0x2653,
     0x26ad,  0x2708,  0x2763,  0x27be,  0x281a,  0x2876,  0x28d2,  0x292e,
     0x298b,  0x29e7,  0x2a44,  0x2aa1,  0x2aff,  0x2b5c,  0x2bba,  0x2c18,
     0x2c76,  0x2cd4,  0x2d33,  0x2d91,  0x2df0,  0x2e4f,  0x2eae,  0x2f0d,
     0x2f6c,  0x2fcc,  0x302b,  0x308b,  0x30ea,  0x314a,  0x31aa,  0x3209,
     0x3269,  0x32c9,  0x3329,  0x3389,  0x33e9,  0x3449,  0x34a9,  0x3509,
     0x3569,  0x35c9,  0x3629,  0x3689,  0x36e8,  0x3748,  0x37a8,  0x3807,
     0x3867,  0x38c6,  0x3926,  0x3985,  0x39e4,  0x3a43,  0x3aa2,  0x3b00,
     0x3b5f,  0x3bbd,  0x3c1b,  0x3c79,  0x3cd7,  0x3d35,  0x3d92,  0x3def,
     0x3e4c,  0x3ea9,  0x3f05,  0x3f62,  0x3fbd,  0x4019,  0x4074,  0x40d0,
     0x412a,  0x4185,  0x41df,  0x4239,  0x4292,  0x42eb,  0x4344,  0x439c,
     0x43f4,  0x444c,  0x44a3,  0x44fa,  0x4550,  0x45a6,  0x45fc,  0x4651,
     0x46a6,  0x46fa,  0x474e,  0x47a1,  0x47f4,  0x4846,  0x4898,  0x48e9,
     0x493a,  0x498a,  0x49d9,  0x4a29,  0x4a77,  0x4ac5,  0x4b13,  0x4b5f,
     0x4bac,  0x4bf7,  0x4c42,  0x4c8d,  0x4cd7,  0x4d20,  0x4d68,  0x4db0,
     0x4df7,  0x4e3e,  0x4e84,  0x4ec9,  0x4f0e,  0x4f52,  0x4f95,  0x4fd7,
     0x5019,  0x505a,  0x509a,  0x50da,  0x5118,  0x5156,  0x5194,  0x51d0,
     0x520c,  0x5247,  0x5281,  0x52ba,  0x52f3,  0x532a,  0x5361,  0x5397,
     0x53cc,  0x5401,  0x5434,  0x5467,  0x5499,  0x54ca,  0x54fa,  0x5529,
     0x5558,  0x5585,  0x55b2,  0x55de,  0x5609,  0x5632,  0x565b,  0x5684,
     0x56ab,  0x56d1,  0x56f6,  0x571b,  0x573e,  0x5761,  0x5782,  0x57a3,
     0x57c3,  0x57e2,  0x57ff,  0x581c,  0x5838,  0x5853,  0x586d,  0x5886,
     0x589e,  0x58b5,  0x58cb,  0x58e0,  0x58f4,  0x5907,  0x5919,  0x592a,
     0x593a,  0x5949,  0x5958,  0x5965,  0x5971,  0x597c,  0x5986,  0x598f,
     0x5997,  0x599e,  0x59a4,  0x59a9,  0x59ad,  0x59b0,  0x59b2,  0x59b3,
];

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn adpcm_block(header: u8, flags: u8, packed: u8) -> [u8; 16] {
        let mut block = [0_u8; 16];
        block[0] = header;
        block[1] = flags;
        block[2..].fill(packed);
        block
    }

    #[test]
    fn coefficient_rom_and_phase_sums_match_the_spu() {
        assert_eq!(GAUSSIAN_COEFFICIENTS.len(), 512);
        assert_eq!(&GAUSSIAN_COEFFICIENTS[..16], &[-1; 16]);
        assert_eq!(GAUSSIAN_COEFFICIENTS[16], 0);
        assert_eq!(GAUSSIAN_COEFFICIENTS[0x1ff], 0x59b3);
        for index in 0..=0xff {
            let sum = i32::from(GAUSSIAN_COEFFICIENTS[0x0ff - index])
                + i32::from(GAUSSIAN_COEFFICIENTS[0x1ff - index])
                + i32::from(GAUSSIAN_COEFFICIENTS[0x100 + index])
                + i32::from(GAUSSIAN_COEFFICIENTS[index]);
            assert!(
                (0x7f7f..=0x7f81).contains(&sum),
                "phase {index:#04x}: {sum:#06x}"
            );
        }
    }

    #[test]
    fn interpolation_uses_counter_bits_four_through_eleven() {
        let history = [1_000, -2_000, 3_000, -4_000];
        assert_eq!(
            [
                interpolate(history, 0x0000),
                interpolate(history, 0x000f),
                interpolate(history, 0x0800),
                interpolate(history, 0x0ff0),
                interpolate(history, 0xffff),
            ],
            [1_216, 1_216, 438, -811, -811]
        );
    }

    #[test]
    fn signed_extreme_goldens_preserve_each_product_shift() {
        assert_eq!(
            [
                interpolate([i16::MAX; 4], 0x0000),
                interpolate([i16::MIN; 4], 0x0000),
                interpolate([i16::MIN, i16::MAX, i16::MIN, i16::MAX], 0x0800),
                interpolate([i16::MAX, i16::MIN, i16::MAX, i16::MIN], 0x0ff0),
            ],
            [32_637, -32_640, 79, -13_286]
        );
    }

    #[test]
    fn startup_history_is_zero_filled_and_pitch_is_clamped() {
        let sample = Sample::new(vec![1_000, 2_000, 3_000, 4_000], None);
        let mut cursor = SpuSampleCursor::new();
        assert_eq!(cursor.next(&sample, u64::MAX), Some(-1));
        assert_eq!(cursor.fraction, 0);
        assert!(cursor.finished);
        assert_eq!(cursor.next(&sample, PITCH_UNITS), None);
    }

    #[test]
    fn loop_history_wraps_to_the_end_only_after_the_first_pass() {
        let sample = Sample::new(vec![1_000, 2_000, 3_000, 4_000], Some(1));
        let mut cursor = SpuSampleCursor::new();
        assert_eq!(
            cursor.next(&sample, 3 * PITCH_UNITS + PITCH_UNITS / 2),
            Some(-1)
        );
        let output = [
            cursor.next(&sample, PITCH_UNITS),
            cursor.next(&sample, PITCH_UNITS),
            cursor.next(&sample, PITCH_UNITS),
        ];
        assert_eq!(output, [Some(2_490), Some(3_447), Some(2_983)]);
        assert!(!cursor.finished);
    }

    #[test]
    fn one_shot_finishes_after_its_last_fractional_sample() {
        let sample = Sample::new(vec![1_000, 2_000, 3_000, 4_000], None);
        let mut cursor = SpuSampleCursor::new();
        assert_eq!(
            cursor.next(&sample, 3 * PITCH_UNITS + PITCH_UNITS / 2),
            Some(-1)
        );
        assert_eq!(cursor.next(&sample, PITCH_UNITS), Some(2_490));
        assert_eq!(cursor.next(&sample, PITCH_UNITS), None);
    }

    #[test]
    fn reset_discards_loop_and_interpolation_history() {
        let sample = Sample::new(vec![1_000, 2_000], Some(0));
        let mut cursor = SpuSampleCursor::new();
        assert_eq!(cursor.next(&sample, 2 * PITCH_UNITS), Some(-1));
        assert_eq!(cursor.next(&sample, PITCH_UNITS), Some(996));
        cursor.reset();
        assert_eq!(cursor.next(&sample, 0), Some(-1));
        assert_eq!(cursor.next(&sample, 0), Some(-1));
        assert_eq!(cursor.fraction, 0);
        assert_eq!(cursor.history, [1_000, 0, 0, 0]);
    }

    #[test]
    fn gaussian_cursor_uses_redecoded_predictor_samples_after_repeat() {
        let mut encoded = Vec::new();
        encoded.extend(adpcm_block(0x00, 0, 0x11));
        encoded.extend(adpcm_block(0x10, 4, 0x00));
        encoded.extend(adpcm_block(0x10, 3, 0x00));
        let sample = Sample::from_adpcm(&encoded);
        let mut cursor = SpuSampleCursor::new();
        let output = (0..92)
            .map(|_| {
                cursor
                    .next(&sample, PITCH_UNITS)
                    .expect("the sample repeats")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            &output[80..92],
            &[150, 140, 130, 123, 115, 108, 101, 95, 89, 84, 78, 73]
        );
        assert_ne!(output[84], output[28]);
    }

    proptest! {
        #[test]
        fn arbitrary_pcm_cursors_are_bounded_and_deterministic(
            pcm in proptest::collection::vec(any::<i16>(), 0..128),
            requested_loop in any::<Option<usize>>(),
            steps in proptest::collection::vec(any::<u64>(), 0..128),
        ) {
            let sample = Sample::new(pcm, requested_loop);
            let mut first = SpuSampleCursor::new();
            let mut second = first.clone();
            for step in steps {
                prop_assert_eq!(first.next(&sample, step), second.next(&sample, step));
            }
            prop_assert_eq!(first, second);
        }
    }
}
