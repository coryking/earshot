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
  --out output/live-acceptance        (identity check: refuses unless the Pluto reports firmware v0.37)
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import datetime as dt
import json
import math
import os
import re
import subprocess
import sys
import time
import wave
from pathlib import Path

import httpx
import websockets

sys.path.insert(0, str(Path(__file__).parent))
from pluto_readback import mismatches, restore_receiver, snapshot
from radio_lock import ensure_borrowed, radio_lock, raise_on_termination

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
    "ServerAliveInterval=5",
    "-o",
    "LogLevel=ERROR",
]
EXPECTED_FIRMWARE = "v0.37"  # the ARM producer is built against this firmware's uClibc sysroot


def pluto_env() -> dict[str, str]:
    # The ADI factory default; set PLUTO_SSH_PASSWORD for a device whose password was changed.
    return {**os.environ, "SSHPASS": os.environ.get("PLUTO_SSH_PASSWORD", "analog")}


def pluto(host: str, command: str, timeout: float = 30) -> str:
    """Run a command on the Pluto. Cleanup-safe: failures come back as an empty/error string."""
    try:
        result = subprocess.run(
            [*SSH, f"root@{host}", command],
            check=False,
            env=pluto_env(),
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except (subprocess.SubprocessError, OSError) as exc:
        return f"<ssh failed: {exc!r}>"
    out = result.stdout.strip()
    err = result.stderr.strip()
    return out + (("\n" + err) if result.returncode and err else "")


def producers(host: str) -> int | None:
    """Number of running producers, or None when the Pluto could not be asked."""
    first = pluto(host, "ps | grep -c '[p]luto-channel-source'; true").split()
    return int(first[0]) if first and first[0].isdigit() else None


def identity(host: str, uri: str) -> dict[str, str]:
    """What the Pluto is actually running now (the FPGA agent may change images)."""
    info = subprocess.run(
        ["iio_info", "-u", uri], capture_output=True, text=True, timeout=30, check=False
    ).stdout
    keep = ("hw_model:", "hw_model_variant:", "fw_version:", "ip,ip-addr:")
    return {
        "uname": pluto(host, "uname -a"),
        "iio_context": "; ".join(
            line.strip() for line in info.splitlines() if line.strip().startswith(keep)
        ),
    }


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

    def __init__(self, base: str, ws_base: str, seconds: float):
        self.base, self.ws_base, self.seconds = base, ws_base, seconds
        self.task_errors: list[str] = []
        self.poll_errors = 0
        self.pcm_mismatches = 0
        self.rejected: set[str] = set()
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
                        "age_s": (time.time() - s["last_samples_at"])
                        if s.get("last_samples_at")
                        else None,
                        "source_pos": s.get("source_position_s"),
                        "rf_active": sum(1 for c in s["channels"] if c["rf_active"]),
                        "slice_active": sum(1 for c in s["channels"] if c.get("slice_rf_active")),
                        "channels": len(s["channels"]),
                        "admitted": s["source"].get("active_decoders"),
                        "metadata_only": s["source"].get("metadata_only_streams"),
                        "last_gap": s["source"].get("last_gap"),
                        "catalog": [
                            {
                                k: c.get(k)
                                for k in (
                                    "id",
                                    "protocol",
                                    "slot",
                                    "rf_active",
                                    "level_db",
                                    "decoded",
                                    "speech_active",
                                    "audio_seconds",
                                    "stream_id",
                                    "decoder_admitted",
                                    "kind",
                                    "frequency_hz",
                                    "frequency_snapped",
                                    "reading_hz",
                                    "carrier_offset_hz",
                                    "label",
                                )
                            }
                            for c in s["channels"]
                            if c["rf_active"] or c.get("decoded")
                        ],
                    }
                )
                if self.pinned is None:
                    for c in s["channels"]:
                        if (
                            c["protocol"] != "Unknown"
                            and c["decoded"]
                            and c["id"] not in self.rejected
                        ):
                            r = await client.post(
                                f"{self.base}/api/monitor", json={"channel_id": c["id"]}
                            )
                            if r.status_code == 200:
                                self.pinned = c["id"]
                                self.pin_result = {"t": self.now(), "response": r.json()}
                                break
                            self.rejected.add(c["id"])
                            self.pin_rejections.append(f"{r.status_code} {r.text}")
                else:
                    lst = (await client.get(f"{self.base}/api/listener")).json()
                    self.pin_result = {**(self.pin_result or {}), "listener": lst}
                sp = (
                    await client.get(
                        f"{self.base}/api/spectrum", params={"after": self.spectrum_seq}
                    )
                ).json()
                if sp["rows"]:
                    self.spectrum_rows += len(sp["rows"])
                    self.spectrum_seq = sp["sequence"]
                    self.spectrum_meta = {
                        k: sp[k] for k in ("center_hz", "span_hz", "unit", "source")
                    }
                    self.spectrum_meta["bins"] = len(sp["rows"][0])
                    lo = min(min(r) for r in sp["rows"])
                    hi = max(max(r) for r in sp["rows"])
                    self.spectrum_ranges.append((lo, hi))
            except Exception as exc:  # noqa: BLE001 - record and keep observing
                self.poll_errors += 1
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
                    self.event_log.append(
                        {"t": self.now(), **{k: e[k] for k in e if k != "channel"}}
                    )

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
                    if meta is None or meta["samples"] * 2 != len(msg):
                        self.pcm_mismatches += 1
                        meta = None
                        continue
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
            for name, result in zip(
                ("poll", "events websocket", "listen websocket"),
                await asyncio.gather(*tasks, return_exceptions=True),
                strict=True,
            ):
                if isinstance(result, BaseException):
                    self.task_errors.append(f"{name}: {result!r}")


