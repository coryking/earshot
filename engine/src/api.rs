use crate::runtime::App;
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
    let edits = c["id"].as_str().and_then(|id| user.get(id));
    out["user"] = edits.cloned().unwrap_or(Value::Null);
    out["display_name"] = edits
        .and_then(|u| u["name"].as_str())
        .map(|n| json!(n))
        .unwrap_or_else(|| c["label"].clone());
    out
}
fn now_s() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}
/// The browser validates this payload strictly (web/src/lib/radio.ts), so every numeric
/// field must be a number even before the first CONFIG or sample arrives.
fn status_json(app: &App) -> Value {
    let e = app.inner.lock().unwrap();
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
    json!({"mode":if e.active{"live"}else{"disconnected"},"state":if e.active{"receiving"}else if e.error.is_some(){"error"}else{"stopped"},"source_name":"Pluto channelized RIQ1","center_hz":number(&e.source["input_center_hz"]),"rate_hz":number(&e.source["input_rate_hz"]),"bandwidth_hz":number(&e.source["input_bandwidth_hz"]),"generation":e.generation,"channels":e.channels.values().map(|c| with_user(c, &e.store.user)).collect::<Vec<_>>(),"received_seconds":position,"source_position_s":position,"decode_latency_s":latency,"dropped_batches":e.drops,"processed_batches":e.frames,"queue_depth":number(&e.source["queue_depth"]),"error":e.error,"decoder_error":null,"started_at":e.started,"last_samples_at":e.source["last_samples_at"],"continuity":"transport sequence checked; hardware continuity unknown","decoder_capacity":app.max_decoders,"calibration_ppm":app.calibration.ppm,"labels_loaded":app.labels.len(),"source":e.source})
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
    let mut e = app.inner.lock().unwrap();
    if let Some(ref id) = m.channel_id {
        if !e.channels.values().any(|c| c["id"] == *id) {
            return Err((StatusCode::NOT_FOUND, "channel not in catalog".into()));
        }
    }
    if e.selected == m.channel_id {
        return Ok(Json(e.listener.clone()));
    }
    let stream_for = |id: Option<&str>| {
        id.and_then(|id| e.channels.get(id))
            .and_then(|c| c["stream_id"].as_u64())
    };
    let previous = stream_for(e.selected.as_deref());
    let next = stream_for(m.channel_id.as_deref());
    if let Some(control) = &e.control {
        if let Some(stream) = previous.filter(|s| Some(*s) != next) {
            let _=control.try_send(json!({"action":"pin","request_id":format!("unpin-{}",e.decoder_generation+1),"stream_id":stream,"pinned":false}));
        }
        if let Some(stream) = next {
            let _=control.try_send(json!({"action":"pin","request_id":format!("pin-{}",e.decoder_generation+1),"stream_id":stream,"pinned":true}));
        }
    }
    e.selected = m.channel_id;
    e.selected_at = Instant::now();
    e.decoder_generation += 1;
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
    let e = app.inner.lock().unwrap();
    let mut value = e.listener.clone();
    value["generation"] = json!(e.decoder_generation);
    value["source_generation"] = json!(e.generation);
    value["queue_blocks"] = e.source["queue_depth"].clone();
    value["dropped_blocks"] = json!(e.drops);
    value["input_dropped"] = json!(e.drops);
    Json(value)
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
async fn spectrum(State(app): State<App>, Query(q): Query<After>) -> Json<Value> {
    let e = app.inner.lock().unwrap();
    let unit = e.source["psd"]["unit"].as_str().unwrap_or("uncalibrated");
    let (low_db, high_db) = display_range(unit, &e.psd);
    let rows: Vec<_> = e
        .psd
        .iter()
        .filter(|(n, _, _)| *n > q.after)
        .take(60)
        .collect();
    Json(
        json!({"sequence":rows.last().map(|x|x.0).unwrap_or(e.psd_seq),"generation":e.generation,"rows":rows.iter().map(|x|&x.2).collect::<Vec<_>>(),"times":rows.iter().map(|x|x.1).collect::<Vec<_>>(),"center_hz":e.source["input_center_hz"],"span_hz":e.source["input_bandwidth_hz"],"low_db":low_db,"high_db":high_db,"unit":unit,"source":if e.active{"live"}else{"disconnected"}}),
    )
}
async fn ws(State(app): State<App>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.max_message_size(8192)
        .on_upgrade(move |s| socket(s, app))
}
async fn socket(mut socket: WebSocket, app: App) {
    let mut rx = app.audio.subscribe();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
                audio=rx.recv()=>{match audio{Ok(a)=>{let current={let e=app.inner.lock().unwrap();e.active&&a.meta["generation"]==e.decoder_generation&&a.meta["source_generation"]==e.generation&&a.meta["channel_id"].as_str()==e.selected.as_deref()};if current {
        if a.created.elapsed()>Duration::from_millis(250){if socket.send(Message::Text(json!({"type":"gap","reason":"audio_expired"}).to_string().into())).await.is_err(){break;}continue;}
        let sent=tokio::time::timeout(Duration::from_millis(250),async{socket.send(Message::Text(a.meta.to_string().into())).await?;socket.send(Message::Binary(a.pcm.into())).await}).await;
        if !matches!(sent,Ok(Ok(()))){break;}
        }},Err(broadcast::error::RecvError::Lagged(n))=>{rx=app.audio.subscribe();if socket.send(Message::Text(json!({"type":"gap","dropped_chunks":n}).to_string().into())).await.is_err(){break;}},Err(_)=>break}},
                _=tick.tick()=>{let value={let e=app.inner.lock().unwrap();json!({"type":"heartbeat","generation":e.decoder_generation,"source_generation":e.generation,"channel_id":e.selected})};if socket.send(Message::Text(value.to_string().into())).await.is_err(){break;}},
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
        _=tick.tick()=>{let e=app.inner.lock().unwrap();json!({"type":"heartbeat","generation":e.decoder_generation,"source_generation":e.generation})},
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
        let e = app.inner.lock().unwrap();
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
            let e = app.inner.lock().unwrap();
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
        let e = app.inner.lock().unwrap();
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
        let mut e = app.inner.lock().unwrap();
        e.active = false;
        e.decoder_generation += 1;
        e.selected = None;
        e.listener["status"] = json!("stopped");
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
    before: Option<u64>,
    limit: Option<usize>,
    source: Option<String>,
}
/// Recent calls, newest first. `next_before` is the cursor for the next (older) page.
async fn calls(State(app): State<App>, Query(q): Query<CallsQuery>) -> Json<Value> {
    let e = app.inner.lock().unwrap();
    let (calls, next) = e.store.list(q.before, q.limit.unwrap_or(50).clamp(1, 200), q.source.as_deref());
    let in_progress = e.store.open_calls();
    Json(json!({"calls":calls,"next_before":next,"in_progress":in_progress}))
}
async fn call_audio(State(app): State<App>, Path(id): Path<String>) -> impl IntoResponse {
    let audio = id
        .parse::<u64>()
        .ok()
        .and_then(|n| app.inner.lock().unwrap().store.audio(n));
    match audio {
        Some(samples) => (
            [
                (header::CONTENT_TYPE, HeaderValue::from_static("audio/wav")),
                (header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400")),
            ],
            crate::store::wav_bytes(&samples),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({"detail":"no audio for that call"}))).into_response(),
    }
}
/// The persisted catalog: channels that were located, decoded or edited, with operator edits.
async fn catalog(State(app): State<App>) -> Json<Value> {
    let e = app.inner.lock().unwrap();
    let mut entries: Vec<Value> = e
        .channels
        .values()
        .filter(|c| {
            c["decoded"] == true
                || c["carrier_snapped"] == true
                || c["id"].as_str().is_some_and(|id| e.store.user.contains_key(id))
        })
        .map(|c| {
            let mut v = with_user(c, &e.store.user);
            // `kind` is channel|talkgroup for the catalog (talkgroups arrive with trunking);
            // the engine's own voice/data/unidentified classification moves to `channel_kind`.
            v["channel_kind"] = c["kind"].clone();
            v["kind"] = json!("channel");
            v
        })
        .collect();
    entries.sort_by(|a, b| {
        let key = |v: &Value| v["last_heard"].as_f64().unwrap_or(0.);
        key(b).total_cmp(&key(a))
    });
    Json(json!({"entries":entries,"count":entries.len()}))
}
async fn catalog_edit(State(app): State<App>, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult {
    let mut e = app.inner.lock().unwrap();
    if !e.channels.contains_key(&id) && !e.store.known.contains_key(&id) {
        return Err(api_error(StatusCode::NOT_FOUND, "channel not in catalog"));
    }
    let object = body.as_object().ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "body must be an object"))?;
    let mut edits = e.store.user.get(&id).cloned().unwrap_or_else(|| json!({}));
    for (key, value) in object {
        match (key.as_str(), value) {
            ("name", Value::String(n)) if n.chars().count() <= 80 => edits["name"] = json!(n.trim()),
            ("name", Value::Null) => {
                edits.as_object_mut().map(|o| o.remove("name"));
            }
            ("notes", Value::String(n)) if n.chars().count() <= 500 => edits["notes"] = json!(n),
            ("followed" | "muted", Value::Bool(b)) => edits[key] = json!(b),
            _ => {
                return Err(api_error(
                    StatusCode::BAD_REQUEST,
                    format!("cannot set {key}: allowed are name (<=80 chars or null), notes (<=500), followed, muted"),
                ));
            }
        }
    }
    e.store.user.insert(id.clone(), edits);
    e.store.user_dirty = true;
    let current = e.channels.get(&id).cloned().unwrap_or_else(|| e.store.known[&id].clone());
    Ok(Json(with_user(&current, &e.store.user)))
}
#[derive(Deserialize)]
struct ActivityQuery {
    window_s: Option<u64>,
    bucket_s: Option<u64>,
}
/// Talk-seconds per bucket (block total and per source) with a same-time baseline.
async fn activity(State(app): State<App>, Query(q): Query<ActivityQuery>) -> Json<Value> {
    let e = app.inner.lock().unwrap();
    Json(e.store.activity(now_s(), q.window_s.unwrap_or(86400), q.bucket_s.unwrap_or(300)))
}
#[derive(Deserialize)]
struct SpectrumBin {
    #[serde(default)]
    after: u64,
    limit: Option<usize>,
}
/// Compact spectrum rows, RSP1. 32-byte little-endian header: magic `RSP1`, version u16 (1),
/// bins u16, rows u16, flags u16 (0), low_db f32, high_db f32, center_hz f64, span_hz f32.
/// Then per row: seq u64, time f64 (producer seconds), bins x u8 where
/// level = round(255 * (clamp(db, low, high) - low) / (high - low)). Rows with seq > `after`,
/// oldest first. `X-Sequence` is the newest sequence number the engine holds.
async fn spectrum_bin(State(app): State<App>, Query(q): Query<SpectrumBin>) -> impl IntoResponse {
    let e = app.inner.lock().unwrap();
    let unit = e.source["psd"]["unit"].as_str().unwrap_or("uncalibrated");
    let (low, high) = display_range(unit, &e.psd);
    let rows: Vec<_> = e.psd.iter().filter(|(n, _, _)| *n > q.after).take(q.limit.unwrap_or(60).clamp(1, 120)).collect();
    let bins = rows.first().map_or(0, |r| r.2.len());
    let mut out = Vec::with_capacity(32 + rows.len() * (16 + bins));
    out.extend_from_slice(b"RSP1");
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(bins as u16).to_le_bytes());
    out.extend_from_slice(&(rows.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(low as f32).to_le_bytes());
    out.extend_from_slice(&(high as f32).to_le_bytes());
    out.extend_from_slice(&e.source["input_center_hz"].as_f64().unwrap_or(0.).to_le_bytes());
    out.extend_from_slice(&(e.source["input_bandwidth_hz"].as_f64().unwrap_or(0.) as f32).to_le_bytes());
    for (seq, time, row) in &rows {
        out.extend_from_slice(&seq.to_le_bytes());
        out.extend_from_slice(&time.to_le_bytes());
        out.extend(row.iter().map(|v| {
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
/// The block currently parked on. Changing it needs a producer that can retune live.
async fn blocks(State(app): State<App>) -> Json<Value> {
    let e = app.inner.lock().unwrap();
    Json(json!({
        "current": {
            "center_hz": e.source["input_center_hz"],
            "rate_hz": e.source["input_rate_hz"],
            "bandwidth_hz": e.source["input_bandwidth_hz"],
            "calibration_ppm": app.calibration.ppm,
        },
        "can_change": e.source["capabilities"]["configure_supported"] == true,
        "presets": [],
    }))
}
async fn set_block(State(app): State<App>, Json(config): Json<Value>) -> ApiResult {
    configure_source(State(app), Json(config)).await
}

pub fn router(app: App, web_dir: Option<PathBuf>) -> Router {
    let mut router = Router::new()
        .route("/api/status", get(status))
        .route("/api/listener", get(listener))
        .route("/api/monitor", post(monitor))
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
            let mut e = app.inner.lock().unwrap();
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
            let mut e = app.inner.lock().unwrap();
            let ch = json!({"id":"454018750-DMR-2","protocol":"DMR","slot":2,"kind":"voice","decoded":true,
                "frequency_hz":454018750.0,"label":"Day Wireless (business trunked)","last_heard":1.0,"carrier_snapped":true});
            e.channels.insert("454018750-DMR-2".into(), ch);
            let noise = json!({"id":"454054498-Unknown-1","protocol":"Unknown","kind":"unidentified","decoded":false});
            e.channels.insert("454054498-Unknown-1".into(), noise);
        }
        app
    }
    #[tokio::test]
    async fn calls_list_newest_first_and_serve_wav_audio() {
        let app = seeded_app();
        {
            let mut e = app.inner.lock().unwrap();
            for (i, id) in ["454018750-DMR-2", "454018750-DMR-1"].iter().enumerate() {
                e.store.record(id, "DMR", 2, 454018750., &json!("Day Wireless"), Some(-30.), &vec![500i16; 16000], 100. + i as f64 * 50.);
            }
            e.store.close_idle(1000.);
        }
        let Json(list) = calls(State(app.clone()), Query(CallsQuery { before: None, limit: Some(10), source: None })).await;
        let items = list["calls"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["source"], "454018750-DMR-1", "newest first");
        assert_eq!(items[0]["audio_url"], "/api/calls/2/audio.wav");
        assert_eq!(items[0]["duration_s"], 2.0);
        assert!(items[0]["talker"].is_null() && items[0]["transcript"].is_null());
        let response = call_audio(State(app.clone()), Path("2".into())).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "audio/wav");
        let body = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        assert_eq!(&body[..4], b"RIFF");
        assert_eq!(body.len(), 44 + 32000);
        assert_eq!(call_audio(State(app), Path("99".into())).await.into_response().status(), StatusCode::NOT_FOUND);
    }
    #[tokio::test]
    async fn catalog_lists_located_channels_and_applies_operator_edits() {
        let app = seeded_app();
        let Json(before) = catalog(State(app.clone())).await;
        assert_eq!(before["count"], 1, "unconfirmed noise bins are not catalog entries");
        assert_eq!(before["entries"][0]["display_name"], "Day Wireless (business trunked)");
        let edited = catalog_edit(
            State(app.clone()),
            Path("454018750-DMR-2".into()),
            Json(json!({"name":"Day Wireless dispatch","followed":true})),
        )
        .await
        .unwrap();
        assert_eq!(edited.0["display_name"], "Day Wireless dispatch");
        assert_eq!(edited.0["user"]["followed"], true);
        let Json(after) = catalog(State(app.clone())).await;
        assert_eq!(after["entries"][0]["display_name"], "Day Wireless dispatch");
        assert_eq!(after["entries"][0]["kind"], "channel");
        let status = status_json(&app);
        assert_eq!(status["channels"][0]["user"].is_object() || status["channels"][1]["user"].is_object(), true);
        let bad = catalog_edit(State(app.clone()), Path("454018750-DMR-2".into()), Json(json!({"frequency_hz":1}))).await;
        assert_eq!(bad.unwrap_err().0, StatusCode::BAD_REQUEST);
        let missing = catalog_edit(State(app), Path("nope".into()), Json(json!({"name":"x"}))).await;
        assert_eq!(missing.unwrap_err().0, StatusCode::NOT_FOUND);
    }
    #[tokio::test]
    async fn spectrum_bin_is_rsp1_with_levels_scaled_to_the_data_range() {
        let app = App::new(3);
        {
            let mut e = app.inner.lock().unwrap();
            e.source = json!({"input_center_hz":454216998,"input_bandwidth_hz":800000,"psd":{"unit":"dBFS"}});
            for n in 1..=3u64 {
                let row: Vec<f32> = (0..64).map(|k| if k == 10 { -12. } else { -60. }).collect();
                e.psd.push_back((n, n as f64, row));
                e.psd_seq = n;
            }
        }
        let response = spectrum_bin(State(app), Query(SpectrumBin { after: 1, limit: None })).await.into_response();
        assert_eq!(response.headers()["x-sequence"], "3");
        let b = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        assert_eq!(&b[..4], b"RSP1");
        assert_eq!(u16::from_le_bytes([b[4], b[5]]), 1);
        assert_eq!(u16::from_le_bytes([b[6], b[7]]), 64, "bins");
        assert_eq!(u16::from_le_bytes([b[8], b[9]]), 2, "rows after seq 1");
        let low = f32::from_le_bytes(b[12..16].try_into().unwrap());
        let high = f32::from_le_bytes(b[16..20].try_into().unwrap());
        assert!(low < -58. && high > -12. - 0.1);
        assert_eq!(f64::from_le_bytes(b[20..28].try_into().unwrap()), 454216998.);
        assert_eq!(b.len(), 32 + 2 * (16 + 64));
        let row = &b[32..32 + 16 + 64];
        assert_eq!(u64::from_le_bytes(row[..8].try_into().unwrap()), 2);
        assert!(row[16 + 10] > 240, "the peak bin sits at the top of the range");
        assert!(row[16] < 20, "floor bins sit at the bottom");
    }
    #[tokio::test]
    async fn activity_reports_buckets_and_blocks_reflects_the_source() {
        let app = App::new(3);
        {
            let mut e = app.inner.lock().unwrap();
            e.store.add_activity("a", now_s(), 20., 12.);
            e.source = json!({"input_center_hz":454216998,"input_rate_hz":800000,"input_bandwidth_hz":800000,"capabilities":{"configure_supported":false}});
        }
        let Json(report) = activity(State(app.clone()), Query(ActivityQuery { window_s: Some(3600), bucket_s: Some(300) })).await;
        let total: f64 = report["total"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).sum();
        assert_eq!(total, 12.);
        let Json(block) = blocks(State(app)).await;
        assert_eq!(block["current"]["center_hz"], 454216998);
        assert_eq!(block["can_change"], false);
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
            let mut e = app.inner.lock().unwrap();
            e.control = Some(tx);
            e.source = json!({"capabilities":{"stop_supported":true,"configure_supported":false}});
        }
        let other = app.clone();
        std::thread::spawn(move || {
            let request = rx.recv().unwrap();
            assert_eq!(request["action"], "stop");
            let mut e = other.inner.lock().unwrap();
            e.source["control_ack"] = json!({"request_id":request["request_id"],"ok":true});
            e.source["receiver_state"] = json!("stopped");
        });
        let result = stop(State(app)).await.unwrap();
        assert_eq!(result.0["status"], "stopped");
    }
}
