//! Scoped GitHub operations. Remote content is untrusted; mutations are reviewed
//! individually and never automatically replayed or switched to another token.
use crate::{asset_files, policy, process, runtime::Core};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap, fs::File, future::Future, path::PathBuf, sync::Arc, time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

pub(crate) const TOOLS: &[&str] = &[
    "git.repos",
    "git.repo",
    "git.issues",
    "git.prs",
    "git.pr",
    "git.actions",
    "git.releases",
    "git.release",
    "git.create_issue",
    "git.merge",
    "git.publish_release",
];
const LIMIT: usize = 4 * 1024 * 1024;
const API_VERSION: &str = "2026-03-10";
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn field<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v[key].as_str().ok_or_else(|| format!("GITHUB_INPUT_{key}"))
}
fn repo_name(s: &str) -> bool {
    let parts: Vec<_> = s.split('/').collect();
    parts.len() == 2
        && parts[0].len() <= 39
        && !parts[0].starts_with(['-', '.'])
        && parts[1].len() <= 100
        && parts.iter().all(|p| {
            !p.is_empty()
                && *p != "."
                && *p != ".."
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        })
}
pub(crate) fn validate_config(v: &Value) -> Result<(), String> {
    let schema = json!({"type":"object","required":["repos"],"properties":{
        "repos":{"type":"array","minItems":1,"maxItems":1000,"uniqueItems":true,"items":{"type":"string","maxLength":140}},
        "auth":{"enum":["prefer_gh","pat"]}},"additionalProperties":false});
    if !jsonschema::validator_for(&schema)
        .map_err(|_| "GITHUB_CONFIG_SCHEMA")?
        .is_valid(v)
        || !v["repos"]
            .as_array()
            .ok_or("GITHUB_REPOS_REQUIRED")?
            .iter()
            .all(|r| r.as_str().is_some_and(repo_name))
    {
        return Err("GITHUB_CONFIG_INVALID".into());
    }
    let mut unique = std::collections::HashSet::new();
    if !v["repos"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| unique.insert(r.as_str().unwrap().to_ascii_lowercase()))
    {
        return Err("GITHUB_CONFIG_DUPLICATE_REPO".into());
    }
    Ok(())
}
pub(crate) fn schema(tool: &str) -> Value {
    let mut props = serde_json::Map::new();
    props.insert(
        "connection".into(),
        json!({"type":"string","format":"uuid","minLength":36,"maxLength":36,"pattern":"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"}),
    );
    let mut required = vec!["connection"];
    if tool != "git.repos" {
        props.insert("repo".into(), json!({"type":"string","minLength":3,"maxLength":140,"pattern":"^[a-zA-Z0-9_][a-zA-Z0-9_.-]*/[a-zA-Z0-9_.-]+$"}));
        required.push("repo");
    }
    if [
        "git.repos",
        "git.issues",
        "git.prs",
        "git.actions",
        "git.releases",
    ]
    .contains(&tool)
    {
        props.insert(
            "page".into(),
            json!({"type":"integer","minimum":1,"maximum":10000}),
        );
        props.insert(
            "per_page".into(),
            json!({"type":"integer","minimum":1,"maximum":100}),
        );
    }
    if ["git.issues", "git.prs"].contains(&tool) {
        props.insert("state".into(), json!({"enum":["open","closed","all"]}));
    }
    if ["git.pr", "git.merge"].contains(&tool) {
        props.insert(
            "number".into(),
            json!({"type":"integer","minimum":1,"maximum":2147483647}),
        );
        required.push("number");
    }
    if ["git.release", "git.publish_release"].contains(&tool) {
        props.insert(
            "release_id".into(),
            json!({"type":"integer","minimum":1,"maximum":9007199254740991u64}),
        );
        required.push("release_id");
    }
    if tool == "git.create_issue" {
        props.insert(
            "title".into(),
            json!({"type":"string","minLength":1,"maxLength":256}),
        );
        props.insert("body".into(), json!({"type":"string","maxLength":60000}));
        required.extend(["title", "body"]);
    }
    if tool == "git.merge" {
        props.insert("method".into(), json!({"enum":["merge","squash","rebase"]}));
    }
    if mutation(tool) {
        props.insert("expected".into(),json!({"type":"object","required":["fingerprint"],"properties":{"fingerprint":{"type":"string","pattern":"^[a-f0-9]{64}$"}},"additionalProperties":false}));
        required.push("expected");
    }
    if !TOOLS.contains(&tool) {
        return json!({"not":{}});
    }
    json!({"type":"object","required":required,"properties":props,"additionalProperties":false})
}
pub(crate) fn mutation(tool: &str) -> bool {
    ["git.create_issue", "git.merge", "git.publish_release"].contains(&tool)
}

