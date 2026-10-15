//! Generic MCP client. This transport layer grants no tool authority. Core
//! policy remains Mandatory; prepared profiles/credential binding are separate.
#[cfg(test)]
use crate::policy;
use serde_json::{json, Value};
use std::collections::BTreeSet;
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use tokio_util::sync::CancellationToken;
#[path = "mcp_protocol.rs"]
pub(crate) mod protocol;
#[path = "mcp_schema.rs"]
pub(crate) mod schema;
#[path = "mcp_sse.rs"]
mod sse;
#[path = "mcp_transport.rs"]
pub(crate) mod transport;

pub(crate) fn tool_schema(tool: &str) -> Value {
    let name = json!({"type":"string","minLength":1,"maxLength":256,"pattern":"^[^\\u0000-\\u001f\\u007f]+$"});
    let mut props = serde_json::Map::new();
    props.insert(
        "connection".into(),
        json!({"type":"string","format":"uuid","minLength":36,"maxLength":36,"pattern":"^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$"}),
    );
    let mut required = vec!["connection"];
    match tool {
        "mcp.health" => (),
        "mcp.listen" => {
            props.insert("notifications".into(), json!({"type":"object","minProperties":1,"properties":{"toolsListChanged":{"type":"boolean"},"resourcesListChanged":{"type":"boolean"},"promptsListChanged":{"type":"boolean"},"resourceSubscriptions":{"type":"array","maxItems":32,"uniqueItems":true,"items":{"type":"string","minLength":1,"maxLength":8192,"format":"uri"}}},"additionalProperties":false}));
            required.push("notifications");
        }
        "mcp.listen_status" | "mcp.listen_poll" | "mcp.listen_stop" => {
            props.insert("stream_id".into(), props["connection"].clone());
            if tool == "mcp.listen_status" {
                required.clear();
            } else {
                required.push("stream_id");
            }
            if tool == "mcp.listen_poll" {
                props.insert(
                    "max_events".into(),
                    json!({"type":"integer","minimum":1,"maximum":128}),
                );
            }
        }
        "mcp.oauth_discover" => {
            props.insert(
                "issuer".into(),
                json!({"type":"string","minLength":1,"maxLength":4096}),
            );
            props.insert(
                "metadata_url".into(),
                json!({"type":"string","minLength":1,"maxLength":4096}),
            );
            required.push("issuer");
        }
        "mcp.oauth_registration" | "mcp.oauth_start" => {
            props.insert(
                "issuer".into(),
                json!({"type":"string","minLength":1,"maxLength":4096}),
            );
            props.insert(
                "client_auth".into(),
                json!({"enum":["none","client_secret_basic","client_secret_post"]}),
            );
            props.insert("preregistered".into(), json!({"type":"object","properties":{"client_id":{"type":"string","minLength":1,"maxLength":4096},"redirect_uri":{"type":"string","minLength":1,"maxLength":4096}},"required":["client_id","redirect_uri"],"additionalProperties":false}));
            props.insert(
                "metadata_document_url".into(),
                json!({"type":"string","minLength":1,"maxLength":4096}),
            );
            props.insert("allow_legacy_dynamic".into(), json!({"type":"boolean"}));
            props.insert("requested_scopes".into(), json!({"type":["array","null"],"minItems":1,"maxItems":256,"uniqueItems":true,"items":{"type":"string","minLength":1,"maxLength":1024}}));
            required.push("issuer");
        }
        "mcp.oauth_status" => (),
        "mcp.oauth_cancel" | "mcp.oauth_refresh" | "mcp.oauth_recover" | "mcp.oauth_cleanup"
        | "mcp.oauth_reset" => {
            props.insert("session_id".into(),json!({"type":"string","minLength":36,"maxLength":36,"pattern":"^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$"}));
            required.push("session_id");
            if tool == "mcp.oauth_reset" {
                props.insert("acknowledge_remote_unknown".into(), json!({"const":true}));
                required.push("acknowledge_remote_unknown");
            }
        }
        "mcp.tools" | "mcp.resources" | "mcp.prompts" => {
            props.insert(
                "cursor".into(),
                json!({"type":"string","minLength":1,"maxLength":4096}),
            );
        }
        "mcp.call" | "mcp.get_prompt" => {
            props.insert("name".into(), name);
            props.insert("body".into(),if tool=="mcp.get_prompt" {
                json!({"type":"object","maxProperties":128,"additionalProperties":{"type":"string","maxLength":65536}})
            } else { json!({"type":"object","maxProperties":256}) });
            required.extend(["name", "body"]);
        }
        "mcp.read_resource" => {
            props.insert(
                "uri".into(),
                json!({"type":"string","minLength":1,"maxLength":8192,"format":"uri"}),
            );
            required.push("uri");
        }
        _ => return json!({"not":{}}),
    }
    if matches!(
        tool,
        "mcp.call"
            | "mcp.read_resource"
            | "mcp.get_prompt"
            | "mcp.oauth_registration"
            | "mcp.oauth_start"
    ) {
        props.insert("expected".into(), json!({"type":"object","properties":{"fingerprint":{"type":"string","pattern":"^[a-f0-9]{64}$"}},"required":["fingerprint"],"additionalProperties":false}));
        required.push("expected");
    }
    json!({"type":"object","properties":props,"required":required,"additionalProperties":false})
}

