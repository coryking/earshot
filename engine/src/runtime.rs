use crate::{
    codec::Decoder,
    dsp::ChannelDsp,
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
#[derive(Clone)]
pub struct App {
    pub(crate) inner: Arc<Mutex<Engine>>,
    pub(crate) audio: broadcast::Sender<Audio>,
    pub(crate) max_decoders: usize,
}
#[derive(Clone)]
pub(crate) struct Audio {
    pub(crate) meta: Value,
    pub(crate) pcm: Vec<u8>,
}
pub(crate) struct Engine {
    pub(crate) source: Value,
    pub(crate) channels: HashMap<String, Value>,
    worker_epochs: HashMap<u32, u64>,
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
        let (audio, _) = broadcast::channel(16);
        Self {
            inner: Arc::new(Mutex::new(Engine {
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
            max_decoders,
        }
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
        for c in e.channels.values_mut() {
            c["available"] = json!(false);
            c["decoder_admitted"] = json!(false);
            c["rf_active"] = json!(false);
            c["speech_active"] = json!(false);
        }
    }
}
struct Work {
    frame: Frame,
    received: Instant,
}
struct Worker {
    sender: mpsc::SyncSender<Work>,
    queued: Arc<AtomicUsize>,
    cost: Arc<AtomicUsize>,
}
fn selected_stream(e: &Engine) -> Option<u32> {
    e.selected
        .as_deref()
        .and_then(|id| e.channels.get(id))
        .and_then(|c| c["stream_id"].as_u64())
        .map(|v| v as u32)
}
fn channel_id(frequency: f64, protocol: &str, slot: u8) -> String {
    format!("{frequency:.0}-{protocol}-{slot}")
}
fn make_channel(stream: u32, frequency: f64, protocol: &str, slot: u8) -> Value {
    json!({"id":channel_id(frequency,protocol,slot),"stream_id":stream,"frequency_hz":frequency,"protocol":protocol,"slot":slot,"kind":"unidentified","rf_active":false,"decoded":false,"speech_active":false,"last_heard":null,"last_rf":null,"level_db":null,"audio_seconds":0.,"sequence":0,"in_range":true,"available":false})
}
fn modes(cfg: &Value) -> Vec<(&'static str, u8)> {
    match cfg["protocol"].as_str() {
        Some("DMR") => vec![("DMR", 1), ("DMR", 2)],
        Some("NXDN48") => vec![("NXDN48", 1)],
        _ => vec![("DMR", 1), ("DMR", 2), ("NXDN48", 1)],
    }
}
static WORKER_EPOCH: AtomicU64 = AtomicU64::new(1);
fn worker(app: App, stream: u32, cfg: Value) -> Result<Worker> {
    let epoch = WORKER_EPOCH.fetch_add(1, Ordering::Relaxed);
    {
        let mut e = app.inner.lock().unwrap();
        e.worker_epochs.insert(stream, epoch);
        if selected_stream(&e) == Some(stream) {
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
        let mut dsp = None;
        let mut generation = None;

        let mut expected_sample = None;
        let mut live_samples = 0u64;
        let mut saw_preroll = false;
        let mut rate = 0;
        while let Ok(work) = rx.recv() {
            worker_queued.fetch_sub(1, Ordering::Relaxed);
            let frame = work.frame;
            let frequency = cfg["target_hz"].as_f64().unwrap_or(frame.center_hz as f64);
            let gap = generation.is_some()
                && (generation != Some(frame.generation)
                    || frame.rate != rate
                    || frame.flags & (DISCONTINUITY | OVERFLOW | START) != 0
                    || expected_sample.is_some_and(|v| v != frame.first_sample));
            if gap || decoders.is_empty() {
                decoders = modes
                    .iter()
                    .filter_map(|&(p, s)| Decoder::new(p, s).ok().map(|d| (p, s, d)))
                    .collect();
                dsp = Some(ChannelDsp::new(
                    cfg["sample_rate_num_hz"]
                        .as_f64()
                        .zip(cfg["sample_rate_den"].as_f64())
                        .filter(|(_, den)| *den > 0.)
                        .map(|(num, den)| num / den)
                        .unwrap_or(frame.rate as f64),
                    frequency - frame.center_hz as f64,
                ));
                generation = Some(frame.generation);
                rate = frame.rate;
                live_samples = 0;
                saw_preroll = false;
                let mut e = app.inner.lock().unwrap();
                if gap {
                    e.drops += 1;
                    if selected_stream(&e) == Some(stream) {
                        e.decoder_generation += 1;
                    }
                }
            }

            expected_sample = Some(frame.first_sample + frame.payload.len() as u64 / 4);
            let iq: Vec<_> = frame
                .payload
                .chunks_exact(4)
                .map(|b| {
                    Complex32::new(
                        i16::from_le_bytes([b[0], b[1]]) as f32,
                        i16::from_le_bytes([b[2], b[3]]) as f32,
                    )
                })
                .collect();
            let pcm = dsp.as_mut().unwrap().push(&iq);
            if frame.flags & PREROLL != 0 {
                live_samples = 0;
                saw_preroll = true;
            } else {
                live_samples += pcm.len() as u64;
            }
            let mut protocol_evidence = None;
            for (protocol, slot, decoder) in &mut decoders {
                let (audio, confirmed) = decoder.push(&pcm);
                let speech = confirmed && !audio.is_empty() && frame.flags & PREROLL == 0;
                let id = channel_id(frequency, protocol, *slot);
                let mut e = app.inner.lock().unwrap();
                if frame.generation != e.generation
                    || !e.active
                    || e.worker_epochs.get(&stream) != Some(&epoch)
                {
                    continue;
                }
                if confirmed && !audio.is_empty() {
                    protocol_evidence = Some(*protocol);
                    let entry = e
                        .channels
                        .entry(id.clone())
                        .or_insert_with(|| make_channel(stream, frequency, protocol, *slot));
                    entry["decoded"] = json!(true);
                    entry["kind"] = json!("voice");
                    entry["last_heard"] = json!(now());
                    entry["available"] = json!(true);
                    entry["speech_active"] = json!(speech);
                    entry["sequence"] = json!(entry["sequence"].as_u64().unwrap_or(0) + 1);
                    entry["audio_seconds"] = json!(
                        entry["audio_seconds"].as_f64().unwrap_or(0.) + audio.len() as f64 / 8000.
                    );
                } else if let Some(entry) = e.channels.get_mut(&id) {
                    entry["speech_active"] = json!(false);
                    entry["available"] = json!(true);
                }
                let selected = e.selected.as_deref() == Some(&id);
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
                    e.listener = json!({"status":"listening","state":"listening","channel_id":id,"voice_active":true,"generation":decoder_generation,"source_generation":source_generation,"sequence":sequence,"audio_chunks":sequence,"last_audio_at":now(),"ingress_age_ms":ingress,"ingress_age_basis":"host ingress receipt; device speech alignment unknown","dropped_blocks":e.drops,"input_dropped":e.drops,"queue_blocks":0,"error":null});
                    let _=app.audio.send(Audio{meta:json!({"type":"audio","encoding":"s16le","channels":1,"rate_hz":8000,"samples":audio.len(),"channel_id":id,"generation":decoder_generation,"source_generation":source_generation,"sequence":sequence,"ingress_age_ms":ingress,"ingress_age_basis":"host ingress receipt; device speech alignment unknown"}),pcm:bytes});
                }
            }
            if let Some(protocol) = protocol_evidence {
                decoders.retain(|(p, _, _)| *p == protocol);
                worker_cost.store(decoders.len(), Ordering::Relaxed);
            }
        }
    });
    Ok(Worker {
        sender: tx,
        queued,
        cost,
    })
}
fn schedule(
    app: &App,
    configs: &HashMap<u32, Value>,
    workers: &mut HashMap<u32, Worker>,
) -> Result<()> {
    let e = app.inner.lock().unwrap();
    let selected = e
        .selected
        .as_deref()
        .and_then(|id| e.channels.get(id))
        .and_then(|c| c["stream_id"].as_u64())
        .map(|v| v as u32);
    let mut effective = configs.clone();
    for (&stream, cfg) in &mut effective {
        if cfg["protocol"].as_str().unwrap_or("Unknown") == "Unknown"
            && let Some(protocol) = e
                .channels
                .values()
                .find(|c| c["stream_id"] == stream && c["in_range"] == true && c["decoded"] == true)
                .and_then(|c| c["protocol"].as_str())
        {
            cfg["protocol"] = json!(protocol);
        }
    }
    let mut streams: Vec<_> = effective.iter().collect();
    streams.sort_by(|(a, _), (b, _)| {
        let priority = |s: &&u32| {
            let active = e
                .channels
                .values()
                .any(|c| c["stream_id"] == **s && c["rf_active"] == true);
            (Some(**s) != selected, !active, **s)
        };
        priority(a).cmp(&priority(b))
    });
    drop(e);
    let mut remaining = app.max_decoders;
    let mut admitted = Vec::new();
    for (&stream, cfg) in streams {
        let cost = workers
            .get(&stream)
            .map(|w| w.cost.load(Ordering::Relaxed))
            .unwrap_or_else(|| modes(cfg).len());
        if cost <= remaining {
            admitted.push(stream);
            remaining -= cost;
        }
    }
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
    for c in e.channels.values_mut() {
        let stream = c["stream_id"].as_u64().unwrap_or(u64::MAX) as u32;
        c["available"] = json!(workers.contains_key(&stream) && c["in_range"] == true);
        c["decoder_admitted"] = c["available"].clone();
    }
    Ok(())
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
    let mut consumed: HashMap<u32, u64> = HashMap::new();
    loop {
        let mut frame = match Frame::read(&mut input) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                app.error("RIQ source disconnected".into());
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
                consumed.clear();
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
                    channel["speech_active"] = json!(false);
                }
            }
            for cfg in streams {
                let stream = cfg["stream_id"].as_u64().context("stream_id missing")?;
                anyhow::ensure!(stream <= u32::MAX as u64, "stream_id overflow");
                let stream = stream as u32;
                let frequency = cfg["target_hz"]
                    .as_f64()
                    .or_else(|| cfg["center_hz"].as_f64())
                    .context("frequency missing")?;
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
                let slot = cfg["slot"].as_u64().unwrap_or(1) as u8;
                let id = channel_id(frequency, protocol, slot);
                let channel = e
                    .channels
                    .entry(id)
                    .or_insert_with(|| make_channel(stream, frequency, protocol, slot));
                channel["stream_id"] = json!(stream);
                channel["in_range"] = json!(true);
                for channel in e.channels.values_mut() {
                    if channel["frequency_hz"] == frequency {
                        channel["stream_id"] = json!(stream);
                        channel["in_range"] = json!(true);
                    }
                }
                configs.insert(stream, cfg.clone());
            }
            drop(e);
            schedule(&app, &configs, &mut workers)?;
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
            e.source["source_position_s"] =
                json!(frame.first_sample as f64 / frame.rate.max(1) as f64);
            e.source["last_samples_at"] = json!(now());
        }
        if scheduled.elapsed() > Duration::from_millis(100) {
            schedule(&app, &configs, &mut workers)?;
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
                if let Some(&previous) = consumed.get(&frame.stream_id)
                    && frame.first_sample < previous
                {
                    let trim =
                        ((previous - frame.first_sample) as usize).min(frame.payload.len() / 4);
                    frame.payload.drain(..trim * 4);
                    frame.first_sample += trim as u64;
                    if frame.payload.is_empty() {
                        continue;
                    }
                }
                let stream_id = frame.stream_id;
                let sample_end = frame
                    .first_sample
                    .checked_add(frame.payload.len() as u64 / 4)
                    .context("IQ sample counter overflow")?;
                if let Some(w) = workers.get(&frame.stream_id) {
                    w.queued.fetch_add(1, Ordering::Relaxed);
                    if w.sender
                        .try_send(Work {
                            frame,
                            received: Instant::now(),
                        })
                        .is_err()
                    {
                        w.queued.fetch_sub(1, Ordering::Relaxed);
                        app.inner.lock().unwrap().drops += 1;
                        pending_gaps.insert(stream_id);
                    } else {
                        consumed.insert(stream_id, sample_end);
                    }
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
                while e.psd.len() > 120 {
                    e.psd.pop_front();
                }
            }
            3 => {
                let activity: Value = serde_json::from_slice(&frame.payload)?;
                let mut e = app.inner.lock().unwrap();
                for c in e
                    .channels
                    .values_mut()
                    .filter(|c| c["stream_id"] == frame.stream_id)
                {
                    c["rf_active"] = activity["rf_active"].clone();
                    c["level_db"] = activity["level_db"].clone();
                    if activity["rf_active"] == true {
                        c["last_rf"] = json!(now());
                    } else {
                        c["speech_active"] = json!(false);
                    }
                }
            }
            5 => {
                workers.remove(&frame.stream_id);
                let mut e = app.inner.lock().unwrap();
                e.drops += 1;
                e.decoder_generation += 1;
                for c in e
                    .channels
                    .values_mut()
                    .filter(|c| c["stream_id"] == frame.stream_id)
                {
                    c["speech_active"] = json!(false);
                    c["available"] = json!(false);
                }
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
        assert_eq!(e.channels["454029223-Unknown-1"]["in_range"], false);
        assert_eq!(e.channels["452685504-Unknown-1"]["in_range"], true);
    }
}
