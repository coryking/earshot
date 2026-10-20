import { useEffect } from "react";
import type { SpectrumRow } from "@/domain/model";
import { createStore, useStore } from "@/lib/store";
import { spectrumAnnounce, useBackend } from "@/state/radio";

export interface SpectrumMeta {
  centerHz: number | null;
  spanHz: number | null;
  unit: string;
  bins: number;
  /** Engine generation of the rows on screen; -1 before the first batch. */
  generation: number;
  /** Bumped whenever history is thrown away (retune, restart); renderers start over. */
  epoch: number;
  /** Wall clock of the newest row, for "is it alive". */
  receivedAt: number;
}

const MAX_ROWS = 600;

/**
 * Recent rows, newest last. Kept outside React (and outside TanStack Query) on
 * purpose: it's a ring buffer fed incrementally by `after=` cursors and read by
 * a WebGL renderer, not cacheable server state.
 */
export const spectrumRows: SpectrumRow[] = [];
export const spectrumMeta = createStore<SpectrumMeta>({
  centerHz: null,
  spanHz: null,
  unit: "",
  bins: 0,
  generation: -1,
  epoch: 0,
  receivedAt: 0,
});
const listeners = new Set<(rows: SpectrumRow[]) => void>();

export function onSpectrumRows(listener: (rows: SpectrumRow[]) => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useSpectrumMeta() {
  return useStore(spectrumMeta);
}

/**
 * Polls new rows when the engine announces them (and every 2 s regardless)
 * while the page is visible; a locked phone doesn't draw a waterfall. Mount once.
 */
export function useSpectrumFeed() {
  const backend = useBackend();
  useEffect(() => {
    let after = 0;
    let busy = false;
    let again = false;
    let disposed = false;
    const visible = () => document.visibilityState !== "hidden";
    const pull = async () => {
      if (disposed || !visible()) return;
      if (busy) {
        again = true;
        return;
      }
      busy = true;
      try {
        const batch = await backend.spectrum(after);
        if (disposed) return;
        const meta = spectrumMeta.get();
        // The engine's sequence going backwards means it restarted; its rows
        // from before are gone and `after` must start over.
        if (batch.sequence < after) {
          after = 0;
          spectrumRows.length = 0;
          spectrumMeta.set({ ...meta, epoch: meta.epoch + 1 });
          again = true;
          return;
        }
        // A new generation, center or span is a different band: drop the old picture.
        const retuned =
          meta.generation !== -1 &&
          (batch.generation !== meta.generation || batch.centerHz !== meta.centerHz || batch.spanHz !== meta.spanHz);
        if (retuned) spectrumRows.length = 0;
        const fresh = batch.rows.filter((r) => r.sequence > after);
        if (fresh.length) {
          after = fresh.at(-1)!.sequence;
          spectrumRows.push(...fresh);
          if (spectrumRows.length > MAX_ROWS) spectrumRows.splice(0, spectrumRows.length - MAX_ROWS);
        }
        const bins = fresh[0]?.bins.length ?? meta.bins;
        if (retuned || fresh.length || bins !== meta.bins || meta.generation === -1)
          spectrumMeta.set({
            centerHz: batch.centerHz,
            spanHz: batch.spanHz,
            unit: batch.unit,
            bins,
            generation: batch.generation,
            epoch: retuned || bins !== meta.bins ? meta.epoch + 1 : meta.epoch,
            receivedAt: fresh.length ? Date.now() : meta.receivedAt,
          });
        // After a reset the renderer rebuilds from spectrumRows; otherwise stream the new rows.
        if (fresh.length && !retuned && bins === meta.bins) listeners.forEach((l) => l(fresh));
      } catch {
        /* the status line reports connection trouble; keep polling */
      } finally {
        busy = false;
        if (again && !disposed) {
          again = false;
          void pull();
        }
      }
    };
    let last = 0;
    let pending: ReturnType<typeof setTimeout> | undefined;
    const throttled = () => {
      const wait = Math.max(0, 200 - (performance.now() - last));
      clearTimeout(pending);
      pending = setTimeout(() => {
        last = performance.now();
        void pull();
      }, wait);
    };
    void pull();
    const unsubscribe = spectrumAnnounce.subscribe(throttled);
    const timer = setInterval(() => void pull(), 2000);
    const onVisibility = () => {
      if (visible()) void pull();
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      disposed = true;
      unsubscribe();
      clearInterval(timer);
      clearTimeout(pending);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [backend]);
}
