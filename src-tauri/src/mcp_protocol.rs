//! MCP wire semantics. Server identity and schemas are untrusted data; this
//! module grants no tool authority, credentials or capability by itself.
use base64::Engine;
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub const MODERN: &str = "2026-07-28";
pub const LEGACY: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];
pub const LIMIT: usize = 8 * 1024 * 1024;
pub const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";

pub enum Reply<'a> {
    Result(&'a Value),
    Error(i64),
}
pub fn reply<'a>(value: &'a Value, id: &Value) -> Result<Reply<'a>, String> {
    if !value.is_object()
        || value["jsonrpc"] != "2.0"
        || value["id"] != *id
        || !(id.is_string() || id.as_i64().is_some())
        || value.get("method").is_some()
        || value.get("result").is_some() == value.get("error").is_some()
    {
        return Err("MCP_RESPONSE_SCOPE".into());
    }
    if let Some(error) = value.get("error") {
        let code = error["code"].as_i64().ok_or("MCP_ERROR_ENVELOPE")?;
        if !error.is_object() || !error["message"].is_string() {
            return Err("MCP_ERROR_ENVELOPE".into());
        }
        return Ok(Reply::Error(code));
    }
    let result = value
        .get("result")
        .filter(|r| r.is_object())
        .ok_or("MCP_RESULT_OBJECT")?;
    Ok(Reply::Result(result))
}
pub fn complete<'a>(value: &'a Value, id: &Value, modern: bool) -> Result<&'a Value, String> {
    let result = match reply(value, id)? {
        Reply::Result(r) => r,
        Reply::Error(_) => return Err("MCP_REMOTE_ERROR".into()),
    };
    match result.get("resultType").and_then(Value::as_str) {
        Some("complete") => (),
        Some("input_required") => return Err("MCP_INPUT_REQUIRED".into()),
        None if !modern && result.get("resultType").is_none() => (),
        _ => return Err("MCP_RESULT_TYPE".into()),
    }
    if result["isError"] == true {
        return Err("MCP_TOOL_ERROR".into());
    }
    Ok(result)
}
pub fn modern_error(code: i64) -> bool {
    matches!(code, -32022..=-32020)
}
pub fn modern_request(request: &Value) -> Result<Value, String> {
    if request["jsonrpc"] != "2.0"
        || !request["method"].is_string()
        || !request
            .get("id")
            .is_some_and(|v| v.is_string() || v.as_i64().is_some())
    {
        return Err("MCP_REQUEST_ENVELOPE".into());
    }
    let mut result = request.clone();
    if result.get("params").is_none() {
        result["params"] = json!({})
    }
    let params = result["params"]
        .as_object_mut()
        .ok_or("MCP_REQUEST_PARAMS")?;
    // No caller-supplied metadata can add roots/sampling/elicitation capabilities.
    params.insert("_meta".into(),json!({META_VERSION:MODERN,"io.modelcontextprotocol/clientInfo":{"name":"alfredpet","version":"0.1.0"},"io.modelcontextprotocol/clientCapabilities":{}}));
    if result.to_string().len() > LIMIT {
        return Err("MCP_REQUEST_LIMIT".into());
    }
    Ok(result)
}
pub fn discover(id: &Value) -> Result<Value, String> {
    modern_request(&json!({"jsonrpc":"2.0","id":id,"method":"server/discover","params":{}}))
}
pub fn discovery(value: &Value, id: &Value) -> Result<Value, String> {
    match reply(value, id)? {
        Reply::Error(code) if modern_error(code) => return Err("MCP_MODERN_PROTOCOL_ERROR".into()),
        Reply::Error(_) => return Err("MCP_LEGACY_PROBE_ERROR".into()),
        Reply::Result(_) => (),
    }
    let result = complete(value, id, true)?;
    let versions = result["supportedVersions"]
        .as_array()
        .ok_or("MCP_DISCOVERY_VERSIONS")?;
    if versions.is_empty()
        || versions.len() > 16
        || versions
            .iter()
            .any(|v| v.as_str().is_none_or(|s| s.len() > 32))
    {
        return Err("MCP_DISCOVERY_VERSIONS".into());
    }
    if !versions.iter().any(|v| v == MODERN) {
        return Err("MCP_PROTOCOL_UNSUPPORTED".into());
    }
    if !result["capabilities"].is_object() {
        return Err("MCP_DISCOVERY_CAPABILITIES".into());
    }
    Ok(result.clone())
}
pub fn encoded_header(value: &str) -> Result<String, String> {
    if value.len() > 8192 {
        return Err("MCP_HEADER_VALUE_LIMIT".into());
    }
    let plain = value.bytes().all(|b| b == 9 || (32..=126).contains(&b))
        && value.trim_matches([' ', '\t']) == value
        && !(value.starts_with("=?base64?") && value.ends_with("?="));
    Ok(if plain {
        value.to_string()
    } else {
        format!(
            "=?base64?{}?=",
            base64::engine::general_purpose::STANDARD.encode(value.as_bytes())
        )
    })
}
fn token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}
#[derive(Clone)]
pub struct HeaderField {
    pub name: String,
    pub path: Vec<String>,
    kinds: BTreeSet<String>,
}
pub fn fields(schema: &Value) -> Result<Vec<HeaderField>, String> {
    if schema.to_string().len() > 65536 {
        return Err("MCP_HEADER_SCHEMA_LIMIT".into());
    }
    fn walk(
        v: &Value,
        path: &[String],
        reachable: bool,
        depth: usize,
        nodes: &mut usize,
        out: &mut Vec<HeaderField>,
        names: &mut BTreeSet<String>,
    ) -> Result<(), String> {
        *nodes += 1;
        if depth > 24 || *nodes > 4096 {
            return Err("MCP_HEADER_SCHEMA_LIMIT".into());
        }
        let Some(obj) = v.as_object() else {
            return Ok(());
        };
        if let Some(annotation) = obj.get("x-mcp-header") {
            let name = annotation
                .as_str()
                .filter(|s| token(s))
                .ok_or("MCP_HEADER_ANNOTATION")?;
            if !reachable
                || path.is_empty()
                || out.len() >= 32
                || !names.insert(name.to_ascii_lowercase())
            {
                return Err("MCP_HEADER_ANNOTATION".into());
            }
            let kinds: BTreeSet<String> = match obj.get("type") {
                Some(Value::String(s)) => [s.clone()].into_iter().collect(),
                Some(Value::Array(a)) => a
                    .iter()
                    .map(|s| {
                        s.as_str()
                            .map(str::to_string)
                            .ok_or("MCP_HEADER_TYPE".to_string())
                    })
                    .collect::<Result<_, _>>()?,
                _ => return Err("MCP_HEADER_TYPE".into()),
            };
            if kinds.is_empty()
                || !kinds.iter().any(|k| k != "null")
                || kinds
                    .iter()
                    .any(|k| !["string", "integer", "boolean", "null"].contains(&k.as_str()))
            {
                return Err("MCP_HEADER_TYPE".into());
            }
            out.push(HeaderField {
                name: format!("Mcp-Param-{name}"),
                path: path.to_vec(),
                kinds,
            });
        }
        for (key, child) in obj {
            if [
                "properties",
                "patternProperties",
                "$defs",
                "definitions",
                "dependentSchemas",
                "dependencies",
            ]
            .contains(&key.as_str())
            {
                if let Some(map) = child.as_object() {
                    for (property, schema) in map {
                        let mut next = path.to_vec();
                        next.push(property.clone());
                        walk(
                            schema,
                            &next,
                            reachable && key == "properties",
                            depth + 1,
                            nodes,
                            out,
                            names,
                        )?;
                    }
                }
            } else if ["allOf", "anyOf", "oneOf", "prefixItems"].contains(&key.as_str()) {
                if let Some(array) = child.as_array() {
                    for schema in array {
                        walk(schema, path, false, depth + 1, nodes, out, names)?;
                    }
                }
            } else if [
                "items",
                "additionalItems",
                "additionalProperties",
                "unevaluatedProperties",
                "unevaluatedItems",
                "contains",
                "propertyNames",
                "not",
                "if",
                "then",
                "else",
                "contentSchema",
            ]
            .contains(&key.as_str())
            {
                if let Some(array) = child.as_array() {
                    for schema in array {
                        walk(schema, path, false, depth + 1, nodes, out, names)?;
                    }
                } else {
                    walk(child, path, false, depth + 1, nodes, out, names)?;
                }
            }
        }
        Ok(())
    }
    let mut result = vec![];
    walk(
        schema,
        &[],
        true,
        0,
        &mut 0,
        &mut result,
        &mut BTreeSet::new(),
    )?;
    Ok(result)
}
pub fn headers(
    request: &Value,
    bindings: &[HeaderField],
) -> Result<reqwest::header::HeaderMap, String> {
    if request["params"]["_meta"][META_VERSION] != MODERN {
        return Err("MCP_HEADER_METADATA".into());
    }
    let method = request["method"].as_str().ok_or("MCP_HEADER_METHOD")?;
    if method.is_empty() || method.len() > 128 || !method.bytes().all(|b| (33..=126).contains(&b)) {
        return Err("MCP_HEADER_METHOD".into());
    }
    let mut pairs = vec![
        ("MCP-Protocol-Version".to_string(), MODERN.to_string()),
        ("Mcp-Method".to_string(), method.to_string()),
    ];
    let name = match method {
        "tools/call" | "prompts/get" => Some("name"),
        "resources/read" => Some("uri"),
        _ => None,
    };
    if let Some(key) = name {
        pairs.push((
            "Mcp-Name".into(),
            encoded_header(request["params"][key].as_str().ok_or("MCP_HEADER_NAME")?)?,
        ));
    }
    if method == "tools/call" {
        for field in bindings {
            let mut value = &request["params"]["arguments"];
            for key in &field.path {
                value = &value[key];
            }
            if value.is_null() {
                continue;
            }
            let text = if let Some(s) = value.as_str().filter(|_| field.kinds.contains("string")) {
                s.to_string()
            } else if let Some(b) = value.as_bool().filter(|_| field.kinds.contains("boolean")) {
                b.to_string()
            } else if let Some(n) = value.as_f64().filter(|n| {
                field.kinds.contains("integer") && n.fract() == 0.0 && n.abs() <= 9007199254740991.0
            }) {
                format!("{n:.0}")
            } else {
                return Err("MCP_HEADER_ARGUMENT_TYPE".into());
            };
            pairs.push((field.name.clone(), encoded_header(&text)?));
        }
    }
    let mut headers = reqwest::header::HeaderMap::new();
    for (name, value) in pairs {
        let key = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| "MCP_HEADER_NAME")?;
        if headers.contains_key(&key) {
            return Err("MCP_HEADER_DUPLICATE".into());
        }
        let mut value =
            reqwest::header::HeaderValue::from_str(&value).map_err(|_| "MCP_HEADER_VALUE")?;
        value.set_sensitive(true);
        headers.insert(key, value);
    }
    Ok(headers)
}

#[cfg(test)]
#[path = "mcp_protocol_tests.rs"]
mod tests;
