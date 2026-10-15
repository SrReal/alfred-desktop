use super::*;
use uuid::Uuid;

async fn receipt_inspect(f: &Fixture, id: &Value) -> Value {
    f.core
        .execute(
            "drive.upload",
            f.input(json!({"action":"inspect_receipts","job_id":id})),
        )
        .await
        .unwrap()
}
fn receipt_cleanup(v: &Value, generation: &Value) -> Value {
    let r = v["receipts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["generation"] == *generation)
        .unwrap();
    json!({"action":"cleanup_receipt","job_id":v["job"]["id"],"expected":{"job_fingerprint":v["job"]["job_fingerprint"],"receipt_generation":r["generation"],"receipt_revision":r["revision"]}})
}
async fn lose_pointer(f: &Fixture, j: &Value) {
    drive_auth::Scope::new(&f.core, &f.id)
        .unwrap()
        .lose_owned_capability_pointer(j["id"].as_str().unwrap(), &j["record"]["immutable"])
        .unwrap();
}
#[tokio::test]
async fn protected_receipt_recovers_missing_pointer_after_client_change_with_mandatory_review() {
    let f = Fixture::new(false, false).await;
    let j = f.upload_start(vec![66; 32]).await;
    lose_pointer(&f, &j).await;
    let mut config = drive_auth::record(&f.core, &f.id).unwrap();
    config["client_id"] = json!("owned-new-client.apps.googleusercontent.com");
    config["allowed_files"] = json!([]);
    config["allowed_folders"] = json!([]);
    save_config(&f, config).await;
    let seen = f.m.lock().unwrap().seen.len();
    let v = receipt_inspect(&f, &j["id"]).await;
    assert_eq!(v["receipts"][0]["state"], "known_orphan_generation");
    assert_eq!(v["receipts"][0]["present_parts"], 1);
    let input = receipt_cleanup(&v, &j["record"]["capability_version"]);
    assert_eq!(
        f.approve("drive.upload", input.clone(), "deny")
            .await
            .unwrap_err(),
        "PERMISSION_DENIED_OR_TIMEOUT"
    );
    assert_eq!(receipt_inspect(&f, &j["id"]).await, v);
    let done = f.approve("drive.upload", input, "allow").await.unwrap();
    assert_eq!(done["known_receipt_retired"], true);
    assert_eq!(done["all_known_receipts_retired"], true);
    assert_eq!(done["job"]["state"], "abandoned");
    let later = receipt_inspect(&f, &j["id"]).await;
    assert_eq!(later["receipts"][0]["retirement_verified"], true);
    assert_eq!(seen, f.m.lock().unwrap().seen.len());
    let audit = f.core.store.query("SELECT risk,input_summary,output_summary FROM activity_events WHERE tool_id='drive.upload'", &[]).unwrap();
    let text = audit.to_string();
    for secret in [
        "owned_private_session_key",
        "owned_fictional_access",
        "immutable_sha256",
        "authorization_binding",
        "Subida",
        "source.bin",
    ] {
        assert!(!text.contains(secret), "{secret}");
    }
    assert_eq!(
        audit
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["risk"] == "Mandatory")
            .count(),
        2
    );
    f.clean();
}
#[tokio::test]
async fn protected_receipt_preserves_other_generation_and_rechecks_private_revision_after_review() {
    let f = Fixture::new(false, false).await;
    let j = f.upload_start(vec![8; 16]).await;
    let grant = drive_auth::grant(&f.core, &f.id).await.unwrap();
    let old_version = Uuid::parse_str(j["record"]["capability_version"].as_str().unwrap()).unwrap();
    let payload = grant
        .capability_read(j["id"].as_str().unwrap(), old_version)
        .unwrap();
    lose_pointer(&f, &j).await;
    let v = receipt_inspect(&f, &j["id"]).await;
    let p = prepare_operation(
        &f.core,
        "drive.upload",
        &f.input(receipt_cleanup(&v, &j["record"]["capability_version"])),
    )
    .await
    .unwrap();
    let newer = grant
        .capability_save(j["id"].as_str().unwrap(), &payload)
        .unwrap();
    assert_eq!(
        execute_operation(p).await.unwrap_err(),
        "DRIVE_RECEIPT_OBSERVATION_CHANGED"
    );
    let v = receipt_inspect(&f, &j["id"]).await;
    assert_eq!(v["receipts"][0]["other_pointer_preserved"], true);
    let seen = f.m.lock().unwrap().seen.len();
    let done = f
        .approve(
            "drive.upload",
            receipt_cleanup(&v, &j["record"]["capability_version"]),
            "allow",
        )
        .await
        .unwrap();
    assert_eq!(done["known_receipt_retired"], true);
    assert_eq!(done["all_known_receipts_retired"], false);
    assert_eq!(done["job"]["record"]["cleanup_pending"], true);
    assert_eq!(
        grant
            .capability_read(j["id"].as_str().unwrap(), newer)
            .unwrap(),
        payload
    );
    assert_eq!(seen, f.m.lock().unwrap().seen.len());
    assert!(grant
        .capability_delete(j["id"].as_str().unwrap(), newer)
        .unwrap());
    drop(grant);
    f.clean();
}
#[tokio::test]
async fn protected_receipt_survives_public_journal_failure_and_reconciles_explicitly() {
    let f = Fixture::new(false, false).await;
    let j = f.upload_start(vec![19; 64]).await;
    let v = receipt_inspect(&f, &j["id"]).await;
    f.core.store.execute("CREATE TRIGGER owned_receipt_journal_failure BEFORE UPDATE ON drive_jobs WHEN NEW.record_json LIKE '%explicit_protected_receipt_retirement%' BEGIN SELECT RAISE(ABORT,'owned_failure'); END", &[]).unwrap();
    assert_eq!(
        f.approve(
            "drive.upload",
            receipt_cleanup(&v, &j["record"]["capability_version"]),
            "allow"
        )
        .await
        .unwrap_err(),
        "DRIVE_LOCAL_CLEANUP_JOURNAL_OUTCOME_UNCERTAIN"
    );
    f.core
        .store
        .execute("DROP TRIGGER owned_receipt_journal_failure", &[])
        .unwrap();
    let later = receipt_inspect(&f, &j["id"]).await;
    assert_eq!(later["job"]["state"], "active");
    assert_eq!(later["job"]["job_fingerprint"], v["job"]["job_fingerprint"]);
    assert_eq!(later["receipts"][0]["retirement_verified"], true);
    assert_ne!(
        later["receipts"][0]["revision"],
        v["receipts"][0]["revision"]
    );
    assert!(f
        .core
        .execute(
            "drive.upload",
            f.input(receipt_cleanup(&v, &j["record"]["capability_version"]))
        )
        .await
        .is_err());
    let seen = f.m.lock().unwrap().seen.len();
    let done = f
        .approve(
            "drive.upload",
            receipt_cleanup(&later, &j["record"]["capability_version"]),
            "allow",
        )
        .await
        .unwrap();
    assert_eq!(done["job"]["record"]["cleanup_pending"], false);
    assert_eq!(
        inspect(&f, &j["id"]).await["capability_state"],
        "retired_known_generation"
    );
    assert_eq!(seen, f.m.lock().unwrap().seen.len());
    f.clean();
}
#[tokio::test]
async fn protected_receipt_guards_lease_profile_privacy_module_generation_job_and_closed_schema() {
    let f = Fixture::new(false, false).await;
    let j = f.upload_start(vec![71; 1]).await;
    let v = receipt_inspect(&f, &j["id"]).await;
    let input = f.input(receipt_cleanup(&v, &j["record"]["capability_version"]));
    let seen = f.m.lock().unwrap().seen.len();
    for extra in [
        json!({"path":"unknown"}),
        json!({"expected":{}}),
        json!({"capability_version":storage::id()}),
    ] {
        let mut bad = input.clone();
        for (k, value) in extra.as_object().unwrap() {
            bad[k] = value.clone();
        }
        assert!(prepare_operation(&f.core, "drive.upload", &bad)
            .await
            .is_err());
    }
    let p = prepare_operation(&f.core, "drive.upload", &input)
        .await
        .unwrap();
    let lease = local::job_lease(&f.core, j["id"].as_str().unwrap()).unwrap();
    assert_eq!(
        execute_operation(p).await.unwrap_err(),
        "DRIVE_JOB_BUSY_OR_LOCK_CHANGED"
    );
    drop(lease);
    for (key, value, expected) in [
        ("privacy".to_string(), json!(true), "PRIVACY_DENIED"),
        (
            format!("drive.connection_generation.{}", f.id),
            json!(storage::id()),
            "DRIVE_CONNECTION_CHANGED",
        ),
        (
            "drive.profile_identity".to_string(),
            json!(storage::id()),
            "DRIVE_CLEANUP_HISTORICAL_BINDING",
        ),
    ] {
        let p = prepare_operation(&f.core, "drive.upload", &input)
            .await
            .unwrap();
        let old = f.core.store.setting(&key);
        f.core.store.set_setting(&key, &value).unwrap();
        let error = execute_operation(p).await.unwrap_err();
        assert!(error.contains(expected), "{error}");
        f.core.store.set_setting(&key, &old).unwrap();
    }
    let p = prepare_operation(&f.core, "drive.upload", &input)
        .await
        .unwrap();
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='drive'",
            &[],
        )
        .unwrap();
    assert_eq!(
        execute_operation(p).await.unwrap_err(),
        "MODULE_DISABLED: drive"
    );
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=1 WHERE module_id='drive'",
            &[],
        )
        .unwrap();
    let p = prepare_operation(&f.core, "drive.upload", &input)
        .await
        .unwrap();
    let old = local::job(&f.core, &f.id, j["id"].as_str().unwrap(), "upload").unwrap();
    local::update(&f.core, &old, "active", &old["record"]).unwrap();
    assert_eq!(
        execute_operation(p).await.unwrap_err(),
        "DRIVE_STALE_JOB_OBSERVATION"
    );
    assert_eq!(
        receipt_inspect(&f, &j["id"]).await["receipts"][0]["present_parts"],
        1
    );
    assert_eq!(seen, f.m.lock().unwrap().seen.len());
    f.clean();
}
#[tokio::test]
async fn protected_receipt_immutable_context_original_root_and_empty_upload_cannot_adopt_parts() {
    let f = Fixture::new(false, false).await;
    let j = f.upload_start(vec![72; 2]).await;
    let old = local::job(&f.core, &f.id, j["id"].as_str().unwrap(), "upload").unwrap();
    let seen = f.m.lock().unwrap().seen.len();
    let mut record = old["record"].clone();
    record["immutable"]["name"] = json!("foreign record");
    let changed = local::update(&f.core, &old, "active", &record).unwrap();
    let unknown = receipt_inspect(&f, &j["id"]).await;
    assert!(unknown["receipts"].as_array().unwrap().is_empty());
    assert_eq!(unknown["unknown_legacy_parts_unverified"], true);
    record = old["record"].clone();
    record["immutable"]["capability_binding"]["root"] = json!("unobserved native root");
    let changed = local::update(&f.core, &changed, "active", &record).unwrap();
    assert_eq!(
        f.core
            .execute(
                "drive.upload",
                f.input(json!({"action":"inspect_receipts","job_id":j["id"]}))
            )
            .await
            .unwrap_err(),
        "DRIVE_CLEANUP_HISTORICAL_BINDING"
    );
    local::update(&f.core, &changed, "active", &old["record"]).unwrap();
    assert_eq!(
        receipt_inspect(&f, &j["id"]).await["receipts"][0]["present_parts"],
        1
    );
    assert_eq!(seen, f.m.lock().unwrap().seen.len());
    let empty = f.upload_start(vec![]).await;
    let before = f.m.lock().unwrap().seen.len();
    let v = receipt_inspect(&f, &empty["id"]).await;
    assert!(v["receipts"].as_array().unwrap().is_empty());
    assert_eq!(v["unknown_legacy_parts_unverified"], false);
    assert_eq!(before, f.m.lock().unwrap().seen.len());
    f.clean();
}

