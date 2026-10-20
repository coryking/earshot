# Frontend design

Source of truth: [`../docs/PRODUCT.md`](../docs/PRODUCT.md). This file maps that
spec onto screens, gestures and data. Where the two disagree, the spec wins.

The listener is a spouse or a sibling: no radio vocabulary, on a phone,
often one-handed, sometimes with the screen locked. Everything on the default
screens is written for them. Radio terms (MHz, slot, DMR, gain) appear only as
secondary detail or in the operator view.

## Ranked actions, mapped to screens and gestures

| Rank | Action | Where | Gesture |
| --- | --- | --- | --- |
| 1 | See what's happening now | **Now** tab (home). A plain sentence at the top ("Two channels talking. Busier than usual for a Tuesday evening."), the live strip, then the source list: talking now first, then recently heard, then quiet. | Open the app. Pull to refresh is unnecessary because the list is live. |
| 2 | Tap to listen | Any row in the Now list, any trace in the live strip, any call in Catch up. The **player bar** docks above the tab bar with stop, mute and volume. | One tap on a row listens. Tapping the playing row again stops. The player bar's stop button is in thumb reach. Volume lives in the expanded player sheet (swipe the bar up). |
| 3 | Catch up on what I missed | **Catch up** tab: recent calls newest first, grouped by "Just now / Earlier this hour / Today", each with a play button and duration. | Tap a call to play it. Playback continues down the list (autoplay next) like a podcast queue, so a listener can catch up hands-free. |
| 4 | Follow or mute a source | Star and mute in the source sheet and the expanded player. Followed sources pin to the top of Now; muted ones sink into a collapsed "Muted" group and never alert. | Long-press a row (or tap its ⋯) to open the source sheet. |
| 5 | Get alerted | Bell in the header opens the alerts sheet: "Tell me when a followed channel starts talking", "Tell me when it's much busier than normal". Uses browser notifications. | Toggle switches. The bell shows a dot when alerts are on. |
| 6 | Change block | Tap the block name in the header ("Downtown business radio"). A sheet explains that the radio hears one stretch of the airwaves at a time, lists the blocks in plain terms, and asks for confirmation. | Deliberately two taps plus a confirm. Never a swipe. |
| 7 | Is it working? / What is this? | The status dot next to the block name ("Listening", "Paused", "Not connected"); tap it for a plain explanation. Each source sheet has "What is this?": probable user, kind of traffic, and, for encrypted sources, why you can't hear them. | Tap. |

Listeners never tune, set gain or bandwidth, or pick decoders. Those live in the
**operator view** (`#/operator`), reached from the Radio tab's "Operator tools"
row and the command palette. It is not hidden for security (the tailnet is the
boundary); it's out of the way so a listener doesn't wander into it.

## Mobile-first layout

Phone portrait is the design target (390 × 844). Everything a listener needs
is reachable with the right thumb.

```
┌──────────────────────────────┐
│ ● Downtown business radio  🔔│  header: status dot, block name (tap = block sheet), alerts
│                              │
│ Two channels talking.        │  the sentence: what's happening, busy vs normal
│ Busier than usual tonight.   │
│ ▁▂▅▇▅▂▁▁▁▂▃▁▁▁▁▆█▆▁▁▁▁▁▁▂▁ │  live strip (waterfall, ~56 px), tap a trace = listen
├──────────────────────────────┤
│ ★ Front desk          ▮▮▮▯  │  talking now: level meter, followed star
│   Business dispatch, digital │
│   Warehouse crew      2 min  │  recently heard: relative time, 30-min activity lane
│   ▁▁▂▁▁▁▁▁▃▂▁▁▁▁▁▁▁▁▁▁▁▁▁▂  │
│   454.300 MHz        quiet   │  unnamed: shows its number until labeled
│   …                          │
├──────────────────────────────┤
│ ▶ Front desk    ■  🔇        │  player bar: stop and mute in thumb reach
│  Now   Catch up   Radio      │  tab bar
└──────────────────────────────┘
```

- **Sheets, not panels.** Source detail, block change, alerts, status and the
  expanded player are bottom sheets (vaul drawers) that open from where the
  thumb already is. They close by swiping down or tapping outside.
- **Bottom-anchored primary actions.** The player bar and tab bar are fixed to
  the bottom, above the home indicator (`env(safe-area-inset-bottom)`).
- **Dense, not cluttered.** Each row has exactly three facts: name, what it is
  (one line), and when it was last heard or a live level meter. The 30-minute
  activity lane under each row is the only chart.
