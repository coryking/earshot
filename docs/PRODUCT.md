# Product spec

## Goal

"What's going on around me?" A home receiver that lets anyone in the household, a spouse or a sibling, hear and follow local two-way radio activity, without knowing anything about radio. It is not an SDR toy and not a broadcast tuner: broadcast AM/FM stations are out of scope.

## Users

- **Household listener** (primary): no radio vocabulary. Uses it on a phone, often one-handed, sometimes with the screen locked.
- **Operator** (Cory): runs the box, picks the hardware, and diagnoses problems. Technical controls exist only for this role, behind an admin view.

## Scope now

- **One parked block.** The receiver covers one block of spectrum at a time: up to about 20 MHz with the FPGA channelizer, about 4 MHz with today's host channelizer. The listener picks the block, described in plain terms, and the app parks there. Discovery, the catalog and listening all happen inside the block. Whole-band scanning is out of scope.
- **Conventional UHF, 450–470 MHz**: analog FM and DMR.
- **Live listening plus lookback.** Recent calls are kept with their audio, so a listener can catch up on what they missed.

## Next (fast-follow)

- Trunking: PSERN and King County Metro P25, DMR Capacity Max. Much of the activity listeners care about is here. Leave room for trunked sources (systems, talkgroups, encrypted-call labels) in the data model and UI, but don't build them yet.

## Later

- Speech-to-text transcripts of calls.
- A wider or multi-block receiver on better hardware.

## Most important actions, ranked

1. **See what's happening right now** in the parked block: plain names, and how busy it is compared with normal. This is the home screen.
2. **Tap to listen.** One tap, near-instant audio, with stop, mute and volume. Audio keeps playing when the phone locks.
3. **Catch up on what I missed.** Recent calls, newest first, playable.
4. **Follow or mute a source.**
5. **Get alerted** when a followed source gets busy or activity spikes.
6. **Change block.** Rare and deliberate; the app explains that it hears one block at a time.
7. **"Is it working?"** One plain status. **"What is this?"** A plain explanation of a source: probable user, kind of traffic, and why encrypted calls can't be heard.

Listeners never tune frequencies, set gain or bandwidth, or pick decoders. Those controls are operator-only.

## The waterfall

The waterfall is a supporting view, not the home screen. On mobile it is a thin live strip that shows the block is alive. Its traces correspond to items in the activity list, and tapping a trace listens; this is the path to signals that have no name yet. It becomes a full view in landscape or on desktop, for exploring. The browser renders it from compact spectrum data.

## Design principles

- Dense, not cluttered. Every element earns its place, and secondary detail sits behind progressive disclosure.
- Mobile first: thumb reach, bottom-anchored primary actions, sheets rather than side panels. Desktop adds space and keyboard shortcuts.
- Prefer a command palette, menus and sheets to panels; any panels are resizable.
- Borrow from good non-SDR apps (music players, chat channel lists, observability timelines). Use SDR conventions only where the domain needs them.
- Shop frontend stack (React, shadcn/ui, Tailwind, Vite, TanStack Query), plus any library that earns its place. The browser does rendering work; the server sends compact data.

## Decisions

- License: GPL, so logic may be ported from OP25, SDRTrunk and trunk-recorder.
- Raw Pluto/IIOD access on the tailnet is accepted.
- FPGA: transmit features are stripped to make room for the channelizer.
- Python stays out of the live data path.
