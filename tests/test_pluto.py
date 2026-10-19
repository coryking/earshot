import json
import wave

import pytest
from typer.testing import CliRunner

from radio_tools.pluto import (
    CaptureConfig,
    DataFormat,
    PlutoReceiver,
    app,
    capture_iq,
    plan_capture,
)


class FakeReceiver:
    def __init__(self, fail=None, actual_rate=None):
        self.values = {
            "sampling_frequency": "5000000",
            "rf_bandwidth": "5000000",
            "frequency": "454216998",
            "gain_control_mode": "fast_attack",
        }
        self.saved = self.values.copy()
        self.calls = []
        self.fail = fail
        self.actual_rate = actual_rate
        self.refills = 0
        self.chunk_frames = 1024

    def open(self):
        self.calls.append("open")

    def read(self, key):
        return self.values[key]

    def write(self, key, value):
        self.calls.append((key, value))
        self.values[key] = value
        if self.fail == "configure" and value == "6000000":
            raise OSError("write failed after mutation")
        if self.fail == "restore" and key == "rf_bandwidth" and value == self.saved[key]:
            raise OSError("restore bandwidth failed")
        if key == "sampling_frequency" and value == "6000000" and self.actual_rate:
            self.values[key] = str(self.actual_rate)

    def start(self, samples):
        self.calls.append(("start", samples))
        if self.fail == "start":
            raise ValueError("format assertion")
        return 12

    def refill(self):
        self.refills += 1
        if self.fail == "refill":
            raise OSError("refill failed")
        if self.fail == "alignment":
            return b"abc"
        return b"\x01\x00\xfe\xff" * self.chunk_frames

    def stop(self):
        self.calls.append("stop")

    def close(self):
        self.calls.append("close")


@pytest.fixture
def config(tmp_path):
    return CaptureConfig(
        rate=6_000_000,
        center=455_000_000,
        seconds=0.0002,
        gain_mode="slow_attack",
        output=tmp_path / "iq.wav",
    )


def perform(config, receiver, clock=lambda: 1):
    return capture_iq(config, receiver_factory=lambda *args: receiver, clock=clock)


def test_capture_readbacks_pcm_format_trims_and_restores(config):
    receiver = FakeReceiver(actual_rate=5_500_000)
    ticks = iter([1, 1, 1.1, 1.2])
    result = perform(config, receiver, clock=lambda: next(ticks))
    assert result.applied.sampling_frequency == 5_500_000
    assert result.frames == 1100
    assert result.data_bytes == 4400
    assert result.sample_duration_seconds == 0.0002
    assert result.wall_seconds == pytest.approx(0.2)
    assert result.effective_samples_per_second == pytest.approx(5500)
    assert result.throughput_limited
    assert result.dropped_samples is None
    assert not result.continuity_verified
    assert result.mode == "streaming"
    assert receiver.values == receiver.saved
    assert receiver.calls[-1] == "close"
    with wave.open(str(config.output)) as wav:
        assert wav.getparams()[:4] == (2, 2, 5_500_000, 1100)
        assert wav.readframes(1) == b"\x01\x00\xfe\xff"
    assert json.loads(config.output.with_suffix(".wav.json").read_text()) == result.model_dump()


@pytest.mark.parametrize("failure", ["configure", "start", "refill", "alignment"])
def test_failures_restore_controls_close_and_remove_partial_files(config, failure):
    receiver = FakeReceiver(fail=failure)
    with pytest.raises((OSError, ValueError)):
        perform(config, receiver)
    assert receiver.values == receiver.saved
    assert receiver.calls[-1] == "close"
    assert "stop" in receiver.calls
    assert not config.output.exists()
    assert not config.output.with_suffix(".wav.json").exists()


def test_restore_failure_still_restores_other_controls_and_closes(config):
    receiver = FakeReceiver(fail="restore")
    with pytest.raises(OSError, match="restore bandwidth"):
        perform(config, receiver)
    assert receiver.values == receiver.saved
    assert receiver.calls[-1] == "close"
    assert not config.output.exists()


def test_refill_and_restore_failure_are_both_reported(config):
    receiver = FakeReceiver(fail="refill")
    original_write = receiver.write

    def write(key, value):
        original_write(key, value)
        if key == "rf_bandwidth" and value == receiver.saved[key]:
            raise OSError("restore failed")

    receiver.write = write
    with pytest.raises(ExceptionGroup) as exc:
        perform(config, receiver)
    assert "refill failed" in str(exc.value.exceptions[0])
    assert "restore failed" in str(exc.value.exceptions[1])
    assert receiver.calls[-1] == "close"


@pytest.mark.parametrize("name", ["iq.wav", "iq.wav.json"])
def test_existing_file_refused_before_hardware(config, name):
    occupied = config.output.parent / name
    occupied.write_text("original")
    receiver = FakeReceiver()
    with pytest.raises(FileExistsError):
        perform(config, receiver)
    assert receiver.calls == []
    assert occupied.read_text() == "original"


