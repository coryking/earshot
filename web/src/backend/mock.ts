/**
 * Demo/test backend. Simulates the engine's real wire shapes (status, channel
 * events, listener audio, spectrum) and the "frontend needs" endpoints (blocks,
 * catalog, calls with audio, activity with a baseline). All names are sample
 * data, not identifications of real users.
 */
import type { AudioDescriptor, EngineChannel, EngineEvent, EngineStatus } from "@/api/engine";
import type { WireActivity, WireBlocks, WireCall, WireCatalogEntry } from "@/api/needs";
import type { SpectrumBatch, SpectrumRow } from "@/domain/model";
import { scaleToByte } from "./spectrum";
import { BackendError, type Backend, type CallQuery, type ListenHandlers, type Need } from "./types";

export function mulberry32(seed: number) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** Speech-like audio: a wobbling voiced tone with syllable envelopes and a little hiss. */
export function synthVoice(seed: number, samples: number, rate = 8000, offset = 0): Int16Array {
  const out = new Int16Array(samples);
  const pitch = 95 + (seed % 7) * 18;
  for (let i = 0; i < samples; i++) {
    const t = (i + offset) / rate;
    const syllable = Math.max(0, Math.sin(2 * Math.PI * (3.1 + (seed % 3) * 0.4) * t)) ** 0.6;
    const phrase = Math.sin(2 * Math.PI * 0.23 * t + seed) > -0.6 ? 1 : 0.05;
    const f0 = pitch * (1 + 0.08 * Math.sin(2 * Math.PI * 1.7 * t + seed));
    let v = 0;
    for (let h = 1; h <= 9; h++) {
      const formant = Math.exp(-(((h * f0 - 600 - 300 * Math.sin(t * 5 + seed)) / 450) ** 2));
      v += (Math.sin(2 * Math.PI * h * f0 * t) * (0.35 + formant)) / h;
    }
    const hiss = Math.sin(i * 12.9898 + seed * 78.233) * 43758.5453;
    v = v * syllable * phrase * 0.45 + (hiss - Math.floor(hiss) - 0.5) * 0.03;
    out[i] = Math.max(-32767, Math.min(32767, Math.round(v * 32767 * 0.6)));
  }
  return out;
}

