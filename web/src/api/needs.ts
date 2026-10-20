/**
 * Wire types for endpoints the frontend needs but the engine does not serve
 * yet. The contract is written up in engine/README.md § "Frontend needs" and on
 * issue #1. Until the engine answers them, live mode falls back to in-browser
 * equivalents (the session fallbacks in state/radio.tsx) and demo mode uses the
 * mock. Lists parse per item, so one bad entry never empties a list.
 */
import { z } from "zod";
import { lenientArray } from "./lenient";

/** Ids are strings on the wire; accept numbers too. */
const id = z.union([z.string(), z.number()]).transform(String);

const num = z.number().nullish().catch(null);
const str = z.string().nullish().catch(null);

/** GET /api/blocks */
export const blockSchema = z.looseObject({
  id,
  name: z.string(),
  description: z.string().catch(""),
  center_hz: z.number(),
  span_hz: z.number(),
});
export const blocksSchema = z.looseObject({
  current_id: str,
  blocks: lenientArray(blockSchema),
});
export type WireBlocks = z.infer<typeof blocksSchema>;

/** GET /api/catalog. One entry per listenable thing, labelled by the operator. */
export const catalogEntrySchema = z.looseObject({
  id,
  kind: z.enum(["channel", "talkgroup"]).catch("channel"),
  /**
   * Engine channel ids this entry covers (`454029223-DMR-2`). The UI also joins
   * by frequency_hz (±3 kHz) and slot, since an id changes when the engine
   * identifies the protocol.
   */
  channel_ids: lenientArray(z.string()),
  frequency_hz: num,
  slot: num,
  /** Trunking (next): the system and talkgroup this entry belongs to. */
  system: z.looseObject({ id: z.string(), name: z.string() }).nullish().catch(null),
  talkgroup_id: num,
  name: z.string(),
  description: z.string().catch(""),
  probable_user: str,
  traffic_kind: str,
  mode: z.enum(["fm", "dmr", "nxdn", "p25", "unknown"]).catch("unknown"),
  encryption: z.enum(["clear", "encrypted", "mixed", "unknown"]).catch("unknown"),
});
export const catalogSchema = z.looseObject({
  sources: lenientArray(catalogEntrySchema),
});
export type WireCatalogEntry = z.infer<typeof catalogEntrySchema>;

/** GET /api/calls?before=&limit=&source= and `{type:"call"}` on /api/events. */
export const callSchema = z.looseObject({
  id,
  source_id: id,
  channel_id: str,
  /** Unix seconds. */
  started_at: z.number(),
  ended_at: num,
  encrypted: z.boolean().catch(false),
  /** Relative URL to the recorded audio (audio/ogg;codecs=opus or audio/wav). */
  audio_url: str,
  /** Trunking: radio ID of the talker, when known. */
  talker: id.nullish().catch(null),
  /** Speech-to-text (later). */
  transcript: str,
});
export const callsSchema = z.looseObject({
  calls: lenientArray(callSchema),
  /** Unix seconds; pass back as `before` for the next page. */
  next_before: num,
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
 *  32  8  u64 latest sequence (also when R = 0)
 *  40  4  u32 generation (changes on retune)
 *  44  4  reserved, 0
 *  48  …  R rows of: u64 sequence, f64 time_s, N × u8 level
 *
 * One 1024-bin row is 1,040 bytes instead of ~8 KB of JSON floats.
 */
export const RSP1_HEADER = 48;
