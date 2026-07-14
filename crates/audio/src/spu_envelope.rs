//! Exact fixed-point PS1 SPU ADSR envelope generation.
//!
//! Retail VAB tones store the two hardware ADSR registers verbatim. The
//! generator below follows the documented 44.1 kHz shift/step/counter rules,
//! including exponential attack slowdown, exponential decay, frozen all-one
//! rates, and phase transitions at the hardware targets. It deliberately
//! keeps the envelope in signed Q15 units until the final mixer conversion.

const MAX_LEVEL: i32 = 0x7fff;
const EXPONENTIAL_ATTACK_THRESHOLD: i32 = 0x6000;
const COUNTER_TRIGGER: u32 = 0x8000;

/// Current phase of one SPU ADSR generator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpuAdsrPhase {
    Attack,
    Decay,
    Sustain,
    Release,
    Off,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RateEnvelope {
    counter: u32,
    counter_increment: u16,
    step: i16,
    rate: u8,
    frozen: bool,
    decreasing: bool,
    exponential: bool,
}

impl RateEnvelope {
    const fn dormant() -> Self {
        Self {
            counter: 0,
            counter_increment: 0,
            step: 0,
            rate: 0,
            frozen: true,
            decreasing: false,
            exponential: false,
        }
    }

    fn reset(rate: u8, rate_mask: u8, decreasing: bool, exponential: bool) -> Self {
        let base_step = i16::from(7 - (rate & 3));
        let mut step = if decreasing { !base_step } else { base_step };
        let mut counter_increment = 0x8000_u16;
        let frozen = rate & rate_mask == rate_mask;

        if rate < 44 {
            step <<= u32::from(11 - (rate >> 2));
        } else if rate >= 48 {
            let shift = u32::from((rate >> 2) - 11);
            counter_increment = counter_increment.checked_shr(shift).unwrap_or(0);
            // An all-one rate never advances. Other very slow rates still
            // receive the hardware's minimum one-count increment.
            if !frozen {
                counter_increment = counter_increment.max(1);
            }
        }

        Self {
            counter: 0,
            counter_increment,
            step,
            rate,
            frozen,
            decreasing,
            exponential,
        }
    }

    fn tick(&mut self, level: &mut i32) {
        if self.counter_increment == 0 {
            return;
        }

        let mut increment = self.counter_increment;
        let mut step = i32::from(self.step);
        if self.exponential {
            if self.decreasing {
                step = (step * *level) >> 15;
            } else if *level > EXPONENTIAL_ATTACK_THRESHOLD {
                if self.rate < 40 {
                    step >>= 2;
                } else if self.rate >= 44 {
                    increment >>= 2;
                } else {
                    step >>= 1;
                    increment >>= 1;
                }
            }
        }
        // Exponential attack applies its quarter-rate slowdown after the
        // ordinary shift/counter decode. Preserve the hardware's minimum
        // one-count increment at that final boundary; only an all-one rate
        // is genuinely frozen.
        if increment == 0 && !self.frozen {
            increment = 1;
        }

        self.counter += u32::from(increment);
        if self.counter & COUNTER_TRIGGER == 0 {
            return;
        }
        self.counter = 0;

        let next = level.saturating_add(step);
        *level = if self.decreasing {
            next.max(0)
        } else {
            next.clamp(i32::from(i16::MIN), MAX_LEVEL)
        };
    }
}

/// One hardware-compatible ADSR generator decoded from a VAB tone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpuAdsrEnvelope {
    adsr1: u16,
    adsr2: u16,
    phase: SpuAdsrPhase,
    level: i32,
    target: i32,
    rate: RateEnvelope,
}

impl SpuAdsrEnvelope {
    /// Starts a newly keyed voice at zero in its attack phase.
    #[must_use]
    pub fn new(adsr1: u16, adsr2: u16) -> Self {
        let mut result = Self {
            adsr1,
            adsr2,
            phase: SpuAdsrPhase::Attack,
            level: 0,
            target: MAX_LEVEL,
            rate: RateEnvelope::dormant(),
        };
        result.configure_phase();
        result
    }

    /// Restarts the attack exactly like a hardware key-on write.
    pub fn key_on(&mut self) {
        self.level = 0;
        self.phase = SpuAdsrPhase::Attack;
        self.configure_phase();
    }

    /// Enters release from attack, decay, or sustain. Repeated key-off writes
    /// during release and writes to an already-off voice are inert.
    pub fn key_off(&mut self) {
        if matches!(self.phase, SpuAdsrPhase::Off | SpuAdsrPhase::Release) {
            return;
        }
        self.phase = SpuAdsrPhase::Release;
        self.configure_phase();
    }

