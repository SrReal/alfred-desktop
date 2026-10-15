use super::*;
use crate::{modules, policy};
use std::sync::Arc;

#[tokio::test]
async fn retention_selects_only_old_undone_owned_unchanged_artifacts() {
    let c = fixture();
    assert_eq!(
        retention(&c, &Control::default()).unwrap_err(),
        "CLEANUP_RETENTION_NOT_CONFIGURED"
    );
    for value in [
        json!(0),
        json!(-1),
        json!(1.5),
        json!(3651),
        json!("30"),
        json!(true),
    ] {
        assert!(validate_retention(&value).is_err());
    }
    validate_retention(&Value::Null).unwrap();
    validate_retention(&json!(30)).unwrap();
    let (token, source, retained) = copied_then_undone(&c, false).await;
    c.store
        .set_setting("file_checkpoint_days", &json!(30))
        .unwrap();
    assert!(retention(&c, &Control::default()).unwrap()["plan_id"].is_null());
    c.store
        .execute(
            "UPDATE undo_records SET updated_at='2020-01-01T00:00:00Z' WHERE id=?",
            &[json!(token)],
        )
        .unwrap();
    let available = modules::dispatch(&c, "files.delete", &json!({"path":source}))
        .await
        .unwrap();
    c.store
        .execute(
            "UPDATE undo_records SET updated_at='2020-01-01T00:00:00Z' WHERE id=?",
            &[available["undo_token"].clone()],
        )
        .unwrap();
    let preview = retention(&c, &Control::default()).unwrap();
    assert_eq!(preview["artifacts"].as_array().unwrap().len(), 1);
    assert_eq!(preview["artifacts"][0]["journal_id"], token);
    std::fs::write(&retained, "later edit").unwrap();
    assert!(retention(&c, &Control::default()).unwrap()["plan_id"].is_null());
    std::fs::write(&retained, "contenido").unwrap();
    c.store
        .execute(
            "UPDATE undo_records SET metadata_json='{}' WHERE id=?",
            &[json!(token)],
        )
        .unwrap();
    // File ownership is known at the root; a tree requires all descendant IDs.
    assert!(retention(&c, &Control::default()).unwrap()["plan_id"].is_string());
    c.store
        .execute(
            "UPDATE undo_records SET object_id=NULL WHERE id=?",
            &[json!(token)],
        )
        .unwrap();
    assert!(retention(&c, &Control::default()).unwrap()["plan_id"].is_null());
    assert!(retained.exists() && Path::new(available["checkpoint"].as_str().unwrap()).exists());
    finish(c);
}

#[tokio::test]
async fn migration_eight_backs_up_seven_and_preserves_prior_undo() {
    let c = fixture();
    let (token, source, retained) = copied_then_undone(&c, false).await;
    let root = c.root.clone();
    drop(c);
    let db_path = root.join("data/alfredpet.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute_batch("DROP TABLE chat_progress;DROP TABLE drive_jobs;DROP TRIGGER clipboard_setting_insert;DROP TRIGGER clipboard_setting_update;DROP TRIGGER clipboard_setting_delete;DROP TRIGGER clipboard_module_update;DROP TABLE clipboard_history;DROP TABLE clipboard_state;DROP TABLE file_cleanup_plans;ALTER TABLE undo_records DROP COLUMN metadata_json;PRAGMA user_version=7;").unwrap();
    drop(db);
    let backups_before = std::fs::read_dir(db_path.parent().unwrap())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("pre-v8")
        })
        .count();
    let c =
        Arc::new(Core::new(root, PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap());
    assert_eq!(
        c.store.query("PRAGMA user_version", &[]).unwrap()[0]["user_version"],
        crate::storage::SCHEMA_VERSION
    );
    assert_eq!(
        c.store
            .query(
                "SELECT metadata_json FROM undo_records WHERE id=?",
                &[json!(token)]
            )
            .unwrap()[0]["metadata_json"],
        "{}"
    );
    assert_eq!(
        std::fs::read_dir(db_path.parent().unwrap())
            .unwrap()
            .filter(|e| e
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("pre-v8"))
            .count(),
        backups_before + 1
    );
    assert!(retained.exists() && source.exists());
    let preview = run(&c, json!({"action":"preview","ids":[token]})).unwrap();
    assert!(preview["plan_id"].is_string()); // A file's v7 root ID and digest remain sufficient.
    finish(c);
}