def test_size_disk_and_single_buffer_caps(config, monkeypatch):
    with pytest.raises(ValueError, match="4 GiB"):
        plan_capture(config.model_copy(update={"seconds": 300, "rate": 61_440_000}))
    with pytest.raises(ValueError, match="128 MiB"):
        plan_capture(config.model_copy(update={"seconds": 30, "single_buffer": True}))
    monkeypatch.setattr(
        "radio_tools.pluto.shutil.disk_usage", lambda p: type("Disk", (), {"free": 0})()
    )
    with pytest.raises(ValueError, match="disk space"):
        plan_capture(config)


def test_single_buffer_uses_requested_sample_count(config):
    receiver = FakeReceiver()
    receiver.chunk_frames = 1200
    result = perform(config.model_copy(update={"single_buffer": True}), receiver)
    assert ("start", 1200) in receiver.calls
    assert result.mode == "single_buffer"
    assert receiver.refills == 1
    assert result.throughput_limited is None
    assert not result.continuity_verified


def test_wall_time_bounded_and_cleanup(config):
    receiver = FakeReceiver()
    ticks = iter([0, 20])
    with pytest.raises(TimeoutError):
        perform(config, receiver, clock=lambda: next(ticks))
    assert receiver.values == receiver.saved
    assert receiver.calls[-1] == "close"
    assert not config.output.exists()


def test_cli_dry_run_and_requires_yes(config, monkeypatch):
    def forbidden(*args):
        raise AssertionError("dry-run must not touch hardware")

    monkeypatch.setattr("radio_tools.pluto.capture_iq", forbidden)
    args = ["capture", "--center", str(config.center), "--output", str(config.output)]
    result = CliRunner().invoke(app, args + ["--dry-run", "--json"])
    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout)["status"] == "dry_run"
    assert result.stderr == ""
    assert not config.output.exists()
    result = CliRunner().invoke(app, args)
    assert result.exit_code == 2
    assert "--yes" in result.stderr
    assert result.stdout == ""


def test_backend_context_cleanup_when_open_fails():
    # Exercise context ownership without loading any native library or opening hardware.
    receiver = object.__new__(PlutoReceiver)
    receiver.uri = "mock:"
    receiver.context = receiver.buffer = None
    receiver.enabled = []
    receiver.create_context_from_uri = lambda uri: 123
    receiver.context_set_timeout = lambda *args: 0
    receiver.context_find_device = lambda *args: None
    destroyed = []
    receiver.context_destroy = destroyed.append
    with pytest.raises(OSError, match="RX phy"):
        receiver.open()
    assert destroyed == [123]
    assert receiver.context is None


@pytest.mark.parametrize("bad", ["shift", "is_be", "length", "is_signed", "repeat", "index"])
def test_backend_asserts_adc_format_before_enabling(bad):
    receiver = object.__new__(PlutoReceiver)
    receiver.channels = [1, 2]
    fmt = DataFormat(length=16, bits=12, shift=0, is_signed=True, repeat=1)
    if bad != "index":
        setattr(
            fmt,
            bad,
            {"shift": 4, "is_be": True, "length": 32, "is_signed": False, "repeat": 2}[bad],
        )
    receiver.channel_get_data_format = lambda channel: type("Pointer", (), {"contents": fmt})()
    receiver.channel_get_index = lambda channel: -1 if bad == "index" else channel - 1
    with pytest.raises(ValueError, match="format"):
        receiver.start(1024)


def test_backend_buffer_destroy_disable_and_context_close():
    receiver = object.__new__(PlutoReceiver)
    receiver.buffer, receiver.context = 100, 200
    receiver.enabled = [1, 2]
    calls = []
    receiver.buffer_destroy = lambda value: calls.append(("buffer", value))
    receiver.channel_disable = lambda value: calls.append(("disable", value))
    receiver.context_destroy = lambda value: calls.append(("context", value))
    receiver.close()
    receiver.close()
    assert calls == [("buffer", 100), ("disable", 1), ("disable", 2), ("context", 200)]


def test_interrupt_restores_and_removes_partial_capture(config):
    receiver = FakeReceiver()

    def interrupted():
        raise KeyboardInterrupt()

    receiver.refill = interrupted
    with pytest.raises(KeyboardInterrupt):
        perform(config, receiver)
    assert receiver.values == receiver.saved
    assert receiver.calls[-1] == "close"
    assert not config.output.exists()


def test_single_buffer_refuses_short_refill(config):
    receiver = FakeReceiver()
    with pytest.raises(ValueError, match="Single RX buffer"):
        perform(config.model_copy(update={"single_buffer": True}), receiver)
    assert receiver.refills == 1
    assert receiver.values == receiver.saved
    assert not config.output.exists()


def test_storage_failure_restores_and_removes_partial_capture(config, monkeypatch):
    receiver = FakeReceiver()

    def fail_write(*args):
        raise OSError("disk write failed")

    monkeypatch.setattr(wave.Wave_write, "writeframesraw", fail_write)
    with pytest.raises(OSError, match="disk write failed"):
        perform(config, receiver)
    assert receiver.values == receiver.saved
    assert receiver.calls[-1] == "close"
    assert not config.output.exists()
    assert not config.output.with_suffix(".wav.json").exists()
