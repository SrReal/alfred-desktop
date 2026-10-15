use super::*;
use crate::modules;
use std::sync::Arc;

fn fixture() -> Arc<Core> {
    let root = std::env::temp_dir().join(format!("ap-zip-{}", storage::id()));
    Arc::new(Core::new(root, PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap())
}
fn finish(c: Arc<Core>) {
    let root = c.root.clone();
    drop(c);
    assert!(
        root.starts_with(std::env::temp_dir())
            && root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ap-zip-")
    );
    std::fs::remove_dir_all(root).unwrap();
}
fn archive(c: &Core, names: &[(&str, &[u8])]) -> PathBuf {
    let path = c.root.join(format!("temp/{}.zip", storage::id()));
    let mut writer = ZipWriter::new(File::create(&path).unwrap());
    for (name, data) in names {
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        if name.ends_with('/') {
            writer.add_directory(*name, options).unwrap();
        } else {
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
    }
    writer.finish().unwrap();
    path
}
#[tokio::test]
async fn real_zip_merge_preserves_existing_objects_and_rolls_back_new_groups_after_restart() {
    let mut c = fixture();
    let path = archive(
        &c,
        &[
            ("existing/empty/", b""),
            ("existing/new.txt", b"new"),
            ("new/nested/a.bin", b"a"),
            ("top.bin", b"top"),
        ],
    );
    let destination = c.root.join("temp/merge");
    std::fs::create_dir_all(destination.join("existing")).unwrap();
    std::fs::write(destination.join("existing/keep.txt"), b"keep").unwrap();
    let before = hash(&c, &destination, &Control::default()).unwrap();
    let original_id = identity(&open(&destination, false, false).unwrap()).unwrap();
    let result = modules::dispatch(
        &c,
        "files.extract",
        &json!({"path":path,"destination":destination}),
    )
    .await
    .unwrap();
    assert_eq!(result["undo_tokens"].as_array().unwrap().len(), 4);
    let root = c.root.clone();
    drop(c);
    c = Arc::new(Core::new(root, PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap());
    let status = modules::dispatch(
        &c,
        "files.archive_status",
        &json!({"id":result["archive_batch_id"]}),
    )
    .await
    .unwrap();
    assert_eq!(status["batch"]["state"], "completed");
    assert_eq!(status["count"], 4);
    let rolled = modules::dispatch(
        &c,
        "files.rollback",
        &json!({"body":{"tokens":result["undo_tokens"]}}),
    )
    .await
    .unwrap();
    assert_eq!(rolled["state"], "completed");
    assert_eq!(hash(&c, &destination, &Control::default()).unwrap(), before);
    assert_eq!(
        identity(&open(&destination, false, false).unwrap()).unwrap(),
        original_id
    );
    println!("FILE_ZIP_MERGE_CONTRACT_PASS: existing objects/empty directories/nested groups/restart/durable rollback");
    finish(c);
}
#[tokio::test]
async fn zip_conflicts_and_unsafe_names_are_rejected_before_any_publication() {
    let c = fixture();
    let destination = c.root.join("temp/destination");
    std::fs::create_dir(&destination).unwrap();
    std::fs::write(destination.join("occupied.txt"), b"keep").unwrap();
    let before = hash(&c, &destination, &Control::default()).unwrap();
    for names in [
        vec![
            ("new.txt", b"new".as_slice()),
            ("occupied.txt", b"bad".as_slice()),
        ],
        vec![("../escape.txt", b"bad".as_slice())],
        vec![("A.txt", b"a".as_slice()), ("a.txt", b"b".as_slice())],
        vec![("CON.txt", b"bad".as_slice())],
        vec![("safe/file:stream", b"bad".as_slice())],
        vec![("a", b"file".as_slice()), ("a/b", b"child".as_slice())],
    ] {
        let path = archive(&c, &names);
        assert!(modules::dispatch(
            &c,
            "files.extract",
            &json!({"path":path,"destination":destination})
        )
        .await
        .is_err());
        assert_eq!(hash(&c, &destination, &Control::default()).unwrap(), before);
    }
    assert_eq!(
        c.store
            .query("SELECT count(*) AS n FROM undo_records", &[])
            .unwrap()[0]["n"],
        0
    );
    finish(c);
}
#[tokio::test]
async fn corrupt_crc_is_retained_in_owned_stage_and_never_published() {
    let c = fixture();
    let path = archive(&c, &[("first.txt", b"first"), ("bad.txt", b"payload")]);
    let mut bytes = std::fs::read(&path).unwrap();
    let payload = bytes.windows(7).position(|b| b == b"payload").unwrap();
    bytes[payload] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    let destination = c.root.join("temp/out");
    let error = modules::dispatch(
        &c,
        "files.extract",
        &json!({"path":path,"destination":destination}),
    )
    .await
    .unwrap_err();
    assert!(error.contains("recovery_undo_id="));
    assert!(!destination.exists());
    let row = c.store.query("SELECT * FROM undo_records", &[]).unwrap()[0].clone();
    assert_eq!(row["state"], "blocked");
    assert!(Path::new(row["staging"].as_str().unwrap()).exists());
    assert!(journal_metadata(&c, row["id"].as_str().unwrap()).unwrap()["stage_id"].is_string());
    finish(c);
}
#[tokio::test]
async fn zip_metadata_limits_duplicate_raw_names_and_symlinks_are_refused() {
    let c = fixture();
    let destination = c.root.join("temp/out");
    let path = archive(&c, &[("aaaa", b"a"), ("bbbb", b"b")]);
    let mut bytes = std::fs::read(&path).unwrap();
    for name in bytes
        .windows(4)
        .enumerate()
        .filter_map(|(i, b)| (b == b"bbbb").then_some(i))
        .collect::<Vec<_>>()
    {
        bytes[name..name + 4].copy_from_slice(b"aaaa");
    }
    std::fs::write(&path, bytes).unwrap();
    assert!(modules::dispatch(
        &c,
        "files.extract",
        &json!({"path":path,"destination":destination})
    )
    .await
    .unwrap_err()
    .contains("COUNT"));
    let path = archive(&c, &[("file", b"a")]);
    let mut bytes = std::fs::read(&path).unwrap();
    let end = bytes.len() - 22;
    bytes[end + 10..end + 12].copy_from_slice(&(MAX_NODES as u16).to_le_bytes());
    bytes[end + 8..end + 10].copy_from_slice(&(MAX_NODES as u16).to_le_bytes());
    std::fs::write(&path, bytes).unwrap();
    assert_eq!(
        modules::dispatch(
            &c,
            "files.extract",
            &json!({"path":path,"destination":destination})
        )
        .await
        .unwrap_err(),
        "ZIP_NODE_LIMIT"
    );
    let path = archive(&c, &[("link", b"target")]);
    let mut bytes = std::fs::read(&path).unwrap();
    let central = bytes
        .windows(4)
        .position(|b| b == [0x50, 0x4b, 0x01, 0x02])
        .unwrap();
    bytes[central + 5] = 3;
    bytes[central + 38..central + 42].copy_from_slice(&(0o120777u32 << 16).to_le_bytes());
    std::fs::write(&path, bytes).unwrap();
    assert_eq!(
        modules::dispatch(
            &c,
            "files.extract",
            &json!({"path":path,"destination":destination})
        )
        .await
        .unwrap_err(),
        "ZIP_LINK_OR_SPECIAL_FILE"
    );
    assert!(!destination.exists());
    finish(c);
}
#[tokio::test]
async fn zip_readonly_file_and_changed_output_are_preserved_by_undo() {
    let c = fixture();
    let source = c.root.join("temp/readonly.txt");
    std::fs::write(&source, b"read only").unwrap();
    let mut p = std::fs::metadata(&source).unwrap().permissions();
    p.set_readonly(true);
    std::fs::set_permissions(&source, p).unwrap();
    let path = c.root.join("temp/readonly.zip");
    modules::dispatch(
        &c,
        "files.compress",
        &json!({"path":source,"destination":path}),
    )
    .await
    .unwrap();
    let destination = c.root.join("temp/out");
    let result = modules::dispatch(
        &c,
        "files.extract",
        &json!({"path":path,"destination":destination}),
    )
    .await
    .unwrap();
    assert!(std::fs::metadata(destination.join("readonly.txt"))
        .unwrap()
        .permissions()
        .readonly());
    std::fs::write(destination.join("later.txt"), b"later edit").unwrap();
    assert!(undo::execute(&c, result["undo_token"].as_str().unwrap())
        .unwrap_err()
        .starts_with("UNDO_FILE_CHANGED"));
    assert!(destination.join("later.txt").exists());
    finish(c);
}

fn as_zip64(bytes: &[u8]) -> Vec<u8> {
    let end = bytes.len() - 22;
    let mut result = bytes[..end].to_vec();
    let mut record = [0u8; 56];
    record[..4].copy_from_slice(&[0x50, 0x4b, 0x06, 0x06]);
    record[4..12].copy_from_slice(&44u64.to_le_bytes());
    record[12..14].copy_from_slice(&45u16.to_le_bytes());
    record[14..16].copy_from_slice(&45u16.to_le_bytes());
    let count = u16_at(bytes, end + 10) as u64;
    record[24..32].copy_from_slice(&count.to_le_bytes());
    record[32..40].copy_from_slice(&count.to_le_bytes());
    record[40..48].copy_from_slice(&(u32_at(bytes, end + 12) as u64).to_le_bytes());
    record[48..56].copy_from_slice(&(u32_at(bytes, end + 16) as u64).to_le_bytes());
    result.extend(record);
    let mut locator = [0u8; 20];
    locator[..4].copy_from_slice(&[0x50, 0x4b, 0x06, 0x07]);
    locator[8..16].copy_from_slice(&(end as u64).to_le_bytes());
    locator[16..20].copy_from_slice(&1u32.to_le_bytes());
    result.extend(locator);
    let mut eocd = bytes[end..].to_vec();
    eocd[8..12].fill(0xff);
    eocd[12..20].fill(0xff);
    result.extend(eocd);
    result
}
#[tokio::test]
async fn zip64_is_supported_with_bounded_end_metadata_and_rejects_huge_counts() {
    let c = fixture();
    let path = archive(&c, &[("nested/file.txt", b"zip64")]);
    let bytes = as_zip64(&std::fs::read(&path).unwrap());
    std::fs::write(&path, &bytes).unwrap();
    let destination = c.root.join("temp/zip64");
    let result = modules::dispatch(
        &c,
        "files.extract",
        &json!({"path":path,"destination":destination}),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read(destination.join("nested/file.txt")).unwrap(),
        b"zip64"
    );
    undo::execute(&c, result["undo_token"].as_str().unwrap()).unwrap();
    let record = bytes.len() - 22 - 20 - 56;
    let mut bad = bytes.clone();
    bad[record + 24..record + 40].fill(0xff);
    std::fs::write(&path, bad).unwrap();
    assert_eq!(
        modules::dispatch(
            &c,
            "files.extract",
            &json!({"path":path,"destination":destination})
        )
        .await
        .unwrap_err(),
        "ZIP_NODE_LIMIT"
    );
    let mut bad = bytes;
    bad[record + 4..record + 12].copy_from_slice(&(MAX_ZIP64_END as u64 + 1).to_le_bytes());
    std::fs::write(&path, bad).unwrap();
    assert_eq!(
        modules::dispatch(
            &c,
            "files.extract",
            &json!({"path":path,"destination":destination})
        )
        .await
        .unwrap_err(),
        "ZIP64_END_LIMIT_OR_INVALID"
    );
    assert!(!destination.exists());
    finish(c);
}
#[tokio::test]
async fn invalid_latest_directory_cannot_fall_back_to_unbounded_embedded_zip64_metadata() {
    let c = fixture();
    let fake = as_zip64(&{
        let mut eocd = vec![0; 22];
        eocd[..4].copy_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
        eocd
    });
    let path = archive(&c, &[("payload.bin", &fake)]);
    let mut bytes = std::fs::read(&path).unwrap();
    let central = bytes
        .windows(4)
        .position(|b| b == [0x50, 0x4b, 0x01, 0x02])
        .unwrap();
    bytes[central] = 0;
    std::fs::write(&path, bytes).unwrap();
    let destination = c.root.join("temp/out");
    let error = modules::dispatch(
        &c,
        "files.extract",
        &json!({"path":path,"destination":destination}),
    )
    .await
    .unwrap_err();
    assert!(error.contains("ZIP_AMBIGUOUS_END"), "{error}");
    assert!(!destination.exists());
    finish(c);
}
#[tokio::test]
async fn partial_archive_publication_reconciles_child_and_keeps_unpublished_stage() {
    let mut c = fixture();
    let path = archive(&c, &[("a.txt", b"a"), ("b.txt", b"b")]);
    let destination = c.root.join("temp/existing");
    std::fs::create_dir(&destination).unwrap();
    let batch =
        undo::prepare_tree(&c, "archive_batch", &path, Some(&destination), None, "").unwrap();
    let stage = stage(&c, &destination, &batch, &path).unwrap();
    std::fs::create_dir(&stage).unwrap();
    let root = open(&stage, false, true).unwrap();
    record_stage(&c, &batch, &root).unwrap();
    drop(root);
    let first = stage.join("a.txt");
    std::fs::write(&first, b"a").unwrap();
    std::fs::write(stage.join("b.txt"), b"b").unwrap();
    let output = Tree::capture(&c, &first, false, &Control::default()).unwrap();
    let target = destination.join("a.txt");
    let retained = checkpoint_path(&c, &target).unwrap();
    let child = undo::prepare_tree(
        &c,
        "tree_copy",
        &path,
        Some(&target),
        Some(&retained),
        &output.digest,
    )
    .unwrap();
    c.store
        .execute(
            "UPDATE undo_records SET staging=? WHERE id=?",
            &[json!(first), json!(child)],
        )
        .unwrap();
    record_stage(&c, &child, &output.root).unwrap();
    ready(&c, &child, &output, "zip_extract", Some((&batch, 0))).unwrap();
    let digest = output.digest.clone();
    let id = output.identity.clone();
    drop(output);
    relocate(&c, &first, &target, &digest, &id, &Control::default()).unwrap();
    // Observe the boundary after rename and before journal commit at a new Core.
    let root = c.root.clone();
    drop(c);
    c = Arc::new(Core::new(root, PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap());
    let status = status(&c, &batch, 0).unwrap();
    assert_eq!(status["batch"]["state"], "blocked");
    assert_eq!(status["operations"][0]["state"], "available");
    assert_eq!(std::fs::read(stage.join("b.txt")).unwrap(), b"b");
    undo::execute(&c, &child).unwrap();
    assert!(!target.exists());
    assert_eq!(std::fs::read(stage.join("b.txt")).unwrap(), b"b");
    assert_eq!(status["next_offset"], Value::Null);
    assert_eq!(
        super::status(&c, &storage::id(), 0).unwrap_err(),
        "ARCHIVE_BATCH_NOT_FOUND"
    );
    finish(c);
}

#[tokio::test]
async fn real_extract_here_in_an_isolated_volume_root_preserves_archive_and_existing_files() {
    let c = fixture();
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".work")
        .canonicalize()
        .unwrap();
    let destination = workspace.join(format!("zip-volume-{}", storage::id()));
    std::fs::create_dir(&destination).unwrap();
    assert_ne!(
        volume(&open(&destination, false, true).unwrap()).unwrap(),
        volume(&open(&c.root.join("temp"), false, true).unwrap()).unwrap()
    );
    let built = archive(
        &c,
        &[
            ("nested/empty/", b""),
            ("nested/new.txt", b"new"),
            ("top.txt", b"top"),
        ],
    );
    let path = destination.join("archive.zip");
    std::fs::copy(built, &path).unwrap();
    std::fs::write(destination.join("keep.txt"), b"keep").unwrap();
    c.store
        .set_setting("allowed_roots", &json!([destination]))
        .unwrap();
    let before = hash(&c, &destination, &Control::default()).unwrap();
    let id = identity(&open(&destination, false, false).unwrap()).unwrap();
    let archive_bytes = std::fs::read(&path).unwrap();
    let result = modules::dispatch(
        &c,
        "files.extract",
        &json!({"path":path,"destination":destination}),
    )
    .await
    .unwrap();
    assert!(destination.join("nested/empty").is_dir());
    assert_eq!(std::fs::read(&path).unwrap(), archive_bytes);
    assert_eq!(
        std::fs::read(destination.join("keep.txt")).unwrap(),
        b"keep"
    );
    assert!(c
        .path(workspace.to_str().unwrap(), false)
        .unwrap_err()
        .starts_with("PATH_SCOPE"));
    modules::dispatch(
        &c,
        "files.rollback",
        &json!({"body":{"tokens":result["undo_tokens"]}}),
    )
    .await
    .unwrap();
    let review = modules::dispatch(
        &c,
        "files.cleanup",
        &json!({"body":{"action":"preview","ids":result["undo_tokens"]}}),
    )
    .await
    .unwrap();
    modules::dispatch(
        &c,
        "files.cleanup",
        &json!({"body":{"action":"apply","plan_id":review["plan_id"]}}),
    )
    .await
    .unwrap();
    assert_eq!(hash(&c, &destination, &Control::default()).unwrap(), before);
    assert_eq!(
        identity(&open(&destination, false, false).unwrap()).unwrap(),
        id
    );
    assert!(destination.canonicalize().unwrap().starts_with(&workspace));
    assert!(destination
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("zip-volume-"));
    finish(c);
    std::fs::remove_dir_all(destination).unwrap();
    println!("FILE_ZIP_HERE_CONTRACT_PASS: actual D isolated root/C profile/in-place archive/staging/undo/cleanup/unchanged existing contents");
}
#[tokio::test]
async fn real_zip_nested_empty_unicode_roundtrip_and_restart_undo() {
    let mut c = fixture();
    let source = c.root.join("temp/source");
    std::fs::create_dir_all(source.join("nested/empty")).unwrap();
    std::fs::write(source.join("nested/ñ.txt"), "Contenido sintético").unwrap();
    std::fs::write(source.join("zero.bin"), []).unwrap();
    let before = hash(&c, &source, &Control::default()).unwrap();
    let archive = c.root.join("temp/archive.zip");
    let compressed = modules::dispatch(
        &c,
        "files.compress",
        &json!({"path":source,"destination":archive}),
    )
    .await
    .unwrap();
    let destination = c.root.join("temp/extracted");
    let extracted = modules::dispatch(
        &c,
        "files.extract",
        &json!({"path":archive,"destination":destination}),
    )
    .await
    .unwrap();
    assert_eq!(
        hash(&c, &destination.join("source"), &Control::default()).unwrap(),
        before
    );
    let root = c.root.clone();
    drop(c);
    c = Arc::new(Core::new(root, PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap());
    undo::execute(&c, extracted["undo_token"].as_str().unwrap()).unwrap();
    assert!(!destination.exists());
    undo::execute(&c, compressed["undo_token"].as_str().unwrap()).unwrap();
    assert!(!archive.exists());
    assert_eq!(hash(&c, &source, &Control::default()).unwrap(), before);
    println!("FILE_ZIP_CONTRACT_PASS: nested/empty/unicode/compress/extract/restart/undo");
    finish(c);
}
