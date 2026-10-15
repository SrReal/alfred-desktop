use super::*;
use std::io::Write;

fn case_spec() -> (Value, PathBuf) {
    let path = PathBuf::from(
        std::env::var_os("ALFREDPET_TEST_WINDOW_SPEC").expect("owned case spec required"),
    )
    .canonicalize()
    .unwrap();
    let root = path
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap();
    assert_eq!(
        root.parent().unwrap(),
        std::env::temp_dir().canonicalize().unwrap()
    );
    uuid::Uuid::parse_str(
        root.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .strip_prefix("ap-wu-")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        std::fs::symlink_metadata(&root).unwrap().file_attributes() & 0x400,
        0
    );
    let raw = std::fs::read(&path).unwrap();
    assert!(raw.len() < 16384);
    let spec: Value = serde_json::from_slice(&raw).unwrap();
    let id = spec["case_id"].as_str().unwrap();
    assert_eq!(uuid::Uuid::parse_str(id).unwrap().to_string(), id);
    assert_eq!(path, root.join(format!("temp/window-case-{id}.json")));
    assert_eq!(root_identity(&root), spec["root_id"].as_str().unwrap());
    (spec, root)
}
fn case_path(root: &std::path::Path, spec: &Value, role: &str) -> PathBuf {
    assert!(["ready", "release", "result", "log"].contains(&role));
    root.join(format!(
        "temp/window-{role}-{}.json",
        spec["case_id"].as_str().unwrap()
    ))
}
fn write_new(path: &std::path::Path, value: &Value) {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    write!(file, "{value}").unwrap();
    file.sync_all().unwrap();
}
pub(in crate::window_undo) fn at(core: &Core, point: &str, index: usize, id: &str) {
    if std::env::var_os("ALFREDPET_TEST_WINDOW_SPEC").is_none() {
        return;
    }
    let (spec, root) = case_spec();
    if spec["point"].as_str() != Some(point) || spec["index"].as_u64() != Some(index as u64) {
        return;
    }
    assert_eq!(core.root.canonicalize().unwrap(), root);
    write_new(
        &case_path(&root, &spec, "ready"),
        &json!({"case_id":spec["case_id"],"pid":std::process::id(),"point":point,"index":index,"journal_id":id}),
    );
    if spec["barrier"] == true {
        let deadline = Instant::now() + Duration::from_secs(45);
        while Instant::now() < deadline {
            if let Ok(raw) = std::fs::read(case_path(&root, &spec, "release")) {
                if let Ok(value) = serde_json::from_slice::<Value>(&raw) {
                    if value["pid"] == std::process::id() && value["case_id"] == spec["case_id"] {
                        return;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("owned backend barrier was not released");
    }
    loop {
        std::thread::park();
    }
}
#[test]
#[ignore = "internal owned backend child; invoked only by the window recovery contracts"]
fn window_backend_child_fixture() {
    let (spec, root) = case_spec();
    let core = Arc::new(
        Core::new(
            root.clone(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap(),
    );
    let allowed = spec["targets"].as_array().unwrap();
    assert_eq!(allowed.len(), 2);
    let owns = |target: &Value| {
        allowed
            .iter()
            .any(|w| w["hwnd"] == target["hwnd"] && w["process_id"] == target["process_id"])
    };
    let tool = spec["tool"].as_str().unwrap();
    assert!(["window.move", "window.set_layout", "window.undo"].contains(&tool));
    if tool == "window.undo" {
        let (record, _) = load(&core, spec["input"]["id"].as_str().unwrap()).unwrap();
        assert!(record.steps.iter().all(|s| owns(&s.before)));
    } else if tool == "window.set_layout" {
        assert!(spec["input"]["body"].as_array().unwrap().iter().all(owns));
    } else {
        assert!(owns(&spec["input"]));
    }
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(Box::pin(window_layouts::dispatch(
            &core,
            tool.strip_prefix("window.").unwrap(),
            &spec["input"],
        )));
    assert!(
        spec["point"].is_null() || spec["barrier"] == true,
        "owned kill point was never reached: {result:?}"
    );
    let result = match result {
        Ok(value) => json!({"ok":true,"value":value}),
        Err(error) => json!({"ok":false,"error":error}),
    };
    write_new(&case_path(&root, &spec, "result"), &result);
}
struct Backend {
    child: tokio::process::Child,
    _job: crate::windows_native::ProcessJob,
    core: Arc<Core>,
    spec: Value,
}
impl Backend {
    async fn start(
        core: &Arc<Core>,
        targets: &[Value],
        tool: &str,
        input: Value,
        point: Option<&str>,
        barrier: bool,
    ) -> Self {
        Self::start_at(core, targets, tool, input, point, barrier, 0).await
    }
    async fn start_at(
        core: &Arc<Core>,
        targets: &[Value],
        tool: &str,
        input: Value,
        point: Option<&str>,
        barrier: bool,
        index: usize,
    ) -> Self {
        let spec = json!({"case_id":storage::id(),"root_id":root_identity(&core.root),"targets":targets,"tool":tool,"input":input,"point":point,"index":index,"barrier":barrier});
        let path = core.root.join(format!(
            "temp/window-case-{}.json",
            spec["case_id"].as_str().unwrap()
        ));
        write_new(&path, &spec);
        let log = std::fs::File::create(case_path(&core.root, &spec, "log")).unwrap();
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "window_undo::tests::recovery::crash::window_backend_child_fixture",
                "--ignored",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("ALFREDPET_TEST_WINDOW_SPEC", &path)
            .creation_flags(0x08000000)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        let child = command.spawn().unwrap();
        let job = crate::windows_native::ProcessJob::attach(&child).unwrap();
        Self {
            child,
            _job: job,
            core: core.clone(),
            spec,
        }
    }
    async fn ready(&mut self) -> Value {
        let deadline = Instant::now() + Duration::from_secs(70);
        loop {
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "owned backend exited before marker: {}",
                std::fs::read_to_string(case_path(&self.core.root, &self.spec, "log")).unwrap()
            );
            if let Ok(raw) = std::fs::read(case_path(&self.core.root, &self.spec, "ready")) {
                if let Ok(value) = serde_json::from_slice::<Value>(&raw) {
                    assert_eq!(value["pid"], self.child.id().unwrap());
                    assert_eq!(value["case_id"], self.spec["case_id"]);
                    assert_eq!(value["point"], self.spec["point"]);
                    assert_eq!(value["index"], self.spec["index"]);
                    uuid::Uuid::parse_str(value["journal_id"].as_str().unwrap()).unwrap();
                    return value;
                }
            }
            assert!(Instant::now() < deadline, "owned backend marker timed out");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    async fn kill(mut self) {
        let pid = self.child.id().unwrap();
        self.child.kill().await.unwrap();
        assert!(!self.child.wait().await.unwrap().success());
        println!(
            "owned window backend killed at {}; pid={pid}",
            self.spec["point"]
        );
    }
    async fn finish(mut self, release: bool) -> Value {
        if release {
            write_new(
                &case_path(&self.core.root, &self.spec, "release"),
                &json!({"pid":self.child.id().unwrap(),"case_id":self.spec["case_id"]}),
            );
        }
        let status = tokio::time::timeout(Duration::from_secs(70), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(
            status.success(),
            "owned backend failed: {}",
            std::fs::read_to_string(case_path(&self.core.root, &self.spec, "log")).unwrap()
        );
        serde_json::from_slice(
            &std::fs::read(case_path(&self.core.root, &self.spec, "result")).unwrap(),
        )
        .unwrap()
    }
}
async fn count(windows: &mut Windows) -> u64 {
    windows.command("report").await;
    windows.info["windows"][0]["position_messages"]
        .as_u64()
        .unwrap()
}
async fn initialized(core: &Core, windows: &Windows) -> Vec<Value> {
    let targets: Vec<Value> = windows.info["windows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| json!({"hwnd":w["hwnd"],"process_id":w["process_id"]}))
        .collect();
    let snapshots = capture(core, &json!(targets), true, &Lease::new())
        .await
        .unwrap();
    for (snapshot, owned) in snapshots
        .iter()
        .zip(windows.info["windows"].as_array().unwrap())
    {
        assert_eq!(snapshot["process_started"], owned["process_started"]);
    }
    targets
}
fn desired(before: &Value, delta: i64) -> Value {
    json!({"hwnd":before["hwnd"],"process_id":before["process_id"],"x":number(before,"x").unwrap()+delta,"y":before["y"],"width":before["width"],"height":before["height"]})
}
#[tokio::test]
async fn real_window_backend_process_kills_preserve_unknown_results_and_reconcile_completed_inverse(
) {
    let f = Fixture::new();
    let mut windows = Windows::new(&f.core).await;
    let targets = initialized(&f.core, &windows).await;
    for point in [
        "prepared",
        "applying",
        "native_result",
        "applied",
        "undoing",
        "inverse_result",
        "inverse_observed",
        "undone",
    ] {
        let before = current(&f.core, &targets[0]).await;
        let inverse = ["undoing", "inverse_result", "inverse_observed", "undone"].contains(&point);
        let (tool, input) = if inverse {
            let changed = change(&f.core, "window.move", &desired(&before, 14)).await;
            ("window.undo", json!({"id":changed["undo_token"]}))
        } else {
            ("window.move", desired(&before, 14))
        };
        let mut backend = Backend::start(&f.core, &targets, tool, input, Some(point), false).await;
        let ready = backend.ready().await;
        let id = ready["journal_id"].as_str().unwrap().to_string();
        backend.kill().await;
        let after_kill = current(&f.core, &targets[0]).await;
        let counter = count(&mut windows).await;
        let restarted = Arc::new(Core::new(f.core.root.clone(), f.core.resource.clone()).unwrap());
        assert_eq!(current(&restarted, &targets[0]).await, after_kill);
        assert_eq!(count(&mut windows).await, counter);
        if point == "undone" {
            assert_eq!(load(&restarted, &id).unwrap().1, "undone");
            assert_eq!(
                execute_undo(&restarted, &json!({"id":id,"revalidate":true}))
                    .await
                    .unwrap_err(),
                "UNDO_NOT_AVAILABLE"
            );
            assert_eq!(after_kill, before);
        } else {
            assert_eq!(load(&restarted, &id).unwrap().1, "blocked");
            if point == "applied" {
                assert!(restarted
                    .execute("window.undo", json!({"id":id,"revalidate":true}))
                    .await
                    .unwrap_err()
                    .starts_with("APP_MISSING"));
                assert_eq!(current(&restarted, &targets[0]).await, after_kill);
            }
            let result = execute_undo(&restarted, &json!({"id":id,"revalidate":true})).await;
            if ["applying", "native_result", "undoing"].contains(&point) {
                assert!(result.unwrap_err().contains("WINDOW_NATIVE_RESULT_UNKNOWN"));
                assert_eq!(current(&restarted, &targets[0]).await, after_kill);
                assert_eq!(count(&mut windows).await, counter);
                windows.external_move(&before).await;
            } else {
                let result = result.unwrap();
                assert_eq!(current(&restarted, &targets[0]).await, before);
                if point != "applied" {
                    assert_eq!(result["reconciled"], true);
                    assert_eq!(count(&mut windows).await, counter);
                }
            }
        }
        drop(restarted);
    }
    // Kill during a partial two-window forward batch and a partial inverse.
    // The already restored member must not receive another positioning message.
    for (point, index) in [("applied", 0), ("inverse_observed", 1)] {
        let before = [
            current(&f.core, &targets[0]).await,
            current(&f.core, &targets[1]).await,
        ];
        let body = before.iter().map(|s| desired(s, 18)).collect::<Vec<_>>();
        let (tool, input) = if point == "inverse_observed" {
            let changed = change(&f.core, "window.set_layout", &json!({"body":body})).await;
            ("window.undo", json!({"id":changed["undo_token"]}))
        } else {
            ("window.set_layout", json!({"body":body}))
        };
        let mut backend =
            Backend::start_at(&f.core, &targets, tool, input, Some(point), false, index).await;
        let ready = backend.ready().await;
        let id = ready["journal_id"].as_str().unwrap().to_string();
        backend.kill().await;
        let after_kill = [
            current(&f.core, &targets[0]).await,
            current(&f.core, &targets[1]).await,
        ];
        windows.command("report").await;
        let counts = [
            windows.info["windows"][0]["position_messages"]
                .as_u64()
                .unwrap(),
            windows.info["windows"][1]["position_messages"]
                .as_u64()
                .unwrap(),
        ];
        let restarted = Arc::new(Core::new(f.core.root.clone(), f.core.resource.clone()).unwrap());
        for (target, after) in targets.iter().zip(&after_kill) {
            assert_eq!(current(&restarted, target).await, *after);
        }
        assert_eq!(load(&restarted, &id).unwrap().1, "blocked");
        let result = execute_undo(&restarted, &json!({"id":id,"revalidate":true}))
            .await
            .unwrap();
        assert_eq!(result["reconciled"], false);
        for (target, original) in targets.iter().zip(&before) {
            assert_eq!(current(&restarted, target).await, *original);
        }
        windows.command("report").await;
        assert!(
            windows.info["windows"][0]["position_messages"]
                .as_u64()
                .unwrap()
                > counts[0]
        );
        assert_eq!(
            windows.info["windows"][1]["position_messages"]
                .as_u64()
                .unwrap(),
            counts[1]
        );
        drop(restarted);
    }
    windows.finish().await;
    f.finish();
    println!("WINDOW_PROCESS_KILL_PASS: 10 owned native backend terminations/8 logical persisted boundaries/single and partial two-window forward+inverse batches/provider retained/startup never positions windows/unknown applying outcomes blocked/explicit known undo/completed inverse reconciled without repeated positioning; excludes whole GUI crash and power loss");
}
// The applied marker follows the durable native readback. Reuse that exact
// owned observation rather than spending the child's barrier lease on another
// native process. Independent live checks still occur after contention/closure.
fn observed_at_applied_barrier(core: &Core, id: &str) -> Value {
    let (record, state) = load(core, id).unwrap();
    assert_eq!(state, "prepared");
    assert_eq!(record.steps.len(), 1);
    assert_eq!(record.steps[0].state, "applied");
    let observed = record.steps[0].after.clone().expect("durable native after");
    snapshot(&observed, true).unwrap();
    observed
}
#[tokio::test]
async fn real_window_profile_actor_barriers_preserve_later_writer_and_module_revocation() {
    let f = Fixture::new();
    let mut windows = Windows::new(&f.core).await;
    let targets = initialized(&f.core, &windows).await;
    let before = current(&f.core, &targets[0]).await;
    let mut owner = Backend::start(
        &f.core,
        &targets,
        "window.move",
        desired(&before, 16),
        Some("applied"),
        true,
    )
    .await;
    let ready = owner.ready().await;
    let id = ready["journal_id"].as_str().unwrap().to_string();
    let observed = observed_at_applied_barrier(&f.core, &id);
    let observer = Arc::new(Core::new(f.core.root.clone(), f.core.resource.clone()).unwrap());
    assert_eq!(load(&observer, &id).unwrap().1, "prepared");
    let competitor = Backend::start(
        &observer,
        &targets,
        "window.move",
        desired(&observed, 22),
        None,
        false,
    )
    .await
    .finish(false)
    .await;
    assert_eq!(competitor["ok"], false);
    assert_eq!(competitor["error"], "WINDOW_ACTION_BUSY");
    assert_eq!(current(&observer, &targets[0]).await, observed);
    // The competing actor is this independently controlled native fixture.
    // Its acknowledged mailbox moves the owned form while the owner is paused.
    windows.command("later_writer").await;
    assert_eq!(owner.finish(true).await["ok"], true);
    let later = current(&observer, &targets[0]).await;
    assert_eq!(
        number(&later, "x").unwrap(),
        number(&observed, "x").unwrap() + 7
    );
    assert!(execute_undo(&observer, &json!({"id":id}))
        .await
        .unwrap_err()
        .contains("WINDOW_CHANGED_AFTER_ACTION"));
    assert_eq!(current(&observer, &targets[0]).await, later);
    windows.external_move(&observed).await;
    execute_undo(&observer, &json!({"id":id,"revalidate":true}))
        .await
        .unwrap();
    assert_eq!(current(&observer, &targets[0]).await, before);
    let mut owner = Backend::start(
        &f.core,
        &targets,
        "window.move",
        desired(&before, 16),
        Some("applied"),
        true,
    )
    .await;
    let ready = owner.ready().await;
    let id = ready["journal_id"].as_str().unwrap().to_string();
    let observed = observed_at_applied_barrier(&f.core, &id);
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='window'",
            &[],
        )
        .unwrap();
    let result = owner.finish(true).await;
    assert_eq!(result["ok"], false);
    assert!(result["error"]
        .as_str()
        .unwrap()
        .contains("TOOL_DISABLED_OR_UNKNOWN"));
    windows.command("report").await;
    assert_eq!(windows.info["windows"][0]["snapshot"], observed);
    assert_eq!(
        execute_undo(&f.core, &json!({"id":id,"revalidate":true}))
            .await
            .unwrap_err(),
        "TOOL_DISABLED_OR_UNKNOWN"
    );
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=1 WHERE module_id='window'",
            &[],
        )
        .unwrap();
    execute_undo(&f.core, &json!({"id":id,"revalidate":true}))
        .await
        .unwrap();
    assert_eq!(current(&f.core, &targets[0]).await, before);
    let changed = change(&f.core, "window.move", &desired(&before, 16)).await;
    let id = changed["undo_token"].as_str().unwrap();
    let observed = current(&f.core, &targets[0]).await;
    let (mut record, _) = load(&f.core, id).unwrap();
    let original_monitors = record.monitors.clone();
    record.monitors[0].scale += 0.25;
    let write_record = |record: &Record| {
        f.core
            .store
            .execute(
                "UPDATE undo_records SET metadata_json=?,expected_hash=? WHERE id=?",
                &[
                    json!(serde_json::to_string(record).unwrap()),
                    json!(hash(record).unwrap()),
                    json!(id),
                ],
            )
            .unwrap();
    };
    write_record(&record);
    let counter = count(&mut windows).await;
    assert!(execute_undo(&f.core, &json!({"id":id}))
        .await
        .unwrap_err()
        .contains("LAYOUT_MONITORS_CHANGED"));
    assert_eq!(current(&f.core, &targets[0]).await, observed);
    assert_eq!(count(&mut windows).await, counter);
    record.monitors = original_monitors;
    write_record(&record);
    execute_undo(&f.core, &json!({"id":id,"revalidate":true}))
        .await
        .unwrap();
    assert_eq!(current(&f.core, &targets[0]).await, before);
    drop(observer);
    windows.finish().await;
    f.finish();
    println!("WINDOW_PROFILE_ACTOR_PASS: two live same-profile backend barriers/competing process denied/live startup preserves journal/later native writer retained/module revocation stops rollback/recorded monitor snapshot mismatch stops inverse; not real hotplug or arbitrary-writer stress");
}
