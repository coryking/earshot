//! Recent calls (with audio), activity history and the persisted, user-labeled catalog.
//!
//! Bounded in memory: calls by count, age and audio bytes; activity by age. With a state
//! directory the call audio (WAV files), the call index, the activity buckets (one file per
//! day) and the catalog survive restarts. All disk writes go through one writer thread, so
//! nothing here does file I/O while the engine lock is held; a failed write is retried on
//! the next save and counted in `io_errors`.
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
};

pub const AUDIO_RATE: u32 = 8000;
/// A call ends after this long without audio.
pub const CALL_HANG_S: f64 = 2.0;
/// Shorter bursts are discarded (false confirmations, key-up blips); no start event is
/// sent for them either.
pub const MIN_CALL_S: f64 = 0.8;
/// A single call is split after this long.
pub const MAX_CALL_S: f64 = 600.;
pub const MAX_CALLS: usize = 500;
pub const MAX_CALL_AGE_S: f64 = 7. * 86400.;
/// Activity buckets are 5 minutes wide and kept for 14 days.
pub const BUCKET_S: u64 = 300;
pub const ACTIVITY_KEEP_S: u64 = 14 * 86400;
/// Operator edits are capped so a client cannot grow the store without bound.
pub const MAX_USER_ENTRIES: usize = 5000;

