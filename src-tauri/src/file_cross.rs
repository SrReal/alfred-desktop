//! Cross-volume move retains the source object on its own volume. No recursive
//! erase is needed in either direction; each namespace transition is journaled.
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Position {
    Moved,
    CopyOnly,
    UndoParked,
    Undone,
}

fn path(c: &Core, row: &Value, key: &str) -> Result<PathBuf, String> {
    c.path(row[key].as_str().ok_or(format!("CROSS_FIELD_{key}"))?, true)
}
fn meta_path(c: &Core, data: &Value) -> Result<PathBuf, String> {
    c.path(
        data["undo_target"].as_str().ok_or("CROSS_UNDO_TARGET")?,
        true,
    )
}
fn verify(c: &Core, p: &Path, digest: &str, id: &str, control: &Control) -> Result<(), String> {
    let tree = Tree::capture(c, p, false, control)?;
    if tree.identity != id {
        return Err("UNDO_OBJECT_REPLACED".into());
    }
    if tree.digest != digest {
        return Err("UNDO_FILE_CHANGED: preserve later edits".into());
    }
    Ok(())
}
pub(super) fn position(c: &Core, row: &Value, control: &Control) -> Result<Position, String> {
    source_parent(c, row, control)?;
    let data = metadata(row)?;
    let original = path(c, row, "path")?;
    let checkpoint = path(c, row, "checkpoint")?;
    let target = path(c, row, "destination")?;
    let parked = meta_path(c, &data)?;
    let digest = row["expected_hash"].as_str().ok_or("UNDO_HASH")?;
    let source_id = data["source_id"].as_str().ok_or("CROSS_SOURCE_ID")?;
    let target_id = row["object_id"].as_str().ok_or("CROSS_TARGET_ID")?;
    if parked.exists() {
        verify(c, &parked, digest, target_id, control)?;
        // An unrelated object may have appeared at the old destination. Leave
        // it alone; our destination is identified at the parked path.
        if original.exists() && !checkpoint.exists() {
            if matches!(
                data["phase"].as_str(),
                Some("undo_copy_only" | "undone_copy_only")
            ) {
                return Ok(Position::Undone);
            }
            verify(c, &original, digest, source_id, control)?;
            return Ok(Position::Undone);
        }
        if !original.exists() && checkpoint.exists() {
            verify(c, &checkpoint, digest, source_id, control)?;
            return Ok(Position::UndoParked);
        }
        return Err("CROSS_RECOVERY_CONFLICT".into());
    }
    verify(c, &target, digest, target_id, control)?;
    if original.exists() && !checkpoint.exists() {
        // Only the copy was published. Undo never rewrites the still-visible
        // source, including any subsequent changes to that source.
        return Ok(Position::CopyOnly);
    }
    if original.exists() {
        return Err("UNDO_DESTINATION_EXISTS".into());
    }
    verify(c, &checkpoint, digest, source_id, control)?;
    Ok(Position::Moved)
}

