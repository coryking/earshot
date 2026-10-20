import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft } from "lucide-react";
import { useState, type ReactNode } from "react";
import { toast } from "sonner";
import type { WireCatalogEntry } from "@/api/needs";
import { navigate, useNow } from "@/app/ui";
import { useSaveLabel } from "@/components/Sheets";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { formatMHz } from "@/domain/adapt";
import type { Need } from "@/backend/types";
import { relativeTime } from "@/domain/summary";
import { keys, useBackend, useCatalog, useStatus, useSupports } from "@/state/radio";
import { useSpectrumMeta } from "@/waterfall/feed";

function Panel({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="rounded-xl border border-border bg-card p-4" aria-label={title}>
      <h2 className="mb-3 text-base font-semibold text-heading">{title}</h2>
      {children}
    </section>
  );
}

function KV({ rows }: { rows: [string, ReactNode][] }) {
  return (
    <dl className="tabular grid grid-cols-[minmax(8rem,auto)_1fr] gap-x-4 gap-y-1 text-sm">
      {rows.map(([k, v]) => (
        <div key={k} className="contents">
          <dt className="text-muted-foreground">{k}</dt>
          <dd className="break-all text-foreground">{v ?? "—"}</dd>
        </div>
      ))}
    </dl>
  );
}

const needNames: Record<Need, string> = {
  blocks: "GET /api/blocks (plain-named listening areas)",
  catalog: "GET/PUT /api/catalog (source names)",
  calls: "GET /api/calls + call audio (lookback)",
  activity: "GET /api/activity (busy vs normal history)",
  spectrumBinary: "GET /api/spectrum.bin (compact RSP1 rows)",
};

