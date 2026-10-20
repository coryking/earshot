import { Play } from "lucide-react";
import { useMemo, useState } from "react";
import { usePlayerInstance } from "@/app/player-context";
import { openSheet, useNow } from "@/app/ui";
import { CallLane } from "@/components/bits";
import { CallItem } from "@/components/Sheets";
import { Button } from "@/components/ui/button";
import type { Call, Source } from "@/domain/model";
import { cn } from "@/lib/utils";
import { usePrefs } from "@/state/prefs";
import { useCalls, useSources } from "@/state/radio";

function groupOf(call: Call, now: number) {
  const age = now - call.startedAt;
  if (age < 5 * 60_000) return "Just now";
  if (age < 60 * 60_000) return "Earlier this hour";
  if (new Date(call.startedAt).toDateString() === new Date(now).toDateString()) return "Today";
  return "Before today";
}

/** Recent calls newest first, like a podcast queue: playback continues down the list. */
export function CatchUpScreen({ timeline, embedded = false }: { timeline: boolean; embedded?: boolean }) {
  const Title = embedded ? "h2" : "h1";
  const { calls, recorded, failed } = useCalls();
  const sources = useSources();
  const { followed, muted } = usePrefs();
  const [onlyFollowed, setOnlyFollowed] = useState(false);
  const player = usePlayerInstance();
  const now = useNow();
  const names = useMemo(() => new Map(sources.map((s) => [s.id, s.name])), [sources]);
  const visible = calls.filter(
    (c) => !muted.includes(c.sourceId) && (!onlyFollowed || followed.includes(c.sourceId)),
  );
  const playable = visible.filter((c) => c.audioUrl);
  const groups = new Map<string, Call[]>();
  for (const c of visible) {
    const g = groupOf(c, now);
    (groups.get(g) ?? groups.set(g, []).get(g)!).push(c);
  }
  const nameOf = (id: string) => names.get(id) ?? "Unnamed channel";

  return (
    <div className="pb-4">
      <div className="px-4 pt-3">
        <Title className="text-[1.75rem] leading-tight font-semibold text-heading">Catch up</Title>
        <p className="mt-1 text-muted-foreground">
          {visible.length
            ? `${visible.length} call${visible.length === 1 ? "" : "s"} recently, newest first.`
            : "No calls yet."}
        </p>
      </div>
      <div className="flex flex-wrap items-center gap-2 px-4 py-3">
        <div role="group" aria-label="Show" className="flex rounded-lg bg-secondary p-1">
          {[
            [false, "All"],
            [true, "Following"],
          ].map(([value, label]) => (
            <button
              key={String(label)}
              type="button"
              aria-pressed={onlyFollowed === value}
              onClick={() => setOnlyFollowed(value as boolean)}
              className={cn(
                "min-h-9 rounded-md px-3 text-sm",
                onlyFollowed === value ? "bg-card text-heading shadow-sm" : "text-muted-foreground",
              )}
            >
              {label}
            </button>
          ))}
        </div>
        <div className="flex-1" />
        {playable.length > 1 && (
          <Button
            variant="secondary"
            className="h-11 gap-2"
            onClick={() => player.playCall(playable[0], nameOf(playable[0].sourceId), playable.slice(1), nameOf)}
          >
            <Play className="size-4 fill-current" /> Play all
          </Button>
        )}
      </div>
      {!recorded && (
        <p className="mx-4 mb-3 rounded-lg bg-secondary p-3 text-sm text-secondary-foreground">
          {failed
            ? "Couldn't load recorded calls from the radio box; trying again. Meanwhile this shows calls heard while this page has been open."
            : "The radio box doesn't record calls yet, so this list only shows calls heard while this page has been open, without audio."}
        </p>
      )}
      {timeline && visible.length > 0 && <Timeline calls={visible} sources={sources} now={now} />}
      {[...groups.entries()].map(([group, items]) => (
        <section key={group} aria-label={group}>
          <h3 className="sticky top-0 z-10 bg-background/95 px-4 py-1.5 text-sm text-muted-foreground backdrop-blur">
            {group}
          </h3>
          <ul>
            {items.map((c) => (
              <CallItem
                key={c.id}
                call={c}
                name={nameOf(c.sourceId)}
                now={now}
                queueAfter={() => playable.slice(playable.indexOf(c) + 1)}
                nameFor={nameOf}
              />
            ))}
          </ul>
        </section>
      ))}
      {onlyFollowed && !visible.length && (
        <p className="px-4 text-sm text-muted-foreground">Follow channels with the star to see just their calls here.</p>
      )}
    </div>
  );
}

/** Desktop: last hour as swimlanes per source (observability-style timeline). */
function Timeline({ calls, sources, now }: { calls: Call[]; sources: Source[]; now: number }) {
  const bySource = new Map<string, Call[]>();
  calls.forEach((c) => (bySource.get(c.sourceId) ?? bySource.set(c.sourceId, []).get(c.sourceId)!).push(c));
  const rows = sources.filter((s) => bySource.has(s.id));
  return (
    <section aria-label="Last hour by channel" className="mx-4 mb-4 rounded-xl border border-border p-3">
      <div className="mb-2 flex justify-between text-xs text-muted-foreground">
        <span>1 hour ago</span>
        <span>now</span>
      </div>
      <ul className="space-y-2">
        {rows.map((s) => (
          <li key={s.id} className="grid grid-cols-[10rem_1fr] items-center gap-3">
            <button
              type="button"
              className="truncate text-left text-sm text-foreground hover:text-heading"
              onClick={() => openSheet({ type: "source", id: s.id })}
            >
              {s.name}
            </button>
            <CallLane calls={bySource.get(s.id)!} now={now} minutes={60} talking={s.talking} className="h-2.5" />
          </li>
        ))}
      </ul>
    </section>
  );
}
