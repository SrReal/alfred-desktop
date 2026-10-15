use super::*;
use crate::{catalog, policy::Risk};
use std::{path::PathBuf, time::Duration};

fn fixture(script: &str) -> Arc<Core> {
    let root = std::env::temp_dir().join(format!("ap-gui-test-{}", storage::id()));
    let mut c = Core::new(root, PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap();
    c.native = c.root.join("temp/mock-native.ps1");
    std::fs::write(&c.native, script).unwrap();
    Arc::new(c)
}
async fn registered(c: &Core) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !c.runs.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn gui_schema_and_mandatory_mutation_permissions() {
    let c = fixture("throw 'native must not run'");
    let gui = catalog::MODULES.iter().find(|m| m.id == "gui").unwrap();
    assert_eq!(gui.tools.len(), 12);
    for input in [
        json!({}),
        json!({"hwnd":1,"_input_tag":42}),
        json!({"hwnd":1,"output":"elsewhere"}),
        json!({"hwnd":1,"selector":{"control_type":49999}}),
        json!({"hwnd":1,"selector":{"path":[{}],"extra":true}}),
    ] {
        assert_eq!(
            c.execute("gui.inspect", input).await.unwrap_err(),
            "INVALID_TOOL_INPUT"
        );
    }
    for action in [
        "invoke",
        "set_value",
        "select",
        "click",
        "hover",
        "drag",
        "send_keys",
    ] {
        let input = match action {
            "drag" => json!({"hwnd":1,"body":{"destination":{"hwnd":1,"name":"target"}}}),
            "set_value" | "send_keys" => json!({"hwnd":1,"text":"fixture"}),
            _ => json!({"hwnd":1}),
        };
        let tool = format!("gui.{action}");
        c.grants
            .lock()
            .unwrap()
            .insert(format!("{tool}:{input}"), Risk::Contextual);
        assert!(c.execute(&tool, input).await.is_err());
    }
    let audits = c
        .store
        .query("SELECT risk,status FROM activity_events", &[])
        .unwrap();
    assert_eq!(audits.as_array().unwrap().len(), 7);
    assert!(audits
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["risk"] == "Mandatory" && row["status"] == "denied"));
}
#[tokio::test]
async fn queued_input_rechecks_revoked_fallback_and_privacy() {
    let c = fixture("throw 'queued native must not run'");
    c.store
        .set_setting("gui_fallback_input", &json!(true))
        .unwrap();
    let lease = c.gui_input.clone().lock_owned().await;
    let job = {
        let c = c.clone();
        tokio::spawn(async move { dispatch(&c, "gui.hover", &json!({"hwnd":1})).await })
    };
    registered(&c).await;
    c.store
        .set_setting("gui_fallback_input", &json!(false))
        .unwrap();
    drop(lease);
    assert!(job
        .await
        .unwrap()
        .unwrap_err()
        .starts_with("SENDINPUT_DISABLED"));
    assert!(c.runs.lock().unwrap().is_empty());
    let lease = c.gui_input.clone().lock_owned().await;
    let job = {
        let c = c.clone();
        tokio::spawn(async move { dispatch(&c, "gui.click", &json!({"hwnd":1})).await })
    };
    registered(&c).await;
    c.store.set_setting("privacy", &json!(true)).unwrap();
    drop(lease);
    assert_eq!(job.await.unwrap().unwrap_err(), "PRIVACY_DENIED");
}
#[tokio::test]
async fn cancellation_is_scoped_and_releases_queued_input() {
    let c = fixture("throw 'cancelled native must not run'");
    let unrelated = CancellationToken::new();
    c.runs
        .lock()
        .unwrap()
        .insert("terminal-fixture".into(), unrelated.clone());
    let lease = c.gui_input.clone().lock_owned().await;
    let job = {
        let c = c.clone();
        tokio::spawn(async move { dispatch(&c, "gui.click", &json!({"hwnd":1})).await })
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        while c.runs.lock().unwrap().len() != 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    cancel_active(&c, true).unwrap();
    assert_eq!(job.await.unwrap().unwrap_err(), "GUI_CANCELLED");
    assert!(!unrelated.is_cancelled());
    drop(lease);
    assert!(c.gui_input.try_lock().is_ok());
    assert_eq!(c.runs.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn active_native_cancels_and_sensitive_output_is_denied() {
    let c=fixture("$request=[Console]::In.ReadToEnd() | ConvertFrom-Json;[IO.File]::WriteAllText(($PSScriptRoot+'/started'),[string]$PID);Start-Sleep -Seconds 30;'{\"ok\":true,\"data\":{}}'");
    let marker = c.root.join("temp/started");
    let job = {
        let c = c.clone();
        tokio::spawn(async move { dispatch(&c, "gui.inspect", &json!({"hwnd":1})).await })
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    #[cfg(windows)]
    let process_handle = {
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_ACCESS_RIGHTS};
        let pid: u32 = std::fs::read_to_string(&marker).unwrap().parse().unwrap();
        unsafe { OpenProcess(PROCESS_ACCESS_RIGHTS(0x00100000), false, pid) }.unwrap()
    };
    cancel_active(&c, true).unwrap();
    assert!(!job.is_finished(), "revoking input must not cancel a read");
    cancel_active(&c, false).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), job)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        "GUI_CANCELLED"
    );
    #[cfg(windows)]
    {
        use windows::Win32::{
            Foundation::{CloseHandle, WAIT_OBJECT_0},
            System::Threading::WaitForSingleObject,
        };
        assert_eq!(
            unsafe { WaitForSingleObject(process_handle, 5000) },
            WAIT_OBJECT_0,
            "owned native process must exit after cancellation"
        );
        unsafe { CloseHandle(process_handle) }.unwrap();
    }
    assert!(c.runs.lock().unwrap().is_empty());
    let secret=fixture("[Console]::In.ReadToEnd() | Out-Null;'{\"ok\":true,\"data\":{\"text\":\"Bearer abcdefghijklmnopqrstuvwxyz0123456789\"}}'");
    assert_eq!(
        dispatch(&secret, "gui.get_text", &json!({"hwnd":1}))
            .await
            .unwrap_err(),
        "SENSITIVE_GUI_CONTENT_DENIED"
    );
}
