use crate::runtime::App;
use axum::{
    Json, Router,
    extract::{
        Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::StatusCode,
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
/// The browser validates this payload strictly (web/src/lib/radio.ts), so every numeric
/// field must be a number even before the first CONFIG or sample arrives.
fn status_json(app: &App) -> Value {
    let e = app.inner.lock().unwrap();
    let number = |v: &Value| json!(v.as_f64().unwrap_or(0.));
    let position = number(&e.source["source_position_s"]);
    // Host ingress age of the latest delivered audio; 0 until audio flows.
    let latency = e.listener["ingress_age_ms"].as_f64().unwrap_or(0.) / 1000.;
    json!({"mode":if e.active{"live"}else{"disconnected"},"state":if e.active{"receiving"}else if e.error.is_some(){"error"}else{"stopped"},"source_name":"Pluto channelized RIQ1","center_hz":number(&e.source["input_center_hz"]),"rate_hz":number(&e.source["input_rate_hz"]),"bandwidth_hz":number(&e.source["input_bandwidth_hz"]),"generation":e.generation,"channels":e.channels.values().collect::<Vec<_>>(),"received_seconds":position,"source_position_s":position,"decode_latency_s":latency,"dropped_batches":e.drops,"processed_batches":e.frames,"queue_depth":number(&e.source["queue_depth"]),"error":e.error,"decoder_error":null,"started_at":e.started,"last_samples_at":e.source["last_samples_at"],"continuity":"transport sequence checked; hardware continuity unknown","decoder_capacity":app.max_decoders,"source":e.source})
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
        if e.channels
            .get(id)
            .is_some_and(|c| c["protocol"] == "Unknown")
        {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                "Channel protocol is not confirmed yet".into(),
            ));
        }
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
    let mut values: Vec<f32> = rows.iter().flat_map(|r| r.2.iter().copied()).collect();
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

pub fn router(app: App, web_dir: Option<PathBuf>) -> Router {
    let mut router = Router::new()
        .route("/api/status", get(status))
        .route("/api/listener", get(listener))
        .route("/api/monitor", post(monitor))
        .route("/api/stop", post(stop))
        .route("/api/source", post(configure_source))
        .route("/api/spectrum", get(spectrum))
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
