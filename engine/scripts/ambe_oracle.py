#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["unicorn==2.1.4"]
# ///
"""Offline test oracle for the AMBE+2 port (src/ambe): the MD-380 D002.032 firmware decoder
under the Unicorn emulator (Cortex-M4). Test tooling only; the engine never runs firmware code.

Needs the firmware and RAM snapshot from scripts/fetch-md380.sh (MD380_DIR, default
engine/.native-md380). Run it in a memory-capped scope:
    systemd-run --user --scope -p MemoryMax=2G -p MemorySwapMax=0 scripts/ambe_oracle.py ...

  decode  <frames> <out.raw>                 decode a frame file to s16le 8 kHz PCM
  capture <func hex> <frames> <out.rec> [--max N] [--skip K] [--every M] [--frames F]
                                             record calls of one routine (see src/ambe/testvec.rs)

Frame files: one frame per line as 49 characters of 0/1 (a line may have leading fields, e.g.
"F 2 0 0 0101..."; "R" lines reset the decoder; "X <hex> <bits>" also passes <hex> as the
decoder's frame-flags argument), or a .bits file with 49 bytes per frame.
"""

import os
import struct
import sys
from collections import Counter, defaultdict

from unicorn import (Uc, UC_ARCH_ARM, UC_MODE_THUMB, UC_MODE_MCLASS, UC_HOOK_CODE, UC_HOOK_BLOCK,
                     UC_HOOK_MEM_READ, UC_HOOK_MEM_WRITE, UC_HOOK_MEM_UNMAPPED)
from unicorn.arm_const import *

FW_DIR = os.path.join(os.environ.get("MD380_DIR", os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".native-md380")), "firmware")
FLASH = 0x0800C000
FLASH_SIZE = 0x100000
SRAM = 0x20000000
SRAM_SIZE = 0x20000
CCM = 0x10000000
CCM_SIZE = 0x10000
STACK = 0x21000000
STACK_SIZE = 0x10000
HALT = 0x30000000

DECODE_WAV = 0x08051248
INBUF = 0x20011C8E
OUT0 = 0x20011AA8
OUT1 = 0x20011B48
MYSTERY = 0x20011224


class Oracle:
    def __init__(self, cpu_model=None):
        self.fw = open(os.path.join(FW_DIR, "D002.032.bin"), "rb").read()
        self.ram0 = open(os.path.join(FW_DIR, "d02032-core.img"), "rb").read()
        uc = Uc(UC_ARCH_ARM, UC_MODE_THUMB | UC_MODE_MCLASS)
        try:
            from unicorn import UC_CTL_CPU_MODEL  # noqa
            from unicorn.arm_const import UC_CPU_ARM_CORTEX_M4
            uc.ctl_set_cpu_model(UC_CPU_ARM_CORTEX_M4)
        except Exception as e:  # pragma: no cover
            print("cpu model:", e, file=sys.stderr)
        uc.mem_map(FLASH, FLASH_SIZE)
        uc.mem_map(SRAM, SRAM_SIZE)
        uc.mem_map(CCM, CCM_SIZE)
        uc.mem_map(STACK, STACK_SIZE)
        uc.mem_map(HALT, 0x1000)
        uc.mem_write(FLASH, self.fw)
        uc.mem_write(HALT, b"\xfe\xe7" * 16)  # b .
        self.uc = uc
        self.reset()

    def reset(self):
        self.uc.mem_write(SRAM, self.ram0)
        self.uc.mem_write(CCM, b"\0" * CCM_SIZE)
        self.uc.mem_write(STACK, b"\0" * STACK_SIZE)

    def call(self, entry, args, stack_args=(), max_insn=50_000_000):
        uc = self.uc
        regs = [UC_ARM_REG_R0, UC_ARM_REG_R1, UC_ARM_REG_R2, UC_ARM_REG_R3]
        for r, v in zip(regs, args):
            uc.reg_write(r, v & 0xFFFFFFFF)
        sp = STACK + STACK_SIZE - 0x1000
        sp -= 4 * len(stack_args)
        for i, v in enumerate(stack_args):
            uc.mem_write(sp + 4 * i, struct.pack("<I", v & 0xFFFFFFFF))
        uc.reg_write(UC_ARM_REG_SP, sp)
        uc.reg_write(UC_ARM_REG_LR, HALT | 1)
        uc.emu_start(entry | 1, HALT, count=max_insn)
        return uc.reg_read(UC_ARM_REG_R0)

    def decode_half(self, half, flags=0):
        out = OUT0 if half == 0 else OUT1
        self.call(DECODE_WAV, [out, 80, INBUF, 0], [flags, half, MYSTERY])
        return list(struct.unpack("<80h", self.uc.mem_read(out, 160)))

    def set_bits(self, bits):
        assert len(bits) == 49
        self.uc.mem_write(INBUF, struct.pack("<49h", *[b & 1 for b in bits]))

    def decode(self, bits):
        """bits: 49 values, optionally followed by the frame flags argument (stack arg 5)."""
        flags = bits[49] if len(bits) > 49 else 0
        self.set_bits(bits[:49])
        return self.decode_half(0, flags) + self.decode_half(1)

    def read(self, addr, n):
        return bytes(self.uc.mem_read(addr, n))

    def write(self, addr, data):
        self.uc.mem_write(addr, bytes(data))


