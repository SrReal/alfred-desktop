use crate::{runtime::Core, storage};
use rusqlite::params;
use serde_json::{json, Value};

// UI-only command. The confirmation is bound to the conversation displayed.
pub fn delete(c: &Core, input: &Value) -> Result<Value, String> {
    let id = input["id"].as_str().ok_or("CONVERSATION_ID")?;
    let expected = input["updated_at"]
        .as_str()
        .ok_or("CONVERSATION_REVISION")?;
    if input["confirmed"] != true {
        return Err("CONVERSATION_CONFIRM_REQUIRED".into());
    }
    let runs = c.runs.lock().map_err(|_| "RUN_LOCK")?;
    if runs.contains_key(id) {
        return Err(
            "CONVERSATION_BUSY: interrumpe la respuesta y espera a que termine antes de borrar"
                .into(),
        );
    }
    if let Some(job) = c.memory_jobs.lock().map_err(|_| "MEMORY_JOB_LOCK")?.get(id) {
        job.cancel();
    }
    let _memory = c.memory_actions.lock().map_err(|_| "MEMORY_ACTION_LOCK")?;
    let db = c.store.0.lock().map_err(|_| "DB_LOCK")?;
    let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
    let current: String = tx
        .query_row(
            "SELECT updated_at FROM conversations WHERE id=?",
            [id],
            |r| r.get(0),
        )
        .map_err(|_| "CONVERSATION_NOT_FOUND")?;
    if current != expected {
        return Err("CONVERSATION_CHANGED".into());
    }
    // Saved memories and external files have their own lifecycle. Keep them,
    // detach provenance and links; remove the local transcript and search data.
    for sql in [
        "DELETE FROM messages_fts WHERE conversation_id=?",
        "DELETE FROM attachments WHERE conversation_id=?",
        "UPDATE memory_sources SET conversation_id=NULL,message_id=NULL WHERE conversation_id=?",
        "UPDATE memories SET source_conversation_id=NULL WHERE source_conversation_id=?",
        "UPDATE conversations SET parent_conversation_id=NULL WHERE parent_conversation_id=?",
        "UPDATE tasks SET conversation_id=NULL,revision=revision+1 WHERE conversation_id=?",
        "DELETE FROM conversations WHERE id=?",
    ] {
        tx.execute(sql, [id]).map_err(|e| e.to_string())?;
    }
    tx.execute(
        "DELETE FROM settings WHERE key=?",
        [format!("chat_draft.{id}")],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "DELETE FROM settings WHERE key='active_conversation' AND json_value=?",
        [json!(id).to_string()],
    )
    .map_err(|e| e.to_string())?;
    tx.execute("INSERT INTO activity_events(id,timestamp,tool_id,kind,risk,input_summary,output_summary,status,duration_ms) VALUES(?,?,'conversations.delete','tool','mandatory',?,'Local conversation deleted','succeeded',0)", params![storage::id(),storage::now(),id]).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    drop(db);
    drop(runs);
    c.emit("alfredpet:conversations", &json!({"deleted":id}));
    c.emit("alfredpet:tasks", &json!({}));
    Ok(json!({"deleted":id,"saved_memories_preserved":true,"original_files_preserved":true}))
}
