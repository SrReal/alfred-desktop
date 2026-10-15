use super::*;
use crate::modules;
use std::sync::Arc;

fn fixture() -> Arc<Core> {
    let root = std::env::temp_dir().join(format!("ap-tree-{}", storage::id()));
    Arc::new(Core::new(root, PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap())
}
fn tree(c: &Core) -> PathBuf {
    let path = c.root.join("temp/source");
    std::fs::create_dir_all(path.join("nested/empty")).unwrap();
    std::fs::write(path.join("nested/ñ.txt"), "Contenido sintético").unwrap();
    std::fs::write(path.join("zero.bin"), []).unwrap();
    path
}
fn finish(c: Arc<Core>) {
    let root = c.root.clone();
    drop(c);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn undo_revalidation_requires_fresh_l3_review_and_rechecks_queue_revocation() {
    let c = fixture();
    let source = tree(&c);
    let target = c.root.join("temp/revalidation");
    let result = mutate(&c, "move", &source, Some(&target), &Control::default()).unwrap();
    let id = result["undo_token"].as_str().unwrap();
    c.store
        .execute(
            "UPDATE undo_records SET state='blocked' WHERE id=?",
            &[json!(id)],
        )
        .unwrap();
    let input = json!({"id":id,"revalidate":true});
    let schema = crate::catalog::tool_schema("files.undo", "id");
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&input));
    assert!(!validator.is_valid(&json!({"id":id,"revalidate":"yes"})));
    assert!(!validator.is_valid(&json!({"id":id,"force":true})));
    assert_eq!(
        crate::policy::classify("files.undo", &input),
        crate::policy::Risk::Mandatory
    );
    let review = undo::review(&c, "files.undo", &input).unwrap();
    assert_eq!(
        review["operations_in_undo_order"][0]["revalidate_blocked"],
        true
    );
    assert_eq!(
        review["operations_in_undo_order"][0]["original"],
        json!(source)
    );
    assert!(undo::history(&c).unwrap()[0]["revalidation_available"] == true);
    assert!(c.execute("files.undo", input.clone()).await.is_err());
    assert!(!source.exists());
    assert!(target.exists());
    assert_eq!(
        c.store
            .query("SELECT state FROM undo_records WHERE id=?", &[json!(id)])
            .unwrap()[0]["state"],
        "blocked"
    );
    let requests = c
        .store
        .query(
            "SELECT status,input_json_redacted FROM permission_requests WHERE tool_id='files.undo'",
            &[],
        )
        .unwrap();
    assert_eq!(requests[0]["status"], "cancelled");
    let saved: Value =
        serde_json::from_str(requests[0]["input_json_redacted"].as_str().unwrap()).unwrap();
    assert_eq!(saved["request"]["revalidate"], true);
    for abandon in [true, false] {
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let held_core = c.clone();
        let held = tokio::task::spawn_blocking(move || {
            let _serial = held_core.file_actions.lock().unwrap();
            ready_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        ready_rx.await.unwrap();
        let core = c.clone();
        let queued = input.clone();
        let mut task =
            tokio::spawn(async move { modules::dispatch(&core, "files.undo", &queued).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!task.is_finished());
        if abandon {
            task.abort();
            assert!((&mut task).await.unwrap_err().is_cancelled());
        } else {
            c.store
                .execute(
                    "UPDATE module_configs SET enabled=0 WHERE module_id='files'",
                    &[],
                )
                .unwrap();
        }
        release_tx.send(()).unwrap();
        if !abandon {
            assert!(task
                .await
                .unwrap()
                .unwrap_err()
                .starts_with("MODULE_DISABLED"));
        }
        held.await.unwrap();
        assert!(!source.exists());
        assert!(target.exists());
        assert_eq!(
            c.store
                .query("SELECT state FROM undo_records WHERE id=?", &[json!(id)])
                .unwrap()[0]["state"],
            "blocked"
        );
    }
    finish(c);
}

#[test]
fn write_revalidation_preserves_edits_rejects_identical_backups_and_restores_native_identity() {
    let c = fixture();
    let path = c.root.join("temp/written.txt");
    std::fs::write(&path, "before").unwrap();
    let original_id = identity(&open(&path, false, true).unwrap()).unwrap();
    let result = write(&c, &path, "after", &Control::default()).unwrap();
    let id = result["undo_token"].as_str().unwrap();
    let checkpoint = Path::new(result["checkpoint"].as_str().unwrap());
    c.store
        .execute(
            "UPDATE undo_records SET state='blocked' WHERE id=?",
            &[json!(id)],
        )
        .unwrap();
    std::fs::write(&path, "later edit").unwrap();
    assert!(undo::execute_request(&c, id, true, &Control::default())
        .unwrap_err()
        .starts_with("UNDO_FILE_CHANGED"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "later edit");
    std::fs::write(&path, "after").unwrap();
    let retained = checkpoint.with_extension("preserved");
    std::fs::rename(checkpoint, &retained).unwrap();
    std::fs::copy(&retained, checkpoint).unwrap();
    assert_eq!(
        undo::execute_request(&c, id, true, &Control::default()).unwrap_err(),
        "UNDO_CHECKPOINT_REPLACED"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "after");
    assert_eq!(std::fs::read_to_string(&retained).unwrap(), "before");
    std::fs::remove_file(checkpoint).unwrap();
    std::fs::rename(&retained, checkpoint).unwrap();
    undo::execute_request(&c, id, true, &Control::default()).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "before");
    assert_eq!(
        identity(&open(&path, false, true).unwrap()).unwrap(),
        original_id
    );
    // A legacy record cannot gain a native recovery contract through this flag.
    let token = undo::prepare(
        &c,
        "copy",
        &path,
        Some(&c.root.join("temp/legacy")),
        None,
        "none",
    )
    .unwrap();
    c.store
        .execute(
            "UPDATE undo_records SET state='blocked' WHERE id=?",
            &[json!(token)],
        )
        .unwrap();
    assert_eq!(
        undo::execute_request(&c, &token, true, &Control::default()).unwrap_err(),
        "UNDO_REVALIDATION_REQUIRES_NATIVE_JOURNAL"
    );
    finish(c);
}

struct ScopeFixture {
    core: Arc<Core>,
    parent: PathBuf,
    source: PathBuf,
}
impl ScopeFixture {
    fn new(directory: bool) -> Self {
        let core = fixture();
        let parent = std::env::temp_dir().join(format!("ap-scope-{}", storage::id()));
        std::fs::create_dir(&parent).unwrap();
        let source = parent.join(if directory { "root" } else { "root.txt" });
        if directory {
            std::fs::create_dir_all(source.join("nested/empty")).unwrap();
            std::fs::write(source.join("nested/file.txt"), b"scope").unwrap();
        } else {
            std::fs::write(&source, b"scope").unwrap();
        }
        core.store
            .set_setting("allowed_roots", &json!([source]))
            .unwrap();
        Self {
            core,
            parent,
            source,
        }
    }
    fn restart(&mut self) {
        let root = self.core.root.clone();
        self.core = Arc::new(
            Core::new(root, PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap(),
        );
    }
    fn finish(self) {
        assert!(self
            .parent
            .canonicalize()
            .unwrap()
            .starts_with(std::env::temp_dir().canonicalize().unwrap()));
        assert!(self
            .parent
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("ap-scope-"));
        finish(self.core);
        std::fs::remove_dir_all(self.parent).unwrap();
    }
}
#[tokio::test]
async fn real_authorized_root_move_rename_delete_restart_undo_never_authorize_parent() {
    for directory in [false, true] {
        let mut f = ScopeFixture::new(directory);
        let original = hash(&f.core, &f.source, &Control::default()).unwrap();
        let original_id = identity(&open(&f.source, false, false).unwrap()).unwrap();
        let sibling = f.parent.join("sibling.txt");
        std::fs::write(&sibling, b"outside").unwrap();
        for action in ["move", "rename", "delete"] {
            let target = f.core.root.join(format!("temp/{action}"));
            let result = modules::dispatch(
                &f.core,
                &format!("files.{action}"),
                &json!({"path":f.source,"destination":target}),
            )
            .await
            .unwrap();
            assert!(!f.source.exists());
            assert!(f
                .core
                .path(f.parent.to_str().unwrap(), false)
                .unwrap_err()
                .starts_with("PATH_SCOPE"));
            assert!(f
                .core
                .path(sibling.to_str().unwrap(), false)
                .unwrap_err()
                .starts_with("PATH_SCOPE"));
            assert_eq!(
                f.core.path(f.source.to_str().unwrap(), false).unwrap_err(),
                "PATH_NOT_FOUND"
            );
            assert!(f.core.path(f.source.to_str().unwrap(), true).is_ok());
            f.restart();
            undo::execute(&f.core, result["undo_token"].as_str().unwrap()).unwrap();
            assert_eq!(
                hash(&f.core, &f.source, &Control::default()).unwrap(),
                original
            );
            assert_eq!(
                identity(&open(&f.source, false, false).unwrap()).unwrap(),
                original_id
            );
            assert_eq!(std::fs::read(&sibling).unwrap(), b"outside");
        }
        f.finish();
    }
    println!("FILE_SCOPE_ROOT_CONTRACT_PASS: exact authorized file/tree root/move/rename/delete/restart/identity/parent excluded");
}
#[tokio::test]
async fn authorized_missing_root_revocation_and_conflict_preserve_both_objects() {
    let mut f = ScopeFixture::new(true);
    let target = f.core.root.join("temp/moved");
    let result = modules::dispatch(
        &f.core,
        "files.move",
        &json!({"path":f.source,"destination":target}),
    )
    .await
    .unwrap();
    let token = result["undo_token"].as_str().unwrap();
    f.core
        .store
        .set_setting("allowed_roots", &json!([]))
        .unwrap();
    assert!(undo::execute(&f.core, token)
        .unwrap_err()
        .starts_with("PATH_SCOPE"));
    assert!(target.exists());
    assert!(!f.source.exists());
    f.core
        .store
        .set_setting("allowed_roots", &json!([f.source]))
        .unwrap();
    std::fs::create_dir(&f.source).unwrap();
    std::fs::write(f.source.join("foreign.txt"), b"foreign").unwrap();
    assert_eq!(
        undo::execute(&f.core, token).unwrap_err(),
        "UNDO_DESTINATION_EXISTS"
    );
    assert_eq!(
        std::fs::read(f.source.join("foreign.txt")).unwrap(),
        b"foreign"
    );
    assert!(target.exists());
    std::fs::remove_dir_all(&f.source).unwrap();
    f.restart();
    undo::execute(&f.core, token).unwrap();
    assert!(f.source.join("nested/empty").is_dir());
    f.finish();
}
#[tokio::test]
async fn changed_source_parent_identity_blocks_root_restore_and_startup_recovery() {
    let mut f = ScopeFixture::new(true);
    let target = f.core.root.join("temp/moved");
    let result = modules::dispatch(
        &f.core,
        "files.move",
        &json!({"path":f.source,"destination":target}),
    )
    .await
    .unwrap();
    let token = result["undo_token"].as_str().unwrap();
    let parked = f
        .parent
        .with_file_name(format!("ap-scope-{}", storage::id()));
    std::fs::rename(&f.parent, &parked).unwrap();
    std::fs::create_dir(&f.parent).unwrap();
    assert_eq!(
        undo::execute(&f.core, token).unwrap_err(),
        "UNDO_SOURCE_PARENT_REPLACED"
    );
    assert!(target.exists());
    assert!(!f.source.exists());
    f.core
        .store
        .execute(
            "UPDATE undo_records SET state='prepared' WHERE id=?",
            &[json!(token)],
        )
        .unwrap();
    f.restart();
    assert_eq!(
        f.core
            .store
            .query("SELECT state FROM undo_records WHERE id=?", &[json!(token)])
            .unwrap()[0]["state"],
        "blocked"
    );
    std::fs::remove_dir(&f.parent).unwrap();
    std::fs::rename(&parked, &f.parent).unwrap();
    f.finish();
}
#[tokio::test]
async fn missing_root_permissions_reject_dangling_junction_and_nonexistent_ancestors() {
    let f = ScopeFixture::new(true);
    let target = f.parent.join("junction-target");
    std::fs::create_dir(&target).unwrap();
    let junction = f.parent.join("junction");
    let result = std::process::Command::new("cmd")
        .args([
            "/d",
            "/c",
            "mklink",
            "/J",
            junction.to_str().unwrap(),
            target.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(result.status.success());
    std::fs::remove_dir(&target).unwrap();
    f.core
        .store
        .set_setting("allowed_roots", &json!([junction]))
        .unwrap();
    assert_eq!(
        f.core.path(junction.to_str().unwrap(), true).unwrap_err(),
        "PATH_REPARSE_DENIED"
    );
    std::fs::remove_dir(&junction).unwrap();
    let missing = f.parent.join("missing/root");
    f.core
        .store
        .set_setting("allowed_roots", &json!([missing]))
        .unwrap();
    assert_eq!(
        f.core.path(missing.to_str().unwrap(), true).unwrap_err(),
        "PATH_RESOLVE"
    );
    assert!(f
        .core
        .path(f.parent.to_str().unwrap(), false)
        .unwrap_err()
        .starts_with("PATH_SCOPE"));
    f.finish();
}

#[tokio::test]
async fn copy_to_a_new_exact_root_stages_in_authorized_backup_without_parent_access() {
    let f = ScopeFixture::new(true);
    let destination = f.parent.join("new-root");
    let source = f.core.root.join("temp/copy-source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("file.txt"), b"copy").unwrap();
    f.core
        .store
        .set_setting("allowed_roots", &json!([destination]))
        .unwrap();
    let before = hash(&f.core, &source, &Control::default()).unwrap();
    let result = modules::dispatch(
        &f.core,
        "files.copy",
        &json!({"path":source,"destination":destination}),
    )
    .await
    .unwrap();
    assert_eq!(
        hash(&f.core, &destination, &Control::default()).unwrap(),
        before
    );
    assert!(f
        .core
        .path(f.parent.to_str().unwrap(), false)
        .unwrap_err()
        .starts_with("PATH_SCOPE"));
    undo::execute(&f.core, result["undo_token"].as_str().unwrap()).unwrap();
    assert!(!destination.exists());
    assert_eq!(hash(&f.core, &source, &Control::default()).unwrap(), before);
    f.finish();
}

#[tokio::test]
async fn authorized_root_case_variant_still_restores_the_original_spelling_after_move() {
    for (actual, grant) in [
        ("root", "ROOT"),
        ("root", "ROOT."),
        ("carpeta-ñ", "CARPETA-Ñ"),
    ] {
        let mut f = ScopeFixture::new(true);
        if actual != "root" {
            let renamed = f.parent.join(actual);
            std::fs::rename(&f.source, &renamed).unwrap();
            f.source = renamed;
        }
        let grant = f.source.with_file_name(grant);
        f.core
            .store
            .set_setting("allowed_roots", &json!([grant]))
            .unwrap();
        let destination = f.core.root.join("temp/case-moved");
        let result = modules::dispatch(
            &f.core,
            "files.move",
            &json!({"path":f.source,"destination":destination}),
        )
        .await
        .unwrap();
        assert_eq!(f.core.store.setting("allowed_roots"), json!([f.source]));
        f.restart();
        undo::execute(&f.core, result["undo_token"].as_str().unwrap()).unwrap();
        assert!(f.source.join("nested/empty").is_dir());
        assert!(f
            .core
            .path(f.parent.to_str().unwrap(), false)
            .unwrap_err()
            .starts_with("PATH_SCOPE"));
        f.finish();
    }
    println!("FILE_ROOT_CASE_CONTRACT_PASS: actual Windows spelling/case/trailing-dot/Unicode/persistent scope/restart/undo");
}

#[tokio::test]
async fn legacy_project_root_case_is_normalized_before_source_root_is_moved() {
    let mut f = ScopeFixture::new(true);
    f.core
        .store
        .set_setting("allowed_roots", &json!([]))
        .unwrap();
    let id = storage::id();
    let grant = f.source.with_file_name("ROOT.");
    f.core
        .store
        .execute(
            "INSERT INTO projects(id,name,root_path,created_at) VALUES(?,?,?,?)",
            &[
                json!(id),
                json!("Scope case fixture"),
                json!(grant),
                json!(storage::now()),
            ],
        )
        .unwrap();
    let target = f.core.root.join("temp/project-moved");
    let result = modules::dispatch(
        &f.core,
        "files.move",
        &json!({"path":f.source,"destination":target}),
    )
    .await
    .unwrap();
    assert_eq!(
        f.core
            .store
            .query("SELECT root_path FROM projects WHERE id=?", &[json!(id)])
            .unwrap()[0]["root_path"],
        json!(f.source)
    );
    f.restart();
    undo::execute(&f.core, result["undo_token"].as_str().unwrap()).unwrap();
    assert!(f.source.join("nested/empty").is_dir());
    f.finish();
}
#[tokio::test]
async fn real_trees_copy_move_rename_delete_and_restart_undo() {
    let mut c = fixture();
    let path = tree(&c);
    let digest = hash(&c, &path, &Control::default()).unwrap();
    for action in ["copy", "move", "rename", "delete"] {
        let destination = c.root.join(format!("temp/{action}"));
        let out = modules::dispatch(
            &c,
            &format!("files.{action}"),
            &json!({"path":path,"destination":destination}),
        )
        .await
        .unwrap();
        let token = out["undo_token"].as_str().unwrap().to_owned();
        assert_eq!(path.exists(), action == "copy");
        let root = c.root.clone();
        drop(c);
        c = Arc::new(
            Core::new(root, PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap(),
        );
        undo::execute(&c, &token).unwrap();
        assert_eq!(hash(&c, &path, &Control::default()).unwrap(), digest);
        assert!(path.join("nested/empty").is_dir());
        assert!(!destination.exists());
        assert_eq!(undo::execute(&c, &token).unwrap_err(), "UNDO_NOT_AVAILABLE");
    }
    println!(
        "FILES_TREE_CONTRACT_PASS: nested/empty/unicode copy/move/rename/delete/restart/one-use"
    );
    finish(c);
}
#[tokio::test]
async fn real_tree_changes_occupied_targets_and_checkpoints_are_preserved() {
    let c = fixture();
    let path = tree(&c);
    let dest = c.root.join("temp/moved");
    let out = modules::dispatch(&c, "files.move", &json!({"path":path,"destination":dest}))
        .await
        .unwrap();
    let token = out["undo_token"].as_str().unwrap();
    std::fs::create_dir(dest.join("added-empty")).unwrap();
    assert!(undo::execute(&c, token)
        .unwrap_err()
        .starts_with("UNDO_FILE_CHANGED"));
    assert!(dest.join("added-empty").exists());
    std::fs::remove_dir(dest.join("added-empty")).unwrap();
    std::fs::write(dest.join("nested/ñ.txt"), "Cambio posterior").unwrap();
    assert!(undo::execute(&c, token)
        .unwrap_err()
        .starts_with("UNDO_FILE_CHANGED"));
    std::fs::write(dest.join("nested/ñ.txt"), "Contenido sintético").unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(
        undo::execute(&c, token).unwrap_err(),
        "UNDO_DESTINATION_EXISTS"
    );
    std::fs::remove_dir(&path).unwrap();
    undo::execute(&c, token).unwrap();
    std::fs::create_dir(&dest).unwrap();
    assert_eq!(
        modules::dispatch(&c, "files.copy", &json!({"path":path,"destination":dest}))
            .await
            .unwrap_err(),
        "DESTINATION_EXISTS"
    );
    assert!(dest.is_dir());
    let deleted = modules::dispatch(&c, "files.delete", &json!({"path":path}))
        .await
        .unwrap();
    let checkpoint = PathBuf::from(deleted["checkpoint"].as_str().unwrap());
    std::fs::write(checkpoint.join("zero.bin"), "Checkpoint cambiado").unwrap();
    assert!(undo::execute(&c, deleted["undo_token"].as_str().unwrap())
        .unwrap_err()
        .starts_with("UNDO_FILE_CHANGED"));
    assert!(!path.exists());
    assert_eq!(
        std::fs::read_to_string(checkpoint.join("zero.bin")).unwrap(),
        "Checkpoint cambiado"
    );
    finish(c);
}
#[tokio::test]
async fn real_tree_lock_reparse_hardlink_stream_limits_cancellation_and_scope() {
    use std::os::windows::fs::OpenOptionsExt;
    let c = fixture();
    let path = tree(&c);
    let dest = c.root.join("temp/copied");
    let locked = OpenOptions::new()
        .write(true)
        .share_mode(0)
        .open(path.join("zero.bin"))
        .unwrap();
    assert!(modules::dispatch(&c, "files.delete", &json!({"path":path}))
        .await
        .unwrap_err()
        .contains("TREE_LOCK_OR_ACCESS"));
    assert!(path.exists());
    drop(locked);
    std::fs::hard_link(path.join("zero.bin"), path.join("hardlink.bin")).unwrap();
    assert_eq!(
        modules::dispatch(&c, "files.copy", &json!({"path":path,"destination":dest}))
            .await
            .unwrap_err(),
        "TREE_HARDLINK_DENIED"
    );
    std::fs::remove_file(path.join("hardlink.bin")).unwrap();
    std::fs::write(path.join("zero.bin:hidden"), "stream").unwrap();
    assert_eq!(
        modules::dispatch(&c, "files.copy", &json!({"path":path,"destination":dest}))
            .await
            .unwrap_err(),
        "TREE_ALTERNATE_STREAM_DENIED"
    );
    std::fs::remove_file(path.join("zero.bin:hidden")).unwrap();
    // Junctions need no symlink privilege and must not be traversed or copied.
    let junction = path.join("junction");
    let target = c.root.join("temp/target");
    std::fs::create_dir(&target).unwrap();
    let status = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(junction.to_string_lossy().replace('/', "\\"))
        .arg(target.to_string_lossy().replace('/', "\\"))
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "junction fixture creation failed: {} {}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    );
    assert_eq!(
        modules::dispatch(&c, "files.delete", &json!({"path":path}))
            .await
            .unwrap_err(),
        "PATH_REPARSE_DENIED"
    );
    std::fs::remove_dir(&junction).unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        mutate(&c, "delete", &path, None, &Control::new(cancel)).unwrap_err(),
        "FILES_CANCELLED"
    );
    assert!(path.exists());
    assert_eq!(
        modules::dispatch(&c, "files.delete", &json!({"path":c.root.join("temp")}))
            .await
            .unwrap_err(),
        "TREE_PROFILE_ROOT_PROTECTED"
    );
    let large = path.join("large.bin");
    File::create(&large)
        .unwrap()
        .set_len(MAX_BYTES + 1)
        .unwrap();
    assert_eq!(
        modules::dispatch(&c, "files.delete", &json!({"path":path}))
            .await
            .unwrap_err(),
        "TREE_SIZE_LIMIT"
    );
    std::fs::remove_file(large).unwrap();
    c.store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='files'",
            &[],
        )
        .unwrap();
    assert!(modules::dispatch(&c, "files.delete", &json!({"path":path}))
        .await
        .unwrap_err()
        .starts_with("MODULE_DISABLED"));
    assert!(path.exists());
    finish(c);
}
#[tokio::test]
async fn real_compound_rollback_nested_edits_partial_resume_and_crash_cursor() {
    let mut c = fixture();
    let source = tree(&c);
    let copy = c.root.join("temp/copy");
    let first = modules::dispatch(&c, "files.copy", &json!({"path":source,"destination":copy}))
        .await
        .unwrap();
    let edit = modules::dispatch(
        &c,
        "files.write",
        &json!({"path":copy.join("zero.bin"),"text":"new"}),
    )
    .await
    .unwrap();
    let deleted = modules::dispatch(&c, "files.delete", &json!({"path":copy.join("zero.bin")}))
        .await
        .unwrap();
    let result = modules::dispatch(
        &c,
        "files.rollback",
        &json!({"body":{"tokens":[first["undo_token"],edit["undo_token"],deleted["undo_token"]]}}),
    )
    .await
    .unwrap();
    assert_eq!(result["state"], "completed");
    assert!(!copy.exists());
    assert!(source.exists());
    let a = c.root.join("temp/a.txt");
    let b = c.root.join("temp/b.txt");
    let first = modules::dispatch(&c, "files.write", &json!({"path":a,"text":"a"}))
        .await
        .unwrap();
    let second = modules::dispatch(&c, "files.write", &json!({"path":b,"text":"b"}))
        .await
        .unwrap();
    std::fs::write(&a, "later").unwrap();
    let error = modules::dispatch(
        &c,
        "files.rollback",
        &json!({"body":{"tokens":[first["undo_token"],second["undo_token"]]}}),
    )
    .await
    .unwrap_err();
    assert!(error.starts_with("ROLLBACK_STOPPED"), "{error}");
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "later");
    assert!(!b.exists());
    let row = c
        .store
        .query("SELECT * FROM file_rollbacks WHERE state='blocked'", &[])
        .unwrap()[0]
        .clone();
    assert_eq!(row["next_index"], 1);
    // Simulate committed undo with a group cursor not yet advanced on restart.
    c.store
        .execute(
            "UPDATE file_rollbacks SET state='running',next_index=0 WHERE id=?",
            &[row["id"].clone()],
        )
        .unwrap();
    let root = c.root.clone();
    drop(c);
    c = Arc::new(Core::new(root, PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap());
    assert_eq!(
        c.store
            .query(
                "SELECT state FROM file_rollbacks WHERE id=?",
                &[row["id"].clone()]
            )
            .unwrap()[0]["state"],
        "interrupted"
    );
    std::fs::write(&a, "a").unwrap();
    let result = modules::dispatch(&c, "files.rollback", &json!({"body":{"id":row["id"]}}))
        .await
        .unwrap();
    assert_eq!(result["state"], "completed");
    assert!(!a.exists());
    assert_eq!(
        crate::policy::classify("files.rollback", &json!({})),
        crate::policy::Risk::Mandatory
    );
    finish(c);
}

#[tokio::test]
async fn real_root_replacement_same_contents_permissions_and_schema_are_rejected() {
    let c = fixture();
    let source = tree(&c);
    let dest = c.root.join("temp/moved");
    let out = modules::dispatch(&c, "files.move", &json!({"path":source,"destination":dest}))
        .await
        .unwrap();
    let previous = c.root.join("temp/previous");
    std::fs::rename(&dest, &previous).unwrap();
    std::fs::create_dir_all(dest.join("nested/empty")).unwrap();
    std::fs::write(dest.join("nested/ñ.txt"), "Contenido sintético").unwrap();
    std::fs::write(dest.join("zero.bin"), []).unwrap();
    assert_eq!(
        hash(&c, &previous, &Control::default()).unwrap(),
        hash(&c, &dest, &Control::default()).unwrap()
    );
    assert_eq!(
        undo::execute(&c, out["undo_token"].as_str().unwrap()).unwrap_err(),
        "UNDO_OBJECT_REPLACED"
    );
    assert!(previous.exists() && dest.exists());
    std::fs::remove_dir_all(&dest).unwrap();
    std::fs::rename(previous, &dest).unwrap();
    let request = json!({"body":{"tokens":[out["undo_token"]]}});
    let schema = crate::catalog::tool_schema("files.rollback", "body");
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&request));
    assert!(!validator.is_valid(&json!({"body":{"tokens":[out["undo_token"],out["undo_token"]]}})));
    assert!(!validator.is_valid(&json!({"body":{"tokens":[],"id":storage::id()}})));
    let review = undo::review(&c, "files.rollback", &request).unwrap();
    assert_eq!(
        PathBuf::from(
            review["operations_in_undo_order"][0]["original"]
                .as_str()
                .unwrap()
        ),
        source
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .join(source.file_name().unwrap())
    );
    assert_eq!(
        PathBuf::from(
            review["operations_in_undo_order"][0]["destination"]
                .as_str()
                .unwrap()
        ),
        dest.canonicalize().unwrap()
    );
    assert_eq!(review["operations_in_undo_order"][0]["state"], "available");
    let denied = c.execute("files.rollback", request).await.unwrap_err();
    assert!(!denied.is_empty());
    assert!(dest.exists());
    assert_eq!(
        c.store
            .query("SELECT count(*) AS n FROM file_rollbacks", &[])
            .unwrap()[0]["n"],
        0
    );
    undo::execute(&c, out["undo_token"].as_str().unwrap()).unwrap();
    finish(c);
}

#[test]
fn migration_seven_preserves_legacy_journals_and_scope_leases_revalidate() {
    let c = fixture();
    let file = c.root.join("temp/old.txt");
    let copy = c.root.join("temp/copied.txt");
    std::fs::write(&file, "old").unwrap();
    std::fs::copy(&file, &copy).unwrap();
    let token = undo::prepare(
        &c,
        "copy",
        &file,
        Some(&copy),
        None,
        &modules::hash(&copy).unwrap(),
    )
    .unwrap();
    undo::commit(&c, &token).unwrap();
    let control = Control::default();
    control.check(&c).unwrap();
    c.store
        .set_setting("allowed_roots", &json!([c.root.join("temp")]))
        .unwrap();
    assert_eq!(control.check(&c).unwrap_err(), "FILES_SCOPE_CHANGED");
    let held = parents(&c, &file).unwrap();
    assert!(std::fs::rename(c.root.join("temp"), c.root.join("renamed")).is_err());
    drop(held);
    let path = c.root.join("data/alfredpet.db");
    let root = c.root.clone();
    let backups_before = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("pre-v7")
        })
        .count();
    drop(c);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(crate::clipboard::TEST_REMOVE_FUTURE_SCHEMA)
        .unwrap();
    db.execute_batch("DROP TABLE file_cleanup_plans;ALTER TABLE undo_records DROP COLUMN metadata_json;DROP TABLE file_rollbacks;ALTER TABLE undo_records DROP COLUMN staging;ALTER TABLE undo_records DROP COLUMN object_id;PRAGMA user_version=6;").unwrap();
    drop(db);
    let c =
        Arc::new(Core::new(root, PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap());
    assert_eq!(
        c.store.query("PRAGMA user_version", &[]).unwrap()[0]["user_version"],
        crate::storage::SCHEMA_VERSION
    );
    assert_eq!(
        c.store
            .query("SELECT state FROM undo_records WHERE id=?", &[json!(token)])
            .unwrap()[0]["state"],
        "available"
    );
    undo::execute(&c, &token).unwrap();
    assert!(file.exists() && !copy.exists());
    let backups_after = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("pre-v7")
        })
        .count();
    assert_eq!(backups_after, backups_before + 1);
    finish(c);
}

#[tokio::test]
async fn queued_file_mutations_stop_on_revocation_and_abandoned_future() {
    for abandon in [false, true] {
        let c = fixture();
        let source = tree(&c);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let held_core = c.clone();
        let held = tokio::task::spawn_blocking(move || {
            let _lease = held_core.file_actions.lock().unwrap();
            ready_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        ready_rx.await.unwrap();
        let pending_core = c.clone();
        let path = source.clone();
        let pending = tokio::spawn(async move {
            modules::dispatch(&pending_core, "files.delete", &json!({"path":path})).await
        });
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(!pending.is_finished());
        if abandon {
            pending.abort();
            assert!(pending.await.unwrap_err().is_cancelled());
            release_tx.send(()).unwrap();
        } else {
            c.store
                .execute(
                    "UPDATE module_configs SET enabled=0 WHERE module_id='files'",
                    &[],
                )
                .unwrap();
            release_tx.send(()).unwrap();
            assert!(pending
                .await
                .unwrap()
                .unwrap_err()
                .starts_with("MODULE_DISABLED"));
        }
        held.await.unwrap();
        // A dropped spawn_blocking future still has to leave its worker safely.
        for _ in 0..100 {
            if Arc::strong_count(&c) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(Arc::strong_count(&c), 1);
        assert!(source.join("nested/ñ.txt").exists());
        assert_eq!(
            c.store
                .query("SELECT count(*) AS n FROM undo_records", &[])
                .unwrap()[0]["n"],
            0
        );
        finish(c);
    }
}