async fn inspect(f: &Fixture, id: &Value) -> Value {
    f.core
        .execute(
            "drive.upload",
            f.input(json!({"action":"inspect_cleanup","job_id":id})),
        )
        .await
        .unwrap()
}
fn cleanup_input(v: &Value) -> Value {
    json!({"action":"cleanup","job_id":v["job"]["id"],"expected":{"job_fingerprint":v["job"]["job_fingerprint"],"capability_version":v["capability_version"]}})
}
async fn save_config(f: &Fixture, config: Value) {
    crate::integrations::save(&f.core,&json!({"id":f.id,"module_id":"drive","name":"Owned Drive file fixture","config":config,"reauthorize":true,"secret":if config["client_secret_mode"]=="credential_manager"{"owned_fictional_client_secret"}else{""}})).await.unwrap();
}
#[tokio::test]
async fn historical_cleanup_after_new_consent_is_mandatory_local_and_redacted() {
    let f = Fixture::new(false, false).await;
    let started = f.upload_start(vec![31; 1024]).await;
    assert_eq!(
        started["record"]["immutable"]["capability_binding"]["version"],
        1
    );
    picked_authorization(&f).await;
    drive_auth::expire_owned_grant(&f.core, &f.id).unwrap();
    let seen = f.m.lock().unwrap().seen.len();
    let v = inspect(&f, &started["id"]).await;
    assert_eq!(v["capability_state"], "observed");
    assert_eq!(v["cleanup_available"], true);
    assert_eq!(
        f.approve("drive.upload", cleanup_input(&v), "deny")
            .await
            .unwrap_err(),
        "PERMISSION_DENIED_OR_TIMEOUT"
    );
    assert_eq!(
        inspect(&f, &started["id"]).await["capability_version"],
        v["capability_version"]
    );
    let done = f
        .approve("drive.upload", cleanup_input(&v), "allow")
        .await
        .unwrap();
    assert_eq!(done["state"], "abandoned");
    assert_eq!(done["known_capability_retired"], true);
    assert_eq!(done["orphan_parts_unverified"], false);
    let later = inspect(&f, &started["id"]).await;
    assert_eq!(later["capability_state"], "retired_known_generation");
    assert_eq!(later["orphan_parts_unverified"], false);
    assert_eq!(seen, f.m.lock().unwrap().seen.len());
    let rows = f.core.store.query("SELECT risk,input_summary,output_summary FROM activity_events WHERE tool_id='drive.upload'", &[]).unwrap();
    let text = rows.to_string();
    for private in [
        "owned_private_session_key",
        "owned_fictional_access",
        "Subida",
        "source.bin",
        "capability_binding",
    ] {
        assert!(!text.contains(private));
    }
    assert_eq!(
        rows.as_array()
            .unwrap()
            .iter()
            .filter(|r| r["risk"] == "Mandatory")
            .count(),
        2
    );
    f.clean();
}
#[tokio::test]
async fn changed_client_cleanup_uses_original_binding_in_owned_native_and_memory_vaults() {
    for native in [false, true] {
        let f = Fixture::new(false, native).await;
        let started = f.upload_start(vec![42; 1024]).await;
        let original = drive_auth::record(&f.core, &f.id).unwrap();

        let mut changed = original.clone();
        changed["client_id"] = json!("owned-new-client.apps.googleusercontent.com");
        changed["client_secret_mode"] = json!("credential_manager");
        changed["read_only"] = json!(true);
        changed["allowed_folders"] = json!([]);
        changed["allowed_files"] = json!([]);
        save_config(&f, changed).await;
        let oauth_version = drive_auth::owned_grant_version(&f.core, &f.id).unwrap();
        let seen = f.m.lock().unwrap().seen.len();
        let v = inspect(&f, &started["id"]).await;
        assert_eq!(v["capability_state"], "observed");
        assert_eq!(v["cleanup_available"], true);
        let done = f
            .approve("drive.upload", cleanup_input(&v), "allow")
            .await
            .unwrap();
        assert_eq!(done["known_capability_retired"], true);
        assert_eq!(
            inspect(&f, &started["id"]).await["capability_version"],
            Value::Null
        );
        assert_eq!(seen, f.m.lock().unwrap().seen.len());
        // Client replacement deliberately clears the grant. Cleanup itself
        // preserves the current envelope and never renews/uses it.
        assert_eq!(
            drive_auth::owned_grant_version(&f.core, &f.id).unwrap(),
            oauth_version
        );
        save_config(&f, original).await;
        assert_eq!(seen, f.m.lock().unwrap().seen.len());
        f.clean();
    }
}
#[tokio::test]
async fn forged_historical_descriptor_or_private_hash_never_adopts_an_observed_binding() {
    let f = Fixture::new(false, false).await;
    let started = f.upload_start(vec![7; 16]).await;
    let id = started["id"].as_str().unwrap();
    let seen = f.m.lock().unwrap().seen.len();
    for (key, bad) in [
        ("namespace", json!("oauth-token-v1")),
        ("job", json!(f.id)),
        ("connection", json!(storage::id())),
        ("profile", json!(storage::id())),
        ("root", json!("foreign_native_root")),
        (
            "client_id",
            json!("owned-forged.apps.googleusercontent.com"),
        ),
        ("unexpected", json!(true)),
    ] {
        let old = local::job(&f.core, &f.id, id, "upload").unwrap();
        let mut record = old["record"].clone();
        record["immutable"]["capability_binding"][key] = bad;
        let forged = local::update(&f.core, &old, "active", &record).unwrap();
        assert!(f
            .core
            .execute(
                "drive.upload",
                f.input(json!({"action":"inspect_cleanup","job_id":id}))
            )
            .await
            .unwrap_err()
            .contains("HISTORICAL_BINDING"));
        local::update(&f.core, &forged, "active", &old["record"]).unwrap();
    }
    let old = local::job(&f.core, &f.id, id, "upload").unwrap();
    let mut changed = old["record"].clone();
    changed["immutable"]["name"] = json!("Forged metadata");
    let forged = local::update(&f.core, &old, "active", &changed).unwrap();
    assert_eq!(
        f.core
            .execute(
                "drive.upload",
                f.input(json!({"action":"inspect_cleanup","job_id":id}))
            )
            .await
            .unwrap_err(),
        "DRIVE_CLEANUP_PRIVATE_BINDING"
    );
    local::update(&f.core, &forged, "active", &old["record"]).unwrap();
    assert_eq!(
        inspect(&f, &started["id"]).await["capability_state"],
        "observed"
    );
    assert_eq!(seen, f.m.lock().unwrap().seen.len());
    f.clean();
}
#[tokio::test]
async fn stale_cleanup_job_generation_privacy_and_module_guards_preserve_native_entries() {
    let f = Fixture::new(false, false).await;
    let started = f.upload_start(vec![3; 1024]).await;
    let id = started["id"].as_str().unwrap();
    let v = inspect(&f, &started["id"]).await;
    let p = prepare_operation(&f.core, "drive.upload", &f.input(cleanup_input(&v)))
        .await
        .unwrap();
    let old = local::job(&f.core, &f.id, id, "upload").unwrap();
    let changed = local::update(&f.core, &old, "active", &old["record"]).unwrap();
    assert_eq!(
        execute_operation(p).await.unwrap_err(),
        "DRIVE_STALE_JOB_OBSERVATION"
    );
    assert!(
        prepare_operation(&f.core, "drive.upload", &f.input(cleanup_input(&v)))
            .await
            .is_err()
    );
    let v = inspect(&f, &started["id"]).await;
    let p = prepare_operation(&f.core, "drive.upload", &f.input(cleanup_input(&v)))
        .await
        .unwrap();
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(execute_operation(p).await.unwrap_err(), "PRIVACY_DENIED");
    f.core.store.set_setting("privacy", &json!(false)).unwrap();
    let p = prepare_operation(&f.core, "drive.upload", &f.input(cleanup_input(&v)))
        .await
        .unwrap();
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='drive'",
            &[],
        )
        .unwrap();
    assert_eq!(
        execute_operation(p).await.unwrap_err(),
        "MODULE_DISABLED: drive"
    );
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=1 WHERE module_id='drive'",
            &[],
        )
        .unwrap();
    let p = prepare_operation(&f.core, "drive.upload", &f.input(cleanup_input(&v)))
        .await
        .unwrap();
    let grant = drive_auth::grant(&f.core, &f.id).await.unwrap();
    let old_version = Uuid::parse_str(v["capability_version"].as_str().unwrap()).unwrap();
    let bytes = grant.capability_read(id, old_version).unwrap();
    assert!(grant.capability_delete(id, old_version).unwrap());
    let newer = grant.capability_save(id, &bytes).unwrap();
    drop(grant);
    assert_eq!(
        execute_operation(p).await.unwrap_err(),
        "DRIVE_CLEANUP_CAPABILITY_CHANGED_OR_MISSING"
    );
    let now = inspect(&f, &started["id"]).await;
    assert_eq!(now["capability_version"], newer.to_string());
    assert_eq!(
        now["capability_state"],
        "changed_known_generation_preserved"
    );
    assert_eq!(now["cleanup_available"], false);
    assert!(
        prepare_operation(&f.core, "drive.upload", &f.input(cleanup_input(&now)))
            .await
            .is_err()
    );
    assert_eq!(changed["revision"], now["job"]["revision"]);
    f.clean();
}
#[tokio::test]
async fn legacy_cleanup_requires_unchanged_config_and_validates_complete_original_payload() {
    let f = Fixture::new(false, false).await;
    let started = f.upload_start(vec![29; 24]).await;
    let id = started["id"].as_str().unwrap();
    let grant = drive_auth::grant(&f.core, &f.id).await.unwrap();
    let version =
        Uuid::parse_str(started["record"]["capability_version"].as_str().unwrap()).unwrap();
    let mut payload = grant.capability_read(id, version).unwrap();
    assert!(grant.capability_delete(id, version).unwrap());
    let old = local::job(&f.core, &f.id, id, "upload").unwrap();
    let mut record = old["record"].clone();
    record["immutable"]
        .as_object_mut()
        .unwrap()
        .remove("capability_binding");
    record["immutable"]
        .as_object_mut()
        .unwrap()
        .remove("upload_protocol");
    payload["immutable_sha256"] = json!(digest(&record["immutable"]));
    record["capability_version"] = json!(grant.capability_save(id, &payload).unwrap().to_string());
    local::update(&f.core, &old, "active", &record).unwrap();
    drop(grant);
    let original = drive_auth::record(&f.core, &f.id).unwrap();
    let mut changed = original.clone();
    changed["client_id"] = json!("owned-changed.apps.googleusercontent.com");
    save_config(&f, changed).await;
    assert_eq!(
        f.core
            .execute(
                "drive.upload",
                f.input(json!({"action":"inspect_cleanup","job_id":id}))
            )
            .await
            .unwrap_err(),
        "DRIVE_CLEANUP_LEGACY_PROFILE_CHANGED"
    );
    save_config(&f, original).await;
    picked_authorization(&f).await;
    let seen = f.m.lock().unwrap().seen.len();
    let v = inspect(&f, &started["id"]).await;
    assert_eq!(v["capability_state"], "observed");
    assert_eq!(
        f.approve("drive.upload", cleanup_input(&v), "allow")
            .await
            .unwrap()["known_capability_retired"],
        true
    );
    assert_eq!(seen, f.m.lock().unwrap().seen.len());
    f.clean();
}
#[tokio::test]
async fn missing_pointer_or_uncertain_cleanup_journal_never_claims_orphans_removed() {
    let f = Fixture::new(false, false).await;
    let started = f.upload_start(vec![13; 64]).await;
    let v = inspect(&f, &started["id"]).await;
    f.core.store.execute("CREATE TRIGGER owned_retirement_store_failure BEFORE UPDATE ON drive_jobs WHEN NEW.record_json LIKE '%explicit_local_retirement%' BEGIN SELECT RAISE(ABORT,'owned_fixture_cleanup_failure'); END", &[]).unwrap();
    assert_eq!(
        f.approve("drive.upload", cleanup_input(&v), "allow")
            .await
            .unwrap_err(),
        "DRIVE_LOCAL_CLEANUP_JOURNAL_OUTCOME_UNCERTAIN"
    );
    f.core
        .store
        .execute("DROP TRIGGER owned_retirement_store_failure", &[])
        .unwrap();
    let later = inspect(&f, &started["id"]).await;
    assert_eq!(later["job"]["state"], "active");
    assert_eq!(later["capability_state"], "missing_or_retired_unverified");
    assert_eq!(later["orphan_parts_unverified"], true);
    assert_eq!(later["cleanup_available"], false);
    assert_eq!(later["job"]["job_fingerprint"], v["job"]["job_fingerprint"]);
    assert!(
        prepare_operation(&f.core, "drive.upload", &f.input(cleanup_input(&v)))
            .await
            .is_err()
    );
    f.clean();
}
#[tokio::test]
async fn local_cleanup_schema_leases_profile_and_empty_jobs_are_closed() {
    let f = Fixture::new(false, false).await;
    let started = f.upload_start(vec![1; 1]).await;
    let seen = f.m.lock().unwrap().seen.len();
    for extra in [
        json!({"path":"foreign"}),
        json!({"expected":{}}),
        json!({"folder_id":"owned_folder"}),
    ] {
        let mut input = f.input(json!({"action":"inspect_cleanup","job_id":started["id"]}));
        for (k, v) in extra.as_object().unwrap() {
            input[k] = v.clone();
        }
        assert!(prepare_operation(&f.core, "drive.upload", &input)
            .await
            .is_err());
    }
    let lock = local::job_lease(&f.core, started["id"].as_str().unwrap()).unwrap();
    assert!(prepare_operation(
        &f.core,
        "drive.upload",
        &f.input(json!({"action":"inspect_cleanup","job_id":started["id"]}))
    )
    .await
    .err()
    .unwrap()
    .contains("BUSY"));
    drop(lock);
    let profile = f.core.store.setting("drive.profile_identity");
    f.core
        .store
        .set_setting("drive.profile_identity", &json!(storage::id()))
        .unwrap();
    assert!(f
        .core
        .execute(
            "drive.upload",
            f.input(json!({"action":"inspect_cleanup","job_id":started["id"]}))
        )
        .await
        .is_err());
    f.core
        .store
        .set_setting("drive.profile_identity", &profile)
        .unwrap();
    assert_eq!(seen, f.m.lock().unwrap().seen.len());
    let empty = f.upload_start(vec![]).await;
    let before = f.m.lock().unwrap().seen.len();
    let v = inspect(&f, &empty["id"]).await;
    assert_eq!(v["capability_state"], "not_applicable");
    assert_eq!(v["orphan_parts_unverified"], false);
    assert_eq!(v["cleanup_available"], false);
    assert_eq!(before, f.m.lock().unwrap().seen.len());
    f.clean();
}

