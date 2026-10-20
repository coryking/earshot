"""Build engine/data/channel-labels.json from the Seattle RF research datasets.

Sources (docs/data/, from the rf-research branch; RadioReference cross-checked against FCC ULS):
  seattle-trunked-systems.json   trunked/conventional systems with per-site channel lists
  seattle-uls-licenses.json      FCC ULS licences; 450-470 MHz band only is used here

Each output entry is keyed by the 6.25 kHz raster channel (Hz) of a TRUE (Pluto-corrected)
frequency and carries a plain name for the household UI. System channels win over bare ULS
licences. A licence is not proof of operation. Names are cleaned automatically and a few
well-known systems get hand-written plain names (PLAIN_NAMES).

    uv run python offload/build_labels.py
"""

from __future__ import annotations

import json
import re
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RASTER_HZ = 6250
# Hand-written plain names, keyed by the system id in seattle-trunked-systems.json.
PLAIN_NAMES = {
    "washington_state_convention_center": "Convention Center radio",
    "day_wireless_capmax": "Day Wireless (business trunked)",
    "trbowest": "Day Wireless (business trunked)",
    "fleetnet_nxdn": "FleetNet (business trunked)",
    "amazon_tier3": "Amazon campus radio",
    "lumen_field_seahawks_and_sounders": "Lumen Field radio",
    "microsoft_headquarters": "Microsoft campus radio",
    "alaska_airlines_seatac_airport": "Alaska Airlines (SeaTac)",
    "delta_airlines_seatac": "Delta Airlines (SeaTac)",
    "providence_dmr": "Providence Medical Centers",
    "muckleshoot_casino": "Muckleshoot Casino radio",
    "bellevue_collection": "Bellevue Collection radio",
}
SUFFIX = re.compile(
    r"[,.]?\s+(INC|LLC|L\.L\.C|CORP|CORPORATION|CO|COMPANY|LP|LTD|DBA.*)\.?$", re.IGNORECASE
)
SMALL = {"of", "and", "the", "for", "at", "in"}


def clean_licensee(raw: str) -> str:
    """'Day Management Corporation dba Day Wireless Systems' -> 'Day Wireless Systems'."""
    name = raw.strip()
    match = re.search(r"\bdba\b\s+(.*)$", name, re.IGNORECASE)
    if match:
        name = match.group(1)
    for _ in range(3):
        name = SUFFIX.sub("", name).strip(" ,.")
    words = name.split()
    out = [
        w
        if (w.isupper() and len(w) <= 4 and not w.isalpha())
        else w.capitalize()
        if w.isupper() or w.islower()
        else w
        for w in words
    ]
    out = [w.lower() if i and w.lower() in SMALL else w for i, w in enumerate(out)]
    return " ".join(out)


def raster(mhz: float) -> int:
    return round(mhz * 1e6 / RASTER_HZ) * RASTER_HZ


def proto_hint(family: str) -> str:
    f = family.upper()
    return "DMR" if "DMR" in f else "NXDN48" if "NXDN" in f else "P25" if "P25" in f else family


def main() -> int:
    systems = json.loads((ROOT / "docs/data/seattle-trunked-systems.json").read_text())["systems"]
    uls = json.loads((ROOT / "docs/data/seattle-uls-licenses.json").read_text())["bands"][
        "uhf_450_470"
    ]
    entries: dict[int, dict] = {}
    # 1. System channels. A frequency can appear in several systems (shared channels); keep the
    #    candidate whose site is nearest downtown, with control channels outranking voice ones.
    candidates: dict[int, list[dict]] = defaultdict(list)
    for system in systems:
        name = PLAIN_NAMES.get(system["id"], clean_licensee(system["name"]))
        hint = proto_hint(system["protocol"]["family"])
        for site in system["sites"]:
            distance = site.get("dist_from_downtown_km")
            distance = 999.0 if distance is None else float(distance)
            for role, key in (("control", "control_channels_mhz"), ("voice", "other_channels_mhz")):
                for mhz in site.get(key) or []:
                    candidates[raster(mhz)].append(
                        {
                            "name": name,
                            "kind": role,
                            "protocol": hint,
                            "system": system["id"],
                            "source": "radioreference",
                            "distance_km": distance,
                        }
                    )
    # 2. Licensed base stations by licensee, nearest to downtown first.
    by_channel: dict[int, dict[str, float]] = defaultdict(dict)
    for lic in uls:
        distances = [
            loc["dist_downtown_km"] for loc in lic["locations"] if loc.get("loc_class") == "F"
        ]
        nearest = min(distances) if distances else 999.0
        for f in lic["freqs"]:
            if not any(c.startswith(("FB", "FX")) for c in f["station_classes"]):
                continue
            name = clean_licensee(lic["licensee"])
            by_channel[raster(f["mhz"])][name] = min(
                nearest, by_channel[raster(f["mhz"])].get(name, 999.0)
            )
    for hz in set(candidates) | set(by_channel):
        cands = candidates.get(hz, [])
        licensees = by_channel.get(hz, {})
        ranked = sorted(licensees, key=licensees.get)
        best = (
            min(cands, key=lambda c: (c["kind"] != "control", c["distance_km"])) if cands else None
        )
        if (
            best
            and ranked
            and licensees[ranked[0]] < best["distance_km"] - 5
            and best["kind"] != "control"
        ):
            best = None  # a licensee right downtown beats a distant system that lists the channel
        if best:
            entry = {k: v for k, v in best.items() if k != "distance_km"}
            others = sorted({c["name"] for c in cands} - {best["name"]})
            if others:
                entry["also_listed_for"] = others[:3]
        elif 450e6 <= hz <= 470e6 and ranked:
            entry = {
                "name": ranked[0] if len(ranked) <= 3 else "Shared business channel",
                "kind": "licensed",
                "protocol": None,
                "source": "fcc-uls",
            }
        else:
            continue
        if ranked:
            entry["licensees"] = ranked[:3]
            entry["n_licensees"] = len(ranked)
        entries[hz] = entry
    out = {
        "schema_version": 1,
        "raster_hz": RASTER_HZ,
        "note": "Keys are TRUE frequencies in Hz on the 6.25 kHz raster. A licence is not proof of operation.",
        "sources": [
            "docs/data/seattle-trunked-systems.json",
            "docs/data/seattle-uls-licenses.json",
        ],
        "channels": {str(hz): entries[hz] for hz in sorted(entries)},
    }
    path = ROOT / "engine/data/channel-labels.json"
    path.parent.mkdir(exist_ok=True)
    path.write_text(json.dumps(out, separators=(",", ":")) + "\n")
    print(f"{len(entries)} channels -> {path} ({path.stat().st_size} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
