"""Bit-accurate model of the Pluto FPGA coarse channelizer.

Structure (harris 2004, ch. 6-7; Crochiere & Rabiner 1983, ch. 7): an M = 512 path
polyphase partition of a 4096-tap prototype low-pass, commutated with hop M/2 = 256
(2x oversampled), followed by an M-point FFT and the alternate-frame phase correction
that a hop of M/2 requires for odd bins. Output bin k is the complex baseband of
RF offset k * fs/M, sampled at 2 fs/M.

Three implementations live here:

* ``pfb_float``: double-precision reference with the same frame alignment as the
  ARM C reference (offload/arm/pfb.c).
* ``pfb_fixed``: integer model of the RTL. The FIR stage is exact integer arithmetic
  with a stated rounding point; the FFT is AMD's bit-accurate xfft C model
  (xfft_cmodel.py) so the RTL/xsim output can be compared sample-for-sample.
* ``fft_int_reference``: a numpy integer FFT used only when the C model is not
  installed (tolerance comparisons, not bit-exact).
"""

from __future__ import annotations

from dataclasses import dataclass, field

import numpy as np
from scipy import signal


@dataclass(frozen=True)
class Config:
    fs: float = 25.6e6          # input complex sample rate
    n: int = 512                # FFT size = polyphase paths M
    taps: int = 8               # taps per polyphase branch; prototype length n*taps
    cutoff_hz: float = 50e3     # prototype -6 dB edge = bin spacing (adjacent bins cross at -6 dB)
    kaiser_beta: float = 9.4    # Kaiser (1974): beta = 0.1102 (A - 8.7) for A ~ 94 dB
    adc_bits: int = 12          # AD9363 ADC; input is clipped to this range before the FIR
    coef_bits: int = 18         # DSP48E1 B port
    fir_shift: int = 12         # rounding right shift of the 8-tap integer sum
    fft_in_bits: int = 18       # xfft input width (FIR output saturates here)
    twiddle_bits: int = 18
    fft_arch: str = "pipelined"
    fft_rounding: bool = True   # convergent rounding inside xfft (C_HAS_ROUNDING)
    out_shift: int = 9          # default output gain: |y| >> 9 -> 16-bit, x16 ADC units
    psd_shift: int = 8          # power path: y >> 8 saturated to 18 bits before squaring

    @property
    def hop(self) -> int:
        return self.n // 2

    @property
    def length(self) -> int:
        return self.n * self.taps

    @property
    def bin_hz(self) -> float:
        return self.fs / self.n

    @property
    def out_rate(self) -> float:
        return self.fs / self.hop

    @property
    def fft_out_bits(self) -> int:
        return self.fft_in_bits + int(np.log2(self.n)) + 1


DEFAULT = Config()


def prototype_float(cfg: Config = DEFAULT) -> np.ndarray:
    """Kaiser-windowed sinc prototype, unit DC gain (sum h = 1)."""
    h = signal.firwin(cfg.length, cfg.cutoff_hz, window=("kaiser", cfg.kaiser_beta), fs=cfg.fs)
    return h / h.sum()


def prototype_fixed(cfg: Config = DEFAULT) -> np.ndarray:
    """Coefficients quantized to coef_bits with the peak at full scale (2^(b-1) - 1)."""
    h = prototype_float(cfg)
    q = np.rint(h / np.abs(h).max() * ((1 << (cfg.coef_bits - 1)) - 1)).astype(np.int64)
    return q


def c_reference_prototype(n: int, taps: int) -> np.ndarray:
    """The ARM prototype's filter (offload/arm/pfb.c), for structural cross-checks."""
    length = n * taps
    j = np.arange(length)
    x = j - (length - 1) * 0.5
    with np.errstate(invalid="ignore", divide="ignore"):
        v = np.where(x == 0, 0.9 / n, np.sin(np.pi * 0.9 * x / n) / (np.pi * x))
    w = 0.42 - 0.5 * np.cos(2 * np.pi * j / (length - 1)) + 0.08 * np.cos(4 * np.pi * j / (length - 1))
    h = v * w
    return h / h.sum()


def frames_available(samples: int, cfg: Config = DEFAULT) -> int:
    return 0 if samples < cfg.length else (samples - cfg.length) // cfg.hop + 1


def _polyphase_inputs(x: np.ndarray, cfg: Config) -> np.ndarray:
    """Windows of length n*taps starting at f*hop, shape (frames, taps, n)."""
    frames = frames_available(len(x), cfg)
    idx = (np.arange(frames)[:, None] * cfg.hop + np.arange(cfg.length)[None, :])
    return x[idx].reshape(frames, cfg.taps, cfg.n)


def phase_correct(y: np.ndarray, cfg: Config = DEFAULT) -> np.ndarray:
    """Negate odd bins on odd frames: removes exp(-j 2 pi k f hop / n) = (-1)^(k f)."""
    y = y.copy()
    y[1::2, 1::2] = -y[1::2, 1::2]
    return y


