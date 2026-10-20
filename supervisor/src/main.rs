//! radio-supervisor: the real SSH, libiio and flock behind `radio_supervisor::Supervisor`.
//! RX only. Nothing here transmits, flashes, or touches the NOR or the U-Boot environment.
//!
//! The Pluto's SSH password comes from `PLUTO_SSH_PASSWORD` (the ADI factory default when
//! unset), as in the Python tools. Host keys are not pinned: the link is a private bridge to a
//! device whose key legitimately changes when its image is replaced.
use clap::Parser;
use radio_supervisor::{
    Env, ExitInfo, Inspection, Lock, Policy, Probe, Radio, Restore, Running, Snapshot, Supervisor,
};
use serde_json::Value;
use std::{
    fs::{File, OpenOptions, TryLockError},
    io::{BufRead, BufReader, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Parser, Clone)]
#[command(about = "Keep the Pluto's channel producer running; restore the radio when it stops. RX only.")]
struct Args {
    #[arg(long, default_value = "192.168.10.10")]
    host: String,
    /// pluto-fpga-source, built by fpga/sw/build.sh. The Pluto must run the channelizer
    /// firmware (fpga/README.md); on any other image the producer exits and is retried.
    #[arg(long)]
    binary: PathBuf,
    #[arg(long, default_value_t = 9010)]
    port: u16,
    /// RX LO. The channelizer covers about 20 MHz around it in 512 bins of 50 kHz.
    #[arg(long, default_value_t = 460_000_000)]
    center: u64,
    /// Manual RX gain in dB (the activity detector needs manual gain).
    #[arg(long, default_value_t = 40)]
    gain: u32,
    /// Stream IDs (FFT bin + 1) to forward IQ for from the start; the engine pins others.
    #[arg(long, default_value = "1")]
    forward: String,
    #[arg(long, default_value = "/tmp/pluto-radio.lock")]
    lock_file: PathBuf,
    /// Written for the engine and UI (radio-engine --receiver-status).
    #[arg(long)]
    status_file: PathBuf,
    /// The engine's loopback HTTP address, asked whether data is arriving. Empty disables it.
    #[arg(long, default_value = "127.0.0.1:8810")]
    engine: String,
}

/// The producer's file name on the Pluto, under /tmp.
const REMOTE_NAME: &str = "pluto-fpga-source";
/// Output line that means "listening, radio not yet opened".
const READY: &str = "waiting on TCP";
/// `ps | grep` pattern for the producer, written so grep does not match itself.
const PROCESS_PATTERN: &str = "[p]luto-fpga-source";

fn producer_args(args: &Args) -> Result<String, String> {
    if !args.forward.chars().all(|c| c.is_ascii_digit() || c == ',') {
        return Err(format!("--forward {:?} must be stream IDs separated by commas", args.forward));
    }
    Ok(format!("--port {} --center {} --gain {} --forward-streams {}", args.port, args.center, args.gain, args.forward))
}

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn epoch() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.)
}

