import { ChevronDown, Lock, MoreHorizontal, Star } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { usePlayerInstance, usePlayerSelect } from "@/app/player-context";
import { openSheet } from "@/app/ui";
import { CallLane, LevelMeter } from "@/components/bits";
import type { Call, Source } from "@/domain/model";
import { relativeTime } from "@/domain/summary";
import { cn } from "@/lib/utils";

interface Props {
  sources: Source[];
  callsBySource: Map<string, Call[]>;
  followed: ReadonlySet<string>;
  muted: ReadonlySet<string>;
  now: number;
}

/** The home list, in the manner of a chat app's channel list. */
export function SourceList({ sources, callsBySource, followed, muted, now }: Props) {
  const [showMuted, setShowMuted] = useState(false);
  const audible = sources.filter((s) => !muted.has(s.id));
  const hidden = sources.filter((s) => muted.has(s.id));
  if (!sources.length)
    return (
      <div className="px-5 py-10 text-center">
        <p className="text-heading">Nothing heard yet.</p>
        <p className="mt-1 text-sm text-muted-foreground">
          Channels show up here the first time someone talks on them. Leave this open and they'll appear.
        </p>
      </div>
    );
  return (
    <div>
      <ul aria-label="Channels" className="divide-y divide-border/60" data-source-list>
        {audible.map((s) => (
          <SourceRow key={s.id} source={s} calls={callsBySource.get(s.id) ?? []} followed={followed.has(s.id)} now={now} />
        ))}
      </ul>
      {hidden.length > 0 && (
        <div className="border-t border-border/60">
          <button
            type="button"
            className="flex min-h-11 w-full items-center gap-2 px-4 text-sm text-muted-foreground"
            onClick={() => setShowMuted((v) => !v)}
            aria-expanded={showMuted}
          >
            <ChevronDown className={cn("size-4 transition-transform", !showMuted && "-rotate-90")} aria-hidden />
            Muted ({hidden.length})
          </button>
          {showMuted && (
            <ul aria-label="Muted channels" className="divide-y divide-border/60 opacity-70">
              {hidden.map((s) => (
                <SourceRow key={s.id} source={s} calls={callsBySource.get(s.id) ?? []} followed={false} now={now} />
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}

function SourceRow({ source, calls, followed, now }: { source: Source; calls: Call[]; followed: boolean; now: number }) {
  const player = usePlayerInstance();
  const playing = usePlayerSelect((s) => s.kind === "live" && s.sourceId === source.id);
  const press = useRef<ReturnType<typeof setTimeout> | null>(null);
  const longPressed = useRef(false);
  const startPress = () => {
    longPressed.current = false;
    press.current = setTimeout(() => {
      longPressed.current = true;
      openSheet({ type: "source", id: source.id });
    }, 550);
  };
  const endPress = () => {
    if (press.current) clearTimeout(press.current);
    press.current = null;
  };
  useEffect(() => endPress, []);
  const right = source.talking ? (
    <span className="flex items-center gap-2 text-xs font-semibold text-lamp">
      {source.encryption === "encrypted" ? <Lock className="size-3.5" aria-hidden /> : <LevelMeter active />}
      <span>{source.encryption === "encrypted" ? "Private call" : "Talking"}</span>
    </span>
  ) : source.signal ? (
    <span className="text-xs text-muted-foreground">Signal, no voice</span>
  ) : (
    <span className="tabular text-xs text-muted-foreground">
      {source.lastHeardAt ? relativeTime(source.lastHeardAt, now) : source.inRange ? "Not heard yet" : "Out of range"}
    </span>
  );

  return (
    <li
      className={cn("relative flex items-stretch", playing && "bg-secondary/60")}
      data-source-id={source.id}
    >
      {playing && <span className="absolute inset-y-2 left-0 w-1 rounded-r bg-signal" aria-hidden />}
      <button
        type="button"
        data-source-row
        className="flex min-w-0 flex-1 flex-col gap-1 py-3 pr-2 pl-4 text-left focus-visible:-outline-offset-2"
        onClick={() => {
          const long = longPressed.current;
          longPressed.current = false;
          if (long) return;
          if (source.listenable.ok || playing) void player.listen(source);
          else openSheet({ type: "source", id: source.id });
        }}
        onPointerDown={startPress}
        onPointerUp={endPress}
        onPointerLeave={endPress}
        onPointerCancel={endPress}
        onContextMenu={(e) => {
          e.preventDefault();
          openSheet({ type: "source", id: source.id });
        }}
      >
        <span className="sr-only">{playing ? "Stop listening to" : source.listenable.ok ? "Listen to" : "About"}</span>{" "}
        <span className="flex items-center gap-2">
          {followed && <Star className="size-3.5 shrink-0 fill-lamp text-lamp" aria-label="Following" />}
          <span
            className={cn(
              "min-w-0 flex-1 truncate text-[1.0625rem] leading-6",
              source.talking ? "font-semibold text-heading" : "text-foreground",
              !source.named && "tabular",
            )}
          >
            {source.name}
          </span>
          {right}
        </span>
        <span className="flex items-center gap-2 text-sm text-muted-foreground">
          <span className="min-w-0 flex-1 truncate">{playing ? "Listening on this device" : source.summary}</span>
        </span>
        <CallLane calls={calls} now={now} talking={source.talking} className="mt-1" />
      </button>
      <button
        type="button"
        className="grid w-12 shrink-0 place-items-center text-muted-foreground hover:text-heading"
        onClick={() => openSheet({ type: "source", id: source.id })}
        aria-label={`More about ${source.name}`}
      >
        <MoreHorizontal className="size-5" aria-hidden />
      </button>
    </li>
  );
}