def percentile(values: list[float], q: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(q * len(ordered)))]


def parse_args() -> argparse.Namespace:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    ap.add_argument("--host", default="192.168.10.10")
    ap.add_argument("--binary", type=Path, required=True, help="ARM pluto-channel-source")
    ap.add_argument("--engine", type=Path, default=Path("engine/target/release/radio-engine"))
    ap.add_argument("--web-dir", type=Path, default=Path("web/dist"))
    ap.add_argument("--seconds", type=float, default=180, help="observation time, 1..210")
    ap.add_argument(
        "--forward", default="1,50,57", help="producer stream IDs to forward IQ for (max 8)"
    )
    ap.add_argument("--center", type=int, default=454216998)
    ap.add_argument("--rate", type=int, default=800000)
    ap.add_argument("--port", type=int, default=18810)
    ap.add_argument(
        "--freq-correction-ppm", type=float, default=23.3, help="Pluto reads this many ppm high"
    )
    ap.add_argument("--labels", type=Path, default=Path("engine/data/channel-labels.json"))
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--lock-timeout", type=float, default=3600)
    ap.add_argument(
        "--allow-firmware",
        action="store_true",
        help=f"run even if the Pluto does not report firmware {EXPECTED_FIRMWARE}",
    )
    args = ap.parse_args()
    # The producer's own --seconds limit is 300; stay well inside it so it is always bounded.
    if not 1 <= args.seconds <= 210:
        ap.error("--seconds must be 1..210")
    if not re.fullmatch(r"[0-9]+(,[0-9]+){0,7}", args.forward):
        ap.error("--forward must be up to 8 comma-separated stream IDs")
    return args


def wait_for_engine(engine: subprocess.Popen, base: str, timeout: float = 20) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if engine.poll() is not None:
            raise RuntimeError(f"engine exited early with {engine.returncode}")
        with contextlib.suppress(httpx.HTTPError):
            if httpx.get(f"{base}/api/status", timeout=2).status_code == 200:
                return
        time.sleep(0.5)
    raise RuntimeError("engine did not answer /api/status")


