"""Bounded, offline digital-radio probing through dsd-fme."""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Annotated, Literal

import numpy as np
import typer
from pydantic import BaseModel, PrivateAttr
from scipy.io import wavfile

app = typer.Typer(no_args_is_help=True, help="Probe demodulated radio audio with local dsd-fme.")
Protocol = Literal["auto", "dmr", "nxdn48", "nxdn96", "dpmr", "p25p1"]
DECODER_FLAGS = {
    "auto": "-fa",
    "dmr": "-fs",
    "nxdn48": "-fi",
    "nxdn96": "-fn",
    "dpmr": "-fm",
    "p25p1": "-f1",
}
MAX_DIAGNOSTIC_CHARS = 4000
MAX_LOG_CHARS = 4 * 1024 * 1024
SYNC_RE = re.compile(r"\bSync:(?!\s*no\s+sync)\s*\S", re.IGNORECASE)
CRC_FAILURE_RE = re.compile(
    r"(?:\bCRC\b.{0,50}\b(?:err(?:or)?|fail(?:ure)?)\b|"
    r"\b(?:err(?:or)?|fail(?:ure)?)\b.{0,50}\bCRC\b)",
    re.IGNORECASE,
)
# These message labels are emitted by decoded NXDN element paths. Their callers
# are CRC gated: SCCH at nxdn_deperm.c:913; CAC at :788; other content paths at
# :498, :702, :995, :1451 and nxdn_element.c:48. Keep counts separate from syncs.
CRC_CHECKED_MESSAGE_RES = {
    "site_id_message": re.compile(r"\bSite ID Message\s*-"),
    "service_information": re.compile(r"\bService Information\s*-"),
    "idle_repeater_message": re.compile(r"\bIdle Repeater Message\s*-"),
    "transmission_release": re.compile(r"\bTransmission Release\s*-"),
}


class DigitalProbeResult(BaseModel):
    _full_diagnostics: str = PrivateAttr(default="")
    input: str
    decoder: str
    requested_protocol: Protocol
    resolved_protocol: str
    analyzed_s: float
    status: Literal["decoder_output", "no_decode"]
    raw_sync_count: int
    crc_checked_messages: dict[str, int]
    crc_failures: int
    stdout: str
    stderr: str
    diagnostic_text: str
    diagnostic_truncated: bool
    notes: list[str]


def _decoder_path(override: str | None) -> str:
    candidate = override or os.environ.get("RADIO_DSD_FME") or shutil.which("dsd-fme")
    if not candidate:
        raise ValueError("dsd-fme was not found. Install it or pass --decoder /path/to/dsd-fme.")
    resolved = shutil.which(candidate)
    if resolved is None:
        path = Path(candidate).expanduser()
        if path.is_file() and os.access(path, os.X_OK):
            resolved = str(path.resolve())
    if resolved is None:
        raise ValueError(f"Decoder is not executable: {candidate}")
    return str(Path(resolved).resolve())


def _read_limited_wav(source: str, seconds: float, temp_dir: Path) -> tuple[Path, float]:
    if source == "-":
        input_path = temp_dir / "stdin.wav"
        with input_path.open("wb") as output:
            shutil.copyfileobj(sys.stdin.buffer, output)
    else:
        input_path = Path(source).expanduser()
    try:
        rate, samples = wavfile.read(input_path, mmap=True)
    except (OSError, ValueError) as exc:
        raise ValueError(f"Cannot read WAV input {source!r}: {exc}") from exc
    if rate != 48000:
        raise ValueError(f"Expected a 48000 Hz mono WAV; found {rate} Hz.")
    if samples.ndim != 1:
        raise ValueError("Expected a mono WAV. Select or downmix a channel before probing.")
    if samples.size == 0:
        raise ValueError("WAV must contain non-empty PCM samples.")
    count = min(samples.size, max(1, int(seconds * rate)))
    selected = np.asarray(samples[:count])
    if selected.dtype == np.dtype("int16"):
        pcm = selected
    elif selected.dtype == np.dtype("int32"):
        pcm = np.clip(selected.astype(np.int64) >> 16, -32768, 32767).astype(np.int16)
    elif selected.dtype == np.dtype("uint8"):
        pcm = ((selected.astype(np.int16) - 128) << 8).astype(np.int16)
    elif selected.dtype in (np.dtype("float32"), np.dtype("float64")):
        values = selected.astype(np.float64)
        if not np.all(np.isfinite(values)):
            raise ValueError("Floating-point WAV contains non-finite samples.")
        pcm = np.clip(values * 32768.0, -32768, 32767).astype(np.int16)
    else:
        raise ValueError(
            f"Unsupported WAV sample type {selected.dtype}; use PCM uint8, int16/int32, or float32/float64."
        )
    limited = temp_dir / "probe-input.wav"
    wavfile.write(limited, rate, pcm)
    return limited, count / rate


