import { useMutation, useQueryClient } from "@tanstack/react-query";
import { BellRing, ChevronRight, Lock, Pause, Play, Star, VolumeX } from "lucide-react";
import { useState, type ReactNode } from "react";
import { toast } from "sonner";
import type { WireCatalogEntry } from "@/api/needs";
import { usePlayer, usePlayerInstance, usePlayerSelect } from "@/app/player-context";
import { navigate, openSheet, sheetStore, useNow, useSheet } from "@/app/ui";
import { LevelMeter } from "@/components/bits";
import { playerLine } from "@/components/PlayerBar";
import { Button } from "@/components/ui/button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { Drawer, DrawerContent, DrawerDescription, DrawerHeader, DrawerTitle } from "@/components/ui/drawer";
import { Input } from "@/components/ui/input";
import { Slider } from "@/components/ui/slider";
import { Switch } from "@/components/ui/switch";
import { formatMHz } from "@/domain/adapt";
import type { Call, Source } from "@/domain/model";
import { relativeTime } from "@/domain/summary";
import { cn } from "@/lib/utils";
import { prefs, usePrefs } from "@/state/prefs";
import { keys, localCatalog, useBackend, useBlocks, useCalls, useHealth, useSources, useStatus } from "@/state/radio";

function Sheet({
  open,
  title,
  description,
  children,
}: {
  open: boolean;
  title: string;
  description?: ReactNode;
  children: ReactNode;
}) {
  return (
    <Drawer open={open} onOpenChange={(o) => !o && sheetStore.set(null)}>
      <DrawerContent className="mx-auto max-h-[88dvh] w-full max-w-lg [&[data-vaul-drawer]]:bg-card">
        <DrawerHeader className="text-left group-data-[vaul-drawer-direction=bottom]/drawer-content:text-left">
          <DrawerTitle className="text-xl text-heading">{title}</DrawerTitle>
          {description ? (
            <DrawerDescription className="text-base text-muted-foreground">{description}</DrawerDescription>
          ) : (
            <DrawerDescription className="sr-only">{title}</DrawerDescription>
          )}
        </DrawerHeader>
        <div className="overflow-y-auto px-4 pb-[max(env(safe-area-inset-bottom),1.25rem)]">{children}</div>
      </DrawerContent>
    </Drawer>
  );
}

export function SheetHost() {
  const sheet = useSheet();
  return (
    <>
      <SourceSheet id={sheet?.type === "source" ? sheet.id : null} />
      <BlockSheet open={sheet?.type === "block"} />
      <AlertsSheet open={sheet?.type === "alerts"} />
      <StatusSheet open={sheet?.type === "status"} />
      <PlayerSheet open={sheet?.type === "player"} />
      <UnnamedSheet frequencyHz={sheet?.type === "unnamed" ? sheet.frequencyHz : null} />
    </>
  );
}

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="py-2">
      <dt className="text-sm text-muted-foreground">{label}</dt>
      <dd className="text-foreground">{children}</dd>
    </div>
  );
}

const modeNames = { dmr: "DMR", nxdn: "NXDN", p25: "P25", fm: "Analog FM", unknown: "Not identified" } as const;

/**
 * Save a name. `previousId` is the source's id before it had this entry; follow,
 * mute and an open sheet move to the new id so naming something doesn't lose them.
 */
export function useSaveLabel() {
  const backend = useBackend();
  const client = useQueryClient();
  return useMutation({
    mutationFn: async ({ entry }: { entry: WireCatalogEntry; previousId?: string }) => {
      const saved = await backend.saveCatalogEntry(entry);
      if (!saved) localCatalog.set((list) => [...list.filter((e) => e.id !== entry.id), entry]);
      return saved;
    },
    onSuccess: (saved, { entry, previousId }) => {
      if (previousId && previousId !== entry.id) {
        prefs.renameSource(previousId, entry.id);
        const sheet = sheetStore.get();
        if (sheet?.type === "source" && sheet.id === previousId) sheetStore.set({ type: "source", id: entry.id });
      }
      void client.invalidateQueries({ queryKey: keys.catalog });
      toast.success(saved ? "Saved" : "Saved on this device");
    },
    onError: () => toast.error("Couldn't save"),
  });
}

