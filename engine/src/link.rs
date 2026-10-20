//! Keeps the engine attached to a `tcp:` RIQ1 source for as long as it runs.
//!
//! The on-device producer serves exactly one client and exits (after restoring the radio)
//! when that client goes away, so a lost link is normal: the supervisor starts a new
//! producer and this loop reconnects with backoff. The loop also treats a connection that
//! goes silent as lost, so a wedged producer or a pulled cable does not hold the engine in
//! a dead "connected" state.
//!
//! Optionally the loop only attempts a connection while the supervisor's status file says a
//! producer is running. Without that gate, a retrying engine could take the single accept
//! slot of a test producer that someone else started while borrowing the radio.
//!
//! `/api/status` reports the link (`link`) and the supervisor's own words (`receiver`) so the
//! UI can say plainly what is going on.
use crate::runtime::{App, attach_control, ingest};
use serde_json::{Value, json};
use std::{
    net::{TcpStream, ToSocketAddrs},
    path::Path,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(crate) fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.)
}

/// The supervisor rewrites its status file at least every 10 s; older than this means the
/// supervisor is gone and nothing it said can be relied on.
pub const RECEIVER_FRESH_S: f64 = 45.;

/// Where the engine stands with its source, as reported in `/api/status` under `link`.
#[derive(Clone, Debug)]
pub(crate) struct Link {
    /// `fixture` (no source configured: a local feeder may attach) or `tcp`.
    mode: &'static str,
    address: Option<String>,
    /// `fixture`, `connecting`, `connected`, `reconnecting` or `waiting`.
    state: &'static str,
    state_since: f64,
    /// When the engine last stopped receiving; `None` while connected.
    offline_since: Option<f64>,
    /// Failed or short-lived attempts in a row.
    failures: u32,
    connects: u64,
    /// The most recent reason the link was lost or could not be made; kept after a reconnect.
    last_error: Option<String>,
    next_retry_at: Option<f64>,
}

impl Link {
    pub(crate) fn fixture() -> Self {
        Self {
            mode: "fixture",
            address: None,
            state: "fixture",
            state_since: now(),
            offline_since: None,
            failures: 0,
            connects: 0,
            last_error: None,
            next_retry_at: None,
        }
    }
    fn tcp(address: &str) -> Self {
        let at = now();
        Self {
            mode: "tcp",
            address: Some(address.to_string()),
            state: "connecting",
            state_since: at,
            offline_since: Some(at),
            failures: 0,
            connects: 0,
            last_error: None,
            next_retry_at: None,
        }
    }
    fn set(&mut self, state: &'static str) {
        if self.state != state {
            self.state = state;
            self.state_since = now();
        }
    }
    pub(crate) fn to_json(&self, at: f64) -> Value {
        json!({
            "mode": self.mode,
            "address": self.address,
            "state": self.state,
            "state_since": self.state_since,
            "offline_since": self.offline_since,
            "offline_s": self.offline_since.map(|t| (at - t).max(0.)),
            "failures": self.failures,
            "connects": self.connects,
            "last_error": self.last_error,
            "next_retry_s": self.next_retry_at.map(|t| (t - at).max(0.)),
        })
    }
}

/// How to retry. The defaults suit a producer that takes several seconds to restart.
#[derive(Clone, Debug)]
pub struct LinkConfig {
    pub address: String,
    pub connect_timeout: Duration,
    /// No frame for this long on an open connection counts as a lost source.
    pub stall_timeout: Duration,
    pub backoff_base: Duration,
    pub backoff_cap: Duration,
    /// A connection that lasts at least this long resets the backoff.
    pub stable_after: Duration,
}

impl LinkConfig {
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            connect_timeout: Duration::from_secs(5),
            stall_timeout: Duration::from_secs(20),
            backoff_base: Duration::from_secs(1),
            backoff_cap: Duration::from_secs(15),
            stable_after: Duration::from_secs(5),
        }
    }
}