def bits_from_amb_line(s):
    return [int(c) for c in s.strip()]


ENCODE = 0x08050D90
WAV_IN0 = 0x2000DE82
WAV_IN1 = 0x2000DF22
AMBE_OUT = 0x2000DFC6
EN_MYSTERY = 0x2000C730


def encode(o, pcm160):
    o.write(WAV_IN0, struct.pack("<80h", *pcm160[:80]))
    o.write(WAV_IN1, struct.pack("<80h", *pcm160[80:]))
    o.write(AMBE_OUT, b"\0" * 98)
    for half in range(2):
        o.call(ENCODE, [AMBE_OUT, 0, WAV_IN1 if half else WAV_IN0, 0x50], [0x1840, half, 0x2000, EN_MYSTERY])
    return [b & 1 for b in struct.unpack("<49h", o.read(AMBE_OUT, 98))]




STATE_LO, STATE_HI = 0x20011200, 0x20011D00


def load_frames(path, keep_resets=False):
    data = open(path, "rb").read()
    if path.endswith(".bits"):
        return [list(data[i:i + 49]) for i in range(0, len(data) - 48, 49)]
    frames = []
    for line in data.decode().splitlines():
        if line.startswith("#"):
            continue
        parts = line.split()
        if not parts:
            continue
        if parts[0] == "R":
            if keep_resets:
                frames.append(None)
            continue
        frame = [int(c) for c in parts[-1]]
        if parts[0] == "X":  # "X <flags hex> <bits>": pass frame flags to the decoder
            frame.append(int(parts[1], 16))
        frames.append(frame)
    return frames


def capture(argv):
    func = int(argv[0], 16)
    frames = load_frames(argv[1])
    out = open(argv[2], "wb")
    args = argv[3:]
    opt = lambda k, d: int(args[args.index(k) + 1]) if k in args else d
    max_n, skip, every, nframes = opt("--max", 40), opt("--skip", 0), opt("--every", 1), opt("--frames", len(frames))
    o = Oracle()
    uc = o.uc
    active = []  # (ret addr, sp, record header, before regions)
    count = [0, 0]  # calls seen, records written
    cur_frame = [0]

    def regions(sp):
        lo = max(STACK, (sp - 0x900) & ~3)
        return [(STATE_LO, STATE_HI - STATE_LO), (lo, STACK + STACK_SIZE - lo)]

    # Basic-block hook: a routine's entry and every return address start a translation block.
    def hook(uc, addr, size, _):
        if addr == func:
            n = count[0]
            count[0] += 1
            if n >= skip and (n - skip) % every == 0 and count[1] < max_n:
                sp = uc.reg_read(UC_ARM_REG_SP)
                regs = [uc.reg_read(r) for r in (UC_ARM_REG_R0, UC_ARM_REG_R1, UC_ARM_REG_R2, UC_ARM_REG_R3)]
                stack_args = list(struct.unpack("<16I", bytes(uc.mem_read(sp, 64))))
                regs_list = regions(sp)
                before = [bytes(uc.mem_read(a, l)) for a, l in regs_list]
                active.append((uc.reg_read(UC_ARM_REG_LR) & ~1, sp, (cur_frame[0], n, regs, sp, stack_args), regs_list, before))
                count[1] += 1
        if active and addr == active[-1][0] and uc.reg_read(UC_ARM_REG_SP) == active[-1][1]:
            ret, sp, (fr, n, regs, sp, stack_args), regs_list, before = active.pop()
            r0, r1 = uc.reg_read(UC_ARM_REG_R0), uc.reg_read(UC_ARM_REG_R1)
            out.write(b"REC1" + struct.pack("<II4II16III", fr, n, *regs, sp, *stack_args, r0, r1))
            out.write(struct.pack("<I", len(regs_list)))
            for (a, l), b in zip(regs_list, before):
                out.write(struct.pack("<II", a, l) + b + bytes(uc.mem_read(a, l)))

    uc.hook_add(UC_HOOK_BLOCK, hook, begin=0x08000000, end=0x08100000)
    frames = [f for f in frames if f is not None]
    for i, bits in enumerate(frames[:nframes]):
        cur_frame[0] = i
        o.decode(bits)
        if count[1] >= max_n and not active:
            break
    out.close()  # explicit: the hook closure keeps `out` in a reference cycle, so no flush at exit
    print(f"{func:#x}: {count[0]} calls seen, {count[1]} recorded", file=sys.stderr)


def decode(argv):
    o = Oracle()
    out = open(argv[1], "wb")
    for bits in load_frames(argv[0], keep_resets=True):
        if bits is None:
            o.reset()
            continue
        out.write(struct.pack("<160h", *o.decode(bits)))
    out.close()


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else ""
    if cmd == "capture":
        capture(sys.argv[2:])
    elif cmd == "decode":
        decode(sys.argv[2:])
    else:
        sys.exit(__doc__)
