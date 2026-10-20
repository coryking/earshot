//! P25 Phase 1 error control used by the control channel (TIA-102.BAAA):
//! the NID BCH(63,16,23) code, the rate-1/2 trellis code with its 196-bit block
//! interleave, and the CRC-CCITT that closes every TSBK.
//!
//! Tables follow SDRTrunk (`edac/bch/BCH_63_16_23_P25_Test.java`,
//! `edac/trellis/P25_1_2_Node.java`, `phase1/P25P1Interleave.java`) and OP25
//! (`p25p1_fdma.cc` `block_deinterleave`, `crc16`). Both are GPL-3.0 like this crate.

/// Parity rows of the NID generator matrix (16 data bits -> 48 parity bits,
/// including the trailing parity bit). From SDRTrunk's BCH_63_16_23_P25_Test,
/// which in turn cites TIA-102.BAAA.
const NID_GENERATOR: [u64; 16] = [
    0o6331141367235452,
    0o5265521614723276,
    0o4603711461164164,
    0o2301744630472072,
    0o7271623073000466,
    0o5605650752635660,
    0o2702724365316730,
    0o1341352172547354,
    0o0560565075263566,
    0o6141333751704220,
    0o3060555764742110,
    0o1430266772361044,
    0o0614133375170422,
    0o6037114611641642,
    0o5326507063515373,
    0o4662302756473127,
];

/// Encode the 64-bit NID: NAC (12 bits), DUID (4 bits), 47 BCH parity bits and
/// the final parity bit, most significant bit first.
pub fn nid_encode(nac: u16, duid: u8) -> u64 {
    let data = ((nac as u64 & 0xfff) << 4) | (duid as u64 & 0xf);
    let mut parity = 0u64;
    for (bit, row) in NID_GENERATOR.iter().enumerate() {
        if data >> (15 - bit) & 1 == 1 {
            parity ^= row;
        }
    }
    (data << 48) | (parity & 0xffff_ffff_ffff)
}

/// Nearest-codeword NID decode. The BCH(63,16) code has minimum distance 23, so
/// exhaustive search over all 65536 codewords is exact and costs microseconds.
/// The trailing (64th) parity bit is not part of the BCH code and is ignored.
/// Returns (nac, duid, corrected bit errors).
pub fn nid_decode(received: u64, max_errors: u32) -> Option<(u16, u8, u32)> {
    use std::sync::OnceLock;
    static TABLE: OnceLock<Vec<u64>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        (0..65536u32)
            .map(|d| nid_encode((d >> 4) as u16, (d & 15) as u8) >> 1)
            .collect()
    });
    let word = received >> 1;
    let (mut best, mut best_errors) = (0usize, u32::MAX);
    for (index, codeword) in table.iter().enumerate() {
        let errors = (codeword ^ word).count_ones();
        if errors < best_errors {
            best = index;
            best_errors = errors;
        }
    }
    (best_errors <= max_errors).then_some(((best >> 4) as u16, (best & 15) as u8, best_errors))
}

/// Rate-1/2 trellis state transition table: `TRELLIS[state][input]` is the
/// 4-bit constellation codeword sent for dibit `input` after dibit `state`.
/// OP25 `next_words`; SDRTrunk `P25_1_2_Node.TRANSITION_MATRIX`.
const TRELLIS: [[u8; 4]; 4] = [
    [0x2, 0xC, 0x1, 0xF],
    [0xE, 0x0, 0xD, 0x3],
    [0x9, 0x7, 0xA, 0x4],
    [0x5, 0xB, 0x6, 0x8],
];

/// Block interleave for 196-bit trellis blocks: deinterleaved bit `b` is
/// received bit `INTERLEAVE[b]` (OP25 `deinterleave_tb`, SDRTrunk `DATA_INTERLEAVE`).
/// The pattern is 4-bit groups taken in the order 0,13,25,37,1,14,26,38,...
pub fn interleave_index(b: usize) -> usize {
    let group = b / 4;
    let (row, column) = (group / 4, group % 4);
    let source = row + [0, 13, 25, 37][column];
    source * 4 + b % 4
}

/// Encode 12 bytes (96 bits) into 196 transmitted bits (one per byte, 0/1),
/// trellis-coded and interleaved. Used to build test vectors.
pub fn trellis_half_encode(data: &[u8; 12]) -> [u8; 196] {
    let mut coded = [0u8; 196];
    let mut state = 0usize;
    for d in 0..49 {
        let input = if d < 48 {
            (data[d / 4] >> (6 - 2 * (d % 4))) as usize & 3
        } else {
            0
        };
        let word = TRELLIS[state][input];
        for k in 0..4 {
            coded[d * 4 + k] = (word >> (3 - k)) & 1;
        }
        state = input;
    }
    let mut sent = [0u8; 196];
    for (b, bit) in coded.iter().enumerate() {
        sent[interleave_index(b)] = *bit;
    }
    sent
}

