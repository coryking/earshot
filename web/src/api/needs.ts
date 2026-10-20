/**
 * Wire types for endpoints the frontend needs but the engine does not serve
 * yet. The contract is written up in engine/README.md § "Frontend needs" and on
 * issue #1. Until the engine answers them, live mode falls back to in-browser
 * equivalents (see backend/fallback.ts) and demo mode uses the mock.
 */
import { z } from "zod";

const num = z.number().nullish().catch(null);
const str = z.string().nullish().catch(null);

/** GET /api/blocks */
export const blockSchema = z.looseObject({
  id: z.string(),
  name: z.string(),
  description: z.string().catch(""),
  center_hz: z.number(),
  span_hz: z.number(),
});
export const blocksSchema = z.looseObject({
  current_id: str,
  blocks: z.array(blockSchema).catch([]),
});
export type WireBlocks = z.infer<typeof blocksSchema>;

/** GET /api/catalog. One entry per listenable thing, labelled by the operator. */
export const catalogEntrySchema = z.looseObject({
  id: z.string(),
  kind: z.enum(["channel", "talkgroup"]).catch("channel"),
  /** Engine channel ids this entry covers (`454029223-DMR-2`). */
  channel_ids: z.array(z.string()).catch([]),
  frequency_hz: num,
  slot: num,
  /** Trunking (next): the system and talkgroup this entry belongs to. */
  system: z.looseObject({ id: z.string(), name: z.string() }).nullish().catch(null),
  talkgroup_id: num,
  name: z.string(),
  description: z.string().catch(""),
  probable_user: str,
  traffic_kind: str,
  /** "fm" | "dmr" | "nxdn" | "p25" | "unknown" */
  mode: z.string().catch("unknown"),
  /** "clear" | "encrypted" | "mixed" | "unknown" */
  encryption: z.string().catch("unknown"),
});
export const catalogSchema = z.looseObject({
  sources: z.array(catalogEntrySchema).catch([]),
});
export type WireCatalogEntry = z.infer<typeof catalogEntrySchema>;

/** GET /api/calls?before=&limit=&source= and `{type:"call"}` on /api/events. */
export const callSchema = z.looseObject({
  id: z.string(),
  source_id: z.string(),
  channel_id: str,
  /** Unix seconds. */
  started_at: z.number(),
  ended_at: num,
  encrypted: z.boolean().catch(false),
  /** Relative URL to the recorded audio (audio/ogg;codecs=opus or audio/wav). */
  audio_url: str,
  /** Trunking: radio ID of the talker, when known. */
  talker: str,
  /** Speech-to-text (later). */
  transcript: str,
});
export const callsSchema = z.looseObject({
  calls: z.array(callSchema).catch([]),
  next_before: str,
});
export type WireCall = z.infer<typeof callSchema>;

/** GET /api/activity?window_s=&bucket_s= */
export const activitySchema = z.looseObject({
  bucket_s: z.number(),
  /** Unix seconds of the first bucket. */
  start: z.number(),
  /** Talk-seconds per bucket for the whole block. */
  block: z.array(z.number()).catch([]),
  /** Talk-seconds per bucket, per source id. */
  sources: z.record(z.string(), z.array(z.number())).catch({}),
  /** Typical talk-seconds per bucket for the same weekday and hour; null until learned. */
  baseline: z.array(z.number()).nullish().catch(null),
});
export type WireActivity = z.infer<typeof activitySchema>;

/**
 * Compact binary spectrum, `RSP1`, on `WS /api/spectrum` (push) or
 * `GET /api/spectrum.bin?after=N`. Little-endian:
 *
 *   0  4  magic "RSP1"
 *   4  2  u16 bins per row (N)
 *   6  2  u16 row count (R)
 *   8  8  f64 center_hz
 *  16  8  f64 span_hz
 *  24  4  f32 low_db   (byte 0)
 *  28  4  f32 high_db  (byte 255)
 *  32  …  R rows of: u64 sequence, f64 time_s, N × u8 level
 *
 * One 1024-bin row is 1,040 bytes instead of ~8 KB of JSON floats.
 */
export const RSP1_HEADER = 32;
