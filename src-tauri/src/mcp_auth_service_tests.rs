use super::*;
use crate::mcp_auth::tests::Fixture;

#[tokio::test]
async fn production_resource_http_is_denied_by_core_before_permission_browser_registration_or_tokens(
) {
    let f = Owned::new().await;
    f.f.core
        .mcp_oauth_owned_network
        .store(false, Ordering::SeqCst);
    let discovery = json!({"issuer":ISSUER,"resource":format!("{}/mcp",f.base),"requested_scopes":["files:read"],"server_metadata":{"issuer":ISSUER,"exchange_endpoint":"https://owned-as.invalid/tenant/token","consent_endpoint":"https://owned-as.invalid/tenant/authorize","pkce_s256":true,"code_flow":true,"callback_issuer_required":true,"public_client_supported":true,"refresh_supported":true}});
    let expected = crate::mcp::owned_oauth_observation(&f.f.core, &f.f.id, &discovery).unwrap();
    let input = json!({"connection":f.f.id,"issuer":ISSUER,"client_auth":"none","requested_scopes":["files:read"],"expected":expected,"allow_legacy_dynamic":false,"preregistered":{"client_id":"owned-client","redirect_uri":"http://127.0.0.1/oauth/mcp/callback"}});
    assert_eq!(
        f.approve("private_start", input, None, "allow", || ())
            .await
            .unwrap_err(),
        "MCP_OAUTH_RESOURCE_TLS_OR_FIELDS"
    );
    assert!(f.f.core.pending.lock().unwrap().is_empty());
    assert!(f.f.core.mcp_oauth_jobs.lock().unwrap().is_empty());
    assert!(f
        .f
        .core
        .mcp_oauth_browser
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .is_empty());
    assert!(f.f.core.store.setting(PROFILE_KEY).is_null());
    assert!(f.f.core.store.setting(&key(&f.f.id)).is_null());
    assert!(f.capture.requests.lock().unwrap().is_empty());
    f.finish().await;
}

