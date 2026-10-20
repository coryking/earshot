#!/bin/bash
# Static ARMv7 (musl) build of chanctl for the Pluto, using zig's cc via uvx.
set -euo pipefail
out="${1:-$HOME/.cache/amateur-radio-fpga/sw/chanctl}"
mkdir -p "$(dirname "$out")"
uvx --from ziglang python-zig cc -target arm-linux-musleabihf -mcpu=cortex_a9 -O2 -static \
  -Wall -Wextra -o "$out" "$(dirname "$0")/chanctl.c"
echo "$out"
