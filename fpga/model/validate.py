"""Quantitative validation of the channelizer model.

Run with the xfft C model on the loader path (see fpga/README.md):

    PYTHONPATH=. uv run python -m fpga.model.validate all

Each experiment writes fpga/model/results/<name>.json (metrics only, no sample data).
Private recordings are read from the main checkout's recordings/ and output/ dirs.
"""

from __future__ import annotations

import dataclasses
import json
import time
from pathlib import Path

import numpy as np
import typer
from scipy import signal

from . import activity as act
from .channelizer import (
    DEFAULT,
    Config,
    c_reference_prototype,
    fixed_gain,
    pfb_fixed,
    pfb_float,
    prototype_fixed,
    prototype_float,
)

app = typer.Typer(add_completion=False, no_args_is_help=True)
RESULTS = Path(__file__).with_name("results")
DATA_ROOT = Path("/Users/coryking/projects/amateur-radio")
FIXTURE = DATA_ROOT / "output/native-handoff/dmr-slot2-25ksps-ci16le.raw"
SNAPSHOT = DATA_ROOT / "recordings/baseband_454216998Hz_wide-snapshot_20261019T215323Z.wav"
WIDE30 = DATA_ROOT / "recordings/baseband_454216998Hz_wide-30s_20261019T215324Z.wav"
PROTECT_HZ = 31_250.0          # 25 kHz worst-case offset + half of a 12.5 kHz channel
ALIAS_EDGE_HZ = DEFAULT.out_rate - PROTECT_HZ   # 68.75 kHz: folds onto the protected band


def save(name: str, data: dict) -> None:
    RESULTS.mkdir(exist_ok=True)
    (RESULTS / f"{name}.json").write_text(json.dumps(data, indent=2, default=float) + "\n")
    print(json.dumps(data, indent=2, default=float))


def response_db(h: np.ndarray, f: np.ndarray, fs: float) -> np.ndarray:
    _, r = signal.freqz(h, worN=np.asarray(f, float), fs=fs)
    return 20 * np.log10(np.abs(r) / abs(h.sum()) + 1e-300)


def filter_metrics(h: np.ndarray, cfg: Config) -> dict:
    fp = np.linspace(0, PROTECT_HZ, 400)
    fst = np.linspace(ALIAS_EDGE_HZ, cfg.fs / 2, 200_000)
    rp = response_db(h, fp, cfg.fs)
    rs = response_db(h, fst, cfg.fs)
    pts = {f"at_{int(f / 1e3)}k_db": float(response_db(h, [f], cfg.fs)[0])
           for f in (25e3, 31.25e3, 50e3, 68.75e3, 100e3, 150e3)}
    return {"passband_ripple_db": float(rp.max() - rp.min()), "passband_min_db": float(rp.min()),
            "alias_rejection_db": float(-rs.max()), **pts}


# ---------------------------------------------------------------------------


@app.command("filter")
def filter_cmd() -> dict:
    """Prototype specs: derivation, float vs 18-bit quantized, vs the ARM C filter."""
    cfg = DEFAULT
    df = ALIAS_EDGE_HZ - PROTECT_HZ
    a_db = 90.0
    out = {
        "requirements": {
            "bin_spacing_hz": cfg.bin_hz, "output_rate_hz": cfg.out_rate,
            "oversampling": cfg.out_rate / cfg.bin_hz,
            "protected_passband_hz": PROTECT_HZ, "alias_stopband_edge_hz": ALIAS_EDGE_HZ,
            "transition_hz": df,
            "harris_length_estimate_for_90db": cfg.fs / df * a_db / 22,
            "kaiser_order_estimate_for_90db": (a_db - 7.95) / (2.285 * 2 * np.pi * df / cfg.fs),
            "chosen_length": cfg.length, "taps_per_branch": cfg.taps,
            "kaiser_beta_for_94db": 0.1102 * (94 - 8.7),
        },
        "kaiser_float": filter_metrics(prototype_float(cfg), cfg),
        "kaiser_18bit": filter_metrics(prototype_fixed(cfg).astype(float), cfg),
        "arm_c_reference_512": filter_metrics(c_reference_prototype(cfg.n, cfg.taps), cfg),
    }
    save("filter", out)
    return out


