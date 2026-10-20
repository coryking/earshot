import type { Activity, Health, Source } from "./model";

const words = ["No", "One", "Two", "Three", "Four", "Five", "Six", "Seven", "Eight", "Nine", "Ten"];

export function countWord(n: number) {
  return words[n] ?? String(n);
}

export function partOfDay(date: Date) {
  const h = date.getHours();
  if (h >= 5 && h < 12) return "morning";
  if (h >= 12 && h < 17) return "afternoon";
  if (h >= 17 && h < 21) return "evening";
  return "night";
}

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

/** The sentence at the top of Now: what's happening, then how that compares with normal. */
export function headline(
  sources: Source[],
  activity: Activity | null,
  health: Health,
  now: number,
): { lead: string; context: string } {
  if (health.state === "offline") return { lead: health.lead ?? "The radio isn't connected.", context: health.detail };
  if (health.state === "paused") return { lead: "The radio is paused.", context: health.detail };

  const talking = sources.filter((s) => s.talking);
  let lead: string;
  if (talking.length === 1) lead = `${talking[0].name} is talking.`;
  else if (talking.length > 1) lead = `${countWord(talking.length)} channels talking.`;
  else {
    const last = sources
      .filter((s) => s.lastHeardAt !== null)
      .sort((a, b) => (b.lastHeardAt ?? 0) - (a.lastHeardAt ?? 0))[0];
    lead = last?.lastHeardAt
      ? `Quiet right now. ${last.name} was last, ${relativeTime(last.lastHeardAt, now)}.`
      : "Quiet so far.";
  }

  const day = new Date(now).toLocaleDateString("en-US", { weekday: "long" });
  const when = `for a ${day} ${partOfDay(new Date(now))}`;
  const level = busyness(activity, now).level;
  const context =
    level === "busier"
      ? `Busier than usual ${when}.`
      : level === "quieter"
        ? `Quieter than usual ${when}.`
        : level === "normal"
          ? `About normal ${when}.`
          : "Still learning what normal sounds like here.";
  return { lead, context };
}
