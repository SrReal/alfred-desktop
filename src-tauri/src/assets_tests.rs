use super::*;
use std::{os::windows::fs::MetadataExt, path::Path};
struct Fixture {
    core: Arc<Core>,
    id: String,
    root: PathBuf,
    object_id: String,
}
impl Fixture {
    fn new() -> Self {
        Self::build(false)
    }
    fn build(legacy: bool) -> Self {
        let id = storage::id();
        let root = std::env::temp_dir().join(format!("ap-assets-{id}"));
        std::fs::create_dir(&root).unwrap();
        let object_id = files::identity(&files::directory(&root).unwrap()).unwrap();
        if legacy {
            std::fs::create_dir(root.join("data")).unwrap();
            let db = rusqlite::Connection::open(root.join("data/alfredpet.db")).unwrap();
            for sql in [
                include_str!("../migrations/001_initial.sql"),
                include_str!("../migrations/002_questions.sql"),
                include_str!("../migrations/003_undo.sql"),
                include_str!("../migrations/004_watches.sql"),
                include_str!("../migrations/005_memory_analysis.sql"),
                include_str!("../migrations/006_semantic_memory.sql"),
                include_str!("../migrations/007_file_rollbacks.sql"),
                include_str!("../migrations/008_file_recovery.sql"),
            ] {
                db.execute_batch(sql).unwrap();
            }
            let conversation = storage::id();
            db.execute("INSERT INTO conversations(id,title,created_at,updated_at) VALUES(?,'Legacy assets fixture',?,?)",rusqlite::params![conversation,storage::now(),storage::now()]).unwrap();
            let path = root.join("legacy.png");
            std::fs::write(&path, png()).unwrap();
            db.execute(
                "INSERT INTO attachments VALUES(?,?,?,'image/png',?,?,?)",
                rusqlite::params![
                    storage::id(),
                    conversation,
                    path.to_str().unwrap(),
                    digest(&png()),
                    png().len() as i64,
                    storage::now()
                ],
            )
            .unwrap();
            assert_eq!(
                db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                8
            );
        }
        let core = Arc::new(
            Core::new(
                root.clone(),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        core.store
            .set_setting("allowed_roots", &json!([root]))
            .unwrap();
        Self {
            core,
            id,
            root,
            object_id,
        }
    }
    fn conversation(&self) -> String {
        let id = storage::id();
        self.core.store.execute("INSERT INTO conversations(id,title,created_at,updated_at) VALUES(?,'Owned assets fixture',?,?)",&[json!(id),json!(storage::now()),json!(storage::now())]).unwrap();
        id
    }
    fn finish(self) {
        assert_eq!(
            self.root.parent().unwrap().canonicalize().unwrap(),
            std::env::temp_dir().canonicalize().unwrap()
        );
        assert_eq!(
            self.root.file_name().unwrap().to_string_lossy(),
            format!("ap-assets-{}", self.id)
        );
        assert_eq!(
            files::identity(&files::directory(&self.root).unwrap()).unwrap(),
            self.object_id
        );
        let mut todo = vec![self.root.clone()];
        while let Some(p) = todo.pop() {
            assert_eq!(
                std::fs::symlink_metadata(&p).unwrap().file_attributes() & 0x400,
                0
            );
            if p.is_dir() {
                todo.extend(std::fs::read_dir(&p).unwrap().map(|v| v.unwrap().path()));
            }
        }
        drop(self.core);
        std::fs::remove_dir_all(self.root).unwrap();
    }
}
fn png() -> Vec<u8> {
    let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        16,
        9,
        image::Rgba([20, 70, 190, 255]),
    ));
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}
fn owned(c: &Core, class: &str) -> (Record, PathBuf) {
    let lease = Lease::new(c, false, false).unwrap();
    let p = Profile::get(c, true).unwrap();
    let r = store(c, &p, &png(), class, "Own fixture", &lease).unwrap();
    let path = p.original(&r);
    (r, path)
}
#[tokio::test]
async fn image_viewer_returns_verified_original_and_respects_privacy_and_file_identity() {
    let f = Fixture::new();
    let (record, path) = owned(&f.core, "attachment");
    let image = ui(&f.core, "image", &json!({"id":record.id}))
        .await
        .unwrap();
    assert_eq!(
        image["data_url"],
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png())
        )
    );
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(
        ui(&f.core, "image", &json!({"id":record.id}))
            .await
            .unwrap_err(),
        "PRIVACY_DENIED"
    );
    f.core.store.set_setting("privacy", &json!(false)).unwrap();
    std::fs::write(&path, b"replaced image").unwrap();
    assert!(ui(&f.core, "image", &json!({"id":record.id}))
        .await
        .is_err());
    f.finish();
}
#[tokio::test]
async fn outgoing_images_wait_for_preview_and_keep_new_previews_queued_until_transfer_finishes() {
    let f = Fixture::new();
    let conversation = f.conversation();
    let (record, _) = owned(&f.core, "attachment");
    attach(
        &f.core,
        &json!({"conversation_id":conversation,"asset_id":record.id}),
    )
    .await
    .unwrap();
    let action = ACTIONS.lock().await;
    let preview_profile = Profile::get(&f.core, false).unwrap();
    let c = f.core.clone();
    let id = conversation.clone();
    let mut send = tokio::spawn(async move {
        let (action, images) = verified_images_for_turn(&c, &id).await?;
        images.check()?;
        let count = images.iter().count();
        drop(images);
        drop(action);
        Ok::<_, String>(count)
    });
    assert!(tokio::time::timeout(Duration::from_millis(50), &mut send)
        .await
        .is_err());
    drop(preview_profile);
    drop(action);
    assert_eq!(send.await.unwrap().unwrap(), 1);
    let (action, images) = verified_images_for_turn(&f.core, &conversation)
        .await
        .unwrap();
    let c = f.core.clone();
    let id = record.id;
    let mut preview = tokio::spawn(async move { ui(&c, "image", &json!({"id":id})).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut preview)
            .await
            .is_err()
    );
    images.check().unwrap();
    drop(images);
    drop(action);
    assert!(preview.await.unwrap().unwrap()["data_url"].is_string());
    f.finish();
}
#[tokio::test]
async fn legacy_attachment_presentation_finds_its_message_but_leaves_unsent_files_pending() {
    let f = Fixture::new();
    let conversation = f.conversation();
    let (record, _) = owned(&f.core, "attachment");
    attach(
        &f.core,
        &json!({"conversation_id":conversation,"asset_id":record.id}),
    )
    .await
    .unwrap();
    let message = storage::id();
    f.core
        .store
        .execute(
            "INSERT INTO messages VALUES(?,?,'user','With image','complete',?)",
            &[json!(message), json!(conversation), json!(storage::now())],
        )
        .unwrap();
    let linked = attachments(&f.core, &conversation).unwrap();
    assert_eq!(linked[0]["message_id"], message);
    assert!(f
        .core
        .store
        .query(
            "SELECT message_id FROM attachments WHERE conversation_id=?",
            &[json!(conversation)]
        )
        .unwrap()[0]["message_id"]
        .is_null());
    let (pending, _) = owned(&f.core, "attachment");
    attach(
        &f.core,
        &json!({"conversation_id":conversation,"asset_id":pending.id}),
    )
    .await
    .unwrap();
    let linked = attachments(&f.core, &conversation).unwrap();
    assert!(linked
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["asset_id"] == pending.id)
        .unwrap()["message_id"]
        .is_null());
    f.finish();
}
#[tokio::test]
async fn private_assets_expire_after_restart_but_attached_and_unregistered_files_survive() {
    let f = Fixture::new();
    let conversation = f.conversation();
    let (kept, kept_path) = owned(&f.core, "capture");
    let attached = attach(
        &f.core,
        &json!({"conversation_id":conversation,"asset_id":kept.id}),
    )
    .await
    .unwrap();
    assert!(attached["id"].is_string());
    let (expired, path) = owned(&f.core, "capture");
    let p = Profile::get(&f.core, false).unwrap();
    let thumb = p.thumb(&expired);
    let file = files::open(&path, false).unwrap();
    files::private(&file).unwrap();
    assert_eq!(files::identity(&file).unwrap(), expired.original.object_id);
    drop(file);
    let thumbnail = image::open(&thumb).unwrap();
    assert_eq!((thumbnail.width(), thumbnail.height()), (16, 9));
    let foreign = p.path.join("unregistered-file.data");
    std::fs::write(&foreign, b"owned foreign fixture, never adopted").unwrap();
    drop(p);
    f.core
        .store
        .execute(
            "UPDATE asset_records SET expires_at='2000-01-01T00:00:00+00:00'",
            &[],
        )
        .unwrap();
    let restarted = Core::new(f.root.clone(), f.core.resource.clone()).unwrap();
    assert!(!path.exists() && !thumb.exists());
    assert!(kept_path.exists() && foreign.exists());
    assert_eq!(
        restarted
            .store
            .query(
                "SELECT state FROM asset_records WHERE id=?",
                &[json!(expired.id)]
            )
            .unwrap()[0]["state"],
        "expired"
    );
    assert_eq!(verified_images(&restarted, &conversation).unwrap().len(), 1);
    drop(restarted);
    f.finish();
}
#[test]
fn replacement_and_changed_thumbnail_block_whole_set_erasure() {
    let f = Fixture::new();
    let (r, path) = owned(&f.core, "capture");
    let lease = Lease::new(&f.core, false, false).unwrap();
    let p = Profile::get(&f.core, false).unwrap();
    let parked = p.path.join("owned-parked.png");
    std::fs::rename(&path, &parked).unwrap();
    let mut replacement = files::create(&path).unwrap();
    replacement.write_all(&png()).unwrap();
    replacement.sync_all().unwrap();
    drop(replacement);
    assert_eq!(
        erase_record(&f.core, &p, &r.id, &lease).unwrap_err(),
        "ASSET_OBJECT_CHANGED"
    );
    assert!(p.thumb(&r).exists() && parked.exists());
    let file = files::open(&path, true).unwrap();
    files::erase(&file).unwrap();
    drop(file);
    std::fs::rename(&parked, &path).unwrap();
    let thumb = p.thumb(&r);
    std::fs::write(&thumb, b"owned later thumbnail writer").unwrap();
    assert_eq!(
        erase_record(&f.core, &p, &r.id, &lease).unwrap_err(),
        "ASSET_CONTENT_CHANGED"
    );
    assert_eq!(std::fs::read(&path).unwrap(), png());
    assert_eq!(
        std::fs::read(&thumb).unwrap(),
        b"owned later thumbnail writer"
    );
    drop(p);
    drop(lease);
    f.finish();
}
#[test]
fn private_acl_read_locks_and_real_byte_limits_are_enforced() {
    let f = Fixture::new();
    let (r, path) = owned(&f.core, "attachment");
    let file = files::open(&path, false).unwrap();
    files::private(&file).unwrap();
    assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
    assert!(std::fs::rename(&path, f.root.join("moved.data")).is_err());
    assert_eq!(files::read(&file, 1).unwrap_err(), "ASSET_BYTE_LIMIT");
    assert_eq!(files::identity(&file).unwrap(), r.original.object_id);
    drop(file);
    let policy = config(&f.core).unwrap();
    assert_eq!(policy.ttl_seconds, 86400);
    assert!(validate_config(
        &json!({"ttl_seconds":1,"max_bytes":policy.max_bytes,"cleanup_enabled":true})
    )
    .is_err());
    assert!(validate_config(&json!({"ttl_seconds":86400,"max_bytes":policy.max_bytes,"cleanup_enabled":true,"extra":true})).is_err());
    f.finish();
}
#[test]
fn malformed_and_oversized_image_headers_never_publish_assets() {
    let f = Fixture::new();
    let lease = Lease::new(&f.core, false, false).unwrap();
    let p = Profile::get(&f.core, true).unwrap();
    assert!(store(
        &f.core,
        &p,
        b"\x89PNG\r\n\x1a\n",
        "capture",
        "broken",
        &lease
    )
    .unwrap_err()
    .contains("ASSET_IMAGE_DECODE"));
    let mut gif = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        2,
        2,
        image::Rgba([0, 0, 0, 255]),
    ))
    .write_to(&mut gif, image::ImageFormat::Gif)
    .unwrap();
    let mut bytes = gif.into_inner();
    bytes[6..8].copy_from_slice(&32767u16.to_le_bytes());
    bytes[8..10].copy_from_slice(&32767u16.to_le_bytes());
    assert!(image_info(&bytes).is_err());
    assert_eq!(
        f.core
            .store
            .query("SELECT COUNT(*) AS n FROM asset_records", &[])
            .unwrap()[0]["n"],
        0
    );
    drop(p);
    drop(lease);
    f.finish();
}
#[tokio::test]
async fn attachment_payload_checks_legacy_hash_privacy_and_limits() {
    let f = Fixture::new();
    let conversation = f.conversation();
    let path = f.root.join("legacy.png");
    std::fs::write(&path, png()).unwrap();
    f.core.store.execute("INSERT INTO attachments(id,conversation_id,path,mime,sha256,size,created_at) VALUES(?,?,?,'image/png',?,?,?)",&[json!(storage::id()),json!(conversation),json!(path),json!(digest(&png())),json!(png().len()),json!(storage::now())]).unwrap();
    assert_eq!(verified_images(&f.core, &conversation).unwrap()[0].1, png());
    std::fs::write(&path, b"owned later image writer").unwrap();
    assert_eq!(
        verified_images(&f.core, &conversation).unwrap_err(),
        "ATTACHMENT_CONTENT_CHANGED"
    );
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert!(verified_images(&f.core, &conversation).unwrap().is_empty());
    f.core.store.set_setting("privacy", &json!(false)).unwrap();
    let conversation = f.conversation();
    let input = f.root.join("source.png");
    std::fs::write(&input, png()).unwrap();
    for _ in 0..6 {
        attach(
            &f.core,
            &json!({"conversation_id":conversation,"path":input}),
        )
        .await
        .unwrap();
    }
    assert!(verified_images(&f.core, &conversation)
        .unwrap_err()
        .contains("ATTACHMENT_IMAGE_LIMIT"));
    f.finish();
}
#[tokio::test]
async fn attached_assets_need_explicit_unlink_and_legacy_files_are_retained() {
    let f = Fixture::new();
    let conversation = f.conversation();
    let source = f.root.join("source.png");
    std::fs::write(&source, png()).unwrap();
    let item = attach(
        &f.core,
        &json!({"conversation_id":conversation,"path":source}),
    )
    .await
    .unwrap();
    let lease = Lease::new(&f.core, false, false).unwrap();
    let p = Profile::get(&f.core, false).unwrap();
    assert_eq!(
        erase_record(&f.core, &p, item["asset_id"].as_str().unwrap(), &lease).unwrap_err(),
        "ASSET_STILL_ATTACHED"
    );
    drop(p);
    drop(lease);
    let input = json!({"conversation_id":conversation,"id":item["id"]});
    let expected = attachment_review(&f.core, &input).unwrap();
    let result = remove_attachment(&f.core, &input, &expected).await.unwrap();
    assert_eq!(result["asset_cleanup"]["state"], "expired");
    assert!(source.exists());
    assert!(!Path::new(item["path"].as_str().unwrap()).exists());
    assert!(attachments(&f.core, &conversation)
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    let legacy_id = storage::id();
    f.core.store.execute("INSERT INTO attachments(id,conversation_id,path,mime,sha256,size,created_at) VALUES(?,?,?,'image/png',?,?,?)",&[json!(legacy_id),json!(conversation),json!(source),json!(digest(&png())),json!(png().len()),json!(storage::now())]).unwrap();
    let input = json!({"conversation_id":conversation,"id":legacy_id});
    let expected = attachment_review(&f.core, &input).unwrap();
    assert_eq!(
        remove_attachment(&f.core, &input, &expected).await.unwrap()["legacy_file_retained"],
        true
    );
    assert_eq!(std::fs::read(&source).unwrap(), png());
    f.finish();
}

#[tokio::test]
async fn genuine_schema8_upgrade_keeps_legacy_data_and_restorable_backup() {
    let f = Fixture::build(true);
    assert_eq!(
        f.core.store.query("PRAGMA user_version", &[]).unwrap()[0]["user_version"],
        crate::storage::SCHEMA_VERSION
    );
    let row = f
        .core
        .store
        .query("SELECT * FROM attachments", &[])
        .unwrap()[0]
        .clone();
    assert!(row["asset_id"].is_null() && row["message_id"].is_null());
    assert_eq!(row["ephemeral"], 0);
    assert_eq!(row["metadata_json"], "{}");
    assert_eq!(
        verified_images(&f.core, row["conversation_id"].as_str().unwrap()).unwrap()[0].1,
        png()
    );
    let backup = std::fs::read_dir(f.root.join("data"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|p| p.file_name().unwrap().to_string_lossy().contains("pre-v9-"))
        .unwrap();
    let db =
        rusqlite::Connection::open_with_flags(backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        8
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM attachments", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.prepare("PRAGMA table_info(attachments)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .count(),
        7
    );
    drop(db);
    f.finish();
}

#[tokio::test]
async fn payload_holds_identity_and_scope_until_send_and_detects_revocation() {
    let f = Fixture::new();
    let conversation = f.conversation();
    let source = f.root.join("source.png");
    std::fs::write(&source, png()).unwrap();
    let item = attach(
        &f.core,
        &json!({"conversation_id":conversation,"path":source}),
    )
    .await
    .unwrap();
    let batch = verified_images(&f.core, &conversation).unwrap();
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(item["path"].as_str().unwrap())
        .is_err());
    assert!(Profile::get(&f.core, false).is_err());
    batch.check().unwrap();
    cancel(&f.core).unwrap();
    assert!(batch.cancellation().is_cancelled());
    assert_eq!(batch.check().unwrap_err(), "ASSET_CANCELLED");
    drop(batch);
    let batch = verified_images(&f.core, &conversation).unwrap();
    f.core
        .store
        .execute(
            "UPDATE attachments SET sha256='later owned metadata change' WHERE id=?",
            &[item["id"].clone()],
        )
        .unwrap();
    assert_eq!(batch.check().unwrap_err(), "ATTACHMENT_RECORD_CHANGED");
    drop(batch);
    let conversation = f.conversation();
    f.core.store.execute("INSERT INTO attachments(id,conversation_id,path,mime,sha256,size,created_at) VALUES(?,?,?,'image/png',?,?,?)",&[json!(storage::id()),json!(conversation),json!(source),json!(digest(&png())),json!(png().len()),json!(storage::now())]).unwrap();
    let batch = verified_images(&f.core, &conversation).unwrap();
    f.core
        .store
        .set_setting("allowed_roots", &json!([]))
        .unwrap();
    assert!(batch.check().unwrap_err().starts_with("PATH_SCOPE"));
    drop(batch);
    f.finish();
}

#[tokio::test]
async fn interrupted_creation_deletion_and_database_failures_preserve_unknown_bytes() {
    let f = Fixture::new();
    let (creating, original) = owned(&f.core, "capture");
    f.core
        .store
        .execute(
            "UPDATE asset_records SET state='creating' WHERE id=?",
            &[json!(creating.id)],
        )
        .unwrap();
    let (deleting, path) = owned(&f.core, "capture");
    f.core
        .store
        .execute(
            "UPDATE asset_records SET state='deleting' WHERE id=?",
            &[json!(deleting.id)],
        )
        .unwrap();
    let file = files::open(&path, true).unwrap();
    files::erase(&file).unwrap();
    drop(file);
    let restarted = Core::new(f.root.clone(), f.core.resource.clone()).unwrap();
    assert!(original.exists());
    let states = restarted
        .store
        .query("SELECT id,state,error FROM asset_records ORDER BY id", &[])
        .unwrap();
    assert!(states
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["id"] == creating.id
            && r["state"] == "blocked"
            && r["error"] == "ASSET_CREATION_INTERRUPTED"));
    assert!(states
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["id"] == deleting.id && r["state"] == "expired"));
    assert!(restarted.store.setting("asset_lifecycle_error").is_null());
    drop(restarted);
    f.core.store.execute("CREATE TRIGGER own_asset_failure BEFORE INSERT ON asset_records BEGIN SELECT RAISE(ABORT,'OWN_ASSET_SQL_FAILURE'); END",&[]).unwrap();
    let p = Profile::get(&f.core, false).unwrap();
    let before = std::fs::read_dir(&p.path).unwrap().count();
    let lease = Lease::new(&f.core, false, false).unwrap();
    assert!(store(&f.core, &p, &png(), "capture", "fixture", &lease)
        .unwrap_err()
        .contains("OWN_ASSET_SQL_FAILURE"));
    assert_eq!(std::fs::read_dir(&p.path).unwrap().count(), before);
    drop(p);
    drop(lease);
    f.finish();
}

