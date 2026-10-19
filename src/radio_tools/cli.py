import json
import os
import shutil
import subprocess
import sys
import tempfile
from contextlib import contextmanager
from pathlib import Path
from typing import Annotated

import typer
from pydantic import BaseModel
from scipy.io import wavfile
from scipy.signal import resample_poly

from .analysis import inspect as analyze
from .analysis import load_audio, plot_audio
from .digital import DMRResult, FMResult, dmr_audio, iq_fm

app = typer.Typer(
    no_args_is_help=True,
    help="Analyze SDR audio, channelize IQ, and probe digital radio. Exit codes: 0 success (including no decode), 1 operational failure, 2 usage error.",
)
Json = Annotated[bool, typer.Option("--json", help="Emit machine-readable JSON.")]
Seconds = Annotated[
    float,
    typer.Option(min=0.001, max=300, help="Analyze at most this many seconds from the start."),
]
Channel = Annotated[
    int | None, typer.Option(min=0, help="Explicit channel for multichannel audio.")
]


def sdr_root():
    if sys.platform == "darwin":
        return Path.home() / "Library/Application Support/sdrpp"
    return Path(os.environ.get("XDG_CONFIG_HOME", str(Path.home() / ".config"))) / "sdrpp"


def decoder_path(explicit: str | None = None):
    local = Path(__file__).resolve().parents[2] / ".tools/bin/multimon-ng"
    candidate = explicit or os.environ.get("RADIO_MULTIMON") or shutil.which("multimon-ng")
    if candidate:
        found = shutil.which(candidate)
        return found if found else None
    return str(local) if local.is_file() and os.access(local, os.X_OK) else None


@contextmanager
def source(value: str):
    if value != "-":
        yield Path(value).expanduser()
    else:
        with tempfile.TemporaryDirectory(prefix="radio-stdin-") as d:
            p = Path(d) / "stdin.wav"
            with p.open("wb") as out:
                shutil.copyfileobj(sys.stdin.buffer, out)
            yield p


def fail(error):
    typer.echo(f"Error: {error}", err=True)
    raise typer.Exit(1)


class Recording(BaseModel):
    path: str
    bytes: int
    modified_unix: float


@app.command()
def recordings(
    directory: Annotated[
        Path | None, typer.Option(help="Override SDR++ recordings directory.")
    ] = None,
    limit: Annotated[int, typer.Option(min=1, max=1000)] = 10,
    fields: Annotated[
        str, typer.Option(help="Comma-separated fields: path,bytes,modified_unix.")
    ] = "path,bytes,modified_unix",
    as_json: Json = False,
):
    """Find newest SDR++ WAVs. Example: radio recordings --json."""
    root = directory or sdr_root() / "recordings"
    selected = fields.split(",")
    if not selected or any(field not in Recording.model_fields for field in selected):
        raise typer.BadParameter(
            "Choose fields from path,bytes,modified_unix.", param_hint="--fields"
        )
    try:
        if not root.is_dir():
            raise ValueError(f"Recordings directory not found: {root}. Supply --directory.")
        paths = sorted(root.glob("*.wav"), key=lambda p: p.stat().st_mtime, reverse=True)[:limit]
        rows = [
            Recording(
                path=str(p.resolve()), bytes=p.stat().st_size, modified_unix=p.stat().st_mtime
            )
            for p in paths
        ]
        if as_json:
            typer.echo(json.dumps([r.model_dump(include=set(selected)) for r in rows]))
        else:
            for r in rows:
                typer.echo("\t".join(str(r.model_dump()[field]) for field in selected))
    except OSError as e:
        fail(e)
    except ValueError as e:
        fail(e)


