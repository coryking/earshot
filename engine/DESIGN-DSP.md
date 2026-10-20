# Fine channel filtering and per-target activity: design basis

Scope: the Rust path after the producer (ARM prototype or FPGA slicer). Complex IQ
slices of 16 to 192 ksps, typically 25 ksps or the coarse 100 ksps slice, are mixed to
a target, band-limited, resampled to 48 kHz, FM-demodulated, and handed to DSDcc.

## Channel plan the filters are derived from

| Item | Value | Source |
| --- | --- | --- |
| 12.5 kHz class necessary bandwidth, F3E | 8.5 kHz or 11 kHz (Class A 11 kHz); max deviation +-2.5 kHz | ITU-R M.478-5, Annex 1 §1.1 and §2.2 |
| 12.5 kHz receiver adjacent channel selectivity | at least 60 dB | ITU-R M.478-5 §3.2.2 |
| 12.5 kHz transmitter adjacent channel power | at least 60 dB below carrier, 8.5 kHz measurement bandwidth | ITU-R M.478-5 §2.3.3 |
| Authorized bandwidth, 12.5 kHz equipment / 6.25 kHz equipment | 11.25 kHz / 6 kHz | 47 CFR §90.209(b)(5), footnote 3 |
| Emission mask, 12.5 kHz (Mask D) | 0 dB to 5.625 kHz from center, then at least 7.27(fd - 2.88 kHz) dB, 50 + 10 log P (or 70 dB) beyond 12.5 kHz | 47 CFR §90.210 |
| Emission mask, 6.25 kHz (Mask E) | 0 dB to 3.0 kHz, then at least 30 + 16.67(fd - 3 kHz) dB | 47 CFR §90.210 |
| DMR | 12.5 kHz channel, 2-slot TDMA, 4FSK, 4800 symbols/s | ETSI TS 102 361-1/-2 (radio performance by reference to EN 300 113) |
| NXDN 6.25 kHz mode | 4-level FSK, 4800 bit/s, about 4 kHz occupied bandwidth | NXDN Forum technical specifications and FAQ |

Not obtained (paywalled or unreadable in this session): the TIA-603-E and ETSI EN 300 113
clause text, and NXDN deviation / adjacent-channel figures. ITU-R M.478-5 carries the
12.5 kHz selectivity number and refers to TIA/EIA-603 (class A) and ETS 300 086 (class E)
for alternates. The 6.25 kHz stopband figure below is therefore a design choice.

## Filter specification (`ChannelSpec` in `src/dsp.rs`)

A neighbor on the 12.5 kHz raster is centered 12.5 kHz away and its Mask D 0 dB region
extends 5.625 kHz each way, so its energy begins 12.5 - 5.625 = 6.875 kHz from our
center. The passband must keep the full authorized bandwidth. That fixes the 12.5 kHz
class: passband edge 5.625 kHz, stopband edge 6.875 kHz, stopband at least 70 dB
(60 dB from M.478 plus 10 dB margin). The -6 dB point falls halfway, at 6.25 kHz, the
channel edge. The 6.25 kHz class follows the same logic from Mask E: a neighbor's energy
begins at 6.25 - 3.0 = 3.25 kHz; the passband stops at 2.5 kHz (NXDN occupies about
4 kHz), stopband 60 dB. NXDN48 therefore gets its own narrower filter chain in each
worker instead of sharing the 12.5 kHz one (at 6.25 kHz offset the 12.5 kHz filter is only
14 dB down, which cannot reject a 6.25 kHz neighbor).

## Design method

* Prototype lowpass: Kaiser-windowed sinc (Kaiser 1974; Oppenheim and Schafer,
  *Discrete-Time Signal Processing*). Kaiser's empirical beta, `0.1102 (A - 8.7)` for
  A > 50 dB, and order estimate `M = (A - 8) / (2.285 dw)` with `dw` in rad/sample.
  Formulas as given at <https://ccrma.stanford.edu/~jos/sasp/Hood_kaiserord.html>.
  For the 12.5 kHz class at 100 ksps that is about 346 taps; at 25 ksps about 87.
* Structure: polyphase rational resampler (Crochiere and Rabiner, *Multirate Digital
  Signal Processing*, 1983). 100 ksps to 48 kHz is 12/25, so 12 phases; the kernel is a
  continuous-time Kaiser sinc sampled at the needed fractional delays, rows normalized to
  unit DC gain. The mixer is a recursive complex oscillator re-normalized every 4096
  samples. The stopband extends to the input Nyquist, so images and aliases that fold
  into the 0 to 5.6 kHz output passband are covered.
* Single-stage is deliberate: the coarse 100 ksps slice is already the first decimation
  stage (FPGA PFB). The wideband to 100 ksps decimation in the tests is a Kaiser
  lowpass plus decimate-by-60, the multistage structure recommended by Crochiere and
  Rabiner for large ratios; fred harris, *Multirate Signal Processing for Communication
  Systems* (2004), gives the same tap-count rule of thumb (N about fs A / (22 df)).
  Neither book was available to read here; the formulas above are the textbook ones and
  are verified numerically below rather than assumed.

