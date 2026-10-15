use crate::policy::contains_secret;
use chrono::Utc;
use rusqlite::{params, types::ValueRef, Connection};
use serde_json::{json, Value};
use std::{path::Path, sync::Mutex};
use uuid::Uuid;
pub struct Store(pub Mutex<Connection>);
pub const SCHEMA_VERSION: i64 = 13;
pub fn now() -> String {
    Utc::now().to_rfc3339()
}
pub fn id() -> String {
    Uuid::new_v4().to_string()
}
impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        let mut db = Connection::open(path).map_err(|e| e.to_string())?;
        db.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| e.to_string())?;
        db.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| e.to_string())?;
        db.busy_timeout(std::time::Duration::from_secs(3))
            .map_err(|e| e.to_string())?;
        let version: i64 = db
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if version > SCHEMA_VERSION {
            return Err("DB_NEWER_THAN_APP".into());
        }
        if version < 1 {
            if path.exists()
                && std::fs::metadata(path)
                    .map(|m| m.len() > 0)
                    .unwrap_or(false)
            {
                let backup =
                    path.with_extension(format!("pre-migration-{}.db", Utc::now().timestamp()));
                db.backup(rusqlite::MAIN_DB, &backup, None)
                    .map_err(|e| e.to_string())?;
            }
            let tx = db.transaction().map_err(|e| e.to_string())?;
            tx.execute_batch(include_str!("../migrations/001_initial.sql"))
                .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
        }
        if version < 2 {
            let backup = path.with_extension(format!("pre-v2-{}.db", id()));
            db.backup(rusqlite::MAIN_DB, &backup, None)
                .map_err(|e| e.to_string())?;
            let tx = db.transaction().map_err(|e| e.to_string())?;
            tx.execute_batch(include_str!("../migrations/002_questions.sql"))
                .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
        }
        if version < 3 {
            let backup = path.with_extension(format!("pre-v3-{}.db", id()));
            db.backup(rusqlite::MAIN_DB, &backup, None)
                .map_err(|e| e.to_string())?;
            let tx = db.transaction().map_err(|e| e.to_string())?;
            tx.execute_batch(include_str!("../migrations/003_undo.sql"))
                .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
        }
        if version < 4 {
            let backup = path.with_extension(format!("pre-v4-{}.db", id()));
            db.backup(rusqlite::MAIN_DB, &backup, None)
                .map_err(|e| e.to_string())?;
            let tx = db.transaction().map_err(|e| e.to_string())?;
            tx.execute_batch(include_str!("../migrations/004_watches.sql"))
                .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
        }
        if version < 5 {
            let backup = path.with_extension(format!("pre-v5-{}.db", id()));
            db.backup(rusqlite::MAIN_DB, &backup, None)
                .map_err(|e| e.to_string())?;
            let tx = db.transaction().map_err(|e| e.to_string())?;
            tx.execute_batch(include_str!("../migrations/005_memory_analysis.sql"))
                .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
        }
        if version < 6 {
            let backup = path.with_extension(format!("pre-v6-{}.db", id()));
            db.backup(rusqlite::MAIN_DB, &backup, None)
                .map_err(|e| e.to_string())?;
            let tx = db.transaction().map_err(|e| e.to_string())?;
            tx.execute_batch(include_str!("../migrations/006_semantic_memory.sql"))
                .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
        }
        if version < 7 {
            let backup = path.with_extension(format!("pre-v7-{}.db", id()));
            db.backup(rusqlite::MAIN_DB, &backup, None)
                .map_err(|e| e.to_string())?;
            let tx = db.transaction().map_err(|e| e.to_string())?;
            tx.execute_batch(include_str!("../migrations/007_file_rollbacks.sql"))
                .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
        }
        if version < 8 {
            let backup = path.with_extension(format!("pre-v8-{}.db", id()));
            db.backup(rusqlite::MAIN_DB, &backup, None)
                .map_err(|e| e.to_string())?;
            let tx = db.transaction().map_err(|e| e.to_string())?;
            tx.execute_batch(include_str!("../migrations/008_file_recovery.sql"))
                .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
        }
        if version < 9 {
            let backup = path.with_extension(format!("pre-v9-{}.db", id()));
            db.backup(rusqlite::MAIN_DB, &backup, None)
                .map_err(|e| e.to_string())?;
            let tx = db.transaction().map_err(|e| e.to_string())?;
            tx.execute_batch(include_str!("../migrations/009_assets.sql"))
                .map_err(|e| e.to_string())?;
            let columns = {
                let mut stmt = tx
                    .prepare("PRAGMA table_info(attachments)")
                    .map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map([], |r| r.get::<_, String>(1))
                    .map_err(|e| e.to_string())?;
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(|e| e.to_string())?
            };
            for (name, definition) in [
                ("asset_id", "TEXT REFERENCES asset_records(id)"),
                ("message_id", "TEXT REFERENCES messages(id)"),
                ("ephemeral", "INTEGER NOT NULL DEFAULT 0"),
                ("metadata_json", "TEXT NOT NULL DEFAULT '{}' "),
            ] {
                if !columns.iter().any(|column| column == name) {
                    tx.execute_batch(&format!(
                        "ALTER TABLE attachments ADD COLUMN {name} {definition}"
                    ))
                    .map_err(|e| e.to_string())?;
                }
            }
            tx.execute_batch("CREATE TRIGGER IF NOT EXISTS attachment_asset_ready_insert BEFORE INSERT ON attachments WHEN NEW.asset_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM asset_records WHERE id=NEW.asset_id AND state='ready') BEGIN SELECT RAISE(ABORT,'ASSET_NOT_READY'); END; CREATE TRIGGER IF NOT EXISTS attachment_asset_ready_update BEFORE UPDATE OF asset_id ON attachments WHEN NEW.asset_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM asset_records WHERE id=NEW.asset_id AND state='ready') BEGIN SELECT RAISE(ABORT,'ASSET_NOT_READY'); END;").map_err(|e|e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
        }
        if version < 10 {
            let backup = path.with_extension(format!("pre-v10-{}.db", id()));
            db.backup(rusqlite::MAIN_DB, &backup, None)
                .map_err(|e| e.to_string())?;
            let tx = db.transaction().map_err(|e| e.to_string())?;
            for (table, additions) in [
                (
                    "tasks",
                    vec![
                        ("description", "TEXT NOT NULL DEFAULT ''"),
                        ("revision", "INTEGER NOT NULL DEFAULT 0"),
                    ],
                ),
                (
                    "task_runs",
                    vec![("scheduled_for", "TEXT"), ("error", "TEXT")],
                ),
            ] {
                let columns = {
                    let mut stmt = tx
                        .prepare(&format!("PRAGMA table_info({table})"))
                        .map_err(|e| e.to_string())?;
                    let rows = stmt
                        .query_map([], |r| r.get::<_, String>(1))
                        .map_err(|e| e.to_string())?;
                    rows.collect::<Result<Vec<_>, _>>()
                        .map_err(|e| e.to_string())?
                };
                for (column, definition) in additions {
                    if !columns.iter().any(|c| c == column) {
                        tx.execute_batch(&format!(
                            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
                        ))
                        .map_err(|e| e.to_string())?;
                    }
                }
            }
            tx.execute_batch(include_str!("../migrations/010_tasks.sql"))
                .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
        }
        if version < 11 {
            let backup = path.with_extension(format!("pre-v11-{}.db", id()));
            db.backup(rusqlite::MAIN_DB, &backup, None)
                .map_err(|e| e.to_string())?;
            let tx = db.transaction().map_err(|e| e.to_string())?;
            tx.execute_batch(include_str!("../migrations/011_clipboard.sql"))
                .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
        }
        if version < 12 {
            let backup = path.with_extension(format!("pre-v12-{}.db", id()));
            db.backup(rusqlite::MAIN_DB, &backup, None)
                .map_err(|e| e.to_string())?;
            let tx = db.transaction().map_err(|e| e.to_string())?;
            tx.execute_batch(include_str!("../migrations/012_drive.sql"))
                .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
        }
        if version < 13 {
            let backup = path.with_extension(format!("pre-v13-{}.db", id()));
            db.backup(rusqlite::MAIN_DB, &backup, None)
                .map_err(|e| e.to_string())?;
            let tx = db.transaction().map_err(|e| e.to_string())?;
            tx.execute_batch(include_str!("../migrations/013_chat_progress.sql"))
                .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
        }
        // Drive recovery uses the job's native lease and a status probe. Opening
        // another Core must never relabel a live cooperating actor's operation.
        db.execute("UPDATE file_cleanup_plans SET state='interrupted',error='CLEANUP_INTERRUPTED' WHERE state='running'", []).map_err(|e| e.to_string())?;
        db.execute("UPDATE file_rollbacks SET state='interrupted',error='ROLLBACK_INTERRUPTED' WHERE state='running'", []).map_err(|e| e.to_string())?;
        db.execute("UPDATE conversation_analysis SET status='interrupted',error_code='MEMORY_ANALYSIS_INTERRUPTED' WHERE status='running'",[]).map_err(|e|e.to_string())?;
        db.execute(
            "UPDATE questions SET status='cancelled' WHERE status='pending'",
            [],
        )
        .map_err(|e| e.to_string())?;
        let integrity: String = db
            .query_row("PRAGMA quick_check(1)", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if integrity != "ok" {
            return Err("DB_INTEGRITY_FAILED".into());
        }
        db.execute(
            "UPDATE permission_requests SET status='cancelled' WHERE status='pending'",
            [],
        )
        .map_err(|e| e.to_string())?;
        db.execute(
            "UPDATE tasks SET status='blocked',revision=revision+1 WHERE status IN ('running','waiting_permission')",
            [],
        )
        .map_err(|e| e.to_string())?;
        Ok(Self(Mutex::new(db)))
    }
    pub fn query(&self, sql: &str, values: &[Value]) -> Result<Value, String> {
        let db = self.0.lock().map_err(|_| "DB_LOCK")?;
        let mut stmt = db.prepare(sql).map_err(|e| e.to_string())?;
        let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let args: Vec<rusqlite::types::Value> = values
            .iter()
            .map(|v| match v {
                Value::Null => rusqlite::types::Value::Null,
                Value::Number(n) if n.is_i64() => {
                    rusqlite::types::Value::Integer(n.as_i64().unwrap_or_default())
                }
                Value::Number(n) => rusqlite::types::Value::Real(n.as_f64().unwrap_or_default()),
                Value::Bool(b) => rusqlite::types::Value::Integer(i64::from(*b)),
                Value::String(s) => rusqlite::types::Value::Text(s.clone()),
                _ => rusqlite::types::Value::Text(v.to_string()),
            })
            .collect();
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args), |row| {
                let mut out = serde_json::Map::new();
                for (i, name) in names.iter().enumerate() {
                    out.insert(
                        name.clone(),
                        match row.get_ref(i)? {
                            ValueRef::Null => Value::Null,
                            ValueRef::Integer(v) => json!(v),
                            ValueRef::Real(v) => json!(v),
                            ValueRef::Text(v) => json!(String::from_utf8_lossy(v)),
                            ValueRef::Blob(_) => json!("[BINARY]"),
                        },
                    );
                }
                Ok(Value::Object(out))
            })
            .map_err(|e| e.to_string())?;
        Ok(Value::Array(
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?,
        ))
    }
    pub fn execute(&self, sql: &str, values: &[Value]) -> Result<usize, String> {
        let db = self.0.lock().map_err(|_| "DB_LOCK")?;
        let args: Vec<rusqlite::types::Value> = values
            .iter()
            .map(|v| match v {
                Value::Null => rusqlite::types::Value::Null,
                Value::Number(n) => rusqlite::types::Value::Real(n.as_f64().unwrap_or_default()),
                Value::Bool(b) => rusqlite::types::Value::Integer(i64::from(*b)),
                Value::String(s) => rusqlite::types::Value::Text(s.clone()),
                _ => rusqlite::types::Value::Text(v.to_string()),
            })
            .collect();
        db.execute(sql, rusqlite::params_from_iter(args))
            .map_err(|e| e.to_string())
    }
    pub fn setting(&self, key: &str) -> Value {
        self.query("SELECT json_value FROM settings WHERE key=?", &[json!(key)])
            .ok()
            .and_then(|v| {
                v[0]["json_value"]
                    .as_str()
                    .and_then(|s| serde_json::from_str(s).ok())
            })
            .unwrap_or(Value::Null)
    }
    pub fn set_setting(&self, key: &str, value: &Value) -> Result<(), String> {
        self.execute("INSERT INTO settings VALUES(?,?) ON CONFLICT(key) DO UPDATE SET json_value=excluded.json_value",&[json!(key),json!(value.to_string())])?;
        Ok(())
    }
    pub fn audit(
        &self,
        tool: &str,
        risk: &str,
        input: &Value,
        status: &str,
        output: &Value,
        duration: u64,
    ) -> Result<(), String> {
        // A remote MCP result cannot publish a local undo capability or carry
        // private provider content through this auxiliary durable column.
        let undo = if tool.starts_with("mcp.") {
            Value::Null
        } else {
            output["undo_token"].clone()
        };
        self.execute("INSERT INTO activity_events(id,timestamp,tool_id,kind,risk,input_summary,output_summary,status,duration_ms,undo_token) VALUES(?,?,?,?,?,?,?,?,?,?)",&[json!(id()),json!(now()),json!(tool),json!("tool"),json!(risk),json!(crate::policy::tool_redact(tool,input).to_string()),json!(crate::policy::tool_redact(tool,output).to_string()),json!(status),json!(duration),undo])?;
        Ok(())
    }
    pub fn remember(&self, input: &Value) -> Result<Value, String> {
        use rusqlite::OptionalExtension;
        let content = input["content"].as_str().ok_or("MEMORY_CONTENT")?;
        let generated_title = content.chars().take(80).collect::<String>();
        let title = input["title"].as_str().unwrap_or(&generated_title);
        let category = input["type"].as_str().unwrap_or("fact");
        if ![
            "preference",
            "fact",
            "person",
            "project",
            "decision",
            "procedure",
            "path_resource",
            "recurring_context",
            "lesson",
            "tool_usage_preference",
        ]
        .contains(&category)
        {
            return Err("MEMORY_TYPE".into());
        }
        if content.trim().is_empty()
            || content.len() > 8000
            || title.len() > 2000
            || contains_secret(content)
            || contains_secret(title)
        {
            return Err("MEMORY_SECRET_OR_TOO_LARGE".into());
        }
        for field in ["project_id", "source_conversation_id", "supersedes"] {
            if !input[field].is_null()
                && !input[field]
                    .as_str()
                    .is_some_and(|v| !v.is_empty() && v.len() <= 100)
            {
                return Err(format!("MEMORY_INVALID_{field}"));
            }
        }
        if !input["explicit"].is_null() && !input["explicit"].is_boolean() {
            return Err("MEMORY_EXPLICIT_TYPE".into());
        }
        if !input["source_message_ids"].is_null() && !input["source_message_ids"].is_array() {
            return Err("MEMORY_SOURCE_IDS".into());
        }
        let sources = input["source_message_ids"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if sources.len() > 20
            || sources
                .iter()
                .any(|s| !s.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 100))
        {
            return Err("MEMORY_SOURCE_IDS".into());
        }
        let project = input["project_id"].as_str();
        let source = input["source_conversation_id"].as_str();
        let explicit = input["explicit"].as_bool().unwrap_or(true);
        for field in ["confidence", "importance"] {
            if input.get(field).is_some_and(|v| !v.is_number()) {
                return Err("MEMORY_SCORE_TYPE".into());
            }
        }
        let confidence = input["confidence"].as_f64().unwrap_or(0.65);
        let importance = input["importance"].as_f64().unwrap_or(0.5);
        if !(0.0..=1.0).contains(&confidence) || !(0.0..=1.0).contains(&importance) {
            return Err("MEMORY_SCORE_RANGE".into());
        }
        let metadata = input.get("metadata").cloned().unwrap_or(json!({}));
        if !metadata.is_object()
            || metadata.to_string().len() > 64000
            || contains_secret(&metadata.to_string())
        {
            return Err("MEMORY_METADATA_INVALID".into());
        }
        let key = content
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        // Privacy, source checks, dedupe, evidence and correction are one transaction.
        let db = self.0.lock().map_err(|_| "DB_LOCK")?;
        let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
        let private: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM settings WHERE key IN ('privacy','no_memory') AND json_value='true')",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if private {
            return Err("PRIVACY_MEMORY_DENIED".into());
        }
        if let Some(source) = source {
            if let Some(expected) = input.get("expected_project_id") {
                let project: Option<String> = tx
                    .query_row(
                        "SELECT project_id FROM conversations WHERE id=?",
                        [source],
                        |r| r.get(0),
                    )
                    .map_err(|_| "MEMORY_SOURCE_NOT_FOUND")?;
                if project.as_deref() != expected.as_str() {
                    return Err("MEMORY_CANDIDATE_SCOPE_CHANGED".into());
                }
            }
            let private: Option<bool> = tx
                .query_row(
                    "SELECT no_memory FROM conversations WHERE id=?",
                    [source],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            match private {
                None => return Err("MEMORY_SOURCE_NOT_FOUND".into()),
                Some(true) => return Err("CONVERSATION_NO_MEMORY".into()),
                _ => {}
            }
        }
        for message in &sources {
            let actual: Option<String> = tx
                .query_row(
                    "SELECT conversation_id FROM messages WHERE id=?",
                    [message.as_str()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            if actual.is_none() || actual.as_deref() != source {
                return Err("MEMORY_SOURCE_SCOPE".into());
            }
            if input["inferred_candidate"] == true {
                let (role, status, text): (String, String, String) = tx
                    .query_row(
                        "SELECT role,status,content_text FROM messages WHERE id=?",
                        [message.as_str()],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .map_err(|e| e.to_string())?;
                if role != "user" || status != "complete" || contains_secret(&text) {
                    return Err("MEMORY_CANDIDATE_SOURCE_UNTRUSTED".into());
                }
                let evidence = metadata["evidence"]
                    .as_array()
                    .ok_or("MEMORY_CANDIDATE_EVIDENCE")?;
                let quotes = evidence
                    .iter()
                    .filter(|e| e["message_id"] == *message)
                    .collect::<Vec<_>>();
                if quotes.is_empty()
                    || quotes.iter().any(|e| {
                        !e["quote"].as_str().is_some_and(|q| {
                            !q.trim().is_empty() && text.contains(q) && !contains_secret(q)
                        })
                    })
                {
                    return Err("MEMORY_CANDIDATE_EVIDENCE_MISMATCH".into());
                }
            }
        }
        if let Some(expected) = input["expected_memory_id"].as_str() {
            let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM memories WHERE id=? AND normalized_key=? AND project_id IS ? AND superseded_by IS NULL AND disabled=0)",params![expected,key,project],|r|r.get(0)).map_err(|e|e.to_string())?;
            if !active {
                return Err("MEMORY_CANDIDATE_MEMORY_CHANGED".into());
            }
        }
        let existing:Option<String>=tx.query_row("SELECT id FROM memories WHERE normalized_key=? AND project_id IS ? AND superseded_by IS NULL",params![key,project],|r|r.get(0)).optional().map_err(|e|e.to_string())?;
        let deduplicated = existing.is_some();
        let memory_id = existing.unwrap_or_else(id);
        if !deduplicated {
            tx.execute("INSERT INTO memories(id,type,title,content,normalized_key,project_id,source_conversation_id,confidence,importance,explicit,created_at,updated_at,metadata_json) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",params![memory_id,category,title,content,key,project,source,if explicit{1.0}else{confidence.min(0.9)},if explicit{0.9}else{importance.min(0.8)},explicit,now(),now(),metadata.to_string()]).map_err(|e|e.to_string())?;
        }
        let mut new_sources = 0;
        for message in &sources {
            new_sources+=tx.execute("INSERT INTO memory_sources(memory_id,conversation_id,message_id) SELECT ?,?,? WHERE NOT EXISTS(SELECT 1 FROM memory_sources WHERE memory_id=? AND message_id=?)",params![memory_id,source,message.as_str(),memory_id,message.as_str()]).map_err(|e|e.to_string())?;
        }
        if deduplicated {
            tx.execute("UPDATE memories SET confidence=CASE WHEN explicit=1 OR ?=1 THEN 1.0 ELSE MIN(0.9,confidence+0.05*?) END,importance=CASE WHEN ?=1 THEN MAX(importance,0.9) ELSE importance END,explicit=MAX(explicit,?),updated_at=? WHERE id=?",params![explicit,new_sources as i64,explicit,explicit,now(),memory_id]).map_err(|e|e.to_string())?;
            if input.get("metadata").is_some() {
                let previous: String = tx
                    .query_row(
                        "SELECT metadata_json FROM memories WHERE id=?",
                        [&memory_id],
                        |r| r.get(0),
                    )
                    .map_err(|e| e.to_string())?;
                let previous: Value =
                    serde_json::from_str(&previous).map_err(|_| "MEMORY_METADATA_INVALID")?;
                let mut merged = metadata.clone();
                let mut evidence = previous["evidence"].as_array().cloned().unwrap_or_default();
                for item in metadata["evidence"].as_array().into_iter().flatten() {
                    if !evidence.contains(item) {
                        evidence.push(item.clone());
                    }
                }
                if evidence.len() > 16 {
                    evidence.drain(..evidence.len() - 16);
                }
                merged["evidence"] = json!(evidence);
                merged["reviewed"] =
                    json!(previous["reviewed"] == true || metadata["reviewed"] == true);
                if merged.to_string().len() > 64000 {
                    return Err("MEMORY_METADATA_INVALID".into());
                }
                tx.execute(
                    "UPDATE memories SET metadata_json=? WHERE id=?",
                    params![merged.to_string(), memory_id],
                )
                .map_err(|e| e.to_string())?;
            }
        }
        if let Some(old) = input["supersedes"].as_str().filter(|old| *old != memory_id) {
            let changed=tx.execute("UPDATE memories SET superseded_by=?,disabled=1,updated_at=? WHERE id=? AND project_id IS ? AND superseded_by IS NULL",params![memory_id,now(),old,project]).map_err(|e|e.to_string())?;
            if changed != 1 {
                return Err("MEMORY_SUPERSEDE_NOT_FOUND".into());
            }
        }
        if let Some(candidate) = input["candidate_id"].as_str() {
            let changed=tx.execute("UPDATE memory_candidates SET state='accepted',memory_id=?,updated_at=? WHERE id=? AND conversation_id=? AND state IN ('pending','accepted')",params![memory_id,now(),candidate,source]).map_err(|e|e.to_string())?;
            if changed != 1 {
                return Err("MEMORY_CANDIDATE_NOT_PENDING".into());
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(json!({"id":memory_id,"deduplicated":deduplicated}))
    }
    pub fn retrieve(&self, query: &str, project: Option<&str>) -> Result<Value, String> {
        crate::semantic_store::retrieve(self, query, project, None)
    }
    pub fn rebuild_search(&self) -> Result<Value, String> {
        let db = self.0.lock().map_err(|_| "DB_LOCK")?;
        let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
        tx.execute_batch("DELETE FROM memories_fts; INSERT INTO memories_fts SELECT id,title,content FROM memories; DELETE FROM conversations_fts; INSERT INTO conversations_fts SELECT id,title,summary FROM conversations; DELETE FROM messages_fts; INSERT INTO messages_fts SELECT id,conversation_id,content_text FROM messages; DELETE FROM projects_fts; INSERT INTO projects_fts SELECT id,name,tags_json FROM projects;").map_err(|e|e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(json!({"rebuilt":true}))
    }
}
pub fn fts_query(q: &str) -> String {
    q.split_whitespace()
        .filter(|s| s.chars().any(char::is_alphanumeric))
        .take(12)
        .map(|s| format!("\"{}\"", s.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ")
}
#[cfg(test)]
mod tests {
    use super::*;
    fn store() -> Store {
        let p = std::env::temp_dir().join(format!("AlfredPet-{}.db", id()));
        Store::open(&p).unwrap()
    }
    #[test]
    fn memory_dedupe_supersede_scope_secrets() {
        let s = store();
        let a = s
            .remember(&json!({"content":"Prefiero ventanas grises"}))
            .unwrap();
        assert_eq!(
            s.remember(&json!({"content":"prefiero  ventanas grises"}))
                .unwrap()["deduplicated"],
            true
        );
        let b = s
            .remember(&json!({"content":"Prefiero ventanas compactas","supersedes":a["id"]}))
            .unwrap();
        assert_ne!(a["id"], b["id"]);
        assert_eq!(
            s.retrieve("ventanas", None)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(s.remember(&json!({"content":"password = abc"})).is_err());
        s.set_setting("privacy", &json!(true)).unwrap();
        assert!(s.remember(&json!({"content":"Un hecho"})).is_err());
    }
    #[test]
    fn ranking_scope_diversity_and_sources_survive_rebuild() {
        let s = store();
        for c in ["c1", "c2"] {
            s.execute(
                "INSERT INTO conversations(id,title,created_at,updated_at) VALUES(?,?,?,?)",
                &[
                    json!(c),
                    json!("Memory fixture"),
                    json!(now()),
                    json!(now()),
                ],
            )
            .unwrap();
        }
        for (message, conversation) in [("m1", "c1"), ("m2", "c1"), ("foreign", "c2")] {
            s.execute(
                "INSERT INTO messages VALUES(?,?,?,?,?,?)",
                &[
                    json!(message),
                    json!(conversation),
                    json!("user"),
                    json!("ventanas fixture"),
                    json!("complete"),
                    json!(now()),
                ],
            )
            .unwrap();
        }
        let first=s.remember(&json!({"content":"ventanas compactas","source_conversation_id":"c1","source_message_ids":["m1"],"explicit":false})).unwrap();
        let repeated=s.remember(&json!({"content":"Ventanas  compactas","source_conversation_id":"c1","source_message_ids":["m2"],"explicit":false})).unwrap();
        assert_eq!(first["id"], repeated["id"]);
        assert!(s.remember(&json!({"content":"Otro hecho","source_conversation_id":"c1","source_message_ids":["foreign"]})).is_err());
        assert_eq!(
            s.query(
                "SELECT COUNT(*) AS n FROM memory_sources WHERE memory_id=?",
                &[first["id"].clone()]
            )
            .unwrap()[0]["n"],
            2
        );
        let confidence = s
            .query(
                "SELECT confidence FROM memories WHERE id=?",
                &[first["id"].clone()],
            )
            .unwrap()[0]["confidence"]
            .as_f64()
            .unwrap();
        assert!(confidence > 0.65 && confidence < 1.0);
        for n in 0..8 {
            s.remember(&json!({"content":format!("ventanas fact fixture {n}"),"type":"fact"}))
                .unwrap();
        }
        let varied = s
            .remember(&json!({"content":"ventanas preferencia única","type":"preference"}))
            .unwrap();
        let foreign = s
            .remember(&json!({"content":"ventanas proyecto privado","project_id":"other-project"}))
            .unwrap();
        let pinned = s
            .remember(&json!({"content":"receta sin coincidencia"}))
            .unwrap();
        s.execute(
            "UPDATE memories SET pinned=1 WHERE id=?",
            &[pinned["id"].clone()],
        )
        .unwrap();
        s.execute("DELETE FROM memories_fts", &[]).unwrap();
        assert!(s
            .retrieve("ventanas", None)
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
        s.rebuild_search().unwrap();
        let rows = s.retrieve("ventanas", Some("active-project")).unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 6);
        assert!(rows.iter().any(|row| row["id"] == varied["id"]));
        assert!(!rows
            .iter()
            .any(|row| row["id"] == foreign["id"] || row["id"] == pinned["id"]));
        assert!(rows
            .iter()
            .all(|row| row["use_count"] == 1 && row["ranking_score"].as_f64().is_some()));
        let long = s.remember(&json!({"content":"memo ".repeat(700)})).unwrap();
        assert!(
            s.query(
                "SELECT title FROM memories WHERE id=?",
                &[long["id"].clone()]
            )
            .unwrap()[0]["title"]
                .as_str()
                .unwrap()
                .chars()
                .count()
                <= 80
        );
        assert!(s.remember(&json!({"content":" "})).is_err());
        s.execute("UPDATE conversations SET no_memory=1 WHERE id='c1'", &[])
            .unwrap();
        assert!(s
            .remember(&json!({"content":"private fact","source_conversation_id":"c1"}))
            .is_err());
        assert!(s
            .remember(&json!({"content":"bad source","source_conversation_id":"missing"}))
            .is_err());
        s.set_setting("privacy", &json!(true)).unwrap();
        assert!(s
            .retrieve("ventanas", None)
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
    }
    #[test]
    fn correction_to_existing_memory_is_atomic() {
        let s = store();
        let old = s
            .remember(&json!({"content":"Preferencia antigua"}))
            .unwrap();
        let current = s
            .remember(&json!({"content":"Preferencia vigente"}))
            .unwrap();
        let correction = s
            .remember(&json!({"content":"preferencia vigente","supersedes":old["id"]}))
            .unwrap();
        assert_eq!(correction["id"], current["id"]);
        assert_eq!(correction["deduplicated"], true);
        assert_eq!(
            s.query(
                "SELECT superseded_by FROM memories WHERE id=?",
                &[old["id"].clone()]
            )
            .unwrap()[0]["superseded_by"],
            current["id"]
        );
        assert_eq!(
            s.query("SELECT COUNT(*) AS n FROM memories", &[]).unwrap()[0]["n"],
            2
        );
        let before = s
            .query(
                "SELECT updated_at FROM memories WHERE id=?",
                &[current["id"].clone()],
            )
            .unwrap();
        assert!(s
            .remember(&json!({"content":"Preferencia vigente","supersedes":"nonexistent"}))
            .is_err());
        assert_eq!(
            before,
            s.query(
                "SELECT updated_at FROM memories WHERE id=?",
                &[current["id"].clone()]
            )
            .unwrap()
        );
    }
    #[test]
    fn migration_integrity_fts() {
        let s = store();
        s.execute(
            "INSERT INTO conversations(id,title,created_at,updated_at) VALUES(?,?,?,?)",
            &[
                json!("c"),
                json!("MariaDB servidor"),
                json!(now()),
                json!(now()),
            ],
        )
        .unwrap();
        assert_eq!(
            s.query(
                "SELECT id FROM conversations_fts WHERE conversations_fts MATCH ?",
                &[json!(fts_query("MariaDB"))]
            )
            .unwrap()[0]["id"],
            "c"
        );
    }
}
