//! Explicit managed editor sessions. CLI receives argv, never a shell wrapper.
//! Public API content is untrusted; mutations require individual human review.
use crate::{asset_files as assets, policy, process, runtime::Core, storage, terminal};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, Weak,
    },
    time::Duration,
};
use tokio::process::Command;

pub(crate) const TOOLS: &[&str] = &[
    "vscode.discover",
    "vscode.sessions",
    "vscode.open",
    "vscode.commands",
    "vscode.command",
    "vscode.palette",
    "vscode.tasks",
    "vscode.run_task",
    "vscode.task_status",
    "vscode.task_cancel",
    "vscode.diagnostics",
    "vscode.run",
];
const LIMIT: usize = 1024 * 1024;
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn field<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v[key].as_str().ok_or_else(|| format!("VSCODE_INPUT_{key}"))
}
fn path_text(p: &Path) -> Result<String, String> {
    policy::normalized_root(p.to_str().ok_or("VSCODE_PATH_ENCODING")?, false)
}
pub(crate) fn mutation(tool: &str) -> bool {
    ![
        "vscode.discover",
        "vscode.sessions",
        "vscode.commands",
        "vscode.task_status",
        "vscode.diagnostics",
    ]
    .contains(&tool)
}
pub(crate) fn schema(tool: &str) -> Value {
    let text = json!({"type":"string","minLength":1,"maxLength":8192});
    let uuid = json!({"type":"string","format":"uuid","pattern":"^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$"});
    let fingerprint = json!({"type":"string","pattern":"^[a-f0-9]{64}$"});
    let mut p = serde_json::Map::new();
    let mut required = vec![];
    match tool {
        "vscode.discover" => {
            p.insert("path".into(), text.clone());
        }
        "vscode.sessions" => {}
        "vscode.open" => {
            p.insert("path".into(), text.clone());
            p.insert("session".into(), uuid.clone());
            required.push("path");
            for k in ["line", "column"] {
                p.insert(
                    k.into(),
                    json!({"type":"integer","minimum":1,"maximum":1000000}),
                );
            }
        }
        "vscode.run" => {
            p.insert("path".into(), text.clone());
            p.insert(
                "command".into(),
                json!({"type":"string","minLength":1,"maxLength":64000}),
            );
            p.insert(
                "timeout_seconds".into(),
                json!({"type":"integer","minimum":1,"maximum":3600}),
            );
            required.extend(["path", "command"]);
        }
        _ => {
            p.insert("session".into(), uuid.clone());
            required.push("session");
            match tool {
                "vscode.command" | "vscode.palette" | "vscode.run_task" => {
                    p.insert("fingerprint".into(), fingerprint);
                    required.push("fingerprint");
                    if tool == "vscode.command" {
                        p.insert("command".into(), json!({"type":"string","minLength":1,"maxLength":200,"pattern":"^[^_]"}));
                        p.insert("arguments".into(), json!({"type":"array","maxItems":20,"items":{"type":["string","number","boolean","null"],"maxLength":8192}}));
                        required.extend(["command", "arguments"]);
                    } else if tool == "vscode.run_task" {
                        p.insert(
                            "task".into(),
                            json!({"type":"string","pattern":"^[a-f0-9]{64}$"}),
                        );
                        required.push("task");
                    }
                }
                "vscode.task_status" | "vscode.task_cancel" => {
                    p.insert("run".into(), uuid);
                    required.push("run");
                }
                "vscode.commands" | "vscode.tasks" | "vscode.diagnostics" => {}
                _ => return json!({"not":{}}),
            }
        }
    }
    json!({"type":"object","properties":p,"required":required,"additionalProperties":false})
}
struct Scope {
    core: Weak<Core>,
    path: PathBuf,
    identity: String,
    _handles: Vec<File>,
}
impl Scope {
    fn new(core: &Arc<Core>, raw: &str, directory: bool) -> Result<Self, String> {
        check(core)?;
        let path = core.path(raw, false)?;
        let mut handles = Vec::new();
        for ancestor in path
            .ancestors()
            .skip(1)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            handles.push(assets::directory(ancestor)?);
        }
        let file = if directory {
            assets::directory(&path)?
        } else {
            assets::open(&path, false)?
        };
        let identity = assets::identity(&file)?;
        handles.push(file);
        let result = Self {
            core: Arc::downgrade(core),
            path,
            identity,
            _handles: handles,
        };
        result.check()?;
        Ok(result)
    }
    fn check(&self) -> Result<(), String> {
        let core = self.core.upgrade().ok_or("VSCODE_CORE_CLOSED")?;
        check(&core)?;
        let path = core.path(self.path.to_str().ok_or("VSCODE_PATH_ENCODING")?, false)?;
        if path != self.path
            || assets::identity(self._handles.last().ok_or("VSCODE_SCOPE_HANDLE")?)?
                != self.identity
        {
            return Err("VSCODE_SCOPE_CHANGED".into());
        }
        Ok(())
    }
}
fn check(c: &Core) -> Result<(), String> {
    if !c.enabled("vscode") {
        return Err("VSCODE_MODULE_DISABLED".into());
    }
    if c.store.setting("privacy") == true {
        return Err("PRIVACY_DENIED".into());
    }
    Ok(())
}
fn fingerprint(v: &Value) -> bool {
    v.as_str().is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}
