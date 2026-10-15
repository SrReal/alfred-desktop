//! Bounded in-memory subscriptions. No durable event content, automatic reconnect
//! or replay. A fresh start always has a new stream ID and an explicit history gap.
use super::*;
use futures_util::{SinkExt, StreamExt};
use std::{collections::VecDeque, sync::Mutex, time::Instant};
use tokio_tungstenite::{
    tungstenite::{protocol::WebSocketConfig, Message},
    MaybeTlsStream, WebSocketStream,
};
use tokio_util::sync::CancellationToken;
type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
const FRAME_LIMIT: usize = 64 * 1024;
const QUEUE_LIMIT: usize = 256 * 1024;
pub(super) struct State {
    pub(super) status: &'static str,
    pub(super) reason: String,
    pub(super) events: VecDeque<Value>,
    bytes: usize,
    received: u64,
    dropped: u64,
    unsubscribe_acknowledged: bool,
}
pub(crate) struct Stream {
    connection: String,
    scope: String,
    event_type: String,
    pub(super) state: Mutex<State>,
    pub(super) cancel: CancellationToken,
}
impl Stream {
    fn finish(&self, reason: &str, clear: bool) {
        if let Ok(mut s) = self.state.lock() {
            s.status = "closed";
            s.reason = reason.into();
            if clear {
                s.events.clear();
                s.bytes = 0;
            }
        }
    }
    fn event(&self, v: Value) -> Result<(), String> {
        let n = v.to_string().len();
        if n > FRAME_LIMIT {
            return Err("HA_WS_EVENT_LIMIT".into());
        }
        let mut s = self.state.lock().map_err(|_| "HA_WS_LOCK")?;
        while s.events.len() >= 128 || s.bytes + n > QUEUE_LIMIT {
            if let Some(old) = s.events.pop_front() {
                s.bytes -= old.to_string().len();
                s.dropped += 1;
            } else {
                return Err("HA_WS_QUEUE_LIMIT".into());
            }
        }
        s.bytes += n;
        s.received += 1;
        s.events.push_back(v);
        Ok(())
    }
}
fn scope(s: &Session) -> String {
    digest(
        &json!({"config":s.scope.raw,"generation":s.scope.generation,"credential_binding":s.scope.credential_binding}),
    )
}
pub(super) fn validate(s: &Session, v: &Value) -> Result<(), String> {
    if v["action"] == "start" {
        let event = v["event_type"].as_str().ok_or("HA_WS_EVENT_REQUIRED")?;
        if !s.scope.config.websocket || !s.scope.config.allowed_events.iter().any(|e| e == event) {
            return Err("HA_WS_EVENT_OUT_OF_SCOPE".into());
        }
        if v.get("stream_id").is_some() || v.get("max_events").is_some() {
            return Err("HA_WS_START_FIELDS".into());
        }
    } else {
        canonical(v["stream_id"].as_str().ok_or("HA_WS_STREAM_REQUIRED")?)?;
        if v.get("event_type").is_some() || (v["action"] != "poll" && v.get("max_events").is_some())
        {
            return Err("HA_WS_CONTROL_FIELDS".into());
        }
    }
    Ok(())
}
async fn read(ws: &mut Socket) -> Result<Value, String> {
    loop {
        match ws
            .next()
            .await
            .ok_or("HA_WS_DISCONNECTED")?
            .map_err(|_| "HA_WS_FRAME_OR_TRANSPORT")?
        {
            Message::Text(t) => {
                return serde_json::from_str(&t).map_err(|_| "HA_WS_MALFORMED_JSON".into())
            }
            Message::Ping(_) => ws.flush().await.map_err(|_| "HA_WS_TRANSPORT")?,
            Message::Pong(_) => (),
            Message::Close(_) => return Err("HA_WS_DISCONNECTED".into()),
            _ => return Err("HA_WS_TEXT_REQUIRED".into()),
        }
    }
}
async fn bounded<T>(s: &Session, f: impl Future<Output = Result<T, String>>) -> Result<T, String> {
    s.scope
        .guard(async {
            tokio::time::timeout(Duration::from_secs(8), f)
                .await
                .map_err(|_| "HA_WS_TIMEOUT")?
        })
        .await
}
async fn send(ws: &mut Socket, v: &Value) -> Result<(), String> {
    ws.send(Message::Text(v.to_string().into()))
        .await
        .map_err(|_| "HA_WS_TRANSPORT".into())
}
async fn connect(s: &Session, event: &str) -> Result<Socket, String> {
    let mut url = s.base.join("/api/websocket").map_err(|_| "HA_WS_ORIGIN")?;
    url.set_scheme(if s.base.scheme() == "https" {
        "wss"
    } else {
        "ws"
    })
    .map_err(|_| "HA_WS_ORIGIN")?;
    let config = WebSocketConfig::default()
        .read_buffer_size(4096)
        .write_buffer_size(0)
        .max_write_buffer_size(2 * FRAME_LIMIT)
        .max_message_size(Some(FRAME_LIMIT))
        .max_frame_size(Some(FRAME_LIMIT));
    let (mut ws, _) = bounded(s, async {
        tokio_tungstenite::connect_async_with_config(url.as_str(), Some(config), false)
            .await
            .map_err(|_| "HA_WS_CONNECT".into())
    })
    .await?;
    let required = bounded(s, read(&mut ws)).await?;
    if required["type"] != "auth_required" || required.get("id").is_some() {
        return Err("HA_WS_AUTH_PROTOCOL".into());
    }
    bounded(
        s,
        send(&mut ws, &json!({"type":"auth","access_token":s.token})),
    )
    .await?;
    let ack = bounded(s, read(&mut ws)).await?;
    if ack["type"] == "auth_invalid" {
        return Err("HA_WS_AUTH_INVALID".into());
    }
    if ack["type"] != "auth_ok" || ack.get("id").is_some() {
        return Err("HA_WS_AUTH_PROTOCOL".into());
    }
    bounded(
        s,
        send(
            &mut ws,
            &json!({"id":1,"type":"subscribe_events","event_type":event}),
        ),
    )
    .await?;
    let ack = bounded(s, read(&mut ws)).await?;
    if ack["type"] != "result" || ack["id"] != 1 || !ack["success"].is_boolean() {
        return Err("HA_WS_RESULT_PROTOCOL".into());
    }
    if ack["success"] != true {
        return Err("HA_WS_SUBSCRIBE_DENIED".into());
    }
    Ok(ws)
}
fn filtered(s: &Session, event: &str, v: &Value) -> Result<Option<Value>, String> {
    if v["type"] != "event"
        || v["id"] != 1
        || v["event"]["event_type"] != event
        || !v["event"]["data"].is_object()
    {
        return Err("HA_WS_EVENT_PROTOCOL".into());
    }
    let e = &v["event"];
    let data = &e["data"];
    if event == "state_changed" {
        let id = data["entity_id"].as_str().ok_or("HA_WS_EVENT_ENTITY")?;
        if s.scope.require_entity(id).is_err() {
            return Ok(None);
        }
        for key in ["old_state", "new_state"] {
            if !data[key].is_null() && data[key]["entity_id"] != id {
                return Err("HA_WS_EVENT_ENTITY".into());
            }
        }
    } else if let Some(id) = data["entity_id"].as_str() {
        if s.scope.require_entity(id).is_err() {
            return Ok(None);
        }
    }
    Ok(Some(s.content(e)))
}
async fn unsubscribe(ws: &mut Socket) -> bool {
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        send(
            ws,
            &json!({"id":2,"type":"unsubscribe_events","subscription":1}),
        )
        .await?;
        for _ in 0..128 {
            let ack = read(ws).await?;
            if ack["type"] == "event" && ack["id"] == 1 {
                continue;
            }
            return Ok::<_, String>(
                ack["type"] == "result" && ack["id"] == 2 && ack["success"] == true,
            );
        }
        Ok(false)
    })
    .await;
    matches!(result, Ok(Ok(true)))
}
async fn run(s: Session, stream: Arc<Stream>, mut ws: Socket) {
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    let deadline = Instant::now() + Duration::from_secs(900);
    let mut last = Instant::now();
    let (reason, clear, stop) = loop {
        tokio::select! {biased;
            _=stream.cancel.cancelled()=>break ("HA_WS_STOPPED".into(),false,true),
            _=tick.tick()=>{
                if let Err(e)=s.scope.check(){break(e,true,false);}
                if Instant::now() >= deadline {break("HA_WS_LEASE_EXPIRED".into(),false,true);}
                if last.elapsed() >= Duration::from_secs(30) {
                    let ping = bounded(&s,async {ws.send(Message::Ping(Vec::new().into())).await.map_err(|_| "HA_WS_TRANSPORT".into())}).await;
                    if let Err(e)=ping {break(e,false,false);}
                    last=Instant::now();
                }
            },
            message=ws.next()=>{
                let v=match message {
                    Some(Ok(Message::Text(t)))=>match serde_json::from_str::<Value>(&t){Ok(v)=>v,Err(_)=>break("HA_WS_MALFORMED_JSON".into(),false,false)},
                    Some(Ok(Message::Ping(_)))=>{if bounded(&s,async{ws.flush().await.map_err(|_|"HA_WS_TRANSPORT".into())}).await.is_err(){break("HA_WS_TRANSPORT".into(),false,false);}continue;},
                    Some(Ok(Message::Pong(_)))=>continue,
                    Some(Ok(Message::Close(_)))|None=>break("HA_WS_DISCONNECTED".into(),false,false),
                    _=>break("HA_WS_FRAME_OR_TRANSPORT".into(),false,false),
                };
                if let Err(e)=s.scope.check(){break(e,true,false);}
                match filtered(&s,&stream.event_type,&v) {
                    Ok(Some(event))=>if let Err(e)=stream.event(event){break(e,false,false)},
                    Ok(None)=>(),Err(e)=>break(e,false,false),
                }
            }
        }
    };
    if stop {
        let ack = unsubscribe(&mut ws).await;
        if let Ok(mut state) = stream.state.lock() {
            state.unsubscribe_acknowledged = ack;
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), ws.close(None)).await;
    stream.finish(&reason, clear);
}
fn snapshot(id: &str, stream: &Stream, poll: Option<usize>) -> Result<Value, String> {
    let mut s = stream.state.lock().map_err(|_| "HA_WS_LOCK")?;
    let mut events = Vec::new();
    if let Some(count) = poll {
        for _ in 0..count {
            if let Some(v) = s.events.pop_front() {
                s.bytes -= v.to_string().len();
                events.push(v)
            } else {
                break;
            }
        }
    }
    Ok(
        json!({"stream_id":id,"event_type":stream.event_type,"status":s.status,"reason":s.reason,
        "events":events,"received":s.received,"dropped":s.dropped,"queued":s.events.len(),"queued_bytes":s.bytes,
        "missed_events_possible":true,"automatic_reconnect":false,"untrusted":true,"unsubscribe_acknowledged":s.unsubscribe_acknowledged}),
    )
}
pub(super) async fn execute(p: Prepared) -> Result<Value, String> {
    let s = p.session.ok_or("HA_PREPARED_SESSION")?;
    let action = p.input["action"].as_str().ok_or("HA_WS_ACTION")?;
    if action == "start" {
        let _serial = s
            .scope
            .guard(async { Ok(s.scope.core.homeassistant_actions.lock().await) })
            .await?;
        {
            let mut streams = s
                .scope
                .core
                .homeassistant_streams
                .lock()
                .map_err(|_| "HA_WS_LOCK")?;
            streams.retain(|_, stream| stream.state.lock().is_ok_and(|s| s.status != "closed"));
            if streams.len() >= 4 {
                return Err("HA_WS_STREAM_LIMIT".into());
            }
        }
        let event = p.input["event_type"]
            .as_str()
            .ok_or("HA_WS_EVENT_REQUIRED")?
            .to_owned();
        let ws = connect(&s, &event).await?;
        s.scope.check()?;
        let id = storage::id();
        let stream = Arc::new(Stream {
            connection: s.scope.connection.clone(),
            scope: scope(&s),
            event_type: event,
            cancel: CancellationToken::new(),
            state: Mutex::new(State {
                status: "active",
                reason: String::new(),
                events: VecDeque::new(),
                bytes: 0,
                received: 0,
                dropped: 0,
                unsubscribe_acknowledged: false,
            }),
        });
        s.scope
            .core
            .homeassistant_streams
            .lock()
            .map_err(|_| "HA_WS_LOCK")?
            .insert(id.clone(), stream.clone());
        let result = snapshot(&id, &stream, None)?;
        drop(_serial);
        tokio::spawn(run(s, stream, ws));
        Ok(result)
    } else {
        Err("HA_WS_START_REQUIRED".into())
    }
}
pub(super) struct LocalControl {
    core: Arc<Core>,
    connection: String,
    id: String,
    action: String,
    count: usize,
    stream: Arc<Stream>,
}
impl LocalControl {
    pub(super) fn new(core: &Arc<Core>, input: &Value) -> Result<Self, String> {
        let connection = input["connection"].as_str().ok_or("HA_CONNECTION")?;
        let id = input["stream_id"].as_str().ok_or("HA_WS_STREAM_REQUIRED")?;
        canonical(connection)?;
        canonical(id)?;
        if input.get("event_type").is_some()
            || (input["action"] != "poll" && input.get("max_events").is_some())
        {
            return Err("HA_WS_CONTROL_FIELDS".into());
        }
        let stream = core
            .homeassistant_streams
            .lock()
            .map_err(|_| "HA_WS_LOCK")?
            .get(id)
            .cloned()
            .ok_or("HA_WS_STREAM_NOT_FOUND")?;
        if stream.connection != connection {
            return Err("HA_WS_STREAM_OUT_OF_SCOPE".into());
        }
        Ok(Self {
            core: core.clone(),
            connection: connection.into(),
            id: id.into(),
            action: input["action"].as_str().ok_or("HA_WS_ACTION")?.into(),
            count: input["max_events"].as_u64().unwrap_or(32) as usize,
            stream,
        })
    }
    pub(super) fn review(&self) -> Value {
        json!({"connection":self.connection,"stream_id":self.id,"action":self.action,"local":true,"reads_credentials":false,"http_or_new_websocket":false})
    }
    pub(super) fn execute(&self) -> Result<Value, String> {
        if self.action == "stop" {
            self.stream.cancel.cancel();
            let mut result = snapshot(&self.id, &self.stream, None)?;
            result["stop_requested"] = json!(true);
            return Ok(result);
        }
        // Metadata/status and local cancellation are usable after credential,
        // profile, privacy or module revocation. Event payloads still require
        // the exact original live scope, without retrieving a token.
        if self.action == "status" {
            return snapshot(&self.id, &self.stream, None);
        }
        let current = Scope::new(&self.core, &self.connection)?;
        if digest(
            &json!({"config":current.raw,"generation":current.generation,"credential_binding":current.credential_binding}),
        ) != self.stream.scope
        {
            return Err("HA_WS_STREAM_SCOPE_CHANGED".into());
        }
        current.check()?;
        let result = snapshot(&self.id, &self.stream, Some(self.count))?;
        current.check()?;
        Ok(result)
    }
}
