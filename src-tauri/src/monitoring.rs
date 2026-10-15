use crate::{runtime::Core, storage};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path, sync::Arc};

const KINDS: &[&str] = &[
    "file",
    "folder",
    "download",
    "port",
    "process",
    "window",
    "page",
    "ui_element",
];
const LIMIT: usize = 1000;
fn field<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v[key].as_str().ok_or(format!("WATCH_INPUT_{key}"))
}
fn validate(c: &Core, kind: &str, config: &Value) -> Result<(), String> {
    if !KINDS.contains(&kind) {
        return Err("WATCH_KIND_UNSUPPORTED".into());
    }
    let path = field(config, "path")?;
    match kind {
        "file" | "folder" | "download" => {
            let p = c.path(path, true)?;
            if p.exists() && (kind == "file") == p.is_dir() {
                return Err("WATCH_PATH_TYPE".into());
            }
        }
        "page" | "ui_element" => crate::watch_extra::validate(kind, config)?,
        "port" => {
            let address: std::net::SocketAddr = path.parse().map_err(|_| "WATCH_PORT_ADDRESS")?;
            if address.port() == 0 {
                return Err("WATCH_PORT_ADDRESS".into());
            }
        }
        "process" | "window" => {
            if path.parse::<u64>().ok().is_none_or(|id| {
                id == 0 || id > i64::MAX as u64 || (kind == "process" && id > i32::MAX as u64)
            }) {
                return Err("WATCH_TARGET_ID".into());
            }
        }
        _ => unreachable!(),
    }
    let condition = config["condition"].as_str().unwrap_or("changed");
    let allowed = match kind {
        "download" => &["ready"][..],
        "file" | "folder" | "page" | "ui_element" => &["changed", "exists", "missing"][..],
        _ => &["changed", "open", "closed"][..],
    };
    if !allowed.contains(&condition) {
        return Err("WATCH_CONDITION".into());
    }
    Ok(())
}
pub fn create(c: &Core, owner: &str, kind: &str, input: &Value) -> Result<Value, String> {
    if (owner == "files" && !["file", "folder"].contains(&kind))
        || (owner == "downloads" && kind != "download")
    {
        return Err("WATCH_OWNER_KIND".into());
    }
    if c.store.setting("privacy") == true {
        return Err("PRIVACY_WATCH_DENIED".into());
    }
    let body = input["body"].as_object();
    let condition = body
        .and_then(|b| b.get("condition"))
        .and_then(Value::as_str)
        .unwrap_or(if kind == "download" {
            "ready"
        } else {
            "changed"
        });
    let mut config = json!({"path":input["path"],"condition":condition,"recursive":input["recursive"].as_bool().unwrap_or(false)});
    crate::watch_extra::add_config(kind, input, &mut config);
    validate(c, kind, &config)?;
    let count = c.store.query("SELECT COUNT(*) AS n FROM watches", &[])?;
    if count[0]["n"].as_u64().unwrap_or(100) >= 100 {
        return Err("WATCH_COUNT_LIMIT".into());
    }
    let id = storage::id();
    c.store.execute("INSERT INTO watches(id,owner_module,kind,config_json,created_at,updated_at) VALUES(?,?,?,?,?,?)", &[json!(id),json!(owner),json!(kind),json!(config.to_string()),json!(storage::now()),json!(storage::now())])?;
    Ok(json!({"id":id,"kind":kind,"condition":condition,"interval_seconds":30,"status":"pending"}))
}
pub fn dispatch(c: &Core, action: &str, v: &Value) -> Result<Value, String> {
    match action {
        "create" => create(c, "monitoring", field(v, "kind")?, v),
        "list" => {
            let mut rows = c
                .store
                .query("SELECT * FROM watches ORDER BY created_at DESC", &[])?;
            for row in rows.as_array_mut().ok_or("WATCH_ROWS")? {
                row["config"] =
                    serde_json::from_str(row["config_json"].as_str().ok_or("WATCH_CONFIG")?)
                        .map_err(|_| "WATCH_CONFIG")?;
                row.as_object_mut().unwrap().remove("config_json");
                row.as_object_mut().unwrap().remove("last_json");
                row["effective_enabled"] =
                    json!(row["enabled"] == 1 && c.enabled(field(row, "owner_module")?));
            }
            Ok(rows)
        }
        "update" => {
            let enabled = v["body"]["enabled"].as_bool().ok_or("WATCH_ENABLED")?;
            if c.store.execute(
                "UPDATE watches SET enabled=?,revision=revision+1,updated_at=? WHERE id=?",
                &[json!(enabled), json!(storage::now()), v["id"].clone()],
            )? != 1
            {
                return Err("WATCH_NOT_FOUND".into());
            }
            Ok(json!({"enabled":enabled}))
        }
        "delete" => {
            if c.store
                .execute("DELETE FROM watches WHERE id=?", &[v["id"].clone()])?
                != 1
            {
                return Err("WATCH_NOT_FOUND".into());
            }
            Ok(json!({"deleted":true}))
        }
        _ => Err("WATCH_ACTION".into()),
    }
}
pub fn import_legacy(c: &Core) -> Result<(), String> {
    if c.store.setting("watches_sqlite_migrated") == true {
        return Ok(());
    }
    if let Some(watches) = c.store.setting("watches").as_array() {
        for watch in watches {
            let Some(kind) = watch["kind"].as_str().filter(|kind| KINDS.contains(kind)) else {
                continue;
            };
            let Some(id) = watch["id"].as_str() else {
                continue;
            };
            let config = json!({"path":watch["path"],"condition":if kind=="download" {"ready"} else {"changed"},"recursive":false});
            c.store.execute("INSERT OR IGNORE INTO watches(id,owner_module,kind,config_json,created_at,updated_at) VALUES(?,'monitoring',?,?,?,?)", &[json!(id),json!(kind),json!(config.to_string()),json!(storage::now()),json!(storage::now())])?;
        }
    }
    c.store.set_setting("watches_sqlite_migrated", &json!(true))
}
pub fn owned(c: &Core, owner: &str, action: &str, input: &Value) -> Result<Value, String> {
    let rows = c.store.query(
        "SELECT owner_module FROM watches WHERE id=?",
        &[input["id"].clone()],
    )?;
    if rows[0]["owner_module"] != owner {
        return Err("WATCH_NOT_FOUND".into());
    }
    dispatch(c, action, input)
}
fn metadata(path: &Path) -> Result<Value, String> {
    let m = std::fs::metadata(path).map_err(|_| "WATCH_METADATA")?;
    Ok(
        json!({"size":m.len(),"directory":m.is_dir(),"modified":m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d|d.as_nanos().to_string())}),
    )
}
fn temporary(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".part", ".partial", ".tmp", ".crdownload", ".download"]
        .iter()
        .any(|extension| lower.ends_with(extension))
}
fn directory(c: &Core, root: &Path, recursive: bool) -> Result<Value, String> {
    let mut stack = vec![root.to_path_buf()];
    let mut entries = BTreeMap::new();
    let mut visited = 0;
    while let Some(dir) = stack.pop() {
        // Revalidate each directory, including paths discovered on a previous iteration.
        let dir = c.path(dir.to_string_lossy().as_ref(), false)?;
        for item in std::fs::read_dir(dir).map_err(|_| "WATCH_DIRECTORY")? {
            let path = item.map_err(|_| "WATCH_DIRECTORY_ENTRY")?.path();
            visited += 1;
            if visited > LIMIT {
                return Err("WATCH_ENTRY_LIMIT".into());
            }
            let Ok(path) = c.path(path.to_string_lossy().as_ref(), false) else {
                continue;
            };
            let relative = path
                .strip_prefix(root)
                .map_err(|_| "WATCH_SCOPE")?
                .to_string_lossy()
                .replace('\\', "/");
            let meta = metadata(&path)?;
            if recursive && meta["directory"] == true {
                stack.push(path);
            }
            entries.insert(relative, meta);
        }
    }
    Ok(json!({"present":true,"entries":entries}))
}
async fn snapshot(
    c: &Arc<Core>,
    kind: &str,
    config: &Value,
    previous: &Value,
) -> Result<Value, String> {
    validate(c, kind, config)?;
    let raw = field(config, "path")?;
    match kind {
        "file" | "folder" | "download" => {
            let path = c.path(raw, true)?;
            if !path.exists() {
                return Ok(json!({"present":false}));
            }
            if kind == "file" {
                let mut result = metadata(&path)?;
                result["present"] = json!(true);
                return Ok(result);
            }
            let mut result = directory(c, &path, config["recursive"] == true)?;
            if kind == "download" {
                let mut stable = BTreeMap::new();
                let mut ready = BTreeMap::new();
                for (name, entry) in result["entries"].as_object().ok_or("WATCH_ENTRIES")? {
                    if entry["directory"] == true || temporary(name) {
                        continue;
                    }
                    let equal = previous["entries"][name] == *entry;
                    let count = if previous.is_null() {
                        2
                    } else if equal {
                        previous["stable"][name]
                            .as_u64()
                            .unwrap_or(0)
                            .saturating_add(1)
                            .min(2)
                    } else {
                        1
                    };
                    stable.insert(name.clone(), count);
                    if count >= 2 {
                        ready.insert(name.clone(), entry.clone());
                    }
                }
                result["stable"] = json!(stable);
                result["ready"] = json!(ready);
            }
            Ok(result)
        }
        "port" => {
            let address: std::net::SocketAddr = raw.parse().map_err(|_| "WATCH_PORT_ADDRESS")?;
            let present = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                tokio::net::TcpStream::connect(address),
            )
            .await
            .is_ok_and(|r| r.is_ok());
            Ok(json!({"present":present}))
        }
        "process" => {
            crate::windows_native::watch_process(raw.parse().map_err(|_| "WATCH_TARGET_ID")?)
        }
        "window" => {
            crate::windows_native::watch_window(raw.parse().map_err(|_| "WATCH_TARGET_ID")?)
        }
        "page" | "ui_element" => crate::watch_extra::snapshot(c, kind, config).await,
        _ => Err("WATCH_KIND_UNSUPPORTED".into()),
    }
}
fn changed(config: &Value, previous: &Value, current: &Value) -> bool {
    if previous.is_null() {
        return false;
    }
    match config["condition"].as_str().unwrap_or("changed") {
        "exists" | "open" => previous["present"] != true && current["present"] == true,
        "missing" | "closed" => previous["present"] == true && current["present"] != true,
        "ready" => current["ready"].as_object().is_some_and(|ready| {
            ready
                .iter()
                .any(|(name, value)| previous["ready"][name] != *value)
        }),
        _ => previous != current,
    }
}
fn summary(value: &Value) -> Value {
    json!({"present":value["present"],"entry_count":value["entries"].as_object().map(|e|e.len()),"ready_count":value["ready"].as_object().map(|e|e.len())})
}
pub async fn poll(c: &Arc<Core>) -> Result<Value, String> {
    let _guard = c.watch_poll.lock().await;
    if c.store.setting("privacy") == true {
        return Ok(json!([]));
    }
    let watches = c.store.query(
        "SELECT * FROM watches WHERE enabled=1 ORDER BY created_at LIMIT 100",
        &[],
    )?;
    let mut events = Vec::new();
    let mut updated = false;
    for watch in watches.as_array().ok_or("WATCH_ROWS")? {
        if !c.enabled(field(watch, "owner_module")?) || c.store.setting("privacy") == true {
            continue;
        }
        let config: Value =
            serde_json::from_str(field(watch, "config_json")?).map_err(|_| "WATCH_CONFIG")?;
        let previous: Value = watch["last_json"]
            .as_str()
            .and_then(|v| serde_json::from_str(v).ok())
            .unwrap_or(Value::Null);
        let authority = crate::media::authority(c)?;
        let observation = snapshot(c, field(watch, "kind")?, &config, &previous).await;
        if crate::media::authority(c)? != authority {
            continue;
        }
        if c.store.setting("privacy") == true || !c.enabled(field(watch, "owner_module")?) {
            continue;
        }
        let (current, status, error) = match observation {
            Ok(current) => (current, "active", Value::Null),
            Err(error) => (previous.clone(), "error", json!(error)),
        };
        let notify = if status == "error" {
            watch["status"] != "error" || watch["error_code"] != error
        } else {
            watch["status"] == "error" || changed(&config, &previous, &current)
        };
        // Revision check preserves pause/delete updates made while native/network I/O was in flight.
        let written=c.store.execute("UPDATE watches SET last_json=?,status=?,error_code=?,revision=revision+1,updated_at=? WHERE id=? AND revision=? AND enabled=1", &[if status=="error" && previous.is_null(){Value::Null}else{json!(current.to_string())},json!(status),error.clone(),json!(storage::now()),watch["id"].clone(),watch["revision"].clone()])?;
        if written == 1 && (notify || watch["status"] != status) {
            updated = true;
        }
        if written != 1 || !notify {
            continue;
        }
        let event = json!({"event":if status=="error"{"watch_error"}else if watch["status"]=="error"{"watch_recovered"}else{"watch_changed"},"watch_id":watch["id"],"kind":watch["kind"],"path":config["path"],"condition":config["condition"],"current":summary(&current),"error_code":error});
        c.store.audit(
            "monitoring.event",
            "free",
            &json!({"watch_id":watch["id"]}),
            if status == "error" {
                "error"
            } else {
                "succeeded"
            },
            &event,
            0,
        )?;
        c.emit("alfredpet:activity", &json!({}));
        c.attention("completed_requires_attention", false).await;
        events.push(event);
    }
    if updated {
        c.emit("alfredpet:watches", &json!({}));
    }
    Ok(json!(events))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Arc<Core> {
        Arc::new(
            Core::new(
                std::env::temp_dir().join(format!("ap-watch-{}", storage::id())),
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        )
    }
    #[tokio::test]
    async fn folder_file_changes_restart_privacy_revocation_and_pause() {
        let c = fixture();
        let root = c.root.clone();
        let dir = root.join("temp/watched");
        std::fs::create_dir(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "a").unwrap();
        let w = create(&c, "files", "folder", &json!({"path":dir,"recursive":true})).unwrap();
        let f = create(
            &c,
            "files",
            "file",
            &json!({"path":file,"body":{"condition":"missing"}}),
        )
        .unwrap();
        assert_eq!(poll(&c).await.unwrap(), json!([]));
        std::fs::write(dir.join("new.txt"), "new").unwrap();
        assert_eq!(poll(&c).await.unwrap().as_array().unwrap().len(), 1);
        assert_eq!(poll(&c).await.unwrap(), json!([]));
        drop(c);
        let c = Arc::new(
            Core::new(
                root.clone(),
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        assert_eq!(poll(&c).await.unwrap(), json!([]));
        c.store.set_setting("privacy", &json!(true)).unwrap();
        std::fs::remove_file(&file).unwrap();
        assert_eq!(poll(&c).await.unwrap(), json!([]));
        c.store.set_setting("privacy", &json!(false)).unwrap();
        assert_eq!(poll(&c).await.unwrap().as_array().unwrap().len(), 2);
        dispatch(
            &c,
            "update",
            &json!({"id":w["id"],"body":{"enabled":false}}),
        )
        .unwrap();
        dispatch(&c, "delete", &json!({"id":f["id"]})).unwrap();
        std::fs::write(dir.join("last.txt"), "last").unwrap();
        assert_eq!(poll(&c).await.unwrap(), json!([]));
        let external = std::env::temp_dir().join(format!("ap-external-{}", storage::id()));
        std::fs::create_dir(&external).unwrap();
        c.store
            .set_setting("allowed_roots", &json!([external]))
            .unwrap();
        let revoked = create(&c, "files", "folder", &json!({"path":external})).unwrap();
        poll(&c).await.unwrap();
        c.store.set_setting("allowed_roots", &json!([])).unwrap();
        let events = poll(&c).await.unwrap();
        assert_eq!(events[0]["watch_id"], revoked["id"]);
        assert_eq!(events[0]["event"], "watch_error");
        assert_eq!(poll(&c).await.unwrap(), json!([]));
        let audit = c
            .store
            .query(
                "SELECT COUNT(*) AS n FROM activity_events WHERE tool_id='monitoring.event'",
                &[],
            )
            .unwrap();
        assert_eq!(audit[0]["n"], 4);
        drop(c);
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir(external).unwrap();
    }
    #[tokio::test]
    async fn downloads_wait_for_stability_ignore_temporary_and_do_not_execute() {
        let c = fixture();
        let root = c.root.clone();
        let dir = root.join("temp/downloads");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("existing.exe"), "fixture").unwrap();
        create(&c, "downloads", "download", &json!({"path":dir})).unwrap();
        assert_eq!(poll(&c).await.unwrap(), json!([]));
        assert_eq!(poll(&c).await.unwrap(), json!([]));
        let partial = dir.join("new.exe.crdownload");
        std::fs::write(&partial, "partial").unwrap();
        assert_eq!(poll(&c).await.unwrap(), json!([]));
        assert_eq!(poll(&c).await.unwrap(), json!([]));
        let done = dir.join("new.exe");
        std::fs::rename(partial, &done).unwrap();
        assert_eq!(poll(&c).await.unwrap(), json!([]));
        std::fs::write(&done, "still writing").unwrap();
        assert_eq!(poll(&c).await.unwrap(), json!([]));
        let events = poll(&c).await.unwrap();
        assert_eq!(events[0]["condition"], "ready");
        assert_eq!(events[0]["current"]["ready_count"], 2);
        assert_eq!(poll(&c).await.unwrap(), json!([]));
        c.store
            .execute(
                "UPDATE module_configs SET enabled=0 WHERE module_id='downloads'",
                &[],
            )
            .unwrap();
        std::fs::write(dir.join("disabled.txt"), "disabled").unwrap();
        assert_eq!(poll(&c).await.unwrap(), json!([]));
        drop(c);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn ports_and_processes_observe_real_lifecycle() {
        let c = fixture();
        c.store
            .execute(
                "UPDATE module_configs SET enabled=1 WHERE module_id='monitoring'",
                &[],
            )
            .unwrap();
        let root = c.root.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        create(&c,"monitoring","port",&json!({"path":listener.local_addr().unwrap().to_string(),"body":{"condition":"closed"}})).unwrap();
        poll(&c).await.unwrap();
        drop(listener);
        assert_eq!(poll(&c).await.unwrap()[0]["condition"], "closed");
        use tokio::io::AsyncBufReadExt;
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "monitoring::tests::owned_lifecycle_child",
                "--ignored",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("ALFREDPET_OWNED_LIFECYCLE", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .creation_flags(0x08000000)
            .spawn()
            .unwrap();
        let job = crate::windows_native::ProcessJob::attach(&child).unwrap();
        let mut output = tokio::io::BufReader::new(child.stdout.take().unwrap());
        let mut ready = String::new();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                ready.clear();
                assert_ne!(output.read_line(&mut ready).await.unwrap(), 0);
                if ready.contains("OWNED_LIFECYCLE_READY") {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert!(ready.contains("OWNED_LIFECYCLE_READY"));
        assert!(child.try_wait().unwrap().is_none());
        create(
            &c,
            "monitoring",
            "process",
            &json!({"path":child.id().unwrap().to_string(),"body":{"condition":"closed"}}),
        )
        .unwrap();
        poll(&c).await.unwrap();
        assert!(child.try_wait().unwrap().is_none());
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        assert_eq!(poll(&c).await.unwrap()[0]["condition"], "closed");
        drop(job);
        drop(c);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[ignore = "Owned child for the real process lifecycle contract"]
    fn owned_lifecycle_child() {
        assert_eq!(
            std::env::var("ALFREDPET_OWNED_LIFECYCLE").as_deref(),
            Ok("1")
        );
        use std::io::Write;
        println!("OWNED_LIFECYCLE_READY");
        std::io::stdout().flush().unwrap();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).unwrap();
    }
    #[test]
    fn schema_three_upgrade_retains_data_imports_watches_once_and_backs_up() {
        let c = fixture();
        let root = c.root.clone();
        let file = root.join("temp/legacy.txt");
        std::fs::write(&file, "legacy").unwrap();
        c.store
            .remember(&json!({"content":"A retained project fact","title":"migration fixture"}))
            .unwrap();
        c.store
            .set_setting(
                "watches",
                &json!([{"id":"legacy-watch","kind":"file","path":file}]),
            )
            .unwrap();
        c.store
            .set_setting("watches_sqlite_migrated", &json!(false))
            .unwrap();
        c.store.execute("DROP TABLE watches", &[]).unwrap();
        c.store.execute("DROP TABLE file_plans", &[]).unwrap();
        c.store
            .0
            .lock()
            .unwrap()
            .execute_batch(crate::clipboard::TEST_REMOVE_FUTURE_SCHEMA)
            .unwrap();
        c.store.0.lock().unwrap().execute_batch("DROP TABLE file_cleanup_plans;ALTER TABLE undo_records DROP COLUMN metadata_json;DROP TABLE file_rollbacks;ALTER TABLE undo_records DROP COLUMN staging;ALTER TABLE undo_records DROP COLUMN object_id;DROP TRIGGER projects_au;DROP TRIGGER semantic_memory_update;DROP TRIGGER semantic_memory_delete;DROP TRIGGER semantic_summary_update;DROP TRIGGER semantic_summary_delete;DROP TABLE semantic_embeddings;").unwrap();
        c.store.execute("PRAGMA user_version=3", &[]).unwrap();
        c.store
            .execute("DROP TABLE memory_candidates", &[])
            .unwrap();
        c.store
            .execute("DROP TABLE conversation_analysis", &[])
            .unwrap();
        c.store
            .execute("ALTER TABLE memories DROP COLUMN metadata_json", &[])
            .unwrap();
        drop(c);
        let c = Core::new(
            root.clone(),
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap();
        assert_eq!(
            c.store.query("PRAGMA user_version", &[]).unwrap()[0]["user_version"],
            crate::storage::SCHEMA_VERSION
        );
        assert_eq!(
            c.store.query("PRAGMA quick_check(1)", &[]).unwrap()[0]["quick_check"],
            "ok"
        );
        assert_eq!(
            dispatch(&c, "list", &json!({})).unwrap()[0]["id"],
            "legacy-watch"
        );
        assert_eq!(
            c.store
                .query("SELECT COUNT(*) AS n FROM memories", &[])
                .unwrap()[0]["n"],
            1
        );
        let backups = std::fs::read_dir(root.join("data"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("pre-v4-"))
            .collect::<Vec<_>>();
        assert!(backups.len() >= 2);
        assert!(backups.iter().any(|backup| {
            let db = rusqlite::Connection::open_with_flags(
                backup.path(),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            db.query_row("SELECT COUNT(*) FROM memories", [], |r| r.get::<_, i64>(0))
                .unwrap()
                == 1
        }));
        c.store
            .execute(
                "INSERT INTO file_plans VALUES('interrupted',?,'[]','running',?,?)",
                &[
                    json!(root.join("temp")),
                    json!(storage::now()),
                    json!(storage::now()),
                ],
            )
            .unwrap();
        drop(c);
        let c = Core::new(
            root.clone(),
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap();
        assert_eq!(
            dispatch(&c, "list", &json!({}))
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            c.store
                .query("SELECT state FROM file_plans WHERE id='interrupted'", &[])
                .unwrap()[0]["state"],
            "interrupted"
        );
        drop(c);
        std::fs::remove_dir_all(root).unwrap();
    }
}
