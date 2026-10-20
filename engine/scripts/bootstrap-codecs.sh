#!/bin/sh
# Build pinned upstream codecs locally. This never opens the radio.
set -eu
engine_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
native_root="$engine_root/.native"
mkdir -p "$native_root/src" "$native_root/lib"
for dependency in dsdcc mbelib; do
    case "$dependency" in
        dsdcc) repository=https://github.com/f4exb/dsdcc; revision=f27b32d2df131ae3a376fe72d3fb880ae1f9ede1;;
        mbelib) repository=https://github.com/szechyjs/mbelib; revision=9a04ed5c78176a9965f3d43f7aa1b1f5330e771f;;
    esac
    source_dir="$native_root/src/$dependency"
    if [ ! -d "$source_dir/.git" ]; then
        git clone --filter=blob:none --no-checkout "$repository" "$source_dir"
    fi
    # Drop a previously applied patch first, so re-running (or a pin bump) starts clean.
    git -C "$source_dir" reset --hard -q
    git -C "$source_dir" checkout --detach "$revision"
done
# DSDcc is patched so the engine can substitute its own AMBE+2 voice decoder (src/ambe) for
# mbelib's; without a substitute the behaviour is upstream's. The patch also exports a marker
# symbol the engine links against, so an unpatched build cannot be used by mistake.
git -C "$native_root/src/dsdcc" apply "$engine_root/patches/dsdcc-ambe-vocoder-hook.patch"
cmake -S "$native_root/src/mbelib" -B "$native_root/build-mbe" -DCMAKE_POLICY_VERSION_MINIMUM=3.5 -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$native_root"
cmake --build "$native_root/build-mbe" --parallel 4
cmake --install "$native_root/build-mbe"
case "$(uname -s)" in Darwin) mbe_library="$native_root/lib/libmbe.dylib";; *) mbe_library="$native_root/lib/libmbe.so";; esac
cmake -S "$native_root/src/dsdcc" -B "$native_root/build-dsdcc" -DCMAKE_POLICY_VERSION_MINIMUM=3.5 -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$native_root" -DUSE_MBELIB=ON -DBUILD_TOOL=OFF -DLIBMBE_INCLUDE_DIR="$native_root/include" -DLIBMBE_LIBRARY="$mbe_library"
cmake --build "$native_root/build-dsdcc" --parallel 4
cmake --install "$native_root/build-dsdcc"
printf '%s\n' 'Native codecs built. Run cargo build --release --locked from engine/.'
