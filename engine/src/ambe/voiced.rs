//! Voiced (harmonic) synthesis: firmware 0x0805df08 .. 0x0805e584.
//!
//! My reading of the algorithm, from the arithmetic: the decoder builds one pitch period of the
//! voiced signal in the frequency domain (one complex bin per harmonic, amplitude from the
//! parameter block, phase from the per-harmonic phase array in the state), inverse-transforms it
//! to a 256 sample table ([`build_period`]), then lays copies of that table into the output
//! accumulator, one per pitch period, by stepping a phase accumulator through the table
//! ([`overlay_period`], [`osc_run`]) with raised-cosine fades at the two ends of each copy. The
//! top routine ([`synth_voiced`]) cross-fades the tail of the previous frame's voiced signal
//! (kept in the state) into the new frame. Names of the intermediate quantities are guesses;
//! where the meaning is unclear the firmware's own variable names from the decompilation
//! (`local_308` ...) are kept in the comments.
#![allow(clippy::too_many_arguments)]

use super::fft::fft_inverse;
use super::math::{div_norm, float_add, float_sub, sqrt_norm};
use super::ops::*;
use super::state::State;
use super::tables::voiced::COS512;

/// The synthesis state lives behind a pointer to state+0x18; these are offsets in the state
/// block (0x18 plus the firmware's offset from that pointer).
mod off {
    /// 84 words: the overlap tail of the accumulator, carried to the next call.
    pub const TAIL: usize = 0x18;
    /// 32-bit position accumulator (`local_308`), a count of pitch periods in Q16 (unclear).
    pub const POS: usize = 0x168;
    /// 16-bit mantissa of the position of the next period start relative to the frame (float).
    pub const MANT: usize = 0x16c;
    /// 32-bit exponent of the same float (only the low half is read back).
    pub const EXP: usize = 0x170;
    /// 56 halfword per-harmonic phases (read here, written elsewhere).
    pub const PHASES: usize = 0x176;
    /// Halfword exponent of the period table.
    pub const TAB_EXP: usize = 0x1e6;
    /// The period table: 256 samples plus a copy of the first (257 halfwords).
    pub const TAB: usize = 0x1e8;
}
/// Words in the overlap tail.
const TAIL_WORDS: usize = 84;
/// Halfwords in the period table including the guard entry.
const TAB_LEN: usize = 0x101;
/// Harmonics the phase array holds.
const PHASE_LEN: usize = 56;

#[inline(always)]
fn sext(x: i32) -> i32 {
    x as i16 as i32
}
/// SBFX: signed bit field of `w` bits starting at bit `lsb`.
#[inline(always)]
fn sbfx(x: i32, lsb: u32, w: u32) -> i32 {
    (x << (32 - lsb - w)) >> (32 - w)
}
/// UBFX.
#[inline(always)]
fn ubfx(x: i32, lsb: u32, w: u32) -> i32 {
    (((x as u32) << (32 - lsb - w)) >> (32 - w)) as i32
}
/// Left shift by `norm32(x)`, as the firmware's normalise idiom does. Returns (shifted, shift).
#[inline(always)]
fn norm(x: i32) -> (i32, i32) {
    let s = norm32(x);
    (lsl(x, s), s)
}
/// `x` shifted left by `e - 15` (right by `15 - e` when `e <= 14`): the firmware's way of turning
/// a (mantissa << 16, exponent) pair into a Q16 fixed-point value.
#[inline(always)]
fn scale16(x: i32, e: i32) -> i32 {
    if e > 14 { lsl(x, e - 15) } else { asr(x, 15 - e) }
}
/// 64-bit signed product shifted right by 15 and truncated to 32 bits (the SMULL / LSR #15 /
/// ORR LSL #17 idiom).
#[inline(always)]
fn mul_q15(a: i32, b: i32) -> i32 {
    (smull(a, b) >> 15) as i32
}

