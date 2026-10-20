import { createContext, useContext, useEffect, useMemo, type ReactNode } from "react";
import { useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import type { EngineChannel, EngineStatus } from "@/api/engine";
import { catalogSchema, type WireActivity, type WireCall, type WireCatalogEntry } from "@/api/needs";
import type { Backend } from "@/backend/types";
import { healthOf, toSources } from "@/domain/adapt";
import { STARTER_CATALOG } from "@/domain/starter";
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

/** Whether /api/events is connected. */
export const connectionStore = createStore({ events: false });
/** Newest spectrum sequence announced on /api/events; only the waterfall feed listens. */
export const spectrumAnnounce = createStore(0);

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
    return;
  }
  if (!was || is) return;
  const open = sessionCalls.get().filter((c) => c.channelId === next.id && c.endedAt === null);
  if (!open.length) return;
  // Close every open call on this channel; only the newest counts as talk time.
  const newest = open.reduce((a, b) => (a.startedAt > b.startedAt ? a : b));
  sessionCalls.set((calls) =>
    calls.map((c) =>
      c.channelId === next.id && c.endedAt === null ? { ...c, endedAt: c === newest ? now : c.startedAt } : c,
    ),
  );
  sessionActivity.set((a) => {
    const index = Math.floor((newest.startedAt - a.start) / SESSION_BUCKET);
    if (index < 0) return a;
    const series = [...(a.byChannel[next.id] ?? [])];
    while (series.length <= index) series.push(0);
    series[index] += (now - newest.startedAt) / 1000;
    return { ...a, byChannel: { ...a.byChannel, [next.id]: series } };
  });
}

/** Which source owns each engine channel. */
function channelOwners(sources: Source[]) {
  const owner = new Map<string, string>();
  sources.forEach((s) => s.channelIds.forEach((id) => owner.set(id, s.id)));
  return owner;
}

const LOCAL_CATALOG = "radio.catalog.local.v1";
export const localCatalog = createStore<WireCatalogEntry[]>(
  local.read(LOCAL_CATALOG, catalogSchema.shape.sources, []),
);
localCatalog.subscribe(() => local.write(LOCAL_CATALOG, localCatalog.get()));

// ---------------------------------------------------------------------------

/** Apply a burst of channel events in one cache update (one render), newest wins. */
function applyChannels(client: QueryClient, updates: EngineChannel[]) {
  // An in-flight refetch may predate these events; drop it rather than let it land after them.
  // Never cancel the first fetch: without a snapshot there's nothing to patch.
  if (!client.getQueryData(keys.status)) return;
  void client.cancelQueries({ queryKey: keys.status });
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
      // A channel that vanished mid-call (engine restart) has stopped talking.
      for (const [id, previous] of seen)
        if (!next.has(id)) recordTransition(previous, { ...previous, speech_active: false }, now);
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
    // Gaps can arrive in bursts (one per gap frame); refresh at most once a second.
    let refresh: ReturnType<typeof setTimeout> | undefined;
    const refreshSnapshot = () => {
      refresh ??= setTimeout(() => {
        refresh = undefined;
        void client.invalidateQueries({ queryKey: keys.status });
        void client.invalidateQueries({ queryKey: keys.blocks });
      }, 1000);
    };
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
            spectrumAnnounce.set(event.sequence);
            break;
          case "source_config":
          case "source_state":
          case "source_gap":
          case "gap":
            refreshSnapshot();
            break;
          case "heartbeat":
            break;
        }
      },
      (open) => {
        connectionStore.set({ events: open });
        // Anything could have happened while disconnected.
        if (open)
          for (const key of [keys.status, keys.calls, keys.activity]) void client.invalidateQueries({ queryKey: key });
      },
    );
    return () => {
      clearTimeout(flush);
      clearTimeout(refresh);
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

/**
 * Names, in priority order: the engine's catalog, labels saved on this device
 * that the engine doesn't have, and (until the engine has a catalog) the
 * starter names from the RF research. `onDevice` is true while labels can only
 * be saved on this device.
 */
export function useCatalog() {
  const backend = useBackend();
  const query = useQuery({ queryKey: keys.catalog, queryFn: () => backend.catalog(), staleTime: 60_000 });
  const saved = useStore(localCatalog);
  const engine = query.data ?? null;
  const entries = useMemo(() => {
    if (!engine) {
      // An edited starter name is saved on the device under the same id and replaces it.
      const mine = new Set(saved.map((e) => e.id));
      return [...saved, ...STARTER_CATALOG.filter((e) => !mine.has(e.id))];
    }
    const known = new Set(engine.map((e) => e.id));
    return [...engine, ...saved.filter((e) => !known.has(e.id))];
  }, [engine, saved]);
  return { entries, onDevice: query.data === null, engineEntries: engine };
}

/**
 * Once the engine serves /api/catalog, upload labels that were saved on this
 * device and forget the ones it accepted. Mount once.
 */
export function CatalogMigration() {
  const backend = useBackend();
  const client = useQueryClient();
  const { engineEntries } = useCatalog();
  const saved = useStore(localCatalog);
  useEffect(() => {
    if (!engineEntries || !saved.length) return;
    let cancelled = false;
    void (async () => {
      const uploaded: string[] = [];
      for (const entry of saved) {
        try {
          if (await backend.saveCatalogEntry(entry)) uploaded.push(entry.id);
        } catch {
          /* keep it on the device and try again next time */
        }
      }
      if (cancelled || !uploaded.length) return;
      localCatalog.set((list) => list.filter((e) => !uploaded.includes(e.id)));
      void client.invalidateQueries({ queryKey: keys.catalog });
    })();
    return () => {
      cancelled = true;
    };
  }, [backend, client, engineEntries, saved]);
  return null;
}

export function useSources(): Source[] {
  const { data } = useStatus();
  const { entries } = useCatalog();
  const capabilities = data?.source?.capabilities as { fm_audio?: unknown } | undefined;
  const fmAudio = capabilities?.fm_audio === true;
  return useMemo(() => toSources(data?.channels ?? [], entries, { fmAudio }), [data?.channels, entries, fmAudio]);
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
export function useCalls(): { calls: Call[]; recorded: boolean; loading: boolean; failed: boolean } {
  const backend = useBackend();
  const sources = useSources();
  const query = useQuery({
    queryKey: keys.calls,
    queryFn: () => backend.calls({ limit: 200 }),
    refetchInterval: (q) => (q.state.data === null ? false : 60_000),
  });
  const session = useStore(sessionCalls);
  return useMemo(() => {
    if (query.data)
      return { calls: query.data.map((c) => toCall(c, backend)), recorded: true, loading: false, failed: false };
    const owner = channelOwners(sources);
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
      // The engine errored rather than answering "not built yet" (null).
      failed: query.isError && query.data === undefined,
    };
  }, [query.data, query.isLoading, query.isError, session, sources, backend]);
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
    const owner = channelOwners(sources);
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
    // The engine reports 0 (newer builds) or null (older) before the first CONFIG.
    const known = (v: number | null | undefined) => (v ? v : null);
    const center = known(status.data?.center_hz);
    const span = known(status.data?.bandwidth_hz) ?? known(status.data?.rate_hz);
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
