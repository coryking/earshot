//! Synthesis-side helpers of the AMBE+2 decoder: everything the per-half-frame synthesis routine
//! (0x08055d74) calls besides the voiced and unvoiced generators.
//!
//! The firmware passes these routines pointers into a 0x44-halfword *parameter block* (a stack
//! local, or one of the copies at state+0x3ec / 0x474 / 0x4fc). The port takes the block as a
//! `&[i16]` / `&mut [i16]` of at least [`BLOCK_WORDS`] halfwords. Halfwords 0x40-0x41 of a block
//! hold a firmware pointer to the per-harmonic voicing array; in the port that array is an
//! explicit `&mut [i16]` of [`MAX_HARMONICS`] entries and the two halfwords are never read.
//!
//! Block layout as far as these routines show it (halfword indices):
//!
//! | index | meaning |
//! |-------|---------|
//! | 0 | frame type: 1 voiced/unvoiced harmonics, 2 erasure/silence, 3 tone |
//! | 1 | pitch / tone code (0xff = none); classified by 0x080556bc |
//! | 2 | number of harmonics L (9..=56) |
//! | 3 | tone path: index of the dominant harmonic; (tone type 3 with class 1..3: two indices packed in bytes) |
//! | 4-5 | 32-bit word, 16 two-bit band voicing codes |
//! | 6 | harmonic spacing (frequency step) |
//! | 8.. | harmonic amplitudes, [`MAX_HARMONICS`] halfwords |
//! | 0x40-0x41 | pointer to the voicing array (firmware only) |
//! | 0x42-0x43 | 32-bit exponent of the amplitudes |

use super::math;
use super::ops::*;
use super::state::State;
use super::tables::synth::{COS_TAB, POPCNT8, SYNTH_FIR10};

/// Halfwords in a parameter block (0x44).
pub const BLOCK_WORDS: usize = 0x44;
/// Capacity of the amplitude and voicing arrays (0x38 halfwords).
pub const MAX_HARMONICS: usize = 0x38;

/// Block halfword indices (see the table in the module documentation).
pub mod blk {
    pub const KIND: usize = 0;
    pub const CODE: usize = 1;
    pub const COUNT: usize = 2;
    pub const PEAK: usize = 3;
    pub const BANDS: usize = 4;
    pub const STEP: usize = 6;
    pub const AMPS: usize = 8;
    pub const EXP: usize = 0x42;
}

/// State-block byte offsets used by the helpers' callers.
pub mod off {
    /// 0x38-halfword output of [`super::amp_fir`] (only ever written for voiced frames).
    pub const AMP_FIR: usize = 0x176;
    /// Two halfwords updated by [`super::enhance`]: mantissa (0x7c4) and exponent (0x7c6).
    pub const ENHANCE_MEM: usize = 0x7c4;
    /// Halfword updated by [`super::level_update`] (0x7c2).
    pub const LEVEL: usize = 0x7c2;
}

/// 32-bit word made of block halfwords `i` (low) and `i + 1` (high).
#[inline(always)]
fn word(p: &[i16], i: usize) -> i32 {
    (p[i] as u16 as i32) | ((p[i + 1] as i32) << 16)
}
#[inline(always)]
fn set_word(p: &mut [i16], i: usize, v: i32) {
    p[i] = v as i16;
    p[i + 1] = (v >> 16) as i16;
}

/// Normalisation shift of the firmware idiom `mvnlt; clz; sub #1` (0 for 0), as a 16-bit value.
#[inline(always)]
fn nrm(x: i32) -> i32 {
    norm32(x) as i16 as i32
}

