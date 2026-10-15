//! Private native consent/code/refresh transport. Entry points still require
//! separately reviewed Core authority and issuer-bound client enrollment.
use super::*;
use base64::{engine::general_purpose, Engine};
use reqwest::{header, Url};
use std::{collections::BTreeMap, fs::File};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const CALLBACK: &str = "/oauth/mcp/callback";
const CALLBACK_LIMIT: usize = 16384;
const TOKEN_LIMIT: usize = 65536;

fn random_text() -> Result<Text> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "MCP_OAUTH_RANDOM")?;
    let value = Text::new(general_purpose::URL_SAFE_NO_PAD.encode(bytes), 43);
    for b in &mut bytes {
        unsafe {
            std::ptr::write_volatile(b, 0);
        }
    }
    compiler_fence(Ordering::SeqCst);
    value
}
fn challenge(verifier: &Text) -> String {
    general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.text().as_bytes()))
}

// Cooperative per-connection ownership lasts across browser/network waits.
// Profile publication uses its separate short-lived save lease. A process
// crash releases this handle, but never makes an uncertain remote POST safe
// to repeat. No credential bytes enter this empty lock file.
#[cfg(windows)]
pub(super) fn ownership(actor: &Actor) -> Result<Vec<File>> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    canonical(&actor.id)?;
    let root = asset_files::directory(&actor.core.root)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .custom_flags(0x00200000)
        .open(actor.core.root.join(format!("mcp-oauth-{}.lock", actor.id)))
        .map_err(|e| {
            if matches!(e.raw_os_error(), Some(32 | 33)) {
                "MCP_OAUTH_BUSY"
            } else {
                "MCP_OAUTH_LEASE"
            }
        })?;
    let metadata = file.metadata().map_err(|_| "MCP_OAUTH_LEASE")?;
    if !metadata.is_file() || metadata.len() != 0 || metadata.file_attributes() & 0x400 != 0 {
        return Err("MCP_OAUTH_LEASE_UNSAFE".into());
    }
    asset_files::identity(&file)?;
    Ok(vec![root, file])
}
#[cfg(not(windows))]
pub(super) fn ownership(_: &Actor) -> Result<Vec<File>> {
    Err("WINDOWS_11_REQUIRED".into())
}

fn client() -> Result<reqwest::Client> {
    crate::http_client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| "MCP_OAUTH_CLIENT".into())
}
fn secret_matches(actor: &Actor, secret: Option<&Text>) -> Result<()> {
    if (actor.authorization.client_auth == ClientAuth::None) != secret.is_none()
        || secret.is_some_and(|s| s.0.len() > 16384)
    {
        return Err("MCP_OAUTH_CLIENT_SECRET_BINDING".into());
    }
    Ok(())
}

