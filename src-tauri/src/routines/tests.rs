use super::*;
use std::path::PathBuf;
struct Fixture {
    c: Arc<Core>,
    root: PathBuf,
    macro_id: String,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ap-routines-{}", storage::id()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("owned.txt"), "Routine fixture").unwrap();
        let c = Arc::new(
            Core::new(
                root.join("profile"),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        for module in ["macros", "files"] {
            c.store
                .execute(
                    "UPDATE module_configs SET enabled=1 WHERE module_id=?",
                    &[json!(module)],
                )
                .unwrap();
        }
        c.store
            .set_setting("allowed_roots", &json!([root]))
            .unwrap();
        c.macro_headless_permission
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let macro_id = storage::id();
        let definition = json!({"version":2,"description":"Owned read","parameters":{"source":{"type":"string","required":true},"read":{"type":"boolean","default":false}},"steps":[{"tool":"files.read","input":{"path":{"$parameter":"source"}}}],"error_strategy":"stop"});
        c.store
            .execute(
                "INSERT INTO macros VALUES(?,?,?,1)",
                &[
                    json!(macro_id),
                    json!("Read fixture"),
                    json!(definition.to_string()),
                ],
            )
            .unwrap();
        Self { c, root, macro_id }
    }
    fn save_input(&self) -> Value {
        let inspected = macros::inspect(&self.c, &json!({"id":self.macro_id})).unwrap();
        json!({"name":"Read report","macroId":self.macro_id,"fingerprint":inspected["fingerprint"],"body":{"source":self.root.join("owned.txt"),"read":false}})
    }
    fn save(&self) -> Value {
        dispatch(&self.c, "routines.save", &self.save_input()).unwrap();
        dispatch(&self.c, "routines.list", &json!({})).unwrap()[0].clone()
    }
    async fn pending(&self) -> String {
        tokio::time::timeout(std::time::Duration::from_secs(8), async {
            loop {
                let rows = self.c.store.query("SELECT id FROM permission_requests WHERE tool_id='macros.run' AND status='pending'", &[]).unwrap();
                if let Some(id) = rows[0]["id"].as_str() { if self.c.pending.lock().unwrap().contains_key(id) { return id.to_owned(); } }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.unwrap()
    }
}
fn cleanup(f: Fixture) {
    let root = f.root.clone();
    assert!(root
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("ap-routines-"));
    assert_eq!(root.parent().unwrap(), std::env::temp_dir());
    drop(f);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn stores_typed_inputs_without_running_and_survives_reopen() {
    let f = Fixture::new();
    let saved = f.save();
    assert_eq!(saved["body"]["read"], false);
    assert_eq!(saved["available"], true);
    assert_eq!(
        f.c.store
            .query("SELECT * FROM permission_requests", &[])
            .unwrap(),
        json!([])
    );
    assert!(status(&f.c).unwrap().is_null());
    let reopened = Core::new(f.root.join("profile"), f.c.resource.clone()).unwrap();
    assert_eq!(
        dispatch(&reopened, "routines.list", &json!({})).unwrap()[0]["id"],
        saved["id"]
    );
    drop(reopened);
    cleanup(f);
}
#[tokio::test]
async fn changed_definitions_and_stale_revisions_require_explicit_review() {
    let f = Fixture::new();
    let saved = f.save();
    f.c.store
        .execute(
            "UPDATE macros SET name='Changed' WHERE id=?",
            &[json!(f.macro_id)],
        )
        .unwrap();
    assert_eq!(
        dispatch(&f.c, "routines.list", &json!({})).unwrap()[0]["available"],
        false
    );
    assert!(run(&f.c, &saved).await.unwrap_err().contains("CHANGED"));
    let mut input = f.save_input();
    input["id"] = saved["id"].clone();
    input["revision"] = saved["revision"].clone();
    dispatch(&f.c, "routines.save", &input).unwrap();
    assert!(run(&f.c, &saved).await.unwrap_err().contains("cambiado"));
    assert!(dispatch(
        &f.c,
        "routines.delete",
        &json!({"id":saved["id"],"revision":saved["revision"],"confirmed":true})
    )
    .is_err());
    cleanup(f);
}
#[tokio::test]
async fn privacy_disabled_modules_and_restored_shortcuts_cannot_run() {
    let f = Fixture::new();
    let saved = f.save();
    f.c.store.set_setting("privacy", &json!(true)).unwrap();
    assert!(run(&f.c, &saved).await.is_err());
    f.c.store.set_setting("privacy", &json!(false)).unwrap();
    f.c.store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='macros'",
            &[],
        )
        .unwrap();
    assert!(run(&f.c, &saved).await.is_err());
    f.c.store
        .execute(
            "UPDATE module_configs SET enabled=1 WHERE module_id='macros'",
            &[],
        )
        .unwrap();
    let mut rows = rows(&f.c).unwrap();
    rows[0].needs_review = true;
    f.c.store
        .set_setting(KEY, &serde_json::to_value(rows).unwrap())
        .unwrap();
    assert!(run(&f.c, &saved).await.unwrap_err().contains("restaurada"));
    cleanup(f);
}
#[test]
fn invalid_or_secret_values_and_overflow_are_rejected_and_delete_is_scoped() {
    let f = Fixture::new();
    for body in [
        json!({}),
        json!({"source":3}),
        json!({"source":"sk-ant-owned-fictional-secret"}),
    ] {
        let mut input = f.save_input();
        input["body"] = body;
        assert!(dispatch(&f.c, "routines.save", &input).is_err());
    }
    for _ in 0..LIMIT {
        dispatch(&f.c, "routines.save", &f.save_input()).unwrap();
    }
    assert!(dispatch(&f.c, "routines.save", &f.save_input()).is_err());
    let saved = dispatch(&f.c, "routines.list", &json!({})).unwrap()[0].clone();
    assert!(dispatch(&f.c, "routines.delete", &saved).is_err());
    let mut input = saved;
    input["confirmed"] = json!(true);
    dispatch(&f.c, "routines.delete", &input).unwrap();
    assert_eq!(rows(&f.c).unwrap().len(), LIMIT - 1);
    assert_eq!(macros::list(&f.c).unwrap().as_array().unwrap().len(), 1);
    cleanup(f);
}
#[tokio::test]
async fn run_uses_real_macro_permission_rejects_duplicates_and_records_completion() {
    let f = Fixture::new();
    let saved = f.save();
    let c = f.c.clone();
    let input = saved.clone();
    let task = tokio::spawn(async move { run(&c, &input).await });
    let permission = f.pending().await;
    assert_eq!(status(&f.c).unwrap()["status"], "running");
    assert!(run(&f.c, &saved).await.unwrap_err().contains("en curso"));
    f.c.pending
        .lock()
        .unwrap()
        .remove(&permission)
        .unwrap()
        .send("allow".into())
        .unwrap();
    let result = task.await.unwrap().unwrap();
    assert_eq!(result["status"], "complete");
    assert_eq!(result["succeeded"], 1);
    assert!(!result.to_string().contains("Routine fixture"));
    cleanup(f);
}
#[tokio::test]
async fn denial_runs_no_child_and_cancellation_is_not_replayed() {
    let f = Fixture::new();
    let saved = f.save();
    let c = f.c.clone();
    let input = saved.clone();
    let task = tokio::spawn(async move { run(&c, &input).await });
    let permission = f.pending().await;
    f.c.pending
        .lock()
        .unwrap()
        .remove(&permission)
        .unwrap()
        .send("deny".into())
        .unwrap();
    assert_eq!(task.await.unwrap().unwrap()["status"], "interrupted");
    assert_eq!(
        f.c.store
            .query(
                "SELECT * FROM activity_events WHERE tool_id='files.read'",
                &[]
            )
            .unwrap(),
        json!([])
    );
    let c = f.c.clone();
    let task = tokio::spawn(async move { run(&c, &saved).await });
    f.pending().await;
    task.abort();
    assert!(task.await.is_err());
    assert_eq!(status(&f.c).unwrap()["status"], "uncertain");
    cleanup(f);
}
