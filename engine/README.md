# Native radio engine

Pluto sends channelized complex IQ, RF activity, and measured spectrum over RIQ1.
Rust keeps each admitted decoder alive, catalogs DMR/NXDN activity, and sends only
new audio from the selected frequency and slot to the browser. The source can
be the ARM prototype or the upcoming FPGA channelizer; the host does not need
the entire ADC stream. There is no transmit path.

## Build and run

```sh
cd engine
./scripts/bootstrap-codecs.sh
cargo build --release --locked
./target/release/radio-engine serve \
  --source tcp:192.168.10.10:9010 \
  --web-dir ../web/dist
```

The HTTP/WebSocket server binds only `127.0.0.1:8810`. Without `--source`, a
fixture TCP listener binds only `127.0.0.1:8811`. Startup does not connect to
libiio or change receiver settings. The on-device producer owns acquisition,
RF gates, pre-roll, actual RF control readback, and restoration when it stops.

The build also accepts `DSDCC_INCLUDE_DIR` and `DSDCC_LIB_DIR`. On this development
Mac, it can use the existing `/tmp/cory-radio-dsdcc-20261019` headers and the
project's `.tools/lib` bundle. `bootstrap-codecs.sh` creates a local ignored
`.native` installation instead. DSDcc is pinned to
`f27b32d2df131ae3a376fe72d3fb880ae1f9ede1`; mbelib is pinned to
`9a04ed5c78176a9965f3d43f7aa1b1f5330e771f`. DSDcc is GPLv3 and mbelib is ISC;
notices are in `licenses/`. Rust wraps the existing C++/C algorithms through a
small C ABI. The production data path does not invoke Python or decoder
subprocesses.

Filter specifications, their derivation from the channel plan (ITU-R M.478-5, 47 CFR
90.209/90.210), the design method, and measured results against synthetic and real IQ are in
[`DESIGN-DSP.md`](DESIGN-DSP.md).

## Streaming contract

