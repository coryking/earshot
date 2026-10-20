import { ChevronDown } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { usePlayerSelect } from "@/app/player-context";
import { openSheet } from "@/app/ui";
import { SourceList, TalkingNow } from "@/components/SourceList";
import { Waterfall } from "@/components/Waterfall";
import { rankSources } from "@/domain/adapt";
import { differentiators, groupSources, heardAgo, StableOrder, TalkTracker, type CalmGroups } from "@/domain/calm";
import type { Call, Source } from "@/domain/model";
import { busyLine } from "@/domain/summary";
import { usePrefs } from "@/state/prefs";
import { useActivity, useBlocks, useCalls, useHealth, useSources, useStatus } from "@/state/radio";

/** The home view refreshes at most this often, whatever the engine sends. */
export const VIEW_INTERVAL_MS = 1000;
/** Coming back to the page after this long re-sorts the list (nobody was looking). */
const RESORT_AFTER_HIDDEN_MS = 30_000;

export interface HomeView {
  groups: CalmGroups;
  talking: ReadonlyMap<string, number>;
  labels: ReadonlyMap<string, string>;
  /** Sources worth marking on the waterfall: talking first, then the main list. */
  marked: Source[];
  now: number;
}

/** Only what's drawn: two views with the same signature render the same. */
function signature(view: HomeView): string {
  const row = (s: Source) =>
    [s.id, s.name, s.summary, view.talking.has(s.id), s.listenable.ok, s.inRange, heardAgo(s.lastHeardAt, view.now)].join("|");
  const { groups } = view;
  return JSON.stringify([
    groups.talking.map(row),
    groups.moreTalking,
    groups.channels.map(row),
    groups.other.map(row),
    groups.muted.map(row),
    [...view.labels],
    // Call lanes drift slowly; let them catch up every half minute.
    Math.floor(view.now / 30_000),
  ]);
}

/**
 * The calm home model. Engine updates arrive many times a second; this samples
 * them once a second, smooths "talking" with hysteresis, keeps the list order
 * frozen while it's on screen, and only re-renders when something visible
 * changed. The order is re-ranked on open, on follow or mute, and when the
 * page returns after being hidden a while.
 */
export function useHome() {
  const sources = useSources();
  const status = useStatus();
  const { calls } = useCalls();
  const { activity } = useActivity();
  const health = useHealth();
  const prefs = usePrefs();
  const followed = useMemo(() => new Set(prefs.followed), [prefs.followed]);
  const muted = useMemo(() => new Set(prefs.muted), [prefs.muted]);
  const carrierCalibrated = status.data?.source?.activity_calibrated === true;

  const latest = useRef({ sources, followed, muted, carrierCalibrated });
  latest.current = { sources, followed, muted, carrierCalibrated };
  const machinery = useRef<{ tracker: TalkTracker; order: StableOrder; resort: boolean; sig: string } | null>(null);
  machinery.current ??= { tracker: new TalkTracker(), order: new StableOrder(), resort: true, sig: "" };

  const compute = (): HomeView => {
    const m = machinery.current!;
    const { sources, followed, muted, carrierCalibrated } = latest.current;
    const now = Date.now();
    const talking = m.tracker.observe(sources, now, { carrierCalibrated });
    const order = m.order.arrange(
      rankSources(sources, followed).map((s) => s.id),
      m.resort,
    );
    m.resort = false;
    const groups = groupSources(sources, talking, order, followed, muted);
    const marked = [...groups.talking, ...groups.channels.filter((s) => !talking.has(s.id))];
    return { groups, talking, labels: differentiators(sources), marked, now };
  };

  const [view, setView] = useState<HomeView>(compute);
  const refresh = useRef(() => {});
  refresh.current = () => {
    const next = compute();
    const sig = signature(next);
    if (sig === machinery.current!.sig) return;
    machinery.current!.sig = sig;
    setView(next);
  };

  useEffect(() => {
    const t = setInterval(() => refresh.current(), VIEW_INTERVAL_MS);
    let hiddenAt: number | null = null;
    const onVisibility = () => {
      if (document.hidden) hiddenAt = Date.now();
      else {
        if (hiddenAt !== null && Date.now() - hiddenAt >= RESORT_AFTER_HIDDEN_MS) machinery.current!.resort = true;
        hiddenAt = null;
        refresh.current();
      }
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      clearInterval(t);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, []);

  // A follow or mute is the listener's own action: re-rank right away.
  const firstPrefs = useRef(true);
  useEffect(() => {
    if (firstPrefs.current) {
      firstPrefs.current = false;
      return;
    }
    machinery.current!.resort = true;
    refresh.current();
  }, [followed, muted]);

  const callsBySource = useMemo(() => {
    const map = new Map<string, Call[]>();
    for (const c of calls) {
      const list = map.get(c.sourceId);
      if (list) list.push(c);
      else map.set(c.sourceId, [c]);
    }
    return map;
  }, [calls]);

  return { view, callsBySource, followed, busy: busyLine(activity, health, view.now), health };
}

/**
 * The page title is the listening area, which doesn't change while you look at
 * it. One small fixed-height line under it compares busyness with normal once
 * there's a baseline; nothing above the list changes height as data arrives.
 */
function AreaTitle({ busy }: { busy: string | null }) {
  const { current } = useBlocks();
  const name = current?.name ?? "Home radio";
  return (
    <div className="px-4 pt-2 pb-2">
      <h1 className="text-[1.375rem] leading-8 font-semibold tracking-[-0.01em] text-heading">
        <button
          type="button"
          onClick={() => openSheet({ type: "block" })}
          className="-mx-1 flex max-w-full min-h-11 items-center gap-1 rounded-lg px-1 text-left"
          aria-label={`Listening area: ${name}. Change area`}
        >
          <span className="tabular truncate">{name}</span>
          <ChevronDown className="size-5 shrink-0 text-muted-foreground" aria-hidden />
        </button>
      </h1>
      {busy && <p className="h-5 truncate text-sm text-muted-foreground">{busy}</p>}
    </div>
  );
}

export function NowScreen({ showStrip }: { showStrip: boolean }) {
  const { view, callsBySource, followed, busy } = useHome();
  const activeSourceId = usePlayerSelect((s) => (s.kind === "live" ? s.sourceId : null));
  const stripMarks = useMemo(
    () => view.marked.filter((s) => view.talking.has(s.id) || s.id === activeSourceId),
    [view, activeSourceId],
  );
  return (
    <div>
      <AreaTitle busy={busy} />
      {showStrip && (
        <div className="px-4 pb-3">
          <Waterfall variant="strip" sources={stripMarks} talking={view.talking} activeSourceId={activeSourceId} />
          <p className="mt-1 h-4 truncate text-xs text-muted-foreground">Live airwaves. Marks show who's talking.</p>
        </div>
      )}
      <TalkingNow groups={view.groups} labels={view.labels} followed={followed} now={view.now} />
      <SourceList
        groups={view.groups}
        talking={view.talking}
        labels={view.labels}
        callsBySource={callsBySource}
        followed={followed}
        now={view.now}
      />
    </div>
  );
}
