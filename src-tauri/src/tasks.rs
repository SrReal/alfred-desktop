use crate::{catalog, policy, runtime::Core, storage};
use chrono::{DateTime, Datelike, Duration, NaiveDate, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Frequency {
    Daily,
    Weekly,
    Monthly,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Recurrence {
    version: u8,
    frequency: Frequency,
    interval: u32,
    anchor: String,
}
#[derive(Clone, Deserialize, Serialize)]
struct Task {
    id: String,
    title: String,
    description: String,
    status: String,
    due_at: Option<String>,
    recurrence_json: Option<String>,
    project_id: Option<String>,
    conversation_id: Option<String>,
    created_at: String,
    updated_at: String,
    revision: i64,
}
const COLUMNS: &str = "id,title,description,status,due_at,recurrence_json,project_id,conversation_id,created_at,updated_at,revision";
fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        status: row.get(3)?,
        due_at: row.get(4)?,
        recurrence_json: row.get(5)?,
        project_id: row.get(6)?,
        conversation_id: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        revision: row.get(10)?,
    })
}
fn load(db: &Connection, id: &str) -> Result<Task, String> {
    db.query_row(
        &format!("SELECT {COLUMNS} FROM tasks WHERE id=?"),
        [id],
        row,
    )
    .optional()
    .map_err(|e| e.to_string())?
    .ok_or("TASK_NOT_FOUND".into())
}
fn instant(raw: &str) -> Result<DateTime<Utc>, String> {
    let value = DateTime::parse_from_rfc3339(raw)
        .map_err(|_| "TASK_DUE_DATE")?
        .with_timezone(&Utc);
    if !(1970..=9999).contains(&value.year()) {
        return Err("TASK_DUE_DATE".into());
    }
    Ok(value)
}
fn stamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}
fn recurrence(task: &Task) -> Result<Option<Recurrence>, String> {
    let Some(raw) = &task.recurrence_json else {
        return Ok(None);
    };
    let rec: Recurrence = serde_json::from_str(raw).map_err(|_| "TASK_RECURRENCE_INVALID")?;
    if rec.version != 1 || !(1..=365).contains(&rec.interval) || task.due_at.is_none() {
        return Err("TASK_RECURRENCE_INVALID".into());
    }
    instant(&rec.anchor)?;
    Ok(Some(rec))
}
// Cadence is anchored in UTC. Missed occurrences coalesce into a single pending
// reminder; completing it selects the first occurrence after both due and now.
fn next(rec: &Recurrence, after: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let anchor = instant(&rec.anchor)?;
    match rec.frequency {
        Frequency::Daily | Frequency::Weekly => {
            let days = i64::from(rec.interval)
                * if rec.frequency == Frequency::Weekly {
                    7
                } else {
                    1
                };
            let step = days * 86_400_000;
            let difference = after.signed_duration_since(anchor).num_milliseconds();
            let n = if difference < 0 {
                0
            } else {
                difference / step + 1
            };
            let candidate = anchor
                .checked_add_signed(Duration::milliseconds(
                    n.checked_mul(step).ok_or("TASK_RECURRENCE_RANGE")?,
                ))
                .ok_or("TASK_RECURRENCE_RANGE")?;
            if candidate.year() > 9999 {
                return Err("TASK_RECURRENCE_RANGE".into());
            }
            Ok(candidate)
        }
        Frequency::Monthly => {
            let origin = i64::from(anchor.year()) * 12 + i64::from(anchor.month0());
            let current = i64::from(after.year()) * 12 + i64::from(after.month0());
            let step = i64::from(rec.interval);
            let start = ((current - origin).max(0) / step).max(0);
            for n in start..start + 2 {
                let month = origin + n * step;
                let year =
                    i32::try_from(month.div_euclid(12)).map_err(|_| "TASK_RECURRENCE_RANGE")?;
                let m = month.rem_euclid(12) as u32 + 1;
                if year > 9999 {
                    return Err("TASK_RECURRENCE_RANGE".into());
                }
                let day = (1..=anchor.day())
                    .rev()
                    .find_map(|d| NaiveDate::from_ymd_opt(year, m, d))
                    .ok_or("TASK_RECURRENCE_RANGE")?;
                let candidate = day.and_time(anchor.time()).and_utc();
                if candidate > after {
                    return Ok(candidate);
                }
            }
            Err("TASK_RECURRENCE_RANGE".into())
        }
    }
}
fn draft(task: &mut Task, input: &Value) -> Result<(), String> {
    let old_due = task.due_at.clone();
    let old_recurrence = recurrence(task).ok().flatten();
    if let Some(title) = input.get("title") {
        task.title = title.as_str().ok_or("TASK_TITLE")?.trim().to_string();
    }
    if let Some(description) = input.get("description") {
        task.description = description.as_str().ok_or("TASK_DESCRIPTION")?.to_string();
    }
    if task.title.is_empty()
        || task.title.chars().count() > 500
        || task.description.chars().count() > 8000
    {
        return Err("TASK_TEXT_LENGTH".into());
    }
    if policy::contains_secret(&task.title) || policy::contains_secret(&task.description) {
        return Err("TASK_SENSITIVE_TEXT".into());
    }
    if let Some(due) = input.get("due_at") {
        task.due_at = if due.is_null() {
            None
        } else {
            Some(stamp(instant(due.as_str().ok_or("TASK_DUE_DATE")?)?))
        };
    }
    if let Some(value) = input.get("recurrence") {
        task.recurrence_json = if value.is_null() {
            None
        } else {
            let frequency: Frequency = serde_json::from_value(value["frequency"].clone())
                .map_err(|_| "TASK_RECURRENCE_INVALID")?;
            let interval = value["interval"].as_u64().unwrap_or(1);
            if !(1..=365).contains(&interval) {
                return Err("TASK_RECURRENCE_INVALID".into());
            }
            let due = task.due_at.clone().ok_or("TASK_RECURRENCE_REQUIRES_DUE")?;
            let anchor = old_recurrence
                .as_ref()
                .filter(|rec| {
                    rec.frequency == frequency
                        && u64::from(rec.interval) == interval
                        && old_due == task.due_at
                })
                .map(|rec| rec.anchor.clone())
                .unwrap_or(due);
            Some(
                serde_json::to_string(&Recurrence {
                    version: 1,
                    frequency,
                    interval: interval as u32,
                    anchor,
                })
                .map_err(|e| e.to_string())?,
            )
        };
    } else if input.get("due_at").is_some() && task.recurrence_json.is_some() {
        let mut rec = recurrence(task)?.ok_or("TASK_RECURRENCE_INVALID")?;
        rec.anchor = task.due_at.clone().ok_or("TASK_RECURRENCE_REQUIRES_DUE")?;
        task.recurrence_json = Some(serde_json::to_string(&rec).map_err(|e| e.to_string())?);
    }
    recurrence(task)?;
    for (field, destination) in [
        ("project_id", &mut task.project_id),
        ("conversation_id", &mut task.conversation_id),
    ] {
        if let Some(value) = input.get(field) {
            *destination = if value.is_null() {
                None
            } else {
                Some(value.as_str().ok_or("TASK_LINK")?.to_string())
            };
        }
    }
    Ok(())
}
fn links(db: &Connection, task: &Task) -> Result<(), String> {
    for (table, id) in [
        ("projects", &task.project_id),
        ("conversations", &task.conversation_id),
    ] {
        if let Some(id) = id {
            let exists: bool = db
                .query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id=?)"),
                    [id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            if !exists {
                return Err(format!("TASK_LINK_NOT_FOUND: {table}"));
            }
        }
    }
    Ok(())
}
fn value(db: &Connection, task: &Task) -> Result<Value, String> {
    let mut result = serde_json::to_value(task).map_err(|e| e.to_string())?;
    // Invalid legacy data remains visible and editable rather than hiding every task.
    match recurrence(task) {
        Ok(rec) => {
            result["recurrence"] = json!(rec);
            result["recurrence_error"] = Value::Null;
        }
        Err(error) => {
            result["recurrence"] = Value::Null;
            result["recurrence_error"] = json!(error);
        }
    }
    for (table, column, id, key) in [
        ("projects", "name", &task.project_id, "project_name"),
        (
            "conversations",
            "title",
            &task.conversation_id,
            "conversation_title",
        ),
    ] {
        result[key] = if let Some(id) = id {
            json!(db
                .query_row(
                    &format!("SELECT {column} FROM {table} WHERE id=?"),
                    [id],
                    |r| r.get::<_, String>(0)
                )
                .optional()
                .map_err(|e| e.to_string())?)
        } else {
            Value::Null
        };
    }
    Ok(result)
}
fn require_enabled(db: &Connection) -> Result<(), String> {
    let enabled: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM module_configs WHERE module_id='tasks' AND enabled=1)",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if enabled {
        Ok(())
    } else {
        Err("MODULE_DISABLED: tasks".into())
    }
}
pub fn dispatch(c: &Core, action: &str, input: &Value) -> Result<Value, String> {
    dispatch_at(c, action, input, Utc::now())
}
fn dispatch_at(c: &Core, action: &str, input: &Value, at: DateTime<Utc>) -> Result<Value, String> {
    let schema = catalog::tool_schema(&format!("tasks.{action}"), "");
    if !jsonschema::validator_for(&schema)
        .map_err(|_| "TASK_SCHEMA")?
        .is_valid(input)
    {
        return Err("TASK_INPUT".into());
    }
    let mut db = c.store.0.lock().map_err(|_| "DB_LOCK")?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    require_enabled(&tx)?;
    if action == "list" {
        let offset = input["offset"].as_i64().unwrap_or(0);
        let limit = input["limit"].as_i64().unwrap_or(50);
        let rows = {
            let mut statement = tx.prepare(&format!("SELECT {COLUMNS} FROM tasks WHERE (? IS NULL OR project_id=?) AND (? IS NULL OR conversation_id=?) ORDER BY created_at DESC,id LIMIT ? OFFSET ?")).map_err(|e| e.to_string())?;
            let p = input["project_id"].as_str();
            let c = input["conversation_id"].as_str();
            let mapped = statement
                .query_map(params![p, p, c, c, limit, offset], row)
                .map_err(|e| e.to_string())?;
            mapped
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
        };
        let result = rows
            .iter()
            .map(|r| value(&tx, r))
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().map_err(|e| e.to_string())?;
        return Ok(json!(result));
    }
    if action == "history" {
        let mut statement = tx.prepare("SELECT id,task_id,status,started_at,ended_at,result_summary,scheduled_for,error FROM task_runs WHERE task_id=? ORDER BY ended_at DESC,id LIMIT 100 OFFSET ?").map_err(|e| e.to_string())?;
        let result = statement.query_map(params![input["id"].as_str(), input["offset"].as_i64().unwrap_or(0)], |r| {
            Ok(json!({"id":r.get::<_,String>(0)?,"task_id":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,"started_at":r.get::<_,String>(3)?,"ended_at":r.get::<_,Option<String>>(4)?,"result_summary":r.get::<_,Option<String>>(5)?,"scheduled_for":r.get::<_,Option<String>>(6)?,"error":r.get::<_,Option<String>>(7)?}))
        }).map_err(|e| e.to_string())?.collect::<Result<Vec<_>,_>>().map_err(|e| e.to_string())?;
        return Ok(json!(result));
    }
    let now = stamp(at);
    let mut task = if action == "create" {
        Task {
            id: storage::id(),
            title: String::new(),
            description: String::new(),
            status: "queued".into(),
            due_at: None,
            recurrence_json: None,
            project_id: None,
            conversation_id: None,
            created_at: now.clone(),
            updated_at: now.clone(),
            revision: 0,
        }
    } else {
        let task = load(&tx, input["id"].as_str().ok_or("TASK_ID")?)?;
        if input["expected_revision"].as_i64() != Some(task.revision) {
            return Err("TASK_CHANGED: reload before retrying".into());
        }
        task
    };
    match action {
        "create" | "update" => {
            let old_due = task.due_at.clone();
            let old_rec = task.recurrence_json.clone();
            draft(&mut task, input)?;
            links(&tx, &task)?;
            if action == "create" {
                tx.execute("INSERT INTO tasks(id,title,description,status,due_at,recurrence_json,project_id,conversation_id,created_at,updated_at,revision) VALUES(?,?,?,?,?,?,?,?,?,?,?)",params![task.id,task.title,task.description,task.status,task.due_at,task.recurrence_json,task.project_id,task.conversation_id,task.created_at,task.updated_at,task.revision]).map_err(|e| e.to_string())?;
            } else {
                if old_due != task.due_at
                    || old_rec != task.recurrence_json
                    || input["reopen"] == true
                {
                    task.status = "queued".into();
                }
                task.updated_at = now;
                task.revision += 1;
                tx.execute("UPDATE tasks SET title=?,description=?,status=?,due_at=?,recurrence_json=?,project_id=?,conversation_id=?,updated_at=?,revision=? WHERE id=?",params![task.title,task.description,task.status,task.due_at,task.recurrence_json,task.project_id,task.conversation_id,task.updated_at,task.revision,task.id]).map_err(|e| e.to_string())?;
            }
        }
        "complete" => {
            if task.status == "succeeded" {
                return Err("TASK_ALREADY_COMPLETED".into());
            }
            if !["queued", "waiting_user", "blocked", "failed", "cancelled"]
                .contains(&task.status.as_str())
            {
                return Err("TASK_STATE_NOT_COMPLETABLE".into());
            }
            let rec = recurrence(&task)?;
            let next_due = if let Some(rec) = rec {
                Some(stamp(next(
                    &rec,
                    instant(task.due_at.as_deref().ok_or("TASK_DUE_DATE")?)?.max(at),
                )?))
            } else {
                None
            };
            tx.execute("INSERT INTO task_runs(id,task_id,status,started_at,ended_at,result_summary,scheduled_for) VALUES(?,?,'succeeded',?,?,?,?)",params![storage::id(),task.id,now,now,json!({"completed_by":"explicit_user_or_tool","title":task.title,"next_due_at":next_due}).to_string(),task.due_at]).map_err(|e| e.to_string())?;
            task.status = if next_due.is_some() {
                "queued"
            } else {
                "succeeded"
            }
            .into();
            if next_due.is_some() {
                task.due_at = next_due;
            }
            task.updated_at = now;
            task.revision += 1;
            tx.execute(
                "UPDATE tasks SET status=?,due_at=?,updated_at=?,revision=? WHERE id=?",
                params![
                    task.status,
                    task.due_at,
                    task.updated_at,
                    task.revision,
                    task.id
                ],
            )
            .map_err(|e| e.to_string())?;
        }
        "delete" => {
            tx.execute("DELETE FROM tasks WHERE id=?", [&task.id])
                .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            c.emit("alfredpet:tasks", &json!({"id":task.id,"deleted":true}));
            return Ok(json!({"id":task.id,"deleted":true,"history_retained":true}));
        }
        _ => return Err("TASK_ACTION".into()),
    }
    let result = value(&tx, &task)?;
    tx.commit().map_err(|e| e.to_string())?;
    c.emit("alfredpet:tasks", &json!({"id":task.id}));
    Ok(result)
}
// Claims occur atomically across independent processes/profiles. This provides
// at-most-once notification attempts; a crash after commit may miss an alert.
pub fn claim_due(c: &Core, at: DateTime<Utc>) -> Result<Vec<Value>, String> {
    let mut db = c.store.0.lock().map_err(|_| "DB_LOCK")?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let paused: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM settings WHERE key IN ('privacy','dnd') AND json_value='true') OR NOT EXISTS(SELECT 1 FROM module_configs WHERE module_id='tasks' AND enabled=1)", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    if paused {
        return Ok(Vec::new());
    }
    let tasks = {
        let mut stmt = tx.prepare(&format!("SELECT {COLUMNS} FROM tasks WHERE status='queued' AND due_at IS NOT NULL AND (julianday(due_at) IS NULL OR julianday(due_at)<=julianday(?)) ORDER BY due_at,id LIMIT 100")).map_err(|e| e.to_string())?;
        let mapped = stmt
            .query_map([stamp(at)], row)
            .map_err(|e| e.to_string())?;
        mapped
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };
    let changed = !tasks.is_empty();
    let mut claimed = Vec::new();
    for task in tasks {
        let due = instant(task.due_at.as_deref().ok_or("TASK_DUE_DATE")?);
        let error = match (&due, recurrence(&task)) {
            (Err(error), _) => Some(error.clone()),
            (_, Err(error)) => Some(error),
            _ => None,
        };
        if let Some(error) = error {
            tx.execute(
                "UPDATE tasks SET status='blocked',updated_at=?,revision=revision+1 WHERE id=?",
                params![stamp(at), task.id],
            )
            .map_err(|e| e.to_string())?;
            tx.execute("INSERT INTO task_runs(id,task_id,status,started_at,ended_at,scheduled_for,error) VALUES(?,?,'failed',?,?,?,?)",params![storage::id(),task.id,stamp(at),stamp(at),task.due_at,error]).map_err(|e|e.to_string())?;
        } else if due.map_err(|e| e.to_string())? <= at && claimed.len() < 100 {
            tx.execute("UPDATE tasks SET status='waiting_user',updated_at=?,revision=revision+1 WHERE id=?",params![stamp(at),task.id]).map_err(|e|e.to_string())?;
            claimed.push(json!({"id":task.id,"title":task.title,"due_at":task.due_at,"project_id":task.project_id,"conversation_id":task.conversation_id}));
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    if changed {
        c.emit("alfredpet:tasks", &json!({"reminders_checked":true}));
    }
    Ok(claimed)
}

#[cfg(all(test, windows))]
#[path = "tasks_tests.rs"]
mod tests;