## Measured (tests in `src/dsp.rs`, run in every `cargo test`)

Tone sweeps through the real filter, inputs of 25, 50, 100 and 192 ksps:

| Class | Passband ripple | Gain at channel edge | Worst stopband (from stop edge to Nyquist) |
| --- | --- | --- | --- |
| 12.5 kHz | 0.0013 to 0.0022 dB | -6.02 dB | -71.0 dB (target -70) |
| 6.25 kHz | 0.005 to 0.006 dB | -6.02 dB | -60.7 dB (target -60) |

Adjacent-channel selectivity (`adjacent_channel_selectivity_at_100_ksps_exceeds_60_db`):
an FM interferer (2.5 kHz deviation) one raster step away, 60 dB stronger than the wanted
signal, leaves the wanted signal 12.9 dB above the residual, so the filter alone supplies
72.9 dB against ITU-R M.478's 60 dB. The test passes only above 10 dB.

Targets sharing one slice (`targets_sharing_one_slice_recover_their_own_audio`): three FM
carriers in one 100 ksps slice, one on the adjacent raster step, each worker recovers its
own tone with the other tones 56 to 85 dB down.

### Against real IQ

`engine/tests/real_iq.rs` (ignored; needs `RADIO_WIDEBAND_WAV`, the private 6 Msps
recording) cuts a 100 ksps slice from `recordings/baseband_454216998Hz_wide-30s_*.wav`
and compares the production filter with the slice's own spectrum on the 12.5 kHz raster
around the 454.217 MHz emitter. Latest run:

| Channel (MHz) | spectrum power | filtered power | difference |
| --- | --- | --- | --- |
| 454.2045 (neighbor, 30 dB weaker) | -73.5 dBFS | -72.9 dBFS | +0.6 dB |
| 454.2170 (strong) | -43.1 dBFS | -43.1 dBFS | -0.1 dB |
| 454.2295 (neighbor, 32 dB weaker) | -75.0 dBFS | -74.6 dBFS | +0.4 dB |
| 454.2545 | -93.6 dBFS | -76.5 dBFS | +17.1 dB (see below) |

The real band is only about 30 dB above its noise next to the strong emitter, so this
test can show the neighbors are not polluted to within noise, not 60 dB. The synthetic
tests carry the 60 to 70 dB claim. The 454.2545 MHz row is off-raster: the narrow
emitter at 454.2607 MHz (6.2 kHz away) sits in the filter's transition band, which the
brick-wall spectrum integration ignores. That is expected for an emitter on a 6.25 kHz
offset grid, not leakage from the strong signal, and is why the test only asserts for
channels beside a strong neighbor.

## Per-target RF activity (`SliceMeter`)

A 100 ksps slice can hold several logical targets. Slice (physical) activity comes from
the producer; per-target activity is measured in Rust from the slice's own spectrum,
independent of codec admission. 1024-point Hann-windowed FFTs are averaged over the
packet (Parseval-normalized so a tone of amplitude A reads 20 log10(A/full scale) dBFS,
tested), the in-band power of each target (authorized bandwidth) is compared with the
slice noise, and the target opens or closes with hysteresis.

Noise reference: the median bin power of the same slice, scaled by m/(m - 1/3) for the
bias of the median of a Gamma(m) average (m windows). The median is a standard robust
noise-floor estimator (the survey tool in `src/radio_tools/survey.py` uses the same idea
with the ln 2 factor). It fails if more than half the slice is occupied, which a
100 ksps slice of 12.5 kHz channels cannot be.

Thresholds: open at 8 dB above noise, close at 5 dB. These are choices, not standard
values. ITU-R SM.1880 (spectrum occupancy measurement) defines occupancy by a preset
threshold and says to derive it from the monitoring system's noise floor, but gives no
number; a GSM-band occupancy study (<https://uilspace.unilorin.edu.ng/handle/20.500.12484/11164>)
reports 10 dB above noise as the common benchmark and finds about 5 dB workable. With
about 115 bins x 9 windows averaged the in-band noise fluctuates by roughly 0.15 dB, so
false alarms from noise alone are negligible (300 noise-only packets, none). The
hysteresis is tested by walking a carrier through 6.5, 12, 6.5 and 2 dB above noise.

Limits, from review: the noise reference is the median bin, so it fails when more than half
of the 100 ksps slice is occupied (tested to hold with 5 of 7 flat 9 kHz emissions, and to
collapse at 6 or 7); a lower quantile would extend that. Packets shorter than 1024 samples
are accumulated across calls (the ARM gate emits at most 256), and the partial window is
dropped on START, discontinuity or overflow. The 8/5 dB thresholds are choices, not
standard values. Only 12.5 and 6.25 kHz raster channels are specified; an emitter between
rasters (for example 6.2 kHz off a 12.5 kHz channel center) falls in the transition band.
