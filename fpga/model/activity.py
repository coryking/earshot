"""Bit-accurate model of the per-bin power spectrum and activity detector.

Estimator (all per coarse bin k, once per block of K = 2^block_log2 frames):

* Welch-style averaged periodogram (Welch 1967): P[k] = sum over the block of the
  power word |y_k|^2 (the PFB prototype is the data window; frames overlap 50%).
* Log conversion: L = log2(P) in Q8 (1 LSB = 3.0103/256 dB of power), using a
  leading-one detector and a 256-entry mantissa table.
* Noise floor by minimum statistics (Martin 2001): the minimum of L over a
  sliding window of V sub-windows of U blocks each. A minimum of noisy estimates is
  biased low; ``bias_q8`` adds the expected-minimum correction (computed below).
* CFAR-style decision: active when L > N + on_q8; once active, stays active while
  L > N + off_q8, and for ``hang`` blocks after it falls below (hysteresis + hang).
  With an accurate floor, the false-alarm probability per bin per block follows
  from the chi-square law of the averaged periodogram (see false_alarm()).

The long-average PSD (for the waterfall) is a second linear accumulator over
2^psd_log2 frames, also reported as log2 Q8.
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np
from scipy import special, stats

LOG_FRAC = 8
DB_PER_LSB = 10 * np.log10(2) / (1 << LOG_FRAC)
MANTISSA_TABLE = np.minimum(
    np.rint((1 << LOG_FRAC) * np.log2(1 + np.arange(256) / 256.0)), 255).astype(np.int64)


def log2_q8(v: np.ndarray) -> np.ndarray:
    """Q8 log2 of non-negative integers (< 2^48). log2_q8(0) = 0."""
    v = np.asarray(v, dtype=np.int64)
    out = np.zeros(v.shape, dtype=np.int64)
    nz = v > 0
    e = np.zeros(v.shape, dtype=np.int64)
    e[nz] = np.floor(np.log2(v[nz].astype(np.float64))).astype(np.int64)
    # exact integer leading-one position (float log2 can round up just below 2^e)
    while True:
        hi = nz & ((v >> e) == 0)
        lo = nz & ((v >> e) > 1)
        if not hi.any() and not lo.any():
            break
        e[hi] -= 1
        e[lo] += 1
    mant = np.where(e >= 8, (v >> np.maximum(e - 8, 0)), v << np.maximum(8 - e, 0)) & 0xFF
    out[nz] = (e[nz] << LOG_FRAC) + MANTISSA_TABLE[mant[nz]]
    return out


@dataclass(frozen=True)
class DetectorConfig:
    block_log2: int = 8        # K = 256 frames = 2.56 ms at 100 kHz frame rate
    sub_blocks: int = 128      # U: blocks per minimum sub-window (0.33 s)
    sub_windows: int = 4       # V: retained sub-window minima -> window U*V*K frames (1.3 s)
    bias_q8: int = 0           # expected-minimum bias correction, Q8 log2 units
    on_q8: int = 0             # open threshold above floor, Q8 log2 units
    off_q8: int = 0            # close threshold above floor
    hang: int = 39             # blocks kept open after falling below off (100 ms)
    psd_log2: int = 13         # long PSD: 8192 frames = 81.9 ms

    @property
    def frames_per_block(self) -> int:
        return 1 << self.block_log2


def db_to_q8(db: float) -> int:
    return int(round(db / DB_PER_LSB))


@dataclass
class DetectorState:
    acc: np.ndarray
    frames: int
    blocks_in_sub: int
    sub_min: np.ndarray
    mins: np.ndarray           # (V, bins), newest first
    active: np.ndarray
    hang: np.ndarray
    psd_acc: np.ndarray
    psd_frames: int


FLOOR_INIT = (1 << 16) - 1     # an empty minimum (16-bit all ones)


def initial_state(bins: int, cfg: DetectorConfig) -> DetectorState:
    z = np.zeros(bins, dtype=np.int64)
    return DetectorState(z.copy(), 0, 0, np.full(bins, FLOOR_INIT, np.int64),
                         np.full((cfg.sub_windows, bins), FLOOR_INIT, np.int64),
                         np.zeros(bins, bool), z.copy(), z.copy(), 0)


@dataclass
class BlockOutput:
    level: np.ndarray          # L, Q8 log2 of the block sum
    floor: np.ndarray          # N, Q8 (min statistics + bias), saturates at 0xFFFF
    active: np.ndarray


def step(state: DetectorState, power: np.ndarray, cfg: DetectorConfig
         ) -> tuple[BlockOutput | None, np.ndarray | None]:
    """Feed one frame's power words. Returns (block result, long PSD) when complete."""
    state.acc += power
    state.psd_acc += power
    state.frames += 1
    state.psd_frames += 1
    block = psd = None
    if state.psd_frames == (1 << cfg.psd_log2):
        psd = log2_q8(state.psd_acc)
        state.psd_acc[:] = 0
        state.psd_frames = 0
    if state.frames == cfg.frames_per_block:
        level = log2_q8(state.acc)
        state.acc[:] = 0
        state.frames = 0
        state.sub_min = np.minimum(state.sub_min, level)
        floor_raw = np.minimum(state.sub_min, state.mins.min(axis=0))
        floor = np.minimum(floor_raw + cfg.bias_q8, FLOOR_INIT)
        above_on = level > floor + cfg.on_q8
        above_off = level > floor + cfg.off_q8
        keep = state.active & above_off
        opened = ~state.active & above_on
        hold = state.active & ~above_off & (state.hang > 0)
        state.hang = np.where(opened | keep, cfg.hang, np.where(hold, state.hang - 1, 0))
        state.active = opened | keep | hold
        block = BlockOutput(level, floor, state.active.copy())
        state.blocks_in_sub += 1
        if state.blocks_in_sub == cfg.sub_blocks:
            state.mins[1:] = state.mins[:-1]
            state.mins[0] = state.sub_min
            state.sub_min = np.full_like(state.sub_min, FLOOR_INIT)
            state.blocks_in_sub = 0
    return block, psd