struct Captured {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Run to completion with a deadline; the child is killed when it is exceeded.
fn capture(mut command: Command, timeout: Duration) -> Result<Captured, String> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| format!("cannot run {:?}: {e}", command.get_program()))?;
    let drain = |mut pipe: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut text = String::new();
            let _ = pipe.read_to_string(&mut text);
            text
        })
    };
    let out = drain(Box::new(child.stdout.take().unwrap()));
    let err = drain(Box::new(child.stderr.take().unwrap()));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {} s", timeout.as_secs()));
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    };
    Ok(Captured {
        code: status.code(),
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}

fn first_line(text: &str) -> String {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("no output").chars().take(160).collect()
}

/// Attributes that must read back the same after a run. The TX clock follows the RX rate in the
/// AD9361 driver, so it is verified but never written.
const RESTORED: [&str; 8] = [
    "rx_sampling_frequency",
    "rx_rf_bandwidth",
    "rx_gain_control_mode",
    "rx_filter_fir_en",
    "rx_lo",
    "tx_sampling_frequency",
    "tx_filter_fir_en",
    "tx_lo",
];

/// name -> iio_attr arguments (channel selection and attribute).
fn attributes() -> Vec<(&'static str, Vec<&'static str>)> {
    let rx = ["-i", "-c", "ad9361-phy", "voltage0"];
    let tx = ["-o", "-c", "ad9361-phy", "voltage0"];
    let with = |base: [&'static str; 4], attr: &'static str| {
        let mut v = base.to_vec();
        v.push(attr);
        v
    };
    vec![
        ("rx_sampling_frequency", with(rx, "sampling_frequency")),
        ("rx_rf_bandwidth", with(rx, "rf_bandwidth")),
        ("rx_gain_control_mode", with(rx, "gain_control_mode")),
        ("rx_filter_fir_en", with(rx, "filter_fir_en")),
        ("rx_lo", with(["-o", "-c", "ad9361-phy", "altvoltage0"], "frequency")),
        ("tx_sampling_frequency", with(tx, "sampling_frequency")),
        ("tx_filter_fir_en", with(tx, "filter_fir_en")),
        ("tx_lo", with(["-o", "-c", "ad9361-phy", "altvoltage1"], "frequency")),
    ]
}

#[derive(Clone)]
struct Pluto {
    args: Args,
    password: String,
    /// Producer command-line arguments (validated: they reach a remote shell).
    producer_args: String,
}

impl Pluto {
    fn new(args: Args, producer_args: String) -> Self {
        let password = std::env::var("PLUTO_SSH_PASSWORD").unwrap_or_else(|_| "analog".into());
        Self { args, password, producer_args }
    }
    fn target(&self) -> String {
        format!("root@{}", self.args.host)
    }
    fn ssh_options() -> [&'static str; 14] {
        [
            "-o", "PubkeyAuthentication=no",
            "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-o", "ConnectTimeout=8",
            "-o", "ServerAliveInterval=5",
            "-o", "ServerAliveCountMax=3",
            "-o", "LogLevel=ERROR",
        ]
    }
    fn ssh(&self, remote: &str) -> Command {
        let mut c = Command::new("sshpass");
        c.args(["-e", "ssh"]).args(Self::ssh_options()).arg(self.target()).arg(remote);
        c.env("SSHPASS", &self.password);
        c
    }
    fn remote(&self, remote: &str, timeout: Duration) -> Result<String, String> {
        let out = capture(self.ssh(remote), timeout)?;
        match out.code {
            Some(0) => Ok(out.stdout),
            code => Err(format!("ssh failed ({code:?}): {}", first_line(&out.stderr))),
        }
    }
    fn uri(&self) -> String {
        format!("ip:{}", self.args.host)
    }
    fn iio_attr(&self, args: &[&str]) -> Result<String, String> {
        let mut c = Command::new("iio_attr");
        c.args(["-u", &self.uri()]).args(args);
        let out = capture(c, Duration::from_secs(15))?;
        if out.code != Some(0) {
            return Err(format!("iio_attr {} failed: {}", args.join(" "), first_line(&out.stderr)));
        }
        out.stdout
            .lines()
            .last()
            .map(|l| l.trim().to_string())
            .ok_or_else(|| format!("iio_attr {} printed nothing", args.join(" ")))
    }
    fn producer_count(&self) -> Result<u32, String> {
        let out = self.remote(&format!("ps | grep -c -E '{}'; true", PROCESS_PATTERN), Duration::from_secs(20))?;
        Ok(out.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0))
    }
    /// A client is or was just connected to the producer's port (any state but LISTEN), so the
    /// radio may be open or still closing.
    fn client_attached(&self) -> Result<bool, String> {
        let table = self.remote("cat /proc/net/tcp", Duration::from_secs(20))?;
        Ok(client_in_table(&table, self.args.port))
    }
    /// TERM so the producer restores the radio itself. A producer still waiting for its client
    /// ignores TERM (libc restarts accept()), but then the radio is not open and KILL is safe.
    fn stop_remote_producers(&self) -> Result<(), String> {
        let kill = |signal: &str| {
            self.remote(
                &format!("kill -{signal} $(ps | grep -E '{}' | awk '{{print $1}}') 2>/dev/null; true", PROCESS_PATTERN),
                Duration::from_secs(20),
            )
            .map(|_| ())
        };
        let gone_within = |seconds: u64| -> Result<bool, String> {
            let end = Instant::now() + Duration::from_secs(seconds);
            loop {
                if self.producer_count()? == 0 {
                    return Ok(true);
                }
                if Instant::now() >= end {
                    return Ok(false);
                }
                thread::sleep(Duration::from_millis(700));
            }
        };
        if self.producer_count()? == 0 {
            return Ok(());
        }
        // Asked before TERM: the producer may close its client socket as the first step of
        // shutting down, and then the question has no honest answer.
        let had_client = self.client_attached()?;
        kill("TERM")?;
        if gone_within(4)? {
            return Ok(());
        }
        // Still there. With a client the radio is open and the producer is restoring: give it
        // time. With none it never opened the radio (and ignores TERM in accept()).
        if had_client && gone_within(25)? {
            return Ok(());
        }
        kill("KILL")?;
        if gone_within(8)? {
            Ok(())
        } else {
            Err("a producer survived KILL".into())
        }
    }
    fn read_snapshot(&self) -> Result<Snapshot, String> {
        let mut snapshot = Snapshot::new();
        for (name, attr) in attributes() {
            snapshot.insert(name.to_string(), self.iio_attr(&attr)?);
        }
        Ok(snapshot)
    }
    fn inspect_now(&self) -> Result<Inspection, String> {
        let out = self.remote(
            &format!("cat /proc/sys/kernel/random/boot_id; ps | grep -c -E '{}'; true", PROCESS_PATTERN),
            Duration::from_secs(20),
        )?;
        let mut lines = out.lines().map(str::trim);
        let boot_id = lines.next().filter(|l| !l.is_empty()).ok_or("no boot id")?.to_string();
        let leftover = lines.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        Ok(Inspection { boot_id, leftover_producers: leftover })
    }
}

fn client_in_table(table: &str, port: u16) -> bool {
    let port = format!(":{port:04X}");
    table.lines().skip(1).any(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        f.len() > 3 && f[1].ends_with(&port) && f[3] != "0A"
    })
}

