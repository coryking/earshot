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
        -o ConnectTimeout=5 -o ServerAliveInterval=5 -o ServerAliveCountMax=2
        -o LogLevel=ERROR)
p() { SSHPASS="${PLUTO_SSH_PASSWORD:-analog}" timeout 15s sshpass -e ssh "${SSHOPT[@]}" "$PLUTO" "$@"; }
put() { SSHPASS="${PLUTO_SSH_PASSWORD:-analog}" timeout 15s sshpass -e scp -O -q "${SSHOPT[@]}" "$1" "$PLUTO:$2"; }

for attempt in $(seq 1 10); do
  if p true 2>/dev/null; then break; fi
  if [ "$attempt" -eq 10 ]; then echo "Pluto did not answer SSH" >&2; exit 1; fi
  sleep 2
done

# The stock image has no safe DMA address. The diagnostic firmware reserves it
# as dma_probe; a persistent channelizer image calls it chan_dma. Either is
# acceptable only if Linux also excludes the whole interval from System RAM.
reservation=$(p 'if test -e /proc/device-tree/reserved-memory/dma_probe@4000000/no-map || test -e /proc/device-tree/reserved-memory/chan_dma@4000000/no-map; then echo RESERVED; else echo MISSING; fi') || {
  echo "Pluto SSH failed while checking the DMA reservation" >&2; exit 1;
}
if [ "$reservation" != RESERVED ]; then
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

# The reservation only describes RAM. It does not prove which PL image is loaded.
# Only the runtime loader creates the ramfs marker after programming the PL.
pl_state=$(p 'if test -e /tmp/pl_is_chan; then echo CHAN; else echo STOCK; fi') || {
  echo "Pluto SSH failed while checking the FPGA image" >&2; exit 1;
}
case "$pl_state" in CHAN|STOCK) ;; *) echo "Unexpected Pluto FPGA state: $pl_state" >&2; exit 1;; esac
if [ "$pl_state" = CHAN ]; then
  put "$tool" /tmp/chanctl
  p chmod +x /tmp/chanctl
  info=$(p /tmp/chanctl info) || { echo "Pluto FPGA probe failed" >&2; exit 1; }
  if grep -q '"magic":"0x4348414e"' <<< "$info"; then
    echo "Pluto FPGA channelizer ready"
    exit 0
  fi
fi

script_dir=$(cd "$(dirname "$0")" && pwd)
# A persistent load fault must not drive the Pluto through an endless USB/reboot
# cycle. Systemd can retry the preflight, but at most one PL load runs per five
# minutes after a failure. A successful load clears the cooldown.
failure_file=${FPGA_LOAD_FAILURE_FILE:-$HOME/.cache/amateur-radio-fpga/last-load-failure}
mkdir -p "$(dirname "$failure_file")"
last_failure=$(cat "$failure_file" 2>/dev/null || echo 0)
now=$(date +%s)
if [[ "$last_failure" =~ ^[0-9]+$ ]] && (( now - last_failure < 300 && now >= last_failure )); then
  echo "Recent FPGA load failed; waiting for five-minute cooldown" >&2
  exit 1
fi
echo "$now" > "$failure_file"  # also survives a killed preflight with no EXIT trap
loading=1
trap 'if (( loading )); then date +%s > "$failure_file"; fi' EXIT
"$script_dir/pluto_runtime_load.sh" load "$bit" "$tool"
info=$(p /tmp/chanctl info) || { echo "Pluto FPGA probe failed after loading" >&2; exit 1; }
if ! grep -q '"magic":"0x4348414e"' <<< "$info"; then
  echo "Pluto did not return with the channelizer" >&2
  exit 1
fi
if [ "$(p cat /sys/kernel/config/usb_gadget/composite_gadget/UDC)" != ci_hdrc.0 ]; then
  echo "Pluto USB gadget is detached" >&2
  exit 1
fi
loading=0
trap - EXIT
echo 0 > "$failure_file"
echo "Pluto FPGA channelizer ready"