/// 0x0805ec20: filter the harmonic amplitudes with a 10-tap odd-difference FIR.
///
/// `amps` are the L amplitudes (block halfwords 8..8+L; log-domain values in the call that
/// matters). They are extended to 64 points (beyond L by a constant slope of -0x05c5 per point,
/// saturating) and, when L > 45, mirrored past point 63. With `x(j) = amps[j-1]` for j >= 1,
/// `x(0) = 0` and `x(-j) = x(j)`, output `t` is `2 * sum_k c[k] * (x(t+2+2k) - x(t-2k))`.
/// `dst` (state+0x176, 0x38 halfwords) receives L outputs followed by zeros. What the result is
/// used for is unclear.
pub fn amp_fir(dst: &mut [i16], amps: &[i16]) {
    let l = amps.len();
    assert!((1..=MAX_HARMONICS).contains(&l));
    // b[0x13] is the (zero) centre; b[0x14..] the extended amplitudes; b[0..0x13] their mirror.
    let mut b = [0i16; 0x60];
    b[0x14..0x14 + l].copy_from_slice(amps);
    if l <= 0x3f {
        let mut v = (amps[l - 1] as i32) << 16;
        for k in l..0x40 {
            v = qsub(v, 0x05c5_0000);
            b[0x14 + k] = (v >> 16) as i16;
        }
    }
    if l > 0x2d {
        for j in 0..l - 0x2d {
            b[0x54 + j] = b[0x52 - j];
        }
    }
    for k in 0..0x13 {
        b[0x12 - k] = b[0x14 + k];
    }
    for t in 0..l {
        let mut acc = 0i32;
        for (k, &c) in SYNTH_FIR10.iter().enumerate() {
            let c = c as i32;
            let x = b[t + 0x15 + 2 * k] as i32;
            let y = b[t + 0x13 - 2 * k] as i32;
            acc = acc.wrapping_add((x * c) << 1);
            acc = acc.wrapping_sub((c * y) << 1);
        }
        // Keeps the sign bit and doubles bits 0..29 (bit 30 is dropped).
        let r = (acc & i32::MIN).wrapping_add((((acc as u32) << 2) >> 1) as i32);
        dst[t] = (r >> 16) as i16;
    }
    dst[l..MAX_HARMONICS].fill(0);
}

/// 0x0805d754: turn the L log-domain amplitudes of a block into linear values with a common
/// exponent. Each is clamped to +-0x77ff, then replaced by `2^(a / 2^11)` scaled to Q(shift)
/// where shift = `(max * 32 + 0x10000) >> 16` (max = largest clamped value); the block's
/// exponent word (halfwords 0x42-0x43) becomes `shift - 15`. The amplitude slots L.. are zeroed.
pub fn amp_expand(p: &mut [i16]) {
    let l = p[blk::COUNT] as usize;
    let a = &mut p[blk::AMPS..blk::AMPS + MAX_HARMONICS];
    let mut max = -0x8000i32;
    for v in a[..l].iter_mut() {
        let c = (*v as i32).clamp(-0x77ff, 0x77ff);
        *v = c as i16;
        max = max.max(c);
    }
    let shift = (((max << 16) >> 11) + 0x10000) >> 16;
    for v in a[..l].iter_mut() {
        *v = math::pow2_scaled(*v as i32, 4, shift) as i16;
    }
    a[l..].fill(0);
    set_word(p, blk::EXP, shift - 15);
}

/// 0x080556bc: classify a pitch/tone code: 0 for 5..=0x7a, 1 for 0x80..=0x8f, 2 for
/// 0x90..=0x9f, 3 for 0xa0..=0xa3, 4 otherwise. (Private copy; the routine itself belongs to the
/// frame-decoding package.)
fn code_class(code: i32) -> i32 {
    let x = code as u16;
    if x.wrapping_sub(5) <= 0x75 {
        0
    } else if x.wrapping_sub(0x80) <= 0xf {
        1
    } else if x.wrapping_sub(0x90) <= 0xf {
        2
    } else if x.wrapping_sub(0xa0) <= 3 {
        3
    } else {
        4
    }
}