/// Delay before attempt number `failures` (1 = the first retry): base, 2x, 4x ... up to cap.
pub fn backoff(failures: u32, base: Duration, cap: Duration) -> Duration {
    let doubled = base.saturating_mul(1u32 << failures.saturating_sub(1).min(16));
    doubled.min(cap)
}

/// +-20% so a fleet of retriers does not move in lockstep. `unit` is in [0, 1).
fn jitter(delay: Duration, unit: f64) -> Duration {
    delay.mul_f64(0.8 + 0.4 * unit.clamp(0., 1.))
}

fn unit_random() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    f64::from(nanos.wrapping_mul(2_654_435_761) >> 8) / f64::from(1u32 << 24)
}

/// The supervisor's status file with its age added, or `None` when there is no readable one.
pub(crate) fn read_receiver(path: &Path, at: f64) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut value: Value = serde_json::from_str(&text).ok()?;
    let age = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map(|m| (at - m.as_secs_f64()).max(0.))?;
    let fields = value.as_object_mut()?;
    fields.insert("age_s".into(), json!(age));
    fields.insert("fresh".into(), json!(age <= RECEIVER_FRESH_S));
    Some(value)
}

/// A producer is expected to be waiting for us only while a fresh status says `running`.
fn receiver_ready(status: &Option<Value>) -> bool {
    status
        .as_ref()
        .is_some_and(|s| s["fresh"] == true && s["state"] == "running")
}

impl App {
    /// The supervisor's status (see `read_receiver`), when a status file is configured.
    pub(crate) fn receiver_status(&self) -> Option<Value> {
        read_receiver(self.receiver_file.as_deref()?, now())
    }
    fn link_update(&self, f: impl FnOnce(&mut Link)) {
        f(&mut self.engine().link);
    }
}

/// Connect, ingest until the source goes away, back off, repeat. Never returns.
///
/// `gate` is the supervisor status file; when `Some`, a connection is attempted only while it
/// says a producer is running.
pub fn run_source(app: App, config: LinkConfig, gate: bool) -> ! {
    app.link_update(|l| *l = Link::tcp(&config.address));
    let mut failures = 0u32;
    loop {
        if gate {
            let status = app.receiver_status();
            if !receiver_ready(&status) {
                app.link_update(|l| {
                    l.set("waiting");
                    l.next_retry_at = None;
                });
                thread::sleep(Duration::from_millis(500));
                continue;
            }
        }
        app.link_update(|l| {
            l.set(if l.connects == 0 { "connecting" } else { "reconnecting" });
            l.next_retry_at = None;
        });
        let started = Instant::now();
        match attach(&app, &config) {
            Ok(stream) => {
                app.link_update(|l| {
                    l.set("connected");
                    l.connects += 1;
                    l.offline_since = None;
                });
                let lost = match ingest(stream, app.clone()) {
                    Ok(()) => "the receiver closed the connection".to_string(),
                    Err(e) => {
                        let message = describe(&e);
                        app.error(message.clone());
                        message
                    }
                };
                if started.elapsed() >= config.stable_after {
                    failures = 0;
                }
                failures += 1;
                app.link_update(|l| {
                    l.set("reconnecting");
                    l.offline_since = Some(now());
                    l.last_error = Some(lost);
                });
            }
            Err(e) => {
                failures += 1;
                let message = format!("cannot reach the receiver at {}: {e}", config.address);
                app.link_update(|l| {
                    l.last_error = Some(message);
                });
            }
        }
        let delay = jitter(
            backoff(failures, config.backoff_base, config.backoff_cap),
            unit_random(),
        );
        app.link_update(|l| {
            l.failures = failures;
            l.next_retry_at = Some(now() + delay.as_secs_f64());
        });
        thread::sleep(delay);
    }
}

