//! Bounded local sources and private binary downloads. Held native handles
//! prevent cooperative replacement; deletion always uses the observed handle.
use crate::{asset_files as files, drive_auth::Scope, runtime::Core, storage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::Arc,
};
type Result<T> = std::result::Result<T, String>;
pub(crate) const BYTE_LIMIT: u64 = 512 * 1024 * 1024;
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceRecord {
    pub path: String,
    pub identity: String,
    pub size: u64,
    pub sha256: String,
}
pub(crate) struct Source {
    pub record: SourceRecord,
    file: File,
    _parents: Vec<File>,
}
impl Source {
    pub(crate) fn new(scope: &Scope, raw: &str) -> Result<Self> {
        scope.check()?;
        let path = scope.core.path(raw, false)?;
        let mut held = Vec::new();
        let ancestors = path
            .parent()
            .ok_or("DRIVE_SOURCE_PARENT")?
            .ancestors()
            .collect::<Vec<_>>();
        for p in ancestors.into_iter().rev() {
            held.push(files::directory(p)?);
        }
        let file = files::open(&path, false)?;
        let identity = files::identity(&file)?;
        let size = file.metadata().map_err(|_| "DRIVE_SOURCE_METADATA")?.len();
        if size > BYTE_LIMIT {
            return Err("DRIVE_SOURCE_BYTE_LIMIT".into());
        }
        let mut reader = file.try_clone().map_err(|_| "DRIVE_SOURCE_READ")?;
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|_| "DRIVE_SOURCE_READ")?;
        let mut hash = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        let mut total = 0;
        loop {
            scope.check()?;
            let n = reader.read(&mut buffer).map_err(|_| "DRIVE_SOURCE_READ")?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > size {
                return Err("DRIVE_SOURCE_CHANGED".into());
            }
            hash.update(&buffer[..n]);
        }
        if total != size {
            return Err("DRIVE_SOURCE_CHANGED".into());
        }
        let record = SourceRecord {
            path: path.to_str().ok_or("DRIVE_SOURCE_PATH")?.into(),
            identity,
            size,
            sha256: format!("{:x}", hash.finalize()),
        };
        let s = Self {
            record,
            file,
            _parents: held,
        };
        s.check(scope)?;
        Ok(s)
    }
    pub(crate) fn check(&self, scope: &Scope) -> Result<()> {
        scope.check()?;
        let path = scope.core.path(&self.record.path, false)?;
        let current = files::open(&path, false)?;
        if files::identity(&current)? != self.record.identity
            || files::identity(&self.file)? != self.record.identity
            || self
                .file
                .metadata()
                .map_err(|_| "DRIVE_SOURCE_METADATA")?
                .len()
                != self.record.size
        {
            return Err("DRIVE_SOURCE_CHANGED".into());
        }
        Ok(())
    }
    pub(crate) fn chunk(&self, scope: &Scope, offset: u64, size: usize) -> Result<Vec<u8>> {
        self.check(scope)?;
        if offset >= self.record.size || size > 1024 * 1024 {
            return Err("DRIVE_SOURCE_RANGE".into());
        }
        let size = size.min((self.record.size - offset) as usize);
        let mut reader = self.file.try_clone().map_err(|_| "DRIVE_SOURCE_READ")?;
        reader
            .seek(SeekFrom::Start(offset))
            .map_err(|_| "DRIVE_SOURCE_READ")?;
        let mut bytes = vec![0; size];
        reader
            .read_exact(&mut bytes)
            .map_err(|_| "DRIVE_SOURCE_READ")?;
        self.check(scope)?;
        Ok(bytes)
    }
}
fn uuid(s: &str) -> Result<()> {
    let u = uuid::Uuid::parse_str(s).map_err(|_| "DRIVE_JOB_ID")?;
    if u.is_nil() || u.to_string() != s {
        return Err("DRIVE_JOB_ID".into());
    }
    Ok(())
}
fn parents(core: &Core, path: &std::path::Path) -> Result<Vec<File>> {
    // Private profile destinations intentionally remain inside the profile.
    if !path.starts_with(&core.root) {
        return Err("DRIVE_LOCAL_PATH".into());
    }
    let mut held = Vec::new();
    for p in path
        .parent()
        .ok_or("DRIVE_LOCAL_PARENT")?
        .ancestors()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        held.push(files::directory(p)?);
    }
    Ok(held)
}
pub(crate) struct JobLease {
    _held: Vec<File>,
    _lock: File,
}
pub(crate) fn job_lease(core: &Core, id: &str) -> Result<JobLease> {
    uuid(id)?;
    let path = core.root.join("temp").join(format!("drive-job-{id}.lock"));
    let held = parents(core, &path)?;
    if !path.try_exists().map_err(|_| "DRIVE_JOB_LOCK")? {
        match files::create(&path) {
            Ok(f) => drop(f),
            Err(_) if path.try_exists().unwrap_or(false) => (),
            Err(_) => return Err("DRIVE_JOB_LOCK".into()),
        }
    }
    let lock = files::lock(&path).map_err(|_| "DRIVE_JOB_BUSY_OR_LOCK_CHANGED")?;
    if lock.metadata().map_err(|_| "DRIVE_JOB_LOCK")?.len() != 0 {
        return Err("DRIVE_JOB_LOCK".into());
    }
    Ok(JobLease {
        _held: held,
        _lock: lock,
    })
}
pub(crate) fn job(core: &Core, connection: &str, id: &str, kind: &str) -> Result<Value> {
    uuid(id)?;
    let rows = core.store.query(
        "SELECT * FROM drive_jobs WHERE id=? AND connection_id=? AND kind=?",
        &[json!(id), json!(connection), json!(kind)],
    )?;
    if rows.as_array().is_none_or(|a| a.len() != 1) {
        return Err("DRIVE_JOB_NOT_FOUND".into());
    }
    let mut row = rows[0].clone();
    row["record"] = serde_json::from_str(row["record_json"].as_str().ok_or("DRIVE_JOB_RECORD")?)
        .map_err(|_| "DRIVE_JOB_RECORD")?;
    Ok(row)
}
pub(crate) fn insert(
    core: &Core,
    connection: &str,
    id: &str,
    kind: &str,
    state: &str,
    record: &Value,
) -> Result<()> {
    uuid(id)?;
    if record.to_string().len() > 32768 {
        return Err("DRIVE_JOB_RECORD_LIMIT".into());
    }
    core.store.execute("INSERT INTO drive_jobs(id,connection_id,kind,state,record_json,created_at,updated_at) VALUES(?,?,?,?,?,?,?)", &[json!(id),json!(connection),json!(kind),json!(state),json!(record.to_string()),json!(storage::now()),json!(storage::now())])?;
    Ok(())
}
pub(crate) fn update(core: &Core, old: &Value, state: &str, record: &Value) -> Result<Value> {
    if record.to_string().len() > 32768 {
        return Err("DRIVE_JOB_RECORD_LIMIT".into());
    }
    if core.store.execute("UPDATE drive_jobs SET state=?,record_json=?,revision=revision+1,updated_at=? WHERE id=? AND connection_id=? AND kind=? AND revision=? AND state=? AND record_json=?", &[json!(state),json!(record.to_string()),json!(storage::now()),old["id"].clone(),old["connection_id"].clone(),old["kind"].clone(),old["revision"].clone(),old["state"].clone(),old["record_json"].clone()])?!=1 { return Err("DRIVE_JOB_CHANGED".into()); }
    job(
        core,
        old["connection_id"].as_str().ok_or("DRIVE_JOB_RECORD")?,
        old["id"].as_str().ok_or("DRIVE_JOB_RECORD")?,
        old["kind"].as_str().ok_or("DRIVE_JOB_RECORD")?,
    )
}
pub(crate) struct Destination {
    core: Arc<Core>,
    pub id: String,
    path: PathBuf,
    file: File,
    identity: String,
    _held: Vec<File>,
    ready: bool,
}
impl Destination {
    pub(crate) fn new(core: &Arc<Core>, id: &str) -> Result<Self> {
        uuid(id)?;
        let path = core
            .root
            .join("temp")
            .join(format!("drive-download-{id}.bin"));
        let held = parents(core, &path)?;
        let file = files::create(&path)?;
        let identity = files::identity(&file)?;
        Ok(Self {
            core: core.clone(),
            id: id.into(),
            path,
            file,
            identity,
            _held: held,
            ready: false,
        })
    }
    pub(crate) fn check(&self) -> Result<()> {
        // The create handle shares reads only, preventing replacement, deletion
        // and other writers for its whole lifetime. A second read-only handle
        // would conflict with this writer's access on Windows.
        if files::identity(&self.file)? != self.identity {
            return Err("DRIVE_DOWNLOAD_CHANGED".into());
        }
        files::private(&self.file)?;
        Ok(())
    }
    pub(crate) fn initial(&self, scope: &Scope) -> Result<Value> {
        self.check()?;
        if self
            .file
            .metadata()
            .map_err(|_| "DRIVE_DOWNLOAD_METADATA")?
            .len()
            != 0
        {
            return Err("DRIVE_DOWNLOAD_INITIAL_NOT_EMPTY".into());
        }
        Ok(
            json!({"version":1,"origin":scope.local_origin()?,"id":self.id,"path":self.path,"identity":self.identity,"size":0}),
        )
    }
    pub(crate) fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.check()?;
        self.file
            .write_all(bytes)
            .map_err(|_| "DRIVE_DOWNLOAD_WRITE".into())
    }
    pub(crate) fn finish(&mut self, size: u64, sha256: &str) -> Result<Value> {
        self.check()?;
        self.file.sync_all().map_err(|_| "DRIVE_DOWNLOAD_SYNC")?;
        if self
            .file
            .metadata()
            .map_err(|_| "DRIVE_DOWNLOAD_METADATA")?
            .len()
            != size
        {
            return Err("DRIVE_DOWNLOAD_CHANGED".into());
        }
        let result = json!({"id":self.id,"path":self.path,"identity":self.identity,"size":size,"sha256":sha256});
        Ok(result)
    }
    pub(crate) fn retain(&mut self) {
        self.ready = true;
    }
}
pub(crate) struct DownloadObservation {
    pub local: Value,
    pub origin: Value,
    pub legacy: bool,
    file: File,
    _parents: Vec<File>,
}
impl DownloadObservation {
    pub(crate) fn erase(self, scope: &Scope) -> Result<()> {
        scope.local_guard(&self.origin, || {
            files::private(&self.file)?;
            if files::identity(&self.file)? != self.local["identity"]
                || self
                    .file
                    .metadata()
                    .map_err(|_| "DRIVE_DOWNLOAD_METADATA")?
                    .len()
                    != self.local["size"].as_u64().ok_or("DRIVE_JOB_RECORD")?
            {
                return Err("DRIVE_DOWNLOAD_CHANGED".into());
            }
            files::erase(&self.file)
        })
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InitialDownload {
    version: u8,
    origin: Value,
    id: String,
    path: PathBuf,
    identity: String,
    size: u64,
}
/// Open the one derived managed path, retain its handle and hash bounded bytes.
/// A filename, arbitrary stored path or partial byte count never proves origin.
pub(crate) fn observe_download(scope: &Scope, row: &Value) -> Result<DownloadObservation> {
    let core = &scope.core;
    let id = row["id"].as_str().ok_or("DRIVE_JOB_RECORD")?;
    uuid(id)?;
    if row["kind"] != "download" || row["connection_id"] != scope.id {
        return Err("DRIVE_JOB_RECORD".into());
    }
    let path = core
        .root
        .join("temp")
        .join(format!("drive-download-{id}.bin"));
    let origin = scope.local_origin()?;
    let legacy = row["record"]["initial"].is_null();
    let identity = if legacy {
        if row["state"] != "ready" {
            return Err("DRIVE_DOWNLOAD_INITIAL_EVIDENCE_REQUIRED".into());
        }
        row["record"]["local"]["identity"]
            .as_str()
            .ok_or("DRIVE_JOB_RECORD")?
            .to_string()
    } else {
        let initial: InitialDownload = serde_json::from_value(row["record"]["initial"].clone())
            .map_err(|_| "DRIVE_DOWNLOAD_INITIAL_EVIDENCE_INVALID")?;
        if initial.version != 1 || initial.id != id || initial.path != path || initial.size != 0 {
            return Err("DRIVE_DOWNLOAD_INITIAL_EVIDENCE_INVALID".into());
        }
        if initial.origin != origin {
            return Err("DRIVE_DOWNLOAD_PROFILE_CHANGED".into());
        }
        initial.identity
    };
    let local = &row["record"]["local"];
    if local["id"] != id
        || local
            .get("path")
            .is_some_and(|p| p.as_str() != path.to_str())
    {
        return Err("DRIVE_DOWNLOAD_LOCAL_PATH_CHANGED".into());
    }
    let held = parents(core, &path)?;
    if !path.try_exists().map_err(|_| "DRIVE_DOWNLOAD_ACCESS")? {
        return Err("DRIVE_DOWNLOAD_MISSING".into());
    }
    let file = files::open(&path, true)?;
    files::private(&file)?;
    if files::identity(&file)? != identity {
        return Err("DRIVE_DOWNLOAD_CHANGED".into());
    }
    let size = file
        .metadata()
        .map_err(|_| "DRIVE_DOWNLOAD_METADATA")?
        .len();
    if size > BYTE_LIMIT {
        return Err("DRIVE_DOWNLOAD_BYTE_LIMIT".into());
    }
    let mut reader = file.try_clone().map_err(|_| "DRIVE_DOWNLOAD_READ")?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|_| "DRIVE_DOWNLOAD_READ")?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    let mut total = 0u64;
    loop {
        scope.check()?;
        let n = reader
            .read(&mut buffer)
            .map_err(|_| "DRIVE_DOWNLOAD_READ")?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > size {
            return Err("DRIVE_DOWNLOAD_CHANGED".into());
        }
        hash.update(&buffer[..n]);
    }
    if total != size {
        return Err("DRIVE_DOWNLOAD_CHANGED".into());
    }
    let observed = json!({"id":id,"path":path,"identity":identity,"size":size,"sha256":format!("{:x}",hash.finalize())});
    if (local.get("sha256").is_some()
        || ["ready", "interrupted"].contains(&row["state"].as_str().unwrap_or("")))
        && *local != observed
    {
        return Err("DRIVE_DOWNLOAD_CHANGED".into());
    }
    scope.local_guard(&origin, || Ok(()))?;
    Ok(DownloadObservation {
        local: observed,
        origin,
        legacy,
        file,
        _parents: held,
    })
}
impl Drop for Destination {
    fn drop(&mut self) {
        if !self.ready && self.check().is_ok() {
            let _ = files::erase(&self.file);
        }
        let _ = &self.core;
    }
}
