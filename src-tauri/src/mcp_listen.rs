//! Explicit modern subscriptions with private active authority and bounded local
//! metadata. Closed records hold neither credentials nor a profile Scope.
use super::*;
use crate::{mcp_request_guard::Guard, mcp_subscription_wire as wire};
use std::{collections::VecDeque, sync::Mutex};
use tokio_util::sync::CancellationToken;

const MAX_ACTIVE: usize = 4;
const MAX_CLOSED: usize = 32;
const QUEUE_EVENTS: usize = 128;
const QUEUE_BYTES: usize = 256 * 1024;
pub(crate) const LOCAL_TOOLS: &[&str] =
    &["mcp.listen_status", "mcp.listen_poll", "mcp.listen_stop"];
pub(crate) fn local(tool: &str) -> bool {
    LOCAL_TOOLS.contains(&tool)
}
pub(crate) fn passive(tool: &str) -> bool {
    matches!(tool, "mcp.listen_status" | "mcp.listen_stop")
}

struct State {
    status: &'static str,
    reason: String,
    honored: Value,
    events: VecDeque<Value>,
    bytes: usize,
    received: u64,
    lost: u64,
    closed_at: Option<Instant>,
}
pub(crate) struct Stream {
    id: String,
    connection: String,
    requested: Value,
    state: Mutex<State>,
    guard: Mutex<Option<Guard>>,
    cancel: CancellationToken,
    done: tokio::sync::Notify,
}
fn summary(filter: &wire::Filter) -> Value {
    json!({"toolsListChanged":filter.tools_list_changed,"resourcesListChanged":filter.resources_list_changed,"promptsListChanged":filter.prompts_list_changed,"resourceSubscriptionCount":filter.resource_subscriptions.len()})
}
fn reason(code: &str) -> String {
    if code.starts_with("MCP_")
        && code.len() <= 128
        && code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    {
        code.into()
    } else {
        "MCP_LISTEN_AUTHORITY_OR_TRANSPORT_CLOSED".into()
    }
}
impl Stream {
    #[cfg(test)]
    pub(crate) fn retains_authority(&self) -> bool {
        self.guard.lock().unwrap().is_some()
    }
    fn finish(&self, code: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.lost = state.lost.saturating_add(state.events.len() as u64);
            state.events.clear();
            state.bytes = 0;
            state.status = "closed";
            state.reason = reason(code);
            state.closed_at = Some(Instant::now());
        }
        if let Ok(mut guard) = self.guard.lock() {
            guard.take();
        }
        self.done.notify_waiters();
    }
    fn stop(&self) {
        if let Ok(mut state) = self.state.lock() {
            if state.status != "closed" {
                state.status = "stopping";
                state.lost = state.lost.saturating_add(state.events.len() as u64);
                state.events.clear();
                state.bytes = 0;
            }
        }
        if let Ok(mut guard) = self.guard.lock() {
            guard.take();
        }
        self.cancel.cancel();
    }
    fn status(&self) -> Result<Value, String> {
        let state = self.state.lock().map_err(|_| "MCP_LISTEN_LOCK")?;
        Ok(
            json!({"stream_id":self.id,"connection":self.connection,"status":state.status,"reason":state.reason,"requested":self.requested,"honored":state.honored,"queued":state.events.len(),"received":state.received,"lost":state.lost,"automatic_reconnect":false,"history_gap":true}),
        )
    }
    fn event(&self, value: Value) -> Result<(), String> {
        let bytes = value.to_string().len();
        if bytes > wire::FRAME_LIMIT {
            return Err("MCP_LISTEN_EVENT_LIMIT".into());
        }
        let mut state = self.state.lock().map_err(|_| "MCP_LISTEN_LOCK")?;
        if state.status != "active" {
            return Err("MCP_LISTEN_STOPPED".into());
        }
        while state.events.len() >= QUEUE_EVENTS || state.bytes + bytes > QUEUE_BYTES {
            let old = state.events.pop_front().ok_or("MCP_LISTEN_QUEUE_LIMIT")?;
            state.bytes -= old.to_string().len();
            state.lost = state.lost.saturating_add(1);
        }
        state.received = state
            .received
            .checked_add(1)
            .ok_or("MCP_LISTEN_EVENT_LIMIT")?;
        state.bytes += bytes;
        state.events.push_back(value);
        Ok(())
    }
}

