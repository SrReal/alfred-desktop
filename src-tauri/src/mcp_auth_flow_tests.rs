use super::super::tests::Fixture;
use super::*;
use axum::{
    body::Bytes,
    extract::State as HttpState,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Router,
};
use std::sync::{atomic::AtomicUsize, Mutex};
use tokio::net::TcpStream;

fn request(query: &str) -> Vec<u8> {
    format!("GET {CALLBACK}?{query} HTTP/1.1\r\nHost: 127.0.0.1:54321\r\n\r\n").into_bytes()
}
#[test]
fn callback_issuer_matrix_and_error_order_use_exact_decoded_strings() {
    let issuer = "https://issuer.invalid/tenant";
    let good = "state=nonce&code=owned-code&iss=https%3A%2F%2Fissuer.invalid%2Ftenant";
    for required in [false, true] {
        assert_eq!(
            parse_callback(&request(good), 54321, "nonce", issuer, required)
                .unwrap()
                .text(),
            "owned-code"
        );
        let bad = good.replace("issuer.invalid", "ISSUER.invalid");
        assert_eq!(
            parse_callback(&request(&bad), 54321, "nonce", issuer, required)
                .err()
                .unwrap(),
            "MCP_OAUTH_CALLBACK_ISSUER"
        );
        let bad_error = "state=nonce&error=access_denied&error_description=secret&error_uri=https%3A%2F%2Fattacker.invalid&iss=https%3A%2F%2Fevil.invalid";
        assert_eq!(
            parse_callback(&request(bad_error), 54321, "nonce", issuer, required)
                .err()
                .unwrap(),
            "MCP_OAUTH_CALLBACK_ISSUER"
        );
    }
    assert!(parse_callback(
        &request("state=nonce&code=owned-code"),
        54321,
        "nonce",
        issuer,
        false
    )
    .is_ok());
    assert_eq!(
        parse_callback(
            &request("state=nonce&code=owned-code"),
            54321,
            "nonce",
            issuer,
            true
        )
        .err()
        .unwrap(),
        "MCP_OAUTH_CALLBACK_ISSUER_REQUIRED"
    );
    assert_eq!(
        parse_callback(
            &request("state=nonce&error=access_denied"),
            54321,
            "nonce",
            issuer,
            true
        )
        .err()
        .unwrap(),
        "MCP_OAUTH_CALLBACK_ISSUER_REQUIRED"
    );
    assert_eq!(
        parse_callback(
            &request("state=nonce&error=access_denied"),
            54321,
            "nonce",
            issuer,
            false
        )
        .err()
        .unwrap(),
        "MCP_OAUTH_CONSENT_REJECTED"
    );
    assert_eq!(
        parse_callback(
            &request("state=nonce&code=owned+code"),
            54321,
            "nonce",
            issuer,
            false
        )
        .unwrap()
        .text(),
        "owned code"
    );
    assert_eq!(
        parse_callback(
            &request(&good.replace("code=owned-code", "code=owned-code&error=access_denied")),
            54321,
            "nonce",
            issuer,
            true
        )
        .err()
        .unwrap(),
        "MCP_OAUTH_CALLBACK_CONTRADICTORY"
    );
}
#[test]
fn callback_rejects_duplicate_decoded_keys_invalid_encoding_routing_host_and_body() {
    let issuer = "https://issuer.invalid";
    for query in [
        "state=nonce&code=a&%63ode=b",
        "state=nonce&state=nonce&code=a",
        "state=nonce&code=%G0",
        "state=nonce&code=%",
        "state=nonce&code=%ff",
        "state=nonce&code=%00",
        "state=other&code=a",
        "state=nonce&code=",
        "state=nonce&code=a&&ignored=b",
        "state=nonce&code=a&iss=https%3A%2F%2Fissuer.invalid%2F",
    ] {
        assert!(
            parse_callback(&request(query), 54321, "nonce", issuer, false).is_err(),
            "query case unexpectedly accepted"
        );
    }
    let text = String::from_utf8(request("state=nonce&code=a")).unwrap();
    for bad in [
        text.replace("GET ", "POST "),
        text.replace(CALLBACK, "/oauth/mcp/../callback"),
        text.replace(CALLBACK, "/oauth/mcp/%63allback"),
        text.replace("HTTP/1.1", "HTTP/1.0"),
        text.replace("127.0.0.1:54321", "evil.invalid:54321"),
        text.replace("\r\n\r\n", "\r\nHost: 127.0.0.1:54321\r\n\r\n"),
        text.replace("\r\n\r\n", "\r\nContent-Length: 1\r\n\r\n"),
        text.replace("\r\n\r\n", "\r\nTransfer-Encoding: chunked\r\n\r\n"),
        format!("{text}body"),
        "x".repeat(CALLBACK_LIMIT + 1),
    ] {
        assert!(parse_callback(bad.as_bytes(), 54321, "nonce", issuer, false).is_err());
    }
}
#[test]
fn pkce_uses_os_random_256_bits_and_s256_known_vector() {
    let verifier = Text::new("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into(), 128).unwrap();
    assert_eq!(
        challenge(&verifier),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    let one = random_text().unwrap();
    let two = random_text().unwrap();
    assert_eq!(one.text().len(), 43);
    assert_eq!(
        general_purpose::URL_SAFE_NO_PAD
            .decode(one.text())
            .unwrap()
            .len(),
        32
    );
    assert_ne!(one.text(), two.text());
}

#[derive(Clone)]
struct Capture {
    calls: Arc<AtomicUsize>,
    requests: CapturedRequests,
    status: StatusCode,
    content: String,
    body: String,
    delay: Duration,
}
type CapturedRequests = Arc<Mutex<Vec<(HeaderMap, Vec<u8>)>>>;
struct Server {
    base: String,
    capture: Capture,
    task: tokio::task::JoinHandle<()>,
}
impl Server {
    async fn new(status: u16, content: &str, body: &str) -> Self {
        Self::delayed(status, content, body, Duration::ZERO).await
    }
    async fn delayed(status: u16, content: &str, body: &str, delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let capture = Capture {
            calls: Arc::new(AtomicUsize::new(0)),
            requests: Arc::new(Mutex::new(vec![])),
            status: StatusCode::from_u16(status).unwrap(),
            content: content.into(),
            body: body.into(),
            delay,
        };
        let app = Router::new()
            .route(
                "/token",
                post(
                    |HttpState(c): HttpState<Capture>, headers: HeaderMap, body: Bytes| async move {
                        let index = c.calls.fetch_add(1, Ordering::SeqCst);
                        c.requests.lock().unwrap().push((headers, body.to_vec()));
                        tokio::time::sleep(c.delay).await;
                        let body = if index == 0 {
                            c.body
                        } else {
                            c.body
                                .replace("owned-http-refresh", "owned-http-refresh-rotated")
                                .replace("owned-http-access", "owned-http-access-renewed")
                        };
                        let mut response =
                            (c.status, [(header::CONTENT_TYPE, c.content)], body).into_response();
                        if c.status == StatusCode::FOUND {
                            response.headers_mut().insert(
                                header::LOCATION,
                                header::HeaderValue::from_static("/token"),
                            );
                        }
                        response
                    },
                ),
            )
            .with_state(capture.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            base,
            capture,
            task,
        }
    }
    fn actor(&self, f: &Fixture, auth: ClientAuth) -> Actor {
        Actor::build(
            &f.core,
            &f.id,
            Authorization {
                issuer: self.base.clone(),
                resource: "https://mcp.invalid/mcp".into(),
                token_endpoint: format!("{}/token", self.base),
                client_id: "owned:client+id".into(),
                client_auth: auth,
                requested_scopes: Some(BTreeSet::from(["files:read".into()])),
            },
            true,
        )
        .unwrap()
    }
    async fn finish(self) {
        self.task.abort();
        assert!(self.task.await.unwrap_err().is_cancelled());
    }
}
fn token_body(refresh: bool) -> String {
    let mut value = json!({"token_type":"Bearer","access_token":"owned-http-access","expires_in":3600,"scope":"files:read"});
    if refresh {
        value["refresh_token"] = json!("owned-http-refresh");
    }
    value.to_string()
}
async fn callback(consent: &Consent, iss: Option<&str>) -> String {
    let redirect = Url::parse(&consent.redirect).unwrap();
    let mut url = redirect.clone();
    url.query_pairs_mut()
        .append_pair("state", consent.nonce.text())
        .append_pair("code", "owned-http-code");
    if let Some(iss) = iss {
        url.query_pairs_mut().append_pair("iss", iss);
    }
    let mut socket = TcpStream::connect(("127.0.0.1", redirect.port().unwrap()))
        .await
        .unwrap();
    let bytes = format!(
        "GET {}?{} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
        url.path(),
        url.query().unwrap(),
        redirect.port().unwrap()
    );
    socket.write_all(bytes.as_bytes()).await.unwrap();
    // Enqueue request now. The caller owns complete() and reads this socket in
    // parallel so no artificial HTTP/browser deadlock is introduced.
    drop(socket);
    bytes
}
#[tokio::test]
async fn real_owned_loopback_callback_code_post_publishes_ready_and_rotates_explicit_refresh() {
    let f = Fixture::new().await;
    let server = Server::new(200, "application/json", &token_body(true)).await;
    let a = server.actor(&f, ClientAuth::None);
    let consent = Consent::bind(a.clone(), &format!("{}/authorize", server.base), true, None)
        .await
        .unwrap();
    let url = Url::parse(consent.browser_url()).unwrap();
    let pairs = url.query_pairs().collect::<BTreeMap<_, _>>();
    assert_eq!(pairs["resource"], "https://mcp.invalid/mcp");
    assert_eq!(pairs["code_challenge_method"], "S256");
    assert_eq!(pairs["code_challenge"], challenge(&consent.verifier));
    assert_eq!(pairs["scope"], "files:read");
    assert!(consent.public_state().phase == Phase::Authorizing);
    callback(&consent, Some(&server.base)).await;
    let ready = consent.complete().await.unwrap();
    assert!(ready.phase == Phase::Ready);
    let (_, env) = a.read().unwrap();
    assert_eq!(
        env.access_token.as_ref().unwrap().text(),
        "owned-http-access"
    );
    drop(env);
    let grant = a.grant().unwrap();
    let renewed = refresh(a.clone()).await.unwrap();
    assert!(renewed.phase == Phase::Ready);
    let (_, env) = a.read().unwrap();
    assert_eq!(
        env.refresh_token.as_ref().unwrap().text(),
        "owned-http-refresh-rotated"
    );
    assert_eq!(
        env.access_token.as_ref().unwrap().text(),
        "owned-http-access-renewed"
    );
    drop(env);
    assert!(grant.check().is_err());
    drop(grant);
    assert_eq!(server.capture.calls.load(Ordering::SeqCst), 2);
    {
        let records = server.capture.requests.lock().unwrap();
        for (index, (headers, bytes)) in records.iter().enumerate() {
            assert!(!headers.contains_key(header::AUTHORIZATION));
            let form = Url::parse(&format!(
                "http://localhost/?{}",
                std::str::from_utf8(bytes).unwrap()
            ))
            .unwrap();
            let fields = form.query_pairs().collect::<BTreeMap<_, _>>();
            assert_eq!(fields["resource"], "https://mcp.invalid/mcp");
            assert_eq!(fields["client_id"], "owned:client+id");
            if index == 0 {
                assert_eq!(fields["grant_type"], "authorization_code");
                assert_eq!(fields["code"], "owned-http-code");
                assert!(fields.contains_key("code_verifier"));
            } else {
                assert_eq!(fields["grant_type"], "refresh_token");
                assert_eq!(fields["refresh_token"], "owned-http-refresh");
                assert!(!fields.contains_key("code_verifier"));
            }
        }
    }
    let settings = f
        .core
        .store
        .query("SELECT json_value FROM settings", &[])
        .unwrap()
        .to_string();
    for secret in ["owned-http-access", "owned-http-refresh", "owned-http-code"] {
        assert!(!settings.contains(secret));
    }
    drop(a);
    server.finish().await;
    f.finish();
}
#[tokio::test]
async fn wrong_or_missing_callback_issuer_never_reaches_token_endpoint() {
    for wrong in [Some("http://127.0.0.1:1"), None] {
        let f = Fixture::new().await;
        let server = Server::new(200, "application/json", &token_body(false)).await;
        let a = server.actor(&f, ClientAuth::None);
        let consent = Consent::bind(a.clone(), &format!("{}/authorize", server.base), true, None)
            .await
            .unwrap();
        callback(&consent, wrong).await;
        let result = consent.complete().await;
        assert!(result.is_err());
        assert_eq!(server.capture.calls.load(Ordering::SeqCst), 0);
        assert!(a.state_held().unwrap().phase == Phase::Cancelled);
        drop(a);
        server.finish().await;
        f.finish();
    }
}
#[tokio::test]
async fn ownership_serializes_actors_across_core_instances_without_quarantining_ready_on_busy() {
    let f = Fixture::new().await;
    let server = Server::new(200, "application/json", &token_body(true)).await;
    let a = server.actor(&f, ClientAuth::None);
    let consent = Consent::bind(a.clone(), &format!("{}/authorize", server.base), true, None)
        .await
        .unwrap();
    let state = json!(consent.public_state());
    let other = Arc::new(Core::new(f.core.root.clone(), f.core.resource.clone()).unwrap());
    let other_actor = Actor::build(&other, &f.id, a.authorization.clone(), true).unwrap();
    assert_eq!(
        Consent::bind(
            other_actor,
            &format!("{}/authorize", server.base),
            true,
            None
        )
        .await
        .err()
        .unwrap(),
        "MCP_OAUTH_BUSY"
    );
    assert_eq!(json!(a.state_held().unwrap()), state);
    assert_eq!(refresh(a.clone()).await.err().unwrap(), "MCP_OAUTH_BUSY");
    a.abandon(&consent.public_state().session_id).unwrap();
    assert!(consent.complete().await.is_err());
    assert_eq!(server.capture.calls.load(Ordering::SeqCst), 0);
    drop(other);
    drop(a);
    server.finish().await;
    f.finish();
}
#[tokio::test]
async fn token_auth_methods_are_exclusive_and_basic_form_encodes_before_base64() {
    let f = Fixture::new().await;
    let server = Server::new(200, "application/json", &token_body(false)).await;
    for auth in [
        ClientAuth::None,
        ClientAuth::ClientSecretBasic,
        ClientAuth::ClientSecretPost,
    ] {
        let a = server.actor(&f, auth.clone());
        let secret = if auth == ClientAuth::None {
            None
        } else {
            Some(Text::new("owned:secret+%".into(), 16384).unwrap())
        };
        let request = token_builder(
            &a,
            &[
                ("grant_type", "authorization_code"),
                ("code", "owned-code"),
                ("redirect_uri", "http://127.0.0.1:54321/oauth/mcp/callback"),
                ("code_verifier", "owned-verifier"),
            ],
            secret.as_ref(),
        )
        .unwrap()
        .build()
        .unwrap();
        assert!(request.url().query().is_none());
        let body = request.body().unwrap().as_bytes().unwrap();
        let form = Url::parse(&format!(
            "http://localhost/?{}",
            std::str::from_utf8(body).unwrap()
        ))
        .unwrap();
        let fields = form.query_pairs().collect::<BTreeMap<_, _>>();
        assert_eq!(fields["resource"], "https://mcp.invalid/mcp");
        match auth {
            ClientAuth::None => {
                assert!(!request.headers().contains_key(header::AUTHORIZATION));
                assert!(!fields.contains_key("client_secret"));
            }
            ClientAuth::ClientSecretPost => {
                assert!(!request.headers().contains_key(header::AUTHORIZATION));
                assert_eq!(fields["client_secret"], "owned:secret+%");
            }
            ClientAuth::ClientSecretBasic => {
                assert!(!fields.contains_key("client_id") && !fields.contains_key("client_secret"));
                let header = &request.headers()[header::AUTHORIZATION];
                assert!(header.is_sensitive());
                let decoded = general_purpose::STANDARD
                    .decode(header.to_str().unwrap().strip_prefix("Basic ").unwrap())
                    .unwrap();
                assert_eq!(
                    std::str::from_utf8(&decoded).unwrap(),
                    "owned%3Aclient%2Bid:owned%3Asecret%2B%25"
                );
                assert!(!format!("{header:?}").contains("Basic"));
            }
        }
        drop(a);
    }
    assert_eq!(server.capture.calls.load(Ordering::SeqCst), 0);
    server.finish().await;
    f.finish();
}

#[tokio::test]
async fn completed_or_stale_exchange_phase_cannot_dispatch_a_second_token_post() {
    let f = Fixture::new().await;
    let server = Server::new(200, "application/json", &token_body(true)).await;
    let a = server.actor(&f, ClientAuth::None);
    let consent = Consent::bind(a.clone(), &format!("{}/authorize", server.base), true, None)
        .await
        .unwrap();
    let mut stale = consent.public_state().clone();
    stale.phase = Phase::ExchangeInFlight;
    callback(&consent, Some(&server.base)).await;
    consent.complete().await.unwrap();
    let fields = [
        ("grant_type", "authorization_code"),
        ("code", "owned-code"),
        ("redirect_uri", "http://127.0.0.1:54321/oauth/mcp/callback"),
        ("code_verifier", "owned-verifier"),
    ];
    assert_eq!(
        token_request(&a, &stale, &fields, None)
            .await
            .err()
            .unwrap(),
        "MCP_OAUTH_PHASE"
    );
    assert_eq!(server.capture.calls.load(Ordering::SeqCst), 1);
    let before = json!(a.state_held().unwrap());
    for fields in [
        vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", "owned-refresh"),
            ("resource", "https://evil.invalid"),
        ],
        vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", "owned-refresh"),
            ("refresh_token", "second"),
        ],
        vec![("grant_type", "authorization_code"), ("code", "owned-code")],
        vec![("grant_type", "refresh_token"), ("refresh_token", "")],
    ] {
        assert!(token_builder(&a, &fields, None).is_err());
    }
    assert_eq!(json!(a.state_held().unwrap()), before);
    drop(a);
    server.finish().await;
    f.finish();
}

