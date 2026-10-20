import { useMemo } from "react";
import { usePlayerInstance, usePlayerSelect } from "@/app/player-context";
import { openSheet, useNow } from "@/app/ui";
import { SourceList } from "@/components/SourceList";
import { Waterfall } from "@/components/Waterfall";
import { rankSources } from "@/domain/adapt";
import type { Call, Source } from "@/domain/model";
import { headline } from "@/domain/summary";
import { cn } from "@/lib/utils";
import { usePrefs } from "@/state/prefs";
import { useActivity, useCalls, useHealth, useSources } from "@/state/radio";

export function useHome() {
  const sources = useSources();
  const { calls } = useCalls();
  const { activity } = useActivity();
  const health = useHealth();
  const prefs = usePrefs();
  const now = useNow();
  const followed = useMemo(() => new Set(prefs.followed), [prefs.followed]);
  const muted = useMemo(() => new Set(prefs.muted), [prefs.muted]);
  const ranked = useMemo(() => rankSources(sources, followed), [sources, followed]);
  const callsBySource = useMemo(() => {
    const map = new Map<string, Call[]>();
    for (const c of calls) {
      const list = map.get(c.sourceId);
      if (list) list.push(c);
      else map.set(c.sourceId, [c]);
    }
    return map;
  }, [calls]);
  const text = headline(
    sources.filter((s) => !muted.has(s.id)),
    activity,
    health,
    now,
  );
  return { ranked, callsBySource, followed, muted, now, text, health };
}

export function Headline({ lead, context, large }: { lead: string; context: string; large?: boolean }) {
  return (
    <section aria-live="polite" className="px-4 pt-3 pb-3">
      <h1
        className={cn(
          "font-semibold tracking-[-0.01em] text-balance text-heading",
          large ? "text-[2rem] leading-[1.12]" : "text-[1.75rem] leading-[1.15]",
        )}
      >
        {lead}
      </h1>
      <p className="mt-1 text-[1.0625rem] text-pretty text-muted-foreground">{context}</p>
    </section>
  );
}

export function NowScreen({ showStrip }: { showStrip: boolean }) {
  const { ranked, callsBySource, followed, muted, now, text } = useHome();
  const player = usePlayerInstance();
  const activeSourceId = usePlayerSelect((s) => (s.kind === "live" ? s.sourceId : null));
  return (
    <div>
      <Headline lead={text.lead} context={text.context} />
      {showStrip && (
        <div className="px-4 pb-2">
          <Waterfall
            variant="strip"
            sources={ranked}
            activeSourceId={activeSourceId}
            onListen={(s: Source) => void player.listen(s)}
            onAbout={(s: Source) => openSheet({ type: "source", id: s.id })}
            onUnnamed={(frequencyHz) => openSheet({ type: "unnamed", frequencyHz })}
          />
          <p className="mt-1 text-xs text-muted-foreground">Live airwaves. Tap a bright line to listen to it.</p>
        </div>
      )}
      <SourceList sources={ranked} callsBySource={callsBySource} followed={followed} muted={muted} now={now} />
    </div>
  );
}
