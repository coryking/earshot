import workletUrl from "./pcm-worklet.ts?worker&url";
import { BackendError, type Backend, type Connection } from "@/backend/types";
import type { Call, Source } from "@/domain/model";
import { createStore } from "@/lib/store";
import { prefsStore } from "@/state/prefs";

export type Phase = "idle" | "starting" | "waiting" | "playing" | "taken" | "error";

export interface PlayerState {
  kind: "idle" | "live" | "call";
  phase: Phase;
  sourceId: string | null;
  sourceName: string | null;
  channelId: string | null;
  callId: string | null;
  /** The engine's selection when another device took it over. */
  takenBy: string | null;
  error: string | null;
  muted: boolean;
  volume: number;
  /** 0..1, smoothed output level for the meter. */
  level: number;
  /** Recorded call progress, seconds. */
  position: number;
  duration: number;
}

const idle: Omit<PlayerState, "muted" | "volume"> = {
  kind: "idle",
  phase: "idle",
  sourceId: null,
  sourceName: null,
  channelId: null,
  callId: null,
  takenBy: null,
  error: null,
  level: 0,
  position: 0,
  duration: 0,
};

function monitorMessage(error: unknown): string {
  if (error instanceof BackendError) {
    if (error.status === 422) return "Can't play this one yet: the radio is still working out what kind of signal it is.";
    if (error.status === 404) return "That channel isn't being picked up any more.";
    return `The radio said no: ${error.message}`;
  }
  return "Couldn't reach the radio. Check the connection and try again.";
}

/**
 * One player for live audio and recorded calls, like a music app's single
 * "now playing". Live audio goes WebSocket → AudioWorklet (60 ms prime).
 * On touch devices it is routed through an <audio> element so it keeps
 * playing with the screen locked; desktops connect straight to the speakers.
 */
export class Player {
  readonly state = createStore<PlayerState>({
    ...idle,
    muted: false,
    volume: prefsStore.get().volume,
  });
  private context: AudioContext | null = null;
  private node: AudioWorkletNode | null = null;
  private gain: GainNode | null = null;
  private element: HTMLAudioElement | null = null;
  private ready: Promise<void> | null = null;
  private connection: Connection | null = null;
  private decoderGeneration: number | null = null;
  private lastAudio = 0;
  private takenSince: number | null = null;
  private watchdog: ReturnType<typeof setInterval> | null = null;
  private callElement: HTMLAudioElement | null = null;
  private queue: Call[] = [];
  private lastSource: Source | null = null;
  private blockName = "";

  private backend: Backend;

  constructor(backend: Backend) {
    this.backend = backend;
  }

  private patch(patch: Partial<PlayerState>) {
    this.state.set((s) => ({ ...s, ...patch }));
  }

  setBlockName(name: string) {
    this.blockName = name;
  }

  /** Must start inside the tap handler so mobile browsers allow audio. */
  private unlock(): Promise<void> {
    if (!this.context) {
      const nav = navigator as Navigator & { audioSession?: { type: string } };
      if (nav.audioSession) nav.audioSession.type = "playback";
      const context = new AudioContext({ latencyHint: "interactive" });
      this.context = context;
      const touch = window.matchMedia?.("(pointer: coarse)").matches ?? false;
      if (touch) {
        this.element = new Audio();
        this.element.setAttribute("playsinline", "");
      }
      this.ready = context.audioWorklet.addModule(workletUrl).then(() => {
        const node = new AudioWorkletNode(context, "pcm-player", { outputChannelCount: [1] });
        const gain = context.createGain();
        node.connect(gain);
        node.port.onmessage = (event: MessageEvent<{ type: string; rms?: number }>) => {
          if (event.data.type === "level") {
            const level = Math.min(1, (event.data.rms ?? 0) * 4);
            const prev = this.state.get().level;
            if (Math.abs(level - prev) > 0.02) this.patch({ level: prev * 0.5 + level * 0.5 });
          }
        };
        if (this.element) {
          const destination = context.createMediaStreamDestination();
          gain.connect(destination);
          this.element.srcObject = destination.stream;
        } else gain.connect(context.destination);
        this.node = node;
        this.gain = gain;
        this.applyVolume();
      });
    }
    void this.context.resume();
    void this.element?.play().catch(() => this.fallbackToSpeakers());
    return this.ready ?? Promise.resolve();
  }