/// Mono signed16 PCM at 8 kHz in a canonical 44-byte-header WAV.
pub fn wav_bytes(samples: &[i16]) -> Vec<u8> {
    let data = samples.len() * 2;
    let mut out = Vec::with_capacity(44 + data);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&AUDIO_RATE.to_le_bytes());
    out.extend_from_slice(&(AUDIO_RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data as u32).to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

enum Job {
    Write {
        kind: String,
        path: PathBuf,
        bytes: Vec<u8>,
    },
    Remove(PathBuf),
    Barrier(mpsc::Sender<()>),
}
/// Single writer thread for everything the store persists.
struct Io {
    tx: mpsc::Sender<Job>,
    failed: Arc<Mutex<HashSet<String>>>,
    errors: Arc<AtomicU64>,
}
impl Io {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        let failed: Arc<Mutex<HashSet<String>>> = Arc::default();
        let errors = Arc::new(AtomicU64::new(0));
        let (f, n) = (failed.clone(), errors.clone());
        thread::spawn(move || {
            for job in rx {
                match job {
                    Job::Write { kind, path, bytes } => {
                        let tmp = path.with_extension("tmp");
                        let result = fs::write(&tmp, &bytes).and_then(|()| fs::rename(&tmp, &path));
                        if let Err(e) = result {
                            let _ = fs::remove_file(&tmp);
                            if n.fetch_add(1, Ordering::Relaxed) < 5 {
                                eprintln!("radio-engine: cannot write {}: {e}", path.display());
                            }
                            f.lock().unwrap().insert(kind);
                        }
                    }
                    Job::Remove(path) => {
                        let _ = fs::remove_file(path);
                    }
                    Job::Barrier(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
        Self { tx, failed, errors }
    }
    fn write(&self, kind: &str, path: PathBuf, bytes: Vec<u8>) {
        let _ = self.tx.send(Job::Write {
            kind: kind.into(),
            path,
            bytes,
        });
    }
}
/// Fail fast when the state directory cannot be used.
pub fn probe_dir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir.join("calls"))?;
    let probe = dir.join(".write-probe");
    fs::write(&probe, b"ok")?;
    fs::remove_file(probe)
}
/// Read a JSON state file. A file that exists but cannot be parsed is renamed aside
/// (`.corrupt`) and reported, never silently replaced by empty state.
fn read_json(path: &Path) -> Option<Value> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            eprintln!("radio-engine: cannot read {}: {e}", path.display());
            return None;
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(v) => Some(v),
        Err(e) => {
            let aside = path.with_extension("corrupt");
            eprintln!(
                "radio-engine: {} is not valid JSON ({e}); moved to {}",
                path.display(),
                aside.display()
            );
            let _ = fs::rename(path, aside);
            None
        }
    }
}

struct OpenCall {
    channel_id: String,
    source_id: String,
    protocol: String,
    slot: u64,
    frequency_hz: f64,
    label: Value,
    started: f64,
    last_audio: f64,
    samples: Vec<i16>,
    level_sum: f64,
    level_n: u32,
    /// Assigned once the call is long enough to count; start/end events carry it.
    id: Option<u64>,
}
impl OpenCall {
    fn wire(
        &self,
        id: u64,
        ended: Option<f64>,
        audio_url: Option<String>,
        duration: f64,
        level: Option<f64>,
    ) -> Value {
        json!({
            "id": id.to_string(),
            "source_id": self.source_id,
            "channel_id": self.channel_id,
            "protocol": self.protocol,
            "slot": self.slot,
            "frequency_hz": self.frequency_hz,
            "label": self.label,
            "started_at": self.started,
            "ended_at": ended,
            "in_progress": ended.is_none(),
            "duration_s": duration,
            "level_db": level,
            "audio_url": audio_url,
            "audio_format": "audio/wav; 8 kHz mono signed16",
            "talker": null,
            "transcript": null,
            "encrypted": false,
        })
    }
}
pub struct Call {
    pub meta: Value,
    id: u64,
    audio: Option<Arc<Vec<i16>>>,
}
/// Where a call's audio can be read from, decided under the lock and read outside it.
pub enum AudioSource {
    Memory(Arc<Vec<i16>>),
    File(PathBuf),
}
pub struct CallEvent {
    pub phase: &'static str,
    pub call: Value,
}
pub struct Store {
    dir: Option<PathBuf>,
    io: Option<Io>,
    calls: VecDeque<Call>,
    open: HashMap<String, OpenCall>,
    next_id: u64,
    memory_budget: usize,
    memory_used: usize,
    /// (bucket start, channel id) -> (rf seconds, talk seconds); bucket-first so a time
    /// range is a range scan.
    activity: BTreeMap<(u64, String), (f32, f32)>,
    /// bucket start -> talk seconds summed over channels, for baselines.
    totals: BTreeMap<u64, f32>,
    dirty_days: HashSet<u64>,
    /// User edits keyed by catalog source id.
    pub user: BTreeMap<String, Value>,
    pub user_dirty: bool,
    /// Last known catalog entries, for restart persistence.
    pub known: BTreeMap<String, Value>,
    pub known_dirty: bool,
    calls_dirty: bool,
}
impl Store {
    pub fn new(dir: Option<PathBuf>, memory_mb: usize) -> Self {
        let mut store = Self {
            io: dir.as_ref().map(|_| Io::new()),
            dir,
            calls: VecDeque::new(),
            open: HashMap::new(),
            // Ids are unique across restarts even when nothing persisted: seed from the clock.
            next_id: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(1, |d| d.as_secs()),
            memory_budget: memory_mb * 1024 * 1024,
            memory_used: 0,
            activity: BTreeMap::new(),
            totals: BTreeMap::new(),
            dirty_days: HashSet::new(),
            user: BTreeMap::new(),
            user_dirty: false,
            known: BTreeMap::new(),
            known_dirty: false,
            calls_dirty: false,
        };
        store.load();
        store
    }
    pub fn io_errors(&self) -> u64 {
        self.io
            .as_ref()
            .map_or(0, |io| io.errors.load(Ordering::Relaxed))
    }
    pub fn persistent(&self) -> bool {
        self.dir.is_some()
    }
    /// Wait until everything queued so far is on disk (tests, shutdown).
    pub fn sync(&self) {
        if let Some(io) = &self.io {
            let (tx, rx) = mpsc::channel();
            if io.tx.send(Job::Barrier(tx)).is_ok() {
                let _ = rx.recv();
            }
        }
    }
    fn load(&mut self) {
        let Some(dir) = self.dir.clone() else { return };
        let _ = fs::create_dir_all(dir.join("calls"));
        if let Some(Value::Array(calls)) = read_json(&dir.join("calls.json")) {
            for mut meta in calls {
                let Some(id) = meta["id"].as_str().and_then(|s| s.parse::<u64>().ok()) else {
                    continue;
                };
                // Drop index entries whose audio vanished.
                if meta["audio_url"].is_string()
                    && !dir.join("calls").join(format!("{id}.wav")).exists()
                {
                    meta["audio_url"] = Value::Null;
                }
                self.next_id = self.next_id.max(id.saturating_add(1));
                self.calls.push_back(Call {
                    meta,
                    id,
                    audio: None,
                });
            }
        }
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("activity-")
                    && name.ends_with(".json")
                    && let Some(Value::Object(map)) = read_json(&entry.path())
                {
                    for (key, v) in map {
                        if let Some((start, id)) = key.split_once('@')
                            && let (Ok(start), Some(rf), Some(talk)) =
                                (start.parse::<u64>(), v[0].as_f64(), v[1].as_f64())
                        {
                            self.activity
                                .insert((start, id.to_string()), (rf as f32, talk as f32));
                            *self.totals.entry(start).or_insert(0.) += talk as f32;
                        }
                    }
                }
            }
        }
        if let Some(Value::Object(user)) = read_json(&dir.join("catalog-user.json")) {
            self.user = user.into_iter().collect();
        }
        if let Some(Value::Object(known)) = read_json(&dir.join("catalog.json")) {
            self.known = known.into_iter().collect();
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0., |d| d.as_secs_f64());
        self.prune(now);
    }
    /// Queue whatever changed for the writer thread. Cheap enough to call every few seconds;
    /// serialization is bounded by one day of activity, not the whole history.
    pub fn save(&mut self) {
        let Some(io) = &self.io else { return };
        // Writes that failed last time are retried now.
        let failed: Vec<String> = io.failed.lock().unwrap().drain().collect();
        for kind in failed {
            match kind.as_str() {
                "calls" => self.calls_dirty = true,
                "user" => self.user_dirty = true,
                "known" => self.known_dirty = true,
                day => {
                    if let Some(d) = day.strip_prefix("activity-").and_then(|d| d.parse().ok()) {
                        self.dirty_days.insert(d);
                    }
                }
            }
        }
        let Some(io) = &self.io else { return };
        let json = |v: &Value| serde_json::to_vec(v).unwrap_or_default();
        let dir = self.dir.clone().unwrap_or_default();
        for day in std::mem::take(&mut self.dirty_days) {
            let lo = day * 86400;
            let map: serde_json::Map<String, Value> = self
                .activity
                .range((lo, String::new())..(lo + 86400, String::new()))
                .map(|((start, id), (rf, talk))| (format!("{start}@{id}"), json!([rf, talk])))
                .collect();
            io.write(
                &format!("activity-{day}"),
                dir.join(format!("activity-{day}.json")),
                json(&Value::Object(map)),
            );
        }
        if self.user_dirty {
            let map: serde_json::Map<String, Value> = self.user.clone().into_iter().collect();
            io.write(
                "user",
                dir.join("catalog-user.json"),
                json(&Value::Object(map)),
            );
            self.user_dirty = false;
        }
        if self.known_dirty {
            let map: serde_json::Map<String, Value> = self.known.clone().into_iter().collect();
            io.write("known", dir.join("catalog.json"), json(&Value::Object(map)));
            self.known_dirty = false;
        }
        if self.calls_dirty {
            let list: Vec<&Value> = self.calls.iter().map(|c| &c.meta).collect();
            io.write("calls", dir.join("calls.json"), json(&json!(list)));
            self.calls_dirty = false;
        }
    }
    /// Append audio heard on a channel. Emits a "start" event once the call is long
    /// enough to be kept, so a discarded burst never produces a dangling start.
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &mut self,
        channel_id: &str,
        source_id: &str,
        protocol: &str,
        slot: u64,
        frequency_hz: f64,
        label: &Value,
        level_db: Option<f64>,
        audio: &[i16],
        at: f64,
    ) -> Vec<CallEvent> {
        let mut events = Vec::new();
        if let Some(open) = self.open.get(channel_id)
            && (at - open.started > MAX_CALL_S || at - open.last_audio > CALL_HANG_S)
        {
            events.extend(self.close(channel_id));
        }
        let open = self
            .open
            .entry(channel_id.to_string())
            .or_insert_with(|| OpenCall {
                channel_id: channel_id.to_string(),
                source_id: source_id.to_string(),
                protocol: protocol.to_string(),
                slot,
                frequency_hz,
                label: label.clone(),
                started: at - audio.len() as f64 / AUDIO_RATE as f64,
                last_audio: at,
                samples: Vec::new(),
                level_sum: 0.,
                level_n: 0,
                id: None,
            });
        open.samples.extend_from_slice(audio);
        open.last_audio = at;
        if let Some(l) = level_db {
            open.level_sum += l;
            open.level_n += 1;
        }
        let duration = open.samples.len() as f64 / AUDIO_RATE as f64;
        if duration >= MIN_CALL_S && open.id.is_none() {
            let id = self.next_id;
            self.next_id += 1;
            let open = self.open.get_mut(channel_id).expect("just inserted");
            open.id = Some(id);
            events.push(CallEvent {
                phase: "start",
                call: open.wire(id, None, None, duration, None),
            });
        }
        events
    }
    /// Close calls that have been silent for the hang time (call periodically).
    pub fn close_idle(&mut self, at: f64) -> Vec<CallEvent> {
        let mut idle: Vec<(f64, String)> = self
            .open
            .iter()
            .filter(|(_, c)| at - c.last_audio > CALL_HANG_S)
            .map(|(id, c)| (c.started, id.clone()))
            .collect();
        // Oldest first, so call ids follow the order calls began.
        idle.sort_by(|a, b| a.0.total_cmp(&b.0));
        idle.iter().flat_map(|(_, id)| self.close(id)).collect()
    }
    /// Close every open call (shutdown), keeping the ones long enough to count.
    pub fn close_all(&mut self) -> Vec<CallEvent> {
        let ids: Vec<String> = self.open.keys().cloned().collect();
        ids.iter().flat_map(|id| self.close(id)).collect()
    }
    fn close(&mut self, channel_id: &str) -> Option<CallEvent> {
        let open = self.open.remove(channel_id)?;
        let duration = open.samples.len() as f64 / AUDIO_RATE as f64;
        let id = open.id?; // never reached MIN_CALL_S: discarded without a trace
        let level = (open.level_n > 0).then(|| open.level_sum / open.level_n as f64);
        let url = Some(format!("/api/calls/{id}/audio.wav"));
        let meta = open.wire(id, Some(open.last_audio), url, duration, level);
        if let (Some(io), Some(dir)) = (&self.io, &self.dir) {
            io.write(
                "wav",
                dir.join("calls").join(format!("{id}.wav")),
                wav_bytes(&open.samples),
            );
        }
        self.memory_used += open.samples.len() * 2;
        let last = open.last_audio;
        self.calls.push_back(Call {
            meta: meta.clone(),
            id,
            audio: Some(Arc::new(open.samples)),
        });
        self.evict(last);
        self.calls_dirty = true;
        Some(CallEvent {
            phase: "end",
            call: meta,
        })
    }
    fn evict(&mut self, now: f64) {
        // Memory: drop the in-memory copy of the oldest audio first (the WAV remains).
        let mut index = 0;
        while self.memory_used > self.memory_budget && index < self.calls.len() {
            if let Some(audio) = self.calls[index].audio.take() {
                self.memory_used = self.memory_used.saturating_sub(audio.len() * 2);
                if self.dir.is_none() {
                    self.calls[index].meta["audio_url"] = Value::Null;
                }
            }
            index += 1;
        }
        while self.calls.len() > MAX_CALLS
            || self.calls.front().is_some_and(|c| {
                now - c.meta["started_at"].as_f64().unwrap_or(now) > MAX_CALL_AGE_S
            })
        {
            let Some(old) = self.calls.pop_front() else {
                break;
            };
            if let Some(audio) = &old.audio {
                self.memory_used = self.memory_used.saturating_sub(audio.len() * 2);
            }
            if let (Some(io), Some(dir)) = (&self.io, &self.dir) {
                let _ = io.tx.send(Job::Remove(
                    dir.join("calls").join(format!("{}.wav", old.id)),
                ));
            }
        }
    }
    /// Newest first. `before` is an exclusive upper bound on `started_at` (unix seconds); the
    /// second value is the cursor for the next page, or None when this is the last one.
    pub fn list(
        &self,
        before: Option<f64>,
        limit: usize,
        source: Option<&str>,
    ) -> (Vec<Value>, Option<f64>) {
        let mut all: Vec<&Value> = self
            .calls
            .iter()
            .map(|c| &c.meta)
            .filter(|m| {
                before.is_none_or(|b| m["started_at"].as_f64().unwrap_or(0.) < b)
                    && source.is_none_or(|s| m["source_id"] == s || m["channel_id"] == s)
            })
            .collect();
        all.sort_by(|a, b| {
            b["started_at"]
                .as_f64()
                .unwrap_or(0.)
                .total_cmp(&a["started_at"].as_f64().unwrap_or(0.))
        });
        let more = all.len() > limit;
        let page: Vec<Value> = all.into_iter().take(limit).cloned().collect();
        let next = more
            .then(|| page.last().and_then(|c| c["started_at"].as_f64()))
            .flatten();
        (page, next)
    }
    pub fn audio_source(&self, id: u64) -> Option<AudioSource> {
        let call = self.calls.iter().find(|c| c.id == id)?;
        if let Some(a) = &call.audio {
            return Some(AudioSource::Memory(a.clone()));
        }
        let path = self.dir.as_ref()?.join("calls").join(format!("{id}.wav"));
        Some(AudioSource::File(path))
    }
    pub fn open_calls(&self) -> usize {
        self.open.values().filter(|c| c.id.is_some()).count()
    }
    /// Operator edit count is bounded by `MAX_USER_ENTRIES`.
    pub fn can_add_user(&self, id: &str) -> bool {
        self.user.contains_key(id) || self.user.len() < MAX_USER_ENTRIES
    }

    /// Add `seconds` of RF/talk time for a channel at unix time `at`.
    pub fn add_activity(&mut self, channel_id: &str, at: f64, rf_s: f32, talk_s: f32) {
        if rf_s <= 0. && talk_s <= 0. {
            return;
        }
        let start = (at as u64) / BUCKET_S * BUCKET_S;
        let slot = self
            .activity
            .entry((start, channel_id.to_string()))
            .or_insert((0., 0.));
        slot.0 += rf_s;
        slot.1 += talk_s;
        *self.totals.entry(start).or_insert(0.) += talk_s;
        self.dirty_days.insert(start / 86400);
    }
    /// Drop history older than the retention window (and its day files).
    pub fn prune(&mut self, now: f64) {
        let cutoff = (now as u64).saturating_sub(ACTIVITY_KEEP_S) / BUCKET_S * BUCKET_S;
        let stale: Vec<(u64, String)> = self
            .activity
            .range(..(cutoff, String::new()))
            .map(|(k, _)| k.clone())
            .collect();
        for key in &stale {
            self.activity.remove(key);
        }
        let old_totals: Vec<u64> = self.totals.range(..cutoff).map(|(k, _)| *k).collect();
        for k in old_totals {
            self.totals.remove(&k);
        }
        if let (Some(io), Some(dir)) = (&self.io, &self.dir)
            && let Ok(entries) = fs::read_dir(dir)
        {
            let cutoff_day = cutoff / 86400;
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(day) = name
                    .strip_prefix("activity-")
                    .and_then(|n| n.strip_suffix(".json"))
                    .and_then(|d| d.parse::<u64>().ok())
                    && day < cutoff_day
                {
                    let _ = io.tx.send(Job::Remove(entry.path()));
                }
            }
        }
    }
    fn totals_in(&self, from: u64, to: u64) -> Option<f64> {
        let mut any = false;
        let mut sum = 0.;
        for (_, v) in self.totals.range(from..to) {
            any = true;
            sum += *v as f64;
        }
        any.then_some(sum)
    }
    /// Talk-seconds per bucket over a window ending at `now`, for the whole block and per
    /// source, plus a baseline from earlier weeks (same weekday and hour) or days (same hour).
    pub fn activity(&self, now: f64, window_s: u64, bucket_s: u64) -> Value {
        // Clamp bucket first so the window clamp's bounds can never cross.
        let bucket_s = bucket_s.clamp(BUCKET_S, ACTIVITY_KEEP_S) / BUCKET_S * BUCKET_S;
        let end = (now as u64) / bucket_s * bucket_s + bucket_s;
        let window_s = window_s.clamp(bucket_s, ACTIVITY_KEEP_S) / bucket_s * bucket_s;
        let start = end.saturating_sub(window_s);
        let n = ((end - start) / bucket_s) as usize;
        let mut total = vec![0f64; n];
        let mut rf_total = vec![0f64; n];
        let mut sources: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for ((bucket, id), (rf, talk)) in self
            .activity
            .range((start, String::new())..(end, String::new()))
        {
            let i = ((*bucket - start) / bucket_s) as usize;
            total[i] += *talk as f64;
            rf_total[i] += *rf as f64;
            sources.entry(id.clone()).or_insert_with(|| vec![0.; n])[i] += *talk as f64;
        }
        let baseline: Vec<Option<f64>> = (0..n)
            .map(|i| {
                let from = start + i as u64 * bucket_s;
                let to = from + bucket_s;
                let at = |back: u64| self.totals_in(from.checked_sub(back)?, to - back);
                let weeks: Vec<f64> = (1..=2u64).filter_map(|k| at(k * 7 * 86400)).collect();
                let days: Vec<f64> = (1..=6u64).filter_map(|k| at(k * 86400)).collect();
                let pool = if weeks.is_empty() { days } else { weeks };
                (!pool.is_empty()).then(|| pool.iter().sum::<f64>() / pool.len() as f64)
            })
            .collect();
        // Wire form: numbers throughout (0 where nothing was observed), null when nothing at
        // all has been learned yet.
        let baseline_wire = baseline
            .iter()
            .any(Option::is_some)
            .then(|| baseline.iter().map(|b| b.unwrap_or(0.)).collect::<Vec<_>>());
        json!({
            "start": start, "end": end, "window_s": window_s, "bucket_s": bucket_s,
            "block": total, "total": total, "rf_total": rf_total, "sources": sources,
            "baseline": baseline_wire,
            "baseline_basis": "mean talk-seconds in the same bucket 1-2 weeks earlier (same weekday), else 1-6 days earlier (same hour); 0 where unobserved; null until any history exists",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn audio(seconds: f64) -> Vec<i16> {
        vec![100; (seconds * AUDIO_RATE as f64) as usize]
    }
    fn record(store: &mut Store, id: &str, at: f64, seconds: f64) -> Vec<CallEvent> {
        store.record(
            id,
            "454018750-2",
            "DMR",
            2,
            454_018_750.,
            &Value::Null,
            Some(-30.),
            &audio(seconds),
            at,
        )
    }
    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }
    fn id_of(event: &CallEvent) -> u64 {
        event.call["id"].as_str().unwrap().parse().unwrap()
    }
    #[test]
    fn wav_header_is_canonical() {
        let w = wav_bytes(&[1, -2, 3]);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()), 36 + 6);
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 8000);
        assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()), 6);
        assert_eq!(w.len(), 44 + 6);
        assert_eq!(i16::from_le_bytes([w[46], w[47]]), -2);
    }
    #[test]
    fn a_call_starts_once_long_enough_ends_after_the_hang_and_pages_newest_first() {
        let mut store = Store::new(None, 8);
        assert!(
            record(&mut store, "a", 100.0, 0.5).is_empty(),
            "0.5 s is not yet a call"
        );
        let started = record(&mut store, "a", 100.6, 0.5);
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].phase, "start");
        let start_id = id_of(&started[0]);
        assert!(started[0].call["ended_at"].is_null());
        assert!(
            record(&mut store, "a", 101.2, 0.5).is_empty(),
            "same call continues"
        );
        assert!(store.close_idle(101.5).is_empty(), "not idle yet");
        let ended = store.close_idle(105.0);
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].phase, "end");
        assert_eq!(id_of(&ended[0]), start_id, "start and end share the id");
        assert!((ended[0].call["duration_s"].as_f64().unwrap() - 1.5).abs() < 1e-6);
        assert_eq!(ended[0].call["ended_at"], 101.2);
        record(&mut store, "b", 200.0, 1.5);
        store.close_idle(210.0);
        let (calls, next) = store.list(None, 10, None);
        assert_eq!(
            calls
                .iter()
                .map(|c| c["channel_id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["b", "a"]
        );
        assert!(next.is_none());
        let (one, next) = store.list(None, 1, None);
        assert_eq!(one.len(), 1);
        assert_eq!(
            next,
            one[0]["started_at"].as_f64(),
            "cursor is the last started_at"
        );
        let (older, last) = store.list(next, 10, None);
        assert_eq!(older[0]["channel_id"], "a");
        assert!(last.is_none());
        assert_eq!(
            store.list(None, 10, Some("454018750-2")).0.len(),
            2,
            "source filter by source_id"
        );
        match store.audio_source(start_id) {
            Some(AudioSource::Memory(a)) => assert_eq!(a.len(), 12000),
            _ => panic!("audio should be in memory"),
        }
        assert!(store.audio_source(7).is_none());
    }
    #[test]
    fn short_blips_leave_no_start_event_and_gaps_split_calls() {
        let mut store = Store::new(None, 8);
        assert!(record(&mut store, "a", 10.0, 0.3).is_empty());
        assert!(store.close_idle(20.0).is_empty(), "0.3 s is not a call");
        assert_eq!(store.open_calls(), 0);
        let first = record(&mut store, "a", 30.0, 1.0);
        assert_eq!(
            first.iter().map(|e| e.phase).collect::<Vec<_>>(),
            ["start"]
        );
        let split = record(&mut store, "a", 40.0, 1.0); // after the hang: previous ends, a new one starts
        assert_eq!(
            split.iter().map(|e| e.phase).collect::<Vec<_>>(),
            ["end", "start"]
        );
    }
    #[test]
    fn a_very_long_call_is_split_and_close_all_keeps_open_calls() {
        let mut store = Store::new(None, 64);
        let mut t = 1000.0;
        let mut starts = 0;
        // 11 minutes of continuous audio in 100 ms chunks.
        for _ in 0..6600 {
            starts += record(&mut store, "a", t, 0.1)
                .iter()
                .filter(|e| e.phase == "start")
                .count();
            t += 0.1;
        }
        assert_eq!(starts, 2, "MAX_CALL_S splits the call");
        assert_eq!(store.close_all().len(), 1);
        assert_eq!(
            store.list(None, 10, None).0.len(),
            2,
            "the split part and the open part are both kept"
        );
    }
    #[test]
    fn call_ids_are_clock_seeded_so_a_restart_cannot_reuse_them() {
        let mut a = Store::new(None, 8);
        record(&mut a, "x", 1.0, 1.0);
        let first = id_of(&a.close_idle(10.0)[0]);
        assert!(
            first > 1_000_000_000,
            "an id that cannot collide with a small restarted counter"
        );
    }
    #[test]
    fn memory_budget_drops_old_audio_but_the_wav_still_serves_after_a_restart() {
        let dir = temp("mem");
        let mut store = Store::new(Some(dir.clone()), 1);
        let mut ids = vec![];
        for i in 0..5 {
            record(&mut store, "a", i as f64 * 100., 40.0); // 640 kB each
            ids.push(id_of(&store.close_idle(i as f64 * 100. + 50.)[0]));
        }
        assert!(store.memory_used <= 1024 * 1024, "{}", store.memory_used);
        assert!(
            matches!(store.audio_source(ids[0]), Some(AudioSource::File(_))),
            "oldest audio evicted from memory"
        );
        store.save();
        store.sync();
        let reopened = Store::new(Some(dir.clone()), 1);
        assert_eq!(reopened.list(None, 10, None).0.len(), 5);
        match reopened.audio_source(ids[4]) {
            Some(AudioSource::File(path)) => {
                assert_eq!(fs::read(path).unwrap().len(), 44 + 40 * 8000 * 2)
            }
            _ => panic!("a restarted store reads from disk"),
        }
        let _ = fs::remove_dir_all(&dir);
    }
    #[test]
    fn eviction_by_count_removes_the_wav() {
        let dir = temp("count");
        let mut store = Store::new(Some(dir.clone()), 64);
        let mut first = 0;
        for i in 0..(MAX_CALLS + 3) {
            record(&mut store, "a", i as f64 * 10., 1.0);
            let id = id_of(&store.close_idle(i as f64 * 10. + 5.)[0]);
            if i == 0 {
                first = id;
            }
        }
        store.sync();
        assert_eq!(store.list(None, 1000, None).0.len(), MAX_CALLS);
        assert!(
            !dir.join("calls").join(format!("{first}.wav")).exists(),
            "evicted call's WAV is deleted"
        );
        let _ = fs::remove_dir_all(&dir);
    }
    #[test]
    fn activity_buckets_aggregate_and_baseline_uses_the_same_hour_earlier() {
        let mut store = Store::new(None, 1);
        let day = 86400.;
        let t0 = 1_800_000_000.0f64;
        store.add_activity("a", t0 - day, 200., 120.);
        store.add_activity("a", t0, 340., 300.);
        store.add_activity("b", t0, 0., 30.);
        let report = store.activity(t0, 3600, 300);
        let n = report["block"].as_array().unwrap().len();
        assert_eq!(n, 12);
        let total_now: f64 = report["block"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .sum();
        assert_eq!(total_now, 330.);
        assert_eq!(
            report["sources"]["a"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_f64().unwrap())
                .sum::<f64>(),
            300.
        );
        let baseline: Vec<f64> = report["baseline"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect();
        assert_eq!(baseline.len(), n, "numeric array, one per bucket");
        assert_eq!(
            baseline.iter().filter(|v| **v > 0.).collect::<Vec<_>>(),
            vec![&120.]
        );
        // A week-earlier observation takes precedence over a day-earlier one.
        store.add_activity("a", t0 - 7. * day, 10., 50.);
        let report = store.activity(t0, 3600, 300);
        let baseline: Vec<f64> = report["baseline"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect();
        assert_eq!(baseline.iter().cloned().fold(0., f64::max), 50.);
        // No history at all: null.
        assert!(Store::new(None, 1).activity(t0, 3600, 300)["baseline"].is_null());
    }
    #[test]
    fn activity_query_parameters_cannot_panic_or_stall() {
        let mut store = Store::new(None, 1);
        store.add_activity("a", 1_800_000_000., 10., 5.);
        for (window, bucket) in [
            (0, 0),
            (1, 1),
            (86400, 2_000_000),
            (u64::MAX, u64::MAX),
            (u64::MAX, 300),
            (3600, 0),
        ] {
            let report = store.activity(1_800_000_000., window, bucket);
            assert!(
                report["block"].as_array().unwrap().len()
                    <= (ACTIVITY_KEEP_S / BUCKET_S) as usize,
                "{window} {bucket}"
            );
        }
    }
    #[test]
    fn activity_with_a_realistic_history_answers_quickly() {
        let mut store = Store::new(None, 1);
        let t0 = 1_800_000_000.0f64;
        for channel in 0..60 {
            for bucket in 0..(14 * 288) {
                store.add_activity(&format!("c{channel}"), t0 - bucket as f64 * 300., 10., 5.);
            }
        }
        let started = std::time::Instant::now();
        let report = store.activity(t0, 86400, 300);
        assert_eq!(report["block"].as_array().unwrap().len(), 288);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }
    #[test]
    fn state_round_trips_activity_per_day_user_edits_and_catalog() {
        let dir = temp("state");
        let mut store = Store::new(Some(dir.clone()), 1);
        store
            .user
            .insert("454018750-2".into(), json!({"name": "Dispatch", "followed": true}));
        store.user_dirty = true;
        store
            .known
            .insert("454018750-DMR-2".into(), json!({"frequency_hz": 454018750.0}));
        store.known_dirty = true;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        store.add_activity("454018750-DMR-2", now, 10., 5.);
        store.save();
        store.sync();
        let again = Store::new(Some(dir.clone()), 1);
        assert_eq!(again.user["454018750-2"]["name"], "Dispatch");
        assert_eq!(again.known["454018750-DMR-2"]["frequency_hz"], 454018750.0);
        assert_eq!(
            again.activity(now, 3600, 300)["block"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_f64().unwrap())
                .sum::<f64>(),
            5.
        );
        let _ = fs::remove_dir_all(&dir);
    }
    #[test]
    fn a_corrupt_state_file_is_set_aside_not_overwritten_silently() {
        let dir = temp("corrupt");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("catalog-user.json"), b"{ not json").unwrap();
        let store = Store::new(Some(dir.clone()), 1);
        assert!(store.user.is_empty());
        assert!(dir.join("catalog-user.corrupt").exists());
        let _ = fs::remove_dir_all(&dir);
    }
    #[test]
    fn a_failed_write_is_retried_and_counted() {
        let dir = temp("fail");
        let mut store = Store::new(Some(dir.clone()), 1);
        store.user.insert("x".into(), json!({"name": "n"}));
        store.user_dirty = true;
        // Make the target unwritable by removing the directory.
        fs::remove_dir_all(&dir).unwrap();
        store.save();
        store.sync();
        assert!(store.io_errors() >= 1);
        fs::create_dir_all(&dir).unwrap();
        store.save(); // the failed kind is re-dirtied and written now
        store.save();
        store.sync();
        assert!(
            dir.join("catalog-user.json").exists(),
            "retried after the failure"
        );
        let _ = fs::remove_dir_all(&dir);
    }
    #[test]
    fn probe_dir_rejects_an_unusable_directory() {
        assert!(probe_dir(Path::new("/proc/nope/state")).is_err());
        let dir = temp("probe");
        assert!(probe_dir(&dir).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }
}
