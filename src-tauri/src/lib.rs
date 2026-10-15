pub mod admin;
mod asset_files;
mod assets;
mod audio;
mod audio_file;
mod audit_history;
#[cfg(all(test, windows))]
mod browser_forms_tests;
mod browser_research;
mod catalog;
mod chat_history;
mod chat_links;
mod chat_progress;
mod chat_stream;
mod claude;
mod claude_model;
mod clipboard;
mod clipboard_native;
mod conversations;
mod database;
mod database_sql;
mod diagnostics;
mod dictation;
mod download_post;
mod drive;
mod drive_auth;
mod drive_credentials;
mod drive_local;
mod drive_upload;
mod edition;
mod explain;
mod file_tree;
mod geometry;
mod git_local;
mod github;
mod gui;
mod gui_guard;
mod homeassistant;
mod http_client;
mod integrations;
mod macro_shortcuts;
mod macros;
mod manual;
mod mcp;
mod mcp_auth;
mod mcp_client;
mod mcp_credentials;
mod mcp_native;
mod mcp_oauth;
mod mcp_profile;
mod mcp_program_hash;
mod mcp_request_guard;
mod mcp_save;
mod mcp_subscription_wire;
mod media;
mod memory;
mod modules;
mod monitoring;
mod notion;
mod organize;
mod path_picker;
mod persona;
mod pet_drag;
mod pet_drop;
mod pet_sprites;
mod pet_topmost;
mod pet_visual;
mod plugin_secrets;
mod plugins;
mod policy;
mod printers;
mod process;
mod profile_backup;
mod remembered_permissions;
mod rest;
mod rest_credentials;
mod rest_openapi;
mod rest_profile;
mod rest_save;
mod routines;
mod runtime;
mod search;
mod semantic;
mod semantic_store;
mod skills;
mod storage;
mod surface_state;
mod system_audit;
mod tasks;
mod terminal;
mod undo;
mod vscode;
mod watch_extra;
mod window_layouts;
mod window_snapshot;
mod window_undo;
mod window_worker;
pub fn window_worker_entry() -> i32 {
    window_worker::entry()
}
mod windows_native;
use claude::{ClaudeCliTransport, ClaudeTransport};
use runtime::Core;
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc};
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

const SURFACES: &[&str] = &[
    "pet",
    "chat",
    "bubble",
    "command_palette",
    "history",
    "settings",
    "activity",
    "permission",
    "capture_overlay",
    "task_center",
];
fn surface_title(label: &str) -> &str {
    match label {
        "chat" => "Chat",
        "history" => "Historial",
        "settings" => "Configuración",
        "activity" => "Actividad",
        "permission" => "Permisos",
        "command_palette" => "Comandos",
        "task_center" => "Tareas",
        "capture_overlay" => "Captura",
        "bubble" => "Aviso",
        _ => "AlfredPet",
    }
}

