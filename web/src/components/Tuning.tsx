import { useState } from "react";
import { useQuery, useMutation } from "@tanstack/react-query";
import { usePlayer, usePlayerInstance } from "@/app/player-context";
import { ChannelIdentity } from "@/components/ChannelIdentity";
import { playerLine } from "@/components/PlayerBar";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { useBackend, useSources } from "@/state/radio";
import { Volume2, VolumeX, Square } from "lucide-react";

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

export function IqCapture({ channelId, frequencyHz }: { channelId: string; frequencyHz: number | null }) {
  const backend = useBackend();
  const iq = useQuery({
    queryKey: ["iq-capture", backend.mode, channelId],
    queryFn: () => backend.iqCaptureStatus!(channelId),
    enabled: !!backend.iqCaptureStatus,
    // Another listener may keep this physical slice rolling after we stop.
    refetchInterval: 1000,
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
  const status = iq.data;
  const rate = status?.sample_rate_hz;
  const center = status?.center_hz;
  return <section aria-label="IQ capture" className="mt-3 rounded-lg border border-border bg-background/50 p-3 text-xs text-muted-foreground">
    <div className="flex flex-wrap items-center justify-between gap-2">
      <div>
        <h3 className="font-medium text-heading">IQ capture · receiver server</h3>
        <p className="mt-1 text-sm font-medium tabular-nums text-foreground">
          {status ? `${status.seconds.toFixed(1)} s buffered / ${status.max_seconds} s maximum` : "Checking IQ buffer…"}
        </p>
        {status && <p className="mt-1">{status.collecting ? "Rolling · receiving samples" : status.samples ? "Buffer paused · no new samples arriving" : "No IQ samples buffered"}</p>}
      </div>
      <Button variant="secondary" disabled={!iq.data?.samples || download.isPending} onClick={() => download.mutate()}>
        {download.isPending ? "Preparing IQ…" : "Download IQ snapshot"}
      </Button>
    </div>
    {rate != null && center != null && <p className="mt-2 tabular-nums text-foreground">
      Sampled range {((center - rate / 2) / 1e6).toFixed(5)}–{((center + rate / 2) / 1e6).toFixed(5)} MHz · {(rate / 1000).toFixed(1)} kHz wide
    </p>}
    <p className="mt-2">Downloads the current rolling buffer; oldest samples are replaced.</p>
    <details className="mt-2">
      <summary className="cursor-pointer">IQ format, retention and capture controls</summary>
      {rate != null && center != null && <p className="mt-1">Physical slice centered at {(center / 1e6).toFixed(5)} MHz · {(rate / 1000).toFixed(1)} kS/s complex samples. The sampled range includes neighboring frequencies, before host channel filtering and FM demodulation.</p>}
      <p className="mt-1">Rolling server memory, not a recording on disk. The shared 32 MiB limit can shorten retention. Download saves SigMF IQ plus frequency, sample-rate and packet metadata. Times count received samples; missing intervals are marked, not filled.</p>
      <p className="mt-1">Mute only silences your speaker. Stop listening releases this browser’s stream; the remaining IQ stays available, but other listeners on the same slice can keep it rolling. Resume collects again. Choosing a frequency shows its physical slice’s buffer, which can include earlier samples. A new receiver generation, slice center or sample format resets that buffer. Later listening can evict old samples; restarting the server loses them.</p>
    </details>
    {(iq.error || download.error) && <p role="alert" className="mt-1 text-destructive">{(iq.error ?? download.error)?.message}</p>}
  </section>;
}

export function LiveSelection() {
  const [player, state] = usePlayer();
  const sources = useSources();
  const source = sources.find((s) => s.id === state.sourceId);
  const speakerSilent = state.muted || state.volume === 0;
  return (
    <section aria-label="Live tuning" className="shrink-0 rounded-xl border border-border bg-card p-4">
      {state.kind === "live" ? (
        <>
          <p className="mb-1 text-xs font-medium uppercase tracking-wide text-primary">{state.phase === "taken" ? "Channel changed elsewhere" : state.phase === "error" ? "Listening error" : "Held channel"}</p>
          <ChannelIdentity name={state.sourceName} frequencyHz={state.frequencyHz} mode={state.mode} slot={state.slot} className="text-lg text-heading" />
          <p className="mt-2 text-sm" role="status">{playerLine(state, null)}</p>
          {state.phase !== "taken" && state.phase !== "error" && (state.raw || state.phase === "waiting") && <p className="mt-1 text-sm text-muted-foreground">
            {state.raw ? "FM signal audio · unsquelched. Sound plays as soon as receiver samples arrive, including noise and digital signals." : source?.signal ? "Signal detected. Digital activity can be data or control traffic and remain silent. Voice plays only when decoded; Hear FM signal lets you hear the signal itself." : "Listening is on. Decoded voice will play automatically when a transmission arrives."}
          </p>}
          <div className="mt-3 flex flex-wrap gap-2">
            <Button variant="secondary" aria-pressed={speakerSilent} onClick={() => player.toggleMute()}>{speakerSilent ? <VolumeX /> : <Volume2 />}{speakerSilent ? "Unmute speaker" : "Mute speaker"}</Button>
            <Button variant="outline" onClick={() => void player.stop()}><Square />Stop listening</Button>
            {!state.raw && <Button variant="outline" onClick={() => void player.listenRaw()}>Hear FM signal</Button>}
          </div>
        </>
      ) : <>
        <p className="font-medium text-heading" role="status">Listening off</p>
        {state.channelId && <p className="mt-1 text-xs font-medium uppercase tracking-wide">Last selection</p>}
        {state.frequencyHz !== null && <ChannelIdentity frequencyHz={state.frequencyHz} name={state.sourceName} mode={state.mode} slot={state.slot} />}
        <p className="mt-1 text-sm text-muted-foreground">Receiving is controlled separately in the header. Audio will not start automatically. Click or drag a signal, or enter a frequency and press Listen.</p>
        {state.channelId && <Button className="mt-3" variant="secondary" onClick={() => void player.resume()}>Resume listening</Button>}
      </>}
      {(state.raw || state.captureSeconds > 0) && <div className="mt-3 flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
        <Button variant="outline" disabled={!state.captureSeconds} onClick={() => player.saveCapture()}>Save FM audio WAV</Button>
        <span>{state.captureSeconds.toFixed(1)} s / 120 s maximum · FM audio WAV · 48 kHz · rolling memory in this browser</span>
      </div>}
      {state.channelId && <IqCapture channelId={state.channelId} frequencyHz={state.frequencyHz} />}
      <div className="mt-3"><TuneForm key={state.frequencyHz ?? "manual"} initialHz={state.frequencyHz ?? undefined} /></div>
    </section>
  );
}
