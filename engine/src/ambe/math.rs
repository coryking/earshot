//! The firmware's shared fixed-point helpers: block copies and shifts, divisions, pseudo-float
//! add/subtract, and the polynomial sqrt / log2 / pow2 approximations.
//!
//! Each function names the firmware routine it ports. Slices replace the firmware's
//! (pointer, count) pairs; routines whose loops would never terminate for a count of zero are
//! only ever called with a positive count, and the port treats an empty slice as a no-op.

use super::ops::*;
use super::tables::math::{LOG2_POLY, POW2_POLY, SQRT_POLY};

/// 0x0805359c: fill halfwords with a value.
#[inline]
pub fn fill(dst: &mut [i16], v: i16) {
    dst.fill(v);
}

/// 0x08056580: |a| / |b| >> 1 (truncating, sign of a ^ b), or 0 when |a| < |b|.
/// |i32::MIN| saturates and |b| is taken as a 16-bit value.
pub fn div_half(a: i32, b: i32) -> i32 {
    let (ma, mb) = div_magnitudes(a, b);
    let q = if ma >= mb { sdiv(ma, mb) >> 1 } else { 0 };
    if (a ^ b) < 0 { q.wrapping_neg() } else { q }
}

/// 0x08056640: |a| / |b| (truncating, sign of a ^ b), or 0 when |a| < |b|.
pub fn div_int(a: i32, b: i32) -> i32 {
    let (ma, mb) = div_magnitudes(a, b);
    let q = if ma >= mb { sdiv(ma, mb) } else { 0 };
    if (a ^ b) < 0 { q.wrapping_neg() } else { q }
}

#[inline(always)]
fn div_magnitudes(a: i32, b: i32) -> (i32, i32) {
    let ma = if a >= 0 { a } else if a == i32::MIN { i32::MAX } else { -a };
    let mb = if b >= 0 {
        b
    } else if b == -0x8000 {
        0x7fff
    } else {
        b.wrapping_neg() as i16 as i32
    };
    (ma, mb)
}

/// 0x08056800: pseudo-float division (num * 2^num_exp) / (den * 2^den_exp).
/// Returns the normalised 16-bit mantissa and its exponent (0 when the mantissa is 0).
pub fn div_norm(num: i32, num_exp: i32, den: i32, den_exp: i32) -> (i32, i32) {
    let sign = den ^ num;
    let mut n = if num >= 0 { num } else if num == i32::MIN { i32::MAX } else { -num };
    let d = if den >= 0 {
        den
    } else if den == -0x8000 {
        0x7fff
    } else {
        den.wrapping_neg() as i16 as i32
    };
    let mut e = num_exp;
    if n >= lsl(d, 16) {
        n >>= 1;
        e += 1;
    }
    let mut q = (sdiv(n, d) >> 1) & 0xffff;
    if sign < 0 {
        q = q.wrapping_neg() & 0xffff;
    }
    let q = q << 16;
    let s = norm32(q) as i16 as i32;
    let m = lsr(lsl(q, s), 16);
    let e = if m != 0 { e - den_exp - s } else { m };
    (m as i16 as i32, e)
}

/// 0x08056ac0: square root of x * 2^exp by polynomial. Returns the mantissa in the high
/// halfword (low halfword 0) and halves `exp` (rounding up). x == 0 returns 0 and leaves `exp`.
pub fn sqrt_norm(x: i32, exp: &mut i32) -> i32 {
    if x == 0 {
        return 0;
    }
    let s = norm32(x) as i16 as i32;
    let m = lsr(lsl(x, s), 16);
    let e = *exp - s;
    let t = &SQRT_POLY;
    let mut r = (((t[1] as i32) << 16).wrapping_add(smulbb(t[0] as u16 as i32, m) << 1)).wrapping_add(0x8000);
    r = ((t[2] as i32) << 16).wrapping_add(smultb(r, m) << 1).wrapping_add(0x8000) & !0xffff;
    if e & 1 != 0 {
        r = smultb(r, t[3] as i32) << 1;
    }
    r = r.wrapping_add(0x8000) & !0xffff;
    *exp = (e + 1) >> 1;
    r
}

