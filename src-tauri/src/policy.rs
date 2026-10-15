use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Free,
    Contextual,
    Mandatory,
}

pub fn classify(tool: &str, input: &Value) -> Risk {
    if crate::download_post::TOOLS.contains(&tool) {
        return crate::download_post::risk(tool);
    }
    if crate::printers::TOOLS.contains(&tool) {
        return crate::printers::risk(tool);
    }
    if crate::media::TOOLS.contains(&tool) {
        return crate::media::risk(tool);
    }
    if crate::skills::TOOLS.contains(&tool) {
        return if crate::skills::passive(tool) {
            Risk::Free
        } else {
            Risk::Mandatory
        };
    }
    if crate::plugins::TOOLS.contains(&tool) {
        return if [
            "custom.list",
            "custom.inspect",
            "custom.versions",
            "plugin_builder.guide",
        ]
        .contains(&tool)
        {
            Risk::Free
        } else {
            Risk::Mandatory
        };
    }
    if crate::macros::TOOLS.contains(&tool) {
        return if ["macros.list", "macros.inspect"].contains(&tool) {
            Risk::Free
        } else {
            Risk::Mandatory
        };
    }
    if crate::drive::TOOLS.contains(&tool) {
        return crate::drive::risk(tool, input);
    }
    if crate::notion::TOOLS.contains(&tool) {
        return if crate::notion::mutation(tool) {
            Risk::Contextual
        } else {
            Risk::Free
        };
    }
    if crate::database::TOOLS.contains(&tool) {
        return crate::database::classify(tool, input);
    }
    if crate::vscode::TOOLS.contains(&tool) {
        return if crate::vscode::mutation(tool) {
            Risk::Mandatory
        } else {
            Risk::Free
        };
    }
    if crate::github::TOOLS.contains(&tool) {
        return if crate::github::mutation(tool) {
            Risk::Mandatory
        } else {
            Risk::Free
        };
    }
    if crate::git_local::TOOLS.contains(&tool) {
        return if [
            "git.commit",
            "git.checkout",
            "git.stash",
            "git.delete_branch",
        ]
        .contains(&tool)
        {
            Risk::Mandatory
        } else {
            Risk::Free
        };
    }
    if tool.starts_with("clipboard.") {
        return match tool {
            "clipboard.set" | "clipboard.image" => Risk::Contextual,
            "clipboard.transform" if input["apply"] == true => Risk::Contextual,
            "clipboard.history_clear" | "clipboard.history_delete" => Risk::Mandatory,
            _ => Risk::Free,
        };
    }
    if ["browser.forms", "browser.extract"].contains(&tool)
        || crate::browser_research::TOOLS.contains(&tool)
    {
        return Risk::Free;
    }
    if ["browser.select", "browser.check"].contains(&tool) {
        return Risk::Mandatory;
    }
    if tool.starts_with("vision.capture_")
        || ["gui.screenshot_target", "browser.screenshot"].contains(&tool)
    {
        return Risk::Contextual;
    }
    if [
        "vision.asset_delete",
        "vision.cleanup",
        "attachments.remove",
    ]
    .contains(&tool)
    {
        return Risk::Mandatory;
    }

    if [
        "files.retention_preview",
        "files.cleanup_plans",
        "files.undo_history",
        "window.undo_history",
    ]
    .contains(&tool)
    {
        return Risk::Free;
    }
    if tool == "files.cleanup" {
        return if input["body"]["action"] == "preview" {
            Risk::Free
        } else {
            Risk::Mandatory
        };
    }
    if [
        "windows.audio_set",
        "window.layout_save",
        "window.layout_apply",
    ]
    .contains(&tool)
    {
        return Risk::Contextual;
    }
    if tool == "window.layout_delete" {
        return Risk::Mandatory;
    }
    if ["terminal.profiles", "terminal.cancel"].contains(&tool) {
        return Risk::Free;
    }
    if ["terminal.profile_save", "terminal.profile_delete"].contains(&tool) {
        return Risk::Contextual;
    }
    if tool == "files.organize" {
        return if input["body"].get("plan_id").is_some() {
            Risk::Contextual
        } else {
            Risk::Free
        };
    }
    let action = tool.rsplit('.').next().unwrap_or(tool);
    if tool.starts_with("custom.") || tool.starts_with("mcp.") || tool.starts_with("terminal.") {
        return Risk::Mandatory;
    }
    if [
        "delete",
        "unwatch",
        "undo",
        "rollback",
        "trash",
        "stop_process",
        "close_app",
        "service_control",
        "registry_set",
        "cancel_job",
        "submit",
        "merge",
        "publish",
        "shutdown",
        "execute",
        "print",
        "install",
        "enable_plugin",
    ]
    .contains(&action)
    {
        return Risk::Mandatory;
    }
    if tool == "homeassistant.call_service" {
        let domain = input["domain"].as_str().unwrap_or("");
        return if [
            "lock",
            "alarm_control_panel",
            "cover",
            "switch",
            "automation",
            "script",
        ]
        .contains(&domain)
        {
            Risk::Mandatory
        } else {
            Risk::Contextual
        };
    }
    if tool == "rest.request" {
        return if input["method"] == "GET" {
            Risk::Free
        } else {
            Risk::Mandatory
        };
    }
    if [
        "move",
        "watch",
        "watch_update",
        "move_monitor",
        "snap",
        "state",
        "focus",
        "rename",
        "copy",
        "write",
        "set",
        "set_value",
        "invoke",
        "select",
        "click",
        "hover",
        "drag",
        "send_keys",
        "resize",
        "crop",
        "convert",
        "compress",
        "extract",
        "create",
        "update",
        "upload",
        "append",
        "run",
        "checkout",
        "stash",
        "set_layout",
        "set_default",
        "type",
        "complete",
        "register",
        "save",
    ]
    .contains(&action)
    {
        Risk::Contextual
    } else {
        Risk::Free
    }
}