def _tone(f: float, n: int, amp: float, cfg: Config, phase: float = 0.0) -> np.ndarray:
    t = np.arange(n)
    return amp * np.exp(1j * (2 * np.pi * f * t / cfg.fs + phase))


@app.command("alias")
def alias_cmd() -> dict:
    """Fixed-point tone tests: in-bin gain flatness and power aliased into bin 0."""
    cfg = DEFAULT
    frames = 64
    n = cfg.length + cfg.hop * (frames - 1)
    amp = 2000.0
    g = fixed_gain(cfg)
    ref_p = (amp * g) ** 2
    rows = []
    for off in [0, 6250, 12500, 18750, 25000, 31250, 40000, 50000, 60000, 68750, 75000,
                100000, 150000, 250000, 1_000_000, 5_000_000, 12_000_000]:
        r = pfb_fixed(_tone(off, n, amp, cfg) + 0.0, cfg, require_bit_exact=True)
        y0 = (r.y_re[:, 0] + 1j * r.y_im[:, 0])[8:]
        p = np.mean(np.abs(y0) ** 2)
        rows.append({"offset_hz": off, "bin0_rel_db": float(10 * np.log10(p / ref_p + 1e-30)),
                     "aliases_into_protected_band": bool(off >= ALIAS_EDGE_HZ)})
    # quantization floor of the fixed-point path with zero input
    z = pfb_fixed(np.zeros(n, complex), cfg, require_bit_exact=True)
    worst = max(r["bin0_rel_db"] for r in rows if r["aliases_into_protected_band"])
    out = {"tone_amplitude_adc": amp, "rows": rows, "worst_alias_db": worst,
           "zero_input_max_abs_y": int(np.abs(z.y_re).max())}
    save("alias", out)
    return out


# ---------------------------------------------------------------------------
# Host-side final channel filter (mirrors engine/src/dsp.rs Resampler: Hann-windowed
# sinc, cutoff 0.98 * 12.5 kHz / input rate, radius ceil(32 / cutoff)).


def host_filter(x100: np.ndarray, rate: float = 100e3, out_rate: float = 25e3) -> np.ndarray:
    cutoff = min(12500.0, out_rate) / rate * 0.98
    radius = int(np.ceil(32 / cutoff))
    k = np.arange(-radius, radius + 1)
    h = np.sinc(k * cutoff) * (0.5 + 0.5 * np.cos(np.pi * k / radius)) * cutoff
    h /= h.sum()
    y = signal.fftconvolve(x100, h, mode="full")[radius:radius + len(x100)]
    return y[:: int(rate / out_rate)]


def load_ci16(path: Path, count: int | None = None, offset: int = 0, wav: bool = False
              ) -> np.ndarray:
    skip = 44 if wav else 0
    raw = np.memmap(path, dtype="<i2", mode="r", offset=skip + 4 * offset,
                    shape=(2 * count,) if count else None)
    raw = np.asarray(raw, dtype=np.float64)
    return raw[0::2] + 1j * raw[1::2]


def dmr_fixture(seconds: float, start: float = 0.0) -> np.ndarray:
    d = load_ci16(FIXTURE)
    a = int(start * 25e3)
    return d[a:a + int(seconds * 25e3)]


def upsample_to_fs(d25: np.ndarray, cfg: Config) -> np.ndarray:
    """25 ksps -> 25.6 Msps (x1024) in two polyphase stages."""
    x = signal.resample_poly(d25, 32, 1)            # 800 ksps
    return signal.resample_poly(x, 32, 1)           # 25.6 Msps


def align_snr(test: np.ndarray, ref: np.ndarray, max_lag: int = 40) -> tuple[float, int]:
    """SNR of test vs ref after best integer lag and least-squares complex gain."""
    best = (-np.inf, 0)
    m = min(len(test), len(ref)) - 2 * max_lag
    for lag in range(-max_lag, max_lag + 1):
        a = test[max_lag + lag:max_lag + lag + m]
        b = ref[max_lag:max_lag + m]
        g = np.vdot(b, a) / np.vdot(b, b)
        e = a - g * b
        snr = 10 * np.log10(np.vdot(g * b, g * b).real / np.vdot(e, e).real)
        if snr > best[0]:
            best = (snr, lag)
    return best


