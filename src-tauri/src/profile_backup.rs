//! Offline profile snapshots. Jobs run before Core, under an OS profile lease.
use crate::{asset_files as files, runtime::Core, storage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

mod archive;
mod restore;
#[cfg(all(test, windows))]
mod tests;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const MAX_FILE: u64 = 512 * 1024 * 1024;
const MAX_TOTAL: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;

fn control(root: &Path) -> Result<PathBuf> {
    Ok(root
        .parent()
        .ok_or("PROFILE_PARENT")?
        .join(crate::edition::MAINTENANCE_DIRECTORY))
}
fn directory(path: &Path) -> Result<()> {
    if !path.exists() {
        files::create_directory(path)?;
    }
    files::directory(path)?;
    Ok(())
}
fn parents(path: &Path) -> Result<Vec<File>> {
    path.parent()
        .ok_or("PROFILE_PARENT")?
        .ancestors()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|p| files::directory(p).map_err(Into::into))
        .collect()
}
fn write_json(path: &Path, value: &Value) -> Result<()> {
    let temp = path.with_extension(format!("{}.tmp", storage::id()));
    let mut f = files::create(&temp)?;
    f.write_all(&serde_json::to_vec(value)?)?;
    f.sync_all()?;
    drop(f);
    fs::rename(temp, path)?;
    Ok(())
}
fn read_json(path: &Path) -> Result<Value> {
    let f = files::open(path, false)?;
    Ok(serde_json::from_slice(&files::read(&f, 4 * 1024 * 1024)?)?)
}
fn digest(file: &mut File) -> Result<String> {
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(format!("{:x}", hasher.finalize()))
}
fn uuid(value: &str) -> Result<()> {
    if uuid::Uuid::parse_str(value)?.to_string() != value {
        return Err("PROFILE_ID".into());
    }
    Ok(())
}

