# Pluto FPGA coarse channelizer: design note

Reference keys like [harris04] point to `references.md`. Measured numbers come from
`fpga/model/results/*.json` and from the build and simulation logs named in
`fpga/README.md`.

## 1. Requirements and derived parameters

**What it receives.** One parked 20 MHz block anywhere in UHF 450–470 MHz. Trunking
at 851–869 MHz is a follow-up. AD9363 RF bandwidth tops out at 20 MHz, so the
whole 450–470 MHz band fits one window: LO 460 MHz, 25.6 Msps complex (usable span
±10 MHz inside the ±12.8 MHz Nyquist band).

**Channel raster.** Part 90 puts 450–470 MHz on a 6.25 kHz centre raster, with
12.5 kHz channels since narrowbanding in 2013 [cfr90]. DMR is a 12.5 kHz carrier:
4800 symbols/s 4FSK with ±1.944 kHz outer deviation [dmr]. NXDN uses 6.25 kHz or
12.5 kHz [nxdn]. The host (Rust) does the final mix and channel filter for each
logical target. The FPGA only has to deliver a coarse slice that contains the
whole channel, undistorted and free of aliases.

**Bin spacing and oversampling.**
- With M = 512 bins at 25.6 Msps the bins are 50 kHz apart.
- A channel centre can sit up to 25 kHz from the nearest bin centre. A 12.5 kHz
  channel then reaches ±31.25 kHz from that centre. This is the *protected band*:
  it must be flat and alias-free.
- A critically sampled bank would output 50 ksps per bin, and the protected band
  would alias.
- Decimating by M/2 instead (2x oversampled, 100 ksps) [harris04 ch. 9; harris10;
  vallance17] folds the output spectrum at ±50 kHz. Energy at offset f lands at
  f − 100 kHz, so only input beyond 100 − 31.25 = **68.75 kHz** can fold into the
  protected band.
- The prototype therefore needs a flat passband to 31.25 kHz, a stopband from
  68.75 kHz, and a 37.5 kHz transition. The transition can overlap the
  neighbouring bin because the oversampled output has room for it.
- This is the property the ARM C prototype lacks. Its filter is −9.5 dB at the bin
  edge and −24 dB at 31.25 kHz (table below).

**Attenuation.**
- The off-air captures in `recordings/` (survey: `output/iq-survey-wide-30s.json`)
  show about 40 dB between the strongest and weakest occupied channels in a 6 MHz
  window: peaks from −78 to −110 dBFS/Hz over a −119 dBFS/Hz floor.
- A weak channel needs about 20 dB SNR for a clean 4FSK decode. Aliases therefore
  have to stay at least 60 dB down. Add margin for stronger near/far cases in a
  full 20 MHz block.
- AD9363 SFDR is roughly 70–80 dB, so making the filter better than the ADC buys
  nothing.
- Target: **≥ 90 dB** alias rejection.

**Length.**
- harris's estimate N ≈ (fs/Δf)(A/22) [harris04] gives 2,793 taps.
- Kaiser's estimate (A − 7.95)/(2.285 Δω) [kaiser74] gives 3,902.
- Chosen: **4096 = 8 taps per branch**, the same per-branch count as the ARM
  prototype.

**Design method.** Kaiser-windowed sinc [kaiser74], cutoff 50 kHz (the −6 dB point
falls at the bin spacing, so neighbours cross at −6 dB), β = 0.1102(94 − 8.7) =
9.4. A Parks–McClellan design [pm72] would reach the same specification with fewer
taps. The length is already fixed at 8 per branch by the 16-DSP FIR, though, and
Kaiser's monotonically falling sidelobes suit a bank that rejects many interferers
at once. Coefficients are quantized to 18 bits (DSP48E1 B port [ug479]) with the
peak at full scale.

| Prototype (`results/filter.json`) | Ripple over ±31.25 kHz | Worst alias ≥ 68.75 kHz | @25 kHz | @50 kHz | @100 kHz |
| --- | ---: | ---: | ---: | ---: | ---: |
| Kaiser, float | 0.0004 dB | −94.2 dB | 0.00 dB | −6.02 dB | −107 dB |
| Kaiser, 18-bit (implemented) | 0.0004 dB | −94.2 dB | 0.00 dB | −6.02 dB | −107 dB |
| ARM C prototype filter at 512 bins | 24 dB | −88.7 dB | −9.46 dB | −84 dB | −110 dB |