fn differences(before: &Snapshot, after: &Snapshot) -> Vec<String> {
    RESTORED
        .iter()
        .filter(|k| before.get(**k) != after.get(**k))
        .map(|k| format!("{k}: {:?} -> {:?}", before.get(*k), after.get(*k)))
        .collect()
}

impl Radio for Pluto {
    fn probe(&mut self) -> Probe {
        let mut c = Command::new("iio_info");
        c.args(["-u", &self.uri()]);
        let out = match capture(c, Duration::from_secs(25)) {
            Ok(o) => o,
            Err(e) => return Probe::Unreachable(e),
        };
        if out.code != Some(0) || out.stdout.trim().is_empty() {
            return Probe::Unreachable(first_line(&out.stderr));
        }
        let keep = ["hw_model:", "hw_model_variant:", "fw_version:"];
        let summary = out
            .stdout
            .lines()
            .map(str::trim)
            .filter(|l| keep.iter().any(|k| l.starts_with(k)))
            .collect::<Vec<_>>()
            .join("; ");
        Probe::Ready(summary)
    }
    fn inspect(&mut self) -> Result<Inspection, String> {
        self.inspect_now()
    }
    fn stop_leftovers(&mut self) -> Result<(), String> {
        self.stop_remote_producers()
    }
    fn snapshot(&mut self) -> Result<Snapshot, String> {
        self.read_snapshot()
    }
    fn start(&mut self) -> Result<Box<dyn Running>, String> {
        let binary = &self.args.binary;
        if !binary.is_file() {
            return Err(format!("producer binary not found: {}", binary.display()));
        }
        let mut copy = Command::new("sshpass");
        copy.args(["-e", "scp", "-O"]).args(Self::ssh_options()).arg(binary);
        copy.arg(format!("{}:/tmp/{REMOTE_NAME}", self.target())).env("SSHPASS", &self.password);
        let copied = capture(copy, Duration::from_secs(60))?;
        if copied.code != Some(0) {
            return Err(format!("copying the producer failed: {}", first_line(&copied.stderr)));
        }
        if STOP.load(Ordering::SeqCst) {
            return Err("stop requested".into());
        }
        // The producer runs in the foreground of this SSH session, so the session ending means
        // the producer ended. The wrapper turns hangup/termination (an SSH drop, a stop request)
        // into TERM for the producer, which restores the radio before it exits.
        let remote = format!(
            "cd /tmp && chmod +x ./{name} && ./{name} {args} & \
             p=$!; trap 'kill -TERM $p 2>/dev/null' HUP INT TERM; rc=0; \
             while kill -0 $p 2>/dev/null; do wait $p; rc=$?; done; exit $rc",
            name = REMOTE_NAME,
            args = self.producer_args
        );
        let mut command = self.ssh(&remote);
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|e| format!("cannot run ssh: {e}"))?;
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        for pipe in [
            Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>,
            Box::new(child.stderr.take().unwrap()),
        ] {
            let lines = lines.clone();
            thread::spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    let mut kept = lines.lock().unwrap();
                    kept.push(line);
                    let excess = kept.len().saturating_sub(100);
                    kept.drain(..excess);
                }
            });
        }
        let mut run = PlutoRun { pluto: self.clone(), child, lines };
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if run.tail().contains(READY) {
                return Ok(Box::new(run));
            }
            if let Some(exit) = run.try_wait() {
                return Err(format!("the producer exited while starting: {}", exit.output.trim()));
            }
            if STOP.load(Ordering::SeqCst) {
                run.stop();
                return Err("stop requested while starting".into());
            }
            if Instant::now() >= deadline {
                run.stop();
                return Err(format!("the producer did not start listening: {}", run.tail().trim()));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    fn verify_restored(&mut self, before: &Snapshot, boot_id: &str, _stopping: bool) -> Restore {
        // Patient while the Pluto reboots, but a stop request (read each round, not once) cuts
        // the wait to a few seconds so shutdown stays inside the unit's stop timeout.
        let began = Instant::now();
        let inspection = loop {
            match self.inspect_now() {
                Ok(i) => break i,
                Err(e) => {
                    let waited = began.elapsed();
                    let limit = if STOP.load(Ordering::SeqCst) { 8 } else { 90 };
                    if waited >= Duration::from_secs(limit) {
                        return Restore::Unverified(format!("the Pluto did not answer for {} s ({e})", waited.as_secs()));
                    }
                    thread::sleep(Duration::from_secs(3));
                }
            }
        };
        if inspection.boot_id != boot_id {
            return Restore::Rebooted;
        }
        if inspection.leftover_producers > 0
            && let Err(e) = self.stop_remote_producers()
        {
            return Restore::Unverified(e);
        }
        let mut after = match self.read_snapshot() {
            Ok(s) => s,
            Err(e) => return Restore::Unverified(e),
        };
        let mut bad = differences(before, &after);
        if bad.is_empty() {
            return Restore::Verified;
        }
        // Receive controls only, one at a time, every write attempted.
        // The ADI low-rate helper the producer runs enables both FIRs; a producer that was killed
        // leaves them on. They go first (a rate change is refused while a FIR is on). These are
        // filter enable bits, not transmit controls.
        let writes = [
            "rx_filter_fir_en",
            "tx_filter_fir_en",
            "rx_sampling_frequency",
            "rx_rf_bandwidth",
            "rx_lo",
            "rx_gain_control_mode",
        ];
        let table = attributes();
        let mut errors = Vec::new();
        for name in writes {
            let Some((_, attr)) = table.iter().find(|(n, _)| *n == name) else { continue };
            let mut with_value = attr.clone();
            let value = before[name].clone();
            with_value.push(&value);
            if let Err(e) = self.iio_attr(&with_value) {
                errors.push(e);
            }
        }
        let keys: Vec<String> = bad.iter().map(|d| d.split(':').next().unwrap_or("").to_string()).collect();
        match self.read_snapshot() {
            Ok(s) => after = s,
            Err(e) => return Restore::Unverified(e),
        }
        bad = differences(before, &after);
        if bad.is_empty() {
            Restore::Corrected(keys)
        } else {
            Restore::Unverified(format!("still different after correcting: {}; {}", bad.join("; "), errors.join("; ")))
        }
    }
    fn cleanup(&mut self) {
        let _ = self.remote(&format!("rm -f /tmp/{REMOTE_NAME}"), Duration::from_secs(20));
    }
}

