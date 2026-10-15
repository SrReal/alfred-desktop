//! Named MCP secrets have one owned native target and a closed private
//! envelope. This codec does not publish ready or perform native/SQLite I/O.
use crate::mcp_profile::{self, Auth, Config};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const KIND: &str = "alfredpet.mcp.secrets";
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Unconfigured,
    Pending,
    Ready,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub version: u8,
    pub connection: String,
    pub target_sha256: String,
    pub generation: Option<String>,
    pub stage: Stage,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u8,
    kind: String,
    connection: String,
    target_sha256: String,
    generation: String,
    secrets: BTreeMap<String, String>,
}
// Deliberately no Debug/Serialize: native bytes must never become a public
// tool result, audit record, or preview through a generic serializer.
pub struct Publication {
    pub pending: Binding,
    pub ready: Binding,
    pub private_json: String,
}
pub fn canonical(id: &str) -> Result<(), String> {
    if uuid::Uuid::parse_str(id)
        .map_err(|_| "MCP_CONNECTION_ID")?
        .to_string()
        != id
    {
        return Err("MCP_CONNECTION_ID".into());
    }
    Ok(())
}
pub fn alias(id: &str) -> Result<String, String> {
    canonical(id)?;
    Ok(format!("mcp.credentials.{id}"))
}
fn target(id: &str, c: &Config) -> Result<String, String> {
    canonical(id)?;
    mcp_profile::config(&json!(c))?;
    Ok(format!(
        "{:x}",
        Sha256::digest(
            json!({"connection":id,"alias":alias(id)?,"target":mcp_profile::credential_target(c)})
                .to_string()
                .as_bytes()
        )
    ))
}
pub fn unconfigured(id: &str, c: &Config) -> Result<Binding, String> {
    Ok(Binding {
        version: 1,
        connection: id.into(),
        target_sha256: target(id, c)?,
        generation: None,
        stage: Stage::Unconfigured,
    })
}
pub fn validate_binding(id: &str, c: &Config, b: &Binding) -> Result<(), String> {
    if b.version != 1
        || b.connection != id
        || b.target_sha256 != target(id, c)?
        || (b.stage == Stage::Unconfigured) != b.generation.is_none()
        || b.generation.as_ref().is_some_and(|g| canonical(g).is_err())
    {
        return Err("MCP_CREDENTIAL_BINDING_INVALID".into());
    }
    Ok(())
}
fn secrets(c: &Config, v: &str) -> Result<BTreeMap<String, String>, String> {
    if v.len() > 8192 {
        return Err("MCP_SECRET_MAP_LIMIT".into());
    }
    let values: BTreeMap<String, String> =
        serde_json::from_str(v).map_err(|_| "MCP_SECRET_MAP_JSON_REQUIRED")?;
    let refs = mcp_profile::required_secrets(c);
    if values
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        != refs
    {
        return Err("MCP_SECRET_REFERENCES_EXACT_MATCH_REQUIRED".into());
    }
    for (name, value) in &values {
        let header = c
            .headers
            .values()
            .any(|h| h.secret_ref.as_ref() == Some(name));
        let bearer = matches!(&c.auth,Auth::Bearer{secret_ref} if secret_ref==name);
        if value.is_empty()
            || value.len() > 2048
            || value.contains('\0')
            || ((header || bearer) && !value.bytes().all(|b| (32..=126).contains(&b)))
            || (bearer && !value.bytes().all(|b| (33..=126).contains(&b)))
        {
            return Err("MCP_SECRET_VALUE_INVALID".into());
        }
    }

    Ok(values)
}
pub fn publication(id: &str, c: &Config, secret_json: &str) -> Result<Publication, String> {
    let values = secrets(c, secret_json)?;
    if values.is_empty() {
        return Err("MCP_SECRET_NOT_REQUIRED".into());
    }
    let generation = uuid::Uuid::new_v4().to_string();
    let target = target(id, c)?;
    let envelope = Envelope {
        version: 1,
        kind: KIND.into(),
        connection: id.into(),
        target_sha256: target.clone(),
        generation: generation.clone(),
        secrets: values,
    };
    let private_json = serde_json::to_string(&envelope).map_err(|_| "MCP_PRIVATE_ENVELOPE")?;
    if private_json.encode_utf16().count() > 1280 {
        return Err("MCP_NATIVE_CREDENTIAL_CAPACITY".into());
    }
    let pending = Binding {
        version: 1,
        connection: id.into(),
        target_sha256: target,
        generation: Some(generation),
        stage: Stage::Pending,
    };
    let mut ready = pending.clone();
    ready.stage = Stage::Ready;
    Ok(Publication {
        pending,
        ready,
        private_json,
    })
}
pub fn read(
    id: &str,
    c: &Config,
    b: &Binding,
    native: Option<&str>,
) -> Result<BTreeMap<String, String>, String> {
    validate_binding(id, c, b)?;
    let refs = mcp_profile::required_secrets(c);
    if b.stage == Stage::Unconfigured && refs.is_empty() {
        return Ok(BTreeMap::new());
    }
    if b.stage != Stage::Ready {
        return Err("MCP_CREDENTIAL_REENTRY_REQUIRED".into());
    }
    let raw = native.ok_or("MCP_CREDENTIAL_UNAVAILABLE")?;
    if raw.encode_utf16().count() > 1280 {
        return Err("MCP_NATIVE_CREDENTIAL_CAPACITY".into());
    }
    let e: Envelope =
        serde_json::from_str(raw).map_err(|_| "MCP_PRIVATE_ENVELOPE_REENTRY_REQUIRED")?;
    if e.version != 1
        || e.kind != KIND
        || e.connection != id
        || e.target_sha256 != b.target_sha256
        || b.generation.as_ref() != Some(&e.generation)
        || canonical(&e.generation).is_err()
    {
        return Err("MCP_PRIVATE_ENVELOPE_BINDING_INVALID".into());
    }
    secrets(c, &json!(e.secrets).to_string())
}
pub fn public_binding(v: &Value) -> Result<Binding, String> {
    serde_json::from_value(v.clone()).map_err(|_| "MCP_CREDENTIAL_BINDING_REQUIRED".into())
}