The fixed-point tone sweep (`results/alias.json`) measures at most −92.9 dB of
aliased power in bin 0 for tones at or beyond 68.75 kHz. That figure includes the
12-bit input quantization.

## 2. Structure

Polyphase partition of the 4096-tap prototype into 512 branches [harris04 ch. 6;
crochiere83], commutated with hop M/2 = 256 and followed by a 512-point FFT:

    u_f[j] = Σ_{t=0..7} h[512 t + j] · x[256 f + 512 t + j]      (j = 0..511)
    Y_f[k] = (−1)^(k f) · FFT_512(u_f)[k]

The (−1)^(k f) factor removes the rotation that a half-length hop puts on every odd
bin [vallance17]. Without it, odd bins alternate sign from frame to frame and are no
longer baseband. Bin k is the complex baseband of RF offset k · 50 kHz (k ≥ 256 is
negative), sampled at 100 ksps, with group delay 2047.5 input samples. Frame
alignment is identical to `offload/arm/pfb.c`. The model checks this by running the
C filter through the float model and comparing.

Comparison with open designs:
- **CASPER** PFBs [casper] are critically sampled spectrometers. They use the same
  partition, but no oversampling and no phase correction.
- **Maia SDR** [maia] has a windowed FFT spectrometer, not a channelizer.
- **ADI hdl** has no channelizer. Its axi_dmac, the up_axi register slave and the
  project flow are reused unchanged.

## 3. Fixed point and quantization

| Stage | Format | Bound / note |
| --- | --- | --- |
| ADC | 12-bit, sign-extended to 16 by axi_ad9361 | clipped to [−2048, 2047] (counted) |
| Coefficients | 18-bit, Σh = 2^25 | |
| Branch sum | 30-bit products, 34-bit sum, round-half-up >> 12, saturate 18 | max 0.555 FS for any 12-bit input |
| FFT (xfft, unscaled) | 18-bit in, 18-bit twiddles, convergent rounding, 28-bit out | Σ\|h\|·2048·√2/2^12 = 2^25.2 < 2^27 |
| Slice | round >> out_shift (default 9), saturate 16 | gain ×16 ADC units, no saturation for 12-bit input |
| Power word | round >> 8, saturate 18, I²+Q² | 35-bit |

Gain from the ADC to the FFT output is exactly 2^13. The 18-bit FIR output keeps one
bit of headroom over the worst case, which makes the FFT overflow-free by
construction (no scaling schedule needed). The model asserts the bound on every
frame.

SNR loss against a float channelizer on the same quantized input:
- Synthetic tone plus white noise: core 0.017 dB with ADC quantization noise only,
  0.002 dB at 1 LSB rms.
- 16-bit slice: 2.5, 0.62 and 0.08 dB at 0, 1 and 3 LSB rms. Real receiver noise
  is about 90 LSB rms wideband in the captures, so the slice quantization is
  irrelevant there.
- Real DMR in the straddle sweep (`results/straddle.json`): **≤ 0.16 dB** worst
  case across offsets 0–25 kHz and interferers at 12.5–150 kHz with 40–60 dB
  near/far.

## 4. FFT: xfft, pipelined streaming

The FFT has to take 512 points per 256-sample hop (1000 clocks at 100 MHz).

| Option (512-point, 18-bit, synthesized OOC) | LUT | BRAM36 | DSP | Fits 1000 clocks/frame? |
| --- | ---: | ---: | ---: | --- |
| xfft pipelined streaming [pg109] | 2,969 | 4 | 24 | yes, 1 sample/clock |
| xfft radix-4 burst, natural order | 2,277 | 7 | 18 | no: load + unload alone is 1024 clocks |
| Custom R2²SDF [he96], as Maia does | ~1.5k (est.) | ~3 | 12 | yes |

Pipelined xfft was chosen: it is AMD IP with a shipped bit-accurate C model, and
after the TX trim the DSP budget holds its 24 multipliers (§9). R2²SDF is the
documented fallback if DSPs become tight. It saves 12 DSPs, at the cost of an
in-house FFT and its own bit-exact model.

