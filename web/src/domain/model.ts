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
  /**
   * Raw evidence of a real transmission right now: the engine decoded voice
   * (DMR/NXDN voice frames with sync, or FM held above the squelch). Never RF
   * energy alone. The home screen smooths this with hysteresis (domain/calm.ts).
   */
  talking: boolean;
  /** RF present (uncalibrated on the ARM path: often noise). Never shown as talking by itself. */
  signal: boolean;
  /** A trunking control channel: data that organizes a radio system, never voice. */
  control: boolean;
  /** The engine has decoded voice here at some point. Sources without it are "Other activity". */
  heardVoice: boolean;
  levelDb: number | null;
  /** Epoch milliseconds of the last decoded voice (not RF). */
  lastHeardAt: number | null;
  /** Epoch milliseconds of the last RF activity of any kind. */
  lastSignalAt: number | null;
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
  | {
      state: "offline";
      detail: string;
      raw: string | null;
      /** The receiver is expected back without anyone doing anything. */
      reconnecting?: boolean;
    };

export interface SpectrumRow {
  sequence: number;
  time: number;
  /** Levels scaled 0..255 between lowDb and highDb. */
  bins: Uint8Array;
}

export interface SpectrumBatch {
  /** Engine source generation; history from another generation is a different band. */
  generation: number;
  centerHz: number | null;
  spanHz: number | null;
  lowDb: number;
  highDb: number;
  unit: string;
  sequence: number;
  rows: SpectrumRow[];
}
