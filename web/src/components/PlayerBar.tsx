import { Loader2, Square, Volume2, VolumeX, X } from "lucide-react";
import { usePlayer } from "@/app/player-context";
import { openSheet } from "@/app/ui";
import { LevelMeter } from "@/components/bits";
import { Button } from "@/components/ui/button";
import type { PlayerState } from "@/audio/player";
import { useSources } from "@/state/radio";

function clock(seconds: number) {
  if (!Number.isFinite(seconds)) return "0:00";
  const s = Math.max(0, Math.round(seconds));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

export function playerLine(state: PlayerState, takenName: string | null): string {
  if (state.kind === "call")
    return state.phase === "error" ? (state.error ?? "") : `Recorded call, ${clock(state.position)} of ${clock(state.duration)}`;
  switch (state.phase) {
    case "starting":
      return "Connecting to the radio…";
    case "waiting":
      return "Live. Waiting for someone to talk.";
    case "playing":
      return "Live. Talking now.";
    case "reconnecting":
      return "Reconnecting to the radio…";
    case "taken":
      return `Someone on another device switched the radio to ${takenName ?? "another channel"}.`;
    case "error":
      return state.error ?? "Something went wrong.";
    default:
      return "";
  }
}

/** Docked above the tab bar, like a music app's mini player. Stop and mute sit under the thumb. */
export function PlayerBar() {
  const [player, state] = usePlayer();
  const sources = useSources();
  if (state.kind === "idle" && state.phase !== "error") return null;
  const taken = state.takenBy ? (sources.find((s) => s.channelIds.includes(state.takenBy!))?.name ?? null) : null;
  const line = playerLine(state, taken);
  const errorOnly = state.kind === "idle";
  return (
    <div role="region" aria-label="Player" className="border-t border-border bg-card/95 backdrop-blur">
      <div className="flex items-center gap-2 py-2 pr-2 pl-4 lg:pr-4">
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
            <LevelMeter active={state.phase === "playing"} className="shrink-0" />
          )}
          <span className="min-w-0 flex-1">
            <span className="block truncate font-semibold text-heading">{state.sourceName}</span>
            <span aria-live="polite" className={state.phase === "error" ? "block text-sm text-destructive" : "block truncate text-sm text-muted-foreground"}>
              {line}
            </span>
          </span>
        </button>
        {state.phase === "taken" && (
          <Button size="sm" variant="secondary" onClick={() => void player.reclaim()}>
            Switch back
          </Button>
        )}
        {!errorOnly && (
          <Button
            variant="ghost"
            size="icon"
            className="size-11"
            onClick={() => player.toggleMute()}
            aria-label={state.muted ? "Unmute" : "Mute"}
            aria-pressed={state.muted}
          >
            {state.muted ? <VolumeX className="size-5" /> : <Volume2 className="size-5" />}
          </Button>
        )}
        <Button
          size="icon"
          className="size-12 rounded-full"
          onClick={() => void player.stop()}
          aria-label={errorOnly ? "Dismiss" : "Stop"}
        >
          {errorOnly ? <X className="size-5" /> : <Square className="size-4 fill-current" />}
        </Button>
      </div>
    </div>
  );
}
