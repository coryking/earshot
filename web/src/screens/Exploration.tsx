import { useMemo, useState } from "react";
import { List, Info, X } from "lucide-react";
import { usePlayer } from "@/app/player-context";
import { openSheet } from "@/app/ui";
import { IqCapture, LiveSelection, useIqCapture } from "@/components/Tuning";
import { SpectrumNavigation } from "@/components/SpectrumNavigation";
import { Waterfall } from "@/components/Waterfall";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ChannelIdentity } from "@/components/ChannelIdentity";
import { useBlocks, useSources, useStatus } from "@/state/radio";

export function Exploration() {
  const [player, state] = usePlayer();
  const sources = useSources();
  const { current, canSwitch } = useBlocks();
  const status = useStatus();
  const [band, setBand] = useState(false);
  const [browse, setBrowse] = useState(false);
  const [inspect, setInspect] = useState(false);
  const [search, setSearch] = useState("");
  const iq = useIqCapture(state.channelId);
  const range = iq.data?.center_hz != null && iq.data.sample_rate_hz != null
    ? { centerHz: iq.data.center_hz, spanHz: iq.data.sample_rate_hz } : null;
  const active = state.kind === "live" && state.phase !== "error" && state.phase !== "taken";
  const source = sources.find((s) => s.id === state.sourceId);
  const channel = status.data?.channels.find((c) => c.id === state.channelId);
  const filtered = useMemo(() => sources.filter((s) => s.inRange &&
    `${s.name} ${(s.frequencyHz ?? 0) / 1e6} ${s.mode}`.toLowerCase().includes(search.toLowerCase()))
    .sort((a, b) => (a.frequencyHz ?? 0) - (b.frequencyHz ?? 0) || (a.slot ?? 0) - (b.slot ?? 0)), [sources, search]);
  return <div className="flex min-h-0 min-w-0 flex-1 flex-col" onKeyDown={(e) => {
    if (e.key === "Escape" && (browse || inspect || band)) { e.stopPropagation(); setBrowse(false); setInspect(false); setBand(false); }
  }}>
    <div className="relative flex shrink-0 items-center gap-2 px-3 py-1">
      <h1 className="min-w-0 flex-1 truncate text-sm font-medium text-heading">
        {canSwitch ? <button className="min-h-10" onClick={() => openSheet({ type: "block" })} aria-label={`Listening area: ${current?.name}. Change area`}>{current?.name} ▾</button> : <button className="min-h-10" aria-label={`${current?.name ?? "Spectrum"}. Receiver band`} aria-expanded={band} onClick={() => setBand(!band)}>{current?.name ?? "Spectrum"} ▾</button>}
      </h1>
      <Button variant={browse ? "secondary" : "ghost"} size="sm" aria-expanded={browse} onClick={() => setBrowse(!browse)}><List className="size-4" />Channels</Button>
      <Button variant={inspect ? "secondary" : "ghost"} size="sm" disabled={state.frequencyHz === null} aria-expanded={inspect} onClick={() => setInspect(!inspect)}><Info className="size-4" />Signal</Button>
    </div>
    {band && <div className="absolute top-28 left-3 z-20 max-w-[calc(100%-1.5rem)] rounded-lg border border-border bg-card p-2 shadow-xl"><SpectrumNavigation /></div>}
    <LiveSelection />
    <div className="relative flex min-h-0 flex-1">
      <Waterfall variant="full" sources={sources} talking={new Map()} activeSourceId={active ? state.sourceId : null}
        tunedFrequencyHz={state.frequencyHz} captureRange={range}
        onListen={(s) => void player.listen(s, { raw: !s.heardVoice || s.control || s.encryption === "encrypted" })} onUnnamed={(hz) => void player.listenFrequency(hz)} className="min-w-0 flex-1 rounded-none" />
      {browse && <aside aria-label="Channels" className="absolute inset-y-0 left-0 z-10 flex w-80 max-w-full flex-col border-r border-border bg-background/95 shadow-xl backdrop-blur">
        <div className="flex items-center gap-2 p-2"><Input aria-label="Search channels" placeholder="Name, frequency, protocol" value={search} onChange={(e) => setSearch(e.target.value)} /><Button variant="ghost" size="icon" aria-label="Close channels" onClick={() => setBrowse(false)}><X /></Button></div>
        <div className="min-h-0 flex-1 overflow-y-auto" role="list" data-source-list aria-label="Channel frequencies">{filtered.map((s) => <div role="listitem" key={s.id} data-source-id={s.id}>
          <button data-source-row className={`min-h-14 w-full border-b border-border px-3 py-2 text-left hover:bg-secondary ${s.id === state.sourceId ? "bg-secondary" : ""}`}
            aria-label={`Tune to ${s.name}, ${(s.frequencyHz ?? 0) / 1e6} MHz, ${s.mode.toUpperCase()}${s.slot ? `, Slot ${s.slot}` : ""}`}
            onClick={() => { void player.listen(s, { raw: !s.heardVoice || s.control || s.encryption === "encrypted" }); setBrowse(false); }}><ChannelIdentity {...s} name={s.named ? s.name : undefined} /></button>
        </div>)}</div>
      </aside>}
      {inspect && <aside aria-label="Signal details" className="absolute inset-y-0 right-0 z-10 w-80 max-w-full overflow-y-auto border-l border-border bg-background/95 p-3 shadow-xl backdrop-blur">
        <div className="mb-4 flex items-center justify-between"><ChannelIdentity name={source?.named ? source.name : undefined} frequencyHz={state.frequencyHz} mode={source?.mode} slot={source?.slot} /><Button variant="ghost" size="icon" aria-label="Close signal details" onClick={() => setInspect(false)}><X /></Button></div>
        <dl className="grid grid-cols-2 gap-3 text-sm"><dt className="text-muted-foreground">Protocol</dt><dd>{channel?.protocol && channel.protocol !== "Unknown" ? channel.protocol : "Unidentified"}</dd>
          <dt className="text-muted-foreground">RF activity</dt><dd>{channel?.rf_active == null ? "Unknown" : channel.rf_active ? "Present" : "Quiet"}</dd>
          <dt className="text-muted-foreground">Frame decoded</dt><dd>{channel?.decoded == null ? "Unknown" : channel.decoded ? "Yes" : "No"}</dd>
          <dt className="text-muted-foreground">Speech</dt><dd>{channel?.speech_active == null ? "Unknown" : channel.speech_active ? "Active" : "None"}</dd>
        </dl>
        {source?.named && <Button className="mt-4" variant="outline" onClick={() => openSheet({ type: "source", id: source.id })}>About channel</Button>}
      </aside>}
    </div>
    {state.channelId && <div className="flex shrink-0 flex-wrap items-center gap-3 border-t border-border px-3 py-2">
      <IqCapture channelId={state.channelId} frequencyHz={state.frequencyHz} />
      {state.captureSeconds > 0 && <Button size="sm" variant="ghost" onClick={() => player.saveCapture()}>Save audio · {Math.round(state.captureSeconds)} s</Button>}
    </div>}
  </div>;
}
