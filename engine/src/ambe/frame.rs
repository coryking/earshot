//! Frame-level parameter decode of the AMBE+2 decoder: field widths, the bit reorder of the
//! 49-bit frame into field order, tone / erasure / repeat handling, and the copies of the
//! 0x44-halfword parameter blocks.
//!
//! Parameter block layout (halfwords), as far as these routines tell:
//! `[0]` frame kind (0 = none, 1 = voice, 2 = repeat, 3 = tone / silence), `[1]` pitch or tone
//! index byte, `[2]` harmonic count (9..=56), `[3]` tone harmonic indices (or pitch-related),
//! `[4..6]` a 32-bit word of two-bit band flags (the "word at +8"), `[6]` fundamental or tone
//! flags, `[7]` a level, `[8..0x40]` 56 per-harmonic values, `[0x40..0x42]` the firmware's
//! pointer to the per-harmonic voicing array (not modelled), `[0x42..0x44]` a word whose upper
//! halfword (`[0x43]`) counts frames. Blocks are passed as `&mut [i16]` slices of at least 0x44
//! halfwords; the decoder state keeps several of them (see `state::off`).

use super::math::{self, Bits};
use super::ops::*;
use super::tables::frame as t;

/// Byte offset of the field-width table in the state block (13 halfwords).
pub const WIDTHS: usize = 0x7a4;
/// Halfwords in the field-width table.
pub const WIDTHS_LEN: usize = 13;
/// Byte offset of the pitch-index history in the state block (counter + 17 indices).
pub const PITCH_HISTORY: usize = 0x7c8;
/// Halfwords in the pitch-index history.
pub const PITCH_HISTORY_LEN: usize = 18;

/// The 32-bit word at halfwords 4..6 of a parameter block (byte offset 8).
#[inline]
pub fn band_word(blk: &[i16]) -> i32 {
    (blk[4] as u16 as i32) | ((blk[5] as i32) << 16)
}
/// Set the 32-bit word at halfwords 4..6 of a parameter block.
#[inline]
pub fn set_band_word(blk: &mut [i16], v: i32) {
    blk[4] = v as i16;
    blk[5] = (v >> 16) as i16;
}

/// 0x08055170: fill the field-width table (`WIDTHS_LEN` halfwords at state+0x7a4) for a
/// frame with `n` (clamped to 0..=4) extra parameter-set bits. The caller passes the table's
/// first halfword as the enable argument; the table is left alone when it is 0.
///
/// Layout afterwards: `[0]=[1]=1`, `[2]` total number of bits (49 when `n == 0`), `[3]=n`,
/// `[4..=12]` widths of the data fields (5, 7, 5, 9, 7, 5, 4, 4, 3 adjusted by `n`).
pub fn field_widths(w: &mut [i16], n: i32) {
    if w[0] == 0 {
        return;
    }
    let n = n.min(4).max(0);
    w[3] = n as i16;
    w[7] = 9;
    w[0] = 1;
    w[1] = 1;
    w[4] = 5;
    w[5] = 7;
    w[6] = 5;
    w[8] = 7;
    w[9] = 5;
    w[10] = 4;
    w[11] = 4;
    w[12] = 3;
    match n {
        1 => w[12] = 2,
        2 => w[12] = 1,
        3 => {
            w[11] = 3;
            w[12] = 1;
        }
        4 => {
            w[10] = 3;
            w[11] = 3;
            w[12] = 1;
        }
        _ => {}
    }
    let mut total = 12i32 + 5;
    for &v in &w[7..=12] {
        total += v as i32;
    }
    w[2] = total as i16;
}

/// 0x0805cc98 (called with param_2 = 1): reorder the frame's `w[2]` bits (one per halfword) in
/// place from the order they arrive in to field order. Copies the stream into a scratch array
/// at the positions the field widths `w` (the 0x7a4 table) dictate, then copies the scratch
/// back. The firmware's other direction (param_2 = 0) is not used by this decoder.
pub fn bit_reorder(bits: &mut [i16], w: &[i16]) {
    let mut l = [0i16; 194];
    let mut p = 0usize;
    let mut put = |dst: i32, n: i32| {
        if n > 0 {
            let (d, n) = (dst as usize, n as usize);
            l[d..d + n].copy_from_slice(&bits[p..p + n]);
            p += n;
        }
    };
    let g = |i: usize| w[i] as i32;
    put(0, 4);
    let a1 = g(5);
    put(a1, 4);
    let a2 = a1 + g(4);
    put(a2, 4);
    let a3 = a2 + g(6);
    put(a3, 8);
    let a4 = a3 + g(7);
    put(a4, 4);
    let a5 = a4 + g(8);
    put(a5, 4);
    let a6 = a5 + g(9);
    put(a6, 3);
    let a7 = a6 + g(10);
    put(a7, 3);
    put(a7 + g(11), 1);
    put(g(5) + 4, g(4) - 4);
    put(g(5) + g(4) + 4, g(6) - 4);
    put(4, g(5) - 4);
    let mut s = g(4) + g(5) + g(6) + g(7);
    put(s + 4, g(8) - 4);
    put(g(4) + g(5) + g(6) + 8, g(7) - 8);
    s += g(8);
    put(s + 4, g(9) - 4);
    s += g(9);
    put(s + 3, g(10) - 3);
    let r8 = g(10) + s;
    put(r8 + 3, g(11) - 3);
    put(g(11) + r8 + 1, g(12) - 1);
    let n = g(2).max(0) as usize;
    bits[..n].copy_from_slice(&l[..n]);
}

