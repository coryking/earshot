"""What does removing axi_ad9361's DC filter and IQ correction cost?

Context: the channelizer taps axi_ad9361's adc_data_i0/q0, which come after ad_dcfilter and
ad_iqcor. The ADI Linux driver leaves both at identity at probe (CHAN_CNTRL_1 = 0: DC
filter off, offset 0; CHAN_CNTRL_2 = 0x40000000 (I) / 0x00004000 (Q): coefficients 1.0 and
0.0 in Q1.14), and nothing in this project changes them, so every capture taken with the stock
bitstream IS the "cores removed" data path. This module measures

  identity-sim  the RTL at those defaults is a bit-exact identity (xsim, fpga/tb)
  verify-rtl    the Python emulations below match the RTL on real capture data
  dc            DC spur of the AD9363 alone: Welch (~1.6 kHz) and at the PFB's resolution
  images        image rejection of strong carriers (coherent mirror-pair estimator) and the
                blind second-order IQ-imbalance estimate with the rejection it implies
  emulate       what the cores could add if enabled and calibrated: ad_dcfilter's IIR at the
                driver's coefficient, and ad_iqcor with Q1.14 coefficients from blind estimates
  summary       compact tables from the sections above

Everything writes sections of fpga/model/results/corrections.json. Run, from the worktree:

    PYTHONPATH=. uv run python -m fpga.model.corrections all

Conventions. Sample values are raw ADC LSBs (12-bit, -2048..2047). "dBFS" is power relative
to a complex tone of amplitude 2048 (a full-scale tone reads 0 dBFS in a Welch bin).

Image estimator. An IQ imbalance makes x = s + eps*conj(s) (eps complex, possibly
frequency dependent), so spectrum bins pair up as X[-m] = eps * conj(X[m]) around a carrier.
Over a window of bins around a carrier at +f,  eps = sum(X[-m] X[m]) / sum(|X[m]|^2).
Unlike reading the level at -f, this separates the coherent image from an unrelated real
signal that happens to sit at -f, and it resolves images far below the noise floor. The
same pairing gives the blind estimate over the whole band: kappa = E[x^2]/E[|x|^2], and an
imbalance with image ratio |eps| gives |kappa| = 2|eps|/(1+|eps|^2).

ad_dcfilter (RTL, the 4-deep DSP loop): Q[n+4] = Q[n] + c * (x[n] - floor(Q[n] >> 17)),
y[n] = x[n] - floor(Q[n] >> 17), i.e. four interleaved one-pole filters, one per n mod 4.
With valid every clock the per-sample coefficient is c / 2**19 (corner ~ c * Fs / (2 pi 2**19));
the driver's formula assumes c / 2**14. The module as shipped also ties the P register's clock
enable low (.CEP(1'd0)), which freezes the accumulator at zero: in xsim (dc_step.sv scratch bench, not in the
repo) the shipped module passed a 1000-LSB step through unchanged with the filter enabled, and the IIR
only decays with CEP = 1. verify-rtl (tb_adc_corr_vec.sv) re-checks this and the Python models bit-for-bit,
but was NOT run for the committed results (box resource pause); treat dcfilter_float as unverified against RTL.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import time
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

import numpy as np
import typer
from scipy import ndimage, signal

from .channelizer import DEFAULT, pfb_float

app = typer.Typer(add_completion=False, no_args_is_help=True)

RESULTS = Path(__file__).with_name("results")
OUT = RESULTS / "corrections.json"
DATA_ROOT = Path("/Users/coryking/projects/amateur-radio")
WIDE = DATA_ROOT / "output/wideband-iq"
STREAM = WIDE / "pluto-20261019T234534Z-lo454216998-4000000sps-stream.sigmf-data"

HDL = Path.home() / ".cache/amateur-radio-fpga/hdl"
SIM_DIR = Path.home() / ".cache/amateur-radio-fpga/optimize/corr_sim"
VIVADO_BIN = Path.home() / "amd/2026.1/Vivado/bin"
TB_DIR = Path(__file__).resolve().parents[1] / "tb"
MEM_LIMIT = ["systemd-run", "--user", "--scope", "-q", "-p", "MemoryMax=12G"]

FULL_SCALE = 2048.0
FS = 25.6e6
BH_ENBW_BINS = 2.0044          # Blackman-Harris equivalent noise bandwidth in bins

# DC analysis
DC_NPER = 32768                # 781 Hz bins, ENBW 1.57 kHz
DC_FLOOR_HZ = (25e3, 150e3)    # annulus for the local floor (median of dB)
DC_ADJ_BINS = (3, 8)           # |k| range of "adjacent" bins (2.3 .. 6.3 kHz)
PFB_SAMPLES = 400_000          # a few hundred thousand samples: ~1547 frames
PFB_ADJ = (2, 10)              # bins +-2..+-10 are the comparison set for bin 0

# image analysis
IMG_NPER = 16384               # 1.56 kHz bins for carrier detection
IMG_MIN_EXCESS_DB = 40.0       # carriers must be this far above the local floor
IMG_PROMINENCE_DB = 10.0       # and this far above their 200 kHz neighbourhood
IMG_F_RANGE = (150e3, 9.6e6)   # skip DC region and the analog filter skirt
COHERENT_T = 25.0              # detection threshold on T = |num|^2 / (D n_eff)

# emulated DC filter coefficients: what the driver writes for a 100 Hz and a 1 kHz request
# (coeff = 102944 * F / Fs, clamped to >= 1), then two that would give a *realised* corner of
# ~100 Hz and ~1 kHz through the RTL's c / 2**19 law
DC_COEFFS = {"driver_100Hz": 1, "driver_1kHz": 4, "realised_100Hz": 13, "realised_1kHz": 128}
SETTLE = 2_000_000             # samples discarded before measuring the emulated DC filter


def jdefault(o):
    if isinstance(o, np.integer):
        return int(o)
    if isinstance(o, np.floating):
        return float(o)
    if isinstance(o, np.bool_):
        return bool(o)
    if isinstance(o, np.ndarray):
        return o.tolist()
    raise TypeError(type(o))


def update_section(name: str, data) -> None:
    RESULTS.mkdir(exist_ok=True)
    cur = json.loads(OUT.read_text()) if OUT.exists() else {}
    cur[name] = data
    OUT.write_text(json.dumps(cur, indent=1, default=jdefault) + "\n")


def read_section(name: str):
    cur = json.loads(OUT.read_text()) if OUT.exists() else {}
    if name not in cur:
        raise typer.BadParameter(f"section {name!r} missing: run that stage first")
    return cur[name]


def db10(p):
    return 10 * np.log10(np.maximum(p, 1e-300))


# ---------------------------------------------------------------------------
# Captures


def list_captures() -> list[dict]:
    paths = sorted(WIDE.glob("pluto-*-25600000sps-buf*.sigmf-data"))
    paths += sorted(WIDE.glob("survey/*/pluto-*-25600000sps-buf*.sigmf-data"))
    caps = []
    for p in paths:
        meta = json.loads(p.with_name(p.name.replace(".sigmf-data", ".sigmf-meta")).read_text())
        g = meta["global"]
        lo = float(g["radio:applied"]["rx_lo"])
        caps.append(dict(
            path=str(p), name=p.name.replace("pluto-", "").replace(".sigmf-data", ""),
            kind="survey" if "survey" in p.parts else "main", lo_hz=lo,
            group=f"LO{lo / 1e6:.3f}MHz", fs=float(g["core:sample_rate"]),
            gain_db=float(g["radio:applied"]["rx_hardwaregain"].split()[0]),
            gain_after_db=float(g["radio:hardwaregain_after"].split()[0]),
            samples=int(g["radio:buffer_samples"])))
    return caps


def load_ci16(path: str, count: int | None = None, offset: int = 0) -> np.ndarray:
    """(n, 2) int16 [I, Q]."""
    a = np.fromfile(path, dtype="<i2", count=-1 if count is None else 2 * count, offset=4 * offset)
    return a.reshape(-1, 2)


def to_complex(a: np.ndarray) -> np.ndarray:
    x = np.empty(len(a), dtype=np.complex64)
    x.real = a[:, 0]
    x.imag = a[:, 1]
    return x


def welch_power(x: np.ndarray, fs: float, nper: int):
    """(f ascending with f[n//2] == 0, power per bin re FULL_SCALE**2; a tone reads A**2)."""
    f, p = signal.welch(x, fs=fs, window="blackmanharris", nperseg=nper, noverlap=nper // 2,
                        return_onesided=False, scaling="spectrum", detrend=False)
    o = np.argsort(f)
    return f[o], p[o] / FULL_SCALE ** 2


# ---------------------------------------------------------------------------
# DC spur


def dc_metrics(x: np.ndarray, fs: float, nper: int = DC_NPER) -> dict:
    mi, mq = float(x.real.mean()), float(x.imag.mean())
    mag = float(np.hypot(mi, mq))
    f, p = welch_power(x, fs, nper)
    db = db10(p)
    c = len(f) // 2
    k = np.arange(len(f)) - c
    ann = (np.abs(f) >= DC_FLOOR_HZ[0]) & (np.abs(f) <= DC_FLOOR_HZ[1])
    adj = (np.abs(k) >= DC_ADJ_BINS[0]) & (np.abs(k) <= DC_ADJ_BINS[1])
    floor = float(np.median(db[ann]))
    adj_db = float(db10(p[adj].mean()))
    return dict(
        mean_i=mi, mean_q=mq, mean_abs=mag, mean_dbfs=float(20 * np.log10(max(mag, 1e-9) / FULL_SCALE)),
        welch_dc_dbfs=float(db[c]), welch_floor_dbfs=floor, dc_over_floor_db=float(db[c] - floor),
        adjacent_dbfs=adj_db, dc_over_adjacent_db=float(db[c] - adj_db),
        bin_hz=float(f[1] - f[0]), rbw_hz=float(BH_ENBW_BINS * (f[1] - f[0])))


def pfb_dc_metrics(x: np.ndarray, n_samples: int = PFB_SAMPLES, offset: int = 0) -> dict:
    """Bin 0 against bins +-2..+-10 of the default 512-bin channelizer (pfb_float)."""
    cfg = DEFAULT
    y = pfb_float(x[offset:offset + n_samples], cfg=cfg)
    pw = (np.abs(y) ** 2).mean(axis=0) / FULL_SCALE ** 2
    nb = cfg.n
    adj = [k for k in range(-PFB_ADJ[1], PFB_ADJ[1] + 1) if PFB_ADJ[0] <= abs(k) <= PFB_ADJ[1]]
    adj_p = pw[[k % nb for k in adj]]
    return dict(
        frames=int(y.shape[0]), bin0_dbfs=float(db10(pw[0])),
        bin_p1_dbfs=float(db10(pw[1])), bin_m1_dbfs=float(db10(pw[-1 % nb])),
        adj_mean_dbfs=float(db10(adj_p.mean())), adj_median_dbfs=float(db10(np.median(adj_p))),
        adj_max_dbfs=float(db10(adj_p.max())),
        bin0_over_adj_mean_db=float(db10(pw[0]) - db10(adj_p.mean())),
        bin0_over_adj_max_db=float(db10(pw[0]) - db10(adj_p.max())))


def _dc_file(cap: dict) -> dict:
    a = load_ci16(cap["path"])
    x = to_complex(a)
    out = dict(name=cap["name"], group=cap["group"], kind=cap["kind"], gain_db=cap["gain_db"])
    out.update(dc_metrics(x, cap["fs"]))
    out["pfb"] = pfb_dc_metrics(x)
    out["clip_fraction"] = float((np.abs(a) >= 2047).mean())
    return out


def _stream_dc() -> dict | None:
    if not STREAM.exists():
        return None
    meta = json.loads(STREAM.with_name(STREAM.name.replace(".sigmf-data", ".sigmf-meta")).read_text())
    fs = float(meta["global"]["core:sample_rate"])
    chunk = 4_194_304
    total = STREAM.stat().st_size // 4
    s = np.zeros(2)
    n = 0
    first = None
    for off in range(0, total, chunk):
        a = load_ci16(str(STREAM), count=chunk, offset=off)
        s += a.astype(np.float64).sum(axis=0)
        n += len(a)
        if first is None:
            first = to_complex(a)
    m = s / n
    d = dc_metrics(first, fs, nper=4096)
    d.update(file_mean_i=float(m[0]), file_mean_q=float(m[1]),
             file_mean_dbfs=float(20 * np.log10(np.hypot(*m) / FULL_SCALE)), samples=n, fs=fs,
             welch_on="first 4194304 samples")
    return d


@app.command("dc")
def dc_cmd(jobs: int = typer.Option(3, help="worker processes")) -> None:
    """DC spur of the AD9363 alone, per capture, at Welch and PFB resolution."""
    caps = list_captures()
    t0 = time.time()
    with ProcessPoolExecutor(max_workers=jobs) as ex:
        files = list(ex.map(_dc_file, caps))
    sec = dict(files=files, stream_4Msps=_stream_dc(), settings=dict(
        welch_window="blackmanharris", nperseg=DC_NPER, floor_annulus_hz=DC_FLOOR_HZ,
        adjacent_bins=DC_ADJ_BINS, pfb_samples=PFB_SAMPLES, pfb_adjacent_bins=PFB_ADJ,
        dbfs="power re a complex tone of amplitude 2048"),
        seconds=time.time() - t0)
    sec["aggregate"] = dc_aggregate(files)
    update_section("dc", sec)
    typer.echo(json.dumps(sec["aggregate"], indent=1, default=jdefault))


def _stat(v) -> dict:
    v = np.asarray(v, dtype=float)
    return dict(n=int(len(v)), min=float(v.min()), median=float(np.median(v)),
                mean=float(v.mean()), max=float(v.max()))


def dc_aggregate(files: list[dict]) -> dict:
    out = {}
    for label, sel in (("all", files), ("main_454MHz", [f for f in files if f["kind"] == "main"]),
                       ("survey", [f for f in files if f["kind"] == "survey"])):
        if not sel:
            continue
        out[label] = dict(
            mean_i_lsb=_stat([f["mean_i"] for f in sel]), mean_q_lsb=_stat([f["mean_q"] for f in sel]),
            mean_abs_lsb=_stat([f["mean_abs"] for f in sel]),
            mean_dbfs=_stat([f["mean_dbfs"] for f in sel]),
            welch_dc_dbfs=_stat([f["welch_dc_dbfs"] for f in sel]),
            dc_over_floor_db=_stat([f["dc_over_floor_db"] for f in sel]),
            dc_over_adjacent_db=_stat([f["dc_over_adjacent_db"] for f in sel]),
            pfb_bin0_dbfs=_stat([f["pfb"]["bin0_dbfs"] for f in sel]),
            pfb_bin0_over_adj_mean_db=_stat([f["pfb"]["bin0_over_adj_mean_db"] for f in sel]),
            pfb_bin0_over_adj_max_db=_stat([f["pfb"]["bin0_over_adj_max_db"] for f in sel]))
    # groups: per LO
    groups = {}
    for g in sorted({f["group"] for f in files}):
        sel = [f for f in files if f["group"] == g]
        groups[g] = dict(
            n=len(sel), mean_abs_lsb_median=float(np.median([f["mean_abs"] for f in sel])),
            dc_over_floor_db_median=float(np.median([f["dc_over_floor_db"] for f in sel])),
            dc_over_adjacent_db_median=float(np.median([f["dc_over_adjacent_db"] for f in sel])),
            welch_dc_dbfs_median=float(np.median([f["welch_dc_dbfs"] for f in sel])),
            floor_dbfs_median=float(np.median([f["welch_floor_dbfs"] for f in sel])),
            pfb_bin0_over_adj_mean_db_median=float(np.median([f["pfb"]["bin0_over_adj_mean_db"] for f in sel])))
    out["by_lo"] = groups
    return out


# ---------------------------------------------------------------------------
# Images


def second_moments(a: np.ndarray) -> dict:
    """Mean-removed second moments of an (n, 2) int array."""
    i = a[:, 0].astype(np.float64)
    q = a[:, 1].astype(np.float64)
    n = len(i)
    mi, mq = i.mean(), q.mean()
    return dict(n=n, mean_i=mi, mean_q=mq, sii=float(((i - mi) ** 2).sum()),
                sqq=float(((q - mq) ** 2).sum()), siq=float(((i - mi) * (q - mq)).sum()))


def pool_moments(ms: list[dict]) -> dict:
    n = sum(m["n"] for m in ms)
    return dict(n=n, sii=sum(m["sii"] for m in ms), sqq=sum(m["sqq"] for m in ms),
                siq=sum(m["siq"] for m in ms))


def blind_estimate(m: dict) -> dict:
    """Amplitude ratio, phase and the image ratio they imply, from second moments."""
    sii, sqq, siq = m["sii"], m["sqq"], m["siq"]
    rho = siq / np.sqrt(sii * sqq)
    kappa = complex(sii - sqq, 2 * siq) / (sii + sqq)
    k = abs(kappa)
    eps = (1 - np.sqrt(1 - k * k)) / k if k > 0 else 0.0
    return dict(
        gain_ratio_q_over_i=float(np.sqrt(sqq / sii)), gain_imbalance_db=float(10 * np.log10(sqq / sii)),
        rho=float(rho), phase_deg=float(np.degrees(np.arcsin(rho))),
        kappa_abs=float(k), eps_abs=float(eps), implied_irr_db=float(-20 * np.log10(max(eps, 1e-12))),
        # the 1-sigma statistical floor of kappa for n white-ish samples is ~ 1/sqrt(n)
        kappa_sigma=float(1 / np.sqrt(m["n"])))


def blind_matrix(m: dict) -> np.ndarray:
    """2x2 [[a, b], [c, d]] with I' = aI + bQ, Q' = cI + dQ that equalises power and
    decorrelates I and Q: I' = I, Q' = (sI / sQ / sqrt(1-rho^2)) Q - (rho / sqrt(1-rho^2)) I."""
    sii, sqq, siq = m["sii"], m["sqq"], m["siq"]
    rho = siq / np.sqrt(sii * sqq)
    r = np.sqrt(1 - rho * rho)
    return np.array([[1.0, 0.0], [-rho / r, np.sqrt(sii / sqq) / r]])


def eps_matrix(eps: complex) -> np.ndarray:
    """x' = x - eps * conj(x) as a real 2x2 on (I, Q); removes x = s + eps conj(s)."""
    return np.array([[1 - eps.real, -eps.imag], [-eps.imag, 1 + eps.real]])


def quantize_matrix(mat: np.ndarray) -> dict:
    """ad_iqcor coefficients: I' = (c1I*I + c2I*Q) >> 14, Q' = (c1Q*I + c2Q*Q) >> 14."""
    q = np.clip(np.rint(mat * 2 ** 14), -32768, 32767).astype(int)
    return dict(i_coeff_1=int(q[0, 0]), i_coeff_2=int(q[0, 1]),
                q_coeff_1=int(q[1, 0]), q_coeff_2=int(q[1, 1]),
                chan_cntrl_2_i=f"0x{(q[0, 0] & 0xFFFF) << 16 | (q[0, 1] & 0xFFFF):08x}",
                chan_cntrl_2_q=f"0x{(q[1, 0] & 0xFFFF) << 16 | (q[1, 1] & 0xFFFF):08x}")


def iqcor_apply(a: np.ndarray, c: dict) -> np.ndarray:
    """Bit-exact ad_iqcor on (n, 2) int data: the 34-bit products are summed and bits [29:14]
    kept (arithmetic shift = floor), wrapping at 16 bits. Returns (n, 2) int32."""
    i = a[:, 0].astype(np.int64)
    q = a[:, 1].astype(np.int64)
    oi = (c["i_coeff_1"] * i + c["i_coeff_2"] * q) >> 14
    oq = (c["q_coeff_1"] * i + c["q_coeff_2"] * q) >> 14
    out = np.stack([oi, oq], axis=1)
    return (((out + 32768) & 0xFFFF) - 32768).astype(np.int32)


IDENTITY = dict(i_coeff_1=0x4000, i_coeff_2=0, q_coeff_1=0, q_coeff_2=0x4000)


def find_carriers(f: np.ndarray, db: np.ndarray) -> list[dict]:
    fl = ndimage.percentile_filter(db, 20, size=2561, mode="nearest")
    loc = ndimage.median_filter(db, size=129, mode="nearest")
    ex = db - fl
    pk, _ = signal.find_peaks(ex, height=IMG_MIN_EXCESS_DB, distance=20)
    n = len(f)
    out = []
    for k in pk:
        if not (IMG_F_RANGE[0] <= abs(f[k]) <= IMG_F_RANGE[1]) or db[k] - loc[k] < IMG_PROMINENCE_DB:
            continue
        thr = max(db[k] - 30.0, fl[k] + 12.0)
        lo = k
        while lo > 0 and k - lo < 32 and db[lo - 1] >= thr:
            lo -= 1
        hi = k
        while hi < n - 1 and hi - k < 32 and db[hi + 1] >= thr:
            hi += 1
        df = f[1] - f[0]
        km = n - k                      # mirror bin index (f[n//2] == 0)
        km = min(max(km, 0), n - 1)
        out.append(dict(f_hz=float(f[k]), level_dbfs=float(db[k]), floor_dbfs=float(fl[k]),
                        mirror_floor_dbfs=float(fl[km]), excess_db=float(ex[k]),
                        band_hz=[float(f[lo] - df), float(f[hi] + df)]))
    return out


def pair_stats(X: np.ndarray, fs: float, band_hz, mirror_floor_dbfs: float, nper: int) -> dict:
    """Coherent mirror-pair estimate over carrier-side window band_hz = (lo, hi) in Hz."""
    N = len(X)
    dfb = fs / N
    m = np.arange(int(np.ceil(band_hz[0] / dfb)), int(np.floor(band_hz[1] / dfb)) + 1)
    xp = X[m % N].astype(np.complex128)
    xm = X[(-m) % N].astype(np.complex128)
    M = len(m)
    D = float((np.abs(xp) ** 2).sum())
    r = float((np.abs(xm) ** 2).sum())
    num = complex((xm * xp).sum())
    eps = num / (D + r)
    pimg = abs(eps) ** 2 * D            # image power implied in the mirror window
    resid = max(r - pimg, 1e-300)
    n_eff = resid / M
    T = abs(num) ** 2 / (D * n_eff)     # ~ Exp(1) if no coherent image and mirror is noise-like
    # expected per-bin noise from the mirror-side floor (Welch bin power -> FFT bin power)
    n_noise = 10 ** (mirror_floor_dbfs / 10) * FULL_SCALE ** 2 * nper / (BH_ENBW_BINS * N)
    coherent = bool(T > COHERENT_T)
    excess = max(r - M * n_noise, 1e-300)
    if coherent:
        verdict = "image" if pimg >= 0.5 * excess else "image+real"
    else:
        verdict = "real_at_mirror" if r > 2 * M * n_noise else "undetected"
    limit = float(np.sqrt(COHERENT_T * n_eff / D))   # |eps| detection limit
    return dict(
        bins=int(M), eps_re=float(eps.real), eps_im=float(eps.imag), eps_abs=float(abs(eps)),
        coherent=coherent, T=float(T), verdict=verdict,
        raw_mirror_dbc=float(db10(r / D)), noise_mirror_dbc=float(db10(M * n_noise / D)),
        coherent_dbc=float(db10(pimg / D)) if coherent else None,
        irr_db=float(-20 * np.log10(abs(eps))) if coherent else None,
        irr_lower_bound_db=float(-20 * np.log10(limit)),
        sigma_eps=float(np.sqrt(n_eff / D)),
        carrier_power_dbfs=float(db10(D / FULL_SCALE ** 2)))


BANDS_HZ = [(lo, lo + 1e6) for lo in np.arange(0.1e6, 9.1e6, 1e6)]


def band_stats(X: np.ndarray, fs: float) -> list[dict]:
    """Broadband mirror-pair sums over 1 MHz bands of positive frequency. For any signal mix
    num = eps (P + Q) + noise, so eps = num / (P + Q) with P, Q the energy at +f and -f;
    V = sum |X[m]|^2 |X[-m]|^2 is the variance scale of num (sigma_eps = sqrt(V) / (P + Q)).
    The sums are returned so captures can be pooled coherently (eps is stable per LO/gain)."""
    N = len(X)
    dfb = fs / N
    out = []
    for lo, hi in BANDS_HZ:
        m = np.arange(int(np.ceil(lo / dfb)), int(np.floor(hi / dfb)) + 1)
        xp = X[m].astype(np.complex128)
        xm = X[N - m].astype(np.complex128)
        ap, am = np.abs(xp) ** 2, np.abs(xm) ** 2
        num = complex((xp * xm).sum())
        out.append(dict(lo=float(lo), hi=float(hi), num_re=num.real, num_im=num.imag,
                        P=float(ap.sum()), Q=float(am.sum()), V=float((ap * am).sum())))
    return out


def pool_bands(lists: list[list[dict]]) -> list[dict]:
    """Pool band sums over captures; returns eps, sigma and image rejection per band."""
    out = []
    for j in range(len(BANDS_HZ)):
        num = sum(complex(b[j]["num_re"], b[j]["num_im"]) for b in lists)
        pq = sum(b[j]["P"] + b[j]["Q"] for b in lists)
        pr = sum(b[j]["P"] for b in lists)
        qr = sum(b[j]["Q"] for b in lists)
        v = sum(b[j]["V"] for b in lists)
        eps = num / pq
        sig = np.sqrt(v) / pq
        out.append(dict(
            band_mhz=[BANDS_HZ[j][0] / 1e6, BANDS_HZ[j][1] / 1e6], files=len(lists),
            eps_re=eps.real, eps_im=eps.imag, eps_abs=abs(eps), sigma=float(sig),
            p_plus_over_minus_db=float(db10(pr / qr)),
            irr_db=float(-20 * np.log10(max(abs(eps), 1e-12))),
            irr_conservative_db=float(-10 * np.log10(abs(eps) ** 2 + (2 * sig) ** 2))))
    return out


def _images_file(cap: dict) -> dict:
    a = load_ci16(cap["path"])
    x = to_complex(a)
    f, p = welch_power(x, cap["fs"], IMG_NPER)
    db = db10(p)
    cars = find_carriers(f, db)
    X = np.fft.fft(x) / len(x)
    for c in cars:
        c.update(pair_stats(X, cap["fs"], c["band_hz"], c["mirror_floor_dbfs"], IMG_NPER))
        c["file"] = cap["name"]
    return dict(name=cap["name"], group=cap["group"], kind=cap["kind"], gain_db=cap["gain_db"],
                moments=second_moments(a), carriers=cars, bands=band_stats(X, cap["fs"]))


def cluster_carriers(files: list[dict]) -> list[dict]:
    """Group a stationary carrier across the captures of one LO (same f within 3.2 kHz)."""
    out = []
    for g in sorted({f["group"] for f in files}):
        items = []
        for f in files:
            if f["group"] == g:
                for c in f["carriers"]:
                    items.append((c["f_hz"], f["name"], c))
        items.sort(key=lambda t: t[0])
        cl: list[list] = []
        for it in items:
            if cl and it[0] - np.mean([t[0] for t in cl[-1]]) < 3200:
                cl[-1].append(it)
            else:
                cl.append([it])
        for c in cl:
            cs = [t[2] for t in c]
            irr = [x["irr_db"] for x in cs if x["irr_db"] is not None]
            lvl = np.array([x["level_dbfs"] for x in cs])
            mirror = np.array([x["level_dbfs"] + x["raw_mirror_dbc"] for x in cs])
            verdicts = [x["verdict"] for x in cs]
            corr = None
            if len(cs) >= 4 and lvl.std() > 0.5:
                corr = float(np.corrcoef(lvl, mirror)[0, 1])
            out.append(dict(
                group=g, f_hz=float(np.mean([t[0] for t in c])), n=len(cs),
                level_dbfs_mean=float(lvl.mean()), level_dbfs_std=float(lvl.std()),
                excess_db_mean=float(np.mean([x["excess_db"] for x in cs])),
                raw_mirror_dbc_mean=float(np.mean([x["raw_mirror_dbc"] for x in cs])),
                raw_mirror_dbc_std=float(np.std([x["raw_mirror_dbc"] for x in cs])),
                irr_db_mean=float(np.mean(irr)) if irr else None,
                irr_db_std=float(np.std(irr)) if irr else None,
                irr_db_min=float(np.min(irr)) if irr else None,
                irr_lower_bound_db_median=float(np.median([x["irr_lower_bound_db"] for x in cs])),
                eps_phase_deg_std=float(np.degrees(np.std(np.angle(
                    [complex(x["eps_re"], x["eps_im"]) for x in cs])))) if irr else None,
                verdicts={v: verdicts.count(v) for v in sorted(set(verdicts))},
                level_vs_mirror_corr=corr))
    return out


@app.command("images")
def images_cmd(jobs: int = typer.Option(3, help="worker processes")) -> None:
    """Carrier image rejection (coherent) and the blind IQ-imbalance estimate."""
    caps = list_captures()
    t0 = time.time()
    with ProcessPoolExecutor(max_workers=jobs) as ex:
        files = list(ex.map(_images_file, caps))
    clusters = cluster_carriers(files)
    per_file_blind = {f["name"]: blind_estimate(f["moments"]) for f in files}
    groups = {}
    for g in sorted({f["group"] for f in files}):
        ms = [f["moments"] for f in files if f["group"] == g]
        groups[g] = dict(n_files=len(ms), **blind_estimate(pool_moments(ms)))
    pooled_all = blind_estimate(pool_moments([f["moments"] for f in files]))
    band_curves = {g: pool_bands([f["bands"] for f in files if f["group"] == g])
                   for g in sorted({f["group"] for f in files})}
    n_car = sum(len(f["carriers"]) for f in files)
    allc = [c for f in files for c in f["carriers"]]
    img = [c for c in allc if c["irr_db"] is not None]
    sec = dict(
        settings=dict(welch_nperseg=IMG_NPER, min_excess_db=IMG_MIN_EXCESS_DB,
                      prominence_db=IMG_PROMINENCE_DB, f_range_hz=IMG_F_RANGE,
                      coherent_threshold_T=COHERENT_T,
                      floor="20th percentile of the Welch dB over a 4 MHz window"),
        n_files=len(files), n_carrier_detections=n_car,
        n_coherent_images=len(img),
        verdict_counts={v: sum(1 for c in allc if c["verdict"] == v) for v in
                        ("image", "image+real", "real_at_mirror", "undetected")},
        worst_case=min(img, key=lambda c: c["irr_db"]) if img else None,
        clusters=clusters, band_irr_per_lo=band_curves, files=files,
        blind=dict(per_file=per_file_blind, per_lo=groups, pooled_all=pooled_all),
        seconds=time.time() - t0)
    update_section("images", sec)
    typer.echo(json.dumps({k: sec[k] for k in ("n_files", "n_carrier_detections", "n_coherent_images",
                                              "verdict_counts", "worst_case")}, indent=1, default=jdefault))
    typer.echo(json.dumps(sec["blind"]["pooled_all"], indent=1))


# ---------------------------------------------------------------------------
# ad_dcfilter emulation


def dcfilter_exact(x: np.ndarray, coeff: int) -> np.ndarray:
    """Bit-exact RTL recurrence (valid every clock); python loop, use for short vectors."""
    q = [0, 0, 0, 0]
    out = np.empty(len(x), dtype=np.int64)
    for n, v in enumerate(x.tolist()):
        p = n & 3
        d = v - (q[p] >> 17)
        d = ((d + 32768) & 0xFFFF) - 32768
        out[n] = d
        q[p] += d * coeff
    return out


def dcfilter_float(x: np.ndarray, coeff: int) -> np.ndarray:
    """Fast approximation of dcfilter_exact: four one-pole filters with a = coeff / 2**17
    on x[p::4]; the integer floor() inside the RTL loop settles the estimate half an LSB
    above the float one, so the subtracted value is floor(est + 0.5)."""
    a = coeff / 2 ** 17
    est = np.empty(len(x))
    for p in range(4):
        s = signal.lfilter([a], [1.0, -(1.0 - a)], x[p::4].astype(np.float64))
        est[p::4] = np.concatenate(([0.0], s[:-1]))
    return x - np.floor(est + 0.5).astype(np.int64)


def dc_corner_hz(coeff: int, fs: float = FS, clocks_per_sample: float = 1.0) -> float:
    """-3 dB corner realised by the RTL (c / 2**19 per clock; valid every clock)."""
    a = clocks_per_sample * coeff / 2 ** 19
    return float(-np.log(1 - a) * fs / (2 * np.pi)) if a < 1 else float("inf")


def driver_coeff(f_hz: float, fs: float = FS) -> int:
    """cf_axi_adc_core.c: llval = 102944 * val / adc_clk, clamp(1, 0x4000)."""
    return int(min(max(int(102944 * f_hz / fs), 1), 0x4000))


def _emulate_file(args) -> dict:
    cap, plan = args
    a = load_ci16(cap["path"])
    out = dict(name=cap["name"], group=cap["group"], kind=cap["kind"])
    seg = slice(SETTLE, None)

    def measure(arr: np.ndarray) -> dict:
        x = to_complex(arr[seg])
        d = dc_metrics(x, cap["fs"])
        d["pfb"] = pfb_dc_metrics(x, offset=0)
        return d

    out["before_dc"] = measure(a)
    out["dc_filter"] = {}
    for label, c in DC_COEFFS.items():
        i = dcfilter_float(a[:, 0].astype(np.int64), c)
        q = dcfilter_float(a[:, 1].astype(np.int64), c)
        out["dc_filter"][label] = measure(np.stack([i, q], axis=1))

    # IQ correction variants: carrier windows and band sums as in the images stage
    cars = plan["carriers"]
    N = len(a)
    out["iq"] = {"none": dict(carriers=cars, bands=plan["bands"])}
    for name, mat in plan["matrices"].items():
        co = quantize_matrix(np.asarray(mat))
        b = iqcor_apply(a, co)
        Xb = np.fft.fft(to_complex(b)) / N
        out["iq"][name] = dict(
            carriers=[pair_stats(Xb, cap["fs"], c["band_hz"], c["mirror_floor_dbfs"], IMG_NPER) for c in cars],
            bands=band_stats(Xb, cap["fs"]), coeffs=co,
            mean_out_lsb=[float(b[:, 0].mean()), float(b[:, 1].mean())])
        del Xb, b
    return out


@app.command("emulate")
def emulate_cmd(jobs: int = typer.Option(3, help="worker processes")) -> None:
    """Before/after for the emulated DC filter and the quantised blind IQ correction."""
    img = read_section("images")
    caps = list_captures()
    by_name = {f["name"]: f for f in img["files"]}
    pooled_all = pool_moments([f["moments"] for f in img["files"]])
    lo_pool = {}
    for g in sorted({f["group"] for f in img["files"]}):
        lo_pool[g] = pool_moments([f["moments"] for f in img["files"] if f["group"] == g])
    # best single frequency-flat correction per LO, fitted in-sample on the band sums:
    # eps = sum(num) / sum(P + Q) over all bands and captures (an upper bound for ad_iqcor)
    lo_eps = {}
    for g in lo_pool:
        fl = [f["bands"] for f in img["files"] if f["group"] == g]
        num = sum(complex(b["num_re"], b["num_im"]) for bl in fl for b in bl)
        pq = sum(b["P"] + b["Q"] for bl in fl for b in bl)
        lo_eps[g] = num / pq
    work = []
    for cap in caps:
        f = by_name[cap["name"]]
        plan = dict(carriers=f["carriers"], bands=f["bands"], matrices=dict(
            blind_self=blind_matrix(f["moments"]),
            blind_lo_pooled=blind_matrix(lo_pool[cap["group"]]),
            blind_global=blind_matrix(pooled_all),
            band_oracle_lo=eps_matrix(lo_eps[cap["group"]])))
        work.append((cap, plan))
    t0 = time.time()
    with ProcessPoolExecutor(max_workers=jobs) as ex:
        files = list(ex.map(_emulate_file, work))
    sec = dict(
        dc_coefficients={k: dict(coeff=v, realised_corner_hz_rtl_fixed_clock1=dc_corner_hz(v),
                                 driver_assumed_corner_hz=v * FS / 102944)
                         for k, v in DC_COEFFS.items()},
        driver_coeff_for=dict({f"{hz}Hz": driver_coeff(hz) for hz in (100, 300, 1000, 3000, 10000)}),
        settle_samples=SETTLE,
        iq_matrices=dict(blind_global=quantize_matrix(blind_matrix(pooled_all)),
                         blind_lo_pooled={g: quantize_matrix(blind_matrix(m)) for g, m in lo_pool.items()},
                         band_oracle_lo={g: dict(eps=[e.real, e.imag], **quantize_matrix(eps_matrix(e)))
                                         for g, e in lo_eps.items()}),
        files=files, seconds=time.time() - t0)
    update_section("emulate", sec)
    typer.echo(f"emulated {len(files)} captures in {sec['seconds']:.0f} s")


# ---------------------------------------------------------------------------
# Summary


def _q(v, p):
    return float(np.percentile(np.asarray(v, dtype=float), p))


def _carrier_irr(c: dict) -> float:
    """Conservative image rejection of one carrier measurement (coherent value, else the bound)."""
    return c["irr_db"] if c["irr_db"] is not None else c["irr_lower_bound_db"]


def build_summary() -> dict:
    cur = json.loads(OUT.read_text())
    s = {}
    if "identity_sim" in cur:
        s["identity_sim"] = {k: cur["identity_sim"][k] for k in ("passed", "samples_checked", "errors")}
    if "dc" in cur:
        s["dc_real_captures"] = cur["dc"]["aggregate"]
    if "images" in cur:
        im = cur["images"]
        s["images"] = dict(
            n_carrier_detections=im["n_carrier_detections"], verdict_counts=im["verdict_counts"],
            worst_case=im["worst_case"],
            clusters=[{k: c[k] for k in ("group", "f_hz", "n", "level_dbfs_mean", "excess_db_mean",
                                         "raw_mirror_dbc_mean", "raw_mirror_dbc_std", "irr_db_mean",
                                         "irr_db_std", "irr_db_min", "irr_lower_bound_db_median",
                                         "verdicts", "level_vs_mirror_corr")}
                      for c in im["clusters"]],
            band_irr_per_lo=im["band_irr_per_lo"],
            blind_pooled_all=im["blind"]["pooled_all"], blind_per_lo=im["blind"]["per_lo"])
    if "emulate" in cur and "images" in cur:
        em = cur["emulate"]
        dcs = {}
        for label in ["before"] + list(DC_COEFFS):
            rows = [f["before_dc"] if label == "before" else f["dc_filter"][label] for f in em["files"]]
            dcs[label] = dict(
                mean_abs_lsb_median=float(np.median([d["mean_abs"] for d in rows])),
                mean_abs_lsb_p90=_q([d["mean_abs"] for d in rows], 90),
                mean_abs_lsb_max=float(max(d["mean_abs"] for d in rows)),
                dc_over_floor_db_median=float(np.median([d["dc_over_floor_db"] for d in rows])),
                dc_over_adjacent_db_median=float(np.median([d["dc_over_adjacent_db"] for d in rows])),
                pfb_bin0_over_adj_mean_db_median=float(np.median(
                    [d["pfb"]["bin0_over_adj_mean_db"] for d in rows])),
                pfb_bin0_over_adj_mean_db_p90=_q([d["pfb"]["bin0_over_adj_mean_db"] for d in rows], 90))
        s["emulated_dc"] = dict(coefficients=em["dc_coefficients"], driver_coeff_for=em["driver_coeff_for"],
                                results=dcs)
        variants = ["none", "blind_self", "blind_lo_pooled", "blind_global", "band_oracle_lo"]
        iqs = {}
        for v in variants:
            car_b, car_a, raw_b, raw_a = [], [], [], []
            band_irr, band_lb = [], []
            means = []
            for f in em["files"]:
                it = f["iq"]
                for cb, ca in zip(it["none"]["carriers"], it[v]["carriers"]):
                    if cb["irr_db"] is not None:
                        car_b.append(_carrier_irr(cb))
                        car_a.append(_carrier_irr(ca))
                        raw_b.append(cb["raw_mirror_dbc"])
                        raw_a.append(ca["raw_mirror_dbc"])
                if v != "none":
                    means.append(it[v]["mean_out_lsb"])
            # band curves pooled per LO for this variant
            curves = {}
            for g in sorted({f["group"] for f in em["files"]}):
                curves[g] = pool_bands([f["iq"][v]["bands"] for f in em["files"] if f["group"] == g])
            allirr = [b["irr_conservative_db"] for c in curves.values() for b in c]
            iqs[v] = dict(
                carriers=dict(n=len(car_b), irr_median_db=float(np.median(car_b)) if car_b else None,
                              irr_worst_db=float(np.min(car_b)) if car_b else None,
                              raw_mirror_dbc_median=float(np.median(raw_b)) if raw_b else None,
                              irr_after_this_variant_median_db=float(np.median(car_a)) if car_a else None,
                              irr_after_this_variant_worst_db=float(np.min(car_a)) if car_a else None),
                bands_pooled_per_lo=dict(
                    irr_conservative_median_db=float(np.median(allirr)), irr_conservative_worst_db=float(np.min(allirr)),
                    irr_conservative_best_db=float(np.max(allirr)), curves=curves),
                added_dc_lsb_median=[float(np.median([m[0] for m in means])), float(np.median([m[1] for m in means]))]
                if means else None)
        # carriers: "before" is variant none; "after" is the variant's own column
        s["emulated_iq"] = iqs
    return s


@app.command("summary")
def summary_cmd() -> None:
    s = build_summary()
    update_section("summary", s)
    typer.echo(json.dumps(s, indent=1, default=jdefault))


# ---------------------------------------------------------------------------
# RTL simulations


def _sim_env() -> dict:
    env = dict(os.environ)
    env["LD_LIBRARY_PATH"] = f"{Path.home() / 'amd/compat-lib'}:{env.get('LD_LIBRARY_PATH', '')}"
    return env


def _run(cmd: list[str], cwd: Path, check: bool = True) -> str:
    r = subprocess.run(MEM_LIMIT + cmd, cwd=cwd, env=_sim_env(), capture_output=True, text=True)
    if check and r.returncode:
        raise RuntimeError(f"{' '.join(cmd)}\n{r.stdout[-2000:]}\n{r.stderr[-2000:]}")
    return r.stdout + r.stderr


GLBL = Path.home() / "amd/2026.1/Vivado/data/verilog/src/glbl.v"
RTL = dict(dcfilter=HDL / "library/xilinx/common/ad_dcfilter.v", iqcor=HDL / "library/common/ad_iqcor.v",
           mul=HDL / "library/xilinx/common/ad_mul.v")


@app.command("identity-sim")
def identity_sim_cmd() -> None:
    """Run fpga/tb/tb_adc_corr_identity.sv: the cores at the driver's defaults are an identity."""
    d = SIM_DIR / "identity"
    d.mkdir(parents=True, exist_ok=True)
    b = VIVADO_BIN
    srcs = [str(TB_DIR / "tb_adc_corr_identity.sv"), str(RTL["dcfilter"]), str(RTL["iqcor"]),
            str(RTL["mul"]), str(GLBL)]
    _run([str(b / "xvlog"), "-sv", *srcs], d)
    _run([str(b / "xelab"), "-L", "unisims_ver", "-L", "unimacro_ver", "tb_adc_corr_identity", "glbl",
          "-s", "sim", "-debug", "off"], d)
    log = _run([str(b / "xsim"), "sim", "-runall"], d)
    m = re.search(r"identity: (\d+) samples checked, (\d+) errors", log)
    nc = re.search(r"negative control: (\d+) of (\d+)", log)
    lat = re.search(r"latency: (\d+) clocks", log)
    sec = dict(passed="PASS tb_adc_corr_identity" in log, samples_checked=int(m.group(1)) if m else None,
               errors=int(m.group(2)) if m else None,
               negative_control_differing=[int(nc.group(1)), int(nc.group(2))] if nc else None,
               latency_clocks_removed=int(lat.group(1)) if lat else None,
               modes="valid every clock / ~50% / ~25%, I and Q, random sign-extended 12-bit incl. -2048 and 2047",
               dcfilt_enb=0, dcfilt_offset=0, i_coeffs=[0x4000, 0], q_coeffs=[0, 0x4000], CR=16,
               log_tail=[ln for ln in log.splitlines() if ln.startswith(("PASS", "FAIL", "identity", "negative", "single"))])
    update_section("identity_sim", sec)
    typer.echo(json.dumps(sec, indent=1))


def _build_vec(variant: str) -> Path:
    d = SIM_DIR / f"vec_{variant}"
    d.mkdir(parents=True, exist_ok=True)
    dcf = RTL["dcfilter"]
    if variant == "cep1":
        patched = d / "ad_dcfilter_cep1.v"
        patched.write_text(dcf.read_text().replace(".CEP (1'd0)", ".CEP (1'd1)"))
        dcf = patched
    b = VIVADO_BIN
    _run([str(b / "xvlog"), "-sv", str(TB_DIR / "tb_adc_corr_vec.sv"), str(dcf), str(RTL["iqcor"]),
          str(RTL["mul"]), str(GLBL)], d)
    _run([str(b / "xelab"), "-L", "unisims_ver", "-L", "unimacro_ver", "tb_adc_corr_vec", "glbl",
          "-s", "vec", "-debug", "off"], d)
    return d


def _run_vec(d: Path, inp: Path, name: str, dcenb: int, dccoeff: int, co: dict) -> np.ndarray:
    outp = d / f"out_{name}.txt"
    _run([str(VIVADO_BIN / "xsim"), "vec", "-runall",
          "-testplusarg", f"in={inp}", "-testplusarg", f"out={outp}",
          "-testplusarg", f"dcenb={dcenb}", "-testplusarg", f"dccoeff={dccoeff}",
          "-testplusarg", f"ic1={co['i_coeff_1']}", "-testplusarg", f"ic2={co['i_coeff_2']}",
          "-testplusarg", f"qc1={co['q_coeff_1']}", "-testplusarg", f"qc2={co['q_coeff_2']}"], d)
    return np.loadtxt(outp, dtype=np.int64)


@app.command("verify-rtl")
def verify_rtl_cmd(samples: int = typer.Option(300_000, help="vector length per run")) -> None:
    """Compare the Python emulations to the RTL on real capture data (xsim)."""
    caps = [c for c in list_captures() if c["kind"] == "main"]
    a = load_ci16(caps[0]["path"], count=samples)
    SIM_DIR.mkdir(parents=True, exist_ok=True)
    inp = SIM_DIR / "vec_in.txt"
    np.savetxt(inp, a, fmt="%d")
    img = read_section("images")
    co = quantize_matrix(blind_matrix(pool_moments([f["moments"] for f in img["files"]])))
    # also a deliberately large correction so the truncation/rounding path is exercised
    co_big = quantize_matrix(np.array([[1.01, -0.03], [0.02, 0.97]]))
    rep: dict = {"samples": int(len(a)), "blind_global_coeffs": co, "big_test_coeffs": co_big}

    d0 = _build_vec("orig")
    r = _run_vec(d0, inp, "shipped_enb1", 1, 4096, IDENTITY)
    rep["shipped_rtl_dcfilter_enabled_coeff4096"] = dict(
        dc_out_equals_input=bool(np.array_equal(r[:, 0], a[:len(r), 0]) and np.array_equal(r[:, 1], a[:len(r), 1])),
        iqcor_identity_out_equals_input=bool(np.array_equal(r[:, 2], a[:len(r), 0]) and np.array_equal(r[:, 3], a[:len(r), 1])),
        n_out=int(len(r)))
    d1 = _build_vec("cep1")
    rep["cep1_rtl"] = {}
    for dc_c in (4096, 128, 4):
        for tag, cc in (("blind", co), ("big", co_big)):
            r = _run_vec(d1, inp, f"cep1_{dc_c}_{tag}", 1, dc_c, cc)
            n = len(r)
            ex_i = dcfilter_exact(a[:n, 0].astype(np.int64), dc_c)
            ex_q = dcfilter_exact(a[:n, 1].astype(np.int64), dc_c)
            fl_i = dcfilter_float(a[:n, 0].astype(np.int64), dc_c)
            fl_q = dcfilter_float(a[:n, 1].astype(np.int64), dc_c)
            dcr = r[:, :2]
            iq_model = iqcor_apply(dcr, cc)
            rep["cep1_rtl"][f"dc{dc_c}_{tag}"] = dict(
                n_out=int(n),
                dc_exact_mismatches=int((ex_i != dcr[:, 0]).sum() + (ex_q != dcr[:, 1]).sum()),
                dc_float_max_abs_diff=int(max(np.abs(fl_i - dcr[:, 0]).max(), np.abs(fl_q - dcr[:, 1]).max())),
                dc_float_frac_equal=float(((fl_i == dcr[:, 0]).mean() + (fl_q == dcr[:, 1]).mean()) / 2),
                dc_float_mean_diff_lsb=float((fl_i - dcr[:, 0]).mean()),
                iqcor_mismatches=int((iq_model != r[:, 2:4]).sum()),
                iqcor_out_mean_lsb=[float(r[:, 2].mean()), float(r[:, 3].mean())])
    update_section("rtl_cross_check", rep)
    typer.echo(json.dumps(rep, indent=1))


@app.command("all")
def all_cmd(jobs: int = typer.Option(3)) -> None:
    identity_sim_cmd()
    dc_cmd(jobs)
    images_cmd(jobs)
    emulate_cmd(jobs)
    verify_rtl_cmd()
    summary_cmd()


if __name__ == "__main__":
    app()
