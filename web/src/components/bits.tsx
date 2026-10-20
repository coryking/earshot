import type { Call } from "@/domain/model";
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