def pfb_float(x: np.ndarray, h: np.ndarray | None = None, cfg: Config = DEFAULT) -> np.ndarray:
    """Complex frames (frames, n), bin k = k*fs/n, rate fs/hop. Matches pfb.c alignment."""
    h = prototype_float(cfg) if h is None else h
    win = _polyphase_inputs(np.asarray(x, dtype=np.complex128), cfg)
    u = np.einsum("ftj,tj->fj", win, h.reshape(cfg.taps, cfg.n))
    return phase_correct(np.fft.fft(u, axis=1), cfg)


# ---------------------------------------------------------------------------
# Fixed point


def clip_adc(x: np.ndarray, cfg: Config = DEFAULT) -> tuple[np.ndarray, np.ndarray]:
    """Saturate I and Q to the ADC's signed range; returns (re, im) int64."""
    lo, hi = -(1 << (cfg.adc_bits - 1)), (1 << (cfg.adc_bits - 1)) - 1
    re = np.clip(np.rint(np.real(x)).astype(np.int64), lo, hi)
    im = np.clip(np.rint(np.imag(x)).astype(np.int64), lo, hi)
    return re, im


def round_shift(v: np.ndarray, s: int) -> np.ndarray:
    """Round half up then arithmetic shift right (adds 2^(s-1) before >> s)."""
    if s == 0:
        return v
    return (v + (1 << (s - 1))) >> s


def saturate(v: np.ndarray, bits: int) -> np.ndarray:
    return np.clip(v, -(1 << (bits - 1)), (1 << (bits - 1)) - 1)


def fir_fixed(re: np.ndarray, im: np.ndarray, hq: np.ndarray, cfg: Config = DEFAULT
              ) -> tuple[np.ndarray, np.ndarray]:
    """Polyphase branch sums, rounded and saturated to fft_in_bits. Shape (frames, n)."""
    wr = _polyphase_inputs(re, cfg)
    wi = _polyphase_inputs(im, cfg)
    hp = hq.reshape(cfg.taps, cfg.n)
    ur = np.einsum("ftj,tj->fj", wr, hp)
    ui = np.einsum("ftj,tj->fj", wi, hp)
    ur = saturate(round_shift(ur, cfg.fir_shift), cfg.fft_in_bits)
    ui = saturate(round_shift(ui, cfg.fir_shift), cfg.fft_in_bits)
    return ur, ui


def fft_int_reference(ur: np.ndarray, ui: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    y = np.fft.fft(ur + 1j * ui, axis=-1)
    return np.rint(y.real).astype(np.int64), np.rint(y.imag).astype(np.int64)


@dataclass
class FixedResult:
    u_re: np.ndarray            # FIR output (FFT input), per frame natural order
    u_im: np.ndarray
    y_re: np.ndarray            # FFT output after phase correction, fft_out_bits
    y_im: np.ndarray
    out_re: np.ndarray          # 16-bit slice samples at the configured gain
    out_im: np.ndarray
    out_sat: np.ndarray         # per-sample saturation flag
    power: np.ndarray           # per-bin power word fed to PSD/activity
    bit_exact_fft: bool = field(default=False)


def output_quantize(y_re: np.ndarray, y_im: np.ndarray, shift: int
                    ) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    r = round_shift(y_re, shift)
    i = round_shift(y_im, shift)
    sr, si = saturate(r, 16), saturate(i, 16)
    return sr, si, (sr != r) | (si != i)


def power_word(y_re: np.ndarray, y_im: np.ndarray, cfg: Config = DEFAULT) -> np.ndarray:
    a = saturate(round_shift(y_re, cfg.psd_shift), 18)
    b = saturate(round_shift(y_im, cfg.psd_shift), 18)
    return a * a + b * b


def pfb_fixed(x: np.ndarray, cfg: Config = DEFAULT, out_shift: int | None = None,
              require_bit_exact: bool = False) -> FixedResult:
    from . import xfft_cmodel  # noqa: PLC0415 - optional native dependency

    re, im = clip_adc(x, cfg)
    hq = prototype_fixed(cfg)
    ur, ui = fir_fixed(re, im, hq, cfg)
    exact = xfft_cmodel.available()
    if exact:
        fft = xfft_cmodel.XFFT(int(np.log2(cfg.n)), cfg.fft_in_bits, cfg.twiddle_bits,
                               cfg.fft_arch, cfg.fft_rounding)
        yr = np.empty_like(ur)
        yi = np.empty_like(ui)
        for f in range(ur.shape[0]):
            yr[f], yi[f] = fft(ur[f], ui[f])
    elif require_bit_exact:
        raise RuntimeError("xfft C model not loadable; set LD_LIBRARY_PATH (see README)")
    else:
        yr, yi = fft_int_reference(ur, ui)
    yr = phase_correct(yr, cfg)
    yi = phase_correct(yi, cfg)
    lim = 1 << (cfg.fft_out_bits - 1)
    assert np.all(np.abs(yr) < lim) and np.all(np.abs(yi) < lim)
    o_r, o_i, sat = output_quantize(yr, yi, cfg.out_shift if out_shift is None else out_shift)
    return FixedResult(ur, ui, yr, yi, o_r, o_i, sat, power_word(yr, yi, cfg), exact)


def fixed_gain(cfg: Config = DEFAULT) -> float:
    """Complex gain from ADC units to FFT-output units for a bin-centred tone."""
    return prototype_fixed(cfg).sum() / float(1 << cfg.fir_shift)
