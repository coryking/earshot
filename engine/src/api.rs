use crate::runtime::{App, retain_host_targets, sync_pins};
use crate::store::AudioSource;
use axum::{
    Json, Router,
    extract::{
        Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderValue, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::broadcast;
/// A channel entry with the operator's edits (name, followed, muted, notes) attached.
fn with_user(c: &Value, user: &std::collections::BTreeMap<String, Value>) -> Value {
    let mut out = c.clone();
    // Edits are stored under the catalog source id (frequency and slot), not the channel id.
    let source = c["id"].as_str().map(|id| source_id(id, c["slot"].as_u64().unwrap_or(1)));
    let edits = source.as_deref().and_then(|id| user.get(id));
    out["user"] = edits.cloned().unwrap_or(Value::Null);
    out["display_name"] = edits
        .and_then(|u| u["name"].as_str())
        .map(|n| json!(n))
        .unwrap_or_else(|| c["label"].clone());
    out
}
fn now_s() -> f64 {
    crate::link::now()
}
/// The browser validates this payload (web/src/api/engine.ts), so every numeric
/// field must be a number even before the first CONFIG or sample arrives.
pub(crate) fn status_json(app: &App) -> Value {
    let receiver = app.receiver_status();
    let e = app.engine();
    let number = |v: &Value| json!(v.as_f64().unwrap_or(0.));
    let position = number(&e.source["source_position_s"]);
    // Host ingress age of the latest delivered audio while audio is flowing (the last
    // chunk was under 5 s ago); otherwise 0, not a stale number.
    let flowing = e.listener["last_audio_at"]
        .as_f64()
        .is_some_and(|t| now_s() - t < 5.);
    let latency = if flowing {
        e.listener["ingress_age_ms"].as_f64().unwrap_or(0.) / 1000.
    } else {
        0.
    };
    // The engine adds what it can do on top of what the producer advertises.
    let mut source = e.source.clone();
    if source.is_object() {
        if !source["capabilities"].is_object() {
            source["capabilities"] = json!({});
        }
        source["capabilities"]["fm_audio"] = json!(true);
        source["capabilities"]["raw_fm_audio"] = json!(true);
        source["capabilities"]["per_client_listening"] = json!(true);
    }
    json!({"mode":if e.active{"live"}else{"disconnected"},"state":if e.active{"receiving"}else if e.error.is_some(){"error"}else{"stopped"},"source_name":"Pluto channelized RIQ1","center_hz":json!(app.calibration.true_hz(e.source["input_center_hz"].as_f64().unwrap_or(0.))),"center_reading_hz":number(&e.source["input_center_hz"]),"rate_hz":number(&e.source["input_rate_hz"]),"bandwidth_hz":number(&e.source["input_bandwidth_hz"]),"generation":e.generation,"channels":e.channels.values().map(|c| with_user(c, &e.store.user)).collect::<Vec<_>>(),"received_seconds":position,"source_position_s":position,"decode_latency_s":latency,"dropped_batches":e.drops,"processed_batches":e.frames,"queue_depth":number(&e.source["queue_depth"]),"error":e.error,"decoder_error":null,"started_at":e.started,"last_samples_at":e.source["last_samples_at"],"continuity":"transport sequence checked; hardware continuity unknown","decoder_capacity":app.max_decoders,"calibration_ppm":app.calibration.ppm,"labels_loaded":app.labels.len(),"voice":{"vocoder":app.voice.vocoder.name(),"post":post_json(&app.voice.post)},"listeners":e.listeners.len(),"persistence":{"enabled":e.store.persistent(),"write_errors":e.store.io_errors()},"link":e.link.to_json(now_s()),"receiver":receiver,"source":source})
}
/// Voice post-processing profile as shown in /api/status.
fn post_json(c: &crate::voice::VoiceConfig) -> Value {
    json!({"enabled":c.enabled,"highpass_hz":c.highpass_hz,"presence_hz":c.presence_hz,"presence_db":c.presence_db,
        "presence_q":c.presence_q,"target_dbfs":c.target_dbfs,"initial_gain_db":c.initial_gain_db,"gate_below_db":c.gate_below_db,"gate_floor_db":c.gate_floor_db})
}
async fn status(State(app): State<App>) -> Json<Value> {
    Json(status_json(&app))
}
#[derive(Deserialize)]
struct Monitor {
    channel_id: Option<String>,
}
async fn monitor(
    State(app): State<App>,
    Json(m): Json<Monitor>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut e = app.engine();
    if let Some(ref id) = m.channel_id {
        if !e.channels.values().any(|c| c["id"] == *id) {
            return Err((StatusCode::NOT_FOUND, "channel not in catalog".into()));
        }
    }
    if e.selected == m.channel_id {
        return Ok(Json(e.listener.clone()));
    }
    e.selected = m.channel_id;
    retain_host_targets(&mut e, None);
    e.selected_at = Instant::now();
    e.decoder_generation += 1;
    sync_pins(&mut e);
    e.listener["channel_id"] = json!(e.selected);
    e.listener["voice_active"] = json!(false);
    e.listener["status"] = json!(if e.selected.is_some() {
        "waiting"
    } else {
        "stopped"
    });
    e.listener["state"] = e.listener["status"].clone();
    Ok(Json(e.listener.clone()))
}
async fn listener(State(app): State<App>) -> Json<Value> {
    let e = app.engine();
    let mut value = e.listener.clone();
    value["generation"] = json!(e.decoder_generation);
    value["source_generation"] = json!(e.generation);
    value["queue_blocks"] = e.source["queue_depth"].clone();
    value["dropped_blocks"] = json!(e.drops);
    value["input_dropped"] = json!(e.drops);
    Json(value)
}
/// Operator tuning uses true Hz, unlike the producer's radio-reading CONFIG frame.
async fn tune(State(app): State<App>, Json(request): Json<Value>) -> ApiResult {
    let frequency = request["frequency_hz"].as_f64().filter(|v| v.is_finite() && (70e6..=6e9).contains(v))
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "frequency_hz must be a receive frequency in true Hz"))?;
    let reading = (frequency * (1. + app.calibration.ppm * 1e-6)).round() as u64;
    let mut e = app.engine();
    if !e.active {
        return Err(api_error(StatusCode::SERVICE_UNAVAILABLE, "Receiver is disconnected or stopped"));
    }
    let raw = request["raw"].as_bool().unwrap_or(false);
    let requested = if raw { None } else { request["channel_id"].as_str() };
    if requested.is_some_and(|id| !e.channels.contains_key(id)) {
        return Err(api_error(StatusCode::NOT_FOUND, "Channel is no longer in the catalog"));
    }
    let cfg = crate::runtime::target_slice(&e.source, reading as f64)
        .ok_or_else(|| api_error(StatusCode::UNPROCESSABLE_ENTITY, "This frequency is outside the receiver's current tuning range"))?;
    let display = app.display_frequency(reading as f64).0;
    e.host_targets.retain(|old| app.display_frequency(*old as f64).0 != display);
    e.host_targets.insert(reading);
    let stream = cfg["stream_id"].as_u64().unwrap() as u32;
    let channel = crate::runtime::make_channel(&app, stream, reading as f64, if raw { "Raw" } else { "Unknown" }, 1);
    let id = channel["id"].as_str().unwrap().to_string();
    let entry = e.channels.entry(id.clone()).or_insert(channel);
    entry["stream_id"] = json!(stream);
    entry["logical_hz"] = json!(reading);
    entry["reading_hz"] = json!(reading);
    entry["in_range"] = json!(true);
    // Identified siblings use the same nominal channel identity. Retarget them
    // together so an explicit DMR-slot listener cannot keep the replaced offset.
    for c in e.channels.values_mut() {
        if c["logical_hz"].as_f64().is_some_and(|r| app.display_frequency(r).0 == display) {
            c["logical_hz"] = json!(reading);
            c["reading_hz"] = json!(reading);
            c["stream_id"] = json!(stream);
            c["in_range"] = json!(true);
        }
    }
    retain_host_targets(&mut e, Some(reading));
    // A saved channel has no live logical_hz after restart. Reattach the selected
    // catalog identity to its frequency while preserving its protocol and DMR slot.
    let selected_id = requested.unwrap_or(&id).to_string();
    if let Some(selected) = e.channels.get_mut(&selected_id) {
        selected["logical_hz"] = json!(reading);
        selected["reading_hz"] = json!(reading);
        selected["stream_id"] = json!(stream);
        selected["in_range"] = json!(true);
    }
    let channel = e.channels[&selected_id].clone();
    drop(e);
    let _ = app.events.send(json!({"type":"channel","channel":channel}));
    Ok(Json(channel))
}