pub(crate) fn available(core: &Core) -> bool {
    core.mcp_streams
        .lock()
        .is_ok_and(|streams| !streams.is_empty())
}
pub(crate) fn stop_all(core: &Core) {
    if let Ok(streams) = core.mcp_streams.lock() {
        for stream in streams.values() {
            stream.stop();
        }
    }
}
pub(super) struct Reservation {
    stream: Arc<Stream>,
    armed: bool,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if self.armed {
            self.stream.finish(if self.stream.cancel.is_cancelled() {
                "MCP_LISTEN_STOPPED"
            } else {
                "MCP_LISTEN_START_CANCELLED"
            });
        }
    }
}
pub(super) fn reserve(
    core: &Core,
    connection: &str,
    filter: &wire::Filter,
) -> Result<Reservation, String> {
    let mut streams = core.mcp_streams.lock().map_err(|_| "MCP_LISTEN_REGISTRY")?;
    let mut active = 0;
    let mut closed = vec![];
    for (id, stream) in streams.iter() {
        let state = stream.state.lock().map_err(|_| "MCP_LISTEN_LOCK")?;
        if let Some(at) = state.closed_at {
            closed.push((at, id.clone()));
        } else {
            active += 1;
        }
    }
    if active >= MAX_ACTIVE {
        return Err("MCP_LISTEN_CAPACITY".into());
    }
    closed.sort_by_key(|(at, _)| *at);
    for (_, id) in closed
        .iter()
        .take(closed.len().saturating_sub(MAX_CLOSED - 1))
    {
        streams.remove(id);
    }
    let stream = Arc::new(Stream {
        id: crate::storage::id(),
        connection: connection.into(),
        requested: summary(filter),
        state: Mutex::new(State {
            status: "starting",
            reason: String::new(),
            honored: Value::Null,
            events: VecDeque::new(),
            bytes: 0,
            received: 0,
            lost: 0,
            closed_at: None,
        }),
        guard: Mutex::new(None),
        cancel: CancellationToken::new(),
        done: tokio::sync::Notify::new(),
    });
    streams.insert(stream.id.clone(), stream.clone());
    Ok(Reservation {
        stream,
        armed: true,
    })
}
fn private(
    frame: &Value,
    material: &credentials::Material,
    grant: Option<&crate::mcp_auth::Grant>,
) -> bool {
    contains_private(frame, &material.needles)
        || grant.is_some_and(|grant| grant.contains_private(frame))
}
struct Completion {
    stream: Arc<Stream>,
    armed: bool,
}
impl Drop for Completion {
    fn drop(&mut self) {
        if self.armed {
            self.stream.finish("MCP_LISTEN_TASK_CANCELLED");
        }
    }
}

