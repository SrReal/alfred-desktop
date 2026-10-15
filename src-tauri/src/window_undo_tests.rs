use super::*;
use std::{
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::PathBuf,
};

fn root_identity(path: &std::path::Path) -> String {
    use windows::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
    };
    let file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(7)
        .custom_flags(0x02000000 | 0x00200000)
        .open(path)
        .unwrap();
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }.unwrap();
    format!(
        "{}:{}:{}",
        info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow
    )
}
struct Fixture {
    core: Arc<Core>,
    root_id: String,
}
impl Fixture {
    fn new() -> Self {
        let core = Arc::new(
            Core::new(
                std::env::temp_dir().join(format!("ap-wu-{}", storage::id())),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        let root_id = root_identity(&core.root);
        Self { core, root_id }
    }
    fn finish(self) {
        let root = self.core.root.canonicalize().unwrap();
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
        assert_eq!(root_identity(&root), self.root_id);
        drop(self.core);
        std::fs::remove_dir_all(root).unwrap();
    }
}
fn fake_snapshot() -> Value {
    json!({"hwnd":123,"process_id":456,"process_started":"123456789","window_identity":"123456789abcdef0","monitor":"fake-negative","x":-1900,"y":20,"width":460,"height":340,"normal_screen":{"x":-1900,"y":20,"width":460,"height":340},"placement":{"show_cmd":1,"min_x":-1,"min_y":-1,"max_x":-1,"max_y":-1,"left":-1900,"top":20,"right":-1440,"bottom":360}})
}
fn fake_record() -> Record {
    let before = fake_snapshot();
    Record {
        version: 1,
        tool: "window.move".into(),
        request: json!({"hwnd":123,"x":1,"y":2,"width":460,"height":340}),
        monitors: vec![Monitor {
            id: "fake-negative".into(),
            work: crate::geometry::Rect {
                x: -1920.,
                y: 0.,
                width: 1920.,
                height: 1040.,
            },
            scale: 1.5,
        }],
        steps: vec![Step {
            action: "move".into(),
            input: json!({"hwnd":123,"x":1,"y":2,"width":460,"height":340}),
            before,
            after: None,
            state: "unstarted".into(),
            restore_to_normal: false,
        }],
        error: None,
    }
}
fn insert(core: &Core, record: &Record, state: &str) -> String {
    let id = storage::id();
    core.store.execute("INSERT INTO undo_records(id,kind,path,expected_hash,state,created_at,updated_at,metadata_json) VALUES(?,'window_change','Owned fixture',?,?,?,?,?)",&[json!(id),json!(hash(record).unwrap()),json!(state),json!(storage::now()),json!(storage::now()),json!(serde_json::to_string(record).unwrap())]).unwrap();
    id
}
#[test]
fn snapshots_plans_and_immutable_record_hash_reject_unknown_or_changed_identity() {
    let before = fake_snapshot();
    snapshot(&before, true).unwrap();
    for (field, value) in [
        ("window_identity", json!(null)),
        ("width", json!(0)),
        ("hwnd", json!(-1)),
    ] {
        let mut bad = before.clone();
        bad[field] = value;
        assert!(snapshot(&bad, true).is_err());
    }
    let mut absent = before.clone();
    absent["window_identity"] = Value::Null;
    assert!(unchanged_before_initialization(&absent, &before));
    let mut changed = before.clone();
    changed["window_identity"] = json!("123456789abcdef1");
    assert!(!unchanged_before_initialization(&before, &changed));
    let record = fake_record();
    let monitors = &record.monitors;
    let (_, snap, normal) = planned(
        "window.snap",
        &json!({"kind":"bottom_right"}),
        &before,
        monitors,
    )
    .unwrap();
    assert!(normal);
    assert_eq!(
        (
            snap["x"].as_i64(),
            snap["y"].as_i64(),
            snap["width"].as_i64(),
            snap["height"].as_i64()
        ),
        (Some(-960), Some(520), Some(960), Some(520))
    );
    assert!(planned(
        "window.move",
        &json!({"x":i64::MIN,"y":0,"width":1,"height":1}),
        &before,
        monitors
    )
    .is_err());
    assert!(planned(
        "window.snap",
        &json!({"kind":"left","monitor":"missing"}),
        &before,
        monitors
    )
    .is_err());
    let (_, integral, _) = planned(
        "window.move",
        &json!({"x":1.0,"y":2.0,"width":3.0,"height":4.0}),
        &before,
        monitors,
    )
    .unwrap();
    assert_eq!(integral["x"], 1);
    let (_, sanitized, _) = planned(
        "window.move",
        &json!({"x":1,"y":2,"width":3,"height":4,"title":"must not persist","content":"private"}),
        &before,
        monitors,
    )
    .unwrap();
    assert!(sanitized.get("title").is_none() && sanitized.get("content").is_none());
    let mut edited = record.clone();
    edited.steps[0].before["x"] = json!(-1800);
    assert_ne!(hash(&edited).unwrap(), hash(&record).unwrap());
}
#[tokio::test]
async fn window_journals_survive_restart_require_l3_and_never_enter_file_recovery() {
    let f = Fixture::new();
    let mut record = fake_record();
    record.steps[0].state = "applying".into();
    let id = insert(&f.core, &record, "prepared");
    let lock = profile_lock(&f.core).unwrap();
    let observer = Core::new(f.core.root.clone(), f.core.resource.clone()).unwrap();
    assert_eq!(history(&observer).unwrap()[0]["state"], "prepared");
    drop(observer);
    drop(lock);
    let restarted = Arc::new(Core::new(f.core.root.clone(), f.core.resource.clone()).unwrap());
    assert_eq!(history(&restarted).unwrap()[0]["state"], "blocked");
    assert!(crate::undo::history(&restarted)
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        crate::undo::execute(&restarted, &id).unwrap_err(),
        "WINDOW_JOURNAL_REQUIRES_WINDOW_UNDO"
    );
    let input = json!({"id":id,"revalidate":true});
    assert_eq!(
        crate::policy::classify("window.undo", &input),
        crate::policy::Risk::Mandatory
    );
    assert!(restarted
        .execute("window.undo", input.clone())
        .await
        .unwrap_err()
        .starts_with("APP_MISSING"));
    assert_eq!(
        restarted
            .store
            .query(
                "SELECT status FROM activity_events WHERE tool_id='window.undo'",
                &[]
            )
            .unwrap()[0]["status"],
        "denied"
    );
    let schema = crate::catalog::tool_schema("window.undo", "id");
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&input));
    assert!(!validator.is_valid(&json!({"id":id,"force":true})));
    assert!(
        review(&restarted, &input).unwrap()["windows_in_undo_order"][0]["original"]
            .get("title")
            .is_none()
    );
    record.steps[0].before["width"] = json!(999);
    save(&restarted, &id, &record, "blocked").unwrap();
    assert!(load(&restarted, &id).is_err());
    drop(restarted);
    f.finish();
}
#[tokio::test]
async fn queued_window_changes_stop_on_disable_or_abandoned_future_before_any_journal() {
    let f = Fixture::new();
    let lock = window_layouts::ACTIONS.lock().await;
    let core = f.core.clone();
    let job = tokio::spawn(async move { execute(&core, Prepared(fake_record())).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    job.abort();
    assert!(job.await.unwrap_err().is_cancelled());
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='window'",
            &[],
        )
        .unwrap();
    drop(lock);
    assert_eq!(
        execute(&f.core, Prepared(fake_record())).await.unwrap_err(),
        "TOOL_DISABLED_OR_UNKNOWN"
    );
    assert!(history(&f.core).unwrap().as_array().unwrap().is_empty());
    f.finish();
}
struct Windows {
    child: tokio::process::Child,
    _job: crate::windows_native::ProcessJob,
    metadata: PathBuf,
    commands: PathBuf,
    info: Value,
}
impl Windows {
    async fn new(core: &Core) -> Self {
        let metadata = core.root.join("temp/windows.json");
        let commands = core.root.join("temp/window-command.txt");
        // Keep compilation and first native readiness inside the existing startup bound.
        let deadline = Instant::now() + Duration::from_secs(20);
        let compiler = PathBuf::from(std::env::var("SystemRoot").unwrap())
            .join("Microsoft.NET/Framework64/v4.0.30319/csc.exe");
        let executable = core.root.join("temp").join("window-recovery.exe");
        let fixture_source = core
            .resource
            .join("tests")
            .join("fixtures")
            .join("window-recovery.cs")
            .canonicalize()
            .unwrap();
        // The legacy Framework compiler rejects mixed separators and verbatim paths.
        let compiler_source = fixture_source
            .to_str()
            .unwrap()
            .strip_prefix(r"\\?\")
            .expect("owned canonical Windows fixture path required");
        assert_eq!(compiler_source.as_bytes().get(1), Some(&b':'));
        let mut compile = tokio::process::Command::new(compiler);
        compile
            .args(["/nologo", "/target:winexe"])
            .arg(format!("/out:{}", executable.display()))
            .arg("/reference:System.Windows.Forms.dll,System.Drawing.dll,System.Web.Extensions.dll")
            .arg(compiler_source)
            .creation_flags(0x08000000)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let child = compile
            .spawn()
            .expect("owned native fixture compiler required");
        let job = crate::windows_native::ProcessJob::attach(&child).unwrap();
        let output = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            child.wait_with_output(),
        )
        .await
        .expect("owned fixture compilation exceeded startup bound")
        .unwrap();
        assert!(
            output.status.success(),
            "owned native fixture compilation failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        drop(job);
        let mut command = tokio::process::Command::new(executable);
        command
            .arg(&metadata)
            .arg(&commands)
            .creation_flags(0x08000000)
            .kill_on_drop(true)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let child = command.spawn().unwrap();
        let job = crate::windows_native::ProcessJob::attach(&child).unwrap();
        let mut result = Self {
            child,
            _job: job,
            metadata,
            commands,
            info: Value::Null,
        };
        result.wait_until(-1, deadline).await;
        result
    }
    async fn wait(&mut self, previous: i64) {
        self.wait_until(previous, Instant::now() + Duration::from_secs(20))
            .await;
    }
    async fn wait_until(&mut self, previous: i64, deadline: Instant) {
        loop {
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "owned fixture exited"
            );
            if let Ok(raw) = std::fs::read(&self.metadata) {
                if let Ok(value) = serde_json::from_slice::<Value>(&raw) {
                    if value["revision"].as_i64().unwrap_or(-1) > previous {
                        assert_eq!(value["windows"][0]["process_id"], self.child.id().unwrap());
                        self.info = value;
                        return;
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "owned fixture startup/command timed out"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    async fn command_file(&self) -> std::fs::File {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .share_mode(0)
                .open(&self.commands)
            {
                Ok(file) => return file,
                Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) => {
                    assert!(Instant::now() < deadline, "Owned mailbox lock timed out");
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => panic!("Owned mailbox open failed: {error}"),
            }
        }
    }
    async fn external_move(&mut self, target: &Value) {
        assert!(self.info["windows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["hwnd"] == target["hwnd"]));
        std::fs::write(
            format!("{}.move", self.commands.display()),
            target.to_string(),
        )
        .unwrap();
        self.command("move").await;
    }
    async fn command(&mut self, command: &str) {
        assert!([
            "report",
            "veto",
            "allow",
            "recreate",
            "later_writer",
            "block_move",
            "move"
        ]
        .contains(&command));
        let previous = self.info["revision"].as_i64().unwrap();
        let mut file = self.command_file().await;
        // The native fixture reads and clears under the same exclusive handle.
        file.set_len(0).unwrap();
        std::io::Write::write_all(&mut file, command.as_bytes()).unwrap();
        std::io::Write::flush(&mut file).unwrap();
        drop(file);
        self.wait(previous).await;
    }
    async fn finish(mut self) {
        self.child.kill().await.unwrap();
        self.child.wait().await.unwrap();
    }
}
#[tokio::test]
async fn owned_command_mailbox_retries_sharing_and_acknowledges_once() {
    let f = Fixture::new();
    let mut windows = Windows::new(&f.core).await;
    let lock = windows.command_file().await;
    let release = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(lock);
    });
    let start = Instant::now();
    let previous = windows.info["revision"].as_i64().unwrap();
    windows.command("report").await;
    assert!(start.elapsed() >= Duration::from_millis(180));
    assert_eq!(windows.info["revision"], previous + 1);
    release.await.unwrap();
    for _ in 0..32 {
        let previous = windows.info["revision"].as_i64().unwrap();
        windows.command("report").await;
        assert_eq!(windows.info["revision"], previous + 1);
    }
    windows.finish().await;
    f.finish();
}
async fn current(core: &Core, target: &Value) -> Value {
    Box::pin(capture(core, &json!([target]), false, &Lease::new()))
        .await
        .unwrap()[0]
        .clone()
}
async fn change(core: &Arc<Core>, tool: &str, input: &Value) -> Value {
    let result = match Box::pin(execute(core, prepare(core, tool, input).await.unwrap())).await {
        Ok(result) => result,
        Err(error) => panic!("owned fixture {tool}: {error}"),
    };
    assert_eq!(result["undo_available"], true);
    result
}
async fn undo_native(core: &Arc<Core>, result: &Value) {
    if let Err(error) = Box::pin(execute_undo(
        core,
        &json!({"id":result["undo_token"],"revalidate":true}),
    ))
    .await
    {
        panic!(
            "{error}; owned fixture observations: {}",
            review(core, &json!({"id":result["undo_token"]})).unwrap()
        );
    }
}
#[tokio::test]
async fn real_owned_windows_persistent_undo_preserves_later_changes_provider_veto_and_recreated_windows(
) {
    let f = Fixture::new();
    let mut windows = Windows::new(&f.core).await;
    let target = windows.info["windows"][0].clone();
    let original = current(&f.core, &target).await;
    let input = json!({"hwnd":target["hwnd"],"process_id":target["process_id"],"x":number(&original,"x").unwrap()+35,"y":number(&original,"y").unwrap()+20,"width":original["width"],"height":original["height"]});
    f.core.grants.lock().unwrap().insert(
        format!("window.move:{input}"),
        crate::policy::Risk::Contextual,
    );
    let moved = f.core.execute("window.move", input).await.unwrap();
    let observed = current(&f.core, &target).await;
    assert!(observed["window_identity"].as_str().is_some());
    assert_ne!(observed["x"], original["x"]);
    let input = json!({"id":moved["undo_token"]});
    assert!(f
        .core
        .execute("window.undo", input.clone())
        .await
        .unwrap_err()
        .starts_with("APP_MISSING"));
    assert_eq!(current(&f.core, &target).await, observed);
    assert_eq!(
        f.core
            .store
            .query(
                "SELECT status FROM activity_events WHERE tool_id='window.undo'",
                &[]
            )
            .unwrap()[0]["status"],
        "denied"
    );
    let restarted = Arc::new(Core::new(f.core.root.clone(), f.core.resource.clone()).unwrap());
    assert_eq!(history(&restarted).unwrap()[0]["state"], "available");
    undo_native(&restarted, &moved).await;
    let restored = current(&restarted, &target).await;
    let mut tagged_original = original.clone();
    tagged_original["window_identity"] = restored["window_identity"].clone();
    assert_eq!(restored, tagged_original);
    assert_eq!(
        execute_undo(&restarted, &input).await.unwrap_err(),
        "UNDO_NOT_AVAILABLE"
    );
    let move_again = json!({"hwnd":target["hwnd"],"x":number(&restored,"x").unwrap()+25,"y":restored["y"],"width":restored["width"],"height":restored["height"]});
    let changed = change(&restarted, "window.move", &move_again).await;
    let owned_after = current(&restarted, &target).await;
    let mut foreign = owned_after.clone();
    foreign["x"] = json!(number(&foreign, "x").unwrap() + 15);
    windows.external_move(&foreign).await;
    let later = current(&restarted, &target).await;
    assert!(
        execute_undo(&restarted, &json!({"id":changed["undo_token"]}))
            .await
            .unwrap_err()
            .contains("WINDOW_CHANGED_AFTER_ACTION")
    );
    assert_eq!(current(&restarted, &target).await, later);
    windows.external_move(&owned_after).await;
    undo_native(&restarted, &changed).await;
    assert_eq!(current(&restarted, &target).await, restored);
    let state = change(
        &restarted,
        "window.state",
        &json!({"hwnd":target["hwnd"],"state":"minimize"}),
    )
    .await;
    assert_eq!(
        current(&restarted, &target).await["placement"]["show_cmd"],
        2
    );
    undo_native(&restarted, &state).await;
    assert_eq!(current(&restarted, &target).await, restored);
    let state = change(
        &restarted,
        "window.state",
        &json!({"hwnd":target["hwnd"],"state":"maximize"}),
    )
    .await;
    assert_eq!(
        current(&restarted, &target).await["placement"]["show_cmd"],
        3
    );
    undo_native(&restarted, &state).await;
    assert_eq!(current(&restarted, &target).await, restored);
    let snap = change(
        &restarted,
        "window.snap",
        &json!({"hwnd":target["hwnd"],"kind":"top_left"}),
    )
    .await;
    undo_native(&restarted, &snap).await;
    assert_eq!(current(&restarted, &target).await, restored);
    let monitors = restarted.monitors().await.unwrap();
    let destination = monitors.last().unwrap().id.clone();
    let monitor = change(
        &restarted,
        "window.move_monitor",
        &json!({"hwnd":target["hwnd"],"monitor":destination}),
    )
    .await;
    undo_native(&restarted, &monitor).await;
    assert_eq!(current(&restarted, &target).await, restored);
    // Moving/snap from non-normal states uses the restored bounds, then undo
    // restores the original minimized/maximized state as well as geometry.
    for state_name in ["minimize", "maximize"] {
        let state = change(
            &restarted,
            "window.state",
            &json!({"hwnd":target["hwnd"],"state":state_name}),
        )
        .await;
        let non_normal = current(&restarted, &target).await;
        let monitor = change(
            &restarted,
            "window.move_monitor",
            &json!({"hwnd":target["hwnd"],"monitor":destination}),
        )
        .await;
        let normal = current(&restarted, &target).await;
        assert_eq!(normal["placement"]["show_cmd"], 1);
        assert_eq!(normal["width"], non_normal["normal_screen"]["width"]);
        assert_eq!(normal["height"], non_normal["normal_screen"]["height"]);
        undo_native(&restarted, &monitor).await;
        assert_eq!(current(&restarted, &target).await, non_normal);
        let snap = change(
            &restarted,
            "window.snap",
            &json!({"hwnd":target["hwnd"],"kind":"top_left"}),
        )
        .await;
        undo_native(&restarted, &snap).await;
        assert_eq!(current(&restarted, &target).await, non_normal);
        undo_native(&restarted, &state).await;
        assert_eq!(current(&restarted, &target).await, restored);
    }
    let mut body = Vec::new();
    let mut before = Vec::new();
    for target in windows.info["windows"].as_array().unwrap() {
        let saved = current(&restarted, target).await;
        let mut placement = saved.clone();
        placement["x"] = json!(number(&saved, "x").unwrap() + 12);
        placement["y"] = json!(number(&saved, "y").unwrap() + 8);
        body.push(placement);
        before.push(saved);
    }
    let layout = change(&restarted, "window.set_layout", &json!({"body":body})).await;
    // A later change to the first member must stop the entire inverse set,
    // including the unchanged second member which would restore first.
    let mut observed_set = Vec::new();
    for target in windows.info["windows"].as_array().unwrap() {
        observed_set.push(current(&restarted, target).await);
    }
    let mut foreign_member = observed_set[0].clone();
    foreign_member["x"] = json!(number(&foreign_member, "x").unwrap() + 7);
    windows.external_move(&foreign_member).await;
    let foreign_member = current(&restarted, &windows.info["windows"][0]).await;
    assert!(
        execute_undo(&restarted, &json!({"id":layout["undo_token"]}))
            .await
            .unwrap_err()
            .contains("WINDOW_CHANGED_AFTER_ACTION")
    );
    assert_eq!(
        current(&restarted, &windows.info["windows"][0]).await,
        foreign_member
    );
    assert_eq!(
        current(&restarted, &windows.info["windows"][1]).await,
        observed_set[1]
    );
    windows.external_move(&observed_set[0]).await;
    undo_native(&restarted, &layout).await;
    for (target, saved) in windows.info["windows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(before)
    {
        let now = current(&restarted, target).await;
        let mut saved = saved;
        saved["window_identity"] = now["window_identity"].clone();
        assert_eq!(now, saved);
    }
    // The native operation succeeds, but SQLite refuses its observation.
    // It must not authorize automatic rollback from an in-memory result.
    restarted.store.0.lock().unwrap().execute_batch("CREATE TRIGGER owned_window_observation_failure BEFORE UPDATE ON undo_records WHEN NEW.kind='window_change' AND json_extract(NEW.metadata_json,'$.steps[0].state')='applied' BEGIN SELECT RAISE(FAIL,'OWNED_WINDOW_OBSERVATION_FAILURE'); END;").unwrap();
    let unknown = execute(
        &restarted,
        prepare(&restarted, "window.move", &move_again)
            .await
            .unwrap(),
    )
    .await
    .unwrap_err();
    assert!(unknown.contains("window_recovery_id="), "{unknown}");
    let unknown_after = current(&restarted, &target).await;
    assert_eq!(unknown_after["x"], move_again["x"]);
    let unknown_id = history(&restarted).unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        load(&restarted, &unknown_id).unwrap().0.steps[0].state,
        "applying"
    );
    restarted
        .store
        .0
        .lock()
        .unwrap()
        .execute_batch("DROP TRIGGER owned_window_observation_failure;")
        .unwrap();
    let recovery = Arc::new(Core::new(restarted.root.clone(), restarted.resource.clone()).unwrap());
    assert_eq!(load(&recovery, &unknown_id).unwrap().1, "blocked");
    assert!(
        execute_undo(&recovery, &json!({"id":unknown_id,"revalidate":true}))
            .await
            .unwrap_err()
            .contains("WINDOW_NATIVE_RESULT_UNKNOWN")
    );
    assert_eq!(current(&recovery, &target).await, unknown_after);
    windows.external_move(&restored).await;
    drop(recovery);
    windows.command("veto").await;
    let veto = json!({"hwnd":target["hwnd"],"x":number(&restored,"x").unwrap()+20,"y":restored["y"],"width":number(&restored,"width").unwrap()+30,"height":restored["height"]});
    let error = execute(
        &restarted,
        prepare(&restarted, "window.move", &veto).await.unwrap(),
    )
    .await
    .unwrap_err();
    assert!(
        error.contains("WINDOW_PROVIDER_CONSTRAINED_GEOMETRY"),
        "{error}"
    );
    assert_eq!(current(&restarted, &target).await, restored);
    let mut partial_targets = Vec::new();
    let mut partial_before = Vec::new();
    for index in [1, 0] {
        let saved = current(&restarted, &windows.info["windows"][index]).await;
        let mut placement = saved.clone();
        placement["x"] = json!(number(&saved, "x").unwrap() + 20);
        partial_before.push(saved);
        partial_targets.push(placement);
    }
    let partial = execute(
        &restarted,
        prepare(
            &restarted,
            "window.set_layout",
            &json!({"body":partial_targets}),
        )
        .await
        .unwrap(),
    )
    .await
    .unwrap_err();
    assert!(
        partial.contains("WINDOW_PROVIDER_CONSTRAINED_GEOMETRY"),
        "{partial}"
    );
    assert_eq!(history(&restarted).unwrap()[0]["state"], "undone");
    for (index, saved) in [1, 0].into_iter().zip(partial_before) {
        assert_eq!(
            current(&restarted, &windows.info["windows"][index]).await,
            saved
        );
    }
    windows.command("allow").await;
    let replacement = change(&restarted, "window.move", &move_again).await;
    windows.command("recreate").await;
    let new_target = windows.info["windows"][0].clone();
    assert_eq!(new_target["process_id"], target["process_id"]);
    let replacement_before = current(&restarted, &new_target).await;
    let stale_identity = json!({"hwnd":new_target["hwnd"],"process_id":target["process_id"],"process_started":restored["process_started"],"window_identity":restored["window_identity"]});
    assert_eq!(
        crate::window_snapshot::capture(&json!([stale_identity]), false).unwrap_err(),
        "WINDOW_IDENTITY_CHANGED"
    );
    assert!(
        execute_undo(&restarted, &json!({"id":replacement["undo_token"]}))
            .await
            .is_err()
    );
    assert_eq!(current(&restarted, &new_target).await, replacement_before);
    assert_eq!(history(&restarted).unwrap()[0]["state"], "blocked");
    windows.finish().await;
    drop(restarted);
    f.finish();
    println!("WINDOW_UNDO_REAL_CONTRACT_PASS: owned two-window native fixture/persisted restart/explicit L3 refusal/normal+minimized+maximized/snap+monitor+batch/whole-set later writer preflight/partial batch rollback/later writer preserved/provider veto and guarded rollback/failed durable observation stays blocked/same-process recreated window preserved; no human permission acceptance or desktop crash certified");
}

#[tokio::test]
async fn direct_window_snapshots_match_isolated_helper_and_reject_changed_identity() {
    let f = Fixture::new();
    let mut windows = Windows::new(&f.core).await;
    let target = windows.info["windows"][0].clone();
    let targets = json!([{"hwnd":target["hwnd"],"process_id":target["process_id"]}]);
    for state in [
        None,
        Some("minimize"),
        Some("restore"),
        Some("maximize"),
        Some("minimize"),
        Some("restore"),
    ] {
        if let Some(state) = state {
            change(
                &f.core,
                "window.state",
                &json!({"hwnd":target["hwnd"],"state":state}),
            )
            .await;
        }
        use windows::Win32::UI::HiDpi::{
            AreDpiAwarenessContextsEqual, GetThreadDpiAwarenessContext,
        };
        let dpi = unsafe { GetThreadDpiAwarenessContext() };
        let direct = crate::window_snapshot::capture(&targets, true).unwrap();
        assert!(
            unsafe { AreDpiAwarenessContextsEqual(dpi, GetThreadDpiAwarenessContext()) }.as_bool()
        );
        windows.command("report").await;
        let reference = &windows.info["windows"][0]["snapshot"];
        assert_eq!(
            direct[0], *reference,
            "independent WinForms snapshot parity for {state:?}"
        );
        assert_eq!(direct[0]["process_started"], target["process_started"]);
        for (field, value, expected) in [
            ("process_id", json!(u32::MAX), "WINDOW_TARGET_CHANGED"),
            ("process_id", json!("malformed"), "WINDOW_TARGET_CHANGED"),
            ("process_started", json!("1"), "LAYOUT_PROCESS_RECREATED"),
            ("process_started", json!(1), "LAYOUT_PROCESS_RECREATED"),
            (
                "window_identity",
                json!("0000000000000001"),
                "WINDOW_IDENTITY_CHANGED",
            ),
            ("window_identity", json!(1), "WINDOW_IDENTITY_CHANGED"),
        ] {
            let mut wrong = direct[0].clone();
            wrong[field] = value;
            assert_eq!(
                crate::window_snapshot::capture(&json!([wrong]), false).unwrap_err(),
                expected
            );
            assert!(
                unsafe { AreDpiAwarenessContextsEqual(dpi, GetThreadDpiAwarenessContext()) }
                    .as_bool()
            );
        }
    }
    assert_eq!(
        crate::window_snapshot::capture(&json!([targets[0], targets[0]]), false).unwrap_err(),
        "LAYOUT_TARGET"
    );
    windows.finish().await;
    f.finish();
}

#[tokio::test]
async fn native_worker_rejects_changed_requests_and_cancels_a_blocked_owned_window() {
    let f = Fixture::new();
    let mut windows = Windows::new(&f.core).await;
    let targets = json!([windows.info["windows"][0]]);
    let before = capture(&f.core, &targets, true, &Lease::new())
        .await
        .unwrap()
        .remove(0);
    let input = json!({"hwnd":before["hwnd"],"x":before["x"].as_i64().unwrap()+13,"y":before["y"],"width":before["width"],"height":before["height"]});
    let request = json!({"expected":before,"action":"move","input":input});
    let mut stale = request.clone();
    stale["expected"]["x"] = json!(before["x"].as_i64().unwrap() + 1);
    assert_eq!(
        process::window_change(&stale, CancellationToken::new())
            .await
            .unwrap_err(),
        "WINDOW_CHANGED_REVIEW_REQUIRED"
    );
    let mut malformed = request.clone();
    malformed["input"]["x"] = json!(i64::MIN);
    assert_eq!(
        process::window_change(&malformed, CancellationToken::new())
            .await
            .unwrap_err(),
        "WINDOW_BOUNDS"
    );
    let mut wrong = request.clone();
    wrong["input"]["hwnd"] = windows.info["windows"][1]["hwnd"].clone();
    assert_eq!(
        process::window_change(&wrong, CancellationToken::new())
            .await
            .unwrap_err(),
        "WINDOW_TARGET_CHANGED"
    );
    assert_eq!(
        capture(&f.core, &targets, false, &Lease::new())
            .await
            .unwrap()[0],
        before
    );
    windows.command("block_move").await;
    let cancel = CancellationToken::new();
    let signal = cancel.clone();
    let task = tokio::spawn(async move { process::window_change(&request, signal).await });
    let blocked = PathBuf::from(format!("{}.blocked", windows.metadata.display()));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !blocked.exists() {
        assert!(
            !task.is_finished(),
            "worker finished before entering the owned window procedure"
        );
        assert!(
            Instant::now() < deadline,
            "owned window never received the mutation"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let started = Instant::now();
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.unwrap_err(), "PROCESS_CANCELLED");
    assert!(started.elapsed() < Duration::from_secs(5));
    // A dispatched mutation has an unknown result after cancellation; never retry.
    std::fs::write(
        format!("{}.release", windows.metadata.display()),
        b"release",
    )
    .unwrap();
    windows.finish().await;
    f.finish();
}

#[path = "window_recovery_contracts.rs"]
pub(in crate::window_undo) mod recovery;