A physical coarse IQ stream may declare multiple logical `targets` in CONFIG.
One shared IQ packet fans out to the admitted logical workers; each worker mixes
and filters its target before FM. Existing one-target stream configurations work
unchanged. Channel `rf_active` is per frequency: on a shared slice it needs both
`slice_rf_active` (the producer's physical activity, same for every target in the slice)
and `target_rf_active` (measured in-band by the host from the slice spectrum, independent
of decoder admission); `slice_targets` says how many targets share the slice. Each
channel also carries `slice_level_db` and `target_level_db`. The coarse 100 ksps profile
has fixture and synthetic acceptance (see DESIGN-DSP.md) but no device acceptance yet.

The fixed 64-byte little-endian RIQ1 header is specified in
[`../offload/PROTOCOL.md`](../offload/PROTOCOL.md). Incoming payloads are bounded
at 1 MiB; IQ packets are at most 100 ms, 16..192 ksps complex signed16. A
physical RF stream includes both DMR slots. Sequence numbers cover every
message type per stream. IQ sample counters independently detect discontinuity;
transport continuity does not establish ADC continuity.

The host resamples each admitted complex stream, mixes the target's offset,
limits the channel bandwidth, FM demodulates, removes DC, and feeds a persistent
48 kHz DSDcc decoder. Unknown streams probe both DMR slots and NXDN48. The
rolling confirmation gate requires repeated DMR voice sync, or sustained NXDN
voice evidence with parity-error checks. An accidental sync does not directly
unmute audio. Data-only channels are not promoted to voice by an empty codec
output.

`--max-decoders 8` limits actual codec instances. Unknown RF streams cost three
instances; known DMR streams cost two and NXDN streams cost one. Admission order is
the pinned stream, then streams the source actually forwards IQ for (CONFIG
`iq_forwarded`/`pinned`, or IQ received in the last 2 s; RF-active before quiet), then
lower frequency. Metadata-only streams never take decoder budget, however RF-active
their activity looks (`source.metadata_only_streams` counts them). A source that does
not publish `iq_forwarded` is assumed to forward every stream it declares. Each stream has a
four-packet input queue; overflow is counted and resets DSP and codec state on
the next accepted packet. Catalog entries survive silence, retuning, and source
loss. `available`/`decoder_admitted` describe the bounded decode budget;
`rf_active`, `decoded`, and `speech_active` remain separate.

Confirmed protocols are cached; unused probes are dropped while the confirmed
decoder keeps its state. RF-idle metadata suppresses audio immediately.

PSD/activity ingestion runs independently of codec workers. `/api/spectrum?after=N`
returns only new producer-measured rows, at most 60 per response. The producer's
PSD unit is retained; a missing unit is `uncalibrated`. No waterfall is fabricated
from activity or decoder output.

`POST /api/monitor` accepts `{ "channel_id": "frequency-protocol-slot" }` or null.
It changes only the pinned listener and sends a RIQ1 CONTROL pin request to the
source. Discovery continues. `/api/status` and `/api/listener` expose state and
bounds. Source controls use `POST /api/source` with live mode, center/rate/width;
changes succeed only after the source returns CONFIG with a matching successful
`control_ack`. A source without that capability returns 501. `POST /api/stop`
stops host audio immediately and requests acknowledged RX stop/restoration when
the producer supports it. An acknowledgement timeout leaves hardware state
explicitly unknown.

`WS /api/listen` sends one JSON audio descriptor followed by one binary PCM
message: signed16 little-endian, mono, 8 kHz. Metadata contains `samples`, stable
`channel_id`, `sequence`, decoder `generation`, and `source_generation`.
Heartbeats carry generation and selection; explicit `gap` messages flush a slow
browser's queued audio. Queues hold at most 16 chunks. Ingress age measures time
since the host received the IQ packet; it is not an aligned speech timestamp.
Pre-roll primes decoders and is withheld from browser audio, with a 360 ms tail
quarantine after the live edge. Overlapping sample intervals are not decoded
twice. Audio older than 250 ms in the host's WebSocket queue is discarded; a
blocked browser write closes after 250 ms. There is no replay or clip route.

`WS /api/events` publishes source configuration, changed RF/voice state, explicit
source gaps, and spectrum-availability sequence numbers. Its queue holds 256
events; a lagging consumer gets a gap notification and fetches a fresh HTTP
snapshot. Spectrum rows stay in the bounded delta endpoint.

## Verification

```sh
cargo test --locked
RADIO_DMR_DISCRIMINATOR=/path/to/mono48k-s16.raw \
  cargo test --locked actual_dmr_fixture_decodes_incrementally -- --ignored
cargo run --release --locked -- probe /path/to/mono48k-s16.raw --paced --json
```

The offline probe is a development command, not a production replay API.
`examples/fixture_ingress.rs` sends an existing 25 ksps CI16 fixture, paced, to a
local test ingest socket. Real device acceptance still needs the on-device
producer, measured RF/USB continuity, and a live browser check. The ARM prototype
has a lower input-rate ceiling than the planned FPGA producer; the host reports
that advertised capacity rather than treating every requested width as applied.

## Live acceptance and status

`offload/live_acceptance.py` runs the bounded Pluto to engine to browser-path acceptance
under the shared `/tmp/pluto-radio.lock` (see offload/arm/README.md for the cross-build and
the exact command). It checks the Pluto's reported firmware first, then records spectrum
rows, WebSocket events and audio, freshness, gaps, pinning, and the acknowledged STOP with
receiver readbacks. Pass criteria are the delivered path and restoration; audio and RF
activity are reported, since the air may be quiet.

Endpoint notes: `/api/status` always reports numeric `received_seconds`,
`source_position_s`, `decode_latency_s`, `center_hz`, `rate_hz` and `bandwidth_hz`, because
the browser validates them strictly (`decode_latency_s` is the ingress age of audio that
flowed in the last 5 s, else 0). `/api/spectrum` `low_db`/`high_db` are display hints: fixed
-125 to -65 for unit `dBFS/Hz`, otherwise fitted to the newest 30 rows (5th percentile - 2 dB
to 99.9th percentile + 2 dB, at least 30 dB apart). On a slice shared by several targets,
per-target `rf_active` needs forwarded IQ; with none, `slice_rf_active` is true while each
target's `rf_active` stays false. Targets the slice cannot contain are refused and counted
in `source.out_of_slice_targets`; targets on the same frequency (DMR slots) count once in
`slice_targets`.

Remaining acceptance: sustained idle/noise behavior over hours; frontend capability checks
for the new channel fields (`slice_*`, `target_*`); device acceptance of a multi-target
100 ksps slice, which waits for the FPGA producer. The ARM producer at 800 ksps runs at about
0.87x real time (late buffers), so its IQ has gaps; hardware continuity is unproven.

## Frontend needs

The web UI (`web/`, design in `web/DESIGN.md`) is typed against the endpoints
above and treats producer-derived fields as nullable; `0` for `center_hz`,
`rate_hz` or `bandwidth_hz` is read as "not known yet", so either is fine. It
also needs the endpoints below. Until the engine serves them, live mode falls
back in the browser (a starter list of names from the RF research, labels
saved on the device, a session-only call log without audio, activity since the
page opened) and `?demo` runs everything on a typed mock
(`web/src/backend/mock.ts`). Wire types are in `web/src/api/needs.ts`; the
operator view shows which of these the engine answers. Each can land
independently.

Conventions: a 404 on one of these paths means "not built yet" (missing
resources use 410 or 422); errors are `{detail: string}`; timestamps are Unix
seconds; ids are strings.

1. **Per-client listening.** `/api/monitor` selects one channel for every
   browser, so two people in the house can't listen to different channels.
   Accept `WS /api/listen?channel_id=…` and keep a decoder for each channel
   that has a listener; release a selection when its last listener closes;
   keep `/api/monitor` for compatibility. Today the UI treats a heartbeat
   `channel_id` of `null` as "released" and re-selects its channel, and a
   different non-null id as "someone else switched", with "Switch back".
2. **Analog FM audio.** Monitoring an FM or `Unknown` channel returns 422 and
   there's no FM audio path. Conventional analog FM is in scope (450–470 MHz).
   Report `protocol: "FM"` for confirmed analog voice, send its audio on the
   same listen socket, and advertise `capabilities.fm_audio: true` in the
   source CONFIG so the UI offers it.
3. **Recent calls with audio (lookback).**
   - `GET /api/calls?before=<unix s>&limit=<n>&source=<id>` →
     `{calls: [{id, source_id, channel_id, started_at, ended_at|null,
     encrypted, audio_url|null, talker|null, transcript|null}],
     next_before: <unix s>|null}`, newest first.
   - `GET <audio_url>` → `audio/ogg; codecs=opus` (or WAV), seekable.
   - `{type:"call", call}` on `/api/events` when a call starts and ends.
   - `talker` is for trunking (radio ID) and `transcript` for later STT; both
     null for now. Retention can be time- or size-bounded; say which.
4. **Source catalog with labels.**
   - `GET /api/catalog` → `{sources: [{id, kind: "channel"|"talkgroup",
     channel_ids, frequency_hz, slot, system: {id, name}|null,
     talkgroup_id|null, name, description, probable_user, traffic_kind,
     mode: "fm"|"dmr"|"nxdn"|"p25"|"unknown",
     encryption: "clear"|"encrypted"|"mixed"|"unknown"}]}`.
   - `PUT /api/catalog/{id}` with the same object is an upsert (household
     members name channels from the UI). Persist it across restarts.
   - The UI joins entries to channels by `frequency_hz` (within 3 kHz) and
     `slot`, falling back to `channel_ids`, because a channel's id changes
     when the engine identifies its protocol (`…-Unknown-1` → `…-DMR-2`).
     Ids should stay stable once issued; the UI keys follow/mute on them.
   - Seed from `docs/rf-environment-seattle.md` on the `rf-research` branch,
     keyed by measured frequency. Trunked talkgroups use
     `kind: "talkgroup"` with `system` and `talkgroup_id`.
   - When this lands, the UI uploads labels that were saved on a device.
5. **Activity history (busy vs normal).**
   `GET /api/activity?window_s=86400&bucket_s=300` →
   `{bucket_s, start, block: [talk_s…], sources: {source_id: [talk_s…]},
   baseline: [talk_s…]|null}`, where `baseline[i]` is the typical talk time
   for the same weekday and hour as `block[i]` (null until learned). The home
   screen compares the last 15 minutes against it.
6. **Compact spectrum rows.** `GET /api/spectrum.bin?after=N`
   (`application/octet-stream`, optionally pushed on `WS /api/spectrum`).
   RSP1, little-endian, 48-byte header: `"RSP1"`, u16 bins, u16 rows,
   f64 center_hz, f64 span_hz, f32 low_db, f32 high_db, u64 latest sequence
   (even with zero rows), u32 generation, u32 reserved; then per row
   u64 sequence, f64 time_s and `bins` × u8 levels scaled between low_db and
   high_db. A 1024-bin row is 1,040 bytes instead of about 8 KB of JSON.
   Keep low/high stable across responses (the fitted display range is fine
   if it changes slowly); the UI clears its history when the generation,
   center or span changes. Until then the UI reads `/api/spectrum` JSON,
   quantizes it over a fixed −150…+10 dB window and auto-levels, since the
   per-response display range would shift the scale between batches.
7. **Listening areas (blocks).** `GET /api/blocks` →
   `{current_id, blocks: [{id, name, description, center_hz, span_hz}]}` and
   `POST /api/blocks/current {id}`, which applies through the same
   acknowledged control path as `POST /api/source`. Names are plain
   ("Downtown business radio").
8. **Push alerts (later).** Alerts work while the page is open. Alerts with
   the phone locked or the app closed need Web Push: `GET /api/push/key`
   (VAPID public key) and `POST /api/push/subscribe {subscription, rules}`.

Dev feeder: `web/dev/fixture_feed.py` sends the DMR fixture plus PSD rows
measured from the 6 Msps recording it was cut from, so the waterfall shows
real recorded RF. It is a development tool, not a data path.