pub fn protected(path: &str) -> bool {
    let p = path.replace('/', "\\").to_ascii_lowercase();
    [
        "\\windows",
        "\\program files",
        "\\.ssh",
        "\\.aws",
        "\\.env",
        "\\credentials",
        "\\cookies",
        "\\user data",
        "\\login data",
        "\\alfredpet\\data",
        "\\alfredpet\\runtime",
    ]
    .iter()
    .any(|s| p.contains(s))
}

pub fn scoped_path(raw: &str, roots: &[PathBuf], write: bool) -> Result<PathBuf, String> {
    // canonicalize() emits extended Windows drive/UNC paths. Tool results and
    // persisted checkpoints must be reusable without treating the drive colon
    // inside the extended prefix as an alternate data stream.
    let normalized = if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(drive) = raw.strip_prefix(r"\\?\") {
        drive.to_string()
    } else {
        raw.to_string()
    };
    let path = Path::new(&normalized);
    if !path.is_absolute()
        || path.components().any(|c| matches!(c, Component::ParentDir))
        || normalized.contains(':') && normalized.chars().skip(2).any(|ch| ch == ':')
    {
        return Err("PATH_INVALID: absolute paths only; no traversal or alternate streams".into());
    }
    if protected(&normalized) {
        return Err("PATH_PROTECTED: secret or system location".into());
    }
    let resolved = if path.exists() {
        path.canonicalize()
    } else if write {
        path.parent()
            .ok_or("PATH_PARENT")?
            .canonicalize()
            .map(|p| p.join(path.file_name().unwrap_or_default()))
    } else {
        return Err("PATH_NOT_FOUND".into());
    }
    .map_err(|_| "PATH_RESOLVE")?;
    if resolved.to_str().is_none_or(protected) {
        return Err("PATH_PROTECTED: secret or system location".into());
    }
    if !roots
        .iter()
        .filter_map(|r| resolve_root(r).ok())
        .any(|r| resolved.starts_with(r))
    {
        return Err("PATH_SCOPE: add a project or permitted folder in Settings".into());
    }
    // Reject reparse/symlink paths even if their destination happens to be inside a permitted root.
    for ancestor in path.ancestors() {
        let meta = match std::fs::symlink_metadata(ancestor) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("PATH_METADATA".into()),
        };
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err("PATH_REPARSE_DENIED".into());
            }
        }
        if meta.file_type().is_symlink() {
            return Err("PATH_SYMLINK_DENIED".into());
        }
    }
    Ok(resolved)
}

// A permission names a path, including when its leaf has been moved/deleted.
// Resolve only that missing leaf; this never grants the containing directory
// and does not create a chain through missing ancestors.
fn resolve_root(root: &Path) -> Result<PathBuf, std::io::Error> {
    if !root.is_absolute() || root.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(std::io::Error::other("invalid permitted root"));
    }
    if root.exists() {
        root.canonicalize()
    } else {
        let parent = root
            .parent()
            .ok_or_else(|| std::io::Error::other("root parent"))?;
        let name = root
            .file_name()
            .ok_or_else(|| std::io::Error::other("root name"))?;
        Ok(parent.canonicalize()?.join(name))
    }
}

