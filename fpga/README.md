# Pluto FPGA coarse channelizer

A 512-bin, 2x-oversampled polyphase filter bank for the PlutoSDR Rev.B
(xc7z010clg225-1). At 25.6 Msps it delivers 50 kHz bins at 100 ksps complex, plus
per-bin PSD and activity detection. Two ADI axi_dmac streams carry the output: a
DDR history ring of every bin, and a forwarded stream of selected bins. The
bit-accurate model, the RTL, the testbench and the Pluto block design are here.

- Design note (derivations, formats, register map, deviations): [`docs/design.md`](docs/design.md)
- References: [`docs/references.md`](docs/references.md)

## Status

The bitstream is routed with timing met. The bit-exact simulation regression
passes. It has **not** yet been loaded on hardware.

### Implementation (Vivado 2026.1, ADI hdl_2026_r1, xc7z010clg225-1, routed)

| Build | LUT | FF | BRAM36 | DSP | Slices | WNS | WHS | WPWS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Stock ADI `projects/pluto` | 12,673 (72.0%) | 22,125 | 2 | 72 (90%) | 99.7% | +0.936 | +0.013 | +0.264 |
| `pluto_rx`: TX DDS, TX IQ correction, TX interpolator removed | 7,124 (40.5%) | 13,312 | 2 | 34 | 80.5% | +1.130 | +0.012 | +0.264 |
| `pluto_chan`: RX-only + channelizer (also axi_ad9361 TDD off) | **13,066 (74.2%)** | 20,281 | 27.5 | 74 (92.5%) | 99.8% | **+0.242** | **+0.007** | +0.264 |

TNS and THS are 0. All constraints are met at 100 MHz (FCLK0) for the channelizer
and at the ADI-defined clocks elsewhere. No clock was loosened.

Channelizer share of the final build:

| Block | LUT | FF | BRAM36 | RAMB18 | DSP |
| --- | ---: | ---: | ---: | ---: | ---: |
| `chan` (RTL) | 3,421 | 3,535 | 9 | 19 | 18 |
| `chan_fft` (xfft) | 2,304 | 4,739 | 0 | 10 | 22 |
| `chan_hist_dma` + `chan_sel_dma` | 971 | 1,291 | 2 | 0 | 0 |

The first full build used the 18-bit, natural-order xfft and failed placement:
15,641 LUT, 89%, and the slices ran out. The fit was recovered three ways:
- xfft with 16-bit input, bit-reversed output and 4 BRAM stages;
- the activity pipeline restructured;
- axi_ad9361's internal TDD controller disabled. It only sequences TX/RX
  switching, so it falls under the authorized TX strip.

**Sign-off checks** (`results/pluto_chan_*.rpt`, compared with `results/pluto_rx_*`):
- **check_timing:** no unclocked or unconstrained internal paths. The only items
  are the stock design's 16 input and 20 output ports with no I/O delay
  (TIMING-18), the same as the RX-only build.
- **report_cdc:** the channelizer contributes 3 rows, all CDC-3 Info (synchronized
  with ASYNC_REG). Every Critical and Warning row is in ADI cores and appears
  identically in the RX-only build (CDC-1 ×55, CDC-13 ×12, etc.). The only
  crossing is l_clk → FCLK0, through xpm_fifo_async, xpm_cdc_single and
  xpm_cdc_gray. No clock groups or false paths were added.
- **report_methodology:**
  - Same as the RX-only build: TIMING-54 ×5, TIMING-9, TIMING-18 ×36 and ULMTCS-1,
    all from ADI constraints and cores.
  - New XDCB-5 ×4: from axi_dmac's own XDC, for the two new DMA instances.
  - New RTGT-1: advisory. The bitmap LUTRAMs could be packed as RAM32M.
- **report_drc:** DPIP-1 ×4 is new. The squarer inputs in `chan_post` are not
  DSP-registered; timing is met with slack. The rest matches the RX-only build.
- **DSP/BRAM inference confirmed** in the utilization report:
  - FIR: 16 DSP48E1, plus 8 history and 4 coefficient BRAMs.
  - Power path: 2 DSP48E1.
  - Activity RAMs, frame buffer and FIFOs: BRAM.

