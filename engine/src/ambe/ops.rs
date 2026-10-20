//! Cortex-M4 integer and DSP-extension instruction semantics, as the firmware uses them.
//!
//! Every helper here is the exact architectural behaviour of one ARMv7E-M instruction (or a
//! short idiom the compiler emits), so ported code can follow the firmware instruction for
//! instruction where the arithmetic matters. "Halfword" operands are the low (b) or high (t)
//! signed 16 bits of a 32-bit register. The Q (saturation) flag is never read by the decoder,
//! so it is not modelled.

/// Low signed halfword of a register.
#[inline(always)]
pub fn lo(x: i32) -> i32 {
    x as i16 as i32
}
/// High signed halfword of a register.
#[inline(always)]
pub fn hi(x: i32) -> i32 {
    x >> 16
}

/// Saturate a 64-bit value to i32 (the saturation every Q-instruction applies).
#[inline(always)]
pub fn sat32(x: i64) -> i32 {
    x.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}
/// Saturate to i16.
#[inline(always)]
pub fn sat16(x: i32) -> i32 {
    x.clamp(i16::MIN as i32, i16::MAX as i32)
}
/// SSAT #bits: signed saturate to a `bits`-bit range.
#[inline(always)]
pub fn ssat(x: i32, bits: u32) -> i32 {
    let max = (1i32 << (bits - 1)) - 1;
    x.clamp(-max - 1, max)
}
/// QADD: saturating 32-bit add.
#[inline(always)]
pub fn qadd(a: i32, b: i32) -> i32 {
    a.saturating_add(b)
}
/// QSUB: saturating 32-bit subtract (a - b).
#[inline(always)]
pub fn qsub(a: i32, b: i32) -> i32 {
    a.saturating_sub(b)
}
/// QDADD: a + sat(2*b), both saturating.
#[inline(always)]
pub fn qdadd(a: i32, b: i32) -> i32 {
    a.saturating_add(b.saturating_mul(2))
}
/// QDSUB: a - sat(2*b), both saturating.
#[inline(always)]
pub fn qdsub(a: i32, b: i32) -> i32 {
    a.saturating_sub(b.saturating_mul(2))
}

/// SMULBB / SMULBT / SMULTB / SMULTT: 16x16 signed multiplies of the selected halves.
#[inline(always)]
pub fn smulbb(a: i32, b: i32) -> i32 {
    lo(a) * lo(b)
}
#[inline(always)]
pub fn smulbt(a: i32, b: i32) -> i32 {
    lo(a) * hi(b)
}
#[inline(always)]
pub fn smultb(a: i32, b: i32) -> i32 {
    hi(a) * lo(b)
}
#[inline(always)]
pub fn smultt(a: i32, b: i32) -> i32 {
    hi(a) * hi(b)
}
/// SMLABB: acc + lo(a)*lo(b), wrapping.
#[inline(always)]
pub fn smlabb(a: i32, b: i32, acc: i32) -> i32 {
    acc.wrapping_add(smulbb(a, b))
}
/// SMUAD: lo*lo + hi*hi, wrapping (overflow only sets Q).
#[inline(always)]
pub fn smuad(a: i32, b: i32) -> i32 {
    smulbb(a, b).wrapping_add(smultt(a, b))
}
/// SMUADX: lo(a)*hi(b) + hi(a)*lo(b), wrapping.
#[inline(always)]
pub fn smuadx(a: i32, b: i32) -> i32 {
    smulbt(a, b).wrapping_add(smultb(a, b))
}
/// SMUSD: lo*lo - hi*hi, wrapping.
#[inline(always)]
pub fn smusd(a: i32, b: i32) -> i32 {
    smulbb(a, b).wrapping_sub(smultt(a, b))
}
/// SMLALD: 64-bit accumulate of lo*lo + hi*hi.
#[inline(always)]
pub fn smlald(a: i32, b: i32, acc: i64) -> i64 {
    acc.wrapping_add(smulbb(a, b) as i64).wrapping_add(smultt(a, b) as i64)
}
/// SMULL: full signed 32x32 -> 64 product.
#[inline(always)]
pub fn smull(a: i32, b: i32) -> i64 {
    a as i64 * b as i64
}
/// SMLAL: 64-bit accumulate of a signed 32x32 product.
#[inline(always)]
pub fn smlal(a: i32, b: i32, acc: i64) -> i64 {
    acc.wrapping_add(smull(a, b))
}
/// UMULL: full unsigned 32x32 -> 64 product.
#[inline(always)]
pub fn umull(a: u32, b: u32) -> u64 {
    a as u64 * b as u64
}