#[tokio::test]
async fn failed_refresh_remains_uncertain_and_a_second_refresh_never_dispatches() {
    let f = Fixture::new().await;
    let server = Server::new(429, "application/json", "{}").await;
    let a = server.actor(&f, ClientAuth::None);
    let state = a.begin(false).unwrap();
    let state = a
        .transition(
            &state.session_id,
            Phase::Authorizing,
            Phase::ExchangeInFlight,
        )
        .unwrap();
    let envelope = a
        .tokens(
            &state,
            serde_json::from_str(&token_body(true)).unwrap(),
            None,
            None,
        )
        .unwrap();
    a.transition(
        &state.session_id,
        Phase::ExchangeInFlight,
        Phase::PublicationPending,
    )
    .unwrap();
    a.publish(&state.session_id, envelope).unwrap();
    let grant = a.grant().unwrap();
    assert_eq!(
        refresh(a.clone()).await.err().unwrap(),
        "MCP_OAUTH_TOKEN_RATE_LIMITED"
    );
    assert!(a.state_held().unwrap().phase == Phase::Uncertain);
    assert!(grant.check().is_err());
    drop(grant);
    assert_eq!(
        refresh(a.clone()).await.err().unwrap(),
        "MCP_OAUTH_REAUTHORIZE_REQUIRED"
    );
    assert_eq!(server.capture.calls.load(Ordering::SeqCst), 1);
    drop(a);
    server.finish().await;
    f.finish();
}
#[tokio::test]
async fn rejected_malformed_rate_limited_and_redirect_token_responses_are_never_replayed() {
    for (status, content, body) in [
        (
            400,
            "application/json",
            "{\"error\":\"invalid_grant\",\"error_description\":\"owned-private-error\"}",
        ),
        (401, "application/json", "{}"),
        (429, "application/json", "{}"),
        (500, "application/json", "{}"),
        (302, "application/json", "{}"),
        (200, "text/plain", "{}"),
        (
            200,
            "application/json",
            "{\"access_token\":\"a\",\"access_token\":\"b\"}",
        ),
        (200, "application/json", "{"),
        (200, "application/json; charset=latin1", "{}"),
        (
            200,
            "application/json",
            "{\"token_type\":\"DPoP\",\"access_token\":\"owned-http-access\"}",
        ),
    ] {
        let f = Fixture::new().await;
        let server = Server::new(status, content, body).await;
        let a = server.actor(&f, ClientAuth::None);
        let consent = Consent::bind(a.clone(), &format!("{}/authorize", server.base), true, None)
            .await
            .unwrap();
        callback(&consent, Some(&server.base)).await;
        let error = consent.complete().await.err().unwrap();
        assert!(!error.contains("owned-private-error"));
        assert_eq!(server.capture.calls.load(Ordering::SeqCst), 1);
        assert!(a.state_held().unwrap().phase == Phase::Uncertain && a.read().is_err());
        drop(a);
        server.finish().await;
        f.finish();
    }
}
#[tokio::test]
async fn missing_refresh_token_keeps_ready_without_remote_request() {
    let f = Fixture::new().await;
    let server = Server::new(200, "application/json", &token_body(false)).await;
    let a = server.actor(&f, ClientAuth::None);
    let consent = Consent::bind(a.clone(), &format!("{}/authorize", server.base), true, None)
        .await
        .unwrap();
    callback(&consent, Some(&server.base)).await;
    consent.complete().await.unwrap();
    let before = json!(a.state_held().unwrap());
    assert_eq!(
        refresh(a.clone()).await.err().unwrap(),
        "MCP_OAUTH_REAUTHORIZE_REQUIRED"
    );
    assert_eq!(json!(a.state_held().unwrap()), before);
    assert_eq!(server.capture.calls.load(Ordering::SeqCst), 1);
    drop(a);
    server.finish().await;
    f.finish();
}