/// 0x0805df08: write `count` samples of the period table into the accumulator, `acc[*pos..]`,
/// with saturating adds. `phase` is the 32-bit read position in the table (table index in bits
/// 16..23, interpolation fraction in bits 1..15), advanced by `step << 5` per sample. The
/// interpolated sample is shifted left by `shift` (right when negative). When `dir` is not 0
/// the sample is also multiplied by a fade window `(1 - cos) / 2` looked up at `ramp` (bits
/// 6..21, a signed index into the cosine table with interpolation) and `ramp` moves by
/// `dir << 16` per sample.
fn osc_run(
    phase: &mut i32,
    table: &[i16],
    step: i32,
    mut ramp: i32,
    acc: &mut [i32],
    pos: &mut usize,
    shift: i32,
    dir: i32,
    count: i32,
) {
    let (shift, dir, count) = (sext(shift), sext(dir), sext(count));
    if count <= 0 {
        return;
    }
    let inc = step << 5;
    for _ in 0..count {
        let p = *phase;
        let i = ubfx(p, 16, 8) as usize;
        let t0 = table[i] as i32;
        let t1 = table[i + 1] as i32;
        let a = t0 << 15;
        let d = ((t1 << 15).wrapping_sub(a) as u32 >> 16) as i32;
        let frac = ubfx(p, 1, 15);
        *phase = p.wrapping_add(inc);
        let w8 = sbfx(ramp, 6, 16) << 16;
        let mut s = a.wrapping_add(smulbb(frac, d) << 1);
        if dir != 0 {
            let x = w8 >> 6;
            let (idx, f) = if x < 0 {
                let m = x.wrapping_neg();
                ((m >> 16) as usize, m & 0xffff)
            } else {
                ((w8 >> 22) as usize, x & 0xffff)
            };
            let c0 = COS512[idx] as i32;
            let c1 = COS512[idx + 1] as i32;
            let v = ((c0 << 16).wrapping_sub(f.wrapping_mul(c0)).wrapping_add(f.wrapping_mul(c1))) >> 16;
            let w = sbfx(0x8000 - v, 1, 16);
            s = mul_q15(w, s);
            ramp = ramp.wrapping_add(dir << 16);
        }
        s = if shift >= 0 { lsl(s, shift) } else { asr(s, -shift) };
        acc[*pos] = qadd(s, acc[*pos]);
        *pos += 1;
    }
}

