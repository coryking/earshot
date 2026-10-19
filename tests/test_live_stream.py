"""Streaming continuity, false-sync rejection, and process ownership checks."""

import sys
import time

import numpy as np
from numpy.testing import assert_allclose

from radio_tools.live_stream import LiveListener, _ChannelDSP, _Resampler, _VoiceGate


def test_resampler_arbitrary_chunks_preserve_output_grid():
    rng = np.random.default_rng(12)
    x = rng.normal(size=40001) + 1j * rng.normal(size=40001)
    for rate, target in ((6000000, 50000), (1250000, 50000), (50000, 48000)):
        whole = _Resampler(rate, target).process(x)
        stream = _Resampler(rate, target)
        split = np.concatenate([stream.process(part) for part in np.array_split(x, 37)])
        assert_allclose(split, whole, atol=1e-12)


def test_channel_dsp_keeps_nco_filter_discriminator_and_resampler_state():
    rate = 1_250_000
    t = np.arange(450001) / rate
    phase = 2 * np.pi * (-100_000 * t + 0.35 * np.sin(2 * np.pi * 1700 * t))
    iq = np.column_stack((np.cos(phase), np.sin(phase)))
    iq = (iq * 20000).astype(np.int16)
    whole = np.frombuffer(_ChannelDSP(rate, -100000).process(iq), dtype="<i2")
    stream = _ChannelDSP(rate, -100000)
    split = np.frombuffer(b"".join(stream.process(x) for x in np.array_split(iq, 23)), dtype="<i2")
    assert_allclose(split, whole, atol=1)


def test_voice_guard_requires_repeated_voice_not_data_and_expires():
    gate = _VoiceGate("DMR")
    gate.update("Sync: +DMRd\n" * 8 + "Sync: +DMRv\n", 10)
    assert not gate.active(10)
    gate.update("Sync: +DM", 10.1)
    gate.update("Rv\n", 10.2)
    assert gate.active(10.3)
    assert not gate.active(12)
    gate = _VoiceGate("NXDN48")
    gate.update("Sync: +NXDN48\n" + "MBE: x\n" * 4, 10)
    assert not gate.active(10)
    gate.update("MBE: x\n" * 8, 10.2)
    assert gate.active(10.3)
    gate.update("MBE: x\n" * 12, 12)
    assert gate.active(12.1)  # A sustained burst need not repeat frame-sync logging.
    gate.update("parity error\n" * 8, 12.2)
    assert not gate.active(12.3)


def _wait(predicate, timeout=3):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.01)
    raise AssertionError("timed out")


def test_reconfigure_and_close_reap_processes_and_suppress_stale_audio(tmp_path):
    binary = tmp_path / "fake-dsd"
    binary.write_text(f"""#!{sys.executable}
import os,sys
probe = '-n' in sys.argv
while True:
    data = os.read(0, 4096)
    if not data: break
    if probe: os.write(2, b'Sync: +DMRv\\nSync: +DMRv\\n')
    else: os.write(1, bytes(320))
""")
    binary.chmod(0o755)
    audio = []
    listener = LiveListener(binary, lambda pcm, meta: audio.append(meta))
    listener.configure("a", 100000, "DMR", 2, 1250000, 100000)
    block = np.ones((62500, 2), dtype=np.int16)
    listener.feed(block, time.monotonic())
    _wait(lambda: listener._session is not None)
    first = listener._session
    _wait(lambda: first.gate.active(time.monotonic()))
    listener.feed(block, time.monotonic())
    _wait(lambda: len(audio) > 0)
    listener.configure("b", 110000, "DMR", 1, 1250000, 100000)
    assert all(p.poll() is not None for p in first.processes)
    before = len(audio)
    listener._audio(first.generation, b"xx", {"ingress_age_ms": 1})
    assert len(audio) == before
    listener.feed(block, time.monotonic())
    _wait(lambda: listener._session is not None)
    second = listener._session
    listener.close()
    assert all(p.poll() is not None for p in second.processes)
    assert not listener._worker.is_alive()
    assert all(not t.is_alive() for t in first.threads + second.threads)


def test_queue_overrun_resets_decoder_and_owns_receiver_memory(monkeypatch):
    import threading

    import radio_tools.live_stream as live

    entered, release = threading.Event(), threading.Event()
    sessions = []

    class Session:
        def __init__(self, owner, config, generation):
            self.generation = generation
            self.closed = False
            self.blocks = []
            sessions.append(self)

        def feed(self, block, acquired):
            self.blocks.append(block)
            if len(sessions) == 1:
                entered.set()
                release.wait(2)

        def close(self):
            self.closed = True

    monkeypatch.setattr(live, "_Session", Session)
    listener = LiveListener("unused", lambda *args: None)
    listener.configure("a", 100000, "DMR", 1, 1250000, 100000)
    block = np.ones((20, 2), dtype=np.int16)
    try:
        listener.feed(block, time.monotonic())
        assert entered.wait(1)
        block[:] = 99
        assert np.all(sessions[0].blocks[0] == 1)
        started = time.monotonic()
        for _ in range(9):
            listener.feed(block, time.monotonic())
        assert time.monotonic() - started < 0.1
        assert listener._queue.qsize() <= 8
        assert listener._state["discontinuities"] == 1
        release.set()
        listener.feed(block, time.monotonic())
        _wait(lambda: len(sessions) == 2)
        assert sessions[0].closed
        assert sessions[1].generation > sessions[0].generation
    finally:
        release.set()
        listener.close()
