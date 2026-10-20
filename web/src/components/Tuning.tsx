import { useState } from "react";
import { useQuery, useMutation } from "@tanstack/react-query";
import { usePlayer, usePlayerInstance } from "@/app/player-context";
import { SpeakerControls } from "@/components/PlayerBar";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { useBackend, useSources } from "@/state/radio";
import { Square, Play } from "lucide-react";

export function TuneForm({ initialHz }: { initialHz?: number }) {
  const [frequency, setFrequency] = useState(initialHz ? (initialHz / 1e6).toFixed(5) : "");
  const player = usePlayerInstance();
  const backend = useBackend();
  const hz = Number(frequency) * 1e6;
  return <form className="flex items-center gap-1" onSubmit={(e) => {
    e.preventDefault();
    if (backend.tune && frequency.trim() && Number.isFinite(hz) && hz >= 70e6 && hz <= 6e9)
      void player.listenFrequency(hz);
  }}>
    <Input aria-label="Frequency (MHz)" className="w-32 text-base tabular-nums" value={frequency}
      onChange={(e) => setFrequency(e.target.value)} inputMode="decimal" placeholder="Frequency" />
    <span className="text-xs text-muted-foreground">MHz</span>
    <Button variant="secondary" type="submit" disabled={!backend.tune || !frequency.trim() || !Number.isFinite(hz) || hz < 70e6 || hz > 6e9}>Tune</Button>
  </form>;
}

export function useIqCapture(channelId: string | null) {
  const backend = useBackend();
  return useQuery({ queryKey: ["iq-capture", backend.mode, channelId],
    queryFn: () => backend.iqCaptureStatus!(channelId!),
    enabled: !!channelId && !!backend.iqCaptureStatus, refetchInterval: 1000, retry: false });
}

export function IqCapture({ channelId, frequencyHz }: { channelId: string; frequencyHz: number | null }) {
  const backend = useBackend();
  const iq = useIqCapture(channelId);
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
  const range = status?.sample_rate_hz != null && status.center_hz != null
    ? `${((status.center_hz - status.sample_rate_hz / 2) / 1e6).toFixed(5)}–${((status.center_hz + status.sample_rate_hz / 2) / 1e6).toFixed(5)} MHz` : null;
  return <section aria-label="IQ capture" className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
    <Button variant="secondary" size="sm" disabled={!status?.samples || download.isPending} onClick={() => download.mutate()}>
      {download.isPending ? "Saving…" : "Save IQ"}
    </Button>
    <span className="tabular-nums">{status ? `${status.seconds.toFixed(1)} / ${status.max_seconds} s` : "…"}</span>
    {status?.sample_rate_hz != null && <span>{status.sample_rate_hz / 1000} kHz</span>}
    {status && <span>{status.collecting ? "Rolling" : "Paused"}</span>}
    <details className="relative">
      <summary className="cursor-pointer">Details</summary>
      <div className="absolute bottom-full right-0 z-20 mb-2 w-72 rounded-lg border border-border bg-card p-3 shadow-xl">
        {range && <p className="tabular-nums">{range}</p>}
        <p className="mt-2">Original I/Q · SigMF archive · rolling server buffer. Shared memory can shorten retention. Receiver restart clears it.</p>
      </div>
    </details>
    {(iq.error || download.error) && <p role="alert" className="text-destructive">{(iq.error ?? download.error)?.message}</p>}
  </section>;
}

export function LiveSelection() {
  const [player, state] = usePlayer();
  const sources = useSources();
  const source = sources.find((s) => s.id === state.sourceId);
  const live = state.kind === "live";
  const status = state.phase === "error" ? state.error : state.phase === "taken" ? "Changed elsewhere"
    : state.phase === "reconnecting" ? "Reconnecting…" : state.phase === "starting" ? "Tuning…"
    : !live ? "Audio off" : state.muted || state.volume === 0 ? "Muted" : state.phase === "playing" ? "Playing" : "Waiting";
  return <section aria-label="Live tuning" className="flex shrink-0 flex-wrap items-center gap-x-4 gap-y-2 border-b border-border px-3 py-2">
    <TuneForm key={state.frequencyHz ?? "manual"} initialHz={state.frequencyHz ?? undefined} />
    <select aria-label="Audio mode" className="h-10 rounded-md border border-border bg-card px-2 text-sm"
      value={state.raw ? "fm" : "decoded"} disabled={!live} onChange={(e) => {
        const raw = e.target.value === "fm";
        if (source) void player.listen(source, { raw });
        else if (state.frequencyHz !== null) void player.listenFrequency(state.frequencyHz, raw);
      }}>
      <option value="fm">FM signal</option><option value="decoded">Decoded voice</option>
    </select>
    <div className="flex items-center gap-2">
      <Button variant="ghost" size="icon" disabled={!live && !state.channelId}
        aria-label={live ? "Stop listening" : "Resume listening"} title={live ? "Stop listening" : "Resume listening"}
        onClick={() => live ? void player.stop() : void player.resume()}>{live ? <Square className="size-4" /> : <Play className="size-4" />}</Button>
      <span role="status" className={state.phase === "error" ? "max-w-64 text-xs text-destructive" : "text-xs text-muted-foreground"}>{status}</span>
    </div>
    <div className="min-w-60 flex-1 sm:ml-auto sm:max-w-80"><SpeakerControls id="tuning-volume" /></div>
  </section>;
}
