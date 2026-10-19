# RIQ1 transport, version 1

The on-Pluto producer filters/decimates wideband IQ, detects RF activity, retains
short pre-roll, and sends per-channel complex IQ plus discovery information.
The Rust host decodes channels and serves browser audio, activity, and spectrum.
Python batch files and full-band USB IQ are development adapters, not the target
interface. The Pluto producer can be native C with the installed uClibc ABI.

TCP provides ordered bytes, not message boundaries. The target Pluto service
listens on port 9010 over USB Ethernet; the host connects. A localhost fixture
listener on port 8811 supports integration before device deployment. Receivers
must use exact reads, validate the header before allocating, and reject payloads
larger than 1 MiB. No unbounded per-client or per-channel queues.

## Fixed 64-byte header

All integers and samples are little endian. Serialize fields explicitly; do not
write a compiler struct whose padding or unaligned accesses vary by platform.

| Offset | Bytes | Field |
| ---: | ---: | --- |
| 0 | 4 | ASCII `RIQ1` |
| 4 | 2 | version, `1` |
| 6 | 2 | header bytes, `64` |
| 8 | 2 | type |
| 10 | 2 | flags |
| 12 | 8 | source generation |
| 20 | 4 | stream ID |
| 24 | 8 | sequence |
| 32 | 8 | first sample |
| 40 | 4 | sample rate in Hz, rounded when rational |
| 44 | 8 | signed stream center frequency in Hz |
| 52 | 4 | payload bytes, at most `1048576` |
| 56 | 2 | format |
| 58 | 2 | valid sample bits |
| 60 | 4 | reserved, zero |

Types: IQ `1`, PSD `2`, activity `3`, configuration `4`, gap `5`, host control `6`.
Formats: interleaved signed I16/Q16 `1`, float32 PSD `2`, UTF-8 JSON `3`.
Flags: start `1`, end `2`, pre-roll `4`, discontinuity `8`, overflow `16`,
continuously forwarded/pinned `32`.

An IQ payload has an integral number of 4-byte complex samples and spans no
more than 100 ms (2500 samples at 25 ksps). `first sample`
counts output samples for that stream, including intentional quiet intervals.
Sequence counts emitted messages independently per stream, across message types;
source-level configuration uses stream ID zero. Metadata can therefore describe
an intentionally suppressed sample interval without fabricating a transport gap.
Counters reset only with a new generation. A new connection/source, LO/sample-rate
change, filter reconfiguration, or unquantifiable sample loss begins a new
source generation or an explicit discontinuity. Dropped transport messages and
intentional closed gates must remain distinguishable.

The header center is the physical complex-IQ center, which can differ from a
logical RF target. Preserve the target offset for the host's final mixer. A DMR
slot is a decoder property, not an IQ stream: a single RF stream contains both
slots. In particular, 25 ksps complex output is valid; the host discriminator and
resampler produce the 48 kHz stream required by DSDcc.

## Configuration and discovery

Configuration is sent before samples, again for a new generation, and contains:

- `input_center_hz`, `input_rate_hz`, `input_bandwidth_hz`: actual device readbacks.
- `hardware_counter`: whether device-origin sample continuity is independently
  established. Stock libiio software counts do **not** establish it; use `false`.
- `streams`: objects with `stream_id`, `center_hz`, `target_hz`, `rate_hz` or exact
  `sample_rate_num_hz`/`sample_rate_den`, `protocol` (`DMR`, `NXDN48`, `Unknown`),
  and optional selected `slot` (`1` or `2`).
- A coarse physical stream can instead declare `targets: [{target_hz, protocol,
  slot?, channel_id?}]`. `channel_id` is an optional stable string; the host can
  derive `stream_id:target_hz:slot` when absent. IQ is transmitted once and fans
  out to bounded logical-channel workers. Each worker mixes and filters its
  target in complex IQ before FM discrimination. Pinning a logical target pins
  its physical source stream. The legacy single-target fields remain valid.
- Channelizer shape, output filter delay in input samples (a rational or doubled
  integer), sample scaling/valid bits, and current gate/pre-roll/queue limits.
- `psd`: `bins`, `center_hz`, `span_hz`, and `unit`.

Power estimates must be labeled honestly. A plot of FFT magnitude is not
automatically dBFS/Hz: density requires sample scaling and filter equivalent
noise bandwidth correction. Use `dBFS` for integrated channel power unless that
correction is implemented. A dBFS/Hz PSD declares `unit: "dBFS/Hz"` only after
normalization. PSD samples run from low to high frequency, independently of
which channel decoders are scheduled.

Activity JSON includes `stream_id`, `rf_active`, `level_db`, and `sample_index`.
An activity heartbeat/catalog persists through quiet periods. Channel presence,
RF activity, decoder confidence, codec speech, intentional gating, and source
loss are separate states. A bounded decoder pool must report queued/skipped
channels rather than implying every discovered channel is being decoded.

## Gates and backpressure

Gate on RF energy, not speech recognition. Use a per-channel noise estimate,
hysteresis, pre-roll, and hang time. Continuously forward pinned control channels.
Pre-roll is immediate live acquisition history used to initialize codecs; it
must not replay a previous conversation to the browser. The host tracks sample
positions to avoid decoding duplicate overlap and primes decoder state while
pre-roll is set. Clearing pre-roll marks the live edge. Do not discard the
headers/state recovered from the opening burst.

The producer must bound queues and report explicit GAP metadata, affected sample
intervals when known, and overflow/discontinuity flags. A TCP writer must never
silently stall a capture loop indefinitely. Slow consumers cannot grow device
RAM without limit. Keep the activity/PSD/control path alive when decoder budgets
are exhausted. Transport sequencing does not prove hardware capture continuity;
keep that capability explicit until FPGA counters/overflow telemetry exist.

## Host control

Control type `6` uses JSON format `3` on the same bidirectional TCP connection.
Commands have an ASCII string `request_id` and `action`: `get_config`, `pin`,
`configure`, or `stop`. A pin request includes `stream_id` and boolean `pinned`.
A configure request supplies requested `input_center_hz`, `input_rate_hz`, and
`input_bandwidth_hz`. The producer advertises supported controls and rate limits
in CONFIG capabilities; unsupported requests fail explicitly. Validate a whole
request before changing device controls. Configuration changes require a new
generation and actual readbacks; pin-only changes keep the generation.

CONFIG type `4` acknowledges with `control_ack: {request_id, ok, error?}` after
the action is verified. Sending a request is not success. `stop` destroys the
receive buffer, disables RX streaming channels, restores original receive
controls, verifies readbacks, and only then acknowledges `receiver_state:
"stopped"` with `streams: []`. A failed restoration must be reported as failure.
The host must not label the hardware stopped until that acknowledgment arrives.
No control command ever opens a transmit streaming device.
