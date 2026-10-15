use super::*;
use axum::{
    body::Body,
    extract::{Request, State},
    http::Response,
    Router,
};
use std::sync::Mutex;
#[path = "mcp_catalog_contracts.rs"]
mod catalog_contracts;
#[cfg(windows)]
#[path = "mcp_listen_contracts.rs"]
mod listen_contracts;
#[cfg(windows)]
#[path = "mcp_sdk_contracts.rs"]
mod sdk;
struct Mock {
    seen: Vec<Value>,
    definition: Value,
    delay: bool,
    delay_method: Option<String>,
    error: bool,
    started: Arc<tokio::sync::Notify>,
    output: Value,
}
async fn handler(State(m): State<Arc<Mutex<Mock>>>, r: Request) -> Response<Body> {
    let headers = r
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect::<std::collections::BTreeMap<_, _>>();
    if r.method() == "GET" {
        let (delay, started) = {
            let mut m = m.lock().unwrap();
            m.seen.push(json!({"request":{"method":"anonymous/GET"},"headers":headers,"path":r.uri().path()}));
            (m.delay, m.started.clone())
        };
        if delay {
            started.notify_one();
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        return Response::builder()
            .status(401)
            .header("www-authenticate", "Bearer scope=read")
            .body(Body::empty())
            .unwrap();
    }
    let body = axum::body::to_bytes(r.into_body(), 128 * 1024)
        .await
        .unwrap();
    let v: Value = serde_json::from_slice(&body).unwrap();
    let (result, delay, error, started) = {
        let mut m = m.lock().unwrap();
        m.seen.push(json!({"request":v,"headers":headers}));
        let result = match v["method"].as_str().unwrap() {
            "server/discover" => {
                json!({"supportedVersions":[client::protocol::MODERN],"capabilities":{"tools":{},"resources":{},"prompts":{}}})
            }
            "tools/list" => {
                json!({"tools":[m.definition.clone(),{"name":"forbidden","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}}]})
            }
            "resources/list" => json!({"resources":[{"uri":"owned:item","name":"item"}]}),
            "prompts/list" => {
                json!({"prompts":[{"name":"greet","arguments":[{"name":"who","required":true}]}]})
            }
            "resources/read" => json!({"contents":[{"uri":"owned:item","text":"Señor 🐾"}]}),
            "prompts/get" => {
                json!({"messages":[{"role":"user","content":{"type":"text","text":"Señor 🐾"}}]})
            }
            "tools/call" => m.output.clone(),
            _ => panic!("unexpected request"),
        };
        (
            result,
            (m.delay && v["method"] == "tools/call")
                || m.delay_method
                    .as_ref()
                    .is_some_and(|method| v["method"] == *method),
            m.error && v["method"] == "tools/call",
            m.started.clone(),
        )
    };
    if delay {
        started.notify_one();
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let mut result = result;
    result["resultType"] = json!("complete");
    let response = if error {
        json!({"jsonrpc":"2.0","id":v["id"],"error":{"code":-32000,"message":"private provider error"}})
    } else {
        json!({"jsonrpc":"2.0","id":v["id"],"result":result})
    };
    Response::builder()
        .header("content-type", "application/json")
        .body(Body::from(response.to_string()))
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
            definition: json!({"name":"write","inputSchema":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false},"outputSchema":{"type":"object","properties":{"ok":{"const":true}},"required":["ok"]},"annotations":{"readOnlyHint":true,"destructiveHint":false},"alfredpetObservation":{"fingerprint":"remote_forged"}}),
            delay: false,
            delay_method: None,
            error: false,
            started: Arc::new(tokio::sync::Notify::new()),
            output: json!({"content":[{"type":"text","text":"Señor 🐾"}],"structuredContent":{"ok":true},"undo_token":"untrusted remote undo"}),
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = Router::new().fallback(handler).with_state(m.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let root = std::env::temp_dir().join(format!("ap-mcp-{}", uuid::Uuid::new_v4()));
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
                "UPDATE module_configs SET enabled=1 WHERE module_id='mcp'",
                &[],
            )
            .unwrap();
        core.mcp_headless_permission.store(true, Ordering::SeqCst);
        let raw = json!({"profile_version":1,"transport":"streamable_http","base_url":format!("http://{addr}"),"endpoint":"/mcp","auth":{"type":"none"},"permissions":{"tools":{"write":true},"resources":["owned:item"],"prompts":{"greet":true}}});
        let saved = integrations::save(
            &core,
            &json!({"module_id":"mcp","name":"Owned MCP","config":raw}),
        )
        .await
        .unwrap();
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
        let mut input = v;
        input["connection"] = json!(self.id);
        input
    }
    fn raw(&self) -> Value {
        record(&self.core, &self.id).unwrap().0
    }
    async fn save(&self, raw: &Value, secret: Option<&str>) -> Result<Value, String> {
        integrations::save(&self.core,&json!({"id":self.id,"module_id":"mcp","name":"Owned MCP edited","config":raw,"secret":secret})).await
    }
    async fn approve(
        &self,
        tool: &str,
        input: Value,
        answer: &str,
        change: impl FnOnce(),
    ) -> Result<Value, String> {
        let core = self.core.clone();
        let input = self.input(input);
        let requested_tool = tool.to_owned();
        let mut task = tokio::spawn(async move { core.execute(&requested_tool, input).await });
        let started = Instant::now();
        let tx = tokio::select! { r=&mut task=>return r.unwrap(), r=tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let id = self.core.pending.lock().unwrap().keys().next().cloned();
                if let Some(id) = id {
                    break self.core.pending.lock().unwrap().remove(&id).unwrap();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })=>r.unwrap_or_else(|_| panic!("Owned approval fixture timed out before review: tool={tool}, elapsed_ms={}, pending={}, task_finished={}", started.elapsed().as_millis(), self.core.pending.lock().unwrap().len(), task.is_finished())) };
        change();
        tx.send(answer.into()).unwrap();
        task.await.unwrap()
    }
    async fn observed(&self) -> Value {
        let out = self
            .approve("mcp.tools", json!({}), "allow", || ())
            .await
            .unwrap();
        json!({"name":"write","body":{"value":"Señor 🐾"},"expected":out["tools"][0]["alfredpetObservation"]})
    }
    fn count(&self, method: &str) -> usize {
        self.m
            .lock()
            .unwrap()
            .seen
            .iter()
            .filter(|v| v["request"]["method"] == method)
            .count()
    }
    async fn clean(self) {
        self.server.abort();
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
            .starts_with("ap-mcp-"));
        assert_eq!(
            crate::asset_files::identity(&crate::asset_files::directory(&root).unwrap()).unwrap(),
            self.identity
        );
        drop(self.core);
        std::fs::remove_dir_all(root).unwrap();
    }
}
#[tokio::test]
async fn oauth_discovery_review_is_pure_and_permission_cannot_be_session_granted() {
    let f = Fixture::new().await;
    let input = f.input(json!({"issuer":"https://as.example/tenant"}));
    let p = prepare(&f.core, "mcp.oauth_discover", &input)
        .await
        .unwrap();
    assert_eq!(p.risk(), Risk::Mandatory);
    assert_eq!(p.review()["oauth_discovery"]["anonymous"], true);
    assert_eq!(
        p.review()["oauth_discovery"]["issuer"],
        "https://as.example/tenant"
    );
    assert_eq!(
        p.review()["oauth_discovery"]["server_metadata_candidates"][0],
        "https://as.example/.well-known/oauth-authorization-server/tenant"
    );
    assert!(f.m.lock().unwrap().seen.is_empty());
    drop(p);
    for answer in ["deny", "session"] {
        assert_eq!(
            f.approve(
                "mcp.oauth_discover",
                json!({"issuer":"https://as.example/tenant"}),
                answer,
                || ()
            )
            .await
            .unwrap_err(),
            "PERMISSION_DENIED_OR_TIMEOUT"
        );
        assert!(f.m.lock().unwrap().seen.is_empty());
    }
    assert!(prepare(
        &f.core,
        "mcp.oauth_discover",
        &f.input(json!({"issuer":"http://as.example"}))
    )
    .await
    .is_err());
    assert!(prepare(
        &f.core,
        "mcp.oauth_discover",
        &f.input(json!({"issuer":"https://as.example","client_secret":"inline denied"}))
    )
    .await
    .is_err());
    f.clean().await;
}

async fn registration_observation(f: &Fixture) -> Value {
    // This contract injects an owned discovery observation to exercise Core's
    // review boundary. The HTTP discovery parser has separate owned fixtures.
    // Pure review must accept a production-valid HTTPS resource, without the
    // cfg(test) loopback OAuth exemption or any network request.
    let mut raw = f.raw();
    let base = raw["base_url"].as_str().unwrap();
    assert!(base.starts_with("http://127.0.0.1:"));
    raw["base_url"] = json!(base.replacen("http://", "https://", 1));
    f.save(&raw, None).await.unwrap();
    let scope = Scope::new(&f.core, &f.id).unwrap();
    let issuer = "https://as.example/tenant";
    let discovery = json!({"issuer":issuer,"resource":profile::endpoint(&scope.config).unwrap().as_str(),"requested_scopes":["files:read"],
        "server_metadata":{"issuer":issuer,"consent_endpoint":"https://as.example/authorize","exchange_endpoint":"https://as.example/token","registration_endpoint":"https://as.example/register","pkce_s256":true,"code_flow":true,"public_client_supported":true,"basic_client_supported":true,"post_client_supported":true,"refresh_supported":true,"client_metadata_documents_supported":true,"callback_issuer_required":true}});
    scope
        .observe(
            "oauth_authorize",
            issuer,
            &discovery,
            client::protocol::MODERN,
            None,
        )
        .unwrap()
}

#[tokio::test]
async fn oauth_registration_core_review_requires_individual_permission_and_performs_no_enrollment()
{
    let f = Fixture::new().await;
    let expected = registration_observation(&f).await;
    let input = json!({"issuer":"https://as.example/tenant","expected":expected,
        "preregistered":{"client_id":"registered-client","redirect_uri":"http://127.0.0.1/oauth/mcp/callback"},
        "metadata_document_url":"https://client.example/metadata.json","allow_legacy_dynamic":true,"client_auth":"client_secret_basic","requested_scopes":["files:write"]});
    let prepared = prepare(&f.core, "mcp.oauth_registration", &f.input(input.clone()))
        .await
        .unwrap();
    assert_eq!(prepared.risk(), Risk::Mandatory);
    assert_eq!(
        prepared.review()["oauth_registration"]["registration"]["choice"]["type"],
        "pre_registered"
    );
    assert_eq!(
        prepared.review()["oauth_registration"]["requested_scopes"],
        json!(["files:write"])
    );
    drop(prepared);
    for answer in ["deny", "session"] {
        assert_eq!(
            f.approve("mcp.oauth_registration", input.clone(), answer, || ())
                .await
                .unwrap_err(),
            "PERMISSION_DENIED_OR_TIMEOUT"
        );
    }
    let out = f
        .approve("mcp.oauth_registration", input, "allow", || ())
        .await
        .unwrap();
    assert_eq!(out["client_id"], "registered-client");
    for field in [
        "browserStarted",
        "credentialsStored",
        "registrationPerformed",
        "loginAvailable",
    ] {
        assert_eq!(out[field], false);
    }
    assert!(f.m.lock().unwrap().seen.is_empty());
    assert!(f
        .core
        .store
        .query("SELECT key FROM settings WHERE key LIKE 'mcp.oauth.%'", &[])
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    assert!(f.core.store.setting("mcp.oauth.profile_identity").is_null());
    f.clean().await;
}

#[tokio::test]
async fn oauth_registration_rejects_unknown_or_expired_observation_secret_inputs_and_cross_issuer()
{
    let f = Fixture::new().await;
    let expected = registration_observation(&f).await;
    let good = json!({"issuer":"https://as.example/tenant","expected":expected});
    for bad in [
        json!({"issuer":"https://as.example/tenant","expected":{"fingerprint":"0".repeat(64)}}),
        json!({"issuer":"https://other.example","expected":expected}),
        json!({"issuer":"https://as.example/tenant","expected":expected,"client_secret":"owned-private-denied"}),
        json!({"issuer":"https://as.example/tenant","expected":expected,"requested_scopes":[]}),
        json!({"issuer":"https://as.example/tenant","expected":expected,"requested_scopes":["files:read","files:read"]}),
    ] {
        assert!(prepare(&f.core, "mcp.oauth_registration", &f.input(bad))
            .await
            .is_err());
    }
    let out = f
        .approve("mcp.oauth_registration", good.clone(), "allow", || ())
        .await
        .unwrap();
    assert_eq!(out["registration"]["choice"]["type"], "manual");
    assert_eq!(out["requested_scopes"], json!(["files:read"]));
    assert!(out["client_id"].is_null());
    let fingerprint = expected["fingerprint"].as_str().unwrap();
    f.core
        .mcp_observations
        .lock()
        .unwrap()
        .get_mut(fingerprint)
        .unwrap()
        .expires = Instant::now() - Duration::from_secs(1);
    assert!(prepare(&f.core, "mcp.oauth_registration", &f.input(good))
        .await
        .is_err());
    assert!(f.core.pending.lock().unwrap().is_empty());
    assert!(f.m.lock().unwrap().seen.is_empty());
    f.clean().await;
}

#[tokio::test]
async fn oauth_registration_scope_and_discovery_expiry_are_rechecked_after_permission() {
    for change in ["expiry", "revocation", "profile"] {
        let f = Fixture::new().await;
        let expected = registration_observation(&f).await;
        let fingerprint = expected["fingerprint"].as_str().unwrap().to_string();
        let input = json!({"issuer":"https://as.example/tenant","expected":expected,"allow_legacy_dynamic":true,"requested_scopes":null});
        let permission_seen = std::sync::atomic::AtomicBool::new(false);
        let out = f
            .approve("mcp.oauth_registration", input, "allow", || {
                permission_seen.store(true, Ordering::SeqCst);
                if change == "expiry" {
                    f.core
                        .mcp_observations
                        .lock()
                        .unwrap()
                        .get_mut(&fingerprint)
                        .unwrap()
                        .expires = Instant::now() - Duration::from_secs(1);
                } else if change == "revocation" {
                    revoke(&f.core).unwrap();
                } else {
                    f.core
                        .store
                        .set_setting(
                            &mcp_save::generation_key(&f.id),
                            &json!(crate::storage::id()),
                        )
                        .unwrap();
                }
            })
            .await;
        assert!(
            permission_seen.load(Ordering::SeqCst),
            "OAuth review never reached permission for {change}"
        );
        assert!(out.is_err(), "stale OAuth review survived {change}");
        assert!(f.m.lock().unwrap().seen.is_empty());
        f.clean().await;
    }
}
#[tokio::test]
async fn oauth_discovery_revocation_before_dispatch_prevents_network() {
    let f = Fixture::new().await;
    let result = f
        .approve(
            "mcp.oauth_discover",
            json!({"issuer":"https://as.example"}),
            "allow",
            || {
                revoke(&f.core).unwrap();
            },
        )
        .await;
    assert_eq!(result.unwrap_err(), "MCP_CONNECTION_CHANGED");
    assert!(f.m.lock().unwrap().seen.is_empty());
    f.clean().await;
}
#[tokio::test]
async fn oauth_discovery_is_anonymous_even_with_unavailable_ready_private_material() {
    let f = Fixture::new().await;
    let mut raw = f.raw();
    raw["auth"] = json!({"type":"bearer","secret_ref":"main"});
    let c = profile::config(&raw).unwrap();
    let private =
        credentials::publication(&f.id, &c, r#"{"main":"fictional-unpublished-credential"}"#)
            .unwrap();
    f.core
        .store
        .execute(
            "UPDATE connections SET config_json=? WHERE id=?",
            &[json!(raw.to_string()), json!(f.id)],
        )
        .unwrap();
    f.core
        .store
        .set_setting(&mcp_save::binding_key(&f.id), &json!(private.ready))
        .unwrap();
    assert_eq!(
        f.approve(
            "mcp.oauth_discover",
            json!({"issuer":"https://as.example"}),
            "allow",
            || ()
        )
        .await
        .unwrap_err(),
        "MCP_OAUTH_METADATA_HTTP_401"
    );
    assert_eq!(f.count("anonymous/GET"), 2);
    for seen in &f.m.lock().unwrap().seen {
        assert!(seen["headers"].get("authorization").is_none());
    }
    f.clean().await;
}
#[tokio::test]
async fn oauth_discovery_revoked_during_resource_get_stops_before_metadata() {
    let f = Fixture::new().await;
    f.m.lock().unwrap().delay = true;
    let started = f.m.lock().unwrap().started.clone();
    let core = f.core.clone();
    let result = f
        .approve(
            "mcp.oauth_discover",
            json!({"issuer":"https://as.example"}),
            "allow",
            move || {
                tokio::spawn(async move {
                    started.notified().await;
                    revoke(&core).unwrap();
                });
            },
        )
        .await;
    assert_eq!(result.unwrap_err(), "MCP_CONNECTION_CHANGED");
    assert_eq!(f.count("anonymous/GET"), 1);
    f.clean().await;
}
#[tokio::test]
async fn mandatory_approval_owned_observation_and_untrusted_hints() {
    let f = Fixture::new().await;
    let tools = f
        .approve("mcp.tools", json!({}), "allow", || ())
        .await
        .unwrap();
    assert_eq!(tools["tools"][1]["alfredpetPermission"], "denied");
    assert!(tools["tools"][1].get("alfredpetObservation").is_none());
    let input = json!({"name":"write","body":{"value":"Señor 🐾"},"expected":tools["tools"][0]["alfredpetObservation"]});
    assert_ne!(input["expected"]["fingerprint"], "remote_forged");
    assert_eq!(
        f.approve("mcp.call", input.clone(), "session", || ())
            .await
            .unwrap_err(),
        "PERMISSION_DENIED_OR_TIMEOUT"
    );
    assert_eq!(f.count("tools/call"), 0);
    let out = f.approve("mcp.call", input, "allow", || ()).await.unwrap();
    assert!(out.get("undo_token").is_none());
    assert_eq!(out["structuredContent"]["ok"], true);
    assert_eq!(f.count("tools/call"), 1);
    let audit = f
        .core
        .store
        .query(
            "SELECT risk,input_summary,output_summary,undo_token FROM activity_events",
            &[],
        )
        .unwrap();
    assert!(!audit.to_string().contains("Señor"));
    assert!(!audit.to_string().contains("remote undo"));
    assert!(audit
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v["risk"] == "Mandatory"));
    f.clean().await;
}
#[tokio::test]
async fn deny_unknown_subject_forged_observation_and_dispatch_bypass_before_io() {
    let f = Fixture::new().await;
    assert_eq!(
        f.core
            .execute(
                "mcp.call",
                f.input(
                    json!({"name":"forbidden","body":{},"expected":{"fingerprint":"a".repeat(64)}})
                )
            )
            .await
            .unwrap_err(),
        "MCP_SUBJECT_NOT_ALLOWED"
    );
    assert_eq!(f.core.execute("mcp.call",f.input(json!({"name":"write","body":{"value":"ok"},"expected":{"fingerprint":"a".repeat(64)}}))).await.unwrap_err(),"MCP_OBSERVATION_EXPIRED_OR_UNKNOWN");
    assert_eq!(
        integrations::dispatch(&f.core, "mcp", "health", &json!({}))
            .await
            .unwrap_err(),
        "MCP_PREPARED_DISPATCH_REQUIRED"
    );
    assert_eq!(f.count("server/discover"), 0);
    assert!(f.core.pending.lock().unwrap().is_empty());
    f.clean().await;
}
#[tokio::test]
async fn profile_generation_and_permission_revocation_invalidate_previous_observation() {
    let f = Fixture::new().await;
    let input = f.observed().await;
    let raw = f.raw();
    let prepared = prepare(&f.core, "mcp.call", &f.input(input.clone()))
        .await
        .unwrap();
    f.save(&raw, None).await.unwrap();
    assert_eq!(
        execute(prepared).await.unwrap_err(),
        "MCP_CONNECTION_CHANGED"
    );
    assert_eq!(
        f.core
            .execute("mcp.call", f.input(input))
            .await
            .unwrap_err(),
        "MCP_OBSERVATION_CHANGED_REVIEW_REQUIRED"
    );
    assert_eq!(f.count("tools/call"), 0);
    f.clean().await;
}
#[tokio::test]
async fn fresh_server_schema_change_denies_without_business_dispatch() {
    let f = Fixture::new().await;
    let input = f.observed().await;
    f.m.lock().unwrap().definition["inputSchema"]["properties"]["value"]["maxLength"] = json!(3);
    assert_eq!(
        f.approve("mcp.call", input, "allow", || ())
            .await
            .unwrap_err(),
        "MCP_SERVER_OBSERVATION_CHANGED_REVIEW_REQUIRED"
    );
    assert_eq!(f.count("tools/call"), 0);
    f.clean().await;
}
#[tokio::test]
async fn pending_approval_is_revoked_by_privacy_epoch_even_after_restore() {
    let f = Fixture::new().await;
    let input = f.observed().await;
    assert_eq!(
        f.approve("mcp.call", input, "allow", || {
            revoke(&f.core).unwrap();
        })
        .await
        .unwrap_err(),
        "MCP_CONNECTION_CHANGED"
    );
    assert_eq!(f.count("tools/call"), 0);
    f.clean().await;
}
#[tokio::test]
async fn in_flight_revocation_and_remote_error_have_uncertain_outcome_without_replay() {
    let f = Fixture::new().await;
    let input = f.observed().await;
    f.m.lock().unwrap().error = true;
    assert_eq!(
        f.approve("mcp.call", input.clone(), "allow", || ())
            .await
            .unwrap_err(),
        "MCP_OUTCOME_UNCERTAIN:MCP_REMOTE_ERROR"
    );
    assert_eq!(f.count("tools/call"), 1);
    let started = {
        let mut m = f.m.lock().unwrap();
        m.error = false;
        m.delay = true;
        m.started.clone()
    };
    let core = f.core.clone();
    let revoke_task = tokio::spawn(async move {
        started.notified().await;
        revoke(&core).unwrap();
    });
    assert_eq!(
        f.approve("mcp.call", input, "allow", || ())
            .await
            .unwrap_err(),
        "MCP_OUTCOME_UNCERTAIN:MCP_CONNECTION_CHANGED"
    );
    revoke_task.await.unwrap();
    assert_eq!(f.count("tools/call"), 2);
    f.clean().await;
}
#[tokio::test]
async fn resources_and_prompts_use_exact_observations_and_closed_arguments() {
    let f = Fixture::new().await;
    let resources = f
        .approve("mcp.resources", json!({}), "allow", || ())
        .await
        .unwrap();
    let prompts = f
        .approve("mcp.prompts", json!({}), "allow", || ())
        .await
        .unwrap();
    let out=f.approve("mcp.read_resource",json!({"uri":"owned:item","expected":resources["resources"][0]["alfredpetObservation"]}),"allow",||()).await.unwrap();
    assert_eq!(out["contents"][0]["text"], "Señor 🐾");
    let mut input =
        json!({"name":"greet","body":{},"expected":prompts["prompts"][0]["alfredpetObservation"]});
    assert_eq!(
        f.core
            .execute("mcp.get_prompt", f.input(input.clone()))
            .await
            .unwrap_err(),
        "MCP_PROMPT_ARGUMENT_REQUIRED"
    );
    input["body"] = json!({"who":"Señor"});
    assert!(f
        .approve("mcp.get_prompt", input, "allow", || ())
        .await
        .is_ok());
    assert_eq!(f.count("prompts/get"), 1);
    f.clean().await;
}
#[tokio::test]
async fn expired_or_cross_subject_observation_and_bad_arguments_deny_before_io() {
    let f = Fixture::new().await;
    let mut input = f.observed().await;
    let count = f.count("server/discover");
    input["body"] = json!({"bad":"extra"});
    assert_eq!(
        f.core
            .execute("mcp.call", f.input(input.clone()))
            .await
            .unwrap_err(),
        "MCP_TOOL_INPUT_SCHEMA"
    );
    input["body"] = json!({"value":"ok"});
    for o in f.core.mcp_observations.lock().unwrap().values_mut() {
        o.expires = Instant::now() - Duration::from_secs(1);
    }
    assert_eq!(
        f.core
            .execute("mcp.call", f.input(input))
            .await
            .unwrap_err(),
        "MCP_OBSERVATION_CHANGED_REVIEW_REQUIRED"
    );
    assert_eq!(f.count("server/discover"), count);
    f.clean().await;
}
#[tokio::test]
async fn native_publication_mismatch_retains_blocked_intent_and_id() {
    let f = Fixture::new().await;
    *f.core.mcp_secret_fixture.lock().unwrap() = Some(crate::mcp_native::Fixture {
        readback: Some("owned_old_scalar".into()),
        writes: 0,
    });
    let mut raw = f.raw();
    raw["auth"] = json!({"type":"bearer","secret_ref":"main"});
    assert_eq!(
        f.save(&raw, Some(r#"{"main":"owned_fictional_token"}"#))
            .await
            .unwrap_err(),
        format!("MCP_CONNECTION_CREDENTIAL_PENDING:{}", f.id)
    );
    assert_eq!(
        mcp_save::binding(&f.core, &f.id, &profile::config(&raw).unwrap())
            .unwrap()
            .stage,
        credentials::Stage::Pending
    );
    assert_eq!(
        f.save(&raw, None).await.unwrap_err(),
        "MCP_CREDENTIAL_REENTRY_REQUIRED"
    );
    assert_eq!(f.count("server/discover"), 0);
    f.clean().await;
}
#[tokio::test]
async fn native_owned_envelope_auth_redaction_rotation_and_ready_cas_failure() {
    use windows::{
        core::PCWSTR,
        Win32::Security::Credentials::{
            CredDeleteW, CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC,
        },
    };
    let f = Fixture::new().await;
    let alias: Vec<u16> = format!("alfredpet.{}", credentials::alias(&f.id).unwrap())
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut ptr = std::ptr::null_mut::<CREDENTIALW>();
    let absent = unsafe { CredReadW(PCWSTR(alias.as_ptr()), CRED_TYPE_GENERIC, None, &mut ptr) };
    if absent.is_ok() {
        unsafe {
            CredFree(ptr.cast());
        }
        panic!("Owned MCP alias already exists");
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
    raw["headers"] = json!({"x-api-key":{"secret_ref":"key"}});
    let secrets =
        json!({"main":"owned_fictional_mcp_token","key":"owned_fictional_mcp_key"}).to_string();
    f.save(&raw, Some(&secrets)).await.unwrap();
    let input = f.observed().await;
    f.m.lock().unwrap().output["content"][0]["text"] =
        json!("owned_fictional_mcp_key and owned_fictional_mcp_token");
    let out = f.approve("mcp.call", input, "allow", || ()).await.unwrap();
    assert!(!out.to_string().contains("owned_fictional_mcp"));
    let seen = f.m.lock().unwrap().seen.last().unwrap().clone();
    assert_eq!(
        seen["headers"]["authorization"],
        "Bearer owned_fictional_mcp_token"
    );
    assert_eq!(seen["headers"]["x-api-key"], "owned_fictional_mcp_key");
    let input = f.observed().await;
    let prepared = prepare(&f.core, "mcp.call", &f.input(input)).await.unwrap();
    f.save(&raw, Some(&secrets)).await.unwrap();
    assert_eq!(
        execute(prepared).await.unwrap_err(),
        "MCP_CONNECTION_CHANGED"
    );
    let mut retarget = raw.clone();
    retarget["endpoint"] = json!("/other");
    assert_eq!(
        f.save(&retarget, None).await.unwrap_err(),
        "MCP_CREDENTIAL_REQUIRED_FOR_TARGET_CHANGE"
    );
    f.m.lock().unwrap().definition["description"] = json!("owned_fictional_mcp_token");
    assert_eq!(
        f.approve("mcp.tools", json!({}), "allow", || ())
            .await
            .unwrap_err(),
        "MCP_CREDENTIAL_CONTENT_DENIED"
    );
    let output = std::process::Command::new("where.exe")
        .arg("node.exe")
        .output()
        .unwrap();
    assert!(output.status.success());
    let command = PathBuf::from(
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .trim(),
    )
    .canonicalize()
    .unwrap();
    let script = f.root.join("private-env.mjs");
    std::fs::write(&script,"import { createInterface } from 'node:readline';\ncreateInterface({input:process.stdin}).on('line',line=>{const r=JSON.parse(line);if(r.id===undefined)return;const result=r.method==='server/discover'?{supportedVersions:['2026-07-28'],capabilities:{tools:{}}}:r.method==='tools/list'?{tools:[{name:'write',inputSchema:{type:'object'}}]}:{content:[{type:'text',text:process.env.OWNED_KEY}],structuredContent:{inheritedHome:process.env.HOME!==undefined}};result.resultType='complete';process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:r.id,result})+'\\n');});\n").unwrap();
    let stdio = json!({"profile_version":1,"transport":"stdio","command":command,"args":[script],"auth":{"type":"none"},"env":{"OWNED_KEY":"main"},"permissions":{"tools":{"write":true}}});
    f.save(&stdio, Some(r#"{"main":"owned_fictional_env_value"}"#))
        .await
        .unwrap();
    let input = f.observed().await;
    let out = f.approve("mcp.call", input, "allow", || ()).await.unwrap();
    assert_eq!(out["content"][0]["text"], "[REDACTED]");
    assert_eq!(out["structuredContent"]["inheritedHome"], false);
    f.save(&raw, Some(&secrets)).await.unwrap();
    let key = mcp_save::binding_key(&f.id);
    f.core.store.execute(&format!("CREATE TRIGGER owned_mcp_ready BEFORE UPDATE ON settings WHEN NEW.key='{key}' AND json_extract(NEW.json_value,'$.stage')='ready' BEGIN SELECT RAISE(ABORT,'owned ready failure'); END"),&[]).unwrap();
    assert_eq!(
        f.save(&raw, Some(&secrets)).await.unwrap_err(),
        format!("MCP_CONNECTION_CREDENTIAL_PENDING:{}", f.id)
    );
    assert_eq!(
        mcp_save::binding(&f.core, &f.id, &profile::config(&raw).unwrap())
            .unwrap()
            .stage,
        credentials::Stage::Pending
    );
    let public = f
        .core
        .store
        .query("SELECT config_json FROM connections", &[])
        .unwrap();
    let audit = f
        .core
        .store
        .query(
            "SELECT input_summary,output_summary,undo_token FROM activity_events",
            &[],
        )
        .unwrap();
    assert!(!public.to_string().contains("owned_fictional_mcp"));
    assert!(!audit.to_string().contains("owned_fictional_mcp"));
    drop(cleanup);
    f.clean().await;
}
#[tokio::test]
async fn stdio_program_and_script_are_pinned_until_dispatch_or_revocation() {
    let f = Fixture::new().await;
    let output = std::process::Command::new("where.exe")
        .arg("node.exe")
        .output()
        .unwrap();
    assert!(output.status.success());
    let command = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .trim()
        .to_string();
    // nvm's PATH entry is a junction. The operator profile pins the actual
    // executable, and rejects unresolved link/reparse ancestors.
    let command = PathBuf::from(command).canonicalize().unwrap();
    let script = f.root.join("owned-server.mjs");
    std::fs::write(&script,"import { createInterface } from 'node:readline';\nconst io = createInterface({ input: process.stdin });\nio.on('line', line => { const r=JSON.parse(line); if (!r.id && r.id!==0) return; const result=r.method==='server/discover'?{supportedVersions:['2026-07-28'],capabilities:{tools:{}}}:r.method==='tools/list'?{tools:[{name:'write',inputSchema:{type:'object',properties:{value:{type:'string'}},required:['value'],additionalProperties:false}}]}:{content:[{type:'text',text:'ok'}]}; result.resultType='complete'; process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:r.id,result})+'\\n'); });\n").unwrap();
    let raw = json!({"profile_version":1,"transport":"stdio","command":command,"args":[script],"auth":{"type":"none"},"permissions":{"tools":{"write":true}}});
    f.save(&raw, None).await.unwrap();
    let input = f.observed().await;
    let prepared = prepare(&f.core, "mcp.call", &f.input(input)).await.unwrap();
    assert!(std::fs::write(&script, "changed").is_err());
    revoke(&f.core).unwrap();
    assert_eq!(
        execute(prepared).await.unwrap_err(),
        "MCP_CONNECTION_CHANGED"
    );
    assert!(std::fs::write(&script, "changed").is_ok());
    f.clean().await;
}
#[tokio::test]
async fn profile_compare_and_swap_and_intent_failure_precede_native_write() {
    let f = Fixture::new().await;
    let old = f.raw();
    let generation = f.core.store.setting(&mcp_save::generation_key(&f.id));
    let mut changed = old.clone();
    changed["permissions"]["tools"]["write"] = json!(false);
    f.save(&changed, None).await.unwrap();
    assert_eq!(
        integrations::save(
            &f.core,
            &json!({"id":f.id,"module_id":"mcp","name":"Stale","config":old,"expected_profile":old})
        )
        .await
        .unwrap_err(),
        "MCP_PROFILE_CHANGED"
    );
    assert_eq!(f.raw(), changed);
    *f.core.mcp_secret_fixture.lock().unwrap() = Some(crate::mcp_native::Fixture {
        readback: None,
        writes: 0,
    });
    let before = f.core.store.setting(&mcp_save::generation_key(&f.id));
    assert_ne!(before, generation);
    f.core.store.execute("CREATE TRIGGER owned_mcp_intent BEFORE UPDATE ON connections BEGIN SELECT RAISE(ABORT,'owned intent failure'); END",&[]).unwrap();
    changed["auth"] = json!({"type":"bearer","secret_ref":"main"});
    assert_eq!(
        f.save(&changed, Some(r#"{"main":"owned_fictional"}"#))
            .await
            .unwrap_err(),
        "MCP_PROFILE_INTENT_TRANSACTION"
    );
    assert_eq!(
        f.core.store.setting(&mcp_save::generation_key(&f.id)),
        before
    );
    assert_eq!(
        f.core
            .mcp_secret_fixture
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .writes,
        0
    );
    assert_eq!(f.count("server/discover"), 0);
    f.clean().await;
}

#[cfg(windows)]
#[tokio::test]
async fn profile_changed_while_worker_pins_cannot_reach_review_or_dispatch() {
    let f = Fixture::new().await;
    let script = f.root.join("never-executed-pin.mjs");
    std::fs::write(&script, "/* owned fixture: never executed */").unwrap();
    let mut raw = json!({"profile_version":1,"transport":"stdio","protocol":"modern","command":sdk::node(),"args":[script],"auth":{"type":"none"},"permissions":{"tools":{"write":true}}});
    f.save(&raw, None).await.unwrap();
    let scope = Scope::capture(&f.core, &f.id).unwrap();
    raw["permissions"]["tools"]["write"] = json!(false);
    f.save(&raw, None).await.unwrap();
    let scope = tokio::task::spawn_blocking(move || scope.pin())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scope.check().unwrap_err(), "MCP_CONNECTION_CHANGED");
    assert!(f.core.pending.lock().unwrap().is_empty());
    assert!(f.m.lock().unwrap().seen.is_empty());
    drop(scope);
    f.clean().await;
}