fn fixture() -> Arc<Core> {
    Arc::new(
        Core::new(
            std::env::temp_dir().join(format!("ap-cleanup-{}", storage::id())),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap(),
    )
}
fn finish(c: Arc<Core>) {
    let root = c.root.clone();
    drop(c);
    std::fs::remove_dir_all(root).unwrap();
}
fn manifest(c: &Core, id: &Value) -> Value {
    parse_plan(&plan(c, id.as_str().unwrap()).unwrap()).unwrap()
}

#[tokio::test]
async fn incomplete_work_before_erasure_marker_is_preserved_for_a_new_review() {
    let mut c = fixture();
    let (token, _, retained) = copied_then_undone(&c, true).await;
    let preview = run(&c, json!({"action":"preview","ids":[token]})).unwrap();
    let data = manifest(&c, &preview["plan_id"]);
    let entry = &data["entries"][0];
    let work = PathBuf::from(entry["work_path"].as_str().unwrap());
    relocate(
        &c,
        &retained,
        &work,
        entry["digest"].as_str().unwrap(),
        entry["root_id"].as_str().unwrap(),
        &Control::default(),
    )
    .unwrap();
    c.store
        .execute(
            "UPDATE file_cleanup_plans SET state='running' WHERE id=?",
            &[preview["plan_id"].clone()],
        )
        .unwrap();
    // An external modification precedes the durable authorization to erase.
    std::fs::remove_file(work.join("zero")).unwrap();
    let root = c.root.clone();
    drop(c);
    c = Arc::new(Core::new(root, PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap());
    let result = run(&c, json!({"action":"apply","plan_id":preview["plan_id"]}));
    let preserved = work.join("nested").exists();
    finish(c);
    assert_eq!(result.unwrap_err(), "CLEANUP_ARTIFACT_CHANGED");
    assert!(preserved);
}

#[tokio::test]
async fn cleanup_descendant_identity_and_expired_review_require_a_new_plan() {
    let c = fixture();
    let (token, _, retained) = copied_then_undone(&c, true).await;
    let file = retained.join("zero");
    let saved = c.root.join("temp/saved-child");
    std::fs::rename(&file, &saved).unwrap();
    std::fs::copy(&saved, &file).unwrap();
    let refused = run(&c, json!({"action":"preview","ids":[token]})).unwrap();
    assert!(refused["plan_id"].is_null());
    assert_eq!(refused["skipped"][0]["reason"], "CLEANUP_ARTIFACT_CHANGED");
    let preview = run(
        &c,
        json!({"action":"preview","ids":[token],"include_changed":true}),
    )
    .unwrap();
    let saved_again = c.root.join("temp/saved-child-again");
    std::fs::rename(&file, &saved_again).unwrap();
    std::fs::copy(&saved_again, &file).unwrap();
    assert_eq!(
        run(&c, json!({"action":"apply","plan_id":preview["plan_id"]})).unwrap_err(),
        "CLEANUP_ARTIFACT_CHANGED"
    );
    assert!(file.exists() && saved.exists() && saved_again.exists());
    let preview = run(
        &c,
        json!({"action":"preview","ids":[token],"include_changed":true}),
    )
    .unwrap();
    c.store
        .execute(
            "UPDATE file_cleanup_plans SET created_at='2020-01-01T00:00:00Z' WHERE id=?",
            &[preview["plan_id"].clone()],
        )
        .unwrap();
    assert_eq!(
        run(&c, json!({"action":"apply","plan_id":preview["plan_id"]})).unwrap_err(),
        "CLEANUP_PLAN_EXPIRED"
    );
    assert!(retained.exists());
    finish(c);
}
fn run(c: &Core, body: Value) -> Result<Value, String> {
    dispatch(c, &body, &Control::default())
}
#[tokio::test]
async fn multiple_artifacts_of_one_journal_resume_only_a_recorded_forgetting_transition() {
    let c = fixture();
    let (id, source, checkpoint) = copied_then_undone(&c, false).await;
    let stage = c
        .root
        .join(format!("temp/.alfredpet-{}.staging", storage::id()));
    std::fs::write(&stage, "incomplete").unwrap();
    let row = c
        .store
        .query("SELECT * FROM undo_records WHERE id=?", &[json!(id)])
        .unwrap()[0]
        .clone();
    let mut info = metadata(&row).unwrap();
    info["stage_id"] = json!(identity(&open(&stage, false, false).unwrap()).unwrap());
    c.store
        .execute(
            "UPDATE undo_records SET staging=?,metadata_json=? WHERE id=?",
            &[json!(stage), json!(info.to_string()), json!(id)],
        )
        .unwrap();
    let preview = run(&c, json!({"action":"preview","ids":[id]})).unwrap();
    let plan_id = preview["plan_id"].as_str().unwrap();
    let mut data = manifest(&c, &preview["plan_id"]);
    assert_eq!(data["entries"].as_array().unwrap().len(), 2);
    c.store
        .execute(
            "UPDATE undo_records SET state='forgotten' WHERE id=?",
            &[json!(id)],
        )
        .unwrap();
    assert_eq!(
        run(&c, json!({"action":"apply","plan_id":plan_id})).unwrap_err(),
        "CLEANUP_JOURNAL_CHANGED"
    );
    assert!(checkpoint.exists() && stage.exists());
    // Native erasure of the first artifact completed, with a durable marker and
    // cursor; the same journal's untouched staging must remain resumable.
    let entry = &data["entries"][0];
    let work = PathBuf::from(entry["work_path"].as_str().unwrap());
    relocate(
        &c,
        &checkpoint,
        &work,
        entry["digest"].as_str().unwrap(),
        entry["root_id"].as_str().unwrap(),
        &Control::default(),
    )
    .unwrap();
    purge(&c, entry, &Control::default()).unwrap();
    data["entries"][0]["erasing"] = json!(true);
    c.store.execute("UPDATE file_cleanup_plans SET snapshot_json=?,state='interrupted',next_index=1 WHERE id=?",&[json!(data.to_string()),json!(plan_id)]).unwrap();
    run(&c, json!({"action":"apply","plan_id":plan_id})).unwrap();
    assert!(source.exists() && !checkpoint.exists() && !stage.exists());
    assert!(c
        .store
        .query("SELECT staging FROM undo_records WHERE id=?", &[json!(id)])
        .unwrap()[0]["staging"]
        .is_null());
    finish(c);
}
async fn copied_then_undone(c: &Arc<Core>, directory: bool) -> (String, PathBuf, PathBuf) {
    let source = c.root.join("temp/source");
    if directory {
        std::fs::create_dir_all(source.join("nested/empty")).unwrap();
        std::fs::write(source.join("nested/ñ.txt"), "contenido").unwrap();
        std::fs::write(source.join("zero"), []).unwrap();
    } else {
        std::fs::write(&source, "contenido").unwrap();
    }
    let target = c.root.join("temp/target");
    let result = modules::dispatch(
        c,
        "files.copy",
        &json!({"path":source,"destination":target}),
    )
    .await
    .unwrap();
    let token = result["undo_token"].as_str().unwrap().to_owned();
    undo::execute(c, &token).unwrap();
    let row = c
        .store
        .query("SELECT * FROM undo_records WHERE id=?", &[json!(token)])
        .unwrap()[0]
        .clone();
    (
        token,
        source,
        PathBuf::from(row["checkpoint"].as_str().unwrap()),
    )
}
#[tokio::test]
async fn real_reviewed_cleanup_deletes_only_retained_file_and_nested_tree() {
    for directory in [false, true] {
        let c = fixture();
        let (token, source, checkpoint) = copied_then_undone(&c, directory).await;
        let preview = run(&c, json!({"action":"preview","ids":[token]})).unwrap();
        assert_eq!(preview["artifacts"][0]["forgets_recovery"], false);
        assert_eq!(
            PathBuf::from(preview["artifacts"][0]["path"].as_str().unwrap()),
            checkpoint
        );
        let review = review(&c, &json!({"plan_id":preview["plan_id"]})).unwrap();
        assert_eq!(review["artifacts"], preview["artifacts"]);
        let denied = c
            .execute(
                "files.cleanup",
                json!({"body":{"action":"apply","plan_id":preview["plan_id"]}}),
            )
            .await
            .unwrap_err();
        assert!(!denied.is_empty());
        assert!(checkpoint.exists());
        assert_eq!(
            policy::classify("files.cleanup", &json!({"body":{"action":"preview"}})),
            policy::Risk::Free
        );
        assert_eq!(
            policy::classify("files.cleanup", &json!({"body":{"action":"apply"}})),
            policy::Risk::Mandatory
        );
        let applied = run(&c, json!({"action":"apply","plan_id":preview["plan_id"]})).unwrap();
        assert_eq!(applied["completed"], 1);
        assert!(!checkpoint.exists());
        assert!(source.exists());
        assert_eq!(
            run(&c, json!({"action":"apply","plan_id":preview["plan_id"]})).unwrap()["state"],
            "completed"
        );
        assert_eq!(undo::execute(&c, &token).unwrap_err(), "UNDO_NOT_AVAILABLE");
        finish(c);
    }
    println!(
        "FILE_CLEANUP_CONTRACT_PASS: actual file+tree deletion/review/L3 refusal/source preserved"
    );
}
#[tokio::test]
async fn cleanup_recovery_and_changed_artifacts_require_explicit_review() {
    let c = fixture();
    let source = c.root.join("temp/source.txt");
    std::fs::write(&source, "original").unwrap();
    let deleted = modules::dispatch(&c, "files.delete", &json!({"path":source}))
        .await
        .unwrap();
    let token = deleted["undo_token"].as_str().unwrap();
    let retained = PathBuf::from(deleted["checkpoint"].as_str().unwrap());
    let protected = run(&c, json!({"action":"preview","ids":[token]})).unwrap();
    assert!(protected["plan_id"].is_null());
    assert_eq!(
        protected["skipped"][0]["reason"],
        "CLEANUP_RECOVERY_RETAINED"
    );
    std::fs::write(&retained, "changed").unwrap();
    let changed = run(
        &c,
        json!({"action":"preview","ids":[token],"forget_undo":true}),
    )
    .unwrap();
    assert!(changed["plan_id"].is_null());
    assert_eq!(changed["skipped"][0]["reason"], "CLEANUP_ARTIFACT_CHANGED");
    let reviewed = run(
        &c,
        json!({"action":"preview","ids":[token],"forget_undo":true,"include_changed":true}),
    )
    .unwrap();
    assert_eq!(reviewed["artifacts"][0]["forgets_recovery"], true);
    std::fs::write(&retained, "changed again").unwrap();
    assert_eq!(
        run(&c, json!({"action":"apply","plan_id":reviewed["plan_id"]})).unwrap_err(),
        "CLEANUP_ARTIFACT_CHANGED"
    );
    assert!(retained.exists());
    let reviewed = run(
        &c,
        json!({"action":"preview","ids":[token],"forget_undo":true,"include_changed":true}),
    )
    .unwrap();
    run(&c, json!({"action":"apply","plan_id":reviewed["plan_id"]})).unwrap();
    assert!(!retained.exists());
    assert!(!source.exists());
    finish(c);
}
#[tokio::test]
async fn cleanup_identical_replacement_foreign_work_path_and_scope_changes_are_preserved() {
    let c = fixture();
    let (token, _, retained) = copied_then_undone(&c, false).await;
    let preview = run(&c, json!({"action":"preview","ids":[token]})).unwrap();
    let data = manifest(&c, &preview["plan_id"]);
    let work = PathBuf::from(data["entries"][0]["work_path"].as_str().unwrap());
    std::fs::write(&work, "foreign").unwrap();
    assert_eq!(
        run(&c, json!({"action":"apply","plan_id":preview["plan_id"]})).unwrap_err(),
        "UNDO_OBJECT_REPLACED"
    );
    assert_eq!(
        c.store
            .query("SELECT state FROM undo_records WHERE id=?", &[json!(token)])
            .unwrap()[0]["state"],
        "undone"
    );
    assert_eq!(std::fs::read_to_string(&work).unwrap(), "foreign");
    std::fs::remove_file(&work).unwrap();
    let old = retained.with_extension("preserved");
    std::fs::rename(&retained, &old).unwrap();
    std::fs::copy(&old, &retained).unwrap();
    assert_eq!(
        run(&c, json!({"action":"apply","plan_id":preview["plan_id"]})).unwrap_err(),
        "CLEANUP_ARTIFACT_CHANGED"
    );
    let new = run(
        &c,
        json!({"action":"preview","ids":[token],"include_changed":true}),
    )
    .unwrap();
    assert!(new["plan_id"].is_null());
    assert_eq!(new["skipped"][0]["reason"], "UNDO_OBJECT_REPLACED");
    std::fs::remove_file(&retained).unwrap();
    std::fs::rename(old, &retained).unwrap();
    c.store
        .set_setting("allowed_roots", &json!([c.root.join("temp")]))
        .unwrap();
    assert_eq!(
        run(&c, json!({"action":"apply","plan_id":preview["plan_id"]})).unwrap_err(),
        "CLEANUP_SCOPE_CHANGED"
    );
    assert!(retained.exists());
    finish(c);
}
#[tokio::test]
async fn cleanup_resume_after_partial_deletion_rejects_new_entries_and_cursor_gap_recovers() {
    let mut c = fixture();
    let (token, source, retained) = copied_then_undone(&c, true).await;
    let preview = run(&c, json!({"action":"preview","ids":[token]})).unwrap();
    let id = preview["plan_id"].as_str().unwrap();
    let mut data = manifest(&c, &preview["plan_id"]);
    let entry = &data["entries"][0];
    let work = PathBuf::from(entry["work_path"].as_str().unwrap());
    relocate(
        &c,
        &retained,
        &work,
        entry["digest"].as_str().unwrap(),
        entry["root_id"].as_str().unwrap(),
        &Control::default(),
    )
    .unwrap();
    delete_handle(&open(&work.join("zero"), true, false).unwrap()).unwrap();
    data["entries"][0]["erasing"] = json!(true);
    c.store
        .execute(
            "UPDATE undo_records SET state='forgotten' WHERE id=?",
            &[json!(token)],
        )
        .unwrap();
    c.store
        .execute(
            "UPDATE file_cleanup_plans SET state='running',snapshot_json=? WHERE id=?",
            &[json!(data.to_string()), json!(id)],
        )
        .unwrap();
    let root = c.root.clone();
    drop(c);
    c = Arc::new(Core::new(root, PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap());
    assert_eq!(plan(&c, id).unwrap()["state"], "interrupted");
    let foreign = work.join("new.txt");
    std::fs::write(&foreign, "keep").unwrap();
    assert_eq!(
        run(&c, json!({"action":"apply","plan_id":id})).unwrap_err(),
        "CLEANUP_NEW_ENTRY"
    );
    assert!(work.join("nested/ñ.txt").exists());
    std::fs::remove_file(foreign).unwrap();
    let result = run(&c, json!({"action":"apply","plan_id":id})).unwrap();
    assert_eq!(result["state"], "completed");
    assert!(source.exists());
    assert!(!work.exists());
    // Simulate native deletion completed before the durable cursor update.
    c.store
        .execute(
            "UPDATE file_cleanup_plans SET state='interrupted',next_index=0 WHERE id=?",
            &[json!(id)],
        )
        .unwrap();
    assert_eq!(
        run(&c, json!({"action":"apply","plan_id":id})).unwrap()["completed"],
        1
    );
    finish(c);
}
#[tokio::test]
async fn cleanup_readonly_checkpoint_and_legacy_staging_are_explicitly_reviewed() {
    let c = fixture();
    let source = c.root.join("temp/readonly.txt");
    std::fs::write(&source, "readonly").unwrap();
    let mut perms = std::fs::metadata(&source).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&source, perms).unwrap();
    let deleted = modules::dispatch(&c, "files.delete", &json!({"path":source}))
        .await
        .unwrap();
    let preview = run(
        &c,
        json!({"action":"preview","ids":[deleted["undo_token"]],"forget_undo":true}),
    )
    .unwrap();
    run(&c, json!({"action":"apply","plan_id":preview["plan_id"]})).unwrap();
    assert!(!Path::new(deleted["checkpoint"].as_str().unwrap()).exists());
    let staging = c
        .root
        .join(format!("temp/.alfredpet-{}.staging", storage::id()));
    std::fs::write(&staging, "partial").unwrap();
    let token = undo::prepare_tree(
        &c,
        "tree_copy",
        &source,
        Some(&c.root.join("temp/dest")),
        None,
        "unused",
    )
    .unwrap();
    c.store
        .execute(
            "UPDATE undo_records SET staging=?,state='blocked' WHERE id=?",
            &[json!(staging), json!(token)],
        )
        .unwrap();
    let legacy = run(
        &c,
        json!({"action":"preview","ids":[token],"forget_undo":true}),
    )
    .unwrap();
    assert!(legacy["plan_id"].is_null());
    assert_eq!(
        legacy["skipped"][0]["reason"],
        "CLEANUP_LEGACY_REQUIRES_REVIEW"
    );
    let adopted = run(
        &c,
        json!({"action":"preview","ids":[token],"forget_undo":true,"adopt_legacy":true}),
    )
    .unwrap();
    run(&c, json!({"action":"apply","plan_id":adopted["plan_id"]})).unwrap();
    assert!(!staging.exists());
    finish(c);
}
