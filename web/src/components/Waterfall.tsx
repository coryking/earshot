import { ChannelIdentity } from "@/components/ChannelIdentity";
import { pickSignal, type WaterfallPick } from "@/waterfall/pick";
import { useEffect, useMemo, useRef, useState } from "react";
import { formatMHz } from "@/domain/adapt";
import type { Source } from "@/domain/model";
import { cn } from "@/lib/utils";
import { createRenderer, type Renderer } from "@/waterfall/renderer";
import { onSpectrumRows, spectrumRows, useSpectrumMeta } from "@/waterfall/feed";

interface Props {
  variant: "strip" | "full";
  /** Sources to mark. The strip gets only who's talking and what's playing. */
  sources: Source[];
  /** Smoothed talking state from the home model (domain/calm.ts). */
  talking: ReadonlyMap<string, number>;
  activeSourceId: string | null;
  tunedFrequencyHz?: number | null;
  /** Full view only: tapping a marked trace listens to it. */
  onListen?(source: Source): void;
  /** Full view only: tapped a trace with no source nearby. */
  onUnnamed?(frequencyHz: number): void;
  className?: string;
}

const HIT_PX = 22;

/**
 * Greedy lane assignment so marker labels don't overlap. Talking and active
 * sources claim lanes first; labels that don't fit in `maxLanes` are dropped
 * (the marker tick stays). Returns pixel centres clamped inside the view.
 */
function stackLabels<T extends { source: Source; x: number; live: boolean }>(
  markers: T[],
  width: number,
  maxLanes: number,
  activeId: string | null,
) {
  const lanes: [number, number][][] = [];
  const rank = (m: T) => (m.live ? 2 : 0) + (m.source.id === activeId ? 1 : 0);
  const ordered = [...markers].sort((a, b) => rank(b) - rank(a) || a.x - b.x);
  return ordered.flatMap((m) => {
    const half = 192 / 2;
    const center = Math.min(width - half - 4, Math.max(half + 4, m.x * width));
    const span: [number, number] = [center - half - 3, center + half + 3];
    let lane = lanes.findIndex((spans) => spans.every(([a, b]) => span[1] < a || span[0] > b));
    if (lane < 0) {
      if (lanes.length >= maxLanes) return [];
      lane = lanes.push([]) - 1;
    }
    lanes[lane].push(span);
    return [{ ...m, lane, center }];
  });
}

/**
 * Live spectrum history rendered in the browser.
 *
 * Phone strip: a picture, not a control. Its channels are a few pixels apart,
 * and most bright lines on this receiver are data, control channels or noise
 * that can't be listened to, so "tap a bright line" sent people to things they
 * couldn't hear. The strip instead labels the traces of whoever is talking and
 * whatever is playing, tying it to the list, which is the way in.
 *
 * Full view (desktop, landscape): history, a frequency readout, labelled
 * markers, and click-to-listen on a marked trace, where a pointer is precise.
 */
