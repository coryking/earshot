import type { Source } from "@/domain/model";

export interface WaterfallPick { frequencyHz: number; source?: Source; snapped: boolean }

/** Nominal 6.25 kHz raster for this monitor's UHF land-mobile band.
 * This is a tuning aid, not evidence of a licensed assignment. Exact tuning
 * remains available for off-raster carriers and other bands. */
export function roundChannel(hz: number): number {
  return hz >= 450e6 && hz <= 470e6 ? Math.round(hz / 6250) * 6250 : Math.round(hz);
}

/** Pick from the row under the pointer, so a past transmission remains selectable. */
export function pickSignal(frequencyHz: number, radiusHz: number, lowHz: number, spanHz: number,
  bins: Uint8Array | undefined, sources: Source[], exact = false): WaterfallPick {
  if (exact) return { frequencyHz, snapped: false };
  const radius = Math.min(radiusHz, 100_000);
  const known = sources.filter((s) => s.frequencyHz !== null && (s.heardVoice || s.named) &&
    Math.abs(s.frequencyHz - frequencyHz) <= radius);
  const nearest = (target: number, tolerance: number) => known
    .filter((s) => Math.abs(s.frequencyHz! - target) <= tolerance)
    .sort((a, b) => Math.abs(a.frequencyHz! - target) - Math.abs(b.frequencyHz! - target) ||
      Number(b.talking) - Number(a.talking) || (a.slot ?? 0) - (b.slot ?? 0))[0];
  if (bins?.length) {
    const step = spanHz / bins.length;
    const noise = [...bins].sort((a, b) => a - b)[Math.floor(bins.length / 2)];
    const peaks = Array.from(bins, (level, i) => ({ level, i, hz: lowHz + (i + 0.5) * step }))
      .filter((p) => p.level >= noise + 12 && p.level > (bins[p.i - 1] ?? -1) &&
        p.level >= (bins[p.i + 1] ?? -1) && Math.abs(p.hz - frequencyHz) <= radius)
      .sort((a, b) => Math.abs(a.hz - frequencyHz) - Math.abs(b.hz - frequencyHz) || b.level - a.level);
    if (peaks.length) {
      const peak = peaks[0];
      const source = nearest(peak.hz, Math.max(step / 2, 6250));
      return { frequencyHz: source?.frequencyHz ?? roundChannel(peak.hz), source, snapped: true };
    }
  }
  const source = nearest(frequencyHz, radius);
  return source ? { frequencyHz: source.frequencyHz!, source, snapped: true } : { frequencyHz: roundChannel(frequencyHz), snapped: roundChannel(frequencyHz) !== frequencyHz };
}
