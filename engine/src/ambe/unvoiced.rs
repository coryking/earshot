//! Unvoiced (noise) synthesis for one sub-frame (firmware 0x0805d8a4).
//!
//! White noise from a 16-bit LCG is windowed, transformed to the frequency domain, shaped per
//! harmonic band (voiced bands are zeroed, unvoiced bands are scaled to the band's amplitude),
//! transformed back and overlap-added into the 32-bit output accumulator.

use super::fft::{fft_forward, fft_inverse};
use super::math;
use super::ops::*;
use super::state::State;
use super::tables::unvoiced::UNVOICED_WINDOW;

/// Byte offset in the state block of the noise state this routine owns (the `r2` argument).
pub const NOISE_STATE: usize = 0x64c;
/// Halfwords of the noise state: [0] LCG seed, [1..0x55) noise history, [0x55..0xa9) overlap
/// tail of the previous sub-frame, [0xa9] block exponent of that tail.
const NOISE_WORDS: usize = 0xaa;
const SEED: usize = 0;
const HISTORY: usize = 1;
const HISTORY_LEN: usize = 0x54;
const TAIL: usize = 0x55;
const TAIL_EXP: usize = 0xa9;

/// FFT size: 256 real samples (`n <= 128` always gives the firmware's 8-bit transform).
const FFT_LOG2: i32 = 8;
const FFT_N: usize = 1 << FFT_LOG2;

/// SBFX: signed bit field extract.
#[inline(always)]
fn sbfx(x: i32, lsb: u32, width: u32) -> i32 {
    (x << (32 - lsb - width)) >> (32 - width)
}

/// Shift left by `sh` (>= 0), saturating to the sign: the firmware's `lsl; asr; compare` idiom.
#[inline(always)]
fn sat_lsl(x: i32, sh: i32) -> i32 {
    let y = lsl(x, sh);
    if asr(y, sh) != x { 0x7fff_ffff ^ (x >> 31) } else { y }
}

/// One overlap-add term: 2 * (a * b) as a saturating 32-bit value (QADD), aligned to the
/// accumulator scale: a block with exponent `e` is shifted by `e - 2` (left, saturating) or
/// `2 - e` (right).
#[inline(always)]
fn ola_term(a: i16, b: i16, e: i32) -> i32 {
    let p = smulbb(a as i32, b as i32);
    let q = qadd(p, p);
    if e > 2 { sat_lsl(q, e - 2) } else { asr(q, 2 - e) }
}

