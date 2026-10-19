import json

import numpy as np
import pytest
from scipy.io import wavfile
from typer.testing import CliRunner

from radio_tools.analysis import inspect, load_audio
from radio_tools.cli import app

runner = CliRunner()


def test_pcm_levels_and_clipping(tmp_path):
    path = tmp_path / "audio_454216000Hz_test.wav"
    wavfile.write(path, 8000, np.array([0, 32767, -32768, 0], dtype=np.int16))
    result = inspect(path)
    assert result.recording_frequency_hz == 454216000
    assert result.near_full_scale_fraction == 0.5
    assert result.exact_silence_fraction == 0.5
    assert result.peak_dbfs == 0
    assert result.duration_s == 0.0005


def test_unsigned_pcm_silence_and_json(tmp_path):
    path = tmp_path / "silent.wav"
    wavfile.write(path, 8000, np.full(8000, 128, dtype=np.uint8))
    result = runner.invoke(app, ["inspect", str(path), "--json"])
    assert result.exit_code == 0, result.output
    data = json.loads(result.stdout)
    assert data["rms_dbfs"] == -300
    assert data["exact_silence_fraction"] == 1
    assert data["repeats"] == []


def test_known_tone_frequency(tmp_path):
    path = tmp_path / "tone.wav"
    rate = 48000
    x = 0.5 * np.sin(2 * np.pi * 1200 * np.arange(rate) / rate)
    wavfile.write(path, rate, x.astype(np.float32))
    result = inspect(path)
    assert result.spectral_peaks[0].frequency_hz == pytest.approx(1200, abs=1)
    assert result.rms_dbfs == pytest.approx(-9.03, abs=0.02)


def test_random_frame_period_and_duration_limit(tmp_path):
    path = tmp_path / "frames.wav"
    rate = 48000
    frame = np.random.default_rng(42).normal(0, 0.1, 2880).astype(np.float32)
    wavfile.write(path, rate, np.tile(frame, 30))
    result = inspect(path, seconds=1)
    assert result.analyzed_s == 1
    assert result.repeats[0].period_ms == pytest.approx(60)
    assert result.repeats[0].correlation > 0.9


def test_stereo_requires_channel_and_baseband_rejected(tmp_path):
    path = tmp_path / "stereo.wav"
    wavfile.write(path, 8000, np.zeros((10, 2), dtype=np.int16))
    with pytest.raises(ValueError, match="Multichannel"):
        load_audio(path)
    assert len(load_audio(path, channel=1)[1]) == 10
    path = tmp_path / "baseband_123.wav"
    wavfile.write(path, 8000, np.zeros(10, dtype=np.int16))
    with pytest.raises(ValueError, match="baseband"):
        load_audio(path)


def test_stdin_json_and_invalid_file(tmp_path):
    path = tmp_path / "tone.wav"
    wavfile.write(path, 8000, np.zeros(8000, dtype=np.int16))
    result = runner.invoke(app, ["inspect", "-", "--json"], input=path.read_bytes())
    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout)["path"] == "-"
    result = runner.invoke(app, ["inspect", str(tmp_path / "missing.wav"), "--json"])
    assert result.exit_code == 1
    assert result.stdout == ""
    assert "Error:" in result.stderr


def test_plot_write_guards(tmp_path):
    path = tmp_path / "tone.wav"
    wavfile.write(path, 8000, np.zeros(8000, dtype=np.int16))
    output = tmp_path / "plot.png"
    result = runner.invoke(app, ["plot", str(path), "-o", str(output)])
    assert result.exit_code == 2
    assert not output.exists()
    result = runner.invoke(app, ["plot", str(path), "-o", str(output), "--yes"])
    assert result.exit_code == 0, result.output
    assert output.read_bytes().startswith(b"\x89PNG")
    result = runner.invoke(app, ["plot", str(path), "-o", str(output), "--yes"])
    assert result.exit_code == 1


def test_saved_mode_is_not_capture_mode(tmp_path):
    (tmp_path / "radio_config.json").write_text('{"Radio":{"selectedDemodId":2}}')
    result = runner.invoke(app, ["doctor", "--directory", str(tmp_path), "--json"])
    assert result.exit_code == 0
    data = json.loads(result.stdout)
    assert data["saved_mode"] == "AM"
    assert "may differ" in data["notes"][0]


def test_paging_with_fake_decoder(tmp_path):
    path = tmp_path / "audio.wav"
    wavfile.write(path, 48000, np.zeros(48000, dtype=np.int16))
    decoder = tmp_path / "decoder"
    decoder.write_text(
        '#!/usr/bin/env python3\nimport sys\ndata=sys.stdin.buffer.read()\nassert len(data)==44100\nprint("Enabled demodulators: POCSAG512")\n'
    )
    decoder.chmod(0o755)
    result = runner.invoke(app, ["paging", str(path), "--decoder", str(decoder), "--json"])
    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout)["status"] == "no_decode"
