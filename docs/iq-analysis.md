# IQ findings and handoff to Refine radio monitoring UX

The captures establish three different kinds of signalling: **454.3375 MHz is NXDN Type-D**, **454.2500 MHz is NXDN Type-C control data**, and **451.3500 MHz is DMR Capacity Plus status signalling**. None of these samples establishes a sustained voice conversation or human-readable text message.

Analysis date: 2026-10-20. This document is the shared handoff to the **Refine radio monitoring UX** chat (`01a15b58-0ff5-7060-a9d3-e6caf84a83a6`), which owns text, data, and other non-audio presentation. That chat has not acknowledged this handoff; direct delivery was blocked by its active writer lock. No frontend changes are included here.

## Verified signals

| Frequency | Identification | Evidence | Limits |
| --- | --- | --- | --- |
| 454.3375 MHz | NXDN4800 / IDAS Type-D distributed trunking | Independently recovered five full NXDN sync words, five valid LICH parity checks, and five valid SCCH CRC7 checks. DSD-FME also decodes repeated Site ID, Service Information, Idle Repeater, and Transmission Release messages. | Operator unknown. These are signalling messages; occasional raw decoder `Voice` labels do not establish a voice call. |
| 454.2500 MHz | NXDN4800 Type-C control data | Follow-up capture has 175 CRC-gated Service Information messages. RAN 42; regional System 80 / Site 42; Location ID `80502A`. | Operator attribution requires a separate catalogue match. |
| 451.3500 MHz | DMR Motorola Capacity Plus | Follow-up has 310 Channel Status messages, Color Code 9, Rest LSN 2, and Site 1 in SLCO. Earlier sample independently yielded 272 CRC-valid CSBKs with opcode `0x3e`, feature ID `0x10`. | Status signalling in these samples; no decoded speech. A frequency listing alone does not verify the operator. |
| 452.2625 MHz | DMR Capacity Plus | Earlier capture has 74 CRC-valid CSBKs matching the 451.3500 status payload. | Archive filename says 452.268750; use metadata and measured carrier. |
| 461.7750 and 464.9250 MHz | DMR Capacity Plus | Repeated status signalling, Color Code 7, Rest LSN 4. | Operator unconfirmed. Existing Amazon catalogue label is not evidence of operator identity. |

The initial 454.206250-labelled slice also contains continuous DMR data with Color Code 15. DSD-FME reports RAS-related CRC bypasses, so parsed fields from that path must not be presented as independently CRC-verified identities.

No clear narrowband signal was found across the saved 100 kHz spans for the 454.543750, 461.606250, and 469.900000 captures. This says what was present in those recordings, not whether those frequencies are generally unused.

## The 450 ms burst

The burst is on nominal **454.3375 MHz**, measured near **454.336927 MHz**. It appears in `iq_454306250Hz.sigmf.tar` because that archive contains a 100 kHz slice, not just the frequency named in its filename.

It occupies roughly 4.2754–4.7257 seconds of stored capture time (450.27 ms). Symbol recovery finds 4FSK at approximately 2,400 symbols/s, with about 613 Hz between adjacent fitted levels. Five complete 20-bit NXDN frame sync words (`0xCDF59`) recur every 192 symbols, or 80 ms. All five frames pass LICH parity and SCCH CRC7, with zero hard-bit corrections in the convolutional decoder:

| Frames | LICH | SCCH payload, including CRC | Meaning |
| --- | --- | --- | --- |
| First four | `0xc6` | `101ff91c` | Site ID signalling |
| Fifth | `0xc2` | `0217fe5d` | Idle repeater signalling |

This is a protocol identification backed by frame checks, not just a spectral resemblance. The earlier decoder path did not correctly interpret Type-D SCCH; the signal is now identified and its signalling is decoded.

## Follow-up capture results

| Archive in `recordings/mac-followup-20261020/` | Duration | Decoder observations |
| --- | --- | --- |
| `iq_454337000Hz.sigmf.tar` | 19.66 s | 24 raw sync reports; 9 CRC-gated Site ID, 4 Service Information, 3 Idle Repeater, 3 Transmission Release messages; 4 CRC failures elsewhere. Metadata target is 454.3375 MHz. |
| `iq_454250000Hz.sigmf.tar` | 15.48 s | 192 raw sync reports; 175 CRC-gated Service Information messages; 1 CRC failure. |
| `iq_451350000Hz.sigmf (2).tar` | 30 s | 335 raw sync reports; 310 Capacity Plus Channel Status messages; 19 CRC errors, including 15 SLCO errors. No voice frames in the full log. |

Raw sync counts include false or unverified detections and are not successful message counts. They exclude `Sync: no sync` diagnostics. CRC failure counts can include unrelated false syncs. Message counts are observations within these samples, not unique sites, radios, or conversations.

For 454.3375, keep the two identifier namespaces separate:

- SCCH Site ID: Area 0, Site Type 16 Narrow, Site Code 3 Open Access.
- FACCH Service Information: Location ID `008001`, global System Code 8 / Site Code 1.

Do not merge these into one contradictory “site” field. They are different protocol fields. No operator name has been established from them.

## What Type-D means

