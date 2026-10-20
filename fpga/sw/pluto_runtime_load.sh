#!/bin/bash
# Load a channelizer bitstream into the running Pluto's PL (non-persistent).
#
#   fpga/sw/pluto_runtime_load.sh load <bitstream.bin> [chanctl]
#   fpga/sw/pluto_runtime_load.sh restore            # reboot to stock, re-apply settings
#
# Holds /tmp/pluto-radio.lock for the whole operation (shared with every other radio
# user on this host). Nothing is written to flash: the bitstream goes to the
# ramfs /lib/firmware and through the kernel FPGA manager. The PL-side drivers
# (AD9361 IIO cores, their DMAs, the PL I2C bus and its power monitor) are unbound
# first so no bus access can hit the fabric mid-configuration, then rebound; the
# ad9361 rebind reinitializes the transceiver. A reboot restores the stock PL.
# Receive settings are saved before and re-applied/read back after. RX only.
set -euo pipefail

PLUTO=${PLUTO:-root@192.168.10.10}
LOCK=/tmp/pluto-radio.lock
STATE=${STATE:-$HOME/.cache/amateur-radio-fpga/pluto-session}
mkdir -p "$STATE"
SSHOPT=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10
        -o ServerAliveInterval=5 -o ServerAliveCountMax=3 -o LogLevel=ERROR)
p() { sshpass -p analog ssh "${SSHOPT[@]}" "$PLUTO" "$@"; }
put() { sshpass -p analog scp -O "${SSHOPT[@]}" "$1" "$PLUTO:$2"; }
log() { echo "[$(date +%H:%M:%S)] $*" | tee -a "$STATE/session.log"; }

exec 9>>"$LOCK"
flock -w 900 9 || { echo "could not get $LOCK"; exit 1; }

PHY='d=$(grep -l ad9361-phy /sys/bus/iio/devices/iio:device*/name | xargs dirname); [ -n "$d" ] || { echo "no ad9361-phy" >&2; exit 1; }'
ATTRS="in_voltage_sampling_frequency in_voltage_rf_bandwidth out_altvoltage0_RX_LO_frequency in_voltage0_gain_control_mode in_voltage0_hardwaregain in_voltage_filter_fir_en out_voltage_filter_fir_en out_voltage_sampling_frequency"

save_settings() {
  p "$PHY; for a in $ATTRS; do echo \"\$a=\$(cat \$d/\$a)\"; done" > "$1.tmp"
  grep -q '^in_voltage_sampling_frequency=[0-9]' "$1.tmp" || { log "settings read failed"; exit 1; }
  mv "$1.tmp" "$1"
  log "saved settings to $1: $(tr '\n' ' ' < "$1")"
}

apply_settings() {
  local f=$1
  # gain mode before gain; FIR enables before rates as the ARM tooling does
  for a in in_voltage0_gain_control_mode in_voltage_filter_fir_en out_voltage_filter_fir_en \
           in_voltage_sampling_frequency out_voltage_sampling_frequency in_voltage_rf_bandwidth \
           out_altvoltage0_RX_LO_frequency in_voltage0_hardwaregain; do
    v=$(grep "^$a=" "$f" | cut -d= -f2- | awk '{print $1}')
    [ -z "$v" ] && continue
    if [ "$a" = in_voltage0_hardwaregain ] && ! grep -q "in_voltage0_gain_control_mode=manual" "$f"; then
      continue
    fi
    p "$PHY; echo $v > \$d/$a" || log "warning: could not set $a=$v"
  done
  p "$PHY; for a in $ATTRS; do echo \"\$a=\$(cat \$d/\$a)\"; done" > "$STATE/readback.txt"
  log "readback: $(tr '\n' ' ' < "$STATE/readback.txt")"
}