fn response_valid(operation: &str, input: &Value, data: &Value) -> bool {
    if !data.is_object() {
        return false;
    }
    match operation {
        "info" => {
            data["session"]
                .as_str()
                .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
                && data["root"].is_string()
                && data["trusted"].is_boolean()
                && data["managedProfile"] == true
                && data["editorVersion"].is_string()
        }
        "commands" => {
            fingerprint(&data["fingerprint"])
                && data["commands"].as_array().is_some_and(|a| {
                    a.len() <= 10000
                        && a.iter().all(|v| {
                            v.as_str().is_some_and(|s| {
                                !s.is_empty() && s.len() <= 200 && !s.starts_with('_')
                            })
                        })
                })
        }
        "tasks" => {
            fingerprint(&data["fingerprint"])
                && data["tasks"].as_array().is_some_and(|a| {
                    a.len() <= 100
                        && a.iter()
                            .all(|v| fingerprint(&v["id"]) && v["name"].is_string())
                })
                && data["configuredTasks"].is_object()
                && data["taskTerminalOutputExposed"] == false
        }
        "diagnostics" => {
            data["items"].as_array().is_some_and(|a| a.len() <= 1000)
                && data["exposed"] == true
                && data["complete"] == false
                && data["compilationVerified"] == false
        }
        "command" => {
            data["invoked"] == true
                && data["command"] == input["command"]
                && data["resultExposed"] == false
                && data["effectsVerified"] == false
        }
        "run_task" | "task_status" | "task_cancel" => {
            data["id"]
                .as_str()
                .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
                && fingerprint(&data["task"])
                && data["state"]
                    .as_str()
                    .is_some_and(|s| ["running", "ended"].contains(&s))
                && data["cancellationRequested"].is_boolean()
                && data["terminalOutputExposed"] == false
                && (operation == "run_task" && data["task"] == input["task"]
                    || operation != "run_task" && data["id"] == input["run"])
        }
        "open" => {
            data["opened"] == true
                && data["path"] == input["path"]
                && data["documentVersion"].is_u64()
        }
        "lease" => data["renewed"] == true,
        "revoke" => data["revoked"] == true && data["editorPreserved"] == true,
        _ => false,
    }
}
struct Cli {
    exe: PathBuf,
    js: PathBuf,
    _handles: Vec<File>,
    version: String,
}
impl Cli {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.exe);
        command.arg(&self.js).env_clear();
        for k in [
            "SystemRoot",
            "WINDIR",
            "TEMP",
            "TMP",
            "USERPROFILE",
            "LOCALAPPDATA",
            "APPDATA",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "PATH",
        ] {
            if let Some(v) = std::env::var_os(k) {
                command.env(k, v);
            }
        }
        command.env("ELECTRON_RUN_AS_NODE", "1");
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        command
    }
}
async fn cli() -> Result<Cli, String> {
    let mut candidates = Vec::new();
    if let Some(p) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(p).join("Programs/Microsoft VS Code"));
    }
    if let Some(p) = std::env::var_os("ProgramFiles") {
        candidates.push(PathBuf::from(p).join("Microsoft VS Code"));
    }
    for base in candidates {
        if !base.join("Code.exe").is_file() {
            continue;
        }
        if base.join("data").exists() {
            return Err("VSCODE_PORTABLE_PROFILE_DENIED".into());
        }
        let mut handles = Vec::new();
        for p in base.ancestors().collect::<Vec<_>>().into_iter().rev() {
            handles.push(assets::directory(p)?);
        }
        let wrapper = assets::open(&base.join("bin/code.cmd"), false)?;
        let bytes = assets::read(&wrapper, 65536)?;
        let wrapper_text = std::str::from_utf8(&bytes).map_err(|_| "VSCODE_CLI_LAYOUT")?;
        // Inspect a data literal only. Never execute or interpolate the wrapper.
        let marker = "%~dp0..\\";
        let js = wrapper_text
            .match_indices(marker)
            .filter_map(|(i, _)| {
                let text = &wrapper_text[i + marker.len()..];
                let end = text.find('"')?;
                let tail = &text[..end];
                let parts = tail.split('\\').collect::<Vec<_>>();
                if parts.as_slice() == ["resources", "app", "out", "cli.js"] {
                    Some(base.join("resources/app/out/cli.js"))
                } else if parts.len() == 5
                    && parts[0].len() <= 64
                    && parts[0].chars().all(|c| c.is_ascii_alphanumeric())
                    && parts[1..] == ["resources", "app", "out", "cli.js"]
                {
                    Some(base.join(parts[0]).join("resources/app/out/cli.js"))
                } else {
                    None
                }
            })
            .next()
            .ok_or("VSCODE_CLI_LAYOUT_UNSUPPORTED")?;
        for p in js.ancestors().skip(1) {
            handles.push(assets::directory(p)?);
        }
        let exe = base.join("Code.exe");
        handles.push(assets::open(&exe, false)?);
        handles.push(assets::open(&js, false)?);
        handles.push(wrapper);
        let mut result = Cli {
            exe,
            js,
            _handles: handles,
            version: String::new(),
        };
        let mut command = result.command();
        command.arg("--version");
        let version = process::output(command, None, 15).await?;
        if version.len() > 1024
            || version.lines().count() > 5
            || !version.lines().next().is_some_and(|s| {
                s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || ".-".contains(c))
            })
        {
            return Err("VSCODE_CLI_VERSION".into());
        }
        result.version = version.trim().to_owned();
        return Ok(result);
    }
    Err("VSCODE_NOT_INSTALLED".into())
}
pub(crate) struct Session {
    id: String,
    scope: Scope,
    token: String,
    port: u16,
    revoked: AtomicBool,
    observations: Mutex<HashMap<String, Value>>,
    _private: Vec<File>,
    bootstrap: File,
}
impl Session {
    fn check(&self) -> Result<(), String> {
        if self.revoked.load(Ordering::SeqCst) {
            return Err("VSCODE_SESSION_REVOKED".into());
        }
        self.scope.check()
    }
    async fn raw(&self, operation: &str, input: &Value) -> Result<Value, String> {
        let id = storage::id();
        let request = json!({"id":id,"session":self.id,"operation":operation,"input":input});
        if request.to_string().len() > 64 * 1024 {
            return Err("VSCODE_INPUT_LIMIT".into());
        }
        let client = crate::http_client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(35))
            .build()
            .map_err(|_| "VSCODE_HTTP_CLIENT")?;
        let mut response = client
            .post(format!("http://127.0.0.1:{}/v1", self.port))
            .bearer_auth(&self.token)
            .json(&request)
            .send()
            .await
            .map_err(|_| "VSCODE_TRANSPORT")?;
        if response.content_length().is_some_and(|n| n > LIMIT as u64) {
            return Err("VSCODE_RESPONSE_LIMIT".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "VSCODE_TRANSPORT")? {
            if bytes.len() + chunk.len() > LIMIT {
                return Err("VSCODE_RESPONSE_LIMIT".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| "VSCODE_RESPONSE_JSON")?;
        if value["id"] != id || value["session"] != self.id {
            return Err("VSCODE_RESPONSE_SCOPE".into());
        }
        if value["ok"] == true && response.status().is_success() {
            if !response_valid(operation, input, &value["data"]) {
                return Err("VSCODE_RESPONSE_SCHEMA".into());
            }
            Ok(value["data"].clone())
        } else {
            let e = value["error"].as_str().unwrap_or("VSCODE_API_FAILED");
            if !e.starts_with("VSCODE_") || !e.chars().all(|c| c.is_ascii_uppercase() || c == '_') {
                return Err("VSCODE_API_FAILED".into());
            }
            Err(e.into())
        }
    }
    async fn request(&self, operation: &str, input: &Value) -> Result<Value, String> {
        self.check()?;
        let action = ["command", "run_task", "task_cancel", "open"].contains(&operation);
        let result = async {
            let operation = self.raw(operation, input);
            tokio::pin!(operation);
            let mut tick = tokio::time::interval(Duration::from_millis(50));
            loop {
                tokio::select! { r=&mut operation=>break r, _=tick.tick()=>self.check()? }
            }
        }
        .await;
        let value = result.map_err(|error| {
            if action
                && [
                    "VSCODE_TRANSPORT",
                    "VSCODE_RESPONSE_JSON",
                    "VSCODE_RESPONSE_SCOPE",
                    "VSCODE_RESPONSE_LIMIT",
                    "VSCODE_RESPONSE_SCHEMA",
                    "VSCODE_SESSION_REVOKED",
                    "VSCODE_SCOPE_CHANGED",
                    "VSCODE_MODULE_DISABLED",
                    "PRIVACY_DENIED",
                ]
                .contains(&error.as_str())
            {
                format!("VSCODE_ACTION_OUTCOME_UNCERTAIN: {error}")
            } else {
                error
            }
        })?;
        self.check().map_err(|error| {
            if action {
                format!("VSCODE_ACTION_OUTCOME_UNCERTAIN: {error}")
            } else {
                error
            }
        })?;
        Ok(value)
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = assets::erase(&self.bootstrap);
        self.token.clear();
    }
}
fn session(c: &Core, id: &str) -> Result<Arc<Session>, String> {
    let session = c
        .vscode_sessions
        .lock()
        .map_err(|_| "VSCODE_SESSION_LOCK")?
        .get(id)
        .cloned()
        .ok_or("VSCODE_SESSION_NOT_OWNED")?;
    session.check()?;
    Ok(session)
}
enum Action {
    Discover(Value),
    Sessions,
    Launch(Scope, Cli),
    Api(Arc<Session>, String, Value, Option<Scope>),
    Run(Scope, terminal::Prepared),
}
pub(crate) struct Prepared {
    core: Arc<Core>,
    action: Action,
    review: Value,
    pub risk: policy::Risk,
    timeout: u64,
}
impl Prepared {
    pub fn review(&self) -> Value {
        self.review.clone()
    }
    pub fn timeout_seconds(&self) -> u64 {
        self.timeout
    }
}
pub(crate) async fn prepare(c: &Arc<Core>, tool: &str, input: &Value) -> Result<Prepared, String> {
    check(c)?;
    let mut review = input.clone();
    let mut timeout = 90;
    let action = match tool {
        "vscode.discover" => Action::Discover(input.clone()),
        "vscode.sessions" => Action::Sessions,
        "vscode.open" if input["session"].is_null() => {
            if input.get("line").is_some() || input.get("column").is_some() {
                return Err("VSCODE_OPEN_FOLDER_POSITION_DENIED".into());
            }
            let scope = Scope::new(c, field(input, "path")?, true)?;
            let cli = Box::pin(cli()).await?;
            scope.check()?;
            review = json!({"path":path_text(&scope.path)?,"editor":cli.exe,"editor_version":cli.version,"profile":"new isolated AlfredPet session","trust":"VS Code user trust prompt retained","effects":"Opening workspace may start VS Code and its built-in extensions; no OS sandbox."});
            Action::Launch(scope, cli)
        }
        "vscode.run" => {
            let scope = Scope::new(c, field(input, "path")?, true)?;
            let terminal_input = json!({"cwd":path_text(&scope.path)?,"command":field(input,"command")?,"timeout_seconds":input["timeout_seconds"].as_u64().unwrap_or(30)});
            let prepared = Box::pin(terminal::prepare(c, "command", &terminal_input)).await?;
            review = prepared.review();
            review["module"] = json!("vscode");
            timeout = prepared.timeout_seconds() + 15;
            Action::Run(scope, prepared)
        }
        _ => {
            let session = session(c, field(input, "session")?)?;
            let operation = if tool == "vscode.palette" {
                "command"
            } else {
                tool.strip_prefix("vscode.").ok_or("VSCODE_TOOL")?
            };
            let mut api = input.clone();
            api.as_object_mut().ok_or("VSCODE_INPUT")?.remove("session");
            if tool == "vscode.palette" {
                api["command"] = json!("workbench.action.showCommands");
                api["arguments"] = json!([]);
            }
            let mut file_scope = None;
            if tool == "vscode.open" {
                let file = Scope::new(c, field(input, "path")?, false)?;
                if !file.path.starts_with(&session.scope.path) {
                    return Err("VSCODE_FILE_WORKSPACE_SCOPE".into());
                }
                api["path"] = json!(path_text(&file.path)?);
                file_scope = Some(file);
            }
            if ["command", "run_task"].contains(&operation) {
                if policy::contains_secret(&api.to_string()) {
                    return Err("VSCODE_LITERAL_SECRET_DENIED".into());
                }
                let key = if operation == "command" {
                    "commands"
                } else {
                    "tasks"
                };
                let observation = session
                    .observations
                    .lock()
                    .map_err(|_| "VSCODE_OBSERVATION_LOCK")?
                    .get(key)
                    .cloned()
                    .ok_or("VSCODE_OBSERVATION_REQUIRED")?;
                if observation["fingerprint"] != input["fingerprint"] {
                    return Err("VSCODE_OBSERVATION_CHANGED".into());
                }
                if operation == "run_task" {
                    let task = observation["tasks"]
                        .as_array()
                        .ok_or("VSCODE_TASK_RESPONSE")?
                        .iter()
                        .find(|t| t["id"] == input["task"])
                        .ok_or("VSCODE_TASK_NOT_OBSERVED")?;
                    review["task_review"] = task.clone();
                    review["configured_tasks"] = observation["configuredTasks"].clone();
                    review["resolution"] = observation["resolution"].clone();
                } else if !observation["commands"]
                    .as_array()
                    .is_some_and(|items| items.iter().any(|item| item == &api["command"]))
                {
                    return Err("VSCODE_COMMAND_NOT_OBSERVED".into());
                }
                review["arguments"] = api["arguments"].clone();
                review["command"] = api["command"].clone();
                review["effects"]=json!("Extension commands and tasks may execute arbitrary code or access network/files. No generic undo or command cancellation. No automatic replay after uncertain results.");
            }
            if operation == "tasks" {
                review["effects"]=json!("Fetching tasks can invoke extension task providers. Enumeration requires individual approval.");
            }
            review["workspace"] = json!(path_text(&session.scope.path)?);
            Action::Api(session, operation.into(), api, file_scope)
        }
    };
    Ok(Prepared {
        core: c.clone(),
        action,
        review,
        risk: if mutation(tool) {
            policy::Risk::Mandatory
        } else {
            policy::Risk::Free
        },
        timeout,
    })
}
fn companion(c: &Core) -> Result<PathBuf, String> {
    let packaged = c.resource.join("sidecars/vscode-bridge");
    let p = if packaged.join("extension.cjs").is_file() {
        packaged
    } else if cfg!(debug_assertions) {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sidecars/vscode-bridge")
    } else {
        return Err("VSCODE_COMPANION_MISSING".into());
    };
    for file in ["package.json", "extension.cjs", "protocol.cjs"] {
        assets::open(&p.join(file), false)?;
    }
    Ok(p)
}
async fn launch(c: &Arc<Core>, scope: Scope, cli: Cli) -> Result<Value, String> {
    let _guard = c.vscode_launch.lock().await;
    scope.check()?;
    {
        let mut sessions = c
            .vscode_sessions
            .lock()
            .map_err(|_| "VSCODE_SESSION_LOCK")?;
        sessions.retain(|_, s| !s.revoked.load(Ordering::SeqCst));
        if sessions.len() >= 8 {
            return Err("VSCODE_SESSION_LIMIT".into());
        }
    }
    let id = storage::id();
    let root = c.root.join("temp").join(format!("vscode-{id}"));
    let mut private = assets::parents(c, &root)?;
    assets::create_directory(&root)?;
    let directory = assets::directory(&root)?;
    assets::private(&directory)?;
    private.push(directory);
    for name in ["profile", "extensions"] {
        let p = root.join(name);
        assets::create_directory(&p)?;
        private.push(assets::directory(&p)?);
    }
    let token = hash(
        format!(
            "{}{}{}{}",
            storage::id(),
            storage::id(),
            storage::id(),
            storage::id()
        )
        .as_bytes(),
    );
    let endpoint_path = root.join("endpoint.json");
    let endpoint = assets::create(&endpoint_path)?;
    let endpoint_identity = assets::identity(&endpoint)?;
    drop(endpoint);
    let bootstrap_path = root.join("bootstrap.json");
    let mut bootstrap = assets::create(&bootstrap_path)?;
    let data = json!({"version":1,"session":id,"token":token,"root":path_text(&scope.path)?,"endpoint":endpoint_path});
    bootstrap
        .write_all(data.to_string().as_bytes())
        .map_err(|_| "VSCODE_BOOTSTRAP_WRITE")?;
    bootstrap.sync_all().map_err(|_| "VSCODE_BOOTSTRAP_WRITE")?;
    let companion = companion(c)?;
    for p in companion.ancestors() {
        private.push(assets::directory(p)?);
    }
    for f in ["package.json", "extension.cjs", "protocol.cjs"] {
        private.push(assets::open(&companion.join(f), false)?);
    }
    let mut command = cli.command();
    command
        .args([
            "--new-window",
            "--sync",
            "off",
            "--skip-welcome",
            "--skip-release-notes",
        ])
        .arg("--user-data-dir")
        .arg(root.join("profile"))
        .arg("--extensions-dir")
        .arg(root.join("extensions"))
        .arg("--extensionDevelopmentPath")
        .arg(&companion)
        .arg(path_text(&scope.path)?)
        .env("ALFREDPET_VSCODE_BOOTSTRAP", &bootstrap_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(false);
    #[cfg(test)]
    if let Some(fixture) = c
        .vscode_fixture
        .lock()
        .map_err(|_| "VSCODE_FIXTURE_LOCK")?
        .as_ref()
    {
        // Only the opt-in test build can bypass trust, for its own UUID profile.
        let fixture = c.path(fixture.to_str().ok_or("VSCODE_FIXTURE_PATH")?, false)?;
        command
            .arg("--disable-workspace-trust")
            .arg("--extensionDevelopmentPath")
            .arg(fixture);
    }
    // CLI transfers to a GUI instance. No kill-on-close Job: unsaved editor state
    // is preserved. A startup failure may leave this isolated editor open.
    let mut child = command.spawn().map_err(|_| "VSCODE_LAUNCH_FAILED")?;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let deadline = tokio::time::sleep(Duration::from_secs(60));
    tokio::pin!(deadline);
    let port = loop {
        tokio::select! {_=&mut deadline=>return Err("VSCODE_ACTION_OUTCOME_UNCERTAIN: VSCODE_STARTUP_TIMEOUT".into()),_=tick.tick()=>{
            scope.check().map_err(|e|format!("VSCODE_ACTION_OUTCOME_UNCERTAIN: {e}"))?;
            if let Some(status)=child.try_wait().map_err(|_|"VSCODE_CLI_WAIT")?{if !status.success(){return Err("VSCODE_ACTION_OUTCOME_UNCERTAIN: VSCODE_CLI_EXIT".into());}}
            let file=match assets::open(&endpoint_path,false){Ok(f)=>f,Err(e)if e=="ASSET_LOCK_OR_ACCESS"=>continue,Err(e)=>return Err(e)};assets::private(&file)?;
            if assets::identity(&file)?!=endpoint_identity{return Err("VSCODE_ENDPOINT_CHANGED".into());}
            let bytes=assets::read(&file,16384)?;if bytes.is_empty(){continue;}
            let endpoint:Value=match serde_json::from_slice(&bytes){Ok(v)=>v,Err(_)=>continue};
            if endpoint["version"]!=1||endpoint["session"]!=id{return Err("VSCODE_ENDPOINT_SCOPE".into());}
            private.push(file);break endpoint["port"].as_u64().filter(|p|*p>0&&*p<=65535).ok_or("VSCODE_ENDPOINT_PORT")? as u16;
        }}
    };
    let session = Arc::new(Session {
        id: id.clone(),
        scope,
        token,
        port,
        revoked: AtomicBool::new(false),
        observations: Mutex::new(HashMap::new()),
        _private: private,
        bootstrap,
    });
    let info = session.request("info", &json!({})).await?;
    if info["session"] != id || info["root"] != path_text(&session.scope.path)? {
        return Err("VSCODE_ACTION_OUTCOME_UNCERTAIN: VSCODE_EDITOR_RESPONSE_SCOPE".into());
    }
    session.request("lease", &json!({})).await?;
    c.vscode_sessions
        .lock()
        .map_err(|_| "VSCODE_SESSION_LOCK")?
        .insert(id.clone(), session.clone());
    let weak = Arc::downgrade(c);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            if weak.upgrade().is_none()
                || session.check().is_err()
                || session.raw("lease", &json!({})).await.is_err()
            {
                session.revoked.store(true, Ordering::SeqCst);
                let _ =
                    tokio::time::timeout(Duration::from_secs(3), session.raw("revoke", &json!({})))
                        .await;
                break;
            }
        }
    });
    Ok(info)
}
fn discover(c: &Arc<Core>, input: &Value) -> Result<Value, String> {
    let roots = if let Some(raw) = input["path"].as_str() {
        let scope = Scope::new(c, raw, true)?;
        vec![scope.path.clone()]
    } else {
        c.roots()
    };
    if roots.len() > 1000 {
        return Err("VSCODE_DISCOVERY_LIMIT".into());
    }
    let mut items = Vec::new();
    for root in roots {
        check(c)?;
        let scope = match Scope::new(c, root.to_str().ok_or("VSCODE_PATH_ENCODING")?, true) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let mut workspaces = Vec::new();
        let mut count = 0;
        for entry in std::fs::read_dir(&scope.path).map_err(|_| "VSCODE_DISCOVERY_READ")? {
            count += 1;
            if count > 500 {
                break;
            }
            let p = entry.map_err(|_| "VSCODE_DISCOVERY_READ")?.path();
            if p.extension().is_some_and(|e| e == "code-workspace") {
                if let Ok(file) = Scope::new(c, p.to_str().ok_or("VSCODE_PATH_ENCODING")?, false) {
                    let bytes =
                        assets::read(file._handles.last().ok_or("VSCODE_HANDLE")?, 256 * 1024)?;
                    workspaces.push(json!({"path":path_text(&p)?,"sha256":hash(&bytes),"contentsEvaluated":false,"multiRootSessionSupported":false}));
                }
            }
        }
        let task_path = scope.path.join(".vscode/tasks.json");
        let task = if task_path.exists() {
            let file = Scope::new(c, task_path.to_str().ok_or("VSCODE_PATH_ENCODING")?, false)?;
            let bytes = assets::read(file._handles.last().ok_or("VSCODE_HANDLE")?, 256 * 1024)?;
            json!({"path":path_text(&task_path)?,"sha256":hash(&bytes),"contentsEvaluated":false})
        } else {
            Value::Null
        };
        scope.check()?;
        items.push(json!({"path":path_text(&scope.path)?,"tasksConfig":task,"workspaceFiles":workspaces,"scanTruncated":count>500}));
    }
    Ok(json!({"items":items,"recursiveScan":false,"resultsUntrusted":true}))
}
pub(crate) async fn execute(p: Prepared) -> Result<Value, String> {
    check(&p.core)?;
    match p.action {
        Action::Discover(v) => discover(&p.core, &v),
        Action::Sessions => {
            let sessions = p
                .core
                .vscode_sessions
                .lock()
                .map_err(|_| "VSCODE_SESSION_LOCK")?;
            Ok(
                json!({"items":sessions.values().filter(|s|s.check().is_ok()).map(|s|json!({"session":s.id,"root":s.scope.path,"managedProfile":true})).collect::<Vec<_>>() }),
            )
        }
        Action::Launch(scope, cli) => Box::pin(launch(&p.core, scope, cli)).await,
        Action::Api(session, operation, input, file_scope) => {
            if let Some(file) = &file_scope {
                file.check()?;
            }
            let value = session.request(&operation, &input).await?;
            if let Some(file) = &file_scope {
                file.check()?;
            }
            if ["commands", "tasks"].contains(&operation.as_str()) {
                session
                    .observations
                    .lock()
                    .map_err(|_| "VSCODE_OBSERVATION_LOCK")?
                    .insert(operation, value.clone());
            }
            Ok(value)
        }
        Action::Run(scope, prepared) => {
            scope.check()?;
            let operation = Box::pin(terminal::execute(&p.core, prepared));
            tokio::pin!(operation);
            let mut tick = tokio::time::interval(Duration::from_millis(50));
            loop {
                tokio::select! {result=&mut operation=>{scope.check()?;return result;},_=tick.tick()=>scope.check()?}
            }
        }
    }
}
#[cfg(all(test, windows))]
#[path = "vscode_tests.rs"]
mod tests;
