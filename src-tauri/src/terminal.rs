use crate::{policy, process, runtime::Core, storage};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

pub struct Prepared {
    shell: PathBuf,
    runner: PathBuf,
    command: String,
    cwd: PathBuf,
    environment: BTreeMap<String, String>,
    timeout: u64,
    elevated: bool,
    system: bool,
    engine_sha256: String,
    pub id: String,
    pub risk: policy::Risk,
}
fn field<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v[key].as_str().ok_or(format!("TERMINAL_INPUT_{key}"))
}
fn env_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && ![
            "PATH",
            "SYSTEMROOT",
            "COMSPEC",
            "TEMP",
            "TMP",
            "PSMODULEPATH",
            "USERPROFILE",
            "HOME",
        ]
        .contains(&key.to_ascii_uppercase().as_str())
        && ![
            "KEY",
            "TOKEN",
            "SECRET",
            "PASSWORD",
            "CREDENTIAL",
            "ANTHROPIC",
            "CLAUDE",
        ]
        .iter()
        .any(|part| key.to_ascii_uppercase().contains(part))
}
pub fn profiles(c: &Core, action: &str, input: &Value) -> Result<Value, String> {
    let mut profiles = c
        .store
        .setting("terminal_profiles")
        .as_array()
        .cloned()
        .unwrap_or_default();
    match action {
        "profiles" => Ok(json!(profiles)),
        "profile_save" => {
            let body = &input["body"];
            let name = field(input, "name")?;
            if name.trim().is_empty() || name.len() > 100 || profiles.len() >= 32 {
                return Err("TERMINAL_PROFILE_LIMIT".into());
            }
            let shell = body["shell"].as_str().unwrap_or("auto");
            if !["auto", "powershell7", "windows_powershell"].contains(&shell) {
                return Err("TERMINAL_SHELL".into());
            }
            let script = body["script_path"].as_str();
            if let Some(script) = script {
                c.path(script, false)?;
            }
            let keys = body["env_allowlist"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if keys.len() > 32 || keys.iter().any(|key| !key.as_str().is_some_and(env_key)) {
                return Err("TERMINAL_ENV_ALLOWLIST".into());
            }
            let id = storage::id();
            profiles.push(json!({"id":id,"name":name,"shell":shell,"script_path":script,"env_allowlist":keys}));
            c.store.set_setting("terminal_profiles", &json!(profiles))?;
            Ok(json!({"id":id}))
        }
        "profile_delete" => {
            let id = field(input, "id")?;
            let before = profiles.len();
            profiles.retain(|p| p["id"] != id);
            if before == profiles.len() {
                return Err("TERMINAL_PROFILE_NOT_FOUND".into());
            }
            c.store.set_setting("terminal_profiles", &json!(profiles))?;
            Ok(json!({"deleted":true}))
        }
        _ => Err("TERMINAL_PROFILE_ACTION".into()),
    }
}
async fn shell(choice: &str) -> Result<PathBuf, String> {
    let system = PathBuf::from(std::env::var("SystemRoot").map_err(|_| "WINDOWS_SYSTEM_ROOT")?);
    if choice != "windows_powershell" {
        if let Ok(programs) = std::env::var("ProgramFiles") {
            let path = PathBuf::from(programs).join("PowerShell/7/pwsh.exe");
            if path.is_file() {
                return Ok(path);
            }
        }
        let mut find = tokio::process::Command::new(system.join("System32/where.exe"));
        find.arg("pwsh.exe");
        if let Ok(found) = process::output(find, None, 5).await {
            if let Some(path) = found
                .lines()
                .map(PathBuf::from)
                .find(|p| p.is_absolute() && p.is_file())
            {
                return Ok(path);
            }
        }
        if choice == "powershell7" {
            return Err("POWERSHELL7_NOT_INSTALLED".into());
        }
    }
    Ok(system.join("System32/WindowsPowerShell/v1.0/powershell.exe"))
}
fn command(
    shell: &PathBuf,
    _runner: &PathBuf,
    mode: &str,
    environment: &BTreeMap<String, String>,
) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(shell);
    cmd.env_clear();
    for key in [
        "SystemRoot",
        "PATH",
        "COMSPEC",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "ProgramFiles",
        "ProgramFiles(x86)",
    ] {
        if let Ok(value) = std::env::var(key) {
            cmd.env(key, value);
        }
    }
    let modules = shell
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("Modules");
    cmd.env("PSModulePath", modules);
    for (key, value) in environment {
        cmd.env(key, value);
    }
    cmd.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-EncodedCommand",
    ]);
    use base64::Engine;
    let source = format!(
        "& {{ {} }} -Mode '{}'",
        include_str!("../../native/terminal.ps1"),
        mode
    );
    let utf16: Vec<u8> = source.encode_utf16().flat_map(u16::to_le_bytes).collect();
    cmd.arg(base64::engine::general_purpose::STANDARD.encode(utf16));
    process::sanitize(&mut cmd);
    cmd
}
pub async fn prepare(c: &Core, action: &str, input: &Value) -> Result<Prepared, String> {
    let elevated = input["elevated"] == true;
    if elevated
        && (input.get("profile_id").is_some()
            || input["body"]["env"]
                .as_object()
                .is_some_and(|v| !v.is_empty()))
    {
        return Err(
            "ADMIN_PROFILE_ENV_UNSUPPORTED: usa un comando sin perfil ni variables añadidas".into(),
        );
    }
    let cwd = c.path(field(input, "cwd")?, false)?;
    if !cwd.is_dir() {
        return Err("TERMINAL_CWD_DIRECTORY".into());
    }
    let profile = if let Some(id) = input["profile_id"].as_str() {
        c.store
            .setting("terminal_profiles")
            .as_array()
            .and_then(|rows| rows.iter().find(|v| v["id"] == id))
            .cloned()
            .ok_or("TERMINAL_PROFILE_NOT_FOUND")?
    } else {
        Value::Null
    };
    let shell = if elevated {
        system_shell()?
    } else {
        shell(profile["shell"].as_str().unwrap_or("auto")).await?
    };
    let runner = c.resource.join("native/terminal.ps1");
    let mut script = String::new();
    for path in [
        profile["script_path"].as_str(),
        if action == "script" {
            Some(field(input, "path")?)
        } else {
            None
        },
    ]
    .into_iter()
    .flatten()
    {
        let path = c.path(path, false)?;
        let bytes = crate::modules::bounded_read(&path)?;
        if bytes.len() > 64000 {
            return Err("TERMINAL_SCRIPT_LIMIT".into());
        }
        script.push_str(std::str::from_utf8(&bytes).map_err(|_| "TERMINAL_SCRIPT_UTF8")?);
        script.push('\n');
    }
    if action != "script" {
        script.push_str(field(input, "command")?);
    }
    if script.trim().is_empty() || script.len() > 128000 {
        return Err("TERMINAL_COMMAND_LIMIT".into());
    }
    if policy::contains_secret(&script) {
        return Err("TERMINAL_LITERAL_SECRET_DENIED".into());
    }
    let timeout = input["timeout_seconds"].as_u64().unwrap_or(30);
    if !(1..=3600).contains(&timeout) {
        return Err("TERMINAL_TIMEOUT_RANGE".into());
    }
    let mut environment = BTreeMap::new();
    if let Some(values) = input["body"]["env"].as_object() {
        if values.len() > 32 {
            return Err("TERMINAL_ENV_LIMIT".into());
        }
        for (key, value) in values {
            let value = value.as_str().ok_or("TERMINAL_ENV_VALUE")?;
            if !env_key(key)
                || !profile["env_allowlist"]
                    .as_array()
                    .is_some_and(|keys| keys.iter().any(|v| v == key))
                || value.len() > 2000
                || policy::contains_secret(value)
            {
                return Err("TERMINAL_ENV_NOT_ALLOWED".into());
            }
            environment.insert(key.clone(), value.to_string());
        }
    }
    let raw = process::output(
        command(&shell, &runner, "analyze", &environment),
        Some(&json!({"command":script}).to_string()),
        15,
    )
    .await?;
    let parsed: Value = serde_json::from_str(&raw).map_err(|_| "TERMINAL_AST_RESPONSE")?;
    if parsed["valid"] != true {
        return Err("TERMINAL_SYNTAX_INVALID".into());
    }
    let read_only = parsed["complex"] == false
        && profile.is_null()
        && environment.is_empty()
        && parsed["commands"].as_array().is_some_and(|commands| {
            !commands.is_empty()
                && commands.iter().all(|name| {
                    name.as_str().is_some_and(|name| {
                        ["get-date", "get-location", "write-output"]
                            .contains(&name.to_ascii_lowercase().as_str())
                    })
                })
        });
    let id = input["id"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| format!("terminal-{}", storage::id()));
    if !id.starts_with("terminal-") || id.len() > 100 {
        return Err("TERMINAL_RUN_ID".into());
    }
    let engine_sha256 = crate::modules::hash(&shell)?;
    Ok(Prepared {
        shell,
        runner,
        command: script,
        cwd,
        environment,
        timeout,
        elevated,
        system: false,
        engine_sha256,
        id,
        risk: if read_only && !elevated {
            policy::Risk::Free
        } else {
            policy::Risk::Mandatory
        },
    })
}
impl Prepared {
    pub fn timeout_seconds(&self) -> u64 {
        self.timeout + if self.elevated { 120 } else { 0 }
    }
    pub fn review(&self) -> Value {
        use sha2::{Digest, Sha256};
        let runner_sha256 = format!(
            "{:x}",
            Sha256::digest(include_bytes!("../../native/terminal.ps1"))
        );
        json!({"id":self.id,"shell":self.shell,"command":self.command,"cwd":self.cwd,"environment":self.environment,"timeout_seconds":self.timeout,"elevated":self.elevated,"engine_sha256":self.engine_sha256,"runner_sha256":runner_sha256})
    }
}
struct Registered {
    core: Arc<Core>,
    id: String,
}
impl Drop for Registered {
    fn drop(&mut self) {
        if let Ok(mut runs) = self.core.runs.lock() {
            runs.remove(&self.id);
        }
    }
}
struct Readers(Vec<tokio::task::JoinHandle<()>>);
impl Drop for Readers {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}
enum Event {
    Line(&'static str, String),
    End,
    Error(String),
}
async fn read<R: tokio::io::AsyncRead + Unpin>(
    stream: R,
    channel: &'static str,
    sender: tokio::sync::mpsc::Sender<Event>,
) {
    let mut reader = BufReader::new(stream);
    loop {
        match process::bounded_line(&mut reader, 65536).await {
            Ok(Some(line)) => {
                if sender.send(Event::Line(channel, line)).await.is_err() {
                    return;
                }
            }
            Ok(None) => {
                let _ = sender.send(Event::End).await;
                return;
            }
            Err(error) => {
                let _ = sender.send(Event::Error(error)).await;
                return;
            }
        }
    }
}
async fn run<F: FnMut(&str, &str) + Send>(
    mut cmd: tokio::process::Command,
    stdin: String,
    timeout: u64,
    cancel: CancellationToken,
    mut emit: F,
) -> Result<Value, String> {
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(timeout));
    tokio::pin!(deadline);
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().map_err(|_| "TERMINAL_SPAWN")?;
    #[cfg(windows)]
    let _job = crate::windows_native::ProcessJob::attach(&child)?;
    let mut input = child.stdin.take().ok_or("TERMINAL_STDIN")?;
    tokio::select! {
        _=cancel.cancelled()=>return Err("TERMINAL_CANCELLED".into()),
        _=&mut deadline=>return Err("TERMINAL_TIMEOUT".into()),
        result=tokio::time::timeout(std::time::Duration::from_secs(10), input.write_all(stdin.as_bytes()))=>result.map_err(|_|"TERMINAL_STDIN_TIMEOUT")?.map_err(|_|"TERMINAL_STDIN")?,
    }
    drop(input);
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let _readers = Readers(vec![
        tokio::spawn(read(
            child.stdout.take().ok_or("TERMINAL_STDOUT")?,
            "stdout",
            tx.clone(),
        )),
        tokio::spawn(read(
            child.stderr.take().ok_or("TERMINAL_STDERR")?,
            "stderr",
            tx,
        )),
    ]);
    let mut output = String::new();
    let mut error = String::new();
    let mut bytes = 0usize;
    let mut ended = 0;
    let mut status = None;
    loop {
        if ended == 2 && status.is_some() {
            break;
        }
        tokio::select! {
            _=cancel.cancelled()=>return Err("TERMINAL_CANCELLED".into()),
            _=&mut deadline=>return Err("TERMINAL_TIMEOUT".into()),
            value=child.wait(),if status.is_none()=>{status=Some(value.map_err(|_|"TERMINAL_WAIT")?);}
            event=rx.recv(),if ended<2=>match event {
                Some(Event::Line(channel,line))=>{bytes+=line.len()+1;if bytes>8*1024*1024{return Err("TERMINAL_OUTPUT_LIMIT".into());}
                    let redacted=policy::redact(&json!(line));let line=redacted.as_str().unwrap_or("[REDACTED]");emit(channel,line);
                    if channel=="stdout"{output.push_str(line);output.push('\n');}else{error.push_str(line);error.push('\n');}
                }
                Some(Event::End)=>ended+=1,
                Some(Event::Error(error))=>return Err(error),
                None=>return Err("TERMINAL_PIPE_CLOSED".into()),
            }
        }
    }
    let status = status.ok_or("TERMINAL_EXIT_MISSING")?;
    if !status.success() {
        return Err(format!(
            "TERMINAL_EXIT_NONZERO: {:?}; stderr={}; stdout={}",
            status.code(),
            error.chars().take(4000).collect::<String>(),
            output.chars().take(4000).collect::<String>()
        ));
    }
    Ok(json!({"stdout":output,"stderr":error,"exit_code":status.code()}))
}
pub async fn execute(c: &Arc<Core>, prepared: Prepared) -> Result<Value, String> {
    if c.store.setting("privacy") == true {
        return Err("PRIVACY_DENIED".into());
    }
    if !c.enabled(if prepared.system {
        "windows"
    } else {
        "terminal"
    }) {
        return Err("MODULE_DISABLED".into());
    }
    if !prepared.system {
        c.path(prepared.cwd.to_str().ok_or("TERMINAL_CWD_UTF8")?, false)?;
    }
    if crate::modules::hash(&prepared.shell)? != prepared.engine_sha256 {
        return Err("TERMINAL_ENGINE_CHANGED".into());
    }
    let cancel = CancellationToken::new();
    {
        let mut runs = c.runs.lock().map_err(|_| "RUN_LOCK")?;
        if runs.contains_key(&prepared.id) {
            return Err("TERMINAL_RUN_BUSY".into());
        }
        runs.insert(prepared.id.clone(), cancel.clone());
    }
    let _registered = Registered {
        core: c.clone(),
        id: prepared.id.clone(),
    };
    let core = c.clone();
    let id = prepared.id.clone();
    c.emit(
        "alfredpet:terminal",
        &json!({"id":prepared.id,"channel":"status","status":"running"}),
    );
    let started = std::time::Instant::now();
    let emit = move |channel: &str, line: &str| {
        core.emit(
            "alfredpet:terminal",
            &json!({"id":id,"channel":channel,"text":line}),
        );
    };
    let monitor_core = c.clone();
    let monitor_cancel = cancel.clone();
    let module = if prepared.system {
        "windows"
    } else {
        "terminal"
    };
    let watcher = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            if monitor_core.store.setting("privacy") == true || !monitor_core.enabled(module) {
                monitor_cancel.cancel();
                break;
            }
        }
    });
    struct Watcher(tokio::task::JoinHandle<()>);
    impl Drop for Watcher {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _watcher = Watcher(watcher);
    let result = if prepared.elevated {
        crate::admin::execute(
            crate::admin::Request {
                command: prepared.command.clone(),
                cwd: prepared.cwd.to_string_lossy().into_owned(),
                timeout: prepared.timeout,
            },
            cancel,
            emit,
        )
        .await
    } else {
        run(
            command(
                &prepared.shell,
                &prepared.runner,
                "execute",
                &prepared.environment,
            ),
            json!({"command":prepared.command,"cwd":prepared.cwd}).to_string(),
            prepared.timeout,
            cancel,
            emit,
        )
        .await
    };
    c.emit("alfredpet:terminal",&json!({"id":prepared.id,"channel":"status","status":if result.is_ok(){"succeeded"}else{"failed"},"error":result.as_ref().err()}));
    result.map(|mut value| {
        value["id"] = json!(prepared.id);
        value["elapsed_ms"] = json!(started.elapsed().as_millis());
        value
    })
}
pub fn system_shell() -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        let mut buffer = vec![0u16; 32768];
        let n = unsafe {
            windows::Win32::System::SystemInformation::GetSystemDirectoryW(Some(&mut buffer))
        } as usize;
        if n == 0 || n >= buffer.len() {
            return Err("ADMIN_SYSTEM_DIRECTORY".into());
        }
        Ok(PathBuf::from(String::from_utf16_lossy(&buffer[..n]))
            .join("WindowsPowerShell/v1.0/powershell.exe"))
    }
    #[cfg(not(windows))]
    {
        Err("WINDOWS_REQUIRED".into())
    }
}
pub fn system_script(script: &str, timeout: u64, elevated: bool) -> Result<Prepared, String> {
    let shell = system_shell()?;
    let cwd = shell
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .ok_or("ADMIN_SYSTEM_DIRECTORY")?
        .to_path_buf();
    Ok(Prepared {
        engine_sha256: crate::modules::hash(&shell)?,
        shell,
        runner: PathBuf::new(),
        command: script.into(),
        cwd,
        environment: BTreeMap::new(),
        timeout,
        elevated,
        system: true,
        id: format!("terminal-{}", storage::id()),
        risk: policy::Risk::Mandatory,
    })
}
impl Prepared {
    pub fn is_elevated(&self) -> bool {
        self.elevated
    }
}
pub async fn elevated_run<F: FnMut(&str, &str) + Send>(
    request: crate::admin::Request,
    cancel: CancellationToken,
    emit: F,
) -> Result<Value, String> {
    let shell = system_shell()?;
    let mut cmd = command(&shell, &PathBuf::new(), "execute", &BTreeMap::new());
    let system = shell
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .ok_or("ADMIN_SYSTEM_DIRECTORY")?;
    let windows = system.parent().ok_or("ADMIN_SYSTEM_DIRECTORY")?;
    cmd.env_clear()
        .env("SystemRoot", windows)
        .env("WINDIR", windows)
        .env("PATH", system)
        .env(
            "PSModulePath",
            shell
                .parent()
                .ok_or("ADMIN_SYSTEM_DIRECTORY")?
                .join("Modules"),
        );
    run(
        cmd,
        json!({"command":request.command,"cwd":request.cwd}).to_string(),
        request.timeout,
        cancel,
        emit,
    )
    .await
}
pub fn cancel(c: &Core, input: &Value) -> Result<Value, String> {
    let id = field(input, "id")?;
    if !id.starts_with("terminal-") {
        return Err("TERMINAL_RUN_ID".into());
    }
    let runs = c.runs.lock().map_err(|_| "RUN_LOCK")?;
    let token = runs.get(id).ok_or("TERMINAL_RUN_NOT_FOUND")?;
    token.cancel();
    Ok(json!({"cancel_requested":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Arc<Core> {
        Arc::new(
            Core::new(
                std::env::temp_dir().join(format!("ap-terminal-{}", storage::id())),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        )
    }
    #[tokio::test]
    async fn output_limits_redaction_and_elevation_are_enforced() {
        let c = fixture();
        c.store
            .execute(
                "UPDATE module_configs SET enabled=1 WHERE module_id='terminal'",
                &[],
            )
            .unwrap();
        let root = c.root.clone();
        let cwd = root.join("temp");
        for (script, expected) in [
            ("Write-Output ('x' * 70000)", "PROCESS_LINE_LIMIT"),
            (
                "1..8500 | ForEach-Object { Write-Output ('x' * 1024) }",
                "TERMINAL_OUTPUT_LIMIT",
            ),
        ] {
            let prepared = prepare(
                &c,
                "command",
                &json!({"cwd":cwd,"command":script,"timeout_seconds":30}),
            )
            .await
            .unwrap();
            assert_eq!(execute(&c, prepared).await.unwrap_err(), expected);
        }
        let prepared = prepare(
            &c,
            "command",
            &json!({"cwd":cwd,"command":"Write-Output ('api_' + 'key=fixture-sensitive')"}),
        )
        .await
        .unwrap();
        let mut lines = Vec::new();
        let result = run(
            command(
                &prepared.shell,
                &prepared.runner,
                "execute",
                &prepared.environment,
            ),
            json!({"cwd":cwd,"command":prepared.command}).to_string(),
            10,
            CancellationToken::new(),
            |_, line| lines.push(line.to_string()),
        )
        .await
        .unwrap();
        assert!(!result.to_string().contains("fixture-sensitive"));
        assert!(!lines.join("").contains("fixture-sensitive"));
        let prepared = prepare(
            &c,
            "command",
            &json!({"cwd":cwd,"command":"Get-Date","elevated":true}),
        )
        .await
        .unwrap();
        assert_eq!(prepared.risk, policy::Risk::Mandatory);
        assert!(prepared.is_elevated());
        assert_eq!(prepared.shell, system_shell().unwrap());
        // Actual UAC consent is a separate interactive native acceptance check.
        assert!(prepare(
            &c,
            "command",
            &json!({"cwd":cwd,"command":"Get-Date","elevated":true,"profile_id":"untrusted"})
        )
        .await
        .is_err());
        assert!(c.runs.lock().unwrap().is_empty());
        drop(c);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn terminal_catalog_policy_and_script_snapshot_are_enforced() {
        let c = fixture();
        let root = c.root.clone();
        let cwd = root.join("temp");
        assert_eq!(
            c.execute("terminal.command", json!({"cwd":cwd,"command":"Get-Date"}))
                .await
                .unwrap_err(),
            "TOOL_DISABLED_OR_UNKNOWN"
        );
        c.store
            .execute(
                "UPDATE module_configs SET enabled=1 WHERE module_id='terminal'",
                &[],
            )
            .unwrap();
        assert_eq!(
            c.execute(
                "terminal.command",
                json!({"cwd":cwd,"command":"Get-Date","timeout_seconds":0})
            )
            .await
            .unwrap_err(),
            "INVALID_TOOL_INPUT"
        );
        let safe = c
            .execute(
                "terminal.command",
                json!({"cwd":cwd,"command":"Write-Output 'safe fixture'"}),
            )
            .await
            .unwrap();
        assert!(safe["stdout"].as_str().unwrap().contains("safe fixture"));
        assert!(c
            .execute(
                "terminal.command",
                json!({"cwd":cwd,"command":"Remove-Item -LiteralPath 'not-executed'"})
            )
            .await
            .is_err());
        let rows=c.store.query("SELECT status FROM activity_events WHERE tool_id='terminal.command' ORDER BY timestamp",&[]).unwrap();
        assert!(rows
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["status"] == "denied"));
        let path = cwd.join("fixture.ps1");
        std::fs::write(&path, "Write-Output 'snapshot original'").unwrap();
        let prepared = prepare(&c, "script", &json!({"cwd":cwd,"path":path}))
            .await
            .unwrap();
        std::fs::write(&path, "throw 'changed after review'").unwrap();
        assert!(execute(&c, prepared).await.unwrap()["stdout"]
            .as_str()
            .unwrap()
            .contains("snapshot original"));
        for command in [
            "Write-Output ([Environment]::TickCount)",
            "Write-Output $(Remove-Item x)",
            "Write-Output x > file",
            "& 'Get-Date'",
        ] {
            assert_eq!(
                prepare(&c, "command", &json!({"cwd":cwd,"command":command}))
                    .await
                    .unwrap()
                    .risk,
                policy::Risk::Mandatory
            );
        }
        drop(c);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn streaming_ast_profiles_env_errors_limits_and_cancellation() {
        let c = fixture();
        c.store
            .execute(
                "UPDATE module_configs SET enabled=1 WHERE module_id='terminal'",
                &[],
            )
            .unwrap();
        let root = c.root.clone();
        let cwd = root.join("temp");
        let profile = profiles(
            &c,
            "profile_save",
            &json!({"name":"fixture","body":{"env_allowlist":["DEMO_VALUE"]}}),
        )
        .unwrap();
        let input = json!({"cwd":cwd,"command":"Write-Output $env:DEMO_VALUE","profile_id":profile["id"],"body":{"env":{"DEMO_VALUE":"allowed"}}});
        let prepared = prepare(&c, "command", &input).await.unwrap();
        assert_eq!(prepared.risk, policy::Risk::Mandatory);
        assert!(execute(&c, prepared).await.unwrap()["stdout"]
            .as_str()
            .unwrap()
            .contains("allowed"));
        assert!(prepare(&c,"command",&json!({"cwd":cwd,"command":"Get-Date","profile_id":profile["id"],"body":{"env":{"NOT_ALLOWED":"x"}}})).await.is_err());
        let safe=prepare(&c,"command",&json!({"cwd":cwd,"command":"Write-Output 'first'; Start-Sleep -Milliseconds 700; Write-Output 'second'"})).await.unwrap();
        let started = std::time::Instant::now();
        let mut seen = Vec::new();
        let result = run(
            command(&safe.shell, &safe.runner, "execute", &safe.environment),
            json!({"command":safe.command,"cwd":safe.cwd}).to_string(),
            5,
            CancellationToken::new(),
            |channel, line| seen.push((channel.to_string(), line.to_string(), started.elapsed())),
        )
        .await
        .unwrap();
        assert!(result["stdout"].as_str().unwrap().contains("first"));
        let first = seen.iter().find(|v| v.1 == "first").unwrap();
        let second = seen.iter().find(|v| v.1 == "second").unwrap();
        assert!(second.2.saturating_sub(first.2) >= std::time::Duration::from_millis(500));
        let safe = prepare(&c, "command", &json!({"cwd":cwd,"command":"Get-Date"}))
            .await
            .unwrap();
        assert_eq!(safe.risk, policy::Risk::Free);
        let dynamic = prepare(
            &c,
            "command",
            &json!({"cwd":cwd,"command":"Write-Output ([IO.File]::ReadAllText('x'))"}),
        )
        .await
        .unwrap();
        assert_eq!(dynamic.risk, policy::Risk::Mandatory);
        assert!(prepare(&c, "command", &json!({"cwd":cwd,"command":"if ("}))
            .await
            .is_err());
        let bad = prepare(
            &c,
            "command",
            &json!({"cwd":cwd,"command":"throw 'fixture failure'"}),
        )
        .await
        .unwrap();
        assert!(execute(&c, bad)
            .await
            .unwrap_err()
            .starts_with("TERMINAL_EXIT_NONZERO"));
        let slow = prepare(
            &c,
            "command",
            &json!({"cwd":cwd,"command":"Start-Sleep -Seconds 20","timeout_seconds":1}),
        )
        .await
        .unwrap();
        assert_eq!(execute(&c, slow).await.unwrap_err(), "TERMINAL_TIMEOUT");
        let slow = prepare(
            &c,
            "command",
            &json!({"cwd":cwd,"command":"Start-Sleep -Seconds 20","id":"terminal-cancel-fixture"}),
        )
        .await
        .unwrap();
        let copy = c.clone();
        let task = tokio::spawn(async move { execute(&copy, slow).await });
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        cancel(&c, &json!({"id":"terminal-cancel-fixture"})).unwrap();
        assert_eq!(task.await.unwrap().unwrap_err(), "TERMINAL_CANCELLED");
        assert!(c.runs.lock().unwrap().is_empty());
        drop(c);
        std::fs::remove_dir_all(root).unwrap();
    }
}
