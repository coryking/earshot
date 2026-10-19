import React, { useEffect, useRef, useState } from "react";
import { z } from "zod";
import { createRoot } from "react-dom/client";
import {
  QueryClient,
  QueryClientProvider,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import {
  Radio,
  Play,
  Pause,
  Square,
  Headphones,
  Waves,
  Activity,
  Antenna,
  Volume2,
} from "lucide-react";
import { Button } from "./components/ui/button";
import {
  api,
  statusSchema,
  spectrumSchema,
  mhz,
  age,
  fallbackChannel,
  listenerSchema,
} from "./lib/radio";
import type { Channel, Spectrum, SourceConfig } from "./lib/radio";
import "./index.css";
import { LivePlayer } from "./lib/live-audio";

function Waterfall({
  spectrum,
  selected,
}: {
  spectrum: Spectrum | undefined;
  selected: number | undefined;
}) {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const width = (canvas.width = 1200),
      height = (canvas.height = 330);
    ctx.fillStyle = "#0b1625";
    ctx.fillRect(0, 0, width, height);
    if (!spectrum?.rows.length) return;
    const rows = spectrum.rows.slice(-200).reverse();
    const bins = rows[0].length;
    const img = ctx.createImageData(bins, rows.length);
    rows.forEach((row, y) =>
      row.forEach((db, x) => {
        const value = Math.max(
          0,
          Math.min(
            1,
            (db - spectrum.low_db) / (spectrum.high_db - spectrum.low_db),
          ),
        );
        const stops = [
          [10, 24, 43],
          [28, 60, 101],
          [38, 123, 153],
          [98, 181, 176],
          [243, 197, 109],
          [255, 244, 206],
        ];
        const n = value * (stops.length - 1),
          a = Math.floor(n),
          b = Math.min(a + 1, stops.length - 1),
          t = n - a;
        const offset = (y * bins + x) * 4;
        for (let c = 0; c < 3; c++)
          img.data[offset + c] = stops[a][c] * (1 - t) + stops[b][c] * t;
        img.data[offset + 3] = 255;
      }),
    );
    const layer = document.createElement("canvas");
    layer.width = bins;
    layer.height = rows.length;
    layer.getContext("2d")!.putImageData(img, 0, 0);
    ctx.imageSmoothingEnabled = false;
    ctx.drawImage(layer, 0, 0, width, height);
    ctx.strokeStyle = "#ffffff12";
    ctx.lineWidth = 1;
    for (let i = 1; i < 8; i++) {
      ctx.beginPath();
      ctx.moveTo((i * width) / 8, 0);
      ctx.lineTo((i * width) / 8, height);
      ctx.stroke();
    }
    if (selected) {
      const x =
        ((selected - spectrum.center_hz + spectrum.span_hz / 2) /
          spectrum.span_hz) *
        width;
      ctx.strokeStyle = "#ffae69";
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.moveTo(x, 0);
      ctx.lineTo(x, height);
      ctx.stroke();
      ctx.fillStyle = "#ffae69";
      ctx.beginPath();
      ctx.moveTo(x - 5, 0);
      ctx.lineTo(x + 5, 0);
      ctx.lineTo(x, 8);
      ctx.fill();
    }
  }, [spectrum, selected]);
  const center = spectrum?.center_hz ?? 454216998,
    span = spectrum?.span_hz ?? 6000000;
  return (
    <div className="waterfall">
      <div className="frequency-axis">
        {Array.from({ length: 7 }, (_, i) => (
          <span key={i}>
            {((center - span / 2 + (span * i) / 6) / 1e6).toFixed(3)}
          </span>
        ))}
      </div>
      <div className="waterfall-body">
        <canvas
          ref={ref}
          aria-label="Measured IQ spectrum waterfall; newest samples at top"
        />
        <div className="time-axis">
          <span>Newest</span>
          <span>
            {spectrum?.times.length
              ? `${spectrum.times.at(-1)?.toFixed(1)} s`
              : "No samples"}
          </span>
          <span>Older</span>
        </div>
        {!spectrum?.rows.length && (
          <div className="waterfall-empty">
            <Waves size={28} />
            <span>Connect a source to see the spectrum</span>
          </div>
        )}
      </div>
      <div className="waterfall-foot">
        <span>Frequency (MHz) · nominal tuning</span>
        <span className="scale">
          <i /> −125 to −65 dBFS/Hz
        </span>
      </div>
    </div>
  );
}

