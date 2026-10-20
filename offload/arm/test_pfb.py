"""Independent filter/phase checks for the exact C core built for Pluto."""

import json
import subprocess
from pathlib import Path

import numpy as np
import pytest
from scipy.signal import firwin, lfilter

ROOT = Path(__file__).resolve().parent


@pytest.fixture(scope="module")
def binary(tmp_path_factory):
    output = tmp_path_factory.mktemp("pfb") / "pfb"
    subprocess.run(
        [
            "cc",
            "-O2",
            "-fsanitize=address,undefined",
            "-Wall",
            "-Wextra",
            "-Werror",
            str(ROOT / "pfb.c"),
            str(ROOT / "prototype.c"),
            str(ROOT / "gate.c"),
            "-lm",
            "-o",
            str(output),
        ],
        check=True,
    )
    return output


@pytest.mark.parametrize("channel,residual", [(8, 700), (9, -700), (-17, 400)])
def test_complex_channels_match_independent_mixer_and_fir(binary, tmp_path, channel, residual):
    rate, bins, taps, count = 256000, 256, 8, 32768
    t = np.arange(count)
    frequency = channel * rate / bins + residual
    signal = 800 * np.exp(2j * np.pi * frequency * t / rate)
    # A second strong carrier tests rejection as well as phase preservation.
    signal += 1100 * np.exp(2j * np.pi * (channel + 4) * t / bins)
    raw = np.column_stack([np.rint(signal.real), np.rint(signal.imag)]).astype("<i2")
    source, output = tmp_path / "in.raw", tmp_path / "out.cf32"
    raw.tofile(source)
    result = subprocess.run(
        [
            str(binary),
            "--input",
            str(source),
            "--output",
            str(output),
            "--rate",
            str(rate),
            "--bins",
            str(bins),
            "--taps",
            str(taps),
            "--samples",
            str(count),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    stats = json.loads(result.stdout)
    actual = np.fromfile(output, "<c8").reshape(-1, bins)[:, channel % bins]
    coeff = firwin(bins * taps, 0.9 / bins, window="blackman")
    mixed = (raw[:, 0].astype(float) + 1j * raw[:, 1]) * np.exp(-2j * np.pi * channel * t / bins)
    expected = lfilter(coeff, [1.0], mixed)[bins * taps - 1 :: bins // 2]
    np.testing.assert_allclose(actual, expected, atol=0.02, rtol=1e-4)
    assert len(actual) == stats["output_frames"]
    assert stats["output_channel_rate_hz"] == 2000
    assert stats["radio_opened"] is False


def test_refuses_output_overwrite(binary, tmp_path):
    target = tmp_path / "existing"
    target.write_bytes(b"preserved")
    result = subprocess.run(
        [str(binary), "--benchmark", "--output", str(target)],
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 1
    assert target.read_bytes() == b"preserved"