fn attach(app: &App, config: &LinkConfig) -> std::io::Result<TcpStream> {
    let mut last = None;
    for address in config.address.to_socket_addrs()? {
        match TcpStream::connect_timeout(&address, config.connect_timeout) {
            Ok(stream) => {
                stream.set_nodelay(true)?;
                stream.set_read_timeout(Some(config.stall_timeout))?;
                stream.set_write_timeout(Some(Duration::from_secs(5)))?;
                attach_control(&stream, app).map_err(std::io::Error::other)?;
                return Ok(stream);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("address did not resolve")))
}

fn describe(error: &anyhow::Error) -> String {
    match error.downcast_ref::<std::io::Error>().map(|e| e.kind()) {
        Some(std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
            "the receiver stopped sending data".to_string()
        }
        _ => format!("RIQ source failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Frame;
    use std::{
        io::Write,
        net::TcpListener,
        sync::mpsc,
    };

    fn config_frame() -> Vec<u8> {
        let payload = json!({"input_center_hz":454216998,"input_rate_hz":800000,"input_bandwidth_hz":800000,
            "receiver_state":"streaming","streams":[{"stream_id":1,"center_hz":454029223u64,"protocol":"Unknown"}],
            "psd":{"unit":"dBFS"}});
        let mut bytes = Vec::new();
        Frame {
            kind: 4,
            flags: 0,
            generation: 7,
            stream_id: 0,
            sequence: 0,
            first_sample: 0,
            rate: 0,
            center_hz: 454216998,
            format: 3,
            valid_bits: 16,
            payload: payload.to_string().into_bytes(),
        }
        .write(&mut bytes)
        .unwrap();
        bytes
    }
    fn fast(address: String) -> LinkConfig {
        LinkConfig {
            address,
            connect_timeout: Duration::from_millis(500),
            stall_timeout: Duration::from_millis(400),
            backoff_base: Duration::from_millis(50),
            backoff_cap: Duration::from_millis(200),
            stable_after: Duration::from_millis(300),
        }
    }
    fn link_state(app: &App) -> Value {
        app.engine().link.to_json(now())
    }
    fn wait_for(what: &str, app: &App, ok: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let link = link_state(app);
            if ok(&link) {
                return link;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}: {link}");
            thread::sleep(Duration::from_millis(20));
        }
    }
    fn start(app: &App, config: LinkConfig, gate: bool) {
        let app = app.clone();
        thread::spawn(move || run_source(app, config, gate));
    }

    #[test]
    fn backoff_doubles_then_holds_at_the_cap() {
        let (base, cap) = (Duration::from_secs(1), Duration::from_secs(15));
        let seq: Vec<u64> = (1..=7).map(|n| backoff(n, base, cap).as_secs()).collect();
        assert_eq!(seq, [1, 2, 4, 8, 15, 15, 15]);
        assert_eq!(backoff(0, base, cap), base);
        assert_eq!(backoff(u32::MAX, base, cap), cap, "no overflow at absurd counts");
        assert!(jitter(Duration::from_secs(10), 0.) >= Duration::from_secs(8));
        assert!(jitter(Duration::from_secs(10), 0.999) <= Duration::from_secs(12));
    }

    #[test]
    fn a_dropped_source_is_reconnected_and_the_outage_is_reported() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (release, held) = mpsc::channel::<()>();
        thread::spawn(move || {
            // First producer: sends CONFIG then dies. Second: sends CONFIG and stays.
            let (mut first, _) = listener.accept().unwrap();
            first.write_all(&config_frame()).unwrap();
            thread::sleep(Duration::from_millis(100));
            drop(first);
            let (mut second, _) = listener.accept().unwrap();
            second.write_all(&config_frame()).unwrap();
            let _ = held.recv();
        });
        let app = App::new(3);
        start(&app, fast(address.clone()), false);
        wait_for("the first connection", &app, |l| l["state"] == "connected" && l["connects"] == 1);
        // The source drops: the outage is visible, then the engine is back by itself.
        let lost = wait_for("a reconnect", &app, |l| l["connects"] == 2 && l["state"] == "connected");
        assert!(lost["offline_since"].is_null(), "no outage once reconnected: {lost}");
        let e = app.engine();
        assert!(e.active, "second CONFIG must reactivate the source");
        assert_eq!(e.source["input_rate_hz"], 800000);
        drop(release);
    }

    #[test]
    fn outage_state_is_visible_while_the_source_is_down() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        drop(listener); // nothing listens: connection refused
        let app = App::new(3);
        start(&app, fast(address.clone()), false);
        let link = wait_for("repeated failures", &app, |l| l["failures"].as_u64().unwrap_or(0) >= 3);
        assert_eq!(link["mode"], "tcp");
        assert_eq!(link["address"], json!(address));
        assert_eq!(link["connects"], 0);
        assert!(link["offline_s"].as_f64().unwrap() >= 0.);
        assert!(link["last_error"].as_str().unwrap().contains("cannot reach the receiver"));
        assert!(link["state"] == "connecting" || link["state"] == "reconnecting", "{link}");
        // Backoff stays at or under the cap (plus jitter).
        assert!(link["next_retry_s"].as_f64().unwrap() <= 0.25, "{link}");
        // It is the same value /api/status carries.
        let status = crate::api::status_json(&app);
        assert_eq!(status["link"]["mode"], "tcp");
        assert!(status["link"]["offline_s"].is_number());
    }

    #[test]
    fn a_silent_connection_is_dropped_as_lost() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (accepted, seen) = mpsc::channel();
        thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming().flatten() {
                held.push(stream); // accepts, never sends
                let _ = accepted.send(held.len());
            }
        });
        let app = App::new(3);
        start(&app, fast(address), false);
        assert_eq!(seen.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
        // Stall timeout is 400 ms: the engine abandons the dead connection and dials again.
        assert_eq!(seen.recv_timeout(Duration::from_secs(5)).unwrap(), 2);
        let link = link_state(&app);
        assert!(
            link["last_error"].as_str().unwrap().contains("stopped sending"),
            "{link}"
        );
        assert!(app.engine().error.is_some());
    }

    #[test]
    fn a_gated_engine_stays_away_until_the_supervisor_says_a_producer_is_running() {
        let dir = std::env::temp_dir().join(format!("link-gate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("receiver.json");
        let write = |state: &str| {
            std::fs::write(&file, json!({"state": state, "detail": "test"}).to_string()).unwrap();
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let app = App::configured(crate::runtime::AppConfig {
            receiver_status: Some(file.clone()),
            ..crate::runtime::AppConfig::new(3)
        });
        write("borrowed");
        start(&app, fast(address), true);
        let waiting = wait_for("waiting state", &app, |l| l["state"] == "waiting");
        thread::sleep(Duration::from_millis(700));
        assert!(
            matches!(listener.accept(), Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "engine dialed a producer that the supervisor did not announce: {waiting}"
        );
        assert_eq!(app.receiver_status().unwrap()["state"], "borrowed");
        write("running");
        let deadline = Instant::now() + Duration::from_secs(5);
        let _stream = loop {
            match listener.accept() {
                Ok((s, _)) => break s,
                Err(_) => {
                    assert!(Instant::now() < deadline, "never connected after running");
                    thread::sleep(Duration::from_millis(20));
                }
            }
        };
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_stale_status_file_is_not_trusted() {
        let dir = std::env::temp_dir().join(format!("link-stale-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("receiver.json");
        std::fs::write(&file, r#"{"state":"running"}"#).unwrap();
        let fresh = read_receiver(&file, now()).unwrap();
        assert!(receiver_ready(&Some(fresh)));
        let later = now() + RECEIVER_FRESH_S + 5.;
        let stale = read_receiver(&file, later).unwrap();
        assert_eq!(stale["fresh"], false);
        assert!(!receiver_ready(&Some(stale)));
        assert!(!receiver_ready(&read_receiver(&dir.join("missing.json"), now())));
        std::fs::write(&file, "not json").unwrap();
        assert!(read_receiver(&file, now()).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
