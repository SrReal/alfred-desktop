use crate::{
    file_tree::{self, Control},
    modules,
    runtime::Core,
    storage,
};
use serde_json::{json, Value};
use std::path::Path;

#[cfg(test)]
pub fn prepare(
    c: &Core,
    kind: &str,
    path: &Path,
    destination: Option<&Path>,
    checkpoint: Option<&Path>,
    expected: &str,
) -> Result<String, String> {
    let id = storage::id();
    let checkpoint_hash = checkpoint.map(modules::hash).transpose()?;
    c.store.execute("INSERT INTO undo_records(id,kind,path,destination,checkpoint,expected_hash,checkpoint_hash,state,created_at,updated_at) VALUES(?,?,?,?,?,?,?,'prepared',?,?)",&[json!(id),json!(kind),json!(path),json!(destination),json!(checkpoint),json!(expected),json!(checkpoint_hash),json!(storage::now()),json!(storage::now())])?;
    Ok(id)
}
pub fn commit(c: &Core, id: &str) -> Result<(), String> {
    c.store.execute(
        "UPDATE undo_records SET state='available',updated_at=? WHERE id=? AND state='prepared'",
        &[json!(storage::now()), json!(id)],
    )?;
    Ok(())
}
pub fn prepare_tree(
    c: &Core,
    kind: &str,
    path: &Path,
    destination: Option<&Path>,
    checkpoint: Option<&Path>,
    expected: &str,
) -> Result<String, String> {
    let id = storage::id();
    c.store.execute("INSERT INTO undo_records(id,kind,path,destination,checkpoint,expected_hash,checkpoint_hash,state,created_at,updated_at) VALUES(?,?,?,?,?,?,?,'prepared',?,?)", &[json!(id),json!(kind),json!(path),json!(destination),json!(checkpoint),json!(expected),if kind == "tree_delete" { json!(expected) } else { Value::Null },json!(storage::now()),json!(storage::now())])?;
    Ok(id)
}
pub fn recover(c: &Core) -> Result<(), String> {
    crate::window_undo::recover(c)?;
    let rows = c.store.query(
        "SELECT * FROM undo_records WHERE state IN ('prepared','running')",
        &[],
    )?;
    for row in rows.as_array().ok_or("UNDO_RECORDS")? {
        if row["kind"] == "window_change" {
            continue;
        }
        if row["kind"] == "archive_batch" {
            c.store.execute(
                "UPDATE undo_records SET state='blocked',updated_at=? WHERE id=?",
                &[json!(storage::now()), row["id"].clone()],
            )?;
            continue;
        }
        if row["kind"] == "tree_cross_move" {
            file_tree::recover_cross(c, row)?;
            continue;
        }
        let valid = if row["state"] == "running" {
            false
        } else {
            expected_state(c, row).is_ok()
        };
        c.store.execute(
            "UPDATE undo_records SET state=?,updated_at=? WHERE id=?",
            &[
                json!(if valid { "available" } else { "blocked" }),
                json!(storage::now()),
                row["id"].clone(),
            ],
        )?;
    }
    Ok(())
}
fn field<'a>(row: &'a Value, key: &str) -> Result<&'a str, String> {
    row[key].as_str().ok_or(format!("UNDO_FIELD_{key}"))
}
fn expected_state(c: &Core, row: &Value) -> Result<(), String> {
    if row["kind"].as_str().is_some_and(|s| s.starts_with("tree_")) {
        return file_tree::expected_state(c, row, &Control::default());
    }
    let original = c.path(field(row, "path")?, true)?;
    if !row["checkpoint"].is_null() {
        let checkpoint = c.path(field(row, "checkpoint")?, false)?;
        if modules::hash(&checkpoint)? != field(row, "checkpoint_hash")? {
            return Err("UNDO_CHECKPOINT_CHANGED".into());
        }
    }
    if row["kind"] == "delete" {
        if original.exists() {
            return Err("UNDO_DESTINATION_EXISTS".into());
        }
        let checkpoint = c.path(field(row, "checkpoint")?, false)?;
        if modules::hash(&checkpoint)? != field(row, "expected_hash")? {
            return Err("UNDO_CHECKPOINT_CHANGED".into());
        }
    } else {
        let current = if row["kind"] == "move" || row["kind"] == "copy" {
            c.path(field(row, "destination")?, true)?
        } else {
            original
        };
        if modules::hash(&current)? != field(row, "expected_hash")? {
            return Err("UNDO_FILE_CHANGED: preserve later edits".into());
        }
    }
    Ok(())
}
pub fn execute(c: &Core, id: &str) -> Result<Value, String> {
    execute_control(c, id, &Control::default())
}
pub fn execute_control(c: &Core, id: &str, control: &Control) -> Result<Value, String> {
    execute_request(c, id, false, control)
}
pub fn can_revalidate(row: &Value) -> bool {
    row["object_id"].as_str().is_some_and(|id| !id.is_empty())
        && matches!(
            row["kind"].as_str(),
            Some("tree_copy" | "tree_move" | "tree_delete" | "tree_cross_move" | "write")
        )
}
pub fn history(c: &Core) -> Result<Value, String> {
    let mut rows = c.store.query("SELECT id,kind,path,destination,checkpoint,staging,object_id,json_extract(metadata_json,'$.undo_target') AS parked_target,json_extract(metadata_json,'$.phase') AS phase,COALESCE(json_extract(metadata_json,'$.undo_failure.error'),json_extract(metadata_json,'$.write_failure.error')) AS failure_reason,state,created_at,updated_at FROM undo_records WHERE kind!='window_change' ORDER BY created_at DESC LIMIT 100", &[])?;
    for row in rows.as_array_mut().ok_or("UNDO_RECORDS")? {
        row["revalidation_available"] = json!(row["state"] == "blocked" && can_revalidate(row));
        row.as_object_mut()
            .ok_or("UNDO_RECORD")?
            .remove("object_id");
    }
    Ok(rows)
}
pub fn execute_request(
    c: &Core,
    id: &str,
    revalidate: bool,
    control: &Control,
) -> Result<Value, String> {
    control.check(c)?;
    let rows = c
        .store
        .query("SELECT * FROM undo_records WHERE id=?", &[json!(id)])?;
    let mut row = rows[0].clone();
    if row.is_null() {
        return Err("UNDO_NOT_FOUND".into());
    }
    if row["kind"] == "window_change" {
        return Err("WINDOW_JOURNAL_REQUIRES_WINDOW_UNDO".into());
    }
    let retry = row["state"] == "blocked" && revalidate;
    if row["state"] != "available" && !retry {
        return Err("UNDO_NOT_AVAILABLE".into());
    }
    if retry && !can_revalidate(&row) {
        return Err("UNDO_REVALIDATION_REQUIRES_NATIVE_JOURNAL".into());
    }
    let previous = row["state"].clone();
    let old_metadata = row["metadata_json"]
        .as_str()
        .ok_or("FILE_METADATA_JOURNAL")?;
    let mut data: Value =
        serde_json::from_str(old_metadata).map_err(|_| "FILE_METADATA_JOURNAL")?;
    if !data.is_object() {
        return Err("FILE_METADATA_JOURNAL".into());
    }
    let claimed = c.store.execute(
        "UPDATE undo_records SET state='running',updated_at=? WHERE id=? AND state=? AND metadata_json=?",
        &[json!(storage::now()), json!(id),previous.clone(),json!(old_metadata)],
    )?;
    if claimed != 1 {
        return Err("UNDO_ALREADY_CLAIMED".into());
    }
    let result = (|| {
        // Revalidate from the previous metadata, not the flag just written by
        // this attempt. A first retry must not infer a completed undo from intent.
        let completed = if retry {
            file_tree::revalidate_undo(c, &row, control)?
        } else {
            None
        };
        if let Some(_verified) = completed {
            control.check(c)?;
            finish(c, id, true)?;
            return Ok(
                json!({"restored":row["path"],"undo_id":id,"state":"undone","reconciled":true}),
            );
        }
        if can_revalidate(&row) {
            // A failed preflight must not turn a never-applied journal into
            // evidence of an inverse mutation. Persist only after validation.
            data["undo_started"] = json!(true);
            let changed = c.store.execute("UPDATE undo_records SET metadata_json=?,updated_at=? WHERE id=? AND state='running' AND metadata_json=?", &[json!(data.to_string()),json!(storage::now()),json!(id),row["metadata_json"].clone()])?;
            if changed != 1 {
                return Err("UNDO_JOURNAL_CHANGED".into());
            }
            row["metadata_json"] = json!(data.to_string());
        }
        if row["kind"] == "write" {
            file_tree::restore_write(c, &row, control)?;
            finish(c, id, false)?;
            return Ok(json!({"restored":row["path"],"undo_id":id,"state":"undone"}));
        }
        if row["kind"].as_str().is_some_and(|s| s.starts_with("tree_")) {
            file_tree::restore(c, &row, control)?;
            finish(c, id, true)?;
            return Ok(json!({"restored":row["path"],"undo_id":id,"state":"undone"}));
        }
        expected_state(c, &row)?;
        control.check(c)?;
        let original = c.path(field(&row, "path")?, true)?;
        match field(&row, "kind")? {
            "delete" => {
                let checkpoint = c.path(field(&row, "checkpoint")?, false)?;
                if !checkpoint.is_file() {
                    return Err("UNDO_CHECKPOINT_MISSING".into());
                }
                if row["kind"] == "delete" {
                    let mut destination = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&original)
                        .map_err(|_| "UNDO_DESTINATION_EXISTS")?;
                    let mut source =
                        std::fs::File::open(checkpoint).map_err(|_| "UNDO_CHECKPOINT_MISSING")?;
                    std::io::copy(&mut source, &mut destination).map_err(|_| "UNDO_RESTORE")?;
                } else {
                    std::fs::copy(checkpoint, &original).map_err(|_| "UNDO_RESTORE")?;
                }
            }
            "copy" => {
                let destination = c.path(field(&row, "destination")?, true)?;
                std::fs::remove_file(destination).map_err(|_| "UNDO_REMOVE")?;
            }
            "move" => {
                if original.exists() {
                    return Err("UNDO_DESTINATION_EXISTS".into());
                }
                let destination = c.path(field(&row, "destination")?, true)?;
                modules::move_no_replace(&destination, &original)?;
            }
            _ => return Err("UNDO_KIND".into()),
        }
        finish(c, id, false)?;
        Ok(json!({"restored":original,"undo_id":id,"state":"undone"}))
    })();
    if result.is_err() {
        let _ = c.store.execute(
            "UPDATE undo_records SET state=?,updated_at=? WHERE id=? AND state='running'",
            &[previous, json!(storage::now()), json!(id)],
        );
    }
    result
}

