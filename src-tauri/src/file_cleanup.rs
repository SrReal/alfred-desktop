//! Cleanup acts only on a reviewed manifest, after relocating the managed
//! artifact to an exclusive sibling. Native deletion uses object handles and
//! never follows an unverified recursive filesystem path.
use super::*;

fn ids(body: &Value) -> Result<Vec<String>, String> {
    let values = body["ids"].as_array().ok_or("CLEANUP_IDS")?;
    if values.is_empty() || values.len() > 100 {
        return Err("CLEANUP_ID_LIMIT".into());
    }
    let mut seen = std::collections::HashSet::new();
    values
        .iter()
        .map(|v| {
            let id = v.as_str().ok_or("CLEANUP_ID")?;
            uuid::Uuid::parse_str(id).map_err(|_| "CLEANUP_ID")?;
            if !seen.insert(id) {
                return Err("CLEANUP_DUPLICATE_ID".into());
            }
            Ok(id.to_owned())
        })
        .collect()
}
pub(super) fn managed(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let name = name.strip_prefix(".alfredpet-").unwrap_or(name);
    [".checkpoint", ".deleted", ".staging", ".purge"]
        .iter()
        .any(|suffix| {
            name.strip_suffix(suffix)
                .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
        })
}
fn snapshot(c: &Core, tree: &Tree, control: &Control) -> Result<Value, String> {
    let mut nodes = Vec::new();
    for node in &tree.nodes {
        let digest = if node.directory {
            Value::Null
        } else {
            json!(hash_file_limit(
                &mut node.file.try_clone().map_err(|_| "CLEANUP_HANDLE")?,
                MAX_BYTES,
                || control.check(c)
            )?)
        };
        nodes.push(json!({"relative":node.relative.to_str().ok_or("TREE_PATH_ENCODING")?,"object_id":identity(&node.file)?,"directory":node.directory,"readonly":node.readonly,"sha256":digest}));
    }
    Ok(json!(nodes))
}
fn plan(c: &Core, id: &str) -> Result<Value, String> {
    uuid::Uuid::parse_str(id).map_err(|_| "CLEANUP_PLAN_ID")?;
    let row = c
        .store
        .query("SELECT * FROM file_cleanup_plans WHERE id=?", &[json!(id)])?[0]
        .clone();
    if row.is_null() {
        return Err("CLEANUP_PLAN_NOT_FOUND".into());
    }
    Ok(row)
}
fn parse_plan(row: &Value) -> Result<Value, String> {
    serde_json::from_str(row["snapshot_json"].as_str().ok_or("CLEANUP_PLAN_JSON")?)
        .map_err(|_| "CLEANUP_PLAN_JSON".into())
}
fn summary(data: &Value) -> Result<Value, String> {
    let entries = data["entries"].as_array().ok_or("CLEANUP_ENTRIES")?;
    Ok(json!(entries.iter().map(|e| json!({"journal_id":e["journal_id"],"path":e["path"],"role":e["role"],"bytes":e["bytes"],"entries":e["nodes"].as_array().map_or(0,Vec::len),"forgets_recovery":e["forgets_recovery"],"manual_review":e["manual_review"]})).collect::<Vec<_>>()))
}
pub(super) fn review(c: &Core, body: &Value) -> Result<Value, String> {
    let row = plan(c, body["plan_id"].as_str().ok_or("CLEANUP_PLAN_ID")?)?;
    let data = parse_plan(&row)?;
    Ok(
        json!({"plan_id":row["id"],"state":row["state"],"completed":row["next_index"],"revalidate_policy":body["revalidate_policy"] == true,"action":"permanently delete the selected managed artifacts; no undo", "artifacts":summary(&data)?}),
    )
}
fn preview(c: &Core, body: &Value, control: &Control) -> Result<Value, String> {
    preview_selected(c, body, control, |_, _| true)
}
pub(super) fn preview_selected(
    c: &Core,
    body: &Value,
    control: &Control,
    select: impl Fn(&Value, &str) -> bool,
) -> Result<Value, String> {
    let mut entries = Vec::new();
    let mut skipped = Vec::new();
    let mut total_nodes = 0usize;
    let mut total_bytes = 0u64;
    for id in ids(body)? {
        control.check(c)?;
        let row = c
            .store
            .query("SELECT * FROM undo_records WHERE id=?", &[json!(id)])?[0]
            .clone();
        if row.is_null() {
            return Err("UNDO_NOT_FOUND".into());
        }
        let data = metadata(&row)?;
        let mut candidates = Vec::new();
        if let Some(path) = row["checkpoint"].as_str() {
            let root_id = if row["kind"] == "tree_copy" || row["kind"] == "tree_delete" {
                row["object_id"].clone()
            } else {
                data["checkpoint_id"].clone()
            };
            let nodes = if row["kind"] == "tree_copy" {
                data["result_nodes"].clone()
            } else {
                data["checkpoint_nodes"].clone()
            };
            let digest = if row["kind"] == "write" || row["kind"] == "delete" {
                row["checkpoint_hash"].clone()
            } else {
                row["expected_hash"].clone()
            };
            candidates.push((PathBuf::from(path), "checkpoint", root_id, nodes, digest));
        }
        if let Some(path) = data["undo_target"].as_str() {
            candidates.push((
                PathBuf::from(path),
                "parked_target",
                row["object_id"].clone(),
                data["result_nodes"].clone(),
                row["expected_hash"].clone(),
            ));
        }
        if let Some(path) = row["staging"].as_str() {
            candidates.push((
                PathBuf::from(path),
                "staging",
                data["stage_id"].clone(),
                Value::Null,
                Value::Null,
            ));
        }
        for (path, role, root_id, ownership, expected) in candidates {
            if !select(&row, role) {
                continue;
            }
            if !path.exists() {
                continue;
            }
            let result = (|| {
                if body["retention"] == true
                    && role != "staging"
                    && (expected.as_str().is_none()
                        || root_id.as_str().is_none()
                        || (path.is_dir() && ownership.as_array().is_none()))
                {
                    return Err("CLEANUP_MANUAL_REVIEW_REQUIRED".into());
                }
                if !managed(&path) {
                    return Err("CLEANUP_NOT_MANAGED".into());
                }
                if role != "staging" && row["state"] != "undone" && body["forget_undo"] != true {
                    return Err("CLEANUP_RECOVERY_RETAINED".into());
                }
                if row["state"] == "running" || row["state"] == "prepared" {
                    return Err("CLEANUP_ACTIVE_OPERATION".into());
                }
                let tree = Tree::capture(c, &path, false, control)?;
                let legacy = root_id.is_null();
                if legacy && body["adopt_legacy"] != true {
                    return Err("CLEANUP_LEGACY_REQUIRES_REVIEW".into());
                }
                if !legacy && root_id != tree.identity {
                    return Err("UNDO_OBJECT_REPLACED".into());
                }
                let actual = if row["kind"] == "write" || row["kind"] == "delete" {
                    raw_hash(
                        c,
                        &mut tree.root.try_clone().map_err(|_| "CLEANUP_HANDLE")?,
                        control,
                    )?
                } else {
                    tree.digest.clone()
                };
                let changed = expected.as_str().is_some_and(|s| s != actual)
                    || ownership.as_array().is_some() && ownership != tree.ownership()?;
                if changed && body["include_changed"] != true {
                    return Err("CLEANUP_ARTIFACT_CHANGED".into());
                }
                total_nodes += tree.nodes.len();
                total_bytes = total_bytes.saturating_add(tree.bytes);
                if total_nodes > MAX_NODES || total_bytes > MAX_CHECKPOINT_BYTES {
                    return Err("CLEANUP_PLAN_LIMIT".into());
                }
                let work = path
                    .parent()
                    .ok_or("TREE_PARENT")?
                    .join(format!(".alfredpet-{}.purge", storage::id()));
                c.path(work.to_str().ok_or("TREE_PATH_ENCODING")?, true)?;
                Ok::<Value, String>(
                    json!({"journal_id":id,"original_state":row["state"],"role":role,"journal_effect":if role == "staging" {"retain"} else {"forget"},"path":path,"work_path":work,"root_id":tree.identity,"digest":tree.digest,"bytes":tree.bytes,"nodes":snapshot(c,&tree,control)?,"forgets_recovery":role != "staging" && row["state"] != "undone","manual_review":if legacy {"legacy ownership adopted explicitly"} else if changed {"changed content adopted explicitly"} else if expected.is_null() {"partial staging"} else {"recorded checkpoint"}}),
                )
            })();
            control.check(c)?;
            match result {
                Ok(entry) => entries.push(entry),
                Err(error) if error == "CLEANUP_PLAN_LIMIT" => return Err(error),
                Err(error) => skipped.push(json!({"journal_id":id,"path":path,"reason":error})),
            }
        }
    }
    if entries.is_empty() {
        return Ok(json!({"plan_id":Value::Null,"artifacts":[],"skipped":skipped}));
    }
    let id = storage::id();
    let data = json!({"roots":c.roots(),"entries":entries});
    if body["maintenance_preview"] != true {
        c.store.execute(
        "INSERT INTO file_cleanup_plans(id,snapshot_json,created_at,updated_at) VALUES(?,?,?,?)",
        &[
            json!(id),
            json!(data.to_string()),
            json!(storage::now()),
            json!(storage::now()),
        ],
    )?;
    }
    let mut output = json!({"plan_id":id,"artifacts":summary(&data)?,"skipped":skipped});
    if body["maintenance_preview"] == true {
        output["snapshot"] = data;
    }
    Ok(output)
}

