import type { Activity, Call, Source } from "@/domain/model";
import { cn } from "@/lib/utils";

/** Three bars that move while a source is talking. */
export function LevelMeter({ active, className }: { active: boolean; className?: string }) {
  return (
    <span aria-hidden className={cn("inline-flex h-4 items-end gap-[3px]", className)}>
      {[0, 0.25, 0.5].map((delay) => (
        <span
          key={delay}
          className={cn("w-[3px] rounded-full", active ? "meter-bar h-4 bg-lamp" : "h-1.5 bg-muted-foreground/40")}
          style={active ? { animationDelay: `${delay}s` } : undefined}
        />
      ))}
    </span>
  );
}

/**
 * Activity lane: talk-time per bucket over the last `minutes`, drawn as small
 * bars. Borrowed from observability timelines.
 */
export function ActivityLane({
  series,
  activity,
  minutes = 30,
  now,
  className,
  label,
}: {
  series: number[] | undefined;
  activity: Activity | null;
  minutes?: number;
  now: number;
  className?: string;
  label: string;
}) {
  if (!activity) return null;
  const bars = 24;
  const window = minutes * 60_000;
  const values = new Array(bars).fill(0);
  (series ?? []).forEach((v, i) => {
    const t = activity.start + i * activity.bucketMs;
    const age = now - t;
    if (age < 0 || age > window) return;
    const slot = Math.min(bars - 1, Math.floor(((window - age) / window) * bars));
    values[slot] += v;
  });
  const max = Math.max(30, ...values);
  return (
    <svg
      viewBox={`0 0 ${bars * 4} 12`}
      preserveAspectRatio="none"
      className={cn("h-3 w-full", className)}
      role="img"
      aria-label={label}
    >
      {values.map((v, i) => {
        const h = v > 0 ? Math.max(2, (v / max) * 12) : 1;
        return (
          <rect
            key={i}
            x={i * 4}
            y={12 - h}
            width={3}
            height={h}
            rx={0.8}
            className={v > 0 ? "fill-signal/70" : "fill-muted-foreground/25"}
          />
        );
      })}
    </svg>
  );
}

export function laneLabel(source: Source, series: number[] | undefined, activity: Activity | null, now: number) {
  if (!activity) return `${source.name}: no history yet`;
  let seconds = 0;
  (series ?? []).forEach((v, i) => {
    if (now - (activity.start + i * activity.bucketMs) <= 30 * 60_000) seconds += v;
  });
  return seconds < 1
    ? `${source.name}: no talking in the last 30 minutes`
    : `${source.name}: about ${Math.max(1, Math.round(seconds / 60))} min of talking in the last 30 minutes`;
}

/**
 * Call lane: each call in the last `minutes` as a block on a timeline, live
 * edge on the right. A call still in progress runs to the edge in lamp amber.
 */
export function CallLane({
  calls,
  now,
  minutes = 30,
  talking,
  className,
}: {
  calls: Call[];
  now: number;
  minutes?: number;
  talking: boolean;
  className?: string;
}) {
  const window = minutes * 60_000;
  const start = now - window;
  const recent = calls.filter((c) => (c.endedAt ?? now) >= start);
  const label =
    recent.length === 0
      ? `No calls in the last ${minutes} minutes`
      : `${recent.length} call${recent.length === 1 ? "" : "s"} in the last ${minutes} minutes`;
  return (
    <div role="img" aria-label={label} className={cn("relative h-1.5 w-full rounded-full bg-muted-foreground/15", className)}>
      {recent.map((c) => {
        const from = Math.max(start, c.startedAt);
        const live = c.endedAt === null && talking;
        const to = live ? now : (c.endedAt ?? c.startedAt + 1000);
        const left = ((from - start) / window) * 100;
        const width = Math.max(0.9, ((to - from) / window) * 100);
        return (
          <span
            key={c.id}
            className={cn("absolute inset-y-0 rounded-full", live ? "bg-lamp" : c.encrypted ? "bg-muted-foreground/60" : "bg-signal")}
            style={{ left: `${Math.min(99.1, left)}%`, width: `${Math.min(width, 100 - left)}%` }}
          />
        );
      })}
    </div>
  );
}
