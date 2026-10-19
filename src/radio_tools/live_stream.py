"""Bounded, persistent live IQ -> 48 kHz discriminator -> DSDcc voice stream.

Ingress age describes the most recently submitted receiver block, not an aligned
speech timestamp. Dropped input resets every DSP/decoder state before continuing.
"""

import os
import queue
import re
import subprocess
import threading
import time
from collections import deque
from dataclasses import dataclass
from math import gcd

import numpy as np
from scipy.signal import firwin, lfilter, upfirdn


class _Resampler:
    """Causal rational FIR resampling on one global output sample grid."""

    def __init__(self, input_rate, output_rate):
        divisor = gcd(input_rate, output_rate)
        self.up, self.down = output_rate // divisor, input_rate // divisor
        factor = max(self.up, self.down)
        self.h = firwin(20 * factor + 1, 1 / factor) * self.up
        self.keep = (len(self.h) + self.up - 1) // self.up + self.down
        self.history = np.empty(0)
        self.total = self.emitted = 0

    def process(self, x):
        if not len(x):
            return x.copy()
        start = self.total - len(self.history)
        aligned_start = start // self.down * self.down
        context = np.concatenate((np.zeros(start - aligned_start, dtype=x.dtype), self.history, x))
        y = upfirdn(self.h, context, up=self.up, down=self.down)
        self.total += len(x)
        end = ((self.total - 1) * self.up) // self.down + 1
        first = self.emitted - aligned_start * self.up // self.down
        result = y[first : first + end - self.emitted]
        self.emitted = end
        self.history = context[-self.keep :].copy()
        # Initial zero padding is not actual input history.
        self.history = self.history[-min(self.total, self.keep) :]
        return result


class _ChannelDSP:
    def __init__(self, rate_hz, offset_hz):
        self.rate = rate_hz
        self.offset = offset_hz
        self.phase = 0.0
        self.channel = _Resampler(rate_hz, 50000)
        self.audio = _Resampler(50000, 48000)
        self.taps = firwin(161, 6250, fs=50000)
        self.zi = np.zeros(160, dtype=np.complex128)
        self.dc = np.zeros(1)
        self.previous = None

    def process(self, block):
        x = block[:, 0].astype(np.complex64)
        x.imag = block[:, 1]
        step = -2 * np.pi * self.offset / self.rate
        x *= np.exp(1j * (self.phase + step * np.arange(len(x))))
        self.phase = (self.phase + step * len(x)) % (2 * np.pi)
        x = self.channel.process(x)
        x, self.zi = lfilter(self.taps, [1.0], x, zi=self.zi)
        if not len(x):
            return b""
        previous = x[0] if self.previous is None else self.previous
        fm = np.angle(x * np.conj(np.concatenate(([previous], x[:-1]))))
        self.previous = x[-1]
        fm, self.dc = lfilter([1, -1], [1, -0.999], fm, zi=self.dc)
        fm = self.audio.process(fm)
        pcm = np.clip(fm * 50000 / (2 * np.pi * 5000) * 16000, -32768, 32767)
        return pcm.astype("<i2").tobytes()


class _VoiceGate:
    """Require repeated real voice evidence; one accidental sync never unmutes."""

    def __init__(self, protocol):
        self.protocol = protocol
        self.events = deque()
        self.tail = ""
        self.nxdn_confirmed = False

    def update(self, text, now):
        text = self.tail + text
        # Keep partial tokens across pipe reads without counting complete ones twice.
        cut = max(0, len(text) - 40)
        tokens = list(re.finditer(r"Sync:\s+[+-](DMRv|DMRd|NXDN48)|MBE:|parity error", text))
        consumed = 0
        for token in tokens:
            self.events.append((now, token.group(1) or token.group()))
            consumed = token.end()
        self.tail = text[max(consumed, cut) :]
        self._expire(now)

    def _expire(self, now):
        while self.events and self.events[0][0] < now - 1.5:
            self.events.popleft()

    def active(self, now):
        self._expire(now)
        kinds = [kind for _, kind in self.events]
        if self.protocol == "DMR":
            return kinds.count("DMRv") >= 2
        frames = kinds.count("MBE:")
        if "NXDN48" in kinds and frames >= 12 and kinds.count("parity error") < frames / 2:
            self.nxdn_confirmed = True
        return self.nxdn_confirmed and frames >= 4 and kinds.count("parity error") < frames / 2


@dataclass(frozen=True)
class _Config:
    channel_id: str
    frequency_hz: float
    protocol: str
    slot: int
    rate_hz: int
    center_hz: float


