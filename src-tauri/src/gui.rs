use crate::{policy, process, runtime::Core, storage};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
#[cfg(test)]
#[path = "gui_tests.rs"]
mod tests;

struct Run {
    core: Arc<Core>,
    id: String,
}
impl Drop for Run {
    fn drop(&mut self) {
        if let Ok(mut runs) = self.core.runs.lock() {
            runs.remove(&self.id);
        }
    }
}
pub fn cancel_active(c: &Core, input_only: bool) -> Result<(), String> {
    for (id, job) in c.runs.lock().map_err(|_| "RUN_LOCK")?.iter() {
        if id.starts_with("gui-")
            && (!input_only
                || ["click", "hover", "drag", "send_keys"]
                    .iter()
                    .any(|action| id.starts_with(&format!("gui-{action}-"))))
        {
            job.cancel();
        }
    }
    Ok(())
}
pub async fn dispatch(c: &Arc<Core>, tool: &str, input: &Value) -> Result<Value, String> {
    if c.store.setting("privacy") == true {
        return Err("PRIVACY_DENIED".into());
    }
    if !c.enabled("gui") {
        return Err("TOOL_DISABLED_OR_UNKNOWN".into());
    }
    let action = tool.split_once('.').ok_or("TOOL_ID")?.1;
    if action == "screenshot_target" {
        return crate::assets::capture(c, tool, input).await;
    }
    let fallback = c.store.setting("gui_fallback_input") == true;
    if ["send_keys", "hover", "drag"].contains(&action) && !fallback {
        return Err("SENDINPUT_DISABLED: opt-in required".into());
    }
    let id = format!("gui-{action}-{}", storage::id());
    let cancel = CancellationToken::new();
    c.runs
        .lock()
        .map_err(|_| "RUN_LOCK")?
        .insert(id.clone(), cancel.clone());
    let _run = Run {
        core: c.clone(),
        id,
    };
    let mut lease = if ["click", "hover", "drag", "send_keys"].contains(&action) {
        Some(
            tokio::select! {_=cancel.cancelled()=>return Err("GUI_CANCELLED".into()),lease=c.gui_input.clone().lock_owned()=>lease},
        )
    } else {
        None
    };
    if c.store.setting("privacy") == true {
        return Err("PRIVACY_DENIED".into());
    }
    if !c.enabled("gui") {
        return Err("TOOL_DISABLED_OR_UNKNOWN".into());
    }
    let fallback = c.store.setting("gui_fallback_input") == true;
    if ["send_keys", "hover", "drag"].contains(&action) && !fallback {
        return Err("SENDINPUT_DISABLED: opt-in required".into());
    }
    if cancel.is_cancelled() {
        return Err("GUI_CANCELLED".into());
    }
    let mut args = input.clone();
    args["_fallback_enabled"] = json!(fallback);
    #[cfg(windows)]
    let _mouse_guard = if ["click", "hover", "drag"].contains(&action) && fallback {
        let guard = crate::gui_guard::Guard::new(lease.take())?;
        args["_input_tag"] = json!(guard.tag());
        Some(guard)
    } else {
        None
    };
    let result = process::native_cancellable(&c.native, tool, &args, cancel.clone())
        .await
        .map_err(|error| {
            if error == "PROCESS_CANCELLED" {
                "GUI_CANCELLED".into()
            } else {
                error
            }
        })?;
    if cancel.is_cancelled() {
        return Err("GUI_CANCELLED".into());
    }
    if c.store.setting("privacy") == true {
        return Err("PRIVACY_DENIED".into());
    }
    if !c.enabled("gui") {
        return Err("TOOL_DISABLED_OR_UNKNOWN".into());
    }
    if policy::contains_secret(&result.to_string()) {
        return Err("SENSITIVE_GUI_CONTENT_DENIED".into());
    }
    Ok(result)
}
