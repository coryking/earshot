//! The decoder's persistent state, laid out exactly as the firmware keeps it in RAM.
//!
//! The firmware passes one pointer (`ambe_mystery`, 0x20011224) to a 0x7ec-byte block and every
//! routine reaches its fields by byte offset from it. The port keeps that layout: `State` is the
//! same block as 16-bit words, and fields are addressed by the firmware's byte offsets (named in
//! [`off`]). That keeps each ported routine readable next to its disassembly and lets the tests
//! compare the whole block with the firmware's RAM after any call.

use super::tables::state::STATE_INIT;

/// Size of the block in 16-bit words (0x7ec bytes).
pub const STATE_WORDS: usize = 0x7ec / 2;

/// Byte offsets of the fields the port names. Offsets are relative to the start of the block.
pub mod off {
    /// Parameter block for the frame being synthesized (0x44 words); the second half-frame
    /// call synthesizes from it.
    pub const PARAMS_CUR: usize = 0x3ec;
    /// Parameter block of the previous frame (0x44 words).
    pub const PARAMS_PREV: usize = 0x474;
    /// Parameter block kept for frame repeats (0x44 words).
    pub const PARAMS_REPEAT: usize = 0x4fc;
    /// Frame history flags; bit 0 set when the current frame is a repeat.
    pub const HISTORY: usize = 0x7be;
}

/// Words in one parameter block.
pub const PARAMS_WORDS: usize = 0x44;

#[derive(Clone)]
pub struct State {
    pub(crate) m: [i16; STATE_WORDS],
}

impl Default for State {
    fn default() -> Self {
        Self { m: STATE_INIT }
    }
}

impl State {
    /// Signed 16-bit field at byte offset `off`.
    #[inline(always)]
    pub fn h(&self, off: usize) -> i16 {
        self.m[off / 2]
    }
    #[inline(always)]
    pub fn set_h(&mut self, off: usize, v: i16) {
        self.m[off / 2] = v;
    }
    /// 32-bit field at byte offset `off` (halfword aligned, little endian).
    #[inline(always)]
    pub fn w(&self, off: usize) -> i32 {
        (self.m[off / 2] as u16 as i32) | ((self.m[off / 2 + 1] as i32) << 16)
    }
    #[inline(always)]
    pub fn set_w(&mut self, off: usize, v: i32) {
        self.m[off / 2] = v as i16;
        self.m[off / 2 + 1] = (v >> 16) as i16;
    }
    /// `n` words starting at byte offset `off`.
    #[inline(always)]
    pub fn s(&self, off: usize, n: usize) -> &[i16] {
        &self.m[off / 2..off / 2 + n]
    }
    #[inline(always)]
    pub fn s_mut(&mut self, off: usize, n: usize) -> &mut [i16] {
        &mut self.m[off / 2..off / 2 + n]
    }
    /// The block as little-endian bytes (what the firmware has in RAM).
    #[cfg(test)]
    pub fn to_bytes(&self) -> Vec<u8> {
        self.m.iter().flat_map(|v| v.to_le_bytes()).collect()
    }
    #[cfg(test)]
    pub fn from_bytes(b: &[u8]) -> Self {
        let mut s = Self::default();
        for (i, w) in s.m.iter_mut().enumerate() {
            *w = i16::from_le_bytes([b[2 * i], b[2 * i + 1]]);
        }
        s
    }
}
