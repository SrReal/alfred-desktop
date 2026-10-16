//! Optional app-owned bindings open macro review; they never execute unreviewed input.
use crate::{runtime::Core, storage};
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

fn parsed(text: &str) -> Result<Shortcut, String> {
    let shortcut: Shortcut = text.parse().map_err(|_| "MACRO_SHORTCUT_INVALID")?;
    let reserved = [
        "Ctrl+Alt+Shift+Space",
        crate::explain::TEXT_SHORTCUT,
        crate::explain::REGION_SHORTCUT,
    ];
    if reserved
        .iter()
        .any(|s| s.parse::<Shortcut>().is_ok_and(|r| shortcut.id() == r.id()))
    {
        return Err("MACRO_SHORTCUT_RESERVED".into());
    }
    // Explicit modifier chords avoid swallowing ordinary typing. The parser
    // remains the authority for supported key names and modifier syntax.
    if !text
        .split('+')
        .any(|s| ["Ctrl", "Control", "Alt", "Super", "Cmd"].contains(&s))
        || text.split('+').count() < 2
    {
        return Err("MACRO_SHORTCUT_MODIFIER_REQUIRED".into());
    }
    Ok(shortcut)
}
pub(crate) fn validate(text: &str) -> Result<(), String> {
    parsed(text).map(|_| ())
}
pub(crate) fn registered(c: &Core, id: &str) -> Result<bool, String> {
    Ok(c.macro_shortcuts
        .lock()
        .map_err(|_| "MACRO_SHORTCUT_LOCK")?
        .contains_key(id))
}
pub(crate) fn unregister(c: &Core, id: &str) -> Result<(), String> {
    let text = c
        .macro_shortcuts
        .lock()
        .map_err(|_| "MACRO_SHORTCUT_LOCK")?
        .get(id)
        .cloned();
    if let Some(text) = text {
        let app = c
            .app
            .lock()
            .map_err(|_| "APP_LOCK")?
            .clone()
            .ok_or("APP_UNAVAILABLE")?;
        app.global_shortcut()
            .unregister(text.as_str())
            .map_err(|_| "MACRO_SHORTCUT_UNREGISTER_FAILED")?;
        c.macro_shortcuts
            .lock()
            .map_err(|_| "MACRO_SHORTCUT_LOCK")?
            .remove(id);
    }
    Ok(())
}
pub(crate) fn register(c: &Arc<Core>, id: &str, text: &str) -> Result<bool, String> {
    let shortcut = parsed(text)?;
    {
        let bindings = c
            .macro_shortcuts
            .lock()
            .map_err(|_| "MACRO_SHORTCUT_LOCK")?;
        if let Some(owned) = bindings.get(id) {
            return if owned == text {
                Ok(true)
            } else {
                Err("MACRO_SHORTCUT_CONFLICT".into())
            };
        }
        if bindings.len() >= 64 {
            return Err("MACRO_SHORTCUT_LIMIT".into());
        }
    }
    let Some(app) = c.app.lock().map_err(|_| "APP_LOCK")?.clone() else {
        return Ok(false);
    };
    if app.global_shortcut().is_registered(shortcut) {
        return Err("MACRO_SHORTCUT_CONFLICT".into());
    }
    let owner = Arc::downgrade(c);
    let id = id.to_owned();
    let callback_id = id.clone();
    let down = Mutex::new(false);
    let last = Mutex::new(None::<Instant>);
    app.global_shortcut()
        .on_shortcut(shortcut, move |_, _, event| {
            let Ok(mut down) = down.lock() else {
                return;
            };
            if event.state() == ShortcutState::Released {
                *down = false;
                return;
            }
            if *down {
                return;
            }
            *down = true;
            drop(down);
            let Ok(mut last) = last.lock() else {
                return;
            };
            if last.is_some_and(|t| t.elapsed() < Duration::from_millis(500)) {
                return;
            }
            *last = Some(Instant::now());
            drop(last);
            let Some(core) = owner.upgrade() else {
                return;
            };
            if !core.enabled("macros") || core.store.setting("privacy") == true {
                return;
            }
            let id = callback_id.clone();
            tauri::async_runtime::spawn(async move {
                if let Ok(rows) = core
                    .store
                    .query("SELECT enabled FROM macros WHERE id=?", &[json!(id)])
                {
                    if rows[0]["enabled"] == 1
                        && core.enabled("macros")
                        && core.store.setting("privacy") != true
                        && core
                            .store
                            .set_setting(
                                "macro_shortcut_pending",
                                &json!({"id":id,"at":storage::now()}),
                            )
                            .is_ok()
                        && crate::show_surface(&core, "command_palette").await.is_ok()
                    {
                        core.emit("alfredpet:macro_shortcut", &json!({"id":id}));
                    }
                }
            });
        })
        .map_err(|_| "MACRO_SHORTCUT_CONFLICT")?;
    c.macro_shortcuts
        .lock()
        .map_err(|_| "MACRO_SHORTCUT_LOCK")?
        .insert(id, text.into());
    Ok(true)
}
pub(crate) fn restore(c: &Arc<Core>) -> Result<Value, String> {
    if !c.enabled("macros") {
        return Ok(json!({"registered":0,"errors":[]}));
    }
    let rows = c
        .store
        .query("SELECT id,definition_json FROM macros WHERE enabled=1", &[])?;
    let mut errors = Vec::new();
    let mut count = 0;
    for row in rows.as_array().ok_or("MACRO_ROWS")? {
        let parsed: Result<Value, _> =
            serde_json::from_str(row["definition_json"].as_str().ok_or("MACRO_DEFINITION")?);
        if let Ok(definition) = parsed {
            if let Some(text) = definition["shortcut"].as_str() {
                let id = row["id"].as_str().ok_or("MACRO_ID")?;
                match register(c, id, text) {
                    Ok(true) => count += 1,
                    Ok(false) => (),
                    Err(error) => errors.push(json!({"id":id,"error":error})),
                }
            }
        }
    }
    Ok(json!({"registered":count,"errors":errors}))
}
pub(crate) fn stop_all(c: &Core) -> Result<(), String> {
    let ids = c
        .macro_shortcuts
        .lock()
        .map_err(|_| "MACRO_SHORTCUT_LOCK")?
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    for id in ids {
        unregister(c, &id)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reserved_and_typing_bindings_cannot_be_registered() {
        assert_eq!(
            validate("Ctrl+Alt+Shift+Space").unwrap_err(),
            "MACRO_SHORTCUT_RESERVED"
        );
        assert_eq!(
            validate("KeyA").unwrap_err(),
            "MACRO_SHORTCUT_MODIFIER_REQUIRED"
        );
        assert!(validate("Ctrl+Alt+U").is_err());
        assert!(validate("Ctrl+Alt+Shift+U").is_err());
        assert!(validate("Ctrl+Alt+KeyM").is_ok());
        assert!(validate("nonsense++garbage").is_err());
    }
}
