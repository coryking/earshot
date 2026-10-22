# RF environment: Seattle / King County, WA

Research for the Pluto/AD9363 wideband receiver and the trunk tracker (tracked privately before this repo went public).
Data retrieved **2026-10-19** (FCC ULS bulk files are dated 2026-10-18; RadioReference pages show "last updated" dates up to 2026-10-18). Branch `rf-research`.

Every number below came from one of the sources in section 11. Anything that is inference, crowd-sourced only, or not cross-checked is marked **[unverified]** or **[inferred]**. Frequencies are in MHz and are *as published or licensed*, not Pluto-corrected (see 6.1 for the Pluto LO error).

## 0. Machine-readable data and schema (for the trunk-tracker agent)

Two files, both in `docs/data/`:

| File | Size | Contents |
|---|---|---|
| `seattle-trunked-systems.json` | ~1.0 MB | Trunked systems (35 from the RR King County list + 4 deprecated/legacy ones), sites, channels, control channels, ULS cross-check, sizing, talkgroups, receive windows, capture reconciliation, sources |
| `seattle-uls-licenses.json` | ~1.7 MB | FCC ULS active licences (conventional and trunk-feeder) within 40 km of downtown Seattle, by band: `uhf_450_470`, `band_700`, `band_800`, `band_900`; `tband_470_512` is empty (see 5.4) |

### `seattle-trunked-systems.json`

Top level: `schema_version`, `generated`, `region`, `conventions`, `pluto_lo_calibration`, `receive_windows[]`, `systems[]`, `capture_matches[]`, `sources[]`.

`systems[]` (one per trunked system):

| Field | Meaning |
|---|---|
| `id` | stable slug, e.g. `psern`, `kc_metro_p25`, `wsp_p25`, `trbowest`; legacy ones are prefixed `legacy_` |
| `status` | `active`, `legacy ...`, or `deprecated per RR listing` |
| `protocol.family` | `APCO P25`, `DMR`, `NXDN`, `Motorola Type II`, `MPT-1327`, ... |
| `protocol.variant` | e.g. `Phase II TDMA`, `Motorola MOTOTRBO Capacity Plus`, `Connect Plus`, `DMR Tier III`, `NXDN 4800 baud Type-C` |
| `protocol.slots_per_channel` | 2 for P25 Phase II and DMR TDMA, 1 for NXDN / analog |
| `system_ids` | `wacn`, `system_id_hex`, `nac`, `rfss` as applicable (P25); `system_id_dec/hex` (Connect Plus/NXDN) |
| `band_description` | human text, e.g. `800 MHz (base TX 851-861, mobile TX 806-816)` |
| `pluto_receivable` | false for systems below 325 MHz (VHF, 220 MHz) |
| `sites[]` | see below |
| `system_channel_union_mhz`, `system_sizing` | union of all channels across the included sites and its span; no concurrency figure at system level |
| `talkgroup_stats`, `talkgroups[]` | counts and the RR talkgroup list (P25 systems only) |
| `rr_sid`, `rr_url`, `rr_last_updated`, `rr_notes`, `notes`, `sources[]` | provenance |
| legacy systems (`legacy_*`) | carry RR site/channel lists and talkgroups only (`sites[]` has `control_channels_mhz`, `other_channels_mhz`, `channel_set_mhz`, `sizing`; no ULS cross-check, no site IDs) |

`sites[]`:

| Field | Meaning |
|---|---|
| `name`, `site_number_dec`, `site_number_hex`, `rfss`, `nac`, `county` | RR site identifiers; for P25 the site number is the Site ID the tracker will see in the network status broadcast |
| `lat`, `lon`, `dist_from_downtown_km`, `coords_suspect` | RR site coordinates; `coords_suspect=true` where RR reuses a placeholder point |
| `neighbors` | RR adjacent-site list (RFSS.site) |
| `control_channels_mhz` | RR-flagged control channels. **RR does not distinguish primary from alternate**; on P25 simulcast cells all flagged candidates are listed. Probe all of them |
| `other_channels_mhz` | RR-listed non-control channels (often empty on P25 because RR lists only control candidates) |
| `uls.licenses[]`, `uls.downlink_mhz`, `uls.uplink_mhz`, `uls.transmit_locations[]` | FCC licence cross-check for the callsigns RR attaches to the site: base-transmit channels (P25: 851-869 / 758-776 MHz), mobile-transmit channels, and fixed tower coordinates from the licence |
| `channel_set_mhz` | channels the tracker should assume the site can use: RR channels, plus ULS base-transmit channels where the licence is site-specific (PSERN, Port of Seattle, WSDOT, South Sound 911) |
| `sizing` | `low_mhz`, `high_mhz`, `span_mhz`, `min_window_mhz` (=span+12.5 kHz), `channels`, `fits_20mhz`, `fits_16mhz_usable`, `max_concurrent_voice_paths`, `slots_per_channel`. Concurrency is a **design maximum** `(channels-1 control) x slots`, not measured traffic |
| `verification` | `rr+uls`, `rr+uls_partial`, `rr_only_control_not_on_licence`, `rr_only`, `uls_callsign_only` (definitions in `conventions.verification_values`) |

`talkgroups[]`: `dec`, `hex`, `alpha_tag`, `description`, `tag`, `category`, `voice_mode` (TDMA/FDMA/analog), `encryption` (`clear` / `encrypted` / `partial`), `rr_mode_raw` (the RR code, e.g. `T Enc`). Encryption flags are crowd-sourced on RR **[unverified]**; the tracker should trust the on-air encryption algorithm ID (ALGID) over this table.

`receive_windows[]`: precomputed contiguous windows (`low_mhz`, `high_mhz`, `span_mhz`, `min_window_mhz`, `suggested_center_mhz`, `fits_20mhz`, `members`).

`pluto_lo_calibration`: `delta_hz` and the rule `actual_hz = nominal_hz - delta_hz` for the existing wideband capture (section 6.1).

`capture_matches[]`: one entry per carrier in `output/wideband-decodes/inventory30.json`: uncorrected and corrected frequency, 6.25 kHz grid residual, RR matches, FCC ULS candidate licensees.

### `seattle-uls-licenses.json`

`bands.<band>[]`: `call`, `licensee`, `radio_service` (IG/YG = Industrial/Business conventional/trunked pool, PW/YW = Public Safety pool, GE/YE/GO/YJ = 800 MHz, SG/SY = 700 MHz, GU/GR/YS/YU = 900 MHz, etc.), `grant`, `expires`, `locations[]` (class, city, county, lat, lon, distance), `freqs[]` (`mhz`, `station_classes`, `emissions`, `likely_modes`). `likely_modes` is a heuristic from the emission designator (7K60FX* = DMR, 4K00F* = NXDN 6.25 kHz, 8K30F* = NXDN 12.5 kHz, 8K10F1* = P25 Phase I, 11K2F3E = analog FM). **A licence is not proof of operation.**

## 1. Headline findings

