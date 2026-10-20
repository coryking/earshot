"""Read-back and last-resort restoration of the Pluto's receiver state through iio_attr.

RX only. Nothing here opens a transmit buffer or writes a TX LO/gain. The fallback restore
writes the receive controls (and the FIR enable bits when they differ) back to a snapshot,
then reads everything again; callers compare the result with the snapshot.
"""

from __future__ import annotations

import subprocess

RX = ("-i", "-c", "ad9361-phy", "voltage0")
TX = ("-o", "-c", "ad9361-phy", "voltage0")
RX_LO = ("-o", "-c", "ad9361-phy", "altvoltage0")
TX_LO = ("-o", "-c", "ad9361-phy", "altvoltage1")

# Read back after every session and compared with the pre-session snapshot. The TX baseband
# clock follows the RX rate in the AD9361 driver, so it is verified but never written here.
RESTORED_KEYS = (
    "rx_sampling_frequency",
    "rx_rf_bandwidth",
    "rx_gain_control_mode",
    "rx_filter_fir_en",
    "rx_lo",
    "tx_sampling_frequency",
    "tx_filter_fir_en",
    "tx_lo",
)


def iio_attr(uri: str, *args: str) -> str:
    """Last output line of `iio_attr -u URI ARGS` (the attribute value, or the write echo)."""
    out = subprocess.run(
        ["iio_attr", "-u", uri, *args], check=True, capture_output=True, text=True, timeout=30
    ).stdout.strip()
    return out.splitlines()[-1].strip()


def snapshot(uri: str) -> dict[str, str]:
    return {
        "rx_sampling_frequency": iio_attr(uri, *RX, "sampling_frequency"),
        "rx_rf_bandwidth": iio_attr(uri, *RX, "rf_bandwidth"),
        "rx_gain_control_mode": iio_attr(uri, *RX, "gain_control_mode"),
        "rx_hardwaregain": iio_attr(uri, *RX, "hardwaregain"),
        "rx_filter_fir_en": iio_attr(uri, *RX, "filter_fir_en"),
        "rx_lo": iio_attr(uri, *RX_LO, "frequency"),
        "tx_sampling_frequency": iio_attr(uri, *TX, "sampling_frequency"),
        "tx_filter_fir_en": iio_attr(uri, *TX, "filter_fir_en"),
        "tx_lo": iio_attr(uri, *TX_LO, "frequency"),
    }


def mismatches(original: dict[str, str], after: dict[str, str]) -> dict[str, list[str]]:
    return {k: [original[k], after[k]] for k in RESTORED_KEYS if original[k] != after[k]}


def restore_receiver(uri: str, original: dict[str, str]) -> list[str]:
    """Write the receive controls back to `original`, one attribute at a time.

    Every write is attempted even if an earlier one fails. Returns the error strings.
    """
    errors: list[str] = []
    writes = [
        (RX, "sampling_frequency", original["rx_sampling_frequency"]),
        (RX, "rf_bandwidth", original["rx_rf_bandwidth"]),
        (RX_LO, "frequency", original["rx_lo"]),
        (RX, "gain_control_mode", original["rx_gain_control_mode"]),
        (RX, "filter_fir_en", original["rx_filter_fir_en"]),
    ]
    for target, attr, value in writes:
        try:
            iio_attr(uri, *target, attr, value)
        except (subprocess.SubprocessError, OSError) as exc:
            errors.append(f"{attr}: {exc!r}")
    return errors