/// 0x0805d7d8: per-harmonic voicing from the 16 two-bit band codes in `bits`.
///
/// Harmonic `i` takes the OR of band codes `idx` and `idx + 1` (the last band repeated at the
/// end), where `idx = ((i + 1) * step * 4) >> 16`; a result of 3 becomes 1. The array is
/// zero-filled from `l` up to [`MAX_HARMONICS`].
pub fn band_voicing(v: &mut [i16], bits: i32, step: i32, l: usize) {
    let mut bands = [0i16; 16];
    let mut b = bits;
    for x in bands.iter_mut() {
        *x = (b & 3) as i16;
        b >>= 2;
    }
    let inc = step << 2;
    let mut acc = inc;
    for out in v[..l].iter_mut() {
        let idx = acc >> 16;
        let (lo, hi) = if idx <= 15 {
            // A negative index would read the firmware's stack below the array; not reachable.
            let i = (idx as usize).min(15);
            (bands[i], bands[(i + 1).min(15)])
        } else {
            (bands[15], bands[15])
        };
        let o = (lo | hi) as i32;
        *out = if o > 2 { 1 } else { o } as i16;
        acc = acc.wrapping_add(inc);
    }
    v[l..MAX_HARMONICS].fill(0);
}

/// 0x08055c74: build the per-harmonic voicing array `v` ([`MAX_HARMONICS`] halfwords) for a
/// parameter block. Called on the current block and on the previous-frame block (state+0x474).
///
/// Type 1: [`band_voicing`]; type 2: all zero; type 3: all zero, then the harmonics named by the
/// tone code get 1 (class 0: index `p[3]`; class 1..=3: the two byte-packed indices of `p[3]`).
/// Any other type leaves `v` untouched (the firmware array is stack memory it does not write).
pub fn voicing_array(p: &[i16], v: &mut [i16]) {
    match p[blk::KIND] {
        1 => band_voicing(v, word(p, blk::BANDS), p[blk::STEP] as i32, p[blk::COUNT] as usize),
        2 => v[..MAX_HARMONICS].fill(0),
        3 => {
            v[..MAX_HARMONICS].fill(0);
            let class = code_class(p[blk::CODE] as i32);
            let mut set = |i: usize| {
                // The firmware indexes unchecked; an index past the array is dropped here.
                if let Some(x) = v.get_mut(i) {
                    *x = 1;
                }
            };
            if class == 0 {
                set(p[blk::PEAK] as usize);
            } else if (1..4).contains(&class) {
                let w = p[blk::PEAK] as u16;
                set((w >> 8) as usize);
                set((w & 0xff) as usize);
            }
        }
        _ => {}
    }
}

/// 0x0805d85c: update the running level `prev` (state+0x7c2) from the block's step `step`
/// (halfword 6) and band voicing word `bits` (halfwords 4-5). When more than seven of the 16
/// band codes have their low bit set the level becomes `0.9 * prev + 0.1 * step` (computed as
/// `(prev * 0x7333 * 2 + step * 0x6666 / 4) >> 16`); otherwise it is unchanged. Returns the new
/// level (the caller stores 16 bits). Unclear what the level represents.
pub fn level_update(prev: i32, step: i32, bits: u32) -> i32 {
    let m = bits & 0x5555_5555;
    let count: i32 = m.to_le_bytes().iter().map(|&b| POPCNT8[b as usize] as i32).sum();
    if count > 7 {
        ((smulbb(prev, 0x7333) << 1).wrapping_add(smulbb(step, 0x6666) >> 2)) >> 16
    } else {
        prev
    }
}

/// 0x0805641c: move the dominant (tone) harmonic of a block from its current index `p[3]` to
/// `new`: the amplitude slot and the voicing flag move with it, `p[3]` is updated, and
/// `p[6]` (step) becomes `p[1] / (new + 1)` as a pseudo-float, `p[2]` (harmonic count) is
/// raised to `new + 1` when that exceeds the current count, clamped to 9..=0x38.
/// Indices must be below [`MAX_HARMONICS`].
pub fn move_peak(p: &mut [i16], v: &mut [i16], new: i32) {
    let new = new as i16 as i32;
    let old = p[blk::PEAK] as usize;
    let moved = p[blk::AMPS + old];
    p[blk::AMPS + old] = 0;
    v[old] = 0;
    p[blk::PEAK] = new as i16;
    p[blk::AMPS + new as usize] = moved;
    v[new as usize] = 1;
    let den = (p[blk::PEAK] as i32 + 1) << 16;
    let num = (p[blk::CODE] as i32) << 16;
    let s_den = nrm(den);
    let s_num = nrm(num);
    let (m, e) = math::div_norm(lsl(num, s_num), 15 - s_num, lsl(den, s_den) >> 16, 0x17 - s_den);
    let x = m << 16;
    let x = if e + 4 >= 0 { lsl(x, e + 4) } else { asr(x, -4 - e) };
    p[blk::STEP] = (x >> 16) as i16;
    let cur = p[blk::PEAK] as i32;
    let mut count = p[blk::COUNT] as i32;
    if count <= cur {
        count = (cur + 1) as i16 as i32;
    }
    p[blk::COUNT] = count.clamp(9, 0x38) as i16;
}

