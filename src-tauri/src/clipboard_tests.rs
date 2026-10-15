use super::*;
use crate::{asset_files, catalog, policy::Risk};
use std::{
    os::windows::fs::MetadataExt,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
};

struct Fixture {
    core: Arc<Core>,
    root: PathBuf,
    identity: String,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ap-clipboard-{}", storage::id()));
        std::fs::create_dir(&root).unwrap();
        let identity = asset_files::identity(&asset_files::directory(&root).unwrap()).unwrap();
        let core = Arc::new(
            Core::new(
                root.clone(),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        core.store
            .execute(
                "UPDATE module_configs SET enabled=1 WHERE module_id='clipboard'",
                &[],
            )
            .unwrap();
        Self {
            core,
            root,
            identity,
        }
    }
    fn control(&self) -> Control {
        Control::new(&self.core, CancellationToken::new()).unwrap()
    }
    fn count(&self) -> i64 {
        self.core
            .store
            .query("SELECT count(*) AS n FROM clipboard_history", &[])
            .unwrap()[0]["n"]
            .as_i64()
            .unwrap()
    }
    fn finish(self) {
        assert_eq!(
            self.root.parent().unwrap().canonicalize().unwrap(),
            std::env::temp_dir().canonicalize().unwrap()
        );
        assert!(self
            .root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("ap-clipboard-"));
        assert_eq!(
            asset_files::identity(&asset_files::directory(&self.root).unwrap()).unwrap(),
            self.identity
        );
        let mut paths = vec![self.root.clone()];
        while let Some(path) = paths.pop() {
            assert_eq!(
                std::fs::symlink_metadata(&path).unwrap().file_attributes() & 0x400,
                0
            );
            if path.is_dir() {
                paths.extend(std::fs::read_dir(path).unwrap().map(|p| p.unwrap().path()));
            }
        }
        drop(self.core);
        std::fs::remove_dir_all(self.root).unwrap();
    }
}
struct Fake {
    snapshot: Mutex<Snapshot>,
    reads: AtomicUsize,
    writes: AtomicUsize,
    sequences: AtomicUsize,
    fail: Mutex<Option<String>>,
    revoke: Mutex<bool>,
    bad_readback: Mutex<bool>,
}
impl Fake {
    fn new(text: &str) -> Self {
        Self {
            snapshot: Mutex::new(Snapshot {
                sequence: 1,
                formats: vec![13],
                text: Some(text.into()),
                image: false,
                excluded: false,
            }),
            reads: AtomicUsize::new(0),
            writes: AtomicUsize::new(0),
            sequences: AtomicUsize::new(0),
            fail: Mutex::new(None),
            revoke: Mutex::new(false),
            bad_readback: Mutex::new(false),
        }
    }
    fn change(&self, text: &str) {
        let mut s = self.snapshot.lock().unwrap();
        s.sequence += 1;
        s.text = Some(text.into());
    }
    fn expected(&self) -> Expected {
        self.snapshot.lock().unwrap().expected()
    }
}
impl Adapter for Fake {
    fn sequence(&self) -> Result<u32, String> {
        self.sequences.fetch_add(1, Ordering::SeqCst);
        Ok(self.snapshot.lock().unwrap().sequence)
    }
    fn read(&self, c: &Control) -> Result<Snapshot, String> {
        c.check()?;
        self.reads.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = self.fail.lock().unwrap().take() {
            return Err(error);
        }
        let result = self.snapshot.lock().unwrap().clone();
        if *self.revoke.lock().unwrap() {
            c.core.store.set_setting("privacy", &json!(true)).unwrap();
        }
        Ok(result)
    }
    fn write(&self, c: &Control, expected: &Expected, text: &str) -> Result<Snapshot, String> {
        c.check()?;
        let mut s = self.snapshot.lock().unwrap();
        if &s.expected() != expected {
            return Err("CLIPBOARD_CHANGED_REINSPECT".into());
        }
        self.writes.fetch_add(1, Ordering::SeqCst);
        s.sequence += 1;
        s.text = Some(if *self.bad_readback.lock().unwrap() {
            "unexpected".into()
        } else {
            text.into()
        });
        Ok(s.clone())
    }
    fn image(&self, _: &Control, _: &Expected) -> Result<Vec<u8>, String> {
        Err("FAKE_NO_IMAGE".into())
    }
}

#[test]
fn unicode_bounds_sensitive_patterns_and_type_detection() {
    assert!(text_valid(&"😀".repeat(MAX_CHARS)).is_ok());
    assert_eq!(
        text_valid(&"😀".repeat(MAX_CHARS + 1)).unwrap_err(),
        "CLIPBOARD_TEXT_LIMIT"
    );
    assert_eq!(text_valid("a\0b").unwrap_err(), "CLIPBOARD_TEXT_NUL_DENIED");
    for text in [
        "Authorization: fixture",
        "Cookie: fixture",
        "-----BEGIN PRIVATE KEY-----",
        "https://user:fake@example.org/",
        "https://example.org/?%74oken=fixture",
        "AKIA1234567890ABCDEF12",
        "a".repeat(32).as_str(),
    ] {
        assert!(sensitive(text), "{text}");
    }
    assert!(!sensitive("Una guía de viaje por Asia y Europa"));
    assert_eq!(detect(" https://example.org/path "), "url");
    assert_eq!(detect("{\"count\":1}"), "code");
    assert_eq!(detect("def fixture():\n  pass"), "code");
    assert_eq!(detect("Hola mundo"), "text");
    let sensitive = Snapshot {
        sequence: 3,
        formats: vec![13],
        text: Some("Cookie: fixture".into()),
        image: false,
        excluded: false,
    };
    assert_eq!(
        sensitive.data(true).unwrap_err(),
        "SENSITIVE_CLIPBOARD_DENIED"
    );
    assert_eq!(sensitive.data(false).unwrap()["kind"], "sensitive");
}
#[test]
fn lexical_json_preserves_precision_duplicate_keys_and_escaped_strings() {
    let input = r#" { "n" : 900719925474099312345, "n":1.23000e+123, "s" : "a \"quote\" \\ b", "empty" : [ {}, [] ] } "#;
    let minified = transform(input, "minify_json").unwrap();
    assert_eq!(
        minified,
        r#"{"n":900719925474099312345,"n":1.23000e+123,"s":"a \"quote\" \\ b","empty":[{},[]]}"#
    );
    let pretty = transform(input, "pretty_json").unwrap();
    assert_eq!(transform(&pretty, "minify_json").unwrap(), minified);
    assert!(pretty.contains("900719925474099312345"));
    assert!(pretty.contains("{}"));
    assert!(pretty.contains("[]"));
    assert_eq!(
        transform("{bad}", "pretty_json").unwrap_err(),
        "CLIPBOARD_JSON_INVALID"
    );
    assert_eq!(transform("z\na\nz", "unique_lines").unwrap(), "z\na");
    assert_eq!(transform("z\na\nb", "sort_lines").unwrap(), "a\nb\nz");
    assert_eq!(
        transform(" á\t β\n 😀 ", "normalize_whitespace").unwrap(),
        "á β 😀"
    );
    assert_eq!(
        transform(&"ß ".repeat(MAX_CHARS / 2), "uppercase").unwrap_err(),
        "CLIPBOARD_TEXT_LIMIT"
    );
}
#[test]
fn previews_stale_observations_and_readback_do_not_hide_mutation_failures() {
    let f = Fixture::new();
    let fake = Fake::new("  Observed 😀  ");
    let c = f.control();
    let preview = dispatch_with(
        &c,
        &fake,
        "clipboard.transform",
        &json!({"operation":"trim","expected":fake.expected()}),
    )
    .unwrap();
    assert_eq!(preview["text"], "Observed 😀");
    assert_eq!(preview["preview_only"], true);
    assert_eq!(fake.writes.load(Ordering::SeqCst), 0);
    let expected = fake.expected();
    fake.change("Concurrent");
    assert_eq!(
        dispatch_with(
            &c,
            &fake,
            "clipboard.set",
            &json!({"text":"Requested","expected":expected})
        )
        .unwrap_err(),
        "CLIPBOARD_CHANGED_REINSPECT"
    );
    assert_eq!(fake.writes.load(Ordering::SeqCst), 0);
    let written = dispatch_with(
        &c,
        &fake,
        "clipboard.set",
        &json!({"text":"Requested 😀","expected":fake.expected()}),
    )
    .unwrap();
    assert_eq!(written["written"], true);
    assert_ne!(written["expected"], json!(expected));
    *fake.bad_readback.lock().unwrap() = true;
    assert_eq!(
        dispatch_with(
            &c,
            &fake,
            "clipboard.set",
            &json!({"text":"Second","expected":fake.expected()})
        )
        .unwrap_err(),
        "CLIPBOARD_WRITE_READBACK_FAILED"
    );
    drop(c);
    f.finish();
}
#[test]
fn cancellation_module_privacy_and_generations_revoke_inflight_scope() {
    let f = Fixture::new();
    let c = f.control();
    c.cancel.cancel();
    assert_eq!(c.check().unwrap_err(), "CLIPBOARD_CANCELLED");
    drop(c);
    let c = f.control();
    set_history(&f.core, true).unwrap();
    assert_eq!(c.check().unwrap_err(), "CLIPBOARD_SCOPE_CHANGED");
    drop(c);
    let c = f.control();
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(c.check().unwrap_err(), "PRIVACY_DENIED");
    f.core.store.set_setting("privacy", &json!(false)).unwrap();
    assert_eq!(c.check().unwrap_err(), "CLIPBOARD_SCOPE_CHANGED");
    drop(c);
    let c = f.control();
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='clipboard'",
            &[],
        )
        .unwrap();
    assert_eq!(c.check().unwrap_err(), "TOOL_DISABLED_OR_UNKNOWN");
    drop(c);
    f.finish();
}
#[test]
fn history_requires_versioned_optin_and_does_not_capture_existing_content() {
    let f = Fixture::new();
    let fake = Fake::new("Existing before consent");
    let mut state = Observer::default();
    assert!(!observe(&f.core, &fake, &mut state).unwrap());
    assert_eq!(fake.sequences.load(Ordering::SeqCst), 0);
    assert_eq!(fake.reads.load(Ordering::SeqCst), 0);
    f.core
        .store
        .set_setting("clipboard_history", &json!(true))
        .unwrap();
    assert!(!observe(&f.core, &fake, &mut state).unwrap());
    assert_eq!(fake.sequences.load(Ordering::SeqCst), 0);
    set_history(&f.core, true).unwrap();
    assert!(!observe(&f.core, &fake, &mut state).unwrap());
    assert_eq!(fake.reads.load(Ordering::SeqCst), 0);
    fake.change("New after consent");
    assert!(observe(&f.core, &fake, &mut state).unwrap());
    assert_eq!(f.count(), 1);
    assert!(!observe(&f.core, &fake, &mut state).unwrap());
    assert_eq!(fake.reads.load(Ordering::SeqCst), 1);
    set_history(&f.core, false).unwrap();
    fake.change("Disabled content");
    assert!(!observe(&f.core, &fake, &mut state).unwrap());
    assert_eq!(fake.reads.load(Ordering::SeqCst), 1);
    assert_eq!(
        history(&f.core, &json!({})).unwrap_err(),
        "CLIPBOARD_HISTORY_DISABLED"
    );
    assert_eq!(f.count(), 1);
    f.finish();
}
#[test]
fn observer_busy_retry_secret_exclusion_and_revocation_prevent_persistence() {
    let f = Fixture::new();
    let fake = Fake::new("Baseline");
    let mut state = Observer::default();
    set_history(&f.core, true).unwrap();
    observe(&f.core, &fake, &mut state).unwrap();
    fake.change("New bounded fixture");
    *fake.fail.lock().unwrap() = Some("CLIPBOARD_BUSY".into());
    assert_eq!(
        observe(&f.core, &fake, &mut state).unwrap_err(),
        "CLIPBOARD_BUSY"
    );
    assert!(observe(&f.core, &fake, &mut state).unwrap());
    assert_eq!(f.count(), 1);
    fake.change("Cookie: fake fixture");
    assert!(!observe(&f.core, &fake, &mut state).unwrap());
    assert_eq!(f.count(), 1);
    fake.change("Provider excluded");
    fake.snapshot.lock().unwrap().excluded = true;
    assert!(!observe(&f.core, &fake, &mut state).unwrap());
    assert_eq!(f.count(), 1);
    fake.snapshot.lock().unwrap().excluded = false;
    {
        let mut s = fake.snapshot.lock().unwrap();
        s.sequence += 1;
        s.text = None;
        s.image = true;
    }
    assert!(!observe(&f.core, &fake, &mut state).unwrap());
    assert_eq!(f.count(), 1);
    fake.change("Late after privacy change");
    *fake.revoke.lock().unwrap() = true;
    assert_eq!(
        observe(&f.core, &fake, &mut state).unwrap_err(),
        "PRIVACY_DENIED"
    );
    assert_eq!(f.count(), 1);
    let reads = fake.reads.load(Ordering::SeqCst);
    assert!(!observe(&f.core, &fake, &mut state).unwrap());
    assert_eq!(fake.reads.load(Ordering::SeqCst), reads);
    f.finish();
}
#[test]
fn history_deduplicates_expires_bounds_storage_and_clear_invalidates_scope() {
    let f = Fixture::new();
    let fake = Fake::new("Baseline");
    let mut state = Observer::default();
    set_history(&f.core, true).unwrap();
    observe(&f.core, &fake, &mut state).unwrap();
    for text in ["Same fixture", "Other fixture", "Same fixture"] {
        fake.change(text);
        observe(&f.core, &fake, &mut state).unwrap();
    }
    assert_eq!(f.count(), 2);
    for i in 0..110 {
        fake.change(&format!("Fixture entry {i}"));
        observe(&f.core, &fake, &mut state).unwrap();
    }
    assert_eq!(f.count(), 100);
    for i in 0..12 {
        fake.change(&format!("Entry {i} {}", "long fixture ".repeat(4900)));
        observe(&f.core, &fake, &mut state).unwrap();
    }
    assert!(
        f.core
            .store
            .query(
                "SELECT sum(length(CAST(text AS BLOB))) AS n FROM clipboard_history",
                &[]
            )
            .unwrap()[0]["n"]
            .as_i64()
            .unwrap()
            <= HISTORY_BYTES
    );
    let rows = history(&f.core, &json!({"limit":2,"offset":0})).unwrap();
    assert_eq!(rows["items"].as_array().unwrap().len(), 2);
    f.core
        .store
        .execute(
            "UPDATE clipboard_history SET expires_at='2000-01-01T00:00:00Z'",
            &[],
        )
        .unwrap();
    observe(&f.core, &fake, &mut state).unwrap();
    assert_eq!(f.count(), 0);
    fake.change("Fresh fixture");
    observe(&f.core, &fake, &mut state).unwrap();
    let c = f.control();
    assert_eq!(
        delete_history(&f.core, "clipboard.history_clear", &json!({})).unwrap()["deleted"],
        1
    );
    assert_eq!(c.check().unwrap_err(), "CLIPBOARD_SCOPE_CHANGED");
    assert!(!observe(&f.core, &fake, &mut state).unwrap());
    assert_eq!(f.count(), 0);
    drop(c);
    f.finish();
}
#[test]
fn audit_omits_all_clipboard_text_even_without_history() {
    let f = Fixture::new();
    let text = "Private ordinary content without a recognizable credential";
    f.core
        .store
        .audit(
            "clipboard.get",
            "free",
            &json!({}),
            "success",
            &json!({"text":text,"items":[{"content":text}]}),
            1,
        )
        .unwrap();
    f.core
        .store
        .audit(
            "clipboard.set",
            "contextual",
            &json!({"text":text}),
            "denied",
            &json!({"old_text":text}),
            1,
        )
        .unwrap();
    let rows = f
        .core
        .store
        .query("SELECT * FROM activity_events", &[])
        .unwrap()
        .to_string();
    assert!(!rows.contains(text));
    assert!(rows.contains("CLIPBOARD_CONTENT_OMITTED"));
    assert_eq!(f.count(), 0);
    f.finish();
}

