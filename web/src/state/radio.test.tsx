import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { describe, expect, it, vi } from "vitest";
import type { EngineChannel, EngineEvent, EngineStatus } from "@/api/engine";
import type { WireCall, WireCatalogEntry } from "@/api/needs";
import type { Backend } from "@/backend/types";
import type { SpectrumBatch } from "@/domain/model";
import { prefsStore } from "@/state/prefs";
import { spectrumMeta, spectrumRows, useSpectrumFeed } from "@/waterfall/feed";
import { BackendProvider, CatalogMigration, RadioSync, localCatalog, useBlocks, useCalls, useCatalog } from "./radio";

vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn() }) }));
const { toast } = await import("sonner");
const { useAlerts } = await import("@/app/alerts");

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
    ...patch,
  };
}

function status(channels: EngineChannel[], patch: Partial<EngineStatus> = {}): EngineStatus {
  return {
    mode: "live",
    state: "receiving",
    source_name: "test",
    center_hz: 454_216_998,
    rate_hz: 6e6,
    bandwidth_hz: 6e6,
    generation: 1,
    channels,
    source: {},
    ...patch,
  } as EngineStatus;
}

/** Today's engine: none of the "frontend needs" endpoints, events on demand. */
function liveEngine(initial: EngineStatus) {
  let emit: (e: EngineEvent) => void = () => {};
  let current = initial;
  const backend = {
    mode: "live",
    status: vi.fn(async () => current),
    events: (onEvent: (e: EngineEvent) => void, onConnection: (open: boolean) => void) => {
      emit = onEvent;
      queueMicrotask(() => onConnection(true));
      return { close() {} };
    },
    calls: vi.fn(async () => null as WireCall[] | null),
    activity: vi.fn(async () => null),
    catalog: vi.fn(async () => null as WireCatalogEntry[] | null),
    blocks: vi.fn(async () => null),
    saveCatalogEntry: vi.fn(async () => false),
    spectrum: vi.fn(),
  } as unknown as Backend & { status: ReturnType<typeof vi.fn>; spectrum: ReturnType<typeof vi.fn> };
  return {
    backend,
    emit: (e: EngineEvent) => act(() => emit(e)),
    setStatus(next: EngineStatus) {
      current = next;
    },
  };
}

function wrapper(backend: Backend) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>
      <BackendProvider backend={backend}>
        <RadioSync />
        <CatalogMigration />
        {children}
      </BackendProvider>
    </QueryClientProvider>
  );
}

const flushBatch = () => act(() => new Promise((r) => setTimeout(r, 80)));

describe("live-mode fallbacks", () => {
  it("builds a session call log, without audio, from speech transitions", async () => {
    const engine = liveEngine(status([channel("454029223-DMR-2")]));
    const { result } = renderHook(() => useCalls(), { wrapper: wrapper(engine.backend) });
    await waitFor(() => expect(engine.backend.status).toHaveBeenCalled());
    await waitFor(() => expect(result.current.loading).toBe(false));
    engine.emit({ type: "channel", generation: 1, channel: channel("454029223-DMR-2", { speech_active: true }) });
    await flushBatch();
    engine.emit({ type: "channel", generation: 1, channel: channel("454029223-DMR-2", { speech_active: false }) });
    await flushBatch();
    await waitFor(() => expect(result.current.calls).toHaveLength(1));
    expect(result.current.recorded).toBe(false);
    expect(result.current.calls[0]).toMatchObject({ sourceId: "starter-columbia-shared", audioUrl: null });
    expect(result.current.calls[0].endedAt).not.toBeNull();
  });

  it("closes a call whose channel vanished (engine restart)", async () => {
    const engine = liveEngine(status([channel("454029223-DMR-2", { speech_active: true })]));
    const { result } = renderHook(() => useCalls(), { wrapper: wrapper(engine.backend) });
    // The first snapshot is the baseline, so open the call with an event.
    await waitFor(() => expect(engine.backend.status).toHaveBeenCalled());
    engine.emit({ type: "channel", generation: 1, channel: channel("454029223-DMR-2", { speech_active: false }) });
    await flushBatch();
    engine.emit({ type: "channel", generation: 1, channel: channel("454029223-DMR-2", { speech_active: true }) });
    await flushBatch();
    await waitFor(() => expect(result.current.calls.some((c) => c.endedAt === null)).toBe(true));
    engine.setStatus(status([]));
    engine.emit({ type: "source_config", generation: 2, source: {} });
    await act(() => new Promise((r) => setTimeout(r, 1200)));
    await waitFor(() => expect(result.current.calls.every((c) => c.endedAt !== null)).toBe(true));
  });

  it("treats a 0 Hz center before CONFIG as unknown, then names the receiver's area", async () => {
    const engine = liveEngine(status([], { center_hz: 0, rate_hz: 0, bandwidth_hz: 0 }));
    const { result } = renderHook(() => useBlocks(), { wrapper: wrapper(engine.backend) });
    await waitFor(() => expect(engine.backend.status).toHaveBeenCalled());
    expect(result.current.current).toBeNull();
    const second = liveEngine(status([]));
    const { result: named } = renderHook(() => useBlocks(), { wrapper: wrapper(second.backend) });
    await waitFor(() => expect(named.current.current?.name).toBe("Business radio, 451–458 MHz"));
    expect(named.current.canChange).toBe(false);
  });

  it("layers names: engine catalog, then device labels it lacks; starter names only without one", async () => {
    const mine: WireCatalogEntry = {
      id: "label-mine",
      kind: "channel",
      channel_ids: [],
      frequency_hz: 452_000_000,
      slot: null,
      system: null,
      talkgroup_id: null,
      name: "Mine",
      description: "",
      probable_user: null,
      traffic_kind: null,
      mode: "unknown",
      encryption: "unknown",
    };
    localCatalog.set([mine]);
    const fallback = liveEngine(status([]));
    const { result: before } = renderHook(() => useCatalog(), { wrapper: wrapper(fallback.backend) });
    await waitFor(() => expect(before.current.onDevice).toBe(true));
    expect(before.current.entries[0].id).toBe("label-mine");
    expect(before.current.entries.some((e) => e.id.startsWith("starter-"))).toBe(true);

    const served = liveEngine(status([]));
    (served.backend.catalog as ReturnType<typeof vi.fn>).mockResolvedValue([{ ...mine, id: "engine-1", name: "Engine" }]);
    (served.backend.saveCatalogEntry as ReturnType<typeof vi.fn>).mockResolvedValue(true);
    const { result: after } = renderHook(() => useCatalog(), { wrapper: wrapper(served.backend) });
    await waitFor(() => expect(after.current.engineEntries).not.toBeNull());
    expect(after.current.entries.some((e) => e.id.startsWith("starter-"))).toBe(false);
    // The device label is uploaded and then forgotten locally.
    await waitFor(() => expect(served.backend.saveCatalogEntry).toHaveBeenCalledWith(mine));
    await waitFor(() => expect(localCatalog.get()).toEqual([]));
  });
});

