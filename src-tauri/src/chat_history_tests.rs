use super::*;
struct Fixture {
    store: Option<Store>,
    root: std::path::PathBuf,
}
impl std::ops::Deref for Fixture {
    type Target = Store;
    fn deref(&self) -> &Store {
        self.store.as_ref().unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.store.take());
        assert_eq!(self.root.parent(), Some(std::env::temp_dir().as_path()));
        assert!(self
            .root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("alfred-history-"));
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
fn store() -> Fixture {
    let root = std::env::temp_dir().join(format!("alfred-history-{}", storage::id()));
    let s = Store::open(&root.join("test.db")).unwrap();
    s.execute("INSERT INTO conversations(id,title,summary,claude_session_id,project_id,no_memory,created_at,updated_at) VALUES('c','Prueba','FUTURE_SUMMARY','parent-session','project',1,'0','9'),('other','Other','','other-session',NULL,0,'0','9')", &[]).unwrap();
    for (id, role, text, date) in [
        ("u1", "user", "antes", "1"),
        ("a1", "assistant", "contexto", "2"),
        ("u2", "user", "petición", "3"),
        ("a2", "assistant", "respuesta parcial", "4"),
        ("a3", "assistant", "respuesta final", "4"),
        ("u3", "user", "FUTURE_PROMPT", "5"),
        ("a4", "assistant", "FUTURE_ANSWER", "6"),
    ] {
        s.execute(
            "INSERT INTO messages VALUES(?,'c',?,?,'complete',?)",
            &[json!(id), json!(role), json!(text), json!(date)],
        )
        .unwrap();
    }
    s.execute("INSERT INTO attachments(id,conversation_id,path,mime,sha256,size,created_at) VALUES('early','c','old.txt','text/plain','hash',1,'2'),('late','c','future.txt','text/plain','hash',1,'5')",&[]).unwrap();
    Fixture {
        store: Some(s),
        root,
    }
}
fn revise(mode: &str, message: &str, expected: &str, text: &str) -> Revision {
    serde_json::from_value(json!({"conversation_id":"c","message_id":message,"expected_text":expected,"mode":mode,"text":text})).unwrap()
}
#[test]
fn edit_copies_exact_prefix_and_old_attachments_without_parent_session_or_future_summary() {
    let s = store();
    let original = s
        .query(
            "SELECT * FROM messages WHERE conversation_id='c' ORDER BY rowid",
            &[],
        )
        .unwrap();
    let b = branch(&s, revise("edit", "u2", "petición", "corregida")).unwrap();
    let c = s
        .query("SELECT * FROM conversations WHERE id=?", &[b["id"].clone()])
        .unwrap();
    assert!(c[0]["claude_session_id"].is_null());
    assert_eq!(c[0]["parent_conversation_id"], "c");
    assert_eq!(c[0]["project_id"], "project");
    assert_eq!(c[0]["no_memory"], 1);
    assert!(!c[0]["summary"].as_str().unwrap().contains("FUTURE"));
    assert!(!c[0]["summary"].as_str().unwrap().contains("petición"));
    let copies = s
        .query(
            "SELECT * FROM messages WHERE conversation_id=? ORDER BY created_at,rowid",
            &[b["id"].clone()],
        )
        .unwrap();
    assert_eq!(copies.as_array().unwrap().len(), 2);
    assert_eq!(copies[1]["content_text"], "contexto");
    assert_ne!(copies[0]["id"], "u1");
    assert_eq!(b["attachments"], 1);
    assert_eq!(draft(&s, b["id"].as_str().unwrap()).unwrap(), "corregida");
    assert_eq!(
        s.query(
            "SELECT * FROM messages WHERE conversation_id='c' ORDER BY rowid",
            &[]
        )
        .unwrap(),
        original
    );
}
#[test]
fn regenerate_any_assistant_chunk_repeats_its_user_turn_once_with_deterministic_order() {
    let s = store();
    for (id, text) in [("a2", "respuesta parcial"), ("a3", "respuesta final")] {
        let b = branch(&s, revise("regenerate", id, text, "")).unwrap();
        assert_eq!(b["text"], "petición");
        assert_eq!(
            s.query(
                "SELECT count(*) AS n FROM messages WHERE conversation_id=?",
                &[b["id"].clone()]
            )
            .unwrap()[0]["n"],
            2
        );
    }
}
#[test]
fn invalid_stale_cross_conversation_or_streaming_targets_do_not_create_branches() {
    let s = store();
    for input in [
        revise("edit", "u2", "stale", "new"),
        revise("edit", "a2", "respuesta parcial", "new"),
        revise("regenerate", "u1", "antes", ""),
        revise("edit", "u2", "petición", " "),
        revise("regenerate", "a2", "respuesta parcial", "injected"),
    ] {
        assert!(branch(&s, input).is_err());
    }
    let mut input = revise("edit", "u2", "petición", "new");
    input.conversation_id = "other".into();
    assert!(branch(&s, input).is_err());
    s.execute("UPDATE messages SET status='streaming' WHERE id='a2'", &[])
        .unwrap();
    assert!(branch(&s, revise("regenerate", "a2", "respuesta parcial", "")).is_err());
    assert_eq!(
        s.query("SELECT count(*) AS n FROM conversations", &[])
            .unwrap()[0]["n"],
        2
    );
}
#[test]
fn oversized_history_is_rejected_without_truncation_or_partial_writes() {
    let s = store();
    s.execute(
        "UPDATE messages SET content_text=? WHERE id='a1'",
        &[json!("a".repeat(192001))],
    )
    .unwrap();
    assert!(branch(&s, revise("edit", "u2", "petición", "new"))
        .unwrap_err()
        .starts_with("BRANCH_HISTORY_LIMIT"));
    assert_eq!(
        s.query("SELECT count(*) AS n FROM conversations", &[])
            .unwrap()[0]["n"],
        2
    );
}
