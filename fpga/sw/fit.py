"""Minimal flattened-device-tree reader/writer for Pluto FIT images and DTBs.

The Pluto firmware (pluto.frm) is a FIT image (an FDT) plus a 32-hex-digit MD5 and
a newline. This module parses and re-serializes FDT blobs without external tools
(dtc/mkimage are not installed here), so a firmware image can be rebuilt with a
different bitstream and device tree while every other component stays bit-identical.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass, field

FDT_MAGIC = 0xD00DFEED
BEGIN_NODE, END_NODE, PROP, NOP, END = 1, 2, 3, 4, 9


@dataclass
class Node:
    name: str
    props: dict[str, bytes] = field(default_factory=dict)
    children: list["Node"] = field(default_factory=list)

    def child(self, name: str) -> "Node":
        for c in self.children:
            if c.name == name:
                return c
        raise KeyError(name)

    def find(self, path: str) -> "Node":
        n = self
        for part in [p for p in path.split("/") if p]:
            n = n.child(part)
        return n

    def add(self, node: "Node") -> "Node":
        self.children.append(node)
        return node


def _align(n: int) -> int:
    return (n + 3) & ~3


def parse(blob: bytes) -> tuple[Node, dict]:
    (magic, totalsize, off_struct, off_strings, off_rsv, version, last_comp, boot_cpu,
     size_strings, size_struct) = struct.unpack(">10I", blob[:40])
    if magic != FDT_MAGIC:
        raise ValueError("not an FDT")
    strings = blob[off_strings:off_strings + size_strings]
    rsv = []
    o = off_rsv
    while True:
        addr, size = struct.unpack(">QQ", blob[o:o + 16])
        o += 16
        if addr == 0 and size == 0:
            break
        rsv.append((addr, size))
    pos = off_struct
    stack: list[Node] = []
    root = None
    while True:
        (tok,) = struct.unpack(">I", blob[pos:pos + 4])
        pos += 4
        if tok == BEGIN_NODE:
            end = blob.index(b"\0", pos)
            node = Node(blob[pos:end].decode())
            pos = _align(end + 1)
            if stack:
                stack[-1].children.append(node)
            else:
                root = node
            stack.append(node)
        elif tok == END_NODE:
            stack.pop()
        elif tok == PROP:
            ln, nameoff = struct.unpack(">II", blob[pos:pos + 8])
            pos += 8
            name = strings[nameoff:strings.index(b"\0", nameoff)].decode()
            stack[-1].props[name] = blob[pos:pos + ln]
            pos = _align(pos + ln)
        elif tok == NOP:
            continue
        elif tok == END:
            break
        else:
            raise ValueError(f"bad token {tok}")
    return root, {"version": version, "last_comp": last_comp, "boot_cpu": boot_cpu, "rsv": rsv}


def serialize(root: Node, meta: dict) -> bytes:
    strings = bytearray()
    offsets: dict[str, int] = {}

    def stroff(name: str) -> int:
        if name not in offsets:
            offsets[name] = len(strings)
            strings.extend(name.encode() + b"\0")
        return offsets[name]

    body = bytearray()

    def emit(n: Node) -> None:
        body.extend(struct.pack(">I", BEGIN_NODE))
        nm = n.name.encode() + b"\0"
        body.extend(nm + b"\0" * (_align(len(nm)) - len(nm)))
        for k, v in n.props.items():
            body.extend(struct.pack(">III", PROP, len(v), stroff(k)))
            body.extend(v + b"\0" * (_align(len(v)) - len(v)))
        for c in n.children:
            emit(c)
        body.extend(struct.pack(">I", END_NODE))

    emit(root)
    body.extend(struct.pack(">I", END))
    rsv = b"".join(struct.pack(">QQ", a, s) for a, s in meta["rsv"]) + b"\0" * 16
    off_rsv = 40
    off_struct = _align(off_rsv + len(rsv))
    off_strings = off_struct + len(body)
    total = off_strings + len(strings)
    hdr = struct.pack(">10I", FDT_MAGIC, total, off_struct, off_strings, off_rsv,
                      meta["version"], meta["last_comp"], meta["boot_cpu"], len(strings),
                      len(body))
    out = bytearray(hdr + rsv)
    out.extend(b"\0" * (off_struct - len(out)))
    out.extend(body)
    out.extend(strings)
    return bytes(out)


def u32(*vals: int) -> bytes:
    return b"".join(struct.pack(">I", v) for v in vals)


def cstr(*vals: str) -> bytes:
    return b"".join(v.encode() + b"\0" for v in vals)
