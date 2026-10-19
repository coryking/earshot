//! Offline acceptance driver. Not a production replay API.
use radio_engine::wire::Frame;
use serde_json::json;
use std::{
    fs::File,
    io::Read,
    net::TcpStream,
    thread,
    time::{Duration, Instant},
};
fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    anyhow::ensure!(
        args.len() == 3,
        "fixture_ingress CI16_25K_FILE 127.0.0.1:8811"
    );
    let mut socket = TcpStream::connect(&args[2])?;
    socket.set_nodelay(true)?;
    let mut input = File::open(&args[1])?;
    let config = json!({"input_center_hz":454216998,"input_rate_hz":6000000,"input_bandwidth_hz":6000000,"hardware_counter":false,"capabilities":{"controls_supported":false},"streams":[{"stream_id":1,"center_hz":454029223,"target_hz":454029223,"rate_hz":25000,"protocol":"Unknown","slot":1}]});
    let frame = |kind, sequence, sample, payload| Frame {
        kind,
        flags: 0,
        generation: 1,
        stream_id: if kind == 4 { 0 } else { 1 },
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
    };
    frame(4, 0, 0, config.to_string().into_bytes()).write(&mut socket)?;
    eprintln!(
        "Fixture ready: select discovered frequency via /api/monitor before speech; starting in 1s"
    );
    thread::sleep(Duration::from_secs(1));
    let started = Instant::now();
    let mut bytes = vec![0u8; 10000];
    let mut sequence = 0;
    let mut sample = 0;
    loop {
        let n = input.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        frame(1, sequence, sample, bytes[..n].to_vec()).write(&mut socket)?;
        // PSD rows here are explicitly fixture measurements, derived from a power value,
        // not a synthetic production waterfall. Production rows come from the producer.
        let activity = json!({"rf_active":true,"level_db":-30.});
        frame(3, sequence + 1, sample, activity.to_string().into_bytes()).write(&mut socket)?;
        sample += n as u64 / 4;
        sequence += 2;
        let delay = sample as f64 / 25000. - started.elapsed().as_secs_f64();
        if delay > 0. {
            thread::sleep(Duration::from_secs_f64(delay));
        }
    }
    thread::sleep(Duration::from_secs(2));
    Ok(())
}
