"""The recovery gate must reject damaged backups before preparing write files."""

import hashlib
import importlib.util
import json
import struct
import sys
import zlib
from pathlib import Path

import pytest

spec = importlib.util.spec_from_file_location(
    "prepare_recovery", Path(__file__).with_name("prepare_recovery.py")
)
recovery = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = recovery
spec.loader.exec_module(recovery)


@pytest.fixture
def backup(tmp_path):
    data = [bytearray(size) for _, size in recovery.EXPECTED]
    data[0][32:36] = bytes.fromhex("665599aa")
    data[1][:4] = struct.pack("<I", zlib.crc32(data[1][4:]))
    data[2][:2] = bytes.fromhex("8519")
    data[3][:8] = bytes.fromhex("d00dfeed") + struct.pack(">I", 44)
    parts = []
    for index, ((name, size), raw) in enumerate(zip(recovery.EXPECTED, data, strict=True)):
        filename = f"mtd{index}.bin"
        (tmp_path / filename).write_bytes(raw)
        parts.append(
            {
                "partition": index,
                "name": name,
                "bytes": size,
                "file": filename,
                "sha256": hashlib.sha256(raw).hexdigest(),
                "verified_second_device_read": True,
            }
        )
    (tmp_path / "manifest.json").write_text(
        json.dumps(
            {"partitions": parts, "full_flash_sha256": hashlib.sha256(b"".join(data)).hexdigest()}
        )
    )
    return tmp_path


def test_validates_complete_backup_without_writing(backup):
    plan = recovery.validate_backup(backup)
    assert plan.status == "verified"
    assert len(plan.files) == 4
    assert plan.device_changed is False
    assert not (backup / "recovery").exists()


def test_rejects_changed_partition(backup):
    with (backup / "mtd0.bin").open("r+b") as stream:
        stream.seek(100)
        stream.write(b"broken")
    with pytest.raises(ValueError, match="size/hash"):
        recovery.validate_backup(backup)


def test_requires_second_read_verification(backup):
    path = backup / "manifest.json"
    manifest = json.loads(path.read_text())
    manifest["partitions"][2]["verified_second_device_read"] = False
    path.write_text(json.dumps(manifest))
    with pytest.raises(ValueError, match="independent"):
        recovery.validate_backup(backup)


def test_rejects_partition_symlink(backup):
    original = backup / "mtd0.bin"
    renamed = backup / "actual.bin"
    original.rename(renamed)
    original.symlink_to(renamed)
    with pytest.raises(ValueError, match="regular files"):
        recovery.validate_backup(backup)