fn spectrum_geometry(source: &Value, calibration: crate::freq::Calibration) -> (f64, f64) {
    let center = source["psd"]["center_hz"].as_f64().or_else(|| source["input_center_hz"].as_f64()).unwrap_or(0.);
    let span = source["psd"]["span_hz"].as_f64().or_else(|| source["input_bandwidth_hz"].as_f64()).unwrap_or(0.);
    (calibration.true_hz(center), calibration.true_hz(span))
}
#[derive(Deserialize)]
struct After {
    #[serde(default)]
    after: u64,
}
/// Display range for the waterfall. A producer that reports power per Hz is shown on
/// the documented fixed scale; integrated per-bin dBFS has no fixed floor, so the range
/// follows the retained rows (5th percentile to 99.9th percentile with a minimum span).
fn display_range(unit: &str, rows: &std::collections::VecDeque<(u64, f64, Vec<f32>)>) -> (f64, f64) {
    // Only the newest rows: bounded work under the engine lock, and the scale tracks now.
    let mut values: Vec<f32> = rows.iter().rev().take(30).flat_map(|r| r.2.iter().copied()).collect();
    if unit == "dBFS/Hz" || values.len() < 16 {
        return (-125., -65.);
    }
    values.sort_by(f32::total_cmp);
    let at = |q: f64| values[((values.len() - 1) as f64 * q) as usize] as f64;
    let low = at(0.05) - 2.;
    let high = (at(0.999) + 2.).max(low + 30.);
    (low, high)
}
/// The display range handed to clients, held steady between responses: it only moves when
/// the fitted range differs from the held one by 3 dB or more, so quantized rows from
/// consecutive batches share one scale.
pub(crate) fn held_range(e: &mut crate::runtime::Engine, unit: &str) -> (f64, f64) {
    let fitted = display_range(unit, &e.psd);
    match e.psd_range {
        Some((low, high)) if (fitted.0 - low).abs() < 3. && (fitted.1 - high).abs() < 3. => (low, high),
        _ => {
            e.psd_range = Some(fitted);
            fitted
        }
    }
}
async fn spectrum(State(app): State<App>, Query(q): Query<After>) -> Json<Value> {
    let mut e = app.engine();
    let unit = e.source["psd"]["unit"].as_str().unwrap_or("uncalibrated").to_string();
    let (low_db, high_db) = held_range(&mut e, &unit);
    let unit = unit.as_str();
    let rows: Vec<_> = e
        .psd
        .iter()
        .filter(|(n, _, _)| *n > q.after)
        .take(60)
        .collect();
    Json(
        json!({"sequence":rows.last().map(|x|x.0).unwrap_or(e.psd_seq),"generation":e.generation,"rows":rows.iter().map(|x|&x.2).collect::<Vec<_>>(),"times":rows.iter().map(|x|x.1).collect::<Vec<_>>(),"center_hz":spectrum_geometry(&e.source, app.calibration).0,"center_reading_hz":e.source["psd"]["center_hz"].as_f64().or_else(|| e.source["input_center_hz"].as_f64()),"span_hz":spectrum_geometry(&e.source, app.calibration).1,"low_db":low_db,"high_db":high_db,"unit":unit,"source":if e.active{"live"}else{"disconnected"}}),
    )
}
#[derive(Deserialize)]
struct Listen {
    channel_id: Option<String>,
}
/// Audio for one browser. With `?channel_id=` this socket listens to that channel on its
/// own (several browsers can listen to different channels at once); without it the socket
/// follows the shared `/api/monitor` selection.
const MAX_LISTENERS: usize = 16;
async fn ws(State(app): State<App>, Query(q): Query<Listen>, ws: WebSocketUpgrade) -> axum::response::Response {
    let requested = q.channel_id.filter(|id| !id.is_empty());
    if let Some(id) = &requested {
        let e = app.engine();
        if !e.channels.contains_key(id) {
            // 422, not 404: the frontend reads 404 on its paths as "not built yet".
            return api_error(StatusCode::UNPROCESSABLE_ENTITY, format!("unknown channel {id}")).into_response();
        }
    }
    // Reserve the listener slot now, under the same lock as the cap check, so parallel
    // handshakes cannot exceed it. The guard releases it if the upgrade never completes.
    let guard = requested.as_ref().map(|id| {
        let connection = NEXT_CONNECTION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut e = app.engine();
        if e.listeners.len() >= MAX_LISTENERS {
            return None;
        }
        e.listeners.insert(connection, id.clone());
        retain_host_targets(&mut e, None);
        sync_pins(&mut e);
        Some(ListenerGuard { app: app.clone(), connection })
    });
    if matches!(guard, Some(None)) {
        return api_error(StatusCode::TOO_MANY_REQUESTS, "too many listeners").into_response();
    }
    let guard = guard.flatten();
    ws.max_message_size(8192)
        .on_upgrade(move |s| socket(s, app, requested, guard))
        .into_response()
}
static NEXT_CONNECTION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
/// Registers a per-socket listener and removes it (and its pin) when the socket ends.
struct ListenerGuard {
    app: App,
    connection: u64,
}
impl Drop for ListenerGuard {
    fn drop(&mut self) {
        let mut e = self.app.engine();
        e.listeners.remove(&self.connection);
        retain_host_targets(&mut e, None);
        sync_pins(&mut e);
    }
}
async fn socket(mut socket: WebSocket, app: App, requested: Option<String>, _guard: Option<ListenerGuard>) {
    let joined = Instant::now();
    let mut rx = app.audio.subscribe();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
                audio=rx.recv()=>{match audio{Ok(a)=>{
                    let current={let e=app.engine();
                        let want=requested.clone().or_else(||e.selected.clone());
                        // Never play audio made before this listener (or the shared selection) began.
                        let since=if requested.is_some(){joined}else{e.selected_at};
                        e.active&&a.meta["source_generation"]==e.generation&&a.meta["channel_id"].as_str()==want.as_deref()&&a.created>=since};
                    if current {
        if a.created.elapsed()>Duration::from_millis(250){if socket.send(Message::Text(json!({"type":"gap","reason":"audio_expired"}).to_string().into())).await.is_err(){break;}continue;}
        let sent=tokio::time::timeout(Duration::from_millis(250),async{socket.send(Message::Text(a.meta.to_string().into())).await?;socket.send(Message::Binary(a.pcm.into())).await}).await;
        if !matches!(sent,Ok(Ok(()))){break;}
        }},Err(broadcast::error::RecvError::Lagged(n))=>{rx=app.audio.subscribe();if socket.send(Message::Text(json!({"type":"gap","dropped_chunks":n}).to_string().into())).await.is_err(){break;}},Err(_)=>break}},
                _=tick.tick()=>{let value={let e=app.engine();json!({"type":"heartbeat","generation":e.decoder_generation,"source_generation":e.generation,"channel_id":requested.clone().or_else(||e.selected.clone()),"listeners":e.listeners.len()})};if !matches!(tokio::time::timeout(Duration::from_millis(250),socket.send(Message::Text(value.to_string().into()))).await,Ok(Ok(()))){break;}},
                msg=socket.recv()=>{if matches!(msg,None|Some(Err(_))|Some(Ok(Message::Close(_)))){break;}}
               }
    }
}
async fn event_ws(State(app): State<App>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.max_message_size(8192)
        .on_upgrade(move |socket| event_socket(socket, app))
}
async fn event_socket(mut socket: WebSocket, app: App) {
    let mut events = app.events.subscribe();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        let outgoing = tokio::select! {
        event=events.recv()=>{match event{Ok(value)=>value,Err(broadcast::error::RecvError::Lagged(n))=>{events=app.events.subscribe();json!({"type":"gap","dropped_events":n,"recover":"GET /api/status and /api/spectrum"})},Err(_)=>break}},
        _=tick.tick()=>{let e=app.engine();json!({"type":"heartbeat","generation":e.decoder_generation,"source_generation":e.generation})},
        input=socket.recv()=>{if matches!(input,None|Some(Err(_))|Some(Ok(Message::Close(_)))){break;}continue;}
        };
        if !matches!(
            tokio::time::timeout(
                Duration::from_millis(250),
                socket.send(Message::Text(outgoing.to_string().into()))
            )
            .await,
            Ok(Ok(()))
        ) {
            break;
        }
    }
}
type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;
fn api_error(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"detail":message.into()})))
}
async fn source_request(app: &App, mut command: Value) -> ApiResult {
    let request_id = format!(
        "host-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    command["request_id"] = json!(request_id);
    let sender = {
        let e = app.engine();
        let capability = match command["action"].as_str() {
            Some("configure") => "configure_supported",
            Some("stop") => "stop_supported",
            _ => "controls_supported",
        };
        if !e.source["capabilities"][capability]
            .as_bool()
            .unwrap_or(e.source["capabilities"]["controls_supported"] == true)
        {
            return Err(api_error(
                StatusCode::NOT_IMPLEMENTED,
                "Source producer does not advertise acknowledged controls; configure the producer and wait for actual CONFIG readback.",
            ));
        }
        e.control.clone().ok_or_else(|| {
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Source control connection unavailable",
            )
        })?
    };
    sender.try_send(command).map_err(|_| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Source control queue full or disconnected",
        )
    })?;
    let started = Instant::now();
    loop {
        {
            let e = app.engine();
            let ack = &e.source["control_ack"];
            if ack["request_id"] == request_id {
                if ack["ok"] != true {
                    return Err(api_error(
                        StatusCode::BAD_GATEWAY,
                        ack["error"]
                            .as_str()
                            .unwrap_or("Source rejected control request"),
                    ));
                }
                return Ok(Json(
                    json!({"status":e.source["receiver_state"],"source":e.source,"generation":e.generation}),
                ));
            }
        }
        if started.elapsed() > Duration::from_secs(5) {
            return Err(api_error(
                StatusCode::GATEWAY_TIMEOUT,
                "Source control acknowledgement timed out; applied hardware state is unknown. Check the producer before retrying.",
            ));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
async fn configure_source(State(app): State<App>, Json(config): Json<Value>) -> ApiResult {
    if config["mode"] != "live" {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Only live sources are supported",
        ));
    }
    let rate = config["rate_hz"]
        .as_u64()
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "rate_hz must be an integer"))?;
    let center = config["center_hz"]
        .as_u64()
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "center_hz must be an integer"))?;
    let bandwidth = config["bandwidth_hz"].as_u64().unwrap_or(rate);
    {
        let e = app.engine();
        if let Some(max) = e.source["capabilities"]["max_input_rate_hz"].as_u64()
            && rate > max
        {
            return Err(api_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("Producer capacity is {max} samples/s; requested {rate}"),
            ));
        }
    }
    if rate == 0 || bandwidth == 0 || !(70000000..=6000000000).contains(&center) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Invalid receive rate, bandwidth or frequency",
        ));
    }
    source_request(&app,json!({"action":"configure","input_center_hz":center,"input_rate_hz":rate,"input_bandwidth_hz":bandwidth})).await
}
async fn stop(State(app): State<App>) -> ApiResult {
    let supported = {
        let mut e = app.engine();
        e.active = false;
        e.decoder_generation += 1;
        e.selected = None;
        e.host_targets.clear();
        sync_pins(&mut e);
        e.listener["status"] = json!("stopped");
        e.listener["channel_id"] = Value::Null;
        e.listener["state"] = json!("stopped");
        e.listener["voice_active"] = json!(false);
        e.source["capabilities"]["stop_supported"]
            .as_bool()
            .unwrap_or(e.source["capabilities"]["controls_supported"] == true)
    };
    if supported {
        source_request(&app, json!({"action":"stop"})).await
    } else {
        Ok(Json(
            json!({"status":"stopped","device_control":"Host audio stopped; source producer does not advertise stop/restore acknowledgement."}),
        ))
    }
}