pub(crate) struct Consent {
    actor: Actor,
    state: State,
    listener: TcpListener,
    redirect: String,
    browser_url: Url,
    verifier: Text,
    nonce: Text,
    issuer_required: bool,
    client_secret: Option<Text>,
    registration: Option<enrollment::Registration>,
    registration_access_token: Option<Text>,
    _ownership: Vec<File>,
}
impl Consent {
    #[cfg(test)]
    pub(crate) async fn bind(
        actor: Actor,
        consent_endpoint: &str,
        issuer_required: bool,
        client_secret: Option<Text>,
    ) -> Result<Self> {
        Self::bind_impl(
            actor,
            consent_endpoint,
            issuer_required,
            client_secret,
            None,
        )
        .await
    }
    pub(crate) async fn bind_registered(
        actor: Actor,
        consent_endpoint: &str,
        issuer_required: bool,
        plan: enrollment::Plan,
        supplied_secret: Option<Text>,
    ) -> Result<Self> {
        Self::bind_impl(
            actor,
            consent_endpoint,
            issuer_required,
            supplied_secret,
            Some(plan),
        )
        .await
    }
    async fn bind_impl(
        mut actor: Actor,
        consent_endpoint: &str,
        issuer_required: bool,
        mut client_secret: Option<Text>,
        plan: Option<enrollment::Plan>,
    ) -> Result<Self> {
        actor.check()?;
        #[cfg(test)]
        let local = actor.local;
        #[cfg(not(test))]
        let local = false;
        flow_endpoint(consent_endpoint, local)?;
        if actor
            .authorization
            .requested_scopes
            .as_ref()
            .is_some_and(BTreeSet::is_empty)
        {
            return Err("MCP_OAUTH_EMPTY_SCOPE_REVIEW_REQUIRED".into());
        }
        if plan.is_none() {
            secret_matches(&actor, client_secret.as_ref())?;
        }
        let ownership = ownership(&actor)?;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| "MCP_OAUTH_CALLBACK_BIND")?;
        let port = listener
            .local_addr()
            .map_err(|_| "MCP_OAUTH_CALLBACK_BIND")?
            .port();
        let redirect = format!("http://127.0.0.1:{port}{CALLBACK}");
        let nonce = random_text()?;
        let verifier = random_text()?;
        let mut browser_url = Url::parse(consent_endpoint).map_err(|_| "MCP_OAUTH_ENDPOINT")?;
        if browser_url.query_pairs().any(|(key, _)| {
            [
                "response_type",
                "client_id",
                "redirect_uri",
                "resource",
                "state",
                "code_challenge",
                "code_challenge_method",
                "scope",
            ]
            .contains(&key.as_ref())
        }) {
            return Err("MCP_OAUTH_CONSENT_RESERVED_FIELDS".into());
        }
        let mut registration = None;
        let mut registration_access_token = None;
        if let Some(plan) = plan {
            let (actual, registered) = plan.acquire(actor, &redirect, client_secret).await?;
            actor = actual;
            client_secret = registered.secret;
            registration = Some(registered.info);
            registration_access_token = registered.management_token;
        }
        {
            let mut pairs = browser_url.query_pairs_mut();
            pairs
                .append_pair("response_type", "code")
                .append_pair("client_id", &actor.authorization.client_id)
                .append_pair("redirect_uri", &redirect)
                .append_pair("resource", &actor.authorization.resource)
                .append_pair("state", nonce.text())
                .append_pair("code_challenge", &challenge(&verifier))
                .append_pair("code_challenge_method", "S256");
            if let Some(scopes) = &actor.authorization.requested_scopes {
                pairs.append_pair(
                    "scope",
                    &scopes.iter().cloned().collect::<Vec<_>>().join(" "),
                );
            }
        }
        let state = actor.begin(false)?;
        Ok(Self {
            actor,
            state,
            listener,
            redirect,
            browser_url,
            verifier,
            nonce,
            issuer_required,
            client_secret,
            registration,
            registration_access_token,
            _ownership: ownership,
        })
    }
    // This URL contains a private per-attempt nonce. Only the system-browser
    // launch path may consume it; it is never returned to generic tools/UI.
    pub(crate) fn browser_url(&self) -> &str {
        self.browser_url.as_str()
    }
    pub(crate) fn public_state(&self) -> &State {
        &self.state
    }
    pub(crate) async fn complete(self) -> Result<State> {
        let result = self
            .actor
            .guard(&self.state.session_id, async {
                tokio::time::timeout(Duration::from_secs(180), self.receive())
                    .await
                    .map_err(|_| "MCP_OAUTH_CALLBACK_TIMEOUT")?
            })
            .await;
        if result.is_err() {
            let _ = self.actor.abandon(&self.state.session_id);
        }
        result
    }
    async fn receive(&self) -> Result<State> {
        let (mut socket, peer) = self
            .listener
            .accept()
            .await
            .map_err(|_| "MCP_OAUTH_CALLBACK_ACCEPT")?;
        if !peer.ip().is_loopback() {
            return Err("MCP_OAUTH_CALLBACK_PEER".into());
        }
        let parsed = tokio::time::timeout(Duration::from_secs(5), async {
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let n = socket
                    .read(&mut chunk)
                    .await
                    .map_err(|_| "MCP_OAUTH_CALLBACK_READ")?;
                if n == 0 || bytes.len() + n > CALLBACK_LIMIT {
                    return Err("MCP_OAUTH_CALLBACK_FORMAT".into());
                }
                bytes.extend_from_slice(&chunk[..n]);
                if bytes.windows(4).any(|b| b == b"\r\n\r\n") {
                    break;
                }
            }
            parse_callback(
                &bytes,
                self.listener
                    .local_addr()
                    .map_err(|_| "MCP_OAUTH_CALLBACK_BIND")?
                    .port(),
                self.nonce.text(),
                &self.actor.authorization.issuer,
                self.issuer_required,
            )
        })
        .await
        .map_err(|_| "MCP_OAUTH_CALLBACK_TIMEOUT")?;
        // Static reply only; no callback values, token or remote error is HTML.
        let response = if parsed.is_ok() {
            "HTTP/1.1 200 OK"
        } else {
            "HTTP/1.1 400 Bad Request"
        };
        let reply = format!("{response}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: 26\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\nReturn to AlfredPet to see.");
        let _ =
            tokio::time::timeout(Duration::from_secs(1), socket.write_all(reply.as_bytes())).await;
        drop(socket);
        let code = parsed?;
        let state = self.actor.transition(
            &self.state.session_id,
            Phase::Authorizing,
            Phase::ExchangeInFlight,
        )?;
        let fields = [
            ("grant_type", "authorization_code"),
            ("code", code.text()),
            ("redirect_uri", self.redirect.as_str()),
            ("code_verifier", self.verifier.text()),
        ];
        let body = token_request(&self.actor, &state, &fields, self.client_secret.as_ref()).await?;
        // Transfer the secret into a validated envelope without a public result.
        let secret = self
            .client_secret
            .as_ref()
            .map(|s| Text::new(s.text().to_string(), 16384))
            .transpose()?;
        let envelope = if let Some(info) = &self.registration {
            let management_token = self
                .registration_access_token
                .as_ref()
                .map(|s| Text::new(s.text().to_string(), 16384))
                .transpose()?;
            self.actor.tokens_registered(
                &state,
                body,
                enrollment::Registered {
                    info: info.clone(),
                    secret,
                    management_token,
                },
            )?
        } else {
            self.actor.tokens(&state, body, secret, None)?
        };
        self.actor.transition(
            &state.session_id,
            Phase::ExchangeInFlight,
            Phase::PublicationPending,
        )?;
        self.actor.publish(&state.session_id, envelope)
    }
}