@app.command("straddle")
def straddle_cmd(seconds: float = 0.25) -> dict:
    """Real DMR (fixture) at offsets across a bin, near/far interferers, realistic levels.

    Levels: the strong signal sits at -10 dBFS of the 12-bit ADC (650 LSB rms); the
    desired DMR is near_far dB below it; white noise of 1.5 LSB rms per component
    stands in for the receiver noise floor. Each case reports the recovered SNR
    (vs. the clean fixture after the host filter) for: an ideal float channelizer on
    the same quantized input, the fixed-point FPGA path (32-bit internal and the
    16-bit slice), and the ARM C prototype filter. "fpga_loss_db" is float - fixed.
    """
    cfg = DEFAULT
    d25 = dmr_fixture(seconds, 0.5)
    dwide = upsample_to_fs(d25, cfg)
    dwide /= np.sqrt(np.mean(np.abs(dwide) ** 2))
    i25 = dmr_fixture(seconds, 3.0)
    iwide = upsample_to_fs(i25, cfg)
    iwide /= np.sqrt(np.mean(np.abs(iwide) ** 2))
    n = len(dwide)
    t = np.arange(n)
    rng = np.random.default_rng(3)
    noise = 1.5 * (rng.standard_normal(n) + 1j * rng.standard_normal(n))
    strong = 650.0
    ref25 = host_filter(signal.resample_poly(d25, 4, 1))
    base_bin = 40
    scenarios = [("alone_strong", None, 0.0), ("alone_weak", None, 40.0)]
    scenarios += [(f"interferer_+{d / 1e3:g}kHz_{p:g}dB", d, p)
                  for d, p in [(12.5e3, 40), (25e3, 40), (37.5e3, 50), (50e3, 50),
                               (75e3, 60), (100e3, 60), (150e3, 60)]]
    cases = []
    for off in [0.0, 6.25e3, 12.5e3, 18.75e3, 25e3]:
        f_d = base_bin * cfg.bin_hz + off
        k = int(np.floor(f_d / cfg.bin_hz + 0.5))
        resid = f_d - k * cfg.bin_hz
        for name, d_int, near_far in scenarios:
            amp = strong * 10 ** (-near_far / 20)
            x = amp * dwide * np.exp(2j * np.pi * f_d * t / cfg.fs) + noise
            if d_int is not None:
                x = x + strong * iwide * np.exp(2j * np.pi * (f_d + d_int) * t / cfg.fs)
            xq = np.rint(x.real) + 1j * np.rint(x.imag)
            row = {"offset_from_bin_center_hz": resid, "scenario": name,
                   "desired_rms_adc": amp, "clipped": int(np.sum(np.abs(xq.real) > 2047)
                                                          + np.sum(np.abs(xq.imag) > 2047))}

            def snr(y):
                y = y * np.exp(-2j * np.pi * resid * np.arange(len(y)) / cfg.out_rate)
                return align_snr(host_filter(y), ref25)[0]

            row["float_snr_db"] = snr(pfb_float(xq, None, cfg)[:, k])
            row["arm_c_filter_snr_db"] = snr(pfb_float(xq, c_reference_prototype(cfg.n, cfg.taps), cfg)[:, k])
            r = pfb_fixed(xq, cfg, require_bit_exact=True)
            row["fixed_snr_db"] = snr((r.y_re[:, k] + 1j * r.y_im[:, k]).astype(complex))
            row["fixed_16bit_snr_db"] = snr((r.out_re[:, k] + 1j * r.out_im[:, k]).astype(complex))
            row["fpga_loss_db"] = row["float_snr_db"] - row["fixed_16bit_snr_db"]
            row["out_saturated"] = int(r.out_sat[:, k].sum())
            cases.append(row)
            print(json.dumps(row, default=float), flush=True)
    out = {"seconds": seconds, "strong_rms_adc": strong, "noise_rms_per_component": 1.5,
           "note": "SNR = recovered desired vs. the clean fixture after the host filter "
                   "(engine/src/dsp.rs equivalent), best lag and complex gain.",
           "cases": cases}
    save("straddle", out)
    return out


