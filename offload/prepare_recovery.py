#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["typer", "pydantic"]
# ///
"""Validate a device-specific raw backup and prepare DFU recovery copies."""

import hashlib
import json
import shutil
import struct
import subprocess
import sys
import zlib
from pathlib import Path

import typer
from pydantic import BaseModel

app = typer.Typer(pretty_exceptions_enable=False)

EXPECTED = [("boot", 0x100000), ("uboot-env", 0x20000), ("nvmfs", 0xE0000), ("linux", 0x1E00000)]
TARGETS = {
    "boot": "boot.dfu",
    "uboot-env": "uboot-env.dfu",
    "nvmfs": "spare.dfu",
    "linux": "firmware.dfu",
}


class RecoveryFile(BaseModel):
    partition: str
    target: str
    source: str
    bytes: int
    sha256: str
    recovery_file: str


class RecoveryPlan(BaseModel):
    status: str
    backup: str
    files: list[RecoveryFile]
    device_changed: bool = False


def validate_backup(directory: Path) -> RecoveryPlan:
    manifest = json.loads((directory / "manifest.json").read_text())
    parts = manifest["partitions"]
    if len(parts) != 4:
        raise ValueError("Expected all four Rev.B NOR partitions.")
    files = []
    data = []
    for index, ((name, size), part) in enumerate(zip(EXPECTED, parts, strict=True)):
        if part["partition"] != index or part["name"] != name or part["bytes"] != size:
            raise ValueError(f"Partition {index} does not match the verified Rev.B flash layout.")
        source = directory / part["file"]
        if source.parent != directory or source.is_symlink():
            raise ValueError("Backup partition paths must be regular files within the backup.")
        raw = source.read_bytes()
        digest = hashlib.sha256(raw).hexdigest()
        if len(raw) != size or digest != part["sha256"]:
            raise ValueError(f"Partition {index} backup size/hash verification failed.")
        if not part.get("verified_second_device_read"):
            raise ValueError(f"Partition {index} lacks independent device-read verification.")
        data.append(raw)
        files.append(
            RecoveryFile(
                partition=name,
                target=TARGETS[name],
                source=str(source),
                bytes=size,
                sha256=digest,
                recovery_file=str(directory / "recovery" / f"original-{name}.dfu"),
            )
        )
    if data[0][32:36] != bytes.fromhex("665599aa"):
        raise ValueError("Zynq boot header invalid.")
    if struct.unpack("<I", data[1][:4])[0] != zlib.crc32(data[1][4:]):
        raise ValueError("U-Boot environment CRC invalid.")
    if data[2][:2] != bytes.fromhex("8519"):
        raise ValueError("NVM JFFS2 header invalid.")
    if data[3][:4] != bytes.fromhex("d00dfeed"):
        raise ValueError("Firmware FIT header invalid.")
    fit_size = struct.unpack(">I", data[3][4:8])[0]
    if not 40 < fit_size <= len(data[3]):
        raise ValueError("Firmware FIT length invalid.")
    if hashlib.sha256(b"".join(data)).hexdigest() != manifest["full_flash_sha256"]:
        raise ValueError("Full-flash verification failed.")
    return RecoveryPlan(status="verified", backup=str(directory), files=files)


@app.command()
def main(
    backup: Path,
    yes: bool = False,
    dry_run: bool = False,
    json_output: bool = typer.Option(False, "--json"),
):
    """Verify BACKUP; add --yes to create original DFU copies. Never accesses the radio.

    Example: uv run offload/prepare_recovery.py output/pluto-backups/TIMESTAMP --yes --json
    Exit codes: 0 verified/prepared, 1 verification/tool failure, 2 bad arguments.
    """
    try:
        backup = backup.expanduser().resolve()
        plan = validate_backup(backup)
        if yes and not dry_run:
            suffix = shutil.which("dfu-suffix")
            if not suffix:
                raise ValueError("Install dfu-util through Homebrew to provide dfu-suffix.")
            recovery = backup / "recovery"
            recovery.mkdir(mode=0o700, exist_ok=True)
            for item in plan.files:
                target = Path(item.recovery_file)
                raw = Path(item.source).read_bytes()
                if target.exists():
                    existing = target.read_bytes()
                    if existing[:-16] != raw or len(existing) != len(raw) + 16:
                        raise ValueError(f"Refusing to replace unexpected recovery file: {target}")
                else:
                    target.write_bytes(raw)
                    target.chmod(0o600)
                    subprocess.run(
                        [suffix, "--vid", "0x0456", "--pid", "0xb673", "--add", str(target)],
                        check=True,
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.PIPE,
                    )
                subprocess.run(
                    [suffix, "--check", str(target)],
                    check=True,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.PIPE,
                )
                if target.read_bytes()[:-16] != raw:
                    raise ValueError("DFU suffix operation unexpectedly changed a backup payload.")
            plan.status = "prepared"
            (recovery / "plan.json").write_text(plan.model_dump_json(indent=2) + "\n")
        if json_output:
            print(plan.model_dump_json())
        else:
            print(f"{plan.status}: {backup} (four partitions; no device changes)")
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as exc:
        print(f"Recovery preparation failed: {exc}", file=sys.stderr)
        raise typer.Exit(1) from exc


if __name__ == "__main__":
    app()