## 5. Spectrum and activity detection

Per coarse bin k, from the power word P_k = |Y_k|²:

1. **Welch PSD** [welch67]. The PFB prototype acts as the data window and frames
   overlap 50%, so averaging K = 256 frames (2.56 ms) gives the block power.
   - Overlap correlation ρ(m) = |Σ h[n] h[n + 256 m]|² / (Σ h²)² gives
     ν = 2K / (1 + 2 Σ (1 − m/K) ρ(m)) = **500.8 equivalent degrees of freedom**
     (≈ 250 independent frames) [welch67; percival93].
   - Under noise, P̂/σ² ~ χ²_ν/ν, so the estimate's standard deviation is
     √(2/ν) = 0.27 dB.
2. **Log domain.** L = log2(P̂) in Q8 (0.0118 dB per LSB) via leading-one detection
   and a 256-entry mantissa table. Every threshold then becomes an addition, and
   the detector uses no DSPs.
3. **Noise floor by minimum statistics** [martin01]. N_k is the minimum of L_k over
   the current sub-window plus V = 4 retained sub-window minima of U = 128 blocks.
   That window is 1.3–1.6 s, so the floor follows slow changes and never needs the
   bin to be empty at any particular moment.
4. **Order-statistic CFAR decision** [rohling83; richards14].
   - Open when L > N + T_on. Stay open while L > N + T_off, then for `hang` more
     blocks (39 blocks = 100 ms, which bridges DMR TDMA gaps).
   - With the floor the minimum of D independent draws:
     Pfa(t) = ∫ f_min(m) (1 − F(m t)) dm, where f_min = D f (1 − F)^(D−1) and F, f
     are the χ²_ν/ν CDF and PDF (`activity.pfa_min_floor`).
   - Design is for the largest window, D = U(V+1) = 640:
     - T_on = 2.60 dB gives Pfa = 1e-8 per bin per block, or 7.2 false opens per
       hour across all 512 bins.
     - T_off = 1.57 dB gives Pfa = 1e-2.
   - Monte Carlo of the bit-exact detector over 1.02e7 bin-blocks:
     - At the 1e-4 design point it measured 7.9e-5. The prediction is
       worst-case D; the window averages fewer draws.
     - At the operating point it saw 0 false opens.
5. **Detection.** For a signal at in-bin SNR s, the block power is a scaled
   noncentral χ²_ν(ν s) [urkowitz67; kay98]. With the floor ~0.89 dB below the
   mean (expected-minimum bias): Pd = 0.58 at s = −3 dB, 0.94 at −2 dB, 0.9994 at
   −1 dB. The bin's equivalent noise bandwidth is 1.865 bins = 93.3 kHz [harris78]
   (the 2x-oversampled bins overlap), so a 12.5 kHz channel's in-bin SNR is its
   channel SNR − 8.7 dB. Detection is reliable from about 8 dB channel SNR, below
   what a DMR decoder needs anyway.
6. **Long PSD.** A linear sum over 2^13 frames (82 ms), reported as Q8 log2, for the
   waterfall. Calibration to dBFS is in §8.

**Limitation found on real data: AGC.**
- The 30 s capture was taken with the AD9363 in fast_attack AGC.
- Its block-to-block noise reference (25th percentile across bins) moves up to
  4.7 dB in one 2.56 ms block, and by 10 dB over 1 s
  (`activity.json → off_air`).
- No per-bin floor tracker can separate that from signal. The detector marks the
  whole band active.
- Channelizer mode therefore has to run the AD9363 in **manual gain**.
- After a deliberate gain change, the ARM writes CONTROL bit 4 (DET_RESET) to
  restart floor tracking.
- Cross-bin normalization by a per-block global percentile would also absorb gain
  steps; it is open issue 3.

## 6. Datapath

    axi_ad9361 ch0 (l_clk) ─ xpm_fifo_async ─► clip 12b ─► PFB FIR (8 banks x 512, 16 DSP) ─► FIFO
      ─► xfft (pipelined) ─► phase corr ─► slice (>>out_shift, sat16) ─► frame buffer ─► packetizer
                                       └─► power word ─► Welch / min-stat / CFAR / long PSD
    packetizer ─► AXIS 64b ─► axi_dmac (cyclic) ─► HP0  (history ring: every bin, every frame)
               └► AXIS 64b ─► axi_dmac          ─► HP3  (selected bins)

