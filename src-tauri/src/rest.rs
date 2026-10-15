//! Prepared, finite REST actions. No caller-supplied URL, method or credential
//! target can bypass the saved profile. Provider content is untrusted.
use crate::{
    integrations, policy::Risk, rest_credentials as credentials, rest_openapi,
    rest_profile as profile, rest_save, runtime::Core,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{future::Future, sync::Arc, time::Duration};

pub(crate) const TOOLS: &[&str] = &["rest.request", "rest.import_openapi"];
pub(crate) fn digest(v: &Value) -> String {
    format!("{:x}", Sha256::digest(v.to_string().as_bytes()))
}
#[cfg(test)]
#[derive(Clone, PartialEq)]
pub(crate) struct TestTransport {
    pub headless_permission: bool,
    pub http2_prior_knowledge: bool,
}
pub(crate) fn schema(tool: &str) -> Value {
    let expected = json!({"type":"object","properties":{"fingerprint":{"type":"string","pattern":"^[a-f0-9]{64}$"}},"required":["fingerprint"],"additionalProperties":false});
    let props = if tool == "rest.request" {
        json!({"connection":{"type":"string","format":"uuid"},"action":{"enum":["inspect","call"]},"request":{"type":"object","properties":{"operation":{"type":"string","minLength":1,"maxLength":64},"path":{"type":"object"},"query":{"type":"object"},"body":{}},"required":["operation"],"additionalProperties":false},"expected":expected})
    } else {
        json!({"connection":{"type":"string","format":"uuid"},"action":{"enum":["preview","apply"]},"document":{"type":"object"},"operations":{"type":"array","items":{"type":"string","minLength":1,"maxLength":64},"minItems":1,"maxItems":128,"uniqueItems":true},"expected":expected})
    };
    json!({"type":"object","properties":props,"required":if tool=="rest.request"{vec!["connection","action","request"]}else{vec!["connection","action"]},"additionalProperties":false})
}
struct Scope {
    core: Arc<Core>,
    id: String,
    raw: Value,
    config: profile::Config,
    generation: Value,
    binding: credentials::Binding,
    #[cfg(test)]
    fixture: Option<TestTransport>,
}
fn record(core: &Core, id: &str) -> Result<(Value, String), String> {
    credentials::canonical(id)?;
    let rows=core.store.query("SELECT id,module_id,name,config_json,secret_ref FROM connections WHERE id=? COLLATE NOCASE",&[json!(id)])?;
    if rows.as_array().is_none_or(|a| a.len() != 1)
        || rows[0]["id"] != id
        || rows[0]["module_id"] != "rest"
        || rows[0]["secret_ref"] != credentials::alias(id)?
    {
        return Err("REST_CONNECTION_ALIAS_CONFLICT".into());
    }
    let raw: Value = serde_json::from_str(rows[0]["config_json"].as_str().ok_or("REST_CONFIG")?)
        .map_err(|_| "REST_CONFIG")?;
    profile::config(&raw)?;
    Ok((
        raw,
        rows[0]["name"]
            .as_str()
            .ok_or("REST_CONNECTION_NAME")?
            .into(),
    ))
}
impl Scope {
    fn new(core: &Arc<Core>, id: &str) -> Result<Self, String> {
        let lock = integrations::save_lock(core)?;
        let (raw, _) = record(core, id)?;
        let config = profile::config(&raw)?;
        let s = Self {
            core: core.clone(),
            id: id.into(),
            binding: rest_save::binding(core, id, &config)?,
            config,
            raw,
            generation: core.store.setting(&rest_save::generation_key(id)),
            #[cfg(test)]
            fixture: core
                .rest_fixture
                .lock()
                .map_err(|_| "REST_FIXTURE_LOCK")?
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
        if !self.core.enabled("rest") {
            return Err("MODULE_DISABLED: rest".into());
        }
        let _lock = integrations::save_lock(&self.core)?;
        if record(&self.core, &self.id)?.0 != self.raw
            || self
                .core
                .store
                .setting(&rest_save::generation_key(&self.id))
                != self.generation
            || self
                .generation
                .as_str()
                .is_none_or(|s| credentials::canonical(s).is_err())
        {
            return Err("REST_CONNECTION_CHANGED".into());
        }
        if rest_save::binding(&self.core, &self.id, &self.config)? != self.binding {
            return Err("REST_CREDENTIAL_BINDING_CHANGED".into());
        }
        if self.binding.stage == credentials::Stage::Pending {
            return Err("REST_CREDENTIAL_REENTRY_REQUIRED".into());
        }
        #[cfg(test)]
        if *self
            .core
            .rest_fixture
            .lock()
            .map_err(|_| "REST_FIXTURE_LOCK")?
            != self.fixture
        {
            return Err("REST_FIXTURE_CHANGED".into());
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
    fn observation(&self, subject: &Value) -> Value {
        json!({"fingerprint":digest(&json!({"connection":self.id,"profile":self.raw,"generation":self.generation,"binding":self.binding,"subject":subject}))})
    }
}
struct Session<'a> {
    scope: &'a Scope,
    client: reqwest::Client,
    headers: reqwest::header::HeaderMap,
    needles: Vec<String>,
}
impl<'a> Session<'a> {
    fn contains_private(&self, value: &Value) -> bool {
        let matched = |s: &str| self.needles.iter().any(|n| !n.is_empty() && s.contains(n));
        match value {
            Value::String(s) => matched(s),
            Value::Object(m) => m
                .iter()
                .any(|(k, v)| matched(k) || self.contains_private(v)),
            Value::Array(a) => a.iter().any(|v| self.contains_private(v)),
            _ => false,
        }
    }
    async fn new(scope: &'a Scope) -> Result<Self, String> {
        scope.check()?;
        if !profile::required_secrets(&scope.config)?.is_empty()
            && scope.binding.stage != credentials::Stage::Ready
        {
            return Err("REST_CREDENTIAL_REENTRY_REQUIRED".into());
        }
        let private = if profile::required_secrets(&scope.config)?.is_empty() {
            None
        } else {
            Some(
                scope
                    .guard(rest_save::native_read(&scope.core, &scope.id))
                    .await?,
            )
        };
        let secrets =
            credentials::read(&scope.id, &scope.config, &scope.binding, private.as_deref())?;
        let mut needles = secrets
            .values()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<_>>();
        let builder = crate::http_client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20));
        #[cfg(test)]
        let builder = if scope
            .fixture
            .as_ref()
            .is_some_and(|f| f.http2_prior_knowledge)
        {
            builder.http2_prior_knowledge()
        } else {
            builder
        };
        let client = builder.build().map_err(|_| "REST_CLIENT")?;
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, h) in &scope.config.headers {
            let value = h
                .value
                .as_ref()
                .or_else(|| h.secret_ref.as_ref().and_then(|r| secrets.get(r)))
                .ok_or("REST_HEADER_REFERENCE")?;
            headers.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| "REST_HEADER_NAME")?,
                reqwest::header::HeaderValue::from_str(value).map_err(|_| "REST_HEADER_VALUE")?,
            );
        }
        match &scope.config.auth {
            profile::Auth::None => (),
            profile::Auth::Bearer { secret_ref } => {
                headers.insert(
                    reqwest::header::AUTHORIZATION,
                    reqwest::header::HeaderValue::from_str(&format!(
                        "Bearer {}",
                        secrets.get(secret_ref).ok_or("REST_AUTH_REFERENCE")?
                    ))
                    .map_err(|_| "REST_AUTH_VALUE")?,
                );
            }
            profile::Auth::Basic {
                username,
                secret_ref,
            } => {
                let r = client
                    .get(profile::base(&scope.config)?)
                    .basic_auth(
                        username,
                        Some(secrets.get(secret_ref).ok_or("REST_AUTH_REFERENCE")?),
                    )
                    .build()
                    .map_err(|_| "REST_AUTH_VALUE")?;
                headers.insert(
                    reqwest::header::AUTHORIZATION,
                    r.headers()
                        .get(reqwest::header::AUTHORIZATION)
                        .ok_or("REST_AUTH_VALUE")?
                        .clone(),
                );
            }
            profile::Auth::Header { header, secret_ref } => {
                headers.insert(
                    reqwest::header::HeaderName::from_bytes(header.as_bytes())
                        .map_err(|_| "REST_AUTH_HEADER")?,
                    reqwest::header::HeaderValue::from_str(
                        secrets.get(secret_ref).ok_or("REST_AUTH_REFERENCE")?,
                    )
                    .map_err(|_| "REST_AUTH_VALUE")?,
                );
            }
        }
        for value in headers.values_mut() {
            value.set_sensitive(true);
        }
        if let Some(h) = headers.get(reqwest::header::AUTHORIZATION) {
            if let Ok(s) = h.to_str() {
                if !s.is_empty() {
                    needles.push(s.into());
                }
            }
        }
        scope.check()?;
        Ok(Self {
            scope,
            client,
            headers,
            needles,
        })
    }
    fn content(&self, value: &Value) -> Value {
        fn scrub(v: &Value, needles: &[String]) -> Value {
            let text = |s: &str| {
                if needles.iter().any(|n| !n.is_empty() && s.contains(n))
                    || s.to_ascii_lowercase().contains("bearer ")
                {
                    "[REDACTED]".into()
                } else {
                    s.to_string()
                }
            };
            match v {
                Value::String(s) => json!(text(s)),
                Value::Array(a) => json!(a.iter().map(|v| scrub(v, needles)).collect::<Vec<_>>()),
                Value::Object(m) => Value::Object(
                    m.iter()
                        .map(|(k, v)| (text(k), scrub(v, needles)))
                        .collect(),
                ),
                _ => v.clone(),
            }
        }
        crate::policy::redact(&scrub(value, &self.needles))
    }
    async fn bytes(
        &self,
        method: &str,
        url: reqwest::Url,
        body: Option<&Value>,
        media: Option<&str>,
        limit: usize,
        dispatched: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<(u16, Option<String>, Vec<u8>), String> {
        self.scope.check()?;
        let cooldown = self
            .scope
            .core
            .store
            .setting(&rest_save::cooldown_key(&self.scope.id));
        if cooldown["manual"] == true
            || cooldown["until"]
                .as_i64()
                .is_some_and(|s| s > chrono::Utc::now().timestamp())
        {
            return Err("REST_RATE_LIMITED_MANUAL_RETRY".into());
        }
        let mut r = self
            .client
            .request(
                reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| "REST_METHOD")?,
                url,
            )
            .headers(self.headers.clone());
        if let Some(body) = body {
            r = r
                .header(
                    reqwest::header::CONTENT_TYPE,
                    media.unwrap_or("application/json"),
                )
                .body(body.to_string());
        }
        self.scope
            .guard(async {
                if let Some(dispatched) = dispatched {
                    dispatched.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                let response = r.send().await.map_err(|_| "REST_TRANSPORT_ERROR")?;
                let status = response.status().as_u16();
                if status == 429 {
                    let retry = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<i64>().ok())
                        .filter(|s| (1..=604800).contains(s));
                    self.scope.core.store.set_setting(
                        &rest_save::cooldown_key(&self.scope.id),
                        &retry.map_or_else(
                            || json!({"manual":true}),
                            |t| json!({"until":chrono::Utc::now().timestamp()+t}),
                        ),
                    )?;
                }
                if !(200..300).contains(&status) {
                    return Err(format!("REST_HTTP_STATUS_{status}"));
                }
                let content_type = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(|s| {
                        s.split(';')
                            .next()
                            .unwrap_or("")
                            .trim()
                            .to_ascii_lowercase()
                    });
                let bytes = integrations::bounded_body(response, limit)
                    .await
                    .map_err(|_| "REST_RESPONSE_LIMIT_OR_TRANSPORT")?;
                Ok((status, content_type, bytes))
            })
            .await
    }
}
enum Action {
    Request {
        plan: profile::Plan,
        inspect: bool,
    },
    Import {
        preview: Box<rest_openapi::Preview>,
        document: Value,
        source: Option<reqwest::Url>,
        selected: Option<Box<profile::Config>>,
    },
}
pub(crate) struct Prepared {
    scope: Scope,
    action: Action,
    expected: Value,
    pub risk: Risk,
}
impl Prepared {
    pub(crate) fn review(&self) -> Value {
        let detail = match &self.action {
            Action::Request { plan, .. } => {
                json!({"operation":plan.operation,"method":plan.method,"url":plan.url.as_str(),"body":crate::policy::redact(&json!(plan.body)),"operation_contract":self.scope.config.operations[&plan.operation]})
            }
            Action::Import {
                preview, selected, ..
            } => {
                json!({"action":if selected.is_some(){"apply"}else{"preview"},"version":preview.version,"proposed":preview.operations_proposed,"diagnostics":preview.diagnostics,"selected_profile":selected,"replaces_operations":true,"read_only_after_apply":true})
            }
        };
        crate::policy::redact(
            &json!({"connection":self.scope.id,"base_url":self.scope.config.base_url,"detail":detail,"expected":self.expected,"remote_undo":false,"automatic_replay":false,"untrusted":true}),
        )
    }
}
async fn document(scope: &Scope, input: &Value) -> Result<(Value, Option<reqwest::Url>), String> {
    if let Some(v) = input.get("document") {
        if v.to_string().len() > 2 * 1024 * 1024 {
            return Err("REST_OPENAPI_DOCUMENT_LIMIT".into());
        }
        return Ok((v.clone(), None));
    }
    let path = scope
        .config
        .openapi_path
        .as_deref()
        .ok_or("REST_OPENAPI_PATH_REQUIRED")?;
    let base = profile::base(&scope.config)?;
    let url = base
        .join(path.trim_start_matches('/'))
        .map_err(|_| "REST_OPENAPI_PATH")?;
    if url.origin() != base.origin() || !url.path().starts_with(base.path()) {
        return Err("REST_OPENAPI_SCOPE".into());
    }
    let session = Session::new(scope).await?;
    let (_, media, bytes) = session
        .bytes("GET", url.clone(), None, None, 2 * 1024 * 1024, None)
        .await?;
    if media.as_deref().is_none_or(|s| !profile::json_media(s)) {
        return Err("REST_OPENAPI_CONTENT_TYPE".into());
    }
    let v: Value = serde_json::from_slice(&bytes).map_err(|_| "REST_OPENAPI_JSON")?;
    if session.contains_private(&v) {
        return Err("REST_OPENAPI_CREDENTIAL_CONTENT_DENIED".into());
    }
    Ok((v, Some(url)))
}
pub(crate) async fn prepare(
    core: &Arc<Core>,
    tool: &str,
    input: &Value,
) -> Result<Prepared, String> {
    if !TOOLS.contains(&tool)
        || !jsonschema::validator_for(&schema(tool))
            .map_err(|_| "REST_TOOL_SCHEMA")?
            .is_valid(input)
    {
        return Err("REST_INPUT".into());
    }
    let scope = Scope::new(core, input["connection"].as_str().ok_or("REST_CONNECTION")?)?;
    let (action, expected, risk) = if tool == "rest.request" {
        let plan = profile::plan(&scope.config, &input["request"])?;
        let expected = scope.observation(
            &json!({"request":input["request"],"method":plan.method,"url":plan.url.as_str()}),
        );
        let inspect = input["action"] == "inspect";
        if !inspect && input["expected"] != expected {
            return Err("REST_OBSERVATION_CHANGED_REVIEW_REQUIRED".into());
        }
        if input.get("operations").is_some() || input.get("document").is_some() {
            return Err("REST_INPUT".into());
        }
        let risk = if inspect {
            Risk::Free
        } else {
            match plan.risk {
                profile::Risk::Free => Risk::Free,
                profile::Risk::Contextual => Risk::Contextual,
                profile::Risk::Mandatory => Risk::Mandatory,
            }
        };
        (Action::Request { plan, inspect }, expected, risk)
    } else {
        let (doc, source) = document(&scope, input).await?;
        let preview = rest_openapi::preview(&doc, &scope.config, source.as_ref())?;
        let expected =
            scope.observation(&json!({"document":doc,"source":source.as_ref().map(|s|s.as_str())}));
        let apply = input["action"] == "apply";
        if !apply && (input.get("operations").is_some() || input.get("expected").is_some()) {
            return Err("REST_PREVIEW_INPUT".into());
        }
        let selected = if apply {
            if input["expected"] != expected {
                return Err("REST_OPENAPI_OBSERVATION_CHANGED_REVIEW_REQUIRED".into());
            }
            let ids: Vec<String> = serde_json::from_value(input["operations"].clone())
                .map_err(|_| "REST_OPENAPI_SELECTION_REQUIRED")?;
            Some(Box::new(rest_openapi::select(&preview, &ids)?))
        } else {
            None
        };
        (
            Action::Import {
                preview: Box::new(preview),
                document: doc,
                source,
                selected,
            },
            expected,
            if apply { Risk::Mandatory } else { Risk::Free },
        )
    };
    scope.check()?;
    Ok(Prepared {
        scope,
        action,
        expected,
        risk,
    })
}
pub(crate) async fn execute(p: Prepared) -> Result<Value, String> {
    p.scope.check()?;
    match &p.action {
        Action::Request { inspect: true, .. } => {
            Ok(json!({"expected":p.expected,"review":p.review(),"untrusted":true}))
        }
        Action::Request { plan, .. } => {
            let _serial = p
                .scope
                .guard(async { Ok(p.scope.core.rest_actions.lock().await) })
                .await?;
            let s = Session::new(&p.scope).await?;
            let op = &p.scope.config.operations[&plan.operation];
            let dispatched = std::sync::atomic::AtomicBool::new(false);
            // Only failures after dispatch begins have an uncertain remote outcome.
            let result=async {
                let (status,media,bytes)=s.bytes(&plan.method,plan.url.clone(),plan.body.as_ref(),op.request_content_type.as_deref(),2*1024*1024,Some(&dispatched)).await?;
                let spec=profile::response_spec(op,status)?;
                let value=match spec.response {
                    profile::ResponseKind::Empty=> {if !bytes.is_empty(){return Err("REST_EMPTY_RESPONSE_CONTRACT".into());} Value::Null},
                    profile::ResponseKind::Text=> {
                        if media.as_deref()!=Some(spec.content_type.as_deref().unwrap_or("text/plain")) {return Err("REST_RESPONSE_CONTENT_TYPE".into());}
                        json!(String::from_utf8(bytes).map_err(|_|"REST_RESPONSE_UTF8")?)
                    },
                    profile::ResponseKind::Json=> {
                        if media.as_deref().is_none_or(|s|!profile::json_media(s)) || spec.content_type.as_ref().is_some_and(|t|media.as_ref()!=Some(t)) {return Err("REST_RESPONSE_CONTENT_TYPE".into());}
                        let v: Value=serde_json::from_slice(&bytes).map_err(|_|"REST_RESPONSE_JSON")?;
                        if let Some(schema)=&spec.schema { if !profile::validator(schema)?.is_valid(&v) {return Err("REST_RESPONSE_SCHEMA".into());} }
                        v
                    }
                };
                p.scope.check()?;
                Ok(json!({"connection":p.scope.id,"operation":plan.operation,"status":status,"data":s.content(&value),"untrusted":true,"remote_undo":false,"automatic_replay":false}))
            }.await;
            result.map_err(|e: String| {
                if plan.mutation && dispatched.load(std::sync::atomic::Ordering::SeqCst) {
                    format!("REST_ACTION_OUTCOME_UNCERTAIN: {e}")
                } else {
                    e
                }
            })
        }
        Action::Import {
            preview,
            selected: None,
            ..
        } => Ok(json!({"expected":p.expected,"preview":preview,"untrusted":true})),
        Action::Import {
            document: doc,
            source,
            selected: Some(config),
            ..
        } => {
            let _serial = p
                .scope
                .guard(async { Ok(p.scope.core.rest_actions.lock().await) })
                .await?;
            if source.is_some() {
                let (fresh, fresh_source) = document(&p.scope, &json!({})).await?;
                if fresh != *doc || fresh_source != *source {
                    return Err("REST_OPENAPI_OBSERVATION_CHANGED_REVIEW_REQUIRED".into());
                }
            }
            p.scope.check()?;
            let _save = integrations::save_lock(&p.scope.core)?;
            let (_, name) = record(&p.scope.core, &p.scope.id)?;
            let expected = rest_save::Expected {
                raw: p.scope.raw.clone(),
                generation: p.scope.generation.clone(),
                binding: json!(p.scope.binding),
            };
            let result = rest_save::save_connection(
                &p.scope.core,
                &p.scope.id,
                &name,
                &json!(config),
                None,
                true,
                Some(&expected),
            )
            .await?;
            Ok(
                json!({"saved":result,"operations":config.operations.keys().collect::<Vec<_>>(),"read_only":true,"automatically_enabled":false}),
            )
        }
    }
}
pub(crate) fn timeout_error(tool: &str, input: &Value) -> String {
    if tool == "rest.request" && input["action"] == "call" {
        "REST_ACTION_OUTCOME_UNCERTAIN: TOOL_TIMEOUT".into()
    } else if input["action"] == "apply" {
        "REST_LOCAL_APPLY_OUTCOME_UNCERTAIN: TOOL_TIMEOUT".into()
    } else {
        "REST_TOOL_TIMEOUT".into()
    }
}
#[cfg(all(test, windows))]
#[path = "rest_tests.rs"]
mod tests;
