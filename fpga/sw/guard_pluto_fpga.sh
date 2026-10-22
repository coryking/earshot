#!/usr/bin/env bash
# Run from the user timer. A Pluto reboot loses a runtime-loaded FPGA image;
# restarting the live unit reruns ensure_pluto_fpga.sh before its producer.
set -euo pipefail

systemctl --user is-active --quiet amateur-radio-live.service || exit 0

PLUTO=${PLUTO:-root@pluto.local}
SSHOPT=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
        -o ConnectTimeout=5 -o ServerAliveInterval=5 -o ServerAliveCountMax=2
        -o LogLevel=ERROR)
p() { SSHPASS="${PLUTO_SSH_PASSWORD:-analog}" timeout 15s sshpass -e ssh "${SSHOPT[@]}" "$PLUTO" "$@"; }

# Only the runtime loader creates this marker after successful PL programming.
# An SSH failure is not evidence that Pluto rebooted: try again next minute.
state=$(p 'if test -e /tmp/pl_is_chan; then echo CHAN; else echo STOCK; fi' 2>/dev/null) || exit 0
case "$state" in CHAN) exit 0;; STOCK) ;; *) exit 0;; esac
echo 'Pluto rebooted without the runtime FPGA; restarting live receiver'
systemctl --user restart amateur-radio-live.service
