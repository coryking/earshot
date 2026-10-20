//! Spectral-magnitude dequantisation of the AMBE+2 decoder.
//!
//! Turns the frame's gain index, the two PRBA vectors and the four HOC vectors into the log2
//! magnitudes of the L harmonics (halfword Q-format values, clamped to about +-0x77ff), using
//! the previous frame's magnitudes for prediction, then smooths them against the previous frame
//! and keeps the "previous frame" parameter block up to date.
//!
//! # Parameter blocks
//!
//! The decoder passes frame parameters around in blocks of halfwords (`cur`, `out` below are
//! slices of such blocks; offsets in the comments are firmware byte offsets into the block):
//!
//! | offset | meaning |
//! |---|---|
//! | 0x00 | frame type: 0 silence/bad, 1 speech, 2 repeat (erasure), 3 tone |
//! | 0x02 | 0xff when the frame carries no extra info (see `blend_frames`) |
//! | 0x04 | harmonic count L (9..56) |
//! | 0x06 | copied through the frame history, meaning unclear |
//! | 0x08 | 32-bit per-band voicing flags (2 bits per band, patterns 0x5555.. / 0xaaaa..) |
//! | 0x0c | fundamental (inverse of the harmonic-count scale, see `math::harmonic_count`) |
//! | 0x0e | unclear (decremented by 0x5000 on repeated frames) |
//! | 0x10.. | 0x38 magnitudes; entries beyond L repeat the last one |
//!
//! The previous frame's block lives in the decoder state at [`PREV`] and has extra fields past
//! the 0x80-byte common part: 0x82 (running level used by `smooth_magnitudes`), 0x84 (gain
//! predictor), 0x86 (repeat counter), 0x88 (set to 0xfffd by the caller), 0x8a.
//!
//! The frame's bit-field widths live in the state at [`WIDTHS`] (written by 0x08055170).
//!
//! # Encoder paths
//!
//! The firmware routines are shared with an encoder: they take a "write" flag that selects
//! vector quantisation (0x08060e38, the forward cosine transform, and the nearest-codeword
//! searches in 0x08061074 and 0x08061340). The decoder always passes 0, so this port only has
//! the read path and 0x08060e38 is not ported (nothing in the decoder reaches it).

use super::math::{self, Bits};
use super::ops::*;
use super::state::State;
use super::tables::spectral as tab;

/// Byte offset in the state of the previous frame's parameter block (the `P` argument that
/// 0x0805cc40, 0x0805d11c, 0x0805d480 and 0x0805c778 receive from 0x08055484).
pub const PREV: usize = 0x584;
/// Byte offset in the state of the current frame's parameter block (the `cur` argument that
/// 0x0805cc40, 0x0805d11c, 0x0805d480 and 0x0805c778 receive from 0x08055484).
pub const CUR: usize = super::state::off::PARAMS_CUR;
/// Byte offset in the state of the bit-field width table (`widths` argument of 0x0805cc40).
pub const WIDTHS: usize = 0x7a4;

/// Halfwords of a parameter block that these routines touch (header 8 + 0x38 magnitudes).
pub const BLOCK_WORDS: usize = 0x40;

const MAGS: usize = 8; // first magnitude, in halfwords
const MAX_HARM: usize = 0x38;
const MAG_LIMIT: i32 = 0x77ff;

/// Saturating doubling: the compiler's `x << 1` with overflow check (`QADD x, x` equivalent).
#[inline(always)]
fn sat_dbl(x: i32) -> i32 {
    let d = x.wrapping_shl(1);
    if x != d >> 1 { i32::MAX ^ (x >> 31) } else { d }
}

/// 0x08061074 (decode path): read `want` bits (fewer if the frame only carries `avail` of them,
/// in which case the table rows are `stride << (want - avail)` apart) as a codebook index and
/// copy that row (`dst.len()` halfwords) out of `table`. Returns the index.
fn vq_read(dst: &mut [i16], avail: i32, table: &[i16], want: i32, stride: i32, bits: &mut Bits) -> i32 {
    let (mut want, mut stride) = (want, stride);
    if want > avail {
        stride = lsl(stride, want - avail);
        want = avail;
    }
    let idx = bits.read(want as usize);
    let base = stride.wrapping_mul(idx) as usize;
    dst.copy_from_slice(&table[base..base + dst.len()]);
    idx
}