1. **PSERN is the whole 800 MHz story.** One P25 Phase II system (WACN `BEE00`, System ID `0x3AB`), 10 RR site entries, all downlink channels in **851.0625-860.9625 MHz (9.9 MHz)**. The whole system fits one 20 MHz window; a single site fits in 3.3-8.75 MHz. Seattle proper is served mainly by site 27 "Seattle Perimeter" **[inferred from ULS tower coordinates]** (NAC `3A7`, control candidates 851.5375 / 852.7875 / 853.675 / 854.4875) with site 25 "Core" (NAC `3A5`, Eastside) and 28 "South King" (NAC `3A8`) overlapping.
2. **The legacy King County system (KCERS/KCERCS, Motorola Type II SmartZone, analog) is decommissioned** per RadioReference ("placed in service in 2023, and the analog legacy KCERCS ... was decommissioned"). Its FCC licences (King County / Valley Communications, grants 2016-2017, expiring 2027) still show as active in ULS **[unverified whether any carrier remains on air]**.
3. **Transit, State Patrol and WSDOT are on 700 MHz P25 Phase II, not PSERN** (Metro sysid `1F9`, WSP sysid `9CC`, WSDOT sysid `03D`). All base-transmit channels fall in **769.1-774.8 MHz, a 5.7 MHz block**: one cheap window covers all four systems (Metro, WSP, WSDOT, South Sound 911's West Seattle site). Port of Seattle (sysid `670`) and Boeing run their own 800 MHz P25 inside the same 851-864 MHz window as PSERN, so **one 12.7 MHz window around 857.4 MHz covers PSERN, Port of Seattle, Boeing P25 and the legacy Boeing/ Port Type II systems**.
4. **450-470 MHz is dominated by Day Wireless-hosted trunking in downtown Seattle.** The 15 carriers in `inventory30.json` line up, after a -10.556 kHz Pluto LO correction **[inferred]**, with RR-listed channels of **TRBOWEST Connect Plus** and **Day Wireless Capacity Max** (Columbia Tower control 454.01875 / 454.20625), **FleetNet/Silke NXDN** (Columbia control 454.25), the Washington State Convention Center Capacity Plus system, Amazon's Tier III campus network, and the Metropolitan Improvement District DMR channel (452.675). Section 6.
5. **T-band (470-512 MHz) is not in use in Seattle.** 47 CFR 90.303 lists the 11 metros that have T-band land-mobile; Seattle is not one, and ULS has 0 active licences in 470-512 MHz within 60 km of downtown (~100,000 frequency-location records nationally).
6. **Pluto LO error is large enough to matter** (~23 ppm, ~10.6 kHz at 454 MHz, ~20 kHz at 856 MHz): the tracker needs AFC or a calibration step before trying to lock P25 control channels.

## 2. Method and trust levels

| Source | What it gives | Limits |
|---|---|---|
| RadioReference (RR) DB pages (system, site, talkgroup, King County lists) | Protocol, sysid/WACN/NAC, site numbers and coordinates, control channels, talkgroups, encryption flags | Crowd-sourced. Free view lists **only control channels for PSERN**; the P25 channel-identifier table is premium. Forum posts were not readable (503 "limited to members"). |
| FCC ULS bulk files (`l_LMpriv`, `l_LMcomm`, `l_paging`; dated 2026-10-18) | Licensee, callsign, service, grant/expiry, every licensed frequency, emission designator, tower coordinates | A licence is not proof a channel is in service; shared licences cover many sites; Part 90 DMR/NXDN control channels at 454.0-454.65 MHz are *not* in LMpriv/LMcomm (see 6). |
| psern.org FAQ, RR wiki | Operator, "simulcast 800 MHz ... P25 Phase II", 40 new sites | No site/channel detail. |
| 47 CFR 90.303, 95.1763; 18 USC 2510/2511; 47 USC 605; RCW 9.73.030 | Rules | Not legal advice. |

Cross-checking: for every P25 site where RR gives a callsign I compared RR control channels with the licence's channels. PSERN: 8 of 10 sites fully agree (all control channels are on the licence), 1 partial (site 30: 858.4625 is on the neighbouring Sobieski/Grass licences), 1 has no callsign in RR (site 32 "Three Sisters", also with placeholder coordinates). That agreement is why I treat the PSERN channel lists as solid.

## 3. PSERN (Puget Sound Emergency Radio Network)

| | |
|---|---|
| Protocol | APCO P25 **Phase II** TDMA, Motorola, simulcast cells; Phase I FDMA talkgroups also present (RR mode `D`: 15 of 380) |
| Band | 800 MHz. Base TX 851-861 MHz, mobile TX 806-816 MHz (45 MHz split verified in ULS pairs, e.g. 851.3625 <-> 806.3625) |
| Identifiers | WACN `BEE00`, System ID `3AB` (hex), RFSS 1, per-site NAC `3A0`-`3A9` (RR) |
| Operator | PSERN Operator; King County lead agency, partners City of Seattle, EPSCA, Valley Communications |
| Licences | 12 callsigns covering 9 of the 10 RR site entries, licensee "Puget Sound Emergency Radio Network", service YE, grants 2018-2021, expiring 2028-2031 (WRAW971, WRBM426, WRCJ800, WRAT815, WRER230, WRER496, WRFQ558, WRHV785, WRED916, WRES253, WRBN634, WRKN407) |
| Status | In service 2023; KCERCS analog decommissioned (RR system notes, updated 2026-10-18). `psern.org/psern-faq/` states 800 MHz P25 Phase II simulcast, 40 new radio sites |
| Sources | RR sid 11628 (retrieved 2026-10-19), ULS (2026-10-18), psern.org FAQ |

### 3.1 Sites, control channels and channel spread

| RR site | Name | RFSS | NAC | RR control-channel candidates (MHz) | Licensed DL ch (ULS) | DL range (MHz) | span | nearest ULS tx point to downtown | max voice paths* |
|---|---|---|---|---|---|---|---|---|---|
| 025 (0x19) | Core (Seattle/Eastside) Simulcast | 1 | 3A5 | 851.8875, 852.8625, 853.825, 854.2875 | 16 | 851.1375-854.4125 | 3.27 | Bellevue 10.5 km | 30 |
| 026 (0x1A) | Northeast King (King Lake, Duvall, Tiger Mtn E) Simulcast | 1 | 3A6 | 851.9875, 852.4375, 853.425, 854.6375 | 16 | 851.0625-854.6375 | 3.58 | Monroe 38.3 km | 30 |
| 027 (0x1B) | Seattle Perimeter (E Madison, 193rd & Fremont, Indianola, West Seattle) Sim | 1 | 3A7 | 851.5375, 852.7875, 853.675, 854.4875 | 16 | 851.3625-859.3125 | 7.95 | Seattle 2.0 km | 30 |
| 028 (0x1C) | South King (CJTC, Renton, Vashon) Simulcast | 1 | 3A8 | 851.4625, 851.8125, 852.8125, 853.175 | 16 | 851.1625-859.9125 | 8.75 | Seattle 14.1 km | 30 |
| 029 (0x1D) | Sobieski Mtn / Skykomish Simulcast | 1 | 3A9 | 858.7125 | 6 | 854.0875-858.7125 | 4.62 | Skykomish 75.1 km | 10 |
| 030 (0x1E) | Dodge Ridge / North Bend Simulcast | 1 | 3A0 | 858.4625, 859.4625 | 6 | 856.1375-859.4625 | 3.33 | North Bend 55.8 km | 12 |
| 031 (0x1F) | Grass Mtn (Enumclaw) SImulcast | 1 | 3A1 | 858.7125 | 6 | 854.0875-858.7125 | 4.62 | Enumclaw 60.3 km | 10 |
| 032 (0x20) | Three Sisters Simulcast | 1 | 3A2 | 855.2125, 856.2375, 858.2375, 859.7125 | n/a (no ULS call in RR) | 855.2125-859.7125 | 4.50 | n/a | >= 6 |
| 033 (0x21) | McDonald Mtn (Kangley) | 1 | 3A3 | 857.4375, 858.8875, 859.4375, 860.7125 | 16 | 855.7125-860.7125 | 5.00 | Kangley 46.6 km | 30 |
| 034 (0x22) | Mercer Island Transit Tunnel Simulcast | 1 | 3A4 | 860.9625 | 16 | 852.7625-860.9625 | 8.20 | Mercer Island 6.4 km | 30 |

\* Design maximum = (licensed downlink channels - 1 control) x 2 TDMA slots. PSERN publishes no occupancy statistics; typical simultaneous calls are **[unverified]** and should be measured by the tracker.

Notes:

- **Control channels.** RR flags 4 candidates per large cell (all four are on the licence). RR does not say which is primary; P25 cells with several candidates normally run one control channel and use the others for voice or as alternates. **[unverified which is primary]** Probe all four; they sit within 1-3 MHz of each other.
- **Which site hears Seattle.** From ULS tower coordinates: site 27 has transmit points at E Madison (47.6158, -122.3089; 2.0 km from downtown), Seattle (47.6317, -122.3508), N Seattle (47.6912, -122.3174), West Seattle (47.5405, -122.3783) and Indianola (Kitsap); site 25 "Core" is Eastside (Bellevue, Issaquah, Redmond, Bothell, Duvall); site 28 covers Seattle South/Renton/Vashon/Port Orchard; site 34 is Mercer Island, "transit tunnel" (47.5908, -122.2396 and 47.5902, -122.2502). In a simulcast cell the same RF is sent from all of a site's towers, so a receiver anywhere in the cell sees the cell's channel list.
- **Channel list (full).** The complete licensed downlink list per site is in the JSON (`sites[].uls.downlink_mhz`). Example, site 27 (16 channels): 851.3625, 851.4125, 851.5375, 852.7875, 853.475, 853.675, 853.875, 854.4875, 854.8375, 854.9875, 855.3875, 856.0125, 857.3125, 858.3125, 858.8125, 859.3125. Site 25 (16 channels): 851.1375-854.4125 only.
- **Band-plan / channel identifier.** The P25 identifier table (base frequency, spacing, bandwidth, transmit offset) is a premium RR field and was **not obtained**. Observable facts: all channels are on a 12.5 kHz raster with the mobile transmit exactly 45 MHz lower; Phase II uses 2 slots per 12.5 kHz channel. The tracker should read the identifier tables from the control channel (IDEN_UP / IDEN_UP_VU TSBKs) rather than hard-code them. **[unverified: exact identifier values]**
- **Modulation of the control channel.** RR leaves "modulation" blank for PSERN sites. Simulcast cells generally need the CQPSK/LSM demodulator rather than C4FM (SDRTrunk user reports, RR forum snippet via search, not directly readable) **[unverified for PSERN]**.
- Three small sites (29 Sobieski/Skykomish, 30 Dodge Ridge/North Bend, 31 Grass Mtn/Enumclaw) have only 6 licensed channels and are 55-95 km from downtown; they are only relevant for comparison.

### 3.2 Talkgroups and encryption

RR lists **380 talkgroups**: 237 flagged clear, 137 flagged fully encrypted, 6 flagged partially encrypted (some traffic clear). By category:

| Category (RR) | TGs | clear | encrypted | partial |
|---|---|---|---|---|
| Seattle Police | 57 | 36 | 21 | 0 |
| King County | 55 | 36 | 19 | 0 |
| Seattle Fire | 39 | 12 | 21 | 6 |
| NORCOM | 33 | 14 | 19 | 0 |
| Sound Transit | 30 | 29 | 1 | 0 |
| King County Sheriff | 27 | 15 | 12 | 0 |
| Hospital | 18 | 18 | 0 | 0 |
| Valley Communications | 18 | 9 | 9 | 0 |
| Seattle | 13 | 13 | 0 | 0 |
| Redmond | 11 | 5 | 6 | 0 |
| Bellevue | 10 | 9 | 1 | 0 |
| Radio Technicians | 10 | 10 | 0 | 0 |
| Kent | 7 | 4 | 3 | 0 |
| Seattle Public Schools | 5 | 5 | 0 | 0 |
| Bothell | 4 | 3 | 1 | 0 |
| Federal Way | 4 | 1 | 3 | 0 |
| Snoqualmie | 4 | 0 | 4 | 0 |
| Algona-Pacific | 3 | 1 | 2 | 0 |
| Auburn | 3 | 0 | 3 | 0 |
| Des Moines | 3 | 0 | 3 | 0 |
| Enumclaw | 3 | 3 | 0 | 0 |
| Issaquah | 3 | 1 | 2 | 0 |
| Renton | 3 | 0 | 3 | 0 |
| Shoreline | 3 | 2 | 1 | 0 |
| Tukwila | 3 | 1 | 2 | 0 |
| University Of Washington | 3 | 2 | 1 | 0 |
| Duval | 1 | 1 | 0 | 0 |
| East Side Agencies | 1 | 1 | 0 | 0 |
| Highline | 1 | 1 | 0 | 0 |
| Kirkland | 1 | 1 | 0 | 0 |
| Lake Forest Park | 1 | 1 | 0 | 0 |
| Mercer Island | 1 | 1 | 0 | 0 |
| Woodinville | 1 | 1 | 0 | 0 |
| Businesses | 1 | 1 | 0 | 0 |

Reading it: hospitals, transit (Sound Transit/Metro), city public works, schools and Seattle Police *dispatch* talkgroups are flagged clear; many police and fire tactical/command/data groups are flagged encrypted; Seattle Fire dispatch groups are flagged *partial* (`enc`). Examples (RR): SPD Disp West/North/South/East = TG 203/205/207/209 clear; SFD 1-4 = TG 103/105/107/109 partial; KCSO dispatch groups 1447/1451/1455/1459 clear; Sound Transit groups 3745-3780 clear; of the 30 Sound Transit-category groups only Metro PD Tac (1499) is flagged encrypted. Full list with `dec/hex/alpha/description/encryption` is in the JSON (`psern.talkgroups`). Flags are **[unverified]**, they change (RR notes "Updated encryption settings for 2 talkgroups for Valley Communications" on 2026-10-18). Encrypted talkgroups are out of scope; no decryption is attempted or proposed.

Hospitals on PSERN (RR): 18 groups, e.g. Harborview 3815, UW Medical Center 3835, Swedish Seattle 3833, Virginia Mason 3841, Seattle Children's 3805, Overlake 3821, Valley Medical Center 3839, Hospital Common 3855, HEMS-to-hospital 3863 (all flagged clear).

### 3.3 Legacy King County system (KCERS / KCERCS), status: decommissioned

RR system "Seattle-King County Public Safety (KCERS)" (sid 604, now in the deprecated list): Motorola Type II SmartZone, sysid `0x3D1C`, analog (+ some P25), 15 sites, 399 talkgroups, 851-861 MHz (Seattle simulcast cell 851.1875-856.8375, control candidates 851.4125 / 853.175 / 853.675 / 854.3625). RR last updated 2023-08-04. psern.org: KCERCS was created in the late 1990s with analog technology and replaced by PSERN. Still-active ULS licences (WQGN958, WQGN960, WQGR279, WQGP996, WQGP539 etc., King County / Valley Communications, expiring 2027) mean ULS alone cannot prove it is off the air **[unverified]**. Stray Type II carriers at 851-861 MHz would be easy to spot in the 800 MHz window.

## 4. Other trunked systems receivable in Seattle

### 4.1 P25 systems

| System | Site | NAC | Control (MHz) | Other listed ch | Range (MHz) | span | ch |
|---|---|---|---|---|---|---|---|
| Port Of Seattle | Seattle Simulcast (010) | 67A | 851.6625, 852.3375, 852.3875, 854.4375 | 6 | 851.2625-854.6625 | 3.40 | 10 |
| King County Metro Transit | Simulcast (001) | 1F1 | 774.38125, 774.39375, 774.63125, 774.6437 | 9 | 769.51875-774.6437 | 5.12 | 13 |
| King County Metro Transit | Convention Place (002) | 1F1 | 769.75625, 770.00625, 770.25625 | 0 | 769.75625-770.25625 | 0.50 | 3 |
| WA State Patrol | King Simulcast (001) | 9C0 | 774.03125, 774.28125 | 4 | 769.21875-774.78125 | 5.56 | 6 |
| Washington Department of Transportation | Federal Way (032) | 20 | 772.59375, 772.84375 | 1 | 772.24375-772.84375 | 0.60 | 3 |
| Washington Department of Transportation | Grass Mtn (035) | 023 | 770.41875, 770.91875, 772.01875 | 2 | 770.41875-772.01875 | 1.60 | 5 |
| Washington Department of Transportation | Kent (036) | 024 | 771.09375, 771.64375 | 1 | 771.09375-771.64375 | 0.55 | 3 |
| Washington Department of Transportation | Rattlesnake Mtn (042) | 02A | 770.20625, 770.71875, 771.40625 | 4 | 770.20625-773.38125 | 3.17 | 7 |
| Washington Department of Transportation | Sobieski Mtn (046) | 02E | 770.18125 | 3 | 770.18125-770.94375 | 0.76 | 4 |
| Washington Department of Transportation | SR99 Tunnel (Seattle) (047) | 02F | 770.95625 | 2 | 770.21875-770.95625 | 0.74 | 3 |
| Washington Department of Transportation | Tiger Mountain East (049) | 031 | 769.11875, 772.96875 | 1 | 769.11875-772.96875 | 3.85 | 3 |
| Washington Department of Transportation | King County Simulcast (073) | 049 | 769.35625, 770.04375, 771.03125 | 5 | 769.35625-773.40625 | 4.05 | 8 |
| South Sound 911 | West Seattle (004) | N/A | 773.20625, 774.20625, 774.70625 | 5 | 769.30625-774.70625 | 5.40 | 8 |
| Boeing P25 | Gold Mountain Simulcast (001) | 3A0 | 862.5625, 862.8625, 863.3125, 863.7625 | 1 | 862.2875-863.7625 | 1.48 | 5 |
| Boeing P25 | Renton (004) | 3A0 | 862.3625, 862.5875, 862.8375, 863.0875 | 6 | 856.3375-863.0875 | 6.75 | 10 |
| Boeing P25 | Seattle (005) | 3A0 | 854.0625, 862.5125, 862.8125, 863.2875 | 5 | 854.0625-863.3625 | 9.30 | 9 |
| Boeing P25 | Auburn (007) | 3A0 | 858.4125, 859.3375, 860.3375, 860.4125 | 6 | 856.3375-860.4125 | 4.08 | 10 |
| Boeing P25 | Sea-Tac (018) | 3A0 | 862.3125, 862.7875, 863.0625 | 9 | 862.2875-863.7625 | 1.48 | 12 |

Notes per system:

- **Port of Seattle (P25 Phase II, 800 MHz)**: RR sid 11481, sysid `670`, WACN `BEE00`, NAC `67A`, site 10 (0x0A) "Seattle Simulcast". ULS WPTD846 (Port of Seattle, YE) licenses 10 base channels: 851.2625, 851.3125, 851.5625, 851.6625, 852.3375, 852.3875, 854.2625, 854.3125, 854.4375, 854.6625; ULS fixed points near SeaTac (47.4411, -122.2983), downtown (47.6047, -122.3292), Kent (47.3637, -122.2904) and NW Seattle (47.6561, -122.4119). 8 talkgroups (Port PD dispatch 61001, Port Fire 61102-61104, maintenance), all flagged clear. Legacy Port Type II (sid 618) is deprecated.
- **King County Metro Transit (P25 Phase II, 700 MHz)**: RR sid 7109, sysid `1F9`, NAC `1F1`, two sites (001 "covers the entire Seattle metropolitan area" per RR; 002 Convention Place / DSTT). RR control candidates 774.38125 / 774.39375 / 774.63125 / 774.64375 (RR prints the last as 774.6437). 66 talkgroups, none flagged encrypted (Sound Transit multigroup `M-STMulti` 28064 etc.). ULS licences WQHJ934/WQHJ935 (King County DOT Metro Transit, service SY, expiring 2027): 20 downlink channels 769.24375-774.64375 (7 more than RR lists for site 001) and 20 uplink 799.24375-804.64375 (30 MHz offset); the licences are shared by sites 001 and 002, so the JSON keeps the RR per-site lists and puts the licensed set under `uls`. Site 001 RR coordinates (47.4986, -121.7944) are a reused placeholder **[coordinates unverified]**. Metro also runs **10 UHF DMR conventional channels** (451.5125, 451.6625, 451.7375, 452.0875, 452.1875 base with 456.xxxx / 457.737 / 458.0875 / 458.1875 pairs), ULS WQWW649, granted 2025-09-26, expiring 2035-12-10 (RR King County page: "10 UHF DMR frequencies ... local simplex communications apart from the 700 MHz Phase 2 system").
- **Washington State Patrol (P25 Phase II, 700 MHz)**: RR sid 7971, sysid `9CC`, WACN `BEE00`, RFSS 2, NAC `9C0`. King Simulcast (site 001): control 774.03125, 774.28125; voice 769.21875, 769.46875, 774.53125, 774.78125. 25 talkgroups; District 2 (Seattle metro): TG 59208 Seattle Metro Dispatch, 59209 Seattle/North King, 59210 South King, 59211 King (digital FDMA), 59212 District 2 Area; cars 59201-59203; none flagged encrypted. The ULS licence RR cites (WPTZ781, State of Washington, service SL) lists no frequencies **[RR channels unverified against ULS]**. Tacoma, King Lake and Capitol Peak sites are also in RR.
- **WSDOT (P25 Phase II, 700 MHz)**: RR sid 10705, WACN `C7D5A`, sysid `03D` (RR lists 03D-042 for regional sub-systems), 99 sites statewide, 8 in King County; 153 talkgroups, none flagged encrypted. Site-specific licences (WRAJ790, WRAS338, WRAJ654, WRAJ657, WRAJ362/364/515/667/678, WRAP235) agree with RR fully for 4 of 8 King sites and partially for 2 (Grass Mtn, Rattlesnake Mtn); the SR 99 tunnel site (047, NAC `02F`, control 770.95625) and Sobieski cite only the statewide licence WPTZ781, which lists no frequencies.
- **South Sound 911 (Pierce County, P25 Phase II, 700 MHz)**: RR sid 5480, sysid `1E7`, WACN `BEE00`. Its **"West Seattle" site 004** (control 773.20625 / 774.20625 / 774.70625; 8 channels 769.30625-774.70625) is licensed to Pierce County WA (WQRL631); coordinates (47.5037, -122.3368) from RR. Relevant to West Seattle only. 70 talkgroups (3 flagged encrypted).
- **Boeing P25 (RR "Project 25 Phase II", all 46 talkgroups digital FDMA, 800 MHz)**: RR sid 7665, sysid `3A5`, WACN `BEE00`, NAC `3A0`. King sites: Renton (004), Seattle/Boeing Field (005), Auburn (007), Sea-Tac (018), Gold Mountain (001). Channels 854.0625-863.7625 MHz. ULS (The Boeing Company, YJ/YO) confirms the control and voice channels listed for those sites. **Legacy "Boeing Wide Area" Type II SmartZone** (sid 2270, analog, 70 talkgroups, RR updated 2020-10-22) shares the same 851-861 MHz range; its status is unknown **[unverified]**.
- **Federal Detention Center SeaTac (RR sid 5805)**: Type II, 406.6 / 407.225 control, voice to 410.525 MHz, sysid `D01A`; RR last updated 2020 **[status unverified]**.
- **Not receivable by a 325 MHz+ Pluto**: Justice Integrated Wireless Network (VHF 167-173 MHz P25 Phase II), Puget Sound Energy (MPT-1327, 217.5-218 MHz), City of Kent NXDN (155 MHz). **US DoD 14C** (P25 Phase I, 380-390 MHz) sites are in Kitsap County (Bangor, Bremerton, Manchester, Keyport), 20-50 km west across Puget Sound **[marginal, not detailed]**.
- **Sound Transit and Seattle City Light have no separate trunked system in RR.** Sound Transit uses PSERN (30 talkgroups), Metro's 700 MHz system (`M-STMulti`) and UHF conventional (ULS WQYP700, service IG, 10 channels 451.775-468.725). Seattle City Light is conventional: analog 851-852 MHz pairs (ULS WNNR525/WNNR517, 851.2875 / 851.5875 / 851.8625 / 852.3625, RR "Seattle Light A-D", DPL 032) plus UHF IG licences (e.g. 451.15, 451.45-451.6875 / 456.xxxx pairs); no SCL trunked system or PSERN talkgroup category was found **[PSERN use unverified]**.

### 4.2 DMR / NXDN trunked systems (UHF 450-465 MHz)

RR lists these King County systems (sites shown are those RR places in King County). "none flagged" = RR gives no control-channel flag (Capacity Plus rotates the control channel across its channels).

| System | Protocol | Site (RR) | Control / first-LCN | Other channels | span MHz |
|---|---|---|---|---|---|
| TRBOWEST (WA-OR-ID-CA) | Motorola MOTOTRBO Connect Plus | Tiger Mtn | 454.39375, 454.50625 | 452.35, 452.5, 452.725, 452.8, 460.95, 463.5, 464.15 | 11.80 |
| TRBOWEST (WA-OR-ID-CA) | Motorola MOTOTRBO Connect Plus | Seattle | 454.14375 | 450.15, 461.6, 464.2 | 14.05 |
| TRBOWEST (WA-OR-ID-CA) | Motorola MOTOTRBO Connect Plus | Columbia Tower | 454.01875, 454.20625 | 454.10625, 454.54375, 464.975 | 10.96 |
| Day Wireless (DMR) | DMR Tier III | Seattle 10 | 454.43125 |  | 0.00 |
| Day Wireless (DMR) | DMR Tier III | Tiger Mtn 11 (2.4) | 460.95 | 452.35, 452.5, 452.725, 452.8, 454.39375, 463.5, 464.15 | 11.80 |
| Day Wireless (DMR) | DMR Tier III | Columbia Tower | 454.20625 | 452.3625, 452.725, 454.54375, 461.6, 464.2, 464.975 | 12.61 |
| Day Wireless (DMR) | DMR Tier III | Seattle 78 | 454.09375 |  | 0.00 |
| FleetNet / Silke Communications | NXDN 4800 baud | Tiger Mountain | 461.125 | 461.7, 461.85, 463.225, 463.275 | 2.15 |
| FleetNet / Silke Communications | NXDN 4800 baud | Cougar Mountain | 454.25625 | 462.94375, 463.33125, 463.69375, 464.45625 | 10.20 |
| FleetNet / Silke Communications | NXDN 4800 baud | Columbia | 454.25 | 452.33125, 460.975, 463.31875, 464.04375 | 11.71 |
| Wiztronics Inc | NXDN 4800 baud | Baldi Mtn | 452.50937 | 451.98438, 454.04375, 462.5375, 462.7375 | 10.75 |
| Wiztronics R93 | NXDN 4800 baud | Baldi Mtn (Enumclaw, WA) | 462.7375 | 451.98438, 454.04375 | 10.75 |
| Wiztronics R93 | NXDN 4800 baud | Tiger Mtn | 451.90938 | 451.5375, 452.43437, 452.73438, 454.05625 | 2.52 |
| Amazon Corporate Offices | DMR Tier III | Cricket (Seattle) | 461.4 | 461.7125, 461.9375 | 0.54 |
| Amazon Corporate Offices | DMR Tier III | Nitro (Seattle) | 464.25 | 461.7375, 464.025, 464.3125, 464.5875 | 2.85 |
| Amazon Corporate Offices | DMR Tier III | Oscar (Seattle) | 452.15 | 452.45, 452.8 | 0.65 |
| Amazon Corporate Offices | DMR Tier III | Brazil (Seattle) | 463.45 | 463.775, 463.85, 464.175 | 0.72 |
| Amazon Corporate Offices | DMR Tier III | Blue Shift (Seattle) | 461.225 | 461.35, 461.775 | 0.55 |
| Amazon Corporate Offices | DMR Tier III | Athena (Redmond) | 464.3625 | 464.15, 464.7125 | 0.56 |
| Amazon Corporate Offices | DMR Tier III | Sonic (Bellevue) | 463.85 | 463.2, 463.45, 463.5625 | 0.65 |
| Amazon Corporate Offices | DMR Tier III | Eclipse (Redmond) | 463.725 | 463.5375 | 0.19 |
| Washington State Convention Center | Motorola MOTOTRBO Capacity Plus | Convention Center Main | none flagged (rotating/unknown) | 451.35, 452.2625, 461.3875 | 10.04 |
| Washington State Convention Center | Motorola MOTOTRBO Capacity Plus | Conference Center | none flagged (rotating/unknown) | 461.6375, 461.6875, 461.9375 | 0.30 |
| Washington State Convention Center | Motorola MOTOTRBO Capacity Plus | Freeway Park Garage | none flagged (rotating/unknown) | 464.9625 | 0.00 |
| Bellevue Collection | Motorola MOTOTRBO Capacity Plus | Bellevue Place | 454.45625 | 451.725, 451.825, 463.575, 464.775 | 13.05 |
| Bellevue Collection | Motorola MOTOTRBO Capacity Plus | Lincoln Square | 454.13125 | 451.9375, 452.975, 463.9375, 464.375 | 12.44 |
| Microsoft Headquarters | Motorola MOTOTRBO Capacity Plus | Site 1 | none flagged (rotating/unknown) | 451.475, 451.6, 451.7, 451.95, 452.1, 452.4 | 0.93 |
| Microsoft Headquarters | Motorola MOTOTRBO Capacity Plus | Site 3 | none flagged (rotating/unknown) | 462.225, 462.45, 463.7875, 464.5875 | 2.36 |
| Lumen Field (Seahawks and Sounders) | Motorola MOTOTRBO Capacity Plus | Centurylink Field | none flagged (rotating/unknown) | 451.45, 451.8375, 451.875, 452.125, 452.175, 452.75, 452.975, 461.075 | 9.62 |
| Alaska Airlines (SeaTac Airport) | Motorola MOTOTRBO Capacity Plus | Terminal C | none flagged (rotating/unknown) | 460.7, 461.65, 462.025, 464.2625 | 3.56 |
| Alaska Airlines (SeaTac Airport) | Motorola MOTOTRBO Capacity Plus | Cargo Terminal | none flagged (rotating/unknown) | 451.45, 452.1, 452.675 | 1.23 |
| Delta Airlines (SeaTac) | Motorola MOTOTRBO Capacity Plus | SeaTac | none flagged (rotating/unknown) | 461.7125, 462.075, 463.3375, 463.875, 464.675 | 2.96 |
| Bill & Melinda Gates Foundation | Motorola MOTOTRBO Capacity Plus | Headquarters 5th Ave N | none flagged (rotating/unknown) | 461.8625, 463.5125, 463.9875 | 2.12 |
| Bill & Melinda Gates Foundation | Motorola MOTOTRBO Capacity Plus | Elliott Ave W | none flagged (rotating/unknown) | 461.7125 | 0.00 |
| Doppler Building | Motorola MOTOTRBO Capacity Plus | Site 1 | none flagged (rotating/unknown) | 463.375, 463.8875 | 0.51 |
| Seattle Children's Research Institute | Motorola MOTOTRBO Capacity Plus | Jack R. MacDonald Building | none flagged (rotating/unknown) | 464.3875 | 0.00 |
| Seattle Children's Research Institute | Motorola MOTOTRBO Capacity Plus | Building Cure | none flagged (rotating/unknown) | 452.725, 452.975 | 0.25 |
| Shoreline School District 412 | Motorola MOTOTRBO Capacity Plus | Site 1 | none flagged (rotating/unknown) | 461.8125, 462.2, 463.3625, 463.4625, 463.975, 464.2375 | 2.42 |
| Shoreline School District 412 | Motorola MOTOTRBO Capacity Plus | Site 2 | none flagged (rotating/unknown) | 461.875, 462.1375, 462.275, 462.375, 463.575, 464.7875 | 2.91 |
| Mercer Island School District | Motorola MOTOTRBO Capacity Plus | Site 1 | none flagged (rotating/unknown) | 463.25, 463.375 | 0.12 |
| Providence Mount Saint Vincent | Motorola MOTOTRBO Capacity Plus | Site 1 | none flagged (rotating/unknown) | 452.05, 461.9625 | 9.91 |
| Muckleshoot Casino | Motorola MOTOTRBO Capacity Plus | Muclkleshoot | none flagged (rotating/unknown) | 464.2625, 464.2625, 464.3125, 464.3125, 464.4375, 464.4375, 464.8625, 464.8625 | 0.60 |
| Snoqualmie Casino (Snoqualmie Entertainment) | Motorola MOTOTRBO Capacity Plus | Snoqualmie | none flagged (rotating/unknown) | 461.4875, 461.6875, 463.2625, 463.4375, 463.6125 | 2.12 |

Protocol notes (all from the RR system type): TRBOWEST is **Connect Plus** (system id 119 / 0x77, dedicated control channel; the RR note on Day Wireless calls the new Capacity Max system a possible replacement), Day Wireless "Capacity Max" is **DMR Tier III** (system id `H0`, restricted access in use), FleetNet/Silke and both Wiztronics systems are **NXDN 4800 (6.25 kHz) Type-C trunking** (ids 80/0x50, 2029/0x7ED, 93/0x5D), Amazon is **DMR Tier III "non-standard"** (system `L1`). Everything else is **Motorola Capacity Plus**. Talkgroups for these private/commercial systems are not published (RR lists none), and restricted access is in use on at least Day Wireless **[unverified]**. Also in RR: Pacific NorthWest DMR (amateur 70 cm, 440-441 MHz) and Providence Medical Centers (conventional networked DMR, 452.350).

Pluto-relevant geometry: downtown sites combine 454.0-454.6 MHz control channels with 450-465 MHz voice channels, a **~11-14 MHz** single-site spread; the largest RR-listed single-site spread in King County is 14.05 MHz (TRBOWEST "Seattle", 450.15-464.2).

### 4.3 Other trunk-feeder bands worth knowing

From ULS (see `seattle-uls-licenses.json`): 99 licences in 800 MHz within 40 km (Valley Communications Center, PSERN, Seattle, Boeing, King County, Snohomish County 911, Kitsap 911, Tacoma, Port of Seattle, State DOC, ...), 26 in 700 MHz (Pierce County WA, Metro, WSDOT, one PSERN `SG` licence with 4 frequencies at 769.1-770.6 MHz, Snohomish 911), 29 in 900 MHz (PDV Spectrum Holding [Anterix] 935.1375-939.95; Snohomish PUD; Harborview Medical Center 935.1375-939.65). RR shows Harborview on **936-937 MHz DMR** (WPPW453: security 937.4625, engineering ops 936.725, flagged "encrypted" for engineering and field units) **[unverified]**, which sits well inside the Pluto range.

## 5. Conventional activity, 450-470 MHz (and neighbours)

ULS shows **1,471 active licences with 7,284 frequency assignments** in 450-470 MHz with fixed coordinates within 40 km of downtown Seattle (county-only mobile locations are excluded, so this undercounts). Distinct licences with at least one assignment of a given type (heuristic from emission designators): analog FM ~1,060 (plus ~350 analog+data), DMR ~735, NXDN 6.25 kHz ~90, NXDN 12.5 kHz ~55, P25 Phase I ~10. Services (frequency assignments): 1,192 IG (Industrial/Business conventional), 116 YG (I/B trunked pool), 156 PW (Public Safety pool), 7 other.

### 5.1 Who holds base/repeater channels (distinct base/fixed frequencies, ULS, 40 km)

Silke Communications Solutions (FleetNet) 70; The Boeing Company 51; University of Washington 42 (+9 more under "University of Washington"); Alaska Airlines 36 (+12); Day Wireless Systems 27; Microsoft 25 (+18); Kitsap 911 24; Muckleshoot Casino Resort 17; SSA Terminals 16; Washington State Convention Center 16; T-Mobile 15; Industrial Communications Inc 15; Valley Medical Center 15; Amazon.com Services 15; City of Seattle 15; Kemper Development 13; Federal Way SD 13; Kent SD 13; Google 12; Swedish Medical Center 12; Seattle Mariners 12; Shoreline SD 12. Many are DMR (7K60FX*) conventional or Capacity Plus feeder licences.

### 5.2 Public-safety pool (PW) UHF 453/458 and 460/465 MHz

City of Seattle holds 33 PW frequency assignments (base and mobile; 453.2125, 453.4625, 453.6, 453.7, 453.7125-453.8625 with 458.xxxx mobile pairs; 458.8375 / 458.85 / 458.8625 at many sites) and King County 11 (453.1, 453.15, 453.825 ...), Port of Seattle 7 (453.2-453.5 / 458.2-458.5), City of Bellevue, Kent, Redmond, Renton, Mercer Island, Tukwila, Port of Tacoma, Kitsap 911 (460.075-460.4 MHz), Harborview Medical Center (463.0-463.075), Stevens Memorial Hospital, WSP (453.7375 / 465.0375). These are the channels typically used for investigative/tactical or utility operations; modes are mostly analog FM (11K2F3E) with some DMR/NXDN **[operational use unverified; encryption unknown]**.

### 5.3 GMRS / FRS (462 and 467 MHz)

47 CFR 95.1763 (retrieved 2026-10-19): GMRS main channels 462.5500 ... 462.7250 (8, 25 kHz steps) and repeater inputs 467.5500 ... 467.7250; interstitial 462.5625 ... 462.7125 and 467.5625 ... 467.7125 (handheld only). RR "King County GMRS" lists repeaters at 462.550, 462.575, 462.625, 462.675 (PL 103.5 / 103.5 / 100.0 / 141.3, Seattle Hubs Repeater Group) and 462.700 (AAECT). GMRS individual licences were not downloaded (privacy and no tracking value).

### 5.4 T-band 470-512 MHz: not in use in Seattle

47 CFR 90.303 (eCFR, retrieved 2026-10-19) lists the urban areas where 470-512 MHz is available for land mobile: Boston, Chicago, Cleveland, Dallas/Fort Worth, Detroit, Houston, Los Angeles, Miami, New York, Philadelphia, Pittsburgh, San Francisco/Oakland, Washington DC. **Seattle is not listed.** ULS agrees: ~100,000 active frequency-location records in 470-512 MHz nationally, **0 within 60 km of downtown Seattle**. The band in Seattle is TV (UHF channels 14-20).

## 6. Reconciling the existing 454 MHz capture with licences

Capture: `recordings/baseband_454216998Hz_wide-30s_20261019T215324Z.wav` (LO 454.216998 MHz, 6 Msps), inventory `output/wideband-decodes/inventory30.json` (15 carriers; the two entries with no `survey` block are 454.117 and 454.5545 MHz).

### 6.1 LO error [inferred]

All 15 carriers sit within +-0.41 kHz of the 6.25 kHz channel raster after a common shift, which fixes the shift only modulo 6.25 kHz. Of the raster-equivalent shifts, only **actual = nominal - 10.556 kHz (about 23.2 ppm low LO)** matches RR-listed UHF trunked channels: 13 of 15 carriers match an RR-listed channel (4 match RR *control* channels) versus 1-4 for the other shifts and a mean of ~3 for random shifts. This is inference from channel-plan agreement, not a measurement against a reference; the conclusion is moderate-high confidence. Recommend verifying with a known carrier (e.g. a PSERN control channel) and then calibrating the AD9363 reference.

### 6.2 Result

| # | Nominal (as measured) | Corrected (-10.556 kHz) | 6.25k grid / residual | Occupied width | DSD-DMR audio emitted (s) slot1/slot2 | RadioReference match (system, site, role) | FCC ULS candidates (licensee, callsign; nearest to downtown) |
|---|---|---|---|---|---|---|---|
| 1 | 451.3603 | 451.34972 | 451.35 / -278 Hz | 8789 Hz | 0.7/0.38 | Washington State Convention Center / Convention Center Main (voice/other); Day Wireless (DMR) / Arlington (voice/other) | Washington State Convention Center WQJB646 (FB2, 1 km); Are - Seattle No 10 Llc WRUW766 (FB2, 2 km); Zefram, Llc WRAP332 (MO, 7 km); +7 more |
| 2 | 451.7731 | 451.76253 | 451.7625 / +33 Hz | 2380 Hz | 0.7/0.38 | none | Alexis Hotel WQRX676 (MO, 0 km); 1918 Eighth Ave Acquistion, Llc WQJW302 (MO, 1 km); Seattle Tower 1 Llc WRDX680 (MO, 1 km); +43 more |
| 3 | 452.1606 | 452.15008 | 452.15 / +76 Hz | 8423 Hz | 1.38/0.42 | Amazon Corporate Offices / Oscar (Seattle) (control) | Amazon.Com Services Llc WRFI741 (FB2, 1 km); Seattle Mariners WQGY698 (MO, 2 km); Southwest Suburban Sewer District WQON644 (FB2, 10 km); +7 more |
| 4 | 452.2731 | 452.26250 | 452.2625 / +3 Hz | 8423 Hz | 1.08/0.0 | Washington State Convention Center / Convention Center Main (voice/other) | Washington State Convention Center WQJB646 (FB2, 1 km); The Summit At First Hill WQLQ632 (FB2, 1 km); Sap Labs WQDE347 (FB2, 10 km); +2 more |
| 5 | 452.3418 | 452.33126 | 452.33125 / +9 Hz | 4944 Hz | 0.34/0.36 | FleetNet / Silke Communications / Columbia (voice/other) | Silke Communications, Inc. WQYY872 (FB6, 0 km) |
| 6 | 452.3608 | 452.35021 | 452.35 / +210 Hz | 8423 Hz | 0.34/0.02 | Day Wireless (DMR) / Tiger Mtn 11 (2.4) (voice/other); TRBOWEST (WA-OR-ID-CA) / Tiger Mtn (voice/other); Providence Medical Centers / Providence Renton (voice/other) | Day Management Corp. Dba Day Wirel WNXY421 (FB6, 0 km); Seattle University WQWL744 (FB2, 1 km); Bristol Myers Squibb Company WRTC525 (MO, 2 km); +8 more |
| 7 | 452.6855 | 452.67495 | 452.675 / -52 Hz | 6409 Hz | 6.46/0.74 | Alaska Airlines (SeaTac Airport) / Cargo Terminal (voice/other) | Dbia Services Inc Dba Metropolitan WPQB331 (FB2, 1 km); Amazon.Com - Swa2 WRDU774 (MO, 6 km); T-Mobile License Llc WRER954 (MO, 13 km); +6 more |
| 8 | 452.7354 | 452.72484 | 452.725 / -156 Hz | 5127 Hz | 0.7/0.74 | Day Wireless (DMR) / Tiger Mtn 11 (2.4) (voice/other); Day Wireless (DMR) / Columbia Tower (voice/other); Seattle Children's Research Institute / Building Cure (voice/other); TRBOWEST (WA-OR-ID-CA) / Tiger Mtn (voice/other) | Day Management Corp. Dba Day Wirel WNXY421 (FB6, 0 km); Pike Place Market Preservation And WQWP426 (FB2, 1 km); Hudson First & King Llc WQTY781 (FB2, 1 km); +6 more |
| 9 | 452.9855 | 452.97497 | 452.975 / -34 Hz | 5859 Hz | 1.74/0.78 | Seattle Children's Research Institute / Building Cure (voice/other); Bellevue Collection / Lincoln Square (voice/other); Lumen Field (Seahawks and Sounders) / Centurylink Field (voice/other) | Washington State Public Facilities WRZE763 (FB2, 1 km); Seattle Childrens WRDT357 (FB2, 1 km); Safeco Insurance Company KSM643 (FB2, 6 km); +3 more |
| 10 | 454.0292 | 454.01867 | 454.01875 / -83 Hz | 9521 Hz | 0.72/12.96 | TRBOWEST (WA-OR-ID-CA) / Columbia Tower (control) | none (no Part 90 licence on this channel in LMpriv/LMcomm/paging) |
| 11 | 454.1170 | 454.10644 | 454.10625 / +192 Hz | n/a Hz | 1.04/0.38 | TRBOWEST (WA-OR-ID-CA) / Columbia Tower (voice/other) | none (no Part 90 licence on this channel in LMpriv/LMcomm/paging) |
| 12 | 454.2170 | 454.20644 | 454.20625 / +192 Hz | 10803 Hz | 0.0/0.0 | Day Wireless (DMR) / Columbia Tower (control); TRBOWEST (WA-OR-ID-CA) / Columbia Tower (control) | none (no Part 90 licence on this channel in LMpriv/LMcomm/paging) |
| 13 | 454.2607 | 454.25011 | 454.25 / +113 Hz | 4761 Hz | 0.34/0.0 | FleetNet / Silke Communications / Columbia (control) | none (no Part 90 licence on this channel in LMpriv/LMcomm/paging) |
| 14 | 454.3476 | 454.33709 | 454.3375 / -412 Hz | 2930 Hz | 1.04/0.02 | none | none (no Part 90 licence on this channel in LMpriv/LMcomm/paging) |
| 15 | 454.5545 | 454.54394 | 454.54375 / +192 Hz | n/a Hz | 1.06/0.38 | Day Wireless (DMR) / Columbia Tower (voice/other); TRBOWEST (WA-OR-ID-CA) / Columbia Tower (voice/other) | none (no Part 90 licence on this channel in LMpriv/LMcomm/paging) |

What this says:

- **454.01875 / 454.20625 / 454.10625 / 454.54375** are Columbia Tower (downtown Seattle) channels of TRBOWEST Connect Plus (dedicated control 454.01875 and 454.20625) and Day Wireless Capacity Max (control 454.20625). Your "DMR at 454.117 and 454.217" is, after correction, **454.10625 (voice/data) and 454.20625 (control)**. The ULS callsign RR attaches to the Columbia Tower site (WNXY421) belongs to Day Management Corp. dba Day Wireless Systems; that licence lists 451-452 / 456-457 / 461-469 MHz channels but not the 454 MHz ones.
- **454.25** is the FleetNet / Silke Communications NXDN control channel at Columbia (RR; ULS licensee of the site, Silke Communications, Inc., WQYY872, is licensed for the voice channels but not 454.25).
- **452.33125** is Silke/FleetNet NXDN Columbia voice (licence WQYY872, class FB6 at Seattle); **452.725** is Day Wireless Columbia Tower voice (licence WNXY421); **452.35** is Tiger Mtn voice for Day Wireless and TRBOWEST (licence WNXY421 includes 452.35 at a Seattle FB6 location; RR lists Columbia Tower voice as 452.3625, so the Tiger Mtn attribution is the safer one).
- **452.15** is the Amazon Tier III "Oscar" control channel (Amazon.com Services LLC, WRFI741; ULS licence includes 452.15, 452.45, 452.8).
- **451.35 and 452.2625** are Washington State Convention Center Capacity Plus channels (licence WQJB646, Washington State Convention Center); **452.675** is the Metropolitan Improvement District's DMR channel (WPQB331; RR lists CC4 TG 11 / TG 12 street ambassadors and sweepers); **452.975** matches Capacity Plus voice at Lumen Field, Seattle Children's and Bellevue Collection Lincoln Square (also Washington State Convention Center Public Facilities District WRZE763).
- **451.7625 and 454.3375 have no RR match in this survey.** 451.7625 is licensed to 46 different licensees within 40 km (many downtown hotels/buildings, IG, mostly mobile class, 12.5 kHz) so it is probably a shared conventional business channel. Later IQ analysis identifies **454.3375 as NXDN Type-D signalling**, with five independently CRC-valid SCCH frames; its operator remains unknown. See [capture findings](iq-analysis.md).
- **Control channels at 454.0-454.65 MHz are not in the Part 90 licence files.** For this band ULS (LMpriv + LMcomm + paging) shows only paging/radiotelephone licences in the Seattle area (Metrocall/USA Mobility 454.05, 454.2, 454.25, 454.35; Arch 454.15, 454.325; SkyTel 454.675). The licensing basis for the Day Wireless / Silke control channels is not visible in these files **[unverified]**; RR attaches ULS callsigns (WNXY421, WQYY872, WQWP279) that licence the *voice* channels but not the 454 MHz ones. Do not assume the paging licences are the same service as the DMR/NXDN control channels.
- The "audio emitted" columns are DSDcc output counts from `inventory30.json` and, per that file's own notes, do not establish speech or encryption status.

### 6.3 Other nearby UHF transmitters Cory could see from the same window

Not in the capture but in RR/ULS inside 451-465 MHz: Metro's 10 DMR channels (above), UW DMR 452.075 / 452.300 / 463.200 / 464.8125 (RR, "DMRE", callsigns WQOA955 and WQOA942; ULS: 42 base channels for Washington State dba University of Washington), Seattle University DMR 461.825 (WPCG298), Overlake Hospital NXDN 452.150 / 464.525 (16 RAN), T-Mobile Park conventional 461.4125-463.675 (WQGY698), Sound Transit UHF, Seattle City Light UHF.

## 7. Coverage sizing

### 7.1 Windows (one Pluto, 20 MHz instantaneous)

| Window | Channels span (MHz) | Min window (MHz) | Suggested centre (MHz) | Fits 20 MHz | Members |
|---|---|---|---|---|---|
| 700 MHz public-safety downlink block (P25 Phase II: Metro, WSP, WSDOT, South Sound 911 West Seattle) | 769.11875 - 774.78125 (5.66) | 5.67 | 771.95 | yes | kc_metro_p25, wsp_p25, wsdot_p25, south_sound_911_p25 |
| 800 MHz block (PSERN + Port of Seattle P25 + Boeing P25/Type II) | 851.0625 - 863.7625 (12.70) | 12.71 | 857.4125 | yes | psern, port_of_seattle_p25, boeing_p25, boeing_wide_area_type2 |
| PSERN only (all 10 RR site entries) | 851.0625 - 860.9625 (9.90) | 9.91 | 856.0125 | yes | psern |
| UHF 450-465 MHz trunked business/institutional systems (DMR Cap+/Connect+/Tier III, NXDN) at King County sites per RR | 450.15 - 464.975 (14.82) | 14.84 | 457.5625 | yes | trbowest, day_wireless_capmax, fleetnet_nxdn, wiztronics_nxdn, wiztronics_r93_nxdn... |

- **Fits one window, no sub-band splitting needed:** every P25 system (one window per band), PSERN alone (9.9 MHz), and the UHF trunked set at 450-465 MHz (14.8 MHz, tight but inside 20 MHz; the two edge channels need filter margin, so a conservative usable width is ~16 MHz).
- **Cannot share a window:** 700 MHz (769-775), 800 MHz (851-864) and UHF (450-465) are separated by 80-390 MHz, so a single Pluto has to retune between bands. A single window covers all of PSERN plus the Port, Boeing and legacy Type II (and SCL conventional at 851-852 MHz). To follow, say, PSERN and Metro at the same time you need two radios or time-slicing.
- **Per site (see the tables in section 3-4):** PSERN's biggest single-site span is South King, 8.75 MHz (851.1625-859.9125), then Mercer Island Tunnel 8.2 MHz, then Seattle Perimeter 7.95 MHz; Core and NE King are only 3.3-3.6 MHz. Boeing Seattle is 9.3 MHz. UHF sites are 10-14 MHz where the 454 control channel is paired with 461-465 MHz voice.
- **Placement tip:** keep control channels at least ~100 kHz from the DC bin and from any known image/LO spur; with PSERN center 856.0125 MHz the four Seattle-relevant control candidates (851.5-854.5) are 1.5-4.5 MHz below center.

**LO error warning (see 6.1).** At 454 MHz the observed LO error was ~10.6 kHz; at 856 MHz the same 23 ppm is ~19.8 kHz, larger than a P25 channel's pass-band. The tracker needs a frequency search or AFC on the control channel (and ideally a stored ppm correction).

### 7.2 Simultaneous voice calls

Design maxima, `(channels - 1 control) x slots` (P25 Phase II and DMR: 2 slots per channel; NXDN 4800: 1):

- PSERN: 30 at each full 16-channel site (25, 26, 27, 28, 33, 34); 10-12 at the 6-channel outlying sites; 206 across all sites combined is **not** meaningful (different sites).
- Port of Seattle: 10 channels -> 18. Metro site 001: 13 RR-listed channels -> 24 (more if the 18 licensed channels are all in service). WSP King: 6 -> 10. WSDOT King sites: 3-8 channels -> 4-14. Boeing P25 sites: 5-12 channels -> 8-22. South Sound 911 West Seattle: 8 -> 14.
- UHF: FleetNet Columbia (NXDN) 5 channels -> 4; TRBOWEST Columbia Tower 5 channels -> 8; Amazon sites 3-5 channels -> 4-8; Capacity Plus campus systems 1-8 channels -> 0-14.
- **Typical (observed) concurrency is not published for any of these systems [unverified].** Assume the tracker must handle bursts to the design maximum on PSERN site 27 during major incidents/events; measure with a few hours of control-channel logging before sizing the decoder pool.

## 8. Implications for the tracker (suggestions, not requirements)

1. **Windows to build first:** (a) 851-864 MHz centre ~857.4 (PSERN + Port + Boeing); (b) 769-775 MHz centre ~771.95 (Metro + WSP + WSDOT + South Sound 911); (c) UHF 450-465 centre ~457.56 for DMR/NXDN/Connect Plus.
2. **P25 acquisition:** probe each RR control candidate for NAC; compare with the site table; read RFSS/Site and IDEN_UP to get the channel plan; expect simulcast (CQPSK/LSM) on PSERN, WSP and Metro **[unverified modulation]**.
3. **Frequency error:** see sections 6.1 and 7.1.
4. **Encryption:** use the ALGID in the encryption sync (HDU/LDU2 on Phase I, ESS on Phase II) as truth; treat the `encryption` field as a hint. Do not log or attempt to defeat encrypted traffic.
5. **UHF trunk:** Connect Plus has a dedicated control channel (TRBOWEST Columbia: 454.01875, 454.20625); Capacity Plus and Tier III may rotate or share control slots; Tier III here uses restricted access, so decoding depends on what the control channel exposes **[unverified]**.

## 9. Legal notes (Washington; not legal advice)

- **Federal.** 18 USC 2511(2)(g)(ii) makes it lawful to intercept radio communications transmitted by "any governmental, law enforcement, civil defense, private land mobile, or public safety communications system, including police and fire, readily accessible to the general public" and by stations in the amateur, CB and GMRS services. 18 USC 2510(16) defines "readily accessible to the general public" as *not* scrambled or encrypted, not using modulation whose essential parameters are withheld for privacy, not on a subcarrier, and not carried on a system provided by a common carrier (except tone-only paging). So: **clear** public-safety and private land-mobile traffic is fine to receive; encrypted traffic is outside the exception. Possible common-carrier carve-out for commercial service providers such as Day Wireless/FleetNet subscriber traffic is a point to ask counsel about **[unverified]**.
- **47 USC 605(a)** forbids divulging or using the contents of intercepted radio communications for one's own benefit, and divulging/publishing them to others, unless they are broadcast, distress, amateur/CB, etc. Receiving is not the problem; republishing or acting on content can be. The tracker should keep decoded content local and not rebroadcast it.
- **Washington.** RCW 9.73.030(1)(a) makes it unlawful to intercept or record any "private communication transmitted by telephone, telegraph, radio, or other device between two or more individuals ... without first obtaining the consent of all the participants." The statute applies to *private* communications; I did not find a Washington statute that specifically addresses police scanners or scanners in vehicles, and the web search for one did not return an authoritative source. Whether dispatch radio is a "private communication" is a question of case law (recalled, not re-verified: *State v. Faford*, 128 Wn.2d 476 (1996), cordless-phone calls were private). **Prudent practice: do not record or retain two-party radio traffic beyond what the receiver needs, do not decrypt, do not publish.** RCW 9.73.030(3) lets a party announce recording; that does not help a passive listener.
- **No decryption:** the project should not attempt to defeat encryption (federal wiretap law and WA statute would then apply to the content), and I make no suggestion that it do so.

## 10. Open items / unverified

- PSERN: primary vs alternate control channel per site; control-channel modulation; P25 channel identifier values; occupancy.
- PSERN site 32 "Three Sisters" (no callsign, placeholder coordinates) and site 30's 858.4625.
- Coordinates for Metro, Boeing "Composites", WSP King and TRBOWEST Columbia Tower in RR reuse (47.4986, -121.7944) or a Tiger Mountain point: marked `coords_suspect`.
- Which UHF DMR/NXDN control channels on 454.0-454.65 MHz are licensed and under what service.
- Carrier 451.7625 and 454.3375 in the capture.
- Whether any KCERS carrier is still radiating.
- Encryption flags (RR crowd-sourced) and the legal questions in section 9.
- RR forums (PSERN live thread, SDRTrunk threads) could not be read (503); hobbyist-site claims are from search snippets only.
- GMRS licences, 900 MHz Anterix usage, Seattle `SG` 700 MHz low-power licences (WSJL882, WSKW895) not investigated.

## 11. Sources (all retrieved 2026-10-19 unless noted)

- RadioReference: PSERN system https://www.radioreference.com/db/sid/11628 ; site pages https://www.radioreference.com/db/site/39040 ... 39045, 40569, 42406, 47120, 47121 ; King County https://www.radioreference.com/db/browse/ctid/2974 ; trunked list https://www.radioreference.com/db/browse/ctid/2974/trs ; deprecated https://www.radioreference.com/db/browse/ctid/2974/trs?deprecated=1 ; Metro https://www.radioreference.com/db/sid/7109 ; WSP https://www.radioreference.com/db/sid/7971 ; WSDOT https://www.radioreference.com/db/sid/10705 ; Port https://www.radioreference.com/db/sid/11481 ; Boeing https://www.radioreference.com/db/sid/7665 and /2270 ; South Sound 911 https://www.radioreference.com/db/sid/5480 ; TRBOWEST https://www.radioreference.com/db/sid/7571 ; Day Wireless https://www.radioreference.com/db/sid/11851 ; FleetNet https://www.radioreference.com/db/sid/9923 ; Wiztronics https://www.radioreference.com/db/sid/8621 and /13414 ; Amazon https://www.radioreference.com/db/sid/13473 ; KCERS https://www.radioreference.com/db/sid/604 ; conventional lists https://www.radioreference.com/db/aid/5941 , /7629 , /11106 ; wiki https://wiki.radioreference.com/index.php/Puget_Sound_Emergency_Radio_Network_(PSERN)
- PSERN FAQ: https://psern.org/psern-faq/
- FCC ULS bulk files (dated 2026-10-18): https://data.fcc.gov/download/pub/uls/complete/l_LMpriv.zip , l_LMcomm.zip , l_paging.zip
- 47 CFR 90.303: https://www.ecfr.gov/current/title-47/section-90.303 ; 47 CFR 95.1763: https://www.ecfr.gov/current/title-47/section-95.1763
- RCW 9.73.030: https://app.leg.wa.gov/RCW/default.aspx?cite=9.73.030
- 18 USC 2510/2511: https://www.law.cornell.edu/uscode/text/18/2510 , /2511 ; 47 USC 605: https://www.law.cornell.edu/uscode/text/47/605
- Local: `output/wideband-decodes/inventory30.json`, `recordings/baseband_454216998Hz_wide-30s_20261019T215324Z.json`

Method (reproducibility): RR pages fetched with curl and parsed (site cards, talkgroup tables); ULS `.dat` files loaded into SQLite and joined HD+EN+LO+FR+EM; licences filtered to active (`license_status=A`), Washington, within 40 km (great-circle) of 47.6062, -122.3321; the build scripts live outside the repo (scratch) and are described here rather than committed, because they depend on ~500 MB of downloads.