/// 0x08054cbc: post-filter the 32-bit accumulator `buf` in place (n = `buf.len()`, 80 in the
/// decoder). A six-word recursive filter whose state lives in the first six 32-bit words of the
/// decoder state block (offsets 0..0x18): with input x, a = s1 * 0x78ab >> 15,
/// b = s3 * 0x7c20 >> 15, c = s5 * 0x78af >> 15,
/// `u = x - s0 + a; y = u - 2 * s2 + s4 + 2 * b - c`; then
/// `(s0..s5) <- (x, u, u, y, s2, s3)`. The empty buffer is a no-op (the firmware would loop).
pub fn post_filter(buf: &mut [i32], st: &mut State) {
    let mut s = [0i32; 6];
    for (i, w) in s.iter_mut().enumerate() {
        *w = st.w(4 * i);
    }
    let mulq = |x: i32, k: i64| ((x as i64 * k) >> 15) as i32;
    for v in buf.iter_mut() {
        let x = *v;
        let a = mulq(s[1], 0x78ab);
        let b = mulq(s[3], 0x7c20);
        let c = mulq(s[5], 0x78af);
        let u = x.wrapping_sub(s[0]).wrapping_add(a);
        let y = u
            .wrapping_sub(s[2].wrapping_mul(2))
            .wrapping_add(s[4])
            .wrapping_add(b.wrapping_mul(2))
            .wrapping_sub(c);
        *v = y;
        s = [x, u, u, y, s[2], s[3]];
    }
    for (i, w) in s.iter().enumerate() {
        st.set_w(4 * i, *w);
    }
}

/// 0x08058284: write the accumulator `buf` as 16-bit samples: shift left by `b - a` (saturating
/// to 32 bits; arithmetic right when negative), add 0x8000 (saturating) and keep the high
/// half. The decoder calls it with a = 15, b = 17. `out.len()` samples are written.
pub fn write_output(out: &mut [i16], buf: &[i32], a: i32, b: i32) {
    let sh = b - a;
    for (o, &x) in out.iter_mut().zip(buf) {
        let r = if sh > 0 {
            let y = lsl(x, sh);
            let y = if asr(y, sh) != x { i32::MAX ^ (x >> 31) } else { y };
            qadd(y, 0x8000) >> 16
        } else if sh == 0 {
            qadd(x, 0x8000) >> 16
        } else {
            asr(x, -sh).wrapping_add(0x8000) >> 16
        };
        *o = r as i16;
    }
}

