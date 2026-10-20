import { useEffect } from "react";
import { usePlayerInstance } from "@/app/player-context";
import {
  navigate,
  openSheet,
  paletteStore,
  sheetStore,
  useRoute,
  type Route,
} from "@/app/ui";
import {
  Command,
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandShortcut,
} from "@/components/ui/command";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Kbd } from "@/components/ui/kbd";
import { useStore } from "@/lib/store";
import { prefs, prefsStore } from "@/state/prefs";
import { useSheet } from "@/app/ui";
import { useSources } from "@/state/radio";

export const shortcuts: [string, string][] = [
  ["⌘K, Ctrl+K or /", "Search"],
  ["j / k or ↓ / ↑", "Move"],
  ["Enter or Space", "Listen / stop"],
  ["Esc", "Stop or close"],
  ["m", "Mute or unmute"],
  ["f", "Follow"],
  ["1 2 3", "Now, Catch up, Radio"],
  ["?", "Show these shortcuts"],
];

export function Palette() {
  const open = useStore(paletteStore);
  const sources = useSources();
  const player = usePlayerInstance();
  const run = (fn: () => void) => () => {
    paletteStore.set(false);
    fn();
  };
  return (
    <CommandDialog
      open={open}
      onOpenChange={paletteStore.set}
      title="Search"
      description="Find a channel or an action"
    >
      <Command>
        <CommandInput placeholder="Channel or action…" />
        <CommandList>
          <CommandEmpty>Nothing matches.</CommandEmpty>
          <CommandGroup heading="Listen">
            {sources
              .filter((s) => s.listenable.ok)
              .map((s) => (
                <CommandItem
                  key={s.id}
                  value={`listen ${s.name} ${s.summary}`}
                  onSelect={run(() => void player.listen(s))}
                >
                  {s.name}
                  {s.talking && <CommandShortcut>talking</CommandShortcut>}
                </CommandItem>
              ))}
          </CommandGroup>
          <CommandGroup heading="About">
            {sources.map((s) => (
              <CommandItem
                key={s.id}
                value={`about ${s.name}`}
                onSelect={run(() => openSheet({ type: "source", id: s.id }))}
              >
                What is {s.name}?
              </CommandItem>
            ))}
          </CommandGroup>
          <CommandGroup heading="Actions">
            <CommandItem onSelect={run(() => void player.stop())}>
              Stop listening <CommandShortcut>Esc</CommandShortcut>
            </CommandItem>
            <CommandItem onSelect={run(() => player.toggleMute())}>
              Mute or unmute <CommandShortcut>m</CommandShortcut>
            </CommandItem>
            <CommandItem onSelect={run(() => navigate("catch-up"))}>
              Catch up on recent calls <CommandShortcut>2</CommandShortcut>
            </CommandItem>
            <CommandItem onSelect={run(() => openSheet({ type: "block" }))}>
              Change listening area
            </CommandItem>
            <CommandItem onSelect={run(() => openSheet({ type: "alerts" }))}>
              Alerts
            </CommandItem>
            <CommandItem onSelect={run(() => openSheet({ type: "status" }))}>
              Is it working?
            </CommandItem>
            <CommandItem
              onSelect={run(() =>
                prefs.set(
                  "theme",
                  prefsStore.get().theme === "light" ? "dark" : "light",
                ),
              )}
            >
              Switch light or dark
            </CommandItem>
            <CommandItem onSelect={run(() => navigate("operator"))}>
              Operator tools
            </CommandItem>
          </CommandGroup>
        </CommandList>
      </Command>
    </CommandDialog>
  );
}

export function ShortcutsDialog() {
  const sheet = useSheet();
  return (
    <Dialog
      open={sheet?.type === "shortcuts"}
      onOpenChange={(o) => !o && sheetStore.set(null)}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Keyboard shortcuts</DialogTitle>
          <DialogDescription>For computers with a keyboard.</DialogDescription>
        </DialogHeader>
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-2 text-sm">
          {shortcuts.map(([keys, what]) => (
            <div key={keys} className="contents">
              <dt>
                <Kbd>{keys}</Kbd>
              </dt>
              <dd>{what}</dd>
            </div>
          ))}
        </dl>
      </DialogContent>
    </Dialog>
  );
}

function rows(): HTMLButtonElement[] {
  return Array.from(
    document.querySelectorAll<HTMLButtonElement>(
      "[data-source-list] [data-source-row]",
    ),
  );
}

/** Global keyboard shortcuts. Ignored while typing or when a dialog is open. */
export function useShortcuts() {
  const player = usePlayerInstance();
  const route = useRoute();
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        paletteStore.set(!paletteStore.get());
        return;
      }
      const target = e.target as HTMLElement | null;
      if (
        target?.closest(
          "input, textarea, select, [contenteditable], [role=dialog]",
        )
      )
        return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const list = rows();
      const index = list.findIndex((el) => el === document.activeElement);
      const focusedId = () =>
        (
          document.activeElement?.closest(
            "[data-source-id]",
          ) as HTMLElement | null
        )?.dataset.sourceId ?? null;
      const tabs: Record<string, Route> = {
        "1": "now",
        "2": "catch-up",
        "3": "radio",
      };
      switch (e.key) {
        case "/":
          e.preventDefault();
          paletteStore.set(true);
          break;
        case "?":
          sheetStore.set({ type: "shortcuts" });
          break;
        case "j":
        case "ArrowDown":
          if (route !== "now" || !list.length) return;
          e.preventDefault();
          list[Math.min(list.length - 1, index + 1)]?.focus();
          break;
        case "k":
        case "ArrowUp":
          if (route !== "now" || !list.length) return;
          e.preventDefault();
          list[Math.max(0, index - 1)]?.focus();
          break;
        case "Escape":
          if (sheetStore.get() || paletteStore.get()) return;
          void player.stop();
          break;
        case "m":
          player.toggleMute();
          break;
        case "f": {
          const id = focusedId() ?? player.state.get().sourceId;
          if (id) prefs.toggleFollow(id);
          break;
        }
        default:
          if (tabs[e.key]) navigate(tabs[e.key]);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [player, route]);
}
