import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { usePlayerInstance } from "@/app/player-context";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { keys, useBackend, useStatus } from "@/state/radio";

/** Move the receiver window, keeping this producer's sample rate and RF width. */
export function SpectrumNavigation() {
  const backend = useBackend();
  const { data: status } = useStatus();
  const player = usePlayerInstance();
  const client = useQueryClient();
  const [draft, setDraft] = useState<string | null>(null);
  const center = status?.center_hz ?? 0;
  const rate = status?.rate_hz ?? 0;
  const width = status?.bandwidth_hz ?? 0;
  const capabilities = status?.source?.capabilities as Record<string, unknown> | undefined;
  const supported = status?.state === "receiving" && center > 0 && rate > 0 && width > 0
    && (capabilities?.configure_supported === true
      || (capabilities?.configure_supported == null && capabilities?.controls_supported === true));
  const text = draft ?? (center ? (center / 1e6).toFixed(5) : "");
  const hz = Number(text) * 1e6;
  const valid = (value: number) => Number.isFinite(value) && value >= 70e6 && value <= 6e9;
  const move = useMutation({
    mutationFn: async (centerHz: number) => {
      if (!supported || !valid(centerHz)) throw new Error("Receiver window unavailable");
      await player.stop();
      await backend.configureSource({ centerHz: Math.round(centerHz), rateHz: rate, bandwidthHz: width });
      // Display readback rather than assuming the requested frequency was applied.
      client.setQueryData(keys.status, await backend.status());
    },
    onSuccess: () => { setDraft(null); },
    onSettled: () => { void client.invalidateQueries({ queryKey: keys.status }); void client.invalidateQueries({ queryKey: keys.blocks }); },
  });
  const disabled = !supported || move.isPending;
  return <section aria-label="Spectrum window" className="shrink-0 rounded-xl border border-border bg-card p-3">
    <div className="mb-2 flex flex-wrap justify-between gap-1 text-xs text-muted-foreground">
      <span>Spectrum window</span>
      {center > 0 && width > 0 && <span className="tabular-nums">{((center - width / 2) / 1e6).toFixed(3)}–{((center + width / 2) / 1e6).toFixed(3)} MHz</span>}
    </div>
    <form className="flex flex-wrap items-end gap-2" onSubmit={(event) => { event.preventDefault(); if (!disabled && valid(hz)) move.mutate(hz); }}>
      <Button type="button" variant="outline" disabled={disabled || !valid(center - width)} onClick={() => move.mutate(center - width)}>← Previous</Button>
      <label className="min-w-24 flex-1 text-xs text-muted-foreground">Center MHz
        <Input className="mt-1" inputMode="decimal" value={text} disabled={disabled} onChange={(event) => setDraft(event.target.value)} />
      </label>
      <Button type="submit" disabled={disabled || !text.trim() || !valid(hz)}>{move.isPending ? "Moving…" : "Go"}</Button>
      <Button type="button" variant="outline" disabled={disabled || !valid(center + width)} onClick={() => move.mutate(center + width)}>Next →</Button>
    </form>
    {!supported && <p className="mt-2 text-xs text-muted-foreground">Start a receiver with spectrum-window controls to move the window.</p>}
    {move.error && <p role="alert" className="mt-2 text-sm text-destructive">{move.error.message}</p>}
  </section>;
}