#[test]
fn changed_namespace_and_hard_links_never_get_adopted() {
    let f = Fixture::new();
    let (r, path) = owned(&f.core, "capture");
    let ordinary = f.root.join("ordinary-private-check.data");
    std::fs::write(&ordinary, b"own fixture").unwrap();
    assert!(files::private(&files::open(&ordinary, false).unwrap()).is_err());
    let alias = f.root.join("owned-alias.png");
    std::fs::hard_link(&path, &alias).unwrap();
    assert!(files::open(&path, false).is_err());
    let p = Profile::get(&f.core, false).unwrap();
    let lease = Lease::new(&f.core, false, false).unwrap();
    assert!(erase_record(&f.core, &p, &r.id, &lease).is_err());
    assert!(alias.exists() && p.thumb(&r).exists());
    drop(p);
    drop(lease);
    let foreign = f.root.join("temp/unregistered.png");
    std::fs::write(&foreign, png()).unwrap();
    f.core
        .store
        .set_setting(ROOT_KEY, &json!({"id":"../foreign"}))
        .unwrap();
    assert_eq!(startup(&f.core).unwrap_err(), "ASSET_ROOT_JSON");
    assert_eq!(std::fs::read(&foreign).unwrap(), png());
    f.finish();
}

#[tokio::test]
async fn reviewed_capture_policy_drift_is_rejected_before_native_execution() {
    let f = Fixture::new();
    let reviewed = config(&f.core).unwrap();
    let mut changed = reviewed.clone();
    changed.ttl_seconds = 600;
    f.core
        .store
        .set_setting("asset_lifecycle", &json!(changed))
        .unwrap();
    assert_eq!(
        dispatch(
            &f.core,
            "vision.capture_window",
            &json!({"hwnd":1,"_asset_policy":reviewed})
        )
        .await
        .unwrap_err(),
        "ASSET_POLICY_CHANGED_REVIEW_REQUIRED"
    );
    assert!(f.core.store.setting(ROOT_KEY).is_null());
    let conversation = f.conversation();
    assert_eq!(
        attach(
            &f.core,
            &json!({"conversation_id":conversation,"path":"x","asset_id":storage::id()})
        )
        .await
        .unwrap_err(),
        "ATTACHMENT_INPUT"
    );
    f.finish();
}

