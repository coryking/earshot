"""Read bounded ranges of signed16 IQ from WAV or SigMF without extracting archives."""

import json
import math
import tarfile
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path

import numpy as np
from scipy.io import wavfile


@dataclass
class IQRecording:
    rate: int
    samples: int
    captures: list[dict]
    raw: object
    is_sigmf: bool = False

    def center_at(self, start_s: float) -> float | None:
        if not math.isfinite(start_s) or start_s < 0:
            raise ValueError("Start time must be finite and nonnegative.")
        sample = int(start_s * self.rate)
        captures = [c for c in self.captures if c["core:sample_start"] <= sample]
        return captures[-1].get("core:frequency") if captures else None

    def read(self, start_s: float, seconds: float):
        if not math.isfinite(start_s) or start_s < 0:
            raise ValueError("Start time must be finite and nonnegative.")
        start = int(start_s * self.rate)
        stop = min(self.samples, start + int(seconds * self.rate))
        if start >= self.samples:
            raise ValueError("Start time is beyond the IQ recording.")
        for capture in self.captures:
            boundary = capture["core:sample_start"]
            if start < boundary < stop:
                raise ValueError(
                    f"Selected range crosses a capture boundary/discontinuity at "
                    f"{boundary / self.rate:.6f} s. Shorten --seconds or use --start-s "
                    "to select a continuous segment. Gaps are never silently joined."
                )
        if isinstance(self.raw, np.ndarray):
            return self.raw[start:stop]
        # A range in an archive is bounded even when the source capture is very large.
        if (stop - start) * 4 > 256 * 1024 * 1024:
            raise ValueError("SigMF range exceeds 256 MiB. Select a shorter --seconds range.")
        self.raw.seek(start * 4)
        data = self.raw.read((stop - start) * 4)
        if len(data) != (stop - start) * 4:
            raise ValueError("SigMF sample data is truncated.")
        return np.frombuffer(data, dtype="<i2").reshape(-1, 2)


def _metadata(data: bytes, data_size: int) -> tuple[int, list[dict]]:
    try:
        meta = json.loads(data)
        g = meta["global"]
        rate = g["core:sample_rate"]
        if g["core:datatype"] != "ci16_le":
            raise ValueError("SigMF input must use core:datatype ci16_le.")
        if not isinstance(rate, (int, float)) or not math.isfinite(rate) or rate != int(rate):
            raise ValueError("SigMF sample rate must be a finite integer.")
        if not 20000 <= rate <= 100000000:
            raise ValueError("SigMF IQ sample rate must be 20 kHz through 100 MHz.")
        if data_size % 4:
            raise ValueError("SigMF sample data ends with an incomplete I/Q pair.")
        captures = meta.get("captures", [])
        if not captures or captures[0].get("core:sample_start") != 0:
            raise ValueError("SigMF input needs a capture starting at sample 0.")
        boundaries = set()
        for c in captures:
            n = c["core:sample_start"]
            if not isinstance(n, int) or n < 0 or n >= data_size // 4:
                raise ValueError("SigMF capture sample_start is outside the data.")
            if n in boundaries:
                raise ValueError("SigMF has duplicate capture boundaries.")
            boundaries.add(n)
            freq = c.get("core:frequency")
            if freq is not None and (not isinstance(freq, (float, int)) or not math.isfinite(freq)):
                raise ValueError("SigMF capture frequency must be finite.")
        # Recorder gap annotations are boundaries even if a producer omitted a capture entry.
        for a in meta.get("annotations", []):
            if a.get("core:label") == "discontinuity":
                n = a["core:sample_start"]
                if not isinstance(n, int) or not 0 <= n < data_size // 4:
                    raise ValueError("SigMF discontinuity is outside the data.")
                if n not in boundaries:
                    previous = max(
                        (c for c in captures if c["core:sample_start"] <= n),
                        key=lambda c: c["core:sample_start"],
                    )
                    captures.append(dict(previous, **{"core:sample_start": n}))
                    boundaries.add(n)
        return int(rate), sorted(captures, key=lambda c: c["core:sample_start"])
    except (KeyError, TypeError, json.JSONDecodeError, AttributeError) as exc:
        raise ValueError(f"Invalid SigMF metadata: {exc}") from exc


@contextmanager
def open_iq(path: Path):
    """Recognize SigMF tar by content (including piped input), or .sigmf-meta/.sigmf-data."""
    if tarfile.is_tarfile(path):
        with tarfile.open(path) as archive:
            members = archive.getmembers()
            metas = [m for m in members if m.name.endswith(".sigmf-meta")]
            if len(metas) != 1:
                raise ValueError("SigMF archive must contain exactly one .sigmf-meta file.")
            meta = metas[0]
            data_name = meta.name.removesuffix(".sigmf-meta") + ".sigmf-data"
            data_members = [m for m in members if m.name == data_name]
            if len(data_members) != 1 or not meta.isfile() or not data_members[0].isfile():
                raise ValueError("SigMF archive needs one matching regular .sigmf-data file.")
            if meta.size > 1024 * 1024:
                raise ValueError("SigMF metadata exceeds 1 MiB.")
            rate, captures = _metadata(archive.extractfile(meta).read(), data_members[0].size)
            with archive.extractfile(data_members[0]) as samples:
                yield IQRecording(rate, data_members[0].size // 4, captures, samples, True)
        return
    if path.name.endswith((".sigmf-meta", ".sigmf-data")):
        stem = path.name.rsplit(".sigmf-", 1)[0]
        metadata = path.with_name(stem + ".sigmf-meta")
        samples = path.with_name(stem + ".sigmf-data")
        if metadata.stat().st_size > 1024 * 1024:
            raise ValueError("SigMF metadata exceeds 1 MiB.")
        rate, captures = _metadata(metadata.read_bytes(), samples.stat().st_size)
        raw = np.memmap(samples, mode="r", dtype="<i2").reshape(-1, 2)
        yield IQRecording(rate, len(raw), captures, raw, True)
        return
    rate, raw = wavfile.read(path, mmap=True)
    if raw.ndim != 2 or raw.shape[1] != 2 or raw.dtype != np.dtype("int16") or rate <= 192000:
        raise ValueError(
            "Expected IQ: two-channel int16 I/Q WAV above 192 kHz, or ci16_le SigMF. "
            "Audio WAV is unsupported here."
        )
    yield IQRecording(rate, len(raw), [], raw)
