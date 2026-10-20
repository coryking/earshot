import { ChevronDown, Info, Lock, Star } from "lucide-react";
import { memo, useEffect, useRef, useState, type ReactNode } from "react";
import { usePlayerInstance, usePlayerSelect } from "@/app/player-context";
import { openSheet } from "@/app/ui";
import { CallLane, LevelMeter } from "@/components/bits";
import { heardAgo, type CalmGroups } from "@/domain/calm";
import type { Call, Source } from "@/domain/model";
import { cn } from "@/lib/utils";

export interface ListProps {
  groups: CalmGroups;
  talking: ReadonlyMap<string, number>;
  labels: ReadonlyMap<string, string>;
  callsBySource: Map<string, Call[]>;
  followed: ReadonlySet<string>;
  now: number;
}

type Variant = "card" | "row" | "quiet";

interface RowProps {
  source: Source;
  variant: Variant;
  talking: boolean;
  label: string | undefined;
  followed: boolean;
  calls?: Call[];
  now: number;
}

/**
 * One source. The whole main area is a single button that listens (tapping the
 * playing source again stops it); "About" is a separate, clearly bounded button
 * on the right. There is no long-press or tap-for-info on the main target.
 */
const SourceRow = memo(function SourceRow({ source, variant, talking, label, followed, calls, now }: RowProps) {
  const player = usePlayerInstance();
  const playing = usePlayerSelect((s) => s.kind === "live" && s.sourceId === source.id);
  const isPrivate = source.encryption === "encrypted";
  const right = talking ? (
    <span className="flex shrink-0 items-center gap-2 text-xs font-semibold text-lamp">
      {isPrivate ? <Lock className="size-3.5" aria-hidden /> : <LevelMeter active />}
      <span>{isPrivate ? "Private call" : "Talking"}</span>
    </span>
  ) : variant === "row" ? (
    <span className="tabular shrink-0 text-xs text-muted-foreground">
      {source.inRange ? (heardAgo(source.lastHeardAt, now) ?? "") : "Out of range"}
    </span>
  ) : null;
  const second = playing ? "Listening on this device" : [label, source.summary].filter(Boolean).join(" · ");
  // Cards sit inside their own list item (the presence wrapper); rows are list items themselves.
  const Root = variant === "card" ? "div" : "li";

  return (
    <Root
      className={cn(
        "relative flex items-stretch",
        variant === "card" && "overflow-hidden rounded-xl border border-lamp/35 bg-card",
        playing && (variant === "card" ? "border-signal" : "bg-secondary/60"),
      )}
      data-source-id={source.id}
      data-talking={talking || undefined}
    >
      {playing && <span className="absolute inset-y-2 left-0 w-1 rounded-r bg-signal" aria-hidden />}
      <button
        type="button"
        data-source-row
        className={cn(
          "flex min-h-14 min-w-0 flex-1 flex-col justify-center gap-0.5 pr-2 pl-4 text-left focus-visible:-outline-offset-2",
          variant === "quiet" ? "py-2" : "py-3",
        )}
        onClick={() => void player.listen(source)}
      >
        <span className="sr-only">{playing ? "Stop listening to" : "Listen to"}</span>{" "}
        <span className="flex min-w-0 items-center gap-2">
          {followed && <Star className="size-3.5 shrink-0 fill-lamp text-lamp" aria-label="Following" />}
          <span
            className={cn(
              "min-w-0 flex-1 truncate leading-6",
              variant === "quiet" ? "text-[0.9375rem]" : "text-[1.0625rem]",
              talking ? "font-semibold text-heading" : "text-foreground",
              !source.named && "tabular",
            )}
          >
            {source.name}
          </span>
          {right}
        </span>
        <span className="tabular truncate text-sm text-muted-foreground">{second}</span>
        {variant === "row" && <CallLane calls={calls ?? []} now={now} talking={talking} className="mt-1.5" />}
      </button>
      <button
        type="button"
        className="my-2 grid w-12 shrink-0 place-items-center border-l border-border/70 text-muted-foreground hover:text-heading focus-visible:-outline-offset-2"
        onClick={() => openSheet({ type: "source", id: source.id })}
        aria-label={`About ${source.name}`}
        data-about
      >
        <Info className="size-5" aria-hidden />
      </button>
    </Root>
  );
});

/**
 * Keeps a leaving item on screen briefly so it can fade and fold away instead
 * of vanishing. Order follows `items`; leavers stay where they were.
 */
function usePresence<T extends { id: string }>(items: T[], ms = 320) {
  const [leaving, setLeaving] = useState<Map<string, T>>(new Map());
  const previous = useRef<T[]>(items);
  useEffect(() => {
    const current = new Set(items.map((i) => i.id));
    const gone = previous.current.filter((i) => !current.has(i.id));
    previous.current = items;
    if (!gone.length) return;
    setLeaving((m) => new Map([...m, ...gone.map((g) => [g.id, g] as const)]));
    const t = setTimeout(
      () => setLeaving((m) => new Map([...m].filter(([id]) => !gone.some((g) => g.id === id)))),
      ms,
    );
    return () => clearTimeout(t);
  }, [items, ms]);
  const present = new Set(items.map((i) => i.id));
  return [
    ...items.map((item) => ({ item, leaving: false })),
    ...[...leaving.values()].filter((i) => !present.has(i.id)).map((item) => ({ item, leaving: true })),
  ];
}