#[tokio::test]
async fn destructive_reviews_detect_new_references_and_audits_omit_image_bytes() {
    let f = Fixture::new();
    let (r, path) = owned(&f.core, "capture");
    let input = json!({"id":r.id});
    let reviewed = review(&f.core, "vision.asset_delete", &input)
        .await
        .unwrap();
    assert_eq!(reviewed["assets"][0]["asset"]["path"], json!(path));
    assert_eq!(reviewed["assets"][0]["asset"]["label"], "Own fixture");
    let conversation = f.conversation();
    attach(
        &f.core,
        &json!({"conversation_id":conversation,"asset_id":r.id}),
    )
    .await
    .unwrap();
    assert_eq!(
        dispatch(
            &f.core,
            "vision.asset_delete",
            &json!({"id":r.id,"_asset_review":reviewed})
        )
        .await
        .unwrap_err(),
        "ASSET_CHANGED_REVIEW_REQUIRED"
    );
    assert!(path.exists());
    let result = dispatch(&f.core, "vision.asset_get", &input).await.unwrap();
    f.core
        .store
        .audit("vision.asset_get", "Free", &input, "succeeded", &result, 0)
        .unwrap();
    let audit = f
        .core
        .store
        .query(
            "SELECT output_summary FROM activity_events WHERE tool_id='vision.asset_get'",
            &[],
        )
        .unwrap()[0]["output_summary"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(audit.contains("VISUAL_BYTES_OMITTED"));
    assert!(!audit.contains("data:image"));
    f.core
        .store
        .execute(
            "UPDATE asset_records SET state='deleting' WHERE id=?",
            &[json!(r.id)],
        )
        .unwrap();
    assert!(f.core.store.execute("INSERT INTO attachments(id,conversation_id,path,mime,sha256,size,created_at,asset_id) VALUES(?,?,?,'image/png',?,?,?,?)",&[json!(storage::id()),json!(conversation),json!(path),json!(r.original.sha256),json!(r.original.size),json!(storage::now()),json!(r.id)]).unwrap_err().contains("ASSET_NOT_READY"));
    f.finish();
}

#[test]
fn bounded_cleanup_cursor_reaches_later_assets_after_repeated_pending_deletions() {
    let f = Fixture::new();
    let p = Profile::get(&f.core, true).unwrap();
    let lease = Lease::new(&f.core, false, false).unwrap();
    for _ in 0..100 {
        let r = store(
            &f.core,
            &p,
            &png(),
            "capture",
            "Owned blocked cleanup",
            &lease,
        )
        .unwrap();
        f.core
            .store
            .execute(
                "UPDATE asset_records SET state='deleting' WHERE id=?",
                &[json!(r.id)],
            )
            .unwrap();
        std::fs::write(p.thumb(&r), b"own changed thumbnail must be retained").unwrap();
    }
    let last = store(
        &f.core,
        &p,
        &png(),
        "capture",
        "Owned later expired",
        &lease,
    )
    .unwrap();
    let last_path = p.original(&last);
    f.core
        .store
        .execute(
            "UPDATE asset_records SET expires_at='2000-01-01T00:00:00+00:00' WHERE id=?",
            &[json!(last.id)],
        )
        .unwrap();
    let first = cleanup(&f.core, &p, &lease, &storage::now()).unwrap();
    assert_eq!(first["examined"], 100);
    assert_eq!(first["expired"], 0);
    assert!(last_path.exists());
    let next = cleanup(&f.core, &p, &lease, &storage::now()).unwrap();
    assert_eq!(next["examined"], 1);
    assert_eq!(next["expired"], 1);
    assert!(!last_path.exists());
    drop(p);
    drop(lease);
    f.finish();
}

#[tokio::test]
async fn oversized_managed_image_growth_is_rejected_before_reading_past_expected_bytes() {
    let f = Fixture::new();
    let conversation = f.conversation();
    let (r, path) = owned(&f.core, "capture");
    attach(
        &f.core,
        &json!({"conversation_id":conversation,"asset_id":r.id}),
    )
    .await
    .unwrap();
    let writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    writer.set_len(7 * 1024 * 1024).unwrap();
    writer.sync_all().unwrap();
    drop(writer);
    assert_eq!(
        verified_images(&f.core, &conversation).unwrap_err(),
        "ASSET_BYTE_LIMIT"
    );
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 7 * 1024 * 1024);
    f.finish();
}