#[tokio::test]
async fn terminal_receipts_distinguish_verified_retirement_from_missing_native_entries() {
    let f = Fixture::new(false, false).await;
    let started = f.upload_start(vec![91; 24]).await;
    let completed = f.resume(&started).await.unwrap();
    assert_eq!(completed["state"], "completed");
    assert_eq!(
        completed["record"]["capability_retirement"]["complete"],
        true
    );
    assert_eq!(
        f.status(&completed).await["record"]["cleanup_pending"],
        false
    );
    let v = inspect(&f, &started["id"]).await;
    assert_eq!(v["capability_state"], "retired_known_generation");
    assert_eq!(v["orphan_parts_unverified"], false);
    f.clean();
    let f = Fixture::new(false, false).await;
    let started = f.upload_start(vec![92; 24]).await;
    let id = started["id"].as_str().unwrap();
    let grant = drive_auth::grant(&f.core, &f.id).await.unwrap();
    let version =
        Uuid::parse_str(started["record"]["capability_version"].as_str().unwrap()).unwrap();
    assert!(grant.capability_delete(id, version).unwrap());
    drop(grant);
    // Owned reconstructed terminal row with no evidence of how its pointer
    // vanished: even if this fixture erased all parts, the app cannot prove it.
    let row = local::job(&f.core, &f.id, id, "upload").unwrap();
    local::update(&f.core, &row, "completed", &row["record"]).unwrap();
    let pending = f.status(&started).await;
    assert_eq!(pending["record"]["cleanup_pending"], true);
    let v = inspect(&f, &started["id"]).await;
    assert_eq!(v["capability_state"], "missing_or_retired_unverified");
    assert_eq!(v["orphan_parts_unverified"], true);
    f.clean();
}
