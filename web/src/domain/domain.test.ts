import { describe, expect, it } from "vitest";
import type { EngineChannel } from "@/api/engine";
import type { WireCatalogEntry } from "@/api/needs";
import { healthOf, numberName, rankSources, toSources } from "./adapt";
import { STARTER_CATALOG } from "./starter";
import { prefs, prefsStore } from "@/state/prefs";
import type { Activity } from "./model";
import { busyLine, busyness } from "./summary";

function channel(id: string, patch: Partial<EngineChannel> = {}): EngineChannel {
  const [freq, protocol, slot] = id.split("-");
  return {
    id,
    stream_id: 1,
    frequency_hz: Number(freq),
    protocol,
    slot: Number(slot),
    kind: "voice",
    rf_active: false,
    decoded: true,
    speech_active: false,
    last_heard: null,
    last_rf: null,
    level_db: null,
    audio_seconds: 0,
    sequence: 0,
    in_range: true,
    available: true,
    decoder_admitted: true,
    ...patch,
  };
}

const entry = (patch: Partial<WireCatalogEntry>): WireCatalogEntry => ({
  id: "x",
  kind: "channel",
  channel_ids: [],
  frequency_hz: null,
  slot: null,
  system: null,
  talkgroup_id: null,
  name: "X",
  description: "",
  probable_user: null,
  traffic_kind: null,
  mode: "unknown",
  encryption: "unknown",
  ...patch,
});

describe("toSources", () => {
  it("names unlabelled channels by number, with the DMR line", () => {
    expect(numberName(454_029_223, 2, "dmr")).toBe("454.029 MHz, line 2");
    expect(numberName(454_347_600, 1, "unknown")).toBe("454.348 MHz");
  });

  it("hides the Unknown placeholder once the frequency is identified", () => {
    const sources = toSources(
      [channel("454029223-Unknown-1", { protocol: "Unknown" }), channel("454029223-DMR-2", { speech_active: true })],
      null,
    );
    expect(sources.map((s) => s.id)).toEqual(["ch-454029223-2"]);
    expect(sources[0].channelIds).toEqual(["454029223-DMR-2"]);
    expect(sources[0].talking).toBe(true);
    expect(sources[0].summary).toBe("Unknown");
  });

  it("keeps an Unknown channel on its own: listenable, but RF alone is never talking", () => {
    const [s] = toSources(
      [channel("454347600-Unknown-1", { protocol: "Unknown", rf_active: true, decoded: false, last_rf: 100 })],
      null,
    );
    expect(s.summary).toBe("Unidentified");
    expect(s.listenable.ok).toBe(true);
    expect(s.signal).toBe(true);
    expect(s.talking).toBe(false);
    expect(s.heardVoice).toBe(false);
    // Last heard means voice, not RF.
    expect(s.lastHeardAt).toBeNull();
    expect(s.lastSignalAt).toBe(100_000);
  });

  it("names unlabelled channels from the engine's labels, and never lets a control channel talk", () => {
    const [control] = toSources(
      [
        channel("454206250-Unknown-1", {
          protocol: "Unknown",
          display_name: "Day Wireless (business trunked)",
          label_kind: "control",
          speech_active: true,
          rf_active: true,
        }),
      ],
      null,
    );
    expect(control).toMatchObject({ name: "Day Wireless (business trunked)", named: true, control: true, talking: false });
    expect(control.heardVoice).toBe(false);
    expect(control.listenable.ok).toBe(false);
  });

  it("decides control from the engine's label kind only, never from free text", () => {
    const [voice] = toSources(
      [channel("454000000-DMR-1", { protocol: "DMR", decoded: true, speech_active: true, label_kind: "voice" })],
      [entry({ id: "ushers", name: "Ushers", frequency_hz: 454_000_000, traffic_kind: "Crowd control and ushers" })],
    );
    expect(voice).toMatchObject({ name: "Ushers", control: false, talking: true, heardVoice: true });
    expect(voice.listenable.ok).toBe(true);
  });

  it("applies catalog labels and treats an encrypted carrier as a private call", () => {
    const [s] = toSources(
      [channel("454554500-DMR-1", { rf_active: true })],
      [entry({ id: "private", channel_ids: ["454554500-DMR-1"], name: "Private business radio", encryption: "encrypted", mode: "dmr" })],
    );
    expect(s.name).toBe("Private business radio");
    expect(s.summary).toBe("Private (encrypted)");
    // A carrier alone isn't evidence here; domain/calm.ts counts it only when sustained and calibrated.
    expect(s.talking).toBe(false);
    expect(s.signal).toBe(true);
    expect(s.listenable).toMatchObject({ ok: false });
  });

  it("says analog FM can't be played yet", () => {
    const [s] = toSources(
      [channel("451773100-FM-1")],
      [entry({ id: "hotels", channel_ids: ["451773100-FM-1"], name: "Hotels", mode: "fm" })],
    );
    expect(s.listenable.ok).toBe(false);
  });

  it("ranks followed first (pinned), then last voice heard, ignoring live talking and RF", () => {
    const sources = toSources(
      [
        channel("451000000-Unknown-1", { protocol: "Unknown", last_rf: 200 }),
        channel("452000000-DMR-1", { last_heard: 100 }),
        channel("453000000-DMR-1", { speech_active: true }),
        channel("454000000-DMR-1", { last_heard: 50 }),
      ],
      null,
    );
    const ranked = rankSources(sources, new Set(["ch-454000000-1"])).map((s) => s.id);
    // 453 is talking and 451 has fresh RF, but neither moves a row: only voice heard, then frequency.
    expect(ranked).toEqual(["ch-454000000-1", "ch-452000000-1", "ch-451000000", "ch-453000000-1"]);
  });
});

