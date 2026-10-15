//! Durable, run-scoped progress. Plans are model declarations; actions are runtime observations.
use crate::{
    policy,
    runtime::Core,
    storage::{self, Store},
};
use rusqlite::params;
use serde_json::{json, Value};
use std::sync::Arc;

tokio::task_local! { pub(crate) static RUN: String; }
tokio::task_local! { static ACTION: String; }

pub fn start(c: &Core, run: &str, conversation: &str, message: &str) -> Result<(), String> {
    let data = json!({"id":run,"conversation_id":conversation,"message_id":message,
        "status":"running","revision":0,"created_at":storage::now(),"updated_at":storage::now(),"plan":[],"actions":[]});
    c.store.execute(
        "INSERT INTO chat_progress VALUES(?,?,?,?)",
        &[
            json!(run),
            json!(conversation),
            json!(message),
            json!(data.to_string()),
        ],
    )?;
    publish(c, &data);
    Ok(())
}
fn publish(c: &Core, data: &Value) {
    c.emit("alfredpet:progress", data);
}
// One connection lock covers read/modify/write, including concurrent tool calls.
fn mutate(store: &Store, run: &str, f: impl FnOnce(&mut Value)) -> Result<Value, String> {
    let db = store.0.lock().map_err(|_| "PROGRESS_LOCK")?;
    let raw: String = db
        .query_row(
            "SELECT data_json FROM chat_progress WHERE id=?",
            [run],
            |r| r.get(0),
        )
        .map_err(|_| "PROGRESS_RUN_UNKNOWN")?;
    let mut data: Value = serde_json::from_str(&raw).map_err(|_| "PROGRESS_INVALID")?;
    if data["status"] != "running" {
        return Err("PROGRESS_RUN_CLOSED".into());
    }
    f(&mut data);
    data["revision"] = json!(data["revision"].as_u64().unwrap_or(0) + 1);
    data["updated_at"] = json!(storage::now());
    db.execute(
        "UPDATE chat_progress SET data_json=? WHERE id=?",
        params![data.to_string(), run],
    )
    .map_err(|_| "PROGRESS_WRITE")?;
    Ok(data)
}
fn update(c: &Core, run: &str, f: impl FnOnce(&mut Value)) -> Result<Value, String> {
    let data = mutate(&c.store, run, f)?;
    publish(c, &data);
    Ok(data)
}
pub fn list(c: &Core, conversation: &str) -> Result<Value, String> {
    let rows = c.store.query(
        "SELECT data_json FROM chat_progress WHERE conversation_id=? ORDER BY rowid",
        &[json!(conversation)],
    )?;
    rows.as_array()
        .ok_or("PROGRESS_ROWS")?
        .iter()
        .map(|row| {
            serde_json::from_str(row["data_json"].as_str().ok_or("PROGRESS_DATA")?)
                .map_err(|_| "PROGRESS_INVALID".into())
        })
        .collect::<Result<Vec<Value>, String>>()
        .map(Value::Array)
}
pub fn plan(c: &Core, input: &Value) -> Result<Value, String> {
    let steps = input["steps"]
        .as_array()
        .filter(|s| !s.is_empty() && s.len() <= 12)
        .ok_or("PLAN_STEPS_1_TO_12")?;
    let mut ids = std::collections::HashSet::new();
    let mut active = 0;
    let mut cleaned = Vec::new();
    for step in steps {
        let id = step["id"]
            .as_str()
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 40
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            })
            .ok_or("PLAN_ID")?;
        let title = step["title"]
            .as_str()
            .map(str::trim)
            .filter(|s| {
                !s.is_empty()
                    && s.chars().count() <= 160
                    && !s.chars().any(char::is_control)
                    && !policy::contains_secret(s)
            })
            .ok_or("PLAN_TITLE")?;
        let status = step["status"]
            .as_str()
            .filter(|s| ["pending", "in_progress", "completed", "blocked", "skipped"].contains(s))
            .ok_or("PLAN_STATUS")?;
        if !ids.insert(id) {
            return Err("PLAN_DUPLICATE_ID".into());
        }
        if status == "in_progress" {
            active += 1;
        }
        cleaned.push(json!({"id":id,"title":title,"status":status}));
    }
    if active > 1 {
        return Err("PLAN_ONE_ACTIVE_STEP".into());
    }
    let run = RUN
        .try_with(Clone::clone)
        .map_err(|_| "RUN_SCOPE_REQUIRED")?;
    update(c, &run, |data| data["plan"] = json!(cleaned))?;
    Ok(json!({"updated":true}))
}
pub fn finish(c: &Core, run: &str, status: &str) -> Result<(), String> {
    update(c, run, |data| close(data, status))?;
    Ok(())
}
fn close(data: &mut Value, status: &str) {
    data["status"] = json!(status);
    for a in data["actions"].as_array_mut().into_iter().flatten() {
        if ["running", "waiting_permission", "waiting_user"]
            .contains(&a["status"].as_str().unwrap_or(""))
        {
            a["status"] = json!("interrupted");
            a["updated_at"] = json!(storage::now());
        }
    }
    for s in data["plan"].as_array_mut().into_iter().flatten() {
        if s["status"] == "in_progress" {
            s["status"] = json!("interrupted");
        }
    }
}
pub fn recover(store: &Store) -> Result<(), String> {
    let rows = store.query(
        "SELECT id FROM chat_progress WHERE json_extract(data_json,'$.status')='running'",
        &[],
    )?;
    for row in rows.as_array().ok_or("PROGRESS_ROWS")? {
        mutate(store, row["id"].as_str().ok_or("PROGRESS_ID")?, |data| {
            close(data, "interrupted")
        })?;
    }
    Ok(())
}