def summarize(obs: Observer, report: dict, stop_response: httpx.Response, final: dict) -> None:
    samples = obs.status_samples
    ages = [s["age_s"] for s in samples if s["age_s"] is not None]
    report["post_stop_source"] = {
        k: final["source"].get(k) for k in ("receiver_state", "control_ack", "last_gap")
    }
    report["post_stop_source"]["streams"] = len(final["source"].get("streams") or [])
    report["engine_final"] = {
        "state": final["state"],
        "dropped_batches": final["dropped_batches"],
        "processed_batches": final["processed_batches"],
        "catalog_size": len(final["channels"]),
        "catalog": [
            {
                k: c.get(k)
                for k in (
                    "id",
                    "protocol",
                    "slot",
                    "decoded",
                    "kind",
                    "audio_seconds",
                    "level_db",
                    "last_rf",
                    "stream_id",
                )
            }
            for c in final["channels"]
            if c.get("decoded") or c.get("last_rf")
        ],
        "source_capabilities": final["source"].get("capabilities"),
        "source_config": {
            k: final["source"].get(k)
            for k in (
                "input_center_hz",
                "input_rate_hz",
                "input_bandwidth_hz",
                "hardware_counter",
                "psd",
            )
        },
    }
    report["browser_path"] = {
        "seconds_observed": obs.seconds,
        "status_polls": len(samples),
        "poll_errors": obs.poll_errors,
        "task_errors": obs.task_errors,
        "pcm_metadata_mismatches": obs.pcm_mismatches,
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
    report["stop"] = {
        "http_status": stop_response.status_code,
        "body_status": stop_response.json().get("status")
        if stop_response.status_code == 200
        else None,
        "detail": stop_response.text[:300],
    }


def household_api_summary(base: str) -> dict:
    """What the household UI's endpoints returned after the run (calls, catalog, activity, RSP1)."""
    out: dict = {}
    try:
        calls = httpx.get(f"{base}/api/calls?limit=50", timeout=5).json()
        out["calls"] = {
            "count": len(calls["calls"]),
            "in_progress": calls["in_progress"],
            "first": calls["calls"][0] if calls["calls"] else None,
            "total_audio_s": sum(c["duration_s"] for c in calls["calls"]),
        }
        catalog = httpx.get(f"{base}/api/catalog", timeout=5).json()
        out["catalog"] = [
            {
                k: s.get(k)
                for k in ("id", "name", "frequency_hz", "mode", "traffic_kind", "channel_ids")
            }
            for s in catalog["sources"]
        ]
        activity = httpx.get(f"{base}/api/activity?window_s=3600&bucket_s=300", timeout=5).json()
        out["activity"] = {
            "talk_seconds": sum(activity["block"]),
            "rf_seconds": sum(activity["rf_total"]),
            "sources": {k: sum(v) for k, v in activity["sources"].items() if sum(v)},
        }
        rsp = httpx.get(f"{base}/api/spectrum.bin?after=0&limit=5", timeout=5)
        out["rsp1"] = {"magic": rsp.content[:4].decode(errors="replace"), "bytes": len(rsp.content)}
    except Exception as exc:  # noqa: BLE001 - reported, not fatal
        out["error"] = repr(exc)
    return out


def acceptance_failures(report: dict) -> list[str]:
    """Pass criteria. Audio and RF activity are reported, not required: the air may be quiet."""
    out: list[str] = []
    browser = report.get("browser_path")
    if not browser:
        return ["no browser-path data collected"]
    if browser["spectrum"]["rows"] == 0:
        out.append("no spectrum rows reached the browser path")
    if browser["task_errors"]:
        out.append(f"observer tasks failed: {browser['task_errors']}")
    if browser["poll_errors"]:
        out.append(f"{browser['poll_errors']} status/spectrum poll errors")
    if browser["pcm_metadata_mismatches"]:
        out.append("audio PCM/metadata mismatch")
    stop = report.get("stop", {})
    post = report.get("post_stop_source", {})
    if stop.get("http_status") != 200 or stop.get("body_status") != "stopped":
        out.append(f"STOP not acknowledged as stopped: {stop}")
    if post.get("receiver_state") != "stopped" or not (post.get("control_ack") or {}).get("ok"):
        out.append(f"producer did not acknowledge stop/restoration: {post}")
    return out


def run_session(args: argparse.Namespace, out: Path, report: dict, failures: list[str]) -> None:
    uri = f"ip:{args.host}"
    report["identity"] = identity(args.host, uri)
    print("identity:", json.dumps(report["identity"]), flush=True)
    if EXPECTED_FIRMWARE not in report["identity"]["iio_context"] and not args.allow_firmware:
        raise RuntimeError(
            f"Pluto does not report firmware {EXPECTED_FIRMWARE} (image may have changed): "
            f"{report['identity']['iio_context']!r}; pass --allow-firmware to run anyway"
        )
    original = snapshot(uri)
    report["original"] = original
    print("original:", json.dumps(original), flush=True)
    running = producers(args.host)
    if running != 0:
        raise RuntimeError(
            f"cannot start: producers on the Pluto = {running!r} (not touching them)"
        )
    producer_started = False
    engine: subprocess.Popen | None = None
    engine_log = open(out / "engine.log", "w")  # noqa: SIM115 - closed in finally below
    base = f"http://127.0.0.1:{args.port}"
    try:
        copy_to_pluto(args.host, args.binary, "/tmp/pluto-channel-source")
        pluto(args.host, "chmod +x /tmp/pluto-channel-source")
        producer_started = True  # from here a producer may exist; cleanup owns it
        pluto(
            args.host,
            "cd /tmp && (nohup ./pluto-channel-source "
            f"--seconds {int(args.seconds + 90)} --rate {args.rate} --bandwidth {args.rate} "
            f"--center {args.center} --forward-streams {args.forward} "
            "> /tmp/producer.log 2>&1 &) ; sleep 1; echo started",
        )
        for _ in range(20):  # LISTEN on :9010 (0x2332) without connecting to it
            if ":2332 00000000:0000 0A" in pluto(args.host, "cat /proc/net/tcp").replace("  ", " "):
                break
            time.sleep(0.5)
        else:
            raise RuntimeError(
                "producer is not listening on 9010: " + pluto(args.host, "cat /tmp/producer.log")
            )
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
                "--freq-correction-ppm",
                str(args.freq_correction_ppm),
                "--labels",
                str(args.labels),
                "--state-dir",
                str(out / "state"),
            ],
            stdout=engine_log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        wait_for_engine(engine, base)
        obs = Observer(base, f"ws://127.0.0.1:{args.port}", args.seconds)
        asyncio.run(obs.run())
        # Acknowledged stop with the engine still connected.
        t_stop = time.monotonic()
        stop_response = httpx.post(f"{base}/api/stop", json={}, timeout=15)
        report["stop_seconds"] = time.monotonic() - t_stop
        final = httpx.get(f"{base}/api/status", timeout=5).json()
        summarize(obs, report, stop_response, final)
        report["household_api"] = household_api_summary(base)
        (out / "status_samples.json").write_text(json.dumps(obs.status_samples))
        if obs.audio_pcm:
            with wave.open(str(out / "audio.wav"), "wb") as w:
                w.setnchannels(1)
                w.setsampwidth(2)
                w.setframerate(8000)
                w.writeframes(bytes(obs.audio_pcm))
            report["browser_path"]["audio"]["wav"] = str(out / "audio.wav")
        failures.extend(acceptance_failures(report))
    except BaseException as exc:  # noqa: BLE001 - restore hardware before anything else
        failures.append(repr(exc))
        if engine is not None and engine.poll() is None:
            with contextlib.suppress(Exception):
                httpx.post(f"{base}/api/stop", json={}, timeout=15)
        if isinstance(exc, (KeyboardInterrupt, SystemExit)):
            report["interrupted"] = repr(exc)
    finally:
        cleanup(args, report, failures, original, uri, engine, engine_log, producer_started)


