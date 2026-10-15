//! Explicit, bounded support export. Raw logs, content and credentials never enter the archive.
use crate::{asset_files, policy, runtime::Core, storage};
use serde_json::{json, Value};
use std::{
    io::{Cursor, Write},
    sync::Arc,
};
#[cfg(windows)]
fn os() -> Value {
    #[repr(C)]
    struct Version {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        service_pack: [u16; 128],
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(version: *mut Version) -> i32;
    }
    let mut v = Version {
        size: std::mem::size_of::<Version>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        platform: 0,
        service_pack: [0; 128],
    };
    if unsafe { RtlGetVersion(&mut v) } != 0 {
        return json!({"os":"windows","architecture":std::env::consts::ARCH,"version":"UNAVAILABLE"});
    }
    json!({"os":"windows","architecture":std::env::consts::ARCH,"major":v.major,"minor":v.minor,"build":v.build})
}
#[cfg(not(windows))]
fn os() -> Value {
    json!({"os":std::env::consts::OS,"architecture":std::env::consts::ARCH})
}
fn safe_label(value: &Value) -> Value {
    match value.as_str() {
        Some(s)
            if s.len() <= 100
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                && !policy::contains_secret(s) =>
        {
            json!(s)
        }
        _ => json!("OMITTED"),
    }
}
pub fn write(c: &Arc<Core>, health: &Value, monitors: &Value) -> Result<Value, String> {
    let modules=c.module_list().as_array().ok_or("DIAGNOSTIC_MODULES")?.iter().map(|row|json!({"id":row["id"],"enabled":row["enabled"],"tool_count":row["tools"],"health":if row["enabled"]==true{"ENABLED_UNPROBED"}else{"DISABLED"},"live":row["live"]})).collect::<Vec<_>>();
    let claude = json!({"version":health.get("version").and_then(Value::as_str).filter(|s|s.len()<=100&&!policy::contains_secret(s)).unwrap_or("UNAVAILABLE"),"loggedIn":health["loggedIn"].as_bool(),"approved":health["approved"].as_bool(),"authMethod":safe_label(&health["authMethod"]),"apiProvider":safe_label(&health["apiProvider"]),"subscriptionType":safe_label(&health["subscriptionType"])});
    let rows=c.store.query("SELECT timestamp,tool_id,kind,risk,status,duration_ms FROM activity_events ORDER BY timestamp DESC,id DESC LIMIT 200",&[])?;
    let mut log = Vec::new();
    for row in rows.as_array().ok_or("DIAGNOSTIC_LOG_ROWS")? {
        let at = row["timestamp"]
            .as_str()
            .filter(|s| chrono::DateTime::parse_from_rfc3339(s).is_ok())
            .unwrap_or("UNAVAILABLE");
        let line = json!({"timestamp":at,"tool_id":safe_label(&row["tool_id"]),"kind":safe_label(&row["kind"]),"risk":safe_label(&row["risk"]),"status":safe_label(&row["status"]),"duration_ms":row["duration_ms"].as_u64()});
        serde_json::to_writer(&mut log, &line).map_err(|_| "DIAGNOSTIC_JSON")?;
        log.push(b'\n');
    }
    let report = json!({"app":"AlfredPet","version":env!("CARGO_PKG_VERSION"),"created_at":storage::now(),"os":os(),"monitors":monitors,"modules":modules,"claude":claude,"schema_version":storage::SCHEMA_VERSION,"logs":"Bounded operational metadata only; raw logs and input/output content omitted","privacy":c.store.setting("privacy")==true});
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("diagnostics.json", options)
        .map_err(|_| "DIAGNOSTIC_ZIP")?;
    zip.write_all(&serde_json::to_vec_pretty(&report).map_err(|_| "DIAGNOSTIC_JSON")?)
        .map_err(|_| "DIAGNOSTIC_ZIP")?;
    zip.start_file("logs.jsonl", options)
        .map_err(|_| "DIAGNOSTIC_ZIP")?;
    zip.write_all(&log).map_err(|_| "DIAGNOSTIC_ZIP")?;
    let bytes = zip.finish().map_err(|_| "DIAGNOSTIC_ZIP")?.into_inner();
    if bytes.len() > 1024 * 1024 {
        return Err("DIAGNOSTIC_BYTE_LIMIT".into());
    }
    let path = c
        .root
        .join("backups")
        .join(format!("diagnostics-{}.zip", storage::id()));
    let ancestors = path
        .parent()
        .ok_or("DIAGNOSTIC_PARENT")?
        .ancestors()
        .collect::<Vec<_>>();
    let _parents = ancestors
        .into_iter()
        .rev()
        .map(asset_files::directory)
        .collect::<Result<Vec<_>, _>>()?;
    let mut file = asset_files::create(&path)?;
    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        asset_files::erase(&file).map_err(|_| "DIAGNOSTIC_PARTIAL_OUTCOME_UNCERTAIN")?;
        return Err(format!("DIAGNOSTIC_WRITE:{error}"));
    }
    asset_files::private(&file)?;
    if asset_files::read(&file, 1024 * 1024)? != bytes {
        return Err("DIAGNOSTIC_READBACK_FAILED".into());
    }
    Ok(
        json!({"path":path,"bytes":bytes.len(),"entries":["diagnostics.json","logs.jsonl"],"screenshots":false,"conversation_content":false,"credentials":false}),
    )
}
pub async fn export(c: &Arc<Core>) -> Result<Value, String> {
    let health = crate::claude::health().await.unwrap_or(json!({}));
    let monitors = serde_json::to_value(c.monitors().await?).map_err(|_| "DIAGNOSTIC_MONITORS")?;
    write(c, &health, &monitors)
}
#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::{io::Read, path::PathBuf};
    #[test]
    fn owned_zip_omits_raw_logs_content_and_auth_extras_and_keeps_metadata() {
        let root = std::env::temp_dir().join(format!("ap-diagnostics-{}", storage::id()));
        let c = Arc::new(
            Core::new(
                root.clone(),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        std::fs::write(
            root.join("logs/alfredpet.jsonl"),
            "PRIVATE CONVERSATION sk-ant-owned-secret",
        )
        .unwrap();
        c.store
            .audit(
                "files.read",
                "Free",
                &json!({"text":"PRIVATE CONVERSATION"}),
                "ok",
                &json!({"text":"PRIVATE CONVERSATION"}),
                4,
            )
            .unwrap();
        let result=write(&c,&json!({"approved":true,"version":"fixture","loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","subscriptionType":"max","api_key":"sk-ant-owned-secret","diagnostic":"PRIVATE CONVERSATION"}),&json!([{"id":"OWNED","scale":1.5,"work":{"x":-100,"y":0,"width":100,"height":100}}])).unwrap();
        let path = PathBuf::from(result["path"].as_str().unwrap());
        let file = asset_files::open(&path, false).unwrap();
        asset_files::private(&file).unwrap();
        let bytes = asset_files::read(&file, 1024 * 1024).unwrap();
        drop(file);
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(zip.len(), 2);
        let mut report = String::new();
        zip.by_name("diagnostics.json")
            .unwrap()
            .read_to_string(&mut report)
            .unwrap();
        let v: Value = serde_json::from_str(&report).unwrap();
        assert_eq!(v["modules"].as_array().unwrap().len(), 26);
        assert_eq!(v["monitors"][0]["scale"], 1.5);
        assert_eq!(v["claude"]["approved"], true);
        let mut log = String::new();
        zip.by_name("logs.jsonl")
            .unwrap()
            .read_to_string(&mut log)
            .unwrap();
        assert!(log.contains("files.read"));
        assert!(!format!("{report}{log}").contains("PRIVATE CONVERSATION"));
        assert!(!format!("{report}{log}").contains("sk-ant"));
        drop(zip);
        drop(c);
        let root = root.canonicalize().unwrap();
        assert_eq!(
            root.parent().unwrap(),
            std::env::temp_dir().canonicalize().unwrap()
        );
        assert!(root
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("ap-diagnostics-"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
