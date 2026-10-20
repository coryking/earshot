#!/usr/bin/env bash
# Prepare the current Pluto boot for the FPGA producer. Safe to run again when
# the channelizer is already loaded; the runtime loader handles USB recovery.
#
#   ensure_pluto_fpga.sh <system_top.bit.bin> <chanctl>
set -euo pipefail

bit=${1:?bitstream .bin required}
tool=${2:?chanctl required}
test -f "$bit" && test -f "$tool"

# This milestone deliberately uses the routed image already tested on hardware.
expected=23632b191f56efce8593764c0f8a6858cd273208f30a7dd344888e978b54e7c9
actual=$(sha256sum "$bit" | cut -d' ' -f1)
if [ "$actual" != "$expected" ]; then
  echo "bitstream hash differs from the tested release: $actual" >&2
  exit 1
fi

PLUTO=${PLUTO:-root@192.168.10.10}
SSHOPT=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
        -o ConnectTimeout=5 -o LogLevel=ERROR)
p() { sshpass -p "${PLUTO_SSH_PASSWORD:-analog}" ssh "${SSHOPT[@]}" "$PLUTO" "$@"; }
put() { sshpass -p "${PLUTO_SSH_PASSWORD:-analog}" scp -O -q "${SSHOPT[@]}" "$1" "$PLUTO:$2"; }

for attempt in $(seq 1 30); do
  if p true 2>/dev/null; then break; fi
  if [ "$attempt" -eq 30 ]; then echo "Pluto did not answer SSH" >&2; exit 1; fi
  sleep 2
done

# The stock image has no safe DMA address. The diagnostic firmware reserves it
# as dma_probe; a persistent channelizer image calls it chan_dma. Either is
# acceptable only if Linux also excludes the whole interval from System RAM.
if ! p 'test -e /proc/device-tree/reserved-memory/dma_probe@4000000/no-map || test -e /proc/device-tree/reserved-memory/chan_dma@4000000/no-map'; then
  echo "Pluto boot has no DMA reservation; refusing to start the producer" >&2
  exit 1
fi
if ! p cat /proc/iomem | python3 -c '
import re, sys
text = sys.stdin.read()
ram = re.findall(r"^([0-9a-f]+)-([0-9a-f]+) : System RAM$", text, re.M)
if not ram or any(int(lo, 16) < 0x08000000 and int(hi, 16) >= 0x04000000 for lo, hi in ram):
    sys.exit(1)
'; then
  echo "DMA reservation overlaps Linux System RAM" >&2
  exit 1
fi

# A firmware-loaded channelizer has the chan_dma node; a runtime load leaves the
# marker in ramfs. Do not touch the custom register address on a stock PL boot.
if p 'test -e /tmp/pl_is_chan || test -e /proc/device-tree/reserved-memory/chan_dma@4000000'; then
  put "$tool" /tmp/chanctl
  p chmod +x /tmp/chanctl
  if p /tmp/chanctl info | grep -q '"magic":"0x4348414e"'; then
    echo "Pluto FPGA channelizer ready"
    exit 0
  fi
fi

script_dir=$(cd "$(dirname "$0")" && pwd)
"$script_dir/pluto_runtime_load.sh" load "$bit" "$tool"
if ! p /tmp/chanctl info | grep -q '"magic":"0x4348414e"'; then
  echo "Pluto did not return with the channelizer" >&2
  exit 1
fi
if [ "$(p cat /sys/kernel/config/usb_gadget/composite_gadget/UDC)" != ci_hdrc.0 ]; then
  echo "Pluto USB gadget is detached" >&2
  exit 1
fi
echo "Pluto FPGA channelizer ready"