/// 0x08060f4c: spread each two-bit group of the low half of `v` into four bits (the pair
/// repeated), most significant group first, giving a 32-bit word; the same for the high half.
/// Returns (word from the low half, word from the high half).
pub fn spread_pairs(v: i32) -> (i32, i32) {
    let (mut a, mut b) = (0i32, 0i32);
    let (mut lo_half, mut hi_half) = (v, v >> 16);
    let mut sh = 0x1e;
    while sh != -2 {
        let (x, y) = (lo_half & 3, hi_half & 3);
        a |= (x << sh) | (x << (sh - 2));
        b |= (y << sh) | (y << (sh - 2));
        lo_half >>= 2;
        hi_half >>= 2;
        sh -= 4;
    }
    (a, b)
}

/// 0x0805d0ec: band-flag words for the two parameter blocks (stored at their halfwords 4..6).
/// With `flag == 1` the `width`-bit field value `v` indexes a table of 7-bit patterns that
/// `spread_pairs` expands to two words; otherwise both are 0. Returns (first, second) where
/// the firmware's output pointers were (block B + 8, block A + 8).
pub fn band_flags(flag: i32, v: i32, width: i32) -> (i32, i32) {
    if flag != 1 {
        return (0, 0);
    }
    let idx = lsl(v, 7 - width) as i16 as usize;
    spread_pairs(t::SPREAD_BITS[idx] as i32)
}

/// 0x0805c368 (called with mode 0, the only way this decoder calls it): dequantise the field
/// value `v` (u16) of an `n`-level quantiser into a 16-bit level, from a pseudo-float
/// reciprocal of `n`: (2v + 1) * (1/n) minus an offset, rescaled by the exponent. Returns the
/// value before the caller truncates it to a halfword.
pub fn dequant_step(v: i32, n: i32) -> i32 {
    let t0 = n << 16;
    let s = norm32(t0) as i16 as i32;
    let mant = lsl(t0, s) >> 16;
    let (m, e) = math::div_norm(0x4100_0000, 8, mant, 15 - s);
    let x = (((v as u16 as i32) << 17).wrapping_add(0x10000)) >> 16;
    let k = -7 - e;
    let off = if k < 0 { asr(0xbf00_0000u32 as i32, e + 7) } else { lsl(0xbf00_0000u32 as i32, k) };
    let mut r = off.wrapping_add(m.wrapping_mul(x).wrapping_shl(1));
    r = if e + 7 >= 0 { lsl(r, e + 7) } else { asr(r, k) };
    r.wrapping_add(0x8000) >> 16
}

/// 0x0805c47c (called with mode 0, the only way this decoder calls it): decode a pair of
/// levels for the two parameter blocks from `idx`, the value of the combined field, using
/// the band flags in each block's word at halfwords 4..6. Each block's halfword 7 receives its
/// level. `shift` is the field width (`w[5]`): the field has `1 << shift` combinations.
///
/// Returns false (both levels set to 0) when a band word has bits in the low position of a
/// pair or neither block has any flag in the high position. With flags in only one block that
/// block gets the whole index, the other gets 0; with flags in both the index is split into a
/// quotient and remainder by sqrt(1 << shift) (scaled by 0x5a82/0x8000 for an odd `shift`).
pub fn pair_levels(idx: i16, blk_b: &mut [i16], blk_a: &mut [i16], shift: i32) -> bool {
    let wb = band_word(blk_b);
    let wa = band_word(blk_a);
    if (wa | wb) & 0x5555_5555 != 0 {
        blk_a[7] = 0;
        blk_b[7] = 0;
        return false;
    }
    let b_has = wb & 0xaaaa_aaaau32 as i32 != 0;
    let a_has = wa & 0xaaaa_aaaau32 as i32 != 0;
    let v = idx as u16 as i32;
    match (b_has, a_has) {
        (false, false) => {
            blk_a[7] = 0;
            blk_b[7] = 0;
            false
        }
        (false, true) => {
            blk_b[7] = 0;
            blk_a[7] = dequant_step(v, lsl(1, shift)) as i16;
            true
        }
        (true, false) => {
            blk_a[7] = 0;
            blk_b[7] = dequant_step(v, lsl(1, shift)) as i16;
            true
        }
        (true, true) => {
            let mut n = lsl(1, shift >> 1);
            if shift & 1 != 0 {
                n = n.wrapping_mul(0x5a82) << 2 >> 16;
            }
            let q = math::div_int(idx as i32, n as i16 as i32);
            let q16 = q as u16 as i32;
            let rem = (v.wrapping_sub(n.wrapping_mul(q16)) as u16) as i32;
            let mut l1 = q16;
            let mut l0 = rem;
            if n <= q16 as i16 as i32 {
                l1 = n - 1;
            }
            if n <= rem as i16 as i32 {
                l0 = n - 1;
            }
            blk_b[7] = dequant_step(l0, n) as i16;
            blk_a[7] = dequant_step(l1, n) as i16;
            true
        }
    }
}

