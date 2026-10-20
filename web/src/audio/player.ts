import { RawCapture } from "./capture";
import { engineChannelSchema } from "@/api/engine";
import { toSources } from "@/domain/adapt";
import workletUrl from "./pcm-worklet.ts?worker&url";
import { BackendError, type Backend, type Connection } from "@/backend/types";
import type { Call, Mode, Source } from "@/domain/model";
import { createStore } from "@/lib/store";
import { prefsStore } from "@/state/prefs";

export type Phase = "idle" | "starting" | "waiting" | "playing" | "reconnecting" | "taken" | "error";

export interface PlayerState {
  kind: "idle" | "live" | "call";
  raw: boolean;
  captureSeconds: number;
  phase: Phase;
  sourceId: string | null;
  sourceName: string | null;
  channelId: string | null;
  frequencyHz: number | null;
  mode: Mode;
  slot: number | null;
  callId: string | null;
  /** The engine's selection when another device switched it to something else. */
  takenBy: string | null;
  error: string | null;
  muted: boolean;
  volume: number;
  /** Recorded call progress, seconds. */
  position: number;
  duration: number;
  /**
   * Live audio route: true when it goes through an <audio> element (keeps
   * playing with the screen locked), false when this browser refused that and
   * it fell back to the speakers directly, null on desktops (not needed).
   */
  background: boolean | null;
}

const idle = {
  kind: "idle",
  raw: false,
  captureSeconds: 0,
  phase: "idle",
  sourceId: null,
  sourceName: null,
  channelId: null,
  frequencyHz: null,
  mode: "unknown",
  slot: null,
  callId: null,
  takenBy: null,
  error: null,
  position: 0,
  duration: 0,
} satisfies Partial<PlayerState>;

/** Heartbeats arrive every 500 ms; give a just-sent selection this long to show up. */
const SETTLE_MS = 1500;
/** No audio or heartbeat for this long means the socket is dead even if it looks open. */
const SILENT_SOCKET_MS = 3000;

function monitorMessage(error: unknown): string {
  if (error instanceof BackendError) {
    if (error.status === 404) return "Channel gone.";
    return error.message;
  }
  return "Can't reach the radio.";
}

/**
 * One player for live audio and recorded calls, like a music app's single
 * "now playing". Live audio goes WebSocket → AudioWorklet (60 ms prime). On
 * touch devices it is routed through an <audio> element so it keeps playing
 * with the screen locked; desktops connect straight to the speakers.
 *
 * The engine has one listener selection shared by every browser. The player
 * tracks whether that selection is its own: it only clears a selection it
 * made, re-selects when the engine releases it (restart, someone else
 * stopping), and reports a takeover only when another channel was chosen.
 */
export class Player {
  readonly state = createStore<PlayerState>({
    ...idle,
    muted: false,
    volume: prefsStore.get().volume,
    background: null,
  });
  private backend: Backend;
  private capture: RawCapture | null = null;
  private context: AudioContext | null = null;
  private node: AudioWorkletNode | null = null;
  private gain: GainNode | null = null;
  private liveElement: HTMLAudioElement | null = null;
  private ready: Promise<void> | null = null;

  /** Bumped by every listen/stop so stale async continuations bow out. */
  private intent = 0;
  /** Monitor requests go out one at a time, in order. */
  private monitorChain: Promise<unknown> = Promise.resolve();
  /** True once the engine's selection is ours (a monitor request succeeded). */
  private ownsSelection = false;
  /** A monitor request for our channel has been sent and not yet answered. */
  private selectionPending = false;
  private reselecting = false;

  private connection: Connection | null = null;
  private connectionToken = 0;
  private reconnectTimer: ReturnType<typeof setTimeout> | undefined;
  private reconnectAttempt = 0;
  private lastMessage = 0;
  private lastAudio = 0;
  private mismatchSince: number | null = null;
  private releasedSince: number | null = null;
  private decoderGeneration: number | null = null;
  private watchdog: ReturnType<typeof setInterval> | null = null;

