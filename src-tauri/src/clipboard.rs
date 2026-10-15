//! Clipboard operations are observed, bounded and private. Text history requires
//! explicit versioned consent; ordinary clipboard text is omitted from durable
//! activity and permission audit. Tool callers can receive untrusted text.
use crate::{policy, runtime::Core, storage};
use chrono::{Duration as ChronoDuration, Utc};
use rusqlite::{params, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, sync::Arc};
use tokio_util::sync::CancellationToken;
pub(crate) const MAX_CHARS: usize = 64000;
pub(crate) const MAX_BYTES: usize = 256 * 1024;
#[cfg(test)]
pub(crate) const TEST_REMOVE_FUTURE_SCHEMA:&str="DROP TABLE chat_progress;DROP TABLE drive_jobs;DROP TRIGGER clipboard_setting_insert;DROP TRIGGER clipboard_setting_update;DROP TRIGGER clipboard_setting_delete;DROP TRIGGER clipboard_module_update;DROP TABLE clipboard_history;DROP TABLE clipboard_state;";
const HISTORY_BYTES: i64 = 2 * 1024 * 1024;
pub(crate) const OPERATIONS: [&str; 8] = [
    "trim",
    "uppercase",
    "lowercase",
    "normalize_whitespace",
    "unique_lines",
    "sort_lines",
    "pretty_json",
    "minify_json",
];
pub(crate) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Expected {
    pub sequence: u32,
    pub fingerprint: String,
}
#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    pub sequence: u32,
    pub formats: Vec<u32>,
    pub text: Option<String>,
    pub image: bool,
    pub excluded: bool,
}
impl Snapshot {
    pub fn expected(&self) -> Expected {
        Expected {
            sequence: self.sequence,
            fingerprint: hash(
                json!([
                    self.sequence,
                    self.formats,
                    self.text.as_ref().map(|t| hash(t.as_bytes())),
                    self.image,
                    self.excluded
                ])
                .to_string()
                .as_bytes(),
            ),
        }
    }
    fn data(&self, include_text: bool) -> Result<Value, String> {
        let sensitive = self.excluded || self.text.as_ref().is_some_and(|text| sensitive(text));
        if include_text && sensitive {
            return Err("SENSITIVE_CLIPBOARD_DENIED".into());
        }
        let kind = if sensitive {
            "sensitive"
        } else {
            match &self.text {
                Some(text) if !text.is_empty() => detect(text),
                _ if self.image => "image",
                _ if self.formats.is_empty() => "empty",
                _ => "other",
            }
        };
        let mut data = json!({"expected":self.expected(),"formats":self.formats.iter().map(|id|json!({"id":id,"name":format_name(*id)})).collect::<Vec<_>>(),"kind":kind,"has_image":self.image,"untrusted_content":true});
        data["sensitive"] = json!(sensitive);
        if include_text {
            data["text"] = json!(self.text.clone().unwrap_or_default());
        }
        Ok(data)
    }
}
fn format_name(id: u32) -> String {
    match id {
        1 => "Text",
        2 => "Bitmap",
        7 => "OEM text",
        8 => "DIB image",
        13 => "Unicode text",
        15 => "File list",
        17 => "DIB V5 image",
        _ => "Other format",
    }
    .into()
}
pub(crate) fn text_valid(text: &str) -> Result<(), String> {
    if text.len() > MAX_BYTES || text.chars().count() > MAX_CHARS {
        return Err("CLIPBOARD_TEXT_LIMIT".into());
    }
    if text.contains('\0') {
        return Err("CLIPBOARD_TEXT_NUL_DENIED".into());
    }
    Ok(())
}
pub(crate) fn sensitive(text: &str) -> bool {
    if policy::contains_secret(text) {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    if [
        "authorization:",
        "cookie:",
        "set-cookie:",
        "private key",
        "aws_secret_access_key",
    ]
    .iter()
    .any(|key| lower.contains(key))
    {
        return true;
    }
    if text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| {
            token.len() >= 20
                && (token.starts_with("AKIA") || token.starts_with("ASIA"))
                && token
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        })
    {
        return true;
    }
    let trimmed = text.trim();
    if trimmed.len() >= 32
        && !trimmed.chars().any(char::is_whitespace)
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-+/=".contains(c))
    {
        return true;
    }
    if let Ok(url) = reqwest::Url::parse(trimmed) {
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query_pairs().any(|(key, _)| {
                let key = key.to_ascii_lowercase();
                [
                    "password", "passwd", "secret", "token", "key", "auth", "session", "cookie",
                    "csrf", "otp",
                ]
                .iter()
                .any(|part| key.contains(part))
                    || ["sid", "code"].contains(&key.as_str())
            })
        {
            return true;
        }
    }
    false
}
fn detect(text: &str) -> &'static str {
    let trimmed = text.trim();
    if reqwest::Url::parse(trimmed)
        .is_ok_and(|u| ["http", "https"].contains(&u.scheme()) && u.host().is_some())
    {
        return "url";
    }
    if serde_json::from_str::<serde::de::IgnoredAny>(trimmed).is_ok()
        && (trimmed.starts_with('{') || trimmed.starts_with('['))
    {
        return "code";
    }
    if trimmed.contains('\n')
        && [
            "function ",
            "fn ",
            "def ",
            "import ",
            "class ",
            "SELECT ",
            "const ",
            "#!/",
        ]
        .iter()
        .any(|start| trimmed.contains(start))
    {
        return "code";
    }
    "text"
}
pub(crate) fn transform(text: &str, operation: &str) -> Result<String, String> {
    text_valid(text)?;
    if sensitive(text) {
        return Err("SENSITIVE_CLIPBOARD_DENIED".into());
    }
    let result = match operation {
        "trim" => text.trim().to_owned(),
        "uppercase" => text.to_uppercase(),
        "lowercase" => text.to_lowercase(),
        "normalize_whitespace" => text.split_whitespace().collect::<Vec<_>>().join(" "),
        "unique_lines" => {
            let mut seen = HashSet::new();
            text.lines()
                .filter(|line| seen.insert(*line))
                .collect::<Vec<_>>()
                .join("\n")
        }
        "sort_lines" => {
            let mut lines = text.lines().collect::<Vec<_>>();
            lines.sort_unstable();
            lines.join("\n")
        }
        "pretty_json" | "minify_json" => {
            serde_json::from_str::<serde::de::IgnoredAny>(text)
                .map_err(|_| "CLIPBOARD_JSON_INVALID")?;
            // Format lexically after validation: preserve duplicate keys, number lexemes
            // and string escapes instead of round-tripping through floating point Value.
            let mut out = String::new();
            let mut quoted = false;
            let mut escaped = false;
            let mut depth = 0usize;
            let mut chars = text.chars().peekable();
            while let Some(ch) = chars.next() {
                if quoted {
                    out.push(ch);
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == '"' {
                        quoted = false;
                    }
                    continue;
                }
                if ch.is_whitespace() {
                    continue;
                }
                if ch == '"' {
                    quoted = true;
                    out.push(ch);
                    continue;
                }
                if operation == "minify_json" {
                    out.push(ch);
                    continue;
                }
                match ch {
                    '{' | '[' => {
                        out.push(ch);
                        depth += 1;
                        let next = chars.clone().find(|c| !c.is_whitespace());
                        if next != Some(if ch == '{' { '}' } else { ']' }) {
                            out.push('\n');
                            out.push_str(&"  ".repeat(depth));
                        }
                    }
                    '}' | ']' => {
                        depth = depth.checked_sub(1).ok_or("CLIPBOARD_JSON_INVALID")?;
                        if !out.ends_with(['{', '[']) {
                            out.push('\n');
                            out.push_str(&"  ".repeat(depth));
                        }
                        out.push(ch);
                    }
                    ',' => {
                        out.push_str(",\n");
                        out.push_str(&"  ".repeat(depth));
                    }
                    ':' => out.push_str(": "),
                    _ => out.push(ch),
                }
            }
            out
        }
        _ => return Err("CLIPBOARD_TRANSFORM_UNSUPPORTED".into()),
    };
    text_valid(&result)?;
    if sensitive(&result) {
        return Err("SENSITIVE_CLIPBOARD_DENIED".into());
    }
    Ok(result)
}

