"""Sparse, memory-bounded spectrum surveys of wideband IQ WAVs."""

import re
from dataclasses import dataclass
from pathlib import Path

import numpy as np
from pydantic import BaseModel
from scipy.io import wavfile
from scipy.ndimage import binary_closing, median_filter
from scipy.signal.windows import hann


class Candidate(BaseModel):
    kind: str
    offset_hz: float
    frequency_hz: float | None
    low_offset_hz: float
    high_offset_hz: float
    width_hz: float
    peak_offset_hz: float
    peak_dbfs_per_hz: float
    excess_local_floor_db: float
    excess_global_floor_db: float
    sampled_occupancy: float
    flags: list[str]


class Survey(BaseModel):
    path: str
    sample_rate_hz: int
    center_frequency_hz: float | None
    duration_s: float
    surveyed_span_s: float
    sampled_duration_s: float
    windows: int
    fft_size: int
    bin_width_hz: float
    sampled_power_dbfs: float
    sampled_near_full_scale_fraction: float
    candidates_total: int
    candidates: list[Candidate]
    notes: list[str]


@dataclass
class SurveyData:
    result: Survey
    offsets: np.ndarray
    mean_psd: np.ndarray
    local_floor: np.ndarray
    window_psd: np.ndarray
    times: np.ndarray


def db(values):
    return 10 * np.log10(np.maximum(values, 1e-30))


def regions(mask):
    mask = binary_closing(mask, structure=np.ones(3))
    boundaries = np.diff(np.r_[False, mask, False].astype(int))
    return zip(np.flatnonzero(boundaries == 1), np.flatnonzero(boundaries == -1), strict=True)


