import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { engineChannelSchema, type AudioDescriptor } from "@/api/engine";
import { BackendError, type Backend, type ListenHandlers } from "@/backend/types";
import type { Call, Source } from "@/domain/model";
import { isAudible, Player } from "./player";

function source(id: string, channel = `${id}-DMR-1`): Source {
  return {
    id,
    kind: "channel",
    name: id,
    named: true,
    summary: "",
    description: "",
    probableUser: null,
    trafficKind: null,
    mode: "dmr",
    encryption: "clear",
    frequencyHz: 454_000_000,
    slot: 1,
    system: null,
    talkgroupId: null,
    channelIds: [channel],
    talking: true,
    signal: true,
    control: false,
    heardVoice: true,
    levelDb: null,
    lastHeardAt: null,
    lastSignalAt: null,
    inRange: true,
    listenable: { ok: true },
  };
}

function audio(channelId: string, generation = 1): AudioDescriptor {
  return {
    type: "audio",
    encoding: "s16le",
    channels: 1,
    rate_hz: 8000,
    samples: 2,
    channel_id: channelId,
    generation,
    source_generation: 1,
    sequence: 1,
    ingress_age_ms: 1,
  };
}

/** A backend whose monitor() calls resolve when the test says so. */
function fakeBackend() {
  const sockets: ListenHandlers[] = [];
  const pending: { id: string | null; resolve(): void; reject(e: unknown): void }[] = [];
  let auto = true;
  const monitor = vi.fn(
    (id: string | null) =>
      new Promise<void>((resolve, reject) => {
        if (auto) resolve();
        else pending.push({ id, resolve, reject });
      }),
  );
  const backend = {
    mode: "live",
    monitor,
    listen: vi.fn((handlers: ListenHandlers) => {
      sockets.push(handlers);
      return { close: () => handlers.onClose() };
    }),
  } as unknown as Backend;
  return {
    backend,
    monitor,
    socket: () => sockets.at(-1)!,
    sockets,
    pending,
    manual() {
      auto = false;
    },
  };
}

async function settle() {
  for (let i = 0; i < 5; i++) await Promise.resolve();
  await vi.advanceTimersByTimeAsync(0);
}

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval", "Date", "performance"] });
});
afterEach(() => {
  vi.useRealTimers();
});

