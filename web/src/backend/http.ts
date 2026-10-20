import {
  engineEventSchema,
  engineChannelSchema,
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
/** 404/405/501 on a "frontend needs" path mean the engine hasn't built it yet. */
function notBuilt(status: number) {
  return status === 404 || status === 405 || status === 501;
}

async function optional<T>(path: string, schema: z.ZodType<T>): Promise<T | null> {
  try {
    return await getJson(path, schema);
  } catch (error) {
    if (error instanceof BackendError && notBuilt(error.status)) return null;
    throw error;
  }
}

/**
 * Probe whether the engine serves a path, for the operator view. ServeDir
 * answers unknown GETs with 404; only a successful non-HTML answer counts.
 */
async function probe(path: string): Promise<boolean> {
  try {
    const response = await fetch(path, { signal: AbortSignal.timeout(4000) });
    return response.ok && !(response.headers.get("content-type") ?? "").includes("text/html");
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
    let lastMessage = 0;
    const connect = () => {
      const current = new WebSocket(wsUrl("/api/events"));
      socket = current;
      lastMessage = Date.now();
      current.onopen = () => {
        if (socket === current) onConnection(true);
      };
      current.onmessage = (message) => {
        if (socket !== current) return;
        lastMessage = Date.now();
        attempt = 0;
        if (typeof message.data !== "string") return;
        const event = parseJson(engineEventSchema, message.data);
        if (event) onEvent(event);
      };
      current.onclose = () => {
        if (closed || socket !== current) return;
        socket = null;
        onConnection(false);
        attempt += 1;
        retry = setTimeout(connect, Math.min(10_000, 500 * 2 ** attempt) * (0.8 + Math.random() * 0.4));
      };
    };
    // The engine sends a heartbeat every second; a half-open socket goes quiet.
    const watchdog = setInterval(() => {
      if (socket && socket.readyState === WebSocket.OPEN && Date.now() - lastMessage > 5000) socket.close();
    }, 1000);
    connect();
    return {
      close() {
        closed = true;
        clearTimeout(retry);
        clearInterval(watchdog);
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
      } else if (notBuilt(response.status) || (response.ok && this.binarySpectrum === null)) {
        // Not served (ServeDir 404, or the SPA answering with HTML): use the JSON rows.
        this.binarySpectrum = false;
      } else if (!response.ok) throw new BackendError(`spectrum ${response.status}`, response.status);
    }
    return fromJson(await getJson(`/api/spectrum?after=${after}`, engineSpectrumSchema));
  }

  async monitor(channelId: string | null) {
    await request("/api/monitor", { method: "POST", body: JSON.stringify({ channel_id: channelId }) });
  }

  async tune(frequencyHz: number) {
    const response = await request("/api/tune", { method: "POST", body: JSON.stringify({ frequency_hz: frequencyHz }) });
    return engineChannelSchema.parse(await response.json());
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
    return data.device_control ?? `Receive session ${data.status ?? "stopped"}. A supervisor may start receiving again.`;
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
      if (error instanceof BackendError && notBuilt(error.status)) return false;
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
      probe("/api/spectrum.bin?after=999999999999"),
    ]);
    return { blocks, catalog, calls, activity, spectrumBinary };
  }
}