describe("catalog joins", () => {
  it("names channels from the starter list by frequency, whatever the slot or protocol", () => {
    const sources = toSources(
      [channel("454029223-Unknown-1", { protocol: "Unknown" }), channel("454029223-DMR-2", { speech_active: true })],
      STARTER_CATALOG,
    );
    expect(sources).toHaveLength(1);
    expect(sources[0].name).toBe("Columbia Tower shared radio");
    expect(sources[0].channelIds[0]).toBe("454029223-DMR-2");
    expect(sources[0].listenable.ok).toBe(true);
  });

  it("gives each DMR timeslot its own row when a frequency-only name covers both", () => {
    const sources = toSources(
      [channel("454029223-DMR-1"), channel("454029223-DMR-2", { speech_active: true })],
      STARTER_CATALOG,
    );
    expect(sources.map((s) => [s.id, s.name, s.channelIds[0]])).toEqual([
      ["starter-columbia-shared-line1", "Columbia Tower shared radio, line 1", "454029223-DMR-1"],
      ["starter-columbia-shared-line2", "Columbia Tower shared radio, line 2", "454029223-DMR-2"],
    ]);
  });

  it("leaves out catalog entries that cover nothing currently heard", () => {
    expect(toSources([], STARTER_CATALOG)).toEqual([]);
  });

  it("keeps a label made on an unidentified signal once the engine identifies it", () => {
    const label = entry({ id: "label-ch-454347600", frequency_hz: 454_347_600, name: "Mystery" });
    const before = toSources([channel("454347600-Unknown-1", { protocol: "Unknown" })], [label]);
    expect(before[0].name).toBe("Mystery");
    // Unidentified is still listenable: the engine pins it and plays any voice it identifies.
    expect(before[0].listenable.ok).toBe(true);
    const after = toSources(
      [channel("454347600-Unknown-1", { protocol: "Unknown" }), channel("454347600-DMR-1")],
      [label],
    );
    expect(after.map((s) => s.name)).toEqual(["Mystery"]);
    expect(after[0].channelIds).toEqual(["454347600-DMR-1"]);
    expect(after[0].listenable.ok).toBe(true);
  });

  it("gives each channel to the first entry that covers it (device labels before starter names)", () => {
    const mine = entry({ id: "label-mine", frequency_hz: 452_685_504, name: "Sidewalk crew" });
    const [source] = toSources([channel("452685504-DMR-1")], [mine, ...STARTER_CATALOG]);
    expect(source.name).toBe("Sidewalk crew");
  });

  it("offers analog FM only when the producer says it can play it", () => {
    const fm = [channel("451773100-FM-1")];
    expect(toSources(fm, null)[0].listenable.ok).toBe(false);
    expect(toSources(fm, null, { fmAudio: true })[0].listenable.ok).toBe(true);
  });
});

describe("busy line", () => {
  const now = new Date("2026-10-19T19:30:00").getTime();
  const listening = { state: "listening" as const, detail: "" };

  it("shows nothing until there's a baseline, and nothing while offline", () => {
    expect(busyLine(null, listening, now)).toBeNull();
    expect(busyLine(null, healthOf(undefined, false), now)).toBeNull();
  });

  it("compares the last 15 minutes against the baseline", () => {
    const bucket = 300_000;
    const start = now - 11 * bucket;
    const activity: Activity = {
      bucketMs: bucket,
      start,
      block: [...new Array(9).fill(20), 90, 90, 90],
      sources: {},
      baseline: new Array(12).fill(30),
    };
    expect(busyness(activity, now).level).toBe("busier");
    expect(busyLine(activity, listening, now)).toBe("Busier than usual");
    expect(busyness({ ...activity, block: new Array(12).fill(5) }, now).level).toBe("quieter");
  });
});

