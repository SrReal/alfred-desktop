use super::*;
use crate::{monitoring, storage};
use std::path::PathBuf;
fn fixture() -> (Arc<Core>, PathBuf) {
    let root = std::env::temp_dir().join(format!("ap-watch-extra-{}", storage::id()));
    std::fs::create_dir(&root).unwrap();
    let resource = root.join("owned-resource");
    std::fs::create_dir_all(resource.join("sidecars/browser-host")).unwrap();
    let state = root.join("state.json");
    std::fs::write(
        &state,
        json!({"text":"OWNED_PRIVATE_SENTINEL","present":false,"ambiguous":false}).to_string(),
    )
    .unwrap();
    let state_path = serde_json::to_string(&state.to_string_lossy()).unwrap();
    let script = format!(
        r#"import {{createInterface}} from 'node:readline';import {{readFileSync,writeFileSync,existsSync}} from 'node:fs';
const statePath={state_path};for await(const line of createInterface({{input:process.stdin}})){{const r=JSON.parse(line);const s=JSON.parse(readFileSync(statePath));if(s.pause){{writeFileSync(statePath+'.entered','entered');while(!existsSync(statePath+'.release'))await new Promise(done=>setTimeout(done,10));}}process.stdout.write(JSON.stringify({{id:r.id,ok:r.operation==='browser.extract',data:{{text:s.text}},error:'OWNED_UNSUPPORTED'}})+'\n');}}"#
    );
    std::fs::write(resource.join("sidecars/browser-host/host.mjs"), script).unwrap();
    let mut c = Core::new(root.join("profile"), resource).unwrap();
    for module in ["monitoring", "browser", "gui"] {
        c.store
            .execute(
                "UPDATE module_configs SET enabled=1 WHERE module_id=?",
                &[json!(module)],
            )
            .unwrap();
    }
    let literal = serde_json::to_string(&state.to_string_lossy())
        .unwrap()
        .replace('\'', "''");
    let ps = format!(
        r#"[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
$r=[Console]::In.ReadToEnd() | ConvertFrom-Json;$statePath=('{literal}' | ConvertFrom-Json);$s=Get-Content -LiteralPath $statePath -Raw | ConvertFrom-Json
if($s.ambiguous){{@{{ok=$false;error='UIA_SELECTOR_AMBIGUOUS'}}|ConvertTo-Json -Compress}}elseif(!$s.present){{@{{ok=$false;error='UIA_ELEMENT_NOT_FOUND'}}|ConvertTo-Json -Compress}}else{{@{{ok=$true;data=@{{hwnd=$r.input.hwnd;process_id=$r.input.process_id;name='OWNED_PRIVATE_SENTINEL'}}}}|ConvertTo-Json -Depth 10 -Compress}}"#
    );
    let native = root.join("owned-ui.ps1");
    std::fs::write(&native, ps).unwrap();
    c.native = native;
    (Arc::new(c), root)
}
fn page(c: &Core) -> Value {
    monitoring::create(c,"monitoring","page",&json!({"path":"https://example.org/owned","body":{"tab":"owned","page_selector":"#content","condition":"changed"}})).unwrap()
}
#[tokio::test]
async fn watch_page_mock_changes_hash_only_and_never_executes_page_content() {
    let (c, root) = fixture();
    page(&c);
    assert_eq!(monitoring::poll(&c).await.unwrap(), json!([]));
    std::fs::write(
        root.join("state.json"),
        json!({"text":"OWNED_PRIVATE_SENTINEL_CHANGED","present":false,"ambiguous":false})
            .to_string(),
    )
    .unwrap();
    let r = monitoring::poll(&c).await.unwrap();
    assert_eq!(r.as_array().unwrap().len(), 1);
    assert_eq!(r[0]["kind"], "page");
    assert_eq!(monitoring::poll(&c).await.unwrap(), json!([]));
    let durable = c.store.query("SELECT last_json FROM watches", &[]).unwrap();
    assert!(!durable.to_string().contains("OWNED_PRIVATE_SENTINEL"));
    assert!(!c
        .store
        .query(
            "SELECT input_summary,output_summary FROM activity_events",
            &[]
        )
        .unwrap()
        .to_string()
        .contains("OWNED_PRIVATE_SENTINEL"));
    drop(c.browser.lock().await.take());
    drop(c);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn watch_ui_mock_appearance_absence_ambiguity_and_recovery_are_visible() {
    let (c, root) = fixture();
    monitoring::create(&c,"monitoring","ui_element",&json!({"path":"123","body":{"process_id":44,"selector":{"automation_id":"owned","scope":"descendants"},"condition":"exists"}})).unwrap();
    assert_eq!(monitoring::poll(&c).await.unwrap(), json!([]));
    std::fs::write(
        root.join("state.json"),
        json!({"text":"unused","present":true,"ambiguous":false}).to_string(),
    )
    .unwrap();
    assert_eq!(monitoring::poll(&c).await.unwrap()[0]["kind"], "ui_element");
    assert_eq!(monitoring::poll(&c).await.unwrap(), json!([]));
    std::fs::write(
        root.join("state.json"),
        json!({"text":"unused","present":true,"ambiguous":true}).to_string(),
    )
    .unwrap();
    assert_eq!(
        monitoring::poll(&c).await.unwrap()[0]["event"],
        "watch_error"
    );
    assert_eq!(monitoring::poll(&c).await.unwrap(), json!([]));
    std::fs::write(
        root.join("state.json"),
        json!({"text":"unused","present":true,"ambiguous":false}).to_string(),
    )
    .unwrap();
    assert_eq!(
        monitoring::poll(&c).await.unwrap()[0]["event"],
        "watch_recovered"
    );
    assert!(!c
        .store
        .query("SELECT last_json FROM watches", &[])
        .unwrap()
        .to_string()
        .contains("OWNED_PRIVATE_SENTINEL"));
    drop(c);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn watch_module_restore_during_owned_browser_response_discards_observation() {
    let (c, root) = fixture();
    let w = page(&c);
    std::fs::write(
        root.join("state.json"),
        json!({"text":"OWNED_PRIVATE_SENTINEL","pause":true}).to_string(),
    )
    .unwrap();
    let c2 = c.clone();
    let job = tokio::spawn(async move { monitoring::poll(&c2).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !root.join("state.json.entered").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    c.store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='browser'",
            &[],
        )
        .unwrap();
    crate::media::revoke(&c).unwrap();
    c.store
        .execute(
            "UPDATE module_configs SET enabled=1 WHERE module_id='browser'",
            &[],
        )
        .unwrap();
    std::fs::write(root.join("state.json.release"), "release").unwrap();
    assert_eq!(job.await.unwrap().unwrap(), json!([]));
    assert_eq!(
        c.store
            .query(
                "SELECT last_json FROM watches WHERE id=?",
                &[w["id"].clone()]
            )
            .unwrap()[0]["last_json"],
        Value::Null
    );
    drop(c.browser.lock().await.take());
    drop(c);
    std::fs::remove_dir_all(root).unwrap();
}
