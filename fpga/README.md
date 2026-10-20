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
| `pluto-fpga-source` | static ARMv7 RIQ1 producer (`sw/pluto_fpga_source.c`), needs the flashed image's carve-out. It serves TCP 9010 like the ARM producer: CONFIG with 512 streams (stream = bin + 1, 100 ksps, `hardware_counter: true`), IQ for selected/pinned bins, PSD and activity from the FPGA, and `get_config`/`pin`/`stop` controls, with RX controls restored on stop. Start: `/tmp/pluto-fpga-source --center 460000000 --gain 40 --forward-streams 1` |

**Staged plan.** Not executed: the session's permission classifier denied the
runtime load as "Remote Shell Writes". Running it needs a permission rule, or
someone with device-write permission. A read-only check at 04:15 Pluto time found
the device on stock v0.37 with nothing from this work on it.
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

## Optimization phase (branch `fpga-optimize`): status and resume plan

**Paused 2026-10-19 ~21:50** by the orchestrator to give Vivado to the
channelizer's critical-path rebuild. Resume only after the channelizer is live on
the Pluto, one Vivado job at a time, under
`systemd-run --user --scope --slice=radio-heavy.slice -p MemoryMax=12G`. Scope is
hobby scale: the DDR history ring work (item 6) is **optional**, and 1024 bins
(item 7) is **dropped** unless Cory asks for it. Stay off the Pluto, ESXi and
vSwitchPluto until told otherwise (home-it-services#258).

Where the 74 DSP48E1 of the baseline go: axi_ad9361 RX 12 (4 channels × DC filter
+ IQ correction), rx_fir_decimator 22, PFB FIR 16, power 2, xfft 22.

### Measured so far

| Item | Result | Evidence |
| --- | --- | --- |
| (1) remove rx_fir_decimator (`CHAN_OPTS=nodecim`) | NODECIM_RESULT | `~/.cache/amateur-radio-fpga/optimize/b_nodecim/pluto_chan/reports/` (baseline RTL from `src-base/`, only this bd change) |
| (1) quality | None on the channelizer by construction: it taps axi_ad9361 directly, never through the decimator. Stock IIO RX: the decimator only acts when the driver sets its GPIO for rates below the AD9363 minimum (x8 FPGA decimation); libad9361 rates down to 520.8 ksps use the AD9363's own FIR and are unaffected. With it removed, an IIO request for the FPGA /8 rate would deliver undecimated data. | `projects/pluto_chan/system_bd.tcl` |
| (2) symmetric FIR via the DSP48E1 pre-adder | **Not applicable, 0 DSP saved, no model change.** A pre-adder saves a multiply only when two samples meet the same coefficient in the same sum. In branch j the 8 taps h[512t+j] are distinct; the symmetric partner of h[n] is h[4095−n] = h[512(7−t)+(511−j)], in branch 511−j. Across frames a sample at window offset n meets h[n−256Δ]; meeting its partner would need n−256Δ = 4095−n, i.e. 2n = 4095+256Δ, even = odd. So no product pair exists to fold. | this table |
| (3) FIR double-pumped at 200 MHz (`FIR2X=1`, 8 DSP instead of 16) | RTL done (`chan_pfb_fir.v` g_mul2x, MMCME2 in `chan_top.v`). Bit-exact by construction (exact integer sums, same p6 cycle as the 1x path). xsim `mixed` with FIR2X=1: all 48 history and 48 selected records matched the model word for word. Not yet built or timed. | sim log was in `optimize/sim/` (overwritten); rerun |
| (5) ADC_DCFILTER/IQCORRECTION off (`CHAN_OPTS=nocorr`) | At driver defaults both cores are identity: ADI's `ad9361_conv.c` writes IQ coefficients 1.0/0.0 with IQCOR_ENB and leaves the DC filter off with offset 0; `ad_iqcor` at 0x4000 gives p[29:14] = input exactly, `ad_dcfilter` disabled gives input + 0. So removal changes no bits unless someone sets calibscale/calibphase/calibbias/HPF. Real-IQ DC spur and image numbers: CORR_RESULT. Build not run. | `/tmp/claude-1000/adisrc/` driver copies; `fpga/model/corrections.py` if present |
| (4) xfft 3-mult twiddles | Already selected (`use_mults_resources`). A phase-factor-width / complex-mult sweep was started and stopped: XFFT_RESULT | `optimize/xfft_sweep/summary.csv` |

### Half-built (on this branch, simulated only partly)

- **History ring additions** (all on top of the existing `chan_hist_dma` cyclic
  ring): HIST_WINDOW (first bin + bin pairs, wraps past 511), HIST_RECORDS /
  HIST_LAST_FRAME / HIST_DONE ring-position registers, an activity event FIFO
  (EVT_STATUS/HEAD/FRAME/POP, 0x024–0x027) stamped with the deciding frame,
  VERSION 1.1.0, STATUS bit 4 = FIR clock locked. Register map and sizing are in
  `docs/design.md` sections 7–9. The ring-position registers checked out in the
  `mixed` sim; the event check failed only on testbench bugs, since fixed
  (`tb_chan.sv` now drains events concurrently through a semaphore-shared AXI
  master and runs `mixed` unpolled to overflow the 512-entry FIFO), not yet rerun.
- **Carve-out** grown from 64 to 160 MiB (`sw/make_frm.py`, 0x04000000–0x0DFFFFFF,
  below the CMA pool at 0x0E400000). Not built into an image; CMA placement with
  the bigger carve-out is unverified on hardware.
- **chanctl**: reads the carve-out from the device tree; new `ring start|status|
  read`, `events`, `preroll` commands. Cross-compiles; never run on hardware.
- **PROTOCOL.md**: "FPGA channelizer source" section (frame number = sample
  position, `hardware_counter: true`, GAP/overflow mapping, pre-roll from the
  ring on open events). `sw/pluto_fpga_source.c` does not use the ring yet.
- Testbench/model: `golden.py` emits events.hex and window/poll plusargs;
  `sim_run.tcl` takes `TB_FIR2X=0|1`; `sim_setup.tcl` adds only tb_chan.sv.

### Next steps, in order

1. Rerun the regression (`FPGA_SIM=<own dir> fpga/scripts/run_sim.sh`), one chain
   at a time: all 8 scenarios with FIR2X=1, then `mixed` and `random` with
   `TB_FIR2X=0`. Fix anything the event/window checks find.
2. Build `CHAN_OPTS=nocorr` from `optimize/src-base` (baseline RTL) to measure (5)
   alone; then the final set from this branch (default `CHAN_OPTS="nodecim fir2x"`,
   add `nocorr` only if the corrections analysis shows nothing lost). Sign-off
   reports via `scripts/reports.tcl`; check that the clk2x paths meet 5 ns and that
   report_methodology has nothing new beyond MMCM advisories.
3. If staying hobby-scale, stop there for fabric: (1) + (3) take DSP from 74 to
   44 without touching quality. Leave the ring additions in only if they built
   clean; otherwise revert chan_pack/chan_activity/chan_top ring changes.
4. On-device (after the channelizer agent is done and the Pluto is free): runtime
   load, `chanctl info` (VERSION 0x00010100, STATUS bit 4), `snap` + `live_check.py
   snap` bit-exact, read axi_ad9361 CHAN_CNTRL 0x79020400/0x79020404/0x79020414 to
   confirm identity corrections; with a carve-out image, `chanctl ring start`,
   `events`, `preroll`, and HIST_DROPS = 0 for minutes with the selected stream on.
5. el-review on the diff (project FPGA personas in `.claude/agents/` plus
   built-ins), then push and report on issue #1.

Dropped: 1024 bins. Optional: the ring additions above and wiring pre-roll into
`pluto_fpga_source.c`.
