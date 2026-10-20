//! Keeps the Pluto's channel producer running for the always-on radio, and puts the radio
//! back the way it found it whenever the producer stops. RX only: nothing here transmits,
//! flashes, or touches the NOR or the U-Boot environment.
//!
//! The data path is untouched: the engine reads the producer over TCP. This process only
//! decides when a producer should exist, starts it over SSH, watches it, and restores the
//! radio afterwards. The decisions live here, behind small traits, so the cycle can be tested
//! without a Pluto; `main.rs` supplies the real SSH, libiio and flock implementations.
//!
//! A cycle:
//!  1. take `/tmp/pluto-radio.lock` (shared with every other radio session); wait if it is held
//!  2. check the Pluto is reachable, otherwise release the lock, back off and look again later
//!  3. stop any leftover producer, snapshot the receiver settings, start the producer
//!  4. watch it until it exits, the engine reports no data for too long, or we are told to stop
//!  5. stop it cleanly, read the hardware back and correct it if it differs from the snapshot
//!  6. release the lock, wait out a backoff, start over
//!
//! `status` (a JSON file) tells the engine and the UI what is going on. It is rewritten at
//! least every few seconds; a stale file means this process is gone.
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};

/// Receiver settings as read from the radio, by name (see `main.rs` for the attributes).
pub type Snapshot = BTreeMap<String, String>;

/// What the Pluto answered when asked who it is.
pub enum Probe {
    /// No answer: powered off, rebooting, cable out, link down.
    Unreachable(String),
    /// Answers; the string is a one-line description for the status.
    Ready(String),
}

/// Facts read under the lock before touching anything.
pub struct Inspection {
    /// Changes when the Pluto reboots, so a reboot during a run is recognized afterwards.
    pub boot_id: String,
    pub leftover_producers: u32,
}

/// How the hardware looked after a run.
#[derive(Debug, PartialEq)]
pub enum Restore {
    /// Every restored attribute read back equal to the snapshot.
    Verified,
    /// They differed; the listed attributes were written back and now match.
    Corrected(Vec<String>),
    /// The Pluto rebooted during the run, so it is at firmware defaults and there is nothing
    /// to restore.
    Rebooted,
    /// Could not be shown to match (unreachable, or still different after correcting).
    Unverified(String),
}

impl Restore {
    pub fn describe(&self) -> String {
        match self {
            Restore::Verified => "verified: settings read back equal to before".into(),
            Restore::Corrected(keys) => format!("corrected and verified: {}", keys.join(", ")),
            Restore::Rebooted => "not needed: the Pluto rebooted and is at its defaults".into(),
            Restore::Unverified(why) => format!("UNVERIFIED: {why}"),
        }
    }
}

pub struct ExitInfo {
    pub code: Option<i32>,
    /// The tail of what the producer printed.
    pub output: String,
}

impl ExitInfo {
    fn describe(&self) -> String {
        let code = self.code.map_or("a signal".to_string(), |c| format!("exit code {c}"));
        let last = self.output.lines().rev().find(|l| !l.trim().is_empty());
        match last {
            Some(line) => format!("{code}: {}", line.chars().take(200).collect::<String>()),
            None => code,
        }
    }
}

/// A producer started on the Pluto.
pub trait Running {
    /// `Some` once the producer (or the SSH session that carries it) has ended.
    fn try_wait(&mut self) -> Option<ExitInfo>;
    /// Ask for a clean exit so the producer restores the radio itself; escalate only when it
    /// ignores the request. Returns once the producer is gone.
    fn stop(&mut self) -> ExitInfo;
}

pub trait Radio {
    fn probe(&mut self) -> Probe;
    fn inspect(&mut self) -> Result<Inspection, String>;
    /// Stop producers somebody left behind (we hold the lock, so they are nobody's live run).
    fn stop_leftovers(&mut self) -> Result<(), String>;
    fn snapshot(&mut self) -> Result<Snapshot, String>;
    /// Copy the producer over, start it and return once it is listening.
    fn start(&mut self) -> Result<Box<dyn Running>, String>;
    /// With the producer gone: confirm the radio matches `before`, correcting it if not.
    /// `stopping` means the supervisor itself is shutting down, so do not wait long.
    fn verify_restored(&mut self, before: &Snapshot, boot_id: &str, stopping: bool) -> Restore;
    /// Remove what we put on the Pluto's volatile storage.
    fn cleanup(&mut self);
}

pub trait Lock {
    /// Non-blocking. True when we now hold the radio lock.
    fn try_acquire(&mut self) -> bool;
    fn release(&mut self);
}

