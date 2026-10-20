//! Fixed-point real FFT / inverse FFT (firmware 0x08057b40 .. 0x08057e34).
//!
//! A real transform of `N = 1 << m` samples is done as a complex transform of `N / 2` points
//! plus an unpacking pass ([`real_pass`]). Buffers are interleaved `i16` (re, im) pairs; the
//! firmware treats each pair as one 32-bit word (re in the low half, im in the high half), and
//! so do the helpers here. All routines work in place and return a block exponent: the
//! transform is scaled by `2^-k` for some `k` that depends on the data (headroom), and the
//! exponent argument is adjusted by that `k` so callers can track the scale.

use super::ops::*;
use super::tables::fft::{BITREV_32, BITREV_128, HEADROOM_SHIFT, TWIDDLE};

/// Complex word `k` of an interleaved buffer as the firmware's packed register (lo = re).
#[inline(always)]
fn word(b: &[i16], k: usize) -> i32 {
    pack(b[2 * k] as i32, b[2 * k + 1] as i32)
}
#[inline(always)]
fn set_word(b: &mut [i16], k: usize, v: i32) {
    b[2 * k] = v as i16;
    b[2 * k + 1] = (v >> 16) as i16;
}
/// Twiddle factor `e` (angle 2*pi*e/512, clockwise) packed (lo = re, hi = im).
#[inline(always)]
fn twiddle(e: usize) -> i32 {
    pack(TWIDDLE[4 * e] as i32, TWIDDLE[4 * e + 1] as i32)
}

/// 0x08057880: headroom of a block of `n` complex words. The largest `re^2 + im^2` (as
/// `smuad`, starting from 0x1000000 so quiet blocks give a fixed answer) gives
/// `clz(max) - 2`: 5 for a block below 0x1000000, down to -1 for a block that can overflow.
fn headroom(b: &[i16], n: usize) -> i32 {
    let mut max = 0x0100_0000;
    for k in 0..n {
        let w = word(b, k);
        max = max.max(smuad(w, w));
    }
    if max == 0 { -1 } else { clz(max as u32) - 2 }
}

/// 0x080578c0: bit-reversal permutation of `n` complex words, driven by a firmware swap list
/// (a count, then pairs of halfword offsets, each relative to the previous swap's positions).
/// The firmware only selects a list for n == 32 and other sizes except 8, 16 and 64 (for
/// which it uses an unset register); n == 128 is the size this decoder uses.
fn bit_reverse(b: &mut [i16], n: usize) {
    let list: &[i16] = match n {
        32 => &BITREV_32,
        8 | 16 | 64 => panic!("0x080578c0: no swap list for {n} points"),
        _ => &BITREV_128,
    };
    let (mut a, mut c) = (0usize, 0usize);
    for pair in list[1..1 + list[0] as usize * 2].chunks(2) {
        a = (a as isize + pair[0] as isize) as usize;
        c = (c as isize + pair[1] as isize) as usize;
        let wa = pack(b[a] as i32, b[a + 1] as i32);
        let wc = pack(b[c] as i32, b[c + 1] as i32);
        b[a] = wc as i16;
        b[a + 1] = (wc >> 16) as i16;
        b[c] = wa as i16;
        b[c + 1] = (wa >> 16) as i16;
        a += 1;
        c += 1;
    }
}

/// 0x08057940: first radix-2 stage on adjacent word pairs, with no twiddle. `shift` selects the
/// scaling: -2 halves twice, 0 adds with saturation (no scaling), anything else halves once.
fn first_stage(b: &mut [i16], n: usize, shift: i32) {
    for k in (0..n).step_by(2) {
        let (a, c) = (word(b, k), word(b, k + 1));
        let (sum, diff) = match shift {
            -2 => (shadd16(shadd16(c, a), 0), shadd16(shsub16(a, c), 0)),
            0 => (qadd16(c, a), qsub16(a, c)),
            _ => (shadd16(c, a), shsub16(a, c)),
        };
        set_word(b, k, sum);
        set_word(b, k + 1, diff);
    }
}

