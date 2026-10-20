"""Offline 4FSK symbol recovery and CRC/FEC checks for sampled IQ recordings.

The measurements are hypothesis checks for a selected baud rate. A good four-level
fit or an NXDN frame check is evidence about these samples, not a general protocol ID.
"""

from __future__ import annotations

import math
import shutil
import sys
import tempfile
from contextlib import contextmanager
from pathlib import Path
from typing import Annotated, Literal

import numpy as np
import typer
from pydantic import BaseModel
from scipy.signal import fftconvolve, firwin, lfilter

from .digital import channelize_iq
from .iq_recording import open_iq

app = typer.Typer(no_args_is_help=True, help="Recover symbols and check NXDN frame evidence.")
OUTPUT_HZ = 50000
RRC_ALPHA = 0.2
NXDN_FSW = 0xCDF59
NXDN_FSW_BITS = 20
DIBIT_TO_LEVEL = np.asarray([1, 3, -1, -3], dtype=np.int8)
LEVELS = np.asarray([-3, -1, 1, 3], dtype=np.int8)
DIBIT_BY_LEVEL_INDEX = np.asarray([3, 2, 0, 1], dtype=np.uint8)
MAX_REPORTED_HITS = 256
TYPE_D_LICH = {
    0x60,
    0x61,
    0x62,
    0x63,
    0x68,
    0x69,
    0x6E,
    0x6F,
    0x70,
    0x71,
    0x72,
    0x73,
    0x75,
    0x76,
    0x77,
}


class FrameCheck(BaseModel):
    symbol_index: int
    sync_word_hex: str = "0xCDF59"
    polarity: Literal["normal", "inverted"]
    lich_hex: str
    lich_parity_ok: bool
    lich_type: int
    type_d_eligible: bool
    scch_crc_ok: bool | None
    scch_crc_computed: int | None
    scch_crc_received: int | None
    payload_hex: str | None
    fec_hard_bit_cost: int | None


class FSWHit(BaseModel):
    symbol_index: int
    word_hex: str
    polarity: Literal["normal", "inverted"]


class SymbolResult(BaseModel):
    input: str
    target_hz: float | None
    center_hz: float | None
    offset_hz: float
    input_rate_hz: int
    analyzed_s: float
    start_s: float = 0
    requested_baud: float
    estimated_baud: float
    clock_phase_symbols: float
    rrc_alpha: float
    bandwidth_hz: float
    frequency_level_centers_hz: list[float]
    level_spacing_hz: float
    normalized_residual: float
    symbols_count: int
    nxdn_fsw_match_count: int
    nxdn_fsw_hits: list[FSWHit]
    frame_checks: list[FrameCheck]
    output_dir: str | None
    notes: list[str]


def rrc_taps(sample_rate_hz: float, baud: float, alpha: float = RRC_ALPHA) -> np.ndarray:
    """Return a unit-sum, 10-symbol root-raised-cosine filter."""
    span = int(5 * sample_rate_hz / baud)
    t = np.arange(-span, span + 1, dtype=np.float64) / sample_rate_hz * baud
    taps = np.empty_like(t)
    for i, x in enumerate(t):
        if abs(x) < 1e-9:
            taps[i] = 1 + alpha * (4 / np.pi - 1)
        elif abs(abs(x) - 1 / (4 * alpha)) < 1e-7:
            taps[i] = (
                alpha
                / np.sqrt(2)
                * (
                    (1 + 2 / np.pi) * np.sin(np.pi / (4 * alpha))
                    + (1 - 2 / np.pi) * np.cos(np.pi / (4 * alpha))
                )
            )
        else:
            taps[i] = (
                np.sin(np.pi * x * (1 - alpha)) + 4 * alpha * x * np.cos(np.pi * x * (1 + alpha))
            ) / (np.pi * x * (1 - (4 * alpha * x) ** 2))
    return taps / np.sum(taps)