#[derive(Deserialize)]
struct CallsQuery {
    /// Unix seconds; only calls that started before this.
    before: Option<f64>,
    limit: Option<usize>,
    source: Option<String>,
}
/// Recent calls, newest first. `next_before` is the cursor (unix seconds) for the next, older
/// page, null on the last page.
async fn calls(State(app): State<App>, Query(q): Query<CallsQuery>) -> Json<Value> {
    let e = app.engine();
    let before = q.before.filter(|b| b.is_finite());
    let (calls, next) = e.store.list(before, q.limit.unwrap_or(50).clamp(1, 200), q.source.as_deref());
    Json(json!({"calls":calls,"next_before":next,"in_progress":e.store.open_calls()}))
}
async fn call_audio(State(app): State<App>, Path(id): Path<String>) -> axum::response::Response {
    // Decide where the audio lives under the lock; read and encode outside it.
    let source = id.parse::<u64>().ok().and_then(|n| app.engine().store.audio_source(n));
    let bytes = match source {
        Some(AudioSource::Memory(samples)) => Some(crate::store::wav_bytes(&samples)),
        Some(AudioSource::File(path)) => tokio::task::spawn_blocking(move || std::fs::read(path).ok())
            .await
            .ok()
            .flatten(),
        None => None,
    };
    match bytes {
        Some(bytes) => (
            [
                (header::CONTENT_TYPE, HeaderValue::from_static("audio/wav")),
                (header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=3600")),
            ],
            bytes,
        )
            .into_response(),
        // 410, not 404: the frontend reads 404 as "endpoint not built yet".
        None => api_error(StatusCode::GONE, "no audio for that call").into_response(),
    }
}
/// Catalog source id for a channel entry: the calibrated nominal frequency and slot, so it
/// stays the same when the engine identifies the protocol (`...-Unknown-1` to `...-DMR-2`).
pub(crate) fn source_id(channel_id: &str, slot: u64) -> String {
    format!("{}-{}", channel_id.split('-').next().unwrap_or(channel_id), slot)
}
fn mode_of(c: &Value) -> &'static str {
    match c["protocol"].as_str().unwrap_or("Unknown") {
        "FM" => "fm",
        "DMR" => "dmr",
        "NXDN48" => "nxdn",
        _ => match c["label_detail"]["protocol"].as_str() {
            Some("DMR") => "dmr",
            Some("NXDN48") => "nxdn",
            Some("P25") => "p25",
            _ => "unknown",
        },
    }
}
/// One catalog entry (the shape the household UI reads) from the channel entries sharing a
/// source id, with operator edits applied.
fn catalog_entry(id: &str, group: &[&Value], user: Option<&Value>) -> Value {
    let rep = group
        .iter()
        .max_by(|a, b| {
            let key = |c: &&Value| (c["decoded"] == true, c["last_heard"].as_f64().unwrap_or(0.) as i64);
            key(a).cmp(&key(b))
        })
        .copied();
    let text = |key: &str| user.and_then(|u| u[key].as_str()).map(String::from);
    let frequency = rep
        .and_then(|c| c["frequency_hz"].as_f64())
        .or_else(|| user.and_then(|u| u["frequency_hz"].as_f64()));
    let label_name = rep.and_then(|c| c["label"].as_str()).map(String::from);
    let detail = rep.map(|c| &c["label_detail"]);
    let licensees: Vec<&str> = detail
        .and_then(|d| d["licensees"].as_array())
        .map(|l| l.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let description = text("description").unwrap_or_else(|| match (label_name.as_deref(), licensees.len()) {
        // A label with no licensee says only what kind of channel it is; traffic_kind carries
        // that, so the description stays empty rather than repeating it with source attribution.
        (Some(_), 0) => String::new(),
        (Some(_), n) => format!("Licensed to {}{}", licensees.iter().take(2).copied().collect::<Vec<_>>().join(", "), if n > 2 { " and others" } else { "" }),
        _ => String::new(),
    });
    json!({
        "id": id,
        "kind": "channel",
        "channel_ids": group.iter().filter_map(|c| c["id"].as_str()).collect::<Vec<_>>(),
        "frequency_hz": frequency,
        "slot": rep.and_then(|c| c["slot"].as_u64()).or_else(|| user.and_then(|u| u["slot"].as_u64())),
        "system": detail.and_then(|d| d["system"].as_str()).map(|s| json!({"id": s, "name": label_name})),
        "talkgroup_id": null,
        "name": text("name").or(label_name.clone()).unwrap_or_else(|| frequency.map_or_else(|| id.to_string(), |f| format!("{:.4} MHz", f / 1e6))),
        "description": description,
        "probable_user": text("probable_user").or_else(|| licensees.first().map(|s| s.to_string())).or(label_name),
        "traffic_kind": text("traffic_kind").or_else(|| detail.and_then(|d| d["kind"].as_str()).map(|k| match k {
            "control" => "control channel (data)".to_string(),
            "voice" => "voice".to_string(),
            other => format!("{other} channel"),
        })),
        "mode": rep.map_or("unknown", mode_of),
        "encryption": "unknown",
        "followed": user.and_then(|u| u["followed"].as_bool()).unwrap_or(false),
        "muted": user.and_then(|u| u["muted"].as_bool()).unwrap_or(false),
        "notes": user.and_then(|u| u["notes"].as_str()),
        "last_heard": rep.map(|c| c["last_heard"].clone()),
        "edited": user.is_some(),
    })
}
/// The catalog: one source per frequency and slot that was located, decoded or edited.
async fn catalog(State(app): State<App>) -> Json<Value> {
    let e = app.engine();
    let mut groups: std::collections::BTreeMap<String, Vec<&Value>> = std::collections::BTreeMap::new();
    for c in e.channels.values() {
        let located = c["decoded"] == true || c["carrier_snapped"] == true;
        let id = source_id(c["id"].as_str().unwrap_or(""), c["slot"].as_u64().unwrap_or(1));
        if located || e.store.user.contains_key(&id) {
            groups.entry(id).or_default().push(c);
        }
    }
    for id in e.store.user.keys() {
        groups.entry(id.clone()).or_default();
    }
    let mut sources: Vec<Value> = groups
        .iter()
        .map(|(id, group)| catalog_entry(id, group, e.store.user.get(id)))
        .collect();
    sources.sort_by(|a, b| {
        let key = |v: &Value| v["last_heard"].as_f64().unwrap_or(0.);
        key(b).total_cmp(&key(a))
    });
    Json(json!({"sources":sources,"count":sources.len()}))
}
/// Upsert the operator's edits for a source. The body may be the whole catalog entry the
/// UI read: read-only fields (id, kind, channel_ids, system, mode, ...) are ignored, the
/// editable ones (name, description, probable_user, traffic_kind, notes, followed, muted)
/// are validated, and frequency_hz/slot are remembered so a source that has not been heard
/// yet still lists.
async fn catalog_edit(State(app): State<App>, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult {
    let valid_id = (1..=80).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    if !valid_id {
        return Err(api_error(StatusCode::UNPROCESSABLE_ENTITY, "source id must be 1-80 characters of letters, digits, - _ ."));
    }
    let object = body.as_object().ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "body must be an object"))?;
    let mut e = app.engine();
    if !e.store.can_add_user(&id) {
        return Err(api_error(StatusCode::UNPROCESSABLE_ENTITY, "too many edited sources"));
    }
    let mut edits = match e.store.user.get(&id) {
        Some(Value::Object(o)) => Value::Object(o.clone()),
        _ => json!({}),
    };
    let limits = [("name", 80usize), ("description", 300), ("probable_user", 80), ("traffic_kind", 80), ("notes", 500)];
    for (key, value) in object {
        if let Some((_, max)) = limits.iter().find(|(k, _)| *k == key.as_str()) {
            match value {
                Value::String(text) if text.chars().count() <= *max => {
                    let text = text.trim();
                    if text.is_empty() {
                        edits.as_object_mut().map(|o| o.remove(key));
                    } else {
                        edits[key] = json!(text);
                    }
                }
                Value::Null => {
                    edits.as_object_mut().map(|o| o.remove(key));
                }
                _ => return Err(api_error(StatusCode::BAD_REQUEST, format!("{key} must be a string of at most {max} characters, or null"))),
            }
        } else if matches!(key.as_str(), "followed" | "muted") {
            match value {
                Value::Bool(b) => edits[key] = json!(b),
                Value::Null => {
                    edits.as_object_mut().map(|o| o.remove(key));
                }
                _ => return Err(api_error(StatusCode::BAD_REQUEST, format!("{key} must be a boolean"))),
            }
        } else if key == "frequency_hz" {
            if let Some(n) = value.as_f64().filter(|n| n.is_finite()) {
                edits[key] = json!(n);
            }
        } else if key == "slot" {
            // Integral slots only (the UI may send 1.0).
            if let Some(n) = value.as_f64().filter(|n| n.is_finite() && n.fract() == 0. && (0. ..=255.).contains(n)) {
                edits[key] = json!(n as u64);
            }
        }
        // Anything else is a read-only field of the entry the UI read: ignored.
    }
    // Nothing left to remember: drop the key instead of keeping an empty edit around.
    if edits.as_object().is_none_or(|o| o.is_empty()) {
        e.store.user.remove(&id);
    } else {
        e.store.user.insert(id.clone(), edits);
    }
    e.store.user_dirty = true;
    let group: Vec<&Value> = e
        .channels
        .values()
        .filter(|c| source_id(c["id"].as_str().unwrap_or(""), c["slot"].as_u64().unwrap_or(1)) == id)
        .collect();
    Ok(Json(catalog_entry(&id, &group, e.store.user.get(&id))))
}
#[derive(Deserialize)]
struct ActivityQuery {
    window_s: Option<u64>,
    bucket_s: Option<u64>,
}
/// Talk-seconds per bucket (block total and per source) with a same-time baseline.
async fn activity(State(app): State<App>, Query(q): Query<ActivityQuery>) -> Json<Value> {
    let e = app.engine();
    Json(e.store.activity(now_s(), q.window_s.unwrap_or(86400), q.bucket_s.unwrap_or(300)))
}
#[derive(Deserialize)]
struct SpectrumBin {
    #[serde(default)]
    after: u64,
    limit: Option<usize>,
}
/// Compact spectrum rows, RSP1 (the layout the frontend decodes). 48-byte little-endian
/// header: magic `RSP1`, bins u16, rows u16, center_hz f64 (true, calibrated), span_hz f64,
/// low_db f32 (byte 0), high_db f32 (byte 255), latest sequence u64 (also with zero rows),
/// source generation u32, reserved u32. Then per row: seq u64, time f64 (producer
/// seconds), bins x u8 levels. Rows with seq > `after`, oldest first. The display range is
/// held steady between responses and only moves when the data moves it by 3 dB or more.
async fn spectrum_bin(State(app): State<App>, Query(q): Query<SpectrumBin>) -> impl IntoResponse {
    let mut e = app.engine();
    let unit = e.source["psd"]["unit"].as_str().unwrap_or("uncalibrated").to_string();
    let (low, high) = held_range(&mut e, &unit);
    let first_len = e.psd.iter().find(|(n, _, _)| *n > q.after).map_or(0, |r| r.2.len());
    // A reconfigure can change the row width: send only rows that match the first one.
    let rows: Vec<_> = e
        .psd
        .iter()
        .filter(|(n, _, row)| *n > q.after && row.len() == first_len)
        .take(q.limit.unwrap_or(60).clamp(1, 120))
        .collect();
    let bins = first_len.min(u16::MAX as usize);
    let mut out = Vec::with_capacity(48 + rows.len() * (16 + bins));
    out.extend_from_slice(b"RSP1");
    out.extend_from_slice(&(bins as u16).to_le_bytes());
    out.extend_from_slice(&(rows.len() as u16).to_le_bytes());
    let (center, span) = spectrum_geometry(&e.source, app.calibration);
    out.extend_from_slice(&center.to_le_bytes());
    out.extend_from_slice(&span.to_le_bytes());
    out.extend_from_slice(&(low as f32).to_le_bytes());
    out.extend_from_slice(&(high as f32).to_le_bytes());
    out.extend_from_slice(&e.psd_seq.to_le_bytes());
    out.extend_from_slice(&((e.generation & 0xffff_ffff) as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for (seq, time, row) in &rows {
        out.extend_from_slice(&seq.to_le_bytes());
        out.extend_from_slice(&time.to_le_bytes());
        out.extend(row.iter().take(bins).map(|v| {
            (255. * ((*v as f64).clamp(low, high) - low) / (high - low)).round() as u8
        }));
    }
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream")),
            (header::HeaderName::from_static("x-sequence"), HeaderValue::from(e.psd_seq)),
        ],
        out,
    )
}
/// Listening areas. The parked block is the only one this producer can offer; choosing a
/// different one needs a producer that retunes live (the ARM prototype cannot).
/// A plain name for the parked block: what it is and where it sits, e.g. "Business radio,
/// 453.8–454.6 MHz". Narrow blocks get one decimal so the two ends differ.
pub(crate) fn block_name(center_hz: f64, span_hz: f64) -> String {
    if center_hz <= 0. || span_hz <= 0. {
        return "Business radio".into();
    }
    let (low, high) = ((center_hz - span_hz / 2.) / 1e6, (center_hz + span_hz / 2.) / 1e6);
    let what = if (450. ..=470.).contains(&low) && (450. ..=470.).contains(&high) { "Business radio" } else { "Radio" };
    if span_hz < 2e6 {
        format!("{what}, {:.1}–{:.1} MHz", low, high)
    } else {
        format!("{what}, {:.0}–{:.0} MHz", low, high)
    }
}
async fn blocks(State(app): State<App>) -> Json<Value> {
    let e = app.engine();
    let center = app.calibration.true_hz(e.source["input_center_hz"].as_f64().unwrap_or(0.));
    let span = e.source["input_bandwidth_hz"].as_f64().unwrap_or(0.);
    Json(json!({
        "current_id": "current",
        "blocks": [{
            "id": "current",
            "name": block_name(center, span),
            "description": "The block of spectrum the radio is parked on now.",
            "center_hz": center,
            "span_hz": span,
        }],
        "can_change": e.source["capabilities"]["configure_supported"] == true,
        "calibration_ppm": app.calibration.ppm,
    }))
}
async fn set_block(State(app): State<App>, Json(body): Json<Value>) -> ApiResult {
    // Legacy form: a raw receiver configuration goes through the acknowledged source path.
    if body["mode"] == "live" {
        return configure_source(State(app), Json(body)).await;
    }
    match body["id"].as_str() {
        Some("current") => Ok(Json(json!({"status": "unchanged", "current_id": "current"}))),
        Some(other) => Err(api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("unknown listening area {other:?}; this receiver can only stay on its current block"),
        )),
        None => Err(api_error(StatusCode::BAD_REQUEST, "body needs an id")),
    }
}