/// 0x080579c0: one radix-2 decimation-in-time stage. `groups` groups of `half` butterflies
/// (A at word `g * 2 * half + j`, B `half` words further); twiddle `j * stride`. `scale` is the
/// firmware's -1 flag: results are halved once more (taken from bit 16 instead of bit 15).
fn stage(b: &mut [i16], half: usize, stride: usize, groups: usize, scale: bool) {
    for g in 0..groups {
        for j in 0..half {
            let ia = g * 2 * half + j;
            let ib = ia + half;
            let x = word(b, ib);
            let w = twiddle(j * stride);
            let p_re = smusd(x, w);
            let p_im = smuadx(x, w);
            let a_re = b[2 * ia] as i32;
            let a_im = b[2 * ia + 1] as i32;
            let (s_re, s_im) = (p_re.wrapping_add(a_re << 15), p_im.wrapping_add(a_im << 15));
            let (d_re, d_im) = ((a_re << 15).wrapping_sub(p_re), (a_im << 15).wrapping_sub(p_im));
            let (sum, diff) = if scale {
                (pkhtb(s_im, s_re, 16), pkhtb(d_im, d_re, 16))
            } else {
                (pkhbt(s_re >> 15, s_im, 1), pkhbt(d_re >> 15, d_im, 1))
            };
            set_word(b, ia, sum);
            set_word(b, ib, diff);
        }
    }
}

/// 0x08057b40: in-place complex FFT of `1 << m` points (radix-2, decimation in time, with
/// headroom-driven scaling). Returns `exp - k`, where `k` is the number of halvings applied
/// (stage 0 uses the table-driven amount, each later stage halves when the headroom estimate
/// says the next stage could overflow). Used for both directions: the inverse is the same
/// transform on data prepared by [`real_pass`].
fn complex_fft(b: &mut [i16], exp: i32, m: i32) -> i32 {
    let n = 1usize << m;
    let h0 = headroom(b, n);
    let mut k = if (h0 + 1) as u32 <= 6 { HEADROOM_SHIFT[(h0 + 1) as usize] as i32 } else { 0 };
    bit_reverse(b, n);
    first_stage(b, n, k);
    let mut h = h0 - 2;
    let mut groups = n >> 2;
    let mut stride = 0x80usize;
    let mut half = 2usize;
    for i in 1..m {
        if i & 1 == 0 {
            h = headroom(b, n);
        }
        let scale = h <= 1;
        stage(b, half, stride, groups, scale);
        stride >>= 1;
        groups >>= 1;
        half <<= 1;
        if scale {
            k -= 1;
        }
        h -= 2;
    }
    exp.wrapping_sub(k)
}

/// 0x08057c00 (forward) / 0x08057c80 (inverse): combine word `1 + i` with word `n - 1 - i` of
/// the half-size complex spectrum (`count` pairs, `n` = number of complex words).
fn unpack_pairs(b: &mut [i16], n: usize, count: usize, inverse: bool) {
    for i in 0..count {
        let (ia, ib) = (1 + i, n - 1 - i);
        let (a_re, a_im) = (b[2 * ia] as i32, b[2 * ia + 1] as i32);
        let (b_re, b_im) = (b[2 * ib] as i32, b[2 * ib + 1] as i32);
        let re_sum = (a_re + b_re) >> 1;
        let im_sum = (a_im + b_im) >> 1;
        let (new_a, new_b) = if inverse {
            (pkhbt(re_sum, b_im - a_im, 15), pkhbt(im_sum, a_re - b_re, 15))
        } else {
            (pkhbt(re_sum, a_im - b_im, 15), pkhbt(im_sum, b_re - a_re, 15))
        };
        set_word(b, ia, new_a);
        set_word(b, ib, new_b);
    }
}

/// 0x08057d00: rotate the same word pairs by the twiddle `(i + 1) * stride`, where
/// `stride = 0x200 >> m`.
fn rotate_pairs(b: &mut [i16], n: usize, count: usize, stride: usize) {
    for i in 0..count {
        let (ia, ib) = (1 + i, n - 1 - i);
        let x = word(b, ib);
        let w = twiddle((i + 1) * stride);
        let t_im = smuadx(x, w);
        let t_re = smusd(x, w);
        let (a_re, a_im) = ((b[2 * ia] as i32) << 15, (b[2 * ia + 1] as i32) << 15);
        let new_a = pkhtb(t_im.wrapping_add(a_im), t_re.wrapping_add(a_re), 16);
        let new_b = pkhtb(t_im.wrapping_sub(a_im), a_re.wrapping_sub(t_re), 16);
        set_word(b, ia, new_a);
        set_word(b, ib, new_b);
    }
}

