//! A negotiated MCP exchange. Negotiation only probes discovery/initialize;
//! business requests are never replayed to change era, status or session.
use super::protocol::{self, HeaderField};
use crate::{integrations, process};
use reqwest::{header::HeaderMap, StatusCode};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[path = "mcp_listen_transport.rs"]
mod listener;
const LIMIT: usize = protocol::LIMIT;
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Auto,
    Modern,
    Legacy,
}
fn mode(config: &Value) -> Result<Mode, String> {
    match config.get("protocol").and_then(Value::as_str) {
        None if config.get("protocol").is_none() => Ok(Mode::Auto),
        Some("auto") => Ok(Mode::Auto),
        Some("modern") => Ok(Mode::Modern),
        Some("legacy") => Ok(Mode::Legacy),
        _ => Err("MCP_PROTOCOL_SELECTION".into()),
    }
}
enum Era {
    Modern,
    Legacy(String),
}
fn initialize() -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":protocol::LEGACY[0],"capabilities":{},"clientInfo":{"name":"alfredpet","version":"0.1.0"}}})
}
fn legacy_version(value: &Value) -> Result<String, String> {
    let version = value["protocolVersion"]
        .as_str()
        .ok_or("MCP_INITIALIZE_INVALID")?;
    if !protocol::LEGACY.contains(&version)
        || !value["capabilities"].is_object()
        || !value["serverInfo"].is_object()
    {
        return Err("MCP_PROTOCOL_UNSUPPORTED".into());
    }
    Ok(version.into())
}
fn notification(value: &Value) -> Result<(), String> {
    if value["jsonrpc"] != "2.0"
        || value.get("id").is_some()
        || value.get("result").is_some()
        || value.get("error").is_some()
        || value["method"]
            .as_str()
            .is_none_or(|s| s.is_empty() || s.len() > 128)
        || value.get("params").is_some_and(|p| !p.is_object())
    {
        return Err("MCP_UNEXPECTED_REQUEST_OR_RESPONSE".into());
    }
    Ok(())
}
struct Stdio {
    child: tokio::process::Child,
    input: Option<tokio::process::ChildStdin>,
    output: BufReader<tokio::process::ChildStdout>,
    bytes: usize,
    messages: usize,
    pending: Option<(Value, String)>,
    #[cfg(windows)]
    _job: crate::windows_native::ProcessJob,
}
impl Stdio {
    fn spawn(
        config: &Value,
        env: &std::collections::BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let mut command =
            tokio::process::Command::new(config["command"].as_str().ok_or("MCP_COMMAND")?);
        if let Some(args) = config.get("args") {
            for arg in args.as_array().ok_or("MCP_ARGS")? {
                command.arg(arg.as_str().ok_or("MCP_ARG")?);
            }
        }
        command.env_clear();
        for key in ["PATH", "SystemRoot", "TEMP", "TMP", "USERPROFILE"] {
            if let Ok(value) = std::env::var(key) {
                command.env(key, value);
            }
        }
        process::sanitize(&mut command);
        // Only references resolved from the owned private envelope are added.
        command.envs(env);
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut child = command.spawn().map_err(|_| "MCP_SPAWN")?;
        #[cfg(windows)]
        let job = crate::windows_native::ProcessJob::attach(&child)?;
        let input = child.stdin.take().ok_or("MCP_STDIN")?;
        let output = BufReader::new(child.stdout.take().ok_or("MCP_STDOUT")?);
        Ok(Self {
            child,
            input: Some(input),
            output,
            bytes: 0,
            messages: 0,
            pending: None,
            #[cfg(windows)]
            _job: job,
        })
    }
    async fn write(&mut self, value: &Value) -> Result<(), String> {
        let mut encoded = serde_json::to_vec(value).map_err(|_| "MCP_JSON")?;
        if encoded.len() > LIMIT {
            return Err("MCP_REQUEST_LIMIT".into());
        }
        encoded.push(b'\n');
        self.input
            .as_mut()
            .ok_or("MCP_STDIN_CLOSED")?
            .write_all(&encoded)
            .await
            .map_err(|_| "MCP_WRITE".into())
    }
    async fn frame(
        &mut self,
        frame_limit: usize,
        guard: Option<&crate::mcp_request_guard::Guard>,
    ) -> Result<Value, String> {
        let mut line = Vec::new();
        loop {
            if let Some(guard) = guard {
                guard.check()?;
            }
            let buf = self.output.fill_buf().await.map_err(|_| "MCP_READ")?;
            if buf.is_empty() {
                return Err(if line.is_empty() {
                    "MCP_EXIT"
                } else {
                    "MCP_STDIO_FRAME"
                }
                .into());
            }
            let end = buf.iter().position(|b| *b == b'\n').map(|n| n + 1);
            let count = end.unwrap_or(buf.len());
            self.bytes = self.bytes.saturating_add(count);
            if self.bytes > LIMIT {
                return Err("MCP_RESPONSE_LIMIT".into());
            }
            if line.len().saturating_add(count) > frame_limit {
                return Err("MCP_STDIO_FRAME_LIMIT".into());
            }
            line.extend_from_slice(&buf[..count]);
            self.output.consume(count);
            if end.is_some() {
                break;
            }
        }
        serde_json::from_slice(&line).map_err(|_| "MCP_JSON".into())
    }
    async fn read(&mut self, id: &Value) -> Result<Value, String> {
        loop {
            let value = self.frame(LIMIT, None).await?;
            self.messages += 1;
            if self.messages > 256 {
                return Err("MCP_NOTIFICATION_LIMIT".into());
            }
            if value.get("id").is_some() {
                protocol::reply(&value, id)?;
                return Ok(value);
            }
            notification(&value)?;
        }
    }
    async fn exchange(&mut self, value: &Value) -> Result<Value, String> {
        self.pending = Some((
            value["id"].clone(),
            value["method"].as_str().ok_or("MCP_REQUEST_METHOD")?.into(),
        ));
        self.write(value).await?;
        let result = self.read(&value["id"]).await?;
        self.pending = None;
        Ok(result)
    }
    async fn initialize(&mut self) -> Result<Value, String> {
        let value = self.exchange(&initialize()).await?;
        let result = protocol::complete(&value, &json!(1), false)?.clone();
        legacy_version(&result)?;
        self.write(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await?;
        Ok(result)
    }
    async fn close(&mut self) {
        if let Some((id, method)) = self.pending.take() {
            // Legacy initialize is not cancellable. A bounded cancellation
            // notification precedes EOF for all other in-flight requests.
            if method != "initialize" {
                let notice = json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":id,"reason":"AlfredPet exchange stopped"}});
                let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, self.write(&notice)).await;
            }
        }
        // EOF precedes bounded wait and termination. Dropped futures also close
        // stdin; kill_on_drop/Windows Job terminates the owned process tree.
        if let Some(mut input) = self.input.take() {
            let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, input.shutdown()).await;
            drop(input);
        }
        if !matches!(
            tokio::time::timeout(SHUTDOWN_TIMEOUT, self.child.wait()).await,
            Ok(Ok(_))
        ) {
            let _ = self.child.kill().await;
        }
    }
}
struct Http {
    client: reqwest::Client,
    url: reqwest::Url,
    auth: HeaderMap,
    session: Option<String>,
    version: Option<String>,
    guard: Option<crate::mcp_request_guard::Guard>,
}
impl Http {
    async fn probe(&self, mode: Mode) -> Result<Option<Value>, String> {
        let response = self
            .post(&protocol::discover(&json!(0))?, &[], true)
            .await?;
        let status = response.status();
        if status == StatusCode::BAD_REQUEST {
            let bytes = integrations::bounded_body(response, LIMIT).await?;
            let value = serde_json::from_slice::<Value>(&bytes).ok();
            if let Some(value) = &value {
                // Even an invalid envelope cannot turn a recognized modern
                // error into authority to downgrade the protocol era.
                if value["error"]["code"]
                    .as_i64()
                    .is_some_and(protocol::modern_error)
                {
                    return protocol::discovery(value, &json!(0)).map(Some);
                }
                if value.get("result").is_some() {
                    return Err("MCP_DISCOVERY_STATUS_CONTRADICTION".into());
                }
            }
            // Legacy servers may reject pre-initialize probes with empty or
            // implementation-defined bodies/codes. No business call was sent.
            return if mode == Mode::Auto {
                Ok(None)
            } else {
                Err("MCP_LEGACY_PROBE_ERROR".into())
            };
        }
        let (status, value) = response_value(response, &json!(0), true).await?;
        if !status.is_success() {
            return Err(format!("HTTP_STATUS_{}", status.as_u16()));
        }
        protocol::discovery(&value, &json!(0)).map(Some)
    }
    fn new(config: &Value, material: &crate::mcp_credentials::Material) -> Result<Self, String> {
        if config["auth"]["type"] == "oauth" && material.guard.is_none() {
            return Err("MCP_OAUTH_GRANT_REQUIRED".into());
        }
        let url = integrations::scoped_url(
            config["base_url"].as_str().ok_or("MCP_URL")?,
            config["endpoint"].as_str().unwrap_or("/mcp"),
        )?;
        Ok(Self {
            client: crate::http_client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(20))
                .build()
                .map_err(|_| "MCP_CLIENT")?,
            url,
            auth: material.headers.clone(),
            session: None,
            version: None,
            guard: material.guard.clone(),
        })
    }
    fn request(&self, method: reqwest::Method) -> Result<reqwest::RequestBuilder, String> {
        if let Some(guard) = &self.guard {
            guard.check()?;
        }
        let mut request = self
            .client
            .request(method, self.url.clone())
            .headers(self.auth.clone())
            .header("Accept", "application/json, text/event-stream");
        if let Some(session) = &self.session {
            let mut value = reqwest::header::HeaderValue::from_str(session)
                .map_err(|_| "MCP_SESSION_INVALID")?;
            value.set_sensitive(true);
            request = request.header("Mcp-Session-Id", value);
        }
        if let Some(version) = &self.version {
            request = request.header("MCP-Protocol-Version", version);
        }
        Ok(request)
    }
    async fn post(
        &self,
        value: &Value,
        fields: &[HeaderField],
        modern: bool,
    ) -> Result<reqwest::Response, String> {
        let mut request = self.request(reqwest::Method::POST)?;
        if modern && value["method"] == "subscriptions/listen" {
            request = request.timeout(Duration::from_secs(900));
        }
        if modern {
            request = request.headers(protocol::headers(value, fields)?);
        }
        crate::mcp_request_guard::Guard::run(self.guard.as_ref(), async {
            request
                .json(value)
                .send()
                .await
                .map_err(|_| "MCP_NETWORK".into())
        })
        .await
    }
    async fn initialize(&mut self) -> Result<Value, String> {
        let response = self.post(&initialize(), &[], false).await?;
        if let Some(header) = response.headers().get("mcp-session-id") {
            let session = header.to_str().map_err(|_| "MCP_SESSION_INVALID")?;
            if session.is_empty()
                || session.len() > 1024
                || !session.bytes().all(|b| (33..=126).contains(&b))
            {
                return Err("MCP_SESSION_INVALID".into());
            }
            // Retain immediately, before parsing/version validation, so all
            // subsequent error/cancellation paths can retire this session.
            self.session = Some(session.into());
        }
        let (_, value) = response_value(response, &json!(1), false).await?;
        let result = protocol::complete(&value, &json!(1), false)?.clone();
        self.version = Some(legacy_version(&result)?);
        let response = self
            .post(
                &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                &[],
                false,
            )
            .await?;
        if response.status() != StatusCode::ACCEPTED {
            return Err("MCP_INITIALIZED_REJECTED".into());
        }
        Ok(result)
    }
    async fn close(&mut self) {
        if let Some(session) = self.session.take() {
            cleanup(
                self.client.clone(),
                self.url.clone(),
                self.auth.clone(),
                session,
                self.version.clone(),
                self.guard.clone(),
            )
            .await;
        }
    }
}
async fn cleanup(
    client: reqwest::Client,
    url: reqwest::Url,
    auth: HeaderMap,
    session: String,
    version: Option<String>,
    guard: Option<crate::mcp_request_guard::Guard>,
) {
    let Ok(mut value) = reqwest::header::HeaderValue::from_str(&session) else {
        return;
    };
    value.set_sensitive(true);
    let _ = crate::mcp_request_guard::Guard::run(guard.as_ref(), async {
        client
            .delete(url)
            .headers(auth)
            .header("Mcp-Session-Id", value)
            .header(
                "MCP-Protocol-Version",
                version.as_deref().unwrap_or(protocol::LEGACY[0]),
            )
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map_err(|_| "MCP_CLEANUP_NETWORK".to_string())?;
        Ok(())
    })
    .await;
}
impl Drop for Http {
    fn drop(&mut self) {
        if let (Some(session), Ok(runtime)) =
            (self.session.take(), tokio::runtime::Handle::try_current())
        {
            // Cancellation may drop the main future. Only DELETE is detached;
            // no tool request is ever retried or revived here.
            runtime.spawn(cleanup(
                self.client.clone(),
                self.url.clone(),
                self.auth.clone(),
                session,
                self.version.clone(),
                self.guard.clone(),
            ));
        }
    }
}
enum Transport {
    Stdio(Box<Stdio>),
    Http(Box<Http>),
}
pub(crate) struct Session {
    transport: Transport,
    era: Era,
    id: i64,
    pub info: Value,
}
impl Session {
    #[cfg(test)]
    pub async fn open(config: &Value, token: Option<&str>) -> Result<Self, String> {
        let mut headers = HeaderMap::new();
        if let Some(token) = token {
            let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| "MCP_AUTH_HEADER")?;
            value.set_sensitive(true);
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
        Self::open_private(
            config,
            &crate::mcp_credentials::Material {
                headers,
                env: Default::default(),
                needles: vec![],
                guard: None,
            },
        )
        .await
    }
    pub async fn open_private(
        config: &Value,
        material: &crate::mcp_credentials::Material,
    ) -> Result<Self, String> {
        let mode = mode(config)?;
        let (transport, era, info) = match config["transport"].as_str() {
            Some("stdio") => {
                let mut stdio = Stdio::spawn(config, &material.env)?;
                let probe = if mode == Mode::Legacy {
                    None
                } else {
                    // Auto gets a short compatibility probe before a fresh
                    // legacy process. Explicit modern is an ordinary bounded
                    // RPC, consistent with HTTP and subsequent stdio requests.
                    let deadline = if mode == Mode::Auto {
                        PROBE_TIMEOUT
                    } else {
                        Duration::from_secs(20)
                    };
                    let result = tokio::time::timeout(
                        deadline,
                        stdio.exchange(&protocol::discover(&json!(0))?),
                    )
                    .await;
                    Some(
                        result
                            .map_err(|_| "MCP_DISCOVERY_TIMEOUT".to_string())
                            .and_then(|v| v),
                    )
                };
                let modern = match probe {
                    Some(Ok(value)) => match protocol::discovery(&value, &json!(0)) {
                        Ok(info) => Some(info),
                        Err(e) if e == "MCP_LEGACY_PROBE_ERROR" && mode == Mode::Auto => None,
                        Err(e) => {
                            stdio.close().await;
                            return Err(e);
                        }
                    },
                    Some(Err(e)) if e == "MCP_DISCOVERY_TIMEOUT" && mode == Mode::Auto => {
                        // A late probe reply must not collide with initialize.
                        // Replace the process before any business dispatch.
                        stdio.close().await;
                        stdio = Stdio::spawn(config, &material.env)?;
                        None
                    }
                    Some(Err(e)) => {
                        stdio.close().await;
                        return Err(e);
                    }
                    None => None,
                };
                if let Some(info) = modern {
                    (Transport::Stdio(Box::new(stdio)), Era::Modern, info)
                } else {
                    let info = match stdio.initialize().await {
                        Ok(info) => info,
                        Err(e) => {
                            stdio.close().await;
                            return Err(e);
                        }
                    };
                    let version = legacy_version(&info)?;
                    (
                        Transport::Stdio(Box::new(stdio)),
                        Era::Legacy(version),
                        info,
                    )
                }
            }
            Some("http" | "streamable_http") => {
                let mut http = Http::new(config, material)?;
                let modern = if mode == Mode::Legacy {
                    None
                } else {
                    http.probe(mode).await?
                };
                if let Some(info) = modern {
                    (Transport::Http(Box::new(http)), Era::Modern, info)
                } else {
                    let info = match http.initialize().await {
                        Ok(info) => info,
                        Err(e) => {
                            http.close().await;
                            return Err(e);
                        }
                    };
                    let version = legacy_version(&info)?;
                    (Transport::Http(Box::new(http)), Era::Legacy(version), info)
                }
            }
            _ => return Err("MCP_TRANSPORT_UNSUPPORTED".into()),
        };
        Ok(Self {
            transport,
            era,
            id: 2,
            info,
        })
    }
    pub fn mirrored(&self) -> bool {
        matches!(self.era, Era::Modern) && matches!(self.transport, Transport::Http(_))
    }
    pub fn version(&self) -> &str {
        match &self.era {
            Era::Modern => protocol::MODERN,
            Era::Legacy(v) => v,
        }
    }
    pub async fn rpc(&mut self, request: &Value, fields: &[HeaderField]) -> Result<Value, String> {
        let mut request = request.clone();
        request["id"] = json!(self.id);
        self.id = self.id.checked_add(1).ok_or("MCP_REQUEST_ID_LIMIT")?;
        let modern = matches!(self.era, Era::Modern);
        if modern {
            request = protocol::modern_request(&request)?;
        }
        let value = match &mut self.transport {
            Transport::Stdio(stdio) => stdio.exchange(&request).await?,
            Transport::Http(http) => {
                crate::mcp_request_guard::Guard::run(http.guard.as_ref(), async {
                    let response = http.post(&request, fields, modern).await?;
                    Ok(response_value(response, &request["id"], false).await?.1)
                })
                .await?
            }
        };
        protocol::complete(&value, &request["id"], modern).cloned()
    }
    pub async fn close(&mut self) {
        match &mut self.transport {
            Transport::Stdio(stdio) => stdio.close().await,
            Transport::Http(http) => http.close().await,
        }
    }
}
async fn response_value(
    mut response: reqwest::Response,
    id: &Value,
    probe: bool,
) -> Result<(StatusCode, Value), String> {
    let status = response.status();
    if !status.is_success() && !probe {
        return Err(format!("HTTP_STATUS_{}", status.as_u16()));
    }
    // Discovery errors need their bounded RPC body for era classification.
    if probe
        && !status.is_success()
        && !matches!(status, StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND)
    {
        return Err(format!("HTTP_STATUS_{}", status.as_u16()));
    }
    let media = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .and_then(|t| t.split(';').next())
        .map(str::trim)
        .unwrap_or("")
        .to_ascii_lowercase();
    if media == "application/json" {
        let bytes = integrations::bounded_body(response, LIMIT).await?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| "MCP_JSON")?;
        protocol::reply(&value, id)?;
        return Ok((status, value));
    }
    if media != "text/event-stream" {
        return Err("MCP_RESPONSE_MEDIA".into());
    }
    let mut decoder = super::sse::Decoder::default();
    let mut total = 0usize;
    let mut messages = 0usize;
    while let Some(chunk) = response.chunk().await.map_err(|_| "MCP_STREAM")? {
        total = total.saturating_add(chunk.len());
        if total > LIMIT {
            return Err("HTTP_RESPONSE_LIMIT".into());
        }
        for data in decoder.push(&chunk)? {
            if data.is_empty() {
                continue;
            }
            messages += 1;
            if messages > 256 {
                return Err("MCP_NOTIFICATION_LIMIT".into());
            }
            let value: Value = serde_json::from_str(&data).map_err(|_| "MCP_JSON")?;
            if value.get("id").is_some() {
                protocol::reply(&value, id)?;
                return Ok((status, value));
            }
            notification(&value)?;
        }
    }
    Err("MCP_STREAM_INCOMPLETE".into())
}
#[cfg(test)]
#[path = "mcp_transport_tests.rs"]
mod tests;