    /// Advances the envelope by one 44.1 kHz SPU sample.
    pub fn tick(&mut self) {
        if self.phase == SpuAdsrPhase::Off {
            return;
        }
        self.rate.tick(&mut self.level);

        if self.phase == SpuAdsrPhase::Sustain {
            return;
        }
        let reached_target = if self.rate.decreasing {
            self.level <= self.target
        } else {
            self.level >= self.target
        };
        if reached_target {
            self.phase = match self.phase {
                SpuAdsrPhase::Attack => SpuAdsrPhase::Decay,
                SpuAdsrPhase::Decay => SpuAdsrPhase::Sustain,
                SpuAdsrPhase::Release => SpuAdsrPhase::Off,
                SpuAdsrPhase::Sustain | SpuAdsrPhase::Off => self.phase,
            };
            self.configure_phase();
        }
    }

    #[must_use]
    pub const fn phase(&self) -> SpuAdsrPhase {
        self.phase
    }

    /// Current non-negative signed-Q15 hardware level (`0..=0x7fff`).
    #[must_use]
    pub fn level(&self) -> u16 {
        u16::try_from(self.level).unwrap_or_default()
    }

    /// Converts the Q15 level only at the software mix boundary. A hardware
    /// maximum of `0x7fff` is intentionally just below mathematical unity.
    #[must_use]
    pub fn gain(&self) -> f32 {
        f32::from(self.level()) / 32_768.0
    }

    #[must_use]
    pub const fn is_off(&self) -> bool {
        matches!(self.phase, SpuAdsrPhase::Off)
    }