# ---------------------------------------------------------------------------
# Statistics


def welch_dof(h: np.ndarray, hop: int, k: int) -> float:
    """Equivalent chi-square degrees of freedom of a K-frame average of |X|^2.

    Welch (1967) / Percival & Walden (1993, eq. 292b): for overlapping windowed
    segments of complex Gaussian noise, nu = 2K / (1 + 2 sum_{m=1}^{K-1} (1 - m/K) rho(m)),
    with rho(m) = |sum_n h[n] h[n + m*hop]|^2 / (sum h^2)^2.
    """
    e = np.sum(h * h)
    rho = []
    m = 1
    while m * hop < len(h) and m < k:
        rho.append((np.sum(h[:-m * hop] * h[m * hop:]) / e) ** 2)
        m += 1
    s = sum((1 - (i + 1) / k) * r for i, r in enumerate(rho))
    return 2 * k / (1 + 2 * s)


def false_alarm(threshold_db: float, nu: float) -> float:
    """P(averaged noise power > threshold x true mean) for chi-square nu dof."""
    t = 10 ** (threshold_db / 10)
    return float(special.gammaincc(nu / 2, nu * t / 2))


def threshold_for_pfa(pfa: float, nu: float) -> float:
    """Threshold in dB above the true noise mean for a given per-test Pfa."""
    t = stats.chi2.isf(pfa, nu) / nu
    return float(10 * np.log10(t))


def detection(snr_db: float, threshold_db: float, nu: float) -> float:
    """Pd for a constant-envelope signal whose in-bin power is snr_db above noise.

    The averaged power is a scaled noncentral chi-square with nu dof and
    noncentrality lambda = nu * SNR (Kay 1998, vol. II, ch. 2 / Urkowitz 1967).
    """
    snr = 10 ** (snr_db / 10)
    t = 10 ** (threshold_db / 10)
    return float(stats.ncx2.sf(nu * t, nu, nu * snr))


def expected_min_bias_db(nu: float, draws: int, trials: int = 20000, seed: int = 1) -> float:
    """E[10 log10(min of `draws` chi2_nu/nu)]: how far below the mean a minimum lands."""
    rng = np.random.default_rng(seed)
    x = rng.chisquare(nu, size=(trials, draws)) / nu
    return float(np.mean(10 * np.log10(x.min(axis=1))))
