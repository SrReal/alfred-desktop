use crate::{asset_files, catalog, policy, runtime::Core, storage};
use serde_json::json;
use std::{os::windows::fs::MetadataExt, path::PathBuf, sync::Arc};
#[tokio::test]
async fn schema_observed_target_and_mandatory_mutations_are_enforced() {
    let root = std::env::temp_dir().join(format!("ap-browser-forms-{}", storage::id()));
    std::fs::create_dir(&root).unwrap();
    let identity = asset_files::identity(&asset_files::directory(&root).unwrap()).unwrap();
    let core = Arc::new(
        Core::new(
            root.clone(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap(),
    );
    assert_eq!(
        core.execute("browser.forms", json!({})).await.unwrap_err(),
        "TOOL_DISABLED_OR_UNKNOWN"
    );
    core.store
        .execute(
            "UPDATE module_configs SET enabled=1 WHERE module_id='browser'",
            &[],
        )
        .unwrap();
    for tool in ["browser.forms", "browser.extract"] {
        assert_eq!(policy::classify(tool, &json!({})), policy::Risk::Free);
    }
    for (tool, body) in [
        ("browser.select", json!({"labels":["Owned label"]})),
        ("browser.check", json!({"checked":true})),
    ] {
        assert_eq!(policy::classify(tool, &json!({})), policy::Risk::Mandatory);
        let input = json!({"url":"http://127.0.0.1/owned","selector":"#own","body":body});
        assert_eq!(
            core.execute(tool, input).await.unwrap_err(),
            "INVALID_TOOL_INPUT"
        );
        let mut input = json!({"url":"http://127.0.0.1/owned","selector":"#own","body":body});
        input["body"]["fingerprint"] = json!("a".repeat(64));
        core.grants
            .lock()
            .unwrap()
            .insert(format!("{tool}:{input}"), policy::Risk::Contextual);
        // Even a stored contextual grant cannot satisfy Mandatory confirmation.
        assert_eq!(core.execute(tool, input).await.unwrap_err(), "APP_MISSING");
        assert!(core.browser.lock().await.is_none());
    }
    let requests = core
        .store
        .query("SELECT risk,status FROM permission_requests", &[])
        .unwrap();
    assert_eq!(requests.as_array().unwrap().len(), 2);
    assert!(requests
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["risk"] == "Mandatory" && row["status"] == "cancelled"));
    let events=core.store.query("SELECT status FROM activity_events WHERE tool_id IN ('browser.check','browser.select')",&[]).unwrap();
    assert_eq!(events.as_array().unwrap().len(), 2);
    assert!(events
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["status"] == "denied"));
    let schema = catalog::tool_schema("browser.select", "url,selector,body");
    let validator = jsonschema::validator_for(&schema).unwrap();
    for body in [
        json!({"fingerprint":"a".repeat(64),"labels":["Same","Same"]}),
        json!({"fingerprint":"a".repeat(64),"labels":["Same"],"javascript":"forbidden"}),
    ] {
        assert!(!validator
            .is_valid(&json!({"url":"http://127.0.0.1/owned","selector":"#own","body":body})));
    }
    assert_eq!(
        root.parent().unwrap().canonicalize().unwrap(),
        std::env::temp_dir().canonicalize().unwrap()
    );
    assert_eq!(
        asset_files::identity(&asset_files::directory(&root).unwrap()).unwrap(),
        identity
    );
    let mut paths = vec![root.clone()];
    while let Some(path) = paths.pop() {
        assert_eq!(
            std::fs::symlink_metadata(&path).unwrap().file_attributes() & 0x400,
            0
        );
        if path.is_dir() {
            paths.extend(std::fs::read_dir(path).unwrap().map(|p| p.unwrap().path()));
        }
    }
    drop(core);
    std::fs::remove_dir_all(root).unwrap();
}
