import type { AudioDescriptor, EngineChannel, EngineEvent, EngineStatus } from "@/api/engine";
import type { WireActivity, WireBlocks, WireCall, WireCatalogEntry } from "@/api/needs";
import type { SpectrumBatch } from "@/domain/model";

export interface ListenHandlers {
  onAudio(descriptor: AudioDescriptor, pcm: Int16Array): void;
  /** The engine's current selection; it is shared by every connected browser. */
  onHeartbeat(channelId: string | null): void;
  onGap(): void;
  onClose(): void;
}

export interface Connection {
  close(): void;
}

export interface CallQuery {
  before?: number;
  limit?: number;
  sourceId?: string;
}

/** Feature endpoints from engine/README.md § "Frontend needs". */
export type Need = "blocks" | "catalog" | "calls" | "activity" | "spectrumBinary";

/**
 * Everything the UI asks of the radio. `HttpBackend` talks to the Rust engine;
 * `MockBackend` simulates it for demo mode and tests. Methods for endpoints the
 * engine doesn't serve yet resolve to `null` so callers can fall back.
 */
export interface Backend {
  readonly mode: "live" | "demo";
  status(): Promise<EngineStatus>;
  events(onEvent: (event: EngineEvent) => void, onConnection: (open: boolean) => void): Connection;
  spectrum(after: number): Promise<SpectrumBatch>;
  /** Select what the engine plays on /api/listen. null clears the selection. */
  monitor(channelId: string | null): Promise<void>;
  /** Operator: add a true-frequency target inside an advertised receiver slice. */
  tune?(frequencyHz: number): Promise<EngineChannel>;
  listen(handlers: ListenHandlers): Connection;
  /** Operator: stop the receiver (not just the listener). */
  stopReceiver(): Promise<string>;
  /** Operator: retune via POST /api/source. */
  configureSource(config: { centerHz: number; rateHz: number; bandwidthHz: number }): Promise<string>;
  blocks(): Promise<WireBlocks | null>;
  selectBlock(id: string): Promise<void>;
  catalog(): Promise<WireCatalogEntry[] | null>;
  /** false when the engine has no catalog store yet. */
  saveCatalogEntry(entry: WireCatalogEntry): Promise<boolean>;
  calls(query: CallQuery): Promise<WireCall[] | null>;
  activity(): Promise<WireActivity | null>;
  /** Turn a call's audio_url into something an <audio> element can play. */
  resolveAudio?(url: string): string;
  /** Which "frontend needs" the backend answers itself. */
  supports(): Promise<Record<Need, boolean>>;
}

export class BackendError extends Error {
  readonly status: number;

  constructor(message: string, status: number) {
    super(message);
    this.status = status;
  }
}
