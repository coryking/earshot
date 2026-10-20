import { ChevronDown, Info, Lock, Star } from "lucide-react";
import { memo, useEffect, useRef, useState, type ReactNode } from "react";
import { usePlayerSelect } from "@/app/player-context";
import { openSheet } from "@/app/ui";
import { CallLane, LevelMeter } from "@/components/bits";
import { heardAgo, MAX_BARS, type CalmGroups } from "@/domain/calm";
import type { Call, Source } from "@/domain/model";
import { cn } from "@/lib/utils";

export interface ListProps {
  groups: CalmGroups;
  talking: ReadonlyMap<string, number>;
  labels: ReadonlyMap<string, string>;
  callsBySource: Map<string, Call[]>;
  followed: ReadonlySet<string>;
  /** Signal bars for the rows in Other activity. */
  bars: ReadonlyMap<string, number>;
  now: number;
  /** Listen to a source by id; the id is resolved when the tap happens. */
  onListen(id: string): void;
}

type Variant = "card" | "row" | "quiet";

interface RowProps {
  source: Source;
  variant: Variant;
  talking: boolean;
  label: string | undefined;
  followed: boolean;
  calls?: Call[];
  /** 1 to MAX_BARS signal bars, for rows in Other activity. */
  bars?: number;
  now: number;
  onListen(id: string): void;
}

/** Signal strength as bars: no words, fixed width, so a changing level never moves the row. */
function Bars({ level }: { level: number }) {
  return (
    <span className="flex h-4 w-5 shrink-0 items-end gap-0.5" role="img" aria-label={`Signal ${level} of ${MAX_BARS}`}>
      {Array.from({ length: MAX_BARS }, (_, i) => (
        <span
          key={i}
          className={cn("w-[3px] rounded-[1px]", i < level ? "bg-signal" : "bg-muted-foreground/25")}
          style={{ height: `${5 + i * 3}px` }}
        />
      ))}
    </span>
  );
}

/**
 * One source. The whole main area is a single button that listens (tapping the
 * playing source again stops it); "About" is a separate, clearly bounded button
 * on the right. There is no long-press or tap-for-info on the main target.
 */
const SourceRow = memo(function SourceRow({ source, variant, talking, label, followed, calls, bars, now, onListen }: RowProps) {
  const playing = usePlayerSelect((s) => s.kind === "live" && s.sourceId === source.id);
  const isPrivate = source.encryption === "encrypted";
  const right = talking ? (
    <span className="flex shrink-0 items-center gap-2 text-lamp">
      {isPrivate ? <Lock className="size-3.5" aria-hidden /> : <LevelMeter active />}
      <span className="sr-only">{isPrivate ? "Private call" : "Talking"}</span>
    </span>
  ) : variant === "row" ? (
    <span className="tabular flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground">
      {isPrivate && <Lock className="size-3" aria-label="Private" />}
      {source.inRange ? (heardAgo(source.lastHeardAt, now) ?? "") : "Out of range"}
    </span>
  ) : variant === "quiet" && bars !== undefined && source.inRange ? (
    <Bars level={bars} />
  ) : null;
  // Names say what a source is; the second line only tells apart rows that share a name.
  const second = playing ? "Listening" : (label ?? null);
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
        onClick={() => onListen(source.id)}
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
        {second && <span className="tabular truncate text-sm text-muted-foreground">{second}</span>}
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
 * of vanishing. The leaver is worked out during the render in which it drops
 * out of `items`, so its element never unmounts: it only gains the leaving
 * class. Order follows `items`; leavers stay where they were. Each leaver has
 * its own removal timer, which only unmounting or the item coming back cancels.
 */
export function usePresence<T extends { id: string }>(items: T[], ms = 320) {
  const [leaving, setLeaving] = useState<Map<string, T>>(new Map());
  const [seen, setSeen] = useState(items);
  if (items !== seen) {
    setSeen(items);
    const current = new Set(items.map((i) => i.id));
    const gone = seen.filter((i) => !current.has(i.id));
    if (gone.length || [...leaving.keys()].some((id) => current.has(id))) {
      setLeaving((m) => {
        const next = new Map([...m].filter(([id]) => !current.has(id)));
        for (const g of gone) next.set(g.id, g);
        return next;
      });
    }
  }
  const timers = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  useEffect(() => {
    for (const [id, t] of timers.current)
      if (!leaving.has(id)) {
        clearTimeout(t);
        timers.current.delete(id);
      }
    for (const id of leaving.keys())
      if (!timers.current.has(id))
        timers.current.set(
          id,
          setTimeout(() => {
            timers.current.delete(id);
            setLeaving((m) => {
              const next = new Map(m);
              next.delete(id);
              return next;
            });
          }, ms),
        );
  }, [leaving, ms]);
  useEffect(() => {
    const all = timers.current;
    return () => {
      for (const t of all.values()) clearTimeout(t);
      all.clear();
    };
  }, []);
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
  onListen,
}: Pick<ListProps, "groups" | "labels" | "followed" | "now" | "onListen">) {
  const shown = usePresence(groups.talking);
  return (
    <section aria-labelledby="talking-now-title" className="px-4 pb-1">
      <h2
        id="talking-now-title"
        className="flex h-6 items-center justify-between text-sm font-semibold text-muted-foreground"
      >
        Talking now
        {groups.moreTalking > 0 && <span className="tabular font-normal">+{groups.moreTalking}</span>}
      </h2>
      <div className="min-h-[4.25rem]">
        {groups.talking.length === 0 && !shown.length ? (
          <p className="calm-fade flex min-h-[4.25rem] items-center rounded-xl border border-dashed border-border px-4 text-sm text-muted-foreground">
            Nobody
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
                      onListen={onListen}
                    />
                  </div>
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>
    </section>
  );
});

/** The quiet list: followed and recently heard channels in a stable order, then the folded groups. */
export const SourceList = memo(function SourceList({
  groups,
  talking,
  labels,
  callsBySource,
  followed,
  bars,
  now,
  onListen,
}: ListProps) {
  // With no voice channels yet, Other activity is all there is: start it open, and keep it as it is.
  const [otherChoice, setOtherChoice] = useState<boolean | null>(null);
  const [otherLatch, setOtherLatch] = useState(false);
  if (!otherLatch && !groups.channels.length && groups.other.length) setOtherLatch(true);
  const showOther = otherChoice ?? otherLatch;
  const [showMuted, setShowMuted] = useState(false);
  const row = (s: Source, variant: Variant, withBars = false) => (
    <SourceRow
      key={s.id}
      source={s}
      variant={variant}
      talking={talking.has(s.id)}
      label={labels.get(s.id)}
      followed={followed.has(s.id)}
      calls={callsBySource.get(s.id)}
      bars={withBars ? bars.get(s.id) : undefined}
      now={now}
      onListen={onListen}
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
          None yet
        </p>
      )}
      {groups.other.length > 0 && (
        <Folded
          title={`Other activity (${groups.other.length})`}
          open={showOther}
          onToggle={() => setOtherChoice(!showOther)}
        >
          <ul aria-label="Other activity" className="divide-y divide-border/40" data-source-list>
            {groups.other.map((s) => row(s, "quiet", true))}
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
  children,
}: {
  title: string;
  open: boolean;
  onToggle(): void;
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
      {open && children}
    </div>
  );
}
