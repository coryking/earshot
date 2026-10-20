import { useEffect, useMemo, useState, useSyncExternalStore } from "react";
import { createStore, useStore } from "@/lib/store";

export type Sheet =
  | { type: "source"; id: string }
  | { type: "block" }
  | { type: "alerts" }
  | { type: "status" }
  | { type: "player" }
  | { type: "shortcuts" }
  | null;

export const sheetStore = createStore<Sheet>(null);
export const paletteStore = createStore(false);

export function useSheet() {
  return useStore(sheetStore);
}

export function openSheet(sheet: Sheet) {
  sheetStore.set(sheet);
}

export type Route = "now" | "catch-up" | "radio" | "operator";

function readRoute(): Route {
  const hash = location.hash.replace(/^#\/?/, "");
  return (["now", "catch-up", "radio", "operator"] as const).includes(hash as Route) ? (hash as Route) : "now";
}

function subscribeHash(listener: () => void) {
  window.addEventListener("hashchange", listener);
  return () => window.removeEventListener("hashchange", listener);
}

export function useRoute(): Route {
  return useSyncExternalStore(subscribeHash, readRoute, () => "now");
}

export function navigate(route: Route) {
  location.hash = route === "now" ? "/" : `/${route}`;
}

function subscribeMedia(query: string) {
  return (listener: () => void) => {
    const mq = window.matchMedia?.(query);
    mq?.addEventListener("change", listener);
    return () => mq?.removeEventListener("change", listener);
  };
}

export function useMedia(query: string) {
  const subscribe = useMemo(() => subscribeMedia(query), [query]);
  return useSyncExternalStore(
    subscribe,
    () => window.matchMedia?.(query).matches ?? false,
    () => false,
  );
}

/** Wide screens and phones on their side get the full waterfall. */
export function useWide() {
  const desktop = useMedia("(min-width: 1024px)");
  const landscape = useMedia("(orientation: landscape) and (max-height: 520px)");
  return { desktop, landscape, wide: desktop || landscape };
}

export function useNow(intervalMs = 5000) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(t);
  }, [intervalMs]);
  return now;
}
