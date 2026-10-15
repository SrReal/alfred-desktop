use super::*;
use axum::{body::Body, http::Request, routing::any, Router};
use std::sync::{Arc, Mutex};

struct Fixture {
    config: Value,
    log: Arc<Mutex<Vec<(String, Value, HeaderMap)>>>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn fixture(path: &str) -> Fixture {
    let log = Arc::new(Mutex::new(Vec::new()));
    let captured = log.clone();
    let router = Router::new().fallback(any(move |request: Request<Body>| {
        let log = captured.clone();
        async move {
            let path = request.uri().path().to_string();
            let method = request.method().clone();
            let headers = request.headers().clone();
            assert_eq!(headers.get("authorization").unwrap(), "Bearer own-fixture-secret");
            if method == reqwest::Method::DELETE {
                assert_eq!(headers.get("mcp-session-id").unwrap(), "own-session");
                log.lock().unwrap().push(("DELETE".into(), json!({}), headers));
                return axum::http::Response::builder().status(204).body(Body::empty()).unwrap();
            }
            let bytes = axum::body::to_bytes(request.into_body(), LIMIT).await.unwrap();
            let v: Value = serde_json::from_slice(&bytes).unwrap();
            let rpc = v["method"].as_str().unwrap();
            log.lock().unwrap().push((rpc.into(), v.clone(), headers.clone()));
            let mut status = 200;
            let result = if rpc == "server/discover" {
                assert_eq!(headers.get("mcp-protocol-version").unwrap(), protocol::MODERN);
                assert_eq!(headers.get("mcp-method").unwrap(), rpc);
                assert_eq!(v["params"]["_meta"][protocol::META_VERSION], protocol::MODERN);
                assert!(headers.get("mcp-session-id").is_none());
                if path.starts_with("/legacy") {
                    if path == "/legacy-empty" {
                        return axum::http::Response::builder().status(400).body(Body::empty()).unwrap();
                    }
                    status = 400;
                    let code = if path == "/legacy-params" { -32602 } else { -32601 };
                    json!({"error":{"code":code,"message":"legacy probe rejected"}})
                } else if path == "/modern-404" {
                    status = 404;
                    json!({"error":{"code":-32601,"message":"unknown modern method"}})
                } else if path == "/modern-error" {
                    status = 400;
                    json!({"error":{"code":-32022,"message":"unsupported", "data":{"supported":["2030-01-01"]}}})
                } else if path == "/malformed-probe" {
                    json!({"result":{"capabilities":{},"supportedVersions":[protocol::MODERN]}})
                } else {
                    json!({"result":{"resultType":"complete","supportedVersions":[protocol::MODERN],"capabilities":{"tools":{},"resources":{},"prompts":{}}}})
                }
            } else if rpc == "initialize" {
                let version = if path == "/legacy-bad-version" { "1900-01-01" } else { protocol::LEGACY[0] };
                if path == "/legacy-bad-envelope" { json!({"error":{"code":"invalid","message":"private"}}) }
                else { json!({"result":{"protocolVersion":version,"capabilities":{"tools":{}},"serverInfo":{"name":"own","version":"1"}}}) }
            } else if rpc == "notifications/initialized" {
                assert_eq!(headers.get("mcp-session-id").unwrap(), "own-session");
                let code = if path == "/legacy-bad-notification" { 400 } else { 202 };
                return axum::http::Response::builder().status(code).body(Body::empty()).unwrap();
            } else {
                if !path.starts_with("/legacy") {
                    assert_eq!(headers.get("mcp-method").unwrap(), rpc);
                    assert_eq!(v["params"]["_meta"][protocol::META_VERSION], protocol::MODERN);
                    assert!(headers.get("mcp-session-id").is_none());
                }
                match rpc {
                    "tools/list" => {
                        let tool = json!({"name":"reviewed","inputSchema":{"type":"object","properties":{"tenant":{"type":"string","x-mcp-header":"Tenant"},"n":{"type":"integer"}},"required":["tenant","n"],"additionalProperties":false},"outputSchema":{"type":"object","properties":{"ok":{"const":true}},"required":["ok"]}});
                        let mut result = if v["params"]["cursor"] == "opaque/ñ+==" {
                            json!({"tools":[tool]})
                        } else {
                            json!({"tools":[],"nextCursor":"opaque/ñ+=="})
                        };
                        if path == "/cycle" { result["nextCursor"] = json!("opaque/ñ+=="); }
                        if !path.starts_with("/legacy") { result["resultType"] = json!("complete"); }
                        json!({"result":result})
                    },
                    "tools/call" => {
                        assert_eq!(headers.get("mcp-name").unwrap(), "reviewed");
                        assert_eq!(headers.get("mcp-param-tenant").unwrap().to_str().unwrap(), protocol::encoded_header("España").unwrap());
                        if path == "/tool-error" {
                            status = 400;
                            json!({"error":{"code":-32020,"message":"do not retry"}})
                        } else if path == "/input-required" {
                            json!({"result":{"resultType":"input_required","requestState":"own-state","inputRequests":{"ask":{"method":"elicitation/create","params":{}}}}})
                        } else if path == "/bad-output" {
                            json!({"result":{"resultType":"complete","content":[],"structuredContent":{"ok":false}}})
                        } else {
                            json!({"result":{"resultType":"complete","content":[{"type":"text","text":"own-fixture-secret"}],"structuredContent":{"ok":true},"undo_token":"own-fixture-secret","undoToken":"untrusted-provider-capability"}})
                        }
                    },
                    "resources/read" => {
                        assert_eq!(headers.get("mcp-name").unwrap().to_str().unwrap(), protocol::encoded_header("file:///España").unwrap());
                        json!({"result":{"resultType":"complete","contents":[]}})
                    },
                    _ => panic!("unexpected fixture method"),
                }
            };
            let mut envelope = json!({"jsonrpc":"2.0","id":v["id"]});
            for (key,value) in result.as_object().unwrap() { envelope[key] = value.clone(); }
            let mut response = axum::http::Response::builder().status(status).header("content-type","application/json; charset=utf-8");
            if rpc == "initialize" { response = response.header("Mcp-Session-Id", "own-session"); }
            response.body(Body::from(envelope.to_string())).unwrap()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Fixture {
        config: json!({"transport":"streamable_http","base_url":format!("http://{address}"),"endpoint":path,"protocol":"auto"}),
        log,
        server,
    }
}
fn names(f: &Fixture) -> Vec<String> {
    f.log.lock().unwrap().iter().map(|r| r.0.clone()).collect()
}

fn owned_directory() -> std::path::PathBuf {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../.work")
        .join(format!("mcp-owned-{}", crate::storage::id()));
    std::fs::create_dir_all(&path).unwrap();
    path
}
fn stdio_fixture(mode: &str, log: &std::path::Path) -> Value {
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/mocks/mcp-modern.mjs");
    json!({"transport":"stdio","command":"node","args":[script,mode,log],"protocol":"auto"})
}
fn stdio_log(log: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
#[tokio::test]
async fn stdio_negotiation_and_timeout_fallback_replace_only_the_probe_process() {
    let temp = owned_directory();
    for mode in ["modern", "dual-legacy", "timeout-legacy"] {
        let log = temp.join(format!("{mode}.jsonl"));
        let config = stdio_fixture(mode, &log);
        let result =
            crate::mcp_client::call(&config, None, "call", &json!({"name":"fixture","body":{}}))
                .await
                .unwrap();
        assert_eq!(result["structuredContent"]["ok"], true);
        let rows = stdio_log(&log);
        let methods: Vec<_> = rows
            .iter()
            .map(|v| v["method"].as_str().unwrap())
            .filter(|m| *m != "notifications/cancelled")
            .collect();
        assert_eq!(methods.iter().filter(|&&m| m == "tools/call").count(), 1);
        assert_eq!(methods[0], "server/discover");
        if mode == "modern" {
            assert_eq!(methods, ["server/discover", "tools/list", "tools/call"]);
        } else {
            assert_eq!(
                methods,
                [
                    "server/discover",
                    "initialize",
                    "notifications/initialized",
                    "tools/list",
                    "tools/call"
                ]
            );
        }
        if mode == "timeout-legacy" {
            assert_ne!(rows[0]["pid"], rows[2]["pid"]);
            assert_eq!(rows[1]["method"], "notifications/cancelled");
            assert_eq!(rows[1]["requestId"], 0);
        } else {
            assert_eq!(rows[0]["pid"], rows.last().unwrap()["pid"]);
        }
    }
}
#[tokio::test]
async fn stdio_framing_server_requests_and_notification_flood_stop_before_business() {
    let temp = owned_directory();
    for (mode, error) in [
        ("partial-frame", "MCP_STDIO_FRAME"),
        ("server-request", "MCP_RESPONSE_SCOPE"),
        ("notification-flood", "MCP_NOTIFICATION_LIMIT"),
    ] {
        let log = temp.join(format!("{mode}.jsonl"));
        let config = stdio_fixture(mode, &log);
        assert_eq!(
            crate::mcp_client::call(&config, None, "call", &json!({"name":"fixture","body":{}}))
                .await
                .unwrap_err(),
            error
        );
        let rows = stdio_log(&log);
        assert_eq!(rows[0]["method"], "server/discover");
        assert!(!rows.iter().any(|v| v["method"] == "tools/call"));
        if mode != "partial-frame" {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[1]["method"], "notifications/cancelled");
            assert_eq!(rows[1]["requestId"], 0);
        }
    }
}
#[tokio::test]
async fn cancelling_a_dispatched_stdio_tool_closes_the_process_without_replay() {
    let temp = owned_directory();
    let log = temp.join("cancel.jsonl");
    let config = stdio_fixture("hang-call", &log);
    let cancel = tokio_util::sync::CancellationToken::new();
    let input = json!({"name":"fixture","body":{}});
    let operation =
        crate::mcp_client::call_cancellable(&config, None, "call", &input, cancel.clone());
    tokio::pin!(operation);
    let result = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            tokio::select! { r=&mut operation=>break r,
                _=tokio::time::sleep(Duration::from_millis(10))=>{
                    if stdio_log(&log).iter().any(|v|v["method"]=="tools/call") { cancel.cancel(); }
                }
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(result.unwrap_err(), "MCP_OUTCOME_UNCERTAIN:MCP_CANCELLED");
    let rows = stdio_log(&log);
    assert_eq!(
        rows.iter().filter(|v| v["method"] == "tools/call").count(),
        1
    );
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[3]["method"], "notifications/cancelled");
    assert_eq!(rows[3]["requestId"], 3);
    // close() waited for the own process, rather than just detaching its future.
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        let pid = rows[0]["pid"].as_u64().unwrap() as u32;
        if let Ok(handle) = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
            let mut code = 0;
            unsafe {
                windows::Win32::System::Threading::GetExitCodeProcess(handle, &mut code).unwrap();
                CloseHandle(handle).unwrap();
            }
            assert_ne!(code, 259);
        }
    }
}

#[tokio::test]
async fn modern_negotiation_paging_and_headers_share_the_actual_wire_request() {
    let f = fixture("/modern").await;
    let value = crate::mcp_client::call(
        &f.config,
        Some("own-fixture-secret"),
        "call",
        &json!({"name":"reviewed","body":{"tenant":"España","n":2}}),
    )
    .await
    .unwrap();
    assert_eq!(value["structuredContent"]["ok"], true);
    assert_eq!(value["content"][0]["text"], "[REDACTED]");
    assert!(value.get("undo_token").is_none() && value.get("undoToken").is_none());
    assert_eq!(
        names(&f),
        ["server/discover", "tools/list", "tools/list", "tools/call"]
    );
    {
        let log = f.log.lock().unwrap();
        assert_eq!(log[2].1["params"]["cursor"], "opaque/ñ+==");
        assert_eq!(log[3].1["id"], 4);
    }
    let value = crate::mcp_client::call(
        &f.config,
        Some("own-fixture-secret"),
        "read_resource",
        &json!({"uri":"file:///España"}),
    )
    .await
    .unwrap();
    assert!(value["contents"].is_array());
}
#[tokio::test]
async fn protocol_fallback_only_precedes_business_and_modern_errors_never_downgrade() {
    for path in ["/legacy", "/legacy-params", "/legacy-empty"] {
        let f = fixture(path).await;
        crate::mcp_client::call(&f.config, Some("own-fixture-secret"), "tools", &json!({}))
            .await
            .unwrap();
        assert_eq!(
            names(&f),
            [
                "server/discover",
                "initialize",
                "notifications/initialized",
                "tools/list",
                "DELETE"
            ]
        );
    }
    for (path, error) in [
        ("/modern-error", "MCP_MODERN_PROTOCOL_ERROR"),
        ("/malformed-probe", "MCP_RESULT_TYPE"),
        ("/modern-404", "HTTP_STATUS_404"),
    ] {
        let f = fixture(path).await;
        assert_eq!(
            crate::mcp_client::call(&f.config, Some("own-fixture-secret"), "tools", &json!({}))
                .await
                .unwrap_err(),
            error
        );
        assert_eq!(names(&f), ["server/discover"]);
    }
    let mut f = fixture("/legacy").await;
    f.config["protocol"] = json!("modern");
    assert_eq!(
        crate::mcp_client::call(&f.config, Some("own-fixture-secret"), "tools", &json!({}))
            .await
            .unwrap_err(),
        "MCP_LEGACY_PROBE_ERROR"
    );
    assert_eq!(names(&f), ["server/discover"]);
}
#[tokio::test]
async fn assigned_legacy_session_is_retired_after_invalid_initialize_and_notification() {
    for (path, error) in [
        ("/legacy-bad-version", "MCP_PROTOCOL_UNSUPPORTED"),
        ("/legacy-bad-envelope", "MCP_ERROR_ENVELOPE"),
        ("/legacy-bad-notification", "MCP_INITIALIZED_REJECTED"),
    ] {
        let mut f = fixture(path).await;
        f.config["protocol"] = json!("legacy");
        assert_eq!(
            crate::mcp_client::call(&f.config, Some("own-fixture-secret"), "tools", &json!({}))
                .await
                .unwrap_err(),
            error
        );
        let log = names(&f);
        assert_eq!(log.first().unwrap(), "initialize");
        assert_eq!(log.last().unwrap(), "DELETE");
        assert_eq!(log.iter().filter(|s| s.as_str() == "DELETE").count(), 1);
        assert!(!log.iter().any(|s| s == "tools/list"));
    }
}
#[tokio::test]
async fn invalid_arguments_and_cursor_cycles_never_dispatch_a_tool() {
    let f = fixture("/modern").await;
    assert_eq!(
        crate::mcp_client::call(
            &f.config,
            Some("own-fixture-secret"),
            "call",
            &json!({"name":"reviewed","body":{"tenant":"España","n":"2"}})
        )
        .await
        .unwrap_err(),
        "MCP_TOOL_INPUT_SCHEMA"
    );
    assert!(!names(&f).iter().any(|s| s == "tools/call"));
    let f = fixture("/cycle").await;
    assert_eq!(
        crate::mcp_client::call(
            &f.config,
            Some("own-fixture-secret"),
            "call",
            &json!({"name":"reviewed","body":{"tenant":"España","n":2}})
        )
        .await
        .unwrap_err(),
        "MCP_CURSOR_CYCLE"
    );
    assert_eq!(names(&f), ["server/discover", "tools/list", "tools/list"]);
}
#[tokio::test]
async fn post_dispatch_errors_input_required_and_invalid_output_never_replay() {
    for (path, error) in [
        ("/tool-error", "MCP_OUTCOME_UNCERTAIN:HTTP_STATUS_400"),
        (
            "/input-required",
            "MCP_OUTCOME_UNCERTAIN:MCP_INPUT_REQUIRED",
        ),
        (
            "/bad-output",
            "MCP_OUTCOME_UNCERTAIN:MCP_TOOL_OUTPUT_SCHEMA",
        ),
    ] {
        let f = fixture(path).await;
        assert_eq!(
            crate::mcp_client::call(
                &f.config,
                Some("own-fixture-secret"),
                "call",
                &json!({"name":"reviewed","body":{"tenant":"España","n":2}})
            )
            .await
            .unwrap_err(),
            error
        );
        assert_eq!(
            names(&f),
            ["server/discover", "tools/list", "tools/list", "tools/call"]
        );
    }
}
#[tokio::test]
async fn dropping_a_session_retires_it_and_modern_health_never_opens_one() {
    let mut f = fixture("/legacy").await;
    f.config["protocol"] = json!("legacy");
    let session = Session::open(&f.config, Some("own-fixture-secret"))
        .await
        .unwrap();
    drop(session);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if names(&f).last().is_some_and(|s| s == "DELETE") {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        names(&f),
        ["initialize", "notifications/initialized", "DELETE"]
    );
    let f = fixture("/modern").await;
    let health =
        crate::mcp_client::call(&f.config, Some("own-fixture-secret"), "health", &json!({}))
            .await
            .unwrap();
    assert_eq!(health["protocolVersion"], protocol::MODERN);
    assert_eq!(health["businessToolExecuted"], false);
    assert_eq!(names(&f), ["server/discover"]);
}
#[tokio::test]
async fn revoked_request_guard_blocks_each_rpc_close_and_detached_drop_cleanup() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for explicit_close in [false, true] {
        let mut f = fixture("/legacy").await;
        f.config["protocol"] = json!("legacy");
        let live = Arc::new(AtomicBool::new(true));
        let check = live.clone();
        let mut auth = HeaderMap::new();
        let mut token = reqwest::header::HeaderValue::from_static("Bearer own-fixture-secret");
        token.set_sensitive(true);
        auth.insert(reqwest::header::AUTHORIZATION, token);
        let material = crate::mcp_credentials::Material {
            headers: auth,
            env: Default::default(),
            needles: vec![],
            guard: Some(crate::mcp_request_guard::Guard::new(move || {
                if check.load(Ordering::SeqCst) {
                    Ok(())
                } else {
                    Err("OWNED_REVOKED".into())
                }
            })),
        };
        let mut session = Session::open_private(&f.config, &material).await.unwrap();
        live.store(false, Ordering::SeqCst);
        assert_eq!(
            session
                .rpc(
                    &crate::mcp_client::request("tools", &json!({})).unwrap(),
                    &[]
                )
                .await
                .unwrap_err(),
            "OWNED_REVOKED"
        );
        if explicit_close {
            session.close().await;
        }
        drop(session);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(names(&f), ["initialize", "notifications/initialized"]);
    }
    let mut f = fixture("/modern").await;
    f.config["auth"] = json!({"type":"oauth","issuer":"https://owned-as.invalid"});
    assert!(
        matches!(Session::open(&f.config,Some("own-fixture-secret")).await,Err(e) if e=="MCP_OAUTH_GRANT_REQUIRED")
    );
    assert!(names(&f).is_empty());
}
