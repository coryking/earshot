# radio-supervisor

Keeps the Pluto's channel producer running for the always-on radio, and puts the radio back the
way it found it whenever the producer stops. RX only: it never transmits, flashes, or touches the
NOR or the U-Boot environment. Not in the data path (the engine reads the producer over TCP).

```sh
cargo build --release --locked        # supervisor/target/release/radio-supervisor
radio-supervisor --binary ~/.cache/amateur-radio-arm/build/pluto-channel-source \
  --status-file /run/user/1000/amateur-radio/receiver.json
```

Per cycle: take `/tmp/pluto-radio.lock` (waits if another session holds it), check the Pluto
answers `iio_info` and which image it runs, stop leftover producers, snapshot the receiver
(`iio_attr`), copy the producer to the Pluto's `/tmp` and start it over SSH, watch it. When it ends
(exit, stall, or a stop request) it reads the receiver back against the snapshot and writes back
any RX attribute that differs, then releases the lock and waits (3 s doubling to 60 s after
failures). A Pluto that is off, rebooting or running another image is waited out with backoff
(5 to 30 s unreachable, 15 to 120 s other image) and the lock is released meanwhile.

Image profiles: `--profiles FILE` (re-read at every check) lists producers for other images; see
`--help`. A profile matches when all its `match` strings are in the `iio_info` output and its
`files_present` / `files_absent` paths agree on the Pluto. The built-in `stock-arm` profile is
always last and requires `fw_version: v0.37` and none of `/tmp/pl_is_chan` or
`/proc/device-tree/reserved-memory`, the two things the channelizer image leaves behind (its
firmware string and IIO devices are otherwise the stock ones).

The status file (JSON, rewritten on every change, mtime refreshed every 3 s while the process
lives): `state` (`starting`, `running`, `waiting`, `stopped`), `reason`
(`lock-held`, `unreachable`, `wrong-image`, `producer-failed`, `radio-state`, `shutdown`),
`detail` (plain English), `since`, `retry_in_s`, `profile`, `pluto`, `last_exit`, `last_restore`.
`offload/radio-borrow` writes the same file with `state: "borrowed"` while a borrower runs.
`SIGTERM` stops the producer, verifies the restore and exits; the unit allows 150 s.

The SSH password is `PLUTO_SSH_PASSWORD` or the ADI factory default; it travels in the `SSHPASS`
environment of the child process only.
