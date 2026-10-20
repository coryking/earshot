import { createContext, useContext, useEffect, useMemo, type ReactNode } from "react";
import { useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import type { EngineChannel, EngineStatus } from "@/api/engine";
import type { WireActivity, WireCall, WireCatalogEntry } from "@/api/needs";
import type { Backend } from "@/backend/types";
import { healthOf, toSources } from "@/domain/adapt";
import type { Activity, Block, Call, Source } from "@/domain/model";
import { createStore, local, useStore } from "@/lib/store";

const BackendContext = createContext<Backend | null>(null);

export function BackendProvider({ backend, children }: { backend: Backend; children: ReactNode }) {
  return <BackendContext.Provider value={backend}>{children}</BackendContext.Provider>;
}

export function useBackend(): Backend {
  const backend = useContext(BackendContext);
  if (!backend) throw new Error("BackendProvider missing");
  return backend;
}

export const keys = {
  status: ["status"] as const,
  catalog: ["catalog"] as const,
  blocks: ["blocks"] as const,
  calls: ["calls"] as const,
  activity: ["activity"] as const,
  supports: ["supports"] as const,
};

/** Live connection to /api/events; spectrum sequence for the waterfall to poll on. */
export const connectionStore = createStore({ events: false, spectrumSequence: 0 });

// ---------------------------------------------------------------------------
// Session fallbacks: used only while the engine lacks /api/calls, /api/activity
// and /api/catalog. They are honest about being session-only and audio-less.

interface SessionCall {
  id: string;
  channelId: string;
  startedAt: number;
  endedAt: number | null;
}
export const sessionCalls = createStore<SessionCall[]>([]);
const SESSION_BUCKET = 60_000;
export const sessionActivity = createStore<{ start: number; byChannel: Record<string, number[]> }>({
  start: Math.floor(Date.now() / SESSION_BUCKET) * SESSION_BUCKET,
  byChannel: {},
});

function recordTransition(previous: EngineChannel | undefined, next: EngineChannel, now: number) {
  const was = previous?.speech_active === true;
  const is = next.speech_active === true;
  if (!was && is) {
    sessionCalls.set((calls) =>
      [{ id: `session-${next.id}-${now}`, channelId: next.id, startedAt: now, endedAt: null }, ...calls].slice(0, 500),
    );
  } else if (was && !is) {
    let started: number | null = null;
    sessionCalls.set((calls) =>
      calls.map((c) => {
        if (c.channelId !== next.id || c.endedAt !== null) return c;
        started = c.startedAt;
        return { ...c, endedAt: now };
      }),
    );
    if (started !== null) {
      const begin: number = started;
      sessionActivity.set((a) => {
        const index = Math.floor((begin - a.start) / SESSION_BUCKET);
        const series = [...(a.byChannel[next.id] ?? [])];
        while (series.length <= index) series.push(0);
        series[index] += (now - begin) / 1000;
        return { ...a, byChannel: { ...a.byChannel, [next.id]: series } };
      });
    }
  }
}

const LOCAL_CATALOG = "radio.catalog.local.v1";
export const localCatalog = createStore<WireCatalogEntry[]>(local.read(LOCAL_CATALOG, []));
localCatalog.subscribe(() => local.write(LOCAL_CATALOG, localCatalog.get()));

// ---------------------------------------------------------------------------

/** Apply a burst of channel events in one cache update (one render), newest wins. */
function applyChannels(client: QueryClient, updates: EngineChannel[]) {
  client.setQueryData<EngineStatus>(keys.status, (status) => {
    if (!status) return status;
    const channels = [...status.channels];
    for (const channel of updates) {
      const index = channels.findIndex((c) => c.id === channel.id);
      if (index >= 0) channels[index] = { ...channels[index], ...channel };
      else channels.push(channel);
    }
    return { ...status, channels };
  });
}

function applyCall(client: QueryClient, call: WireCall) {
  client.setQueryData<WireCall[] | null>(keys.calls, (calls) => {
    if (!calls) return calls;
    const index = calls.findIndex((c) => c.id === call.id);
    if (index < 0) return [call, ...calls];
    const next = [...calls];
    next[index] = call;
    return next;
  });
}

/** Keeps the status snapshot live from /api/events. Mount once. */
export function RadioSync() {
  const backend = useBackend();
  const client = useQueryClient();
  // Diff every status snapshot (event-patched or refetched) so a refetch that
  // races an event can't hide a call starting or ending.
  useEffect(() => {
    let seen = new Map<string, EngineChannel>();
    const diff = () => {
      const status = client.getQueryData<EngineStatus>(keys.status);
      if (!status) return;
      const now = Date.now();
      const next = new Map(status.channels.map((c) => [c.id, c]));
      for (const channel of status.channels) recordTransition(seen.get(channel.id), channel, now);
      seen = next;
    };
    diff();
    return client.getQueryCache().subscribe((event) => {
      if (event.type === "updated" && event.query.queryKey[0] === keys.status[0]) diff();
    });
  }, [client]);
  useEffect(() => {
    let pending: EngineChannel[] = [];
    let flush: ReturnType<typeof setTimeout> | undefined;
    const connection = backend.events(
      (event) => {
        switch (event.type) {
          case "channel":
            pending.push(event.channel);
            flush ??= setTimeout(() => {
              applyChannels(client, pending);
              pending = [];
              flush = undefined;
            }, 50);
            break;
          case "call":
            applyCall(client, event.call);
            break;
          case "spectrum_available":
            connectionStore.set((c) => ({ ...c, spectrumSequence: event.sequence }));
            break;
          case "source_config":
          case "source_state":
          case "source_gap":
          case "gap":
            void client.invalidateQueries({ queryKey: keys.status });
            void client.invalidateQueries({ queryKey: keys.blocks });
            break;
          case "heartbeat":
            break;
        }
      },
      (open) => {
        connectionStore.set((c) => ({ ...c, events: open }));
        if (open) void client.invalidateQueries({ queryKey: keys.status });
      },
    );
    return () => {
      clearTimeout(flush);
      connection.close();
    };
  }, [backend, client]);
  return null;
}

export function useStatus() {
  const backend = useBackend();
  return useQuery({
    queryKey: keys.status,
    queryFn: () => backend.status(),
    refetchInterval: 15_000,
    retry: 1,
  });
}

export function useCatalog() {
  const backend = useBackend();
  const query = useQuery({ queryKey: keys.catalog, queryFn: () => backend.catalog(), staleTime: 60_000 });
  const fallback = useStore(localCatalog);
  const engineHasCatalog = query.data !== null && query.data !== undefined;
  return { entries: engineHasCatalog ? query.data! : fallback, onDevice: query.data === null };
}

export function useSources(): Source[] {
  const { data } = useStatus();
  const { entries } = useCatalog();
  return useMemo(() => toSources(data?.channels ?? [], entries), [data?.channels, entries]);
}

export function useHealth() {
  const status = useStatus();
  const { events } = useStore(connectionStore);
  return healthOf(status.isError ? undefined : status.data, events);
}

function toCall(call: WireCall, backend: Backend): Call {
  return {
    id: call.id,
    sourceId: call.source_id,
    channelId: call.channel_id ?? null,
    startedAt: call.started_at * 1000,
    endedAt: call.ended_at ? call.ended_at * 1000 : null,
    encrypted: call.encrypted,
    audioUrl: call.audio_url ? (backend.resolveAudio?.(call.audio_url) ?? call.audio_url) : null,
    talker: call.talker ?? null,
    transcript: call.transcript ?? null,
  };
}

/** Recent calls. `recorded` is false while calls come from this browser session only. */
export function useCalls(): { calls: Call[]; recorded: boolean; loading: boolean } {
  const backend = useBackend();
  const sources = useSources();
  const query = useQuery({
    queryKey: keys.calls,
    queryFn: () => backend.calls({ limit: 200 }),
    refetchInterval: (q) => (q.state.data === null ? false : 60_000),
  });
  const session = useStore(sessionCalls);
  return useMemo(() => {
    if (query.data) return { calls: query.data.map((c) => toCall(c, backend)), recorded: true, loading: false };
    const owner = new Map<string, string>();
    sources.forEach((s) => s.channelIds.forEach((id) => owner.set(id, s.id)));
    return {
      calls: session.map((c) => ({
        id: c.id,
        sourceId: owner.get(c.channelId) ?? c.channelId,
        channelId: c.channelId,
        startedAt: c.startedAt,
        endedAt: c.endedAt,
        encrypted: false,
        audioUrl: null,
        talker: null,
        transcript: null,
      })),
      recorded: false,
      loading: query.isLoading,
    };
  }, [query.data, query.isLoading, session, sources, backend]);
}

function toActivity(a: WireActivity): Activity {
  return {
    bucketMs: a.bucket_s * 1000,
    start: a.start * 1000,
    block: a.block,
    sources: a.sources,
    baseline: a.baseline ?? null,
  };
}

/** Talk-time history. Falls back to activity since this page opened, with no baseline. */
export function useActivity(): { activity: Activity | null; learned: boolean } {
  const backend = useBackend();
  const sources = useSources();
  const query = useQuery({
    queryKey: keys.activity,
    queryFn: () => backend.activity(),
    refetchInterval: (q) => (q.state.data === null ? false : 60_000),
  });
  const session = useStore(sessionActivity);
  return useMemo(() => {
    if (query.data) return { activity: toActivity(query.data), learned: true };
    const owner = new Map<string, string>();
    sources.forEach((s) => s.channelIds.forEach((id) => owner.set(id, s.id)));
    const bySource: Record<string, number[]> = {};
    const length = Math.max(1, Math.ceil((Date.now() - session.start) / SESSION_BUCKET));
    const block = new Array(length).fill(0);
    for (const [channel, series] of Object.entries(session.byChannel)) {
      const id = owner.get(channel) ?? channel;
      const target = (bySource[id] ??= new Array(length).fill(0));
      series.forEach((v, i) => {
        target[i] = (target[i] ?? 0) + v;
        block[i] = (block[i] ?? 0) + v;
      });
    }
    return {
      activity: { bucketMs: SESSION_BUCKET, start: session.start, block, sources: bySource, baseline: null },
      learned: false,
    };
  }, [query.data, session, sources]);
}

/** Blocks from the engine, or the one the receiver reports, unnamed. */
export function useBlocks(): { blocks: Block[]; current: Block | null; canChange: boolean } {
  const backend = useBackend();
  const status = useStatus();
  const query = useQuery({ queryKey: keys.blocks, queryFn: () => backend.blocks(), staleTime: 60_000 });
  return useMemo(() => {
    if (query.data) {
      const blocks = query.data.blocks.map((b) => ({
        id: b.id,
        name: b.name,
        description: b.description,
        centerHz: b.center_hz,
        spanHz: b.span_hz,
      }));
      return { blocks, current: blocks.find((b) => b.id === query.data?.current_id) ?? null, canChange: true };
    }
    const center = status.data?.center_hz ?? null;
    const span = status.data?.bandwidth_hz ?? status.data?.rate_hz ?? null;
    if (center === null || span === null) return { blocks: [], current: null, canChange: false };
    const lowMHz = Math.floor((center - span / 2) / 1e6);
    const highMHz = Math.ceil((center + span / 2) / 1e6);
    const current: Block = {
      id: "receiver",
      name: `Business radio, ${lowMHz}–${highMHz} MHz`,
      description: "The stretch of the airwaves the receiver is set to.",
      centerHz: center,
      spanHz: span,
    };
    return { blocks: [current], current, canChange: false };
  }, [query.data, status.data]);
}

export function useSupports() {
  const backend = useBackend();
  return useQuery({ queryKey: keys.supports, queryFn: () => backend.supports(), staleTime: 300_000 });
}
