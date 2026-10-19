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

## Streaming contract

A physical coarse IQ stream may declare multiple logical `targets` in CONFIG.
One shared IQ packet fans out to the admitted logical workers; each worker mixes
and filters its target before FM. Existing one-target stream configurations work
unchanged. The coarse 100 ksps profile has not yet had fixture or device acceptance.

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

## Checkpoint, 2026-10-19

The embedded codec and 25 ksps IQ-to-WebSocket fixture paths passed. The actual
on-Pluto producer has not been connected to this host: integration was stopped
for the session transfer before acquisition began. Native servers are stopped.

Decoder admission now honors the source forwarding mask (see Streaming contract).

Remaining acceptance: multi-target 100 ksps adjacent-channel rejection and live
producer-to-host binary audio; source stop ACK and actual RF/FIR/clock restoration
with the host connected; sustained idle/noise behavior; frontend capability and
waterfall checks. ARM throughput and the FPGA build remain producer work.
