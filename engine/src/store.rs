//! Recent calls (with audio), activity history and the persisted, user-labeled catalog.
//!
//! Everything here lives behind the engine lock and is bounded: calls by count, age and
//! audio memory; activity by age. With a state directory the call audio (WAV files), the
//! call index, the activity buckets and the catalog survive restarts.
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

pub const AUDIO_RATE: u32 = 8000;
/// A call ends after this long without audio.
pub const CALL_HANG_S: f64 = 2.0;
/// Shorter bursts are discarded (false confirmations, key-up blips).
pub const MIN_CALL_S: f64 = 0.8;
/// A single call is split after this long.
pub const MAX_CALL_S: f64 = 600.;
pub const MAX_CALLS: usize = 500;
pub const MAX_CALL_AGE_S: f64 = 7. * 86400.;
/// Activity buckets are 5 minutes wide and kept for 14 days.
pub const BUCKET_S: u64 = 300;
pub const ACTIVITY_KEEP_S: u64 = 14 * 86400;

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
fn read_wav(path: &Path) -> Option<Vec<i16>> {
    let bytes = fs::read(path).ok()?;
    (bytes.len() > 44 && &bytes[..4] == b"RIFF").then(|| {
        bytes[44..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect()
    })
}
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

struct OpenCall {
    channel_id: String,
    protocol: String,
    slot: u64,
    frequency_hz: f64,
    label: Value,
    started: f64,
    last_audio: f64,
    samples: Vec<i16>,
    level_sum: f64,
    level_n: u32,
}
pub struct Call {
    pub meta: Value,
    audio: Option<Arc<Vec<i16>>>,
}
pub struct Store {
    dir: Option<PathBuf>,
    calls: VecDeque<Call>,
    open: HashMap<String, OpenCall>,
    next_id: u64,
    memory_budget: usize,
    memory_used: usize,
    /// (channel id, bucket start) -> (rf seconds, talk seconds)
    activity: BTreeMap<(String, u64), (f32, f32)>,
    activity_dirty: bool,
    /// User edits keyed by channel id: name, followed, muted, notes.
    pub user: BTreeMap<String, Value>,
    pub user_dirty: bool,
    /// Last known catalog entries, for restart persistence.
    pub known: BTreeMap<String, Value>,
    pub known_dirty: bool,
}
/// Events produced by the store for the engine's event stream.
pub struct CallEvent {
    pub phase: &'static str,
    pub call: Value,
}
impl Store {
    pub fn new(dir: Option<PathBuf>, memory_mb: usize) -> Self {
        let mut store = Self {
            dir,
            calls: VecDeque::new(),
            open: HashMap::new(),
            next_id: 1,
            memory_budget: memory_mb * 1024 * 1024,
            memory_used: 0,
            activity: BTreeMap::new(),
            activity_dirty: false,
            user: BTreeMap::new(),
            user_dirty: false,
            known: BTreeMap::new(),
            known_dirty: false,
        };
        store.load();
        store
    }
    fn path(&self, name: &str) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(name))
    }
    fn load(&mut self) {
        let Some(dir) = self.dir.clone() else { return };
        let _ = fs::create_dir_all(dir.join("calls"));
        let read = |name: &str| -> Option<Value> {
            serde_json::from_slice(&fs::read(dir.join(name)).ok()?).ok()
        };
        if let Some(Value::Array(calls)) = read("calls.json") {
            for meta in calls {
                if let Some(id) = meta["id"].as_u64() {
                    self.next_id = self.next_id.max(id + 1);
                    self.calls.push_back(Call { meta, audio: None });
                }
            }
        }
        if let Some(Value::Object(activity)) = read("activity.json") {
            for (key, v) in activity {
                if let Some((id, start)) = key.rsplit_once('@')
                    && let (Ok(start), Some(rf), Some(talk)) =
                        (start.parse::<u64>(), v[0].as_f64(), v[1].as_f64())
                {
                    self.activity.insert((id.to_string(), start), (rf as f32, talk as f32));
                }
            }
        }
        if let Some(Value::Object(user)) = read("catalog-user.json") {
            self.user = user.into_iter().collect();
        }
        if let Some(Value::Object(known)) = read("catalog.json") {
            self.known = known.into_iter().collect();
        }
    }
    /// Flush whatever changed to disk (call from the janitor and at shutdown).
    pub fn save(&mut self) {
        if self.dir.is_none() {
            return;
        }
        let json = |v: &Value| serde_json::to_vec(v).unwrap_or_default();
        if self.activity_dirty {
            let map: serde_json::Map<String, Value> = self
                .activity
                .iter()
                .map(|((id, start), (rf, talk))| (format!("{id}@{start}"), json!([rf, talk])))
                .collect();
            if let Some(p) = self.path("activity.json") {
                let _ = write_atomic(&p, &json(&Value::Object(map)));
            }
            self.activity_dirty = false;
        }
        if self.user_dirty {
            let map: serde_json::Map<String, Value> = self.user.clone().into_iter().collect();
            if let Some(p) = self.path("catalog-user.json") {
                let _ = write_atomic(&p, &json(&Value::Object(map)));
            }
            self.user_dirty = false;
        }
        if self.known_dirty {
            let map: serde_json::Map<String, Value> = self.known.clone().into_iter().collect();
            if let Some(p) = self.path("catalog.json") {
                let _ = write_atomic(&p, &json(&Value::Object(map)));
            }
            self.known_dirty = false;
        }
    }
    fn save_calls(&self) {
        if let Some(p) = self.path("calls.json") {
            let list: Vec<&Value> = self.calls.iter().map(|c| &c.meta).collect();
            let _ = write_atomic(&p, &serde_json::to_vec(&list).unwrap_or_default());
        }
    }
    /// Append audio heard on a channel; returns a "start" event when this opens a call.
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &mut self,
        channel_id: &str,
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
            events.extend(self.close(channel_id, at));
        }
        let open = self.open.entry(channel_id.to_string()).or_insert_with(|| OpenCall {
            channel_id: channel_id.to_string(),
            protocol: protocol.to_string(),
            slot,
            frequency_hz,
            label: label.clone(),
            started: at - audio.len() as f64 / AUDIO_RATE as f64,
            last_audio: at,
            samples: Vec::new(),
            level_sum: 0.,
            level_n: 0,
        });
        if open.samples.is_empty() {
            events.push(CallEvent {
                phase: "start",
                call: json!({
                    "id": null, "source": channel_id, "protocol": protocol, "slot": slot,
                    "frequency_hz": frequency_hz, "label": label, "started_at": open.started,
                    "in_progress": true,
                }),
            });
        }
        open.samples.extend_from_slice(audio);
        open.last_audio = at;
        if let Some(l) = level_db {
            open.level_sum += l;
            open.level_n += 1;
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
        idle.iter().flat_map(|(_, id)| self.close(id, at)).collect()
    }
    fn close(&mut self, channel_id: &str, _at: f64) -> Option<CallEvent> {
        let open = self.open.remove(channel_id)?;
        let duration = open.samples.len() as f64 / AUDIO_RATE as f64;
        if duration < MIN_CALL_S {
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;
        let level = (open.level_n > 0).then(|| open.level_sum / open.level_n as f64);
        let meta = json!({
            "id": id,
            "source": open.channel_id,
            "protocol": open.protocol,
            "slot": open.slot,
            "frequency_hz": open.frequency_hz,
            "label": open.label,
            "started_at": open.started,
            "duration_s": duration,
            "level_db": level,
            "in_progress": false,
            "audio_url": format!("/api/calls/{id}/audio.wav"),
            "audio_format": "audio/wav; 8 kHz mono signed16",
            "talker": null,
            "transcript": null,
            "encrypted": false,
        });
        if let Some(dir) = &self.dir {
            let _ = write_atomic(&dir.join("calls").join(format!("{id}.wav")), &wav_bytes(&open.samples));
        }
        self.memory_used += open.samples.len() * 2;
        let call = Call {
            meta: meta.clone(),
            audio: Some(Arc::new(open.samples)),
        };
        self.calls.push_back(call);
        self.evict(open.last_audio);
        self.save_calls();
        Some(CallEvent { phase: "end", call: meta })
    }
    fn evict(&mut self, now: f64) {
        // Memory: drop the in-memory copy of the oldest audio first (disk copy remains).
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
            || self
                .calls
                .front()
                .is_some_and(|c| now - c.meta["started_at"].as_f64().unwrap_or(now) > MAX_CALL_AGE_S)
        {
            if let Some(old) = self.calls.pop_front() {
                if let Some(audio) = &old.audio {
                    self.memory_used = self.memory_used.saturating_sub(audio.len() * 2);
                }
                if let (Some(dir), Some(id)) = (&self.dir, old.meta["id"].as_u64()) {
                    let _ = fs::remove_file(dir.join("calls").join(format!("{id}.wav")));
                }
            }
        }
    }
    /// Newest first. `before` is an exclusive upper bound on the call id.
    pub fn list(&self, before: Option<u64>, limit: usize, source: Option<&str>) -> (Vec<Value>, Option<u64>) {
        let mut out: Vec<Value> = Vec::new();
        let mut next = None;
        for call in self.calls.iter().rev() {
            let id = call.meta["id"].as_u64().unwrap_or(0);
            if before.is_some_and(|b| id >= b) || source.is_some_and(|s| call.meta["source"] != s) {
                continue;
            }
            if out.len() == limit {
                next = out.last().and_then(|c| c["id"].as_u64());
                break;
            }
            out.push(call.meta.clone());
        }
        (out, next)
    }
    pub fn audio(&self, id: u64) -> Option<Arc<Vec<i16>>> {
        let call = self.calls.iter().find(|c| c.meta["id"].as_u64() == Some(id))?;
        if let Some(a) = &call.audio {
            return Some(a.clone());
        }
        let dir = self.dir.as_ref()?;
        read_wav(&dir.join("calls").join(format!("{id}.wav"))).map(Arc::new)
    }
    pub fn open_calls(&self) -> usize {
        self.open.len()
    }

    /// Add `seconds` of RF/talk time for a channel at unix time `at`.
    pub fn add_activity(&mut self, channel_id: &str, at: f64, rf_s: f32, talk_s: f32) {
        if rf_s <= 0. && talk_s <= 0. {
            return;
        }
        let start = (at as u64) / BUCKET_S * BUCKET_S;
        let slot = self.activity.entry((channel_id.to_string(), start)).or_insert((0., 0.));
        slot.0 += rf_s;
        slot.1 += talk_s;
        self.activity_dirty = true;
        if self.activity.len() > 400_000 {
            let cutoff = (at as u64).saturating_sub(ACTIVITY_KEEP_S);
            self.activity.retain(|(_, s), _| *s >= cutoff);
        }
    }
    /// Talk-seconds per bucket over a window ending at `now`, for the whole block and per
    /// source, plus a baseline from earlier weeks (same weekday and hour) or days (same hour).
    pub fn activity(&self, now: f64, window_s: u64, bucket_s: u64) -> Value {
        let bucket_s = bucket_s.max(BUCKET_S) / BUCKET_S * BUCKET_S;
        let end = (now as u64) / bucket_s * bucket_s + bucket_s;
        let window_s = window_s.clamp(bucket_s, ACTIVITY_KEEP_S) / bucket_s * bucket_s;
        let start = end - window_s;
        let n = (window_s / bucket_s) as usize;
        let mut total = vec![0f64; n];
        let mut rf_total = vec![0f64; n];
        let mut sources: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for ((id, bucket), (rf, talk)) in &self.activity {
            if *bucket >= start && *bucket < end {
                let i = ((*bucket - start) / bucket_s) as usize;
                total[i] += *talk as f64;
                rf_total[i] += *rf as f64;
                sources.entry(id.clone()).or_insert_with(|| vec![0.; n])[i] += *talk as f64;
            }
        }
        let sum_range = |from: u64, to: u64| -> Option<f64> {
            let mut any = false;
            let mut sum = 0.;
            for ((_, b), (_, talk)) in &self.activity {
                if *b >= from && *b < to {
                    sum += *talk as f64;
                    any = true;
                }
            }
            any.then_some(sum)
        };
        let baseline: Vec<Option<f64>> = (0..n)
            .map(|i| {
                let from = start + i as u64 * bucket_s;
                let to = from + bucket_s;
                let weeks: Vec<f64> = (1..=2u64)
                    .filter_map(|k| sum_range(from.checked_sub(k * 7 * 86400)?, to - k * 7 * 86400))
                    .collect();
                let days: Vec<f64> = (1..=6u64)
                    .filter_map(|k| sum_range(from.checked_sub(k * 86400)?, to - k * 86400))
                    .collect();
                let pool = if weeks.is_empty() { days } else { weeks };
                (!pool.is_empty()).then(|| pool.iter().sum::<f64>() / pool.len() as f64)
            })
            .collect();
        json!({
            "start": start, "end": end, "window_s": window_s, "bucket_s": bucket_s,
            "total": total, "rf_total": rf_total, "sources": sources, "baseline": baseline,
            "baseline_basis": "mean talk-seconds in the same bucket 1-2 weeks earlier (same weekday), else 1-6 days earlier (same hour); null without history",
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
        store.record(id, "DMR", 2, 454_018_750., &Value::Null, Some(-30.), &audio(seconds), at)
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
    fn a_call_spans_chunks_ends_after_the_hang_and_is_listed_newest_first() {
        let mut store = Store::new(None, 8);
        let started = record(&mut store, "a", 100.0, 0.5);
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].phase, "start");
        assert!(record(&mut store, "a", 100.6, 0.5).is_empty(), "same call continues");
        assert!(store.close_idle(101.0).is_empty(), "not idle yet");
        let ended = store.close_idle(103.0);
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].phase, "end");
        assert!((ended[0].call["duration_s"].as_f64().unwrap() - 1.0).abs() < 1e-6);
        record(&mut store, "b", 200.0, 1.5);
        store.close_idle(210.0);
        let (calls, next) = store.list(None, 10, None);
        assert_eq!(calls.iter().map(|c| c["source"].as_str().unwrap()).collect::<Vec<_>>(), ["b", "a"]);
        assert!(next.is_none());
        let (one, next) = store.list(None, 1, None);
        assert_eq!(one.len(), 1);
        assert_eq!(next, one[0]["id"].as_u64());
        let (older, _) = store.list(next, 10, None);
        assert_eq!(older[0]["source"], "a");
        assert_eq!(store.list(None, 10, Some("a")).0.len(), 1);
        assert_eq!(store.audio(1).unwrap().len(), 8000);
    }
    #[test]
    fn short_blips_are_discarded_and_gaps_split_calls() {
        let mut store = Store::new(None, 8);
        record(&mut store, "a", 10.0, 0.3);
        assert!(store.close_idle(20.0).is_empty(), "0.3 s is not a call");
        record(&mut store, "a", 30.0, 1.0);
        let split = record(&mut store, "a", 40.0, 1.0); // after the hang: previous call ends, new starts
        assert_eq!(split.iter().map(|e| e.phase).collect::<Vec<_>>(), ["end", "start"]);
    }
    #[test]
    fn memory_budget_drops_old_audio_but_keeps_the_index_with_a_disk_copy() {
        let dir = std::env::temp_dir().join(format!("store-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut store = Store::new(Some(dir.clone()), 1);
        for i in 0..5 {
            record(&mut store, "a", i as f64 * 100., 40.0); // 640 kB each
            store.close_idle(i as f64 * 100. + 50.);
        }
        assert!(store.memory_used <= 1024 * 1024 + 700_000);
        // The oldest call's audio was evicted from memory yet is still served from its WAV.
        assert_eq!(store.audio(1).unwrap().len(), 40 * 8000);
        // A new store on the same directory sees the index again.
        let reopened = Store::new(Some(dir.clone()), 1);
        assert_eq!(reopened.list(None, 10, None).0.len(), 5);
        assert_eq!(reopened.audio(5).unwrap().len(), 40 * 8000);
        let _ = fs::remove_dir_all(&dir);
    }
    #[test]
    fn activity_buckets_aggregate_and_baseline_uses_the_same_hour_earlier() {
        let mut store = Store::new(None, 1);
        let day = 86400.;
        let t0 = 1_800_000_000.0f64; // some instant
        // Yesterday this hour: 120 s talk; today: 300 s talk, 40 s rf-only.
        store.add_activity("a", t0 - day, 200., 120.);
        store.add_activity("a", t0, 340., 300.);
        store.add_activity("b", t0, 0., 30.);
        let report = store.activity(t0, 3600, 300);
        let n = report["total"].as_array().unwrap().len();
        assert_eq!(n, 12);
        let total_now: f64 = report["total"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).sum();
        assert_eq!(total_now, 330.);
        assert_eq!(report["sources"]["a"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).sum::<f64>(), 300.);
        let baselines: Vec<f64> = report["baseline"].as_array().unwrap().iter().filter_map(|v| v.as_f64()).collect();
        assert_eq!(baselines, vec![120.]);
    }
    #[test]
    fn user_edits_and_catalog_persist() {
        let dir = std::env::temp_dir().join(format!("store-user-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut store = Store::new(Some(dir.clone()), 1);
        store.user.insert("454018750-DMR-2".into(), json!({"name": "Dispatch", "followed": true}));
        store.user_dirty = true;
        store.known.insert("454018750-DMR-2".into(), json!({"frequency_hz": 454018750.0}));
        store.known_dirty = true;
        store.add_activity("454018750-DMR-2", 1_800_000_000., 10., 5.);
        store.save();
        let again = Store::new(Some(dir.clone()), 1);
        assert_eq!(again.user["454018750-DMR-2"]["name"], "Dispatch");
        assert_eq!(again.known["454018750-DMR-2"]["frequency_hz"], 454018750.0);
        assert_eq!(again.activity(1_800_000_000., 3600, 300)["total"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).sum::<f64>(), 5.);
        let _ = fs::remove_dir_all(&dir);
    }
}
