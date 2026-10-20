import { useEffect, useRef } from "react";
import { toast } from "sonner";
import { busyness } from "@/domain/summary";
import { usePrefs } from "@/state/prefs";
import { useActivity, useSources } from "@/state/radio";

function notify(title: string, body: string, tag: string) {
  if (typeof Notification !== "undefined" && Notification.permission === "granted" && document.hidden) {
    try {
      new Notification(title, { body, tag });
      return;
    } catch {
      /* some mobile browsers only allow notifications from a service worker */
    }
  }
  toast(title, { description: body });
}

/**
 * In-page alerts: a followed source starts talking, or the area gets much
 * busier than normal. Runs while the page is open; closed-app push needs the
 * engine (see Frontend needs).
 */
export function useAlerts() {
  const sources = useSources();
  const { activity } = useActivity();
  const { followed, alertFollowed, alertBusy } = usePrefs();
  const talking = useRef(new Set<string>());
  const lastBusy = useRef(0);
  const first = useRef(true);

  useEffect(() => {
    const now = new Set(sources.filter((s) => s.talking).map((s) => s.id));
    if (!first.current && alertFollowed) {
      for (const s of sources)
        if (now.has(s.id) && !talking.current.has(s.id) && followed.includes(s.id))
          notify(`${s.name} is talking`, s.listenable.ok ? "Open the radio to listen." : s.summary, `talk-${s.id}`);
    }
    first.current = false;
    talking.current = now;
  }, [sources, followed, alertFollowed]);

  useEffect(() => {
    if (!alertBusy) return;
    const level = busyness(activity, Date.now()).level;
    if (level === "busier" && Date.now() - lastBusy.current > 30 * 60_000) {
      lastBusy.current = Date.now();
      notify("Busier than usual", "Radio traffic around you has picked up.", "busy");
    }
  }, [activity, alertBusy]);
}
