use super::*;
use crate::storage;
use axum::{
    body::Body,
    extract::{Request, State},
    http::Response,
    Router,
};
use std::{os::windows::fs::MetadataExt, path::PathBuf, sync::Mutex};
const PAGE: &str = "11111111-1111-4111-8111-111111111111";
const DB: &str = "22222222-2222-4222-8222-222222222222";
const SOURCE: &str = "33333333-3333-4333-8333-333333333333";
const FOREIGN: &str = "44444444-4444-4444-8444-444444444444";
const CREATED: &str = "55555555-5555-4555-8555-555555555555";
fn page() -> Value {
    json!({"object":"page","id":PAGE,"parent":{"type":"workspace","workspace":true},"last_edited_time":"2026-10-04T00:00:00Z","properties":{"title":{"type":"title","title":[{"type":"text","text":{"content":"Señor 🐾 owned private title"}}]}},"archived":false,"in_trash":false})
}
fn source() -> Value {
    json!({"object":"data_source","id":SOURCE,"parent":{"type":"database_id","database_id":DB},"last_edited_time":"2026-10-04T00:00:00Z","properties":{"Name":{"id":"title","type":"title","title":{}}}})
}
fn block() -> Value {
    json!({"object":"block","id":CREATED,"parent":{"type":"page_id","page_id":PAGE},"type":"paragraph","has_children":true,"paragraph":{"rich_text":[{"text":{"content":"Owned private block"}}]}})
}
fn list_body(rows: Value) -> Value {
    json!({"object":"list","results":rows,"has_more":false,"next_cursor":null})
}
struct Mock {
    page: Value,
    source: Value,
    seen: Vec<Value>,
    error: Option<(u16, String)>,
    retry: Option<String>,
    redirect: Option<String>,
    malformed: bool,
    list_override: Option<Value>,
    write_error: bool,
    delay: bool,
    delay_write: bool,
    started: Arc<tokio::sync::Notify>,
}
async fn handler(State(state): State<Arc<Mutex<Mock>>>, request: Request) -> Response<Body> {
    let method = request.method().to_string();
    let path = request.uri().path().to_string();
    let uri = request.uri().to_string();
    let write = method == "PATCH" || (method == "POST" && path == "/v1/pages");
    let version = request
        .headers()
        .get("notion-version")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let auth = request
        .headers()
        .get("authorization")
        .is_some_and(|v| v == "Bearer owned_fictional_notion_token");
    let bytes = axum::body::to_bytes(request.into_body(), REQUEST_LIMIT + 1)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    let (delay, notify) = {
        let mut m = state.lock().unwrap();
        m.seen.push(json!({"method":method,"path":path,"uri":uri,"body":body,"version":version,"fictionalAuthMatched":auth}));
        (m.delay || (m.delay_write && write), m.started.clone())
    };
    if delay {
        notify.notify_one();
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let mut m = state.lock().unwrap();
    if let Some(redirect) = &m.redirect {
        return Response::builder()
            .status(302)
            .header("location", redirect)
            .body(Body::from("{}"))
            .unwrap();
    }
    if let Some((status, code)) = &m.error {
        if !m.write_error || write {
            let mut r = Response::builder()
                .status(*status)
                .header("content-type", "application/json");
            if let Some(retry) = &m.retry {
                r = r.header("retry-after", retry);
            }
            return r.body(Body::from(json!({"object":"error","status":status,"code":code,"message":"Owned private content must not escape","additional_data":{"committed_resource_id":CREATED}}).to_string())).unwrap();
        }
    }
    if m.malformed && write {
        return Response::builder().body(Body::from("{broken")).unwrap();
    }
    let mut data = match (method.as_str(), path.as_str()) {
        ("GET", p) if p == format!("/v1/pages/{PAGE}") => m.page.clone(),
        ("GET", p) if p == format!("/v1/data_sources/{SOURCE}") => m.source.clone(),
        ("GET", p) if p == format!("/v1/databases/{DB}") => {
            json!({"object":"database","id":DB,"data_sources":[{"id":SOURCE,"name":"Owned source"}]})
        }
        ("POST", "/v1/search") => {
            let mut foreign = m.page.clone();
            foreign["id"] = json!(FOREIGN);
            list_body(json!([m.page, foreign, m.source]))
        }
        ("GET", p) if p == format!("/v1/blocks/{PAGE}/children") => list_body(json!([block()])),
        ("POST", p) if p == format!("/v1/data_sources/{SOURCE}/query") => {
            let mut row = m.page.clone();
            row["parent"] = json!({"type":"data_source_id","data_source_id":SOURCE});
            list_body(json!([row]))
        }
        ("POST", "/v1/pages") => {
            let mut v = m.page.clone();
            v["id"] = json!(CREATED);
            v["parent"] = body["parent"].clone();
            v["properties"] = body["properties"].clone();
            v
        }
        ("PATCH", p) if p == format!("/v1/pages/{PAGE}") => {
            m.page["properties"] = body["properties"].clone();
            m.page["last_edited_time"] = json!("2026-10-04T00:01:00Z");
            m.page.clone()
        }
        ("PATCH", p) if p == format!("/v1/blocks/{PAGE}/children") => list_body(json!([block()])),
        _ => {
            return Response::builder()
                .status(404)
                .body(Body::from("{}"))
                .unwrap()
        }
    };
    if let Some(override_body) = &m.list_override {
        if data["object"] == "list" {
            data = override_body.clone();
        }
    }
    Response::builder()
        .header("content-type", "application/json")
        .body(Body::from(data.to_string()))
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
            page: page(),
            source: source(),
            seen: vec![],
            error: None,
            retry: None,
            redirect: None,
            malformed: false,
            list_override: None,
            write_error: false,
            delay: false,
            delay_write: false,
            started: Arc::new(tokio::sync::Notify::new()),
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new().fallback(handler).with_state(m.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let root = std::env::temp_dir().join(format!("ap-notion-{}", storage::id()));
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
                "UPDATE module_configs SET enabled=1 WHERE module_id='notion'",
                &[],
            )
            .unwrap();
        let saved=integrations::save(&core,&json!({"module_id":"notion","name":"Owned fixture","config":{"allowed_pages":[PAGE],"allowed_databases":[DB],"allowed_data_sources":[SOURCE],"read_only":read_only}})).await.unwrap();
        *core.notion_fixture.lock().unwrap() = Some(TestTransport {
            base: format!("http://{address}"),
            token: Some("owned_fictional_notion_token".into()),
        });
        Self {
            core,
            root,
            identity,
            id: saved["id"].as_str().unwrap().into(),
            m,
            server,
        }
    }
    fn input(&self, extra: Value) -> Value {
        let mut v = json!({"connection":self.id});
        for (k, value) in extra.as_object().unwrap() {
            v[k] = value.clone();
        }
        v
    }
    async fn read(&self) -> Value {
        self.core
            .execute("notion.read", self.input(json!({"page_id":PAGE})))
            .await
            .unwrap()
    }
    async fn prepare_write(&self, tool: &str, extra: Value) -> Prepared {
        prepare(&self.core, tool, &self.input(extra)).await.unwrap()
    }
    fn writes(&self) -> usize {
        self.m
            .lock()
            .unwrap()
            .seen
            .iter()
            .filter(|v| {
                v["method"] == "PATCH" || (v["method"] == "POST" && v["path"] == "/v1/pages")
            })
            .count()
    }
    async fn clean(self) {
        let Self {
            core,
            root,
            identity,
            server,
            ..
        } = self;
        server.abort();
        let _ = server.await;
        drop(core);
        assert_eq!(
            root.canonicalize().unwrap().parent(),
            Some(std::env::temp_dir().canonicalize().unwrap().as_path())
        );
        assert!(root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("ap-notion-"));
        assert_eq!(
            crate::asset_files::identity(&crate::asset_files::directory(&root).unwrap()).unwrap(),
            identity
        );
        let mut pending = vec![root.clone()];
        while let Some(p) = pending.pop() {
            assert_eq!(
                std::fs::symlink_metadata(&p).unwrap().file_attributes() & 0x400,
                0
            );
            if p.is_dir() {
                pending.extend(std::fs::read_dir(p).unwrap().map(|e| e.unwrap().path()));
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
fn update_body() -> Value {
    json!({"properties":{"title":{"title":[{"text":{"content":"Señor 🐾 reviewed change"}}]}}})
}
fn append_body() -> Value {
    json!({"children":[{"object":"block","type":"paragraph","paragraph":{"rich_text":[{"text":{"content":"Owned reviewed block"}}]}}]})
}
#[tokio::test]
async fn notion_closed_profiles_alias_scope_and_default_readonly() {
    let f = Fixture::new(true).await;
    for config in [
        json!({"base_url":"http://127.0.0.1:1"}),
        json!({"allowed_pages":[PAGE],"allowed_databases":[],"allowed_data_sources":[],"secret_ref":"foreign"}),
        json!({"allowed_pages":[],"allowed_databases":[],"allowed_data_sources":[]}),
        json!({"allowed_pages":["page1"],"allowed_databases":[],"allowed_data_sources":[]}),
    ] {
        assert!(validate_config(&config).is_err());
    }
    let observed = f.read().await;
    let count = f.m.lock().unwrap().seen.len();
    let e = prepare(
        &f.core,
        "notion.update",
        &f.input(json!({"page_id":PAGE,"body":update_body(),"expected":observed["observation"]})),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(e, "NOTION_CONNECTION_READ_ONLY");
    assert_eq!(
        prepare(&f.core, "notion.read", &f.input(json!({"page_id":FOREIGN})))
            .await
            .err()
            .unwrap(),
        "NOTION_RESOURCE_OUT_OF_SCOPE"
    );
    assert_eq!(f.m.lock().unwrap().seen.len(), count);
    f.core
        .store
        .execute(
            "UPDATE connections SET secret_ref='connection.foreign' WHERE id=?",
            &[json!(f.id)],
        )
        .unwrap();
    assert!(f.read_error().await.contains("ALIAS_CONFLICT"));
    f.clean().await;
}
impl Fixture {
    async fn read_error(&self) -> String {
        self.core
            .execute("notion.read", self.input(json!({"page_id":PAGE})))
            .await
            .unwrap_err()
    }
}
#[tokio::test]
async fn notion_owned_routes_unicode_manual_pages_and_observations() {
    let f = Fixture::new(false).await;
    let read = f.read().await;
    assert!(read["page"].to_string().contains("Señor 🐾"));
    assert_eq!(
        read["observation"]["fingerprint"].as_str().unwrap().len(),
        64
    );
    let search=f.core.execute("notion.search",f.input(json!({"query":"Owned","page_token":"opaque cursor/+== session metadata","page_size":50}))).await.unwrap();
    assert_eq!(search["results"].as_array().unwrap().len(), 2);
    assert_eq!(search["filtered_out_of_scope"], 1);
    let blocks = f
        .core
        .execute(
            "notion.blocks",
            f.input(json!({"page_id":PAGE,"page_token":"opaque cursor/+=="})),
        )
        .await
        .unwrap();
    assert_eq!(blocks["recursive_blocks_complete"], false);
    let db = f
        .core
        .execute("notion.read_database", f.input(json!({"database_id":DB})))
        .await
        .unwrap();
    assert_eq!(db["database"]["id"], DB);
    let query = f
        .core
        .execute(
            "notion.query_database",
            f.input(json!({"data_source_id":SOURCE,"body":{},"page_token":"opaque source cursor"})),
        )
        .await
        .unwrap();
    assert_eq!(
        query["observation"]["fingerprint"].as_str().unwrap().len(),
        64
    );
    {
        let m = f.m.lock().unwrap();
        assert!(m
            .seen
            .iter()
            .all(|r| r["version"] == VERSION && r["fictionalAuthMatched"] == true));
        assert!(m
            .seen
            .iter()
            .any(|r| r["body"]["start_cursor"] == "opaque cursor/+== session metadata"));
        assert!(m.seen.iter().any(|r| r["uri"]
            .as_str()
            .unwrap()
            .contains("start_cursor=opaque+cursor%2F%2B%3D%3D")));
    }
    f.clean().await;
}
#[tokio::test]
async fn notion_three_observed_writes_and_stale_changes() {
    let f = Fixture::new(false).await;
    let observed = f.read().await;
    let p = f
        .prepare_write(
            "notion.update",
            json!({"page_id":PAGE,"body":update_body(),"expected":observed["observation"]}),
        )
        .await;
    assert_eq!(p.risk, policy::Risk::Contextual);
    f.m.lock().unwrap().page["last_edited_time"] = json!("2026-10-04T00:00:01Z");
    assert_eq!(execute(p).await.unwrap_err(), "NOTION_STALE_OBSERVATION");
    assert_eq!(f.writes(), 0);
    let observed = f.read().await;
    let p = f
        .prepare_write(
            "notion.update",
            json!({"page_id":PAGE,"body":update_body(),"expected":observed["observation"]}),
        )
        .await;
    let v = execute(p).await.unwrap();
    assert!(v["page"].to_string().contains("reviewed change"));
    let observed = f.read().await;
    let p = f
        .prepare_write(
            "notion.append",
            json!({"page_id":PAGE,"body":append_body(),"expected":observed["observation"]}),
        )
        .await;
    assert_eq!(
        execute(p).await.unwrap()["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let observed = f.read().await;
    let p=f.prepare_write("notion.create",json!({"body":{"parent":{"page_id":PAGE},"properties":update_body()["properties"]},"expected":observed["observation"]})).await;
    let v = execute(p).await.unwrap();
    assert_eq!(v["page"]["id"], CREATED);
    assert_eq!(v["created_page_requires_explicit_scope"], true);
    assert_eq!(f.writes(), 3);
    assert!(f
        .core
        .execute("notion.read", f.input(json!({"page_id":CREATED})))
        .await
        .unwrap_err()
        .contains("OUT_OF_SCOPE"));
    f.clean().await;
}
#[tokio::test]
async fn notion_core_contextual_review_and_durable_content_omission() {
    let f = Fixture::new(false).await;
    let observed = f.read().await;
    let input =
        f.input(json!({"page_id":PAGE,"body":update_body(),"expected":observed["observation"]}));
    let p = prepare(&f.core, "notion.update", &input).await.unwrap();
    let review = p.review();
    drop(p);
    assert!(review.to_string().contains("reviewed change"));
    assert!(review.to_string().contains("owned private title"));
    // A synthetic existing L2 grant, never a human UI claim.
    f.core.grants.lock().unwrap().insert(
        format!("notion.update:{}", policy::redact(&review)),
        policy::Risk::Contextual,
    );
    f.core.execute("notion.update", input).await.unwrap();
    assert_eq!(f.writes(), 1);
    let audit = f
        .core
        .store
        .query("SELECT input_summary,output_summary FROM activity_events WHERE tool_id='notion.update'", &[])
        .unwrap()
        .to_string();
    assert!(!audit.contains("reviewed change"));
    assert!(!audit.contains("owned private title"));
    assert!(audit.contains("NOTION_CONTENT_OMITTED"));
    f.clean().await;
}
#[tokio::test]
async fn notion_pagination_cycles_bounds_and_foreign_query_rows() {
    let f = Fixture::new(true).await;
    f.m.lock().unwrap().list_override =
        Some(json!({"object":"list","results":[],"has_more":true,"next_cursor":"cursor-a"}));
    for extra in [
        json!({"query":"owned","page_token":"cursor-a"}),
        json!({"query":"owned","page_token":"cursor-b","seen_cursors":["cursor-a"]}),
    ] {
        assert_eq!(
            f.core
                .execute("notion.search", f.input(extra))
                .await
                .unwrap_err(),
            "NOTION_CURSOR_CYCLE"
        );
    }
    f.m.lock().unwrap().list_override =
        Some(json!({"object":"list","results":[],"has_more":true,"next_cursor":"x".repeat(8193)}));
    assert_eq!(
        f.core
            .execute("notion.search", f.input(json!({"query":"owned"})))
            .await
            .unwrap_err(),
        "NOTION_CURSOR"
    );
    f.m.lock().unwrap().list_override = Some(list_body(json!([page()])));
    assert_eq!(
        f.core
            .execute(
                "notion.query_database",
                f.input(json!({"data_source_id":SOURCE,"body":{}}))
            )
            .await
            .unwrap_err(),
        "NOTION_QUERY_PARENT"
    );
    f.clean().await;
}
#[tokio::test]
async fn notion_rate_limit_cooldown_safe_errors_and_no_replay() {
    let f = Fixture::new(true).await;
    {
        let mut m = f.m.lock().unwrap();
        m.error = Some((429, "rate_limited".into()));
        m.retry = Some("120".into());
    }
    let e = f.read_error().await;
    assert!(e.ends_with("RETRY_AFTER_120"));
    assert!(!e.contains("private content"));
    let count = f.m.lock().unwrap().seen.len();
    assert!(f
        .read_error()
        .await
        .starts_with("NOTION_RATE_LIMITED_RETRY_AFTER_"));
    assert_eq!(f.m.lock().unwrap().seen.len(), count);
    f.core
        .store
        .set_setting(&format!("notion.cooldown.{}", f.id), &Value::Null)
        .unwrap();
    {
        let mut m = f.m.lock().unwrap();
        m.error = Some((529, "service_overload".into()));
        m.retry = Some("999999999999999999999999".into());
    }
    assert!(f.read_error().await.ends_with("REVIEW_REQUIRED"));
    assert_eq!(f.read_error().await, "NOTION_RATE_LIMIT_REQUIRES_REVIEW");
    f.clean().await;
}
#[tokio::test]
async fn notion_uncertain_write_transport_503_and_malformed_responses() {
    let f = Fixture::new(false).await;
    for malformed in [false, true] {
        let observed = f.read().await;
        let p = f
            .prepare_write(
                "notion.update",
                json!({"page_id":PAGE,"body":update_body(),"expected":observed["observation"]}),
            )
            .await;
        {
            let mut m = f.m.lock().unwrap();
            m.error = if malformed {
                None
            } else {
                Some((503, "service_unavailable".into()))
            };
            m.write_error = true;
            m.malformed = malformed;
        }
        let before = f.writes();
        let e = execute(p).await.unwrap_err();
        assert!(e.starts_with("NOTION_WRITE_OUTCOME_UNCERTAIN"));
        assert!(!e.contains("private content"));
        assert_eq!(f.writes(), before + 1);
        {
            let mut m = f.m.lock().unwrap();
            m.error = None;
            m.write_error = false;
            m.malformed = false;
        }
    }
    f.clean().await;
}
#[tokio::test]
async fn notion_privacy_and_connection_generation_abort_inflight() {
    let f = Fixture::new(false).await;
    let p = prepare(&f.core, "notion.read", &f.input(json!({"page_id":PAGE})))
        .await
        .unwrap();
    let started = {
        let mut m = f.m.lock().unwrap();
        m.delay = true;
        m.started.clone()
    };
    let task = tokio::spawn(async move { execute(p).await });
    started.notified().await;
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        "PRIVACY_DENIED"
    );
    f.core.store.set_setting("privacy", &json!(false)).unwrap();
    f.m.lock().unwrap().delay = false;
    let p = prepare(&f.core, "notion.read", &f.input(json!({"page_id":PAGE})))
        .await
        .unwrap();
    let config = record(&f.core, &f.id).unwrap();
    integrations::save(&f.core,&json!({"id":f.id,"module_id":"notion","name":"Rename invalidates outstanding request","config":config})).await.unwrap();
    assert_eq!(execute(p).await.unwrap_err(), "NOTION_CONNECTION_CHANGED");
    tokio::time::sleep(Duration::from_secs(2)).await;
    f.clean().await;
}
#[tokio::test]
async fn notion_redirect_id_mismatch_and_body_limits_fail_closed() {
    let f = Fixture::new(false).await;
    f.m.lock().unwrap().redirect = Some("http://127.0.0.1:1/foreign".into());
    assert_eq!(f.read_error().await, "NOTION_HTTP_302_unknown_error");
    assert_eq!(f.m.lock().unwrap().seen.len(), 1);
    {
        let mut m = f.m.lock().unwrap();
        m.redirect = None;
        m.page["id"] = json!(FOREIGN);
    }
    assert_eq!(f.read_error().await, "NOTION_RESPONSE_ID_MISMATCH");
    f.m.lock().unwrap().page = page();
    let observed = f.read().await;
    for body in [
        json!({"properties":{},"in_trash":true}),
        json!({"properties":{"x":{"relation":[{"id":FOREIGN}]}}}),
        json!({"properties":{"title":{"title":[{"text":{"content":"x".repeat(2001)}}]}}}),
    ] {
        assert!(prepare(
            &f.core,
            "notion.update",
            &f.input(json!({"page_id":PAGE,"body":body,"expected":observed["observation"]}))
        )
        .await
        .is_err());
    }
    assert_eq!(f.writes(), 0);
    f.clean().await;
}
#[tokio::test]
async fn notion_data_source_create_schema_changes_and_read_archived() {
    let f = Fixture::new(false).await;
    let observed = f
        .core
        .execute(
            "notion.query_database",
            f.input(json!({"data_source_id":SOURCE,"body":{}})),
        )
        .await
        .unwrap();
    let body = json!({"parent":{"type":"data_source_id","data_source_id":SOURCE},"properties":{"Name":{"title":[{"text":{"content":"Owned data source page"}}]}}});
    let p = f
        .prepare_write(
            "notion.create",
            json!({"body":body,"expected":observed["observation"]}),
        )
        .await;
    f.m.lock().unwrap().source["properties"]["Name"]["type"] = json!("number");
    assert_eq!(execute(p).await.unwrap_err(), "NOTION_STALE_OBSERVATION");
    assert_eq!(f.writes(), 0);
    assert!(prepare(
        &f.core,
        "notion.create",
        &f.input(json!({"body":body,"expected":observed["observation"]}))
    )
    .await
    .is_err());
    f.m.lock().unwrap().source = source();
    let p = f
        .prepare_write(
            "notion.create",
            json!({"body":body,"expected":observed["observation"]}),
        )
        .await;
    assert_eq!(
        execute(p).await.unwrap()["page"]["parent"]["data_source_id"],
        SOURCE
    );
    f.m.lock().unwrap().page["archived"] = json!(true);
    let read = f.read().await;
    assert_eq!(read["page"]["archived"], true);
    assert_eq!(
        prepare(
            &f.core,
            "notion.update",
            &f.input(json!({"page_id":PAGE,"body":update_body(),"expected":read["observation"]}))
        )
        .await
        .err()
        .unwrap(),
        "NOTION_OBJECT_READ_ONLY"
    );
    f.m.lock().unwrap().page = page();
    f.m.lock().unwrap().page["unknown_additive_field"] = json!({"opaque":"x".repeat(LIMIT)});
    assert_eq!(f.read_error().await, "HTTP_RESPONSE_LIMIT");
    f.clean().await;
}
#[tokio::test]
async fn notion_dispatched_write_revocation_is_uncertain_and_never_replayed() {
    let f = Fixture::new(false).await;
    let observed = f.read().await;
    let p = f
        .prepare_write(
            "notion.update",
            json!({"page_id":PAGE,"body":update_body(),"expected":observed["observation"]}),
        )
        .await;
    let started = {
        let mut m = f.m.lock().unwrap();
        m.delay_write = true;
        m.started.clone()
    };
    let task = tokio::spawn(async move { execute(p).await });
    tokio::time::timeout(Duration::from_secs(3), started.notified())
        .await
        .unwrap();
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    let e = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(e.starts_with("NOTION_WRITE_OUTCOME_UNCERTAIN: PRIVACY_DENIED"));
    assert_eq!(f.writes(), 1);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(f.writes(), 1);
    f.clean().await;
}
#[tokio::test]
async fn notion_legacy_or_foreign_alias_requires_explicit_credential_before_any_write() {
    let f = Fixture::new(false).await;
    let valid = record(&f.core, &f.id).unwrap();
    for (old_config, old_alias) in [
        (
            json!({"base_url":"https://legacy.example","auth":"bearer"}).to_string(),
            format!("connection.{}", f.id),
        ),
        (
            json!({"base_url":"https://api.notion.com","auth":"none"}).to_string(),
            format!("connection.{}", f.id),
        ),
        (
            valid.to_string(),
            "connection.owned_foreign_alias".to_string(),
        ),
        ("{malformed".to_string(), format!("connection.{}", f.id)),
    ] {
        f.core
            .store
            .execute(
                "UPDATE connections SET config_json=?,secret_ref=? WHERE id=?",
                &[json!(old_config), json!(old_alias), json!(f.id)],
            )
            .unwrap();
        let before = f
            .core
            .store
            .query("SELECT * FROM connections WHERE id=?", &[json!(f.id)])
            .unwrap();
        let generation = f
            .core
            .store
            .setting(&format!("notion.connection_generation.{}", f.id));
        let result=integrations::save(&f.core,&json!({"id":f.id,"module_id":"notion","name":"Owned rejected legacy upgrade","config":valid,"secret":""})).await;
        if result.is_ok() {
            let config_changed = f
                .core
                .store
                .query("SELECT * FROM connections WHERE id=?", &[json!(f.id)])
                .unwrap()
                != before;
            let generation_changed = f
                .core
                .store
                .setting(&format!("notion.connection_generation.{}", f.id))
                != generation;
            println!(
                "NOTION_LEGACY_NEGATIVE: {}",
                json!({"acceptedWithoutExplicitCredential":true,"configurationChanged":config_changed,"generationChanged":generation_changed,"providerRequests":f.m.lock().unwrap().seen.len()})
            );
        }
        assert_eq!(
            result.unwrap_err(),
            "NOTION_CREDENTIAL_REQUIRED_FOR_PROFILE_MIGRATION"
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
                .setting(&format!("notion.connection_generation.{}", f.id)),
            generation
        );
        assert!(f.m.lock().unwrap().seen.is_empty());
    }
    // No real WCM credential is created/read, and rejection precedes its setter.
    f.clean().await;
}