#[tokio::test]
async fn text_attachments_preserve_document_reading_and_original_name_metadata() {
    let f = Fixture::new();
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=1 WHERE module_id='documents'",
            &[],
        )
        .unwrap();
    let conversation = f.conversation();
    for suffix in ["txt", "MD", "json", "csv"] {
        let source = f.root.join(format!("owned-report.{suffix}"));
        let content = "Owned attachment data\n";
        std::fs::write(&source, content).unwrap();
        let item = attach(
            &f.core,
            &json!({"conversation_id":conversation,"path":source}),
        )
        .await
        .unwrap();
        assert_eq!(
            Path::new(item["path"].as_str().unwrap())
                .extension()
                .unwrap()
                .to_string_lossy(),
            suffix.to_ascii_lowercase()
        );
        let result = f
            .core
            .execute("documents.extract_text", json!({"path":item["path"]}))
            .await
            .unwrap();
        assert_eq!(result["text"], content);
        let meta = f
            .core
            .store
            .query(
                "SELECT metadata_json FROM attachments WHERE id=?",
                &[item["id"].clone()],
            )
            .unwrap()[0]["metadata_json"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            serde_json::from_str::<Value>(&meta).unwrap()["label"],
            source.file_name().unwrap().to_string_lossy().as_ref()
        );
        assert_eq!(std::fs::read_to_string(&source).unwrap(), content);
    }
    f.finish();
}