describe("Player live listening", () => {
  it("fine tunes a saved out-of-range channel and preserves its slot identity", async () => {
    const f = fakeBackend();
    const target = source("saved", "452675000-DMR-2");
    target.frequencyHz = 452675000;
    target.slot = 2;
    target.inRange = false;
    target.listenable = { ok: false, reason: "Out of range" };
    f.backend.tune = vi.fn(async () => engineChannelSchema.parse({ id: target.channelIds[0],
      frequency_hz: target.frequencyHz, protocol: "DMR", slot: 2, in_range: true }));
    const player = new Player(f.backend);
    await player.listen(target);
    expect(f.backend.tune).toHaveBeenCalledWith(452675000, "452675000-DMR-2", false);
    expect(f.monitor).toHaveBeenCalledWith("452675000-DMR-2");
    expect(player.state.get()).toMatchObject({ frequencyHz: 452675000, slot: 2, phase: "waiting" });
    await player.stop();
  });

  it("never selects a late tune response after Stop", async () => {
    const f = fakeBackend();
    let finish!: (value: ReturnType<typeof engineChannelSchema.parse>) => void;
    f.backend.tune = vi.fn(() => new Promise<ReturnType<typeof engineChannelSchema.parse>>((resolve) => { finish = resolve; }));
    const player = new Player(f.backend);
    const listening = player.listenFrequency(452675000);
    await settle();
    const stopping = player.stop();
    finish(engineChannelSchema.parse({ id: "452675000-Unknown-1", frequency_hz: 452675000 }));
    await Promise.all([listening, stopping]);
    expect(f.monitor.mock.calls).toEqual([[null]]);
    expect(player.state.get().kind).toBe("idle");
  });

  it("reports the actual tuning error and clears the previous channel", async () => {
    const f = fakeBackend();
    const player = new Player(f.backend);
    await player.listen(source("a"));
    f.backend.tune = vi.fn().mockRejectedValue(new BackendError("Frequency is outside the receive window", 422));
    await player.listen(source("b"));
    expect(f.monitor).toHaveBeenLastCalledWith(null);
    expect(player.state.get()).toMatchObject({ phase: "error", error: "Frequency is outside the receive window" });
    await player.stop();
  });

  it("keeps explicit Listen playing when the same frequency is submitted again", async () => {
    const f = fakeBackend();
    const player = new Player(f.backend);
    await player.listen(source("a"), { toggle: false });
    f.socket().onAudio(audio("a-DMR-1"), new Int16Array([1, 2]));
    await player.listen(source("a"), { toggle: false });
    expect(player.state.get()).toMatchObject({ kind: "live", phase: "playing", channelId: "a-DMR-1" });
    expect(f.monitor).toHaveBeenCalledTimes(1);
    expect(f.backend.listen).toHaveBeenCalledTimes(1);
    // A repeated channel selection keeps playback armed; stopping is explicit.
    await player.listen(source("a"));
    expect(player.state.get().kind).toBe("live");
    await player.stop();
    expect(f.monitor).toHaveBeenLastCalledWith(null);
    expect(player.state.get().kind).toBe("idle");
  });
  it("selects the channel, plays audio, and clears its own selection on stop", async () => {
    const f = fakeBackend();
    const player = new Player(f.backend);
    await player.listen(source("a"));
    expect(f.monitor).toHaveBeenCalledWith("a-DMR-1");
    expect(player.state.get().phase).toBe("waiting");
    f.socket().onAudio(audio("a-DMR-1"), new Int16Array([1, 2]));
    expect(player.state.get().phase).toBe("playing");
    await player.stop();
    expect(f.monitor).toHaveBeenLastCalledWith(null);
    expect(player.state.get().kind).toBe("idle");
  });

  it("ignores audio for a channel it didn't pick", async () => {
    const f = fakeBackend();
    const player = new Player(f.backend);
    await player.listen(source("a"));
    f.socket().onAudio(audio("other-DMR-1"), new Int16Array([1, 2]));
    expect(player.state.get().phase).toBe("waiting");
  });

  it("mutes only speaker output and unmute restores a zeroed volume without changing the held channel", async () => {
    const f = fakeBackend();
    const player = new Player(f.backend);
    await player.listen(source("a"));
    f.socket().onAudio(audio("a-DMR-1"), new Int16Array([1, 2]));
    player.setVolume(0.35);
    expect(isAudible(player.state.get())).toBe(true);
    player.toggleMute();
    expect(isAudible(player.state.get())).toBe(false);
    expect(player.state.get()).toMatchObject({ kind: "live", phase: "playing", volume: 0.35, muted: true });
    player.toggleMute();
    expect(isAudible(player.state.get())).toBe(true);
    player.setVolume(0);
    expect(isAudible(player.state.get())).toBe(false);
    player.toggleMute();
    expect(player.state.get()).toMatchObject({ volume: 0.35, muted: false });
    expect(f.monitor.mock.calls).toEqual([["a-DMR-1"]]);
    await player.stop();
  });

  it("does not let socket audio hide a selection error", async () => {
    const f = fakeBackend();
    const player = new Player(f.backend);
    await player.listen(source("a"));
    f.socket().onHeartbeat("b-DMR-1");
    vi.advanceTimersByTime(1600);
    f.socket().onHeartbeat("b-DMR-1");
    f.monitor.mockRejectedValueOnce(new BackendError("Receiver unavailable", 503));
    await player.reclaim();
    f.socket().onAudio(audio("a-DMR-1"), new Int16Array([1, 2]));
    expect(player.state.get()).toMatchObject({ phase: "error", error: "Receiver unavailable" });
    expect(isAudible(player.state.get())).toBe(false);
    await player.stop();
  });

  it("doesn't clear anyone's selection after its own request failed", async () => {
    const f = fakeBackend();
    f.monitor.mockRejectedValueOnce(new BackendError("Channel protocol is not confirmed yet", 422));
    const player = new Player(f.backend);
    await player.listen(source("a"));
    expect(player.state.get()).toMatchObject({ phase: "error" });
    expect(player.state.get().error).toBe("Channel protocol is not confirmed yet");
    await player.stop();
    expect(f.monitor).toHaveBeenCalledTimes(1);
  });

  it("reports a takeover only for another channel, and only after it settles", async () => {
    const f = fakeBackend();
    const player = new Player(f.backend);
    await player.listen(source("a"));
    f.socket().onHeartbeat("b-DMR-1");
    expect(player.state.get().phase).toBe("waiting");
    vi.advanceTimersByTime(1600);
    f.socket().onHeartbeat("b-DMR-1");
    expect(player.state.get()).toMatchObject({ phase: "taken", takenBy: "b-DMR-1" });
    // Stopping after a takeover must not clear the other device's choice.
    await player.stop();
    expect(f.monitor).toHaveBeenCalledTimes(1);
  });

  it("switches back on request", async () => {
    const f = fakeBackend();
    const player = new Player(f.backend);
    await player.listen(source("a"));
    f.socket().onHeartbeat("b-DMR-1");
    vi.advanceTimersByTime(1600);
    f.socket().onHeartbeat("b-DMR-1");
    await player.reclaim();
    expect(f.monitor).toHaveBeenLastCalledWith("a-DMR-1");
    expect(player.state.get().phase).toBe("waiting");
  });

  it("quietly re-selects when the engine releases the selection (restart, someone stopped)", async () => {
    const f = fakeBackend();
    const player = new Player(f.backend);
    await player.listen(source("a"));
    f.socket().onHeartbeat(null);
    vi.advanceTimersByTime(1600);
    f.socket().onHeartbeat(null);
    await settle();
    expect(f.monitor).toHaveBeenCalledTimes(2);
    expect(f.monitor).toHaveBeenLastCalledWith("a-DMR-1");
    expect(player.state.get().phase).not.toBe("taken");
  });

  it("sends selections in tap order even when the first is slow", async () => {
    const f = fakeBackend();
    f.manual();
    const player = new Player(f.backend);
    const first = player.listen(source("a"));
    await settle();
    const second = player.listen(source("b"));
    await settle();
    expect(f.pending.map((p) => p.id)).toEqual(["a-DMR-1"]);
    f.pending.shift()!.resolve();
    await settle();
    expect(f.pending.map((p) => p.id)).toEqual(["b-DMR-1"]);
    f.pending.shift()!.resolve();
    await Promise.all([first, second]);
    expect(player.state.get()).toMatchObject({ sourceId: "b", phase: "waiting" });
  });

  it("clears a selection still on its way when stopped during start", async () => {
    const f = fakeBackend();
    f.manual();
    const player = new Player(f.backend);
    const listening = player.listen(source("a"));
    await settle();
    const stopping = player.stop();
    f.pending.shift()!.resolve();
    await settle();
    f.pending.shift()!.resolve();
    await Promise.all([listening, stopping]);
    expect(f.monitor.mock.calls.map((c) => c[0])).toEqual(["a-DMR-1", null]);
    expect(player.state.get().kind).toBe("idle");
  });

  it("reconnects a socket that closes, and ignores a stale socket's late close", async () => {
    const f = fakeBackend();
    const player = new Player(f.backend);
    await player.listen(source("a"));
    const old = f.socket();
    old.onClose();
    expect(player.state.get().phase).toBe("reconnecting");
    await vi.advanceTimersByTimeAsync(1000);
    expect(f.sockets).toHaveLength(2);
    old.onClose();
    await vi.advanceTimersByTimeAsync(2000);
    // The stale close didn't schedule another socket; the live one heartbeats and recovers.
    expect(f.sockets).toHaveLength(2);
    f.socket().onHeartbeat("a-DMR-1");
    expect(player.state.get().phase).toBe("waiting");
  });

  it("replaces a socket that goes silent", async () => {
    const f = fakeBackend();
    const player = new Player(f.backend);
    await player.listen(source("a"));
    await vi.advanceTimersByTimeAsync(3500);
    expect(player.state.get().phase).toBe("reconnecting");
    await vi.advanceTimersByTimeAsync(1000);
    expect(f.sockets.length).toBeGreaterThanOrEqual(2);
  });

  it("lets the next tap retry after the audio worklet failed to load", async () => {
    const f = fakeBackend();
    const addModule = vi.fn().mockRejectedValueOnce(new Error("blocked")).mockResolvedValue(undefined);
    const Original = globalThis.AudioContext;
    vi.stubGlobal(
      "AudioContext",
      class extends (Original as unknown as { new (): object }) {
        audioWorklet = { addModule };
      },
    );
    const player = new Player(f.backend);
    await player.listen(source("a"));
    expect(player.state.get().error).toMatch(/Audio didn't start/);
    expect(f.monitor).not.toHaveBeenCalled();
    await player.listen(source("a"));
    expect(player.state.get().phase).toBe("waiting");
    vi.stubGlobal("AudioContext", Original);
  });
});

describe("Player start-up races", () => {
  it("doesn't send a stale selection when stopped while audio is still starting", async () => {
    const f = fakeBackend();
    let finishLoad: () => void = () => {};
    const Original = globalThis.AudioContext;
    vi.stubGlobal(
      "AudioContext",
      class extends (Original as unknown as { new (): object }) {
        audioWorklet = { addModule: () => new Promise<void>((r) => (finishLoad = r)) };
      },
    );
    const player = new Player(f.backend);
    const listening = player.listen(source("a"));
    await player.stop();
    finishLoad();
    await listening;
    await settle();
    // Nothing was sent for this device, so nothing is cleared (it might be someone else's channel).
    expect(f.monitor).not.toHaveBeenCalled();
    expect(player.state.get().kind).toBe("idle");
    vi.stubGlobal("AudioContext", Original);
  });
});

describe("Player recorded calls", () => {
  const call = (id: string): Call => ({
    frequencyHz: 452675000,
    mode: "dmr",
    slot: 2,
    id,
    sourceId: "a",
    channelId: null,
    startedAt: 0,
    endedAt: 1000,
    encrypted: false,
    audioUrl: `blob:${id}`,
    talker: null,
    transcript: null,
  });

  it("stopping a call doesn't surface as an error, and the queue advances", async () => {
    const f = fakeBackend();
    const player = new Player(f.backend);
    player.playCall(call("1"), "A", [call("2")], () => "A");
    const element = (player as unknown as { callElement: HTMLAudioElement }).callElement;
    element.dispatchEvent(new Event("ended"));
    await settle();
    expect(player.state.get()).toMatchObject({ kind: "call", callId: "2", phase: "playing",
      frequencyHz: 452675000, mode: "dmr", slot: 2 });
    void player.stop();
    element.dispatchEvent(new Event("error"));
    expect(player.state.get()).toMatchObject({ kind: "idle", phase: "idle" });
  });

  it("waits for recording playback to start and reflects buffering", async () => {
    const f = fakeBackend();
    const player = new Player(f.backend);
    let start!: () => void;
    const play = vi.spyOn(HTMLMediaElement.prototype, "play").mockImplementationOnce(() => new Promise<void>((resolve) => { start = resolve; }));
    player.playCall(call("1"), "A");
    expect(player.state.get().phase).toBe("starting");
    expect(isAudible(player.state.get())).toBe(false);
    start();
    await settle();
    expect(isAudible(player.state.get())).toBe(true);
    const element = (player as unknown as { callElement: HTMLAudioElement }).callElement;
    element.dispatchEvent(new Event("waiting"));
    expect(player.state.get().phase).toBe("waiting");
    expect(isAudible(player.state.get())).toBe(false);
    element.dispatchEvent(new Event("playing"));
    expect(isAudible(player.state.get())).toBe(true);
    await player.stop();
    play.mockRestore();
  });
});