def _fit_four_levels(samples: np.ndarray) -> tuple[float, float, float, np.ndarray]:
    """Fit affine centers for -3,-1,+1,+3 and return normalized squared residual."""
    if samples.size < 16 or not np.all(np.isfinite(samples)):
        raise ValueError("Not enough finite recovered samples for a four-level fit.")
    variance = float(np.var(samples))
    if variance <= 1e-12:
        raise ValueError("Recovered frequency samples have no measurable variation.")
    center = float(np.mean(samples))
    scale = float(np.std(samples) / np.sqrt(5))
    if not math.isfinite(scale) or scale <= 1e-9:
        raise ValueError("Could not fit four distinct FSK levels.")
    indexes = np.zeros(samples.size, dtype=np.int8)
    for _ in range(15):
        indexes = np.argmin(np.abs((samples[:, None] - center) / scale - LEVELS), axis=1)
        ideal = LEVELS[indexes]
        ideal_mean = float(np.mean(ideal))
        sample_mean = float(np.mean(samples))
        denominator = float(np.sum((ideal - ideal_mean) ** 2))
        if denominator <= 0:
            raise ValueError("Could not fit four distinct FSK levels.")
        scale = max(
            float(np.dot(samples - sample_mean, ideal - ideal_mean) / denominator),
            1e-9,
        )
        center = sample_mean - scale * ideal_mean
    residual = float(np.mean((samples - (center + scale * LEVELS[indexes])) ** 2) / variance)
    return residual, center, scale, indexes


def _sample_candidate(
    filtered: np.ndarray, sample_rate_hz: float, baud: float, phase: float
) -> tuple[np.ndarray, np.ndarray]:
    duration = filtered.size / sample_rate_hz
    indexes = np.arange(0, int(duration * baud), dtype=np.float64)
    times = (indexes + phase) / baud
    keep = (times >= duration * 0.05) & (times < duration * 0.95)
    times = times[keep]
    return np.interp(times * sample_rate_hz, np.arange(filtered.size), filtered), times


def recover_fsk(
    frequency_hz: np.ndarray,
    sample_rate_hz: int,
    baud: float,
    *,
    alpha: float = RRC_ALPHA,
) -> dict:
    """Recover hard 4FSK levels, refining clock within +/-2% of the selected baud.

    A deterministic 48-phase by 17-rate grid selects an initialization. A local
    deterministic grid refines the clock and phase; fit values are not calibrated
    frequency or baud accuracy claims.
    """
    if not isinstance(sample_rate_hz, (int, np.integer)) or sample_rate_hz < 2:
        raise ValueError("Sample rate must be an integer greater than 1 Hz.")
    if not math.isfinite(baud) or not 300 <= baud <= 12000:
        raise ValueError("Baud must be finite and between 300 and 12000.")
    if baud * 1.02 >= sample_rate_hz:
        raise ValueError("Baud is too high for the supplied sample rate.")
    if not 0 < alpha <= 1:
        raise ValueError("RRC alpha must be greater than 0 and at most 1.")
    values = np.asarray(frequency_hz, dtype=np.float64)
    if values.ndim != 1 or values.size < sample_rate_hz // 20:
        raise ValueError("Need at least 50 ms of one-dimensional demodulated frequency samples.")
    if not np.all(np.isfinite(values)):
        raise ValueError("Frequency samples must be finite.")
    filtered = fftconvolve(values, rrc_taps(sample_rate_hz, baud, alpha), mode="same")

    candidates = []
    rates = np.linspace(baud * 0.98, baud * 1.02, 17)
    phases = np.arange(48, dtype=np.float64) / 48
    for rate in rates:
        for phase in phases:
            samples, _ = _sample_candidate(filtered, sample_rate_hz, float(rate), float(phase))
            try:
                fit = _fit_four_levels(samples)
            except ValueError:
                continue
            candidates.append((fit[0], float(rate), float(phase)))
    if not candidates:
        raise ValueError("Could not find enough stable samples for a four-level clock fit.")
    _, coarse_rate, coarse_phase = min(candidates, key=lambda item: item[0])
    rate_step = baud * 0.04 / 16
    refine_rates = np.linspace(
        max(baud * 0.98, coarse_rate - rate_step),
        min(baud * 1.02, coarse_rate + rate_step),
        9,
    )
    phase_offsets = np.arange(-4, 5, dtype=np.float64) / (48 * 4)
    refined = []
    for rate in refine_rates:
        for phase in (coarse_phase + phase_offsets) % 1:
            samples, times = _sample_candidate(filtered, sample_rate_hz, float(rate), float(phase))
            try:
                fit = _fit_four_levels(samples)
            except ValueError:
                continue
            refined.append((fit[0], float(rate), float(phase), samples, times, fit))
    if not refined:
        raise ValueError("Could not refine the four-level clock fit.")
    residual, estimated_baud, phase, _samples, _times, fit = min(refined, key=lambda item: item[0])
    _, center, scale, _indexes = fit
    symbol_stop = int(filtered.size / sample_rate_hz * estimated_baud) - 3
    all_indexes = np.arange(3, max(3, symbol_stop), dtype=np.float64)
    all_times = (all_indexes + phase) / estimated_baud
    all_samples = np.interp(all_times * sample_rate_hz, np.arange(filtered.size), filtered)
    all_levels = LEVELS[np.argmin(np.abs((all_samples[:, None] - center) / scale - LEVELS), axis=1)]
    dibits = DIBIT_BY_LEVEL_INDEX[
        np.argmin(np.abs((all_samples[:, None] - center) / scale - LEVELS), axis=1)
    ]
    return {
        "requested_baud": float(baud),
        "estimated_baud": estimated_baud,
        "phase": phase,
        "alpha": float(alpha),
        "center_hz": center,
        "level_spacing_hz": float(2 * scale),
        "level_centers_hz": (center + scale * LEVELS).astype(float).tolist(),
        "normalized_residual": residual,
        "symbols": all_levels,
        "dibits": dibits,
        "symbol_times_s": all_times,
    }