#[cfg(windows)]
pub(super) fn delete_handle(file: &File) -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{
            FileDispositionInfoEx, SetFileInformationByHandle, FILE_DISPOSITION_FLAG_DELETE,
            FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_INFO_EX,
            FILE_DISPOSITION_INFO_EX_FLAGS,
        },
    };
    let info = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_INFO_EX_FLAGS(
            FILE_DISPOSITION_FLAG_DELETE.0 | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE.0,
        ),
    };
    unsafe {
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileDispositionInfoEx,
            (&info as *const FILE_DISPOSITION_INFO_EX).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    }
    .map_err(|e| format!("CLEANUP_NATIVE_DELETE: 0x{:08x}", e.code().0 as u32))
}
#[cfg(not(windows))]
pub(super) fn delete_handle(_: &File) -> Result<(), String> {
    Err("WINDOWS_11_REQUIRED".into())
}
fn purge(c: &Core, entry: &Value, control: &Control) -> Result<(), String> {
    let work = c.path(
        entry["work_path"].as_str().ok_or("CLEANUP_WORK_PATH")?,
        true,
    )?;
    if !work.exists() {
        return Ok(());
    }
    let mut tree = Tree::capture(c, &work, true, control)?;
    if entry["root_id"] != tree.identity {
        return Err("UNDO_OBJECT_REPLACED".into());
    }
    validate_remaining(&tree, entry, control, c)?;
    let mut handles = Vec::new();
    for node in &tree.nodes {
        control.check(c)?;
        let pinned = if node.relative.as_os_str().is_empty() {
            tree.root.try_clone().map_err(|_| "CLEANUP_HANDLE")?
        } else {
            open(&work.join(&node.relative), true, false)?
        };
        if identity(&node.file)? != identity(&pinned)? {
            return Err("CLEANUP_ENTRY_CHANGED".into());
        }
        handles.push((node.relative.components().count(), pinned));
    }
    // Verify again after every remaining node is pinned against write/delete.
    if hash(c, &work, control)? != tree.digest {
        return Err("CLEANUP_CHANGED_DURING_PREFLIGHT".into());
    }
    let _parents = std::mem::take(&mut tree._parents);
    drop(tree);
    handles.sort_by_key(|h| std::cmp::Reverse(h.0));
    for (_, handle) in handles {
        control.check(c)?;
        delete_handle(&handle)?;
        drop(handle);
        #[cfg(all(test, windows))]
        crash_tests::at(c, "cleanup_child_deleted");
    }
    if work.exists() {
        return Err("CLEANUP_NOT_EMPTY".into());
    }
    Ok(())
}
fn validate_remaining(
    tree: &Tree,
    entry: &Value,
    control: &Control,
    c: &Core,
) -> Result<(), String> {
    if entry["root_id"] != tree.identity {
        return Err("UNDO_OBJECT_REPLACED".into());
    }
    let expected = entry["nodes"].as_array().ok_or("CLEANUP_NODES")?;
    for node in &tree.nodes {
        control.check(c)?;
        let relative = node.relative.to_str().ok_or("TREE_PATH_ENCODING")?;
        let prior = expected
            .iter()
            .find(|v| v["relative"] == relative)
            .ok_or("CLEANUP_NEW_ENTRY")?;
        if prior["object_id"] != identity(&node.file)?
            || prior["directory"] != node.directory
            || prior["readonly"] != node.readonly
        {
            return Err("CLEANUP_ENTRY_CHANGED".into());
        }
        if !node.directory
            && prior["sha256"]
                != hash_file_limit(
                    &mut node.file.try_clone().map_err(|_| "CLEANUP_HANDLE")?,
                    MAX_BYTES,
                    || control.check(c),
                )?
        {
            return Err("CLEANUP_ENTRY_CHANGED".into());
        }
    }
    Ok(())
}