# ---------------------------------------------------------------------------


@app.command("real")
def real_cmd(seconds: float = 0.1) -> dict:
    """20 Msps off-air snapshot resampled to 25.6 Msps: fixed vs float, per bin."""
    cfg = DEFAULT
    x20 = load_ci16(SNAPSHOT, int(seconds * 20e6) + 4096, 0, wav=True)
    x = signal.resample_poly(x20, 32, 25)
    xq = np.rint(x.real) + 1j * np.rint(x.imag)
    t0 = time.time()
    r = pfb_fixed(xq, cfg, require_bit_exact=True)
    yf = pfb_float(xq, cfg=cfg)
    elapsed = time.time() - t0
    g = fixed_gain(cfg)
    y = (r.y_re + 1j * r.y_im) / g
    err = np.mean(np.abs(y - yf) ** 2, axis=0)
    pwr = np.mean(np.abs(yf) ** 2, axis=0)
    o = (r.out_re + 1j * r.out_im) / (g / 2 ** cfg.out_shift)
    err16 = np.mean(np.abs(o - yf) ** 2, axis=0)
    inband = np.abs(np.fft.fftfreq(cfg.n, 1 / cfg.fs)) < 9.5e6
    rel = 10 * np.log10(err[inband] / pwr[inband])
    rel16 = 10 * np.log10(err16[inband] / pwr[inband])
    pdb = 10 * np.log10(pwr[inband])
    out = {"seconds": seconds, "frames": int(y.shape[0]), "elapsed_s": elapsed,
           "input_rms_adc": float(np.sqrt(np.mean(np.abs(xq) ** 2))),
           "input_peak_adc": float(np.abs(np.concatenate([xq.real, xq.imag])).max()),
           "bins_evaluated": int(inband.sum()),
           "bin_power_db_min_median_max": [float(pdb.min()), float(np.median(pdb)), float(pdb.max())],
           "core_error_rel_db_worst_bin": float(rel.max()),
           "core_snr_loss_db_worst_bin": float(10 * np.log10(1 + 10 ** (rel.max() / 10))),
           "out16_error_rel_db_worst_bin": float(rel16.max()),
           "out16_snr_loss_db_worst_bin": float(10 * np.log10(1 + 10 ** (rel16.max() / 10))),
           "out16_saturated_samples": int(r.out_sat.sum()),
           "bit_exact_fft": r.bit_exact_fft}
    save("real", out)
    return out


@app.command("real-dmr")
def real_dmr_cmd(seconds: float = 0.5) -> dict:
    """Off-air DMR (454.029 MHz) from the 6 Msps capture, through the 25.6 Msps PFB.

    Reference = direct mix + host filter on the 6 Msps data; test = PFB bin -4
    (-200 kHz, residual +12.2 kHz) + the same host filter.
    """
    cfg = DEFAULT
    off = -187_775.0
    x6 = load_ci16(WIDE30, int(seconds * 6e6), int(0.5 * 6e6), wav=True)
    t6 = np.arange(len(x6))
    base = x6 * np.exp(-2j * np.pi * off * t6 / 6e6)
    ref100 = signal.resample_poly(base, 1, 60)
    ref25 = host_filter(ref100)
    x = signal.resample_poly(x6, 64, 15)
    xq = np.rint(x.real) + 1j * np.rint(x.imag)
    k = int(np.round(off / cfg.bin_hz)) % cfg.n
    resid = off - (k - cfg.n) * cfg.bin_hz
    res = {}
    r = pfb_fixed(xq, cfg, require_bit_exact=True)
    for label, y in [("float", pfb_float(xq, cfg=cfg)[:, k]),
                     ("fixed", (r.y_re[:, k] + 1j * r.y_im[:, k]).astype(complex)),
                     ("fixed_16bit", (r.out_re[:, k] + 1j * r.out_im[:, k]).astype(complex))]:
        y = y * np.exp(-2j * np.pi * resid * np.arange(len(y)) / cfg.out_rate)
        res[label + "_agreement_db"] = align_snr(host_filter(y), ref25)[0]
    out = {"seconds": seconds, "bin": k, "residual_hz": resid,
           "note": "Agreement between PFB+host filter and direct mix+host filter on the "
                   "same off-air samples; limited by the two resampling paths, not noise.",
           **res}
    save("real_dmr", out)
    return out


