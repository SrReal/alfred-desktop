//! Real official SDK contracts; all processes, profiles and HTTP endpoints are
//! owned fixtures. This is not provider/account/TLS/human-consent evidence.
use super::*;
use tokio::io::{AsyncBufReadExt, AsyncReadExt};

pub(super) fn node() -> PathBuf {
    let output = std::process::Command::new("where.exe")
        .arg("node.exe")
        .output()
        .unwrap();
    assert!(output.status.success());
    PathBuf::from(
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .trim(),
    )
    .canonicalize()
    .unwrap()
}
fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/mcp-sdk-server.mjs")
        .canonicalize()
        .unwrap()
}

struct HttpSdk {
    child: tokio::process::Child,
    endpoint: String,
}
impl HttpSdk {
    async fn new() -> Self {
        let mut command = tokio::process::Command::new(node());
        command
            .arg(script())
            .env_clear()
            // Native Node initialization on Windows needs SystemRoot. Keep the
            // fixture environment explicit; inherit no provider credentials.
            .env(
                "SystemRoot",
                std::env::var_os("SystemRoot").expect("Windows SystemRoot"),
            )
            .creation_flags(0x08000000)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        let mut stdout = tokio::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        let count = tokio::time::timeout(Duration::from_secs(10), stdout.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        if count == 0 {
            if child.try_wait().unwrap().is_none() {
                child.kill().await.unwrap();
            }
            let status = child.wait().await.unwrap();
            let mut stderr = Vec::new();
            tokio::time::timeout(
                Duration::from_secs(3),
                child
                    .stderr
                    .take()
                    .unwrap()
                    .take(8193)
                    .read_to_end(&mut stderr),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(stderr.len() <= 8192);
            panic!(
                "Owned SDK fixture exited before readiness: {status}: {}",
                String::from_utf8_lossy(&stderr)
            );
        }
        assert!(line.len() <= 2048);
        let ready: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["scope"], "owned-loopback-sdk-fixture");
        assert_eq!(ready["pins"], json!({"server":"2.3.0","node":"2.1.1"}));
        let endpoint = ready["endpoint"].as_str().unwrap().to_string();
        let url = reqwest::Url::parse(&endpoint).unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert_eq!(url.path(), "/mcp");
        Self { child, endpoint }
    }
    async fn count(&self) -> u64 {
        let mut url = reqwest::Url::parse(&self.endpoint).unwrap();
        url.set_path("/__owned_sdk_stats");
        let response = crate::http_client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap()
            .get(url)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let bytes = response.bytes().await.unwrap();
        assert!(bytes.len() < 2048);
        serde_json::from_slice::<Value>(&bytes).unwrap()["acceptedCalls"]
            .as_u64()
            .unwrap()
    }
    async fn finish(mut self) {
        self.child.kill().await.unwrap();
        self.child.wait().await.unwrap();
    }
}

async fn call_contract(f: &Fixture) -> Value {
    let tools = f
        .approve("mcp.tools", json!({}), "allow", || ())
        .await
        .unwrap();
    assert_eq!(tools["tools"].as_array().unwrap().len(), 1);
    assert_eq!(tools["tools"][0]["name"], "owned_echo");
    let input = json!({"name":"owned_echo","body":{"text":"Señor 🐾"},"expected":tools["tools"][0]["alfredpetObservation"]});
    let reviewed = prepare(&f.core, "mcp.call", &f.input(input.clone()))
        .await
        .unwrap();
    assert_eq!(reviewed.risk(), Risk::Mandatory);
    drop(reviewed);
    for answer in ["deny", "session"] {
        assert_eq!(
            f.approve("mcp.call", input.clone(), answer, || ())
                .await
                .unwrap_err(),
            "PERMISSION_DENIED_OR_TIMEOUT"
        );
    }
    let invalid = json!({"name":"owned_echo","body":{"text":42},"expected":input["expected"]});
    assert!(prepare(&f.core, "mcp.call", &f.input(invalid))
        .await
        .is_err());
    let result = f
        .approve("mcp.call", input.clone(), "allow", || ())
        .await
        .unwrap();
    assert_eq!(result["content"][0]["text"], "Señor 🐾");
    assert_eq!(result["structuredContent"]["echo"], "Señor 🐾");
    assert!(result.get("undo_token").is_none());
    let audit = f
        .core
        .store
        .query(
            "SELECT input_summary,output_summary,undo_token FROM activity_events",
            &[],
        )
        .unwrap();
    assert!(!audit.to_string().contains("Señor"));
    assert!(f.core.pending.lock().unwrap().is_empty());
    input
}

#[tokio::test]
async fn official_sdk_http_modern_and_legacy_use_core_permissions_exact_headers_and_schemas() {
    for era in ["modern", "legacy"] {
        let f = Fixture::new().await;
        let sdk = HttpSdk::new().await;
        let endpoint = reqwest::Url::parse(&sdk.endpoint).unwrap();
        let mut raw = f.raw();
        raw["base_url"] = json!(endpoint.origin().ascii_serialization());
        raw["protocol"] = json!(era);
        raw["permissions"] = json!({"tools":{"owned_echo":true}});
        f.save(&raw, None).await.unwrap();
        let input = call_contract(&f).await;
        assert_eq!(sdk.count().await, 1);
        revoke(&f.core).unwrap();
        assert!(f.approve("mcp.call", input, "allow", || ()).await.is_err());
        assert_eq!(sdk.count().await, 1);
        assert!(f.m.lock().unwrap().seen.is_empty());
        sdk.finish().await;
        f.clean().await;
    }
}

#[tokio::test]
async fn official_sdk_stdio_modern_and_legacy_use_owned_pinned_program_and_core_permissions() {
    for era in ["modern", "legacy"] {
        let f = Fixture::new().await;
        let raw = json!({"profile_version":1,"transport":"stdio","protocol":era,"command":node(),"args":[script(),"--stdio"],"auth":{"type":"none"},"permissions":{"tools":{"owned_echo":true}}});
        f.save(&raw, None).await.unwrap();
        let input = call_contract(&f).await;
        revoke(&f.core).unwrap();
        assert!(f.approve("mcp.call", input, "allow", || ()).await.is_err());
        assert!(f.m.lock().unwrap().seen.is_empty());
        f.clean().await;
    }
}

#[tokio::test]
async fn explicit_modern_stdio_owned_sdk_startup_longer_than_auto_probe_succeeds() {
    let f = Fixture::new().await;
    let raw = json!({"profile_version":1,"transport":"stdio","protocol":"modern","command":node(),"args":[script(),"--stdio","--owned-delay-discovery"],"auth":{"type":"none"},"permissions":{"tools":{"owned_echo":true}}});
    f.save(&raw, None).await.unwrap();
    let began = Instant::now();
    let result = f
        .approve("mcp.health", json!({}), "allow", || ())
        .await
        .unwrap();
    assert_eq!(result["protocolVersion"], client::protocol::MODERN);
    assert_eq!(result["businessToolExecuted"], false);
    assert!(began.elapsed() >= Duration::from_secs(4));
    assert!(f.m.lock().unwrap().seen.is_empty());
    f.clean().await;
}