/// 0x08060fe0: from `x` (a log-like period measure, normally a halfword) compute the
/// fundamental `*out_a` and the harmonic count `*out_b` (via `math::harmonic_count`). `x` is
/// folded into a polynomial range by repeated +0x1000 steps; the result is clamped to
/// 0x1079..=0x6bca (the upper clamp only when `flag == 0`).
pub fn period_params(out_a: &mut i16, out_b: &mut i16, x: i32, flag: i32) {
    let mut x = x;
    let mut sh = -4i32;
    while x < -0x1000 {
        x = (x + 0x1000) as i16 as i32;
        sh += 1;
    }
    let mut a = (x * 5).wrapping_mul(64).wrapping_sub(x * 5) >> 12;
    a = (a + 0x71b) as u16 as i32;
    let mut b = smulbb(x, a) >> 12;
    b = (b + 0x1ec0) as u16 as i32;
    let mut c = smulbb(x, b) >> 12;
    c = (c + 0x58b9) as u16 as i32;
    let d = (smulbb(x, c) as u32 >> 12) as i32 & 0xffff;
    let d = (d ^ 0x8000) as i16 as i32;
    let r1 = asr(d, sh) as u16;
    let mut r2 = r1 as i16 as i32;
    let mut stored = r1;
    if r2 <= 0x1078 {
        stored = 0x1079;
        r2 = 0x1079;
    } else if r2 > 0x6bca && flag == 0 {
        stored = 0x6bca;
        r2 = 0x6bca;
    }
    *out_a = stored as i16;
    *out_b = math::harmonic_count(r2) as i16;
}

/// 0x0805d708: voice-frame pitch parameters. For a voice block (`blk[0] == 1`; the glue passes
/// exactly that halfword as its first argument) derive `blk[6]` (fundamental) and `blk[2]`
/// (harmonic count) from the pitch index `idx` and the field width `shift` (`w[5]`) through
/// `period_params`. Any other kind gets the default `blk[6] = 0x4027`, `blk[2] = 0xf`.
/// `flag` is the glue's last argument (a halfword); it only controls the upper clamp.
pub fn pitch_params(blk: &mut [i16], idx: i32, shift: i32, flag: i32) {
    if blk[0] != 1 {
        blk[6] = 0x4027;
        blk[2] = 0xf;
        return;
    }
    let d = lsl(0x78, shift - 6) as i16 as i32;
    let n = (idx << 1).wrapping_mul(0x2a52).wrapping_add(0x2a52);
    let q = math::div_int(n, d);
    let x = (0xbb00_0000u32 as i32).wrapping_sub(q << 16).wrapping_add(0x30000) >> 16;
    let (mut out_a, mut out_b) = (blk[6], blk[2]);
    period_params(&mut out_a, &mut out_b, x, flag as i16 as i32);
    blk[6] = out_a;
    blk[2] = out_b;
}

/// 0x080556bc: class of a tone / pitch index byte: 0 = 5..=0x7a (single tone), 1 = 0x80..=0x8f,
/// 2 = 0x90..=0x9f, 3 = 0xa0..=0xa3, 4 = anything else. Only the low 16 bits of `idx` count.
pub fn tone_class(idx: i32) -> i32 {
    let i = idx as u16;
    if i.wrapping_sub(5) <= 0x75 {
        0
    } else if i.wrapping_sub(0x80) <= 0xf {
        1
    } else if i.wrapping_sub(0x90) <= 0xf {
        2
    } else if i.wrapping_sub(0xa0) <= 3 {
        3
    } else {
        4
    }
}

