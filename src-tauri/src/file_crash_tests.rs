//! Kill a separate native backend test process at persisted filesystem boundaries.
//! The hook and child entrypoint do not exist in production builds.
use super::*;
use std::{
    process::{Child, Command, Stdio},
    sync::Arc,
};

fn owned_base(raw: &Path) -> PathBuf {
    let base = raw.canonicalize().unwrap();
    assert_eq!(
        base.parent().unwrap(),
        std::env::temp_dir().canonicalize().unwrap()
    );
    let name = base.file_name().unwrap().to_str().unwrap();
    uuid::Uuid::parse_str(name.strip_prefix("ap-crash-").unwrap()).unwrap();
    base
}

pub(super) fn at(c: &Core, point: &str) {
    if std::env::var("ALFREDPET_TEST_CRASH_POINT").ok().as_deref() != Some(point) {
        return;
    }
    let base = owned_base(Path::new(
        &std::env::var_os("ALFREDPET_TEST_CRASH_BASE").expect("owned crash fixture required"),
    ));
    assert_eq!(c.root.canonicalize().unwrap(), base.join("profile"));
    let marker = c.root.join("temp/crash-ready.json");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marker)
        .unwrap();
    write!(file, "{}", json!({"point":point,"pid":std::process::id()})).unwrap();
    file.sync_all().unwrap();
    // Parent verifies this marker and then terminates this exact child handle.
    if std::env::var("ALFREDPET_TEST_BARRIER").ok().as_deref() == Some("1") {
        let deadline = Instant::now() + Duration::from_secs(30);
        let release = c.root.join("temp/crash-release.json");
        while Instant::now() < deadline {
            if let Ok(bytes) = std::fs::read(&release) {
                if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                    if value["pid"] == std::process::id() {
                        return;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("owned native barrier was not released");
    }
    loop {
        std::thread::park();
    }
}

#[test]
#[ignore = "internal subprocess fixture; invoked only by the process-kill contract"]
fn crash_child_fixture() {
    let base = owned_base(Path::new(
        &std::env::var_os("ALFREDPET_TEST_CRASH_BASE").expect("owned crash fixture required"),
    ));
    let spec: Value =
        serde_json::from_slice(&std::fs::read(base.join("fixture.json")).unwrap()).unwrap();
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap();
    let volume = spec["volume_root"].as_str().map(|raw| {
        let path = Path::new(raw).canonicalize().unwrap();
        assert_eq!(
            path.parent().unwrap(),
            workspace.join(".work").canonicalize().unwrap()
        );
        let id = base
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .strip_prefix("ap-crash-")
            .unwrap();
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            format!("crash-volume-{id}")
        );
        path
    });
    let profile = PathBuf::from(spec["profile"].as_str().unwrap());
    assert_eq!(profile.canonicalize().unwrap(), base.join("profile"));
    // Keep the calling application's exact configured representation. Cleanup
    // deliberately invalidates reviews when the saved root configuration changes.
    let core = Arc::new(Core::new(profile, workspace).unwrap());
    let input = spec["input"].clone();
    for field in ["path", "destination"] {
        if let Some(raw) = input[field].as_str() {
            let path = core.path(raw, true).unwrap();
            assert!(
                path.starts_with(&base)
                    || volume.as_ref().is_some_and(|root| path.starts_with(root))
            );
        }
    }
    let tool = spec["tool"].as_str().unwrap();
    assert!([
        "files.copy",
        "files.move",
        "files.delete",
        "files.undo",
        "files.cleanup",
        "files.extract",
        "files.write"
    ]
    .contains(&tool));
    let result = if spec["io_fault"].is_object() {
        assert_eq!(tool, "files.write");
        let control = Control::default();
        *control.io_fault.lock().unwrap() = Some(io_control::Fault {
            point: io_control::Point::Write,
            remaining: spec["io_fault"]["after_bytes"].as_u64().unwrap() as usize,
            os_error: windows::Win32::Foundation::ERROR_DISK_FULL.0 as i32,
            fired: false,
        });
        write(
            &core,
            &core.path(input["path"].as_str().unwrap(), true).unwrap(),
            input["text"].as_str().unwrap(),
            &control,
        )
    } else {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(crate::modules::dispatch(&core, tool, &input))
    };
    if spec["barrier"] == true {
        let value = match result {
            Ok(value) => json!({"ok":true,"value":value}),
            Err(error) => json!({"ok":false,"error":error}),
        };
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(base.join("result.json"))
            .unwrap();
        write!(out, "{value}").unwrap();
        out.sync_all().unwrap();
        return;
    }
    result.unwrap();
    panic!("crash point was not reached");
}

struct Fixture {
    base: PathBuf,
    base_id: String,
    profile: PathBuf,
    source: PathBuf,
    target: PathBuf,
    volume: Option<(PathBuf, String)>,
    original: Value,
}
impl Fixture {
    fn new(directory: bool, cross_volume: bool) -> Self {
        let id = storage::id();
        let base = std::env::temp_dir().join(format!("ap-crash-{id}"));
        std::fs::create_dir(&base).unwrap();
        let base_id = identity(&open(&base, false, true).unwrap()).unwrap();
        let profile = base.join("profile");
        let core = Core::new(
            profile.clone(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap();
        let volume = if cross_volume {
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../.work")
                .canonicalize()
                .unwrap()
                .join(format!("crash-volume-{id}"));
            std::fs::create_dir(&path).unwrap();
            std::fs::create_dir(path.join("recovery")).unwrap();
            let native = open(&path, false, true).unwrap();
            assert_ne!(
                super::volume(&native).unwrap(),
                super::volume(&open(&profile, false, true).unwrap()).unwrap()
            );
            Some((path, identity(&native).unwrap()))
        } else {
            None
        };
        let source = volume
            .as_ref()
            .map_or_else(|| base.clone(), |(root, _)| root.clone())
            .join(if directory { "source" } else { "source.txt" });
        if directory {
            std::fs::create_dir_all(source.join("nested/empty")).unwrap();
            std::fs::write(source.join("nested/ñ.txt"), "native crash fixture").unwrap();
            std::fs::write(source.join("zero.bin"), []).unwrap();
            let path = source.join("nested/ñ.txt");
            let mut attributes = std::fs::metadata(&path).unwrap().permissions();
            attributes.set_readonly(true);
            std::fs::set_permissions(path, attributes).unwrap();
        } else {
            std::fs::write(&source, "native crash fixture").unwrap();
        }
        let mut grants = vec![json!(source)];
        if let Some((root, _)) = &volume {
            grants.push(json!(root.join("recovery")));
        }
        core.store
            .set_setting("allowed_roots", &json!(grants))
            .unwrap();
        let original = observed(&core, &source);
        // A legacy alias is canonicalized before its namespace disappears.
        if !cross_volume && directory {
            core.store
                .set_setting("allowed_roots", &json!([source.with_file_name("SOURCE.")]))
                .unwrap();
        }
        let target = core.root.join("temp/result");
        drop(core);
        Self {
            base,
            base_id,
            profile,
            source,
            target,
            volume,
            original,
        }
    }
    fn core(&self) -> Arc<Core> {
        Arc::new(
            Core::new(
                self.profile.clone(),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        )
    }
    fn kill(&self, tool: &str, input: Value, point: &str) -> u32 {
        self.kill_fault(tool, input, point, Value::Null)
    }
    fn kill_fault(&self, tool: &str, input: Value, point: &str, fault: Value) -> u32 {
        let spec = json!({"tool":tool,"input":input,"profile":self.profile,"volume_root":self.volume.as_ref().map(|(root,_)|root),"io_fault":fault});
        std::fs::write(self.base.join("fixture.json"), spec.to_string()).unwrap();
        use std::os::windows::process::CommandExt;
        let log = File::create(self.base.join("child.log")).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "file_tree::crash_tests::crash_child_fixture",
                "--ignored",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("ALFREDPET_TEST_CRASH_BASE", &self.base)
            .env("ALFREDPET_TEST_CRASH_POINT", point)
            .creation_flags(0x08000000)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let mut guard = ChildGuard(Some(child));
        let pid = guard.0.as_ref().unwrap().id();
        let marker = self.profile.join("temp/crash-ready.json");
        let deadline = Instant::now() + Duration::from_secs(30);
        let ready = loop {
            assert!(
                guard.0.as_mut().unwrap().try_wait().unwrap().is_none(),
                "child exited before {point}: {}",
                std::fs::read_to_string(self.base.join("child.log")).unwrap()
            );
            if let Ok(raw) = std::fs::read(&marker) {
                if let Ok(value) = serde_json::from_slice::<Value>(&raw) {
                    break value;
                }
            }
            assert!(Instant::now() < deadline, "child did not reach {point}");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(ready["pid"], pid);
        assert_eq!(ready["point"], point);
        let child = guard.0.as_mut().unwrap();
        child.kill().unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success());
        guard.0.take();
        println!("native process killed at {point}; pid={pid}");
        pid
    }
    fn barrier(
        &self,
        tool: &str,
        input: Value,
        point: &str,
        fault: Value,
        interfere: impl FnOnce(&Arc<Core>),
    ) -> Value {
        // Open the observer before the child creates a prepared journal: startup
        // recovery must not modify a still-live writer's journal in this test.
        let observer = self.core();
        let spec = json!({"tool":tool,"input":input,"profile":self.profile,"volume_root":self.volume.as_ref().map(|(root,_)|root),"io_fault":fault,"barrier":true});
        std::fs::write(self.base.join("fixture.json"), spec.to_string()).unwrap();
        use std::os::windows::process::CommandExt;
        let log = File::create(self.base.join("child.log")).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "file_tree::crash_tests::crash_child_fixture",
                "--ignored",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("ALFREDPET_TEST_CRASH_BASE", &self.base)
            .env("ALFREDPET_TEST_CRASH_POINT", point)
            .env("ALFREDPET_TEST_BARRIER", "1")
            .creation_flags(0x08000000)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let mut guard = ChildGuard(Some(child));
        let pid = guard.0.as_ref().unwrap().id();
        assert_ne!(pid, std::process::id());
        let deadline = Instant::now() + Duration::from_secs(30);
        let marker = self.profile.join("temp/crash-ready.json");
        let ready = loop {
            assert!(
                guard.0.as_mut().unwrap().try_wait().unwrap().is_none(),
                "child exited before {point}: {}",
                std::fs::read_to_string(self.base.join("child.log")).unwrap()
            );
            if let Ok(bytes) = std::fs::read(&marker) {
                if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                    break value;
                }
            }
            assert!(Instant::now() < deadline, "barrier was not reached");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(ready["pid"], pid);
        assert_eq!(ready["point"], point);
        interfere(&observer);
        let mut release = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.profile.join("temp/crash-release.json"))
            .unwrap();
        write!(release, "{}", json!({"pid":pid})).unwrap();
        release.sync_all().unwrap();
        drop(release);
        let status = loop {
            if let Some(status) = guard.0.as_mut().unwrap().try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "child did not finish after release"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(
            status.success(),
            "native barrier child failed: {}",
            std::fs::read_to_string(self.base.join("child.log")).unwrap()
        );
        guard.0.take();
        drop(observer);
        serde_json::from_slice(&std::fs::read(self.base.join("result.json")).unwrap()).unwrap()
    }
    fn finish(self) {
        assert_eq!(
            identity(&open(&self.base, false, true).unwrap()).unwrap(),
            self.base_id
        );
        let base = owned_base(&self.base);
        clear_owned_readonly(&base);
        std::fs::remove_dir_all(base).unwrap();
        if let Some((root, id)) = self.volume {
            let root = root.canonicalize().unwrap();
            assert_eq!(
                root.parent().unwrap(),
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../.work")
                    .canonicalize()
                    .unwrap()
            );
            uuid::Uuid::parse_str(
                root.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .strip_prefix("crash-volume-")
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(identity(&open(&root, false, true).unwrap()).unwrap(), id);
            clear_owned_readonly(&root);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
struct ChildGuard(Option<Child>);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
// This entire module is Windows-only. Clearing the readonly attribute is limited
// to validated, test-owned synthetic files; Unix permission modes are irrelevant.
#[allow(clippy::permissions_set_readonly_false)]
fn clear_owned_readonly(root: &Path) {
    let mut stack = vec![root.to_path_buf()];
    let mut count = 0;
    while let Some(path) = stack.pop() {
        count += 1;
        assert!(count < 1000);
        let meta = std::fs::symlink_metadata(&path).unwrap();
        use std::os::windows::fs::MetadataExt;
        assert_eq!(meta.file_attributes() & 0x400, 0);
        if meta.is_dir() {
            stack.extend(
                std::fs::read_dir(&path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path()),
            );
        }
        if meta.permissions().readonly() {
            let mut permissions = meta.permissions();
            permissions.set_readonly(false);
            std::fs::set_permissions(path, permissions).unwrap();
        }
    }
}
fn observed(c: &Core, path: &Path) -> Value {
    let tree = Tree::capture(c, path, false, &Control::default()).unwrap();
    json!({"id":tree.identity,"digest":tree.digest,"nodes":tree.ownership().unwrap()})
}
fn last_journal(c: &Core) -> Value {
    c.store
        .query(
            "SELECT * FROM undo_records ORDER BY created_at DESC,id DESC LIMIT 1",
            &[],
        )
        .unwrap()[0]
        .clone()
}

fn revoke_source(f: &Fixture) {
    // Change the persisted policy before opening Core, so startup reconciliation
    // observes the revoked grant rather than a previously recovered journal.
    let store = storage::Store::open(&f.profile.join("data/alfredpet.db")).unwrap();
    let roots = f
        .volume
        .as_ref()
        .map_or_else(Vec::new, |(root, _)| vec![root.join("recovery")]);
    store.set_setting("allowed_roots", &json!(roots)).unwrap();
}

fn cleanup_fixture(f: &Fixture) -> (Value, Value) {
    let c = f.core();
    let copied = mutate(&c, "copy", &f.source, Some(&f.target), &Control::default()).unwrap();
    undo::execute(&c, copied["undo_token"].as_str().unwrap()).unwrap();
    let plan = cleanup(
        &c,
        &json!({"action":"preview","ids":[copied["undo_token"]]}),
        &Control::default(),
    )
    .unwrap();
    let data: Value = serde_json::from_str(
        c.store
            .query(
                "SELECT snapshot_json FROM file_cleanup_plans WHERE id=?",
                &[plan["plan_id"].clone()],
            )
            .unwrap()[0]["snapshot_json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    (plan, data)
}

#[test]
fn real_process_kill_preserves_revoked_conflicted_and_foreign_objects() {
    // Windows may recycle a PID after wait(); the owned child handle and marker
    // authenticate each independent termination. PID uniqueness is not required.
    let mut terminations = Vec::new();
    for directory in [false, true] {
        for revoked in [false, true] {
            let f = Fixture::new(directory, false);
            terminations.push(f.kill(
                "files.move",
                json!({"path":f.source,"destination":f.target}),
                "tree_namespace_published",
            ));
            if revoked {
                revoke_source(&f);
            } else {
                std::fs::write(&f.source, "foreign occupant").unwrap();
            }
            let c = f.core();
            let row = last_journal(&c);
            assert_eq!(row["state"], "blocked");
            assert_eq!(
                undo::execute(&c, row["id"].as_str().unwrap()).unwrap_err(),
                "UNDO_NOT_AVAILABLE"
            );
            assert_eq!(observed(&c, &f.target), f.original);
            if revoked {
                assert!(!f.source.exists());
                assert!(c
                    .path(f.source.to_str().unwrap(), true)
                    .unwrap_err()
                    .starts_with("PATH_SCOPE"));
            } else {
                assert_eq!(
                    std::fs::read_to_string(&f.source).unwrap(),
                    "foreign occupant"
                );
            }
            drop(c);
            // Blocked records remain blocked across another restart. No blind
            // journal reset or automatic mutation is part of this contract.
            let c = f.core();
            assert_eq!(last_journal(&c)["state"], "blocked");
            assert_eq!(observed(&c, &f.target), f.original);
            let conflict =
                undo::execute_request(&c, row["id"].as_str().unwrap(), true, &Control::default())
                    .unwrap_err();
            assert!(
                conflict.starts_with(if revoked {
                    "PATH_SCOPE"
                } else {
                    "UNDO_DESTINATION_EXISTS"
                }),
                "{conflict}"
            );
            let foreign = f.profile.join("temp/foreign-preserved.txt");
            if revoked {
                c.store
                    .set_setting("allowed_roots", &json!([f.source]))
                    .unwrap();
            } else {
                std::fs::rename(&f.source, &foreign).unwrap();
            }
            undo::execute_request(&c, row["id"].as_str().unwrap(), true, &Control::default())
                .unwrap();
            assert_eq!(observed(&c, &f.source), f.original);
            assert!(!f.target.exists());
            if !revoked {
                assert_eq!(
                    std::fs::read_to_string(&foreign).unwrap(),
                    "foreign occupant"
                );
            }
            drop(c);
            f.finish();
        }
        for revoked in [false, true] {
            let f = Fixture::new(directory, true);
            terminations.push(f.kill(
                "files.move",
                json!({"path":f.source,"destination":f.target}),
                "cross_original_retained",
            ));
            if revoked {
                revoke_source(&f);
            } else {
                std::fs::write(&f.source, "foreign cross-volume occupant").unwrap();
            }
            let c = f.core();
            let row = last_journal(&c);
            assert_eq!(row["state"], "blocked");
            assert_eq!(
                undo::execute(&c, row["id"].as_str().unwrap()).unwrap_err(),
                "UNDO_NOT_AVAILABLE"
            );
            let retained = Path::new(row["checkpoint"].as_str().unwrap());
            assert_eq!(observed(&c, retained), f.original);
            let target = observed(&c, &f.target);
            assert_eq!(target["id"], row["object_id"]);
            assert_eq!(target["digest"], f.original["digest"]);
            if revoked {
                assert!(!f.source.exists());
            } else {
                assert_eq!(
                    std::fs::read_to_string(&f.source).unwrap(),
                    "foreign cross-volume occupant"
                );
            }
            let conflict =
                undo::execute_request(&c, row["id"].as_str().unwrap(), true, &Control::default())
                    .unwrap_err();
            assert!(
                conflict.starts_with(if revoked {
                    "PATH_SCOPE"
                } else {
                    "UNDO_DESTINATION_EXISTS"
                }),
                "{conflict}"
            );
            let foreign = f
                .volume
                .as_ref()
                .unwrap()
                .0
                .join("recovery/foreign-preserved.txt");
            if revoked {
                c.store
                    .set_setting(
                        "allowed_roots",
                        &json!([f.source, f.volume.as_ref().unwrap().0.join("recovery")]),
                    )
                    .unwrap();
            } else {
                std::fs::rename(&f.source, &foreign).unwrap();
            }
            undo::execute_request(&c, row["id"].as_str().unwrap(), true, &Control::default())
                .unwrap();
            assert_eq!(observed(&c, &f.source), f.original);
            assert!(!f.target.exists());
            if !revoked {
                assert_eq!(
                    std::fs::read_to_string(&foreign).unwrap(),
                    "foreign cross-volume occupant"
                );
            }
            drop(c);
            f.finish();
        }
    }
    for change in ["new_entry", "replacement", "revoked"] {
        let f = Fixture::new(true, false);
        let (plan, data) = cleanup_fixture(&f);
        terminations.push(f.kill(
            "files.cleanup",
            json!({"body":{"action":"apply","plan_id":plan["plan_id"]}}),
            "cleanup_child_deleted",
        ));
        let work = PathBuf::from(data["entries"][0]["work_path"].as_str().unwrap());
        let foreign = work.join("foreign.txt");
        let saved = f.profile.join("temp/retained-original.txt");
        match change {
            "new_entry" => std::fs::write(&foreign, "foreign cleanup occupant").unwrap(),
            "replacement" => {
                // Select a surviving file after the actual first deletion.
                let node = data["entries"][0]["nodes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|node| {
                        node["directory"] == false
                            && work.join(node["relative"].as_str().unwrap()).exists()
                    })
                    .unwrap();
                let path = work.join(node["relative"].as_str().unwrap());
                std::fs::rename(&path, &saved).unwrap();
                std::fs::copy(&saved, &path).unwrap();
                assert_ne!(
                    identity(&open(&saved, false, true).unwrap()).unwrap(),
                    identity(&open(&path, false, true).unwrap()).unwrap()
                );
            }
            "revoked" => revoke_source(&f),
            _ => unreachable!(),
        }
        let c = f.core();
        let before = observed(&c, &work);
        let error = cleanup(
            &c,
            &json!({"action":"apply","plan_id":plan["plan_id"]}),
            &Control::default(),
        )
        .unwrap_err();
        assert_eq!(
            error,
            match change {
                "new_entry" => "CLEANUP_NEW_ENTRY",
                "replacement" => "CLEANUP_ENTRY_CHANGED",
                "revoked" => "CLEANUP_SCOPE_CHANGED",
                _ => unreachable!(),
            }
        );
        assert_eq!(observed(&c, &work), before);
        let row = c
            .store
            .query(
                "SELECT state,next_index FROM file_cleanup_plans WHERE id=?",
                &[plan["plan_id"].clone()],
            )
            .unwrap()[0]
            .clone();
        assert_eq!(row["state"], "interrupted");
        assert_eq!(row["next_index"], 0);
        if change == "new_entry" {
            assert_eq!(
                std::fs::read_to_string(&foreign).unwrap(),
                "foreign cleanup occupant"
            );
            // Removing only this test-owned foreign file makes the original
            // reviewed partial manifest safe to resume without resetting DB.
            std::fs::remove_file(&foreign).unwrap();
            cleanup(
                &c,
                &json!({"action":"apply","plan_id":plan["plan_id"]}),
                &Control::default(),
            )
            .unwrap();
            assert!(!work.exists());
        } else if change == "replacement" {
            assert!(saved.exists());
        }
        if change != "revoked" {
            assert_eq!(observed(&c, &f.source), f.original);
        } else {
            assert!(f.source.exists());
            assert!(c
                .path(f.source.to_str().unwrap(), false)
                .unwrap_err()
                .starts_with("PATH_SCOPE"));
        }
        drop(c);
        f.finish();
    }
    assert_eq!(terminations.len(), 11);
    println!("FILES_PROCESS_KILL_GUARD_PASS: 11 actual Windows child terminations/revoked source grants/occupied originals/C+D/file+tree/foreign cleanup nodes/identical replacements/preserve and stop/explicit native revalidation after conflict resolution/resume reviewed subset");
}

#[test]
fn real_process_kill_completed_undo_is_reconciled_without_repeating_mutations() {
    let mut terminations = Vec::new();
    for directory in [false, true] {
        for action in ["copy", "move", "delete"] {
            let f = Fixture::new(directory, false);
            let c = f.core();
            let result = mutate(
                &c,
                action,
                &f.source,
                if action == "delete" {
                    None
                } else {
                    Some(&f.target)
                },
                &Control::default(),
            )
            .unwrap();
            let id = result["undo_token"].as_str().unwrap();
            let row = last_journal(&c);
            drop(c);
            terminations.push(f.kill("files.undo", json!({"id":id}), "undo_namespace_restored"));
            let c = f.core();
            assert_eq!(last_journal(&c)["state"], "blocked");
            let vacant = if action == "delete" {
                PathBuf::from(row["checkpoint"].as_str().unwrap())
            } else {
                f.target.clone()
            };
            assert!(!vacant.exists());
            std::fs::write(&vacant, "foreign after completed undo").unwrap();
            let foreign = observed(&c, &vacant);
            assert_eq!(undo::execute(&c, id).unwrap_err(), "UNDO_NOT_AVAILABLE");
            let restored = if action == "copy" {
                PathBuf::from(row["checkpoint"].as_str().unwrap())
            } else {
                f.source.clone()
            };
            let preserved = f.profile.join("temp/preserved-restored-object");
            std::fs::rename(&restored, &preserved).unwrap();
            let clone =
                mutate(&c, "copy", &preserved, Some(&restored), &Control::default()).unwrap();
            let before = observed(&c, &restored);
            assert_eq!(
                undo::execute_request(&c, id, true, &Control::default()).unwrap_err(),
                "UNDO_OBJECT_REPLACED"
            );
            assert_eq!(observed(&c, &restored), before);
            assert!(preserved.exists());
            // Retire only the clone created by this fixture, then put the exact
            // native restored object back. Never reset the original journal.
            undo::execute(&c, clone["undo_token"].as_str().unwrap()).unwrap();
            std::fs::rename(&preserved, &restored).unwrap();
            let result = undo::execute_request(&c, id, true, &Control::default()).unwrap();
            assert_eq!(result["reconciled"], true);
            assert_eq!(observed(&c, &vacant), foreign);
            assert_eq!(observed(&c, &f.source), f.original);
            assert_eq!(
                undo::execute_request(&c, id, true, &Control::default()).unwrap_err(),
                "UNDO_NOT_AVAILABLE"
            );
            drop(c);
            f.finish();
        }
        let f = Fixture::new(directory, true);
        let c = f.core();
        let result = mutate(&c, "move", &f.source, Some(&f.target), &Control::default()).unwrap();
        let id = result["undo_token"].as_str().unwrap();
        drop(c);
        terminations.push(f.kill("files.undo", json!({"id":id}), "undo_namespace_restored"));
        std::fs::write(&f.target, "foreign after cross undo").unwrap();
        let c = f.core();
        assert_eq!(last_journal(&c)["state"], "undone");
        assert_eq!(observed(&c, &f.source), f.original);
        assert_eq!(
            std::fs::read_to_string(&f.target).unwrap(),
            "foreign after cross undo"
        );
        assert_eq!(
            undo::execute_request(&c, id, true, &Control::default()).unwrap_err(),
            "UNDO_NOT_AVAILABLE"
        );
        drop(c);
        f.finish();
    }
    for created in [false, true] {
        let f = Fixture::new(false, false);
        let c = f.core();
        if created {
            std::fs::remove_file(&f.source).unwrap();
        }
        let result = write(&c, &f.source, "changed", &Control::default()).unwrap();
        let id = result["undo_token"].as_str().unwrap();
        drop(c);
        terminations.push(f.kill("files.undo", json!({"id":id}), "undo_namespace_restored"));
        let c = f.core();
        assert_eq!(last_journal(&c)["state"], "blocked");
        if created {
            assert!(!f.source.exists());
            std::fs::write(&f.source, "foreign creation").unwrap();
            assert_eq!(
                undo::execute_request(&c, id, true, &Control::default()).unwrap_err(),
                "UNDO_OBJECT_REPLACED"
            );
            let foreign = f.profile.join("temp/foreign-creation.txt");
            std::fs::rename(&f.source, &foreign).unwrap();
            let result = undo::execute_request(&c, id, true, &Control::default()).unwrap();
            assert_eq!(result["reconciled"], true);
            assert!(!f.source.exists());
            assert_eq!(
                std::fs::read_to_string(foreign).unwrap(),
                "foreign creation"
            );
        } else {
            assert_eq!(observed(&c, &f.source), f.original);
            let result = undo::execute_request(&c, id, true, &Control::default()).unwrap();
            assert_eq!(result["reconciled"], true);
            assert_eq!(observed(&c, &f.source), f.original);
        }
        drop(c);
        f.finish();
    }
    assert_eq!(terminations.len(), 10);
    println!("FILES_UNDO_REVALIDATION_CONTRACT_PASS: 10 actual child terminations after native undo/file+tree/copy+move+delete/cross-volume/write+creation/explicit blocked reconciliation/foreign occupants preserved/no repeated mutation");
}

#[test]
fn real_process_kill_reconciles_tree_cross_zip_and_cleanup_boundaries() {
    // Windows may recycle a PID after wait(); the owned child handle and marker
    // authenticate each independent termination. PID uniqueness is not required.
    let mut terminations = Vec::new();
    for directory in [false, true] {
        for action in ["copy", "move", "delete"] {
            let f = Fixture::new(directory, false);
            let tool = format!("files.{action}");
            let input = if action == "delete" {
                json!({"path":f.source})
            } else {
                json!({"path":f.source,"destination":f.target})
            };
            terminations.push(f.kill(&tool, input, "tree_namespace_published"));
            let c = f.core();
            let row = last_journal(&c);
            assert_eq!(row["state"], "available");
            undo::execute(&c, row["id"].as_str().unwrap()).unwrap();
            assert_eq!(observed(&c, &f.source), f.original);
            assert!(!f.target.exists());
            assert_eq!(
                c.path(f.base.to_str().unwrap(), false)
                    .unwrap_err()
                    .split(':')
                    .next()
                    .unwrap(),
                "PATH_SCOPE"
            );
            drop(c);
            f.finish();
        }
    }
    {
        let f = Fixture::new(true, false);
        terminations.push(f.kill(
            "files.move",
            json!({"path":f.source,"destination":f.target}),
            "tree_journal_prepared",
        ));
        let c = f.core();
        assert_eq!(last_journal(&c)["state"], "blocked");
        let row = last_journal(&c);
        for _ in 0..2 {
            assert!(undo::execute_request(
                &c,
                row["id"].as_str().unwrap(),
                true,
                &Control::default()
            )
            .is_err());
            assert_ne!(metadata(&last_journal(&c)).unwrap()["undo_started"], true);
        }
        assert_eq!(observed(&c, &f.source), f.original);
        assert!(!f.target.exists());
        drop(c);
        f.finish();
    }
    for point in [
        "cross_copy_published",
        "cross_original_retained",
        "cross_undo_target_parked",
    ] {
        for directory in [false, true] {
            let f = Fixture::new(directory, true);
            let (tool, input) = if point == "cross_undo_target_parked" {
                let c = f.core();
                let moved =
                    mutate(&c, "move", &f.source, Some(&f.target), &Control::default()).unwrap();
                drop(c);
                ("files.undo", json!({"id":moved["undo_token"]}))
            } else {
                (
                    "files.move",
                    json!({"path":f.source,"destination":f.target}),
                )
            };
            terminations.push(f.kill(tool, input, point));
            let c = f.core();
            let row = last_journal(&c);
            assert_eq!(row["state"], "available");
            undo::execute(&c, row["id"].as_str().unwrap()).unwrap();
            assert_eq!(observed(&c, &f.source), f.original);
            assert!(!f.target.exists());
            assert!(c
                .path(f.volume.as_ref().unwrap().0.to_str().unwrap(), false)
                .unwrap_err()
                .starts_with("PATH_SCOPE"));
            drop(c);
            f.finish();
        }
    }
    for point in ["cleanup_erasing_committed", "cleanup_child_deleted"] {
        let f = Fixture::new(true, false);
        let c = f.core();
        let copied = mutate(&c, "copy", &f.source, Some(&f.target), &Control::default()).unwrap();
        undo::execute(&c, copied["undo_token"].as_str().unwrap()).unwrap();
        let plan = cleanup(
            &c,
            &json!({"action":"preview","ids":[copied["undo_token"]]}),
            &Control::default(),
        )
        .unwrap();
        let data: Value = serde_json::from_str(
            c.store
                .query(
                    "SELECT snapshot_json FROM file_cleanup_plans WHERE id=?",
                    &[plan["plan_id"].clone()],
                )
                .unwrap()[0]["snapshot_json"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        drop(c);
        terminations.push(f.kill(
            "files.cleanup",
            json!({"body":{"action":"apply","plan_id":plan["plan_id"]}}),
            point,
        ));
        let c = f.core();
        let row = c
            .store
            .query(
                "SELECT * FROM file_cleanup_plans WHERE id=?",
                &[plan["plan_id"].clone()],
            )
            .unwrap()[0]
            .clone();
        assert_eq!(row["state"], "interrupted");
        let work = Path::new(data["entries"][0]["work_path"].as_str().unwrap());
        assert!(work.exists());
        let remaining = Tree::capture(&c, work, false, &Control::default())
            .unwrap()
            .nodes
            .len();
        let full = data["entries"][0]["nodes"].as_array().unwrap().len();
        assert_eq!(remaining == full, point == "cleanup_erasing_committed");
        cleanup(
            &c,
            &json!({"action":"apply","plan_id":plan["plan_id"]}),
            &Control::default(),
        )
        .unwrap();
        assert!(!work.exists());
        assert_eq!(observed(&c, &f.source), f.original);
        drop(c);
        f.finish();
    }
    {
        let mut f = Fixture::new(false, false);
        let c = f.core();
        let archive = f.source.with_extension("zip");
        let mut writer = zip::ZipWriter::new(File::create(&archive).unwrap());
        for (path, body) in [("a/new.txt", "published"), ("z/later.txt", "retained")] {
            writer
                .start_file(path, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(body.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
        c.store
            .set_setting("allowed_roots", &json!([archive]))
            .unwrap();
        f.source = archive;
        f.original = observed(&c, &f.source);
        std::fs::create_dir(&f.target).unwrap();
        std::fs::write(f.target.join("keep.txt"), "existing").unwrap();
        let target_id = identity(&open(&f.target, false, true).unwrap()).unwrap();
        drop(c);
        terminations.push(f.kill(
            "files.extract",
            json!({"path":f.source,"destination":f.target}),
            "zip_namespace_published",
        ));
        let c = f.core();
        let batch = c
            .store
            .query("SELECT * FROM undo_records WHERE kind='archive_batch'", &[])
            .unwrap()[0]
            .clone();
        assert_eq!(batch["state"], "blocked");
        let child = c
            .store
            .query(
                "SELECT * FROM undo_records WHERE json_extract(metadata_json,'$.archive_batch')=?",
                &[batch["id"].clone()],
            )
            .unwrap()[0]
            .clone();
        assert_eq!(child["state"], "available");
        assert_eq!(
            std::fs::read_to_string(f.target.join("a/new.txt")).unwrap(),
            "published"
        );
        assert!(!f.target.join("z").exists());
        let stage = Path::new(batch["staging"].as_str().unwrap());
        assert_eq!(
            std::fs::read_to_string(stage.join("z/later.txt")).unwrap(),
            "retained"
        );
        undo::execute(&c, child["id"].as_str().unwrap()).unwrap();
        assert!(!f.target.join("a").exists());
        assert_eq!(
            std::fs::read_to_string(f.target.join("keep.txt")).unwrap(),
            "existing"
        );
        assert_eq!(
            identity(&open(&f.target, false, true).unwrap()).unwrap(),
            target_id
        );
        assert_eq!(observed(&c, &f.source), f.original);
        drop(c);
        f.finish();
    }
    assert_eq!(terminations.len(), 16);
    println!("FILES_PROCESS_KILL_CONTRACT_PASS: 16 actual Windows child terminations/8 boundaries/C+D/file+tree/native IDs/readonly/Unicode/scoped root/restart/undo/partial cleanup/ZIP merge");
}

#[test]
fn real_process_kill_staging_cleanup_preserves_available_recovery() {
    let mut terminations = Vec::new();
    for directory in [false, true] {
        for point in ["cleanup_erasing_committed", "cleanup_child_deleted"] {
            let f = Fixture::new(directory, false);
            let c = f.core();
            let deleted = mutate(&c, "delete", &f.source, None, &Control::default()).unwrap();
            let token = deleted["undo_token"].clone();
            let checkpoint = PathBuf::from(deleted["checkpoint"].as_str().unwrap());
            let stage = c
                .root
                .join(format!("temp/.alfredpet-{}.staging", storage::id()));
            if directory {
                std::fs::create_dir_all(stage.join("nested/empty")).unwrap();
                std::fs::write(stage.join("nested/partial.txt"), "incomplete").unwrap();
                std::fs::write(stage.join("zero"), []).unwrap();
            } else {
                std::fs::write(&stage, "incomplete").unwrap();
            }
            let row = c
                .store
                .query(
                    "SELECT * FROM undo_records WHERE id=?",
                    std::slice::from_ref(&token),
                )
                .unwrap()[0]
                .clone();
            let mut data = metadata(&row).unwrap();
            data["stage_id"] = json!(identity(&open(&stage, false, false).unwrap()).unwrap());
            c.store.execute("UPDATE undo_records SET staging=?,metadata_json=?,updated_at='2020-01-01T00:00:00Z' WHERE id=?",&[json!(stage),json!(data.to_string()),token.clone()]).unwrap();
            c.store
                .set_setting(
                    "file_maintenance",
                    &json!({"enabled":true,"staging_days":7,"include_recovery":false}),
                )
                .unwrap();
            c.store
                .set_setting("file_checkpoint_days", &Value::Null)
                .unwrap();
            let mut state = json!({"cursor":null,"seen":[],"policy_key":"owned-crash-contract"});
            let id = maintenance::prepare(
                &c,
                &Control::default(),
                &maintenance::policy(&c),
                &mut state,
            )
            .unwrap()
            .unwrap();
            let saved: Value = serde_json::from_str(
                c.store
                    .query(
                        "SELECT snapshot_json FROM file_cleanup_plans WHERE id=?",
                        &[json!(id)],
                    )
                    .unwrap()[0]["snapshot_json"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(saved["entries"].as_array().unwrap().len(), 1);
            assert_eq!(saved["entries"][0]["journal_effect"], "retain");
            let work = PathBuf::from(saved["entries"][0]["work_path"].as_str().unwrap());
            drop(c);
            terminations.push(f.kill(
                "files.cleanup",
                json!({"body":{"action":"apply","plan_id":id}}),
                point,
            ));
            let c = f.core();
            assert_eq!(
                c.store
                    .query(
                        "SELECT state FROM undo_records WHERE id=?",
                        std::slice::from_ref(&token)
                    )
                    .unwrap()[0]["state"],
                "available"
            );
            assert!(checkpoint.exists());
            c.store
                .execute(
                    "UPDATE module_configs SET enabled=0 WHERE module_id='files'",
                    &[],
                )
                .unwrap();
            assert_eq!(
                cleanup(
                    &c,
                    &json!({"action":"apply","plan_id":id}),
                    &Control::default()
                )
                .unwrap_err(),
                "MODULE_DISABLED: files"
            );
            c.store
                .execute(
                    "UPDATE module_configs SET enabled=1 WHERE module_id='files'",
                    &[],
                )
                .unwrap();
            if directory && point == "cleanup_child_deleted" {
                let foreign = work.join("foreign.txt");
                std::fs::write(&foreign, "preserve").unwrap();
                assert_eq!(
                    cleanup(
                        &c,
                        &json!({"action":"apply","plan_id":id}),
                        &Control::default()
                    )
                    .unwrap_err(),
                    "CLEANUP_NEW_ENTRY"
                );
                assert!(foreign.exists() && checkpoint.exists());
                std::fs::remove_file(foreign).unwrap();
            }
            cleanup(
                &c,
                &json!({"action":"apply","plan_id":id}),
                &Control::default(),
            )
            .unwrap();
            assert!(!work.exists() && !stage.exists());
            assert!(c
                .store
                .query(
                    "SELECT staging FROM undo_records WHERE id=?",
                    std::slice::from_ref(&token)
                )
                .unwrap()[0]["staging"]
                .is_null());
            undo::execute(&c, token.as_str().unwrap()).unwrap();
            assert_eq!(observed(&c, &f.source), f.original);
            drop(c);
            f.finish();
        }
    }
    assert_eq!(terminations.len(), 4);
    println!("FILES_STAGING_PROCESS_KILL_PASS: 4 actual Windows child terminations/file+tree/erasing+partial-purge/module revocation/foreign node preserved/checkpoint+native undo retained");
}

#[test]
fn real_two_process_writers_and_namespace_conflicts_preserve_owned_and_foreign_objects() {
    // An existing write handle excludes a second process before truncation and
    // while failed partial bytes are being observed for a durable journal.
    for failure in [false, true] {
        let f = Fixture::new(false, false);
        let point = if failure {
            "write_failure_before_observation"
        } else {
            "write_before_truncate"
        };
        let fault = if failure {
            json!({"after_bytes":3})
        } else {
            Value::Null
        };
        let result = f.barrier(
            "files.write",
            json!({"path":f.source,"text":"replacement"}),
            point,
            fault,
            |c| {
                assert_eq!(
                    std::fs::write(&f.source, "foreign writer")
                        .unwrap_err()
                        .raw_os_error(),
                    Some(32)
                );
                assert_eq!(
                    std::fs::rename(&f.source, f.base.join("foreign-rename"))
                        .unwrap_err()
                        .raw_os_error(),
                    Some(32)
                );
                let bytes = std::fs::read(&f.source).unwrap();
                assert_eq!(
                    bytes,
                    if failure {
                        b"rep".to_vec()
                    } else {
                        b"native crash fixture".to_vec()
                    }
                );
                let row = last_journal(c);
                assert!(metadata(&row).unwrap()["write_failure"].is_null());
            },
        );
        assert_eq!(result["ok"], !failure);
        let c = f.core();
        let row = last_journal(&c);
        if failure {
            assert_eq!(
                metadata(&row).unwrap()["write_failure"]["observed_hash"],
                format!("{:x}", Sha256::digest(b"rep"))
            );
            undo::execute_request(&c, row["id"].as_str().unwrap(), true, &Control::default())
                .unwrap();
        } else {
            undo::execute(&c, row["id"].as_str().unwrap()).unwrap();
        }
        assert_eq!(observed(&c, &f.source), f.original);
        drop(c);
        f.finish();
    }
    for directory in [false, true] {
        let f = Fixture::new(directory, false);
        let result = f.barrier(
            "files.copy",
            json!({"path":f.source,"destination":f.target}),
            "copy_stage_created",
            Value::Null,
            |_| {
                let leaf = if directory {
                    f.source.join("zero.bin")
                } else {
                    f.source.clone()
                };
                assert_eq!(
                    std::fs::write(&leaf, "foreign writer")
                        .unwrap_err()
                        .raw_os_error(),
                    Some(32)
                );
                assert_eq!(
                    std::fs::rename(&leaf, f.profile.join("temp/foreign-rename"))
                        .unwrap_err()
                        .raw_os_error(),
                    Some(32)
                );
                if directory {
                    assert_eq!(
                        std::fs::rename(&f.source, f.base.join("renamed-source-root"))
                            .unwrap_err()
                            .raw_os_error(),
                        Some(32)
                    );
                }
            },
        );
        assert_eq!(result["ok"], true);
        let c = f.core();
        assert_eq!(observed(&c, &f.source), f.original);
        undo::execute(&c, result["value"]["undo_token"].as_str().unwrap()).unwrap();
        drop(c);
        f.finish();
    }
    for directory in [false, true] {
        let f = Fixture::new(directory, false);
        let result = f.barrier(
            "files.copy",
            json!({"path":f.source,"destination":f.target}),
            "copy_before_publication",
            Value::Null,
            |_| {
                let mut foreign = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&f.target)
                    .unwrap();
                foreign.write_all(b"foreign destination").unwrap();
                foreign.sync_all().unwrap();
            },
        );
        assert_eq!(result["ok"], false);
        assert_eq!(
            std::fs::read_to_string(&f.target).unwrap(),
            "foreign destination"
        );
        let c = f.core();
        assert_eq!(observed(&c, &f.source), f.original);
        let row = last_journal(&c);
        assert_eq!(row["state"], "blocked");
        assert!(Path::new(row["staging"].as_str().unwrap()).exists());
        drop(c);
        f.finish();
    }
    {
        let f = Fixture::new(true, false);
        let added = std::cell::Cell::new(false);
        let foreign = f.source.join("foreign-entry.txt");
        let result = f.barrier(
            "files.copy",
            json!({"path":f.source,"destination":f.target}),
            "copy_before_validation",
            Value::Null,
            |_| match std::fs::write(&foreign, "foreign addition") {
                Ok(()) => added.set(true),
                Err(error) => assert_eq!(error.raw_os_error(), Some(32)),
            },
        );
        let c = f.core();
        if added.get() {
            assert_eq!(result["ok"], false);
            assert!(result["error"]
                .as_str()
                .unwrap()
                .contains("TREE_COPY_CHANGED"));
            assert_eq!(
                std::fs::read_to_string(&foreign).unwrap(),
                "foreign addition"
            );
            assert!(!f.target.exists());
        } else {
            assert_eq!(result["ok"], true);
            undo::execute(&c, result["value"]["undo_token"].as_str().unwrap()).unwrap();
        }
        let source = Tree::capture(&c, &f.source, false, &Control::default()).unwrap();
        let owned = source.ownership().unwrap();
        for original in f.original["nodes"].as_array().unwrap() {
            assert!(owned.as_array().unwrap().contains(original));
        }
        drop(source);
        drop(c);
        f.finish();
    }
    {
        let f = Fixture::new(false, false);
        let result = f.barrier(
            "files.write",
            json!({"path":f.source,"text":"replacement"}),
            "write_before_truncate",
            Value::Null,
            |c| {
                c.store
                    .execute(
                        "UPDATE module_configs SET enabled=0 WHERE module_id='files'",
                        &[],
                    )
                    .unwrap();
            },
        );
        assert_eq!(result["ok"], false);
        assert!(result["error"]
            .as_str()
            .unwrap()
            .contains("MODULE_DISABLED"));
        let c = f.core();
        assert_eq!(std::fs::read(&f.source).unwrap(), b"native crash fixture");
        c.store
            .execute(
                "UPDATE module_configs SET enabled=1 WHERE module_id='files'",
                &[],
            )
            .unwrap();
        let row = last_journal(&c);
        let undone =
            undo::execute_request(&c, row["id"].as_str().unwrap(), true, &Control::default())
                .unwrap();
        assert_eq!(undone["reconciled"], true);
        assert_eq!(observed(&c, &f.source), f.original);
        drop(c);
        f.finish();
    }
    println!("FILES_TWO_PROCESS_IO_CONTRACT_PASS: 8 actual child barriers/second-process write+rename denied/native sharing errors/source file+tree pins/new source member checked/foreign publication targets preserved/module revoked before truncation/failure observed before releasing lease/explicit undo");
}

#[test]
fn real_process_kill_after_partial_write_observation_preserves_explicit_native_recovery() {
    let mut terminations = Vec::new();
    for existed in [true, false] {
        let f = Fixture::new(false, false);
        let path = if existed {
            f.source.clone()
        } else {
            f.target.clone()
        };
        terminations.push(f.kill_fault(
            "files.write",
            json!({"path":path,"text":"replacement"}),
            "write_failure_observed",
            json!({"after_bytes":3}),
        ));
        let c = f.core();
        let row = last_journal(&c);
        assert_eq!(row["state"], "blocked");
        assert_eq!(std::fs::read(&path).unwrap(), b"rep");
        assert_eq!(
            metadata(&row).unwrap()["write_failure"]["observed_hash"],
            format!("{:x}", Sha256::digest(b"rep"))
        );
        undo::execute_request(&c, row["id"].as_str().unwrap(), true, &Control::default()).unwrap();
        if existed {
            assert_eq!(observed(&c, &path), f.original);
        } else {
            assert!(!path.exists());
            assert_eq!(observed(&c, &f.source), f.original);
        }
        drop(c);
        f.finish();
    }
    assert_eq!(terminations.len(), 2);
    println!("FILES_PARTIAL_WRITE_PROCESS_KILL_PASS: 2 actual Windows child terminations/observed partial bytes/existing+new file/native identity/checkpoint integrity/explicit revalidation after restart");
}
