import { z } from "zod";
const packetSchema = z.object({
  type: z.literal("audio"),
  pcm: z.string(),
  channel_id: z.string(),
  sequence: z.number(),
  generation: z.number(),
  source_generation: z.number(),
  rate_hz: z.number(),
  ingress_age_ms: z.number(),
});
export type PlaybackState = {
  playing: boolean;
  error: string | null;
  inputAgeMs: number | null;
};
/** Schedules fresh streamed PCM only. No recording, replay, or segment history. */
export class LivePlayer {
  private context: AudioContext | null = null;
  private socket: WebSocket | null = null;
  private channel: string | null = null;
  private sourceGeneration = 0;
  private decoderGeneration: number | null = null;
  private nextTime = 0;
  private nodes = new Set<AudioBufferSourceNode>();
  private timeout: ReturnType<typeof setTimeout> | undefined;
  constructor(private change: (state: PlaybackState) => void) {}
  enable() {
    this.context ??= new AudioContext();
    return this.context.resume();
  }
  play(channel: string, sourceGeneration: number) {
    if (
      this.channel !== channel ||
      this.sourceGeneration !== sourceGeneration
    ) {
      this.flush();
      this.decoderGeneration = null;
    }
    this.channel = channel;
    this.sourceGeneration = sourceGeneration;
    if (this.socket) return;
    const socket = new WebSocket(
      `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/api/listen`,
    );
    this.socket = socket;
    socket.onmessage = (event) => {
      let raw: unknown;
      try {
        raw = JSON.parse(String(event.data));
      } catch {
        return;
      }
      const result = packetSchema.safeParse(raw);
      if (!result.success) return;
      const packet = result.data;
      if (
        packet.channel_id !== this.channel ||
        packet.source_generation !== this.sourceGeneration ||
        !this.context ||
        this.context.state !== "running"
      )
        return;
      if (this.decoderGeneration !== packet.generation) {
        this.flush();
        this.decoderGeneration = packet.generation;
      }
      const bytes = Uint8Array.from(atob(packet.pcm), (c) => c.charCodeAt(0));
      if (bytes.length % 2) return;
      const samples = bytes.length / 2,
        buffer = this.context.createBuffer(1, samples, packet.rate_hz),
        out = buffer.getChannelData(0),
        view = new DataView(bytes.buffer);
      for (let i = 0; i < samples; i++)
        out[i] = view.getInt16(i * 2, true) / 32768;
      const now = this.context.currentTime;
      if (this.nextTime > now + 0.5) this.flush();
      this.nextTime = Math.max(this.nextTime, now + 0.035);
      const node = this.context.createBufferSource();
      node.buffer = buffer;
      node.connect(this.context.destination);
      this.nodes.add(node);
      node.onended = () => {
        this.nodes.delete(node);
        node.disconnect();
      };
      node.start(this.nextTime);
      this.nextTime += buffer.duration;
      this.change({
        playing: true,
        error: null,
        inputAgeMs: packet.ingress_age_ms,
      });
      clearTimeout(this.timeout);
      this.timeout = setTimeout(
        () =>
          this.change({
            playing: false,
            error: null,
            inputAgeMs: packet.ingress_age_ms,
          }),
        Math.max(0, (this.nextTime - now) * 1000 + 75),
      );
    };
    socket.onerror = () =>
      this.change({
        playing: false,
        error: "Live audio connection failed. Press Listen to reconnect.",
        inputAgeMs: null,
      });
    socket.onclose = () => {
      if (this.socket === socket) {
        this.socket = null;
        this.flush();
      }
    };
  }
  flush() {
    clearTimeout(this.timeout);
    for (const node of this.nodes) {
      try {
        node.stop();
      } catch {
        /* already stopped */
      }
      node.disconnect();
    }
    this.nodes.clear();
    this.nextTime = 0;
    this.change({ playing: false, error: null, inputAgeMs: null });
  }
  pause() {
    this.channel = null;
    const socket = this.socket;
    this.socket = null;
    socket?.close();
    this.flush();
  }
  close() {
    this.pause();
    void this.context?.close();
    this.context = null;
  }
}
