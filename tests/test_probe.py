from __future__ import annotations

import json
import os
import stat
from pathlib import Path

import numpy as np
import pytest
from click.testing import CliRunner
from scipy.io import wavfile
from typer.main import get_command

from radio_tools import probe


def make_wav(path: Path, duration_s: float = 2, rate: int = 48000, channels: int = 1):
    samples = np.zeros((int(duration_s * rate), channels), dtype=np.int16)
    if channels == 1:
        samples = samples[:, 0]
    wavfile.write(path, rate, samples)
    return path


def make_decoder(path: Path, stdout: str = "", stderr: str = "", max_samples: int | None = None):
    sample_check = f"assert samples.size <= {max_samples}\n" if max_samples is not None else ""
    program = (
        "#!/usr/bin/env python3\n"
        "import json, os, sys\n"
        "from scipy.io import wavfile\n"
        "args = sys.argv[1:]\n"
        "assert args[args.index('-o') + 1] == 'null'\n"
        "assert args[0] == '-i'\n"
        "rate, samples = wavfile.read(args[1])\n"
        "assert rate == 48000 and samples.ndim == 1 and samples.dtype == 'int16'\n"
        + sample_check
        + f"sys.stdout.write({stdout!r})\n"
        + f"sys.stderr.write({stderr!r})\n"
    )
    path.write_text(program)
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return path


def test_probe_uses_offline_flags_caps_samples_and_keeps_streams_separate(tmp_path):
    audio = make_wav(tmp_path / "input.wav", duration_s=3)
    binary = make_decoder(
        tmp_path / "fake-dsd-fme",
        stdout="decoder stdout\n",
        stderr="Sync: +NXDN96\nCRC Error\n",
        max_samples=48000,
    )

    result = probe.run_digital_probe(str(audio), seconds=1, protocol="nxdn96", decoder=str(binary))

    assert result.status == "decoder_output"
    assert result.raw_sync_count == 1
    assert result.crc_failures == 1
    assert result.crc_checked_messages == {
        "site_id_message": 0,
        "service_information": 0,
        "idle_repeater_message": 0,
        "transmission_release": 0,
    }
    assert result.analyzed_s == 1
    assert result.stdout == "decoder stdout\n"
    assert result.stderr == "Sync: +NXDN96\nCRC Error\n"
    assert "diagnostic evidence only" in result.notes[-1]


@pytest.mark.parametrize(
    ("protocol", "flag"),
    [
        ("auto", "-fa"),
        ("dmr", "-fs"),
        ("nxdn48", "-fi"),
        ("nxdn96", "-fn"),
        ("dpmr", "-fm"),
        ("p25p1", "-f1"),
    ],
)
def test_each_protocol_maps_to_pinned_dsd_fme_flag(tmp_path, monkeypatch, protocol, flag):
    audio = make_wav(tmp_path / "input.wav")
    binary = make_decoder(tmp_path / "fake-dsd-fme")
    seen = {}

    def run(args, **kwargs):
        seen.update(args=args, kwargs=kwargs)
        return type("Completed", (), {"returncode": 0, "stdout": b"", "stderr": b""})()

    monkeypatch.setattr(probe.subprocess, "run", run)
    probe.run_digital_probe(str(audio), protocol=protocol, decoder=str(binary))

    assert flag in seen["args"]
    assert "-o" in seen["args"] and seen["args"][seen["args"].index("-o") + 1] == "null"
    assert seen["kwargs"]["timeout"] == 120
    assert seen["kwargs"]["cwd"] != str(tmp_path)
    assert seen["kwargs"]["env"]["HOME"] != os.environ.get("HOME")


def test_input_sampling_guard_rejects_wrong_rate_and_stereo_before_decoder(tmp_path):
    binary = make_decoder(tmp_path / "fake-dsd-fme")
    wrong_rate = make_wav(tmp_path / "wrong.wav", rate=44100)
    stereo = make_wav(tmp_path / "stereo.wav", channels=2)

    with pytest.raises(ValueError, match="48000 Hz"):
        probe.run_digital_probe(str(wrong_rate), decoder=str(binary))
    with pytest.raises(ValueError, match="mono WAV"):
        probe.run_digital_probe(str(stereo), decoder=str(binary))
    with pytest.raises(ValueError, match="at most 120"):
        probe.run_digital_probe(str(stereo), seconds=121, decoder=str(binary))


def test_stdin_wav_is_spooled_and_probed(tmp_path, monkeypatch):
    audio = make_wav(tmp_path / "stdin-source.wav", duration_s=0.5)
    binary = make_decoder(tmp_path / "fake-dsd-fme", stderr="Sync: +DMR\n")
    stream = audio.open("rb")
    monkeypatch.setattr(probe.sys, "stdin", type("Input", (), {"buffer": stream})())

    result = probe.run_digital_probe("-", decoder=str(binary))

    stream.close()
    assert result.input == "-"
    assert result.analyzed_s == 0.5
    assert result.raw_sync_count == 1


def test_stdout_and_stderr_are_bounded_and_truncation_is_reported(tmp_path):
    audio = make_wav(tmp_path / "input.wav")
    binary = make_decoder(tmp_path / "fake-dsd-fme", stdout="x" * 5000)

    result = probe.run_digital_probe(str(audio), decoder=str(binary))

    assert len(result.stdout) == probe.MAX_DIAGNOSTIC_CHARS
    assert result.diagnostic_truncated
    assert result.status == "no_decode"


