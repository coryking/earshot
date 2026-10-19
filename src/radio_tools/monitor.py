"""Local receive console: continuous acquisition, bounded analysis, real decoded audio."""

from __future__ import annotations

import argparse
import asyncio
import base64
import concurrent.futures
import os
import queue
import re
import shutil
import subprocess
import tempfile
import threading
import time
from collections import deque
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Literal

import numpy as np
from fastapi import FastAPI, HTTPException, Request, WebSocket, WebSocketDisconnect
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel, Field
from scipy.io import wavfile
from scipy.signal import firwin, lfilter, resample_poly

from .digital import channelize_iq
from .live_stream import LiveListener
from .pluto import CONTROLS, PlutoReceiver
from .survey import survey_iq

ROOT = Path(__file__).resolve().parents[2]
DATA_ROOT = Path(os.getenv("RADIO_DATA_ROOT", str(ROOT)))
# IDs identify actual receiver frequencies and slots, not station identities.
KNOWN = [
    (454029223, "DMR", 2),
    (452341815, "NXDN48", 1),
    (452685504, "DMR", 1),
    (454216998, "DMR", 0),
    (452360766, "DMR", 0),
    (452273059, "DMR", 0),
]


class SourceConfig(BaseModel):
    mode: Literal["live"] = "live"
    uri: str = "usb:32.18.5"
    center_hz: int = Field(default=454216998, ge=70_000_000, le=6_000_000_000)
    rate_hz: int = Field(default=6_000_000, ge=520834, le=6_000_000)


class MonitorSelection(BaseModel):
    channel_id: str | None


class Channel(BaseModel):
    id: str
    frequency_hz: float
    protocol: str = "Unknown"
    slot: int | None = None
    kind: Literal["voice", "data", "unidentified"] = "unidentified"
    rf_active: bool = False
    decoded: bool = False
    speech_active: bool = False
    last_heard: float | None = None
    last_rf: float | None = None
    level_db: float | None = None
    audio_seconds: float = 0
    sequence: int = 0
    in_range: bool = True
    available: bool = False


class Status(BaseModel):
    mode: Literal["live", "disconnected"] = "disconnected"
    state: Literal["stopped", "connecting", "receiving", "error", "stopping"] = "stopped"
    source_name: str = "No source connected"
    center_hz: int = 454216998
    rate_hz: int = 6000000
    bandwidth_hz: int = 6000000
    generation: int = 0
    received_seconds: float = 0
    source_position_s: float = 0
    decode_latency_s: float = 0
    dropped_batches: int = 0
    processed_batches: int = 0
    queue_depth: int = 0
    error: str | None = None
    decoder_error: str | None = None
    started_at: float | None = None
    last_samples_at: float | None = None
    continuity: str = "Hardware sample continuity is unverified."
    channels: list[Channel] = Field(default_factory=list)


class Spectrum(BaseModel):
    sequence: int = 0
    generation: int = 0
    rows: list[list[float]]
    times: list[float]
    center_hz: int
    span_hz: int
    low_db: float = -125
    high_db: float = -65
    unit: str = "dBFS/Hz (container scale, not calibrated dBm)"
    source: str


def protocol_evidence(name: str, text: str, confirmed: bool = False) -> tuple[bool, bool]:
    """Separate real frame evidence from DSDcc's occasional false-sync audio."""
    syncs = re.findall(r"Sync:\s+([+-]\S+)", text)
    if name == "DMR":
        voice = sum(x.endswith("DMRv") for x in syncs) >= (1 if confirmed else 2)
        data = sum(x.endswith("DMRd") for x in syncs) >= 3
        return voice, voice or data
    frames = text.count("MBE:")
    voice = (
        any("NXDN48" in x for x in syncs)
        and frames >= (4 if confirmed else 12)
        and text.count("parity error") < frames / 2
    )
    return voice, voice