  private fallbackToSpeakers() {
    if (!this.context || !this.gain || !this.element) return;
    this.gain.disconnect();
    this.gain.connect(this.context.destination);
    this.element = null;
  }

  private applyVolume() {
    const { muted, volume } = this.state.get();
    const value = muted ? 0 : volume;
    if (this.gain && this.context) this.gain.gain.setTargetAtTime(value, this.context.currentTime, 0.02);
    if (this.callElement) this.callElement.volume = value;
  }

  setVolume(volume: number) {
    this.patch({ volume, muted: volume === 0 });
    prefsStore.set((p) => ({ ...p, volume }));
    this.applyVolume();
  }

  toggleMute() {
    this.patch({ muted: !this.state.get().muted });
    this.applyVolume();
  }

  /** Listen live to a source. Tapping the playing source again stops. */
  async listen(source: Source) {
    const current = this.state.get();
    if (current.kind === "live" && current.sourceId === source.id && current.phase !== "error") {
      await this.stop();
      return;
    }
    if (!source.listenable.ok) {
      this.patch({ ...idle, error: source.listenable.reason, sourceName: source.name, phase: "error" });
      return;
    }
    this.stopCall();
    const channelId = source.channelIds[0];
    this.lastSource = source;
    const unlocked = this.unlock();
    this.patch({
      ...idle,
      kind: "live",
      phase: "starting",
      sourceId: source.id,
      sourceName: source.name,
      channelId,
    });
    this.flush();
    this.decoderGeneration = null;
    this.takenSince = null;
    this.updateSession(source.name, "Live");
    try {
      await Promise.all([unlocked, this.backend.monitor(channelId)]);
    } catch (error) {
      if (this.state.get().channelId === channelId)
        this.patch({ phase: "error", error: monitorMessage(error) });
      return;
    }
    if (this.state.get().channelId !== channelId) return;
    this.patch({ phase: "waiting" });
    this.openSocket();
    this.startWatchdog();
  }

  /** Take the engine's shared selection back after another device changed it. */
  async reclaim() {
    const { channelId } = this.state.get();
    if (!channelId) return;
    this.takenSince = null;
    this.patch({ phase: "waiting", takenBy: null });
    try {
      await this.backend.monitor(channelId);
    } catch (error) {
      this.patch({ phase: "error", error: monitorMessage(error) });
    }
  }

  private openSocket() {
    if (this.connection) return;
    this.connection = this.backend.listen({
      onAudio: (descriptor, pcm) => {
        const s = this.state.get();
        if (s.kind !== "live" || descriptor.channel_id !== s.channelId) return;
        if (this.decoderGeneration !== descriptor.generation) {
          this.flush();
          this.decoderGeneration = descriptor.generation;
        }
        const data = new Float32Array(pcm.length);
        for (let i = 0; i < pcm.length; i++) data[i] = pcm[i] / 32768;
        this.node?.port.postMessage({ type: "pcm", data, rate: descriptor.rate_hz }, [data.buffer]);
        this.lastAudio = performance.now();
        if (s.phase !== "playing") this.patch({ phase: "playing", takenBy: null });
      },
      onHeartbeat: (selected) => {
        const s = this.state.get();
        if (s.kind !== "live" || s.phase === "starting" || s.phase === "error") return;
        if (selected === s.channelId) {
          this.takenSince = null;
          if (s.phase === "taken") this.patch({ phase: "waiting", takenBy: null });
          return;
        }
        // The engine has one selection for every browser. Give a just-sent
        // monitor request a moment before calling it a takeover.
        this.takenSince ??= performance.now();
        if (performance.now() - this.takenSince > 1500 && s.phase !== "taken") {
          this.flush();
          this.patch({ phase: "taken", takenBy: selected });
        }
      },
      onGap: () => this.flush(),
      onClose: () => {
        this.connection = null;
        const s = this.state.get();
        if (s.kind === "live" && s.phase !== "error") {
          // Reconnect quietly; the selection is still ours on the engine.
          setTimeout(() => {
            if (this.state.get().kind === "live" && !this.connection) this.openSocket();
          }, 1000);
        }
      },
    });
  }