/// 0x08056b40: sqrt of x * 2^exp as a Q(shift) 16-bit value (rounded).
pub fn sqrt_scaled(x: i32, exp: i32, shift: i32) -> i32 {
    let mut e = exp;
    let r = sqrt_norm(x, &mut e);
    let d = e - shift;
    let r = if d < 0 { asr(r, shift - e) } else { lsl(r, d) };
    r.wrapping_add(0x8000) >> 16
}

/// 0x080569c0: 2^x for x in Q16 (integer part in the high halfword). Returns the mantissa
/// (Q30-ish, 32 bits) and the exponent (integer part + 1).
pub fn pow2_poly(x: i32) -> (i32, i32) {
    let int = x >> 16;
    let f = (x & 0xffff) >> 1;
    let t = &POW2_POLY;
    let step = |acc: i32| -> i32 { (smultb(acc, f) << 1).wrapping_add(0x8000) >> 16 };
    let mut r2 = (smulbb(t[0] as i32, f) << 1).wrapping_add(0x8000) >> 16;
    r2 = step(((t[1] as i32) << 16).wrapping_add(r2 << 13));
    r2 = step(((t[2] as i32) << 15).wrapping_add(r2 << 13));
    r2 = step(((t[3] as i32) << 15).wrapping_add(r2 << 14));
    r2 = step(((t[4] as i32) << 16).wrapping_add(r2 << 15));
    (((t[5] as i32) << 16).wrapping_add(r2 << 15), int + 1)
}

/// 0x08056a80: 2^(m * 2^(e-15)) as a Q(shift) 16-bit value (rounded).
pub fn pow2_scaled(m: i32, e: i32, shift: i32) -> i32 {
    let x = m << 16;
    let x = if e > 14 { lsl(x, e - 15) } else { asr(x, 15 - e) };
    let (r, e) = pow2_poly(x);
    let d = e - shift;
    let r = if d < 0 { asr(r, shift - e) } else { lsl(r, d) };
    r.wrapping_add(0x8000) >> 16
}

/// 0x08056900: log2 of x * 2^e by polynomial; result in Q16 (integer part high).
pub fn log2_poly(x: i32, e: i32) -> i32 {
    let x = x << 16;
    let s = if x != 0 { clz(x as u32) - 1 } else { 0 } as i16 as i32;
    let t = &LOG2_POLY;
    let r2 = lsl(x, s).wrapping_sub((t[0] as i32) << 16);
    let mut r0 = smulbt(t[1] as u16 as i32, r2);
    r0 = (t[2] as i32).wrapping_add(lsr(0x8000i32.wrapping_add(r0 << 1), 16));
    r0 = smulbt(r0, r2);
    r0 = (t[3] as i32).wrapping_add(lsr(0x8000i32.wrapping_add(r0 << 1), 16));
    r0 = smulbt(r0, r2);
    r0 = 0x8000i32.wrapping_add(r0 << 1) & !0xffff;
    r0 = ((t[4] as i32) << 16).wrapping_add(r0 >> 1);
    let p = lsr(0x8000i32.wrapping_add(smultt(r0, r2) << 1), 16);
    let r0 = ((t[5] as i32) << 16).wrapping_add(p << 18);
    ((e - s) << 16).wrapping_add(r0 >> 15)
}

/// 0x08056980: log2 of x * 2^e as a Q(15 - shift) 16-bit value (rounded).
pub fn log2_scaled(x: i32, e: i32, shift: i32) -> i32 {
    let r = log2_poly(x, e);
    let d = 15 - shift;
    let r = if d >= 0 { lsl(r, d) } else { asr(r, shift - 15) };
    r.wrapping_add(0x8000) >> 16
}