def nxdn_crc7(bits: np.ndarray) -> int:
    """Compute NXDN SCCH CRC-7 (initial 0x7f, polynomial 0x09)."""
    crc = 0x7F
    for bit in bits:
        feedback = ((crc >> 6) & 1) ^ int(bit)
        crc = ((crc << 1) & 0x7F) ^ (0x09 if feedback else 0)
    return crc


def _viterbi_scch(coded: np.ndarray) -> tuple[np.ndarray, int]:
    costs = np.full(16, 10**9, dtype=np.int64)
    costs[0] = 0
    history = []
    for pair in np.asarray(coded).reshape(-1, 2):
        next_costs = np.full(16, 10**9, dtype=np.int64)
        back = np.zeros(16, dtype=np.int8)
        for state in range(16):
            for bit in (0, 1):
                register = (state << 1) | bit
                next_state = register & 15
                output = ((register & 0x19).bit_count() % 2, (register & 0x17).bit_count() % 2)
                cost = int(costs[state]) + int(np.sum((pair != output) & (pair >= 0)))
                if cost < next_costs[next_state]:
                    next_costs[next_state] = cost
                    back[next_state] = state
        history.append(back)
        costs = next_costs
    state = 0  # The decoder's trellis starts and ends in state zero.
    bits = []
    for back in reversed(history):
        bits.append(state & 1)
        state = int(back[state])
    return np.asarray(bits[::-1], dtype=np.uint8), int(costs[0])


def _pn9_descramble(dibits: np.ndarray) -> np.ndarray:
    output = np.asarray(dibits, dtype=np.uint8).copy()
    register = 0xE4
    for i in range(len(output)):
        bit = register & 1
        feedback = ((register >> 4) ^ register) & 1
        register = (register >> 1) | (feedback << 8)
        output[i] ^= bit << 1
    return output


