//! End-to-end regression for the native AMBE+2 decoder (src/ambe).
//!
//! The expected outputs are the MD-380 firmware decoder's, produced offline by
//! scripts/ambe_oracle.py. The committed checks are FNV-1a hashes of the whole output per frame
//! corpus; when the oracle's raw outputs are present in .native-md380/expected/ (Git-ignored,
//! see NOTES.md) the test also reports the first differing frame and sample.
use radio_engine::ambe::Decoder;
use std::path::Path;

/// (corpus, FNV-1a 64 of the firmware's s16le output)
const EXPECTED: &[(&str, u64)] = &[
    ("fixture", 0x8181ed6d2c94c0db),
    ("fixture-concealed", 0x2e393bf158a76491),
    ("random", 0x71b073ae1fbc834c),
    ("edge", 0x7ea4e2ddf3ee3d3c),
    ("harvard-0010", 0x207c0042cbd145af),
    ("harvard-0011", 0xd0c963708cfb2c91),
    ("harvard-0030", 0xb21cef05a9a6652d),
    ("harvard-0031", 0x1b6e915e314e1350),
];

fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Decode a frame file the way the oracle does: one frame per line, "R" resets, "X <hex> <bits>"
/// passes decoder flags, '#' comments.
fn decode_corpus(name: &str) -> Vec<i16> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(root.join(format!("tests/data/ambe/{name}.frames"))).unwrap();
    let mut dec = Decoder::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() || line.starts_with('#') {
            continue;
        }
        if parts[0] == "R" {
            dec.reset();
            continue;
        }
        let flags = if parts[0] == "X" { u16::from_str_radix(parts[1], 16).unwrap() } else { 0 };
        let s = parts[parts.len() - 1].as_bytes();
        let mut bits = [0u8; 49];
        for (b, c) in bits.iter_mut().zip(s) {
            *b = c - b'0';
        }
        out.extend_from_slice(&dec.decode_flags(&bits, flags));
    }
    out
}

fn bytes(pcm: &[i16]) -> Vec<u8> {
    pcm.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// The tests run on parallel threads; the RSS check must not see the other test's buffers.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn matches_firmware_on_every_corpus() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut failures = Vec::new();
    for &(name, want) in EXPECTED {
        let pcm = decode_corpus(name);
        let got = fnv1a(&bytes(&pcm));
        if got == want {
            continue;
        }
        let mut msg = format!("{name}: output hash {got:#x}, firmware {want:#x}");
        if let Ok(raw) = std::fs::read(root.join(format!(".native-md380/expected/{name}.raw"))) {
            let fw: Vec<i16> = raw.chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
            let diff = pcm.iter().zip(&fw).position(|(a, b)| a != b);
            let max = pcm.iter().zip(&fw).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
            let bad = pcm.chunks(160).zip(fw.chunks(160)).filter(|(a, b)| a != b).count();
            msg += &format!(
                "; first difference at sample {diff:?} (frame {:?}), {bad} of {} frames differ, max |diff| {max}",
                diff.map(|i| i / 160),
                fw.len() / 160
            );
        }
        failures.push(msg);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Decoding must not grow memory: run a long stream and check RSS stays flat.
#[test]
fn rss_flat_over_a_long_stream() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    fn rss_kb() -> Option<u64> {
        let s = std::fs::read_to_string("/proc/self/status").ok()?;
        s.lines().find(|l| l.starts_with("VmRSS:"))?.split_whitespace().nth(1)?.parse().ok()
    }
    let mut dec = Decoder::new();
    let mut x: u64 = 1;
    let mut frame = || {
        let mut bits = [0u8; 49];
        for b in &mut bits {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            *b = (x >> 63) as u8;
        }
        bits
    };
    for _ in 0..2_000 {
        dec.decode(&frame());
    }
    let Some(before) = rss_kb() else { return };
    for _ in 0..20_000 {
        dec.decode(&frame());
    }
    let after = rss_kb().unwrap();
    assert!(after <= before + 512, "RSS grew from {before} kB to {after} kB over 20k frames");
}
