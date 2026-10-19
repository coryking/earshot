import re
from pathlib import Path

import numpy as np
from pydantic import BaseModel
from scipy.io import wavfile
from scipy.signal import correlate, find_peaks, welch


class Peak(BaseModel):
    frequency_hz: float
    relative_db: float


class Repeat(BaseModel):
    period_ms: float
    correlation: float


class Inspection(BaseModel):
    path: str
    sample_rate_hz: int
    channels: int
    duration_s: float
    analyzed_s: float
    recording_frequency_hz: int | None
    rms_dbfs: float
    peak_dbfs: float
    dc_offset: float
    near_full_scale_fraction: float
    exact_silence_fraction: float
    spectral_peaks: list[Peak]
    repeats: list[Repeat]
    warnings: list[str]


def load_audio(path: Path, seconds: float = 30, channel: int | None = None):
    if not np.isfinite(seconds) or seconds <= 0:
        raise ValueError("Analysis duration must be positive and finite.")
    rate, raw = wavfile.read(path)
    if rate <= 0:
        raise ValueError("WAV sample rate must be positive.")
    if rate > 192000 or path.name.lower().startswith("baseband_"):
        raise ValueError("This command expects demodulated audio, not baseband/IQ WAV.")
    channels = 1 if raw.ndim == 1 else raw.shape[1]
    duration = len(raw) / rate
    if not len(raw):
        raise ValueError("WAV contains no samples.")
    raw = raw[: max(1, int(seconds * rate))]
    if channels > 1:
        if channel is None:
            raise ValueError("Multichannel WAV: select --channel 0 or 1; IQ must not be averaged.")
        if channel < 0 or channel >= channels:
            raise ValueError(f"Channel must be between 0 and {channels - 1}.")
        raw = raw[:, channel]
    elif channel not in (None, 0):
        raise ValueError("Mono WAV only has channel 0.")
    if np.issubdtype(raw.dtype, np.unsignedinteger):
        x = (raw.astype(float) - 128) / 128
    elif np.issubdtype(raw.dtype, np.signedinteger):
        x = raw.astype(float) / (2 ** (np.iinfo(raw.dtype).bits - 1))
    elif np.issubdtype(raw.dtype, np.floating):
        x = raw.astype(float)
    else:
        raise ValueError(f"Unsupported WAV format: {raw.dtype}")
    if not np.all(np.isfinite(x)):
        raise ValueError("WAV contains non-finite samples.")
    return rate, x, channels, duration


def inspect(path: Path, seconds: float = 30, channel: int | None = None):
    rate, x, channels, duration = load_audio(path, seconds, channel)
    warnings = ["Timing and spectral peaks are observations, not protocol identification."]
    near_full = float(np.mean(np.abs(x) >= 0.999))
    if near_full > 0.001:
        warnings.append("Samples approach full scale; possible clipping. Lower recording volume.")
    silence = float(np.mean(x == 0))
    if silence > 0.1:
        warnings.append(
            "Substantial exact silence; squelch, a stopped source, or silence may be present."
        )
    centered = x - np.mean(x)
    f, power = welch(centered, rate, nperseg=min(len(x), rate * 2))
    indices, _ = find_peaks(power)
    indices = sorted(indices, key=lambda i: power[i], reverse=True)[:10]
    reference = max(float(np.max(power)), 1e-30)
    peaks = [
        Peak(
            frequency_hz=float(f[i]),
            relative_db=float(10 * np.log10(max(power[i], 1e-30) / reference)),
        )
        for i in indices
    ]
    # Normalize by zero-lag energy. No overlap correction: longer lags remain conservative.
    ac = correlate(centered, centered, method="fft", mode="full")[len(x) - 1 :]
    repeats = []
    if ac[0] > 1e-20:
        ac /= ac[0]
        start, end = max(1, int(rate * 0.005)), min(len(ac), int(rate * 0.2) + 1)
        ids, _ = find_peaks(ac[start:end], height=0.1, distance=max(1, int(rate * 0.001)))
        ids = sorted(ids + start, key=lambda i: ac[i], reverse=True)[:8]
        repeats = [Repeat(period_ms=float(i / rate * 1000), correlation=float(ac[i])) for i in ids]
    match = re.search(r"_(\d+)Hz", path.name)
    db = lambda value: float(20 * np.log10(max(float(value), 1e-15)))
    return Inspection(
        path=str(path.resolve()),
        sample_rate_hz=rate,
        channels=channels,
        duration_s=duration,
        analyzed_s=len(x) / rate,
        recording_frequency_hz=int(match[1]) if match else None,
        rms_dbfs=db(np.sqrt(np.mean(x * x))),
        peak_dbfs=db(np.max(np.abs(x))),
        dc_offset=float(np.mean(x)),
        near_full_scale_fraction=near_full,
        exact_silence_fraction=silence,
        spectral_peaks=peaks,
        repeats=repeats,
        warnings=warnings,
    )


def plot_audio(path: Path, destination: Path, seconds: float, channel: int | None):
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    rate, x, _, _ = load_audio(path, seconds, channel)
    result = inspect(path, seconds, channel)
    fig, axes = plt.subplots(3, 1, figsize=(12, 10), layout="constrained")
    stride = max(1, len(x) // 100000)
    axes[0].plot(np.arange(len(x))[::stride] / rate, x[::stride], linewidth=0.4)
    axes[0].set(xlabel="Time (s)", ylabel="Amplitude (full scale)", title=path.name)
    f, power = welch(x - np.mean(x), rate, nperseg=min(len(x), rate * 2))
    axes[1].plot(f, 10 * np.log10(np.maximum(power, 1e-30)))
    axes[1].set(
        xlabel="Audio frequency (Hz)", ylabel="PSD (dB FS²/Hz)", xlim=(0, min(10000, rate / 2))
    )
    if len(x) >= 256:
        with np.errstate(divide="ignore"):
            axes[2].specgram(x, NFFT=256, Fs=rate, noverlap=192, cmap="magma")
        axes[2].set(
            ylim=(0, min(10000, rate / 2)), xlabel="Time (s)", ylabel="Audio frequency (Hz)"
        )
    else:
        axes[2].text(0.1, 0.5, "Too few samples for a spectrogram", transform=axes[2].transAxes)
    fig.suptitle(
        f"Analyzed {result.analyzed_s:.2f}s | near full scale {result.near_full_scale_fraction:.1%} | protocol unconfirmed"
    )
    fig.savefig(destination, dpi=150)
    plt.close(fig)
