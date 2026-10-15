//! Explicit owned browser/UI observations; durable snapshots contain hashes, not page text.
use crate::{catalog, policy, runtime::Core};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
pub fn schema(tool: &str) -> Value {
    let mut p = json!({}).as_object().unwrap().clone();
    let mut required = Vec::new();
    if tool.ends_with(".list") || tool == "monitoring.check" {
        return json!({"type":"object","properties":p,"additionalProperties":false});
    }
    if tool.ends_with(".delete")
        || tool.ends_with(".unwatch")
        || tool.ends_with(".update")
        || tool.ends_with(".watch_update")
    {
        p.insert(
            "id".into(),
            json!({"type":"string","minLength":1,"maxLength":100}),
        );
        required.push("id");
        if tool.ends_with(".update") || tool.ends_with(".watch_update") {
            p.insert("body".into(),json!({"type":"object","properties":{"enabled":{"type":"boolean"}},"required":["enabled"],"additionalProperties":false}));
            required.push("body");
        }
    } else {
        p.insert(
            "path".into(),
            json!({"type":"string","minLength":1,"maxLength":8192}),
        );
        required.push("path");
        p.insert("kind".into(),json!({"enum":if tool.starts_with("files."){vec!["file","folder"]}else if tool.starts_with("downloads."){vec!["download"]}else{vec!["file","folder","download","port","process","window","page","ui_element"]}}));
        if tool == "monitoring.create" {
            required.push("kind");
        }
        p.insert("recursive".into(), json!({"type":"boolean"}));
        let gui = catalog::tool_schema("gui.find_element", "");
        p.insert("body".into(),json!({"type":"object","properties":{"condition":{"enum":["changed","exists","missing","open","closed","ready"]},"tab":{"type":"string","minLength":1,"maxLength":100},"selector":gui["properties"]["selector"],"page_selector":{"type":"string","minLength":1,"maxLength":2000},"process_id":{"type":"integer","minimum":1,"maximum":2147483647}},"additionalProperties":false}));
    }
    json!({"type":"object","properties":p,"required":required,"additionalProperties":false})
}
pub fn add_config(kind: &str, input: &Value, config: &mut Value) {
    if kind == "page" {
        config["tab"] = input["body"]["tab"].clone();
        config["page_selector"] = input["body"]["page_selector"]
            .as_str()
            .map_or(json!("body"), |s| json!(s));
    }
    if kind == "ui_element" {
        config["selector"] = input["body"]["selector"].clone();
        config["process_id"] = input["body"]["process_id"].clone();
    }
}
pub fn validate(kind: &str, config: &Value) -> Result<(), String> {
    if kind == "page" {
        let raw = config["path"].as_str().ok_or("WATCH_PAGE_URL")?;
        let url = reqwest::Url::parse(raw).map_err(|_| "WATCH_PAGE_URL")?;
        if !["http", "https"].contains(&url.scheme())
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || policy::contains_secret(raw)
        {
            return Err("WATCH_PAGE_URL".into());
        }
        if config["tab"]
            .as_str()
            .is_none_or(|s| s.is_empty() || s.len() > 100)
            || config["page_selector"]
                .as_str()
                .is_none_or(|s| s.is_empty() || s.len() > 2000)
        {
            return Err("WATCH_PAGE_TARGET".into());
        }
    } else {
        let input = json!({"hwnd":config["path"].as_str().and_then(|s|s.parse::<u64>().ok()),"process_id":config["process_id"],"selector":config["selector"]});
        if input["process_id"].is_null()
            || input["selector"].is_null()
            || !jsonschema::validator_for(&catalog::tool_schema(
                "gui.find_element",
                "hwnd,process_id,selector",
            ))
            .map_err(|_| "WATCH_UI_SCHEMA")?
            .is_valid(&input)
        {
            return Err("WATCH_UI_TARGET".into());
        }
    }
    Ok(())
}
fn page_observation(value: &Value) -> Result<Value, String> {
    let text = value["text"].as_str().ok_or("WATCH_PAGE_TEXT")?;
    if text.len() > 64000 || policy::contains_secret(text) {
        return Err("WATCH_PAGE_CONTENT_LIMIT_OR_SECRET".into());
    }
    Ok(
        json!({"present":true,"content_sha256":format!("{:x}",Sha256::digest(text.as_bytes())),"characters":text.chars().count()}),
    )
}
pub async fn snapshot(c: &Arc<Core>, kind: &str, config: &Value) -> Result<Value, String> {
    validate(kind, config)?;
    if kind == "page" {
        if !c.enabled("browser") {
            return Err("WATCH_BROWSER_DISABLED".into());
        }
        let r=c.browser("browser.extract",&json!({"tab":config["tab"],"url":config["path"],"selector":config["page_selector"],"body":{"max_chars":16000}})).await?;
        if !c.enabled("browser") || c.store.setting("privacy") == true {
            return Err("WATCH_SCOPE_REVOKED".into());
        }
        page_observation(&r)
    } else {
        if !c.enabled("gui") {
            return Err("WATCH_GUI_DISABLED".into());
        }
        let r=crate::gui::dispatch(c,"gui.find_element",&json!({"hwnd":config["path"].as_str().unwrap().parse::<u64>().unwrap(),"process_id":config["process_id"],"selector":config["selector"]})).await;
        if !c.enabled("gui") || c.store.setting("privacy") == true {
            return Err("WATCH_SCOPE_REVOKED".into());
        }
        match r {
            Ok(v) => Ok(
                json!({"present":true,"target_sha256":format!("{:x}",Sha256::digest(v.to_string().as_bytes()))}),
            ),
            Err(e) if e.contains("UIA_ELEMENT_NOT_FOUND") => Ok(json!({"present":false})),
            Err(e) => Err(e),
        }
    }
}
#[cfg(all(test, windows))]
#[path = "watch_extra_contracts.rs"]
mod contracts;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owned_page_targets_and_hashes_never_persist_text() {
        let c = json!({"path":"https://example.org/owned","tab":"owned-tab","page_selector":"#content"});
        assert!(validate("page", &c).is_ok());
        let mut bad = c.clone();
        bad["path"] = json!("https://user:password@example.org/owned");
        assert!(validate("page", &bad).is_err());
        bad = c.clone();
        bad["tab"] = Value::Null;
        assert!(validate("page", &bad).is_err());
        let a = page_observation(&json!({"text":"Owned private DOM content"})).unwrap();
        let b = page_observation(&json!({"text":"Updated private DOM content"})).unwrap();
        assert_ne!(a, b);
        assert!(!a.to_string().contains("private DOM"));
    }
    #[test]
    fn ui_selectors_are_closed_and_appearances_require_explicit_pid() {
        let valid = json!({"path":"123","process_id":44,"selector":{"automation_id":"owned","scope":"descendants"}});
        assert!(validate("ui_element", &valid).is_ok());
        let mut wrong = valid;
        wrong["process_id"] = Value::Null;
        assert!(validate("ui_element", &wrong).is_err());
        let s = jsonschema::validator_for(&schema("monitoring.create")).unwrap();
        assert!(!s.is_valid(
            &json!({"kind":"page","path":"https://example.org","body":{"javascript":"arbitrary"}})
        ));
    }
}