pub(crate) fn request(action: &str, input: &Value) -> Result<Value, String> {
    if matches!(action, "call" | "get_prompt") {
        if input["name"]
            .as_str()
            .is_none_or(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
        {
            return Err("MCP_TOOL_NAME".into());
        }
        if !input["body"].is_object() {
            return Err("MCP_ARGUMENTS_OBJECT".into());
        }
        if action == "get_prompt"
            && input["body"]
                .as_object()
                .is_none_or(|m| m.values().any(|v| !v.is_string()))
        {
            return Err("MCP_PROMPT_ARGUMENTS".into());
        }
    }
    if action == "read_resource"
        && input["uri"]
            .as_str()
            .is_none_or(|s| s.is_empty() || s.len() > 8192)
    {
        return Err("MCP_RESOURCE_URI".into());
    }
    let (method, mut params) = match action {
        "tools" => ("tools/list", json!({})),
        "resources" => ("resources/list", json!({})),
        "prompts" => ("prompts/list", json!({})),
        "call" => (
            "tools/call",
            json!({"name": input["name"], "arguments": input["body"]}),
        ),
        "read_resource" => ("resources/read", json!({"uri": input["uri"]})),
        "get_prompt" => (
            "prompts/get",
            json!({"name": input["name"], "arguments": input["body"]}),
        ),
        _ => return Err("MCP_ACTION".into()),
    };
    if ["tools", "resources", "prompts"].contains(&action) {
        if let Some(cursor) = input.get("cursor") {
            cursor_value(cursor)?;
            params["cursor"] = cursor.clone();
        }
    }
    let request = json!({"jsonrpc":"2.0","id":2,"method":method,"params":params});
    schema::bounded(&request, 128 * 1024)?;
    Ok(request)
}
pub(crate) fn cursor_value(v: &Value) -> Result<&str, String> {
    v.as_str()
        .filter(|s| !s.is_empty() && s.len() <= 4096)
        .ok_or("MCP_CURSOR_INVALID".into())
}
pub(crate) fn capability(session: &transport::Session, action: &str) -> Result<(), String> {
    let key = match action {
        "tools" | "call" => "tools",
        "resources" | "read_resource" => "resources",
        "prompts" | "get_prompt" => "prompts",
        _ => return Err("MCP_ACTION".into()),
    };
    if !session.info["capabilities"][key].is_object() {
        return Err("MCP_CAPABILITY_NOT_DECLARED".into());
    }
    Ok(())
}
pub(crate) fn tool_page(
    result: &Value,
    mirrored: bool,
) -> Result<(Value, Vec<schema::Tool>), String> {
    let raw = result["tools"].as_array().ok_or("MCP_TOOLS_LIST")?;
    if raw.len() > 256 {
        return Err("MCP_TOOLS_LIST_LIMIT".into());
    }
    let mut seen = BTreeSet::new();
    let mut tools = vec![];
    let mut excluded = vec![];
    for value in raw {
        if let Some(name) = value["name"].as_str() {
            if !seen.insert(name) {
                return Err("MCP_DUPLICATE_TOOL".into());
            }
        }
        match schema::Tool::new(value, mirrored) {
            Ok(tool) => tools.push(tool),
            Err(reason) => excluded.push(json!({"name":value["name"].as_str().unwrap_or("").chars().take(256).collect::<String>(),"reason":reason})),
        }
    }
    let mut result = result.clone();
    result["tools"] = json!(tools.iter().map(|t| &t.definition).collect::<Vec<_>>());
    result["alfredpetValidation"] =
        json!({"excludedInvalidTools": excluded.len(),"warnings":excluded});
    if let Some(cursor) = result.get("nextCursor") {
        cursor_value(cursor)?;
    }
    Ok((result, tools))
}
pub(crate) async fn find_tool(
    session: &mut transport::Session,
    name: &str,
) -> Result<schema::Tool, String> {
    if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        return Err("MCP_TOOL_NAME".into());
    }
    let mut cursor = None;
    let mut cursors = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut selected = None;
    for _ in 0..16 {
        let input = cursor
            .as_ref()
            .map(|c| json!({"cursor":c}))
            .unwrap_or_else(|| json!({}));
        let result = session.rpc(&request("tools", &input)?, &[]).await?;
        for raw in result["tools"].as_array().ok_or("MCP_TOOLS_LIST")? {
            if let Some(name) = raw["name"].as_str() {
                if !names.insert(name.to_string()) {
                    return Err("MCP_DUPLICATE_TOOL".into());
                }
                if names.len() > 2048 {
                    return Err("MCP_TOOLS_LIST_LIMIT".into());
                }
            }
        }
        let (result, tools) = tool_page(&result, session.mirrored())?;
        for tool in tools {
            let tool_name = tool.definition["name"].as_str().ok_or("MCP_TOOL_NAME")?;
            if tool_name == name {
                selected = Some(tool);
            }
        }
        let Some(next) = result.get("nextCursor") else {
            return selected.ok_or("MCP_TOOL_NOT_AVAILABLE".into());
        };
        let next = cursor_value(next)?.to_string();
        if !cursors.insert(next.clone()) {
            return Err("MCP_CURSOR_CYCLE".into());
        }
        cursor = Some(next);
    }
    Err("MCP_PAGINATION_LIMIT".into())
}
#[cfg(test)]
async fn operation(
    session: &mut transport::Session,
    action: &str,
    input: &Value,
    dispatched: &mut bool,
) -> Result<Value, String> {
    if action == "health" {
        return Ok(
            json!({"protocolVersion":session.version(),"discovery":session.info,"businessToolExecuted":false}),
        );
    }
    capability(session, action)?;
    let request = request(action, input)?;
    if action == "call" {
        let name = input["name"].as_str().ok_or("MCP_TOOL_NAME")?;
        let tool = find_tool(session, name).await?;
        tool.arguments(&request["params"]["arguments"])?;
        // Validate mirrored fields before the conservative dispatch boundary.
        if session.mirrored() {
            protocol::headers(&protocol::modern_request(&request)?, &tool.fields)?;
        }
        *dispatched = true;
        let result = session.rpc(&request, &tool.fields).await?;
        tool.output(&result)?;
        return Ok(result);
    }
    let result = session.rpc(&request, &[]).await?;
    if action == "tools" {
        return tool_page(&result, session.mirrored()).map(|r| r.0);
    }
    if ["resources", "prompts"].contains(&action) {
        let items = result[action].as_array().ok_or("MCP_LIST_RESULT")?;
        if items.len() > 256 {
            return Err("MCP_LIST_LIMIT".into());
        }
        if let Some(cursor) = result.get("nextCursor") {
            cursor_value(cursor)?;
        }
    }
    Ok(result)
}
#[cfg(test)]
pub async fn call(
    config: &Value,
    token: Option<&str>,
    action: &str,
    input: &Value,
) -> Result<Value, String> {
    call_cancellable(config, token, action, input, CancellationToken::new()).await
}
#[cfg(test)]
pub async fn call_cancellable(
    config: &Value,
    token: Option<&str>,
    action: &str,
    input: &Value,
    cancel: CancellationToken,
) -> Result<Value, String> {
    // Local invalid input cannot start a configured subprocess or HTTP probe.
    if action != "health" {
        request(action, input)?;
    }
    let opened = tokio::select! { biased;
        _ = cancel.cancelled() => return Err("MCP_CANCELLED".into()),
        r = tokio::time::timeout(Duration::from_secs(30), transport::Session::open(config, token)) => r.map_err(|_| "MCP_TIMEOUT")?,
    };
    let mut session = opened?;
    let mut dispatched = false;
    let result = tokio::select! { biased;
        _ = cancel.cancelled() => Err("MCP_CANCELLED".into()),
        r = tokio::time::timeout(Duration::from_secs(30), operation(&mut session, action, input, &mut dispatched)) => r.map_err(|_| "MCP_TIMEOUT".to_string()).and_then(|r| r),
    };
    session.close().await;
    let result = match result {
        Err(error) if dispatched => return Err(format!("MCP_OUTCOME_UNCERTAIN:{error}")),
        other => other?,
    };
    let mut result = result;
    if let Some(object) = result.as_object_mut() {
        object.remove("undo_token");
        object.remove("undoToken");
    }
    let result = policy::redact(&result);
    Ok(if let Some(token) = token.filter(|t| !t.is_empty()) {
        redact_literal(&result, token)
    } else {
        result
    })
}
pub(crate) fn redact_literal(v: &Value, needle: &str) -> Value {
    match v {
        Value::String(s) if s.contains(needle) => json!("[REDACTED]"),
        Value::Array(a) => json!(a
            .iter()
            .map(|v| redact_literal(v, needle))
            .collect::<Vec<_>>()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| {
                    (
                        if k.contains(needle) {
                            "[REDACTED]".into()
                        } else {
                            k.clone()
                        },
                        redact_literal(v, needle),
                    )
                })
                .collect(),
        ),
        _ => v.clone(),
    }
}
#[cfg(test)]
const VERSION: &str = protocol::LEGACY[0];
#[cfg(test)]
const LIMIT: usize = protocol::LIMIT;
#[cfg(test)]
fn rpc_result(v: &Value, id: i64) -> Result<Value, String> {
    protocol::complete(v, &json!(id), false).cloned()
}
#[cfg(test)]
#[path = "mcp_client_tests.rs"]
mod tests;
