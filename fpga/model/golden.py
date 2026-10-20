"""Memory images for the RTL and bit-accurate vectors for the self-checking testbench.

    PYTHONPATH=. uv run python -m fpga.model.golden mems fpga/rtl/mem
    PYTHONPATH=. uv run python -m fpga.model.golden vectors <scenario> <outdir>

Vector files (hex, one value per line) consumed by fpga/tb/tb_chan.sv:
  stim.hex        ADC words {Q[15:0], I[15:0]}
  bins.hex        expected 16-bit slice samples {Q,I} for frame f, bin k at f*512 + k
  level.hex       expected LEVEL window {floor, level} after the last complete block
  active.hex      expected ACTIVE bitmap, 16 x 32-bit words
  psd.hex         expected PSD window after the last complete long-PSD period
  mask.hex        FORWARD_MASK, 16 x 32-bit words
  events.hex      expected activity events {open, bin[8:0], frame[31:0]} (42 bits),
                  in RTL order: by frame, then FFT output (bit-reversed) bin order
  params.json     scenario summary (expected counters)
  plusargs.txt    the same values as testbench plusargs
"""

from __future__ import annotations

import dataclasses
import json
from pathlib import Path

import numpy as np
import typer

from . import activity as act
from .channelizer import DEFAULT, pfb_fixed, prototype_fixed

app = typer.Typer(add_completion=False, no_args_is_help=True)

# Small detector constants so a simulation covers several blocks, sub-window
# rotations and long-PSD periods; the RTL takes the same values as parameters.
SIM_DET = act.DetectorConfig(block_log2=2, sub_blocks=2, sub_windows=4, bias_q8=20,
                             on_q8=300, off_q8=150, hang=2, psd_log2=3)


def hexfile(path: Path, values, width_bits: int) -> None:
    digits = (width_bits + 3) // 4
    mask = (1 << width_bits) - 1
    path.write_text("".join(f"{int(v) & mask:0{digits}x}\n" for v in values))


def bit_reverse_order(n: int) -> list[int]:
    """Bin order of the xfft's bit-reversed output (XK_INDEX sequence)."""
    bits = n.bit_length() - 1
    return [int(f"{i:0{bits}b}"[::-1], 2) for i in range(n)]


@app.command()
def mems(outdir: Path) -> None:
    """Coefficient ROM images (4 x 1024 x 18) and the log2 mantissa table."""
    outdir.mkdir(parents=True, exist_ok=True)
    h = prototype_fixed(DEFAULT).reshape(DEFAULT.taps, DEFAULT.n)
    for m in range(4):
        hexfile(outdir / f"pfb_coef_p{m}.mem", np.concatenate([h[2 * m], h[2 * m + 1]]), 18)
    hexfile(outdir / "chan_log2_table.mem", act.MANTISSA_TABLE, 8)