/// 0x0805ed14: spectral enhancement of a block's linear amplitudes (after [`amp_expand`]).
///
/// Works on the L amplitudes `p[8..8+L]` (exponent word `p[0x42..]`, pitch step `p[6]`) and on
/// two memory halfwords `mem` = state+0x7c4 (mantissa) and +0x7c6 (exponent), a pseudo-float
/// running energy updated as `0.95 * old + 0.05 * (block energy)`. From the block energy and the
/// cosine-weighted sum `sum a_i^2 cos(step * i)` (table lookup) it derives constants (U, R) and,
/// for every harmonic past the first L/8, a gain `sqrt(sqrt(R - U * cos_i) * a_i)` clamped to
/// 0.25..0.6 (`0x2000..0x4ccd`, Q15); the first L/8 amplitudes are halved. The amplitudes are
/// then renormalised and rescaled to the original energy. The exact formula is not understood
/// beyond this instruction-level port. Only `mem` changes when the energy mantissa is not
/// positive.
pub fn enhance(p: &mut [i16], mem: &mut [i16]) {
    let hw6 = p[blk::STEP] as i32;
    let l = p[blk::COUNT] as usize;
    let mut e = word(p, blk::EXP);

    let (en_m, en_e) = math::energy(&p[blk::AMPS..blk::AMPS + l]);
    let ex1 = en_e.wrapping_add(e << 1);
    let me = (en_m >> 16) as i16 as i32;

    // Cosine-weighted energy `mm * 2^ex2`.
    let mut w = [0i16; MAX_HARMONICS];
    let (mm, ex2);
    if me == 0 {
        mm = 0;
        ex2 = 0;
    } else {
        let step = (hw6 << 10) >> 4;
        let mut phase = step;
        let mut acc = 0i64;
        for (i, wv) in w[..l].iter_mut().enumerate() {
            let a = p[blk::AMPS + i] as i32;
            // A negative step (not produced by the decoder) would index before the table.
            let c = COS_TAB.get((phase >> 16) as usize).copied().unwrap_or(0) as i32;
            let q = (smulbb(a, a) >> 15) & 0xffff;
            acc += (smulbb(c, q) << 1) as i64;
            *wv = c as i16;
            phase = phase.wrapping_add(step);
        }
        if acc == 0 {
            mm = 0;
            ex2 = e << 1;
        } else {
            let h = (acc >> 32) as i16 as i32;
            let lo = acc as i32;
            let (ch, cl) = if h < 0 { (!h, !lo) } else { (h, lo) };
            let mut n = clz(ch as u32);
            if n == 32 {
                n = clz(cl as u32) + 32;
            }
            let r1 = n - 0x21;
            let r7 = if r1 < 0 { lsr(lo, -r1) | lsl(h, n - 1) } else { lsl(lo, r1) };
            ex2 = (e << 1) - r1;
            let m = r7 >> 16;
            mm = if m == -0x8000 { -0x7fff } else { m };
        }
    }

    // Smoothed energy memory: mem = 0x799a * old (Q15) + 0x6666 * me (scaled by ex1 - 4).
    let m_old = mem[0] as i32;
    let e_old = mem[1] as i32;
    let ex_max = e_old.max(ex1);
    let a1 = smulbb(m_old, 0x799a) << 1;
    let a2 = if e_old - ex_max < 0 { asr(a1, ex_max - e_old) } else { lsl(a1, e_old - ex_max) };
    let b1 = smulbb(me, 0x6666) << 1;
    let sh = ex1.wrapping_sub(4).wrapping_sub(ex_max);
    let b2 = if sh < 0 { asr(b1, ex_max.wrapping_add(4).wrapping_sub(ex1)) } else { lsl(b1, sh) };
    let sum = a2.wrapping_add(b2);
    let n = nrm(sum);
    let sum_n = lsl(sum, n);
    let r1 = ex_max - n;
    if sum_n == 0 || r1 < -0x10 {
        mem[0] = 0x7fff;
        mem[1] = 0xffefu16 as i16;
    } else {
        mem[0] = (sum_n as u32 >> 16) as i16;
        mem[1] = r1 as i16;
    }
    if me <= 0 {
        return;
    }

    let r9 = (ex1 << 1) + 1;
    // (r9 - 2 * ex1 is 1.)
    let sq = (smulbb(me, me) << 1) >> 1;
    let (d, s, special);
    if mm == 0 {
        d = sq >> 16;
        s = d;
        special = false;
    } else {
        let t = mm.wrapping_mul(mm) << 1;
        let sh = (ex2 << 1).wrapping_sub(r9);
        let t = if sh < 0 { asr(t, r9.wrapping_sub(ex2 << 1)) } else { lsl(t, sh) };
        d = sq.wrapping_sub(t) >> 16;
        s = sq.wrapping_add(t) >> 16;
        special = d == 0;
    }

    let n8 = l >> 3;
    let (u, rval, xn, s2);
    if special {
        // Both exponents are 0 here.
        u = 0;
        xn = 1;
        s2 = 1;
        rval = asr(0x7fff_0000, 1);
    } else {
        let g1 = (d.wrapping_mul(me) >> 15) as i16 as i32;
        let prod = g1.wrapping_mul(hw6) << 1;
        let n1 = nrm(prod);
        let g = math::div_half(0x3cd0_13a9, lsl(prod, n1) >> 16) as i16 as i32;
        let tf = s.wrapping_mul(g) << 1;
        let nt = nrm(tf);
        let t = lsl(tf, nt) >> 16;
        let n1p = n1 + (4 - (ex1 + r9));
        let x1 = if t != 0 { r9 + n1p - nt } else { -nt };
        let u1 = (mm.wrapping_mul(me) >> 15) & 0xffff;
        let u3 = ((smulbb(g, u1) >> 15) as i16 as i32) << 15;
        let nu = nrm(u3);
        u = lsl(u3, nu) >> 16;
        let x2 = if u != 0 { ex2 + ex1 + n1p + 2 - nu } else { -nu };
        xn = x1.max(x2) + 1;
        s2 = xn - x2;
        rval = asr(t << 16, xn - x1);
    }

    // The first L/8 harmonics are halved; the others are rescaled by a gain in 0.25..0.6.
    math::shift_in_place(&mut p[blk::AMPS..blk::AMPS + n8], e.wrapping_sub(e.wrapping_add(1)));
    for i in n8..l {
        let t = asr(u.wrapping_mul(w[i] as i32) << 1, s2);
        let dd = rval.wrapping_sub(t);
        let ai = p[blk::AMPS + i] as i32;
        let mut r0 = 0;
        let mut ex = 0;
        if dd != 0 {
            ex = xn;
            let r = math::sqrt_norm(dd, &mut ex);
            let v = smultb(r, ai) << 1;
            ex = ex.wrapping_add(e);
            r0 = math::sqrt_norm(v, &mut ex);
            if ex > 1 {
                p[blk::AMPS + i] = ((0x4ccd * ai) >> 15) as i16;
                continue;
            }
        }
        if ex != 1 {
            r0 = asr(r0, 1 - ex);
        }
        let g = (r0 >> 16).clamp(0x2000, 0x4ccd);
        p[blk::AMPS + i] = ((g * ai) >> 15) as i16;
    }

    // Renormalise and restore the energy.
    e += 1;
    let a = &mut p[blk::AMPS..blk::AMPS + l];
    math::normalize_in_place(a, &mut e);
    let (m2, e2) = math::energy(a);
    let t3 = e2.wrapping_add(e << 1);
    if m2 >> 16 != 0 {
        let (mq, mut eq) = math::div_norm(me << 16, ex1, m2 >> 16, t3);
        let g = math::sqrt_norm(mq << 16, &mut eq);
        math::mul_q15_in_place(a, g >> 16);
        e = e.wrapping_add(eq);
        if e > 0 {
            let mut src = [0i16; MAX_HARMONICS];
            src[..l].copy_from_slice(a);
            math::shift_diff_sat(a, &src[..l], e, 0);
            e = 0;
        }
    }
    set_word(p, blk::EXP, e);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ambe::testvec::{self, Record, assert_h_eq, assert_state_eq};

    fn each(addr: u32, f: impl Fn(&Record)) {
        let recs = testvec::load(addr).expect("recordings");
        assert!(!recs.is_empty());
        println!("{addr:#010x}: replaying {} calls", recs.len());
        for r in &recs {
            f(r);
        }
    }

    /// Block at `ptr` before / after the call, with the two pointer halfwords cleared.
    fn blocks(r: &Record, ptr: u32) -> (Vec<i16>, Vec<i16>) {
        let mut b = r.h_before(ptr, BLOCK_WORDS);
        let mut a = r.h_after(ptr, BLOCK_WORDS);
        b[0x40] = 0;
        b[0x41] = 0;
        a[0x40] = 0;
        a[0x41] = 0;
        (b, a)
    }
    /// The voicing array a block points to, before / after the call.
    fn voicing(r: &Record, ptr: u32) -> (Vec<i16>, Vec<i16>) {
        let w = r.w_before(ptr + 0x80, 1)[0] as u32;
        (r.h_before(w, MAX_HARMONICS), r.h_after(w, MAX_HARMONICS))
    }

    #[test]
    fn amp_fir_matches_firmware() {
        each(0x0805_ec20, |r| {
            let l = r.r[2] as usize;
            let amps = r.h_before(r.r[1], l);
            let mut st = r.state_before();
            let o = r.state_off(r.r[0]);
            assert_eq!(o, off::AMP_FIR);
            amp_fir(st.s_mut(o, MAX_HARMONICS), &amps);
            assert_state_eq(r, &st, &r.state_after(), &[]);
        });
    }

    #[test]
    fn amp_expand_matches_firmware() {
        each(0x0805_d754, |r| {
            let base = r.r[0] - 0x10;
            assert_eq!(r.r[1], base + 0x84);
            let (mut p, want) = blocks(r, base);
            amp_expand(&mut p);
            assert_h_eq("block", r, &p, &want);
        });
    }

    #[test]
    fn voicing_array_matches_firmware() {
        each(0x0805_5c74, |r| {
            let (p, _) = blocks(r, r.r[0]);
            let (mut v, want) = voicing(r, r.r[0]);
            voicing_array(&p, &mut v);
            assert_h_eq("voicing", r, &v, &want);
        });
    }

    #[test]
    fn level_update_matches_firmware() {
        each(0x0805_d85c, |r| {
            let got = level_update(r.r[0] as i32, r.r[1] as i32, r.r[2]);
            assert_eq!(got, r.ret[0] as i32);
        });
    }

    #[test]
    fn move_peak_matches_firmware() {
        // No frame corpus reaches this routine (it needs state+0x7a4 == 1); the recordings are
        // empty. It was verified against direct emulator calls with synthetic blocks instead:
        // point AMBE_VECTORS at such a recording to replay it here.
        let Some(recs) = testvec::load(0x0805_641c) else { return };
        if recs.is_empty() {
            eprintln!("0x0805641c: no recorded calls");
            return;
        }
        println!("0x0805641c: replaying {} calls", recs.len());
        for r in &recs {
            let (mut p, want) = blocks(r, r.r[0]);
            let (mut v, want_v) = voicing(r, r.r[0]);
            move_peak(&mut p, &mut v, r.r[1] as i32);
            assert_h_eq("block", r, &p, &want);
            assert_h_eq("voicing", r, &v, &want_v);
        }
    }

    #[test]
    fn post_filter_matches_firmware() {
        each(0x0805_4cbc, |r| {
            assert_eq!(r.r[0], r.r[1], "call sites filter in place");
            let n = r.r[3] as usize;
            assert_eq!(r.state_off(r.r[2]), 0);
            let mut buf = r.w_before(r.r[0], n);
            let mut st = r.state_before();
            post_filter(&mut buf, &mut st);
            assert_eq!(buf, r.w_after(r.r[1], n));
            assert_state_eq(r, &st, &r.state_after(), &[]);
        });
    }

    #[test]
    fn write_output_matches_firmware() {
        each(0x0805_8284, |r| {
            let n = r.stack[0] as usize;
            let buf = r.w_before(r.r[2], n);
            let mut out = r.h_before(r.r[0], n);
            write_output(&mut out, &buf, r.r[1] as i32, r.r[3] as i32);
            assert_h_eq("output", r, &out, &r.h_after(r.r[0], n));
        });
    }

    #[test]
    fn enhance_matches_firmware() {
        each(0x0805_ed14, |r| {
            let base = r.r[0] - 0x10;
            assert_eq!(r.r[1], base + 0x84);
            let (mut p, want) = blocks(r, base);
            let so = r.state_off(r.r[2]);
            assert_eq!(so, off::ENHANCE_MEM);
            assert_eq!(r.state_off(r.r[3]), off::ENHANCE_MEM + 2);
            assert_eq!(r.stack[0] as i32, p[blk::STEP] as i32);
            assert_eq!(r.stack[1] as i32, p[blk::COUNT] as i32);
            let sb = r.state_before();
            let sa = r.state_after();
            let mut mem = [sb.h(so), sb.h(so + 2)];
            enhance(&mut p, &mut mem);
            assert_h_eq("block", r, &p, &want);
            assert_h_eq("mem", r, &mem, &[sa.h(so), sa.h(so + 2)]);
        });
    }
}