export function Waterfall({ variant, sources, talking, activeSourceId, tunedFrequencyHz, onListen, onUnnamed, className }: Props) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const drag = useRef<{ pointerId: number; lastTune: number; frequency: number | null } | null>(null);
  const box = useRef<HTMLDivElement>(null);
  const meta = useSpectrumMeta();
  // Bumped when the GPU context is restored after the browser dropped it (common on backgrounded phones).
  const [contextEpoch, setContextEpoch] = useState(0);
  const [width, setWidth] = useState(0);
  const [height, setHeight] = useState(0);
  const [hover, setHover] = useState<WaterfallPick | null>(null);
  const visible = variant === "strip" ? 60 : 300;

  useEffect(() => {
    const el = canvas.current;
    if (!el || !meta.bins) return;
    let r: Renderer;
    try {
      r = createRenderer(el, { bins: meta.bins, capacity: 512, visible, smoothing: 1 });
    } catch {
      return;
    }
    spectrumRows.slice(-visible).forEach((row) => r.push(row.bins));
    let frame = 0;
    let dirty = true;
    const draw = () => {
      if (dirty) {
        r.draw();
        dirty = false;
      }
      frame = requestAnimationFrame(draw);
    };
    const resize = () => {
      const rect = el.getBoundingClientRect();
      const dpr = Math.min(2, window.devicePixelRatio || 1);
      r.resize(Math.max(1, Math.round(rect.width * dpr)), Math.max(1, Math.round(rect.height * dpr)));
      setWidth(rect.width);
      setHeight(rect.height);
      dirty = true;
    };
    resize();
    const observer = new ResizeObserver(resize);
    observer.observe(el);
    const unsubscribe = onSpectrumRows((rows) => {
      rows.forEach((row) => r.push(row.bins));
      dirty = true;
    });
    frame = requestAnimationFrame(draw);
    const lost = (event: Event) => {
      event.preventDefault();
      cancelAnimationFrame(frame);
    };
    const restored = () => setContextEpoch((n) => n + 1);
    el.addEventListener("webglcontextlost", lost);
    el.addEventListener("webglcontextrestored", restored);
    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
      unsubscribe();
      el.removeEventListener("webglcontextlost", lost);
      el.removeEventListener("webglcontextrestored", restored);
      r.dispose();
    };
  }, [meta.bins, meta.epoch, visible, contextEpoch]);

  const low = meta.centerHz !== null && meta.spanHz !== null ? meta.centerHz - meta.spanHz / 2 : null;
  const markers = useMemo(() => {
    if (low === null || !meta.spanHz) return [];
    // Both TDMA slots and raw/unknown aliases occupy the same RF trace.
    const unique = new Map<number, Source>();
    const rank = (s: Source) => Number(s.id === activeSourceId) * 8 + Number(talking.has(s.id)) * 4 + Number(s.named) * 2 + Number(s.heardVoice);
    for (const source of sources) {
      if (source.frequencyHz === null || !source.inRange) continue;
      const hz = Math.round(source.frequencyHz);
      const existing = unique.get(hz);
      if (!existing || rank(source) > rank(existing)) unique.set(hz, source);
    }
    return [...unique.values()]
      .map((s) => ({ source: s, x: (s.frequencyHz! - low) / meta.spanHz!, live: talking.has(s.id) }))
      .filter((m) => m.x >= 0 && m.x <= 1);
  }, [sources, talking, low, meta.spanHz, activeSourceId]);

  const frequencyAt = (clientX: number) => {
    const rect = box.current?.getBoundingClientRect();
    if (!rect || low === null || !meta.spanHz) return null;
    return low + Math.max(0, Math.min(1, (clientX - rect.left) / rect.width)) * meta.spanHz;
  };

  const interactive = variant === "full" && !!onListen;
  const targetAt = (clientX: number, clientY: number, exact: boolean) => {
    const rect = box.current?.getBoundingClientRect();
    const frequency = frequencyAt(clientX);
    if (!rect || frequency === null || low === null || !meta.spanHz) return null;
    const row = Math.floor(Math.max(0, Math.min(0.999, (clientY - rect.top) / rect.height)) * visible);
    const bins = spectrumRows[spectrumRows.length - 1 - row]?.bins;
    return pickSignal(frequency, HIT_PX / rect.width * meta.spanHz, low, meta.spanHz, bins, sources, exact);
  };
  const pick = (clientX: number, clientY: number, exact: boolean) => {
    const target = targetAt(clientX, clientY, exact);
    if (!target) return;
    if (drag.current?.frequency === target.frequencyHz) return;
    if (drag.current) { drag.current.frequency = target.frequencyHz; drag.current.lastTune = performance.now(); }
    if (target.source) onListen?.(target.source);
    else onUnnamed?.(target.frequencyHz);
  };

  const empty = !meta.bins;
  const stale = !empty && Date.now() - meta.receivedAt > 10_000;

  const tickCount = Math.max(2, Math.floor(width / 110));
  const rawStep = (meta.spanHz ?? 1) / tickCount;
  const magnitude = 10 ** Math.floor(Math.log10(rawStep));
  const tickStep = [1, 2, 5, 10].find((n) => n * magnitude >= rawStep)! * magnitude;
  const frequencyTicks: number[] = [];
  if (low !== null && meta.spanHz) {
    for (let hz = Math.ceil(low / tickStep) * tickStep; hz <= low + meta.spanHz; hz += tickStep)
      frequencyTicks.push(hz);
  }
  const history = spectrumRows.slice(-visible).reverse();
  const timeTicks = Array.from({ length: Math.max(2, Math.floor(height / 90)) }, (_, i) => {
    const row = Math.floor(i * visible / Math.max(2, Math.floor(height / 90)));
    const age = history[row] ? history[0].time - history[row].time : null;
    return { row, age };
  }).filter((tick) => tick.age !== null);

  return (
    <div className={cn("flex flex-col overflow-hidden bg-[#0f1a2b]", variant === "strip" ? "h-14 rounded-lg" : "h-full min-h-40 rounded-xl", className)}>
      {variant === "full" && low !== null && meta.spanHz && (
        <div aria-label="Frequency scale (MHz)" className="relative h-7 shrink-0 border-b border-[#93a1b8]/30 text-[11px] text-[#c9d4e5]">
          {frequencyTicks.map((hz) => (
            <span key={hz} className="absolute bottom-0 h-2 border-l border-[#93a1b8]/60" style={{ left: `${(hz - low) / meta.spanHz! * 100}%` }}>
              <span className="absolute bottom-2 -translate-x-1/2 whitespace-nowrap">{(hz / 1e6).toFixed(Math.max(0, Math.ceil(6 - Math.log10(tickStep))))}</span>
            </span>
          ))}
          <span className="absolute top-0 right-1 bg-[#0f1a2b] px-1">MHz</span>
        </div>
      )}
    <div
      ref={box}
      className={cn(
        "relative overflow-hidden bg-[#0f1a2b] select-none",
        "min-h-0 flex-1", interactive && "touch-none",
      )}
      onPointerDown={interactive ? (e) => {
        if (e.button !== 0) return;
        e.preventDefault();
        e.currentTarget.setPointerCapture(e.pointerId);
        drag.current = { pointerId: e.pointerId, lastTune: 0, frequency: null };
        setHover(targetAt(e.clientX, e.clientY, e.shiftKey));
        pick(e.clientX, e.clientY, e.shiftKey);
      } : undefined}
      onPointerMove={interactive ? (e) => {
        setHover(targetAt(e.clientX, e.clientY, e.shiftKey));
        if (drag.current?.pointerId === e.pointerId && performance.now() - drag.current.lastTune >= 120)
          pick(e.clientX, e.clientY, e.shiftKey);
      } : undefined}
      onPointerUp={interactive ? (e) => {
        if (drag.current?.pointerId !== e.pointerId) return;
        pick(e.clientX, e.clientY, e.shiftKey);
        drag.current = null;
        if (e.currentTarget.hasPointerCapture(e.pointerId)) e.currentTarget.releasePointerCapture(e.pointerId);
      } : undefined}
      onPointerCancel={interactive ? () => { drag.current = null; setHover(null); } : undefined}
      onLostPointerCapture={interactive ? () => { drag.current = null; } : undefined}
      onPointerLeave={interactive ? () => { if (!drag.current) setHover(null); } : undefined}
      role="img"
      aria-label={
        empty
          ? "Live airwaves: no data yet"
          : `Live airwaves, ${formatMHz(low ?? 0, 1)} to ${formatMHz((low ?? 0) + (meta.spanHz ?? 0), 1)} MHz`
      }
      data-waterfall={variant}
    >
      <canvas ref={canvas} className="absolute inset-0 h-full w-full" aria-hidden />
      {variant === "full" && timeTicks.map(({ row, age }) => (
        <span key={row} aria-hidden className="pointer-events-none absolute left-0 border-t border-white/30 bg-[#0f1a2b]/80 px-1 text-[10px] text-[#c9d4e5]" style={{ top: `${row / visible * 100}%` }}>
          {row === 0 ? "Newest" : `−${age!.toFixed(1)} s`}
        </span>
      ))}
      {variant === "full" && meta.bins > 0 && meta.spanHz && (
        <span className="pointer-events-none absolute bottom-1 right-1 rounded bg-[#0f1a2b]/85 px-1 text-[10px] text-[#c9d4e5]">{(meta.spanHz / meta.bins / 1000).toFixed(2)} kHz/bin</span>
      )}
      {empty && (
        <p className="absolute inset-0 grid place-items-center px-4 text-center text-xs text-[#93a1b8]">
          No signal
        </p>
      )}
      {stale && (
        <p className="absolute right-2 bottom-1 rounded bg-[#0f1a2b]/80 px-1.5 text-[11px] text-[#93a1b8]">Paused</p>
      )}
      {width > 0 &&
        markers.map(({ source, x, live }) => (
          <span
            key={source.id}
            aria-hidden
            className={cn(
              "pointer-events-none absolute w-0.5 -translate-x-1/2 rounded-full",
              // On the strip, a marked trace gets a full-height guide line so the label points at it.
              variant === "strip" ? "inset-y-0 opacity-80" : "top-0 h-2.5",
              live ? "bg-[#f5a524]" : "bg-[#93a1b8]/60",
              source.id === activeSourceId && "w-1 bg-[#5eb8c9]",
            )}
            style={{ left: `${x * 100}%` }}
          />
        ))}
      {width > 0 &&
        stackLabels(
          variant === "strip"
            ? markers
            : markers.filter((m) => m.live || m.source.id === activeSourceId || m.source.named),
          width,
          variant === "strip" ? 1 : height < 260 ? 2 : 4,
          activeSourceId,
        ).map(({ source, lane, center, live }) => (
          <span
            key={`label-${source.id}`}
            aria-hidden
            className={cn(
              "pointer-events-none absolute w-48 -translate-x-1/2 truncate rounded px-1 text-[11px] leading-4 [&_.text-muted-foreground]:text-inherit",
              source.id === activeSourceId
                ? "bg-[#5eb8c9] text-[#0f1a2b]"
                : live
                  ? "bg-[#f5a524] text-[#0f1a2b]"
                  : "bg-[#0f1a2b]/80 text-[#c9d4e5]",
            )}
            style={{ left: center, top: (variant === "strip" ? 4 : 12) + lane * 34 }}
          >
            <ChannelIdentity {...source} compact />
          </span>
        ))}
      {variant === "full" && tunedFrequencyHz != null && low !== null && meta.spanHz && tunedFrequencyHz >= low && tunedFrequencyHz <= low + meta.spanHz && (
        <span aria-hidden className="pointer-events-none absolute inset-y-0 w-0.5 bg-[#5eb8c9]" style={{ left: `${(tunedFrequencyHz - low) / meta.spanHz * 100}%` }} />
      )}
      {variant === "full" && hover !== null && low !== null && meta.spanHz && (
        <span aria-hidden className="pointer-events-none absolute inset-y-0 w-px bg-white/70" style={{ left: `${(hover.frequencyHz - low) / meta.spanHz * 100}%` }} />
      )}
      {variant === "full" && hover !== null && (
        <span className="tabular pointer-events-none absolute bottom-2 left-2 rounded bg-[#0f1a2b]/85 px-1.5 py-0.5 text-xs text-[#f2f5fa]">
          {hover.snapped ? "Snap to " : "Tune "}<ChannelIdentity name={hover.source?.name} frequencyHz={hover.frequencyHz} mode={hover.source?.mode} slot={hover.source?.slot} compact />
        </span>
      )}
    </div>
    </div>
  );
}
