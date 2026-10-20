import type { EngineChannel, EngineStatus } from "@/api/engine";
import type { WireCatalogEntry } from "@/api/needs";
import type { Encryption, Health, Listenable, Mode, Source } from "./model";

export function formatMHz(hz: number, digits = 3): string {
  return (hz / 1e6).toFixed(digits);
}

export function modeOf(protocol: string | null | undefined): Mode {
  switch ((protocol ?? "").toUpperCase()) {
    case "DMR":
      return "dmr";
    case "NXDN48":
    case "NXDN96":
    case "NXDN":
      return "nxdn";
    case "P25":
      return "p25";
    case "FM":
    case "NFM":
      return "fm";
    default:
      return "unknown";
  }
}

const modeWords: Record<Mode, string> = {
  fm: "Voice",
  dmr: "Digital voice",
  nxdn: "Digital voice",
  p25: "Digital voice",
  unknown: "Unidentified",
};

/** What an unlabelled source is, as far as anyone can tell. */
const unlabelledWords: Record<Mode, string> = {
  fm: "Unknown",
  dmr: "Unknown",
  nxdn: "Unknown",
  p25: "Unknown",
  unknown: "Unidentified",
};

export const PRIVATE_SUMMARY = "Private (encrypted)";
export const CONTROL_SUMMARY = "Data, no voice";

/** A name for something nobody has labelled: its number, like an unknown caller. */
export function numberName(frequencyHz: number, slot: number | null, mode: Mode) {
  const base = `${formatMHz(frequencyHz)} MHz`;
  return mode === "dmr" && slot ? `${base}, line ${slot}` : base;
}

function seconds(value: number | null | undefined): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value * 1000 : null;
}

export interface SourceOptions {
  /** The producer advertises analog FM audio (CONFIG capabilities.fm_audio). */
  fmAudio?: boolean;
}

/** What the engine can play right now, judged by the channel's own protocol. */
function listenableFor(
  channel: EngineChannel | undefined,
  encryption: Encryption,
  control: boolean,
  options: SourceOptions,
): Listenable {
  if (encryption === "encrypted")
    return { ok: false, reason: "Encrypted" };
  if (control)
    return { ok: false, reason: "Data only" };
  if (!channel) return { ok: false, reason: "Off the air" };
  if (channel.in_range === false)
    return { ok: false, reason: "Out of range" };
  const mode = modeOf(channel.protocol);
  // An unidentified channel can still be listened to: the engine keeps it pinned and plays
  // whatever voice it identifies there.
  if (mode === "fm" && !options.fmAudio)
    return { ok: false, reason: "Analog not supported yet" };
  return { ok: true };
}

/** The engine's own label kind says a channel is a trunking control channel; free text never does. */
function isControl(channel: EngineChannel): boolean {
  return channel.label_kind === "control";
}

/** Strong evidence of a real transmission: decoded speech, never RF energy alone. */
function speaking(channel: EngineChannel, control: boolean): boolean {
  return channel.speech_active === true && !control;
}

function heard(channel: EngineChannel): boolean {
  return channel.decoded === true || channel.last_heard != null;
}

/** Catalog entries match channels this close (kHz raster plus receiver LO error). */
const MATCH_HZ = 3000;

/**
 * Does a catalog entry cover this channel? By id first; otherwise by frequency
 * and slot, because a channel's id changes when the engine identifies its
 * protocol (`…-Unknown-1` becomes `…-DMR-2`). An entry without a slot covers
 * every slot on its frequency.
 */
function covers(entry: WireCatalogEntry, channel: EngineChannel): boolean {
  if (entry.channel_ids.includes(channel.id)) return true;
  if (entry.frequency_hz == null || Math.abs(entry.frequency_hz - channel.frequency_hz) > MATCH_HZ) return false;
  return entry.slot == null || channel.slot == null || channel.slot === entry.slot || modeOf(channel.protocol) === "unknown";
}

/** Stable id for an unlabelled channel; survives the engine identifying the protocol. */
export function channelKey(frequencyHz: number, slot: number | null, mode: Mode) {
  return mode === "dmr" && slot ? `ch-${Math.round(frequencyHz)}-${slot}` : `ch-${Math.round(frequencyHz)}`;
}

