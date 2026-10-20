use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use radio_engine::{
    codec::Decoder,
    runtime::{App, attach_control, ingest},
};
use serde_json::json;
use std::{
    io::Read,
    thread,
    time::{Duration, Instant},
};
#[derive(Parser)]
#[command(
    about = "Receive-only native channelized radio engine. No TX path. Exit: 0 success, 1 failure, 2 usage."
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(long, default_value_t = 8810)]
        port: u16,
        #[arg(long, default_value_t = 8811)]
        ingest_port: u16,
        #[arg(long, default_value_t = 8)]
        max_decoders: usize,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        web_dir: Option<std::path::PathBuf>,
        /// How many ppm HIGH the radio reads (true = reading / (1 + ppm*1e-6)). Pluto Rev.B on
        /// wigglebutt measured +23.3 (ATSC pilots, RadioReference raster fit).
        #[arg(long, default_value_t = 0.0, allow_negative_numbers = true)]
        freq_correction_ppm: f64,
        /// Plain-name channel labels (engine/data/channel-labels.json).
        #[arg(long)]
        labels: Option<std::path::PathBuf>,
        /// Directory for recent calls (WAV), activity history and the catalog; omitted means
        /// memory only.
        #[arg(long)]
        state_dir: Option<std::path::PathBuf>,
        /// Memory held for recent call audio (older audio is served from state-dir).
        #[arg(long, default_value_t = 64)]
        call_memory_mb: usize,
        /// The receiver supervisor's status file (radio-supervisor --status-file). With
        /// --source, the engine then connects only while it says a producer is running, and
        /// /api/status reports what the supervisor says.
        #[arg(long)]
        receiver_status: Option<std::path::PathBuf>,
    },
    /// Offline fixture only: mono 48 kHz discriminator signed16 raw input; '-' for stdin.
    Probe {
        input: String,
        #[arg(long, default_value = "DMR")]
        protocol: String,
        #[arg(long, default_value_t = 2)]
        slot: u8,
        #[arg(long)]
        paced: bool,
        #[arg(long)]
        json: bool,
    },
}
#[tokio::main]
async fn main() -> Result<()> {
    match Args::parse().command {
        Command::Probe {
            input,
            protocol,
            slot,
            paced,
            json: _,
        } => {
            let mut input: Box<dyn Read> = if input == "-" {
                Box::new(std::io::stdin())
            } else {
                Box::new(std::fs::File::open(input)?)
            };
            let mut decoder = Decoder::new(&protocol)?;
            let start = Instant::now();
            let (mut chunks, mut samples, mut frames, mut first) = (0, 0, 0, None);
            let mut bytes = [0u8; 9600];
            loop {
                let n = input.read(&mut bytes)?;
                if n == 0 {
                    break;
                }
                let pcm: Vec<_> = bytes[..n]
                    .chunks_exact(2)
                    .map(|x| i16::from_le_bytes([x[0], x[1]]))
                    .collect();
                frames += pcm.len();
                let decoded = decoder.push(&pcm);
                let audio = if slot == 2 { decoded.slot2 } else { decoded.slot1 };
                if !audio.is_empty() {
                    chunks += 1;
                    samples += audio.len();
                    if first.is_none() {
                        first = Some(start.elapsed().as_secs_f64());
                    }
                }
                if paced {
                    thread::sleep(Duration::from_secs_f64(
                        (frames as f64 / 48000. - start.elapsed().as_secs_f64()).max(0.),
                    ));
                }
            }
            println!(
                "{}",
                json!({"input_samples":frames,"audio_samples":samples,"audio_chunks":chunks,"first_audio_wall_s":first,"wall_s":start.elapsed().as_secs_f64()})
            );
        }
        Command::Serve {
            port,
            ingest_port,
            max_decoders,
            source,
            web_dir,
            freq_correction_ppm,
            labels,
            state_dir,
            call_memory_mb,
            receiver_status,
        } => {
            anyhow::ensure!(
                (3..=64).contains(&max_decoders),
                "max-decoders must be 3..64"
            );
            anyhow::ensure!(
                freq_correction_ppm.abs() <= 500.,
                "freq-correction-ppm must be within +-500"
            );
            if let Some(dir) = &state_dir {
                radio_engine::store::probe_dir(dir)
                    .with_context(|| format!("state dir {} is not writable", dir.display()))?;
            }
            let labels = match labels {
                Some(path) => radio_engine::freq::Labels::load(&path)
                    .with_context(|| format!("cannot load labels {}", path.display()))?,
                None => Default::default(),
            };
            let app = App::configured(radio_engine::runtime::AppConfig {
                ppm: freq_correction_ppm,
                labels,
                state_dir,
                call_memory_mb,
                receiver_status: receiver_status.clone(),
                ..radio_engine::runtime::AppConfig::new(max_decoders)
            });
            radio_engine::runtime::spawn_janitor(app.clone());
            let flush_app = app.clone();
            let ingest_app = app.clone();
            if let Some(source) = source {
                let address = source
                    .strip_prefix("tcp:")
                    .context("source must be tcp:HOST:PORT")?
                    .to_string();
                // Reconnects with backoff for as long as the engine runs.
                let gate = receiver_status.is_some();
                thread::spawn(move || {
                    radio_engine::link::run_source(
                        ingest_app,
                        radio_engine::link::LinkConfig::new(address),
                        gate,
                    )
                });
            } else {
                thread::spawn(move || {
                    let run = || -> Result<()> {
                        let listener = std::net::TcpListener::bind(("127.0.0.1", ingest_port))?;
                        for stream in listener.incoming() {
                            let s = stream?;
                            attach_control(&s, &ingest_app)?;
                            if let Err(error) = ingest(s, ingest_app.clone()) {
                                ingest_app.error(error.to_string());
                            }
                        }
                        Ok(())
                    };
                    if let Err(e) = run() {
                        ingest_app.error(e.to_string());
                    }
                });
            }
            let router = radio_engine::api::router(app, web_dir);
            let server = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
            eprintln!(
                "radio-engine HTTP 127.0.0.1:{port}; RIQ1 {}",
                if ingest_port > 0 {
                    format!("127.0.0.1:{ingest_port}")
                } else {
                    "source".into()
                }
            );
            axum::serve(server, router)
                .with_graceful_shutdown(async {
                    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("SIGTERM handler");
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {},
                        _ = term.recv() => {},
                    }
                })
                .await?;
            radio_engine::runtime::flush(&flush_app);
        }
    }
    Ok(())
}
