import type { Activity, Health } from "./model";

export function relativeTime(then: number, now: number): string {
  const s = Math.max(0, Math.round((now - then) / 1000));
  if (s < 10) return "just now";
  if (s < 60) return `${s} sec ago`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m} min ago`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h} hr ago`;
  return `${Math.round(h / 24)} d ago`;
}

export type Busyness = "busier" | "normal" | "quieter" | "unknown";

/** Last 15 minutes of block talk-time against the learned baseline for the same buckets. */
export function busyness(activity: Activity | null, now: number): { level: Busyness; ratio: number | null } {
  if (!activity?.baseline) return { level: "unknown", ratio: null };
  const window = 15 * 60 * 1000;
  let actual = 0;
  let typical = 0;
  activity.block.forEach((v, i) => {
    const t = activity.start + i * activity.bucketMs;
    if (t + activity.bucketMs > now - window && t <= now) {
      actual += v;
      typical += activity.baseline?.[i] ?? 0;
    }
  });
  if (typical < 1) return actual > 30 ? { level: "busier", ratio: null } : { level: "normal", ratio: null };
  const ratio = actual / typical;
  if (ratio >= 1.6) return { level: "busier", ratio };
  if (ratio <= 0.5) return { level: "quieter", ratio };
  return { level: "normal", ratio };
}

/**
 * The one small status line under the area name: how busy it is compared with
 * normal for this weekday and time. Null until there's a baseline to compare
 * with, or while the radio isn't listening; the screen then shows nothing.
 */
export function busyLine(activity: Activity | null, health: Health, now: number): string | null {
  if (health.state !== "listening") return null;
  const level = busyness(activity, now).level;
  if (level === "unknown") return null;
  if (level === "busier") return "Busier than usual";
  if (level === "quieter") return "Quieter than usual";
  return "About normal";
}