**PFB FIR (`chan_pfb_fir.v`).**
- The newest 4096 samples are held in eight 512-word banks, bank = address[11:9].
- For branch j, all eight taps sit at the same offset in eight different banks, so
  a single cycle reads all of them. The coefficient for bank b is tap
  (b − bank0) mod 8, chosen by rotating the eight ROM outputs.
- One branch sum per clock means 512 of the 1000 clocks per hop.
- A write may overwrite sample n − 4096 only after the read engine has issued that
  address, so the write pointer chases the read pointer. The input FIFO absorbs the
  short stalls.

**Frame scheduling.**
- The FFT receives a 512-word burst per frame.
- Post-processing never back-pressures.
- The packetizer admits a frame to a stream only if that stream's output FIFO
  (1024 × 64) can take the whole record. A stalled DMA therefore costs whole,
  counted frames, flagged in the next header, and never truncated records or
  upstream stalls.

## 7. Output formats (little-endian 64-bit words)

| Word | Bits |
| --- | --- |
| Header | [63:60]=0xA, [59:56]=1 history / 2 selected, [55:48]=flags, [47:0]=frame |
| History data (256 per frame) | [15:0]=I(2p) [31:16]=Q(2p) [47:32]=I(2p+1) [63:48]=Q(2p+1) |
| Selected sample | [63:60]=0x5, [56:48]=bin, [47:32]=frame[15:0], [31:16]=Q, [15:0]=I |

Flags:
- bit 0: frames of this stream were dropped before this record.
- bit 1: ADC samples were lost (CDC FIFO full) since the last header.
- bit 2: a sample in this frame hit 16-bit saturation.

**Frame numbering.**
- Frame f covers input samples [256 f, 256 f + 4095] counted from RUN, centred at
  256 f + 2047.5.
- SAMPLES (0x010) counts every accepted input sample, so frame numbers convert to
  device sample time.
- Any loss shows up in the counters and flags. This is the hardware sample counter
  that PROTOCOL.md's `hardware_counter` was waiting for.

**History ring.**
- chan_hist_dma runs in axi_dmac cyclic mode over a reserved DDR region of N
  records × 2056 bytes. Records are 257 words; the header carries the frame number.
- After enabling the DMA, the ARM sets HIST_EN. HIST_FIRST_FRAME (0x01E) is the
  frame number that lands at the region base.
- Frame f is at ((f − first) mod N) × 2056, provided HIST_DROPS has not moved. The
  record header confirms the frame, and that check catches overwrite races.
- At 205.6 MB/s, 64 MiB holds 326 ms of pre-roll for every bin.

**Selected stream.**
- A non-cyclic transfer queue on chan_sel_dma. Records are self-describing, so the
  ARM parses whole buffers.
- The selection is FORWARD_MASK, optionally ORed with the activity bitmap
  (CONTROL.auto_forward).

## 8. Register map (`chan_top`, DWORD addresses; byte address = 4 × DWORD)

