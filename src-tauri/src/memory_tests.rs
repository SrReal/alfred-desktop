use super::*;
use std::sync::Mutex;

struct Fixture {
    core: Option<Core>,
    root: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ap-memory-{}", storage::id()));
        let core = Core::new(
            root.clone(),
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap();
        core.store.execute("INSERT INTO conversations(id,title,project_id,created_at,updated_at) VALUES('c','Fixture','p',?,?)",&[json!(storage::now()),json!(storage::now())]).unwrap();
        Self {
            core: Some(core),
            root,
        }
    }
    fn c(&self) -> &Core {
        self.core.as_ref().unwrap()
    }
    fn message(&self, id: &str, role: &str, text: &str) {
        self.c()
            .store
            .execute(
                "INSERT INTO messages VALUES(?,'c',?,?,'complete',?)",
                &[json!(id), json!(role), json!(text), json!(storage::now())],
            )
            .unwrap();
    }
    fn count(&self, table: &str) -> i64 {
        self.c()
            .store
            .query(&format!("SELECT COUNT(*) AS n FROM {table}"), &[])
            .unwrap()[0]["n"]
            .as_i64()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.core.take());
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
struct Mock {
    result: Value,
    prompts: Mutex<Vec<String>>,
    change: Option<&'static str>,
}
impl Mock {
    fn new(result: Value) -> Self {
        Self {
            result,
            prompts: Mutex::new(Vec::new()),
            change: None,
        }
    }
}
impl Extractor for Mock {
    fn extract<'a>(&'a self, c: &'a Core, prompt: String) -> claude::TransportFuture<'a> {
        Box::pin(async move {
            self.prompts.lock().unwrap().push(prompt);
            match self.change {
                Some("privacy") => c.store.set_setting("privacy", &json!(true)).unwrap(),
                Some("exclude") => {
                    c.store
                        .execute("UPDATE conversations SET no_memory=1 WHERE id='c'", &[])
                        .unwrap();
                }
                Some("project") => {
                    c.store
                        .execute(
                            "UPDATE conversations SET project_id='changed' WHERE id='c'",
                            &[],
                        )
                        .unwrap();
                }
                Some("source") => {
                    c.store
                        .execute(
                            "UPDATE messages SET content_text='Changed source' WHERE id='u1'",
                            &[],
                        )
                        .unwrap();
                }
                Some("cancel") => std::future::pending::<()>().await,
                _ => {}
            }
            Ok(self.result.clone())
        })
    }
}
fn candidate(source: &str, confidence: f64) -> Value {
    json!({"type":"preference","title":"Ventanas","content":"Prefiere ventanas compactas","scope":"project","confidence":confidence,"importance":0.7,"rationale":"Preferencia expresada por el usuario","source_message_ids":[source],"evidence":[{"message_id":source,"quote":"Prefiero las ventanas compactas."}],"supersedes":null})
}
fn response(candidates: Vec<Value>) -> Value {
    json!({"conversation_summary":"Se registró una preferencia de ventanas.","task_outcome":"Preferencia guardada","decisions":["Ventanas compactas"],"artifacts":[],"candidates":candidates})
}
async fn extract(f: &Fixture, m: &Mock) -> Value {
    extract_with(f.c(), "c", m, CancellationToken::new())
        .await
        .unwrap()
}

#[tokio::test]
async fn candidates_consolidate_sources_scores_and_incremental_summaries() {
    let f = Fixture::new();
    f.message("u1", "user", "Prefiero las ventanas compactas.");
    let m = Mock::new(response(vec![candidate("u1", 0.9)]));
    assert_eq!(extract(&f, &m).await["accepted"], 1);
    assert_eq!(f.count("memories"), 1);
    assert_eq!(f.count("memory_candidates"), 1);
    let memory = f.c().store.query("SELECT * FROM memories", &[]).unwrap()[0].clone();
    assert_eq!(memory["explicit"], 0);
    assert_eq!(memory["project_id"], "p");
    let metadata: Value = serde_json::from_str(memory["metadata_json"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["evidence"][0]["message_id"], "u1");
    assert_eq!(metadata["reviewed"], false);
    assert_eq!(extract(&f, &m).await["processed"], 0);
    assert_eq!(m.prompts.lock().unwrap().len(), 1);
    f.message("u2", "user", "Prefiero las ventanas compactas.");
    let m = Mock::new(response(vec![candidate("u2", 0.99)]));
    extract(&f, &m).await;
    assert_eq!(f.count("memory_candidates"), 1);
    assert_eq!(f.count("memories"), 1);
    assert_eq!(f.count("memory_sources"), 2);
    let prompt = m.prompts.lock().unwrap()[0].clone();
    let data: Value = serde_json::from_str(prompt.split("DATA:\n").nth(1).unwrap()).unwrap();
    assert_eq!(data["messages"].as_array().unwrap().len(), 1);
    assert_eq!(data["messages"][0]["id"], "u2");
    assert_eq!(
        data["previous_summary"]["task_outcome"],
        "Preferencia guardada"
    );
    assert!(
        f.c()
            .store
            .query("SELECT confidence FROM memories", &[])
            .unwrap()[0]["confidence"]
            .as_f64()
            .unwrap()
            <= 0.9
    );
    // A later correction or deletion must not resurrect this accepted candidate.
    f.c()
        .store
        .execute("UPDATE memories SET disabled=1", &[])
        .unwrap();
    let id = f
        .c()
        .store
        .query("SELECT id FROM memory_candidates", &[])
        .unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        accept(f.c(), &id, true).unwrap_err(),
        "MEMORY_CANDIDATE_MEMORY_CHANGED"
    );
}
#[tokio::test]
async fn review_rejection_and_replacements_remain_under_user_control() {
    let f = Fixture::new();
    f.message("u1", "user", "Prefiero las ventanas compactas.");
    let m = Mock::new(response(vec![candidate("u1", 0.6)]));
    assert_eq!(extract(&f, &m).await["pending"], 1);
    let pending = candidates(f.c()).unwrap();
    let id = pending[0]["id"].as_str().unwrap();
    assert_eq!(f.count("memories"), 0);
    reject(f.c(), id).unwrap();
    f.message("u2", "user", "Prefiero las ventanas compactas.");
    extract(&f, &Mock::new(response(vec![candidate("u2", 0.99)]))).await;
    assert_eq!(f.count("memories"), 0);
    assert_eq!(f.count("memory_candidates"), 1);
    let old = f
        .c()
        .store
        .remember(&json!({"content":"Prefiere ventanas grandes","project_id":"p"}))
        .unwrap();
    f.message("u3", "user", "Prefiero las ventanas compactas.");
    let mut proposal = candidate("u3", 0.99);
    proposal["content"] = json!("Ahora prefiere las ventanas compactas");
    proposal["supersedes"] = old["id"].clone();
    extract(&f, &Mock::new(response(vec![proposal]))).await;
    let pending = candidates(f.c()).unwrap();
    assert_eq!(pending.as_array().unwrap().len(), 1);
    assert_eq!(
        f.c()
            .store
            .query(
                "SELECT disabled FROM memories WHERE id=?",
                &[old["id"].clone()]
            )
            .unwrap()[0]["disabled"],
        0
    );
    let saved = accept(f.c(), pending[0]["id"].as_str().unwrap(), false).unwrap();
    assert_eq!(
        f.c()
            .store
            .query(
                "SELECT superseded_by FROM memories WHERE id=?",
                &[old["id"].clone()]
            )
            .unwrap()[0]["superseded_by"],
        saved["id"]
    );
    assert_eq!(
        accept(f.c(), pending[0]["id"].as_str().unwrap(), false).unwrap_err(),
        "MEMORY_CANDIDATE_NOT_PENDING"
    );
    let metadata: Value = serde_json::from_str(
        f.c()
            .store
            .query(
                "SELECT metadata_json FROM memories WHERE id=?",
                &[saved["id"].clone()],
            )
            .unwrap()[0]["metadata_json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(metadata["reviewed"], true);
}
#[tokio::test]
async fn independent_evidence_can_promote_a_pending_candidate_but_repeats_cannot() {
    let f = Fixture::new();
    f.message("u1", "user", "Prefiero las ventanas compactas.");
    extract(&f, &Mock::new(response(vec![candidate("u1", 0.8)]))).await;
    let id = candidates(f.c()).unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(accept(f.c(), &id, true).is_err());
    f.message("u2", "user", "Prefiero las ventanas compactas.");
    assert_eq!(
        extract(&f, &Mock::new(response(vec![candidate("u2", 0.8)]))).await["accepted"],
        1
    );
    assert_eq!(f.count("memories"), 1);
    assert_eq!(f.count("memory_sources"), 2);
}
#[tokio::test]
async fn extraction_rejects_assistant_foreign_fabricated_and_secret_evidence() {
    for bad in [
        "assistant",
        "foreign",
        "quote",
        "extra_quote",
        "secret",
        "outside_batch",
        "replacement",
    ] {
        let f = Fixture::new();
        f.message("u1", "user", "Prefiero las ventanas compactas.");
        f.message("a1", "assistant", "Prefiero las ventanas compactas.");
        let mut body = candidate("u1", 0.9);
        match bad {
            "assistant" => body = candidate("a1", 0.9),
            "foreign" => body = candidate("missing", 0.9),
            "quote" => body["evidence"][0]["quote"] = json!("Frase inventada por el modelo"),
            "extra_quote" => body["evidence"]
                .as_array_mut()
                .unwrap()
                .push(json!({"message_id":"u1","quote":"Frase inventada adicional"})),
            "secret" => body["content"] = json!("password=synthetic-secret"),
            "outside_batch" => {
                f.c().store.execute("INSERT INTO conversation_analysis(conversation_id,last_message_rowid,updated_at) VALUES('c',(SELECT rowid FROM messages WHERE id='u1'),?)",&[json!(storage::now())]).unwrap();
            }
            "replacement" => body["supersedes"] = json!("missing"),
            _ => unreachable!(),
        }
        extract(&f, &Mock::new(response(vec![body]))).await;
        assert_eq!(f.count("memories"), 0, "{bad}");
        assert_eq!(f.count("memory_candidates"), 0, "{bad}");
        assert_eq!(f.count("messages"), 2);
    }
}
#[tokio::test]
async fn privacy_scope_source_changes_and_invalid_output_do_not_advance_the_cursor() {
    for change in ["privacy", "exclude", "project", "source", "schema"] {
        let f = Fixture::new();
        f.message("u1", "user", "Prefiero las ventanas compactas.");
        let mut m = Mock::new(response(vec![candidate("u1", 0.9)]));
        m.change = Some(change);
        if change == "schema" {
            m.result["extra"] = json!("invalid");
        }
        let result = extract_with(f.c(), "c", &m, CancellationToken::new()).await;
        if change == "source" {
            assert!(result.is_ok());
        } else {
            assert!(result.is_err(), "{change}");
        }
        assert_eq!(f.count("memories"), 0);
        assert_eq!(f.count("memory_candidates"), 0);
        if change != "source" {
            assert_eq!(
                f.c()
                    .store
                    .query("SELECT last_message_rowid FROM conversation_analysis", &[])
                    .unwrap()[0]["last_message_rowid"],
                0
            );
        }
        assert_eq!(f.count("messages"), 1);
    }
    let f = Fixture::new();
    f.message("u1", "user", "Prefiero las ventanas compactas.");
    let mut m = Mock::new(response(vec![]));
    m.change = Some("cancel");
    let token = CancellationToken::new();
    let cancel = token.clone();
    let task = async {
        tokio::task::yield_now().await;
        cancel.cancel();
    };
    let (result, _) = tokio::join!(extract_with(f.c(), "c", &m, token), task);
    assert_eq!(result.unwrap_err(), "MEMORY_ANALYSIS_CANCELLED");
    assert_eq!(f.count("memories"), 0);
}
#[tokio::test]
async fn large_history_is_batched_and_secret_messages_are_never_sent() {
    let f = Fixture::new();
    f.message("secret", "user", "password=synthetic-secret");
    for n in 0..4 {
        f.message(&format!("u{n}"), "user", &"x".repeat(50000));
    }
    let m = Mock::new(response(vec![]));
    assert_eq!(extract(&f, &m).await["processed"], 4);
    assert_eq!(m.prompts.lock().unwrap().len(), 4);
    assert!(m
        .prompts
        .lock()
        .unwrap()
        .iter()
        .all(|p| !p.contains("synthetic-secret") && p.len() < 128000));
    assert_eq!(f.count("messages"), 5);
}
#[test]
fn structured_decode_and_startup_recovery_are_bounded() {
    assert!(decode("not JSON").is_err());
    assert!(decode(&json!({"structured_output":response(vec![])}).to_string()).is_ok());
    assert!(
        decode(&json!({"is_error":true,"structured_output":response(vec![])}).to_string()).is_err()
    );
    assert_eq!(
        decode(&"x".repeat(262145)).unwrap_err(),
        "MEMORY_RESPONSE_LIMIT"
    );
    let mut f = Fixture::new();
    f.message("u1", "user", "Prefiero las ventanas compactas.");
    persist(
        f.c(),
        &conversation(f.c(), "c").unwrap(),
        Some(&response(vec![])),
        &[candidate("u1", 0.9)],
        1,
    )
    .unwrap();
    f.c()
        .store
        .execute("UPDATE conversation_analysis SET status='running'", &[])
        .unwrap();
    drop(f.core.take());
    f.core = Some(
        Core::new(
            f.root.clone(),
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap(),
    );
    assert_eq!(f.count("memories"), 1);
    assert_eq!(
        f.c()
            .store
            .query("SELECT status FROM conversation_analysis", &[])
            .unwrap()[0]["status"],
        "interrupted"
    );
    assert_eq!(
        f.c().store.query("PRAGMA quick_check(1)", &[]).unwrap()[0]["quick_check"],
        "ok"
    );
}

#[test]
fn accepting_a_candidate_rolls_back_memory_and_correction_if_queue_write_fails() {
    let f = Fixture::new();
    f.message("u1", "user", "Prefiero las ventanas compactas.");
    let old = f
        .c()
        .store
        .remember(&json!({"content":"Prefiere ventanas grandes","project_id":"p"}))
        .unwrap();
    let mut body = candidate("u1", 0.99);
    body["supersedes"] = old["id"].clone();
    let ids = persist(
        f.c(),
        &conversation(f.c(), "c").unwrap(),
        Some(&response(vec![])),
        &[body],
        1,
    )
    .unwrap();
    f.c().store.execute("CREATE TRIGGER reject_candidate_write BEFORE UPDATE OF state ON memory_candidates BEGIN SELECT RAISE(ABORT,'fixture failure'); END",&[]).unwrap();
    assert!(accept(f.c(), &ids[0], false).is_err());
    assert_eq!(f.count("memories"), 1);
    assert_eq!(f.count("memory_sources"), 0);
    assert_eq!(
        f.c()
            .store
            .query("SELECT disabled,superseded_by FROM memories", &[])
            .unwrap()[0]["disabled"],
        0
    );
    assert_eq!(candidates(f.c()).unwrap().as_array().unwrap().len(), 1);
    f.c()
        .store
        .execute("DROP TRIGGER reject_candidate_write", &[])
        .unwrap();
    accept(f.c(), &ids[0], false).unwrap();
    assert_eq!(f.count("memories"), 2);
}

#[tokio::test]
async fn concurrent_turns_coalesce_and_jobs_release_their_cancel_tokens() {
    struct Paused {
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }
    impl Extractor for Paused {
        fn extract<'a>(&'a self, _: &'a Core, _: String) -> claude::TransportFuture<'a> {
            Box::pin(async move {
                self.entered.notify_one();
                self.release.notified().await;
                Ok(response(vec![]))
            })
        }
    }
    let mut f = Fixture::new();
    f.message("u1", "user", "Prefiero las ventanas compactas.");
    let core = Arc::new(f.core.take().unwrap());
    let provider = Arc::new(Paused {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    start_with(&core, "c", provider.clone()).unwrap();
    provider.entered.notified().await;
    assert_eq!(
        start_with(&core, "c", provider.clone()).unwrap()["rerun_requested"],
        true
    );
    cancel(&core, "c").unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if core.memory_jobs.lock().unwrap().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(core.memory_rerun.lock().unwrap().is_empty());
    assert_eq!(
        core.store
            .query("SELECT error_code FROM conversation_analysis", &[])
            .unwrap()[0]["error_code"],
        "MEMORY_ANALYSIS_CANCELLED"
    );
    start_with(&core, "c", provider.clone()).unwrap();
    provider.entered.notified().await;
    provider.release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if core.memory_jobs.lock().unwrap().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        core.store
            .query("SELECT status FROM conversation_analysis", &[])
            .unwrap()[0]["status"],
        "succeeded"
    );
    f.core = Some(Arc::try_unwrap(core).ok().unwrap());
}