pub(super) async fn start(
    p: &Prepared,
    mut reservation: Reservation,
    session: client::transport::Session,
    material: credentials::Material,
    grant: Option<Arc<crate::mcp_auth::Grant>>,
) -> Result<Value, String> {
    let filter = wire::Filter::parse(&p.input["notifications"], true)?;
    if private(&p.input["notifications"], &material, grant.as_deref()) {
        return Err("MCP_CREDENTIAL_CONTENT_DENIED".into());
    }
    let guard = material
        .guard
        .as_ref()
        .ok_or("MCP_LISTEN_GUARD_REQUIRED")?
        .clone();
    let id = json!(format!("listen-{}", reservation.stream.id));
    let mut protocol = wire::Protocol::new(id.clone(), filter.clone())?;
    let mut listening = Guard::run(Some(&guard), async {
        tokio::select! { biased;
        _ = reservation.stream.cancel.cancelled() => Err("MCP_LISTEN_STOPPED".to_string()),
        value = tokio::time::timeout(
            Duration::from_secs(30),
            session.listen(
                id,
                serde_json::to_value(&filter).map_err(|_| "MCP_LISTEN_FILTER")?,
                guard.clone(),
            ),
        )
        => value.map_err(|_| "MCP_LISTEN_START_TIMEOUT".to_string())?,
        }
    })
    .await?;
    let acknowledged = Guard::run(Some(&guard), async {
        tokio::select! { biased;
            _ = reservation.stream.cancel.cancelled() => Err("MCP_LISTEN_STOPPED".to_string()),
            value = tokio::time::timeout(Duration::from_secs(30), listening.next()) => value.map_err(|_| "MCP_LISTEN_ACK_TIMEOUT".to_string())?,
        }
    })
    .await;
    let honored = match acknowledged.and_then(|frame| {
        if private(&frame, &material, grant.as_deref()) {
            return Err("MCP_CREDENTIAL_CONTENT_DENIED".into());
        }
        match protocol.accept(&frame)? {
            wire::Event::Acknowledged(filter) => Ok(filter),
            _ => Err("MCP_LISTEN_ACK_REQUIRED".into()),
        }
    }) {
        Ok(filter) => filter,
        Err(error) => {
            listening.close().await;
            return Err(error);
        }
    };
    {
        let mut state = reservation
            .stream
            .state
            .lock()
            .map_err(|_| "MCP_LISTEN_LOCK")?;
        state.honored = summary(&honored);
    }
    if honored.is_empty() {
        listening.close().await;
        reservation.stream.finish("MCP_LISTEN_EMPTY_HONORED_FILTER");
        reservation.armed = false;
        let mut result = reservation.stream.status()?;
        result["notifications"] = serde_json::to_value(honored).map_err(|_| "MCP_LISTEN_FILTER")?;
        return Ok(result);
    }
    if let Err(error) = guard.check() {
        listening.close().await;
        return Err(error);
    }
    let activating = {
        let mut state = reservation
            .stream
            .state
            .lock()
            .map_err(|_| "MCP_LISTEN_LOCK")?;
        if state.status == "starting" && !reservation.stream.cancel.is_cancelled() {
            state.status = "active";
            true
        } else {
            false
        }
    };
    if !activating {
        listening.close().await;
        return Err("MCP_LISTEN_STOPPED".into());
    }
    *reservation
        .stream
        .guard
        .lock()
        .map_err(|_| "MCP_LISTEN_LOCK")? = Some(guard.clone());
    if reservation.stream.cancel.is_cancelled() {
        listening.close().await;
        return Err("MCP_LISTEN_STOPPED".into());
    }
    let stream = reservation.stream.clone();
    let scope = p.scope.clone();
    let result = {
        let mut value = stream.status()?;
        value["notifications"] = serde_json::to_value(&honored).map_err(|_| "MCP_LISTEN_FILTER")?;
        value
    };
    reservation.armed = false;
    tokio::spawn(async move {
        let mut completion = Completion {
            stream: stream.clone(),
            armed: true,
        };
        let until = tokio::time::sleep(Duration::from_secs(900));
        tokio::pin!(until);
        let code = loop {
            let received = tokio::select! {biased;
                _=stream.cancel.cancelled()=>break "MCP_LISTEN_STOPPED".to_string(),
                _=&mut until=>break "MCP_LISTEN_LEASE_EXPIRED".to_string(),
                value=Guard::run(Some(&guard),listening.next())=>value,
            };
            let frame = match received {
                Ok(frame) => frame,
                Err(error) => {
                    let code = if matches!(error.as_str(), "MCP_EXIT" | "MCP_LISTEN_ABRUPT_EOF") {
                        protocol.eof().err().unwrap_or(error)
                    } else {
                        error
                    };
                    break reason(&code);
                }
            };
            if private(&frame, &material, grant.as_deref()) {
                break "MCP_CREDENTIAL_CONTENT_DENIED".to_string();
            }
            if let Err(error) = guard.check() {
                break reason(&error);
            }
            let event = match protocol.accept(&frame) {
                Ok(event) => event,
                Err(error) => break reason(&error),
            };
            let (category, metadata) = match event {
                wire::Event::ToolsChanged => {
                    (catalog::Category::Tools, json!({"type":"tools_changed"}))
                }
                wire::Event::PromptsChanged => (
                    catalog::Category::Prompts,
                    json!({"type":"prompts_changed"}),
                ),
                wire::Event::ResourcesChanged => (
                    catalog::Category::Resources,
                    json!({"type":"resources_changed"}),
                ),
                wire::Event::ResourceUpdated(uri) => (
                    catalog::Category::Resources,
                    json!({"type":"resource_updated","uri":uri}),
                ),
                wire::Event::Complete => {
                    listening.completed();
                    break "MCP_LISTEN_COMPLETE".to_string();
                }
                wire::Event::Acknowledged(_) => break "MCP_LISTEN_DUPLICATE_ACK".to_string(),
            };
            if let Err(error) = catalog::invalidate(&scope.core, &scope.id, category) {
                break reason(&error);
            }
            if let Err(error) = guard.check() {
                break reason(&error);
            }
            if let Err(error) = stream.event(metadata) {
                break reason(&error);
            }
        };
        listening.close().await;
        drop(material);
        drop(grant);
        drop(scope);
        drop(guard);
        stream.finish(&code);
        completion.armed = false;
    });
    p.scope.check()?;
    Ok(result)
}

