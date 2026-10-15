//! Opt-in maintenance prepares bounded, concrete manifests. Every erasure still
//! goes through Core's mandatory permission; no standing destructive grant.
use super::*;
use std::sync::Arc;

const HISTORY_LIMIT: usize = 128;
pub(crate) fn validate(value: &Value) -> Result<(), String> {
    let object = value.as_object().ok_or("MAINTENANCE_CONFIG")?;
    if object.len() != 3
        || object
            .keys()
            .any(|key| !["enabled", "staging_days", "include_recovery"].contains(&key.as_str()))
        || value["enabled"].as_bool().is_none()
        || value["include_recovery"].as_bool().is_none()
    {
        return Err("MAINTENANCE_CONFIG".into());
    }
    cleanup::validate_retention(&value["staging_days"])
}
fn config(c: &Core) -> Result<Value, String> {
    let value = c.store.setting("file_maintenance");
    if value.is_null() {
        return Ok(json!({"enabled":false,"staging_days":null,"include_recovery":false}));
    }
    validate(&value)?;
    Ok(value)
}
pub(super) fn policy(c: &Core) -> Value {
    json!({"config":c.store.setting("file_maintenance"),"checkpoint_days":c.store.setting("file_checkpoint_days"),"privacy":c.store.setting("privacy"),"dnd":c.store.setting("dnd"),"roots":c.roots()})
}
pub(crate) fn cancel(c: &Core) -> Result<(), String> {
    if let Some(token) = c
        .file_maintenance_cancel
        .lock()
        .map_err(|_| "MAINTENANCE_LOCK")?
        .as_ref()
    {
        token.cancel();
    }
    Ok(())
}
fn hash_value(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}
fn save(c: &Core, state: &mut Value, status: &str, error: Option<&str>) -> Result<Value, String> {
    state["status"] = json!(status);
    state["last_at"] = json!(storage::now());
    state["last_error"] = json!(error);
    c.store.set_setting("file_maintenance_state", state)?;
    c.emit("alfredpet:file_maintenance", state);
    Ok(state.clone())
}
// Cursor ordering uses SQLite's parsed instant plus UUID as a tie-breaker;
// textual RFC3339 ordering is incorrect for offsets/fractional seconds.
pub(super) fn page(
    c: &Core,
    cutoff: &str,
    cursor: &Value,
    include_recovery: bool,
) -> Result<(Vec<Value>, Value), String> {
    let date = cursor["date"].as_str().unwrap_or("0001-01-01T00:00:00Z");
    let id = cursor["id"].as_str().unwrap_or("");
    let rows = c.store.query("SELECT id,updated_at FROM undo_records WHERE (state='undone' OR (?=1 AND state IN ('available','blocked','forgotten'))) AND julianday(updated_at)<=julianday(?) AND (julianday(updated_at)>julianday(?) OR (julianday(updated_at)=julianday(?) AND id>?)) ORDER BY julianday(updated_at),id LIMIT 100", &[json!(include_recovery),json!(cutoff),json!(date),json!(date),json!(id)])?;
    let rows = rows.as_array().ok_or("UNDO_RECORDS")?;
    let next = if rows.len() == 100 {
        let last = rows.last().ok_or("MAINTENANCE_CURSOR")?;
        json!({"date":last["updated_at"],"id":last["id"]})
    } else {
        Value::Null
    };
    Ok((rows.iter().map(|row| row["id"].clone()).collect(), next))
}
pub(super) fn prepare(
    c: &Core,
    control: &Control,
    settings: &Value,
    state: &mut Value,
) -> Result<Option<String>, String> {
    control.bind_maintenance(c, settings)?;
    let cfg = config(c)?;
    let checkpoint = settings["checkpoint_days"].as_u64();
    let staging = cfg["staging_days"].as_u64();
    let age = checkpoint
        .into_iter()
        .chain(staging)
        .min()
        .ok_or("MAINTENANCE_AGE_NOT_CONFIGURED")?;
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(age as i64)).to_rfc3339();
    // Staging may belong to an otherwise still useful blocked/available journal.
    let (ids, cursor) = page(
        c,
        &cutoff,
        &state["cursor"],
        staging.is_some() || cfg["include_recovery"] == true,
    )?;
    state["cursor"] = cursor;
    state["scanned"] = json!(ids.len());
    if ids.is_empty() {
        return Ok(None);
    }
    let preview = cleanup::preview_selected(
        c,
        &json!({"ids":ids,"retention":true,"forget_undo":cfg["include_recovery"],"maintenance_preview":true}),
        control,
        |row, role| {
            let days = if role == "staging" {
                staging
            } else if row["state"] == "undone" || cfg["include_recovery"] == true {
                checkpoint
            } else {
                None
            };
            days.zip(row["updated_at"].as_str())
                .is_some_and(|(days, date)| {
                    chrono::DateTime::parse_from_rfc3339(date).is_ok_and(|date| {
                        date <= chrono::Utc::now() - chrono::Duration::days(days as i64)
                    })
                })
        },
    )?;
    if preview["plan_id"].is_null() {
        return Ok(None);
    }
    let mut snapshot = preview["snapshot"].clone();
    snapshot["maintenance_policy"] = settings.clone();
    let mut stable = snapshot.clone();
    stable["maintenance_policy"] = state["policy_key"].clone();
    for entry in stable["entries"].as_array_mut().ok_or("CLEANUP_ENTRIES")? {
        entry
            .as_object_mut()
            .ok_or("CLEANUP_ENTRY")?
            .remove("work_path");
    }
    let signature = hash_value(&stable);
    let seen = state["seen"].as_array_mut().ok_or("MAINTENANCE_HISTORY")?;
    let completed = c.store.query("SELECT id FROM file_cleanup_plans WHERE state='completed' AND id IN (SELECT json_extract(value,'$.plan_id') FROM json_each(?))", &[json!(serde_json::to_string(seen).map_err(|_| "MAINTENANCE_HISTORY")?)])?;
    seen.retain(|review| {
        !completed
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"] == review["plan_id"]))
    });
    if seen.iter().any(|item| item["signature"] == signature) {
        return Ok(None);
    }
    if seen.len() >= HISTORY_LIMIT {
        return Err("MAINTENANCE_REVIEW_HISTORY_FULL".into());
    }
    control.check(c)?;
    let id = preview["plan_id"]
        .as_str()
        .ok_or("CLEANUP_PLAN_ID")?
        .to_owned();
    c.store.execute(
        "INSERT INTO file_cleanup_plans(id,snapshot_json,created_at,updated_at) VALUES(?,?,?,?)",
        &[
            json!(id),
            json!(snapshot.to_string()),
            json!(storage::now()),
            json!(storage::now()),
        ],
    )?;
    // Remember before opening permission so a process restart cannot repeatedly
    // solicit the same unchanged rejected/unanswered destructive batch.
    seen.push(json!({"signature":signature,"plan_id":id}));
    state["plan_id"] = json!(id);
    save(c, state, "awaiting_permission", None)?;
    Ok(Some(id))
}

