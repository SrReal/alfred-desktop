use super::*;
use crate::process;
use axum::{
    body::Body,
    http::{Request, Response, StatusCode},
    routing::any,
    Router,
};
use std::sync::{Arc, Mutex};
use tokio::io::BufReader;

#[test]
fn invalid_header_tools_are_excluded_with_bounded_diagnostics_without_hiding_valid_tools() {
    let raw = json!({"resultType":"complete","tools":[
        {"name":"valid","inputSchema":{"type":"object"}},
        {"name":"invalid","inputSchema":{"type":"object","properties":{"v":{"type":"number","x-mcp-header":"Number"}}}}
    ]});
    let (page, tools) = tool_page(&raw, true).unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(page["tools"].as_array().unwrap().len(), 1);
    assert_eq!(page["tools"][0]["name"], "valid");
    assert_eq!(page["alfredpetValidation"]["excludedInvalidTools"], 1);
    assert_eq!(
        page["alfredpetValidation"]["warnings"][0]["name"],
        "invalid"
    );
    assert_eq!(
        page["alfredpetValidation"]["warnings"][0]["reason"],
        "MCP_HEADER_TYPE"
    );
    assert_eq!(tool_page(&json!({"tools":[{"name":"same","inputSchema":{"type":"object"}},{"name":"same","inputSchema":{"type":"object"}}]}),true).err().unwrap(),"MCP_DUPLICATE_TOOL");
}