#[tokio::test]
async fn valid_text_with_image_magic_prefix_is_attached_as_text() {
    let f = Fixture::new();
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=1 WHERE module_id='documents'",
            &[],
        )
        .unwrap();
    let conversation = f.conversation();
    for (name, text) in [
        ("prefix.txt", "BM es una abreviatura de texto\n"),
        ("prefix.MD", "GIF89a es una cabecera descrita aquí\n"),
        ("prefix.csv", "BM,description\n1,fixture\n"),
    ] {
        let source = f.root.join(name);
        std::fs::write(&source, text).unwrap();
        let added = attach(
            &f.core,
            &json!({"conversation_id":conversation,"path":source}),
        )
        .await
        .unwrap();
        let path = Path::new(added["path"].as_str().unwrap());
        assert_eq!(std::fs::read_to_string(path).unwrap(), text);
        let extracted = f
            .core
            .execute("documents.extract_text", json!({"path":path}))
            .await
            .unwrap();
        assert_eq!(extracted["text"], text);
        assert_eq!(added["mime"], "application/octet-stream");
        assert_eq!(std::fs::read_to_string(source).unwrap(), text);
    }
    f.finish();
}

#[tokio::test]
async fn capture_origins_work_without_vision_and_are_rechecked_for_preview_attach_and_send() {
    let mut f = Fixture::new();
    let native = f.root.join("temp/owned-capture.ps1");
    let encoded = base64::engine::general_purpose::STANDARD.encode(png());
    std::fs::write(&native, format!("$r=[Console]::In.ReadToEnd() | ConvertFrom-Json;if($r.input.output){{throw 'unexpected path'}};if(-not $r.input.return_png){{throw 'expected memory capture'}};'{{\"ok\":true,\"data\":{{\"png_base64\":\"{encoded}\",\"method\":\"owned fixture\"}}}}'")).unwrap();
    Arc::get_mut(&mut f.core).unwrap().native = native;
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='vision'",
            &[],
        )
        .unwrap();
    let capture = crate::gui::dispatch(&f.core, "gui.screenshot_target", &json!({"hwnd":1}))
        .await
        .unwrap();
    assert_eq!(capture["capture_module"], "gui");
    assert_eq!(capture["coordinate_space"], "physical");
    let id = capture["id"].as_str().unwrap();
    assert_eq!(
        ui(&f.core, "get", &json!({"id":id})).await.unwrap()["sha256"],
        digest(&png())
    );
    let conversation = f.conversation();
    attach(
        &f.core,
        &json!({"conversation_id":conversation,"asset_id":id}),
    )
    .await
    .unwrap();
    {
        let images = verified_images(&f.core, &conversation).unwrap();
        assert_eq!(images.len(), 1);
        f.core
            .store
            .execute(
                "UPDATE module_configs SET enabled=0 WHERE module_id='gui'",
                &[],
            )
            .unwrap();
        assert_eq!(images.check().unwrap_err(), "TOOL_DISABLED_OR_UNKNOWN");
    }
    assert_eq!(
        ui(&f.core, "get", &json!({"id":id})).await.unwrap_err(),
        "TOOL_DISABLED_OR_UNKNOWN"
    );
    assert_eq!(
        attach(
            &f.core,
            &json!({"conversation_id":conversation,"asset_id":id})
        )
        .await
        .unwrap_err(),
        "TOOL_DISABLED_OR_UNKNOWN"
    );
    assert_eq!(
        verified_images(&f.core, &conversation).unwrap_err(),
        "TOOL_DISABLED_OR_UNKNOWN"
    );
    assert!(
        ui(&f.core, "delete", &json!({"id":id})).await.is_err(),
        "headless L3 must deny deletion"
    );
    assert!(Path::new(capture["path"].as_str().unwrap()).exists());
    f.finish();
}

