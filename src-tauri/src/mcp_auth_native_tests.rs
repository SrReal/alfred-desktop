//! Actual owned child termination inside the MCP actor's native publication.
//! UUID-scoped WinCred, fictional tokens, stable original root, exact handles.
use super::tests::Fixture;
use super::*;
use std::{
    io::BufRead,
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};
fn authorization() -> Authorization {
    Authorization {
        issuer: "https://issuer.invalid/tenant".into(),
        resource: "https://mcp.invalid/mcp".into(),
        token_endpoint: "https://issuer.invalid/tenant/token".into(),
        client_id: "owned-native-crash-client".into(),
        client_auth: ClientAuth::None,
        requested_scopes: Some(BTreeSet::from(["files:read".into()])),
    }
}
fn body() -> Value {
    json!({"token_type":"Bearer","access_token":"owned".repeat(1600),"refresh_token":"owned-native-crash-refresh","expires_in":3600,"scope":"files:read"})
}
async fn fixture() -> Fixture {
    let f = Fixture::new().await;
    *f.core.mcp_oauth_vault.lock().unwrap() = None;
    let mut raw = record(&f.core, &f.id).unwrap();
    raw["auth"] = json!({"type":"oauth","issuer":"https://issuer.invalid/tenant"});
    integrations::save(
        &f.core,
        &json!({"id":f.id,"module_id":"mcp","name":"Owned native actor crash","config":raw}),
    )
    .await
    .unwrap();
    f
}
fn safe_root(path: &std::path::Path, identity: &str) {
    let target = path.canonicalize().unwrap();
    let parent = std::env::temp_dir().canonicalize().unwrap();
    assert_eq!(target.parent(), Some(parent.as_path()));
    canonical(
        target
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .strip_prefix("ap-mcp-auth-")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        asset_files::identity(&asset_files::directory(&target).unwrap()).unwrap(),
        identity
    );
}
fn kill_child(mut command: Command, boundary: usize) -> Result<()> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "OWNED_MCP_CHILD_START")?;
    let stdout = child.stdout.take().ok_or("OWNED_MCP_CHILD_STDOUT")?;
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(std::result::Result::ok)
        {
            if line.starts_with("OWNED_MCP_ACTOR_BOUNDARY:") {
                let _ = tx.send(line);
            }
        }
    });
    let signal = rx.recv_timeout(Duration::from_secs(30));
    let killed = child.kill();
    let reaped = child.wait();
    reader.join().map_err(|_| "OWNED_MCP_CHILD_READER")?;
    if signal.ok().as_deref() != Some(&format!("OWNED_MCP_ACTOR_BOUNDARY:{boundary}")) {
        return Err("OWNED_MCP_CHILD_BOUNDARY_MISSING".into());
    }
    killed.map_err(|_| "OWNED_MCP_CHILD_KILL")?;
    if reaped.map_err(|_| "OWNED_MCP_CHILD_REAP")?.success() {
        return Err("OWNED_MCP_CHILD_NOT_TERMINATED".into());
    }
    Ok(())
}
#[tokio::test]
async fn actual_owned_process_kills_at_every_native_actor_publication_boundary_never_adopt_partial_tokens(
) {
    let sizing = fixture().await;
    let a = Actor::new(&sizing.core, &sizing.id, authorization()).unwrap();
    let state = a.begin(false).unwrap();
    let state = a
        .transition(
            &state.session_id,
            Phase::Authorizing,
            Phase::ExchangeInFlight,
        )
        .unwrap();
    let packet = a
        .tokens(&state, body(), None, None)
        .unwrap()
        .bytes(&a)
        .unwrap();
    let count = vault::win::owned_mcp_publication_boundary_count(packet.bytes().len());
    drop(packet);
    drop(a);
    sizing.finish();
    assert!(count >= 6);
    for boundary in 1..=count {
        let f = fixture().await;
        safe_root(&f.root, &f.identity);
        let actor = Actor::new(&f.core, &f.id, authorization()).unwrap();
        let binding = actor.binding().unwrap();
        let context = actor.receipt_context();
        assert!(vault::win::observe(&binding).unwrap().is_none());
        assert!(vault::win::mcp_context_inventory(&binding, context)
            .unwrap()
            .is_empty());
        let nonce = storage::id();
        let marker = json!({"version":1,"connection":f.id,"profile":f.core.store.setting(PROFILE_KEY),"root_identity":f.identity,"nonce":nonce,"boundary":boundary});
        std::fs::write(f.root.join("owned-mcp-crash.json"), marker.to_string()).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--ignored",
                "--exact",
                "mcp_auth::native_tests::owned_mcp_actor_termination_helper",
                "--nocapture",
            ])
            .env("AP_MCP_ACTOR_TEST_ROOT", &f.root)
            .env("AP_MCP_ACTOR_TEST_NONCE", &nonce)
            .env("AP_MCP_ACTOR_TEST_BOUNDARY", boundary.to_string());
        let killed = kill_child(command, boundary);
        drop(actor);
        // Use the original on-disk Core again after the handle was reaped.
        // Native APIs must accept the abandoned Windows lease, never a new root.
        let restarted = Arc::new(Core::new(f.core.root.clone(), f.core.resource.clone()).unwrap());
        let summary = service::startup(&restarted).unwrap();
        assert_eq!(summary["reconciled"], 1);
        let actor = Actor::load(&restarted, &f.id).unwrap();
        let state = actor.state().unwrap();
        assert!(state.phase == Phase::Uncertain);
        let recover = actor.recover_local(&state);
        if boundary == count {
            assert!(recover.unwrap().phase == Phase::Ready);
            assert!(actor.grant().is_ok());
        } else {
            assert!(recover.is_err());
            assert!(actor.grant().is_err());
        }
        let state = actor.state().unwrap();
        let cleaned = actor.cleanup_receipts(&state).unwrap();
        assert!(!cleaned.cleanup_pending);
        if let Some(current) = vault::win::observe(&binding).unwrap() {
            assert!(vault::win::erase_context(&binding, current.version, context).unwrap());
        }
        for item in vault::win::mcp_context_inventory(&binding, context).unwrap() {
            if !item.recovery.retirement_verified {
                let fresh = vault::win::mcp_context_inventory(&binding, context)
                    .unwrap()
                    .into_iter()
                    .find(|r| r.recovery.version == item.recovery.version)
                    .unwrap();
                assert!(
                    vault::win::recover(&fresh.binding, Some(context), &fresh.recovery).unwrap()
                );
            }
        }
        vault::win::compact_mcp_context(&binding, context).unwrap();
        assert!(vault::win::observe(&binding).unwrap().is_none());
        assert!(vault::win::mcp_context_inventory(&binding, context)
            .unwrap()
            .is_empty());
        let public = f
            .core
            .store
            .query("SELECT json_value FROM settings", &[])
            .unwrap()
            .to_string();
        assert!(!public.contains("owned-native-crash-refresh"));
        assert!(!public.contains(&"owned".repeat(50)));
        killed.unwrap();
        drop(actor);
        drop(restarted);
        f.finish();
    }
}
#[test]
#[ignore = "Exact owned-process helper, invoked by the MCP actor termination test"]
fn owned_mcp_actor_termination_helper() {
    let root = PathBuf::from(std::env::var("AP_MCP_ACTOR_TEST_ROOT").expect("owned helper only"));
    let nonce = std::env::var("AP_MCP_ACTOR_TEST_NONCE").unwrap();
    canonical(&nonce).unwrap();
    let boundary: usize = std::env::var("AP_MCP_ACTOR_TEST_BOUNDARY")
        .unwrap()
        .parse()
        .unwrap();
    let marker: Value =
        serde_json::from_slice(&std::fs::read(root.join("owned-mcp-crash.json")).unwrap()).unwrap();
    assert_eq!(marker["nonce"], nonce);
    assert_eq!(marker["boundary"], boundary);
    assert_eq!(marker["version"], 1);
    safe_root(&root, marker["root_identity"].as_str().unwrap());
    let core = Arc::new(
        Core::new(
            root.join("profile"),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap(),
    );
    assert_eq!(core.store.setting(PROFILE_KEY), marker["profile"]);
    assert!(core.mcp_oauth_vault.lock().unwrap().is_none());
    let id = marker["connection"].as_str().unwrap();
    canonical(id).unwrap();
    let actor = Actor::new(&core, id, authorization()).unwrap();
    let _ownership = flow::ownership(&actor).unwrap();
    assert!(vault::win::observe(&actor.binding().unwrap())
        .unwrap()
        .is_none());
    assert!(
        vault::win::mcp_context_inventory(&actor.binding().unwrap(), actor.receipt_context())
            .unwrap()
            .is_empty()
    );
    let state = actor.begin(false).unwrap();
    let state = actor
        .transition(
            &state.session_id,
            Phase::Authorizing,
            Phase::ExchangeInFlight,
        )
        .unwrap();
    let packet = actor.tokens(&state, body(), None, None).unwrap();
    actor
        .transition(
            &state.session_id,
            Phase::ExchangeInFlight,
            Phase::PublicationPending,
        )
        .unwrap();
    let max = vault::win::owned_mcp_publication_boundary_count(
        packet.bytes(&actor).unwrap().bytes().len(),
    );
    assert!(boundary > 0 && boundary <= max);
    vault::win::install_owned_mcp_boundary(&actor.binding().unwrap(), boundary).unwrap();
    actor.publish(&state.session_id, packet).unwrap();
    panic!("owned native boundary did not pause");
}
