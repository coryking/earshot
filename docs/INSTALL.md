# Installing earshot

This takes you from a stock PlutoSDR to a receiver you can open on your phone. It's the
setup that runs at my house, written down so someone else can repeat it. It has been done
exactly once, on one Pluto, so expect rough edges and please file issues.

There are four levels. Stop at whichever one you need:

1. [Look at the app with no radio](#1-look-at-the-app-with-no-radio). Node only.
2. [Build the host software](#2-build-the-host-software). Rust, Node, uv, cmake.
3. [Prepare the Pluto](#3-prepare-the-pluto) and [load the FPGA image](#4-get-the-fpga-image).
   This is the first step that writes to the Pluto's flash.
4. [Run it all the time](#5-run-it) with systemd user units.

## What you need

- **A PlutoSDR Rev.B** (Zynq xc7z010, AD9363) on ADI firmware **v0.37**. Other revisions and
  firmware versions haven't been tried. The boot-image step checks what it's modifying and
  refuses anything it doesn't recognise, but it can't vouch for hardware I don't have.
- **A Linux host** the Pluto is plugged into over USB, or that can reach it over the network as
  `pluto.local`. Mine is Ubuntu on x86-64. The host software also builds on macOS.
- An antenna for the band you care about. The default window is 450–470 MHz.

Host packages (Ubuntu names):

```sh
sudo apt install build-essential cmake git sshpass openssh-client libiio-utils dfu-util
```

Plus [Rust](https://rustup.rs/) (edition 2024, so 1.85 or newer), Node 22 and
[uv](https://docs.astral.sh/uv/). `libiio-utils` provides `iio_info` and `iio_attr`, which the
supervisor uses to read the radio's settings back. `dfu-util` is only needed for the boot
image and recovery.

Clone to `~/projects/earshot`. The example systemd units assume that path.

```sh
git clone https://github.com/coryking/earshot ~/projects/earshot
cd ~/projects/earshot
```

## 1. Look at the app with no radio

```sh
npm ci --prefix web && npm run dev --prefix web
```

Open the printed URL with `/?demo` on the end. Everything you see is sample data.

## 2. Build the host software

**Engine and web app.** `bootstrap-codecs.sh` clones DSDcc and mbelib at pinned commits,
patches DSDcc so the engine can supply its own AMBE+2 decoder, and installs both into
`engine/.native`. Nothing goes system-wide.

```sh
cd engine
./scripts/bootstrap-codecs.sh
cargo build --release --locked
cargo test --release --locked     # off-air corpora aren't in the repo; those tests skip
cd ..
npm ci --prefix web && npm run build --prefix web
```

**Supervisor.** It keeps the Pluto's producer running (see [`supervisor/`](../supervisor/README.md)).

```sh
cargo build --release --locked --manifest-path supervisor/Cargo.toml
```

**On-Pluto tools.** `chanctl` (register access to the channelizer) and `pluto-fpga-source`
(the producer that runs on the Pluto) are static ARMv7 binaries. The build uses zig's C
compiler through `uvx`, so there's no cross toolchain to install:

```sh
fpga/sw/build.sh        # writes ~/.cache/amateur-radio-fpga/sw/{chanctl,pluto-fpga-source}
```

You can now run the engine against its built-in fixture listener with no radio. Leave
`--source` off and it waits for IQ on `127.0.0.1:8811`:

```sh
engine/target/release/radio-engine serve --web-dir web/dist
```

## 3. Prepare the Pluto

The channelizer streams IQ into the Pluto's DDR through two DMA engines, and the producer
reads it through `/dev/mem`. That memory has to be kept away from Linux. The stock boot image
doesn't do that, so the Pluto needs a boot image whose device trees reserve 64 MiB at
`0x04000000` with `no-map`. Nothing else changes: the stock FPGA, kernel and ramdisk are
kept byte for byte, and the channelizer itself is loaded later, at runtime, into the
volatile FPGA.

This is the one step that writes flash. Read [`offload/RECOVERY.md`](../offload/RECOVERY.md)
first. It has the flash map and how to get back to stock.

**Check access and firmware.** The Pluto answers SSH as `root` (password `analog` unless
you've changed it; set `PLUTO_SSH_PASSWORD` if you have, or install your SSH key).

```sh
ssh root@pluto.local true
iio_info -u ip:pluto.local | grep -E 'hw_model|fw_version'
```

You want `fw_version: v0.37`. If you're on something else, update with
[ADI's firmware guide](https://wiki.analog.com/university/tools/pluto/users/firmware) first.

**Back up the flash.** Put the Pluto in flash DFU mode and read every partition. Keep these
files private: the environment and NVM partitions can hold your device's serial and
credentials.

```sh
mkdir -p ~/pluto-backup && cd ~/pluto-backup
ssh root@pluto.local device_reboot sf
dfu-util -l                                   # wait for 0456:b674 with the five alternates
for a in boot uboot-env spare firmware; do
  dfu-util -d 0456:b673,0456:b674 -a $a.dfu -U $a.bin
done
dfu-util -d 0456:b673,0456:b674 -a firmware.dfu -e     # leave DFU and boot normally
```

Do it twice and compare the SHA256s if you want to be sure the reads are good. That's what I
did.

**Build the reservation image.** `firmware.bin` is the whole 30 MiB partition. Cut the FIT
out of it (its length is in the FDT header), then add the reservation:

```sh
python3 -I -c 'import sys; d = open(sys.argv[1], "rb").read(); open(sys.argv[2], "wb").write(d[:int.from_bytes(d[4:8], "big")])' \
  firmware.bin firmware-original.itb
python3 ~/projects/earshot/fpga/sw/make_reserved_stock_itb.py firmware-original.itb reserved.itb
```

The script checks every stock image hash, changes only the three DTBs, verifies the result,
and pads it to the original length so U-Boot's `fit_size` stays correct. It prints the
hashes as JSON. It never touches the device.

**Try it from RAM first.** The Pluto can boot an image sent over DFU into RAM without
touching flash. A power cycle puts you back on the stock image.

```sh
cp reserved.itb reserved.dfu && dfu-suffix -a reserved.dfu -v 0456 -p b673
ssh root@pluto.local device_reboot ram
dfu-util -d 0456:b673,0456:b674 -a firmware.dfu -D reserved.dfu
dfu-util -d 0456:b673,0456:b674 -a firmware.dfu -e
ssh root@pluto.local ls /proc/device-tree/reserved-memory/    # should list dma_probe@4000000
```

**Flash it.** Once it boots from RAM, write it the same way:

```sh
ssh root@pluto.local device_reboot sf
dfu-util -d 0456:b673,0456:b674 -a firmware.dfu -D reserved.dfu
dfu-util -d 0456:b673,0456:b674 -a firmware.dfu -U readback.bin
cmp -n "$(stat -c %s reserved.itb)" reserved.itb readback.bin && echo match
dfu-util -d 0456:b673,0456:b674 -a firmware.dfu -e
```

Send the FIT, not the whole 30 MiB partition. On my unit a full-partition download failed
partway with DFU status 14, and the FIT-only download worked. Keep power connected while it
writes. To go back to stock, flash `firmware-original.itb` the same way.

## 4. Get the FPGA image

**Use the tested release.** The loader only accepts the bitstream that was tested on
hardware. It checks the SHA256 before it touches the Pluto. Download it from the
[`pluto-chan-8cea8ae` release](https://github.com/coryking/earshot/releases/tag/pluto-chan-8cea8ae):

```sh
B=~/.cache/amateur-radio-fpga/release/pluto_chan-8cea8ae
mkdir -p "$B" && cd "$B"
gh release download pluto-chan-8cea8ae -R coryking/earshot
sha256sum -c SHA256SUMS
```

**Load it once by hand** to see it work:

```sh
cd ~/projects/earshot
fpga/sw/ensure_pluto_fpga.sh "$B/system_top.bit.bin" ~/.cache/amateur-radio-fpga/sw/chanctl
```

This checks the reservation and Linux's memory map, loads the image through the FPGA
manager, and reads the channelizer's magic register before it reports ready. Run it again and
it does nothing. If anything about the boot looks wrong, it refuses to start DMA. Over USB it
also detaches and reattaches the Pluto's USB gadget around the swap, and arms a 90-second
on-device timer that reboots the Pluto if the swap doesn't finish. A reboot loses the runtime
image, which is why the service runs this before every start.

**Or build it yourself.** You need Vivado 2026.1 (the free Standard edition covers the
xc7z010) and ADI's HDL repository at `hdl_2026_r1`:

```sh
git clone --branch hdl_2026_r1 --depth 1 https://github.com/analogdevicesinc/hdl ~/.cache/amateur-radio-fpga/hdl
ADI_IGNORE_VERSION_CHECK=1 make -C ~/.cache/amateur-radio-fpga/hdl/projects/pluto
```

`hdl_2026_r1` expects Vivado 2025.1. `ADI_IGNORE_VERSION_CHECK=1` is ADI's own override, and
it's what I build with. Building ADI's stock Pluto project also builds the IP libraries the
channelizer project needs. Allow an hour or more the first time. Then build the channelizer:

```sh
fpga/scripts/build_project.sh pluto_chan       # ~35 min; output in ~/.cache/amateur-radio-fpga/build/pluto_chan
```

[`fpga/README.md`](../fpga/README.md) covers the model, the simulation and the reports. Two
things to know:

- The release came from commit `8cea8ae` of the private repo this one was split from. The
  RTL on `main` is the same design, apart from one block of declarations in `chan_pack.v`
  that moved, but Vivado won't produce the same bytes. Your build won't match the pinned
  hash, so `ensure_pluto_fpga.sh` won't load it until you change `expected` in that script.
  That check is there so the always-on service can only ever load an image that passed
  hardware testing. Change it on purpose, after testing your build.
- To turn a `.bit` into the `.bit.bin` the FPGA manager loads, use Vivado's `bootgen`:
  `bootgen -image system_top.bif -arch zynq -process_bitstream bin`, with a `.bif` that lists
  `system_top.bit`.

## 5. Run it

**By hand:**

```sh
supervisor/target/release/radio-supervisor --host pluto.local \
  --binary ~/.cache/amateur-radio-fpga/sw/pluto-fpga-source \
  --center 460016998 --forward 404 --status-file /tmp/receiver.json --engine 127.0.0.1:8810 &
engine/target/release/radio-engine serve --source tcp:pluto.local:9010 \
  --receiver-status /tmp/receiver.json --web-dir web/dist
```

Open `http://127.0.0.1:8810/`. The engine serves on loopback only and has no login. If you
want it on your phone, put your own access control in front of it. I use Tailscale Serve.

`--center` is the LO in Hz as the radio reads it. The window covers about 20 MHz around it,
so 460016998 covers roughly 450–470 MHz. Pick the frequency you want to listen to in the
browser.

**All the time.** [`deploy/systemd/`](../deploy/systemd/) has the user units I run:

| Unit | What it does |
| --- | --- |
| `amateur-radio.service` | Engine and web app on `127.0.0.1:8810` |
| `amateur-radio-live.service` | Loads the FPGA image if needed, then runs the supervisor |
| `amateur-radio-fpga-guard.timer` | Restarts the live unit if the Pluto rebooted and lost its image |
| `amateur-radio-borrow-guard.timer` | Restarts the live unit if an `offload/radio-borrow` command was killed |

```sh
mkdir -p ~/.config/systemd/user ~/.config/amateur-radio
cp deploy/systemd/* ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now amateur-radio.service amateur-radio-live.service \
  amateur-radio-fpga-guard.timer amateur-radio-borrow-guard.timer
sudo loginctl enable-linger "$USER"       # keep them running after you log out
```

Settings go in env files, so the units stay unedited:

- `~/.config/amateur-radio/fpga-window.env`: `FPGA_CENTER=<Hz>` and `FPGA_FORWARD=<bin>` to
  move the window.
- `~/.config/amateur-radio/engine.env`: `FREQ_PPM=<ppm>`. This is how far high your Pluto's
  crystal reads. Mine is +23.3. Tune to a carrier whose frequency you know and compare.
- `~/.config/amateur-radio/live.env`: `PLUTO_SSH_PASSWORD=...` if you changed it.

To use the Pluto for something else, don't just stop the live unit and forget about it. Run
`offload/radio-borrow <command>`. It stops the receiver, runs your command, and starts the
receiver again when the command exits.

## Check that it works

Don't count a successful build or a clean load as done. The test is this: open the page,
pick a busy channel on **Now**, and hear audio. Then:

```sh
curl -s http://127.0.0.1:8810/api/status | python3 -m json.tool | less
journalctl --user -u amateur-radio-live -n 50
```

`/api/status` shows whether the receiver is live or reconnecting, any drops, and the
spectrum geometry it measured. The live unit's journal shows each load, start and restart.

## Undo

- **Stop it:** `systemctl --user disable --now amateur-radio-live amateur-radio`. The
  supervisor puts the radio's settings back the way it found them before it exits.
- **Remove the channelizer:** reboot the Pluto. The runtime image doesn't survive a reboot.
- **Remove the reservation:** flash `firmware-original.itb` back to `firmware.dfu`, as in
  step 3. The full procedure, including what to do when Linux won't boot, is in
  [`offload/RECOVERY.md`](../offload/RECOVERY.md).
