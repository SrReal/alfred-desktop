//! Product Core/transport contracts with exact owned official SDK processes.
use super::*;
use tokio::io::AsyncBufReadExt;

struct Wire {
    mode: String,
    started: tokio::sync::Notify,
    dropped: std::sync::atomic::AtomicBool,
    requests: std::sync::atomic::AtomicUsize,
}
struct WireLife(Arc<Wire>);
impl Drop for WireLife {
    fn drop(&mut self) {
        self.0.dropped.store(true, Ordering::SeqCst);
    }
}
async fn wire_handler(State(wire): State<Arc<Wire>>, r: Request) -> Response<Body> {
    let bytes = axum::body::to_bytes(r.into_body(), 65536).await.unwrap();
    let request: Value = serde_json::from_slice(&bytes).unwrap();
    if request["method"] == "server/discover" {
        return Response::builder().header("content-type","application/json").body(Body::from(json!({"jsonrpc":"2.0","id":request["id"],"result":{"resultType":"complete","supportedVersions":[client::protocol::MODERN],"capabilities":{"tools":{},"resources":{},"prompts":{}}}}).to_string())).unwrap();
    }
    assert_eq!(request["method"], "subscriptions/listen");
    wire.requests.fetch_add(1, Ordering::SeqCst);
    let id = &request["id"];
    let key = crate::mcp_subscription_wire::SUBSCRIPTION_ID;
    let notice =
        |method: &str| json!({"jsonrpc":"2.0","method":method,"params":{"_meta":{key:id}}});
    let ack = json!({"jsonrpc":"2.0","method":"notifications/subscriptions/acknowledged","params":{"notifications":{"toolsListChanged":true},"_meta":{key:id}}});
    let encoded = |value: &Value| axum::body::Bytes::from(format!("data: {value}\n\n"));
    let mut chunks = std::collections::VecDeque::new();
    let mut hold = false;
    match wire.mode.as_str() {
        "blocked_ack" => hold = true,
        "partial_ack" => {
            chunks.push_back(axum::body::Bytes::from("data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/subscriptions/acknowledged\",\"params\":{\"opaque\":\"Se"));
            hold = true;
        }
        "before_ack" => chunks.push_back(encoded(&notice("notifications/tools/list_changed"))),
        "wrong_ack" => {
            let mut v = ack.clone();
            v["params"]["_meta"][key] = json!(2);
            chunks.push_back(encoded(&v));
        }
        mode => {
            chunks.push_back(encoded(&ack));
            match mode {
                "duplicate_ack" => chunks.push_back(encoded(&ack)),
                "unrequested" => {
                    chunks.push_back(encoded(&notice("notifications/prompts/list_changed")))
                }
                "foreign_id" => {
                    let mut v = notice("notifications/tools/list_changed");
                    v["params"]["_meta"][key] = json!(2);
                    chunks.push_back(encoded(&v));
                }
                "server_request" => chunks.push_back(encoded(
                    &json!({"jsonrpc":"2.0","id":99,"method":"sampling/createMessage","params":{}}),
                )),
                "oversize_frame" => chunks.push_back(axum::body::Bytes::from(format!(
                    "data: {}\n\n",
                    "x".repeat(65537)
                ))),
                "total_limit" | "comment_flood" => {
                    let comment = axum::body::Bytes::from(format!(":{}\n\n", "x".repeat(4093)));
                    for _ in 0..if mode == "total_limit" { 2049 } else { 5000 } {
                        chunks.push_back(comment.clone());
                    }
                }
                "abrupt_eof" => {}
                "private_event" => {
                    let mut value = notice("notifications/tools/list_changed");
                    value["params"]["_meta"]["owned_private_echo"] =
                        json!("owned_fictional_listen_private_token");
                    chunks.push_back(encoded(&value));
                }
                _ => panic!("unknown owned wire mode"),
            }
        }
    }
    wire.started.notify_one();
    let stream = futures_util::stream::unfold(
        (chunks, hold, WireLife(wire)),
        |(mut chunks, hold, life)| async move {
            if let Some(chunk) = chunks.pop_front() {
                Some((Ok::<_, std::io::Error>(chunk), (chunks, hold, life)))
            } else if hold {
                std::future::pending().await
            } else {
                None
            }
        },
    );
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}
async fn wire_fixture(f: &Fixture, mode: &str) -> (Arc<Wire>, tokio::task::JoinHandle<()>) {
    let wire = Arc::new(Wire {
        mode: mode.into(),
        started: tokio::sync::Notify::new(),
        dropped: std::sync::atomic::AtomicBool::new(false),
        requests: std::sync::atomic::AtomicUsize::new(0),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = Router::new()
        .fallback(wire_handler)
        .with_state(wire.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut raw = f.raw();
    raw["base_url"] = json!(format!("http://{address}"));
    raw["protocol"] = json!("modern");
    f.save(&raw, None).await.unwrap();
    (wire, server)
}
async fn wire_dropped(wire: &Wire) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !wire.dropped.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn stop_or_revoke_before_fragmented_ack_closes_original_response_without_activation() {
    for mode in ["blocked_ack", "partial_ack"] {
        for stopping in [true, false] {
            let f = Fixture::new().await;
            let (wire, server) = wire_fixture(&f, mode).await;
            {
                let call = f.approve(
                    "mcp.listen",
                    json!({"notifications":{"toolsListChanged":true}}),
                    "allow",
                    || (),
                );
                tokio::pin!(call);
                tokio::select! {r=&mut call=>panic!("unexpected ACK result: {r:?}"),_=wire.started.notified()=>{}}
                let id = f
                    .core
                    .mcp_streams
                    .lock()
                    .unwrap()
                    .keys()
                    .next()
                    .unwrap()
                    .clone();
                if stopping {
                    f.core
                        .execute("mcp.listen_stop", f.input(json!({"stream_id":id})))
                        .await
                        .unwrap();
                } else {
                    revoke(&f.core).unwrap();
                }
                assert!(tokio::time::timeout(Duration::from_secs(3), &mut call)
                    .await
                    .unwrap()
                    .is_err());
                let value = closed(&f, &id).await;
                assert_eq!(value["received"], 0);
                wire_dropped(&wire).await;
                assert_eq!(wire.requests.load(Ordering::SeqCst), 1);
            }
            server.abort();
            f.clean().await;
        }
    }
}
#[tokio::test]
async fn pre_ack_frames_and_foreign_ack_fail_before_active_authority() {
    for (mode, code) in [
        ("before_ack", "MCP_LISTEN_ACK_REQUIRED"),
        ("wrong_ack", "MCP_LISTEN_CORRELATION"),
    ] {
        let f = Fixture::new().await;
        let (wire, server) = wire_fixture(&f, mode).await;
        assert_eq!(
            f.approve(
                "mcp.listen",
                json!({"notifications":{"toolsListChanged":true}}),
                "allow",
                || ()
            )
            .await
            .unwrap_err(),
            code
        );
        let id = f
            .core
            .mcp_streams
            .lock()
            .unwrap()
            .keys()
            .next()
            .unwrap()
            .clone();
        assert_eq!(closed(&f, &id).await["received"], 0);
        wire_dropped(&wire).await;
        assert_eq!(wire.requests.load(Ordering::SeqCst), 1);
        server.abort();
        f.clean().await;
    }
}
#[tokio::test]
async fn malformed_or_out_of_scope_active_frames_and_eof_close_without_events_or_replay() {
    for (mode, code) in [
        ("duplicate_ack", "MCP_LISTEN_DUPLICATE_ACK"),
        ("unrequested", "MCP_LISTEN_EVENT_SCOPE"),
        ("foreign_id", "MCP_LISTEN_CORRELATION"),
        ("server_request", "MCP_RESPONSE_SCOPE"),
        ("oversize_frame", "MCP_LISTEN_FRAME_LIMIT"),
        ("total_limit", "MCP_RESPONSE_LIMIT"),
        ("abrupt_eof", "MCP_LISTEN_ABRUPT_EOF"),
    ] {
        let f = Fixture::new().await;
        let (wire, server) = wire_fixture(&f, mode).await;
        let result = start(&f, json!({"toolsListChanged":true})).await;
        let id = result["stream_id"].as_str().unwrap();
        eprintln!("Owned negative listener case awaiting closure: {mode}");
        let value = closed(&f, id).await;
        assert_eq!(value["reason"], code);
        assert_eq!(value["received"], 0);
        assert!(f
            .core
            .execute("mcp.listen_poll", f.input(json!({"stream_id":id})))
            .await
            .is_err());
        assert_eq!(wire.requests.load(Ordering::SeqCst), 1);
        wire_dropped(&wire).await;
        server.abort();
        f.clean().await;
    }
}

#[tokio::test]
async fn registry_limits_active_and_closed_records_without_retaining_authority() {
    let f = Fixture::new().await;
    let filter =
        crate::mcp_subscription_wire::Filter::parse(&json!({"toolsListChanged":true}), true)
            .unwrap();
    let active = (0..4)
        .map(|_| listen::reserve(&f.core, &f.id, &filter).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        listen::reserve(&f.core, &f.id, &filter).err().unwrap(),
        "MCP_LISTEN_CAPACITY"
    );
    drop(active);
    for _ in 0..42 {
        drop(listen::reserve(&f.core, &f.id, &filter).unwrap());
    }
    assert_eq!(f.core.mcp_streams.lock().unwrap().len(), 32);
    assert!(f
        .core
        .mcp_streams
        .lock()
        .unwrap()
        .values()
        .all(|s| !s.retains_authority()));
    f.clean().await;
}

#[tokio::test]
async fn local_stop_preempts_continuously_ready_comment_flood_without_events() {
    let f = Fixture::new().await;
    let (wire, server) = wire_fixture(&f, "comment_flood").await;
    let result = start(&f, json!({"toolsListChanged":true})).await;
    let id = result["stream_id"].as_str().unwrap();
    let stopped = tokio::time::timeout(
        Duration::from_secs(3),
        f.core
            .execute("mcp.listen_stop", f.input(json!({"stream_id":id}))),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(stopped["status"], "closed");
    assert_eq!(stopped["received"], 0);
    wire_dropped(&wire).await;
    assert_eq!(wire.requests.load(Ordering::SeqCst), 1);
    server.abort();
    f.clean().await;
}

#[tokio::test]
async fn private_native_credential_echo_is_rejected_before_catalog_change_or_publication() {
    use windows::{
        core::PCWSTR,
        Win32::Security::Credentials::{
            CredDeleteW, CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC,
        },
    };
    let f = Fixture::new().await;
    let (wire, server) = wire_fixture(&f, "private_event").await;
    let alias: Vec<u16> = format!("alfredpet.public.{}", credentials::alias(&f.id).unwrap())
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut ptr = std::ptr::null_mut::<CREDENTIALW>();
    let absent = unsafe { CredReadW(PCWSTR(alias.as_ptr()), CRED_TYPE_GENERIC, None, &mut ptr) };
    if absent.is_ok() {
        unsafe {
            CredFree(ptr.cast());
        }
        panic!("Owned listen credential alias already exists");
    }
    assert_eq!(absent.unwrap_err().code().0 as u32, 0x80070490);
    struct Cleanup(Vec<u16>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = unsafe { CredDeleteW(PCWSTR(self.0.as_ptr()), CRED_TYPE_GENERIC, None) };
        }
    }
    let cleanup = Cleanup(alias);
    let mut raw = f.raw();
    raw["auth"] = json!({"type":"bearer","secret_ref":"main"});
    f.save(
        &raw,
        Some(&json!({"main":"owned_fictional_listen_private_token"}).to_string()),
    )
    .await
    .unwrap();
    let before = catalog::Fence::capture(&f.core.store, &f.id, catalog::Category::Tools).unwrap();
    let result = start(&f, json!({"toolsListChanged":true})).await;
    let id = result["stream_id"].as_str().unwrap();
    let value = closed(&f, id).await;
    assert_eq!(value["reason"], "MCP_CREDENTIAL_CONTENT_DENIED");
    assert_eq!(value["received"], 0);
    before.check(&f.core.store).unwrap();
    let audit = f
        .core
        .store
        .query(
            "SELECT input_summary,output_summary FROM activity_events",
            &[],
        )
        .unwrap();
    assert!(!audit
        .to_string()
        .contains("owned_fictional_listen_private_token"));
    assert!(!value
        .to_string()
        .contains("owned_fictional_listen_private_token"));
    wire_dropped(&wire).await;
    assert_eq!(wire.requests.load(Ordering::SeqCst), 1);
    server.abort();
    drop(cleanup);
    f.clean().await;
}

struct Sdk {
    child: Option<tokio::process::Child>,
    endpoint: Option<String>,
    folder: PathBuf,
    sequence: u64,
}
impl Sdk {
    async fn new(f: &Fixture, stdio: bool, empty: bool) -> Self {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/mcp-listen-sdk-server.mjs")
            .canonicalize()
            .unwrap();
        let folder = f.root.join("owned-listen-mailbox");
        std::fs::create_dir(&folder).unwrap();
        if stdio {
            let mut args = vec![json!(script), json!("--stdio")];
            if empty {
                args.push(json!("--empty"));
            }
            args.push(json!(folder));
            f.save(&json!({"profile_version":1,"transport":"stdio","protocol":"modern","command":sdk::node(),"args":args,"auth":{"type":"none"},"permissions":{"tools":{"owned_echo":true},"resources":["owned:///one"]}}),None).await.unwrap();
            return Self {
                child: None,
                endpoint: None,
                folder,
                sequence: 0,
            };
        }
        let mut command = tokio::process::Command::new(sdk::node());
        command
            .arg(script)
            .env_clear()
            .env("SystemRoot", std::env::var_os("SystemRoot").unwrap())
            .creation_flags(0x08000000)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if empty {
            command.arg("--empty");
        }
        let mut child = command.spawn().unwrap();
        let mut line = String::new();
        let mut reader = tokio::io::BufReader::new(child.stdout.take().unwrap());
        let count = tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert!(
            count > 0 && count < 2048,
            "Owned listen SDK exited before readiness: {:?}",
            child.try_wait().unwrap()
        );
        let ready: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["pins"], json!({"server":"2.3.0","node":"2.1.1"}));
        assert_eq!(ready["scope"], "owned-listen-sdk-fixture");
        let endpoint = ready["endpoint"].as_str().unwrap().to_string();
        let url = reqwest::Url::parse(&endpoint).unwrap();
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        let mut raw = f.raw();
        raw["base_url"] = json!(url.origin().ascii_serialization());
        raw["protocol"] = json!("modern");
        raw["permissions"] = json!({"tools":{"owned_echo":true},"resources":["owned:///one"]});
        f.save(&raw, None).await.unwrap();
        Self {
            child: Some(child),
            endpoint: Some(endpoint),
            folder,
            sequence: 0,
        }
    }
    async fn http(&self, path: &str, input: Option<Value>) -> Value {
        let mut url = reqwest::Url::parse(self.endpoint.as_ref().unwrap()).unwrap();
        url.set_path(path);
        let client = crate::http_client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let request = if let Some(input) = input {
            client.post(url).json(&input)
        } else {
            client.get(url)
        };
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), 200);
        let bytes = response.bytes().await.unwrap();
        assert!(bytes.len() < 65536);
        serde_json::from_slice(&bytes).unwrap()
    }
    async fn stats(&self) -> Value {
        if self.endpoint.is_some() {
            return self.http("/__owned_stats", None).await;
        }
        let bytes = std::fs::read(self.folder.join("stats.json")).unwrap();
        assert!(bytes.len() < 65536);
        serde_json::from_slice(&bytes).unwrap()
    }
    async fn signal(&mut self, mode: &str, count: u64) {
        self.sequence += 1;
        let input = json!({"sequence":self.sequence,"mode":mode,"count":count});
        if self.endpoint.is_some() {
            self.http("/__owned_command", Some(input)).await;
        } else {
            let temporary = self.folder.join("command.pending");
            std::fs::write(&temporary, input.to_string()).unwrap();
            std::fs::rename(temporary, self.folder.join("command.json")).unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if self.stats().await["sequence"] == self.sequence {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn active(&self, count: u64) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if self.stats().await["active"] == count {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn finish(mut self) {
        if let Some(mut child) = self.child.take() {
            child.kill().await.unwrap();
            child.wait().await.unwrap();
        }
    }
}
async fn status(f: &Fixture, id: &str) -> Value {
    // Observe the real local controller without writing hundreds of audit rows
    // into the very WAL whose durable catalog commits the test is waiting for.
    // Actual Core status routing after revocation is asserted separately below.
    let result = listen::prepare_control(
        &f.core,
        "mcp.listen_status",
        &f.input(json!({"stream_id":id})),
    )
    .unwrap()
    .execute()
    .await
    .unwrap();
    assert_eq!(result["streams"].as_array().unwrap().len(), 1);
    result["streams"][0].clone()
}
async fn received(f: &Fixture, id: &str, count: u64) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let value = status(f, id).await;
            if value["received"].as_u64().unwrap() >= count {
                return value;
            }
            assert_eq!(
                value["status"], "active",
                "Unexpected listener closure: {value}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
async fn closed(f: &Fixture, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let value = status(f, id).await;
            if value["status"] == "closed" {
                assert_eq!(value["queued"], 0);
                assert!(!f.core.mcp_streams.lock().unwrap()[id].retains_authority());
                return value;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
async fn start(f: &Fixture, filter: Value) -> Value {
    f.approve(
        "mcp.listen",
        json!({"notifications":filter}),
        "allow",
        || (),
    )
    .await
    .unwrap()
}
#[tokio::test]
async fn official_sdk_http_listen_requires_individual_approval_and_invalidates_owned_catalog() {
    let f = Fixture::new().await;
    let mut sdk = Sdk::new(&f, false, false).await;
    let filter = json!({"toolsListChanged":true,"resourceSubscriptions":["owned:///one"]});
    for answer in ["deny", "session"] {
        assert_eq!(
            f.approve("mcp.listen", json!({"notifications":filter}), answer, || ())
                .await
                .unwrap_err(),
            "PERMISSION_DENIED_OR_TIMEOUT"
        );
    }
    assert_eq!(sdk.stats().await["listens"], 0);
    assert!(f.core.mcp_streams.lock().unwrap().is_empty());
    let list = f
        .approve("mcp.tools", json!({}), "allow", || ())
        .await
        .unwrap();
    let input=f.input(json!({"name":"owned_echo","body":{"text":"owned"},"expected":list["tools"][0]["alfredpetObservation"]}));
    let prepared = prepare(&f.core, "mcp.call", &input).await.unwrap();
    let result = start(&f, filter.clone()).await;
    assert_eq!(result["notifications"], filter);
    let id = result["stream_id"].as_str().unwrap();
    sdk.active(1).await;
    sdk.signal("emit", 1).await;
    received(&f, id, 2).await;
    assert_eq!(
        execute(prepared).await.unwrap_err(),
        "MCP_CATALOG_CHANGED_REVIEW_REQUIRED"
    );
    let poll = f
        .core
        .execute("mcp.listen_poll", f.input(json!({"stream_id":id})))
        .await
        .unwrap();
    assert_eq!(
        poll["events"],
        json!([{"type":"tools_changed"},{"type":"resource_updated","uri":"owned:///one"}])
    );
    let stopped = f
        .core
        .execute("mcp.listen_stop", f.input(json!({"stream_id":id})))
        .await
        .unwrap();
    assert_eq!(stopped["status"], "closed");
    sdk.active(0).await;
    let stats = sdk.stats().await;
    assert_eq!(stats["acceptedCalls"], 0);
    assert_eq!(stats["listens"], 1);
    assert!(stats["methods"]
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v == "POST"));
    assert!(stats["headers"]
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v["session"].is_null() && v["lastEvent"].is_null()));
    assert_eq!(stats["errors"], json!([]));
    sdk.finish().await;
    f.clean().await;
}
#[tokio::test]
async fn official_sdk_stdio_listen_correlates_notifications_and_cancels_exact_owned_id() {
    let f = Fixture::new().await;
    let mut sdk = Sdk::new(&f, true, false).await;
    let result = start(
        &f,
        json!({"toolsListChanged":true,"resourceSubscriptions":["owned:///one"]}),
    )
    .await;
    let id = result["stream_id"].as_str().unwrap();
    sdk.active(1).await;
    sdk.signal("emit", 1).await;
    received(&f, id, 2).await;
    let poll = f
        .core
        .execute("mcp.listen_poll", f.input(json!({"stream_id":id})))
        .await
        .unwrap();
    assert_eq!(
        poll["events"],
        json!([{"type":"tools_changed"},{"type":"resource_updated","uri":"owned:///one"}])
    );
    f.core
        .execute("mcp.listen_stop", f.input(json!({"stream_id":id})))
        .await
        .unwrap();
    closed(&f, id).await;
    let stats = sdk.stats().await;
    assert_eq!(stats["cancelled"], json!([format!("listen-{id}")]));
    assert_eq!(stats["acceptedCalls"], 0);
    assert_eq!(stats["errors"], json!([]));
    assert_eq!(stats["active"], 0);
    sdk.finish().await;
    f.clean().await;
}
#[tokio::test]
async fn official_sdk_graceful_completion_and_empty_honored_filters_close_without_reconnect() {
    for stdio in [false, true] {
        for empty in [false, true] {
            let f = Fixture::new().await;
            let mut sdk = Sdk::new(&f, stdio, empty).await;
            let result = start(&f, json!({"toolsListChanged":true})).await;
            let id = result["stream_id"].as_str().unwrap();
            if empty {
                assert_eq!(result["notifications"], json!({}));
                assert_eq!(result["status"], "closed");
            } else {
                sdk.signal("close", 1).await;
                assert_eq!(closed(&f, id).await["reason"], "MCP_LISTEN_COMPLETE");
            }
            assert_eq!(sdk.stats().await["listens"], 1);
            sdk.active(0).await;
            sdk.finish().await;
            f.clean().await;
        }
    }
}
#[tokio::test]
async fn active_subscription_queue_is_bounded_and_closure_discards_payloads() {
    let f = Fixture::new().await;
    let mut sdk = Sdk::new(&f, false, false).await;
    let result = start(&f, json!({"toolsListChanged":true})).await;
    let id = result["stream_id"].as_str().unwrap();
    sdk.signal("emit", 140).await;
    let value = received(&f, id, 140).await;
    assert_eq!(value["queued"], 128);
    assert_eq!(value["lost"], 12);
    f.core
        .execute("mcp.listen_stop", f.input(json!({"stream_id":id})))
        .await
        .unwrap();
    let value = closed(&f, id).await;
    assert_eq!(value["lost"], 140);
    assert!(f
        .core
        .execute("mcp.listen_poll", f.input(json!({"stream_id":id})))
        .await
        .is_err());
    sdk.active(0).await;
    sdk.finish().await;
    f.clean().await;
}
#[tokio::test]
async fn revocation_clears_original_authority_but_local_status_and_stop_remain_available() {
    for change in ["privacy", "module", "profile", "epoch"] {
        let f = Fixture::new().await;
        let sdk = Sdk::new(&f, false, false).await;
        let result = start(&f, json!({"toolsListChanged":true})).await;
        let id = result["stream_id"].as_str().unwrap();
        match change {
            "privacy" => {
                f.core.store.set_setting("privacy", &json!(true)).unwrap();
                revoke(&f.core).unwrap();
            }
            "module" => {
                f.core
                    .store
                    .execute(
                        "UPDATE module_configs SET enabled=0 WHERE module_id='mcp'",
                        &[],
                    )
                    .unwrap();
                revoke(&f.core).unwrap();
            }
            "profile" => {
                let mut raw = f.raw();
                raw["permissions"] = json!({});
                f.save(&raw, None).await.unwrap();
            }
            _ => revoke(&f.core).unwrap(),
        }
        closed(&f, id).await;
        let routed_status = f
            .core
            .execute("mcp.listen_status", f.input(json!({"stream_id":id})))
            .await
            .unwrap();
        assert_eq!(routed_status["streams"][0]["status"], "closed");
        sdk.active(0).await;
        assert!(f
            .core
            .execute("mcp.listen_poll", f.input(json!({"stream_id":id})))
            .await
            .is_err());
        assert_eq!(
            f.core
                .execute("mcp.listen_stop", f.input(json!({"stream_id":id})))
                .await
                .unwrap()["status"],
            "closed"
        );
        assert_eq!(sdk.stats().await["listens"], 1);
        sdk.finish().await;
        f.clean().await;
    }
}
#[tokio::test]
async fn notification_storage_failure_closes_without_publishing_unfenced_event() {
    let f = Fixture::new().await;
    let mut sdk = Sdk::new(&f, false, false).await;
    let result = start(&f, json!({"toolsListChanged":true})).await;
    let id = result["stream_id"].as_str().unwrap();
    f.core.store.execute("CREATE TRIGGER owned_fail_catalog BEFORE INSERT ON settings WHEN NEW.key LIKE 'mcp.catalog.%' BEGIN SELECT RAISE(ABORT,'owned catalog failure'); END",&[]).unwrap();
    sdk.signal("emit", 1).await;
    let value = closed(&f, id).await;
    assert_eq!(value["received"], 0);
    assert_eq!(value["reason"], "MCP_CATALOG_STORAGE");
    sdk.active(0).await;
    sdk.finish().await;
    f.clean().await;
}
