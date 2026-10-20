use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use radio_engine::{
    codec::{Decoder, DecoderOptions, Vocoder},
    runtime::{App, VoiceSettings, attach_control, ingest},
    voice::VoiceConfig,
};
use serde_json::json;
use std::{
    io::Read,
    thread,
    time::{Duration, Instant},
};
/// Voice synthesis and post-processing. The defaults are the shipped profile (DESIGN-VOCODER.md).
#[derive(clap::Args, Clone)]
struct VoiceArgs {
    /// AMBE+2 voice decoder: neo (mbelib-neo, the default) or mbelib (the original, for A/B
    /// and rollback).
    #[arg(long, default_value = "neo")]
    vocoder: Vocoder,
    /// Voice post-processing: high-pass, presence EQ, level normalization (on|off).
    #[arg(long, default_value = "on", value_parser = ["on", "off"])]
    voice_post: String,
    /// Post-processing high-pass corner in Hz.
    #[arg(long, default_value_t = VoiceConfig::default().highpass_hz)]
    voice_highpass_hz: f64,
    /// Presence peak gain in dB (0 disables), centred at --voice-presence-hz.
    #[arg(long, default_value_t = VoiceConfig::default().presence_db, allow_negative_numbers = true)]
    voice_presence_db: f64,
    #[arg(long, default_value_t = VoiceConfig::default().presence_hz)]
    voice_presence_hz: f64,
    /// Active-speech level the normalizer aims for, dBFS RMS.
    #[arg(long, default_value_t = VoiceConfig::default().target_dbfs, allow_negative_numbers = true)]
    voice_target_dbfs: f64,
    /// Level-control gain before it has heard the talker (start of every call), dB.
    #[arg(long, default_value_t = VoiceConfig::default().initial_gain_db)]
    voice_initial_gain_db: f64,
    /// Enable the noise gate: closes when audio is this many dB below the talker's level.
    #[arg(long)]
    voice_gate_db: Option<f64>,
    /// DEV-ONLY (md380-oracle builds, `probe` only): directory the MD380 oracle was installed to
    /// by oracle/fetch-md380.sh (default: engine/.native-md380).
    #[cfg(feature = "md380-oracle")]
    #[arg(long)]
    oracle_dir: Option<std::path::PathBuf>,
}
impl VoiceArgs {
    fn voice_config(&self) -> Result<VoiceConfig> {
        let cfg = VoiceConfig {
            enabled: self.voice_post == "on",
            highpass_hz: self.voice_highpass_hz,
            presence_db: self.voice_presence_db,
            presence_hz: self.voice_presence_hz,
            target_dbfs: self.voice_target_dbfs,
            initial_gain_db: self.voice_initial_gain_db,
            gate_below_db: self.voice_gate_db,
            ..VoiceConfig::default()
        };
        cfg.validate().map_err(anyhow::Error::msg)?;
        Ok(cfg)
    }
}
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
        #[command(flatten)]
        voice: VoiceArgs,
    },
    /// Offline development tool: run the voice post-processing chain over mono 8 kHz signed16
    /// raw audio (always enabled; tune it with the --voice-* flags).
    VoicePost {
        input: std::path::PathBuf,
        output: std::path::PathBuf,
        #[command(flatten)]
        voice: VoiceArgs,
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
        /// Write the selected slot's decoded 8 kHz audio to this WAV file.
        #[arg(long)]
        wav: Option<std::path::PathBuf>,
        #[command(flatten)]
        voice: VoiceArgs,
    },
}
#[tokio::main]
async fn main() -> Result<()> {
    match Args::parse().command {
        Command::VoicePost { input, output, voice } => {
            let mut config = voice.voice_config()?;
            config.enabled = true;
            let bytes = std::fs::read(&input).with_context(|| format!("cannot read {}", input.display()))?;
            let mut pcm: Vec<i16> = bytes.chunks_exact(2).map(|x| i16::from_le_bytes([x[0], x[1]])).collect();
            let mut post = radio_engine::voice::VoicePost::new(config);
            for chunk in pcm.chunks_mut(160) {
                post.process(chunk);
            }
            std::fs::write(&output, pcm.iter().flat_map(|s| s.to_le_bytes()).collect::<Vec<u8>>())?;
        }
        Command::Probe {
            input,
            protocol,
            slot,
            paced,
            json: _,
            wav,
            voice,
        } => {
            #[cfg(feature = "md380-oracle")]
            if voice.vocoder == Vocoder::Md380 {
                radio_engine::oracle::load(&voice.oracle_dir.clone().unwrap_or_else(radio_engine::oracle::default_dir))
                    .map_err(|e| anyhow::anyhow!("MD380 oracle: {e}"))?;
            }
            let options = DecoderOptions {
                vocoder: voice.vocoder,
                voice: voice.voice_config()?,
            };
            let mut wav_pcm: Vec<i16> = Vec::new();
            let mut input: Box<dyn Read> = if input == "-" {
                Box::new(std::io::stdin())
            } else {
                Box::new(std::fs::File::open(input)?)
            };
            let mut decoder = Decoder::with_options(&protocol, &options)?;
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
                if wav.is_some() {
                    wav_pcm.extend_from_slice(&audio);
                }
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
            if let Some(path) = wav {
                std::fs::write(&path, wav_bytes(&wav_pcm))
                    .with_context(|| format!("cannot write {}", path.display()))?;
            }
            println!(
                "{}",
                json!({"input_samples":frames,"audio_samples":samples,"audio_chunks":chunks,"first_audio_wall_s":first,"wall_s":start.elapsed().as_secs_f64(),"vocoder":options.vocoder.name(),"voice_post":options.voice.enabled})
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
            voice,
            receiver_status,
        } => {
            let voice_post = voice.voice_config()?;
            let vocoder = voice.vocoder;
            #[cfg(feature = "md380-oracle")]
            anyhow::ensure!(
                vocoder != Vocoder::Md380,
                "the MD380 oracle is a test-only tool and cannot serve"
            );
            eprintln!(
                "voice: vocoder {}; post-processing {}",
                vocoder.name(),
                if voice_post.enabled { "on" } else { "off" }
            );
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
                voice: VoiceSettings { vocoder, post: voice_post },
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
                thread::spawn(move || {
                    radio_engine::link::run_source(
                        ingest_app,
                        radio_engine::link::LinkConfig::new(address),
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

/// 8 kHz mono signed16 WAV.
fn wav_bytes(pcm: &[i16]) -> Vec<u8> {
    let data = pcm.len() * 2;
    let mut out = Vec::with_capacity(44 + data);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&8000u32.to_le_bytes());
    out.extend_from_slice(&16000u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data as u32).to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}