| DWORD | Name | Access | Notes |
| --- | --- | --- | --- |
| 0x000 | VERSION | RO | 0x0001_0000 |
| 0x001 | ID | RO | instance ID |
| 0x002 | SCRATCH | RW | |
| 0x003 | MAGIC | RO | 0x4348_414E "CHAN" |
| 0x004 | CONTROL | RW | [0] RUN (0 = soft reset of the datapath) [1] HIST_EN [2] SEL_EN [3] AUTO_FORWARD [4] DET_RESET (self-clearing) |
| 0x005 | STATUS | RO | [0] running |
| 0x006 | OUT_SHIFT | RW | slice gain, default 9 (×16 ADC units) |
| 0x007 | CONFIG | RO | [15:0] bins, [23:16] taps/branch, [31:24] block log2 |
| 0x008–0x00B | DET_ON / DET_OFF / DET_BIAS (Q8 log2) / DET_HANG (blocks) | RW | defaults 221 / 133 / 0 / 39 |
| 0x00C | DET_CONFIG | RO | [15:0] U, [23:16] V, [31:24] PSD log2 |
| 0x010/0x011 | SAMPLES lo/hi | RO | reading lo latches hi |
| 0x012/0x013 | FRAMES lo/hi | RO | frames out of the FFT |
| 0x014 | ADC_OVERFLOWS | RO | CDC FIFO overflows (Gray-synchronized) |
| 0x015 | ADC_CLIPS | RO | samples clipped to 12 bits |
| 0x016 | OUT_SATURATIONS | RO | slice samples saturated |
| 0x017/0x018 | HIST_FRAMES / HIST_DROPS | RO | |
| 0x019/0x01A | SEL_FRAMES / SEL_DROPS | RO | |
| 0x01B | PACK_OVERRUNS | RO | must stay 0 |
| 0x01C/0x01D | BLOCK_COUNT / PSD_COUNT | RO | |
| 0x01E/0x01F | HIST_FIRST_FRAME lo/hi | RO | |
| 0x020 | FFT_EVENTS | RO | xfft tlast/halt events, must stay 0 |
| 0x021 | FIR_FRAMES | RO | |
| 0x040–0x04F | FORWARD_MASK | RW | bit b of word w = bin 32w + b |
| 0x050–0x05F | ACTIVE | RO | activity bitmap |
| 0x200–0x3FF | LEVEL[k] | RO | [15:0] block level L, [31:16] floor N (Q8 log2) |
| 0x400–0x5FF | PSD[k] | RO | long PSD (Q8 log2) |

**Level calibration.** A complex tone of amplitude A ADC units at a bin centre gives
|Y| = 2^13 A, a power word of (2^13 A / 2^8)² = 2^10 A², and a sum of S frames of S
times that (S = 2^8 for LEVEL, 2^13 for PSD). So, with full scale = 2048 ADC units:

    P [dBFS] = 3.0103 · L / 256 − 10 log10(S · 2^10 · 2048²)
             = 3.0103 · L / 256 − 96.33 − 10 log10(S)

For density, subtract 10 log10(93.3 kHz): the prototype's equivalent noise bandwidth
is 1.865 bins [harris78], and the label is then `dBFS/Hz` per PROTOCOL.md.

## 9. Clocks, CDC, resets (UG949)

- **Clocks.** One crossing: axi_ad9361 l_clk → FCLK0 (100 MHz), through
  xpm_fifo_async for the data and xpm_cdc_single for RUN. The overflow counter
  returns through xpm_cdc_gray [ug953; ug949 "Multi-Bit CDC"]. The xfft, both
  axi_dmac stream sides, the HP ports and the AXI-Lite slave all run on FCLK0, so
  nothing else crosses. The XPM macros carry their own scoped constraints. No
  clock groups or false paths were added.
- **Resets.** Synchronous, active-high, and on control state only: counters,
  FSMs, valid bits. The datapath registers, DSP and BRAM contents have no reset
  [ug949 "When and Where to Use a Reset"].
  - The system reset is FCLK0's proc_sys_reset.
  - The soft reset is CONTROL.RUN = 0.
  - Output FIFOs reset only with the system reset, so a soft reset never drops
    TVALID mid-handshake toward the DMA.
  - The xfft is reset together with the FIR that feeds it.
  - The activity state RAMs are swept clear after every reset (512 cycles).
- **DSP and BRAM.** Inferred following [ug901] templates (registered multiplier
  inputs, MREG/PREG; simple dual-port RAM with an output register).
  Utilization is confirmed in the implementation report (§10).

## 10. Verification and implementation results

See `fpga/README.md` for current numbers and how to reproduce them.

## 11. Deviations from the cited approaches

- **Kaiser vs. Parks–McClellan.** A remez design of the same length would reach
  about 100 dB. Kaiser was kept for its monotonic sidelobe decay and its
  closed-form β. The 94 dB it achieves exceeds the 90 dB requirement.
- **Minimum statistics without Martin's optimal smoothing.** The block average is
  already ν ≈ 500 dof. The bias is folded into the thresholds instead of applied as
  a multiplicative correction.
- **CFAR reference in time, not frequency.** A cell-averaging or OS-CFAR across
  neighbouring bins fails in clustered occupied bands. The per-bin time minimum
  follows the RFIC passband shape, which is the false-alarm cause the ARM prototype
  hit with a global median. The cost is the AGC sensitivity described in §5.