/// 0x0805e19c: lay one copy of the period table into the accumulator. The copy starts at
/// sample `m * 2^e` (a pseudo-float position, may be negative), advances the table by
/// `step` (a 16-bit value, 32 phase units per unit) per sample, and covers `0x10 + ...` samples
/// with a 16 sample fade in, a flat part and a 16 sample fade out (three calls of [`osc_run`]);
/// the length comes from `m * 2^e + 1/step` (unclear what that physically is). `tab_exp` is
/// the table's exponent and becomes the output shift (`tab_exp - 0x10`). Nothing is written
/// when the copy would end before the start of the accumulator.
pub fn overlay_period(acc: &mut [i32], m: i32, e: i32, step: i32, table: &[i16], tab_exp: i32) {
    let start_q16 = scale16(m << 16, e); // r8 (iVar10)
    let step_hi = step << 16; // r6
    let (step_n, s) = norm(step_hi);
    let (q, qe) = div_norm(0x40000000, 1, step_n >> 16, -4 - s);
    let (sm, se) = float_add(m, e, q, qe);
    let r4 = qadd(start_q16, 0xffff) >> 16; // iVar6: start rounded up to a sample
    let r10 = r4 << 16;
    let r3 = scale16(sm << 16, se); // iVar2: end position, Q16
    let r2 = r3 >> 16;
    let mut r5 = r2.min(0xa6); // iVar7: last sample index, capped
    let r11;
    if r5 > 0x96 {
        r11 = 0xa6;
    } else {
        r11 = r5 + 0x10;
        if r11 <= 0 {
            return;
        }
    }
    let r8 = ubfx(r10.wrapping_sub(start_q16), 1, 16);
    let mut r12 = sbfx(r3.wrapping_sub(r2 << 16), 0, 17);
    let g = ubfx(step_hi, 16, 16); // step as an unsigned halfword
    let mut phase = sbfx(smulbb(g, r8), 10, 21);
    let mut ramp = (r8 << 16) >> 15;
    r12 &= !1;
    r12 = r12.wrapping_add(0xf0000);
    let mut pos: usize;
    let mut r7: i32;
    if r4 < 0 {
        phase = phase.wrapping_add(smulbb(-r4, g) << 5);
        if r4 < -0x10 {
            ramp = ramp.wrapping_add(0x100000);
        } else {
            ramp = ramp.wrapping_sub(r10);
        }
        if r5 < 0 {
            r12 = r12.wrapping_add((r5 + 1) << 16);
        }
        pos = 0;
        r7 = 0;
    } else {
        pos = r4 as usize;
        r7 = r4;
    }
    let end_fade_in = r4 + 0x10;
    let n1 = end_fade_in - r7;
    let shift = sext(tab_exp - 0x10);
    let step = sext(g);
    if n1 > 0 {
        osc_run(&mut phase, table, step, ramp, acc, &mut pos, shift, 1, n1);
        r7 = end_fade_in;
    }
    r5 = r5 - r7 + 1;
    if r5 > 0 {
        osc_run(&mut phase, table, step, 0, acc, &mut pos, shift, 0, r5);
        r7 += r5;
    }
    osc_run(&mut phase, table, step, r12, acc, &mut pos, shift, -1, r11 - r7 + 1);
}

/// 0x0805e354: lay down the series of period copies that start inside this frame, `cnt` of them.
/// Copy `k` (1-based) starts at `k - loc/65536` periods; its position and length are computed
/// with pseudo-float arithmetic from `loc` (`local_308`), `diff` (difference of the two
/// pitch values being interpolated), `pitch` (`local_2f8`), `n` (samples per call) and an
/// integer square root (unclear what quantity this is; it looks like the sample position of the
/// k-th period under a linearly changing pitch). Each copy goes through [`overlay_period`] with
/// the gain `c`. Returns the mantissa and exponent computed for the last copy (the firmware
/// stores them through two pointers). `cnt` must be positive and at most 20 (the firmware's
/// local arrays hold 20; larger counts would corrupt its stack, here they are truncated).
pub fn overlay_series(
    acc: &mut [i32],
    c: i32,
    table: &[i16],
    tab_exp: i32,
    loc: i32,
    diff: i32,
    pitch: i32,
    n: i32,
    cnt: i32,
) -> (i16, i16) {
    let diff = sext(diff);
    let p9 = sext(pitch);
    let mut mant = [0i16; 20];
    let mut exps = [0i16; 20];
    if cnt <= 0 {
        // The firmware would hand back whatever its uninitialised arrays hold; never called so.
        return (0, 0);
    }
    let cnt = cnt.min(20);
    {
        let (pp, sp) = norm(p9.wrapping_mul(p9) << 1);
        let (nn, sn) = norm(n.wrapping_mul(n) << 1);
        let q = smultt(pp, nn) << 1;
        let sa = norm32(q);
        let pn = p9.wrapping_mul(n) << 1;
        let sb = norm32(pn);
        let e0 = 0x16 - sp - sn - sa;
        let n_hi = n << 16;
        for k in 1..=cnt {
            let (r7, s6) = norm((k << 16).wrapping_sub(loc));
            let r7 = (r7 >> 16).wrapping_mul(n) << 1;
            let (r7, s3) = norm(r7);
            let r7 = ((r7 as u32) >> 16) as i32;
            let r2 = smulbb(r7, diff) << 1;
            let e1 = 0x1f - s6 - s3;
            let (r2n, sc) = norm(r2);
            let (sm, se) = float_add(lsl(q, sa) >> 16, e0, r2n >> 16, (e1 - 4) - sc);
            let mut ex = se;
            let rt = sqrt_norm(sm << 16, &mut ex);
            let (rtn, sd) = norm(rt);
            let (sm2, se2) = float_add(lsl(pn, sb) >> 16, 11 - sb, rtn >> 16, ex - sd);
            let (dm, de) = div_norm(r7 << 16, e1, sext(sm2), se2);
            let v = scale16((dm & 0xffff) << 16, de);
            let i = (k - 1) as usize;
            if v.wrapping_sub(n_hi) >= 0 {
                // The computed value is at least n: use n itself.
                let (nh, s) = norm(n_hi);
                mant[i] = ((nh as u32) >> 16) as i16;
                exps[i] = (15 - s) as i16;
            } else {
                mant[i] = dm as i16;
                exps[i] = de as i16;
            }
        }
        for i in 0..cnt as usize {
            overlay_period(acc, mant[i] as i32, exps[i] as i32, c, table, tab_exp);
        }
    }
    let last = (cnt - 1) as usize;
    (mant[last], exps[last])
}