pub trait Env {
    /// Unix seconds.
    fn now(&self) -> f64;
    fn sleep(&mut self, duration: Duration);
    /// A stop was requested (SIGTERM/SIGINT).
    fn stopping(&self) -> bool;
    /// Operator intent, retained independently of producer crashes and service restarts.
    fn receiving_enabled(&self) -> bool { true }
    fn publish(&mut self, status: &Value);
    /// Age in seconds of the newest data the engine has received, or `None` when the engine
    /// cannot be asked (not running, not configured): then there is no verdict either way.
    fn samples_age(&mut self) -> Option<f64>;
}

#[derive(Clone, Debug)]
pub struct Policy {
    pub heartbeat: Duration,
    pub poll: Duration,
    pub lock_poll: Duration,
    pub restart_base: Duration,
    pub restart_cap: Duration,
    /// A run at least this long counts as healthy and resets the restart backoff.
    pub stable_run: Duration,
    pub unreachable_base: Duration,
    pub unreachable_cap: Duration,
    /// How long to wait before looking again at a receiver left in a state we will not touch.
    pub radio_state_wait: Duration,
    /// No data verdicts this soon after a start.
    pub startup_grace: Duration,
    /// Data older than this (seconds) with the producer "running" means it is wedged.
    pub wedge_after: f64,
    pub health_every: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            heartbeat: Duration::from_secs(5),
            poll: Duration::from_millis(500),
            lock_poll: Duration::from_secs(2),
            restart_base: Duration::from_secs(3),
            restart_cap: Duration::from_secs(60),
            stable_run: Duration::from_secs(60),
            unreachable_base: Duration::from_secs(5),
            unreachable_cap: Duration::from_secs(30),
            radio_state_wait: Duration::from_secs(120),
            startup_grace: Duration::from_secs(60),
            wedge_after: 60.,
            health_every: Duration::from_secs(5),
        }
    }
}

/// base, 2x, 4x ... up to cap; `n` counts consecutive failures, 1 = the first.
pub fn backoff(n: u32, base: Duration, cap: Duration) -> Duration {
    base.saturating_mul(1u32 << n.saturating_sub(1).min(16)).min(cap)
}

enum Next {
    Stop,
    Retry(Duration),
}

pub struct Supervisor<R: Radio, L: Lock, E: Env> {
    pub radio: R,
    pub lock: L,
    pub env: E,
    pub policy: Policy,
    state: &'static str,
    reason: Option<&'static str>,
    detail: String,
    since: f64,
    last_publish: f64,
    retry_in: Option<f64>,
    pluto: String,
    producer_started_at: Option<f64>,
    starts: u64,
    last_exit: Option<String>,
    last_restore: Option<String>,
    unreachable_n: u32,
    crash_n: u32,
    deployed: bool,
}

impl<R: Radio, L: Lock, E: Env> Supervisor<R, L, E> {
    pub fn new(radio: R, lock: L, env: E, policy: Policy) -> Self {
        let now = env.now();
        Self {
            radio,
            lock,
            env,
            policy,
            state: "starting",
            reason: None,
            detail: "Starting.".into(),
            since: now,
            last_publish: f64::MIN,
            retry_in: None,
            pluto: String::new(),
            producer_started_at: None,
            starts: 0,
            last_exit: None,
            last_restore: None,
            unreachable_n: 0,
            crash_n: 0,
            deployed: false,
        }
    }

    /// Run until asked to stop. On the way out the radio is restored, the lock released and a
    /// final `stopped` status written.
    pub fn run(&mut self) {
        while !self.env.stopping() {
            if !self.env.receiving_enabled() {
                self.set("stopped", Some("operator"), "Receiving is stopped. Start receiving to resume.", None);
                self.env.sleep(self.policy.poll);
                continue;
            }
            if !self.lock.try_acquire() {
                self.set(
                    "waiting",
                    Some("lock-held"),
                    "Another session is using the receiver. Waiting for it to finish.",
                    None,
                );
                let nap = self.policy.lock_poll;
                self.nap(nap);
                continue;
            }
            let next = self.session();
            self.lock.release();
            match next {
                Next::Stop => break,
                Next::Retry(wait) => {
                    self.retry_in = Some(wait.as_secs_f64());
                    self.publish();
                    self.nap(wait);
                }
            }
        }
        if self.deployed {
            // Only under the lock, so a borrower that already took it is left alone.
            if self.lock.try_acquire() {
                self.radio.cleanup();
                self.lock.release();
            }
        }
        self.set("stopped", Some("shutdown"), "The receiver supervisor is stopped.", None);
    }

