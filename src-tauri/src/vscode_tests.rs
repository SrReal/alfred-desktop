use super::*;
use axum::{
    body::Body,
    extract::{Request, State},
    http::Response,
    Router,
};
struct Fixture {
    core: Arc<Core>,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ap-vscode-owned-{}", storage::id()));
        std::fs::create_dir(&root).unwrap();
        let core = Arc::new(
            Core::new(
                root.join("core"),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        core.store
            .set_setting("allowed_roots", &json!([root]))
            .unwrap();
        Self { core, root }
    }
    fn cleanup(self) {
        let root = self.root.clone();
        drop(self);
        std::fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn strict_catalog_schema_and_action_permissions() {
    let c = Fixture::new();
    for tool in TOOLS {
        let v = jsonschema::validator_for(&schema(tool)).unwrap();
        assert!(!v.is_valid(&json!({"unknown":1})));
        assert_eq!(
            policy::classify(tool, &json!({})),
            if mutation(tool) {
                policy::Risk::Mandatory
            } else {
                policy::Risk::Free
            }
        );
        assert!(c.core.tools().iter().any(|t| t["id"] == *tool));
    }
    let id = storage::id();
    assert!(jsonschema::validator_for(&schema("vscode.command")).unwrap().is_valid(&json!({"session":id,"command":"workbench.action.showCommands","arguments":[],"fingerprint":"a".repeat(64)})));
    assert!(!jsonschema::validator_for(&schema("vscode.command")).unwrap().is_valid(&json!({"session":id,"command":"_internal","arguments":[{}],"fingerprint":"a".repeat(64)})));
    c.cleanup();
}
#[tokio::test]
async fn discovery_is_scoped_bounded_inert_and_unicode() {
    let c = Fixture::new();
    let dir = c.root.join(".vscode");
    std::fs::create_dir(&dir).unwrap();
    let raw = "/* owned */ {\"tasks\":[{\"command\":\"${command:never.execute}\"}],}";
    std::fs::write(dir.join("tasks.json"), raw).unwrap();
    std::fs::write(c.root.join("ü & 测试.code-workspace"), "{invalid}").unwrap();
    let value = c
        .core
        .execute("vscode.discover", json!({"path":c.root}))
        .await
        .unwrap();
    assert_eq!(
        value["items"][0]["tasksConfig"]["sha256"],
        hash(raw.as_bytes())
    );
    assert_eq!(
        value["items"][0]["workspaceFiles"][0]["contentsEvaluated"],
        false
    );
    assert!(!value.to_string().contains("never.execute"));
    assert!(c
        .core
        .execute("vscode.discover", json!({"path":std::env::temp_dir()}))
        .await
        .unwrap_err()
        .starts_with("PATH_SCOPE"));
    c.cleanup();
}
#[tokio::test]
async fn core_privacy_module_missing_session_and_schema_deny_before_dispatch() {
    let c = Fixture::new();
    assert_eq!(
        c.core.execute("vscode.run", json!({})).await.unwrap_err(),
        "INVALID_TOOL_INPUT"
    );
    c.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(
        c.core
            .execute("vscode.sessions", json!({}))
            .await
            .unwrap_err(),
        "PRIVACY_DENIED"
    );
    c.core.store.set_setting("privacy", &json!(false)).unwrap();
    assert_eq!(
        c.core
            .execute("vscode.commands", json!({"session":storage::id()}))
            .await
            .unwrap_err(),
        "VSCODE_SESSION_NOT_OWNED"
    );
    c.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='vscode'",
            &[],
        )
        .unwrap();
    assert_eq!(
        c.core
            .execute("vscode.discover", json!({}))
            .await
            .unwrap_err(),
        "TOOL_DISABLED_OR_UNKNOWN"
    );
    c.cleanup();
}
#[test]
fn scoped_handles_deny_revocation_and_core_does_not_cycle() {
    let c = Fixture::new();
    let scope = Scope::new(&c.core, c.root.to_str().unwrap(), true).unwrap();
    c.core
        .store
        .set_setting("allowed_roots", &json!([]))
        .unwrap();
    assert!(scope.check().unwrap_err().starts_with("PATH_SCOPE"));
    drop(scope);
    c.core
        .store
        .set_setting("allowed_roots", &json!([c.root]))
        .unwrap();
    let weak = Arc::downgrade(&c.core);
    let root = c.root.clone();
    drop(c);
    assert!(weak.upgrade().is_none());
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn vscode_run_requires_human_even_for_readonly_and_reuses_real_terminal() {
    let c = Fixture::new();
    let input = json!({"path":c.root,"command":"Write-Output 'owned á 测试'","timeout_seconds":10});
    assert_eq!(
        c.core
            .execute("vscode.run", input.clone())
            .await
            .unwrap_err(),
        "APP_MISSING"
    );
    assert!(c.core.runs.lock().unwrap().is_empty());
    let p = prepare(&c.core, "vscode.run", &input).await.unwrap();
    assert_eq!(p.risk, policy::Risk::Mandatory);
    assert_eq!(p.review()["module"], "vscode");
    // Running through VS Code retains Terminal's independent module control.
    assert_eq!(execute(p).await.unwrap_err(), "MODULE_DISABLED");
    c.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=1 WHERE module_id='terminal'",
            &[],
        )
        .unwrap();
    let p = prepare(&c.core, "vscode.run", &input).await.unwrap();
    let output = execute(p).await.unwrap();
    assert!(output.to_string().contains("owned á 测试"));
    assert!(c.core.runs.lock().unwrap().is_empty());
    // Terminal aborts its privacy watcher on return. Let the runtime dispose
    // the aborted task and release its Core before deleting the owned profile.
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while Arc::strong_count(&c.core) > 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Terminal watcher must release the owned profile");
    c.cleanup();
}
#[test]
fn durable_audit_omits_command_task_arguments_and_diagnostic_content() {
    let c = Fixture::new();
    let input = json!({"session":storage::id(),"command":"owned private command","arguments":["owned private argument"],"task_review":{"name":"owned private task"}});
    let output =
        json!({"items":[{"message":"owned private diagnostic"}],"stdout":"owned private stdout"});
    c.core
        .store
        .audit(
            "vscode.command",
            "Mandatory",
            &input,
            "succeeded",
            &output,
            0,
        )
        .unwrap();
    let value=c.core.store.query("SELECT input_summary,output_summary FROM activity_events WHERE tool_id='vscode.command'",&[]).unwrap().to_string();
    assert!(!value.contains("owned private"));
    assert!(value.contains("VSCODE_CONTENT_OMITTED"));
    c.cleanup();
}
struct Mock {
    requests: Mutex<Vec<Value>>,
    delay: AtomicBool,
    malformed: AtomicBool,
}
async fn handler(State(m): State<Arc<Mock>>, request: Request) -> Response<Body> {
    assert_eq!(
        request.headers()["authorization"],
        "Bearer owned_fixture_only"
    );
    let bytes = axum::body::to_bytes(request.into_body(), 64 * 1024)
        .await
        .unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    m.requests.lock().unwrap().push(v.clone());
    if m.delay.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
    if m.malformed.load(Ordering::SeqCst) {
        return Response::new(Body::from("{invalid}"));
    }
    Response::new(Body::from(
        json!({"id":v["id"],"session":v["session"],"ok":true,"data":{"fixture":true}}).to_string(),
    ))
}
async fn mock_session(c: &Fixture) -> (Arc<Session>, Arc<Mock>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let m = Arc::new(Mock {
        requests: Mutex::new(vec![]),
        delay: AtomicBool::new(false),
        malformed: AtomicBool::new(false),
    });
    let app = Router::new()
        .route("/v1", axum::routing::post(handler))
        .with_state(m.clone());
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let p = c.root.join(format!("owned-bootstrap-{}", storage::id()));
    let bootstrap = assets::create(&p).unwrap();
    let session = Arc::new(Session {
        id: storage::id(),
        scope: Scope::new(&c.core, c.root.to_str().unwrap(), true).unwrap(),
        token: "owned_fixture_only".into(),
        port,
        revoked: AtomicBool::new(false),
        observations: Mutex::new(HashMap::new()),
        _private: vec![],
        bootstrap,
    });
    c.core
        .vscode_sessions
        .lock()
        .unwrap()
        .insert(session.id.clone(), session.clone());
    (session, m, handle)
}
#[tokio::test]
async fn cached_observation_is_required_and_mandatory_headless_does_not_send_action() {
    let c = Fixture::new();
    let (s, m, server) = mock_session(&c).await;
    let input = json!({"session":s.id,"command":"owned.fixture","arguments":[],"fingerprint":"a".repeat(64)});
    assert_eq!(
        c.core
            .execute("vscode.command", input.clone())
            .await
            .unwrap_err(),
        "VSCODE_OBSERVATION_REQUIRED"
    );
    s.observations.lock().unwrap().insert(
        "commands".into(),
        json!({"fingerprint":"a".repeat(64),"commands":["owned.fixture"]}),
    );
    assert_eq!(
        c.core
            .execute("vscode.command", input.clone())
            .await
            .unwrap_err(),
        "APP_MISSING"
    );
    assert!(m.requests.lock().unwrap().is_empty());
    let mut stale = input;
    stale["fingerprint"] = json!("b".repeat(64));
    assert_eq!(
        prepare(&c.core, "vscode.command", &stale)
            .await
            .err()
            .unwrap(),
        "VSCODE_OBSERVATION_CHANGED"
    );
    server.abort();
    drop(s);
    c.cleanup();
}
#[tokio::test]
async fn malformed_mutation_response_is_uncertain_and_never_replayed() {
    let c = Fixture::new();
    let (s, m, server) = mock_session(&c).await;
    m.malformed.store(true, Ordering::SeqCst);
    assert_eq!(
        s.request(
            "command",
            &json!({"command":"owned.fixture","arguments":[],"fingerprint":"a".repeat(64)})
        )
        .await
        .unwrap_err(),
        "VSCODE_ACTION_OUTCOME_UNCERTAIN: VSCODE_RESPONSE_JSON"
    );
    assert_eq!(m.requests.lock().unwrap().len(), 1);
    server.abort();
    drop(s);
    c.cleanup();
}
#[tokio::test]
async fn invalid_success_payload_is_uncertain_without_replay() {
    let c = Fixture::new();
    let (s, m, server) = mock_session(&c).await;
    assert_eq!(
        s.request(
            "command",
            &json!({"command":"owned.fixture","arguments":[],"fingerprint":"a".repeat(64)})
        )
        .await
        .unwrap_err(),
        "VSCODE_ACTION_OUTCOME_UNCERTAIN: VSCODE_RESPONSE_SCHEMA"
    );
    assert_eq!(m.requests.lock().unwrap().len(), 1);
    server.abort();
    drop(s);
    c.cleanup();
}
#[tokio::test]
async fn privacy_aborts_inflight_scoped_read() {
    let c = Fixture::new();
    let (s, m, server) = mock_session(&c).await;
    m.delay.store(true, Ordering::SeqCst);
    let t = s.clone();
    let operation = tokio::spawn(async move { t.request("diagnostics", &json!({})).await });
    for _ in 0..100 {
        if !m.requests.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(m.requests.lock().unwrap().len(), 1);
    c.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), operation)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        "PRIVACY_DENIED"
    );
    server.abort();
    drop(s);
    c.cleanup();
}
#[tokio::test]
#[ignore = "Opt-in: launches installed VS Code only in an owned UUID profile; no real editor profile"]
async fn owned_installed_vscode_editor_lifecycle() {
    let c = Fixture::new();
    println!(
        "VSCODE_OWNED_FIXTURE: {}",
        c.root.file_name().unwrap().to_string_lossy()
    );
    std::fs::write(c.root.join("owned.ts"), "const owned = 1;\n").unwrap();
    let fixture_extension = c.root.join("fixture-extension");
    std::fs::create_dir(&fixture_extension).unwrap();
    let source =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/vscode-extension");
    for name in ["package.json", "extension.cjs"] {
        std::fs::copy(source.join(name), fixture_extension.join(name)).unwrap();
    }
    *c.core.vscode_fixture.lock().unwrap() = Some(fixture_extension);
    let runtime = c.core.resource.join(".work/runtime/node.exe");
    assert!(runtime.is_file());
    std::fs::create_dir(c.root.join(".vscode")).unwrap();
    let tasks = json!({"version":"2.0.0","tasks":[
        {"label":"owned exit seven","type":"process","command":runtime,"args":["-e","require('fs').writeFileSync(process.argv[1], 'owned task á 测试'); process.exit(7)",c.root.join("task-observed.txt")],"problemMatcher":[],"runOptions":{"runOn":"default"}},
        {"label":"owned cancel","type":"process","command":runtime,"args":["-e","setInterval(()=>{},1000)"],"problemMatcher":[]}
    ]});
    std::fs::write(c.root.join(".vscode/tasks.json"), tasks.to_string()).unwrap();
    let launched = execute(
        prepare(&c.core, "vscode.open", &json!({"path":c.root}))
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(launched["managedProfile"], true);
    assert_eq!(launched["trusted"], true);
    let id = launched["session"].as_str().unwrap().to_owned();
    let s = session(&c.core, &id).unwrap();
    let commands = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let value = execute(
                prepare(&c.core, "vscode.commands", &json!({"session":id}))
                    .await
                    .unwrap(),
            )
            .await
            .unwrap();
            if value["commands"]
                .as_array()
                .unwrap()
                .iter()
                .any(|x| x == "alfredpet.fixture.touch")
            {
                break value;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("Owned fixture command activation timeout");
    let action = json!({"session":id,"command":"alfredpet.fixture.touch","arguments":[],"fingerprint":commands["fingerprint"]});
    let result = execute(prepare(&c.core, "vscode.command", &action).await.unwrap())
        .await
        .unwrap();
    assert_eq!(result["resultExposed"], false);
    assert_eq!(
        std::fs::read_to_string(c.root.join("command-observed.txt")).unwrap(),
        "owned command á 测试"
    );
    let diagnostics = execute(
        prepare(&c.core, "vscode.diagnostics", &json!({"session":id}))
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(diagnostics["complete"], false);
    assert!(diagnostics["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["source"] == "AlfredPet owned fixture"
            && d["message"] == "Owned fixture diagnostic á 测试"));
    let opened = execute(
        prepare(
            &c.core,
            "vscode.open",
            &json!({"session":id,"path":c.root.join("owned.ts"),"line":1,"column":2}),
        )
        .await
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(opened["opened"], true);
    let observed = execute(
        prepare(&c.core, "vscode.tasks", &json!({"session":id}))
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    let seven = observed["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "owned exit seven")
        .unwrap()["id"]
        .clone();
    let attempt = execute(
        prepare(
            &c.core,
            "vscode.run_task",
            &json!({"session":id,"task":seven,"fingerprint":observed["fingerprint"]}),
        )
        .await
        .unwrap(),
    )
    .await;
    if let Err(error) = &attempt {
        let current = s.request("tasks", &json!({})).await.unwrap();
        println!(
            "VSCODE_OWNED_TASK_FINGERPRINT_DIAGNOSTIC: {}",
            json!({"error":error,"expected":observed["fingerprintDetails"],"actual":current["fingerprintDetails"]})
        );
        let commands = s.request("commands", &json!({})).await.unwrap();
        let _=s.request("command",&json!({"command":"workbench.action.closeWindow","arguments":[],"fingerprint":commands["fingerprint"]})).await;
    }
    let start = attempt.unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let value = s
                .request("task_status", &json!({"run":start["id"]}))
                .await
                .unwrap();
            if value["state"] == "ended" {
                break value;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("Owned task completion timeout");
    assert_eq!(ended["exitCode"], 7);
    assert_eq!(ended["terminalOutputExposed"], false);
    assert_eq!(
        std::fs::read_to_string(c.root.join("task-observed.txt")).unwrap(),
        "owned task á 测试"
    );
    let observed = execute(
        prepare(&c.core, "vscode.tasks", &json!({"session":id}))
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    let cancel = observed["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "owned cancel")
        .unwrap()["id"]
        .clone();
    let start = execute(
        prepare(
            &c.core,
            "vscode.run_task",
            &json!({"session":id,"task":cancel,"fingerprint":observed["fingerprint"]}),
        )
        .await
        .unwrap(),
    )
    .await
    .unwrap();
    let cancelled = execute(
        prepare(
            &c.core,
            "vscode.task_cancel",
            &json!({"session":id,"run":start["id"]}),
        )
        .await
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(cancelled["cancellationRequested"], true);
    for _ in 0..100 {
        if s.request("task_status", &json!({"run":start["id"]}))
            .await
            .unwrap()["state"]
            == "ended"
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        s.request("task_status", &json!({"run":start["id"]}))
            .await
            .unwrap()["state"],
        "ended"
    );
    // This fixture opened an on-disk file and never edited it in the editor.
    // Close only the authenticated owned window, retaining an uncertain result.
    let commands = s.request("commands", &json!({})).await.unwrap();
    let closed=s.request("command",&json!({"command":"workbench.action.closeWindow","arguments":[],"fingerprint":commands["fingerprint"]})).await;
    assert!(
        closed.is_ok()
            || closed
                .as_ref()
                .unwrap_err()
                .starts_with("VSCODE_ACTION_OUTCOME_UNCERTAIN")
    );
    s.revoked.store(true, Ordering::SeqCst);
    let retired = Arc::downgrade(&s);
    c.core.vscode_sessions.lock().unwrap().clear();
    drop(s);
    tokio::time::timeout(Duration::from_secs(12), async {
        while retired.strong_count() > 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("Owned session handles not released");
    c.cleanup();
    println!("VSCODE_OWNED_NATIVE: managed CLI profile + companion, observed command/Unicode file, native TaskExecution exit=7/cancel, scoped available diagnostics and fixture cleanup PASS; human L3 and real profile acceptance NOT_RUN");
}