- **Progressive disclosure.** Frequencies, slots and protocols are in the
  source sheet's "Details" section, collapsed by default.

## Desktop and landscape

Desktop gains space; it does not gain new concepts. At ≥ 1024 px wide (and in
phone landscape) the layout becomes two resizable panes:

```
┌──────────────────────┬───────────────────────────────────────────────┐
│ ● Downtown business  │  full waterfall (resizable height)            │
│ Two channels talking │  source markers along the top edge            │
│ ─────────────────────│───────────────────────────────────────────────│
│ source list          │  Catch up timeline (calls by source over time)│
│                      │                                               │
├──────────────────────┴───────────────────────────────────────────────┤
│ player bar                                                           │
└──────────────────────────────────────────────────────────────────────┘
```

Keyboard shortcuts (shown with `?`):

| Key | Action |
| --- | --- |
| `⌘K` / `Ctrl+K` / `/` | Command palette: jump to a source, follow, change block, open operator view |
| `j` / `k` or ↓ / ↑ | Move through the source list |
| `Enter` or `Space` | Listen to the focused source; again to stop |
| `Esc` | Stop listening (or close the open sheet) |
| `m` | Mute or unmute |
| `f` | Follow or unfollow the focused source |
| `1` `2` `3` | Now, Catch up, Radio |
| `?` | Show shortcuts |

## The waterfall

A supporting view, as the spec says. Rendered entirely in the browser:

