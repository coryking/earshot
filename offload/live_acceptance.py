"""Bounded live acceptance: Pluto ARM producer -> Rust engine -> browser WebSocket path.

RX only. Holds /tmp/pluto-radio.lock for the whole session. The producer runs from the
Pluto's volatile /tmp (nothing is flashed or written to flash). Sequence:

  1. snapshot original receiver settings, FIR enables and TX clock (iio_attr, read-only)
  2. copy the ARM binary to the Pluto's /tmp and start it (it opens RX only once the engine connects)
  3. start a private engine instance wired to it (separate ports; the systemd service is untouched)
  4. observe /api/status, /api/spectrum, /api/events and the /api/listen WebSocket for --seconds;
     pin the first confirmed voice channel and record its audio
  5. POST /api/stop and require the acknowledged restoration, then read the hardware back
  6. kill and delete the producer, and compare readbacks with the snapshot

  uv run python offload/live_acceptance.py --binary ~/.cache/amateur-radio-arm/build/pluto-channel-source
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import datetime as dt
import json
import math
import os
import subprocess
import sys
import time
import wave
from pathlib import Path

import httpx
import websockets

sys.path.insert(0, str(Path(__file__).parent))
from capture_wideband import RESTORED_KEYS, snapshot  # noqa: E402
from radio_lock import radio_lock  # noqa: E402

SSH = [
    "sshpass",
    "-e",
    "ssh",
    "-o",
    "PubkeyAuthentication=no",
    "-o",
    "StrictHostKeyChecking=accept-new",
    "-o",
    "ConnectTimeout=8",
    "-o",
    "LogLevel=ERROR",
]


def pluto_env() -> dict[str, str]:
    # The factory default; override for a device whose password was changed.
    return {**os.environ, "SSHPASS": os.environ.get("PLUTO_SSH_PASSWORD", "analog")}


def pluto(host: str, command: str, timeout: float = 30) -> str:
    result = subprocess.run(
        [*SSH, f"root@{host}", command],
        env=pluto_env(),
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    out = result.stdout.strip()
    err = result.stderr.strip()
    return out + (("\n" + err) if result.returncode and err else "")


def producers(host: str) -> int:
    first = pluto(host, "ps | grep -c '[p]luto-channel-source'; true").split()
    return int(first[0]) if first else 0


def copy_to_pluto(host: str, source: Path, target: str) -> None:
    subprocess.run(
        [
            "sshpass",
            "-e",
            "scp",
            "-O",
            "-o",
            "PubkeyAuthentication=no",
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-o",
            "LogLevel=ERROR",
            str(source),
            f"root@{host}:{target}",
        ],
        env=pluto_env(),
        check=True,
        timeout=120,
    )


class Observer:
    """Everything the browser path would see, plus freshness and gap metrics."""

    def __init__(self, base: str, ws_base: str, seconds: float, out: Path):
        self.base, self.ws_base, self.seconds, self.out = base, ws_base, seconds, out
        self.t0 = time.monotonic()
        self.status_samples: list[dict] = []
        self.spectrum_rows = 0
        self.spectrum_seq = 0
        self.spectrum_meta: dict = {}
        self.spectrum_ranges: list[tuple[float, float]] = []
        self.events: dict[str, int] = {}
        self.event_log: list[dict] = []
        self.audio_chunks = 0
        self.audio_samples = 0
        self.audio_sq = 0.0
        self.audio_peak = 0
        self.audio_ages: list[float] = []
        self.audio_pcm = bytearray()
        self.listen_gaps = 0
        self.listen_heartbeats = 0
        self.pinned: str | None = None
        self.pin_result: dict | None = None
        self.pin_rejections: list[str] = []
        self.first_audio_s: float | None = None
        self.stop = asyncio.Event()

    def now(self) -> float:
        return time.monotonic() - self.t0

    async def poll(self, client: httpx.AsyncClient) -> None:
        while not self.stop.is_set():
            try:
                s = (await client.get(f"{self.base}/api/status")).json()
                self.status_samples.append(
                    {
                        "t": self.now(),
                        "state": s["state"],
                        "error": s.get("error"),
                        "dropped": s["dropped_batches"],
                        "frames": s["processed_batches"],
                        "queue": s["queue_depth"],
                        "age_s": (time.time() - s["last_samples_at"]) if s.get("last_samples_at") else None,
                        "source_pos": s.get("source_position_s"),
                        "rf_active": sum(1 for c in s["channels"] if c["rf_active"]),
                        "slice_active": sum(1 for c in s["channels"] if c.get("slice_rf_active")),
                        "channels": len(s["channels"]),
                        "admitted": s["source"].get("active_decoders"),
                        "metadata_only": s["source"].get("metadata_only_streams"),
                        "last_gap": s["source"].get("last_gap"),
                        "catalog": [
                            {k: c.get(k) for k in ("id", "protocol", "slot", "rf_active", "level_db", "decoded", "speech_active", "audio_seconds", "stream_id", "decoder_admitted", "kind")}
                            for c in s["channels"]
                            if c["rf_active"] or c.get("decoded")
                        ],
                    }
                )
                if self.pinned is None:
                    for c in s["channels"]:
                        if c["protocol"] != "Unknown" and c["decoded"]:
                            r = await client.post(f"{self.base}/api/monitor", json={"channel_id": c["id"]})
                            if r.status_code == 200:
                                self.pinned = c["id"]
                                self.pin_result = {"t": self.now(), "response": r.json()}
                            else:
                                self.pin_rejections.append(f"{r.status_code} {r.text}")
                            break
                else:
                    lst = (await client.get(f"{self.base}/api/listener")).json()
                    self.pin_result = {**(self.pin_result or {}), "listener": lst}
                sp = (await client.get(f"{self.base}/api/spectrum", params={"after": self.spectrum_seq})).json()
                if sp["rows"]:
                    self.spectrum_rows += len(sp["rows"])
                    self.spectrum_seq = sp["sequence"]
                    self.spectrum_meta = {k: sp[k] for k in ("center_hz", "span_hz", "unit", "source")}
                    self.spectrum_meta["bins"] = len(sp["rows"][0])
                    lo = min(min(r) for r in sp["rows"])
                    hi = max(max(r) for r in sp["rows"])
                    self.spectrum_ranges.append((lo, hi))
            except Exception as exc:  # noqa: BLE001 - record and keep observing
                self.event_log.append({"t": self.now(), "poll_error": repr(exc)})
            await asyncio.sleep(1.0)

    async def events_ws(self) -> None:
        async with websockets.connect(f"{self.ws_base}/api/events", max_size=1 << 20) as ws:
            while not self.stop.is_set():
                try:
                    raw = await asyncio.wait_for(ws.recv(), timeout=1.0)
                except TimeoutError:
                    continue
                e = json.loads(raw)
                self.events[e["type"]] = self.events.get(e["type"], 0) + 1
                if e["type"] in ("source_gap", "source_state", "gap"):
                    self.event_log.append({"t": self.now(), **{k: e[k] for k in e if k != "channel"}})

    async def listen_ws(self) -> None:
        async with websockets.connect(f"{self.ws_base}/api/listen", max_size=1 << 20) as ws:
            meta = None
            while not self.stop.is_set():
                try:
                    msg = await asyncio.wait_for(ws.recv(), timeout=1.0)
                except TimeoutError:
                    continue
                if isinstance(msg, str):
                    m = json.loads(msg)
                    if m["type"] == "audio":
                        meta = m
                    elif m["type"] == "gap":
                        self.listen_gaps += 1
                    elif m["type"] == "heartbeat":
                        self.listen_heartbeats += 1
                else:
                    assert meta is not None and meta["samples"] * 2 == len(msg), "PCM/metadata mismatch"
                    self.audio_chunks += 1
                    self.audio_samples += meta["samples"]
                    self.audio_ages.append(meta["ingress_age_ms"])
                    if self.first_audio_s is None:
                        self.first_audio_s = self.now()
                    vals = memoryview(msg).cast("h")
                    self.audio_sq += sum(v * v for v in vals)
                    self.audio_peak = max(self.audio_peak, max((abs(v) for v in vals), default=0))
                    self.audio_pcm += msg
                    meta = None

    async def run(self) -> None:
        async with httpx.AsyncClient(timeout=5) as client:
            tasks = [
                asyncio.create_task(self.poll(client)),
                asyncio.create_task(self.events_ws()),
                asyncio.create_task(self.listen_ws()),
            ]
            await asyncio.sleep(self.seconds)
            self.stop.set()
            for t in tasks:
                with contextlib.suppress(Exception):
                    await t


def percentile(values: list[float], q: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(q * len(ordered)))]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    ap.add_argument("--host", default="192.168.10.10")
    ap.add_argument("--binary", type=Path, required=True, help="ARM pluto-channel-source")
    ap.add_argument("--engine", type=Path, default=Path("engine/target/release/radio-engine"))
    ap.add_argument("--web-dir", type=Path, default=Path("web/dist"))
    ap.add_argument("--seconds", type=float, default=180)
    ap.add_argument("--forward", default="1,50,57", help="producer stream IDs to forward IQ for (max 8)")
    ap.add_argument("--center", type=int, default=454216998)
    ap.add_argument("--rate", type=int, default=800000)
    ap.add_argument("--port", type=int, default=18810)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--lock-timeout", type=float, default=3600)
    args = ap.parse_args()
    stamp = dt.datetime.now(dt.UTC).strftime("%Y%m%dT%H%M%SZ")
    out = args.out / stamp
    out.mkdir(parents=True, exist_ok=True)
    report: dict = {"started": stamp, "args": {k: str(v) for k, v in vars(args).items()}}
    engine = None
    failures: list[str] = []
    with radio_lock(args.lock_timeout):
        report["lock_acquired_utc"] = dt.datetime.now(dt.UTC).isoformat()
        original = snapshot(f"ip:{args.host}")
        report["original"] = original
        print("original:", json.dumps(original), flush=True)
        try:
            if producers(args.host):
                raise RuntimeError("a producer already runs on the Pluto")
            copy_to_pluto(args.host, args.binary, "/tmp/pluto-channel-source")
            pluto(args.host, "chmod +x /tmp/pluto-channel-source")
            # The producer's own bound is at most 300 s; 0 means until stopped (this script's
            # finally block still stops and verifies it).
            bound = int(args.seconds + 90)
            bound = bound if bound <= 300 else 0
            pluto(
                args.host,
                "cd /tmp && (nohup ./pluto-channel-source "
                f"--seconds {bound} --rate {args.rate} --bandwidth {args.rate} --center {args.center} "
                f"--forward-streams {args.forward} > /tmp/producer.log 2>&1 &) ; sleep 1; echo started",
            )
            for _ in range(20):  # LISTEN on :9010 (0x2332) without connecting to it
                if ":2332 00000000:0000 0A" in pluto(args.host, "cat /proc/net/tcp").replace("  ", " "):
                    break
                time.sleep(0.5)
            else:
                raise RuntimeError("producer is not listening on 9010: " + pluto(args.host, "cat /tmp/producer.log"))
            engine = subprocess.Popen(
                [
                    str(args.engine),
                    "serve",
                    "--source",
                    f"tcp:{args.host}:9010",
                    "--port",
                    str(args.port),
                    "--max-decoders",
                    "8",
                    "--web-dir",
                    str(args.web_dir),
                ],
                stdout=open(out / "engine.log", "w"),
                stderr=subprocess.STDOUT,
            )
            time.sleep(3)
            base = f"http://127.0.0.1:{args.port}"
            obs = Observer(base, f"ws://127.0.0.1:{args.port}", args.seconds, out)
            asyncio.run(obs.run())
            # Acknowledged stop with the engine still connected.
            t_stop = time.monotonic()
            stop_response = httpx.post(f"{base}/api/stop", json={}, timeout=15)
            report["stop"] = {
                "http_status": stop_response.status_code,
                "seconds": time.monotonic() - t_stop,
                "body_status": stop_response.json().get("status") if stop_response.status_code == 200 else None,
                "detail": stop_response.text[:500],
            }
            final = httpx.get(f"{base}/api/status", timeout=5).json()
            report["post_stop_source"] = {
                k: final["source"].get(k) for k in ("receiver_state", "control_ack", "streams", "last_gap")
            }
            report["post_stop_source"]["streams"] = len(final["source"].get("streams") or [])
            report["engine_final"] = {
                "state": final["state"],
                "dropped_batches": final["dropped_batches"],
                "processed_batches": final["processed_batches"],
                "catalog_size": len(final["channels"]),
                "catalog": [
                    {k: c.get(k) for k in ("id", "protocol", "slot", "decoded", "kind", "audio_seconds", "level_db", "last_rf", "stream_id")}
                    for c in final["channels"]
                    if c.get("decoded") or c.get("last_rf")
                ],
                "source_capabilities": final["source"].get("capabilities"),
                "source_config": {k: final["source"].get(k) for k in ("input_center_hz", "input_rate_hz", "input_bandwidth_hz", "hardware_counter", "psd")},
                "n_streams_declared": None,
            }
            samples = obs.status_samples
            ages = [s["age_s"] for s in samples if s["age_s"] is not None]
            report["browser_path"] = {
                "seconds_observed": obs.seconds,
                "status_polls": len(samples),
                "spectrum": {
                    "rows": obs.spectrum_rows,
                    "rows_per_s": obs.spectrum_rows / obs.seconds,
                    **obs.spectrum_meta,
                    "value_range_min_max": [
                        min((r[0] for r in obs.spectrum_ranges), default=None),
                        max((r[1] for r in obs.spectrum_ranges), default=None),
                    ],
                },
                "events": obs.events,
                "event_log_head": obs.event_log[:30],
                "freshness_s": {
                    "median": percentile(ages, 0.5),
                    "p95": percentile(ages, 0.95),
                    "max": max(ages, default=None),
                },
                "dropped_batches_end": samples[-1]["dropped"] if samples else None,
                "max_queue_depth": max((s["queue"] or 0) for s in samples) if samples else None,
                "max_rf_active_channels": max((s["rf_active"] for s in samples), default=0),
                "max_slice_active_channels": max((s["slice_active"] for s in samples), default=0),
                "decoders_admitted_max": max((s["admitted"] or 0) for s in samples) if samples else None,
                "metadata_only_streams": samples[-1]["metadata_only"] if samples else None,
                "catalog_seen": sorted({c["id"] for s in samples for c in s["catalog"]}),
                "pin": {
                    "pinned_channel": obs.pinned,
                    "result": obs.pin_result,
                    "rejections": obs.pin_rejections[:5],
                },
                "audio": {
                    "chunks": obs.audio_chunks,
                    "samples": obs.audio_samples,
                    "seconds_at_8khz": obs.audio_samples / 8000,
                    "rms": math.sqrt(obs.audio_sq / obs.audio_samples) if obs.audio_samples else None,
                    "peak": obs.audio_peak,
                    "ingress_age_ms_median": percentile(obs.audio_ages, 0.5),
                    "ingress_age_ms_p95": percentile(obs.audio_ages, 0.95),
                    "ingress_age_ms_max": max(obs.audio_ages, default=None),
                    "first_audio_at_s": obs.first_audio_s,
                    "listen_gap_messages": obs.listen_gaps,
                    "listen_heartbeats": obs.listen_heartbeats,
                },
            }
            (out / "status_samples.json").write_text(json.dumps(samples))
            if obs.audio_pcm:
                with wave.open(str(out / "audio.wav"), "wb") as w:
                    w.setnchannels(1)
                    w.setsampwidth(2)
                    w.setframerate(8000)
                    w.writeframes(bytes(obs.audio_pcm))
                report["browser_path"]["audio"]["wav"] = str(out / "audio.wav")
            if stop_response.status_code != 200:
                failures.append(f"stop not acknowledged: HTTP {stop_response.status_code}")
        except BaseException as exc:  # noqa: BLE001 - restore hardware before anything else
            failures.append(repr(exc))
            with contextlib.suppress(Exception):
                if engine:
                    httpx.post(f"http://127.0.0.1:{args.port}/api/stop", json={}, timeout=15)
        finally:
            if engine:
                engine.terminate()
                with contextlib.suppress(Exception):
                    engine.wait(timeout=10)
            time.sleep(1)
            # The producer restores on stop/disconnect/signal; confirm it exited, else TERM it.
            report["producer_log_tail"] = pluto(args.host, "tail -c 1500 /tmp/producer.log")
            for _ in range(10):
                if not producers(args.host):
                    break
                time.sleep(1)
            else:
                pluto(args.host, "kill -TERM $(ps | grep '[p]luto-channel-source' | awk '{print $1}')")
                time.sleep(3)
            report["producer_exited"] = not producers(args.host)
            pluto(args.host, "rm -f /tmp/pluto-channel-source /tmp/producer.log")
            report["pluto_tmp_after"] = pluto(args.host, "ls /tmp")
            after = snapshot(f"ip:{args.host}")
            report["restored"] = after
            mismatch = {k: [original[k], after[k]] for k in RESTORED_KEYS if original[k] != after[k]}
            report["restoration_mismatch"] = mismatch
            print("restored:", json.dumps(after), flush=True)
            print("restoration mismatch:", json.dumps(mismatch), flush=True)
            if mismatch:
                failures.append(f"restoration readback differs: {mismatch}")
    report["failures"] = failures
    (out / "report.json").write_text(json.dumps(report, indent=1, default=str))
    print(json.dumps(report.get("browser_path", {}), indent=1, default=str)[:6000])
    print("failures:", failures, "report:", out / "report.json")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
