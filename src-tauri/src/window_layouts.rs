use crate::{geometry::Monitor, process, runtime::Core, storage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(crate) static ACTIONS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const PREFIX: &str = "window_layout.";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Placement {
    hwnd: i64,
    process_id: u32,
    process_started: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window_identity: Option<String>,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Layout {
    version: u32,
    id: String,
    name: String,
    windows: Vec<Placement>,
    monitors: Vec<Monitor>,
    created_at: String,
    fingerprint: String,
}
fn key(id: &str) -> Result<String, String> {
    let parsed = uuid::Uuid::parse_str(id).map_err(|_| "LAYOUT_ID")?;
    if parsed.to_string() != id {
        return Err("LAYOUT_ID".into());
    }
    Ok(format!("{PREFIX}{id}"))
}
fn name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err("LAYOUT_NAME".into());
    }
    Ok(name.into())
}
fn fingerprint(layout: &Layout) -> Result<String, String> {
    let mut unsigned = layout.clone();
    unsigned.fingerprint.clear();
    let data = serde_json::to_vec(&unsigned).map_err(|_| "LAYOUT_JSON")?;
    Ok(format!("{:x}", Sha256::digest(data)))
}
fn validate(layout: &Layout) -> Result<(), String> {
    key(&layout.id)?;
    if ![1, 2].contains(&layout.version)
        || name(&layout.name)? != layout.name
        || layout.windows.is_empty()
        || layout.windows.len() > 32
        || layout.monitors.is_empty()
        || layout.monitors.len() > 32
        || chrono::DateTime::parse_from_rfc3339(&layout.created_at).is_err()
    {
        return Err("LAYOUT_RECORD".into());
    }
    let mut seen = std::collections::HashSet::new();
    for window in &layout.windows {
        if window.hwnd <= 0
            || window.process_id == 0
            || window.process_id > i32::MAX as u32
            || !seen.insert(window.hwnd)
            || window
                .process_started
                .parse::<u64>()
                .ok()
                .filter(|n| *n > 0)
                .map(|n| n.to_string())
                != Some(window.process_started.clone())
            || window.width <= 0
            || window.height <= 0
            || (layout.version == 2
                && !window.window_identity.as_ref().is_some_and(|s| {
                    s.len() == 16
                        && s != "0000000000000000"
                        && s.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                }))
            || [
                window.x as i64,
                window.y as i64,
                window.width as i64,
                window.height as i64,
            ]
            .iter()
            .any(|n| n.abs() > 100000)
        {
            return Err("LAYOUT_PLACEMENT".into());
        }
    }
    let mut seen = std::collections::HashSet::new();
    for monitor in &layout.monitors {
        if monitor.id.is_empty()
            || monitor.id.len() > 200
            || !seen.insert(&monitor.id)
            || !monitor.scale.is_finite()
            || !(0.5..=8.).contains(&monitor.scale)
            || [
                monitor.work.x,
                monitor.work.y,
                monitor.work.width,
                monitor.work.height,
            ]
            .iter()
            .any(|n| !n.is_finite() || n.abs() > 100000.)
            || monitor.work.width <= 0.
            || monitor.work.height <= 0.
        {
            return Err("LAYOUT_MONITOR".into());
        }
    }
    if fingerprint(layout)? != layout.fingerprint {
        return Err("LAYOUT_FINGERPRINT".into());
    }
    Ok(())
}
fn load(core: &Core, id: &str) -> Result<Layout, String> {
    let rows = core.store.query(
        "SELECT json_value FROM settings WHERE key=?",
        &[json!(key(id)?)],
    )?;
    let raw = rows[0]["json_value"].as_str().ok_or("LAYOUT_NOT_FOUND")?;
    let layout: Layout = serde_json::from_str(raw).map_err(|_| "LAYOUT_RECORD")?;
    validate(&layout)?;
    if layout.id != id {
        return Err("LAYOUT_ID_MISMATCH".into());
    }
    Ok(layout)
}
fn selected(core: &Core, input: &Value) -> Result<Layout, String> {
    let layout = load(core, input["id"].as_str().ok_or("LAYOUT_ID")?)?;
    if input["fingerprint"].as_str() != Some(&layout.fingerprint) {
        return Err("LAYOUT_CHANGED_REVIEW_REQUIRED".into());
    }
    Ok(layout)
}
pub fn review(core: &Core, input: &Value) -> Result<Value, String> {
    let layout = selected(core, input)?;
    Ok(
        json!({"id":layout.id,"fingerprint":layout.fingerprint,"name":layout.name,"windows":layout.windows,"monitors":layout.monitors,"coordinate_space":"physical"}),
    )
}
pub(crate) fn apply_targets(core: &Core, input: &Value) -> Result<(Value, Vec<Monitor>), String> {
    let layout = selected(core, input)?;
    if layout.version != 2 {
        return Err("LAYOUT_WINDOW_IDENTITY_MISSING: capture a new layout".into());
    }
    Ok((json!(layout.windows), layout.monitors))
}
fn save(
    core: &Core,
    label: &str,
    windows: Vec<Placement>,
    monitors: Vec<Monitor>,
) -> Result<Value, String> {
    let mut layout = Layout {
        version: 2,
        id: storage::id(),
        name: name(label)?,
        windows,
        monitors,
        created_at: storage::now(),
        fingerprint: String::new(),
    };
    layout.fingerprint = fingerprint(&layout)?;
    validate(&layout)?;
    let raw = serde_json::to_string(&layout).map_err(|_| "LAYOUT_JSON")?;
    let mut db = core.store.0.lock().map_err(|_| "DB_LOCK")?;
    let tx = db.transaction().map_err(|_| "LAYOUT_TRANSACTION")?;
    let names: Vec<String> = {
        let mut statement = tx
            .prepare("SELECT json_value FROM settings WHERE key GLOB 'window_layout.*' LIMIT 101")
            .map_err(|_| "LAYOUT_QUERY")?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|_| "LAYOUT_QUERY")?;
        rows.map(|r| {
            let old: Layout = serde_json::from_str(&r.map_err(|_| "LAYOUT_QUERY")?)
                .map_err(|_| "LAYOUT_RECORD")?;
            validate(&old)?;
            Ok(old.name.to_lowercase())
        })
        .collect::<Result<_, String>>()?
    };
    if names.len() >= 100 {
        return Err("LAYOUT_COUNT_LIMIT".into());
    }
    if names.contains(&layout.name.to_lowercase()) {
        return Err("LAYOUT_NAME_EXISTS: no implicit overwrite".into());
    }
    tx.execute(
        "INSERT INTO settings(key,json_value) VALUES(?,?)",
        rusqlite::params![key(&layout.id)?, raw],
    )
    .map_err(|_| "LAYOUT_SAVE")?;
    tx.commit().map_err(|_| "LAYOUT_COMMIT")?;
    serde_json::to_value(layout).map_err(|_| "LAYOUT_JSON".into())
}
pub(crate) fn monitors_match(
    stored_monitors: &[Monitor],
    current: &[Monitor],
) -> Result<(), String> {
    if stored_monitors.len() != current.len() {
        return Err("LAYOUT_MONITORS_CHANGED: capture a new layout".into());
    }
    for stored in stored_monitors {
        let now = current
            .iter()
            .find(|m| m.id == stored.id)
            .ok_or("LAYOUT_MONITORS_CHANGED")?;
        let a = stored.work;
        let b = now.work;
        if (stored.scale - now.scale).abs() > 0.001
            || [a.x - b.x, a.y - b.y, a.width - b.width, a.height - b.height]
                .iter()
                .any(|n| n.abs() > 0.01)
        {
            return Err("LAYOUT_MONITORS_CHANGED: capture a new layout".into());
        }
    }
    Ok(())
}
struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
fn enabled(core: &Core, capture: bool, cancel: &CancellationToken) -> Result<(), String> {
    if cancel.is_cancelled() {
        return Err("LAYOUT_CANCELLED".into());
    }
    if !core.enabled("window") {
        return Err("TOOL_DISABLED_OR_UNKNOWN".into());
    }
    if capture && core.store.setting("privacy") == true {
        return Err("PRIVACY_DENIED".into());
    }
    Ok(())
}
pub async fn dispatch(core: &Arc<Core>, action: &str, input: &Value) -> Result<Value, String> {
    if crate::window_undo::is_change(&format!("window.{action}")) {
        let prepared =
            crate::window_undo::prepare(core, &format!("window.{action}"), input).await?;
        return Box::pin(crate::window_undo::execute(core, prepared)).await;
    }
    if action == "undo" {
        return Box::pin(crate::window_undo::execute_undo(core, input)).await;
    }
    if action == "undo_history" {
        return crate::window_undo::history(core);
    }
    let cancel = CancellationToken::new();
    let _lease = CancelOnDrop(cancel.clone());
    let _lock = ACTIONS.lock().await;
    enabled(core, action == "layout_save", &cancel)?;
    match action {
        "layouts" => {
            let rows=core.store.query("SELECT json_value FROM settings WHERE key GLOB 'window_layout.*' ORDER BY key LIMIT 101",&[])?;
            let mut layouts = Vec::new();
            for row in rows.as_array().ok_or("LAYOUT_QUERY")? {
                let layout: Layout =
                    serde_json::from_str(row["json_value"].as_str().ok_or("LAYOUT_RECORD")?)
                        .map_err(|_| "LAYOUT_RECORD")?;
                validate(&layout)?;
                layouts.push(layout);
            }
            if layouts.len() > 100 {
                return Err("LAYOUT_COUNT_LIMIT".into());
            }
            layouts.sort_by_key(|a| a.name.to_lowercase());
            Ok(json!(layouts))
        }
        "layout_get" => serde_json::to_value(load(core, input["id"].as_str().ok_or("LAYOUT_ID")?)?)
            .map_err(|_| "LAYOUT_JSON".into()),
        "layout_save" => {
            let label = name(input["name"].as_str().ok_or("LAYOUT_NAME")?)?;
            let monitors = core.monitors().await?;
            let targets = input["body"].as_array().ok_or("LAYOUT_TARGET")?;
            if targets.iter().any(|target| {
                target["process_id"]
                    .as_u64()
                    .filter(|pid| *pid > 0 && *pid <= i32::MAX as u64)
                    .is_none()
            }) {
                return Err("LAYOUT_TARGET".into());
            }
            let snapshots = crate::window_snapshot::capture(&input["body"], true)?;
            let mut windows = Vec::with_capacity(snapshots.len());
            for snapshot in snapshots {
                if snapshot["placement"]["show_cmd"] != 1 {
                    return Err("LAYOUT_WINDOW_NOT_NORMAL".into());
                }
                // Persist only the original layout contract, without titles or extra metadata.
                let mut placement = serde_json::Map::new();
                for field in [
                    "hwnd",
                    "process_id",
                    "process_started",
                    "window_identity",
                    "x",
                    "y",
                    "width",
                    "height",
                ] {
                    placement.insert(field.into(), snapshot[field].clone());
                }
                windows.push(
                    serde_json::from_value::<Placement>(Value::Object(placement))
                        .map_err(|_| "LAYOUT_SNAPSHOT")?,
                );
            }
            enabled(core, true, &cancel)?;
            if monitors_match(&monitors, &core.monitors().await?).is_err() {
                return Err("LAYOUT_MONITORS_CHANGED_DURING_CAPTURE".into());
            }
            save(core, &label, windows, monitors)
        }
        "layout_delete" => {
            let layout = selected(core, input)?;
            let n = core.store.execute(
                "DELETE FROM settings WHERE key=? AND json_value=?",
                &[
                    json!(key(&layout.id)?),
                    json!(serde_json::to_string(&layout).map_err(|_| "LAYOUT_JSON")?),
                ],
            )?;
            if n != 1 {
                return Err("LAYOUT_CHANGED_REVIEW_REQUIRED".into());
            }
            Ok(json!({"deleted":layout.id}))
        }
        _ => {
            process::native_cancellable(
                &core.native,
                &format!("window.{action}"),
                input,
                cancel.clone(),
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Arc<Core> {
        let root = std::env::temp_dir().join(format!("ap-layout-fixture-{}", storage::id()));
        Arc::new(
            Core::new(
                root,
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        )
    }
    fn monitor() -> Monitor {
        Monitor {
            id: "fixture-negative".into(),
            work: crate::geometry::Rect {
                x: -1920.,
                y: 0.,
                width: 1920.,
                height: 1040.,
            },
            scale: 1.5,
        }
    }
    fn placement() -> Placement {
        Placement {
            hwnd: 123,
            process_id: 100,
            process_started: "123456789".into(),
            window_identity: Some("123456789abcdef0".into()),
            x: -1900,
            y: 20,
            width: 720,
            height: 540,
        }
    }
    fn record(c: &Core) -> Value {
        save(c, " Trabajo ", vec![placement()], vec![monitor()]).unwrap()
    }
    #[test]
    fn persistence_names_fingerprint_review_and_corrupt_data_are_checked() {
        let c = fixture();
        let saved = record(&c);
        assert_eq!(saved["name"], "Trabajo");
        assert!(save(&c, "trabajo", vec![placement()], vec![monitor()])
            .unwrap_err()
            .starts_with("LAYOUT_NAME_EXISTS"));
        assert!(name("\n").is_err());
        assert!(name("a\nb").is_err());
        let restarted = Core::new(c.root.clone(), c.resource.clone()).unwrap();
        let id = saved["id"].as_str().unwrap();
        let loaded = load(&restarted, id).unwrap();
        assert_eq!(loaded.fingerprint, saved["fingerprint"]);
        let input = json!({"id":id,"fingerprint":loaded.fingerprint});
        let review = review(&restarted, &input).unwrap();
        assert_eq!(review["windows"][0]["x"], -1900);
        assert_eq!(review["windows"][0]["process_started"], "123456789");
        assert!(review["windows"][0].get("title").is_none());
        assert_eq!(
            selected(&c, &json!({"id":id,"fingerprint":"0".repeat(64)})).unwrap_err(),
            "LAYOUT_CHANGED_REVIEW_REQUIRED"
        );
        let mut corrupt = saved.clone();
        corrupt["windows"][0]["process_id"] = json!(101);
        c.store.set_setting(&key(id).unwrap(), &corrupt).unwrap();
        assert_eq!(load(&c, id).unwrap_err(), "LAYOUT_FINGERPRINT");
        assert!(load(&c, "../id").is_err());
    }
    #[test]
    fn legacy_layout_fingerprint_remains_readable_but_apply_requires_new_capture() {
        let c = fixture();
        let saved = record(&c);
        let mut legacy: Layout = serde_json::from_value(saved).unwrap();
        legacy.version = 1;
        legacy.windows[0].window_identity = None;
        let raw = serde_json::to_string(&legacy).unwrap();
        assert!(!raw.contains("window_identity"));
        legacy.fingerprint = fingerprint(&legacy).unwrap();
        c.store
            .set_setting(&key(&legacy.id).unwrap(), &json!(legacy))
            .unwrap();
        let restarted = Core::new(c.root.clone(), c.resource.clone()).unwrap();
        let loaded = load(&restarted, &legacy.id).unwrap();
        assert_eq!(loaded.fingerprint, legacy.fingerprint);
        let input = json!({"id":legacy.id,"fingerprint":legacy.fingerprint});
        assert!(review(&restarted, &input).is_ok());
        assert_eq!(
            apply_targets(&restarted, &input).unwrap_err(),
            "LAYOUT_WINDOW_IDENTITY_MISSING: capture a new layout"
        );
    }
    #[test]
    fn recreated_identity_bounds_duplicates_and_monitor_changes_are_rejected() {
        let c = fixture();
        let saved = record(&c);
        let mut layout: Layout = serde_json::from_value(saved).unwrap();
        assert!(monitors_match(&layout.monitors, &[monitor()]).is_ok());
        let mut changed = monitor();
        changed.scale = 1.;
        assert!(monitors_match(&layout.monitors, &[changed]).is_err());
        assert!(monitors_match(&layout.monitors, &[]).is_err());
        let mut changed = monitor();
        changed.work.x = 0.;
        assert!(monitors_match(&layout.monitors, &[changed]).is_err());
        layout.windows[0].process_started = "000123".into();
        assert_eq!(validate(&layout).unwrap_err(), "LAYOUT_PLACEMENT");
        layout.windows[0] = placement();
        layout.windows.push(placement());
        assert_eq!(validate(&layout).unwrap_err(), "LAYOUT_PLACEMENT");
        layout.windows.pop();
        layout.windows[0].width = 100001;
        assert_eq!(validate(&layout).unwrap_err(), "LAYOUT_PLACEMENT");
        layout.windows[0] = placement();
        layout.monitors[0].scale = f64::NAN;
        assert_eq!(validate(&layout).unwrap_err(), "LAYOUT_MONITOR");
    }
    #[tokio::test]
    async fn schemas_permissions_privacy_and_disabled_modules_are_enforced() {
        let save_schema = crate::catalog::tool_schema("window.layout_save", "name,body");
        let schema = jsonschema::validator_for(&save_schema).unwrap();
        let valid = json!({"name":"Trabajo","body":[{"hwnd":123,"process_id":100}]});
        assert!(schema.is_valid(&valid));
        for v in [
            json!({"name":"Trabajo","body":[]}),
            json!({"name":"Trabajo","body":[{"hwnd":123}]}),
            json!({"name":"Trabajo","body":[{"hwnd":123,"process_id":100,"x":1}]}),
            json!({"name":"Trabajo","body":[{"hwnd":"123","process_id":100}]}),
        ] {
            assert!(!schema.is_valid(&v));
        }
        assert_eq!(
            crate::policy::classify("window.layout_save", &valid),
            crate::policy::Risk::Contextual
        );
        assert_eq!(
            crate::policy::classify("window.layout_apply", &json!({})),
            crate::policy::Risk::Contextual
        );
        assert_eq!(
            crate::policy::classify("window.layout_delete", &json!({})),
            crate::policy::Risk::Mandatory
        );
        let c = fixture();
        let saved = record(&c);
        let input = json!({"id":saved["id"],"fingerprint":saved["fingerprint"]});
        assert!(c
            .execute("window.layout_apply", input.clone())
            .await
            .is_err());
        let events = c
            .store
            .query("SELECT status,input_summary FROM activity_events", &[])
            .unwrap();
        assert_eq!(events[0]["status"], "denied");
        let review: Value =
            serde_json::from_str(events[0]["input_summary"].as_str().unwrap()).unwrap();
        assert_eq!(review["name"], "Trabajo");
        assert_eq!(review["windows"][0]["hwnd"], 123);
        assert!(c
            .execute("window.layout_delete", input.clone())
            .await
            .is_err());
        assert!(load(&c, saved["id"].as_str().unwrap()).is_ok());
        c.store.set_setting("privacy", &json!(true)).unwrap();
        assert_eq!(
            c.execute("window.layout_save", valid).await.unwrap_err(),
            "PRIVACY_DENIED"
        );
        c.store
            .execute(
                "UPDATE module_configs SET enabled=0 WHERE module_id='window'",
                &[],
            )
            .unwrap();
        assert_eq!(
            c.execute("window.layouts", json!({})).await.unwrap_err(),
            "TOOL_DISABLED_OR_UNKNOWN"
        );
        assert_eq!(
            dispatch(&c, "layout_apply", &input).await.unwrap_err(),
            "TOOL_DISABLED_OR_UNKNOWN"
        );
    }
    #[tokio::test]
    async fn listing_and_conditional_deletion_preserve_other_records() {
        let c = fixture();
        let first = record(&c);
        c.store
            .set_setting(
                "windowXlayout.unrelated",
                &json!({"unrelated_setting":true}),
            )
            .unwrap();
        let second = save(&c, "Ocio", vec![placement()], vec![monitor()]).unwrap();
        let rows = dispatch(&c, "layouts", &json!({})).await.unwrap();
        assert_eq!(rows[0]["name"], "Ocio");
        let bad = json!({"id":first["id"],"fingerprint":"0".repeat(64)});
        assert_eq!(
            dispatch(&c, "layout_delete", &bad).await.unwrap_err(),
            "LAYOUT_CHANGED_REVIEW_REQUIRED"
        );
        let input = json!({"id":first["id"],"fingerprint":first["fingerprint"]});
        dispatch(&c, "layout_delete", &input).await.unwrap();
        assert!(load(&c, first["id"].as_str().unwrap()).is_err());
        assert!(load(&c, second["id"].as_str().unwrap()).is_ok());
        let cancel = CancellationToken::new();
        let lease = CancelOnDrop(cancel.clone());
        drop(lease);
        assert_eq!(enabled(&c, false, &cancel).unwrap_err(), "LAYOUT_CANCELLED");
    }
    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "real owned WinForms window: capture, persistence and restore geometry"]
    async fn real_owned_window_saved_layout_roundtrip() {
        let c = fixture();
        let metadata = c.root.join("temp/layout-fixture.json");
        let shell = std::path::PathBuf::from(std::env::var("SystemRoot").unwrap())
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let mut command = tokio::process::Command::new(shell);
        command
            .args(["-NoProfile", "-STA", "-File"])
            .arg(c.resource.join("tests/fixtures/uia-window.ps1"))
            .arg("-MetadataPath")
            .arg(&metadata);
        command
            .creation_flags(0x08000000)
            .kill_on_drop(true)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = command.spawn().unwrap();
        let _job = crate::windows_native::ProcessJob::attach(&child).unwrap();
        let mut info = Value::Null;
        for _ in 0..200 {
            if let Ok(raw) = std::fs::read(&metadata) {
                if let Ok(value) = serde_json::from_slice(&raw) {
                    info = value;
                    break;
                }
            }
            if child.try_wait().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(
            info["hwnd"].as_i64().unwrap_or_default() > 0,
            "owned fixture metadata required"
        );
        let save_input = json!({"name":"Fixture roundtrip","body":[{"hwnd":info["hwnd"],"process_id":info["process_id"]}]});
        c.grants.lock().unwrap().insert(
            format!("window.layout_save:{save_input}"),
            crate::policy::Risk::Contextual,
        );
        let saved = c.execute("window.layout_save", save_input).await.unwrap();
        let original = saved["windows"][0].clone();
        assert!(original.get("title").is_none());
        let mut moved = original.clone();
        moved["x"] = json!(original["x"].as_i64().unwrap() + 40);
        moved["y"] = json!(original["y"].as_i64().unwrap() + 20);
        process::native(&c.native, "window.move", &moved)
            .await
            .unwrap();
        let restarted = Arc::new(Core::new(c.root.clone(), c.resource.clone()).unwrap());
        let input = json!({"id":saved["id"],"fingerprint":saved["fingerprint"]});
        let review = review(&restarted, &input).unwrap();
        restarted.grants.lock().unwrap().insert(
            format!("window.layout_apply:{review}"),
            crate::policy::Risk::Contextual,
        );
        let applied = restarted
            .execute("window.layout_apply", input.clone())
            .await
            .unwrap();
        let after = &applied["result"]["windows"][0]["after"];
        for field in ["x", "y", "width", "height"] {
            assert_eq!(after[field], original[field]);
        }
        let mut stale = original.clone();
        stale["process_started"] = json!("1");
        assert!(
            process::native(&c.native, "window.set_layout", &json!({"body":[stale]}))
                .await
                .is_err()
        );
        let current = process::native(
            &c.native,
            "window.get_layout",
            &json!({"body":[{"hwnd":info["hwnd"],"process_id":info["process_id"]}]}),
        )
        .await
        .unwrap();
        for field in ["x", "y", "width", "height"] {
            assert_eq!(current["windows"][0][field], original[field]);
        }
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        assert!(restarted
            .execute("window.layout_apply", input.clone())
            .await
            .is_err());
        assert!(restarted
            .execute("window.layout_delete", input.clone())
            .await
            .is_err());
        dispatch(&restarted, "layout_delete", &input).await.unwrap();
        println!("WINDOW_LAYOUT_REAL_CONTRACT_PASS: owned fixture, concrete permission review, persisted restart, geometric restore, stale process timestamp rejection, closed target and mandatory deletion denial; no foreground input");
    }
}
