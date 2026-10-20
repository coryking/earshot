# Native ARM experiment

The files here are an experimental producer, not a completed 20 MHz receiver.
Read ../RESULTS.md before selecting a profile. ../PROTOCOL.md is the shared Rust
wire contract; ../RECOVERY.md records the required device recovery preparation.

## Build

The exact original firmware uses ADI v0.37's uClibc sysroot. The tested Mac copy
is in the main checkout's ignored `.tools/pluto/sysroot-v0.37/staging/`.
`build.sh` uses native clang's ARM target and the installed Rust toolchain's LLD:

```sh
offload/arm/build.sh \
  /Users/coryking/projects/amateur-radio/.tools/pluto/sysroot-v0.37/staging \
  /Users/coryking/projects/amateur-radio/.tools/pluto/build
```

This produces `pluto-pfb` (file/synthetic correctness and compute-only benchmark)
and `pluto-channel-source` (native libiio receiver and RIQ1 TCP server).
No firmware replacement is required. On Linux use an ARM compiler/linker against
the same uClibc sysroot or adapt the script to a compatible clang/LLD installation;
the current script's compiler discovery was validated on the Mac only.

```sh
uv run pytest -q offload/arm/test_pfb.py offload/test_recovery.py
cc -O1 -g -fsanitize=address,undefined -Wall -Wextra -Werror \
  offload/arm/gate.c offload/arm/gate_test.c -lm -o /tmp/pluto-gate-test
/tmp/pluto-gate-test
```

The vendored jsmn header is MIT licensed, from
https://github.com/zserge/jsmn/blob/master/jsmn.h; its license is retained in the
header. ADI libiio/libad9361 are dynamically linked from the existing device.

### Linux cross-build (no clang, used on wigglebutt)

`build-gcc.sh` builds with an ARM GNU gcc against ADI's published v0.37 sysroot. Both downloads
live under `~/.cache/amateur-radio-arm/` (the sysroot is the `sysroot-v0.37.tar.gz` asset of
analogdevicesinc/plutosdr-fw release v0.37; the compiler is ARM's
`arm-gnu-toolchain-13.3.rel1-x86_64-arm-none-linux-gnueabihf`):

```sh
PATH=~/.cache/amateur-radio-arm/tc/arm-gnu-toolchain-13.3.rel1-x86_64-arm-none-linux-gnueabihf/bin:$PATH \
  offload/arm/build-gcc.sh ~/.cache/amateur-radio-arm/sysroot/staging ~/.cache/amateur-radio-arm/build
```

## Live sessions (hold the shared lock)

Every session that configures, captures or streams holds `/tmp/pluto-radio.lock` (the helpers
in `offload/radio_lock.py` take it; release it between runs). They check the Pluto's firmware
first, because the image can change between sessions. The default SSH password is ADI's factory
default; set `PLUTO_SSH_PASSWORD` if it was changed.

```sh
# bounded acceptance: producer -> private engine -> WebSocket path, acknowledged STOP, readbacks
uv run python offload/live_acceptance.py --binary ~/.cache/amateur-radio-arm/build/pluto-channel-source \
  --out output/live-acceptance --seconds 120
# listening session on the service's own URL (stops the fixture service, restores it afterwards)
uv run python offload/live_session.py --binary ~/.cache/amateur-radio-arm/build/pluto-channel-source --minutes 60
```

## Run only with an exclusive receiver lease

Copy the ARM binary to `/tmp/pluto-channel-source` through the authorized device
access path, make it executable, and run:

```sh
/tmp/pluto-channel-source --seconds 30 --forward-streams 50
```

It listens on TCP port 9010 and opens RX only after one client connects. The
host connects to `192.168.10.10:9010`. There must be no competing IIO capture
client. RF setup can take part of a second; no data should be represented as live
until CONFIG arrives. `--seconds 0` (the default) runs until stopped/disconnected.
The source takes bounded selected stream IDs; by default IQ requires a host pin.
All 64 stream metadata and spectrum/activity estimates remain available.

A stop control request must receive a successful CONFIG acknowledgment after
original control readbacks. On connection failure or SIGINT/SIGTERM during
capture, cleanup also restores original active controls. A source blocked in
`accept()` before any client connects may have its signal handler restarted by
libc; verify it exited, and terminate the unopened process if necessary. Do not
force-kill a running capture before checking restoration.

The source snapshots RX controls, RX/TX FIR enables, and TX clock. It refuses an
already enabled custom FIR. The ADI low-rate helper loads a volatile FIR profile;
cleanup disables it and restores the former active settings. No transmit buffer
or transmit streaming device is opened. Dormant FIR coefficients remain loaded
until reboot; active filtering is restored to its original disabled state.

Current capabilities are `get_config`, `pin`, and verified `stop`; live configure
requests fail explicitly. The source has a 250 ms send timeout and bounded socket
buffer; a stalled consumer ends capture rather than growing memory indefinitely.
It does not have hardware sample counters and cannot prove DMA continuity.

For the current 800 ksps/64-bin test, stream IDs are bin+1. Bins above 31 have
negative offset; stream 50 is bin49, offset -187500 Hz. Avoid hardcoding this map
in a UI: read CONFIG's physical centers and exact rational output rates.
