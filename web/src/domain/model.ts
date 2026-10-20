/** The UI's domain model. Screens use these, never engine JSON. */

export type SourceKind = "channel" | "talkgroup";
export type Mode = "fm" | "dmr" | "nxdn" | "p25" | "unknown";
export type Encryption = "clear" | "encrypted" | "mixed" | "unknown";

export type Listenable = { ok: true } | { ok: false; reason: string };

export interface Source {
  /** Catalog id when labelled, otherwise the engine channel id. */
  id: string;
  kind: SourceKind;
  /** Plain name: the label, or the number when there's no label yet. */
  name: string;
  named: boolean;
  /** One line: what this is, in plain words. */
  summary: string;
  description: string;
  probableUser: string | null;
  trafficKind: string | null;
  mode: Mode;
  encryption: Encryption;
  /** Conventional channels. */
  frequencyHz: number | null;
  slot: number | null;
  /** Trunking (next): system and talkgroup. */
  system: { id: string; name: string } | null;
  talkgroupId: number | null;
  /** Engine channel ids this source covers; the first is the one to monitor. */
  channelIds: string[];
  talking: boolean;
  /** RF present without decoded speech (data, carrier, or not identified). */
  signal: boolean;
  levelDb: number | null;
  /** Epoch milliseconds. */
  lastHeardAt: number | null;
  inRange: boolean;
  listenable: Listenable;
}

export interface Call {
  id: string;
  sourceId: string;
  channelId: string | null;
  /** Epoch milliseconds. */
  startedAt: number;
  endedAt: number | null;
  encrypted: boolean;
  audioUrl: string | null;
  talker: string | null;
  transcript: string | null;
}

export interface Activity {
  bucketMs: number;
  /** Epoch ms of the first bucket. */
  start: number;
  /** Talk-seconds per bucket for the whole block. */
  block: number[];
  sources: Record<string, number[]>;
  /** Typical talk-seconds per bucket; null until there's enough history. */
  baseline: number[] | null;
}

export interface Block {
  id: string;
  name: string;
  description: string;
  centerHz: number;
  spanHz: number;
}

export type Health =
  | { state: "listening"; detail: string }
  | { state: "paused"; detail: string }
  | { state: "offline"; detail: string; raw: string | null };

export interface SpectrumRow {
  sequence: number;
  time: number;
  /** Levels scaled 0..255 between lowDb and highDb. */
  bins: Uint8Array;
}

export interface SpectrumBatch {
  centerHz: number | null;
  spanHz: number | null;
  lowDb: number;
  highDb: number;
  unit: string;
  sequence: number;
  rows: SpectrumRow[];
}
