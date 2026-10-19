"""Receive-only ADALM-Pluto capture through the libiio 0.x ABI.

No transmit streaming device or transmit LO is opened. WAV channels are I then Q,
16-bit signed little-endian container values (normally 12 valid ADC bits).
"""

import ctypes as c
import ctypes.util
import math
import shutil
import sys
import time
import wave
from collections.abc import Callable
from pathlib import Path
from typing import Annotated, Literal

import typer
from pydantic import BaseModel, ConfigDict, Field

WAV_MAX_DATA = 0xFFFFFFFF - 36
BUNDLED_LIBRARY = Path("/Applications/SDR++.app/Contents/Frameworks/iio")
CONTROLS = ("sampling_frequency", "rf_bandwidth", "frequency", "gain_control_mode")

app = typer.Typer(
    no_args_is_help=True,
    help="Receive-only Pluto IQ capture. Exit codes: 0 success, 1 failure, 2 usage error.",
)


class CaptureConfig(BaseModel):
    model_config = ConfigDict(allow_inf_nan=False)
    uri: str = "ip:pluto.local"
    rate: int = Field(default=5_000_000, ge=520_834, le=61_440_000)
    center: int = Field(ge=70_000_000, le=6_000_000_000)
    seconds: float = Field(default=10, gt=0, le=300)
    output: Path
    bandwidth: int | None = Field(default=None, ge=200_000, le=56_000_000)
    gain_mode: Literal["slow_attack", "fast_attack", "manual", "hybrid"] | None = None
    library: Path | None = None
    buffer_samples: int = Field(default=262_144, ge=1024, le=4_194_304)
    single_buffer: bool = False


class CapturePlan(BaseModel):
    status: Literal["dry_run"] = "dry_run"
    uri: str
    output: str
    metadata_output: str
    requested_rate_hz: int
    requested_center_hz: int
    requested_seconds: float
    estimated_frames: int
    estimated_bytes: int
    free_disk_bytes: int
    mode: Literal["streaming", "single_buffer"]


class Controls(BaseModel):
    sampling_frequency: int = Field(gt=0)
    rf_bandwidth: int = Field(gt=0)
    frequency: int = Field(gt=0)
    gain_control_mode: str


class CaptureResult(BaseModel):
    status: Literal["captured"] = "captured"
    uri: str
    output: str
    metadata_output: str
    requested_seconds: float
    original: Controls
    applied: Controls
    frames: int
    data_bytes: int
    sample_duration_seconds: float
    wall_seconds: float
    effective_samples_per_second: float
    throughput_ratio: float
    throughput_limited: bool | None = Field(
        description="Streaming throughput below 95% of applied rate; null for a single snapshot."
    )
    dropped_samples: None = None
    continuity_verified: bool = False
    mode: Literal["streaming", "single_buffer"]
    channels: list[str] = Field(default_factory=lambda: ["I", "Q"])
    container: str = "WAV PCM signed int16 little-endian"
    valid_bits: int
    adc_shift: int = 0
    limitation: str = (
        "No sample sequence numbers or timestamps: continuity and dropped samples are unknown. "
        "Sample duration is frames / applied rate; elapsed wall time may be longer."
    )


def metadata_path(output: Path) -> Path:
    return output.with_suffix(output.suffix + ".json")


def _size(frames: int) -> int:
    size = frames * 4
    if frames < 1 or size > WAV_MAX_DATA:
        raise ValueError("Capture must fit a classic WAV below 4 GiB; reduce --seconds or --rate.")
    return size + 44


def plan_capture(config: CaptureConfig) -> CapturePlan:
    output = config.output.expanduser().absolute()
    if str(config.output) == "-":
        raise ValueError("Use an output WAV path, not '-'; JSON results are written to stdout.")
    for path in (output, metadata_path(output)):
        if path.exists() or path.is_symlink():
            raise FileExistsError(f"Refusing to overwrite {path}; choose a new --output.")
    if not output.parent.is_dir():
        raise ValueError(f"Output directory does not exist: {output.parent}")
    frames = math.ceil(config.rate * config.seconds)
    size = _size(frames)
    if config.single_buffer and frames * 4 > 128 * 1024**2:
        raise ValueError("Single-buffer capture is limited to 128 MiB; reduce --seconds or --rate.")
    free = shutil.disk_usage(output.parent).free
    if size + 1_048_576 > free:
        raise ValueError("Insufficient disk space for capture and metadata; reduce --seconds.")
    return CapturePlan(
        uri=config.uri,
        output=str(output),
        metadata_output=str(metadata_path(output)),
        requested_rate_hz=config.rate,
        requested_center_hz=config.center,
        requested_seconds=config.seconds,
        estimated_frames=frames,
        estimated_bytes=size,
        free_disk_bytes=free,
        mode="single_buffer" if config.single_buffer else "streaming",
    )