/// Apply a halfword-wise operation to both halves and repack.
#[inline(always)]
fn per_half(a: i32, b: i32, f: impl Fn(i32, i32) -> i32) -> i32 {
    let l = f(lo(a), lo(b)) & 0xffff;
    let h = f(hi(a), hi(b)) << 16;
    h | l
}
/// QADD16 / QSUB16: per-halfword saturating add / subtract.
#[inline(always)]
pub fn qadd16(a: i32, b: i32) -> i32 {
    per_half(a, b, |x, y| sat16(x + y))
}
#[inline(always)]
pub fn qsub16(a: i32, b: i32) -> i32 {
    per_half(a, b, |x, y| sat16(x - y))
}
/// SHADD16 / SHSUB16: per-halfword halving add / subtract.
#[inline(always)]
pub fn shadd16(a: i32, b: i32) -> i32 {
    per_half(a, b, |x, y| (x + y) >> 1)
}
#[inline(always)]
pub fn shsub16(a: i32, b: i32) -> i32 {
    per_half(a, b, |x, y| (x - y) >> 1)
}
/// PKHBT a, b, LSL #sh: bottom half of a, top half of (b << sh).
#[inline(always)]
pub fn pkhbt(a: i32, b: i32, sh: u32) -> i32 {
    (a & 0xffff) | ((b << sh) & !0xffff)
}
/// PKHTB a, b, ASR #sh: top half of a, bottom half of (b >> sh).
#[inline(always)]
pub fn pkhtb(a: i32, b: i32, sh: u32) -> i32 {
    (a & !0xffff) | ((b >> sh) & 0xffff)
}
/// Pack two halfwords into one register (low, high).
#[inline(always)]
pub fn pack(low: i32, high: i32) -> i32 {
    (low & 0xffff) | (high << 16)
}

/// SDIV: truncating signed divide; division by zero gives 0 (DIV_0_TRP clear) and
/// i32::MIN / -1 gives i32::MIN.
#[inline(always)]
pub fn sdiv(a: i32, b: i32) -> i32 {
    if b == 0 { 0 } else { a.wrapping_div(b) }
}
/// CLZ.
#[inline(always)]
pub fn clz(x: u32) -> i32 {
    x.leading_zeros() as i32
}
/// The firmware's normalisation idiom: the left shift that puts the first bit that differs
/// from the sign bit just below the sign bit (0 for x == 0, 31 for x == -1).
#[inline(always)]
pub fn norm32(x: i32) -> i32 {
    if x == 0 { 0 } else { clz((x ^ (x >> 31)) as u32) - 1 }
}
/// LSL by a register amount as the hardware does it (amounts 32..255 give 0).
#[inline(always)]
pub fn lsl(x: i32, sh: i32) -> i32 {
    let s = (sh & 0xff) as u32;
    if s >= 32 { 0 } else { ((x as u32) << s) as i32 }
}
/// ASR by a register amount as the hardware does it (amounts 32..255 give the sign).
#[inline(always)]
pub fn asr(x: i32, sh: i32) -> i32 {
    let s = (sh & 0xff) as u32;
    if s >= 32 { x >> 31 } else { x >> s }
}
/// LSR by a register amount as the hardware does it.
#[inline(always)]
pub fn lsr(x: i32, sh: i32) -> i32 {
    let s = (sh & 0xff) as u32;
    if s >= 32 { 0 } else { ((x as u32) >> s) as i32 }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn halfword_ops() {
        assert_eq!(qadd16(pack(0x7000, -0x7000), pack(0x2000, -0x2000)), pack(0x7fff, -0x8000));
        assert_eq!(shadd16(pack(3, -3), pack(0, 0)), pack(1, -2));
        assert_eq!(smuad(pack(2, 3), pack(5, 7)), 31);
        assert_eq!(smusd(pack(2, 3), pack(5, 7)), -11);
        assert_eq!(smuadx(pack(2, 3), pack(5, 7)), 2 * 7 + 3 * 5);
        assert_eq!(pkhbt(0x1234_5678, 0x0000_abcd, 16), 0xabcd_5678u32 as i32);
        assert_eq!(pkhtb(0x1234_5678, 0xabcd_0000u32 as i32, 16), 0x1234_abcd);
    }
    #[test]
    fn norm_and_div() {
        assert_eq!(norm32(0), 0);
        assert_eq!(norm32(1), 30);
        assert_eq!(norm32(-1), 31);
        assert_eq!(norm32(0x4000_0000), 0);
        assert_eq!(norm32(i32::MIN), 0);
        assert_eq!(sdiv(7, 0), 0);
        assert_eq!(sdiv(i32::MIN, -1), i32::MIN);
        assert_eq!(sdiv(-7, 2), -3);
        assert_eq!(qdadd(0x7000_0000, 0x1000_0000), i32::MAX);
    }
}
