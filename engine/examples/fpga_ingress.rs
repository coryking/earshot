//! Offline FPGA-shaped ingress. Never opens a radio or a live source port.
use anyhow::Result;
use clap::Parser;
use radio_engine::wire::Frame;
use serde_json::{Value, json};
use std::{collections::HashMap, io::Read, net::TcpStream, path::PathBuf, sync::mpsc, thread, time::{Duration, Instant}};

#[derive(Parser)]
#[command(about = "Replay 100 ksps CI16 with 512 FPGA bin streams and acknowledged pin/stop controls. IQ must already be mixed to --slice-center. No PSD is fabricated. Exit 0 success, 1 failure, 2 usage.")]
struct Args {
    input: PathBuf,
    #[arg(long, default_value = "127.0.0.1:18811")]
    ingest: String,
    #[arg(long, default_value_t = 454216998)]
    center: i64,
    #[arg(long, default_value_t = 509)]
    stream: u32,
    #[arg(long, default_value_t = 1)]
    loops: u32,
    #[arg(long, default_value_t = 2.)]
    quiet_seconds: f64,
    /// Optional producer-captured 512-float PSD rows, low to high frequency. Replayed
    /// as recorded evidence; no host spectrum estimator is used.
    #[arg(long)]
    psd: Option<PathBuf>,
}
fn bin_center(center: i64, id: u32) -> i64 {
    let k = id as i64 - 1;
    center + if k < 256 { k } else { k - 512 } * 50_000
}
fn config(args: &Args, pinned: bool, stopped: bool, ack: Option<Value>) -> Value {
    let streams: Vec<Value> = if stopped { vec![] } else {
        (1..=512).map(|id| json!({"stream_id":id,"center_hz":bin_center(args.center,id),
            "target_hz":bin_center(args.center,id),"sample_rate_num_hz":25600000,"sample_rate_den":256,
            "protocol":"Unknown","pinned":pinned && id==args.stream,"iq_forwarded":pinned && id==args.stream})).collect()
    };
    let mut cfg = json!({"input_center_hz":args.center,"input_rate_hz":25600000,"input_bandwidth_hz":20000000,
        "receiver_state":if stopped {"stopped"} else {"running"},"producer":"recorded-fpga-fixture",
        "hardware_counter":false,"continuity_verified":false,"activity_calibrated":false,
        "capabilities":{"controls_supported":true,"configure_supported":false,"pin_supported":true,"stop_supported":true},
        "psd":{"bins":512,"center_hz":args.center,"span_hz":25600000,"unit":"dBFS"},"streams":streams});
    if let Some(ack) = ack { cfg["control_ack"] = ack; }
    cfg
}
fn main() -> Result<()> {
    let args = Args::parse();
    anyhow::ensure!((1..=512).contains(&args.stream), "stream must be 1..512");
    anyhow::ensure!(args.quiet_seconds.is_finite() && args.quiet_seconds >= 0., "quiet-seconds must be finite and nonnegative");
    let mut iq = Vec::new();
    std::fs::File::open(&args.input)?.read_to_end(&mut iq)?;
    anyhow::ensure!(!iq.is_empty() && iq.len()%4==0, "input must contain whole CI16 samples");
    let psd = args.psd.as_ref().map(std::fs::read).transpose()?;
    if let Some(rows) = &psd { anyhow::ensure!(!rows.is_empty() && rows.len()%2048==0, "PSD must contain complete 512-float rows"); }
    let mut output = TcpStream::connect(&args.ingest)?;
    output.set_nodelay(true)?;
    let mut input = output.try_clone()?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || { while let Ok(frame) = Frame::read(&mut input) { if frame.kind == 6 && let Ok(v) = serde_json::from_slice::<Value>(&frame.payload) { if tx.send(v).is_err() { break; } } } });
    let mut sequences = HashMap::<u32,u64>::new();
    let mut send = |kind, stream, first_sample, payload: Vec<u8>| -> Result<()> {
        let seq = sequences.entry(stream).or_default();
        Frame {kind,flags:0,generation:1,stream_id:stream,sequence:*seq,first_sample,
            rate:100000,center_hz:if stream==0 {args.center} else {bin_center(args.center,stream)},
            format:match kind {1=>1,2=>2,_=>3},valid_bits:if kind==1 {16} else {0},payload}.write(&mut output)?;
        *seq+=1; Ok(())
    };
    let mut pinned = true;
    send(4,0,0,serde_json::to_vec(&config(&args,pinned,false,None))?)?;
    let quiet_packets = (args.quiet_seconds * 100.).round() as usize;
    let data_packets = iq.len().div_ceil(4000);
    let start = Instant::now();
    let mut sample = 0u64;
    let mut packet = 0usize;
    loop {
        for request in rx.try_iter() {
            let action = request["action"].as_str().unwrap_or("");
            let ok = match action {
                "get_config"|"stop" => true,
                "pin" if request["stream_id"] == args.stream => { pinned = request["pinned"] == true; true },
                "pin" => !request["pinned"].as_bool().unwrap_or(true),
                _=>false,
            };
            let stopped = action=="stop";
            let ack=json!({"request_id":request["request_id"],"ok":ok,"error":if ok {""} else {"Fixture only contains one recorded slice"}});
            send(4,0,0,serde_json::to_vec(&config(&args,pinned,stopped,Some(ack)))?)?;
            if stopped { println!("{}",json!({"status":"stopped","samples":sample}));return Ok(()); }
        }
        let cycle = data_packets + quiet_packets;
        let position = packet % cycle;
        let quiet = position >= data_packets;
        if packet%10==0 {
            send(3,args.stream,sample,serde_json::to_vec(&json!({"stream_id":args.stream,"rf_active":!quiet,"level_db":if quiet {-95.} else {-35.},"floor_db":-95.}))?)?;
            if let Some(rows) = &psd {
                let row=(packet/10)%(rows.len()/2048);
                send(2,0,sample,rows[row*2048..(row+1)*2048].to_vec())?;
            }
        }
        let payload = if quiet { vec![0;4000] } else {iq[position*4000..((position+1)*4000).min(iq.len())].to_vec()};
        let count=payload.len()/4;
        if pinned {send(1,args.stream,sample,payload)?;}
        sample+=count as u64;packet+=1;
        thread::sleep(Duration::from_secs_f64((sample as f64/100000.-start.elapsed().as_secs_f64()).max(0.)));
        if args.loops>0 && packet>=cycle*args.loops as usize {break;}
    }
    println!("{}",json!({"status":"complete","samples":sample,"seconds":sample as f64/100000.}));
    Ok(())
}
