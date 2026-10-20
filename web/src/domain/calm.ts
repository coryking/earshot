/**
 * What keeps the home screen calm: a strict, smoothed "talking" signal, a list
 * order that doesn't move while someone is looking at it, and grouping that
 * keeps noise out of the way. Pure classes and functions; screens/Now.tsx
 * drives them at a limited rate.
 */
import { formatMHz } from "./adapt";
import type { Source } from "./model";

/** A source stays "talking" this long after its last evidence, so back-and-forth replies don't flicker. */
export const HOLD_MS = 5000;
/** An encrypted source's carrier must hold this long (and the detector be calibrated) to count. */
export const CARRIER_MIN_MS = 2000;
/** "Talking now" shows at most this many sources. */
export const TALKING_SHOWN = 3;

interface Track {
  since: number;
  lastEvidence: number;
}

export interface TalkOptions {
  /** The engine's RF detector is calibrated (source.activity_calibrated). On the ARM path it isn't. */
  carrierCalibrated: boolean;
}

/**
 * Talking with hysteresis. Evidence is decoded voice (Source.talking, which the
 * adapter already limits to decoded speech off control channels), or, for a
 * source known to be encrypted, a carrier held for CARRIER_MIN_MS by a
 * calibrated detector. Control channels and data never count.
 */
export class TalkTracker {
  private tracks = new Map<string, Track>();
  private carrierSince = new Map<string, number>();

  /** Feed the latest sources; returns talking source ids with when each started. */
  observe(sources: Source[], now: number, options: TalkOptions): Map<string, number> {
    const seen = new Set<string>();
    for (const s of sources) {
      seen.add(s.id);
      let evidence = s.talking && !s.control;
      if (s.encryption === "encrypted" && !s.control && options.carrierCalibrated && s.signal) {
        const since = this.carrierSince.get(s.id) ?? now;
        this.carrierSince.set(s.id, since);
        if (now - since >= CARRIER_MIN_MS) evidence = true;
      } else this.carrierSince.delete(s.id);
      const track = this.tracks.get(s.id);
      if (evidence) this.tracks.set(s.id, { since: track?.since ?? now, lastEvidence: now });
    }
    const talking = new Map<string, number>();
    for (const [id, track] of this.tracks) {
      if (!seen.has(id) || now - track.lastEvidence > HOLD_MS) this.tracks.delete(id);
      else talking.set(id, track.since);
    }
    for (const id of this.carrierSince.keys()) if (!seen.has(id)) this.carrierSince.delete(id);
    return talking;
  }
}

/**
 * A list order that only changes when asked. Known ids keep their relative
 * positions; new ids join the end; ids that disappear drop out. `resort`
 * replaces the order with a fresh ranking (on open, on follow/mute, and when
 * the page comes back after a while).
 */
export class StableOrder {
  private order: string[] = [];

  arrange(ranked: string[], resort: boolean): string[] {
    if (resort || !this.order.length) {
      this.order = [...ranked];
      return this.order;
    }
    const present = new Set(ranked);
    const kept = this.order.filter((id) => present.has(id));
    const known = new Set(kept);
    this.order = [...kept, ...ranked.filter((id) => !known.has(id))];
    return this.order;
  }
}

/**
 * A short differentiator for sources that share a name ("Line 1 · 454.106 MHz"),
 * so two rows never read the same. Unique names get none.
 */
export function differentiators(sources: Source[]): Map<string, string> {
  const byName = new Map<string, Source[]>();
  for (const s of sources) byName.set(s.name, [...(byName.get(s.name) ?? []), s]);
  const out = new Map<string, string>();
  for (const group of byName.values()) {
    if (group.length < 2) continue;
    const frequencies = new Set(group.map((s) => (s.frequencyHz === null ? null : Math.round(s.frequencyHz / 1000))));
    for (const s of group) {
      const parts: string[] = [];
      if (s.slot !== null && s.mode === "dmr" && !/, line \d$/.test(s.name)) parts.push(`Line ${s.slot}`);
      if (s.frequencyHz !== null && (frequencies.size > 1 || !parts.length)) parts.push(`${formatMHz(s.frequencyHz)} MHz`);
      if (parts.length) out.set(s.id, parts.join(" · "));
    }
  }
  return out;
}

export interface CalmGroups {
  /** Talking now, in the order they started (stable), at most TALKING_SHOWN. */
  talking: Source[];
  /** Talking but beyond TALKING_SHOWN. */
  moreTalking: number;
  /** Followed, or heard with decoded voice: the quiet main list, in stable order. */
  channels: Source[];
  /** Activity without decoded content (noise, data, control, unidentified), by frequency. */
  other: Source[];
  muted: Source[];
}

/** Main list membership: the listener follows it, or a real voice has been heard on it. */
export function isListed(s: Source, followed: ReadonlySet<string>, talking: ReadonlyMap<string, number>) {
  return followed.has(s.id) || (!s.control && (s.heardVoice || talking.has(s.id)));
}

export function groupSources(
  sources: Source[],
  talking: ReadonlyMap<string, number>,
  order: string[],
  followed: ReadonlySet<string>,
  muted: ReadonlySet<string>,
): CalmGroups {
  const byId = new Map(sources.map((s) => [s.id, s]));
  const audible = sources.filter((s) => !muted.has(s.id));
  const talkingNow = audible
    .filter((s) => talking.has(s.id))
    .sort((a, b) => talking.get(a.id)! - talking.get(b.id)! || a.id.localeCompare(b.id));
  const listed = new Set(audible.filter((s) => isListed(s, followed, talking)).map((s) => s.id));
  return {
    talking: talkingNow.slice(0, TALKING_SHOWN),
    moreTalking: Math.max(0, talkingNow.length - TALKING_SHOWN),
    channels: order.filter((id) => listed.has(id)).map((id) => byId.get(id)!),
    other: audible
      .filter((s) => !listed.has(s.id))
      .sort((a, b) => (a.frequencyHz ?? 0) - (b.frequencyHz ?? 0) || a.id.localeCompare(b.id)),
    muted: sources.filter((s) => muted.has(s.id)).sort((a, b) => a.name.localeCompare(b.name)),
  };
}

/** Coarse "last heard" for the quiet list: minutes, not seconds, so it doesn't tick. */
export function heardAgo(then: number | null, now: number): string | null {
  if (then === null) return null;
  const m = Math.floor(Math.max(0, now - then) / 60_000);
  if (m < 1) return "Just now";
  if (m < 60) return `${m} min ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h} hr ago`;
  return `${Math.floor(h / 24)} d ago`;
}
