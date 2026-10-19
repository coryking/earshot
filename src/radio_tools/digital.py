"""Bounded IQ channelization and optional DMR voice decoding."""

import subprocess
from math import gcd
from pathlib import Path

import numpy as np
from pydantic import BaseModel
from scipy.io import wavfile
from scipy.signal import firwin, lfilter, resample_poly

from .analysis import load_audio


class FMResult(BaseModel):
    input: str
    output: str
    input_rate_hz: int
    offset_hz: float
    channel_bandwidth_hz: float
    analyzed_s: float
    output_rate_hz: int = 48000
    output_samples: int
    notes: list[str]


def iq_fm(path: Path, seconds: float, offset_hz: float, bandwidth_hz: float = 12500):
    if not np.isfinite(seconds) or not 0.001 <= seconds <= 60:
        raise ValueError("IQ duration must be between 0.001 and 60 seconds.")
    if not np.isfinite(offset_hz):
        raise ValueError("Offset must be finite.")
    if not np.isfinite(bandwidth_hz) or not 1000 <= bandwidth_hz <= 40000:
        raise ValueError("Channel bandwidth must be between 1000 and 40000 Hz.")
    rate, raw = wavfile.read(path, mmap=True)
    if raw.ndim != 2 or raw.shape[1] != 2 or raw.dtype != np.dtype("int16") or rate <= 192000:
        raise ValueError(
            "Expected wideband IQ WAV: two int16 I/Q channels and rate above 192 kHz. Audio WAV is unsupported here."
        )
    # Keep the complete channel, including its filter transition, inside the capture.
    if abs(offset_hz) + bandwidth_hz / 2 + 2500 >= rate / 2:
        raise ValueError(
            "Target channel and 2500 Hz filter guard must fit inside the IQ capture's Nyquist bandwidth."
        )
    count = min(len(raw), int(seconds * rate))
    if count < max(2, rate // 1000):
        raise ValueError("IQ WAV must contain at least 1 ms of samples.")
    x = raw[:count, 0].astype(np.complex64)
    x.imag = raw[:count, 1]
    # Bound the oscillator allocation as well as the mapped input window.
    for start in range(0, count, 1000000):
        stop = min(count, start + 1000000)
        x[start:stop] *= np.exp(-2j * np.pi * offset_hz * np.arange(start, stop) / rate)
    divisor = gcd(rate, 50000)
    y = resample_poly(x, 50000 // divisor, rate // divisor)
    y = lfilter(firwin(161, bandwidth_hz / 2, fs=50000), [1.0], y)
    fm = np.angle(y[1:] * np.conj(y[:-1]))
    fm -= np.mean(fm)
    fm = resample_poly(fm, 24, 25)
    # 5 kHz nominal deviation -> half-scale-ish discriminator PCM, as verified with DSDcc.
    pcm = np.clip(fm * 50000 / (2 * np.pi * 5000) * 16000, -32768, 32767).astype("<i2")
    return rate, count / rate, pcm


class DMRResult(BaseModel):
    input: str
    output: str
    decoder: str
    slot: int
    analyzed_s: float
    output_rate_hz: int = 8000
    output_samples: int
    output_duration_s: float
    status: str
    diagnostics: str
    notes: list[str]


def dmr_audio(path: Path, binary: str, slot: int, seconds: float, channel: int | None):
    if slot not in (1, 2):
        raise ValueError("DMR slot must be 1 or 2.")
    rate, x, _, _ = load_audio(path, seconds, channel)
    if rate != 48000:
        raise ValueError(
            "DMR input must be 48000 Hz demodulated audio. Use radio iq-fm for IQ captures."
        )
    pcm = np.clip(x * 32768, -32768, 32767).astype("<i2").tobytes()
    proc = subprocess.run(
        [binary, "-i", "-", "-o", "-", "-fr", "-T", str(slot), "-q"],
        input=pcm,
        capture_output=True,
        timeout=120,
        check=False,
    )
    if proc.returncode:
        raise ValueError(
            f"DSDcc exited {proc.returncode}: {proc.stderr.decode(errors='replace')[:1000]}"
        )
    if len(proc.stdout) % 2:
        raise ValueError("DSDcc returned an incomplete 16-bit PCM sample.")
    return (
        len(x) / rate,
        np.frombuffer(proc.stdout, dtype="<i2"),
        proc.stderr.decode(errors="replace")[:16000],
    )