const MAIN_SURFACES: &[&str] = &[
    "chat",
    "history",
    "command_palette",
    "task_center",
    "activity",
    "settings",
];
fn window_label(surface: &str) -> &str {
    if MAIN_SURFACES.contains(&surface) {
        "chat"
    } else {
        surface
    }
}
fn caller_surface(core: &Core, window: &tauri::WebviewWindow) -> Result<String, String> {
    if window.label() == "chat" {
        Ok(core
            .surfaces
            .lock()
            .map_err(|_| "WINDOW_STATE_LOCK")?
            .get("chat")
            .map(|s| s.view.clone())
            .unwrap_or_default())
    } else {
        Ok(window.label().into())
    }
}
pub async fn show_surface(core: &Arc<Core>, label: &str) -> Result<(), String> {
    present_surface(core, label, false, false).await
}
async fn present_surface(
    core: &Arc<Core>,
    view: &str,
    toggle: bool,
    warm: bool,
) -> Result<(), String> {
    if !SURFACES.contains(&view) {
        return Err("WINDOW_UNKNOWN".into());
    }
    let _action = core.surface_actions.lock().await;
    let app = core
        .app
        .lock()
        .map_err(|_| "APP_LOCK")?
        .as_ref()
        .cloned()
        .ok_or("APP_MISSING")?;
    if view == "pet" {
        app.get_webview_window("pet")
            .ok_or("PET_WINDOW_MISSING")?
            .show()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    let label = window_label(view);
    let (changed, ready, visible) = {
        let mut states = core.surfaces.lock().map_err(|_| "WINDOW_STATE_LOCK")?;
        let state = states.entry(label.into()).or_default();
        // A late prewarm must not replace a view the user already requested.
        if warm && !state.view.is_empty() {
            return Ok(());
        }
        let changed = state.select(view);
        if !warm {
            if toggle && !changed {
                state.toggle();
            } else {
                state.request_show();
            }
        }
        (changed, state.ready, state.visible)
    };
    if label == "chat" && (view != "chat" || !visible) {
        dictation::clear(core)?;
    }
    if let Some(w) = app.get_webview_window(label) {
        if changed {
            w.hide().map_err(|e| e.to_string())?;
            w.set_title(&format!("AlfredPet · {}", surface_title(view)))
                .map_err(|e| e.to_string())?;
            w.emit_to(
                tauri::EventTarget::WebviewWindow {
                    label: label.into(),
                },
                "alfredpet:navigate",
                &json!({"surface":view}),
            )
            .map_err(|e| e.to_string())?;
        } else if !visible {
            w.hide().map_err(|e| e.to_string())?;
        } else if ready {
            reveal_surface(core, &w, view).await?;
        }
        return Ok(());
    }
    let (width, height) = match view {
        "chat" => (720., 620.),
        "command_palette" => (460., 420.),
        "bubble" => (300., 100.),
        "permission" => (540., 460.),
        "capture_overlay" => (900., 600.),
        _ => (760., 660.),
    };
    let builder = WebviewWindowBuilder::new(
        &app,
        label,
        WebviewUrl::App(format!("index.html?surface={view}").into()),
    )
    .title(format!("AlfredPet · {}", surface_title(view)))
    .inner_size(width, height)
    .theme(Some(tauri::Theme::Dark))
    .focused(false)
    .decorations(view != "capture_overlay")
    .transparent(view == "capture_overlay")
    .background_color(tauri::window::Color(
        24,
        25,
        28,
        if view == "capture_overlay" { 0 } else { 255 },
    ))
    .visible(false);
    // WebView2 must deliver File objects to DOM drop handlers in the main shell.
    // The pet retains the native handler and its existing approval queue.
    let builder = if label == "chat" {
        builder.disable_drag_drop_handler()
    } else {
        builder
    };
    let w = builder.build().map_err(|e| e.to_string())?;
    if view == "bubble" {
        w.set_always_on_top(true).map_err(|e| e.to_string())?;
        w.set_skip_taskbar(true).map_err(|e| e.to_string())?;
    }
    Ok(())
}
async fn surface_ready(
    core: &Arc<Core>,
    window: &tauri::WebviewWindow,
    view: &str,
) -> Result<(), String> {
    if window.label() == "pet" {
        pet_visual::apply(window)?;
        let core = core.clone();
        tauri::async_runtime::spawn(async move {
            if present_surface(&core, "chat", false, true).await.is_err() {
                tracing::warn!(code = "CHAT_PREWARM_FAILED");
            }
        });
        return Ok(());
    }
    let _action = core.surface_actions.lock().await;
    let visible = core
        .surfaces
        .lock()
        .map_err(|_| "WINDOW_STATE_LOCK")?
        .entry(window.label().into())
        .or_default()
        .rendered(view);
    if visible {
        reveal_surface(core, window, view).await?;
    }
    Ok(())
}
async fn reveal_surface(
    core: &Arc<Core>,
    window: &tauri::WebviewWindow,
    view: &str,
) -> Result<(), String> {
    position_surface(core, window, view).await?;
    window.show().map_err(|e| e.to_string())?;
    if view != "bubble" {
        window.set_focus().map_err(|e| e.to_string())?;
    }
    Ok(())
}
async fn hide_surface(core: &Arc<Core>, view: &str) -> Result<(), String> {
    if window_label(view) == "chat" {
        dictation::clear(core)?;
    }
    let _action = core.surface_actions.lock().await;
    let label = window_label(view);
    core.surfaces
        .lock()
        .map_err(|_| "WINDOW_STATE_LOCK")?
        .entry(label.into())
        .or_default()
        .hide();
    let app = core
        .app
        .lock()
        .map_err(|_| "APP_LOCK")?
        .as_ref()
        .cloned()
        .ok_or("APP_MISSING")?;
    if let Some(window) = app.get_webview_window(label) {
        window.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}
async fn position_surface(
    core: &Arc<Core>,
    window: &tauri::WebviewWindow,
    view: &str,
) -> Result<(), String> {
    let Some(pet) = window.app_handle().get_webview_window("pet") else {
        return Ok(());
    };
    let pos = pet.outer_position().map_err(|_| "PET_POSITION")?;
    let size = pet.outer_size().map_err(|_| "PET_SIZE")?;
    let pos = if view == "capture_overlay" {
        core.explain_region_origin
            .lock()
            .map_err(|_| "EXPLAIN_LOCK")?
            .map(|(x, y)| tauri::PhysicalPosition::new(x, y))
            .unwrap_or(pos)
    } else {
        pos
    };
    let monitors = core.monitors().await?;
    if let Some(m) = monitors
        .iter()
        .find(|m| {
            (pos.x as f64) >= m.work.x
                && (pos.x as f64) < m.work.x + m.work.width
                && (pos.y as f64) >= m.work.y
                && (pos.y as f64) < m.work.y + m.work.height
        })
        .or(monitors.first())
    {
        // Windows preserves logical size on WM_DPICHANGED. Move to the target
        // monitor before measuring its frame and applying the physical size.
        if (window.scale_factor().map_err(|_| "WINDOW_SCALE")? - m.scale).abs() > 0.001 {
            let current = window.outer_size().map_err(|_| "WINDOW_SIZE")?;
            window
                .set_position(tauri::PhysicalPosition::new(
                    (m.work.x + (m.work.width - current.width as f64) / 2.) as i32,
                    (m.work.y + (m.work.height - current.height as f64) / 2.) as i32,
                ))
                .map_err(|_| "WINDOW_POSITION")?;
        }
        let outer = window.outer_size().map_err(|_| "WINDOW_SIZE")?;
        let inner = window.inner_size().map_err(|_| "WINDOW_SIZE")?;
        let frame_x = outer.width.saturating_sub(inner.width) as f64;
        let frame_y = outer.height.saturating_sub(inner.height) as f64;
        let (w, h) = match view {
            "chat" => (720., 620.),
            "command_palette" => (460., 420.),
            "bubble" => (300., 100.),
            "permission" => (540., 460.),
            "capture_overlay" => (m.work.width / m.scale, m.work.height / m.scale),
            _ => (760., 660.),
        };
        window
            .set_size(tauri::PhysicalSize::new(
                (w * m.scale).min((m.work.width - frame_x).max(1.)) as u32,
                (h * m.scale).min((m.work.height - frame_y).max(1.)) as u32,
            ))
            .map_err(|_| "WINDOW_SIZE")?;
        let actual = window.outer_size().map_err(|_| "WINDOW_SIZE")?;
        let rect = if view == "chat" || view == "bubble" {
            geometry::anchor_sized(
                geometry::Rect {
                    x: pos.x as f64,
                    y: pos.y as f64,
                    width: size.width as f64,
                    height: size.height as f64,
                },
                m,
                actual.width as f64,
                actual.height as f64,
            )
        } else {
            geometry::Rect {
                x: m.work.x + (m.work.width - actual.width as f64).max(0.) / 2.,
                y: m.work.y + (m.work.height - actual.height as f64).max(0.) / 2.,
                width: actual.width as f64,
                height: actual.height as f64,
            }
        };
        window
            .set_position(tauri::PhysicalPosition::new(rect.x as i32, rect.y as i32))
            .map_err(|_| "WINDOW_POSITION")?;
    }
    Ok(())
}

#[tauri::command]
async fn request(
    window: tauri::WebviewWindow,
    core: tauri::State<'_, Arc<Core>>,
    operation: String,
    input: Value,
) -> Result<Value, String> {
    let c = core.inner();
    match operation.as_str() {
        "window.ready" => {
            surface_ready(c, &window, input["surface"].as_str().ok_or("SURFACE_REQUIRED")?).await?;
            Ok(json!({"surface":caller_surface(c,&window)?}))
        }
        "window.toggle" => {
            if window.label() != "pet" || input["surface"] != "chat" {
                return Err("PET_CHAT_TOGGLE_ONLY".into());
            }
            present_surface(c, "chat", true, false).await?;
            Ok(json!({}))
        }
        "pet.drag" => {
            if window.label() != "pet" { return Err("PET_WINDOW_REQUIRED".into()); }
            pet_drag::finish(&window).await?;
            Ok(json!({}))
        }
        "pet.frame" => {
            pet_visual::set_frame(&window,
                input["state"].as_str().ok_or("PET_STATE_REQUIRED")?,
                input["frame"].as_u64().ok_or("PET_FRAME_REQUIRED")? as usize)?;
            Ok(json!({}))
        }
        "pet.menu" => {
            if window.label() != "pet" {
                return Err("PET_WINDOW_REQUIRED".into());
            }
            let menu = c
                .pet_menu
                .lock()
                .map_err(|_| "PET_MENU_LOCK")?
                .clone()
                .ok_or("PET_MENU_MISSING")?;
            window.popup_menu(&menu).map_err(|e| e.to_string())?;
            Ok(json!({}))
        }
        "window.show" => {
            show_surface(c, input["surface"].as_str().ok_or("SURFACE_REQUIRED")?).await?;
            Ok(json!({}))
        }
        "manual.open" => {
            if caller_surface(c, &window)? != "settings" { return Err("SETTINGS_WINDOW_REQUIRED".into()); }
            if !input.as_object().is_some_and(|fields| fields.is_empty()) { return Err("MANUAL_NO_ARGUMENTS".into()); }
            manual::open(c).await?;
            Ok(json!({"opened":true}))
        }
        "modules.list" => Ok(c.module_list()),
        "attention.last" => Ok(c.store.setting("last_attention")),
        "pet.restore" => {
            let saved = c.store.setting("pet_position");
            let monitors = c.monitors().await?;
            let m = monitors
                .iter()
                .find(|m| saved["monitor"] == m.id)
                .or(monitors.first())
                .ok_or("NO_MONITORS")?;
            let x = m.work.x
                + saved["x_normalized"].as_f64().unwrap_or(0.8).clamp(0., 1.) * m.work.width;
            let app = c
                .app
                .lock()
                .map_err(|_| "APP_LOCK")?
                .as_ref()
                .cloned()
                .ok_or("APP_MISSING")?;
            if let Some(pet) = app.get_webview_window("pet") {
                pet.set_position(tauri::PhysicalPosition::new(x as i32, m.work.y as i32))
                    .map_err(|_| "PET_POSITION")?;
            }
            settle(c).await
        }
        "modules.toggle" => {
            let module = input["id"].as_str().ok_or("MODULE_ID")?;
            if !catalog::MODULES.iter().any(|m| m.id == module) {
                return Err("MODULE_UNKNOWN".into());
            }
            crate::remembered_permissions::invalidate(c)?;
            let _mcp_lease = if module == "mcp" { Some(integrations::save_lock(c)?) } else { None };
            c.store.execute(
                "UPDATE module_configs SET enabled=? WHERE module_id=?",
                &[input["enabled"].clone(), json!(module)],
            )?;
            if module == "mcp" { crate::mcp::revoke(c)?; }
            if module=="skills" {crate::skills::revoke(c)?;}
            if ["custom","plugin_builder"].contains(&module) {crate::plugins::revoke(c)?;}
            if ["images","documents","printers","monitoring","browser","gui","files","downloads","macros"].contains(&module) {crate::media::revoke(c)?;}
            if module == "macros" {
                crate::macros::revoke(c)?;
                let _lease=c.macro_actions.lock().await;
                if input["enabled"]==false {crate::macro_shortcuts::stop_all(c)?;} else {
                    let summary=crate::macro_shortcuts::restore(c)?;c.store.set_setting("macro_shortcut_startup",&summary)?;
                }
            }
            if module == "files" { crate::file_tree::cancel_maintenance(c)?; }
            if ["vision", "gui", "browser", "clipboard"].contains(&module) && input["enabled"] == false { crate::assets::cancel_module(c,module)?; }
            if module == "browser" && input["enabled"] == false {
                *c.browser.lock().await = None;
            }
            if module == "gui" && input["enabled"] == false {
                crate::explain::clear(c)?;
                gui::cancel_active(c, false)?;
            }
            c.emit("alfredpet:modules", &json!({}));
            Ok(json!({}))
        }
        "custom.secret_set" => {if caller_surface(c,&window)?!="settings"{return Err("SETTINGS_WINDOW_REQUIRED".into())}crate::plugin_secrets::save(c,input).await},
        "tools.list" => Ok(json!(c.tools())),
        "tools.execute" => {
            c.execute(
                input["tool"].as_str().ok_or("TOOL_REQUIRED")?,
                input["input"].clone(),
            )
            .await
        }
        "watches.list" => crate::monitoring::dispatch(c,"list",&input),
        "paths.pick" => {
            if !["settings", "task_center", "command_palette", "chat"].contains(&caller_surface(c, &window)?.as_str()) {
                return Err("PATH_PICKER_SURFACE_REQUIRED".into());
            }
            let options = path_picker::Options::parse(input)?;
            Ok(json!(path_picker::pick(&window, options).await?))
        }
        "settings.pick_audio" => {
            if caller_surface(c, &window)? != "settings" { return Err("SETTINGS_WINDOW_REQUIRED".into()); }
            Ok(json!(audio_file::pick(&window).await?))
        }
        "settings.get" => c.store.query("SELECT key,json_value FROM settings", &[]),
        "clipboard.status" => clipboard::status(c),
        "settings.set" => {
            let key = input["key"].as_str().ok_or("SETTING_KEY")?;
            if key=="clipboard_history" {
                let result=clipboard::set_history(c,input["value"].as_bool().ok_or("CLIPBOARD_HISTORY_ENABLED_TYPE")?)?;
                c.emit("alfredpet:settings",&json!({}));return Ok(result);
            }
            if ![
                "privacy",
                "dnd",
                "allowed_roots",
                "attention_audio",
                "claude_model",
                "always_on_top",
                "clipboard_history",
                "shortcut",
                "pet_position",
                "no_memory",
                "semantic_memory",
                "gui_fallback_input",
                "file_checkpoint_days",
                "file_maintenance",
                "asset_lifecycle",
            ]
            .contains(&key)
            {
                return Err("SETTING_UNKNOWN".into());
            }
            if key == "always_on_top" {
                let top=input["value"].as_bool().ok_or("ALWAYS_ON_TOP_TYPE")?;
                if let Some(pet)=window.app_handle().get_webview_window("pet") { pet.set_always_on_top(top).map_err(|e|e.to_string())?; }
            }
            if key == "claude_model" { crate::claude_model::ClaudeModel::parse(&input["value"])?; }
            if key == "semantic_memory" {
                return semantic::set_enabled(c,input["value"].as_bool().ok_or("SEMANTIC_ENABLED_TYPE")?).await;
            }
            if key == "file_checkpoint_days" { crate::file_tree::validate_retention(&input["value"])?; }
            if key == "file_maintenance" { crate::file_tree::validate_maintenance(&input["value"])?; }
            if key=="asset_lifecycle" { crate::assets::validate_config(&input["value"])?; }
            let value=if key=="allowed_roots" {policy::normalized_roots(&input["value"])?}else{input["value"].clone()};
            let _mcp_lease = if key == "privacy" { Some(integrations::save_lock(c)?) } else { None };
            if key=="allowed_roots" {crate::remembered_permissions::invalidate(c)?;}
            c.store.set_setting(key, &value)?;
            if key == "privacy" { crate::skills::revoke(c)?; crate::mcp::revoke(c)?; crate::macros::revoke(c)?; }
            if ["privacy","allowed_roots"].contains(&key){crate::plugins::revoke(c)?;crate::media::revoke(c)?;}
            if ["allowed_roots","asset_lifecycle"].contains(&key) { crate::assets::cancel(c)?; }
            if ["file_maintenance","file_checkpoint_days","allowed_roots","privacy","dnd"].contains(&key) { crate::file_tree::cancel_maintenance(c)?; }
            if ["privacy","no_memory"].contains(&key) {
                semantic::cancel(c)?;
                *c.semantic_host.lock().await=None;
                if input["value"]==false {semantic::schedule(c);}
            }
            if ["privacy","no_memory"].contains(&key) && input["value"]==true {
                for job in c.memory_jobs.lock().map_err(|_|"MEMORY_JOB_LOCK")?.values(){job.cancel();}
            }
            if key=="privacy" && input["value"]==true {
                crate::dictation::clear(c)?;
                crate::explain::clear(c)?;
                gui::cancel_active(c, false)?;
                crate::assets::cancel(c)?;
            }
            if key == "gui_fallback_input" && input["value"] == false {
                gui::cancel_active(c, true)?;
            }
            c.emit("alfredpet:settings", &json!({}));
            Ok(json!({}))
        }
        "conversations.list" => c
            .store
            .query("SELECT * FROM conversations ORDER BY updated_at DESC", &[]),
        "conversations.active" => Ok(c.store.setting("active_conversation")),
        "conversations.open" => {
            c.store.set_setting("active_conversation", &input["id"])?;
            show_surface(c, "chat").await?;
            c.emit(
                "alfredpet:conversation_selected",
                &json!({"id":input["id"]}),
            );
            Ok(json!({}))
        }
        "conversations.create" => {
            let id = storage::id();
            c.store.execute(
                "INSERT INTO conversations(id,title,created_at,updated_at,project_id) VALUES(?,?,?,?,?)",
                &[
                    json!(id),
                    json!("Nueva conversación"),
                    json!(storage::now()),
                    json!(storage::now()),
                    input["project_id"].clone(),
                ],
            )?;
            Ok(json!({"id":id}))
        }
        "conversations.messages" => c.store.query(
            "SELECT * FROM messages WHERE conversation_id=? ORDER BY created_at,rowid",
            &[input["id"].clone()],
        ),
        "conversations.delete" => {
            if !["chat", "history"].contains(&caller_surface(c, &window)?.as_str()) { return Err("CHAT_WINDOW_REQUIRED".into()); }
            conversations::delete(c, &input)
        }
        "chat.progress" => {
            if !["chat", "history"].contains(&caller_surface(c, &window)?.as_str()) { return Err("CHAT_WINDOW_REQUIRED".into()); }
            chat_progress::list(c, input["id"].as_str().ok_or("CONVERSATION_ID")?)
        }
        "chat.state" => Ok(json!({"running":c.runs.lock().map_err(|_|"RUN_LOCK")?.contains_key(input["id"].as_str().unwrap_or(""))})),
        "conversations.update" => {
            c.store.execute("UPDATE conversations SET title=?,title_user_edited=1,favorite=?,no_memory=?,project_id=? WHERE id=?",&[input["title"].clone(),input["favorite"].clone(),input["no_memory"].clone(),input["project_id"].clone(),input["id"].clone()])?;
            if input["no_memory"]==1 || input["no_memory"]==true {
                if let Some(job)=c.memory_jobs.lock().map_err(|_|"MEMORY_JOB_LOCK")?.get(input["id"].as_str().unwrap_or("")){job.cancel();}
            }
            semantic::schedule(c);
            Ok(json!({}))
        }
        "chat.draft" => {
            if caller_surface(c, &window)? != "chat" { return Err("CHAT_WINDOW_REQUIRED".into()); }
            chat_history::draft(&c.store, input["id"].as_str().ok_or("CONVERSATION_ID")?)
        }
        "conversations.branch" => {
            if caller_surface(c, &window)? != "chat" { return Err("CHAT_WINDOW_REQUIRED".into()); }
            let revision: chat_history::Revision = serde_json::from_value(input).map_err(|_| "MESSAGE_ACTION_INVALID")?;
            let runs = c.runs.lock().map_err(|_| "RUN_LOCK")?;
            if runs.contains_key(&revision.conversation_id) { return Err("CONVERSATION_BUSY".into()); }
            let result = chat_history::branch(&c.store, revision)?;
            drop(runs);
            c.emit("alfredpet:conversations", &json!({}));
            Ok(result)
        }
        "conversations.fork" => {
            let parent = c.store.query(
                "SELECT * FROM conversations WHERE id=?",
                &[input["id"].clone()],
            )?;
            let id = storage::id();
            c.store.execute("INSERT INTO conversations(id,title,summary,parent_conversation_id,created_at,updated_at) VALUES(?,?,?,?,?,?)",&[json!(id),json!(format!("Desde {}",parent[0]["title"].as_str().unwrap_or("conversación"))),parent[0]["summary"].clone(),input["id"].clone(),json!(storage::now()),json!(storage::now())])?;
            Ok(json!({"id":id}))
        }
        "chat.open_link" => {
            if !["chat", "history"].contains(&caller_surface(c, &window)?.as_str()) { return Err("CHAT_WINDOW_REQUIRED".into()); }
            chat_links::open(&input).await?;
            Ok(json!({}))
        }
        "dictation.info" | "dictation.state" | "dictation.start" | "dictation.action" => {
            if window.label() != "chat" { return Err("CHAT_WINDOW_REQUIRED".into()); }
            if (operation == "dictation.start" || input["action"] == "use")
                && (caller_surface(c, &window)? != "chat" || !window.is_visible().unwrap_or(false)) {
                return Err("CHAT_WINDOW_REQUIRED".into());
            }
            match operation.as_str() {
                "dictation.info" => dictation::info(c).await,
                "dictation.state" => dictation::state(c),
                "dictation.start" => dictation::start(c, &input),
                _ => dictation::action(c, &input),
            }
        }
        "explain.pending" | "explain.prepare" | "explain.region" => {
            if caller_surface(c, &window)? != "chat" { return Err("CHAT_WINDOW_REQUIRED".into()); }
            match operation.as_str() {
                "explain.pending" => explain::pending(c),
                "explain.prepare" => explain::prepare(c, &input).await,
                _ => { explain::region(c).await?; Ok(json!({})) }
            }
        }
        "explain.asset" => {
            if window.label() != "capture_overlay" { return Err("CAPTURE_WINDOW_REQUIRED".into()); }
            explain::asset(c, input["id"].as_str().ok_or("ASSET_ID")?).await
        }
        "chat.send" => ClaudeCliTransport.start(c, input).await,
        "chat.interrupt" => {
            if let Some(run) = c
                .runs
                .lock()
                .map_err(|_| "RUN_LOCK")?
                .get(input["run_id"].as_str().unwrap_or(""))
            {
                run.cancel();
            }
            Ok(json!({}))
        }
        "claude.health" => ClaudeCliTransport.health().await,
        "permissions.list" => c.permissions_list(),
        "permissions.rules" | "permissions.revoke" => {
            if caller_surface(c, &window)? != "settings" { return Err("SETTINGS_WINDOW_REQUIRED".into()); }
            if operation == "permissions.rules" { crate::remembered_permissions::list(c) }
            else { crate::remembered_permissions::revoke(c, input["id"].as_str().ok_or("PERMISSION_RULE_ID")?) }
        }
        "questions.list" => c
            .store
            .query("SELECT * FROM questions WHERE status='pending'", &[]),
        "questions.answer" => {
            if window.label() != "permission" {
                return Err("PERMISSION_WINDOW_REQUIRED".into());
            }
            let id = input["id"].as_str().ok_or("QUESTION_ID")?;
            let answer = input["answer"].as_str().ok_or("QUESTION_ANSWER")?;
            if answer.is_empty() || answer.len() > 8000 {
                return Err("QUESTION_ANSWER_LENGTH".into());
            }
            let tx = c
                .pending
                .lock()
                .map_err(|_| "QUESTION_LOCK")?
                .remove(id)
                .ok_or("QUESTION_EXPIRED")?;
            tx.send(answer.into()).map_err(|_| "QUESTION_EXPIRED")?;
            Ok(json!({}))
        }
        "permissions.resolve" => {
            if window.label() != "permission" {
                return Err("PERMISSION_WINDOW_REQUIRED".into());
            }
            let id = input["id"].as_str().ok_or("PERMISSION_ID")?;
            let decision = input["decision"].as_str().ok_or("PERMISSION_DECISION")?;
            if !["allow", "deny", "session", "always"].contains(&decision) {
                return Err("PERMISSION_DECISION_INVALID".into());
            }
            if decision == "always" && !c.permissions_list()?.as_array().is_some_and(|rows| rows.iter().any(|row| row["id"]==id && row["rememberable"]==true)) {
                return Err("PERMISSION_NOT_REMEMBERABLE".into());
            }
            let tx = c
                .pending
                .lock()
                .map_err(|_| "POLICY_LOCK")?
                .remove(id)
                .ok_or("PERMISSION_EXPIRED")?;
            if c.pending.lock().map_err(|_| "POLICY_LOCK")?.is_empty() {
                window.hide().map_err(|_| "PERMISSION_WINDOW_HIDE")?;
                if let Some(bubble) = window.app_handle().get_webview_window("bubble") {
                    let _ = bubble.hide();
                }
            }
            tx.send(decision.into()).map_err(|_| "PERMISSION_EXPIRED")?;
            Ok(json!({}))
        }
        "memory.list" => c
            .store
            .query("SELECT * FROM memories ORDER BY updated_at DESC", &[]),
        "memory.save" => {let result=c.store.remember(&input)?;semantic::schedule(c);Ok(result)},
        "semantic.status" => semantic::status(c),
        "semantic.download" => semantic::start(c,"download",input["consent"]==true),
        "semantic.rebuild" => semantic::start(c,"rebuild",false),
        "semantic.cancel" => {semantic::cancel(c)?;*c.semantic_host.lock().await=None;Ok(json!({}))},
        "semantic.query" => semantic::retrieve(c,input["query"].as_str().ok_or("SEMANTIC_QUERY")?,input["project_id"].as_str()).await,
        "memory.candidates" => crate::memory::candidates(c),
        "memory.candidate_accept" => {let result=crate::memory::accept(c,input["id"].as_str().ok_or("MEMORY_CANDIDATE_ID")?,false)?;semantic::schedule(c);Ok(result)},
        "memory.candidate_reject" => crate::memory::reject(c,input["id"].as_str().ok_or("MEMORY_CANDIDATE_ID")?),
        "memory.extract" => crate::memory::start(c,input["conversation_id"].as_str().ok_or("CONVERSATION_ID")?),
        "memory.cancel_extraction" => crate::memory::cancel(c,input["conversation_id"].as_str().ok_or("CONVERSATION_ID")?),
        "memory.analyses" => c.store.query("SELECT a.*,c.title FROM conversation_analysis a JOIN conversations c ON a.conversation_id=c.id ORDER BY a.updated_at DESC LIMIT 100",&[]),
        "memory.rebuild_index" => {let result=c.store.rebuild_search()?;semantic::schedule(c);Ok(result)},
        "memory.export" => {
            let memories=c.store.query("SELECT * FROM memories ORDER BY created_at", &[])?;
            let sources=c.store.query("SELECT * FROM memory_sources", &[])?;
            let path=c.root.join("temp").join(format!("memories-{}.json",storage::id()));
            std::fs::write(&path,serde_json::to_vec_pretty(&json!({"version":1,"memories":memories,"sources":sources})).map_err(|_|"MEMORY_EXPORT_JSON")?).map_err(|_|"MEMORY_EXPORT_WRITE")?;
            Ok(json!({"path":path}))
        }
        "memory.delete" => {
            c.store
                .execute("DELETE FROM memories WHERE id=?", &[input["id"].clone()])?;
            semantic::schedule(c);
            Ok(json!({}))
        }
        "memory.toggle" => {
            c.store.execute(
                "UPDATE memories SET pinned=?,disabled=? WHERE id=?",
                &[
                    input["pinned"].clone(),
                    input["disabled"].clone(),
                    input["id"].clone(),
                ],
            )?;
            semantic::schedule(c);
            Ok(json!({}))
        }
        "activity.list" => c.store.query(
            "SELECT a.*,u.state AS undo_state FROM activity_events a LEFT JOIN undo_records u ON a.undo_token=u.id ORDER BY a.timestamp DESC LIMIT 500",
            &[],
        ),
        "search" => crate::search::search(c,input["query"].as_str().unwrap_or("")).await,
        "projects.list" => c.store.query("SELECT * FROM projects", &[]),
        "macros.list" => crate::macros::list(c),
        "routines.list" | "routines.status" | "routines.save" | "routines.delete" | "routines.run" => {
            if caller_surface(c,&window)? != "command_palette" {return Err("ROUTINE_PALETTE_REQUIRED".into());}
            if operation == "routines.run" { routines::run(c,&input).await }
            else { routines::dispatch(c,&operation,&input) }
        },
        "macros.shortcut_pending" => {
            if caller_surface(c,&window)? != "command_palette" {return Err("MACRO_PALETTE_REQUIRED".into());}
            crate::macros::consume_shortcut(c)
        },
        "projects.save" => {
            let raw = input["root_path"].as_str().ok_or("PROJECT_PATH")?;
            let path=policy::normalized_root(raw,false)?;
            if !std::path::Path::new(&path).is_dir() {
                return Err("PROJECT_PATH_INVALID".into());
            }
            let id = storage::id();
            c.store.execute(
                "INSERT INTO projects(id,name,root_path,created_at) VALUES(?,?,?,?)",
                &[
                    json!(id),
                    input["name"].clone(),
                    json!(path),
                    json!(storage::now()),
                ],
            )?;
            Ok(json!({"id":id}))
        }
        "connections.list" => c.store.query(
            "SELECT id,module_id,name,config_json,health FROM connections",
            &[],
        ),
        "connections.save" => integrations::save(c, &input).await,
        "connections.oauth" => drive_auth::start(c, &input).await,
        "connections.oauth_status" => drive_auth::status(c, &input),
        "connections.oauth_cancel" => drive_auth::cancel(c, &input),
        "connections.mcp_oauth" => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct PrivateStart {input:Value,client_secret:Option<String>}
            let value:PrivateStart=serde_json::from_value(input).map_err(|_|"MCP_OAUTH_PRIVATE_INPUT")?;
            let secret=value.client_secret.map(|s|crate::mcp_auth::Text::new(s,16384)).transpose()?;
            c.execute_mcp_oauth(value.input,secret).await
        },
        "connections.mcp_oauth_status" => crate::mcp_auth::service::status(c,&input),
        "pet.settle" => settle(c).await,
        "monitors.list" => Ok(json!(c.monitors().await?)),
        "pet_drop.list" => crate::pet_drop::list(c),
        "pet_drop.attach" | "pet_drop.dismiss" => {
            if caller_surface(c,&window)? != "chat" { return Err("CHAT_WINDOW_REQUIRED".into()); }
            crate::pet_drop::dispatch(c,&operation,&input).await
        },
        "assets.get" => crate::assets::ui(c,"get",&input).await,
        "assets.delete" => crate::assets::ui(c,"delete",&input).await,
        "attachments.import" => {
            if caller_surface(c,&window)? != "chat" { return Err("CHAT_WINDOW_REQUIRED".into()); }
            assets::import_attachment(c,&input).await
        },
        "attachments.add" => modules::attach(c, &input).await,
        "attachments.list" => crate::assets::attachments(c,input["conversation_id"].as_str().ok_or("ATTACHMENT_CONVERSATION")?),
        "attachments.remove" => {
            let expected=crate::assets::attachment_review(c,&input)?;
            if let Err(error)=c.permission("attachments.remove",&expected,policy::Risk::Mandatory).await {
                c.store.audit("attachments.remove","Mandatory",&expected,"denied",&json!({"error":error}),0)?;
                c.emit("alfredpet:activity",&json!({}));
                return Err(error);
            }
            let result=crate::assets::remove_attachment(c,&input,&expected).await;
            c.store.audit("attachments.remove","Mandatory",&expected,if result.is_ok(){"succeeded"}else{"failed"},&result.clone().unwrap_or_else(|error|json!({"error":error})),0)?;
            c.emit("alfredpet:activity",&json!({}));
            result
        },
        "diagnostics.export" => Box::pin(crate::diagnostics::export(c)).await,
        "system_audit.list" | "system_audit.get" | "system_audit.compare" | "system_audit.delete" => {
            if caller_surface(c,&window)? != "settings" { return Err("SETTINGS_WINDOW_REQUIRED".into()); }
            audit_history::dispatch(&c.store,&operation,&input)
        },
        "profile.status" | "profile.preview_backup" | "profile.preview_restore" | "profile.schedule" => {
            if caller_surface(c,&window)? != "settings" { return Err("SETTINGS_WINDOW_REQUIRED".into()); }
            profile_backup::dispatch(c,&operation,input).await
        },
        "profile.restart" => {
            if caller_surface(c,&window)? != "settings" { return Err("SETTINGS_WINDOW_REQUIRED".into()); }
            window.app_handle().restart();
        },
        "backup" => {
            let db = c.store.0.lock().map_err(|_| "DB_LOCK")?;
            let path = c
                .root
                .join("backups")
                .join(format!("alfredpet-{}.db", storage::id()));
            db.backup(rusqlite::MAIN_DB, &path, None)
                .map_err(|_| "BACKUP_FAILED")?;
            Ok(json!({"path":path}))
        }
        "app.quit" => {
            crate::mcp_auth::service::stop_all(c);
            crate::mcp::stop_listeners(c);
            window.app_handle().exit(0);
            Ok(json!({}))
        }
        _ => Err("OPERATION_UNKNOWN".into()),
    }
}
async fn settle(c: &Arc<Core>) -> Result<Value, String> {
    let app = c
        .app
        .lock()
        .map_err(|_| "APP_LOCK")?
        .as_ref()
        .cloned()
        .ok_or("APP_MISSING")?;
    let pet = app.get_webview_window("pet").ok_or("PET_MISSING")?;
    let pos = pet.outer_position().map_err(|_| "PET_POSITION")?;
    let monitors = c.monitors().await?;
    let monitor = monitors
        .iter()
        .find(|m| {
            (pos.x as f64) >= m.work.x
                && (pos.x as f64) < m.work.x + m.work.width
                && (pos.y as f64) >= m.work.y
                && (pos.y as f64) < m.work.y + m.work.height
        })
        .or(monitors.first())
        .ok_or("NO_MONITOR")?;
    let target = geometry::settle(pos.x as f64, monitor);
    pet.set_size(tauri::PhysicalSize::new(
        target.width as u32,
        target.height as u32,
    ))
    .map_err(|_| "PET_SIZE")?;
    pet_visual::apply(&pet)?;
    let actual = pet.outer_size().map_err(|_| "PET_SIZE")?;
    let target = geometry::settle_sized(
        pos.x as f64,
        monitor,
        actual.width as f64,
        actual.height as f64,
    );
    c.store.set_setting(
        "pet_position",
        &json!({"monitor":monitor.id,"x_normalized":(target.x-monitor.work.x)/monitor.work.width}),
    )?;
    Ok(json!({"x":target.x,"y":target.y,"from_y":pos.y,"scale":monitor.scale}))
}
async fn maintain_windows(c: Arc<Core>) {
    let mut timer = tokio::time::interval(std::time::Duration::from_secs(1));
    let mut signature = String::new();
    let mut previous_position = None;
    loop {
        timer.tick().await;
        let Ok(monitors) = c.monitors().await else {
            continue;
        };
        let current = serde_json::to_string(&monitors).unwrap_or_default();
        if !signature.is_empty() && signature != current {
            c.emit("alfredpet:pet_settle", &json!({"reason":"display_change"}));
        }
        signature = current;
        let app = c.app.lock().ok().and_then(|a| a.as_ref().cloned());
        let Some(app) = app else {
            continue;
        };
        let Some(pet) = app.get_webview_window("pet") else {
            continue;
        };
        let Ok(pos) = pet.outer_position() else {
            continue;
        };
        if previous_position == Some(pos) {
            continue;
        }
        previous_position = Some(pos);
        let Some(chat) = app.get_webview_window("chat") else {
            continue;
        };
        if !chat.is_visible().unwrap_or(false) {
            continue;
        }
        if caller_surface(&c, &chat).as_deref() == Ok("chat") {
            let _ = position_surface(&c, &chat, "chat").await;
        }
    }
}
pub fn run() {
    let profile_root = PathBuf::from(std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA"))
        .join(edition::PROFILE_DIRECTORY);
    let maintenance_start = profile_root.parent().is_some_and(|p| {
        p.join(edition::MAINTENANCE_DIRECTORY)
            .join("job.json")
            .exists()
    });
    let _profile_lease = match profile_backup::startup(&profile_root) {
        Ok(lease) => lease,
        Err(error) => {
            eprintln!("{error}");
            return;
        }
    };
    #[cfg(windows)]
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        let core = app.state::<Arc<Core>>().inner().clone();
                        let text: tauri_plugin_global_shortcut::Shortcut =
                            explain::TEXT_SHORTCUT.parse().expect("static shortcut");
                        let region: tauri_plugin_global_shortcut::Shortcut =
                            explain::REGION_SHORTCUT.parse().expect("static shortcut");
                        let id = shortcut.id();
                        let hwnd = explain::foreground();
                        tauri::async_runtime::spawn(async move {
                            let result = if id == text.id() {
                                explain::selection(&core, hwnd).await
                            } else if id == region.id() {
                                explain::region(&core).await
                            } else if "Ctrl+Alt+Space"
                                .parse::<tauri_plugin_global_shortcut::Shortcut>()
                                .is_ok_and(|s| s.id() == id)
                            {
                                show_surface(&core, "command_palette").await
                            } else {
                                Ok(())
                            };
                            if let Err(error) = result {
                                core.attention(&error, true).await;
                            }
                        });
                    }
                })
                .build(),
        )
        .setup(move |app| {
            let root =
                PathBuf::from(std::env::var("LOCALAPPDATA")?).join(edition::PROFILE_DIRECTORY);
            let resource = app.path().resource_dir()?;
            let c = Arc::new(Core::new(root, resource).map_err(std::io::Error::other)?);
            let startup = crate::mcp_auth::service::startup(&c).map_err(std::io::Error::other)?;
            c.store
                .set_setting("mcp.oauth.startup_summary", &startup)
                .map_err(std::io::Error::other)?;
            std::fs::create_dir_all(c.root.join("logs"))?;
            let log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(c.root.join("logs/alfredpet.jsonl"))?;
            let _ = tracing_subscriber::fmt()
                .json()
                .with_max_level(tracing::Level::INFO)
                .with_writer(std::sync::Mutex::new(log))
                .try_init();
            tracing::info!(product = "AlfredPet", version = "0.1.0", "startup");
            *c.app
                .lock()
                .map_err(|_| std::io::Error::other("APP_LOCK"))? = Some(app.handle().clone());
            app.manage(c.clone());
            app.global_shortcut().register("Ctrl+Alt+Space")?;
            for shortcut in [explain::TEXT_SHORTCUT, explain::REGION_SHORTCUT] {
                let registered = app.global_shortcut().register(shortcut).is_ok();
                c.store
                    .set_setting(&format!("shortcut.{shortcut}"), &json!(registered))
                    .map_err(std::io::Error::other)?;
            }
            let macro_core = c.clone();
            tauri::async_runtime::spawn(async move {
                let summary = crate::macro_shortcuts::restore(&macro_core)
                    .unwrap_or_else(|_| json!({"error":"MACRO_SHORTCUT_RESTORE_FAILED"}));
                let _ = macro_core
                    .store
                    .set_setting("macro_shortcut_startup", &summary);
                macro_core.emit("alfredpet:macros", &json!({}));
            });
            let menu = tauri::menu::Menu::with_items(
                app,
                &[
                    &tauri::menu::MenuItem::with_id(app, "chat", "Abrir chat", true, None::<&str>)?,
                    &tauri::menu::MenuItem::with_id(
                        app,
                        "command_palette",
                        "Comandos",
                        true,
                        None::<&str>,
                    )?,
                    &tauri::menu::MenuItem::with_id(
                        app,
                        "task_center",
                        "Tareas",
                        true,
                        None::<&str>,
                    )?,
                    &tauri::menu::MenuItem::with_id(
                        app,
                        "activity",
                        "Actividad",
                        true,
                        None::<&str>,
                    )?,
                    &tauri::menu::MenuItem::with_id(
                        app,
                        "settings",
                        "Configuración",
                        true,
                        None::<&str>,
                    )?,
                    &tauri::menu::MenuItem::with_id(
                        app,
                        "privacy",
                        "Alternar privacidad",
                        true,
                        None::<&str>,
                    )?,
                    &tauri::menu::MenuItem::with_id(
                        app,
                        "dnd",
                        "Alternar No molestar",
                        true,
                        None::<&str>,
                    )?,
                    &tauri::menu::MenuItem::with_id(
                        app,
                        "manual",
                        "Manual de uso (PDF)",
                        true,
                        None::<&str>,
                    )?,
                    &tauri::menu::MenuItem::with_id(app, "quit", "Salir", true, None::<&str>)?,
                ],
            )?;
            *c.pet_menu
                .lock()
                .map_err(|_| std::io::Error::other("PET_MENU_LOCK"))? = Some(menu.clone());
            let tray = tauri::tray::TrayIconBuilder::new()
                .icon(tauri::include_image!("./icons/tray.png"))
                .tooltip("AlfredPet")
                .menu(&menu)
                .on_menu_event(|app, event| {
                    if event.id.as_ref() == "quit" {
                        let core = app.state::<Arc<Core>>().inner().clone();
                        crate::mcp_auth::service::stop_all(&core);
                        crate::mcp::stop_listeners(&core);
                        app.exit(0);
                    } else {
                        let core = app.state::<Arc<Core>>().inner().clone();
                        let label = event.id.as_ref().to_string();
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let result = if label == "manual" {
                                let result = manual::open(&core).await;
                                if let Err(error) = &result {
                                    core.attention(error, true).await;
                                }
                                result.map(|_| json!({"opened":true}))
                            } else if ["privacy", "dnd"].contains(&label.as_str()) {
                                if let Some(window) = app.get_webview_window("pet") {
                                    let value = core.store.setting(&label) != true;
                                    Box::pin(request(
                                        window,
                                        app.state(),
                                        "settings.set".into(),
                                        json!({"key":label,"value":value}),
                                    ))
                                    .await
                                } else {
                                    Err("TRAY_PET_WINDOW_MISSING".into())
                                }
                            } else {
                                show_surface(&core, &label).await.map(|_| json!({}))
                            };
                            if result.is_err() {
                                tracing::error!(code = "TRAY_ACTION_FAILED", "tray_action_failed");
                            }
                        });
                    }
                });
            tray.build(app)?;
            profile_backup::finish_startup(&c.root).map_err(std::io::Error::other)?;
            if maintenance_start {
                let ready = c.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = show_surface(&ready, "settings").await;
                });
            }
            let background = c.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = claude::serve(background).await {
                    tracing::error!(error=%e,"bridge_start_failed");
                }
            });
            let scheduler = c.clone();
            tauri::async_runtime::spawn(async move {
                modules::scheduler(scheduler).await;
            });
            tauri::async_runtime::spawn(crate::file_tree::maintain_files(c.clone()));
            tauri::async_runtime::spawn(clipboard::maintain(c.clone()));
            let display_watcher = c.clone();
            tauri::async_runtime::spawn(pet_topmost::maintain(display_watcher.clone()));
            tauri::async_runtime::spawn(maintain_windows(display_watcher));
            let semantic_start = c.clone();
            tauri::async_runtime::spawn(async move {
                semantic::schedule(&semantic_start);
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "pet" {
                if matches!(
                    event,
                    tauri::WindowEvent::Resized(_) | tauri::WindowEvent::ScaleFactorChanged { .. }
                ) {
                    if let Some(pet) = window.app_handle().get_webview_window("pet") {
                        if pet_visual::apply(&pet).is_err() {
                            tracing::warn!(code = "PET_REGION_REFRESH_FAILED");
                        }
                    }
                }
                if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop {
                    paths,
                    position,
                }) = event
                {
                    if let Some(core) = window.try_state::<Arc<Core>>() {
                        let c = core.inner().clone();
                        let size = window.inner_size().ok();
                        let valid = size.is_some_and(|s| {
                            crate::pet_visual::hit(position.x, position.y, s.width, s.height)
                        });
                        let paths = if paths.len() <= 5 {
                            paths.clone()
                        } else {
                            Vec::new()
                        };
                        tauri::async_runtime::spawn(async move {
                            if let Err(error) =
                                crate::pet_drop::receive(&c, "pet", valid, &paths).await
                            {
                                let _ = crate::pet_drop::record_error(&c, &error);
                            }
                            let _ = show_surface(&c, "chat").await;
                        });
                    }
                }
            }
            if window.label() != "pet" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                    if let Some(core) = window.try_state::<Arc<Core>>() {
                        let core = core.inner().clone();
                        let label = window.label().to_string();
                        tauri::async_runtime::spawn(async move {
                            let _ = hide_surface(&core, &label).await;
                        });
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![request])
        .build(tauri::generate_context!())
        .expect("AlfredPet startup failed")
        .run(|app, event| {
            if matches!(
                event,
                tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
            ) {
                if let Some(core) = app.try_state::<Arc<Core>>() {
                    crate::mcp_auth::service::stop_all(&core);
                    crate::mcp::stop_listeners(&core);
                }
            }
        });
}
pub fn mcp_client() {
    claude::stdio_bridge();
}
