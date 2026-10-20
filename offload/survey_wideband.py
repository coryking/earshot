"""Turn offload/capture_wideband.py snapshot sets into a catalog-style activity inventory.

Each center directory holds SigMF buffer captures (ci16_le, 25.6 Msps). Buffers from all
passes of one center are concatenated into a temporary WAV and surveyed with the project's
existing `radio_tools.survey.survey_iq` (the same candidate format as
output/iq-survey-wide-*.json and output/wideband-decodes/inventory*.json). The buffers are
spaced about a second apart, so `sampled_occupancy` is a duty-cycle estimate over a few
hundred short windows spread across the capture session, not a continuous measurement.

    uv run python offload/survey_wideband.py output/wideband-iq/survey output/wideband-iq/survey-inventory.json
"""

from __future__ import annotations

import json
import sys
import tempfile
import wave
from collections import defaultdict
from pathlib import Path

import numpy as np

from radio_tools.survey import survey_iq

USABLE_HALF_SPAN_HZ = 10_000_000  # 20 MHz RF bandwidth; outside it the analog filter rolls off


def mode_guess(width_hz: float, kind: str) -> str:
    """Rough emission class from occupied width alone. Not protocol identification."""
    if kind == "narrow_carrier" or width_hz < 1500:
        return "unmodulated carrier / spur / data tone"
    if width_hz < 5000:
        return "narrow (<=6.25 kHz channel): NXDN48, dPMR, or data"
    if width_hz < 9500:
        return "12.5 kHz digital (DMR, P25, NXDN96) or low-deviation FM"
    if width_hz < 13000:
        return "narrowband analog FM (11K0F3E) or 12.5 kHz digital"
    if width_hz < 30000:
        return "wider FM (25 kHz) or adjacent channels merged"
    return "wideband emission (broadcast, TV, LTE, or similar)"


def atsc_pilot(frequency_hz: float) -> int | None:
    """Nearest UHF ATSC pilot (channel lower edge + 309.441 kHz) within 25 kHz, else None."""
    if frequency_hz < 470e6:
        return None
    k = round((frequency_hz - 470e6 - 309_441) / 6e6)
    nominal = 470e6 + 6e6 * k + 309_441
    return int(nominal) if abs(frequency_hz - nominal) < 25_000 else None


def concatenate(files: list[Path], wav_path: Path, rate: int) -> int:
    total = 0
    with wave.open(str(wav_path), "wb") as wav:
        wav.setnchannels(2)
        wav.setsampwidth(2)
        wav.setframerate(rate)
        for f in files:
            raw = np.fromfile(f, dtype="<i2")
            wav.writeframesraw(raw.tobytes())
            total += raw.size // 2
    return total


def main() -> int:
    root, output = Path(sys.argv[1]), Path(sys.argv[2])
    groups: dict[tuple[int, int], list[Path]] = defaultdict(list)
    gains: dict[tuple[int, int], list[str]] = defaultdict(list)
    for sidecar in sorted(root.glob("*/*.sigmf-meta")):
        info = json.loads(sidecar.read_text())
        g = info["global"]
        if g.get("radio:capture_kind") != "single_buffer" or g.get("core:datatype") != "ci16_le":
            continue
        key = (int(info["captures"][0]["core:frequency"]), int(g["core:sample_rate"]))
        groups[key].append(sidecar.with_suffix(".sigmf-data"))
        gains[key].append(g.get("radio:hardwaregain_after", "unknown"))
    inventory: list[dict] = []
    pilots: list[float] = []
    summary: list[dict] = []
    for (center, rate), files in sorted(groups.items()):
        with tempfile.TemporaryDirectory() as tmp:
            wav_path = Path(tmp) / f"baseband_{center}Hz_survey.wav"
            samples = concatenate(files, wav_path, rate)
            data = survey_iq(
                wav_path,
                seconds=samples / rate,
                center_hz=float(center),
                fft_size=32768,
                windows=512,
                limit=100,
            )
        result = data.result
        usable = [
            c
            for c in result.candidates
            if abs(c.offset_hz) <= USABLE_HALF_SPAN_HZ and "edge_artifact_possible" not in c.flags
        ]
        summary.append(
            {
                "center_hz": center,
                "rate_hz": rate,
                "buffers": len(files),
                "sampled_duration_s": result.sampled_duration_s,
                "window_s": result.fft_size / rate,
                "candidates_total": result.candidates_total,
                "candidates_usable": len(usable),
                "near_full_scale_fraction": result.sampled_near_full_scale_fraction,
                "rf_gain_db_by_buffer": gains[(center, rate)],
            }
        )
        for c in usable:
            pilot = atsc_pilot(c.frequency_hz) if c.width_hz < 6000 else None
            if pilot:
                pilots.append(c.frequency_hz / pilot - 1)
            inventory.append(
                {
                    "offset_hz": round(c.offset_hz),
                    "nominal_frequency_hz": round(c.frequency_hz),
                    "frequency_calibrated": False,
                    "intelligibility": "not evaluated",
                    "survey": c.model_dump(),
                    "window_center_hz": center,
                    "mode_guess": mode_guess(c.width_hz, c.kind),
                    "atsc_pilot_nominal_hz": pilot,
                    "duty_cycle": c.sampled_occupancy,
                }
            )
    inventory.sort(key=lambda r: r["nominal_frequency_hz"])
    output.write_text(
        json.dumps(
            {
                "notes": [
                    "Frequencies are Pluto-referenced and uncalibrated.",
                    (
                        "Duty cycle is the fraction of ~1.3 ms spectrum windows above threshold, drawn"
                        " evenly from buffers captured about a second apart in two passes; it"
                        " under-samples short bursts."
                    ),
                    (
                        "Mode guess uses occupied width only. Windows overlap near their edges, so the"
                        " same emitter can appear twice; only |offset| <= 10 MHz and non-edge"
                        " candidates are listed."
                    ),
                    (
                        "Narrow lines at 470 MHz + k*6 MHz + 0.309 MHz are ATSC digital-TV pilots"
                        " (atsc_pilot_nominal_hz). Their offset from the nominal pilot gives the"
                        " Pluto's frequency error (pilot_ppm): an estimate that includes the"
                        " stations' own tolerance, not a calibration."
                    ),
                    "AGC was left on, so absolute levels differ between windows (gain recorded per window).",
                ],
                "pilot_ppm": {
                    "count": len(pilots),
                    "median": float(np.median(pilots) * 1e6) if pilots else None,
                    "min": float(min(pilots) * 1e6) if pilots else None,
                    "max": float(max(pilots) * 1e6) if pilots else None,
                    "meaning": "measured minus nominal pilot frequency, ppm; positive = Pluto reads high",
                },
                "windows": summary,
                "inventory": inventory,
            },
            indent=1,
        )
    )
    print(f"{len(inventory)} candidates from {len(groups)} windows -> {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