# ---------------------------------------------------------------------------


def power_words_float(x: np.ndarray, fs: float, cfg: Config) -> np.ndarray:
    """Power words for a PFB with the same 50 kHz bins/100 kHz frames at another fs.

    Same prototype duration (8 x n samples) and Kaiser beta, so the bin response in Hz
    matches the 25.6 Msps design; components are scaled and rounded exactly as the
    FPGA power path does (gain 2^13, >> psd_shift, 18-bit saturate, square, sum).
    """
    n = int(round(fs / cfg.bin_hz))
    sub = dataclasses.replace(cfg, fs=fs, n=n)
    h = signal.firwin(n * cfg.taps, cfg.cutoff_hz, window=("kaiser", cfg.kaiser_beta), fs=fs)
    h /= h.sum()
    y = pfb_float(x, h, sub) * fixed_gain(cfg)
    a = np.clip(np.floor(y.real / 2 ** cfg.psd_shift + 0.5), -2**17, 2**17 - 1)
    b = np.clip(np.floor(y.imag / 2 ** cfg.psd_shift + 0.5), -2**17, 2**17 - 1)
    return (a * a + b * b).astype(np.int64)


@app.command("activity")
def activity_cmd(real_seconds: float = 12.0, mc_blocks: int = 20000) -> dict:
    """Detector statistics: dof, thresholds, Pfa/Pd math, Monte Carlo, off-air run."""
    cfg = DEFAULT
    h = prototype_float(cfg)
    det = act.DetectorConfig()
    k = det.frames_per_block
    nu = act.welch_dof(h, cfg.hop, k)
    window_blocks = det.sub_blocks * det.sub_windows
    # The floor is the min over the current partial sub-window and V full ones:
    # between U*V + 1 and U*(V + 1) blocks. The largest D gives the lowest floor and
    # so the highest false-alarm rate; design for it.
    draws = det.sub_blocks * (det.sub_windows + 1)
    bias_db = act.expected_min_bias_db(nu, draws)
    pfa_on, pfa_off = 1e-8, 1e-2
    on_db = act.threshold_min_floor(pfa_on, nu, draws)
    off_db = act.threshold_min_floor(pfa_off, nu, draws)
    det = dataclasses.replace(det, bias_q8=0, on_q8=act.db_to_q8(on_db),
                              off_q8=act.db_to_q8(off_db))
    # Detection: signal in-bin SNR s above a floor that sits ~bias below the mean.
    pd_rows = [{"in_bin_snr_db": s_db,
                "pd": act.detection(s_db, on_db + bias_db, nu)}
               for s_db in (-6, -5, -4, -3, -2, -1, 0)]
    # Monte Carlo of the bit-exact detector on chi-square blocks, at a looser
    # threshold (Pfa 1e-4) where the predicted rate can be measured.
    rng = np.random.default_rng(7)
    bins = 512
    sigma_word = 2.0e4
    mc = {}
    for label, t_db in [("pfa_1e-4", act.threshold_min_floor(1e-4, nu, draws)),
                        ("operating", on_db)]:
        mdet = dataclasses.replace(det, on_q8=act.db_to_q8(t_db), off_q8=act.db_to_q8(t_db),
                                   hang=0)
        st = act.initial_state(bins, mdet)
        warm = det.sub_blocks * (det.sub_windows + 1)
        opens = 0
        prev = np.zeros(bins, bool)
        for blk_i in range(mc_blocks + warm):
            acc = rng.chisquare(nu, bins) / nu * sigma_word * k
            st.acc[:] = 0
            st.frames = k - 1
            blk, _ = act.step(st, np.rint(acc).astype(np.int64), mdet)
            if blk_i >= warm:
                opens += int((blk.active & ~prev).sum())
            prev = blk.active
        mc[label] = {"threshold_db": t_db,
                     "predicted_pfa_at_max_window": act.pfa_min_floor(t_db, nu, draws),
                     "measured_open_rate": opens / (mc_blocks * bins),
                     "trials": mc_blocks * bins}
    frame_rate = cfg.out_rate
    tests_per_hour = bins * frame_rate / k * 3600
    real = activity_real(real_seconds, det, cfg)
    out = {"block_frames": k, "block_ms": k / cfg.out_rate * 1e3,
           "welch_equivalent_dof": nu, "independent_frame_equivalent": nu / 2,
           "min_window_blocks": window_blocks,
           "min_window_s": window_blocks * k / cfg.out_rate,
           "floor_draws_max": draws,
           "expected_min_bias_db": bias_db,
           "pfa_on": pfa_on, "pfa_off": pfa_off,
           "open_threshold_above_floor_db": on_db, "close_threshold_above_floor_db": off_db,
           "expected_false_opens_per_hour_whole_band": pfa_on * tests_per_hour,
           "on_q8": det.on_q8, "off_q8": det.off_q8, "hang_blocks": det.hang,
           "pd_vs_snr": pd_rows, "monte_carlo": mc, "off_air": real}
    save("activity", out)
    return out