  private startWatchdog() {
    if (this.watchdog) return;
    this.watchdog = setInterval(() => {
      const s = this.state.get();
      if (s.kind !== "live") return;
      if (s.phase === "playing" && performance.now() - this.lastAudio > 700) this.patch({ phase: "waiting", level: 0 });
    }, 250);
  }

  private flush() {
    this.node?.port.postMessage({ type: "flush" });
  }

  async stop() {
    const s = this.state.get();
    if (s.kind === "call") {
      this.stopCall();
      this.patch({ ...idle });
      this.updateSession(null, "");
      return;
    }
    this.connection?.close();
    this.connection = null;
    if (this.watchdog) clearInterval(this.watchdog);
    this.watchdog = null;
    this.flush();
    this.patch({ ...idle });
    this.updateSession(null, "");
    // Only clear the engine's selection if it's still ours.
    if (s.kind === "live" && s.channelId && s.phase !== "taken") {
      try {
        await this.backend.monitor(null);
      } catch {
        /* nothing to undo */
      }
    }
  }

  /** Play a recorded call; `queue` continues down the list when it ends, named by `nameFor`. */
  playCall(call: Call, sourceName: string, queue: Call[] = [], nameFor?: (sourceId: string) => string) {
    if (!call.audioUrl) return;
    const s = this.state.get();
    if (s.kind === "live") void this.stop();
    this.stopCall();
    this.queue = queue;
    const element = new Audio(call.audioUrl);
    element.preload = "auto";
    this.callElement = element;
    this.applyVolume();
    element.ontimeupdate = () => this.patch({ position: element.currentTime, duration: element.duration || 0 });
    element.onended = () => {
      const next = this.queue.shift();
      if (next?.audioUrl) this.playCall(next, nameFor?.(next.sourceId) ?? sourceName, this.queue, nameFor);
      else this.patch({ ...idle });
    };
    element.onerror = () => this.patch({ phase: "error", error: "This recording couldn't be played." });
    this.patch({
      ...idle,
      kind: "call",
      phase: "playing",
      sourceId: call.sourceId,
      sourceName,
      callId: call.id,
      channelId: call.channelId,
    });
    this.updateSession(sourceName, "Recorded call");
    void element.play().catch(() => this.patch({ phase: "error", error: "Tap play again to allow audio." }));
  }

  private stopCall() {
    if (!this.callElement) return;
    this.callElement.onended = null;
    this.callElement.pause();
    this.callElement.src = "";
    this.callElement = null;
  }

  private updateSession(title: string | null, artist: string) {
    const session = navigator.mediaSession;
    if (!session) return;
    if (!title) {
      session.metadata = null;
      session.playbackState = "none";
      return;
    }
    session.metadata = new MediaMetadata({ title, artist, album: this.blockName || "Home radio" });
    session.playbackState = "playing";
    const handle = (action: MediaSessionAction, fn: (() => void) | null) => {
      try {
        session.setActionHandler(action, fn);
      } catch {
        /* action not supported on this browser */
      }
    };
    handle("pause", () => void this.stop());
    handle("stop", () => void this.stop());
    handle("play", () => {
      if (this.lastSource) void this.listen(this.lastSource);
    });
    handle("nexttrack", this.queue.length ? () => this.callElement?.dispatchEvent(new Event("ended")) : null);
  }

  dispose() {
    void this.stop();
    void this.context?.close();
  }
}
