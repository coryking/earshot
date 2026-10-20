"""Check live Pluto channelizer output against the bit-accurate model.

    PYTHONPATH=. uv run python -m fpga.model.live_check snap <snap.bin> [--out-shift 9]
    PYTHONPATH=. uv run python -m fpga.model.live_check stream <hist|sel> <dma.bin>
    PYTHONPATH=. uv run python -m fpga.model.live_check psd <psd.json> <raw_ci16.bin>

`snap` is the strong test: the hardware froze at a frame boundary and returned that
frame's exact 4096 input samples and its 512 output bins; the model recomputes the
frame from those inputs and every bin must match bit for bit.
"""

from __future__ import annotations

import json
from pathlib import Path

import numpy as np
import typer

from . import activity as act
from .channelizer import DEFAULT, fir_fixed, output_quantize, power_word, prototype_fixed

app = typer.Typer(add_completion=False, no_args_is_help=True)


@app.command()
def snap(path: Path, out_shift: int = DEFAULT.out_shift) -> None:
    from . import xfft_cmodel

    raw = np.fromfile(path, "<u4")
    frame = int(raw[0])
    inp = raw[1:1 + 4096]
    bins = raw[1 + 4096:1 + 4096 + 512]
    i = (inp & 0xFFFF).astype(np.int64)
    q = (inp >> 16).astype(np.int64)
    i = np.where(i >= 32768, i - 65536, i)
    q = np.where(q >= 32768, q - 65536, q)
    ur, ui = fir_fixed(i, q, prototype_fixed(DEFAULT), DEFAULT)
    fft = xfft_cmodel.XFFT(9, DEFAULT.fft_in_bits, DEFAULT.twiddle_bits, DEFAULT.fft_arch,
                           DEFAULT.fft_rounding)
    yr, yi = fft(ur[0], ui[0])
    y = np.stack([yr, yi])
    if frame & 1:
        y[:, 1::2] = -y[:, 1::2]
    o_r, o_i, sat = output_quantize(y[0], y[1], out_shift)
    exp = ((o_i & 0xFFFF) << 16) | (o_r & 0xFFFF)
    mism = int(np.sum(exp.astype(np.uint32) != bins))
    hw = (bins & 0xFFFF).astype(np.int64)
    hw = np.where(hw >= 32768, hw - 65536, hw)
    pw = power_word(yr, yi)
    strongest = np.argsort(-pw)[:5]
    out = {"frame": frame, "input_rms_adc": float(np.sqrt(np.mean(i * i + q * q))),
           "input_peak_adc": int(max(np.abs(i).max(), np.abs(q).max())),
           "bins_compared": 512, "mismatches": mism, "saturated": int(sat.sum()),
           "strongest_bins": [{"bin": int(k), "level_db": float(10 * np.log10(pw[k] + 1))}
                              for k in strongest]}
    print(json.dumps(out))
    raise typer.Exit(0 if mism == 0 else 1)


@app.command()
def stream(kind: str, path: Path) -> None:
    w = np.fromfile(path, "<u8")
    out = {"words": int(len(w))}
    if kind == "hist":
        hdr = np.nonzero((w >> 56) == 0xA1)[0]
        frames = (w[hdr] & ((1 << 48) - 1)).astype(np.int64)
        whole = [h for h in hdr if h + 256 < len(w)]
        gaps = np.diff(frames)
        out.update({"records": int(len(hdr)), "complete_records": len(whole),
                    "first_frame": int(frames[0]) if len(frames) else None,
                    "frame_steps": {str(int(k)): int(v) for k, v in
                                    zip(*np.unique(gaps, return_counts=True))},
                    "record_spacing_ok": bool(np.all(np.diff(hdr) == 257)),
                    "flags_set": int(np.sum(((w[hdr] >> 48) & 0xFF) != 0))})
    else:
        hdr = np.nonzero((w >> 56) == 0xA2)[0]
        frames = (w[hdr] & ((1 << 48) - 1)).astype(np.int64)
        samp = w[(w >> 60) == 0x5]
        binsel = np.unique((samp >> 48) & 0x1FF)
        per = np.diff(np.append(hdr, len(w))) - 1
        out.update({"records": int(len(hdr)), "bins": binsel.tolist(),
                    "samples_per_record": {str(int(k)): int(v) for k, v in
                                           zip(*np.unique(per, return_counts=True))},
                    "frame_steps": {str(int(k)): int(v) for k, v in
                                    zip(*np.unique(np.diff(frames), return_counts=True))},
                    "flags_set": int(np.sum(((w[hdr] >> 48) & 0xFF) != 0))})
    print(json.dumps(out))


@app.command()
def psd(psd_json: Path, raw: Path, fs: float = 25.6e6) -> None:
    """Compare the FPGA long PSD with the model PSD of a contemporaneous raw capture."""
    from .channelizer import pfb_fixed

    hw = np.array(json.loads(psd_json.read_text()), dtype=float) * act.DB_PER_LSB
    x = np.fromfile(raw, "<i2").astype(np.int64)
    xi, xq = x[0::2], x[1::2]
    r = pfb_fixed(xi + 1j * xq, DEFAULT)
    n = 1 << act.DetectorConfig().psd_log2
    p = r.power[:n].sum(axis=0) if r.power.shape[0] >= n else r.power.sum(axis=0) * n / r.power.shape[0]
    model = act.log2_q8(np.rint(p).astype(np.int64)) * act.DB_PER_LSB
    inband = np.abs(np.fft.fftfreq(512, 1 / fs)) < 9.5e6
    d = hw[inband] - model[inband]
    print(json.dumps({"frames_in_capture": int(r.power.shape[0]), "bins": int(inband.sum()),
                      "median_diff_db": float(np.median(d)), "p90_abs_diff_db":
                      float(np.percentile(np.abs(d - np.median(d)), 90)),
                      "corr": float(np.corrcoef(hw[inband], model[inband])[0, 1])}))


if __name__ == "__main__":
    from .xfft_cmodel import ensure_loader_path

    ensure_loader_path(["-m", "fpga.model.live_check", *__import__("sys").argv[1:]])
    app()
