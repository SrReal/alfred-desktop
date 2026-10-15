use super::*;
use crate::{asset_files, catalog};
use std::{os::windows::fs::MetadataExt, path::PathBuf, sync::Mutex};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

struct Profile {
    core: Arc<Core>,
    identity: String,
}
impl Profile {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ap-browser-research-{}", storage::id()));
        std::fs::create_dir(&root).unwrap();
        let identity = asset_files::identity(&asset_files::directory(&root).unwrap()).unwrap();
        let core = Arc::new(
            Core::new(root, PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap(),
        );
        Self { core, identity }
    }
    fn enable(&self) {
        self.core
            .store
            .execute(
                "UPDATE module_configs SET enabled=1 WHERE module_id='browser'",
                &[],
            )
            .unwrap();
    }
    fn finish(self) {
        let root = self.core.root.clone();
        assert_eq!(
            root.parent().unwrap().canonicalize().unwrap(),
            std::env::temp_dir().canonicalize().unwrap()
        );
        assert_eq!(
            asset_files::identity(&asset_files::directory(&root).unwrap()).unwrap(),
            self.identity
        );
        let mut paths = vec![root.clone()];
        while let Some(path) = paths.pop() {
            assert_eq!(
                std::fs::symlink_metadata(&path).unwrap().file_attributes() & 0x400,
                0
            );
            if path.is_dir() {
                paths.extend(std::fs::read_dir(path).unwrap().map(|p| p.unwrap().path()));
            }
        }
        drop(self.core);
        std::fs::remove_dir_all(root).unwrap();
    }
}
struct Server {
    port: u16,
    worker: tokio::task::JoinHandle<()>,
    requests: Arc<Mutex<Vec<String>>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.worker.abort();
    }
}
impl Server {
    async fn new() -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let worker = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_request(&mut stream).await;
                recorded.lock().unwrap().push(request.clone());
                let path = request.split_whitespace().nth(1).unwrap();
                let response = fixture_response(path);
                let _ = stream.write_all(&response).await;
            }
        });
        Self {
            port,
            worker,
            requests,
        }
    }
    fn url(&self, path: &str) -> String {
        format!("http://owned.test:{}{path}", self.port)
    }
    fn resolver(&self) -> Resolver {
        Resolver::OwnedFixture { port: self.port }
    }
}
async fn read_request(stream: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0; 1];
        if stream.read(&mut byte).await.unwrap() == 0 {
            break;
        }
        bytes.push(byte[0]);
        assert!(bytes.len() < 16384);
    }
    String::from_utf8(bytes).unwrap()
}
fn response(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = format!("HTTP/1.1 {status}\r\nConnection: close\r\n{headers}\r\n").into_bytes();
    bytes.extend_from_slice(body);
    bytes
}
fn fixture_response(path: &str) -> Vec<u8> {
    match path {
        "/redirect" => response("302 Found","Location: /html\r\n",b""),
        "/loop" => response("302 Found","Location: /loop\r\n",b""),
        "/private" => response("302 Found","Location: http://127.0.0.1/owned\r\n",b""),
        "/credential" => response("302 Found","Location: /html?%74oken=OWNED_SENTINEL\r\n",b""),
        "/error" => response("503 Unavailable","Content-Type: text/plain\r\n",b"OWNED_ERROR_BODY_NOT_EXPORTED"),
        "/large-header" => response("200 OK",&format!("Content-Type: text/plain\r\nContent-Length: {}\r\n",BODY_LIMIT+1),b""),
        "/large-chunk" => response("200 OK","Content-Type: text/plain\r\nTransfer-Encoding: chunked\r\n",format!("{:x}\r\n{}\r\n0\r\n\r\n",BODY_LIMIT+1,"x".repeat(BODY_LIMIT+1)).as_bytes()),
        "/truncated" => response("200 OK","Content-Type: text/plain\r\nContent-Length: 999\r\n",b"short"),
        "/charset" => response("200 OK","Content-Type: text/plain; charset=windows-1252\r\n",b"text"),
        "/encoding" => response("200 OK","Content-Type: text/plain\r\nContent-Encoding: gzip\r\n",b"uncompressed fixture"),
        "/binary" => response("200 OK","Content-Type: application/octet-stream\r\n",b"bytes"),
        "/missing-type" => response("200 OK","",b"text"),
        "/invalid-utf8" => response("200 OK","Content-Type: text/plain\r\n",&[0xff,0xfe]),
        "/plain" => response("200 OK","Content-Type: text/plain; charset=\"UTF-8\"\r\nSet-Cookie: owned=discarded\r\n", "\u{feff}Public 😀 text".as_bytes()),
        "/json" => response("200 OK","Content-Type: application/json\r\n",br#"{"owned":true}"#),
        path if path.starts_with("/limit/") => {
            let count=path.trim_start_matches("/limit/").parse::<u32>().unwrap();
            response("302 Found",&format!("Location: /limit/{}\r\n",count+1),b"")
        },
        _ => response("200 OK","Content-Type: text/html; charset=utf-8\r\n",br#"<title>Owned &amp; inert</title><article>Public <a href="/source">source</a> text</article><input value="OWNED_SECRET_VALUE"><textarea>OWNED_SECRET_VALUE</textarea><select><option>OWNED_SECRET_VALUE</option></select><div contenteditable>OWNED_SECRET_VALUE</div><script>OWNED_SECRET_VALUE</script><div hidden>OWNED_SECRET_VALUE</div><span aria-hidden="true">OWNED_SECRET_VALUE</span><span style="display:none">OWNED_SECRET_VALUE</span><a href="/bad?%74oken=OWNED_SENTINEL">Bad credential link</a><a href="javascript:alert(1)">Bad script link</a>"#),
    }
}

#[test]
fn url_and_audit_reject_encoded_credentials_without_echoing_them() {
    for raw in [
        "file:///c:/owned",
        "ftp://example.org/",
        "https://owned:OWNED_SENTINEL@example.org/",
        "https://example.org/?%74oken=OWNED_SENTINEL",
        "https://example.org/#access%5ftoken=OWNED_SENTINEL",
        "https://example.org/?api-key=OWNED_SENTINEL",
        "https://example.org:0/",
        " https://example.org/",
        "https://example.org/\n",
    ] {
        assert!(url(raw).is_err(), "{raw}");
        assert_eq!(review(&json!({"url":raw}))["url"], "[REDACTED]");
    }
    assert_eq!(
        url("https://example.org/a?q=public#section")
            .unwrap()
            .as_str(),
        "https://example.org/a?q=public"
    );
    assert!(!review(&json!({"urls":["https://u:OWNED_SENTINEL@example.org/","https://example.org/?sid=OWNED_SENTINEL"]})).to_string().contains("OWNED_SENTINEL"));
    assert!(url(&format!("https://example.org/{}", "a".repeat(8192))).is_err());
}
#[test]
fn public_address_policy_dns_sets_and_peer_pinning_cover_boundaries() {
    for ip in [
        "0.1.2.3",
        "10.0.0.1",
        "100.64.0.1",
        "100.127.255.255",
        "127.0.0.1",
        "169.254.169.254",
        "172.16.0.1",
        "172.31.255.255",
        "192.0.0.9",
        "192.0.2.1",
        "192.88.99.1",
        "192.168.0.1",
        "198.18.0.1",
        "198.19.255.255",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "255.255.255.255",
        "::",
        "::1",
        "::ffff:127.0.0.1",
        "64:ff9b::a00:1",
        "fc00::1",
        "fe80::1",
        "ff02::1",
        "2001::1",
        "2001:1ff::1",
        "2001:db8::1",
        "2002::1",
        "3fff::1",
        "3fff:fff::1",
    ] {
        assert!(!public_ip(ip.parse().unwrap()), "{ip}");
    }
    for ip in [
        "8.8.8.8",
        "100.63.255.255",
        "100.128.0.1",
        "172.15.255.255",
        "172.32.0.1",
        "198.17.255.255",
        "198.20.0.1",
        "::ffff:8.8.8.8",
        "2001:4860:4860::8888",
        "2606:4700::1111",
        "3fff:1000::1",
    ] {
        assert!(public_ip(ip.parse().unwrap()), "{ip}");
    }
    for host in [
        "localhost",
        "owned.localhost.",
        "owned.local",
        "owned.internal",
        "owned.test",
        "owned.invalid",
        "owned.example",
        "router.home.arpa",
        "singlelabel",
    ] {
        assert!(!public_host(host));
    }
    for raw in [
        "http://2130706433/",
        "http://0x7f000001/",
        "http://0177.0.0.1/",
    ] {
        assert!(!public_host(&hostname(&url(raw).unwrap()).unwrap()));
    }
    let public: SocketAddr = "8.8.8.8:443".parse().unwrap();
    let private: SocketAddr = "127.0.0.1:443".parse().unwrap();
    assert!(validate_addresses(&[public], public_ip).is_ok());
    for list in [vec![], vec![public, private], vec![public; 17]] {
        assert!(validate_addresses(&list, public_ip).is_err());
    }
    assert!(peer_pinned(Some(public), &[public]));
    assert!(peer_pinned(
        Some("[::ffff:8.8.8.8]:443".parse().unwrap()),
        &[public]
    ));
    assert!(!peer_pinned(Some(private), &[public]));
    assert!(!peer_pinned(Some("8.8.8.8:80".parse().unwrap()), &[public]));
    assert!(!peer_pinned(None, &[public]));
}

#[test]
fn redirects_and_content_types_are_revalidated_without_downgrade() {
    let base = url("https://example.org/path").unwrap();
    assert_eq!(
        redirect(&base, "/other").unwrap().as_str(),
        "https://example.org/other"
    );
    assert_eq!(
        redirect(&base, "http://example.org/").unwrap_err(),
        "WEB_TLS_DOWNGRADE_DENIED"
    );
    assert_eq!(
        redirect(&base, "https://u:OWNED_SENTINEL@example.org/").unwrap_err(),
        "WEB_URL_DENIED"
    );
    assert_eq!(
        redirect(&base, "/other#csrf=OWNED_SENTINEL").unwrap_err(),
        "WEB_URL_SECRET_DENIED"
    );
    assert_eq!(
        content_kind(Some("Text/HTML; CHARSET=\"UTF-8\"")).unwrap(),
        "html"
    );
    assert!(content_kind(Some("text/html; charset=latin1")).is_err());
    assert!(content_kind(None).is_err());
    assert!(content_kind(Some("image/png")).is_err());
}

#[test]
fn inert_html_and_search_provider_results_remain_bounded_untrusted_sources() {
    let base = url("https://example.org/dir/").unwrap();
    let source = String::from_utf8(fixture_response("/html"))
        .unwrap()
        .split("\r\n\r\n")
        .nth(1)
        .unwrap()
        .to_owned();
    let extracted = html(&source, &base, 64000);
    assert_eq!(extracted["title"], "Owned & inert");
    assert!(!extracted.to_string().contains("OWNED_SECRET_VALUE"));
    assert!(!extracted.to_string().contains("OWNED_SENTINEL"));
    assert_eq!(extracted["links"].as_array().unwrap().len(), 1);
    assert_eq!(extracted["links"][0]["url"], "https://example.org/source");
    assert_eq!(extracted["css_and_javascript_evaluated"], false);
    let emoji = html("<p>Public 😀 next</p>", &base, 8);
    assert_eq!(emoji["text"], "Public 😀");
    assert_eq!(emoji["truncated"], true);
    let many = html(&format!("<p>{}</p>", "x ".repeat(70000)), &base, 64000);
    assert_eq!(many["text"].as_str().unwrap().chars().count(), 64000);
    assert_eq!(many["truncated"], true);
    let deep = html(&"<div></div>".repeat(10001), &base, 10);
    assert_eq!(deep["truncated"], true);
    let links = html(&"<a href='/x'>Link</a>".repeat(101), &base, 64000);
    assert_eq!(links["links"].as_array().unwrap().len(), 100);
    assert_eq!(links["links_truncated"], true);
    let provider = url("https://html.duckduckgo.com/html/?q=owned").unwrap();
    let fixture = r#"<div class="result__body"><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.org%2Fsource">First &amp; source</a><span class="result__snippet">Owned snippet</span></div><div class="result__body"><a class="result__a" href="https://example.net/second">Second</a><span class="result__snippet">Second snippet</span></div><a class="result__a" href="https://user:OWNED_SENTINEL@example.org/">Credential</a><a class="result__a" href="http://127.0.0.1/">Private</a><a class="result__a" href="https://example.org/source">Duplicate</a>"#;
    let result = search_results(fixture, &provider, 20).unwrap();
    assert_eq!(result["results"].as_array().unwrap().len(), 2);
    assert_eq!(result["results"][0]["title"], "First & source");
    assert_eq!(result["results"][0]["snippet"], "Owned snippet");
    assert_eq!(result["untrusted_content"], true);
    assert!(!result.to_string().contains("OWNED_SENTINEL"));
    assert_eq!(
        search_results(fixture, &provider, 1).unwrap()["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        search_results("<form id='challenge-form'></form>", &provider, 10).unwrap_err(),
        "WEB_SEARCH_PROVIDER_CHALLENGE"
    );
    assert_eq!(
        search_results("<p>Changed markup</p>", &provider, 10).unwrap_err(),
        "WEB_SEARCH_UNSUPPORTED_PROVIDER_MARKUP"
    );
    assert!(
        search_results("<div class='no-results'>No results</div>", &provider, 10).unwrap()
            ["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // Inert HTML deliberately cannot infer visibility from arbitrary CSS classes.
    assert!(html(
        "<p class='unknown-css'>Visible in source only</p>",
        &base,
        100
    )["text"]
        .as_str()
        .unwrap()
        .contains("Visible in source"));
}

#[tokio::test]
async fn owned_http_fetch_and_redirects_enforce_body_limits_and_transport_metadata() {
    let profile = Profile::new();
    profile.enable();
    let server = Server::new().await;
    let control = Control(&profile.core);
    let fetched = fetch(
        &control,
        &server.resolver(),
        &server.url("/redirect"),
        16000,
    )
    .await
    .unwrap();
    assert_eq!(fetched.data["title"], "Owned & inert");
    assert_eq!(fetched.data["url"], server.url("/html"));
    assert_eq!(fetched.data["redirects"], 1);
    assert_eq!(fetched.data["untrusted_content"], true);
    assert_eq!(fetched.data["authenticated"], false);
    assert_eq!(
        fetched.data["sha256"].as_str().unwrap(),
        format!("{:x}", Sha256::digest(fetched.source.as_bytes()))
    );
    let plain = fetch(&control, &server.resolver(), &server.url("/plain"), 8)
        .await
        .unwrap();
    assert_eq!(plain.data["text"], "Public 😀");
    assert_eq!(plain.data["truncated"], true);
    assert_eq!(
        fetch(&control, &server.resolver(), &server.url("/json"), 100)
            .await
            .unwrap()
            .data["content_type"],
        "json"
    );
    for (path, error) in [
        ("/loop", "WEB_REDIRECT_LOOP"),
        ("/limit/0", "WEB_REDIRECT_LIMIT"),
        ("/private", "WEB_DESTINATION_DENIED"),
        ("/credential", "WEB_URL_SECRET_DENIED"),
        ("/error", "WEB_HTTP_STATUS_503"),
        ("/large-header", "WEB_BODY_LIMIT"),
        ("/large-chunk", "WEB_BODY_LIMIT"),
        ("/truncated", "WEB_BODY_READ_FAILED"),
        ("/charset", "WEB_UTF8_REQUIRED"),
        ("/encoding", "WEB_CONTENT_ENCODING_DENIED"),
        ("/binary", "WEB_CONTENT_TYPE_DENIED"),
        ("/missing-type", "WEB_CONTENT_TYPE_REQUIRED"),
        ("/invalid-utf8", "WEB_UTF8_REQUIRED"),
    ] {
        assert_eq!(
            fetch(&control, &server.resolver(), &server.url(path), 100)
                .await
                .err()
                .unwrap(),
            error,
            "{path}"
        );
    }
    for request in server.requests.lock().unwrap().iter() {
        let request = request.to_ascii_lowercase();
        assert!(request.starts_with("get "));
        assert!(request.contains("accept-encoding: identity"));
        assert!(
            !request.contains("authorization:")
                && !request.contains("cookie:")
                && !request.contains("referer:")
        );
    }
    assert_eq!(
        fetch(&control, &Resolver::System, &server.url("/plain"), 100)
            .await
            .err()
            .unwrap(),
        "WEB_DESTINATION_DENIED"
    );
    profile.finish();
}

#[tokio::test]
async fn research_partial_failures_keep_source_indices_and_redact_invalid_urls() {
    let profile = Profile::new();
    profile.enable();
    let server = Server::new().await;
    let input = json!({"urls":[server.url("/plain"),server.url("/error"),"https://u:OWNED_SENTINEL@example.org/",format!("{}#section",server.url("/plain"))]});
    let result = dispatch_with(
        &profile.core,
        "browser.research",
        &input,
        &server.resolver(),
    )
    .await
    .unwrap();
    assert_eq!(result["sources"][0]["status"], "succeeded");
    assert_eq!(result["sources"][1]["error"], "WEB_HTTP_STATUS_503");
    assert_eq!(result["sources"][2]["index"], 2);
    assert_eq!(result["sources"][2]["url"], "[REDACTED]");
    assert_eq!(result["sources"][3]["error"], "WEB_DUPLICATE_SOURCE");
    assert!(!result.to_string().contains("OWNED_SENTINEL"));
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    profile.finish();
}

#[tokio::test]
async fn public_catalog_privacy_policy_and_audit_block_network_without_an_edge_host() {
    let profile = Profile::new();
    assert_eq!(
        profile
            .core
            .execute("browser.fetch", json!({"url":"http://127.0.0.1/"}))
            .await
            .unwrap_err(),
        "TOOL_DISABLED_OR_UNKNOWN"
    );
    profile.enable();
    for (tool, required, input) in [
        (
            "browser.fetch",
            "url",
            json!({"url":"https://example.org/"}),
        ),
        (
            "browser.research",
            "urls",
            json!({"urls":["https://example.org/"]}),
        ),
        (
            "browser.search",
            "provider,query",
            json!({"provider":"duckduckgo","query":"owned"}),
        ),
    ] {
        assert_eq!(policy::classify(tool, &input), policy::Risk::Free);
        let validator = jsonschema::validator_for(&catalog::tool_schema(tool, required)).unwrap();
        assert!(validator.is_valid(&input));
        let mut forbidden = input.clone();
        forbidden["allow_private"] = json!(true);
        assert!(!validator.is_valid(&forbidden));
        forbidden = input.clone();
        forbidden["headers"] = json!({"cookie":"owned"});
        assert!(!validator.is_valid(&forbidden));
        profile
            .core
            .store
            .set_setting("privacy", &json!(true))
            .unwrap();
        assert_eq!(
            profile.core.execute(tool, input).await.unwrap_err(),
            "PRIVACY_DENIED"
        );
        profile
            .core
            .store
            .set_setting("privacy", &json!(false))
            .unwrap();
    }
    let duplicate = json!({"urls":["https://example.org/","https://example.org/"]});
    assert_eq!(
        profile
            .core
            .execute("browser.research", duplicate)
            .await
            .unwrap_err(),
        "INVALID_TOOL_INPUT"
    );
    assert_eq!(
        profile
            .core
            .execute(
                "browser.search",
                json!({"provider":"duckduckgo","query":"password OWNED_SENTINEL"})
            )
            .await
            .unwrap_err(),
        "WEB_QUERY_DENIED"
    );
    for (raw, error) in [
        ("https://u:OWNED_SENTINEL@example.org/", "WEB_URL_DENIED"),
        (
            "https://example.org/?%74oken=OWNED_SENTINEL",
            "WEB_URL_SECRET_DENIED",
        ),
        ("http://127.0.0.1/", "WEB_DESTINATION_DENIED"),
    ] {
        assert_eq!(
            profile
                .core
                .execute("browser.fetch", json!({"url":raw}))
                .await
                .unwrap_err(),
            error
        );
    }
    let events=profile.core.store.query("SELECT input_summary,output_summary,status FROM activity_events WHERE tool_id LIKE 'browser.%'",&[]).unwrap();
    assert_eq!(events.as_array().unwrap().len(), 4);
    assert!(!events.to_string().contains("OWNED_SENTINEL"));
    assert!(events
        .as_array()
        .unwrap()
        .iter()
        .all(|event| event["status"] == "failed"));
    assert!(profile.core.browser.lock().await.is_none());
    assert!(profile.core.pending.lock().unwrap().is_empty());
    profile.finish();
}

#[tokio::test]
async fn privacy_module_revocation_and_dropped_future_close_owned_http_reads() {
    for mode in [
        "privacy",
        "module",
        "privacy_before_headers",
        "module_before_headers",
        "drop",
        "timeout",
    ] {
        let profile = Profile::new();
        profile.enable();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_request(&mut stream).await;
            if !mode.ends_with("before_headers") {
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 100\r\nConnection: close\r\n\r\nPartial").await.unwrap();
            }
            ready_tx.send(()).unwrap();
            let mut byte = [0; 1];
            match tokio::time::timeout(
                Duration::from_secs(FETCH_SECONDS + 3),
                stream.read(&mut byte),
            )
            .await
            .unwrap()
            {
                Ok(n) => assert_eq!(n, 0),
                Err(error) => assert!([
                    std::io::ErrorKind::ConnectionReset,
                    std::io::ErrorKind::ConnectionAborted
                ]
                .contains(&error.kind())),
            }
        });
        let core = profile.core.clone();
        let operation = tokio::spawn(async move {
            fetch(
                &Control(&core),
                &Resolver::OwnedFixture { port },
                &format!("http://owned.test:{port}/held"),
                100,
            )
            .await
            .map(|f| f.data)
        });
        ready_rx.await.unwrap();
        match mode {
            "privacy" | "privacy_before_headers" => {
                profile
                    .core
                    .store
                    .set_setting("privacy", &json!(true))
                    .unwrap();
                assert_eq!(operation.await.unwrap().unwrap_err(), "PRIVACY_DENIED");
            }
            "module" | "module_before_headers" => {
                profile
                    .core
                    .store
                    .execute(
                        "UPDATE module_configs SET enabled=0 WHERE module_id='browser'",
                        &[],
                    )
                    .unwrap();
                assert_eq!(
                    operation.await.unwrap().unwrap_err(),
                    "TOOL_DISABLED_OR_UNKNOWN"
                );
            }
            "timeout" => assert_eq!(operation.await.unwrap().unwrap_err(), "WEB_TIMEOUT"),
            _ => {
                operation.abort();
                assert!(operation.await.unwrap_err().is_cancelled());
            }
        }
        peer.await.unwrap();
        profile.finish();
    }
}

#[tokio::test]
#[ignore = "Opt-in public internet probe; provider availability is reported separately from deterministic contracts"]
async fn public_web_fetch_search_and_research_probe() {
    assert_eq!(
        std::env::var("ALFREDPET_PUBLIC_RESEARCH").as_deref(),
        Ok("1")
    );
    let profile = Profile::new();
    profile.enable();
    let mut checks = Vec::new();
    for (tool, input) in [
        (
            "browser.fetch",
            json!({"url":"https://example.com/","max_chars":2000}),
        ),
        (
            "browser.search",
            json!({"provider":"duckduckgo","query":"IANA example domains","limit":3}),
        ),
        (
            "browser.research",
            json!({"urls":["https://example.com/","https://example.org/"],"max_chars":2000}),
        ),
    ] {
        match profile.core.execute(tool, input).await {
            Ok(data) => {
                assert_eq!(data["untrusted_content"], true);
                let available = if tool == "browser.research" {
                    data["sources"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|s| s["status"] == "succeeded")
                } else {
                    true
                };
                if tool == "browser.fetch" {
                    assert!(data["sha256"].as_str().unwrap().len() == 64);
                }
                checks.push(json!({"tool":tool,"status":if available{"AVAILABLE"}else{"PARTIAL"},"results":if tool=="browser.search" {data["results"].as_array().unwrap().len()}else{0},"source_statuses":if tool=="browser.research"{data["sources"].as_array().unwrap().iter().map(|s|json!({"status":s["status"],"error":s["error"]})).collect::<Vec<_>>()}else{vec![]}}));
            }
            Err(error) => checks.push(json!({"tool":tool,"status":"UNAVAILABLE","error":error})),
        }
    }
    assert!(profile.core.browser.lock().await.is_none());
    println!(
        "BROWSER_RESEARCH_PUBLIC_PROBE: {}",
        json!({"checks":checks,"scope":"Actual stateless Rust Core public internet reads in this host; point-in-time availability, no product Claude turn or human acceptance"})
    );
    profile.finish();
}