class DataFormat(c.Structure):
    _fields_ = [
        ("length", c.c_uint),
        ("bits", c.c_uint),
        ("shift", c.c_uint),
        ("is_signed", c.c_bool),
        ("is_fully_defined", c.c_bool),
        ("is_be", c.c_bool),
        ("with_scale", c.c_bool),
        ("scale", c.c_double),
        ("repeat", c.c_uint),
    ]


class PlutoReceiver:
    """Own one libiio context and RX buffer. Constructing does not touch hardware."""

    def __init__(self, uri: str, library: Path | None = None):
        name = (
            str(library)
            if library
            else (
                str(BUNDLED_LIBRARY)
                if BUNDLED_LIBRARY.is_file()
                else ctypes.util.find_library("iio")
            )
        )
        if not name:
            raise OSError("libiio 0.x not found; supply --library /path/to/libiio.")
        try:
            # SDR++'s dylib uses a relative libusb install name on macOS.
            if sys.platform == "darwin" and Path(name).is_file():
                for usb in sorted(Path(name).parent.glob("*usb*.*dylib")):
                    c.CDLL(str(usb), mode=c.RTLD_GLOBAL)
            self.lib = c.CDLL(name, use_errno=True)
        except OSError as exc:
            raise OSError(
                "Cannot load libiio; for SDR++ on macOS run with "
                "DYLD_LIBRARY_PATH=/Applications/SDR++.app/Contents/Frameworks. " + str(exc)
            ) from exc
        self.uri = uri
        self.context = None
        self.buffer = None
        self.channels = []
        self.enabled = []
        self.controls = {}
        self._bind()

    def _bind(self):
        ptr, string = c.c_void_p, c.c_char_p
        declarations = {
            "iio_create_context_from_uri": (ptr, [string]),
            "iio_context_destroy": (None, [ptr]),
            "iio_context_set_timeout": (c.c_int, [ptr, c.c_uint]),
            "iio_context_find_device": (ptr, [ptr, string]),
            "iio_device_find_channel": (ptr, [ptr, string, c.c_bool]),
            "iio_channel_attr_read": (c.c_ssize_t, [ptr, string, ptr, c.c_size_t]),
            "iio_channel_attr_write": (c.c_ssize_t, [ptr, string, string]),
            "iio_channel_get_data_format": (c.POINTER(DataFormat), [ptr]),
            "iio_channel_get_index": (c.c_long, [ptr]),
            "iio_channel_enable": (None, [ptr]),
            "iio_channel_disable": (None, [ptr]),
            "iio_device_create_buffer": (ptr, [ptr, c.c_size_t, c.c_bool]),
            "iio_device_get_sample_size": (c.c_ssize_t, [ptr]),
            "iio_buffer_destroy": (None, [ptr]),
            "iio_buffer_refill": (c.c_ssize_t, [ptr]),
            "iio_buffer_step": (c.c_ssize_t, [ptr]),
            "iio_buffer_first": (ptr, [ptr, ptr]),
            "iio_buffer_end": (ptr, [ptr]),
        }
        for name, (result, args) in declarations.items():
            try:
                fn = getattr(self.lib, name)
            except AttributeError as exc:
                raise OSError(
                    "This tool requires libiio 0.x; supply a compatible --library."
                ) from exc
            fn.restype, fn.argtypes = result, args
            setattr(self, name.removeprefix("iio_"), fn)

    @staticmethod
    def _required(value, label):
        if not value:
            raise OSError(c.get_errno(), f"libiio could not open {label}")
        return value

    def open(self):
        self.context = self._required(self.create_context_from_uri(self.uri.encode()), self.uri)
        try:
            status = self.context_set_timeout(self.context, 5000)
            if status < 0:
                raise OSError(-status, "setting libiio timeout")
            phy = self._required(self.context_find_device(self.context, b"ad9361-phy"), "RX phy")
            rx = self._required(self.device_find_channel(phy, b"voltage0", False), "RX control")
            lo = self._required(self.device_find_channel(phy, b"altvoltage0", True), "RX LO")
            self.controls = {key: (lo if key == "frequency" else rx) for key in CONTROLS}
            self.device = self._required(
                self.context_find_device(self.context, b"cf-ad9361-lpc"), "RX streaming device"
            )
            self.channels = [
                self._required(self.device_find_channel(self.device, name, False), name.decode())
                for name in (b"voltage0", b"voltage1")
            ]
        except BaseException:
            self.close()
            raise

    def read(self, key: str) -> str:
        data = c.create_string_buffer(2048)
        count = self.channel_attr_read(self.controls[key], key.encode(), data, len(data))
        if count < 0:
            raise OSError(-count, f"read RX {key}")
        return data.value.decode().strip()

    def write(self, key: str, value: str):
        count = self.channel_attr_write(self.controls[key], key.encode(), str(value).encode())
        if count < 0:
            raise OSError(-count, f"write RX {key}")

    def start(self, samples: int) -> int:
        formats = []
        for expected, channel in enumerate(self.channels):
            fmt = self.channel_get_data_format(channel).contents
            if (
                fmt.length != 16
                or fmt.bits not in (12, 16)
                or fmt.shift != 0
                or not fmt.is_signed
                or fmt.is_be
                or fmt.repeat != 1
                or self.channel_get_index(channel) != expected
            ):
                raise ValueError("Unsupported RX format; expected little-endian signed I/Q s12/16.")
            formats.append(fmt.bits)
        if formats[0] != formats[1]:
            raise ValueError("I and Q formats differ")
        for channel in self.channels:
            self.channel_enable(channel)
            self.enabled.append(channel)
        if self.device_get_sample_size(self.device) != 4:
            raise ValueError("Unexpected RX scan elements; expected only I and Q (4 bytes).")
        self.buffer = self._required(
            self.device_create_buffer(self.device, samples, False), "RX buffer"
        )
        if self.buffer_step(self.buffer) != 4:
            raise ValueError("Unexpected RX buffer stride; expected 4 bytes per IQ pair.")
        return formats[0]

    def refill(self) -> bytes:
        count = self.buffer_refill(self.buffer)
        if count < 0:
            raise OSError(-count, "RX buffer refill")
        first = self.buffer_first(self.buffer, self.channels[0])
        end = self.buffer_end(self.buffer)
        if not first or not end or count <= 0 or count % 4 or end - first != count:
            raise ValueError("Malformed RX buffer; refusing misaligned IQ data.")
        return c.string_at(first, count)

    def stop(self):
        if self.buffer:
            self.buffer_destroy(self.buffer)
            self.buffer = None
        for channel in self.enabled:
            self.channel_disable(channel)
        self.enabled.clear()

    def close(self):
        try:
            self.stop()
        finally:
            if self.context:
                self.context_destroy(self.context)
                self.context = None


