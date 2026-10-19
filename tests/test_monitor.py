import shutil
import time

import numpy as np
from fastapi.testclient import TestClient

from radio_tools.monitor import Channel, Monitor, SourceConfig, create_app, protocol_evidence


def test_catalog_retains_quiet_voice_and_excludes_duplicate_rf(tmp_path):
    m = Monitor(tmp_path)
    m.status.state = "receiving"
    m.channels["voice"] = Channel(
        id="voice",
        frequency_hz=454000000,
        protocol="DMR",
        slot=2,
        kind="voice",
        decoded=True,
        last_heard=time.time() - 60,
    )
    m.channels["454000000:rf"] = Channel(
        id="454000000:rf", frequency_hz=454000000, last_rf=time.time()
    )
    snapshot = m.snapshot()
    assert len(snapshot.channels) == 1
    assert snapshot.channels[0].decoded
    assert not snapshot.channels[0].speech_active
    m.close()


def test_queue_keeps_latest_complete_batch_without_blocking_capture(tmp_path):
    m = Monitor(tmp_path)
    old = np.zeros((2, 2), dtype="int16")
    new = np.ones((2, 2), dtype="int16")
    m._enqueue(old, 6000000, 1)
    m._enqueue(new, 6000000, 2)
    assert m.status.dropped_batches == 1
    assert m.jobs.qsize() == 1
    assert m.jobs.get_nowait()[0] is new
    m.close()


def test_disconnect_clears_activity_but_retains_catalog_and_expired_audio_errors(tmp_path):
    m = Monitor(tmp_path)
    m.channels["a"] = Channel(
        id="a",
        frequency_hz=454000000,
        kind="voice",
        decoded=True,
        last_heard=time.time(),
        last_rf=time.time(),
    )
    with TestClient(create_app(m)) as c:
        response = c.post("/api/stop")
        assert response.status_code == 200
        data = response.json()
        assert data["mode"] == "disconnected"
        assert not data["channels"][0]["speech_active"]
        assert not data["channels"][0]["rf_active"]
        assert c.get("/api/audio/missing").status_code == 404
        assert (
            c.post(
                "/api/source", json={"mode": "replay", "recording": "../../secret.wav"}
            ).status_code
            == 422
        )
        assert (
            c.post("/api/stop", headers={"Origin": "https://untrusted.example"}).status_code == 403
        )
        assert c.get("/api/status", headers={"Host": "evil.example"}).status_code == 403


def test_receiver_failure_restores_controls_and_exposes_error(tmp_path):
    controls = {
        "sampling_frequency": "5000000",
        "rf_bandwidth": "5000000",
        "frequency": "454000000",
        "gain_control_mode": "slow_attack",
    }

    class Receiver:
        def __init__(self, uri):
            self.values = controls.copy()
            self.closed = False

        def open(self):
            pass

        def read(self, key):
            return self.values[key]

        def write(self, key, value):
            self.values[key] = value

        def start(self, size):
            pass

        def refill(self):
            raise OSError("USB disconnected")

        def stop(self):
            pass

        def close(self):
            self.closed = True

    rx = Receiver("test")
    m = Monitor(tmp_path, receiver_factory=lambda uri: rx)
    m.binary = shutil.which("true")
    m.start(SourceConfig(mode="live"))
    m.producer.join(timeout=2)
    assert rx.values == controls
    assert rx.closed
    status = m.snapshot()
    assert status.state == "error"
    assert "USB disconnected" in status.error
    assert not status.channels
    m.close()


def test_production_console_is_live_only(tmp_path):
    m = Monitor(tmp_path)
    with TestClient(create_app(m)) as c:
        assert c.post("/api/source", json={"mode": "replay"}).status_code == 422
        assert c.get("/api/archive").status_code == 404
        assert c.get("/api/recordings").status_code == 404
        assert c.get("/api/audio/old").status_code == 404
        assert c.post("/api/monitor", json={"channel_id": "missing"}).status_code == 400
        assert c.post("/api/monitor", json={"channel_id": None}).status_code == 200


def test_false_sync_and_control_carriers_do_not_become_voice():
    assert protocol_evidence("DMR", "Sync: +DMRv") == (False, False)
    assert protocol_evidence("DMR", "Sync: +DMRd\n" * 6) == (False, True)
    assert protocol_evidence("DMR", "Sync: +DMRv\n" * 3) == (True, True)
    assert protocol_evidence("NXDN48", "Sync: +NXDN48\n" + "MBE: x\n" * 4) == (False, False)
    assert protocol_evidence("NXDN48", "Sync: +NXDN48\n" + "MBE: x\n" * 15) == (True, True)


def test_width_marks_retained_catalog_outside_range_unavailable(tmp_path):
    m = Monitor(tmp_path)
    m.status.state = "receiving"
    m.status.rate_hz = 1250000
    m.channels["near"] = Channel(
        id="near", frequency_hz=454029223, kind="voice", last_rf=time.time(), last_heard=time.time()
    )
    m.channels["far"] = Channel(
        id="far", frequency_hz=452341815, kind="voice", last_rf=time.time(), last_heard=time.time()
    )
    rows = {c.id: c for c in m.snapshot().channels}
    assert rows["near"].available
    assert not rows["far"].in_range
    assert not rows["far"].available
    assert not rows["far"].rf_active
    assert not rows["far"].speech_active
    assert len(rows) == 2
    m.close()


def test_waterfall_delta_is_bounded_and_only_returns_new_rows(tmp_path):
    m = Monitor(tmp_path)
    block = np.zeros((8192, 2), dtype="int16")
    m._spectrum(block, 6000000, 1)
    first = m.spectrum()
    assert first.sequence == 1
    assert len(first.rows) == 1
    assert m.spectrum(after=first.sequence).rows == []
    m.last_fft = 0
    m._spectrum(block, 6000000, 2)
    delta = m.spectrum(after=first.sequence)
    assert delta.sequence == 2
    assert delta.times == [2]
    assert len(delta.rows) == 1
    m.close()


def test_live_pcm_socket_routes_fresh_audio_with_source_generation(tmp_path):
    import base64

    m = Monitor(tmp_path)
    m.generation = 4
    m.channels["dmr"] = Channel(
        id="dmr", frequency_hz=454029223, protocol="DMR", slot=2, kind="voice"
    )
    with TestClient(create_app(m)) as c, c.websocket_connect("/api/listen") as socket:
        deadline = time.monotonic() + 2
        while not m.subscribers and time.monotonic() < deadline:
            time.sleep(0.01)
        m._listener_audio(
            b"\x01\x00" * 160,
            {
                "channel_id": "dmr",
                "rate_hz": 8000,
                "sequence": 1,
                "generation": 3,
                "ingress_age_ms": 25,
            },
        )
        packet = socket.receive_json()
        assert packet["type"] == "audio"
        assert packet["source_generation"] == 4
        assert packet["channel_id"] == "dmr"
        assert base64.b64decode(packet["pcm"]) == b"\x01\x00" * 160


def test_fast_stream_can_promote_rf_row_without_hiding_its_only_voice(tmp_path):
    m = Monitor(tmp_path)
    m.status.state = "receiving"
    m.channels["454029223:rf"] = Channel(
        id="454029223:rf",
        frequency_hz=454029223,
        protocol="DMR",
        slot=2,
        kind="voice",
        last_heard=time.time(),
    )
    rows = m.snapshot().channels
    assert len(rows) == 1
    assert rows[0].slot == 2
    assert rows[0].kind == "voice"
    m.close()
