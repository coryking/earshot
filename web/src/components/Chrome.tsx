import { Bell, BellRing, Clock3, Command as CommandIcon, Radio, SlidersHorizontal } from "lucide-react";
import { navigate, openSheet, paletteStore, useRoute, type Route } from "@/app/ui";
import { Button } from "@/components/ui/button";
import { ReceiverControls } from "@/components/ReceiverControls";
import { cn } from "@/lib/utils";
import { usePrefs } from "@/state/prefs";
import { useBackend, useHealth, useStatus } from "@/state/radio";

const dot = {
  listening: "bg-ok",
  paused: "bg-muted-foreground",
  offline: "bg-destructive",
} as const;

const healthWord = { listening: "Receiving", paused: "Paused", offline: "Not connected" } as const;

function wordFor(health: ReturnType<typeof useHealth>): string {
  return health.state === "offline" && health.reconnecting ? "Reconnecting" : healthWord[health.state];
}

/** Status dot (tap for status) and the alerts bell. Desktop adds tabs and the palette. The area name is the Now title. */
export function Header({ wide }: { wide: boolean }) {
  const health = useHealth();
  const status = useStatus();
  const stopped = status.data?.receiver?.reason === "operator" && status.data.receiver.state === "stopped";
  const statusWord = stopped ? "Receiver stopped" : wordFor(health);
  const { alertFollowed, alertBusy } = usePrefs();
  const backend = useBackend();
  const alerts = alertFollowed || alertBusy;
  return (
    <header className="sticky top-0 z-30 flex items-center gap-1 border-b border-transparent bg-background/90 px-2 pt-[max(env(safe-area-inset-top),0.5rem)] pb-1 backdrop-blur lg:border-border lg:px-4">
      <button
        type="button"
        onClick={() => openSheet({ type: "status" })}
        className="flex min-h-11 items-center gap-2 rounded-lg px-2"
        aria-label={`Radio status: ${statusWord}`}
      >
        <span className={cn("size-2.5 rounded-full", stopped ? dot.paused : dot[health.state])} aria-hidden />
        <span className="hidden text-sm whitespace-nowrap text-muted-foreground sm:inline">{statusWord}</span>
      </button>
      <ReceiverControls />
      {backend.mode === "demo" && (
        <span className="ml-1 shrink-0 rounded-full border border-signal/50 px-2 py-0.5 text-xs text-signal">Demo</span>
      )}
      <div className="flex-1" />
      {wide && <DesktopTabs />}
      {wide && (
        <Button variant="ghost" size="sm" onClick={() => paletteStore.set(true)} className="gap-2 text-muted-foreground">
          <CommandIcon className="size-4" aria-hidden />
          <span>Search</span>
          <kbd className="rounded border border-border px-1 text-[11px]">⌘K</kbd>
        </Button>
      )}
      <Button
        variant="ghost"
        size="icon"
        className="size-11"
        onClick={() => openSheet({ type: "alerts" })}
        aria-label={alerts ? "Alerts are on" : "Alerts"}
      >
        {alerts ? <BellRing className="size-5 text-lamp" /> : <Bell className="size-5" />}
      </Button>
    </header>
  );
}

const tabs: { route: Route; label: string; icon: typeof Radio }[] = [
  { route: "now", label: "Explore", icon: Radio },
  { route: "catch-up", label: "Catch up", icon: Clock3 },
  { route: "radio", label: "Radio", icon: SlidersHorizontal },
];

function DesktopTabs() {
  const route = useRoute();
  return (
    <nav aria-label="Sections" className="mr-2 flex gap-1">
      {tabs.map((t, i) => (
        <button
          key={t.route}
          type="button"
          onClick={() => navigate(t.route)}
          aria-current={route === t.route ? "page" : undefined}
          className={cn(
            "rounded-lg px-3 py-1.5 text-sm",
            route === t.route ? "bg-secondary text-heading" : "text-muted-foreground hover:text-heading",
          )}
        >
          {t.label}
          <span className="sr-only"> (shortcut {i + 1})</span>
        </button>
      ))}
    </nav>
  );
}

export function TabBar() {
  const route = useRoute();
  return (
    <nav aria-label="Sections" className="flex border-t border-border bg-card pb-safe">
      {tabs.map((t) => (
        <button
          key={t.route}
          type="button"
          onClick={() => navigate(t.route)}
          aria-current={route === t.route ? "page" : undefined}
          className={cn(
            "flex min-h-14 flex-1 flex-col items-center justify-center gap-0.5 text-xs",
            route === t.route ? "text-heading" : "text-muted-foreground",
          )}
        >
          <t.icon className="size-5" aria-hidden />
          {t.label}
        </button>
      ))}
    </nav>
  );
}