  private callElement: HTMLAudioElement | null = null;
  private queue: Call[] = [];
  private nameFor: ((sourceId: string) => string) | undefined;
  private lastSource: Source | null = null;
  private lastRaw = false;
  private blockName = "";
  private onVisible = () => {
    if (document.visibilityState === "visible" && this.state.get().kind === "live") this.resumeAudio();
  };

  constructor(backend: Backend) {
    this.backend = backend;
    document.addEventListener("visibilitychange", this.onVisible);
  }

  private patch(patch: Partial<PlayerState>) {
    this.state.set((s) => ({ ...s, ...patch }));
  }

  setBlockName(name: string) {
    this.blockName = name;
  }

  // ---------------------------------------------------------------- audio graph

  /**
   * Build the audio graph inside the tap handler: mobile browsers only allow
   * audio to start from a gesture, so the context, the <audio> element and its
   * play() call all happen synchronously here. The worklet module loads after.
   */
  private unlock(): Promise<void> {
    if (!this.ready) {
      try {
        const nav = navigator as Navigator & { audioSession?: { type: string } };
        if (nav.audioSession) nav.audioSession.type = "playback";
        const context = new AudioContext({ latencyHint: "interactive" });
        context.onstatechange = () => {
          if (context.state === "suspended" && this.state.get().kind === "live") void context.resume().catch(() => {});
        };
        const gain = context.createGain();
        this.context = context;
        this.gain = gain;
        this.applyVolume();
        const touch = window.matchMedia?.("(pointer: coarse)").matches ?? false;
        if (touch) {
          const destination = context.createMediaStreamDestination();
          gain.connect(destination);
          const element = new Audio();
          element.setAttribute("playsinline", "");
          element.srcObject = destination.stream;
          this.liveElement = element;
          this.patch({ background: true });
        } else {
          gain.connect(context.destination);
          this.patch({ background: null });
        }
        this.ready = context.audioWorklet
          .addModule(workletUrl)
          .then(() => {
            const node = new AudioWorkletNode(context, "pcm-player", { outputChannelCount: [1] });
            node.connect(gain);
            this.node = node;
          })
          .catch((error: unknown) => {
            this.resetAudio();
            throw error;
          });
      } catch (error) {
        this.resetAudio();
        return Promise.reject(error);
      }
    }
    this.resumeAudio();
    return this.ready ?? Promise.reject(new Error("audio unavailable"));
  }

  private resumeAudio() {
    if (this.context && this.context.state !== "closed") void this.context.resume().catch(() => {});
    const element = this.liveElement;
    if (!element) return;
    try {
      void Promise.resolve(element.play()).catch((error: unknown) => {
        // A pause racing play() rejects with AbortError; only a real refusal means "use the speakers".
        const name = error instanceof DOMException ? error.name : "";
        if (name === "NotAllowedError" || name === "NotSupportedError") this.fallbackToSpeakers();
      });
    } catch {
      this.fallbackToSpeakers();
    }
  }

  /** This browser won't play the stream element: use the speakers, and say lock-screen playback is off. */
  private fallbackToSpeakers() {
    if (!this.context || !this.gain || !this.liveElement) return;
    this.gain.disconnect();
    this.gain.connect(this.context.destination);
    this.liveElement = null;
    this.patch({ background: false });
  }

