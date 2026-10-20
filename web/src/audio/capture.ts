import type { AudioDescriptor } from "@/api/engine";

/** Unsquelched FM discriminator, before voice processing. Bounded to two minutes.
 * The WAV INFO comment records transport gaps; this is not a continuous IQ capture. */
export class RawCapture {
  private chunks: Int16Array[] = [];
  private samples = 0;
  private dropped = 0;
  private gaps: { sample: number; reason: string }[] = [];
  private generation: string | null = null;
  private expectedInput: number | null = null;
  readonly startedAt = new Date().toISOString();
  readonly frequencyHz: number;
  readonly rate: number;
  private maxSeconds: number;
  constructor(frequencyHz: number, rate = 48000, maxSeconds = 120) {
    this.frequencyHz = frequencyHz;
    this.rate = rate;
    this.maxSeconds = maxSeconds;
  }
  get seconds() { return this.samples / this.rate; }
  gap(reason: string) {
    if (this.samples && this.gaps.at(-1)?.sample !== this.dropped + this.samples) this.gaps.push({ sample: this.dropped + this.samples, reason });
    // Coalesce repeated reconnect notifications until more audio arrives.
    this.expectedInput = null;
  }
  push(meta: AudioDescriptor, pcm: Int16Array) {
    if (meta.rate_hz !== this.rate) return;
    const generation = `${meta.source_generation}:${meta.generation}`;
    if (this.generation !== null && this.generation !== generation) this.gap("source or decoder restart");
    this.generation = generation;
    if (typeof meta.first_sample === "number" && typeof meta.input_rate_hz === "number") {
      if (this.expectedInput !== null && Math.abs(meta.first_sample - this.expectedInput) > 2)
        this.gap("input sample discontinuity");
      this.expectedInput = meta.first_sample + pcm.length / this.rate * meta.input_rate_hz;
    }
    this.chunks.push(pcm.slice());
    this.samples += pcm.length;
    const excess = this.samples - this.rate * this.maxSeconds;
    let trim = Math.max(0, excess);
    while (trim > 0 && this.chunks.length) {
      const first = this.chunks[0];
      const count = Math.min(trim, first.length);
      if (count === first.length) this.chunks.shift();
      else this.chunks[0] = first.slice(count);
      this.samples -= count;
      this.dropped += count;
      trim -= count;
    }
    this.gaps = this.gaps.filter((gap) => gap.sample >= this.dropped);
  }
  wav(): ArrayBuffer {
    const comment = new TextEncoder().encode(JSON.stringify({
      frequency_hz: this.frequencyHz, started_at: this.startedAt,
      mode: "raw FM discriminator", sample_rate_hz: this.rate, channel_bandwidth_hz: 11250,
      processing: "channel filter, FM demodulation and DC removal; no squelch or de-emphasis",
      retained_from_sample: this.dropped, continuity: "host transport checked; ADC continuity unknown",
      gaps: this.gaps.map((gap) => ({ ...gap, sample: gap.sample - this.dropped })),
    }) + "\0");
    const padded = comment.length + comment.length % 2;
    const buffer = new ArrayBuffer(44 + this.samples * 2 + 20 + padded);
    const view = new DataView(buffer);
    const text = (at: number, value: string) => [...value].forEach((c, i) => view.setUint8(at + i, c.charCodeAt(0)));
    text(0, "RIFF"); view.setUint32(4, buffer.byteLength - 8, true); text(8, "WAVEfmt ");
    view.setUint32(16, 16, true); view.setUint16(20, 1, true); view.setUint16(22, 1, true);
    view.setUint32(24, this.rate, true); view.setUint32(28, this.rate * 2, true);
    view.setUint16(32, 2, true); view.setUint16(34, 16, true); text(36, "data");
    view.setUint32(40, this.samples * 2, true);
    let at = 44;
    for (const chunk of this.chunks) for (const sample of chunk) { view.setInt16(at, sample, true); at += 2; }
    text(at, "LIST"); view.setUint32(at + 4, 12 + padded, true); text(at + 8, "INFOICMT");
    view.setUint32(at + 16, comment.length, true); new Uint8Array(buffer, at + 20, comment.length).set(comment);
    return buffer;
  }
}
