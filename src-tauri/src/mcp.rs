//! Prepared MCP authority. Discovery describes the server; only a saved local
//! profile, a fresh owned observation and individual Core permission authorize it.
use crate::{
    integrations, mcp_client as client, mcp_credentials as credentials, mcp_profile as profile,
    mcp_save, policy::Risk, runtime::Core,
};
#[path = "mcp_catalog.rs"]
mod catalog;
#[path = "mcp_listen.rs"]
mod listen;
pub(crate) use listen::{
    available as listeners_available, local as listen_local, passive as listen_passive,
    prepare_control as prepare_listen_control, stop_all as stop_listeners, Stream,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::File,
    future::Future,
    io::Read,
    path::PathBuf,
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};

pub(crate) const TOOLS: &[&str] = &[
    "mcp.health",
    "mcp.listen",
    "mcp.listen_status",
    "mcp.listen_poll",
    "mcp.listen_stop",
    "mcp.oauth_discover",
    "mcp.oauth_registration",
    "mcp.oauth_start",
    "mcp.oauth_status",
    "mcp.oauth_cancel",
    "mcp.oauth_refresh",
    "mcp.oauth_recover",
    "mcp.oauth_cleanup",
    "mcp.oauth_reset",
    "mcp.tools",
    "mcp.resources",
    "mcp.prompts",
    "mcp.call",
    "mcp.read_resource",
    "mcp.get_prompt",
];
fn digest(v: &Value) -> String {
    format!("{:x}", Sha256::digest(v.to_string().as_bytes()))
}
#[derive(Clone)]
pub(crate) struct Observation {
    scope: String,
    action: String,
    subject: String,
    definition: Value,
    version: String,
    catalog: Option<catalog::Fence>,
    expires: Instant,
}
pub(crate) fn revoke(core: &Core) -> Result<(), String> {
    listen::stop_all(core);
    core.mcp_epoch.fetch_add(1, Ordering::SeqCst);
    // Durable fencing also invalidates cooperating actors in another process.
    core.store
        .set_setting(crate::mcp_auth::AUTHORITY_KEY, &json!(crate::storage::id()))?;
    core.mcp_observations
        .lock()
        .map_err(|_| "MCP_OBSERVATION_LOCK")?
        .clear();
    Ok(())
}
struct Program {
    path: PathBuf,
    identity: String,
    sha256: String,
    _file: File,
    _parents: Vec<File>,
}
impl Program {
    fn new(path: PathBuf, budget: &mut u64) -> Result<Self, String> {
        if !path.is_absolute() {
            return Err("MCP_PROGRAM_ABSOLUTE_PATH_REQUIRED".into());
        }
        let mut parents = vec![];
        for parent in path.ancestors().skip(1) {
            parents.push(crate::asset_files::directory(parent).map_err(|_| "MCP_PROGRAM_PARENT")?);
        }
        let file = crate::asset_files::open(&path, false).map_err(|_| "MCP_PROGRAM_LOCK")?;
        let identity = crate::asset_files::identity(&file).map_err(|_| "MCP_PROGRAM_IDENTITY")?;
        let length = file.metadata().map_err(|_| "MCP_PROGRAM_LIMIT")?.len();
        *budget = budget.checked_sub(length).ok_or("MCP_PROGRAM_LIMIT")?;
        let reader = file
            .try_clone()
            .map_err(|_| "MCP_PROGRAM_READ")?
            .take(length + 1);
        let sha256 = crate::mcp_program_hash::hash(reader, length)?;
        Ok(Self {
            path,
            identity,
            sha256,
            _file: file,
            _parents: parents,
        })
    }
    fn check(&self) -> Result<(), String> {
        let file =
            crate::asset_files::open(&self.path, false).map_err(|_| "MCP_PROGRAM_CHANGED")?;
        if crate::asset_files::identity(&file).map_err(|_| "MCP_PROGRAM_CHANGED")? != self.identity
        {
            return Err("MCP_PROGRAM_CHANGED".into());
        }
        Ok(())
    }
}
struct Scope {
    core: Arc<Core>,
    id: String,
    name: String,
    raw: Value,
    config: profile::Config,
    generation: Value,
    binding: credentials::Binding,
    epoch: u64,
    authority_generation: Value,
    programs: Vec<Program>,
}
fn record(core: &Core, id: &str) -> Result<(Value, String), String> {
    credentials::canonical(id)?;
    let rows=core.store.query("SELECT id,module_id,name,config_json,secret_ref FROM connections WHERE id=? COLLATE NOCASE",&[json!(id)])?;
    if rows.as_array().is_none_or(|a| a.len() != 1)
        || rows[0]["id"] != id
        || rows[0]["module_id"] != "mcp"
        || rows[0]["secret_ref"] != credentials::alias(id)?
    {
        return Err("MCP_CONNECTION_ALIAS_CONFLICT".into());
    }
    let raw = serde_json::from_str(rows[0]["config_json"].as_str().ok_or("MCP_CONFIG")?)
        .map_err(|_| "MCP_CONFIG")?;
    profile::config(&raw)?;
    Ok((
        raw,
        rows[0]["name"]
            .as_str()
            .ok_or("MCP_CONNECTION_NAME")?
            .into(),
    ))
}
impl Scope {
    fn capture(core: &Arc<Core>, id: &str) -> Result<Self, String> {
        let lock = integrations::save_lock(core)?;
        let (raw, name) = record(core, id)?;
        let config = profile::config(&raw)?;
        let binding = mcp_save::binding(core, id, &config)?;
        let generation = core.store.setting(&mcp_save::generation_key(id));
        let epoch = core.mcp_epoch.load(Ordering::SeqCst);
        let authority_generation = core.store.setting(crate::mcp_auth::AUTHORITY_KEY);
        drop(lock);
        let scope = Self {
            core: core.clone(),
            id: id.into(),
            name,
            raw,
            config,
            generation,
            binding,
            epoch,
            authority_generation,
            programs: vec![],
        };
        scope.check()?;
        Ok(scope)
    }
    fn pin(mut self) -> Result<Self, String> {
        let mut budget = 256 * 1024 * 1024;
        if self.config.transport == profile::Transport::Stdio {
            self.programs.push(Program::new(
                PathBuf::from(self.config.command.as_deref().ok_or("MCP_COMMAND")?),
                &mut budget,
            )?);
            for arg in &self.config.args {
                let path = PathBuf::from(arg);
                if path.is_absolute() && path.exists() && path.is_file() {
                    self.programs.push(Program::new(path, &mut budget)?);
                }
            }
        }
        // Worker does only bounded file pinning. Profile locks/SQLite and the
        // fresh authority check run back on the caller's async executor.
        Ok(self)
    }
    #[cfg(test)]
    fn new(core: &Arc<Core>, id: &str) -> Result<Self, String> {
        let scope = Self::capture(core, id)?.pin()?;
        scope.check()?;
        Ok(scope)
    }
    fn fingerprint(&self) -> String {
        digest(
            &json!({"connection":self.id,"profile":self.raw,"generation":self.generation,"binding":self.binding,"epoch":self.epoch,"authority_generation":self.authority_generation,"programs":self.programs.iter().map(|p|json!({"path":p.path,"identity":p.identity,"sha256":p.sha256})).collect::<Vec<_>>()}),
        )
    }
    fn check(&self) -> Result<(), String> {
        if self.core.store.setting("privacy") == true {
            return Err("PRIVACY_DENIED".into());
        }
        if !self.core.enabled("mcp") {
            return Err("MODULE_DISABLED: mcp".into());
        }
        let _lock = integrations::save_lock(&self.core)?;
        if self.core.mcp_epoch.load(Ordering::SeqCst) != self.epoch
            || self.core.store.setting(crate::mcp_auth::AUTHORITY_KEY) != self.authority_generation
            || record(&self.core, &self.id)?.0 != self.raw
            || self.core.store.setting(&mcp_save::generation_key(&self.id)) != self.generation
            || self
                .generation
                .as_str()
                .is_none_or(|s| credentials::canonical(s).is_err())
        {
            return Err("MCP_CONNECTION_CHANGED".into());
        }
        if mcp_save::binding(&self.core, &self.id, &self.config)? != self.binding {
            return Err("MCP_CREDENTIAL_BINDING_CHANGED".into());
        }
        if !self.config.enabled {
            return Err("MCP_CONNECTION_DISABLED".into());
        }
        if self.binding.stage == credentials::Stage::Pending {
            return Err("MCP_CREDENTIAL_REENTRY_REQUIRED".into());
        }
        if !profile::required_secrets(&self.config).is_empty()
            && self.binding.stage != credentials::Stage::Ready
        {
            return Err("MCP_CREDENTIAL_REENTRY_REQUIRED".into());
        }
        for p in &self.programs {
            p.check()?;
        }
        Ok(())
    }
    async fn guard<T>(&self, f: impl Future<Output = Result<T, String>>) -> Result<T, String> {
        self.check()?;
        tokio::pin!(f);
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        loop {
            tokio::select! {biased; _=tick.tick()=>self.check()?,r=&mut f=>{self.check()?;return r;}}
        }
    }
    fn observe(
        &self,
        action: &str,
        subject: &str,
        definition: &Value,
        version: &str,
        catalog: Option<&catalog::Fence>,
    ) -> Result<Value, String> {
        if let Some(fence) = catalog {
            fence.check(&self.core.store)?;
        }
        let fingerprint = digest(
            &json!({"scope":self.fingerprint(),"action":action,"subject":subject,"definition":definition,"version":version,"catalog":catalog,"nonce":uuid::Uuid::new_v4().to_string()}),
        );
        let mut cache = self
            .core
            .mcp_observations
            .lock()
            .map_err(|_| "MCP_OBSERVATION_LOCK")?;
        cache.retain(|_, o| o.expires > Instant::now());
        if cache.len() >= 512
            || cache
                .values()
                .map(|o| o.definition.to_string().len())
                .sum::<usize>()
                + definition.to_string().len()
                > 8 * 1024 * 1024
        {
            return Err("MCP_OBSERVATION_LIMIT".into());
        }
        cache.insert(
            fingerprint.clone(),
            Observation {
                scope: self.fingerprint(),
                action: action.into(),
                subject: subject.into(),
                definition: definition.clone(),
                version: version.into(),
                catalog: catalog.cloned(),
                expires: Instant::now() + Duration::from_secs(600),
            },
        );
        Ok(json!({"fingerprint":fingerprint}))
    }
}
pub(crate) struct Prepared {
    scope: Arc<Scope>,
    action: String,
    input: Value,
    observation: Option<Observation>,
    catalog: Option<catalog::Fence>,
    oauth: Option<crate::mcp_oauth::Plan>,
    oauth_registration: Option<crate::mcp_auth::enrollment::Reviewed>,
    oauth_state: Option<crate::mcp_auth::State>,
    client_secret: Option<crate::mcp_auth::Text>,
}
impl Prepared {
    fn check_catalog(&self) -> Result<(), String> {
        if let Some(fence) = &self.catalog {
            fence.check(&self.scope.core.store)?;
        }
        Ok(())
    }
    pub(crate) fn private_secret(
        &mut self,
        secret: Option<crate::mcp_auth::Text>,
    ) -> Result<(), String> {
        if self.action == "oauth_start" {
            self.oauth_registration
                .as_ref()
                .ok_or("MCP_OAUTH_PLAN_REQUIRED")?
                .validate_start(secret.is_some())?;
            self.client_secret = secret;
        } else if secret.is_some() {
            return Err("MCP_OAUTH_PRIVATE_INPUT".into());
        }
        Ok(())
    }
    pub(crate) fn review(&self) -> Value {
        let mut review = crate::policy::redact(
            &json!({"connection":self.scope.id,"name":self.scope.name,"transport":self.scope.config.transport,"target":profile::credential_target(&self.scope.config),"action":self.action,"input":self.input,"definition":self.observation.as_ref().map(|o|&o.definition),"protocolVersion":self.observation.as_ref().map(|o|&o.version),"profile_generation":self.scope.generation,"programs":self.scope.programs.iter().map(|p|json!({"path":p.path,"sha256":p.sha256})).collect::<Vec<_>>(),"automatic_replay":false,"remote_undo":false,"untrusted":true}),
        );
        if let Some(plan) = &self.oauth {
            review["oauth_discovery"] = plan.review();
        }
        if let Some(plan) = &self.oauth_registration {
            review["oauth_registration"] = plan.review();
        }
        if self.action == "oauth_start" {
            review["client_secret_supplied"] = json!(self.client_secret.is_some());
        }
        if let Some(state) = &self.oauth_state {
            review["oauth_state"] = crate::mcp_auth::service::public(state);
        }
        if self.action == "oauth_cleanup" {
            review["local_credential_cleanup"] = json!({"scope":"original profile/root protected receipts","active_generation_preserved":true,"changed_entries_preserved":true,"network_requests":false});
        }
        if self.action == "oauth_reset" {
            review["reset_unknown"] = json!({"remote_operation_may_have_succeeded":true,"remote_undo":false,"new_registration_may_create_another_client":true,"tokens_remain_quarantined":true,"new_start_requires_separate_approval":true,"network_requests":false});
        }
        review
    }
    pub(crate) fn risk(&self) -> Risk {
        Risk::Mandatory
    }
}
pub(crate) async fn prepare(
    core: &Arc<Core>,
    tool: &str,
    input: &Value,
) -> Result<Prepared, String> {
    if !TOOLS.contains(&tool)
        || !jsonschema::validator_for(&client::tool_schema(tool))
            .map_err(|_| "MCP_TOOL_SCHEMA")?
            .is_valid(input)
    {
        return Err("MCP_INPUT".into());
    }
    let action = tool.strip_prefix("mcp.").ok_or("MCP_ACTION")?;
    static PIN_WORKERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
    let scope = Scope::capture(core, input["connection"].as_str().ok_or("MCP_CONNECTION")?)?;
    profile::allowed(&scope.config, action, input)?;
    let scope = if scope.config.transport == profile::Transport::Stdio {
        let permit = PIN_WORKERS
            .acquire()
            .await
            .map_err(|_| "MCP_PREPARE_WORKER")?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            scope.pin()
        })
        .await
        .map_err(|_| "MCP_PREPARE_WORKER")??
    } else {
        scope
    };
    scope.check()?;
    let scope = Arc::new(scope);
    if action == "listen" {
        crate::mcp_subscription_wire::Filter::parse(&input["notifications"], true)?;
    }
    let oauth = if action == "oauth_discover" {
        Some(crate::mcp_oauth::Plan::new(&scope.config, input)?)
    } else {
        None
    };
    if !matches!(action, "health" | "listen") && oauth.is_none() && !action.starts_with("oauth_") {
        client::request(action, input)?;
    }
    let observation = if matches!(
        action,
        "call" | "read_resource" | "get_prompt" | "oauth_registration" | "oauth_start"
    ) {
        let fingerprint = input["expected"]["fingerprint"]
            .as_str()
            .ok_or("MCP_OBSERVATION_REQUIRED")?;
        let o = core
            .mcp_observations
            .lock()
            .map_err(|_| "MCP_OBSERVATION_LOCK")?
            .get(fingerprint)
            .cloned()
            .ok_or("MCP_OBSERVATION_EXPIRED_OR_UNKNOWN")?;
        let subject = if matches!(action, "oauth_registration" | "oauth_start") {
            &input["issuer"]
        } else if action == "read_resource" {
            &input["uri"]
        } else {
            &input["name"]
        };
        if o.expires <= Instant::now()
            || o.scope != scope.fingerprint()
            || o.action
                != if matches!(action, "oauth_registration" | "oauth_start") {
                    "oauth_authorize"
                } else {
                    action
                }
            || subject.as_str() != Some(o.subject.as_str())
        {
            return Err("MCP_OBSERVATION_CHANGED_REVIEW_REQUIRED".into());
        }
        if action == "call" {
            client::schema::Tool::new(&o.definition, false)?.arguments(&input["body"])?;
        }
        if action == "get_prompt" {
            prompt_arguments(&o.definition, &input["body"])?;
        }
        Some(o)
    } else {
        None
    };
    let catalog = if let Some(category) = catalog::Category::for_action(action) {
        if let Some(observation) = &observation {
            let fence = observation
                .catalog
                .clone()
                .ok_or("MCP_CATALOG_OBSERVATION_REQUIRED")?;
            fence.check(&core.store)?;
            Some(fence)
        } else {
            Some(catalog::Fence::capture(&core.store, &scope.id, category)?)
        }
    } else {
        None
    };
    let oauth_registration = if matches!(action, "oauth_registration" | "oauth_start") {
        Some(crate::mcp_auth::enrollment::Reviewed::for_core(
            core,
            &scope.config,
            input,
            &observation
                .as_ref()
                .ok_or("MCP_OBSERVATION_REQUIRED")?
                .definition,
        )?)
    } else {
        None
    };
    let oauth_state = if action == "oauth_status" {
        crate::mcp_auth::service::optional_snapshot(core, &scope.id)?
    } else if matches!(
        action,
        "oauth_cancel" | "oauth_refresh" | "oauth_recover" | "oauth_cleanup" | "oauth_reset"
    ) {
        let state = crate::mcp_auth::service::snapshot(core, &scope.id)?;
        if action != "oauth_status" && input["session_id"] != state.session_id {
            return Err("MCP_OAUTH_STATE_CHANGED_REVIEW_REQUIRED".into());
        }
        Some(state)
    } else {
        None
    };
    scope.check()?;
    Ok(Prepared {
        scope,
        action: action.into(),
        input: input.clone(),
        observation,
        catalog,
        oauth,
        oauth_registration,
        oauth_state,
        client_secret: None,
    })
}
fn prompt_arguments(definition: &Value, args: &Value) -> Result<(), String> {
    let mut known = BTreeSet::new();
    for arg in definition
        .get("arguments")
        .map(|v| v.as_array().ok_or("MCP_PROMPT_DEFINITION"))
        .transpose()?
        .into_iter()
        .flatten()
    {
        let name = arg["name"]
            .as_str()
            .filter(|n| profile::name(n))
            .ok_or("MCP_PROMPT_DEFINITION")?;
        if !known.insert(name) || arg.get("required").is_some_and(|v| !v.is_boolean()) {
            return Err("MCP_PROMPT_DEFINITION".into());
        }
        if arg["required"] == true && args.get(name).is_none() {
            return Err("MCP_PROMPT_ARGUMENT_REQUIRED".into());
        }
    }
    if args.as_object().is_none_or(|m| {
        m.keys().any(|k| !known.contains(k.as_str())) || m.values().any(|v| !v.is_string())
    }) {
        return Err("MCP_PROMPT_ARGUMENTS".into());
    }
    client::schema::bounded(args, 65536)
}
fn list(result: &Value, action: &str) -> Result<Vec<Value>, String> {
    let items = result[action].as_array().ok_or("MCP_LIST_RESULT")?;
    if items.len() > 256 {
        return Err("MCP_LIST_LIMIT".into());
    }
    if let Some(cursor) = result.get("nextCursor") {
        client::cursor_value(cursor)?;
    }
    let mut seen = BTreeSet::new();
    for item in items {
        let subject = if action == "resources" {
            item["uri"].as_str().filter(|s| {
                s.len() <= 8192
                    && reqwest::Url::parse(s).is_ok()
                    && !s.chars().any(char::is_control)
            })
        } else {
            item["name"].as_str().filter(|s| profile::name(s))
        }
        .ok_or("MCP_LIST_DEFINITION")?;
        if !seen.insert(subject) {
            return Err("MCP_DUPLICATE_SUBJECT".into());
        }
        if action == "prompts" {
            let mut dummy = serde_json::Map::new();
            if let Some(args) = item.get("arguments") {
                for arg in args.as_array().ok_or("MCP_PROMPT_DEFINITION")? {
                    dummy.insert(
                        arg["name"].as_str().ok_or("MCP_PROMPT_DEFINITION")?.into(),
                        json!(""),
                    );
                }
            }
            prompt_arguments(item, &Value::Object(dummy))?;
        }
        client::schema::bounded(item, 128 * 1024)?;
    }
    Ok(items.clone())
}
async fn find_subject(
    session: &mut client::transport::Session,
    action: &str,
    subject: &str,
) -> Result<Value, String> {
    let mut cursor = None;
    let mut cursors = BTreeSet::new();
    let mut subjects = BTreeSet::new();
    let mut selected = None;
    for _ in 0..16 {
        let result = session
            .rpc(
                &client::request(
                    action,
                    &cursor
                        .as_ref()
                        .map(|c| json!({"cursor":c}))
                        .unwrap_or_else(|| json!({})),
                )?,
                &[],
            )
            .await?;
        for item in list(&result, action)? {
            let key = if action == "resources" {
                &item["uri"]
            } else {
                &item["name"]
            }
            .as_str()
            .ok_or("MCP_LIST_DEFINITION")?;
            if !subjects.insert(key.to_owned()) || subjects.len() > 2048 {
                return Err("MCP_DUPLICATE_SUBJECT_OR_LIST_LIMIT".into());
            }
            if key == subject {
                selected = Some(item);
            }
        }
        let Some(next) = result.get("nextCursor") else {
            return selected.ok_or("MCP_SUBJECT_NOT_AVAILABLE".into());
        };
        let next = client::cursor_value(next)?.to_string();
        if !cursors.insert(next.clone()) {
            return Err("MCP_CURSOR_CYCLE".into());
        }
        cursor = Some(next);
    }
    Err("MCP_PAGINATION_LIMIT".into())
}
fn contains_private(v: &Value, needles: &[String]) -> bool {
    let matched = |s: &str| needles.iter().any(|n| !n.is_empty() && s.contains(n));
    match v {
        Value::String(s) => matched(s),
        Value::Array(a) => a.iter().any(|v| contains_private(v, needles)),
        Value::Object(m) => m
            .iter()
            .any(|(k, v)| matched(k) || contains_private(v, needles)),
        _ => false,
    }
}
async fn operation(
    p: &Prepared,
    session: &mut client::transport::Session,
    material: &credentials::Material,
    grant: Option<&crate::mcp_auth::Grant>,
    dispatched: &mut bool,
) -> Result<Value, String> {
    if p.action == "health" {
        return Ok(
            json!({"protocolVersion":session.version(),"discovery":session.info,"businessToolExecuted":false}),
        );
    }
    client::capability(session, &p.action)?;
    let request = client::request(&p.action, &p.input)?;
    if let Some(o) = &p.observation {
        let tool = if p.action == "call" {
            Some(client::find_tool(session, &o.subject).await?)
        } else {
            None
        };
        let fresh = if let Some(tool) = &tool {
            tool.definition.clone()
        } else {
            find_subject(
                session,
                if p.action == "read_resource" {
                    "resources"
                } else {
                    "prompts"
                },
                &o.subject,
            )
            .await?
        };
        if session.version() != o.version || fresh != o.definition {
            return Err("MCP_SERVER_OBSERVATION_CHANGED_REVIEW_REQUIRED".into());
        }
        if o.expires <= Instant::now() {
            return Err("MCP_OBSERVATION_EXPIRED_OR_UNKNOWN".into());
        }
        let fields = if let Some(tool) = &tool {
            tool.arguments(&p.input["body"])?;
            tool.fields.clone()
        } else {
            vec![]
        };
        if session.mirrored() {
            client::protocol::headers(&client::protocol::modern_request(&request)?, &fields)?;
        }
        p.scope.check()?;
        p.check_catalog()?;
        *dispatched = true;
        let result = session.rpc(&request, &fields).await?;
        if let Some(tool) = tool {
            tool.output(&result)?;
        }
        if p.action == "read_resource" && !result["contents"].is_array() {
            return Err("MCP_RESOURCE_CONTENT".into());
        }
        if p.action == "get_prompt" && !result["messages"].is_array() {
            return Err("MCP_PROMPT_CONTENT".into());
        }
        return Ok(result);
    }
    let result = session.rpc(&request, &[]).await?;
    if contains_private(&result, &material.needles)
        || grant.is_some_and(|g| g.contains_private(&result))
    {
        return Err("MCP_CREDENTIAL_CONTENT_DENIED".into());
    }
    let mut result = if p.action == "tools" {
        client::tool_page(&result, session.mirrored())?.0
    } else {
        list(&result, &p.action)?;
        result
    };
    let action = match p.action.as_str() {
        "tools" => "call",
        "resources" => "read_resource",
        "prompts" => "get_prompt",
        _ => return Err("MCP_ACTION".into()),
    };
    let items = result[&p.action].as_array_mut().ok_or("MCP_LIST_RESULT")?;
    for item in items {
        let subject = if action == "read_resource" {
            item["uri"].as_str()
        } else {
            item["name"].as_str()
        }
        .ok_or("MCP_LIST_DEFINITION")?
        .to_owned();
        let allowed = profile::allowed(
            &p.scope.config,
            action,
            &json!({"name":subject,"uri":subject}),
        )
        .is_ok();
        // Provider-defined lookalike fields never become an owned observation.
        let definition = item.clone();
        item.as_object_mut()
            .ok_or("MCP_LIST_DEFINITION")?
            .remove("alfredpetObservation");
        item["alfredpetPermission"] = json!(if allowed { "allowed" } else { "denied" });
        if allowed {
            item["alfredpetObservation"] = p.scope.observe(
                action,
                &subject,
                &definition,
                session.version(),
                p.catalog.as_ref(),
            )?;
        }
    }
    Ok(result)
}
fn check_owned_observation(p: &Prepared) -> Result<(), String> {
    p.scope.check()?;
    p.check_catalog()?;
    let observed = p.observation.as_ref().ok_or("MCP_OBSERVATION_REQUIRED")?;
    let cache = p
        .scope
        .core
        .mcp_observations
        .lock()
        .map_err(|_| "MCP_OBSERVATION_LOCK")?;
    let live = p.input["expected"]["fingerprint"]
        .as_str()
        .and_then(|s| cache.get(s))
        .ok_or("MCP_OBSERVATION_EXPIRED_OR_UNKNOWN")?;
    if observed.expires <= Instant::now()
        || observed.scope != p.scope.fingerprint()
        || live.expires <= Instant::now()
        || live.scope != observed.scope
        || live.action != observed.action
        || live.subject != observed.subject
        || live.definition != observed.definition
        || live.version != observed.version
        || live.catalog != observed.catalog
    {
        return Err("MCP_OBSERVATION_CHANGED_REVIEW_REQUIRED".into());
    }
    Ok(())
}
pub(crate) async fn execute(mut p: Prepared) -> Result<Value, String> {
    p.check_catalog()?;
    if p.action == "oauth_status" && p.oauth_state.is_none() {
        return p
            .scope
            .guard(async { crate::mcp_auth::service::status(&p.scope.core, &p.input) })
            .await;
    }
    if let Some(state) = &p.oauth_state {
        return p
            .scope
            .guard(crate::mcp_auth::service::control(
                &p.scope.core,
                &p.scope.id,
                state,
                &p.action,
            ))
            .await;
    }
    let _serial = p
        .scope
        .guard(async { Ok(p.scope.core.mcp_actions.lock().await) })
        .await?;
    if let Some(plan) = &p.oauth_registration {
        check_owned_observation(&p)?;
        if p.action == "oauth_start" {
            let plan = p
                .oauth_registration
                .take()
                .ok_or("MCP_OAUTH_PLAN_REQUIRED")?;
            return p
                .scope
                .guard(crate::mcp_auth::service::start(
                    &p.scope.core,
                    &p.scope.id,
                    plan,
                    p.client_secret.take(),
                ))
                .await;
        }
        return Ok(crate::policy::redact(&plan.review()));
    }
    if let Some(plan) = &p.oauth {
        let mut result = p
            .scope
            .guard(async {
                tokio::time::timeout(Duration::from_secs(30), plan.discover())
                    .await
                    .map_err(|_| "MCP_OAUTH_DISCOVERY_TIMEOUT".to_string())?
            })
            .await?;
        let issuer = result["issuer"]
            .as_str()
            .ok_or("MCP_OAUTH_ISSUER_REQUIRED")?;
        let observed = p.scope.observe(
            "oauth_authorize",
            issuer,
            &result,
            client::protocol::MODERN,
            None,
        )?;
        result["alfredpetObservation"] = observed;
        p.scope.check()?;
        return Ok(crate::policy::redact(&result));
    }
    let listener = if p.action == "listen" {
        Some(listen::reserve(
            &p.scope.core,
            &p.scope.id,
            &crate::mcp_subscription_wire::Filter::parse(&p.input["notifications"], true)?,
        )?)
    } else {
        None
    };
    let native = if profile::required_secrets(&p.scope.config).is_empty() {
        None
    } else {
        Some(
            p.scope
                .guard(mcp_save::native_read(&p.scope.core, &p.scope.id))
                .await?,
        )
    };
    let values = credentials::read(
        &p.scope.id,
        &p.scope.config,
        &p.scope.binding,
        native.as_deref(),
    )?;
    let mut material = credentials::material(&p.scope.config, &values)?;
    let grant = if matches!(p.scope.config.auth, profile::Auth::OAuth2 { .. }) {
        let grant = Arc::new(crate::mcp_auth::Actor::load(&p.scope.core, &p.scope.id)?.grant()?);
        material.headers.insert(
            reqwest::header::AUTHORIZATION,
            grant.header_for_resource(profile::endpoint(&p.scope.config)?.as_str())?,
        );
        Some(grant)
    } else {
        None
    };
    let scope = p.scope.clone();
    let private_grant = grant.clone();
    let private_catalog = p.catalog.clone();
    material.guard = Some(crate::mcp_request_guard::Guard::new(move || {
        scope.check()?;
        if let Some(fence) = &private_catalog {
            fence.check(&scope.core.store)?;
        }
        if let Some(grant) = &private_grant {
            grant.check()?;
        }
        Ok(())
    }));
    let mut session = p
        .scope
        .guard(crate::mcp_request_guard::Guard::run(
            material.guard.as_ref(),
            async {
                tokio::time::timeout(
                    Duration::from_secs(30),
                    client::transport::Session::open_private(&p.scope.raw, &material),
                )
                .await
                .map_err(|_| "MCP_TIMEOUT".to_string())?
            },
        ))
        .await?;
    if let Some(listener) = listener {
        return listen::start(&p, listener, session, material, grant).await;
    }
    let mut dispatched = false;
    let result = p
        .scope
        .guard(crate::mcp_request_guard::Guard::run(
            material.guard.as_ref(),
            async {
                tokio::time::timeout(
                    Duration::from_secs(30),
                    operation(
                        &p,
                        &mut session,
                        &material,
                        grant.as_deref(),
                        &mut dispatched,
                    ),
                )
                .await
                .map_err(|_| "MCP_TIMEOUT".to_string())?
            },
        ))
        .await;
    session.close().await;
    let mut result = result.map_err(|e| {
        if dispatched {
            format!("MCP_OUTCOME_UNCERTAIN:{e}")
        } else {
            e
        }
    })?;
    p.scope
        .check()
        .and_then(|()| p.check_catalog())
        .map_err(|e| {
            if dispatched {
                format!("MCP_OUTCOME_UNCERTAIN:{e}")
            } else {
                e
            }
        })?;
    if let Some(object) = result.as_object_mut() {
        object.remove("undo_token");
        object.remove("undoToken");
    }
    for needle in &material.needles {
        if !needle.is_empty() {
            result = client::redact_literal(&result, needle);
        }
    }
    if let Some(grant) = grant {
        result = grant.redact(&result);
    }
    Ok(crate::policy::redact(&result))
}
#[cfg(test)]
pub(crate) fn owned_oauth_observation(
    core: &Arc<Core>,
    id: &str,
    discovery: &Value,
) -> Result<Value, String> {
    let scope = Scope::new(core, id)?;
    scope.observe(
        "oauth_authorize",
        discovery["issuer"]
            .as_str()
            .ok_or("MCP_OAUTH_ISSUER_REQUIRED")?,
        discovery,
        client::protocol::MODERN,
        None,
    )
}
#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
