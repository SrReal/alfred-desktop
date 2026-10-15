//! Native token publication is not a transaction with SQLite. Persist a blocked
//! profile intent first, verify exact native bytes, and only then mark it ready.
use super::{canonical, config, digest, origin, validate_token};
use crate::{process, runtime::Core, storage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

const KIND: &str = "alfredpet.homeassistant.token";
fn key(id: &str) -> String {
    format!("homeassistant.credential_binding.{id}")
}
fn target(id: &str, raw: &Value) -> Result<String, String> {
    canonical(id)?;
    Ok(digest(
        &json!({"connection":id,"origin":origin(&config(raw)?)?.as_str(),"alias":format!("connection.{id}")}),
    ))
}
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Binding {
    version: u8,
    connection: String,
    target_sha256: String,
    generation: Option<String>,
    stage: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u8,
    kind: String,
    connection: String,
    target_sha256: String,
    generation: String,
    token: String,
}
fn load(core: &Core, id: &str, raw: &Value) -> Result<Binding, String> {
    let b: Binding = serde_json::from_value(core.store.setting(&key(id)))
        .map_err(|_| "HA_CREDENTIAL_BINDING_REQUIRED")?;
    if b.version != 1
        || b.connection != id
        || b.target_sha256 != target(id, raw)?
        || !["unconfigured", "pending", "ready"].contains(&b.stage.as_str())
        || (b.stage == "unconfigured") != b.generation.is_none()
        || b.generation.as_ref().is_some_and(|g| canonical(g).is_err())
    {
        return Err("HA_CREDENTIAL_BINDING_INVALID".into());
    }
    Ok(b)
}
pub(super) fn binding(core: &Core, id: &str, raw: &Value) -> Result<Value, String> {
    serde_json::to_value(load(core, id, raw)?).map_err(|_| "HA_CREDENTIAL_BINDING_INVALID".into())
}
fn envelope(raw: &str, b: &Binding) -> Result<Envelope, String> {
    if raw.encode_utf16().count() > 1280 || b.stage != "ready" {
        return Err("HA_CREDENTIAL_REENTRY_REQUIRED".into());
    }
    let e: Envelope =
        serde_json::from_str(raw).map_err(|_| "HA_CREDENTIAL_LEGACY_REENTRY_REQUIRED")?;
    if e.version != 1
        || e.kind != KIND
        || e.connection != b.connection
        || e.target_sha256 != b.target_sha256
        || Some(&e.generation) != b.generation.as_ref()
        || canonical(&e.generation).is_err()
    {
        return Err("HA_CREDENTIAL_ENVELOPE_BINDING_INVALID".into());
    }
    validate_token(&e.token)?;
    Ok(e)
}
pub(super) async fn native_read(core: &Core, id: &str) -> Result<String, String> {
    let v = process::native(
        &core.native,
        "secret.get",
        &json!({"alias":format!("connection.{id}")}),
    )
    .await
    .map_err(|_| "HA_CREDENTIAL_UNAVAILABLE")?;
    v["value"]
        .as_str()
        .map(str::to_owned)
        .ok_or("HA_CREDENTIAL_REQUIRED".into())
}
pub(super) async fn read_token(
    core: &Core,
    id: &str,
    raw: &Value,
    expected: &Value,
) -> Result<String, String> {
    let b = load(core, id, raw)?;
    if json!(b) != *expected || b.stage != "ready" {
        return Err("HA_CREDENTIAL_REENTRY_REQUIRED".into());
    }
    let native = native_read(core, id).await?;
    let token = envelope(&native, &b)?.token;
    if binding(core, id, raw)? != *expected {
        return Err("HA_CREDENTIAL_BINDING_CHANGED".into());
    }
    Ok(token)
}
/// Called only while integrations::save owns the exact profile save lock and
/// has validated canonical connection/module/alias ownership and target change.
pub(super) async fn save(
    core: &Arc<Core>,
    id: &str,
    name: &str,
    raw: &Value,
    secret: Option<&str>,
    existing: bool,
) -> Result<Value, String> {
    canonical(id)?;
    config(raw)?;
    if name.trim().is_empty() || name.len() > 200 {
        return Err("HA_CONNECTION_NAME".into());
    }
    let destination = target(id, raw)?;
    let supplied = secret.filter(|s| !s.is_empty());
    let old = core.store.setting(&key(id));
    let (binding, encoded) = if let Some(token) = supplied {
        validate_token(token)?;
        let generation = storage::id();
        let e = Envelope {
            version: 1,
            kind: KIND.into(),
            connection: id.into(),
            target_sha256: destination.clone(),
            generation: generation.clone(),
            token: token.into(),
        };
        let text = serde_json::to_string(&e).map_err(|_| "HA_CREDENTIAL_ENVELOPE")?;
        if text.encode_utf16().count() > 1280 {
            return Err("HA_CREDENTIAL_NATIVE_CAPACITY".into());
        }
        (
            Binding {
                version: 1,
                connection: id.into(),
                target_sha256: destination,
                generation: Some(generation),
                stage: "pending".into(),
            },
            Some(text),
        )
    } else if existing {
        let b = load(core, id, raw)?;
        if b.stage == "pending" {
            return Err("HA_CREDENTIAL_REENTRY_REQUIRED".into());
        }
        if b.stage == "ready" {
            let native = native_read(core, id).await?;
            envelope(&native, &b)?;
        }
        (b, None)
    } else {
        (
            Binding {
                version: 1,
                connection: id.into(),
                target_sha256: destination,
                generation: None,
                stage: "unconfigured".into(),
            },
            None,
        )
    };
    let save_generation = storage::id();
    let alias = format!("connection.{id}");
    let config_json = raw.to_string();
    // SQLite generation, blocked binding and candidate profile are one local
    // transaction. No native token is written before this transaction commits.
    {
        let mut db = core.store.0.lock().map_err(|_| "SQLITE_LOCK")?;
        let tx = db
            .transaction()
            .map_err(|_| "HA_PROFILE_INTENT_TRANSACTION")?;
        let current = tx
            .query_row(
                "SELECT json_value FROM settings WHERE key=?",
                [key(id)],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or(Value::Null);
        if current != old {
            return Err("HA_CREDENTIAL_BINDING_CHANGED".into());
        }
        tx.execute("INSERT INTO settings(key,json_value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET json_value=excluded.json_value",rusqlite::params![format!("homeassistant.connection_generation.{id}"),json!(save_generation).to_string()]).map_err(|_|"HA_PROFILE_INTENT_TRANSACTION")?;
        tx.execute("INSERT INTO settings(key,json_value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET json_value=excluded.json_value",rusqlite::params![key(id),json!(binding).to_string()]).map_err(|_|"HA_PROFILE_INTENT_TRANSACTION")?;
        tx.execute("INSERT INTO connections(id,module_id,name,config_json,secret_ref,health) VALUES(?,'homeassistant',?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,config_json=excluded.config_json,secret_ref=excluded.secret_ref,health=excluded.health",rusqlite::params![id,name,config_json,alias,if encoded.is_some(){"CREDENTIAL_PENDING"}else if binding.stage=="unconfigured"{"CREDENTIAL_REQUIRED"}else{"LIVE_NOT_RUN"}]).map_err(|_|"HA_PROFILE_INTENT_TRANSACTION")?;
        tx.commit().map_err(|_| "HA_PROFILE_INTENT_TRANSACTION")?;
    }
    if let Some(text) = encoded {
        let published=async {
            process::native(&core.native,"secret.set",&json!({"alias":alias,"value":text})).await.map_err(|_|"HA_CREDENTIAL_WRITE_UNCERTAIN")?;
            let observed=native_read(core,id).await?;
            if observed!=text{return Err("HA_CREDENTIAL_WRITE_READBACK_UNCERTAIN".into());}
            let mut ready=binding.clone();ready.stage="ready".into();envelope(&observed,&ready)?;
            let mut db=core.store.0.lock().map_err(|_|"SQLITE_LOCK")?;
            let tx=db.transaction().map_err(|_|"HA_CREDENTIAL_PUBLICATION_TRANSACTION")?;
            let profile=tx.query_row("SELECT module_id,config_json,secret_ref FROM connections WHERE id=?",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).map_err(|_|"HA_CREDENTIAL_PROFILE_CHANGED")?;
            if profile!=("homeassistant".into(),config_json,alias){return Err("HA_CREDENTIAL_PROFILE_CHANGED".into());}
            if tx.execute("UPDATE settings SET json_value=? WHERE key=? AND json_value=? AND EXISTS(SELECT 1 FROM settings WHERE key=? AND json_value=?)",rusqlite::params![json!(ready).to_string(),key(id),json!(binding).to_string(),format!("homeassistant.connection_generation.{id}"),json!(save_generation).to_string()]).map_err(|_|"HA_CREDENTIAL_PUBLICATION_TRANSACTION")?!=1{return Err("HA_CREDENTIAL_BINDING_CHANGED".into());}
            tx.execute("UPDATE connections SET health='LIVE_NOT_RUN' WHERE id=?",[id]).map_err(|_|"HA_CREDENTIAL_PUBLICATION_TRANSACTION")?;
            tx.commit().map_err(|_|"HA_CREDENTIAL_PUBLICATION_TRANSACTION")?;
            Ok::<_,String>(())
        }.await;
        if published.is_err() {
            return Err(format!("HA_CONNECTION_CREDENTIAL_PENDING:{id}"));
        }
    }
    core.store
        .set_setting(&format!("homeassistant.cooldown.{id}"), &Value::Null)?;
    Ok(
        json!({"id":id,"live":"LIVE_NOT_RUN","credential_state":if supplied.is_some(){"ready"}else{binding.stage.as_str()}}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn owned() -> (Binding, String) {
        let b = Binding {
            version: 1,
            connection: storage::id(),
            target_sha256: "a".repeat(64),
            generation: Some(storage::id()),
            stage: "ready".into(),
        };
        let e = Envelope {
            version: 1,
            kind: KIND.into(),
            connection: b.connection.clone(),
            target_sha256: b.target_sha256.clone(),
            generation: b.generation.clone().unwrap(),
            token: "owned_fictional_codec_token".into(),
        };
        (b, serde_json::to_string(&e).unwrap())
    }
    #[test]
    fn homeassistant_credentials_envelope_binds_exact_subject_origin_and_generation() {
        let (b, raw) = owned();
        assert_eq!(
            envelope(&raw, &b).unwrap().token,
            "owned_fictional_codec_token"
        );
        for key in [
            "connection",
            "target_sha256",
            "generation",
            "kind",
            "version",
        ] {
            let mut v: Value = serde_json::from_str(&raw).unwrap();
            v[key] = json!("foreign");
            assert!(envelope(&v.to_string(), &b).is_err(), "{key}");
        }
        let mut v: Value = serde_json::from_str(&raw).unwrap();
        v["extra"] = json!(true);
        assert!(envelope(&v.to_string(), &b).is_err());
        assert!(envelope("owned_legacy_scalar", &b).is_err());
    }
    #[test]
    fn homeassistant_credentials_native_capacity_and_blocked_states_fail_closed() {
        let (mut b, raw) = owned();
        for stage in ["pending", "unconfigured"] {
            b.stage = stage.into();
            assert!(envelope(&raw, &b).is_err());
        }
        b.stage = "ready".into();
        let mut v: Value = serde_json::from_str(&raw).unwrap();
        v["token"] = json!("x".repeat(1280));
        assert!(envelope(&v.to_string(), &b).is_err());
        v["token"] = json!("bad\nheader");
        assert!(envelope(&v.to_string(), &b).is_err());
        assert!(envelope(&"x".repeat(2561), &b).is_err());
    }
}