fn finish_entry(c: &Core, id: &str, entry: &Value, next: usize) -> Result<(), String> {
    let mut db = c.store.0.lock().map_err(|_| "SQLITE_LOCK")?;
    let tx = db.transaction().map_err(|e| e.to_string())?;
    if entry["role"] == "staging" && entry["journal_effect"] == "retain" {
        // This partial output is separate from the checkpoint used for undo.
        // Clearing the reference and advancing the cursor are atomic, including
        // retries after native deletion completed before the SQLite commit.
        tx.execute(
            "UPDATE undo_records SET staging=NULL WHERE id=? AND staging=?",
            rusqlite::params![entry["journal_id"].as_str(), entry["path"].as_str()],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.execute(
        "UPDATE file_cleanup_plans SET next_index=?,updated_at=? WHERE id=?",
        rusqlite::params![next as i64, storage::now(), id],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}
fn apply(c: &Core, id: &str, control: &Control) -> Result<Value, String> {
    let row = plan(c, id)?;
    let mut data = parse_plan(&row)?;
    if row["state"] == "completed" {
        return Ok(json!({"plan_id":id,"state":"completed"}));
    }
    if !data["maintenance_policy"].is_null() {
        control.bind_maintenance(c, &data["maintenance_policy"])?;
    }
    if data["roots"] != json!(c.roots()) {
        return Err("CLEANUP_SCOPE_CHANGED".into());
    }
    if row["state"] == "prepared" {
        let created = chrono::DateTime::parse_from_rfc3339(
            row["created_at"].as_str().ok_or("CLEANUP_CREATED")?,
        )
        .map_err(|_| "CLEANUP_CREATED")?;
        if chrono::Utc::now()
            .signed_duration_since(created)
            .num_minutes()
            >= 60
        {
            return Err("CLEANUP_PLAN_EXPIRED".into());
        }
    }
    let entries = data["entries"].as_array().ok_or("CLEANUP_ENTRIES")?.clone();
    let mut next = row["next_index"].as_u64().ok_or("CLEANUP_CURSOR")? as usize;
    if next > entries.len() {
        return Err("CLEANUP_CURSOR".into());
    }
    // All untouched artifacts must still satisfy the reviewed manifest before
    // any artifact is made irrecoverable in this attempt.
    for entry in &entries[next..] {
        let work = Path::new(entry["work_path"].as_str().ok_or("CLEANUP_WORK_PATH")?);
        let journal = c.store.query(
            "SELECT state,staging FROM undo_records WHERE id=?",
            &[entry["journal_id"].clone()],
        )?[0]
            .clone();
        let state = &journal["state"];
        let forgotten_by_this_plan = entries.iter().any(|prior| {
            prior["journal_id"] == entry["journal_id"]
                && prior["erasing"] == true
                && prior["journal_effect"] != "retain"
        });
        if *state != entry["original_state"] && !(state == "forgotten" && forgotten_by_this_plan) {
            return Err("CLEANUP_JOURNAL_CHANGED".into());
        }
        if entry["role"] == "staging"
            && entry["journal_effect"] == "retain"
            && journal["staging"] != entry["path"]
            && (entry["erasing"] != true
                || !journal["staging"].is_null()
                || work.exists()
                || Path::new(entry["path"].as_str().ok_or("CLEANUP_PATH")?).exists())
        {
            return Err("CLEANUP_JOURNAL_CHANGED".into());
        }
        if work.exists() {
            let tree = Tree::capture(c, work, false, control)?;
            validate_remaining(&tree, entry, control, c)?;
            if entry["erasing"] != true
                && (entry["digest"] != tree.digest
                    || entry["nodes"] != snapshot(c, &tree, control)?)
            {
                return Err("CLEANUP_ARTIFACT_CHANGED".into());
            }
            continue;
        }
        if entry["erasing"] == true
            && !Path::new(entry["path"].as_str().ok_or("CLEANUP_PATH")?).exists()
        {
            continue;
        }
        let path = c.path(entry["path"].as_str().ok_or("CLEANUP_PATH")?, false)?;
        let current = Tree::capture(c, &path, false, control)?;
        if entry["root_id"] != current.identity
            || entry["digest"] != current.digest
            || entry["nodes"] != snapshot(c, &current, control)?
        {
            return Err("CLEANUP_ARTIFACT_CHANGED".into());
        }
    }
    let claimed = c.store.execute("UPDATE file_cleanup_plans SET state='running',error=NULL,updated_at=? WHERE id=? AND state IN ('prepared','interrupted','failed')", &[json!(storage::now()),json!(id)])?;
    if claimed != 1 {
        return Err("CLEANUP_ALREADY_RUNNING".into());
    }
    let result = (|| {
        while next < entries.len() {
            control.check(c)?;
            let entry = &entries[next];
            let original = Path::new(entry["path"].as_str().ok_or("CLEANUP_PATH")?);
            let work = c.path(
                entry["work_path"].as_str().ok_or("CLEANUP_WORK_PATH")?,
                true,
            )?;
            if entry["erasing"] == true && !work.exists() && !original.exists() {
                next += 1;
                finish_entry(c, id, entry, next)?;
                continue;
            }
            if !work.exists() {
                let source = c.path(entry["path"].as_str().ok_or("CLEANUP_PATH")?, false)?;
                relocate(
                    c,
                    &source,
                    &work,
                    entry["digest"].as_str().ok_or("CLEANUP_DIGEST")?,
                    entry["root_id"].as_str().ok_or("CLEANUP_ROOT_ID")?,
                    control,
                )?;
            }
            let tree = Tree::capture(c, &work, false, control)?;
            validate_remaining(&tree, entry, control, c)?;
            drop(tree);
            data["entries"][next]["erasing"] = json!(true);
            {
                let mut db = c.store.0.lock().map_err(|_| "SQLITE_LOCK")?;
                let tx = db.transaction().map_err(|e| e.to_string())?;
                if entry["journal_effect"] != "retain" {
                    tx.execute(
                        "UPDATE undo_records SET state='forgotten',updated_at=? WHERE id=?",
                        rusqlite::params![
                            storage::now(),
                            entry["journal_id"].as_str().ok_or("CLEANUP_JOURNAL")?
                        ],
                    )
                    .map_err(|e| e.to_string())?;
                }
                tx.execute(
                    "UPDATE file_cleanup_plans SET snapshot_json=?,updated_at=? WHERE id=?",
                    rusqlite::params![data.to_string(), storage::now(), id],
                )
                .map_err(|e| e.to_string())?;
                tx.commit().map_err(|e| e.to_string())?;
            }
            #[cfg(all(test, windows))]
            crash_tests::at(c, "cleanup_erasing_committed");
            purge(c, entry, control)?;
            next += 1;
            finish_entry(c, id, entry, next)?;
        }
        c.store.execute(
            "UPDATE file_cleanup_plans SET state='completed',updated_at=? WHERE id=?",
            &[json!(storage::now()), json!(id)],
        )?;
        Ok::<(), String>(())
    })();
    if let Err(error) = result {
        c.store.execute(
            "UPDATE file_cleanup_plans SET state='interrupted',error=?,updated_at=? WHERE id=?",
            &[json!(error), json!(storage::now()), json!(id)],
        )?;
        return Err(format!(
            "CLEANUP_STOPPED: plan_id={id}; completed={next}/{}; {error}",
            entries.len()
        ));
    }
    Ok(json!({"plan_id":id,"state":"completed","completed":next,"total":entries.len()}))
}
pub(super) fn dispatch(c: &Core, body: &Value, control: &Control) -> Result<Value, String> {
    control.check(c)?;
    if body["action"] == "apply" && body["revalidate_policy"] == true {
        let id = body["plan_id"].as_str().ok_or("CLEANUP_PLAN_ID")?;
        let row = plan(c, id)?;
        if row["state"] != "completed" {
            let mut data = parse_plan(&row)?;
            if data["maintenance_policy"].is_null() {
                return Err("CLEANUP_NOT_MAINTENANCE".into());
            }
            if data["roots"] != json!(c.roots()) {
                return Err("CLEANUP_SCOPE_CHANGED".into());
            }
            // Explicit manual L3 retry can resume a cancelled periodic plan
            // while automation is disabled. It never adopts changed objects,
            // restores revoked roots or relaxes the manifest/state guards.
            data["maintenance_policy"] = maintenance::policy(c);
            let changed = c.store.execute("UPDATE file_cleanup_plans SET snapshot_json=?,created_at=?,updated_at=? WHERE id=? AND snapshot_json=? AND state IN ('prepared','interrupted','failed')", &[json!(data.to_string()),json!(storage::now()),json!(storage::now()),json!(id),row["snapshot_json"].clone()])?;
            if changed != 1 {
                return Err("CLEANUP_ALREADY_RUNNING".into());
            }
        }
    }
    match body["action"].as_str() {
        Some("preview") => preview(c, body, control),
        Some("apply") => apply(
            c,
            body["plan_id"].as_str().ok_or("CLEANUP_PLAN_ID")?,
            control,
        ),
        _ => Err("CLEANUP_ACTION".into()),
    }
}

#[cfg(all(test, windows))]
#[path = "file_cleanup_tests.rs"]
mod tests;

pub(super) fn validate_retention(value: &Value) -> Result<(), String> {
    if value.is_null()
        || value
            .as_u64()
            .is_some_and(|days| (1..=3650).contains(&days))
    {
        Ok(())
    } else {
        Err("CLEANUP_RETENTION_DAYS".into())
    }
}
pub(super) fn retention(c: &Core, control: &Control) -> Result<Value, String> {
    let days = c.store.setting("file_checkpoint_days");
    validate_retention(&days)?;
    let Some(days) = days.as_u64() else {
        return Err("CLEANUP_RETENTION_NOT_CONFIGURED".into());
    };
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(days as i64))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let saved = c.store.setting("file_retention_cursor");
    let cursor = if saved["days"] == days {
        saved["cursor"].clone()
    } else {
        Value::Null
    };
    let (ids, cursor) = maintenance::page(c, &cutoff, &cursor, false)?;
    c.store.set_setting(
        "file_retention_cursor",
        &json!({"days":days,"cursor":cursor}),
    )?;
    if ids.is_empty() {
        return Ok(json!({"plan_id":Value::Null,"artifacts":[],"skipped":[],"days":days}));
    }
    let mut output = preview_selected(
        c,
        &json!({"ids":ids,"retention":true}),
        control,
        |_, role| role != "staging",
    )?;
    output["days"] = json!(days);
    output["more"] = json!(!cursor.is_null());
    Ok(output)
}
