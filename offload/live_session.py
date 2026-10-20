"""A bounded live session with settings of your own (centre, rate, forwarded streams).

The public service is live all the time now (amateur-radio-live + amateur-radio), so you only
need this to listen with different settings than the service's. It runs under radio-borrow: the
always-on receiver steps aside first and resumes afterwards. For acceptance testing see
live_acceptance.py. The session

  1. takes /tmp/pluto-radio.lock (other agents' radio sessions wait),
  2. checks the Pluto's firmware, snapshots its receiver state, copies the ARM producer to the
     Pluto's volatile /tmp and starts it,
  3. stops the public `amateur-radio` engine service and starts a live engine on its
     port (127.0.0.1:8810, so the tailnet URL serves live data),
  4. waits for --minutes (or Ctrl-C / SIGTERM), then asks the engine to STOP (acknowledged
     RX restoration), stops the producer, reads the hardware back (corrective restore if
     needed), and restarts the public engine service.

    uv run python offload/live_session.py --binary ~/.cache/amateur-radio-arm/build/pluto-channel-source --minutes 60

The session ends when the producer or connection ends (this private engine is not
supervised); the cleanup path still runs. RX only.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import subprocess
import sys
import time
from pathlib import Path

import httpx

sys.path.insert(0, str(Path(__file__).parent))
import live_acceptance as la
from pluto_readback import snapshot
from radio_lock import ensure_borrowed, radio_lock, raise_on_termination

SERVICE = "amateur-radio"


def systemctl(*args: str) -> int:
    return subprocess.run(["systemctl", "--user", *args], check=False, timeout=60).returncode


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--host", default="192.168.10.10")
    ap.add_argument("--binary", type=Path, required=True)
    ap.add_argument("--engine", type=Path, default=Path("engine/target/release/radio-engine"))
    ap.add_argument("--web-dir", type=Path, default=Path("web/dist"))
    ap.add_argument("--minutes", type=float, default=60, help="1..240")
    ap.add_argument("--forward", default="1,50,57")
    ap.add_argument("--center", type=int, default=454216998)
    ap.add_argument("--rate", type=int, default=800000)
    ap.add_argument("--port", type=int, default=8810)
    ap.add_argument("--freq-correction-ppm", type=float, default=23.3)
    ap.add_argument("--labels", type=Path, default=Path("engine/data/channel-labels.json"))
    ap.add_argument("--lock-timeout", type=float, default=3600)
    ap.add_argument("--allow-firmware", action="store_true")
    args = ap.parse_args()
    if not 1 <= args.minutes <= 240:
        ap.error("--minutes must be 1..240")
    ensure_borrowed()  # the always-on receiver steps aside, and resumes afterwards
    raise_on_termination()
    uri = f"ip:{args.host}"
    report: dict = {}
    failures: list[str] = []
    engine = None
    producer_started = False
    service_stopped = False
    original: dict[str, str] = {}
    log = open(Path("/tmp") / "amateur-radio-live-engine.log", "w")  # noqa: SIM115 - closed in cleanup
    try:
        with radio_lock(args.lock_timeout):
            try:
                report["identity"] = la.identity(args.host, uri)
                print("identity:", json.dumps(report["identity"]), flush=True)
                if (
                    la.EXPECTED_FIRMWARE not in report["identity"]["iio_context"]
                    and not args.allow_firmware
                ):
                    raise RuntimeError(
                        f"Pluto firmware is not {la.EXPECTED_FIRMWARE}: {report['identity']}"
                    )
                original = snapshot(uri)
                if la.producers(args.host) != 0:
                    raise RuntimeError(
                        "a producer is already running on the Pluto; not touching it"
                    )
                la.copy_to_pluto(args.host, args.binary, "/tmp/pluto-channel-source")
                la.pluto(args.host, "chmod +x /tmp/pluto-channel-source")
                producer_started = True
                bound = int(args.minutes * 60 + 90)
                la.pluto(
                    args.host,
                    "cd /tmp && (nohup ./pluto-channel-source "
                    f"--seconds {bound if bound <= 300 else 0} --rate {args.rate} --bandwidth {args.rate} "
                    f"--center {args.center} --forward-streams {args.forward} "
                    "> /tmp/producer.log 2>&1 &) ; sleep 1",
                )
                for _ in range(20):
                    if ":2332 00000000:0000 0A" in la.pluto(args.host, "cat /proc/net/tcp").replace(
                        "  ", " "
                    ):
                        break
                    time.sleep(0.5)
                else:
                    raise RuntimeError("producer is not listening on 9010")
                service_stopped = systemctl("stop", SERVICE) == 0
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
                    ],
                    stdout=log,
                    stderr=subprocess.STDOUT,
                    start_new_session=True,
                )
                base = f"http://127.0.0.1:{args.port}"
                la.wait_for_engine(engine, base)
                print(
                    f"live on {base} (tailnet URL via tailscale serve) for {args.minutes:g} min",
                    flush=True,
                )
                deadline = time.monotonic() + args.minutes * 60
                while time.monotonic() < deadline and engine.poll() is None:
                    time.sleep(2)
                if engine.poll() is None:
                    stop = httpx.post(f"{base}/api/stop", json={}, timeout=15)
                    report["stop"] = {"http_status": stop.status_code, "detail": stop.text[:200]}
                    if stop.status_code != 200:
                        failures.append(f"STOP not acknowledged: {stop.status_code}")
                else:
                    failures.append(f"engine exited early with {engine.returncode}")
            except BaseException as exc:  # noqa: BLE001 - cleanup must still run
                failures.append(repr(exc))
                if engine is not None and engine.poll() is None:
                    with contextlib.suppress(Exception):
                        httpx.post(f"http://127.0.0.1:{args.port}/api/stop", json={}, timeout=15)
            finally:
                if original:
                    la.cleanup(args, report, failures, original, uri, engine, log, producer_started)
                else:
                    with contextlib.suppress(Exception):
                        log.close()
    finally:
        # Always bring the public engine service back, even if the radio session failed.
        if service_stopped or engine is not None:
            systemctl("start", SERVICE)
    print(json.dumps({"failures": failures, **report}, indent=1, default=str))
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