#[path = "clipboard_native_tests.rs"]
mod native;

#[test]
fn dib_fixture_decoding_bounds_dimensions_allocation_and_cancellation_without_native_reads() {
    let f = Fixture::new();
    let c = f.control();
    let bytes = crate::clipboard_native::decode_dib(&c, native::dib()).unwrap();
    let image = image::load_from_memory(&bytes).unwrap();
    assert_eq!((image.width(), image.height()), (2, 2));
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    let mut oversized = native::dib();
    oversized[4..8].copy_from_slice(&8193i32.to_le_bytes());
    assert_eq!(
        crate::clipboard_native::decode_dib(&c, oversized).unwrap_err(),
        "CLIPBOARD_IMAGE_DIMENSION_LIMIT"
    );
    assert_eq!(
        crate::clipboard_native::decode_dib(&c, vec![]).unwrap_err(),
        "CLIPBOARD_IMAGE_UNSUPPORTED"
    );
    assert_eq!(
        crate::clipboard_native::decode_dib(&c, vec![0; 32 * 1024 * 1024 + 1]).unwrap_err(),
        "CLIPBOARD_IMAGE_BYTE_LIMIT"
    );
    c.cancel.cancel();
    assert_eq!(
        crate::clipboard_native::decode_dib(&c, native::dib()).unwrap_err(),
        "CLIPBOARD_CANCELLED"
    );
    drop(c);
    f.finish();
}

