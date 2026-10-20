"""Exclusive lease on the Pluto for scripts that touch the radio.

Every session that configures, captures or streams (and restores afterwards) holds
/tmp/pluto-radio.lock, shared with the other agents that use the same device. Hold it only
for the radio session itself and release it between runs.
"""

from __future__ import annotations

import contextlib
import fcntl
import time
from collections.abc import Iterator

LOCK_PATH = "/tmp/pluto-radio.lock"


@contextlib.contextmanager
def radio_lock(timeout_s: float = 3600.0) -> Iterator[None]:
    """Block (polling) until the lock is free; raise TimeoutError after `timeout_s`."""
    with open(LOCK_PATH, "a") as handle:
        deadline = time.monotonic() + timeout_s
        while True:
            try:
                fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() > deadline:
                    raise TimeoutError(f"{LOCK_PATH} still held after {timeout_s:.0f} s") from None
                time.sleep(1.0)
        try:
            yield
        finally:
            fcntl.flock(handle, fcntl.LOCK_UN)