    fn session(&mut self) -> Next {
        self.set("starting", None, "Checking the receiver and starting its software.", None);
        match self.radio.probe() {
            Probe::Unreachable(why) => {
                self.unreachable_n += 1;
                let wait = backoff(self.unreachable_n, self.policy.unreachable_base, self.policy.unreachable_cap);
                self.set(
                    "waiting",
                    Some("unreachable"),
                    &format!("The receiver box isn't answering ({why}). Looking again in {} s.", wait.as_secs()),
                    Some(wait),
                );
                return Next::Retry(wait);
            }
            Probe::Ready(summary) => {
                // unreachable_n is not reset here: a Pluto that answers iio_info but refuses
                // ssh must still back off. It resets once a producer has started.
                self.pluto = summary;
            }
        }
        let inspection = match self.radio.inspect() {
            Ok(i) => i,
            Err(e) => return self.unreachable_now(&e),
        };
        if inspection.leftover_producers > 0 {
            self.set("starting", None, "A leftover producer was running on the receiver; stopping it first.", None);
            if let Err(e) = self.radio.stop_leftovers() {
                return self.failed(&format!("could not stop a leftover producer: {e}"));
            }
        }
        if self.env.stopping() {
            return Next::Stop;
        }
        if !self.env.receiving_enabled() {
            return Next::Retry(Duration::ZERO);
        }
        let before = match self.radio.snapshot() {
            Ok(s) => s,
            Err(e) => return self.unreachable_now(&e),
        };
        if before.get("rx_filter_fir_en").is_some_and(|v| v != "0")
            || before.get("tx_filter_fir_en").is_some_and(|v| v != "0")
        {
            let wait = self.policy.radio_state_wait;
            self.set(
                "waiting",
                Some("radio-state"),
                "The receiver has a custom filter switched on that this software did not set. Not touching it; looking again later.",
                Some(wait),
            );
            return Next::Retry(wait);
        }
        if self.env.stopping() {
            return Next::Stop;
        }
        if !self.env.receiving_enabled() {
            return Next::Retry(Duration::ZERO);
        }
        self.deployed = true;
        let mut running = match self.radio.start() {
            Ok(r) => r,
            Err(e) => {
                // A failed start may still have opened the radio before dying.
                let stopping = self.env.stopping();
                let restore = self.radio.verify_restored(&before, &inspection.boot_id, stopping);
                self.last_restore = Some(restore.describe());
                if stopping {
                    return Next::Stop;
                }
                return self.failed(&format!("could not start: {e}"));
            }
        };
        self.unreachable_n = 0;
        let started = self.env.now();
        self.starts += 1;
        self.producer_started_at = Some(started);
        self.set("running", None, "Receiver software is running on the Pluto.", None);

        let mut last_health = started;
        let mut stale_polls = 0u32;
        let ended = loop {
            if self.env.stopping() || !self.env.receiving_enabled() {
                // Close the engine's gate before the producer goes away.
                self.set("starting", None, "Stopping and putting the radio back.", None);
                break (running.stop(), "asked to stop");
            }
            if let Some(exit) = running.try_wait() {
                break (exit, "exited");
            }
            let now = self.env.now();
            if now - last_health >= self.policy.health_every.as_secs_f64() {
                last_health = now;
                let past_grace = now - started >= self.policy.startup_grace.as_secs_f64();
                match self.env.samples_age() {
                    Some(age) if past_grace && age > self.policy.wedge_after => stale_polls += 1,
                    _ => stale_polls = 0,
                }
                if stale_polls >= 2 {
                    self.set(
                        "starting",
                        None,
                        "The receiver software is running but no data is arriving. Restarting it.",
                        None,
                    );
                    break (running.stop(), "no data arriving");
                }
            }
            let poll = self.policy.poll;
            self.env.sleep(poll);
            self.beat();
        };
        let (exit, why) = ended;
        let ran = self.env.now() - started;
        self.producer_started_at = None;
        self.last_exit = Some(format!("{why}; {}", exit.describe()));
        let stopping = self.env.stopping();
        self.set(
            "starting",
            None,
            if stopping { "Stopping and putting the radio back." } else { "The receiver software stopped. Putting the radio back." },
            None,
        );
        let restore = self.radio.verify_restored(&before, &inspection.boot_id, stopping);
        self.last_restore = Some(restore.describe());
        if stopping {
            return Next::Stop;
        }
        if !self.env.receiving_enabled() {
            return Next::Retry(Duration::ZERO);
        }
        // A run that never delivered data does not count as healthy, however long it lasted.
        if ran >= self.policy.stable_run.as_secs_f64() && why != "no data arriving" {
            self.crash_n = 0;
        }
        self.failed(&format!("the producer {why} after {ran:.0} s"))
    }