#[tokio::test]
async fn reviewed_endpoint_queries_are_preserved_without_duplicate_protocol_parameters() {
    let f = Fixture::new().await;
    let server = Server::new(200, "application/json", &token_body(true)).await;
    let original = server.actor(&f, ClientAuth::None);
    let mut auth = original.authorization.clone();
    drop(original);
    auth.token_endpoint = format!("{}/token?tenant=owned", server.base);
    let a = Actor::build(&f.core, &f.id, auth, true).unwrap();
    let consent = Consent::bind(
        a.clone(),
        &format!("{}/authorize?ui_locales=es", server.base),
        true,
        None,
    )
    .await
    .unwrap();
    let url = Url::parse(consent.browser_url()).unwrap();
    let fields = url.query_pairs().collect::<BTreeMap<_, _>>();
    assert_eq!(fields["ui_locales"], "es");
    assert_eq!(fields["resource"], "https://mcp.invalid/mcp");
    callback(&consent, Some(&server.base)).await;
    consent.complete().await.unwrap();
    let fields = [
        ("grant_type", "refresh_token"),
        ("refresh_token", "owned-refresh"),
    ];
    let request = token_builder(&a, &fields, None).unwrap().build().unwrap();
    assert_eq!(request.url().query(), Some("tenant=owned"));
    assert!(!request.url().as_str().contains("owned-refresh"));
    let before = json!(a.state_held().unwrap());
    for query in [
        "state=preset",
        "client_id=wrong",
        "%72esource=https%3A%2F%2Fevil.invalid",
        "scope=other",
    ] {
        assert_eq!(
            Consent::bind(
                a.clone(),
                &format!("{}/authorize?{query}", server.base),
                true,
                None
            )
            .await
            .err()
            .unwrap(),
            "MCP_OAUTH_CONSENT_RESERVED_FIELDS"
        );
    }
    assert_eq!(json!(a.state_held().unwrap()), before);
    assert_eq!(server.capture.calls.load(Ordering::SeqCst), 1);
    drop(a);
    server.finish().await;
    f.finish();
}

