use super::*;
use rusqlite::Connection;
use std::{
    collections::{BTreeMap, HashSet},
    io::{Seek, SeekFrom},
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    pub size: u64,
    pub sha256: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    format: String,
    version: u32,
    schema: i64,
    pub created_at: String,
    pub source_root: String,
    pub entries: BTreeMap<String, Entry>,
}
fn name(value: &str) -> Result<()> {
    if value.len() > 220
        || !["data/alfredpet.db", "manifest.json"].contains(&value)
            && !value.starts_with("temp/")
            && !value.starts_with("plugins/")
    {
        return Err("PROFILE_ARCHIVE_PATH".into());
    }
    for part in value.split('/') {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with([' ', '.'])
            || part
                .chars()
                .any(|c| c.is_control() || "\\:*?\"<>|".contains(c))
            || ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
            || (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && stem.as_bytes()[3].is_ascii_digit()
        {
            return Err("PROFILE_ARCHIVE_PATH".into());
        }
    }
    Ok(())
}
fn collect(path: &Path, prefix: &str, out: &mut BTreeMap<String, PathBuf>) -> Result<()> {
    let _dir = files::directory(path)?;
    for item in fs::read_dir(path)? {
        let item = item?;
        let item_name = item
            .file_name()
            .into_string()
            .map_err(|_| "PROFILE_FILENAME")?;
        let target = format!("{prefix}/{item_name}");
        name(&target)?;
        // Never follow symlinks, junctions, hard links or device files.
        if item.file_type()?.is_dir() {
            collect(&item.path(), &target, out)?;
        } else {
            files::open(&item.path(), false)?;
            out.insert(target, item.path());
        }
        if out.len() >= MAX_ENTRIES {
            return Err("La copia supera el límite de 10 000 archivos.".into());
        }
    }
    Ok(())
}
fn portable(path: &Path, root: &Path) -> bool {
    path.starts_with(root.join("temp")) || path.starts_with(root.join("plugins"))
}
pub(super) fn pack(root: &Path, stage: &Path, destination: &Path) -> Result<Manifest> {
    let _parents = parents(destination)?;
    let actual_root = dunce::canonicalize(root)?;
    let actual_parent = dunce::canonicalize(destination.parent().ok_or("PROFILE_PARENT")?)?;
    if actual_parent.starts_with(&actual_root) {
        return Err("Guarda la copia fuera de la carpeta de datos de AlfredPet.".into());
    }
    if destination.exists() {
        return Err("El destino ya existe; no se sobrescribe.".into());
    }
    let db_path = stage.join("snapshot.db");
    let source = Connection::open(root.join("data/alfredpet.db"))?;
    source.backup(rusqlite::MAIN_DB, &db_path, None)?;
    drop(source);
    let db = checked_db(&db_path, stage)?;
    let mut sources = BTreeMap::new();
    sources.insert("data/alfredpet.db".into(), db_path.clone());
    for folder in ["temp", "plugins"] {
        let path = root.join(folder);
        if path.exists() {
            collect(&path, folder, &mut sources)?;
        }
    }
    // Legacy attachments and registered external plugins become owned copies.
    // External project folders are references, not an implicit filesystem backup.
    let rows = db
        .prepare("SELECT id,path FROM attachments")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, path) in rows {
        let path = PathBuf::from(path);
        if portable(&path, root) {
            continue;
        }
        let key = format!("temp/imported-{}.data", storage::id());
        sources.insert(key.clone(), path);
        db.execute(
            "UPDATE attachments SET path=? WHERE id=?",
            rusqlite::params![root.join(key).to_string_lossy(), id],
        )?;
    }
    let rows = db
        .prepare("SELECT id,install_path FROM custom_tools")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, path) in rows {
        let path = PathBuf::from(path);
        if path.starts_with(root.join("plugins")) {
            continue;
        }
        let key = format!("plugins/imported-{}", storage::id());
        collect(&path, &key, &mut sources)?;
        db.execute(
            "UPDATE custom_tools SET install_path=? WHERE id=?",
            rusqlite::params![root.join(key).to_string_lossy(), id],
        )?;
    }
    // Configured attention audio is a personal file and is included explicitly.
    if let Ok(raw) = db.query_row(
        "SELECT json_value FROM settings WHERE key='attention_audio'",
        [],
        |r| r.get::<_, String>(0),
    ) {
        if let Some(path) = serde_json::from_str::<Value>(&raw)?.as_str() {
            let key = format!("temp/attention-{}.wav", storage::id());
            sources.insert(key.clone(), PathBuf::from(path));
            db.execute(
                "UPDATE settings SET json_value=? WHERE key='attention_audio'",
                [json!(root.join(key)).to_string()],
            )?;
        }
    }
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")?;
    drop(db);
    let mut manifest = Manifest {
        format: "AlfredPet profile".into(),
        version: 1,
        schema: storage::SCHEMA_VERSION,
        created_at: storage::now(),
        source_root: root.to_str().ok_or("PROFILE_PATH")?.into(),
        entries: BTreeMap::new(),
    };
    let temp = destination.with_extension(format!("{}.partial", storage::id()));
    let target = files::create(&temp)?;
    let result = (|| -> Result<()> {
        let mut zip = zip::ZipWriter::new(target);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let mut total = 0;
        for (key, path) in sources {
            name(&key)?;
            let _parents = parents(&path)?;
            let mut file = files::open(&path, false)?;
            let size = file.metadata()?.len();
            total += size;
            if size > MAX_FILE || total > MAX_TOTAL || manifest.entries.len() >= MAX_ENTRIES {
                return Err(
                    "PROFILE_SIZE_LIMIT: máximo 512 MiB por archivo y 2 GiB en total.".into(),
                );
            }
            let hash = digest(&mut file)?;
            zip.start_file(&key, options)?;
            if std::io::copy(&mut file, &mut zip)? != size {
                return Err("PROFILE_FILE_CHANGED".into());
            }
            manifest.entries.insert(key, Entry { size, sha256: hash });
        }
        validate_links(&db_path, &manifest)?;
        zip.start_file("manifest.json", options)?;
        zip.write_all(&serde_json::to_vec(&manifest)?)?;
        let output = zip.finish()?;
        output.sync_all()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
    // Verify all compressed bytes and hashes before publishing the destination.
    let verify = files::open(&temp, false)?;
    scan(verify, None)?;
    publish(&temp, destination)?;
    Ok(manifest)
}

pub(super) fn publish(temp: &Path, destination: &Path) -> Result<()> {
    // A destination created after preview must survive too. Do not use the
    // replacing variant of Windows rename for publishing a new user backup.
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::{
            core::PCWSTR,
            Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH},
        };
        let from = temp
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let to = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        unsafe {
            MoveFileExW(
                PCWSTR(from.as_ptr()),
                PCWSTR(to.as_ptr()),
                MOVEFILE_WRITE_THROUGH,
            )
        }?;
    }
    #[cfg(not(windows))]
    {
        fs::hard_link(temp, destination)?;
        fs::remove_file(temp)?;
    }
    Ok(())
}

