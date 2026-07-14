//! Deterministic retail Euler matrices shared by object and sprite paths.

use crate::projection::Matrix3;

const TRIG_Q: u32 = 48;
const TRIG_HALF: i128 = 1_i128 << (TRIG_Q - 1);
const HALF_PI_Q48: i128 = 442_139_859_501_778;

pub(crate) fn yxy_rotation_matrix(rotation_xyz: [u16; 3]) -> Matrix3 {
    let sx = sine_q12(rotation_xyz[0]);
    let sy = sine_q12(rotation_xyz[1]);
    let sz = sine_q12(rotation_xyz[2]);
    let cx = cosine_q12(rotation_xyz[0]);
    let cy = cosine_q12(rotation_xyz[1]);
    let cz = cosine_q12(rotation_xyz[2]);
    let sxsy = multiply_q12(sx, sy);
    let sxsz = multiply_q12(sx, sz);
    let sysz = multiply_q12(sy, sz);
    let cxsy = multiply_q12(cx, sy);
    let cxsz = multiply_q12(cx, sz);
    let sxcy = multiply_q12(sx, cy);
    let sxcz = multiply_q12(sx, cz);
    let sycz = multiply_q12(sy, cz);
    let cxcz = multiply_q12(cx, cz);
    let sxcysz = multiply_q12(sxcy, sz);
    let sxcycz = multiply_q12(sxcy, cz);
    let cxcysz = multiply_q12(cxsz, cy);
    let cxcycz = multiply_q12(cxcz, cy);
    Matrix3 {
        values: [
            [cxcz.wrapping_sub(sxcysz), sysz, sxcz.wrapping_add(cxcysz)],
            [sxsy, cy, cxsy.wrapping_neg()],
            [
                cxsz.wrapping_neg().wrapping_sub(sxcycz),
                sycz,
                sxsz.wrapping_neg().wrapping_add(cxcycz),
            ],
        ],
    }
}

/// Source `SwRotMatrixZXY`, retaining each intermediate Q12 truncation.
pub(crate) fn zxy_rotation_matrix(rotation_xyz: [u16; 3]) -> Matrix3 {
    let sx = sine_q12(rotation_xyz[0]);
    let sy = sine_q12(rotation_xyz[1]);
    let sz = sine_q12(rotation_xyz[2]);
    let cx = cosine_q12(rotation_xyz[0]);
    let cy = cosine_q12(rotation_xyz[1]);
    let cz = cosine_q12(rotation_xyz[2]);
    let sxsy = multiply_q12(sx, sy);
    let sxsz = multiply_q12(sx, sz);
    let cxsz = multiply_q12(cx, sz);
    let cysz = multiply_q12(cy, sz);
    let sxcy = multiply_q12(sx, cy);
    let sxcz = multiply_q12(sx, cz);
    let cxcy = multiply_q12(cx, cy);
    let cxcz = multiply_q12(cx, cz);
    let cycz = multiply_q12(cy, cz);
    let sxsysz = multiply_q12(sxsy, sz);
    let cxsysz = multiply_q12(cxsz, sy);
    let sxsycz = multiply_q12(sxsy, cz);
    let cxsycz = multiply_q12(cxcz, sy);
    Matrix3 {
        values: [
            [
                cxcz.wrapping_sub(sxsysz),
                cysz.wrapping_neg(),
                sxcz.wrapping_add(cxsysz),
            ],
            [cxsz.wrapping_add(sxsycz), cycz, sxsz.wrapping_sub(cxsycz)],
            [sxcy.wrapping_neg(), sy, cxcy],
        ],
    }
}

pub(crate) fn angle12(value: i32) -> u16 {
    u16::try_from(value.rem_euclid(0x1000)).expect("a reduced angle is in 0..4096")
}

fn multiply_q12(left: i16, right: i16) -> i16 {
    i16::try_from((i32::from(left) * i32::from(right)) >> 12)
        .expect("a product of two Q12 i16 coefficients still fits i16")
}

fn sine_q12(angle: u16) -> i16 {
    let angle = angle & 0x0fff;
    let (quarter_index, sign) = match angle {
        0x000..=0x3ff => (angle, 1_i32),
        0x400..=0x7ff => (0x800 - angle, 1),
        0x800..=0xbff => (angle - 0x800, -1),
        _ => (0x1000 - angle, -1),
    };
    i16::try_from(i32::from(quarter_sine_q12(quarter_index)) * sign)
        .expect("signed Q12 sine fits i16")
}

fn cosine_q12(angle: u16) -> i16 {
    sine_q12(angle.wrapping_add(0x400) & 0x0fff)
}

fn quarter_sine_q12(index: u16) -> i16 {
    let x = (HALF_PI_Q48 * i128::from(index) + 512) / 1024;
    let x_squared = q48_multiply(x, x);
    let mut term = x;
    let mut sum = x;
    let mut subtract = true;
    for term_index in 1_i128..=8 {
        let divisor = (term_index * 2) * (term_index * 2 + 1);
        term = q48_multiply(term, x_squared) / divisor;
        if subtract {
            sum -= term;
        } else {
            sum += term;
        }
        subtract = !subtract;
    }
    i16::try_from((sum * 4096 + TRIG_HALF) >> TRIG_Q).expect("quarter-wave Q12 sine fits i16")
}

fn q48_multiply(left: i128, right: i128) -> i128 {
    (left * right + TRIG_HALF) >> TRIG_Q
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zxy_quarter_turns_retain_retail_axis_order() {
        assert_eq!(zxy_rotation_matrix([0, 0, 0]), Matrix3::IDENTITY);
        assert_eq!(
            zxy_rotation_matrix([0x400, 0, 0]).values,
            [[0, 0, 4096], [0, 4096, 0], [-4096, 0, 0]]
        );
        assert_eq!(
            zxy_rotation_matrix([0, 0x400, 0]).values,
            [[4096, 0, 0], [0, 0, -4096], [0, 4096, 0]]
        );
        assert_eq!(
            zxy_rotation_matrix([0, 0, 0x400]).values,
            [[0, -4096, 0], [4096, 0, 0], [0, 0, 4096]]
        );
    }
}
