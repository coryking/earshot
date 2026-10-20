# Radio monitor UI design

The app parks on one receiver window, a chunk of spectrum up to about 20 MHz wide.
Discovery, the catalog and listening all happen inside that window. Scanning the
whole band is out of scope.

## What people do, ranked

1. **See what's on the air right now.** This happens all the time and without a
   click, so it gets the largest share of the screen.
2. **Listen to a channel or talkgroup with one click.** That pins the listener.
   Silence leaves it armed, and discovery keeps running elsewhere.
3. **Stop, pause or mute audio instantly.** The transport is always on screen and
   bound to the keyboard.
4. **Find a specific channel or talkgroup.** Search through the command palette.
5. **Pick or retune the window.** This is less frequent, but it's the app's frame,
   so the current window is the title of the page and opens its own control.
6. **Check whether the receiver is healthy.** This is ambient: one indicator that
   stays quiet while things are fine. Details sit behind it.
7. **Receiver internals** (decoder budget, forwarded slices, queue, drops,
   continuity): these go in the health drawer or a row's details, never in the
   main view.

## Layout

```
┌ 454.217 MHz ±3 MHz ▾ ──────────── [⌘K Search] ──────── ● Receiving ┐  window title + palette + health
├──────────────────────────────────────────────────────────────────────┤
│ spectrum trace                                                         │  resizable split
│ waterfall (WebGL): zoom/pan, catalog markers, listener band             │
├════════════════════════════ drag ══════════════════════════════════════┤
│ Activity timeline: one row per talkgroup or channel; the last minutes   │
│ run to the live edge on the right. Lit rows sort to the top. Click to   │
│ listen.                                                                 │
├──────────────────────────────────────────────────────────────────────┤
│ ▶ Now playing: Seattle Fire Dispatch   PSERN TG 1234   0:12   ▮▮▮▯   ◀ vol │  transport bar
└──────────────────────────────────────────────────────────────────────┘
```

On a phone, the window title and health dot stay at the top. The timeline becomes
the main list with a compressed activity strip in each row. The waterfall
collapses to a short strip you can expand, and the transport bar is pinned to the
bottom like a mini player.

### Where the patterns come from

- **Activity timeline (Grafana state timeline):** this is the catalog. Each row is
  something you can listen to, and the bars show voice, carrier-only, encrypted,
  and data gaps (hatched) across the last few minutes. It answers "what's live"
  and "what's been busy" in one glance, without replaying audio. It merges the
  catalog list, the activity monitor and the gap indicators.
- **Now-playing bar (Spotify/Apple Music):** one persistent place for the pinned
  target, its state (armed, live, encrypted, out of window), audio level, latency
  and transport. On a phone it's the mini player.
- **Command palette (Linear/Raycast, ⌘K):** jumps to any channel or talkgroup,
  retunes ("454.5"), changes the width, mutes, switches the theme, and opens
  health. Search replaces filter chrome.
- **Window title as a switcher (Figma file name, Linear workspace menu):** the
  current receiver window is the page title. Clicking it opens retune/width
  controls.
- **Discord voice-channel list:** rows light up while someone is talking. A
  talkgroup row shows the talking unit inline.
- **SDR conventions, domain only:** waterfall with a spectrum trace above it
  (SDR++), shading for the listener's band, click-to-listen on the waterfall
  (OpenWebRX), and adjustable dB floor/ceiling with auto-range.
- **Trunking vocabulary (SDRTrunk, rdio-scanner, Broadcastify Calls):** talkgroup
  alpha tags and a lock glyph for encryption. *Hold* semantics are the pin.
  Talkgroups and conventional channels are peers in one list, and both pin the
  same listener.

## Visual system

- Dark first, light supported, and the theme follows the OS until toggled. The
  waterfall stays dark in both themes because it's an image surface.
- Color carries state only. Amber means voice is live. Teal means carrier only.
  Violet with a hatch means encrypted. Red means a fault or gap. Everything else
  is neutral slate. No decorative color, gradients or card shadows.
- One family, Archivo (variable width and weight), with tabular figures for every
  number. Dense rows use a narrower width. The now-playing title uses a wider
  width.
- Sentence-case labels, no uppercase eyebrows, no badges that repeat what a row
  already shows.

## Client-side work

The browser owns rendering: colormap, zoom/pan, averaging, peak hold, the time
axis, formatting, and all interaction state. The engine sends measured dB rows.
The proposed compact binary stream is specified in `engine/README.md`. Until it
lands, the client uses the JSON delta endpoint. The waterfall is a WebGL2 ring
texture, so a new row costs one `texSubImage2D`, and range or zoom changes cost
only a uniform. Spectrum data never passes through React state.

## Trunking

Talkgroups are catalog rows just like channels, so trunking adds rows rather than
a separate screen. Systems and sites, the control channel lock and decode rate
appear in the health drawer and in group headers. The client codes against
`src/lib/trunking.ts`, and an engine without trunking (a 404) simply shows no
talkgroups.