def stimulus(name: str, rng: np.random.Generator) -> tuple[np.ndarray, dict]:
    cfg = DEFAULT
    frames = 48
    n = cfg.length + cfg.hop * (frames - 1)
    t = np.arange(n)

    def tone(f, a):
        return a * np.exp(1j * (2 * np.pi * f * t / cfg.fs + rng.uniform(0, 2 * np.pi)))

    opts = {"out_shift": DEFAULT.out_shift, "ready_pct": 70, "adc_period_ns": 39.0625, "adc_gap_pct": 0,
            "mask_bins": [0, 1, 7, 40, 41, 255, 256, 300, 511], "auto_forward": 0,
            "expect_drops": 0, "expect_overflow": 0, "reset_at_frame": -1,
            "snapshot_at_frame": -1, "det_reset_at_frame": -1, "adc_losses": 0,
            "hist_first": 0, "hist_pairs": 256, "evt_poll": 1}
    if name == "mixed":
        x = (tone(0, 300) + tone(40 * cfg.bin_hz, 600) + tone(41 * cfg.bin_hz + 25e3, 400)
             + tone(-212 * cfg.bin_hz + 12.5e3, 200) + tone(7 * cfg.bin_hz + 31.25e3, 100)
             + 30 * (rng.standard_normal(n) + 1j * rng.standard_normal(n)))
        # a carrier that keys up, then down: exercises open, hang and close
        x[n // 4: 5 * n // 8] += tone(300 * cfg.bin_hz, 900)[n // 4: 5 * n // 8]
        opts["adc_gap_pct"] = 20
        # nobody drains events here: more than 512 occur, so the FIFO overflows
        opts["evt_poll"] = 0
    elif name == "fullscale":
        # beyond the 12-bit range (clipped), with DC; small out_shift forces 16-bit
        # output saturation
        x = 2600 * np.sign(np.cos(2 * np.pi * 40 * cfg.bin_hz * t / cfg.fs)) + 1j * 2047
        x = x + tone(5 * cfg.bin_hz, 1800)
        opts["out_shift"] = 2
        opts["ready_pct"] = 100
    elif name == "random":
        x = rng.integers(-2048, 2048, n) + 1j * rng.integers(-2048, 2048, n)
        opts["ready_pct"] = 50
        opts["mask_bins"] = sorted(rng.choice(512, 64, replace=False).tolist())
        # a history window that wraps past bin 511 (a span centred on DC)
        opts["hist_first"], opts["hist_pairs"] = 400, 100
    elif name == "backpressure":
        x = tone(10 * cfg.bin_hz, 1000) + 20 * (rng.standard_normal(n) + 1j * rng.standard_normal(n))
        opts["ready_pct"] = 4
        opts["expect_drops"] = 1
        opts["mask_bins"] = list(range(0, 512, 2))
        # an odd-length window: the FIFO room check uses the shorter record
        opts["hist_first"], opts["hist_pairs"] = 101, 37
    elif name == "overflow":
        x = tone(3 * cfg.bin_hz, 1000)
        opts["adc_period_ns"] = 5.0          # 200 Msps into a 100 MHz consumer
        opts["expect_overflow"] = 1
        opts["expect_drops"] = 1
    elif name == "snapshot":
        # live self-test path: freeze at a frame boundary, read back the exact input
        # window and FFT output; the ADC keeps running, so the FIFO overflows
        x = (tone(17 * cfg.bin_hz + 9e3, 700) + tone(-100 * cfg.bin_hz, 300)
             + 25 * (rng.standard_normal(n) + 1j * rng.standard_normal(n)))
        opts["snapshot_at_frame"] = 12
        opts["expect_overflow"] = 1
    elif name == "reset":
        x = tone(20 * cfg.bin_hz, 800) + 10 * (rng.standard_normal(n) + 1j * rng.standard_normal(n))
        opts["reset_at_frame"] = 20
        opts["adc_losses"] = 1                # the ADC core reset counts as one loss
    elif name == "autofwd":
        # auto-forward (mask | ACTIVE) and a detector reset while running; data words
        # are still checked bit-exactly whatever selected them
        x = (tone(60 * cfg.bin_hz, 800) + tone(-30 * cfg.bin_hz + 20e3, 500)
             + 15 * (rng.standard_normal(n) + 1j * rng.standard_normal(n)))
        opts["auto_forward"] = 1
        opts["det_reset_at_frame"] = 24
        opts["mask_bins"] = [5]
    else:
        raise typer.BadParameter(name)
    return x, opts


@app.command()
def vectors(scenario: str, outdir: Path, seed: int = 1) -> None:
    rng = np.random.default_rng(seed)
    x, opts = stimulus(scenario, rng)
    outdir.mkdir(parents=True, exist_ok=True)
    xi = np.rint(x.real).astype(np.int64)
    xq = np.rint(x.imag).astype(np.int64)
    xi = np.clip(xi, -32768, 32767)
    xq = np.clip(xq, -32768, 32767)
    hexfile(outdir / "stim.hex", ((xq & 0xFFFF) << 16) | (xi & 0xFFFF), 32)

    cfg = DEFAULT
    r = pfb_fixed(xi + 1j * xq, cfg, out_shift=opts["out_shift"], require_bit_exact=True)
    words = ((r.out_im & 0xFFFF) << 16) | (r.out_re & 0xFFFF)
    hexfile(outdir / "bins.hex", words.reshape(-1), 32)

    st = act.initial_state(cfg.n, SIM_DET)
    last_block = last_psd = None
    prev_active = np.zeros(cfg.n, bool)
    fft_order = bit_reverse_order(cfg.n)
    events = []
    for f in range(r.power.shape[0]):
        blk, psd = act.step(st, r.power[f], SIM_DET)
        if blk is not None:
            last_block = blk
            changed = blk.active != prev_active
            events += [(int(blk.active[k]) << 41) | (k << 32) | (f & 0xFFFFFFFF)
                       for k in fft_order if changed[k]]
            prev_active = blk.active.copy()
        if psd is not None:
            last_psd = psd
    hexfile(outdir / "events.hex", events if events else [0], 42)
    level = (last_block.floor.astype(np.int64) << 16) | last_block.level
    hexfile(outdir / "level.hex", level, 32)
    bits = last_block.active.astype(np.int64)
    hexfile(outdir / "active.hex",
            [int(sum(int(bits[32 * w + b]) << b for b in range(32))) for w in range(16)], 32)
    hexfile(outdir / "psd.hex", last_psd, 16)
    frames = r.out_re.shape[0]
    params = {**opts, "frames": frames, "samples": len(xi),
              "sat_samples": int(r.out_sat.sum()), "events": len(events),
              "clipped_samples": int(np.sum((np.abs(np.clip(xi, -2048, 2047) - xi) > 0)
                                            | (np.abs(np.clip(xq, -2048, 2047) - xq) > 0))),
              "det": dataclasses.asdict(SIM_DET)}
    (outdir / "params.json").write_text(json.dumps(params, indent=2) + "\n")
    mask = np.zeros(512, np.int64)
    mask[opts["mask_bins"]] = 1
    hexfile(outdir / "mask.hex",
            [int(sum(int(mask[32 * w + b]) << b for b in range(32))) for w in range(16)], 32)
    plus = [f"+FRAMES={frames}", f"+SAMPLES={len(xi)}", f"+OUT_SHIFT={opts['out_shift']}",
            f"+READY_PCT={opts['ready_pct']}", f"+ADC_PERIOD_PS={int(opts['adc_period_ns'] * 1000)}",
            f"+ADC_GAP_PCT={opts['adc_gap_pct']}", f"+EXPECT_DROPS={opts['expect_drops']}",
            f"+EXPECT_OVERFLOW={opts['expect_overflow']}", f"+SAT_SAMPLES={params['sat_samples']}",
            f"+CLIPPED={params['clipped_samples']}", f"+RESET_AT_FRAME={opts['reset_at_frame']}",
            f"+ON_Q8={SIM_DET.on_q8}", f"+OFF_Q8={SIM_DET.off_q8}", f"+HANG={SIM_DET.hang}",
            f"+BIAS_Q8={SIM_DET.bias_q8}", f"+SNAPSHOT_AT_FRAME={opts['snapshot_at_frame']}",
            f"+AUTO_FWD={opts['auto_forward']}", f"+DET_RESET_AT_FRAME={opts['det_reset_at_frame']}",
            f"+ADC_LOSSES={opts['adc_losses']}", f"+HIST_FIRST={opts['hist_first']}",
            f"+HIST_PAIRS={opts['hist_pairs']}", f"+EVENTS={len(events)}",
            f"+EVT_POLL={opts['evt_poll']}"]
    (outdir / "plusargs.txt").write_text(" ".join(plus) + "\n")
    print(json.dumps({k: v for k, v in params.items() if k != "det"}))


if __name__ == "__main__":
    from .xfft_cmodel import ensure_loader_path

    ensure_loader_path(["-m", "fpga.model.golden", *__import__("sys").argv[1:]])
    app()
