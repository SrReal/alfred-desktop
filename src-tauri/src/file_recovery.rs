//! Explicit reconciliation never resets a blocked journal to available. It
//! proves the remaining inverse action, or pins a verified restored object until
//! its durable journal is completed without repeating filesystem mutations.
use super::*;

pub(crate) struct VerifiedRestoration {
    _trees: Vec<Tree>,
    _parents: Vec<File>,
}

fn pinned(
    c: &Core,
    path: &Path,
    id: &str,
    digest: &str,
    control: &Control,
) -> Result<Tree, String> {
    let tree = Tree::capture(c, path, true, control)?;
    if tree.identity != id {
        return Err("UNDO_OBJECT_REPLACED".into());
    }
    if tree.digest != digest {
        return Err("UNDO_FILE_CHANGED: preserve later edits".into());
    }
    Ok(tree)
}

pub(crate) fn revalidate(
    c: &Core,
    row: &Value,
    control: &Control,
) -> Result<Option<VerifiedRestoration>, String> {
    control.check(c)?;
    let id = row["object_id"].as_str().ok_or("UNDO_OBJECT_ID_MISSING")?;
    let original = c.path(row["path"].as_str().ok_or("UNDO_PATH")?, true)?;
    let original_parents = parents(c, &original)?;
    source_parent(c, row, control)?;
    let data = metadata(row)?;
    let digest = row["expected_hash"].as_str().ok_or("UNDO_HASH")?;
    let mut trees = Vec::new();
    let completed = match row["kind"].as_str() {
        Some("tree_cross_move") => {
            if cross::position(c, row, control)? == cross::Position::Undone {
                let parked = c.path(
                    data["undo_target"].as_str().ok_or("CROSS_UNDO_TARGET")?,
                    false,
                )?;
                trees.push(pinned(c, &parked, id, digest, control)?);
                if !matches!(
                    data["phase"].as_str(),
                    Some("undo_copy_only" | "undone_copy_only")
                ) {
                    trees.push(pinned(
                        c,
                        &original,
                        data["source_id"].as_str().ok_or("CROSS_SOURCE_ID")?,
                        digest,
                        control,
                    )?);
                }
                true
            } else {
                false
            }
        }
        Some("tree_move" | "tree_delete" | "tree_copy") => {
            // Revalidate every configured path even when only the journal needs
            // reconciliation; a revoked path never becomes implicitly granted.
            for key in ["destination", "checkpoint"] {
                if let Some(path) = row[key].as_str() {
                    c.path(path, true)?;
                }
            }
            let restored = if row["kind"] == "tree_copy" {
                c.path(row["checkpoint"].as_str().ok_or("UNDO_CHECKPOINT")?, true)?
            } else {
                original.clone()
            };
            if data["undo_started"] == true && restored.exists() {
                trees.push(pinned(c, &restored, id, digest, control)?);
                true
            } else {
                expected_state(c, row, control)?;
                false
            }
        }
        Some("write") => {
            // A failed checkpoint never authorized truncation. Reconcile an
            // intact original only from its recorded hash and native identity;
            // the incomplete checkpoint is retained, never used for restore.
            if data["write_failure"]["mutation_attempted"] == false
                && data["existed"] == true
                && data["write_failure"]["object_id"] == row["object_id"]
                && data["write_failure"]["observed_hash"] == data["original_hash"]
            {
                if let Some(path) = row["checkpoint"].as_str() {
                    c.path(path, true)?;
                }
                let tree = Tree::capture(c, &original, true, control)?;
                if tree.identity != id {
                    return Err("UNDO_OBJECT_REPLACED".into());
                }
                if raw_hash(
                    c,
                    &mut tree.root.try_clone().map_err(|_| "TREE_HANDLE")?,
                    control,
                )? != data["original_hash"].as_str().ok_or("UNDO_ORIGINAL_HASH")?
                {
                    return Err("UNDO_FILE_CHANGED: preserve later edits".into());
                }
                trees.push(tree);
                control.check(c)?;
                return Ok(Some(VerifiedRestoration {
                    _trees: trees,
                    _parents: original_parents,
                }));
            }
            let saved = if let Some(path) = row["checkpoint"].as_str() {
                let checkpoint = c.path(path, false)?;
                let tree = Tree::capture(c, &checkpoint, true, control)?;
                if tree.identity
                    != data["checkpoint_id"]
                        .as_str()
                        .ok_or("UNDO_CHECKPOINT_ID_MISSING")?
                {
                    return Err("UNDO_CHECKPOINT_REPLACED".into());
                }
                let hash = row["checkpoint_hash"]
                    .as_str()
                    .ok_or("UNDO_CHECKPOINT_HASH")?;
                if raw_hash(
                    c,
                    &mut tree.root.try_clone().map_err(|_| "TREE_HANDLE")?,
                    control,
                )? != hash
                {
                    return Err("UNDO_CHECKPOINT_CHANGED".into());
                }
                trees.push(tree);
                Some(hash)
            } else {
                None
            };
            if !original.exists() {
                if saved.is_none() && data["undo_started"] == true {
                    true
                } else {
                    return Err("UNDO_ORIGINAL_MISSING".into());
                }
            } else {
                let tree = Tree::capture(c, &original, true, control)?;
                if tree.identity != id {
                    return Err("UNDO_OBJECT_REPLACED".into());
                }
                let actual = raw_hash(
                    c,
                    &mut tree.root.try_clone().map_err(|_| "TREE_HANDLE")?,
                    control,
                )?;
                let restored =
                    data["undo_started"] == true && saved.is_some_and(|hash| actual == hash);
                if !restored && actual != write_expected_hash(row)? {
                    return Err("UNDO_FILE_CHANGED: preserve later edits".into());
                }
                trees.push(tree);
                restored
            }
        }
        _ => return Err("UNDO_REVALIDATION_REQUIRES_NATIVE_JOURNAL".into()),
    };
    control.check(c)?;
    Ok(completed.then_some(VerifiedRestoration {
        _trees: trees,
        _parents: original_parents,
    }))
}
