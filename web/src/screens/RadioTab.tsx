import { Bell, ChevronRight, Keyboard, MapPin, Activity, Wrench } from "lucide-react";
import type { ReactNode } from "react";
import { navigate, openSheet, sheetStore } from "@/app/ui";
import { cn } from "@/lib/utils";
import { prefs, usePrefs, type ThemeChoice } from "@/state/prefs";
import { useBackend, useBlocks, useHealth } from "@/state/radio";

function Row({ icon, title, detail, onClick }: { icon: ReactNode; title: string; detail: string; onClick(): void }) {
  return (
    <li>
      <button type="button" onClick={onClick} className="flex min-h-16 w-full items-center gap-3 px-4 py-2 text-left">
        <span className="text-muted-foreground" aria-hidden>
          {icon}
        </span>
        <span className="min-w-0 flex-1">
          <span className="block text-heading">{title}</span>
          <span className="block truncate text-sm text-muted-foreground">{detail}</span>
        </span>
        <ChevronRight className="size-4 text-muted-foreground" aria-hidden />
      </button>
    </li>
  );
}

export function RadioTab() {
  const health = useHealth();
  const { current } = useBlocks();
  const { theme, alertBusy, alertFollowed } = usePrefs();
  const backend = useBackend();
  return (
    <div className="pb-6">
      <h1 className="px-4 pt-3 pb-2 text-[1.75rem] font-semibold text-heading">Radio</h1>
      <ul className="divide-y divide-border/60">
        <Row icon={<Activity className="size-5" />} title="Is it working?" detail={health.detail} onClick={() => openSheet({ type: "status" })} />
        <Row
          icon={<MapPin className="size-5" />}
          title="Listening area"
          detail={current?.name ?? "Not set"}
          onClick={() => openSheet({ type: "block" })}
        />
        <Row
          icon={<Bell className="size-5" />}
          title="Alerts"
          detail={alertFollowed || alertBusy ? "On" : "Off"}
          onClick={() => openSheet({ type: "alerts" })}
        />
        <Row
          icon={<Keyboard className="size-5" />}
          title="Keyboard shortcuts"
          detail="For computers: press ? anywhere"
          onClick={() => sheetStore.set({ type: "shortcuts" })}
        />
      </ul>
      <fieldset className="mt-4 px-4">
        <legend className="py-2 text-heading">Appearance</legend>
        <div className="flex rounded-lg bg-secondary p-1">
          {(
            [
              ["dark", "Dark"],
              ["light", "Light"],
              ["system", "Match device"],
            ] as [ThemeChoice, string][]
          ).map(([value, label]) => (
            <label
              key={value}
              className={cn(
                "flex min-h-10 flex-1 cursor-pointer items-center justify-center rounded-md text-sm has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-signal",
                theme === value ? "bg-card text-heading shadow-sm" : "text-muted-foreground",
              )}
            >
              <input
                type="radio"
                name="theme"
                value={value}
                checked={theme === value}
                onChange={() => prefs.set("theme", value)}
                className="sr-only"
              />
              {label}
            </label>
          ))}
        </div>
      </fieldset>
      <ul className="mt-6 divide-y divide-border/60 border-t border-border/60">
        <Row
          icon={<Wrench className="size-5" />}
          title="Operator tools"
          detail="Receiver settings and diagnostics, for Cory"
          onClick={() => navigate("operator")}
        />
      </ul>
      {backend.mode === "demo" && (
        <p className="mx-4 mt-4 text-sm text-muted-foreground">
          You're looking at demo data. Names and calls are samples; open the page without <code>?demo</code> for the real
          radio.
        </p>
      )}
    </div>
  );
}