/** A label keyed by frequency (and DMR line), so it still applies after the engine identifies the signal. */
function entryFor(source: Source, name: string): WireCatalogEntry {
  return {
    id: source.named ? source.id : `label-${source.id}`,
    kind: source.kind,
    channel_ids: [],
    frequency_hz: source.frequencyHz,
    slot: source.mode === "dmr" ? source.slot : null,
    system: source.system,
    talkgroup_id: source.talkgroupId,
    name,
    description: source.description,
    probable_user: source.probableUser,
    traffic_kind: source.trafficKind,
    mode: source.mode,
    encryption: source.encryption,
  };
}

function NameIt({ source }: { source: Source }) {
  const [name, setName] = useState("");
  const save = useSaveLabel();
  return (
    <form
      className="mt-3 flex gap-2"
      onSubmit={(e) => {
        e.preventDefault();
        if (name.trim()) save.mutate({ entry: entryFor(source, name.trim()), previousId: source.id });
      }}
    >
      <label htmlFor="name-it" className="sr-only">
        Name for {source.name}
      </label>
      <Input
        id="name-it"
        value={name}
        onChange={(e) => setName(e.target.value)}
        placeholder="Name"
        className="h-11"
      />
      <Button type="submit" className="h-11" disabled={!name.trim() || save.isPending}>
        Save
      </Button>
    </form>
  );
}

function encryptionText(source: Source): string | null {
  switch (source.encryption) {
    case "encrypted":
      return "Encrypted: only their own radios can hear it";
    case "mixed":
      return "Some calls encrypted";
    case "clear":
      return "No";
    default:
      return null;
  }
}