#[cfg(test)]
#[derive(Clone, PartialEq)]
pub(crate) struct TestTransport {
    pub base: String,
    pub pat: Option<String>,
    pub gh: Option<(PathBuf, Vec<String>)>,
    pub execution_timeout: Option<Duration>,
}
struct Scope {
    core: Arc<Core>,
    id: String,
    config: Value,
    config_hash: String,
    generation: Value,
    #[cfg(test)]
    fixture: Option<TestTransport>,
}
impl Scope {
    fn new(core: &Arc<Core>, id: &str) -> Result<Self, String> {
        if uuid::Uuid::parse_str(id)
            .map_err(|_| "GITHUB_CONNECTION_ID")?
            .to_string()
            != id
        {
            return Err("GITHUB_CONNECTION_ID".into());
        }
        let config = crate::integrations::connection(core, id, "git")?;
        let mut public = config.clone();
        public
            .as_object_mut()
            .ok_or("GITHUB_CONFIG_INVALID")?
            .remove("secret_ref");
        validate_config(&public)?;
        let s = Self {
            core: core.clone(),
            id: id.into(),
            config_hash: digest(config.to_string().as_bytes()),
            config,
            generation: core
                .store
                .setting(&format!("github.connection_generation.{id}")),
            #[cfg(test)]
            fixture: core
                .github_fixture
                .lock()
                .map_err(|_| "GITHUB_FIXTURE_LOCK")?
                .clone(),
        };
        s.check()?;
        Ok(s)
    }
    fn check(&self) -> Result<(), String> {
        if !self.core.enabled("git") {
            return Err("MODULE_DISABLED: git".into());
        }
        if self.core.store.setting("privacy") == true {
            return Err("PRIVACY_DENIED".into());
        }
        let _credentials = crate::integrations::save_lock(&self.core)?;
        let aliases = self.core.store.query(
            "SELECT id,module_id FROM connections WHERE id=? COLLATE NOCASE",
            &[json!(self.id)],
        )?;
        if aliases.as_array().is_none_or(|rows| rows.len() != 1)
            || aliases[0]["id"] != self.id
            || aliases[0]["module_id"] != "git"
        {
            return Err("GITHUB_CONNECTION_ALIAS_CONFLICT".into());
        }
        let current = crate::integrations::connection(&self.core, &self.id, "git")?;
        if digest(current.to_string().as_bytes()) != self.config_hash
            || self.generation
                != self
                    .core
                    .store
                    .setting(&format!("github.connection_generation.{}", self.id))
        {
            return Err("GITHUB_CONNECTION_CHANGED".into());
        }
        #[cfg(test)]
        if *self
            .core
            .github_fixture
            .lock()
            .map_err(|_| "GITHUB_FIXTURE_LOCK")?
            != self.fixture
        {
            return Err("GITHUB_FIXTURE_CHANGED".into());
        }
        Ok(())
    }
    fn allows(&self, repo: &str) -> bool {
        self.config["repos"].as_array().is_some_and(|v| {
            v.iter()
                .any(|r| r.as_str().is_some_and(|s| s.eq_ignore_ascii_case(repo)))
        })
    }
    async fn guard<T>(&self, future: impl Future<Output = Result<T, String>>) -> Result<T, String> {
        self.check()?;
        tokio::pin!(future);
        let deadline = tokio::time::sleep(Duration::from_secs(30));
        tokio::pin!(deadline);
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        loop {
            tokio::select! { biased;
                _=tick.tick()=>self.check()?,
                _=&mut deadline=>return Err("GITHUB_TIMEOUT".into()),
                result=&mut future=>{ self.check()?; return result; }
            }
        }
    }
}
enum Auth {
    Gh {
        exe: PathBuf,
        args: Vec<String>,
        _leases: Vec<File>,
    },
    Pat(String),
}
struct Session {
    scope: Scope,
    auth: Auth,
    account: Value,
    client: reqwest::Client,
    base: reqwest::Url,
}
struct Response {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Value,
}
fn status_error(status: u16, headers: &BTreeMap<String, String>) -> Result<(), String> {
    if (200..300).contains(&status) {
        return Ok(());
    }
    let e = match status {
        401 => "GITHUB_AUTH_REQUIRED",
        403 if headers
            .get("x-ratelimit-remaining")
            .is_some_and(|s| s == "0")
            || headers.contains_key("retry-after") =>
        {
            "GITHUB_RATE_LIMITED"
        }
        403 => "GITHUB_FORBIDDEN",
        404 => "GITHUB_NOT_FOUND",
        400 => "GITHUB_BAD_REQUEST",
        405 => "GITHUB_METHOD_NOT_ALLOWED",
        409 => "GITHUB_CONFLICT",
        422 => "GITHUB_UNPROCESSABLE",
        429 => "GITHUB_RATE_LIMITED",
        300..=399 => "GITHUB_REDIRECT_DENIED",
        500..=599 => "GITHUB_SERVER_ERROR",
        _ => "GITHUB_HTTP_STATUS",
    };
    Err(e.into())
}
fn decoded(
    status: u16,
    headers: BTreeMap<String, String>,
    bytes: &[u8],
) -> Result<Response, String> {
    status_error(status, &headers)?;
    let content = headers
        .get("content-type")
        .ok_or("GITHUB_CONTENT_TYPE")?
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    if content != "application/json"
        && !(content.starts_with("application/") && content.ends_with("+json"))
    {
        return Err("GITHUB_CONTENT_TYPE".into());
    }
    let body = serde_json::from_slice(bytes).map_err(|_| "GITHUB_MALFORMED_JSON")?;
    Ok(Response {
        status,
        headers,
        body,
    })
}
fn gh_response(code: Option<i32>, stdout: &[u8]) -> Result<Response, String> {
    if code == Some(4) && stdout.is_empty() {
        return Err("GITHUB_AUTH_REQUIRED".into());
    }
    let s = std::str::from_utf8(stdout).map_err(|_| "GITHUB_GH_UTF8")?;
    let (header, body) = s
        .split_once("\r\n\r\n")
        .or_else(|| s.split_once("\n\n"))
        .ok_or("GITHUB_GH_HEADERS")?;
    if header.len() > 65536 {
        return Err("GITHUB_HEADER_LIMIT".into());
    }
    let mut lines = header.lines();
    let first = lines.next().ok_or("GITHUB_GH_HEADERS")?;
    let parts: Vec<_> = first.split_whitespace().collect();
    if parts.len() < 2 || !["HTTP/1.1", "HTTP/2.0", "HTTP/2", "HTTP/3.0"].contains(&parts[0]) {
        return Err("GITHUB_GH_STATUS".into());
    }
    let status = parts[1].parse::<u16>().map_err(|_| "GITHUB_GH_STATUS")?;
    let mut headers = BTreeMap::new();
    let mut count = 0;
    for line in lines {
        count += 1;
        if count > 128 {
            return Err("GITHUB_HEADER_LIMIT".into());
        }
        let (k, v) = line.split_once(':').ok_or("GITHUB_GH_HEADERS")?;
        if !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || v.chars().any(|c| c == '\0' || c == '\r')
        {
            return Err("GITHUB_GH_HEADERS".into());
        }
        let k = k.to_ascii_lowercase();
        if [
            "content-type",
            "link",
            "retry-after",
            "x-ratelimit-remaining",
        ]
        .contains(&k.as_str())
            && headers.insert(k, v.trim().to_string()).is_some()
        {
            return Err("GITHUB_GH_HEADERS".into());
        }
    }
    status_error(status, &headers)?;
    if code != Some(0) {
        return Err("GITHUB_GH_EXIT".into());
    }
    decoded(status, headers, body.as_bytes())
}
async fn bounded<R: tokio::io::AsyncRead + Unpin>(
    mut r: R,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = r.read(&mut buf).await.map_err(|_| "GITHUB_PROCESS_READ")?;
        if n == 0 {
            return Ok(bytes);
        }
        if bytes.len() + n > limit {
            return Err("GITHUB_RESPONSE_LIMIT".into());
        }
        bytes.extend_from_slice(&buf[..n]);
    }
}
impl Session {
    fn source(&self) -> &'static str {
        match self.auth {
            Auth::Gh { .. } => "gh",
            Auth::Pat(_) => "credential_manager_pat",
        }
    }
    async fn request(
        &self,
        method: &str,
        endpoint: &str,
        body: Option<&Value>,
    ) -> Result<Response, String> {
        self.scope.check()?;
        if !endpoint.starts_with('/')
            || endpoint.contains(['{', '}', '\\', '#'])
            || endpoint
                .split('?')
                .next()
                .unwrap_or("")
                .split('/')
                .any(|p| p == "." || p == "..")
        {
            return Err("GITHUB_ENDPOINT_SCOPE".into());
        }
        let payload = body.map(Value::to_string);
        if payload.as_ref().is_some_and(|p| p.len() > 256 * 1024) {
            return Err("GITHUB_REQUEST_LIMIT".into());
        }
        self.scope
            .guard(async {
                match &self.auth {
                    Auth::Pat(token) => {
                        let url = self.base.join(endpoint).map_err(|_| "GITHUB_URL")?;
                        if url.origin() != self.base.origin() {
                            return Err("GITHUB_ENDPOINT_SCOPE".into());
                        }
                        let mut req = self
                            .client
                            .request(
                                reqwest::Method::from_bytes(method.as_bytes())
                                    .map_err(|_| "GITHUB_METHOD")?,
                                url,
                            )
                            .bearer_auth(token)
                            .header("Accept", "application/vnd.github+json")
                            .header("X-GitHub-Api-Version", API_VERSION)
                            .header("User-Agent", "AlfredPet/0.1.0");
                        if let Some(body) = body {
                            req = req.json(body);
                        }
                        let mut response = req.send().await.map_err(|e| {
                            if e.is_timeout() {
                                "GITHUB_TIMEOUT"
                            } else {
                                "GITHUB_NETWORK"
                            }
                        })?;
                        let status = response.status().as_u16();
                        let mut headers = BTreeMap::new();
                        for k in [
                            "content-type",
                            "link",
                            "retry-after",
                            "x-ratelimit-remaining",
                        ] {
                            if let Some(v) = response.headers().get(k) {
                                let v = v.to_str().map_err(|_| "GITHUB_HEADER_ENCODING")?;
                                if v.len() > 65536 {
                                    return Err("GITHUB_HEADER_LIMIT".into());
                                }
                                headers.insert(k.into(), v.into());
                            }
                        }
                        status_error(status, &headers)?;
                        if response.content_length().is_some_and(|n| n > LIMIT as u64) {
                            return Err("GITHUB_RESPONSE_LIMIT".into());
                        }
                        let mut bytes = Vec::new();
                        while let Some(chunk) =
                            response.chunk().await.map_err(|_| "GITHUB_BODY_READ")?
                        {
                            if bytes.len() + chunk.len() > LIMIT {
                                return Err("GITHUB_RESPONSE_LIMIT".into());
                            }
                            bytes.extend_from_slice(&chunk);
                        }
                        decoded(status, headers, &bytes)
                    }
                    Auth::Gh { exe, args, .. } => {
                        let mut cmd = Command::new(exe);
                        cmd.env_clear();
                        for key in [
                            "SystemRoot",
                            "WINDIR",
                            "PATH",
                            "USERPROFILE",
                            "HOMEDRIVE",
                            "HOMEPATH",
                            "APPDATA",
                            "LOCALAPPDATA",
                            "TEMP",
                            "TMP",
                        ] {
                            if let Some(v) = std::env::var_os(key) {
                                cmd.env(key, v);
                            }
                        }
                        cmd.env("GH_PROMPT_DISABLED", "1")
                            .env("GH_NO_UPDATE_NOTIFIER", "1")
                            .env("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1")
                            .env("GH_COLOR_LABELS", "0")
                            .env("NO_COLOR", "1")
                            .env("GH_PAGER", "")
                            .current_dir(&self.scope.core.root);
                        cmd.args(args).args([
                            "api",
                            "--hostname",
                            "github.com",
                            "--method",
                            method,
                            "--include",
                            "--header",
                            "Accept: application/vnd.github+json",
                            "--header",
                            &format!("X-GitHub-Api-Version: {API_VERSION}"),
                        ]);
                        if payload.is_some() {
                            cmd.args(["--input", "-"]);
                        }
                        cmd.arg(endpoint);
                        #[cfg(windows)]
                        cmd.creation_flags(0x08000000);
                        cmd.kill_on_drop(true)
                            .stdin(std::process::Stdio::piped())
                            .stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped());
                        let mut child = cmd.spawn().map_err(|_| "GITHUB_GH_SPAWN")?;
                        #[cfg(windows)]
                        let _job = crate::windows_native::ProcessJob::attach(&child)
                            .map_err(|_| "GITHUB_PROCESS_JOB")?;
                        let stdout = child.stdout.take().ok_or("GITHUB_PROCESS_STDOUT")?;
                        let stderr = child.stderr.take().ok_or("GITHUB_PROCESS_STDERR")?;
                        let mut stdin = child.stdin.take().ok_or("GITHUB_PROCESS_STDIN")?;
                        let (status, out, _) = tokio::try_join!(
                            async {
                                child
                                    .wait()
                                    .await
                                    .map_err(|_| "GITHUB_PROCESS_WAIT".to_string())
                            },
                            bounded(stdout, LIMIT + 65536),
                            bounded(stderr, 256 * 1024),
                            async {
                                if let Some(p) = payload {
                                    stdin
                                        .write_all(p.as_bytes())
                                        .await
                                        .map_err(|_| "GITHUB_PROCESS_STDIN")?;
                                }
                                drop(stdin);
                                Ok::<_, String>(())
                            }
                        )
                        .map(|(s, o, e, _)| (s, o, e))?;
                        gh_response(status.code(), &out)
                    }
                }
            })
            .await
    }
    async fn account(&self) -> Result<Value, String> {
        let r = self.request("GET", "/user", None).await?;
        let id = r.body["id"]
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or("GITHUB_ACCOUNT_RESPONSE")?;
        let login = field(&r.body, "login")?;
        if login.len() > 100
            || login.is_empty()
            || !login
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("GITHUB_ACCOUNT_RESPONSE".into());
        }
        Ok(json!({"id":id,"login":login}))
    }
    async fn recheck_account(&self) -> Result<(), String> {
        if self.account().await? != self.account {
            return Err("GITHUB_ACCOUNT_CHANGED".into());
        }
        Ok(())
    }
}
fn find_gh() -> Option<PathBuf> {
    std::env::var_os("PATH")
        .and_then(|s| {
            std::env::split_paths(&s)
                .filter(|p| p.is_absolute())
                .map(|p| p.join(if cfg!(windows) { "gh.exe" } else { "gh" }))
                .find(|p| p.is_file())
        })
        .and_then(|p| p.canonicalize().ok())
}
fn pin_exe(exe: PathBuf, args: Vec<String>) -> Result<Auth, String> {
    let (file, _) =
        crate::git_local::executable_lease(&exe).map_err(|_| "GITHUB_EXECUTABLE_LEASE")?;
    let mut leases = vec![file];
    for p in exe.ancestors().skip(1) {
        leases.push(asset_files::directory(p).map_err(|_| "GITHUB_EXECUTABLE_DIRECTORY")?);
    }
    Ok(Auth::Gh {
        exe,
        args,
        _leases: leases,
    })
}
async fn session(core: &Arc<Core>, input: &Value) -> Result<Session, String> {
    let scope = Scope::new(core, field(input, "connection")?)?;
    if let Some(repo) = input["repo"].as_str() {
        if !repo_name(repo) || !scope.allows(repo) {
            return Err("GITHUB_REPOSITORY_SCOPE".into());
        }
    }
    let client = crate::http_client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "GITHUB_HTTP_CLIENT")?;
    #[allow(unused_mut)]
    let mut base = reqwest::Url::parse("https://api.github.com/").map_err(|_| "GITHUB_BASE")?;
    #[cfg(test)]
    if let Some(f) = &scope.fixture {
        base = reqwest::Url::parse(&f.base).map_err(|_| "GITHUB_FIXTURE_BASE")?;
        if base.scheme() != "http" || base.host_str() != Some("127.0.0.1") {
            return Err("GITHUB_FIXTURE_BASE".into());
        }
    }
    let prefer = scope.config["auth"] != "pat";
    let gh = if prefer {
        #[cfg(test)]
        {
            if let Some(f) = &scope.fixture {
                f.gh.clone()
            } else {
                find_gh().map(|p| (p, vec![]))
            }
        }
        #[cfg(not(test))]
        {
            find_gh().map(|p| (p, vec![]))
        }
    } else {
        None
    };
    let mut s = Session {
        scope,
        auth: Auth::Pat(String::new()),
        account: Value::Null,
        client,
        base,
    };
    if let Some((exe, args)) = gh {
        s.auth = pin_exe(exe, args)?;
        match s.account().await {
            Ok(a) => {
                s.account = a;
                return Ok(s);
            }
            Err(e) if e == "GITHUB_AUTH_REQUIRED" => (),
            Err(e) => return Err(e),
        }
    }
    #[cfg(test)]
    let fixture_token = s.scope.fixture.as_ref().and_then(|f| f.pat.clone());
    #[cfg(not(test))]
    let fixture_token: Option<String> = None;
    let token = if let Some(t) = fixture_token {
        t
    } else {
        let alias = field(&s.scope.config, "secret_ref")?;
        if alias != format!("connection.{}", s.scope.id) {
            return Err("GITHUB_CREDENTIAL_SCOPE".into());
        }
        let secret = s
            .scope
            .guard(async {
                process::native(&core.native, "secret.get", &json!({"alias":alias}))
                    .await
                    .map_err(|_| "GITHUB_PAT_UNAVAILABLE".into())
            })
            .await?;
        field(&secret, "value")?.to_string()
    };
    if token.len() < 8
        || token.len() > 2048
        || token
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b < 32 || b == 127)
    {
        return Err("GITHUB_PAT_INVALID".into());
    }
    s.auth = Auth::Pat(token);
    s.account = s.account().await?;
    Ok(s)
}

