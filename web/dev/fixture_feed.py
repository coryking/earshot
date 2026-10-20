#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["numpy>=2", "typer>=0.15"]
# ///
"""Dev-only RIQ1 fixture feeder for frontend work. Not a production path.

Sends the same 25 ksps CI16 channel fixture as engine/examples/fixture_ingress.rs,
plus PSD rows measured from the 6 Msps recording the fixture was cut from, so the
waterfall shows real recorded RF. Activity frames carry the channel's measured
level. Loops so the UI has something to show. Receive-only; never touches hardware.

Example:
  web/dev/fixture_feed.py --ingest 127.0.0.1:8821 \\
    --iq output/native-handoff/dmr-slot2-25ksps-ci16le.raw \\
    --wideband recordings/baseband_454216998Hz_wide-30s_20261019T215324Z.wav --loops 0

Exit codes: 0 done, 1 connection or file error, 2 bad usage.
"""

import json
import socket
import struct
import sys
import time
from pathlib import Path

import numpy as np
import typer

CENTER = 454_216_998
WIDE_RATE = 6_000_000
TARGET = 454_029_223
RATE = 25_000
BINS = 1024
ROW_S = 0.1


def frame(kind: int, sequence: int, sample: int, payload: bytes, rate: int = RATE, stream: int = 1) -> bytes:
    fmt = {1: 1, 2: 2}.get(kind, 3)
    header = struct.pack(
        "<4sHHHHQIQQIqIHHI",
        b"RIQ1", 1, 64, kind, 0, 1, stream if kind != 4 else 0, sequence, sample, rate,
        CENTER if kind == 2 else TARGET, len(payload), fmt, 16, 0,
    )
    assert len(header) == 64
    return header + payload


def psd_rows(path: Path, seconds: float) -> list[np.ndarray]:
    """dBFS PSD rows, 1024 bins, one per 100 ms, from a 6 Msps interleaved s16 WAV."""
    data = np.memmap(path, dtype="<i2", mode="r", offset=44)
    window = np.hanning(BINS).astype(np.float32)
    norm = (2048.0 * window.sum()) ** 2
    rows = []
    per_row = int(WIDE_RATE * ROW_S)
    for r in range(int(seconds / ROW_S)):
        start = r * per_row
        acc = np.zeros(BINS, dtype=np.float64)
        n = 24
        for k in range(n):
            o = (start + k * (per_row // n)) * 2
            chunk = np.asarray(data[o : o + BINS * 2], dtype=np.float32)
            z = (chunk[0::2] + 1j * chunk[1::2]) * window
            acc += np.abs(np.fft.fftshift(np.fft.fft(z))) ** 2
        rows.append((10 * np.log10(acc / n / norm + 1e-20)).astype("<f4"))
    return rows


def main(
    ingest: str = typer.Option("127.0.0.1:8821", help="Engine fixture ingest HOST:PORT (never the Pluto)."),
    iq: Path = typer.Option(..., exists=True, help="25 ksps CI16 channel fixture."),
    wideband: Path | None = typer.Option(None, help="6 Msps WAV the fixture was cut from, for PSD rows."),
    loops: int = typer.Option(1, help="Times to play; 0 = forever."),
    pause: float = typer.Option(3.0, help="Seconds between loops."),
) -> None:
    raw = iq.read_bytes()
    seconds = len(raw) / 4 / RATE
    rows = psd_rows(wideband, seconds) if wideband else []
    print(f"fixture {seconds:.1f}s, {len(rows)} PSD rows", file=sys.stderr)
    host, port = ingest.rsplit(":", 1)
    config = {
        "input_center_hz": CENTER, "input_rate_hz": WIDE_RATE, "input_bandwidth_hz": WIDE_RATE,
        "hardware_counter": False, "capabilities": {"controls_supported": False},
        "psd": {"unit": "dBFS", "bins": BINS},
        "streams": [{"stream_id": 1, "center_hz": TARGET, "target_hz": TARGET, "rate_hz": RATE,
                     "protocol": "Unknown", "slot": 1}],
    }
    played = 0
    while loops == 0 or played < loops:
        try:
            sock = socket.create_connection((host, int(port)), timeout=5)
        except OSError as error:
            print(f"cannot connect to {ingest}: {error}", file=sys.stderr)
            raise typer.Exit(1)
        sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        sock.sendall(frame(4, 0, 0, json.dumps(config).encode()))
        time.sleep(1)
        started = time.monotonic()
        sequence = 0
        step = int(RATE * ROW_S)
        for i, offset in enumerate(range(0, len(raw), step * 4)):
            chunk = raw[offset : offset + step * 4]
            sample = offset // 4
            iqv = np.frombuffer(chunk, dtype="<i2").astype(np.float32)
            level = float(10 * np.log10(np.mean(iqv**2) / 32768**2 + 1e-12))
            activity = json.dumps({"rf_active": level > -60, "level_db": round(level, 1)}).encode()
            out = frame(1, sequence, sample, chunk) + frame(3, sequence + 1, sample, activity)
            sequence += 2
            if i < len(rows):
                out += frame(2, sequence, int(i * ROW_S * WIDE_RATE), rows[i].tobytes(), rate=WIDE_RATE)
                sequence += 1
            sock.sendall(out)
            delay = (sample + step) / RATE - (time.monotonic() - started)
            if delay > 0:
                time.sleep(delay)
        time.sleep(0.5)
        sock.close()
        played += 1
        print(f"loop {played} done", file=sys.stderr)
        if loops == 0 or played < loops:
            time.sleep(pause)


if __name__ == "__main__":
    typer.run(main)
