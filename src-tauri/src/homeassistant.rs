//! Fixed-origin Home Assistant actor. Remote content is untrusted, and an
//! acknowledged service action never proves a physical device reached its goal.
use crate::{integrations, policy::Risk, runtime::Core, storage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, future::Future, sync::Arc, time::Duration};

#[path = "homeassistant_credentials.rs"]
mod credentials;
pub(crate) async fn save_connection(
    core: &Arc<Core>,
    id: &str,
    name: &str,
    raw: &Value,
    secret: Option<&str>,
    existing: bool,
) -> Result<Value, String> {
    credentials::save(core, id, name, raw, secret, existing).await
}
#[path = "homeassistant_ws.rs"]
mod homeassistant_ws;
pub(crate) use homeassistant_ws::Stream;
pub(crate) const TOOLS: &[&str] = &[
    "homeassistant.entities",
    "homeassistant.states",
    "homeassistant.services",
    "homeassistant.call_service",
    "homeassistant.automations",
    "homeassistant.camera_snapshot",
    "homeassistant.events",
    "homeassistant.websocket",
];
const LIMIT: usize = 4 * 1024 * 1024;
const BODY_LIMIT: usize = 32 * 1024;
fn yes() -> bool {
    true
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Service {
    input_schema: Value,
    #[serde(default)]
    global: bool,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Config {
    profile_version: u8,
    base_url: String,
    #[serde(default = "yes")]
    read_only: bool,
    #[serde(default)]
    websocket: bool,
    #[serde(default)]
    allow_insecure_http: bool,
    allowed_entities: Vec<String>,
    allowed_services: BTreeMap<String, Service>,
    allowed_events: Vec<String>,
}
fn atom(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
fn entity(s: &str) -> bool {
    s.split_once('.')
        .is_some_and(|(domain, name)| atom(domain) && atom(name))
}
fn unique(list: &[String], valid: fn(&str) -> bool) -> bool {
    list.len() <= 256
        && list.iter().all(|s| valid(s))
        && list.iter().collect::<std::collections::BTreeSet<_>>().len() == list.len()
}
fn schema_reference(v: &Value) -> bool {
    match v {
        Value::Object(m) => m
            .iter()
            .any(|(k, v)| k.starts_with('$') || schema_reference(v)),
        Value::Array(a) => a.iter().any(schema_reference),
        _ => false,
    }
}
fn origin(config: &Config) -> Result<reqwest::Url, String> {
    let u = reqwest::Url::parse(&config.base_url).map_err(|_| "HA_ORIGIN")?;
    if !matches!(u.scheme(), "http" | "https")
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.path() != "/"
        || u.query().is_some()
        || u.fragment().is_some()
        || (u.scheme() == "http"
            && !config.allow_insecure_http
            && !matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
    {
        return Err("HA_ORIGIN_OR_TLS_POLICY".into());
    }
    Ok(u)
}
fn config(v: &Value) -> Result<Config, String> {
    if v.to_string().len() > 256 * 1024 {
        return Err("HA_CONFIG_LIMIT".into());
    }
    let c: Config = serde_json::from_value(v.clone()).map_err(|_| "HA_CONFIG_INVALID")?;
    if c.profile_version != 1
        || !unique(&c.allowed_entities, entity)
        || !unique(&c.allowed_events, atom)
        || c.allowed_services.len() > 256
    {
        return Err("HA_CONFIG_SCOPE".into());
    }
    origin(&c)?;
    for (name, service) in &c.allowed_services {
        if !entity(name)
            || service.input_schema["type"] != "object"
            || service.input_schema["additionalProperties"] != false
            || schema_reference(&service.input_schema)
            || jsonschema::validator_for(&service.input_schema).is_err()
        {
            return Err("HA_SERVICE_SCHEMA".into());
        }
    }
    Ok(c)
}
pub(crate) fn validate_config(v: &Value) -> Result<(), String> {
    config(v).map(|_| ())
}
pub(crate) fn validate_token(s: &str) -> Result<(), String> {
    if s.is_empty() || s.len() > 1280 || !s.bytes().all(|b| (33..=126).contains(&b)) {
        Err("HA_CREDENTIAL_INVALID".into())
    } else {
        Ok(())
    }
}
fn digest(v: &Value) -> String {
    format!("{:x}", Sha256::digest(v.to_string().as_bytes()))
}
fn canonical(s: &str) -> Result<(), String> {
    if uuid::Uuid::parse_str(s)
        .map_err(|_| "HA_CONNECTION_ID")?
        .to_string()
        != s
    {
        return Err("HA_CONNECTION_ID".into());
    }
    Ok(())
}
pub(crate) fn schema(tool: &str) -> Value {
    let mut p = serde_json::Map::new();
    p.insert(
        "connection".into(),
        json!({"type":"string","format":"uuid"}),
    );
    let mut required = vec!["connection"];
    if [
        "homeassistant.states",
        "homeassistant.camera_snapshot",
        "homeassistant.automations",
    ]
    .contains(&tool)
    {
        p.insert(
            "entity_id".into(),
            json!({"type":"string","pattern":"^[a-z0-9_]+\\.[a-z0-9_]+$","maxLength":257}),
        );
        if tool == "homeassistant.camera_snapshot" {
            required.push("entity_id");
        }
    }
    if ["homeassistant.call_service", "homeassistant.automations"].contains(&tool) {
        p.insert("return_response".into(), json!({"type":"boolean"}));
        p.insert("action".into(), json!({"enum":if tool == "homeassistant.automations" {vec!["list","inspect","call"]}else{vec!["inspect","call"]}}));
        p.insert(
            "service".into(),
            json!({"type":"string","pattern":"^[a-z0-9_]+$","maxLength":128}),
        );
        p.insert("body".into(), json!({"type":"object"}));
        p.insert("expected".into(), json!({"type":"object","required":["fingerprint"],"properties":{"fingerprint":{"type":"string","pattern":"^[a-f0-9]{64}$"}},"additionalProperties":false}));
        if tool == "homeassistant.call_service" {
            required.extend(["action", "domain", "service", "entity_ids", "body"]);
            p.insert(
                "domain".into(),
                json!({"type":"string","pattern":"^[a-z0-9_]+$","maxLength":128}),
            );
            p.insert("entity_ids".into(), json!({"type":"array","maxItems":64,"uniqueItems":true,"items":{"type":"string","pattern":"^[a-z0-9_]+\\.[a-z0-9_]+$","maxLength":257}}));
        }
    }
    if tool == "homeassistant.websocket" {
        required.push("action");
        p.insert(
            "action".into(),
            json!({"enum":["start","status","poll","stop"]}),
        );
        p.insert(
            "event_type".into(),
            json!({"type":"string","pattern":"^[a-z0-9_]+$","maxLength":128}),
        );
        p.insert("stream_id".into(), json!({"type":"string","format":"uuid"}));
        p.insert(
            "max_events".into(),
            json!({"type":"integer","minimum":1,"maximum":128}),
        );
    }
    json!({"type":"object","required":required,"properties":p,"additionalProperties":false})
}
#[cfg(test)]
#[derive(Clone, PartialEq)]
pub(crate) struct TestTransport {
    pub token: Option<String>,
    pub headless_permission: bool,
}
#[derive(Clone)]
struct Scope {
    core: Arc<Core>,
    connection: String,
    raw: Value,
    config: Config,
    generation: Value,
    credential_binding: Value,
    #[cfg(test)]
    fixture: Option<TestTransport>,
}
fn record(core: &Core, id: &str) -> Result<Value, String> {
    canonical(id)?;
    let rows = core.store.query(
        "SELECT id,module_id,config_json,secret_ref FROM connections WHERE id=? COLLATE NOCASE",
        &[json!(id)],
    )?;
    if rows.as_array().is_none_or(|a| a.len() != 1)
        || rows[0]["id"] != id
        || rows[0]["module_id"] != "homeassistant"
        || rows[0]["secret_ref"] != format!("connection.{id}")
    {
        return Err("HA_CONNECTION_ALIAS_CONFLICT".into());
    }
    let v: Value =
        serde_json::from_str(rows[0]["config_json"].as_str().ok_or("HA_CONFIG_INVALID")?)
            .map_err(|_| "HA_CONFIG_INVALID")?;
    validate_config(&v)?;
    Ok(v)
}
impl Scope {
    fn new(core: &Arc<Core>, connection: &str) -> Result<Self, String> {
        let lock = integrations::save_lock(core)?;
        let raw = record(core, connection)?;
        let s = Self {
            core: core.clone(),
            connection: connection.into(),
            config: config(&raw)?,
            credential_binding: credentials::binding(core, connection, &raw)?,
            raw,
            generation: core
                .store
                .setting(&format!("homeassistant.connection_generation.{connection}")),
            #[cfg(test)]
            fixture: core
                .homeassistant_fixture
                .lock()
                .map_err(|_| "HA_FIXTURE_LOCK")?
                .clone(),
        };
        drop(lock);
        s.check()?;
        Ok(s)
    }
    fn check(&self) -> Result<(), String> {
        if self.core.store.setting("privacy") == true {
            return Err("PRIVACY_DENIED".into());
        }
        if !self.core.enabled("homeassistant") {
            return Err("MODULE_DISABLED: homeassistant".into());
        }
        let _lock = integrations::save_lock(&self.core)?;
        if record(&self.core, &self.connection)? != self.raw
            || self.core.store.setting(&format!(
                "homeassistant.connection_generation.{}",
                self.connection
            )) != self.generation
        {
            return Err("HA_CONNECTION_CHANGED".into());
        }
        if credentials::binding(&self.core, &self.connection, &self.raw)? != self.credential_binding
        {
            return Err("HA_CREDENTIAL_BINDING_CHANGED".into());
        }
        if self.credential_binding["stage"] == "pending" {
            return Err("HA_CREDENTIAL_REENTRY_REQUIRED".into());
        }
        #[cfg(test)]
        if *self
            .core
            .homeassistant_fixture
            .lock()
            .map_err(|_| "HA_FIXTURE_LOCK")?
            != self.fixture
        {
            return Err("HA_FIXTURE_CHANGED".into());
        }
        Ok(())
    }
    fn require_entity(&self, id: &str) -> Result<(), String> {
        if !entity(id) || !self.config.allowed_entities.iter().any(|s| s == id) {
            return Err("HA_ENTITY_OUT_OF_SCOPE".into());
        }
        Ok(())
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
    async fn new(core: &Arc<Core>, id: &str) -> Result<Self, String> {
        let scope = Scope::new(core, id)?;
        #[cfg(test)]
        let token = scope.fixture.as_ref().and_then(|f| f.token.clone());
        #[cfg(not(test))]
        let token: Option<String> = None;
        let token = if let Some(t) = token {
            t
        } else {
            scope
                .guard(credentials::read_token(
                    core,
                    id,
                    &scope.raw,
                    &scope.credential_binding,
                ))
                .await?
        };
        validate_token(&token)?;
        let client = crate::http_client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| "HA_CLIENT")?;
        Ok(Self {
            base: origin(&scope.config)?,
            scope,
            client,
            token,
        })
    }
    fn content(&self, v: &Value) -> Value {
        fn scrub(v: &Value, token: &str) -> Value {
            match v {
                Value::Object(m) => Value::Object(
                    m.iter()
                        .filter(|(k, _)| {
                            ![
                                "access_token",
                                "token",
                                "password",
                                "authorization",
                                "entity_picture",
                                "code",
                                "pin",
                            ]
                            .contains(&k.as_str())
                        })
                        .map(|(k, v)| (k.clone(), scrub(v, token)))
                        .collect(),
                ),
                Value::Array(a) => Value::Array(a.iter().map(|v| scrub(v, token)).collect()),
                Value::String(s)
                    if s.contains(token) || s.to_ascii_lowercase().contains("bearer ") =>
                {
                    json!("[REDACTED]")
                }
                _ => v.clone(),
            }
        }
        crate::policy::redact(&scrub(v, &self.token))
    }
    async fn bytes(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        limit: usize,
    ) -> Result<Vec<u8>, String> {
        self.scope.check()?;
        let cooldown = self
            .scope
            .core
            .store
            .setting(&format!("homeassistant.cooldown.{}", self.scope.connection));
        if cooldown["manual"] == true
            || cooldown["until"]
                .as_i64()
                .is_some_and(|t| t > chrono::Utc::now().timestamp())
        {
            return Err("HA_RATE_LIMITED_MANUAL_RETRY".into());
        }
        let url = integrations::scoped_url(self.base.as_str(), path)?;
        let r = self
            .client
            .request(
                reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| "HA_METHOD")?,
                url,
            )
            .bearer_auth(&self.token);
        let r = if let Some(b) = body { r.json(b) } else { r };
        self.scope
            .guard(async {
                let response = r.send().await.map_err(|_| "HA_TRANSPORT_ERROR")?;
                let status = response.status().as_u16();
                if status == 429 {
                    let retry = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<i64>().ok())
                        .filter(|s| (1..=604800).contains(s));
                    self.scope.core.store.set_setting(
                        &format!("homeassistant.cooldown.{}", self.scope.connection),
                        &retry.map_or_else(
                            || json!({"manual":true}),
                            |t| json!({"until":chrono::Utc::now().timestamp()+t}),
                        ),
                    )?;
                }
                if !(200..300).contains(&status) {
                    return Err(format!("HA_HTTP_STATUS_{status}"));
                }
                integrations::bounded_body(response, limit)
                    .await
                    .map_err(|_| "HA_RESPONSE_LIMIT_OR_TRANSPORT".into())
            })
            .await
    }
    async fn get(&self, path: &str) -> Result<Value, String> {
        let b = self.bytes("GET", path, None, LIMIT).await?;
        serde_json::from_slice(&b).map_err(|_| "HA_MALFORMED_JSON".into())
    }
    async fn state(&self, id: &str) -> Result<Value, String> {
        self.scope.require_entity(id)?;
        let v = self.get(&format!("/api/states/{id}")).await?;
        if v["entity_id"] != id || !v["state"].is_string() || !v["attributes"].is_object() {
            return Err("HA_STATE_CONTRACT".into());
        }
        Ok(v)
    }
    async fn states(&self) -> Result<Value, String> {
        let v = self.get("/api/states").await?;
        let a = v.as_array().ok_or("HA_STATES_CONTRACT")?;
        if a.len() > 10000 {
            return Err("HA_STATES_LIMIT".into());
        }
        let mut result = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for state in a {
            let id = state["entity_id"].as_str().ok_or("HA_STATE_CONTRACT")?;
            if self.scope.config.allowed_entities.iter().any(|s| s == id) {
                if !seen.insert(id)
                    || !state["state"].is_string()
                    || !state["attributes"].is_object()
                {
                    return Err("HA_STATE_CONTRACT".into());
                }
                result.push(state.clone());
            }
        }
        Ok(json!(result))
    }
    async fn services(&self) -> Result<Value, String> {
        let v = self.get("/api/services").await?;
        let rows = v.as_array().ok_or("HA_SERVICES_CONTRACT")?;
        if rows.len() > 2000 {
            return Err("HA_SERVICES_LIMIT".into());
        }
        let mut out = serde_json::Map::new();
        for row in rows {
            let domain = row["domain"]
                .as_str()
                .filter(|s| atom(s))
                .ok_or("HA_SERVICES_CONTRACT")?;
            let services = row["services"].as_object().ok_or("HA_SERVICES_CONTRACT")?;
            for (name, spec) in services {
                let id = format!("{domain}.{name}");
                if self.scope.config.allowed_services.contains_key(&id)
                    && (!atom(name) || !spec.is_object() || out.insert(id, spec.clone()).is_some())
                {
                    return Err("HA_SERVICE_CONTRACT".into());
                }
            }
        }
        Ok(Value::Object(out))
    }
}
fn reject_targets(v: &Value, depth: usize) -> Result<(), String> {
    if depth > 16 {
        return Err("HA_BODY_DEPTH".into());
    }
    match v {
        Value::Object(m) => {
            for (k, v) in m {
                if ["entity_id", "area_id", "device_id", "label_id", "target"].contains(&k.as_str())
                {
                    return Err("HA_IMPLICIT_TARGET_DENIED".into());
                }
                reject_targets(v, depth + 1)?;
            }
        }
        Value::Array(a) => {
            for v in a {
                reject_targets(v, depth + 1)?;
            }
        }
        _ => (),
    }
    Ok(())
}
struct Call {
    domain: String,
    service: String,
    ids: Vec<String>,
    body: Value,
    return_response: bool,
}
impl Call {
    fn new(s: &Session, tool: &str, input: &Value) -> Result<Self, String> {
        let automation = tool == "homeassistant.automations";
        let domain = if automation {
            "automation"
        } else {
            input["domain"].as_str().ok_or("HA_DOMAIN")?
        };
        let service = input["service"].as_str().ok_or("HA_SERVICE")?;
        if !atom(domain)
            || !atom(service)
            || (automation
                && !["turn_on", "turn_off", "toggle", "trigger", "reload"].contains(&service))
        {
            return Err("HA_SERVICE".into());
        }
        let ids: Vec<String> = if automation {
            if service == "reload" {
                if input.get("entity_id").is_some() {
                    return Err("HA_RELOAD_IS_GLOBAL".into());
                }
                Vec::new()
            } else {
                vec![input["entity_id"]
                    .as_str()
                    .filter(|id| id.starts_with("automation."))
                    .ok_or("HA_AUTOMATION_ENTITY")?
                    .into()]
            }
        } else {
            serde_json::from_value(input["entity_ids"].clone()).map_err(|_| "HA_TARGETS")?
        };
        let grant = s
            .scope
            .config
            .allowed_services
            .get(&format!("{domain}.{service}"))
            .ok_or("HA_SERVICE_OUT_OF_SCOPE")?;
        if ids.is_empty() != grant.global {
            return Err("HA_EXPLICIT_TARGET_REQUIRED".into());
        }
        if ids.len() > 64
            || ids.iter().collect::<std::collections::BTreeSet<_>>().len() != ids.len()
        {
            return Err("HA_TARGETS".into());
        }
        for id in &ids {
            s.scope.require_entity(id)?;
        }
        let body = input.get("body").cloned().unwrap_or_else(|| json!({}));
        if body.to_string().len() > BODY_LIMIT {
            return Err("HA_BODY_LIMIT".into());
        }
        reject_targets(&body, 0)?;
        if !jsonschema::validator_for(&grant.input_schema)
            .map_err(|_| "HA_SERVICE_SCHEMA")?
            .is_valid(&body)
        {
            return Err("HA_SERVICE_INPUT_INVALID".into());
        }
        Ok(Self {
            domain: domain.into(),
            service: service.into(),
            ids,
            body,
            return_response: input["return_response"].as_bool().unwrap_or(false),
        })
    }
    async fn observe(&self, s: &Session) -> Result<Value, String> {
        let services = s.services().await?;
        let spec = services
            .get(format!("{}.{}", self.domain, self.service))
            .ok_or("HA_SERVICE_UNAVAILABLE")?;
        let mut states = Vec::new();
        for id in &self.ids {
            states.push(s.state(id).await?);
        }
        let observation = json!({"connection":s.scope.connection,"profile":s.scope.raw,"generation":s.scope.generation,"credential_binding":s.scope.credential_binding,
            "domain":self.domain,"service":self.service,"entity_ids":self.ids,"body":self.body,"return_response":self.return_response,"service_spec":spec,"states":states});
        Ok(
            json!({"fingerprint":digest(&observation),"states":s.content(&json!(states)),"service_spec":s.content(spec)}),
        )
    }
}
pub(crate) struct Prepared {
    session: Option<Session>,
    local: Option<homeassistant_ws::LocalControl>,
    tool: String,
    input: Value,
    call: Option<Call>,
    observation: Value,
    pub risk: Risk,
    asset_policy: Value,
}
impl Prepared {
    pub(crate) fn review(&self) -> Value {
        if let Some(local) = &self.local {
            return local.review();
        }
        let Some(session) = &self.session else {
            return json!({"error":"HA_PREPARED_SESSION"});
        };
        json!({"connection":session.scope.connection,"origin":session.base.as_str(),"insecure_http":session.base.scheme()=="http",
            "action":self.input["action"],"entity_id":self.input["entity_id"],"event_type":self.input["event_type"],"stream_id":self.input["stream_id"],
            "domain":self.call.as_ref().map(|c|&c.domain),"service":self.call.as_ref().map(|c|&c.service),"entity_ids":self.call.as_ref().map(|c|&c.ids),
            "body":self.call.as_ref().map(|c|session.content(&c.body)),"return_response":self.call.as_ref().map(|c|c.return_response),"observation":self.observation,"asset_policy":self.asset_policy,
            "physical_outcome":"UNVERIFIED","remote_undo":false,"automatic_replay":false})
    }
}
pub(crate) async fn prepare(
    core: &Arc<Core>,
    tool: &str,
    input: &Value,
) -> Result<Prepared, String> {
    if !TOOLS.contains(&tool)
        || !jsonschema::validator_for(&schema(tool))
            .map_err(|_| "HA_SCHEMA")?
            .is_valid(input)
    {
        return Err("HA_INPUT".into());
    }
    if tool == "homeassistant.websocket" && input["action"] != "start" {
        let local = homeassistant_ws::LocalControl::new(core, input)?;
        return Ok(Prepared {
            session: None,
            local: Some(local),
            tool: tool.into(),
            input: input.clone(),
            call: None,
            observation: Value::Null,
            risk: Risk::Free,
            asset_policy: Value::Null,
        });
    }
    let s = Session::new(core, input["connection"].as_str().ok_or("HA_CONNECTION")?).await?;
    let action = input["action"].as_str().unwrap_or("list");
    let is_call = tool == "homeassistant.call_service"
        || (tool == "homeassistant.automations" && action != "list");
    let call = if is_call {
        Some(Call::new(&s, tool, input)?)
    } else {
        None
    };
    let observation = if let Some(c) = &call {
        c.observe(&s).await?
    } else {
        Value::Null
    };
    let risk = if call.is_some() && action == "call" {
        if s.scope.config.read_only {
            return Err("HA_READ_ONLY".into());
        }
        if input["expected"]["fingerprint"] != observation["fingerprint"] {
            return Err("HA_OBSERVATION_CHANGED_REVIEW_REQUIRED".into());
        }
        Risk::Mandatory
    } else if tool == "homeassistant.camera_snapshot"
        || (tool == "homeassistant.websocket" && action == "start")
    {
        Risk::Contextual
    } else {
        Risk::Free
    };
    if tool == "homeassistant.camera_snapshot" {
        let id = input["entity_id"].as_str().ok_or("HA_CAMERA")?;
        if !id.starts_with("camera.") {
            return Err("HA_CAMERA_ENTITY".into());
        }
        s.scope.require_entity(id)?;
    }
    if tool == "homeassistant.websocket" {
        homeassistant_ws::validate(&s, input)?;
    }
    let asset_policy = if tool == "homeassistant.camera_snapshot" {
        json!(crate::assets::config(core)?)
    } else {
        Value::Null
    };
    Ok(Prepared {
        session: Some(s),
        local: None,
        tool: tool.into(),
        input: input.clone(),
        call,
        observation,
        risk,
        asset_policy,
    })
}
pub(crate) async fn execute(p: Prepared) -> Result<Value, String> {
    if let Some(local) = &p.local {
        return local.execute();
    }
    let s = p.session.as_ref().ok_or("HA_PREPARED_SESSION")?;
    s.scope.check()?;
    if let Some(call) = &p.call {
        if p.input["action"] == "inspect" {
            return Ok(
                json!({"expected":{"fingerprint":p.observation["fingerprint"]},"review":p.review(),"untrusted":true}),
            );
        }
        let _serial = s
            .scope
            .guard(async { Ok(s.scope.core.homeassistant_actions.lock().await) })
            .await?;
        if call.observe(s).await? != p.observation {
            return Err("HA_OBSERVATION_CHANGED_REVIEW_REQUIRED".into());
        }
        s.scope.check()?;
        let mut body = call.body.clone();
        if !call.ids.is_empty() {
            body["entity_id"] = json!(call.ids);
        }
        let bytes = s
            .bytes(
                "POST",
                &format!(
                    "/api/services/{}/{}{}",
                    call.domain,
                    call.service,
                    if call.return_response {
                        "?return_response"
                    } else {
                        ""
                    }
                ),
                Some(&body),
                LIMIT,
            )
            .await
            .map_err(|e| format!("HA_ACTION_OUTCOME_UNCERTAIN: {e}"))?;
        let returned: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "HA_ACTION_OUTCOME_UNCERTAIN: HA_MALFORMED_JSON")?;
        let mut observed = Vec::new();
        let mut errors = Vec::new();
        for id in &call.ids {
            match s.state(id).await {
                Ok(v) => observed.push(s.content(&v)),
                Err(e) => errors.push(json!({"entity_id":id,"error":e})),
            }
        }
        let changes = if call.return_response {
            &returned["changed_states"]
        } else {
            &returned
        };
        let service_response = if call.return_response {
            if !returned.is_object() || returned.get("service_response").is_none() {
                return Err("HA_ACTION_OUTCOME_UNCERTAIN: HA_SERVICE_RESPONSE_CONTRACT".into());
            }
            s.content(&returned["service_response"])
        } else {
            Value::Null
        };
        let returned = if let Some(rows) = changes.as_array() {
            Value::Array(
                rows.iter()
                    .filter(|v| {
                        v["entity_id"].as_str().is_some_and(|id| {
                            s.scope.config.allowed_entities.iter().any(|s| s == id)
                        })
                    })
                    .map(|v| s.content(v))
                    .collect(),
            )
        } else {
            return Err("HA_ACTION_OUTCOME_UNCERTAIN: HA_SERVICE_RESPONSE_CONTRACT".into());
        };
        return Ok(
            json!({"acknowledged":true,"physical_outcome":"UNVERIFIED","states":observed,"observation_errors":errors,"reported_changes":returned,"service_response":service_response,"remote_undo":false,"automatic_replay":false,"untrusted":true}),
        );
    }
    match p.tool.as_str() {
        "homeassistant.entities" | "homeassistant.states" | "homeassistant.automations" => {
            let v = if let Some(id) = p.input["entity_id"].as_str() {
                json!([s.state(id).await?])
            } else {
                s.states().await?
            };
            let v = if p.tool == "homeassistant.automations" {
                json!(v
                    .as_array()
                    .ok_or("HA_STATES_CONTRACT")?
                    .iter()
                    .filter(|v| v["entity_id"]
                        .as_str()
                        .is_some_and(|id| id.starts_with("automation.")))
                    .collect::<Vec<_>>())
            } else {
                v
            };
            Ok(
                json!({"items":s.content(&v),"untrusted":true,"snapshot_complete_within_response_limit":true,"scope":"configured_entities"}),
            )
        }
        "homeassistant.services" => {
            Ok(json!({"services":s.content(&s.services().await?),"untrusted":true}))
        }
        "homeassistant.events" => {
            let v = s.get("/api/events").await?;
            let a = v.as_array().ok_or("HA_EVENTS_CONTRACT")?;
            if a.len() > 10000 {
                return Err("HA_EVENTS_LIMIT".into());
            }
            let v: Vec<_> = a
                .iter()
                .filter(|v| {
                    v["event"]
                        .as_str()
                        .is_some_and(|e| s.scope.config.allowed_events.iter().any(|s| s == e))
                })
                .collect();
            Ok(
                json!({"items":s.content(&json!(v)),"kind":"catalogue","live_stream":false,"untrusted":true}),
            )
        }
        "homeassistant.camera_snapshot" => {
            let id = p.input["entity_id"].as_str().ok_or("HA_CAMERA")?;
            let bytes = s
                .bytes(
                    "GET",
                    &format!("/api/camera_proxy/{id}"),
                    None,
                    8 * 1024 * 1024,
                )
                .await?;
            s.scope.check()?;
            let result = crate::assets::remote_image(
                &s.scope.core,
                "homeassistant",
                &bytes,
                &format!("Home Assistant {id}"),
                &p.asset_policy,
                || s.scope.check(),
            )
            .await?;
            s.scope.check()?;
            Ok(result)
        }
        "homeassistant.websocket" => homeassistant_ws::execute(p).await,
        _ => Err("HA_TOOL".into()),
    }
}
pub(crate) fn timeout_error(tool: &str, input: &Value) -> String {
    if ["homeassistant.call_service", "homeassistant.automations"].contains(&tool)
        && input["action"] == "call"
    {
        "HA_ACTION_OUTCOME_UNCERTAIN: TOOL_TIMEOUT".into()
    } else {
        "HA_TOOL_TIMEOUT".into()
    }
}

#[cfg(test)]
#[path = "homeassistant_tests.rs"]
mod homeassistant_tests;
