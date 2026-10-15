use super::*;
use crate::{runtime::Core, undo};
use std::sync::Arc;
use windows::Win32::Foundation::{ERROR_DISK_FULL, ERROR_IO_DEVICE};

struct Fixture {
    c: Arc<Core>,
    root_id: String,
}
impl Fixture {
    fn new() -> Self {
        let c = Arc::new(
            Core::new(
                std::env::temp_dir().join(format!("ap-io-{}", storage::id())),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        let root_id = identity(&open(&c.root, false, true).unwrap()).unwrap();
        Self { c, root_id }
    }
    fn finish(self) {
        let root = self.c.root.canonicalize().unwrap();
        assert_eq!(
            root.parent().unwrap(),
            std::env::temp_dir().canonicalize().unwrap()
        );
        uuid::Uuid::parse_str(
            root.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .strip_prefix("ap-io-")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            identity(&open(&root, false, true).unwrap()).unwrap(),
            self.root_id
        );
        drop(self.c);
        std::fs::remove_dir_all(root).unwrap();
    }
}
fn fault(point: Point, bytes: usize, code: u32) -> Control {
    let control = Control::default();
    *control.io_fault.lock().unwrap() = Some(Fault {
        point,
        remaining: bytes,
        os_error: code as i32,
        fired: false,
    });
    control
}
fn journal(c: &Core) -> Value {
    c.store
        .query(
            "SELECT * FROM undo_records ORDER BY created_at DESC,id DESC LIMIT 1",
            &[],
        )
        .unwrap()[0]
        .clone()
}
fn bytes_hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[tokio::test]
async fn real_partial_write_fault_is_observed_under_lease_and_only_explicit_l3_undo_can_restore() {
    for existed in [true, false] {
        let f = Fixture::new();
        let path = f.c.root.join("temp/written.txt");
        let original = vec![b'o'; 130_000];
        let original_id = if existed {
            std::fs::write(&path, &original).unwrap();
            Some(identity(&open(&path, false, false).unwrap()).unwrap())
        } else {
            None
        };
        let control = fault(Point::Write, 4, ERROR_DISK_FULL.0);
        let failure = super::super::write(&f.c, &path, "new content", &control).unwrap_err();
        assert!(failure.contains("FILE_WRITE: os=112") && failure.contains("recovery_undo_id="));
        assert!(control.io_fault.lock().unwrap().as_ref().unwrap().fired);
        let partial = std::fs::read(&path).unwrap();
        assert_eq!(partial, b"new ");
        let row = journal(&f.c);
        let id = row["id"].as_str().unwrap();
        let data = metadata(&row).unwrap();
        assert_eq!(row["state"], "blocked");
        assert_eq!(data["write_failure"]["observed_hash"], bytes_hash(&partial));
        assert_eq!(data["write_failure"]["object_id"], row["object_id"]);
        assert!(undo::history(&f.c).unwrap()[0]["failure_reason"]
            .as_str()
            .unwrap()
            .contains("os=112"));
        let review = undo::review(&f.c, "files.undo", &json!({"id":id,"revalidate":true})).unwrap();
        assert_eq!(
            review["operations_in_undo_order"][0]["write_failure"]["observed_hash"],
            bytes_hash(&partial)
        );
        assert!(f
            .c
            .execute("files.undo", json!({"id":id,"revalidate":true}))
            .await
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), partial);
        std::fs::write(&path, "later external edit").unwrap();
        assert!(undo::execute_request(&f.c, id, true, &Control::default())
            .unwrap_err()
            .starts_with("UNDO_FILE_CHANGED"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "later external edit"
        );
        std::fs::write(&path, &partial).unwrap();
        let preserved = f.c.root.join("temp/preserved-partial.txt");
        std::fs::rename(&path, &preserved).unwrap();
        std::fs::copy(&preserved, &path).unwrap();
        assert_eq!(
            undo::execute_request(&f.c, id, true, &Control::default()).unwrap_err(),
            "UNDO_OBJECT_REPLACED"
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&preserved, &path).unwrap();
        undo::execute_request(&f.c, id, true, &Control::default()).unwrap();
        if let Some(original_id) = original_id {
            assert_eq!(std::fs::read(&path).unwrap(), original);
            assert_eq!(
                identity(&open(&path, false, false).unwrap()).unwrap(),
                original_id
            );
        } else {
            assert!(!path.exists());
        }
        f.finish();
    }
    println!("FILES_PARTIAL_IO_CONTRACT_PASS: actual partial bytes/injected Win32 disk-full code/held observation/native IDs/L3 refusal/later edits and identical replacements preserved/explicit existing+new file undo");
}
#[test]
fn checkpoint_read_write_and_flush_failures_never_truncate_the_original() {
    for (point, budget, code) in [
        (Point::CheckpointRead, 65536, ERROR_IO_DEVICE.0),
        (Point::CheckpointWrite, 65539, ERROR_DISK_FULL.0),
        (Point::CheckpointFlush, 0, ERROR_IO_DEVICE.0),
    ] {
        let f = Fixture::new();
        let path = f.c.root.join("temp/written.txt");
        let original = vec![b'x'; 200_000];
        std::fs::write(&path, &original).unwrap();
        let original_id = identity(&open(&path, false, false).unwrap()).unwrap();
        let control = fault(point, budget, code);
        assert!(super::super::write(&f.c, &path, "after", &control).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let row = journal(&f.c);
        let data = metadata(&row).unwrap();
        assert_eq!(data["write_failure"]["mutation_attempted"], false);
        assert_ne!(data["checkpoint_complete"], true);
        let backup = Path::new(row["checkpoint"].as_str().unwrap());
        let before = std::fs::read(backup).unwrap();
        let result =
            undo::execute_request(&f.c, row["id"].as_str().unwrap(), true, &Control::default())
                .unwrap();
        assert_eq!(result["reconciled"], true);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(
            identity(&open(&path, false, false).unwrap()).unwrap(),
            original_id
        );
        assert_eq!(std::fs::read(backup).unwrap(), before);
        f.finish();
    }
}
#[test]
fn write_and_undo_flush_and_partial_restore_failures_retain_the_verified_backup() {
    for point in [Point::Flush, Point::UndoRestore, Point::UndoFlush] {
        let f = Fixture::new();
        let path = f.c.root.join("temp/written.txt");
        let original = vec![b'o'; 150_000];
        std::fs::write(&path, &original).unwrap();
        let original_id = identity(&open(&path, false, false).unwrap()).unwrap();
        let control = fault(
            point,
            if point == Point::UndoRestore { 7 } else { 0 },
            ERROR_IO_DEVICE.0,
        );
        if point == Point::Flush {
            assert!(super::super::write(&f.c, &path, "after", &control).is_err());
        } else {
            let written = super::super::write(&f.c, &path, "after", &Control::default()).unwrap();
            assert!(
                undo::execute_control(&f.c, written["undo_token"].as_str().unwrap(), &control)
                    .is_err()
            );
        }
        let row = journal(&f.c);
        assert_eq!(row["state"], "blocked");
        assert_eq!(
            std::fs::read(row["checkpoint"].as_str().unwrap()).unwrap(),
            original
        );
        let observed = std::fs::read(&path).unwrap();
        if point == Point::UndoRestore {
            assert_eq!(observed, vec![b'o'; 7]);
        }
        std::fs::write(&path, "unrelated later change").unwrap();
        assert!(undo::execute_request(
            &f.c,
            row["id"].as_str().unwrap(),
            true,
            &Control::default()
        )
        .is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "unrelated later change"
        );
        std::fs::write(&path, &observed).unwrap();
        undo::execute_request(&f.c, row["id"].as_str().unwrap(), true, &Control::default())
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(
            identity(&open(&path, false, false).unwrap()).unwrap(),
            original_id
        );
        f.finish();
    }
}
#[test]
fn copy_io_failures_retain_owned_staging_and_never_publish_a_partial_destination() {
    for point in [Point::CopyRead, Point::CopyWrite, Point::CopyFlush] {
        let f = Fixture::new();
        let source = f.c.root.join("temp/source.txt");
        let target = f.c.root.join("temp/result.txt");
        let original = vec![b'x'; 200_000];
        std::fs::write(&source, &original).unwrap();
        let control = fault(
            point,
            if point == Point::CopyFlush { 0 } else { 65539 },
            ERROR_IO_DEVICE.0,
        );
        assert!(mutate(&f.c, "copy", &source, Some(&target), &control).is_err());
        assert!(!target.exists());
        assert_eq!(std::fs::read(&source).unwrap(), original);
        let row = journal(&f.c);
        let stage = Path::new(row["staging"].as_str().unwrap());
        assert!(stage.exists());
        assert_eq!(
            identity(&open(stage, false, false).unwrap()).unwrap(),
            metadata(&row).unwrap()["stage_id"]
        );
        assert!(!std::fs::read(stage).unwrap().is_empty());
        f.finish();
    }
}
fn full_trigger(c: &Core, when: &str) {
    c.store
        .execute("CREATE TABLE owned_full_fixture(bytes BLOB)", &[])
        .unwrap();
    let pages = c.store.query("PRAGMA page_count", &[]).unwrap()[0]["page_count"]
        .as_u64()
        .unwrap();
    c.store
        .query(&format!("PRAGMA max_page_count={}", pages + 4), &[])
        .unwrap();
    c.store.execute(&format!("CREATE TRIGGER owned_full_trigger BEFORE UPDATE ON undo_records WHEN {when} BEGIN INSERT INTO owned_full_fixture VALUES(zeroblob(1048576)); END"),&[]).unwrap();
}
#[test]
fn real_sqlite_full_after_write_commit_keeps_recovery_and_never_reports_success() {
    let f = Fixture::new();
    let path = f.c.root.join("temp/written.txt");
    std::fs::write(&path, "before").unwrap();
    full_trigger(&f.c, "NEW.state='available'");
    let failure = super::super::write(&f.c, &path, "after", &Control::default()).unwrap_err();
    assert!(failure.contains("database or disk is full") && failure.contains("recovery_undo_id="));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "after");
    let row = journal(&f.c);
    assert_eq!(row["state"], "blocked");
    assert_eq!(
        metadata(&row).unwrap()["write_failure"]["observed_hash"],
        bytes_hash(b"after")
    );
    f.c.store
        .execute("DROP TRIGGER owned_full_trigger", &[])
        .unwrap();
    undo::execute_request(&f.c, row["id"].as_str().unwrap(), true, &Control::default()).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "before");
    f.finish();
}
#[test]
fn real_sqlite_full_during_failure_observation_never_adopts_unrecorded_partial_bytes() {
    let f = Fixture::new();
    let path = f.c.root.join("temp/written.txt");
    std::fs::write(&path, "before").unwrap();
    full_trigger(
        &f.c,
        "json_type(NEW.metadata_json,'$.write_failure') IS NOT NULL",
    );
    let failure = super::super::write(
        &f.c,
        &path,
        "after",
        &fault(Point::Write, 3, ERROR_DISK_FULL.0),
    )
    .unwrap_err();
    assert!(
        failure.contains("FILE_FAILURE_NOT_JOURNALED") && failure.contains("recovery_undo_id=")
    );
    let row = journal(&f.c);
    assert_eq!(row["state"], "blocked");
    assert!(metadata(&row).unwrap()["write_failure"].is_null());
    f.c.store
        .execute("DROP TRIGGER owned_full_trigger", &[])
        .unwrap();
    assert!(
        undo::execute_request(&f.c, row["id"].as_str().unwrap(), true, &Control::default())
            .unwrap_err()
            .starts_with("UNDO_FILE_CHANGED")
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "aft");
    assert_eq!(
        std::fs::read_to_string(row["checkpoint"].as_str().unwrap()).unwrap(),
        "before"
    );
    f.finish();
}
