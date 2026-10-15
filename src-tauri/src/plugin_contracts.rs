use super::*;

#[tokio::test]
async fn chat_upgrade_builder_update_preserves_code_and_requires_new_tests() {
    let f = Fixture::new();
    f.register().await;
    let original = std::fs::read(f.path.join("index.mjs")).unwrap();
    let prior_path = row(&f.core, &f.id).unwrap()["install_path"].clone();
    let input = json!({"id":f.id,"expected":f.expected(),"manifest":f.manifest("1.1.0"),"files":Fixture::files()});
    assert!(f
        .execute("plugin_builder.update", input.clone(), "deny")
        .await
        .is_err());
    assert_eq!(row(&f.core, &f.id).unwrap()["install_path"], prior_path);
    let result = f
        .execute("plugin_builder.update", input.clone(), "allow")
        .await
        .unwrap();
    assert_eq!(result["previous_preserved"], true);
    assert_eq!(result["enabled"], false);
    assert_eq!(std::fs::read(f.path.join("index.mjs")).unwrap(), original);
    let current = row(&f.core, &f.id).unwrap();
    assert_ne!(current["install_path"], prior_path);
    assert_eq!(
        f.core
            .store
            .setting(&format!("plugin_versions.{}", f.id))
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(Prepared::prepare(&f.core, "plugin_builder.update", &input).is_err());
    assert!(Prepared::prepare(
        &f.core,
        "plugin_builder.enable_plugin",
        &json!({"id":f.id,"expected":f.expected()})
    )
    .is_err());
}

#[tokio::test]
async fn chat_upgrade_skills_permissions_revisions_and_revocation() {
    let f = Fixture::new();
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=1 WHERE module_id='skills'",
            &[],
        )
        .unwrap();
    f.core
        .macro_headless_permission
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mut input = json!({"id":"weekly-review","name":"Repaso semanal","description":"Preparar el repaso semanal del usuario","instructions":"Resume las tareas y pregunta por las prioridades.","enabled":true,"expected_revision":0});
    assert!(f
        .execute("skills.save", input.clone(), "deny")
        .await
        .is_err());
    assert_eq!(crate::skills::catalog(&f.core).unwrap(), json!([]));
    f.execute("skills.save", input.clone(), "allow")
        .await
        .unwrap();
    assert_eq!(crate::skills::catalog(&f.core).unwrap()[0]["revision"], 1);
    assert!(crate::skills::Prepared::prepare(&f.core, "skills.save", &input).is_err());
    input["expected_revision"] = json!(1);
    input["instructions"] =
        json!("Empieza por los vencimientos; después pregunta por prioridades.");
    f.execute("skills.save", input.clone(), "allow")
        .await
        .unwrap();
    let row = f
        .core
        .execute("skills.inspect", json!({"id":"weekly-review"}))
        .await
        .unwrap();
    assert_eq!(row["revision"], 2);
    assert_eq!(row["history"][0]["revision"], 1);
    input["expected_revision"] = json!(2);
    let prepared = crate::skills::Prepared::prepare(&f.core, "skills.save", &input).unwrap();
    crate::skills::revoke(&f.core).unwrap();
    assert!(prepared.execute(&f.core).is_err());
    f.execute(
        "skills.set_enabled",
        json!({"id":"weekly-review","enabled":false,"expected_revision":2}),
        "allow",
    )
    .await
    .unwrap();
    assert_eq!(crate::skills::catalog(&f.core).unwrap(), json!([]));
    f.execute(
        "skills.delete",
        json!({"id":"weekly-review","expected_revision":3}),
        "allow",
    )
    .await
    .unwrap();
    assert_eq!(
        f.core.execute("skills.list", json!({})).await.unwrap(),
        json!([])
    );
}