#[test]
fn catalog_is_closed_cursor_is_opaque_and_all_remote_hints_keep_mandatory_policy() {
    let connection = "10000000-0000-4000-8000-000000000000";
    for action in [
        "health",
        "tools",
        "resources",
        "prompts",
        "call",
        "read_resource",
        "get_prompt",
    ] {
        let tool = format!("mcp.{action}");
        assert_eq!(
            crate::policy::classify(&tool, &json!({"annotations":{"readOnlyHint":true}})),
            crate::policy::Risk::Mandatory
        );
        let validator = jsonschema::validator_for(&tool_schema(&tool)).unwrap();
        assert!(
            !validator.is_valid(&json!({"connection":connection,"override":"https://other.test"}))
        );
    }
    let v = jsonschema::validator_for(&tool_schema("mcp.tools")).unwrap();
    assert!(v.is_valid(&json!({"connection":connection,"cursor":"opaque/ñ+=="})));
    assert!(!v.is_valid(&json!({"connection":connection,"cursor":1})));
    assert!(
        request("tools", &json!({"cursor":"opaque/ñ+=="})).unwrap()["params"]["cursor"]
            == "opaque/ñ+=="
    );
    assert_eq!(
        request("get_prompt", &json!({"name":"prompt","body":{"n":1}})).unwrap_err(),
        "MCP_PROMPT_ARGUMENTS"
    );
}
#[tokio::test]
async fn core_privacy_stops_mcp_before_permission_or_connection_lookup_and_audit_omits_content() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../.work")
        .join(format!("mcp-core-{}", crate::storage::id()));
    let core = std::sync::Arc::new(
        crate::runtime::Core::new(
            root,
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap(),
    );
    core.store
        .execute(
            "UPDATE module_configs SET enabled=1 WHERE module_id='mcp'",
            &[],
        )
        .unwrap();
    core.store.set_setting("privacy", &json!(true)).unwrap();
    let input = json!({"connection":"10000000-0000-4000-8000-000000000000"});
    assert_eq!(
        core.execute("mcp.health", input).await.unwrap_err(),
        "PRIVACY_DENIED"
    );
    assert!(core.pending.lock().unwrap().is_empty());
    let private = json!({"body":{"opaque":"own-secret-fragment"},"result":{"text":"own-secret-fragment"},"undo_token":"own-secret-fragment"});
    let omitted = crate::policy::tool_redact("mcp.call", &private);
    assert_eq!(
        omitted,
        json!({"content":"[MCP_CONTENT_OMITTED]","automatic_replay":false})
    );
    core.store
        .audit("mcp.call", "Mandatory", &private, "failed", &private, 0)
        .unwrap();
    let audit = core
        .store
        .query(
            "SELECT input_summary,output_summary,undo_token FROM activity_events WHERE tool_id='mcp.call'",
            &[],
        )
        .unwrap();
    assert!(!audit.to_string().contains("own-secret-fragment"));
    assert!(audit[0]["undo_token"].is_null());
}
#[tokio::test]
async fn streamable_http_lifecycle_sse_errors_and_auth_scope() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured = log.clone();
    let router=Router::new().fallback(any(move |request:Request<Body>| {
            let log=captured.clone();
            async move {
                let path=request.uri().path().to_string();
                assert_eq!(request.headers().get("authorization").unwrap(),"Bearer fixture-token");
                if path=="/unauthorized" { return Response::builder().status(StatusCode::UNAUTHORIZED).body(Body::empty()).unwrap(); }
                let method=request.method().clone();
                if method==reqwest::Method::DELETE {
                    assert_eq!(request.headers().get("mcp-session-id").unwrap(),"fixture-session");
                    log.lock().unwrap().push("DELETE".into());
                    return Response::builder().status(204).body(Body::empty()).unwrap();
                }
                assert_eq!(request.headers().get("accept").unwrap(),"application/json, text/event-stream");
                let headers=request.headers().clone();
                let bytes=axum::body::to_bytes(request.into_body(),LIMIT).await.unwrap();
                let value:Value=serde_json::from_slice(&bytes).unwrap();
                let rpc=value["method"].as_str().unwrap();
                log.lock().unwrap().push(rpc.into());
                if rpc=="initialize" {
                    return Response::builder().header("content-type","application/json").header("Mcp-Session-Id","fixture-session").body(Body::from(json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":VERSION,"capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}}).to_string())).unwrap();
                }
                assert_eq!(headers.get("mcp-session-id").unwrap(),"fixture-session");
                assert_eq!(headers.get("mcp-protocol-version").unwrap(),VERSION);
                if rpc=="notifications/initialized" { return Response::builder().status(202).body(Body::empty()).unwrap(); }
                if path=="/wrong" { return Response::builder().header("content-type","application/json").body(Body::from(json!({"jsonrpc":"2.0","id":999,"result":{}}).to_string())).unwrap(); }
                if path=="/error" { return Response::builder().header("content-type","application/json").body(Body::from(json!({"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"fixture"}}).to_string())).unwrap(); }
                let data=json!({"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"fixture","inputSchema":{"type":"object"}}]}});
                Response::builder().header("content-type","text/event-stream").body(Body::from(format!(": keepalive\r\n\r\ndata: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}}\r\n\r\ndata: {data}\r\n\r\n"))).unwrap()
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut config = json!({"transport":"http","base_url":format!("http://{address}"),"endpoint":"/mcp","protocol":"legacy"});
    let result = call(&config, Some("fixture-token"), "tools", &json!({}))
        .await
        .unwrap();
    assert_eq!(result["tools"][0]["name"], "fixture");
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            "initialize",
            "notifications/initialized",
            "tools/list",
            "DELETE"
        ]
    );
    for (endpoint, error) in [
        ("/wrong", "MCP_RESPONSE_SCOPE"),
        ("/error", "MCP_REMOTE_ERROR"),
        ("/unauthorized", "HTTP_STATUS_401"),
    ] {
        config["endpoint"] = json!(endpoint);
        assert_eq!(
            call(&config, Some("fixture-token"), "tools", &json!({}))
                .await
                .unwrap_err(),
            error
        );
    }
    server.abort();
}
#[tokio::test]
async fn stdio_handshake_and_malformed_or_unsupported_protocol() {
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/mocks/mcp-contract.mjs");
    for (mode, error) in [
        ("ok", None),
        ("malformed", Some("MCP_JSON")),
        ("old", Some("MCP_PROTOCOL_UNSUPPORTED")),
    ] {
        let config =
            json!({"transport":"stdio","command":"node","args":[fixture,mode],"protocol":"legacy"});
        let result = call(&config, None, "tools", &json!({})).await;
        if let Some(error) = error {
            assert_eq!(result.unwrap_err(), error);
        } else {
            assert_eq!(result.unwrap()["tools"][0]["name"], "fixture");
        }
    }
}
#[tokio::test]
async fn response_limits_before_allocation() {
    let mut reader = BufReader::new(&b"12345\n"[..]);
    assert_eq!(
        process::bounded_line(&mut reader, 4).await.unwrap_err(),
        "PROCESS_LINE_LIMIT"
    );
    let mut reader = BufReader::new(&b"one\r\ntwo\n"[..]);
    assert_eq!(
        process::bounded_line(&mut reader, 10)
            .await
            .unwrap()
            .unwrap(),
        "one"
    );
    assert_eq!(
        process::bounded_line(&mut reader, 10)
            .await
            .unwrap()
            .unwrap(),
        "two"
    );
    assert!(process::bounded_line(&mut reader, 10)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        rpc_result(
            &json!({"jsonrpc":"2.0","id":2,"result":{"isError":true}}),
            2
        )
        .unwrap_err(),
        "MCP_TOOL_ERROR"
    );
}