export function wavUrl(pcm: Int16Array, rate = 8000): string {
  const buffer = new ArrayBuffer(44 + pcm.length * 2);
  const view = new DataView(buffer);
  const text = (offset: number, s: string) => s.split("").forEach((c, i) => view.setUint8(offset + i, c.charCodeAt(0)));
  text(0, "RIFF");
  view.setUint32(4, 36 + pcm.length * 2, true);
  text(8, "WAVE");
  text(12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, rate, true);
  view.setUint32(28, rate * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  text(36, "data");
  view.setUint32(40, pcm.length * 2, true);
  new Int16Array(buffer, 44).set(pcm);
  return URL.createObjectURL(new Blob([buffer], { type: "audio/wav" }));
}

interface Def {
  entry: WireCatalogEntry;
  protocol: string;
  /** Calls per hour. */
  rate: number;
  /** Mean call length, seconds. */
  length: number;
  weight: number;
}

const BLOCK_CENTER = 454_216_998;
const BLOCK_SPAN = 6_000_000;

function def(
  id: string,
  name: string,
  frequency: number,
  protocol: string,
  slot: number,
  extra: Partial<WireCatalogEntry> & { rate: number; length: number; weight: number },
): Def {
  const { rate, length, weight, ...rest } = extra;
  return {
    protocol,
    rate,
    length,
    weight,
    entry: {
      id,
      kind: "channel",
      channel_ids: [`${frequency}-${protocol}-${slot}`],
      frequency_hz: frequency,
      slot,
      system: null,
      talkgroup_id: null,
      name,
      description: "",
      probable_user: null,
      traffic_kind: null,
      mode: protocol === "DMR" ? "dmr" : protocol === "NXDN48" ? "nxdn" : protocol === "FM" ? "fm" : "unknown",
      encryption: "clear",
      ...rest,
    },
  };
}

/**
 * Sample catalog for the 454 MHz block, using the carriers measured in the
 * 2026-10-19 wideband capture and the probable users from
 * docs/rf-environment-seattle.md § 6 (rf-research branch). A licence is not
 * proof of who is talking; descriptions say "probably".
 */
export const MOCK_DEFS: Def[] = [
  def("columbia-shared", "Columbia Tower shared radio", 454_029_223, "DMR", 2, {
    description: "Shared business radio system run from Columbia Tower",
    probable_user: "Businesses renting time on a shared radio network (TRBOWEST / Day Wireless)",
    traffic_kind: "Work chatter from many small businesses: deliveries, security, building crews",
    rate: 40, length: 9, weight: 3,
  }),
  def("ambassadors", "Downtown ambassadors", 452_685_504, "DMR", 1, {
    description: "Street ambassadors and sidewalk cleaning crews",
    probable_user: "Probably the Metropolitan Improvement District (FCC licence WPQB331)",
    traffic_kind: "Cleanups, people needing help, sidewalk issues",
    rate: 30, length: 7, weight: 2.5,
  }),
  def("convention", "Convention Center radio", 451_360_300, "DMR", 1, {
    description: "Event, security and building staff",
    probable_user: "Probably the Washington State Convention Center (FCC licence WQJB646)",
    traffic_kind: "Doors, loading docks, event setup; busiest on show days",
    rate: 14, length: 8, weight: 1.5,
  }),
  def("fleetnet", "FleetNet business radio", 452_341_800, "NXDN48", 1, {
    description: "Shared radio network for service fleets",
    probable_user: "Customers of FleetNet / Silke Communications",
    traffic_kind: "Dispatch for trades and delivery fleets",
    rate: 12, length: 11, weight: 1.2,
  }),
  def("amazon", "Amazon operations", 452_160_600, "DMR", 1, {
    description: "Campus operations and facilities",
    probable_user: "Probably Amazon's downtown campus (FCC licence WRFI741)",
    traffic_kind: "Facilities, security and building operations",
    rate: 10, length: 6, weight: 1,
  }),
  def("stadium", "Stadium and event staff", 452_985_500, "DMR", 1, {
    description: "Event staff around the stadiums",
    probable_user: "Probably Lumen Field event operations, or a nearby venue sharing the channel",
    traffic_kind: "Gates, crowds, ushers; busy only around games and concerts",
    rate: 4, length: 6, weight: 0.4,
  }),
  def("hotels", "Hotels and buildings", 451_773_100, "FM", 1, {
    description: "A shared channel many downtown hotels and buildings use",
    probable_user: "One of dozens of hotels and office buildings licensed on this channel",
    traffic_kind: "Housekeeping, maintenance, door staff",
    rate: 12, length: 5, weight: 0.8,
  }),
  def("private", "Private business radio", 454_554_500, "DMR", 1, {
    description: "Private (encrypted)",
    probable_user: "A subscriber on the Day Wireless network at Columbia Tower",
    traffic_kind: "Encrypted: you can see when they talk, but not hear it",
    encryption: "encrypted",
    rate: 16, length: 10, weight: 1.4,
  }),
];

/** Signals the engine has seen that nobody has labelled. */
const UNLABELLED: { frequency: number; protocol: string; slot: number; rate: number; length: number }[] = [
  { frequency: 452_735_400, protocol: "DMR", slot: 1, rate: 9, length: 6 },
  { frequency: 454_347_600, protocol: "Unknown", slot: 1, rate: 20, length: 4 },
];

export const MOCK_BLOCKS: WireBlocks = {
  current_id: "downtown",
  blocks: [
    {
      id: "downtown",
      name: "Downtown business radio",
      description: "Business radios around downtown Seattle: hotels, venues, building crews and shared business networks.",
      center_hz: BLOCK_CENTER,
      span_hz: BLOCK_SPAN,
    },
    {
      id: "upper-business",
      name: "Upper business band",
      description: "More business and utility radios, 461 to 467 MHz.",
      center_hz: 464_200_000,
      span_hz: 6_000_000,
    },
    {
      id: "family",
      name: "Family and hiking radios",
      description: "Walkie-talkies people buy at the store (FRS and GMRS), 462 to 468 MHz.",
      center_hz: 465_000_000,
      span_hz: 6_000_000,
    },
  ],
};

interface Live {
  talkingUntil: number;
  callId: string | null;
  startedAt: number;
}

export interface MockOptions {
  seed?: number;
  now?: () => number;
  /** Simulation step, ms. */
  tickMs?: number;
  /** Source ids talking at start, so a fresh demo isn't silent. */
  startTalking?: string[];
  /** Skip timers (unit tests drive `step()` directly). */
  manual?: boolean;
}

const BUCKET = 300_000;

export class MockBackend implements Backend {
  readonly mode = "demo" as const;
  private rand: () => number;
  private now: () => number;
  private channels = new Map<string, EngineChannel>();
  private live = new Map<string, Live>();
  private calls_: WireCall[] = [];
  private audioUrls = new Map<string, string>();
  private eventSubs = new Set<(e: EngineEvent) => void>();
  private listenSubs = new Set<ListenHandlers>();
  private selected: string | null = null;
  private spectrumRows: SpectrumRow[] = [];
  private spectrumSeq = 0;
  private activity_: WireActivity;
  private timers: ReturnType<typeof setInterval>[] = [];
  private audioOffset = 0;
  private generation = 1;
  private decoderGeneration = 1;
  private state: "receiving" | "stopped" = "receiving";
  private catalog_: WireCatalogEntry[];
  private blockId = "downtown";
  private callCounter = 0;

  constructor(options: MockOptions = {}) {
    this.rand = mulberry32(options.seed ?? 7);
    this.now = options.now ?? Date.now;
    this.catalog_ = MOCK_DEFS.map((d) => structuredClone(d.entry));
    const now = this.now();
    for (const d of MOCK_DEFS) this.addChannel(d.entry.frequency_hz!, d.protocol, d.entry.slot!);
    for (const u of UNLABELLED) this.addChannel(u.frequency, u.protocol, u.slot);
    // The engine keeps an Unknown placeholder beside identified entries.
    this.addChannel(454_029_223, "Unknown", 1);
    this.activity_ = this.buildActivity(now);
    this.backfillCalls(now);
    for (const id of options.startTalking ?? ["columbia-shared", "ambassadors"]) {
      const d = MOCK_DEFS.find((x) => x.entry.id === id);
      if (d) this.startCall(d.entry.channel_ids[0], now, 25 + this.rand() * 20, d.entry.encryption === "encrypted");
    }
    for (let i = 0; i < 400; i++) this.spectrumRow(now - (400 - i) * 100);
    if (!options.manual) {
      const tick = options.tickMs ?? 200;
      this.timers.push(setInterval(() => this.step(tick), tick));
      this.timers.push(setInterval(() => this.spectrumRow(this.now()), 100));
      this.timers.push(setInterval(() => this.pumpAudio(60), 60));
      this.timers.push(setInterval(() => this.heartbeat(), 500));
    }
  }

  dispose() {
    this.timers.forEach(clearInterval);
    this.audioUrls.forEach((url) => URL.revokeObjectURL(url));
  }

  private addChannel(frequency: number, protocol: string, slot: number) {
    const id = `${frequency}-${protocol}-${slot}`;
    this.channels.set(id, {
      id,
      stream_id: Math.round((frequency - (BLOCK_CENTER - BLOCK_SPAN / 2)) / 50_000) + 1,
      frequency_hz: frequency,
      protocol,
      slot,
      kind: protocol === "Unknown" ? "unidentified" : "voice",
      rf_active: false,
      decoded: protocol !== "Unknown",
      speech_active: false,
      last_heard: null,
      last_rf: null,
      level_db: -118,
      audio_seconds: 0,
      sequence: 0,
      in_range: true,
      available: true,
      decoder_admitted: true,
    });
  }

  private defFor(channelId: string) {
    return MOCK_DEFS.find((d) => d.entry.channel_ids.includes(channelId));
  }

  private emit(event: EngineEvent) {
    this.eventSubs.forEach((fn) => fn(event));
  }

  private update(id: string, patch: Partial<EngineChannel>) {
    const channel = this.channels.get(id);
    if (!channel) return;
    Object.assign(channel, patch);
    this.emit({ type: "channel", generation: this.generation, channel: { ...channel } });
  }

  private startCall(id: string, now: number, seconds: number, encrypted: boolean) {
    const channel = this.channels.get(id);
    if (!channel || this.state !== "receiving" || !channel.in_range) return;
    const unknown = channel.protocol === "Unknown";
    const audible = !unknown && !encrypted && channel.protocol !== "FM";
    const callId = unknown ? null : `call-${++this.callCounter}`;
    this.live.set(id, { talkingUntil: now + seconds * 1000, callId, startedAt: now });
    this.update(id, {
      rf_active: true,
      speech_active: audible || channel.protocol === "FM",
      level_db: -62 - this.rand() * 18,
      last_rf: now / 1000,
      last_heard: unknown ? channel.last_heard : now / 1000,
    });
    if (callId) {
      const call = this.makeCall(callId, id, now, null, encrypted);
      this.calls_.unshift(call);
      this.emit({ type: "call", call });
    }
  }

  private makeCall(callId: string, channelId: string, start: number, end: number | null, encrypted: boolean): WireCall {
    const d = this.defFor(channelId);
    const playable = !encrypted && this.channels.get(channelId)?.protocol !== "FM";
    return {
      id: callId,
      source_id: d?.entry.id ?? channelId,
      channel_id: channelId,
      started_at: start / 1000,
      ended_at: end === null ? null : end / 1000,
      encrypted,
      audio_url: playable && end !== null ? `mock:${callId}` : null,
      talker: null,
      transcript: null,
    };
  }

  private endCall(id: string, now: number) {
    const live = this.live.get(id);
    this.live.delete(id);
    this.update(id, { rf_active: false, speech_active: false, level_db: -118 });
    const index = this.calls_.findIndex((c) => c.id === live?.callId);
    if (index >= 0) {
      const done = this.makeCall(this.calls_[index].id, id, live!.startedAt, now, this.calls_[index].encrypted);
      this.calls_[index] = done;
      this.emit({ type: "call", call: done });
      this.addActivity(done.source_id, live!.startedAt, now);
    }
  }

  private addActivity(sourceId: string, start: number, end: number) {
    const a = this.activity_;
    const i = Math.floor((start - a.start) / a.bucket_s / 1000);
    const seconds = (end - start) / 1000;
    if (i < 0) return;
    while (a.block.length <= i) {
      a.block.push(0);
      a.baseline?.push(a.baseline[a.baseline.length - 288] ?? 30);
      Object.values(a.sources).forEach((arr) => arr.push(0));
    }
    a.block[i] += seconds;
    (a.sources[sourceId] ??= new Array(a.block.length).fill(0))[i] += seconds;
  }

  /** Advance the simulation by `dt` ms. */
  step(dt: number) {
    const now = this.now();
    for (const [id, live] of this.live) if (now >= live.talkingUntil) this.endCall(id, now);
    const hour = new Date(now).getHours();
    const daytime = hour >= 7 && hour < 20 ? 1 : 0.45;
    for (const d of MOCK_DEFS) {
      const id = d.entry.channel_ids[0];
      if (this.live.has(id)) continue;
      if (this.rand() < ((d.rate * daytime) / 3600) * (dt / 1000) * 3)
        this.startCall(id, now, d.length * (0.4 + this.rand() * 1.4), d.entry.encryption === "encrypted");
    }
    for (const u of UNLABELLED) {
      const id = `${u.frequency}-${u.protocol}-${u.slot}`;
      if (!this.live.has(id) && this.rand() < (u.rate / 3600) * (dt / 1000) * 3)
        this.startCall(id, now, u.length * (0.5 + this.rand()), false);
    }
  }

  private spectrumRow(time: number) {
    const bins = 1024;
    const low = -125;
    const high = -55;
    const row = new Uint8Array(bins);
    const start = BLOCK_CENTER - BLOCK_SPAN / 2;
    const binHz = BLOCK_SPAN / bins;
    const carriers = [...this.channels.values()]
      .filter((c) => c.rf_active && c.in_range)
      .map((c) => ({ bin: (c.frequency_hz - start) / binHz, level: (c.level_db ?? -80) + (this.rand() - 0.5) * 3 }));
    // A steady data carrier nobody has catalogued, and a weak birdie.
    // The Columbia Tower control channel transmits continuously.
    carriers.push({ bin: (454_217_000 - start) / binHz, level: -84 + (this.rand() - 0.5) * 2 });
    carriers.push({ bin: (454_260_700 - start) / binHz, level: -92 + (this.rand() - 0.5) * 2 });
        for (let b = 0; b < bins; b++) {
      const edge = Math.min(b, bins - b) < 40 ? (40 - Math.min(b, bins - b)) * 0.25 : 0;
      let db = -114 - edge + (this.rand() - 0.5) * 5 + Math.sin(b / 37) * 0.8;
      for (const c of carriers) {
        const d = (b - c.bin) / 1.3;
        if (Math.abs(d) < 6) db = Math.max(db, c.level - d * d * 3);
      }
      row[b] = scaleToByte(this.state === "receiving" && this.blockId === "downtown" ? db : -122, low, high);
    }
    this.spectrumSeq += 1;
    this.spectrumRows.push({ sequence: this.spectrumSeq, time: time / 1000, bins: row });
    if (this.spectrumRows.length > 600) this.spectrumRows.shift();
    this.emit({ type: "spectrum_available", generation: this.generation, sequence: this.spectrumSeq });
  }

  private pumpAudio(ms: number) {
    if (!this.selected || this.state !== "receiving") return;
    const channel = this.channels.get(this.selected);
    if (!channel?.speech_active || channel.protocol === "FM") return;
    const samples = Math.round((8000 * ms) / 1000);
    const pcm = synthVoice(channel.frequency_hz % 97, samples, 8000, this.audioOffset);
    this.audioOffset += samples;
    const descriptor: AudioDescriptor = {
      type: "audio",
      encoding: "s16le",
      channels: 1,
      rate_hz: 8000,
      samples,
      channel_id: this.selected,
      generation: this.decoderGeneration,
      source_generation: this.generation,
      sequence: this.audioOffset,
      ingress_age_ms: 40,
    };
    this.listenSubs.forEach((h) => h.onAudio(descriptor, pcm));
  }

  private heartbeat() {
    this.listenSubs.forEach((h) => h.onHeartbeat(this.selected));
    this.emit({ type: "heartbeat", generation: this.decoderGeneration, source_generation: this.generation });
  }

  private buildActivity(now: number): WireActivity {
    const buckets = 288;
    const start = Math.floor(now / BUCKET) * BUCKET - (buckets - 1) * BUCKET;
    const block: number[] = [];
    const baseline: number[] = [];
    const sources: Record<string, number[]> = {};
    const total = MOCK_DEFS.reduce((s, d) => s + d.weight, 0);
    for (let i = 0; i < buckets; i++) {
      const t = new Date(start + i * BUCKET);
      const h = t.getHours() + t.getMinutes() / 60;
      const typical = 12 + 70 * Math.max(0, Math.sin(((h - 6) / 15) * Math.PI)) ** 1.5;
      const recent = i >= buckets - 4 ? 1.9 : 1;
      const actual = Math.max(0, typical * recent * (0.6 + this.rand() * 0.8));
      baseline.push(typical);
      block.push(actual);
      for (const d of MOCK_DEFS) (sources[d.entry.id] ??= []).push((actual * d.weight) / total);
    }
    return { bucket_s: BUCKET / 1000, start: start / 1000, block, baseline, sources };
  }

  private backfillCalls(now: number) {
    const calls: WireCall[] = [];
    for (let t = now - 2 * 3600_000; t < now - 30_000; t += 20_000 + this.rand() * 90_000) {
      const d = MOCK_DEFS[Math.floor(this.rand() * MOCK_DEFS.length)];
      const length = d.length * (0.4 + this.rand() * 1.4) * 1000;
      const id = `call-${++this.callCounter}`;
      calls.unshift(this.makeCall(id, d.entry.channel_ids[0], t, t + length, d.entry.encryption === "encrypted"));
      const channel = this.channels.get(d.entry.channel_ids[0]);
      if (channel && d.entry.encryption !== "encrypted") channel.last_heard = (t + length) / 1000;
      if (channel) channel.last_rf = (t + length) / 1000;
    }
    this.calls_ = calls;
    const unknown = this.channels.get("452735400-DMR-1");
    if (unknown) unknown.last_heard = (now - 14 * 60_000) / 1000;
  }

  // Backend

  async status(): Promise<EngineStatus> {
    const block = MOCK_BLOCKS.blocks.find((b) => b.id === this.blockId)!;
    return {
      mode: this.state === "receiving" ? "live" : "disconnected",
      state: this.state,
      source_name: "Demo receiver",
      center_hz: block.center_hz,
      rate_hz: block.span_hz,
      bandwidth_hz: block.span_hz,
      generation: this.generation,
      channels: [...this.channels.values()].map((c) => ({ ...c })),
      received_seconds: null,
      source_position_s: null,
      decode_latency_s: null,
      dropped_batches: 0,
      processed_batches: this.spectrumSeq,
      queue_depth: 0,
      error: null,
      decoder_error: null,
      started_at: null,
      last_samples_at: this.now() / 1000,
      continuity: "demo",
      decoder_capacity: 8,
      source: { input_center_hz: block.center_hz, input_bandwidth_hz: block.span_hz, demo: true },
    };
  }

  events(onEvent: (event: EngineEvent) => void, onConnection: (open: boolean) => void) {
    this.eventSubs.add(onEvent);
    queueMicrotask(() => onConnection(true));
    return { close: () => void this.eventSubs.delete(onEvent) };
  }

  async spectrum(after: number): Promise<SpectrumBatch> {
    const rows = this.spectrumRows.filter((r) => r.sequence > after).slice(after === 0 ? -400 : -120);
    const block = MOCK_BLOCKS.blocks.find((b) => b.id === this.blockId)!;
    return {
      centerHz: block.center_hz,
      spanHz: block.span_hz,
      lowDb: -125,
      highDb: -55,
      unit: "dBFS (demo)",
      sequence: rows.at(-1)?.sequence ?? after,
      rows,
    };
  }

  async monitor(channelId: string | null) {
    if (channelId !== null) {
      const channel = this.channels.get(channelId);
      if (!channel) throw new BackendError("channel not in catalog", 404);
      if (channel.protocol === "Unknown") throw new BackendError("Channel protocol is not confirmed yet", 422);
    }
    this.selected = channelId;
    this.decoderGeneration += 1;
  }

  listen(handlers: ListenHandlers) {
    this.listenSubs.add(handlers);
    queueMicrotask(() => handlers.onHeartbeat(this.selected));
    return {
      close: () => {
        this.listenSubs.delete(handlers);
        handlers.onClose();
      },
    };
  }

  async stopReceiver() {
    this.state = "stopped";
    for (const id of [...this.live.keys()]) this.endCall(id, this.now());
    this.emit({ type: "source_state", state: "stopped", error: null, generation: this.generation });
    return "Demo receiver stopped.";
  }

  async configureSource(): Promise<string> {
    throw new BackendError(
      "Source producer does not advertise acknowledged controls; configure the producer and wait for actual CONFIG readback.",
      501,
    );
  }

  async blocks() {
    return { ...MOCK_BLOCKS, current_id: this.blockId };
  }

  async selectBlock(id: string) {
    if (!MOCK_BLOCKS.blocks.some((b) => b.id === id)) throw new BackendError("Unknown block", 404);
    this.blockId = id;
    this.generation += 1;
    for (const [channelId] of this.live) this.endCall(channelId, this.now());
    for (const channel of this.channels.values()) this.update(channel.id, { in_range: id === "downtown" });
    this.emit({ type: "source_config", generation: this.generation, source: {} });
  }

  async catalog() {
    return this.catalog_.map((e) => structuredClone(e));
  }

  async saveCatalogEntry(entry: WireCatalogEntry) {
    const index = this.catalog_.findIndex((e) => e.id === entry.id);
    if (index >= 0) this.catalog_[index] = structuredClone(entry);
    else this.catalog_.push(structuredClone(entry));
    return true;
  }

  async calls(query: CallQuery) {
    return this.calls_
      .filter((c) => !query.sourceId || c.source_id === query.sourceId)
      .filter((c) => !query.before || c.started_at * 1000 < query.before)
      .slice(0, query.limit ?? 100)
      .map((c) => ({ ...c }));
  }

  async activity() {
    return structuredClone(this.activity_);
  }

  /** Mock call audio is generated on demand rather than stored. */
  resolveAudio(url: string): string {
    if (!url.startsWith("mock:")) return url;
    const id = url.slice(5);
    const cached = this.audioUrls.get(id);
    if (cached) return cached;
    const call = this.calls_.find((c) => c.id === id);
    const seconds = call?.ended_at ? Math.min(20, call.ended_at - call.started_at) : 4;
    const seed = Number(id.replace(/\D/g, "")) || 1;
    const result = wavUrl(synthVoice(seed, Math.round(seconds * 8000)));
    this.audioUrls.set(id, result);
    return result;
  }

  async supports(): Promise<Record<Need, boolean>> {
    return { blocks: true, catalog: true, calls: true, activity: true, spectrumBinary: true };
  }
}
