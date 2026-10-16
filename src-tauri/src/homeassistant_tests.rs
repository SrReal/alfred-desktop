use super::*;
use axum::{
    body::Body,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Request, State,
    },
    http::Response,
    response::IntoResponse,
    Router,
};
use std::{path::PathBuf, sync::Mutex};
const TOKEN: &str = "owned_fictional_ha_token";
fn state(id: &str) -> Value {
    json!({"entity_id":id,"state":"off","attributes":{"friendly_name":"Señor 🐾 private state","access_token":"owned_fictional_camera_token","entity_picture":"/api/camera_proxy/camera.fixture?token=private"},"last_changed":"2026-10-04T00:00:00Z","last_updated":"2026-10-04T00:00:00Z"})
}
fn event(id: &str) -> Value {
    json!({"id":1,"type":"event","event":{"event_type":"state_changed","time_fired":"2026-10-04T00:00:00Z","data":{"entity_id":id,"old_state":null,"new_state":state(id)}}})
}
struct Mock {
    states: BTreeMap<String, Value>,
    services: Value,
    seen: Vec<Value>,
    status: Option<u16>,
    malformed: bool,
    delay: bool,
    delay_write: bool,
    camera_bad: bool,
    ws_mode: String,
    ws_events: Vec<Value>,
    unsubscribe: bool,
    started: Arc<tokio::sync::Notify>,
}
async fn handler(State(m): State<Arc<Mutex<Mock>>>, r: Request) -> Response<Body> {
    let return_response = r.uri().query() == Some("return_response");
    let method = r.method().to_string();
    let path = r.uri().path().to_owned();
    let auth = r
        .headers()
        .get("authorization")
        .is_some_and(|s| s == format!("Bearer {TOKEN}").as_str());
    let bytes = axum::body::to_bytes(r.into_body(), BODY_LIMIT + 1024)
        .await
        .unwrap();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    let (delay, notify) = {
        let mut s = m.lock().unwrap();
        s.seen
            .push(json!({"method":method,"path":path,"body":body,"fictional_auth_matched":auth}));
        (
            s.delay || (s.delay_write && method == "POST"),
            s.started.clone(),
        )
    };
    if delay {
        notify.notify_one();
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let mut s = m.lock().unwrap();
    if let Some(status) = s.status {
        return Response::builder()
            .status(status)
            .header("retry-after", "1")
            .header("location", "http://127.0.0.1:1/should-never-follow")
            .body(Body::from("private provider failure body"))
            .unwrap();
    }
    if s.malformed {
        return Response::builder()
            .body(Body::from("broken private JSON"))
            .unwrap();
    }
    if path.starts_with("/api/camera_proxy/") {
        let bytes = if s.camera_bad {
            b"not an image".to_vec()
        } else {
            let mut out = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(3, 2)
                .write_to(&mut out, image::ImageFormat::Jpeg)
                .unwrap();
            out.into_inner()
        };
        return Response::builder()
            .header("content-type", "image/jpeg")
            .body(Body::from(bytes))
            .unwrap();
    }
    let value = match (method.as_str(), path.as_str()) {
        ("GET", "/api/states") => json!(s.states.values().collect::<Vec<_>>()),
        ("GET", p) if p.starts_with("/api/states/") => s
            .states
            .get(p.trim_start_matches("/api/states/"))
            .cloned()
            .unwrap_or(Value::Null),
        ("GET", "/api/services") => s.services.clone(),
        ("GET", "/api/events") => {
            json!([{"event":"state_changed","listener_count":1},{"event":"private_foreign_event","listener_count":2}])
        }
        ("POST", p) if p.starts_with("/api/services/") => {
            if let Some(ids) = body["entity_id"].as_array() {
                for id in ids {
                    if let Some(v) = s.states.get_mut(id.as_str().unwrap()) {
                        v["state"] = json!("on");
                        v["last_updated"] = json!("2026-10-04T00:00:01Z");
                    }
                }
            }
            if return_response {
                json!({"changed_states":s.states.values().collect::<Vec<_>>(),"service_response":{"owned_result":"fixture private response"}})
            } else {
                json!(s.states.values().collect::<Vec<_>>())
            }
        }
        _ => return Response::builder().status(404).body(Body::empty()).unwrap(),
    };
    Response::builder()
        .header("content-type", "application/json")
        .body(Body::from(value.to_string()))
        .unwrap()
}
async fn ws_upgrade(ws: WebSocketUpgrade, State(m): State<Arc<Mutex<Mock>>>) -> impl IntoResponse {
    ws.on_upgrade(move |ws| ws_server(ws, m))
}
async fn ws_server(mut ws: WebSocket, m: Arc<Mutex<Mock>>) {
    let (mode, events) = {
        let s = m.lock().unwrap();
        (s.ws_mode.clone(), s.ws_events.clone())
    };
    let send = |v: Value| Message::Text(v.to_string().into());
    if ws
        .send(send(
            json!({"type":"auth_required","ha_version":"owned_fixture"}),
        ))
        .await
        .is_err()
    {
        return;
    }
    let Some(Ok(Message::Text(auth))) = ws.recv().await else {
        return;
    };
    let v: Value = serde_json::from_str(&auth).unwrap();
    let matched = v["type"] == "auth" && v["access_token"] == TOKEN;
    m.lock()
        .unwrap()
        .seen
        .push(json!({"method":"WS_AUTH","fictional_auth_matched":matched}));
    if mode == "auth_invalid" {
        let _ = ws
            .send(send(
                json!({"type":"auth_invalid","message":"private failure"}),
            ))
            .await;
        return;
    }
    if ws.send(send(json!({"type":"auth_ok"}))).await.is_err() {
        return;
    }
    let Some(Ok(Message::Text(sub))) = ws.recv().await else {
        return;
    };
    let sub: Value = serde_json::from_str(&sub).unwrap();
    m.lock().unwrap().seen.push(sub.clone());
    let id = if mode == "wrong_id" { 99 } else { 1 };
    if ws
        .send(send(
            json!({"id":id,"type":"result","success":mode!="denied","result":null}),
        ))
        .await
        .is_err()
    {
        return;
    }
    for v in events {
        if ws.send(send(v)).await.is_err() {
            return;
        }
    }
    while let Some(Ok(message)) = ws.recv().await {
        match message {
            Message::Text(t) => {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v["type"] == "unsubscribe_events" {
                    m.lock().unwrap().unsubscribe = v["id"] == 2 && v["subscription"] == 1;
                    let _ = ws
                        .send(send(
                            json!({"id":2,"type":"result","success":true,"result":null}),
                        ))
                        .await;
                }
            }
            Message::Ping(p) => {
                let _ = ws.send(Message::Pong(p)).await;
            }
            Message::Close(_) => break,
            _ => (),
        }
    }
}
struct Fixture {
    core: Arc<Core>,
    root: PathBuf,
    identity: String,
    id: String,
    m: Arc<Mutex<Mock>>,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new(read_only: bool) -> Self {
        let ids = [
            "light.fixture",
            "lock.fixture",
            "automation.fixture",
            "camera.fixture",
            "light.foreign",
        ];
        let services = json!([{"domain":"light","services":{"turn_on":{"fields":{"brightness":{}},"description":"private service description"}}},{"domain":"lock","services":{"unlock":{"fields":{}}}},{"domain":"automation","services":{"trigger":{"fields":{}},"turn_on":{"fields":{}},"turn_off":{"fields":{}},"toggle":{"fields":{}},"reload":{"fields":{}}}}]);
        let m = Arc::new(Mutex::new(Mock {
            states: ids.into_iter().map(|id| (id.into(), state(id))).collect(),
            services,
            seen: Vec::new(),
            status: None,
            malformed: false,
            delay: false,
            delay_write: false,
            camera_bad: false,
            ws_mode: String::new(),
            ws_events: vec![event("light.foreign"), event("light.fixture")],
            unsubscribe: false,
            started: Arc::new(tokio::sync::Notify::new()),
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new()
            .route("/api/websocket", axum::routing::get(ws_upgrade))
            .fallback(handler)
            .with_state(m.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let root = std::env::temp_dir().join(format!("ap-ha-{}", storage::id()));
        std::fs::create_dir(&root).unwrap();
        let identity =
            crate::asset_files::identity(&crate::asset_files::directory(&root).unwrap()).unwrap();
        let core = Arc::new(
            Core::new(
                root.join("profile"),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        core.store
            .execute(
                "UPDATE module_configs SET enabled=1 WHERE module_id='homeassistant'",
                &[],
            )
            .unwrap();
        let mut grants = serde_json::Map::new();
        for name in [
            "light.turn_on",
            "lock.unlock",
            "automation.trigger",
            "automation.turn_on",
            "automation.turn_off",
            "automation.toggle",
            "automation.reload",
        ] {
            grants.insert(name.into(),json!({"global":name=="automation.reload","input_schema":{"type":"object","properties":{"brightness":{"type":"integer","minimum":0,"maximum":255},"skip_condition":{"type":"boolean"},"stop_actions":{"type":"boolean"}},"additionalProperties":false}}));
        }
        let cfg = json!({"profile_version":1,"base_url":format!("http://{address}"),"read_only":read_only,"websocket":true,"allowed_entities":&ids[..4],"allowed_services":grants,"allowed_events":["state_changed"]});
        let saved = integrations::save(
            &core,
            &json!({"module_id":"homeassistant","name":"Owned HA fixture","config":cfg}),
        )
        .await
        .unwrap();
        *core.homeassistant_fixture.lock().unwrap() = Some(TestTransport {
            token: Some(TOKEN.into()),
            headless_permission: true,
        });
        Self {
            core,
            root,
            identity,
            id: saved["id"].as_str().unwrap().into(),
            m,
            server,
        }
    }
    fn input(&self, v: Value) -> Value {
        let mut input = json!({"connection":self.id});
        for (k, v) in v.as_object().unwrap() {
            input[k] = v.clone();
        }
        input
    }
    async fn run(&self, tool: &str, v: Value) -> Result<Value, String> {
        self.core.execute(tool, self.input(v)).await
    }
    async fn draft(&self, tool: &str, v: Value) -> Prepared {
        prepare(&self.core, tool, &self.input(v)).await.unwrap()
    }
    fn calls(&self) -> usize {
        self.m
            .lock()
            .unwrap()
            .seen
            .iter()
            .filter(|v| v["method"] == "POST")
            .count()
    }
    fn call() -> Value {
        json!({"action":"inspect","domain":"light","service":"turn_on","entity_ids":["light.fixture"],"body":{"brightness":100}})
    }
    async fn observed_call(&self) -> Value {
        let mut v = Self::call();
        let observed = self
            .run("homeassistant.call_service", v.clone())
            .await
            .unwrap();
        v["action"] = json!("call");
        v["expected"] = observed["expected"].clone();
        v
    }
    async fn approve(
        &self,
        tool: &str,
        v: Value,
        answer: &str,
        change: impl FnOnce(),
    ) -> Result<Value, String> {
        let core = self.core.clone();
        let input = self.input(v);
        let tool = tool.to_owned();
        let task = tokio::spawn(async move { core.execute(&tool, input).await });
        let sender = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let id = self.core.pending.lock().unwrap().keys().next().cloned();
                if let Some(id) = id {
                    break self.core.pending.lock().unwrap().remove(&id).unwrap();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        change();
        sender.send(answer.into()).unwrap();
        task.await.unwrap()
    }
    async fn start(&self) -> Value {
        self.approve(
            "homeassistant.websocket",
            json!({"action":"start","event_type":"state_changed"}),
            "allow",
            || (),
        )
        .await
        .unwrap()
    }
    async fn wait_stream(&self, id: &Value, closed: bool) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let v = self
                    .run(
                        "homeassistant.websocket",
                        json!({"action":"status","stream_id":id}),
                    )
                    .await
                    .unwrap();
                if (closed && v["status"] == "closed")
                    || (!closed && v["received"].as_u64().unwrap_or(0) > 0)
                {
                    break v;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }
    async fn clean(self) {
        for stream in self.core.homeassistant_streams.lock().unwrap().values() {
            stream.cancel.cancel();
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        self.core.homeassistant_streams.lock().unwrap().clear();
        self.server.abort();
        let _ = self.server.await;
        let root = self.root.canonicalize().unwrap();
        assert_eq!(
            root.parent().unwrap(),
            std::env::temp_dir().canonicalize().unwrap()
        );
        assert!(root
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("ap-ha-"));
        assert_eq!(
            crate::asset_files::identity(&crate::asset_files::directory(&root).unwrap()).unwrap(),
            self.identity
        );
        drop(self.core);
        std::fs::remove_dir_all(root).unwrap();
    }
}
#[tokio::test]
async fn homeassistant_reads_filter_scopes_and_omit_camera_credentials() {
    let f = Fixture::new(true).await;
    for tool in [
        "homeassistant.entities",
        "homeassistant.states",
        "homeassistant.automations",
    ] {
        let v = f.run(tool, json!({})).await.unwrap();
        assert!(!v.to_string().contains("light.foreign"));
        assert!(!v.to_string().contains("owned_fictional_camera_token"));
        assert!(!v.to_string().contains("entity_picture"));
        assert_eq!(v["untrusted"], true);
        if tool == "homeassistant.automations" {
            assert_eq!(v["items"].as_array().unwrap().len(), 1);
        }
    }
    let v = f.run("homeassistant.services", json!({})).await.unwrap();
    assert!(v["services"]["lock.unlock"].is_object());
    let v = f.run("homeassistant.events", json!({})).await.unwrap();
    assert_eq!(v["live_stream"], false);
    assert_eq!(v["items"].as_array().unwrap().len(), 1);
    assert!(f
        .m
        .lock()
        .unwrap()
        .seen
        .iter()
        .all(|v| v["fictional_auth_matched"] == true));
    f.clean().await;
}
#[tokio::test]
async fn homeassistant_read_only_and_unknown_or_implicit_targets_block_before_write() {
    let f = Fixture::new(true).await;
    let v = f.observed_call().await;
    assert_eq!(
        f.run("homeassistant.call_service", v).await.unwrap_err(),
        "HA_READ_ONLY"
    );
    for body in [
        json!({"entity_id":"all"}),
        json!({"nested":{"area_id":"everywhere"}}),
        json!({"brightness":999}),
    ] {
        let mut v = Fixture::call();
        v["body"] = body;
        assert!(prepare(&f.core, "homeassistant.call_service", &f.input(v))
            .await
            .is_err());
    }
    let mut v = Fixture::call();
    v["entity_ids"] = json!(["light.foreign"]);
    assert!(prepare(&f.core, "homeassistant.call_service", &f.input(v))
        .await
        .is_err());
    assert_eq!(f.calls(), 0);
    f.clean().await;
}
#[tokio::test]
async fn homeassistant_mandatory_denial_session_denial_and_allow_are_real_oneshot_paths() {
    let f = Fixture::new(false).await;
    let v = f.observed_call().await;
    for answer in ["deny", "session"] {
        assert_eq!(
            f.approve("homeassistant.call_service", v.clone(), answer, || ())
                .await
                .unwrap_err(),
            "PERMISSION_DENIED_OR_TIMEOUT"
        );
        assert_eq!(f.calls(), 0);
    }
    let result = f
        .approve("homeassistant.call_service", v, "allow", || ())
        .await
        .unwrap();
    assert_eq!(result["acknowledged"], true);
    assert_eq!(result["physical_outcome"], "UNVERIFIED");
    assert_eq!(result["states"][0]["state"], "on");
    assert!(!result.to_string().contains("light.foreign"));
    assert_eq!(f.calls(), 1);
    let rows=f.core.store.query("SELECT input_summary,output_summary FROM activity_events WHERE tool_id LIKE 'homeassistant.%'",&[]).unwrap().to_string();
    for secret in [
        TOKEN,
        "private state",
        "private service description",
        "brightness",
        "light.fixture",
    ] {
        assert!(!rows.contains(secret), "{secret}");
    }
    f.clean().await;
}
#[tokio::test]
async fn homeassistant_sensitive_lock_and_indirect_automation_actions_cannot_reduce_risk() {
    let f = Fixture::new(false).await;
    for (tool, mut v) in [
        (
            "homeassistant.call_service",
            json!({"action":"inspect","domain":"lock","service":"unlock","entity_ids":["lock.fixture"],"body":{}}),
        ),
        (
            "homeassistant.automations",
            json!({"action":"inspect","entity_id":"automation.fixture","service":"trigger","body":{"skip_condition":false}}),
        ),
        (
            "homeassistant.automations",
            json!({"action":"inspect","service":"reload","body":{}}),
        ),
    ] {
        let obs = f.run(tool, v.clone()).await.unwrap();
        v["action"] = json!("call");
        v["expected"] = obs["expected"].clone();
        let p = f.draft(tool, v).await;
        assert_eq!(p.risk, Risk::Mandatory);
    }
    f.clean().await;
}
#[tokio::test]
async fn homeassistant_fresh_state_service_and_connection_are_rechecked_after_review() {
    let f = Fixture::new(false).await;
    let v = f.observed_call().await;
    assert_eq!(
        f.approve("homeassistant.call_service", v, "allow", || {
            f.m.lock().unwrap().states.get_mut("light.fixture").unwrap()["state"] = json!("on");
        })
        .await
        .unwrap_err(),
        "HA_OBSERVATION_CHANGED_REVIEW_REQUIRED"
    );
    assert_eq!(f.calls(), 0);
    let v = f.observed_call().await;
    assert_eq!(
        f.approve("homeassistant.call_service", v, "allow", || {
            f.core
                .store
                .set_setting(
                    &format!("homeassistant.connection_generation.{}", f.id),
                    &json!(storage::id()),
                )
                .unwrap();
        })
        .await
        .unwrap_err(),
        "HA_CONNECTION_CHANGED"
    );
    assert_eq!(f.calls(), 0);
    f.clean().await;
}
#[tokio::test]
async fn homeassistant_privacy_and_disable_cancel_owned_inflight_request() {
    for privacy in [true, false] {
        let f = Fixture::new(false).await;
        f.m.lock().unwrap().delay = true;
        let notify = f.m.lock().unwrap().started.clone();
        let core = f.core.clone();
        let input = f.input(json!({}));
        let task = tokio::spawn(async move { core.execute("homeassistant.states", input).await });
        notify.notified().await;
        if privacy {
            f.core.store.set_setting("privacy", &json!(true)).unwrap();
        } else {
            f.core
                .store
                .execute(
                    "UPDATE module_configs SET enabled=0 WHERE module_id='homeassistant'",
                    &[],
                )
                .unwrap();
        }
        let e = task.await.unwrap().unwrap_err();
        assert!(e == "PRIVACY_DENIED" || e == "MODULE_DISABLED: homeassistant");
        assert_eq!(f.calls(), 0);
        f.clean().await;
    }
}
#[tokio::test]
async fn homeassistant_errors_are_bounded_sanitized_and_never_redirect_or_retry() {
    let f = Fixture::new(false).await;
    for status in [302, 401, 403, 404, 429, 500] {
        f.m.lock().unwrap().status = Some(status);
        let before = f.m.lock().unwrap().seen.len();
        let e = f.run("homeassistant.states", json!({})).await.unwrap_err();
        assert_eq!(e, format!("HA_HTTP_STATUS_{status}"));
        assert_eq!(f.m.lock().unwrap().seen.len(), before + 1);
        f.core
            .store
            .set_setting(&format!("homeassistant.cooldown.{}", f.id), &Value::Null)
            .unwrap();
    }
    f.m.lock().unwrap().status = None;
    f.m.lock().unwrap().malformed = true;
    assert_eq!(
        f.run("homeassistant.states", json!({})).await.unwrap_err(),
        "HA_MALFORMED_JSON"
    );
    f.clean().await;
}
#[tokio::test]
async fn homeassistant_write_transport_or_revocation_reports_uncertain_without_replay() {
    let f = Fixture::new(false).await;
    let v = f.observed_call().await;
    let p = f.draft("homeassistant.call_service", v).await;
    f.m.lock().unwrap().delay_write = true;
    let notify = f.m.lock().unwrap().started.clone();
    let task = tokio::spawn(execute(p));
    notify.notified().await;
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(
        task.await.unwrap().unwrap_err(),
        "HA_ACTION_OUTCOME_UNCERTAIN: PRIVACY_DENIED"
    );
    assert_eq!(f.calls(), 1);
    f.clean().await;
}
#[tokio::test]
async fn homeassistant_camera_private_decoded_managed_asset_and_bad_image_rejection() {
    let f = Fixture::new(true).await;
    let v = f
        .approve(
            "homeassistant.camera_snapshot",
            json!({"entity_id":"camera.fixture"}),
            "allow",
            || (),
        )
        .await
        .unwrap();
    assert_eq!(v["mime"], "image/png");
    assert_eq!(v["width"], 3);
    assert_eq!(v["height"], 2);
    assert!(!v.to_string().contains(TOKEN));
    let rows = f
        .core
        .store
        .query("SELECT record_json,state FROM asset_records", &[])
        .unwrap();
    assert_eq!(rows[0]["state"], "ready");
    let r: Value = serde_json::from_str(rows[0]["record_json"].as_str().unwrap()).unwrap();
    assert_eq!(r["capture_module"], "homeassistant");
    assert!(r["thumbnail"].is_object());
    f.m.lock().unwrap().camera_bad = true;
    assert!(f
        .approve(
            "homeassistant.camera_snapshot",
            json!({"entity_id":"camera.fixture"}),
            "allow",
            || ()
        )
        .await
        .is_err());
    assert_eq!(
        f.core
            .store
            .query("SELECT COUNT(*) AS n FROM asset_records", &[])
            .unwrap()[0]["n"],
        1
    );
    f.clean().await;
}
#[tokio::test]
async fn homeassistant_camera_approval_rechecks_policy_before_asset_creation() {
    let f = Fixture::new(true).await;
    let e=f.approve("homeassistant.camera_snapshot",json!({"entity_id":"camera.fixture"}),"allow",||{f.core.store.set_setting("asset_lifecycle",&json!({"ttl_seconds":3600,"max_bytes":128*1024*1024,"cleanup_enabled":true})).unwrap();}).await.unwrap_err();
    assert_eq!(e, "ASSET_POLICY_CHANGED_REVIEW_REQUIRED");
    assert_eq!(
        f.core
            .store
            .query("SELECT COUNT(*) AS n FROM asset_records", &[])
            .unwrap()[0]["n"],
        0
    );
    f.clean().await;
}
#[tokio::test]
async fn homeassistant_websocket_auth_scoped_events_poll_and_correlated_unsubscribe() {
    let f = Fixture::new(true).await;
    let start = f.start().await;
    let id = &start["stream_id"];
    f.wait_stream(id, false).await;
    let events = f
        .run(
            "homeassistant.websocket",
            json!({"action":"poll","stream_id":id,"max_events":128}),
        )
        .await
        .unwrap();
    assert_eq!(events["events"].as_array().unwrap().len(), 1);
    assert!(events.to_string().contains("light.fixture"));
    assert!(!events.to_string().contains("light.foreign"));
    assert!(!events.to_string().contains("owned_fictional_camera_token"));
    f.run(
        "homeassistant.websocket",
        json!({"action":"stop","stream_id":id}),
    )
    .await
    .unwrap();
    let ended = f.wait_stream(id, true).await;
    assert_eq!(ended["unsubscribe_acknowledged"], true);
    assert!(f.m.lock().unwrap().unsubscribe);
    assert_eq!(ended["automatic_reconnect"], false);
    f.clean().await;
}
#[tokio::test]
async fn homeassistant_websocket_auth_denied_and_wrong_result_id_are_sanitized() {
    let f = Fixture::new(true).await;
    for (mode, error) in [
        ("auth_invalid", "HA_WS_AUTH_INVALID"),
        ("denied", "HA_WS_SUBSCRIBE_DENIED"),
        ("wrong_id", "HA_WS_RESULT_PROTOCOL"),
    ] {
        f.m.lock().unwrap().ws_mode = mode.into();
        assert_eq!(
            f.approve(
                "homeassistant.websocket",
                json!({"action":"start","event_type":"state_changed"}),
                "allow",
                || ()
            )
            .await
            .unwrap_err(),
            error
        );
        assert!(f.core.homeassistant_streams.lock().unwrap().is_empty());
    }
    f.clean().await;
}
#[tokio::test]
async fn homeassistant_websocket_overflow_is_bounded_and_losses_visible() {
    let f = Fixture::new(true).await;
    f.m.lock().unwrap().ws_events = (0..300)
        .map(|i| {
            let mut e = event("light.fixture");
            e["event"]["data"]["new_state"]["attributes"]["sequence"] = json!(i);
            e
        })
        .collect();
    let started = f.start().await;
    let id = &started["stream_id"];
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let v = f
                .run(
                    "homeassistant.websocket",
                    json!({"action":"status","stream_id":id}),
                )
                .await
                .unwrap();
            if v["received"] == 300 {
                assert!(v["dropped"].as_u64().unwrap() >= 172);
                assert!(v["queued"].as_u64().unwrap() <= 128);
                assert!(v["queued_bytes"].as_u64().unwrap() <= 256 * 1024);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    f.clean().await;
}
#[tokio::test]
async fn homeassistant_websocket_privacy_revokes_and_clears_inmemory_events() {
    let f = Fixture::new(true).await;
    let started = f.start().await;
    let id = started["stream_id"].as_str().unwrap();
    f.wait_stream(&json!(id), false).await;
    let stream = f
        .core
        .homeassistant_streams
        .lock()
        .unwrap()
        .get(id)
        .unwrap()
        .clone();
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let closed = {
                let s = stream.state.lock().unwrap();
                if s.status == "closed" {
                    assert_eq!(s.reason, "PRIVACY_DENIED");
                    assert!(s.events.is_empty());
                    true
                } else {
                    false
                }
            };
            if closed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    f.clean().await;
}
#[tokio::test]
async fn homeassistant_profile_migration_reorigin_alias_and_tool_schemas_fail_closed() {
    let f = Fixture::new(false).await;
    let raw = record(&f.core, &f.id).unwrap();
    let mut changed = raw.clone();
    changed["base_url"] = json!("https://another.example");
    assert_eq!(
        integrations::save(
            &f.core,
            &json!({"id":f.id,"module_id":"homeassistant","name":"Changed","config":changed})
        )
        .await
        .unwrap_err(),
        "HA_CREDENTIAL_REQUIRED_FOR_TARGET_CHANGE"
    );
    f.core
        .store
        .execute(
            "UPDATE connections SET secret_ref='connection.foreign' WHERE id=?",
            &[json!(f.id)],
        )
        .unwrap();
    assert!(Scope::new(&f.core, &f.id).is_err());
    assert!(f
        .run(
            "homeassistant.states",
            json!({"endpoint":"https://arbitrary.example"})
        )
        .await
        .is_err());
    assert_eq!(
        integrations::dispatch(&f.core, "homeassistant", "states", &f.input(json!({})))
            .await
            .unwrap_err(),
        "HA_PREPARED_DISPATCH_REQUIRED"
    );
    f.clean().await;
}
#[test]
fn homeassistant_config_is_closed_and_http_requires_explicit_choice() {
    let base = json!({"profile_version":1,"base_url":"https://ha.example","allowed_entities":[],"allowed_services":{},"allowed_events":[]});
    assert!(validate_config(&base).is_ok());
    for (key, value) in [
        ("auth", json!("none")),
        ("token", json!(TOKEN)),
        ("base_url", json!("https://user:secret@ha.example")),
        ("base_url", json!("https://ha.example/escape")),
        ("base_url", json!("http://192.168.1.9:8123")),
        (
            "allowed_entities",
            json!(["light.fixture", "light.fixture"]),
        ),
        ("allowed_events", json!(["*"])),
    ] {
        let mut v = base.clone();
        v[key] = value;
        assert!(validate_config(&v).is_err(), "{key}: {v}");
    }
    let mut v = base;
    v["base_url"] = json!("http://192.168.1.9:8123");
    v["allow_insecure_http"] = json!(true);
    assert!(validate_config(&v).is_ok());
    for tool in TOOLS {
        jsonschema::validator_for(&schema(tool)).unwrap();
    }
}
#[tokio::test]
async fn homeassistant_service_response_flag_is_reviewed_and_response_data_bounded() {
    let f = Fixture::new(false).await;
    let mut v = Fixture::call();
    v["return_response"] = json!(true);
    let observed = f
        .run("homeassistant.call_service", v.clone())
        .await
        .unwrap();
    v["action"] = json!("call");
    v["expected"] = observed["expected"].clone();
    let mut changed = v.clone();
    changed["return_response"] = json!(false);
    assert_eq!(
        f.run("homeassistant.call_service", changed)
            .await
            .unwrap_err(),
        "HA_OBSERVATION_CHANGED_REVIEW_REQUIRED"
    );
    assert_eq!(f.calls(), 0);
    let result = f
        .approve("homeassistant.call_service", v, "allow", || ())
        .await
        .unwrap();
    assert_eq!(
        result["service_response"]["owned_result"],
        "fixture private response"
    );
    assert_eq!(result["acknowledged"], true);
    assert_eq!(f.calls(), 1);
    f.clean().await;
}
#[tokio::test]
async fn homeassistant_websocket_oversized_frames_and_foreign_state_payload_close() {
    for oversized in [true, false] {
        let f = Fixture::new(true).await;
        let mut e = event("light.fixture");
        if oversized {
            e["event"]["data"]["blob"] = json!("x".repeat(64 * 1024));
        } else {
            e["event"]["data"]["new_state"]["entity_id"] = json!("light.foreign");
        }
        f.m.lock().unwrap().ws_events = vec![e];
        let started = f.start().await;
        let ended = f.wait_stream(&started["stream_id"], true).await;
        assert_eq!(ended["received"], 0);
        assert_eq!(
            ended["reason"],
            if oversized {
                "HA_WS_FRAME_OR_TRANSPORT"
            } else {
                "HA_WS_EVENT_ENTITY"
            }
        );
        f.clean().await;
    }
}
#[tokio::test]
async fn homeassistant_websocket_generation_and_module_loss_revoke_without_reconnect() {
    for generation in [true, false] {
        let f = Fixture::new(true).await;
        let start = f.start().await;
        let id = start["stream_id"].as_str().unwrap();
        f.wait_stream(&json!(id), false).await;
        let stream = f
            .core
            .homeassistant_streams
            .lock()
            .unwrap()
            .get(id)
            .unwrap()
            .clone();
        if generation {
            f.core
                .store
                .set_setting(
                    &format!("homeassistant.connection_generation.{}", f.id),
                    &json!(storage::id()),
                )
                .unwrap();
        } else {
            f.core
                .store
                .execute(
                    "UPDATE module_configs SET enabled=0 WHERE module_id='homeassistant'",
                    &[],
                )
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let closed = {
                    let state = stream.state.lock().unwrap();
                    if state.status == "closed" {
                        assert!(state.events.is_empty());
                        assert_eq!(
                            state.reason,
                            if generation {
                                "HA_CONNECTION_CHANGED"
                            } else {
                                "MODULE_DISABLED: homeassistant"
                            }
                        );
                        true
                    } else {
                        false
                    }
                };
                if closed {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            f.m.lock()
                .unwrap()
                .seen
                .iter()
                .filter(|v| v["method"] == "WS_AUTH")
                .count(),
            1
        );
        f.clean().await;
    }
}
#[cfg(windows)]
#[tokio::test]
async fn homeassistant_native_fictional_credential_save_read_rotation_revokes_prepared_actor() {
    use windows::{
        core::PCWSTR,
        Win32::Security::Credentials::{
            CredDeleteW, CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC,
        },
    };
    let f = Fixture::new(false).await;
    let raw = record(&f.core, &f.id).unwrap();
    let target = format!("alfredpet.public.connection.{}", f.id);
    let name: Vec<u16> = target.encode_utf16().chain(Some(0)).collect();
    let mut ptr = std::ptr::null_mut::<CREDENTIALW>();
    // Never overwrite or delete a preexisting native credential, even for a
    // randomly generated owned fixture UUID.
    let missing = unsafe { CredReadW(PCWSTR(name.as_ptr()), CRED_TYPE_GENERIC, None, &mut ptr) };
    if missing.is_ok() {
        unsafe {
            CredFree(ptr.cast());
        }
        panic!("Owned fixture native alias already exists");
    }
    assert_eq!(missing.unwrap_err().code().0 as u32, 0x80070490);
    struct Cleanup(Vec<u16>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = unsafe { CredDeleteW(PCWSTR(self.0.as_ptr()), CRED_TYPE_GENERIC, None) };
        }
    }
    let cleanup = Cleanup(name);
    integrations::save(&f.core,&json!({"id":f.id,"module_id":"homeassistant","name":"Owned native fixture","config":raw,"secret":TOKEN})).await.unwrap();
    f.core
        .homeassistant_fixture
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .token = None;
    let p = f.draft("homeassistant.states", json!({})).await;
    assert_eq!(
        execute(p).await.unwrap()["items"].as_array().unwrap().len(),
        4
    );
    let p = f.draft("homeassistant.states", json!({})).await;
    integrations::save(&f.core,&json!({"id":f.id,"module_id":"homeassistant","name":"Owned native rotated","config":raw,"secret":"owned_fictional_rotated_ha_token"})).await.unwrap();
    assert_eq!(execute(p).await.unwrap_err(), "HA_CONNECTION_CHANGED");
    // A failure after real WinCred readback must retain a blocked intent.
    // The trigger belongs only to this isolated fixture database.
    let binding_key = format!("homeassistant.credential_binding.{}", f.id);
    f.core.store.execute(&format!("CREATE TRIGGER owned_block_ha_ready BEFORE UPDATE ON settings WHEN NEW.key='{binding_key}' AND json_extract(NEW.json_value,'$.stage')='ready' BEGIN SELECT RAISE(ABORT,'owned fixture ready failure'); END"),&[]).unwrap();
    let count = f.m.lock().unwrap().seen.len();
    assert_eq!(integrations::save(&f.core,&json!({"id":f.id,"module_id":"homeassistant","name":"Owned publication failure","config":raw,"secret":TOKEN})).await.unwrap_err(),format!("HA_CONNECTION_CREDENTIAL_PENDING:{}",f.id));
    let pending = credentials::binding(&f.core, &f.id, &raw).unwrap();
    assert_eq!(pending["stage"], "pending");
    let private: Value =
        serde_json::from_str(&credentials::native_read(&f.core, &f.id).await.unwrap()).unwrap();
    assert_eq!(private["generation"], pending["generation"]);
    assert_eq!(private["token"], TOKEN);
    assert_eq!(
        f.run("homeassistant.states", json!({})).await.unwrap_err(),
        "HA_CREDENTIAL_REENTRY_REQUIRED"
    );
    assert_eq!(integrations::save(&f.core,&json!({"id":f.id,"module_id":"homeassistant","name":"No implicit adoption","config":raw})).await.unwrap_err(),"HA_CREDENTIAL_REENTRY_REQUIRED");
    assert_eq!(f.m.lock().unwrap().seen.len(), count);
    f.core
        .store
        .execute("DROP TRIGGER owned_block_ha_ready", &[])
        .unwrap();
    integrations::save(&f.core,&json!({"id":f.id,"module_id":"homeassistant","name":"Owned reconciled","config":raw,"secret":TOKEN})).await.unwrap();
    let saved = credentials::native_read(&f.core, &f.id).await.unwrap();
    assert_ne!(
        credentials::binding(&f.core, &f.id, &raw).unwrap()["generation"],
        pending["generation"]
    );
    integrations::save(&f.core,&json!({"id":f.id,"module_id":"homeassistant","name":"Owned retain credential","config":raw})).await.unwrap();
    assert_eq!(
        credentials::native_read(&f.core, &f.id).await.unwrap(),
        saved
    );
    assert_eq!(
        f.run("homeassistant.states", json!({})).await.unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    // Legacy scalar bytes are not silently interpreted as a ready token.
    crate::process::native(
        &f.core.native,
        "secret.set",
        &json!({"alias":format!("connection.{}",f.id),"value":"owned_legacy_scalar"}),
    )
    .await
    .unwrap();
    let count = f.m.lock().unwrap().seen.len();
    assert_eq!(
        f.run("homeassistant.states", json!({})).await.unwrap_err(),
        "HA_CREDENTIAL_LEGACY_REENTRY_REQUIRED"
    );
    assert_eq!(f.m.lock().unwrap().seen.len(), count);
    integrations::save(&f.core,&json!({"id":f.id,"module_id":"homeassistant","name":"Owned final reconciliation","config":raw,"secret":TOKEN})).await.unwrap();
    let v = f
        .core
        .store
        .query(
            "SELECT config_json,secret_ref FROM connections WHERE id=?",
            &[json!(f.id)],
        )
        .unwrap()
        .to_string();
    assert!(!v.contains(TOKEN));
    drop(cleanup);
    let mut ptr = std::ptr::null_mut::<CREDENTIALW>();
    let erased_name: Vec<u16> = target.encode_utf16().chain(Some(0)).collect();
    assert!(unsafe {
        CredReadW(
            PCWSTR(erased_name.as_ptr()),
            CRED_TYPE_GENERIC,
            None,
            &mut ptr,
        )
    }
    .is_err());
    f.clean().await;
}
#[tokio::test]
async fn homeassistant_websocket_local_status_stop_work_without_token_after_privacy_and_disable() {
    let f = Fixture::new(true).await;
    let start = f.start().await;
    let id = start["stream_id"].clone();
    f.wait_stream(&id, false).await;
    f.core
        .homeassistant_fixture
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .token = None;
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='homeassistant'",
            &[],
        )
        .unwrap();
    let count = f.m.lock().unwrap().seen.len();
    let status = f
        .run(
            "homeassistant.websocket",
            json!({"action":"status","stream_id":id}),
        )
        .await
        .unwrap();
    assert!(status["events"].as_array().unwrap().is_empty());
    assert!(f
        .run(
            "homeassistant.websocket",
            json!({"action":"poll","stream_id":id})
        )
        .await
        .is_err());
    assert_eq!(
        f.run(
            "homeassistant.websocket",
            json!({"action":"stop","stream_id":id})
        )
        .await
        .unwrap()["stop_requested"],
        true
    );
    let closed = f.wait_stream(&id, true).await;
    assert_eq!(closed["status"], "closed");
    assert_eq!(f.m.lock().unwrap().seen.len(), count);
    f.clean().await;
}
#[tokio::test]
async fn homeassistant_websocket_local_controls_cannot_select_another_connection() {
    let f = Fixture::new(true).await;
    let start = f.start().await;
    let mut v = f.input(json!({"action":"stop","stream_id":start["stream_id"]}));
    v["connection"] = json!(storage::id());
    assert_eq!(
        f.core
            .execute("homeassistant.websocket", v)
            .await
            .unwrap_err(),
        "HA_WS_STREAM_OUT_OF_SCOPE"
    );
    f.run(
        "homeassistant.websocket",
        json!({"action":"stop","stream_id":start["stream_id"]}),
    )
    .await
    .unwrap();
    f.wait_stream(&start["stream_id"], true).await;
    f.clean().await;
}
#[tokio::test]
async fn homeassistant_manual_cooldown_requires_explicit_save_and_revokes_old_action() {
    let f = Fixture::new(false).await;
    let v = f.observed_call().await;
    let p = f.draft("homeassistant.call_service", v).await;
    let count = f.m.lock().unwrap().seen.len();
    f.core
        .store
        .set_setting(
            &format!("homeassistant.cooldown.{}", f.id),
            &json!({"manual":true}),
        )
        .unwrap();
    assert_eq!(
        f.run("homeassistant.states", json!({})).await.unwrap_err(),
        "HA_RATE_LIMITED_MANUAL_RETRY"
    );
    assert_eq!(f.m.lock().unwrap().seen.len(), count);
    let raw = record(&f.core, &f.id).unwrap();
    integrations::save(
        &f.core,
        &json!({"id":f.id,"module_id":"homeassistant","name":"Owned explicit retry","config":raw}),
    )
    .await
    .unwrap();
    assert_eq!(execute(p).await.unwrap_err(), "HA_CONNECTION_CHANGED");
    assert_eq!(f.calls(), 0);
    assert_eq!(
        f.run("homeassistant.states", json!({})).await.unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    f.clean().await;
}
#[tokio::test]
async fn homeassistant_blocked_native_readback_preserves_intent_and_denies_http() {
    let mut f = Fixture::new(false).await;
    let raw = record(&f.core, &f.id).unwrap();
    let script = f.root.join("native-readback-mismatch.ps1");
    std::fs::write(&script,"$ErrorActionPreference='Stop'\n$null=[Console]::In.ReadToEnd()\n[Console]::Out.WriteLine('{\"ok\":true,\"data\":{\"saved\":true,\"value\":\"owned_old_scalar\"}}')\n").unwrap();
    Arc::get_mut(&mut f.core).unwrap().native = script;
    assert_eq!(integrations::save(&f.core,&json!({"id":f.id,"module_id":"homeassistant","name":"Owned blocked","config":raw,"secret":TOKEN})).await.unwrap_err(),format!("HA_CONNECTION_CREDENTIAL_PENDING:{}",f.id));
    let binding = credentials::binding(&f.core, &f.id, &raw).unwrap();
    assert_eq!(binding["stage"], "pending");
    assert_eq!(
        f.run("homeassistant.states", json!({})).await.unwrap_err(),
        "HA_CREDENTIAL_REENTRY_REQUIRED"
    );
    assert!(f.m.lock().unwrap().seen.is_empty());
    assert_eq!(integrations::save(&f.core,&json!({"id":f.id,"module_id":"homeassistant","name":"No implicit adoption","config":raw})).await.unwrap_err(),"HA_CREDENTIAL_REENTRY_REQUIRED");
    let row = f
        .core
        .store
        .query(
            "SELECT health,config_json FROM connections WHERE id=?",
            &[json!(f.id)],
        )
        .unwrap();
    assert_eq!(row[0]["health"], "CREDENTIAL_PENDING");
    assert!(!row.to_string().contains(TOKEN));
    f.clean().await;
}
#[tokio::test]
async fn homeassistant_profile_intent_failure_rolls_back_before_native_or_http() {
    let mut f = Fixture::new(false).await;
    let marker = f.root.join("native-was-called");
    let script = f.root.join("native-intent-marker.ps1");
    std::fs::write(&script,"$ErrorActionPreference='Stop'\n$null=[Console]::In.ReadToEnd()\n[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'native-was-called'),'called')\n[Console]::Out.WriteLine('{\"ok\":true,\"data\":{}}')\n").unwrap();
    Arc::get_mut(&mut f.core).unwrap().native = script;
    let raw = record(&f.core, &f.id).unwrap();
    let generation = f
        .core
        .store
        .setting(&format!("homeassistant.connection_generation.{}", f.id));
    let binding = credentials::binding(&f.core, &f.id, &raw).unwrap();
    f.core.store.execute("CREATE TRIGGER owned_block_ha_intent BEFORE UPDATE ON connections WHEN NEW.module_id='homeassistant' BEGIN SELECT RAISE(ABORT,'owned fixture intent failure'); END",&[]).unwrap();
    assert_eq!(integrations::save(&f.core,&json!({"id":f.id,"module_id":"homeassistant","name":"Owned failure","config":raw,"secret":TOKEN})).await.unwrap_err(),"HA_PROFILE_INTENT_TRANSACTION");
    assert_eq!(credentials::binding(&f.core, &f.id, &raw).unwrap(), binding);
    assert_eq!(
        f.core
            .store
            .setting(&format!("homeassistant.connection_generation.{}", f.id)),
        generation
    );
    assert!(f.m.lock().unwrap().seen.is_empty());
    assert!(!marker.exists());
    f.clean().await;
}
