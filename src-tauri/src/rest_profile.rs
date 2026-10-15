//! Closed manual REST profiles and pure request planning. The prepared Core
//! transport will consume these plans; this module performs no credential I/O.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Free,
    Contextual,
    Mandatory,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Auth {
    None,
    Bearer {
        secret_ref: String,
    },
    Basic {
        username: String,
        secret_ref: String,
    },
    Header {
        header: String,
        secret_ref: String,
    },
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub value: Option<String>,
    pub secret_ref: Option<String>,
}
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResponseKind {
    Json,
    Text,
    Empty,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseSpec {
    pub response: ResponseKind,
    pub schema: Option<Value>,
    pub content_type: Option<String>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub method: String,
    pub path: String,
    pub path_schema: Value,
    pub query_schema: Value,
    pub body_schema: Option<Value>,
    #[serde(default = "yes")]
    pub body_required: bool,
    pub request_content_type: Option<String>,
    pub response_schema: Option<Value>,
    pub response: ResponseKind,
    pub success_statuses: Vec<u16>,
    #[serde(default)]
    pub responses: BTreeMap<u16, ResponseSpec>,
    pub risk: Risk,
}
fn yes() -> bool {
    true
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub profile_version: u8,
    pub base_url: String,
    pub allowed_hosts: Vec<String>,
    #[serde(default = "yes")]
    pub read_only: bool,
    pub auth: Auth,
    pub headers: BTreeMap<String, Header>,
    pub operations: BTreeMap<String, Operation>,
    pub openapi_path: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inputs {
    pub operation: String,
    #[serde(default = "empty_object")]
    pub path: Value,
    #[serde(default = "empty_object")]
    pub query: Value,
    #[serde(default, deserialize_with = "present_body")]
    pub body: Option<Value>,
}
fn present_body<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}
fn empty_object() -> Value {
    json!({})
}
#[derive(Debug)]
pub struct Plan {
    pub operation: String,
    pub method: String,
    pub url: reqwest::Url,
    pub body: Option<Value>,
    pub risk: Risk,
    pub mutation: bool,
}
fn atom(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn header_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && ![
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
            "accept-encoding",
        ]
        .contains(&s)
}
fn sensitive(s: &str) -> bool {
    [
        "authorization",
        "x-api-key",
        "api-key",
        "token",
        "access_token",
        "password",
        "secret",
    ]
    .contains(&s)
        || s.contains("token")
        || s.contains("password")
        || s.contains("secret")
        || s.contains("api-key")
}
fn header_value(s: &str) -> bool {
    !s.is_empty() && s.len() <= 2048 && s.bytes().all(|b| (32..=126).contains(&b))
}
pub fn json_media(s: &str) -> bool {
    (s == "application/json" || (s.starts_with("application/") && s.ends_with("+json")))
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"/.+-".contains(&b))
}
pub fn response_spec(op: &Operation, status: u16) -> Result<ResponseSpec, String> {
    if !op.success_statuses.contains(&status) {
        return Err("REST_STATUS_UNEXPECTED".into());
    }
    Ok(op
        .responses
        .get(&status)
        .cloned()
        .unwrap_or_else(|| ResponseSpec {
            response: op.response,
            schema: op.response_schema.clone(),
            content_type: None,
        }))
}
pub fn base(c: &Config) -> Result<reqwest::Url, String> {
    let u = reqwest::Url::parse(&c.base_url).map_err(|_| "REST_BASE_URL")?;
    let host = u.host_str().ok_or("REST_HOST")?;
    if !matches!(u.scheme(), "http" | "https")
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
        || !u.path().ends_with('/')
        || (u.scheme() == "http" && !matches!(host, "localhost" | "127.0.0.1" | "[::1]"))
        || c.base_url != u.as_str()
        || !c.allowed_hosts.iter().any(|v| v == host)
    {
        return Err("REST_ORIGIN_OR_TLS_POLICY".into());
    }
    for segment in u.path().split('/').filter(|s| !s.is_empty()) {
        if segment == "."
            || segment == ".."
            || !segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._~-".contains(&b))
        {
            return Err("REST_BASE_PATH".into());
        }
    }
    Ok(u)
}
fn schema_safe(v: &Value, depth: usize, nodes: &mut usize) -> bool {
    *nodes += 1;
    if depth > 24 || *nodes > 4096 {
        return false;
    }
    match v {
        Value::Object(m) => m
            .iter()
            .all(|(k, v)| !k.starts_with('$') && schema_safe(v, depth + 1, nodes)),
        Value::Array(a) => a.iter().all(|v| schema_safe(v, depth + 1, nodes)),
        _ => true,
    }
}
pub fn schema(v: &Value) -> Result<(), String> {
    validator(v).map(|_| ())
}
pub fn validator(v: &Value) -> Result<jsonschema::Validator, String> {
    if v.to_string().len() > 65536 || !schema_safe(v, 0, &mut 0) {
        return Err("REST_SCHEMA_UNSUPPORTED_OR_LIMIT".into());
    }
    jsonschema::options()
        .should_validate_formats(true)
        .should_ignore_unknown_formats(false)
        .build(v)
        .map_err(|_| "REST_SCHEMA_UNSUPPORTED_OR_LIMIT".into())
}
fn parameters(v: &Value) -> Result<BTreeSet<String>, String> {
    schema(v)?;
    if v["type"] != "object" || v["additionalProperties"] != false {
        return Err("REST_PARAMETER_SCHEMA_CLOSED_OBJECT".into());
    }
    let properties = v["properties"]
        .as_object()
        .ok_or("REST_PARAMETER_PROPERTIES")?;
    if properties.len() > 64 {
        return Err("REST_PARAMETER_LIMIT".into());
    }
    for (k, p) in properties {
        if !atom(k)
            || sensitive(&k.to_ascii_lowercase())
            || !matches!(
                p["type"].as_str(),
                Some("string" | "integer" | "number" | "boolean")
            )
        {
            return Err("REST_PARAMETER_PRIMITIVE_REQUIRED".into());
        }
    }
    if let Some(a) = v.get("required") {
        let a = a.as_array().ok_or("REST_PARAMETER_REQUIRED")?;
        let mut names = BTreeSet::new();
        for n in a {
            let n = n.as_str().ok_or("REST_PARAMETER_REQUIRED")?;
            if !properties.contains_key(n) || !names.insert(n) {
                return Err("REST_PARAMETER_REQUIRED".into());
            }
        }
    }
    Ok(properties.keys().cloned().collect())
}
fn segments(path: &str) -> Result<(Vec<&str>, BTreeSet<String>), String> {
    if path.len() > 2048
        || !path.starts_with('/')
        || path.starts_with("//")
        || path.contains(['?', '#', '\\', '%'])
    {
        return Err("REST_OPERATION_PATH".into());
    }
    let parts: Vec<_> = path[1..].split('/').collect();
    let mut names = BTreeSet::new();
    for (index, s) in parts.iter().enumerate() {
        if s.starts_with('{') && s.ends_with('}') {
            let name = &s[1..s.len() - 1];
            if !atom(name) || !names.insert(name.to_string()) {
                return Err("REST_PATH_PARAMETER".into());
            }
        } else if (s.is_empty() && index + 1 != parts.len())
            || *s == "."
            || *s == ".."
            || !s
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._~-".contains(&b))
        {
            return Err("REST_OPERATION_PATH".into());
        }
    }
    Ok((parts, names))
}
pub fn required_secrets(c: &Config) -> Result<BTreeSet<String>, String> {
    let mut refs = BTreeSet::new();
    let mut names = BTreeSet::new();
    let (auth_header, auth_ref) = match &c.auth {
        Auth::None => (None, None),
        Auth::Bearer { secret_ref } => (Some("authorization"), Some(secret_ref)),
        Auth::Basic {
            username,
            secret_ref,
        } => {
            if !header_value(username) || username.contains(':') {
                return Err("REST_AUTH_USERNAME".into());
            }
            (Some("authorization"), Some(secret_ref))
        }
        Auth::Header { header, secret_ref } => {
            if !header_name(header) {
                return Err("REST_AUTH_HEADER".into());
            }
            (Some(header.as_str()), Some(secret_ref))
        }
    };
    if let Some(h) = auth_header {
        names.insert(h);
    }
    if let Some(s) = auth_ref {
        if !atom(s) {
            return Err("REST_SECRET_REFERENCE".into());
        }
        refs.insert(s.clone());
    }
    if c.headers.len() > 32 {
        return Err("REST_HEADERS_LIMIT".into());
    }
    for (name, h) in &c.headers {
        if !header_name(name) || !names.insert(name.as_str()) {
            return Err("REST_HEADER_NAME_OR_CONFLICT".into());
        }
        match (&h.value, &h.secret_ref) {
            (Some(v), None) if header_value(v) && !sensitive(name) => {}
            (None, Some(s)) if atom(s) => {
                refs.insert(s.clone());
            }
            _ => return Err("REST_HEADER_VALUE_OR_SECRET_REFERENCE".into()),
        }
    }
    if refs.len() > 16 {
        return Err("REST_SECRET_REFERENCES_LIMIT".into());
    }
    Ok(refs)
}
pub fn config(v: &Value) -> Result<Config, String> {
    if v.to_string().len() > 512 * 1024 {
        return Err("REST_CONFIG_LIMIT".into());
    }
    let c: Config = serde_json::from_value(v.clone()).map_err(|_| "REST_CONFIG_INVALID")?;
    if c.profile_version != 1
        || c.allowed_hosts.is_empty()
        || c.allowed_hosts.len() > 16
        || c.allowed_hosts.iter().collect::<BTreeSet<_>>().len() != c.allowed_hosts.len()
        || c.allowed_hosts.iter().any(|h| {
            h.len() > 253
                || h.to_ascii_lowercase() != *h
                || (!matches!(h.as_str(), "[::1]")
                    && !h
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b)))
        })
        || c.operations.len() > 128
    {
        return Err("REST_CONFIG_SCOPE".into());
    }
    base(&c)?;
    required_secrets(&c)?;
    if let Some(p) = &c.openapi_path {
        let (_, names) = segments(p)?;
        if !names.is_empty() {
            return Err("REST_OPENAPI_FIXED_PATH".into());
        }
    }
    for (id, op) in &c.operations {
        if !atom(id)
            || op.method.len() > 32
            || reqwest::Method::from_bytes(op.method.as_bytes()).is_err()
            || matches!(op.method.to_ascii_uppercase().as_str(), "CONNECT" | "TRACE")
        {
            return Err("REST_OPERATION_ID_OR_METHOD".into());
        }
        let (_, names) = segments(&op.path)?;
        if parameters(&op.path_schema)? != names
            || op.path_schema["required"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).collect::<BTreeSet<_>>())
                .unwrap_or_default()
                != names.iter().map(String::as_str).collect::<BTreeSet<_>>()
        {
            return Err("REST_PATH_SCHEMA_MATCH".into());
        }
        parameters(&op.query_schema)?;
        if op.body_schema.is_some() && matches!(op.method.as_str(), "GET" | "HEAD") {
            return Err("REST_READ_BODY_DENIED".into());
        }
        if let Some(s) = &op.body_schema {
            schema(s)?;
        }
        if op
            .request_content_type
            .as_ref()
            .is_some_and(|s| !json_media(s))
        {
            return Err("REST_REQUEST_MEDIA_UNSUPPORTED".into());
        }
        if let Some(s) = &op.response_schema {
            schema(s)?;
            if op.response != ResponseKind::Json {
                return Err("REST_RESPONSE_SCHEMA_KIND".into());
            }
        }
        if op.method == "HEAD" && op.response != ResponseKind::Empty {
            return Err("REST_HEAD_RESPONSE".into());
        }
        if op.success_statuses.is_empty()
            || op.success_statuses.len() > 100
            || op.success_statuses.iter().any(|s| !(200..=299).contains(s))
            || op.success_statuses.iter().collect::<BTreeSet<_>>().len()
                != op.success_statuses.len()
        {
            return Err("REST_SUCCESS_STATUSES".into());
        }
        if !op.responses.is_empty()
            && op.responses.keys().copied().collect::<BTreeSet<_>>()
                != op.success_statuses.iter().copied().collect::<BTreeSet<_>>()
        {
            return Err("REST_RESPONSE_STATUS_MATCH".into());
        }
        for r in op.responses.values() {
            if let Some(s) = &r.schema {
                schema(s)?;
                if r.response != ResponseKind::Json {
                    return Err("REST_RESPONSE_SCHEMA_KIND".into());
                }
            }
            if (op.method == "HEAD" && r.response != ResponseKind::Empty)
                || r.content_type.as_ref().is_some_and(|s| match r.response {
                    ResponseKind::Json => !json_media(s),
                    ResponseKind::Text => s != "text/plain",
                    ResponseKind::Empty => true,
                })
            {
                return Err("REST_RESPONSE_MEDIA_UNSUPPORTED".into());
            }
        }
    }
    Ok(c)
}
fn valid(v: &Value, s: &Value) -> Result<(), String> {
    if !validator(s)?.is_valid(v) {
        return Err("REST_INPUT_SCHEMA".into());
    }
    Ok(())
}
fn scalar(v: &Value) -> Result<String, String> {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => return Err("REST_PARAMETER_PRIMITIVE_REQUIRED".into()),
    };
    if s.len() > 2048 || s.chars().any(char::is_control) {
        return Err("REST_PARAMETER_VALUE".into());
    }
    Ok(s)
}
pub fn plan(c: &Config, v: &Value) -> Result<Plan, String> {
    if v.to_string().len() > 128 * 1024 {
        return Err("REST_INPUT_LIMIT".into());
    }
    let input: Inputs = serde_json::from_value(v.clone()).map_err(|_| "REST_INPUT_CLOSED")?;
    let op = c
        .operations
        .get(&input.operation)
        .ok_or("REST_OPERATION_NOT_ALLOWED")?;
    let mutation = !matches!(op.method.as_str(), "GET" | "HEAD");
    if mutation && c.read_only {
        return Err("REST_READ_ONLY".into());
    }
    valid(&input.path, &op.path_schema)?;
    valid(&input.query, &op.query_schema)?;
    match (&input.body, &op.body_schema) {
        (Some(body), Some(s)) => {
            if body.to_string().len() > 65536 {
                return Err("REST_BODY_LIMIT".into());
            }
            valid(body, s)?;
        }
        (None, None) => {}
        (None, Some(_)) if !op.body_required => {}
        _ => return Err("REST_BODY_SCHEMA_REQUIRED".into()),
    }
    let (parts, _) = segments(&op.path)?;
    let mut url = base(c)?;
    {
        let mut target = url.path_segments_mut().map_err(|_| "REST_BASE_PATH")?;
        target.pop_if_empty();
        for segment in parts {
            if segment.starts_with('{') && segment.ends_with('}') {
                let text = scalar(&input.path[&segment[1..segment.len() - 1]])?;
                if text.is_empty()
                    || text == "."
                    || text == ".."
                    || text.contains(['/', '\\', '?', '#', '%'])
                {
                    return Err("REST_PATH_VALUE_SCOPE".into());
                }
                target.push(&text);
            } else {
                target.push(segment);
            }
        }
    }
    let query = input.query.as_object().ok_or("REST_QUERY_OBJECT")?;
    if !query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (name, value) in query {
            pairs.append_pair(name, &scalar(value)?);
        }
    }
    if url.as_str().len() > 8192 {
        return Err("REST_URL_LIMIT".into());
    }
    Ok(Plan {
        operation: input.operation,
        method: op.method.clone(),
        url,
        body: input.body,
        risk: if mutation { Risk::Mandatory } else { op.risk },
        mutation,
    })
}