pub fn router(app: App, web_dir: Option<PathBuf>) -> Router {
    let mut router = Router::new()
        .route("/api/status", get(status))
        .route("/api/listener", get(listener))
        .route("/api/monitor", post(monitor))
        .route("/api/tune", post(tune))
        .route("/api/stop", post(stop))
        .route("/api/source", post(configure_source))
        .route("/api/spectrum", get(spectrum))
        .route("/api/spectrum.bin", get(spectrum_bin))
        .route("/api/calls", get(calls))
        .route("/api/calls/{id}/audio.wav", get(call_audio))
        .route("/api/catalog", get(catalog))
        .route("/api/catalog/{id}", axum::routing::put(catalog_edit))
        .route("/api/activity", get(activity))
        .route("/api/blocks", get(blocks))
        .route("/api/blocks/current", post(set_block))
        .route("/api/listen", get(ws))
        .route("/api/events", get(event_ws))
        .with_state(app);
    if let Some(dir) = web_dir {
        router = router.fallback_service(tower_http::services::ServeDir::new(dir));
    }

    router
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Mirrors the required numeric fields of statusSchema in web/src/lib/radio.ts.
    #[test]
    fn status_satisfies_the_browser_schema_before_and_after_a_source_connects() {
        let required = [
            "center_hz",
            "rate_hz",
            "bandwidth_hz",
            "generation",
            "received_seconds",
            "source_position_s",
            "decode_latency_s",
            "dropped_batches",
            "processed_batches",
            "queue_depth",
        ];
        let app = App::new(3);
        for phase in 0..2 {
            let status = status_json(&app);
            for key in required {
                assert!(status[key].is_number(), "phase {phase}: {key} = {}", status[key]);
            }
            assert!(status["channels"].is_array() && status["continuity"].is_string());
            let mut e = app.engine();
            e.source = json!({"input_center_hz":454216998,"input_rate_hz":800000,"input_bandwidth_hz":800000,"queue_depth":0});
        }
    }
    #[test]
    fn waterfall_range_follows_integrated_dbfs_and_keeps_the_density_scale() {
        let rows: std::collections::VecDeque<_> = (0..20u64)
            .map(|n| (n, n as f64, (0..64).map(|k| if k == 5 { -12.0 } else { -60.0 + (k % 7) as f32 }).collect()))
            .collect();
        let (low, high) = display_range("dBFS", &rows);
        assert!(low < -58. && low > -66., "low {low}");
        assert!(high > -12. && high < -5., "high {high}");
        assert_eq!(display_range("dBFS/Hz", &rows), (-125., -65.));
    }
    fn seeded_app() -> App {
        let app = App::new(3);
        {
            let mut e = app.engine();
            let ch = json!({"id":"454018750-DMR-2","protocol":"DMR","slot":2,"kind":"voice","decoded":true,
                "frequency_hz":454018750.0,"label":"Day Wireless (business trunked)","last_heard":1.0,"carrier_snapped":true,
                "label_detail":{"name":"Day Wireless (business trunked)","kind":"control","system":"trbowest","protocol":"DMR","licensees":["Day Management"]}});
            e.channels.insert("454018750-DMR-2".into(), ch);
            let twin = json!({"id":"454018750-Unknown-1","protocol":"Unknown","slot":1,"kind":"unidentified","decoded":false});
            e.channels.insert("454018750-Unknown-1".into(), twin);
            let noise = json!({"id":"454054498-Unknown-1","protocol":"Unknown","slot":1,"kind":"unidentified","decoded":false});
            e.channels.insert("454054498-Unknown-1".into(), noise);
        }
        app
    }
    #[tokio::test]
    async fn calls_list_newest_first_page_by_unix_seconds_and_serve_wav_audio() {
        let app = seeded_app();
        {
            let mut e = app.engine();
            for (i, id) in ["454018750-DMR-2", "454018750-DMR-1"].iter().enumerate() {
                e.store.record(id, "454018750-2", "DMR", 2, 454018750., &json!("Day Wireless"), Some(-30.), &vec![500i16; 16000], 100. + i as f64 * 50.);
            }
            e.store.close_idle(1000.);
        }
        let Json(list) = calls(State(app.clone()), Query(CallsQuery { before: None, limit: Some(1), source: None })).await;
        let items = list["calls"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["channel_id"], "454018750-DMR-1", "newest first");
        assert_eq!(items[0]["source_id"], "454018750-2");
        assert!(items[0]["id"].is_string() && items[0]["ended_at"].is_number());
        assert_eq!(items[0]["duration_s"], 2.0);
        assert!(items[0]["talker"].is_null() && items[0]["transcript"].is_null());
        // The cursor is a unix timestamp that pages back to the older call.
        let cursor = list["next_before"].as_f64().expect("another page exists");
        let Json(page2) = calls(State(app.clone()), Query(CallsQuery { before: Some(cursor), limit: Some(10), source: None })).await;
        assert_eq!(page2["calls"][0]["channel_id"], "454018750-DMR-2");
        assert!(page2["next_before"].is_null());
        let url = items[0]["audio_url"].as_str().unwrap().to_string();
        let id = url.trim_start_matches("/api/calls/").trim_end_matches("/audio.wav").to_string();
        let response = call_audio(State(app.clone()), Path(id)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "audio/wav");
        let body = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        assert_eq!(&body[..4], b"RIFF");
        assert_eq!(body.len(), 44 + 32000);
        // 410, not 404 (404 means "endpoint not built" to the frontend).
        assert_eq!(call_audio(State(app), Path("99".into())).await.status(), StatusCode::GONE);
    }
    #[tokio::test]
    async fn catalog_groups_channels_into_sources_and_upserts_operator_edits() {
        let app = seeded_app();
        let Json(before) = catalog(State(app.clone())).await;
        let sources = before["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 1, "unconfirmed noise bins are not sources, and the Unknown twin joins its source");
        let entry = &sources[0];
        assert_eq!(entry["id"], "454018750-2");
        assert_eq!(entry["kind"], "channel");
        assert_eq!(entry["mode"], "dmr");
        assert_eq!(entry["encryption"], "unknown");
        assert_eq!(entry["name"], "Day Wireless (business trunked)");
        assert_eq!(entry["system"]["id"], "trbowest");
        assert_eq!(entry["channel_ids"], json!(["454018750-DMR-2"]));
        // The UI sends back the whole entry it read: read-only fields are ignored, edits apply.
        let mut upload = entry.clone();
        upload["name"] = json!("  Day Wireless dispatch ");
        upload["followed"] = json!(true);
        let edited = catalog_edit(State(app.clone()), Path("454018750-2".into()), Json(upload)).await.unwrap();
        assert_eq!(edited.0["name"], "Day Wireless dispatch");
        assert_eq!(edited.0["followed"], true);
        assert_eq!(edited.0["mode"], "dmr", "read-only fields are not editable");
        // Clearing a name falls back to the label; whitespace-only counts as cleared.
        let cleared = catalog_edit(State(app.clone()), Path("454018750-2".into()), Json(json!({"name":"   "}))).await.unwrap();
        assert_eq!(cleared.0["name"], "Day Wireless (business trunked)");
        // An edit for a source not heard yet is kept and listed.
        catalog_edit(State(app.clone()), Path("452000000-1".into()), Json(json!({"name":"Garage","frequency_hz":452000000.0,"slot":1}))).await.unwrap();
        let Json(after) = catalog(State(app.clone())).await;
        let garage = after["sources"].as_array().unwrap().iter().find(|s| s["id"] == "452000000-1").expect("user-only source listed");
        assert_eq!(garage["name"], "Garage");
        assert_eq!(garage["frequency_hz"], 452000000.0);
        assert_eq!(garage["slot"], 1, "the remembered slot reads back as an integer");
        let status = status_json(&app);
        assert!(status["channels"].as_array().unwrap().iter().any(|c| c["user"].is_object() || c["display_name"].is_string()));
    }
    #[tokio::test]
    async fn catalog_edit_validation_matrix() {
        let app = seeded_app();
        let put = |id: &'static str, body: Value| {
            let app = app.clone();
            async move { catalog_edit(State(app), Path(id.into()), Json(body)).await }
        };
        assert!(put("454018750-2", json!({"name":"x".repeat(80)})).await.is_ok());
        assert_eq!(put("454018750-2", json!({"name":"x".repeat(81)})).await.unwrap_err().0, StatusCode::BAD_REQUEST);
        assert!(put("454018750-2", json!({"notes":"n".repeat(500)})).await.is_ok());
        assert_eq!(put("454018750-2", json!({"notes":"n".repeat(501)})).await.unwrap_err().0, StatusCode::BAD_REQUEST);
        assert_eq!(put("454018750-2", json!({"followed":"yes"})).await.unwrap_err().0, StatusCode::BAD_REQUEST);
        assert_eq!(put("454018750-2", json!(["not","an","object"])).await.unwrap_err().0, StatusCode::BAD_REQUEST);
        assert_eq!(put("../etc/passwd", json!({"name":"x"})).await.unwrap_err().0, StatusCode::UNPROCESSABLE_ENTITY);
        // A bad later key leaves no partial commit.
        let before = app.engine().store.user.get("454018750-2").cloned();
        assert!(put("454018750-2", json!({"name":"changed","followed":123})).await.is_err());
        assert_eq!(app.engine().store.user.get("454018750-2").cloned(), before);
        assert!(put("454018750-2", json!({"name":null})).await.is_ok());
        // An edit that ends up empty leaves no entry behind (and cannot be used to fill the store).
        put("452111111-1", json!({})).await.unwrap();
        assert!(!app.engine().store.user.contains_key("452111111-1"));
        put("452111111-1", json!({"name":"n"})).await.unwrap();
        put("452111111-1", json!({"name":null})).await.unwrap();
        assert!(!app.engine().store.user.contains_key("452111111-1"), "clearing the last edit removes the entry");
        // A producer that sends capabilities as a non-object cannot panic /api/status.
        app.engine().source = json!({"capabilities": 7});
        assert_eq!(status_json(&app)["source"]["capabilities"]["fm_audio"], true);
        // A hand-damaged state file (an edit that is not an object) cannot panic the handler.
        app.engine().store.user.insert("454018750-2".into(), json!("garbage"));
        assert!(put("454018750-2", json!({"followed":true})).await.is_ok());
    }
    #[tokio::test]
    async fn spectrum_bin_is_rsp1_in_the_frontend_layout() {
        let app = App::new(3);
        {
            let mut e = app.engine();
            e.source = json!({"input_center_hz":454216998,"input_bandwidth_hz":800000,"psd":{"unit":"dBFS"}});
            e.generation = 7;
            for n in 1..=3u64 {
                let row: Vec<f32> = (0..64).map(|k| if k == 10 { -12. } else { -60. }).collect();
                e.psd.push_back((n, n as f64, row));
                e.psd_seq = n;
            }
        }
        let response = spectrum_bin(State(app.clone()), Query(SpectrumBin { after: 1, limit: None })).await.into_response();
        assert_eq!(response.headers()["x-sequence"], "3");
        let b = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        let u16at = |o: usize| u16::from_le_bytes(b[o..o + 2].try_into().unwrap());
        let f64at = |o: usize| f64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        let f32at = |o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        assert_eq!(&b[..4], b"RSP1");
        assert_eq!(u16at(4), 64, "bins");
        assert_eq!(u16at(6), 2, "rows after seq 1");
        assert_eq!(f64at(8), 454216998.0, "center (ppm 0)");
        assert_eq!(f64at(16), 800000.0, "span");
        let (low, high) = (f32at(24), f32at(28));
        assert!(low < -58. && high > -12. - 0.1);
        assert_eq!(u64::from_le_bytes(b[32..40].try_into().unwrap()), 3, "latest sequence");
        assert_eq!(u32::from_le_bytes(b[40..44].try_into().unwrap()), 7, "generation");
        assert_eq!(b.len(), 48 + 2 * (16 + 64));
        let row = &b[48..48 + 16 + 64];
        assert_eq!(u64::from_le_bytes(row[..8].try_into().unwrap()), 2);
        assert!(row[16 + 10] > 240 && row[16] < 20);
        // Zero rows still carry the latest sequence; the range is held between responses.
        let response = spectrum_bin(State(app.clone()), Query(SpectrumBin { after: 3, limit: None })).await.into_response();
        let b2 = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        assert_eq!(u16::from_le_bytes(b2[6..8].try_into().unwrap()), 0);
        assert_eq!(u64::from_le_bytes(b2[32..40].try_into().unwrap()), 3);
        assert_eq!(b2[24..32], b[24..32], "low/high unchanged");
        // Calibrated center: the radio reads 23.3 ppm high.
        let calibrated = App::configured(crate::runtime::AppConfig { ppm: 23.3, ..crate::runtime::AppConfig::new(3) });
        calibrated.engine().source = json!({"input_center_hz":454216998,"input_bandwidth_hz":800000,"psd":{"unit":"dBFS"}});
        let b3 = axum::body::to_bytes(spectrum_bin(State(calibrated), Query(SpectrumBin { after: 0, limit: None })).await.into_response().into_body(), 1 << 20).await.unwrap();
        let center = f64::from_le_bytes(b3[8..16].try_into().unwrap());
        assert!((center - 454_206_418.).abs() < 5., "{center}");
    }
    #[tokio::test]
    async fn activity_has_the_block_series_and_blocks_follow_the_contract() {
        let app = App::new(3);
        {
            let mut e = app.engine();
            e.store.add_activity("a", now_s(), 20., 12.);
            e.source = json!({"input_center_hz":454216998,"input_rate_hz":800000,"input_bandwidth_hz":800000,"capabilities":{"configure_supported":false}});
        }
        let Json(report) = activity(State(app.clone()), Query(ActivityQuery { window_s: Some(3600), bucket_s: Some(300) })).await;
        let block: f64 = report["block"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).sum();
        assert_eq!(block, 12.);
        assert!(report["baseline"].is_null(), "nothing learned yet");
        let Json(b) = blocks(State(app.clone())).await;
        assert_eq!(b["current_id"], "current");
        assert_eq!(b["blocks"][0]["center_hz"], 454216998.0);
        assert_eq!(b["blocks"][0]["name"], "Business radio, 453.8–454.6 MHz");
        assert_eq!(block_name(454_206_415., 800_000.), "Business radio, 453.8–454.6 MHz");
        assert_eq!(block_name(453_000_000., 6_000_000.), "Business radio, 450–456 MHz");
        assert_eq!(block_name(0., 0.), "Business radio");
        assert_eq!(b["can_change"], false);
        assert!(set_block(State(app.clone()), Json(json!({"id":"current"}))).await.is_ok());
        assert_eq!(set_block(State(app), Json(json!({"id":"elsewhere"}))).await.unwrap_err().0, StatusCode::UNPROCESSABLE_ENTITY);
    }
    #[tokio::test]
    async fn a_handler_panic_cannot_brick_the_engine_lock() {
        let app = App::new(3);
        let poisoner = app.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.engine();
            panic!("simulated handler panic under the engine lock");
        })
        .join();
        assert!(app.inner.is_poisoned());
        // Everything still answers.
        assert_eq!(status_json(&app)["mode"], "disconnected");
        let Json(report) = activity(State(app), Query(ActivityQuery { window_s: Some(u64::MAX), bucket_s: Some(u64::MAX) })).await;
        assert!(report["block"].is_array());
    }
    /// The listen sockets end to end: per-socket channel filtering, join-time filtering, pins on
    /// connect and unpin when the socket closes, and rejection of unknown channels.
    #[tokio::test]
    async fn per_browser_listen_sockets_route_audio_and_manage_pins() {
        type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
        /// (channel id, first PCM byte) of every audio message that arrives in 600 ms.
        async fn read_audio(ws: &mut Socket) -> Vec<(String, u8)> {
            use futures_util::StreamExt;
            use tokio_tungstenite::tungstenite::Message as Wire;
            let mut got = vec![];
            let deadline = tokio::time::Instant::now() + Duration::from_millis(600);
            let mut channel = String::new();
            while let Ok(Some(Ok(message))) = tokio::time::timeout_at(deadline, ws.next()).await {
                match message {
                    Wire::Text(t) => {
                        let v: Value = serde_json::from_str(t.as_str()).unwrap();
                        if v["type"] == "audio" {
                            channel = v["channel_id"].as_str().unwrap().to_string();
                        }
                    }
                    Wire::Binary(pcm) => got.push((channel.clone(), pcm[0])),
                    _ => {}
                }
            }
            got
        }
        let app = App::new(3);
        let (tx, rx) = std::sync::mpsc::sync_channel::<Value>(32);
        {
            let mut e = app.engine();
            e.active = true;
            e.generation = 5;
            e.control = Some(tx);
            for (id, stream) in [("A-1", 1u64), ("B-1", 2), ("C-1", 3)] {
                e.channels.insert(id.into(), json!({"id":id,"stream_id":stream,"in_range":true,"logical_hz":454000000.0 + stream as f64,"protocol":"Unknown"}));
            }
        }
        let router = router(app.clone(), None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let audio = |id: &str, tag: u8| crate::runtime::Audio {
            meta: json!({"type":"audio","channel_id":id,"source_generation":5,"samples":2}),
            pcm: vec![tag, 0, tag, 0],
            created: Instant::now(),
        };
        // Audio made before anyone joined must never be played.
        let _ = app.audio.send(audio("A-1", 9));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let (mut a, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/api/listen?channel_id=A-1")).await.unwrap();
        let (mut b, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/api/listen?channel_id=B-1")).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let pins: Vec<Value> = rx.try_iter().collect();
        let pinned: std::collections::BTreeSet<u64> = pins.iter().filter(|p| p["pinned"] == true).map(|p| p["stream_id"].as_u64().unwrap()).collect();
        assert_eq!(pinned, [1, 2].into(), "each socket pins its own stream: {pins:?}");
        assert_eq!(app.engine().listeners.len(), 2);
        let _ = app.audio.send(audio("A-1", 1));
        let _ = app.audio.send(audio("C-1", 3)); // nobody is listening to C
        let _ = app.audio.send(audio("B-1", 2));
        // Each socket receives exactly its own channel's audio (metadata then PCM).
        assert_eq!(read_audio(&mut a).await, vec![("A-1".to_string(), 1)]);
        assert_eq!(read_audio(&mut b).await, vec![("B-1".to_string(), 2)]);
        // Closing A releases its pin; B's stays.
        a.close(None).await.ok();
        drop(a);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let unpins: Vec<Value> = rx.try_iter().filter(|p| p["pinned"] == false).collect();
        assert_eq!(unpins.len(), 1, "{unpins:?}");
        assert_eq!(unpins[0]["stream_id"], 1);
        assert_eq!(app.engine().listeners.len(), 1);
        // An unknown channel is refused before the upgrade.
        let refused = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/api/listen?channel_id=nope")).await;
        assert!(refused.is_err(), "unknown channel must not upgrade");
        // Without channel_id the socket follows the shared selection (legacy behavior).
        let (mut legacy, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/api/listen")).await.unwrap();
        app.engine().selected = Some("C-1".into());
        app.engine().selected_at = Instant::now() - Duration::from_secs(1);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = app.audio.send(audio("C-1", 7));
        assert_eq!(read_audio(&mut legacy).await, vec![("C-1".to_string(), 7)]);
    }
    #[tokio::test]
    async fn the_listener_cap_holds_under_parallel_handshakes() {
        let app = App::new(3);
        {
            let mut e = app.engine();
            e.channels.insert("A-1".into(), json!({"id":"A-1","stream_id":1,"in_range":true,"logical_hz":454000001.0}));
        }
        let router = router(app.clone(), None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let url = format!("ws://127.0.0.1:{port}/api/listen?channel_id=A-1");
        let attempts: Vec<_> = (0..MAX_LISTENERS + 8).map(|_| tokio::spawn(tokio_tungstenite::connect_async(url.clone()))).collect();
        let mut sockets = vec![];
        let (mut accepted, mut refused) = (0, 0);
        for attempt in attempts {
            match attempt.await.unwrap() {
                Ok(socket) => {
                    accepted += 1;
                    sockets.push(socket);
                }
                Err(_) => refused += 1,
            }
        }
        assert_eq!(accepted, MAX_LISTENERS, "exactly the cap is admitted ({refused} refused)");
        assert_eq!(app.engine().listeners.len(), MAX_LISTENERS);
    }
    #[tokio::test]
    async fn missing_source_controls_never_report_applied() {
        let app = App::new(3);
        let error = configure_source(
            State(app),
            Json(json!({"mode":"live","rate_hz":1250000,"center_hz":454216998})),
        )
        .await
        .unwrap_err();
        assert_eq!(error.0, StatusCode::NOT_IMPLEMENTED);
    }
    #[tokio::test]
    async fn stop_ack_is_independent_of_configure_capability() {
        let app = App::new(3);
        let (tx, rx) = std::sync::mpsc::sync_channel::<Value>(1);
        {
            let mut e = app.engine();
            e.control = Some(tx);
            e.source = json!({"capabilities":{"stop_supported":true,"configure_supported":false}});
        }
        let other = app.clone();
        std::thread::spawn(move || {
            let request = rx.recv().unwrap();
            assert_eq!(request["action"], "stop");
            let mut e = other.engine();
            e.source["control_ack"] = json!({"request_id":request["request_id"],"ok":true});
            e.source["receiver_state"] = json!("stopped");
        });
        let result = stop(State(app)).await.unwrap();
        assert_eq!(result.0["status"], "stopped");
    }
}

#[cfg(test)]
mod fpga_tests {
    use super::*;
    use crate::runtime::AppConfig;

    #[tokio::test]
    async fn repeated_tuning_releases_targets_and_stopped_catalog_entries_can_be_reselected() {
        let app = App::new(8);
        {
            let mut e = app.engine();
            e.active = true;
            e.source = json!({"input_center_hz":460000000,"input_bandwidth_hz":20000000,
                "streams":(0..512).map(|k| json!({"stream_id":k+1,
                    "center_hz":460000000i64 + if k<256 {k as i64} else {k as i64-512}*50000,
                    "sample_rate_num_hz":25600000,"sample_rate_den":256})).collect::<Vec<_>>()});
        }
        let mut first_id = String::new();
        for n in 0..80 {
            let Json(channel) = tune(State(app.clone()), Json(json!({"frequency_hz":459012345+n*12500}))).await.unwrap();
            let id = channel["id"].as_str().unwrap().to_string();
            if n == 0 { first_id = id.clone(); }
            assert!(app.engine().host_targets.len() <= 2, "current listener plus pending replacement");
            let _ = monitor(State(app.clone()), Json(Monitor { channel_id: Some(id) })).await.unwrap();
            assert_eq!(app.engine().host_targets.len(), 1);
        }
        let _ = monitor(State(app.clone()), Json(Monitor { channel_id: None })).await.unwrap();
        assert!(app.engine().host_targets.is_empty());
        assert!(app.engine().channels.contains_key(&first_id), "stopping preserves the catalog");
        let _ = monitor(State(app.clone()), Json(Monitor { channel_id: Some(first_id.clone()) })).await.unwrap();
        assert!(app.engine().host_targets.contains(&459012345), "reselecting restores fine tuning");
        let dmr = {
            let mut e = app.engine();
            let stream = e.channels[&first_id]["stream_id"].as_u64().unwrap() as u32;
            let channel = crate::runtime::make_channel(&app, stream, 459012345., "DMR", 2);
            let id = channel["id"].as_str().unwrap().to_string();
            e.channels.insert(id.clone(), channel);
            e.listeners.insert(1, id.clone());
            id
        };
        // Replacing a target within the same display-frequency identity is also bounded.
        let _ = tune(State(app.clone()), Json(json!({"frequency_hz":459012400}))).await.unwrap();
        assert_eq!(app.engine().host_targets.len(), 1);
        assert!(app.engine().host_targets.contains(&459012400));
        assert_eq!(app.engine().channels[&dmr]["logical_hz"], 459012400);
        let _ = monitor(State(app.clone()), Json(Monitor { channel_id: None })).await.unwrap();
        assert_eq!(app.engine().host_targets.len(), 1, "an explicit listener still owns its target");
        drop(ListenerGuard { app: app.clone(), connection: 1 });
        assert!(app.engine().host_targets.is_empty(), "last listener releases its target");
        let _ = tune(State(app.clone()), Json(json!({"frequency_hz":459012345}))).await.unwrap();
        let _ = stop(State(app.clone())).await.unwrap();
        assert!(app.engine().host_targets.is_empty(), "receiver stop clears a pending tune too");
        assert!(app.engine().listener["channel_id"].is_null());
    }

    #[tokio::test]
    async fn tuning_a_saved_channel_restores_its_frequency_and_keeps_the_requested_dmr_slot() {
        let app = App::configured(AppConfig { ppm: 23.3, ..AppConfig::new(8) });
        let id = "452656250-DMR-2";
        {
            let mut e = app.engine();
            e.active = true;
            e.source = json!({"input_center_hz":460016998,"input_bandwidth_hz":20000000,
                "streams":[{"stream_id":366,"center_hz":452666998,"sample_rate_num_hz":25600000,"sample_rate_den":256}]});
            // The id predates the measured carrier and survives a service restart.
            e.channels.insert(id.into(), json!({"id":id,"frequency_hz":452675000,
                "protocol":"DMR","slot":2,"in_range":false,"logical_hz":null,"stream_id":0}));
        }
        let Json(channel) = tune(State(app.clone()), Json(json!({"frequency_hz":452675000,"channel_id":id}))).await.unwrap();
        assert_eq!(channel["id"], id);
        assert_eq!(channel["slot"], 2);
        assert_eq!(channel["logical_hz"], 452685547);
        assert_eq!(channel["stream_id"], 366);
        assert_eq!(channel["in_range"], true);
        monitor(State(app.clone()), Json(Monitor { channel_id: Some(id.into()) })).await.unwrap();
        assert!(app.engine().host_targets.contains(&452685547));
        let error = tune(State(app.clone()), Json(json!({"frequency_hz":471000000,"channel_id":id}))).await.unwrap_err();
        assert_eq!(error.0, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(app.engine().channels[id]["logical_hz"], 452685547);
        let error = tune(State(app), Json(json!({"frequency_hz":452675000,"channel_id":"missing"}))).await.unwrap_err();
        assert_eq!(error.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn operator_tuning_uses_reading_frame_and_rejects_outside_rf_filter() {
        let app = App::configured(AppConfig { ppm: 23.3, ..AppConfig::new(8) });
        {
            let mut e = app.engine();
            e.active = true;
            e.source = json!({"input_center_hz":454216998,"input_bandwidth_hz":20000000,
                "streams":[{"stream_id":509,"center_hz":454016998,"sample_rate_num_hz":25600000,"sample_rate_den":256}]});
        }
        let Json(channel) = tune(State(app.clone()), Json(json!({"frequency_hz":454029223. / 1.0000233}))).await.unwrap();
        assert_eq!(channel["stream_id"], 509);
        assert_eq!(channel["logical_hz"], 454029223.);
        assert!(app.engine().host_targets.contains(&454029223));
        let Json(raw) = tune(State(app.clone()), Json(json!({"frequency_hz":454018750,"raw":true}))).await.unwrap();
        assert_eq!(raw["protocol"], "Raw");
        assert_eq!(raw["id"], "454018750-Raw-1");
        assert_ne!(raw["decoded"], true);
        let _ = monitor(State(app.clone()), Json(Monitor { channel_id: raw["id"].as_str().map(str::to_string) })).await.unwrap();
        assert_eq!(app.engine().selected.as_deref(), Some("454018750-Raw-1"));
        let error = tune(State(app.clone()), Json(json!({"frequency_hz":470000000}))).await.unwrap_err();
        assert_eq!(error.0, StatusCode::UNPROCESSABLE_ENTITY);
        app.engine().active = false;
        assert_eq!(tune(State(app), Json(json!({"frequency_hz":454018750}))).await.unwrap_err().0, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn spectrum_uses_measured_span_with_the_same_frequency_calibration_as_markers() {
        let source = json!({"input_center_hz":460000000,"input_bandwidth_hz":20000000,
            "psd":{"center_hz":459000000,"span_hz":25600000}});
        let cal = crate::freq::Calibration { ppm: 23.3 };
        let (center, span) = spectrum_geometry(&source, cal);
        assert_eq!(center, cal.true_hz(459000000.));
        assert_eq!(span, cal.true_hz(25600000.));
        assert_eq!(spectrum_geometry(&json!({"input_center_hz":460000000,"input_bandwidth_hz":20000000}), cal),
            (cal.true_hz(460000000.), cal.true_hz(20000000.)));
    }
}
