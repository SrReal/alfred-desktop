//! Explicit per-plugin/per-version WinCred references; values are private RPC data.
use crate::{plugins, policy, runtime::Core, storage};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};
fn prefix(c: &Core, id: &str, version: &str) -> String {
    format!(
        "alfredpet.public.custom.{:x}.",
        Sha256::digest(
            json!({"root":c.root,"id":id,"version":version})
                .to_string()
                .as_bytes()
        )
    )
}
fn target_allowed(c: &Core, id: &str, version: &str, target: &str) -> bool {
    target
        .strip_prefix(&prefix(c, id, version))
        .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok_and(|v| !v.is_nil() && v.to_string() == s))
}
pub(crate) async fn load(
    c: &Core,
    names: &[String],
    id: &str,
    version: &str,
) -> Result<BTreeMap<String, String>, String> {
    if names.is_empty() {
        return Ok(BTreeMap::new());
    }
    let refs = c.store.setting(&format!("plugin_secrets.{id}"));
    if refs["version"] != version {
        return Err("PLUGIN_SECRETS_NOT_CONFIGURED".into());
    }
    let mut targets = Vec::new();
    for name in names {
        let target = refs["entries"][name]
            .as_str()
            .ok_or("PLUGIN_SECRET_NOT_CONFIGURED")?;
        if !target_allowed(c, id, version, target) {
            return Err("PLUGIN_SECRET_SCOPE".into());
        }
        targets.push((name.clone(), target.to_string()));
    }
    tokio::task::spawn_blocking(move || {
        let mut out = BTreeMap::new();
        for (name, target) in targets {
            out.insert(name, read(&target)?);
        }
        Ok(out)
    })
    .await
    .map_err(|_| "PLUGIN_SECRET_WORKER")?
}
fn check(c: &Core, id: &str, fp: &str, authority: &Value) -> Result<Value, String> {
    if c.store.setting("privacy") == true
        || !c.enabled("custom")
        || c.store.setting("plugin_authority") != *authority
    {
        return Err("PLUGIN_SECRET_REVOKED".into());
    }
    let r = plugins::inspect(c, id)?;
    if r["fingerprint"] != fp {
        return Err("PLUGIN_CHANGED".into());
    }
    Ok(r)
}
pub(crate) async fn save(c: &Arc<Core>, v: Value) -> Result<Value, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        id: String,
        expected: String,
        name: String,
        value: String,
    }
    let input: Input = serde_json::from_value(v).map_err(|_| "PLUGIN_SECRET_INPUT")?;
    if input.value.is_empty()
        || input.value.contains('\0')
        || input.value.encode_utf16().count() > 1280
    {
        return Err("PLUGIN_SECRET_CAPACITY".into());
    }
    let authority = c.store.setting("plugin_authority");
    if authority
        .as_str()
        .is_none_or(|s| uuid::Uuid::parse_str(s).is_err())
    {
        return Err("PLUGIN_AUTHORITY".into());
    }
    let observed = check(c, &input.id, &input.expected, &authority)?;
    let version = observed["manifest"]["version"]
        .as_str()
        .ok_or("PLUGIN_VERSION")?
        .to_owned();
    if !observed["manifest"]["secrets"]
        .as_array()
        .is_some_and(|a| a.contains(&json!(input.name)))
    {
        return Err("PLUGIN_SECRET_REFERENCE".into());
    }
    c.permission("custom.secret_set",&json!({"id":input.id,"version":version,"name":input.name,"fingerprint":input.expected,"value":"[PRIVATE_VALUE_OMITTED]"}),policy::Risk::Mandatory).await?;
    check(c, &input.id, &input.expected, &authority)?;
    let lease = c
        .plugin_actions
        .clone()
        .try_lock_owned()
        .map_err(|_| "PLUGIN_BUSY")?;
    let token = tokio_util::sync::CancellationToken::new();
    struct Cancel(tokio_util::sync::CancellationToken);
    impl Drop for Cancel {
        fn drop(&mut self) {
            self.0.cancel();
        }
    }
    let _cancel = Cancel(token.clone());
    let core = c.clone();
    tokio::task::spawn_blocking(move||{
        let _lease=lease;
        if token.is_cancelled(){return Err("PLUGIN_SECRET_CANCELLED".into())}
        check(&core,&input.id,&input.expected,&authority)?;
        let key=format!("plugin_secrets.{}",input.id);let before=core.store.setting(&key);
        let target=format!("{}{}",prefix(&core,&input.id,&version),storage::id());
        write(&target,&input.value)?;
        let readback=read(&target)?;
        if readback!=input.value||token.is_cancelled()||check(&core,&input.id,&input.expected,&authority).is_err(){let _=delete(&target);return Err("PLUGIN_SECRET_OUTCOME_UNCERTAIN".into())}
        let mut next=if before["version"]==version{before.clone()}else{json!({"version":version,"entries":{}})};
        let previous=next["entries"][&input.name].as_str().map(str::to_owned);
        next["entries"][&input.name]=json!(target);
        let mut db=core.store.0.lock().map_err(|_|"PLUGIN_SECRET_STORE")?;let tx=db.transaction().map_err(|_|"PLUGIN_SECRET_STORE")?;
        let current:Option<String>=tx.query_row("SELECT json_value FROM settings WHERE key=?",[&key],|r|r.get(0)).optional().map_err(|_|"PLUGIN_SECRET_STORE")?;
        let current_value=current.as_ref().map(|s|serde_json::from_str::<Value>(s)).transpose().map_err(|_|"PLUGIN_SECRET_STORE")?.unwrap_or(Value::Null);
        if current_value!=before||token.is_cancelled(){drop(tx);drop(db);let _=delete(&target);return Err("PLUGIN_SECRET_OUTCOME_UNCERTAIN".into())}
        tx.execute("INSERT INTO settings(key,json_value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET json_value=excluded.json_value",rusqlite::params![key,next.to_string()]).map_err(|_|"PLUGIN_SECRET_STORE")?;
        tx.commit().map_err(|_|"PLUGIN_SECRET_OUTCOME_UNCERTAIN:STORE_COMMIT")?;drop(db);
        if let Some(old)=previous.filter(|old|target_allowed(&core,&input.id,&version,old)){let _=delete(&old);}
        core.emit("alfredpet:modules",&json!({}));
        Ok(json!({"saved":true,"id":input.id,"name":input.name,"value_returned":false,"retest_required":true}))
    }).await.map_err(|_|"PLUGIN_SECRET_WORKER")?
}
use rusqlite::OptionalExtension;
#[cfg(windows)]
fn read(target: &str) -> Result<String, String> {
    use windows::{
        core::PCWSTR,
        Win32::Security::Credentials::{CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC},
    };
    struct Allocation(*mut CREDENTIALW);
    impl Drop for Allocation {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CredFree(self.0.cast()) }
            }
        }
    }
    let name: Vec<u16> = target.encode_utf16().chain(Some(0)).collect();
    let mut p = std::ptr::null_mut();
    unsafe { CredReadW(PCWSTR(name.as_ptr()), CRED_TYPE_GENERIC, None, &mut p) }
        .map_err(|_| "PLUGIN_SECRET_UNAVAILABLE")?;
    let _allocation = Allocation(p);
    if p.is_null() {
        return Err("PLUGIN_SECRET_FORMAT".into());
    }
    let r = unsafe { &*p };
    let len = r.CredentialBlobSize as usize;
    if r.Type != CRED_TYPE_GENERIC
        || len == 0
        || len > 2560
        || !len.is_multiple_of(2)
        || r.CredentialBlob.is_null()
        || r.TargetName.is_null()
        || unsafe { r.TargetName.to_string() }.map_err(|_| "PLUGIN_SECRET_FORMAT")? != target
    {
        return Err("PLUGIN_SECRET_FORMAT".into());
    }
    let bytes = unsafe { std::slice::from_raw_parts_mut(r.CredentialBlob, len) };
    let mut wide = bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect::<Vec<_>>();
    let result = String::from_utf16(&wide).map_err(|_| "PLUGIN_SECRET_FORMAT".into());
    wide.fill(0);
    bytes.fill(0);
    result
}
#[cfg(windows)]
fn write(target: &str, value: &str) -> Result<(), String> {
    use windows::{
        core::PWSTR,
        Win32::Security::Credentials::{
            CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
        },
    };
    let mut name: Vec<u16> = target.encode_utf16().chain(Some(0)).collect();
    let mut user: Vec<u16> = "AlfredPet".encode_utf16().chain(Some(0)).collect();
    let mut bytes: Vec<u8> = value.encode_utf16().flat_map(u16::to_le_bytes).collect();
    if bytes.is_empty() || bytes.len() > 2560 || value.contains('\0') {
        bytes.fill(0);
        return Err("PLUGIN_SECRET_CAPACITY".into());
    }
    let r = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(name.as_mut_ptr()),
        CredentialBlobSize: bytes.len() as u32,
        CredentialBlob: bytes.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: PWSTR(user.as_mut_ptr()),
        ..Default::default()
    };
    let result = unsafe { CredWriteW(&r, 0) }.map_err(|_| "PLUGIN_SECRET_WRITE".into());
    bytes.fill(0);
    result
}
#[cfg(windows)]
fn delete(target: &str) -> Result<(), String> {
    use windows::{
        core::PCWSTR,
        Win32::Security::Credentials::{CredDeleteW, CRED_TYPE_GENERIC},
    };
    let name: Vec<u16> = target.encode_utf16().chain(Some(0)).collect();
    unsafe { CredDeleteW(PCWSTR(name.as_ptr()), CRED_TYPE_GENERIC, None) }
        .map_err(|_| "PLUGIN_SECRET_DELETE".into())
}
#[cfg(not(windows))]
fn read(_: &str) -> Result<String, String> {
    Err("WINDOWS_11_REQUIRED".into())
}
#[cfg(not(windows))]
fn write(_: &str, _: &str) -> Result<(), String> {
    Err("WINDOWS_11_REQUIRED".into())
}
#[cfg(not(windows))]
fn delete(_: &str) -> Result<(), String> {
    Err("WINDOWS_11_REQUIRED".into())
}
#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn owned_native_utf16_roundtrip_and_exact_target_namespace() {
        let target = format!("alfredpet.public.custom.owned-fixture.{}", storage::id());
        let value = "fictional-value-ñ-日本";
        write(&target, value).unwrap();
        assert_eq!(read(&target).unwrap(), value);
        delete(&target).unwrap();
        assert!(read(&target).is_err());
        assert!(write(&target, &"a".repeat(1281)).is_err());
    }

    #[tokio::test]
    async fn private_save_uses_actual_individual_permission_and_owned_subject_before_loading() {
        let root = std::env::temp_dir().join(format!("ap-plugin-vault-{}", storage::id()));
        std::fs::create_dir(&root).unwrap();
        let core = Arc::new(
            Core::new(
                root.join("profile"),
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        core.plugin_headless_permission
            .store(true, std::sync::atomic::Ordering::SeqCst);
        core.store
            .execute(
                "UPDATE module_configs SET enabled=1 WHERE module_id='custom'",
                &[],
            )
            .unwrap();
        plugins::revoke(&core).unwrap();
        let path = core.root.join("plugins").join(storage::id());
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("owned.txt"), "owned non-executing fixture").unwrap();
        let id = format!("custom.{}", storage::id());
        let name = "ALFREDPET_SECRET_SERVICE";
        core.store
            .execute(
                "INSERT INTO custom_tools VALUES(?,?,?,'disabled')",
                &[
                    json!(id),
                    json!(
                        json!({"name":"Owned secret reference","version":"1.0.0","secrets":[name]})
                            .to_string()
                    ),
                    json!(path),
                ],
            )
            .unwrap();
        let input = json!({"id":id,"expected":plugins::inspect(&core,&id).unwrap()["fingerprint"],"name":name,"value":"fictional-private-value-ñ-日本"});
        for answer in ["deny", "session", "allow"] {
            let c = core.clone();
            let v = input.clone();
            let task = tokio::spawn(async move { save(&c, v).await });
            let tx=tokio::time::timeout(std::time::Duration::from_secs(8),async{
                loop{let rows=core.store.query("SELECT id FROM permission_requests WHERE tool_id='custom.secret_set' AND status='pending'",&[]).unwrap();
                    if let Some(id)=rows[0]["id"].as_str(){if let Some(tx)=core.pending.lock().unwrap().remove(id){break tx;}}
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }).await.unwrap();
            tx.send(answer.into()).unwrap();
            let result = task.await.unwrap();
            if answer == "allow" {
                assert_eq!(result.unwrap()["saved"], true)
            } else {
                assert!(result.is_err());
                assert!(core
                    .store
                    .setting(&format!("plugin_secrets.{id}"))
                    .is_null());
            }
        }
        let refs = core.store.setting(&format!("plugin_secrets.{id}"));
        let target = refs["entries"][name].as_str().unwrap();
        assert!(target_allowed(&core, &id, "1.0.0", target));
        assert!(!target_allowed(&core, "custom.other", "1.0.0", target));
        assert_eq!(
            load(&core, &[name.into()], &id, "1.0.0").await.unwrap()[name],
            "fictional-private-value-ñ-日本"
        );
        assert!(load(&core, &[name.into()], &id, "2.0.0").await.is_err());
        let text = core
            .store
            .query("SELECT json_value FROM settings", &[])
            .unwrap()
            .to_string()
            + &core
                .store
                .query("SELECT input_json_redacted FROM permission_requests", &[])
                .unwrap()
                .to_string();
        assert!(!text.contains("fictional-private-value"));
        assert_eq!(core.grants.lock().unwrap().len(), 0);
        delete(target).unwrap();
        let canonical = root.canonicalize().unwrap();
        assert_eq!(
            canonical.parent().unwrap(),
            std::env::temp_dir().canonicalize().unwrap()
        );
        assert!(canonical
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("ap-plugin-vault-"));
        drop(core);
        std::fs::remove_dir_all(canonical).unwrap();
    }
}