/// 0x080556fc: harmonic index of a tone component. Class 0 (single tone): `which` is ignored
/// and the index is `(5 * idx * 1024 + 0xffff) >> 16` less 1, floored at 0 (idx as a
/// sign-extended halfword). Classes 1..=3 (see `tone_class`): the table entry for component
/// `which` (0 or 1) of tone `idx` (0x80..=0xa3), less 1. Other classes give 0. Returns a
/// sign-extended halfword.
pub fn tone_harmonic(class: i32, idx: i32, which: i32) -> i32 {
    if class == 0 {
        let x = qadd((idx + (idx << 2)) << 10, 0xffff);
        let r = ((x as u32 >> 16) as i32 - 1) as i16 as i32;
        return r.max(0) as u16 as i16 as i32;
    }
    if class as u32 > 3 {
        return 0;
    }
    let i = (idx - 0x80) * 2 + which;
    (t::TONE_INDEX[i as usize] as i32 - 1) as i16 as i32
}

/// 0x08055628: raise the 56 per-harmonic values `arr` (the block's halfwords 8..0x40) around
/// harmonic `idx` towards a ramp: `arr[k] = max(arr[k], v_k)` where `v` starts at `val`
/// (floored at -0x77ff) on harmonic idx-1 going down and again on idx+1 going up, and each
/// next step is 0x400 - previous (floored at -0x77ff). Only used with a nonzero `flag`
/// in 0x08055808, which this decoder never passes.
pub fn raise_levels(arr: &mut [i16], idx: i32, val: i32) {
    let idx = idx as u16;
    let val = val.max(-0x77ff);
    let mut k = idx.wrapping_sub(1) as i16 as i32;
    let mut v = val;
    let mut cnt = idx.wrapping_sub(1);
    if k >= 0 {
        loop {
            let nxt = qsub(0x0400_0000, v << 16) >> 16;
            if (arr[k as usize] as i32) < v {
                arr[k as usize] = v as i16;
            }
            cnt = cnt.wrapping_sub(1);
            v = nxt.max(-0x77ff);
            k -= 1;
            if cnt & 0x8000 != 0 {
                break;
            }
        }
    }
    let mut j = idx.wrapping_add(1) as i16 as i32;
    let mut v = val;
    while j <= 0x37 {
        let nxt = qsub(0x0400_0000, v << 16) >> 16;
        if v > arr[j as usize] as i32 {
            arr[j as usize] = v as i16;
        }
        v = nxt.max(-0x77ff);
        j += 1;
    }
}

/// 0x08055808: build a tone / silence parameter block from the tone index `idx`
/// (`blk` = [kind 3, idx, harmonic count, ...]). Class 0 (see `tone_class`) is a single tone,
/// classes 1..=3 two tones. `a2` and `a3` are the two amplitude values (single tone: `a3`
/// only), clamped to -0x5000..=0x6fff (0x77ff for a single tone) and stored at the tone's
/// harmonic positions in the 56 values at `blk[8..]`. With `flag != 0` the levels are also
/// smoothed with `raise_levels` (not used by this decoder).
///
/// Returns 0, or 4 (and sets `blk[0] = 0`) when `idx` is not a valid tone index.
pub fn tone_block(blk: &mut [i16], idx: i16, a2: i32, a3: i32, flag: i32) -> i32 {
    let flag = flag as i16 as i32;
    blk[0] = 3;
    blk[7] = 0;
    blk[4] = 1;
    blk[5] = 0;
    math::fill(&mut blk[8..8 + 0x38], 0xb001u16 as i16);
    let idx32 = idx as i32;
    let u = idx as u16;
    let class;
    if u.wrapping_sub(5) <= 0x75 {
        // Single tone.
        let h = tone_harmonic(0, idx32, 0);
        blk[1] = idx;
        blk[3] = h as i16;
        let r = math::div_half(idx32 << 20, ((h << 24).wrapping_add(0x0100_0000)) >> 16);
        blk[6] = r as i16;
        let level = a3.max(-0x5000).min(0x77ff);
        blk[2] = (h + 1).clamp(9, 0x38) as i16;
        blk[8 + h as usize] = level as i16;
        if flag != 0 {
            let v = qsub(0x5000_0000, (level as u16 as i32) << 16) >> 16;
            raise_levels(&mut blk[8..8 + 0x38], h, v);
        }
        return 0;
    } else if u.wrapping_sub(0x80) <= 0xf {
        class = 1;
    } else if u.wrapping_sub(0x90) <= 0xf {
        class = 2;
    } else if u.wrapping_sub(0xa0) <= 3 {
        class = 3;
    } else {
        blk[0] = 0;
        return 4;
    }
    let t_idx = (u.wrapping_sub(0x80)) as i16 as usize;
    let first = tone_harmonic(class, idx32, 0);
    let second = tone_harmonic(class, idx32, 1);
    blk[1] = idx;
    let s16 = second as u16 as i32;
    let count = ((s16 + 1) as i16 as i32).clamp(9, 0x38);
    blk[3] = (s16 + (first << 8)) as i16;
    blk[6] = t::TONE_FLAGS[t_idx] as i16;
    blk[2] = count as i16;
    if flag == 0 {
        let lo = -0x5000;
        let v1 = a2.max(lo).min(0x6fff);
        let v2 = a3.max(lo).min(0x6fff);
        blk[8 + first as usize] = v1 as i16;
        blk[8 + second as usize] = v2 as i16;
        0
    } else {
        let level = t::TONE_LEVEL[t_idx] as i32;
        let sum = qadd(a2 << 15, a3 << 15) >> 16 << 16;
        let base = level << 16;
        let m1 = (qsub(base, sum) >> 16).max(-0x5000).min(0x6fff);
        blk[8 + first as usize] = m1 as i16;
        let m2 = (qadd(sum, base) >> 16).max(-0x5000);
        raise_levels(&mut blk[8..8 + 0x38], first, qsub(0x5000_0000, (m1 as i16 as i32) << 16) >> 16);
        let m2 = m2.min(0x6fff);
        blk[8 + second as usize] = m2 as i16;
        raise_levels(&mut blk[8..8 + 0x38], second, qsub(0x5000_0000, (m2 as i16 as i32) << 16) >> 16);
        0
    }
}

