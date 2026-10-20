import type { Mode } from "@/domain/model";
import { cn } from "@/lib/utils";

export interface ChannelIdentityProps {
  name?: string | null;
  frequencyHz?: number | null;
  mode?: Mode;
  slot?: number | null;
  compact?: boolean;
  className?: string;
}

function frequencyLabel(hz: number): string {
  return `${(hz / 1e6).toFixed(5).replace(/0{1,2}$/, "")} MHz`;
}

/** The same channel identity everywhere: name, precise frequency, mode and slot. */
export function ChannelIdentity({ name, frequencyHz, mode, slot, compact, className }: ChannelIdentityProps) {
  const frequency = frequencyHz == null ? "Frequency unavailable" : frequencyLabel(frequencyHz);
  const named = name && !/^\d+(?:\.\d+)? MHz(?:, line \d+)?$/i.test(name);
  const details = [frequency, mode && mode !== "unknown" ? mode.toUpperCase() : null,
    mode === "dmr" && slot ? `Slot ${slot}` : null].filter(Boolean).join(" · ");
  return (
    <span className={cn("block min-w-0", className)}>
      {named && <span className={cn("block truncate", !compact && "font-medium")}>{name}</span>}
      <span className={cn("tabular block truncate", named && "text-muted-foreground", compact ? "text-[11px]" : "text-sm")}>
        {details}
      </span>
    </span>
  );
}