NXDN is the digital radio protocol family. Type-D describes distributed trunking: repeaters share channel management rather than relying on a permanently dedicated control channel. A home channel can carry signalling while idle and carry voice when needed. Type-C uses centralized control and can have a dedicated control channel. See the [NXDN Forum FAQ](https://www.nxdn-forum.com/faq/) and [Icom's description of Type-C and Type-D](https://www.icomamerica.com/case_studies/wplinq2021/).

“Type-D” is therefore a system variant, not a synonym for text, data-only, encrypted, or permanently silent.

## UX handoff

Keep protocol identity, current activity, and decoded content separate. Suggested presentation:

| Situation | Protocol badge | Activity / result |
| --- | --- | --- |
| Verified 454.3375 signalling | `NXDN · Type-D` | `Idle beacon`, `Site information`, or the actual decoded event |
| Verified 454.250 control stream | `NXDN · Type-C` | `Control data`; expandable system/site/RAN fields |
| Verified 451.350 status | `DMR · Capacity Plus` | `Channel status`; Color Code 9, Rest LSN 2 |
| RF detected without a valid frame | Known family only if supported by evidence | `Signal detected; no valid message decoded` |
| Valid protocol frame with unsupported payload | Verified family / variant | `Protocol identified; payload decode unavailable` |
| Actual decoded text | Verified family / variant | Message body, sender/recipient when decoded, timestamp, copy/save |

The non-audio result area should support typed events and packet fields as well as actual text messages. Site ID and channel status are structured machine messages; do not render them as a human chat conversation. Voice activity should require speech evidence. Silence and failed decoding do not establish encryption.

Useful detail fields are frequency, protocol family and variant, message type, capture timestamp, decoded identifiers with their namespaces, and validation status. Put raw payload, sync/FEC/CRC details, and decoder diagnostics behind details. Preserve failed checks as diagnostics without promoting their contents to verified facts.

Current integration points:

- `web/src/components/ChannelIdentity.tsx`: protocol badge currently uses `mode.toUpperCase()`.
- `web/src/components/SourceList.tsx`: talking lamp and recent voice presentation.
- `web/src/components/Sheets.tsx`: source detail fields.
- `web/src/api/engine.ts`: engine channel contract includes protocol, RF activity, decoded state, speech activity, and label kind.
- `web/src/domain/model.ts` and `adapt.ts`: frontend mode currently collapses NXDN variants.

`label_kind=control` is catalogue metadata, not proof of a decoded control frame. It also affects talking/listening behavior. Do not permanently mark every Type-D channel as control-only: it may later carry voice.

The live engine does not currently expose the Type-D SCCH metadata demonstrated by this offline analysis. A new badge or result panel needs real engine events before it can claim live decoding. Offline findings can inform catalogue annotations, with their provenance, while the live decoder integration remains separate.

## Evidence and reproduction

On wigglebutt, captures and outputs are under `/Users/coryking/projects/earshot/`:

- `recordings/mac-downloads-20261020/`: original ten archives, including the off-center 450 ms burst.
- `recordings/mac-followup-20261020/`: the new 454.3375, 454.2500, and 451.3500 captures.
- `output/mac-iq-20261020/verified-symbols.json` and `verified-symbols/`: reusable symbol command result and saved arrays; all five SCCH CRCs pass.
- `output/mac-iq-20261020/followup-4543375-nxdn48-final.json`, `followup-454250-nxdn48-final.json`, and `followup-451350-dmr-final.json`: bounded decoder summaries.
- Corresponding `*-full.log` files: full diagnostics, required when a JSON preview is truncated.

Recordings and generated output are ignored by Git. They exist on wigglebutt; the same path on another host does not imply replication.

The reusable commands are `radio iq-fm`, `radio digital-probe`, and `radio iq-symbols`. Run them from the repository checkout with uv:

```sh
uv run radio iq-fm /Users/coryking/projects/earshot/recordings/mac-followup-20261020/iq_454337000Hz.sigmf.tar --target-hz 454337500 --seconds 19.66 -o /tmp/type-d-fm.wav --yes --json
uv run radio digital-probe /tmp/type-d-fm.wav --protocol nxdn48 --json
uv run radio iq-symbols /Users/coryking/projects/earshot/recordings/mac-downloads-20261020/iq_454306250Hz.sigmf.tar --target-hz 454336927 --start-s 4.2804 --seconds 0.44025 --baud 2400 --json
```

Use SigMF capture metadata for the physical center and `--target-hz` for the desired carrier. These files have 100,000 complex samples/s; some initial files have capture discontinuities. The reader rejects a requested segment crossing a capture boundary instead of silently treating a gap as continuous RF.

DSD-FME is now installed on the normal PATH on wigglebutt, without a library-path override. Managed installer source is `run_onchange_install-dsd-fme.sh.tmpl` on dotfiles main, commit `98a57e6faefb24d6806ae8bcf7957c8d2552f137`. It pins DSD-FME `5244c857747904811d5fdcc9622758bb67167684` and statically links pinned mbelib. Linux runtime and offline WAV input are verified; the installer’s Darwin path is not yet validated.

DSD-FME auto mode excludes NXDN. Select `--protocol nxdn48` explicitly for these NXDN captures. Decoder log labels alone are not a universal protocol-verification API; CRC-gated counts depend on this pinned implementation. Independent frame validation supplies the stronger evidence for the Type-D burst.
