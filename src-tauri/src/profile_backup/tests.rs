use super::*;
use base64::Engine;
use rusqlite::Connection;
struct Fixture {
    base: PathBuf,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!("ap-profile-test-{}", storage::id()));
        fs::create_dir(&base).unwrap();
        let root = base.join("AlfredPet");
        let c = Core::new(
            root.clone(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap();
        c.store.execute("INSERT INTO conversations(id,title,created_at,updated_at) VALUES('original','Conversación original',?,?)",&[json!(storage::now()),json!(storage::now())]).unwrap();
        c.store
            .set_setting(
                "alfred_skills",
                &json!([{"id":"fixture","name":"Estilo","instructions":"Texto conservado"}]),
            )
            .unwrap();
        c.store.set_setting("privacy", &json!(false)).unwrap();
        drop(c);
        Self { base, root }
    }
    fn stage(&self) -> PathBuf {
        let p = self.base.join(format!("stage-{}", storage::id()));
        directory(&p).unwrap();
        p
    }
    fn pack(&self) -> PathBuf {
        let path = self.base.join(format!("{}.alfredbackup", storage::id()));
        archive::pack(&self.root, &self.stage(), &path).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        assert!(self.base.parent() == Some(std::env::temp_dir().as_path()));
        assert!(self
            .base
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("ap-profile-test-"));
        let _ = fs::remove_dir_all(&self.base);
    }
}
#[tokio::test]
async fn portable_profile_roundtrip_rebinds_private_assets_and_resets_authority() {
    let f = Fixture::new();
    let c = Arc::new(
        Core::new(
            f.root.clone(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap(),
    );
    let id = storage::id();
    c.store
        .execute(
            "INSERT INTO conversations(id,title,created_at,updated_at) VALUES(?,'Adjunto',?,?)",
            &[json!(id), json!(storage::now()), json!(storage::now())],
        )
        .unwrap();
    let bytes = b"Contenido del adjunto portable";
    c.store
        .set_setting(
            "system_audit_history",
            &json!([{"fixture":"saved observations"}]),
        )
        .unwrap();
    c.store
        .set_setting("system_audit_origin", &json!({"id":"old-origin"}))
        .unwrap();
    c.store.set_setting("routine_shortcuts", &json!([{"id":"owned-routine","needsReview":false,"body":{"source":"unchanged local reference"}}])).unwrap();
    let attachment=crate::assets::import_attachment(&c,&json!({"conversation_id":id,"name":"nota.txt","data_base64":base64::engine::general_purpose::STANDARD.encode(bytes)})).await.unwrap();
    c.store
        .set_setting(&format!("chat_draft.{id}"), &json!("Borrador conservado"))
        .unwrap();
    c.store
        .execute("DELETE FROM module_configs WHERE module_id='gui'", &[])
        .unwrap();
    c.store
        .set_setting(
            "mcp.oauth.fixture",
            &json!({"native_binding":"do-not-transfer"}),
        )
        .unwrap();
    c.store
        .execute(
            "INSERT INTO permission_rules VALUES('p','terminal.run','{}',NULL,NULL,'allow')",
            &[],
        )
        .unwrap();
    fs::write(f.root.join("plugins/fixture.js"), "console.log('owned');").unwrap();
    drop(c);
    let path = f.pack();
    let preview = archive::preview(&path, &control(&f.root).unwrap()).unwrap();
    assert_eq!(preview["counts"]["attachments"], 1);
    let stage = f.stage();
    let manifest = archive::unpack(files::open(&path, false).unwrap(), &stage).unwrap();
    let target = f.base.join("AnotherProfile");
    restore::prepare(&stage, &target, &manifest).unwrap();
    fs::rename(&stage, &target).unwrap();
    let restored = Arc::new(
        Core::new(
            target.clone(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap(),
    );
    assert_eq!(restored.store.setting("privacy"), true);
    assert_eq!(restored.store.setting("allowed_roots"), json!([]));
    assert_eq!(
        restored.store.setting(&format!("chat_draft.{id}")),
        "Borrador conservado"
    );
    assert!(restored.store.setting("mcp.oauth.fixture").is_null());
    assert_eq!(
        restored.store.setting("system_audit_history"),
        json!([{"fixture":"saved observations"}])
    );
    assert!(restored.store.setting("system_audit_origin").is_null());
    assert_eq!(
        restored.store.setting("routine_shortcuts")[0]["needsReview"],
        true
    );
    assert_eq!(
        restored.store.setting("routine_shortcuts")[0]["body"]["source"],
        "unchanged local reference"
    );
    assert_eq!(
        restored
            .store
            .query("SELECT COUNT(*) n FROM permission_rules", &[])
            .unwrap()[0]["n"],
        0
    );
    assert_eq!(
        restored
            .store
            .query("SELECT COUNT(*) n FROM module_configs WHERE enabled=1", &[])
            .unwrap()[0]["n"],
        0
    );
    assert!(restored.store.setting("asset_lifecycle_error").is_null());
    let row = crate::assets::attachments(&restored, &id).unwrap()[0].clone();
    assert_eq!(fs::read(row["path"].as_str().unwrap()).unwrap(), bytes);
    assert!(Path::new(row["path"].as_str().unwrap()).starts_with(&target));
    restored
        .store
        .set_setting("privacy", &json!(false))
        .unwrap();
    let result = crate::assets::ui(&restored, "get", &json!({"id":attachment["asset_id"]})).await;
    assert!(result.is_ok(), "{result:?}");
    assert!(f.root.join("data/alfredpet.db").exists());
}
#[test]
fn rejects_unexpected_schema_corruption_and_existing_destination() {
    let f = Fixture::new();
    let path = f.pack();
    let before = fs::read(&path).unwrap();
    assert!(archive::pack(&f.root, &f.stage(), &path).is_err());
    assert_eq!(before, fs::read(&path).unwrap());
    let db = Connection::open(f.root.join("data/alfredpet.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER injected AFTER INSERT ON tasks BEGIN DELETE FROM conversations; END;",
    )
    .unwrap();
    drop(db);
    assert!(
        archive::pack(&f.root, &f.stage(), &f.base.join("unsafe.alfredbackup"))
            .unwrap_err()
            .to_string()
            .contains("SCHEMA")
    );
    let corrupt = f.base.join("bad.alfredbackup");
    fs::write(&corrupt, b"not a zip").unwrap();
    assert!(archive::preview(&corrupt, &control(&f.root).unwrap()).is_err());
}
#[test]
fn refuses_changed_copy_before_touching_live_profile() {
    let f = Fixture::new();
    let path = f.pack();
    let ctl = control(&f.root).unwrap();
    directory(&ctl).unwrap();
    let result = perform(
        &f.root,
        &ctl,
        &Job {
            id: storage::id(),
            mode: "restore".into(),
            path,
            sha256: "0".repeat(64),
            phase: "queued".into(),
        },
    );
    assert!(result.unwrap_err().to_string().contains("ha cambiado"));
    assert!(f.root.join("data/alfredpet.db").exists());
}
#[test]
fn interrupted_swaps_recover_before_opening_database_and_never_replay() {
    for installed in [false, true] {
        let f = Fixture::new();
        let ctl = control(&f.root).unwrap();
        directory(&ctl).unwrap();
        let id = storage::id();
        fs::rename(&f.root, ctl.join(format!("previous-{id}"))).unwrap();
        if installed {
            directory(&f.root).unwrap();
            fs::write(f.root.join("incoming-marker"), b"incomplete").unwrap();
        }
        write_json(
            &ctl.join("job.json"),
            &json!(Job {
                id,
                mode: "restore".into(),
                path: f.base.join("unused"),
                sha256: String::new(),
                phase: "running".into()
            }),
        )
        .unwrap();
        let lease = startup(&f.root).unwrap();
        assert!(f.root.join("data/alfredpet.db").exists());
        assert!(!f.root.join("incoming-marker").exists());
        assert_eq!(
            read_json(&ctl.join("status.json")).unwrap()["status"],
            "failed"
        );
        assert!(!ctl.join("job.json").exists());
        drop(lease);
        let _again = startup(&f.root).unwrap();
        assert_eq!(
            Connection::open(f.root.join("data/alfredpet.db"))
                .unwrap()
                .query_row(
                    "SELECT title FROM conversations WHERE id='original'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "Conversación original"
        );
    }
}
#[test]
fn rejects_unsafe_names_unlisted_entries_and_bad_hashes() {
    let f = Fixture::new();
    let good = f.pack();
    let bytes = fs::read(good).unwrap();
    for bad in [
        "../escape",
        "temp/CON.txt",
        "temp/a:ads",
        "plugins/X/../../x",
        "temp/trailing.",
        "temp/\\server",
    ] {
        let mut original = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
        let path = f.base.join(format!("{}.alfredbackup", storage::id()));
        let mut out = zip::ZipWriter::new(File::create(&path).unwrap());
        for i in 0..original.len() {
            out.raw_copy_file(original.by_index(i).unwrap()).unwrap();
        }
        out.start_file(bad, zip::write::SimpleFileOptions::default())
            .unwrap();
        out.write_all(b"bad").unwrap();
        out.finish().unwrap();
        assert!(archive::preview(&path, &control(&f.root).unwrap()).is_err());
    }
    let mut original = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
    let bad = f.base.join("hash.alfredbackup");
    let mut out = zip::ZipWriter::new(File::create(&bad).unwrap());
    for i in 0..original.len() {
        let mut item = original.by_index(i).unwrap();
        let mut data = Vec::new();
        item.read_to_end(&mut data).unwrap();
        if item.name() == "data/alfredpet.db" {
            data[100] ^= 1;
        }
        out.start_file(item.name(), zip::write::SimpleFileOptions::default())
            .unwrap();
        out.write_all(&data).unwrap();
    }
    out.finish().unwrap();
    assert!(archive::preview(&bad, &control(&f.root).unwrap())
        .unwrap_err()
        .to_string()
        .contains("HASH"));
}

#[test]
fn successful_restore_commits_only_after_startup_and_keeps_previous_copy() {
    let f = Fixture::new();
    let path = f.pack();
    let ctl = control(&f.root).unwrap();
    directory(&ctl).unwrap();
    let mut input = files::open(&path, false).unwrap();
    let hash = digest(&mut input).unwrap();
    drop(input);
    let db = Connection::open(f.root.join("data/alfredpet.db")).unwrap();
    db.execute("UPDATE conversations SET title='Before replacement'", [])
        .unwrap();
    drop(db);
    let id = storage::id();
    write_json(
        &ctl.join("job.json"),
        &json!(Job {
            id: id.clone(),
            mode: "restore".into(),
            path,
            sha256: hash,
            phase: "queued".into()
        }),
    )
    .unwrap();
    let lease = startup(&f.root).unwrap();
    assert_eq!(
        read_json(&ctl.join("status.json")).unwrap()["status"],
        "awaiting_startup"
    );
    assert!(ctl.join("job.json").exists());
    let core = Core::new(
        f.root.clone(),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
    )
    .unwrap();
    finish_startup(&f.root).unwrap();
    assert_eq!(
        read_json(&ctl.join("status.json")).unwrap()["status"],
        "completed"
    );
    assert!(!ctl.join("job.json").exists());
    assert!(!ctl.join(format!("stage-{id}")).exists());
    let previous = ctl.join(format!("previous-{id}/data/alfredpet.db"));
    assert_eq!(
        Connection::open(previous)
            .unwrap()
            .query_row("SELECT title FROM conversations", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "Before replacement"
    );
    assert_eq!(
        core.store
            .query("SELECT title FROM conversations", &[])
            .unwrap()[0]["title"],
        "Conversación original"
    );
    drop(core);
    drop(lease);
}
#[test]
fn old_profile_survives_failed_swap_and_external_plugin_is_included() {
    let f = Fixture::new();
    let external = f.base.join("plugin-external");
    directory(&external).unwrap();
    fs::write(external.join("run.js"), "fixture").unwrap();
    let db = Connection::open(f.root.join("data/alfredpet.db")).unwrap();
    db.execute(
        "INSERT INTO custom_tools VALUES('fixture','{}',?,'enabled')",
        [external.to_str().unwrap()],
    )
    .unwrap();
    drop(db);
    let path = f.pack();
    let stage = f.stage();
    let manifest = archive::unpack(files::open(&path, false).unwrap(), &stage).unwrap();
    assert!(manifest
        .entries
        .keys()
        .any(|p| p.starts_with("plugins/imported-") && p.ends_with("/run.js")));
    restore::prepare(&stage, &f.root, &manifest).unwrap();
    let lock = files::directory(&f.root).unwrap();
    let ctl = control(&f.root).unwrap();
    directory(&ctl).unwrap();
    assert!(restore::install(&f.root, &ctl, &storage::id(), &stage).is_err());
    assert!(f.root.join("data/alfredpet.db").exists());
    drop(lock);
}

#[test]
fn publication_never_replaces_a_late_created_destination() {
    let f = Fixture::new();
    let tmp = f.base.join("new.partial");
    let target = f.base.join("existing.alfredbackup");
    fs::write(&tmp, b"new").unwrap();
    fs::write(&target, b"existing").unwrap();
    assert!(super::archive::publish(&tmp, &target).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"existing");
}

#[test]
fn missing_linked_file_does_not_produce_a_successful_backup() {
    let f = Fixture::new();
    let db = Connection::open(f.root.join("data/alfredpet.db")).unwrap();
    db.execute("INSERT INTO attachments(id,conversation_id,path,mime,sha256,size,created_at) VALUES('missing','original',?,'text/plain',?,5,?)",rusqlite::params![f.root.join("temp/missing.txt").to_string_lossy(),"0".repeat(64),storage::now()]).unwrap();
    drop(db);
    let target = f.base.join("incomplete.alfredbackup");
    assert!(archive::pack(&f.root, &f.stage(), &target)
        .unwrap_err()
        .to_string()
        .contains("ATTACHMENT_MISSING"));
    assert!(!target.exists());
}