struct PlutoRun {
    pluto: Pluto,
    child: Child,
    lines: Arc<Mutex<Vec<String>>>,
}

impl PlutoRun {
    fn tail(&self) -> String {
        self.lines.lock().unwrap().join("\n")
    }
}

impl Running for PlutoRun {
    fn try_wait(&mut self) -> Option<ExitInfo> {
        let status = self.child.try_wait().ok()??;
        thread::sleep(Duration::from_millis(100)); // let the output readers drain
        Some(ExitInfo { code: status.code(), output: self.tail() })
    }
    fn stop(&mut self) -> ExitInfo {
        // Through a second session so it works even if the carrying session is wedged. If the
        // Pluto does not answer there is nothing more to do remotely; the session is closed below.
        let _ = self.pluto.stop_remote_producers();
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break status.code(),
                _ if Instant::now() >= deadline => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break None;
                }
                _ => thread::sleep(Duration::from_millis(100)),
            }
        };
        thread::sleep(Duration::from_millis(100));
        ExitInfo { code: status, output: self.tail() }
    }
}

impl Drop for PlutoRun {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct FileLock {
    path: PathBuf,
    held: Option<File>,
    /// The last error logged, so a persistent one is reported once, not every 2 s.
    logged: Option<String>,
}

impl FileLock {
    fn complain(&mut self, what: String) {
        if self.logged.as_ref() != Some(&what) {
            eprintln!("{what}");
            self.logged = Some(what);
        }
    }
}

impl Lock for FileLock {
    fn try_acquire(&mut self) -> bool {
        if self.held.is_some() {
            return true;
        }
        let file = match OpenOptions::new().create(true).append(true).open(&self.path) {
            Ok(f) => f,
            Err(e) => {
                self.complain(format!("cannot open {}: {e}", self.path.display()));
                return false;
            }
        };
        match file.try_lock() {
            Ok(()) => {
                self.held = Some(file);
                true
            }
            Err(TryLockError::WouldBlock) => false,
            Err(TryLockError::Error(e)) => {
                self.complain(format!("cannot lock {}: {e}", self.path.display()));
                false
            }
        }
    }
    fn release(&mut self) {
        if let Some(file) = self.held.take() {
            let _ = file.unlock();
        }
    }
}

struct RealEnv {
    status_file: PathBuf,
    engine: Option<String>,
    logged: Option<(String, String)>,
}

impl RealEnv {
    fn write_status(&self, status: &Value) -> std::io::Result<()> {
        if let Some(dir) = self.status_file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.status_file.with_extension("json.tmp");
        std::fs::File::create(&tmp)?.write_all(status.to_string().as_bytes())?;
        std::fs::rename(&tmp, &self.status_file)
    }
}

impl Env for RealEnv {
    fn now(&self) -> f64 {
        epoch()
    }
    fn sleep(&mut self, duration: Duration) {
        thread::sleep(duration);
    }
    fn stopping(&self) -> bool {
        STOP.load(Ordering::SeqCst)
    }
    fn publish(&mut self, status: &Value) {
        if let Err(e) = self.write_status(status) {
            eprintln!("cannot write {}: {e}", self.status_file.display());
        }
        let key = (
            status["state"].as_str().unwrap_or("").to_string(),
            status["reason"].as_str().unwrap_or("").to_string(),
        );
        if self.logged.as_ref() != Some(&key) {
            println!("{} {} {}", key.0, key.1, status["detail"].as_str().unwrap_or(""));
            if let Some(restore) = status["last_restore"].as_str() {
                println!("  last restore: {restore}");
            }
            if let Some(exit) = status["last_exit"].as_str() {
                println!("  last exit: {exit}");
            }
            let _ = std::io::stdout().flush();
            self.logged = Some(key);
        }
    }
    fn samples_age(&mut self) -> Option<f64> {
        let address = self.engine.as_ref()?.to_socket_addrs().ok()?.next()?;
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2)).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(4))).ok()?;
        stream.write_all(b"GET /api/status HTTP/1.0\r\nHost: engine\r\n\r\n").ok()?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).ok()?;
        let split = response.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
        let status: Value = serde_json::from_slice(&response[split..]).ok()?;
        // Never received anything counts as very old; the caller applies a startup grace.
        Some(status["last_samples_at"].as_f64().map_or(1e9, |t| (epoch() - t).max(0.)))
    }
}