class Monitor:
    def __init__(self, data_root: Path = DATA_ROOT, receiver_factory=PlutoReceiver):
        self.data_root = data_root
        self.receiver_factory = receiver_factory
        self.lock = threading.RLock()
        self.lifecycle = threading.Lock()
        self.status = Status()
        self.channels: dict[str, Channel] = {}
        self.protocols: dict[int, str] = {}
        self.rows: deque = deque(maxlen=240)
        self.cache = Path(tempfile.mkdtemp(prefix="radio-monitor-"))
        self.stop_event = threading.Event()
        self.producer: threading.Thread | None = None
        self.worker: threading.Thread | None = None
        self.jobs: queue.Queue = queue.Queue(maxsize=1)
        self.generation = 0
        self.spectrum_sequence = 0
        self.last_fft = 0.0
        self.binary = os.getenv("RADIO_DSDCC") or str(data_root / ".tools/bin/dsdccx")
        self.pool = concurrent.futures.ThreadPoolExecutor(max_workers=3)
        self.monitor_id: str | None = None
        self.subscribers: set[queue.Queue] = set()
        self.listener = LiveListener(self.binary, self._listener_audio)

    def _listener_audio(self, pcm, metadata):
        packet = {
            "type": "audio",
            "source_generation": self.generation,
            **metadata,
            "pcm": base64.b64encode(pcm).decode("ascii"),
        }
        with self.lock:
            ch = self.channels.get(metadata["channel_id"])
            if ch:
                ch.last_heard = time.time()
                ch.decoded = True
                ch.kind = "voice"
            subscribers = list(self.subscribers)
        for subscriber in subscribers:
            if subscriber.full():
                try:
                    subscriber.get_nowait()
                except queue.Empty:
                    pass
            try:
                subscriber.put_nowait(packet)
            except queue.Full:
                pass

    def select(self, channel_id):
        with self.lock:
            if channel_id is not None and channel_id not in self.channels:
                raise ValueError("Channel is unavailable; choose a detected channel.")
            self.monitor_id = channel_id
        self._configure_listener()
        return self.listener.snapshot()

    def _configure_listener(self):
        with self.lock:
            ch = self.channels.get(self.monitor_id)
            rate, center, receiving = (
                self.status.rate_hz,
                self.status.center_hz,
                self.status.state == "receiving",
            )
        if (
            not ch
            or not receiving
            or ch.protocol not in ("DMR", "NXDN48")
            or ch.slot not in (1, 2)
            or abs(ch.frequency_hz - center) + 15000 >= rate / 2
        ):
            self.listener.stop()
            return
        self.listener.configure(
            channel_id=ch.id,
            frequency_hz=ch.frequency_hz,
            protocol=ch.protocol,
            slot=ch.slot,
            rate_hz=rate,
            center_hz=center,
        )

    def snapshot(self):
        with self.lock:
            now = time.time()
            channels = []
            voice_frequencies = {
                ch.frequency_hz
                for ch in self.channels.values()
                if ch.kind == "voice" and not ch.id.endswith(":rf")
            }
            for ch in self.channels.values():
                if ch.id.endswith(":rf") and ch.frequency_hz in voice_frequencies:
                    continue
                item = ch.model_copy()
                item.in_range = (
                    abs(item.frequency_hz - self.status.center_hz) + 15000 < self.status.rate_hz / 2
                )
                item.available = item.in_range and self.status.state == "receiving"
                item.rf_active = bool(item.last_rf and now - item.last_rf < 10 and item.available)
                item.speech_active = bool(
                    item.last_heard
                    and item.last_heard >= (self.status.started_at or 0)
                    and now - item.last_heard < max(8, self.status.decode_latency_s + 5)
                    and item.available
                    and item.kind == "voice"
                )
                channels.append(item)
            self.status.queue_depth = self.jobs.qsize()
            return self.status.model_copy(
                update={
                    "channels": sorted(
                        channels,
                        key=lambda x: (x.frequency_hz, x.slot or 0),
                    )
                }
            )

    def start(self, config: SourceConfig):
        with self.lifecycle:
            self._stop()
            if not Path(self.binary).is_file():
                raise ValueError(
                    "DSDcc decoder missing. Run radio doctor and install .tools/bin/dsdccx."
                )
            self.stop_event = threading.Event()
            self.generation += 1
            self.jobs = queue.Queue(maxsize=1)
            with self.lock:
                self.rows.clear()
                self.last_fft = 0.0
                self.status = Status(
                    mode=config.mode,
                    generation=self.generation,
                    state="connecting",
                    center_hz=config.center_hz,
                    rate_hz=config.rate_hz,
                    bandwidth_hz=config.rate_hz,
                    started_at=time.time(),
                    source_name=f"Pluto · {config.uri}",
                )
            self.worker = threading.Thread(target=self._work, args=(self.generation,), daemon=True)
            self.producer = threading.Thread(target=self._receive, args=(config,), daemon=True)
            self.worker.start()
            self.producer.start()
        return self.snapshot()

    def _stop(self):
        self.stop_event.set()
        with self.lock:
            if self.producer and self.producer.is_alive():
                self.status.state = "stopping"
        # libiio has a 5-second read timeout; only its owning thread closes the context.
        for thread in (self.producer, self.worker):
            if thread:
                thread.join(timeout=35)
                if thread.is_alive():
                    raise RuntimeError("Source is still stopping; wait before reconnecting.")
        self.listener.stop()
        with self.lock:
            self.status.mode = "disconnected"
            if self.status.state != "error":
                self.status.state = "stopped"
            for ch in self.channels.values():
                ch.speech_active = ch.rf_active = False

    def stop(self):
        with self.lifecycle:
            self._stop()
        return self.snapshot()

    def close(self):
        self.stop()
        self.listener.close()
        self.pool.shutdown(wait=True, cancel_futures=True)
        shutil.rmtree(self.cache, ignore_errors=True)

    def _receive(self, config):
        rx = None
        original = {}
        touched = False
        try:
            rx = self.receiver_factory(config.uri)
            rx.open()
            original = {key: rx.read(key) for key in CONTROLS}
            touched = True
            for key, value in {
                "sampling_frequency": config.rate_hz,
                "rf_bandwidth": config.rate_hz,
                "frequency": config.center_hz,
                "gain_control_mode": "fast_attack",
            }.items():
                rx.write(key, str(value))
            rate, center = int(rx.read("sampling_frequency")), int(rx.read("frequency"))
            applied_bandwidth = int(rx.read("rf_bandwidth"))
            rx.start(min(262144, max(16384, rate // 16 // 1024 * 1024)))
            with self.lock:
                self.status.rate_hz, self.status.center_hz = rate, center
                self.status.bandwidth_hz = applied_bandwidth
                self.status.state = "receiving"
            self._configure_listener()
            parts, frames, position = [], 0, 0
            while not self.stop_event.is_set():
                block = np.frombuffer(rx.refill(), dtype="<i2").reshape(-1, 2)
                self.listener.feed(block, time.monotonic())
                # Spectrum updates with each received buffer, independent of decoding.
                self._spectrum(block, rate, position / rate)
                with self.lock:
                    self.status.last_samples_at = time.time()
                    self.status.received_seconds += len(block) / rate
                    self.status.source_position_s = position / rate
                parts.append(block)
                frames += len(block)
                position += len(block)
                if frames >= rate * 3:
                    self._enqueue(np.concatenate(parts), rate, time.time())
                    parts, frames = [], 0
        except Exception as exc:  # noqa: BLE001 - thread boundary must report failure and restore RX
            with self.lock:
                self.status.state = "error"
                self.status.error = f"Receive failed: {exc}"
            self.stop_event.set()
        finally:
            self.listener.stop()
            errors = []
            if rx:
                try:
                    rx.stop()
                except Exception as exc:  # noqa: BLE001 - thread boundary must report failure and restore RX
                    errors.append(str(exc))
                if touched:
                    for key in CONTROLS:
                        try:
                            rx.write(key, original[key])
                            if rx.read(key) != original[key]:
                                raise OSError(f"{key} restore readback differs")
                        except Exception as exc:  # noqa: BLE001 - thread boundary must report failure and restore RX
                            errors.append(f"Restore {key}: {exc}")
                try:
                    rx.close()
                except Exception as exc:  # noqa: BLE001 - thread boundary must report failure and restore RX
                    errors.append(str(exc))
            if errors:
                with self.lock:
                    self.status.state = "error"
                    self.status.error = "; ".join(errors)

    def _spectrum(self, block, rate, position):
        now = time.monotonic()
        if now - self.last_fft < 0.115:
            return
        self.last_fft = now
        n = min(8192, len(block))
        if n < 1024:
            return
        z = (block[:n, 0].astype(float) + 1j * block[:n, 1]) / 32768
        w = np.hanning(n)
        psd = np.abs(np.fft.fftshift(np.fft.fft(z * w))) ** 2 / (rate * np.sum(w * w))
        # Preserve narrow carriers when reducing bins to screen resolution.
        psd = np.max(psd[: len(psd) // 8 * 8].reshape(-1, 8), axis=1)
        row = (10 * np.log10(np.maximum(psd, 1e-30))).round(1).tolist()
        with self.lock:
            self.spectrum_sequence += 1
            self.rows.append((self.spectrum_sequence, round(position, 3), row))

    def _enqueue(self, block, rate, acquired):
        try:
            self.jobs.put_nowait((block, rate, acquired))
        except queue.Full:
            # Keep the newest complete batch. The capture thread never waits for analysis.
            try:
                self.jobs.get_nowait()
            except queue.Empty:
                pass
            self.jobs.put_nowait((block, rate, acquired))
            with self.lock:
                self.status.dropped_batches += 1

    def _work(self, generation):
        while not self.stop_event.is_set():
            try:
                block, rate, acquired = self.jobs.get(timeout=0.2)
            except queue.Empty:
                continue
            try:
                self._analyze(block, rate, acquired, generation)
            except Exception as exc:  # noqa: BLE001 - thread boundary must report failure and restore RX
                with self.lock:
                    self.status.decoder_error = f"Decode failed: {exc}"

    def _analyze(self, block, rate, acquired, generation):
        center = self.status.center_hz
        path = self.cache / "batch.wav"
        wavfile.write(path, rate, block)
        survey = survey_iq(path, seconds=len(block) / rate, center_hz=center, windows=24, limit=16)
        candidates = [
            c
            for c in survey.result.candidates
            if c.kind == "occupied_region"
            and c.width_hz < 30000
            and abs(c.offset_hz) + 15000 < rate / 2
        ]
        targets = []
        for c in candidates:
            freq = c.frequency_hz
            known = next((k for k in KNOWN if abs(k[0] - freq) < 6500), None)
            # Known channel centers are more accurate than a short-burst survey centroid.
            if not known:
                with self.lock:
                    prior = next(
                        (
                            ch.frequency_hz
                            for ch in self.channels.values()
                            if abs(ch.frequency_hz - freq) < 2500
                        ),
                        None,
                    )
                freq = prior if prior is not None else round(freq)
            else:
                freq = known[0]
            freq = round(freq)
            protocol = self.protocols.get(freq, known[1] if known else "Unknown")
            with self.lock:
                key = f"{freq}:rf"
                ch = self.channels.setdefault(key, Channel(id=key, frequency_hz=freq))
                ch.last_rf = acquired
                ch.level_db = c.peak_dbfs_per_hz
                for stream in self.channels.values():
                    if stream.frequency_hz == freq:
                        stream.last_rf = acquired
                        stream.level_db = c.peak_dbfs_per_hz
                if known:
                    ch.protocol = protocol
                    if known[2] > 0:
                        ch.slot = known[2]
                    ch.kind = "data" if known[2] == 0 else "unidentified"
            targets.append((freq, protocol, block, rate, center, acquired, generation))
        # Rotate discovery candidates so persistent strong carriers cannot starve quieter ones.
        known_targets = [t for t in targets if t[1] != "Unknown"]
        other_targets = [t for t in targets if t[1] == "Unknown"]
        if other_targets:
            rotation = self.status.processed_batches % len(other_targets)
            other_targets = other_targets[rotation:] + other_targets[:rotation]
        futures = [
            self.pool.submit(self._decode_channel, *args)
            for args in (known_targets + other_targets)[:12]
        ]
        for future in futures:
            future.result()
        with self.lock:
            self.status.processed_batches += 1
            self.status.decode_latency_s = round(time.time() - acquired + len(block) / rate, 2)
            self.status.decoder_error = None
        path.unlink(missing_ok=True)

    def _decode_channel(self, freq, protocol, block, rate, center, acquired, generation):
        y = channelize_iq(block, rate, len(block), freq - center)
        y = lfilter(firwin(161, 6250, fs=50000), [1.0], y)
        fm = np.angle(y[1:] * np.conj(y[:-1]))
        fm -= np.mean(fm)
        fm = resample_poly(fm, 24, 25)
        pcm = (
            np.clip(fm * 50000 / (2 * np.pi * 5000) * 16000, -32768, 32767).astype("<i2").tobytes()
        )
        protocols = [("DMR", "-fr"), ("NXDN48", "-fi")]
        if protocol != "Unknown":
            protocols = [p for p in protocols if p[0] == protocol]
        modes = []
        for name, flag in protocols:
            if self.stop_event.is_set():
                return
            probe = subprocess.run(
                [self.binary, "-i", "-", "-o", "-", flag, "-n", "-v", "2"],
                input=pcm,
                capture_output=True,
                timeout=12,
                check=False,
            )
            if probe.returncode:
                raise RuntimeError(f"DSDcc probe exited {probe.returncode}")
            voice, recognized = protocol_evidence(
                name, probe.stderr.decode(errors="replace"), self.protocols.get(freq) == name
            )
            if recognized:
                with self.lock:
                    self.protocols[freq] = name
                    ch = self.channels.get(f"{freq}:rf")
                    if ch:
                        ch.protocol, ch.decoded = name, True
                        ch.kind = "unidentified" if voice else "data"
            if voice:
                modes.extend([(name, flag, slot) for slot in ((1, 2) if name == "DMR" else (1,))])
        for name, flag, slot in modes:
            if self.stop_event.is_set():
                return
            args = [self.binary, "-i", "-", "-o", "-", flag, "-T", str(slot)]
            decoded = subprocess.run(
                args + ["-q"], input=pcm, capture_output=True, timeout=12, check=False
            )
            if decoded.returncode:
                raise RuntimeError(
                    f"DSDcc exited {decoded.returncode}: {decoded.stderr.decode(errors='replace')[-300:]}"
                )
            key = f"{freq}:{name}:{slot}"
            minimum = 1600 if key in self.channels else 6400
            if len(decoded.stdout) < minimum or len(decoded.stdout) % 2:
                continue
            samples = np.frombuffer(decoded.stdout, dtype="<i2")
            if np.max(np.abs(samples.astype(float))) < 10:
                continue
            key = f"{freq}:{name}:{slot}"
            with self.lock:
                if generation != self.generation or self.stop_event.is_set():
                    return
                ch = self.channels.setdefault(
                    key, Channel(id=key, frequency_hz=freq, protocol=name, slot=slot, kind="voice")
                )
                ch.last_heard = time.time()
                ch.last_rf = acquired
                ch.decoded = True
                ch.sequence += 1
                ch.audio_seconds = len(samples) / 8000
                # No ambiguous RF-only duplicate once a voice stream has been decoded.
                self.channels.pop(f"{freq}:rf", None)

    def spectrum(self, after: int = 0):
        with self.lock:
            rows = [r for r in self.rows if r[0] > after][-60:]
            return Spectrum(
                sequence=self.spectrum_sequence,
                generation=self.generation,
                rows=[r for _, _, r in rows],
                times=[t for _, t, _ in rows],
                center_hz=self.status.center_hz,
                span_hz=self.status.rate_hz,
                source=self.status.mode,
            )


def create_app(monitor: Monitor | None = None):
    monitor = monitor or Monitor()

    @asynccontextmanager
    async def lifespan(app):
        yield
        monitor.close()

    app = FastAPI(title="Radio monitor", lifespan=lifespan)
    app.state.monitor = monitor

    @app.middleware("http")
    async def local_only(request: Request, call_next):
        # Loopback binding plus Host/Origin validation blocks remote sites driving local RX.
        host = request.headers.get("host", "").split(":")[0]
        if host not in {"127.0.0.1", "localhost", "testserver"}:
            from fastapi.responses import JSONResponse

            return JSONResponse({"detail": "Local console only"}, status_code=403)
        origin = request.headers.get("origin")
        if (
            origin
            and origin != f"http://{request.headers.get('host')}"
            and origin
            not in {
                "http://localhost:8765",
                "http://127.0.0.1:8765",
                "http://localhost:5173",
                "http://127.0.0.1:5173",
            }
        ):
            from fastapi.responses import JSONResponse

            return JSONResponse({"detail": "Local console origin required"}, status_code=403)
        return await call_next(request)

    @app.get("/api/status", response_model=Status)
    def status():
        return monitor.snapshot()

    @app.get("/api/spectrum", response_model=Spectrum)
    def spectrum(after: int = 0):
        return monitor.spectrum(after)

    @app.get("/api/listener")
    def listener_status():
        state = monitor.listener.snapshot()
        state.pop("diagnostics", None)
        return state

    @app.post("/api/monitor")
    def select(selection: MonitorSelection):
        try:
            return monitor.select(selection.channel_id)
        except ValueError as exc:
            raise HTTPException(400, str(exc)) from exc

    @app.websocket("/api/listen")
    async def listen(socket: WebSocket):
        host = socket.headers.get("host", "").split(":")[0]
        origin = socket.headers.get("origin", "")
        from urllib.parse import urlsplit

        if host not in {"127.0.0.1", "localhost", "testserver"} or (
            origin and urlsplit(origin).hostname not in {"127.0.0.1", "localhost"}
        ):
            await socket.close(code=1008)
            return
        await socket.accept()
        subscriber = queue.Queue(maxsize=12)
        with monitor.lock:
            monitor.subscribers.add(subscriber)
        try:
            while True:
                try:
                    packet = await asyncio.to_thread(subscriber.get, True, 0.5)
                    await socket.send_json(packet)
                except queue.Empty:
                    await socket.send_json({"type": "heartbeat"})
        except (WebSocketDisconnect, RuntimeError):
            pass
        finally:
            with monitor.lock:
                monitor.subscribers.discard(subscriber)

    @app.post("/api/source", response_model=Status)
    def start(config: SourceConfig):
        try:
            return monitor.start(config)
        except (ValueError, RuntimeError) as exc:
            raise HTTPException(400, str(exc)) from exc

    @app.post("/api/stop", response_model=Status)
    def stop():
        return monitor.stop()

    dist = ROOT / "web/dist"
    if dist.is_dir():
        app.mount("/", StaticFiles(directory=dist, html=True), name="console")
    return app


def main():
    parser = argparse.ArgumentParser(description="Run the local receive-only radio console.")
    parser.add_argument("--port", type=int, default=8765)
    args = parser.parse_args()
    import uvicorn

    uvicorn.run(create_app(), host="127.0.0.1", port=args.port)


if __name__ == "__main__":
    main()
