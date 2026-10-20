# AMBE+2 port: working map

Resume file for the port of the MD-380 D002.032 AMBE+2 decoder (see DESIGN.md for the why).
Update the status column as routines land. Addresses are firmware (flash at 0x0800C000).

## Inputs and tooling

- Firmware and RAM snapshot: `scripts/fetch-md380.sh` puts them in
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

## Routine map

Every routine reachable from the entry point, and its Rust port (`module::function`). Not ported:
0x08060e38 (encoder-only codebook search; reached only with the encode flag, never by the
decoder) and the encode branches of 0x08061074 / 0x08061340.

| Firmware | Rust |
|---|---|
| 0x08051248 | decoder::decode_wav |
| 0x0805359c | math::fill |
| 0x08054cbc | synth::post_filter |
| 0x08055170 | frame::field_widths |
| 0x08055484 | decoder::decode_params |
| 0x08055628 | frame::raise_levels |
| 0x080556bc | frame::tone_class |
| 0x080556fc | frame::tone_harmonic |
| 0x0805573c | frame::pitch_history |
| 0x08055808 | frame::tone_block |
| 0x08055b64 | math::read_bits |
| 0x08055bac | math::write_bits |
| 0x08055c74 | synth::voicing_array |
| 0x08055cf4 | frame::copy_params |
| 0x08055d74 | decoder::synthesize |
| 0x08056224 | decoder::frame_control |
| 0x0805641c | synth::move_peak |
| 0x08056580 | math::div_half |
| 0x08056640 | math::div_int |
| 0x08056680 | math::float_add |
| 0x08056700 | math::float_sub |
| 0x08056800 | math::div_norm |
| 0x08056900 | math::log2_poly |
| 0x08056980 | math::log2_scaled |
| 0x080569c0 | math::pow2_poly |
| 0x08056a80 | math::pow2_scaled |
| 0x08056ac0 | math::sqrt_norm |
| 0x08056b40 | math::sqrt_scaled |
| 0x08057880 | fft::headroom |
| 0x080578c0 | fft::bit_reverse |
| 0x08057940 | fft::first_stage |
| 0x080579c0 | fft::stage |
| 0x08057b40 | fft::complex_fft |
| 0x08057c00 | fft::unpack_pairs |
| 0x08057d00 | fft::rotate_pairs |
| 0x08057d5c | fft::real_pass |
| 0x08057e10 | fft::fft_forward |
| 0x08057e34 | fft::fft_inverse |
| 0x08057e80 | math::mean |
| 0x08058058 | math::scale_shift, math::scale_shift_in_place |
| 0x08058098 | math::mul_q15, math::mul_q15_in_place |
| 0x080580b8 | math::energy |
| 0x08058170 | math::shift, math::shift_in_place |
| 0x080581dc | math::max_abs_norm |
| 0x0805821c | math::normalize, math::normalize_in_place |
| 0x08058284 | synth::write_output |
| 0x08058334 | math::shift_diff |
| 0x0805836c | math::shift_diff_sat |
| 0x0805c368 | frame::dequant_step |
| 0x0805c47c | frame::pair_levels |
| 0x0805c778 | spectral::save_frame |
| 0x0805c7e4 | frame::bump_frame_counter |
| 0x0805c7f8 | spectral::dequant_magnitudes |
| 0x0805cc40 | spectral::dequant_spectrum |
| 0x0805cc98 | frame::bit_reorder |
| 0x0805d0d4 | math::read |
| 0x0805d0ec | frame::band_flags |
| 0x0805d11c | spectral::smooth_magnitudes |
| 0x0805d380 | spectral::resample_step |
| 0x0805d3e8 | spectral::resample_mags |
| 0x0805d480 | spectral::blend_frames |
| 0x0805d618 | frame::tone_frame |
| 0x0805d708 | frame::pitch_params |
| 0x0805d754 | synth::amp_expand |
| 0x0805d7d8 | synth::band_voicing |
| 0x0805d85c | synth::level_update |
| 0x0805d8a4 | unvoiced::unvoiced |
| 0x0805df08 | voiced::osc_run |
| 0x0805e010 | voiced::build_period |
| 0x0805e19c | voiced::overlay_period |
| 0x0805e354 | voiced::overlay_series |
| 0x0805e584 | voiced::synth_voiced |
| 0x0805ec20 | synth::amp_fir |
| 0x0805ed14 | synth::enhance |
| 0x08060f4c | frame::spread_pairs |
| 0x08060f9c | math::harmonic_count |
| 0x08060fe0 | frame::period_params |
| 0x08061074 | spectral::vq_read |
| 0x080612ac | spectral::inverse_dct |
| 0x08061340 | spectral::gain_dequant |
| 0x08061440 | frame::copy_params_head |

## Status (2026-10-19)

- Every ported routine replays its firmware recordings exactly (`cargo test --lib ambe` with
  `.native-md380/vectors` present: 36-48 recorded calls per routine; the porting runs also
  checked several hundred extra calls per routine).
- End to end the decoder is bit-exact: all 8,043 frames of the nine corpora (DMR fixture with and
  without the bad-frame flag, decoder resets,
  four Harvard files, random and edge frames incl. flag bits) give the firmware's samples
  exactly (`tests/ambe.rs`, hashes; first-difference report when `.native-md380/expected/*.raw`
  from `ambe_oracle.py decode` is present).
- Reached by no corpus, so ported from the assembly but not exercised: 0x0805641c is verified only
  by direct-call fuzz recordings; 0x0805836c and 0x08057e80 have no recordings; FFT sizes other
  than 256 and the `edge` FFT layout; a few saturation branches (see the doc comments).
- Specialised constants: n = 80 samples per half call, rate index 0 (49-bit frames), encode
  flag 0, the PREV/CUR/WIDTHS state offsets.

## Gotchas found on the way

- In the REA/Ghidra assembly listings `qsub rd, A, B` computes B - A (and `qdadd`/`qdsub`
  likewise take the doubled operand first); Ghidra's C has it right.
- Unicorn hooks: use a block hook (entry and return addresses start blocks); close the
  recording file explicitly (the hook closure keeps it in a reference cycle).