// A drained batch is lost if authority/closure validation fails before return.
// Committing means returned locally, not guaranteed delivery over the bridge.
struct PollReceipt {
    stream: Arc<Stream>,
    count: usize,
    committed: bool,
}
impl Drop for PollReceipt {
    fn drop(&mut self) {
        if !self.committed {
            if let Ok(mut state) = self.stream.state.lock() {
                state.lost = state.lost.saturating_add(self.count as u64);
            }
        }
    }
}
pub(crate) enum Control {
    Status(Vec<Arc<Stream>>),
    Poll(Arc<Stream>, Guard, usize),
    Stop(Arc<Stream>),
}
impl Control {
    pub(crate) fn review(&self) -> Value {
        match self {
            Self::Status(streams) => {
                json!({"local_operation":"status","streams":streams.iter().map(|s|&s.id).collect::<Vec<_>>()})
            }
            Self::Poll(stream, _, max) => {
                json!({"local_operation":"poll","stream_id":stream.id,"max_events":max})
            }
            Self::Stop(stream) => {
                json!({"local_operation":"stop","stream_id":stream.id,"network_business_calls":false})
            }
        }
    }
    pub(crate) fn risk(&self) -> Risk {
        Risk::Free
    }
    pub(crate) async fn execute(self) -> Result<Value, String> {
        match self {
            Self::Status(streams) => Ok(
                json!({"streams":streams.iter().map(|stream|stream.status()).collect::<Result<Vec<_>,_>>()?}),
            ),
            Self::Stop(stream) => {
                let done = stream.done.notified();
                tokio::pin!(done);
                done.as_mut().enable();
                stream.stop();
                let waiting =
                    stream.state.lock().map_err(|_| "MCP_LISTEN_LOCK")?.status != "closed";
                if waiting {
                    let _ = tokio::time::timeout(Duration::from_secs(3), done).await;
                }
                stream.status()
            }
            Self::Poll(stream, guard, max) => {
                guard.check()?;
                let events = {
                    let mut state = stream.state.lock().map_err(|_| "MCP_LISTEN_LOCK")?;
                    if state.status != "active" {
                        return Err("MCP_LISTEN_NOT_ACTIVE".into());
                    }
                    let mut events = vec![];
                    for _ in 0..max {
                        let Some(event) = state.events.pop_front() else {
                            break;
                        };
                        state.bytes -= event.to_string().len();
                        events.push(event);
                    }
                    events
                };
                let mut receipt = PollReceipt {
                    stream: stream.clone(),
                    count: events.len(),
                    committed: false,
                };
                guard.check()?;
                let mut value = stream.status()?;
                value["events"] = json!(events);
                if stream.cancel.is_cancelled()
                    || value["status"] != "active"
                    || stream
                        .guard
                        .lock()
                        .map_err(|_| "MCP_LISTEN_LOCK")?
                        .is_none()
                {
                    return Err("MCP_LISTEN_NOT_ACTIVE".into());
                }
                guard.check()?;
                receipt.committed = true;
                Ok(value)
            }
        }
    }
}
pub(crate) fn prepare_control(
    core: &Arc<Core>,
    tool: &str,
    input: &Value,
) -> Result<Control, String> {
    if !local(tool)
        || !jsonschema::validator_for(&client::tool_schema(tool))
            .map_err(|_| "MCP_TOOL_SCHEMA")?
            .is_valid(input)
    {
        return Err("MCP_LISTEN_LOCAL_INPUT".into());
    }
    let streams = core.mcp_streams.lock().map_err(|_| "MCP_LISTEN_REGISTRY")?;
    if tool == "mcp.listen_status" {
        let connection = input["connection"].as_str();
        let id = input["stream_id"].as_str();
        let selected = streams
            .values()
            .filter(|stream| {
                connection.is_none_or(|id| stream.connection == id)
                    && id.is_none_or(|id| stream.id == id)
            })
            .cloned()
            .collect::<Vec<_>>();
        return Ok(Control::Status(selected));
    }
    let id = input["stream_id"]
        .as_str()
        .ok_or("MCP_LISTEN_STREAM_REQUIRED")?;
    credentials::canonical(id)?;
    let stream = streams
        .get(id)
        .filter(|stream| input["connection"] == stream.connection)
        .ok_or("MCP_LISTEN_STREAM_UNKNOWN")?
        .clone();
    drop(streams);
    if tool == "mcp.listen_stop" {
        return Ok(Control::Stop(stream));
    }
    let guard = stream
        .guard
        .lock()
        .map_err(|_| "MCP_LISTEN_LOCK")?
        .clone()
        .ok_or("MCP_LISTEN_NOT_ACTIVE")?;
    guard.check()?;
    Ok(Control::Poll(
        stream,
        guard,
        input["max_events"].as_u64().unwrap_or(32) as usize,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn failed_poll_after_drain_discards_payload_and_accounts_for_loss() {
        for fail_at in [2, 3, 4] {
            let checks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counter = checks.clone();
            let guard = Guard::new(move || {
                if counter.fetch_add(1, Ordering::SeqCst) + 1 == fail_at {
                    Err("MCP_OWNED_POLL_REVOKED".into())
                } else {
                    Ok(())
                }
            });
            let stream = Arc::new(Stream {
                id: crate::storage::id(),
                connection: crate::storage::id(),
                requested: json!({}),
                state: Mutex::new(State {
                    status: "active",
                    reason: String::new(),
                    honored: json!({}),
                    events: VecDeque::new(),
                    bytes: 0,
                    received: 0,
                    lost: 0,
                    closed_at: None,
                }),
                guard: Mutex::new(Some(guard.clone())),
                cancel: CancellationToken::new(),
                done: tokio::sync::Notify::new(),
            });
            stream
                .event(json!({"type":"resource_updated","uri":"owned:///one"}))
                .unwrap();
            let result = Control::Poll(stream.clone(), guard, 1).execute().await;
            let status = stream.status().unwrap();
            assert_eq!(status["queued"], 0);
            assert_eq!(status["received"], 1);
            if fail_at < 4 {
                assert_eq!(result.unwrap_err(), "MCP_OWNED_POLL_REVOKED");
                assert_eq!(status["lost"], 1);
            } else {
                assert_eq!(
                    result.unwrap()["events"],
                    json!([{"type":"resource_updated","uri":"owned:///one"}])
                );
                assert_eq!(status["lost"], 0);
            }
            stream.finish("MCP_OWNED_TEST_COMPLETE");
            assert!(!stream.retains_authority());
        }
    }
}
