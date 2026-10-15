//! User-reviewed reusable instructions, versioned independently of executable plugins.
use crate::{policy, runtime::Core, storage};
use serde_json::{json, Value};

pub const TOOLS: &[&str] = &[
    "skills.list",
    "skills.inspect",
    "skills.save",
    "skills.set_enabled",
    "skills.delete",
];
const KEY: &str = "alfred_skills";
pub fn schema(tool: &str) -> Value {
    let id = json!({"type":"string","pattern":"^[a-z][a-z0-9_-]{0,63}$"});
    let revision = json!({"type":"integer","minimum":0});
    let (required, properties) = match tool {
        "skills.list" => (vec![], json!({})),
        "skills.inspect" => (vec!["id"], json!({"id":id})),
        "skills.save" => (
            vec![
                "id",
                "name",
                "description",
                "instructions",
                "enabled",
                "expected_revision",
            ],
            json!({"id":id,"name":{"type":"string","minLength":1,"maxLength":120},"description":{"type":"string","minLength":1,"maxLength":600},"instructions":{"type":"string","minLength":1,"maxLength":12000},"enabled":{"type":"boolean"},"expected_revision":revision}),
        ),
        "skills.set_enabled" => (
            vec!["id", "enabled", "expected_revision"],
            json!({"id":id,"enabled":{"type":"boolean"},"expected_revision":revision}),
        ),
        "skills.delete" => (
            vec!["id", "expected_revision"],
            json!({"id":id,"expected_revision":revision}),
        ),
        _ => return json!({"not":{}}),
    };
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
pub fn passive(tool: &str) -> bool {
    matches!(tool, "skills.list" | "skills.inspect")
}
pub fn revoke(c: &Core) -> Result<(), String> {
    c.store
        .set_setting("skill_authority", &json!(storage::id()))
}
fn rows(v: &Value) -> Result<Vec<Value>, String> {
    if v.is_null() {
        Ok(vec![])
    } else {
        v.as_array().cloned().ok_or("SKILL_STORAGE".into())
    }
}
pub fn catalog(c: &Core) -> Result<Value, String> {
    if !c.enabled("skills") || c.store.setting("privacy") == true {
        return Ok(json!([]));
    }
    Ok(json!(rows(&c.store.setting(KEY))?
        .iter()
        .filter(|r| r["enabled"] == true)
        .map(summary)
        .collect::<Vec<_>>()))
}
fn summary(row: &Value) -> Value {
    json!({"id":row["id"],"name":row["name"],"description":row["description"],"revision":row["revision"],"enabled":row["enabled"]})
}
pub struct Prepared {
    tool: String,
    input: Value,
    before: Value,
    authority: Value,
}
impl Prepared {
    pub fn prepare(c: &Core, tool: &str, input: &Value) -> Result<Self, String> {
        if !jsonschema::validator_for(&schema(tool))
            .map_err(|_| "SKILL_SCHEMA")?
            .is_valid(input)
        {
            return Err("SKILL_INPUT".into());
        }
        let p = Self {
            tool: tool.into(),
            input: input.clone(),
            before: c.store.setting(KEY),
            authority: c.store.setting("skill_authority"),
        };
        p.check(c)?;
        p.result()?;
        Ok(p)
    }
    pub fn check(&self, c: &Core) -> Result<(), String> {
        if !c.enabled("skills")
            || c.store.setting("privacy") == true
            || c.store.setting("skill_authority") != self.authority
        {
            return Err("SKILL_REVOKED".into());
        }
        if c.store.setting(KEY) != self.before {
            return Err("SKILL_CHANGED".into());
        }
        Ok(())
    }
    pub fn review(&self) -> Value {
        json!({"action":self.tool,"proposal":self.input,"previous":rows(&self.before).unwrap_or_default().into_iter().find(|r|r["id"]==self.input["id"]),"effect":"Reusable instructions for relevant future chat requests. Does not grant tool permissions or execute code."})
    }
    fn result(&self) -> Result<Value, String> {
        let mut entries = rows(&self.before)?;
        if self.tool == "skills.list" {
            return Ok(json!(entries.iter().map(summary).collect::<Vec<_>>()));
        }
        let index = entries.iter().position(|r| r["id"] == self.input["id"]);
        if self.tool == "skills.inspect" {
            return index
                .map(|i| entries[i].clone())
                .ok_or("SKILL_NOT_FOUND".into());
        }
        let revision = index
            .and_then(|i| entries[i]["revision"].as_u64())
            .unwrap_or(0);
        if self.input["expected_revision"].as_u64() != Some(revision) {
            return Err("SKILL_CHANGED".into());
        }
        if self.tool == "skills.save" {
            for key in ["name", "description", "instructions"] {
                let value = self.input[key].as_str().ok_or("SKILL_TEXT")?;
                if value.trim().is_empty() || policy::contains_secret(value) {
                    return Err("SKILL_TEXT_OR_SECRET".into());
                }
            }
            if index.is_none() && entries.len() >= 24 {
                return Err("SKILL_LIMIT_24".into());
            }
            let mut row = self.input.clone();
            row.as_object_mut()
                .ok_or("SKILL_INPUT")?
                .remove("expected_revision");
            row["revision"] = json!(revision + 1);
            row["updated_at"] = json!(storage::now());
            let mut history = index
                .and_then(|i| entries[i]["history"].as_array().cloned())
                .unwrap_or_default();
            if let Some(i) = index {
                let mut previous = entries[i].clone();
                previous.as_object_mut().unwrap().remove("history");
                history.push(previous);
            }
            if history.len() > 10 {
                history.remove(0);
            }
            row["history"] = json!(history);
            if let Some(i) = index {
                entries[i] = row;
            } else {
                entries.push(row);
            }
        } else {
            let i = index.ok_or("SKILL_NOT_FOUND")?;
            if self.tool == "skills.delete" {
                entries.remove(i);
            } else if self.tool == "skills.set_enabled" {
                entries[i]["enabled"] = self.input["enabled"].clone();
                entries[i]["revision"] = json!(revision + 1);
                entries[i]["updated_at"] = json!(storage::now());
            } else {
                return Err("SKILL_TOOL".into());
            }
        }
        Ok(json!(entries))
    }
    pub fn execute(&self, c: &Core) -> Result<Value, String> {
        self.check(c)?;
        let result = self.result()?;
        if passive(&self.tool) {
            return Ok(result);
        }
        let db = c.store.0.lock().map_err(|_| "DB_LOCK")?;
        let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
        let read = |key: &str| -> Result<Value, String> {
            use rusqlite::OptionalExtension;
            let text: Option<String> = tx
                .query_row("SELECT json_value FROM settings WHERE key=?", [key], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(|e| e.to_string())?;
            text.map(|s| serde_json::from_str(&s).map_err(|_| "SKILL_STORAGE".into()))
                .unwrap_or(Ok(Value::Null))
        };
        let enabled: bool = tx
            .query_row(
                "SELECT enabled FROM module_configs WHERE module_id='skills'",
                [],
                |r| r.get(0),
            )
            .map_err(|_| "SKILL_DISABLED")?;
        if !enabled
            || read("privacy")? == true
            || read("skill_authority")? != self.authority
            || read(KEY)? != self.before
        {
            return Err("SKILL_CHANGED".into());
        }
        tx.execute("INSERT INTO settings(key,json_value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET json_value=excluded.json_value",[KEY,&result.to_string()]).map_err(|e|e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        drop(db);
        c.emit("alfredpet:skills", &json!({}));
        Ok(
            json!({"saved":self.tool!="skills.delete","deleted":self.tool=="skills.delete","skill":result.as_array().and_then(|a|a.iter().find(|r|r["id"]==self.input["id"])).map(summary)}),
        )
    }
}
