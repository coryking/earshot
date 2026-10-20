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
| `pluto_rx`: TX DDS, TX IQ correction, TX interpolator removed (built before TDD_DISABLE was added to the overlay) | 7,124 (40.5%) | 13,312 | 2 | 34 | 80.5% | +1.130 | +0.012 | +0.264 |
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

## Live bring-up (for the agent doing it)

One path: check the bitstream with a runtime load, flash it, then run
`pluto-fpga-source` under a plain restart loop. Once it runs, it is the only
producer. The stock ARM producer and the supervisor's image detection and
profiles are retired, not extended.

**State at hand-off.**
- Nothing from this work has been loaded or flashed. The Pluto runs stock ADI
  v0.37.
- **Wait for home-it-services#258 before any of this.** A Pluto reboot currently
  drops its network link (the ESXi vSwitchPluto uplink), and a Codex session is
  fixing that. Both the runtime load's fail-safe and the flash end in a reboot.
- The release bundle, with `SHA256SUMS`, is listed under "Release" below:
  `system_top.bit` / `.xsa`, `system_top.bit.bin` (for fpga_manager), `pluto.frm`
  (flash image), `chanctl`, `pluto-fpga-source`.

**Rules.** RX only. Use radio-borrow (`offload/radio-borrow` on main) for any
Pluto access while the stock live receiver is still running: it stops live,
restores the radio, runs your command and resumes live. Device-write commands
were denied by this session's permission classifier ("Remote Shell Writes");
check that you have permission before starting.

### 1. Runtime load and check (non-persistent)

```sh
B=~/.cache/amateur-radio-fpga/release/pluto_chan-<sha>
offload/radio-borrow fpga/sw/pluto_runtime_load.sh load $B/system_top.bit.bin $B/chanctl
```

`pluto_runtime_load.sh` saves the RX settings, copies the `.bin` to
`/lib/firmware` (sha256 checked), then runs a detached swap script on the Pluto.
That script kills any producer, unbinds the PL drivers, programs through
`/sys/class/fpga_manager/fpga0`, rebinds and touches `/tmp/pl_is_chan`. If the
state isn't `operating` it reboots the Pluto to stock. The RX settings are then
re-applied and read back. Log: `~/.cache/amateur-radio-fpga/pluto-session/session.log`.

Check, inside one `radio-borrow --lock` session, with RX set by hand (25.6 Msps,
20 MHz, LO 460 MHz, `gain_control_mode=manual`, gain 40):
- `/tmp/chanctl start --sel --bins 1`, then `/tmp/chanctl info` twice 1 s apart:
  FRAMES advances about 100,000/s; `adc_losses`, `pack_overruns`, `fft_events` stay 0.
- `/tmp/chanctl snap /tmp/s.bin`, copy it back, then
  `PYTHONPATH=. uv run python -m fpga.model.live_check snap s.bin --out-shift 7`.
  It **must report 0 mismatches** (hardware vs model, bit-exact, live input), with
  `input_peak_adc` ≤ 2048.
- `/tmp/chanctl psd`: occupied bins in 450–470 MHz stand out (bin k at LO + k·50 kHz,
  k ≥ 256 negative). Then `/tmp/chanctl stop`.

The DMA streams need the flashed carve-out, so they aren't tested here.
`fpga/sw/pluto_runtime_load.sh restore` reboots to stock.

### 2. Flash

`pluto.frm` in the bundle is ADI's v0.37 release image with the FPGA swapped and a
no-map `reserved-memory/chan_dma@4000000` (64 MiB at 0x04000000) added to every DT
(`fpga/sw/make_frm.py`; kernel and ramdisk byte-identical). Flash it with the stock
updater (checked in the v0.37 ramdisk):

```sh
scp -O pluto.frm root@192.168.10.10:/tmp/pluto.frm
ssh root@192.168.10.10 'update_frm.sh /tmp/pluto.frm && reboot'
```

`update_frm.sh` checks the md5 trailer and FIT magic, writes `/dev/mtdblock3`
(firmware only, never boot or env) and sets `fit_size`; it doesn't reboot on its
own. After the reboot, check `/proc/iomem`: System RAM excludes
0x04000000–0x07FFFFFF. Record the hashes on issue #1. Recovery if it doesn't
boot: USB DFU from the Mac, `offload/RECOVERY.md`.

### 3. Run the producer