/// 0x08061340 (decode path): read the gain index (`width` = widths[6] bits) and turn it into
/// the frame gain, predicted from the previous frame's gain `prev` (state PREV+0x84).
/// `ftype` is the frame type (block offset 0). Returns the gain (Q12-ish, at most 0x6800) and,
/// except for repeat frames (type 2), stores it as the new `prev`.
fn gain_dequant(prev: &mut i16, ftype: i32, width: i32, bits: &mut Bits) -> i16 {
    let mut g = [0i16; 1];
    vq_read(&mut g, width, &tab::GAIN, width, 1, bits);
    let mut g = g[0] as i32;
    let half_prev = (*prev as i32) << 15;
    if ftype == 2 {
        g -= 0x1000;
        let v = (half_prev.wrapping_add(g << 16) >> 16).min(0x6800);
        return v as i16;
    }
    let v = (half_prev.wrapping_add(g << 16) >> 16).min(0x6800);
    *prev = v as i16;
    v as i16
}

/// 0x080612ac: in-place `n`-point inverse cosine transform of the first `m` (at most `n`)
/// coefficients of `buf` (coefficients above the first are doubled). Cosines come from a
/// 512-entry table indexed by a 16-bit phase.
fn inverse_dct(buf: &mut [i16], n: usize, m: usize) {
    let m = m.min(n);
    let mut c = [0i16; 8];
    c[0] = buf[0];
    for j in 1..m {
        c[j] = lsl(buf[j] as i32, 1) as i16;
    }
    let g = tab::DCT_STEP[n] as i32;
    let mut step = g >> 1;
    for out in buf.iter_mut().take(n) {
        let mut phase = 0x40i32;
        let mut acc = 0i32;
        for &x in &c[..m] {
            let cos = tab::COS[((phase >> 7) & 0x1ff) as usize] as i32;
            acc = acc.wrapping_add(smulbb(cos, x as i32).wrapping_mul(2));
            phase = phase.wrapping_add(step);
        }
        *out = (acc.wrapping_add(0x8000) >> 16) as i16;
        step = (step + g) as i16 as i32;
    }
}