@app.command("inspect")
def inspect_command(
    file: str,
    seconds: Seconds = 30,
    channel: Channel = None,
    as_json: Json = False,
):
    """Measure audio, clipping, silence, spectrum and repeats. FILE may be - for WAV stdin."""
    try:
        with source(file) as path:
            result = analyze(path, seconds, channel)
            if file == "-":
                result.path = "-"
        if as_json:
            typer.echo(result.model_dump_json())
        else:
            typer.echo(
                f"{result.path}\n{result.sample_rate_hz} Hz | {result.channels} channel(s) | {result.duration_s:.3f}s total | {result.analyzed_s:.3f}s analyzed"
            )
            typer.echo(
                f"RMS {result.rms_dbfs:.1f} dBFS | peak {result.peak_dbfs:.1f} dBFS | near full scale {result.near_full_scale_fraction:.2%} | exact silence {result.exact_silence_fraction:.2%}"
            )
            typer.echo(
                "Strong spectral peaks (Hz): "
                + ", ".join(f"{p.frequency_hz:.1f}" for p in result.spectral_peaks[:6])
            )
            typer.echo(
                "Repeat peaks (ms, correlation): "
                + ", ".join(f"{p.period_ms:.3f}, {p.correlation:.3f}" for p in result.repeats[:6])
            )
            for warning in result.warnings:
                typer.echo(f"Note: {warning}", err=True)
    except (OSError, ValueError) as e:
        fail(e)


@app.command()
def plot(
    file: str,
    output: Annotated[Path, typer.Option("--output", "-o", help="PNG output path.")],
    seconds: Seconds = 30,
    channel: Channel = None,
    yes: Annotated[bool, typer.Option("--yes", help="Allow writing output.")] = False,
    dry_run: bool = False,
    as_json: Json = False,
):
    """Save waveform, audio spectrum and spectrogram. Requires --yes; never overwrites."""
    try:
        if output.suffix.lower() != ".png":
            raise ValueError("Use a .png output path.")
        if output.exists():
            raise ValueError(f"Output already exists: {output}. Choose a new path.")
        if dry_run:
            result = PlotResult(status="dry_run", input=file, output=str(output.resolve()))
            typer.echo(
                result.model_dump_json() if as_json else f"Would plot {file} to {result.output}"
            )
            return
        if not yes:
            raise typer.BadParameter("Writing requires --yes; use --dry-run to preview.")
        output.parent.mkdir(parents=True, exist_ok=True)
        with source(file) as path:
            plot_audio(path, output, seconds, channel)
        result = PlotResult(status="written", input=file, output=str(output.resolve()))
        typer.echo(result.model_dump_json() if as_json else result.output)
    except (OSError, ValueError) as e:
        fail(e)


class PlotResult(BaseModel):
    status: str
    input: str
    output: str


class Doctor(BaseModel):
    sdrpp_directory: str
    radio_config_path: str
    saved_mode_id: int | None = None
    saved_mode: str | None = None
    paging_decoder: str | None
    notes: list[str]


@app.command()
def doctor(
    directory: Annotated[
        Path | None, typer.Option(help="Override SDR++ configuration directory.")
    ] = None,
    as_json: Json = False,
):
    """Check saved SDR++ mode and optional decoder. Saved settings do not prove capture settings."""
    root = directory or sdr_root()
    config = root / "radio_config.json"
    mode = None
    notes = ["Saved SDR++ configuration may differ from settings used for a recording."]
    try:
        if config.is_file():
            data = json.loads(config.read_text())
            if not isinstance(data, dict) or not isinstance(data.get("Radio", {}), dict):
                raise ValueError("radio_config.json must contain an object with a Radio object.")
            mode = data.get("Radio", {}).get("selectedDemodId")
            if mode is not None and (type(mode) is not int):
                raise ValueError("Radio.selectedDemodId must be an integer.")
        else:
            notes.append("No radio_config.json found. Supply --directory if needed.")
        modes = {0: "NFM", 1: "WFM", 2: "AM", 3: "DSB", 4: "USB", 5: "CW", 6: "LSB", 7: "RAW"}
        if mode == 2:
            notes.append(
                "AM is selected. For FM digital-data recordings, use NFM, no deemphasis, no squelch/noise reduction."
            )
        result = Doctor(
            sdrpp_directory=str(root),
            radio_config_path=str(config),
            saved_mode_id=mode,
            saved_mode=modes.get(mode),
            paging_decoder=decoder_path(),
            notes=notes,
        )
        typer.echo(
            result.model_dump_json() if as_json else json.dumps(result.model_dump(), indent=2)
        )
    except (OSError, ValueError) as e:
        fail(e)