def run_digital_probe(
    source: str,
    seconds: float = 30,
    protocol: Protocol = "auto",
    decoder: str | None = None,
    timeout: float = 120,
) -> DigitalProbeResult:
    if not np.isfinite(seconds) or not 0 < seconds <= 120:
        raise ValueError("Probe duration must be greater than 0 and at most 120 seconds.")
    if not np.isfinite(timeout) or not 0 < timeout <= 120:
        raise ValueError("Decoder timeout must be greater than 0 and at most 120 seconds.")
    binary = _decoder_path(decoder)
    with tempfile.TemporaryDirectory(prefix="radio-digital-probe-") as work:
        cwd = Path(work)
        wav_path, analyzed_s = _read_limited_wav(source, seconds, cwd)
        env = {
            key: value
            for key, value in os.environ.items()
            if key not in {"RADIO_DSD_FME", "RIGCTL", "RIGCTLD", "RTLSDR_DEVICE"}
        }
        env["HOME"] = str(cwd / "home")
        env["XDG_CONFIG_HOME"] = str(cwd / "config")
        Path(env["HOME"]).mkdir()
        Path(env["XDG_CONFIG_HOME"]).mkdir()
        try:
            proc = subprocess.run(
                [binary, "-i", str(wav_path), "-o", "null", DECODER_FLAGS[protocol]],
                cwd=cwd,
                env=env,
                capture_output=True,
                timeout=timeout,
                check=False,
            )
        except subprocess.TimeoutExpired as exc:
            raise ValueError(f"dsd-fme exceeded the {timeout:g} second timeout.") from exc
        except OSError as exc:
            raise ValueError(f"Could not run dsd-fme: {exc}") from exc
    if proc.returncode:
        detail = proc.stderr.decode(errors="replace")[:1000]
        raise ValueError(f"dsd-fme exited {proc.returncode}: {detail}")

    raw_stdout = proc.stdout.decode(errors="replace")
    raw_stderr = proc.stderr.decode(errors="replace")
    stdout = raw_stdout[:MAX_DIAGNOSTIC_CHARS]
    stderr = raw_stderr[:MAX_DIAGNOSTIC_CHARS]
    merged = f"stdout:\n{stdout}\nstderr:\n{stderr}".strip()
    sync_count = len(SYNC_RE.findall(raw_stdout + "\n" + raw_stderr))
    crc_failures = len(CRC_FAILURE_RE.findall(raw_stdout + "\n" + raw_stderr))
    all_diagnostics = raw_stdout + "\n" + raw_stderr
    crc_checked_messages = {
        name: len(pattern.findall(all_diagnostics))
        for name, pattern in CRC_CHECKED_MESSAGE_RES.items()
    }
    notes = []
    if protocol == "auto":
        notes.append(
            "dsd-fme -fa auto covers P25, YSF, DSTAR, X2-TDMA, and DMR; it excludes NXDN and dPMR."
        )
    notes.append(
        "CRC-checked payload-label counts depend on the managed pinned dsd-fme version; "
        "an explicit decoder override may change its output. They count decoded messages, "
        "not speakers or speech."
    )
    notes.append(
        "Decoder output is diagnostic evidence only; it does not verify a protocol identification."
    )
    result = DigitalProbeResult(
        input=source,
        decoder=binary,
        requested_protocol=protocol,
        resolved_protocol=protocol,
        analyzed_s=analyzed_s,
        status=(
            "decoder_output" if sync_count or any(crc_checked_messages.values()) else "no_decode"
        ),
        raw_sync_count=sync_count,
        crc_checked_messages=crc_checked_messages,
        crc_failures=crc_failures,
        stdout=stdout,
        stderr=stderr,
        diagnostic_text=merged[: MAX_DIAGNOSTIC_CHARS * 2 + 20],
        diagnostic_truncated=(
            len(raw_stdout) > len(stdout)
            or len(raw_stderr) > len(stderr)
            or len(merged) > MAX_DIAGNOSTIC_CHARS * 2 + 20
        ),
        notes=notes,
    )
    full = f"stdout:\n{raw_stdout}\nstderr:\n{raw_stderr}"
    result._full_diagnostics = full[:MAX_LOG_CHARS]
    if len(full) > MAX_LOG_CHARS:
        result._full_diagnostics += "\n[Diagnostic log truncated at 4 MiB.]\n"
    return result


def digital_probe_command(
    file: Annotated[str, typer.Argument(help="Mono 48000 Hz WAV path, or - for stdin.")],
    seconds: Annotated[float, typer.Option(min=0.001, max=120)] = 30,
    protocol: Annotated[Protocol, typer.Option(help="Decoder family to test.")] = "auto",
    decoder: Annotated[str | None, typer.Option(help="Explicit dsd-fme executable path.")] = None,
    log_output: Annotated[
        Path | None, typer.Option(help="Write full diagnostics to a new file (up to 4 MiB).")
    ] = None,
    yes: Annotated[bool, typer.Option("--yes", help="Confirm creating --log-output.")] = False,
    as_json: Annotated[bool, typer.Option("--json", help="Emit the result as JSON.")] = False,
):
    """Run a bounded offline decoder probe. Example: radio digital-probe channel.wav --protocol nxdn48 --json."""
    if log_output is not None and not yes:
        raise typer.BadParameter("--log-output requires --yes to create a file.")
    if log_output is not None and log_output.exists():
        raise typer.BadParameter(f"Log output already exists: {log_output}")
    try:
        result = run_digital_probe(file, seconds, protocol, decoder)
    except ValueError as exc:
        typer.echo(f"Error: {exc}", err=True)
        raise typer.Exit(1) from exc
    except OSError as exc:
        typer.echo(f"Error: {exc}", err=True)
        raise typer.Exit(1) from exc
    if log_output is not None:
        log_output.parent.mkdir(parents=True, exist_ok=True)
        try:
            with log_output.open("x", encoding="utf-8") as output:
                output.write(result._full_diagnostics + "\n")
        except FileExistsError as exc:
            raise typer.BadParameter(f"Log output already exists: {log_output}") from exc
    if as_json:
        typer.echo(result.model_dump_json())
    else:
        checked = (
            ", ".join(
                f"{label}={count}" for label, count in result.crc_checked_messages.items() if count
            )
            or "none"
        )
        typer.echo(
            f"{result.status}: {result.requested_protocol}; {result.analyzed_s:.3f}s; "
            f"syncs={result.raw_sync_count}; CRC failures={result.crc_failures}; "
            f"CRC-checked messages: {checked}"
        )
        for note in result.notes:
            typer.echo(f"Note: {note}")


app.command("digital-probe")(digital_probe_command)