function SourceSheet({ id }: { id: string | null }) {
  const sources = useSources();
  const source = sources.find((s) => s.id === id) ?? null;
  const [player, state] = usePlayer();
  const { followed, muted } = usePrefs();
  const { calls } = useCalls();
  if (!source) return <Sheet open={false} title="">{null}</Sheet>;
  const playing = state.kind === "live" && state.sourceId === source.id;
  const mine = calls.filter((c) => c.sourceId === source.id).slice(0, 5);
  const isFollowed = followed.includes(source.id);
  const isMuted = muted.includes(source.id);
  return (
    <Sheet open={!!id} title={source.name} description={source.summary}>
      <div className="flex flex-wrap gap-2">
        {source.listenable.ok ? (
          <Button size="lg" className="h-12 gap-2" onClick={() => void player.listen(source)}>
            {playing ? <Pause className="size-4" /> : <Play className="size-4 fill-current" />}
            {playing ? "Stop listening" : "Listen"}
          </Button>
        ) : (
          <p className="w-full rounded-lg bg-secondary p-3 text-sm text-secondary-foreground">
            {source.encryption === "encrypted" && <Lock className="mr-1 inline size-4" aria-hidden />}
            {source.listenable.reason}
          </p>
        )}
        <Button
          variant={isFollowed ? "secondary" : "outline"}
          size="lg"
          className="h-12 gap-2"
          onClick={() => prefs.toggleFollow(source.id)}
          aria-pressed={isFollowed}
        >
          <Star className={cn("size-4", isFollowed && "fill-lamp text-lamp")} />
          {isFollowed ? "Following" : "Follow"}
        </Button>
        <Button
          variant={isMuted ? "secondary" : "outline"}
          size="lg"
          className="h-12 gap-2"
          onClick={() => prefs.toggleMute(source.id)}
          aria-pressed={isMuted}
        >
          <VolumeX className="size-4" />
          {isMuted ? "Muted" : "Mute"}
        </Button>
      </div>

      <dl className="mt-4 divide-y divide-border/60">
        {source.description && source.description !== source.summary && <Fact label="About">{source.description}</Fact>}
        {source.probableUser && source.probableUser !== source.name && <Fact label="Used by">{source.probableUser}</Fact>}
        {source.trafficKind && <Fact label="Traffic">{source.trafficKind}</Fact>}
        {encryptionText(source) && <Fact label="Encrypted">{encryptionText(source)}</Fact>}
      </dl>
      {!source.named && <NameIt source={source} />}

      {mine.length > 0 && (
        <>
          <h3 className="mt-6 text-base font-semibold text-heading">Recent calls</h3>
          <ul className="mt-1">
            {mine.map((c) => (
              <CallItem key={c.id} call={c} name={source.name} compact />
            ))}
          </ul>
        </>
      )}

      <Collapsible className="mt-6 mb-2">
        <CollapsibleTrigger className="flex min-h-11 items-center gap-1 text-sm text-muted-foreground">
          <ChevronRight className="size-4" aria-hidden /> Details
        </CollapsibleTrigger>
        <CollapsibleContent>
          <dl className="tabular grid grid-cols-2 gap-x-4 text-sm">
            {source.frequencyHz !== null && <Fact label="Frequency">{formatMHz(source.frequencyHz, 4)} MHz</Fact>}
            <Fact label="Kind">{modeNames[source.mode]}</Fact>
            {source.slot !== null && source.mode === "dmr" && <Fact label="Timeslot">{source.slot}</Fact>}
            {source.system && <Fact label="System">{source.system.name}</Fact>}
            {source.talkgroupId !== null && <Fact label="Talkgroup">{source.talkgroupId}</Fact>}
            <Fact label="Level">{source.levelDb !== null ? `${source.levelDb.toFixed(0)} dB` : "unknown"}</Fact>
            <div className="col-span-2">
              <Fact label="Engine channel">{source.channelIds.join(", ")}</Fact>
            </div>
          </dl>
        </CollapsibleContent>
      </Collapsible>
    </Sheet>
  );
}

export function CallItem({
  call,
  name,
  queueAfter,
  nameFor,
  compact,
}: {
  call: Call;
  name: string;
  /** Calls to play after this one, worked out when tapped. */
  queueAfter?: () => Call[];
  nameFor?: (sourceId: string) => string;
  compact?: boolean;
}) {
  const player = usePlayerInstance();
  const playing = usePlayerSelect((s) => s.kind === "call" && s.callId === call.id);
  const seconds = call.endedAt ? Math.max(1, Math.round((call.endedAt - call.startedAt) / 1000)) : null;
  const when = new Date(call.startedAt).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  const live = call.endedAt === null;
  return (
    <li className={cn("flex items-center gap-3", compact ? "py-1.5" : "px-4 py-2.5", playing && "bg-secondary/60")}>
      {call.audioUrl ? (
        <Button
          variant="secondary"
          size="icon"
          className="size-11 shrink-0 rounded-full"
          aria-label={playing ? `Stop this call from ${name}` : `Play call from ${name} at ${when}`}
          onClick={() => {
            if (playing) void player.stop();
            else player.playCall(call, name, queueAfter?.() ?? [], nameFor);
          }}
        >
          {playing ? <Pause className="size-4" /> : <Play className="size-4 fill-current" />}
        </Button>
      ) : (
        <span className="grid size-11 shrink-0 place-items-center text-muted-foreground" aria-hidden>
          {call.encrypted ? <Lock className="size-4" /> : live ? <LevelMeter active /> : <span className="size-1.5 rounded-full bg-muted-foreground/60" />}
        </span>
      )}
      <span className="min-w-0 flex-1">
        {!compact && <span className="block truncate font-medium text-heading">{name}</span>}
        <span className="tabular block text-sm text-muted-foreground">
          {when}
          {seconds !== null && ` · ${seconds} s`}
          {live && " · live"}
        </span>
        {call.transcript && <span className="mt-0.5 block text-sm text-foreground">“{call.transcript}”</span>}
      </span>
    </li>
  );
}

