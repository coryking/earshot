# Pluto FPGA coarse channelizer

A 512-bin, 2x-oversampled polyphase filter bank for the PlutoSDR Rev.B
(xc7z010clg225-1). At 25.6 Msps it delivers 50 kHz bins at 100 ksps complex, plus
per-bin PSD and activity detection. Two ADI axi_dmac streams carry the output: a
DDR history ring of every bin, and a forwarded stream of selected bins. The
bit-accurate model, the RTL, the testbench and the Pluto block design are here.

- Design note (derivations, formats, register map, deviations): [`docs/design.md`](docs/design.md)
- References: [`docs/references.md`](docs/references.md)

## Status

The routed bitstream meets timing and has been tested on the Pluto Rev.B. A live
25.6 Msps input advanced at 100,000 channelizer frames/s; a frozen frame matched
the bit-exact model in all 512 bins. Selected DMA and the RIQ1 producer delivered
continuous 100 ksps IQ, measured FPGA PSD, and live unencrypted DMR audio. The
boot image currently reserves DMA memory while the tested FPGA image is loaded at
runtime.

### Implementation (Vivado 2026.1, ADI hdl_2026_r1, xc7z010clg225-1, routed)

| Build | LUT | FF | BRAM36 | DSP | Slices | WNS | WHS | WPWS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Stock ADI `projects/pluto` | 12,673 (72.0%) | 22,125 | 2 | 72 (90%) | 99.7% | +0.936 | +0.013 | +0.264 |
| `pluto_rx`: TX DDS, TX IQ correction, TX interpolator removed (built before TDD_DISABLE was added to the overlay) | 7,124 (40.5%) | 13,312 | 2 | 34 | 80.5% | +1.130 | +0.012 | +0.264 |
| `pluto_chan`: deployed `8cea8ae` RX-only channelizer (also axi_ad9361 TDD off) | **13,149 (74.7%)** | 20,319 | 27.5 | 74 (92.5%) | 99.7% | **+0.403** | **+0.013** | +0.264 |

TNS and THS are 0. All constraints are met at 100 MHz (FCLK0) for the channelizer
and at the ADI-defined clocks elsewhere. No clock was loosened.

Channelizer share in the earlier synthesis breakdown (the release-wide totals
above come from the exact deployed `8cea8ae` reports):

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

**Sign-off checks:** `results/pluto_chan_{check_timing,drc,methodology,timing_summary,utilization}.rpt`
are copied from the deployed `pluto_chan-8cea8ae/reports/` bundle; the other
reports in `results/` are from earlier comparison builds. Compared with
`results/pluto_rx_*`:
- **Baseline note:** the `pluto_rx_*` reports predate TDD_DISABLE. CDC-15 warning
  counts and clk_fpga_0→rx_clk endpoint counts therefore differ, but the critical
  counts match.
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

## Live operation

The Pluto Rev.B currently boots ADI v0.37 with a 64 MiB `no-map` reservation at
0x04000000 for DMA. The stock FPGA, kernel and ramdisk remain byte-identical to
the verified backup. `fpga/sw/ensure_pluto_fpga.sh` checks that reservation and
Linux's memory map, then loads the already routed image through FPGA manager if
needed. It checks the channelizer magic before reporting ready. The runtime loader
unbinds and rebinds the USB gadget around `iiod`; a 90-second on-device timer
reboots if the swap or USB enumeration stays incomplete. A configured gadget
and completed swap also cancel it if the host's check is delayed. A normal Pluto reboot loses the
runtime image. The installed `amateur-radio-live` preflight reloads it, and its
one-minute guard restarts the unit after an unexpected Pluto reboot.

On wigglebutt, build the ARM tools and supervisor, and install the home-it-services
radio units. With the managed units installed:

```sh
fpga/sw/build.sh ~/.cache/amateur-radio-fpga/sw
cargo build --release --locked --manifest-path supervisor/Cargo.toml
systemctl --user start amateur-radio amateur-radio-live
systemctl --user status amateur-radio amateur-radio-live
```

`amateur-radio-live` owns Pluto and starts the single RX-only producer. The
producer receives at 25.6 Msps, covers about 20 MHz around the configured LO,
and sends one 100 ksps IQ slice per selected bin. The engine pins the bin when a
browser selects a frequency; the selection stays armed through silence. The
measured 512-bin FPGA PSD is sent at 10 Hz. Activity reports are staggered
over those updates, refreshing each bin about once per second. Current activity
flags overreport occupied bins, so `activity_calibrated` is false and the host
does not use those flags to admit decoders. Read `/api/status` for live vs
reconnecting, drops, and measured spectrum geometry.

The default radio-reading LO is 460016998 Hz (roughly 450–470 MHz receive
window), with +23.3 ppm display correction and an initial forwarded bin near
the live 454.54375 MHz DMR voice channel. To move the window, put
`FPGA_CENTER=<radio-reading Hz>` in `~/.config/amateur-radio/fpga-window.env`
and restart `amateur-radio-live`; use the browser to choose an individual
frequency. On the tailnet, open
`https://radio-host.example.ts.net:8449/`, select a channel and
press Listen. The selection stays armed during silence; press Stop listening
to leave it, then select another frequency. The original boot image and DFU recovery procedure
are in `offload/RECOVERY.md`; do not flash an unchanged older channelizer image
that failed to boot during historical bring-up.

For a direct maintenance preflight, with no service owning Pluto:

```sh
B=~/.cache/amateur-radio-fpga/release/pluto_chan-8cea8ae
fpga/sw/ensure_pluto_fpga.sh "$B/system_top.bit.bin" "$B/chanctl"
```

This preflight is idempotent. It refuses DMA if the reservation is missing and
will not touch custom registers on a stock FPGA boot. Stop the live unit before
other hardware experiments; `offload/radio-borrow` manages that handoff.

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

## Current limitations

- Pluto's FPGA image is loaded into volatile PL after each boot. The installed
  guard handles a normal Pluto reboot, but physical USB/power faults can still
  require someone at the device. The hardware watchdog is fed by Linux and does
  not detect a detached but otherwise healthy USB gadget.
- The FPGA activity detector overflags quiet bins on live RF; PSD, IQ and DMR
  voice work, while the host treats activity as uncalibrated. See `docs/design.md`
  for the floor-tracking behavior. No new RTL was built for this milestone.
- The user-space `/dev/mem` DMA path is suitable for this prototype. A kernel
  driver and a persistent FPGA boot image are future reliability work.
- The host channel filter limits rejection of a 40 dB stronger adjacent DMR
  channel at a 12.5 kHz offset (model result: −8.8 dB SNR).

## Next phase: freeing DSPs (planned, not started)

The DDR history ring already exists and uses **no** DSPs: `chan_hist_dma`, in
cyclic mode, writes every bin of every frame. The installed DMA carve-out permits
testing it; host support for consuming that ring is still future work. DSPs matter
for 1024 bins. Where the 74 used DSPs
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