/// 0x08057d5c: the real-transform pass between the half-size complex FFT and the real
/// spectrum (forward: after the FFT; inverse: before it). `m` is log2 of the number of real
/// samples. Packs/unpacks the DC and Nyquist terms, combines mirrored bins, applies the
/// twiddles and fixes up the centre bin. With `edge` set, DC and Nyquist are kept in
/// separate words (`buf[0]`, `buf[N]`; the buffer needs `N + 2` halfwords) instead of
/// packed as `buf[0..2]`. Returns `exp + 1`.
fn real_pass(b: &mut [i16], exp: i32, m: i32, inverse: bool, edge: bool) -> i32 {
    let n = 1usize << m;
    let x0 = b[0] as i32;
    let (shift, x1) = if edge && inverse { (14, b[n] as i32) } else { (15, b[1] as i32) };
    let (p0, p1) = (x0 << shift, x1 << shift);
    let (dc, other) = (p1.wrapping_add(p0) >> 16, p0.wrapping_sub(p1) >> 16);
    b[0] = dc as i16;
    if edge && !inverse {
        b[n] = other as i16;
        b[n + 1] = 0;
        b[1] = 0;
    } else {
        b[1] = other as i16;
    }
    let count = (n >> 2) - 1;
    unpack_pairs(b, n / 2, count, inverse);
    rotate_pairs(b, n / 2, count, asr(0x200, m) as usize);
    let mid = n / 2;
    b[mid] >>= 1;
    let v = (b[mid + 1] as i32) << 16;
    let neg = if v == i32::MIN { i32::MAX } else { -v };
    b[mid + 1] = (neg >> 17) as i16;
    exp.wrapping_add(1)
}

/// 0x08057e10: forward real FFT, in place, of `1 << m` real samples (m = 8 in this decoder:
/// 256 samples as 128 complex words; `buf` needs `N` halfwords, `N + 2` with `edge`).
/// Output is the half spectrum as interleaved (re, im) bins; with `edge` false the Nyquist
/// real part is packed into `buf[1]`, with `edge` true it goes to `buf[N]` and `buf[1]`,
/// `buf[N + 1]` are zero. `exp` is a block exponent which is returned adjusted for the
/// scaling applied (halvings of the transform stages, plus one).
pub fn fft_forward(buf: &mut [i16], exp: i32, m: i32, edge: bool) -> i32 {
    let e = complex_fft(buf, exp, m - 1);
    let r = real_pass(buf, e, m, false, edge);
    if !edge {
        buf[1] = 0;
    }
    r
}