### Model results (`model/results/*.json`)

**Prototype filter.** 4096 taps (8 per branch), Kaiser β = 9.4, −6 dB at 50 kHz.

| Metric | Value |
| --- | --- |
| Passband ripple over ±31.25 kHz | 0.0004 dB |
| Worst alias into the protected band (≥ 68.75 kHz) | −94.2 dB |
| Same, measured in fixed point with 12-bit input | −91.8 dB |
| Response at bin edge (25 kHz) / at 31.25 kHz | 0.00 dB / −0.0002 dB |
| ARM C reference filter at 512 bins, same metrics | −9.5 dB at 25 kHz, −24 dB at 31.25 kHz |

**Real DMR (fixture) placed across a bin.** Recovered SNR is measured after a host
filter equivalent to `engine/src/dsp.rs` (`straddle.json`).

| Offset from bin centre | 0 | 6.25 kHz | 12.5 kHz | 18.75 kHz | 25 kHz |
| --- | ---: | ---: | ---: | ---: | ---: |
| FPGA path, strong signal | 73.0 dB | 72.9 | 73.0 | 72.9 | 73.0 |
| ARM C filter, same signal | 72.9 | 53.6 | 30.5 | 18.5 | 11.4 |

- **Near/far.** An interferer 40–60 dB stronger at +25, +37.5, +50, +75, +100 or
  +150 kHz leaves the desired channel at its noise-limited SNR at every offset.
- **FPGA loss vs float:** ≤ 0.20 dB in every case.
- **The exception is +12.5 kHz at 40 dB:** −8.8 dB for float, fixed and the C
  filter alike. That is the host's channel filter. Its Hann-windowed sinc can't
  reject a 40 dB-stronger adjacent DMR channel. It's a Rust-side issue, not a
  channelizer one.

**Off-air checks.**
- **20 Msps snapshot resampled to 25.6 Msps** (`real.json`): the worst bin's error
  from fixed-point vs float is −55.9 dB relative to that bin's power, an SNR loss
  of 1e-5 dB. The 16-bit slice costs at most 0.0003 dB.
- **Off-air DMR at 454.029 MHz** (`real_dmr.json`): through bin 508 plus the host
  filter, it agrees with a direct mix of the same samples to 69.1 dB. Float and
  fixed match.
- **Synthetic noise, quantization budget** (bin noise vs FPGA rounding):

  | Input noise | FIR + FFT core | 16-bit slice at the default ×16 gain |
  | ---: | ---: | ---: |
  | ADC quantization only | 0.18 dB | 2.6 dB |
  | 1 LSB rms | 0.026 dB | 0.64 dB |
  | 3 LSB rms | 0.003 dB | 0.09 dB |

  Real captures show about 90 LSB rms of wideband noise. OUT_SHIFT trades
  headroom for resolution.

**Activity detector** (`activity.json`):
- **Welch estimate:** 256-frame (2.56 ms) blocks, ν = 500.8 equivalent dof.
- **Floor:** minimum statistics over 1.3–1.6 s.
- **Thresholds (order-statistic CFAR):** open +2.60 dB (Pfa 1e-8 per bin-block,
  about 7 false opens per hour across the band); close +1.57 dB; hang 100 ms.
- **Monte Carlo of the bit-exact detector:** 7.9e-5 measured vs 1e-4 predicted
  (worst-case window); 0 opens in 1.02e7 trials at the operating point.
- **Detection:** Pd = 0.94 at −2 dB in-bin SNR, which is about +6.7 dB in a
  12.5 kHz channel.
- **Requires manual gain.** On the AGC (fast_attack) capture the floor jumps up
  to 4.7 dB block-to-block, and every bin reads active (design.md §5).

**Unit tests:** `uv run pytest -q fpga/model/test_model.py` passes 54 tests,
including frame and phase alignment against the ARM C reference
(`offload/arm/pfb.c`) built for the host.

### Simulation (`scripts/run_sim.sh`, xsim, bit-accurate vectors from `model/golden.py`)

