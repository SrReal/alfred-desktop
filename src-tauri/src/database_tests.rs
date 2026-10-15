use super::*;
use std::os::windows::fs::MetadataExt;
struct Fixture {
    core: Arc<Core>,
    root: PathBuf,
    db: PathBuf,
    id: String,
    identity: String,
}
impl Fixture {
    async fn new(read_only: bool) -> Self {
        let root = std::env::temp_dir().join(format!("ap-database-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join("workspace")).unwrap();
        let identity = asset_files::identity(&asset_files::directory(&root).unwrap()).unwrap();
        let db = root.join("workspace/owned.sqlite");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch("CREATE TABLE items(id INTEGER PRIMARY KEY,n INTEGER NOT NULL,name TEXT,payload BLOB);INSERT INTO items VALUES(1,1,'Señor 🐾',X'0001FF'); INSERT INTO items VALUES(2,2,NULL,NULL);").unwrap();
        }
        let core = Arc::new(
            Core::new(
                root.join("profile"),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        core.store
            .set_setting(
                "allowed_roots",
                &json!([root.join("workspace").canonicalize().unwrap()]),
            )
            .unwrap();
        let saved = integrations::save(&core,&json!({"module_id":"database","name":"Owned SQLite fixture","config":{"engine":"sqlite","path":db,"read_only":read_only}})).await.unwrap();
        core.store
            .execute(
                "UPDATE module_configs SET enabled=1 WHERE module_id='database'",
                &[],
            )
            .unwrap();
        Self {
            core,
            root,
            db,
            id: saved["id"].as_str().unwrap().into(),
            identity,
        }
    }
    fn input(&self, extra: Value) -> Value {
        let mut v = json!({"connection":self.id});
        for (k, value) in extra.as_object().unwrap() {
            v[k] = value.clone();
        }
        v
    }
    async fn prepared(&self, tool: &str, input: Value) -> Prepared {
        prepare(&self.core, tool, &self.input(input)).await.unwrap()
    }
    async fn meta(&self) -> Value {
        self.core
            .execute("database.schema", self.input(json!({})))
            .await
            .unwrap()
    }
    fn count(&self) -> i64 {
        Connection::open(&self.db)
            .unwrap()
            .query_row("SELECT count(*) FROM items", [], |row| row.get(0))
            .unwrap()
    }
    fn clean(self) {
        let Self {
            core,
            root,
            identity,
            ..
        } = self;
        drop(core);
        assert_eq!(
            root.canonicalize().unwrap().parent(),
            Some(std::env::temp_dir().canonicalize().unwrap().as_path())
        );
        assert!(root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("ap-database-"));
        assert_eq!(
            asset_files::identity(&asset_files::directory(&root).unwrap()).unwrap(),
            identity
        );
        let mut pending = vec![root.clone()];
        while let Some(p) = pending.pop() {
            assert_eq!(
                std::fs::symlink_metadata(&p).unwrap().file_attributes() & 0x400,
                0
            );
            if p.is_dir() {
                pending.extend(std::fs::read_dir(p).unwrap().map(|e| e.unwrap().path()));
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
#[tokio::test]
async fn database_schema_read_plan_and_output_bounds() {
    let f = Fixture::new(true).await;
    let meta = f.meta().await;
    assert_eq!(meta["engine"], "sqlite");
    assert_eq!(meta["fingerprint"].as_str().unwrap().len(), 64);
    let r=f.core.execute("database.query",f.input(json!({"sql":"WITH a AS (SELECT * FROM items) SELECT name,payload FROM a ORDER BY id","limit":1}))).await.unwrap();
    assert_eq!(r["rows"][0][0], "Señor 🐾");
    assert_eq!(r["rows"][0][1]["blob_base64"], "AAH/");
    assert_eq!(r["incomplete"], true);
    let p = f
        .core
        .execute(
            "database.explain",
            f.input(json!({"sql":"SELECT * FROM items WHERE id=?","parameters":[1]})),
        )
        .await
        .unwrap();
    assert!(!p["rows"].as_array().unwrap().is_empty());
    assert_eq!(f.count(), 2);
    for sql in [
        "SELECT load_extension('x')",
        "SELECT 1; DELETE FROM items",
        "PRAGMA writable_schema=1",
        "SELECT writefile('x','y')",
    ] {
        assert!(
            prepare(&f.core, "database.query", &f.input(json!({"sql":sql})))
                .await
                .is_err()
        );
    }
    f.clean();
}
#[tokio::test]
async fn database_readonly_denies_writes_before_permission() {
    let f = Fixture::new(true).await;
    let meta = f.meta().await;
    let e = prepare(
        &f.core,
        "database.execute",
        &f.input(json!({"sql":"DELETE FROM items","fingerprint":meta["fingerprint"]})),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(e, "DATABASE_READ_ONLY");
    assert_eq!(f.count(), 2);
    assert!(f
        .core
        .store
        .query("SELECT * FROM permission_requests", &[])
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    f.clean();
}
#[tokio::test]
async fn database_import_export_roundtrip_and_atomic_failure() {
    let f = Fixture::new(false).await;
    let meta = f.meta().await;
    let path = f.root.join("workspace/export.json");
    let p = f
        .prepared(
            "database.export",
            json!({"sql":"SELECT id,n,name,payload FROM items ORDER BY id","path":path}),
        )
        .await;
    assert_eq!(p.risk, Risk::Contextual);
    let output = execute(p).await.unwrap();
    assert_eq!(output["rows"], 2);
    let bytes = std::fs::read(&path).unwrap();
    let data = rows(&bytes).unwrap();
    assert_eq!(data.rows[0][2], "Señor 🐾");
    assert!(prepare(
        &f.core,
        "database.export",
        &f.input(json!({"sql":"SELECT 1","path":path}))
    )
    .await
    .is_err());
    let mut data = data;
    data.rows[0][0] = json!(3);
    data.rows[1][0] = json!(1); // first insert valid, second duplicate -> rollback both
    let import = f.root.join("workspace/import.json");
    std::fs::write(&import, serde_json::to_vec(&data).unwrap()).unwrap();
    let p = f
        .prepared(
            "database.import",
            json!({"path":import,"table":"items","fingerprint":meta["fingerprint"]}),
        )
        .await;
    assert!(execute(p).await.is_err());
    assert_eq!(f.count(), 2);
    data.rows[1][0] = json!(4);
    std::fs::write(&import, serde_json::to_vec(&data).unwrap()).unwrap();
    let p = f
        .prepared(
            "database.import",
            json!({"path":import,"table":"items","fingerprint":meta["fingerprint"]}),
        )
        .await;
    assert_eq!(execute(p).await.unwrap()["changed"], 2);
    assert_eq!(f.count(), 4);
    f.clean();
}
#[tokio::test]
async fn database_schema_and_scope_change_prevent_write() {
    let f = Fixture::new(false).await;
    let meta = f.meta().await;
    let p = f
        .prepared(
            "database.execute",
            json!({"sql":"UPDATE items SET n=9 WHERE id=1","fingerprint":meta["fingerprint"]}),
        )
        .await;
    Connection::open(&f.db)
        .unwrap()
        .execute_batch("ALTER TABLE items ADD COLUMN added TEXT")
        .unwrap();
    assert_eq!(
        execute(p).await.unwrap_err(),
        "DATABASE_SCHEMA_OBSERVATION_CHANGED"
    );
    assert!(prepare(
        &f.core,
        "database.execute",
        &f.input(json!({"sql":"DELETE FROM items","fingerprint":meta["fingerprint"]}))
    )
    .await
    .is_err());
    let meta = f.meta().await;
    let p = f
        .prepared(
            "database.execute",
            json!({"sql":"DELETE FROM items WHERE id=1","fingerprint":meta["fingerprint"]}),
        )
        .await;
    f.core
        .store
        .set_setting(
            &format!("database.connection_generation.{}", f.id),
            &json!(uuid::Uuid::new_v4()),
        )
        .unwrap();
    assert_eq!(execute(p).await.unwrap_err(), "DATABASE_CONNECTION_CHANGED");
    assert_eq!(f.count(), 2);
    f.clean();
}
#[tokio::test]
async fn database_authorizer_blocks_indirect_trigger_function() {
    let f = Fixture::new(false).await;
    Connection::open(&f.db).unwrap().execute_batch("CREATE TRIGGER changed AFTER UPDATE ON items BEGIN SELECT load_extension('never'); END").unwrap();
    let meta = f.meta().await;
    let p = f
        .prepared(
            "database.execute",
            json!({"sql":"UPDATE items SET n=9 WHERE id=1","fingerprint":meta["fingerprint"]}),
        )
        .await;
    assert!(execute(p).await.is_err());
    assert_eq!(
        Connection::open(&f.db)
            .unwrap()
            .query_row::<i64, _, _>("SELECT n FROM items WHERE id=1", [], |r| r.get(0))
            .unwrap(),
        1
    );
    f.clean();
}
#[tokio::test]
async fn database_policy_review_separates_durable_content() {
    let f = Fixture::new(false).await;
    let meta = f.meta().await;
    let p=f.prepared("database.execute",json!({"sql":"INSERT INTO items(id,n,name) VALUES(?,?,?)","parameters":[3,3,"private fixture text"],"fingerprint":meta["fingerprint"]})).await;
    assert_eq!(p.risk, Risk::Contextual);
    assert!(p.review().to_string().contains("private fixture text"));
    let redacted = crate::policy::tool_redact("database.execute", &p.review());
    assert!(!redacted.to_string().contains("private fixture text"));
    drop(p);
    let denied = f
        .core
        .execute(
            "database.execute",
            f.input(json!({"sql":"DELETE FROM items","fingerprint":meta["fingerprint"]})),
        )
        .await
        .unwrap_err();
    // Headless fixture has no UI app: no action executes on inability to show L3.
    assert_eq!(denied, "APP_MISSING");
    assert_eq!(f.count(), 2);
    f.clean();
}
#[tokio::test]
async fn database_connection_config_validation_and_invalid_inputs() {
    let f = Fixture::new(false).await;
    for config in [
        json!({"engine":"sqlite","path":f.db,"password":"fictional"}),
        json!({"engine":"sqlserver","host":"db.example","port":1433,"database":"owned","user":"reader","driver":"SQL Server","ssl":"verify_full"}),
        json!({"engine":"sqlite","path":f.db,"read_only":"false"}),
    ] {
        assert!(validate_config(&config).is_err());
    }
    assert!(validate_config(&json!({"engine":"sqlserver","host":"db.example","port":1433,"database":"owned","user":"reader","driver":"ODBC Driver 18 for SQL Server","ssl":"verify_full"})).is_ok());
    assert!(rows(br#"{"format":"alfredpet.database.rows.v1","columns":["id"],"rows":[[1]],"incomplete":true}"#).is_err());
    assert!(f
        .core
        .execute(
            "database.query",
            f.input(json!({"sql":"SELECT 1","unknown":true}))
        )
        .await
        .is_err());
    assert!(prepare(
        &f.core,
        "database.query",
        &f.input(json!({"sql":"SELECT ?","parameters":[{"unexpected":1}]}))
    )
    .await
    .is_err());
    f.clean();
}

#[tokio::test]
async fn database_privacy_interrupts_inflight_write_and_rolls_back() {
    let f = Fixture::new(false).await;
    let meta = f.meta().await;
    let sql="INSERT INTO items(id,n) WITH RECURSIVE counter(n) AS (VALUES(3) UNION ALL SELECT n+1 FROM counter WHERE n<1000000000) SELECT n,n FROM counter";
    let p = f
        .prepared(
            "database.execute",
            json!({"sql":sql,"fingerprint":meta["fingerprint"]}),
        )
        .await;
    let task = tokio::spawn(execute(p));
    let started = Instant::now();
    let mut active = false;
    while started.elapsed() < Duration::from_secs(3) {
        let probe = Connection::open(&f.db).unwrap();
        probe.busy_timeout(Duration::ZERO).unwrap();
        match probe.execute_batch("BEGIN IMMEDIATE") {
            Ok(()) => {
                probe.execute_batch("ROLLBACK").unwrap();
            }
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::DatabaseBusy =>
            {
                active = true;
                break;
            }
            Err(e) => panic!("Unexpected owned probe: {e}"),
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(active, "owned write never entered its transaction");
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_err());
    assert_eq!(f.count(), 2);
    f.core.store.set_setting("privacy", &json!(false)).unwrap();
    f.clean();
}

#[tokio::test]
async fn database_core_dispatch_applies_contextual_review_and_redaction() {
    let f = Fixture::new(false).await;
    let meta = f.meta().await;
    let input=f.input(json!({"sql":"UPDATE items SET name=? WHERE id=?","parameters":["private data",1],"fingerprint":meta["fingerprint"]}));
    let p = prepare(&f.core, "database.execute", &input).await.unwrap();
    // This is a synthetic existing L2 grant, not a human permission UI pass.
    f.core
        .grants
        .lock()
        .unwrap()
        .insert(format!("database.execute:{}", p.review()), Risk::Contextual);
    drop(p);
    assert_eq!(
        f.core.execute("database.execute", input).await.unwrap()["changed"],
        1
    );
    let audit=f.core.store.query("SELECT input_summary,output_summary FROM activity_events WHERE tool_id='database.execute'",&[]).unwrap();
    assert_eq!(audit.as_array().unwrap().len(), 1);
    assert!(!audit.to_string().contains("private data"));
    f.clean();
}

#[tokio::test]
async fn database_real_sqlite_ddl_and_like_are_observed_and_mandatory() {
    let f = Fixture::new(false).await;
    for sql in [
        "CREATE TABLE owned_extra(id INTEGER PRIMARY KEY,name TEXT)",
        "ALTER TABLE owned_extra ADD COLUMN extra TEXT",
        "ALTER TABLE owned_extra RENAME COLUMN name TO label",
        "ALTER TABLE owned_extra RENAME TO owned_renamed",
        "DROP TABLE owned_renamed",
    ] {
        let meta = f.meta().await;
        let p = f
            .prepared(
                "database.execute",
                json!({"sql":sql,"fingerprint":meta["fingerprint"]}),
            )
            .await;
        assert_eq!(p.risk, Risk::Mandatory);
        assert!(p.review().get("schema").is_some());
        execute(p).await.unwrap();
    }
    let r = f
        .core
        .execute(
            "database.query",
            f.input(json!({"sql":"SELECT name FROM items WHERE name LIKE 'Se%'"})),
        )
        .await
        .unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 1);
    f.clean();
}

#[tokio::test]
async fn database_hardlinked_sidecars_are_denied_without_touching_canary() {
    let f = Fixture::new(true).await;
    let canary = f.root.join("workspace/canary.txt");
    std::fs::write(&canary, b"owned canary intact").unwrap();
    let mut wal = f.db.as_os_str().to_os_string();
    wal.push("-wal");
    let wal = PathBuf::from(wal);
    std::fs::hard_link(&canary, &wal).unwrap();
    let result = prepare(&f.core, "database.schema", &f.input(json!({}))).await;
    assert!(result.is_err());
    assert_eq!(std::fs::read(&canary).unwrap(), b"owned canary intact");
    std::fs::remove_file(wal).unwrap();
    f.clean();
}

#[tokio::test]
async fn database_endpoint_changes_require_explicit_credential_before_any_write() {
    let f = Fixture::new(false).await;
    let config = json!({"engine":"sqlserver","host":"owned.example","port":1433,"database":"owned","user":"reader","driver":"ODBC Driver 18 for SQL Server","ssl":"verify_full","read_only":true});
    // Seed configuration only: no WCM credential exists and no server is opened.
    f.core
        .store
        .execute(
            "UPDATE connections SET config_json=? WHERE id=?",
            &[json!(config.to_string()), json!(f.id)],
        )
        .unwrap();
    let generation = f
        .core
        .store
        .setting(&format!("database.connection_generation.{}", f.id));
    for (key, value) in [
        ("host", json!("other.example")),
        ("user", json!("different")),
        ("port", json!(1444)),
        ("database", json!("other")),
        ("ssl", json!("require")),
    ] {
        let mut changed = config.clone();
        changed[key] = value;
        let error = integrations::save(
            &f.core,
            &json!({"id":f.id,"module_id":"database","name":"Changed endpoint","config":changed}),
        )
        .await
        .unwrap_err();
        assert_eq!(error, "DATABASE_CREDENTIAL_REQUIRED_FOR_TARGET_CHANGE");
        assert_eq!(
            f.core
                .store
                .setting(&format!("database.connection_generation.{}", f.id)),
            generation
        );
        assert_eq!(
            serde_json::from_str::<Value>(
                record(&f.core, &f.id).unwrap()["config_json"]
                    .as_str()
                    .unwrap()
            )
            .unwrap(),
            config
        );
    }
    let mut unchanged_target = config.clone();
    unchanged_target["read_only"] = json!(false);
    integrations::save(
        &f.core,
        &json!({"id":f.id,"module_id":"database","name":"Same endpoint","config":unchanged_target}),
    )
    .await
    .unwrap();
    assert_ne!(
        f.core
            .store
            .setting(&format!("database.connection_generation.{}", f.id)),
        generation
    );
    f.clean();
}
