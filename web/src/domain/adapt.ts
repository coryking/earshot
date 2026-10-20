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
  unknown: "Not identified yet",
};

/** What an unlabelled source is, as far as anyone can tell. */
const unlabelledWords: Record<Mode, string> = {
  fm: "Unknown business radio",
  dmr: "Unknown business radio",
  nxdn: "Unknown business radio",
  p25: "Unknown radio system",
  unknown: "Unidentified signal",
};

export const PRIVATE_SUMMARY = "Private (encrypted)";

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
function listenableFor(channel: EngineChannel | undefined, encryption: Encryption, options: SourceOptions): Listenable {
  if (encryption === "encrypted")
    return { ok: false, reason: "Private: these calls are encrypted, so nobody outside the group can hear them." };
  if (!channel) return { ok: false, reason: "The radio isn't picking this up right now." };
  if (channel.in_range === false)
    return { ok: false, reason: "Outside the part of the airwaves the radio is listening to." };
  const mode = modeOf(channel.protocol);
  if (mode === "unknown") return { ok: false, reason: "The radio is still working out what kind of signal this is." };
  if (mode === "fm" && !options.fmAudio)
    return { ok: false, reason: "Plain voice channels can't be played yet; the radio only plays digital voice so far." };
  return { ok: true };
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
  return {
    id,
    kind: entry.kind,
    name: entry.name + suffix,
    named: true,
    summary: encryption === "encrypted" ? PRIVATE_SUMMARY : entry.description || modeWords[mode],
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
    // Encrypted calls never decode to speech; a carrier is the only sign of a call.
    talking: covered.some((c) => c.speech_active === true || (encryption === "encrypted" && c.rf_active === true)),
    signal: covered.some((c) => c.rf_active === true),
    levelDb: primary.level_db ?? null,
    lastHeardAt: maxOf(covered.map((c) => seconds(c.last_heard) ?? seconds(c.last_rf))),
    inRange: covered.some((c) => c.in_range !== false),
    listenable: listenableFor(primary, encryption, options),
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
    sources.push({
      id: channelKey(channel.frequency_hz, slot, mode),
      kind: "channel",
      name: numberName(channel.frequency_hz, slot, mode),
      named: false,
      summary: unlabelledWords[mode],
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
      talking: channel.speech_active === true,
      signal: channel.rf_active === true,
      levelDb: channel.level_db ?? null,
      lastHeardAt: seconds(channel.last_heard) ?? seconds(channel.last_rf),
      inRange: channel.in_range !== false,
      listenable: listenableFor(channel, "unknown", options),
    });
  }
  return sources;
}

function maxOf(values: (number | null)[]): number | null {
  const real = values.filter((v): v is number => v !== null);
  return real.length ? Math.max(...real) : null;
}

/** Followed sources pinned first (like pinned chats), then talking, listenable, recent; out of range last. */
export function rankSources(sources: Source[], followed: ReadonlySet<string>): Source[] {
  return [...sources].sort((a, b) => {
    const score = (s: Source) =>
      (followed.has(s.id) ? 8 : 0) + (s.talking ? 4 : 0) + (s.listenable.ok ? 2 : 0) + (s.inRange ? 1 : 0);
    return (
      score(b) - score(a) ||
      (b.lastHeardAt ?? 0) - (a.lastHeardAt ?? 0) ||
      (a.frequencyHz ?? 0) - (b.frequencyHz ?? 0) ||
      a.id.localeCompare(b.id)
    );
  });
}

/** Why the receiver is away, in words for the household, from what the supervisor reported. */
function receiverReason(receiver: EngineStatus["receiver"]): string | null {
  if (!receiver) return null;
  // A borrowed or cleanly stopped supervisor writes its status once and goes quiet: the age of
  // that file says nothing about whether it is alive.
  if (receiver.state === "borrowed") return "The receiver is being used for a test and comes back when that's done.";
  if (receiver.state === "stopped") return "The receiver has been switched off for maintenance.";
  if (receiver.fresh === false)
    return "The program that keeps the receiver running isn't running, so nothing will restart it until it is.";
  switch (receiver.state) {
    case "starting":
    case "running":
      return "The receiver is starting up.";
    case "waiting":
      switch (receiver.reason) {
        case "lock-held":
          return "The receiver is being used for a test and comes back when that's done.";
        case "wrong-image":
          return "The receiver is running different software for a test. It comes back when that's finished.";
        case "unreachable":
          return "The receiver box isn't answering. Check that it's powered and plugged in.";
        case "producer-failed":
          return "The receiver's software stopped and is restarting.";
        case "radio-state":
          return "The receiver is in an unexpected state, so it's waiting.";
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
    return { state: "offline", detail: "Can't reach the radio. Check that the receiver box is on.", raw: null };
  const link = status.link;
  // "connected" is only the TCP link: until the first CONFIG arrives the engine is still not receiving.
  if (link?.mode === "tcp" && (link.state !== "connected" || status.state !== "receiving")) {
    const reason = receiverReason(status.receiver);
    const away = link.offline_s ?? 0;
    const detail =
      away >= LONG_OUTAGE_S
        ? `The receiver has been offline for ${duration(away)}. Nothing new is coming in; you're seeing what it heard earlier.${reason ? ` ${reason}` : ""}`
        : (reason ?? "It usually comes back by itself within a minute.");
    return {
      state: "offline",
      lead: "Receiver offline, reconnecting.",
      reconnecting: true,
      detail,
      raw: link.last_error ?? status.error ?? null,
    };
  }
  if (status.state === "receiving" && status.mode === "live")
    return connected
      ? { state: "listening", detail: "The radio is on and listening." }
      : { state: "offline", detail: "Lost touch with the radio. Reconnecting.", raw: null };
  if (status.state === "error")
    return {
      state: "offline",
      detail: "The radio lost its feed. It usually reconnects by itself within a minute; if it doesn't, Cory may need to restart it.",
      raw: status.error ?? null,
    };
  return { state: "paused", detail: "The radio is paused. Nothing new will come in until it's started again." };
}
