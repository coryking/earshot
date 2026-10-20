#!/bin/sh
# Offline tooling only: fetch the MD-380 firmware image and RAM snapshot that the AMBE+2 port
# (src/ambe) was made from. The engine never needs them; scripts/ambe-tables.py (regenerate
# the constant tables) and scripts/ambe_oracle.py (the test oracle) read them.
#
# The Tytera MD-380 firmware is proprietary (DVSI AMBE+2 code inside Tytera's image). Nothing is
# vendored: this downloads it from the md380tools community mirror into the Git-ignored
# engine/.native-md380/, and checks every artifact against a pinned SHA-256. Each download lives
# in its own directory under downloads/ and is untrusted data; the only downloaded code that is
# run is the pinned md380-fw.py unwrapper, with `python3 -I` from outside its directory.
#
# Usage: [MD380_DIR=/some/dir] scripts/fetch-md380.sh   (idempotent)
set -eu
engine_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
root="${MD380_DIR:-$engine_root/.native-md380}"
dl="$root/downloads"

# Pins. Change them only together with a review of what the new revision contains.
FW_URL=https://md380.org/firmware/orig/TYT-Tytera-MD-380-FW-v232.zip
FW_ZIP_SHA=50eea6a7e8ed8046de52a464e5f07cd9c9d000885a4b571b1a11ea5d5502d0ed
FW_WRAPPED_SHA=e1a1a11a84d9afd2d2ceed61be8395e32a8034785abd0be0236478f7d8609f43
FW_IMAGE_SHA=c0351f250a834660641bca3b06be931c80cc1ed0ef808356c550b99cd0f4c632
EMU_REPO=https://github.com/rhgndf/md380emu.git
EMU_REV=9ba1f1ba3b52a10e4c957774dbd89d609e1df44d
EMU_UNWRAP_SHA=9d94aabb492b42407abd745f8788e24caaaf6f3bedcbf704e31a0445588535ef
EMU_RAM_SHA=d40398183e9aa8db6288a62d9a0e0c233cb143ef5633680abd829cc03c719f66

if command -v sha256sum >/dev/null 2>&1; then
    sha_of() { sha256sum "$1" | cut -d' ' -f1; }
else
    sha_of() { shasum -a 256 "$1" | cut -d' ' -f1; }
fi
check() { # file expected-sha what
    got=$(sha_of "$1")
    [ "$got" = "$2" ] || { echo "FATAL: $3 hash mismatch: got $got, want $2" >&2; exit 1; }
}
for tool in curl unzip git python3; do
    command -v "$tool" >/dev/null 2>&1 || { echo "FATAL: $tool is required" >&2; exit 1; }
done

mkdir -p "$dl/firmware" "$dl/md380emu" "$root/firmware"
cat > "$root/README.txt" <<'TXT'
Git-ignored, host-local. Contains an untrusted download of proprietary Tytera/DVSI firmware
under firmware/ and downloads/. Never commit, copy to a public location or ship it.
Created by engine/scripts/fetch-md380.sh.
TXT

if [ -f "$root/firmware/D002.032.bin" ] && [ "$(sha_of "$root/firmware/D002.032.bin")" = "$FW_IMAGE_SHA" ] \
    && [ -f "$root/firmware/d02032-core.img" ] && [ "$(sha_of "$root/firmware/d02032-core.img")" = "$EMU_RAM_SHA" ]; then
    echo "firmware: present and verified"
    exit 0
fi
[ -f "$dl/firmware/fw.zip" ] || curl -fsSL -o "$dl/firmware/fw.zip" "$FW_URL"
check "$dl/firmware/fw.zip" "$FW_ZIP_SHA" "firmware zip"
unzip -qj -o "$dl/firmware/fw.zip" "Firmware 2.32/MD-380-D2.32(AD).bin" -d "$dl/firmware"
check "$dl/firmware/MD-380-D2.32(AD).bin" "$FW_WRAPPED_SHA" "wrapped firmware"
if [ ! -d "$dl/md380emu/.git" ]; then
    git init -q "$dl/md380emu"
    git -C "$dl/md380emu" remote add origin "$EMU_REPO"
fi
git -C "$dl/md380emu" fetch -q --depth 1 origin "$EMU_REV"
git -C "$dl/md380emu" checkout -q --detach FETCH_HEAD
[ "$(git -C "$dl/md380emu" rev-parse HEAD)" = "$EMU_REV" ] || { echo "FATAL: md380emu revision mismatch" >&2; exit 1; }
check "$dl/md380emu/firmware/md380-fw.py" "$EMU_UNWRAP_SHA" "md380-fw.py"
check "$dl/md380emu/firmware/d02032-core.img" "$EMU_RAM_SHA" "RAM snapshot"
(cd "$root" && python3 -I "$dl/md380emu/firmware/md380-fw.py" --radio MD380 --unwrap \
    "$dl/firmware/MD-380-D2.32(AD).bin" "$root/firmware/D002.032.bin" >/dev/null)
check "$root/firmware/D002.032.bin" "$FW_IMAGE_SHA" "unwrapped firmware"
cp "$dl/md380emu/firmware/d02032-core.img" "$root/firmware/d02032-core.img"
echo "firmware: fetched and verified into $root/firmware"
