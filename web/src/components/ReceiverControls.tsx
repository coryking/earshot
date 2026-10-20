import { useMutation, useMutationState, useQueryClient } from "@tanstack/react-query";
import { LoaderCircle, Radio, Square } from "lucide-react";
import { useState } from "react";
import { usePlayerInstance } from "@/app/player-context";
import { Button } from "@/components/ui/button";
import { keys, useBackend, useStatus } from "@/state/radio";

const mutationKey = ["receiver-control"];

/** Receiver acquisition is shared by every browser; speaker mute and listening are local. */
export function ReceiverControls({ details = false }: { details?: boolean }) {
  const backend = useBackend();
  const { data: status, isError } = useStatus();
  const player = usePlayerInstance();
  const client = useQueryClient();
  const [error, setError] = useState<string | null>(null);
  const receiver = status?.receiver;
  const supported = receiver?.fresh === true && receiver.control_supported === true && !!backend.startReceiver;
  const requested = receiver?.receiving_requested;
  const receiving = requested ?? status?.state === "receiving";
  const pending = useMutationState({ filters: { mutationKey, status: "pending" }, select: (m) => m.state.variables as boolean });
  const busy = pending.length > 0;
  const startPending = pending.at(-1) === true;
  const control = useMutation({
    mutationKey,
    mutationFn: async (start: boolean) => {
      setError(null);
      // Both samples come from the engine's clock; the browser may disagree.
      const previousSamples = start ? (await backend.status()).last_samples_at : null;
      if (start) await backend.startReceiver!();
      else {
        await player.stop();
        await backend.stopReceiver();
      }
      // An accepted request is not proof of hardware state. Read back samples or restoration.
      const deadline = Date.now() + 120_000;
      do {
        const snapshot = await backend.status();
        client.setQueryData(keys.status, snapshot);
        const actual = snapshot.receiver;
        if (actual?.fresh && actual.receiving_requested === start) {
          if (start && actual.state === "running" && snapshot.state === "receiving"
            && (snapshot.last_samples_at ?? 0) > (previousSamples ?? 0)) return;
          if (!start && actual.state === "stopped" && actual.reason === "operator") {
            if (!actual.last_restore || actual.last_restore.startsWith("UNVERIFIED")) {
              throw new Error("Receiving stopped; receiver restoration could not be verified. Check Operator tools.");
            }
            return;
          }
        }
        await new Promise((resolve) => setTimeout(resolve, 500));
      } while (Date.now() < deadline);
      throw new Error(`${start ? "Start" : "Stop"} requested, but the receiver has not confirmed it. Check Operator tools.`);
    },
    onError: (e) => setError(e instanceof Error ? e.message : "Receiver control failed"),
    onSettled: () => { void client.invalidateQueries({ queryKey: keys.status }); },
  });
  const label = busy ? (startPending ? "Starting receiver…" : "Stopping receiver…")
    : receiving ? "Stop receiving" : "Start receiving";
  return (
    <div className={details ? "space-y-2" : "relative shrink-0"}>
      <Button
        variant={receiving ? "outline" : "default"}
        size="sm"
        className="min-h-10 gap-2 whitespace-nowrap"
        disabled={!supported || isError || busy}
        onClick={() => control.mutate(!receiving)}
        title={supported ? "Controls receiver acquisition for everyone" : "Receiver start/stop is unavailable from this engine"}
      >
        {busy ? <LoaderCircle className="size-4 animate-spin" aria-hidden />
          : receiving ? <Square className="size-3.5" aria-hidden /> : <Radio className="size-4" aria-hidden />}
        {label}
      </Button>
      {details && <p className="text-sm text-muted-foreground">Controls receiving for everyone. Stop receiving stays stopped until Start receiving. To silence only your speaker, use Mute; to release your frequency, use Stop listening.</p>}
      {details && !supported && <p className="text-sm text-muted-foreground">Receiver controls require a connected supervisor with start/stop support.</p>}
      {error && <p role="alert" className={details ? "text-sm text-destructive" : "absolute top-full left-0 z-40 mt-2 w-72 rounded-lg border border-destructive bg-background p-3 text-sm text-destructive shadow-lg"}>{error}</p>}
    </div>
  );
}