def activity_real(seconds: float, det: act.DetectorConfig, cfg: Config) -> dict:
    """Run the detector on the 6 Msps off-air capture (central +/-2.7 MHz bins)."""
    fs = 6e6
    n = int(round(fs / cfg.bin_hz))
    chunk = int(fs)                         # 1 s
    st = act.initial_state(n, det)
    tail = np.zeros(0, complex)
    levels, floors, actives = [], [], []
    for c in range(int(seconds)):
        x = load_ci16(WIDE30, chunk, c * chunk, wav=True)
        x = np.concatenate([tail, x])
        hop = int(round(fs / cfg.out_rate))
        pw = power_words_float(x, fs, cfg)
        used = pw.shape[0] * hop
        tail = x[used:]
        for f in range(pw.shape[0]):
            blk, _ = act.step(st, pw[f], det)
            if blk is not None:
                levels.append(blk.level)
                floors.append(blk.floor)
                actives.append(blk.active)
    lv = np.array(levels)
    fl = np.array(floors)
    ac = np.array(actives)
    keep = np.abs(np.fft.fftfreq(n, 1 / fs)) < 2.7e6
    settled = det.sub_blocks * det.sub_windows
    occ = ac[settled:, keep].mean(axis=0)
    freqs = np.fft.fftfreq(n, 1 / fs)[keep]
    order = np.argsort(-occ)
    busy = [{"offset_hz": float(freqs[i]), "rf_hz": float(454216998 + freqs[i]),
             "occupancy": float(occ[i])} for i in order[:15] if occ[i] > 0.01]
    quiet = occ[occ <= 0.01]
    floor_spread = (fl[settled:, keep].max(axis=0) - fl[settled:, keep].min(axis=0)) * act.DB_PER_LSB
    return {"seconds": seconds, "bins": int(keep.sum()), "blocks": int(lv.shape[0]),
            "busy_bins": busy, "bins_never_or_rarely_active": int(len(quiet)),
            "floor_variation_db_median": float(np.median(floor_spread)),
            "hidden_hold_note": "hang_blocks keeps a bin open across DMR TDMA gaps"}


@app.command("all")
def all_cmd() -> None:
    filter_cmd()
    alias_cmd()
    straddle_cmd()
    real_cmd()
    real_dmr_cmd()
    activity_cmd()


if __name__ == "__main__":
    from .xfft_cmodel import ensure_loader_path

    ensure_loader_path(["-m", "fpga.model.validate", *__import__("sys").argv[1:]])
    app()
