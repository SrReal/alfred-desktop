use crate::{policy, process, runtime::Core, storage};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path, sync::Arc};
fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v[key]
        .as_str()
        .ok_or_else(|| format!("INPUT_REQUIRED: {key}"))
}
pub(crate) fn hash(path: &Path) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|_| "FILE_READ")?;
    let mut hasher = Sha256::new();
    let mut b = [0u8; 65536];
    loop {
        let n = f.read(&mut b).map_err(|_| "FILE_READ")?;
        if n == 0 {
            break;
        }
        hasher.update(&b[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
pub(crate) fn bounded_read(path: &Path) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|_| "FILE_READ")?;
    if file.metadata().map_err(|_| "FILE_METADATA")?.len() > 32 * 1024 * 1024 {
        return Err("FILE_SIZE_LIMIT".into());
    }
    let mut bytes = Vec::new();
    file.take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "FILE_READ")?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("FILE_SIZE_LIMIT".into());
    }
    Ok(bytes)
}
pub(crate) fn move_no_replace(source: &Path, destination: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::{core::PCWSTR, Win32::Storage::FileSystem::MoveFileW};
        let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        // Unlike std::fs::rename/MoveFileEx(REPLACE_EXISTING), this fails if the
        // destination appears between our preview and the actual operation.
        unsafe { MoveFileW(PCWSTR(source.as_ptr()), PCWSTR(destination.as_ptr())) }
            .map_err(|_| "MOVE_FAILED_NO_REPLACE".into())
    }
    #[cfg(not(windows))]
    {
        let _ = (source, destination);
        Err("WINDOWS_11_REQUIRED".into())
    }
}
pub async fn dispatch(c: &Arc<Core>, tool: &str, input: &Value) -> Result<Value, String> {
    if ["windows.audio_get", "windows.audio_set"].contains(&tool) {
        return crate::audio::dispatch(c, tool, input).await;
    }
    let (module, action) = tool.split_once('.').ok_or("TOOL_ID")?;
    match module {
        "files" => {
            let cancellation = tokio_util::sync::CancellationToken::new();
            struct CancelOnDrop(tokio_util::sync::CancellationToken);
            impl Drop for CancelOnDrop {
                fn drop(&mut self) {
                    self.0.cancel();
                }
            }
            let _guard = CancelOnDrop(cancellation.clone());
            let core = c.clone();
            let action = action.to_owned();
            let input = input.clone();
            let control = crate::file_tree::Control::new(cancellation);
            tokio::task::spawn_blocking(move || {
                let _serial = core.file_actions.lock().map_err(|_| "FILES_LOCK")?;
                control.check(&core)?;
                match action.as_str() {
                    "watch" => crate::monitoring::create(
                        &core,
                        "files",
                        input["kind"].as_str().unwrap_or("folder"),
                        &input,
                    ),
                    "organize" => crate::organize::dispatch_control(&core, &input, &control),
                    "watch_update" => crate::monitoring::owned(&core, "files", "update", &input),
                    "unwatch" => crate::monitoring::owned(&core, "files", "delete", &input),
                    _ => files(&core, &action, &input, &control),
                }
            })
            .await
            .map_err(|_| "FILES_WORKER")?
        }
        "gui" => crate::gui::dispatch(c, tool, input).await,
        "window" => Box::pin(crate::window_layouts::dispatch(c, action, input)).await,
        "clipboard" => crate::clipboard::dispatch(c, tool, input).await,
        "printers" => Err("PRINTER_PREPARED_CORE_REQUIRED".into()),
        "windows" => {
            if let Some(p) = input["path"].as_str() {
                if module == "printers" {
                    c.path(p, false)?;
                }
            }
            let v = process::native(&c.native, tool, input).await?;
            Ok(v)
        }
        "vision" => {
            if action == "ocr" {
                c.path(string(input, "path")?, false)?;
                process::native(&c.native, tool, input).await
            } else {
                crate::assets::dispatch(c, tool, input).await
            }
        }
        "browser" => {
            if crate::browser_research::TOOLS.contains(&tool) {
                return crate::browser_research::dispatch(c, tool, input).await;
            }
            if action == "screenshot" {
                return crate::assets::capture(c, tool, input).await;
            }
            let mut args = input.clone();
            if action == "open" {
                validate_url(string(input, "url")?)?;
            }
            if action == "upload" {
                args["path"] = json!(c.path(string(input, "path")?, false)?);
            }
            if action == "download" {
                args["output"] =
                    json!(c
                        .root
                        .join("temp")
                        .join(format!("{}.{}", storage::id(), "download")));
            }
            c.browser(tool, &args).await
        }
        "terminal" => {
            if action == "cancel" {
                crate::terminal::cancel(c, input)
            } else {
                crate::terminal::profiles(c, action, input)
            }
        }
        "downloads" => {
            if action == "watch_update" || action == "unwatch" {
                return crate::monitoring::owned(
                    c,
                    "downloads",
                    if action == "unwatch" {
                        "delete"
                    } else {
                        "update"
                    },
                    input,
                );
            }
            if action == "watch" {
                return crate::monitoring::create(c, "downloads", "download", input);
            }
            if action == "list" {
                return files(c, "list", input, &crate::file_tree::Control::default());
            }
            let path = c.path(string(input, "path")?, false)?;
            match action {
                "hash" => Ok(json!({"sha256":hash(&path)?})),
                "classify" => Ok(
                    json!({"extension":path.extension().and_then(|s|s.to_str()),"size":std::fs::metadata(path).map_err(|_|"FILE_METADATA")?.len(),"auto_execute":false}),
                ),
                "wait" => {
                    let mut prev = 0;
                    for _ in 0..20 {
                        let len = std::fs::metadata(&path)
                            .map_err(|_| "DOWNLOAD_MISSING")?
                            .len();
                        if len > 0
                            && len == prev
                            && !path
                                .extension()
                                .is_some_and(|e| e == "tmp" || e == "crdownload" || e == "part")
                        {
                            return Ok(json!({"path":path,"size":len}));
                        }
                        prev = len;
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                    Err("DOWNLOAD_TIMEOUT".into())
                }
                _ => Err("DOWNLOAD_ACTION".into()),
            }
        }
        "git" => Err("GIT_REVIEW_REQUIRED".into()),
        "vscode" => Err("VSCODE_REVIEW_REQUIRED".into()),
        "database" => Err("DATABASE_PREPARED_DISPATCH_REQUIRED".into()),
        "notion" => Err("NOTION_PREPARED_DISPATCH_REQUIRED".into()),
        "drive" | "homeassistant" | "rest" | "mcp" => {
            crate::integrations::dispatch(c, module, action, input).await
        }
        "images" | "documents" => Err("MEDIA_PREPARED_CORE_REQUIRED".into()),
        "tasks" => crate::tasks::dispatch(c, action, input),
        "monitoring" => {
            if action == "check" {
                crate::monitoring::poll(c).await
            } else {
                crate::monitoring::dispatch(c, action, input)
            }
        }
        "macros" => crate::macros::dispatch(c, action, input).await,
        "custom" | "plugin_builder" => Err("PLUGIN_PREPARED_CORE_REQUIRED".into()),
        _ => Err("MODULE_UNKNOWN".into()),
    }
}
fn validate_url(raw: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(raw).map_err(|_| "URL_INVALID")?;
    if !["https", "http"].contains(&url.scheme())
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("URL_SCHEME_OR_CREDENTIALS_DENIED".into());
    }
    Ok(())
}
fn files(
    c: &Core,
    action: &str,
    v: &Value,
    control: &crate::file_tree::Control,
) -> Result<Value, String> {
    if action == "undo" {
        return crate::undo::execute_request(c, string(v, "id")?, v["revalidate"] == true, control);
    }
    if action == "archive_status" {
        return crate::file_tree::archive_status(
            c,
            string(v, "id")?,
            v["offset"].as_u64().unwrap_or(0),
        );
    }
    if action == "rollback" {
        return crate::undo::rollback(c, &v["body"], control);
    }
    if action == "retention_preview" {
        return crate::file_tree::retention(c, control);
    }
    if action == "cleanup" {
        return crate::file_tree::cleanup(c, &v["body"], control);
    }
    if action == "cleanup_plans" {
        return c.store.query("SELECT id,state,next_index,error,created_at,updated_at,json_type(snapshot_json,'$.maintenance_policy') IS NOT NULL AS maintenance FROM file_cleanup_plans ORDER BY created_at DESC LIMIT 100",&[]);
    }
    if action == "undo_history" {
        return crate::undo::history(c);
    }
    let path = c.path(string(v, "path")?, action == "write")?;
    match action {
        "list" | "find" | "duplicates" => {
            let mut rows = Vec::new();
            let mut stack = vec![path.clone()];
            let mut hashes = std::collections::HashMap::<String, Vec<String>>::new();
            while let Some(dir) = stack.pop() {
                for entry in std::fs::read_dir(dir)
                    .map_err(|_| "DIRECTORY_READ")?
                    .take(5000)
                {
                    let e = entry.map_err(|_| "DIRECTORY_ENTRY")?;
                    let p = e.path();
                    if c.path(p.to_string_lossy().as_ref(), false).is_err() {
                        continue;
                    }
                    let meta = e.metadata().map_err(|_| "FILE_METADATA")?;
                    if action != "list" && meta.is_dir() && rows.len() < 5000 {
                        stack.push(p.clone());
                    }
                    if action == "duplicates" && meta.is_file() {
                        hashes
                            .entry(hash(&p)?)
                            .or_default()
                            .push(p.to_string_lossy().to_string());
                    }
                    if action != "find"
                        || p.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_lowercase()
                            .contains(&v["query"].as_str().unwrap_or("").to_lowercase())
                    {
                        rows.push(json!({"path":p,"directory":meta.is_dir(),"size":meta.len()}));
                    }
                    if rows.len() >= 5000 {
                        break;
                    }
                }
                if rows.len() >= 5000 {
                    break;
                }
            }
            if action == "duplicates" {
                Ok(json!(hashes
                    .into_iter()
                    .filter(|(_, p)| p.len() > 1)
                    .map(|(h, p)| json!({"sha256":h,"paths":p}))
                    .collect::<Vec<_>>()))
            } else {
                Ok(json!(rows))
            }
        }
        "read" => {
            let bytes = bounded_read(&path)?;
            let text = String::from_utf8(bytes).map_err(|_| "FILE_NOT_UTF8")?;
            if policy::contains_secret(&text) {
                return Err("FILE_CONTAINS_SECRET".into());
            }
            Ok(json!({"text":text}))
        }
        "write" => crate::file_tree::write(c, &path, string(v, "text")?, control),
        "copy" | "move" | "rename" => {
            let dest = c.path(string(v, "destination")?, true)?;
            crate::file_tree::mutate(c, action, &path, Some(&dest), control)
        }
        "delete" => crate::file_tree::mutate(c, action, &path, None, control),
        "compare" => {
            let dest = c.path(string(v, "destination")?, false)?;
            Ok(
                json!({"equal":hash(&path)?==hash(&dest)?,"left_sha256":hash(&path)?,"right_sha256":hash(&dest)?}),
            )
        }
        "compress" | "extract" => {
            let dest = c.path(string(v, "destination")?, true)?;
            crate::file_tree::archive(c, action, &path, &dest, control)
        }
        _ => Err("FILE_ACTION_UNKNOWN".into()),
    }
}
pub async fn attach(c: &Arc<Core>, v: &Value) -> Result<Value, String> {
    crate::assets::attach(c, v).await
}
pub async fn scheduler(c: Arc<Core>) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        match crate::tasks::claim_due(&c, chrono::Utc::now()) {
            Ok(tasks) => {
                for task in tasks {
                    if !c.enabled("tasks")
                        || c.store.setting("privacy") == true
                        || c.store.setting("dnd") == true
                    {
                        break;
                    }
                    c.emit(
                        "alfredpet:attention",
                        &json!({"event":"reminder","task":task}),
                    );
                    c.attention("waiting_user", false).await;
                }
            }
            Err(error) => tracing::warn!(error_code = %error, "Task reminder polling failed"),
        }
        if let Err(error) = crate::monitoring::poll(&c).await {
            tracing::warn!(error_code = %error, "Watch polling failed");
        }
    }
}