/// 0x08058058: dst = src * gain, shifted left by (e1 - e2 - 15) (right when negative).
pub fn scale_shift(dst: &mut [i16], src: &[i16], gain: i32, e1: i32, e2: i32) {
    let sh = e1 - e2 - 15;
    for (d, &s) in dst.iter_mut().zip(src) {
        let p = gain.wrapping_mul(s as i32);
        *d = if sh >= 0 { lsl(p, sh) } else { asr(p, -sh) } as i16;
    }
}
/// 0x08058058 with dst == src.
pub fn scale_shift_in_place(buf: &mut [i16], gain: i32, e1: i32, e2: i32) {
    let sh = e1 - e2 - 15;
    for d in buf.iter_mut() {
        let p = gain.wrapping_mul(*d as i32);
        *d = if sh >= 0 { lsl(p, sh) } else { asr(p, -sh) } as i16;
    }
}

/// 0x080580b8: block energy 2 * sum(x^2), normalised to 32 bits. Returns (mantissa, exponent)
/// with energy = mantissa * 2^exponent; (0, 0) for silence.
pub fn energy(x: &[i16]) -> (i32, i32) {
    let mut acc: i64 = 0;
    for &v in x {
        acc = acc.wrapping_add(v as i64 * v as i64);
    }
    let v = acc.wrapping_mul(2);
    if v == 0 {
        return (0, 0);
    }
    let s = v >> 63;
    let a = (v ^ s).wrapping_sub(s);
    let hi16 = ((a >> 32) as u32) & 0xffff;
    if hi16 == 0 {
        let low = a as u32;
        if (low as i32) < 0 {
            ((((v as u64) >> 1) as u32) as i32, 1)
        } else {
            let sh = clz(low) - 1;
            (lsl(v as i32, sh), -sh)
        }
    } else {
        let sh = 33 - clz(hi16 as i16 as i32 as u32);
        ((v >> sh) as i32, sh)
    }
}

/// 0x08058098: dst = (src * gain) >> 15 (bits 15..30, sign extended).
pub fn mul_q15(dst: &mut [i16], src: &[i16], gain: i32) {
    for (d, &s) in dst.iter_mut().zip(src) {
        *d = (gain.wrapping_mul(s as i32) >> 15) as i16;
    }
}
/// 0x08058098 with dst == src.
pub fn mul_q15_in_place(buf: &mut [i16], gain: i32) {
    for d in buf.iter_mut() {
        *d = (gain.wrapping_mul(*d as i32) >> 15) as i16;
    }
}

#[inline(always)]
fn shift_one(v: i16, sh: i32) -> i16 {
    if sh > 0 { lsl(v as i32, sh) as i16 } else { asr(v as i32, -sh) as i16 }
}
/// 0x08058170: dst = src << sh (>> -sh when negative; plain copy for 0).
pub fn shift(dst: &mut [i16], src: &[i16], sh: i32) {
    for (d, &s) in dst.iter_mut().zip(src) {
        *d = shift_one(s, sh);
    }
}
/// 0x08058170 with dst == src.
pub fn shift_in_place(buf: &mut [i16], sh: i32) {
    if sh != 0 {
        for d in buf.iter_mut() {
            *d = shift_one(*d, sh);
        }
    }
}

/// 0x080581dc: -(normalisation shift of the block's largest magnitude), i.e. the headroom
/// to give back after normalising; 0 for an all-zero block.
pub fn max_abs_norm(x: &[i16]) -> i32 {
    let mut m = 0i32;
    for &v in x {
        let a = (v as i32).abs();
        if m < a {
            m = a;
        }
    }
    let m = if m == 0x8000 { 0x7fff_0000 } else { m << 16 };
    -norm32(m)
}

