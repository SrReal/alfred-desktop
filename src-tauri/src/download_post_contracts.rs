use super::*;
struct Fixture {
    core: Arc<Core>,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ap-download-post-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let core = Arc::new(
            Core::new(
                root.join("profile"),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        for module in ["downloads", "macros", "files", "documents", "images"] {
            core.store
                .execute(
                    "UPDATE module_configs SET enabled=1 WHERE module_id=?",
                    &[json!(module)],
                )
                .unwrap();
        }
        core.store
            .set_setting("allowed_roots", &json!([root]))
            .unwrap();
        core.media_headless_permission
            .store(true, std::sync::atomic::Ordering::SeqCst);
        core.macro_headless_permission
            .store(true, std::sync::atomic::Ordering::SeqCst);
        std::fs::write(root.join("finished.txt"), "owned downloaded text").unwrap();
        Self { core, root }
    }
    async fn answer(&self, tool: &str, answer: &str) {
        let tx = tokio::time::timeout(std::time::Duration::from_secs(8), async {
            loop {
                let rows = self
                    .core
                    .store
                    .query(
                        "SELECT id FROM permission_requests WHERE tool_id=? AND status='pending'",
                        &[json!(tool)],
                    )
                    .unwrap();
                if let Some(id) = rows[0]["id"].as_str() {
                    if let Some(tx) = self.core.pending.lock().unwrap().remove(id) {
                        break tx;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Owned post-action permission did not arrive");
        tx.send(answer.into()).unwrap();
    }
    fn start(&self, tool: &str, input: Value) -> tokio::task::JoinHandle<Result<Value, String>> {
        let c = self.core.clone();
        let tool = tool.to_owned();
        tokio::spawn(async move { c.execute(&tool, input).await })
    }
    async fn save(&self, steps: Value) -> Value {
        let task=self.start("downloads.post_actions_save",json!({"name":"Owned manual preset","description":"Private owned preset description","steps":steps}));
        self.answer("downloads.post_actions_save", "allow").await;
        self.answer("macros.save", "allow").await;
        task.await.unwrap().unwrap()
    }
    fn run_input(&self, preset: &Value) -> Value {
        json!({"id":preset["id"],"expected":{"fingerprint":preset["fingerprint"]},"path":self.root.join("finished.txt")})
    }
    fn clean(self) {
        let root = self.root.canonicalize().unwrap();
        assert_eq!(
            root.parent().unwrap(),
            std::env::temp_dir().canonicalize().unwrap()
        );
        assert!(root
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("ap-download-post-"));
        drop(self.core);
        std::fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn schemas_limit_presets_and_exclude_executables_and_source_overrides() {
    let f = Fixture::new();
    assert!(!jsonschema::validator_for(&schema("downloads.post_actions_save")).unwrap().is_valid(&json!({"name":"Bad","steps":[{"tool":"terminal.command","input":{"command":"whoami"}}]})));
    assert!(declaration(
        &f.core,
        &json!({"name":"Bad","steps":[{"tool":"downloads.hash","input":{"path":"other"}}]})
    )
    .unwrap_err()
    .contains("PATH_IS_REVIEWED"));
    assert_eq!(risk("downloads.post_actions_run"), Risk::Mandatory);
    assert!(!crate::policy::tool_redact(
        "downloads.post_actions_save",
        &json!({"preset":"Private fixture","steps":[]})
    )
    .to_string()
    .contains("Private fixture"));
    f.clean();
}
#[tokio::test]
async fn saving_and_running_have_independent_mandatory_reviews_and_never_autorun() {
    let f = Fixture::new();
    let steps =
        json!([{"tool":"downloads.hash","input":{}},{"tool":"downloads.classify","input":{}}]);
    let denied = f.start(
        "downloads.post_actions_save",
        json!({"name":"Denied","steps":steps}),
    );
    f.answer("downloads.post_actions_save", "session").await;
    assert!(denied
        .await
        .unwrap()
        .unwrap_err()
        .contains("PERMISSION_DENIED"));
    assert_eq!(
        f.core
            .store
            .query("SELECT COUNT(*) AS n FROM macros", &[])
            .unwrap()[0]["n"],
        0
    );
    let nested = f.start(
        "downloads.post_actions_save",
        json!({"name":"Child denied","steps":steps}),
    );
    f.answer("downloads.post_actions_save", "allow").await;
    f.answer("macros.save", "deny").await;
    assert!(nested.await.unwrap().is_err());
    assert_eq!(
        f.core
            .store
            .query("SELECT COUNT(*) AS n FROM macros", &[])
            .unwrap()[0]["n"],
        0
    );
    let preset = f.save(steps).await;
    assert_eq!(preset["auto_execute"], false);
    assert_eq!(
        f.core
            .store
            .query(
                "SELECT COUNT(*) AS n FROM activity_events WHERE tool_id='downloads.hash'",
                &[]
            )
            .unwrap()[0]["n"],
        0
    );
    let list = f
        .core
        .execute("downloads.post_actions_list", json!({}))
        .await
        .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["auto_execute"], false);
    let run = f.start("downloads.post_actions_run", f.run_input(&preset));
    f.answer("downloads.post_actions_run", "allow").await;
    f.answer("macros.run", "allow").await;
    let result = run.await.unwrap().unwrap();
    assert_eq!(result["complete"], true);
    assert_eq!(result["auto_execute"], false);
    assert_eq!(
        std::fs::read_to_string(f.root.join("finished.txt")).unwrap(),
        "owned downloaded text"
    );
    f.clean();
}
#[tokio::test]
async fn copy_child_needs_its_own_permission_and_preserves_reviewed_source() {
    let f = Fixture::new();
    let destination = f.root.join("copy.txt");
    let preset = f
        .save(json!([{"tool":"files.copy","input":{"destination":destination}}]))
        .await;
    assert!(!destination.exists());
    let task = f.start("downloads.post_actions_run", f.run_input(&preset));
    f.answer("downloads.post_actions_run", "allow").await;
    f.answer("macros.run", "allow").await;
    f.answer("files.copy", "deny").await;
    assert_eq!(task.await.unwrap().unwrap()["complete"], false);
    assert!(!destination.exists());
    let task = f.start("downloads.post_actions_run", f.run_input(&preset));
    f.answer("downloads.post_actions_run", "allow").await;
    f.answer("macros.run", "allow").await;
    f.answer("files.copy", "allow").await;
    assert_eq!(task.await.unwrap().unwrap()["complete"], true);
    assert_eq!(
        std::fs::read_to_string(destination).unwrap(),
        "owned downloaded text"
    );
    assert_eq!(
        std::fs::read_to_string(f.root.join("finished.txt")).unwrap(),
        "owned downloaded text"
    );
    let rows=f.core.store.query("SELECT input_summary,output_summary FROM activity_events WHERE tool_id LIKE 'downloads.post_actions_%'",&[]).unwrap();
    assert!(!rows
        .to_string()
        .contains("Private owned preset description"));
    f.clean();
}
#[tokio::test]
async fn partial_download_and_changed_definition_are_rejected_before_review() {
    let f = Fixture::new();
    let preset = f.save(json!([{"tool":"downloads.hash","input":{}}])).await;
    let partial = f.root.join("unfinished.crdownload");
    std::fs::write(&partial, "pending").unwrap();
    let mut input = f.run_input(&preset);
    input["path"] = json!(partial);
    assert!(f
        .core
        .execute("downloads.post_actions_run", input)
        .await
        .unwrap_err()
        .contains("DOWNLOAD_NOT_READY"));
    f.core
        .store
        .execute(
            "UPDATE macros SET name='Changed' WHERE id=?",
            &[preset["id"].clone()],
        )
        .unwrap();
    assert!(f
        .core
        .execute("downloads.post_actions_run", f.run_input(&preset))
        .await
        .unwrap_err()
        .contains("PRESET_CHANGED"));
    assert!(f.core.pending.lock().unwrap().is_empty());
    f.clean();
}
#[tokio::test]
async fn source_is_pinned_and_false_restore_revokes_parent_before_child_dispatch() {
    let f = Fixture::new();
    let preset = f.save(json!([{"tool":"downloads.hash","input":{}}])).await;
    let p =
        Prepared::prepare(&f.core, "downloads.post_actions_run", &f.run_input(&preset)).unwrap();
    assert!(std::fs::write(f.root.join("finished.txt"), "changed while reviewed").is_err());
    drop(p);
    let task = f.start("downloads.post_actions_run", f.run_input(&preset));
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        while f.core.pending.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    crate::media::revoke(&f.core).unwrap();
    f.answer("downloads.post_actions_run", "allow").await;
    assert!(task.await.unwrap().unwrap_err().contains("SCOPE_REVOKED"));
    assert_eq!(
        f.core
            .store
            .query(
                "SELECT COUNT(*) AS n FROM activity_events WHERE tool_id='downloads.hash'",
                &[]
            )
            .unwrap()[0]["n"],
        0
    );
    f.clean();
}
