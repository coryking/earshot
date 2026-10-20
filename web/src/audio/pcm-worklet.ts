/// <reference lib="webworker" />
/**
 * Low-latency PCM player. Receives 8 kHz mono Float32 chunks over the port,
 * resamples linearly to the context rate into a ring buffer, and starts after a
 * short prime (60 ms). On underrun it goes quiet and re-primes. If the buffer
 * grows past 400 ms (a stalled tab catching up), it drops the oldest audio so
 * playback stays near the live edge.
 */

declare const sampleRate: number;
declare function registerProcessor(name: string, ctor: unknown): void;
declare class AudioWorkletProcessor {
  readonly port: MessagePort;
}

type Message = { type: "pcm"; data: Float32Array; rate: number } | { type: "flush" };

class PcmPlayer extends AudioWorkletProcessor {
  private ring = new Float32Array(Math.ceil(sampleRate * 2));
  private read = 0;
  private write = 0;
  private primed = false;
  private phase = 0;
  private tail = 0;
  private readonly prime = Math.round(sampleRate * 0.06);
  private readonly ceiling = Math.round(sampleRate * 0.4);

  constructor() {
    super();
    this.port.onmessage = (event: MessageEvent<Message>) => {
      const message = event.data;
      if (message.type === "flush") {
        this.read = this.write = 0;
        this.primed = false;
        this.phase = 0;
        this.tail = 0;
        return;
      }
      this.push(message.data, message.rate);
    };
  }

  private get buffered() {
    return (this.write - this.read + this.ring.length) % this.ring.length;
  }

  private push(input: Float32Array, rate: number) {
    const step = rate / sampleRate;
    let pos = this.phase;
    const n = input.length;
    while (pos < n) {
      const i = Math.floor(pos);
      const f = pos - i;
      const a = i === 0 ? this.tail : input[i - 1];
      const b = input[i];
      this.ring[this.write] = a + (b - a) * f;
      this.write = (this.write + 1) % this.ring.length;
      if (this.write === this.read) this.read = (this.read + 1) % this.ring.length;
      pos += step;
    }
    this.phase = pos - n;
    this.tail = input[n - 1] ?? this.tail;
    const excess = this.buffered - this.ceiling;
    if (excess > 0) this.read = (this.read + excess) % this.ring.length;
  }

  process(_inputs: Float32Array[][], outputs: Float32Array[][]) {
    const out = outputs[0]?.[0];
    if (!out) return true;
    if (!this.primed && this.buffered >= this.prime) this.primed = true;
    for (let i = 0; i < out.length; i++) {
      if (this.primed && this.read !== this.write) {
        const v = this.ring[this.read];
        this.read = (this.read + 1) % this.ring.length;
        out[i] = v;
      } else {
        this.primed = false;
        out[i] = 0;
      }
    }
    for (let c = 1; c < outputs[0].length; c++) outputs[0][c].set(out);
    return true;
  }
}

registerProcessor("pcm-player", PcmPlayer);