def _decode_nxdn_frame(frame_dibits: np.ndarray, symbol_index: int, polarity: str) -> FrameCheck:
    # LICH uses the most-significant bit from the first eight descrambled dibits.
    lich_bits = (frame_dibits[:8] >> 1).astype(np.uint8)
    lich = int(np.packbits(lich_bits)[0])
    parity_ok = (lich & 1) == ((lich >> 4).bit_count() % 2)
    lich_type = lich >> 1
    eligible = parity_ok and lich_type in TYPE_D_LICH
    if not eligible:
        return FrameCheck(
            symbol_index=symbol_index,
            polarity=polarity,
            lich_hex=f"0x{lich:02x}",
            lich_parity_ok=parity_ok,
            lich_type=lich_type,
            type_d_eligible=False,
            scch_crc_ok=None,
            scch_crc_computed=None,
            scch_crc_received=None,
            payload_hex=None,
            fec_hard_bit_cost=None,
        )

    raw_bits = np.column_stack((frame_dibits >> 1, frame_dibits & 1)).astype(np.uint8).ravel()
    scch = raw_bits[16:76]
    permutation = np.arange(60).reshape(5, 12).T.ravel()
    deinterleaved = np.empty(60, dtype=np.uint8)
    deinterleaved[permutation] = scch
    keep = np.tile(np.asarray([1, 1, 1, 1, 1, 0], dtype=bool), 12)
    coded = np.full(72, -1, dtype=np.int8)
    coded[keep] = deinterleaved
    decoded, cost = _viterbi_scch(coded)
    computed = nxdn_crc7(decoded[:25])
    received = int(np.packbits(decoded[25:32])[0]) >> 1
    payload_hex = np.packbits(decoded[:32]).tobytes().hex()
    return FrameCheck(
        symbol_index=symbol_index,
        polarity=polarity,
        lich_hex=f"0x{lich:02x}",
        lich_parity_ok=parity_ok,
        lich_type=lich_type,
        type_d_eligible=True,
        scch_crc_ok=computed == received,
        scch_crc_computed=computed,
        scch_crc_received=received,
        payload_hex=payload_hex,
        fec_hard_bit_cost=cost,
    )


def check_nxdn_frames(dibits: np.ndarray) -> tuple[list[FSWHit], list[FrameCheck], int]:
    """Find exact 20-bit FSW words and validate eligible Type-D SCCH frames."""
    values = np.asarray(dibits, dtype=np.uint8)
    if values.ndim != 1 or np.any(values > 3):
        raise ValueError("Dibit sequence must be one-dimensional values from 0 through 3.")
    hits: list[FSWHit] = []
    frames: list[FrameCheck] = []
    total = 0
    inverted = np.asarray([2, 3, 0, 1], dtype=np.uint8)[values]
    for polarity, candidate in (("normal", values), ("inverted", inverted)):
        bits = np.column_stack((candidate >> 1, candidate & 1)).astype(np.uint8).ravel()
        for bit_start in range(max(0, len(bits) - NXDN_FSW_BITS + 1)):
            if bit_start % 2:
                continue
            window = bits[bit_start : bit_start + NXDN_FSW_BITS]
            word = int("".join(str(int(bit)) for bit in window), 2)
            if word != NXDN_FSW:
                continue
            symbol_index = bit_start // 2
            total += 1
            if len(hits) < MAX_REPORTED_HITS:
                hits.append(
                    FSWHit(symbol_index=symbol_index, word_hex=f"0x{word:05x}", polarity=polarity)
                )
            frame_start = symbol_index + 10
            frame_stop = frame_start + 182
            if frame_stop > len(candidate):
                continue
            frame_dibits = _pn9_descramble(candidate[frame_start:frame_stop])
            if len(frames) < MAX_REPORTED_HITS:
                frames.append(_decode_nxdn_frame(frame_dibits, symbol_index, polarity))
    return hits, frames, total