/// 0x0805d8a4: synthesize `acc.len()` (= 80) unvoiced samples of one sub-frame and add them
/// into the 32-bit accumulator `acc`.
///
/// * `st`: the decoder state; this routine owns the noise state at [`NOISE_STATE`]: an LCG
///   seed (`x = x * 173 + 13849`), the last 84 noise samples, the second half of the previous
///   sub-frame's inverse transform (overlap tail) and that tail's block exponent.
/// * `pb`: the 0x44-halfword parameter block: `pb[0]` mode (2 and 3 are special: 3 means
///   silence, 2 scales every band by a fixed gain instead of measuring its energy), `pb[2]`
///   harmonic count L, `pb[6]` harmonic spacing (the band edges follow from it), `pb[8..8+L]`
///   harmonic amplitudes, word at `pb[0x42]` a block exponent.
/// * `voicing`: the per-harmonic voicing array that `pb[0x40..0x42]` points to (0 = unvoiced).
/// * `gain`: a 16-bit value (state+0x7c2 in the caller) that sets the low-frequency cutoff
///   (spectrum bins below `256 * gain >> 19` are cleared).
///
/// Steps: window the noise (the 81-point table interpolated to n+1 points; first half over the
/// older history, second half over the fresh noise), forward FFT, then per harmonic band
/// either clear it (voiced) or scale it to the harmonic amplitude (unvoiced), clear the bins
/// above the last band and below the cutoff, inverse FFT, and overlap-add: the previous tail
/// times the falling window plus this block times the rising window, each aligned to a common
/// scale by its block exponent. The second half of the block becomes the next tail.
/// Unclear: the physical meaning of the constants 0x435b / 0x57fa / 0x86b6-style gain factors.
pub fn unvoiced(acc: &mut [i32], st: &mut State, pb: &[i16], voicing: &[i16], gain: i16) {
    let n = acc.len();
    // The caller clamps n to 76..84; the history shift below needs n <= 84.
    assert!((1..=HISTORY_LEN).contains(&n));
    let s = st.s_mut(NOISE_STATE, NOISE_WORDS);
    let old_exp = s[TAIL_EXP] as i32;

    // Window: ratio 80 / n as a pseudo-float, then linear interpolation into the table.
    let sh = norm32((n as i32) << 16) as i16 as i32;
    let den = lsl((n as i32) << 16, sh) >> 16;
    let (rm, re) = math::div_norm(0x5000_0000, -8, den, -sh);
    let rm = rm << 16;
    let ratio = if re <= 14 { asr(rm, 15 - re) } else { lsl(rm, re - 15) };
    let mut w = [0i16; HISTORY_LEN + 2];
    w[0] = UNVOICED_WINDOW[0];
    let mut pos = ratio;
    for wi in w.iter_mut().take(n).skip(1) {
        let idx = (pos >> 16) as usize;
        let (t0, t1) = (UNVOICED_WINDOW[idx] as i32, UNVOICED_WINDOW[idx + 1] as i32);
        let f = (pos >> 1) & 0x7fff;
        let v = ((t0 << 16).wrapping_sub(smulbb(t0, f) << 1)).wrapping_add(smulbb(f, t1) << 1);
        *wi = (v >> 16) as i16;
        pos = pos.wrapping_add(ratio);
    }
    w[n] = UNVOICED_WINDOW[0x50];

    let mut buf = [0i16; FFT_N];
    let new_exp = if pb[0] == 3 {
        // Silence: clear the noise history; the (zero) block still overlaps the old tail.
        s[HISTORY..HISTORY + HISTORY_LEN].fill(0);
        0
    } else {
        // Windowed noise: older history under the rising window, then the newest samples
        // (after shifting the history down and appending n fresh LCG samples) under the
        // falling window.
        for i in 0..n {
            buf[i] = (smulbb(s[HISTORY + i] as i32, w[i] as i32) >> 15) as i16;
        }
        let keep = HISTORY_LEN - n;
        s.copy_within(HISTORY + n..HISTORY + n + keep, HISTORY);
        for j in 0..n {
            let x = (smulbb(s[SEED] as i32, 173) + 13849) as u16 as i16;
            s[SEED] = x;
            s[HISTORY + keep + j] = x;
        }
        for j in 0..n {
            buf[n + j] = (smulbb(s[HISTORY + j] as i32, w[n - j] as i32) >> 15) as i16;
        }
        let exp_f = fft_forward(&mut buf, 0, FFT_LOG2, false);

        // Spectrum shaping.
        let mode2 = pb[0] == 2;
        let l = pb[2] as i32;
        let lu = l.max(0) as usize;
        // Largest unvoiced amplitude, as a left-aligned value, and its headroom.
        let mut max_amp = 0i32;
        for k in 0..lu {
            if voicing[k] == 0 {
                max_amp = max_amp.max((pb[8 + k] as u16 as i32) << 16);
            }
        }
        let hr = norm32(max_amp);
        // Overall gain (a 16-bit fraction) and its exponent.
        let (gain1, e_loc);
        if mode2 {
            let mut ex = 0x11;
            let r = math::sqrt_norm(smulbb(l, 0x435b) << 1, &mut ex);
            e_loc = exp_f.wrapping_add(1).wrapping_add(ex);
            gain1 = (smultb(r, 0x57fa) >> 15) & 0xffff;
        } else {
            let mut ex = 0x11;
            let r = math::sqrt_norm(smulbb(l.wrapping_mul(n as i32), 0x435b) << 1, &mut ex);
            e_loc = ex;
            gain1 = ((r as u32) >> 16) as i32;
        }
        // Band edges (in bins): edge k is (1/2 + (k + 1)) * step >> 16, clamped to FFT_N / 2.
        let step = sbfx((FFT_N as i32).wrapping_mul(pb[6] as i32), 3, 28);
        let b0 = step.wrapping_add(0x10000);
        let e_ref = e_loc.wrapping_sub(hr).wrapping_add(2);
        let first = b0 >> 17;
        let mut p = (2 * first).max(0) as usize; // halfword position in buf
        buf[..p].fill(0);
        let mut edge_acc = b0 >> 1;
        let mut prev = first;
        let mut last = first;
        for k in 0..lu {
            edge_acc = edge_acc.wrapping_add(step);
            last = (edge_acc >> 16).min(FFT_N as i32 / 2);
            let width = last - prev;
            let count = (2 * width).max(0) as usize;
            if voicing[k] != 0 {
                buf[p..p + count].fill(0);
            } else {
                // The firmware's scaling loop does not terminate for an empty band; bands are
                // never empty or negative for a real parameter block.
                assert!(count > 0, "empty unvoiced band");
                let amp = pb[8 + k] as i32;
                let band = &mut buf[p..p + count];
                let (g, e1) = if mode2 {
                    (smulbb(gain1, amp) << 1, e_loc)
                } else {
                    let (em, ee) = math::energy(band);
                    let e_hi = em >> 16;
                    if e_hi <= 0 {
                        (0, e_loc)
                    } else {
                        let a = width << 16;
                        let s = norm32(a) as i16 as i32;
                        let mut a = lsl(a, s);
                        let mut ex = 15 - s;
                        if e_hi <= a >> 16 {
                            ex = 16 - s;
                            a >>= 1;
                        }
                        let q = math::div_half(a, e_hi);
                        ex -= ee;
                        let r = math::sqrt_norm(q << 16, &mut ex);
                        let t = (smultb(r, gain1) >> 15) & 0xffff;
                        (smulbb(t, amp) << 1, ex.wrapping_add(e_loc))
                    }
                };
                let g = if hr >= 0 { lsl(g, hr) } else { asr(g, -hr) } >> 16;
                math::scale_shift_in_place(band, g, e1.wrapping_sub(hr), e_ref);
            }
            p += count;
            prev = last;
        }
        // Clear the top bins and the bins below the cutoff.
        let top = (FFT_N as i32 / 2 - 0x79).max(FFT_N as i32 / 2 - last) as usize;
        buf[FFT_N - 2 * top..].fill(0);
        let cutoff = (sbfx((FFT_N as i32).wrapping_mul(gain as i32), 19, 12) * 2).max(0) as usize;
        buf[..cutoff].fill(0);
        let exp_in = ((pb[0x42] as u16 as i32) | ((pb[0x43] as i32) << 16)).wrapping_add(e_ref);
        fft_inverse(&mut buf, exp_in, FFT_LOG2, false)
    };

    // Overlap-add: old tail under the falling window, new block under the rising window.
    for (i, a) in acc.iter_mut().enumerate() {
        let old = ola_term(s[TAIL + i], w[n - i], old_exp);
        let new = ola_term(buf[i], w[i], new_exp);
        *a = qadd(qadd(old, new), *a);
    }
    s[TAIL..TAIL + n].copy_from_slice(&buf[n..2 * n]);
    s[TAIL + n..TAIL + HISTORY_LEN].fill(0);
    s[TAIL_EXP] = new_exp as u16 as i16;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ambe::testvec;

    #[test]
    fn unvoiced_matches_firmware() {
        let recs = testvec::load(0x0805d8a4).expect("recordings");
        assert!(!recs.is_empty());
        let mut seen = std::collections::BTreeSet::new();
        for rec in &recs {
            let n = rec.arg(1) as usize;
            assert_eq!(rec.state_off(rec.arg(2)), NOISE_STATE);
            let mut st = rec.state_before();
            let pb = rec.h_before(rec.arg(3), 0x44);
            let vptr = (pb[0x40] as u16 as u32) | ((pb[0x41] as u16 as u32) << 16);
            let l = pb[2].max(0) as usize;
            let voicing = if l > 0 { rec.h_before(vptr, l) } else { vec![] };
            let gain = rec.arg(4) as i16;
            let mut acc = rec.w_before(rec.arg(0), n);
            unvoiced(&mut acc, &mut st, &pb, &voicing, gain);
            seen.insert((pb[0], n));
            let want = rec.w_after(rec.arg(0), n);
            assert_eq!(acc, want, "accumulator: frame {} call {}", rec.frame, rec.call);
            testvec::assert_state_eq(rec, &st, &rec.state_after(), &[]);
        }
        eprintln!("0x0805d8a4: {} calls, (mode, n) seen: {seen:?}", recs.len());
    }
}
