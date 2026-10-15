use super::*;
use crate::{modules, undo};

fn fixture() -> Arc<Core> {
    Arc::new(
        Core::new(
            std::env::temp_dir().join(format!("ap-maintenance-{}", storage::id())),
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
fn enable(c: &Core, staging: Value) {
    c.store
        .set_setting("file_checkpoint_days", &json!(30))
        .unwrap();
    c.store
        .set_setting(
            "file_maintenance",
            &json!({"enabled":true,"staging_days":staging,"include_recovery":false}),
        )
        .unwrap();
}
async fn retained(c: &Arc<Core>) -> (String, PathBuf) {
    let source = c.root.join(format!("temp/source-{}", storage::id()));
    let target = c.root.join(format!("temp/target-{}", storage::id()));
    std::fs::write(&source, "original").unwrap();
    let copied = modules::dispatch(
        c,
        "files.copy",
        &json!({"path":source,"destination":target}),
    )
    .await
    .unwrap();
    let id = copied["undo_token"].as_str().unwrap().to_owned();
    undo::execute(c, &id).unwrap();
    c.store
        .execute(
            "UPDATE undo_records SET updated_at='2020-01-01T00:00:00Z' WHERE id=?",
            &[json!(id)],
        )
        .unwrap();
    let row = c
        .store
        .query("SELECT * FROM undo_records WHERE id=?", &[json!(id)])
        .unwrap()[0]
        .clone();
    (id, PathBuf::from(row["checkpoint"].as_str().unwrap()))
}
fn prepare_plan(c: &Core) -> Value {
    let settings = policy(c);
    let mut state = json!({"cursor":null,"seen":[],"policy_key":"test"});
    let id = prepare(c, &Control::default(), &settings, &mut state)
        .unwrap()
        .unwrap();
    json!(id)
}
#[tokio::test]
async fn default_off_and_every_background_batch_requires_current_l3_permission() {
    let mut c = fixture();
    let (_, artifact) = retained(&c).await;
    assert_eq!(cycle(&c).await.unwrap()["status"], "disabled");
    assert!(c
        .store
        .query("SELECT * FROM file_cleanup_plans", &[])
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    for value in [
        json!(true),
        json!({}),
        json!({"enabled":true,"staging_days":0,"include_recovery":false}),
        json!({"enabled":true,"staging_days":null,"include_recovery":false,"force":true}),
        json!({"enabled":"yes","staging_days":null,"include_recovery":false}),
    ] {
        assert!(validate(&value).is_err());
    }
    enable(&c, Value::Null);
    let result = cycle(&c).await.unwrap();
    assert_eq!(result["status"], "retained");
    assert!(artifact.exists());
    let requests = c
        .store
        .query(
            "SELECT risk,status FROM permission_requests WHERE tool_id='files.cleanup'",
            &[],
        )
        .unwrap();
    assert_eq!(requests.as_array().unwrap().len(), 1);
    assert_eq!(requests[0]["risk"], "Mandatory");
    assert_eq!(requests[0]["status"], "cancelled"); // Headless fixture has no permission surface.
    assert_eq!(cycle(&c).await.unwrap()["status"], "scanned");
    assert_eq!(
        c.store
            .query("SELECT * FROM file_cleanup_plans", &[])
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let root = c.root.clone();
    drop(c);
    c = Arc::new(Core::new(root, PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap());
    cycle(&c).await.unwrap();
    assert_eq!(
        c.store
            .query(
                "SELECT * FROM permission_requests WHERE tool_id='files.cleanup'",
                &[]
            )
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(artifact.exists());
    finish(c);
}
#[tokio::test]
async fn bounded_rotation_pages_past_absent_artifacts_and_retention_offsets() {
    let c = fixture();
    let (_, artifact) = retained(&c).await;
    // 105 older valid journals without artifacts must not starve the owned copy.
    for _ in 0..105 {
        let id = undo::prepare_tree(
            &c,
            "tree_copy",
            &c.root.join("temp/absent"),
            Some(&c.root.join("temp/also-absent")),
            None,
            "unused",
        )
        .unwrap();
        c.store.execute("UPDATE undo_records SET state='undone',updated_at='2019-01-01T01:00:00+01:00' WHERE id=?",&[json!(id)]).unwrap();
    }
    enable(&c, Value::Null);
    assert_eq!(cycle(&c).await.unwrap()["scanned"], 100);
    let next = cycle(&c).await.unwrap();
    assert_eq!(next["scanned"], 6);
    assert_eq!(next["status"], "retained");
    assert!(artifact.exists());
    let manual = cleanup::retention(&c, &Control::default()).unwrap();
    assert!(manual["plan_id"].is_null());
    assert!(cleanup::retention(&c, &Control::default()).unwrap()["plan_id"].is_string());
    finish(c);
}
#[tokio::test]
async fn real_staging_rotation_preserves_available_checkpoint_and_native_undo() {
    let c = fixture();
    let source = c.root.join("temp/source.txt");
    std::fs::write(&source, "useful recovery").unwrap();
    let deleted = modules::dispatch(&c, "files.delete", &json!({"path":source}))
        .await
        .unwrap();
    let id = deleted["undo_token"].clone();
    let checkpoint = PathBuf::from(deleted["checkpoint"].as_str().unwrap());
    let stage = c
        .root
        .join(format!("temp/.alfredpet-{}.staging", storage::id()));
    std::fs::create_dir(&stage).unwrap();
    std::fs::write(stage.join("partial.txt"), "incomplete output").unwrap();
    let row = c
        .store
        .query(
            "SELECT * FROM undo_records WHERE id=?",
            std::slice::from_ref(&id),
        )
        .unwrap()[0]
        .clone();
    let mut data = metadata(&row).unwrap();
    data["stage_id"] = json!(identity(&open(&stage, false, false).unwrap()).unwrap());
    c.store.execute("UPDATE undo_records SET staging=?,metadata_json=?,updated_at='2020-01-01T00:00:00Z' WHERE id=?",&[json!(stage),json!(data.to_string()),id.clone()]).unwrap();
    enable(&c, json!(7));
    let plan = prepare_plan(&c);
    let review = cleanup::review(&c, &json!({"plan_id":plan})).unwrap();
    assert_eq!(review["artifacts"].as_array().unwrap().len(), 1);
    assert_eq!(review["artifacts"][0]["role"], "staging");
    assert_eq!(review["artifacts"][0]["forgets_recovery"], false);
    cleanup::dispatch(
        &c,
        &json!({"action":"apply","plan_id":plan}),
        &Control::default(),
    )
    .unwrap();
    assert!(!stage.exists() && checkpoint.exists());
    let row = c
        .store
        .query(
            "SELECT state,staging FROM undo_records WHERE id=?",
            std::slice::from_ref(&id),
        )
        .unwrap()[0]
        .clone();
    assert_eq!(row["state"], "available");
    assert!(row["staging"].is_null());
    undo::execute(&c, id.as_str().unwrap()).unwrap();
    assert_eq!(std::fs::read_to_string(source).unwrap(), "useful recovery");
    finish(c);
    println!("FILE_MAINTENANCE_CONTRACT_PASS: bounded native staging deletion preserves checkpoint+undo; L3 required separately");
}
#[tokio::test]
async fn maintenance_policy_changes_and_unknown_staging_never_erase() {
    let c = fixture();
    let (id, artifact) = retained(&c).await;
    enable(&c, json!(7));
    let plan = prepare_plan(&c);
    let original = c.store.setting("file_maintenance");
    c.store
        .set_setting(
            "file_maintenance",
            &json!({"enabled":false,"staging_days":7,"include_recovery":false}),
        )
        .unwrap();
    assert_eq!(
        cleanup::dispatch(
            &c,
            &json!({"action":"apply","plan_id":plan}),
            &Control::default()
        )
        .unwrap_err(),
        "MAINTENANCE_POLICY_CHANGED"
    );
    c.store.set_setting("file_maintenance", &original).unwrap();
    c.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(cycle(&c).await.unwrap()["status"], "privacy");
    c.store.set_setting("privacy", &Value::Null).unwrap();
    let control = Control::default();
    control.bind_maintenance(&c, &policy(&c)).unwrap();
    c.store
        .set_setting("file_checkpoint_days", &json!(31))
        .unwrap();
    assert_eq!(control.check(&c).unwrap_err(), "MAINTENANCE_POLICY_CHANGED");
    let unknown = c
        .root
        .join(format!("temp/.alfredpet-{}.staging", storage::id()));
    std::fs::write(&unknown, "unknown owner").unwrap();
    let row = c
        .store
        .query("SELECT * FROM undo_records WHERE id=?", &[json!(id)])
        .unwrap()[0]
        .clone();
    let mut data = metadata(&row).unwrap();
    data.as_object_mut().unwrap().remove("stage_id");
    c.store
        .execute(
            "UPDATE undo_records SET metadata_json=? WHERE id=?",
            &[json!(data.to_string()), json!(id)],
        )
        .unwrap();
    c.store
        .execute(
            "UPDATE undo_records SET staging=? WHERE id=?",
            &[json!(unknown), json!(id)],
        )
        .unwrap();
    let preview = cleanup::preview_selected(
        &c,
        &json!({"ids":[id],"retention":true,"maintenance_preview":true}),
        &Control::default(),
        |_, role| role == "staging",
    )
    .unwrap();
    assert!(preview["plan_id"].is_null());
    assert_eq!(
        preview["skipped"][0]["reason"],
        "CLEANUP_LEGACY_REQUIRES_REVIEW"
    );
    assert!(unknown.exists() && artifact.exists());
    finish(c);
}
#[tokio::test]
async fn cancelled_maintenance_waiting_for_file_mutex_preserves_batch() {
    let c = fixture();
    let (_, artifact) = retained(&c).await;
    enable(&c, Value::Null);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = c.clone();
    let thread = std::thread::spawn(move || {
        let _lock = holder.file_actions.lock().unwrap();
        ready_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    ready_rx.await.unwrap();
    let worker = c.clone();
    let running = tokio::spawn(async move { cycle(&worker).await });
    for _ in 0..100 {
        if c.file_maintenance_cancel.lock().unwrap().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    cancel(&c).unwrap();
    release_tx.send(()).unwrap();
    thread.join().unwrap();
    let status = running.await.unwrap().unwrap();
    assert_eq!(status["last_error"], "FILES_CANCELLED");
    assert!(artifact.exists());
    assert!(c
        .store
        .query(
            "SELECT * FROM permission_requests WHERE tool_id='files.cleanup'",
            &[]
        )
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    finish(c);
}

#[tokio::test]
async fn explicit_manual_l3_retry_can_resume_disabled_maintenance_but_never_revoked_roots() {
    let c = fixture();
    let (_, artifact) = retained(&c).await;
    enable(&c, Value::Null);
    let id = prepare_plan(&c);
    c.store
        .set_setting(
            "file_maintenance",
            &json!({"enabled":false,"staging_days":null,"include_recovery":false}),
        )
        .unwrap();
    let input = json!({"body":{"action":"apply","plan_id":id,"revalidate_policy":true}});
    assert!(c.execute("files.cleanup", input.clone()).await.is_err());
    assert!(artifact.exists());
    assert_eq!(
        crate::policy::classify("files.cleanup", &input),
        crate::policy::Risk::Mandatory
    );
    cleanup::dispatch(&c, &input["body"], &Control::default()).unwrap();
    assert!(!artifact.exists());
    let (_, second) = retained(&c).await;
    enable(&c, Value::Null);
    let id = prepare_plan(&c);
    c.store
        .set_setting("allowed_roots", &json!([c.root.join("temp")]))
        .unwrap();
    assert_eq!(
        cleanup::dispatch(
            &c,
            &json!({"action":"apply","plan_id":id,"revalidate_policy":true}),
            &Control::default()
        )
        .unwrap_err(),
        "CLEANUP_SCOPE_CHANGED"
    );
    assert!(second.exists());
    finish(c);
}
#[tokio::test]
async fn unresolved_review_limit_preserves_artifacts_until_prior_plans_are_completed() {
    let c = fixture();
    let (_, artifact) = retained(&c).await;
    enable(&c, Value::Null);
    let prior = storage::id();
    c.store.execute("INSERT INTO file_cleanup_plans(id,snapshot_json,created_at,updated_at) VALUES(?,?,?,?)",&[json!(prior),json!("{}"),json!(storage::now()),json!(storage::now())]).unwrap();
    let mut state = json!({"cursor":null,"seen":(0..HISTORY_LIMIT).map(|i|json!({"signature":i.to_string(),"plan_id":prior})).collect::<Vec<_>>(),"policy_key":"test"});
    assert_eq!(
        prepare(&c, &Control::default(), &policy(&c), &mut state).unwrap_err(),
        "MAINTENANCE_REVIEW_HISTORY_FULL"
    );
    assert!(artifact.exists());
    c.store
        .execute(
            "UPDATE file_cleanup_plans SET state='completed' WHERE id=?",
            &[json!(prior)],
        )
        .unwrap();
    assert!(prepare(&c, &Control::default(), &policy(&c), &mut state)
        .unwrap()
        .is_some());
    assert_eq!(state["seen"].as_array().unwrap().len(), 1);
    assert!(artifact.exists());
    finish(c);
}
