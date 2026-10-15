use super::*;
use crate::asset_files;
use std::{
    os::windows::fs::MetadataExt,
    path::PathBuf,
    sync::{Arc, Barrier},
};
struct Fixture {
    c: Arc<Core>,
    root: PathBuf,
    identity: String,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ap-tasks-{}", storage::id()));
        std::fs::create_dir(&root).unwrap();
        let identity = asset_files::identity(&asset_files::directory(&root).unwrap()).unwrap();
        let c = Arc::new(
            Core::new(
                root.clone(),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        Self { c, root, identity }
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
            .starts_with("ap-tasks-"));
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
                paths.extend(std::fs::read_dir(&path).unwrap().map(|p| p.unwrap().path()));
            }
        }
        drop(self.c);
        std::fs::remove_dir_all(self.root).unwrap();
    }
}
fn at(raw: &str) -> DateTime<Utc> {
    instant(raw).unwrap()
}
fn create(c: &Core, input: Value) -> Value {
    dispatch_at(c, "create", &input, at("2026-01-01T00:00:00Z")).unwrap()
}
fn complete(c: &Core, task: &Value, when: &str) -> Result<Value, String> {
    dispatch_at(
        c,
        "complete",
        &json!({"id":task["id"],"expected_revision":task["revision"]}),
        at(when),
    )
}
#[test]
fn edit_links_partial_update_and_restart_preserve_the_task() {
    let mut f = Fixture::new();
    let project = storage::id();
    let conversation = storage::id();
    f.c.store
        .execute(
            "INSERT INTO projects(id,name,root_path,created_at) VALUES(?,'Own project',?,?)",
            &[json!(project), json!(f.root), json!(storage::now())],
        )
        .unwrap();
    f.c.store.execute("INSERT INTO conversations(id,title,created_at,updated_at) VALUES(?,'Own conversation',?,?)",&[json!(conversation),json!(storage::now()),json!(storage::now())]).unwrap();
    let task = create(
        &f.c,
        json!({"title":"  Review fixture  ","description":"Owned description","due_at":"2026-02-02T11:30:00+02:00","project_id":project,"conversation_id":conversation}),
    );
    assert_eq!(task["title"], "Review fixture");
    assert_eq!(task["due_at"], "2026-02-02T09:30:00.000Z");
    assert_eq!(task["project_name"], "Own project");
    assert_eq!(task["conversation_title"], "Own conversation");
    let edited = dispatch(
        &f.c,
        "update",
        &json!({"id":task["id"],"expected_revision":0,"title":"Edited"}),
    )
    .unwrap();
    assert_eq!(edited["description"], task["description"]);
    assert_eq!(edited["due_at"], task["due_at"]);
    assert_eq!(edited["revision"], 1);
    let unlinked = dispatch(
        &f.c,
        "update",
        &json!({"id":task["id"],"expected_revision":1,"project_id":null,"conversation_id":null}),
    )
    .unwrap();
    assert!(unlinked["project_id"].is_null());
    assert!(unlinked["conversation_id"].is_null());
    drop(f.c);
    f.c = Arc::new(
        Core::new(
            f.root.clone(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap(),
    );
    assert_eq!(dispatch(&f.c, "list", &json!({})).unwrap()[0], unlinked);
    f.finish();
}
#[test]
fn calendar_anchor_handles_leap_months_intervals_and_missed_occurrences() {
    let rec = Recurrence {
        version: 1,
        frequency: Frequency::Monthly,
        interval: 1,
        anchor: "2024-01-31T22:30:00.000Z".into(),
    };
    assert_eq!(
        stamp(next(&rec, at("2024-01-31T22:30:00Z")).unwrap()),
        "2024-02-29T22:30:00.000Z"
    );
    assert_eq!(
        stamp(next(&rec, at("2024-02-29T22:30:00Z")).unwrap()),
        "2024-03-31T22:30:00.000Z"
    );
    assert_eq!(
        stamp(next(&rec, at("2026-03-30T22:30:00Z")).unwrap()),
        "2026-03-31T22:30:00.000Z"
    );
    let daily = Recurrence {
        frequency: Frequency::Daily,
        interval: 2,
        anchor: "2026-03-28T09:00:00Z".into(),
        ..rec.clone()
    };
    assert_eq!(
        stamp(next(&daily, at("2026-03-29T09:00:00Z")).unwrap()),
        "2026-03-30T09:00:00.000Z"
    );
    let weekly = Recurrence {
        frequency: Frequency::Weekly,
        interval: 2,
        ..daily
    };
    assert_eq!(
        stamp(next(&weekly, at("2026-04-11T09:00:00Z")).unwrap()),
        "2026-04-25T09:00:00.000Z"
    );
    let range = Recurrence {
        anchor: "9999-12-31T00:00:00Z".into(),
        ..rec
    };
    assert_eq!(
        next(&range, at("9999-12-31T00:00:00Z")).unwrap_err(),
        "TASK_RECURRENCE_RANGE"
    );
}
#[test]
fn complete_records_one_run_coalesces_backlog_and_preserves_the_original_anchor() {
    let f = Fixture::new();
    let task = create(
        &f.c,
        json!({"title":"Monthly fixture","due_at":"2026-01-31T10:00:00Z","recurrence":{"frequency":"monthly"}}),
    );
    let next_task = complete(&f.c, &task, "2026-02-28T10:00:00Z").unwrap();
    assert_eq!(next_task["status"], "queued");
    assert_eq!(next_task["due_at"], "2026-03-31T10:00:00.000Z");
    assert!(complete(&f.c, &task, "2026-02-28T10:00:00Z")
        .unwrap_err()
        .starts_with("TASK_CHANGED"));
    let edited = dispatch(&f.c,"update",&json!({"id":task["id"],"expected_revision":1,"title":"Renamed","due_at":next_task["due_at"],"recurrence":{"frequency":"monthly","interval":1}})).unwrap();
    assert_eq!(edited["recurrence"]["anchor"], "2026-01-31T10:00:00.000Z");
    let resumed = complete(&f.c, &edited, "2026-10-03T11:00:00Z").unwrap();
    assert_eq!(resumed["due_at"], "2026-10-31T10:00:00.000Z");
    let history = dispatch(&f.c, "history", &json!({"id":task["id"]})).unwrap();
    assert_eq!(history.as_array().unwrap().len(), 2);
    assert_eq!(history[0]["scheduled_for"], next_task["due_at"]);
    let single = create(&f.c, json!({"title":"One off"}));
    let done = complete(&f.c, &single, "2026-10-03T11:00:00Z").unwrap();
    assert_eq!(done["status"], "succeeded");
    assert_eq!(
        complete(&f.c, &done, "2026-10-03T11:00:00Z").unwrap_err(),
        "TASK_ALREADY_COMPLETED"
    );
    let reopened = dispatch(
        &f.c,
        "update",
        &json!({"id":done["id"],"expected_revision":done["revision"],"reopen":true}),
    )
    .unwrap();
    let done = complete(&f.c, &reopened, "2026-10-03T12:00:00Z").unwrap();
    assert_eq!(
        dispatch(
            &f.c,
            "delete",
            &json!({"id":done["id"],"expected_revision":done["revision"]})
        )
        .unwrap()["history_retained"],
        true
    );
    assert_eq!(
        dispatch(&f.c, "history", &json!({"id":done["id"]}))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    f.finish();
}
#[test]
fn reminder_claims_respect_privacy_dnd_module_and_actual_offset_time() {
    let mut f = Fixture::new();
    let task = create(
        &f.c,
        json!({"title":"Due fixture","due_at":"2026-10-03T13:00:00+02:00"}),
    );
    for key in ["privacy", "dnd"] {
        f.c.store.set_setting(key, &json!(true)).unwrap();
        assert!(claim_due(&f.c, at("2026-10-03T11:00:00Z"))
            .unwrap()
            .is_empty());
        assert_eq!(
            dispatch(&f.c, "list", &json!({})).unwrap()[0]["revision"],
            0
        );
        f.c.store.set_setting(key, &json!(false)).unwrap();
    }
    f.c.store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='tasks'",
            &[],
        )
        .unwrap();
    assert!(claim_due(&f.c, at("2026-10-03T11:00:00Z"))
        .unwrap()
        .is_empty());
    f.c.store
        .execute(
            "UPDATE module_configs SET enabled=1 WHERE module_id='tasks'",
            &[],
        )
        .unwrap();
    // Legacy ISO timestamps may carry an offset. Lexical ordering cannot decide due time.
    f.c.store
        .execute(
            "UPDATE tasks SET due_at='2026-10-03T13:00:00+02:00' WHERE id=?",
            &[task["id"].clone()],
        )
        .unwrap();
    assert!(claim_due(&f.c, at("2026-10-03T10:59:59Z"))
        .unwrap()
        .is_empty());
    let due = claim_due(&f.c, at("2026-10-03T11:00:00Z")).unwrap();
    assert_eq!(due.len(), 1);
    assert!(claim_due(&f.c, at("2026-10-03T11:01:00Z"))
        .unwrap()
        .is_empty());
    drop(f.c);
    f.c = Arc::new(
        Core::new(
            f.root.clone(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap(),
    );
    assert!(claim_due(&f.c, at("2026-10-04T11:00:00Z"))
        .unwrap()
        .is_empty());
    assert_eq!(
        dispatch(&f.c, "list", &json!({})).unwrap()[0]["status"],
        "waiting_user"
    );
    f.finish();
}
#[test]
fn independent_actors_claim_once_and_stale_edits_or_completion_cannot_overwrite() {
    let f = Fixture::new();
    let task = create(
        &f.c,
        json!({"title":"Actor fixture","due_at":"2026-10-03T10:00:00Z"}),
    );
    let other = Arc::new(
        Core::new(
            f.root.clone(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap(),
    );
    let barrier = Arc::new(Barrier::new(2));
    let jobs: Vec<_> = [f.c.clone(), other.clone()]
        .into_iter()
        .map(|core| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                claim_due(&core, at("2026-10-03T11:00:00Z")).unwrap().len()
            })
        })
        .collect();
    assert_eq!(
        jobs.into_iter().map(|j| j.join().unwrap()).sum::<usize>(),
        1
    );
    assert!(dispatch(
        &other,
        "update",
        &json!({"id":task["id"],"expected_revision":0,"title":"Stale edit"})
    )
    .unwrap_err()
    .starts_with("TASK_CHANGED"));
    let current = dispatch(&f.c, "list", &json!({})).unwrap()[0].clone();
    let barrier = Arc::new(Barrier::new(2));
    let jobs: Vec<_> = [f.c.clone(), other.clone()]
        .into_iter()
        .map(|core| {
            let barrier = barrier.clone();
            let task = current.clone();
            std::thread::spawn(move || {
                barrier.wait();
                complete(&core, &task, "2026-10-03T11:00:00Z")
            })
        })
        .collect();
    let results: Vec<_> = jobs.into_iter().map(|j| j.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        dispatch(&f.c, "history", &json!({"id":task["id"]}))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    drop(other);
    f.finish();
}
#[test]
fn schema_dates_links_and_disabled_module_reject_without_partial_mutation() {
    let f = Fixture::new();
    for input in [
        json!({"title":" "}),
        json!({"title":"Bad due","due_at":"2026-10-03"}),
        json!({"title":"Bad due","due_at":"2026-02-30T11:00:00Z"}),
        json!({"title":"Rec","recurrence":{"frequency":"daily"}}),
        json!({"title":"Link","project_id":"missing"}),
        json!({"title":"Link","conversation_id":"missing"}),
        json!({"title":"Bad schema","recurrence":{"frequency":"daily","interval":0}}),
        json!({"title":"Bad schema","untrusted":true}),
        json!({"title":"Token","description":"password=owned-fixture-value"}),
    ] {
        assert!(dispatch(&f.c, "create", &input).is_err(), "{input}");
    }
    assert!(dispatch(&f.c, "list", &json!({}))
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    let task = create(
        &f.c,
        json!({"title":"Retain fixture","due_at":"2026-10-03T11:00:00Z","recurrence":{"frequency":"daily"}}),
    );
    for input in [
        json!({"id":task["id"],"title":"No revision"}),
        json!({"id":task["id"],"expected_revision":0,"title":"Do not apply","due_at":null}),
        json!({"id":task["id"],"expected_revision":0,"project_id":"missing"}),
    ] {
        assert!(dispatch(&f.c, "update", &input).is_err());
    }
    assert_eq!(dispatch(&f.c, "list", &json!({})).unwrap()[0], task);
    f.c.store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='tasks'",
            &[],
        )
        .unwrap();
    assert_eq!(
        dispatch(
            &f.c,
            "complete",
            &json!({"id":task["id"],"expected_revision":0})
        )
        .unwrap_err(),
        "MODULE_DISABLED: tasks"
    );
    f.finish();
}
#[test]
fn migration_from_real_schema_nine_retains_tasks_runs_and_restorable_backup() {
    let mut f = Fixture::new();
    let task = create(
        &f.c,
        json!({"title":"Legacy task","due_at":"2026-10-03T11:00:00Z"}),
    );
    complete(&f.c, &task, "2026-10-03T11:00:00Z").unwrap();
    f.c.store.0.lock().unwrap().execute_batch("DROP TABLE chat_progress;DROP TABLE drive_jobs;DROP TRIGGER clipboard_setting_insert;DROP TRIGGER clipboard_setting_update;DROP TRIGGER clipboard_setting_delete;DROP TRIGGER clipboard_module_update;DROP TABLE clipboard_history;DROP TABLE clipboard_state;ALTER TABLE tasks DROP COLUMN description;ALTER TABLE tasks DROP COLUMN revision;ALTER TABLE task_runs DROP COLUMN scheduled_for;ALTER TABLE task_runs DROP COLUMN error;DROP INDEX tasks_due;DROP INDEX task_runs_history;PRAGMA user_version=9;").unwrap();
    let path = f.root.join("data/alfredpet.db");
    // A fresh profile already has a schema-nine pre-v10 backup from its initial
    // migrations. Identify the newly produced backup rather than the first
    // schema-nine file in nondeterministic directory order.
    let before = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<std::collections::HashSet<_>>();
    drop(f.c);
    f.c = Arc::new(
        Core::new(
            f.root.clone(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap(),
    );
    assert_eq!(
        f.c.store.query("PRAGMA user_version", &[]).unwrap()[0]["user_version"],
        crate::storage::SCHEMA_VERSION
    );
    let restored = dispatch(&f.c, "list", &json!({})).unwrap();
    assert_eq!(restored[0]["id"], task["id"]);
    assert_eq!(restored[0]["description"], "");
    assert_eq!(restored[0]["revision"], 0);
    assert_eq!(
        dispatch(&f.c, "history", &json!({"id":task["id"]}))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let backups = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains("pre-v10-")
                && p.extension().is_some_and(|ext| ext == "db")
                && !before.contains(p)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        backups.len(),
        1,
        "one new backup for this schema-nine migration"
    );
    let legacy = &backups[0];
    let db =
        Connection::open_with_flags(legacy, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        9
    );
    assert_eq!(
        db.query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM task_runs WHERE task_id=?",
            [task["id"].as_str().unwrap()],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT title FROM tasks WHERE id=?",
            [task["id"].as_str().unwrap()],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "Legacy task"
    );
    drop(db);
    assert_eq!(
        f.c.store.query("PRAGMA quick_check(1)", &[]).unwrap()[0]["quick_check"],
        "ok"
    );
    f.finish();
}
#[test]
fn run_insert_failure_rolls_back_completion_and_invalid_legacy_metadata_stays_visible() {
    let f = Fixture::new();
    let task = create(
        &f.c,
        json!({"title":"Atomic task","due_at":"2026-10-03T11:00:00Z","recurrence":{"frequency":"daily"}}),
    );
    f.c.store.0.lock().unwrap().execute_batch("CREATE TRIGGER owned_task_failure BEFORE INSERT ON task_runs BEGIN SELECT RAISE(ABORT,'OWNED_TASK_RUN_FAILURE'); END;").unwrap();
    assert!(complete(&f.c, &task, "2026-10-03T11:00:00Z")
        .unwrap_err()
        .contains("OWNED_TASK_RUN_FAILURE"));
    assert_eq!(dispatch(&f.c, "list", &json!({})).unwrap()[0], task);
    assert!(dispatch(&f.c, "history", &json!({"id":task["id"]}))
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    f.c.store.0.lock().unwrap().execute_batch("DROP TRIGGER owned_task_failure;UPDATE tasks SET recurrence_json='invalid' WHERE title='Atomic task';").unwrap();
    assert_eq!(
        dispatch(&f.c, "list", &json!({})).unwrap()[0]["recurrence_error"],
        "TASK_RECURRENCE_INVALID"
    );
    assert!(claim_due(&f.c, at("2026-10-03T11:00:00Z"))
        .unwrap()
        .is_empty());
    assert!(claim_due(&f.c, at("2026-10-04T11:00:00Z"))
        .unwrap()
        .is_empty());
    let blocked = dispatch(&f.c, "list", &json!({})).unwrap()[0].clone();
    assert_eq!(blocked["status"], "blocked");
    let history = dispatch(&f.c, "history", &json!({"id":task["id"]})).unwrap();
    assert_eq!(history.as_array().unwrap().len(), 1);
    assert_eq!(history[0]["error"], "TASK_RECURRENCE_INVALID");
    let fixed = dispatch(
        &f.c,
        "update",
        &json!({"id":task["id"],"expected_revision":blocked["revision"],"recurrence":null}),
    )
    .unwrap();
    assert!(fixed["recurrence_error"].is_null());
    assert_eq!(fixed["status"], "queued");
    f.finish();
}
#[test]
fn task_and_history_pages_have_bounded_stable_order_and_link_filters() {
    let f = Fixture::new();
    let project = storage::id();
    f.c.store
        .execute(
            "INSERT INTO projects(id,name,root_path,created_at) VALUES(?,'Page project',?,?)",
            &[json!(project), json!(f.root), json!(storage::now())],
        )
        .unwrap();
    for i in 0..53 {
        create(
            &f.c,
            json!({"title":format!("Page {i}"),"project_id":if i%2==0 {Some(&project)} else {None}}),
        );
    }
    let first = dispatch(&f.c, "list", &json!({})).unwrap();
    let second = dispatch(&f.c, "list", &json!({"offset":50})).unwrap();
    assert_eq!(first.as_array().unwrap().len(), 50);
    assert_eq!(second.as_array().unwrap().len(), 3);
    assert!(!first.as_array().unwrap().iter().any(|a| second
        .as_array()
        .unwrap()
        .iter()
        .any(|b| a["id"] == b["id"])));
    let filtered = dispatch(&f.c, "list", &json!({"project_id":project})).unwrap();
    assert_eq!(filtered.as_array().unwrap().len(), 27);
    assert!(filtered
        .as_array()
        .unwrap()
        .iter()
        .all(|t| t["project_id"] == project));
    assert!(dispatch(&f.c, "list", &json!({"limit":1000000})).is_err());
    f.finish();
}
#[tokio::test]
async fn public_catalog_requires_revision_and_keeps_writes_in_permission_and_audit_flow() {
    let f = Fixture::new();
    assert_eq!(
        f.c.execute("tasks.complete", json!({"id":"missing"}))
            .await
            .unwrap_err(),
        "INVALID_TOOL_INPUT"
    );
    assert_eq!(
        policy::classify("tasks.complete", &json!({})),
        policy::Risk::Contextual
    );
    assert_eq!(
        policy::classify("tasks.delete", &json!({})),
        policy::Risk::Mandatory
    );
    let input = json!({"title":"Catalog task","description":"Own fixture"});
    f.c.grants
        .lock()
        .unwrap()
        .insert(format!("tasks.create:{input}"), policy::Risk::Contextual);
    let task = f.c.execute("tasks.create", input).await.unwrap();
    let events =
        f.c.store
            .query(
                "SELECT tool_id,status FROM activity_events WHERE tool_id='tasks.create'",
                &[],
            )
            .unwrap();
    assert_eq!(events.as_array().unwrap().len(), 1);
    assert_eq!(events[0]["status"], "succeeded");
    assert_eq!(
        f.c.execute("tasks.list", json!({})).await.unwrap()[0]["id"],
        task["id"]
    );
    f.c.store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='tasks'",
            &[],
        )
        .unwrap();
    assert_eq!(
        f.c.execute("tasks.list", json!({})).await.unwrap_err(),
        "TOOL_DISABLED_OR_UNKNOWN"
    );
    f.finish();
}
