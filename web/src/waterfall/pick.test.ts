import { expect, it } from "vitest";
import { pickSignal } from "./pick";
import { toSources } from "@/domain/adapt";
import { engineChannelSchema } from "@/api/engine";

const station = toSources([engineChannelSchema.parse({ id: "452675000-DMR-1", frequency_hz: 452675000,
  protocol: "DMR", slot: 1, decoded: true, display_name: "Street crews", in_range: true })], null)[0];

it("snaps a missed click to a known channel, including one outside the visible named list", () => {
  expect(pickSignal(452680000, 20000, 452600000, 200000, undefined, [station])).toMatchObject({ source: station, frequencyHz: 452675000 });
});
it("uses the clicked historical row's signal peak, not an arbitrary coarse channel", () => {
  const bins = new Uint8Array(100).fill(10);
  bins[37] = 100;
  expect(pickSignal(452681000, 20000, 452600000, 200000, bins, [station])).toMatchObject({ source: station, frequencyHz: 452675000 });
  expect(pickSignal(452681000, 20000, 452600000, 200000, bins, [])).toMatchObject({ frequencyHz: 452675000, snapped: true });
});
it("rounds empty spectrum to the UHF channel raster and supports exact tuning with Shift", () => {
  expect(pickSignal(452680000, 20000, 452600000, 200000, new Uint8Array(100).fill(10), [])).toEqual({ frequencyHz: 452681250, snapped: true });
  expect(pickSignal(452680000, 20000, 452600000, 200000, undefined, [station], true)).toEqual({ frequencyHz: 452680000, snapped: false });
});
it("does not pull a click across a wide empty frequency range", () => {
  expect(pickSignal(453000000, 800000, 450000000, 20000000, undefined, [station]).source).toBeUndefined();
});

it("never treats an off-raster FFT bin center as a nominal UHF channel", () => {
  const bins = new Uint8Array(512).fill(10);
  bins[298] = 150;
  const pick = pickSignal(464914330, 25000, 450000000, 25600000, bins, []);
  expect(pick.frequencyHz % 6250).toBe(0);
  expect(pickSignal(464914330, 25000, 450000000, 25600000, bins, [], true).frequencyHz).toBe(464914330);
});
