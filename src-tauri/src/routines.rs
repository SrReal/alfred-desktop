//! Local shortcuts to reviewed macro inputs. They confer no execution authority.
use crate::{macros, policy, runtime::Core, storage};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

const KEY: &str = "routine_shortcuts";
const LIMIT: usize = 12;
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Routine {
    id: String,
    revision: String,
    name: String,
    macro_id: String,
    fingerprint: String,
    body: Value,
    needs_review: bool,
}
fn read(db: &Connection) -> Result<Vec<Routine>> {
    let raw: Option<String> = db
        .query_row("SELECT json_value FROM settings WHERE key=?", [KEY], |r| {
            r.get(0)
        })
        .optional()
        .map_err(|_| "ROUTINES_READ")?;
    let Some(raw) = raw else { return Ok(vec![]) };
    if raw.len() > LIMIT * 70_000 {
        return Err("ROUTINES_LIMIT".into());
    }
    let rows: Vec<Routine> = serde_json::from_str(&raw).map_err(|_| "ROUTINES_INVALID")?;
    if rows.len() > LIMIT {
        return Err("ROUTINES_LIMIT".into());
    }
    let mut ids = std::collections::BTreeSet::new();
    for r in &rows {
        if !ids.insert(&r.id)
            || uuid::Uuid::parse_str(&r.id).is_err()
            || uuid::Uuid::parse_str(&r.macro_id).is_err()
            || r.name.trim().is_empty()
            || r.name.len() > 200
            || r.fingerprint.len() != 64
            || !r.fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
            || !r.body.is_object()
            || r.body.to_string().len() > 65_536
        {
            return Err("ROUTINES_INVALID".into());
        }
    }
    Ok(rows)
}
fn rows(c: &Core) -> Result<Vec<Routine>> {
    let db = c.store.0.lock().map_err(|_| "DB_LOCK")?;
    read(&db)
}
fn selected(c: &Core, input: &Value) -> Result<Routine> {
    let r = rows(c)?
        .into_iter()
        .find(|r| input["id"] == r.id)
        .ok_or("La rutina ya no existe. Actualiza la lista.")?;
    if input["revision"] != r.revision {
        return Err("La rutina ha cambiado. Actualiza la lista y revísala.".into());
    }
    Ok(r)
}
fn macro_input(r: &Routine) -> Value {
    json!({"id":r.macro_id,"body":r.body,"expected":{"fingerprint":r.fingerprint}})
}
fn available(c: &Core, r: &Routine) -> Result<()> {
    if r.needs_review {
        return Err("Revisa los valores de esta rutina restaurada antes de usarla.".into());
    }
    macros::prepare(c, &macro_input(r)).map(|_| ())
}
pub fn status(c: &Core) -> Result<Value> {
    Ok(c.routine_run.lock().map_err(|_| "ROUTINE_LOCK")?.clone())
}
pub fn dispatch(c: &Core, operation: &str, input: &Value) -> Result<Value> {
    if operation == "routines.status" {
        return status(c);
    }
    if operation == "routines.list" {
        return Ok(Value::Array(
            rows(c)?
                .iter()
                .map(|r| {
                    let issue = if r.needs_review {
                        Some("Revisa los valores de esta rutina restaurada antes de usarla.".into())
                    } else {
                        macros::check_review(c, &r.macro_id, &r.fingerprint).err()
                    };
                    let mut value = serde_json::to_value(r).expect("routine serializable");
                    value["available"] = json!(issue.is_none());
                    value["issue"] = json!(issue);
                    value
                })
                .collect(),
        ));
    }
    let active = c.routine_run.lock().map_err(|_| "ROUTINE_LOCK")?;
    if active["status"] == "running" && active["id"] == input["id"] {
        return Err("La rutina sigue en curso. Espera al resultado antes de modificarla.".into());
    }
    let replacement = if operation == "routines.save" {
        let name = input["name"]
            .as_str()
            .filter(|s| !s.trim().is_empty() && s.len() <= 200)
            .ok_or("Escribe un nombre de hasta 200 caracteres.")?;
        if policy::contains_secret(name) {
            return Err("No guardes credenciales en una rutina.".into());
        }
        let r = Routine {
            id: input["id"].as_str().unwrap_or("").into(),
            revision: storage::id(),
            name: name.trim().into(),
            macro_id: input["macroId"].as_str().ok_or("MACRO_ID")?.into(),
            fingerprint: input["fingerprint"]
                .as_str()
                .ok_or("MACRO_FINGERPRINT")?
                .into(),
            body: input.get("body").cloned().unwrap_or(json!({})),
            needs_review: false,
        };
        // Resolves typed values and validates each child schema without executing it.
        available(c, &r)?;
        Some(r)
    } else if operation == "routines.delete" {
        if input["confirmed"] != true {
            return Err("ROUTINE_DELETE_CONFIRMATION".into());
        }
        None
    } else {
        return Err("ROUTINE_OPERATION".into());
    };
    let db = c.store.0.lock().map_err(|_| "DB_LOCK")?;
    let mut all = read(&db)?;
    let index = if let Some(id) = input["id"].as_str() {
        let index = all
            .iter()
            .position(|r| r.id == id)
            .ok_or("ROUTINE_NOT_FOUND")?;
        if input["revision"] != all[index].revision {
            return Err("La rutina ha cambiado. Actualiza la lista y revísala.".into());
        }
        Some(index)
    } else {
        None
    };
    match (replacement, index) {
        (Some(r), Some(index)) => all[index] = r,
        (Some(mut r), None) => {
            if all.len() >= LIMIT {
                return Err("Puedes guardar hasta 12 rutinas. Quita una para añadir otra.".into());
            }
            r.id = storage::id();
            all.push(r);
        }
        (None, Some(index)) => {
            all.remove(index);
        }
        (None, None) => return Err("ROUTINE_NOT_FOUND".into()),
    }
    db.execute(
        "INSERT OR REPLACE INTO settings VALUES(?,?)",
        params![
            KEY,
            serde_json::to_string(&all).map_err(|_| "ROUTINES_JSON")?
        ],
    )
    .map_err(|_| "ROUTINES_WRITE")?;
    drop(db);
    drop(active);
    c.emit("alfredpet:routines", &json!({}));
    Ok(json!({"saved":true}))
}
struct RunGuard(Arc<Core>);
impl Drop for RunGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.routine_run.lock() {
            if state["status"] == "running" {
                state["status"] = json!("uncertain");
                state["message"] = json!("Ejecución interrumpida; algún paso puede haberse completado. Revisa Actividad antes de repetir.");
            }
        }
        self.0.emit("alfredpet:routines", &json!({}));
    }
}
pub async fn run(c: &Arc<Core>, input: &Value) -> Result<Value> {
    let r = {
        let mut state = c.routine_run.lock().map_err(|_| "ROUTINE_LOCK")?;
        if state["status"] == "running" {
            return Err("Ya hay una rutina en curso.".into());
        }
        let r = selected(c, input)?;
        available(c, &r)?;
        *state = json!({"id":r.id,"name":r.name,"status":"running","startedAt":storage::now()});
        r
    };
    let guard = RunGuard(c.clone());
    c.emit("alfredpet:routines", &json!({}));
    // Mandatory macro approval and every normal child permission remain in Core.
    let result = c.execute("macros.run", macro_input(&r)).await;
    let mut state = c.routine_run.lock().map_err(|_| "ROUTINE_LOCK")?;
    state["finishedAt"] = json!(storage::now());
    match result {
        Ok(value) => {
            let results = value["results"].as_array().ok_or("ROUTINE_RESULT")?;
            state["status"] = json!(if value["complete"] == true {
                "complete"
            } else {
                "incomplete"
            });
            for (key, status) in [
                ("succeeded", "success"),
                ("failed", "failed"),
                ("skipped", "skipped"),
            ] {
                state[key] = json!(results.iter().filter(|v| v["status"] == status).count());
            }
        }
        Err(error) => {
            state["status"] = json!("interrupted");
            state["message"] = policy::redact(&json!(error.chars().take(600).collect::<String>()));
        }
    }
    let result = state.clone();
    drop(state);
    drop(guard);
    Ok(result)
}

#[cfg(test)]
mod tests;
