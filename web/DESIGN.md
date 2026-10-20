# Frontend design

Source of truth: [`../docs/PRODUCT.md`](../docs/PRODUCT.md). This file maps that
spec onto screens, gestures and data. Where the two disagree, the spec wins.

## Current workspace: spectrum exploration

The household radio in PRODUCT.md remains the destination. During receiver and
protocol discovery, Explore is the default workspace on every screen size.

- The waterfall fills the workspace. A pointer or touch selects a frequency;
  a single hover preview replaces persistent catalog-name overlays.
- One tuning strip owns frequency entry, audio mode, playback status, mute and
  volume. Receiving remains a separate shared receiver control in the header.
- Channels opens a searchable frequency list. Selecting a row tunes and closes
  it. Signal opens details about the selection; neither list occupies a
  permanent column.
- The band heading opens receiver-window navigation. Its values come from
  receiver readback, including synthesizer rounding.
- Save IQ sits below the waterfall with sampled bandwidth, buffered seconds
  and rolling/paused state. The actual physical slice is shaded on the plot.
  FM audio export is a separate action when audio has been buffered.
- Signal details separate RF activity, protocol decoding and speech. Catalog
  names are annotations, not decoder evidence. Non-audio results must be typed
  messages or events, with text only when an actual text payload was decoded.
  The engine does not yet expose the offline NXDN SCCH or Capacity Plus fields
  documented in [the IQ analysis](../docs/iq-analysis.md); the frontend does not
  pretend these are live results.

## Household listener design (future workspace)

The following describes the household flow, retained as the destination rather
than a second set of controls around Explore.

The listener is a spouse or a sibling: no radio vocabulary, on a phone,
often one-handed, sometimes with the screen locked. Everything on the default
screens is written for them. Radio terms (MHz, slot, DMR, gain) appear only as
secondary detail or in the operator view.

## Ranked actions, mapped to screens and gestures

