import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { busyness, type Busyness } from "@/domain/summary";
import { usePrefs } from "@/state/prefs";
import { useActivity, useCalls, useSources } from "@/state/radio";

/** A followed source "gets busy" at this many calls within the window. */
export const BUSY_CALLS = 3;
export const BUSY_WINDOW_MS = 10 * 60_000;
const COOLDOWN_MS = 30 * 60_000;

function notify(title: string, body: string, tag: string) {
  if (typeof Notification !== "undefined" && Notification.permission === "granted" && document.hidden) {
    try {
      new Notification(title, { body, tag });
      return;
    } catch {
      /* some mobile browsers only allow notifications from a service worker; fall back to in-app */
    }
  }
  toast(title, { description: body });
}

/**
 * In-page alerts, per PRODUCT.md: a followed source gets busy, or the whole
 * area spikes above normal. Only activity since the page opened counts, so
 * opening the app never fires a burst of alerts. Closed-app push needs the
 * engine (Frontend needs #8).
 */
export function useAlerts() {
  const sources = useSources();
  const { calls } = useCalls();
  const { activity } = useActivity();
  const { followed, alertFollowed, alertBusy } = usePrefs();
  const [openedAt] = useState(() => Date.now());
  const lastAlert = useRef(new Map<string, number>());
  const lastLevel = useRef<Busyness | null>(null);

  useEffect(() => {
    if (!alertFollowed || !followed.length) return;
    const now = Date.now();
    for (const id of followed) {
      const recent = calls.filter(
        (c) => c.sourceId === id && c.startedAt >= openedAt && now - c.startedAt <= BUSY_WINDOW_MS,
      );
      if (recent.length < BUSY_CALLS) continue;
      if (now - (lastAlert.current.get(id) ?? 0) < COOLDOWN_MS) continue;
      lastAlert.current.set(id, now);
      const name = sources.find((s) => s.id === id)?.name ?? "A channel you follow";
      notify(`${name} is busy`, `${recent.length} calls in the last 10 minutes.`, `busy-${id}`);
    }
  }, [calls, followed, alertFollowed, sources, openedAt]);

  useEffect(() => {
    const level = busyness(activity, Date.now()).level;
    const previous = lastLevel.current;
    lastLevel.current = level;
    if (!alertBusy || previous === null || previous === "busier" || level !== "busier") return;
    const now = Date.now();
    if (now - (lastAlert.current.get("area") ?? 0) < COOLDOWN_MS) return;
    lastAlert.current.set("area", now);
    notify("Busier than usual", "Radio traffic around you has picked up.", "busy");
  }, [activity, alertBusy]);
}
