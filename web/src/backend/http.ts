import {
  engineEventSchema,
  engineSpectrumSchema,
  engineStatusSchema,
  listenMessageSchema,
  parseJson,
  type AudioDescriptor,
} from "@/api/engine";
import { activitySchema, blocksSchema, callsSchema, catalogSchema } from "@/api/needs";
import { z } from "zod";
import { decodeRsp1, fromJson } from "./spectrum";
import { BackendError, type Backend, type CallQuery, type ListenHandlers, type Need } from "./types";

/** /api/stop: `status` is a string when stopped locally, an object from the producer's readback otherwise. */
const stopSchema = z.looseObject({
  device_control: z.string().nullish().catch(null),
  status: z.string().nullish().catch(null),
});

function wsUrl(path: string) {
  return `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}${path}`;
}

/** Engine errors are plain text from /api/monitor and `{detail}` JSON elsewhere. */
async function errorText(response: Response): Promise<string> {
  const text = await response.text().catch(() => "");
  const json = parseJson(z.looseObject({ detail: z.string() }), text);
  return json?.detail ?? (text.trim() || `${response.status} ${response.statusText}`);
}

async function request(path: string, init?: RequestInit): Promise<Response> {
  const response = await fetch(path, {
    ...init,
    headers: init?.body ? { "Content-Type": "application/json", ...init.headers } : init?.headers,
    signal: init?.signal ?? AbortSignal.timeout(8000),
  });
  if (!response.ok) throw new BackendError(await errorText(response), response.status);
  return response;
}

async function getJson<T>(path: string, schema: z.ZodType<T>): Promise<T> {
  const response = await request(path);
  const parsed = schema.safeParse(await response.json());
  if (!parsed.success) throw new BackendError(`Unexpected response from ${path}`, 502);
  return parsed.data;
}

/** A "frontend needs" endpoint: null when the engine doesn't serve it yet. */
async function optional<T>(path: string, schema: z.ZodType<T>): Promise<T | null> {
  try {
    return await getJson(path, schema);
  } catch (error) {
    if (error instanceof BackendError && (error.status === 404 || error.status === 405 || error.status === 501))
      return null;
    throw error;
  }
}

/**
 * Probe whether the engine serves a path. ServeDir answers unknown GETs with
 * 404, so a 404 means "not built yet"; anything else means the route exists.
 */
async function probe(path: string): Promise<boolean> {
  try {
    const response = await fetch(path, { signal: AbortSignal.timeout(4000) });
    return response.status !== 404 && response.status !== 405 && response.status !== 501;
  } catch {
    return false;
  }
}

export class HttpBackend implements Backend {
  readonly mode = "live" as const;
  private binarySpectrum: boolean | null = null;

  status() {
    return getJson("/api/status", engineStatusSchema);
  }

  events(onEvent: Parameters<Backend["events"]>[0], onConnection: (open: boolean) => void) {
    let socket: WebSocket | null = null;
    let closed = false;
    let retry: ReturnType<typeof setTimeout> | undefined;
    let attempt = 0;
    const connect = () => {
      socket = new WebSocket(wsUrl("/api/events"));
      socket.onopen = () => {
        attempt = 0;
        onConnection(true);
      };
      socket.onmessage = (message) => {
        if (typeof message.data !== "string") return;
        const event = parseJson(engineEventSchema, message.data);
        if (event) onEvent(event);
      };
      socket.onclose = () => {
        onConnection(false);
        if (closed) return;
        attempt += 1;
        retry = setTimeout(connect, Math.min(10_000, 500 * 2 ** attempt));
      };
    };
    connect();
    return {
      close() {
        closed = true;
        clearTimeout(retry);
        socket?.close();
      },
    };
  }