/// 0x0805c7f8 (decode path): the magnitudes of the current frame.
///
/// 1. Predict: resample the previous frame's L' magnitudes (`st` PREV+0x10) onto L harmonics by
///    linear interpolation, remove their mean, scale by 0.65 (0x5333/0x8000).
/// 2. Read the PRBA vectors (9 bits -> 3 terms, 7 bits -> 4 terms) and four HOC vectors
///    (5/4/4/3 bits, up to 4 terms each; fewer bits are read when the frame carries fewer,
///    widths[9..13]); block lengths come from `tab::CNT[L - 9]`.
/// 3. Inverse-DCT the PRBA terms, then each block of the spectrum from its PRBA pair and HOC
///    terms (6 coefficients), add the gain-derived offset, and add the prediction. The result
///    is clamped to -0x7800..0x77ff and the unused tail (L..0x38) repeats the last magnitude.
///
/// `gain` is the gain from [`gain_dequant`]; `cur` is the current block (L at 0x04 must be set;
/// magnitudes are written at 0x10..).
fn dequant_magnitudes(st: &State, cur: &mut [i16], gain: i32, bits: &mut Bits) {
    let p = |off: usize| st.h(PREV + off) as i32;
    let width = |off: usize| st.h(WIDTHS + off) as i32;
    let l = cur[2] as i32;
    let nl = l as usize;

    // Prediction from the previous frame.
    let step = {
        let q = math::div_half(0x80000, l);
        let x = smultb(p(4).wrapping_shl(25), q);
        x.wrapping_shl(1) >> 12 // sbfx #11, #20
    };
    let mut pred = [0i16; MAX_HARM];
    let mut sum = 0i32;
    let mut phase = step;
    for out in pred.iter_mut().take(nl) {
        let idx = (phase >> 16).min(0x37);
        let frac = (phase >> 1) & 0x7fff;
        let (v, acc);
        if idx == 0 {
            let a = p(0x10);
            v = a as i16;
            acc = a << 16;
        } else {
            let a = p(2 * (idx as usize + 7));
            let b = p(2 * (idx as usize + 8));
            let t = frac.wrapping_mul(a).wrapping_mul(2);
            let t = if t != i32::MIN { -t } else { i32::MAX };
            let s = t.wrapping_add(a << 16).wrapping_add(frac.wrapping_mul(b).wrapping_mul(2));
            v = (s >> 16) as i16;
            acc = s;
        }
        *out = v;
        sum = sum.wrapping_add(acc >> 6);
        phase = phase.wrapping_add(step);
    }
    let mean = math::div_half(sum, (l << 25) >> 16);
    let t = smulbb(mean, 0x5333).wrapping_mul(2);
    let off = if t == i32::MIN { 0x3fff_ffff } else { -t >> 1 };
    for v in pred.iter_mut().take(nl) {
        let x = (*v as i32).wrapping_mul(0x5333).wrapping_add(off);
        *v = (sat_dbl(x) >> 16) as i16;
    }

    // Block lengths.
    let packed = tab::CNT[nl - 9] as i32;
    let cnt: [usize; 4] = std::array::from_fn(|k| (((packed >> (4 * k)) & 0xf) + 2) as usize);

    // PRBA vectors: r[0] is the (unsent) DC term.
    let mut r = [0i16; 8];
    vq_read(&mut r[1..4], width(0xe), &tab::PRBA24, 9, 3, bits);
    vq_read(&mut r[4..8], width(0x10), &tab::PRBA58, 7, 4, bits);

    // HOC vectors go to positions 2.. of each block of the magnitude array.
    let hoc: [(&[i16], i32); 4] = [(&tab::HOC0, 5), (&tab::HOC1, 4), (&tab::HOC2, 4), (&tab::HOC3, 3)];
    let mut start = MAGS;
    for k in 0..4 {
        let avail = width(0x12 + 2 * k);
        if cnt[k] > 2 {
            let n = (cnt[k] - 2).min(4);
            vq_read(&mut cur[start + 2..start + 2 + n], avail, hoc[k].0, hoc[k].1, 4, bits);
        } else {
            bits.read(avail as usize);
        }
        start += cnt[k];
    }

    // PRBA inverse transform, then the sum/difference butterflies of the first four pairs.
    inverse_dct(&mut r, 8, 8);
    for k in 0..4 {
        let a = (r[2 * k] as i32) << 15;
        let b = (r[2 * k + 1] as i32) << 15;
        let d = smultb(a - b, 0x5a82);
        r[2 * k] = ((a + b) >> 16) as i16;
        r[2 * k + 1] = (d >> 15) as i16;
    }
    let mut start = MAGS;
    for k in 0..4 {
        cur[start] = r[2 * k];
        cur[start + 1] = r[2 * k + 1];
        inverse_dct(&mut cur[start..start + cnt[k]], cnt[k], 6);
        start += cnt[k];
    }

    // Gain offset: the spectrum's level is fixed by the frame gain, the shape by the vectors.
    let lg = math::log2_scaled(l, 15, 5);
    let mut acc = smulbb(cur[2] as i32, gain - lg).wrapping_mul(2);
    for k in 0..4 {
        acc = acc.wrapping_sub((cnt[k] as i32).wrapping_mul(r[2 * k] as i32).wrapping_mul(2));
    }
    let off = (math::div_half(acc, l) << 16) >> 1;
    for v in cur[MAGS..MAGS + nl].iter_mut() {
        let x = off.wrapping_add((*v as i32) << 15);
        *v = (sat_dbl(x) >> 16) as i16;
    }

    // Add the prediction; clamp.
    let mut last = 0;
    for (v, &pr) in cur[MAGS..MAGS + nl].iter_mut().zip(&pred) {
        let x = ((pr as i32) << 15).wrapping_add((*v as i32) << 15);
        last = (sat_dbl(x) >> 16).clamp(-0x7800, MAG_LIMIT) as i16;
        *v = last;
    }
    cur[MAGS + nl..MAGS + MAX_HARM].fill(last);
}

/// Copy of the current frame's block (state CUR), for the routines that need the state and the
/// block at the same time.
fn load_cur(st: &State) -> [i16; BLOCK_WORDS] {
    st.s(CUR, BLOCK_WORDS).try_into().unwrap()
}