  /** Forget a failed graph so the next tap builds a fresh one. */
  private resetAudio() {
    void this.context?.close().catch(() => {});
    this.context = null;
    this.node = null;
    this.gain = null;
    this.liveElement = null;
    this.ready = null;
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

  private flush() {
    this.node?.port.postMessage({ type: "flush" });
  }

  // ---------------------------------------------------------------- selection

  private sendMonitor(channelId: string | null): Promise<void> {
    const run = this.monitorChain.catch(() => {}).then(() => this.backend.monitor(channelId));
    this.monitorChain = run;
    return run;
  }

  /** Tune a frequency even before it has a catalog entry. Unlock audio in the tap handler. */
  listenFrequency(frequencyHz: number, raw = true) {
    const source = toSources([engineChannelSchema.parse({ id: `tune-${frequencyHz}`,
      frequency_hz: frequencyHz, protocol: "Unknown", in_range: true })], null)[0];
    source.channelIds = [];
    return this.listen(source, { raw });
  }

  /** Selection is idempotent; Stop is an explicit action. */
  async listen(source: Source, { toggle = false, raw = source.mode === "unknown" || source.control || (!source.heardVoice && source.encryption !== "encrypted") }: { toggle?: boolean; raw?: boolean } = {}) {
    const current = this.state.get();
    if (current.kind === "live" && current.sourceId === source.id && current.raw === raw) {
      if (current.phase === "starting" || current.phase === "reconnecting") return;
      if (current.phase === "taken") return this.reclaim();
      if (current.phase !== "error") return toggle ? this.stop() : undefined;
    }
    const canTune = !!this.backend.tune && source.frequencyHz !== null;
    if (!source.listenable.ok && !(raw && canTune) && !(canTune && source.listenable.reason === "Out of range")) {
      if (current.kind !== "idle") await this.stop();
      this.patch({ ...idle, error: source.listenable.reason, sourceName: source.name,
        frequencyHz: source.frequencyHz, mode: source.mode, slot: source.slot, phase: "error" });
      return;
    }
    const clearPrevious = current.kind === "live" && (this.ownsSelection || this.selectionPending);
    if (current.kind !== "idle") this.teardown();
    const intent = ++this.intent;
    let channelId = raw ? null : source.channelIds[0] ?? null;
    this.capture = raw && source.frequencyHz !== null ? new RawCapture(source.frequencyHz) : null;
    this.lastSource = source;
    this.lastRaw = raw;
    const unlocked = this.unlock();
    this.patch({ ...idle, kind: "live", raw, phase: "starting", sourceId: source.id, sourceName: source.name, frequencyHz: source.frequencyHz, mode: source.mode, slot: source.slot, channelId });
    this.updateSession(source.name, "Live");
    try {
      await unlocked;
    } catch {
      if (intent === this.intent)
        this.patch({ phase: "error", error: "Audio didn't start. Tap to retry." });
      return;
    }
    if (intent !== this.intent) return;
    this.selectionPending = true;
    try {
      // Tuning and selection share the stop/reselect queue. An old request may finish,
      // but it must never select its channel after a newer click or Stop.
      const run = this.monitorChain.catch(() => {}).then(async () => {
        if (intent !== this.intent) return;
        if (canTune) {
          const tuned = await this.backend.tune!(source.frequencyHz!, channelId ?? undefined, raw);
          channelId = tuned.id;
        }
        if (intent !== this.intent) return;
        if (!channelId) throw new Error("Frequency tuning unavailable");
        this.patch({ channelId });
        await this.backend.monitor(channelId);
      });
      this.monitorChain = run;
      await run;
    } catch (error) {
      if (intent === this.intent) {
        this.selectionPending = false;
        if (clearPrevious) await this.sendMonitor(null).catch(() => {});
        if (intent === this.intent) this.patch({ phase: "error", error: monitorMessage(error) });
      }
      return;
    }
    if (intent !== this.intent) return;
    this.selectionPending = false;
    this.ownsSelection = true;
    this.patch({ phase: "waiting" });
    this.openSocket();
    this.startWatchdog();
  }

  /** Take the shared selection back after another device switched it. */
  async reclaim() {
    const { kind, channelId } = this.state.get();
    if (kind !== "live" || !channelId) return;
    const intent = this.intent;
    this.mismatchSince = null;
    this.patch({ phase: "waiting", takenBy: null });
    try {
      await this.sendMonitor(channelId);
      if (intent === this.intent) this.ownsSelection = true;
    } catch (error) {
      if (intent === this.intent) this.patch({ phase: "error", error: monitorMessage(error) });
    }
  }

  private onHeartbeat(selected: string | null) {
    const s = this.state.get();
    if (s.kind !== "live" || s.phase === "starting" || s.phase === "error") return;
    const now = performance.now();
    if (selected === s.channelId) {
      this.mismatchSince = null;
      this.releasedSince = null;
      this.ownsSelection = true;
      if (s.phase === "taken" || s.phase === "reconnecting") this.patch({ phase: "waiting", takenBy: null });
      return;
    }
    if (selected === null) {
      // Released: the engine restarted, or someone stopped. Quietly take it back,
      // retrying until the channel is in the engine's catalog again.
      this.releasedSince ??= now;
      // Also after a takeover: the other device let go, so carry on listening.
      if (now - this.releasedSince < SETTLE_MS || this.reselecting) return;
      this.reselecting = true;
      this.releasedSince = null;
      const intent = this.intent;
      this.sendMonitor(s.channelId)
        .then(() => {
          if (intent === this.intent) this.ownsSelection = true;
        })
        .catch(() => {})
        .finally(() => (this.reselecting = false));
      return;
    }
    this.mismatchSince ??= now;
    if (now - this.mismatchSince > SETTLE_MS && s.phase !== "taken") {
      this.flush();
      this.ownsSelection = false;
      this.patch({ phase: "taken", takenBy: selected });
    }
  }

  // ---------------------------------------------------------------- socket

  private openSocket() {
    if (this.connection) return;
    const token = ++this.connectionToken;
    this.lastMessage = performance.now();
    this.connection = this.backend.listen({
      onAudio: (descriptor, pcm) => {
        if (token !== this.connectionToken) return;
        this.lastMessage = performance.now();
        const s = this.state.get();
        if (s.kind !== "live" || descriptor.channel_id !== s.channelId || s.phase === "taken") return;
        if (this.decoderGeneration !== descriptor.generation) {
          this.flush();
          this.decoderGeneration = descriptor.generation;
        }
        if (s.raw && descriptor.raw === true && descriptor.rate_hz === 48000) {
          this.capture?.push(descriptor, pcm);
          this.patch({ captureSeconds: this.capture?.seconds ?? 0 });
        }
        const data = new Float32Array(pcm.length);
        for (let i = 0; i < pcm.length; i++) data[i] = pcm[i] / 32768;
        this.node?.port.postMessage({ type: "pcm", data, rate: descriptor.rate_hz }, [data.buffer]);
        this.lastAudio = performance.now();
        this.reconnectAttempt = 0;
        if (s.phase !== "playing") this.patch({ phase: "playing", takenBy: null });
      },
      onHeartbeat: (selected) => {
        if (token !== this.connectionToken) return;
        this.lastMessage = performance.now();
        this.reconnectAttempt = 0;
        this.onHeartbeat(selected);
      },
      onGap: () => {
        if (token === this.connectionToken) { this.flush(); this.capture?.gap("socket gap"); }
      },
      onClose: () => {
        if (token !== this.connectionToken) return;
        this.connection = null;
        this.capture?.gap("socket closed");
        this.scheduleReconnect();
      },
    });
  }

  private dropSocket() {
    this.connectionToken += 1;
    const connection = this.connection;
    this.connection = null;
    connection?.close();
  }

  private scheduleReconnect() {
    const s = this.state.get();
    if (s.kind !== "live" || s.phase === "error") return;
    if (s.phase !== "taken") this.patch({ phase: "reconnecting" });
    clearTimeout(this.reconnectTimer);
    const delay = Math.min(10_000, 500 * 2 ** this.reconnectAttempt) * (0.8 + Math.random() * 0.4);
    this.reconnectAttempt += 1;
    this.reconnectTimer = setTimeout(() => {
      if (this.state.get().kind === "live" && !this.connection) this.openSocket();
    }, delay);
  }

  private startWatchdog() {
    if (this.watchdog) return;
    this.watchdog = setInterval(() => {
      const s = this.state.get();
      if (s.kind !== "live") return;
      const now = performance.now();
      if (s.phase === "playing" && now - this.lastAudio > 700) this.patch({ phase: "waiting" });
      if (this.connection && now - this.lastMessage > SILENT_SOCKET_MS) {
        this.dropSocket();
        this.scheduleReconnect();
      }
    }, 250);
  }

  // ---------------------------------------------------------------- stop

  /** Stop everything this player is doing, without touching the engine. */
  private teardown() {
    this.intent += 1;
    this.ownsSelection = false;
    this.selectionPending = false;
    this.stopCall();
    this.queue = [];
    this.dropSocket();
    clearTimeout(this.reconnectTimer);
    if (this.watchdog) clearInterval(this.watchdog);
    this.watchdog = null;
    this.flush();
    this.decoderGeneration = null;
    this.mismatchSince = null;
    this.releasedSince = null;
  }

  async stop() {
    const s = this.state.get();
    // A selection we made, or one already sent and on its way, is ours to clear.
    const clear = s.kind === "live" && (this.ownsSelection || this.selectionPending);
    this.ownsSelection = false;
    this.selectionPending = false;
    this.teardown();
    this.patch({ ...idle, captureSeconds: this.capture?.seconds ?? 0,
      frequencyHz: s.frequencyHz, channelId: s.channelId, sourceName: s.sourceName, mode: s.mode, slot: s.slot });
    this.updateSession(null, "");
    this.liveElement?.pause();
    if (clear) {
      try {
        await this.sendMonitor(null);
      } catch {
        /* nothing to undo */
      }
    }
  }

  resume() {
    if (this.lastSource) return this.listen(this.lastSource, { raw: this.lastRaw });
  }

  listenRaw() {
    const hz = this.state.get().frequencyHz;
    if (hz !== null) return this.listenFrequency(hz, true);
  }

  saveCapture() {
    if (!this.capture?.seconds) return;
    const url = URL.createObjectURL(new Blob([this.capture.wav()], { type: "audio/wav" }));
    const link = document.createElement("a");
    link.href = url;
    link.download = `audio_${this.capture.frequencyHz}Hz_raw-fm_${this.capture.startedAt.replace(/[:.]/g, "-")}.wav`;
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }

  // ---------------------------------------------------------------- recorded calls

  /** Play a recorded call; `queue` continues down the list when it ends, named by `nameFor`. */
  playCall(call: Call, sourceName: string, queue: Call[] = [], nameFor?: (sourceId: string) => string) {
    if (!call.audioUrl) return;
    if (this.state.get().kind === "live") void this.stop();
    else this.stopCall();
    this.queue = [...queue];
    this.nameFor = nameFor;
    const element = (this.callElement ??= this.makeCallElement());
    element.src = call.audioUrl;
    this.applyVolume();
    this.patch({
      ...idle,
      kind: "call",
      phase: "playing",
      sourceId: call.sourceId,
      sourceName,
      frequencyHz: call.frequencyHz ?? null,
      mode: call.mode ?? "unknown",
      slot: call.slot ?? null,
      callId: call.id,
      channelId: call.channelId,
    });
    this.updateSession(sourceName, "Recorded call");
    const callId = call.id;
    try {
      void Promise.resolve(element.play()).catch(() => {
        if (this.state.get().callId === callId) this.patch({ phase: "error", error: "Tap play again." });
      });
    } catch {
      this.patch({ phase: "error", error: "Can't play recordings here." });
    }
  }

  /** One element for every recorded call, so a locked phone can keep advancing the queue. */
  private makeCallElement() {
    const element = new Audio();
    element.preload = "auto";
    element.ontimeupdate = () => {
      if (this.state.get().kind === "call") this.patch({ position: element.currentTime, duration: element.duration || 0 });
    };
    element.onended = () => this.advance();
    element.onerror = () => {
      if (this.state.get().kind === "call" && element.getAttribute("src"))
        this.patch({ phase: "error", error: "Couldn't play." });
    };
    return element;
  }

  private advance() {
    const next = this.queue.shift();
    if (next?.audioUrl) this.playCall(next, this.nameFor?.(next.sourceId) ?? "Recorded call", this.queue, this.nameFor);
    else {
      this.stopCall();
      this.patch({ ...idle });
      this.updateSession(null, "");
    }
  }

  private stopCall() {
    const element = this.callElement;
    if (!element || !element.getAttribute("src")) return;
    element.pause();
    element.removeAttribute("src");
    element.load();
  }

  // ---------------------------------------------------------------- lock screen

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
    handle("nexttrack", this.queue.length ? () => this.advance() : null);
  }

  dispose() {
    document.removeEventListener("visibilitychange", this.onVisible);
    void this.stop();
    this.resetAudio();
  }
}