def _demodulate_iq(raw: np.ndarray, input_rate_hz: int, offset_hz: float, bandwidth_hz: float):
    """Mix and resample IQ to 50 kHz, low-pass, then differentiate phase to Hz.

    `channelize_iq` performs the complex translation and rational resampling. A
    161-tap Hamming FIR at 50 kHz uses half the requested complex-channel width
    as its cutoff. The phase difference is scaled to instantaneous frequency in Hz.
    """
    if not math.isfinite(offset_hz):
        raise ValueError("Target offset must be finite.")
    if not math.isfinite(bandwidth_hz) or not 1000 <= bandwidth_hz <= 40000:
        raise ValueError("Bandwidth must be between 1000 and 40000 Hz.")
    if abs(offset_hz) + bandwidth_hz / 2 + 2500 >= input_rate_hz / 2:
        raise ValueError("Target channel plus filter guard must fit inside IQ Nyquist bandwidth.")
    count = len(raw)
    if count < max(2, input_rate_hz // 1000):
        raise ValueError("IQ recording must contain at least 1 ms of selected samples.")
    channel = channelize_iq(raw, input_rate_hz, count, offset_hz)
    taps = firwin(161, bandwidth_hz / 2, fs=OUTPUT_HZ)
    filtered = lfilter(taps, [1.0], channel)
    frequency = np.angle(filtered[1:] * np.conj(filtered[:-1])) * OUTPUT_HZ / (2 * np.pi)
    return frequency, count / input_rate_hz


def analyze_iq(
    path: Path,
    *,
    target_hz: float | None,
    offset_hz: float | None,
    center_hz: float | None,
    start_s: float,
    seconds: float,
    baud: float,
    bandwidth_hz: float,
) -> tuple[SymbolResult, dict]:
    if not math.isfinite(start_s) or start_s < 0:
        raise ValueError("Start time must be finite and nonnegative.")
    if not math.isfinite(seconds) or not 0.05 <= seconds <= 5:
        raise ValueError("Duration must be between 0.05 and 5 seconds.")
    if (target_hz is None) == (offset_hz is None):
        raise ValueError("Choose exactly one of --target-hz or --offset-hz.")
    if center_hz is not None and target_hz is None:
        raise ValueError("--center-hz is only valid with --target-hz.")
    with open_iq(path) as recording:
        rate = recording.rate
        raw = recording.read(start_s, seconds)
        capture_center = center_hz if center_hz is not None else recording.center_at(start_s)
    if target_hz is not None:
        if not math.isfinite(target_hz):
            raise ValueError("Target frequency must be finite.")
        if capture_center is None:
            raise ValueError(
                "Absolute --target-hz needs SigMF center metadata or explicit --center-hz; "
                "use --offset-hz for an IQ WAV without tuned-frequency metadata."
            )
        offset = target_hz - capture_center
    else:
        offset = float(offset_hz)
    frequency, analyzed_s = _demodulate_iq(raw, rate, offset, bandwidth_hz)
    recovery = recover_fsk(frequency, OUTPUT_HZ, baud)
    hits, frames, total_hits = check_nxdn_frames(recovery["dibits"])
    notes = [
        "Four-level fit residual is a selected-baud hypothesis quality measure, not a protocol identification.",
        "Baud is estimated within +/-2% of the requested value; it is not calibrated precision.",
        "Symbol timestamps use stored capture time and include the 1.6 ms channel FIR delay.",
        "NXDN FSW and Type-D checks provide sample evidence only; no speaker or speech claim is made.",
    ]
    if total_hits > len(hits) or len(frames) >= MAX_REPORTED_HITS:
        notes.append(
            f"Frame evidence is capped at {MAX_REPORTED_HITS} entries; total exact FSW matches are reported separately."
        )
    result = SymbolResult(
        input=str(path),
        target_hz=float(target_hz) if target_hz is not None else None,
        center_hz=float(capture_center) if capture_center is not None else None,
        offset_hz=offset,
        input_rate_hz=rate,
        analyzed_s=analyzed_s,
        start_s=start_s,
        requested_baud=float(baud),
        estimated_baud=recovery["estimated_baud"],
        clock_phase_symbols=recovery["phase"],
        rrc_alpha=RRC_ALPHA,
        bandwidth_hz=bandwidth_hz,
        frequency_level_centers_hz=recovery["level_centers_hz"],
        level_spacing_hz=recovery["level_spacing_hz"],
        normalized_residual=recovery["normalized_residual"],
        symbols_count=len(recovery["symbols"]),
        nxdn_fsw_match_count=total_hits,
        nxdn_fsw_hits=hits,
        frame_checks=frames,
        output_dir=None,
        notes=notes,
    )
    arrays = {
        "symbols": recovery["symbols"],
        "dibits": recovery["dibits"],
        "symbol_times_s": recovery["symbol_times_s"] + start_s,
        "baud": np.asarray(recovery["estimated_baud"]),
        "level_centers_hz": np.asarray(recovery["level_centers_hz"]),
    }
    return result, arrays


@contextmanager
def _input_source(file: Path):
    if str(file) != "-":
        yield file.expanduser()
        return
    with tempfile.TemporaryDirectory(prefix="radio-iq-stdin-") as work:
        path = Path(work) / "stdin.wav"
        with path.open("wb") as stream:
            shutil.copyfileobj(sys.stdin.buffer, stream)
        yield path


def _save_output(result: SymbolResult, arrays: dict, destination: Path, yes: bool) -> SymbolResult:
    if not yes:
        raise typer.BadParameter("--output-dir requires --yes to create a new directory.")
    destination = destination.expanduser()
    try:
        destination.mkdir(parents=True, exist_ok=False)
    except FileExistsError as exc:
        raise typer.BadParameter(f"Output directory already exists: {destination}") from exc
    np.savez_compressed(destination / "symbols.npz", **arrays)
    saved = result.model_copy(update={"output_dir": str(destination.resolve())})
    (destination / "result.json").write_text(saved.model_dump_json(indent=2) + "\n")
    return saved


def iq_symbols_command(
    file: Annotated[
        Path, typer.Argument(help="IQ WAV, SigMF metadata/data, SigMF tar, or - for stdin.")
    ],
    target_hz: Annotated[
        float | None, typer.Option(help="Absolute target frequency in Hz.")
    ] = None,
    offset_hz: Annotated[
        float | None, typer.Option(help="Target offset from IQ center in Hz.")
    ] = None,
    center_hz: Annotated[
        float | None, typer.Option(help="Tuned center frequency for an IQ WAV.")
    ] = None,
    start_s: Annotated[float, typer.Option(min=0, help="Start time in seconds.")] = 0,
    seconds: Annotated[
        float, typer.Option(min=0.05, max=5, help="Analyze 0.05 through 5 seconds.")
    ] = 0.5,
    baud: Annotated[
        float, typer.Option(min=300, max=12000, help="Selected baud hypothesis.")
    ] = 2400,
    bandwidth_hz: Annotated[
        float, typer.Option(min=1000, max=40000, help="Complex IQ channel filter bandwidth.")
    ] = 6250,
    output_dir: Annotated[
        Path | None, typer.Option(help="Write symbols.npz and result.json into a new directory.")
    ] = None,
    yes: Annotated[bool, typer.Option("--yes", help="Confirm creating --output-dir.")] = False,
    as_json: Annotated[bool, typer.Option("--json", help="Emit machine-readable JSON.")] = False,
):
    """Recover 4FSK symbols and check NXDN Type-D frame evidence without audio output."""
    try:
        with _input_source(file) as path:
            result, arrays = analyze_iq(
                path,
                target_hz=target_hz,
                offset_hz=offset_hz,
                center_hz=center_hz,
                start_s=start_s,
                seconds=seconds,
                baud=baud,
                bandwidth_hz=bandwidth_hz,
            )
        result.input = str(file)
        if output_dir is not None:
            result = _save_output(result, arrays, output_dir, yes)
    except (OSError, ValueError) as exc:
        typer.echo(f"Error: {exc}", err=True)
        raise typer.Exit(1) from exc
    if as_json:
        typer.echo(result.model_dump_json())
    else:
        checks = sum(frame.scch_crc_ok is True for frame in result.frame_checks)
        typer.echo(
            f"4FSK fit: requested={result.requested_baud:.3f} baud, "
            f"estimated={result.estimated_baud:.3f}, residual={result.normalized_residual:.4f}; "
            f"symbols={result.symbols_count}, NXDN FSWs={result.nxdn_fsw_match_count}, "
            f"Type-D SCCH CRC passes={checks}"
        )
        for note in result.notes:
            typer.echo(f"Note: {note}")


app.command("iq-symbols")(iq_symbols_command)