/// 0x0805d618: decode a tone / silence frame. Reads from the bit cursor `bits`: 6 bits
/// (dropped), 6 bits (the high part of a level), then `(w[2] - 13) >> 3` bytes of 8 bits (the
/// tone index byte repeated, with redundancy check), then 1 bit. The index byte is accepted
/// when the high nibbles of the first two bytes agree and the other bytes differ from the first
/// in at most 5 bit positions in all; otherwise 0xffff is used. The level
/// is `((7 bits) * 2 - 17) * 0x78f2 >> 8` (both amplitudes). Calls `tone_block` for `blk`
/// and, if it returns 0, copies the result to `copy_to` with `copy_params_head`.
/// Returns the `tone_block` result.
pub fn tone_frame(blk: &mut [i16], copy_to: &mut [i16], bits: &mut Bits, w: &[i16]) -> i32 {
    bits.read(6);
    let hi = bits.read(6);
    let mut local = [0i16; 8];
    let cnt = ((w[2] as i32) - 0xd) >> 3;
    let hi = hi << 1;
    let mut sum = 0i32;
    let first;
    if cnt > 0 {
        for v in local.iter_mut().take(cnt as usize) {
            *v = bits.read(8) as i16;
        }
        first = local[0] as i32;
        if cnt > 1 {
            for &v in &local[1..cnt as usize] {
                sum += t::POPCOUNT8[((v as i32 ^ first) & 0xff) as usize] as i32 + t::POPCOUNT8[0] as i32;
            }
        }
    } else {
        first = local[0] as i32;
    }
    let ok = ((local[0] ^ local[1]) as u16 & 0xf0) == 0 && sum <= 5;
    let idx = if ok { first as u16 as i16 } else { -1 };
    let lo = bits.read(1);
    let x = ((lo | hi) << 17).wrapping_sub(0x0011_0000);
    let p = smultb(x, 0x78f2);
    let level = (p >> 8) as i16 as i32;
    let r = tone_block(blk, idx, level, level, 0);
    if r == 0 {
        copy_params_head(copy_to, blk);
    }
    r
}

/// 0x0805573c: pitch-history update and "marker" detection. Shifts the 17-entry history
/// `hist[1..=17]` down and appends `*p` (the new pitch index). `hist[0]` is a countdown:
/// while it is positive and `*p` is in 0x13..=0x16 the index is replaced by 0x15 minus a bit
/// of a fixed 32-bit pattern (and the countdown decremented, returning 1). Otherwise the countdown
/// is cleared, and if the history matches one of the two stored 17-entry patterns (`mode == 1`
/// selects the first, anything else the second) in all but at most one entry, it is set to 0x20.
/// An index of 0xff clears the countdown immediately. Returns 1 only when `*p` was replaced.
pub fn pitch_history(p: &mut i16, hist: &mut [i16], mode: i32) -> i32 {
    let tbl = &t::PITCH_PATTERNS;
    let k = ((tbl[0x28] as u32) << 24) | ((tbl[0x29] as u32) << 16) | ((tbl[0x2a] as u32) << 8) | tbl[0x2b] as u32;
    hist.copy_within(2..18, 1);
    hist[0x11] = *p;
    let base = if mode != 1 { 0x14 } else { 0 };
    let pv = *p as i32;
    if pv == 0xff {
        hist[0] = 0;
        return 0;
    }
    if hist[0] > 0 && pv > 0x12 && pv <= 0x16 {
        let sh = 0x20 - hist[0] as i32;
        let bit = asr((k ^ 0x3c67_0ce3) as i32, sh) & 1;
        *p = (0x15 - bit) as i16;
        hist[0] -= 1;
        return 1;
    }
    hist[0] = 0;
    let mut miss = 0x11i32;
    for i in 0..0x11 {
        if hist[1 + i] as i32 == tbl[base + i] as i32 {
            miss -= 1;
        }
    }
    if miss <= 1 {
        hist[0] = 0x20;
    }
    0
}