def _read_controls(receiver) -> Controls:
    return Controls(**{key: receiver.read(key) for key in CONTROLS})


def capture_iq(
    config: CaptureConfig,
    *,
    receiver_factory: Callable = PlutoReceiver,
    clock: Callable[[], float] = time.monotonic,
) -> CaptureResult:
    """Capture a bounded number of IQ frames and restore changed RX controls on every exit."""
    plan = plan_capture(config)
    output, metadata = Path(plan.output), Path(plan.metadata_output)
    receiver = receiver_factory(config.uri, config.library)
    original = None
    touched = []
    created = []
    errors = []
    result = None
    try:
        receiver.open()
        original = _read_controls(receiver)
        desired = {
            "sampling_frequency": config.rate,
            "rf_bandwidth": config.bandwidth or min(config.rate, 56_000_000),
            "frequency": config.center,
            "gain_control_mode": config.gain_mode or original.gain_control_mode,
        }
        for key, value in desired.items():
            if str(value) != str(getattr(original, key)):
                touched.append(key)  # A failed write may still have changed hardware.
                receiver.write(key, str(value))
        applied = _read_controls(receiver)
        target = math.ceil(applied.sampling_frequency * config.seconds)
        actual_size = _size(target)
        if actual_size + 1_048_576 > shutil.disk_usage(output.parent).free:
            raise ValueError("Applied sample rate requires more free disk space than available.")
        if config.single_buffer and target * 4 > 128 * 1024**2:
            raise ValueError("Applied rate exceeds single-buffer limit of 128 MiB.")
        started = clock()
        valid_bits = receiver.start(target if config.single_buffer else config.buffer_samples)
        # Both files use exclusive creation, including when another process races the preview.
        with output.open("xb") as raw:
            created.append(output)
            with metadata.open("x", encoding="utf-8") as sidecar:
                created.append(metadata)
                with wave.open(raw, "wb") as wav:
                    wav.setnchannels(2)
                    wav.setsampwidth(2)
                    wav.setframerate(applied.sampling_frequency)
                    frames = 0
                    while frames < target:
                        if clock() - started > max(10, config.seconds * 3):
                            raise TimeoutError("Capture exceeded wall-time bound; reduce --rate.")
                        data = receiver.refill()
                        if not data or len(data) % 4:
                            raise ValueError("Malformed RX buffer; expected complete IQ pairs.")
                        if config.single_buffer and len(data) != target * 4:
                            raise ValueError(
                                "Single RX buffer did not contain the requested frame count."
                            )
                        data = data[: (target - frames) * 4]
                        wav.writeframesraw(data)
                        frames += len(data) // 4
                    elapsed = max(clock() - started, 1e-9)
                duration = frames / applied.sampling_frequency
                ratio = duration / elapsed
                result = CaptureResult(
                    uri=config.uri,
                    output=str(output),
                    metadata_output=str(metadata),
                    requested_seconds=config.seconds,
                    original=original,
                    applied=applied,
                    frames=frames,
                    data_bytes=frames * 4,
                    sample_duration_seconds=duration,
                    wall_seconds=elapsed,
                    effective_samples_per_second=frames / elapsed,
                    throughput_ratio=ratio,
                    throughput_limited=(ratio < 0.95) if not config.single_buffer else None,
                    valid_bits=valid_bits,
                    mode=plan.mode,
                )
                sidecar.write(result.model_dump_json(indent=2) + "\n")
    except BaseException as exc:  # noqa: BLE001 - retain interrupts until RX cleanup completes
        errors.append(exc)
    finally:
        try:
            receiver.stop()
        except BaseException as exc:  # noqa: BLE001 - retain interrupts until RX cleanup completes
            errors.append(exc)
        if original is not None:
            # Restore all touched controls, even if one restoration fails.
            for key in CONTROLS if touched else ():
                try:
                    receiver.write(key, str(getattr(original, key)))
                except BaseException as exc:  # noqa: BLE001 - retain interrupts until RX cleanup completes
                    errors.append(exc)
            for key in CONTROLS if touched else ():
                try:
                    if receiver.read(key) != str(getattr(original, key)):
                        raise OSError(f"RX {key} restoration readback differs")
                except BaseException as exc:  # noqa: BLE001 - retain interrupts until RX cleanup completes
                    errors.append(exc)
        try:
            receiver.close()
        except BaseException as exc:  # noqa: BLE001 - retain interrupts until RX cleanup completes
            errors.append(exc)
    if errors:
        for path in created:
            path.unlink(missing_ok=True)
        if len(errors) == 1:
            raise errors[0]
        raise BaseExceptionGroup("Capture or RX restoration failed", errors)
    return result