fn validate_links(path: &Path, manifest: &Manifest) -> Result<()> {
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let root = Path::new(&manifest.source_root);
    let mut query = db.prepare("SELECT path,sha256,size FROM attachments")?;
    for row in query.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })? {
        let (path, hash, size) = row?;
        let key = Path::new(&path)
            .strip_prefix(root)?
            .to_str()
            .ok_or("PROFILE_PATH")?
            .replace('\\', "/");
        let entry = manifest
            .entries
            .get(&key)
            .ok_or("PROFILE_ATTACHMENT_MISSING")?;
        if entry.sha256 != hash || entry.size != u64::try_from(size)? {
            return Err("PROFILE_ATTACHMENT_HASH".into());
        }
    }
    let mut query =
        db.prepare("SELECT record_json,state FROM asset_records WHERE state!='expired'")?;
    for row in query.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (raw, state) = row?;
        if state != "ready" {
            return Err(
                "PROFILE_ASSET_INCOMPLETE: resuelve primero los archivos pendientes del perfil."
                    .into(),
            );
        }
        let record: Value = serde_json::from_str(&raw)?;
        let id = record["id"].as_str().ok_or("PROFILE_ASSET_ID")?;
        uuid(id)?;
        let root_id = record["root_id"].as_str().ok_or("PROFILE_ASSET_ROOT")?;
        uuid(root_id)?;
        let extension = record["extension"]
            .as_str()
            .ok_or("PROFILE_ASSET_EXTENSION")?;
        for (key, object) in [
            (
                format!("temp/assets-{root_id}/{id}.{extension}"),
                &record["original"],
            ),
            (
                format!("temp/assets-{root_id}/{id}.thumb.png"),
                &record["thumbnail"],
            ),
        ] {
            if object.is_null() {
                continue;
            }
            let entry = manifest.entries.get(&key).ok_or("PROFILE_ASSET_MISSING")?;
            if object["sha256"] != entry.sha256 || object["size"] != entry.size {
                return Err("PROFILE_ASSET_HASH".into());
            }
        }
    }
    Ok(())
}

fn schema(db: &Connection) -> Result<Vec<(String, String, String)>> {
    Ok(db
        .prepare("SELECT type,name,COALESCE(sql,'') FROM sqlite_master ORDER BY type,name")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?)
}
pub(super) fn checked_db(path: &Path, scratch: &Path) -> Result<Connection> {
    let db = Connection::open(path)?;
    db.pragma_update(None, "trusted_schema", false)?;
    if db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?
        != storage::SCHEMA_VERSION
    {
        return Err("Esta copia necesita la misma versión del esquema de AlfredPet.".into());
    }
    let reference = scratch.join(format!("schema-{}.db", storage::id()));
    let template = storage::Store::open(&reference)?;
    let expected = schema(&*template.0.lock().map_err(|_| "DB_LOCK")?)?;
    drop(template);
    fs::remove_file(reference)?;
    if schema(&db)? != expected {
        return Err("PROFILE_DATABASE_SCHEMA: estructura no reconocida.".into());
    }
    if db.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))? != "ok"
        || db.prepare("PRAGMA foreign_key_check")?.exists([])?
    {
        return Err("PROFILE_DATABASE_INTEGRITY".into());
    }
    Ok(db)
}

