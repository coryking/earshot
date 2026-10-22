# Pluto recovery

The device-specific backup is in the main checkout at
`output/pluto-backups/20261019T221101Z/`. Keep that directory private. The raw
environment, NVM filesystem, and runtime archive can contain credentials.

The backup was read from this Rev.B device's NOR flash, not downloaded from a
release. Each partition's SHA256 matched a second independent device read. Its
Zynq boot header, U-Boot environment CRC32, JFFS2 header, and firmware FIT header
and length were validated. `manifest.json` records sizes and hashes. The
concatenated `qspi-complete-32MiB.bin` covers all 32 MiB without gaps. The original
firmware FIT is also extracted separately. The runtime archive contains `/etc`,
`/mnt/jffs2`, and `/root`; the live device tree is saved separately.

The device currently boots a reservation-only FIT in the Linux/FPGA/rootfs
partition, SHA256 `b3ae872c10413f654990c7baa2bf74a476ce422973e1632a22ecf6887f377ecc`.
Its stock FPGA, kernel and ramdisk data match this backup byte for byte. Only
three DTBs reserve `0x04000000–0x07ffffff` as `dma_probe@4000000` with `no-map`;
the channelizer PL is loaded separately at runtime. The installed FIT can be
reproduced exactly, without a device write or U-Boot environment change:

```sh
python3 fpga/sw/make_reserved_stock_itb.py \
  output/pluto-backups/20261019T221101Z/firmware-original.itb \
  output/fpga-mvp/reproduced-reserved.itb
```

The output must hash to the installed SHA above. `make_frm.py` instead makes a
channelizer-in-FIT image for a different boot path; do not use its `chan_dma`
reservation node as proof that custom PL is running.

Recheck the backup and prepare original DFU files on the Mac:

```sh
uv run offload/prepare_recovery.py \
  /Users/coryking/projects/earshot/output/pluto-backups/20261019T221101Z \
  --yes --json
```

This only writes protected local recovery copies. It verifies each payload and
uses `dfu-suffix` to add/check a DFU suffix. Original files are untouched. Both
`dfu-util` and `dfu-suffix` are installed through Homebrew. A `--dry-run` or
omitting `--yes` verifies without creating files.

## Exact flash map

Confirmed from live `/proc/mtd`, `/sys/class/mtd`, and this device's saved
`dfu_sf_info` environment. Use names, not assumed alternate-setting numbers.

| Partition | Offset | Bytes | DFU alternate name | Recovery file |
| --- | ---: | ---: | --- | --- |
| boot | `0x000000` | `0x100000` | `boot.dfu` | `original-boot.dfu` |
| environment | `0x100000` | `0x020000` | `uboot-env.dfu` | `original-uboot-env.dfu` |
| NVM filesystem | `0x120000` | `0x0e0000` | `spare.dfu` | `original-nvmfs.dfu` |
| Linux/FPGA/rootfs FIT | `0x200000` | `0x1e00000` | `firmware.dfu` | FIT-only `firmware-original.dfu` (see below) |

`uboot-extra-env.dfu` is a 4 KiB region at `0x0ff000` inside the boot partition;
the full boot backup already includes it. Do not restore an extra environment
from an unrelated image.

## Enter and leave recovery mode

Obtain an exclusive receiver lease before rebooting; stop all host IIO clients.
The normal identity is serial `<your Pluto serial>`, VID/PID
`0456:b673`. DFU uses `0456:b674`; verify exactly one matching physical device.

With a working Linux console, `device_reboot sf` enters flash DFU. If Linux does
not boot, hold the recessed button while reconnecting power/data USB, then
release it after recovery starts. Rev.B uses the middle data USB connector;
do not depend on Rev.C/D's extra power-connector console behavior.

```sh
dfu-util -l
```

The saved environment identifies the expected alternates above. On 2026-10-19,
the device entered DFU and all five alternates enumerated as expected. Uploads
of all four flash partitions matched the original SHA256 hashes byte for byte
(all 32 MiB). `dfu-proof/proof.json` and private readbacks/logs record that check.
The device then left DFU, booted the original Linux, and accepted the original
5 MHz sample rate/bandwidth, 454216998 Hz LO, and fast-attack gain mode. No flash
download/write command was used for this rescue-path test. Repeat the check if
bootloader or partition layout changes; a saved map alone is insufficient.

To leave DFU without writing anything:

```sh
dfu-util -d 0456:b673,0456:b674 -a firmware.dfu -e
```

Wait for the normal USB/network device, verify identity, and restore the prior
receive controls before returning the hardware lease. There is no reason to
erase/rewrite flash simply to test recovery entry.

## Restore only when needed

First revalidate the backup with the preparation command. Prepare a FIT-only
firmware payload from the verified backup (the full partition stays useful as
an archival readback):

```sh
cd /Users/coryking/projects/earshot/output/pluto-backups/20261019T221101Z
cp firmware-original.itb recovery/firmware-original.dfu
dfu-suffix -a recovery/firmware-original.dfu -v 0456 -p b673
```

Do not download `original-linux.dfu`, the full 30 MiB partition. On this unit it
failed at 23,044,096 bytes with DFU status 14; the FIT-only payload restored
successfully. The runtime/DFU ID pair below also accepts the prepared files'
runtime-ID suffix. These corrections were verified during the recovery in
the original bring-up (tracked privately).

In DFU mode, the following commands restore this unit's original content. They are recovery
instructions, not a sequence to run for routine experiments:

```sh
cd /Users/coryking/projects/earshot/output/pluto-backups/20261019T221101Z/recovery
dfu-util -d 0456:b673,0456:b674 -a firmware.dfu -D firmware-original.dfu
# Restore the following only if their contents were changed/damaged:
dfu-util -d 0456:b673,0456:b674 -a boot.dfu -D original-boot.dfu
dfu-util -d 0456:b673,0456:b674 -a spare.dfu -D original-nvmfs.dfu
dfu-util -d 0456:b673,0456:b674 -a uboot-env.dfu -D original-uboot-env.dfu
```

Read restored partitions back with `dfu-util -U` and compare their payload
hashes to `manifest.json` before leaving recovery. A completed download is not
readback verification. Boot/environment restoration is unnecessary for a
Linux-FIT-only change and should not be added casually. Power must remain
connected throughout flash writes.

If the first-stage bootloader itself is damaged and the button cannot enumerate
DFU, recovery requires the appropriate ADALM-JTAGUART/JTAG bootstrap path. An
ordinary USB DFU command cannot recover a missing first-stage bootloader. Keep
initial experiments in userspace `/tmp` or the Linux FIT to preserve that path.

References: [ADI firmware/DFU guide](https://wiki.analog.com/university/tools/pluto/users/firmware),
[ADI boot flow](https://wiki.analog.com/university/tools/pluto/devs/booting).
