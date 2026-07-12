//! Wrapping fixed-point and 12-bit-angle mathematics.

use core::ops::{Add, AddAssign, Sub, SubAssign};

const TRIG_Q: u32 = 48;
const TRIG_HALF: i128 = 1_i128 << (TRIG_Q - 1);
const HALF_PI_Q48: i128 = 442_139_859_501_778;

/// Signed two-dimensional vector in engine coordinate units.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Vec2 {
    pub x: i32,
    pub y: i32,
}

/// Signed three-dimensional vector in engine coordinate units.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Vec3 {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl Vec3 {
    pub const ZERO: Self = Self { x: 0, y: 0, z: 0 };

    /// Component-wise wrapping addition.
    #[must_use]
    pub const fn wrapping_add(self, rhs: Self) -> Self {
        Self {
            x: self.x.wrapping_add(rhs.x),
            y: self.y.wrapping_add(rhs.y),
            z: self.z.wrapping_add(rhs.z),
        }
    }

    /// Component-wise wrapping subtraction.
    #[must_use]
    pub const fn wrapping_sub(self, rhs: Self) -> Self {
        Self {
            x: self.x.wrapping_sub(rhs.x),
            y: self.y.wrapping_sub(rhs.y),
            z: self.z.wrapping_sub(rhs.z),
        }
    }

    /// Scales each component with wrapping multiplication.
    #[must_use]
    pub const fn wrapping_scale(self, factor: i32) -> Self {
        Self {
            x: self.x.wrapping_mul(factor),
            y: self.y.wrapping_mul(factor),
            z: self.z.wrapping_mul(factor),
        }
    }
}

impl Add for Vec3 {
    type Output = Self;
    fn add(self, rhs: Self) -> Self::Output {
        self.wrapping_add(rhs)
    }
}

impl AddAssign for Vec3 {
    fn add_assign(&mut self, rhs: Self) {
        *self = self.wrapping_add(rhs);
    }
}

impl Sub for Vec3 {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self::Output {
        self.wrapping_sub(rhs)
    }
}

impl SubAssign for Vec3 {
    fn sub_assign(&mut self, rhs: Self) {
        *self = self.wrapping_sub(rhs);
    }
}

/// Axis-aligned inclusive bounds.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Bounds3 {
    pub min: Vec3,
    pub max: Vec3,
}

impl Bounds3 {
    /// Constructs bounds when every minimum is at or below its maximum.
    #[must_use]
    pub const fn new(min: Vec3, max: Vec3) -> Option<Self> {
        if min.x <= max.x && min.y <= max.y && min.z <= max.z {
            Some(Self { min, max })
        } else {
            None
        }
    }

    #[must_use]
    pub const fn contains(self, point: Vec3) -> bool {
        point.x >= self.min.x
            && point.x <= self.max.x
            && point.y >= self.min.y
            && point.y <= self.max.y
            && point.z >= self.min.z
            && point.z <= self.max.z
    }

    #[must_use]
    pub const fn intersects(self, rhs: Self) -> bool {
        self.max.x >= rhs.min.x
            && rhs.max.x >= self.min.x
            && self.max.y >= rhs.min.y
            && rhs.max.y >= self.min.y
            && self.max.z >= rhs.min.z
            && rhs.max.z >= self.min.z
    }

    #[must_use]
    pub const fn translated(self, delta: Vec3) -> Self {
        Self {
            min: self.min.wrapping_add(delta),
            max: self.max.wrapping_add(delta),
        }
    }
}

/// Euler angles in the engine's unusual storage order (`y`, `x`, `z`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Angles {
    pub y: Angle12,
    pub x: Angle12,
    pub z: Angle12,
}

/// A wrapping angle where 4096 units are one complete revolution.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Angle12(u16);

impl Angle12 {
    pub const FULL_TURN: u16 = 0x1000;
    pub const HALF_TURN: u16 = 0x800;
    pub const QUARTER_TURN: u16 = 0x400;

    #[must_use]
    pub const fn new(value: i32) -> Self {
        Self((value as u16) & 0x0fff)
    }

    #[must_use]
    pub const fn raw(self) -> u16 {
        self.0
    }

