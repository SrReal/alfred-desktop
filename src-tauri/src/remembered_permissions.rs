use crate::{
    policy::{self, Risk},
    runtime::Core,
    storage,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const TOOLS_SYSTEM: &[&str] = &["windows.audit", "windows.admin_action"];

// Explicit user opt-in for an exact reviewed action, never a tool-wide wildcard.
pub fn eligible(tool: &str, risk: Risk) -> bool {
    if risk == Risk::Free {
        return false;
    }
    risk == Risk::Contextual
        || matches!(
            tool,
            "terminal.command"
                | "terminal.script"
                | "windows.open_app"
                | "window.focus"
                | "window.state"
                | "gui.invoke"
                | "gui.set_value"
                | "gui.select"
                | "gui.click"
                | "gui.hover"
                | "gui.drag"
                | "gui.send_keys"
                | "browser.click"
                | "browser.select"
                | "browser.check"
                | "windows.audit"
                | "windows.admin_action"
        )
}

pub fn scope(c: &Core, tool: &str, input: &Value, risk: Risk) -> Option<Value> {
    if !eligible(tool, risk) {
        return None;
    }
    // Repairs remain individually reviewed; Defender updates/scans can be remembered.
    if tool == "windows.admin_action"
        && !matches!(
            input["action"].as_str(),
            Some("defender_update" | "defender_quick_scan" | "defender_full_scan")
        )
    {
        return None;
    }
    let mut exact = input.clone();
    if matches!(tool, "terminal.command" | "terminal.script") {
        exact.as_object_mut()?.remove("id");
    }
    if TOOLS_SYSTEM.contains(&tool) {
        exact.get_mut("execution")?.as_object_mut()?.remove("id");
    }
    let epoch = c.store.setting("permission_epoch");
    let binding = json!({"version":1,"tool":tool,"risk":risk,"input":exact,
        "epoch":epoch,"allowed_roots":c.store.setting("allowed_roots")});
    let fingerprint = format!("{:x}", Sha256::digest(binding.to_string().as_bytes()));
    Some(
        json!({"version":1,"epoch":epoch,"fingerprint":fingerprint,"preview":policy::redact(&exact)}),
    )
}

pub fn allowed(c: &Core, tool: &str, scope: &Value) -> Result<bool, String> {
    let rows = c.store.query("SELECT scope_json FROM permission_rules WHERE tool_id=? AND decision='exact_v1' AND session_id IS NULL AND expires_at IS NULL", &[json!(tool)])?;
    Ok(rows.as_array().is_some_and(|rows| {
        rows.iter().any(|r| {
            serde_json::from_str::<Value>(r["scope_json"].as_str().unwrap_or("")).is_ok_and(
                |saved| saved["version"] == 1 && saved["fingerprint"] == scope["fingerprint"],
            )
        })
    }))
}

pub fn save(c: &Core, tool: &str, scope: &Value) -> Result<(), String> {
    if allowed(c, tool, scope)? {
        return Ok(());
    }
    let mut db = c.store.0.lock().map_err(|_| "DB_LOCK")?;
    let tx = db.transaction().map_err(|e| e.to_string())?;
    use rusqlite::OptionalExtension;
    let epoch: Option<String> = tx
        .query_row(
            "SELECT json_value FROM settings WHERE key='permission_epoch'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let epoch: Value = epoch
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| "PERMISSION_EPOCH")?
        .unwrap_or(Value::Null);
    if scope["epoch"] != epoch {
        return Err("PERMISSION_SCOPE_CHANGED".into());
    }
    let count: i64 = tx
        .query_row(
            "SELECT count(*) FROM permission_rules WHERE decision='exact_v1'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if count >= 200 {
        return Err("PERMISSION_RULE_LIMIT: revoca permisos antiguos en Configuración".into());
    }
    let id = storage::id();
    let mut record = scope.clone();
    record["created_at"] = json!(storage::now());
    tx.execute(
        "INSERT INTO permission_rules(id,tool_id,scope_json,decision) VALUES(?,?,?,'exact_v1')",
        rusqlite::params![id, tool, record.to_string()],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    drop(db);
    c.emit("alfredpet:permissions_changed", &json!({}));
    Ok(())
}

pub fn list(c: &Core) -> Result<Value, String> {
    c.store.query("SELECT id,tool_id,scope_json FROM permission_rules WHERE decision='exact_v1' ORDER BY rowid DESC LIMIT 200", &[])
}
pub fn revoke(c: &Core, id: &str) -> Result<Value, String> {
    c.store.execute(
        "DELETE FROM permission_rules WHERE id=? AND decision='exact_v1'",
        &[json!(id)],
    )?;
    c.store.audit(
        "permissions.revoke",
        "User",
        &json!({"id":id}),
        "succeeded",
        &json!({}),
        0,
    )?;
    c.emit("alfredpet:permissions_changed", &json!({}));
    Ok(json!({"revoked":true}))
}
pub fn invalidate(c: &Core) -> Result<(), String> {
    let mut db = c.store.0.lock().map_err(|_| "DB_LOCK")?;
    let tx = db.transaction().map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM permission_rules WHERE decision='exact_v1'", [])
        .map_err(|e| e.to_string())?;
    tx.execute("INSERT INTO settings VALUES('permission_epoch',?) ON CONFLICT(key) DO UPDATE SET json_value=excluded.json_value",[json!(storage::id()).to_string()]).map_err(|e|e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    drop(db);
    c.emit("alfredpet:permissions_changed", &json!({}));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn only_explicit_answer_remembers_and_changed_action_prompts_again() {
        use std::sync::{atomic::Ordering, Arc};
        let root = std::env::temp_dir().join(format!("alfred-rule-flow-{}", storage::id()));
        let c = Arc::new(
            Core::new(
                root.clone(),
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        c.macro_headless_permission.store(true, Ordering::SeqCst);
        let input = json!({"command":"Get-Service","id":"terminal-first","elevated":false});
        async fn answer(c: &Arc<Core>, value: &str) {
            for _ in 0..100 {
                let row = c.permissions_list().unwrap();
                if let Some(id) = row[0]["id"].as_str() {
                    let tx = c.pending.lock().unwrap().remove(id).unwrap();
                    tx.send(value.into()).unwrap();
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            panic!("request not shown");
        }
        let core = c.clone();
        let request = input.clone();
        let task = tokio::spawn(async move {
            core.permission("terminal.command", &request, Risk::Mandatory)
                .await
        });
        answer(&c, "always").await;
        task.await.unwrap().unwrap();
        let mut same = input.clone();
        same["id"] = json!("terminal-second");
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            c.permission("terminal.command", &same, Risk::Mandatory),
        )
        .await
        .unwrap()
        .unwrap();
        let core = c.clone();
        same["command"] = json!("Get-Process");
        let task = tokio::spawn(async move {
            core.permission("terminal.command", &same, Risk::Mandatory)
                .await
        });
        answer(&c, "deny").await;
        assert!(task.await.unwrap().is_err());
        let stale = scope(&c, "terminal.command", &input, Risk::Mandatory).unwrap();
        invalidate(&c).unwrap();
        assert_eq!(
            save(&c, "terminal.command", &stale).unwrap_err(),
            "PERMISSION_SCOPE_CHANGED"
        );
        let core = c.clone();
        let task = tokio::spawn(async move {
            core.permission(
                "files.delete",
                &json!({"path":"never-deleted"}),
                Risk::Mandatory,
            )
            .await
        });
        answer(&c, "always").await;
        assert_eq!(
            task.await.unwrap().unwrap_err(),
            "PERMISSION_NOT_REMEMBERABLE"
        );
        assert_eq!(list(&c).unwrap(), json!([]));
        drop(c);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn exact_rules_bind_command_privilege_scope_and_survive_restart() {
        let root = std::env::temp_dir().join(format!("alfred-rules-{}", storage::id()));
        let resource = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        let c = Core::new(root.clone(), resource.clone()).unwrap();
        let review = json!({"id":"terminal-1","command":"Get-Service","cwd":"C:/scope","elevated":false,"shell":"fixed","engine_sha256":"abc"});
        let a = scope(&c, "terminal.command", &review, Risk::Mandatory).unwrap();
        save(&c, "terminal.command", &a).unwrap();
        drop(c);
        let c = Core::new(root.clone(), resource).unwrap();
        let mut same = review.clone();
        same["id"] = json!("terminal-2");
        assert!(allowed(
            &c,
            "terminal.command",
            &scope(&c, "terminal.command", &same, Risk::Mandatory).unwrap()
        )
        .unwrap());
        for key in ["command", "cwd", "elevated", "engine_sha256"] {
            let mut changed = review.clone();
            changed[key] = json!("changed");
            assert!(!allowed(
                &c,
                "terminal.command",
                &scope(&c, "terminal.command", &changed, Risk::Mandatory).unwrap()
            )
            .unwrap());
        }
        c.store
            .set_setting("allowed_roots", &json!(["C:/other"]))
            .unwrap();
        assert!(!allowed(
            &c,
            "terminal.command",
            &scope(&c, "terminal.command", &review, Risk::Mandatory).unwrap()
        )
        .unwrap());
        let id = list(&c).unwrap()[0]["id"].as_str().unwrap().to_owned();
        revoke(&c, &id).unwrap();
        assert_eq!(list(&c).unwrap(), json!([]));
        assert!(scope(&c, "files.delete", &json!({}), Risk::Mandatory).is_none());
        assert!(scope(
            &c,
            "windows.admin_action",
            &json!({"action":"sfc_repair"}),
            Risk::Mandatory
        )
        .is_none());
        drop(c);
        std::fs::remove_dir_all(root).unwrap();
    }
}