struct ActionGuard {
    core: Arc<Core>,
    run: String,
    id: String,
    finished: bool,
}
impl ActionGuard {
    fn status(&self, status: &str) {
        let _ = update(&self.core, &self.run, |data| {
            if let Some(a) = data["actions"]
                .as_array_mut()
                .and_then(|rows| rows.iter_mut().find(|a| a["id"] == self.id))
            {
                a["status"] = json!(status);
                a["updated_at"] = json!(storage::now());
            }
        });
    }
}
impl Drop for ActionGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.status("interrupted");
        }
    }
}
pub async fn action<F>(c: &Arc<Core>, tool: &str, future: F) -> Result<Value, String>
where
    F: std::future::Future<Output = Result<Value, String>>,
{
    let run = RUN
        .try_with(Clone::clone)
        .map_err(|_| "RUN_SCOPE_REQUIRED")?;
    let id = storage::id();
    // Bounded per turn. Never save tool arguments, output, permission previews or tokens here.
    let data = update(c, &run, |data| {
        let rows = data["actions"].as_array_mut().expect("actions array");
        if rows.len() < 200 {
            rows.push(json!({"id":id,"tool":tool,"status":"running","created_at":storage::now(),"updated_at":storage::now()}));
        }
    })?;
    if data["actions"]
        .as_array()
        .is_none_or(|a| !a.iter().any(|a| a["id"] == id))
    {
        return Err("PROGRESS_ACTION_LIMIT".into());
    }
    let mut guard = ActionGuard {
        core: c.clone(),
        run,
        id: id.clone(),
        finished: false,
    };
    let result = ACTION.scope(id, future).await;
    let status = match &result {
        Ok(v) if v["isError"] == true => "error",
        Ok(_) => "completed",
        Err(e) if e.contains("DENIED") => "denied",
        Err(e) if e.contains("INTERRUPTED") || e.contains("CANCELLED") => "interrupted",
        Err(_) => "error",
    };
    guard.status(status);
    guard.finished = true;
    result
}
pub struct WaitingGuard(Option<ActionGuard>);
// Drop restores activity on allow, deny, timeout or cancellation; the action guard
// subsequently records the final outcome. Already remembered permissions never enter this scope.
pub fn waiting(c: &Arc<Core>, status: &str) -> WaitingGuard {
    let guard = RUN
        .try_with(Clone::clone)
        .ok()
        .zip(ACTION.try_with(Clone::clone).ok())
        .map(|(run, id)| ActionGuard {
            core: c.clone(),
            run,
            id,
            finished: true,
        });
    if let Some(g) = &guard {
        g.status(status);
    }
    WaitingGuard(guard)
}
impl Drop for WaitingGuard {
    fn drop(&mut self) {
        if let Some(g) = &self.0 {
            g.status("running");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Arc<Core> {
        let root = std::env::temp_dir().join(format!("ap-progress-{}", storage::id()));
        let c = Arc::new(
            Core::new(
                root,
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        c.store.execute("INSERT INTO conversations(id,title,created_at,updated_at) VALUES('chat','Test',?,?)", &[json!(storage::now()),json!(storage::now())]).unwrap();
        c.store
            .execute(
                "INSERT INTO messages VALUES('message','chat','user','Test','complete',?)",
                &[json!(storage::now())],
            )
            .unwrap();
        start(&c, "run", "chat", "message").unwrap();
        c
    }
    fn current(c: &Core) -> Value {
        list(c, "chat").unwrap()[0].clone()
    }
    fn clean(c: Arc<Core>) {
        let root = c.root.clone();
        drop(c);
        assert!(root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("ap-progress-"));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn plan_is_scoped_bounded_and_never_executes_tools() {
        let c = fixture();
        let valid =
            json!({"steps":[{"id":"read","title":"Consultar información","status":"pending"}]});
        assert_eq!(plan(&c, &valid).unwrap_err(), "RUN_SCOPE_REQUIRED");
        RUN.scope("run".into(),async {
            plan(&c,&valid).unwrap();
            for bad in [json!({"steps":[]}),json!({"steps":[{"id":"a","title":"a","status":"invented"}]}),json!({"steps":[{"id":"a","title":"a","status":"in_progress"},{"id":"b","title":"b","status":"in_progress"}]}),json!({"steps":[{"id":"a","title":"a","status":"pending"},{"id":"a","title":"b","status":"pending"}]})] { assert!(plan(&c,&bad).is_err()); }
        }).await;
        assert_eq!(current(&c)["revision"], 1);
        assert_eq!(current(&c)["actions"], json!([]));
        assert!(list(&c, "another-chat")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
        clean(c);
    }
    #[tokio::test]
    async fn action_outcomes_waits_and_inputs_are_separate_from_plan() {
        let c = fixture();
        RUN.scope("run".into(), async {
            action(&c, "skills.list", async {
                let wait = waiting(&c, "waiting_permission");
                assert_eq!(current(&c)["actions"][0]["status"], "waiting_permission");
                drop(wait);
                assert_eq!(current(&c)["actions"][0]["status"], "running");
                Ok(json!({"private":"not in progress"}))
            })
            .await
            .unwrap();
            assert!(action(&c, "files.copy", async {
                Err("PERMISSION_DENIED_OR_TIMEOUT".into())
            })
            .await
            .is_err());
            assert!(action(&c, "files.read", async {
                Err("ERROR with private content".into())
            })
            .await
            .is_err());
            action(&c, "mcp.call", async { Ok(json!({"isError":true})) })
                .await
                .unwrap();
        })
        .await;
        let data = current(&c);
        assert_eq!(data["actions"][0]["status"], "completed");
        assert_eq!(data["actions"][1]["status"], "denied");
        assert_eq!(data["actions"][2]["status"], "error");
        assert_eq!(data["actions"][3]["status"], "error");
        assert!(!data.to_string().contains("private"));
        clean(c);
    }
    #[tokio::test]
    async fn cancellation_drops_wait_then_records_interruption() {
        let c = fixture();
        RUN.scope("run".into(), async {
            let result = tokio::time::timeout(
                std::time::Duration::from_millis(10),
                action(&c, "alfredpet.ask_user", async {
                    let _waiting = waiting(&c, "waiting_user");
                    assert_eq!(current(&c)["actions"][0]["status"], "waiting_user");
                    std::future::pending::<Result<Value, String>>().await
                }),
            )
            .await;
            assert!(result.is_err());
        })
        .await;
        assert_eq!(current(&c)["actions"][0]["status"], "interrupted");
        finish(&c, "run", "interrupted").unwrap();
        assert_eq!(current(&c)["status"], "interrupted");
        clean(c);
    }
    #[tokio::test]
    async fn recovery_persistence_and_delete_do_not_invent_completion() {
        let c = fixture();
        RUN.scope("run".into(),async { plan(&c,&json!({"steps":[{"id":"a","title":"Revisar","status":"in_progress"},{"id":"b","title":"Informar","status":"pending"}]})).unwrap(); }).await;
        recover(&c.store).unwrap();
        let data = current(&c);
        assert_eq!(data["status"], "interrupted");
        assert_eq!(data["plan"][0]["status"], "interrupted");
        assert_eq!(data["plan"][1]["status"], "pending");
        assert!(finish(&c, "run", "completed").is_err());
        let second = Store::open(&c.root.join("data/alfredpet.db")).unwrap();
        assert_eq!(
            second
                .query("SELECT count(*) AS n FROM chat_progress", &[])
                .unwrap()[0]["n"],
            1
        );
        drop(second);
        c.store
            .execute("DELETE FROM conversations WHERE id='chat'", &[])
            .unwrap();
        assert!(list(&c, "chat").unwrap().as_array().unwrap().is_empty());
        clean(c);
    }
    #[tokio::test]
    async fn parallel_actions_keep_distinct_ids_and_results() {
        let c = fixture();
        RUN.scope("run".into(), async {
            let (a, b) = tokio::join!(
                action(&c, "skills.list", async {
                    tokio::task::yield_now().await;
                    Ok(json!({}))
                }),
                action(&c, "tasks.list", async { Ok(json!([])) })
            );
            a.unwrap();
            b.unwrap();
        })
        .await;
        let data = current(&c);
        assert_eq!(data["actions"].as_array().unwrap().len(), 2);
        assert_ne!(data["actions"][0]["id"], data["actions"][1]["id"]);
        assert!(data["actions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["status"] == "completed"));
        assert_eq!(data["revision"], 4);
        clean(c);
    }
    #[tokio::test]
    async fn actual_permission_channel_reports_wait_and_denial() {
        let c = fixture();
        c.media_headless_permission
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let worker = c.clone();
        let task = tokio::spawn(async move {
            RUN.scope(
                "run".into(),
                action(&worker, "files.copy", async {
                    worker
                        .permission(
                            "files.copy",
                            &json!({"source":"fixture","destination":"fixture-copy"}),
                            policy::Risk::Contextual,
                        )
                        .await?;
                    Ok(json!({}))
                }),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if current(&c)["actions"][0]["status"] == "waiting_permission" {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let id = c
            .store
            .query(
                "SELECT id FROM permission_requests WHERE status='pending'",
                &[],
            )
            .unwrap()[0]["id"]
            .as_str()
            .unwrap()
            .to_string();
        c.pending
            .lock()
            .unwrap()
            .remove(&id)
            .unwrap()
            .send("deny".into())
            .unwrap();
        assert_eq!(
            task.await.unwrap().unwrap_err(),
            "PERMISSION_DENIED_OR_TIMEOUT"
        );
        assert_eq!(current(&c)["actions"][0]["status"], "denied");
        // Attention is a detached notification; wait for its owned Arc to release.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        clean(c);
    }
}