/// 0x0805cc40: dequantise the spectral magnitudes of a frame.
///
/// Reads the gain index and the PRBA/HOC vectors from `bits` (the frame's bit cursor, positioned
/// after the fundamental and voicing fields) with the widths from the state (WIDTHS), and writes
/// the magnitudes into the current block (state CUR, offset 0x10..), using the previous frame's
/// block ([`PREV`]) for prediction. Updates the gain predictor (PREV+0x84). The current block
/// must already hold the frame type (0x00) and the harmonic count (0x04). The firmware's
/// arguments are `(PREV, CUR, WIDTHS, bits, 0)`; the trailing 0 selects decoding.
pub fn dequant_spectrum(st: &mut State, bits: &mut Bits) {
    let mut cur = load_cur(st);
    let width = st.h(WIDTHS + 0xc) as i32;
    let mut prev_gain = st.h(PREV + 0x84);
    let gain = gain_dequant(&mut prev_gain, cur[0] as i32, width, bits);
    st.set_h(PREV + 0x84, prev_gain);
    dequant_magnitudes(st, &mut cur, gain as i32, bits);
    st.s_mut(CUR, BLOCK_WORDS).copy_from_slice(&cur);
}

/// 0x0805d11c: track the frame level and smooth the magnitudes against the previous frame.
///
/// Computes the frame's RMS level `rms` (sqrt of mean square of the L magnitudes, Q4) and
/// updates the running level at PREV+0x82: if `rms` is above `level - 0x800` it becomes
/// `0.96 * level + 0.02 * rms`, otherwise it drops by 0x14.
///
/// Then, only when `cfg_a > 3 || cfg_b > 1` (the state words at 0x648 and 0x63c, read by the
/// caller; both are 0 in the RAM snapshot and nothing in the decoder writes them), each
/// magnitude of the current block that exceeds the previous frame's magnitude at the same
/// frequency (previous block interpolated at index `(i+1) * f_cur / f_prev`) by more than 0x1000
/// is pulled back toward it, by an amount that grows with the excess (capped at 0x400/65536
/// of it per unit), the harmonic number, and the running level. Unclear what the two config
/// values select (`cfg_b` also enters the threshold).
pub fn smooth_magnitudes(st: &mut State, cfg_a: i32, cfg_b: i32) {
    let mut cur = load_cur(st);
    let l = cur[2] as i32;
    let nl = l as usize;
    let (em, ee) = math::energy(&cur[MAGS..MAGS + nl]);
    let ln = l << 16;
    let s = norm32(ln);
    let (qm, qe) = math::div_norm(em, ee + 8, lsl(ln, s) >> 16, 15 - s);
    let rms = math::sqrt_scaled(qm << 16, qe, 4) & 0xffff;

    let level = st.h(PREV + 0x82) as i32;
    let level_hi = level << 16;
    let probe = qsub(qsub(level_hi, 0x0800_0000), rms << 16);
    let new_level = if probe < 0 {
        (smulbb(level, 0x7d70).wrapping_mul(2).wrapping_add(smulbb(rms, 0x51eb) >> 4)) >> 16
    } else {
        level_hi.wrapping_sub(0x14_0000) >> 16
    };
    st.set_h(PREV + 0x82, new_level as i16);

    if cfg_a <= 3 && cfg_b <= 1 {
        return;
    }

    // Previous magnitudes shifted by one, so that index i interpolates between lm[i], lm[i+1].
    let mut lm = [0i16; 60];
    lm[0] = st.h(PREV + 0x10);
    for t in 0..MAX_HARM {
        lm[t + 1] = st.h(PREV + 0x10 + 2 * t);
    }
    let last = lm[MAX_HARM];
    lm[MAX_HARM + 1..].fill(last);

    let f_prev = (st.h(PREV + 0xc) as i32) << 16;
    let f_cur = (cur[6] as i32) << 16;
    let s_prev = norm32(f_prev);
    let s_cur = norm32(f_cur);
    let level = (st.h(PREV + 0x82) as i32) << 16;
    let cfg_term = (cfg_b.wrapping_shl(27)) >> 1;
    let limit = qadd(0x2c00_0000i32.wrapping_sub(cfg_term), level);
    let (ratio, ratio_exp) =
        math::div_norm(lsl(f_cur, s_cur), -4 - s_cur, lsl(f_prev, s_prev) >> 16, -4 - s_prev);
    let limit_hi = (limit >> 16) << 16;
    let f_cur_s = cur[6] as i32;

    for i in 0..nl {
        let pos = ratio.wrapping_mul(i as i32 + 1).wrapping_shl(1);
        let pos = if ratio_exp < 0 { asr(pos, -ratio_exp) } else { lsl(pos, ratio_exp) };
        // The firmware indexes its stack array with this; it is within 0..=58 for any frame
        // pair the decoder produces (the clamp only guards against reading outside lm).
        let idx = (pos >> 16).clamp(0, 58) as usize;
        let frac = (pos >> 1) & 0x7fff;
        let a = lm[idx] as i32;
        let b = lm[idx + 1] as i32;
        let mix = (0x7fff - frac).wrapping_mul(a).wrapping_add(b.wrapping_mul(frac));
        let pred = (mix.wrapping_shl(1) >> 16) << 16;
        let mag = cur[MAGS + i] as i32;
        let mag_hi = mag << 16;
        let margin = ((3 * (i as i32 + 1)).wrapping_shl(17) >> 16).wrapping_mul(f_cur_s);
        let margin = margin.wrapping_shl(8) >> 16;
        if qsub(qsub(mag_hi, pred), 0x1000_0000) <= 0 {
            continue;
        }
        let m = margin.min(0x1000);
        let room = qsub(mag_hi, limit_hi.wrapping_sub(m << 16));
        if room <= 0 {
            continue;
        }
        let w = (room >> 16).min(0x400);
        let d = qsub(pred, mag_hi) >> 16;
        let adj = sat_dbl(d.wrapping_mul(w));
        let adj = {
            let sh = adj.wrapping_shl(4);
            if adj != sh >> 4 { i32::MAX ^ (adj >> 31) } else { sh }
        };
        let v = qadd(adj, mag_hi) >> 16;
        cur[MAGS + i] = v.clamp(-MAG_LIMIT, MAG_LIMIT) as i16;
    }
    st.s_mut(CUR + 2 * MAGS, MAX_HARM).copy_from_slice(&cur[MAGS..MAGS + MAX_HARM]);
}