/// 0x0805821c: normalise a block (dst = src << headroom) and add the shift applied
/// (as a negative exponent change) to `exp`.
pub fn normalize(dst: &mut [i16], src: &[i16], exp: &mut i32) {
    let sh = max_abs_norm(src);
    shift(dst, src, -sh);
    *exp += sh;
}
/// 0x0805821c with dst == src.
pub fn normalize_in_place(buf: &mut [i16], exp: &mut i32) {
    let sh = max_abs_norm(buf);
    if sh != 0 {
        shift_in_place(buf, -sh);
    }
    *exp += sh;
}

/// 0x08058334: dst = src << (a - b) (>> when not positive).
pub fn shift_diff(dst: &mut [i16], src: &[i16], a: i32, b: i32) {
    shift(dst, src, a - b);
}

/// 0x0805836c: dst = src << (a - b) with saturation (>> when not positive).
pub fn shift_diff_sat(dst: &mut [i16], src: &[i16], a: i32, b: i32) {
    let sh = a - b;
    for (d, &s) in dst.iter_mut().zip(src) {
        *d = if sh > 0 {
            let x = (s as i32) << 16;
            let mut y = lsl(x, sh);
            if asr(y, sh) != x {
                y = i32::MAX ^ (x >> 31);
            }
            (y >> 16) as i16
        } else {
            asr(s as i32, -sh) as i16
        };
    }
}

/// 0x08056680: pseudo-float add (m1 * 2^e1) + (m2 * 2^e2) -> (mantissa, exponent).
pub fn float_add(m1: i32, e1: i32, m2: i32, e2: i32) -> (i32, i32) {
    float_addsub(m1, e1, m2, e2, false)
}
/// 0x08056700: pseudo-float subtract (m2 * 2^e2) - (m1 * 2^e1) -> (mantissa, exponent).
pub fn float_sub(m1: i32, e1: i32, m2: i32, e2: i32) -> (i32, i32) {
    float_addsub(m1, e1, m2, e2, true)
}
fn float_addsub(m1: i32, e1: i32, m2: i32, e2: i32, sub: bool) -> (i32, i32) {
    // m1 == 0, or m2's exponent so far above m1 that m1 vanishes: the result is m2.
    if m1 == 0 || e2 - e1 > 0x1f {
        return (m2, if m2 == 0 { 0 } else { e2 });
    }
    // m2 == 0, or m1 dominates: the result is m1 (negated for a subtraction).
    if m2 == 0 || e1 - e2 > 0x1f {
        let m = if sub {
            if m1 == -0x8000 { 0x7fff } else { m1.wrapping_neg() as i16 as i32 }
        } else {
            m1
        };
        return (m, e1);
    }
    let e = if e2 >= e1 { e2 + 1 } else { e1 + 1 };
    let a = m1 << 16;
    let a = if e1 - e < 0 { asr(a, e - e1) } else { lsl(a, e1 - e) };
    let b = m2 << 16;
    let b = if e2 - e < 0 { asr(b, e - e2) } else { lsl(b, e2 - e) };
    let sum = if sub { b.wrapping_sub(a) } else { a.wrapping_add(b) };
    let s = norm32(sum) as i16 as i32;
    let m = lsr(lsl(sum, s), 16);
    (m, if m != 0 { e - s } else { m })
}

/// 0x08057e80: sum(x) / n via `div_int` (n taken as 16 bits).
pub fn mean(x: &[i16]) -> i32 {
    let mut s = 0i32;
    for &v in x {
        s = s.wrapping_add(v as i32);
    }
    div_int(s, x.len() as i16 as i32)
}

/// 0x08060f9c: harmonic count for a fundamental (Q-format period index), clamped to 9..56.
pub fn harmonic_count(x: i32) -> i32 {
    let mut r = div_int(0x3b39c, x) as i16 as i32;
    if (r * 2).wrapping_mul(x).wrapping_add(x) >= 0x80000 {
        r = ((div_int(0x80000, x) << 16).wrapping_sub(0x10000)) >> 17;
    }
    r.clamp(9, 56)
}