pub(crate) fn normalized_root(raw: &str, write: bool) -> Result<String, String> {
    let resolved = scoped_path(raw, &[PathBuf::from(raw)], write)?;
    let text = resolved.to_str().ok_or("PATH_ENCODING")?;
    Ok(if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        text.strip_prefix(r"\\?\").unwrap_or(text).to_owned()
    })
}

pub(crate) fn normalize_existing_root(raw: &str) -> String {
    if Path::new(raw).exists() {
        normalized_root(raw, false).unwrap_or_else(|_| raw.to_owned())
    } else {
        raw.to_owned()
    }
}

pub(crate) fn normalized_roots(value: &Value) -> Result<Value, String> {
    let roots = value.as_array().ok_or("ROOTS_ARRAY")?;
    let roots = roots
        .iter()
        .map(|root| {
            normalized_root(root.as_str().ok_or("ROOT_PATH_STRING")?, true).map(Value::String)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!(roots))
}

pub fn contains_secret(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "api_key",
        "apikey",
        "api key",
        "secret",
        "bearer ",
        "access_token",
        "refresh_token",
        "-----begin",
        "sk-ant-",
        "ghp_",
        "github_pat_",
        "eyjhb",
        "token=",
    ]
    .iter()
    .any(|s| t.contains(s))
}
pub fn redact(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        if ["png_base64", "thumbnail_data_url"].contains(&k.as_str()) {
                            json!("[VISUAL_BYTES_OMITTED]")
                        } else if contains_secret(k)
                            || k.to_lowercase().contains("token")
                            || k.to_lowercase().contains("authorization")
                        {
                            json!("[REDACTED]")
                        } else {
                            redact(v)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(redact).collect()),
        Value::String(s) if contains_secret(s) => json!("[REDACTED]"),
        _ => value.clone(),
    }
}
pub fn tool_redact(tool: &str, value: &Value) -> Value {
    if crate::system_audit::TOOLS.contains(&tool) {
        return json!({"action":value.get("action"),"content":"[SYSTEM_DIAGNOSTICS_OMITTED]"});
    }
    if crate::download_post::TOOLS.contains(&tool) {
        return json!({"id":value.get("id"),"sha256":value.get("sha256"),"auto_execute":false,"content":"[POST_ACTION_CONTENT_OMITTED]"});
    }
    if crate::printers::TOOLS.contains(&tool) {
        return json!({"path":value.get("path"),"printer":value.get("explicit_target"),"content":"[PRINT_CONTENT_OMITTED]"});
    }
    if crate::media::TOOLS.contains(&tool) {
        return json!({"path":value.get("path"),"sha256":value.get("sha256"),"conversation_id":value.get("conversation_id"),"content":"[MEDIA_CONTENT_OMITTED]"});
    }
    if tool.starts_with("custom.") || tool.starts_with("plugin_builder.") {
        return json!({"id":value.get("id"),"fingerprint":value.get("fingerprint"),"state":value.get("state"),"content":"[PLUGIN_CONTENT_OMITTED]"});
    }
    if crate::macros::TOOLS.contains(&tool) {
        // Child tools retain their own normal audit. The parent persists no resolved arguments/content.
        return json!({"id":value.get("id"),"fingerprint":value.get("fingerprint"),"complete":value.get("complete"),"content":"[MACRO_CONTENT_OMITTED]"});
    }
    fn database(value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, v)| {
                        (
                            key.clone(),
                            if [
                                "sql",
                                "parameters",
                                "rows",
                                "columns",
                                "schema",
                                "definition",
                                "column_default",
                                "data",
                            ]
                            .contains(&key.as_str())
                            {
                                json!("[DATABASE_CONTENT_OMITTED]")
                            } else {
                                database(v)
                            },
                        )
                    })
                    .collect(),
            ),
            Value::Array(v) => Value::Array(v.iter().map(database).collect()),
            _ => value.clone(),
        }
    }
    fn vscode(value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, v)| {
                        (
                            key.clone(),
                            if [
                                "items",
                                "tasks",
                                "configuredTasks",
                                "task_review",
                                "configured_tasks",
                                "arguments",
                                "command",
                                "output",
                                "stdout",
                                "stderr",
                                "environment",
                                "message",
                                "commands",
                            ]
                            .contains(&key.as_str())
                            {
                                json!("[VSCODE_CONTENT_OMITTED]")
                            } else {
                                vscode(v)
                            },
                        )
                    })
                    .collect(),
            ),
            Value::Array(values) => Value::Array(values.iter().map(vscode).collect()),
            _ => value.clone(),
        }
    }
    fn github(value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, v)| {
                        (
                            key.clone(),
                            if [
                                "items",
                                "data",
                                "review_content",
                                "title",
                                "body",
                                "issue",
                                "release",
                                "pull_request",
                                "target",
                            ]
                            .contains(&key.as_str())
                            {
                                json!("[GITHUB_CONTENT_OMITTED]")
                            } else {
                                github(v)
                            },
                        )
                    })
                    .collect(),
            ),
            Value::Array(values) => Value::Array(values.iter().map(github).collect()),
            _ => value.clone(),
        }
    }
    fn git(value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, value)| {
                        (
                            key.clone(),
                            if ["patch", "staged_patch", "message", "subject"]
                                .contains(&key.as_str())
                            {
                                json!("[GIT_CONTENT_OMITTED]")
                            } else {
                                git(value)
                            },
                        )
                    })
                    .collect(),
            ),
            Value::Array(values) => Value::Array(values.iter().map(git).collect()),
            _ => value.clone(),
        }
    }
    fn clipboard(value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, value)| {
                        (
                            key.clone(),
                            if ["text", "content", "replacement_text", "old_text"]
                                .contains(&key.as_str())
                            {
                                json!("[CLIPBOARD_CONTENT_OMITTED]")
                            } else {
                                clipboard(value)
                            },
                        )
                    })
                    .collect(),
            ),
            Value::Array(values) => Value::Array(values.iter().map(clipboard).collect()),
            _ => value.clone(),
        }
    }
    if tool.starts_with("mcp.") {
        json!({"content":"[MCP_CONTENT_OMITTED]","automatic_replay":false})
    } else if crate::rest::TOOLS.contains(&tool) {
        json!({"content":"[REST_CONTENT_OMITTED]","error":value["error"],"status":value["status"],"automatic_replay":false})
    } else if crate::drive::TOOLS.contains(&tool) {
        redact(&crate::drive::durable(value))
    } else if crate::homeassistant::TOOLS.contains(&tool) {
        json!({"content":"[HOMEASSISTANT_CONTENT_OMITTED]","error":value["error"],"physical_outcome":value["physical_outcome"],"acknowledged":value["acknowledged"]})
    } else if crate::notion::TOOLS.contains(&tool) {
        redact(&crate::notion::durable(value))
    } else if crate::database::TOOLS.contains(&tool) {
        redact(&database(value))
    } else if crate::vscode::TOOLS.contains(&tool) {
        redact(&vscode(value))
    } else if crate::github::TOOLS.contains(&tool) {
        redact(&github(value))
    } else if crate::git_local::TOOLS.contains(&tool) {
        redact(&git(value))
    } else if tool.starts_with("clipboard.") {
        redact(&clipboard(value))
    } else {
        redact(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn destructive_never_free() {
        for t in [
            "files.delete",
            "windows.registry_set",
            "terminal.command",
            "custom.run",
            "mcp.call",
            "browser.submit",
            "github.merge",
        ] {
            assert_eq!(classify(t, &json!({})), Risk::Mandatory);
        }
    }
    #[test]
    fn sql_fail_closed() {
        assert_eq!(
            classify("database.query", &json!({"sql":"SELECT name FROM tasks"})),
            Risk::Free
        );
        for s in [
            "SELECT 1; DROP TABLE x",
            "PRAGMA writable_schema=1",
            "WITH x AS (...) DELETE FROM a",
            "SELECT load_extension('x')",
        ] {
            assert_eq!(
                classify("database.query", &json!({"sql":s})),
                Risk::Mandatory
            );
        }
    }
    #[test]
    fn secrets_nested() {
        assert_eq!(
            redact(&json!({"a":{"refresh_token":"value"},"b":"Bearer abc"})),
            json!({"a":{"refresh_token":"[REDACTED]"},"b":"[REDACTED]"})
        );
    }
    #[test]
    fn privacy_paths() {
        for p in [
            "C:\\Windows\\System32",
            "C:\\Users\\me\\.ssh\\id_rsa",
            "C:\\Users\\me\\.env",
            "C:\\Users\\me\\Chrome\\User Data\\Cookies",
        ] {
            assert!(protected(p));
        }
    }
}