// Explicit renewal, never an automatic reaction to 401 or an unsafe replay.
#[cfg(test)]
pub(crate) async fn refresh(actor: Actor) -> Result<State> {
    refresh_checked(actor, None).await
}
pub(crate) async fn refresh_checked(actor: Actor, expected: Option<&State>) -> Result<State> {
    actor.check()?;
    let _ownership = ownership(&actor)?;
    let (state, previous) = actor.begin_refresh_checked(expected)?;
    let result = async {
        let fields = [
            ("grant_type", "refresh_token"),
            (
                "refresh_token",
                previous
                    .refresh_token
                    .as_ref()
                    .ok_or("MCP_OAUTH_REAUTHORIZE_REQUIRED")?
                    .text(),
            ),
        ];
        let body = token_request(&actor, &state, &fields, previous.client_secret.as_ref()).await?;
        let envelope = actor.tokens(&state, body, None, Some(previous))?;
        actor.transition(
            &state.session_id,
            Phase::RefreshInFlight,
            Phase::PublicationPending,
        )?;
        actor.publish(&state.session_id, envelope)
    }
    .await;
    if result.is_err() {
        let _ = actor.abandon(&state.session_id);
    }
    result
}

fn form_component(text: &str) -> String {
    Url::parse("http://127.0.0.1")
        .expect("literal URL")
        .query_pairs_mut()
        .append_pair("v", text)
        .finish()
        .query()
        .expect("literal query")[2..]
        .to_string()
}
fn token_builder(
    actor: &Actor,
    fields: &[(&str, &str)],
    secret: Option<&Text>,
) -> Result<reqwest::RequestBuilder> {
    secret_matches(actor, secret)?;
    let supplied = fields.iter().map(|(k, _)| *k).collect::<BTreeSet<_>>();
    let names = match fields
        .iter()
        .find(|(k, _)| *k == "grant_type")
        .map(|(_, v)| *v)
    {
        Some("authorization_code") => {
            BTreeSet::from(["grant_type", "code", "redirect_uri", "code_verifier"])
        }
        Some("refresh_token") => BTreeSet::from(["grant_type", "refresh_token"]),
        _ => return Err("MCP_OAUTH_TOKEN_REQUEST".into()),
    };
    if supplied.len() != fields.len()
        || supplied != names
        || fields.iter().any(|(_, v)| v.is_empty())
    {
        return Err("MCP_OAUTH_TOKEN_REQUEST".into());
    }
    let mut fields = fields.to_vec();
    fields.push(("resource", &actor.authorization.resource));
    if actor.authorization.client_auth != ClientAuth::ClientSecretBasic {
        fields.push(("client_id", &actor.authorization.client_id));
    }
    if actor.authorization.client_auth == ClientAuth::ClientSecretPost {
        fields.push((
            "client_secret",
            secret.ok_or("MCP_OAUTH_CLIENT_SECRET_BINDING")?.text(),
        ));
    }
    let mut request = client()?
        .post(&actor.authorization.token_endpoint)
        .header(header::ACCEPT, "application/json")
        .form(&fields);
    if actor.authorization.client_auth == ClientAuth::ClientSecretBasic {
        let basic = general_purpose::STANDARD.encode(format!(
            "{}:{}",
            form_component(&actor.authorization.client_id),
            form_component(secret.ok_or("MCP_OAUTH_CLIENT_SECRET_BINDING")?.text())
        ));
        let mut header = header::HeaderValue::from_str(&format!("Basic {basic}"))
            .map_err(|_| "MCP_OAUTH_CLIENT_AUTH")?;
        header.set_sensitive(true);
        request = request.header(header::AUTHORIZATION, header);
    }
    Ok(request)
}
async fn token_request(
    actor: &Actor,
    state: &State,
    fields: &[(&str, &str)],
    secret: Option<&Text>,
) -> Result<Value> {
    if !matches!(
        state.phase,
        Phase::ExchangeInFlight | Phase::RefreshInFlight
    ) {
        return Err("MCP_OAUTH_PHASE".into());
    }
    let request = token_builder(actor, fields, secret)?;
    actor
        .guard_phase(&state.session_id, state.phase.clone(), async {
            let mut response = request
                .send()
                .await
                .map_err(|_| "MCP_OAUTH_TOKEN_OUTCOME_UNCERTAIN")?;
            let status = response.status().as_u16();
            if response.headers().contains_key("dpop-nonce") {
                return Err("MCP_OAUTH_DPOP_UNSUPPORTED".into());
            }
            if status != 200 {
                // No body/error_description/error_uri is returned or logged.
                return Err(match status {
                    400 | 401 => "MCP_OAUTH_TOKEN_REJECTED",
                    429 => "MCP_OAUTH_TOKEN_RATE_LIMITED",
                    _ => "MCP_OAUTH_TOKEN_OUTCOME_UNCERTAIN",
                }
                .into());
            }
            let content = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|s| s.to_str().ok())
                .ok_or("MCP_OAUTH_TOKEN_CONTENT_TYPE")?;
            let mut parts = content.split(';');
            if !parts
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")
                || parts.any(|p| !p.trim().eq_ignore_ascii_case("charset=utf-8"))
            {
                return Err("MCP_OAUTH_TOKEN_CONTENT_TYPE".into());
            }
            if response
                .content_length()
                .is_some_and(|n| n > TOKEN_LIMIT as u64)
            {
                return Err("MCP_OAUTH_TOKEN_LIMIT".into());
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| "MCP_OAUTH_TOKEN_OUTCOME_UNCERTAIN")?
            {
                if bytes.len() + chunk.len() > TOKEN_LIMIT {
                    return Err("MCP_OAUTH_TOKEN_LIMIT".into());
                }
                bytes.extend_from_slice(&chunk);
            }
            // Clear the owned raw body even on malformed JSON. Parsed strings are
            // moved to Text by tokens(); no claim about all allocator/HTTP copies.
            let value =
                crate::mcp_oauth::strict_json(&bytes).map_err(|_| "MCP_OAUTH_TOKEN_JSON".into());
            for b in &mut bytes {
                unsafe {
                    std::ptr::write_volatile(b, 0);
                }
            }
            compiler_fence(Ordering::SeqCst);
            value
        })
        .await
}

