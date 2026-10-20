import { Loader2, Square, Volume2, VolumeX, X } from "lucide-react";
import { usePlayer } from "@/app/player-context";
import { openSheet } from "@/app/ui";
import { ChannelIdentity } from "@/components/ChannelIdentity";
import { LevelMeter } from "@/components/bits";
import { Button } from "@/components/ui/button";
import { isAudible, type PlayerState } from "@/audio/player";
import { useSources } from "@/state/radio";

function clock(seconds: number) {
  if (!Number.isFinite(seconds)) return "0:00";
  const s = Math.max(0, Math.round(seconds));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

export function playerLine(state: PlayerState, takenName: string | null): string {
  switch (state.phase) {
    case "taken":
      return `Switched to ${takenName ?? "another channel"} elsewhere`;
    case "error":
      return state.error ?? "Something went wrong";
    case "reconnecting":
      return "Reconnecting…";
    case "starting":
      return state.kind === "call" ? "Loading recording…" : "Tuning…";
    case "idle":
      return "";
  }
  const silent = state.muted || state.volume === 0;
  if (state.kind === "call") {
    const progress = `Recorded · ${clock(state.position)} / ${clock(state.duration)}`;
    if (state.phase === "waiting") return `Buffering recording…${silent ? " · speaker muted" : ""}`;
    return silent ? `Speaker muted · ${progress}` : `Playing ${progress.toLowerCase()}`;
  }
  if (state.phase === "waiting") {
    const waiting = state.raw ? "waiting for receiver samples" : "waiting for decoded voice";
    return `Held · ${waiting}${silent ? " · speaker muted" : ""}`;
  }
  if (silent) return "Speaker muted · live audio arriving";
  return state.raw ? "Playing FM signal audio" : "Playing live voice";
}

/** The same speaker controls in the dock and expanded player; mute keeps listening armed. */
export function SpeakerControls({ id }: { id: string }) {
  const [player, state] = usePlayer();
  const silent = state.muted || state.volume === 0;
  return (
    <div className="flex min-w-0 items-center gap-3">
      <Button
        variant={silent ? "default" : "secondary"}
        className="h-11 shrink-0 gap-2"
        onClick={() => player.toggleMute()}
        aria-pressed={silent}
      >
        {silent ? <VolumeX className="size-4" aria-hidden /> : <Volume2 className="size-4" aria-hidden />}
        {silent ? "Unmute speaker" : "Mute speaker"}
      </Button>
      <div className="min-w-0 flex-1">
        <label htmlFor={id} className="flex justify-between text-xs text-muted-foreground">
          <span>Volume</span><span className="tabular">{Math.round(state.volume * 100)}%</span>
        </label>
        <input
          id={id}
          type="range"
          className="h-7 w-full cursor-pointer accent-signal"
          value={state.volume}
          min={0}
          max={1}
          step={0.05}
          onChange={(event) => player.setVolume(Number(event.currentTarget.value))}
          aria-label="Speaker volume"
        />
      </div>
    </div>
  );
}

export function PlayerBar() {
  const [player, state] = usePlayer();
  const sources = useSources();
  if (state.kind === "idle" && state.phase !== "error") return null;
  const taken = state.takenBy ? (sources.find((s) => s.channelIds.includes(state.takenBy!))?.name ?? null) : null;
  const errorOnly = state.kind === "idle";
  return (
    <div role="region" aria-label="Player" className="border-t border-border bg-card/95 px-4 py-2 backdrop-blur">
      <div className="flex items-center gap-3">
        <button
          type="button"
          className="flex min-h-11 min-w-0 flex-1 items-center gap-3 text-left"
          onClick={() => !errorOnly && openSheet({ type: "player" })}
          disabled={errorOnly}
        >
          {!errorOnly && <span className="sr-only">Open player:</span>}{" "}
          {state.phase === "starting" || state.phase === "reconnecting" ? (
            <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" aria-hidden />
          ) : (
            <LevelMeter active={isAudible(state)} className="shrink-0" />
          )}
          <span className="min-w-0 flex-1">
            <ChannelIdentity name={state.sourceName} frequencyHz={state.frequencyHz} mode={state.mode} slot={state.slot} className="text-heading" />
            <span aria-live="polite" className={state.phase === "error" ? "block text-sm text-destructive" : "block text-sm text-muted-foreground"}>
              {playerLine(state, taken)}
            </span>
          </span>
        </button>
        <Button
          variant="outline"
          className="h-11 shrink-0 gap-2"
          onClick={() => void player.stop()}
        >
          {errorOnly ? <X className="size-4" aria-hidden /> : <Square className="size-3 fill-current" aria-hidden />}
          {errorOnly ? "Dismiss" : state.kind === "call" ? "Stop playback" : "Stop listening"}
        </Button>
      </div>
      {state.phase === "taken" && (
        <Button size="sm" variant="secondary" className="my-2" onClick={() => void player.reclaim()}>
          Switch back
        </Button>
      )}
      {!errorOnly && <div className="mt-2"><SpeakerControls id="dock-speaker-volume" /></div>}
    </div>
  );
}