  async spectrum(after: number) {
    if (this.binarySpectrum !== false) {
      const response = await fetch(`/api/spectrum.bin?after=${after}`, { signal: AbortSignal.timeout(4000) });
      const type = response.headers.get("content-type") ?? "";
      if (response.ok && type.includes("octet-stream")) {
        const batch = decodeRsp1(await response.arrayBuffer());
        this.binarySpectrum = true;
        if (batch) return batch;
      } else this.binarySpectrum = false;
    }
    return fromJson(await getJson(`/api/spectrum?after=${after}`, engineSpectrumSchema));
  }

  async monitor(channelId: string | null) {
    await request("/api/monitor", { method: "POST", body: JSON.stringify({ channel_id: channelId }) });
  }

  listen(handlers: ListenHandlers) {
    const socket = new WebSocket(wsUrl("/api/listen"));
    socket.binaryType = "arraybuffer";
    let pending: AudioDescriptor | null = null;
    socket.onmessage = (message) => {
      if (message.data instanceof ArrayBuffer) {
        const descriptor = pending;
        pending = null;
        if (!descriptor || message.data.byteLength !== descriptor.samples * 2) return;
        handlers.onAudio(descriptor, new Int16Array(message.data));
        return;
      }
      if (typeof message.data !== "string") return;
      const parsed = parseJson(listenMessageSchema, message.data);
      pending = null;
      if (!parsed) return;
      if (parsed.type === "audio") pending = parsed;
      else if (parsed.type === "heartbeat") handlers.onHeartbeat(parsed.channel_id ?? null);
      else handlers.onGap();
    };
    socket.onclose = () => handlers.onClose();
    return { close: () => socket.close() };
  }

  async stopReceiver() {
    const response = await request("/api/stop", { method: "POST", body: "{}" });
    const body = stopSchema.safeParse(await response.json().catch(() => ({})));
    const data = body.success ? body.data : { device_control: null, status: null };
    return data.device_control ?? `Receiver ${data.status ?? "stopped"}.`;
  }

  async configureSource(config: { centerHz: number; rateHz: number; bandwidthHz: number }) {
    await request("/api/source", {
      method: "POST",
      body: JSON.stringify({
        mode: "live",
        center_hz: Math.round(config.centerHz),
        rate_hz: Math.round(config.rateHz),
        bandwidth_hz: Math.round(config.bandwidthHz),
      }),
    });
    return "The receiver confirmed the new settings.";
  }

  blocks() {
    return optional("/api/blocks", blocksSchema);
  }

  async selectBlock(id: string) {
    await request("/api/blocks/current", { method: "POST", body: JSON.stringify({ id }) });
  }

  async catalog() {
    return (await optional("/api/catalog", catalogSchema))?.sources ?? null;
  }

  async saveCatalogEntry(entry: Parameters<Backend["saveCatalogEntry"]>[0]) {
    try {
      await request(`/api/catalog/${encodeURIComponent(entry.id)}`, { method: "PUT", body: JSON.stringify(entry) });
      return true;
    } catch (error) {
      if (error instanceof BackendError && [404, 405, 501].includes(error.status)) return false;
      throw error;
    }
  }

  async calls(query: CallQuery) {
    const params = new URLSearchParams();
    if (query.before) params.set("before", String(query.before / 1000));
    if (query.limit) params.set("limit", String(query.limit));
    if (query.sourceId) params.set("source", query.sourceId);
    return (await optional(`/api/calls?${params}`, callsSchema))?.calls ?? null;
  }

  activity() {
    return optional("/api/activity?window_s=86400&bucket_s=300", activitySchema);
  }

  async supports(): Promise<Record<Need, boolean>> {
    const [blocks, catalog, calls, activity, spectrumBinary] = await Promise.all([
      probe("/api/blocks"),
      probe("/api/catalog"),
      probe("/api/calls?limit=1"),
      probe("/api/activity?window_s=300&bucket_s=300"),
      probe("/api/spectrum.bin?after=0"),
    ]);
    return { blocks, catalog, calls, activity, spectrumBinary };
  }
}