- **Phone portrait:** a thin live strip (~56 px, about 20 seconds of history)
  under the sentence. It proves the block is alive. Each catalog source has a
  thin tick at its position; when a source is talking, its tick glows. Tapping
  the strip listens to the nearest source within a hit radius; tapping a trace
  with no source opens "Unnamed signal" with what we know and why it can't be
  played yet (when that's the case).
- **Landscape and desktop:** the full view, a few minutes of history, with
  hover readout, source markers and the same tap-to-listen.
- **Rendering:** WebGL2 texture ring buffer, one row per spectrum line, colormap
  as a 256-entry lookup texture, exponential averaging per bin in the client,
  and a 2D canvas fallback when WebGL is unavailable. The server sends compact
  rows; the browser does the rest.
- **Colormap:** a perceptual dark-to-amber ramp derived from inferno, starting
  from the app's ink blue rather than black so the empty waterfall sits in the
  page instead of punching a hole in it.

## Plain-language naming

| Engine says | Listener sees |
| --- | --- |
| channel `454029223-DMR-2` with a catalog label | the label: "Front desk" |
| channel with no label | its number, the way a phone shows an unknown caller: "454.029 MHz", plus "line 2" when one frequency carries two conversations (DMR slots) |
| `protocol: DMR / NXDN48` | "digital voice" (detail sheet: "DMR, slot 2") |
| `protocol: Unknown` | "not identified yet" |
| analog FM (catalog, later engine) | "voice" |
| `speech_active` | "Talking now" plus a level meter |
| `rf_active` without speech | "Signal, no voice" (data or a carrier) |
| encrypted (catalog/trunking) | "Private: encrypted, can't be heard" with an explanation in "What is this?" |
| `/api/status` state `receiving` | "Listening" (green), `stopped` "Paused", `error` or disconnected "Not connected" (with the error in plain words and the raw message under Details) |
| center / bandwidth | a block name: "Downtown business radio (454 MHz)" |
| talkgroup (trunking, next) | the talkgroup label, grouped under its system: "Metro buses" in "King County Metro" |

## Operator view

`#/operator`. Technical controls and raw state for Cory:

- Engine status: state, error, generation, decoder capacity and use, queue
  depth, drops, processed batches, last samples, source position, continuity.
- Source config (the CONFIG JSON as read back from the producer) and a form to
  set center/rate/bandwidth via `POST /api/source`, which reports 501 plainly
  when the producer can't do it.
- Receiver stop (`POST /api/stop`) behind a confirm. This is the only place it
  appears; the listener's stop button only clears the listener selection.
- Channel table: id, stream, frequency, protocol, slot, rf/decoded/speech
  flags, level, last heard, admitted.
- Catalog editor: name, description, probable user, traffic kind, encrypted.
- Feature status: which "frontend needs" endpoints the engine answers, and
  which are running on the in-browser fallback.

## Borrowed from non-SDR apps

- **Music players (Apple Music, Spotify):** the docked mini player above the
  tab bar that expands into a full "now playing" sheet; lock-screen controls
  through the Media Session API; autoplay-next in Catch up.
- **Chat channel lists (Slack, Discord, iMessage):** the source list: bold when
  active, a quiet secondary line, followed sources pinned, muted ones grouped
  and greyed, unknown numbers shown as the number until named.
- **Observability timelines (Honeycomb, Grafana, Datadog):** the per-source
  activity lane (a 30-minute sparkline of call blocks), "busy vs normal" as a
  comparison against a baseline band, and the desktop Catch up timeline with
  calls laid out per source over time.
- **Podcast apps (Overcast):** Catch up as a queue with play-through.
- **Weather apps:** the plain sentence at the top that summarises the state
  before showing data.

## Visual identity

Subject: a household radio for listening to the neighbourhood at night. The
vernacular is the lit dial of a kitchen radio and the amber "on air" lamp, not
the green-on-black of SDR software.

- **Colour (dark first):** ink `#0F1A2B` (background, a deep blue rather than
  black), slate `#1B2940` (raised surfaces), mist `#C9D4E5` (body text),
  paper `#F2F5FA` (headings), lamp `#F5A524` (talking now, the only warm
  colour, reserved for "live"), signal `#5EB8C9` (selection and focus).
  Light theme inverts to paper backgrounds with ink text; lamp darkens to
  `#B86E00` for contrast.
- **Type:** Atkinson Hyperlegible Next, one family, designed for legibility
  for low-vision readers, which suits a household audience and small phone
  text. Tabular figures for times and frequencies; no monospace.
- **Motion:** only the live level meter and the waterfall move by themselves.
  Sheets animate in response to touch. `prefers-reduced-motion` freezes the
  meter to a static lamp.
- **The one bold thing:** the sentence at the top of Now, set large, which
  reads as the answer to "what's going on around me?"

## Data model

The UI works on a domain model, not on engine JSON. Adapters translate the
engine's wire types (typed in `src/api/engine.ts`, with nulls handled) into it.

- **Source**: anything you can listen to. `kind: "channel" | "talkgroup"`.
  Channels have `frequencyHz` and optional `slot`. Talkgroups (trunking, next)
  have `system: { id, name }` and `talkgroupId`. Both have `name`,
  `description`, `probableUser`, `trafficKind`, `mode`
  (`fm | dmr | nxdn | p25 | unknown`), `encryption`
  (`clear | encrypted | mixed | unknown`), and live state (`talking`,
  `signal`, `levelDb`, `lastHeardAt`, `listenable` with a reason when not).
- **Call**: one transmission. `sourceId`, `startedAt`, `endedAt`, `encrypted`,
  `audioUrl | null`, and nullable `talker` (radio ID, for trunking) and
  `transcript` (speech-to-text, later).
- **Activity**: buckets of talk-seconds per source and for the whole block,
  with a `baseline` (typical for this hour and weekday) for busy-vs-normal.
- **Block**: `id`, plain `name`, `description`, center and width.

## Real vs pending engine work

The engine today gives live channels, status, global listener selection, live
PCM audio and JSON spectrum rows. The rest is defined in
`engine/README.md` § "Frontend needs" and on issue #1. Until the engine
answers those endpoints, the app uses honest in-browser fallbacks in live
mode (a session-only call log derived from events, without audio; frequency
names; activity since the page opened) and a complete typed mock in demo mode
(`?demo`), which tests and screenshots use.

## Prototype decision

Three home screens were built on the real stack against the demo backend and
screenshotted at 390 × 844 (`output/frontend-v2/proto-{a,b,c}.png` on
wigglebutt, Git-ignored):

- **A, sentence + channel list (chosen).** The plain sentence answers "what's
  going on?" before any data; the list reads like a chat app's channels, scales
  from zero to dozens of sources, and one tap on a row listens.
- **B, swimlanes.** One 30-minute lane per source with the live edge on the
  right. It makes busyness vivid, but names lose their "what is it" line and
  lanes need horizontal precision a phone doesn't have. Rejected for the phone
  home screen; its lanes moved into A's rows and its layout became the desktop
  Catch up timeline.
- **C, now-playing hero.** A big card for the loudest source with a big play
  button. It serves one source, falls apart with zero or five talking, and its
  play button duplicates tap-to-listen. Rejected; its hero treatment became the
  expanded player sheet.

A's prototype also showed the activity lanes were wrong: bucketed block
activity split by weight gave every source the same pattern. Lanes are now
drawn from each source's actual calls (start and end times), with a live call
running to the right edge in lamp amber.