function named(
  entry: WireCatalogEntry,
  covered: EngineChannel[],
  options: SourceOptions,
  id: string,
  suffix: string,
): Source {
  const primary =
    covered.find((c) => c.speech_active) ?? covered.find((c) => modeOf(c.protocol) !== "unknown") ?? covered[0];
  const mode = modeOf(primary.protocol) !== "unknown" ? modeOf(primary.protocol) : entry.mode;
  const encryption = entry.encryption;
  const control = covered.some(isControl);
  return {
    id,
    kind: entry.kind,
    name: entry.name + suffix,
    named: true,
    summary: encryption === "encrypted" ? PRIVATE_SUMMARY : control ? CONTROL_SUMMARY : entry.description || modeWords[mode],
    description: entry.description,
    probableUser: entry.probable_user ?? null,
    trafficKind: entry.traffic_kind ?? null,
    mode,
    encryption,
    frequencyHz: primary.frequency_hz,
    slot: primary.slot ?? null,
    system: entry.system ?? null,
    talkgroupId: entry.talkgroup_id ?? null,
    channelIds: [primary.id, ...covered.filter((c) => c !== primary).map((c) => c.id)],
    // Encrypted calls never decode to speech; their carrier is judged with hysteresis in domain/calm.ts.
    talking: covered.some((c) => speaking(c, control)),
    signal: covered.some((c) => c.rf_active === true),
    control,
    heardVoice: !control && covered.some(heard),
    levelDb: primary.level_db ?? null,
    lastHeardAt: maxOf(covered.map((c) => seconds(c.last_heard))),
    lastSignalAt: maxOf(covered.map((c) => seconds(c.last_rf))),
    inRange: covered.some((c) => c.in_range !== false),
    listenable: listenableFor(primary, encryption, control, options),
  };
}

/**
 * Engine channels plus catalog labels → sources.
 *
 * Catalogs are applied in priority order (the engine's, then labels saved on
 * this device, then the starter names); each channel belongs to the first entry
 * that covers it, and entries that cover nothing currently heard are left out.
 * The engine keeps a `…-Unknown-1` placeholder per frequency until it
 * identifies the signal; once a frequency has an identified channel, the
 * placeholder is hidden.
 */
export function toSources(
  channels: EngineChannel[],
  catalog: WireCatalogEntry[] | null,
  options: SourceOptions = {},
): Source[] {
  const identified = new Set(
    channels.filter((c) => modeOf(c.protocol) !== "unknown").map((c) => Math.round(c.frequency_hz)),
  );
  const visible = channels.filter(
    (c) => modeOf(c.protocol) !== "unknown" || !identified.has(Math.round(c.frequency_hz)),
  );
  const claimed = new Set<string>();
  const sources: Source[] = [];

  for (const entry of catalog ?? []) {
    const covered = visible.filter((c) => !claimed.has(c.id) && covers(entry, c));
    if (!covered.length) continue;
    covered.forEach((c) => claimed.add(c.id));
    // The player follows one engine channel, and DMR's two timeslots are separate
    // conversations: when a frequency-only entry covers both, give each its own row.
    const slots = [...new Set(covered.filter((c) => modeOf(c.protocol) === "dmr").map((c) => c.slot ?? 1))].sort();
    if (entry.slot == null && slots.length > 1) {
      for (const slot of slots)
        sources.push(
          named(entry, covered.filter((c) => (c.slot ?? 1) === slot), options, `${entry.id}-line${slot}`, `, line ${slot}`),
        );
    } else sources.push(named(entry, covered, options, entry.id, ""));
  }

  for (const channel of visible) {
    if (claimed.has(channel.id)) continue;
    const mode = modeOf(channel.protocol);
    const slot = channel.slot ?? null;
    const control = isControl(channel);
    const label = channel.display_name?.trim() || null;
    sources.push({
      id: channelKey(channel.frequency_hz, slot, mode),
      kind: "channel",
      name: label ?? numberName(channel.frequency_hz, slot, mode),
      named: label !== null,
      summary: control ? CONTROL_SUMMARY : unlabelledWords[mode],
      description: "",
      probableUser: null,
      trafficKind: null,
      mode,
      encryption: "unknown",
      frequencyHz: channel.frequency_hz,
      slot,
      system: null,
      talkgroupId: null,
      channelIds: [channel.id],
      talking: speaking(channel, control),
      signal: channel.rf_active === true,
      control,
      heardVoice: !control && heard(channel),
      levelDb: channel.level_db ?? null,
      lastHeardAt: seconds(channel.last_heard),
      lastSignalAt: seconds(channel.last_rf),
      inRange: channel.in_range !== false,
      listenable: listenableFor(channel, "unknown", control, options),
    });
  }
  return sources;
}