/** Technical controls and raw state, for the operator. */
export function OperatorScreen() {
  const status = useStatus();
  const supports = useSupports();
  const backend = useBackend();
  const client = useQueryClient();
  const spectrum = useSpectrumMeta();
  const now = useNow(1000);
  const s = status.data;
  const [center, setCenter] = useState("");
  const [rate, setRate] = useState("");
  const [width, setWidth] = useState("");

  const mhz = (text: string) => {
    const value = Number(text);
    return text.trim() !== "" && Number.isFinite(value) && value > 0 ? value * 1e6 : null;
  };
  const centerHz = mhz(center);
  const rateHz = mhz(rate);
  const widthHz = width.trim() === "" ? rateHz : mhz(width);
  const valid = centerHz !== null && rateHz !== null && widthHz !== null;

  const configure = useMutation({
    mutationFn: () => backend.configureSource({ centerHz: centerHz!, rateHz: rateHz!, bandwidthHz: widthHz! }),
    onSuccess: (message) => {
      toast.success(message);
      void client.invalidateQueries({ queryKey: keys.status });
    },
    onError: (e) => toast.error(e instanceof Error ? e.message : "Configure failed"),
  });
  const stop = useMutation({
    mutationFn: () => backend.stopReceiver(),
    onSuccess: (message) => {
      toast.success(message);
      void client.invalidateQueries({ queryKey: keys.status });
    },
    onError: (e) => toast.error(e instanceof Error ? e.message : "Stop failed"),
  });

  return (
    <div className="mx-auto max-w-5xl space-y-4 px-4 pt-3 pb-8">
      <div className="flex items-center gap-2">
        <Button variant="ghost" size="icon" className="size-11" onClick={() => navigate("radio")} aria-label="Back to Radio">
          <ArrowLeft className="size-5" />
        </Button>
        <h1 className="text-2xl font-semibold text-heading">Operator tools</h1>
      </div>
      <div className="grid gap-4 lg:grid-cols-2">
        <Panel title="Engine">
          {status.isError && <p className="mb-2 text-sm text-destructive">Status request failed: {String(status.error)}</p>}
          <KV
            rows={[
              ["mode / state", s ? `${s.mode} / ${s.state}` : null],
              ["error", s?.error ?? "none"],
              ["source", s?.source_name],
              ["center", s?.center_hz ? `${formatMHz(s.center_hz, 6)} MHz` : "not reported yet"],
              ["rate / bandwidth", s?.rate_hz ? `${s.rate_hz} / ${s.bandwidth_hz || "?"} Hz` : "not reported yet"],
              ["generation", s?.generation],
              ["decoders", s ? `${String(s.source?.active_decoders ?? "?")} of ${s.decoder_capacity ?? "?"}` : null],
              ["queue depth", s?.queue_depth],
              ["processed / dropped", s ? `${s.processed_batches ?? 0} / ${s.dropped_batches ?? 0}` : null],
              ["last samples", s?.last_samples_at ? relativeTime(s.last_samples_at * 1000, now) : "never"],
              ["source position", s?.source_position_s != null ? `${s.source_position_s.toFixed(1)} s` : null],
              ["continuity", s?.continuity],
              ["spectrum", spectrum.bins ? `${spectrum.bins} bins, generation ${spectrum.generation}, ${spectrum.unit}` : "no rows"],
            ]}
          />
        </Panel>
        <Panel title="Receiver">
          <form
            className="grid grid-cols-3 gap-2"
            onSubmit={(e) => {
              e.preventDefault();
              configure.mutate();
            }}
          >
            <label className="text-sm text-muted-foreground">
              Center MHz
              <Input value={center} onChange={(e) => setCenter(e.target.value)} inputMode="decimal" placeholder="454.217" />
            </label>
            <label className="text-sm text-muted-foreground">
              Rate Msps
              <Input value={rate} onChange={(e) => setRate(e.target.value)} inputMode="decimal" placeholder="6" />
            </label>
            <label className="text-sm text-muted-foreground">
              Width MHz
              <Input value={width} onChange={(e) => setWidth(e.target.value)} inputMode="decimal" placeholder="6" />
            </label>
            <Button type="submit" variant="secondary" className="col-span-3 h-10" disabled={!valid || configure.isPending}>
              Apply (waits for the producer's acknowledgement)
            </Button>
          </form>
          <Dialog>
            <DialogTrigger asChild>
              <Button variant="outline" className="mt-3 h-10 w-full border-destructive/60 text-destructive hover:bg-destructive/10">
                Stop receiver…
              </Button>
            </DialogTrigger>
            <DialogContent>
              <DialogHeader>
                <DialogTitle>Stop the receiver?</DialogTitle>
                <DialogDescription>
                  Stops host audio for everyone and asks the producer to stop RX and restore its settings. Nothing is
                  heard until it's started again.
                </DialogDescription>
              </DialogHeader>
              <DialogFooter>
                <DialogClose asChild>
                  <Button variant="outline">Cancel</Button>
                </DialogClose>
                <DialogClose asChild>
                  <Button variant="outline" className="border-destructive/60 text-destructive" onClick={() => stop.mutate()}>
                    Stop receiver
                  </Button>
                </DialogClose>
              </DialogFooter>
            </DialogContent>
          </Dialog>
          <details className="mt-3 text-sm">
            <summary className="cursor-pointer text-muted-foreground">Producer CONFIG readback</summary>
            <pre className="mt-2 max-h-64 overflow-auto rounded-lg bg-secondary p-2 text-xs">
              {JSON.stringify(s?.source ?? {}, null, 2)}
            </pre>
          </details>
        </Panel>
      </div>
      <Panel title="Frontend needs">
        <ul className="space-y-1 text-sm">
          {(Object.keys(needNames) as Need[]).map((need) => (
            <li key={need} className="flex gap-2">
              <span className={supports.data?.[need] ? "text-ok" : "text-muted-foreground"}>
                {supports.data ? (supports.data[need] ? "engine" : "fallback") : "…"}
              </span>
              <span>{needNames[need]}</span>
            </li>
          ))}
        </ul>
        <p className="mt-2 text-xs text-muted-foreground">Contract: engine/README.md § Frontend needs.</p>
      </Panel>
      <Panel title="Channels">
        <div className="overflow-x-auto" tabIndex={0} role="region" aria-label="Channel table">
          <table className="tabular w-full text-left text-sm">
            <thead className="text-muted-foreground">
              <tr>
                {["id", "stream", "MHz", "proto", "slot", "rf", "decoded", "speech", "level", "last heard", "admitted", "in range"].map((h) => (
                  <th key={h} className="px-2 py-1 font-normal whitespace-nowrap">
                    {h}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {(s?.channels ?? []).map((c) => (
                <tr key={c.id} className="border-t border-border/60">
                  <td className="px-2 py-1 whitespace-nowrap">{c.id}</td>
                  <td className="px-2 py-1">{c.stream_id ?? "—"}</td>
                  <td className="px-2 py-1">{formatMHz(c.frequency_hz, 4)}</td>
                  <td className="px-2 py-1">{c.protocol}</td>
                  <td className="px-2 py-1">{c.slot ?? "—"}</td>
                  <td className="px-2 py-1">{flag(c.rf_active)}</td>
                  <td className="px-2 py-1">{flag(c.decoded)}</td>
                  <td className="px-2 py-1">{flag(c.speech_active)}</td>
                  <td className="px-2 py-1">{c.level_db != null ? c.level_db.toFixed(0) : "—"}</td>
                  <td className="px-2 py-1 whitespace-nowrap">{c.last_heard ? relativeTime(c.last_heard * 1000, now) : "—"}</td>
                  <td className="px-2 py-1">{flag(c.decoder_admitted)}</td>
                  <td className="px-2 py-1">{flag(c.in_range)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </Panel>
      <CatalogEditor />
    </div>
  );
}

function flag(v: boolean | null | undefined) {
  return v === true ? "yes" : v === false ? "no" : "—";
}

function CatalogEditor() {
  const { entries, onDevice } = useCatalog();
  const save = useSaveLabel();
  return (
    <Panel title="Names (catalog)">
      {onDevice && (
        <p className="mb-2 text-sm text-muted-foreground">
          The engine has no catalog store yet, so names are saved on this device only.
        </p>
      )}
      {entries.length === 0 && <p className="text-sm text-muted-foreground">No names yet. Name channels from their sheet.</p>}
      <ul className="space-y-3">
        {entries.map((e) => (
          <CatalogRow key={e.id} entry={e} onSave={(next) => save.mutate({ entry: next })} />
        ))}
      </ul>
    </Panel>
  );
}

function CatalogRow({ entry, onSave }: { entry: WireCatalogEntry; onSave(entry: WireCatalogEntry): void }) {
  const [draft, setDraft] = useState(entry);
  const field = (key: "name" | "description" | "probable_user" | "traffic_kind", label: string) => (
    <label className="text-xs text-muted-foreground">
      {label}
      <Input value={draft[key] ?? ""} onChange={(ev) => setDraft({ ...draft, [key]: ev.target.value })} className="h-9" />
    </label>
  );
  return (
    <li className="grid gap-2 rounded-lg border border-border/60 p-3 sm:grid-cols-2">
      {field("name", "Name")}
      {field("description", "One-line description")}
      {field("probable_user", "Probably used by")}
      {field("traffic_kind", "What you'll hear")}
      <label className="text-xs text-muted-foreground">
        Encryption
        <select
          value={draft.encryption}
          onChange={(ev) => setDraft({ ...draft, encryption: ev.target.value as WireCatalogEntry["encryption"] })}
          className="mt-0.5 h-9 w-full rounded-md border border-input bg-transparent px-2 text-sm text-foreground"
        >
          {["unknown", "clear", "encrypted", "mixed"].map((v) => (
            <option key={v} value={v}>
              {v}
            </option>
          ))}
        </select>
      </label>
      <div className="flex items-end justify-between gap-2">
        <span className="tabular truncate text-xs text-muted-foreground">{entry.channel_ids.join(", ")}</span>
        <Button size="sm" variant="secondary" onClick={() => onSave(draft)} disabled={JSON.stringify(draft) === JSON.stringify(entry)}>
          Save
        </Button>
      </div>
    </li>
  );
}
