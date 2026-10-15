use super::*;
use rusqlite::params;

fn relocate(value: &mut Value, old: &Path, new: &Path) {
    match value {
        Value::String(s) => {
            let path = Path::new(s);
            if let Ok(relative) = dunce::simplified(path).strip_prefix(dunce::simplified(old)) {
                *s = new.join(relative).to_string_lossy().into_owned();
            }
        }
        Value::Array(values) => {
            for v in values {
                relocate(v, old, new);
            }
        }
        Value::Object(values) => {
            for v in values.values_mut() {
                relocate(v, old, new);
            }
        }
        _ => {}
    }
}
pub(super) fn prepare(stage: &Path, target: &Path, manifest: &archive::Manifest) -> Result<()> {
    let db = archive::checked_db(&stage.join("data/alfredpet.db"), stage)?;
    let old = Path::new(&manifest.source_root);
    // Only structured paths are rebased. Messages, memories and user prose
    // remain byte-for-byte as recorded; they are never search/replaced.
    for (table, column) in [("attachments", "path"), ("custom_tools", "install_path")] {
        let rows = db
            .prepare(&format!("SELECT id,{column} FROM {table}"))?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (id, path) in rows {
            let mut value = json!(path);
            relocate(&mut value, old, target);
            db.execute(
                &format!("UPDATE {table} SET {column}=? WHERE id=?"),
                params![value.as_str(), id],
            )?;
        }
    }
    let rows = db
        .prepare("SELECT key,json_value FROM settings")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (key, raw) in rows {
        // Session identities, undo receipts, tokens, grants and native bindings
        // cannot be transplanted. Keep explicit user preferences and drafts.
        if ![
            "privacy",
            "dnd",
            "attention_audio",
            "claude_model",
            "alfred_skills",
            "managed_assets_directory",
            "asset_lifecycle",
            "no_memory",
            "always_on_top",
            "gui_fallback_input",
            "file_checkpoint_days",
            "terminal_profiles",
            "active_conversation",
            "system_audit_history",
            "routine_shortcuts",
        ]
        .contains(&key.as_str())
            && !key.starts_with("chat_draft.")
            && !key.starts_with("window_layout.")
            && !key.starts_with("download_post_actions.")
        {
            db.execute("DELETE FROM settings WHERE key=?", [key])?;
        } else {
            let mut value: Value = serde_json::from_str(&raw)?;
            if key == "attention_audio" {
                relocate(&mut value, old, target);
            }
            if key == "routine_shortcuts" {
                for routine in value.as_array_mut().ok_or("RESTORE_ROUTINES_INVALID")? {
                    routine
                        .as_object_mut()
                        .ok_or("RESTORE_ROUTINES_INVALID")?
                        .insert("needsReview".into(), json!(true));
                }
            }
            db.execute(
                "UPDATE settings SET json_value=? WHERE key=?",
                params![value.to_string(), key],
            )?;
        }
    }
    db.execute_batch(
        "DELETE FROM permission_rules;
        UPDATE permission_requests SET status='cancelled' WHERE status='pending';
        UPDATE questions SET status='cancelled' WHERE status='pending';
        UPDATE module_configs SET enabled=0;
        UPDATE custom_tools SET state='draft';
        UPDATE macros SET enabled=0;
        UPDATE watches SET enabled=0;
        UPDATE connections SET secret_ref=NULL,health='REAUTH_REQUIRED';
        UPDATE conversations SET claude_session_id=NULL;
        UPDATE projects SET root_path='';
        DELETE FROM undo_records;
        DELETE FROM file_plans;
        DELETE FROM file_rollbacks;
        DELETE FROM file_cleanup_plans;
        DELETE FROM clipboard_history;
        DELETE FROM semantic_documents;
        DELETE FROM semantic_embeddings;
        INSERT OR REPLACE INTO settings VALUES('privacy','true');
        INSERT OR REPLACE INTO settings VALUES('allowed_roots','[]');",
    )?;
    // A compatible archive may predate modules added without a schema change.
    // Seed them disabled before Core can insert its ordinary fresh-profile defaults.
    for module in crate::catalog::MODULES {
        db.execute(
            "INSERT OR IGNORE INTO module_configs(module_id,enabled) VALUES(?,0)",
            [module.id],
        )?;
    }
    // Verify attachment references resolve to exactly archived bytes. Never
    // probe a path from the archive outside the fresh staging directory.
    let attachments = db
        .prepare("SELECT path,sha256,size FROM attachments")?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (path, hash, size) in attachments {
        let relative = Path::new(&path).strip_prefix(target)?;
        let key = relative.to_str().ok_or("PROFILE_PATH")?.replace('\\', "/");
        let entry = manifest
            .entries
            .get(&key)
            .ok_or("PROFILE_ATTACHMENT_MISSING")?;
        if entry.sha256 != hash || entry.size != u64::try_from(size)? {
            return Err("PROFILE_ATTACHMENT_HASH".into());
        }
    }
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")?;
    drop(db);
    let store = storage::Store::open(&stage.join("data/alfredpet.db"))?;
    crate::assets::rebind_restored(&store, stage)?;
    store
        .0
        .lock()
        .map_err(|_| "DB_LOCK")?
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")?;
    Ok(())
}

pub(super) fn install(root: &Path, ctl: &Path, id: &str, incoming: &Path) -> Result<()> {
    uuid(id)?;
    let previous = ctl.join(format!("previous-{id}"));
    if previous.exists() {
        return Err("PROFILE_RECOVERY_EXISTS".into());
    }
    // All handles to stage databases/assets are closed before either rename.
    // The journal (running job) already exists durably outside both trees.
    fs::rename(root, &previous)?;
    if let Err(e) = fs::rename(incoming, root) {
        fs::rename(&previous, root).map_err(|_| {
            "PROFILE_ROLLBACK_FAILED: perfil anterior conservado en la carpeta de mantenimiento de esta edición"
        })?;
        return Err(e.into());
    }
    Ok(())
}
pub(super) fn recover(root: &Path, ctl: &Path, id: &str) -> Result<()> {
    uuid(id)?;
    let previous = ctl.join(format!("previous-{id}"));
    if previous.exists() {
        files::directory(&previous)?;
        if root.exists() {
            files::directory(root)?;
            let failed = ctl.join(format!("interrupted-{id}-{}", storage::id()));
            fs::rename(root, failed)?;
        }
        fs::rename(previous, root)?;
    }
    Ok(())
}