#[tokio::test]
async fn startup_respects_live_lease_then_quarantines_only_orphaned_intents_without_io() {
    let f = Owned::new().await;
    let started = f
        .approve(
            "private_start",
            f.input(false, "none"),
            None,
            "allow",
            || (),
        )
        .await
        .unwrap();
    let summary = startup(&f.f.core).unwrap();
    assert_eq!(summary["busy_preserved"], 1);
    assert_eq!(
        f.wait_phase("authorizing").await["session_id"],
        started["session_id"]
    );
    stop_all(&f.f.core);
    tokio::time::timeout(Duration::from_secs(5), async {
        while Arc::strong_count(&f.f.core) != 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let actor = Actor::load(&f.f.core, &f.f.id).unwrap();
    for phase in [
        Phase::Authorizing,
        Phase::RegistrationInFlight,
        Phase::ExchangeInFlight,
        Phase::RefreshInFlight,
        Phase::PublicationPending,
    ] {
        let mut state = actor.begin(false).unwrap();
        {
            let _lease = integrations::save_lock(&f.f.core).unwrap();
            let mut changed = state.clone();
            changed.phase = phase.clone();
            actor.cas_held(&state, &changed).unwrap();
            state = changed;
        }
        let summary = startup(&f.f.core).unwrap();
        assert_eq!(summary["reconciled"], 1);
        let result = actor.state().unwrap();
        assert_eq!(result.session_id, state.session_id);
        assert!(
            result.phase
                == if phase == Phase::Authorizing {
                    Phase::Cancelled
                } else {
                    Phase::Uncertain
                }
        );
    }
    assert_eq!(f.count("/token", "POST"), 0);
    assert_eq!(f.count("/register", "POST"), 0);
    assert_eq!(startup(&f.f.core).unwrap()["reconciled"], 0);
    drop(actor);
    f.finish().await;
}
#[tokio::test]
async fn unknown_reset_requires_acknowledgement_and_cannot_revive_tokens_or_replay_registration() {
    let f = Owned::new().await;
    f.approve("private_start", f.input(true, "none"), None, "allow", || ())
        .await
        .unwrap();
    f.callback(ISSUER).await;
    f.wait_phase("ready").await;
    let actor = Actor::load(&f.f.core, &f.f.id).unwrap();
    let state = actor.begin(false).unwrap();
    actor
        .transition(
            &state.session_id,
            Phase::Authorizing,
            Phase::ExchangeInFlight,
        )
        .unwrap();
    actor.abandon(&state.session_id).unwrap();
    let input = json!({"connection":f.f.id,"session_id":state.session_id});
    assert_eq!(
        f.f.core
            .execute("mcp.oauth_reset", input.clone())
            .await
            .unwrap_err(),
        "INVALID_TOOL_INPUT"
    );
    let mut input = input;
    input["acknowledge_remote_unknown"] = json!(true);
    assert!(f
        .approve("mcp.oauth_reset", input.clone(), None, "deny", || ())
        .await
        .is_err());
    assert!(actor.state().unwrap().phase == Phase::Uncertain);
    let result = f
        .approve("mcp.oauth_reset", input, None, "allow", || ())
        .await
        .unwrap();
    assert_eq!(result["phase"], "cancelled");
    assert!(actor.grant().is_err());
    assert_eq!(f.count("/token", "POST"), 1);
    assert_eq!(f.count("/register", "POST"), 1);
    drop(actor);
    let started = f
        .approve("private_start", f.input(true, "none"), None, "allow", || ())
        .await
        .unwrap();
    assert_eq!(started["phase"], "authorizing");
    assert_eq!(f.count("/register", "POST"), 1);
    f.callback(ISSUER).await;
    f.wait_phase("ready").await;
    f.finish().await;
}
#[cfg(windows)]
#[tokio::test]
async fn actual_actor_wincred_code_refresh_cleanup_and_publication_recovery_keep_tokens_private() {
    let f = Owned::new().await;
    *f.f.core.mcp_oauth_vault.lock().unwrap() = None;
    let started = f
        .approve(
            "private_start",
            f.input(false, "client_secret_post"),
            Some("owned-native-client-secret"),
            "allow",
            || (),
        )
        .await
        .unwrap();
    f.callback(ISSUER).await;
    let ready = f.wait_phase("ready").await;
    let actor = Actor::load(&f.f.core, &f.f.id).unwrap();
    let binding = actor.binding().unwrap();
    let context = actor.receipt_context();
    let (initial, bytes) = vault::win::read(&binding).unwrap();
    let env: Envelope =
        serde_json::from_value(crate::mcp_oauth::strict_json(bytes.bytes()).unwrap()).unwrap();
    env.validate(&actor).unwrap();
    assert_eq!(
        env.access_token.as_ref().unwrap().text(),
        "owned-access-service"
    );
    drop(env);
    drop(bytes);
    let renewed = f
        .approve(
            "mcp.oauth_refresh",
            json!({"connection":f.f.id,"session_id":ready["session_id"]}),
            None,
            "allow",
            || (),
        )
        .await
        .unwrap();
    let current = vault::win::read(&binding).unwrap().0;
    assert_ne!(initial, current);
    let cleaned = f
        .approve(
            "mcp.oauth_cleanup",
            json!({"connection":f.f.id,"session_id":renewed["session_id"]}),
            None,
            "allow",
            || (),
        )
        .await
        .unwrap();
    assert_eq!(cleaned["cleanup_pending"], false);
    assert_eq!(vault::win::read(&binding).unwrap().0, current);
    let entries = vault::win::mcp_context_inventory(&binding, context).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].recovery.version, current);
    assert_eq!(f.count("/token", "POST"), 2);
    assert_eq!(started["phase"], "authorizing");
    let state = actor.state().unwrap();
    let refreshed_input = json!({"connection":f.f.id,"session_id":state.session_id});
    let state_key = key(&f.f.id);
    f.f.core.store.execute(&format!("CREATE TRIGGER owned_native_publication BEFORE UPDATE ON settings WHEN NEW.key='{state_key}' AND json_extract(NEW.json_value,'$.phase')='ready' BEGIN SELECT RAISE(ABORT,'owned native publication stop'); END"),&[]).unwrap();
    assert!(f
        .approve("mcp.oauth_refresh", refreshed_input, None, "allow", || ())
        .await
        .is_err());
    let uncertain = actor.state().unwrap();
    assert!(uncertain.phase == Phase::Uncertain);
    f.f.core
        .store
        .execute("DROP TRIGGER owned_native_publication", &[])
        .unwrap();
    let recovered = f
        .approve(
            "mcp.oauth_recover",
            json!({"connection":f.f.id,"session_id":uncertain.session_id}),
            None,
            "allow",
            || (),
        )
        .await
        .unwrap();
    assert_eq!(recovered["phase"], "ready");
    let public =
        f.f.core
            .store
            .query("SELECT json_value FROM settings", &[])
            .unwrap()
            .to_string();
    let audit =
        f.f.core
            .store
            .query(
                "SELECT input_summary,output_summary FROM activity_events",
                &[],
            )
            .unwrap()
            .to_string();
    for data in [public, audit] {
        for needle in [
            "owned-native-client-secret",
            "owned-access-service",
            "owned-refresh-service",
            "owned-access-renewed",
            "owned-refresh-renewed",
        ] {
            assert!(!data.contains(needle));
        }
    }
    let current = vault::win::read(&binding).unwrap().0;
    assert!(vault::win::erase_context(&binding, current, context).unwrap());
    for item in vault::win::mcp_context_inventory(&binding, context).unwrap() {
        if !item.recovery.retirement_verified {
            let fresh = vault::win::mcp_context_inventory(&binding, context)
                .unwrap()
                .into_iter()
                .find(|r| r.recovery.version == item.recovery.version)
                .unwrap();
            assert!(vault::win::recover(&fresh.binding, Some(context), &fresh.recovery).unwrap());
        }
    }
    vault::win::compact_mcp_context(&binding, context).unwrap();
    assert!(vault::win::observe(&binding).unwrap().is_none());
    assert!(vault::win::mcp_context_inventory(&binding, context)
        .unwrap()
        .is_empty());
    drop(actor);
    f.finish().await;
}
use axum::{
    body::Body,
    extract::{Request, State as HttpState},
    http::Response,
    Router,
};
use std::{
    collections::BTreeMap,
    sync::{atomic::AtomicBool, Mutex},
    time::Duration,
};
use tokio::io::AsyncWriteExt;
const ISSUER: &str = "https://owned-as.invalid/tenant";
#[derive(Default)]
struct Capture {
    requests: Mutex<Vec<Value>>,
    delay: AtomicBool,
    started: tokio::sync::Notify,
    expires: std::sync::atomic::AtomicU64,
}
async fn handler(HttpState(c): HttpState<Arc<Capture>>, request: Request) -> Response<Body> {
    let path = request.uri().path().to_owned();
    let method = request.method().to_string();
    let headers = request
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_owned()))
        .collect::<BTreeMap<_, _>>();
    let bytes = axum::body::to_bytes(request.into_body(), 65536)
        .await
        .unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    c.requests
        .lock()
        .unwrap()
        .push(json!({"path":path,"method":method,"headers":headers,"body":body}));
    if method == "DELETE" {
        return Response::builder().status(202).body(Body::empty()).unwrap();
    }
    let mut session = false;
    let value = if path == "/token" {
        let refresh = body.contains("grant_type=refresh_token");
        let expires = c.expires.load(Ordering::SeqCst);
        json!({"token_type":"Bearer","access_token":if refresh{"owned-access-renewed"}else{"owned-access-service"},"refresh_token":if refresh{"owned-refresh-renewed"}else{"owned-refresh-service"},"expires_in":if expires==0 {3600}else{expires},"scope":"files:read"})
    } else if path == "/register" {
        let v: Value = serde_json::from_str(&body).unwrap();
        return Response::builder().status(201).header("content-type","application/json").body(Body::from(json!({"client_id":"owned-dcr-client","redirect_uris":v["redirect_uris"],"grant_types":v["grant_types"],"response_types":["code"],"token_endpoint_auth_method":"none"}).to_string())).unwrap();
    } else {
        assert_eq!(
            headers.get("authorization").map(String::as_str),
            Some(
                if headers
                    .get("authorization")
                    .is_some_and(|s| s.contains("renewed"))
                {
                    "Bearer owned-access-renewed"
                } else {
                    "Bearer owned-access-service"
                }
            )
        );
        let request: Value = serde_json::from_str(&body).unwrap();
        let result = match request["method"].as_str().unwrap() {
            "initialize" => {
                session = true;
                json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"owned","version":"1"}})
            }
            "notifications/initialized" => {
                return Response::builder().status(202).body(Body::empty()).unwrap()
            }
            "tools/list" => json!({"tools":[{"name":"write","inputSchema":{"type":"object"}}]}),
            "tools/call" => {
                if c.delay.load(Ordering::SeqCst) {
                    c.started.notify_one();
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                json!({"content":[{"type":"text","text":"owned-access-service"}]})
            }
            other => panic!("Unexpected owned MCP request: {other}"),
        };
        json!({"jsonrpc":"2.0","id":request["id"],"result":result})
    };
    let mut response = Response::builder().header("content-type", "application/json");
    if session {
        response = response.header("mcp-session-id", "owned-service-session");
    }
    response.body(Body::from(value.to_string())).unwrap()
}
struct Owned {
    f: Fixture,
    base: String,
    capture: Arc<Capture>,
    server: tokio::task::JoinHandle<()>,
}
impl Owned {
    async fn new() -> Self {
        let f = Fixture::new().await;
        f.core.mcp_headless_permission.store(true, Ordering::SeqCst);
        f.core.mcp_oauth_owned_network.store(true, Ordering::SeqCst);
        *f.core.mcp_oauth_browser.lock().unwrap() = Some(vec![]);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let capture = Arc::new(Capture::default());
        let router = Router::new().fallback(handler).with_state(capture.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let raw = json!({"profile_version":1,"transport":"streamable_http","protocol":"legacy","base_url":base,"endpoint":"/mcp","auth":{"type":"oauth","issuer":ISSUER},"permissions":{"tools":{"write":true}}});
        integrations::save(
            &f.core,
            &json!({"id":f.id,"module_id":"mcp","name":"Owned service","config":raw}),
        )
        .await
        .unwrap();
        Self {
            f,
            base,
            capture,
            server,
        }
    }
    fn input(&self, dynamic: bool, auth: &str) -> Value {
        // Explicit owned metadata fixture; discovery network has separate tests.
        let discovery = json!({"issuer":ISSUER,"resource":format!("{}/mcp",self.base),"requested_scopes":["files:read"],"server_metadata":{"issuer":ISSUER,"exchange_endpoint":format!("{}/token",self.base),"consent_endpoint":format!("{}/authorize",self.base),"pkce_s256":true,"code_flow":true,"callback_issuer_required":true,"public_client_supported":true,"basic_client_supported":true,"post_client_supported":true,"refresh_supported":true,"registration_endpoint":format!("{}/register",self.base)}});
        let expected =
            crate::mcp::owned_oauth_observation(&self.f.core, &self.f.id, &discovery).unwrap();
        let mut input = json!({"connection":self.f.id,"issuer":ISSUER,"client_auth":auth,"requested_scopes":["files:read"],"expected":expected,"allow_legacy_dynamic":dynamic});
        if !dynamic {
            input["preregistered"] = json!({"client_id":"owned-client","redirect_uri":"http://127.0.0.1/oauth/mcp/callback"});
        }
        input
    }
    async fn approve(
        &self,
        tool: &str,
        input: Value,
        secret: Option<&str>,
        answer: &str,
        change: impl FnOnce(),
    ) -> Result<Value> {
        let core = self.f.core.clone();
        let tool = tool.to_owned();
        let secret = secret.map(|s| Text::new(s.into(), 16384).unwrap());
        let mut task = tokio::spawn(async move {
            if tool == "private_start" {
                core.execute_mcp_oauth(input, secret).await
            } else {
                core.execute(&tool, input).await
            }
        });
        let tx = tokio::select! {result=&mut task=>return result.unwrap(),r=tokio::time::timeout(Duration::from_secs(8),async {
            loop {let id=self.f.core.pending.lock().unwrap().keys().next().cloned();if let Some(id)=id {
                let tx=self.f.core.pending.lock().unwrap().remove(&id).unwrap();break tx;
            } tokio::time::sleep(Duration::from_millis(10)).await;}
        })=>r.unwrap()};
        change();
        tx.send(answer.into()).unwrap();
        task.await.unwrap()
    }
    async fn callback(&self, issuer: &str) {
        let url = reqwest::Url::parse(
            self.f
                .core
                .mcp_oauth_browser
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .last()
                .unwrap(),
        )
        .unwrap();
        let pairs = url.query_pairs().collect::<BTreeMap<_, _>>();
        let mut redirect = reqwest::Url::parse(&pairs["redirect_uri"]).unwrap();
        redirect
            .query_pairs_mut()
            .append_pair("state", &pairs["state"])
            .append_pair("code", "owned-code")
            .append_pair("iss", issuer);
        let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", redirect.port().unwrap()))
            .await
            .unwrap();
        let path = format!("{}?{}", redirect.path(), redirect.query().unwrap());
        socket
            .write_all(
                format!(
                    "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
                    redirect.port().unwrap()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    }
    async fn wait_phase(&self, phase: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let state = status(&self.f.core, &json!({"connection":self.f.id})).unwrap();
                if state["phase"] == phase {
                    return state;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    fn count(&self, path: &str, method: &str) -> usize {
        self.capture
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["path"] == path && r["method"] == method)
            .count()
    }
    async fn finish(self) {
        stop_all(&self.f.core);
        tokio::time::timeout(Duration::from_secs(5), async {
            while Arc::strong_count(&self.f.core) != 1 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        self.server.abort();
        self.f.finish();
    }
}
#[tokio::test]
async fn approved_private_start_callback_native_grant_transport_redaction_and_explicit_refresh() {
    let f = Owned::new().await;
    let input = f.input(false, "client_secret_basic");
    let before = f.f.core.store.setting(PROFILE_KEY);
    let prepared = crate::mcp::prepare(&f.f.core, "mcp.oauth_start", &input)
        .await
        .unwrap();
    assert!(prepared.review()["oauth_registration"]["loginAvailable"] == true);
    assert_eq!(f.f.core.store.setting(PROFILE_KEY), before);
    drop(prepared);
    let started = f
        .approve(
            "private_start",
            input,
            Some("owned-client-service-secret"),
            "allow",
            || (),
        )
        .await
        .unwrap();
    assert_eq!(started["phase"], "authorizing");
    assert_eq!(f.count("/token", "POST"), 0);
    assert!(!started.to_string().contains("code_challenge"));
    f.callback(ISSUER).await;
    let ready = f.wait_phase("ready").await;
    assert_eq!(ready["credential_status"], "usable");
    let list = f
        .approve(
            "mcp.tools",
            json!({"connection":f.f.id}),
            None,
            "allow",
            || (),
        )
        .await
        .unwrap();
    let out=f.approve("mcp.call",json!({"connection":f.f.id,"name":"write","body":{},"expected":list["tools"][0]["alfredpetObservation"]}),None,"allow",||()).await.unwrap();
    assert_eq!(out["content"][0]["text"], "[REDACTED]");
    assert_eq!(f.count("/mcp", "DELETE"), 2);
    let refreshed = f
        .approve(
            "mcp.oauth_refresh",
            json!({"connection":f.f.id,"session_id":ready["session_id"]}),
            None,
            "allow",
            || (),
        )
        .await
        .unwrap();
    assert_eq!(refreshed["phase"], "ready");
    assert_eq!(f.count("/token", "POST"), 2);
    let public =
        f.f.core
            .store
            .query("SELECT json_value FROM settings", &[])
            .unwrap()
            .to_string();
    let audit =
        f.f.core
            .store
            .query(
                "SELECT input_summary,output_summary FROM activity_events",
                &[],
            )
            .unwrap()
            .to_string();
    let reviews =
        f.f.core
            .store
            .query("SELECT input_json_redacted FROM permission_requests", &[])
            .unwrap()
            .to_string();
    for value in [
        public,
        audit,
        reviews,
        started.to_string(),
        refreshed.to_string(),
    ] {
        for needle in [
            "owned-client-service-secret",
            "owned-access-service",
            "owned-refresh-service",
            "code_verifier",
            "code_challenge",
        ] {
            assert!(!value.contains(needle));
        }
    }
    f.finish().await;
}
#[tokio::test]
async fn denial_stale_discovery_and_inline_secret_never_launch_or_register() {
    let f = Owned::new().await;
    let input = f.input(true, "none");
    assert!(f
        .approve("private_start", input.clone(), None, "deny", || ())
        .await
        .is_err());
    assert!(f.f.core.store.setting(PROFILE_KEY).is_null());
    let initial_status = f
        .approve(
            "mcp.oauth_status",
            json!({"connection":f.f.id}),
            None,
            "allow",
            || (),
        )
        .await
        .unwrap();
    assert_eq!(initial_status["phase"], "not_started");
    assert!(f.f.core.store.setting(PROFILE_KEY).is_null());
    let mut inline = input.clone();
    inline["client_secret"] = json!("owned-inline");
    assert_eq!(
        f.f.core
            .execute("mcp.oauth_start", inline)
            .await
            .unwrap_err(),
        "INVALID_TOOL_INPUT"
    );
    let error = f
        .approve("private_start", input, None, "allow", || {
            crate::mcp::revoke(&f.f.core).unwrap();
        })
        .await
        .unwrap_err();
    assert_eq!(error, "MCP_CONNECTION_CHANGED");
    assert_eq!(f.count("/register", "POST"), 0);
    assert!(f
        .f
        .core
        .mcp_oauth_browser
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .is_empty());
    assert!(f.f.core.store.setting(PROFILE_KEY).is_null());
    f.finish().await;
}
#[tokio::test]
async fn uncertain_intent_cannot_adopt_an_older_private_packet_or_replay_token() {
    let f = Owned::new().await;
    f.approve(
        "private_start",
        f.input(false, "none"),
        None,
        "allow",
        || (),
    )
    .await
    .unwrap();
    f.callback(ISSUER).await;
    f.wait_phase("ready").await;
    let actor = Actor::load(&f.f.core, &f.f.id).unwrap();
    let state = actor.begin(false).unwrap();
    actor
        .transition(
            &state.session_id,
            Phase::Authorizing,
            Phase::ExchangeInFlight,
        )
        .unwrap();
    let uncertain = actor.abandon(&state.session_id).unwrap();
    let error = f
        .approve(
            "mcp.oauth_recover",
            json!({"connection":f.f.id,"session_id":uncertain.session_id}),
            None,
            "allow",
            || (),
        )
        .await
        .unwrap_err();
    assert_eq!(error, "MCP_OAUTH_REMOTE_OUTCOME_UNKNOWN");
    assert!(actor.grant().is_err());
    assert_eq!(f.count("/token", "POST"), 1);
    drop(actor);
    f.finish().await;
}
#[tokio::test]
async fn dynamic_client_cancel_reuses_native_client_without_second_registration() {
    let f = Owned::new().await;
    let started = f
        .approve("private_start", f.input(true, "none"), None, "allow", || ())
        .await
        .unwrap();
    assert_eq!(f.count("/register", "POST"), 1);
    let cancelled = f
        .approve(
            "mcp.oauth_cancel",
            json!({"connection":f.f.id,"session_id":started["session_id"]}),
            None,
            "allow",
            || (),
        )
        .await
        .unwrap();
    assert_eq!(cancelled["phase"], "cancelled");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !f.f.core.mcp_oauth_jobs.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let second = f
        .approve("private_start", f.input(true, "none"), None, "allow", || ())
        .await
        .unwrap();
    assert_ne!(second["session_id"], started["session_id"]);
    assert_eq!(f.count("/register", "POST"), 1);
    f.callback(ISSUER).await;
    f.wait_phase("ready").await;
    f.finish().await;
}
#[tokio::test]
async fn revocation_during_business_blocks_reply_and_remote_delete_with_old_token() {
    let f = Owned::new().await;
    f.approve(
        "private_start",
        f.input(false, "none"),
        None,
        "allow",
        || (),
    )
    .await
    .unwrap();
    f.callback(ISSUER).await;
    f.wait_phase("ready").await;
    let list = f
        .approve(
            "mcp.tools",
            json!({"connection":f.f.id}),
            None,
            "allow",
            || (),
        )
        .await
        .unwrap();
    let before = f.count("/mcp", "DELETE");
    f.capture.delay.store(true, Ordering::SeqCst);
    let future=f.approve("mcp.call",json!({"connection":f.f.id,"name":"write","body":{},"expected":list["tools"][0]["alfredpetObservation"]}),None,"allow",||());
    let revoke = async {
        f.capture.started.notified().await;
        crate::mcp::revoke(&f.f.core).unwrap();
    };
    let (result, _) = tokio::join!(future, revoke);
    assert!(result.unwrap_err().starts_with("MCP_OUTCOME_UNCERTAIN:"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(f.count("/mcp", "DELETE"), before);
    f.finish().await;
}
#[tokio::test]
async fn publication_failure_requires_native_proof_and_recovery_never_reposts_token() {
    let f = Owned::new().await;
    let started = f
        .approve(
            "private_start",
            f.input(false, "none"),
            None,
            "allow",
            || (),
        )
        .await
        .unwrap();
    let state_key = key(&f.f.id);
    f.f.core.store.execute(&format!("CREATE TRIGGER owned_service_publication BEFORE UPDATE ON settings WHEN NEW.key='{state_key}' AND json_extract(NEW.json_value,'$.phase')='ready' BEGIN SELECT RAISE(ABORT,'owned publication stop'); END"),&[]).unwrap();
    f.callback(ISSUER).await;
    let uncertain = f.wait_phase("uncertain").await;
    assert_eq!(uncertain["session_id"], started["session_id"]);
    assert_eq!(f.count("/token", "POST"), 1);
    assert!(Actor::load(&f.f.core, &f.f.id).unwrap().grant().is_err());
    f.f.core
        .store
        .execute("DROP TRIGGER owned_service_publication", &[])
        .unwrap();
    let recovered = f
        .approve(
            "mcp.oauth_recover",
            json!({"connection":f.f.id,"session_id":uncertain["session_id"]}),
            None,
            "allow",
            || (),
        )
        .await
        .unwrap();
    assert_eq!(recovered["phase"], "ready");
    assert_eq!(recovered["cleanup_pending"], true);
    assert_eq!(f.count("/token", "POST"), 1);
    assert_eq!(f.wait_phase("ready").await["credential_status"], "usable");
    assert!(f
        .approve(
            "mcp.oauth_refresh",
            json!({"connection":f.f.id,"session_id":storage::id()}),
            None,
            "allow",
            || ()
        )
        .await
        .is_err());
    assert_eq!(f.count("/token", "POST"), 1);
    f.finish().await;
}
#[tokio::test]
async fn wrong_callback_issuer_and_expired_grant_cannot_dispatch_mcp() {
    let f = Owned::new().await;
    f.approve(
        "private_start",
        f.input(false, "none"),
        None,
        "allow",
        || (),
    )
    .await
    .unwrap();
    f.callback("https://foreign.invalid").await;
    f.wait_phase("cancelled").await;
    assert_eq!(f.count("/token", "POST"), 0);
    f.capture.expires.store(1, Ordering::SeqCst);
    f.approve(
        "private_start",
        f.input(false, "none"),
        None,
        "allow",
        || (),
    )
    .await
    .unwrap();
    f.callback(ISSUER).await;
    f.wait_phase("ready").await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(f.wait_phase("ready").await["credential_status"], "expired");
    let error = f
        .approve(
            "mcp.tools",
            json!({"connection":f.f.id}),
            None,
            "allow",
            || (),
        )
        .await
        .unwrap_err();
    assert_eq!(error, "MCP_OAUTH_REFRESH_REQUIRED");
    assert_eq!(f.count("/mcp", "POST"), 0);
    f.finish().await;
}