| Rank | Action | Where | Gesture |
| --- | --- | --- | --- |
| 1 | See what's happening now | **Now** tab (home). Titled by the listening area, then the live strip, a small **Recent activity** area (at most three, "+N" beside its heading for more), and the channel list in a stable order. | Open the app. Activity is separate from the speaker's playback state. |
| 2 | Tap to listen | Any channel row or recent activity card, any call in Catch up, a marked trace in the desktop waterfall. The **player bar** docks above the tab bar with labeled controls. | One tap holds that channel and slot through silence. **Stop listening** releases it; **Mute speaker** keeps it held. Volume is visible beside mute. There is no automatic scanning. |
| 3 | Catch up on what I missed | **Catch up** tab: recent calls newest first, grouped by "Just now / Earlier this hour / Today", each with a play button and duration. | Tap a call to play it. Playback continues down the list (autoplay next) like a podcast queue, so a listener can catch up hands-free. |
| 4 | Follow or hide a source | Star and **Hide activity** in the source sheet and expanded player. Followed sources pin to the top of Now; hidden ones move to a collapsed group and never alert. | Tap the row's ⓘ button for details. Hiding activity is separate from muting the speaker. |
| 5 | Get alerted | Bell in the header opens the alerts sheet: "A channel I follow gets busy" (three or more calls within ten minutes) and "It's much busier than normal" (the area moves into busier-than-usual). Only activity since the page opened counts, with a 30-minute cooldown. In-page notifications today; closed-app push needs the engine. | Toggle switches. The bell turns amber when alerts are on. |
| 6 | Change block | Tap the area name at the top ("Downtown business radio"). A sheet lists the blocks in plain terms and asks for confirmation. With only one block (today's live engine) the name is plain text, with no chevron and no sheet. | Deliberately two taps plus a confirm. Never a swipe. |
| 7 | Is it working? / What is this? | The status dot next to the block name ("Receiving", "Paused", "Not connected"); tap it for a plain explanation. Each source sheet has "What is this?": probable user, kind of traffic, and, for encrypted sources, why you can't hear them. | Tap. |

Now includes frequency entry and the selected channel’s live status. Gain,
bandwidth and decoder controls live in the **operator view** (`#/operator`), reached from the Radio tab's "Operator tools"
row and the command palette. It is not hidden for security (the tailnet is the
boundary); it's out of the way so a listener doesn't wander into it.

The header also has **Start receiving / Stop receiving**. This changes the
supervisor's desired receive state and waits for observed receiver status.
Stop restores the receiver's settings and keeps reception off until Start,
including across supervisor restarts. Errors and unavailable control are
visible. Stopping reception is separate from this browser's listening selection.

## Mobile-first layout

Phone portrait is the design target (390 × 844). Everything a listener needs
is reachable with the right thumb.

```
┌──────────────────────────────┐
│ ● Receiving                🔔│  header: status (tap = status sheet), alerts
│ Downtown business radio ⌄    │  title: the area (tap = block sheet); never changes with data
│ Busier than usual            │  fixed-height line, reserved once there's a baseline
│ ▁▂▅▇▅▂▁▁▁▂▃▁▁▁▁▆█▆▁▁▁▁▁▁▂▁ │  live strip: labels on who's talking / playing; not a control
│ Talking now              +2  │
│ ┌ Front desk        ▮▮▮  ⓘ ┐ │  at most three cards, in the order they started
├──────────────────────────────┤
│ Channels                     │
│ ★ Front desk          ▮▮▮ ⓘ │  followed first, then last voice heard; frozen while shown
│   Warehouse crew  2 min ago ⓘ│  30-min call lane under each row
│   Day Wireless   1 min ago  ⓘ│  each row shows frequency, mode and DMR slot
│ › Other activity (28)        │  no decoded voice: strongest signal first, with bars
├──────────────────────────────┤
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
- **Few words.** Rows carry a name, when a voice was last heard (in minutes),
  or the level meter, and the 30-minute call lane. Every channel identity shows
  its frequency, mode and DMR slot below the name. The selected row also shows
  whether audio is starting, waiting, playing or muted. Rows in Other
  activity show signal bars, relative to the strongest in the group; they are
  listenable like any row (analog FM through the engine's pin path).
- **Calm.** "Talking" means decoded voice (DMR/NXDN voice with sync, FM held
  above the squelch), never RF energy, held 5 s past the last evidence. An
  encrypted source counts by carrier only after 2 s on a calibrated detector
  (not the ARM path). Control channels never talk. The view samples the engine
  once a second and re-renders only when something visible changed. The list
  order is frozen while shown and re-ranks on open (once the engine's names
  have arrived), on follow or mute, and on returning after 30 s away. Only
  listed rows are ranked, so a row that becomes listed joins the end. Other
  activity is ordered the same way by recent signal strength. The talking
  tracker sees every engine update, so a short key-up still counts; only the
  render is limited to once a second. The Other group starts open when there
  are no voice channels yet. The leaving Talking now card stays mounted while
  it folds. Talking now cards fold in
  and out; `prefers-reduced-motion` turns that off. Logic: `src/domain/calm.ts`.
- **Channel identity.** `ChannelIdentity` renders the name, frequency, mode and
  slot in lists, sheets, players, recordings and waterfall labels.

## Desktop and landscape

Desktop gains space; it does not gain new concepts. At ≥ 1024 px wide (and in
phone landscape) the layout becomes two resizable panes:

```
┌──────────────────────┬───────────────────────────────────────────────┐
│ ● Downtown business  │  selected channel + frequency entry          │
│ Two channels talking │  source markers along the top edge            │
│ ─────────────────────│───────────────────────────────────────────────│
│ source list          │  waterfall history                           │
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
| `Enter` or `Space` | Tune to the focused source; stays selected |
| `Esc` | Stop listening (or close the open sheet) |
| `m` | Mute or unmute |
| `f` | Follow or unfollow the focused source |
| `1` `2` `3` | Now, Catch up, Radio |
| `?` | Show shortcuts |

## The waterfall

A supporting view, as the spec says. Rendered entirely in the browser:

- **Phone portrait:** a thin live strip (~56 px, about 20 seconds of history)
  under the title. It proves the block is alive and is a picture, not a
  control (the playing source is marked even if it sits under Other activity): channels sit a few pixels apart, and on this receiver most bright
  lines are data, control channels or noise that can't be heard, so "tap a
  bright line" led people to nothing. Instead, the trace of each source
  talking now gets an amber guide line and name label, and the playing source a
  blue one, tying the strip to the list, which is the way in.
- **Landscape and desktop:** the full view, a few minutes of history, with
  hover preview and click-to-tune. Picking uses the historical row under the
  pointer and snaps to a nearby peak or known channel, capped at 100 kHz. Shift
  bypasses snapping. Unnamed signals open an editable frequency form.
- **Time axis:** whole-second ticks on a 1/2/5 scale, positioned from the
  rows' actual timestamps. Amber marks recent activity; blue marks the held
  channel, including while the speaker is muted or waiting for voice.
- **Capture:** the tuning panel shows buffered sample-seconds and the maximum,
  rolling/paused state, and the physical slice's sampled frequency range.
  **Download IQ snapshot** saves the current server buffer as SigMF, before
  host channel filtering and FM demodulation. It is separate from **Save FM
  audio WAV**, which saves demodulated audio from this browser's rolling buffer.
  Stop and retune details are available below the capture summary.
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
| channel with no label | its number, the way a phone shows an unknown caller: "454.029 MHz", with mode and DMR slot below the name |
| `protocol: DMR / NXDN48` | frequency plus mode and slot, e.g. "DMR · Slot 2" |
| `protocol: Unknown` | "not identified yet" |
| analog FM (catalog, later engine) | "voice" |
| `speech_active` (held 5 s) | a Talking now card plus a level meter |
| `rf_active` without decoded voice, or `label_kind: control` (the engine's label kind only, never free text) | folded into "Other activity (N)", still playable unless it is a control channel |
| encrypted (catalog/trunking) | "Private: encrypted, can't be heard" with an explanation in "What is this?" |
| `/api/status` state `receiving` | "Receiving" (green), `stopped` "Paused", `error` or disconnected "Not connected" (with the error in plain words and the raw message under Details) |
| center / bandwidth | a block name: "Downtown business radio (454 MHz)" |
| talkgroup (trunking, next) | the talkgroup label, grouped under its system: "Metro buses" in "King County Metro" |

## Operator view

`#/operator`. Technical controls and raw state for Cory:

- Engine status: state, error, generation, decoder capacity and use, queue
  depth, drops, processed batches, last samples, source position, continuity.
- Source config (the CONFIG JSON as read back from the producer) and a form to
  set center/rate/bandwidth via `POST /api/source`, which reports 501 plainly
  when the producer can't do it.
- Receiver controls shared with the header (`POST /api/receiver`). Start and
  stop wait for observed state; stop includes restoration readback. The
  listener's **Stop listening** only clears its selection.
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
- **Slack:** a stable channel list where activity bolds a row instead of
  moving it.

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
- **The one bold thing:** the amber Talking now cards: the only warm colour on
  the screen, answering "who's talking?" at a glance.

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
answers those endpoints, live mode uses honest in-browser fallbacks:

- **Names:** labels saved on this device, then starter names from the RF
  research (`src/domain/starter.ts`), matched by frequency within 3 kHz.
  Anything unmatched shows its number. When the engine gains a catalog, the
  device labels are uploaded and the starter list steps aside.
- **Catch up:** a session-only call log built from speech transitions, with no
  audio.
- **Busy vs normal:** activity since the page opened; the line stays blank
  until there is a baseline.

Demo mode (`?demo`) runs everything on a typed mock, which tests and
screenshots use; it's badged "Demo" and loaded only on request.

## Shared listener selection

The engine plays one channel for every browser. The player only clears a
selection it made, quietly re-selects when the engine releases the selection
(restart, someone else stopping), and shows "Switched to … elsewhere" with
"Switch back" only when another channel was chosen.
Per-client listening is Frontend needs #1.

## Spectrum feed

The spectrum rows live in a module-level ring buffer (`src/waterfall/feed.ts`),
not TanStack Query: they arrive incrementally by `after=` cursor and are read
by the WebGL renderer, so there's nothing to cache or refetch. The feed pauses
while the page is hidden and starts the picture over on a retune or engine
restart.

## Not yet verified

- **Lock-screen playback on a real phone.** Live audio on touch devices goes
  through an `<audio>` element fed by a MediaStream, with Media Session
  metadata and controls, which is the route iOS Safari and Android Chrome keep
  alive when locked. It has only been exercised in desktop Chromium. If a
  browser refuses that route, the player falls back to the speakers and the
  expanded player says the screen has to stay on. Acceptance test: start a
  live channel on an iPhone and an Android phone, lock the screen, and confirm
  audio continues for two minutes and the lock-screen stop works.

## Prototype decision

Three home screens were built on the real stack against the demo backend and
screenshotted at 390 × 844 (`output/frontend-v2/proto-{a,b,c}.png` on
wigglebutt, Git-ignored):

- **A, sentence + channel list (chosen; the sentence was later dropped for a
  stable area title, because a changing headline reflowed the page).** The plain sentence answers "what's
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
