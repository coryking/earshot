import json

import numpy as np
import pytest
from scipy.io import wavfile
from typer.testing import CliRunner

from radio_tools.analysis import inspect, load_audio
from radio_tools.cli import app

runner = CliRunner()


@pytest.fixture
def audio(tmp_path):
    path = tmp_path / "audio.wav"
    wavfile.write(path, 48000, np.full(48000, 0.25, dtype=np.float32))
    return path


@pytest.mark.parametrize("seconds", [0, -1, float("nan"), float("inf")])
def test_invalid_analysis_duration(audio, seconds):
    with pytest.raises(ValueError, match="duration"):
        load_audio(audio, seconds)


def test_multichannel_does_not_cancel_or_mix(tmp_path):
    path = tmp_path / "stereo.wav"
    wavfile.write(path, 8000, np.tile(np.array([0.5, -0.5], dtype=np.float32), (8000, 1)))
    assert inspect(path, channel=0).dc_offset == 0.5
    assert inspect(path, channel=1).dc_offset == -0.5
    with pytest.raises(ValueError, match="between"):
        inspect(path, channel=2)


def test_float_outside_full_scale_and_nonfinite(tmp_path):
    path = tmp_path / "float.wav"
    wavfile.write(path, 8000, np.full(100, 1.5, dtype=np.float32))
    result = inspect(path)
    assert result.near_full_scale_fraction == 1
    assert result.peak_dbfs > 0
    wavfile.write(path, 8000, np.array([np.nan], dtype=np.float32))
    with pytest.raises(ValueError, match="non-finite"):
        inspect(path)


def test_recordings_bounded_fields_and_bad_directory(audio):
    result = runner.invoke(
        app,
        [
            "recordings",
            "--directory",
            str(audio.parent),
            "--limit",
            "1",
            "--fields",
            "path",
            "--json",
        ],
    )
    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout) == [{"path": str(audio)}]
    result = runner.invoke(app, ["recordings", "--fields", "unknown", "--json"])
    assert result.exit_code == 2
    assert result.stdout == ""
    result = runner.invoke(
        app, ["recordings", "--directory", str(audio.parent / "absent"), "--json"]
    )
    assert result.exit_code == 1
    assert result.stdout == ""


def test_plot_preview_json_has_no_writes(audio):
    output = audio.parent / "missing" / "test.png"
    result = runner.invoke(app, ["plot", str(audio), "-o", str(output), "--dry-run", "--json"])
    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout)["status"] == "dry_run"
    assert not output.parent.exists()


@pytest.mark.parametrize("content", ["[]", '{"Radio":[]}', '{"Radio":{"selectedDemodId":[]}}'])
def test_doctor_malformed_structure_is_clean_error(tmp_path, content):
    (tmp_path / "radio_config.json").write_text(content)
    result = runner.invoke(app, ["doctor", "--directory", str(tmp_path), "--json"])
    assert result.exit_code == 1
    assert result.stdout == ""
    assert "Error:" in result.stderr


def test_paging_failure_and_success(audio, tmp_path):
    decoder = tmp_path / "decoder"
    decoder.write_text('#!/bin/sh\ncat >/dev/null\necho "bad input" >&2\nexit 7\n')
    decoder.chmod(0o755)
    result = runner.invoke(app, ["paging", str(audio), "--decoder", str(decoder), "--json"])
    assert result.exit_code == 1
    assert result.stdout == ""
    assert "exited 7" in result.stderr
    decoder.write_text('#!/bin/sh\ncat >/dev/null\necho "POCSAG1200: test fixture"\n')
    result = runner.invoke(app, ["paging", str(audio), "--decoder", str(decoder), "--json"])
    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout)["status"] == "decoder_output"
    result = runner.invoke(
        app, ["paging", str(audio), "--decoder", str(tmp_path / "absent"), "--json"]
    )
    assert result.exit_code == 1
    assert "not found" in result.stderr


def test_json_error_for_nonwav(tmp_path):
    path = tmp_path / "bad.wav"
    path.write_text("not audio")
    result = runner.invoke(app, ["inspect", str(path), "--json"])
    assert result.exit_code == 1
    assert result.stdout == ""
    assert "Error:" in result.stderr