Stop the stock live receiver first, so nothing else opens the radio. Then run
the producer from the host in a restart loop. The loop re-copies the binary each
time, since `/tmp` on the Pluto doesn't survive a reboot:

```sh
B=~/.cache/amateur-radio-fpga/release/pluto_chan-<sha>
while :; do
  scp -O $B/pluto-fpga-source root@192.168.10.10:/tmp/ &&
  ssh root@192.168.10.10 'chmod +x /tmp/pluto-fpga-source &&
    exec /tmp/pluto-fpga-source --center 460000000 --gain 40 --forward-streams 1'
  sleep 5
done
```

`pluto-fpga-source` accepts the flashed image by its DT carve-out node. It:
- listens on TCP 9010 (`pluto-fpga-source waiting on TCP:9010; radio unopened`);
- opens RX when a client connects, saving and restoring the RX settings;
- serves RIQ1 with 512 streams (stream = bin + 1, 100 ksps, centre LO + k·50 kHz,
  `hardware_counter: true`);
- sends IQ for forwarded or pinned bins, PSD every 100 ms, and activity for
  active, forwarded or pinned bins (all bins every 2 s);
- accepts `get_config`, `pin` and `stop`.

**Unverified or risky** (none of this has run on hardware):
- hdl_2026_r1 IP against the v0.37 kernel 5.10 drivers: cf_axi_dds probes a core
  with DDS and TX IQ correction off, and axi_ad9361 has TDD off. A probe failure
  aborts the runtime load before `PL_SWAP_DONE`; run restore.
- During the swap the AD9363 RESETB/ENABLE/TXNRX pins pass through the PL and
  float. Nothing transmits by design (TX LO off, DDS scale 0), but the loader
  doesn't explicitly hold TX quiet.
- The PL rebind can renumber IIO devices under iiod. Network libiio clients may
  need an iiod restart; sysfs is unaffected.
- `pluto-fpga-source` has never run. Its record parser is checked against the
  format spec, not against an RTL sel-stream dump.
- The detector thresholds assume manual gain; under AGC every bin reads active.

## Reproduce

Requirements: Vivado 2026.1, ADI hdl `hdl_2026_r1` with its libraries built
(`ADI_HDL_DIR`, default `~/.cache/amateur-radio-fpga/hdl`), and uv. Build outputs
go to `~/.cache/amateur-radio-fpga/`, never into Git.

```sh
PYTHONPATH=. uv run python -m fpga.model.validate all        # model results (private recordings)
uv run pytest -q fpga/model/test_model.py                    # model unit tests
PYTHONPATH=. uv run python -m fpga.model.golden mems fpga/rtl/mem
fpga/scripts/run_sim.sh                                      # bit-exact regression (all scenarios)
fpga/scripts/build_project.sh pluto_chan                     # clean bitstream + reports build (default)
fpga/scripts/build_project.sh --incremental pluto_chan       # reuse a compatible routed reference
fpga/sw/build.sh                                             # chanctl
python3 fpga/sw/make_frm.py <plutosdr-fw-v0.37/pluto.frm> <system_top.bit> <out/pluto.frm>
```

`--clean` removes generated runs and any saved reference checkpoint. The opt-in
`--incremental` mode keeps a routed `reference.dcp` outside the generated run
directories and lets ADI's project flow use it for incremental placement and
routing. Vivado, ADI HDL, or target/run setup changes invalidate that reference.
RTL, block-design, and IP edits can use it as a physical reference; Vivado may
decline reuse when the design changed too much. The checkpoint is refreshed
only after a successful route with worst setup slack of at least -0.250 ns. If
no compatible reference exists, that build runs without implementation reuse
and can establish one for a later build. Both modes still recreate the project
and run OOC synthesis; this path targets implementation, which took about 12
minutes in the last recorded full build. Its speed-up has not been measured.

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
6. **Detector floor freeze (P2, review 20261019-212400).** Freezing the floor
   while a bin is active keeps long overs open, but false opens and small floor
   rises now latch for up to about 30 s. The level-gated fix is in design.md §5,
   not yet implemented. Carriers on for longer than about 31 s read inactive.
   Also open: a frame that starts during a DET_RESET sweep is still counted as
   whole (P3; it only follows an operator reset).
7. **Host channel filter.** It limits 12.5 kHz adjacent-channel rejection
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
