# Verified state at the migration checkpoint

## Device and recovery

The connected unit is physical PlutoSDR Rev.B, Z7010/AD9363. Its expanded IIO
AD9364 label does not identify different physical silicon. Existing Linux is
5.10.0-98725-g3eae70065be9, Buildroot 2022.02.3, uClibc 1.0.41, libiio 0.24.
Both Cortex-A9 CPUs are enabled at approximately 667 MHz; NEON is available.
RAM reports 508540 kB.

All four NOR partitions were backed up and verified against a second device
read. The device entered DFU, all expected alternate names enumerated, and all
32 MiB uploaded over DFU matched the original partition hashes. It then booted
its original Linux. No flash download/write command was issued. See RECOVERY.md
and the protected backup's manifest/proof files for exact artifacts and hashes.

## ARM feasibility

The portable C channelizer is a 2x-oversampled, eight-tap-per-phase polyphase
filter bank with a radix-2 FFT. It preserves complex phase, including odd/negative
bins. Independent SciPy mixer/FIR comparisons pass with address/undefined
sanitizers. Its current filter is deliberately narrow; arbitrary RF targets near
bin boundaries require a different/coarser profile or additional overlap.

Measured **on Pluto**, compute-only, one ARM core:

| Shape | Input samples per second |
| --- | ---: |
| 2048 bins, 8 taps | 1.128 million |
| 64 bins, 8 taps | 1.785 million |
| 64 bins, 8 taps, including gate/quantization | 1.158 million |

The 25.6 Msps input needed for the proposed full-window design is far beyond
this ARM implementation. FPGA channelization is required. Mac compute-only
2048-bin performance was 53.45 Msps; that is not a device performance claim.

## Actual RF and transport proof

`pluto-channel-source` runs as a volatile `/tmp` executable against the original
firmware. Its tested experimental profile is 800 ksps, 800 kHz RF bandwidth,
64 bins, 25 ksps complex per stream, LO 454216998 Hz. Stream 50 centers at
454029498 Hz, close to the previously measured DMR carrier at 454029223 Hz.

The first all-active attempt processed only 6.29 seconds of signal in 12.23
seconds of wall time. Quantizer and vectored-write fixes improved throughput,
but forwarding dozens of falsely opened noise gates still exceeded the budget.
Do not use that mode as a real-time full-band receiver.

The final bounded selected-stream proof received 291060 complex samples for
stream 50, 1137 IQ messages, 97 PSD messages, and 6208 activity messages over
12.22 seconds including RFIC setup. Device input was 9322496 samples, equivalent
to 11.65 seconds at 800 ksps; seven processing blocks exceeded their individual
time budget. All emitted per-stream message sequences were contiguous and no
IQ sample indices repeated. Hardware continuity remains **unknown**, because
stock DMA has no independent sample counter. There was no Rust/browser test
against this actual RF stream before the migration checkpoint; the Rust agent
validated its native pipeline with a separate fixture.

CONTROL `stop` acknowledged only after restoring and reading back RX sample
rate/bandwidth/LO/gain, both RFIC FIR enable states, and the prior TX baseband
clock. Verified restored state: 5 Msps, 5 MHz, 454216998 Hz, fast_attack, RX FIR
0, TX FIR 0, TX sample rate 5 Msps. No TX streaming device is opened. Low-rate
setup uses the installed ADI libad9361 helper; it loads dormant FIR coefficients
and changes shared clocks transiently. The prototype refuses to overwrite an
already enabled custom FIR.

## Known limits and next implementation

The global-median RF gate has false positives with uneven RFIC passband/noise;
CONFIG labels it experimental and uncalibrated. The ARM source forwards only
configured stream IDs (at most eight) or host-pinned streams; other streams keep
PSD/activity/catalog metadata. It supports get_config, pin, and stop; live
rate/LO configuration is explicitly unsupported. This is a bounded integration
prototype, not the completed 20 MHz feature.

The agreed target is a coarse FPGA PFB (initial candidate: 512 bins at
25.6 Msps, 50 kHz spacing, 100 ksps complex output), fine PSD activity detection,
a DDR history ring, and selected complex slices over RIQ1. Rust performs final
logical-channel mixing/filtering before FM/codec decoding. Several logical RF
targets may share one physical IQ stream. The precise FPGA filter, resource fit,
DMA layout, device counters, overflow handling, and calibrated activity gate
remain to be implemented and measured.

Vivado installation is owned by IT services and was still in progress when the
user paused work. Do not run a competing installer. Maia upstream expects
Vivado/Vitis/XSCT/bootgen 2023.2; verify the actual installation, device support,
and version compatibility before building. Hardware target is `pluto`,
`xc7z010clg225-1`, not Pluto+ `clg400`.
