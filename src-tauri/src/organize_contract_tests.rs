use super::*;
use std::sync::Arc;

struct Fixture {
    core: Arc<Core>,
    root: PathBuf,
    inbox: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ap-organizer-{}", storage::id()));
        let core = Arc::new(
            Core::new(
                root.clone(),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        let inbox = core
            .path(root.join("temp").to_str().unwrap(), false)
            .unwrap()
            .join("inbox");
        std::fs::create_dir(&inbox).unwrap();
        for (name, contents) in [("a.txt", "first"), ("b.txt", "second")] {
            std::fs::write(inbox.join(name), contents).unwrap();
        }
        Self { core, root, inbox }
    }
    fn rules(&self) -> Value {
        json!({"path":self.inbox,"body":{"rules":[{"extensions":["txt","checkpoint","staging"],"folder":"Docs"}]}})
    }
    fn preview(&self) -> Value {
        dispatch(&self.core, &self.rules()).unwrap()
    }
    fn apply_input(&self, plan: &Value) -> Value {
        json!({"path":self.inbox,"body":{"plan_id":plan["plan_id"]}})
    }
    fn restart(&mut self) {
        self.core = Arc::new(
            Core::new(
                self.root.clone(),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
    }
    fn finish(self) {
        drop(self.core);
        let root = self.root.canonicalize().unwrap();
        assert!(root.starts_with(std::env::temp_dir().canonicalize().unwrap()));
        assert!(root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("ap-organizer-"));
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn real_organizer_native_journals_restart_undo_and_recovery_exclusion() {
    let mut f = Fixture::new();
    let checkpoint = f.inbox.join("retained.txt");
    let staging = f.inbox.join("unfinished.txt");
    let parked = f.inbox.join("parked.txt");
    let work = f.inbox.join("work.txt");
    let reserved = f
        .inbox
        .join(format!(".ALFREDPET-{}.CHECKPOINT", storage::id()));
    for path in [&checkpoint, &staging, &parked, &work, &reserved] {
        std::fs::write(path, "keep recovery").unwrap();
    }
    let token = undo::prepare(
        &f.core,
        "delete",
        &f.inbox.join("absent.txt"),
        None,
        Some(&checkpoint),
        &crate::modules::hash(&checkpoint).unwrap(),
    )
    .unwrap();
    f.core
        .store
        .execute(
            "UPDATE undo_records SET state='blocked',staging=?,metadata_json=? WHERE id=?",
            &[
                json!(staging),
                json!(json!({"undo_target":parked}).to_string()),
                json!(token),
            ],
        )
        .unwrap();
    f.core.store.execute("INSERT INTO file_cleanup_plans(id,snapshot_json,created_at,updated_at) VALUES(?,?,?,?)",&[json!(storage::id()),json!(json!({"roots":f.core.roots(),"entries":[{"work_path":work}]}).to_string()),json!(storage::now()),json!(storage::now())]).unwrap();
    let plan = f.preview();
    assert_eq!(plan["moves"].as_array().unwrap().len(), 2);
    let review = review(&f.core, &f.apply_input(&plan)).unwrap();
    assert_eq!(review["moves"].as_array().unwrap().len(), 2);
    assert!(review["moves"][0].get("object_id").is_none());
    let applied = dispatch(&f.core, &f.apply_input(&plan)).unwrap();
    for (i, step) in applied["moves"].as_array().unwrap().iter().enumerate() {
        let journal = f
            .core
            .store
            .query(
                "SELECT kind,object_id,metadata_json FROM undo_records WHERE id=?",
                &[step["undo_token"].clone()],
            )
            .unwrap();
        assert_eq!(journal[0]["kind"], "tree_move");
        assert_eq!(journal[0]["object_id"], plan["moves"][i]["object_id"]);
        let data: Value =
            serde_json::from_str(journal[0]["metadata_json"].as_str().unwrap()).unwrap();
        assert_eq!(data["source_parent_id"], plan["moves"][i]["root_id"]);
    }
    for path in [&checkpoint, &staging, &parked, &work, &reserved] {
        assert_eq!(std::fs::read_to_string(path).unwrap(), "keep recovery");
    }
    for path in [
        f.core.root.join("backups"),
        f.inbox
            .join(format!(".alfredpet-{}.staging", storage::id())),
    ] {
        if !path.exists() {
            std::fs::create_dir(&path).unwrap();
        }
        assert_eq!(
            dispatch(
                &f.core,
                &json!({"path":path,"body":{"rules":[{"extensions":["txt"],"folder":"Docs"}]}})
            )
            .unwrap_err(),
            "ORGANIZE_RECOVERY_ARTIFACT"
        );
    }
    f.restart();
    for (i, step) in applied["moves"].as_array().unwrap().iter().enumerate() {
        undo::execute(&f.core, step["undo_token"].as_str().unwrap()).unwrap();
        let info = file_tree::file_snapshot(
            &f.core,
            Path::new(step["source"].as_str().unwrap()),
            &Control::default(),
        )
        .unwrap();
        assert_eq!(info["object_id"], plan["moves"][i]["object_id"]);
        assert_eq!(info["sha256"], plan["moves"][i]["sha256"]);
    }
    assert_eq!(applied["group_undo_tokens"].as_array().unwrap().len(), 1);
    undo::execute(&f.core, applied["group_undo_tokens"][0].as_str().unwrap()).unwrap();
    assert!(!f.inbox.join("Docs").exists());
    println!("FILE_ORGANIZER_CONTRACT_PASS: native IDs/restart/undo/empty group publication/legacy and active recovery exclusion/concrete permission review");
    f.finish();
}

#[test]
fn organizer_rejects_identical_replacement_new_groups_legacy_plans_and_changed_root() {
    let f = Fixture::new();
    let plan = f.preview();
    let input = f.apply_input(&plan);
    let source = f.inbox.join("a.txt");
    let parked = f.core.root.join("temp/parked.txt");
    std::fs::rename(&source, &parked).unwrap();
    std::fs::write(&source, "first").unwrap();
    assert_eq!(
        dispatch(&f.core, &input).unwrap_err(),
        "ORGANIZE_SOURCE_CHANGED"
    );
    assert!(!f.inbox.join("Docs").exists());
    std::fs::remove_file(&source).unwrap();
    std::fs::rename(&parked, &source).unwrap();
    let group = f.inbox.join("Docs");
    std::fs::create_dir(&group).unwrap();
    assert_eq!(
        dispatch(&f.core, &input).unwrap_err(),
        "ORGANIZE_GROUP_APPEARED"
    );
    let plan = f.preview();
    let input = f.apply_input(&plan);
    let old_group = f.inbox.join("old-Docs");
    std::fs::rename(&group, &old_group).unwrap();
    std::fs::create_dir(&group).unwrap();
    std::fs::write(group.join("keep.txt"), "foreign").unwrap();
    assert_eq!(
        dispatch(&f.core, &input).unwrap_err(),
        "ORGANIZE_DESTINATION_PARENT_CHANGED"
    );
    assert_eq!(
        std::fs::read_to_string(group.join("keep.txt")).unwrap(),
        "foreign"
    );
    std::fs::remove_file(group.join("keep.txt")).unwrap();
    std::fs::remove_dir(&group).unwrap();
    std::fs::rename(&old_group, &group).unwrap();
    // The final native operation rechecks the reviewed ID after preflight.
    let item = &plan["moves"][0];
    let (checked, target) = validate_move(&f.core, &f.inbox, item, &Control::default()).unwrap();
    std::fs::rename(&checked, &parked).unwrap();
    std::fs::write(&checked, "first").unwrap();
    assert_eq!(
        file_tree::mutate_checked(
            &f.core,
            "move",
            &checked,
            Some(&target),
            &Control::default(),
            Some(item)
        )
        .unwrap_err(),
        "ORGANIZE_SOURCE_CHANGED"
    );
    std::fs::remove_file(&checked).unwrap();
    std::fs::rename(&parked, &checked).unwrap();
    let parked_root = f.inbox.with_file_name("inbox-parked");
    std::fs::rename(&f.inbox, &parked_root).unwrap();
    std::fs::create_dir(&f.inbox).unwrap();
    std::fs::write(f.inbox.join("a.txt"), "first").unwrap();
    assert_eq!(
        dispatch(&f.core, &input).unwrap_err(),
        "ORGANIZE_ROOT_CHANGED"
    );
    std::fs::remove_file(f.inbox.join("a.txt")).unwrap();
    std::fs::remove_dir(&f.inbox).unwrap();
    std::fs::rename(&parked_root, &f.inbox).unwrap();
    let mut legacy = plan["moves"].clone();
    legacy[0].as_object_mut().unwrap().remove("object_id");
    f.core
        .store
        .execute(
            "UPDATE file_plans SET plan_json=? WHERE id=?",
            &[json!(legacy.to_string()), plan["plan_id"].clone()],
        )
        .unwrap();
    assert_eq!(
        dispatch(&f.core, &input).unwrap_err(),
        "ORGANIZE_LEGACY_PLAN_REVIEW_REQUIRED"
    );
    assert!(source.exists());
    assert!(f.inbox.join("b.txt").exists());
    f.finish();
}

#[tokio::test]
async fn queued_or_cancelled_organizer_calls_do_not_mutate_after_revocation_or_abandonment() {
    let f = Fixture::new();
    let plan = f.preview();
    let input = f.apply_input(&plan);
    let token = tokio_util::sync::CancellationToken::new();
    let control = Control::new(token.clone());
    control.check(&f.core).unwrap();
    token.cancel();
    assert_eq!(
        dispatch_control(&f.core, &input, &control).unwrap_err(),
        "FILES_CANCELLED"
    );
    let control = Control::default();
    control.check(&f.core).unwrap();
    f.core
        .store
        .set_setting("allowed_roots", &json!([f.inbox]))
        .unwrap();
    assert_eq!(
        dispatch_control(&f.core, &input, &control).unwrap_err(),
        "FILES_SCOPE_CHANGED"
    );
    for abandon in [true, false] {
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let held_core = f.core.clone();
        let held = tokio::task::spawn_blocking(move || {
            let _serial = held_core.file_actions.lock().unwrap();
            ready_tx.send(()).unwrap();
            release_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        });
        ready_rx.await.unwrap();
        let core = f.core.clone();
        let queued_input = input.clone();
        let task = tokio::spawn(async move {
            crate::modules::dispatch(&core, "files.organize", &queued_input).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!task.is_finished());
        if abandon {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            release_tx.send(()).unwrap();
        } else {
            f.core
                .store
                .execute(
                    "UPDATE module_configs SET enabled=0 WHERE module_id='files'",
                    &[],
                )
                .unwrap();
            release_tx.send(()).unwrap();
            assert_eq!(task.await.unwrap().unwrap_err(), "MODULE_DISABLED: files");
        }
        held.await.unwrap();
        for _ in 0..100 {
            if Arc::strong_count(&f.core) == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(Arc::strong_count(&f.core), 1);
    }
    assert_eq!(
        f.core
            .store
            .query(
                "SELECT state FROM file_plans WHERE id=?",
                &[plan["plan_id"].clone()]
            )
            .unwrap()[0]["state"],
        "preview"
    );
    assert_eq!(
        f.core
            .store
            .query("SELECT COUNT(*) AS n FROM undo_records", &[])
            .unwrap()[0]["n"],
        0
    );
    assert!(!f.inbox.join("Docs").exists());
    f.finish();
}

#[test]
fn organizer_preview_rejects_hardlinks_and_streams_before_persisting_a_plan() {
    let f = Fixture::new();
    let source = f.inbox.join("a.txt");
    let link = f.core.root.join("temp/hardlink.txt");
    std::fs::hard_link(&source, &link).unwrap();
    assert_eq!(
        dispatch(&f.core, &f.rules()).unwrap_err(),
        "TREE_HARDLINK_DENIED"
    );
    std::fs::remove_file(&link).unwrap();
    std::fs::write(format!("{}:secret", source.display()), "unselected stream").unwrap();
    assert_eq!(
        dispatch(&f.core, &f.rules()).unwrap_err(),
        "TREE_ALTERNATE_STREAM_DENIED"
    );
    assert_eq!(
        f.core
            .store
            .query("SELECT COUNT(*) AS n FROM file_plans", &[])
            .unwrap()[0]["n"],
        0
    );
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "first");
    f.finish();
}

#[tokio::test]
async fn organizer_permission_displays_the_saved_moves_and_denial_leaves_files_untouched() {
    let f = Fixture::new();
    let plan = f.preview();
    let input = f.apply_input(&plan);
    assert!(f.core.execute("files.organize", input).await.is_err());
    let rows=f.core.store.query("SELECT status,input_json_redacted FROM permission_requests WHERE tool_id='files.organize'",&[]).unwrap();
    assert_eq!(rows[0]["status"], "cancelled");
    let shown: Value =
        serde_json::from_str(rows[0]["input_json_redacted"].as_str().unwrap()).unwrap();
    assert_eq!(shown["plan_id"], plan["plan_id"]);
    assert_eq!(shown["moves"].as_array().unwrap().len(), 2);
    assert_eq!(shown["moves"][0]["source"], plan["moves"][0]["source"]);
    assert_eq!(
        shown["moves"][0]["destination"],
        plan["moves"][0]["destination"]
    );
    assert!(f.inbox.join("a.txt").exists());
    assert!(!f.inbox.join("Docs").exists());
    assert_eq!(
        f.core
            .store
            .query("SELECT COUNT(*) AS n FROM undo_records", &[])
            .unwrap()[0]["n"],
        0
    );
    f.finish();
}

#[test]
fn organizer_group_creation_database_failure_retains_a_journaled_stage_for_review() {
    let mut f = Fixture::new();
    let plan = f.preview();
    f.core.store.execute("CREATE TRIGGER fail_group_publication BEFORE UPDATE OF expected_hash ON undo_records WHEN json_extract(NEW.metadata_json,'$.operation')='organize_group' BEGIN SELECT RAISE(ABORT,'fixture publication failure'); END",&[]).unwrap();
    let error = dispatch(&f.core, &f.apply_input(&plan)).unwrap_err();
    assert!(error.contains("fixture publication failure"));
    assert!(error.contains("recovery_undo_id="));
    assert!(!f.inbox.join("Docs").exists());
    assert!(f.inbox.join("a.txt").exists());
    let row = f
        .core
        .store
        .query("SELECT * FROM undo_records", &[])
        .unwrap()[0]
        .clone();
    assert_eq!(row["state"], "prepared");
    let data: Value = serde_json::from_str(row["metadata_json"].as_str().unwrap()).unwrap();
    assert!(data["stage_id"].as_str().is_some());
    let stage = PathBuf::from(row["staging"].as_str().unwrap());
    assert!(stage.is_dir());
    assert_eq!(
        f.core
            .store
            .query(
                "SELECT state FROM file_plans WHERE id=?",
                &[plan["plan_id"].clone()]
            )
            .unwrap()[0]["state"],
        "partial"
    );
    f.restart();
    assert_eq!(
        f.core
            .store
            .query(
                "SELECT state FROM undo_records WHERE id=?",
                &[row["id"].clone()]
            )
            .unwrap()[0]["state"],
        "blocked"
    );
    assert!(stage.is_dir());
    let cleanup = file_tree::cleanup(
        &f.core,
        &json!({"action":"preview","ids":[row["id"]],"forget_undo":true}),
        &Control::default(),
    )
    .unwrap();
    assert_eq!(cleanup["artifacts"][0]["role"], "staging");
    assert!(stage.is_dir());
    file_tree::cleanup(
        &f.core,
        &json!({"action":"apply","plan_id":cleanup["plan_id"]}),
        &Control::default(),
    )
    .unwrap();
    assert!(!stage.exists());
    assert_eq!(
        std::fs::read_to_string(f.inbox.join("a.txt")).unwrap(),
        "first"
    );
    f.finish();
}
