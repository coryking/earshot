#!/bin/sh
# Cross-build the Pluto producer with an ARM GNU gcc against ADI's v0.37 uClibc sysroot.
# Linux-friendly alternative to build.sh (which needs clang). Nothing here touches the device.
#   build-gcc.sh SYSROOT_STAGING_DIR OUTPUT_DIR [CROSS_PREFIX]
# CROSS_PREFIX defaults to arm-none-linux-gnueabihf- and must be on PATH.
set -eu
if [ "$#" -lt 2 ]; then
    echo "Usage: $0 SYSROOT_DIRECTORY OUTPUT_DIRECTORY [CROSS_PREFIX]" >&2
    exit 2
fi
sysroot=$(cd "$1" && pwd)
mkdir -p "$2"
build=$(cd "$2" && pwd)
cc="${3:-arm-none-linux-gnueabihf-}gcc"
source_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
flags="--sysroot=$sysroot -mcpu=cortex-a9 -mfpu=neon -mfloat-abi=hard -O3 -ffast-math -fno-math-errno -Wall -Wextra -Werror"
link="--sysroot=$sysroot -Wl,--dynamic-linker=/lib/ld-uClibc.so.1 -Wl,-rpath-link,$sysroot/usr/lib -static-libgcc"
for name in pfb prototype gate receiver; do
    $cc $flags -c "$source_dir/$name.c" -o "$build/$name.o"
done
$cc $link "$build/pfb.o" "$build/gate.o" "$build/prototype.o" -lm -o "$build/pluto-pfb"
$cc $link "$build/pfb.o" "$build/gate.o" "$build/receiver.o" -lad9361 -liio -lm -o "$build/pluto-channel-source"
printf '%s\n%s\n' "$build/pluto-pfb" "$build/pluto-channel-source"