fn finish(c: &Core, id: &str, clear_stage: bool) -> Result<(), String> {
    #[cfg(all(test, windows))]
    file_tree::undo_crash_point(c);
    let changed = c.store.execute(
        "UPDATE undo_records SET state='undone',staging=CASE WHEN ? THEN NULL ELSE staging END,updated_at=? WHERE id=? AND state='running'",
        &[json!(clear_stage),json!(storage::now()),json!(id)],
    )?;
    if changed != 1 {
        return Err("UNDO_JOURNAL_CHANGED".into());
    }
    Ok(())
}

fn ids(body: &Value) -> Result<Vec<String>, String> {
    let array = body["tokens"]
        .as_array()
        .ok_or("ROLLBACK_TOKENS_REQUIRED")?;
    if array.is_empty() || array.len() > 100 {
        return Err("ROLLBACK_TOKEN_LIMIT".into());
    }
    let mut seen = std::collections::HashSet::new();
    array
        .iter()
        .map(|v| {
            let id = v.as_str().ok_or("ROLLBACK_TOKEN_INVALID")?;
            uuid::Uuid::parse_str(id).map_err(|_| "ROLLBACK_TOKEN_INVALID")?;
            if !seen.insert(id) {
                return Err("ROLLBACK_TOKEN_DUPLICATE".into());
            }
            Ok(id.to_owned())
        })
        .collect()
}
fn rollback_row(c: &Core, id: &str) -> Result<Value, String> {
    uuid::Uuid::parse_str(id).map_err(|_| "ROLLBACK_ID_INVALID")?;
    let row = c
        .store
        .query("SELECT * FROM file_rollbacks WHERE id=?", &[json!(id)])?[0]
        .clone();
    if row.is_null() {
        return Err("ROLLBACK_NOT_FOUND".into());
    }
    Ok(row)
}
fn rollback_ids(c: &Core, body: &Value) -> Result<Vec<String>, String> {
    let object = body.as_object().ok_or("ROLLBACK_BODY")?;
    if object.len() != 1 {
        return Err("ROLLBACK_BODY".into());
    }
    if let Some(id) = body["id"].as_str() {
        let row = rollback_row(c, id)?;
        serde_json::from_str(field(&row, "tokens_json")?).map_err(|_| "ROLLBACK_JOURNAL".into())
    } else {
        ids(body)
    }
}
pub fn review(c: &Core, tool: &str, input: &Value) -> Result<Value, String> {
    let mut ids = if tool == "files.undo" {
        vec![input["id"].as_str().ok_or("UNDO_ID_REQUIRED")?.to_owned()]
    } else {
        rollback_ids(c, &input["body"])?
    };
    if tool == "files.rollback" {
        ids.reverse();
    }
    let mut operations = Vec::new();
    for id in ids {
        let row = c
            .store
            .query("SELECT * FROM undo_records WHERE id=?", &[json!(id)])?[0]
            .clone();
        if row.is_null() {
            return Err("UNDO_NOT_FOUND".into());
        }
        let data: Value = serde_json::from_str(row["metadata_json"].as_str().unwrap_or("{}"))
            .map_err(|_| "FILE_METADATA_JOURNAL")?;
        operations.push(json!({"journal_id":id,"kind":row["kind"],"original":row["path"],"destination":row["destination"],"checkpoint":row["checkpoint"],"state":row["state"],"revalidate_blocked":tool == "files.undo" && input["revalidate"] == true,"write_failure":data["write_failure"],"undo_failure":data["undo_failure"],"checkpoint_complete":data["checkpoint_complete"],"validation":"current path permissions, object identity and recorded contents; preserve conflicts"}));
    }
    Ok(
        json!({"request":input,"operations_in_undo_order":operations,"on_conflict":"stop and retain completed progress"}),
    )
}
pub fn rollback(c: &Core, body: &Value, control: &Control) -> Result<Value, String> {
    control.check(c)?;
    let tokens = rollback_ids(c, body)?;
    let id = if let Some(id) = body["id"].as_str() {
        id.to_owned()
    } else {
        let mut previous = String::new();
        for token in &tokens {
            let row = c.store.query(
                "SELECT state,created_at FROM undo_records WHERE id=?",
                &[json!(token)],
            )?[0]
                .clone();
            if row["state"] != "available" {
                return Err("ROLLBACK_TOKEN_NOT_AVAILABLE".into());
            }
            let created = field(&row, "created_at")?.to_owned();
            if created < previous {
                return Err("ROLLBACK_REQUIRES_CHRONOLOGICAL_TOKENS".into());
            }
            previous = created;
        }
        let id = storage::id();
        c.store.execute(
            "INSERT INTO file_rollbacks(id,tokens_json,created_at,updated_at) VALUES(?,?,?,?)",
            &[
                json!(id),
                json!(serde_json::to_string(&tokens).map_err(|_| "ROLLBACK_JSON")?),
                json!(storage::now()),
                json!(storage::now()),
            ],
        )?;
        id
    };
    let row = rollback_row(c, &id)?;
    if row["state"] == "completed" {
        return Ok(
            json!({"id":id,"state":"completed","completed":tokens.len(),"total":tokens.len()}),
        );
    }
    let claimed = c.store.execute("UPDATE file_rollbacks SET state='running',error=NULL,updated_at=? WHERE id=? AND state IN ('prepared','blocked','interrupted')", &[json!(storage::now()),json!(id)])?;
    if claimed != 1 {
        return Err("ROLLBACK_ALREADY_RUNNING".into());
    }
    let mut next = row["next_index"].as_u64().ok_or("ROLLBACK_INDEX")? as usize;
    let result = (|| {
        while next < tokens.len() {
            control.check(c)?;
            let token = &tokens[tokens.len() - 1 - next];
            let row = c
                .store
                .query("SELECT state FROM undo_records WHERE id=?", &[json!(token)])?[0]
                .clone();
            // Reconcile a crash after undo committed but before the group cursor.
            if row["state"] != "undone" {
                execute_control(c, token, control)?;
            }
            next += 1;
            c.store.execute("UPDATE file_rollbacks SET next_index=?,updated_at=? WHERE id=? AND state='running'", &[json!(next),json!(storage::now()),json!(id)])?;
        }
        c.store.execute(
            "UPDATE file_rollbacks SET state='completed',updated_at=? WHERE id=?",
            &[json!(storage::now()), json!(id)],
        )?;
        Ok::<(), String>(())
    })();
    match result {
        Ok(()) => Ok(json!({"id":id,"state":"completed","completed":next,"total":tokens.len()})),
        Err(error) => {
            c.store.execute(
                "UPDATE file_rollbacks SET state='blocked',error=?,updated_at=? WHERE id=?",
                &[json!(error), json!(storage::now()), json!(id)],
            )?;
            Err(format!(
                "ROLLBACK_STOPPED: rollback_id={id}; completed={next}/{}; {error}",
                tokens.len()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::PathBuf, sync::Arc};
    #[tokio::test]
    async fn file_undo_conflicts_one_use_copy_move_delete_and_recovery() {
        let root = std::env::temp_dir().join(format!("ap-undo-{}", storage::id()));
        let c = Arc::new(
            Core::new(
                root.clone(),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        let file = root.join("temp/source.txt");
        std::fs::write(&file, "original").unwrap();
        let write = modules::dispatch(
            &c,
            "files.write",
            &json!({"path":file,"text":"replacement"}),
        )
        .await
        .unwrap();
        std::fs::write(&file, "later edit").unwrap();
        let error = execute(&c, write["undo_token"].as_str().unwrap()).unwrap_err();
        assert!(error.starts_with("UNDO_FILE_CHANGED"), "{error}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "later edit");
        std::fs::write(&file, "replacement").unwrap();
        execute(&c, write["undo_token"].as_str().unwrap()).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "original");
        assert_eq!(
            execute(&c, write["undo_token"].as_str().unwrap()).unwrap_err(),
            "UNDO_NOT_AVAILABLE"
        );
        for action in ["copy", "move"] {
            let destination = root.join(format!("temp/{action}.txt"));
            let result = modules::dispatch(
                &c,
                &format!("files.{action}"),
                &json!({"path":file,"destination":destination}),
            )
            .await
            .unwrap();
            execute(&c, result["undo_token"].as_str().unwrap()).unwrap();
            assert!(file.exists());
            assert!(!destination.exists());
        }
        let deleted = modules::dispatch(&c, "files.delete", &json!({"path":file}))
            .await
            .unwrap();
        assert!(!file.exists());
        std::fs::write(&file, "another file").unwrap();
        assert_eq!(
            execute(&c, deleted["undo_token"].as_str().unwrap()).unwrap_err(),
            "UNDO_DESTINATION_EXISTS"
        );
        std::fs::remove_file(&file).unwrap();
        execute(&c, deleted["undo_token"].as_str().unwrap()).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "original");
        let created = root.join("temp/new.txt");
        let result = modules::dispatch(&c, "files.write", &json!({"path":created,"text":"new"}))
            .await
            .unwrap();
        c.store
            .execute(
                "UPDATE undo_records SET state='prepared' WHERE id=?",
                &[result["undo_token"].clone()],
            )
            .unwrap();
        recover(&c).unwrap();
        execute(&c, result["undo_token"].as_str().unwrap()).unwrap();
        assert!(!created.exists());
        assert_eq!(
            crate::policy::classify("files.undo", &json!({})),
            crate::policy::Risk::Mandatory
        );
        drop(c);
        std::fs::remove_dir_all(root).unwrap();
    }
}