class _Session:
    def __init__(self, owner, config, generation):
        self.owner, self.config, self.generation = owner, config, generation
        self.dsp = _ChannelDSP(config.rate_hz, config.frequency_hz - config.center_hz)
        self.gate = _VoiceGate(config.protocol)
        self.lock = threading.Lock()
        self.closed = threading.Event()
        self._close_lock = threading.Lock()
        self.latest_acquired = None
        self.sequence = 0
        self.processes = []
        self.threads = []
        mode = "-fr" if config.protocol == "DMR" else "-fi"
        args = [owner.binary, "-i", "-", "-o", "-", mode, "-T", str(config.slot)]
        try:
            self.audio = subprocess.Popen(
                args + ["-q"],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                bufsize=0,
            )
            self.processes.append(self.audio)
            self.probe = subprocess.Popen(
                args + ["-n", "-v", "2"],
                stdin=subprocess.PIPE,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                bufsize=0,
            )
            self.processes.append(self.probe)
            for name, target, arg in (
                ("audio", self._audio, self.audio.stdout),
                ("probe", self._diagnostics, self.probe.stderr),
                ("errors", self._errors, self.audio.stderr),
            ):
                thread = threading.Thread(
                    target=target, args=(arg,), name=f"live-{name}-{generation}", daemon=True
                )
                self.threads.append(thread)
                thread.start()
        except Exception:
            self.close()
            raise

    def _read(self, pipe):
        while not self.closed.is_set():
            try:
                data = os.read(pipe.fileno(), 4096)
            except (OSError, ValueError):
                return
            if not data:
                if not self.closed.is_set():
                    self.owner._error(self.generation, "DSDcc stream closed unexpectedly")
                return
            yield data

    def _diagnostics(self, pipe):
        for data in self._read(pipe):
            text = data.decode(errors="replace")
            with self.lock:
                self.gate.update(text, time.monotonic())
                active = self.gate.active(time.monotonic())
            self.owner._activity(self.generation, active, text)

    def _errors(self, pipe):
        for data in self._read(pipe):
            self.owner._diagnostic(self.generation, data.decode(errors="replace"))

    def _audio(self, pipe):
        pending = b""
        for data in self._read(pipe):
            data = pending + data
            pending = data[len(data) // 2 * 2 :]
            data = data[: len(data) // 2 * 2]
            with self.lock:
                active = self.gate.active(time.monotonic())
                acquired = self.latest_acquired
            if data and active and acquired is not None:
                self.sequence += 1
                self.owner._audio(
                    self.generation,
                    data,
                    {
                        "sequence": self.sequence,
                        "generation": self.generation,
                        "channel_id": self.config.channel_id,
                        "rate_hz": 8000,
                        "ingress_age_ms": max(0, (time.monotonic() - acquired) * 1000),
                        "ingress_age_basis": "latest decoder input block; speech alignment unknown",
                    },
                )

    def feed(self, block, acquired):
        pcm = self.dsp.process(block)
        with self.lock:
            self.latest_acquired = acquired
        for process in self.processes:
            view = memoryview(pcm)
            while view and not self.closed.is_set():
                written = process.stdin.write(view)
                if not written:
                    raise BrokenPipeError("DSDcc stopped accepting input")
                view = view[written:]

    def close(self):
        with self._close_lock:
            self._close()

    def _close(self):
        # Terminate first: another thread may be blocked writing into stdin.
        self.closed.set()
        for process in self.processes:
            if process.poll() is None:
                try:
                    process.terminate()
                except ProcessLookupError:
                    pass
        for process in self.processes:
            try:
                process.wait(timeout=1)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=1)
        for thread in self.threads:
            if thread is not threading.current_thread():
                thread.join(timeout=1)
        for process in self.processes:
            for pipe in (process.stdin, process.stdout, process.stderr):
                if pipe:
                    pipe.close()


class LiveListener:
    """One selected live channel; receiver threads only copy/enqueue input."""

    def __init__(self, binary, on_audio, on_activity=None):
        self.binary, self.on_audio, self.on_activity = str(binary), on_audio, on_activity
        self._lock = threading.RLock()
        self._queue = queue.Queue(maxsize=8)
        self._config = None
        self._generation = 0
        self._session = None
        self._closed = threading.Event()
        self._state = {
            "status": "stopped",
            "dropped_blocks": 0,
            "discontinuities": 0,
            "audio_chunks": 0,
            "voice_active": False,
            "error": None,
            "diagnostics": "",
        }
        self._worker = threading.Thread(target=self._run, name="live-listener", daemon=True)
        self._worker.start()

    def configure(self, channel_id, frequency_hz, protocol, slot, rate_hz, center_hz):
        if protocol not in ("DMR", "NXDN48") or slot not in (1, 2):
            raise ValueError("Live listener requires DMR/NXDN48 and slot 1/2")
        if rate_hz <= 50000 or abs(frequency_hz - center_hz) + 15000 >= rate_hz / 2:
            raise ValueError("Selected channel must fit inside receiver bandwidth")
        config = _Config(channel_id, frequency_hz, protocol, slot, int(rate_hz), center_hz)
        with self._lock:
            if self._closed.is_set():
                raise RuntimeError("Live listener is closed")
            if config == self._config:
                return
            self._generation += 1
            self._config = config
            self._clear_queue()
            self._state.update(
                status="starting", channel_id=channel_id, voice_active=False, error=None
            )
            old = self._session
            self._session = None
        if old:
            old.close()

    def _clear_queue(self):
        while True:
            try:
                self._queue.get_nowait()
            except queue.Empty:
                return

    def feed(self, block, acquired):
        if self._closed.is_set():
            return False
        with self._lock:
            if self._config is None:
                return False
            generation = self._generation
        # Receiver buffers can be reused immediately after feed returns.
        owned = np.array(block, dtype=np.int16, copy=True, order="C")
        if owned.ndim != 2 or owned.shape[1] != 2:
            raise ValueError("Expected ci16 I/Q array with shape (N, 2)")
        try:
            self._queue.put_nowait((generation, owned, acquired))
            return True
        except queue.Full:
            with self._lock:
                self._state["dropped_blocks"] += self._queue.qsize() + 1
                self._state["discontinuities"] += 1
                self._generation += 1
                self._state.update(status="discontinuity", voice_active=False)
                self._clear_queue()
            return False

    def _run(self):
        while not self._closed.is_set():
            try:
                generation, block, acquired = self._queue.get(timeout=0.1)
            except queue.Empty:
                continue
            with self._lock:
                if generation != self._generation or self._config is None:
                    continue
                config = self._config
                session = self._session
            try:
                if session is None or session.generation != generation:
                    if session:
                        session.close()
                    session = _Session(self, config, generation)
                    with self._lock:
                        stale = generation != self._generation or self._closed.is_set()
                        if not stale:
                            self._session = session
                            self._state["status"] = "listening"
                    if stale:
                        session.close()
                        continue
                session.feed(block, acquired)
            except Exception as exc:  # noqa: BLE001 - retain worker and surface failure
                self._error(generation, f"{type(exc).__name__}: {exc}")
                if session:
                    session.close()
                with self._lock:
                    if self._session is session:
                        self._session = None

    def _audio(self, generation, pcm, metadata):
        with self._lock:
            if generation != self._generation or self._config is None:
                return
            self._state["audio_chunks"] += 1
            self._state["ingress_age_ms"] = metadata["ingress_age_ms"]
            self._state["sequence"] = metadata["sequence"]
            self._state["last_audio_at"] = time.time()
            # Keep callback ordering atomic with reconfiguration.
            try:
                self.on_audio(pcm, metadata)
            except Exception as exc:  # noqa: BLE001 - callbacks must not kill the pipe reader
                self._error(generation, f"Audio callback failed: {exc}")

    def _activity(self, generation, active, diagnostic):
        with self._lock:
            if generation != self._generation:
                return
            self._state["voice_active"] = active
            self._state["diagnostics"] = (self._state["diagnostics"] + diagnostic)[-4000:]
            if self.on_activity:
                try:
                    self.on_activity(
                        {
                            "generation": generation,
                            "channel_id": self._config.channel_id,
                            "voice_active": active,
                        }
                    )
                except Exception as exc:  # noqa: BLE001 - expose callback failure in status
                    self._error(generation, f"Activity callback failed: {exc}")

    def _diagnostic(self, generation, diagnostic):
        with self._lock:
            if generation == self._generation:
                self._state["diagnostics"] = (self._state["diagnostics"] + diagnostic)[-4000:]

    def _error(self, generation, message):
        with self._lock:
            if generation == self._generation:
                self._state.update(status="error", error=message, voice_active=False)

    def snapshot(self):
        with self._lock:
            state = dict(self._state, generation=self._generation, queue_blocks=self._queue.qsize())
            state["state"] = state["status"]
            state["input_dropped"] = state["dropped_blocks"]
            state["last_audio_age_ms"] = (
                (time.time() - state["last_audio_at"]) * 1000
                if state.get("last_audio_at")
                else None
            )
            session = self._session
            if session:
                state["decoder_pids"] = [process.pid for process in session.processes]
                with session.lock:
                    state["voice_active"] = session.gate.active(time.monotonic())
            return state

    def stop(self):
        with self._lock:
            self._generation += 1
            self._config = None
            self._clear_queue()
            session = self._session
            self._session = None
            self._state.update(status="stopped", voice_active=False)
        if session:
            session.close()

    def close(self):
        self._closed.set()
        self.stop()
        if threading.current_thread() is not self._worker:
            self._worker.join(timeout=3)