fn scan(input: File, output: Option<&Path>) -> Result<Manifest> {
    if input.metadata()?.len() > MAX_TOTAL + 16 * 1024 * 1024 {
        return Err("PROFILE_SIZE_LIMIT".into());
    }
    let mut zip = zip::ZipArchive::new(input)?;
    if zip.len() > MAX_ENTRIES + 1 {
        return Err("PROFILE_ENTRY_LIMIT".into());
    }
    let mut raw = Vec::new();
    zip.by_name("manifest.json")?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut raw)?;
    if raw.len() > 4 * 1024 * 1024 {
        return Err("PROFILE_MANIFEST_LIMIT".into());
    }
    let manifest: Manifest = serde_json::from_slice(&raw)?;
    if manifest.format != "AlfredPet profile"
        || manifest.version != 1
        || manifest.schema != storage::SCHEMA_VERSION
        || !Path::new(&manifest.source_root).is_absolute()
        || manifest.entries.len() + 1 != zip.len()
        || !manifest.entries.contains_key("data/alfredpet.db")
    {
        return Err("PROFILE_FORMAT".into());
    }
    let mut names = HashSet::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let key = entry.name().to_owned();
        name(&key)?;
        if !names.insert(key.to_lowercase())
            || entry.is_dir()
            || entry
                .unix_mode()
                .is_some_and(|m| m & 0o170000 != 0 && m & 0o170000 != 0o100000)
        {
            return Err("PROFILE_ARCHIVE_ENTRY".into());
        }
        if key == "manifest.json" {
            continue;
        }
        let expected = manifest
            .entries
            .get(&key)
            .ok_or("PROFILE_UNDECLARED_ENTRY")?;
        total = total
            .checked_add(expected.size)
            .ok_or("PROFILE_SIZE_LIMIT")?;
        if entry.size() != expected.size || expected.size > MAX_FILE || total > MAX_TOTAL {
            return Err("PROFILE_SIZE_LIMIT".into());
        }
        let mut target = if let Some(base) = output {
            let path = base.join(&key);
            let parent = path.parent().ok_or("PROFILE_PARENT")?;
            let mut current = base.to_path_buf();
            for component in parent.strip_prefix(base)?.components() {
                current.push(component);
                directory(&current)?;
            }
            Some(files::create(&path)?)
        } else {
            None
        };
        let mut hash = Sha256::new();
        let mut seen = 0;
        let mut buffer = [0u8; 65536];
        loop {
            let n = entry.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            seen += n as u64;
            if seen > expected.size {
                return Err("PROFILE_EXPANSION_LIMIT".into());
            }
            hash.update(&buffer[..n]);
            if let Some(f) = &mut target {
                f.write_all(&buffer[..n])?;
            }
        }
        if seen != expected.size || format!("{:x}", hash.finalize()) != expected.sha256 {
            return Err("PROFILE_HASH_MISMATCH".into());
        }
        if let Some(f) = target {
            f.sync_all()?;
        }
    }
    Ok(manifest)
}
pub(super) fn unpack(input: File, destination: &Path) -> Result<Manifest> {
    scan(input, Some(destination))
}
pub(super) fn preview(path: &Path, ctl: &Path) -> Result<Value> {
    directory(ctl)?;
    let _parents = parents(path)?;
    let mut input = files::open(path, false)?;
    let hash = digest(&mut input)?;
    let manifest = scan(input.try_clone()?, None)?;
    input.seek(SeekFrom::Start(0))?;
    // Only the database is materialized for preview; no plugin is loaded.
    let stage = ctl.join(format!("preview-{}", storage::id()));
    directory(&stage)?;
    let db_path = stage.join("snapshot.db");
    let result = (|| -> Result<Value> {
        let mut zip = zip::ZipArchive::new(input)?;
        let mut target = files::create(&db_path)?;
        std::io::copy(&mut zip.by_name("data/alfredpet.db")?, &mut target)?;
        drop(target);
        let db = checked_db(&db_path, &stage)?;
        let counts = counts(&db)?;
        Ok(
            json!({"sha256":hash,"created_at":manifest.created_at,"files":manifest.entries.len(),"bytes":manifest.entries.values().map(|v|v.size).sum::<u64>(),"counts":counts}),
        )
    })();
    // This UUID-owned directory has never been part of a live profile.
    if stage.parent() == Some(ctl) {
        let _ = fs::remove_dir_all(stage);
    }
    result
}
