use serde_json::{json, Value};
pub struct Module {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: &'static str,
    pub tools: &'static [(&'static str, &'static str)],
}
pub const MODULES: &[Module] = &[
    Module {
        id: "windows",
        name: "Windows",
        kind: "native",
        tools: &[
            ("audit", ""),
            ("admin_action", "action"),
            ("open_app", "app"),
            ("close_app", "process_id"),
            ("list_processes", ""),
            ("stop_process", "process_id"),
            ("list_services", ""),
            ("service_control", "name,action"),
            ("registry_get", "path,name"),
            ("registry_set", "path,name,value"),
            ("list_scheduled_tasks", ""),
            ("run_scheduled_task", "name"),
            ("power_status", ""),
            ("audio_get", ""),
            ("audio_set", "device_id,expected"),
            ("devices", ""),
            ("open_settings_uri", "uri"),
        ],
    },
    Module {
        id: "files",
        name: "Explorador de archivos",
        kind: "native",
        tools: &[
            ("list", "path"),
            ("read", "path"),
            ("find", "path,query"),
            ("write", "path,text"),
            ("copy", "path,destination"),
            ("move", "path,destination"),
            ("rename", "path,destination"),
            ("delete", "path"),
            ("undo", "id"),
            ("undo_history", ""),
            ("rollback", "body"),
            ("cleanup", "body"),
            ("cleanup_plans", ""),
            ("retention_preview", ""),
            ("watch", "path"),
            ("watch_update", "id,body"),
            ("unwatch", "id"),
            ("organize", "path,body"),
            ("compare", "path,destination"),
            ("duplicates", "path"),
            ("compress", "path,destination"),
            ("extract", "path,destination"),
            ("archive_status", "id"),
        ],
    },
    Module {
        id: "terminal",
        name: "PowerShell / Terminal",
        kind: "native",
        tools: &[
            ("command", "command,cwd"),
            ("script", "path,cwd"),
            ("profiles", ""),
            ("profile_save", "name,body"),
            ("profile_delete", "id"),
            ("cancel", "id"),
        ],
    },
    Module {
        id: "window",
        name: "Control de ventanas",
        kind: "native",
        tools: &[
            ("list", ""),
            ("active", ""),
            ("move", "hwnd,x,y,width,height"),
            ("state", "hwnd,state"),
            ("focus", "hwnd"),
            ("snap", "hwnd,kind"),
            ("move_monitor", "hwnd,monitor"),
            ("set_layout", "body"),
            ("layouts", ""),
            ("layout_get", "id"),
            ("layout_save", "name,body"),
            ("layout_apply", "id,fingerprint"),
            ("layout_delete", "id,fingerprint"),
            ("undo", "id"),
            ("undo_history", ""),
        ],
    },
    Module {
        id: "vision",
        name: "Visión de escritorio",
        kind: "native",
        tools: &[
            ("capture_monitor", ""),
            ("capture_window", ""),
            ("capture_region", "x,y,width,height"),
            ("ocr", "path"),
            ("assets", ""),
            ("asset_get", "id"),
            ("asset_delete", "id"),
            ("cleanup", ""),
        ],
    },
    Module {
        id: "gui",
        name: "Automatización GUI",
        kind: "native",
        tools: &[
            ("inspect", "hwnd"),
            ("find_element", "hwnd"),
            ("invoke", "hwnd"),
            ("set_value", "hwnd,text"),
            ("get_text", "hwnd"),
            ("select", "hwnd"),
            ("click", "hwnd"),
            ("hover", "hwnd"),
            ("drag", "hwnd,body"),
            ("send_keys", "hwnd,text"),
            ("wait_for_element", "hwnd"),
            ("screenshot_target", "hwnd"),
        ],
    },
    Module {
        id: "browser",
        name: "Navegador",
        kind: "native",
        tools: &[
            ("open", "url"),
            ("tabs", ""),
            ("inspect", ""),
            ("click", "selector"),
            ("type", "selector,text"),
            ("upload", "selector,path"),
            ("download", "selector"),
            ("screenshot", ""),
            ("wait", "selector"),
            ("submit", "selector"),
            ("forms", ""),
            ("extract", "url"),
            ("select", "url,selector,body"),
            ("check", "url,selector,body"),
            ("fetch", "url"),
            ("search", "provider,query"),
            ("research", "urls"),
        ],
    },
    Module {
        id: "downloads",
        name: "Descargas",
        kind: "native",
        tools: &[
            ("list", "path"),
            ("hash", "path"),
            ("classify", "path"),
            ("wait", "path"),
            ("post_actions_list", ""),
            ("post_actions_inspect", "id"),
            ("post_actions_save", "name,steps"),
            ("post_actions_run", "id,path,expected"),
            ("watch", "path"),
            ("watch_update", "id,body"),
            ("unwatch", "id"),
        ],
    },
    Module {
        id: "clipboard",
        name: "Portapapeles",
        kind: "native",
        tools: &[
            ("get", ""),
            ("set", "text,expected"),
            ("formats", ""),
            ("detect", ""),
            ("transform", "operation,expected"),
            ("image", "expected"),
            ("history", ""),
            ("history_delete", "id"),
            ("history_clear", ""),
        ],
    },
    Module {
        id: "git",
        name: "Git / GitHub",
        kind: "native",
        tools: &[
            ("status", "path"),
            ("diff", "path"),
            ("branches", "path"),
            ("log", "path"),
            ("commits", "path"),
            ("commit", "path,message,expected"),
            ("checkout", "path,branch,expected"),
            ("stash", "path,expected"),
            ("delete_branch", "path,branch,expected"),
            ("repos", "connection"),
            ("repo", "connection,repo"),
            ("issues", "connection,repo"),
            ("prs", "connection,repo"),
            ("pr", "connection,repo,number"),
            ("actions", "connection,repo"),
            ("releases", "connection,repo"),
            ("release", "connection,repo,release_id"),
            ("create_issue", "connection,repo,title,body,expected"),
            ("merge", "connection,repo,number,expected"),
            ("publish_release", "connection,repo,release_id,expected"),
        ],
    },
    Module {
        id: "vscode",
        name: "VS Code / desarrollo",
        kind: "native",
        tools: &[
            ("discover", ""),
            ("sessions", ""),
            ("open", "path"),
            ("commands", "session"),
            ("command", "session,command,arguments,fingerprint"),
            ("palette", "session,fingerprint"),
            ("tasks", "session"),
            ("run_task", "session,task,fingerprint"),
            ("task_status", "session,run"),
            ("task_cancel", "session,run"),
            ("diagnostics", "session"),
            ("run", "path,command"),
        ],
    },
    Module {
        id: "database",
        name: "Bases de datos",
        kind: "native",
        tools: &[
            ("query", "connection,sql"),
            ("execute", "connection,sql"),
            ("tables", "connection"),
            ("schema", "connection"),
            ("explain", "connection,sql"),
            ("export", "connection,sql,path"),
            ("import", "connection,table,path,fingerprint"),
        ],
    },
    Module {
        id: "notion",
        name: "Notion",
        kind: "integration",
        tools: &[
            ("search", "connection,query"),
            ("read", "connection,page_id"),
            ("create", "connection,body"),
            ("update", "connection,page_id,body"),
            ("append", "connection,page_id,body"),
            ("blocks", "connection,page_id"),
            ("read_database", "connection,database_id"),
            ("query_database", "connection,data_source_id,body"),
        ],
    },
    Module {
        id: "drive",
        name: "Google Drive",
        kind: "integration",
        tools: &[
            ("search", "connection,query"),
            ("list", "connection"),
            ("metadata", "connection,file_id"),
            ("download", "connection,file_id"),
            ("upload", "connection,path"),
            ("create_folder", "connection,folder_id,name,expected"),
            ("move", "connection,file_id,body"),
            ("rename", "connection,file_id,body"),
            ("trash", "connection,file_id"),
        ],
    },
    Module {
        id: "homeassistant",
        name: "Home Assistant",
        kind: "integration",
        tools: &[
            ("entities", "connection"),
            ("states", "connection"),
            ("automations", "connection"),
            ("camera_snapshot", "connection,entity_id"),
            ("websocket", "connection,action"),
            ("services", "connection"),
            ("call_service", "connection,domain,service,body"),
            ("events", "connection"),
        ],
    },
    Module {
        id: "rest",
        name: "APIs REST",
        kind: "integration",
        tools: &[
            ("request", "connection,action,request"),
            ("import_openapi", "connection,action"),
        ],
    },
    Module {
        id: "mcp",
        name: "MCP genérico",
        kind: "extensible",
        tools: &[
            ("health", "connection"),
            ("listen", "connection,notifications"),
            ("listen_status", ""),
            ("listen_poll", "connection,stream_id"),
            ("listen_stop", "connection,stream_id"),
            ("oauth_discover", "connection,issuer"),
            ("oauth_registration", "connection,issuer,expected"),
            ("oauth_start", "connection,issuer,expected"),
            ("oauth_status", "connection"),
            ("oauth_cancel", "connection,session_id"),
            ("oauth_refresh", "connection,session_id"),
            ("oauth_recover", "connection,session_id"),
            ("oauth_cleanup", "connection,session_id"),
            (
                "oauth_reset",
                "connection,session_id,acknowledge_remote_unknown",
            ),
            ("tools", "connection"),
            ("resources", "connection"),
            ("prompts", "connection"),
            ("call", "connection,name,body"),
            ("read_resource", "connection,uri"),
            ("get_prompt", "connection,name,body"),
        ],
    },
    Module {
        id: "documents",
        name: "PDF / documentos",
        kind: "native",
        tools: &[
            ("metadata", "path"),
            ("extract_text", "path"),
            ("render_pages", "path,pages"),
            ("split", "path,destination,page"),
            ("merge", "paths,destination"),
            ("convert", "path,destination"),
            ("summarize", "path"),
            ("create_derived_text", "path,destination"),
            ("search", "path,query"),
            ("compare", "path,destination"),
        ],
    },
    Module {
        id: "images",
        name: "Imágenes",
        kind: "native",
        tools: &[
            ("metadata", "path"),
            ("resize", "path,destination,width,height"),
            ("crop", "path,destination,x,y,width,height"),
            ("convert", "path,destination"),
            ("optimize", "path,destination"),
            ("screenshot_prep", "path"),
            ("contact_sheet", "paths,destination"),
            ("analyze", "path"),
            ("compare", "path,destination"),
        ],
    },
    Module {
        id: "printers",
        name: "Impresoras",
        kind: "native",
        tools: &[
            ("list", ""),
            ("default", ""),
            ("status", "name"),
            ("queue", "name"),
            ("jobs", "name"),
            ("cancel_job", "name,job_id"),
            ("print", "path"),
        ],
    },
    Module {
        id: "tasks",
        name: "Sistema de tareas",
        kind: "native",
        tools: &[
            ("list", ""),
            ("create", "title"),
            ("update", "id,expected_revision"),
            ("complete", "id,expected_revision"),
            ("delete", "id,expected_revision"),
            ("history", "id"),
        ],
    },
    Module {
        id: "monitoring",
        name: "Monitorización",
        kind: "native",
        tools: &[
            ("list", ""),
            ("create", "kind,path"),
            ("update", "id,body"),
            ("delete", "id"),
            ("check", ""),
        ],
    },
    Module {
        id: "macros",
        name: "Macros personales",
        kind: "extensible",
        tools: &[
            ("list", ""),
            ("inspect", "id"),
            ("save", "name,steps"),
            ("run", "id"),
            ("set_enabled", "id,enabled,expected"),
            ("delete", "id,expected"),
        ],
    },
    Module {
        id: "custom",
        name: "Herramientas personalizadas",
        kind: "extensible",
        tools: &[
            ("list", ""),
            ("register", "manifest,path"),
            ("run", "id,expected,body"),
            ("inspect", "id"),
            ("configure", "id,expected,enabled"),
            ("versions", "id"),
            ("update", "id,expected,manifest,path"),
            ("rollback", "id,expected,version"),
            ("uninstall", "id,expected"),
        ],
    },
    Module {
        id: "skills",
        name: "Skills de Alfred",
        kind: "extensible",
        tools: &[
            ("list", ""),
            ("inspect", "id"),
            (
                "save",
                "id,name,description,instructions,enabled,expected_revision",
            ),
            ("set_enabled", "id,enabled,expected_revision"),
            ("delete", "id,expected_revision"),
        ],
    },
    Module {
        id: "plugin_builder",
        name: "Plugin Builder",
        kind: "extensible",
        tools: &[
            ("guide", ""),
            ("update", "id,expected,manifest,files"),
            ("create", "manifest,files"),
            ("test", "id,expected"),
            ("enable_plugin", "id,expected"),
        ],
    },
];
pub fn schema(required: &str) -> Value {
    let mut props = serde_json::Map::new();
    for key in [
        "id",
        "path",
        "destination",
        "query",
        "text",
        "app",
        "name",
        "action",
        "uri",
        "state",
        "monitor",
        "selector",
        "automation_id",
        "tab",
        "url",
        "command",
        "profile_id",
        "cwd",
        "branch",
        "repo",
        "title",
        "connection",
        "sql",
        "page_id",
        "page_token",
        "database_id",
        "data_source_id",
        "file_id",
        "domain",
        "service",
        "method",
        "endpoint",
        "kind",
        "due_at",
        "type",
        "content",
        "project_id",
        "source_conversation_id",
        "supersedes",
    ] {
        props.insert(key.into(), json!({"type":"string","maxLength":64000}));
    }
    for key in [
        "process_id",
        "hwnd",
        "x",
        "y",
        "width",
        "height",
        "job_id",
        "number",
        "page",
    ] {
        props.insert(key.into(), json!({"type":"number"}));
    }
    for key in ["body", "manifest", "value"] {
        props.insert(key.into(), json!({}));
    }
    props.insert(
        "args".into(),
        json!({"type":"array","items":{"type":"string"},"maxItems":64}),
    );
    props.insert("steps".into(),json!({"type":"array","items":{"type":"object","required":["tool","input"],"properties":{"tool":{"type":"string"},"input":{"type":"object"}},"additionalProperties":false},"minItems":1,"maxItems":50}));
    props.insert("explicit".into(), json!({"type":"boolean"}));
    props.insert("recursive".into(), json!({"type":"boolean"}));
    props.insert("elevated".into(), json!({"type":"boolean"}));
    props.insert(
        "timeout_seconds".into(),
        json!({"type":"integer","minimum":1,"maximum":3600}),
    );
    json!({"type":"object","properties":props,"required":required.split(',').filter(|s|!s.is_empty()).collect::<Vec<_>>(),"additionalProperties":false})
}
pub fn tool_schema(tool: &str, required: &str) -> Value {
    if crate::system_audit::TOOLS.contains(&tool) {
        return crate::system_audit::schema(tool);
    }
    if tool.starts_with("monitoring.")
        || [
            "files.watch",
            "files.watch_update",
            "files.unwatch",
            "downloads.watch",
            "downloads.watch_update",
            "downloads.unwatch",
        ]
        .contains(&tool)
    {
        return crate::watch_extra::schema(tool);
    }
    if crate::download_post::TOOLS.contains(&tool) {
        return crate::download_post::schema(tool);
    }
    if crate::printers::TOOLS.contains(&tool) {
        return crate::printers::schema(tool);
    }
    if crate::media::TOOLS.contains(&tool) {
        return crate::media::schema(tool);
    }
    if tool.starts_with("mcp.") {
        return crate::mcp_client::tool_schema(tool);
    }
    if crate::rest::TOOLS.contains(&tool) {
        return crate::rest::schema(tool);
    }
    if crate::homeassistant::TOOLS.contains(&tool) {
        return crate::homeassistant::schema(tool);
    }
    if crate::skills::TOOLS.contains(&tool) {
        return crate::skills::schema(tool);
    }
    if crate::macros::TOOLS.contains(&tool) {
        return crate::macros::schema(tool);
    }
    if crate::plugins::TOOLS.contains(&tool) {
        return crate::plugins::schema(tool);
    }
    if crate::notion::TOOLS.contains(&tool) {
        return crate::notion::schema(tool);
    }
    if crate::drive::TOOLS.contains(&tool) {
        return crate::drive::schema(tool);
    }
    if crate::database::TOOLS.contains(&tool) {
        return crate::database::schema(tool);
    }
    if crate::vscode::TOOLS.contains(&tool) {
        return crate::vscode::schema(tool);
    }
    if crate::github::TOOLS.contains(&tool) {
        return crate::github::schema(tool);
    }
    if crate::git_local::TOOLS.contains(&tool) {
        return crate::git_local::schema(tool);
    }
    if tool.starts_with("clipboard.") {
        let expected = json!({"type":"object","required":["sequence","fingerprint"],"properties":{"sequence":{"type":"integer","minimum":0,"maximum":4294967295u64},"fingerprint":{"type":"string","pattern":"^[a-f0-9]{64}$"}},"additionalProperties":false});
        let properties = match tool {
            "clipboard.get"
            | "clipboard.formats"
            | "clipboard.detect"
            | "clipboard.history_clear" => json!({}),
            "clipboard.set" => {
                json!({"text":{"type":"string","maxLength":64000},"expected":expected})
            }
            "clipboard.transform" => {
                json!({"operation":{"enum":crate::clipboard::OPERATIONS},"expected":expected,"apply":{"type":"boolean"}})
            }
            "clipboard.image" => json!({"expected":expected}),
            "clipboard.history" => {
                json!({"limit":{"type":"integer","minimum":1,"maximum":50},"offset":{"type":"integer","minimum":0,"maximum":1000}})
            }
            "clipboard.history_delete" => {
                json!({"id":{"type":"string","format":"uuid","minLength":36,"maxLength":36}})
            }
            _ => return json!({"not":{}}),
        };
        return json!({"type":"object","required":required.split(',').filter(|s|!s.is_empty()).collect::<Vec<_>>(),"properties":properties,"additionalProperties":false});
    }
    if crate::browser_research::TOOLS.contains(&tool) {
        let url = json!({"type":"string","minLength":1,"maxLength":8192});
        let mut properties = serde_json::Map::new();
        properties.insert(
            "max_chars".into(),
            json!({"type":"integer","minimum":1,"maximum":64000}),
        );
        match tool {
            "browser.fetch" => {
                properties.insert("url".into(), url);
            }
            "browser.research" => {
                properties.insert("urls".into(),json!({"type":"array","minItems":1,"maxItems":5,"uniqueItems":true,"items":url}));
            }
            "browser.search" => {
                properties.insert("provider".into(), json!({"const":"duckduckgo"}));
                properties.insert(
                    "query".into(),
                    json!({"type":"string","minLength":1,"maxLength":500}),
                );
                properties.insert(
                    "limit".into(),
                    json!({"type":"integer","minimum":1,"maximum":20}),
                );
            }
            _ => unreachable!(),
        }
        return json!({"type":"object","required":required.split(',').collect::<Vec<_>>(),"properties":properties,"additionalProperties":false});
    }
    if [
        "browser.forms",
        "browser.extract",
        "browser.select",
        "browser.check",
    ]
    .contains(&tool)
    {
        let mut properties = serde_json::Map::new();
        properties.insert(
            "tab".into(),
            json!({"type":"string","minLength":1,"maxLength":100}),
        );
        if tool != "browser.forms" {
            properties.insert(
                "url".into(),
                json!({"type":"string","minLength":1,"maxLength":8192}),
            );
            properties.insert(
                "selector".into(),
                json!({"type":"string","minLength":1,"maxLength":2000}),
            );
            properties.insert("body".into(), if tool == "browser.extract" {
                json!({"type":"object","properties":{"max_chars":{"type":"integer","minimum":1,"maximum":64000}},"additionalProperties":false})
            } else {
                let mut body = json!({"type":"object","properties":{"fingerprint":{"type":"string","pattern":"^[a-f0-9]{64}$"}},"required":["fingerprint"],"additionalProperties":false});
                let key = if tool=="browser.select" { "labels" } else { "checked" };
                body["properties"][key] = if key=="labels" {json!({"type":"array","minItems":1,"maxItems":20,"uniqueItems":true,"items":{"type":"string","minLength":1,"maxLength":300}})} else {json!({"type":"boolean"})};
                body["required"]=json!(["fingerprint",key]);
                body
            });
        }
        return json!({"type":"object","properties":properties,"required":required.split(',').filter(|s|!s.is_empty()).collect::<Vec<_>>(),"additionalProperties":false});
    }
    if tool.starts_with("tasks.") {
        let id = json!({"type":"string","minLength":1,"maxLength":100});
        let linked_id = json!({"type":["string","null"],"minLength":1,"maxLength":100});
        let page = json!({"type":"integer","minimum":0,"maximum":1000000});
        let edit = json!({"title":{"type":"string","minLength":1,"maxLength":500},"description":{"type":"string","maxLength":8000},"due_at":{"type":["string","null"],"maxLength":64},"recurrence":{"oneOf":[{"type":"null"},{"type":"object","required":["frequency"],"properties":{"frequency":{"enum":["daily","weekly","monthly"]},"interval":{"type":"integer","minimum":1,"maximum":365}},"additionalProperties":false}]},"project_id":linked_id,"conversation_id":linked_id});
        let mut properties = edit.as_object().cloned().unwrap_or_default();
        let required = match tool {
            "tasks.list" => {
                return json!({"type":"object","properties":{"project_id":id,"conversation_id":id,"offset":page,"limit":{"type":"integer","minimum":1,"maximum":100}},"additionalProperties":false});
            }
            "tasks.history" => {
                return json!({"type":"object","required":["id"],"properties":{"id":id,"offset":page},"additionalProperties":false});
            }
            "tasks.create" => vec!["title"],
            "tasks.update" => {
                properties.insert("reopen".into(), json!({"type":"boolean"}));
                vec!["id", "expected_revision"]
            }
            "tasks.complete" | "tasks.delete" => {
                properties.clear();
                vec!["id", "expected_revision"]
            }
            _ => return json!({"not":{}}),
        };
        if tool != "tasks.create" {
            properties.insert("id".into(), id);
            properties.insert(
                "expected_revision".into(),
                json!({"type":"integer","minimum":0,"maximum":9007199254740991u64}),
            );
        }
        let mut schema = json!({"type":"object","required":required,"properties":properties,"additionalProperties":false});
        if tool == "tasks.update" {
            schema["anyOf"] = json!([{"required":["title"]},{"required":["description"]},{"required":["due_at"]},{"required":["recurrence"]},{"required":["project_id"]},{"required":["conversation_id"]},{"required":["reopen"]}]);
        }
        return schema;
    }
    if tool == "files.undo" {
        return json!({"type":"object","required":["id"],"properties":{"id":{"type":"string","format":"uuid","minLength":36,"maxLength":36},"revalidate":{"type":"boolean"}},"additionalProperties":false});
    }
    if tool == "files.archive_status" {
        return json!({"type":"object","required":["id"],"properties":{"id":{"type":"string","format":"uuid","minLength":36,"maxLength":36},"offset":{"type":"integer","minimum":0,"maximum":10000}},"additionalProperties":false});
    }
    if [
        "files.cleanup_plans",
        "files.undo_history",
        "files.retention_preview",
    ]
    .contains(&tool)
    {
        return json!({"type":"object","properties":{},"additionalProperties":false});
    }
    if tool == "files.cleanup" {
        let id = json!({"type":"string","format":"uuid","minLength":36,"maxLength":36});
        return json!({"type":"object","required":["body"],"properties":{"body":{"oneOf":[{"type":"object","required":["action","ids"],"properties":{"action":{"const":"preview"},"ids":{"type":"array","minItems":1,"maxItems":100,"uniqueItems":true,"items":id},"forget_undo":{"type":"boolean"},"include_changed":{"type":"boolean"},"adopt_legacy":{"type":"boolean"}},"additionalProperties":false},{"type":"object","required":["action","plan_id"],"properties":{"action":{"const":"apply"},"plan_id":id,"revalidate_policy":{"type":"boolean"}},"additionalProperties":false}]}},"additionalProperties":false});
    }
    if tool == "files.rollback" {
        let id = json!({"type":"string","format":"uuid","minLength":36,"maxLength":36});
        return json!({"type":"object","required":["body"],"properties":{"body":{"oneOf":[{"type":"object","required":["tokens"],"properties":{"tokens":{"type":"array","minItems":1,"maxItems":100,"uniqueItems":true,"items":id}},"additionalProperties":false},{"type":"object","required":["id"],"properties":{"id":id},"additionalProperties":false}]}},"additionalProperties":false});
    }
    if tool == "window.undo" {
        return json!({"type":"object","required":["id"],"properties":{"id":{"type":"string","format":"uuid","minLength":36,"maxLength":36},"revalidate":{"type":"boolean"}},"additionalProperties":false});
    }
    if tool == "window.undo_history" {
        return json!({"type":"object","properties":{},"additionalProperties":false});
    }
    if tool.starts_with("window.layout") {
        let id = json!({"type":"string","format":"uuid","minLength":36,"maxLength":36});
        return match tool {
            "window.layouts" => {
                json!({"type":"object","properties":{},"additionalProperties":false})
            }
            "window.layout_get" => {
                json!({"type":"object","properties":{"id":id},"required":["id"],"additionalProperties":false})
            }
            "window.layout_save" => {
                json!({"type":"object","properties":{"name":{"type":"string","minLength":1,"maxLength":100},"body":{"type":"array","minItems":1,"maxItems":32,"items":{"type":"object","properties":{"hwnd":{"type":"integer","minimum":1},"process_id":{"type":"integer","minimum":1,"maximum":2147483647}},"required":["hwnd","process_id"],"additionalProperties":false}}},"required":["name","body"],"additionalProperties":false})
            }
            _ => {
                json!({"type":"object","properties":{"id":id,"fingerprint":{"type":"string","pattern":"^[a-f0-9]{64}$"}},"required":["id","fingerprint"],"additionalProperties":false})
            }
        };
    }
    if ["vision.assets", "vision.cleanup"].contains(&tool) {
        return json!({"type":"object","properties":{},"additionalProperties":false});
    }
    if ["vision.asset_get", "vision.asset_delete"].contains(&tool) {
        return json!({"type":"object","properties":{"id":{"type":"string","format":"uuid","maxLength":36}},"required":["id"],"additionalProperties":false});
    }
    if tool == "windows.audio_get" {
        return json!({"type":"object","properties":{"device_id":{"type":"string","minLength":1,"maxLength":4096},"role":{"enum":["console","multimedia","communications"]}},"additionalProperties":false});
    }
    if tool == "windows.audio_set" {
        return json!({"type":"object","properties":{"device_id":{"type":"string","minLength":1,"maxLength":4096},"expected":{"type":"object","properties":{"volume":{"type":"number","minimum":0,"maximum":100},"muted":{"type":"boolean"}},"required":["volume","muted"],"additionalProperties":false},"volume":{"type":"number","minimum":0,"maximum":100},"muted":{"type":"boolean"}},"required":["device_id","expected"],"anyOf":[{"required":["volume"]},{"required":["muted"]}],"additionalProperties":false});
    }
    let mut result = schema(required);
    if tool.starts_with("gui.") {
        let part = json!({"type":"object","properties":{"name":{"type":"string","minLength":1,"maxLength":1024},"automation_id":{"type":"string","minLength":1,"maxLength":1024},"control_type":{"type":"integer","minimum":50000,"maximum":50040},"scope":{"enum":["self","children","descendants"]}},"additionalProperties":false});
        let mut selector = part.clone();
        selector["properties"]["path"] =
            json!({"type":"array","minItems":1,"maxItems":8,"items":part});
        result["properties"]["selector"] = selector.clone();
        result["properties"]["hwnd"] = json!({"type":"integer","minimum":1});
        result["properties"]["process_id"] =
            json!({"type":"integer","minimum":1,"maximum":2147483647});
        result["properties"]["name"] = json!({"type":"string","minLength":1,"maxLength":1024});
        result["properties"]["automation_id"] =
            json!({"type":"string","minLength":1,"maxLength":1024});
        result["properties"]["timeout_seconds"] =
            json!({"type":"integer","minimum":1,"maximum":30});
        result["properties"]["state"] = json!({"enum":["present","enabled","absent"]});
        result["properties"]["body"] = json!({"type":"object","properties":{"focus_and_restore":{"type":"boolean"},"duration_ms":{"type":"integer","minimum":100,"maximum":3000},"destination":{"type":"object","properties":{"hwnd":{"type":"integer","minimum":1},"process_id":{"type":"integer","minimum":1,"maximum":2147483647},"selector":selector,"name":{"type":"string","minLength":1,"maxLength":1024},"automation_id":{"type":"string","minLength":1,"maxLength":1024}},"additionalProperties":false}},"additionalProperties":false});
        if tool == "gui.drag" {
            result["properties"]["body"]["required"] = json!(["destination"]);
        }
    }
    if ["terminal.command", "terminal.script"].contains(&tool) {
        result["properties"]["body"] = json!({"type":"object","properties":{"env":{"type":"object","maxProperties":32,"additionalProperties":{"type":"string","maxLength":2000}}},"additionalProperties":false});
    }
    if tool == "terminal.profile_save" {
        result["properties"]["body"] = json!({"type":"object","properties":{"shell":{"enum":["auto","powershell7","windows_powershell"]},"script_path":{"type":"string","minLength":1,"maxLength":4000},"env_allowlist":{"type":"array","maxItems":32,"uniqueItems":true,"items":{"type":"string","minLength":1,"maxLength":64}}},"additionalProperties":false});
    }
    if tool == "files.organize" {
        result["properties"]["body"] = json!({"oneOf":[
            {"type":"object","required":["rules"],"properties":{"rules":{"type":"array","minItems":1,"maxItems":32,"items":{"type":"object","required":["extensions","folder"],"properties":{"extensions":{"type":"array","minItems":1,"maxItems":50,"items":{"type":"string","maxLength":20}},"folder":{"type":"string","minLength":1,"maxLength":100}},"additionalProperties":false}}},"additionalProperties":false},
            {"type":"object","required":["plan_id"],"properties":{"plan_id":{"type":"string","minLength":1,"maxLength":100}},"additionalProperties":false},
            {"type":"object","required":["action"],"properties":{"action":{"const":"list"}},"additionalProperties":false}
        ]});
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_unique_valid() {
        assert_eq!(MODULES.len(), 26);
        let mut ids = std::collections::HashSet::new();
        for m in MODULES {
            assert!(ids.insert(m.id));
            for (_, required) in m.tools {
                jsonschema::validator_for(&schema(required)).unwrap();
            }
            for (action, required) in m.tools {
                jsonschema::validator_for(&tool_schema(&format!("{}.{}", m.id, action), required))
                    .unwrap();
            }
        }
    }
}
