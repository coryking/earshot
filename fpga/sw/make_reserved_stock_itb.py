"""Reproduce the installed stock-PL Pluto boot image with a DMA reservation.

    python3 fpga/sw/make_reserved_stock_itb.py <verified-original.itb> <out.itb>

The input is the raw firmware partition backup, not a pluto.frm updater file.
Only the three DTBs change. The original FIT length and all stock image data
are retained, so U-Boot's existing fit_size setting does not need an edit.
This builds a file; it never writes a device.
"""

from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path

import fit
from make_frm import add_reserved_memory


def checked_images(root: fit.Node) -> dict[str, bytes]:
    data = {}
    for image in root.child("images").children:
        blob = image.props["data"]
        data[image.name] = blob
        for child in image.children:
            if child.name.startswith("hash"):
                if child.props.get("algo") != b"md5\0" or hashlib.md5(blob).digest() != child.props.get("value"):
                    raise ValueError(f"{image.name}: stock hash mismatch or unsupported hash")
    return data


def build(stock: Path, out: Path) -> dict:
    if stock.resolve() == out.resolve():
        raise ValueError("refusing to overwrite the stock backup")
    raw = stock.read_bytes()
    root, meta = fit.parse(raw)
    original = checked_images(root)
    changed = []
    for image in root.child("images").children:
        if image.props.get("type") == b"flat_dt\0":
            image.props["data"] = add_reserved_memory(image.props["data"], "dma_probe")
            changed.append(image.name)
    if len(changed) != 3:
        raise ValueError(f"expected three Pluto DTBs, found {len(changed)}")
    built = fit.serialize(root, meta)
    if len(built) > len(raw):
        raise ValueError("reserved FIT exceeds original fit_size")
    padded = built + bytes(len(raw) - len(built))
    check, _ = fit.parse(padded)
    current = checked_images(check)
    for name, blob in original.items():
        if name not in changed and current[name] != blob:
            raise ValueError(f"{name}: stock image data changed")
    for name in changed:
        dt, _ = fit.parse(current[name])
        node = dt.find("/reserved-memory/dma_probe@4000000")
        if node.props.get("no-map") != b"" or node.props.get("reg") != fit.u32(0x04000000, 0x04000000):
            raise ValueError(f"{name}: DMA reservation is wrong")
    out.write_bytes(padded)
    return {"stock_sha256": hashlib.sha256(raw).hexdigest(),
            "reserved_sha256": hashlib.sha256(padded).hexdigest(),
            "fit_size": len(raw), "reserved_fit_bytes": len(built),
            "dtbs_changed": changed,
            "stock_images_unchanged": [name for name in original if name not in changed]}


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    print(json.dumps(build(Path(sys.argv[1]), Path(sys.argv[2])), indent=2))
