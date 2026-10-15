use super::*;
use axum::{
    body::Body,
    extract::{Request, State},
    http::Response,
    Router,
};
use std::{path::PathBuf, sync::Mutex};
const TOKEN: &str = "owned_fictional_rest_token";
fn empty() -> Value {
    json!({"type":"object","properties":{},"additionalProperties":false})
}
fn operation(method: &str, path: &str) -> Value {
    json!({"method":method,"path":path,"path_schema":empty(),"query_schema":empty(),"body_schema":null,"response_schema":null,"response":"json","success_statuses":[200],"risk":"free"})
}
fn openapi() -> Value {
    json!({"openapi":"3.2.1","info":{"title":"Owned document","version":"1"},"servers":[{"url":"/api/"}],"paths":{"/health":{"get":{"operationId":"health","responses":{"200":{"content":{"application/json":{"schema":{"type":"object"}}}}}}},"/write":{"post":{"operationId":"write","responses":{"204":{}}}}}})
}
struct Mock {
    seen: Vec<Value>,
    status: u16,
    media: String,
    body: String,
    delay: bool,
    started: Arc<tokio::sync::Notify>,
    doc: Value,
}
async fn handler(State(m): State<Arc<Mutex<Mock>>>, r: Request) -> Response<Body> {
    let method = r.method().to_string();
    let uri = r.uri().to_string();
    let headers = r
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let body = axum::body::to_bytes(r.into_body(), 65537).await.unwrap();
    let (status, media, out, delay, notify) = {
        let mut s = m.lock().unwrap();
        s.seen.push(json!({"method":method,"uri":uri,"headers":headers,"body":String::from_utf8(body.to_vec()).unwrap()}));
        (
            s.status,
            s.media.clone(),
            if uri == "/api/openapi.json" {
                s.doc.to_string()
            } else {
                s.body.clone()
            },
            s.delay,
            s.started.clone(),
        )
    };
    if delay {
        notify.notify_one();
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Response::builder()
        .status(status)
        .header("content-type", media)
        // Keep a margin across epoch-second boundaries and loaded CI hosts.
        .header("retry-after", "60")
        .header("location", "http://127.0.0.1:1/foreign")
        .body(Body::from(out))
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
    async fn new(read_only: bool) -> Self {
        let m = Arc::new(Mutex::new(Mock {
            seen: vec![],
            status: 200,
            media: "application/json".into(),
            body: json!({"value":"Señor 🐾 provider private content"}).to_string(),
            delay: false,
            started: Arc::new(tokio::sync::Notify::new()),
            doc: openapi(),
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = Router::new().fallback(handler).with_state(m.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let root = std::env::temp_dir().join(format!("ap-rest-{}", uuid::Uuid::new_v4()));
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
                "UPDATE module_configs SET enabled=1 WHERE module_id='rest'",
                &[],
            )
            .unwrap();
        *core.rest_fixture.lock().unwrap() = Some(TestTransport {
            headless_permission: true,
            http2_prior_knowledge: false,
        });
        let raw = json!({"profile_version":1,"base_url":format!("http://{addr}/api/"),"allowed_hosts":["127.0.0.1"],"auth":{"type":"none"},"headers":{},"read_only":read_only,"operations":{"read":operation("GET","/health"),"write":operation("POST","/write")},"openapi_path":"/openapi.json"});
        let saved = integrations::save(
            &core,
            &json!({"module_id":"rest","name":"Owned REST fixture","config":raw}),
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
        let mut out = json!({"connection":self.id});
        for (k, v) in v.as_object().unwrap() {
            out[k] = v.clone();
        }
        out
    }
    async fn run(&self, tool: &str, v: Value) -> Result<Value, String> {
        self.core.execute(tool, self.input(v)).await
    }
    async fn observed(&self, operation: &str) -> Value {
        let mut v = json!({"action":"inspect","request":{"operation":operation}});
        let obs = self.run("rest.request", v.clone()).await.unwrap();
        v["action"] = json!("call");
        v["expected"] = obs["expected"].clone();
        v
    }
    fn raw(&self) -> Value {
        record(&self.core, &self.id).unwrap().0
    }
    async fn save(&self, raw: &Value, secret: Option<&str>) -> Result<Value, String> {
        integrations::save(&self.core,&json!({"id":self.id,"module_id":"rest","name":"Owned edited","config":raw,"secret":secret})).await
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
        let tx = tokio::time::timeout(Duration::from_secs(8), async {
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
        tx.send(answer.into()).unwrap();
        task.await.unwrap()
    }
    fn count(&self) -> usize {
        self.m.lock().unwrap().seen.len()
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
            .starts_with("ap-rest-"));
        assert_eq!(
            crate::asset_files::identity(&crate::asset_files::directory(&root).unwrap()).unwrap(),
            self.identity
        );
        drop(self.core);
        std::fs::remove_dir_all(root).unwrap();
    }
}
#[tokio::test]
async fn rest_actor_inspect_is_pure_and_call_preserves_unicode_prefix_and_query() {
    let mut f = Fixture::new(false).await;
    let marker = f.root.join("native-called");
    let script = f.root.join("native-marker.ps1");
    std::fs::write(&script,"$null=[Console]::In.ReadToEnd()\n[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'native-called'),'called')\n[Console]::Out.WriteLine('{\"ok\":false,\"error\":\"should not call\"}')\n").unwrap();
    Arc::get_mut(&mut f.core).unwrap().native = script;
    let mut raw = f.raw();
    raw["operations"]["read"]["path"] = json!("/item/{id}");
    raw["operations"]["read"]["path_schema"] = json!({"type":"object","properties":{"id":{"type":"string"}},"required":["id"],"additionalProperties":false});
    raw["operations"]["read"]["query_schema"] =
        json!({"type":"object","properties":{"q":{"type":"string"}},"additionalProperties":false});
    f.save(&raw, None).await.unwrap();
    let mut v = json!({"action":"inspect","request":{"operation":"read","path":{"id":"Señor 🐾"},"query":{"q":"x/y &"}}});
    let obs = f.run("rest.request", v.clone()).await.unwrap();
    assert_eq!(f.count(), 0);
    assert!(!marker.exists());
    v["action"] = json!("call");
    v["expected"] = obs["expected"].clone();
    let result = f.run("rest.request", v).await.unwrap();
    assert_eq!(result["data"]["value"], "Señor 🐾 provider private content");
    assert_eq!(result["untrusted"], true);
    let seen = f.m.lock().unwrap().seen[0].clone();
    assert_eq!(seen["method"], "GET");
    assert_eq!(
        seen["uri"],
        "/api/item/Se%C3%B1or%20%F0%9F%90%BE?q=x%2Fy+%26"
    );
    assert_eq!(seen["body"], "");
    assert!(!marker.exists());
    let audit = f
        .core
        .store
        .query(
            "SELECT input_summary,output_summary FROM activity_events",
            &[],
        )
        .unwrap();
    assert!(!audit.to_string().contains("provider private content"));
    f.clean().await;
}
#[tokio::test]
async fn rest_actor_closed_input_readonly_expected_and_privacy_deny_before_io() {
    let f = Fixture::new(true).await;
    for input in [
        json!({"action":"call","request":{"operation":"read"}}),
        json!({"action":"inspect","request":{"operation":"unknown"}}),
        json!({"action":"inspect","request":{"operation":"write"}}),
        json!({"action":"inspect","request":{"operation":"read","method":"DELETE"}}),
        json!({"method":"GET","endpoint":"/health"}),
    ] {
        assert!(f.run("rest.request", input).await.is_err());
    }
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(
        f.run(
            "rest.request",
            json!({"action":"inspect","request":{"operation":"read"}})
        )
        .await
        .unwrap_err(),
        "PRIVACY_DENIED"
    );
    assert_eq!(f.count(), 0);
    f.clean().await;
}
#[tokio::test]
async fn rest_actor_mutation_uses_actual_permission_channel_and_mandatory_floor() {
    let f = Fixture::new(false).await;
    let v = f.observed("write").await;
    assert_eq!(
        f.approve("rest.request", v.clone(), "deny", || ())
            .await
            .unwrap_err(),
        "PERMISSION_DENIED_OR_TIMEOUT"
    );
    assert_eq!(
        f.approve("rest.request", v.clone(), "session", || ())
            .await
            .unwrap_err(),
        "PERMISSION_DENIED_OR_TIMEOUT"
    );
    assert_eq!(f.count(), 0);
    let out = f.approve("rest.request", v, "allow", || ()).await.unwrap();
    assert_eq!(out["status"], 200);
    assert_eq!(f.count(), 1);
    let reqs = f
        .core
        .store
        .query("SELECT risk FROM permission_requests", &[])
        .unwrap();
    assert!(reqs
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["risk"] == "Mandatory"));
    f.clean().await;
}
#[tokio::test]
async fn rest_actor_revokes_profile_changed_while_permission_is_pending() {
    let f = Fixture::new(false).await;
    let v = f.observed("write").await;
    let e = f
        .approve("rest.request", v, "allow", || {
            f.core
                .store
                .set_setting(
                    &rest_save::generation_key(&f.id),
                    &json!(uuid::Uuid::new_v4().to_string()),
                )
                .unwrap();
        })
        .await
        .unwrap_err();
    assert_eq!(e, "REST_CONNECTION_CHANGED");
    assert_eq!(f.count(), 0);
    f.clean().await;
}
#[tokio::test]
async fn rest_actor_cancels_delayed_response_on_privacy_and_module_revocation() {
    for privacy in [true, false] {
        let f = Fixture::new(false).await;
        let input = f.observed("read").await;
        f.m.lock().unwrap().delay = true;
        let notify = f.m.lock().unwrap().started.clone();
        let core = f.core.clone();
        let input = f.input(input);
        let task = tokio::spawn(async move { core.execute("rest.request", input).await });
        tokio::time::timeout(Duration::from_secs(3), notify.notified())
            .await
            .unwrap();
        if privacy {
            f.core.store.set_setting("privacy", &json!(true)).unwrap();
        } else {
            f.core
                .store
                .execute(
                    "UPDATE module_configs SET enabled=0 WHERE module_id='rest'",
                    &[],
                )
                .unwrap();
        }
        let result = tokio::time::timeout(Duration::from_millis(700), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(
            result,
            if privacy {
                "PRIVACY_DENIED"
            } else {
                "MODULE_DISABLED: rest"
            }
        );
        assert_eq!(f.count(), 1);
        f.clean().await;
    }
}
#[tokio::test]
async fn rest_actor_status_media_schema_limits_and_rate_limit_have_no_retry() {
    let f = Fixture::new(false).await;
    let mut raw = f.raw();
    raw["operations"]["read"]["response_schema"] = json!({"type":"object","required":["value"],"properties":{"value":{"type":"string"}},"additionalProperties":false});
    f.save(&raw, None).await.unwrap();
    for (status, media, body, error) in [
        (302, "application/json", "{}", "REST_HTTP_STATUS_302"),
        (
            401,
            "application/json",
            "private error",
            "REST_HTTP_STATUS_401",
        ),
        (200, "text/html", "{}", "REST_RESPONSE_CONTENT_TYPE"),
        (
            200,
            "application/json",
            "private broken",
            "REST_RESPONSE_JSON",
        ),
        (201, "application/json", "{}", "REST_STATUS_UNEXPECTED"),
        (200, "application/json", "{}", "REST_RESPONSE_SCHEMA"),
    ] {
        {
            let mut m = f.m.lock().unwrap();
            m.status = status;
            m.media = media.into();
            m.body = body.into();
        }
        let v = f.observed("read").await;
        let n = f.count();
        assert_eq!(f.run("rest.request", v).await.unwrap_err(), error);
        assert_eq!(f.count(), n + 1);
    }
    {
        let mut m = f.m.lock().unwrap();
        m.status = 200;
        m.body = "x".repeat(2 * 1024 * 1024 + 1);
    }
    let v = f.observed("read").await;
    assert_eq!(
        f.run("rest.request", v).await.unwrap_err(),
        "REST_RESPONSE_LIMIT_OR_TRANSPORT"
    );
    f.m.lock().unwrap().status = 429;
    let v = f.observed("read").await;
    assert_eq!(
        f.run("rest.request", v.clone()).await.unwrap_err(),
        "REST_HTTP_STATUS_429"
    );
    let n = f.count();
    assert_eq!(
        f.run("rest.request", v).await.unwrap_err(),
        "REST_RATE_LIMITED_MANUAL_RETRY"
    );
    assert_eq!(f.count(), n);
    f.save(&raw, None).await.unwrap();
    assert_eq!(
        f.core.store.setting(&rest_save::cooldown_key(&f.id)),
        Value::Null
    );
    f.clean().await;
}
#[tokio::test]
async fn rest_actor_per_status_text_and_empty_responses_are_checked() {
    let f = Fixture::new(false).await;
    let mut raw = f.raw();
    raw["operations"]["read"]["success_statuses"] = json!([200, 204]);
    raw["operations"]["read"]["responses"] =
        json!({"200":{"response":"text","content_type":"text/plain"},"204":{"response":"empty"}});
    f.save(&raw, None).await.unwrap();
    {
        let mut m = f.m.lock().unwrap();
        m.media = "text/plain".into();
        m.body = "Señor plain".into();
    }
    assert_eq!(
        f.run("rest.request", f.observed("read").await)
            .await
            .unwrap()["data"],
        "Señor plain"
    );
    {
        let mut m = f.m.lock().unwrap();
        m.status = 204;
        m.body = String::new();
    }
    assert_eq!(
        f.run("rest.request", f.observed("read").await)
            .await
            .unwrap()["data"],
        Value::Null
    );
    f.clean().await;
}
#[tokio::test]
async fn rest_actor_local_openapi_requires_selected_mandatory_apply_without_enabling_writes() {
    let f = Fixture::new(false).await;
    let raw = f.raw();
    let preview = f
        .run(
            "rest.import_openapi",
            json!({"action":"preview","document":openapi()}),
        )
        .await
        .unwrap();
    assert_eq!(f.count(), 0);
    assert_eq!(f.raw(), raw);
    let input = json!({"action":"apply","document":openapi(),"expected":preview["expected"],"operations":["health","write"]});
    assert_eq!(
        f.approve("rest.import_openapi", input.clone(), "deny", || ())
            .await
            .unwrap_err(),
        "PERMISSION_DENIED_OR_TIMEOUT"
    );
    assert_eq!(f.raw(), raw);
    let result = f
        .approve("rest.import_openapi", input, "allow", || ())
        .await
        .unwrap();
    assert_eq!(result["read_only"], true);
    let cfg = profile::config(&f.raw()).unwrap();
    assert_eq!(cfg.operations.len(), 2);
    assert!(!cfg.operations.contains_key("read"));
    assert!(cfg.read_only);
    assert_eq!(cfg.operations["write"].risk, profile::Risk::Mandatory);
    assert_eq!(f.count(), 0);
    assert!(f
        .run(
            "rest.request",
            json!({"action":"inspect","request":{"operation":"write"}})
        )
        .await
        .is_err());
    f.clean().await;
}
#[tokio::test]
async fn rest_actor_remote_openapi_changed_after_permission_is_not_applied() {
    let f = Fixture::new(false).await;
    let raw = f.raw();
    let preview = f
        .run("rest.import_openapi", json!({"action":"preview"}))
        .await
        .unwrap();
    assert_eq!(f.count(), 1);
    let input = json!({"action":"apply","expected":preview["expected"],"operations":["health"]});
    let err = f
        .approve("rest.import_openapi", input, "allow", || {
            f.m.lock().unwrap().doc["info"]["version"] = json!("2");
        })
        .await
        .unwrap_err();
    assert_eq!(err, "REST_OPENAPI_OBSERVATION_CHANGED_REVIEW_REQUIRED");
    assert_eq!(f.count(), 3);
    assert_eq!(f.raw(), raw);
    f.clean().await;
}
#[tokio::test]
async fn rest_actor_import_rechecks_generation_in_local_transaction() {
    let f = Fixture::new(false).await;
    let raw = f.raw();
    let config = profile::config(&raw).unwrap();
    let expected = rest_save::Expected {
        raw: raw.clone(),
        generation: json!(uuid::Uuid::new_v4().to_string()),
        binding: json!(rest_save::binding(&f.core, &f.id, &config).unwrap()),
    };
    let lock = integrations::save_lock(&f.core).unwrap();
    assert_eq!(
        rest_save::save_connection(
            &f.core,
            &f.id,
            "Owned CAS",
            &raw,
            None,
            true,
            Some(&expected)
        )
        .await
        .unwrap_err(),
        "REST_PROFILE_CHANGED"
    );
    drop(lock);
    assert_eq!(f.raw(), raw);
    assert_eq!(f.count(), 0);
    f.clean().await;
}
#[tokio::test]
async fn rest_actor_intent_failure_precedes_native_and_rolls_back() {
    let mut f = Fixture::new(false).await;
    let before = f.raw();
    let generation = f.core.store.setting(&rest_save::generation_key(&f.id));
    let script = f.root.join("native-marker.ps1");
    std::fs::write(&script,"$null=[Console]::In.ReadToEnd()\n[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'native-called'),'called')\n[Console]::Out.WriteLine('{\"ok\":true,\"data\":{}}')\n").unwrap();
    Arc::get_mut(&mut f.core).unwrap().native = script;
    f.core.store.execute("CREATE TRIGGER owned_rest_block BEFORE UPDATE ON connections BEGIN SELECT RAISE(ABORT,'owned intent failure'); END",&[]).unwrap();
    let mut raw = before.clone();
    raw["auth"] = json!({"type":"bearer","secret_ref":"main"});
    assert_eq!(
        f.save(&raw, Some(&json!({"main":TOKEN}).to_string()))
            .await
            .unwrap_err(),
        "REST_PROFILE_INTENT_TRANSACTION"
    );
    assert_eq!(f.raw(), before);
    assert_eq!(
        f.core.store.setting(&rest_save::generation_key(&f.id)),
        generation
    );
    assert!(!f.root.join("native-called").exists());
    assert_eq!(f.count(), 0);
    f.clean().await;
}
#[tokio::test]
async fn rest_actor_readback_mismatch_retains_blocked_id_and_requires_reentry() {
    let mut f = Fixture::new(false).await;
    let script = f.root.join("native-mismatch.ps1");
    std::fs::write(&script,"$null=[Console]::In.ReadToEnd()\n[Console]::Out.WriteLine('{\"ok\":true,\"data\":{\"value\":\"owned_old_scalar\"}}')\n").unwrap();
    Arc::get_mut(&mut f.core).unwrap().native = script;
    let mut raw = f.raw();
    raw["auth"] = json!({"type":"bearer","secret_ref":"main"});
    assert_eq!(
        f.save(&raw, Some(&json!({"main":TOKEN}).to_string()))
            .await
            .unwrap_err(),
        format!("REST_CONNECTION_CREDENTIAL_PENDING:{}", f.id)
    );
    assert_eq!(
        rest_save::binding(&f.core, &f.id, &profile::config(&raw).unwrap())
            .unwrap()
            .stage,
        credentials::Stage::Pending
    );
    assert_eq!(
        f.run(
            "rest.request",
            json!({"action":"inspect","request":{"operation":"read"}})
        )
        .await
        .unwrap_err(),
        "REST_CREDENTIAL_REENTRY_REQUIRED"
    );
    assert_eq!(
        f.save(&raw, None).await.unwrap_err(),
        "REST_CREDENTIAL_REENTRY_REQUIRED"
    );
    assert_eq!(f.count(), 0);
    f.clean().await;
}
#[tokio::test]
async fn rest_actor_native_owned_secrets_auth_rotation_and_publication_failure() {
    use windows::{
        core::PCWSTR,
        Win32::Security::Credentials::{
            CredDeleteW, CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC,
        },
    };
    let f = Fixture::new(false).await;
    let name: Vec<u16> = format!("alfredpet.{}", credentials::alias(&f.id).unwrap())
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut ptr = std::ptr::null_mut::<CREDENTIALW>();
    let missing = unsafe { CredReadW(PCWSTR(name.as_ptr()), CRED_TYPE_GENERIC, None, &mut ptr) };
    if missing.is_ok() {
        unsafe {
            CredFree(ptr.cast());
        }
        panic!("Owned REST alias already exists");
    }
    assert_eq!(missing.unwrap_err().code().0 as u32, 0x80070490);
    struct Cleanup(Vec<u16>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = unsafe { CredDeleteW(PCWSTR(self.0.as_ptr()), CRED_TYPE_GENERIC, None) };
        }
    }
    let cleanup = Cleanup(name);
    let mut raw = f.raw();
    raw["auth"] = json!({"type":"bearer","secret_ref":"main"});
    raw["headers"] = json!({"x-api-key":{"secret_ref":"key"},"x-public":{"value":"owned-public"}});
    let secret = json!({"main":TOKEN,"key":"owned_fictional_header_key"}).to_string();
    f.save(&raw, Some(&secret)).await.unwrap();
    f.m.lock().unwrap().body =
        json!({"value":TOKEN,"other":"owned_fictional_header_key"}).to_string();
    let out = f
        .run("rest.request", f.observed("read").await)
        .await
        .unwrap();
    assert!(!out.to_string().contains(TOKEN));
    assert!(!out.to_string().contains("owned_fictional_header_key"));
    assert_eq!(
        f.m.lock().unwrap().seen.last().unwrap()["headers"]["authorization"],
        format!("Bearer {TOKEN}")
    );
    {
        assert!(format!(
            "{:?}",
            reqwest::header::HeaderValue::from_str(TOKEN).unwrap()
        )
        .contains(TOKEN));
        let scope = Scope::new(&f.core, &f.id).unwrap();
        let session = Session::new(&scope).await.unwrap();
        assert!(!format!("{:?}", session.headers).contains(TOKEN));
        assert!(!format!("{:?}", session.headers).contains("owned_fictional_header_key"));
    }
    let bytes = rest_save::native_read(&f.core, &f.id).await.unwrap();
    f.save(&raw, None).await.unwrap();
    assert_eq!(rest_save::native_read(&f.core, &f.id).await.unwrap(), bytes);
    let prepared = prepare(&f.core, "rest.request", &f.input(f.observed("read").await))
        .await
        .unwrap();
    f.save(&raw, Some(&secret)).await.unwrap();
    assert_eq!(
        execute(prepared).await.unwrap_err(),
        "REST_CONNECTION_CHANGED"
    );
    raw["auth"] = json!({"type":"basic","username":"fixture","secret_ref":"main"});
    assert_eq!(
        f.save(&raw, None).await.unwrap_err(),
        "REST_CREDENTIAL_REQUIRED_FOR_TARGET_CHANGE"
    );
    f.save(&raw, Some(&secret)).await.unwrap();
    f.run("rest.request", f.observed("read").await)
        .await
        .unwrap();
    assert!(
        f.m.lock().unwrap().seen.last().unwrap()["headers"]["authorization"]
            .as_str()
            .unwrap()
            .starts_with("Basic ")
    );
    let basic = f.m.lock().unwrap().seen.last().unwrap()["headers"]["authorization"]
        .as_str()
        .unwrap()
        .to_string();
    f.m.lock().unwrap().body = json!({"value":basic}).to_string();
    assert!(!f
        .run("rest.request", f.observed("read").await)
        .await
        .unwrap()
        .to_string()
        .contains(&basic));
    raw["auth"] = json!({"type":"header","header":"x-access","secret_ref":"main"});
    f.save(&raw, Some(&secret)).await.unwrap();
    f.run("rest.request", f.observed("read").await)
        .await
        .unwrap();
    assert_eq!(
        f.m.lock().unwrap().seen.last().unwrap()["headers"]["x-access"],
        TOKEN
    );
    f.m.lock().unwrap().doc["info"]["title"] = json!(TOKEN);
    assert_eq!(
        f.run("rest.import_openapi", json!({"action":"preview"}))
            .await
            .unwrap_err(),
        "REST_OPENAPI_CREDENTIAL_CONTENT_DENIED"
    );
    f.m.lock().unwrap().doc = openapi();
    let key = rest_save::binding_key(&f.id);
    f.core.store.execute(&format!("CREATE TRIGGER owned_rest_ready BEFORE UPDATE ON settings WHEN NEW.key='{key}' AND json_extract(NEW.json_value,'$.stage')='ready' BEGIN SELECT RAISE(ABORT,'owned ready failure'); END"),&[]).unwrap();
    assert_eq!(
        f.save(&raw, Some(&secret)).await.unwrap_err(),
        format!("REST_CONNECTION_CREDENTIAL_PENDING:{}", f.id)
    );
    assert_eq!(
        rest_save::binding(&f.core, &f.id, &profile::config(&raw).unwrap())
            .unwrap()
            .stage,
        credentials::Stage::Pending
    );
    assert!(f.observed_result().await.is_err());
    f.core
        .store
        .execute("DROP TRIGGER owned_rest_ready", &[])
        .unwrap();
    f.save(&raw, Some(&secret)).await.unwrap();
    crate::process::native(
        &f.core.native,
        "secret.set",
        &json!({"alias":credentials::alias(&f.id).unwrap(),"value":"owned_legacy_scalar"}),
    )
    .await
    .unwrap();
    let n = f.count();
    assert_eq!(
        f.run("rest.request", f.observed("read").await)
            .await
            .unwrap_err(),
        "REST_PRIVATE_ENVELOPE_REENTRY_REQUIRED"
    );
    assert_eq!(f.count(), n);
    let rows = f
        .core
        .store
        .query("SELECT config_json FROM connections", &[])
        .unwrap();
    assert!(!rows.to_string().contains(TOKEN));
    let audit = f
        .core
        .store
        .query(
            "SELECT input_summary,output_summary FROM activity_events",
            &[],
        )
        .unwrap();
    assert!(!audit.to_string().contains(TOKEN));
    drop(cleanup);
    f.clean().await;
}
impl Fixture {
    async fn observed_result(&self) -> Result<Value, String> {
        self.run(
            "rest.request",
            json!({"action":"inspect","request":{"operation":"read"}}),
        )
        .await
    }
}

#[tokio::test]
async fn rest_actor_json_payload_media_schema_and_uncertain_write_never_replays() {
    let f = Fixture::new(false).await;
    let mut raw = f.raw();
    raw["operations"]["write"]["body_schema"] = json!({"type":"object","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false});
    raw["operations"]["write"]["request_content_type"] = json!("application/vnd.owned+json");
    raw["operations"]["write"]["response_schema"] = json!({"type":"object","properties":{"accepted":{"type":"boolean"}},"required":["accepted"],"additionalProperties":false});
    f.save(&raw, None).await.unwrap();
    assert!(f
        .run(
            "rest.request",
            json!({"action":"inspect","request":{"operation":"write","body":{}}})
        )
        .await
        .is_err());
    assert_eq!(f.count(), 0);
    let mut input = json!({"action":"inspect","request":{"operation":"write","body":{"value":"Señor 🐾 request"}}});
    let observation = f.run("rest.request", input.clone()).await.unwrap();
    input["action"] = json!("call");
    input["expected"] = observation["expected"].clone();
    f.m.lock().unwrap().body = json!({"accepted":true}).to_string();
    assert_eq!(
        f.approve("rest.request", input.clone(), "allow", || ())
            .await
            .unwrap()["data"]["accepted"],
        true
    );
    let sent = f.m.lock().unwrap().seen[0].clone();
    assert_eq!(
        serde_json::from_str::<Value>(sent["body"].as_str().unwrap()).unwrap(),
        input["request"]["body"]
    );
    assert_eq!(
        sent["headers"]["content-type"],
        "application/vnd.owned+json"
    );
    f.m.lock().unwrap().body = "broken private response".into();
    assert_eq!(
        f.approve("rest.request", input.clone(), "allow", || ())
            .await
            .unwrap_err(),
        "REST_ACTION_OUTCOME_UNCERTAIN: REST_RESPONSE_JSON"
    );
    assert_eq!(f.count(), 2);
    f.m.lock().unwrap().status = 500;
    assert_eq!(
        f.approve("rest.request", input.clone(), "allow", || ())
            .await
            .unwrap_err(),
        "REST_ACTION_OUTCOME_UNCERTAIN: REST_HTTP_STATUS_500"
    );
    assert_eq!(f.count(), 3);
    f.m.lock().unwrap().status = 429;
    assert_eq!(
        f.approve("rest.request", input.clone(), "allow", || ())
            .await
            .unwrap_err(),
        "REST_ACTION_OUTCOME_UNCERTAIN: REST_HTTP_STATUS_429"
    );
    let n = f.count();
    assert_eq!(
        f.approve("rest.request", input.clone(), "allow", || ())
            .await
            .unwrap_err(),
        "REST_RATE_LIMITED_MANUAL_RETRY"
    );
    assert_eq!(f.count(), n);
    f.save(&raw, None).await.unwrap();
    let observation = f
        .run(
            "rest.request",
            json!({"action":"inspect","request":input["request"]}),
        )
        .await
        .unwrap();
    input["expected"] = observation["expected"].clone();
    {
        let mut m = f.m.lock().unwrap();
        m.status = 200;
        m.body = json!({"accepted":true}).to_string();
        m.delay = true;
    }
    let notify = f.m.lock().unwrap().started.clone();
    let core = f.core.clone();
    let revoke = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(3), notify.notified())
            .await
            .unwrap();
        core.store.set_setting("privacy", &json!(true)).unwrap();
    });
    assert_eq!(
        f.approve("rest.request", input, "allow", || ())
            .await
            .unwrap_err(),
        "REST_ACTION_OUTCOME_UNCERTAIN: PRIVACY_DENIED"
    );
    revoke.await.unwrap();
    assert_eq!(f.count(), n + 1);
    let audit = f
        .core
        .store
        .query(
            "SELECT input_summary,output_summary FROM activity_events",
            &[],
        )
        .unwrap();
    assert!(!audit.to_string().contains("Señor"));
    assert!(!audit.to_string().contains("broken private"));
    f.clean().await;
}

#[tokio::test]
async fn rest_actor_http2_protocol_nack_has_one_dispatch_with_explicit_never_policy() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mut f = Fixture::new(false).await;
    f.server.abort();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = seen.clone();
    f.server = tokio::spawn(async move {
        let mut children = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted=listener.accept()=> {
                    let Ok((stream,_))=accepted else {break};let counter=counter.clone();
                    children.spawn(async move {
                        let mut conn=h2::server::handshake(stream).await.unwrap();
                        while let Some(Ok((request,mut respond)))=conn.accept().await {
                            assert_eq!(request.method(),"POST");assert_eq!(request.uri().path(),"/api/write");
                            counter.fetch_add(1,Ordering::SeqCst);
                            respond.send_reset(h2::Reason::REFUSED_STREAM);
                        }
                    });
                },
                _=children.join_next(), if !children.is_empty()=>(),
            }
        }
    });
    // Positive control proves that this exact installed reqwest/protocol fault
    // can replay a cloneable POST; the production actor must override it.
    let control = reqwest::Client::builder()
        .no_proxy()
        .http2_prior_knowledge()
        .timeout(Duration::from_secs(4))
        .build()
        .unwrap();
    assert!(control
        .post(format!("http://{addr}/api/write"))
        .body("owned control payload")
        .send()
        .await
        .is_err());
    let default_count = seen.load(Ordering::SeqCst);
    assert!(
        default_count >= 2,
        "positive control did not provoke a retry: {default_count}"
    );
    drop(control);
    f.core
        .rest_fixture
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .http2_prior_knowledge = true;
    let mut raw = f.raw();
    raw["base_url"] = json!(format!("http://{addr}/api/"));
    f.save(&raw, None).await.unwrap();
    let input = f.observed("write").await;
    assert_eq!(
        f.approve("rest.request", input, "allow", || ())
            .await
            .unwrap_err(),
        "REST_ACTION_OUTCOME_UNCERTAIN: REST_TRANSPORT_ERROR"
    );
    assert_eq!(seen.load(Ordering::SeqCst) - default_count, 1);
    f.clean().await;
}