fn int(v: &Value, key: &str) -> Result<Value, String> {
    let n = v[key]
        .as_u64()
        .filter(|n| *n > 0 && *n <= 9007199254740991)
        .ok_or("GITHUB_REMOTE_ID")?;
    Ok(json!(n))
}
fn string(v: &Value, key: &str, limit: usize) -> Result<Value, String> {
    let s = v[key].as_str().ok_or("GITHUB_REMOTE_STRING")?;
    if s.len() > limit || s.contains('\0') {
        return Err("GITHUB_REMOTE_STRING_LIMIT".into());
    }
    Ok(json!(s))
}
fn boolean(v: &Value, key: &str) -> Result<Value, String> {
    Ok(json!(v[key].as_bool().ok_or("GITHUB_REMOTE_BOOLEAN")?))
}
fn optional_string(v: &Value, key: &str, limit: usize) -> Result<Value, String> {
    if v[key].is_null() {
        Ok(Value::Null)
    } else {
        string(v, key, limit)
    }
}
fn body(v: &Value) -> Result<Value, String> {
    if v["body"].is_null() {
        Ok(json!(""))
    } else {
        string(v, "body", 256 * 1024)
    }
}
fn oid(v: &Value) -> Result<Value, String> {
    let s = v.as_str().ok_or("GITHUB_REMOTE_SHA")?;
    if s.len() != 40 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("GITHUB_REMOTE_SHA".into());
    }
    Ok(json!(s.to_ascii_lowercase()))
}
fn repository(v: &Value, name: &str) -> Result<Value, String> {
    let full = string(v, "full_name", 140)?;
    if !full.as_str().is_some_and(|s| s.eq_ignore_ascii_case(name)) {
        return Err("GITHUB_REPOSITORY_MOVED".into());
    }
    Ok(
        json!({"id":int(v,"id")?,"full_name":full,"private":boolean(v,"private")?,"archived":boolean(v,"archived")?,"disabled":boolean(v,"disabled")?,"has_issues":boolean(v,"has_issues")?,"default_branch":string(v,"default_branch",1024)?,"updated_at":string(v,"updated_at",128)?,"push":v["permissions"]["push"].as_bool()}),
    )
}
fn issue(v: &Value) -> Result<Value, String> {
    Ok(
        json!({"id":int(v,"id")?,"number":int(v,"number")?,"title":string(v,"title",4096)?,"body":body(v)?,"state":string(v,"state",32)?,"url":string(v,"html_url",4096)?,"updated_at":string(v,"updated_at",128)?}),
    )
}
fn pull(v: &Value, repo: &Value) -> Result<Value, String> {
    if v["base"]["repo"]["id"] != repo["id"] {
        return Err("GITHUB_PR_REPOSITORY".into());
    }
    Ok(
        json!({"id":int(v,"id")?,"number":int(v,"number")?,"title":string(v,"title",4096)?,"body":body(v)?,"state":string(v,"state",32)?,"draft":boolean(v,"draft")?,"merged":boolean(v,"merged")?,"mergeable":v["mergeable"].as_bool(),"head_sha":oid(&v["head"]["sha"] )?,"base_sha":oid(&v["base"]["sha"] )?,"head_ref":string(&v["head"],"ref",1024)?,"base_ref":string(&v["base"],"ref",1024)?,"updated_at":string(v,"updated_at",128)?}),
    )
}
fn release(v: &Value) -> Result<Value, String> {
    let assets = v["assets"]
        .as_array()
        .filter(|a| a.len() <= 100)
        .ok_or("GITHUB_RELEASE_ASSETS_LIMIT")?;
    let assets:Result<Vec<_>,String>=assets.iter().map(|a|Ok(json!({"id":int(a,"id")?,"name":string(a,"name",4096)?,"size":a["size"].as_u64().ok_or("GITHUB_RELEASE_ASSET_SIZE")?,"state":string(a,"state",128)?,"digest":optional_string(a,"digest",512)?,"updated_at":string(a,"updated_at",128)?}))).collect();
    Ok(
        json!({"id":int(v,"id")?,"name":if v["name"].is_null(){json!("")}else{string(v,"name",4096)?},"body":body(v)?,"tag_name":string(v,"tag_name",1024)?,"target_commitish":string(v,"target_commitish",1024)?,"draft":boolean(v,"draft")?,"prerelease":boolean(v,"prerelease")?,"immutable":boolean(v,"immutable")?,"published_at":optional_string(v,"published_at",128)?,"assets":assets?}),
    )
}
struct Snapshot {
    value: Value,
    fingerprint: String,
}
async fn snapshot(s: &Session, tool: &str, input: &Value) -> Result<Snapshot, String> {
    let name = field(input, "repo")?;
    let r = s.request("GET", &format!("/repos/{name}"), None).await?;
    let repo = repository(&r.body, name)?;
    let target = match tool {
        "git.pr" | "git.merge" => {
            let n = input["number"].as_u64().ok_or("GITHUB_NUMBER")?;
            let r = s
                .request("GET", &format!("/repos/{name}/pulls/{n}"), None)
                .await?;
            let p = pull(&r.body, &repo)?;
            if p["number"] != n {
                return Err("GITHUB_PR_NUMBER".into());
            }
            p
        }
        "git.release" | "git.publish_release" => {
            let n = input["release_id"].as_u64().ok_or("GITHUB_RELEASE_ID")?;
            let r = s
                .request("GET", &format!("/repos/{name}/releases/{n}"), None)
                .await?;
            let rel = release(&r.body)?;
            if rel["id"] != n {
                return Err("GITHUB_RELEASE_ID".into());
            }
            rel
        }
        _ => Value::Null,
    };
    let value = json!({"repository":repo,"target":target});
    let fingerprint=digest(json!({"config":s.scope.config_hash,"generation":s.scope.generation,"account":s.account,"auth":s.source(),"snapshot":value}).to_string().as_bytes());
    Ok(Snapshot { value, fingerprint })
}
pub(crate) struct Prepared {
    s: Session,
    tool: String,
    input: Value,
    snapshot: Option<Snapshot>,
    pub risk: policy::Risk,
}
impl Prepared {
    pub fn review(&self) -> Value {
        json!({"connection":self.s.scope.id,"repo":self.input["repo"],"operation":self.tool,"authentication":self.s.source(),"account":self.s.account,"input":self.input,"review_content":self.snapshot.as_ref().map(|s|s.value.clone()),"warning":"Every write is individually approved. No automatic retry, remote rollback or isolation against arbitrary external writers. Merge binds head SHA; release publication has a client recheck without a server-wide release CAS."})
    }
}
pub(crate) async fn prepare(
    core: &Arc<Core>,
    tool: &str,
    input: &Value,
) -> Result<Prepared, String> {
    tokio::time::timeout(Duration::from_secs(60), async {
        if !TOOLS.contains(&tool)
            || !jsonschema::validator_for(&schema(tool))
                .map_err(|_| "GITHUB_SCHEMA")?
                .is_valid(input)
        {
            return Err("GITHUB_INPUT_INVALID".into());
        }
        uuid::Uuid::parse_str(field(input, "connection")?).map_err(|_| "GITHUB_CONNECTION_ID")?;
        if mutation(tool) && policy::contains_secret(&input.to_string()) {
            return Err("GITHUB_SENSITIVE_INPUT".into());
        }
        let s = session(core, input).await?;
        let snap = if mutation(tool) {
            let a = snapshot(&s, tool, input).await?;
            let b = snapshot(&s, tool, input).await?;
            if a.fingerprint != b.fingerprint || input["expected"]["fingerprint"] != b.fingerprint {
                return Err("GITHUB_STALE_OBSERVATION".into());
            }
            let r = &b.value["repository"];
            if r["archived"] == true || r["disabled"] == true {
                return Err("GITHUB_REPOSITORY_READ_ONLY".into());
            }
            if tool == "git.create_issue" && r["has_issues"] != true {
                return Err("GITHUB_ISSUES_DISABLED".into());
            }
            if tool == "git.merge"
                && (r["push"] != true
                    || b.value["target"]["state"] != "open"
                    || b.value["target"]["draft"] == true
                    || b.value["target"]["merged"] == true
                    || b.value["target"]["mergeable"] != true)
            {
                return Err("GITHUB_PR_NOT_MERGEABLE".into());
            }
            if tool == "git.publish_release"
                && (r["push"] != true
                    || b.value["target"]["draft"] != true
                    || b.value["target"]["immutable"] != false
                    || !b.value["target"]["published_at"].is_null())
            {
                return Err("GITHUB_RELEASE_NOT_PUBLISHABLE".into());
            }
            if policy::contains_secret(&b.value.to_string()) {
                return Err("GITHUB_SENSITIVE_REVIEW".into());
            }
            Some(b)
        } else {
            None
        };
        Ok(Prepared {
            s,
            tool: tool.into(),
            input: input.clone(),
            snapshot: snap,
            risk: if mutation(tool) {
                policy::Risk::Mandatory
            } else {
                policy::Risk::Free
            },
        })
    })
    .await
    .map_err(|_| "GITHUB_PREPARATION_TIMEOUT".to_string())?
}
fn next_page(s: &Session, r: &Response, endpoint: &str, page: u64) -> Result<Option<u64>, String> {
    let Some(link) = r.headers.get("link") else {
        return Ok(None);
    };
    if link.len() > 65536 {
        return Err("GITHUB_HEADER_LIMIT".into());
    }
    let expected = s.base.join(endpoint).map_err(|_| "GITHUB_PAGE_SCOPE")?;
    let mut next = None;
    for part in link.split(',') {
        if !part.split(';').skip(1).any(|a| a.trim() == "rel=\"next\"") {
            continue;
        }
        if next.is_some() {
            return Err("GITHUB_PAGE_SCOPE".into());
        }
        let raw = part.split(';').next().unwrap_or("").trim();
        let raw = raw
            .strip_prefix('<')
            .and_then(|x| x.strip_suffix('>'))
            .ok_or("GITHUB_PAGE_SCOPE")?;
        let u = reqwest::Url::parse(raw).map_err(|_| "GITHUB_PAGE_SCOPE")?;
        if u.origin() != expected.origin()
            || u.path() != expected.path()
            || !u.username().is_empty()
            || u.password().is_some()
            || u.fragment().is_some()
        {
            return Err("GITHUB_PAGE_SCOPE".into());
        }
        let mut a: BTreeMap<String, String> = expected
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let mut b = BTreeMap::new();
        for (k, v) in u.query_pairs() {
            if b.insert(k.into_owned(), v.into_owned()).is_some() {
                return Err("GITHUB_PAGE_SCOPE".into());
            }
        }
        let n = b
            .get("page")
            .ok_or("GITHUB_PAGE_SCOPE")?
            .parse::<u64>()
            .map_err(|_| "GITHUB_PAGE_SCOPE")?;
        if n != page + 1 || n > 10000 {
            return Err("GITHUB_PAGE_SCOPE".into());
        }
        a.insert("page".into(), n.to_string());
        if a != b {
            return Err("GITHUB_PAGE_SCOPE".into());
        }
        next = Some(n);
    }
    Ok(next)
}
pub(crate) async fn execute(p: Prepared) -> Result<Value, String> {
    let tool = p.tool.clone();
    #[cfg(test)]
    let deadline =
        p.s.scope
            .fixture
            .as_ref()
            .and_then(|f| f.execution_timeout)
            .unwrap_or(Duration::from_secs(90));
    #[cfg(not(test))]
    let deadline = Duration::from_secs(90);
    tokio::time::timeout(deadline, execute_inner(p))
        .await
        .map_err(|_| timeout_error(&tool))?
}
pub(crate) fn timeout_error(tool: &str) -> String {
    if mutation(tool) {
        "GITHUB_MUTATION_OUTCOME_UNCERTAIN: GITHUB_EXECUTION_TIMEOUT; inspect remote state before retry".into()
    } else {
        "GITHUB_EXECUTION_TIMEOUT".into()
    }
}
async fn execute_inner(p: Prepared) -> Result<Value, String> {
    let _lock = p.s.scope.core.github_actions.lock().await;
    p.s.scope.check()?;
    p.s.recheck_account().await?;
    if let Some(observed) = &p.snapshot {
        let now = snapshot(&p.s, &p.tool, &p.input).await?;
        if now.fingerprint != observed.fingerprint {
            return Err("GITHUB_STALE_OBSERVATION".into());
        }
        let name = field(&p.input, "repo")?;
        let (method, path, body) = match p.tool.as_str() {
            "git.create_issue" => (
                "POST",
                format!("/repos/{name}/issues"),
                json!({"title":p.input["title"],"body":p.input["body"]}),
            ),
            "git.merge" => (
                "PUT",
                format!("/repos/{name}/pulls/{}/merge", p.input["number"]),
                json!({"sha":now.value["target"]["head_sha"],"merge_method":p.input["method"].as_str().unwrap_or("merge")}),
            ),
            "git.publish_release" => (
                "PATCH",
                format!("/repos/{name}/releases/{}", p.input["release_id"]),
                json!({"draft":false}),
            ),
            _ => return Err("GITHUB_WRITE_UNKNOWN".into()),
        };
        let r = p.s.request(method, &path, Some(&body)).await.map_err(|e| {
            if [
                "GITHUB_AUTH_REQUIRED",
                "GITHUB_FORBIDDEN",
                "GITHUB_NOT_FOUND",
                "GITHUB_CONFLICT",
                "GITHUB_UNPROCESSABLE",
                "GITHUB_RATE_LIMITED",
                "GITHUB_BAD_REQUEST",
                "GITHUB_METHOD_NOT_ALLOWED",
            ]
            .contains(&e.as_str())
            {
                e
            } else {
                format!("GITHUB_MUTATION_OUTCOME_UNCERTAIN: {e}; inspect remote state before retry")
            }
        })?;
        let result = (|| -> Result<Value, String> {
            match p.tool.as_str() {
                "git.create_issue" => {
                    if r.status != 201 || r.body.get("pull_request").is_some() {
                        return Err("GITHUB_ISSUE_WRITE_RESPONSE".into());
                    }
                    Ok(json!({"issue":issue(&r.body)?}))
                }
                "git.merge" => {
                    if r.body["merged"] != true {
                        return Err("GITHUB_MERGE_RESPONSE".into());
                    }
                    Ok(json!({"merged":true,"sha":oid(&r.body["sha"])?}))
                }
                "git.publish_release" => {
                    let rel = release(&r.body)?;
                    if rel["id"] != p.input["release_id"] || rel["draft"] != false {
                        return Err("GITHUB_RELEASE_WRITE_RESPONSE".into());
                    }
                    Ok(json!({"release":rel}))
                }
                _ => Err("GITHUB_WRITE_UNKNOWN".into()),
            }
        })()
        .map_err(|e| {
            format!("GITHUB_MUTATION_OUTCOME_UNCERTAIN: {e}; inspect remote state before retry")
        })?;
        return Ok(
            json!({"operation":p.tool,"repo":name,"account":p.s.account,"authentication":p.s.source(),"result":policy::redact(&result),"remote_content_untrusted":true,"durable_remote_undo_available":false}),
        );
    }
    if ["git.repo", "git.pr", "git.release"].contains(&p.tool.as_str()) {
        let snap = snapshot(&p.s, &p.tool, &p.input).await?;
        return Ok(
            json!({"data":policy::redact(&snap.value),"expected":{"fingerprint":snap.fingerprint},"authentication":p.s.source(),"account":p.s.account,"remote_content_untrusted":true}),
        );
    }
    let page = p.input["page"].as_u64().unwrap_or(1);
    let per = p.input["per_page"].as_u64().unwrap_or(30);
    let name = p.input["repo"].as_str().unwrap_or("");
    let endpoint = if p.tool == "git.repos" {
        format!("/user/repos?sort=full_name&direction=asc&per_page={per}&page={page}")
    } else {
        let r = p.s.request("GET", &format!("/repos/{name}"), None).await?;
        repository(&r.body, name)?;
        let suffix = match p.tool.as_str() {
            "git.issues" => "issues",
            "git.prs" => "pulls",
            "git.actions" => "actions/runs",
            "git.releases" => "releases",
            _ => return Err("GITHUB_READ_UNKNOWN".into()),
        };
        let state = if ["git.issues", "git.prs"].contains(&p.tool.as_str()) {
            format!("&state={}", p.input["state"].as_str().unwrap_or("open"))
        } else {
            String::new()
        };
        format!("/repos/{name}/{suffix}?per_page={per}&page={page}{state}")
    };
    let r = p.s.request("GET", &endpoint, None).await?;
    let next = next_page(&p.s, &r, &endpoint, page)?;
    let raw = if p.tool == "git.actions" {
        &r.body["workflow_runs"]
    } else {
        &r.body
    };
    let raw = raw
        .as_array()
        .filter(|a| a.len() <= per as usize)
        .ok_or("GITHUB_PAGE_RESPONSE_LIMIT")?;
    let mut items = Vec::new();
    for item in raw {
        let v = match p.tool.as_str() {
            "git.repos" => {
                let n = field(item, "full_name")?;
                if !p.s.scope.allows(n) {
                    continue;
                }
                repository(item, n)?
            }
            "git.issues" => {
                if item.get("pull_request").is_some() {
                    continue;
                }
                issue(item)?
            }
            "git.prs" => {
                json!({"id":int(item,"id")?,"number":int(item,"number")?,"title":string(item,"title",4096)?,"body":body(item)?,"state":string(item,"state",32)?,"draft":boolean(item,"draft")?,"head_sha":oid(&item["head"]["sha"] )?,"base_sha":oid(&item["base"]["sha"] )?})
            }
            "git.actions" => {
                json!({"id":int(item,"id")?,"name":string(item,"name",4096)?,"status":string(item,"status",128)?,"conclusion":optional_string(item,"conclusion",128)?,"head_sha":oid(&item["head_sha"] )?,"run_number":int(item,"run_number")?,"url":string(item,"html_url",4096)?})
            }
            "git.releases" => release(item)?,
            _ => return Err("GITHUB_READ_UNKNOWN".into()),
        };
        items.push(policy::redact(&v));
    }
    Ok(
        json!({"items":items,"page":page,"per_page":per,"next_page":next,"authentication":p.s.source(),"account":p.s.account,"remote_content_untrusted":true,"pages_are_not_transactional_snapshot":true}),
    )
}

#[cfg(all(test, windows))]
#[path = "github_tests.rs"]
mod tests;
