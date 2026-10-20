import { useEffect } from "react";
import type { SpectrumRow } from "@/domain/model";
import { createStore, useStore } from "@/lib/store";
import { connectionStore, useBackend } from "@/state/radio";

export interface SpectrumMeta {
  centerHz: number | null;
  spanHz: number | null;
  unit: string;
  bins: number;
  /** Sequence of the newest row. */
  sequence: number;
  /** Wall clock of the newest row, for "is it alive". */
  receivedAt: number;
}

const MAX_ROWS = 600;

/** Recent rows, newest last. Kept outside React so remounts don't lose history. */
export const spectrumRows: SpectrumRow[] = [];
export const spectrumMeta = createStore<SpectrumMeta>({
  centerHz: null,
  spanHz: null,
  unit: "",
  bins: 0,
  sequence: 0,
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

/** Polls new rows when the engine announces them (and every 2 s regardless). Mount once. */
export function useSpectrumFeed() {
  const backend = useBackend();
  useEffect(() => {
    let after = 0;
    let busy = false;
    let disposed = false;
    const pull = async () => {
      if (busy || disposed) return;
      busy = true;
      try {
        const batch = await backend.spectrum(after);
        if (disposed) return;
        // A sequence going backwards means the engine restarted or retuned.
        if (batch.sequence < after) {
          after = 0;
          spectrumRows.length = 0;
        }
        const fresh = batch.rows.filter((r) => r.sequence > after);
        if (fresh.length) {
          after = fresh.at(-1)!.sequence;
          spectrumRows.push(...fresh);
          if (spectrumRows.length > MAX_ROWS) spectrumRows.splice(0, spectrumRows.length - MAX_ROWS);
          listeners.forEach((l) => l(fresh));
        }
        spectrumMeta.set((m) => ({
          centerHz: batch.centerHz,
          spanHz: batch.spanHz,
          unit: batch.unit,
          bins: fresh[0]?.bins.length ?? m.bins,
          sequence: after,
          receivedAt: fresh.length ? Date.now() : m.receivedAt,
        }));
      } catch {
        /* the status line reports connection trouble; keep polling */
      } finally {
        busy = false;
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
    const unsubscribe = connectionStore.subscribe(throttled);
    const timer = setInterval(() => void pull(), 2000);
    return () => {
      disposed = true;
      unsubscribe();
      clearInterval(timer);
      clearTimeout(pending);
    };
  }, [backend]);
}
