//! Closed, operator-owned MCP profiles. Remote schemas and annotations cannot
//! grant authority or choose a credential destination.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Stdio,
    StreamableHttp,
}
#[derive(Clone, Copy, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Auto,
    Modern,
    Legacy,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Auth {
    None,
    Bearer {
        secret_ref: String,
    },
    #[serde(rename = "oauth")]
    OAuth2 {
        issuer: String,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub value: Option<String>,
    pub secret_ref: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    #[serde(default)]
    pub tools: BTreeMap<String, bool>,
    #[serde(default)]
    pub resources: BTreeSet<String>,
    #[serde(default)]
    pub prompts: BTreeMap<String, bool>,
}
fn yes() -> bool {
    true
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub profile_version: u8,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub transport: Transport,
    #[serde(default)]
    pub protocol: Protocol,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    pub base_url: Option<String>,
    pub endpoint: Option<String>,
    pub auth: Auth,
    #[serde(default)]
    pub headers: BTreeMap<String, Header>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub permissions: Permissions,
}
pub fn name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)
}
pub fn reference(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
fn header(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with("mcp-")
        && ![
            "authorization",
            "host",
            "content-length",
            "transfer-encoding",
            "connection",
            "upgrade",
            "trailer",
            "te",
            "proxy-authorization",
            "proxy-connection",
            "cookie",
            "set-cookie",
            "content-type",
            "accept",
            "accept-encoding",
        ]
        .contains(&s)
}
fn secret_header(s: &str) -> bool {
    [
        "token",
        "secret",
        "password",
        "api-key",
        "apikey",
        "credential",
    ]
    .iter()
    .any(|part| s.contains(part))
}
pub fn required_secrets(c: &Config) -> BTreeSet<String> {
    let mut names: BTreeSet<_> = c.env.values().cloned().collect();
    names.extend(c.headers.values().filter_map(|h| h.secret_ref.clone()));
    if let Auth::Bearer { secret_ref } = &c.auth {
        names.insert(secret_ref.clone());
    }
    names
}
pub fn endpoint(c: &Config) -> Result<reqwest::Url, String> {
    let base = c.base_url.as_deref().ok_or("MCP_PROFILE_BASE_URL")?;
    let url = reqwest::Url::parse(base).map_err(|_| "MCP_PROFILE_BASE_URL")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || url.path() != "/"
        || url.as_str().trim_end_matches('/') != base.trim_end_matches('/')
        || (url.scheme() == "http"
            && !matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost")))
    {
        return Err("MCP_PROFILE_ORIGIN_OR_TLS".into());
    }
    let path = c.endpoint.as_deref().ok_or("MCP_PROFILE_ENDPOINT")?;
    if path.len() > 2048
        || !path.starts_with('/')
        || path.contains(['?', '#', '\\'])
        || path.chars().any(char::is_control)
    {
        return Err("MCP_PROFILE_ENDPOINT".into());
    }
    let target = crate::integrations::scoped_url(base, path)?;
    if target.path() != path {
        return Err("MCP_PROFILE_ENDPOINT_CANONICAL".into());
    }
    Ok(target)
}
pub fn config(raw: &Value) -> Result<Config, String> {
    if raw.to_string().len() > 65536 {
        return Err("MCP_PROFILE_LIMIT".into());
    }
    let c: Config =
        serde_json::from_value(raw.clone()).map_err(|_| "MCP_PROFILE_CLOSED".to_string())?;
    if c.profile_version != 1
        || c.args.len() > 64
        || c.headers.len() > 32
        || c.env.len() > 32
        || c.permissions.tools.len() > 256
        || c.permissions.prompts.len() > 256
        || c.permissions.resources.len() > 256
    {
        return Err("MCP_PROFILE_VERSION_OR_LIMIT".into());
    }
    for key in c
        .permissions
        .tools
        .keys()
        .chain(c.permissions.prompts.keys())
    {
        if !name(key) {
            return Err("MCP_PROFILE_PERMISSION_NAME".into());
        }
    }
    for uri in &c.permissions.resources {
        if uri.is_empty()
            || uri.len() > 8192
            || uri.chars().any(char::is_control)
            || reqwest::Url::parse(uri).is_err()
        {
            return Err("MCP_PROFILE_RESOURCE_URI".into());
        }
    }
    for (key, h) in &c.headers {
        if !header(key)
            || h.value.is_some() == h.secret_ref.is_some()
            || h.value.as_ref().is_some_and(|s| {
                s.len() > 2048
                    || !s.bytes().all(|b| (32..=126).contains(&b))
                    || secret_header(key)
                    || crate::policy::contains_secret(s)
            })
            || h.secret_ref.as_ref().is_some_and(|s| !reference(s))
        {
            return Err("MCP_PROFILE_HEADER".into());
        }
    }
    for (key, value) in &c.env {
        if key.is_empty()
            || key.len() > 64
            || !key
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            || key.as_bytes()[0].is_ascii_digit()
            || !reference(value)
            || [
                "PATH",
                "PATHEXT",
                "SYSTEMROOT",
                "COMSPEC",
                "TEMP",
                "TMP",
                "USERPROFILE",
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "ALL_PROXY",
                "NO_PROXY",
                "NODE_OPTIONS",
                "PYTHONPATH",
                "PYTHONHOME",
            ]
            .contains(&key.as_str())
        {
            return Err("MCP_PROFILE_ENV_REFERENCE".into());
        }
    }
    if let Auth::Bearer { secret_ref } = &c.auth {
        if !reference(secret_ref) {
            return Err("MCP_PROFILE_AUTH_REFERENCE".into());
        }
    }
    if let Auth::OAuth2 { issuer } = &c.auth {
        crate::mcp_oauth::issuer_url(issuer)?;
    }
    match c.transport {
        Transport::Stdio => {
            let command = c.command.as_deref().ok_or("MCP_PROFILE_COMMAND")?;
            if command.is_empty()
                || command.len() > 4096
                || command.chars().any(char::is_control)
                || !std::path::Path::new(command).is_absolute()
                || crate::policy::contains_secret(command)
                || c.args.iter().any(|s| {
                    s.len() > 4096
                        || s.chars().any(char::is_control)
                        || crate::policy::contains_secret(s)
                })
                || c.base_url.is_some()
                || c.endpoint.is_some()
                || !c.headers.is_empty()
                || c.auth != Auth::None
            {
                return Err("MCP_PROFILE_STDIO_FIELDS".into());
            }
        }
        Transport::StreamableHttp => {
            endpoint(&c)?;
            if c.command.is_some() || !c.args.is_empty() || !c.env.is_empty() {
                return Err("MCP_PROFILE_HTTP_FIELDS".into());
            }
        }
    }
    Ok(c)
}
pub fn credential_target(c: &Config) -> Value {
    // Permission/enable changes get a new save generation but cannot broaden
    // the destination authorized by the separate native credential envelope.
    json!({"transport":c.transport,"protocol":c.protocol,"command":c.command,"args":c.args,"base_url":c.base_url,"endpoint":c.endpoint,"auth":c.auth,"headers":c.headers,"env":c.env})
}
pub fn allowed(c: &Config, action: &str, input: &Value) -> Result<(), String> {
    if !c.enabled {
        return Err("MCP_CONNECTION_DISABLED".into());
    }
    let allow = match action {
        "health" | "tools" | "resources" | "prompts" | "oauth_discover" | "oauth_registration"
        | "oauth_start" | "oauth_status" | "oauth_cancel" | "oauth_refresh" | "oauth_recover"
        | "oauth_cleanup" | "oauth_reset" => true,
        "listen" => crate::mcp_subscription_wire::Filter::parse(&input["notifications"], true)?
            .resource_subscriptions
            .iter()
            .all(|uri| c.permissions.resources.contains(uri)),
        "call" => input["name"]
            .as_str()
            .is_some_and(|n| c.permissions.tools.get(n) == Some(&true)),
        "get_prompt" => input["name"]
            .as_str()
            .is_some_and(|n| c.permissions.prompts.get(n) == Some(&true)),
        "read_resource" => input["uri"]
            .as_str()
            .is_some_and(|uri| c.permissions.resources.contains(uri)),
        _ => return Err("MCP_ACTION".into()),
    };
    if !allow {
        return Err("MCP_SUBJECT_NOT_ALLOWED".into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "mcp_profile_tests.rs"]
mod tests;
