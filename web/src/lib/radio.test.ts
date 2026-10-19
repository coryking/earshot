import { describe, it, expect } from "vitest";
import { fallbackChannel } from "./radio";
import type { Channel } from "./radio";
const make = (id: string, active = true): Channel => ({
  id,
  frequency_hz: 454000000,
  protocol: "DMR",
  kind: "voice",
  rf_active: true,
  decoded: true,
  speech_active: active,
  audio_seconds: 1,
  sequence: 1,
  in_range: true,
  available: true,
});
describe("pinned monitor selection", () => {
  it("keeps an idle pinned slot selected", () =>
    expect(fallbackChannel([make("a", false), make("b")], "a")).toBe("a"));
  it("retains an out-of-range frequency visibly rather than claiming it is monitored", () =>
    expect(
      fallbackChannel(
        [{ ...make("a"), available: false, in_range: false }, make("b")],
        "a",
      ),
    ).toBe("a"));
  it("chooses recent active voice when a selected stream is truly removed", () =>
    expect(
      fallbackChannel(
        [
          { ...make("a"), last_heard: 1 },
          { ...make("b"), last_heard: 5 },
        ],
        "gone",
      ),
    ).toBe("b"));
  it("does not fall to a control carrier", () =>
    expect(
      fallbackChannel([{ ...make("control"), kind: "data" }], "gone"),
    ).toBeNull());
});
