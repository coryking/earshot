import { useState } from "react";
import { useQuery, useMutation } from "@tanstack/react-query";
import { usePlayer, usePlayerInstance } from "@/app/player-context";
import { ChannelIdentity } from "@/components/ChannelIdentity";
import { playerLine } from "@/components/PlayerBar";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { useBackend, useSources } from "@/state/radio";

export function TuneForm({ initialHz }: { initialHz?: number }) {
  const [frequency, setFrequency] = useState(initialHz ? (initialHz / 1e6).toFixed(5) : "");
  const player = usePlayerInstance();
  const backend = useBackend();
  const hz = Number(frequency) * 1e6;
  return (
    <form className="flex items-end gap-2" onSubmit={(e) => {
      e.preventDefault();
      if (!backend.tune || !frequency.trim() || !Number.isFinite(hz) || hz < 70e6 || hz > 6e9) return;
      void player.listenFrequency(hz);
    }}>
      <label className="min-w-0 flex-1 text-sm text-muted-foreground">
        Frequency (MHz)
        <Input className="mt-1" value={frequency} onChange={(e) => setFrequency(e.target.value)}
          inputMode="decimal" placeholder="452.67500" />
      </label>
      <Button type="submit" disabled={!backend.tune || !frequency.trim() || !Number.isFinite(hz) || hz < 70e6 || hz > 6e9}>Listen</Button>
    </form>
  );
}

function IqCapture({ channelId, frequencyHz, live }: { channelId: string; frequencyHz: number | null; live: boolean }) {
  const backend = useBackend();
  const iq = useQuery({
    queryKey: ["iq-capture", backend.mode, channelId, live],
    queryFn: () => backend.iqCaptureStatus!(channelId),
    enabled: !!backend.iqCaptureStatus,
    refetchInterval: live ? 1000 : false,
    retry: false,
  });
  const download = useMutation({ mutationFn: async () => {
    const blob = await backend.downloadIq!(channelId);
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = `iq_${frequencyHz ?? "slice"}Hz.sigmf.tar`;
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  } });
  if (!backend.iqCaptureStatus || !backend.downloadIq || iq.data === null) return null;
  return <div className="mt-3 border-t border-border pt-3 text-xs text-muted-foreground">
    <div className="flex flex-wrap items-center gap-2">
      <Button variant="secondary" disabled={!iq.data?.samples || download.isPending} onClick={() => download.mutate()}>
        {download.isPending ? "Preparing IQ…" : "Save IQ"}
      </Button>
      <span>{iq.data?.seconds.toFixed(1) ?? "0.0"} s of IQ buffered{!live ? " · recording stopped" : iq.data?.collecting ? " · buffering IQ" : " · waiting for receiver samples"}</span>
    </div>
    <p className="mt-1">For signal analysis · original complex samples before FM demodulation.
      {iq.data?.sample_rate_hz ? ` ${(iq.data.sample_rate_hz / 1000).toFixed(1)} kS/s slice centered at ${((iq.data.center_hz ?? 0) / 1e6).toFixed(5)} MHz; up to ${iq.data.max_seconds} s retained on the receiver server.` : " Waiting for IQ from the receiver."}
    </p>
    {(iq.error || download.error) && <p role="alert" className="mt-1 text-destructive">{(iq.error ?? download.error)?.message}</p>}
  </div>;
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
          {(state.raw || state.phase === "waiting") && <p className="mt-1 text-sm text-muted-foreground">
            {state.raw ? "FM signal audio · unsquelched. Sound plays as soon as receiver samples arrive, including noise and digital signals." : source?.signal ? "Signal detected. Voice will play automatically if decoded; use FM signal audio to hear the signal now." : "Listening is on. Decoded voice will play automatically when a transmission arrives."}
          </p>}
          <div className="mt-3 flex flex-wrap gap-2">
            <Button variant="secondary" onClick={() => player.toggleMute()}>{state.muted ? "Unmute" : "Mute"}</Button>
            <Button variant="outline" onClick={() => void player.stop()}>Stop audio</Button>
            {!state.raw && <Button variant="outline" onClick={() => void player.listenRaw()}>Hear FM signal</Button>}
          </div>
        </>
      ) : <>
        <p className="font-medium text-heading" role="status">{state.channelId ? "Listening stopped" : "Not listening"}</p>
        {state.frequencyHz !== null && <ChannelIdentity frequencyHz={state.frequencyHz} name={state.sourceName} mode={state.mode} slot={state.slot} />}
        <p className="mt-1 text-sm text-muted-foreground">The waterfall keeps updating. Audio will not start automatically. Click or drag a signal, or enter a frequency and press Listen.</p>
        {state.channelId && <Button className="mt-3" variant="secondary" onClick={() => void player.resume()}>Resume listening</Button>}
      </>}
      {(state.raw || state.captureSeconds > 0) && <div className="mt-3 flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
        <Button variant="outline" disabled={!state.captureSeconds} onClick={() => player.saveCapture()}>Save FM audio WAV</Button>
        <span>{state.captureSeconds.toFixed(1)} s · FM audio, not IQ · 48 kHz · last 120 s kept in this browser</span>
      </div>}
      {state.channelId && <IqCapture channelId={state.channelId} frequencyHz={state.frequencyHz} live={state.kind === "live" && state.phase !== "error" && state.phase !== "taken"} />}
      <div className="mt-3"><TuneForm key={state.frequencyHz ?? "manual"} initialHz={state.frequencyHz ?? undefined} /></div>
    </section>
  );
}