fn decode_form(text: &str) -> Result<String> {
    let input = text.as_bytes();
    let mut output = Vec::new();
    let mut i = 0;
    while i < input.len() {
        match input[i] {
            b'+' => output.push(b' '),
            b'%' => {
                let bytes = input
                    .get(i + 1..i + 3)
                    .ok_or("MCP_OAUTH_CALLBACK_ENCODING")?;
                let hex = |b: u8| -> Option<u8> { (b as char).to_digit(16).map(|x| x as u8) };
                output.push(
                    hex(bytes[0]).ok_or("MCP_OAUTH_CALLBACK_ENCODING")? * 16
                        + hex(bytes[1]).ok_or("MCP_OAUTH_CALLBACK_ENCODING")?,
                );
                i += 2;
            }
            b => output.push(b),
        }
        i += 1;
    }
    let value = String::from_utf8(output).map_err(|_| "MCP_OAUTH_CALLBACK_ENCODING")?;
    if value.chars().any(char::is_control) {
        return Err("MCP_OAUTH_CALLBACK_ENCODING".into());
    }
    Ok(value)
}
fn parse_callback(
    bytes: &[u8],
    port: u16,
    nonce: &str,
    issuer: &str,
    issuer_required: bool,
) -> Result<Text> {
    if bytes.len() > CALLBACK_LIMIT
        || !bytes.ends_with(b"\r\n\r\n")
        || !bytes.is_ascii()
        || bytes.contains(&0)
    {
        return Err("MCP_OAUTH_CALLBACK_FORMAT".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "MCP_OAUTH_CALLBACK_FORMAT")?;
    let mut lines = text[..text.len() - 4].split("\r\n");
    let fields = lines
        .next()
        .ok_or("MCP_OAUTH_CALLBACK_FORMAT")?
        .split(' ')
        .collect::<Vec<_>>();
    if fields.len() != 3
        || fields[0] != "GET"
        || fields[2] != "HTTP/1.1"
        || !fields[1].starts_with(&format!("{CALLBACK}?"))
        || fields[1].contains(['#', '\\'])
        || fields[1].bytes().any(|b| !(33..=126).contains(&b))
    {
        return Err("MCP_OAUTH_CALLBACK_ROUTE".into());
    }
    let mut host = false;
    let mut length = false;
    for line in lines {
        let (key, value) = line.split_once(':').ok_or("MCP_OAUTH_CALLBACK_HEADERS")?;
        if key.is_empty()
            || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || value.bytes().any(|b| b.is_ascii_control() && b != b'\t')
        {
            return Err("MCP_OAUTH_CALLBACK_HEADERS".into());
        }
        if key.eq_ignore_ascii_case("host") {
            if host || value.trim() != format!("127.0.0.1:{port}") {
                return Err("MCP_OAUTH_CALLBACK_HOST".into());
            }
            host = true;
        }
        if key.eq_ignore_ascii_case("content-length") {
            if length || value.trim() != "0" {
                return Err("MCP_OAUTH_CALLBACK_BODY_DENIED".into());
            }
            length = true;
        }
        if key.eq_ignore_ascii_case("transfer-encoding") {
            return Err("MCP_OAUTH_CALLBACK_BODY_DENIED".into());
        }
    }
    if !host {
        return Err("MCP_OAUTH_CALLBACK_HOST".into());
    }
    let mut pairs = BTreeMap::new();
    for part in fields[1][CALLBACK.len() + 1..].split('&') {
        let (key, value) = part.split_once('=').ok_or("MCP_OAUTH_CALLBACK_FORMAT")?;
        let key = decode_form(key)?;
        if key.is_empty() || pairs.insert(key, decode_form(value)?).is_some() {
            return Err("MCP_OAUTH_CALLBACK_DUPLICATE".into());
        }
    }
    if pairs.get("state").map(String::as_str) != Some(nonce) {
        return Err("MCP_OAUTH_CALLBACK_STATE".into());
    }
    // Exact decoded string comparison, including error callbacks. Never act
    // on/display remote error information before authenticating its issuer.
    match pairs.get("iss") {
        Some(received) if received != issuer => return Err("MCP_OAUTH_CALLBACK_ISSUER".into()),
        None if issuer_required => return Err("MCP_OAUTH_CALLBACK_ISSUER_REQUIRED".into()),
        _ => {}
    }
    if pairs.contains_key("error") {
        if pairs.contains_key("code") {
            return Err("MCP_OAUTH_CALLBACK_CONTRADICTORY".into());
        }
        return Err("MCP_OAUTH_CONSENT_REJECTED".into());
    }
    Text::new(pairs.remove("code").ok_or("MCP_OAUTH_CALLBACK_CODE")?, 4096)
        .map_err(|_| "MCP_OAUTH_CALLBACK_CODE".into())
}

#[cfg(test)]
#[path = "mcp_auth_flow_tests.rs"]
mod tests;
