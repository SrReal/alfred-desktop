use super::*;
use crate::semantic_store::{self, DIMENSIONS};
fn fixture() -> Arc<Core> {
    Arc::new(
        Core::new(
            std::env::temp_dir().join(format!("ap-semantic-{}", storage::id())),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap(),
    )
}
fn vector() -> Vec<f32> {
    let mut v = vec![0.0; DIMENSIONS];
    v[0] = 1.0;
    v
}
fn conversation(c: &Core, scope: Option<&str>) -> String {
    let id = storage::id();
    c.store.execute("INSERT INTO conversations(id,title,summary,project_id,created_at,updated_at) VALUES(?,?,?,?,?,?)",&[json!(id),json!("Bases de datos"),json!("La copia nocturna de MySQL en producción está configurada."),json!(scope),json!(storage::now()),json!(storage::now())]).unwrap();
    id
}
#[test]
fn cache_is_incremental_and_invalidates_only_source_changes() {
    let c = fixture();
    assert_eq!(status(&c).unwrap()["enabled"], false);
    c.store
        .set_setting("semantic_memory", &json!(true))
        .unwrap();
    let memory = c
        .store
        .remember(&json!({"title":"Preferencia","content":"Prefiero interfaces grises"}))
        .unwrap();
    let doc = semantic_store::pending(&c.store).unwrap().pop().unwrap();
    assert!(semantic_store::persist(&c.store, &doc, &vector()).unwrap());
    assert!(semantic_store::pending(&c.store).unwrap().is_empty());
    c.store
        .execute("UPDATE semantic_embeddings SET vector_json='[]'", &[])
        .unwrap();
    assert_eq!(semantic_store::pending(&c.store).unwrap().len(), 1);
    semantic_store::persist(&c.store, &doc, &vector()).unwrap();
    c.store
        .execute(
            "UPDATE memories SET pinned=1,use_count=7 WHERE id=?",
            &[memory["id"].clone()],
        )
        .unwrap();
    assert!(semantic_store::pending(&c.store).unwrap().is_empty());
    c.store
        .execute(
            "UPDATE memories SET content='Prefiero interfaces oscuras' WHERE id=?",
            &[memory["id"].clone()],
        )
        .unwrap();
    assert!(!semantic_store::persist(&c.store, &doc, &vector()).unwrap());
    assert_eq!(semantic_store::pending(&c.store).unwrap().len(), 1);
    let doc = semantic_store::pending(&c.store).unwrap().pop().unwrap();
    c.store
        .execute("DELETE FROM memories WHERE id=?", &[memory["id"].clone()])
        .unwrap();
    assert!(!semantic_store::persist(&c.store, &doc, &vector()).unwrap());
}

#[test]
fn lexical_search_still_works_beyond_the_embedding_budget() {
    let c = fixture();
    c.store.0.lock().unwrap().execute_batch("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<10001) INSERT INTO memories(id,type,title,content,normalized_key,confidence,importance,explicit,created_at,updated_at) SELECT 'bulk-'||x,'fact','Archivo',CASE WHEN x=10001 THEN 'zebracontract' ELSE 'fixture archivado' END,'bulk-'||x,1,0.5,1,'2026-10-01T00:00:00Z','2026-10-01T00:00:00Z' FROM n;").unwrap();
    assert!(semantic_store::documents(&c.store).is_err());
    let matches = c.store.retrieve("zebracontract", None).unwrap();
    assert_eq!(matches.as_array().unwrap().len(), 1);
    assert_eq!(matches[0]["id"], "bulk-10001");
}
#[test]
fn summaries_scope_privacy_and_vectors_are_checked_at_write_and_retrieval() {
    let c = fixture();
    c.store
        .set_setting("semantic_memory", &json!(true))
        .unwrap();
    let global = conversation(&c, None);
    let other = conversation(&c, Some("other"));
    for doc in semantic_store::pending(&c.store).unwrap() {
        semantic_store::persist(&c.store, &doc, &vector()).unwrap();
    }
    let matched =
        semantic_store::retrieve(&c.store, "MariaDB", Some("current"), Some(&vector())).unwrap();
    assert_eq!(matched.as_array().unwrap().len(), 1);
    assert_eq!(matched[0]["id"], global);
    assert_eq!(matched[0]["fts_score"], 0.0);
    let snapshot = semantic_store::documents(&c.store)
        .unwrap()
        .into_iter()
        .find(|d| d.row["id"] == global)
        .unwrap();
    c.store
        .execute(
            "UPDATE conversations SET no_memory=1 WHERE id=?",
            &[json!(global)],
        )
        .unwrap();
    assert!(!semantic_store::persist(&c.store, &snapshot, &vector()).unwrap());
    assert!(
        semantic_store::retrieve(&c.store, "MariaDB", Some("current"), Some(&vector()))
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    let doc = semantic_store::documents(&c.store)
        .unwrap()
        .into_iter()
        .find(|d| d.row["id"] == other)
        .unwrap();
    let mut invalid = vector();
    invalid[0] = f32::NAN;
    assert!(semantic_store::persist(&c.store, &doc, &invalid).is_err());
    c.store.set_setting("privacy", &json!(true)).unwrap();
    assert!(!semantic_store::persist(&c.store, &doc, &vector()).unwrap());
    assert!(semantic_store::retrieve(&c.store, "MySQL", None, None)
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    c.store.set_setting("privacy", &json!(false)).unwrap();
    c.store.set_setting("no_memory", &json!(true)).unwrap();
    assert!(semantic_store::documents(&c.store).is_err());
}
#[test]
fn pinned_irrelevant_memory_and_superseded_memory_never_enter_results() {
    let c = fixture();
    c.store
        .set_setting("semantic_memory", &json!(true))
        .unwrap();
    let old = c
        .store
        .remember(&json!({"content":"MySQL anterior"}))
        .unwrap();
    let latest = c
        .store
        .remember(&json!({"content":"MySQL actualizado","supersedes":old["id"]}))
        .unwrap();
    let irrelevant = c
        .store
        .remember(&json!({"content":"Me gustan las novelas históricas"}))
        .unwrap();
    c.store
        .execute(
            "UPDATE memories SET pinned=1 WHERE id=?",
            &[irrelevant["id"].clone()],
        )
        .unwrap();
    let rows = semantic_store::retrieve(&c.store, "MySQL", None, None).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["id"], latest["id"]);
}
#[test]
fn model_redirects_and_opt_in_are_restricted() {
    let c = fixture();
    assert!(start(&c, "download", false).is_err());
    for url in [
        "http://huggingface.co/x",
        "https://huggingface.co.attacker.invalid/x",
        "https://user@huggingface.co/x",
        "https://huggingface.co:444/x",
        "https://127.0.0.1/x",
    ] {
        assert!(!allowed_url(&reqwest::Url::parse(url).unwrap()));
    }
    assert!(allowed_url(
        &reqwest::Url::parse("https://cas-bridge.xethub.hf.co/x").unwrap()
    ));
    assert!(verify_model(&model_dir(&c)).is_err());
}
#[tokio::test]
async fn missing_model_disable_and_cancel_preserve_fts() {
    let c = fixture();
    c.store
        .remember(&json!({"content":"MariaDB local"}))
        .unwrap();
    assert!(set_enabled(&c, true).await.is_err());
    assert_eq!(status(&c).unwrap()["enabled"], false);
    assert_eq!(
        retrieve(&c, "MariaDB", None)
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    c.store
        .set_setting("semantic_memory", &json!(true))
        .unwrap();
    let cancel_token = CancellationToken::new();
    cancel_token.cancel();
    assert!(embed(&c, &["MariaDB".into()], &cancel_token).await.is_err());
    assert!(c.semantic_host.lock().await.is_none());
    set_enabled(&c, false).await.unwrap();
    assert_eq!(
        retrieve(&c, "MariaDB", None)
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn queued_inference_is_cancelled_when_privacy_is_revoked() {
    let c = fixture();
    c.store
        .set_setting("semantic_memory", &json!(true))
        .unwrap();
    let held = c.semantic_host.lock().await;
    let core = c.clone();
    let pending = tokio::spawn(async move {
        embed(
            &core,
            &["Texto sintético".into()],
            &CancellationToken::new(),
        )
        .await
    });
    tokio::task::yield_now().await;
    c.store.set_setting("privacy", &json!(true)).unwrap();
    cancel(&c).unwrap();
    drop(held);
    assert!(pending.await.unwrap().is_err());
    assert!(c.semantic_host.lock().await.is_none());
    assert!(c.store.remember(&json!({"content":"No ingerir"})).is_err());
}
#[test]
fn migration_6_preserves_previous_data_and_retains_restorable_backup() {
    let c = fixture();
    let path = c.root.join("data/alfredpet.db");
    drop(c);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(crate::clipboard::TEST_REMOVE_FUTURE_SCHEMA)
        .unwrap();
    db.execute_batch("DROP TABLE file_cleanup_plans;ALTER TABLE undo_records DROP COLUMN metadata_json;DROP TABLE file_rollbacks;ALTER TABLE undo_records DROP COLUMN staging;ALTER TABLE undo_records DROP COLUMN object_id;DROP TRIGGER projects_au;DROP TRIGGER semantic_memory_update;DROP TRIGGER semantic_memory_delete;DROP TRIGGER semantic_summary_update;DROP TRIGGER semantic_summary_delete;DROP TABLE semantic_embeddings;PRAGMA user_version=5;").unwrap();
    db.execute(
        "INSERT INTO settings VALUES('migration_fixture','true')",
        [],
    )
    .unwrap();
    drop(db);
    let store = storage::Store::open(&path).unwrap();
    assert_eq!(store.setting("migration_fixture"), true);
    assert_eq!(
        store.query("PRAGMA user_version", &[]).unwrap()[0]["user_version"],
        crate::storage::SCHEMA_VERSION
    );
    assert!(std::fs::read_dir(path.parent().unwrap()).unwrap().any(|p| p
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains("pre-v6")));
    assert_eq!(
        store.query("PRAGMA quick_check(1)", &[]).unwrap()[0]["quick_check"],
        "ok"
    );
}
#[tokio::test]
#[ignore = "requires the pinned local model fixture; run explicitly with ALFREDPET_TEST_SEMANTIC_MODEL"]
async fn real_cpu_worker_indexes_and_retrieves_cross_language_without_fts_match() {
    let source = PathBuf::from(
        std::env::var("ALFREDPET_TEST_SEMANTIC_MODEL").expect("pinned fixture required"),
    );
    let c = fixture();
    let root = model_dir(&c);
    std::fs::create_dir_all(root.join("onnx")).unwrap();
    for file in manifest().unwrap().files {
        std::fs::copy(source.join(&file.path), root.join(&file.path)).unwrap();
    }
    std::fs::write(root.join(".complete"), REVISION).unwrap();
    let selected = conversation(&c, None);
    let other = conversation(&c, Some("other"));
    c.store
        .remember(&json!({"content":"Me gustan las novelas históricas"}))
        .unwrap();
    c.store
        .set_setting("semantic_memory", &json!(true))
        .unwrap();
    let id = storage::id();
    let token = CancellationToken::new();
    c.semantic_control.lock().unwrap().job = Some((id.clone(), token.clone()));
    assert_eq!(index(&c, &id, &token, false).await.unwrap()["indexed"], 3);
    let vectors = embed(
        &c,
        &[
            "lo de MariaDB del servidor".into(),
            "production database backup".into(),
        ],
        &token,
    )
    .await
    .unwrap();
    for query in &vectors {
        let rows =
            semantic_store::retrieve(&c.store, "MariaDB", Some("current"), Some(query)).unwrap();
        assert!(rows.as_array().unwrap().iter().any(|r| r["id"] == selected));
        assert!(!rows.as_array().unwrap().iter().any(|r| r["id"] == other));
    }
    assert!(semantic_store::pending(&c.store).unwrap().is_empty());
    set_enabled(&c, false).await.unwrap();
    assert!(c.semantic_host.lock().await.is_none());
}

#[tokio::test]
#[ignore = "explicit developer opt-in: downloads the 122MiB pinned public model to a disposable test profile"]
async fn real_model_download_is_verified_and_does_not_enable() {
    let c = fixture();
    let id = storage::id();
    let token = CancellationToken::new();
    c.semantic_control.lock().unwrap().job = Some((id.clone(), token.clone()));
    let result = download(&c, &id, &token).await.unwrap();
    assert_eq!(result["downloaded"], 127540686);
    verify_model(&model_dir(&c)).unwrap();
    assert_eq!(status(&c).unwrap()["installed"], true);
    assert_eq!(status(&c).unwrap()["enabled"], false);
    assert_eq!(c.store.setting("semantic_memory"), Value::Null);
    assert!(c.semantic_host.lock().await.is_none());
}