- **Bit-exact checks.** Every history and selected-bin record is compared
  word-for-word against the model by frame number. The LEVEL/PSD/ACTIVE windows
  are compared at the end.
- **Protocol checkers.** AMD AXI4-Lite (PG101) and AXI4-Stream (PG145) checkers
  watch the register bus, both FFT streams and both DMA streams.
- **Scenarios:**

| Scenario | Exercises | Result |
| --- | --- | --- |
| mixed | tones at DC, bin centres, bin edges, 31.25 kHz; keyed carrier; noise; ADC valid gaps; 70% ready | <!-- R:mixed --> |
| fullscale | 12-bit clipping, DC, 16-bit output saturation (OUT_SHIFT 2) | <!-- R:fullscale --> |
| random | full-range random input, 64 random selected bins, 50% ready | <!-- R:random --> |
| backpressure | 4% ready: frame drops, discontinuity flags, record integrity | <!-- R:backpressure --> |
| overflow | ADC at 200 Msps: CDC overflow counted and flagged | <!-- R:overflow --> |
| reset | RUN=0 mid-record (record completes), then system and ADC-core reset mid-stream, defaults, replay | <!-- R:reset --> |
| snapshot | freeze, read back the exact input window and output frame, compare, resume | <!-- R:snapshot --> |
| autofwd | AUTO_FORWARD (mask ∪ ACTIVE) and DET_RESET while running | <!-- R:autofwd --> |

Every run also reads back the reset defaults and the configuration registers.
Where nothing is dropped, it checks that every frame produced exactly one record
per stream.

## Hardware: what is ready, what was not run

Nothing has been loaded onto or flashed to the Pluto from this work. Read-only
probes, done under `/tmp/pluto-radio.lock`, established:
- the FPGA manager is present;
- device-tree overlays are not available;
- the driver bindings for the PL devices;
- U-Boot's load addresses;
- the CMA placement.

Prepared artifacts are in `~/.cache/amateur-radio-fpga/release/pluto_chan-<sha>/`,
with a `SHA256SUMS` file:

| File | Purpose |
| --- | --- |
| `system_top.bit` / `.xsa` | bitstream / hardware handoff |
| `system_top.bit.bin` | for `/sys/class/fpga_manager` (bootgen `-process_bitstream bin`) |
| `pluto.frm` | ADI's public v0.37 image (the unit's version) with this bitstream and a no-map 64 MiB DMA carve-out at 0x04000000; kernel and ramdisk byte-identical (`sw/make_frm.py`) |
| `chanctl` | static ARMv7 tool: `info`, `start`, `stop`, `psd`, `level`, `active`, `set-det`, `det-reset`, `snap`, `dma` |

**Staged plan** (not executed: the session's permission policy blocks writes to
the device, see the final report):
1. `sw/pluto_runtime_load.sh load system_top.bit.bin chanctl`. This checks the
   hash, then runs the unbind → fpga_manager → state check → rebind sequence
   detached on the device. It reboots to stock if programming fails, and
   restores and reads back the receive settings.