/// 0x0805e010: build the period table. For each harmonic `k` in `start..end` whose voicing
/// entry equals `want`, put the complex bin `amp[k] * (cos a, sin a)` at bin `k + 1` of `buf`,
/// where the angle `a` comes from `(k + 1) * scale + phases[k]` (a binary angle, 9 bits of the
/// cosine table) and the amplitudes are scaled up by the headroom of the largest one. The
/// rest of `buf` is zero. If any harmonic matched, `buf` is inverse-transformed (256 point real
/// IFFT, [`fft_inverse`]) and `*exp` becomes the transform's exponent; otherwise `*exp` is
/// -32. `buf[0x100]` is always a copy of `buf[0]` so the table can be interpolated across the
/// wrap. Returns the number of harmonics placed. `p7` is the block's halfword 0x42 (an
/// exponent of the amplitudes).
pub fn build_period(
    buf: &mut [i16],
    exp: &mut i16,
    scale: i32,
    phases: &[i16],
    voicing: &[i16],
    amps: &[i16],
    p7: i32,
    end: i32,
    start: i32,
    want: i32,
) -> i32 {
    let mut count = 0;
    if start < end {
        let mut max = 0i32;
        for k in start..end {
            if voicing[k as usize] as i32 == want {
                max = max.max((amps[k as usize] as i32) << 16);
            }
        }
        buf[..0x100].fill(0);
        let sh = norm32(max);
        for k in start..end {
            let ku = k as usize;
            if voicing[ku] as i32 != want {
                continue;
            }
            let k1 = k + 1;
            let x = (k1.wrapping_mul(scale).wrapping_add(phases[ku] as i32) << 10).wrapping_add(0x8000);
            let y = (x & i32::MIN).wrapping_add(((x << 7) as u32 >> 1) as i32);
            let z = y >> 6;
            let idx = if z < 0 {
                let m = z.wrapping_neg();
                ((((m as u32 >> 16) << 16) as i32).wrapping_neg() >> 16) & 0x1ff
            } else {
                ((z as u32 >> 16) & 0x1ff) as i32
            } as usize;
            let a = amps[ku] as i32;
            let re = smulbb(a, COS512[idx] as i32) << 1;
            let im = smulbb(a, COS512[(idx + 0x180) & 0x1ff] as i32) << 1;
            buf[2 * k1 as usize] = (lsl(re, sh) >> 16) as i16;
            buf[2 * k1 as usize + 1] = (lsl(im, sh) >> 16) as i16;
            count += 1;
        }
        *exp = (p7 + 0x17 - sh) as i16;
        if count != 0 {
            *exp = fft_inverse(&mut buf[..0x100], sext(*exp as i32), 8, false) as i16;
        }
    } else {
        buf[..0x100].fill(0);
    }
    if count == 0 {
        *exp = -0x20;
    }
    buf[0x100] = buf[0];
    count
}