// No Debug/Serialize: resolved auth is ephemeral and must not enter previews.
pub struct Material {
    pub headers: reqwest::header::HeaderMap,
    pub env: BTreeMap<String, String>,
    pub needles: Vec<String>,
    pub(crate) guard: Option<crate::mcp_request_guard::Guard>,
}
pub fn material(c: &Config, values: &BTreeMap<String, String>) -> Result<Material, String> {
    mcp_profile::config(&json!(c))?;
    secrets(c, &json!(values).to_string())?;
    if values
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        != mcp_profile::required_secrets(c)
    {
        return Err("MCP_SECRET_REFERENCES_EXACT_MATCH_REQUIRED".into());
    }
    let mut headers = reqwest::header::HeaderMap::new();
    for (key, h) in &c.headers {
        let text = match (&h.value, &h.secret_ref) {
            (Some(text), None) => text,
            (None, Some(name)) => values.get(name).ok_or("MCP_SECRET_REFERENCE_MISSING")?,
            _ => return Err("MCP_PROFILE_HEADER".into()),
        };
        let mut value =
            reqwest::header::HeaderValue::from_str(text).map_err(|_| "MCP_AUTH_HEADER")?;
        value.set_sensitive(true);
        let name = reqwest::header::HeaderName::from_bytes(key.as_bytes())
            .map_err(|_| "MCP_AUTH_HEADER")?;
        headers.insert(name, value);
    }
    if let Auth::Bearer { secret_ref } = &c.auth {
        let secret = values
            .get(secret_ref)
            .ok_or("MCP_SECRET_REFERENCE_MISSING")?;
        let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {secret}"))
            .map_err(|_| "MCP_AUTH_HEADER")?;
        value.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, value);
    }
    let mut env = BTreeMap::new();
    for (key, name) in &c.env {
        env.insert(
            key.clone(),
            values
                .get(name)
                .ok_or("MCP_SECRET_REFERENCE_MISSING")?
                .clone(),
        );
    }
    Ok(Material {
        headers,
        env,
        needles: values.values().cloned().collect(),
        guard: None,
    })
}
#[cfg(test)]
#[path = "mcp_credentials_tests.rs"]
mod tests;