pub(super) fn mutate(
    c: &Core,
    original: &Path,
    target: &Path,
    source: Tree,
    control: &Control,
) -> Result<Value, String> {
    let retained = checkpoint(c, original, source.bytes.saturating_mul(2), control)?;
    let parked_target = checkpoint(c, target, 0, control)?;
    let token = undo::prepare_tree(
        c,
        "tree_cross_move",
        original,
        Some(target),
        Some(&retained),
        &source.digest,
    )?;
    let mut data = json!({"source_id":source.identity,"checkpoint_id":source.identity,"checkpoint_nodes":source.ownership()?,"undo_target":parked_target,"phase":"copying","source_parent_id":identity(&source._parents[0])?,"scope_root":scoped_root(c,original)});
    save_metadata(c, &token, &data)?;
    let digest = source.digest.clone();
    let source_id = source.identity.clone();
    let result = (|| {
        copy(c, &source, original, target, &token, control)?;
        #[cfg(all(test, windows))]
        crash_tests::at(c, "cross_copy_published");
        data = journal_metadata(c, &token)?;
        data["phase"] = json!("target_published");
        save_metadata(c, &token, &data)?;
        drop(source);
        control.check(c)?;
        relocate(c, original, &retained, &digest, &source_id, control)?;
        #[cfg(all(test, windows))]
        crash_tests::at(c, "cross_original_retained");
        data["phase"] = json!("source_parked");
        save_metadata(c, &token, &data)?;
        undo::commit(c, &token)?;
        Ok::<(), String>(())
    })();
    if let Err(error) = result {
        // Reconcile only from observed objects. A surviving copy can be undone
        // explicitly without resuming the failed move or touching source edits.
        let row = c
            .store
            .query("SELECT * FROM undo_records WHERE id=?", &[json!(token)])?[0]
            .clone();
        let _ = recover(c, &row);
        return Err(format!("{error}; recovery_undo_id={token}"));
    }
    Ok(
        json!({"path":target,"original":original,"checkpoint":retained,"cross_volume":true,"undo_available":true,"undo_token":token}),
    )
}

pub(super) fn expected_state(c: &Core, row: &Value, control: &Control) -> Result<(), String> {
    match position(c, row, control)? {
        Position::Moved | Position::CopyOnly | Position::UndoParked => Ok(()),
        Position::Undone => Err("UNDO_ALREADY_RESTORED".into()),
    }
}
pub(super) fn restore(c: &Core, row: &Value, control: &Control) -> Result<(), String> {
    let current = position(c, row, control)?;
    if current == Position::Undone {
        return Ok(());
    }
    let mut data = metadata(row)?;
    let original = path(c, row, "path")?;
    let retained = path(c, row, "checkpoint")?;
    let target = path(c, row, "destination")?;
    let parked = meta_path(c, &data)?;
    let digest = row["expected_hash"].as_str().ok_or("UNDO_HASH")?;
    if current != Position::UndoParked {
        // A different operation may have consumed the quota since this journal
        // was prepared. Check again before retaining both objects in undo.
        let bytes = Tree::capture(c, &target, false, control)?.bytes;
        let _ = checkpoint(c, &target, bytes, control)?;
        data["phase"] = json!(if current == Position::CopyOnly {
            "undo_copy_only"
        } else {
            "undo_parking_target"
        });
        save_metadata(c, row["id"].as_str().ok_or("UNDO_ID")?, &data)?;
        relocate(
            c,
            &target,
            &parked,
            digest,
            row["object_id"].as_str().ok_or("CROSS_TARGET_ID")?,
            control,
        )?;
        #[cfg(all(test, windows))]
        crash_tests::at(c, "cross_undo_target_parked");
    }
    if current != Position::CopyOnly {
        data["phase"] = json!("undo_target_parked");
        save_metadata(c, row["id"].as_str().ok_or("UNDO_ID")?, &data)?;
        let parent = source_parent(c, row, control)?;
        relocate_bound(
            c,
            &retained,
            &original,
            digest,
            data["source_id"].as_str().ok_or("CROSS_SOURCE_ID")?,
            parent.as_deref(),
            control,
        )?;
    }
    data["phase"] = json!(if current == Position::CopyOnly {
        "undone_copy_only"
    } else {
        "undone"
    });
    save_metadata(c, row["id"].as_str().ok_or("UNDO_ID")?, &data)?;
    Ok(())
}

pub(super) fn recover(c: &Core, row: &Value) -> Result<(), String> {
    let state = match position(c, row, &Control::default()) {
        Ok(Position::Undone) => "undone",
        Ok(Position::Moved | Position::CopyOnly | Position::UndoParked) => "available",
        Err(_) => "blocked",
    };
    c.store.execute(
        "UPDATE undo_records SET state=?,updated_at=? WHERE id=?",
        &[json!(state), json!(storage::now()), row["id"].clone()],
    )?;
    Ok(())
}

#[cfg(all(test, windows))]
#[path = "file_cross_tests.rs"]
mod tests;
