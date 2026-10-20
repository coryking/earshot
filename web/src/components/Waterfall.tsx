import { ChannelIdentity } from "@/components/ChannelIdentity";
import { pickSignal, type WaterfallPick } from "@/waterfall/pick";
import { useEffect, useMemo, useRef, useState } from "react";
import { formatMHz } from "@/domain/adapt";
import type { Source } from "@/domain/model";
import { cn } from "@/lib/utils";
import { createRenderer, type Renderer } from "@/waterfall/renderer";
import { onSpectrumRows, spectrumRows, useSpectrumMeta } from "@/waterfall/feed";
import { historyTicks } from "@/waterfall/ticks";

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
  captureRange?: { centerHz: number; spanHz: number } | null;
  className?: string;
}

const HIT_PX = 22;

export function Waterfall({ variant, sources, talking, activeSourceId, tunedFrequencyHz, onListen, onUnnamed, captureRange, className }: Props) {
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

  const interactive = variant === "full" && (!!onListen || !!onUnnamed);
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
  const timeTicks = historyTicks(history.map((row) => row.time), visible, height);

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
      {variant === "full" && timeTicks.map(({ row, ageSeconds }) => (
        <span key={row} aria-hidden className="pointer-events-none absolute left-0 border-t border-white/30 bg-[#0f1a2b]/80 px-1 text-[10px] text-[#c9d4e5]" style={{ top: `${row / visible * 100}%` }}>
          {ageSeconds === 0 ? "Newest" : `−${ageSeconds} s`}
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
        markers.filter(({ source }) => source.id === activeSourceId).map(({ source, x, live }) => (
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
      {captureRange && low !== null && meta.spanHz && (
        <span aria-label="IQ capture range" className="pointer-events-none absolute inset-y-0 border-x border-[#5eb8c9]/90 bg-[#5eb8c9]/15"
          style={{ left: `${Math.max(0, (captureRange.centerHz - captureRange.spanHz / 2 - low) / meta.spanHz) * 100}%`,
            width: `${Math.max(0, Math.min(low + meta.spanHz, captureRange.centerHz + captureRange.spanHz / 2) - Math.max(low, captureRange.centerHz - captureRange.spanHz / 2)) / meta.spanHz * 100}%` }} />
      )}
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
