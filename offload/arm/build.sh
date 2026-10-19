#!/bin/sh
# Build with the current Pluto's uClibc ABI, without installing a compiler.
set -eu
if [ "$#" -ne 2 ]; then
    echo "Usage: $0 SYSROOT_DIRECTORY OUTPUT_DIRECTORY" >&2
    exit 2
fi
sysroot=$(cd "$1" && pwd)
mkdir -p "$2"
build=$(cd "$2" && pwd)
source_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
clang_bin=$(command -v clang)
lld_bin=$(find "${RUSTUP_HOME:-$HOME/.rustup}/toolchains" -path '*/bin/rust-lld' -type f | head -n 1)
if [ -z "$lld_bin" ]; then
    echo "rust-lld not found; install the managed Rust toolchain or supply an ARM linker." >&2
    exit 1
fi
for name in pfb prototype gate receiver; do
    "$clang_bin" --target=arm-linux-gnueabihf -mcpu=cortex-a9 -mfpu=neon -mfloat-abi=hard \
      --sysroot="$sysroot" -O3 -ffast-math -fno-math-errno -Wall -Wextra -Werror \
      -c "$source_dir/$name.c" -o "$build/$name.o"
done
"$lld_bin" -flavor gnu -m armelf_linux_eabi --sysroot="$sysroot" \
    --dynamic-linker=/lib/ld-uClibc.so.1 -L"$sysroot/usr/lib" -L"$sysroot/lib" \
    "$sysroot/usr/lib/crt1.o" "$sysroot/usr/lib/crti.o" \
    "$build/pfb.o" "$build/gate.o" "$build/prototype.o" -lm -lc "$sysroot/lib/libgcc_s.so.1" \
    "$sysroot/usr/lib/crtn.o" -o "$build/pluto-pfb"
printf '%s\n' "$build/pluto-pfb"

"$lld_bin" -flavor gnu -m armelf_linux_eabi --sysroot="$sysroot" \
    --dynamic-linker=/lib/ld-uClibc.so.1 -L"$sysroot/usr/lib" -L"$sysroot/lib" \
    "$sysroot/usr/lib/crt1.o" "$sysroot/usr/lib/crti.o" \
    "$build/pfb.o" "$build/gate.o" "$build/receiver.o" -lad9361 -liio -lm -lc "$sysroot/lib/libgcc_s.so.1" \
    "$sysroot/usr/lib/crtn.o" -o "$build/pluto-channel-source"
printf '%s\n' "$build/pluto-channel-source"