pub(super) async fn cycle(c: &Arc<Core>) -> Result<Value, String> {
    let cfg = config(c)?;
    let mut state = c.store.setting("file_maintenance_state");
    if !state.is_object() {
        state = json!({});
    }
    let paused = if cfg["enabled"] != true {
        Some("disabled")
    } else if !c.enabled("files") {
        Some("module_disabled")
    } else if c.store.setting("privacy") == true {
        Some("privacy")
    } else if c.store.setting("dnd") == true {
        Some("dnd")
    } else if cfg["staging_days"].is_null() && c.store.setting("file_checkpoint_days").is_null() {
        Some("age_not_configured")
    } else {
        None
    };
    if let Some(status) = paused {
        return save(c, &mut state, status, None);
    }
    cleanup::validate_retention(&c.store.setting("file_checkpoint_days"))?;
    c.normalize_scopes()?;
    let token = CancellationToken::new();
    {
        let mut active = c
            .file_maintenance_cancel
            .lock()
            .map_err(|_| "MAINTENANCE_LOCK")?;
        if active.is_some() {
            return Err("MAINTENANCE_ALREADY_RUNNING".into());
        }
        *active = Some(token.clone());
    }
    struct Active<'a>(&'a Core);
    impl Drop for Active<'_> {
        fn drop(&mut self) {
            if let Ok(mut active) = self.0.file_maintenance_cancel.lock() {
                if let Some(token) = active.take() {
                    token.cancel();
                }
            }
        }
    }
    let _active = Active(c);
    let settings = policy(c);
    // Enable/privacy/DND only pause. Returning to the same retention/scope
    // does not reset suppression or re-prompt unchanged declined manifests.
    let key = hash_value(
        &json!({"checkpoint":settings["checkpoint_days"],"staging":cfg["staging_days"],"include_recovery":cfg["include_recovery"],"roots":settings["roots"]}),
    );
    if state["policy_key"] != key {
        state = json!({"policy_key":key,"cursor":null,"seen":[],"plan_id":null});
    }
    let core = c.clone();
    let guarded = settings.clone();
    let worker_token = token.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        let control = Control::new(worker_token);
        let _serial = core.file_actions.lock().map_err(|_| "FILES_LOCK")?;
        let plan = prepare(&core, &control, &guarded, &mut state);
        Ok::<_, String>((state, plan))
    })
    .await
    .map_err(|_| "MAINTENANCE_WORKER")??;
    state = prepared.0;
    let id = match prepared.1 {
        Ok(Some(id)) => id,
        Ok(None) => return save(c, &mut state, "scanned", None),
        Err(error) => return save(c, &mut state, "attention_required", Some(&error)),
    };
    let result = tokio::select! {
        biased;
        _ = token.cancelled() => Err("MAINTENANCE_CANCELLED".to_owned()),
        result = c.execute("files.cleanup", json!({"body":{"action":"apply","plan_id":id}})) => result,
    };
    match result {
        Ok(_) => {
            // Completed artifacts are gone; only unresolved reviews consume
            // suppression capacity, so healthy rotation can run indefinitely.
            state["seen"]
                .as_array_mut()
                .ok_or("MAINTENANCE_HISTORY")?
                .pop();
            save(c, &mut state, "completed", None)
        }
        Err(error) => save(c, &mut state, "retained", Some(&error)),
    }
}
pub(crate) async fn run(c: Arc<Core>) {
    loop {
        if let Err(error) = cycle(&c).await {
            tracing::warn!(error=%error, "file_maintenance_failed");
            let mut state = c.store.setting("file_maintenance_state");
            if !state.is_object() {
                state = json!({});
            }
            let _ = save(&c, &mut state, "attention_required", Some(&error));
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

#[cfg(all(test, windows))]
#[path = "file_maintenance_tests.rs"]
mod tests;
