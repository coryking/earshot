#!/bin/bash
# Static ARMv7 (musl) build of chanctl for the Pluto, using zig's cc via uvx.
set -euo pipefail
# Static ARMv7 (musl) builds of chanctl and pluto-fpga-source for the Pluto.
out="${1:-$HOME/.cache/amateur-radio-fpga/sw}"
mkdir -p "$out"
here="$(dirname "$0")"
cc=(uvx --from ziglang==0.15.2 python-zig cc -target arm-linux-musleabihf -mcpu=cortex_a9 -O2 -static -Wall -Wextra)
"${cc[@]}" -o "$out/chanctl" "$here/chanctl.c"
"${cc[@]}" -o "$out/pluto-fpga-source" "$here/pluto_fpga_source.c" -lm
echo "$out"