UNBIND='
set -e
u() { [ -e /sys/bus/$1/drivers/$2/$3 ] && echo $3 > /sys/bus/$1/drivers/$2/unbind || true; }
u platform cf_axi_dds 79024000.cf-ad9361-dds-core-lpc
u platform cf_axi_adc 79020000.cf-ad9361-lpc
u spi ad9361 spi0.0
u platform dma-axi-dmac 7c400000.dma
u platform dma-axi-dmac 7c420000.dma
u i2c adm1177-iio 0-005a
u platform xiic-i2c 41600000.i2c
'
BIND='
b() { [ -e /sys/bus/$1/drivers/$2/$3 ] || echo $3 > /sys/bus/$1/drivers/$2/bind; }
b platform xiic-i2c 41600000.i2c
sleep 0.2
b i2c adm1177-iio 0-005a
b platform dma-axi-dmac 7c400000.dma
b platform dma-axi-dmac 7c420000.dma
b spi ad9361 spi0.0
sleep 1
b platform cf_axi_adc 79020000.cf-ad9361-lpc
b platform cf_axi_dds 79024000.cf-ad9361-dds-core-lpc
sleep 0.5
for d in /sys/bus/iio/devices/iio:device*; do cat $d/name; done
'

case "${1:-}" in
  load)
    bin=${2:?bitstream .bin}; tool=${3:-}
    log "load $(basename "$bin") sha256 $(sha256sum "$bin" | cut -c1-64)"
    save_settings "$STATE/settings-before.txt"
    put "$bin" /lib/firmware/pluto_chan.bin
    local_sha=$(sha256sum "$bin" | cut -c1-64)
    dev_sha=$(p sha256sum /lib/firmware/pluto_chan.bin | cut -c1-64)
    [ "$local_sha" = "$dev_sha" ] || { log "sha256 mismatch on device ($dev_sha); aborting before unbind"; exit 1; }
    if [ -n "$tool" ]; then put "$tool" /tmp/chanctl; p chmod +x /tmp/chanctl; fi
    # Unbind, program, verify and rebind run as one detached script on the Pluto, so
    # an ssh drop cannot strand it half-way. If programming does not reach
    # 'operating', it reboots (stock PL) instead of rebinding drivers onto dead fabric.
    p "cat > /tmp/pl_swap.sh <<'EOS'
[ -f /tmp/pl_is_chan ] && /tmp/chanctl stop
rm -f /tmp/pl_is_chan
$UNBIND
echo 0 > /sys/class/fpga_manager/fpga0/flags
echo pluto_chan.bin > /sys/class/fpga_manager/fpga0/firmware || true
state=\$(cat /sys/class/fpga_manager/fpga0/state)
echo \"fpga0 state: \$state\"
if [ \"\$state\" != operating ]; then echo 'programming failed: rebooting to stock'; sync; reboot; exit 1; fi
$BIND
touch /tmp/pl_is_chan
echo PL_SWAP_DONE
EOS
setsid sh /tmp/pl_swap.sh > /tmp/pl_swap.log 2>&1 < /dev/null &"
    log "PL swap running on the device"
    for _ in $(seq 60); do
      sleep 2
      out=$(p 'cat /tmp/pl_swap.log' 2>/dev/null || true)
      case "$out" in *PL_SWAP_DONE*|*"programming failed"*) break;; esac
    done
    echo "$out" | tee -a "$STATE/session.log"
    case "$out" in *PL_SWAP_DONE*) ;; *) log "PL swap did not complete; run '$0 restore' after the device is back"; exit 1;; esac
    apply_settings "$STATE/settings-before.txt"
    if [ -n "$tool" ]; then p /tmp/chanctl info | tee -a "$STATE/session.log"; fi
    ;;
  restore)
    log "restore: reboot to the stock image"
    [ -f "$STATE/settings-before.txt" ] || { echo "no saved settings"; exit 1; }
    p 'reboot' || true
    sleep 20
    for _ in $(seq 60); do p true 2>/dev/null && break; sleep 5; done
    sleep 5
    log "back up: $(p 'cat /sys/class/fpga_manager/fpga0/state; uptime')"
    apply_settings "$STATE/settings-before.txt"
    ;;
  *) echo "usage: $0 load <bitstream.bin> [chanctl] | restore"; exit 2 ;;
esac