function maxOf(values: (number | null)[]): number | null {
  const real = values.filter((v): v is number => v !== null);
  return real.length ? Math.max(...real) : null;
}

/**
 * The meaningful order for the home list: followed first (like pinned chats),
 * then most recently heard voice, then frequency. Deliberately ignores live RF
 * and talking state, which change by the second; the home screen also freezes
 * this order while it's on screen (domain/calm.ts).
 */
export function rankSources(sources: Source[], followed: ReadonlySet<string>): Source[] {
  return [...sources].sort(
    (a, b) =>
      Number(followed.has(b.id)) - Number(followed.has(a.id)) ||
      Number(b.inRange) - Number(a.inRange) ||
      (b.lastHeardAt ?? 0) - (a.lastHeardAt ?? 0) ||
      (a.frequencyHz ?? 0) - (b.frequencyHz ?? 0) ||
      a.id.localeCompare(b.id),
  );
}

/** Why the receiver is away, in words for the household, from what the supervisor reported. */
function receiverReason(receiver: EngineStatus["receiver"]): string | null {
  if (!receiver) return null;
  // A borrowed or cleanly stopped supervisor writes its status once and goes quiet: the age of
  // that file says nothing about whether it is alive.
  if (receiver.state === "borrowed") return "In use for a test.";
  if (receiver.state === "stopped") return "Switched off.";
  if (receiver.fresh === false)
    return "Receiver supervisor isn't running.";
  switch (receiver.state) {
    case "starting":
    case "running":
      return "Starting up.";
    case "waiting":
      switch (receiver.reason) {
        case "lock-held":
          return "In use for a test.";
        case "wrong-image":
          return "Running test software.";
        case "unreachable":
          return "Receiver box isn't answering.";
        case "producer-failed":
          return "Restarting.";
        case "radio-state":
          return "Waiting on the receiver.";
      }
  }
  return null;
}

function duration(seconds: number): string {
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  return minutes % 60 === 0 ? `${hours} h` : `${hours} h ${minutes % 60} min`;
}

/** After this long without the receiver the page stops calling it a blip. */
const LONG_OUTAGE_S = 600;

export function healthOf(status: EngineStatus | undefined, connected: boolean): Health {
  if (!status)
    return { state: "offline", detail: "Can't reach the radio.", raw: null };
  if (status.receiver?.fresh === true && status.receiver.state === "stopped" && status.receiver.reason === "operator")
    return { state: "paused", detail: "Receiving stopped. Start receiving to resume." };
  const link = status.link;
  // "connected" is only the TCP link: until the first CONFIG arrives the engine is still not receiving.
  if (link?.mode === "tcp" && (link.state !== "connected" || status.state !== "receiving")) {
    const reason = receiverReason(status.receiver);
    const away = link.offline_s ?? 0;
    const detail =
      away >= LONG_OUTAGE_S
        ? `Offline for ${duration(away)}.${reason ? ` ${reason}` : ""}`
        : (reason ?? "Reconnecting.");
    return {
      state: "offline",
      reconnecting: true,
      detail,
      raw: link.last_error ?? status.error ?? null,
    };
  }
  if (status.state === "receiving" && status.mode === "live")
    return connected
      ? { state: "listening", detail: "Receiving." }
      : { state: "offline", detail: "Reconnecting.", raw: null };
  if (status.state === "error")
    return {
      state: "offline",
      detail: "Receiver feed lost.",
      raw: status.error ?? null,
    };
  return { state: "paused", detail: "Paused." };
}
