import { expect, it } from "vitest";
import { historyTicks } from "./ticks";

it("uses round seconds rather than fractional ages of arbitrarily selected rows", () => {
  const times = Array.from({ length: 300 }, (_, i) => 100 - i * 0.0576);
  const ticks = historyTicks(times, 300, 400);
  expect(ticks.map((t) => t.ageSeconds)).toEqual([0, 5, 10, 15]);
  expect(ticks[1].row).toBeCloseTo(5 / 0.0576);
});

it("places ticks using timestamps rather than assuming a fixed row rate", () => {
  expect(historyTicks([100, 99.5, 98, 96, 95], 5, 500)).toEqual([
    { ageSeconds: 0, row: 0 }, { ageSeconds: 1, row: 1 + 1 / 3 },
    { ageSeconds: 2, row: 2 },
    { ageSeconds: 4, row: 3 }, { ageSeconds: 5, row: 4 },
  ]);
});

it("does not label empty history or extend beyond retained visible rows", () => {
  expect(historyTicks([], 300, 400)).toEqual([]);
  expect(historyTicks([1, 0.9], 300, 400)).toEqual([{ ageSeconds: 0, row: 0 }]);
  expect(historyTicks([10, 9, 8, 0], 3, 300).at(-1)?.ageSeconds).toBeLessThanOrEqual(2);
});

it("keeps labels apart across large missing intervals", () => {
  const ticks = historyTicks([100, 99.9, 1, 0], 4, 400);
  for (let i = 1; i < ticks.length; i++)
    expect((ticks[i].row - ticks[i - 1].row) / 4 * 400).toBeGreaterThanOrEqual(60);
});