    fn unreachable_now(&mut self, why: &str) -> Next {
        self.unreachable_n += 1;
        let wait = backoff(self.unreachable_n, self.policy.unreachable_base, self.policy.unreachable_cap);
        self.set(
            "waiting",
            Some("unreachable"),
            &format!("Lost touch with the receiver box ({why}). Looking again in {} s.", wait.as_secs()),
            Some(wait),
        );
        Next::Retry(wait)
    }

    fn failed(&mut self, what: &str) -> Next {
        self.crash_n += 1;
        let wait = backoff(self.crash_n, self.policy.restart_base, self.policy.restart_cap);
        self.set(
            "waiting",
            Some("producer-failed"),
            &format!("The receiver software stopped ({what}). Restarting in {} s.", wait.as_secs()),
            Some(wait),
        );
        Next::Retry(wait)
    }

    fn set(&mut self, state: &'static str, reason: Option<&'static str>, detail: &str, retry: Option<Duration>) {
        if self.state != state || self.reason != reason {
            self.since = self.env.now();
        }
        self.state = state;
        self.reason = reason;
        self.detail = detail.to_string();
        self.retry_in = retry.map(|d| d.as_secs_f64());
        self.publish();
    }

    /// Rewrite the status when it is due, so its age tells the engine we are alive.
    fn beat(&mut self) {
        if self.env.now() - self.last_publish >= self.policy.heartbeat.as_secs_f64() {
            self.publish();
        }
    }

    fn nap(&mut self, total: Duration) {
        let mut left = total;
        while !left.is_zero() && !self.env.stopping() && self.env.receiving_enabled() {
            let slice = left.min(Duration::from_millis(200));
            self.env.sleep(slice);
            left -= slice;
            self.beat();
        }
    }

    fn publish(&mut self) {
        let now = self.env.now();
        self.last_publish = now;
        let status = json!({
            "state": self.state,
            "control_supported": true,
            "receiving_requested": self.env.receiving_enabled(),
            "reason": self.reason,
            "detail": self.detail,
            "since": self.since,
            "updated": now,
            "retry_in_s": self.retry_in,
            "pluto": self.pluto,
            "producer_started_at": self.producer_started_at,
            "starts": self.starts,
            "last_exit": self.last_exit,
            "last_restore": self.last_restore,
            "supervisor_pid": std::process::id(),
        });
        self.env.publish(&status);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::VecDeque, rc::Rc};

    /// What the fake world does, scripted per call.
    #[derive(Default)]
    struct World {
        now: f64,
        stop_after: Option<f64>,
        probes: VecDeque<Probe>,
        /// Seconds each started producer lives (None = until stopped).
        lifetimes: VecDeque<Option<f64>>,
        restores: VecDeque<Restore>,
        samples_age: VecDeque<Option<f64>>,
        lock_busy: u32,
        calls: Vec<String>,
        statuses: Vec<Value>,
        snapshot: Snapshot,
        leftovers: u32,
        /// The fake radio is only legal to touch while the lock is held.
        held: bool,
        inspect_errors: u32,
        snapshot_errors: u32,
        start_errors: u32,
        /// Seconds the snapshot call takes (a slow Pluto).
        snapshot_takes: f64,
        /// Half-open intervals during which operator receiving is disabled.
        paused: Vec<(f64, f64)>,
    }
    type Shared = Rc<RefCell<World>>;

    struct FakeRadio(Shared);
    struct FakeRunning {
        world: Shared,
        ends_at: Option<f64>,
    }
    struct FakeLock(Shared);
    struct FakeEnv(Shared);