function BlockSheet({ open }: { open: boolean }) {
  const { blocks, current, canChange } = useBlocks();
  const backend = useBackend();
  const client = useQueryClient();
  const [choice, setChoice] = useState<string | null>(null);
  const change = useMutation({
    mutationFn: (id: string) => backend.selectBlock(id),
    onSuccess: () => {
      void client.invalidateQueries();
      toast.success("Area changed");
      sheetStore.set(null);
      setChoice(null);
    },
    onError: (e) => toast.error(`Couldn't change the area: ${e instanceof Error ? e.message : "unknown error"}`),
  });
  const chosen = blocks.find((b) => b.id === choice);
  return (
    <Sheet open={open} title="Listening area">
      <ul role="radiogroup" aria-label="Listening areas" className="space-y-2">
        {blocks.map((b) => {
          const active = b.id === current?.id;
          const selected = (choice ?? current?.id) === b.id;
          return (
            <li key={b.id}>
              <button
                type="button"
                role="radio"
                aria-checked={selected}
                disabled={!canChange}
                onClick={() => setChoice(b.id)}
                className={cn(
                  "w-full rounded-xl border p-3 text-left",
                  selected ? "border-signal bg-secondary/60" : "border-border",
                )}
              >
                <span className="flex items-baseline justify-between gap-2">
                  <span className="font-semibold text-heading">{b.name}</span>
                  {active && <span className="text-xs text-ok">Current</span>}
                </span>
                <span className="tabular mt-1 block text-xs text-muted-foreground">
                  {formatMHz(b.centerHz - b.spanHz / 2, 1)}–{formatMHz(b.centerHz + b.spanHz / 2, 1)} MHz
                </span>
              </button>
            </li>
          );
        })}
      </ul>
      {chosen && chosen.id !== current?.id && (
        <Button size="lg" className="mt-4 h-12 w-full" onClick={() => change.mutate(chosen.id)} disabled={change.isPending}>
          Switch to {chosen.name}
        </Button>
      )}
    </Sheet>
  );
}

function AlertsSheet({ open }: { open: boolean }) {
  const { alertFollowed, alertBusy } = usePrefs();
  const permission = typeof Notification === "undefined" ? "unsupported" : Notification.permission;
  const enable = async (key: "alertFollowed" | "alertBusy", value: boolean) => {
    if (value && typeof Notification !== "undefined" && Notification.permission === "default")
      await Notification.requestPermission();
    prefs.set(key, value);
  };
  return (
    <Sheet open={open} title="Alerts">
      <div className="divide-y divide-border/60">
        <label className="flex min-h-14 items-center gap-3 py-2">
          <span className="flex-1">
            <span className="block text-heading">Followed channel gets busy</span>
            <span className="tabular block text-sm text-muted-foreground">3+ calls in 10 min</span>
          </span>
          <Switch checked={alertFollowed} onCheckedChange={(v) => void enable("alertFollowed", v)} />
        </label>
        <label className="flex min-h-14 items-center gap-3 py-2">
          <span className="flex-1">
            <span className="block text-heading">Busier than usual</span>
          </span>
          <Switch checked={alertBusy} onCheckedChange={(v) => void enable("alertBusy", v)} />
        </label>
      </div>
      {permission === "denied" && (
        <p className="mt-4 flex gap-2 text-sm text-muted-foreground">
          <BellRing className="mt-0.5 size-4 shrink-0" aria-hidden />
          Notifications blocked in browser settings
        </p>
      )}
    </Sheet>
  );
}