#[test]
fn history_insert_failure_is_retryable_without_marking_the_sequence_saved() {
    let f = Fixture::new();
    let fake = Fake::new("Baseline");
    let mut state = Observer::default();
    set_history(&f.core, true).unwrap();
    observe(&f.core, &fake, &mut state).unwrap();
    fake.change("Retry fixture");
    f.core.store.0.lock().unwrap().execute_batch("CREATE TRIGGER fixture_clipboard_failure BEFORE INSERT ON clipboard_history BEGIN SELECT RAISE(ABORT,'Owned fixture failure'); END;").unwrap();
    assert_eq!(
        observe(&f.core, &fake, &mut state).unwrap_err(),
        "CLIPBOARD_HISTORY_INSERT"
    );
    assert_eq!(f.count(), 0);
    f.core
        .store
        .0
        .lock()
        .unwrap()
        .execute_batch("DROP TRIGGER fixture_clipboard_failure;")
        .unwrap();
    assert!(observe(&f.core, &fake, &mut state).unwrap());
    assert_eq!(f.count(), 1);
    f.finish();
}
#[test]
fn independent_history_actors_deduplicate_without_losing_optin_generation() {
    let f = Fixture::new();
    let second = Arc::new(
        Core::new(
            f.root.clone(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap(),
    );
    let fake = Arc::new(Fake::new("Baseline"));
    set_history(&f.core, true).unwrap();
    let mut first_state = Observer::default();
    let mut second_state = Observer::default();
    observe(&f.core, &*fake, &mut first_state).unwrap();
    observe(&second, &*fake, &mut second_state).unwrap();
    fake.change("Shared new fixture");
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let c = f.core.clone();
    let a = fake.clone();
    let b = barrier.clone();
    let first = std::thread::spawn(move || {
        b.wait();
        observe(&c, &*a, &mut first_state).unwrap()
    });
    let c = second.clone();
    let second_thread = std::thread::spawn(move || {
        barrier.wait();
        observe(&c, &*fake, &mut second_state).unwrap()
    });
    assert!(first.join().unwrap());
    assert!(second_thread.join().unwrap());
    assert_eq!(f.count(), 1);
    drop(second);
    f.finish();
}
#[tokio::test]
async fn core_schema_privacy_and_transient_permission_review_omit_durable_text() {
    let f = Fixture::new();
    assert_eq!(
        f.core
            .execute("clipboard.set", json!({"text":"Missing expected"}))
            .await
            .unwrap_err(),
        "INVALID_TOOL_INPUT"
    );
    assert_eq!(f.core.execute("clipboard.transform",json!({"operation":"run_script","expected":{"sequence":1,"fingerprint":"a".repeat(64)}})).await.unwrap_err(),"INVALID_TOOL_INPUT");
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(
        f.core
            .execute("clipboard.get", json!({}))
            .await
            .unwrap_err(),
        "PRIVACY_DENIED"
    );
    let review = json!({"text":"Private proposed ordinary fixture","characters":33,"effect":"Explicit replacement"});
    assert_eq!(
        f.core
            .permission("clipboard.set", &review, Risk::Contextual)
            .await
            .unwrap_err(),
        "APP_MISSING"
    );
    assert!(f.core.permission_previews.lock().unwrap().is_empty());
    assert!(f.core.pending.lock().unwrap().is_empty());
    let durable = f
        .core
        .store
        .query(
            "SELECT input_json_redacted,status FROM permission_requests",
            &[],
        )
        .unwrap();
    assert_eq!(durable[0]["status"], "cancelled");
    assert!(!durable
        .to_string()
        .contains("Private proposed ordinary fixture"));
    let id = storage::id();
    f.core.store.execute("INSERT INTO permission_requests(id,tool_id,risk,input_json_redacted,status,created_at) VALUES(?,'clipboard.set','Contextual',?,'pending',?)",&[json!(id),json!(policy::tool_redact("clipboard.set",&review).to_string()),json!(storage::now())]).unwrap();
    f.core
        .permission_previews
        .lock()
        .unwrap()
        .insert(id.clone(), review.clone());
    assert!(f
        .core
        .permissions_list()
        .unwrap()
        .to_string()
        .contains("Private proposed ordinary fixture"));
    f.core.permission_previews.lock().unwrap().remove(&id);
    assert!(!f
        .core
        .permissions_list()
        .unwrap()
        .to_string()
        .contains("Private proposed ordinary fixture"));
    f.finish();
}
#[test]
fn strict_catalog_and_policy_keep_preview_free_and_writes_reviewed() {
    assert_eq!(
        policy::classify("clipboard.transform", &json!({})),
        Risk::Free
    );
    assert_eq!(
        policy::classify("clipboard.transform", &json!({"apply":true})),
        Risk::Contextual
    );
    assert_eq!(
        policy::classify("clipboard.image", &json!({})),
        Risk::Contextual
    );
    assert_eq!(
        policy::classify("clipboard.history_clear", &json!({})),
        Risk::Mandatory
    );
    let schema = catalog::tool_schema("clipboard.set", "text,expected");
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        schema["properties"]["expected"]["additionalProperties"],
        false
    );
    assert_eq!(schema["properties"]["text"]["maxLength"], 64000);
    assert_eq!(
        catalog::tool_schema("clipboard.transform", "operation,expected")["properties"]
            ["operation"]["enum"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
}
#[test]
fn migration_from_schema_ten_retains_data_and_restorable_backup() {
    let mut f = Fixture::new();
    let path = f.root.join("data/alfredpet.db");
    f.core
        .store
        .set_setting("fixture_marker", &json!("retained"))
        .unwrap();
    f.core.store.0.lock().unwrap().execute_batch("DROP TABLE chat_progress;DROP TABLE drive_jobs;DROP TRIGGER clipboard_setting_insert; DROP TRIGGER clipboard_setting_update; DROP TRIGGER clipboard_setting_delete; DROP TRIGGER clipboard_module_update; DROP TABLE clipboard_history; DROP TABLE clipboard_state; PRAGMA user_version=10;").unwrap();
    let before = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect::<HashSet<_>>();
    drop(f.core);
    f.core = Arc::new(
        Core::new(
            f.root.clone(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap(),
    );
    assert_eq!(f.core.store.setting("fixture_marker"), "retained");
    assert_eq!(
        f.core.store.query("PRAGMA user_version", &[]).unwrap()[0]["user_version"],
        crate::storage::SCHEMA_VERSION
    );
    assert_eq!(f.count(), 0);
    let backups = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| !before.contains(p) && p.to_string_lossy().contains("pre-v11-"))
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    let backup = rusqlite::Connection::open(&backups[0]).unwrap();
    assert_eq!(
        backup
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        10
    );
    assert_eq!(
        backup
            .query_row(
                "SELECT json_value FROM settings WHERE key='fixture_marker'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "\"retained\""
    );
    drop(backup);
    f.finish();
}