describe("alerts", () => {
  it("alerts once when a followed source gets busy, not for calls before the page opened", async () => {
    const now = Date.now();
    const call = (id: string, startedAt: number): WireCall => ({
      id,
      source_id: "starter-ambassadors",
      started_at: startedAt / 1000,
      ended_at: startedAt / 1000 + 5,
      encrypted: false,
    });
    const engine = liveEngine(status([channel("452685504-DMR-1")]));
    const calls = engine.backend.calls as ReturnType<typeof vi.fn>;
    calls.mockResolvedValue([call("old1", now - 120_000), call("old2", now - 60_000), call("old3", now - 30_000)]);
    prefsStore.set((p) => ({ ...p, followed: ["starter-ambassadors"], alertFollowed: true }));
    renderHook(() => useAlerts(), { wrapper: wrapper(engine.backend) });
    await waitFor(() => expect(calls).toHaveBeenCalled());
    expect(toast).not.toHaveBeenCalled();
    engine.emit({ type: "call", call: call("n1", now + 1000) });
    engine.emit({ type: "call", call: call("n2", now + 2000) });
    expect(toast).not.toHaveBeenCalled();
    engine.emit({ type: "call", call: call("n3", now + 3000) });
    await waitFor(() => expect(toast).toHaveBeenCalledTimes(1));
    expect(vi.mocked(toast).mock.calls[0][0]).toBe("Downtown ambassadors is busy");
    engine.emit({ type: "call", call: call("n4", now + 4000) });
    await act(() => new Promise((r) => setTimeout(r, 20)));
    expect(toast).toHaveBeenCalledTimes(1);
  });
});

describe("spectrum feed", () => {
  const batch = (generation: number, sequences: number[], center = 454e6, latest?: number): SpectrumBatch => ({
    generation,
    centerHz: center,
    spanHz: 6e6,
    lowDb: -150,
    highDb: 10,
    unit: "dBFS",
    sequence: latest ?? sequences.at(-1) ?? 0,
    rows: sequences.map((sequence) => ({ sequence, time: sequence, bins: new Uint8Array(4) })),
  });

  it("drops the old picture on a retune, ignores empty batches, and starts over after a restart", async () => {
    const engine = liveEngine(status([]));
    const spectrum = engine.backend.spectrum;
    spectrum.mockResolvedValueOnce(batch(1, [1, 2, 3]));
    const { unmount } = renderHook(() => useSpectrumFeed(), { wrapper: wrapper(engine.backend) });
    await waitFor(() => expect(spectrumRows.map((r) => r.sequence)).toEqual([1, 2, 3]));
    const epoch = spectrumMeta.get().epoch;
    // Retune: new generation, sequence keeps counting.
    spectrum.mockResolvedValueOnce(batch(2, [4, 5], 464e6));
    spectrum.mockImplementation(async () => batch(2, [], 464e6, 5));
    await act(() => new Promise((r) => setTimeout(r, 2100)));
    expect(spectrumRows.map((r) => r.sequence)).toEqual([4, 5]);
    expect(spectrumMeta.get().epoch).toBe(epoch + 1);
    // Empty batches (the steady state between rows) change nothing.
    await act(() => new Promise((r) => setTimeout(r, 2100)));
    expect(spectrumRows.map((r) => r.sequence)).toEqual([4, 5]);
    // Engine restart: its sequence starts over below ours, so history is dropped and `after` resets.
    spectrum.mockImplementation(async (after: number) => (after === 0 ? batch(1, [1]) : batch(1, [], 464e6, 0)));
    await act(() => new Promise((r) => setTimeout(r, 2100)));
    expect(spectrumRows.map((r) => r.sequence)).toEqual([1]);
    unmount();
  }, 15_000);
});