def survey_iq(path: Path, seconds=30.0, center_hz=None, fft_size=16384, windows=240, limit=20):
    if not np.isfinite(seconds) or not 0.001 <= seconds <= 120:
        raise ValueError("Survey span must be between 0.001 and 120 seconds.")
    if not 1024 <= fft_size <= 65536 or fft_size & (fft_size - 1):
        raise ValueError("FFT size must be a power of two between 1024 and 65536.")
    if not 4 <= windows <= 512 or not 1 <= limit <= 100:
        raise ValueError("Use 4..512 windows and 1..100 candidates.")
    if center_hz is not None and (not np.isfinite(center_hz) or center_hz <= 0):
        raise ValueError("Center frequency must be positive and finite.")
    rate, raw = wavfile.read(path, mmap=True)
    if raw.ndim != 2 or raw.shape[1] != 2 or raw.dtype != np.dtype("int16") or rate <= 192000:
        raise ValueError("Expected two int16 I/Q channels at a rate above 192 kHz.")
    count = min(len(raw), int(seconds * rate))
    if count < fft_size * 4:
        raise ValueError(
            "Survey requires at least four FFT windows. Increase --seconds or reduce --fft-size."
        )
    notes = [
        "Power uses complex I+jQ / 32768: unit complex magnitude is 0 dBFS relative to the int16 container. Raw 12-bit ADC units and scaled SDR audio have different levels; density is not calibrated RF dBm.",
        "Evenly spaced nonoverlapping Hann windows sample the selected span. Occupancy refers only to these windows; short bursts can be missed.",
        "Times come from file sample indices; acquisition continuity and dropped buffers are not verified.",
        "Local floor is a 100 kHz frequency median of the temporal median PSD divided by ln(2), a white-noise estimator.",
        "Candidates exceed local floor by 8 dB with at least 5% sampled occupancy. Broad humps exceed global floor by 12 dB over at least max(25 kHz,4% of span). These are heuristics, not protocol identification.",
        "Narrow regions at DC and the outer 5% of the span are flagged as possible artifacts. Wider regions overlapping center are flagged separately. Narrow carriers and broad humps can also be receiver artifacts.",
    ]
    if center_hz is None:
        match = re.search(r"_(\d+)Hz", path.name)
        if match:
            center_hz = float(match[1])
            notes.append(
                "Center frequency inferred from filename metadata, not measured RF tuning."
            )
        else:
            notes.append(
                "No center frequency supplied or found in filename; candidates report offsets only."
            )
    windows = min(windows, count // fft_size)
    starts = np.linspace(0, count - fft_size, windows, dtype=np.int64)
    window = hann(fft_size, sym=False)
    scale = rate * np.sum(window * window)
    powers = np.empty((windows, fft_size), dtype=np.float32)
    saturated = 0
    for row, start in enumerate(starts):
        block = raw[start : start + fft_size]
        saturated += int(np.count_nonzero(np.abs(block.astype(np.int32)) >= 32760))
        z = (block[:, 0].astype(float) + 1j * block[:, 1]) / 32768
        powers[row] = np.fft.fftshift(np.abs(np.fft.fft(z * window)) ** 2 / scale)
    mean = powers.mean(axis=0, dtype=np.float64)
    offsets = np.fft.fftshift(np.fft.fftfreq(fft_size, 1 / rate))
    bin_width = rate / fft_size
    floor_width = min(fft_size - 1, max(5, int(100000 / bin_width) | 1))
    floor = median_filter(np.median(powers, axis=0) / np.log(2), size=floor_width, mode="reflect")
    floor = np.maximum(floor, 1e-30)
    global_floor = max(float(np.percentile(floor, 30)), 1e-30)
    local_excess = db(mean / floor)
    global_excess = db(mean / global_floor)
    occupancy = np.mean(powers > floor * 10**0.8, axis=0)
    global_occupancy = np.mean(powers > global_floor * 10**1.2, axis=0)
    candidates = []
    masks = [
        (local_excess >= 8) & (occupancy >= 0.05),
        (global_excess >= 12) & (global_occupancy >= 0.05),
    ]
    for broad, mask in enumerate(masks):
        for low, high in regions(mask):
            width = (high - low) * bin_width
            if broad and width < max(25000, rate * 0.04):
                continue
            peak = low + int(np.argmax(mean[low:high]))
            midpoint = float((offsets[low] + offsets[high - 1]) / 2)
            flags = []
            if offsets[low] <= max(1000, 2 * bin_width) and offsets[high - 1] >= -max(
                1000, 2 * bin_width
            ):
                flags.append(
                    "dc_or_center_artifact_possible"
                    if width <= max(1500, 4 * bin_width)
                    else "overlaps_capture_center"
                )
            if max(abs(offsets[low]), abs(offsets[high - 1])) >= rate * 0.45:
                flags.append("edge_artifact_possible")
            kind = (
                "broad_hump_or_passband_shape"
                if broad
                else ("narrow_carrier" if width <= max(1500, 4 * bin_width) else "occupied_region")
            )
            candidates.append(
                Candidate(
                    kind=kind,
                    offset_hz=midpoint,
                    frequency_hz=center_hz + midpoint if center_hz else None,
                    low_offset_hz=float(offsets[low] - bin_width / 2),
                    high_offset_hz=float(offsets[high - 1] + bin_width / 2),
                    width_hz=width,
                    peak_offset_hz=float(offsets[peak]),
                    peak_dbfs_per_hz=float(db(mean[peak])),
                    excess_local_floor_db=float(local_excess[peak]),
                    excess_global_floor_db=float(global_excess[peak]),
                    sampled_occupancy=float(
                        np.max(global_occupancy[low:high] if broad else occupancy[low:high])
                    ),
                    flags=flags,
                )
            )
    candidates.sort(
        key=lambda c: max(c.excess_local_floor_db, c.excess_global_floor_db) * c.sampled_occupancy,
        reverse=True,
    )
    result = Survey(
        path=str(path.resolve()),
        sample_rate_hz=rate,
        center_frequency_hz=center_hz,
        duration_s=len(raw) / rate,
        surveyed_span_s=count / rate,
        sampled_duration_s=windows * fft_size / rate,
        windows=windows,
        fft_size=fft_size,
        bin_width_hz=bin_width,
        sampled_power_dbfs=float(db(np.sum(mean) * bin_width)),
        sampled_near_full_scale_fraction=saturated / (windows * fft_size * 2),
        candidates_total=len(candidates),
        candidates=candidates[:limit],
        notes=notes,
    )
    return SurveyData(result, offsets, mean, floor, powers, starts / rate)


def plot_survey(data: SurveyData, output: Path):
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    result = data.result
    center = result.center_frequency_hz
    axis = (data.offsets + center) / 1e6 if center else data.offsets / 1000
    xlabel = (
        "RF frequency (MHz; supplied/filename center)"
        if center
        else "Offset from capture center (kHz)"
    )
    fig, axes = plt.subplots(2, 1, figsize=(12, 8), layout="constrained", sharex=True)
    axes[0].plot(axis, db(data.mean_psd), linewidth=0.7, label="Mean sampled PSD")
    axes[0].plot(axis, db(data.local_floor), linewidth=0.7, label="Estimated local noise floor")
    axes[0].set(ylabel="Density (dBFS/Hz)", title=Path(result.path).name)
    axes[0].legend(loc="lower left")
    levels = db(data.window_psd)
    image = axes[1].imshow(
        levels,
        origin="lower",
        aspect="auto",
        extent=[axis[0], axis[-1], data.times[0], data.times[-1]],
        cmap="magma",
        vmin=float(np.percentile(levels, 15)),
        vmax=float(np.percentile(levels, 99.7)),
    )
    axes[1].set(xlabel=xlabel, ylabel="Window start time (s)")
    fig.colorbar(image, ax=axes[1], label="dBFS/Hz")
    fig.suptitle(
        f"Sparse IQ survey: {result.windows} windows / {result.surveyed_span_s:.2f}s span; protocol unknown"
    )
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("xb") as stream:
        fig.savefig(stream, format="png", dpi=130)
    plt.close(fig)