function StatusSheet({ open }: { open: boolean }) {
  const health = useHealth();
  const status = useStatus();
  const now = useNow(1000);
  const last = status.data?.last_samples_at ? status.data.last_samples_at * 1000 : null;
  const title =
    health.state === "offline" && health.reconnecting
      ? "Reconnecting"
      : { listening: "Working", paused: "Paused", offline: "Not connected" }[health.state];
  return (
    <Sheet open={open} title={title} description={health.detail}>
      <dl className="divide-y divide-border/60">
        <Fact label="Last data">{last ? relativeTime(last, now) : "Not yet"}</Fact>
        <Fact label="Channels">{status.data?.channels.length ?? 0}</Fact>
      </dl>
      {health.state === "offline" && health.raw && (
        <Collapsible className="mt-3">
          <CollapsibleTrigger className="flex min-h-11 items-center gap-1 text-sm text-muted-foreground">
            <ChevronRight className="size-4" aria-hidden /> Error
          </CollapsibleTrigger>
          <CollapsibleContent>
            <p className="rounded-lg bg-secondary p-3 text-sm break-words">{health.raw}</p>
          </CollapsibleContent>
        </Collapsible>
      )}
      <Button
        variant="outline"
        className="mt-4 h-11 w-full"
        onClick={() => {
          sheetStore.set(null);
          navigate("operator");
        }}
      >
        Operator tools
      </Button>
    </Sheet>
  );
}

/** The expanded player: music-app "now playing". */
function PlayerSheet({ open }: { open: boolean }) {
  const [player, state] = usePlayer();
  const sources = useSources();
  const { followed } = usePrefs();
  const source = sources.find((s) => s.id === state.sourceId) ?? null;
  const taken = state.takenBy ? (sources.find((s) => s.channelIds.includes(state.takenBy!))?.name ?? null) : null;
  return (
    <Sheet open={open && state.kind !== "idle"} title={state.sourceName ?? "Player"} description={source?.summary}>
      <div className="flex items-center gap-4 rounded-2xl bg-secondary/60 p-4">
        <LevelMeter active={state.phase === "playing"} className="h-8 [&>span]:w-1.5" />
        <p aria-live="polite" className="text-heading">
          {playerLine(state, taken)}
        </p>
      </div>
      <div className="mt-5">
        <label htmlFor="volume" className="text-sm text-muted-foreground">
          Volume
        </label>
        <Slider
          id="volume"
          className="mt-3"
          value={[state.muted ? 0 : state.volume]}
          max={1}
          step={0.05}
          onValueChange={([v]) => player.setVolume(v)}
          aria-label="Volume"
        />
      </div>
      <div className="mt-6 flex flex-wrap gap-2">
        <Button size="lg" className="h-12" onClick={() => void player.stop()}>
          Stop
        </Button>
        <Button variant="outline" size="lg" className="h-12" onClick={() => player.toggleMute()} aria-pressed={state.muted}>
          {state.muted ? "Unmute" : "Mute"}
        </Button>
        {source && (
          <Button variant="outline" size="lg" className="h-12 gap-2" onClick={() => prefs.toggleFollow(source.id)}>
            <Star className={cn("size-4", followed.includes(source.id) && "fill-lamp text-lamp")} />
            {followed.includes(source.id) ? "Following" : "Follow"}
          </Button>
        )}
        {source && (
          <Button variant="ghost" size="lg" className="h-12" onClick={() => openSheet({ type: "source", id: source.id })}>
            About
          </Button>
        )}
      </div>
      {state.kind === "live" && state.background === false && (
        <p className="mt-5 text-sm text-muted-foreground">Keep the screen on</p>
      )}
    </Sheet>
  );
}

function UnnamedSheet({ frequencyHz }: { frequencyHz: number | null }) {
  return (
    <Sheet
      open={frequencyHz !== null}
      title={frequencyHz ? `${formatMHz(frequencyHz)} MHz` : "Signal"}
      description="Unidentified"
    >
      {null}
    </Sheet>
  );
}