#[derive(Clone)]
pub(crate) struct Control {
    pub core: Arc<Core>,
    pub cancel: CancellationToken,
    pub generation: i64,
}
impl Control {
    pub(crate) fn new(core: &Arc<Core>, cancel: CancellationToken) -> Result<Self, String> {
        let generation = core
            .store
            .query("SELECT generation FROM clipboard_state WHERE id=1", &[])?[0]["generation"]
            .as_i64()
            .ok_or("CLIPBOARD_STATE")?;
        let control = Self {
            core: core.clone(),
            cancel,
            generation,
        };
        control.check()?;
        Ok(control)
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        if self.cancel.is_cancelled() {
            return Err("CLIPBOARD_CANCELLED".into());
        }
        if !self.core.enabled("clipboard") {
            return Err("TOOL_DISABLED_OR_UNKNOWN".into());
        }
        if self.core.store.setting("privacy") == true {
            return Err("PRIVACY_DENIED".into());
        }
        if self
            .core
            .store
            .query("SELECT generation FROM clipboard_state WHERE id=1", &[])?[0]["generation"]
            != self.generation
        {
            return Err("CLIPBOARD_SCOPE_CHANGED".into());
        }
        Ok(())
    }
}
pub(crate) trait Adapter: Send + Sync {
    fn sequence(&self) -> Result<u32, String>;
    fn read(&self, control: &Control) -> Result<Snapshot, String>;
    fn write(&self, control: &Control, expected: &Expected, text: &str)
        -> Result<Snapshot, String>;
    fn image(&self, control: &Control, expected: &Expected) -> Result<Vec<u8>, String>;
}
struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
pub(crate) async fn read(core: &Arc<Core>) -> Result<Snapshot, String> {
    let control = Control::new(core, CancellationToken::new())?;
    let _drop = CancelOnDrop(control.cancel.clone());
    tokio::task::spawn_blocking(move || crate::clipboard_native::Native.read(&control))
        .await
        .map_err(|_| "CLIPBOARD_WORKER")?
}
pub(crate) async fn image(
    core: &Arc<Core>,
    input: &Value,
    cancel: CancellationToken,
) -> Result<Vec<u8>, String> {
    let expected: Expected =
        serde_json::from_value(input["expected"].clone()).map_err(|_| "CLIPBOARD_EXPECTED")?;
    let control = Control::new(core, cancel.child_token())?;
    let _drop = CancelOnDrop(control.cancel.clone());
    if input
        .get("_clipboard_generation")
        .is_some_and(|generation| *generation != control.generation)
    {
        return Err("CLIPBOARD_SCOPE_CHANGED".into());
    }
    tokio::task::spawn_blocking(move || crate::clipboard_native::Native.image(&control, &expected))
        .await
        .map_err(|_| "CLIPBOARD_WORKER")?
}
pub(crate) async fn review(core: &Arc<Core>, tool: &str, input: &Value) -> Result<Value, String> {
    if !["clipboard.set", "clipboard.transform", "clipboard.image"].contains(&tool) {
        return Ok(input.clone());
    }
    if tool == "clipboard.transform" && input["apply"] != true {
        return Ok(input.clone());
    }
    let snapshot = read(core).await?;
    let expected: Expected =
        serde_json::from_value(input["expected"].clone()).map_err(|_| "CLIPBOARD_EXPECTED")?;
    if expected != snapshot.expected() {
        return Err("CLIPBOARD_CHANGED_REINSPECT".into());
    }
    snapshot.data(false)?;
    let generation = Control::new(core, CancellationToken::new())?.generation;
    if tool == "clipboard.image" {
        if snapshot.excluded || snapshot.text.as_ref().is_some_and(|t| sensitive(t)) {
            return Err("SENSITIVE_CLIPBOARD_DENIED".into());
        }
        if !snapshot.image {
            return Err("CLIPBOARD_IMAGE_UNAVAILABLE".into());
        }
        return Ok(
            json!({"expected":expected,"formats":snapshot.formats,"scope_generation":generation,"effect":"Copy the observed clipboard image to the private asset registry; no automatic image history"}),
        );
    }
    let replacement = if tool == "clipboard.set" {
        input["text"]
            .as_str()
            .ok_or("CLIPBOARD_TEXT_REQUIRED")?
            .to_owned()
    } else {
        transform(
            snapshot.text.as_deref().ok_or("CLIPBOARD_TEXT_REQUIRED")?,
            input["operation"]
                .as_str()
                .ok_or("CLIPBOARD_TRANSFORM_REQUIRED")?,
        )?
    };
    text_valid(&replacement)?;
    if sensitive(&replacement) {
        return Err("SENSITIVE_CLIPBOARD_DENIED".into());
    }
    Ok(
        json!({"expected":expected,"text":replacement,"characters":replacement.chars().count(),"sha256":hash(replacement.as_bytes()),"scope_generation":generation,"discarded_formats":snapshot.formats,"effect":"Replace current clipboard formats with Unicode text; original clipboard is not a durable undo"}),
    )
}
pub(crate) async fn dispatch(core: &Arc<Core>, tool: &str, input: &Value) -> Result<Value, String> {
    if tool == "clipboard.history" {
        return history(core, input);
    }
    if tool == "clipboard.history_delete" || tool == "clipboard.history_clear" {
        return delete_history(core, tool, input);
    }
    if tool == "clipboard.image" {
        return crate::assets::capture(core, tool, input).await;
    }
    let control = Control::new(core, CancellationToken::new())?;
    let _drop = CancelOnDrop(control.cancel.clone());
    if input
        .get("_clipboard_generation")
        .is_some_and(|generation| *generation != control.generation)
    {
        return Err("CLIPBOARD_SCOPE_CHANGED".into());
    }
    let input = input.clone();
    let tool = tool.to_owned();
    tokio::task::spawn_blocking(move || {
        dispatch_with(&control, &crate::clipboard_native::Native, &tool, &input)
    })
    .await
    .map_err(|_| "CLIPBOARD_WORKER")?
}
fn dispatch_with(
    control: &Control,
    adapter: &dyn Adapter,
    tool: &str,
    input: &Value,
) -> Result<Value, String> {
    control.check()?;
    let snapshot = adapter.read(control)?;
    snapshot.data(false)?;
    let output = match tool {
        "clipboard.get" => snapshot.data(true)?,
        "clipboard.formats" | "clipboard.detect" => snapshot.data(false)?,
        "clipboard.set" | "clipboard.transform" => {
            let expected: Expected = serde_json::from_value(input["expected"].clone())
                .map_err(|_| "CLIPBOARD_EXPECTED")?;
            if expected != snapshot.expected() {
                return Err("CLIPBOARD_CHANGED_REINSPECT".into());
            }
            let replacement = if tool == "clipboard.set" {
                input["text"]
                    .as_str()
                    .ok_or("CLIPBOARD_TEXT_REQUIRED")?
                    .to_owned()
            } else {
                transform(
                    snapshot.text.as_deref().ok_or("CLIPBOARD_TEXT_REQUIRED")?,
                    input["operation"]
                        .as_str()
                        .ok_or("CLIPBOARD_TRANSFORM_REQUIRED")?,
                )?
            };
            text_valid(&replacement)?;
            if sensitive(&replacement) {
                return Err("SENSITIVE_CLIPBOARD_DENIED".into());
            }
            if tool == "clipboard.transform" && input["apply"] != true {
                json!({"text":replacement,"expected":expected,"preview_only":true,"untrusted_content":true})
            } else {
                control.check()?;
                let written = adapter.write(control, &expected, &replacement)?;
                if written.text.as_deref() != Some(&replacement) {
                    return Err("CLIPBOARD_WRITE_READBACK_FAILED".into());
                }
                let mut data = written.data(false)?;
                data["written"] = json!(true);
                data
            }
        }
        _ => return Err("TOOL_UNKNOWN".into()),
    };
    control.check()?;
    Ok(output)
}

