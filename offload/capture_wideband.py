"""Receive-only wideband IQ capture from a Pluto, written as SigMF (ci16_le).

Each *buffer* capture is one libiio refill of one DMA block, so its samples are
contiguous in hardware. Separate captures are NOT contiguous with each other.
A *stream* capture refills one buffer repeatedly; stock Pluto DMA has no sample
counter, so continuity across refills is not proven (the sidecar records the
wall time of every refill so a stall is visible).

RX only, and the session holds /tmp/pluto-radio.lock. The original sample rate, bandwidth,
LO and gain mode are written back on every exit path (each write attempted independently,
SIGTERM/SIGHUP included), then rate, bandwidth, LO, gain mode, both FIR enables and the TX
baseband clock/LO are read back and compared. The FIR enables and TX clock are verified, not
written: the driver keeps the TX clock tied to the RX rate. A mismatch triggers one
corrective restore through iio_attr and a second readback.

    uv run python offload/capture_wideband.py --uri ip:192.168.10.10 \
        --out output/wideband-iq --buffers 12 --stream-seconds 8
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import sys
import time
from pathlib import Path

import numpy as np

from radio_tools.pluto import PlutoReceiver

sys.path.insert(0, str(Path(__file__).parent))
from pluto_readback import iio_attr, mismatches, restore_receiver, snapshot
from radio_lock import radio_lock, raise_on_termination

LO_HZ = 454_216_998


def sigmf_meta(
    *,
    rate: int,
    lo: int,
    datatype_bits: int,
    applied: dict,
    note: str,
    started: str,
    stats: dict,
    kind: str,
    extra: dict,
) -> dict:
    return {
        "global": {
            "core:datatype": "ci16_le",
            "core:sample_rate": rate,
            "core:version": "1.0.0",
            "core:hw": "ADALM-PLUTO Rev.B (Z7010/AD9363), original ADI firmware v0.37, libiio over ip",
            "core:description": note,
            "core:recorder": "offload/capture_wideband.py",
            "radio:valid_bits": datatype_bits,
            "radio:capture_kind": kind,
            "radio:applied": applied,
            "radio:stats": stats,
            **extra,
        },
        "captures": [{"core:sample_start": 0, "core:frequency": lo, "core:datetime": started}],
        "annotations": [],
    }


def stats_of(raw: bytes) -> dict:
    a = np.frombuffer(raw, dtype="<i2").astype(np.float64)
    i, q = a[0::2], a[1::2]
    return {
        "samples": int(i.size),
        "rms": float(np.sqrt(np.mean(i * i + q * q) / 2)),
        "peak_abs": int(np.max(np.abs(a))),
        "mean_i": float(i.mean()),
        "mean_q": float(q.mean()),
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    ap.add_argument("--uri", default="ip:192.168.10.10")
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--rate", type=int, default=25_600_000)
    ap.add_argument("--bandwidth", type=int, default=20_000_000)
    ap.add_argument("--center", type=int, default=LO_HZ)
    ap.add_argument("--buffer-samples", type=int, default=4_194_304)
    ap.add_argument("--buffers", type=int, default=12)
    ap.add_argument("--stream-rate", type=int, default=0, help="0 disables the long capture")
    ap.add_argument("--stream-seconds", type=float, default=8)
    ap.add_argument("--stream-buffer-samples", type=int, default=262_144)
    ap.add_argument("--lock-timeout", type=float, default=3600)
    args = ap.parse_args()
    raise_on_termination()
    with radio_lock(args.lock_timeout):
        return session(args)


def session(args: argparse.Namespace) -> int:
    args.out.mkdir(parents=True, exist_ok=True)
    original = snapshot(args.uri)
    print("original:", json.dumps(original), flush=True)
    rx = PlutoReceiver(args.uri)
    errors: list[BaseException] = []
    manifest: dict = {"original": original, "captures": []}
    stamp = dt.datetime.now(dt.UTC).strftime("%Y%m%dT%H%M%SZ")

    def set_rx(rate: int):
        rx.write("sampling_frequency", str(rate))
        rx.write("rf_bandwidth", str(min(args.bandwidth, max(rate, 200_000))))
        rx.write("frequency", str(args.center))

    try:
        rx.open()
        set_rx(args.rate)
        ap_ = snapshot(args.uri)
        manifest["applied_wide"] = ap_
        print("applied:", json.dumps(ap_), flush=True)
        rate = int(ap_["rx_sampling_frequency"])
        for n in range(args.buffers):
            started = dt.datetime.now(dt.UTC).isoformat()
            t0 = time.monotonic()
            bits = rx.start(args.buffer_samples)
            t1 = time.monotonic()
            raw = rx.refill()
            t2 = time.monotonic()
            gain = iio_attr(args.uri, "-i", "-c", "ad9361-phy", "voltage0", "hardwaregain")
            rx.stop()
            name = f"pluto-{stamp}-lo{args.center}-{rate}sps-buf{n:02d}"
            (args.out / f"{name}.sigmf-data").write_bytes(raw)
            st = stats_of(raw)
            meta = sigmf_meta(
                rate=rate,
                lo=args.center,
                datatype_bits=bits,
                applied=ap_,
                note=(
                    "Single libiio refill of one DMA block: contiguous within this file. "
                    "No hardware sample counter exists, and separate files are NOT contiguous "
                    "with each other. AGC was left in the device's gain mode, so gain may "
                    "vary; hardwaregain read after refill is in extra."
                ),
                started=started,
                stats=st,
                kind="single_buffer",
                extra={
                    "radio:hardwaregain_after": gain,
                    "radio:buffer_samples": args.buffer_samples,
                    "radio:buffer_create_s": t1 - t0,
                    "radio:refill_s": t2 - t1,
                },
            )
            (args.out / f"{name}.sigmf-meta").write_text(json.dumps(meta, indent=2))
            manifest["captures"].append(name)
            print(
                f"buf{n:02d} {len(raw)} B refill {t2 - t1:.3f}s rms {st['rms']:.1f} peak {st['peak_abs']}",
                flush=True,
            )
        if args.stream_rate:
            set_rx(args.stream_rate)
            sp = snapshot(args.uri)
            srate = int(sp["rx_sampling_frequency"])
            target = int(srate * args.stream_seconds)
            started = dt.datetime.now(dt.UTC).isoformat()
            name = f"pluto-{stamp}-lo{args.center}-{srate}sps-stream"
            bits = rx.start(args.stream_buffer_samples)
            got = 0
            times: list[float] = []
            raw_all = bytearray()
            t0 = time.monotonic()
            while got < target:
                t = time.monotonic()
                raw = rx.refill()
                times.append(time.monotonic() - t)
                raw_all += raw
                got += len(raw) // 4
                if time.monotonic() - t0 > args.stream_seconds * 4 + 10:
                    break
            wall = time.monotonic() - t0
            rx.stop()
            (args.out / f"{name}.sigmf-data").write_bytes(bytes(raw_all))
            st = stats_of(bytes(raw_all))
            ratio = (got / srate) / wall
            meta = sigmf_meta(
                rate=srate,
                lo=args.center,
                datatype_bits=bits,
                applied=sp,
                note=(
                    "Streamed refills of one buffer. Contiguity across refills is NOT proven: "
                    "stock DMA has no sample counter. throughput_ratio<~0.95 means the link "
                    "could not keep up and samples were probably dropped between refills."
                ),
                started=started,
                stats=st,
                kind="stream",
                extra={
                    "radio:buffer_samples": args.stream_buffer_samples,
                    "radio:wall_s": wall,
                    "radio:throughput_ratio": ratio,
                    "radio:refills": len(times),
                    "radio:refill_max_s": max(times),
                    "radio:refill_median_s": float(np.median(times)),
                    "radio:refill_s": times,
                },
            )
            (args.out / f"{name}.sigmf-meta").write_text(json.dumps(meta, indent=2))
            manifest["captures"].append(name)
            print(
                f"stream {srate} sps {got} samples wall {wall:.2f}s ratio {ratio:.3f}", flush=True
            )
    except BaseException as exc:  # noqa: BLE001 - restore first, then re-raise
        errors.append(exc)
    finally:
        try:
            rx.stop()
        except BaseException as exc:  # noqa: BLE001
            errors.append(exc)
        # Restore through libiio in dependency order (rate first). Each write is attempted
        # on its own, so one failure cannot leave the later controls unrestored.
        for key, value in (
            ("sampling_frequency", original["rx_sampling_frequency"]),
            ("rf_bandwidth", original["rx_rf_bandwidth"]),
            ("frequency", original["rx_lo"]),
            ("gain_control_mode", original["rx_gain_control_mode"]),
        ):
            try:
                rx.write(key, value)
            except BaseException as exc:  # noqa: BLE001
                errors.append(exc)
        try:
            rx.close()
        except BaseException as exc:  # noqa: BLE001
            errors.append(exc)
        try:
            after = snapshot(args.uri)
            bad = mismatches(original, after)
            if bad:
                # One corrective pass through iio_attr, then read everything back again.
                fix_errors = restore_receiver(args.uri, original)
                manifest["corrective_restore_errors"] = fix_errors
                after = snapshot(args.uri)
                bad = mismatches(original, after)
            manifest["restored"] = after
            manifest["restoration_mismatch"] = bad
            print("restored:", json.dumps(after), flush=True)
            print("restoration mismatch:", json.dumps(bad), flush=True)
            if bad:
                errors.append(RuntimeError(f"restoration readback differs: {bad}"))
        except BaseException as exc:  # noqa: BLE001
            errors.append(exc)
    (args.out / f"pluto-{stamp}-manifest.json").write_text(json.dumps(manifest, indent=2))
    for e in errors:
        print("ERROR:", repr(e), file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main())
