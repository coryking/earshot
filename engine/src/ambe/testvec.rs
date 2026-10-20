//! Firmware call recordings for per-routine tests (test builds only).
//!
//! `scripts/ambe-capture.py` runs the firmware decoder offline under an emulator and records
//! calls of one routine: argument registers, stack arguments, return registers, and the decoder
//! state plus live stack before and after the call. Recordings live in
//! `.native-md380/vectors/<address>.rec` (Git-ignored, derived from the firmware); the tests
//! that use them skip when they are absent. The committed regression tests in `tests/` need no
//! recordings.

use std::path::PathBuf;

/// A memory region captured before and after the call.
pub struct Region {
    pub addr: u32,
    pub before: Vec<u8>,
    pub after: Vec<u8>,
}

pub struct Record {
    pub frame: u32,
    pub call: u32,
    /// r0..r3 at entry.
    pub r: [u32; 4],
    pub sp: u32,
    /// The first 16 words on the stack at entry: stack arguments 5, 6, ... are `stack[0]`, ...
    pub stack: [u32; 16],
    /// r0 and r1 at return.
    pub ret: [u32; 2],
    pub regions: Vec<Region>,
}

/// Base address of the decoder state block in the firmware's RAM.
pub const STATE_ADDR: u32 = 0x2001_1224;

pub fn dir() -> PathBuf {
    std::env::var_os("AMBE_VECTORS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".native-md380/vectors"))
}

/// Recorded calls of the routine at `addr`, or None (test should skip) without recordings.
pub fn load(addr: u32) -> Option<Vec<Record>> {
    let path = dir().join(format!("{addr:08x}.rec"));
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(_) => {
            // Development crates set AMBE_REQUIRE_VECTORS so a missing recording is an error.
            assert!(std::env::var_os("AMBE_REQUIRE_VECTORS").is_none(), "no recordings at {}", path.display());
            eprintln!("no recordings at {} - skipping", path.display());
            return None;
        }
    };
    let mut p = 0usize;
    let u32_at = |p: &mut usize| {
        let v = u32::from_le_bytes(data[*p..*p + 4].try_into().unwrap());
        *p += 4;
        v
    };
    let mut out = Vec::new();
    while p < data.len() {
        assert_eq!(&data[p..p + 4], b"REC1", "corrupt recording {}", path.display());
        p += 4;
        let frame = u32_at(&mut p);
        let call = u32_at(&mut p);
        let mut r = [0; 4];
        for v in &mut r {
            *v = u32_at(&mut p);
        }
        let sp = u32_at(&mut p);
        let mut stack = [0; 16];
        for v in &mut stack {
            *v = u32_at(&mut p);
        }
        let ret = [u32_at(&mut p), u32_at(&mut p)];
        let n = u32_at(&mut p);
        let mut regions = Vec::new();
        for _ in 0..n {
            let addr = u32_at(&mut p);
            let len = u32_at(&mut p) as usize;
            let before = data[p..p + len].to_vec();
            let after = data[p + len..p + 2 * len].to_vec();
            p += 2 * len;
            regions.push(Region { addr, before, after });
        }
        out.push(Record { frame, call, r, sp, stack, ret, regions });
    }
    Some(out)
}

impl Record {
    fn find(&self, addr: u32, len: usize) -> (&Region, usize) {
        for g in &self.regions {
            if addr >= g.addr && (addr - g.addr) as usize + len <= g.before.len() {
                return (g, (addr - g.addr) as usize);
            }
        }
        panic!("address {addr:#x}+{len} not captured");
    }
    /// `n` halfwords at `addr` before / after the call.
    pub fn h_before(&self, addr: u32, n: usize) -> Vec<i16> {
        let (g, o) = self.find(addr, 2 * n);
        g.before[o..o + 2 * n].chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect()
    }
    pub fn h_after(&self, addr: u32, n: usize) -> Vec<i16> {
        let (g, o) = self.find(addr, 2 * n);
        g.after[o..o + 2 * n].chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect()
    }
    /// `n` words at `addr` before / after the call.
    pub fn w_before(&self, addr: u32, n: usize) -> Vec<i32> {
        let (g, o) = self.find(addr, 4 * n);
        g.before[o..o + 4 * n].chunks(4).map(|c| i32::from_le_bytes(c.try_into().unwrap())).collect()
    }
    pub fn w_after(&self, addr: u32, n: usize) -> Vec<i32> {
        let (g, o) = self.find(addr, 4 * n);
        g.after[o..o + 4 * n].chunks(4).map(|c| i32::from_le_bytes(c.try_into().unwrap())).collect()
    }
    /// The decoder state block before / after the call.
    pub fn state_before(&self) -> super::state::State {
        let (g, o) = self.find(STATE_ADDR, 0x7ec);
        super::state::State::from_bytes(&g.before[o..o + 0x7ec])
    }
    pub fn state_after(&self) -> super::state::State {
        let (g, o) = self.find(STATE_ADDR, 0x7ec);
        super::state::State::from_bytes(&g.after[o..o + 0x7ec])
    }
    /// Argument `i` (0-based) of the call: r0..r3, then the stack.
    pub fn arg(&self, i: usize) -> u32 {
        if i < 4 { self.r[i] } else { self.stack[i - 4] }
    }
    /// Byte offset into the state block of a pointer argument.
    pub fn state_off(&self, ptr: u32) -> usize {
        assert!((STATE_ADDR..STATE_ADDR + 0x7ec).contains(&ptr), "{ptr:#x} is not in the state block");
        (ptr - STATE_ADDR) as usize
    }
}

/// Compare two halfword slices, reporting the first difference.
#[track_caller]
pub fn assert_h_eq(what: &str, rec: &Record, got: &[i16], want: &[i16]) {
    if got != want {
        let i = got.iter().zip(want).position(|(a, b)| a != b).unwrap_or(got.len().min(want.len()));
        panic!(
            "{what}: frame {} call {}: first difference at word {i} ({:#x} bytes): got {:?} want {:?}",
            rec.frame,
            rec.call,
            i * 2,
            &got[i.saturating_sub(2)..(i + 6).min(got.len())],
            &want[i.saturating_sub(2)..(i + 6).min(want.len())]
        );
    }
}

/// Compare decoder state blocks, ignoring the byte ranges in `ignore` (e.g. fields that hold
/// firmware stack pointers).
#[track_caller]
pub fn assert_state_eq(rec: &Record, got: &super::state::State, want: &super::state::State, ignore: &[(usize, usize)]) {
    for i in 0..super::state::STATE_WORDS {
        let off = 2 * i;
        if ignore.iter().any(|&(a, n)| off >= a && off < a + n) {
            continue;
        }
        if got.m[i] != want.m[i] {
            panic!(
                "state: frame {} call {}: first difference at offset {off:#x}: got {} want {}",
                rec.frame, rec.call, got.m[i], want.m[i]
            );
        }
    }
}