2. Under the lock (or the live service's borrow command, once it exists), set RX
   to 25.6 Msps, 20 MHz, LO 460 MHz, manual gain. Then:
   - `chanctl start --hist --sel --bins …`;
   - check FRAMES advances at 100,000/s at 25.6 Msps, with ADC_LOSSES and
     PACK_OVERRUNS at 0;
   - `chanctl snap` ×3, then `model/live_check.py snap` must report 0
     mismatches. That is a bit-exact hardware vs model check on live signals.
   - `chanctl psd` against a contemporaneous IIO capture, through
     `live_check.py psd`.
3. `sw/pluto_runtime_load.sh restore` reboots to the stock image and re-applies
   the saved settings.
4. Only after 1–3 pass: boot-test `pluto.frm` from RAM (DFU-RAM `ramboot`) and
   check the kernel log for the reserved region and the initrd. Then flash per
   ADI's documented update path, with the recovery plan in `offload/RECOVERY.md`
   ready. The DMA streams (`chanctl dma`) need the carve-out, so they can only be
   exercised after this stage.

## Reproduce

Requirements: Vivado 2026.1, ADI hdl `hdl_2026_r1` with its libraries built
(`ADI_HDL_DIR`, default `~/.cache/amateur-radio-fpga/hdl`), and uv. Build outputs
go to `~/.cache/amateur-radio-fpga/`, never into Git.

```sh
PYTHONPATH=. uv run python -m fpga.model.validate all        # model results (private recordings)
uv run pytest -q fpga/model/test_model.py                    # model unit tests
PYTHONPATH=. uv run python -m fpga.model.golden mems fpga/rtl/mem
fpga/scripts/run_sim.sh                                      # bit-exact regression (all scenarios)
fpga/scripts/build_project.sh pluto_chan                     # bitstream + reports/ (~35 min)
fpga/sw/build.sh                                             # chanctl
python3 fpga/sw/make_frm.py <plutosdr-fw-v0.37/pluto.frm> <system_top.bit> <out/pluto.frm>
```

The xfft bit-accurate C model ships with Vivado. The Python CLIs set its
`LD_LIBRARY_PATH` themselves.

## Open issues

1. **Hardware validation has not run.** Runtime load, live snapshot and PSD
   checks, then firmware ramboot and flash, as above.
2. **ARM/RIQ1 consumer.** No ARM producer reads the selected-bin records yet. The
   mapping is direct:
   - stream = bin;
   - `first_sample` = frame;
   - header bit0/bit1 → discontinuity/overflow, GAP;
   - `hardware_counter: true` from the frame numbers and SAMPLES.
   That belongs to the ARM/Rust side.
3. **Activity detection needs manual gain.** Under AGC the floor tracker can't work.
   Cross-bin normalization (a per-block percentile across bins) is the candidate fix.
4. **DMA access path.** DMA is driven from user space through `/dev/mem`. An ADI
   IIO driver for the two streams is the conventional follow-up.
5. **Fit margin.** Slices are 99.8% packed, DSP 92.5%. Further logic needs the
   R2²SDF FFT (about −1k LUT and −9 DSP) or giving up the RX low-rate decimator.
6. **Host channel filter.** It limits 12.5 kHz adjacent-channel rejection
   (−8.8 dB SNR with a 40 dB-stronger neighbour), whatever the channelizer does.

## Next phase: freeing DSPs (planned, not started; waits for on-device validation)

The DDR history ring already exists and uses **no** DSPs: `chan_hist_dma`, in
cyclic mode, writes every bin of every frame. What gates it is the carve-out in
the flashed image, not fabric. DSPs matter for 1024 bins. Where the 74 used DSPs
are: axi_ad9361 RX 12 (4 channels × DC filter + IQ correction), rx_fir_decimator
22, PFB FIR 16, power 2, xfft 22.

| Candidate | Expected saving | Notes |
| --- | --- | --- |
| (1) remove rx_fir_decimator | 22 DSP, ~620 LUT, ~800 SRL | Breaks stock RX below 2.08 Msps |
| (2) ADC_DCFILTER_DISABLE + ADC_IQCORRECTION_DISABLE | 12 DSP, ~600 LUT | Relies on AD9363 tracking. Re-run `real*`/straddle on captures taken with the cores off |
| (3) symmetric FIR via DSP pre-adder | none for this structure | Symmetry pairs h[512t+j] with h[512(7−t)+511−j], i.e. different branches (u_j vs u_511−j). A branch has no equal coefficients to pre-add. Computing u_j and u_511−j together is possible but needs both data sets at once (twice the bank reads) |
| (4) FIR at 200 MHz (FCLK1), 2 cycles/branch | 8 DSP | Adds a clock domain, tight on a 99.8%-packed −1 part |
| (5) 3-mult twiddles | xfft already uses `use_mults_resources` (22 DSP) | A custom R2²SDF with 3-DSP multipliers needs 13 DSP and about 1k fewer LUT |

1024 bins (25 kHz spacing, 50 ksps output at 2x oversampling) doubles the FIR
banks and coefficient memory (+12 RAMB18), the FFT size and the activity RAMs. The
MAC rate per input sample is unchanged: 1024 branch sums per 512-sample hop, still
one per clock at 100 MHz.
