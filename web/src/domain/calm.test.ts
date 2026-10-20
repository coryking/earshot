import { describe, expect, it } from "vitest";
import {
  CARRIER_MIN_MS,
  differentiators,
  groupSources,
  heardAgo,
  HOLD_MS,
  StableOrder,
  TalkTracker,
  TALKING_SHOWN,
} from "./calm";
import type { Source } from "./model";

function src(id: string, patch: Partial<Source> = {}): Source {
  return {
    id,
    kind: "channel",
    name: id,
    named: true,
    summary: "",
    description: "",
    probableUser: null,
    trafficKind: null,
    mode: "dmr",
    encryption: "clear",
    frequencyHz: 454_000_000,
    slot: 1,
    system: null,
    talkgroupId: null,
    channelIds: [`${id}-DMR-1`],
    talking: false,
    signal: false,
    control: false,
    heardVoice: true,
    levelDb: null,
    lastHeardAt: null,
    lastSignalAt: null,
    inRange: true,
    listenable: { ok: true },
    ...patch,
  };
}

const calibrated = { carrierCalibrated: true };
const uncalibrated = { carrierCalibrated: false };

describe("TalkTracker", () => {
  it("talks on decoded voice and holds through short gaps, then lets go", () => {
    const t = new TalkTracker();
    expect([...t.observe([src("a", { talking: true })], 0, uncalibrated).keys()]).toEqual(["a"]);
    // Between replies the engine reports no speech; the source stays put.
    expect(t.observe([src("a")], HOLD_MS - 1, uncalibrated).has("a")).toBe(true);
    expect(t.observe([src("a", { talking: true })], HOLD_MS + 500, uncalibrated).get("a")).toBe(0);
    expect(t.observe([src("a")], 2 * HOLD_MS + 501, uncalibrated).has("a")).toBe(false);
  });

  it("never counts RF alone, flapping RF, or control channels", () => {
    const t = new TalkTracker();
    for (let ms = 0; ms < 20_000; ms += 1000) {
      const talking = t.observe(
        [src("noise", { signal: ms % 2000 === 0, heardVoice: false }), src("ctl", { control: true, talking: true, signal: true })],
        ms,
        calibrated,
      );
      expect(talking.size).toBe(0);
    }
  });

  it("counts an encrypted carrier only when sustained and the detector is calibrated", () => {
    const priv = src("p", { encryption: "encrypted", signal: true });
    const t = new TalkTracker();
    expect(t.observe([priv], 0, calibrated).has("p")).toBe(false);
    expect(t.observe([priv], CARRIER_MIN_MS - 1, calibrated).has("p")).toBe(false);
    expect(t.observe([priv], CARRIER_MIN_MS, calibrated).has("p")).toBe(true);
    // A dropout restarts the clock.
    const flappy = new TalkTracker();
    flappy.observe([priv], 0, calibrated);
    flappy.observe([{ ...priv, signal: false }], 1000, calibrated);
    expect(flappy.observe([priv], CARRIER_MIN_MS + 500, calibrated).has("p")).toBe(false);
    // Uncalibrated (the ARM path): carriers never count.
    const arm = new TalkTracker();
    arm.observe([priv], 0, uncalibrated);
    expect(arm.observe([priv], 10_000, uncalibrated).has("p")).toBe(false);
  });

  it("forgets sources that disappear", () => {
    const t = new TalkTracker();
    t.observe([src("a", { talking: true })], 0, uncalibrated);
    expect(t.observe([], 100, uncalibrated).size).toBe(0);
  });
});

describe("StableOrder", () => {
  it("keeps positions until a re-sort is asked for; new rows join the end", () => {
    const order = new StableOrder();
    expect(order.arrange(["a", "b", "c"], true)).toEqual(["a", "b", "c"]);
    // A fresh ranking would move c to the top; on screen nothing moves.
    expect(order.arrange(["c", "a", "b"], false)).toEqual(["a", "b", "c"]);
    expect(order.arrange(["d", "c", "a", "b"], false)).toEqual(["a", "b", "c", "d"]);
    expect(order.arrange(["d", "a"], false)).toEqual(["a", "d"]);
    expect(order.arrange(["d", "a"], true)).toEqual(["d", "a"]);
  });
});

describe("differentiators", () => {
  it("tells apart rows that share a name, and leaves unique names alone", () => {
    const labels = differentiators([
      src("s1", { name: "Day Wireless", slot: 1, frequencyHz: 454_106_250 }),
      src("s2", { name: "Day Wireless", slot: 2, frequencyHz: 454_106_250 }),
      src("f1", { name: "Hotel", mode: "fm", slot: null, frequencyHz: 451_000_000 }),
      src("f2", { name: "Hotel", mode: "fm", slot: null, frequencyHz: 452_000_000 }),
      src("solo", { name: "Solo" }),
    ]);
    expect(labels.get("s1")).toBe("Line 1");
    expect(labels.get("s2")).toBe("Line 2");
    expect(labels.get("f1")).toBe("451.000 MHz");
    expect(labels.get("f2")).toBe("452.000 MHz");
    expect(labels.has("solo")).toBe(false);
  });
});

describe("groupSources", () => {
  it("pins at most a few talkers in start order and folds noise into other activity", () => {
    const sources = [
      src("a", { frequencyHz: 1 }),
      src("b", { frequencyHz: 2 }),
      src("c", { frequencyHz: 3 }),
      src("d", { frequencyHz: 4 }),
      src("noise", { heardVoice: false, signal: true, frequencyHz: 5 }),
      src("ctl", { control: true, heardVoice: false, frequencyHz: 0 }),
      src("starred", { heardVoice: false, frequencyHz: 6 }),
      src("hush", { frequencyHz: 7 }),
    ];
    const talking = new Map([
      ["c", 10],
      ["a", 20],
      ["b", 30],
      ["d", 40],
    ]);
    const order = ["starred", "d", "c", "b", "a", "hush", "noise", "ctl"];
    const g = groupSources(sources, talking, order, new Set(["starred"]), new Set(["hush"]));
    expect(g.talking.map((s) => s.id)).toEqual(["c", "a", "b"].slice(0, TALKING_SHOWN));
    expect(g.moreTalking).toBe(1);
    expect(g.channels.map((s) => s.id)).toEqual(["starred", "d", "c", "b", "a"]);
    expect(g.other.map((s) => s.id)).toEqual(["ctl", "noise"]);
    expect(g.muted.map((s) => s.id)).toEqual(["hush"]);
  });
});

it("says last heard in minutes, not seconds", () => {
  expect(heardAgo(null, 0)).toBeNull();
  expect(heardAgo(0, 59_000)).toBe("Just now");
  expect(heardAgo(0, 61_000)).toBe("1 min ago");
  expect(heardAgo(0, 3 * 3600_000)).toBe("3 hr ago");
});
