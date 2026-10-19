# On-Pluto channelization

This subsystem moves frequency selection, filtering, decimation, and activity
selection before Pluto's USB link. Host decoders continue to receive complex IQ.

Development starts with an unchanged-firmware ARM prototype and a measured
transport. Full-window FPGA channelization follows once a reproducible build is
available. Firmware or boot changes require verified device-specific flash
backups and an available recovery path.

Device backups live under the main checkout's ignored `output/pluto-backups/`,
with owner-only permissions. They contain private configuration and must not be
committed or uploaded.
