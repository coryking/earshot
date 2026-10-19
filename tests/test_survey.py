import json

import numpy as np
import pytest
from scipy.io import wavfile
from typer.testing import CliRunner

from radio_tools.cli import app
from radio_tools.survey import survey_iq

runner = CliRunner()


def write_iq(path, z, rate=250000):
    wavfile.write(
        path,
        rate,
        np.column_stack([z.real, z.imag]).__mul__(32768).clip(-32768, 32767).astype(np.int16),
    )


def test_psd_full_scale_calibration_and_carrier(tmp_path):
    path = tmp_path / "baseband_454217000Hz_test.wav"
    rate = 250000
    t = np.arange(rate * 2) / rate
    random = np.random.default_rng(7)
    noise = 0.003 * (random.normal(size=len(t)) + 1j * random.normal(size=len(t)))
    write_iq(path, 0.25 * np.exp(2j * np.pi * 30000 * t) + noise)
    data = survey_iq(path, seconds=2, fft_size=4096, windows=80)
    assert data.result.sampled_power_dbfs == pytest.approx(20 * np.log10(0.25), abs=0.01)
    assert data.result.center_frequency_hz == 454217000
    assert data.result.windows == 80
    assert data.result.sampled_duration_s < data.result.surveyed_span_s
    carrier = min(data.result.candidates, key=lambda c: abs(c.offset_hz - 30000))
    assert carrier.kind == "narrow_carrier"
    assert carrier.offset_hz == pytest.approx(30000, abs=100)
    assert carrier.frequency_hz == pytest.approx(454247000, abs=100)


def test_noise_does_not_become_occupied_and_silence_is_finite(tmp_path):
    path = tmp_path / "noise.wav"
    random = np.random.default_rng(42)
    z = random.normal(0, 0.02, 500000) + 1j * random.normal(0, 0.02, 500000)
    write_iq(path, z)
    result = survey_iq(path, fft_size=4096, windows=100).result
    assert result.candidates == []
    assert result.center_frequency_hz is None
    write_iq(path, np.zeros(500000, dtype=complex))
    result = survey_iq(path, fft_size=4096, windows=10).result
    assert result.candidates == []
    assert result.sampled_power_dbfs == -300


def test_broad_hump_is_distinct_from_narrow_carrier(tmp_path):
    path = tmp_path / "hump.wav"
    random = np.random.default_rng(8)
    rate, count = 250000, 500000
    noise = random.normal(size=count) + 1j * random.normal(size=count)
    spectrum = np.fft.fft(noise)
    frequencies = np.fft.fftfreq(count, 1 / rate)
    spectrum[(frequencies < -95000) | (frequencies > -25000)] = 0
    z = 0.1 * np.fft.ifft(spectrum) + 0.002 * noise
    write_iq(path, z)
    result = survey_iq(path, fft_size=4096, windows=100).result
    broad = [c for c in result.candidates if c.kind == "broad_hump_or_passband_shape"]
    assert broad
    assert broad[0].width_hz > 60000


def test_burst_occupancy_and_dc_edge_flags(tmp_path):
    path = tmp_path / "signals.wav"
    rate = 250000
    t = np.arange(rate * 2) / rate
    random = np.random.default_rng(12)
    z = 0.003 * (random.normal(size=len(t)) + 1j * random.normal(size=len(t)))
    z += 0.1 + 0.1 * np.exp(2j * np.pi * 120000 * t)
    z += 0.2 * (t < 1) * np.exp(2j * np.pi * -40000 * t)
    write_iq(path, z)
    result = survey_iq(path, seconds=2, fft_size=4096, windows=100).result
    burst = min(result.candidates, key=lambda c: abs(c.offset_hz + 40000))
    assert burst.offset_hz == pytest.approx(-40000, abs=100)
    assert 0.4 < burst.sampled_occupancy < 0.6
    assert any("dc_or_center_artifact_possible" in c.flags for c in result.candidates)
    assert any("edge_artifact_possible" in c.flags for c in result.candidates)


def test_survey_cli_json_limit_and_plot_guard(tmp_path):
    path = tmp_path / "iq.wav"
    t = np.arange(250000) / 250000
    write_iq(path, 0.1 + 0.1 * np.exp(2j * np.pi * 40000 * t))
    args = [
        "iq-survey",
        str(path),
        "--fft-size",
        "4096",
        "--windows",
        "8",
        "--limit",
        "1",
        "--json",
    ]
    result = runner.invoke(app, args)
    assert result.exit_code == 0, result.output
    assert len(json.loads(result.stdout)["candidates"]) == 1
    output = tmp_path / "survey.png"
    result = runner.invoke(app, args + ["-o", str(output)])
    assert result.exit_code == 2
    assert not output.exists()
    result = runner.invoke(app, args + ["-o", str(output), "--yes"])
    assert result.exit_code == 0, result.output
    assert output.read_bytes().startswith(b"\x89PNG")
    result = runner.invoke(app, args + ["-o", str(output), "--yes"])
    assert result.exit_code == 1
    assert result.stdout == ""


@pytest.mark.parametrize(
    "kwargs",
    [
        {"fft_size": 3000},
        {"seconds": 121},
        {"windows": 2},
        {"limit": 0},
        {"center_hz": float("nan")},
    ],
)
def test_bad_survey_limits(tmp_path, kwargs):
    with pytest.raises(ValueError):
        survey_iq(tmp_path / "unused.wav", **kwargs)


def test_survey_rejects_audio(tmp_path):
    path = tmp_path / "audio.wav"
    wavfile.write(path, 48000, np.zeros(48000, dtype=np.int16))
    with pytest.raises(ValueError, match="I/Q"):
        survey_iq(path)
