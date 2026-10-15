use super::*;
use axum::{
    body::Body,
    extract::{Request, State},
    http::Response,
    Router,
};
use std::{os::windows::fs::MetadataExt, path::PathBuf, sync::Mutex};
const CLIENT: &str = "owned-fixture.apps.googleusercontent.com";
fn config() -> Value {
    json!({"auth":"installed_app_pkce","client_id":CLIENT,"client_secret_mode":"none","allowed_files":["owned_file"],"allowed_folders":["owned_folder"],"read_only":true})
}
struct Mock {
    seen: Vec<Value>,
    body: Value,
    status: u16,
    delay: bool,
    redirect: bool,
    started: Arc<tokio::sync::Notify>,
}
fn tokens() -> Value {
    json!({"access_token":"owned_fictional_access","refresh_token":"owned_fictional_refresh","expires_in":3600,"scope":DRIVE_SCOPE,"token_type":"Bearer","unknown_future_field":{"private":"owned_private_extension"}})
}
async fn handler(State(m): State<Arc<Mutex<Mock>>>, request: Request) -> Response<Body> {
    let method = request.method().to_string();
    let path = request.uri().path().to_string();
    let bytes = axum::body::to_bytes(request.into_body(), 16384)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let url = reqwest::Url::parse(&format!("http://127.0.0.1/?{text}")).unwrap();
    let fields: Value = url
        .query_pairs()
        .map(|(k, v)| (k.to_string(), json!(v.as_ref())))
        .collect::<serde_json::Map<_, _>>()
        .into();
    let (delay, notify) = {
        let mut s = m.lock().unwrap();
        s.seen
            .push(json!({"method":method,"path":path,"fields":fields}));
        (s.delay, s.started.clone())
    };
    notify.notify_one();
    if delay {
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let s = m.lock().unwrap();
    if s.redirect {
        return Response::builder()
            .status(302)
            .header("location", "http://127.0.0.1:1/foreign")
            .body(Body::from("{}"))
            .unwrap();
    }
    Response::builder()
        .status(s.status)
        .header("content-type", "application/json")
        .body(Body::from(s.body.to_string()))
        .unwrap()
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
    async fn new() -> Self {
        let m = Arc::new(Mutex::new(Mock {
            seen: vec![],
            body: tokens(),
            status: 200,
            delay: false,
            redirect: false,
            started: Arc::new(tokio::sync::Notify::new()),
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = Router::new().fallback(handler).with_state(m.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let root = std::env::temp_dir().join(format!("ap-drive-auth-{}", storage::id()));
        std::fs::create_dir(&root).unwrap();
        let identity = asset_files::identity(&asset_files::directory(&root).unwrap()).unwrap();
        let core = Arc::new(
            Core::new(
                root.join("profile"),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        core.store
            .execute(
                "UPDATE module_configs SET enabled=1 WHERE module_id='drive'",
                &[],
            )
            .unwrap();
        *core.drive_fixture.lock().unwrap() = Some(TestTransport {
            token_url: format!("http://{addr}/token"),
            api_base: format!("http://{addr}/"),
            vault: Arc::new(Mutex::new(vault::TestVault::default())),
            timeout_ms: 5000,
            native_storage: false,
            headless_permission: false,
        });
        let result = integrations::save(
            &core,
            &json!({"module_id":"drive","name":"Owned Drive","config":config(),"secret":""}),
        )
        .await
        .unwrap();
        let id = result["id"].as_str().unwrap().to_string();
        Self {
            core,
            root,
            identity,
            id,
            m,
            server,
        }
    }
    fn input(&self) -> Value {
        json!({"connection":self.id})
    }
    fn install(&self, expired: bool) {
        let scope = Scope::new(&self.core, &self.id).unwrap();
        let mut env = Envelope::empty(&scope.config, None);
        env.access_token = Some(Text::from_str("owned_fictional_old_access", 2048).unwrap());
        env.refresh_token = Some(Text::from_str("owned_fictional_old_refresh", 512).unwrap());
        env.granted_scopes = vec![DRIVE_SCOPE.into()];
        let now = chrono::Utc::now().timestamp();
        env.issued_at = Some(now - 4000);
        env.expires_at = Some(if expired { now - 1 } else { now + 1000 });
        env.validate(&scope.config).unwrap();
        let bytes = env.bytes().unwrap();
        let receipt = scope
            .private(|b| storage_save(&self.core, b, None, bytes.bytes()))
            .unwrap();
        let expected = mark(&scope, "owned_fixture_session", "fixture", None).unwrap();
        finish(&scope, &expected, "ready", None, Some(&receipt)).unwrap();
    }
    async fn wait_phase(&self, phase: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if self.core.store.setting(&state_key(&self.id))["phase"] == phase {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn consent(&self, auth: &Value, fragmented: bool) -> String {
        let url = reqwest::Url::parse(auth["fixture_auth_url"].as_str().unwrap()).unwrap();
        let params: std::collections::HashMap<_, _> = url.query_pairs().collect();
        let mut redirect = reqwest::Url::parse(&params["redirect_uri"]).unwrap();
        redirect
            .query_pairs_mut()
            .append_pair("state", &params["state"])
            .append_pair("code", "owned_fictional_code");
        let port = redirect.port().unwrap();
        let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let request = format!(
            "GET {}?{} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n",
            redirect.path(),
            redirect.query().unwrap()
        );
        if fragmented {
            let split = request.len() / 2;
            socket
                .write_all(&request.as_bytes()[..split])
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(40)).await;
            socket
                .write_all(&request.as_bytes()[split..])
                .await
                .unwrap();
        } else {
            socket.write_all(request.as_bytes()).await.unwrap();
        }
        let mut body = vec![];
        tokio::time::timeout(Duration::from_secs(5), socket.read_to_end(&mut body))
            .await
            .unwrap()
            .unwrap();
        String::from_utf8(body).unwrap()
    }
    async fn clean(self) {
        self.core
            .store
            .set_setting("privacy", &json!(true))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        self.server.abort();
        let _ = self.server.await;
        drop(self.core);
        assert_eq!(
            self.root.canonicalize().unwrap().parent(),
            Some(std::env::temp_dir().canonicalize().unwrap().as_path())
        );
        assert!(self
            .root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("ap-drive-auth-"));
        assert_eq!(
            asset_files::identity(&asset_files::directory(&self.root).unwrap()).unwrap(),
            self.identity
        );
        let mut paths = vec![self.root.clone()];
        while let Some(p) = paths.pop() {
            let m = std::fs::symlink_metadata(&p).unwrap();
            assert_eq!(m.file_attributes() & 0x400, 0);
            if m.is_dir() {
                paths.extend(std::fs::read_dir(p).unwrap().map(|e| e.unwrap().path()));
            }
        }
        std::fs::remove_dir_all(self.root).unwrap();
    }
}

#[test]
fn drive_auth_callback_closed_headers_state_and_private_errors() {
    let request = |route: &str, headers: &str| {
        format!("GET {route} HTTP/1.1\r\nHost: 127.0.0.1:8123\r\n{headers}\r\n")
    };
    let valid = request("/oauth/owned?state=owned_private_nonce&code=owned_code", "");
    assert!(
        parse_callback(
            valid.as_bytes(),
            "/oauth/owned",
            8123,
            "owned_private_nonce"
        )
        .unwrap()
        .text()
            == "owned_code"
    );
    for route in [
        "/oauth/other?state=owned_private_nonce&code=owned_code",
        "/oauth/owned?state=wrong&code=owned_code",
        "/oauth/owned?state=owned_private_nonce&code=a&code=b",
        "/oauth/owned?state=owned_private_nonce&code=owned_code&error=owned_private_description",
        "/oauth/owned?state=owned_private_nonce&code=%00",
        "/oauth/owned?state=owned_private_nonce&code=owned_code&iss=https%3A%2F%2Fforeign.test",
    ] {
        let error = parse_callback(
            request(route, "").as_bytes(),
            "/oauth/owned",
            8123,
            "owned_private_nonce",
        )
        .err()
        .unwrap();
        assert!(error.starts_with("DRIVE_"));
        assert!(!error.contains("owned_private"));
    }
    for headers in [
        "Host: foreign.test\r\n",
        "Content-Length: 1\r\n",
        "Transfer-Encoding: chunked\r\n",
        " bad: value\r\n",
        ": value\r\n",
        "X-Header: \u{7f}\r\n",
    ] {
        assert!(parse_callback(
            request(
                "/oauth/owned?state=owned_private_nonce&code=owned_code",
                headers
            )
            .as_bytes(),
            "/oauth/owned",
            8123,
            "owned_private_nonce"
        )
        .is_err());
    }
}

#[tokio::test]
async fn drive_auth_fragmented_pkce_callback_persists_only_private_envelope() {
    let f = Fixture::new().await;
    let auth = start(&f.core, &f.input()).await.unwrap();
    assert_eq!(auth["system_browser_launched"], false);
    let url = reqwest::Url::parse(auth["fixture_auth_url"].as_str().unwrap()).unwrap();
    let params: std::collections::HashMap<_, _> = url.query_pairs().collect();
    assert_eq!(
        url.origin().ascii_serialization(),
        "https://accounts.google.com"
    );
    assert_eq!(params["code_challenge_method"], "S256");
    assert_eq!(params["scope"], DRIVE_SCOPE);
    assert_ne!(params["state"], auth["session_id"].as_str().unwrap());
    let reply = f.consent(&auth, true).await;
    assert!(reply.contains("200 OK"));
    assert!(reply.contains("no-store"));
    f.wait_phase("ready").await;
    let seen = f.m.lock().unwrap().seen[0]["fields"].clone();
    assert_eq!(seen["grant_type"], "authorization_code");
    assert_eq!(seen["client_id"], CLIENT);
    let verifier = seen["code_verifier"].as_str().unwrap();
    assert!((43..=128).contains(&verifier.len()));
    assert_eq!(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes())),
        params["code_challenge"].as_ref()
    );
    drop(params);
    let grant = grant(&f.core, &f.id).await.unwrap();
    assert_eq!(grant.token.text(), "owned_fictional_access");
    drop(grant);
    for sql in [
        "SELECT config_json,secret_ref FROM connections",
        "SELECT key,json_value FROM settings",
        "SELECT input_summary,output_summary FROM activity_events",
    ] {
        let data = f.core.store.query(sql, &[]).unwrap().to_string();
        for secret in [
            "owned_fictional_access",
            "owned_fictional_refresh",
            "owned_fictional_code",
            "owned_private_extension",
            verifier,
        ] {
            assert!(!data.contains(secret));
        }
    }
    f.clean().await;
}

#[tokio::test]
async fn drive_auth_expired_grant_renews_once_across_two_core_actors_and_retains_refresh() {
    let f = Fixture::new().await;
    f.install(true);
    {
        let mut m = f.m.lock().unwrap();
        m.body.as_object_mut().unwrap().remove("scope");
        m.body.as_object_mut().unwrap().remove("refresh_token");
        m.delay = true;
    }
    let other = Arc::new(
        Core::new(
            f.root.join("profile"),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap(),
    );
    *other.drive_fixture.lock().unwrap() = f.core.drive_fixture.lock().unwrap().clone();
    let (a, b) = tokio::join!(grant(&f.core, &f.id), grant(&other, &f.id));
    assert!(a.is_ok() && b.is_ok());
    assert_eq!(f.m.lock().unwrap().seen.len(), 1);
    let scope = Scope::new(&f.core, &f.id).unwrap();
    let (_, env) = scope.envelope().unwrap();
    assert_eq!(
        env.refresh_token.as_ref().unwrap().text(),
        "owned_fictional_old_refresh"
    );
    drop(a);
    drop(b);
    drop(other);
    drop(scope);
    drop(env);
    f.clean().await;
}

#[tokio::test]
async fn drive_auth_uncertain_refresh_or_private_save_never_replays() {
    for storage_failure in [false, true] {
        let f = Fixture::new().await;
        f.install(true);
        if storage_failure {
            f.core
                .drive_fixture
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .vault
                .lock()
                .unwrap()
                .fail_commit = true;
        } else {
            f.m.lock().unwrap().status = 503;
            f.m.lock().unwrap().body = json!({"error":"owned_provider_private","error_description":"owned_private_description"});
        }
        let error = grant(&f.core, &f.id).await.err().unwrap();
        assert!(!error.contains("owned_private"));
        assert_eq!(
            f.core.store.setting(&state_key(&f.id))["phase"],
            "uncertain"
        );
        assert!(grant(&f.core, &f.id).await.is_err());
        assert_eq!(f.m.lock().unwrap().seen.len(), 1);
        f.clean().await;
    }
}

#[tokio::test]
async fn drive_auth_privacy_and_superseding_attempt_close_listener_without_exchange() {
    let f = Fixture::new().await;
    let old = start(&f.core, &f.input()).await.unwrap();
    let started = tokio::time::Instant::now();
    let new = start(&f.core, &f.input()).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_ne!(old["session_id"], new["session_id"]);
    assert!(cancel(
        &f.core,
        &json!({"connection":f.id,"session_id":old["session_id"]})
    )
    .is_err());
    cancel(
        &f.core,
        &json!({"connection":f.id,"session_id":new["session_id"]}),
    )
    .unwrap();
    f.wait_phase("reauthorize").await;
    assert!(f.m.lock().unwrap().seen.is_empty());
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(
        start(&f.core, &f.input()).await.err().unwrap(),
        "PRIVACY_DENIED"
    );
    f.clean().await;
}

#[tokio::test]
async fn drive_auth_dispatched_refresh_privacy_revocation_is_uncertain() {
    let f = Fixture::new().await;
    f.install(true);
    f.m.lock().unwrap().delay = true;
    let notify = f.m.lock().unwrap().started.clone();
    let c = f.core.clone();
    let id = f.id.clone();
    let job = tokio::spawn(async move { grant(&c, &id).await });
    notify.notified().await;
    let now = tokio::time::Instant::now();
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    let error = job.await.unwrap().err().unwrap();
    assert_eq!(error, "PRIVACY_DENIED");
    assert!(now.elapsed() < Duration::from_secs(1));
    assert_eq!(
        f.core.store.setting(&state_key(&f.id))["phase"],
        "uncertain"
    );
    assert_eq!(f.m.lock().unwrap().seen.len(), 1);
    f.clean().await;
}

#[tokio::test]
async fn drive_auth_profiles_alias_migration_and_client_change_require_explicit_reauth() {
    let f = Fixture::new().await;
    let rows = f
        .core
        .store
        .query("SELECT * FROM connections WHERE id=?", &[json!(f.id)])
        .unwrap();
    let old = &rows[0];
    let generation = f
        .core
        .store
        .setting(&format!("drive.connection_generation.{}", f.id));
    let mut changed = config();
    changed["client_id"] = json!("owned-new-client.apps.googleusercontent.com");
    {
        let _lease = integrations::save_lock(&f.core).unwrap();
        assert_eq!(
            save(
                &f.core,
                &f.id,
                &json!({"name":"Owned rejected","config":changed,"secret":""}),
                Some(old)
            )
            .unwrap_err(),
            "DRIVE_EXPLICIT_REAUTHORIZATION_REQUIRED"
        );
    }
    assert_eq!(
        f.core
            .store
            .setting(&format!("drive.connection_generation.{}", f.id)),
        generation
    );
    let mut legacy = old.clone();
    legacy["secret_ref"] = json!("connection.foreign");
    legacy["config_json"] = json!("{malformed");
    {
        let _lease = integrations::save_lock(&f.core).unwrap();
        assert_eq!(
            save(
                &f.core,
                &f.id,
                &json!({"name":"Owned rejected","config":config(),"secret":""}),
                Some(&legacy)
            )
            .unwrap_err(),
            "DRIVE_EXPLICIT_REAUTHORIZATION_REQUIRED"
        );
    }
    assert_eq!(
        f.core
            .store
            .setting(&format!("drive.connection_generation.{}", f.id)),
        generation
    );
    for key in [
        "token",
        "secret_ref",
        "client_secret",
        "refresh_token",
        "unknown",
    ] {
        let mut bad = config();
        bad[key] = json!("owned_fictional");
        assert!(validate_config(&bad).is_err());
    }
    let mut bad = config();
    bad["base_url"] = json!("http://127.0.0.1:1");
    assert!(validate_config(&bad).is_err());
    assert!(f.m.lock().unwrap().seen.is_empty());
    f.clean().await;
}

#[tokio::test]
async fn drive_auth_token_validation_redirect_scope_expiry_and_private_error_limits() {
    let f = Fixture::new().await;
    for (key, value) in [
        ("token_type", json!("Basic")),
        ("scope", json!("https://www.googleapis.com/auth/drive")),
        ("expires_in", json!(0)),
        ("access_token", json!("a".repeat(2049))),
        ("refresh_token", Value::Null),
    ] {
        let mut body = tokens();
        body[key] = value;
        assert!(parse_tokens(&body, &config(), Envelope::empty(&config(), None), false).is_err());
    }
    let mut body = tokens();
    body.as_object_mut().unwrap().remove("refresh_token");
    assert_eq!(
        parse_tokens(&body, &config(), Envelope::empty(&config(), None), false)
            .err()
            .unwrap(),
        "DRIVE_FRESH_REFRESH_TOKEN_REQUIRED"
    );
    f.install(true);
    f.m.lock().unwrap().redirect = true;
    assert_eq!(
        grant(&f.core, &f.id).await.err().unwrap(),
        "DRIVE_TOKEN_OUTCOME_UNCERTAIN"
    );
    assert_eq!(f.m.lock().unwrap().seen.len(), 1);
    f.clean().await;
}

#[tokio::test]
async fn drive_auth_production_save_preserves_aliases_and_requires_explicit_client_migration() {
    let f = Fixture::new().await;
    f.install(false);
    let old_grant = grant(&f.core, &f.id).await.unwrap();
    let before = f
        .core
        .store
        .query("SELECT * FROM connections WHERE id=?", &[json!(f.id)])
        .unwrap();
    let generation = f
        .core
        .store
        .setting(&format!("drive.connection_generation.{}", f.id));
    let mut new = config();
    new["client_id"] = json!("owned-replacement.apps.googleusercontent.com");
    let mut input =
        json!({"id":f.id,"module_id":"drive","name":"Owned new","config":new,"secret":""});
    assert_eq!(
        integrations::save(&f.core, &input).await.err().unwrap(),
        "DRIVE_EXPLICIT_REAUTHORIZATION_REQUIRED"
    );
    assert_eq!(
        f.core
            .store
            .query("SELECT * FROM connections WHERE id=?", &[json!(f.id)])
            .unwrap(),
        before
    );
    assert_eq!(
        f.core
            .store
            .setting(&format!("drive.connection_generation.{}", f.id)),
        generation
    );
    input["reauthorize"] = json!(true);
    integrations::save(&f.core, &input).await.unwrap();
    assert_eq!(old_grant.check().unwrap_err(), "DRIVE_CONNECTION_CHANGED");
    let scope = Scope::new(&f.core, &f.id).unwrap();
    let (_, env) = scope.envelope().unwrap();
    assert_eq!(
        env.client_id,
        "owned-replacement.apps.googleusercontent.com"
    );
    assert!(env.access_token.is_none() && env.refresh_token.is_none());
    assert_eq!(
        f.core.store.setting(&state_key(&f.id))["phase"],
        "reauthorize"
    );
    assert!(grant(&f.core, &f.id).await.is_err());
    let foreign = storage::id();
    f.core
        .store
        .execute(
            "INSERT INTO connections(id,module_id,name,config_json,secret_ref) VALUES(?,?,?,?,?)",
            &[
                json!(foreign),
                json!("rest"),
                json!("Owned other provider"),
                json!("{}"),
                json!("connection.owned_other"),
            ],
        )
        .unwrap();
    input["id"] = json!(foreign);
    assert_eq!(
        integrations::save(&f.core, &input).await.err().unwrap(),
        "CONNECTION_MODULE_CHANGED"
    );
    input["id"] = json!(storage::id());
    assert_eq!(
        integrations::save(&f.core, &input).await.err().unwrap(),
        "CONNECTION_NOT_FOUND"
    );
    // Legacy aliases are not adopted; migration is explicitly authorized and
    // creates only a client-only envelope in this connection's own namespace.
    f.core
        .store
        .execute(
            "UPDATE connections SET secret_ref=?,config_json=? WHERE id=?",
            &[
                json!("connection.owned_legacy"),
                json!("{malformed"),
                json!(f.id),
            ],
        )
        .unwrap();
    input["id"] = json!(f.id);
    input["reauthorize"] = json!(false);
    assert_eq!(
        integrations::save(&f.core, &input).await.err().unwrap(),
        "DRIVE_EXPLICIT_REAUTHORIZATION_REQUIRED"
    );
    input["reauthorize"] = json!(true);
    integrations::save(&f.core, &input).await.unwrap();
    assert_eq!(record(&f.core, &f.id).unwrap(), new);
    assert!(f.m.lock().unwrap().seen.is_empty());
    drop(old_grant);
    drop(scope);
    drop(env);
    f.clean().await;
}

#[tokio::test]
async fn drive_auth_optional_client_secret_and_rotated_refresh_remain_private() {
    let f = Fixture::new().await;
    let mut profile = config();
    profile["client_secret_mode"] = json!("credential_manager");
    let mut input = json!({"id":f.id,"module_id":"drive","name":"Owned private client","config":profile,"reauthorize":true,"secret":""});
    assert_eq!(
        integrations::save(&f.core, &input).await.err().unwrap(),
        "DRIVE_NEW_CLIENT_SECRET_REQUIRED"
    );
    input["secret"] = json!("owned_fictional_client_secret");
    integrations::save(&f.core, &input).await.unwrap();
    let auth = start(&f.core, &f.input()).await.unwrap();
    assert!(f.consent(&auth, false).await.contains("200 OK"));
    f.wait_phase("ready").await;
    let scope = Scope::new(&f.core, &f.id).unwrap();
    let (version, mut env) = scope.envelope().unwrap();
    env.issued_at = Some(chrono::Utc::now().timestamp() - 4000);
    env.expires_at = Some(chrono::Utc::now().timestamp() - 1);
    let bytes = env.bytes().unwrap();
    let receipt = scope
        .private(|b| storage_replace(&f.core, b, version, bytes.bytes()))
        .unwrap();
    let expected = mark(&scope, "owned_expiry_fixture", "fixture", None).unwrap();
    finish(&scope, &expected, "ready", None, Some(&receipt)).unwrap();
    f.m.lock().unwrap().body["refresh_token"] = json!("owned_rotated_refresh");
    let renewed = grant(&f.core, &f.id).await.unwrap();
    let fields: Vec<Value> =
        f.m.lock()
            .unwrap()
            .seen
            .iter()
            .map(|s| s["fields"].clone())
            .collect();
    assert_eq!(fields.len(), 2);
    assert!(fields
        .iter()
        .all(|s| s["client_secret"] == "owned_fictional_client_secret"));
    assert_eq!(fields[1]["grant_type"], "refresh_token");
    assert_eq!(
        scope.envelope().unwrap().1.refresh_token.unwrap().text(),
        "owned_rotated_refresh"
    );
    let public = f
        .core
        .store
        .query("SELECT key,json_value FROM settings", &[])
        .unwrap()
        .to_string()
        + &f.core
            .store
            .query("SELECT * FROM connections", &[])
            .unwrap()
            .to_string();
    for value in [
        "owned_fictional_client_secret",
        "owned_rotated_refresh",
        "owned_fictional_access",
    ] {
        assert!(!public.contains(value));
    }
    drop(renewed);
    drop(env);
    drop(bytes);
    drop(scope);
    f.clean().await;
}

#[tokio::test]
async fn drive_auth_grant_revocation_before_and_during_owned_api_request() {
    for revocation in ["module", "generation", "credential"] {
        let f = Fixture::new().await;
        f.install(false);
        let grant = grant(&f.core, &f.id).await.unwrap();
        f.m.lock().unwrap().delay = true;
        let notify = f.m.lock().unwrap().started.clone();
        let c = f.core.clone();
        let id = f.id.clone();
        let revoke = async {
            notify.notified().await;
            match revocation {
                "module" => {
                    c.store
                        .execute(
                            "UPDATE module_configs SET enabled=0 WHERE module_id='drive'",
                            &[],
                        )
                        .unwrap();
                }
                "generation" => {
                    c.store
                        .set_setting(
                            &format!("drive.connection_generation.{id}"),
                            &json!(storage::id()),
                        )
                        .unwrap();
                }
                _ => {
                    let scope = Scope::new(&c, &id).unwrap();
                    let (old, env) = scope.envelope().unwrap();
                    scope
                        .private(|b| storage_replace(&c, b, old, env.bytes()?.bytes()))
                        .unwrap();
                }
            }
        };
        let began = tokio::time::Instant::now();
        let (response, _) = tokio::join!(
            grant.request(reqwest::Method::GET, "/drive/v3/files/owned_file", None),
            revoke
        );
        assert!(response.is_err());
        assert!(began.elapsed() < Duration::from_secs(1));
        assert_eq!(f.m.lock().unwrap().seen.len(), 1);
        assert!(grant
            .request(reqwest::Method::GET, "/drive/v3/files/owned_file", None)
            .await
            .is_err());
        assert_eq!(f.m.lock().unwrap().seen.len(), 1);
        drop(grant);
        drop(c);
        f.clean().await;
    }
}

#[tokio::test]
async fn drive_auth_callback_failure_reply_is_static_and_exchange_is_never_replayed() {
    let f = Fixture::new().await;
    f.m.lock().unwrap().status = 400;
    f.m.lock().unwrap().body =
        json!({"error":"invalid_grant","error_description":"owned_private_provider_description"});
    let auth = start(&f.core, &f.input()).await.unwrap();
    let reply = f.consent(&auth, false).await;
    assert!(reply.contains("400 Bad Request") && reply.contains("no-store"));
    assert!(!reply.contains("owned_private") && !reply.contains("owned_fictional_code"));
    f.wait_phase("uncertain").await;
    assert_eq!(
        f.core.store.setting(&state_key(&f.id))["error"],
        "DRIVE_REAUTHORIZE_REQUIRED"
    );
    assert!(grant(&f.core, &f.id).await.is_err());
    assert_eq!(f.m.lock().unwrap().seen.len(), 1);
    f.clean().await;
}

#[tokio::test]
async fn drive_auth_callback_probes_consent_denial_and_deadline_close_without_exchange() {
    for denied in [false, true] {
        let f = Fixture::new().await;
        f.core
            .drive_fixture
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .timeout_ms = 350;
        let auth = start(&f.core, &f.input()).await.unwrap();
        let url = reqwest::Url::parse(auth["fixture_auth_url"].as_str().unwrap()).unwrap();
        let params: std::collections::HashMap<_, _> = url.query_pairs().collect();
        let mut redirect = reqwest::Url::parse(&params["redirect_uri"]).unwrap();
        let port = redirect.port().unwrap();
        let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        if denied {
            redirect
                .query_pairs_mut()
                .append_pair("state", &params["state"])
                .append_pair("error", "access_denied")
                .append_pair("error_description", "owned_private_consent");
        } else {
            redirect
                .query_pairs_mut()
                .append_pair("state", "wrong_probe")
                .append_pair("code", "owned_probe_code");
        }
        socket
            .write_all(
                format!(
                    "GET {}?{} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n",
                    redirect.path(),
                    redirect.query().unwrap()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut bytes = vec![];
        socket.read_to_end(&mut bytes).await.unwrap();
        let reply = String::from_utf8(bytes).unwrap();
        assert!(reply.contains("400 Bad Request") && !reply.contains("owned_private"));
        f.wait_phase("reauthorize").await;
        assert_eq!(
            f.core.store.setting(&state_key(&f.id))["error"],
            if denied {
                "DRIVE_AUTH_CONSENT_DENIED"
            } else {
                "DRIVE_AUTH_TIMEOUT"
            }
        );
        assert!(tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err());
        assert!(f.m.lock().unwrap().seen.is_empty());
        drop(socket);
        drop(params);
        f.clean().await;
    }
}

#[tokio::test]
async fn drive_auth_closed_public_commands_and_generic_route_deny_before_transport() {
    let f = Fixture::new().await;
    assert!(f
        .core
        .tools()
        .iter()
        .any(|v| v["id"].as_str().unwrap().starts_with("drive.")));
    assert_eq!(
        f.core
            .execute(
                "drive.trash",
                json!({"connection":f.id,"file_id":"owned_file"})
            )
            .await
            .err()
            .unwrap(),
        "INVALID_TOOL_INPUT"
    );
    assert_eq!(
        f.core
            .store
            .query("SELECT * FROM permission_requests", &[])
            .unwrap(),
        json!([])
    );
    for input in [
        Value::Null,
        json!({"wrong":"owned"}),
        json!({"connection":f.id,"token":"owned_private"}),
        json!({"connection":12}),
    ] {
        assert!(status(&f.core, &input).is_err());
        assert!(start(&f.core, &input).await.is_err());
        assert!(cancel(&f.core, &input).is_err());
    }
    assert_eq!(
        integrations::dispatch(
            &f.core,
            "drive",
            "metadata",
            &json!({"connection":f.id,"file_id":"owned_file"})
        )
        .await
        .err()
        .unwrap(),
        "DRIVE_PREPARED_DISPATCH_REQUIRED"
    );
    assert!(f.m.lock().unwrap().seen.is_empty());
    f.clean().await;
}

#[test]
#[ignore = "Exact owned-process helper, invoked by the cross-process lease test"]
fn drive_auth_owned_process_lease_helper() {
    let root = PathBuf::from(std::env::var("ALFREDPET_OWNED_DRIVE_LEASE_ROOT").unwrap());
    let parent = root.parent().unwrap().canonicalize().unwrap();
    assert_eq!(parent, std::env::temp_dir().canonicalize().unwrap());
    assert!(root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("ap-drive-auth-"));
    let identity = std::env::var("ALFREDPET_OWNED_DRIVE_LEASE_IDENTITY").unwrap();
    assert_eq!(
        asset_files::identity(&asset_files::directory(&root).unwrap()).unwrap(),
        identity
    );
    let id = std::env::var("ALFREDPET_OWNED_DRIVE_CONNECTION").unwrap();
    canonical(&id).unwrap();
    let c = Core::new(
        root.join("profile"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
    )
    .unwrap();
    match std::env::var("ALFREDPET_OWNED_DRIVE_EXPECT_BUSY")
        .unwrap()
        .as_str()
    {
        "yes" => assert_eq!(renewal_file(&c, &id).err().unwrap(), "DRIVE_AUTH_BUSY"),
        "no" => {
            let lease = renewal_file(&c, &id).unwrap();
            assert_eq!(lease.len(), 2);
        }
        _ => panic!("OWNED_LEASE_TEST_MODE"),
    }
}

#[tokio::test]
async fn drive_auth_native_profile_lease_excludes_another_owned_process() {
    use std::os::windows::process::CommandExt;
    let f = Fixture::new().await;
    let mut held = Some(renewal_file(&f.core, &f.id).unwrap());
    for busy in ["yes", "no"] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--ignored",
                "--exact",
                "drive_auth::tests::drive_auth_owned_process_lease_helper",
                "--test-threads=1",
            ])
            .env("ALFREDPET_OWNED_DRIVE_LEASE_ROOT", &f.root)
            .env("ALFREDPET_OWNED_DRIVE_LEASE_IDENTITY", &f.identity)
            .env("ALFREDPET_OWNED_DRIVE_CONNECTION", &f.id)
            .env("ALFREDPET_OWNED_DRIVE_EXPECT_BUSY", busy)
            .creation_flags(0x08000000);
        let output = tokio::time::timeout(
            Duration::from_secs(15),
            tokio::process::Command::from(command)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
        if busy == "yes" {
            drop(held.take());
        }
    }
    drop(held);
    assert!(f.m.lock().unwrap().seen.is_empty());
    f.clean().await;
}

#[tokio::test]
async fn drive_auth_new_attempt_quarantines_old_grant_before_waiting_and_cancels_pending() {
    let f = Fixture::new().await;
    f.install(false);
    let old = grant(&f.core, &f.id).await.unwrap();
    let held = renewal_file(&f.core, &f.id).unwrap();
    let c = f.core.clone();
    let input = f.input();
    let task = tokio::spawn(async move { start(&c, &input).await });
    f.wait_phase("authorization_pending").await;
    assert_eq!(old.check().unwrap_err(), "DRIVE_REAUTHORIZE_REQUIRED");
    let state = status(&f.core, &f.input()).unwrap();
    let result = cancel(
        &f.core,
        &json!({"connection":f.id,"session_id":state["session_id"]}),
    )
    .unwrap();
    assert_eq!(result["cancelled"], true);
    assert_eq!(result["system_browser_may_have_opened"], false);
    let outcome = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome.err().unwrap(), "DRIVE_AUTH_SUPERSEDED");
    assert_eq!(
        f.core.store.setting(&state_key(&f.id))["error"],
        "DRIVE_AUTH_CANCELLED"
    );
    assert!(f.m.lock().unwrap().seen.is_empty());
    drop(held);
    drop(old);
    f.clean().await;
}

#[tokio::test]
async fn drive_auth_completed_consent_cannot_cancel_and_status_checks_manifest_binding() {
    let f = Fixture::new().await;
    let auth = start(&f.core, &f.input()).await.unwrap();
    assert!(f.consent(&auth, false).await.contains("200 OK"));
    f.wait_phase("ready").await;
    assert_eq!(status(&f.core, &f.input()).unwrap()["phase"], "ready");
    assert_eq!(
        cancel(
            &f.core,
            &json!({"connection":f.id,"session_id":auth["session_id"]})
        )
        .err()
        .unwrap(),
        "DRIVE_AUTH_ALREADY_FINISHED"
    );
    let scope = Scope::new(&f.core, &f.id).unwrap();
    scope
        .private(|b| {
            let observed = storage_observe(&f.core, b)?.unwrap();
            f.core
                .drive_fixture
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .vault
                .lock()
                .unwrap()
                .corrupt_client_metadata(b)
                .map_err(str::to_string)?;
            // The public UUID still matches: version-only checking would
            // incorrectly label this wrong-client manifest as ready.
            assert_eq!(
                storage_observe(&f.core, b)?.unwrap().version,
                observed.version
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(
        status(&f.core, &f.input()).err().unwrap(),
        "DRIVE_CREDENTIAL_CHANGED"
    );
    assert!(grant(&f.core, &f.id).await.is_err());
    assert_eq!(f.m.lock().unwrap().seen.len(), 1);
    drop(scope);
    f.clean().await;
}

#[tokio::test]
async fn drive_auth_startup_failure_ends_pending_without_adopting_an_old_ready_state() {
    let f = Fixture::new().await;
    let mut profile = config();
    profile["client_secret_mode"] = json!("credential_manager");
    let saved=integrations::save(&f.core,&json!({"module_id":"drive","name":"Owned missing client secret","config":profile,"secret":""})).await.unwrap();
    let input = json!({"connection":saved["id"]});
    assert_eq!(
        start(&f.core, &input).await.err().unwrap(),
        "DRIVE_CLIENT_SECRET_REQUIRED"
    );
    let state = status(&f.core, &input).unwrap();
    assert_eq!(state["phase"], "reauthorize");
    assert_eq!(state["error"], "DRIVE_CLIENT_SECRET_REQUIRED");
    assert!(grant(&f.core, saved["id"].as_str().unwrap()).await.is_err());
    assert!(f.m.lock().unwrap().seen.is_empty());
    f.clean().await;
}

#[tokio::test]
async fn drive_auth_new_authorization_revokes_dispatched_exchange_without_ready_publication() {
    let f = Fixture::new().await;
    f.m.lock().unwrap().delay = true;
    let notify = f.m.lock().unwrap().started.clone();
    let old = start(&f.core, &f.input()).await.unwrap();
    let replace = async {
        notify.notified().await;
        start(&f.core, &f.input()).await.unwrap()
    };
    let (reply, new) = tokio::join!(f.consent(&old, false), replace);
    assert!(reply.contains("400 Bad Request"));
    let state = status(&f.core, &f.input()).unwrap();
    assert_eq!(state["session_id"], new["session_id"]);
    assert_eq!(state["phase"], "authorizing");
    let scope = Scope::new(&f.core, &f.id).unwrap();
    assert!(scope.observe().unwrap().is_none());
    assert_eq!(f.m.lock().unwrap().seen.len(), 1);
    cancel(
        &f.core,
        &json!({"connection":f.id,"session_id":new["session_id"]}),
    )
    .unwrap();
    drop(scope);
    f.clean().await;
}
