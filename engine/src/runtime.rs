use crate::{
    codec::Decoder,
    dsp::{ChannelDsp, ChannelSpec, SliceMeter, TargetBand},
    fm::{FmAudio, Squelch},
    freq::{Calibration, Labels, label_fields, snap},
    store::Store,
    wire::{DISCONTINUITY, Frame, OVERFLOW, PREROLL, START},
};
use anyhow::{Context, Result};
use num_complex::Complex32;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::Read,
    net::TcpStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::broadcast;
fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}
/// Everything `App` needs at startup.
pub struct AppConfig {
    pub max_decoders: usize,
    /// ppm the Pluto reads high.
    pub ppm: f64,
    pub labels: Labels,
    /// Where calls, activity and the catalog persist; `None` keeps them in memory only.
    pub state_dir: Option<std::path::PathBuf>,
    pub call_memory_mb: usize,
}
impl AppConfig {
    pub fn new(max_decoders: usize) -> Self {
        Self {
            max_decoders,
            ppm: 0.,
            labels: Labels::default(),
            state_dir: None,
            call_memory_mb: 64,
        }
    }
}
#[derive(Clone)]
pub struct App {
    pub(crate) inner: Arc<Mutex<Engine>>,
    pub(crate) audio: broadcast::Sender<Audio>,
    pub(crate) events: broadcast::Sender<Value>,
    pub(crate) max_decoders: usize,
    /// ppm correction from the Pluto's readings to true frequencies.
    pub(crate) calibration: Calibration,
    pub(crate) labels: Arc<Labels>,
}
#[derive(Clone)]
pub(crate) struct Audio {
    pub(crate) meta: Value,
    pub(crate) pcm: Vec<u8>,
    pub(crate) created: Instant,
}
pub(crate) struct Engine {
    pub(crate) store: Store,
    pub(crate) source: Value,
    pub(crate) channels: HashMap<String, Value>,
    worker_epochs: HashMap<u64, u64>,
    pub(crate) selected_at: Instant,
    pub(crate) control: Option<mpsc::SyncSender<Value>>,
    pub(crate) selected: Option<String>,
    pub(crate) generation: u64,
    pub(crate) decoder_generation: u64,
    pub(crate) listener: Value,
    pub(crate) psd: VecDeque<(u64, f64, Vec<f32>)>,
    pub(crate) psd_seq: u64,
    pub(crate) started: Option<f64>,
    pub(crate) error: Option<String>,
    pub(crate) frames: u64,
    pub(crate) drops: u64,
    pub(crate) active: bool,
}
impl App {
    pub fn new(max_decoders: usize) -> Self {
        Self::configured(AppConfig::new(max_decoders))
    }
    pub fn configured(config: AppConfig) -> Self {
        let AppConfig {
            max_decoders,
            ppm,
            labels,
            state_dir,
            call_memory_mb,
        } = config;
        let (audio, _) = broadcast::channel(16);
        let (events, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(Mutex::new(Engine {
                store: Store::new(state_dir, call_memory_mb),
                source: json!({}),
                channels: HashMap::new(),
                worker_epochs: HashMap::new(),
                selected: None,
                selected_at: Instant::now(),
                control: None,
                generation: 0,
                decoder_generation: 0,
                listener: json!({"status":"stopped","state":"stopped","voice_active":false,"audio_chunks":0,"sequence":0,"dropped_blocks":0,"input_dropped":0,"queue_blocks":0,"error":null}),
                psd: VecDeque::new(),
                psd_seq: 0,
                started: None,
                error: None,
                frames: 0,
                drops: 0,
                active: false,
            })),
            audio,
            events,
            max_decoders,
            calibration: Calibration { ppm },
            labels: Arc::new(labels),
        }
    }
    /// The calibrated, raster-snapped frequency for a device reading, and whether it snapped.
    pub(crate) fn display_frequency(&self, reading_hz: f64) -> (f64, bool) {
        snap(self.calibration.true_hz(reading_hz))
    }
    pub fn error(&self, msg: String) {
        let mut e = self.inner.lock().unwrap();
        e.error = Some(msg);
        e.active = false;
        e.decoder_generation += 1;
        e.control = None;
        e.worker_epochs.clear();
        e.source["active_decoders"] = json!(0);
        e.source["queue_depth"] = json!(0);
        e.listener["voice_active"] = json!(false);
        e.listener["error"] = json!(e.error);
        e.listener["status"] = json!("error");
        e.listener["state"] = json!("error");
        let _=self.events.send(json!({"type":"source_state","state":"error","error":e.error,"generation":e.generation}));
        for c in e.channels.values_mut() {
            c["available"] = json!(false);
            c["decoder_admitted"] = json!(false);
            c["rf_active"] = json!(false);
            c["slice_rf_active"] = json!(false);
            c["target_rf_active"] = json!(false);
            c["speech_active"] = json!(false);
        }
    }
}
struct Work {
    frame: Arc<Frame>,
    received: Instant,
}
struct Worker {
    sender: mpsc::SyncSender<Work>,
    stream: u32,
    queued: Arc<AtomicUsize>,
    cost: Arc<AtomicUsize>,
}
fn selected_logical(e: &Engine) -> Option<u64> {
    e.selected
        .as_deref()
        .and_then(|id| e.channels.get(id))
        .and_then(|c| c["logical_hz"].as_f64())
        .map(|v| v.round() as u64)
}
fn channel_id(frequency: f64, protocol: &str, slot: u8) -> String {
    format!("{frequency:.0}-{protocol}-{slot}")
}
/// `rf_active` is what a listener sees for this exact frequency. On a slice shared by
/// several logical targets it needs both the slice (physical) activity and the
/// target's own in-band activity; on a one-target stream it is the slice activity.
fn refresh_rf(c: &mut Value) {
    let slice = c["slice_rf_active"] == true;
    let shared = c["slice_targets"].as_u64().unwrap_or(1) > 1;
    let own = c["target_rf_active"] == true;
    c["rf_active"] = json!(slice && (!shared || own));
    c["level_db"] = if shared && c["target_level_db"].is_number() {
        c["target_level_db"].clone()
    } else {
        c["slice_level_db"].clone()
    };
}
fn make_channel(app: &App, stream: u32, reading: f64, protocol: &str, slot: u8) -> Value {
    let (frequency, snapped) = app.display_frequency(reading);
    let mut channel = make_channel_json(stream, reading, frequency, snapped, protocol, slot);
    if let (Some(fields), Some(extra)) = (
        channel.as_object_mut(),
        label_fields(app.labels.lookup(app.calibration.true_hz(reading))).as_object(),
    ) {
        fields.extend(extra.clone());
    }
    channel
}
fn make_channel_json(stream: u32, reading: f64, frequency: f64, snapped: bool, protocol: &str, slot: u8) -> Value {
    json!({"id":channel_id(frequency,protocol,slot),"stream_id":stream,"frequency_hz":frequency,"frequency_snapped":snapped,"logical_hz":reading,"reading_hz":reading,"carrier_offset_hz":null,"protocol":protocol,"slot":slot,"kind":"unidentified","rf_active":false,"slice_rf_active":false,"slice_level_db":null,"slice_targets":1,"target_rf_active":null,"target_level_db":null,"decoded":false,"speech_active":false,"last_heard":null,"last_rf":null,"level_db":null,"audio_seconds":0.,"sequence":0,"in_range":true,"available":false})
}
/// Whether a logical target's channel lies inside its physical slice. Without a declared
/// slice rate and center there is nothing to check.
fn fits_slice(cfg: &Value, frequency: f64) -> bool {
    let rate = cfg["sample_rate_num_hz"]
        .as_f64()
        .zip(cfg["sample_rate_den"].as_f64())
        .map(|(n, d)| n / d)
        .or_else(|| cfg["rate_hz"].as_f64());
    let band = spec_for(cfg["protocol"].as_str().unwrap_or("Unknown")).band_hz;
    match (rate, cfg["center_hz"].as_f64()) {
        (Some(rate), Some(center)) => (frequency - center).abs() + band / 2. <= rate / 2.,
        _ => true,
    }
}
/// RIQ1 interleaved signed16 I/Q payload to complex samples.
fn iq_from_payload(payload: &[u8]) -> Vec<Complex32> {
    payload
        .chunks_exact(4)
        .map(|b| {
            Complex32::new(
                i16::from_le_bytes([b[0], b[1]]) as f32,
                i16::from_le_bytes([b[2], b[3]]) as f32,
            )
        })
        .collect()
}
fn spec_for(protocol: &str) -> ChannelSpec {
    if protocol == "NXDN48" {
        ChannelSpec::CHANNEL_6_25
    } else {
        ChannelSpec::CHANNEL_12_5
    }
}
/// Decoder instances a stream needs: one DSDcc instance per protocol. A DMR instance
/// decodes both TDMA slots, so a DMR channel costs one.
fn modes(cfg: &Value) -> Vec<&'static str> {
    match cfg["protocol"].as_str() {
        Some("DMR") => vec!["DMR"],
        Some("NXDN48") => vec!["NXDN48"],
        _ => vec!["DMR", "NXDN48"],
    }
}
static WORKER_EPOCH: AtomicU64 = AtomicU64::new(1);
fn worker(app: App, logical: u64, cfg: Value) -> Result<Worker> {
    let stream = cfg["stream_id"].as_u64().context("stream_id missing")? as u32;
    let epoch = WORKER_EPOCH.fetch_add(1, Ordering::Relaxed);
    {
        let mut e = app.inner.lock().unwrap();
        e.worker_epochs.insert(logical, epoch);
        if selected_logical(&e) == Some(logical) {
            e.decoder_generation += 1;
        }
    }
    let modes = modes(&cfg);
    let cost = Arc::new(AtomicUsize::new(modes.len()));
    let worker_cost = cost.clone();
    let queued = Arc::new(AtomicUsize::new(0));
    let worker_queued = queued.clone();
    let (tx, rx) = mpsc::sync_channel::<Work>(4);
    thread::spawn(move || {
        let mut decoders = Vec::new();
        let mut dsps: Vec<(ChannelSpec, ChannelDsp)> = Vec::new();
        let analog_capable = modes.iter().any(|&p| p != "NXDN48");
        let mut fm = FmAudio::new();
        let mut squelch = Squelch::default();
        let mut fm_run = 0u32;
        let mut digital_hold = 0f32;
        let mut generation = None;

        let mut expected_sample = None;
        let mut live_samples = 0u64;
        let mut saw_preroll = false;
        let mut rate = 0;
        while let Ok(work) = rx.recv() {
            worker_queued.fetch_sub(1, Ordering::Relaxed);
            let frame = &work.frame;
            let mut first_sample = frame.first_sample;
            let mut payload = frame.payload.as_slice();
            if generation == Some(frame.generation) && frame.flags & (DISCONTINUITY | OVERFLOW) == 0
                && let Some(previous) = expected_sample
                    && first_sample < previous {
                        let trim = ((previous - first_sample) as usize).min(payload.len() / 4);
                        payload = &payload[trim * 4..];
                        first_sample += trim as u64;
                        if payload.is_empty() {
                            continue;
                        }
                    }
            let frequency = cfg["target_hz"].as_f64().unwrap_or(frame.center_hz as f64);
            let gap = generation.is_some()
                && (generation != Some(frame.generation)
                    || frame.rate != rate
                    || frame.flags & (DISCONTINUITY | OVERFLOW | START) != 0
                    || expected_sample.is_some_and(|v| v != first_sample));
            if gap || decoders.is_empty() {
                decoders = modes
                    .iter()
                    .filter_map(|&p| Decoder::new(p).ok().map(|d| (p, d)))
                    .collect();
                let input_rate = cfg["sample_rate_num_hz"]
                    .as_f64()
                    .zip(cfg["sample_rate_den"].as_f64())
                    .filter(|(_, den)| *den > 0.)
                    .map(|(num, den)| num / den)
                    .unwrap_or(frame.rate as f64);
                // One channel filter per channel class: NXDN48 (6.25 kHz) must not
                // share the 12.5 kHz filter or its 6.25 kHz neighbors leak in.
                fm = FmAudio::new();
                squelch = Squelch::default();
                fm_run = 0;
                digital_hold = 0.;
                dsps.clear();
                for &protocol in &modes {
                    let spec = spec_for(protocol);
                    if !dsps.iter().any(|(s, _)| *s == spec) {
                        dsps.push((
                            spec,
                            ChannelDsp::new(input_rate, frequency - frame.center_hz as f64, spec),
                        ));
                    }
                }
                generation = Some(frame.generation);
                rate = frame.rate;
                live_samples = 0;
                saw_preroll = false;
                let mut e = app.inner.lock().unwrap();
                if gap {
                    if frame.flags & START != 0 && frame.flags & (DISCONTINUITY | OVERFLOW) == 0 {
                        e.source["intentional_gate_restarts"] =
                            json!(e.source["intentional_gate_restarts"].as_u64().unwrap_or(0) + 1);
                    } else {
                        e.drops += 1;
                    }
                    if selected_logical(&e) == Some(logical) {
                        e.decoder_generation += 1;
                    }
                }
            }

            expected_sample = Some(first_sample + payload.len() as u64 / 4);
            let iq = iq_from_payload(payload);
            let mut outputs: Vec<(ChannelSpec, Vec<i16>)> = Vec::new();
            for (spec, dsp) in &mut dsps {
                if decoders.iter().any(|(p, _)| spec_for(p) == *spec) {
                    outputs.push((*spec, dsp.push(&iq)));
                }
            }
            let produced = outputs.first().map_or(0, |(_, pcm)| pcm.len());
            if frame.flags & PREROLL != 0 {
                live_samples = 0;
                saw_preroll = true;
            } else {
                live_samples += produced as u64;
            }
            let mut protocol_evidence = None;
            // Everything heard in this block: (protocol, slot, 8 kHz audio, confirmed voice).
            let mut heard: Vec<(&'static str, u8, Vec<i16>, bool)> = Vec::new();
            let mut digital_sync = false;
            for (protocol, decoder) in &mut decoders {
                let spec = spec_for(protocol);
                let Some((_, pcm)) = outputs.iter().find(|(s, _)| *s == spec) else {
                    continue;
                };
                let decoded = decoder.push(pcm);
                digital_sync |= decoded.sync;
                let confirmed = decoded.confirmed;
                if *protocol == "DMR" {
                    heard.push(("DMR", 1, decoded.slot1, confirmed));
                    heard.push(("DMR", 2, decoded.slot2, confirmed));
                } else {
                    heard.push((protocol, 1, decoded.slot1, confirmed));
                }
            }
            // Analog FM shares the 12.5 kHz channel filter's discriminator. It is muted while
            // DMR/NXDN signalling is present (3 s hold) and needs a quieted carrier with voice
            // energy for three consecutive blocks before the channel is called analog voice.
            if analog_capable
                && let Some((_, pcm)) = outputs.iter().find(|(s, _)| *s == ChannelSpec::CHANNEL_12_5)
            {
                let seconds = pcm.len() as f32 / 48000.;
                digital_hold = if digital_sync { 3. } else { (digital_hold - seconds).max(0.) };
                let block = fm.push(pcm);
                let open = squelch.update(&block) && digital_hold <= 0.;
                fm_run = if open { fm_run + 1 } else { 0 };
                let audio = if open { block.audio } else { Vec::new() };
                heard.push(("FM", 1, audio, fm_run >= 3));
            }
            for (protocol, slot, audio, confirmed) in heard {
                let speech = confirmed && !audio.is_empty() && frame.flags & PREROLL == 0;
                let id = channel_id(app.display_frequency(frequency).0, protocol, slot);
                let mut e = app.inner.lock().unwrap();
                if frame.generation != e.generation
                    || !e.active
                    || e.worker_epochs.get(&logical) != Some(&epoch)
                {
                    continue;
                }
                let physical = e
                    .channels
                    .values()
                    .find(|c| c["stream_id"] == stream && c["slice_rf_active"] == true)
                    .cloned();
                let sibling = e
                    .channels
                    .values()
                    .find(|c| {
                        c["stream_id"] == stream
                            && c["id"] != id.as_str()
                            && c["logical_hz"].as_f64().map(|v| v.round() as u64) == Some(logical)
                    })
                    .cloned();
                let speech = speech && physical.is_some();
                if confirmed && !audio.is_empty() && physical.is_some() {
                    if protocol != "FM" {
                        protocol_evidence = Some(protocol);
                    }
                    let entry = e
                        .channels
                        .entry(id.clone())
                        .or_insert_with(|| make_channel(&app, stream, frequency, protocol, slot));
                    if let Some(sibling) = &sibling {
                        for key in [
                            "slice_rf_active",
                            "slice_level_db",
                            "slice_targets",
                            "target_rf_active",
                            "target_level_db",
                            "rf_active",
                            "last_rf",
                            "level_db",
                            "carrier_offset_hz",
                            "carrier_hz",
                            "carrier_snapped",
                            "frequency_hz",
                            "frequency_snapped",
                            "label",
                            "label_kind",
                            "label_detail",
                        ] {
                            entry[key] = sibling[key].clone();
                        }
                    }
                    entry["decoded"] = json!(true);
                    entry["kind"] = json!("voice");
                    entry["last_heard"] = json!(now());
                    entry["available"] = json!(true);
                    let changed = entry["speech_active"] != speech;
                    entry["speech_active"] = json!(speech);
                    if changed {
                        let _ = app.events.send(
                            json!({"type":"channel","generation":frame.generation,"channel":entry}),
                        );
                    }
                    entry["sequence"] = json!(entry["sequence"].as_u64().unwrap_or(0) + 1);
                    entry["audio_seconds"] = json!(
                        entry["audio_seconds"].as_f64().unwrap_or(0.) + audio.len() as f64 / 8000.
                    );
                } else if let Some(entry) = e.channels.get_mut(&id) {
                    entry["speech_active"] = json!(false);
                    entry["available"] = json!(true);
                }
                if speech {
                    // Every speaking channel is recorded as a call, not only the one being heard.
                    let (call_frequency, label, level) = e
                        .channels
                        .get(&id)
                        .map(|c| {
                            (
                                c["frequency_hz"].as_f64().unwrap_or(frequency),
                                c["label"].clone(),
                                c["level_db"].as_f64(),
                            )
                        })
                        .unwrap_or((frequency, Value::Null, None));
                    let events = e.store.record(
                        &id,
                        protocol,
                        slot as u64,
                        call_frequency,
                        &label,
                        level,
                        &audio,
                        now(),
                    );
                    for event in events {
                        let _ = app.events.send(
                            json!({"type":"call","phase":event.phase,"generation":frame.generation,"call":event.call}),
                        );
                    }
                }
                // A pin on an unconfirmed ("Unknown") channel follows whatever is heard at that
                // frequency; audio is then labeled with the pinned id so the listener matches.
                let pinned_unknown = e.selected.as_deref().is_some_and(|s| {
                    e.channels.get(s).is_some_and(|c| {
                        c["protocol"] == "Unknown"
                            && c["logical_hz"].as_f64().map(|v| v.round() as u64) == Some(logical)
                    })
                });
                let selected = e.selected.as_deref() == Some(&id) || pinned_unknown;
                let out_id = if pinned_unknown {
                    e.selected.clone().unwrap_or_else(|| id.clone())
                } else {
                    id.clone()
                };
                let source_generation = e.generation;
                let decoder_generation = e.decoder_generation;
                if selected {
                    e.listener["voice_active"] = json!(speech);
                }
                if speech
                    && selected
                    && work.received >= e.selected_at
                    && (!saw_preroll || live_samples >= 17280)
                {
                    let sequence = e.listener["sequence"].as_u64().unwrap_or(0) + 1;
                    let ingress = work.received.elapsed().as_secs_f64() * 1000.;
                    let bytes: Vec<u8> = audio.iter().flat_map(|v| v.to_le_bytes()).collect();
                    e.listener = json!({"status":"listening","state":"listening","channel_id":out_id,"voice_active":true,"generation":decoder_generation,"source_generation":source_generation,"sequence":sequence,"audio_chunks":sequence,"last_audio_at":now(),"ingress_age_ms":ingress,"ingress_age_basis":"host ingress receipt; device speech alignment unknown","dropped_blocks":e.drops,"input_dropped":e.drops,"queue_blocks":0,"error":null});
                    let _=app.audio.send(Audio{meta:json!({"type":"audio","encoding":"s16le","channels":1,"rate_hz":8000,"samples":audio.len(),"channel_id":out_id,"generation":decoder_generation,"source_generation":source_generation,"sequence":sequence,"ingress_age_ms":ingress,"ingress_age_basis":"host ingress receipt; device speech alignment unknown"}),pcm:bytes,created:Instant::now()});
                }
            }
            if let Some(protocol) = protocol_evidence {
                decoders.retain(|(p, _)| *p == protocol);
                worker_cost.store(decoders.len(), Ordering::Relaxed);
            }
        }
    });
    Ok(Worker {
        sender: tx,
        stream,
        queued,
        cost,
    })
}
/// One logical target competing for the bounded codec budget.
struct Candidate {
    logical: u64,
    selected: bool,
    /// The pin request for the selected target is recent enough that its IQ may still be
    /// on the way; after that an unforwarded selection stops holding codec budget.
    pin_pending: bool,
    /// The source is actually delivering IQ for this target's physical stream.
    forwarded: bool,
    rf_active: bool,
    cost: usize,
}
/// Pick the logical targets that get codec instances. The pinned listener comes
/// first while its pin is recent (its IQ is requested even if the confirming CONFIG is
/// still in flight),
/// then streams with forwarded IQ (RF-active before quiet), then lower frequency.
/// A stream whose IQ the source does not forward can never be decoded, so it
/// never takes budget, however RF-active its metadata looks.
fn admit(mut candidates: Vec<Candidate>, budget: usize) -> Vec<u64> {
    candidates.retain(|c| c.forwarded || (c.selected && c.pin_pending));
    candidates.sort_by_key(|c| (!c.selected, !c.forwarded, !c.rf_active, c.logical));
    let mut remaining = budget;
    let mut admitted = Vec::new();
    for c in candidates {
        if c.cost <= remaining {
            admitted.push(c.logical);
            remaining -= c.cost;
        }
    }
    admitted
}
/// How long a fresh selection may hold budget before the source forwards its IQ.
const PIN_GRACE: Duration = Duration::from_secs(5);
/// Streams that delivered IQ this recently count as forwarded even when the
/// source's CONFIG does not say (older producers, or a pin acknowledged later).
const IQ_RECENT: Duration = Duration::from_secs(2);
fn schedule(
    app: &App,
    configs: &HashMap<u64, Value>,
    workers: &mut HashMap<u64, Worker>,
    iq_seen: &HashMap<u32, Instant>,
) -> Result<()> {
    let e = app.inner.lock().unwrap();
    let selected = selected_logical(&e);
    let pin_pending = e.selected_at.elapsed() < PIN_GRACE;
    let mut effective = configs.clone();
    for (&stream, cfg) in &mut effective {
        if cfg["protocol"].as_str().unwrap_or("Unknown") == "Unknown"
            && let Some(protocol) = e
                .channels
                .values()
                .find(|c| {
                    c["logical_hz"].as_f64().map(|v| v.round() as u64) == Some(stream)
                        && c["in_range"] == true
                        && c["decoded"] == true
                })
                .and_then(|c| c["protocol"].as_str())
        {
            cfg["protocol"] = json!(protocol);
        }
    }
    let candidates: Vec<Candidate> = effective
        .iter()
        .map(|(&logical, cfg)| {
            let physical = cfg["stream_id"].as_u64().unwrap_or(0) as u32;
            // CONFIG's `iq_forwarded`/`pinned` is the source forwarding mask; a source
            // that does not publish it is assumed to forward everything it declares.
            // Absent means "forwards everything it declares"; a present non-boolean
            // value is malformed and fails closed.
            let declared = match &cfg["iq_forwarded"] {
                Value::Null => true,
                Value::Bool(b) => *b,
                _ => false,
            } || cfg["pinned"] == true;
            let received = iq_seen
                .get(&physical)
                .is_some_and(|t| t.elapsed() < IQ_RECENT);
            Candidate {
                logical,
                selected: Some(logical) == selected,
                pin_pending,
                forwarded: declared || received,
                rf_active: e.channels.values().any(|c| {
                    c["logical_hz"].as_f64().map(|v| v.round() as u64) == Some(logical)
                        && c["rf_active"] == true
                }),
                cost: workers
                    .get(&logical)
                    .map(|w| w.cost.load(Ordering::Relaxed))
                    .unwrap_or_else(|| modes(cfg).len()),
            }
        })
        .collect();
    let metadata_only = candidates
        .iter()
        .filter(|c| !c.selected && !c.forwarded)
        .count();
    drop(e);
    let admitted = admit(candidates, app.max_decoders);
    workers.retain(|key, _| admitted.contains(key));
    for stream in admitted {
        if let std::collections::hash_map::Entry::Vacant(e) = workers.entry(stream) {
            e.insert(worker(app.clone(), stream, effective[&stream].clone())?);
        }
    }
    let mut e = app.inner.lock().unwrap();
    e.worker_epochs
        .retain(|stream, _| workers.contains_key(stream));
    e.source["queue_depth"] = json!(
        workers
            .values()
            .map(|w| w.queued.load(Ordering::Relaxed))
            .sum::<usize>()
    );
    e.source["active_decoders"] = json!(
        workers
            .values()
            .map(|w| w.cost.load(Ordering::Relaxed))
            .sum::<usize>()
    );
    e.source["skipped_streams"] = json!(configs.len() - workers.len());
    e.source["metadata_only_streams"] = json!(metadata_only);
    for c in e.channels.values_mut() {
        let logical = c["logical_hz"]
            .as_f64()
            .map(|v| v.round() as u64)
            .unwrap_or(0);
        c["available"] = json!(workers.contains_key(&logical) && c["in_range"] == true);
        c["decoder_admitted"] = c["available"].clone();
    }
    Ok(())
}
/// One spectrum meter per physical stream: per-target activity for shared slices, and the
/// carrier estimate for every target. Targets already measured keep their state across
/// CONFIG refreshes.
fn rebuild_meters(configs: &HashMap<u64, Value>, meters: &mut HashMap<u32, SliceMeter>) {
    let mut grouped: HashMap<u32, Vec<TargetBand>> = HashMap::new();
    for (&logical, cfg) in configs {
        let stream = cfg["stream_id"].as_u64().unwrap_or(0) as u32;
        let spec = spec_for(cfg["protocol"].as_str().unwrap_or("Unknown"));
        let mut target = TargetBand::new(logical, 0., spec.band_hz);
        if let Some(old) = meters
            .get(&stream)
            .and_then(|m| m.targets.iter().find(|t| t.logical == logical))
        {
            target.active = old.active;
            target.carrier_hz = old.carrier_hz;
            target.carrier_updates = old.carrier_updates;
        }
        grouped.entry(stream).or_default().push(target);
    }
    meters.clear();
    for (stream, mut targets) in grouped {
        targets.sort_by_key(|t| t.logical);
        meters.insert(stream, SliceMeter::new(targets));
    }
}
/// Record a measured carrier on a channel entry. `carrier_hz` always holds the calibrated
/// estimate. The published `frequency_hz` follows it only when the estimate lands on a
/// raster point (`frequency_snapped`): a coarse 12.5 kHz producer bin can contain several
/// overlapping emitters, and then the spectral centroid is not a carrier, so the entry
/// keeps its nominal frequency instead of a misleading one. Returns whether the published
/// frequency or label changed.
fn apply_carrier(app: &App, c: &mut Value, reading_hz: f64, offset_hz: f64) -> bool {
    let truth = app.calibration.true_hz(reading_hz);
    let (frequency, snapped) = snap(truth);
    let before = (c["frequency_hz"].clone(), c["label"].clone());
    c["carrier_offset_hz"] = json!(offset_hz.round());
    c["carrier_hz"] = json!(truth.round());
    c["carrier_snapped"] = json!(snapped);
    if snapped {
        let label = app.labels.lookup(truth).cloned();
        c["frequency_hz"] = json!(frequency);
        c["frequency_snapped"] = json!(true);
        if let (Some(fields), Some(extra)) =
            (c.as_object_mut(), label_fields(label.as_ref()).as_object())
        {
            fields.extend(extra.clone());
        }
    }
    before != (c["frequency_hz"].clone(), c["label"].clone())
}
pub fn attach_control(stream: &TcpStream, app: &App) -> Result<()> {
    let mut writer = stream.try_clone()?;
    let (tx, rx) = mpsc::sync_channel::<Value>(32);
    app.inner.lock().unwrap().control = Some(tx);
    let app = app.clone();
    thread::spawn(move || {
        for (sequence, value) in rx.into_iter().enumerate() {
            let generation = app.inner.lock().unwrap().generation;
            let frame = Frame {
                kind: 6,
                flags: 0,
                generation,
                stream_id: 0,
                sequence: sequence as u64,
                first_sample: 0,
                rate: 0,
                center_hz: 0,
                format: 3,
                valid_bits: 0,
                payload: value.to_string().into_bytes(),
            };
            if let Err(e) = frame.write(&mut writer) {
                app.error(format!("source control failed: {e}"));
                break;
            }
        }
    });
    Ok(())
}
pub fn ingest(mut input: impl Read, app: App) -> Result<()> {
    let mut workers = HashMap::new();
    let mut configs = HashMap::new();
    let mut config_generation = None;
    let mut scheduled = Instant::now();
    let mut transport_sequences = HashMap::new();
    let mut pending_gaps = HashSet::new();
    let mut iq_seen: HashMap<u32, Instant> = HashMap::new();
    let mut meters: HashMap<u32, SliceMeter> = HashMap::new();

    loop {
        let mut frame = match Frame::read(&mut input) {
            Ok(f) => f,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
                ) =>
            {
                let stopped = app.inner.lock().unwrap().source["receiver_state"] == "stopped";
                if stopped {
                    let mut e = app.inner.lock().unwrap();
                    e.active = false;
                    e.control = None;
                    e.worker_epochs.clear();
                    e.source["active_decoders"] = json!(0);
                    e.source["queue_depth"] = json!(0);
                    for c in e.channels.values_mut() {
                        c["available"] = json!(false);
                        c["decoder_admitted"] = json!(false);
                        c["speech_active"] = json!(false);
                        c["rf_active"] = json!(false);
            c["slice_rf_active"] = json!(false);
            c["target_rf_active"] = json!(false);
                    }
                } else {
                    app.error(format!("RIQ source disconnected: {e}"));
                }
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };
        if frame.kind == 4 {
            let config: Value = serde_json::from_slice(&frame.payload)?;
            let streams = config["streams"]
                .as_array()
                .context("CONFIG requires streams")?;
            anyhow::ensure!(streams.len() <= 4096, "CONFIG exceeds 4096 streams");
            let changed = config_generation != Some(frame.generation);
            if config["receiver_state"] == "stopped" {
                workers.clear();
                configs.clear();
            }
            if changed {
                workers.clear();
                configs.clear();
                transport_sequences.clear();
                pending_gaps.clear();
                iq_seen.clear();
                meters.clear();
            }
            let mut e = app.inner.lock().unwrap();
            e.source = config.clone();
            e.generation = frame.generation;
            e.active = config["receiver_state"].as_str() != Some("stopped");
            e.error = None;
            if changed {
                e.decoder_generation += 1;
                e.psd.clear();
                if e.started.is_none() {
                    e.started = Some(now());
                }
                for channel in e.channels.values_mut() {
                    channel["in_range"] = json!(false);
                    channel["available"] = json!(false);
                    channel["rf_active"] = json!(false);
                    channel["slice_rf_active"] = json!(false);
                    channel["target_rf_active"] = json!(false);
                    channel["speech_active"] = json!(false);
                }
            }
            for physical in streams {
                let stream = physical["stream_id"]
                    .as_u64()
                    .context("stream_id missing")?;
                anyhow::ensure!(stream <= u32::MAX as u64, "stream_id overflow");
                let stream = stream as u32;
                let targets = physical["targets"]
                    .as_array()
                    .cloned()
                    .unwrap_or_else(|| vec![physical.clone()]);
                anyhow::ensure!(
                    targets.len() <= 64,
                    "physical stream exceeds 64 logical targets"
                );
                // Targets on the same frequency (DMR slots 1 and 2) are one logical target,
                // and targets the slice cannot contain are not counted.
                let target_count = targets
                    .iter()
                    .filter_map(|t| {
                        let mut cfg = physical.clone();
                        cfg.as_object_mut()?.extend(t.as_object()?.clone());
                        let frequency = cfg["target_hz"].as_f64().or_else(|| cfg["center_hz"].as_f64())?;
                        fits_slice(&cfg, frequency).then_some(frequency.round() as u64)
                    })
                    .collect::<HashSet<_>>()
                    .len();
                for target in targets {
                    let mut cfg = physical.clone();
                    cfg.as_object_mut()
                        .context("stream must be an object")?
                        .remove("targets");
                    cfg.as_object_mut().unwrap().extend(
                        target
                            .as_object()
                            .context("target must be an object")?
                            .clone(),
                    );
                    cfg["stream_id"] = json!(stream);
                    let frequency = cfg["target_hz"]
                        .as_f64()
                        .or_else(|| cfg["center_hz"].as_f64())
                        .context("frequency missing")?;
                    anyhow::ensure!(
                        (70000000.0..=6000000000.0).contains(&frequency),
                        "logical target out of receiver range"
                    );
                    cfg["target_hz"] = json!(frequency);
                    let protocol = cfg["protocol"].as_str().unwrap_or("Unknown");
                    anyhow::ensure!(
                        matches!(protocol, "Unknown" | "DMR" | "NXDN48"),
                        "unsupported source protocol"
                    );
                    if let Some(num) = cfg["sample_rate_num_hz"].as_f64() {
                        let den = cfg["sample_rate_den"]
                            .as_f64()
                            .context("rational sample rate denominator missing")?;
                        anyhow::ensure!(
                            den > 0. && (16000.0..=192000.0).contains(&(num / den)),
                            "invalid rational IQ sample rate"
                        );
                    }
                    // A target the slice cannot contain would alias in the mixer and decode
                    // some other signal under this frequency; refuse it.
                    if !fits_slice(&cfg, frequency) {
                        let skipped = e.source["out_of_slice_targets"].as_u64().unwrap_or(0);
                        e.source["out_of_slice_targets"] = json!(skipped + 1);
                        continue;
                    }
                    let slot = cfg["slot"].as_u64().unwrap_or(1) as u8;
                    let id = channel_id(app.display_frequency(frequency).0, protocol, slot);
                    let channel = e
                        .channels
                        .entry(id)
                        .or_insert_with(|| make_channel(&app, stream, frequency, protocol, slot));
                    channel["stream_id"] = json!(stream);
                    channel["in_range"] = json!(true);
                    // A catalog entry restored from disk has no live stream until now.
                    channel["logical_hz"] = json!(frequency);
                    channel["reading_hz"] = json!(frequency);
                    for channel in e.channels.values_mut() {
                        if channel["logical_hz"] == frequency {
                            channel["stream_id"] = json!(stream);
                            channel["in_range"] = json!(true);
                            channel["slice_targets"] = json!(target_count);
                            refresh_rf(channel);
                        }
                    }
                    configs.insert(frequency.round() as u64, cfg);
                }
            }
            drop(e);
            let _ = app.events.send(
                json!({"type":"source_config","generation":frame.generation,"source":config}),
            );
            schedule(&app, &configs, &mut workers, &iq_seen)?;
            rebuild_meters(&configs, &mut meters);
            transport_sequences.insert(frame.stream_id, frame.sequence);
            config_generation = Some(frame.generation);
            continue;
        }
        anyhow::ensure!(
            Some(frame.generation) == config_generation,
            "frame generation requires matching CONFIG"
        );
        if let Some(previous) = transport_sequences.insert(frame.stream_id, frame.sequence)
            && frame.sequence != previous.wrapping_add(1)
        {
            pending_gaps.insert(frame.stream_id);
            app.inner.lock().unwrap().drops += 1;
        }
        if frame.kind == 1 && pending_gaps.remove(&frame.stream_id) {
            frame.flags |= DISCONTINUITY;
        }
        {
            let mut e = app.inner.lock().unwrap();
            e.frames += 1;
            if frame.rate > 0 && matches!(frame.kind, 1 | 2) {
                let position = frame.first_sample as f64 / frame.rate as f64;
                e.source["source_position_s"] =
                    json!(position.max(e.source["source_position_s"].as_f64().unwrap_or(0.)));
                e.source["last_samples_at"] = json!(now());
            }
        }
        if scheduled.elapsed() > Duration::from_millis(100) {
            schedule(&app, &configs, &mut workers, &iq_seen)?;
            scheduled = Instant::now();
        }
        match frame.kind {
            1 => {
                anyhow::ensure!(
                    frame.rate >= 16000 && frame.rate <= 192000,
                    "channelized IQ must be 16..192ksps"
                );
                anyhow::ensure!(
                    frame.payload.len() / 4 <= (frame.rate / 10 + 1) as usize,
                    "IQ packets must be <=100ms"
                );
                frame
                    .first_sample
                    .checked_add(frame.payload.len() as u64 / 4)
                    .context("IQ sample counter overflow")?;
                iq_seen.insert(frame.stream_id, Instant::now());
                if frame.flags & PREROLL == 0
                    && let Some(meter) = meters.get_mut(&frame.stream_id)
                {
                    if frame.flags & (START | DISCONTINUITY | OVERFLOW) != 0 {
                        meter.reset_window();
                    }
                    let iq = iq_from_payload(&frame.payload);
                    for target in &mut meter.targets {
                        target.offset_hz = target.logical as f64 - frame.center_hz as f64;
                    }
                    let full_scale = 2f64.powi(frame.valid_bits.clamp(1, 16) as i32 - 1);
                    if meter.measure(&iq, frame.rate as f64, full_scale) {
                        let mut e = app.inner.lock().unwrap();
                        for target in &meter.targets {
                            for c in e.channels.values_mut().filter(|c| {
                                c["stream_id"] == frame.stream_id
                                    && c["logical_hz"].as_f64().map(|v| v.round() as u64)
                                        == Some(target.logical)
                            }) {
                                let before = c["rf_active"].clone();
                                c["target_rf_active"] = json!(target.active);
                                c["target_level_db"] = json!(target.level_dbfs);
                                refresh_rf(c);
                                let moved = match target.carrier_hz {
                                    Some(offset) if target.carrier_updates >= 3 => {
                                        apply_carrier(&app, c, frame.center_hz as f64 + offset, offset)
                                    }
                                    _ => false,
                                };
                                if c["rf_active"] == true {
                                    c["last_rf"] = json!(now());
                                }
                                if c["rf_active"] != before || moved {
                                    let _ = app.events.send(json!({"type":"channel","generation":frame.generation,"channel":c}));
                                }
                            }
                        }
                    }
                }
                let shared = Arc::new(frame);
                let mut missed = false;
                for worker in workers.values().filter(|w| w.stream == shared.stream_id) {
                    worker.queued.fetch_add(1, Ordering::Relaxed);
                    if worker
                        .sender
                        .try_send(Work {
                            frame: shared.clone(),
                            received: Instant::now(),
                        })
                        .is_err()
                    {
                        worker.queued.fetch_sub(1, Ordering::Relaxed);
                        missed = true;
                    }
                }
                if missed {
                    app.inner.lock().unwrap().drops += 1;
                    pending_gaps.insert(shared.stream_id);
                }
            }
            2 => {
                anyhow::ensure!(
                    frame.format == 2
                        && frame.payload.len() % 4 == 0
                        && frame.payload.len() <= 4096,
                    "PSD requires <=1024 f32 bins"
                );
                let row: Vec<f32> = frame
                    .payload
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                    .collect();
                anyhow::ensure!(row.iter().all(|x| x.is_finite()), "nonfinite PSD");
                let mut e = app.inner.lock().unwrap();
                e.psd_seq += 1;
                let seq = e.psd_seq;
                e.psd.push_back((
                    seq,
                    frame.first_sample as f64 / frame.rate.max(1) as f64,
                    row,
                ));
                let _ = app.events.send(
                    json!({"type":"spectrum_available","generation":e.generation,"sequence":seq}),
                );
                while e.psd.len() > 120 {
                    e.psd.pop_front();
                }
            }
            3 => {
                let activity: Value = serde_json::from_slice(&frame.payload)?;
                // The browser validates these strictly; never pass wire values through raw.
                let slice_active = activity["rf_active"].as_bool().unwrap_or(false);
                let slice_level = activity["level_db"]
                    .as_f64()
                    .filter(|v| v.is_finite())
                    .map_or(Value::Null, |v| json!(v));
                if !slice_active && let Some(meter) = meters.get_mut(&frame.stream_id) {
                    // A closed gate ends the burst; the next one opens from the open threshold.
                    for target in &mut meter.targets {
                        target.active = false;
                    }
                }
                let mut e = app.inner.lock().unwrap();
                for c in e
                    .channels
                    .values_mut()
                    .filter(|c| c["stream_id"] == frame.stream_id)
                {
                    let before = c["rf_active"].clone();
                    c["slice_rf_active"] = json!(slice_active);
                    c["slice_level_db"] = slice_level.clone();
                    if !slice_active {
                        // No IQ is forwarded while the gate is closed, so the target
                        // measurement is stale; the next live packet re-measures it.
                        c["target_rf_active"] = json!(false);
                        c["target_level_db"] = Value::Null;
                    }
                    refresh_rf(c);
                    if c["rf_active"] == true {
                        c["last_rf"] = json!(now());
                    } else {
                        c["speech_active"] = json!(false);
                    }
                    if c["rf_active"] != before {
                        let _ = app.events.send(
                            json!({"type":"channel","generation":frame.generation,"channel":c}),
                        );
                    }
                }
            }
            5 => {
                let gap: Value = serde_json::from_slice(&frame.payload)
                    .unwrap_or_else(|_| json!({"reason":"unspecified"}));
                if frame.stream_id == 0 {
                    workers.clear();
                } else {
                    workers.retain(|_, worker| worker.stream != frame.stream_id);
                }
                let mut e = app.inner.lock().unwrap();
                e.drops += 1;
                if frame.stream_id == 0 {
                    e.worker_epochs.clear();
                    e.decoder_generation += 1;
                } else {
                    e.worker_epochs
                        .retain(|logical, _| workers.contains_key(logical));
                    if e.selected
                        .as_deref()
                        .and_then(|id| e.channels.get(id))
                        .is_some_and(|c| c["stream_id"] == frame.stream_id)
                    {
                        e.decoder_generation += 1;
                    }
                }
                e.source["last_gap"] = gap.clone();
                for c in e
                    .channels
                    .values_mut()
                    .filter(|c| frame.stream_id == 0 || c["stream_id"] == frame.stream_id)
                {
                    c["speech_active"] = json!(false);
                    c["available"] = json!(false);
                    c["decoder_admitted"] = json!(false);
                }
                let _=app.events.send(json!({"type":"source_gap","generation":e.generation,"stream_id":frame.stream_id,"detail":gap}));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(
        kind: u16,
        generation: u64,
        stream_id: u32,
        sequence: u64,
        sample: u64,
        payload: Vec<u8>,
    ) -> Frame {
        Frame {
            kind,
            flags: 0,
            generation,
            stream_id,
            sequence,
            first_sample: sample,
            rate: 25000,
            center_hz: 454029223,
            format: if kind == 1 {
                1
            } else if kind == 2 {
                2
            } else {
                3
            },
            valid_bits: 16,
            payload,
        }
    }
    fn configuration(frequency: u64) -> Vec<u8> {
        json!({"input_center_hz":454216998,"input_rate_hz":6000000,"input_bandwidth_hz":6000000,"streams":[{"stream_id":1,"center_hz":frequency,"protocol":"Unknown"}],"psd":{"unit":"dBFS"}}).to_string().into_bytes()
    }
    #[test]
    fn all_message_types_share_transport_sequence_without_resetting_iq() {
        let app = App::new(3);
        let mut bytes = Vec::new();
        for f in [
            frame(4, 1, 0, 0, 0, configuration(454029223)),
            frame(1, 1, 1, 0, 0, vec![0; 10000]),
            frame(
                3,
                1,
                1,
                1,
                0,
                br#"{"rf_active":false,"level_db":-100}"#.to_vec(),
            ),
            frame(1, 1, 1, 2, 2500, vec![0; 10000]),
            frame(2, 1, 1, 3, 5000, (-90f32).to_le_bytes().to_vec()),
        ] {
            f.write(&mut bytes).unwrap();
        }
        ingest(&bytes[..], app.clone()).unwrap();
        let e = app.inner.lock().unwrap();
        assert_eq!(e.drops, 0);
        assert_eq!(e.psd.len(), 1);
        assert_eq!(e.source["psd"]["unit"], "dBFS");
    }

    fn forwarding_configuration() -> Vec<u8> {
        // Streams 1..=4 are metadata-only (lower IDs, lower frequencies); stream 50
        // is the one the source actually forwards IQ for.
        let mut streams: Vec<Value> = (1..=4)
            .map(|id| json!({"stream_id":id,"center_hz":454000000u64+id*25000,"protocol":"Unknown","iq_forwarded":false,"pinned":false}))
            .collect();
        streams.push(json!({"stream_id":50,"center_hz":454200000u64,"protocol":"Unknown","iq_forwarded":true,"pinned":false}));
        json!({"input_center_hz":454216998,"input_rate_hz":800000,"input_bandwidth_hz":800000,"streams":streams,"psd":{"unit":"dBFS"}}).to_string().into_bytes()
    }
    /// Feeds `data`, then blocks until released, so a test can observe the live
    /// scheduler state before end-of-stream marks the source disconnected.
    struct HeldOpen {
        data: std::io::Cursor<Vec<u8>>,
        release: mpsc::Receiver<()>,
    }
    impl Read for HeldOpen {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.data.read(buf)?;
            if n == 0 {
                let _ = self.release.recv();
            }
            Ok(n)
        }
    }
    #[test]
    fn forwarded_stream_is_admitted_ahead_of_metadata_only_lower_ids() {
        // An unknown stream costs three codec instances, so a budget of three admits one.
        let app = App::new(3);
        let mut bytes = Vec::new();
        frame(4, 1, 0, 0, 0, forwarding_configuration())
            .write(&mut bytes)
            .unwrap();
        let (release, rx) = mpsc::channel();
        let input = HeldOpen {
            data: std::io::Cursor::new(bytes),
            release: rx,
        };
        let runner = {
            let app = app.clone();
            thread::spawn(move || ingest(input, app))
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let admitted = loop {
            let admitted: Vec<u64> = {
                let e = app.inner.lock().unwrap();
                e.channels
                    .values()
                    .filter(|c| c["decoder_admitted"] == true)
                    .map(|c| c["stream_id"].as_u64().unwrap())
                    .collect()
            };
            if !admitted.is_empty() || Instant::now() > deadline {
                break admitted;
            }
            thread::sleep(Duration::from_millis(10));
        };
        let metadata_only = app.inner.lock().unwrap().source["metadata_only_streams"].clone();
        release.send(()).unwrap();
        runner.join().unwrap().unwrap();
        assert_eq!(admitted, vec![50], "forwarded stream must hold the decoder budget");
        assert_eq!(metadata_only, 4);
    }
    #[test]
    fn metadata_only_streams_take_no_budget_even_when_budget_is_spare() {
        let c = |logical, forwarded| Candidate { logical, selected: false, pin_pending: false, forwarded, rf_active: true, cost: 2 };
        assert_eq!(admit(vec![c(1, false), c(2, false), c(3, true)], 20), vec![3]);
    }
    #[test]
    fn an_unforwarded_selection_holds_budget_only_while_its_pin_is_pending() {
        let c = |pin_pending| vec![
            Candidate { logical: 1, selected: true, pin_pending, forwarded: false, rf_active: false, cost: 3 },
            Candidate { logical: 50, selected: false, pin_pending: false, forwarded: true, rf_active: false, cost: 3 },
        ];
        assert_eq!(admit(c(true), 3), vec![1]);
        assert_eq!(admit(c(false), 3), vec![50]);
    }
    /// `pinned` overrides `iq_forwarded: false`, an absent flag means forwarded, and a
    /// malformed flag fails closed.
    #[test]
    fn forwarding_flags_pinned_absent_and_malformed() {
        let streams = json!([
            {"stream_id":1,"center_hz":454000000u64,"protocol":"Unknown","iq_forwarded":false,"pinned":true},
            {"stream_id":2,"center_hz":454025000u64,"protocol":"Unknown","iq_forwarded":"yes"},
            {"stream_id":3,"center_hz":454050000u64,"protocol":"Unknown"},
            {"stream_id":4,"center_hz":454075000u64,"protocol":"Unknown","iq_forwarded":false},
        ]);
        let config = json!({"input_center_hz":454216998,"input_rate_hz":800000,"input_bandwidth_hz":800000,"streams":streams,"psd":{"unit":"dBFS"}});
        let mut bytes = Vec::new();
        frame(4, 1, 0, 0, 0, config.to_string().into_bytes()).write(&mut bytes).unwrap();
        let (release, rx) = mpsc::channel();
        let app = App::new(6); // two unknown streams
        let runner = {
            let app = app.clone();
            let input = HeldOpen { data: std::io::Cursor::new(bytes), release: rx };
            thread::spawn(move || ingest(input, app))
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let admitted = loop {
            let mut admitted: Vec<u64> = app
                .inner
                .lock()
                .unwrap()
                .channels
                .values()
                .filter(|c| c["decoder_admitted"] == true)
                .map(|c| c["stream_id"].as_u64().unwrap())
                .collect();
            admitted.sort();
            if admitted.len() == 2 || Instant::now() > deadline {
                break admitted;
            }
            thread::sleep(Duration::from_millis(10));
        };
        release.send(()).unwrap();
        runner.join().unwrap().unwrap();
        assert_eq!(admitted, vec![1, 3]);
    }
    #[test]
    fn a_dmr_channel_costs_one_decoder_and_an_unknown_stream_two() {
        let cost = |protocol: &str| modes(&json!({"protocol": protocol})).len();
        assert_eq!(cost("DMR"), 1, "one DSDcc instance decodes both slots");
        assert_eq!(cost("NXDN48"), 1);
        assert_eq!(cost("Unknown"), 2);
        // A budget of 3 holds three known DMR channels.
        let streams: Vec<Value> = (1..=4)
            .map(|id| json!({"stream_id":id,"center_hz":454000000u64+id*12500,"protocol":"DMR","iq_forwarded":true}))
            .collect();
        let config = json!({"input_center_hz":454216998,"input_rate_hz":800000,"input_bandwidth_hz":800000,"streams":streams,"psd":{"unit":"dBFS"}});
        let mut bytes = Vec::new();
        frame(4, 1, 0, 0, 0, config.to_string().into_bytes()).write(&mut bytes).unwrap();
        let (release, rx) = mpsc::channel();
        let app = App::new(3);
        let runner = {
            let app = app.clone();
            let input = HeldOpen { data: std::io::Cursor::new(bytes), release: rx };
            thread::spawn(move || ingest(input, app))
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let admitted = loop {
            let n = app.inner.lock().unwrap().channels.values().filter(|c| c["decoder_admitted"] == true).count();
            if n >= 3 || Instant::now() > deadline {
                break n;
            }
            thread::sleep(Duration::from_millis(10));
        };
        release.send(()).unwrap();
        runner.join().unwrap().unwrap();
        assert_eq!(admitted, 3);
    }
    /// 25 ksps IQ of a quiet narrowband-FM carrier (1 kHz tone, 1.5 kHz deviation) or, with
    /// `voice` false, receiver noise only.
    fn fm_stream_payload(voice: bool, first: u64) -> Vec<u8> {
        let mut state = 0xC0FF_EE11u32 ^ first as u32;
        let mut noise = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state >> 8) as f64 / 16_777_216. - 0.5
        };
        let mut out = Vec::new();
        for i in 0..2500u64 {
            let t = (first + i) as f64 / 25000.;
            let (re, im) = if voice {
                let phase = 2. * std::f64::consts::PI * 1000. * t;
                let carrier = 1.5 / 1.0 * phase.sin() * 1.0; // modulation index 1.5 (1.5 kHz dev / 1 kHz tone)
                (3000. * carrier.cos() + 30. * noise(), 3000. * carrier.sin() + 30. * noise())
            } else {
                (4000. * noise(), 4000. * noise())
            };
            out.extend_from_slice(&(re as i16).to_le_bytes());
            out.extend_from_slice(&(im as i16).to_le_bytes());
        }
        out
    }
    fn fm_session(voice: bool, pin_unknown: bool) -> (Option<Value>, Vec<String>, bool, Vec<Value>) {
        let config = json!({"input_center_hz":454216998,"input_rate_hz":800000,"input_bandwidth_hz":800000,
            "streams":[{"stream_id":1,"center_hz":454000000u64,"target_hz":454000000u64,"rate_hz":25000,"protocol":"Unknown","iq_forwarded":true}],"psd":{"unit":"dBFS"}});
        let mut bytes = Vec::new();
        let mut cfg = frame(4, 1, 0, 0, 0, config.to_string().into_bytes());
        cfg.center_hz = 454_000_000;
        cfg.write(&mut bytes).unwrap();
        let mut frames = vec![(0., bytes)];
        let (mut sequence, mut sample) = (1u64, 0u64);
        for _ in 0..30 {
            let at = sample as f64 / 25000.;
            let mut bytes = Vec::new();
            let mut act = frame(3, 1, 1, sequence, sample, br#"{"rf_active":true,"level_db":-20.0}"#.to_vec());
            act.center_hz = 454_000_000;
            act.write(&mut bytes).unwrap();
            let mut iq = frame(1, 1, 1, sequence + 1, sample, fm_stream_payload(voice, sample));
            iq.center_hz = 454_000_000;
            iq.write(&mut bytes).unwrap();
            frames.push((at, bytes));
            sequence += 2;
            sample += 2500;
        }
        let (release, rx) = mpsc::channel();
        let app = App::new(8);
        let mut audio_rx = app.audio.subscribe();
        let runner = {
            let app = app.clone();
            let input = PacedFrames {
                frames: frames.into_iter(),
                current: std::io::Cursor::new(vec![]),
                started: Instant::now(),
                speed: 5.,
                hold: Some(rx),
            };
            thread::spawn(move || ingest(input, app))
        };
        if pin_unknown {
            // Pin the unconfirmed bin entry (no 422): audio must follow it.
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let mut e = app.inner.lock().unwrap();
                if e.channels.contains_key("454000000-Unknown-1") {
                    e.selected = Some("454000000-Unknown-1".into());
                    e.selected_at = Instant::now() - Duration::from_secs(1);
                    e.decoder_generation += 1;
                    break;
                }
                drop(e);
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(2));
            }
        }
        thread::sleep(Duration::from_millis(1200));
        let mut audio_ids = vec![];
        loop {
            match audio_rx.try_recv() {
                Ok(a) => audio_ids.push(a.meta["channel_id"].as_str().unwrap_or("").to_string()),
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
        let fm = app.inner.lock().unwrap().channels.get("454000000-FM-1").cloned();
        let digital = app.inner.lock().unwrap().channels.values().any(|c| c["protocol"] != "Unknown" && c["protocol"] != "FM" && c["decoded"] == true);
        let calls = {
            let mut e = app.inner.lock().unwrap();
            e.store.close_idle(now() + 10.);
            e.store.list(None, 10, None).0
        };
        release.send(()).unwrap();
        runner.join().unwrap().unwrap();
        (fm, audio_ids, digital, calls)
    }
    #[test]
    fn quiet_narrowband_fm_becomes_an_analog_voice_channel_and_noise_does_not() {
        let (fm, _, digital, calls) = fm_session(true, false);
        // The speaking channel is recorded as a call even though nobody pinned it.
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0]["source"], "454000000-FM-1");
        assert!(calls[0]["duration_s"].as_f64().unwrap() >= 0.8);
        let fm = fm.expect("analog voice entry");
        assert_eq!(fm["protocol"], "FM");
        assert_eq!(fm["kind"], "voice");
        assert!(fm["audio_seconds"].as_f64().unwrap() > 0.5, "{fm}");
        assert!(!digital, "analog voice must not be called digital");
        let (fm, _, _, calls) = fm_session(false, false);
        assert!(fm.is_none() && calls.is_empty(), "squelch opened on receiver noise");
    }
    #[test]
    fn pinning_an_unconfirmed_channel_streams_analog_audio_under_the_pinned_id() {
        let (_, audio_ids, _, _) = fm_session(true, true);
        assert!(!audio_ids.is_empty(), "no audio reached the listener");
        assert!(audio_ids.iter().all(|id| id == "454000000-Unknown-1"), "{audio_ids:?}");
    }
    /// With the Pluto's ppm correction, a measured carrier is published at its true raster
    /// frequency and carries the plain-name label for that channel.
    #[test]
    fn measured_carrier_is_published_calibrated_snapped_and_labeled() {
        let labels = Labels::load(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/channel-labels.json")).unwrap();
        let app = App::configured(AppConfig { ppm: 23.3, labels, ..AppConfig::new(8) });
        let config = json!({"input_center_hz":454216998,"input_rate_hz":800000,"input_bandwidth_hz":800000,
            "streams":[{"stream_id":1,"center_hz":454217000u64,"target_hz":454217000u64,"rate_hz":25000,"protocol":"Unknown","iq_forwarded":true}],"psd":{"unit":"dBFS"}});
        let mut bytes = Vec::new();
        let mut cfg = frame(4, 1, 0, 0, 0, config.to_string().into_bytes());
        cfg.center_hz = 454_217_000;
        cfg.write(&mut bytes).unwrap();
        let mut sample = 0u64;
        for packet in 0..8u64 {
            let mut iq = frame(1, 1, 1, packet + 1, sample, fm_stream_payload(true, sample));
            iq.center_hz = 454_217_000;
            iq.write(&mut bytes).unwrap();
            sample += 2500;
        }
        let (release, rx) = mpsc::channel();
        let runner = {
            let app = app.clone();
            let input = HeldOpen { data: std::io::Cursor::new(bytes), release: rx };
            thread::spawn(move || ingest(input, app))
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let channel = loop {
            let c = app.inner.lock().unwrap().channels.values().find(|c| c["label"].is_string()).cloned();
            if c.is_some() || Instant::now() > deadline {
                break c;
            }
            thread::sleep(Duration::from_millis(10));
        };
        release.send(()).unwrap();
        runner.join().unwrap().unwrap();
        let channel = channel.expect("labeled channel");
        assert_eq!(channel["frequency_hz"], 454_206_250.0);
        assert_eq!(channel["frequency_snapped"], true);
        assert_eq!(channel["label"], "Day Wireless (business trunked)");
        assert_eq!(channel["label_kind"], "control");
        assert_eq!(channel["reading_hz"], 454_217_000.0);
        assert!(channel["carrier_offset_hz"].as_f64().unwrap().abs() < 100.);
        // The id was fixed from the calibrated nominal frequency, not the reading.
        assert_eq!(channel["id"], "454206250-Unknown-1");
    }
    #[test]
    fn rf_active_metadata_only_streams_do_not_starve_forwarded_stream() {
        let candidates = vec![
            Candidate { logical: 1, selected: false, pin_pending: false, forwarded: false, rf_active: true, cost: 3 },
            Candidate { logical: 2, selected: false, pin_pending: false, forwarded: false, rf_active: true, cost: 3 },
            Candidate { logical: 50, selected: false, pin_pending: false, forwarded: true, rf_active: false, cost: 3 },
        ];
        assert_eq!(admit(candidates, 3), vec![50]);
    }
    #[test]
    fn selected_stream_wins_then_forwarded_then_rf_active() {
        let c = |logical, selected, forwarded, rf_active| Candidate { logical, selected, pin_pending: true, forwarded, rf_active, cost: 2 };
        let candidates = vec![
            c(10, false, true, false),
            c(20, false, true, true),
            c(30, true, false, false),
            c(40, false, false, true),
        ];
        // Budget for three: selected (even before its pin is confirmed), then the
        // forwarded RF-active stream, then the quiet forwarded stream; metadata-only
        // streams never take a slot.
        assert_eq!(admit(candidates, 6), vec![30, 20, 10]);
    }

    /// Replays an IQ fixture through the real ingest and worker path at `speed` times
    /// real time and reports what the DMR decoder produced.
    struct PacedFrames {
        frames: std::vec::IntoIter<(f64, Vec<u8>)>,
        current: std::io::Cursor<Vec<u8>>,
        started: Instant,
        speed: f64,
        /// When set, hold the stream open after the last frame until released.
        hold: Option<mpsc::Receiver<()>>,
    }
    impl Read for PacedFrames {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            loop {
                let n = self.current.read(buf)?;
                if n > 0 {
                    return Ok(n);
                }
                let Some((at, bytes)) = self.frames.next() else {
                    if let Some(hold) = &self.hold {
                        let _ = hold.recv();
                    }
                    return Ok(0);
                };
                let due = Duration::from_secs_f64(at / self.speed);
                if let Some(wait) = due.checked_sub(self.started.elapsed()) {
                    thread::sleep(wait);
                }
                self.current = std::io::Cursor::new(bytes);
            }
        }
    }
    #[test]
    #[ignore = "set RADIO_DMR_IQ25K to the 25 ksps CI16 fixture"]
    fn actual_dmr_iq_fixture_decodes_through_channel_filter() {
        let path = std::env::var("RADIO_DMR_IQ25K").expect("RADIO_DMR_IQ25K");
        let data = std::fs::read(path).unwrap();
        let mut frames = vec![];
        let config = json!({"input_center_hz":454216998,"input_rate_hz":6000000,"input_bandwidth_hz":6000000,"hardware_counter":false,"streams":[{"stream_id":1,"center_hz":454029223,"target_hz":454029223,"rate_hz":25000,"protocol":"Unknown","slot":1}]});
        let mut b = vec![];
        frame(4, 1, 0, 0, 0, config.to_string().into_bytes()).write(&mut b).unwrap();
        frames.push((0., b));
        let (mut sequence, mut sample) = (1u64, 0u64);
        for chunk in data.chunks(10000) {
            let mut b = vec![];
            let mut f = frame(1, 1, 1, sequence, sample, chunk.to_vec());
            f.format = 1;
            f.write(&mut b).unwrap();
            let at = sample as f64 / 25000.;
            frames.push((at, b));
            let mut a = vec![];
            frame(3, 1, 1, sequence + 1, sample, br#"{"rf_active":true,"level_db":-30.0}"#.to_vec())
                .write(&mut a)
                .unwrap();
            frames.push((at, a));
            sample += chunk.len() as u64 / 4;
            sequence += 2;
        }
        let app = App::new(8);
        let input = PacedFrames {
            frames: frames.into_iter(),
            current: std::io::Cursor::new(vec![]),
            started: Instant::now(),
            speed: 8.,
            hold: None,
        };
        // Observe the catalog while the source is still connected.
        let observer = {
            let app = app.clone();
            thread::spawn(move || {
                let mut best = (0., String::new());
                let mut analog = 0f64;
                for _ in 0..400 {
                    thread::sleep(Duration::from_millis(25));
                    let e = app.inner.lock().unwrap();
                    for c in e.channels.values() {
                        let seconds = c["audio_seconds"].as_f64().unwrap_or(0.);
                        if c["protocol"] == "FM" {
                            analog = analog.max(seconds);
                        } else if seconds > best.0 {
                            best = (seconds, c["id"].as_str().unwrap_or("").to_string());
                        }
                    }
                }
                assert!(analog < 0.5, "digital DMR audio leaked into the analog FM path: {analog} s");
                best
            })
        };
        ingest(input, app.clone()).unwrap();
        let best = observer.join().unwrap();
        println!("DMR fixture decoded audio: {} s on {}", best.0, best.1);
        assert!(best.0 > 0.5, "no decoded DMR audio: {best:?}");
    }

    /// 100 ksps coarse slice centered at `center` carrying a strong FM carrier at
    /// `carrier` over noise, as interleaved i16 IQ.
    fn slice_payload(carrier: f64, center: f64, seed: u64) -> Vec<u8> {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut gauss = move || {
            let mut u = || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                ((state >> 11) as f64 + 0.5) / (1u64 << 53) as f64
            };
            (-2. * u().ln()).sqrt() * (2. * std::f64::consts::PI * u()).cos()
        };
        let mut out = Vec::new();
        for i in 0..10000 {
            let t = i as f64 / 100000.;
            let phase = 2. * std::f64::consts::PI * (carrier - center) * t
                + 2.5 * (2. * std::f64::consts::PI * 1000. * t).sin();
            let (re, im) = (3000. * phase.cos() + 20. * gauss(), 3000. * phase.sin() + 20. * gauss());
            out.extend_from_slice(&(re as i16).to_le_bytes());
            out.extend_from_slice(&(im as i16).to_le_bytes());
        }
        out
    }
    /// Several logical targets share one physical 100 ksps slice. The slice is RF-active
    /// because of one carrier; only that target may report RF activity, and the
    /// physical slice's activity must stay visible separately.
    #[test]
    fn shared_slice_reports_target_activity_apart_from_slice_activity() {
        let center = 454_100_000.;
        let targets = [454_075_000u64, 454_087_500, 454_100_000, 454_112_500];
        let config = json!({"input_center_hz":454100000u64,"input_rate_hz":25600000,"input_bandwidth_hz":20000000,
            "streams":[{"stream_id":7,"center_hz":454100000u64,"rate_hz":100000,"protocol":"DMR","iq_forwarded":true,
                "targets":targets.iter().map(|t| json!({"target_hz":t,"protocol":"DMR"}))
                    // DMR slot 2 of the carrier's channel (same frequency) and one target the
                    // slice cannot contain.
                    .chain([json!({"target_hz":454_087_500u64,"protocol":"DMR","slot":2}), json!({"target_hz":454_300_000u64,"protocol":"DMR"})])
                    .collect::<Vec<_>>()}],
            "psd":{"unit":"dBFS"}});
        let mut bytes = Vec::new();
        let mut cfg = frame(4, 1, 0, 0, 0, config.to_string().into_bytes());
        cfg.center_hz = 454_100_000;
        cfg.write(&mut bytes).unwrap();
        let mut sequence = 1;
        let mut sample = 0;
        for packet in 0..4 {
            let mut act = frame(3, 1, 7, sequence, sample, br#"{"rf_active":true,"level_db":-25.0}"#.to_vec());
            act.rate = 100000;
            act.center_hz = 454_100_000;
            act.write(&mut bytes).unwrap();
            let mut iq = frame(1, 1, 7, sequence + 1, sample, slice_payload(454_087_500., center, packet));
            iq.rate = 100000;
            iq.center_hz = 454_100_000;
            iq.write(&mut bytes).unwrap();
            sequence += 2;
            sample += 10000;
        }
        let (release, rx) = mpsc::channel();
        let input = HeldOpen { data: std::io::Cursor::new(bytes), release: rx };
        let app = App::new(8);
        let runner = {
            let app = app.clone();
            thread::spawn(move || ingest(input, app))
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let snapshot = loop {
            let snap: HashMap<u64, Value> = {
                let e = app.inner.lock().unwrap();
                e.channels
                    .values()
                    .filter(|c| c["protocol"] == "DMR" && c["slot"] == 1)
                    .map(|c| (c["logical_hz"].as_f64().unwrap() as u64, c.clone()))
                    .collect()
            };
            if snap.get(&454_087_500).is_some_and(|c| c["rf_active"] == true)
                || Instant::now() > deadline
            {
                break snap;
            }
            thread::sleep(Duration::from_millis(10));
        };
        let (out_of_slice, slot2_active) = {
            let e = app.inner.lock().unwrap();
            (
                e.source["out_of_slice_targets"].as_u64().unwrap_or(0),
                e.channels.get("454087500-DMR-2").map(|c| c["rf_active"] == true),
            )
        };
        release.send(()).unwrap();
        runner.join().unwrap().unwrap();
        assert_eq!(slot2_active, Some(true), "slot 2 of the active carrier shares its activity");
        assert_eq!(snapshot.len(), 4, "the out-of-slice target must not enter the catalog");
        assert!(!snapshot.contains_key(&454_300_000));
        assert_eq!(out_of_slice, 1);
        for (frequency, c) in &snapshot {
            assert_eq!(c["slice_rf_active"], true, "{frequency}: physical slice activity");
            assert_eq!(c["slice_targets"], 4);
            assert_eq!(c["rf_active"] == true, *frequency == 454_087_500, "{frequency}: {c}");
        }
        let level = snapshot[&454_087_500]["level_db"].as_f64().unwrap();
        // 3000-amplitude carrier, 16-bit full scale
        assert!((level - 20. * (3000f64 / 32768.).log10()).abs() < 1., "level {level}");
        assert_eq!(snapshot[&454_075_000]["slice_level_db"], -25.0);
    }
    #[test]
    fn catalog_survives_retune_without_reusing_channel_identity() {
        let app = App::new(3);
        let mut bytes = Vec::new();
        frame(4, 1, 0, 0, 0, configuration(454029223))
            .write(&mut bytes)
            .unwrap();
        frame(4, 2, 0, 0, 0, configuration(452685504))
            .write(&mut bytes)
            .unwrap();
        ingest(&bytes[..], app.clone()).unwrap();
        let e = app.inner.lock().unwrap();
        assert_eq!(e.channels.len(), 2);
        assert_eq!(e.channels["454029200-Unknown-1"]["in_range"], false);
        assert_eq!(e.channels["452685500-Unknown-1"]["in_range"], true);
    }
}

/// Slim copy of a channel entry for restart persistence.
fn persisted_entry(c: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for key in [
        "id",
        "protocol",
        "slot",
        "kind",
        "frequency_hz",
        "frequency_snapped",
        "label",
        "label_kind",
        "label_detail",
        "carrier_hz",
        "carrier_snapped",
        "decoded",
        "audio_seconds",
        "last_heard",
        "last_rf",
        "sequence",
    ] {
        out.insert(key.into(), c[key].clone());
    }
    Value::Object(out)
}
/// Put catalog entries saved by an earlier run back, marked out of range until a source
/// declares them again.
pub fn restore_catalog(app: &App) {
    let mut e = app.inner.lock().unwrap();
    let known: Vec<(String, Value)> = e.store.known.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    for (id, saved) in known {
        let mut entry = make_channel_json(0, 0., saved["frequency_hz"].as_f64().unwrap_or(0.), false, saved["protocol"].as_str().unwrap_or("Unknown"), saved["slot"].as_u64().unwrap_or(1) as u8);
        if let (Some(fields), Some(old)) = (entry.as_object_mut(), saved.as_object()) {
            fields.extend(old.clone());
        }
        entry["id"] = json!(id.clone());
        entry["in_range"] = json!(false);
        entry["available"] = json!(false);
        entry["logical_hz"] = Value::Null;
        entry["persisted"] = json!(true);
        e.channels.entry(id).or_insert(entry);
    }
}
/// Background upkeep: close finished calls, accumulate activity history, persist.
pub fn spawn_janitor(app: App) {
    restore_catalog(&app);
    thread::spawn(move || {
        let mut last = Instant::now();
        let mut last_save = Instant::now();
        loop {
            thread::sleep(Duration::from_millis(500));
            let dt = last.elapsed().as_secs_f32();
            last = Instant::now();
            let at = now();
            let mut e = app.inner.lock().unwrap();
            let events = e.store.close_idle(at);
            for event in events {
                let _ = app.events.send(json!({"type":"call","phase":event.phase,"generation":e.generation,"call":event.call}));
            }
            let observed: Vec<(String, bool, bool, Option<Value>)> = e
                .channels
                .values()
                .map(|c| {
                    let keep = c["decoded"] == true || c["carrier_snapped"] == true;
                    (
                        c["id"].as_str().unwrap_or("").to_string(),
                        c["rf_active"] == true,
                        c["speech_active"] == true,
                        keep.then(|| persisted_entry(c)),
                    )
                })
                .collect();
            for (id, rf, talk, entry) in observed {
                e.store.add_activity(&id, at, if rf { dt } else { 0. }, if talk { dt } else { 0. });
                if let Some(entry) = entry
                    && e.store.known.get(&id) != Some(&entry)
                {
                    e.store.known.insert(id, entry);
                    e.store.known_dirty = true;
                }
            }
            if last_save.elapsed() > Duration::from_secs(10) {
                e.store.save();
                last_save = Instant::now();
            }
        }
    });
}
/// Flush persistent state now (graceful shutdown).
pub fn flush(app: &App) {
    app.inner.lock().unwrap().store.save();
}
