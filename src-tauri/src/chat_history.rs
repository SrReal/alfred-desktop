//! UI initiated variants: copy a bounded prefix, never resume the parent's CLI session.
use crate::storage::{self, Store};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub conversation_id: String,
    message_id: String,
    expected_text: String,
    mode: String,
    text: String,
}

pub fn branch(store: &Store, input: Revision) -> Result<Value, String> {
    let db = store.0.lock().map_err(|_| "DB_LOCK")?;
    let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
    let parent = tx
        .query_row(
            "SELECT title,project_id,no_memory FROM conversations WHERE id=?",
            [&input.conversation_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )
        .map_err(|_| "CONVERSATION_NOT_FOUND")?;
    let mut stmt = tx.prepare("SELECT id,role,content_text,status,created_at FROM messages WHERE conversation_id=? ORDER BY created_at,rowid").map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([&input.conversation_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    drop(stmt);
    let target = rows
        .iter()
        .position(|r| r.0 == input.message_id)
        .ok_or("MESSAGE_NOT_FOUND")?;
    if rows[target].2 != input.expected_text || rows[target].3 == "streaming" {
        return Err("MESSAGE_CHANGED".into());
    }
    let (cut, text, label) = match input.mode.as_str() {
        "edit" if rows[target].1 == "user" => (target, input.text, "Edición"),
        "regenerate" if rows[target].1 == "assistant" && input.text.is_empty() => {
            let cut = (0..target)
                .rev()
                .find(|i| rows[*i].1 == "user")
                .ok_or("USER_MESSAGE_NOT_FOUND")?;
            (cut, rows[cut].2.clone(), "Regeneración")
        }
        _ => return Err("MESSAGE_ACTION_INVALID".into()),
    };
    if text.trim().is_empty() || text.len() > 64000 {
        return Err("PROMPT_LENGTH".into());
    }
    let history: Vec<Value> = rows[..cut]
        .iter()
        .filter(|r| ["user", "assistant"].contains(&r.1.as_str()))
        .map(|r| json!({"role":r.1,"text":r.2,"status":r.3}))
        .collect();
    let context =
        json!({"kind":"visible_transcript_before_revised_request","messages":history}).to_string();
    if cut > 400 || context.len() > 192000 {
        return Err("BRANCH_HISTORY_LIMIT: el historial anterior supera 400 mensajes o 192 KB; crea una conversación con el contexto que quieras conservar".into());
    }
    let id = storage::id();
    let now = storage::now();
    tx.execute("INSERT INTO conversations(id,title,title_user_edited,summary,project_id,parent_conversation_id,no_memory,created_at,updated_at) VALUES(?,?,1,?,?,?,?,?,?)",
        params![id,format!("{label} · {}",parent.0),context,parent.1,input.conversation_id,parent.2,now,now]).map_err(|e| e.to_string())?;
    let mut ids = HashMap::new();
    for row in &rows[..cut] {
        let copy = storage::id();
        tx.execute(
            "INSERT INTO messages VALUES(?,?,?,?,?,?)",
            params![copy, id, row.1, row.2, row.3, row.4],
        )
        .map_err(|e| e.to_string())?;
        ids.insert(row.0.clone(), copy);
    }
    // Old attachments have no message link. Only those present when this request
    // was sent are eligible; never inherit files attached later in the parent.
    let mut stmt = tx.prepare("SELECT id,message_id FROM attachments WHERE conversation_id=? AND created_at<=? ORDER BY created_at,rowid").map_err(|e| e.to_string())?;
    let attachments = stmt
        .query_map(params![input.conversation_id, rows[cut].4], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    drop(stmt);
    let mut copied = 0;
    for (source, message) in attachments {
        if message
            .as_ref()
            .is_some_and(|m| m != &rows[cut].0 && !ids.contains_key(m))
        {
            continue;
        }
        let linked = message.as_ref().and_then(|m| ids.get(m));
        copied += tx.execute("INSERT INTO attachments(id,conversation_id,path,mime,sha256,size,created_at,asset_id,message_id,ephemeral,metadata_json) SELECT ?,?,path,mime,sha256,size,created_at,asset_id,?,ephemeral,metadata_json FROM attachments WHERE id=?",
            params![storage::id(),id,linked,source]).map_err(|e| e.to_string())?;
    }
    // Persist the unsent request as a recoverable draft, not a fabricated sent message.
    tx.execute(
        "INSERT INTO settings(key,json_value) VALUES(?,?)",
        params![format!("chat_draft.{id}"), json!(text).to_string()],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(
        json!({"id":id,"text":text,"attachments":copied,"parent_conversation_id":input.conversation_id}),
    )
}

pub fn draft(store: &Store, id: &str) -> Result<Value, String> {
    let db = store.0.lock().map_err(|_| "DB_LOCK")?;
    let raw: Option<String> = db
        .query_row(
            "SELECT json_value FROM settings WHERE key=?",
            [format!("chat_draft.{id}")],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    raw.map(|v| serde_json::from_str(&v).map_err(|e| e.to_string()))
        .unwrap_or(Ok(Value::Null))
}

#[cfg(test)]
#[path = "chat_history_tests.rs"]
mod tests;