describe("health and preferences", () => {
  const base = { mode: "live", state: "receiving", generation: 1, channels: [] } as unknown as Parameters<typeof healthOf>[0];

  it("says plainly whether the radio is working", () => {
    expect(healthOf(base, true).state).toBe("listening");
    expect(healthOf(base, false)).toMatchObject({ state: "offline", detail: expect.stringMatching(/Reconnecting/) });
    expect(healthOf({ ...base!, state: "stopped", mode: "disconnected" }, true).state).toBe("paused");
    expect(healthOf({ ...base!, state: "error", mode: "disconnected", error: "RIQ source disconnected" }, true)).toMatchObject({
      state: "offline",
      raw: "RIQ source disconnected",
    });
  });

  it("shows an intentional receiver stop without claiming it is reconnecting", () => {
    const status = { ...base, mode: "disconnected", state: "stopped",
      receiver: { fresh: true, state: "stopped", reason: "operator" },
      link: { mode: "tcp", state: "waiting", offline_s: 900 } } as unknown as Parameters<typeof healthOf>[0];
    expect(healthOf(status, true)).toEqual({ state: "paused", detail: "Receiving stopped. Start receiving to resume." });
  });

  it("says plainly that the receiver is offline and retrying, and for how long", () => {
    const down = (link: object, receiver: object | null = null) =>
      ({ ...base, state: "error", mode: "disconnected", error: "RIQ source disconnected", link: { mode: "tcp", state: "reconnecting", ...link }, receiver }) as unknown as Parameters<typeof healthOf>[0];
    const short = healthOf(down({ offline_s: 20 }), true);
    expect(short).toMatchObject({ state: "offline", reconnecting: true });
    expect(short.detail).toBe("Reconnecting.");

    // The supervisor's reason is passed on in plain words.
    const busy = healthOf(down({ offline_s: 30 }, { state: "waiting", reason: "lock-held", fresh: true }), true);
    expect(busy.detail).toBe("In use for a test.");
    const lost = healthOf(down({ offline_s: 30 }, { state: "waiting", reason: "unreachable", fresh: true }), true);
    expect(lost.detail).toMatch(/isn't answering/);
    const dead = healthOf(down({ offline_s: 30 }, { state: "running", fresh: false }), true);
    expect(dead.detail).toMatch(/supervisor isn't running/);

    // After ten minutes it is no longer a blip: say how long, and that old activity is on screen.
    const long = healthOf(down({ offline_s: 1500 }, { state: "waiting", reason: "wrong-image", fresh: true }), true);
    expect(long.detail).toBe("Offline for 25 min. Running test software.");
    expect(healthOf(down({ offline_s: 7200 }), true).detail).toContain("Offline for 2 h");

    // A borrow or a clean stop writes its status once; its age must not read as "supervisor dead".
    const borrowed = healthOf(down({ offline_s: 30 }, { state: "borrowed", fresh: false }), true);
    expect(borrowed.detail).toBe("In use for a test.");
    expect(healthOf(down({ offline_s: 30 }, { state: "stopped", fresh: false }), true).detail).toBe("Switched off.");
    // No status from the supervisor at all is not "switched off".
    expect(healthOf(down({ offline_s: 30 }, { state: "unknown", fresh: false }), true).detail).toMatch(/isn't running/);
    // TCP is up but the first CONFIG has not arrived: still reconnecting, not the old "lost its feed" text.
    const connecting = { ...down({ offline_s: 1 }), link: { mode: "tcp", state: "connected" } } as unknown as Parameters<typeof healthOf>[0];
    expect(healthOf(connecting, true)).toMatchObject({ reconnecting: true });
    // The raw diagnostic is the current link error, not a stale engine error.
    const rawErr = healthOf(down({ offline_s: 30, last_error: "cannot reach the receiver at x: timed out" }), true);
    expect(rawErr).toMatchObject({ raw: "cannot reach the receiver at x: timed out" });

    // Back: connected link plus a receiving state is the normal listening state again.
    const back = { ...base, link: { mode: "tcp", state: "connected" } } as unknown as Parameters<typeof healthOf>[0];
    expect(healthOf(back, true).state).toBe("listening");
    // No receiver configured (fixture mode) keeps the old wording.
    const fixture = { ...base, state: "stopped", mode: "disconnected", link: { mode: "fixture", state: "fixture" } } as unknown as Parameters<typeof healthOf>[0];
    expect(healthOf(fixture, true).state).toBe("paused");
  });

  it("keeps follow and mute exclusive, and carries them over when a source is renamed", () => {
    prefs.toggleFollow("a");
    prefs.toggleMute("a");
    expect(prefsStore.get()).toMatchObject({ followed: [], muted: ["a"] });
    prefs.toggleFollow("a");
    expect(prefsStore.get()).toMatchObject({ followed: ["a"], muted: [] });
    prefs.renameSource("a", "label-a");
    expect(prefsStore.get().followed).toEqual(["label-a"]);
  });
});