    fn configure_phase(&mut self) {
        match self.phase {
            SpuAdsrPhase::Attack => {
                self.target = MAX_LEVEL;
                let attack_rate = ((self.adsr1 >> 8) & 0x7f) as u8;
                self.rate = RateEnvelope::reset(attack_rate, 0x7f, false, self.adsr1 & 0x8000 != 0);
            }
            SpuAdsrPhase::Decay => {
                self.target = (i32::from(self.adsr1 & 0x0f) + 1)
                    .saturating_mul(0x800)
                    .min(MAX_LEVEL);
                let decay_rate = ((self.adsr1 >> 4) & 0x0f) as u8;
                self.rate = RateEnvelope::reset(decay_rate << 2, 0x1f << 2, true, true);
            }
            SpuAdsrPhase::Sustain => {
                self.target = 0;
                let sustain_rate = ((self.adsr2 >> 6) & 0x7f) as u8;
                self.rate = RateEnvelope::reset(
                    sustain_rate,
                    0x7f,
                    self.adsr2 & 0x4000 != 0,
                    self.adsr2 & 0x8000 != 0,
                );
            }
            SpuAdsrPhase::Release => {
                self.target = 0;
                let release_rate = (self.adsr2 & 0x1f) as u8;
                self.rate =
                    RateEnvelope::reset(release_rate << 2, 0x1f << 2, true, self.adsr2 & 0x20 != 0);
            }
            SpuAdsrPhase::Off => {
                self.target = 0;
                self.level = 0;
                self.rate = RateEnvelope::dormant();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_attack_and_decay_match_exact_q15_golden_levels() {
        // Linear attack rate 0, exponential decay rate 0, sustain level 0.
        let mut envelope = SpuAdsrEnvelope::new(0, 0x1fc0);
        let expected = [14_336, 28_672, 32_767, 16_383, 8_191, 4_095, 2_047];
        for level in expected {
            envelope.tick();
            assert_eq!(envelope.level(), level);
        }
        assert_eq!(envelope.phase(), SpuAdsrPhase::Sustain);
    }

    #[test]
    fn exponential_attack_slows_after_the_documented_threshold() {
        let adsr1 = 0x8000 | (40_u16 << 8);
        let mut below = SpuAdsrEnvelope::new(adsr1, 0);
        below.level = EXPONENTIAL_ATTACK_THRESHOLD - 1;
        below.tick();
        assert_eq!(below.level, EXPONENTIAL_ATTACK_THRESHOLD - 1 + 14);

        let mut at = SpuAdsrEnvelope::new(adsr1, 0);
        at.level = EXPONENTIAL_ATTACK_THRESHOLD;
        at.tick();
        assert_eq!(at.level, EXPONENTIAL_ATTACK_THRESHOLD + 14);
        at.tick();
        assert_eq!(at.level, EXPONENTIAL_ATTACK_THRESHOLD + 14);
        at.tick();
        assert_eq!(at.level, EXPONENTIAL_ATTACK_THRESHOLD + 21);
    }

    #[test]
    fn slow_exponential_attack_keeps_the_minimum_nonzero_counter_increment() {
        let adsr1 = 0x8000 | (100_u16 << 8);
        let mut envelope = SpuAdsrEnvelope::new(adsr1, 0);
        envelope.level = EXPONENTIAL_ATTACK_THRESHOLD + 1;
        for _ in 0..32_767 {
            envelope.tick();
        }
        assert_eq!(envelope.level, EXPONENTIAL_ATTACK_THRESHOLD + 1);
        envelope.tick();
        assert_eq!(envelope.level, EXPONENTIAL_ATTACK_THRESHOLD + 8);
        assert_eq!(envelope.phase(), SpuAdsrPhase::Attack);
    }

    #[test]
    fn linear_and_exponential_release_diverge_exactly() {
        let mut linear = SpuAdsrEnvelope::new(0, 0);
        linear.level = MAX_LEVEL;
        linear.phase = SpuAdsrPhase::Sustain;
        linear.key_off();
        linear.tick();
        assert_eq!(linear.level(), 16_383);
        linear.tick();
        assert_eq!(linear.level(), 0);
        assert_eq!(linear.phase(), SpuAdsrPhase::Off);

        let mut exponential = SpuAdsrEnvelope::new(0, 0x20);
        exponential.level = MAX_LEVEL;
        exponential.phase = SpuAdsrPhase::Sustain;
        exponential.key_off();
        exponential.tick();
        assert_eq!(exponential.level(), 16_383);
        exponential.tick();
        assert_eq!(exponential.level(), 8_191);
        assert_eq!(exponential.phase(), SpuAdsrPhase::Release);
    }

    #[test]
    fn sustain_direction_and_mode_use_the_register_bits_verbatim() {
        let mut increasing = SpuAdsrEnvelope::new(0, 0);
        increasing.level = 20_000;
        increasing.phase = SpuAdsrPhase::Sustain;
        increasing.configure_phase();
        increasing.tick();
        assert_eq!(increasing.level(), 32_767);
        assert_eq!(increasing.phase(), SpuAdsrPhase::Sustain);

        let mut linear_decrease = SpuAdsrEnvelope::new(0, 0x4000);
        linear_decrease.level = 20_000;
        linear_decrease.phase = SpuAdsrPhase::Sustain;
        linear_decrease.configure_phase();
        linear_decrease.tick();
        assert_eq!(linear_decrease.level(), 3_616);

        let mut exponential_decrease = SpuAdsrEnvelope::new(0, 0xc000);
        exponential_decrease.level = 20_000;
        exponential_decrease.phase = SpuAdsrPhase::Sustain;
        exponential_decrease.configure_phase();
        exponential_decrease.tick();
        assert_eq!(exponential_decrease.level(), 10_000);
    }

    #[test]
    fn all_one_rates_reproduce_the_hardware_frozen_cases() {
        let mut attack = SpuAdsrEnvelope::new(0x7f00, 0);
        for _ in 0..100_000 {
            attack.tick();
        }
        assert_eq!(attack.level(), 0);
        assert_eq!(attack.phase(), SpuAdsrPhase::Attack);

        let mut release = SpuAdsrEnvelope::new(0, 0x1f);
        release.level = MAX_LEVEL;
        release.phase = SpuAdsrPhase::Sustain;
        release.key_off();
        for _ in 0..100_000 {
            release.tick();
        }
        assert_eq!(release.level(), 32_767);
        assert_eq!(release.phase(), SpuAdsrPhase::Release);
    }

    #[test]
    fn every_rate_stays_bounded_and_can_be_rekeyed() {
        for attack_rate in 0_u16..=127 {
            for exponential in [false, true] {
                let mode = if exponential { 0x8000 } else { 0 };
                let mut envelope = SpuAdsrEnvelope::new(mode | (attack_rate << 8), 0);
                for _ in 0..512 {
                    envelope.tick();
                    assert!(envelope.level <= MAX_LEVEL);
                    assert!(envelope.level >= 0);
                }
                envelope.key_off();
                envelope.tick();
                envelope.key_on();
                assert_eq!(envelope.level(), 0);
                assert_eq!(envelope.phase(), SpuAdsrPhase::Attack);
            }
        }
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "the Q15 conversion has one exact binary floating-point result"
    )]
    fn full_level_gain_preserves_q15_headroom() {
        let mut envelope = SpuAdsrEnvelope::new(0, 0);
        envelope.level = MAX_LEVEL;
        assert_eq!(envelope.gain(), 32_767.0 / 32_768.0);
    }
}
