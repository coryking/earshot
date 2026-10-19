import { z } from "zod";
import type { components } from "./api";
export type Channel = components["schemas"]["Channel"];
export type Status = components["schemas"]["Status"];
export type Spectrum = components["schemas"]["Spectrum"];
export type SourceConfig = components["schemas"]["SourceConfig"];
const channel = z.object({
  id: z.string(),
  frequency_hz: z.number(),
  protocol: z.string(),
  slot: z.number().nullable().optional(),
  kind: z.enum(["voice", "data", "unidentified"]),
  rf_active: z.boolean(),
  decoded: z.boolean(),
  speech_active: z.boolean(),
  last_heard: z.number().nullable().optional(),
  last_rf: z.number().nullable().optional(),
  level_db: z.number().nullable().optional(),
  audio_seconds: z.number(),
  sequence: z.number(),
  in_range: z.boolean(),
  available: z.boolean(),
});
export const statusSchema = z.object({
  mode: z.enum(["live", "disconnected"]),
  state: z.enum(["stopped", "connecting", "receiving", "error", "stopping"]),
  source_name: z.string(),
  center_hz: z.number(),
  rate_hz: z.number(),
  bandwidth_hz: z.number(),
  generation: z.number(),
  received_seconds: z.number(),
  source_position_s: z.number(),
  decode_latency_s: z.number(),
  dropped_batches: z.number(),
  processed_batches: z.number(),
  queue_depth: z.number(),
  error: z.string().nullable().optional(),
  decoder_error: z.string().nullable().optional(),
  started_at: z.number().nullable().optional(),
  last_samples_at: z.number().nullable().optional(),
  continuity: z.string(),
  channels: z.array(channel),
});
export const channelsSchema = z.array(channel);
export const spectrumSchema = z.object({
  sequence: z.number(),
  generation: z.number(),
  rows: z.array(z.array(z.number())),
  times: z.array(z.number()),
  center_hz: z.number(),
  span_hz: z.number(),
  low_db: z.number(),
  high_db: z.number(),
  unit: z.string(),
  source: z.string(),
});
export async function api<T>(
  path: string,
  schema: z.ZodType<T>,
  body?: unknown,
): Promise<T> {
  const response = await fetch(
    `/api/${path}`,
    body === undefined
      ? undefined
      : {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(body),
        },
  );
  if (!response.ok) {
    const error = await response
      .json()
      .catch(() => ({ detail: response.statusText }));
    throw new Error(error.detail || "Receiver request failed");
  }
  return schema.parse(await response.json());
}
// A physical channel stays pinned through ordinary silence and band-width changes.
export function fallbackChannel(
  channels: Channel[],
  current: string | null,
): string | null {
  if (channels.some((c) => c.id === current)) return current;
  return (
    [...channels]
      .filter((c) => c.kind === "voice" && c.speech_active && c.available)
      .sort(
        (a, b) =>
          (b.last_heard ?? 0) - (a.last_heard ?? 0) ||
          (b.level_db ?? -200) - (a.level_db ?? -200),
      )[0]?.id ?? null
  );
}
export const mhz = (hz: number) => (hz / 1e6).toFixed(6);
export function age(at: number | null | undefined) {
  if (!at) return "—";
  const n = Math.max(0, Math.floor(Date.now() / 1000 - at));
  return n < 2
    ? "just now"
    : n < 60
      ? `${n}s ago`
      : `${Math.floor(n / 60)}m ago`;
}

export const listenerSchema = z.object({
  status: z.string(),
  state: z.string().optional(),
  channel_id: z.string().nullable().optional(),
  voice_active: z.boolean(),
  error: z.string().nullable(),
  ingress_age_ms: z.number().nullable().optional(),
  input_dropped: z.number().optional(),
  dropped_blocks: z.number(),
  audio_chunks: z.number(),
  last_audio_at: z.number().nullable().optional(),
});