#[tokio::test]
async fn cancellation_after_token_dispatch_quarantines_outcome_and_never_replays() {
    let f = Fixture::new().await;
    let server = Server::delayed(
        200,
        "application/json",
        &token_body(true),
        Duration::from_secs(10),
    )
    .await;
    let a = server.actor(&f, ClientAuth::None);
    let consent = Consent::bind(a.clone(), &format!("{}/authorize", server.base), true, None)
        .await
        .unwrap();
    let session = consent.public_state().session_id.clone();
    callback(&consent, Some(&server.base)).await;
    let task = tokio::spawn(consent.complete());
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.capture.calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(a.abandon(&session).unwrap().phase == Phase::Uncertain);
    assert!(tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    assert_eq!(server.capture.calls.load(Ordering::SeqCst), 1);
    assert!(a.read().is_err());
    drop(a);
    server.finish().await;
    f.finish();
}

#[tokio::test]
async fn token_http_timeout_is_uncertain_and_not_automatically_replayed() {
    let f = Fixture::new().await;
    let server = Server::delayed(
        200,
        "application/json",
        &token_body(true),
        Duration::from_secs(30),
    )
    .await;
    let a = server.actor(&f, ClientAuth::None);
    let consent = Consent::bind(a.clone(), &format!("{}/authorize", server.base), true, None)
        .await
        .unwrap();
    callback(&consent, Some(&server.base)).await;
    assert_eq!(
        consent.complete().await.err().unwrap(),
        "MCP_OAUTH_TOKEN_OUTCOME_UNCERTAIN"
    );
    assert!(a.state_held().unwrap().phase == Phase::Uncertain);
    assert_eq!(server.capture.calls.load(Ordering::SeqCst), 1);
    assert!(a.read().is_err());
    drop(a);
    server.finish().await;
    f.finish();
}

#[tokio::test]
async fn atomic_refresh_returns_current_native_generation_and_quarantines_it() {
    let f = Fixture::new().await;
    let server = Server::new(200, "application/json", &token_body(true)).await;
    let a = server.actor(&f, ClientAuth::None);
    let consent = Consent::bind(a.clone(), &format!("{}/authorize", server.base), true, None)
        .await
        .unwrap();
    callback(&consent, Some(&server.base)).await;
    consent.complete().await.unwrap();
    let old_version = a.read().unwrap().0;
    refresh(a.clone()).await.unwrap();
    let (current_version, old_envelope) = a.read().unwrap();
    assert_ne!(old_version, current_version);
    drop(old_envelope);
    let (state, envelope) = a.begin_refresh().unwrap();
    assert_eq!(
        state.prior_version.as_deref(),
        Some(current_version.to_string().as_str())
    );
    assert_eq!(
        envelope.refresh_token.as_ref().unwrap().text(),
        "owned-http-refresh-rotated"
    );
    assert!(a.read().is_err());
    a.abandon(&state.session_id).unwrap();
    drop(envelope);
    drop(a);
    server.finish().await;
    f.finish();
}