    /// Shortest signed difference `target - self`, in `[-2048, 2048]`.
    #[must_use]
    pub const fn difference_to(self, target: Self) -> i16 {
        let difference = target.0 as i32 - self.0 as i32;
        let wrapped = if difference > 0x800 {
            difference - 0x1000
        } else if difference < -0x800 {
            difference + 0x1000
        } else {
            difference
        };
        wrapped as i16
    }

    #[must_use]
    pub const fn wrapping_add(self, delta: i32) -> Self {
        Self::new((self.0 as i32).wrapping_add(delta))
    }

    /// Q12 sine. This integer Taylor evaluation reproduces all 1,025 values
    /// in the source quarter-wave table, without platform `libm` behavior.
    #[must_use]
    pub fn sin_q12(self) -> i16 {
        let raw = self.0;
        let (quarter_index, sign) = match raw {
            0x000..=0x3ff => (raw, 1_i32),
            0x400..=0x7ff => (0x800 - raw, 1),
            0x800..=0xbff => (raw - 0x800, -1),
            _ => (0x1000 - raw, -1),
        };
        let magnitude = quarter_sine_q12(quarter_index);
        (i32::from(magnitude) * sign) as i16
    }

    #[must_use]
    pub fn cos_q12(self) -> i16 {
        self.wrapping_add(i32::from(Self::QUARTER_TURN)).sin_q12()
    }
}

fn q48_mul(left: i128, right: i128) -> i128 {
    (left * right + TRIG_HALF) >> TRIG_Q
}

fn quarter_sine_q12(index: u16) -> i16 {
    let x = (HALF_PI_Q48 * i128::from(index) + 512) / 1024;
    let x_squared = q48_mul(x, x);
    let mut term = x;
    let mut sum = x;
    let mut subtract = true;
    for term_index in 1_i128..=8 {
        let divisor = (term_index * 2) * (term_index * 2 + 1);
        term = q48_mul(term, x_squared) / divisor;
        if subtract {
            sum -= term;
        } else {
            sum += term;
        }
        subtract = !subtract;
    }
    ((sum * 4096 + TRIG_HALF) >> TRIG_Q) as i16
}

/// Generic signed fixed-point value with wrapping storage arithmetic.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct FixedI32<const FRACTION_BITS: u32>(i32);

impl<const FRACTION_BITS: u32> FixedI32<FRACTION_BITS> {
    pub const ONE: Self = Self(1_i32.wrapping_shl(FRACTION_BITS));

    #[must_use]
    pub const fn from_raw(raw: i32) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn raw(self) -> i32 {
        self.0
    }

    #[must_use]
    pub const fn from_integer(value: i32) -> Self {
        Self(value.wrapping_shl(FRACTION_BITS))
    }

    #[must_use]
    pub const fn trunc(self) -> i32 {
        self.0 >> FRACTION_BITS
    }

    #[must_use]
    pub const fn wrapping_add(self, rhs: Self) -> Self {
        Self(self.0.wrapping_add(rhs.0))
    }

    #[must_use]
    pub const fn wrapping_sub(self, rhs: Self) -> Self {
        Self(self.0.wrapping_sub(rhs.0))
    }

    #[must_use]
    pub fn wrapping_mul(self, rhs: Self) -> Self {
        let product = i64::from(self.0) * i64::from(rhs.0);
        Self((product >> FRACTION_BITS) as i32)
    }
}

/// Arithmetic-right-shift floor helper matching the source macro.
#[must_use]
pub const fn shift_floor(value: i32, bits: u32) -> i32 {
    value >> bits
}

/// Division by a power of two rounded toward positive infinity.
#[must_use]
pub const fn shift_ceil(value: i32, bits: u32) -> i32 {
    if bits == 0 {
        return value;
    }
    let bias = (1_i32 << bits) - 1;
    value.wrapping_add(bias) >> bits
}

/// Seeks toward a target without overshoot.
#[must_use]
pub const fn seek(current: i32, target: i32, delta: i32) -> i32 {
    let delta = delta.saturating_abs();
    if current < target {
        let next = current.saturating_add(delta);
        if next < target { next } else { target }
    } else {
        let next = current.saturating_sub(delta);
        if next > target { next } else { target }
    }
}

