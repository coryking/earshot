# radio-supervisor

Keeps the Pluto's channel producer running for the always-on radio, and puts the radio back the
way it found it whenever the producer stops. RX only: it never transmits, flashes, or touches the
NOR or the U-Boot environment. Not in the data path (the engine reads the producer over TCP).

```sh
cargo build --release --locked        # supervisor/target/release/radio-supervisor
radio-supervisor --binary ~/.cache/amateur-radio-fpga/sw/pluto-fpga-source \
  --status-file /run/user/1000/amateur-radio/receiver.json
```

The producer is `pluto-fpga-source` (`fpga/sw/build.sh`), which needs the flashed channelizer
firmware (`fpga/README.md`). It is started as
`--port 9010 --center 460000000 --gain 40 --forward-streams 1` (`--center`, `--gain`, `--forward`
change it): about 20 MHz in 512 bins of 50 kHz, manual gain. There is no other producer and no
image detection; on a stock image the producer exits and is retried with backoff.

Per cycle: take `/tmp/pluto-radio.lock` (waits if another session holds it), check the Pluto
answers `iio_info`, stop leftover producers, snapshot the receiver (`iio_attr`), copy the
producer to the Pluto's `/tmp` and start it over SSH, watch it. When it ends (exit, stall, or a
stop request) it reads the receiver back against the snapshot and writes back any RX attribute
that differs, then releases the lock and waits (3 s doubling to 60 s after failures). A Pluto
that is off or rebooting is waited out with backoff (5 to 30 s) and the lock is released meanwhile.

The status file (JSON, rewritten on every change, mtime refreshed every 3 s while the process
lives): `state` (`starting`, `running`, `waiting`, `stopped`), `reason`
(`lock-held`, `unreachable`, `producer-failed`, `radio-state`, `shutdown`),
`detail` (plain English), `since`, `retry_in_s`, `pluto`, `last_exit`, `last_restore`.
`offload/radio-borrow` writes the same file with `state: "borrowed"` while a borrower runs.
`SIGTERM` stops the producer, verifies the restore and exits; the unit allows 150 s.

The SSH password is `PLUTO_SSH_PASSWORD` or the ADI factory default; it travels in the `SSHPASS`
environment of the child process only.
