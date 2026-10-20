import type { EngineChannel, EngineStatus } from "@/api/engine";
import type { WireCatalogEntry } from "@/api/needs";
import type { Encryption, Health, Mode, Source } from "./model";

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

function asMode(value: string): Mode {
  return (["fm", "dmr", "nxdn", "p25"] as const).includes(value as never)
    ? (value as Mode)
    : "unknown";
}

function asEncryption(value: string): Encryption {
  return (["clear", "encrypted", "mixed"] as const).includes(value as never)
    ? (value as Encryption)
    : "unknown";
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

function listenableFor(mode: Mode, encryption: Encryption, channel: EngineChannel | undefined) {
  if (encryption === "encrypted")
    return { ok: false as const, reason: "Private: these calls are encrypted, so nobody outside the group can hear them." };
  if (!channel) return { ok: false as const, reason: "The radio isn't picking this up right now." };
  if (channel.in_range === false)
    return { ok: false as const, reason: "Outside the part of the airwaves the radio is listening to." };
  if (mode === "unknown")
    return { ok: false as const, reason: "The radio is still working out what kind of signal this is." };
  if (mode === "fm")
    return { ok: false as const, reason: "Plain voice channels can't be played yet; the radio only plays digital voice so far." };
  return { ok: true as const };
}

function keyHz(hz: number) {
  return Math.round(hz);
}

/**
 * Engine channels plus optional catalog labels → sources.
 *
 * The engine keeps a `…-Unknown-1` entry per frequency until it identifies the
 * signal, then adds `…-DMR-1/2` entries beside it. Once a frequency has an
 * identified entry, its Unknown placeholder is hidden.
 */
export function toSources(
  channels: EngineChannel[],
  catalog: WireCatalogEntry[] | null,
): Source[] {
  const byId = new Map(channels.map((c) => [c.id, c]));
  const identified = new Set(
    channels.filter((c) => modeOf(c.protocol) !== "unknown").map((c) => keyHz(c.frequency_hz)),
  );
  const claimed = new Set<string>();
  const sources: Source[] = [];

  for (const entry of catalog ?? []) {
    const covered = entry.channel_ids.map((id) => byId.get(id)).filter((c): c is EngineChannel => !!c);
    entry.channel_ids.forEach((id) => claimed.add(id));
    const primary =
      covered.find((c) => c.speech_active) ??
      covered.find((c) => modeOf(c.protocol) !== "unknown") ??
      covered[0];
    const mode = entry.mode !== "unknown" ? asMode(entry.mode) : modeOf(primary?.protocol);
    const encryption = asEncryption(entry.encryption);
    sources.push({
      id: entry.id,
      kind: entry.kind,
      name: entry.name,
      named: true,
      summary: encryption === "encrypted" ? PRIVATE_SUMMARY : entry.description || modeWords[mode],
      description: entry.description,
      probableUser: entry.probable_user ?? null,
      trafficKind: entry.traffic_kind ?? null,
      mode,
      encryption,
      frequencyHz: entry.frequency_hz ?? primary?.frequency_hz ?? null,
      slot: entry.slot ?? primary?.slot ?? null,
      system: entry.system ?? null,
      talkgroupId: entry.talkgroup_id ?? null,
      channelIds: primary ? [primary.id, ...entry.channel_ids.filter((id) => id !== primary.id)] : entry.channel_ids,
      // Encrypted calls never decode to speech; a carrier is the only sign of a call.
      talking: covered.some((c) => c.speech_active === true || (encryption === "encrypted" && c.rf_active === true)),
      signal: covered.some((c) => c.rf_active === true),
      levelDb: primary?.level_db ?? null,
      lastHeardAt: maxOf(covered.map((c) => seconds(c.last_heard) ?? seconds(c.last_rf))),
      inRange: covered.some((c) => c.in_range !== false),
      listenable: listenableFor(mode, encryption, primary),
    });
  }

  for (const channel of channels) {
    if (claimed.has(channel.id)) continue;
    const mode = modeOf(channel.protocol);
    if (mode === "unknown" && identified.has(keyHz(channel.frequency_hz))) continue;
    const slot = channel.slot ?? null;
    sources.push({
      id: channel.id,
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
      listenable: listenableFor(mode, "unknown", channel),
    });
  }
  return sources;
}

function maxOf(values: (number | null)[]): number | null {
  const real = values.filter((v): v is number => v !== null);
  return real.length ? Math.max(...real) : null;
}

/** Talking first, then followed, then listenable, then most recently heard; out of range last. */
export function rankSources(sources: Source[], followed: ReadonlySet<string>): Source[] {
  return [...sources].sort((a, b) => {
    const score = (s: Source) =>
      (s.talking ? 8 : 0) + (followed.has(s.id) ? 4 : 0) + (s.listenable.ok ? 2 : 0) + (s.inRange ? 1 : 0);
    return (
      score(b) - score(a) ||
      (b.lastHeardAt ?? 0) - (a.lastHeardAt ?? 0) ||
      (a.frequencyHz ?? 0) - (b.frequencyHz ?? 0)
    );
  });
}

export function healthOf(status: EngineStatus | undefined, connected: boolean): Health {
  if (!status)
    return { state: "offline", detail: "Can't reach the radio. Check that the receiver box is on.", raw: null };
  if (status.state === "receiving" && status.mode === "live")
    return connected
      ? { state: "listening", detail: "The radio is on and listening." }
      : { state: "offline", detail: "Lost touch with the radio. Reconnecting.", raw: null };
  if (status.state === "error")
    return {
      state: "offline",
      detail: "The radio stopped receiving. Cory may need to restart it.",
      raw: status.error ?? null,
    };
  return { state: "paused", detail: "The radio is paused. Nothing new will come in until it's started again." };
}