/// Words 0..1 of a parameter block as a 32-bit value (bytes 8..11: bit mask of voiced bands).
fn voiced_mask(blk: &[i16]) -> bool {
    let w = (blk[4] as u16 as u32) | ((blk[5] as u16 as u32) << 16);
    w & 0x5555_5555 != 0
}

/// Renormalise the way the firmware does after summing the position: shift left by the
/// normalisation count and then back right (the identity unless the shifts differ).
fn renorm(t: i32) -> i32 {
    let (r1, s) = norm(t);
    scale16(r1, 15 - s)
}

/// 0x0805e584: synthesise the voiced part of one half frame. `acc[..n]` receives (saturating
/// adds) `n` samples; `st` is the decoder state (the routine's own state starts at state+0x18);
/// `cur` and `prev` are the 0x44 halfword parameter blocks of the current and previous frame
/// (copies; the routine only reads them), `cur_voicing` the current block's per-harmonic voicing
/// array (the array its halfwords 0x40-0x41 point to; the previous frame's array is never read)
/// and `val` the 16-bit value from state+0x7c2 that the caller passes as the last argument
/// (it scales the pitch used for the period count; unclear).
///
/// Parameter block fields used: halfword 2 = harmonic count, 4..5 = voicing bit mask (any bit of
/// 0x55555555 means the frame has voiced bands), 6 = pitch, 7 = pitch fraction, 8.. = amplitudes,
/// 0x42 = amplitude exponent.
pub fn synth_voiced(
    acc: &mut [i32],
    n: i32,
    st: &mut State,
    cur: &[i16],
    cur_voicing: &[i16],
    prev: &[i16],
    val: i16,
) {
    let cur_voiced = voiced_mask(cur);
    let prev_voiced = voiced_mask(prev);
    let n_hi = n << 16;
    // 1/n as a pseudo-float: step of the cross-fade ramps.
    let (n_norm, sn) = norm(n_hi);
    let (recip_m, recip_e) = div_norm(0x40000000, 1, n_norm >> 16, 15 - sn);
    let recip_m = recip_m & 0xffff;
    let recip_e = sext(recip_e);
    let mut m_mant = st.h(off::MANT) as i32; // local_2c
    let mut m_exp = st.h(off::EXP) as i32; // local_2a
    let mut loc = st.w(off::POS); // local_308
    let prev_pitch = prev[6] as u16 as i32;
    let cur_pitch = cur[6] as u16 as i32;

    // local_30c / local_310: the position sum and its integer part (count of period starts in
    // this call); diff (local_2e4), f60 (local_2e0), f64 (local_2dc): pitch-jump handling flags.
    let mut v30c = 0i32;
    let mut diff = 0i32;
    let mut f60 = 0i32;
    let mut f64 = 0i32;
    let mut pitch = prev_pitch; // local_2f8
    if !prev_voiced {
        if cur_voiced {
            pitch = cur_pitch;
            v30c = renorm(loc.wrapping_add(sbfx(sext(pitch).wrapping_mul(n), 3, 28)));
        }
    } else if !cur_voiced {
        v30c = renorm(loc.wrapping_add(sbfx(sext(prev_pitch).wrapping_mul(n), 3, 28)));
    } else {
        let p = sext(prev_pitch);
        let mut c = sext(cur_pitch);
        let lim = smulbb(cur_pitch, 0x4ccc);
        if p < lim >> 15 && p > sbfx(c * 13107, 15, 16) && p < 0x4000 {
            c >>= 1;
            f64 = 1;
        } else if c > smulbb(prev_pitch, 0x3555) >> 15 && c < (p + (p << 2)) >> 3 && c < 0x4000 {
            c = sext(c << 1);
            f60 = 1;
        }
        let mid = ((p << 16) >> 1).wrapping_add((c << 16) >> 1) >> 16;
        diff = ((c << 16).wrapping_sub(p << 16) as u32 >> 16) as i32;
        v30c = renorm(loc.wrapping_add(sbfx(mid.wrapping_mul(n), 3, 28)));
    }
    let mut v310 = v30c >> 16;

    let mut tmp = [0i32; 2 * TAIL_WORDS];
    for (i, t) in tmp[..TAIL_WORDS].iter_mut().enumerate() {
        *t = st.w(off::TAIL + 4 * i);
    }
    let tab_exp = |st: &State| st.h(off::TAB_EXP) as i32;

    // Fade the old voiced signal out: the series of periods that continue from the last call.
    if prev_voiced && v310 > 0 {
        let (m, e) = overlay_series(
            &mut tmp,
            sext(prev[6] as i32),
            st.s(off::TAB, TAB_LEN),
            tab_exp(st),
            loc,
            sext(diff),
            sext(pitch),
            n,
            v310,
        );
        m_mant = m as i32;
        m_exp = e as i32;
    }
    let ramp_step = if recip_e >= 0 { lsl(recip_m << 16, recip_e) } else { asr(recip_m << 16, -recip_e) };
    if sext(n) > 0 {
        let mut ramp = 0i32;
        for i in 0..sext(n) as usize {
            let t = tmp[i];
            let r = sext(ramp >> 16);
            let v = t.wrapping_sub(mul_q15(r, t));
            acc[i] = qadd(v, acc[i]);
            ramp = qadd(ramp, ramp_step);
        }
    }

    // New signal, faded in.
    tmp.fill(0);
    let c0 = sext(cur[6] as i32);
    let (c0n, s0) = norm(c0 << 16);
    let (vn, s6) = norm((val as i32) << 16);
    let (m, e) = div_norm(vn, -4 - s6, c0n >> 16, -4 - s0);
    let start = (scale16(m << 16, e).wrapping_add(0x8000)) >> 16;
    let scale = sext(-c0);
    let end = sext(cur[2] as i32);
    let p7 = cur[0x42] as i32;
    let mut buf = [0i16; TAB_LEN];
    let mut texp = tab_exp(st) as i16;
    // The phase array and the table live in the state; work on copies and write the table back.
    let mut phases = [0i16; PHASE_LEN];
    phases.copy_from_slice(st.s(off::PHASES, PHASE_LEN));
    let count = build_period(
        &mut buf,
        &mut texp,
        scale,
        &phases,
        cur_voicing,
        &cur[8..],
        p7,
        end,
        start,
        2,
    );
    st.s_mut(off::TAB, TAB_LEN).copy_from_slice(&buf);
    st.set_h(off::TAB_EXP, texp);
    if count != 0 {
        let h7 = sext(cur[7] as i32);
        let v = n_hi.wrapping_add(h7 << 8).wrapping_sub(0x100000);
        let (vs, s) = norm(v);
        overlay_period(&mut tmp, vs >> 16, 15 - s, c0, st.s(off::TAB, TAB_LEN), tab_exp(st));
    }
    if cur_voiced {
        if f64 == 0 {
            if f60 != 0 {
                let w = st.w(off::POS);
                loc = w >> 1;
                v30c = loc.wrapping_add(v30c.wrapping_sub(w) >> 1);
                st.set_w(off::POS, loc);
                diff = sbfx(diff, 1, 15) & 0xffff;
                pitch = sbfx(pitch, 1, 15) & 0xffff;
                v310 = v30c >> 16;
            }
        } else {
            let w = st.w(off::POS);
            loc = w << 1;
            let d = v30c.wrapping_sub(w);
            if loc >= 0x10000 {
                loc -= 0x10000;
                let r1 = if loc == i32::MIN { 0x3fffffff } else { loc.wrapping_neg() >> 1 };
                let (den, s0) = norm(prev_pitch << 16);
                let (num, sr) = norm(r1);
                let (m, e) = div_norm(num, 15 - sr, den >> 16, -4 - s0);
                st.set_h(off::MANT, m as i16);
                st.set_w(off::EXP, e);
            }
            let p = sext(pitch);
            let r1 = ((diff << 17) as u32 >> 16) as i32;
            st.set_w(off::POS, loc);
            let ne = (p << 16) != ((p << 17) >> 1);
            let mut r3 = p << 17;
            if ne {
                r3 = 0x7fffffff ^ ((p << 16) >> 31);
            }
            diff = r1;
            v30c = loc.wrapping_add(d << 1);
            pitch = ((r3 as u32) >> 16) as i32;
            v310 = v30c >> 16;
        }
        build_period(
            &mut buf,
            &mut texp,
            scale,
            &phases,
            cur_voicing,
            &cur[8..],
            p7,
            end,
            0,
            1,
        );
        st.s_mut(off::TAB, TAB_LEN).copy_from_slice(&buf);
        st.set_h(off::TAB_EXP, texp);
        overlay_period(
            &mut tmp,
            st.h(off::MANT) as i32,
            st.h(off::EXP) as i32,
            c0,
            st.s(off::TAB, TAB_LEN),
            tab_exp(st),
        );
        if v310 > 0 {
            let (m, e) = overlay_series(
                &mut tmp,
                c0,
                st.s(off::TAB, TAB_LEN),
                tab_exp(st),
                loc,
                sext(diff),
                sext(pitch),
                n,
                v310,
            );
            m_mant = m as i32;
            m_exp = e as i32;
        }
    }
    st.set_w(off::POS, v30c.wrapping_sub(v310 << 16));
    if sext(n) > 0 {
        let mut ramp = 0i32;
        for i in 0..sext(n) as usize {
            acc[i] = qadd(acc[i], mul_q15(sext(ramp >> 16), tmp[i]));
            ramp = qadd(ramp, ramp_step);
        }
    }
    for i in 0..TAIL_WORDS {
        st.set_w(off::TAIL + 4 * i, tmp[n as usize + i]);
    }
    if prev_voiced || cur_voiced {
        let (nm, sn2) = norm(n_hi);
        let (m, e) = float_sub(nm >> 16, 15 - sn2, m_mant, m_exp);
        st.set_h(off::MANT, m as i16);
        st.set_w(off::EXP, e);
    } else {
        st.set_h(off::MANT, 0);
        st.set_w(off::EXP, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ambe::testvec::{self, Record, assert_h_eq, assert_state_eq};

    /// Replay every recorded call of `addr`; returns how many there were (0 when the
    /// recordings are not installed and the test is skipped).
    fn each(addr: u32, f: impl Fn(&Record)) -> usize {
        let Some(recs) = testvec::load(addr) else { return 0 };
        assert!(!recs.is_empty(), "no recorded calls at {addr:#x}");
        for r in &recs {
            f(r);
        }
        eprintln!("{addr:#x}: {} calls replayed", recs.len());
        recs.len()
    }

    #[test]
    fn osc_run_matches_firmware() {
        each(0x0805_df08, |r| {
            let phase_ptr = r.r[0];
            let slot = r.stack[0];
            let out = r.w_before(slot, 1)[0] as u32;
            let n = sext(r.stack[3] as i32).max(0) as usize;
            let table = r.h_before(r.r[1], TAB_LEN);
            let mut acc = r.w_before(out, n);
            let mut phase = r.w_before(phase_ptr, 1)[0];
            let mut pos = 0;
            osc_run(
                &mut phase,
                &table,
                r.r[2] as i32,
                r.r[3] as i32,
                &mut acc,
                &mut pos,
                r.stack[1] as i32,
                r.stack[2] as i32,
                r.stack[3] as i32,
            );
            assert_eq!(phase, r.w_after(phase_ptr, 1)[0], "phase: frame {} call {}", r.frame, r.call);
            assert_eq!(acc, r.w_after(out, n), "acc: frame {} call {}", r.frame, r.call);
            assert_eq!(out + 4 * pos as u32, r.w_after(slot, 1)[0] as u32, "out pointer");
        });
    }

    #[test]
    fn overlay_period_matches_firmware() {
        each(0x0805_e19c, |r| {
            let table = r.h_before(r.stack[0], TAB_LEN);
            let mut acc = r.w_before(r.r[0], 2 * TAIL_WORDS);
            overlay_period(&mut acc, r.r[1] as i32, r.r[2] as i32, r.r[3] as i32, &table, r.stack[1] as i32);
            assert_eq!(acc, r.w_after(r.r[0], 2 * TAIL_WORDS), "acc: frame {} call {}", r.frame, r.call);
        });
    }

    #[test]
    fn overlay_series_matches_firmware() {
        each(0x0805_e354, |r| {
            let table = r.h_before(r.stack[0], TAB_LEN);
            let mut acc = r.w_before(r.r[0], 2 * TAIL_WORDS);
            let (m, e) = overlay_series(
                &mut acc,
                r.r[3] as i32,
                &table,
                r.stack[1] as i32,
                r.stack[2] as i32,
                r.stack[3] as i32,
                r.stack[4] as i32,
                r.stack[5] as i32,
                r.stack[6] as i32,
            );
            assert_eq!(acc, r.w_after(r.r[0], 2 * TAIL_WORDS), "acc: frame {} call {}", r.frame, r.call);
            assert_eq!(m, r.h_after(r.r[1], 1)[0], "mantissa: frame {} call {}", r.frame, r.call);
            assert_eq!(e, r.h_after(r.r[2], 1)[0], "exponent: frame {} call {}", r.frame, r.call);
        });
    }

    #[test]
    fn build_period_matches_firmware() {
        each(0x0805_e010, |r| {
            let (end, start) = (r.stack[3] as i32, r.stack[4] as i32);
            let n = end.max(0) as usize;
            let voicing = r.h_before(r.stack[0], n);
            let amps = r.h_before(r.stack[1], n);
            let phases = r.h_before(r.r[3], n);
            let mut buf = r.h_before(r.r[0], TAB_LEN);
            let mut exp = r.h_before(r.r[1], 1)[0];
            let ret = build_period(
                &mut buf,
                &mut exp,
                r.r[2] as i32,
                &phases,
                &voicing,
                &amps,
                r.stack[2] as i32,
                end,
                start,
                r.stack[5] as i32,
            );
            assert_eq!(ret, r.ret[0] as i32, "count: frame {} call {}", r.frame, r.call);
            assert_h_eq("table", r, &buf, &r.h_after(r.r[0], TAB_LEN));
            assert_eq!(exp, r.h_after(r.r[1], 1)[0], "exponent: frame {} call {}", r.frame, r.call);
        });
    }

    #[test]
    fn synth_voiced_matches_firmware() {
        each(0x0805_e584, |r| {
            let n = r.r[1] as i32;
            assert_eq!(r.state_off(r.r[2]), 0x18);
            let cur = r.h_before(r.r[3], 0x44);
            let prev = r.h_before(r.stack[0], 0x44);
            let vptr = (cur[0x40] as u16 as u32) | ((cur[0x41] as u16 as u32) << 16);
            let voicing = r.h_before(vptr, cur[2].max(0) as usize);
            let mut acc = r.w_before(r.r[0], n as usize);
            let mut st = r.state_before();
            synth_voiced(&mut acc, n, &mut st, &cur, &voicing, &prev, r.stack[1] as i16);
            assert_eq!(acc, r.w_after(r.r[0], n as usize), "acc: frame {} call {}", r.frame, r.call);
            let ignore = [(0x3ec + 0x80, 4), (0x474 + 0x80, 4), (0x4fc + 0x80, 4)];
            assert_state_eq(r, &st, &r.state_after(), &ignore);
        });
    }
}