/** The small pinned area at the top: who is talking right now, at most three. */
export const TalkingNow = memo(function TalkingNow({
  groups,
  labels,
  followed,
  now,
}: Pick<ListProps, "groups" | "labels" | "followed" | "now">) {
  const shown = usePresence(groups.talking);
  return (
    <section aria-labelledby="talking-now-title" className="px-4 pb-1">
      <h2 id="talking-now-title" className="flex h-6 items-center text-sm font-semibold text-muted-foreground">
        Talking now
      </h2>
      <div className="min-h-[4.25rem]">
        {groups.talking.length === 0 && !shown.length ? (
          <p className="calm-fade flex min-h-[4.25rem] flex-col justify-center rounded-xl border border-dashed border-border px-4 text-sm text-muted-foreground">
            <span className="text-foreground">Nobody's talking right now.</span>
            <span className="truncate">Tap a channel below to wait for its next call.</span>
          </p>
        ) : (
          <ul aria-label="Talking now" className="flex flex-col">
            {shown.map(({ item, leaving }) => (
              <li key={item.id} className={cn("calm-presence grid", leaving && "calm-leaving")} inert={leaving}>
                <div className="min-h-0 overflow-hidden">
                  <div className="pb-2">
                    <SourceRow
                      source={item}
                      variant="card"
                      talking={!leaving}
                      label={labels.get(item.id)}
                      followed={followed.has(item.id)}
                      now={now}
                    />
                  </div>
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>
      {groups.moreTalking > 0 && (
        <p className="h-5 truncate text-xs text-muted-foreground">
          {groups.moreTalking} more talking, marked in the list below.
        </p>
      )}
    </section>
  );
});

/** The quiet list: followed and recently heard channels in a stable order, then the folded groups. */
export const SourceList = memo(function SourceList({ groups, talking, labels, callsBySource, followed, now }: ListProps) {
  const [showOther, setShowOther] = useState(false);
  const [showMuted, setShowMuted] = useState(false);
  const row = (s: Source, variant: Variant) => (
    <SourceRow
      key={s.id}
      source={s}
      variant={variant}
      talking={talking.has(s.id)}
      label={labels.get(s.id)}
      followed={followed.has(s.id)}
      calls={callsBySource.get(s.id)}
      now={now}
    />
  );
  return (
    <section aria-labelledby="channels-title" className="mt-2">
      <h2 id="channels-title" className="flex h-8 items-center px-4 text-sm font-semibold text-muted-foreground">
        Channels
      </h2>
      {groups.channels.length ? (
        <ul aria-label="Channels" className="divide-y divide-border/60" data-source-list>
          {groups.channels.map((s) => row(s, "row"))}
        </ul>
      ) : (
        <p className="px-4 pb-4 text-sm text-muted-foreground" data-source-list>
          No voices heard yet. Channels show up here the first time the radio hears someone talk on them.
        </p>
      )}
      {groups.other.length > 0 && (
        <Folded
          title={`Other activity (${groups.other.length})`}
          open={showOther}
          onToggle={() => setShowOther((v) => !v)}
          note="Signals the radio picks up but hasn't heard a voice on: data, system control channels, or noise. Tap one to listen in case someone talks."
        >
          <ul aria-label="Other activity" className="divide-y divide-border/40" data-source-list>
            {groups.other.map((s) => row(s, "quiet"))}
          </ul>
        </Folded>
      )}
      {groups.muted.length > 0 && (
        <Folded title={`Muted (${groups.muted.length})`} open={showMuted} onToggle={() => setShowMuted((v) => !v)}>
          <ul aria-label="Muted channels" className="divide-y divide-border/40 opacity-70" data-source-list>
            {groups.muted.map((s) => row(s, "quiet"))}
          </ul>
        </Folded>
      )}
    </section>
  );
});

function Folded({
  title,
  open,
  onToggle,
  note,
  children,
}: {
  title: string;
  open: boolean;
  onToggle(): void;
  note?: string;
  children: ReactNode;
}) {
  return (
    <div className="border-t border-border/60">
      <button
        type="button"
        className="tabular flex min-h-12 w-full items-center gap-2 px-4 text-sm text-muted-foreground"
        onClick={onToggle}
        aria-expanded={open}
      >
        <ChevronDown className={cn("size-4 transition-transform motion-reduce:transition-none", !open && "-rotate-90")} aria-hidden />
        {title}
      </button>
      {open && (
        <>
          {note && <p className="px-4 pb-2 text-xs text-muted-foreground">{note}</p>}
          {children}
        </>
      )}
    </div>
  );
}