/// 0x08057e34: inverse real FFT, in place; the converse of [`fft_forward`], same layout and
/// arguments (`edge` set means DC and Nyquist come separately in `buf[0]` / `buf[N]`).
/// Returns `exp` adjusted for the scaling applied, as an i32 (callers keep the low 16 bits).
pub fn fft_inverse(buf: &mut [i16], exp: i32, m: i32, edge: bool) -> i32 {
    if !edge {
        buf[1] = 0;
    }
    let e = real_pass(buf, exp, m, true, edge);
    let r = complex_fft(buf, e, m - 1);
    r.wrapping_add(1 - m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ambe::testvec;

    /// Replay every recorded call of one entry point.
    fn replay(addr: u32, f: fn(&mut [i16], i32, i32, bool) -> i32) {
        let recs = testvec::load(addr).expect("recordings");
        assert!(!recs.is_empty());
        let mut sizes = std::collections::BTreeSet::new();
        for rec in &recs {
            let ptr = rec.arg(0);
            let (exp, m, edge) = (rec.arg(1) as i32, rec.arg(2) as i32, rec.arg(3) != 0);
            sizes.insert((m, edge));
            let len = (1usize << m) + if edge { 2 } else { 0 };
            let mut buf = rec.h_before(ptr, len);
            let ret = f(&mut buf, exp, m, edge);
            testvec::assert_h_eq("buffer", rec, &buf, &rec.h_after(ptr, len));
            assert_eq!(ret as u32, rec.ret[0], "return value: frame {} call {}", rec.frame, rec.call);
        }
        eprintln!("{addr:#x}: {} calls, (m, edge) seen: {sizes:?}", recs.len());
    }

    #[test]
    fn headroom_matches_firmware() {
        let recs = testvec::load(0x08057880).expect("recordings");
        for rec in &recs {
            let n = rec.arg(1) as usize;
            let buf = rec.h_before(rec.arg(0), 2 * n);
            assert_eq!(headroom(&buf, n) as u32, rec.ret[0], "frame {} call {}", rec.frame, rec.call);
        }
        eprintln!("0x08057880: {} calls", recs.len());
    }

    #[test]
    fn complex_fft_matches_firmware() {
        let recs = testvec::load(0x08057b40).expect("recordings");
        let mut sizes = std::collections::BTreeSet::new();
        for rec in &recs {
            let (exp, m) = (rec.arg(1) as i32, rec.arg(2) as i32);
            sizes.insert(m);
            let len = 2usize << m;
            let mut buf = rec.h_before(rec.arg(0), len);
            let ret = complex_fft(&mut buf, exp, m);
            testvec::assert_h_eq("buffer", rec, &buf, &rec.h_after(rec.arg(0), len));
            assert_eq!(ret as u32, rec.ret[0], "return: frame {} call {}", rec.frame, rec.call);
        }
        eprintln!("0x08057b40: {} calls, m seen: {sizes:?}", recs.len());
    }

    #[test]
    fn stage_matches_firmware() {
        let recs = testvec::load(0x080579c0).expect("recordings");
        for rec in &recs {
            let (stride, half, groups, flag) = (rec.arg(2) as usize, rec.arg(3) as usize, rec.arg(4) as usize, rec.arg(5) as i32);
            let len = 2 * 2 * half * groups;
            let mut buf = rec.h_before(rec.arg(0), len);
            stage(&mut buf, half, stride, groups, flag == -1);
            testvec::assert_h_eq("buffer", rec, &buf, &rec.h_after(rec.arg(0), len));
        }
        eprintln!("0x080579c0: {} calls", recs.len());
    }

    /// Replay a routine on word pairs: r0 = word 1 of a buffer of n = 2 * (count + 1) words,
    /// r1 = word n - 1.
    fn pairs(addr: u32, f: impl Fn(&mut [i16], usize, usize, usize)) {
        let recs = testvec::load(addr).expect("recordings");
        for rec in &recs {
            let (stride, count) = if addr == 0x08057d00 { (rec.arg(2) as usize, rec.arg(3) as usize) } else { (0, rec.arg(2) as usize) };
            let n = 2 * (count + 1);
            let base = rec.arg(0) - 4;
            assert_eq!(rec.arg(1), base + 4 * (n as u32 - 1));
            let mut buf = rec.h_before(base, 2 * n);
            f(&mut buf, n, count, stride);
            testvec::assert_h_eq("buffer", rec, &buf, &rec.h_after(base, 2 * n));
        }
        eprintln!("{addr:#x}: {} calls", recs.len());
    }

    #[test]
    fn rotate_matches_firmware() {
        pairs(0x08057d00, rotate_pairs);
    }

    #[test]
    fn unpack_forward_matches_firmware() {
        pairs(0x08057c00, |b, n, count, _| unpack_pairs(b, n, count, false));
    }

    #[test]
    fn unpack_inverse_matches_firmware() {
        pairs(0x08057c80, |b, n, count, _| unpack_pairs(b, n, count, true));
    }

    #[test]
    fn real_pass_matches_firmware() {
        let recs = testvec::load(0x08057d5c).expect("recordings");
        let mut seen = std::collections::BTreeSet::new();
        for rec in &recs {
            let (exp, m, inverse, edge) = (rec.arg(1) as i32, rec.arg(2) as i32, rec.arg(3) != 0, rec.arg(4) != 0);
            seen.insert((m, inverse, edge));
            let len = (1usize << m) + 2;
            let mut buf = rec.h_before(rec.arg(0), len);
            let ret = real_pass(&mut buf, exp, m, inverse, edge);
            testvec::assert_h_eq("buffer", rec, &buf, &rec.h_after(rec.arg(0), len));
            assert_eq!(ret as u32, rec.ret[0]);
        }
        eprintln!("0x08057d5c: {} calls, (m, inverse, edge) seen: {seen:?}", recs.len());
    }

    #[test]
    fn forward_matches_firmware() {
        replay(0x08057e10, fft_forward);
    }

    #[test]
    fn inverse_matches_firmware() {
        replay(0x08057e34, fft_inverse);
    }
}
