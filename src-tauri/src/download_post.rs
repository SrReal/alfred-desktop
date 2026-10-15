//! Reusable, manually invoked post-actions reuse reviewed personal macros.
use crate::{asset_files, macros, policy::Risk, runtime::Core};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs::File, path::PathBuf, sync::Arc};
pub const TOOLS: &[&str] = &[
    "downloads.post_actions_list",
    "downloads.post_actions_inspect",
    "downloads.post_actions_save",
    "downloads.post_actions_run",
];
const ALLOWED: &[&str] = &[
    "downloads.hash",
    "downloads.classify",
    "files.copy",
    "files.extract",
    "documents.metadata",
    "documents.extract_text",
    "images.metadata",
    "images.screenshot_prep",
];
const PREFIX: &str = "download_post_actions.";
pub fn risk(tool: &str) -> Risk {
    if [
        "downloads.post_actions_list",
        "downloads.post_actions_inspect",
    ]
    .contains(&tool)
    {
        Risk::Free
    } else {
        Risk::Mandatory
    }
}
pub fn schema(tool: &str) -> Value {
    let id = json!({"type":"string","format":"uuid","minLength":36,"maxLength":36});
    let (required, properties) = match tool {
        "downloads.post_actions_list" => (vec![], json!({})),
        "downloads.post_actions_inspect" => (vec!["id"], json!({"id":id})),
        "downloads.post_actions_save" => (
            vec!["name", "steps"],
            json!({"name":{"type":"string","minLength":1,"maxLength":200},"description":{"type":"string","maxLength":2000},"steps":{"type":"array","minItems":1,"maxItems":10,"items":{"type":"object","required":["tool","input"],"properties":{"tool":{"enum":ALLOWED},"input":{"type":"object","maxProperties":16}},"additionalProperties":false}}}),
        ),
        "downloads.post_actions_run" => (
            vec!["id", "path", "expected"],
            json!({"id":id,"path":{"type":"string","minLength":1,"maxLength":4000},"expected":{"type":"object","required":["fingerprint"],"properties":{"fingerprint":{"type":"string","pattern":"^[a-f0-9]{64}$"}},"additionalProperties":false}}),
        ),
        _ => return json!({"not":{}}),
    };
    json!({"type":"object","required":required,"properties":properties,"additionalProperties":false})
}
fn key(id: &str) -> Result<String, String> {
    let u = uuid::Uuid::parse_str(id).map_err(|_| "POST_ACTION_ID")?;
    if u.is_nil() || u.to_string() != id {
        return Err("POST_ACTION_ID".into());
    }
    Ok(format!("{PREFIX}{id}"))
}
fn inspect(c: &Core, id: &str) -> Result<Value, String> {
    let registered = c.store.setting(&key(id)?);
    if registered["id"] != id {
        return Err("POST_ACTION_PRESET_NOT_FOUND".into());
    }
    let mut row = macros::inspect(c, &json!({"id":id}))?;
    row["registered_fingerprint"] = registered["fingerprint"].clone();
    row["auto_execute"] = json!(false);
    Ok(row)
}
fn declaration(c: &Core, input: &Value) -> Result<Value, String> {
    let mut steps = input["steps"]
        .as_array()
        .ok_or("POST_ACTION_STEPS")?
        .clone();
    for step in &mut steps {
        let tool = step["tool"].as_str().ok_or("POST_ACTION_TOOL")?.to_owned();
        if !ALLOWED.contains(&tool.as_str()) {
            return Err("POST_ACTION_TOOL_NOT_ALLOWED".into());
        }
        let child = step["input"].as_object_mut().ok_or("POST_ACTION_INPUT")?;
        if child.contains_key("path") {
            return Err("POST_ACTION_PATH_IS_REVIEWED_AT_RUN".into());
        }
        child.insert("path".into(), json!({"$parameter":"download_path"}));
    }
    let v = json!({"name":input["name"],"description":input.get("description").cloned().unwrap_or(json!("Reviewed manual download post-actions")),"parameters":{"download_path":{"type":"string","required":true,"description":"Exact approved completed download path"}},"steps":steps,"error_strategy":"stop"});
    macros::validate_save(c, &v)?;
    Ok(v)
}
struct Source {
    path: PathBuf,
    file: File,
    identity: String,
    sha: String,
    _parents: Vec<File>,
}
pub struct Prepared {
    tool: String,
    input: Value,
    record: Value,
    source: Option<Source>,
    roots: Value,
    authority: Value,
}
impl Prepared {
    pub fn prepare(c: &Core, tool: &str, input: &Value) -> Result<Self, String> {
        if !TOOLS.contains(&tool)
            || !jsonschema::validator_for(&schema(tool))
                .map_err(|_| "POST_ACTION_SCHEMA")?
                .is_valid(input)
        {
            return Err("INVALID_TOOL_INPUT".into());
        }
        if !c.enabled("downloads") || c.store.setting("privacy") == true {
            return Err("POST_ACTION_SCOPE_REVOKED".into());
        }
        let record = if tool == "downloads.post_actions_save" {
            declaration(c, input)?
        } else if let Some(id) = input["id"].as_str() {
            inspect(c, id)?
        } else {
            Value::Null
        };
        let source = if tool == "downloads.post_actions_run" {
            if record["fingerprint"] != input["expected"]["fingerprint"]
                || record["fingerprint"] != record["registered_fingerprint"]
            {
                return Err("POST_ACTION_PRESET_CHANGED".into());
            }
            if record["enabled"] != true {
                return Err("POST_ACTION_PRESET_DISABLED".into());
            }
            let path = c.path(input["path"].as_str().unwrap(), false)?;
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            if [".part", ".partial", ".tmp", ".crdownload", ".download"]
                .iter()
                .any(|e| name.ends_with(e))
            {
                return Err("DOWNLOAD_NOT_READY".into());
            }
            let parents = asset_files::parents(c, &path)?;
            let file = asset_files::open(&path, false)?;
            let bytes = asset_files::read(&file, 32 * 1024 * 1024)?;
            if bytes.is_empty() {
                return Err("DOWNLOAD_NOT_READY".into());
            }
            let identity = asset_files::identity(&file)?;
            let sha = format!("{:x}", Sha256::digest(&bytes));
            Some(Source {
                path,
                file,
                identity,
                sha,
                _parents: parents,
            })
        } else {
            None
        };
        let p = Self {
            tool: tool.into(),
            input: input.clone(),
            record,
            source,
            roots: c.store.setting("allowed_roots"),
            authority: crate::media::authority(c)?,
        };
        p.check(c)?;
        Ok(p)
    }
    pub fn review(&self) -> Value {
        json!({"operation":self.tool,"preset":self.record,"path":self.source.as_ref().map(|s|&s.path),"sha256":self.source.as_ref().map(|s|&s.sha),"auto_execute":false,"preserve_download":true,"child_permissions":"Every macro and child uses normal Core policy; no permission inherited","actions":"Hash/classify/copy/extract/local document-image reads only"})
    }
    pub fn check(&self, c: &Core) -> Result<(), String> {
        if !c.enabled("downloads")
            || c.store.setting("privacy") == true
            || c.store.setting("allowed_roots") != self.roots
            || crate::media::authority(c)? != self.authority
        {
            return Err("POST_ACTION_SCOPE_REVOKED".into());
        }
        if let Some(id) = self.input["id"].as_str() {
            if inspect(c, id)? != self.record {
                return Err("POST_ACTION_PRESET_CHANGED".into());
            }
        }
        if let Some(s) = &self.source {
            if asset_files::identity(&s.file)? != s.identity
                || c.path(s.path.to_str().ok_or("POST_ACTION_PATH")?, false)? != s.path
            {
                return Err("POST_ACTION_SOURCE_CHANGED".into());
            }
        }
        Ok(())
    }
    pub async fn execute(self, c: &Arc<Core>) -> Result<Value, String> {
        self.check(c)?;
        match self.tool.as_str() {
            "downloads.post_actions_list" => {
                let rows=c.store.query("SELECT json_value FROM settings WHERE key LIKE 'download_post_actions.%' ORDER BY key LIMIT 50",&[])?;
                let mut out = Vec::new();
                for row in rows.as_array().ok_or("POST_ACTION_ROWS")? {
                    let r: Value = serde_json::from_str(
                        row["json_value"]
                            .as_str()
                            .ok_or("POST_ACTION_REGISTRATION")?,
                    )
                    .map_err(|_| "POST_ACTION_REGISTRATION")?;
                    let id = r["id"].as_str().ok_or("POST_ACTION_REGISTRATION")?;
                    out.push(inspect(c, id).unwrap_or_else(
                        |e| json!({"id":id,"health":"ERROR","error_code":e,"auto_execute":false}),
                    ));
                }
                Ok(json!(out))
            }
            "downloads.post_actions_inspect" => Ok(self.record),
            "downloads.post_actions_save" => {
                if c.store.query(
                    "SELECT COUNT(*) AS n FROM settings WHERE key LIKE 'download_post_actions.%'",
                    &[],
                )?[0]["n"]
                    .as_u64()
                    .unwrap_or(50)
                    >= 50
                {
                    return Err("POST_ACTION_PRESET_LIMIT".into());
                }
                let operation = Box::pin(c.execute("macros.save", self.record.clone()));
                tokio::pin!(operation);
                let mut timer = tokio::time::interval(std::time::Duration::from_millis(100));
                let result = loop {
                    tokio::select! {r=&mut operation=>break r?,_=timer.tick()=>{self.check(c).map_err(|e|format!("POST_ACTION_REGISTRATION_UNCERTAIN:{e}: inspect Macros before retry"))?;}}
                };
                self.check(c).map_err(|_| {
                    format!(
                        "POST_ACTION_REGISTRATION_UNCERTAIN: macro {} may remain; inspect Macros",
                        result["id"]
                    )
                })?;
                let id = result["id"].as_str().ok_or("POST_ACTION_SAVE_RESULT")?;
                let registered=c.store.execute("INSERT INTO settings(key,json_value) SELECT ?,? WHERE (SELECT COUNT(*) FROM settings WHERE key LIKE 'download_post_actions.%')<50",&[json!(key(id)?),json!(json!({"id":id,"fingerprint":result["fingerprint"]}).to_string())]).map_err(|e|format!("POST_ACTION_REGISTRATION_UNCERTAIN: macro {id} may remain: {e}"))?;
                if registered != 1 {
                    return Err(format!("POST_ACTION_REGISTRATION_UNCERTAIN: preset limit reached; macro {id} remains; inspect Macros"));
                }
                c.emit("alfredpet:download_post_actions", &json!({}));
                let mut result = result;
                result["auto_execute"] = json!(false);
                Ok(result)
            }
            "downloads.post_actions_run" => {
                let source = self.source.as_ref().ok_or("POST_ACTION_SOURCE")?;
                let operation=Box::pin(c.execute("macros.run",json!({"id":self.input["id"],"expected":self.input["expected"],"body":{"download_path":source.path}})));
                tokio::pin!(operation);
                let mut timer = tokio::time::interval(std::time::Duration::from_millis(100));
                loop {
                    tokio::select! {r=&mut operation=>return r.map(|mut r|{r["auto_execute"]=json!(false);r}),_=timer.tick()=>{self.check(c)?;}}
                }
            }
            _ => Err("POST_ACTION_TOOL".into()),
        }
    }
}
#[cfg(all(test, windows))]
#[path = "download_post_contracts.rs"]
mod tests;