    impl Running for FakeRunning {
        fn try_wait(&mut self) -> Option<ExitInfo> {
            let w = self.world.borrow();
            assert!(w.held, "producer polled without the lock");
            self.ends_at
                .filter(|t| w.now >= *t)
                .map(|_| ExitInfo { code: Some(1), output: "boom\n".into() })
        }
        fn stop(&mut self) -> ExitInfo {
            let mut w = self.world.borrow_mut();
            assert!(w.held, "producer stopped without the lock");
            w.calls.push("producer.stop".into());
            ExitInfo { code: Some(0), output: "{\"status\":\"stopped\"}\n".into() }
        }
    }
    impl Radio for FakeRadio {
        fn probe(&mut self) -> Probe {
            let mut w = self.0.borrow_mut();
            assert!(w.held, "probe without the lock");
            w.calls.push("probe".into());
            w.probes.pop_front().unwrap_or(Probe::Ready("Pluto v0.37".into()))
        }
        fn inspect(&mut self) -> Result<Inspection, String> {
            let mut w = self.0.borrow_mut();
            assert!(w.held, "inspect without the lock");
            w.calls.push("inspect".into());
            if w.inspect_errors > 0 {
                w.inspect_errors -= 1;
                return Err("ssh failed".into());
            }
            Ok(Inspection { boot_id: "boot-1".into(), leftover_producers: std::mem::take(&mut w.leftovers) })
        }
        fn stop_leftovers(&mut self) -> Result<(), String> {
            let mut w = self.0.borrow_mut();
            assert!(w.held, "stop_leftovers without the lock");
            w.calls.push("stop_leftovers".into());
            Ok(())
        }
        fn snapshot(&mut self) -> Result<Snapshot, String> {
            let mut w = self.0.borrow_mut();
            assert!(w.held, "snapshot without the lock");
            w.calls.push("snapshot".into());
            w.now += w.snapshot_takes;
            if w.snapshot_errors > 0 {
                w.snapshot_errors -= 1;
                return Err("iio_attr failed".into());
            }
            Ok(w.snapshot.clone())
        }
        fn start(&mut self) -> Result<Box<dyn Running>, String> {
            let mut w = self.0.borrow_mut();
            assert!(w.held, "start without the lock");
            w.calls.push("start".into());
            if w.start_errors > 0 {
                w.start_errors -= 1;
                return Err("scp failed".into());
            }
            let life = w.lifetimes.pop_front().unwrap_or(None);
            let ends_at = life.map(|l| w.now + l);
            Ok(Box::new(FakeRunning { world: self.0.clone(), ends_at }))
        }
        fn verify_restored(&mut self, _: &Snapshot, _: &str, stopping: bool) -> Restore {
            let mut w = self.0.borrow_mut();
            assert!(w.held, "verify_restored without the lock");
            w.calls.push(format!("verify_restored(stopping={stopping})"));
            w.restores.pop_front().unwrap_or(Restore::Verified)
        }
        fn cleanup(&mut self) {
            let mut w = self.0.borrow_mut();
            assert!(w.held, "cleanup without the lock");
            w.calls.push("cleanup".into());
        }
    }
    impl Lock for FakeLock {
        fn try_acquire(&mut self) -> bool {
            let mut w = self.0.borrow_mut();
            if w.lock_busy > 0 {
                w.lock_busy -= 1;
                w.calls.push("lock-busy".into());
                return false;
            }
            w.calls.push("lock".into());
            w.held = true;
            true
        }
        fn release(&mut self) {
            let mut w = self.0.borrow_mut();
            w.held = false;
            w.calls.push("unlock".into());
        }
    }
    impl Env for FakeEnv {
        fn now(&self) -> f64 {
            self.0.borrow().now
        }
        fn sleep(&mut self, d: Duration) {
            self.0.borrow_mut().now += d.as_secs_f64();
        }
        fn stopping(&self) -> bool {
            let w = self.0.borrow();
            w.stop_after.is_some_and(|t| w.now >= t)
        }
        fn receiving_enabled(&self) -> bool {
            let w = self.0.borrow();
            !w.paused.iter().any(|(start, end)| w.now >= *start && w.now < *end)
        }
        fn publish(&mut self, status: &Value) {
            self.0.borrow_mut().statuses.push(status.clone());
        }
        fn samples_age(&mut self) -> Option<f64> {
            self.0.borrow_mut().samples_age.pop_front().unwrap_or(Some(0.1))
        }
    }