#[tokio::test]
async fn module_capture_leases_cancel_independently_and_revoke_queued_work() {
    let f = Fixture::new();
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=1 WHERE module_id='browser'",
            &[],
        )
        .unwrap();
    let gui = Lease::for_module(&f.core, Some("gui"), true).unwrap();
    let browser = Lease::for_module(&f.core, Some("browser"), true).unwrap();
    let vision = Lease::for_module(&f.core, Some("vision"), true).unwrap();
    cancel_module(&f.core, "vision").unwrap();
    assert!(vision.cancel.is_cancelled());
    gui.check().unwrap();
    browser.check().unwrap();
    drop((gui, browser, vision));
    for module in ["gui", "browser"] {
        let serial = ACTIONS.lock().await;
        let c = f.core.clone();
        let tool = if module == "gui" {
            "gui.screenshot_target"
        } else {
            "browser.screenshot"
        };
        let job = tokio::spawn(async move { capture(&c, tool, &json!({"hwnd":1})).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            while f.core.asset_runs.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        cancel_module(&f.core, module).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), job)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err(),
            "ASSET_CANCELLED"
        );
        drop(serial);
        assert!(f.core.store.setting(ROOT_KEY).is_null());
    }
    f.finish();
}

#[test]
fn text_suffix_does_not_bypass_binary_image_validation_or_capture_format() {
    let info = content_info(&png(), "attachment", "renamed.txt").unwrap();
    assert_eq!(info.mime, "image/png");
    assert!(info.thumbnail.is_some());
    assert!(content_info(b"BM valid prose", "capture", "capture.txt").is_err());
    assert!(content_info(b"GIF89a valid prose", "attachment", "image.gif").is_err());
}

#[tokio::test]
async fn image_payload_rechecks_capture_origin_record_before_send() {
    let f = Fixture::new();
    let conversation = f.conversation();
    let (record, _) = owned(&f.core, "capture");
    attach(
        &f.core,
        &json!({"conversation_id":conversation,"asset_id":record.id}),
    )
    .await
    .unwrap();
    {
        let images = verified_images(&f.core, &conversation).unwrap();
        let mut changed = record.clone();
        changed.capture_module = Some("gui".into());
        f.core
            .store
            .execute(
                "UPDATE asset_records SET record_json=? WHERE id=?",
                &[
                    json!(serde_json::to_string(&changed).unwrap()),
                    json!(record.id),
                ],
            )
            .unwrap();
        assert_eq!(images.check().unwrap_err(), "ASSET_RECORD_CHANGED");
    }
    f.finish();
}

