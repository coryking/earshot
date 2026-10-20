"""Build a Pluto firmware image (pluto.frm) carrying the channelizer bitstream.

    python3 fpga/sw/make_frm.py <stock pluto.frm> <system_top.bit> <out pluto.frm>

Starts from ADI's public release image (plutosdr-fw v0.37, the version this unit
runs) and changes exactly two things:

* images/fpga@1: the new bitstream (.bit with header, as in the stock image), with
  its md5 hash node recomputed so U-Boot's FIT verification passes;
* every flat_dt image: a `reserved-memory` node that takes 64 MiB at 64 MiB out of
  Linux's hands (no-map) as the channelizer's DMA area, so user space can map it
  uncached through /dev/mem without colliding with kernel allocations.
  Placement (from this unit's U-Boot environment and boot log): the FIT is staged at
  0x02080000 (about 11 MiB), the kernel runs from 0x8000, initrd_high = fdt_high =
  0x20000000 puts the initrd and FDT just below 512 MiB, and the kernel's 256 MiB
  CMA pool needs a contiguous span (today 0x0E400000-0x1E400000). 0x04000000-
  0x08000000 avoids all of them and leaves ~380 MiB contiguous above it for CMA.

Kernel and ramdisk are carried over byte-for-byte (their hashes are checked). The
output is the FIT plus the md5 trailer ADI's update path expects. Nothing is
written to any device here.
"""

from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import fit  # noqa: E402

DMA_BASE = 0x04000000   # see module docstring for why 64 MiB at 64 MiB
DMA_SIZE = 0x04000000


def add_reserved_memory(dtb: bytes) -> bytes:
    root, meta = fit.parse(dtb)
    if any(c.name == "reserved-memory" for c in root.children):
        raise SystemExit("device tree already has reserved-memory; refusing to merge")
    rm = fit.Node("reserved-memory", {
        "#address-cells": fit.u32(1), "#size-cells": fit.u32(1), "ranges": b""})
    rm.add(fit.Node(f"chan_dma@{DMA_BASE:x}", {
        "reg": fit.u32(DMA_BASE, DMA_SIZE), "no-map": b""}))
    root.add(rm)
    return fit.serialize(root, meta)


def main(stock: Path, bit: Path, out: Path) -> None:
    raw = stock.read_bytes()
    itb, trailer = raw[:-33], raw[-33:]
    if hashlib.md5(itb).hexdigest().encode() + b"\n" != trailer:
        raise SystemExit("stock image md5 trailer does not match")
    root, meta = fit.parse(itb)
    images = root.child("images")
    report = {"stock_md5": hashlib.md5(itb).hexdigest()}
    for img in images.children:
        h = next((c for c in img.children if c.name.startswith("hash")), None)
        if h is not None and h.props.get("algo") == b"md5\0":
            ok = hashlib.md5(img.props["data"]).digest() == h.props["value"]
            report[f"{img.name}_stock_hash_ok"] = ok
            if not ok:
                raise SystemExit(f"{img.name}: stock hash mismatch")
    new_bit = bit.read_bytes()
    if new_bit[:4] != bytes.fromhex("00090ff0"):
        raise SystemExit("expected a .bit file with header (as in the stock image)")
    fpga = images.child("fpga@1")
    fpga.props["data"] = new_bit
    fpga.child("hash@1").props["value"] = hashlib.md5(new_bit).digest()
    for img in images.children:
        if img.props.get("type") == b"flat_dt\0":
            img.props["data"] = add_reserved_memory(img.props["data"])
    new_itb = fit.serialize(root, meta)
    # re-parse to prove the result is well formed and the hashes hold
    chk, _ = fit.parse(new_itb)
    for img in chk.child("images").children:
        h = next((c for c in img.children if c.name.startswith("hash")), None)
        if h is not None:
            if h.props.get("algo") != b"md5\0":
                raise SystemExit(f"{img.name}: unsupported hash {h.props.get('algo')!r}")
            if hashlib.md5(img.props["data"]).digest() != h.props["value"]:
                raise SystemExit(f"{img.name}: hash mismatch after rebuild")
    md5 = hashlib.md5(new_itb).hexdigest()
    out.write_bytes(new_itb + md5.encode() + b"\n")
    report.update({"bitstream_sha256": hashlib.sha256(new_bit).hexdigest(),
                   "itb_md5": md5, "frm_sha256": hashlib.sha256(out.read_bytes()).hexdigest(),
                   "frm_bytes": out.stat().st_size,
                   "reserved_dma": f"0x{DMA_BASE:08x}+0x{DMA_SIZE:x}"})
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit(__doc__)
    if Path(sys.argv[3]).resolve() == Path(sys.argv[1]).resolve():
        raise SystemExit("refusing to overwrite the stock image")
    main(Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3]))