/// 0x0805d380: phase step (Q16: integer harmonic index in the high half, fraction in bits
/// 1..15) for resampling a magnitude array measured at fundamental `b` onto fundamental `a`;
/// roughly 0x10000 * a / b.
fn resample_step(a: i32, b: i32) -> i32 {
    if a == b {
        return 0x10000;
    }
    let b16 = b.wrapping_shl(16);
    let s = norm32(b16);
    let bn = lsl(b16, s) >> 16;
    let e = -4 - s; // exponent of bn
    let a16 = a.wrapping_shl(16);
    let (q, sh) = if a < bn {
        (math::div_half(a16, bn).wrapping_shl(16), -4 - e)
    } else {
        (math::div_half(a16 >> 1, bn).wrapping_shl(16), -3 - e)
    };
    if sh > 0xe { lsl(q, sh - 0xf) } else { asr(q, 15 - sh) }
}

/// 0x0805d3e8: resample the magnitudes `src` (0x38 of them, valid fundamental `b`) to the
/// fundamental `a`: `dst[i]` is the linear interpolation of the source at index
/// (i+1) * step, with the source padded by one repeated entry in front and repeated at the end.
fn resample_mags(dst: &mut [i16], a: i32, b: i32, src: &[i16]) {
    let mut t = [0i16; 60];
    t[0] = src[0];
    t[1..=MAX_HARM].copy_from_slice(&src[..MAX_HARM]);
    let last = src[MAX_HARM - 1];
    t[MAX_HARM + 1..].fill(last);
    let step = resample_step(a, b);
    let mut phase = step;
    for d in dst.iter_mut() {
        let frac = (phase >> 1) & 0x7fff;
        let w = 0x7fff - frac;
        let (i0, i1) = {
            let i = phase >> 16;
            if i > 0x3b {
                (0x3b, 0x3b)
            } else {
                let j = i + 1;
                (i, if j == 0x3c { 0x3b } else { j })
            }
        };
        let x = smulbb(t[i0.max(0) as usize] as i32, w).wrapping_mul(2);
        let y = smulbb(t[i1.max(0) as usize] as i32, frac);
        *d = (qdadd(x, y) >> 16) as i16;
        phase = phase.wrapping_add(step);
    }
}

/// Copy a whole parameter block header and magnitudes (0x08061440).
fn copy_block(dst: &mut [i16], src: &[i16]) {
    dst[..BLOCK_WORDS].copy_from_slice(&src[..BLOCK_WORDS]);
}

#[inline(always)]
fn word(b: &[i16], off: usize) -> i32 {
    (b[off / 2] as u16 as i32) | ((b[off / 2 + 1] as i32) << 16)
}
#[inline(always)]
fn set_word(b: &mut [i16], off: usize, v: i32) {
    b[off / 2] = v as i16;
    b[off / 2 + 1] = (v >> 16) as i16;
}

