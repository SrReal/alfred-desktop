//! Native leases and reviewed object identities shared by the file organizer.
use super::*;

pub(crate) struct DirectoryLease {
    pub(crate) identity: String,
    pub(crate) undo_token: Option<String>,
    _file: File,
    _parents: Vec<File>,
}

pub(crate) fn directory_lease(
    c: &Core,
    path: &Path,
    control: &Control,
) -> Result<DirectoryLease, String> {
    control.check(c)?;
    c.path(path.to_str().ok_or("TREE_PATH_ENCODING")?, false)?;
    let parents = parents(c, path)?;
    let file = open(path, false, true)?;
    if !file.metadata().map_err(|_| "TREE_METADATA")?.is_dir() {
        return Err("ORGANIZE_FOLDER_NOT_DIRECTORY".into());
    }
    plain_streams(path)?;
    Ok(DirectoryLease {
        identity: identity(&file)?,
        undo_token: None,
        _file: file,
        _parents: parents,
    })
}

pub(crate) fn file_snapshot(c: &Core, path: &Path, control: &Control) -> Result<Value, String> {
    let tree = Tree::capture(c, path, false, control)?;
    if tree.nodes.len() != 1 || tree.nodes[0].directory {
        return Err("ORGANIZE_FILE_REQUIRED".into());
    }
    Ok(
        json!({"object_id":tree.identity,"sha256":tree.digest,"bytes":tree.bytes,"root_id":identity(&tree._parents[0])?}),
    )
}

pub(crate) fn recovery_paths(c: &Core, control: &Control) -> Result<Vec<PathBuf>, String> {
    control.check(c)?;
    let rows = c.store.query("SELECT checkpoint AS path FROM undo_records WHERE checkpoint IS NOT NULL UNION SELECT staging AS path FROM undo_records WHERE staging IS NOT NULL UNION SELECT json_extract(metadata_json,'$.undo_target') AS path FROM undo_records WHERE json_extract(metadata_json,'$.undo_target') IS NOT NULL UNION SELECT json_extract(value,'$.work_path') AS path FROM file_cleanup_plans,json_each(snapshot_json,'$.entries') WHERE file_cleanup_plans.state!='completed' LIMIT 10001", &[])?;
    let rows = rows.as_array().ok_or("UNDO_RECORDS")?;
    if rows.len() > MAX_NODES {
        return Err("ORGANIZE_RECOVERY_INDEX_LIMIT".into());
    }
    let mut paths = vec![c
        .root
        .join("backups")
        .canonicalize()
        .map_err(|_| "TREE_PATH_RESOLVE")?];
    for row in rows {
        control.check(c)?;
        let path = Path::new(row["path"].as_str().ok_or("UNDO_CHECKPOINT")?);
        if let Ok(path) = path.canonicalize() {
            paths.push(path);
        }
    }
    Ok(paths)
}

pub(crate) fn reserved_path(path: &Path, recovery: &[PathBuf]) -> bool {
    recovery.iter().any(|root| path.starts_with(root))
        || path.ancestors().any(|ancestor| {
            ancestor
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| cleanup::managed(Path::new(&name.to_ascii_lowercase())))
        })
}

pub(crate) fn grouping_directory(
    c: &Core,
    path: &Path,
    root_id: &str,
    existing_id: Option<&str>,
    control: &Control,
) -> Result<DirectoryLease, String> {
    control.check(c)?;
    let parents = parents(c, path)?;
    if identity(&parents[0])? != root_id {
        return Err("ORGANIZE_ROOT_CHANGED".into());
    }
    let mut token = None;
    let expected = if path.exists() {
        existing_id.ok_or("ORGANIZE_GROUP_APPEARED")?.to_owned()
    } else {
        if existing_id.is_some() {
            return Err("ORGANIZE_DESTINATION_PARENT_CHANGED".into());
        }
        // Publish an exclusive empty object by handle. Its known native ID must
        // survive reopening; adopting a replacement at the public name is forbidden.
        let stage = path
            .parent()
            .ok_or("TREE_PARENT")?
            .join(format!(".alfredpet-{}.staging", storage::id()));
        c.path(stage.to_str().ok_or("TREE_PATH_ENCODING")?, true)?;
        let retained = checkpoint(c, path, 0, control)?;
        let journal = undo::prepare_tree(
            c,
            "tree_copy",
            path.parent().ok_or("TREE_PARENT")?,
            Some(path),
            Some(&retained),
            "",
        )?;
        c.store.execute(
            "UPDATE undo_records SET staging=? WHERE id=?",
            &[json!(stage), json!(journal)],
        )?;
        let result = (|| {
            control.check(c)?;
            std::fs::create_dir(&stage).map_err(|_| "ORGANIZE_CREATE_FOLDER")?;
            let tree = Tree::capture(c, &stage, true, control)?;
            save_metadata(
                c,
                &journal,
                &json!({"stage_id":tree.identity,"operation":"organize_group","result_nodes":tree.ownership()?}),
            )?;
            if tree.nodes.len() != 1 || hash(c, &stage, control)? != tree.digest {
                return Err("ORGANIZE_GROUP_STAGING_CHANGED".into());
            }
            let id = tree.identity.clone();
            c.store.execute(
                "UPDATE undo_records SET object_id=?,expected_hash=? WHERE id=?",
                &[json!(id), json!(tree.digest), json!(journal)],
            )?;
            control.check(c)?;
            rename(&tree.root, &parents[0], path)?;
            drop(tree);
            undo::commit(c, &journal)?;
            Ok::<_, String>(id)
        })();
        token = Some(journal.clone());
        result.map_err(|error| format!("{error}; recovery_undo_id={journal}"))?
    };
    let mut lease = directory_lease(c, path, control).map_err(|error| {
        token.as_ref().map_or_else(
            || error.clone(),
            |id| format!("{error}; recovery_undo_id={id}"),
        )
    })?;
    if lease.identity != expected {
        return Err(token.as_ref().map_or_else(
            || "ORGANIZE_DESTINATION_PARENT_CHANGED".into(),
            |id| format!("ORGANIZE_DESTINATION_PARENT_CHANGED; recovery_undo_id={id}"),
        ));
    }
    lease.undo_token = token;
    Ok(lease)
}