fn main() {
    let args = Args::parse();
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        unsafe { libc::signal(signal, on_signal as *const () as libc::sighandler_t) };
    }
    let producer = producer_args(&args).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2);
    });
    println!("producer: {} {producer}", args.binary.display());
    let engine = (!args.engine.is_empty()).then(|| args.engine.clone());
    let env = RealEnv { status_file: args.status_file.clone(), engine, logged: None };
    let lock = FileLock { path: args.lock_file.clone(), held: None, logged: None };
    // Liveness for the engine: the status file's age says "the supervisor process is alive", even
    // while the main thread sits in a long ssh, scp or reboot wait.
    let status_file = args.status_file.clone();
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_secs(3));
            if let Ok(file) = OpenOptions::new().write(true).open(&status_file) {
                let _ = file.set_modified(SystemTime::now());
            }
        }
    });
    let mut supervisor = Supervisor::new(Pluto::new(args, producer), lock, env, Policy::default());
    supervisor.run();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn producer_arguments_default_to_the_wide_window_and_reject_shell_text() {
        let args = Args::parse_from(["x", "--binary", "/b/pluto-fpga-source", "--status-file", "/s"]);
        assert_eq!(producer_args(&args).unwrap(), "--port 9010 --center 460000000 --gain 40 --forward-streams 1");
        let bad = Args::parse_from(["x", "--binary", "/b", "--status-file", "/s", "--forward", "1;reboot"]);
        assert!(producer_args(&bad).is_err());
    }

    #[test]
    fn a_client_on_the_producer_port_in_any_state_but_listen_counts_as_attached() {
        let header = "  sl  local_address rem_address   st\n";
        let row = |local: &str, st: &str| format!("   0: {local} 0A0AA8C0:C350 {st} 00000000:00000000 00:00000000 00000000\n");
        let listen = format!("{header}{}", row("00000000:2332", "0A"));
        assert!(!client_in_table(&listen, 9010), "listening only");
        for state in ["01", "04", "05", "06", "08"] {
            let t = format!("{listen}{}", row("0A0AA8C0:2332", state));
            assert!(client_in_table(&t, 9010), "state {state}");
        }
        let other = format!("{header}{}", row("0A0AA8C0:1F90", "01"));
        assert!(!client_in_table(&other, 9010), "a different port");
    }
}