def cleanup(
    args: argparse.Namespace,
    report: dict,
    failures: list[str],
    original: dict[str, str],
    uri: str,
    engine: subprocess.Popen | None,
    engine_log: object,
    producer_started: bool,
) -> None:
    """Every step runs even if an earlier one fails; failures are recorded, never raised."""

    def step(name: str, fn) -> None:
        try:
            fn()
        except BaseException as exc:  # noqa: BLE001
            failures.append(f"cleanup {name}: {exc!r}")

    def stop_engine() -> None:
        if engine is None:
            return
        engine.terminate()
        try:
            engine.wait(timeout=10)
        except subprocess.TimeoutExpired:
            engine.kill()
            engine.wait(timeout=5)

    step("engine", stop_engine)
    step("engine log", engine_log.close)
    if producer_started:

        def stop_producer() -> None:
            time.sleep(1)
            report["producer_log_tail"] = pluto(args.host, "tail -c 1500 /tmp/producer.log")
            for sig in (None, "TERM", "KILL"):
                if sig:
                    pluto(
                        args.host,
                        f"kill -{sig} $(ps | grep '[p]luto-channel-source' | awk '{{print $1}}')",
                    )
                for _ in range(10):
                    if producers(args.host) == 0:
                        report["producer_exited"] = sig or "on its own"
                        return
                    time.sleep(1)
            report["producer_exited"] = False
            failures.append("producer did not exit")

        step("producer", stop_producer)
        if report.get("producer_exited"):
            step(
                "remove producer files",
                lambda: pluto(args.host, "rm -f /tmp/pluto-channel-source /tmp/producer.log"),
            )
            report["pluto_tmp_after"] = pluto(args.host, "ls /tmp")

    def verify() -> None:
        after = snapshot(uri)
        bad = mismatches(original, after)
        if bad:
            report["corrective_restore_errors"] = restore_receiver(uri, original)
            after = snapshot(uri)
            bad = mismatches(original, after)
        report["restored"] = after
        report["restoration_mismatch"] = bad
        print("restored:", json.dumps(after), flush=True)
        print("restoration mismatch:", json.dumps(bad), flush=True)
        if bad:
            failures.append(f"restoration readback differs: {bad}")

    step("restoration readback", verify)


def main() -> int:
    args = parse_args()
    ensure_borrowed()  # the always-on receiver steps aside, and resumes afterwards
    raise_on_termination()
    stamp = dt.datetime.now(dt.UTC).strftime("%Y%m%dT%H%M%SZ")
    out = args.out / stamp
    out.mkdir(parents=True, exist_ok=True)
    report: dict = {"started": stamp, "args": {k: str(v) for k, v in vars(args).items()}}
    failures: list[str] = []
    try:
        with radio_lock(args.lock_timeout):
            report["lock_acquired_utc"] = dt.datetime.now(dt.UTC).isoformat()
            try:
                run_session(args, out, report, failures)
            except BaseException as exc:  # noqa: BLE001 - before the producer was started
                failures.append(repr(exc))
    finally:
        report["failures"] = failures
        (out / "report.json").write_text(json.dumps(report, indent=1, default=str))
    print(json.dumps(report.get("browser_path", {}), indent=1, default=str)[:6000])
    print("failures:", failures, "report:", out / "report.json")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
