use super::*;
async fn inspect(f: &Fixture, id: &Value) -> Value {
    f.core
        .execute(
            "drive.download",
            f.input(json!({"action":"inspect","job_id":id})),
        )
        .await
        .unwrap()
}
fn deletion(v: &Value) -> Value {
    json!({"action":"delete","job_id":v["job"]["id"],"expected":{"job_fingerprint":v["job"]["job_fingerprint"]}})
}
fn partial(f: &Fixture, bytes: &[u8]) -> (String, PathBuf) {
    let id = storage::id();
    let scope = Scope::new(&f.core, &f.id).unwrap();
    let mut destination = local::Destination::new(&f.core, &id).unwrap();
    let initial = destination.initial(&scope).unwrap();
    local::insert(
        &f.core,
        &f.id,
        &id,
        "download",
        "receiving",
        &json!({"initial":initial,"local":{"id":id},"requested_at":storage::now()}),
    )
    .unwrap();
    destination.write(bytes).unwrap();
    // Retain this owned fixture to model stopped bytes; the separate process
    // contract below supplies real termination evidence at all five barriers.
    destination.retain();
    drop(destination);
    let path = f
        .core
        .root
        .join("temp")
        .join(format!("drive-download-{id}.bin"));
    (id, path)
}
#[tokio::test]
async fn interrupted_download_inspection_is_offline_mandatory_and_redacted() {
    let f = Fixture::new(true, false).await;
    let (id, path) = partial(&f, b"owned unverified partial");
    drive_auth::expire_owned_grant(&f.core, &f.id).unwrap();
    let seen = f.m.lock().unwrap().seen.len();
    let v = inspect(&f, &json!(id)).await;
    assert_eq!(v["job"]["state"], "interrupted");
    assert_eq!(v["provider_completion_verified"], false);
    assert_eq!(v["delete_available"], true);
    assert_eq!(
        v["job"]["record"]["local"]["sha256"],
        format!("{:x}", Sha256::digest(b"owned unverified partial"))
    );
    assert_eq!(
        f.approve("drive.download", deletion(&v), "deny")
            .await
            .unwrap_err(),
        "PERMISSION_DENIED_OR_TIMEOUT"
    );
    assert!(path.exists());
    assert_eq!(
        f.approve("drive.download", deletion(&v), "allow")
            .await
            .unwrap()["state"],
        "deleted"
    );
    assert!(!path.exists());
    assert_eq!(seen, f.m.lock().unwrap().seen.len());
    let audit=f.core.store.query("SELECT risk,input_summary,output_summary FROM activity_events WHERE tool_id='drive.download'",&[]).unwrap();
    let text = audit.to_string();
    assert!(!text.contains("owned unverified partial"));
    assert!(!text.contains("drive-download-"));
    assert!(audit
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["risk"] == "Mandatory"));
    f.clean();
}
#[tokio::test]
async fn download_recovery_preserves_active_unknown_replaced_and_changed_bytes() {
    let f = Fixture::new(true, false).await;
    let (id, path) = partial(&f, b"original partial");
    let lease = local::job_lease(&f.core, &id).unwrap();
    let busy = inspect(&f, &json!(id)).await;
    assert_eq!(busy["job"]["state"], "receiving");
    assert_eq!(busy["delete_available"], false);
    assert_eq!(busy["active_or_lock_unverified"], true);
    drop(lease);
    let v = inspect(&f, &json!(id)).await;
    let input = f.input(deletion(&v));
    let prepared = prepare_operation(&f.core, "drive.download", &input)
        .await
        .unwrap();
    std::fs::write(&path, b"changed partial!").unwrap();
    assert_eq!(
        execute_operation(prepared).await.unwrap_err(),
        "DRIVE_DOWNLOAD_CHANGED"
    );
    let changed = inspect(&f, &json!(id)).await;
    assert_eq!(changed["delete_available"], false);
    assert!(path.exists());
    // A failed journal may already retain a final observed hash (failure
    // after ready publication). Recovery must not silently resnapshot changed
    // bytes merely because the state is failed instead of interrupted.
    let row = local::job(&f.core, &f.id, &id, "download").unwrap();
    local::update(&f.core, &row, "failed", &row["record"]).unwrap();
    let failed = inspect(&f, &json!(id)).await;
    assert_eq!(failed["delete_available"], false);
    assert_eq!(failed["job"]["state"], "failed");
    assert_eq!(failed["diagnostic"], "DRIVE_DOWNLOAD_CHANGED");
    let (replacement_id, replacement_path) = partial(&f, b"owned");
    std::fs::remove_file(&replacement_path).unwrap();
    let file = crate::asset_files::create(&replacement_path).unwrap();
    drop(file);
    let replaced = inspect(&f, &json!(replacement_id)).await;
    assert_eq!(replaced["delete_available"], false);
    assert_eq!(replaced["diagnostic"], "DRIVE_DOWNLOAD_CHANGED");
    let unknown = storage::id();
    let unknown_path = f
        .core
        .root
        .join("temp")
        .join(format!("drive-download-{unknown}.bin"));
    let file = crate::asset_files::create(&unknown_path).unwrap();
    drop(file);
    local::insert(
        &f.core,
        &f.id,
        &unknown,
        "download",
        "receiving",
        &json!({"local":{"id":unknown}}),
    )
    .unwrap();
    let unknown_result = inspect(&f, &json!(unknown)).await;
    assert_eq!(
        unknown_result["diagnostic"],
        "DRIVE_DOWNLOAD_INITIAL_EVIDENCE_REQUIRED"
    );
    assert_eq!(unknown_result["job"]["state"], "receiving");
    assert!(unknown_path.exists());
    f.clean();
}
#[tokio::test]
async fn local_download_guards_profile_privacy_connection_schema_and_stale_review() {
    let f = Fixture::new(true, false).await;
    let (id, path) = partial(&f, b"owned");
    let v = inspect(&f, &json!(id)).await;
    // Keep the input owned while awaiting preparation.
    let input = f.input(deletion(&v));
    let prepared = prepare_operation(&f.core, "drive.download", &input)
        .await
        .unwrap();
    let origin = f.core.store.setting("drive.profile_identity");
    f.core
        .store
        .set_setting("drive.profile_identity", &json!(storage::id()))
        .unwrap();
    assert_eq!(
        execute_operation(prepared).await.unwrap_err(),
        "DRIVE_DOWNLOAD_PROFILE_CHANGED"
    );
    f.core
        .store
        .set_setting("drive.profile_identity", &origin)
        .unwrap();
    let prepared = prepare_operation(&f.core, "drive.download", &input)
        .await
        .unwrap();
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(
        execute_operation(prepared).await.unwrap_err(),
        "PRIVACY_DENIED"
    );
    f.core.store.set_setting("privacy", &json!(false)).unwrap();
    let prepared = prepare_operation(&f.core, "drive.download", &input)
        .await
        .unwrap();
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='drive'",
            &[],
        )
        .unwrap();
    assert!(execute_operation(prepared)
        .await
        .unwrap_err()
        .starts_with("MODULE_DISABLED"));
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=1 WHERE module_id='drive'",
            &[],
        )
        .unwrap();
    let prepared = prepare_operation(&f.core, "drive.download", &input)
        .await
        .unwrap();
    let row = local::job(&f.core, &f.id, &id, "download").unwrap();
    local::update(&f.core, &row, "interrupted", &row["record"]).unwrap();
    assert_eq!(
        execute_operation(prepared).await.unwrap_err(),
        "DRIVE_STALE_JOB_OBSERVATION"
    );
    let invalid = f.input(json!({"action":"inspect","job_id":id,"file_id":"owned_file"}));
    assert!(prepare_operation(&f.core, "drive.download", &invalid)
        .await
        .is_err());
    assert!(path.exists());
    f.clean();
}
#[tokio::test]
async fn local_download_after_client_change_and_legacy_ready_contract() {
    let f = Fixture::new(true, false).await;
    let meta = f.meta("owned_file").await;
    let downloaded=execute(f.prepare("drive.download",json!({"file_id":"owned_file","expected":{"file_fingerprint":meta["file_fingerprint"]}})).await).await.unwrap();
    let mut config = Scope::new(&f.core, &f.id).unwrap().config;
    config["client_id"] = json!("owned-new-client.apps.googleusercontent.com");
    crate::integrations::save(&f.core,&json!({"id":f.id,"module_id":"drive","name":"Owned fixture changed","config":config,"secret":"","reauthorize":true})).await.unwrap();
    let seen = f.m.lock().unwrap().seen.len();
    let v = inspect(&f, &downloaded["id"]).await;
    assert_eq!(v["provider_completion_verified"], true);
    assert_eq!(v["delete_available"], true);
    let row = local::job(
        &f.core,
        &f.id,
        downloaded["id"].as_str().unwrap(),
        "download",
    )
    .unwrap();
    let mut record = row["record"].clone();
    record.as_object_mut().unwrap().remove("initial");
    local::update(&f.core, &row, "ready", &record).unwrap();
    let legacy = inspect(&f, &downloaded["id"]).await;
    assert_eq!(legacy["diagnostic"], "legacy_origin_unverified");
    assert_eq!(legacy["delete_available"], true);
    f.approve("drive.download", deletion(&legacy), "allow")
        .await
        .unwrap();
    assert_eq!(seen, f.m.lock().unwrap().seen.len());
    f.clean();
}
#[tokio::test]
async fn forged_download_origin_paths_links_and_missing_files_are_preserved() {
    let f = Fixture::new(true, false).await;
    for mode in ["origin", "initial_path", "local_path", "link", "missing"] {
        let (id, path) = partial(&f, b"owned");
        let row = local::job(&f.core, &f.id, &id, "download").unwrap();
        let mut record = row["record"].clone();
        match mode {
            "origin" => record["initial"]["origin"]["connection"] = json!(storage::id()),
            "initial_path" => {
                record["initial"]["path"] = json!(f.root.join("workspace/foreign.bin"))
            }
            "local_path" => record["local"]["path"] = json!(f.root.join("workspace/foreign.bin")),
            "link" => std::fs::hard_link(&path, f.root.join("workspace/owned-link.bin")).unwrap(),
            "missing" => std::fs::remove_file(&path).unwrap(),
            _ => unreachable!(),
        }
        local::update(&f.core, &row, "receiving", &record).unwrap();
        let v = inspect(&f, &json!(id)).await;
        assert_eq!(v["delete_available"], false, "{mode}");
        assert_eq!(v["job"]["state"], "receiving", "{mode}");
        assert_ne!(v["diagnostic"], "verified_local_identity", "{mode}");
        if mode != "missing" {
            assert!(path.exists());
        }
    }
    f.clean();
}
fn kill_at_barrier(mut command: std::process::Command, phase: &str) -> Result<String> {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    command.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = command.spawn().map_err(|_| "OWNED_CHILD_SPAWN")?;
    let stdout = child.stdout.take().ok_or("OWNED_CHILD_STDOUT")?;
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().take(256) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut signal = None;
    let prefix = format!("OWNED_DOWNLOAD_BARRIER:{phase}:");
    while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
        match rx.recv_timeout(remaining) {
            Ok(Ok(line)) if line.starts_with(&prefix) => {
                signal = Some(line[prefix.len()..].to_string());
                break;
            }
            Ok(Ok(_)) => (),
            _ => break,
        }
    }
    // Only this explicitly spawned child handle is terminated and reaped.
    let killed = child.kill();
    let status = child.wait().map_err(|_| "OWNED_CHILD_WAIT")?;
    reader.join().map_err(|_| "OWNED_CHILD_READER")?;
    killed.map_err(|_| "OWNED_CHILD_TERMINATE")?;
    if status.success() {
        return Err("OWNED_CHILD_NOT_TERMINATED".into());
    }
    signal.ok_or("OWNED_DOWNLOAD_BARRIER_MISSING".into())
}
#[tokio::test]
async fn download_delete_journal_failure_remains_uncertain_and_does_not_adopt_missing() {
    let f = Fixture::new(true, false).await;
    let (id, path) = partial(&f, b"owned journal fixture");
    let v = inspect(&f, &json!(id)).await;
    let input = f.input(deletion(&v));
    let prepared = prepare_operation(&f.core, "drive.download", &input)
        .await
        .unwrap();
    f.core.store.execute("CREATE TRIGGER owned_download_update_failure BEFORE UPDATE ON drive_jobs BEGIN SELECT RAISE(ABORT,'owned interrupted journal'); END", &[]).unwrap();
    assert_eq!(
        execute_operation(prepared).await.unwrap_err(),
        "DRIVE_LOCAL_DELETE_JOURNAL_OUTCOME_UNCERTAIN"
    );
    assert!(!path.exists());
    f.core
        .store
        .execute("DROP TRIGGER owned_download_update_failure", &[])
        .unwrap();
    let later = inspect(&f, &json!(id)).await;
    assert_eq!(later["job"]["state"], "interrupted");
    assert_eq!(later["diagnostic"], "DRIVE_DOWNLOAD_MISSING");
    assert_eq!(later["delete_available"], false);
    assert_eq!(later["provider_completion_verified"], false);
    f.clean();
}
#[tokio::test]
async fn changed_download_acl_connection_and_lease_after_review_preserve_file() {
    let f = Fixture::new(true, false).await;
    let (id, path) = partial(&f, b"owned ACL fixture");
    let v = inspect(&f, &json!(id)).await;
    let input = f.input(deletion(&v));
    let prepared = prepare_operation(&f.core, "drive.download", &input)
        .await
        .unwrap();
    let lease = local::job_lease(&f.core, &id).unwrap();
    assert_eq!(
        execute_operation(prepared).await.unwrap_err(),
        "DRIVE_JOB_BUSY_OR_LOCK_CHANGED"
    );
    drop(lease);
    let prepared = prepare_operation(&f.core, "drive.download", &input)
        .await
        .unwrap();
    let key = format!("drive.connection_generation.{}", f.id);
    let generation = f.core.store.setting(&key);
    f.core
        .store
        .set_setting(&key, &json!(storage::id()))
        .unwrap();
    assert_eq!(
        execute_operation(prepared).await.unwrap_err(),
        "DRIVE_CONNECTION_CHANGED"
    );
    f.core.store.set_setting(&key, &generation).unwrap();
    let prepared = prepare_operation(&f.core, "drive.download", &input)
        .await
        .unwrap();
    // This single UUID-owned file belongs to this fixture. Enabling inheritance
    // removes its protected-DACL contract while retaining current-user access.
    let status = std::process::Command::new("icacls.exe")
        .arg(&path)
        .arg("/inheritance:e")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        execute_operation(prepared).await.unwrap_err(),
        "ASSET_DACL_NOT_PRIVATE"
    );
    let later = inspect(&f, &json!(id)).await;
    assert_eq!(later["delete_available"], false);
    assert_eq!(later["diagnostic"], "ASSET_DACL_NOT_PRIVATE");
    assert!(path.exists());
    f.clean();
}
#[tokio::test]
async fn owned_process_termination_at_five_download_boundaries_recovers_without_http() {
    for phase in ["create", "journal", "first_chunk", "fsync", "ready"] {
        let f = Fixture::new(true, true).await;
        {
            let mut m = f.m.lock().unwrap();
            m.download = vec![79; 2 * 1024 * 1024 + 17];
            let bytes = m.download.clone();
            m.files.insert(
                "owned_file".into(),
                raw_file(
                    "owned_file",
                    "Owned large binary",
                    "application/octet-stream",
                    &["owned_folder"],
                    Some(&bytes),
                ),
            );
        }
        let nonce = storage::id();
        f.core
            .store
            .set_setting("drive.owned_process_fixture", &json!(nonce))
            .unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--ignored",
                "--exact",
                "drive::tests::download_recovery::owned_download_termination_helper",
                "--nocapture",
            ])
            .env("AP_DRIVE_OWNED_ROOT", &f.core.root)
            .env("AP_DRIVE_OWNED_NONCE", &nonce)
            .env("AP_DRIVE_OWNED_CONNECTION", &f.id)
            .env("AP_DRIVE_OWNED_BASE", f.m.lock().unwrap().base.clone())
            .env("AP_DRIVE_DOWNLOAD_PHASE", phase);
        let killed = tokio::task::spawn_blocking(move || kill_at_barrier(command, phase))
            .await
            .unwrap();
        let id = match killed {
            Ok(id) => id,
            Err(e) => {
                f.clean();
                panic!("{phase}: {e}");
            }
        };
        let restarted = Arc::new(
            Core::new(
                f.core.root.clone(),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        let before = f.m.lock().unwrap().seen.len();
        let listed = restarted
            .execute("drive.download", json!({"connection":f.id,"action":"list"}))
            .await
            .unwrap();
        assert_eq!(before, f.m.lock().unwrap().seen.len(), "{phase}");
        let path = f
            .core
            .root
            .join("temp")
            .join(format!("drive-download-{id}.bin"));
        assert!(path.exists(), "{phase}");
        if phase == "create" {
            assert!(listed["jobs"].as_array().unwrap().is_empty());
            assert!(local::job(&restarted, &f.id, &id, "download").is_err());
            // A file without a published initial observation stays unknown.
        } else {
            let v = &listed["jobs"][0];
            assert_eq!(v["job"]["id"], id);
            assert_eq!(
                v["job"]["state"],
                if phase == "ready" {
                    "ready"
                } else {
                    "interrupted"
                }
            );
            assert_eq!(v["provider_completion_verified"], phase == "ready");
            assert_eq!(v["delete_available"], true);
            if phase == "first_chunk" {
                let size = v["local_bytes_observed"]["size"].as_u64().unwrap();
                assert!(size > 0 && size < f.m.lock().unwrap().download.len() as u64);
            }
            if phase == "journal" {
                assert_eq!(v["local_bytes_observed"]["size"], 0);
            }
            if ["fsync", "ready"].contains(&phase) {
                assert_eq!(std::fs::read(&path).unwrap(), f.m.lock().unwrap().download);
            }
            let prepared = prepare_operation(&restarted, "drive.download", &f.input(deletion(v)))
                .await
                .unwrap();
            assert!(matches!(prepared.risk(), policy::Risk::Mandatory));
            // The permission contract is covered separately with Core.execute.
            assert_eq!(
                execute_operation(prepared).await.unwrap()["state"],
                "deleted"
            );
            assert!(!path.exists());
        }
        assert_eq!(before, f.m.lock().unwrap().seen.len(), "{phase}");
        drop(restarted);
        f.clean();
    }
}
#[tokio::test]
#[ignore = "Exact UUID-owned child killed by the download boundary contract"]
async fn owned_download_termination_helper() {
    let root = PathBuf::from(std::env::var("AP_DRIVE_OWNED_ROOT").unwrap());
    let canonical = root.canonicalize().unwrap();
    let outer = canonical.parent().unwrap();
    assert_eq!(
        outer.parent(),
        Some(std::env::temp_dir().canonicalize().unwrap().as_path())
    );
    assert!(outer
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("ap-drive-files-"));
    let core =
        Arc::new(Core::new(root, PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap());
    assert_eq!(
        core.store.setting("drive.owned_process_fixture"),
        std::env::var("AP_DRIVE_OWNED_NONCE").unwrap()
    );
    let connection = std::env::var("AP_DRIVE_OWNED_CONNECTION").unwrap();
    let base = std::env::var("AP_DRIVE_OWNED_BASE").unwrap();
    let url = reqwest::Url::parse(&base).unwrap();
    assert_eq!(url.scheme(), "http");
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    *core.drive_fixture.lock().unwrap() = Some(drive_auth::TestTransport {
        token_url: format!("{base}/token"),
        api_base: format!("{base}/"),
        vault: Arc::new(Mutex::new(crate::drive_credentials::TestVault::default())),
        timeout_ms: 5000,
        native_storage: true,
        headless_permission: true,
    });
    let meta = execute(
        prepare(
            &core,
            "drive.metadata",
            &json!({"connection":connection,"file_id":"owned_file"}),
        )
        .await
        .unwrap(),
    )
    .await
    .unwrap();
    let p=prepare(&core,"drive.download",&json!({"connection":connection,"file_id":"owned_file","expected":{"file_fingerprint":meta["file_fingerprint"]}})).await.unwrap();
    let result = execute(p).await;
    panic!(
        "Owned child should have been terminated at its exact barrier: {}",
        result.is_ok()
    );
}