/// Viterbi decode of one interleaved 196-bit rate-1/2 block (bits as 0/1 bytes).
/// Returns the 12 decoded bytes and the Hamming path metric (corrected bit count).
pub fn trellis_half_decode(received: &[u8]) -> Option<([u8; 12], u32)> {
    if received.len() < 196 {
        return None;
    }
    let mut words = [0u8; 49];
    for b in 0..196 {
        words[b / 4] |= (received[interleave_index(b)] & 1) << (3 - b % 4);
    }
    const INF: u32 = u32::MAX / 2;
    let mut metric = [0, INF, INF, INF];
    let mut history = [[0u8; 4]; 49];
    for (step, word) in words.iter().enumerate() {
        let mut next = [INF; 4];
        for (input, slot) in next.iter_mut().enumerate() {
            for (state, m) in metric.iter().enumerate() {
                let cost = m + (TRELLIS[state][input] ^ word).count_ones();
                if cost < *slot {
                    *slot = cost;
                    history[step][input] = state as u8;
                }
            }
        }
        metric = next;
    }
    // The encoder flushes with a zero dibit, so the path ends in state 0.
    let errors = metric[0];
    let mut state = 0usize;
    let mut dibits = [0u8; 49];
    for step in (0..49).rev() {
        dibits[step] = state as u8;
        state = history[step][state] as usize;
    }
    let mut out = [0u8; 12];
    for (d, dibit) in dibits.iter().take(48).enumerate() {
        out[d / 4] |= dibit << (6 - 2 * (d % 4));
    }
    Some((out, errors))
}

/// Bit-serial CRC-CCITT (x^16 + x^12 + x^5 + 1) polynomial division remainder.
fn ccitt_remainder(bytes: &[u8]) -> u16 {
    let mut crc: u32 = 0;
    for byte in bytes {
        for j in 0..8 {
            crc = ((crc << 1) | ((*byte as u32 >> (7 - j)) & 1)) & 0x1ffff;
            if crc & 0x10000 != 0 {
                crc = (crc & 0xffff) ^ 0x1021;
            }
        }
    }
    crc as u16
}

/// TSBK / PDU header CRC check over 12 bytes (80 data bits + 16-bit CRC),
/// equivalent to OP25 `crc16(buf, 12) == 0`.
pub fn crc_ccitt_ok(block: &[u8; 12]) -> bool {
    ccitt_remainder(block) ^ 0xffff == 0
}

/// Set bytes 10..12 of a block to its CRC (test-vector construction).
pub fn crc_ccitt_fill(block: &mut [u8; 12]) {
    block[10] = 0;
    block[11] = 0;
    let crc = ccitt_remainder(block) ^ 0xffff;
    block[10] = (crc >> 8) as u8;
    block[11] = crc as u8;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nid_roundtrip_corrects_errors() {
        let word = nid_encode(0x3a3, 7);
        assert_eq!(word >> 48, 0x3a37);
        assert_eq!(nid_decode(word, 0), Some((0x3a3, 7, 0)));
        // Flip eleven code bits (t = 11 for BCH(63,16,23)).
        let mut damaged = word;
        for bit in [1, 5, 9, 14, 20, 27, 33, 40, 47, 55, 62] {
            damaged ^= 1 << bit;
        }
        assert_eq!(nid_decode(damaged, 11), Some((0x3a3, 7, 11)));
        assert_eq!(nid_decode(damaged, 4), None);
    }
    #[test]
    fn interleave_is_a_permutation() {
        let mut seen = [false; 196];
        for b in 0..196 {
            seen[interleave_index(b)] = true;
        }
        assert!(seen.iter().all(|x| *x));
        // Spot-check against the published table (OP25 deinterleave_tb).
        let table = [0, 1, 2, 3, 52, 53, 54, 55, 100, 101, 102, 103, 148, 149, 150, 151, 4];
        for (b, expected) in table.iter().enumerate() {
            assert_eq!(interleave_index(b), *expected);
        }
        assert_eq!(interleave_index(195), 51);
        assert_eq!(interleave_index(191), 195);
    }
    #[test]
    fn trellis_roundtrip_corrects_scattered_errors() {
        let mut block = [0x80, 0x00, 0x00, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0, 0];
        crc_ccitt_fill(&mut block);
        assert!(crc_ccitt_ok(&block));
        let mut sent = trellis_half_encode(&block);
        assert_eq!(trellis_half_decode(&sent), Some((block, 0)));
        for bit in [3, 50, 97, 140, 190] {
            sent[bit] ^= 1;
        }
        let (decoded, errors) = trellis_half_decode(&sent).unwrap();
        assert_eq!(decoded, block);
        assert_eq!(errors, 5);
    }
    #[test]
    fn crc_rejects_single_bit_damage() {
        let mut block = [0x3d, 0x00, 0x0a, 0x04, 0x81, 0x90, 0xa2, 0x86, 0xe4, 0x40, 0, 0];
        crc_ccitt_fill(&mut block);
        for bit in 0..96 {
            let mut damaged = block;
            damaged[bit / 8] ^= 0x80 >> (bit % 8);
            assert!(!crc_ccitt_ok(&damaged));
        }
    }
}
