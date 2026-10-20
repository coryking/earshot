#!/usr/bin/env bash
# Run from the user timer. A Pluto reboot loses a runtime-loaded FPGA image;
# restarting the live unit reruns ensure_pluto_fpga.sh before its producer.
set -euo pipefail

systemctl --user is-active --quiet amateur-radio-live.service || exit 0

PLUTO=${PLUTO:-root@192.168.10.10}
SSHOPT=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
        -o ConnectTimeout=5 -o LogLevel=ERROR)
p() { sshpass -p "${PLUTO_SSH_PASSWORD:-analog}" ssh "${SSHOPT[@]}" "$PLUTO" "$@"; }

# A channelizer flashed into the boot image has the DT marker. A runtime load
# has the ramfs marker. Leave a healthy producer alone.
if p 'test -e /tmp/pl_is_chan || test -e /proc/device-tree/reserved-memory/chan_dma@4000000' 2>/dev/null; then
  exit 0
fi
# An unreachable Pluto can return on its own. The supervisor already backs off
# while it is absent; restart only once a booted Pluto actually answers.
p true 2>/dev/null || exit 0
echo 'Pluto rebooted without the runtime FPGA; restarting live receiver'
systemctl --user restart amateur-radio-live.service