fn history_enabled(db: &rusqlite::Connection) -> Result<bool, String> {
    db.query_row("SELECT EXISTS(SELECT 1 FROM settings WHERE key='clipboard_history' AND json_value='true') AND EXISTS(SELECT 1 FROM settings WHERE key='clipboard_history_consent' AND json_value='1')",[],|r|r.get(0)).map_err(|_|"CLIPBOARD_HISTORY_STATE".into())
}
pub(crate) fn set_history(core: &Arc<Core>, value: bool) -> Result<Value, String> {
    let mut db = core.store.0.lock().map_err(|_| "DB_LOCK")?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| "DB_TRANSACTION")?;
    for (key, value) in [
        ("clipboard_history", json!(value)),
        ("clipboard_history_consent", json!(1)),
    ] {
        tx.execute("INSERT INTO settings(key,json_value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET json_value=excluded.json_value",params![key,value.to_string()]).map_err(|_|"CLIPBOARD_HISTORY_SETTING")?;
    }
    tx.commit().map_err(|_| "DB_COMMIT")?;
    core.emit("alfredpet:clipboard", &json!({}));
    Ok(json!({"enabled":value}))
}
fn prune(db: &rusqlite::Connection, now: &str) -> Result<(), String> {
    db.execute("DELETE FROM clipboard_history WHERE expires_at<=?", [now])
        .map_err(|_| "CLIPBOARD_HISTORY_PRUNE")?;
    loop {
        let (count,bytes):(i64,i64)=db.query_row("SELECT count(*),COALESCE(sum(length(CAST(text AS BLOB))),0) FROM clipboard_history",[],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|_|"CLIPBOARD_HISTORY_QUOTA")?;
        if count <= 100 && bytes <= HISTORY_BYTES {
            break;
        }
        db.execute("DELETE FROM clipboard_history WHERE id=(SELECT id FROM clipboard_history ORDER BY updated_at,id LIMIT 1)",[]).map_err(|_|"CLIPBOARD_HISTORY_PRUNE")?;
    }
    Ok(())
}
pub(crate) fn status(core: &Arc<Core>) -> Result<Value, String> {
    let db = core.store.0.lock().map_err(|_| "DB_LOCK")?;
    let enabled = history_enabled(&db)?;
    let generation: i64 = db
        .query_row(
            "SELECT generation FROM clipboard_state WHERE id=1",
            [],
            |r| r.get(0),
        )
        .map_err(|_| "CLIPBOARD_STATE")?;
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM clipboard_history WHERE expires_at>?",
            [storage::now()],
            |r| r.get(0),
        )
        .map_err(|_| "CLIPBOARD_HISTORY_COUNT")?;
    drop(db);
    Ok(
        json!({"enabled":enabled,"scope_generation":generation,"module_enabled":core.enabled("clipboard"),"privacy":core.store.setting("privacy")==true,"entries":count,"maximum_entries":100,"retention_days":7,"history_kind":"text_only","last_error":core.store.setting("clipboard_last_error")}),
    )
}
fn history(core: &Arc<Core>, input: &Value) -> Result<Value, String> {
    let control = Control::new(core, CancellationToken::new())?;
    control.check()?;
    let mut db = core.store.0.lock().map_err(|_| "DB_LOCK")?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| "DB_TRANSACTION")?;
    if !history_enabled(&tx)? {
        return Err("CLIPBOARD_HISTORY_DISABLED".into());
    }
    prune(&tx, &storage::now())?;
    let limit = input["limit"].as_u64().unwrap_or(20).min(50) as i64;
    let offset = input["offset"].as_u64().unwrap_or(0).min(1000) as i64;
    let rows = {
        let mut stmt=tx.prepare("SELECT id,text,sha256,updated_at,expires_at FROM clipboard_history ORDER BY updated_at DESC,id LIMIT ? OFFSET ?").map_err(|_|"CLIPBOARD_HISTORY_QUERY")?;
        let rows=stmt.query_map(params![limit,offset],|r|Ok(json!({"id":r.get::<_,String>(0)?,"text":r.get::<_,String>(1)?,"sha256":r.get::<_,String>(2)?,"updated_at":r.get::<_,String>(3)?,"expires_at":r.get::<_,String>(4)?}))).map_err(|_|"CLIPBOARD_HISTORY_QUERY")?.collect::<Result<Vec<_>,_>>().map_err(|_|"CLIPBOARD_HISTORY_QUERY")?;
        rows
    };
    tx.commit().map_err(|_| "DB_COMMIT")?;
    drop(db);
    control.check()?;
    Ok(json!({"items":rows,"offset":offset,"untrusted_content":true}))
}
fn delete_history(core: &Arc<Core>, tool: &str, input: &Value) -> Result<Value, String> {
    let mut db = core.store.0.lock().map_err(|_| "DB_LOCK")?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| "DB_TRANSACTION")?;
    let count = if tool == "clipboard.history_clear" {
        tx.execute("DELETE FROM clipboard_history", [])
    } else {
        tx.execute(
            "DELETE FROM clipboard_history WHERE id=?",
            [input["id"].as_str().ok_or("CLIPBOARD_HISTORY_ID")?],
        )
    }
    .map_err(|_| "CLIPBOARD_HISTORY_DELETE")?;
    // Invalidate in-flight reads so clearing cannot be followed by a late reinsert.
    tx.execute(
        "UPDATE clipboard_state SET generation=generation+1 WHERE id=1",
        [],
    )
    .map_err(|_| "CLIPBOARD_STATE")?;
    tx.commit().map_err(|_| "DB_COMMIT")?;
    core.emit("alfredpet:clipboard", &json!({}));
    Ok(json!({"deleted":count}))
}
#[derive(Default)]
pub(crate) struct Observer {
    generation: Option<i64>,
    sequence: Option<u32>,
}
pub(crate) fn observe(
    core: &Arc<Core>,
    adapter: &dyn Adapter,
    state: &mut Observer,
) -> Result<bool, String> {
    // Expiry maintenance reads no native clipboard and continues while opt-in is off.
    {
        let db = core.store.0.lock().map_err(|_| "DB_LOCK")?;
        prune(&db, &storage::now())?;
    }
    let enabled = {
        let db = core.store.0.lock().map_err(|_| "DB_LOCK")?;
        history_enabled(&db)?
    };
    if !enabled || !core.enabled("clipboard") || core.store.setting("privacy") == true {
        state.generation = None;
        state.sequence = None;
        return Ok(false);
    }
    let control = Control::new(core, CancellationToken::new())?;
    let sequence = adapter.sequence()?;
    if state.generation != Some(control.generation) {
        state.generation = Some(control.generation);
        state.sequence = Some(sequence);
        return Ok(false);
    }
    if state.sequence == Some(sequence) {
        return Ok(false);
    }
    let snapshot = adapter.read(&control)?;
    control.check()?;
    if snapshot.sequence != sequence {
        state.sequence = None;
        return Err("CLIPBOARD_CHANGED_RETRY".into());
    }
    if snapshot.excluded {
        state.sequence = Some(sequence);
        return Ok(false);
    }
    let Some(text) = snapshot.text.as_deref().filter(|text| !text.is_empty()) else {
        state.sequence = Some(sequence);
        return Ok(false);
    };
    text_valid(text)?;
    if sensitive(text) {
        state.sequence = Some(sequence);
        return Ok(false);
    }
    let now = Utc::now();
    let expires = (now + ChronoDuration::days(7)).to_rfc3339();
    let now = now.to_rfc3339();
    let mut db = core.store.0.lock().map_err(|_| "DB_LOCK")?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| "DB_TRANSACTION")?;
    let valid:bool=tx.query_row("SELECT generation=? AND NOT EXISTS(SELECT 1 FROM settings WHERE key='privacy' AND json_value='true') AND EXISTS(SELECT 1 FROM module_configs WHERE module_id='clipboard' AND enabled=1) FROM clipboard_state WHERE id=1",[control.generation],|r|r.get(0)).map_err(|_|"CLIPBOARD_STATE")?;
    if !valid || !history_enabled(&tx)? || control.cancel.is_cancelled() {
        return Err("CLIPBOARD_SCOPE_CHANGED".into());
    }
    tx.execute("INSERT INTO clipboard_history(id,sha256,text,sequence,created_at,updated_at,expires_at) VALUES(?,?,?,?,?,?,?) ON CONFLICT(sha256) DO UPDATE SET sequence=excluded.sequence,updated_at=excluded.updated_at,expires_at=excluded.expires_at",params![storage::id(),hash(text.as_bytes()),text,snapshot.sequence,now,now,expires]).map_err(|_|"CLIPBOARD_HISTORY_INSERT")?;
    prune(&tx, &now)?;
    tx.commit().map_err(|_| "DB_COMMIT")?;
    drop(db);
    state.sequence = Some(sequence);
    core.emit("alfredpet:clipboard", &json!({}));
    Ok(true)
}
pub(crate) async fn maintain(core: Arc<Core>) {
    let mut state = Observer::default();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let c = core.clone();
        let result = tokio::task::spawn_blocking(move || {
            let result = observe(&c, &crate::clipboard_native::Native, &mut state);
            (state, result)
        })
        .await;
        match result {
            Ok((next, result)) => {
                state = next;
                let error = result.err().unwrap_or_default();
                if core.store.setting("clipboard_last_error") != error {
                    let _ = core
                        .store
                        .set_setting("clipboard_last_error", &json!(error));
                    core.emit("alfredpet:clipboard", &json!({}));
                }
            }
            Err(_) => {
                let _ = core
                    .store
                    .set_setting("clipboard_last_error", &json!("CLIPBOARD_WORKER"));
                state = Observer::default();
            }
        }
    }
}
#[cfg(all(test, windows))]
#[path = "clipboard_tests.rs"]
mod tests;
