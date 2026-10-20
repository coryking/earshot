# AMBE+2 port: working map

Resume file for the port of the MD-380 D002.032 AMBE+2 decoder (see DESIGN.md for the why).
Update the status column as routines land. Addresses are firmware (flash at 0x0800C000).

## Inputs and tooling

- Firmware and RAM snapshot: `scripts/fetch-md380.sh` (voice-decoder branch) puts them in
  `engine/.native-md380/firmware/` (Git-ignored; SHA-256 pinned: image c0351f25…, RAM d4039818…).
- Decompilation: REA (rea-agents 6.5.0) MCP server over Ghidra 12.1.4 (JDK 21), raw-binary ARM,
  entry 0x0800C000. Exported pseudo-C and assembly of all 83 decoder routines were kept outside
  the repo (session scratchpad `rea/funcs/0x….{c,s}`); re-export with REA `procedure_pseudo_code`
  / `procedure_assembly` on the addresses below if needed.
- Oracle: `scripts/ambe_oracle.py` (Unicorn, Cortex-M4). `decode` matches the dynarmic
  emulator (`libradio_md380.so`) sample for sample (300 random frames, 1500 Harvard frames).
  `capture` records calls of one routine; recordings go in `.native-md380/vectors/<addr>.rec`
  and the `#[cfg(test)]` replay tests in each module consume them (`testvec.rs`).
- Tables: `scripts/ambe-tables.py` regenerates `tables/*.rs` from `tables/*.spec`.
- Frame corpora (`tests/data/ambe/`): `fixture.frames` (DSDcc FEC output of the DMR golden
  fixture, 392 frames), `harvard-00{10,11,30,31}.frames` (Harvard sentences through the MD-380
  encoder), `random.frames`, `edge.frames` (tone/erasure frames and decoder flag bits).

## Entry point and calling convention

`ambe_decode_wav` 0x08051248 (out, n=80, bits, r3=0, [sp]=flags, [sp+4]=half, [sp+8]=state).
Called twice per 20 ms frame: half 0 decodes the 49 bits (one per halfword, mbelib `ambe_d`
order) into a parameter block and synthesizes samples 0-79; half 1 synthesizes 80-159 from the
block saved at state+0x3ec. n is clamped to 76..84 (always 80 here). `flags` bits seen in code:
0x4, 0x8 (bad frame / repeat), 0x4000, 0x80; 0 in normal use.

The 49 bits are post-FEC: Golay/Hamming correction, deinterleave and descrambling happen before
this routine (DSDcc / mbelib `mbe_eccAmbe3600x2450*` in the engine).

## Call graph (static) and ownership

```
0x08051248 decode_wav                       glue (integrator)
  0x08056224 frame top                      glue
    0x08055484 decode params                glue
      0x08055170 rate/field widths          frame
      0x0805cc98 bit reorder                frame
      0x0805d0ec                            frame
      0x0805c47c -> 0x0805c368              frame
      0x0805d708                            frame
      0x0805d618 -> 0x08055808 -> 0x08055628 frame
      0x0805cc40 -> 0x08061340, 0x0805c7f8 (-> 0x08060e38, 0x08061074, 0x080612ac)  spectral
      0x0805d11c                            spectral
      0x0805d480 -> 0x0805d3e8 -> 0x0805d380 spectral
      0x0805c778                            spectral
    0x0805573c, 0x080556bc, 0x080556fc, 0x08055cf4, 0x0805c7e4, 0x08061440   frame
  0x08055d74 synth top (per half)           glue
    0x0805ec20, 0x0805d754, 0x0805ed14, 0x08055c74, 0x0805d85c, 0x0805641c   synth
    0x0805d8a4 unvoiced (LCG noise, FFT, band scaling, IFFT, OLA)           unvoiced
      0x08057e10 / 0x08057e34 FFT / IFFT (0x08057b40.., 0x08057d5c..)        fft
    0x0805e584 voiced (0x0805e010, 0x0805e19c, 0x0805df08, 0x0805e354)        voiced
    0x08054cbc, 0x08058284 output                                           synth
shared helpers (math.rs, ops.rs): 0x0805359c 0x08053538 0x08056580 0x08056640 0x08056800
  0x08056ac0 0x08056b40 0x080569c0 0x08056a80 0x08056900 0x08056980 0x08058058 0x080580b8
  0x08058098 0x08058170 0x080581dc 0x0805821c 0x08058334 0x0805836c 0x08056680 0x08056700
  0x08057e80 0x08060f9c 0x08055b64 0x0805d0d4 0x08055bac 0x0805c610
```

## State block (state.rs)

0x7ec bytes at RAM 0x20011224, initialised from the RAM snapshot (`tables/state.spec`).
Known fields: 0x3ec / 0x474 / 0x4fc parameter blocks (0x44 halfwords each; halfword 0x40-0x41
of a block holds a firmware pointer to a per-harmonic voicing array, meaningless in the port and
ignored in state comparisons), 0x7be frame history flags, 0x7a4 field-width table.

## Status

| Stage | Routines | Status |
|---|---|---|
| shared helpers | math.rs list above | ported; replay tests |
| frame dequant | frame | in progress |
| spectral dequant | spectral | in progress |
| unvoiced + FFT | unvoiced, fft | in progress |
| voiced | voiced | in progress |
| synth leaves | synth | in progress |
| glue + end to end | decode_wav, 0x08056224, 0x08055484, 0x08055d74 | not started |