/// Integer square root (`floor(sqrt(value))`).
#[must_use]
pub fn integer_sqrt(value: u64) -> u32 {
    let mut remainder = value;
    let mut root = 0_u64;
    let mut bit = 1_u64 << 62;
    while bit > remainder {
        bit >>= 2;
    }
    while bit != 0 {
        if remainder >= root + bit {
            remainder -= root + bit;
            root = (root >> 1) + bit;
        } else {
            root >>= 1;
        }
        bit >>= 2;
    }
    root as u32
}

/// Source-compatible approximate distance: max axis plus one quarter of the
/// other two axes.
#[must_use]
pub fn approximate_distance(left: Vec3, right: Vec3) -> i32 {
    let dx = left.x.wrapping_sub(right.x).unsigned_abs();
    let dy = left.y.wrapping_sub(right.y).unsigned_abs();
    let dz = left.z.wrapping_sub(right.z).unsigned_abs();
    let maximum = dx.max(dy).max(dz);
    let remainder = dx.wrapping_add(dy).wrapping_add(dz).wrapping_sub(maximum);
    maximum.wrapping_add(remainder / 4) as i32
}

/// Euclidean distance with the source's eight-bit pre/post scaling.
#[must_use]
pub fn euclidean_distance(left: Vec3, right: Vec3) -> i32 {
    let dx = i64::from(left.x.wrapping_sub(right.x) >> 8);
    let dy = i64::from(left.y.wrapping_sub(right.y) >> 8);
    let dz = i64::from(left.z.wrapping_sub(right.z) >> 8);
    i32::try_from(
        u64::from(integer_sqrt(
            dx.unsigned_abs().pow(2) + dy.unsigned_abs().pow(2) + dz.unsigned_abs().pow(2),
        )) << 8,
    )
    .unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn angle_cardinals_and_source_table_samples_are_exact() {
        assert_eq!(Angle12::new(0).sin_q12(), 0);
        assert_eq!(Angle12::new(0x100).sin_q12(), 1_567);
        assert_eq!(Angle12::new(0x400).sin_q12(), 4_096);
        assert_eq!(Angle12::new(0x800).sin_q12(), 0);
        assert_eq!(Angle12::new(0xc00).sin_q12(), -4_096);
        assert_eq!(Angle12::new(0).cos_q12(), 4_096);
    }

    #[test]
    fn signed_angle_difference_uses_short_route() {
        assert_eq!(Angle12::new(0xff0).difference_to(Angle12::new(0x010)), 0x20);
        assert_eq!(
            Angle12::new(0x010).difference_to(Angle12::new(0xff0)),
            -0x20
        );
    }

    #[test]
    fn integer_square_root_is_floor() {
        assert_eq!(integer_sqrt(0), 0);
        assert_eq!(integer_sqrt(15), 3);
        assert_eq!(integer_sqrt(16), 4);
        assert_eq!(integer_sqrt(u64::from(u32::MAX)), 65_535);
    }

    #[test]
    fn source_approximate_distance_rule() {
        assert_eq!(
            approximate_distance(
                Vec3::ZERO,
                Vec3 {
                    x: 100,
                    y: 40,
                    z: 20
                }
            ),
            115
        );
    }

    proptest! {
        #[test]
        fn trig_has_half_turn_antisymmetry(angle in 0_u16..0x1000) {
            let a = Angle12::new(i32::from(angle));
            prop_assert_eq!(a.sin_q12(), -a.wrapping_add(0x800).sin_q12());
        }

        #[test]
        fn bounds_intersection_is_symmetric(
            ax in -1000_i32..1000, ay in -1000_i32..1000, az in -1000_i32..1000,
            bx in -1000_i32..1000, by in -1000_i32..1000, bz in -1000_i32..1000,
        ) {
            let a = Bounds3 { min: Vec3 { x: ax, y: ay, z: az }, max: Vec3 { x: ax + 10, y: ay + 10, z: az + 10 } };
            let b = Bounds3 { min: Vec3 { x: bx, y: by, z: bz }, max: Vec3 { x: bx + 10, y: by + 10, z: bz + 10 } };
            prop_assert_eq!(a.intersects(b), b.intersects(a));
        }
    }
}
