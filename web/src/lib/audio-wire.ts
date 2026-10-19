import { z } from "zod";
const metadataSchema = z.object({
  type: z.literal("audio"),
  channel_id: z.string(),
  sequence: z.number().int(),
  generation: z.number().int(),
  source_generation: z.number().int(),
  rate_hz: z.number().int().positive().max(96000),
  ingress_age_ms: z.number(),
  pcm: z.string().optional(),
  encoding: z.literal("s16le").optional(),
  channels: z.literal(1).optional(),
  samples: z.number().int().positive().optional(),
});
type Metadata = z.infer<typeof metadataSchema>;
export type AudioFrame = Metadata & { bytes: Uint8Array };
/** Accepts the interim JSON PCM and the Rust metadata/binary PCM pair. */
export class AudioWire {
  private pending: Metadata | null = null;
  reset() {
    this.pending = null;
  }
  read(data: string | ArrayBuffer): AudioFrame | null {
    if (data instanceof ArrayBuffer) {
      const packet = this.pending;
      this.pending = null;
      if (!packet) return null;
      const bytes = new Uint8Array(data);
      return this.validate(packet, bytes);
    }
    let raw: unknown;
    try {
      raw = JSON.parse(data);
    } catch {
      return null;
    }
    const parsed = metadataSchema.safeParse(raw);
    if (!parsed.success) {
      this.pending = null;
      return null;
    }
    const packet = parsed.data;
    if (packet.pcm === undefined) {
      this.pending =
        packet.encoding === "s16le" &&
        packet.channels === 1 &&
        packet.samples !== undefined
          ? packet
          : null;
      return null;
    }
    this.pending = null;
    try {
      return this.validate(
        packet,
        Uint8Array.from(atob(packet.pcm), (c) => c.charCodeAt(0)),
      );
    } catch {
      return null;
    }
  }
  private validate(packet: Metadata, bytes: Uint8Array): AudioFrame | null {
    if (
      !bytes.length ||
      bytes.length % 2 ||
      (packet.samples !== undefined && packet.samples * 2 !== bytes.length)
    )
      return null;
    return { ...packet, bytes };
  }
}