#[tokio::test]
async fn browser_capture_protocol_bounds_cancellation_and_origin_are_enforced() {
    for mode in ["success", "scope", "oversize", "cancel", "privacy"] {
        let mut f = Fixture::new();
        f.core
            .store
            .execute(
                "UPDATE module_configs SET enabled=1 WHERE module_id='browser'",
                &[],
            )
            .unwrap();
        f.core
            .store
            .execute(
                "UPDATE module_configs SET enabled=0 WHERE module_id='vision'",
                &[],
            )
            .unwrap();
        let resource = f.root.join("mock-resource");
        std::fs::create_dir_all(resource.join("sidecars/browser-host")).unwrap();
        let marker = resource.join("started");
        let png = base64::engine::general_purpose::STANDARD.encode(png());
        let script = format!(
            r#"import {{createInterface}} from 'node:readline'; import {{writeFileSync}} from 'node:fs';
for await (const line of createInterface({{input:process.stdin}})) {{
 const req=JSON.parse(line); writeFileSync({},'started');
 if(req.input.output !== undefined || !req.input.return_png) throw Error('memory input required');
 if('{mode}' === 'cancel') await new Promise(r=>setTimeout(r,30000));
 if('{mode}' === 'privacy') await new Promise(r=>setTimeout(r,250));
 if('{mode}' === 'oversize') process.stdout.write('x'.repeat(48*1024*1024+1)+'\n');
 else process.stdout.write(JSON.stringify({{id:'{mode}'==='scope'?'wrong':req.id,ok:true,data:{{png_base64:'{png}'}}}})+'\n');
}}
"#,
            json!(marker)
        );
        std::fs::write(resource.join("sidecars/browser-host/host.mjs"), script).unwrap();
        Arc::get_mut(&mut f.core).unwrap().resource = resource;
        let c = f.core.clone();
        let job = tokio::spawn(async move {
            crate::modules::dispatch(&c, "browser.screenshot", &json!({})).await
        });
        if ["cancel", "privacy"].contains(&mode) {
            tokio::time::timeout(Duration::from_secs(10), async {
                while !marker.exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            if mode == "cancel" {
                cancel_module(&f.core, "browser").unwrap();
            } else {
                f.core.store.set_setting("privacy", &json!(true)).unwrap();
            }
        }
        let result = tokio::time::timeout(Duration::from_secs(10), job)
            .await
            .unwrap()
            .unwrap();
        if mode == "success" {
            let capture = result.unwrap();
            assert_eq!(capture["capture_module"], "browser");
            assert_eq!(capture["coordinate_space"], "css");
            let conversation = f.conversation();
            attach(
                &f.core,
                &json!({"conversation_id":conversation,"asset_id":capture["id"]}),
            )
            .await
            .unwrap();
            assert_eq!(verified_images(&f.core, &conversation).unwrap().len(), 1);
            assert!(ui(&f.core, "get", &json!({"id":capture["id"]}))
                .await
                .unwrap()["thumbnail_data_url"]
                .is_string());
        } else {
            let expected = match mode {
                "scope" => "BROWSER_RESPONSE_SCOPE",
                "oversize" => "PROCESS_LINE_LIMIT",
                "cancel" => "BROWSER_CANCELLED",
                _ => "PRIVACY_DENIED",
            };
            assert_eq!(result.unwrap_err(), expected);
            assert!(f.core.store.setting(ROOT_KEY).is_null());
            assert!(f.core.browser.lock().await.is_none());
            assert_eq!(
                f.core
                    .store
                    .query("SELECT COUNT(*) AS n FROM asset_records", &[])
                    .unwrap()[0]["n"],
                0
            );
        }
        *f.core.browser.lock().await = None;
        assert!(f.core.asset_runs.lock().unwrap().is_empty());
        f.finish();
    }
}

#[tokio::test]
#[ignore = "owned native window capture; explicit live opt-in"]
async fn real_owned_window_capture_thumbnail_attach_expiry_contract() {
    assert_eq!(std::env::var("ALFREDPET_NATIVE_ASSETS").as_deref(), Ok("1"));
    let f = Fixture::new();
    let metadata = f.root.join("temp/owned-window.json");
    let shell = PathBuf::from(std::env::var("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let mut command = tokio::process::Command::new(shell);
    command
        .args(["-NoProfile", "-STA", "-File"])
        .arg(f.core.resource.join("tests/fixtures/uia-window.ps1"))
        .arg("-MetadataPath")
        .arg(&metadata)
        .creation_flags(0x08000000)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = command.spawn().unwrap();
    let job = crate::windows_native::ProcessJob::attach(&child).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let hwnd = loop {
        assert!(child.try_wait().unwrap().is_none());
        if let Ok(bytes) = std::fs::read(&metadata) {
            if let Ok(info) = serde_json::from_slice::<Value>(&bytes) {
                assert_eq!(info["process_id"], child.id().unwrap());
                break info["hwnd"].clone();
            }
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let capture = dispatch(&f.core, "vision.capture_window", &json!({"hwnd":hwnd}))
        .await
        .unwrap();
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='vision'",
            &[],
        )
        .unwrap();
    let gui = crate::gui::dispatch(
        &f.core,
        "gui.screenshot_target",
        &json!({"hwnd":hwnd,"name":"AlfredPetFixtureInput"}),
    )
    .await
    .unwrap();
    assert_eq!(gui["capture_module"], "gui");
    assert_eq!(gui["method"], "PrintWindow");
    assert_eq!(gui["width"], gui["rect"]["width"]);
    assert!(gui["thumbnail_data_url"].is_string());
    let conversation_visual = f.conversation();
    attach(
        &f.core,
        &json!({"conversation_id":conversation_visual,"asset_id":gui["id"]}),
    )
    .await
    .unwrap();
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=1 WHERE module_id='browser'",
            &[],
        )
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request).await;
            let html = format!("<!doctype html><meta charset=utf-8><title>Owned capture fixture</title><h1>AlfredPet private browser capture</h1><article id='emoji'>Public 😀</article><label>{}😀<input id='unicode-field' name='{}😀'></label><select id='unicode-option'><option>{}😀</option></select><script>document.title+='\\ud800';</script>","x".repeat(299),"n".repeat(299),"o".repeat(299));
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", html.len(), html);
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    let opened = f
        .core
        .browser("browser.open", &json!({"url":url}))
        .await
        .unwrap();
    assert_eq!(opened["title"], "Owned capture fixture\u{fffd}");
    let observed_url = opened["url"].as_str().unwrap();
    let extracted = f
        .core
        .browser(
            "browser.extract",
            &json!({"url":observed_url,"selector":"#emoji","body":{"max_chars":8}}),
        )
        .await
        .unwrap();
    assert_eq!(extracted["text"], "Public 😀");
    let forms = f.core.browser("browser.forms", &json!({})).await.unwrap();
    let controls = forms["controls"].as_array().unwrap();
    let field = controls
        .iter()
        .find(|c| c["id"] == "unicode-field")
        .unwrap();
    assert_eq!(field["label"], format!("{}😀", "x".repeat(299)));
    assert_eq!(field["name"], format!("{}😀", "n".repeat(299)));
    let option = controls
        .iter()
        .find(|c| c["id"] == "unicode-option")
        .unwrap();
    assert_eq!(
        option["options"][0]["label"],
        format!("{}😀", "o".repeat(299))
    );
    assert_eq!(option["options"][0]["truncated"], false);
    let browser = crate::modules::dispatch(&f.core, "browser.screenshot", &json!({}))
        .await
        .unwrap();
    assert_eq!(browser["capture_module"], "browser");
    assert_eq!(browser["coordinate_space"], "css");
    assert_eq!(browser["width"], browser["rect"]["width"]);
    assert_eq!(browser["height"], browser["rect"]["height"]);
    assert!(browser["thumbnail_data_url"].is_string());
    attach(
        &f.core,
        &json!({"conversation_id":conversation_visual,"asset_id":browser["id"]}),
    )
    .await
    .unwrap();
    {
        let images = verified_images(&f.core, &conversation_visual).unwrap();
        assert_eq!(images.len(), 2);
        assert_eq!(digest(&images[0].1), gui["sha256"]);
        assert_eq!(digest(&images[1].1), browser["sha256"]);
    }
    *f.core.browser.lock().await = None;
    server.abort();
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=1 WHERE module_id='vision'",
            &[],
        )
        .unwrap();
    assert_eq!(capture["coordinate_space"], "physical");
    assert_eq!(capture["state"], "ready");
    assert_eq!(capture["class"], "capture");
    let path = Path::new(capture["path"].as_str().unwrap());
    let decoded = image::open(path).unwrap();
    assert_eq!(capture["width"], decoded.width());
    assert_eq!(capture["height"], decoded.height());
    let preview = dispatch(&f.core, "vision.asset_get", &json!({"id":capture["id"]}))
        .await
        .unwrap();
    let thumbnail = base64::engine::general_purpose::STANDARD
        .decode(
            preview["thumbnail_data_url"]
                .as_str()
                .unwrap()
                .strip_prefix("data:image/png;base64,")
                .unwrap(),
        )
        .unwrap();
    let thumb = image::load_from_memory(&thumbnail).unwrap();
    assert!(thumb.width() <= 640 && thumb.height() <= 640);
    let conversation = f.conversation();
    attach(
        &f.core,
        &json!({"conversation_id":conversation,"asset_id":capture["id"]}),
    )
    .await
    .unwrap();
    f.core
        .store
        .execute(
            "UPDATE asset_records SET expires_at='2000-01-01T00:00:00+00:00'",
            &[],
        )
        .unwrap();
    let restarted = Core::new(f.root.clone(), f.core.resource.clone()).unwrap();
    assert!(path.exists());
    {
        let images = verified_images(&restarted, &conversation).unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(digest(&images[0].1), capture["sha256"]);
    }
    drop(restarted);
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    drop(job);
    println!(
        "ASSET_NATIVE_CONTRACT_PASS: {}",
        json!({"ownedWindowPrintWindow":true,"ownedGuiTargetPrintWindow":true,"ownedBrowserViewportCapture":true,"browserUnicodeRustWireVerified":true,"guiAndBrowserWithoutVision":true,"captureStoredPrivately":true,"thumbnailVerified":true,"physicalCoordinates":true,"attachedCaptureSurvivedStartupTtl":true,"claudePayloadHashVerified":true,"userDesktopCaptured":false,"physicalDpiHotplugVerified":false,"humanL3Verified":false})
    );
    f.finish();
}
#[tokio::test]
async fn queued_disable_abandonment_and_headless_l3_do_not_create_or_delete_assets() {
    let f = Fixture::new();
    let held = ACTIONS.lock().await;
    let c = f.core.clone();
    let job =
        tokio::spawn(
            async move { dispatch(&c, "vision.capture_window", &json!({"hwnd":1})).await },
        );
    for _ in 0..100 {
        if !f.core.asset_runs.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='vision'",
            &[],
        )
        .unwrap();
    drop(held);
    assert_eq!(job.await.unwrap().unwrap_err(), "TOOL_DISABLED_OR_UNKNOWN");
    assert!(f.core.store.setting(ROOT_KEY).is_null());
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=1 WHERE module_id='vision'",
            &[],
        )
        .unwrap();
    let held = ACTIONS.lock().await;
    let c = f.core.clone();
    let job =
        tokio::spawn(
            async move { dispatch(&c, "vision.capture_window", &json!({"hwnd":1})).await },
        );
    for _ in 0..100 {
        if !f.core.asset_runs.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    job.abort();
    let _ = job.await;
    drop(held);
    assert!(f.core.asset_runs.lock().unwrap().is_empty());
    assert!(f.core.store.setting(ROOT_KEY).is_null());
    let (r, path) = owned(&f.core, "capture");
    let result = f
        .core
        .execute("vision.asset_delete", json!({"id":r.id}))
        .await;
    assert!(result.is_err());
    assert!(path.exists());
    assert_eq!(
        crate::policy::classify("vision.asset_delete", &json!({})),
        crate::policy::Risk::Mandatory
    );
    assert_eq!(
        crate::policy::classify("vision.capture_window", &json!({})),
        crate::policy::Risk::Contextual
    );
    f.finish();
}

#[tokio::test]
async fn ui_import_preserves_image_and_document_bytes_without_reading_a_source_path() {
    let f = Fixture::new();
    let conversation = f.conversation();
    for (name, bytes) in [
        ("captura.png", png()),
        ("documento.pdf", b"%PDF-1.7 owned fixture".to_vec()),
    ] {
        let result = import_attachment(
            &f.core,
            &json!({"conversation_id":conversation,"name":name,
            "data_base64":base64::engine::general_purpose::STANDARD.encode(&bytes)}),
        )
        .await
        .unwrap();
        let file = files::open(Path::new(result["path"].as_str().unwrap()), false).unwrap();
        assert_eq!(files::read(&file, MAX_BYTES).unwrap(), bytes);
        assert!(attachments(&f.core, &conversation)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["asset_id"] == result["asset_id"]));
        let p = Profile::get(&f.core, false).unwrap();
        let (record, state) = load(&f.core, &p, result["asset_id"].as_str().unwrap()).unwrap();
        assert_eq!(record.label, name);
        assert_eq!(state["state"], "ready");
    }
    assert_eq!(
        verified_images(&f.core, &conversation)
            .unwrap()
            .images
            .len(),
        1
    );
    f.finish();
}
#[tokio::test]
async fn ui_import_rejects_paths_unknown_fields_bad_encoding_and_privacy() {
    let f = Fixture::new();
    let conversation = f.conversation();
    let good = json!({"conversation_id":conversation,"name":"captura.png","data_base64":base64::engine::general_purpose::STANDARD.encode(png())});
    for (key, value) in [("name", "C:\\private.png"), ("path", "secret")] {
        let mut bad = good.clone();
        bad[key] = json!(value);
        assert!(import_attachment(&f.core, &bad).await.is_err());
    }
    let mut bad = good.clone();
    bad["data_base64"] = json!("%%%invalid%%%");
    assert_eq!(
        import_attachment(&f.core, &bad).await.unwrap_err(),
        "ATTACHMENT_ENCODING"
    );
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(
        import_attachment(&f.core, &good).await.unwrap_err(),
        "PRIVACY_DENIED"
    );
    assert!(attachments(&f.core, &conversation)
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    f.finish();
}

#[test]
fn model_image_output_rejects_revoked_missing_and_modified_assets() {
    let f = Fixture::new();
    let (r, path) = owned(&f.core, "capture");
    let ids = vec![r.id.clone()];
    assert_eq!(model_images(&f.core, &ids).unwrap()[0]["type"], "image");
    assert!(model_images(&f.core, &vec![r.id.clone(); 6]).is_err());
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='vision'",
            &[],
        )
        .unwrap();
    assert!(model_images(&f.core, &ids).is_err());
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=1 WHERE module_id='vision'",
            &[],
        )
        .unwrap();
    std::fs::write(path, b"changed pixels").unwrap();
    assert!(model_images(&f.core, &ids).is_err());
    assert!(model_images(&f.core, &[storage::id()]).is_err());
    f.finish();
}