@app.command("capture")
def capture_command(
    output: Annotated[Path, typer.Option(help="New output WAV path; sidecar is PATH.wav.json.")],
    center: Annotated[int, typer.Option(help="RX center frequency in Hz.")],
    uri: str = "ip:pluto.local",
    rate: int = 5_000_000,
    seconds: float = 10,
    bandwidth: int | None = None,
    gain_mode: str | None = None,
    library: Path | None = None,
    single_buffer: Annotated[bool, typer.Option(help="One hardware buffer, max 128 MiB.")] = False,
    yes: Annotated[
        bool, typer.Option("--yes", help="Authorize bounded capture and file writes.")
    ] = False,
    dry_run: Annotated[
        bool, typer.Option(help="Preview size/disk checks without opening Pluto.")
    ] = False,
    as_json: Annotated[bool, typer.Option("--json", help="Emit typed JSON to stdout.")] = False,
):
    """Write receive-only stereo IQ WAV. Example: capture --center 454200000 --output iq.wav --yes.

    Close SDR++ first. RX rate, bandwidth, LO, and gain mode are restored on exit.
    Continuity cannot be verified: USB throughput may be below the applied sample rate.
    """
    try:
        config = CaptureConfig(
            uri=uri,
            rate=rate,
            center=center,
            seconds=seconds,
            output=output,
            bandwidth=bandwidth,
            gain_mode=gain_mode,
            library=library,
            single_buffer=single_buffer,
        )
        plan = plan_capture(config)
        if dry_run:
            typer.echo(
                plan.model_dump_json()
                if as_json
                else f"Would capture {plan.estimated_frames} IQ frames, "
                f"about {plan.estimated_bytes} bytes to {plan.output}"
            )
            return
        if not yes:
            raise typer.BadParameter("Pass --yes to capture, or --dry-run to preview.")
        result = capture_iq(config)
        if result.throughput_limited:
            typer.echo(
                "Warning: measured capture throughput is below the applied sample rate; "
                "sample continuity is unknown.",
                err=True,
            )
        typer.echo(
            result.model_dump_json()
            if as_json
            else f"Captured {result.frames} IQ frames to {result.output}; "
            f"{result.sample_duration_seconds:.3f} sample seconds in "
            f"{result.wall_seconds:.3f} wall seconds. Continuity unknown."
        )
    except typer.BadParameter:
        raise
    except ValueError as exc:
        raise typer.BadParameter(str(exc)) from exc
    except Exception as exc:
        typer.echo(f"Error: {exc}", err=True)
        raise typer.Exit(1) from exc


# Keep `capture` as a group subcommand when invoked through `python -m`.
@app.callback()
def main():
    pass


if __name__ == "__main__":
    app()
