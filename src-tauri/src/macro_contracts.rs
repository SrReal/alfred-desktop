use super::*;
use std::path::PathBuf;
struct Fixture {
    core: Arc<Core>,
    root: PathBuf,
    id: String,
}
impl Fixture {
    fn new(steps: Value, parameters: Value, error_strategy: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ap-macros-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let core = Arc::new(
            Core::new(
                root.join("profile"),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        for module in ["macros", "files"] {
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
        core.macro_headless_permission
            .store(true, std::sync::atomic::Ordering::SeqCst);
        std::fs::write(root.join("owned.txt"), "owned safe content").unwrap();
        let id=save(&core,&json!({"name":"Owned macro","description":"Contract fixture","steps":steps,"parameters":parameters,"error_strategy":error_strategy})).unwrap()["id"].as_str().unwrap().to_owned();
        Self { core, root, id }
    }
    fn input(&self, body: Value) -> Value {
        json!({"id":self.id,"body":body})
    }
    fn path(&self) -> String {
        self.root.join("owned.txt").to_string_lossy().into()
    }
    async fn answer(&self, tool: &str, answer: &str) {
        let sender = tokio::time::timeout(std::time::Duration::from_secs(8), async {
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
                    if let Some(sender) = self.core.pending.lock().unwrap().remove(id) {
                        break sender;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Owned macro approval did not arrive");
        sender.send(answer.into()).unwrap();
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
            .starts_with("ap-macros-"));
        drop(self.core);
        std::fs::remove_dir_all(root).unwrap();
    }
}
fn read_step() -> Value {
    json!({"tool":"files.read","input":{"path":{"$parameter":"source"}},"rollback_hint":"No inverse required for a read"})
}
#[tokio::test]
async fn typed_substitution_defaults_conditions_and_legacy_arrays_keep_core_routing() {
    let f = Fixture::new(
        json!([read_step(),{"tool":"files.read","input":{"path":{"$parameter":"source"}},"when":{"parameter":"enabled","operator":"equals","value":true}}]),
        json!({"source":{"type":"string","required":true},"enabled":{"type":"boolean","default":false}}),
        "stop",
    );
    let p = prepare(&f.core, &f.input(json!({"source":f.path()}))).unwrap();
    assert_eq!(p.review()["steps"][0]["input"]["path"], f.path());
    let result = p.execute(&f.core).await.unwrap();
    assert_eq!(result["complete"], true);
    assert_eq!(result["results"][0]["status"], "success");
    assert_eq!(result["results"][1]["status"], "skipped");
    assert_eq!(result["rollback_hints"][0]["executed"], false);
    let legacy = json!([{"tool":"files.read","input":{"path":f.path()}}]);
    f.core
        .store
        .execute(
            "UPDATE macros SET definition_json=? WHERE id=?",
            &[json!(legacy.to_string()), json!(f.id)],
        )
        .unwrap();
    assert_eq!(
        prepare(&f.core, &f.input(json!({})))
            .unwrap()
            .execute(&f.core)
            .await
            .unwrap()["complete"],
        true
    );
    f.clean();
}
#[test]
fn declarations_and_runtime_parameters_are_closed_bounded_and_nonrecursive() {
    let f = Fixture::new(
        json!([read_step()]),
        json!({"source":{"type":"string","required":true},"choice":{"type":"integer","enum":[1,2]}}),
        "stop",
    );
    for body in [
        json!({}),
        json!({"source":42}),
        json!({"source":f.path(),"choice":3}),
        json!({"source":f.path(),"unknown":true}),
        json!({"source":"sk-ant-owned-fictional-not-a-real-key"}),
    ] {
        assert!(prepare(&f.core, &f.input(body)).is_err());
    }
    for step in [
        json!({"tool":"macros.run","input":{"id":f.id}}),
        json!({"tool":"not_a_tool","input":{}}),
        json!({"tool":"files.read","input":{"path":{"$parameter":"unknown"}}}),
        json!({"tool":"files.read","input":{"path":{"$parameter":"source","other":true}}}),
    ] {
        assert!(save(
            &f.core,
            &json!({"name":"Rejected","steps":[step],"parameters":{"source":{"type":"string"}}})
        )
        .is_err());
    }
    let audit = f
        .core
        .store
        .query(
            "SELECT input_summary,output_summary FROM activity_events",
            &[],
        )
        .unwrap();
    assert!(!audit.to_string().contains("sk-ant-owned"));
    f.clean();
}
#[tokio::test]
async fn stale_definition_before_dispatch_and_during_child_permission_cancels_without_mutation() {
    let f = Fixture::new(
        json!([{"tool":"files.delete","input":{"path":{"$parameter":"source"}}}]),
        json!({"source":{"type":"string","required":true}}),
        "continue",
    );
    let p = prepare(&f.core, &f.input(json!({"source":f.path()}))).unwrap();
    f.core
        .store
        .execute(
            "UPDATE macros SET name='Changed' WHERE id=?",
            &[json!(f.id)],
        )
        .unwrap();
    assert_eq!(
        p.execute(&f.core).await.unwrap_err(),
        "MACRO_CHANGED_REVIEW_REQUIRED"
    );
    let p = prepare(&f.core, &f.input(json!({"source":f.path()}))).unwrap();
    let core = f.core.clone();
    let task = tokio::spawn(async move { p.execute(&core).await });
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        while f.core.pending.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    f.core
        .store
        .execute("UPDATE macros SET enabled=0 WHERE id=?", &[json!(f.id)])
        .unwrap();
    let outcome = task.await.unwrap();
    assert!(outcome.is_err() || outcome.unwrap()["complete"] == false);
    assert!(f.root.join("owned.txt").exists());
    assert!(f.core.pending.lock().unwrap().is_empty());
    f.clean();
}
#[tokio::test]
async fn error_strategy_does_not_replay_and_rollback_hints_do_not_execute_inverses() {
    for strategy in ["stop", "continue"] {
        let f = Fixture::new(
            json!([{"tool":"files.read","input":{"path":{"$parameter":"missing"}}},read_step()]),
            json!({"missing":{"type":"string","required":true},"source":{"type":"string","required":true}}),
            strategy,
        );
        let p = prepare(
            &f.core,
            &f.input(json!({"missing":f.root.join("absent.txt"),"source":f.path()})),
        )
        .unwrap();
        let result = p.execute(&f.core).await.unwrap();
        assert_eq!(result["complete"], false);
        assert_eq!(
            result["results"].as_array().unwrap().len(),
            if strategy == "stop" { 1 } else { 2 }
        );
        let audit = f
            .core
            .store
            .query(
                "SELECT COUNT(*) AS n FROM activity_events WHERE tool_id='files.read'",
                &[],
            )
            .unwrap();
        assert_eq!(
            audit[0]["n"],
            if strategy == "stop" {
                json!(1)
            } else {
                json!(2)
            }
        );
        assert_eq!(result["automatic_rollback"], false);
        assert_eq!(result["automatic_replay"], false);
        assert!(f.root.join("owned.txt").exists());
        f.clean();
    }
}
#[tokio::test]
async fn macro_review_and_each_mandatory_child_use_individual_actual_oneshot_permissions() {
    for answer in ["deny", "session"] {
        let f = Fixture::new(
            json!([{"tool":"files.delete","input":{"path":{"$parameter":"source"}}}]),
            json!({"source":{"type":"string","required":true}}),
            "continue",
        );
        let core = f.core.clone();
        let input = f.input(json!({"source":f.path()}));
        let task = tokio::spawn(async move { core.execute("macros.run", input).await });
        f.answer("macros.run", "allow").await;
        f.answer("files.delete", answer).await;
        let result = task.await.unwrap().unwrap();
        assert_eq!(result["complete"], false);
        assert_eq!(
            result["results"][0]["error"],
            "PERMISSION_DENIED_OR_TIMEOUT"
        );
        assert!(f.root.join("owned.txt").exists());
        let audit = f
            .core
            .store
            .query(
                "SELECT tool_id,status FROM activity_events ORDER BY timestamp",
                &[],
            )
            .unwrap();
        assert!(audit
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["tool_id"] == "files.delete" && v["status"] == "denied"));
        f.clean();
    }
}
#[tokio::test]
async fn original_review_fingerprint_privacy_and_module_revocation_are_enforced() {
    let f = Fixture::new(
        json!([read_step()]),
        json!({"source":{"type":"string","required":true}}),
        "stop",
    );
    let mut input = f.input(json!({"source":f.path()}));
    input["expected"] = json!({"fingerprint":"a".repeat(64)});
    assert_eq!(
        prepare(&f.core, &input).err().unwrap(),
        "MACRO_CHANGED_REVIEW_REQUIRED"
    );
    let p = prepare(&f.core, &f.input(json!({"source":f.path()}))).unwrap();
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(p.execute(&f.core).await.unwrap_err(), "PRIVACY_DENIED");
    f.core.store.set_setting("privacy", &json!(false)).unwrap();
    let p = prepare(&f.core, &f.input(json!({"source":f.path()}))).unwrap();
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='macros'",
            &[],
        )
        .unwrap();
    assert_eq!(
        p.execute(&f.core).await.unwrap_err(),
        "MODULE_DISABLED: macros"
    );
    f.clean();
}
#[tokio::test]
async fn save_requires_individual_review_and_parent_audit_omits_the_declaration_and_resolved_values(
) {
    let f = Fixture::new(
        json!([read_step()]),
        json!({"source":{"type":"string","required":true}}),
        "stop",
    );
    for answer in ["deny", "session", "allow"] {
        let core = f.core.clone();
        let input = json!({"name":"Reviewed owned creation","description":"Private fixture description","steps":[{"tool":"files.read","input":{"path":f.path()}}]});
        let task = tokio::spawn(async move { core.execute("macros.save", input).await });
        f.answer("macros.save", answer).await;
        let result = task.await.unwrap();
        if answer == "allow" {
            assert!(result.is_ok());
        } else {
            assert_eq!(result.unwrap_err(), "PERMISSION_DENIED_OR_TIMEOUT");
        }
    }
    let rows = f
        .core
        .store
        .query("SELECT COUNT(*) AS n FROM macros", &[])
        .unwrap();
    assert_eq!(rows[0]["n"], 2);
    let rows = f
        .core
        .store
        .query(
            "SELECT input_summary,output_summary FROM activity_events WHERE tool_id='macros.save'",
            &[],
        )
        .unwrap();
    assert!(!rows.to_string().contains("Private fixture description"));
    assert!(!rows.to_string().contains("owned.txt"));
    assert!(rows.to_string().contains("MACRO_CONTENT_OMITTED"));
    assert!(f
        .core
        .execute(
            "macros.save",
            json!({"name":"Invalid","steps":[{"tool":"macros.run","input":{"id":f.id}}]})
        )
        .await
        .is_err());
    assert!(f.core.pending.lock().unwrap().is_empty());
    f.clean();
}
#[tokio::test]
async fn inspect_configure_delete_and_shortcut_pending_require_original_identity_and_never_execute_a_child(
) {
    let f = Fixture::new(
        json!([read_step()]),
        json!({"source":{"type":"string","required":true}}),
        "stop",
    );
    let snapshot = inspect(&f.core, &json!({"id":f.id})).unwrap();
    assert_eq!(snapshot["enabled"], true);
    assert_eq!(snapshot["definition"]["version"], 2);
    assert_eq!(
        dispatch(
            &f.core,
            "delete",
            &json!({"id":f.id,"expected":{"fingerprint":"a".repeat(64)}})
        )
        .await
        .unwrap_err(),
        "MACRO_CHANGED_REVIEW_REQUIRED"
    );
    f.core
        .store
        .set_setting(
            "macro_shortcut_pending",
            &json!({"id":f.id,"at":storage::now()}),
        )
        .unwrap();
    assert_eq!(consume_shortcut(&f.core).unwrap()["id"], f.id);
    assert_eq!(consume_shortcut(&f.core).unwrap(), Value::Null);
    f.core
        .store
        .set_setting(
            "macro_shortcut_pending",
            &json!({"id":f.id,"at":"2000-01-01T00:00:00Z"}),
        )
        .unwrap();
    assert_eq!(consume_shortcut(&f.core).unwrap(), Value::Null);
    let disabled = dispatch(
        &f.core,
        "set_enabled",
        &json!({"id":f.id,"enabled":false,"expected":{"fingerprint":snapshot["fingerprint"]}}),
    )
    .await
    .unwrap();
    assert_eq!(disabled["enabled"], false);
    assert_eq!(
        prepare(&f.core, &f.input(json!({"source":f.path()})))
            .err()
            .unwrap(),
        "MACRO_DISABLED"
    );
    let snapshot = inspect(&f.core, &json!({"id":f.id})).unwrap();
    dispatch(
        &f.core,
        "delete",
        &json!({"id":f.id,"expected":{"fingerprint":snapshot["fingerprint"]}}),
    )
    .await
    .unwrap();
    assert!(inspect(&f.core, &json!({"id":f.id})).is_err());
    assert_eq!(
        f.core
            .store
            .query(
                "SELECT COUNT(*) AS n FROM activity_events WHERE tool_id LIKE 'files.%'",
                &[]
            )
            .unwrap()[0]["n"],
        0
    );
    f.clean();
}
#[tokio::test]
async fn peer_revocation_and_corrupt_authority_cannot_revive_an_original_macro_after_privacy_restore(
) {
    let f = Fixture::new(
        json!([read_step()]),
        json!({"source":{"type":"string","required":true}}),
        "stop",
    );
    let p = prepare(&f.core, &f.input(json!({"source":f.path()}))).unwrap();
    let peer = Arc::new(Core::new(f.core.root.clone(), f.core.resource.clone()).unwrap());
    revoke(&peer).unwrap();
    assert_eq!(
        p.execute(&f.core).await.unwrap_err(),
        "MACRO_AUTHORITY_CHANGED_REVIEW_REQUIRED"
    );
    let p = prepare(&f.core, &f.input(json!({"source":f.path()}))).unwrap();
    f.core
        .store
        .execute(
            "DELETE FROM settings WHERE key='macro_authority_generation'",
            &[],
        )
        .unwrap();
    assert_eq!(p.execute(&f.core).await.unwrap_err(), "MACRO_AUTHORITY");
    revoke(&peer).unwrap();
    for bad in ["null", "{broken", "42"] {
        f.core
            .store
            .execute(
                "UPDATE settings SET json_value=? WHERE key='macro_authority_generation'",
                &[json!(bad)],
            )
            .unwrap();
        assert_eq!(
            prepare(&f.core, &f.input(json!({"source":f.path()})))
                .err()
                .unwrap(),
            "MACRO_AUTHORITY"
        );
    }
    revoke(&peer).unwrap();
    let p = prepare(&f.core, &f.input(json!({"source":f.path()}))).unwrap();
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    revoke(&f.core).unwrap();
    f.core.store.set_setting("privacy", &json!(false)).unwrap();
    assert_eq!(
        p.execute(&f.core).await.unwrap_err(),
        "MACRO_AUTHORITY_CHANGED_REVIEW_REQUIRED"
    );
    drop(peer);
    f.clean();
}
#[tokio::test]
async fn disabled_child_schema_still_supports_inspection_and_parameter_contracts_are_strict() {
    let f = Fixture::new(
        json!([read_step()]),
        json!({"source":{"type":"string","required":true}}),
        "stop",
    );
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='files'",
            &[],
        )
        .unwrap();
    assert_eq!(
        inspect(&f.core, &json!({"id":f.id})).unwrap()["definition"]["steps"][0]["tool"],
        "files.read"
    );
    assert!(prepare(&f.core, &f.input(json!({"source":f.path()}))).is_err());
    assert!(f
        .core
        .execute("macros.run", json!({"id":f.id,"body":[],"unexpected":true}))
        .await
        .is_err());
    assert!(f.core.pending.lock().unwrap().is_empty());
    for tool in TOOLS {
        assert!(jsonschema::validator_for(&schema(tool)).is_ok());
    }
    assert!(boundary_error("MCP_OUTCOME_UNCERTAIN: disconnected"));
    assert!(boundary_error("TOOL_DISABLED_OR_UNKNOWN"));
    assert!(!boundary_error("PATH_NOT_FOUND"));
    f.clean();
}
#[tokio::test]
async fn configuration_revoked_while_individual_permission_waits_never_persists_a_new_declaration()
{
    let f = Fixture::new(
        json!([read_step()]),
        json!({"source":{"type":"string","required":true}}),
        "stop",
    );
    let core = f.core.clone();
    let input = json!({"name":"Revoked creation","steps":[{"tool":"files.read","input":{"path":f.path()}}]});
    let task = tokio::spawn(async move { core.execute("macros.save", input).await });
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        while f.core.pending.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    revoke(&f.core).unwrap();
    f.answer("macros.save", "allow").await;
    assert_eq!(
        task.await.unwrap().unwrap_err(),
        "MACRO_AUTHORITY_CHANGED_REVIEW_REQUIRED"
    );
    assert_eq!(
        f.core
            .store
            .query("SELECT COUNT(*) AS n FROM macros", &[])
            .unwrap()[0]["n"],
        1
    );
    f.clean();
}
#[tokio::test]
async fn cancelled_configuration_retains_the_worker_lease_and_cannot_publish_after_the_await_is_dropped(
) {
    let f = Fixture::new(
        json!([read_step()]),
        json!({"source":{"type":"string","required":true}}),
        "stop",
    );
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    *f.core.macro_mutation_gate.lock().unwrap() = Some(Arc::new(mutation::Gate {
        entered: entered_tx,
        resume: std::sync::Mutex::new(resume_rx),
        finished: finished_tx,
    }));
    let input = json!({"name":"Cancelled creation","steps":[{"tool":"files.read","input":{"path":f.path()}}]});
    let core = f.core.clone();
    let first = input.clone();
    let task = tokio::spawn(async move { core.execute("macros.save", first).await });
    f.answer("macros.save", "allow").await;
    async fn signal(rx: &std::sync::mpsc::Receiver<()>) {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                match rx.try_recv() {
                    Ok(()) => break,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await
                    }
                    Err(_) => panic!("Owned macro worker disconnected"),
                }
            }
        })
        .await
        .unwrap();
    }
    signal(&entered_rx).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(
        mutation::Mutation::prepare(&f.core, "macros.save", &input)
            .unwrap()
            .execute(&f.core)
            .await
            .unwrap_err(),
        "MACRO_CONFIGURATION_BUSY"
    );
    resume_tx.send(()).unwrap();
    signal(&finished_rx).await;
    assert!(f.core.macro_actions.try_lock().is_ok());
    assert!(f.core.pending.lock().unwrap().is_empty());
    assert_eq!(
        f.core
            .store
            .query("SELECT COUNT(*) AS n FROM macros", &[])
            .unwrap()[0]["n"],
        1
    );
    f.clean();
}
