import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { usePresence } from "./SourceList";

const item = (id: string) => ({ id });

describe("usePresence", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("keeps a leaver in the very render it drops out of, then removes it", () => {
    const { result, rerender } = renderHook(({ items }) => usePresence(items), {
      initialProps: { items: [item("a"), item("b")] },
    });
    rerender({ items: [item("a")] });
    expect(result.current.map((r) => [r.item.id, r.leaving])).toEqual([
      ["a", false],
      ["b", true],
    ]);
    act(() => void vi.advanceTimersByTime(319));
    expect(result.current).toHaveLength(2);
    act(() => void vi.advanceTimersByTime(2));
    expect(result.current.map((r) => r.item.id)).toEqual(["a"]);
  });

  it("still removes the leaver when the list changes again before the timer fires", () => {
    const { result, rerender } = renderHook(({ items }) => usePresence(items), {
      initialProps: { items: [item("a"), item("b")] },
    });
    rerender({ items: [item("a")] });
    act(() => void vi.advanceTimersByTime(100));
    rerender({ items: [item("a")] });
    rerender({ items: [item("a"), item("c")] });
    act(() => void vi.advanceTimersByTime(400));
    expect(result.current.map((r) => r.item.id)).toEqual(["a", "c"]);
  });
});