/// Held outside the profile so renaming a profile cannot release its lease.
/// File locks are released by the OS even after an abrupt process termination.
pub fn startup(root: &Path) -> std::result::Result<File, String> {
    startup_inner(root).map_err(|e| e.to_string())
}
fn startup_inner(root: &Path) -> Result<File> {
    let ctl = control(root)?;
    directory(&ctl)?;
    let _parents = parents(&ctl.join("instance.lock"))?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(ctl.join("instance.lock"))?;
    files::identity(&lock)?;
    // A restarting predecessor may still be unwinding its event loop.
    let started = std::time::Instant::now();
    loop {
        if lock.try_lock().is_ok() {
            break;
        }
        if started.elapsed().as_secs() >= 20 {
            return Err(
                "AlfredPet ya está abierto en este perfil. Ciérralo antes de continuar.".into(),
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let job_path = ctl.join("job.json");
    if job_path.exists() {
        let value = read_json(&job_path)?;
        let job: Job = serde_json::from_value(value.clone())?;
        uuid(&job.id)?;
        let result = if job.phase == "queued" {
            let mut started = value;
            started["phase"] = json!("running");
            write_json(&job_path, &started)?;
            perform(root, &ctl, &job)
        } else {
            restore::recover(root, &ctl, &job.id)?;
            Err("Operación interrumpida. Se ha conservado o recuperado el perfil anterior; no se repite automáticamente.".into())
        };
        // The durable receipt precedes consumption of the job. A crash before
        // this point rolls back the swap; a completed job is never replayed.
        let receipt = match result {
            Ok(v) => {
                json!({"status":if job.mode=="restore" {"awaiting_startup"}else{"completed"},"operation":job.mode,"at":storage::now(),"result":v})
            }
            Err(e) => {
                restore::recover(root, &ctl, &job.id)?;
                json!({"status":"failed","operation":job.mode,"at":storage::now(),"error":e.to_string()})
            }
        };
        write_json(&ctl.join("status.json"), &receipt)?;
        if receipt["status"] == "awaiting_startup" {
            let mut pending = serde_json::to_value(&job)?;
            pending["phase"] = json!("installed");
            write_json(&job_path, &pending)?;
        } else {
            fs::remove_file(job_path)?;
            cleanup_stage(&ctl, &job.id)?;
        }
    }
    Ok(lock)
}

fn cleanup_stage(ctl: &Path, id: &str) -> Result<()> {
    uuid(id)?;
    let stage = ctl.join(format!("stage-{id}"));
    if stage.exists() {
        let actual = dunce::canonicalize(&stage)?;
        if actual.parent() != Some(dunce::canonicalize(ctl)?.as_path()) {
            return Err("PROFILE_STAGE_SCOPE".into());
        }
        files::directory(&stage)?;
        fs::remove_dir_all(stage)?;
    }
    Ok(())
}
/// Commit only after the restored Core and startup checks have opened successfully.
pub fn finish_startup(root: &Path) -> std::result::Result<(), String> {
    (|| -> Result<()> {
        let ctl = control(root)?;
        let job_path = ctl.join("job.json");
        if !job_path.exists() {
            return Ok(());
        }
        let job: Job = serde_json::from_value(read_json(&job_path)?)?;
        if job.phase != "installed" {
            return Err("PROFILE_STARTUP_PHASE".into());
        }
        let mut receipt = read_json(&ctl.join("status.json"))?;
        receipt["status"] = json!("completed");
        write_json(&ctl.join("status.json"), &receipt)?;
        fs::remove_file(job_path)?;
        cleanup_stage(&ctl, &job.id)?;
        Ok(())
    })()
    .map_err(|e| e.to_string())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Job {
    id: String,
    mode: String,
    path: PathBuf,
    sha256: String,
    phase: String,
}

fn perform(root: &Path, ctl: &Path, job: &Job) -> Result<Value> {
    let stage = ctl.join(format!("stage-{}", job.id));
    directory(&stage)?;
    match job.mode.as_str() {
        "backup" => {
            let manifest = archive::pack(root, &stage, &job.path)?;
            Ok(
                json!({"path":job.path,"files":manifest.entries.len(),"created_at":manifest.created_at}),
            )
        }
        "restore" => {
            let mut input = files::open(&job.path, false)?;
            if digest(&mut input)? != job.sha256 {
                return Err(
                    "La copia ha cambiado desde la vista previa. Selecciónala de nuevo.".into(),
                );
            }
            let incoming = stage.join("incoming");
            directory(&incoming)?;
            let manifest = archive::unpack(input, &incoming)?;
            restore::prepare(&incoming, root, &manifest)?;
            // Recoverability is a prerequisite, not a best-effort afterthought.
            let previous = ctl.join(format!("antes-de-restaurar-{}.alfredbackup", job.id));
            let previous_stage = stage.join("previous");
            directory(&previous_stage)?;
            archive::pack(root, &previous_stage, &previous)?;
            restore::install(root, ctl, &job.id, &incoming)?;
            Ok(
                json!({"path":job.path,"recovery_archive":previous,"recovery_directory":ctl.join(format!("previous-{}",job.id)),"reauthorization_required":true}),
            )
        }
        _ => Err("PROFILE_OPERATION".into()),
    }
}

fn counts(db: &rusqlite::Connection) -> Result<Value> {
    let mut out = serde_json::Map::new();
    for table in [
        "conversations",
        "messages",
        "attachments",
        "custom_tools",
        "tasks",
        "memories",
    ] {
        let n: i64 = db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
        out.insert(table.into(), json!(n));
    }
    Ok(Value::Object(out))
}

pub async fn dispatch(
    c: &Arc<Core>,
    operation: &str,
    input: Value,
) -> std::result::Result<Value, String> {
    let ctl = control(&c.root).map_err(|e| e.to_string())?;
    match operation {
        "profile.status" => {
            if ctl.join("status.json").exists() {
                read_json(&ctl.join("status.json")).map_err(|e| e.to_string())
            } else {
                Ok(Value::Null)
            }
        }
        "profile.preview_backup" => counts(&*c.store.0.lock().map_err(|_| "DB_LOCK")?)
            .map(|counts| json!({"counts":counts}))
            .map_err(|e| e.to_string()),
        "profile.preview_restore" => {
            let path = PathBuf::from(input["path"].as_str().ok_or("PROFILE_PATH")?);
            tokio::task::spawn_blocking(move || {
                archive::preview(&path, &ctl).map_err(|e| e.to_string())
            })
            .await
            .map_err(|_| "PROFILE_WORKER")?
        }
        "profile.schedule" => {
            if input["confirmed"] != true {
                return Err("PROFILE_CONFIRMATION_REQUIRED".into());
            }
            if !c.runs.lock().map_err(|_| "RUN_LOCK")?.is_empty()
                || !c.pending.lock().map_err(|_| "PENDING_LOCK")?.is_empty()
                || !c.asset_runs.lock().map_err(|_| "ASSET_LOCK")?.is_empty()
                || c.dictation.lock().map_err(|_| "DICTATION_LOCK")?.is_some()
            {
                return Err("Termina las respuestas, los permisos pendientes y el dictado antes de reiniciar.".into());
            }
            let mode = input["mode"].as_str().ok_or("PROFILE_MODE")?;
            if !["backup", "restore"].contains(&mode) {
                return Err("PROFILE_MODE".into());
            }
            let path = PathBuf::from(input["path"].as_str().ok_or("PROFILE_PATH")?);
            if !path.is_absolute() {
                return Err("PROFILE_ABSOLUTE_PATH".into());
            }
            if mode == "backup" && path.exists() {
                return Err(
                    "El archivo ya existe. Elige otro nombre; no se sobrescriben copias.".into(),
                );
            }
            if mode == "restore" && (input["sha256"].as_str().is_none_or(|s| s.len() != 64)) {
                return Err("PROFILE_PREVIEW_REQUIRED".into());
            }
            let job = Job {
                id: storage::id(),
                mode: mode.into(),
                path,
                sha256: input["sha256"].as_str().unwrap_or("").into(),
                phase: "queued".into(),
            };
            directory(&ctl).map_err(|e| e.to_string())?;
            let _guard = c.profile_actions.lock().await;
            if ctl.join("job.json").exists() {
                return Err(
                    "Ya hay una operación preparada. Reinicia AlfredPet para terminarla.".into(),
                );
            }
            // create_new prevents overwriting a separately scheduled operation.
            let mut file = files::create(&ctl.join("job.json"))?;
            file.write_all(&serde_json::to_vec(&job).map_err(|e| e.to_string())?)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            Ok(json!({"queued":true}))
        }
        _ => Err("PROFILE_OPERATION".into()),
    }
}
