# earshot

"What's going on around me?"

earshot is a home receiver that lets anyone in the house hear and follow local two-way radio
without knowing anything about radio. You open it on your phone, see which channels near you are
busy right now under plain names, and tap one to listen. No scanner programming, no frequency
lists, no SDR knobs.

Under the hood it's an ADALM-Pluto SDR with a custom FPGA image, a Rust engine that
finds and decodes the voice traffic, and a phone-first web app. It's receive-only. Nothing in
here transmits.

<!-- screenshots go here -->

## What works today

- **A 512-channel FPGA channelizer inside the Pluto.** The Pluto's Zynq 7010 runs a 2x-oversampled
  polyphase filter bank on the full 25.6 Msps ADC stream: 512 bins of 50 kHz at 100 ksps each,
  plus per-bin power and activity detection. Only the channels worth looking at leave the radio.
  To make room I stripped the transmit path out of ADI's reference design. It routes at 74.7% of
  the LUTs with timing met and no clocks loosened, and a frozen live frame matched the
  bit-accurate Python model in all 512 bins. Details in [`fpga/`](fpga/README.md).
- **A Rust engine that finds the voice.** It reads channelized IQ from the Pluto over a small TCP
  protocol (RIQ1), probes every busy channel for DMR, NXDN48 and analog FM, keeps a decoder alive
  for each channel it admits, catalogs what it hears, and streams live audio to browsers over a
  WebSocket. See [`engine/`](engine/README.md).
- **A native AMBE+2 decoder.** DMR voice uses the AMBE+2 vocoder. The open-source option, mbelib,
  is an approximation and sounds like it. So I ported the decoder from the Tytera MD-380 radio's
  firmware to Rust, routine by routine, and it matches the firmware's output sample for sample on
  every test corpus. No emulator, no ARM code at run time, and the firmware isn't needed to build
  or run. How and why: [`engine/src/ambe/DESIGN.md`](engine/src/ambe/DESIGN.md).
- **A web app for people who don't care about radio.** It's phone-first: **Now** shows what's
  active, **Catch up** has recent calls, **Radio** picks the block of spectrum, and the waterfall
  and technical controls sit behind an **Operator** view. Built with React, shadcn/ui and
  Tailwind. Run it against sample data with `/?demo`. See [`web/`](web/DESIGN.md).
- **An always-on receiver that cleans up after itself.** [`supervisor/`](supervisor/README.md)
  keeps the Pluto's channel producer running, backs off when the Pluto reboots, and puts the
  radio back the way it found it whenever the producer stops. It has run unattended at home as
  a service.
- **Homework on what's actually on the air in Seattle.** 35 trunked systems, their sites and
  control channels, cross-checked against FCC licence records, plus the licensed land-mobile
  channels within 40 km of downtown. See [`docs/rf-environment-seattle.md`](docs/rf-environment-seattle.md).

## Not done yet

- Trunking. Most of the traffic people care about in Seattle is on P25 trunked systems (PSERN,
  King County Metro). The data model leaves room for systems and talkgroups; following them isn't
  built.
- Encrypted calls can't be heard, and earshot won't try. They show up as activity.
- Speech-to-text transcripts are a later idea.
- One Pluto parks on one block of up to 20 MHz at a time. Covering more needs better hardware.

The bigger picture of what I'm trying to solve is in
[`docs/problem-statement.md`](docs/problem-statement.md), and the product scope is in
[`docs/PRODUCT.md`](docs/PRODUCT.md).

## Running it

You need a PlutoSDR Rev.B, Rust, Node and [uv](https://docs.astral.sh/uv/). Getting the FPGA
image onto the Pluto has its own guide in [`fpga/README.md`](fpga/README.md). The engine:

```sh
cd engine
./scripts/bootstrap-codecs.sh        # builds DSDcc and mbelib into engine/.native
cargo build --release --locked
npm ci --prefix ../web && npm run build --prefix ../web
./target/release/radio-engine serve --source tcp:pluto.local:9010 --web-dir ../web/dist
```

It serves on `127.0.0.1:8810` only. Put your own access control in front of it if you want it
on your phone.

To look at the app with no radio at all:

```sh
npm ci --prefix web && npm run dev --prefix web    # then open /?demo
```

## Layout

| Path | What's there |
| --- | --- |
| `fpga/` | Channelizer RTL, bit-accurate Python model, testbench, Pluto block design, build scripts |
| `engine/` | Rust engine: ingest, DSP, decoders, catalog, HTTP/WebSocket API, the AMBE+2 port |
| `web/` | React frontend |
| `supervisor/` | Always-on receiver supervisor |
| `offload/` | Pluto-side ARM prototype, the RIQ1 protocol, live acceptance tools, Pluto recovery notes |
| `src/radio_tools/` | The earlier Python analysis CLI and receive console ([docs](docs/radio-tools.md)) |
| `docs/` | Problem statement, product spec, Seattle RF research |

## About the AMBE+2 code

The AMBE+2 decoder in `engine/src/ambe` is derived from DVSI's proprietary implementation in the
MD-380 firmware, including constant tables copied from that image. The firmware has been public
since Tytera shipped it in 2015, and md380tools and friends have been running this same decoder in
public for about ten years. This is a personal project. If you build something with it, that part
is your call. If you want to stay clean, run the engine with `--vocoder mbelib`.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE). Third-party notices are in
[`engine/licenses/`](engine/licenses/).
