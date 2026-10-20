# FPGA coarse channelizer (paused)

Status: **paused at the model stage, before any RTL**. The spec is being
rewritten. The design review gates this work on a measured trimmed-TX build and a
measured 512-point FFT fit. Nothing here has been synthesized into the Pluto
design, and nothing has been flashed or programmed.

## What exists

- `model/channelizer.py`: a numpy model of a 512-path, 2x-oversampled polyphase
  filter bank (hop 256). At 25.6 Msps that gives 50 kHz bins and 100 ksps complex
  output per bin. It has the same frame alignment and odd-bin phase correction as
  `offload/arm/pfb.c`. There's a float reference (`pfb_float`) and an
  integer model of the planned RTL (`pfb_fixed`).
- `model/xfft_cmodel.py`: a ctypes binding to AMD's bit-accurate xfft v9.1 C model,
  which ships with Vivado. With it, `pfb_fixed` predicts xfft IP output bit for bit.
  The model's libraries have no SONAME, so set the loader path first:

  ```sh
  export LD_LIBRARY_PATH=$HOME/amd/2026.1/lnx64/tools/fft_v9_1:$HOME/amd/2026.1/lnx64/tools/fpo_v7_1
  PYTHONPATH=. uv run python -c 'from fpga.model.channelizer import *'
  ```

## Model results so far

The prototype is a 4096-tap (8 taps/branch) Kaiser-windowed sinc with beta 9.4 and
a -6 dB edge at 50 kHz, so adjacent bins cross at -6 dB. Because the output is 2x
oversampled (100 ksps for 50 kHz spacing), the transition band can run into the
adjacent bin without aliasing into the protected passband.

| Prototype | Ripple over +/-31.25 kHz | Worst response >= 68.75 kHz (aliases into +/-31.25 kHz) | @25 kHz | @50 kHz |
| --- | ---: | ---: | ---: | ---: |
| Kaiser fc=50 kHz, beta 9.4 (proposed) | 0.0004 dB | -94.2 dB | 0.00 dB | -6.02 dB |
| ARM C reference filter at 512 bins | 24 dB (-24 dB at 31.25 kHz) | -88.7 dB | -9.46 dB | -84 dB |

The protected band of +/-31.25 kHz = 25 kHz worst-case offset from a bin centre
plus half of a 12.5 kHz channel. Any channel on the 6.25 kHz raster is therefore
inside some bin's flat passband. The C reference filter can't do this at bin edges.

Fixed point: 12-bit clipped input, 18-bit coefficients, an 8-tap integer sum rounded
by >> 12 into 18 bits (bounded at 0.555 of full scale for any 12-bit input), and an
unscaled xfft with 18-bit input and twiddles and convergent rounding (28-bit output,
worst case bounded at 2^25.2). The gain from ADC units to FFT output is exactly 2^13.
Measured against float with a bin-centred tone plus white noise, SNR loss is:

| Input noise per sample (ADC LSB) | FIR + FFT core | 16-bit output at >> 9 (x16 ADC units) |
| ---: | ---: | ---: |
| 0 (ADC quantization only) | 0.017 dB | 2.5 dB |
| 1 | 0.002 dB | 0.62 dB |
| 3 | 0.0003 dB | 0.08 dB |

The real 6 Msps captures have a noise floor of about 90 LSB rms (complex,
wideband), so the 16-bit output quantization is negligible in practice.

Not done yet: alias and straddle sweeps on synthetic DMR/FM, validation against the
real captures (`recordings/`, 20 Msps snapshot resampled to 25.6 Msps), the
C-binary cross-check, and the activity-detector statistics.

## Resource findings (the central risk)

Stock ADI `projects/pluto` (hdl_2026_r1, Vivado 2026.1, routed): **12,673 LUT
(72%), 22,125 FF, 2 BRAM36, 72/80 DSP (90%), slices 99.7%, WNS +0.936 ns, WHS
+0.013 ns**. All timing constraints were met.

Main consumers: the axi_ad9361 TX path (5,159 LUT and 16 DSP; four DDS at about
1,010 LUT and 2 DSP each), the axi_ad9361 RX path (1,265 LUT, 12 DSP),
rx_fir_decimator (22 DSP) and tx_fir_interpolator (22 DSP).

Synthesized OOC, the xfft v9.1 512-point pipelined streaming core (18-bit
data and twiddles, unscaled, convergent rounding, natural order) is **2,969 LUT,
5,554 FF, 4 BRAM36, 24 DSP**. The polyphase FIR at one output per 100 MHz clock
needs 16 DSP. Without trimming the stock design, the channelizer doesn't fit.
Setting DAC_DDS_DISABLE and removing tx_fir_interpolator would free about 4,600 LUT
and 30 DSP. That's not measured yet; it's the gate the design review asks for.