function App() {
  const client = useQueryClient();
  const status = useQuery({
    queryKey: ["status"],
    queryFn: () => api("status", statusSchema),
    refetchInterval: 650,
    retry: 1,
  });
  const spectrumBuffer = useRef<Spectrum | undefined>(undefined);
  const spectrum = useQuery({
    queryKey: ["spectrum"],
    queryFn: async () => {
      const previous = spectrumBuffer.current;
      const delta = await api(
        `spectrum?after=${previous?.sequence ?? 0}`,
        spectrumSchema,
      );
      const reset =
        !previous ||
        previous.generation !== delta.generation ||
        (previous.sequence ?? 0) > delta.sequence;
      const merged = {
        ...delta,
        rows: [...(reset ? [] : previous.rows), ...delta.rows].slice(-240),
        times: [...(reset ? [] : previous.times), ...delta.times].slice(-240),
      };
      spectrumBuffer.current = merged;
      return merged;
    },
    refetchInterval: 200,
    retry: 1,
  });
  const [bandwidth, setBandwidth] = useState(6000000);
  const pinnedFrequency = useRef<number | null>(null);
  const pinnedSlot = useRef<number | null>(null);
  const [uri, setUri] = useState("usb:32.18.5");
  const [selection, setSelection] = useState<string | null>(null);
  const [listening, setListening] = useState(false);
  const [audioError, setAudioError] = useState<string | null>(null);
  const [playing, setPlaying] = useState(false);
  const [inputAge, setInputAge] = useState<number | null>(null);
  const player = useRef<LivePlayer | null>(null);
  const listener = useQuery({
    queryKey: ["listener"],
    queryFn: () => api("listener", listenerSchema),
    refetchInterval: 400,
    retry: 1,
  });
  const d = status.data;
  const channels = d?.channels ?? [];
  const selected = channels.find((c) => c.id === selection);
  const connection = useMutation({
    mutationFn: (config: SourceConfig) => api("source", statusSchema, config),
    onSuccess: (result) => {
      client.setQueryData(["status"], result);
      setPlaying(false);
      setAudioError(null);
      player.current?.pause();
      client.invalidateQueries({ queryKey: ["status"] });
    },
  });
  const stop = useMutation({
    mutationFn: () => api("stop", statusSchema, {}),
    onSuccess: () => client.invalidateQueries({ queryKey: ["status"] }),
  });
  useEffect(() => {
    if (d?.mode === "live") setBandwidth(d.rate_hz);
  }, [d?.rate_hz, d?.mode]);
  const active = channels.filter((c) => c.speech_active).length;
  const connected = d?.state === "receiving";
  const isBusy =
    connection.isPending ||
    stop.isPending ||
    d?.state === "connecting" ||
    d?.state === "stopping";
  useEffect(() => {
    if (!channels.length) return;
    if (!selection || !channels.some((c) => c.id === selection))
      setSelection(
        channels.find(
          (c) =>
            c.frequency_hz === pinnedFrequency.current &&
            c.kind === "voice" &&
            (pinnedSlot.current === null || c.slot === pinnedSlot.current),
        )?.id ??
          fallbackChannel(channels, null) ??
          null,
      );
  }, [channels, selection]);
  useEffect(() => {
    if (!listening || !selected?.available) {
      player.current?.pause();
      return;
    }
    player.current?.play(selected.id, d?.generation ?? 0);
    void api("monitor", z.unknown(), { channel_id: selected.id }).catch(
      (error: Error) => setAudioError(error.message),
    );
  }, [listening, selected?.id, selected?.available, d?.generation]);
  useEffect(() => () => player.current?.close(), []);
  function enableAudio() {
    player.current ??= new LivePlayer((state) => {
      setPlaying(state.playing);
      setAudioError(state.error);
      setInputAge(state.inputAgeMs);
    });
    void player.current
      .enable()
      .catch((error: Error) => setAudioError(error.message));
  }
  function monitor(ch: Channel) {
    enableAudio();
    player.current?.flush();
    pinnedFrequency.current = ch.frequency_hz;
    pinnedSlot.current = ch.slot ?? null;
    setSelection(ch.id);
    setListening(true);
    setAudioError(null);
  }
  function toggleListen() {
    if (listening) {
      setListening(false);
      player.current?.pause();
    } else {
      enableAudio();
      setListening(true);
      setAudioError(null);
    }
  }
  return (
    <div className="console-shell">
      <header>
        <div className="brand">
          <div className="brand-icon">
            <Radio size={24} />
          </div>
          <div>
            <h1>Radio monitor</h1>
            <p>Local receive console</p>
          </div>
        </div>
        <div className={`source-indicator ${connected ? "connected" : ""}`}>
          <span />
          {status.isError
            ? "Backend offline"
            : d?.state === "receiving"
              ? "Live"
              : d?.state === "connecting"
                ? "Connecting"
                : d?.state === "stopping"
                  ? "Stopping"
                  : d?.state === "error"
                    ? "Source error"
                    : "Disconnected"}
        </div>
      </header>
      <section className="source-bar" aria-label="Receiver source">
        <div className="live-source-label">
          <Antenna size={16} /> Pluto live
        </div>
        <label className="uri">
          Receiver{" "}
          <input
            aria-label="Receiver URI"
            value={uri}
            onChange={(e) => setUri(e.target.value)}
          />
        </label>
        <label className="bandwidth-control">
          Receiver width
          <select
            aria-label="Receiver bandwidth"
            value={bandwidth}
            onChange={(e) => setBandwidth(Number(e.target.value))}
            disabled={isBusy}
          >
            {[1250000, 2500000, 5000000, 6000000].map((width) => (
              <option key={width} value={width}>
                {width / 1e6} MHz
              </option>
            ))}
          </select>
        </label>
        {connected && (
          <Button
            variant="outline"
            disabled={isBusy || bandwidth === d?.rate_hz}
            onClick={() =>
              connection.mutate({
                mode: "live",
                uri,
                center_hz: d?.center_hz ?? 454216998,
                rate_hz: bandwidth,
              })
            }
          >
            Apply width
          </Button>
        )}
        <Button
          disabled={isBusy}
          onClick={() =>
            connection.mutate({
              mode: "live",
              uri,
              center_hz: d?.center_hz ?? 454216998,
              rate_hz: bandwidth,
            })
          }
        >
          {connected ? "Reconnect" : "Connect"}
        </Button>
        <Button
          variant="outline"
          disabled={!connected && d?.state !== "connecting"}
          onClick={() => stop.mutate()}
        >
          <Square size={13} /> Stop
        </Button>
      </section>
      {(status.isError ||
        connection.error ||
        stop.error ||
        d?.error ||
        d?.decoder_error) && (
        <div role="alert" className="error-panel">
          {status.error?.message ||
            connection.error?.message ||
            stop.error?.message ||
            d?.error ||
            d?.decoder_error}
        </div>
      )}
      <main>
        <section className="spectrum-panel">
          <div className="panel-top">
            <div>
              <h2>Spectrum waterfall</h2>
              <p>{connected ? d?.source_name : "Receiver is idle"}</p>
            </div>
            <div className="spectrum-readout">
              <strong>
                {((d?.center_hz ?? 454216998) / 1e6).toFixed(3)}{" "}
                <small>MHz</small>
              </strong>
              <span>{(d?.rate_hz ?? 6000000) / 1e6} MHz span</span>
            </div>
          </div>
          <Waterfall
            spectrum={spectrum.data}
            selected={selected?.frequency_hz}
          />
          <div className="telemetry">
            <span>
              <Activity size={13} />
              {d?.received_seconds.toFixed(1) ?? "0.0"} s received
            </span>
            <span>
              {d?.decode_latency_s.toFixed(1) ?? "0.0"} s discovery lag
            </span>
            <span>{d?.dropped_batches ?? 0} batches skipped</span>
            <span>{d?.queue_depth ?? 0}/1 queued</span>
          </div>
        </section>
        <div className="lower-grid">
          <section className="channel-panel">
            <div className="panel-top">
              <div>
                <h2>
                  Channel catalog{" "}
                  <span className="count">{channels.length}</span>
                </h2>
                <p>Detected channels stay listed when quiet</p>
              </div>
            </div>
            <div className="channel-table">
              <div className="table-head">
                <span>Frequency / protocol</span>
                <span>Activity</span>
                <span>Last heard</span>
                <span />
              </div>
              {channels.length === 0 ? (
                <div className="empty-channels">
                  <Radio size={28} />
                  <strong>
                    {connected
                      ? "Listening across the band…"
                      : "Your channel catalog starts here"}
                  </strong>
                  <p>
                    {connected
                      ? "New RF channels appear as they are detected. Voice becomes playable after a successful decode."
                      : "Connect Pluto to discover and monitor live channels across the selected bandwidth."}
                  </p>
                </div>
              ) : (
                channels.map((ch) => (
                  <div
                    key={ch.id}
                    className={`channel-row ${selection === ch.id ? "selected" : ""}`}
                  >
                    <div className="channel-label">
                      <strong>
                        {mhz(ch.frequency_hz)}
                        <small> MHz</small>
                      </strong>
                      <span>
                        {ch.protocol}
                        {ch.slot ? ` · Slot ${ch.slot}` : ""}
                        {ch.kind === "data"
                          ? " · Control / data"
                          : ch.kind === "unidentified"
                            ? " · RF detected"
                            : ""}
                      </span>
                    </div>
                    <div>
                      <span
                        className={`activity-pill ${ch.speech_active ? "voice" : ch.rf_active ? "rf" : ""}`}
                      >
                        <i />
                        {ch.in_range === false
                          ? "Out of range"
                          : !ch.available
                            ? "Unavailable"
                            : ch.speech_active
                              ? "Voice"
                              : ch.rf_active
                                ? "RF present"
                                : "Idle"}
                      </span>
                    </div>
                    <span className="last-heard">{age(ch.last_heard)}</span>
                    <Button
                      variant={selection === ch.id ? "default" : "outline"}
                      size="sm"
                      onClick={() => monitor(ch)}
                      aria-label={`Monitor ${mhz(ch.frequency_hz)} MHz ${ch.protocol}${ch.slot ? ` slot ${ch.slot}` : ""}`}
                    >
                      {selection === ch.id ? (
                        <Headphones size={13} />
                      ) : (
                        <Play size={12} />
                      )}
                      <span>
                        {selection === ch.id ? "Monitoring" : "Monitor"}
                      </span>
                    </Button>
                  </div>
                ))
              )}
            </div>
          </section>
          <aside className="listen-panel">
            <div className="panel-top">
              <h2>
                <Headphones size={16} />
                Listening
              </h2>
              <span className="audio-status">Pinned frequency</span>
            </div>
            <div className="tuning-display">
              <div className="tuning-frequency">
                {selected ? mhz(selected.frequency_hz) : "— — —"}
                <span>MHz</span>
              </div>
              <p>
                {selected
                  ? `${selected.protocol}${selected.slot ? ` · Slot ${selected.slot}` : ""}`
                  : "Choose a channel to monitor"}
              </p>
              <div className={`signal-bars ${playing ? "moving" : ""}`}>
                {Array.from({ length: 32 }, (_, i) => (
                  <i
                    key={i}
                    style={{
                      height: `${6 + (Math.sin(i * 1.7) + 1) * 13}px`,
                      animationDelay: `${i * 31}ms`,
                    }}
                  />
                ))}
              </div>
              <div className="listening-state">
                <Volume2 size={14} />
                {selected?.in_range === false
                  ? "Outside receiver bandwidth"
                  : selected?.available === false
                    ? "Receiver unavailable · frequency retained"
                    : playing
                      ? "Playing decoded voice"
                      : !listening
                        ? "Audio paused"
                        : selected?.kind === "data"
                          ? "Control / data · no voice"
                          : "Armed · waiting for live decoded voice"}
              </div>
              <Button
                className="listen-button"
                disabled={!selected}
                onClick={toggleListen}
              >
                {listening ? <Pause size={17} /> : <Play size={17} />}{" "}
                {listening ? "Pause audio" : "Listen"}
              </Button>
              {audioError && (
                <p role="alert" className="audio-error">
                  {audioError}
                </p>
              )}
              {listener.data?.error &&
                listener.data.channel_id === selected?.id && (
                  <p role="alert" className="audio-error">
                    {listener.data.error}
                  </p>
                )}
              <p className="stream-metric">
                {inputAge !== null
                  ? `${inputAge.toFixed(0)} ms since latest decoder input block`
                  : "Continuous decoder · waiting for live voice"}
              </p>
            </div>
            <div className="scan-control">
              <p>
                Stay on this frequency through silence. All other channels keep
                decoding in parallel.
              </p>
            </div>
            <div className="session-info">
              <div>
                <span>Voice active</span>
                <strong>{active}</strong>
              </div>
              <div>
                <span>Decoded streams</span>
                <strong>{channels.filter((c) => c.decoded).length}</strong>
              </div>
              <div>
                <span>RF present</span>
                <strong>{channels.filter((c) => c.rf_active).length}</strong>
              </div>
            </div>
          </aside>
        </div>
        <footer>
          <span>
            <Radio size={13} />
            Receive only · local to this Mac
          </span>
          <span>
            Nominal frequencies · uncalibrated power · continuity unverified
          </span>
        </footer>
      </main>
    </div>
  );
}
const queryClient = new QueryClient({
  defaultOptions: { queries: { refetchOnWindowFocus: false } },
});
createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <App />
    </QueryClientProvider>
  </React.StrictMode>,
);
