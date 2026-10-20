import { ChevronDown } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { usePlayerInstance, usePlayerSelect } from "@/app/player-context";
import { openSheet } from "@/app/ui";
import { SourceList, TalkingNow } from "@/components/SourceList";
import { Waterfall } from "@/components/Waterfall";
import { rankSources } from "@/domain/adapt";
import {
  differentiators,
  groupSources,
  heardAgo,
  listedSources,
  rankByStrength,
  signalBars,
  StableOrder,
  StrengthTracker,
  TalkTracker,
  type CalmGroups,
} from "@/domain/calm";
import type { Call, Source } from "@/domain/model";
import { busyLine } from "@/domain/summary";
import { usePrefs } from "@/state/prefs";
import { useActivity, useBlocks, useCalls, useCatalog, useHealth, useSources, useStatus } from "@/state/radio";

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
  /** Everything on screen, for picking out what the strip should mark. */
  all: Source[];
  /** Signal bars for the rows in Other activity. */
  bars: ReadonlyMap<string, number>;
  now: number;
}

/** Only what's drawn: two views with the same signature render the same. */
function signature(view: HomeView): string {
  const row = (s: Source) =>
    [s.id, s.name, s.encryption, view.talking.has(s.id), s.inRange, heardAgo(s.lastHeardAt, view.now), view.bars.get(s.id) ?? ""].join("|");
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
  const { settled: catalogSettled } = useCatalog();
  const player = usePlayerInstance();
  const { calls } = useCalls();
  const { activity } = useActivity();
  const health = useHealth();
  const prefs = usePrefs();
  const followed = useMemo(() => new Set(prefs.followed), [prefs.followed]);
  const muted = useMemo(() => new Set(prefs.muted), [prefs.muted]);
  const carrierCalibrated = status.data?.source?.activity_calibrated === true;

  const latest = useRef({ sources, followed, muted, carrierCalibrated, catalogSettled });
  latest.current = { sources, followed, muted, carrierCalibrated, catalogSettled };
  const machinery = useRef<{
    tracker: TalkTracker;
    strength: StrengthTracker;
    order: StableOrder;
    otherOrder: StableOrder;
    resort: boolean;
    sig: string;
  } | null>(null);
  machinery.current ??= {
    tracker: new TalkTracker(),
    strength: new StrengthTracker(),
    order: new StableOrder(),
    otherOrder: new StableOrder(),
    resort: true,
    sig: "",
  };

  const compute = (): HomeView => {
    const m = machinery.current!;
    const { sources, followed, muted, carrierCalibrated, catalogSettled } = latest.current;
    const now = Date.now();
    const talking = m.tracker.observe(sources, now, { carrierCalibrated });
    const strength = m.strength.observe(sources, now);
    // Rank only what is listed, so a row that becomes listed joins the end instead of landing mid-list.
    const listed = listedSources(sources, talking, followed, muted);
    const listedIds = new Set(listed.map((s) => s.id));
    const others = sources.filter((s) => !muted.has(s.id) && !listedIds.has(s.id));
    // Until the engine's names have arrived the ranking would put named rows last: keep re-ranking.
    const resort = m.resort || !catalogSettled;
    const order = m.order.arrange(
      rankSources(listed, followed).map((s) => s.id),
      resort,
    );
    const otherOrder = m.otherOrder.arrange(rankByStrength(others, strength), resort);
    if (catalogSettled && sources.length) m.resort = false;
    const groups = groupSources(sources, talking, order, followed, muted, otherOrder);
    const bars = signalBars(
      groups.other.map((s) => s.id),
      strength,
    );
    const pinned = new Set(groups.talking.map((s) => s.id));
    const marked = [...groups.talking, ...groups.channels.filter((s) => !pinned.has(s.id))];
    // Tell apart rows that share a name within what's shown together, so a folded-away
    // control channel doesn't add a frequency to the voice rows' labels.
    const labels = new Map([...differentiators(marked), ...differentiators([...groups.other, ...groups.muted])]);
    return { groups, talking, labels, marked, all: sources, bars, now };
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

  // Every engine update feeds the trackers, so a key-up shorter than the 1 s render still counts.
  useEffect(() => {
    const m = machinery.current!;
    const now = Date.now();
    m.tracker.observe(sources, now, { carrierCalibrated });
    m.strength.observe(sources, now);
  }, [sources, carrierCalibrated]);

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

  // Taps resolve the source when they happen, not from the last render.
  const listen = useCallback(
    (id: string) => {
      const source = latest.current.sources.find((s) => s.id === id);
      if (source) void player.listen(source);
    },
    [player],
  );

  return {
    view,
    callsBySource,
    followed,
    listen,
    busy: busyLine(activity, health, view.now),
    hasBaseline: !!activity?.baseline,
    health,
  };
}

/**
 * The page title is the listening area, which doesn't change while you look at
 * it. Once there's a baseline, a fixed-height line under it compares busyness
 * with normal (blank while it can't), so nothing above the list changes height.
 */
function AreaTitle({ busy, reserve }: { busy: string | null; reserve: boolean }) {
  const { current, canSwitch } = useBlocks();
  const name = current?.name ?? "Home radio";
  return (
    <div className="px-4 pt-2 pb-2">
      <h1 className="text-[1.375rem] leading-8 font-semibold tracking-[-0.01em] text-heading">
        {canSwitch ? (
          <button
            type="button"
            onClick={() => openSheet({ type: "block" })}
            className="-mx-1 flex max-w-full min-h-11 items-center gap-1 rounded-lg px-1 text-left"
            aria-label={`Listening area: ${name}. Change area`}
          >
            <span className="tabular truncate">{name}</span>
            <ChevronDown className="size-5 shrink-0 text-muted-foreground" aria-hidden />
          </button>
        ) : (
          <span className="tabular -mx-1 flex min-h-11 max-w-full items-center px-1">
            <span className="truncate">{name}</span>
          </span>
        )}
      </h1>
      {reserve && <p className="h-5 truncate text-sm text-muted-foreground">{busy}</p>}
    </div>
  );
}

export function NowScreen({ showStrip }: { showStrip: boolean }) {
  const { view, callsBySource, followed, listen, busy, hasBaseline } = useHome();
  const activeSourceId = usePlayerSelect((s) => (s.kind === "live" ? s.sourceId : null));
  // The playing source is marked wherever it sits in the list, Other activity included.
  const stripMarks = useMemo(
    () => view.all.filter((s) => view.talking.has(s.id) || s.id === activeSourceId),
    [view, activeSourceId],
  );
  return (
    <div>
      <AreaTitle busy={busy} reserve={hasBaseline} />
      {showStrip && (
        <div className="px-4 pb-3">
          <Waterfall variant="strip" sources={stripMarks} talking={view.talking} activeSourceId={activeSourceId} />
        </div>
      )}
      <TalkingNow groups={view.groups} labels={view.labels} followed={followed} now={view.now} onListen={listen} />
      <SourceList
        groups={view.groups}
        talking={view.talking}
        labels={view.labels}
        callsBySource={callsBySource}
        followed={followed}
        bars={view.bars}
        now={view.now}
        onListen={listen}
      />
    </div>
  );
}
