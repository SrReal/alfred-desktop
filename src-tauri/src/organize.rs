use crate::{
    file_tree::{self, Control},
    runtime::Core,
    storage, undo,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v[key].as_str().ok_or(format!("ORGANIZE_INPUT_{key}"))
}
fn folder_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let device = lower.split('.').next().unwrap_or("");
    !name.is_empty()
        && name.len() <= 100
        && !name.starts_with('.')
        && !name.ends_with(['.', ' '])
        && !name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        && ![
            "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7",
            "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
        ]
        .contains(&device)
}
fn preview(c: &Core, path: &Path, input: &Value, control: &Control) -> Result<Value, String> {
    let lease = file_tree::directory_lease(c, path, control)?;
    let recovery = file_tree::recovery_paths(c, control)?;
    if file_tree::reserved_path(path, &recovery) {
        return Err("ORGANIZE_RECOVERY_ARTIFACT".into());
    }
    let rules = input["body"]["rules"].as_array().ok_or("ORGANIZE_RULES")?;
    if rules.is_empty() || rules.len() > 32 {
        return Err("ORGANIZE_RULE_LIMIT".into());
    }
    let mut groups = BTreeMap::new();
    for rule in rules {
        control.check(c)?;
        let folder = string(rule, "folder")?;
        if !folder_name(folder) {
            return Err("ORGANIZE_FOLDER_NAME".into());
        }
        let extensions = rule["extensions"].as_array().ok_or("ORGANIZE_EXTENSIONS")?;
        if extensions.is_empty() || extensions.len() > 50 {
            return Err("ORGANIZE_EXTENSION_LIMIT".into());
        }
        for extension in extensions {
            let ext = extension
                .as_str()
                .ok_or("ORGANIZE_EXTENSION")?
                .trim_start_matches('.')
                .to_ascii_lowercase();
            if ext.is_empty() || ext.len() > 20 || !ext.chars().all(|ch| ch.is_ascii_alphanumeric())
            {
                return Err("ORGANIZE_EXTENSION".into());
            }
            if groups.insert(ext, folder.to_string()).is_some() {
                return Err("ORGANIZE_DUPLICATE_RULE".into());
            }
        }
        c.path(path.join(folder).to_string_lossy().as_ref(), true)?;
    }
    let mut entries = std::fs::read_dir(path)
        .map_err(|_| "ORGANIZE_DIRECTORY")?
        .take(501)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "ORGANIZE_DIRECTORY")?;
    if entries.len() > 500 {
        return Err("ORGANIZE_ENTRY_LIMIT".into());
    }
    entries.sort_by_key(|entry| entry.file_name());
    let mut moves = Vec::new();
    let mut destinations = HashSet::new();
    let mut bytes = 0u64;
    for entry in entries {
        control.check(c)?;
        let Ok(source) = c.path(entry.path().to_string_lossy().as_ref(), false) else {
            continue;
        };
        if file_tree::reserved_path(&source, &recovery) || !source.is_file() {
            continue;
        }
        let extension = source
            .extension()
            .and_then(|v| v.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let Some(folder) = groups.get(&extension) else {
            continue;
        };
        let dest = path
            .join(folder)
            .join(source.file_name().ok_or("ORGANIZE_FILE_NAME")?);
        // New grouping directories do not exist yet. Check their parent now and
        // validate the full destination again after creating the directory.
        let parent = c.path(
            dest.parent()
                .ok_or("ORGANIZE_PARENT")?
                .to_string_lossy()
                .as_ref(),
            true,
        )?;
        if parent.exists() && !parent.is_dir() {
            return Err("ORGANIZE_FOLDER_NOT_DIRECTORY".into());
        }
        if dest.exists() || !destinations.insert(dest.to_string_lossy().to_ascii_lowercase()) {
            return Err("ORGANIZE_DESTINATION_EXISTS".into());
        }
        let info = file_tree::file_snapshot(c, &source, control)?;
        if info["root_id"] != lease.identity {
            return Err("ORGANIZE_ROOT_CHANGED".into());
        }
        let parent_id = if parent.exists() {
            let parent = c.path(parent.to_str().ok_or("ORGANIZE_PARENT")?, false)?;
            if file_tree::reserved_path(&parent, &recovery) {
                return Err("ORGANIZE_RECOVERY_ARTIFACT".into());
            }
            json!(file_tree::directory_lease(c, &parent, control)?.identity)
        } else {
            Value::Null
        };
        bytes = bytes.saturating_add(info["bytes"].as_u64().ok_or("ORGANIZE_METADATA")?);
        if bytes > 256 * 1024 * 1024 || moves.len() >= 250 {
            return Err("ORGANIZE_SIZE_LIMIT".into());
        }
        moves.push(json!({"source":source,"destination":dest,"sha256":info["sha256"],"object_id":info["object_id"],"root_id":lease.identity,"destination_parent_id":parent_id}));
    }
    control.check(c)?;
    let id = storage::id();
    c.store.execute(
        "INSERT INTO file_plans(id,root_path,plan_json,created_at,updated_at) VALUES(?,?,?,?,?)",
        &[
            json!(id),
            json!(path),
            json!(json!(moves).to_string()),
            json!(storage::now()),
            json!(storage::now()),
        ],
    )?;
    Ok(
        json!({"plan_id":id,"state":"preview","moves":moves,"applied":false,"expires_after_minutes":60}),
    )
}
fn validate_move(
    c: &Core,
    root: &Path,
    item: &Value,
    control: &Control,
) -> Result<(PathBuf, PathBuf), String> {
    control.check(c)?;
    if item["object_id"].as_str().is_none() || item["root_id"].as_str().is_none() {
        return Err("ORGANIZE_LEGACY_PLAN_REVIEW_REQUIRED".into());
    }
    let source = c.path(string(item, "source")?, false)?;
    let destination = PathBuf::from(string(item, "destination")?);
    if source.parent() != Some(root)
        || !destination.starts_with(root)
        || destination.parent().and_then(Path::parent) != Some(root)
    {
        return Err("ORGANIZE_PLAN_SCOPE".into());
    }
    let parent = c.path(
        destination
            .parent()
            .ok_or("ORGANIZE_PARENT")?
            .to_string_lossy()
            .as_ref(),
        true,
    )?;
    if parent.exists() && !parent.is_dir() {
        return Err("ORGANIZE_FOLDER_NOT_DIRECTORY".into());
    }
    if destination.exists() {
        return Err("ORGANIZE_DESTINATION_EXISTS".into());
    }
    let info = file_tree::file_snapshot(c, &source, control)?;
    if info["sha256"] != item["sha256"]
        || info["object_id"] != item["object_id"]
        || info["root_id"] != item["root_id"]
    {
        return Err("ORGANIZE_SOURCE_CHANGED".into());
    }
    Ok((source, destination))
}
fn apply(c: &Core, root: &Path, id: &str, control: &Control) -> Result<Value, String> {
    let root_lease = file_tree::directory_lease(c, root, control)?;
    let recovery = file_tree::recovery_paths(c, control)?;
    if file_tree::reserved_path(root, &recovery) {
        return Err("ORGANIZE_RECOVERY_ARTIFACT".into());
    }
    let rows = c
        .store
        .query("SELECT * FROM file_plans WHERE id=?", &[json!(id)])?;
    let row = &rows[0];
    if row.is_null() {
        return Err("ORGANIZE_PLAN_NOT_FOUND".into());
    }
    if row["state"] != "preview" {
        return Err("ORGANIZE_PLAN_NOT_AVAILABLE".into());
    }
    if c.path(string(row, "root_path")?, false)? != root {
        return Err("ORGANIZE_PLAN_SCOPE".into());
    }
    let created = chrono::DateTime::parse_from_rfc3339(string(row, "created_at")?)
        .map_err(|_| "ORGANIZE_PLAN_DATE")?;
    if chrono::Utc::now()
        .signed_duration_since(created)
        .num_minutes()
        >= 60
    {
        return Err("ORGANIZE_PLAN_EXPIRED".into());
    }
    let moves: Vec<Value> =
        serde_json::from_str(string(row, "plan_json")?).map_err(|_| "ORGANIZE_PLAN_JSON")?;
    if moves.len() > 250 {
        return Err("ORGANIZE_ENTRY_LIMIT".into());
    }
    // Reject the whole plan before moving anything if any source changed or
    // destination is occupied. Recheck again immediately before each move.
    for item in &moves {
        if item["object_id"].as_str().is_none() || item["root_id"].as_str().is_none() {
            return Err("ORGANIZE_LEGACY_PLAN_REVIEW_REQUIRED".into());
        }
        if item["root_id"] != root_lease.identity {
            return Err("ORGANIZE_ROOT_CHANGED".into());
        }
        let (source, destination) = validate_move(c, root, item, control)?;
        if file_tree::reserved_path(&source, &recovery)
            || file_tree::reserved_path(&destination, &recovery)
        {
            return Err("ORGANIZE_RECOVERY_ARTIFACT".into());
        }
        let parent = destination.parent().ok_or("ORGANIZE_PARENT")?;
        if parent.exists() {
            if item["destination_parent_id"].is_null() {
                return Err("ORGANIZE_GROUP_APPEARED".into());
            }
            if file_tree::directory_lease(c, parent, control)?.identity
                != item["destination_parent_id"]
            {
                return Err("ORGANIZE_DESTINATION_PARENT_CHANGED".into());
            }
        } else if !item["destination_parent_id"].is_null() {
            return Err("ORGANIZE_DESTINATION_PARENT_CHANGED".into());
        }
    }
    if c.store.execute(
        "UPDATE file_plans SET state='running',updated_at=? WHERE id=? AND state='preview'",
        &[json!(storage::now()), json!(id)],
    )? != 1
    {
        return Err("ORGANIZE_PLAN_CLAIMED".into());
    }
    let mut completed = Vec::new();
    let mut groups = BTreeMap::new();
    let mut failure = None;
    for item in &moves {
        let step = (|| {
            let (source, destination) = validate_move(c, root, item, control)?;
            let parent = destination.parent().ok_or("ORGANIZE_PARENT")?;
            if !groups.contains_key(parent) {
                let lease = file_tree::grouping_directory(
                    c,
                    parent,
                    &root_lease.identity,
                    item["destination_parent_id"].as_str(),
                    control,
                )?;
                let group_token = lease.undo_token.clone();
                groups.insert(parent.to_path_buf(), lease);
                if let Some(token) = group_token {
                    c.store.audit(
                        "files.organize",
                        "contextual",
                        &json!({"plan_id":id,"folder":parent}),
                        "succeeded",
                        &json!({"path":parent,"undo_token":token,"operation":"created_group"}),
                        0,
                    )?;
                    c.emit("alfredpet:activity", &json!({}));
                }
            }
            let mut reviewed = item.clone();
            reviewed["destination_parent_id"] =
                json!(groups.get(parent).ok_or("ORGANIZE_GROUP")?.identity);
            let destination = c.path(destination.to_string_lossy().as_ref(), true)?;
            let moved = file_tree::mutate_checked(
                c,
                "move",
                &source,
                Some(&destination),
                control,
                Some(&reviewed),
            )?;
            completed.push(
                json!({"source":source,"destination":destination,"undo_token":moved["undo_token"]}),
            );
            Ok::<(), String>(())
        })();
        if let Err(error) = step {
            failure = Some(error);
            break;
        }
    }
    let group_tokens = groups
        .values()
        .filter_map(|group| group.undo_token.clone())
        .collect::<Vec<_>>();
    drop(groups);
    if let Some(error) = failure {
        let mut rollback_errors = Vec::new();
        for step in completed.iter().rev() {
            let token = string(step, "undo_token")?;
            if let Err(error) = undo::execute(c, token) {
                rollback_errors.push(json!({"undo_token":token,"error_code":error}));
            }
        }
        for token in group_tokens.iter().rev() {
            if let Err(error) = undo::execute(c, token) {
                rollback_errors.push(json!({"undo_token":token,"error_code":error}));
            }
        }
        c.store.execute(
            "UPDATE file_plans SET state=?,updated_at=? WHERE id=?",
            &[
                json!(
                    if rollback_errors.is_empty() && !error.contains("recovery_undo_id=") {
                        "rolled_back"
                    } else {
                        "partial"
                    }
                ),
                json!(storage::now()),
                json!(id),
            ],
        )?;
        return Err(format!(
            "ORGANIZE_FAILED: {error}; rollback_errors={}",
            json!(rollback_errors)
        ));
    }
    c.store.execute(
        "UPDATE file_plans SET state='applied',updated_at=? WHERE id=?",
        &[json!(storage::now()), json!(id)],
    )?;
    // Individual tokens get audit entries so Activity exposes each undo button.
    for step in &completed {
        c.store.audit(
            "files.move",
            "contextual",
            &json!({"plan_id":id,"path":step["source"],"destination":step["destination"]}),
            "succeeded",
            &json!({"path":step["destination"],"undo_token":step["undo_token"]}),
            0,
        )?;
    }
    c.emit("alfredpet:activity", &json!({}));
    Ok(
        json!({"plan_id":id,"state":"applied","moves":completed,"group_undo_tokens":group_tokens,"applied":true}),
    )
}
pub fn recover(c: &Core) -> Result<(), String> {
    c.store.execute(
        "UPDATE file_plans SET state='interrupted',updated_at=? WHERE state='running'",
        &[json!(storage::now())],
    )?;
    Ok(())
}
pub(crate) fn review(c: &Core, input: &Value) -> Result<Value, String> {
    let id = input["body"]["plan_id"]
        .as_str()
        .ok_or("ORGANIZE_PLAN_ID")?;
    let row = c.store.query(
        "SELECT root_path,state,plan_json FROM file_plans WHERE id=?",
        &[json!(id)],
    )?[0]
        .clone();
    if row.is_null() {
        return Err("ORGANIZE_PLAN_NOT_FOUND".into());
    }
    if row["state"] != "preview" {
        return Err("ORGANIZE_PLAN_NOT_AVAILABLE".into());
    }
    if c.path(string(&row, "root_path")?, false)? != c.path(string(input, "path")?, false)? {
        return Err("ORGANIZE_PLAN_SCOPE".into());
    }
    let moves: Vec<Value> =
        serde_json::from_str(string(&row, "plan_json")?).map_err(|_| "ORGANIZE_PLAN_JSON")?;
    if moves.len() > 250 {
        return Err("ORGANIZE_ENTRY_LIMIT".into());
    }
    Ok(
        json!({"plan_id":id,"path":row["root_path"],"action":"move the reviewed files into their grouping folders without replacement","moves":moves.iter().map(|item|json!({"source":item["source"],"destination":item["destination"]})).collect::<Vec<_>>() }),
    )
}
#[cfg(test)]
pub fn dispatch(c: &Core, input: &Value) -> Result<Value, String> {
    dispatch_control(c, input, &Control::default())
}
pub fn dispatch_control(c: &Core, input: &Value, control: &Control) -> Result<Value, String> {
    control.check(c)?;
    let path = c.path(string(input, "path")?, false)?;
    if !path.is_dir() {
        return Err("ORGANIZE_DIRECTORY_REQUIRED".into());
    }
    if input["body"]["action"] == "list" {
        return c.store.query("SELECT id,state,created_at,updated_at,plan_json FROM file_plans WHERE root_path=? ORDER BY created_at DESC LIMIT 100",&[json!(path)]);
    }
    if let Some(id) = input["body"]["plan_id"].as_str() {
        apply(c, &path, id, control)
    } else {
        preview(c, &path, input, control)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_apply_changed_sources_collisions_and_individual_undo() {
        let root = std::env::temp_dir().join(format!("ap-organize-{}", storage::id()));
        let c = Core::new(
            root.clone(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap();
        let dir = root.join("temp/inbox");
        std::fs::create_dir(&dir).unwrap();
        let source = dir.join("a.TXT");
        std::fs::write(&source, "original").unwrap();
        std::fs::write(dir.join("b.md"), "second").unwrap();
        std::fs::write(dir.join("ignored.zip"), "untouched").unwrap();
        let rules =
            json!({"path":dir,"body":{"rules":[{"extensions":["txt","md"],"folder":"Documents"}]}});
        let plan = dispatch(&c, &rules).unwrap();
        assert_eq!(plan["moves"].as_array().unwrap().len(), 2);
        assert!(source.exists());
        assert!(!dir.join("Documents").exists());
        let apply_input = json!({"path":dir,"body":{"plan_id":plan["plan_id"]}});
        std::fs::write(&source, "later edit").unwrap();
        assert_eq!(
            dispatch(&c, &apply_input).unwrap_err(),
            "ORGANIZE_SOURCE_CHANGED"
        );
        assert!(dir.join("b.md").exists());
        std::fs::write(&source, "original").unwrap();
        std::fs::create_dir(dir.join("Documents")).unwrap();
        std::fs::write(dir.join("Documents/a.TXT"), "occupied").unwrap();
        assert_eq!(
            dispatch(&c, &apply_input).unwrap_err(),
            "ORGANIZE_DESTINATION_EXISTS"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("Documents/a.TXT")).unwrap(),
            "occupied"
        );
        std::fs::remove_file(dir.join("Documents/a.TXT")).unwrap();
        // A grouping directory appeared after the first review. Require a
        // fresh preview to bind its identity before adopting that destination.
        let plan = dispatch(&c, &rules).unwrap();
        let apply_input = json!({"path":dir,"body":{"plan_id":plan["plan_id"]}});
        let applied = dispatch(&c, &apply_input).unwrap();
        assert!(!source.exists());
        assert!(dir.join("ignored.zip").exists());
        assert_eq!(
            dispatch(&c, &apply_input).unwrap_err(),
            "ORGANIZE_PLAN_NOT_AVAILABLE"
        );
        for step in applied["moves"].as_array().unwrap() {
            undo::execute(&c, step["undo_token"].as_str().unwrap()).unwrap();
        }
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "original");
        assert!(dir.join("b.md").exists());
        let audit = c
            .store
            .query(
                "SELECT COUNT(*) AS n FROM activity_events WHERE undo_token IS NOT NULL",
                &[],
            )
            .unwrap();
        assert_eq!(audit[0]["n"], 2);
        assert_eq!(
            crate::policy::classify("files.organize", &rules),
            crate::policy::Risk::Free
        );
        assert_eq!(
            crate::policy::classify("files.organize", &apply_input),
            crate::policy::Risk::Contextual
        );
        for name in ["../escape", "CON", "LPT1.txt", "folder.", "other:stream"] {
            assert!(dispatch(
                &c,
                &json!({"path":dir,"body":{"rules":[{"extensions":["txt"],"folder":name}]}})
            )
            .is_err());
        }
        // The primitive itself must reject an existing destination, independently
        // of any preflight exists() check.
        let destination = dir.join("already.txt");
        std::fs::write(&destination, "keep").unwrap();
        assert!(crate::modules::move_no_replace(&source, &destination).is_err());
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), "keep");
        assert!(source.exists());
        // A source can become locked after preview. The first move must roll
        // back when the second move fails, preserving both original files.
        let plan = dispatch(&c, &rules).unwrap();
        use std::os::windows::fs::OpenOptionsExt;
        let locked = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(dir.join("b.md"))
            .unwrap();
        let failure =
            dispatch(&c, &json!({"path":dir,"body":{"plan_id":plan["plan_id"]}})).unwrap_err();
        assert!(failure.starts_with("ORGANIZE_FAILED:"));
        assert!(failure.ends_with("rollback_errors=[]"), "{failure}");
        assert!(source.exists());
        assert!(dir.join("b.md").exists());
        assert_eq!(
            c.store
                .query(
                    "SELECT state FROM file_plans WHERE id=?",
                    &[plan["plan_id"].clone()]
                )
                .unwrap()[0]["state"],
            "partial"
        );
        drop(locked);
        drop(c);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(all(test, windows))]
#[path = "organize_contract_tests.rs"]
mod contract_tests;