/// 0x08055cf4: copy a whole parameter block (halfwords 0..0x40 and the word at 0x42..0x44) and
/// clear the voicing pointer (halfwords 0x40..0x42). `dst` and `src` are 0x44-halfword blocks.
pub fn copy_params(dst: &mut [i16], src: &[i16]) {
    dst[..8].copy_from_slice(&src[..8]);
    dst[0x42] = src[0x42];
    dst[0x43] = src[0x43];
    dst[0x40] = 0;
    dst[0x41] = 0;
    dst[8..8 + 0x38].copy_from_slice(&src[8..8 + 0x38]);
}

/// 0x08061440: copy halfwords 0..0x40 of a parameter block (everything but the voicing pointer
/// and the frame counter).
pub fn copy_params_head(dst: &mut [i16], src: &[i16]) {
    dst[..8].copy_from_slice(&src[..8]);
    dst[8..8 + 0x38].copy_from_slice(&src[8..8 + 0x38]);
}

/// 0x0805c7e4: increment the frame counter (halfword 0x43) of a parameter block up to 100.
pub fn bump_frame_counter(blk: &mut [i16]) {
    if blk[0x43] <= 99 {
        blk[0x43] += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ambe::state::{PARAMS_WORDS, State, off};
    use crate::ambe::testvec::{self, Record, STATE_ADDR, assert_h_eq, assert_state_eq};

    /// Replay every recorded call of the routine at `addr`; returns the number of calls.
    fn each(addr: u32, mut f: impl FnMut(&Record)) -> usize {
        let recs = testvec::load(addr).unwrap_or_default();
        for r in &recs {
            f(r);
        }
        eprintln!("{addr:#010x}: {} recorded calls", recs.len());
        recs.len()
    }

    /// State comparison ignores the voicing-pointer halfwords (0x40..0x42) of the parameter
    /// blocks the state holds (0x88 bytes apart from 0x3ec).
    fn ignore() -> Vec<(usize, usize)> {
        (0..7).map(|k| (off::PARAMS_CUR + 0x88 * k + 0x80, 4)).collect()
    }
    fn in_state(ptr: u32) -> bool {
        (STATE_ADDR..STATE_ADDR + 0x7ec).contains(&ptr)
    }
    /// A parameter block argument, before the call.
    fn blk(r: &Record, ptr: u32) -> Vec<i16> {
        r.h_before(ptr, PARAMS_WORDS)
    }
    /// Compare a parameter block with its recorded "after" image, ignoring the voicing pointer.
    #[track_caller]
    fn check_blk(what: &str, r: &Record, ptr: u32, got: &[i16]) {
        let mut want = r.h_after(ptr, PARAMS_WORDS);
        let mut got = got.to_vec();
        for v in [0x40, 0x41] {
            want[v] = 0;
            got[v] = 0;
        }
        assert_h_eq(what, r, &got, &want);
    }
    /// Put a block that lives in the state back into `st`.
    fn store(st: &mut State, ptr: u32, v: &[i16]) {
        if in_state(ptr) {
            st.s_mut(ptr as usize - STATE_ADDR as usize, v.len()).copy_from_slice(v);
        }
    }
    #[track_caller]
    fn check_state(r: &Record, st: &State) {
        assert_state_eq(r, st, &r.state_after(), &ignore());
    }

    /// Branch tally printed by the replay tests (visible with `--nocapture`).
    #[derive(Default)]
    struct Tally(std::collections::BTreeMap<String, usize>);
    impl Tally {
        fn hit(&mut self, k: impl Into<String>) {
            *self.0.entry(k.into()).or_default() += 1;
        }
        fn show(&self, name: &str) {
            eprintln!("coverage {name}: {:?}", self.0);
        }
    }

    #[test]
    fn field_widths_match_firmware() {
        let mut tl = Tally::default();
        let n = each(0x0805_5170, |r| {
            tl.hit(format!("n={}", (r.r[2] as i32).clamp(0, 4)));
            let mut st = r.state_before();
            let o = r.state_off(r.r[0]);
            assert_eq!(o, WIDTHS);
            assert_eq!(r.r[1] as i16, st.h(o), "enable argument is the table's first halfword");
            field_widths(st.s_mut(o, WIDTHS_LEN), r.r[2] as i32);
            check_state(r, &st);
        });
        tl.show("field_widths");
        assert!(n > 0);
    }

    #[test]
    fn bit_reorder_matches_firmware() {
        let n = each(0x0805_cc98, |r| {
            assert_eq!(r.r[1], 1, "only the reorder direction is ported");
            let st = r.state_before();
            let w = st.s(r.state_off(r.r[2]), WIDTHS_LEN).to_vec();
            let mut bits = r.h_before(r.r[0], 49);
            bit_reorder(&mut bits, &w);
            assert_h_eq("bits", r, &bits, &r.h_after(r.r[0], 49));
        });
        assert!(n > 0);
    }

    #[test]
    fn band_flags_match_firmware() {
        let mut tl = Tally::default();
        let n = each(0x0805_d0ec, |r| {
            tl.hit(format!("flag={}", r.r[0] as i32));
            let (a, b) = band_flags(r.r[0] as i32, r.r[3] as i16 as i32, r.stack[0] as i32);
            assert_eq!(a, r.w_after(r.r[1], 1)[0], "first word");
            assert_eq!(b, r.w_after(r.r[2], 1)[0], "second word");
        });
        tl.show("band_flags");
        assert!(n > 0);
    }

    #[test]
    fn dequant_step_matches_firmware() {
        let n = each(0x0805_c368, |r| {
            assert_eq!(r.stack[0], 0, "only mode 0 is ported");
            let v = r.h_before(r.r[0], 1)[0] as u16 as i32;
            assert_eq!(dequant_step(v, r.r[1] as i32), r.ret[0] as i32);
        });
        assert!(n > 0);
    }

    #[test]
    fn pair_levels_match_firmware() {
        let mut tl = Tally::default();
        let n = each(0x0805_c47c, |r| {
            assert_eq!(r.stack[2], 0, "only mode 0 is ported");
            let idx = r.h_before(r.r[0], 1)[0];
            let (mut b, mut a) = (blk(r, r.r[1]), blk(r, r.r[2]));
            let (wb, wa) = (band_word(&b), band_word(&a));
            tl.hit(if (wa | wb) & 0x5555_5555 != 0 {
                "low-bit fail".to_string()
            } else {
                format!("b_has={} a_has={} shift={}", wb & 0xaaaa_aaaau32 as i32 != 0, wa & 0xaaaa_aaaau32 as i32 != 0, r.stack[1])
            });
            let ok = pair_levels(idx, &mut b, &mut a, r.stack[1] as i32);
            assert_eq!(ok as u32, r.ret[0], "result");
            check_blk("block B", r, r.r[1], &b);
            check_blk("block A", r, r.r[2], &a);
            assert_eq!(r.h_after(r.r[0], 1)[0], idx, "index halfword is not written");
        });
        tl.show("pair_levels");
        assert!(n > 0);
    }

    #[test]
    fn pitch_params_match_firmware() {
        let mut tl = Tally::default();
        let n = each(0x0805_d708, |r| {
            tl.hit(format!("kind={} flag={}", r.r[0] as i32, r.stack[1] as i16));
            let base = r.r[2] - 4;
            assert_eq!(r.r[1], base + 12);
            let mut b = blk(r, base);
            assert_eq!(r.r[0] as i16, b[0]);
            pitch_params(&mut b, r.r[3] as i16 as i32, r.stack[0] as i32, r.stack[1] as i16 as i32);
            check_blk("block", r, base, &b);
        });
        tl.show("pitch_params");
        assert!(n > 0);
    }

    #[test]
    fn tone_class_and_harmonic_match_firmware() {
        let mut tl = Tally::default();
        let n = each(0x0805_56bc, |r| {
            tl.hit(format!("class={}", tone_class(r.r[0] as i32)));
            assert_eq!(tone_class(r.r[0] as i32), r.ret[0] as i16 as i32)
        });
        tl.show("tone_class");
        assert!(n > 0);
        let mut tl = Tally::default();
        let n = each(0x0805_56fc, |r| {
            tl.hit(format!("class={} which={}", r.r[0] as i32, r.r[2] as i32));
            assert_eq!(tone_harmonic(r.r[0] as i32, r.r[1] as i32, r.r[2] as i32), r.ret[0] as i16 as i32);
        });
        tl.show("tone_harmonic");
        assert!(n > 0);
    }

    #[test]
    fn tone_block_matches_firmware() {
        let mut tl = Tally::default();
        let n = each(0x0805_5808, |r| {
            tl.hit(format!("class={} flag={}", tone_class(r.r[1] as i32), r.stack[0] as i16));
            let mut b = blk(r, r.r[0]);
            let res = tone_block(&mut b, r.r[1] as i16, r.r[2] as i32, r.r[3] as i32, r.stack[0] as i32);
            assert_eq!(res, r.ret[0] as i16 as i32);
            check_blk("block", r, r.r[0], &b);
        });
        tl.show("tone_block");
        assert!(n > 0);
    }

    #[test]
    fn raise_levels_match_firmware() {
        let n = each(0x0805_5628, |r| {
            let mut v = r.h_before(r.r[0], 0x38);
            raise_levels(&mut v, r.r[1] as u16 as i32, r.r[2] as i32);
            assert_h_eq("levels", r, &v, &r.h_after(r.r[0], 0x38));
        });
        eprintln!("raise_levels is unreachable with the decoder's flag argument: {n} calls");
    }

    #[test]
    fn tone_frame_matches_firmware() {
        let mut tl = Tally::default();
        let n = each(0x0805_d618, |r| {
            let mut st = r.state_before();
            let w = st.s(r.state_off(r.r[3]), WIDTHS_LEN).to_vec();
            let p = r.w_before(r.r[2], 1)[0] as u32;
            let bits = r.h_before(p, 49);
            let mut cur = Bits::new(&bits);
            let (mut a, mut b) = (blk(r, r.r[0]), blk(r, r.r[1]));
            let res = tone_frame(&mut a, &mut b, &mut cur, &w);
            tl.hit(format!("result={res} idx={}", a[1]));
            assert_eq!(res, r.ret[0] as i32);
            assert_eq!(p + 2 * cur.pos as u32, r.w_after(r.r[2], 1)[0] as u32, "cursor");
            check_blk("block A", r, r.r[0], &a);
            check_blk("block B", r, r.r[1], &b);
            store(&mut st, r.r[0], &a);
            store(&mut st, r.r[1], &b);
            check_state(r, &st);
        });
        tl.show("tone_frame");
        assert!(n > 0);
    }

    #[test]
    fn pitch_history_matches_firmware() {
        let mut tl = Tally::default();
        let n = each(0x0805_573c, |r| {
            let mut st = r.state_before();
            let o = r.state_off(r.r[1]);
            assert_eq!(o, PITCH_HISTORY);
            let mut p = r.h_before(r.r[0], 1)[0];
            let (c0, p0) = (st.h(o), p);
            let res = pitch_history(&mut p, st.s_mut(o, PITCH_HISTORY_LEN), r.r[2] as i32);
            tl.hit(if p0 == 0xff {
                "0xff".to_string()
            } else if res == 1 {
                "replaced".to_string()
            } else if st.h(o) == 0x20 {
                "armed".to_string()
            } else {
                format!("nothing (count was {})", c0.signum())
            });
            assert_eq!(res, r.ret[0] as i32);
            assert_eq!(p, r.h_after(r.r[0], 1)[0]);
            check_state(r, &st);
        });
        tl.show("pitch_history");
        assert!(n > 0);
    }

    #[test]
    fn copy_params_match_firmware() {
        let n = each(0x0805_5cf4, |r| {
            let mut st = r.state_before();
            let (mut d, s) = (blk(r, r.r[0]), blk(r, r.r[1]));
            copy_params(&mut d, &s);
            check_blk("dst", r, r.r[0], &d);
            store(&mut st, r.r[0], &d);
            check_state(r, &st);
        });
        assert!(n > 0);
        let n = each(0x0806_1440, |r| {
            let mut st = r.state_before();
            let (mut d, s) = (blk(r, r.r[0]), blk(r, r.r[1]));
            copy_params_head(&mut d, &s);
            check_blk("dst", r, r.r[0], &d);
            store(&mut st, r.r[0], &d);
            check_state(r, &st);
        });
        assert!(n > 0);
    }

    #[test]
    fn bump_frame_counter_matches_firmware() {
        let n = each(0x0805_c7e4, |r| {
            let mut b = blk(r, r.r[0]);
            bump_frame_counter(&mut b);
            check_blk("block", r, r.r[0], &b);
        });
        assert!(n > 0);
    }

    #[test]
    fn spread_pairs_example() {
        assert_eq!(spread_pairs(0b11), (0xf000_0000u32 as i32, 0));
        assert_eq!(spread_pairs(0x0001_0000), (0, 0x5000_0000));
        assert_eq!(spread_pairs(0b10 << 14), (0xa, 0));
    }
}
