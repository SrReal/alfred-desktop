use super::*;
use crate::modules;
use std::sync::Arc;

struct Fixture {
    core: Arc<Core>,
    other: PathBuf,
    workspace: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(".work");
        std::fs::create_dir_all(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let other = workspace.join(format!("cross-volume-{}", storage::id()));
        std::fs::create_dir(&other).unwrap();
        let core = Arc::new(
            Core::new(
                std::env::temp_dir().join(format!("ap-cross-{}", storage::id())),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        core.store
            .set_setting("allowed_roots", &json!([other]))
            .unwrap();
        assert_ne!(
            volume(&open(&core.root.join("temp"), false, true).unwrap()).unwrap(),
            volume(&open(&other, false, true).unwrap()).unwrap(),
            "real two-volume fixture required"
        );
        Self {
            core,
            other,
            workspace,
        }
    }
    fn source(&self, directory: bool) -> PathBuf {
        let source = self
            .other
            .join(if directory { "source" } else { "source.txt" });
        if directory {
            std::fs::create_dir_all(source.join("nested/empty")).unwrap();
            std::fs::write(source.join("nested/ñ.txt"), "Contenido").unwrap();
        } else {
            std::fs::write(&source, "Contenido").unwrap();
        }
        source
    }
    fn restart(&mut self) {
        let root = self.core.root.clone();
        let replacement = Arc::new(
            Core::new(root, PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap(),
        );
        self.core = replacement;
    }
    fn finish(self) {
        let root = self.core.root.clone();
        assert!(self
            .other
            .canonicalize()
            .unwrap()
            .starts_with(&self.workspace));
        assert!(self
            .other
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("cross-volume-"));
        drop(self.core);
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(self.other).unwrap();
    }
}
#[tokio::test]
async fn real_isolated_volume_root_uses_another_authorized_recovery_root() {
    let mut f = Fixture::new();
    let source = f.source(true);
    let recovery = f.other.join("recovery");
    std::fs::create_dir(&recovery).unwrap();
    f.core
        .store
        .set_setting("allowed_roots", &json!([source, recovery]))
        .unwrap();
    let original = hash(&f.core, &source, &Control::default()).unwrap();
    let id = identity(&open(&source, false, false).unwrap()).unwrap();
    assert!(f
        .core
        .path(f.other.to_str().unwrap(), false)
        .unwrap_err()
        .starts_with("PATH_SCOPE"));
    for action in ["delete", "move"] {
        let target = f.core.root.join("temp/root-target");
        let result = modules::dispatch(
            &f.core,
            &format!("files.{action}"),
            &json!({"path":source,"destination":target}),
        )
        .await
        .unwrap();
        assert!(!source.exists());
        assert!(Path::new(result["checkpoint"].as_str().unwrap())
            .starts_with(recovery.canonicalize().unwrap()));
        f.restart();
        undo::execute(&f.core, result["undo_token"].as_str().unwrap()).unwrap();
        assert_eq!(
            hash(&f.core, &source, &Control::default()).unwrap(),
            original
        );
        assert_eq!(identity(&open(&source, false, false).unwrap()).unwrap(), id);
        assert!(!target.exists());
    }
    println!("FILE_SCOPE_VOLUME_CONTRACT_PASS: isolated D root/C destination/authorized D recovery/root identity/restart/delete+move");
    f.finish();
}
#[tokio::test]
async fn isolated_volume_root_without_recovery_permission_is_preserved_before_publication() {
    let f = Fixture::new();
    let source = f.source(true);
    f.core
        .store
        .set_setting("allowed_roots", &json!([source]))
        .unwrap();
    let target = f.core.root.join("temp/target");
    for action in ["delete", "move"] {
        assert!(modules::dispatch(
            &f.core,
            &format!("files.{action}"),
            &json!({"path":source,"destination":target})
        )
        .await
        .unwrap_err()
        .starts_with("TREE_CHECKPOINT_SCOPE"));
        assert!(source.join("nested/empty").is_dir());
        assert!(!target.exists());
        assert_eq!(
            f.core
                .store
                .query("SELECT count(*) AS n FROM undo_records", &[])
                .unwrap()[0]["n"],
            0
        );
    }
    let exact_destination = f.other.join("exact-new-root");
    f.core
        .store
        .set_setting("allowed_roots", &json!([source, exact_destination]))
        .unwrap();
    assert!(modules::dispatch(
        &f.core,
        "files.copy",
        &json!({"path":source,"destination":exact_destination})
    )
    .await
    .unwrap_err()
    .starts_with("TREE_CHECKPOINT_SCOPE"));
    assert!(!exact_destination.exists());
    assert_eq!(std::fs::read_dir(&source).unwrap().count(), 1);
    assert_eq!(
        f.core
            .store
            .query("SELECT count(*) AS n FROM undo_records", &[])
            .unwrap()[0]["n"],
        0
    );
    f.finish();
}
#[tokio::test]
async fn real_two_volumes_move_files_and_trees_restore_original_object_after_restart() {
    for directory in [false, true] {
        let mut f = Fixture::new();
        let source = f.source(directory);
        let tree = Tree::capture(&f.core, &source, false, &Control::default()).unwrap();
        let source_id = tree.identity.clone();
        let digest = tree.digest.clone();
        drop(tree);
        let target = f.core.root.join("temp/target");
        let output = modules::dispatch(
            &f.core,
            "files.move",
            &json!({"path":source,"destination":target}),
        )
        .await
        .unwrap();
        assert_eq!(output["cross_volume"], true);
        assert!(!source.exists());
        assert_eq!(hash(&f.core, &target, &Control::default()).unwrap(), digest);
        let row = f
            .core
            .store
            .query(
                "SELECT * FROM undo_records WHERE id=?",
                &[output["undo_token"].clone()],
            )
            .unwrap()[0]
            .clone();
        assert_ne!(row["object_id"], source_id);
        let retained = PathBuf::from(row["checkpoint"].as_str().unwrap());
        assert!(retained.starts_with(&f.other));
        f.core
            .store
            .execute(
                "UPDATE undo_records SET state='prepared' WHERE id=?",
                &[row["id"].clone()],
            )
            .unwrap();
        f.restart();
        undo::execute(&f.core, row["id"].as_str().unwrap()).unwrap();
        let restored = Tree::capture(&f.core, &source, false, &Control::default()).unwrap();
        assert_eq!(restored.identity, source_id);
        assert_eq!(restored.digest, digest);
        drop(restored);
        assert!(!target.exists() && !retained.exists());
        assert!(meta_path(&f.core, &metadata(&row).unwrap())
            .unwrap()
            .exists());
        let preview = cleanup(
            &f.core,
            &json!({"action":"preview","ids":[row["id"]]}),
            &Control::default(),
        )
        .unwrap();
        assert_eq!(preview["artifacts"].as_array().unwrap().len(), 1);
        cleanup(
            &f.core,
            &json!({"action":"apply","plan_id":preview["plan_id"]}),
            &Control::default(),
        )
        .unwrap();
        assert_eq!(
            Tree::capture(&f.core, &source, false, &Control::default())
                .unwrap()
                .identity,
            source_id
        );
        assert!(!meta_path(&f.core, &metadata(&row).unwrap())
            .unwrap()
            .exists());
        f.finish();
    }
    println!(
        "FILE_CROSS_VOLUME_CONTRACT_PASS: actual distinct volumes/file+tree/identity/restart/undo"
    );
}
#[tokio::test]
async fn real_cross_inverse_interruption_reconciles_parked_target_and_completed_restore() {
    for complete in [false, true] {
        let mut f = Fixture::new();
        let source = f.source(true);
        let target = f.core.root.join("temp/target");
        let output = modules::dispatch(
            &f.core,
            "files.move",
            &json!({"path":source,"destination":target}),
        )
        .await
        .unwrap();
        let row = f
            .core
            .store
            .query(
                "SELECT * FROM undo_records WHERE id=?",
                &[output["undo_token"].clone()],
            )
            .unwrap()[0]
            .clone();
        let data = metadata(&row).unwrap();
        let parked = meta_path(&f.core, &data).unwrap();
        let digest = row["expected_hash"].as_str().unwrap();
        relocate(
            &f.core,
            &target,
            &parked,
            digest,
            row["object_id"].as_str().unwrap(),
            &Control::default(),
        )
        .unwrap();
        if complete {
            relocate(
                &f.core,
                Path::new(row["checkpoint"].as_str().unwrap()),
                &source,
                digest,
                data["source_id"].as_str().unwrap(),
                &Control::default(),
            )
            .unwrap();
        }
        f.core
            .store
            .execute(
                "UPDATE undo_records SET state='running' WHERE id=?",
                &[row["id"].clone()],
            )
            .unwrap();
        f.restart();
        let state = f
            .core
            .store
            .query(
                "SELECT state FROM undo_records WHERE id=?",
                &[row["id"].clone()],
            )
            .unwrap()[0]["state"]
            .clone();
        assert_eq!(state, if complete { "undone" } else { "available" });
        if !complete {
            undo::execute(&f.core, row["id"].as_str().unwrap()).unwrap();
        }
        assert!(source.join("nested/empty").is_dir());
        assert!(!target.exists());
        assert_eq!(hash(&f.core, &source, &Control::default()).unwrap(), digest);
        f.finish();
    }
}
#[tokio::test]
async fn real_cross_copy_only_recovery_preserves_source_edits_and_changed_target_conflicts() {
    let mut f = Fixture::new();
    let source = f.source(false);
    let target = f.core.root.join("temp/target.txt");
    let source_tree = Tree::capture(&f.core, &source, false, &Control::default()).unwrap();
    let retained =
        checkpoint(&f.core, &source, source_tree.bytes * 2, &Control::default()).unwrap();
    let parked = checkpoint(&f.core, &target, 0, &Control::default()).unwrap();
    let token = undo::prepare_tree(
        &f.core,
        "tree_cross_move",
        &source,
        Some(&target),
        Some(&retained),
        &source_tree.digest,
    )
    .unwrap();
    save_metadata(
        &f.core,
        &token,
        &json!({"source_id":source_tree.identity,"undo_target":parked,"phase":"copying"}),
    )
    .unwrap();
    copy(
        &f.core,
        &source_tree,
        &source,
        &target,
        &token,
        &Control::default(),
    )
    .unwrap();
    drop(source_tree);
    std::fs::write(&source, "Cambio posterior en origen").unwrap();
    f.restart();
    std::fs::write(&target, "Cambio posterior en destino").unwrap();
    assert!(undo::execute(&f.core, &token)
        .unwrap_err()
        .starts_with("UNDO_FILE_CHANGED"));
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        "Cambio posterior en origen"
    );
    std::fs::write(&target, "Contenido").unwrap();
    undo::execute(&f.core, &token).unwrap();
    assert!(!target.exists());
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        "Cambio posterior en origen"
    );
    assert!(!retained.exists());
    assert!(parked.exists());
    f.finish();
}