    fn rig(world: World) -> (Supervisor<FakeRadio, FakeLock, FakeEnv>, Shared) {
        let shared = Rc::new(RefCell::new(world));
        let sup = Supervisor::new(
            FakeRadio(shared.clone()),
            FakeLock(shared.clone()),
            FakeEnv(shared.clone()),
            Policy::default(),
        );
        (sup, shared)
    }
    fn world(stop_after: f64) -> World {
        World {
            stop_after: Some(stop_after),
            snapshot: Snapshot::from([("rx_filter_fir_en".into(), "0".into()), ("tx_filter_fir_en".into(), "0".into())]),
            ..World::default()
        }
    }
    fn count(w: &World, what: &str) -> usize {
        w.calls.iter().filter(|c| c.as_str() == what).count()
    }
    fn states(w: &World) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in &w.statuses {
            let tag = match s["reason"].as_str() {
                Some(r) => format!("{}/{r}", s["state"].as_str().unwrap()),
                None => s["state"].as_str().unwrap().to_string(),
            };
            if out.last() != Some(&tag) {
                out.push(tag);
            }
        }
        out
    }

    #[test]
    fn operator_stop_restores_and_stays_stopped_until_start() {
        let mut w = world(20.);
        w.paused = vec![(3., 12.)];
        let (mut sup, shared) = rig(w);
        sup.run();
        let w = shared.borrow();
        assert_eq!(count(&w, "start"), 2, "one initial start and one explicit resume: {:?}", w.calls);
        let paused: Vec<_> = w.statuses.iter().filter(|s| s["reason"] == "operator").collect();
        assert!(!paused.is_empty());
        assert!(paused.iter().all(|s| s["receiving_requested"] == false && s["last_restore"].as_str().unwrap().starts_with("verified")));
        assert!(w.statuses.iter().filter(|s| s["state"] == "running").all(|s| {
            let t = s["updated"].as_f64().unwrap();
            t < 3. || t >= 12.
        }));
    }

    #[test]
    fn stopped_intent_at_start_does_not_acquire_or_touch_the_radio() {
        let mut w = world(8.);
        w.paused = vec![(0., 100.)];
        let (mut sup, shared) = rig(w);
        sup.run();
        assert!(shared.borrow().calls.is_empty());
    }

    #[test]
    fn a_healthy_run_is_stopped_cleanly_with_the_radio_verified_and_the_lock_released() {
        let (mut sup, shared) = rig(world(100.));
        sup.run();
        let w = shared.borrow();
        assert_eq!(count(&w, "start"), 1);
        assert_eq!(count(&w, "producer.stop"), 1);
        assert!(w.calls.contains(&"verify_restored(stopping=true)".to_string()));
        assert_eq!(w.calls.last().unwrap(), "unlock", "cleanup then final unlock: {:?}", w.calls);
        assert!(w.calls.contains(&"cleanup".to_string()));
        let last = w.statuses.last().unwrap();
        assert_eq!(last["state"], "stopped");
        assert_eq!(last["last_restore"], "verified: settings read back equal to before");
        assert!(states(&w).contains(&"running".to_string()));
    }

    #[test]
    fn a_dead_producer_is_restored_then_restarted_after_a_short_wait_with_the_lock_released_between() {
        let mut w = world(60.);
        w.lifetimes = VecDeque::from([Some(10.), None]);
        let (mut sup, shared) = rig(w);
        sup.run();
        let w = shared.borrow();
        assert_eq!(count(&w, "start"), 2);
        assert_eq!(count(&w, "verify_restored(stopping=false)"), 1, "restore checked after the crash");
        // lock, ... unlock, lock, ... : the lock is not held through the backoff.
        let first_unlock = w.calls.iter().position(|c| c == "unlock").unwrap();
        let second_lock = w.calls.iter().rposition(|c| c == "lock").unwrap();
        assert!(first_unlock < second_lock);
        let crash = w.statuses.iter().find(|s| s["reason"] == "producer-failed").unwrap();
        assert!(crash["detail"].as_str().unwrap().contains("Restarting in 3 s"), "{crash}");
        assert!(crash["last_exit"].as_str().unwrap().contains("exit code 1: boom"));
    }

    #[test]
    fn crash_loops_back_off_and_a_stable_run_resets_the_backoff() {
        let mut w = world(10_000.);
        // Four quick failures, then a 100 s run (stable), then another quick failure.
        w.lifetimes = VecDeque::from([Some(1.), Some(1.), Some(1.), Some(1.), Some(100.), Some(1.), None]);
        let (mut sup, shared) = rig(w);
        sup.run();
        let waits: Vec<u64> = shared
            .borrow()
            .statuses
            .iter()
            .filter(|s| s["reason"] == "producer-failed")
            .map(|s| s["retry_in_s"].as_f64().unwrap() as u64)
            .fold(Vec::new(), |mut acc, w| {
                if acc.last() != Some(&w) {
                    acc.push(w);
                }
                acc
            });
        assert_eq!(waits, vec![3, 6, 12, 24, 3, 6], "grows 3,6,12,24; a healthy run resets it to 3, then it grows again");
    }

    #[test]
    fn a_held_lock_is_waited_for_without_touching_the_radio() {
        let mut w = world(30.);
        w.lock_busy = 5;
        let (mut sup, shared) = rig(w);
        sup.run();
        let w = shared.borrow();
        let first_probe = w.calls.iter().position(|c| c == "probe").unwrap();
        let busy = w.calls.iter().filter(|c| *c == "lock-busy").count();
        assert_eq!(busy, 5);
        assert!(w.calls[..first_probe].iter().all(|c| c == "lock-busy" || c == "lock"), "{:?}", w.calls);
        assert_eq!(w.calls[..first_probe].iter().filter(|c| *c == "lock").count(), 1);
        assert!(states(&w).contains(&"waiting/lock-held".to_string()));
    }

    #[test]
    fn an_unreachable_pluto_is_retried_with_backoff_capped_at_thirty_seconds() {
        let mut w = world(200.);
        w.probes = (0..8).map(|_| Probe::Unreachable("timed out".into())).collect();
        let (mut sup, shared) = rig(w);
        sup.run();
        let w = shared.borrow();
        let waits: Vec<u64> = w
            .statuses
            .iter()
            .filter(|s| s["reason"] == "unreachable")
            .map(|s| s["retry_in_s"].as_f64().unwrap() as u64)
            .fold(Vec::new(), |mut acc, x| {
                if acc.last() != Some(&x) {
                    acc.push(x);
                }
                acc
            });
        assert_eq!(waits, vec![5, 10, 20, 30]);
        assert_eq!(count(&w, "start"), 1, "starts once the Pluto answers again");
    }

    #[test]
    fn a_producer_that_runs_but_delivers_nothing_is_restarted() {
        let mut w = world(400.);
        // Past the 60 s grace, two stale health polls in a row; first start only.
        w.samples_age = VecDeque::new();
        w.lifetimes = VecDeque::from([None, None]);
        let (mut sup, shared) = rig(w);
        // Health answers: fresh for a while, then stale forever for the first run.
        shared.borrow_mut().samples_age = (0..30).map(|_| Some(0.2)).chain((0..4).map(|_| Some(500.))).collect();
        sup.run();
        let w = shared.borrow();
        assert!(count(&w, "start") >= 2, "restarted after the stall: {:?}", w.calls);
        assert!(count(&w, "producer.stop") >= 1);
        assert!(w.statuses.iter().any(|s| s["detail"].as_str().is_some_and(|d| d.contains("no data is arriving"))));
    }

    #[test]
    fn no_data_verdict_is_made_when_the_engine_cannot_be_asked_or_during_startup() {
        let mut w = world(120.);
        w.samples_age = (0..100).map(|_| None).collect();
        let (mut sup, shared) = rig(w);
        sup.run();
        assert_eq!(count(&shared.borrow(), "start"), 1);
        assert_eq!(count(&shared.borrow(), "producer.stop"), 1, "only the final stop");
        // Stale data inside the startup grace is ignored too.
        let mut w = world(50.);
        w.samples_age = (0..100).map(|_| Some(999.)).collect();
        let (mut sup, shared) = rig(w);
        sup.run();
        assert_eq!(count(&shared.borrow(), "start"), 1);
    }

    #[test]
    fn a_leftover_producer_is_stopped_before_the_snapshot() {
        let mut w = world(20.);
        w.leftovers = 1;
        let (mut sup, shared) = rig(w);
        sup.run();
        let w = shared.borrow();
        let leftover = w.calls.iter().position(|c| c == "stop_leftovers").unwrap();
        let snapshot = w.calls.iter().position(|c| c == "snapshot").unwrap();
        assert!(leftover < snapshot);
    }

    #[test]
    fn a_receiver_with_a_custom_filter_on_is_left_alone() {
        let mut w = world(30.);
        w.snapshot.insert("rx_filter_fir_en".into(), "1".into());
        let (mut sup, shared) = rig(w);
        sup.run();
        let w = shared.borrow();
        assert_eq!(count(&w, "start"), 0);
        assert!(states(&w).contains(&"waiting/radio-state".to_string()));
    }

    #[test]
    fn the_status_is_refreshed_while_waiting_so_a_stale_file_means_a_dead_supervisor() {
        let mut w = world(120.);
        w.probes = VecDeque::from([Probe::Unreachable("x".into())]);
        w.lock_busy = 0;
        let (mut sup, shared) = rig(w);
        sup.run();
        let w = shared.borrow();
        let times: Vec<f64> = w.statuses.iter().map(|s| s["updated"].as_f64().unwrap()).collect();
        let longest_gap = times.windows(2).map(|p| p[1] - p[0]).fold(0., f64::max);
        assert!(longest_gap <= 6., "status went quiet for {longest_gap} s");
    }

    #[test]
    fn unverified_restores_are_reported_not_hidden() {
        let mut w = world(100.);
        w.lifetimes = VecDeque::from([Some(5.), None]);
        w.restores = VecDeque::from([Restore::Unverified("tx_lo differs".into())]);
        let (mut sup, shared) = rig(w);
        sup.run();
        let w = shared.borrow();
        let s = w.statuses.iter().find(|s| s["last_restore"].as_str().is_some_and(|r| r.starts_with("UNVERIFIED"))).unwrap();
        assert!(s["last_restore"].as_str().unwrap().contains("tx_lo differs"));
    }

    #[test]
    fn a_failed_start_checks_the_radio_then_backs_off() {
        let mut w = world(60.);
        w.start_errors = 2;
        let (mut sup, shared) = rig(w);
        sup.run();
        let w = shared.borrow();
        assert!(count(&w, "verify_restored(stopping=false)") >= 2, "a start that failed may have opened the radio: {:?}", w.calls);
        assert!(w.statuses.iter().any(|s| s["detail"].as_str().is_some_and(|d| d.contains("could not start: scp failed"))));
        assert_eq!(count(&w, "start"), 3, "third attempt succeeds");
    }

    #[test]
    fn a_pluto_that_answers_iio_but_not_ssh_backs_off_instead_of_hammering() {
        let mut w = world(300.);
        w.inspect_errors = 4;
        let (mut sup, shared) = rig(w);
        sup.run();
        let w = shared.borrow();
        let waits: Vec<u64> = w
            .statuses
            .iter()
            .filter(|s| s["reason"] == "unreachable")
            .map(|s| s["retry_in_s"].as_f64().unwrap() as u64)
            .fold(Vec::new(), |mut acc, x| {
                if acc.last() != Some(&x) {
                    acc.push(x);
                }
                acc
            });
        assert_eq!(waits, vec![5, 10, 20, 30], "probe success must not reset the backoff");
        assert_eq!(count(&w, "start"), 1);
    }

    #[test]
    fn snapshot_failures_are_unreachable_not_producer_crashes() {
        let mut w = world(60.);
        w.snapshot_errors = 1;
        let (mut sup, shared) = rig(w);
        sup.run();
        let w = shared.borrow();
        assert!(states(&w).contains(&"waiting/unreachable".to_string()));
        assert_eq!(count(&w, "start"), 1);
    }

    #[test]
    fn a_stop_requested_during_a_slow_setup_never_starts_a_producer() {
        let mut w = world(50.);
        w.snapshot_takes = 100.; // the Pluto is slow; the stop request lands meanwhile
        let (mut sup, shared) = rig(w);
        sup.run();
        let w = shared.borrow();
        assert_eq!(count(&w, "start"), 0, "{:?}", w.calls);
        assert_eq!(w.statuses.last().unwrap()["state"], "stopped");
    }

    #[test]
    fn a_producer_that_never_delivers_data_backs_off_instead_of_cycling_every_minute() {
        let mut w = world(900.);
        w.samples_age = (0..400).map(|_| Some(500.)).collect();
        let (mut sup, shared) = rig(w);
        sup.run();
        let w = shared.borrow();
        let waits: Vec<u64> = w
            .statuses
            .iter()
            .filter(|s| s["reason"] == "producer-failed")
            .map(|s| s["retry_in_s"].as_f64().unwrap() as u64)
            .fold(Vec::new(), |mut acc, x| {
                if acc.last() != Some(&x) {
                    acc.push(x);
                }
                acc
            });
        assert!(waits.len() >= 3 && waits[0] == 3 && waits[1] == 6 && waits[2] == 12, "{waits:?}");
    }

    #[test]
    fn the_engine_gate_closes_before_the_producer_is_stopped() {
        let (mut sup, shared) = rig(world(30.));
        sup.run();
        let w = shared.borrow();
        let last_running = w.statuses.iter().rposition(|s| s["state"] == "running").unwrap();
        let stopped_state = w.statuses.iter().position(|s| s["detail"] == "Stopping and putting the radio back.").unwrap();
        assert!(stopped_state > last_running);
        // producer.stop is recorded in calls, statuses are separate: the status change is
        // published by set() before running.stop() is called, so a running status is never the
        // last one published before the stop.
        assert_ne!(w.statuses[stopped_state - 1]["state"], "stopped");
    }
}
