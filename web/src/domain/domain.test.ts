import { describe, expect, it } from "vitest";
import type { EngineChannel } from "@/api/engine";
import type { WireCatalogEntry } from "@/api/needs";
import { healthOf, numberName, rankSources, toSources } from "./adapt";
import type { Activity } from "./model";
import { busyness, headline } from "./summary";

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
    expect(sources.map((s) => s.id)).toEqual(["454029223-DMR-2"]);
    expect(sources[0].talking).toBe(true);
    expect(sources[0].summary).toBe("Unknown business radio");
  });

  it("keeps an Unknown channel on its own, as not listenable", () => {
    const [s] = toSources([channel("454347600-Unknown-1", { protocol: "Unknown", rf_active: true })], null);
    expect(s.summary).toBe("Unidentified signal");
    expect(s.listenable.ok).toBe(false);
    expect(s.signal).toBe(true);
  });

  it("applies catalog labels and treats an encrypted carrier as a private call", () => {
    const [s] = toSources(
      [channel("454554500-DMR-1", { rf_active: true })],
      [entry({ id: "private", channel_ids: ["454554500-DMR-1"], name: "Private business radio", encryption: "encrypted", mode: "dmr" })],
    );
    expect(s.name).toBe("Private business radio");
    expect(s.summary).toBe("Private (encrypted)");
    expect(s.talking).toBe(true);
    expect(s.listenable).toMatchObject({ ok: false });
  });

  it("says analog FM can't be played yet", () => {
    const [s] = toSources(
      [channel("451773100-FM-1")],
      [entry({ id: "hotels", channel_ids: ["451773100-FM-1"], name: "Hotels", mode: "fm" })],
    );
    expect(s.listenable.ok).toBe(false);
  });

  it("ranks talking, then followed, then listenable", () => {
    const sources = toSources(
      [
        channel("451000000-Unknown-1", { protocol: "Unknown", last_rf: 200 }),
        channel("452000000-DMR-1", { last_heard: 100 }),
        channel("453000000-DMR-1", { speech_active: true }),
        channel("454000000-DMR-1", { last_heard: 50 }),
      ],
      null,
    );
    const ranked = rankSources(sources, new Set(["454000000-DMR-1"])).map((s) => s.id);
    expect(ranked).toEqual(["453000000-DMR-1", "454000000-DMR-1", "452000000-DMR-1", "451000000-Unknown-1"]);
  });
});

describe("headline", () => {
  const now = new Date("2026-10-19T19:30:00").getTime();
  const listening = { state: "listening" as const, detail: "" };

  it("names a single talker and admits when it doesn't know normal yet", () => {
    const sources = toSources([channel("452685504-DMR-1", { speech_active: true })], null);
    expect(headline(sources, null, listening, now)).toEqual({
      lead: "452.686 MHz, line 1 is talking.",
      context: "Still learning what normal sounds like here.",
    });
  });

  it("says when it was last quiet", () => {
    const sources = toSources([channel("452685504-DMR-1", { last_heard: (now - 120_000) / 1000 })], null);
    expect(headline(sources, null, listening, now).lead).toBe("Quiet right now. 452.686 MHz, line 1 was last, 2 min ago.");
  });

  it("reports an offline radio plainly", () => {
    const health = healthOf(undefined, false);
    expect(headline([], null, health, now).lead).toBe("The radio isn't connected.");
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
    expect(headline([], activity, listening, now).context).toBe("Busier than usual for a Monday evening.");
    expect(busyness({ ...activity, block: new Array(12).fill(5) }, now).level).toBe("quieter");
  });
});
