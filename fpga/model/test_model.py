"""Unit tests for the channelizer model (no Vivado, no private data needed).

    uv run pytest -q fpga/model/test_model.py
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import numpy as np
import pytest

from fpga.model import activity as act
from fpga.model.channelizer import (DEFAULT, Config, c_reference_prototype, fir_fixed,
                                    fixed_gain, pfb_float, prototype_fixed, round_shift)

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "fpga" / "sw"))
import fit  # noqa: E402


@pytest.fixture(scope="module")
def c_pfb(tmp_path_factory):
    """The ARM reference (offload/arm/pfb.c) built for the host."""
    out = tmp_path_factory.mktemp("cref") / "pfb"
    arm = ROOT / "offload" / "arm"
    subprocess.run(["cc", "-O2", "-w", str(arm / "pfb.c"), str(arm / "prototype.c"),
                    str(arm / "gate.c"), "-lm", "-o", str(out)], check=True)
    return out


def test_frame_alignment_matches_arm_reference(c_pfb, tmp_path):
    """Same filter as pfb.c through the model's PFB: same frames, phase and bins."""
    rng = np.random.default_rng(5)
    n_bins, taps, frames = 512, 8, 12
    count = n_bins * taps + (frames - 1) * n_bins // 2
    raw = rng.integers(-2000, 2000, size=(count, 2)).astype("<i2")
    src, dst = tmp_path / "in.raw", tmp_path / "out.cf32"
    raw.tofile(src)
    subprocess.run([str(c_pfb), "--input", str(src), "--output", str(dst), "--rate", "25600000",
                    "--bins", str(n_bins), "--taps", str(taps), "--samples", str(count)],
                   check=True, capture_output=True)
    c_out = np.fromfile(dst, "<c8").reshape(-1, n_bins)
    x = raw[:, 0].astype(float) + 1j * raw[:, 1]
    model = pfb_float(x, c_reference_prototype(n_bins, taps), DEFAULT)
    assert c_out.shape == model.shape
    np.testing.assert_allclose(c_out, model, rtol=1e-4, atol=2e-3 * np.abs(model).max())


def test_fixed_gain_and_headroom():
    cfg = DEFAULT
    hq = prototype_fixed(cfg)
    assert fixed_gain(cfg) == pytest.approx(2.0 ** 11, rel=1e-4)
    worst_u = np.abs(hq.reshape(cfg.taps, cfg.n)).sum(axis=0).max() * 2048 / 2 ** cfg.fir_shift
    assert worst_u < 2 ** (cfg.fft_in_bits - 1)
    worst_y = np.abs(hq).sum() * 2048 * np.sqrt(2) / 2 ** cfg.fir_shift
    assert worst_y < 2 ** (cfg.fft_out_bits - 2)


def test_fir_rounding_is_round_half_up():
    assert list(round_shift(np.array([-3, -2, -1, 1, 2, 3]), 1)) == [-1, -1, 0, 1, 1, 2]
    cfg = Config()
    re = np.zeros(cfg.length, np.int64)
    re[cfg.length // 2] = 1
    ur, _ = fir_fixed(re, re * 0, prototype_fixed(cfg), cfg)
    assert ur.shape == (1, cfg.n)


@pytest.mark.parametrize("e", range(0, 48))
def test_log2_q8_boundaries(e):
    for v in {max(1, (1 << e) - 1), 1 << e, (1 << e) + 1}:
        got = int(act.log2_q8(np.array([v]))[0])
        top = v.bit_length() - 1
        mant = ((v << 8) >> top) & 0xFF
        assert got == (top << 8) + int(act.MANTISSA_TABLE[mant])
        assert abs(got / 256 - np.log2(v)) < 1 / 256 + 1e-9
    assert int(act.log2_q8(np.array([0]))[0]) == 0


def test_detector_hysteresis_and_hang():
    det = act.DetectorConfig(block_log2=0, sub_blocks=4, sub_windows=4, bias_q8=0,
                             on_q8=act.db_to_q8(3.0), off_q8=act.db_to_q8(1.0), hang=2,
                             psd_log2=4)
    st = act.initial_state(1, det)
    seq = [100] * 20 + [400] * 3 + [150] * 1 + [110] * 5
    states = []
    for v in seq:
        blk, _ = act.step(st, np.array([v]), det)
        states.append(bool(blk.active[0]))
    assert not any(states[:20])                  # flat noise: never opens
    assert states[20] and states[21] and states[22]   # +6 dB: opens
    assert states[23]                            # +1.76 dB: above off, stays open
    assert states[24] and states[25]             # below off: hang keeps it open 2 blocks
    assert not states[26]                        # then closes


def test_false_alarm_math_consistent():
    nu, draws = 500.0, 640
    t = act.threshold_min_floor(1e-8, nu, draws)
    assert act.pfa_min_floor(t, nu, draws) == pytest.approx(1e-8, rel=0.05)
    assert act.threshold_for_pfa(1e-6, nu) > 0
    assert act.detection(6.0, 1.0, nu) > 0.999


def test_fit_roundtrip():
    root = fit.Node("", {"description": fit.cstr("test"), "#address-cells": fit.u32(1)})
    img = root.add(fit.Node("images"))
    f = img.add(fit.Node("fpga@1", {"data": bytes(range(7)), "type": fit.cstr("fpga")}))
    f.add(fit.Node("hash@1", {"algo": fit.cstr("md5"), "value": b"\0" * 16}))
    meta = {"version": 17, "last_comp": 16, "boot_cpu": 0, "rsv": []}
    blob = fit.serialize(root, meta)
    back, meta2 = fit.parse(blob)
    assert fit.serialize(back, meta2) == blob
    assert back.find("images/fpga@1").props["data"] == bytes(range(7))
