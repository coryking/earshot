"""Exclusive lease on the Pluto for scripts that touch the radio.

Every session that configures, captures or streams (and restores afterwards) holds
/tmp/pluto-radio.lock, shared with the other agents that use the same device. Hold it only
for the radio session itself and release it between runs.

The always-on receiver (amateur-radio-live) holds that lock whenever it is live. A script that
needs the radio calls `ensure_borrowed()` first: it re-runs the script under
offload/radio-borrow, which stops the live service (restoring the Pluto and releasing the lock),
runs the script, and starts the live service again afterwards.
"""

from __future__ import annotations

import contextlib
import fcntl
import os
import signal
import sys
import time
from collections.abc import Iterator
from pathlib import Path

LOCK_PATH = os.environ.get("PLUTO_RADIO_LOCK", "/tmp/pluto-radio.lock")
BORROW = Path(__file__).resolve().parent / "radio-borrow"


@contextlib.contextmanager
def radio_lock(timeout_s: float = 3600.0) -> Iterator[None]:
    """Block (polling) until the lock is free; raise TimeoutError after `timeout_s`.

    Under `radio-borrow --lock` (PLUTO_RADIO_LOCK_HELD=1) the lock is held around this process;
    that claim is checked, not trusted: if nobody actually holds the lock the usual path runs.
    PLUTO_RADIO_LOCK (the lock file path) exists for tests; every real user must share the default.
    """
    with open(LOCK_PATH, "a") as handle:
        if os.environ.get("PLUTO_RADIO_LOCK_HELD") == "1":
            try:
                fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                yield  # held by our parent, as claimed
                return
            # Nobody holds it: the flag is stale. We now hold the lock; fall through and keep it.
        else:
            _acquire(handle, timeout_s)
        try:
            yield
        finally:
            fcntl.flock(handle, fcntl.LOCK_UN)


def _acquire(handle: object, timeout_s: float) -> None:
    deadline = time.monotonic() + timeout_s
    while True:
        try:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return
        except BlockingIOError:
            if time.monotonic() > deadline:
                raise TimeoutError(f"{LOCK_PATH} still held after {timeout_s:.0f} s") from None
            time.sleep(1.0)


def raise_on_termination() -> None:
    """Turn SIGTERM/SIGHUP into SystemExit so `finally` blocks restore the radio first."""

    def handler(signum: int, _frame: object) -> None:
        raise SystemExit(128 + signum)

    for sig in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, handler)


def ensure_borrowed(argv: list[str] | None = None) -> None:
    """Step the always-on live service aside for the rest of this process.

    Replaces the process with `radio-borrow python <script> <args>` unless it is already running
    under it. Returns normally when already borrowed, or when there is no radio-borrow to run.
    The caller takes the radio lock itself afterwards, as before.
    """
    if os.environ.get("PLUTO_RADIO_BORROWED") == "1" or not BORROW.is_file():
        return
    command = argv if argv is not None else [sys.executable, *sys.argv]
    os.execv(str(BORROW), [str(BORROW), *command])
