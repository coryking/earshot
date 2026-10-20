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

`--max-decoders 8` limits actual codec instances. Unknown RF streams cost two
instances (DMR and NXDN48); a known DMR channel costs one, because one DSDcc instance
decodes both TDMA slots, and a known NXDN stream costs one. Admission order is
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

Analog FM: every stream not known to be NXDN48 also runs an FM audio unit (see DESIGN-DSP.md)
that produces `FM` slot 1 channels once a quieted voice carrier is heard; it is muted while DMR/NXDN
sync is present. `POST /api/monitor` also accepts a channel whose protocol is still `Unknown`: the
pin forwards its IQ and audio follows whatever is heard at that frequency, labeled with the pinned id.

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

## API for the household UI

All JSON unless noted. A 404 from a route means the feature is not built yet; none of these
require a source to be connected.

* `GET /api/status`, `GET /api/spectrum` (JSON rows), `GET /api/listener`, `POST /api/monitor`,
  `POST /api/stop`, `POST /api/source`, `WS /api/events` as before. Channel entries now carry
  `frequency_hz` (calibrated true frequency, snapped to the 6.25 kHz raster when a measured
  carrier lands on it; `frequency_snapped`), `reading_hz`/`logical_hz` (what the radio reported),
  `carrier_hz`/`carrier_offset_hz`/`carrier_snapped` (the measured carrier), `label`,
  `label_kind`, `label_detail` (plain name from RadioReference/FCC data, see
  `offload/build_labels.py`), and `user` (operator edits) with `display_name`
  (user name, else label). Status also reports `calibration_ppm`, `labels_loaded`, `listeners`.
* **Per-browser listening.** `WS /api/listen?channel_id=ID` listens to that channel on its own;
  several browsers can listen to different channels at once. The engine pins the producer
  streams for every listened channel (and unpins when the last listener leaves), admits
  decoders for them first, and sends audio labeled with the requested id. Without
  `channel_id` the socket follows the shared `/api/monitor` selection. Pinning an `Unknown`
  channel works; audio follows whatever is heard on it (digital or analog `FM`).
* **Recent calls.** `GET /api/calls?before=&limit=&source=` returns
  `{calls, next_before, in_progress}`, newest first (`before` is an exclusive id cursor).
  Each call: `id`, `source` (channel id), `protocol`, `slot`, `frequency_hz`, `label`,
  `started_at` (unix s), `duration_s`, `level_db`, `audio_url`
  (`/api/calls/{id}/audio.wav`, 8 kHz mono signed16 WAV), `talker` and `transcript` (null),
  `encrypted` (false). Every speaking channel is recorded, not only the one being heard. A
  call ends 2 s after its audio stops; bursts under 0.8 s are discarded. `WS /api/events` sends
  `{type:"call", phase:"start"|"end", call}`.
* **Catalog.** `GET /api/catalog` lists located, decoded or edited channels
  (`kind: "channel"`; `channel_kind` is voice/data/unidentified). `PUT /api/catalog/{id}`
  accepts `name` (80 chars, or null), `notes` (500), `followed`, `muted`; edits persist.
* **Activity.** `GET /api/activity?window_s=86400&bucket_s=300` returns talk-seconds per
  bucket for the block (`total`), per source (`sources`), RF-active seconds (`rf_total`) and
  a `baseline` (mean of the same bucket one or two weeks earlier, else one to six days
  earlier; null without history). Buckets are 5 minutes, kept 14 days.
* **Compact spectrum.** `GET /api/spectrum.bin?after=N&limit=` returns RSP1: a 32-byte
  little-endian header (`RSP1`, version u16 = 1, bins u16, rows u16, flags u16 = 0, low_db f32,
  high_db f32, center_hz f64, span_hz f32), then per row `seq u64, time f64, bins x u8` with
  `level = round(255 * (clamp(db, low, high) - low) / (high - low))`. `low`/`high` are fitted
  to the data like `/api/spectrum`. `X-Sequence` carries the newest sequence number.
* **Block.** `GET /api/blocks` reports the parked block and whether the producer can retune
  (`can_change`; the ARM producer cannot). `POST /api/blocks/current` goes through the same
  acknowledged path as `/api/source` and returns 501 for producers without it.

Persistence: with `--state-dir DIR` the call index and WAVs (`calls/`), activity buckets,
the catalog and operator edits survive restarts and shutdown flushes on SIGTERM. Without it
everything stays in memory. `--call-memory-mb` bounds the call audio held in RAM (older audio
is served from disk). Retention: 500 calls, 7 days.

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