/// A cursor over the one-bit-per-halfword frame buffers the firmware uses for bit fields.
pub struct Bits<'a> {
    pub bits: &'a [i16],
    pub pos: usize,
}
impl<'a> Bits<'a> {
    pub fn new(bits: &'a [i16]) -> Self {
        Self { bits, pos: 0 }
    }
    /// 0x0805d0d4 (with 0x08055b64): read `n` bits MSB first and advance.
    pub fn read(&mut self, n: usize) -> i32 {
        let v = read_bits(&self.bits[self.pos..self.pos + n]);
        self.pos += n;
        v
    }
}
/// 0x08055b64: fold one-bit-per-halfword values MSB first (bits are OR-ed, not masked).
pub fn read_bits(bits: &[i16]) -> i32 {
    let mut v = 0i32;
    for &b in bits {
        v = (b as i32) | (v << 1);
    }
    v
}
/// 0x08055bac: write the low `dst.len()` bits of `v` MSB first, one bit per halfword.
pub fn write_bits(dst: &mut [i16], v: i32) {
    let mut v = v;
    for d in dst.iter_mut().rev() {
        *d = (v & 1) as i16;
        v >>= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ambe::testvec::{self, Record};

    fn each(addr: u32, f: impl Fn(&Record)) {
        // Some helpers (0x0805836c, 0x08057e80) are reached by no corpus: empty recordings.
        if let Some(recs) = testvec::load(addr) {
            for r in &recs {
                f(r);
            }
        }
    }
    fn ret(r: &Record) -> i32 {
        r.ret[0] as i32
    }

    #[test]
    fn div_half_matches_firmware() {
        each(0x0805_6580, |r| assert_eq!(div_half(r.r[0] as i32, r.r[1] as i32), ret(r)));
    }
    #[test]
    fn div_int_matches_firmware() {
        each(0x0805_6640, |r| assert_eq!(div_int(r.r[0] as i32, r.r[1] as i32), ret(r)));
    }
    #[test]
    fn div_norm_matches_firmware() {
        each(0x0805_6800, |r| {
            let (m, e) = div_norm(r.r[0] as i32, r.r[1] as i32, r.r[2] as i32, r.r[3] as i32);
            assert_eq!(m, ret(r));
            assert_eq!(e, r.w_after(r.stack[0], 1)[0]);
        });
    }
    #[test]
    fn sqrt_matches_firmware() {
        each(0x0805_6ac0, |r| {
            let mut e = r.w_before(r.r[1], 1)[0];
            assert_eq!(sqrt_norm(r.r[0] as i32, &mut e), ret(r));
            assert_eq!(e, r.w_after(r.r[1], 1)[0]);
        });
        each(0x0805_6b40, |r| {
            assert_eq!(sqrt_scaled(r.r[0] as i32, r.r[1] as i32, r.r[2] as i32), ret(r))
        });
    }
    #[test]
    fn pow2_matches_firmware() {
        each(0x0805_69c0, |r| {
            let (m, e) = pow2_poly(r.r[0] as i32);
            assert_eq!(m, ret(r));
            assert_eq!(e, r.w_after(r.r[1], 1)[0]);
        });
        each(0x0805_6a80, |r| assert_eq!(pow2_scaled(r.r[0] as i32, r.r[1] as i32, r.r[2] as i32), ret(r)));
    }
    #[test]
    fn log2_matches_firmware() {
        each(0x0805_6900, |r| assert_eq!(log2_poly(r.r[0] as i32, r.r[1] as i32), ret(r)));
        each(0x0805_6980, |r| assert_eq!(log2_scaled(r.r[0] as i32, r.r[1] as i32, r.r[2] as i32), ret(r)));
    }
    #[test]
    fn energy_matches_firmware() {
        each(0x0805_80b8, |r| {
            let x = r.h_before(r.r[1], r.r[2] as usize);
            let (m, e) = energy(&x);
            assert_eq!(m, ret(r));
            assert_eq!(e, r.w_after(r.r[0], 1)[0]);
        });
    }
    #[test]
    fn block_ops_match_firmware() {
        // dst/src may alias in the firmware; compute from the source as it was before.
        each(0x0805_8058, |r| {
            let n = r.r[2] as usize;
            let src = r.h_before(r.r[1], n);
            let mut dst = r.h_before(r.r[0], n);
            scale_shift(&mut dst, &src, r.r[3] as i32, r.stack[0] as i32, r.stack[1] as i32);
            assert_eq!(dst, r.h_after(r.r[0], n));
        });
        each(0x0805_8098, |r| {
            let n = r.r[2] as usize;
            let src = r.h_before(r.r[1], n);
            let mut dst = r.h_before(r.r[0], n);
            mul_q15(&mut dst, &src, r.r[3] as i32);
            assert_eq!(dst, r.h_after(r.r[0], n));
        });
        each(0x0805_8170, |r| {
            let n = r.r[2] as usize;
            let src = r.h_before(r.r[1], n);
            let mut dst = r.h_before(r.r[0], n);
            shift(&mut dst, &src, r.r[3] as i32);
            assert_eq!(dst, r.h_after(r.r[0], n));
        });
        each(0x0805_81dc, |r| assert_eq!(max_abs_norm(&r.h_before(r.r[0], r.r[1] as usize)), ret(r)));
        each(0x0805_821c, |r| {
            let n = r.r[2] as usize;
            let src = r.h_before(r.r[1], n);
            let mut dst = r.h_before(r.r[0], n);
            let mut e = r.w_before(r.r[3], 1)[0];
            normalize(&mut dst, &src, &mut e);
            assert_eq!(dst, r.h_after(r.r[0], n));
            assert_eq!(e, r.w_after(r.r[3], 1)[0]);
        });
        each(0x0805_8334, |r| {
            let n = r.r[2] as usize;
            let src = r.h_before(r.r[1], n);
            let mut dst = r.h_before(r.r[0], n);
            shift_diff(&mut dst, &src, r.r[3] as i32, r.stack[0] as i32);
            assert_eq!(dst, r.h_after(r.r[0], n));
        });
        each(0x0805_836c, |r| {
            let n = r.r[2] as usize;
            let src = r.h_before(r.r[1], n);
            let mut dst = r.h_before(r.r[0], n);
            shift_diff_sat(&mut dst, &src, r.r[3] as i32, r.stack[0] as i32);
            assert_eq!(dst, r.h_after(r.r[0], n));
        });
    }
    #[test]
    fn float_ops_match_firmware() {
        each(0x0805_6680, |r| {
            let (m, e) = float_add(r.r[0] as i32, r.r[1] as i32, r.r[2] as i32, r.r[3] as i32);
            assert_eq!(m, ret(r));
            assert_eq!(e, r.w_after(r.stack[0], 1)[0]);
        });
        each(0x0805_6700, |r| {
            let (m, e) = float_sub(r.r[0] as i32, r.r[1] as i32, r.r[2] as i32, r.r[3] as i32);
            assert_eq!(m, ret(r));
            assert_eq!(e, r.w_after(r.stack[0], 1)[0]);
        });
    }
    #[test]
    fn misc_match_firmware() {
        each(0x0805_7e80, |r| assert_eq!(mean(&r.h_before(r.r[0], r.r[1] as usize)), ret(r)));
        each(0x0806_0f9c, |r| assert_eq!(harmonic_count(r.r[0] as i32), ret(r)));
        each(0x0805_5b64, |r| assert_eq!(read_bits(&r.h_before(r.r[0], r.r[1] as usize)), ret(r)));
    }
}