#[test]
fn chat_upgrade_delete_is_local_atomic_and_rejects_busy_or_changed_conversations() {
    let f = Fixture::new();
    let c = &f.core;
    c.store.execute("INSERT INTO conversations(id,title,created_at,updated_at) VALUES('chat-a','A','now','rev-a'),('chat-b','B','now','rev-b')",&[]).unwrap();
    c.store.execute("INSERT INTO messages VALUES('message-a','chat-a','user','private fixture','complete','now'),('message-b','chat-b','user','keep fixture','complete','now')",&[]).unwrap();
    let original = f.root.join("original.txt");
    std::fs::write(&original, "original remains").unwrap();
    c.store.execute("INSERT INTO attachments(id,conversation_id,path,mime,sha256,size,created_at) VALUES('attachment','chat-a',?,'text/plain','fixture',16,'now')",&[json!(original)]).unwrap();
    c.store.execute("INSERT INTO tasks(id,title,status,conversation_id,created_at,updated_at) VALUES('task','Keep task','pending','chat-a','now','now')",&[]).unwrap();
    c.store
        .set_setting("active_conversation", &json!("chat-a"))
        .unwrap();
    let input = json!({"id":"chat-a","updated_at":"rev-a","confirmed":true});
    assert!(crate::conversations::delete(c, &json!({"id":"chat-a","updated_at":"rev-a"})).is_err());
    assert!(crate::conversations::delete(
        c,
        &json!({"id":"chat-a","updated_at":"wrong","confirmed":true})
    )
    .is_err());
    c.runs
        .lock()
        .unwrap()
        .insert("chat-a".into(), tokio_util::sync::CancellationToken::new());
    assert!(crate::conversations::delete(c, &input).is_err());
    c.runs.lock().unwrap().clear();
    crate::conversations::delete(c, &input).unwrap();
    assert_eq!(
        c.store
            .query(
                "SELECT count(*) AS n FROM messages WHERE conversation_id='chat-a'",
                &[]
            )
            .unwrap()[0]["n"],
        0
    );
    assert_eq!(
        c.store
            .query(
                "SELECT count(*) AS n FROM messages_fts WHERE conversation_id='chat-a'",
                &[]
            )
            .unwrap()[0]["n"],
        0
    );
    assert_eq!(
        c.store
            .query(
                "SELECT count(*) AS n FROM messages WHERE conversation_id='chat-b'",
                &[]
            )
            .unwrap()[0]["n"],
        1
    );
    assert!(c
        .store
        .query("SELECT conversation_id FROM tasks WHERE id='task'", &[])
        .unwrap()[0]["conversation_id"]
        .is_null());
    assert_eq!(
        std::fs::read_to_string(original).unwrap(),
        "original remains"
    );
    assert!(c.store.setting("active_conversation").is_null());
    assert_eq!(
        c.store.query("PRAGMA foreign_key_check", &[]).unwrap(),
        json!([])
    );
}
struct Fixture {
    core: Arc<Core>,
    root: PathBuf,
    id: String,
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ap-plugin-contract-{}", storage::id()));
        std::fs::create_dir(&root).unwrap();
        let core = Arc::new(
            Core::new(
                root.join("profile"),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        for m in ["custom", "plugin_builder"] {
            core.store
                .execute(
                    "UPDATE module_configs SET enabled=1 WHERE module_id=?",
                    &[json!(m)],
                )
                .unwrap();
        }
        core.store
            .set_setting("allowed_roots", &json!([root]))
            .unwrap();
        core.plugin_headless_permission
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let id = format!("custom.owned-{}", storage::id());
        let path = root.join("v1");
        std::fs::create_dir(&path).unwrap();
        Self {
            core,
            root,
            id,
            path,
        }
    }
    fn manifest(&self, version: &str) -> Value {
        json!({"id":self.id,"name":"Owned reviewed tool","version":version,"type":"custom","runtime":"node","entrypoint":"index.mjs","test":"test.mjs","description":"Owned fixture echo","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false},"outputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false},"permissions":{"defaultRisk":"mandatory","network":false,"filesystem":[]},"timeoutMs":5000,"sideEffects":["Own local Node process; echoes text"],"rollback":"No inverse for an echo","secrets":[],"fixtures":{"happy":{"text":"hello"},"badInput":{}}})
    }
    fn files() -> Value {
        json!({"index.mjs":"export function run(input){if(typeof input.text!=='string')throw new Error('INVALID_INPUT');return {text:input.text};}if(!process.env.ALFREDPET_TEST_CASE){let data='';for await(const chunk of process.stdin)data+=chunk;process.stdout.write(JSON.stringify(run(JSON.parse(data))));}","test.mjs":"if(process.env.ALFREDPET_TEST_CASE==='timeout'){await new Promise(resolve=>setTimeout(resolve,10000));}else{const assert=await import('node:assert/strict');const {run}=await import('./index.mjs');assert.deepEqual(run({text:'ok'}),{text:'ok'});assert.throws(()=>run({}));}","README.md":"# Owned fixture\nEcho tool with input/output schema and individual review.","icon.svg":"<svg xmlns='http://www.w3.org/2000/svg' width='32' height='32'><rect width='32' height='32' fill='#777'/></svg>"})
    }
    fn write(&self, path: &Path) {
        for (k, v) in Self::files().as_object().unwrap() {
            std::fs::write(path.join(k), v.as_str().unwrap()).unwrap();
        }
    }
    async fn register(&self) {
        self.write(&self.path);
        Prepared::prepare(
            &self.core,
            "custom.register",
            &json!({"manifest":self.manifest("1.0.0"),"path":self.path}),
        )
        .unwrap()
        .execute(&self.core)
        .await
        .unwrap();
    }
    fn expected(&self) -> Value {
        json!({"fingerprint":inspect(&self.core,&self.id).unwrap()["fingerprint"]})
    }
    async fn execute(&self, tool: &str, input: Value, answer: &str) -> Result<Value, String> {
        let c = self.core.clone();
        let t = tool.to_owned();
        let job = tokio::spawn(async move { c.execute(&t, input).await });
        self.answer(tool, answer).await;
        job.await.unwrap()
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
                    if let Some(tx) = self.core.pending.lock().unwrap().remove(id) {
                        break tx;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Owned plugin permission did not arrive");
        sender.send(answer.into()).unwrap();
    }
    async fn tested(&self) {
        let r = self
            .execute(
                "plugin_builder.test",
                json!({"id":self.id,"expected":self.expected()}),
                "allow",
            )
            .await
            .unwrap();
        assert_eq!(r["status"], "PASS", "{r}");
        assert_eq!(r["cases"].as_array().unwrap().len(), 6);
    }
    async fn enabled(&self) {
        self.tested().await;
        self.execute(
            "plugin_builder.enable_plugin",
            json!({"id":self.id,"expected":self.expected()}),
            "allow",
        )
        .await
        .unwrap();
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
            .starts_with("ap-plugin-contract-"));
        drop(self.core);
        std::fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn closed_manifest_versions_offline_schemas_and_explicit_references() {
    let f = Fixture::new();
    let base = f.manifest("1.0.0");
    manifest(&base).unwrap();
    for (key, value) in [
        ("extra", json!(true)),
        ("version", json!("01.0.0")),
        ("timeoutMs", json!(0)),
        ("entrypoint", json!("../bad.mjs")),
        ("runtime", json!("shell")),
        ("outputSchema", json!({"type":"string"})),
        ("secrets", json!(["PATH"])),
    ] {
        let mut m = base.clone();
        m[key] = value;
        assert!(manifest(&m).is_err(), "{key}");
    }
    let mut m = base.clone();
    m["inputSchema"]["$ref"] = json!("https://example.invalid/schema");
    assert!(manifest(&m).is_err());
    m = base.clone();
    m["permissions"]["defaultRisk"] = json!("free");
    assert!(manifest(&m).is_err());
    m = base;
    m["secrets"] = json!(["ALFREDPET_SECRET_SERVICE"]);
    manifest(&m).unwrap();
    assert_eq!(
        policy::classify("custom.run", &json!({})),
        policy::Risk::Mandatory
    );
    assert_eq!(
        policy::classify("plugin_builder.test", &json!({})),
        policy::Risk::Mandatory
    );
    f.clean();
}
#[tokio::test]
async fn generated_draft_requires_review_then_six_actual_runner_controls_before_enable() {
    let f = Fixture::new();
    let input = json!({"manifest":f.manifest("1.0.0"),"files":Fixture::files()});
    let r = f
        .execute("plugin_builder.create", input, "allow")
        .await
        .unwrap();
    assert_eq!(r["state"], "draft");
    assert!(Prepared::prepare(
        &f.core,
        "plugin_builder.enable_plugin",
        &json!({"id":f.id,"expected":f.expected()})
    )
    .is_err());
    f.enabled().await;
    let r = f
        .execute(
            "custom.run",
            json!({"id":f.id,"expected":f.expected(),"body":{"text":"ñ-日本"}}),
            "allow",
        )
        .await
        .unwrap();
    assert_eq!(r["text"], "ñ-日本");
    let audit = f
        .core
        .store
        .query(
            "SELECT * FROM activity_events WHERE tool_id='custom.run'",
            &[],
        )
        .unwrap()
        .to_string();
    assert!(!audit.contains("ñ-日本"));
    f.clean();
}
#[tokio::test]
async fn individual_denial_and_session_answer_never_launch_business_code() {
    let f = Fixture::new();
    f.register().await;
    f.enabled().await;
    let input = json!({"id":f.id,"expected":f.expected(),"body":{"text":"owned private marker"}});
    for answer in ["deny", "session"] {
        assert!(f
            .execute("custom.run", input.clone(), answer)
            .await
            .is_err());
    }
    assert_eq!(row(&f.core, &f.id).unwrap()["state"], "enabled");
    assert_eq!(f.core.grants.lock().unwrap().len(), 0);
    let r = f.execute("custom.run", input, "allow").await.unwrap();
    assert_eq!(r["text"], "owned private marker");
    f.clean();
}
#[tokio::test]
async fn malformed_output_and_timeout_quarantine_without_automatic_replay() {
    let f = Fixture::new();
    f.register().await;
    std::fs::write(
        f.path.join("index.mjs"),
        "process.stdout.write(JSON.stringify({wrong:true}));",
    )
    .unwrap();
    let r = f
        .execute(
            "plugin_builder.test",
            json!({"id":f.id,"expected":f.expected()}),
            "allow",
        )
        .await
        .unwrap();
    assert_eq!(r["status"], "FAIL");
    assert_eq!(row(&f.core, &f.id).unwrap()["state"], "quarantined");
    assert!(Prepared::prepare(
        &f.core,
        "plugin_builder.enable_plugin",
        &json!({"id":f.id,"expected":f.expected()})
    )
    .is_err());
    f.clean();
}
#[tokio::test]
async fn original_candidate_and_catalog_bytes_are_rechecked_after_individual_review() {
    let f = Fixture::new();
    f.write(&f.path);
    let input = json!({"manifest":f.manifest("1.0.0"),"path":f.path});
    let c = f.core.clone();
    let job = tokio::spawn(async move { c.execute("custom.register", input).await });
    let rows=tokio::time::timeout(std::time::Duration::from_secs(8),async{loop{let r=f.core.store.query("SELECT id FROM permission_requests WHERE tool_id='custom.register' AND status='pending'",&[]).unwrap();if !r[0].is_null(){break r}tokio::time::sleep(std::time::Duration::from_millis(10)).await;}}).await.unwrap();
    std::fs::write(f.path.join("index.mjs"), "process.stdout.write('{}');").unwrap();
    let tx = f
        .core
        .pending
        .lock()
        .unwrap()
        .remove(rows[0]["id"].as_str().unwrap())
        .unwrap();
    tx.send("allow".into()).unwrap();
    assert!(job.await.unwrap().is_err());
    assert!(row(&f.core, &f.id).is_err());
    f.clean();
}
#[tokio::test]
async fn version_update_keeps_original_files_and_rollback_always_requires_retest() {
    let f = Fixture::new();
    f.register().await;
    f.enabled().await;
    let old = std::fs::read(f.path.join("index.mjs")).unwrap();
    let v2 = f.root.join("v2");
    std::fs::create_dir(&v2).unwrap();
    f.write(&v2);
    f.execute(
        "custom.update",
        json!({"id":f.id,"expected":f.expected(),"manifest":f.manifest("2.0.0"),"path":v2}),
        "allow",
    )
    .await
    .unwrap();
    assert_eq!(row(&f.core, &f.id).unwrap()["state"], "validated");
    assert_eq!(std::fs::read(f.path.join("index.mjs")).unwrap(), old);
    f.execute(
        "custom.rollback",
        json!({"id":f.id,"expected":f.expected(),"version":"1.0.0"}),
        "allow",
    )
    .await
    .unwrap();
    assert_eq!(row(&f.core, &f.id).unwrap()["state"], "validated");
    assert!(f
        .execute(
            "plugin_builder.enable_plugin",
            json!({"id":f.id,"expected":f.expected()}),
            "allow"
        )
        .await
        .is_err());
    f.enabled().await;
    f.clean();
}
#[tokio::test]
async fn privacy_restore_disable_corrupt_authority_and_changed_tested_code_deny() {
    let f = Fixture::new();
    f.register().await;
    f.enabled().await;
    let p = Prepared::prepare(
        &f.core,
        "custom.run",
        &json!({"id":f.id,"expected":f.expected(),"body":{"text":"owned"}}),
    )
    .unwrap();
    revoke(&f.core).unwrap();
    assert!(p.check(&f.core).is_err());
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert!(p.check(&f.core).is_err());
    f.core.store.set_setting("privacy", &json!(false)).unwrap();
    assert!(p.check(&f.core).is_err());
    std::fs::write(f.path.join("index.mjs"), "process.stdout.write('{}');").unwrap();
    assert!(Prepared::prepare(
        &f.core,
        "custom.run",
        &json!({"id":f.id,"expected":f.expected(),"body":{"text":"owned"}})
    )
    .is_err());
    f.core
        .store
        .set_setting("plugin_authority", &json!({"corrupt":true}))
        .unwrap();
    assert!(Prepared::prepare(&f.core, "custom.inspect", &json!({"id":f.id})).is_err());
    f.clean();
}
#[test]
fn secret_echo_is_removed_from_keys_content_and_parent_audit() {
    let secrets = BTreeMap::from([("ALFREDPET_SECRET_TEST".into(), "private-owned-value".into())]);
    let r = redact(
        &json!({"text":"prefix private-owned-value suffix","private-owned-value":"fixture-key","password":"fixture","nested":[{"authorization":"Bearer fixture"}]}),
        &secrets,
    );
    assert!(!r.to_string().contains("private-owned-value"));
    assert!(!r.to_string().contains("Bearer fixture"));
    let audit = policy::tool_redact(
        "custom.run",
        &json!({"body":{"text":"owned private content"},"files":{"index.mjs":"owned private source"}}),
    );
    assert!(!audit.to_string().contains("owned private"));
}

#[tokio::test]
async fn broken_path_remains_inspectable_and_can_be_disabled_or_unregistered() {
    let f = Fixture::new();
    f.register().await;
    let moved = f.root.join("temporarily-moved");
    std::fs::rename(&f.path, &moved).unwrap();
    assert_eq!(inspect(&f.core, &f.id).unwrap()["health"], "ERROR");
    let input = json!({"id":f.id,"expected":f.expected(),"enabled":false});
    f.execute("custom.configure", input, "allow").await.unwrap();
    let prepared = Prepared::prepare(
        &f.core,
        "custom.uninstall",
        &json!({"id":f.id,"expected":f.expected()}),
    )
    .unwrap();
    std::fs::rename(&moved, &f.path).unwrap();
    assert!(prepared.check(&f.core).is_err());
    f.execute(
        "custom.uninstall",
        json!({"id":f.id,"expected":f.expected()}),
        "allow",
    )
    .await
    .unwrap();
    assert!(row(&f.core, &f.id).is_err());
    assert!(f.path.join("index.mjs").exists());
    f.clean();
}