/// 0x0805d480: build the block `out` that gets synthesized, from the decoded current block
/// `cur` and the previous frame's block (state PREV).
///
/// * No extra info on either frame (offset 2 != 0xff): `out` is a copy of `cur`.
/// * Previous frame was a repeat (type 2): copy of `cur`, keeping `out`'s flags (0x08) and 0x0e;
///   magnitudes are halved unless `cur` is a repeat too.
/// * `cur` is a repeat: copy of the previous block (or of `cur` if the previous frame was a
///   tone or has been repeated already, PREV+0x86 >= 2), then fade: 0x0e drops by 0x5000
///   while positive, and once it is not positive the odd-bit voicing flags are masked off.
/// * Otherwise: both frames are speech. The new fundamental is `cur`'s, the previous frame's,
///   or the geometric mean of the two, depending on the voicing flags; L follows from it
///   (`harmonic_count`); both magnitude arrays are resampled to it and averaged.
pub fn blend_frames(out: &mut [i16], st: &State) {
    let cur = &load_cur(st);
    let prev = st.s(PREV, BLOCK_WORDS);
    let p = |off: usize| st.h(PREV + off) as i32;
    if cur[1] != 0xff || p(2) != 0xff {
        copy_block(out, cur);
        return;
    }
    if p(0) == 2 {
        let flags = word(out, 8);
        let h = out[7];
        copy_block(out, cur);
        set_word(out, 8, flags);
        out[7] = h;
        if cur[0] != 2 {
            for v in &mut out[MAGS..MAGS + MAX_HARM] {
                *v >>= 1;
            }
        }
    } else if cur[0] == 2 {
        if p(0x86) <= 1 && p(0) != 3 {
            copy_block(out, prev);
        } else {
            copy_block(out, cur);
        }
        if out[7] > 0 {
            out[7] = out[7].wrapping_sub(0x5000);
        } else {
            let f = word(out, 8) & 0x5555_5555;
            set_word(out, 8, f);
            out[7] = 0;
        }
    } else {
        let f = if (st.w(PREV + 8) & 0x5555_5555) == 0 || (word(out, 8) & 0xaaaa_aaaau32 as i32) != 0 {
            cur[6] as i32
        } else if (word(cur, 8) & 0x5555_5555) == 0 {
            p(0xc)
        } else {
            let prod = smulbb(cur[6] as i32, p(0xc)).wrapping_shl(1);
            math::sqrt_scaled(prod, -8, -4)
        };
        out[6] = f as i16;
        let l = math::harmonic_count(f as i16 as i32) as i16;
        out[1] = cur[1];
        out[2] = l;
        let nl = l as usize;
        let mut ra = [0i16; MAX_HARM];
        let mut rp = [0i16; MAX_HARM];
        resample_mags(&mut ra[..nl], out[6] as i32, cur[6] as i32, &cur[MAGS..]);
        resample_mags(&mut rp[..nl], out[6] as i32, p(0xc), &prev[MAGS..]);
        let mut v = 0i32;
        for k in 0..nl {
            let sum = ((ra[k] as i64) << 16) + ((rp[k] as i64) << 16);
            v = ((sum >> 1) as i32) >> 16;
            v = v.clamp(-MAG_LIMIT, MAG_LIMIT);
            out[MAGS + k] = v as i16;
        }
        out[MAGS + nl..MAGS + MAX_HARM].fill(v as i16);
    }
}

