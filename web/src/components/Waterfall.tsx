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
    const half = Math.min(144, m.source.name.length * 6.2 + 10) / 2;
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
export function Waterfall({ variant, sources, talking, activeSourceId, onListen, onUnnamed, className }: Props) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const box = useRef<HTMLDivElement>(null);
  const meta = useSpectrumMeta();
  // Bumped when the GPU context is restored after the browser dropped it (common on backgrounded phones).
  const [contextEpoch, setContextEpoch] = useState(0);
  const [width, setWidth] = useState(0);
  const [height, setHeight] = useState(0);
  const [hover, setHover] = useState<number | null>(null);
  const visible = variant === "strip" ? 60 : 300;

  useEffect(() => {
    const el = canvas.current;
    if (!el || !meta.bins) return;
    let r: Renderer;
    try {
      r = createRenderer(el, { bins: meta.bins, capacity: 512, visible, smoothing: 0.55 });
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
    return sources
      .filter((s) => s.frequencyHz !== null && s.inRange)
      .map((s) => ({ source: s, x: (s.frequencyHz! - low) / meta.spanHz!, live: talking.has(s.id) }))
      .filter((m) => m.x >= 0 && m.x <= 1);
  }, [sources, talking, low, meta.spanHz]);

  const frequencyAt = (clientX: number) => {
    const rect = box.current?.getBoundingClientRect();
    if (!rect || low === null || !meta.spanHz) return null;
    return low + ((clientX - rect.left) / rect.width) * meta.spanHz;
  };

  const interactive = variant === "full" && !!onListen;
  const pick = (clientX: number) => {
    const rect = box.current?.getBoundingClientRect();
    const frequency = frequencyAt(clientX);
    if (!interactive || !rect || frequency === null) return;
    const px = clientX - rect.left;
    const nearest = markers
      .map((m) => ({ ...m, d: Math.abs(m.x * rect.width - px) }))
      .filter((m) => m.d <= HIT_PX)
      .sort((a, b) => a.d - b.d || Number(b.live) - Number(a.live))[0];
    if (nearest) onListen?.(nearest.source);
    else onUnnamed?.(frequency);
  };

  const empty = !meta.bins;
  const stale = !empty && Date.now() - meta.receivedAt > 10_000;

  return (
    <div
      ref={box}
      className={cn(
        "relative overflow-hidden bg-[#0f1a2b] select-none",
        variant === "strip" ? "h-14 rounded-lg" : "h-full min-h-40 rounded-xl",
        className,
      )}
      onPointerUp={interactive ? (e) => pick(e.clientX) : undefined}
      onPointerMove={interactive ? (e) => setHover(frequencyAt(e.clientX)) : undefined}
      onPointerLeave={interactive ? () => setHover(null) : undefined}
      role="img"
      aria-label={
        empty
          ? "Live airwaves view: no spectrum data from the radio yet."
          : `Live airwaves view, ${formatMHz(low ?? 0, 1)} to ${formatMHz((low ?? 0) + (meta.spanHz ?? 0), 1)} MHz. Use the list to choose a channel.`
      }
      data-waterfall={variant}
    >
      <canvas ref={canvas} className="absolute inset-0 h-full w-full" aria-hidden />
      {empty && (
        <p className="absolute inset-0 grid place-items-center px-4 text-center text-xs text-[#93a1b8]">
          No airwaves picture from the radio yet
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
          variant === "strip" ? 2 : height < 260 ? 2 : 4,
          activeSourceId,
        ).map(({ source, lane, center, live }) => (
          <span
            key={`label-${source.id}`}
            aria-hidden
            className={cn(
              "pointer-events-none absolute max-w-36 -translate-x-1/2 truncate rounded px-1 text-[11px] leading-4",
              source.id === activeSourceId
                ? "bg-[#5eb8c9] text-[#0f1a2b]"
                : live
                  ? "bg-[#f5a524] text-[#0f1a2b]"
                  : "bg-[#0f1a2b]/80 text-[#c9d4e5]",
            )}
            style={{ left: center, top: (variant === "strip" ? 4 : 12) + lane * 18 }}
          >
            {source.name}
          </span>
        ))}
      {variant === "full" && hover !== null && (
        <span className="tabular pointer-events-none absolute bottom-2 left-2 rounded bg-[#0f1a2b]/85 px-1.5 py-0.5 text-xs text-[#f2f5fa]">
          {formatMHz(hover)} MHz
        </span>
      )}
    </div>
  );
}
