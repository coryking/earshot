/**
 * Wire types for what the Rust engine (engine/src/api.rs, runtime.rs) actually
 * sends today. Every field the engine builds from producer JSON (`e.source[..]`)
 * or from activity frames can be null or missing, so those are nullish here and
 * get defaults at the boundary. Unknown extra fields are ignored, never fatal.
 */
import { z } from "zod";
import { callSchema } from "./needs";
import { lenientArray } from "./lenient";

const num = z.number().nullish().catch(null);
const str = z.string().nullish().catch(null);
const bool = z.boolean().nullish().catch(null);

/** One catalog entry: `${frequency}-${protocol}-${slot}`. */
export const engineChannelSchema = z.looseObject({
  id: z.string(),
  stream_id: num,
  frequency_hz: z.number(),
  /** "Unknown" | "DMR" | "NXDN48"; kept open for P25/FM later. */
  protocol: z.string().catch("Unknown"),
  slot: num,
  /** "unidentified" | "voice" */
  kind: str,
  /** Copied from activity frames; null when the producer omits it. */
  rf_active: bool,
  decoded: bool,
  speech_active: bool,
  /** Unix seconds (float). */
  last_heard: num,
  last_rf: num,
  level_db: num,
  audio_seconds: num,
  sequence: num,
  in_range: bool,
  available: bool,
  /** Absent until the scheduler has run once. */
  decoder_admitted: bool,
});
export type EngineChannel = z.infer<typeof engineChannelSchema>;

const channelList = lenientArray(engineChannelSchema);

/** How the engine stands with its source (engine/src/link.rs). */
export const engineLinkSchema = z.looseObject({
  /** "tcp" (a receiver is configured) | "fixture" (nothing to reconnect to) */
  mode: z.string().catch("fixture"),
  /** "connected" | "connecting" | "reconnecting" | "waiting" | "fixture" */
  state: z.string().catch("fixture"),
  offline_s: num,
  failures: num,
  last_error: str,
  next_retry_s: num,
});
export type EngineLink = z.infer<typeof engineLinkSchema>;

/** What the receiver supervisor says (radio-supervisor status file); null when there is none. */
export const engineReceiverSchema = z.looseObject({
  /** "starting" | "running" | "waiting" | "borrowed" | "stopped" | "unknown" (no status file) */
  state: z.string().catch("unknown"),
  /** "lock-held" | "unreachable" | "wrong-image" | "producer-failed" | "radio-state" | "shutdown" | null */
  reason: str,
  detail: str,
  /** False when the status file is old, meaning the supervisor is not running. */
  fresh: bool,
  last_restore: str,
});
export type EngineReceiver = z.infer<typeof engineReceiverSchema>;

export const engineStatusSchema = z.looseObject({
  /** "live" | "disconnected" */
  mode: z.string().catch("disconnected"),
  /** "receiving" | "error" | "stopped" */
  state: z.string().catch("stopped"),
  source_name: str,
  center_hz: num,
  rate_hz: num,
  bandwidth_hz: num,
  generation: z.number().catch(0),
  channels: channelList,
  received_seconds: num,
  source_position_s: num,
  decode_latency_s: num,
  dropped_batches: num,
  processed_batches: num,
  queue_depth: num,
  error: str,
  decoder_error: str,
  started_at: num,
  last_samples_at: num,
  continuity: str,
  decoder_capacity: num,
  /** Absent on engines from before the reconnecting link. */
  link: engineLinkSchema.nullish().catch(null),
  receiver: engineReceiverSchema.nullish().catch(null),
  /** Producer CONFIG JSON as read back; `{}` before the first CONFIG. */
  source: z.record(z.string(), z.unknown()).nullish().catch(null),
});
export type EngineStatus = z.infer<typeof engineStatusSchema>;

export const engineSpectrumSchema = z.looseObject({
  sequence: z.number().catch(0),
  generation: z.number().catch(0),
  rows: z.array(z.array(z.number())).catch([]),
  times: z.array(z.number()).catch([]),
  center_hz: num,
  span_hz: num,
  low_db: z.number().catch(-125),
  high_db: z.number().catch(-65),
  unit: z.string().catch("uncalibrated"),
  source: z.string().catch("disconnected"),
});
export type EngineSpectrum = z.infer<typeof engineSpectrumSchema>;

/** WS /api/events messages. */
export const engineEventSchema = z.discriminatedUnion("type", [
  z.looseObject({
    type: z.literal("channel"),
    generation: num,
    channel: engineChannelSchema,
  }),
  z.looseObject({
    type: z.literal("source_config"),
    generation: num,
    source: z.record(z.string(), z.unknown()).catch({}),
  }),
  z.looseObject({
    type: z.literal("source_state"),
    state: z.string(),
    error: str,
    generation: num,
  }),
  z.looseObject({
    type: z.literal("source_gap"),
    generation: num,
    stream_id: num,
  }),
  z.looseObject({
    type: z.literal("spectrum_available"),
    generation: num,
    sequence: z.number(),
  }),
  z.looseObject({
    type: z.literal("heartbeat"),
    generation: num,
    source_generation: num,
  }),
  z.looseObject({ type: z.literal("gap"), dropped_events: num }),
  /** Frontend need: a call started or ended (see api/needs.ts). */
  z.looseObject({ type: z.literal("call"), call: callSchema }),
]);
export type EngineEvent = z.infer<typeof engineEventSchema>;

/** WS /api/listen text messages. A binary PCM message follows each "audio". */
export const listenMessageSchema = z.discriminatedUnion("type", [
  z.looseObject({
    type: z.literal("audio"),
    encoding: z.literal("s16le"),
    channels: z.literal(1),
    rate_hz: z.number().int().positive().max(96000),
    samples: z.number().int().positive(),
    channel_id: z.string(),
    generation: z.number(),
    source_generation: z.number(),
    sequence: z.number(),
    ingress_age_ms: num,
  }),
  z.looseObject({
    type: z.literal("heartbeat"),
    generation: num,
    source_generation: num,
    channel_id: str,
  }),
  z.looseObject({
    type: z.literal("gap"),
    reason: str,
    dropped_chunks: num,
  }),
]);
export type ListenMessage = z.infer<typeof listenMessageSchema>;
export type AudioDescriptor = Extract<ListenMessage, { type: "audio" }>;

export function parseJson<T>(schema: z.ZodType<T>, text: string): T | null {
  try {
    const parsed = schema.safeParse(JSON.parse(text));
    return parsed.success ? parsed.data : null;
  } catch {
    return null;
  }
}