/// 0x0805c778: record the frame just decoded as the previous frame (state PREV).
///
/// Always copies the frame type and offsets 0x02 and 0x06. For a speech frame (type 1) it also
/// copies L, the fundamental, the voicing flags, offset 0x0e and the magnitudes (padding to
/// 0x38 with the last one), stores `extra` at PREV+0x8a and clears the repeat counter (0x86).
/// For any other type it clears the flags, 0x0e, 0x8a and 0x86. (`extra` is 0 in the decoder.)
pub fn save_frame(st: &mut State, extra: i16) {
    let cur = &load_cur(st);
    st.set_h(PREV + 6, cur[3]);
    st.set_h(PREV, cur[0]);
    st.set_h(PREV + 2, cur[1]);
    if cur[0] != 1 {
        st.set_w(PREV + 8, 0);
        st.set_h(PREV + 0x8a, 0);
        st.set_h(PREV + 0xe, 0);
        st.set_h(PREV + 0x86, 0);
        return;
    }
    let l = cur[2];
    let nl = l as usize;
    st.set_h(PREV + 4, l);
    st.set_h(PREV + 0xc, cur[6]);
    st.set_h(PREV + 0x8a, extra);
    st.set_w(PREV + 8, word(cur, 8));
    st.set_h(PREV + 0xe, cur[7]);
    let mags = st.s_mut(PREV + 0x10, MAX_HARM);
    mags[..nl].copy_from_slice(&cur[MAGS..MAGS + nl]);
    mags[nl..].fill(cur[MAGS + nl - 1]);
    st.set_h(PREV + 0x86, 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ambe::testvec::{self, Record, STATE_ADDR, assert_h_eq, assert_state_eq};

    fn each(addr: u32, f: impl Fn(&Record)) -> usize {
        match testvec::load(addr) {
            Some(recs) => {
                assert!(!recs.is_empty());
                for r in &recs {
                    f(r);
                }
                recs.len()
            }
            None => 0,
        }
    }

    /// The bit buffer behind a firmware cursor: every halfword from the cursor's pointer to
    /// the end of the captured region.
    fn bit_buffer(r: &Record, cursor_ptr: u32) -> (u32, Vec<i16>) {
        let bp = r.w_before(cursor_ptr, 1)[0] as u32;
        let g = r.regions.iter().find(|g| bp >= g.addr && (bp - g.addr) < g.before.len() as u32).unwrap();
        let o = (bp - g.addr) as usize;
        let v = g.before[o..].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
        (bp, v)
    }
    fn check_cursor(r: &Record, cursor_ptr: u32, bp: u32, bits: &Bits) {
        assert_eq!(bp + 2 * bits.pos as u32, r.w_after(cursor_ptr, 1)[0] as u32, "bit cursor, frame {}", r.frame);
    }
    fn table_for(addr: u32) -> &'static [i16] {
        match addr {
            0x0809_984c => &tab::GAIN,
            0x0809_02a4 => &tab::PRBA24,
            0x0808_fea4 => &tab::PRBA58,
            0x0808_fda4 => &tab::HOC0,
            0x0808_fd24 => &tab::HOC1,
            0x0808_fca4 => &tab::HOC2,
            0x0808_fc64 => &tab::HOC3,
            _ => panic!("unknown codebook {addr:#x}"),
        }
    }

    #[test]
    fn vq_read_matches_firmware() {
        let n = each(0x0806_1074, |r| {
            assert_eq!(r.arg(7), 0, "encoder path recorded");
            let dim = r.r[1] as usize;
            let (bp, buf) = bit_buffer(r, r.arg(6));
            let mut bits = Bits::new(&buf);
            let mut dst = r.h_before(r.r[0], dim);
            let idx = vq_read(&mut dst, r.r[2] as i32, table_for(r.r[3]), r.arg(4) as i32, r.arg(5) as i32, &mut bits);
            assert_eq!(idx, r.ret[0] as i32);
            assert_h_eq("vq dst", r, &dst, &r.h_after(r.r[0], dim));
            check_cursor(r, r.arg(6), bp, &bits);
        });
        eprintln!("0x08061074: {n} calls");
    }

    #[test]
    fn gain_dequant_matches_firmware() {
        let n = each(0x0806_1340, |r| {
            assert_eq!(r.arg(7), 0, "encoder path recorded");
            let (bp, buf) = bit_buffer(r, r.arg(6));
            let mut bits = Bits::new(&buf);
            let mut prev = r.h_before(r.arg(4), 1)[0];
            let g = gain_dequant(&mut prev, r.r[1] as i32, r.arg(5) as i32, &mut bits);
            assert_eq!(g, r.h_after(r.r[0], 1)[0], "gain, frame {}", r.frame);
            assert_eq!(prev, r.h_after(r.arg(4), 1)[0], "prev gain, frame {}", r.frame);
            check_cursor(r, r.arg(6), bp, &bits);
        });
        eprintln!("0x08061340: {n} calls");
    }

    #[test]
    fn inverse_dct_matches_firmware() {
        let n = each(0x0806_12ac, |r| {
            let n = r.r[1] as usize;
            let mut buf = r.h_before(r.r[0], n);
            inverse_dct(&mut buf, n, r.r[2] as usize);
            assert_h_eq("dct", r, &buf, &r.h_after(r.r[0], n));
        });
        eprintln!("0x080612ac: {n} calls");
    }

    #[test]
    fn dequant_magnitudes_matches_firmware() {
        let n = each(0x0805_c7f8, |r| {
            assert_eq!(r.arg(5), 0, "encoder path recorded");
            assert_eq!(r.state_off(r.r[0]), PREV);
            assert_eq!(r.state_off(r.r[3]), WIDTHS);
            let st = r.state_before();
            let mut cur = r.h_before(r.r[1], BLOCK_WORDS);
            let (bp, buf) = bit_buffer(r, r.arg(4));
            let mut bits = Bits::new(&buf);
            dequant_magnitudes(&st, &mut cur, r.r[2] as i32, &mut bits);
            assert_h_eq("magnitudes", r, &cur, &r.h_after(r.r[1], BLOCK_WORDS));
            check_cursor(r, r.arg(4), bp, &bits);
        });
        eprintln!("0x0805c7f8: {n} calls");
    }

    #[test]
    fn dequant_spectrum_matches_firmware() {
        let n = each(0x0805_cc40, |r| {
            assert_eq!(r.arg(4), 0, "encoder path recorded");
            assert_eq!(r.state_off(r.r[0]), PREV);
            assert_eq!(r.state_off(r.r[1]), CUR);
            assert_eq!(r.state_off(r.r[2]), WIDTHS);
            let mut st = r.state_before();
            let (bp, buf) = bit_buffer(r, r.r[3]);
            let mut bits = Bits::new(&buf);
            dequant_spectrum(&mut st, &mut bits);
            assert_state_eq(r, &st, &r.state_after(), &[]);
            check_cursor(r, r.r[3], bp, &bits);
        });
        eprintln!("0x0805cc40: {n} calls");
    }

    #[test]
    fn smooth_magnitudes_matches_firmware() {
        let n = each(0x0805_d11c, |r| {
            assert_eq!(r.state_off(r.r[0]), CUR);
            assert_eq!(r.state_off(r.r[1]), PREV);
            let mut st = r.state_before();
            smooth_magnitudes(&mut st, r.r[2] as i32, r.r[3] as i32);
            assert_state_eq(r, &st, &r.state_after(), &[]);
        });
        eprintln!("0x0805d11c: {n} calls");
    }

    #[test]
    fn resample_matches_firmware() {
        let n = each(0x0805_d380, |r| {
            assert_eq!(resample_step(r.r[0] as i32, r.r[1] as i32), r.ret[0] as i32);
        });
        eprintln!("0x0805d380: {n} calls");
        let n = each(0x0805_d3e8, |r| {
            let count = r.arg(4) as usize;
            let src = r.h_before(r.r[3], MAX_HARM);
            let mut dst = vec![0i16; count];
            resample_mags(&mut dst, r.r[1] as i32, r.r[2] as i32, &src);
            assert_h_eq("resampled", r, &dst, &r.h_after(r.r[0], count));
        });
        eprintln!("0x0805d3e8: {n} calls");
    }

    #[test]
    fn blend_frames_matches_firmware() {
        let n = each(0x0805_d480, |r| {
            assert_eq!(r.state_off(r.r[1]), CUR);
            assert_eq!(r.state_off(r.r[2]), PREV);
            let mut st = r.state_before();
            let mut out = r.h_before(r.r[0], BLOCK_WORDS);
            blend_frames(&mut out, &st);
            assert_h_eq("out block", r, &out, &r.h_after(r.r[0], BLOCK_WORDS));
            // The state is not written.
            st = r.state_before();
            assert_state_eq(r, &st, &r.state_after(), &[]);
        });
        eprintln!("0x0805d480: {n} calls");
    }

    #[test]
    fn save_frame_matches_firmware() {
        let n = each(0x0805_c778, |r| {
            assert_eq!(r.state_off(r.r[0]), PREV);
            assert_eq!(r.state_off(r.r[1]), CUR);
            let mut st = r.state_before();
            save_frame(&mut st, r.r[2] as i16);
            assert_state_eq(r, &st, &r.state_after(), &[]);
        });
        eprintln!("0x0805c778: {n} calls");
    }

    #[test]
    fn state_addr_matches() {
        assert_eq!(STATE_ADDR, 0x2001_1224);
    }
}
