import { useState } from "react";
import { usePlayer, usePlayerInstance } from "@/app/player-context";
import { ChannelIdentity } from "@/components/ChannelIdentity";
import { playerLine } from "@/components/PlayerBar";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { useBackend, useSources } from "@/state/radio";

export function TuneForm({ initialHz, onTune }: { initialHz?: number; onTune?(): void }) {
  const [frequency, setFrequency] = useState(initialHz ? (initialHz / 1e6).toFixed(5) : "");
  const player = usePlayerInstance();
  const backend = useBackend();
  const hz = Number(frequency) * 1e6;
  return (
    <form className="flex items-end gap-2" onSubmit={(e) => {
      e.preventDefault();
      if (!backend.tune || !frequency.trim() || !Number.isFinite(hz) || hz < 70e6 || hz > 6e9) return;
      void player.listenFrequency(hz);
      onTune?.();
    }}>
      <label className="min-w-0 flex-1 text-sm text-muted-foreground">
        Frequency (MHz)
        <Input className="mt-1" value={frequency} onChange={(e) => setFrequency(e.target.value)}
          inputMode="decimal" placeholder="452.67500" />
      </label>
      <Button type="submit" disabled={!backend.tune || !frequency.trim() || !Number.isFinite(hz) || hz < 70e6 || hz > 6e9}>Tune</Button>
    </form>
  );
}

export function LiveSelection() {
  const [player, state] = usePlayer();
  const sources = useSources();
  const source = sources.find((s) => s.id === state.sourceId);
  return (
    <section aria-label="Live tuning" className="rounded-xl border border-border bg-card p-4">
      {state.kind === "live" ? (
        <>
          <ChannelIdentity name={state.sourceName} frequencyHz={state.frequencyHz} mode={state.mode} slot={state.slot} className="text-lg text-heading" />
          <p className="mt-2 text-sm" role="status">{playerLine(state, null)}</p>
          {state.phase === "waiting" && <p className="mt-1 text-sm text-muted-foreground">
            {source?.signal ? "Signal detected; no decoded audio yet." : "Audio will play when a transmission is decoded."}
          </p>}
          <div className="mt-3 flex gap-2">
            <Button variant="secondary" onClick={() => player.toggleMute()}>{state.muted ? "Unmute" : "Mute"}</Button>
            <Button variant="outline" onClick={() => void player.stop()}>Stop audio</Button>
          </div>
        </>
      ) : <p className="text-sm text-muted-foreground">Choose a channel or enter a frequency to hear live audio.</p>}
      <div className="mt-3"><TuneForm /></div>
    </section>
  );
}
