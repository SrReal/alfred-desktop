//! Explicit chat dictation only. Ephemeral text, no audio files, no model tool.
use crate::{process, runtime::Core, storage};
use serde_json::{json, Value};
use std::{process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    process::Command,
};
use tokio_util::sync::CancellationToken;

pub struct Session {
    id: String,
    conversation: String,
    status: String,
    text: String,
    error: String,
    cancel: CancellationToken,
    stop: CancellationToken,
}
fn snapshot(s: &Session) -> Value {
    json!({"id":s.id,"conversation_id":s.conversation,"status":s.status,"text":s.text,"error":s.error})
}
fn emit(c: &Core) {
    c.emit("alfredpet:dictation", &json!({}));
}
fn allowed(c: &Core) -> Result<(), String> {
    if c.store.setting("privacy") == true {
        Err("PRIVACY_DENIED".into())
    } else {
        Ok(())
    }
}
pub fn clear(c: &Core) -> Result<(), String> {
    let changed = if let Some(s) = c.dictation.lock().map_err(|_| "DICTATION_LOCK")?.take() {
        s.cancel.cancel();
        true
    } else {
        false
    };
    if changed {
        emit(c);
    }
    Ok(())
}
pub fn state(c: &Core) -> Result<Value, String> {
    if allowed(c).is_err() {
        clear(c)?;
    }
    Ok(c.dictation
        .lock()
        .map_err(|_| "DICTATION_LOCK")?
        .as_ref()
        .map(snapshot)
        .unwrap_or(Value::Null))
}
pub fn start(c: &Arc<Core>, input: &Value) -> Result<Value, String> {
    allowed(c)?;
    let conversation = input["conversation_id"].as_str().ok_or("CONVERSATION_ID")?;
    if !conversation.is_empty()
        && c.store.query(
            "SELECT id FROM conversations WHERE id=?",
            &[json!(conversation)],
        )?[0]
            .is_null()
    {
        return Err("CONVERSATION_NOT_FOUND".into());
    }
    if c.runs
        .lock()
        .map_err(|_| "RUN_LOCK")?
        .contains_key(conversation)
    {
        return Err("CONVERSATION_BUSY".into());
    }
    let session = Session {
        id: storage::id(),
        conversation: conversation.into(),
        status: "starting".into(),
        text: String::new(),
        error: String::new(),
        cancel: CancellationToken::new(),
        stop: CancellationToken::new(),
    };
    let value = snapshot(&session);
    let (id, cancel, stop) = (
        session.id.clone(),
        session.cancel.clone(),
        session.stop.clone(),
    );
    {
        let mut current = c.dictation.lock().map_err(|_| "DICTATION_LOCK")?;
        allowed(c)?;
        if current.is_some() {
            return Err("DICTATION_PENDING".into());
        }
        *current = Some(session);
    }
    emit(c);
    let core = c.clone();
    tokio::spawn(async move {
        let _lease = core.dictation_worker.lock().await;
        if cancel.is_cancelled() {
            return;
        }
        let result = worker(&core, &id, cancel, stop).await;
        finish(&core, &id, result.err());
    });
    Ok(value)
}
pub fn action(c: &Core, input: &Value) -> Result<Value, String> {
    allowed(c)?;
    let mut current = c.dictation.lock().map_err(|_| "DICTATION_LOCK")?;
    let s = current.as_mut().ok_or("DICTATION_CHANGED")?;
    if input["id"] != s.id {
        return Err("DICTATION_CHANGED".into());
    }
    let result = match input["action"].as_str() {
        Some("stop") => {
            if ["starting", "listening", "stopping"].contains(&s.status.as_str()) {
                s.status = "stopping".into();
                s.stop.cancel();
            }
            snapshot(s)
        }
        Some("discard") => {
            s.cancel.cancel();
            *current = None;
            Value::Null
        }
        Some("use") => {
            if s.status != "review" {
                return Err("DICTATION_NOT_READY".into());
            }
            if input["conversation_id"] != s.conversation {
                return Err("DICTATION_CONVERSATION_CHANGED".into());
            }
            let text = input["text"].as_str().ok_or("DICTATION_TEXT")?;
            valid_text(text)?;
            let result = json!({"text":text});
            *current = None;
            result
        }
        _ => return Err("DICTATION_ACTION".into()),
    };
    drop(current);
    emit(c);
    Ok(result)
}
fn valid_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty() || text.len() > 48000 || text.contains('\0') {
        Err("DICTATION_TEXT_LIMIT".into())
    } else {
        Ok(())
    }
}
fn event(c: &Core, id: &str, data: &Value) -> Result<(), String> {
    allowed(c)?;
    let mut current = c.dictation.lock().map_err(|_| "DICTATION_LOCK")?;
    let s = current
        .as_mut()
        .filter(|s| s.id == id)
        .ok_or("DICTATION_CANCELLED")?;
    match data["type"].as_str() {
        Some("ready") => {
            if s.status == "starting" {
                s.status = "listening".into();
            }
        }
        Some("text") => {
            let text = data["text"].as_str().ok_or("DICTATION_PROTOCOL")?;
            let combined = [s.text.as_str(), text]
                .into_iter()
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            valid_text(&combined)?;
            s.text = combined;
        }
        Some("error") => {
            return Err(match data["code"].as_str() {
                Some("DICTATION_SPANISH_REQUIRED") => "DICTATION_SPANISH_REQUIRED",
                Some("DICTATION_MICROPHONE_UNAVAILABLE") => "DICTATION_MICROPHONE_UNAVAILABLE",
                Some("DICTATION_TEXT_LIMIT") => "DICTATION_TEXT_LIMIT",
                _ => "DICTATION_RECOGNITION_FAILED",
            }
            .into())
        }
        Some("complete") => {}
        _ => return Err("DICTATION_PROTOCOL".into()),
    }
    drop(current);
    emit(c);
    Ok(())
}
fn finish(c: &Core, id: &str, error: Option<String>) {
    if let Ok(mut current) = c.dictation.lock() {
        if let Some(s) = current.as_mut().filter(|s| s.id == id) {
            s.error = error.unwrap_or_else(|| {
                if s.text.trim().is_empty() {
                    "DICTATION_NO_SPEECH".into()
                } else {
                    String::new()
                }
            });
            s.status = if !s.text.trim().is_empty() {
                "review"
            } else {
                "error"
            }
            .into();
        }
    }
    emit(c);
}
async fn worker(
    c: &Core,
    id: &str,
    cancel: CancellationToken,
    stop: CancellationToken,
) -> Result<(), String> {
    allowed(c)?;
    worker_command(c, id, cancel, stop, native_command(c, "listen")?).await
}
fn native_command(c: &Core, mode: &str) -> Result<Command, String> {
    if !["info", "listen"].contains(&mode) {
        return Err("DICTATION_MODE".into());
    }
    let installed = c
        .resource
        .join("sidecars/dictation/AlfredPet.Dictation.exe");
    let executable = if installed.is_file() {
        installed
    } else {
        c.resource
            .join(".work/dictation-host/AlfredPet.Dictation.exe")
    };
    if !executable.is_file() {
        return Err("DICTATION_ENGINE_UNAVAILABLE".into());
    }
    let mut cmd = Command::new(dunce::simplified(&executable));
    cmd.arg(mode);
    Ok(cmd)
}
// Read-only readiness probe: enumerate engines without a shell or audio input.
pub async fn info(c: &Core) -> Result<Value, String> {
    let output = process::output(native_command(c, "info")?, None, 30).await?;
    let value: Value = serde_json::from_str(&output).map_err(|_| "DICTATION_PROTOCOL")?;
    if !value["available"].is_boolean() {
        return Err("DICTATION_ENGINE_UNAVAILABLE".into());
    }
    Ok(value)
}
async fn worker_command(
    c: &Core,
    id: &str,
    cancel: CancellationToken,
    stop: CancellationToken,
    mut cmd: Command,
) -> Result<(), String> {
    process::sanitize(&mut cmd);
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "DICTATION_ENGINE_UNAVAILABLE")?;
    #[cfg(windows)]
    let _job = crate::windows_native::ProcessJob::attach(&child)?;
    let mut input = child.stdin.take().ok_or("PROCESS_STDIN")?;
    let mut output = BufReader::new(child.stdout.take().ok_or("PROCESS_STDOUT")?);
    let deadline = tokio::time::sleep(Duration::from_secs(140));
    tokio::pin!(deadline);
    let mut stop_sent = false;
    let mut complete = false;
    let operation=async {
        loop {
            // Keep the partially read line alive when Stop arrives mid-frame.
            let reading=process::bounded_line(&mut output,64000);
            tokio::pin!(reading);
            let line=loop {
            tokio::select! {
                biased;
                _=cancel.cancelled()=>return Err("DICTATION_CANCELLED".into()),
                _=&mut deadline=>return Err("DICTATION_TIMEOUT".into()),
                _=stop.cancelled(),if !stop_sent=> {
                    input.write_all(b"stop\n").await.map_err(|_|"DICTATION_STOP_FAILED")?;
                    stop_sent=true;
                    deadline.as_mut().reset(tokio::time::Instant::now()+Duration::from_secs(10));
                },
                line=&mut reading=>break line?,
            }
            };
            let Some(line)=line else {break;};
            let data:Value=serde_json::from_str(line.trim_start_matches('\u{feff}')).map_err(|_|"DICTATION_PROTOCOL")?;
            event(c,id,&data)?;
            if data["type"]=="complete" {complete=true;break;}
        }
        if complete {Ok(())} else {Err("DICTATION_RECOGNITION_FAILED".into())}
    }.await;
    // Await termination before showing Review or starting another microphone session.
    let _ = child.kill().await;
    operation
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Arc<Core> {
        Arc::new(
            Core::new(
                std::env::temp_dir().join(format!("ap-dictation-{}", storage::id())),
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        )
    }
    fn seed(c: &Core, status: &str) -> (CancellationToken, CancellationToken) {
        let cancel = CancellationToken::new();
        let stop = CancellationToken::new();
        *c.dictation.lock().unwrap() = Some(Session {
            id: "test".into(),
            conversation: "chat".into(),
            status: status.into(),
            text: String::new(),
            error: String::new(),
            cancel: cancel.clone(),
            stop: stop.clone(),
        });
        (cancel, stop)
    }
    fn clean(c: Arc<Core>) {
        let path = c.root.clone();
        drop(c);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[cfg(windows)]
    #[tokio::test]
    async fn readiness_loads_helper_from_canonical_resource_path_without_audio_input() {
        let mut c = fixture();
        let resource = std::fs::canonicalize(&c.resource).unwrap();
        Arc::get_mut(&mut c).unwrap().resource = resource;
        assert!(native_command(&c, "unknown").is_err());
        let result = info(&c).await.unwrap();
        assert!(result["available"].is_boolean());
        assert_eq!(result["maxSeconds"], 120);
        clean(c);
    }
    #[test]
    fn review_requires_stop_same_session_same_chat_and_explicit_use_without_messages() {
        let c = fixture();
        seed(&c, "listening");
        event(&c, "test", &json!({"type":"text","text":"Hola Alfred"})).unwrap();
        let input =
            json!({"id":"test","action":"use","conversation_id":"chat","text":"Hola, Alfred."});
        assert_eq!(action(&c, &input).unwrap_err(), "DICTATION_NOT_READY");
        action(&c, &json!({"id":"test","action":"stop"})).unwrap();
        finish(&c, "test", None);
        assert!(action(&c, &json!({"id":"old","action":"use"})).is_err());
        assert!(action(
            &c,
            &json!({"id":"test","action":"use","conversation_id":"other","text":"hola"})
        )
        .is_err());
        assert_eq!(action(&c, &input).unwrap()["text"], "Hola, Alfred.");
        assert!(state(&c).unwrap().is_null());
        assert_eq!(
            c.store
                .query("SELECT count(*) AS n FROM messages", &[])
                .unwrap()[0]["n"],
            0
        );
        assert_eq!(
            c.store
                .query("SELECT count(*) AS n FROM conversations", &[])
                .unwrap()[0]["n"],
            0
        );
        clean(c);
    }
    #[test]
    fn privacy_erases_text_cancels_worker_and_rejects_late_events_and_start() {
        let c = fixture();
        let (cancel, _) = seed(&c, "listening");
        event(&c, "test", &json!({"type":"text","text":"privado"})).unwrap();
        c.store.set_setting("privacy", &json!(true)).unwrap();
        assert!(state(&c).unwrap().is_null());
        assert!(cancel.is_cancelled());
        assert!(event(&c, "test", &json!({"type":"text","text":"late"})).is_err());
        assert_eq!(
            start(&c, &json!({"conversation_id":""})).unwrap_err(),
            "PRIVACY_DENIED"
        );
        clean(c);
    }
    #[test]
    fn discarding_stale_completion_cannot_replace_a_new_session_and_text_is_bounded() {
        let c = fixture();
        let (cancel, _) = seed(&c, "starting");
        action(&c, &json!({"id":"test","action":"discard"})).unwrap();
        assert!(cancel.is_cancelled());
        seed(&c, "starting");
        c.dictation.lock().unwrap().as_mut().unwrap().id = "next".into();
        finish(&c, "test", None);
        assert_eq!(state(&c).unwrap()["status"], "starting");
        assert!(valid_text(" ").is_err());
        assert!(valid_text(&"á".repeat(24001)).is_err());
        assert!(valid_text("a\0b").is_err());
        finish(&c, "next", None);
        assert_eq!(state(&c).unwrap()["error"], "DICTATION_NO_SPEECH");
        clean(c);
    }
    #[cfg(windows)]
    #[tokio::test]
    async fn worker_stop_flushes_final_text_and_cancel_terminates_owned_process_without_audio() {
        let c = fixture();
        let script = c.root.join("synthetic-dictation.mjs");
        std::fs::write(&script,r#"import {createInterface} from 'node:readline';
console.log(JSON.stringify({type:'ready'}));
for await (const line of createInterface({input:process.stdin})) {
 if(line==='stop') { console.log(JSON.stringify({type:'text',text:'Texto final'})); console.log(JSON.stringify({type:'complete'})); break; }
}"#).unwrap();
        let cmd = || {
            let mut v = Command::new(c.resource.join(".work/runtime/node.exe"));
            v.arg(&script);
            v
        };
        let (cancel, stop) = seed(&c, "starting");
        stop.cancel();
        worker_command(&c, "test", cancel, stop, cmd())
            .await
            .unwrap();
        finish(&c, "test", None);
        assert_eq!(state(&c).unwrap()["text"], "Texto final");
        assert_eq!(state(&c).unwrap()["status"], "review");
        let (cancel, stop) = seed(&c, "starting");
        cancel.cancel();
        assert_eq!(
            worker_command(&c, "test", cancel, stop, cmd())
                .await
                .unwrap_err(),
            "DICTATION_CANCELLED"
        );
        clear(&c).unwrap();
        clean(c);
    }
}
