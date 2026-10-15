//! Local publication protocol. SQLite and WCM are separate systems: readiness
//! follows a committed blocked intent and exact native readback.
use crate::{process, rest_credentials as credentials, rest_profile, runtime::Core, storage};
use serde_json::{json, Value};
use std::sync::Arc;

pub(crate) fn binding_key(id: &str) -> String {
    format!("rest.credential_binding.{id}")
}
pub(crate) fn generation_key(id: &str) -> String {
    format!("rest.connection_generation.{id}")
}
pub(crate) fn cooldown_key(id: &str) -> String {
    format!("rest.cooldown.{id}")
}
pub(crate) fn binding(
    core: &Core,
    id: &str,
    config: &rest_profile::Config,
) -> Result<credentials::Binding, String> {
    let b = credentials::public_binding(&core.store.setting(&binding_key(id)))?;
    credentials::validate_binding(id, config, &b)?;
    Ok(b)
}
pub(crate) async fn native_read(core: &Core, id: &str) -> Result<String, String> {
    process::native(
        &core.native,
        "secret.get",
        &json!({"alias":credentials::alias(id)?}),
    )
    .await
    .map_err(|_| "REST_CREDENTIAL_UNAVAILABLE")?["value"]
        .as_str()
        .map(str::to_string)
        .ok_or("REST_CREDENTIAL_UNAVAILABLE".into())
}
pub(crate) struct Expected {
    pub raw: Value,
    pub generation: Value,
    pub binding: Value,
}
pub(crate) async fn save_connection(
    core: &Arc<Core>,
    id: &str,
    name: &str,
    raw: &Value,
    secret: Option<&str>,
    existing: bool,
    expected: Option<&Expected>,
) -> Result<Value, String> {
    // The caller holds integrations::save_lock across this entire operation.
    credentials::canonical(id)?;
    let config = rest_profile::config(raw)?;
    if name.trim().is_empty() || name.len() > 200 {
        return Err("REST_CONNECTION_NAME".into());
    }
    let old = core.store.setting(&binding_key(id));
    let supplied = secret.filter(|s| !s.is_empty());
    let publication = supplied
        .map(|s| credentials::publication(id, &config, s))
        .transpose()?;
    let requires = !rest_profile::required_secrets(&config)?.is_empty();
    let b = if let Some(p) = &publication {
        p.pending.clone()
    } else if !requires {
        credentials::unconfigured(id, &config)?
    } else if existing {
        let b =
            binding(core, id, &config).map_err(|_| "REST_CREDENTIAL_REQUIRED_FOR_TARGET_CHANGE")?;
        if b.stage == credentials::Stage::Pending {
            return Err("REST_CREDENTIAL_REENTRY_REQUIRED".into());
        }
        if b.stage == credentials::Stage::Ready {
            credentials::read(id, &config, &b, Some(&native_read(core, id).await?))?;
        }
        b
    } else {
        credentials::unconfigured(id, &config)?
    };
    let generation = storage::id();
    let alias = credentials::alias(id)?;
    let text = raw.to_string();
    {
        let mut db = core.store.0.lock().map_err(|_| "SQLITE_LOCK")?;
        let tx = db
            .transaction()
            .map_err(|_| "REST_PROFILE_INTENT_TRANSACTION")?;
        let current = tx
            .query_row(
                "SELECT json_value FROM settings WHERE key=?",
                [binding_key(id)],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or(Value::Null);
        if current != old {
            return Err("REST_CREDENTIAL_BINDING_CHANGED".into());
        }
        if let Some(e) = expected {
            let row = tx
                .query_row(
                    "SELECT module_id,config_json,secret_ref FROM connections WHERE id=?",
                    [id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    },
                )
                .map_err(|_| "REST_PROFILE_CHANGED")?;
            let g = tx
                .query_row(
                    "SELECT json_value FROM settings WHERE key=?",
                    [generation_key(id)],
                    |r| r.get::<_, String>(0),
                )
                .map_err(|_| "REST_PROFILE_CHANGED")?;
            if row != ("rest".into(), e.raw.to_string(), alias.clone())
                || serde_json::from_str::<Value>(&g).map_err(|_| "REST_PROFILE_CHANGED")?
                    != e.generation
                || current != e.binding
                || tx
                    .query_row(
                        "SELECT json_value FROM settings WHERE key='privacy'",
                        [],
                        |r| r.get::<_, String>(0),
                    )
                    .ok()
                    .is_some_and(|s| s == "true")
            {
                return Err("REST_PROFILE_CHANGED".into());
            }
            let enabled = tx
                .query_row(
                    "SELECT enabled FROM module_configs WHERE module_id='rest'",
                    [],
                    |r| r.get::<_, bool>(0),
                )
                .unwrap_or(false);
            if !enabled {
                return Err("MODULE_DISABLED: rest".into());
            }
        }
        tx.execute("INSERT INTO settings(key,json_value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET json_value=excluded.json_value",rusqlite::params![generation_key(id),json!(generation).to_string()]).map_err(|_|"REST_PROFILE_INTENT_TRANSACTION")?;
        tx.execute("INSERT INTO settings(key,json_value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET json_value=excluded.json_value",rusqlite::params![binding_key(id),json!(b).to_string()]).map_err(|_|"REST_PROFILE_INTENT_TRANSACTION")?;
        tx.execute("INSERT INTO connections(id,module_id,name,config_json,secret_ref,health) VALUES(?,'rest',?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,config_json=excluded.config_json,secret_ref=excluded.secret_ref,health=excluded.health",rusqlite::params![id,name,text,alias,if publication.is_some(){"CREDENTIAL_PENDING"}else if requires && b.stage==credentials::Stage::Unconfigured{"CREDENTIAL_REQUIRED"}else{"LIVE_NOT_RUN"}]).map_err(|_|"REST_PROFILE_INTENT_TRANSACTION")?;
        tx.commit().map_err(|_| "REST_PROFILE_INTENT_TRANSACTION")?;
    }
    if let Some(p) = publication {
        let published=async {
            process::native(&core.native,"secret.set",&json!({"alias":alias,"value":p.private_json})).await.map_err(|_|"REST_CREDENTIAL_WRITE_UNCERTAIN")?;
            let observed=native_read(core,id).await?;
            if observed!=p.private_json { return Err("REST_CREDENTIAL_READBACK_UNCERTAIN".into()); }
            credentials::read(id,&config,&p.ready,Some(&observed))?;
            let mut db=core.store.0.lock().map_err(|_|"SQLITE_LOCK")?;
            let tx=db.transaction().map_err(|_|"REST_CREDENTIAL_PUBLICATION_TRANSACTION")?;
            let row=tx.query_row("SELECT module_id,config_json,secret_ref FROM connections WHERE id=?",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).map_err(|_|"REST_PROFILE_CHANGED")?;
            if row!=("rest".into(),text,alias) { return Err("REST_PROFILE_CHANGED".into()); }
            if tx.execute("UPDATE settings SET json_value=? WHERE key=? AND json_value=? AND EXISTS(SELECT 1 FROM settings WHERE key=? AND json_value=?)",rusqlite::params![json!(p.ready).to_string(),binding_key(id),json!(p.pending).to_string(),generation_key(id),json!(generation).to_string()]).map_err(|_|"REST_CREDENTIAL_PUBLICATION_TRANSACTION")?!=1 { return Err("REST_CREDENTIAL_BINDING_CHANGED".into()); }
            tx.execute("UPDATE connections SET health='LIVE_NOT_RUN' WHERE id=?",[id]).map_err(|_|"REST_CREDENTIAL_PUBLICATION_TRANSACTION")?;
            tx.commit().map_err(|_|"REST_CREDENTIAL_PUBLICATION_TRANSACTION")?;
            Ok::<_,String>(())
        }.await;
        if published.is_err() {
            return Err(format!("REST_CONNECTION_CREDENTIAL_PENDING:{id}"));
        }
    }
    core.store.set_setting(&cooldown_key(id), &Value::Null)?;
    Ok(
        json!({"id":id,"live":"LIVE_NOT_RUN","credential_state":if supplied.is_some(){credentials::Stage::Ready}else{b.stage}}),
    )
}
