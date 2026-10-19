import json

import numpy as np
import pytest
from scipy.io import wavfile
from scipy.signal import welch
from typer.testing import CliRunner

from radio_tools.cli import app
from radio_tools.digital import iq_fm

runner = CliRunner()


@pytest.fixture
def iq(tmp_path):
    rate = 250000
    t = np.arange(rate // 2) / rate
    # FM at -60 kHz offset, 1200 Hz audio, 2500 Hz peak deviation.
    phase = 2 * np.pi * -60000 * t + 2500 / 1200 * np.sin(2 * np.pi * 1200 * t)
    z = 12000 * np.exp(1j * phase)
    path = tmp_path / "baseband.wav"
    wavfile.write(path, rate, np.column_stack([z.real, z.imag]).astype(np.int16))
    return path


def test_iq_offset_demodulation_and_bound(iq):
    rate, duration, pcm = iq_fm(iq, 0.25, -60000)
    assert rate == 250000
    assert duration == 0.25
    assert abs(len(pcm) - 12000) < 3
    frequencies, power = welch(pcm[1000:].astype(float), 48000, nperseg=8192)
    assert frequencies[np.argmax(power)] == pytest.approx(1200, abs=6)
    assert np.std(pcm[1000:]) == pytest.approx(8000 / np.sqrt(2), rel=0.08)


@pytest.mark.parametrize(
    "offset,seconds,bandwidth",
    [(124000, 1, 12500), (0, 61, 12500), (0, 0, 12500), (float("nan"), 1, 12500), (0, 1, 50000)],
)
def test_iq_reject_bounds(iq, offset, seconds, bandwidth):
    with pytest.raises(ValueError):
        iq_fm(iq, seconds, offset, bandwidth)


@pytest.mark.parametrize("shape,rate", [((1000,), 250000), ((1000, 2), 48000)])
def test_iq_reject_audio(tmp_path, shape, rate):
    path = tmp_path / "audio.wav"
    wavfile.write(path, rate, np.zeros(shape, dtype=np.int16))
    with pytest.raises(ValueError, match="I/Q"):
        iq_fm(path, 1, 0)


def test_iq_cli_output_guards_and_frequency_pair(iq, tmp_path):
    output = tmp_path / "fm.wav"
    args = [
        "iq-fm",
        str(iq),
        "--center-hz",
        "454217000",
        "--target-hz",
        "454157000",
        "-o",
        str(output),
        "--seconds",
        "0.25",
        "--json",
    ]
    result = runner.invoke(app, args)
    assert result.exit_code == 2
    assert not output.exists()
    result = runner.invoke(app, args + ["--yes"])
    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout)["offset_hz"] == -60000
    assert wavfile.read(output)[0] == 48000
    original = output.read_bytes()
    result = runner.invoke(app, args + ["--yes"])
    assert result.exit_code == 1
    assert output.read_bytes() == original


def test_dmr_fake_decoder_and_empty_output(tmp_path):
    path = tmp_path / "audio.wav"
    wavfile.write(path, 48000, np.zeros(48000, dtype=np.int16))
    decoder = tmp_path / "dsdccx"
    decoder.write_text(
        '#!/usr/bin/env python3\nimport sys\nassert sys.argv[1:]==["-i","-","-o","-","-fr","-T","2","-q"]\nassert len(sys.stdin.buffer.read())==96000\nsys.stderr.write("diagnostic")\nsys.stdout.buffer.write(b"\\x01\\x00"*800)\n'
    )
    decoder.chmod(0o755)
    output = tmp_path / "decoded.wav"
    args = [
        "dmr",
        str(path),
        "--slot",
        "2",
        "--decoder",
        str(decoder),
        "-o",
        str(output),
        "--yes",
        "--json",
    ]
    result = runner.invoke(app, args)
    assert result.exit_code == 0, result.output
    data = json.loads(result.stdout)
    assert data["status"] == "audio_emitted"
    assert data["output_duration_s"] == 0.1
    assert data["diagnostics"] == "diagnostic"
    assert wavfile.read(output)[0] == 8000
    output.unlink()
    decoder.write_text("#!/bin/sh\ncat >/dev/null\n")
    result = runner.invoke(app, args)
    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout)["status"] == "no_audio"


def test_dmr_reject_wrong_rate_and_decoder_failure(tmp_path):
    path = tmp_path / "audio.wav"
    wavfile.write(path, 8000, np.zeros(8000, dtype=np.int16))
    decoder = tmp_path / "dsdccx"
    decoder.write_text("#!/bin/sh\ncat >/dev/null\nexit 7\n")
    decoder.chmod(0o755)
    output = tmp_path / "decoded.wav"
    args = ["dmr", str(path), "--decoder", str(decoder), "-o", str(output), "--yes", "--json"]
    result = runner.invoke(app, args)
    assert result.exit_code == 1
    assert "48000 Hz" in result.stderr
    assert not output.exists()
    wavfile.write(path, 48000, np.zeros(48000, dtype=np.int16))
    result = runner.invoke(app, args)
    assert result.exit_code == 1
    assert "exited 7" in result.stderr
    assert not output.exists()