def test_payload_labels_count_after_crc_gate_even_beyond_diagnostic_limit(tmp_path):
    audio = make_wav(tmp_path / "input.wav")
    payload = (
        "Site ID Message - Area: 0;\n"
        "Service Information - Location ID [008001]\n"
        "Idle Repeater Message - Area: 0;\n"
        "Transmission Release  - Src=0 - Dst/TG=0\n"
    )
    binary = make_decoder(tmp_path / "fake-dsd-fme", stderr="x" * 5000 + "\n" + payload)

    result = probe.run_digital_probe(str(audio), protocol="nxdn48", decoder=str(binary))

    assert result.raw_sync_count == 0
    assert result.status == "decoder_output"
    assert result.crc_checked_messages == {
        "site_id_message": 1,
        "service_information": 1,
        "idle_repeater_message": 1,
        "transmission_release": 1,
    }
    assert result.diagnostic_truncated


def test_raw_sync_and_crc_errors_do_not_count_crc_checked_payload(tmp_path):
    audio = make_wav(tmp_path / "input.wav")
    binary = make_decoder(
        tmp_path / "fake-dsd-fme",
        stderr="Sync: +NXDN48\n CRC ERR\nSite ID Message CRC Error\n",
    )

    result = probe.run_digital_probe(str(audio), protocol="nxdn48", decoder=str(binary))

    assert result.raw_sync_count == 1
    assert result.crc_failures == 2
    assert result.crc_checked_messages["site_id_message"] == 0


def test_float_wav_is_normalized_and_nonfinite_float_is_rejected(tmp_path):
    binary = make_decoder(tmp_path / "fake-dsd-fme")
    float_audio = tmp_path / "float.wav"
    wavfile.write(float_audio, 48000, np.asarray([0.0, 0.5, -1.0], dtype=np.float32))

    result = probe.run_digital_probe(str(float_audio), decoder=str(binary))
    assert result.analyzed_s == pytest.approx(3 / 48000)

    nonfinite = tmp_path / "nonfinite.wav"
    wavfile.write(nonfinite, 48000, np.asarray([0.0, np.nan], dtype=np.float32))
    with pytest.raises(ValueError, match="non-finite"):
        probe.run_digital_probe(str(nonfinite), decoder=str(binary))


def test_timeout_is_reported(tmp_path, monkeypatch):
    audio = make_wav(tmp_path / "input.wav")
    binary = make_decoder(tmp_path / "fake-dsd-fme")

    def run(*args, **kwargs):
        raise probe.subprocess.TimeoutExpired(args[0], kwargs["timeout"])

    monkeypatch.setattr(probe.subprocess, "run", run)
    with pytest.raises(ValueError, match="timeout"):
        probe.run_digital_probe(str(audio), decoder=str(binary))


def test_crc_err_abbreviation_from_real_dsd_fme_diagnostics(tmp_path):
    audio = make_wav(tmp_path / "input.wav")
    binary = make_decoder(tmp_path / "fake-dsd-fme", stderr="UDCH (CRC ERR)\nSLCO CRC ERR\n")
    result = probe.run_digital_probe(str(audio), decoder=str(binary))
    assert result.crc_failures == 2
    assert not any(result.crc_checked_messages.values())


def test_no_sync_diagnostic_is_not_a_sync_hit(tmp_path):
    audio = make_wav(tmp_path / "input.wav")
    binary = make_decoder(tmp_path / "fake-dsd-fme", stderr="Sync: no sync\nSync:   no sync\n")
    result = probe.run_digital_probe(str(audio), decoder=str(binary))
    assert result.raw_sync_count == 0
    assert result.status == "no_decode"


def test_cli_log_requires_yes_and_never_overwrites(tmp_path, monkeypatch):
    audio = make_wav(tmp_path / "input.wav")
    binary = make_decoder(tmp_path / "fake-dsd-fme", stdout="ok\n")
    monkeypatch.setenv("RADIO_DSD_FME", str(binary))
    command = get_command(probe.app)
    runner = CliRunner()
    log = tmp_path / "probe.log"

    refused = runner.invoke(command, [str(audio), "--log-output", str(log)])
    assert refused.exit_code != 0
    assert not log.exists()

    written = runner.invoke(command, [str(audio), "--log-output", str(log), "--yes", "--json"])
    assert written.exit_code == 0, written.output
    result = json.loads(written.output)
    assert result["status"] == "no_decode"
    assert "stdout:\nok" in log.read_text()
    original = log.read_bytes()

    refused_overwrite = runner.invoke(command, [str(audio), "--log-output", str(log), "--yes"])
    assert refused_overwrite.exit_code != 0
    assert log.read_bytes() == original


def test_requested_log_keeps_later_messages_beyond_json_preview(tmp_path):
    audio = make_wav(tmp_path / "input.wav")
    binary = make_decoder(
        tmp_path / "fake-dsd-fme", stderr="x" * 5000 + "\nSite ID Message - late\n"
    )
    command = get_command(probe.app)
    log = tmp_path / "full.log"
    result = CliRunner().invoke(
        command, [str(audio), "--decoder", str(binary), "--log-output", str(log), "--yes", "--json"]
    )
    assert result.exit_code == 0, result.output
    data = json.loads(result.output)
    assert data["diagnostic_truncated"]
    assert data["crc_checked_messages"]["site_id_message"] == 1
    assert "Site ID Message - late" in log.read_text()
    assert "Site ID Message - late" not in data["stderr"]
