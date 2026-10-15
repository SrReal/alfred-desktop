//! Native file drops are queued for an explicit conversation choice; no automatic turn.
use crate::{runtime::Core, storage};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashSet, path::PathBuf, sync::Arc};
const KEY: &str = "pet_drop_queue";
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    id: String,
    path: String,
    name: String,
    created_at: String,
    error: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Queue {
    version: u32,
    items: Vec<Item>,
    last_error: Option<String>,
}
impl Default for Queue {
    fn default() -> Self {
        Self {
            version: 1,
            items: Vec::new(),
            last_error: None,
        }
    }
}
fn uuid(id: &str) -> Result<(), String> {
    if uuid::Uuid::parse_str(id)
        .map_err(|_| "DROP_ID")?
        .to_string()
        != id
    {
        return Err("DROP_ID".into());
    }
    Ok(())
}
fn update<T>(
    c: &Core,
    action: impl FnOnce(&rusqlite::Transaction<'_>, &mut Queue) -> Result<T, String>,
) -> Result<T, String> {
    let mut db = c.store.0.lock().map_err(|_| "DB_LOCK")?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| "DROP_TRANSACTION")?;
    let raw: Option<String> = tx
        .query_row("SELECT json_value FROM settings WHERE key=?", [KEY], |r| {
            r.get(0)
        })
        .optional()
        .map_err(|_| "DROP_QUEUE_READ")?;
    let mut q = match raw {
        Some(s) => serde_json::from_str(&s).map_err(|_| "DROP_QUEUE_JSON")?,
        None => Queue::default(),
    };
    let mut ids = HashSet::new();
    if q.version != 1 || q.items.len() > 20 {
        return Err("DROP_QUEUE_CHANGED".into());
    }
    for item in &q.items {
        uuid(&item.id)?;
        if !ids.insert(item.id.clone())
            || item.path.len() > 32767
            || item.name.len() > 1024
            || chrono::DateTime::parse_from_rfc3339(&item.created_at).is_err()
        {
            return Err("DROP_QUEUE_CHANGED".into());
        }
    }
    let result = action(&tx, &mut q)?;
    let raw = serde_json::to_string(&q).map_err(|_| "DROP_QUEUE_JSON")?;
    tx.execute("INSERT INTO settings(key,json_value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET json_value=excluded.json_value",rusqlite::params![KEY,raw]).map_err(|_|"DROP_QUEUE_WRITE")?;
    tx.commit().map_err(|_| "DROP_QUEUE_COMMIT")?;
    Ok(result)
}
pub fn inside(x: f64, y: f64, width: u32, height: u32) -> bool {
    x.is_finite()
        && y.is_finite()
        && x >= 0.0
        && y >= 0.0
        && x < f64::from(width)
        && y < f64::from(height)
}
pub fn record_error(c: &Core, error: &str) -> Result<(), String> {
    update(c, |_, q| {
        q.last_error = Some(error.chars().take(2000).collect());
        Ok(())
    })?;
    c.emit("alfredpet:drops", &json!({}));
    Ok(())
}
pub fn list(c: &Core) -> Result<Value, String> {
    update(c, |tx, q| {
        // The attachment UUID is the durable receipt if the process stopped before consuming the queue item.
        let mut keep = Vec::new();
        for item in q.items.drain(..) {
            let n: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM attachments WHERE id=?",
                    [&item.id],
                    |r| r.get(0),
                )
                .map_err(|_| "DROP_RECEIPT_READ")?;
            if n == 0 {
                keep.push(item);
            }
        }
        q.items = keep;
        Ok(json!(q))
    })
}
pub async fn receive(
    c: &Arc<Core>,
    surface: &str,
    in_bounds: bool,
    paths: &[PathBuf],
) -> Result<Value, String> {
    if surface != "pet" || !in_bounds {
        return Err("PET_DROP_TARGET_REQUIRED".into());
    }
    if paths.is_empty() || paths.len() > 5 {
        return Err("DROP_FILE_COUNT: max five files per drop".into());
    }
    if c.store.setting("privacy") == true {
        return Err("PRIVACY_DENIED".into());
    }
    let _action = c.drop_actions.lock().await;
    let mut incoming = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        let path = c.path(path.to_str().ok_or("DROP_PATH_ENCODING")?, false)?;
        let metadata = std::fs::metadata(&path).map_err(|_| "DROP_FILE_MISSING")?;
        if !metadata.is_file() {
            return Err("DROP_REGULAR_FILE_REQUIRED".into());
        }
        if metadata.len() > 32 * 1024 * 1024 {
            return Err("ASSET_BYTE_LIMIT".into());
        }
        let path = path.to_str().ok_or("DROP_PATH_ENCODING")?.to_owned();
        if path.len() > 32767 {
            return Err("DROP_PATH_LIMIT".into());
        }
        if seen.insert(path.to_lowercase()) {
            incoming.push(Item {
                id: storage::id(),
                name: PathBuf::from(&path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .chars()
                    .take(200)
                    .collect(),
                path,
                created_at: storage::now(),
                error: None,
            });
        }
    }
    if c.store.setting("privacy") == true {
        return Err("PRIVACY_DENIED".into());
    }
    let value = update(c, |tx, q| {
        let privacy: Option<String> = tx
            .query_row(
                "SELECT json_value FROM settings WHERE key='privacy'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| "DROP_PRIVACY_READ")?;
        if privacy.as_deref() == Some("true") {
            return Err("PRIVACY_DENIED".into());
        }

        incoming.retain(|new| {
            !q.items
                .iter()
                .any(|old| old.path.to_lowercase() == new.path.to_lowercase())
        });
        if q.items.len() + incoming.len() > 20 {
            return Err("DROP_QUEUE_FULL: review pending files first".into());
        }
        q.items.extend(incoming);
        q.last_error = None;
        Ok(json!(q))
    })?;
    c.emit("alfredpet:drops", &json!({}));
    Ok(value)
}
pub async fn dispatch(c: &Arc<Core>, operation: &str, input: &Value) -> Result<Value, String> {
    let object = input.as_object().ok_or("DROP_INPUT")?;
    let attach = operation == "pet_drop.attach";
    if !attach && operation != "pet_drop.dismiss" {
        return Err("DROP_OPERATION".into());
    }
    if object.keys().any(|key| {
        !if attach {
            ["ids", "conversation_id"].contains(&key.as_str())
        } else {
            key == "ids"
        }
    }) {
        return Err("DROP_INPUT".into());
    }
    let ids: Vec<String> = serde_json::from_value(input["ids"].clone()).map_err(|_| "DROP_IDS")?;
    if ids.is_empty() || ids.len() > if attach { 5 } else { 20 } {
        return Err("DROP_SELECTION_LIMIT".into());
    }
    let mut selected = HashSet::new();
    for id in &ids {
        uuid(id)?;
        if !selected.insert(id.clone()) {
            return Err("DROP_DUPLICATE_ID".into());
        }
    }
    let _action = c.drop_actions.lock().await;
    let chosen = update(c, |_, q| {
        let items: Vec<Item> = q
            .items
            .iter()
            .filter(|i| selected.contains(&i.id))
            .cloned()
            .collect();
        if items.len() != ids.len() {
            return Err("DROP_SELECTION_CHANGED".into());
        }
        Ok(items)
    })?;
    if !attach {
        update(c, |_, q| {
            q.items.retain(|i| !selected.contains(&i.id));
            q.last_error = None;
            Ok(())
        })?;
        c.emit("alfredpet:drops", &json!({}));
        return Ok(json!({"dismissed":ids,"original_files_preserved":true}));
    }
    let conversation = input["conversation_id"]
        .as_str()
        .ok_or("ATTACHMENT_CONVERSATION")?;
    uuid(conversation)?;
    if c.store.setting("privacy") == true {
        return Err("PRIVACY_DENIED".into());
    }
    let mut attached = Vec::new();
    let mut failed = Vec::new();
    for item in chosen {
        let result = if c.store.setting("privacy") == true {
            Err("PRIVACY_DENIED".into())
        } else {
            crate::assets::attach_from_drop(
                c,
                &json!({"conversation_id":conversation,"path":item.path}),
                &item.id,
            )
            .await
        };
        match result {
            Ok(value) => {
                attached.push(value);
                update(c, |_, q| {
                    q.items.retain(|i| i.id != item.id);
                    Ok(())
                })?;
            }
            Err(error) => {
                failed.push(json!({"id":item.id,"name":item.name,"error":error}));
                update(c, |_, q| {
                    if let Some(i) = q.items.iter_mut().find(|i| i.id == item.id) {
                        i.error = Some(error.chars().take(2000).collect());
                    }
                    Ok(())
                })?;
            }
        }
    }
    let result = json!({"conversation_id":conversation,"attached":attached,"failed":failed,"original_files_preserved":true});
    c.store.audit(
        "pet_drop.attach",
        "Contextual",
        input,
        if failed.is_empty() {
            "succeeded"
        } else {
            "failed"
        },
        &result,
        0,
    )?;
    c.emit("alfredpet:drops", &json!({}));
    c.emit("alfredpet:activity", &json!({}));
    Ok(result)
}
#[cfg(all(test, windows))]
#[path = "pet_drop_tests.rs"]
mod tests;
