//! Scoped Notion API operations. Pages are manual and writes are observed L2
//! actions, with no automatic replay or claim of remote compare-and-swap.
use crate::{integrations, policy, process, runtime::Core};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{future::Future, sync::Arc, time::Duration};

pub(crate) const TOOLS: &[&str] = &[
    "notion.search",
    "notion.read",
    "notion.create",
    "notion.update",
    "notion.append",
    "notion.blocks",
    "notion.read_database",
    "notion.query_database",
];
const VERSION: &str = "2025-09-03";
const LIMIT: usize = 4 * 1024 * 1024;
const REQUEST_LIMIT: usize = 400 * 1024;
fn digest(v: &Value) -> String {
    format!("{:x}", Sha256::digest(v.to_string().as_bytes()))
}
fn id(s: &str) -> Result<String, String> {
    uuid::Uuid::parse_str(s)
        .map(|u| u.to_string())
        .map_err(|_| "NOTION_RESOURCE_ID".into())
}
fn input_id(v: &Value, key: &str) -> Result<String, String> {
    let s = v[key].as_str().ok_or("NOTION_RESOURCE_ID")?;
    if id(s)? != s {
        return Err("NOTION_CANONICAL_ID_REQUIRED".into());
    }
    Ok(s.into())
}
pub(crate) fn validate_config(v: &Value) -> Result<(), String> {
    let schema = json!({"type":"object","required":["allowed_pages","allowed_databases","allowed_data_sources"],"properties":{
        "allowed_pages":{"type":"array","maxItems":1000,"uniqueItems":true,"items":{"type":"string"}},
        "allowed_databases":{"type":"array","maxItems":1000,"uniqueItems":true,"items":{"type":"string"}},
        "allowed_data_sources":{"type":"array","maxItems":1000,"uniqueItems":true,"items":{"type":"string"}},
        "base_url":{"const":"https://api.notion.com"},"auth":{"const":"integration_token"},
        "api_version":{"const":VERSION},"read_only":{"type":"boolean"}},"additionalProperties":false});
    if !jsonschema::validator_for(&schema)
        .map_err(|_| "NOTION_CONFIG_SCHEMA")?
        .is_valid(v)
    {
        return Err("NOTION_CONFIG_INVALID".into());
    }
    let mut count = 0;
    for key in ["allowed_pages", "allowed_databases", "allowed_data_sources"] {
        for entry in v[key].as_array().ok_or("NOTION_CONFIG_INVALID")? {
            input_id(&json!({"id":entry}), "id")?;
            count += 1;
        }
    }
    if count == 0 {
        return Err("NOTION_SCOPE_EMPTY".into());
    }
    Ok(())
}
pub(crate) fn mutation(tool: &str) -> bool {
    ["notion.create", "notion.update", "notion.append"].contains(&tool)
}
pub(crate) fn schema(tool: &str) -> Value {
    if !TOOLS.contains(&tool) {
        return json!({"not":{}});
    }
    let mut props = serde_json::Map::new();
    props.insert(
        "connection".into(),
        json!({"type":"string","format":"uuid"}),
    );
    let mut required = vec!["connection"];
    if [
        "notion.read",
        "notion.update",
        "notion.append",
        "notion.blocks",
    ]
    .contains(&tool)
    {
        props.insert("page_id".into(), json!({"type":"string","format":"uuid"}));
        required.push("page_id");
    }
    for (t, key) in [
        ("notion.read_database", "database_id"),
        ("notion.query_database", "data_source_id"),
    ] {
        if tool == t {
            props.insert(key.into(), json!({"type":"string","format":"uuid"}));
            required.push(key);
        }
    }
    if tool == "notion.search" {
        props.insert("query".into(), json!({"type":"string","maxLength":2000}));
        required.push("query");
    }
    if ["notion.search", "notion.blocks", "notion.query_database"].contains(&tool) {
        props.insert(
            "page_size".into(),
            json!({"type":"integer","minimum":1,"maximum":100}),
        );
        props.insert(
            "page_token".into(),
            json!({"type":"string","minLength":1,"maxLength":8192}),
        );
        props.insert("seen_cursors".into(),json!({"type":"array","maxItems":64,"uniqueItems":true,"items":{"type":"string","minLength":1,"maxLength":8192}}));
    }
    if mutation(tool) || tool == "notion.query_database" {
        props.insert("body".into(), json!({"type":"object"}));
        required.push("body");
    }
    if mutation(tool) {
        props.insert("expected".into(),json!({"type":"object","required":["fingerprint"],"properties":{"fingerprint":{"type":"string","pattern":"^[a-f0-9]{64}$"}},"additionalProperties":false}));
        required.push("expected");
    }
    json!({"type":"object","required":required,"properties":props,"additionalProperties":false})
}
#[cfg(test)]
#[derive(Clone, PartialEq)]
pub(crate) struct TestTransport {
    pub base: String,
    pub token: Option<String>,
}
struct Scope {
    core: Arc<Core>,
    connection: String,
    config: Value,
    generation: Value,
    #[cfg(test)]
    fixture: Option<TestTransport>,
}
fn record(core: &Core, connection: &str) -> Result<Value, String> {
    input_id(&json!({"connection":connection}), "connection")?;
    let rows = core.store.query(
        "SELECT id,module_id,config_json,secret_ref FROM connections WHERE id=? COLLATE NOCASE",
        &[json!(connection)],
    )?;
    if rows.as_array().is_none_or(|r| r.len() != 1)
        || rows[0]["id"] != connection
        || rows[0]["module_id"] != "notion"
        || rows[0]["secret_ref"] != format!("connection.{connection}")
    {
        return Err("NOTION_CONNECTION_ALIAS_CONFLICT".into());
    }
    let config: Value = serde_json::from_str(
        rows[0]["config_json"]
            .as_str()
            .ok_or("NOTION_CONFIG_INVALID")?,
    )
    .map_err(|_| "NOTION_CONFIG_INVALID")?;
    validate_config(&config)?;
    Ok(config)
}
impl Scope {
    fn new(core: &Arc<Core>, connection: &str) -> Result<Self, String> {
        let _lock = integrations::save_lock(core)?;
        let s = Self {
            core: core.clone(),
            connection: connection.into(),
            config: record(core, connection)?,
            generation: core
                .store
                .setting(&format!("notion.connection_generation.{connection}")),
            #[cfg(test)]
            fixture: core
                .notion_fixture
                .lock()
                .map_err(|_| "NOTION_FIXTURE_LOCK")?
                .clone(),
        };
        drop(_lock);
        s.check()?;
        Ok(s)
    }
    fn check(&self) -> Result<(), String> {
        if self.core.store.setting("privacy") == true {
            return Err("PRIVACY_DENIED".into());
        }
        if !self.core.enabled("notion") {
            return Err("MODULE_DISABLED: notion".into());
        }
        let _lock = integrations::save_lock(&self.core)?;
        if record(&self.core, &self.connection)? != self.config
            || self
                .core
                .store
                .setting(&format!("notion.connection_generation.{}", self.connection))
                != self.generation
        {
            return Err("NOTION_CONNECTION_CHANGED".into());
        }
        #[cfg(test)]
        if *self
            .core
            .notion_fixture
            .lock()
            .map_err(|_| "NOTION_FIXTURE_LOCK")?
            != self.fixture
        {
            return Err("NOTION_FIXTURE_CHANGED".into());
        }
        Ok(())
    }
    fn allows(&self, key: &str, target: &str) -> bool {
        self.config[key]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == target))
    }
    fn require(&self, key: &str, target: &str) -> Result<(), String> {
        if self.allows(key, target) {
            Ok(())
        } else {
            Err("NOTION_RESOURCE_OUT_OF_SCOPE".into())
        }
    }
    async fn guard<T>(&self, f: impl Future<Output = Result<T, String>>) -> Result<T, String> {
        self.check()?;
        tokio::pin!(f);
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        loop {
            tokio::select! {biased; _=tick.tick()=>self.check()?, r=&mut f=>{self.check()?;return r;}}
        }
    }
}
struct Session {
    scope: Scope,
    client: reqwest::Client,
    base: reqwest::Url,
    token: String,
}
impl Session {
    async fn new(core: &Arc<Core>, connection: &str) -> Result<Self, String> {
        let scope = Scope::new(core, connection)?;
        let base = reqwest::Url::parse("https://api.notion.com").map_err(|_| "NOTION_ORIGIN")?;
        #[cfg(test)]
        let base = if let Some(f) = &scope.fixture {
            let u = reqwest::Url::parse(&f.base).map_err(|_| "NOTION_FIXTURE_ORIGIN")?;
            if u.scheme() != "http"
                || u.host_str() != Some("127.0.0.1")
                || u.port().is_none()
                || u.path() != "/"
                || !u.username().is_empty()
                || u.password().is_some()
                || u.query().is_some()
                || u.fragment().is_some()
            {
                return Err("NOTION_FIXTURE_ORIGIN".into());
            }
            u
        } else {
            base
        };
        #[cfg(test)]
        let fixture_token = scope.fixture.as_ref().and_then(|f| f.token.clone());
        #[cfg(not(test))]
        let fixture_token: Option<String> = None;
        let token = if let Some(token) = fixture_token {
            token
        } else {
            let v = scope
                .guard(process::native(
                    &core.native,
                    "secret.get",
                    &json!({"alias":format!("connection.{connection}")}),
                ))
                .await?;
            v["value"]
                .as_str()
                .ok_or("NOTION_CREDENTIAL_REQUIRED")?
                .to_string()
        };
        if token.is_empty()
            || token.len() > 16384
            || !token.bytes().all(|c| (33..=126).contains(&c))
        {
            return Err("NOTION_CREDENTIAL_INVALID".into());
        }
        scope.check()?;
        let client = crate::http_client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| "NOTION_CLIENT")?;
        Ok(Self {
            scope,
            client,
            base,
            token,
        })
    }
    fn cooldown(&self) -> Result<(), String> {
        let saved = self
            .scope
            .core
            .store
            .setting(&format!("notion.cooldown.{}", self.scope.connection));
        if saved["manual"] == true {
            return Err("NOTION_RATE_LIMIT_REQUIRES_REVIEW".into());
        }
        if let Some(until) = saved["until"].as_i64() {
            if until > chrono::Utc::now().timestamp() {
                return Err(format!(
                    "NOTION_RATE_LIMITED_RETRY_AFTER_{}",
                    until - chrono::Utc::now().timestamp()
                ));
            }
        }
        Ok(())
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, String> {
        self.scope.check()?;
        self.cooldown()?;
        if body.is_some_and(|b| b.to_string().len() > REQUEST_LIMIT) {
            return Err("NOTION_REQUEST_LIMIT".into());
        }
        let url = integrations::scoped_url(self.base.as_str(), path)?;
        let request = self
            .client
            .request(
                reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| "NOTION_METHOD")?,
                url,
            )
            .bearer_auth(&self.token)
            .header("Notion-Version", VERSION);
        let request = if let Some(body) = body {
            request.json(body)
        } else {
            request
        };
        self.scope
            .guard(async {
                let response = request.send().await.map_err(|e| {
                    if e.is_timeout() {
                        "NOTION_TIMEOUT"
                    } else {
                        "NOTION_NETWORK_ERROR"
                    }
                })?;
                let status = response.status().as_u16();
                let retry = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok());
                let rate_limited = [429, 529].contains(&status);
                if rate_limited {
                    let saved = match retry.filter(|s| *s > 0 && *s <= 604800) {
                        Some(seconds) => {
                            json!({"until":chrono::Utc::now().timestamp()+seconds as i64})
                        }
                        None => json!({"manual":true}),
                    };
                    self.scope.core.store.set_setting(
                        &format!("notion.cooldown.{}", self.scope.connection),
                        &saved,
                    )?;
                }
                let bytes = integrations::bounded_body(response, LIMIT).await?;
                let body: Value =
                    serde_json::from_slice(&bytes).map_err(|_| "NOTION_MALFORMED_JSON")?;
                if rate_limited
                    && body["additional_data"]["rate_limit_reason"] == "public_api_request_blocked"
                {
                    self.scope.core.store.set_setting(
                        &format!("notion.cooldown.{}", self.scope.connection),
                        &json!({"manual":true}),
                    )?;
                    return Err("NOTION_RATE_LIMIT_REQUIRES_REVIEW".into());
                }
                if !(200..300).contains(&status) {
                    let code = body["code"].as_str().unwrap_or("");
                    let known = [
                        "invalid_json",
                        "invalid_request_url",
                        "invalid_request",
                        "validation_error",
                        "missing_version",
                        "unauthorized",
                        "restricted_resource",
                        "object_not_found",
                        "conflict_error",
                        "rate_limited",
                        "internal_server_error",
                        "bad_gateway",
                        "service_unavailable",
                        "database_connection_unavailable",
                        "gateway_timeout",
                        "service_overload",
                    ];
                    let safe = if known.contains(&code) {
                        code
                    } else {
                        "unknown_error"
                    };
                    if rate_limited {
                        return Err(format!(
                            "NOTION_HTTP_{status}_{safe}_RETRY_AFTER_{}",
                            retry
                                .filter(|s| *s > 0 && *s <= 604800)
                                .map(|s| s.to_string())
                                .unwrap_or("REVIEW_REQUIRED".into())
                        ));
                    }
                    return Err(format!("NOTION_HTTP_{status}_{safe}"));
                }
                if !body.is_object() {
                    return Err("NOTION_RESPONSE_OBJECT".into());
                }
                Ok(body)
            })
            .await
    }
}
fn bounded_json(v: &Value) -> Result<(), String> {
    if v.to_string().len() > REQUEST_LIMIT {
        return Err("NOTION_REQUEST_LIMIT".into());
    }
    let mut nodes = 0;
    let mut pending = vec![(v, 0)];
    while let Some((v, depth)) = pending.pop() {
        nodes += 1;
        if nodes > 10000 || depth > 16 {
            return Err("NOTION_BODY_COMPLEXITY".into());
        }
        match v {
            Value::Object(m) => {
                if m.len() > 100 {
                    return Err("NOTION_BODY_COMPLEXITY".into());
                }
                pending.extend(m.values().map(|v| (v, depth + 1)));
            }
            Value::Array(a) => {
                if a.len() > 100 {
                    return Err("NOTION_ARRAY_LIMIT".into());
                }
                pending.extend(a.iter().map(|v| (v, depth + 1)));
            }
            Value::String(s) if s.len() > 8192 => return Err("NOTION_STRING_LIMIT".into()),
            _ => {}
        }
    }
    Ok(())
}
fn only(v: &Value, allowed: &[&str]) -> Result<(), String> {
    let m = v.as_object().ok_or("NOTION_BODY_OBJECT")?;
    if m.is_empty() || m.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err("NOTION_BODY_FIELDS".into());
    }
    Ok(())
}
fn properties(scope: &Scope, v: &Value) -> Result<(), String> {
    let map = v.as_object().ok_or("NOTION_PROPERTIES")?;
    if map.is_empty() || map.len() > 100 {
        return Err("NOTION_PROPERTIES".into());
    }
    for (key, property) in map {
        if key.is_empty() || key.len() > 200 {
            return Err("NOTION_PROPERTY_NAME".into());
        }
        only(
            property,
            &[
                "title",
                "rich_text",
                "number",
                "checkbox",
                "url",
                "email",
                "phone_number",
                "select",
                "multi_select",
                "date",
                "relation",
            ],
        )?;
        if property.as_object().unwrap().len() != 1 {
            return Err("NOTION_PROPERTY_TYPE".into());
        }
        for k in ["title", "rich_text"] {
            if let Some(t) = property.get(k) {
                rich_text(scope, t)?;
            }
        }
        if let Some(r) = property.get("relation") {
            for item in r.as_array().ok_or("NOTION_RELATION")? {
                only(item, &["id"])?;
                scope.require("allowed_pages", &input_id(item, "id")?)?;
            }
        }
    }
    Ok(())
}
fn rich_text(scope: &Scope, v: &Value) -> Result<(), String> {
    for t in v.as_array().ok_or("NOTION_RICH_TEXT")? {
        only(t, &["type", "text", "annotations", "mention", "equation"])?;
        if let Some(text) = t.get("text") {
            only(text, &["content", "link"])?;
            if text["content"]
                .as_str()
                .is_none_or(|s| s.chars().count() > 2000)
            {
                return Err("NOTION_TEXT_LIMIT".into());
            }
        }
        if let Some(mention) = t.get("mention") {
            only(mention, &["type", "page", "database", "date"])?;
            for (k, scope_key) in [("page", "allowed_pages"), ("database", "allowed_databases")] {
                if let Some(target) = mention.get(k) {
                    only(target, &["id"])?;
                    scope.require(scope_key, &input_id(target, "id")?)?;
                }
            }
        }
    }
    Ok(())
}
fn children(scope: &Scope, v: &Value) -> Result<(), String> {
    let children = v.as_array().ok_or("NOTION_CHILDREN")?;
    if children.is_empty() || children.len() > 100 {
        return Err("NOTION_CHILDREN_LIMIT".into());
    }
    for block in children {
        let kind = block["type"].as_str().ok_or("NOTION_BLOCK_TYPE")?;
        if ![
            "paragraph",
            "heading_1",
            "heading_2",
            "heading_3",
            "bulleted_list_item",
            "numbered_list_item",
            "to_do",
            "quote",
            "code",
            "divider",
        ]
        .contains(&kind)
        {
            return Err("NOTION_BLOCK_TYPE_UNSUPPORTED".into());
        }
        only(block, &["object", "type", kind])?;
        let value = &block[kind];
        if kind == "divider" {
            if value != &json!({}) {
                return Err("NOTION_DIVIDER".into());
            }
            continue;
        }
        only(
            value,
            &["rich_text", "color", "checked", "language", "caption"],
        )?;
        rich_text(scope, &value["rich_text"])?;
        if let Some(caption) = value.get("caption") {
            rich_text(scope, caption)?;
        }
    }
    Ok(())
}
fn create_parent(scope: &Scope, body: &Value) -> Result<(String, String), String> {
    let p = &body["parent"];
    only(p, &["type", "page_id", "data_source_id"])?;
    let mut target = None;
    for (key, scope_key) in [
        ("page_id", "allowed_pages"),
        ("data_source_id", "allowed_data_sources"),
    ] {
        if p.get(key).is_some() {
            if target.is_some() {
                return Err("NOTION_PARENT_AMBIGUOUS".into());
            }
            let id = input_id(p, key)?;
            scope.require(scope_key, &id)?;
            target = Some((key.to_string(), id));
        }
    }
    let result = target.ok_or("NOTION_PARENT_REQUIRED")?;
    if p.get("type").is_some_and(|t| t != &result.0) {
        return Err("NOTION_PARENT_TYPE".into());
    }
    Ok(result)
}
fn validate_body(scope: &Scope, tool: &str, body: &Value) -> Result<(), String> {
    bounded_json(body)?;
    match tool {
        "notion.create" => {
            only(body, &["parent", "properties", "children"])?;
            create_parent(scope, body)?;
            properties(scope, &body["properties"])?;
            if let Some(c) = body.get("children") {
                children(scope, c)?;
            }
        }
        "notion.update" => {
            only(body, &["properties"])?;
            properties(scope, &body["properties"])?;
        }
        "notion.append" => {
            only(body, &["children"])?;
            children(scope, &body["children"])?;
        }
        "notion.query_database"
            if !body.is_object()
                || body
                    .as_object()
                    .unwrap()
                    .keys()
                    .any(|k| !["filter", "sorts"].contains(&k.as_str())) =>
        {
            return Err("NOTION_QUERY_FIELDS".into());
        }
        _ => {}
    }
    Ok(())
}
fn object(v: Value, kind: &str, target: &str) -> Result<Value, String> {
    if v["object"] != kind || id(v["id"].as_str().ok_or("NOTION_RESPONSE_ID")?)? != target {
        return Err("NOTION_RESPONSE_ID_MISMATCH".into());
    }
    if kind != "database"
        && v["last_edited_time"]
            .as_str()
            .is_none_or(|s| s.len() > 64 || chrono::DateTime::parse_from_rfc3339(s).is_err())
    {
        return Err("NOTION_EDIT_TIME_REQUIRED".into());
    }
    Ok(v)
}
struct Snapshot {
    value: Value,
    fingerprint: String,
}
async fn snapshot(s: &Session, kind: &str, target: &str) -> Result<Snapshot, String> {
    let (endpoint, object_type) = if kind == "page_id" {
        ("pages", "page")
    } else {
        ("data_sources", "data_source")
    };
    let value = object(
        s.request("GET", &format!("/v1/{endpoint}/{target}"), None)
            .await?,
        object_type,
        target,
    )?;
    if kind == "data_source_id" {
        let parent = id(value["parent"]["database_id"]
            .as_str()
            .ok_or("NOTION_DATA_SOURCE_PARENT")?)?;
        s.scope.require("allowed_databases", &parent)?;
    }
    let observed = json!({"object":value["object"],"id":target,"last_edited_time":value["last_edited_time"],"parent":value["parent"],"properties":value["properties"],"archived":value["archived"],"in_trash":value["in_trash"],"is_archived":value["is_archived"],"is_locked":value["is_locked"]});
    let fingerprint = digest(
        &json!({"connection":s.scope.connection,"generation":s.scope.generation,"config":s.scope.config,"observed":observed}),
    );
    Ok(Snapshot { value, fingerprint })
}
fn writable(snapshot: &Snapshot) -> Result<(), String> {
    if ["archived", "is_archived", "in_trash", "is_locked"]
        .iter()
        .any(|k| snapshot.value[*k] == true)
    {
        return Err("NOTION_OBJECT_READ_ONLY".into());
    }
    Ok(())
}
fn observed_properties(
    tool: &str,
    target_kind: &str,
    body: &Value,
    snapshot: &Snapshot,
) -> Result<(), String> {
    if tool == "notion.append" {
        return Ok(());
    }
    let props = body["properties"].as_object().ok_or("NOTION_PROPERTIES")?;
    if tool == "notion.create" && target_kind == "page_id" {
        if props.len() != 1 || !props.contains_key("title") || props["title"].get("title").is_none()
        {
            return Err("NOTION_CHILD_PAGE_TITLE_REQUIRED".into());
        }
        return Ok(());
    }
    let schema = snapshot.value["properties"]
        .as_object()
        .ok_or("NOTION_OBSERVED_PROPERTIES")?;
    for (key, value) in props {
        let observed = schema
            .get(key)
            .or_else(|| schema.values().find(|v| v["id"] == key.as_str()))
            .ok_or("NOTION_PROPERTY_NOT_IN_OBSERVED_SCHEMA")?;
        let kind = value
            .as_object()
            .ok_or("NOTION_PROPERTY_TYPE")?
            .keys()
            .next()
            .ok_or("NOTION_PROPERTY_TYPE")?;
        if observed["type"] != kind.as_str() {
            return Err("NOTION_PROPERTY_TYPE_CHANGED".into());
        }
    }
    Ok(())
}
pub(crate) struct Prepared {
    s: Session,
    tool: String,
    input: Value,
    snapshot: Option<Snapshot>,
    target: Option<(String, String)>,
    pub risk: policy::Risk,
}
impl Prepared {
    pub fn review(&self) -> Value {
        json!({"connection":self.s.scope.connection,"operation":self.tool,"input":self.input,"review_content":self.snapshot.as_ref().map(|s|&s.value),"warning":"Observed L2 write; client recheck only, no remote CAS, automatic replay or undo. Inspect remote state after an uncertain outcome. Page properties and first-level blocks can be incomplete."})
    }
}
pub(crate) async fn prepare(
    core: &Arc<Core>,
    tool: &str,
    input: &Value,
) -> Result<Prepared, String> {
    tokio::time::timeout(Duration::from_secs(55), async {
        if !TOOLS.contains(&tool)
            || !jsonschema::validator_for(&schema(tool))
                .map_err(|_| "NOTION_SCHEMA")?
                .is_valid(input)
        {
            return Err("NOTION_INPUT_INVALID".into());
        }
        bounded_json(input)?;
        let connection = input_id(input, "connection")?;
        let scope = Scope::new(core, &connection)?;
        let mut target = None;
        for (key, scope_key) in [
            ("page_id", "allowed_pages"),
            ("database_id", "allowed_databases"),
            ("data_source_id", "allowed_data_sources"),
        ] {
            if input.get(key).is_some() {
                let target_id = input_id(input, key)?;
                scope.require(scope_key, &target_id)?;
                target = Some((key.to_string(), target_id));
            }
        }
        if mutation(tool) && scope.config["read_only"] != false {
            return Err("NOTION_CONNECTION_READ_ONLY".into());
        }
        if input.get("body").is_some() {
            validate_body(&scope, tool, &input["body"])?;
        }
        if tool == "notion.create" {
            target = Some(create_parent(&scope, &input["body"])?);
        }
        if mutation(tool) && policy::contains_secret(&input["body"].to_string()) {
            return Err("NOTION_SENSITIVE_INPUT".into());
        }
        let s = Session::new(core, &connection).await?;
        let observed = if mutation(tool) {
            let (kind, target) = target.as_ref().ok_or("NOTION_PARENT_REQUIRED")?;
            let a = snapshot(&s, kind, target).await?;
            writable(&a)?;
            observed_properties(tool, kind, &input["body"], &a)?;
            if input["expected"]["fingerprint"] != a.fingerprint {
                return Err("NOTION_STALE_OBSERVATION".into());
            }
            let b = snapshot(&s, kind, target).await?;
            writable(&b)?;
            if a.fingerprint != b.fingerprint {
                return Err("NOTION_STALE_OBSERVATION".into());
            }
            Some(b)
        } else {
            None
        };
        Ok(Prepared {
            s,
            tool: tool.into(),
            input: input.clone(),
            snapshot: observed,
            target,
            risk: if mutation(tool) {
                policy::Risk::Contextual
            } else {
                policy::Risk::Free
            },
        })
    })
    .await
    .map_err(|_| "NOTION_PREPARATION_TIMEOUT".to_string())?
}
fn cursor(v: &Value) -> Result<&str, String> {
    let c = v.as_str().ok_or("NOTION_CURSOR")?;
    if c.is_empty() || c.len() > 8192 {
        return Err("NOTION_CURSOR".into());
    }
    Ok(c)
}
fn list(s: &Session, tool: &str, input: &Value, body: Value) -> Result<Value, String> {
    if body["object"] != "list" {
        return Err("NOTION_LIST_OBJECT".into());
    }
    let rows = body["results"].as_array().ok_or("NOTION_LIST_RESULTS")?;
    if rows.len() > input["page_size"].as_u64().unwrap_or(50) as usize {
        return Err("NOTION_LIST_LIMIT".into());
    }
    let more = body["has_more"].as_bool().ok_or("NOTION_PAGINATION")?;
    let next = if more {
        let next = cursor(&body["next_cursor"])?;
        if input["page_token"] == next
            || input["seen_cursors"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v == next))
        {
            return Err("NOTION_CURSOR_CYCLE".into());
        }
        Some(next)
    } else {
        if !body["next_cursor"].is_null() {
            return Err("NOTION_PAGINATION".into());
        }
        None
    };
    let mut results = vec![];
    let mut filtered = 0;
    for row in rows {
        let target = id(row["id"].as_str().ok_or("NOTION_RESPONSE_ID")?)?;
        if tool == "notion.search" {
            let allowed = match row["object"].as_str() {
                Some("page") => s.scope.allows("allowed_pages", &target),
                Some("data_source") => s.scope.allows("allowed_data_sources", &target),
                _ => false,
            };
            if !allowed {
                filtered += 1;
                continue;
            }
        } else if tool == "notion.query_database" {
            if row["object"] != "page"
                || id(row["parent"]["data_source_id"]
                    .as_str()
                    .ok_or("NOTION_QUERY_PARENT")?)?
                    != input["data_source_id"]
                        .as_str()
                        .ok_or("NOTION_QUERY_PARENT")?
            {
                return Err("NOTION_QUERY_SCOPE".into());
            }
        } else if row["object"] != "block"
            || id(row["parent"]["page_id"]
                .as_str()
                .ok_or("NOTION_BLOCK_PARENT")?)?
                != input["page_id"].as_str().ok_or("NOTION_BLOCK_PARENT")?
        {
            return Err("NOTION_BLOCK_SCOPE".into());
        }
        results.push(row.clone());
    }
    Ok(
        json!({"results":results,"has_more":more,"next_cursor":next,"filtered_out_of_scope":filtered,"untrusted":true,"page_only":true,"properties_complete":false,"recursive_blocks_complete":false}),
    )
}
pub(crate) fn timeout_error(tool: &str) -> String {
    if mutation(tool) {
        "NOTION_WRITE_OUTCOME_UNCERTAIN: TOOL_TIMEOUT; inspect remote state before retry".into()
    } else {
        "NOTION_TIMEOUT".into()
    }
}
pub(crate) async fn execute(p: Prepared) -> Result<Value, String> {
    let tool = p.tool.clone();
    tokio::time::timeout(Duration::from_secs(70), execute_inner(p))
        .await
        .map_err(|_| timeout_error(&tool))?
}
async fn execute_inner(p: Prepared) -> Result<Value, String> {
    let _actions = p.s.scope.core.notion_actions.lock().await;
    p.s.scope.check()?;
    if let Some(before) = &p.snapshot {
        let (kind, target) = p.target.as_ref().ok_or("NOTION_PARENT_REQUIRED")?;
        let now = snapshot(&p.s, kind, target).await?;
        writable(&now)?;
        if before.fingerprint != now.fingerprint {
            return Err("NOTION_STALE_OBSERVATION".into());
        }
        let (method, path) = match p.tool.as_str() {
            "notion.create" => ("POST", "/v1/pages".into()),
            "notion.update" => ("PATCH", format!("/v1/pages/{target}")),
            "notion.append" => ("PATCH", format!("/v1/blocks/{target}/children")),
            _ => return Err("NOTION_WRITE_TOOL".into()),
        };
        let response =
            p.s.request(method, &path, Some(&p.input["body"]))
                .await
                .map_err(|e| {
                    if e.starts_with("NOTION_HTTP_4") && !e.starts_with("NOTION_HTTP_409") {
                        e
                    } else {
                        format!(
                        "NOTION_WRITE_OUTCOME_UNCERTAIN: {e}; inspect remote state before retry"
                    )
                    }
                })?;
        let checked = (|| -> Result<Value, String> {
            if p.tool == "notion.append" {
                let v = list(
                    &p.s,
                    "notion.blocks",
                    &json!({"page_id":target,"page_size":100}),
                    response,
                )?;
                if v["has_more"] != false
                    || v["results"].as_array().is_none_or(|a| {
                        a.len() != p.input["body"]["children"].as_array().unwrap().len()
                    })
                {
                    return Err("NOTION_APPEND_RESPONSE".into());
                }
                Ok(v)
            } else {
                let response_id = if p.tool == "notion.create" {
                    id(response["id"].as_str().ok_or("NOTION_CREATED_ID")?)?
                } else {
                    target.clone()
                };
                let page = object(response, "page", &response_id)?;
                if p.tool == "notion.create"
                    && id(page["parent"][kind]
                        .as_str()
                        .ok_or("NOTION_CREATED_PARENT")?)?
                        != *target
                {
                    return Err("NOTION_CREATED_PARENT".into());
                }
                Ok(
                    json!({"page":page,"created_page_requires_explicit_scope":p.tool=="notion.create","untrusted":true,"properties_complete":false}),
                )
            }
        })();
        return checked.map_err(|e| {
            format!("NOTION_WRITE_OUTCOME_UNCERTAIN: {e}; inspect remote state before retry")
        });
    }
    let target = p.target.as_ref().map(|(_, v)| v.as_str()).unwrap_or("");
    match p.tool.as_str() {
        "notion.read" => {
            let s = snapshot(&p.s, "page_id", target).await?;
            Ok(
                json!({"page":s.value,"observation":{"fingerprint":s.fingerprint},"untrusted":true,"properties_complete":false}),
            )
        }
        "notion.read_database" => {
            let v = object(
                p.s.request("GET", &format!("/v1/databases/{target}"), None)
                    .await?,
                "database",
                target,
            )?;
            Ok(json!({"database":v,"untrusted":true,"query_uses_explicit_data_source":true}))
        }
        "notion.search" | "notion.blocks" | "notion.query_database" => {
            let mut body = if p.tool == "notion.query_database" {
                p.input["body"].clone()
            } else {
                json!({})
            };
            body["page_size"] = json!(p.input["page_size"].as_u64().unwrap_or(50));
            if let Some(c) = p.input.get("page_token") {
                body["start_cursor"] = json!(cursor(c)?);
            }
            let observation = if p.tool == "notion.query_database" {
                Some(snapshot(&p.s, "data_source_id", target).await?)
            } else {
                None
            };
            let response = match p.tool.as_str() {
                "notion.search" => {
                    body["query"] = p.input["query"].clone();
                    p.s.request("POST", "/v1/search", Some(&body)).await?
                }
                "notion.query_database" => {
                    p.s.request(
                        "POST",
                        &format!("/v1/data_sources/{target}/query"),
                        Some(&body),
                    )
                    .await?
                }
                _ => {
                    let mut u =
                        p.s.base
                            .join(&format!("/v1/blocks/{target}/children"))
                            .map_err(|_| "NOTION_BLOCK_URL")?;
                    u.query_pairs_mut()
                        .append_pair("page_size", &body["page_size"].to_string());
                    if let Some(c) = body["start_cursor"].as_str() {
                        u.query_pairs_mut().append_pair("start_cursor", c);
                    }
                    p.s.request(
                        "GET",
                        &format!("{}?{}", u.path(), u.query().unwrap_or("")),
                        None,
                    )
                    .await?
                }
            };
            let mut result = list(&p.s, &p.tool, &p.input, response)?;
            if let Some(s) = observation {
                result["data_source"] = s.value;
                result["observation"] = json!({"fingerprint":s.fingerprint});
            }
            Ok(result)
        }
        _ => Err("NOTION_TOOL_UNKNOWN".into()),
    }
}
pub(crate) fn durable(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        if [
                            "body",
                            "query",
                            "properties",
                            "page",
                            "database",
                            "data_source",
                            "results",
                            "review_content",
                            "page_token",
                            "next_cursor",
                            "seen_cursors",
                        ]
                        .contains(&k.as_str())
                        {
                            json!("[NOTION_CONTENT_OMITTED]")
                        } else {
                            durable(v)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(durable).collect()),
        _ => v.clone(),
    }
}
#[cfg(all(test, windows))]
#[path = "notion_tests.rs"]
mod tests;
