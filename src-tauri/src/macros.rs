//! Personal declarations; every child is executed through the normal Core policy.
use crate::{policy, runtime::Core, storage};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};

const LIMIT: usize = 64 * 1024;
fn authority(c: &Core) -> Result<String, String> {
    let rows = c.store.query(
        "SELECT json_value FROM settings WHERE key='macro_authority_generation'",
        &[],
    )?;
    if rows.as_array().ok_or("MACRO_AUTHORITY")?.is_empty() {
        return Err("MACRO_AUTHORITY".into());
    }
    let value: Value =
        serde_json::from_str(rows[0]["json_value"].as_str().ok_or("MACRO_AUTHORITY")?)
            .map_err(|_| "MACRO_AUTHORITY")?;
    let id = value.as_str().ok_or("MACRO_AUTHORITY")?;
    canonical(id).map_err(|_| "MACRO_AUTHORITY")?;
    Ok(id.into())
}
pub(crate) fn revoke(c: &Core) -> Result<(), String> {
    c.store
        .set_setting("macro_authority_generation", &json!(storage::id()))
}
fn initial_authority(c: &Core) -> Result<String, String> {
    c.store.execute(
        "INSERT OR IGNORE INTO settings(key,json_value) VALUES('macro_authority_generation',?)",
        &[json!(json!(storage::id()).to_string())],
    )?;
    authority(c)
}
pub(crate) const TOOLS: &[&str] = &[
    "macros.list",
    "macros.inspect",
    "macros.save",
    "macros.run",
    "macros.set_enabled",
    "macros.delete",
];
pub(crate) fn schema(tool: &str) -> Value {
    let id = json!({"type":"string","pattern":"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"});
    let expected = json!({"type":"object","required":["fingerprint"],"properties":{"fingerprint":{"type":"string","pattern":"^[a-f0-9]{64}$"}},"additionalProperties":false});
    let (required, properties) = match tool {
        "macros.list" => (vec![], json!({})),
        "macros.inspect" => (vec!["id"], json!({"id":id})),
        "macros.run" => (
            vec!["id"],
            json!({"id":id,"body":{"type":"object","maxProperties":16},"expected":expected}),
        ),
        "macros.set_enabled" => (
            vec!["id", "enabled", "expected"],
            json!({"id":id,"enabled":{"type":"boolean"},"expected":expected}),
        ),
        "macros.delete" => (vec!["id", "expected"], json!({"id":id,"expected":expected})),
        "macros.save" => (
            vec!["name", "steps"],
            json!({"name":{"type":"string","minLength":1,"maxLength":200},"description":{"type":"string","maxLength":4000},"parameters":{"type":"object","maxProperties":16},"steps":{"type":"array","minItems":1,"maxItems":50,"items":{"type":"object"}},"error_strategy":{"enum":["stop","continue"]},"shortcut":{"type":"string","minLength":1,"maxLength":100}}),
        ),
        _ => return json!({"not":{}}),
    };
    json!({"type":"object","required":required,"properties":properties,"additionalProperties":false})
}
fn canonical(id: &str) -> Result<(), String> {
    let value = uuid::Uuid::parse_str(id).map_err(|_| "MACRO_ID")?;
    if value.is_nil() || value.to_string() != id {
        return Err("MACRO_ID".into());
    }
    Ok(())
}
fn templated(v: &Value) -> bool {
    match v {
        Value::Object(m) => m.contains_key("$parameter") || m.values().any(templated),
        Value::Array(a) => a.iter().any(templated),
        _ => false,
    }
}
fn closed(v: &Value, fields: &[&str]) -> Result<(), String> {
    let map = v.as_object().ok_or("MACRO_OBJECT")?;
    if map.keys().any(|k| !fields.contains(&k.as_str())) {
        return Err("MACRO_UNKNOWN_FIELD".into());
    }
    Ok(())
}
fn name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}
fn safe(v: &Value, depth: usize) -> Result<(), String> {
    if depth > 16 || v.to_string().len() > LIMIT {
        return Err("MACRO_LIMIT".into());
    }
    match v {
        Value::String(s) if policy::contains_secret(s) => return Err("MACRO_SECRET".into()),
        Value::Array(a) => {
            for v in a {
                safe(v, depth + 1)?;
            }
        }
        Value::Object(m) => {
            for (k, v) in m {
                if ["password", "token", "secret", "api_key", "authorization"]
                    .contains(&k.to_ascii_lowercase().as_str())
                {
                    return Err("MACRO_SECRET".into());
                }
                safe(v, depth + 1)?;
            }
        }
        _ => (),
    }
    Ok(())
}
fn typed(value: &Value, spec: &Value) -> bool {
    let ok = match spec["type"].as_str() {
        Some("string") => value.as_str().is_some_and(|s| s.len() <= 4000),
        Some("integer") => value
            .as_i64()
            .is_some_and(|n| n.unsigned_abs() <= 9007199254740991),
        Some("number") => value
            .as_f64()
            .is_some_and(|n| n.is_finite() && n.abs() <= 9007199254740991.),
        Some("boolean") => value.is_boolean(),
        _ => false,
    };
    ok && spec
        .get("enum")
        .is_none_or(|e| e.as_array().is_some_and(|a| a.contains(value)))
}
fn strategy(v: Option<&Value>) -> Result<&str, String> {
    match v {
        None => Ok("stop"),
        Some(Value::String(s)) if ["stop", "continue"].contains(&s.as_str()) => Ok(s),
        _ => Err("MACRO_ERROR_STRATEGY".into()),
    }
}
fn boundary_error(error: &str) -> bool {
    error.contains("UNCERTAIN")
        || error.contains("CHANGED")
        || error.contains("REVOKED")
        || error.contains("DENIED")
        || error.contains("TIMEOUT")
        || error.starts_with("PRIVACY_")
        || error.starts_with("MODULE_DISABLED")
        || error.starts_with("TOOL_DISABLED")
        || error.starts_with("MACRO_CONTINUATION_CANCELLED")
}
fn resolve(v: &Value, values: &BTreeMap<String, Value>, depth: usize) -> Result<Value, String> {
    if depth > 16 {
        return Err("MACRO_LIMIT".into());
    }
    Ok(match v {
        Value::Object(m) if m.contains_key("$parameter") => {
            if m.len() != 1 {
                return Err("MACRO_PARAMETER_LEAF".into());
            }
            values
                .get(m["$parameter"].as_str().ok_or("MACRO_PARAMETER_NAME")?)
                .cloned()
                .ok_or_else(|| "MACRO_PARAMETER_REQUIRED".to_string())?
        }
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| Ok((k.clone(), resolve(v, values, depth + 1)?)))
                .collect::<Result<_, String>>()?,
        ),
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|v| resolve(v, values, depth + 1))
                .collect::<Result<_, _>>()?,
        ),
        _ => v.clone(),
    })
}
fn condition(v: Option<&Value>, values: &BTreeMap<String, Value>) -> Result<bool, String> {
    let Some(v) = v else {
        return Ok(true);
    };
    closed(v, &["parameter", "operator", "value"])?;
    let key = v["parameter"]
        .as_str()
        .filter(|s| name(s))
        .ok_or("MACRO_CONDITION_PARAMETER")?;
    let value = values.get(key);
    match v["operator"].as_str() {
        Some("exists") if v.get("value").is_none() => Ok(value.is_some()),
        Some("equals") if v.get("value").is_some() => Ok(value == v.get("value")),
        Some("not_equals") if v.get("value").is_some() => Ok(value != v.get("value")),
        _ => Err("MACRO_CONDITION_OPERATOR".into()),
    }
}
fn known(c: &Core, tool: &str, active: bool) -> Option<Value> {
    c.tools().into_iter().find(|t| t["id"] == tool).or_else(|| {
        if active {
            return None;
        }
        crate::catalog::MODULES.iter().find_map(|module| {
            module.tools.iter().find_map(|(action, required)| {
                (format!("{}.{action}", module.id) == tool).then(
                    || json!({"id":tool,"inputSchema":crate::catalog::tool_schema(tool,required)}),
                )
            })
        })
    })
}
fn normalize(c: &Core, raw: &Value, active: bool) -> Result<Value, String> {
    let definition = if raw.is_array() {
        json!({"version":2,"description":"","parameters":{},"steps":raw,"error_strategy":"stop"})
    } else {
        raw.clone()
    };
    closed(
        &definition,
        &[
            "version",
            "description",
            "parameters",
            "steps",
            "error_strategy",
            "shortcut",
        ],
    )?;
    if definition["version"] != 2
        || definition["description"]
            .as_str()
            .is_none_or(|s| s.len() > 4000)
    {
        return Err("MACRO_DEFINITION".into());
    }
    safe(&definition, 0)?;
    strategy(definition.get("error_strategy"))?;
    let parameters = definition["parameters"]
        .as_object()
        .ok_or("MACRO_PARAMETERS")?;
    if parameters.len() > 16 {
        return Err("MACRO_PARAMETER_LIMIT".into());
    }
    let mut samples = BTreeMap::new();
    for (key, spec) in parameters {
        if !name(key) {
            return Err("MACRO_PARAMETER_NAME".into());
        }
        closed(
            spec,
            &["type", "description", "required", "default", "enum"],
        )?;
        if spec.get("required").is_some_and(|v| !v.is_boolean())
            || spec
                .get("description")
                .is_some_and(|v| v.as_str().is_none_or(|s| s.len() > 1000))
        {
            return Err("MACRO_PARAMETER_SPEC".into());
        }
        let sample = match spec["type"].as_str() {
            Some("string") => json!(""),
            Some("integer" | "number") => json!(0),
            Some("boolean") => json!(false),
            _ => return Err("MACRO_PARAMETER_TYPE".into()),
        };
        if let Some(e) = spec.get("enum") {
            let choices = e
                .as_array()
                .filter(|a| !a.is_empty() && a.len() <= 32)
                .ok_or("MACRO_PARAMETER_ENUM")?;
            if choices
                .iter()
                .any(|v| !typed(v, &json!({"type":spec["type"]})))
            {
                return Err("MACRO_PARAMETER_ENUM".into());
            }
        }
        if spec.get("default").is_some_and(|v| !typed(v, spec)) {
            return Err("MACRO_PARAMETER_DEFAULT".into());
        }
        samples.insert(key.clone(), spec.get("default").cloned().unwrap_or(sample));
    }
    let steps = definition["steps"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 50)
        .ok_or("MACRO_STEPS")?;
    for step in steps {
        closed(
            step,
            &["tool", "input", "when", "error_strategy", "rollback_hint"],
        )?;
        let tool = step["tool"].as_str().ok_or("MACRO_TOOL")?;
        if tool.starts_with("macros.") || known(c, tool, active).is_none() {
            return Err("MACRO_TOOL_INVALID_OR_RECURSIVE".into());
        }
        if !step["input"].is_object() {
            return Err("MACRO_STEP_INPUT".into());
        }
        resolve(&step["input"], &samples, 0)?;
        if !templated(&step["input"]) {
            let definition = known(c, tool, active).ok_or("MACRO_TOOL_INVALID_OR_RECURSIVE")?;
            if !jsonschema::validator_for(&definition["inputSchema"])
                .map_err(|_| "MACRO_TOOL_SCHEMA")?
                .is_valid(&step["input"])
            {
                return Err("MACRO_CHILD_INPUT_INVALID".into());
            }
        }
        strategy(step.get("error_strategy"))?;
        if step
            .get("rollback_hint")
            .is_some_and(|v| v.as_str().is_none_or(|s| s.len() > 2000))
        {
            return Err("MACRO_ROLLBACK_HINT".into());
        }
        if let Some(w) = step.get("when") {
            if !parameters.contains_key(w["parameter"].as_str().ok_or("MACRO_CONDITION_PARAMETER")?)
            {
                return Err("MACRO_CONDITION_PARAMETER".into());
            }
            condition(Some(w), &samples)?;
        }
    }
    if definition
        .get("shortcut")
        .is_some_and(|v| v.as_str().is_none_or(|s| s.is_empty() || s.len() > 100))
    {
        return Err("MACRO_SHORTCUT".into());
    }
    if let Some(text) = definition["shortcut"].as_str() {
        crate::macro_shortcuts::validate(text)?;
    }
    Ok(definition)
}
fn stored(c: &Core, id: &str) -> Result<Value, String> {
    canonical(id)?;
    let rows = c.store.query(
        "SELECT id,name,definition_json,enabled FROM macros WHERE id=?",
        &[json!(id)],
    )?;
    rows.as_array()
        .filter(|a| a.len() == 1)
        .ok_or("MACRO_NOT_FOUND")?;
    Ok(rows[0].clone())
}
fn fingerprint(row: &Value) -> String {
    format!("{:x}", Sha256::digest(row.to_string().as_bytes()))
}
pub(crate) fn check_review(c: &Core, id: &str, expected: &str) -> Result<(), String> {
    if c.store.setting("privacy") == true {
        return Err("PRIVACY_DENIED".into());
    }
    if !c.enabled("macros") {
        return Err("MODULE_DISABLED: macros".into());
    }
    let row = stored(c, id)?;
    if row["enabled"] != 1 {
        return Err("MACRO_DISABLED".into());
    }
    if fingerprint(&row) != expected {
        return Err("MACRO_CHANGED_REVIEW_REQUIRED".into());
    }
    Ok(())
}
pub(crate) struct Prepared {
    id: String,
    fingerprint: String,
    definition: Value,
    steps: Vec<Value>,
    name: String,
    authority: String,
}
impl Prepared {
    pub(crate) fn review(&self) -> Value {
        policy::redact(
            &json!({"id":self.id,"name":self.name,"description":self.definition["description"],"steps":self.steps,"error_strategy":self.definition["error_strategy"],"rollback_hints_are_not_executed":true,"child_permissions":"normal Core policy; no grant inherited"}),
        )
    }
    pub(crate) fn check(&self, c: &Core) -> Result<(), String> {
        if c.store.setting("privacy") == true {
            return Err("PRIVACY_DENIED".into());
        }
        if !c.enabled("macros") {
            return Err("MODULE_DISABLED: macros".into());
        }
        if authority(c)? != self.authority {
            return Err("MACRO_AUTHORITY_CHANGED_REVIEW_REQUIRED".into());
        }
        let row = stored(c, &self.id)?;
        if row["enabled"] != 1 || fingerprint(&row) != self.fingerprint {
            return Err("MACRO_CHANGED_REVIEW_REQUIRED".into());
        }
        Ok(())
    }
    pub(crate) async fn guard<T>(
        &self,
        c: &Core,
        f: impl std::future::Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        self.check(c)?;
        tokio::pin!(f);
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(50));
        loop {
            tokio::select! { biased;
                _=tick.tick()=>self.check(c).map_err(|e|format!("MACRO_CONTINUATION_CANCELLED: {e}; an already dispatched child may have completed"))?,
                r=&mut f=>{self.check(c).map_err(|e|format!("MACRO_OUTCOME_UNCERTAIN: {e}; an already dispatched child may have completed"))?;return r;}
            }
        }
    }
    pub(crate) async fn execute(self, c: &Arc<Core>) -> Result<Value, String> {
        self.check(c)?;
        let mut results = Vec::new();
        let mut hints = Vec::new();
        let mut failed = false;
        for (index, step) in self.steps.iter().enumerate() {
            self.check(c)?;
            if step["execute"] != true {
                results.push(json!({"index":index,"status":"skipped"}));
                continue;
            }
            let tool = step["tool"].as_str().ok_or("MACRO_TOOL")?;
            if tool.starts_with("macros.") {
                return Err("MACRO_RECURSION".into());
            }
            let result = Box::pin(c.execute_macro_child(tool, step["input"].clone(), &self)).await;
            self.check(c).map_err(|e| {
                format!("MACRO_OUTCOME_UNCERTAIN: {e}; earlier steps may have completed")
            })?;
            match result {
                Ok(value) => {
                    if let Some(h) = step.get("rollback_hint") {
                        hints.push(json!({"index":index,"hint":h,"executed":false}));
                    }
                    results.push(json!({"index":index,"tool":tool,"status":"success","output":policy::redact(&value)}));
                }
                Err(error) => {
                    failed = true;
                    results.push(json!({"index":index,"tool":tool,"status":"failed","error":policy::redact(&json!(error))}));
                    if strategy(
                        step.get("error_strategy")
                            .or_else(|| self.definition.get("error_strategy")),
                    )? == "stop"
                        || boundary_error(&error)
                    {
                        break;
                    }
                }
            }
            if json!(results).to_string().len() > LIMIT {
                return Err("MACRO_RESULT_LIMIT: earlier steps may have completed".into());
            }
        }
        let output = json!({"id":self.id,"complete":!failed,"results":results,"rollback_hints":hints,"automatic_rollback":false,"automatic_replay":false});
        if output.to_string().len() > LIMIT {
            return Err("MACRO_RESULT_LIMIT: earlier steps may have completed".into());
        }
        Ok(output)
    }
}
pub(crate) fn prepare(c: &Core, input: &Value) -> Result<Prepared, String> {
    // Establish an initial durable identity before review. Checks never create
    // it: deleting/corrupting it cannot revive an already prepared declaration.
    c.store.execute(
        "INSERT OR IGNORE INTO settings(key,json_value) VALUES('macro_authority_generation',?)",
        &[json!(json!(storage::id()).to_string())],
    )?;
    let authority = authority(c)?;
    let id = input["id"].as_str().ok_or("MACRO_ID")?;
    let row = stored(c, id)?;
    if let Some(expected) = input.get("expected") {
        closed(expected, &["fingerprint"])?;
        if expected["fingerprint"].as_str() != Some(fingerprint(&row).as_str()) {
            return Err("MACRO_CHANGED_REVIEW_REQUIRED".into());
        }
    }
    if row["enabled"] != 1 {
        return Err("MACRO_DISABLED".into());
    }
    let raw = serde_json::from_str(row["definition_json"].as_str().ok_or("MACRO_DEFINITION")?)
        .map_err(|_| "MACRO_DEFINITION")?;
    let definition = normalize(c, &raw, true)?;
    let params = definition["parameters"].as_object().unwrap();
    let supplied = input.get("body").cloned().unwrap_or(json!({}));
    safe(&supplied, 0)?;
    let supplied = supplied.as_object().ok_or("MACRO_PARAMETERS")?;
    if supplied.keys().any(|k| !params.contains_key(k)) {
        return Err("MACRO_PARAMETER_UNKNOWN".into());
    }
    let mut values = BTreeMap::new();
    for (key, spec) in params {
        match supplied.get(key).or_else(|| spec.get("default")) {
            Some(v) if typed(v, spec) => {
                values.insert(key.clone(), v.clone());
            }
            Some(_) => return Err("MACRO_PARAMETER_INVALID".into()),
            None if spec["required"] == true => return Err("MACRO_PARAMETER_REQUIRED".into()),
            None => (),
        }
    }
    let mut steps = Vec::new();
    for step in definition["steps"].as_array().unwrap() {
        let mut step = step.clone();
        let execute = condition(step.get("when"), &values)?;
        if execute {
            step["input"] = resolve(&step["input"], &values, 0)?;
            let tool = c
                .tools()
                .into_iter()
                .find(|t| t["id"] == step["tool"])
                .ok_or("MACRO_TOOL_INVALID_OR_RECURSIVE")?;
            if !jsonschema::validator_for(&tool["inputSchema"])
                .map_err(|_| "MACRO_TOOL_SCHEMA")?
                .is_valid(&step["input"])
            {
                return Err("MACRO_CHILD_INPUT_INVALID".into());
            }
        }
        step["execute"] = json!(execute);
        steps.push(step);
    }
    let p = Prepared {
        id: id.into(),
        fingerprint: fingerprint(&row),
        definition,
        steps,
        name: row["name"].as_str().ok_or("MACRO_NAME")?.into(),
        authority,
    };
    p.check(c)?;
    Ok(p)
}
pub(crate) fn validate_save(c: &Core, input: &Value) -> Result<Value, String> {
    let title = input["name"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= 200)
        .ok_or("MACRO_NAME")?;
    if policy::contains_secret(title) {
        return Err("MACRO_SECRET".into());
    }
    let mut definition = json!({"version":2,"description":input.get("description").cloned().unwrap_or(json!("")),"parameters":input.get("parameters").cloned().unwrap_or(json!({})),"steps":input["steps"],"error_strategy":input.get("error_strategy").cloned().unwrap_or(json!("stop"))});
    if let Some(shortcut) = input.get("shortcut") {
        definition["shortcut"] = shortcut.clone();
    }
    let definition = normalize(c, &definition, true)?;
    Ok(
        json!({"name":title,"definition":definition,"child_permissions":"normal Core policy; no grant inherited"}),
    )
}
#[cfg(test)]
fn save(c: &Arc<Core>, input: &Value) -> Result<Value, String> {
    save_sync(c, input, || Ok(()))
}
fn save_sync(
    c: &Arc<Core>,
    input: &Value,
    check: impl Fn() -> Result<(), String>,
) -> Result<Value, String> {
    check()?;
    let review = validate_save(c, input)?;
    let title = review["name"].as_str().ok_or("MACRO_NAME")?;
    let definition = &review["definition"];
    let id = storage::id();
    let registered = if let Some(text) = definition["shortcut"].as_str() {
        crate::macro_shortcuts::register(c, &id, text)?
    } else {
        false
    };
    if let Err(error) = check() {
        if registered {
            crate::macro_shortcuts::unregister(c, &id)
                .map_err(|_| "MACRO_SAVE_SHORTCUT_OUTCOME_UNCERTAIN")?;
        }
        return Err(error);
    }
    if let Err(error) = c.store.execute(
        "INSERT INTO macros VALUES(?,?,?,1)",
        &[json!(id), json!(title), json!(definition.to_string())],
    ) {
        if registered {
            crate::macro_shortcuts::unregister(c, &id)
                .map_err(|_| "MACRO_SAVE_SHORTCUT_OUTCOME_UNCERTAIN")?;
        }
        return Err(error);
    }
    c.emit("alfredpet:macros", &json!({}));
    Ok(
        json!({"id":id,"version":2,"enabled":true,"shortcut_registered":registered,"fingerprint":fingerprint(&stored(c,&id)?)}),
    )
}
pub(crate) fn inspect(c: &Core, input: &Value) -> Result<Value, String> {
    let id = input["id"].as_str().ok_or("MACRO_ID")?;
    let row = stored(c, id)?;
    let raw = serde_json::from_str(row["definition_json"].as_str().ok_or("MACRO_DEFINITION")?)
        .map_err(|_| "MACRO_DEFINITION")?;
    Ok(policy::redact(
        &json!({"id":id,"name":row["name"],"enabled":row["enabled"]==1,"fingerprint":fingerprint(&row),"definition":normalize(c,&raw,false)?,"shortcut_registered":crate::macro_shortcuts::registered(c,id)?}),
    ))
}
pub(crate) fn list(c: &Core) -> Result<Value, String> {
    if !c.enabled("macros") || c.store.setting("privacy") == true {
        return Ok(json!([]));
    }
    Ok(policy::redact(&c.store.query(
        "SELECT id,name,enabled FROM macros ORDER BY name,id LIMIT 250",
        &[],
    )?))
}
pub(crate) fn consume_shortcut(c: &Core) -> Result<Value, String> {
    // Only metadata is persisted. Consuming it cannot execute a macro or pass a grant.
    let value = c.store.setting("macro_shortcut_pending");
    c.store
        .set_setting("macro_shortcut_pending", &json!(null))?;
    if !c.enabled("macros") || c.store.setting("privacy") == true {
        return Ok(json!(null));
    }
    let Some(at) = value["at"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
    else {
        return Ok(json!(null));
    };
    if !(0..=30_000).contains(&(chrono::Utc::now().timestamp_millis() - at.timestamp_millis())) {
        return Ok(json!(null));
    }
    let Some(id) = value["id"].as_str() else {
        return Ok(json!(null));
    };
    if stored(c, id)?["enabled"] != 1 {
        return Ok(json!(null));
    }
    Ok(json!({"id":id}))
}
fn configure_sync(
    c: &Arc<Core>,
    action: &str,
    input: &Value,
    check: impl Fn() -> Result<(), String>,
) -> Result<Value, String> {
    check()?;
    match action {
        "list" => list(c),
        "inspect" => inspect(c, input),
        "save" => save_sync(c, input, check),
        "delete" | "set_enabled" => {
            let id = input["id"].as_str().ok_or("MACRO_ID")?;
            let row = stored(c, id)?;
            if input["expected"]["fingerprint"].as_str() != Some(fingerprint(&row).as_str()) {
                return Err("MACRO_CHANGED_REVIEW_REQUIRED".into());
            }
            let enabled =
                action == "set_enabled" && input["enabled"].as_bool().ok_or("MACRO_ENABLED")?;
            let registered = if enabled {
                let definition = inspect(c, input)?["definition"].clone();
                if let Some(text) = definition["shortcut"].as_str() {
                    if crate::macro_shortcuts::registered(c, id)? {
                        true
                    } else {
                        crate::macro_shortcuts::register(c, id, text)?
                    }
                } else {
                    false
                }
            } else {
                crate::macro_shortcuts::unregister(c, id)?;
                false
            };
            check().map_err(|_| {
                "MACRO_CONFIGURATION_OUTCOME_UNCERTAIN: inspect definition and shortcut"
            })?;
            let changed = if action == "delete" {
                c.store.execute(
                    "DELETE FROM macros WHERE id=? AND name=? AND definition_json=? AND enabled=?",
                    &[
                        json!(id),
                        row["name"].clone(),
                        row["definition_json"].clone(),
                        row["enabled"].clone(),
                    ],
                )?
            } else {
                c.store.execute("UPDATE macros SET enabled=? WHERE id=? AND name=? AND definition_json=? AND enabled=?",&[json!(enabled),json!(id),row["name"].clone(),row["definition_json"].clone(),row["enabled"].clone()])?
            };
            if changed != 1 {
                return Err(
                    "MACRO_CONFIGURATION_OUTCOME_UNCERTAIN: re-inspect definition and shortcut"
                        .into(),
                );
            }
            c.emit("alfredpet:macros", &json!({}));
            Ok(
                json!({"id":id,"enabled":enabled,"deleted":action=="delete","shortcut_registered":registered}),
            )
        }
        _ => Err("MACRO_ACTION".into()),
    }
}
pub(crate) async fn dispatch(c: &Arc<Core>, action: &str, input: &Value) -> Result<Value, String> {
    match action {
        "list" => list(c),
        "inspect" => inspect(c, input),
        "run" => prepare(c, input)?.execute(c).await,
        "save" | "delete" | "set_enabled" => {
            mutation::Mutation::prepare(c, &format!("macros.{action}"), input)?
                .execute(c)
                .await
        }
        _ => Err("MACRO_ACTION".into()),
    }
}
#[path = "macro_mutation.rs"]
pub(crate) mod mutation;
#[cfg(test)]
#[path = "macro_contracts.rs"]
mod tests;
