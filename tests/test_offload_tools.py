"""Radio-safety helpers in offload/: the shared lock, readback/restore, and acceptance policy."""

import sys
import threading
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "offload"))

import live_acceptance
import pluto_readback
import radio_lock
import survey_wideband


@pytest.fixture
def lock_path(tmp_path, monkeypatch):
    monkeypatch.setattr(radio_lock, "LOCK_PATH", str(tmp_path / "pluto-radio.lock"))


def test_radio_lock_excludes_a_second_holder_until_released(lock_path):
    entered = threading.Event()
    release = threading.Event()

    def holder():
        with radio_lock.radio_lock():
            entered.set()
            release.wait(5)

    thread = threading.Thread(target=holder)
    thread.start()
    assert entered.wait(5)
    with pytest.raises(TimeoutError), radio_lock.radio_lock(timeout_s=0.3):
        pytest.fail("lock was granted while held")
    release.set()
    thread.join(5)
    with radio_lock.radio_lock(timeout_s=2):
        pass


def test_radio_lock_is_released_when_the_session_raises(lock_path):
    with pytest.raises(RuntimeError), radio_lock.radio_lock():
        raise RuntimeError("session failed")
    with radio_lock.radio_lock(timeout_s=1):
        pass


def test_termination_signals_become_system_exit(monkeypatch):
    import signal

    installed = {}
    monkeypatch.setattr(signal, "signal", lambda sig, handler: installed.update({sig: handler}))
    radio_lock.raise_on_termination()
    assert set(installed) == {signal.SIGTERM, signal.SIGHUP}
    with pytest.raises(SystemExit) as exit_info:
        installed[signal.SIGTERM](signal.SIGTERM, None)
    assert exit_info.value.code == 128 + signal.SIGTERM


ORIGINAL = {
    "rx_sampling_frequency": "30720000",
    "rx_rf_bandwidth": "18000000",
    "rx_gain_control_mode": "slow_attack",
    "rx_hardwaregain": "58.000000 dB",
    "rx_filter_fir_en": "0",
    "rx_lo": "2400000000",
    "tx_sampling_frequency": "30720000",
    "tx_filter_fir_en": "0",
    "tx_lo": "2450000000",
}


def test_mismatches_ignores_agc_gain_and_names_differences():
    after = {**ORIGINAL, "rx_hardwaregain": "41.000000 dB", "rx_lo": "454216998"}
    assert pluto_readback.mismatches(ORIGINAL, after) == {"rx_lo": ["2400000000", "454216998"]}


def test_restore_attempts_every_write_even_after_a_failure(monkeypatch):
    calls = []

    def fake_iio_attr(uri, *args):
        calls.append(args)
        if args[-2] == "sampling_frequency":
            raise OSError("device busy")
        return args[-1]

    monkeypatch.setattr(pluto_readback, "iio_attr", fake_iio_attr)
    errors = pluto_readback.restore_receiver("ip:x", ORIGINAL)
    written = [c[-2] for c in calls]
    assert written == [
        "sampling_frequency",
        "rf_bandwidth",
        "frequency",
        "gain_control_mode",
        "filter_fir_en",
    ]
    assert len(errors) == 1 and errors[0].startswith("sampling_frequency")
    # Receive controls only: no TX attribute is ever written.
    assert all(c[0] != "-o" or c[3] == "altvoltage0" for c in calls)


def test_producers_distinguishes_none_running_from_unknown(monkeypatch):
    monkeypatch.setattr(live_acceptance, "pluto", lambda host, cmd, timeout=30: "0")
    assert live_acceptance.producers("h") == 0
    monkeypatch.setattr(live_acceptance, "pluto", lambda host, cmd, timeout=30: "<ssh failed: x>")
    assert live_acceptance.producers("h") is None
    monkeypatch.setattr(live_acceptance, "pluto", lambda host, cmd, timeout=30: "2")
    assert live_acceptance.producers("h") == 2


def _good_report():
    return {
        "browser_path": {
            "spectrum": {"rows": 100},
            "task_errors": [],
            "poll_errors": 0,
            "pcm_metadata_mismatches": 0,
        },
        "stop": {"http_status": 200, "body_status": "stopped"},
        "post_stop_source": {"receiver_state": "stopped", "control_ack": {"ok": True}},
    }


def test_acceptance_passes_a_clean_run_and_fails_each_broken_path():
    assert live_acceptance.acceptance_failures(_good_report()) == []
    broken = _good_report()
    broken["browser_path"]["spectrum"]["rows"] = 0
    assert any("spectrum" in f for f in live_acceptance.acceptance_failures(broken))
    broken = _good_report()
    broken["browser_path"]["task_errors"] = ["listen websocket: boom"]
    assert any("observer" in f for f in live_acceptance.acceptance_failures(broken))
    broken = _good_report()
    broken["stop"]["http_status"] = 504
    assert any("STOP" in f for f in live_acceptance.acceptance_failures(broken))
    broken = _good_report()
    broken["post_stop_source"]["control_ack"] = {"ok": False}
    assert any("acknowledge" in f for f in live_acceptance.acceptance_failures(broken))
    assert live_acceptance.acceptance_failures({}) == ["no browser-path data collected"]


@pytest.mark.parametrize(
    "argv",
    [
        ["--binary", "b", "--out", "o", "--seconds", "500"],
        ["--binary", "b", "--out", "o", "--forward", "1;rm -rf /"],
        ["--binary", "b", "--out", "o", "--forward", "1,2,3,4,5,6,7,8,9"],
    ],
)
def test_live_acceptance_rejects_unbounded_or_unsafe_arguments(monkeypatch, argv):
    monkeypatch.setattr(sys, "argv", ["live_acceptance", *argv])
    with pytest.raises(SystemExit) as exit_info:
        live_acceptance.parse_args()
    assert exit_info.value.code == 2


def test_atsc_pilot_matches_the_uhf_pilot_grid_only():
    assert survey_wideband.atsc_pilot(470_320_300) == 470_309_441
    assert survey_wideband.atsc_pilot(512_321_500) == 512_309_441
    assert survey_wideband.atsc_pilot(454_217_000) is None  # below UHF TV
    assert survey_wideband.atsc_pilot(473_000_000) is None  # between pilots


def test_mode_guess_bands():
    assert "carrier" in survey_wideband.mode_guess(800, "narrow_carrier")
    assert "NXDN48" in survey_wideband.mode_guess(4000, "occupied_region")
    assert "DMR" in survey_wideband.mode_guess(8000, "occupied_region")
    assert "analog FM" in survey_wideband.mode_guess(11000, "occupied_region")
    assert "wideband" in survey_wideband.mode_guess(6_000_000, "broad_hump_or_passband_shape")