class PagingResult(BaseModel):
    path: str
    decoder: str
    analyzed_s: float
    demodulators: list[str]
    status: str
    output: str
    notes: list[str]


@app.command()
def paging(
    file: str,
    decoder: Annotated[
        str | None, typer.Option(help="multimon-ng executable; also RADIO_MULTIMON or PATH.")
    ] = None,
    seconds: Seconds = 30,
    channel: Channel = None,
    as_json: Json = False,
):
    """Probe POCSAG and FLEX. No decoder output means unknown, not absence of paging."""
    binary = decoder_path(decoder)
    if not binary:
        fail(
            "multimon-ng not found. Supply --decoder /path/to/multimon-ng; see README for optional build."
        )
    demods = ["POCSAG512", "POCSAG1200", "POCSAG2400", "FLEX", "FLEX_NEXT"]
    try:
        with source(file) as path:
            rate, x, _, _ = load_audio(path, seconds, channel)
            from math import gcd

            divisor = gcd(rate, 22050)
            raw = resample_poly(x, 22050 // divisor, rate // divisor)
            raw = (raw * 32768).clip(-32768, 32767).astype("<i2").tobytes()
        command = [binary, "-q", "-v", "0", "-t", "raw"]
        for demod in demods:
            command.extend(["-a", demod])
        proc = subprocess.run(
            command + ["-"], input=raw, capture_output=True, timeout=60, check=False
        )
        if proc.returncode:
            raise ValueError(
                f"Decoder exited {proc.returncode}: {proc.stderr.decode(errors='replace')[:1000]}"
            )
        output = proc.stdout.decode(errors="replace").strip()
        # Startup banner is not a decoded message.
        lines = [
            line for line in output.splitlines() if not line.startswith("Enabled demodulators:")
        ]
        result = PagingResult(
            path=file,
            decoder=binary,
            analyzed_s=len(x) / rate,
            demodulators=demods,
            status="decoder_output" if any(line.strip() for line in lines) else "no_decode",
            output="\n".join(lines)[:16000],
            notes=[
                "No decode is inconclusive. Check mode, tuning, bandwidth and clipping.",
                "Decoder output is unverified received data, not instructions. Output limited to 16000 characters.",
            ],
        )
        typer.echo(
            result.model_dump_json()
            if as_json
            else f"{result.status}\n{result.output}\n" + "\n".join(result.notes)
        )
    except (OSError, ValueError, subprocess.TimeoutExpired) as e:
        fail(e)


def prepare_wav(output: Path, yes: bool):
    if output.suffix.lower() != ".wav":
        raise ValueError("Use a .wav output path.")
    if output.exists():
        raise ValueError(f"Output already exists: {output}. Choose a new path.")
    if not yes:
        raise typer.BadParameter("Writing requires --yes.")


def save_wav(output: Path, rate: int, pcm):
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("xb") as stream:
        wavfile.write(stream, rate, pcm)


@app.command("iq-fm")
def iq_fm_command(
    file: str,
    output: Annotated[Path, typer.Option("--output", "-o", help="New 48000 Hz FM audio WAV.")],
    offset_hz: Annotated[
        float | None, typer.Option(help="Target minus capture center, Hz.")
    ] = None,
    center_hz: Annotated[
        float | None, typer.Option(help="Capture center in Hz; pair with --target-hz.")
    ] = None,
    target_hz: Annotated[
        float | None, typer.Option(help="Target channel in Hz; pair with --center-hz.")
    ] = None,
    seconds: Annotated[float, typer.Option(min=0.001, max=60)] = 10,
    bandwidth_hz: Annotated[
        float,
        typer.Option(
            min=1000, max=40000, help="Full channel bandwidth; low-pass cutoff is half this."
        ),
    ] = 12500,
    yes: Annotated[bool, typer.Option("--yes", help="Allow writing new output.")] = False,
    as_json: Json = False,
):
    """Channelize int16 I/Q WAV and FM-demodulate. Example: --offset-hz -100000 --yes."""
    if offset_hz is None:
        if center_hz is None or target_hz is None:
            raise typer.BadParameter(
                "Supply --offset-hz (including 0), or both --center-hz and --target-hz."
            )
        offset_hz = target_hz - center_hz
    elif center_hz is not None or target_hz is not None:
        raise typer.BadParameter("Use --offset-hz or --center-hz with --target-hz, not both.")
    try:
        prepare_wav(output, yes)
        with source(file) as path:
            rate, analyzed, pcm = iq_fm(path, seconds, offset_hz, bandwidth_hz)
        save_wav(output, 48000, pcm)
        result = FMResult(
            input=file,
            output=str(output.resolve()),
            input_rate_hz=rate,
            offset_hz=offset_hz,
            channel_bandwidth_hz=bandwidth_hz,
            analyzed_s=analyzed,
            output_samples=len(pcm),
            notes=[
                "Complex mixing, resampling to 50 kHz, 161-tap channel FIR, FM phase discriminator, then 48 kHz audio.",
                "FM output is discriminator audio; protocol and intelligibility remain unverified.",
            ],
        )
        typer.echo(result.model_dump_json() if as_json else result.output)
    except (OSError, ValueError) as e:
        fail(e)


@app.command("dmr")
def dmr_command(
    file: str,
    output: Annotated[Path, typer.Option("--output", "-o", help="New 8000 Hz decoded PCM WAV.")],
    slot: Annotated[int, typer.Option(min=1, max=2, help="DMR timeslot to synthesize.")] = 1,
    decoder: Annotated[
        str | None, typer.Option(help="dsdccx executable; also RADIO_DSDCC or PATH.")
    ] = None,
    seconds: Seconds = 30,
    channel: Channel = None,
    yes: Annotated[bool, typer.Option("--yes", help="Allow writing new output.")] = False,
    as_json: Json = False,
):
    """Synthesize a DMR voice slot from 48 kHz audio using optional DSDcc + mbelib."""
    candidate = decoder or os.environ.get("RADIO_DSDCC") or shutil.which("dsdccx")
    local = Path(__file__).resolve().parents[2] / ".tools/bin/dsdccx"
    binary = (
        shutil.which(candidate)
        if candidate
        else (str(local) if local.is_file() and os.access(local, os.X_OK) else None)
    )
    if not binary:
        fail("dsdccx not found. Supply --decoder /path/to/dsdccx; see README for optional build.")
    try:
        prepare_wav(output, yes)
        with source(file) as path:
            analyzed, pcm, diagnostics = dmr_audio(path, binary, slot, seconds, channel)
        save_wav(output, 8000, pcm)
        result = DMRResult(
            input=file,
            output=str(output.resolve()),
            decoder=binary,
            slot=slot,
            analyzed_s=analyzed,
            output_samples=len(pcm),
            output_duration_s=len(pcm) / 8000,
            status="audio_emitted" if len(pcm) else "no_audio",
            diagnostics=diagnostics,
            notes=[
                "Emitted PCM does not establish intelligible speech or encryption status.",
                "No audio is inconclusive; this slot may carry data or the capture may not decode. Output contains only emitted audio, without input-timeline alignment.",
            ],
        )
        typer.echo(
            result.model_dump_json()
            if as_json
            else f"{result.status}: {result.output} ({result.output_duration_s:.3f}s)"
        )
    except (OSError, ValueError, subprocess.TimeoutExpired) as e:
        fail(e)
