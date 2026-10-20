# radio-supervisor

Keeps the Pluto's channel producer running for the always-on radio, and puts the radio back the
way it found it whenever the producer stops. RX only: it never transmits, flashes, or touches the
NOR or the U-Boot environment. Not in the data path (the engine reads the producer over TCP).

```sh
cargo build --release --locked        # supervisor/target/release/radio-supervisor
radio-supervisor --binary ~/.cache/amateur-radio-fpga/sw/pluto-fpga-source \
  --status-file /run/user/1000/amateur-radio/receiver.json
```

The producer is `pluto-fpga-source` (`fpga/sw/build.sh`). The installed boot FIT
reserves DMA memory while `amateur-radio-live` loads the tested FPGA image into
volatile PL before this supervisor starts (`fpga/README.md`). The managed unit
starts it as `--port 9010 --center 460016998 --gain 40 --forward-streams 404`;
`--center`, `--gain` and `--forward` can change it. That gives about 20 MHz of RF
in 512 bins of 50 kHz, with manual gain. There is no ARM fallback or image
detection. If the runtime PL image is absent, the producer refuses to open the
custom registers.

The console's **Spectrum window** controls move by one RF bandwidth with
**Previous / Next**, or jump to a **Center MHz** with **Go**. These retune the
running producer with LO readback; rate and RF bandwidth stay fixed. Moving
releases this browser's listening selection and resets spectrum history. The
window is shared by all listeners. A producer restart returns to the supervisor's
startup `--center`.

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

The browser's **Start receiving / Stop receiving** uses `POST /api/receiver` on the engine.
The engine atomically saves `{"receiving":false}` or `true` beside the status file,
with extension `.control.json` (for `receiver.json`, `receiver.control.json`). Missing means
receiving is enabled; unreadable or malformed means disabled. The supervisor checks this
intent while running and between retries. Stop closes the engine's connection gate, stops
the producer, verifies restoration, releases the lock, and stays at `stopped/operator`.
Start goes through the normal lock, snapshot and producer startup path. The file survives
service restarts; if its directory is under `/run`, it does not survive a host reboot.

Status advertises `control_supported: true` and `receiving_requested`. A successful control
request means intent was saved, so the UI waits for fresh samples after Start or
`stopped/operator` with restoration readback after Stop. The old `POST /api/stop` remains a
single producer-session stop and can still be restarted by the supervisor.

The SSH password is `PLUTO_SSH_PASSWORD` or the ADI factory default; it travels in the `SSHPASS`
environment of the child process only.
