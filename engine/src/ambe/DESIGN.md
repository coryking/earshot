# Native AMBE+2 decoder (port of the MD-380 firmware decoder)

## What this is and where it comes from

`src/ambe` decodes DMR voice (AMBE+2, 3600x2450: 49 data bits per 20 ms frame) to 8 kHz PCM.
It is a function-by-function port to Rust of the AMBE+2 decoder inside the Tytera MD-380 radio
firmware, version D002.032, the build the md380tools / md380-emu community documented. The
decoder in that firmware is DVSI's proprietary AMBE+2 implementation, so this code and its
generated tables (`tables/`) are derived from analysis of proprietary firmware. Cory chose to do
this for personal, non-commercial use, to get the same voice quality as a real radio rather than
mbelib's approximation. Nothing from the firmware is executed: there is no emulator, no JIT and
no ARM code at run time, and the engine does not need the firmware to build or run.

How it was made:
- The firmware image and RAM snapshot come from `scripts/fetch-md380.sh` (SHA-256 pinned,
  downloaded into the Git-ignored `.native-md380/`, never committed). Only the oracle and the
  table generator read them.
- REA (reverse-engineer-anything) drove Ghidra 12.1.4 over the raw image to decompile and
  disassemble the 83 routines reachable from `ambe_decode_wav` (0x08051248). Entry points and
  buffer addresses come from md380tools' D002.032 symbol file and md380-emu.
- Each routine was ported from its assembly (Ghidra's C was the map, the instructions the
  arithmetic): saturating and wrapping arithmetic, ARMv7E-M DSP instructions (`ops.rs`) and the
  firmware's own polynomial math (`math.rs`) are reproduced exactly.
- The constant tables and the decoder's initial state are copied from the image by
  `scripts/ambe-tables.py`.
- `scripts/ambe_oracle.py` runs the firmware decoder offline under Unicorn as a test oracle. It
  reproduces the dynarmic emulator the voice-decoder work used, sample for sample.

## Structure

The firmware keeps all decoder state in one 0x7ec-byte RAM block and addresses fields by byte
offset; `State` keeps that layout, so each ported routine reads next to its disassembly and tests
can compare the whole block with the firmware's RAM. NOTES.md maps every firmware routine to its
Rust function and records the verification status.

Per 20 ms frame (two firmware calls, one per 10 ms half):
1. Frame classification and parameter decode (`frame.rs`, `spectral.rs`, glue in `decoder.rs`):
   reorder the 49 bits into fields, decode pitch, voicing, gain and the PRBA/HOC spectral
   magnitudes with prediction from the previous frame; tone, erasure and frame-repeat handling.
2. Synthesis per half frame (`synth.rs`, `voiced.rs`, `unvoiced.rs`, `fft.rs`): spectral
   enhancement, voiced harmonics, unvoiced noise shaped in the frequency domain (LCG noise, FFT,
   per-band gains, inverse FFT, overlap-add), then output scaling to 16-bit samples.

## Interface

`Decoder::decode(&[u8; 49]) -> [i16; 160]` takes the error-corrected data bits in mbelib's
`ambe_d` order (DSDcc does sync, deinterleave, Golay/Hamming FEC and descrambling) and returns
160 samples. `decode_flags` also passes the firmware's frame-flag argument (bad-frame / repeat
bits), which the radio's baseband sets from FEC results. No allocation per frame; the state is a
fixed 2 KB block.

## Verification

- Per routine: `#[cfg(test)]` replay tests feed recorded firmware calls (arguments plus state and
  stack images before and after) into the port and compare every output.
- End to end: `tests/ambe.rs` decodes the frame corpora in `tests/data/ambe` and compares the
  output with the firmware's (hash per corpus; full sample comparison when the oracle outputs
  are present), plus a short RSS check (20k frames).
