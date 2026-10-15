use crate::{
    process,
    runtime::Core,
    semantic_store::{self},
    storage,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};
use tokio_util::sync::CancellationToken;

pub const REVISION: &str = "e8f8c211226b894fcb81acc59f3b34ba3efd5f42";
const MANIFEST: &str = include_str!("../../assets/models/semantic-model.json");
#[derive(Deserialize)]
struct ModelFile {
    path: String,
    url: String,
    bytes: u64,
    sha256: String,
}
#[derive(Deserialize)]
struct Manifest {
    id: String,
    revision: String,
    files: Vec<ModelFile>,
}
pub struct Host {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    #[cfg(windows)]
    _job: crate::windows_native::ProcessJob,
}
pub struct Control {
    session: CancellationToken,
    job: Option<(String, CancellationToken)>,
    rerun: bool,
}
impl Default for Control {
    fn default() -> Self {
        Self {
            session: CancellationToken::new(),
            job: None,
            rerun: false,
        }
    }
}
fn model_dir(c: &Core) -> PathBuf {
    c.root.join("models/semantic").join(REVISION)
}
fn manifest() -> Result<Manifest, String> {
    serde_json::from_str(MANIFEST).map_err(|_| "SEMANTIC_MANIFEST_INVALID".into())
}
fn resource(c: &Core, relative: &str) -> PathBuf {
    let bundled = c.resource.join(relative);
    if bundled.exists() || !cfg!(debug_assertions) {
        bundled
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join(
            relative
                .replace("sidecars/semantic-host", ".work/semantic-host")
                .replace("runtime/", ".work/runtime/"),
        )
    }
}
pub fn status(c: &Core) -> Result<Value, String> {
    let manifest = manifest()?;
    let count = c.store.query(
        "SELECT count(*) AS total FROM semantic_embeddings WHERE index_revision=?",
        &[json!(semantic_store::INDEX_REVISION)],
    )?;
    Ok(
        json!({"available":cfg!(feature="semantic_memory"),"enabled":c.store.setting("semantic_memory")==true,"installed":model_dir(c).join(".complete").is_file(),"model":manifest.id,"revision":manifest.revision,"bytes":manifest.files.iter().map(|f|f.bytes).sum::<u64>(),"dimensions":384,"provider":"cpu","indexed":count[0]["total"],"job":c.store.setting("semantic_job_state"),"last_error":c.store.setting("semantic_last_error")}),
    )
}
fn publish(c: &Core, id: &str, value: Value) -> Result<(), String> {
    let control = c.semantic_control.lock().map_err(|_| "SEMANTIC_LOCK")?;
    if control.job.as_ref().is_none_or(|(active, _)| active != id) {
        return Ok(());
    }
    c.store.set_setting("semantic_job_state", &value)?;
    c.emit("alfredpet:semantic", &value);
    Ok(())
}
pub fn recover(c: &Core) -> Result<(), String> {
    if c.store.setting("semantic_job_state")["status"] == "running" {
        c.store.set_setting(
            "semantic_job_state",
            &json!({"status":"interrupted","error":"SEMANTIC_JOB_INTERRUPTED"}),
        )?;
    }
    Ok(())
}
pub fn cancel(c: &Core) -> Result<(), String> {
    let mut control = c.semantic_control.lock().map_err(|_| "SEMANTIC_LOCK")?;
    control.session.cancel();
    control.session = CancellationToken::new();
    if let Some((_, job)) = &control.job {
        job.cancel();
    }
    control.rerun = false;
    Ok(())
}
fn usable(c: &Core) -> Result<(), String> {
    if !cfg!(feature = "semantic_memory") {
        return Err("SEMANTIC_FEATURE_UNAVAILABLE".into());
    }
    if semantic_store::blocked(&c.store) {
        return Err("SEMANTIC_PRIVACY_OR_NO_MEMORY".into());
    }
    Ok(())
}
pub async fn set_enabled(c: &Arc<Core>, enabled: bool) -> Result<Value, String> {
    if enabled {
        usable(c)?;
        let path = model_dir(c);
        tokio::task::spawn_blocking(move || verify_model(&path))
            .await
            .map_err(|_| "SEMANTIC_VERIFY_TASK")??;
        usable(c)?;
    }
    cancel(c)?;
    c.store.set_setting("semantic_memory", &json!(enabled))?;
    *c.semantic_host.lock().await = None;
    if enabled {
        schedule(c);
    }
    c.emit("alfredpet:semantic", &json!({}));
    status(c)
}
pub fn schedule(c: &Arc<Core>) {
    if c.store.setting("semantic_memory") != true || usable(c).is_err() {
        return;
    }
    if let Ok(mut control) = c.semantic_control.lock() {
        if control.job.is_some() {
            control.rerun = true;
            return;
        }
    }
    let _ = start(c, "index", false);
}
pub fn start(c: &Arc<Core>, operation: &str, consent: bool) -> Result<Value, String> {
    usable(c)?;
    if !["download", "index", "rebuild"].contains(&operation) {
        return Err("SEMANTIC_JOB_INVALID".into());
    }
    if operation == "download" && !consent {
        return Err("SEMANTIC_DOWNLOAD_CONSENT_REQUIRED".into());
    }
    if operation == "download" && c.store.setting("semantic_memory") == true {
        return Err("SEMANTIC_DISABLE_BEFORE_DOWNLOAD".into());
    }
    if operation != "download" && c.store.setting("semantic_memory") != true {
        return Err("SEMANTIC_DISABLED".into());
    }
    let id = storage::id();
    let token = CancellationToken::new();
    {
        let mut control = c.semantic_control.lock().map_err(|_| "SEMANTIC_LOCK")?;
        if control.job.is_some() {
            return Err("SEMANTIC_JOB_BUSY".into());
        }
        control.job = Some((id.clone(), token.clone()));
    }
    let operation = operation.to_string();
    if let Err(error) = publish(
        c,
        &id,
        json!({"id":id,"status":"running","phase":operation,"completed":0}),
    ) {
        c.semantic_control.lock().map_err(|_| "SEMANTIC_LOCK")?.job = None;
        return Err(error);
    }
    let core = c.clone();
    let job_id = id.clone();
    tokio::spawn(async move {
        let result = if operation == "download" {
            download(&core, &job_id, &token).await
        } else {
            index(&core, &job_id, &token, operation == "rebuild").await
        };
        let state = match result {
            Ok(data) => json!({"id":job_id,"status":"succeeded","phase":operation,"result":data}),
            Err(error) => {
                json!({"id":job_id,"status":if token.is_cancelled(){"cancelled"}else{"error"},"phase":operation,"error":error})
            }
        };
        let _ = publish(&core, &job_id, state);
        let rerun = core
            .semantic_control
            .lock()
            .map(|mut control| {
                control.job = None;
                let rerun = control.rerun;
                control.rerun = false;
                rerun
            })
            .unwrap_or(false);
        if rerun {
            schedule(&core);
        }
    });
    Ok(json!({"job_id":id}))
}
fn safe_file(path: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| "SEMANTIC_MODEL_MISSING")?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err("SEMANTIC_REPARSE_DENIED".into());
        }
    }
    if meta.file_type().is_symlink() {
        return Err("SEMANTIC_REPARSE_DENIED".into());
    }
    Ok(())
}
fn verify_model(path: &Path) -> Result<(), String> {
    use std::io::Read;
    for parent in path.ancestors().take(4) {
        safe_file(parent)?;
    }
    for file in manifest()?.files {
        let target = path.join(file.path);
        safe_file(target.parent().ok_or("SEMANTIC_MODEL_PATH")?)?;
        safe_file(&target)?;
        let mut input = std::fs::File::open(&target).map_err(|_| "SEMANTIC_MODEL_MISSING")?;
        if input
            .metadata()
            .map_err(|_| "SEMANTIC_MODEL_MISSING")?
            .len()
            != file.bytes
        {
            return Err("SEMANTIC_MODEL_HASH_MISMATCH".into());
        }
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop {
            let n = input.read(&mut buffer).map_err(|_| "SEMANTIC_MODEL_READ")?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        if format!("{:x}", hash.finalize()) != file.sha256 {
            return Err("SEMANTIC_MODEL_HASH_MISMATCH".into());
        }
    }
    Ok(())
}
fn allowed_url(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.host_str().is_some_and(|host| {
            host == "huggingface.co"
                || host == "hf.co"
                || host.ends_with(".hf.co")
                || host.ends_with(".huggingface.co")
        })
}
async fn download(c: &Arc<Core>, id: &str, token: &CancellationToken) -> Result<Value, String> {
    let root = model_dir(c);
    for dir in [
        c.root.clone(),
        c.root.join("models"),
        c.root.join("models/semantic"),
        root.clone(),
        root.join("onnx"),
    ] {
        if !dir.exists() {
            tokio::fs::create_dir(&dir)
                .await
                .map_err(|_| "SEMANTIC_MODEL_DIRECTORY")?;
        }
        safe_file(&dir)?;
    }
    if root.join(".complete").exists() {
        safe_file(&root.join(".complete"))?;
        tokio::fs::remove_file(root.join(".complete"))
            .await
            .map_err(|_| "SEMANTIC_MODEL_MARKER")?;
    }
    let client = crate::http_client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(600))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() < 8 && allowed_url(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("SEMANTIC_REDIRECT_DENIED")
            }
        }))
        .build()
        .map_err(|_| "SEMANTIC_DOWNLOAD_CLIENT")?;
    let manifest = manifest()?;
    let total = manifest.files.iter().map(|f| f.bytes).sum::<u64>();
    let mut completed = 0;
    for file in &manifest.files {
        if token.is_cancelled() || usable(c).is_err() {
            return Err("SEMANTIC_CANCELLED".into());
        }
        let target = root.join(&file.path);
        let part = root.join(format!(".download-{id}.part"));
        let result = async {
            let url=reqwest::Url::parse(&file.url).map_err(|_| "SEMANTIC_DOWNLOAD_URL")?;
            if !allowed_url(&url) {return Err("SEMANTIC_DOWNLOAD_URL".into());}
            let mut response=tokio::select! { _=token.cancelled()=>return Err("SEMANTIC_CANCELLED".into()), response=client.get(url).send()=>response.map_err(|_| "SEMANTIC_DOWNLOAD_NETWORK")? };
            if !response.status().is_success() {return Err("SEMANTIC_DOWNLOAD_HTTP".into());}
            if response.content_length().is_some_and(|n|n!=file.bytes) {return Err("SEMANTIC_DOWNLOAD_SIZE".into());}
            let mut output=tokio::fs::OpenOptions::new().write(true).create_new(true).open(&part).await.map_err(|_| "SEMANTIC_DOWNLOAD_FILE")?;
            let mut received=0u64;let mut hash=Sha256::new();let mut last_report=std::time::Instant::now();
            loop {
                let chunk=tokio::select! {_=token.cancelled()=>return Err("SEMANTIC_CANCELLED".into()),chunk=response.chunk()=>chunk.map_err(|_| "SEMANTIC_DOWNLOAD_NETWORK")?};
                let Some(chunk)=chunk else{break;};
                received+=chunk.len() as u64;if received>file.bytes {return Err("SEMANTIC_DOWNLOAD_SIZE".into());}
                hash.update(&chunk);output.write_all(&chunk).await.map_err(|_| "SEMANTIC_DOWNLOAD_WRITE")?;
                if last_report.elapsed()>Duration::from_millis(250) {
                    publish(c,id,json!({"id":id,"status":"running","phase":"download","completed":completed+received,"total":total}))?;last_report=std::time::Instant::now();
                }
            }
            if received!=file.bytes || format!("{:x}",hash.finalize())!=file.sha256 {return Err("SEMANTIC_MODEL_HASH_MISMATCH".into());}
            output.sync_all().await.map_err(|_| "SEMANTIC_DOWNLOAD_SYNC")?;drop(output);
            if token.is_cancelled() || usable(c).is_err() {return Err("SEMANTIC_CANCELLED".into());}
            if target.exists() {safe_file(&target)?;tokio::fs::remove_file(&target).await.map_err(|_| "SEMANTIC_MODEL_REPLACE")?;}
            tokio::fs::rename(&part,&target).await.map_err(|_| "SEMANTIC_MODEL_RENAME")?;
            Ok::<_,String>(())
        }.await;
        // Only this job's generated partial path is removed.
        let _ = tokio::fs::remove_file(&part).await;
        result?;
        completed += file.bytes;
    }
    if token.is_cancelled() || usable(c).is_err() {
        return Err("SEMANTIC_CANCELLED".into());
    }
    tokio::fs::write(root.join(".complete"), REVISION)
        .await
        .map_err(|_| "SEMANTIC_MODEL_MARKER")?;
    Ok(json!({"downloaded":completed,"enabled":false}))
}
async fn spawn(c: &Core) -> Result<Host, String> {
    for parent in model_dir(c).ancestors().take(4) {
        safe_file(parent)?;
    }
    let node = resource(c, "runtime/node.exe");
    let host = resource(c, "sidecars/semantic-host/host.mjs");
    let model_manifest = resource(c, "assets/models/semantic-model.json");
    if !node.is_file() || !host.is_file() || !model_manifest.is_file() {
        return Err("SEMANTIC_RUNTIME_MISSING".into());
    }
    let mut command = Command::new(node);
    command.env_clear();
    for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .arg(host)
        .arg(model_dir(c))
        .arg(model_manifest)
        .current_dir(&c.root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    process::sanitize(&mut command);
    let mut child = command.spawn().map_err(|_| "SEMANTIC_HOST_SPAWN")?;
    #[cfg(windows)]
    let job = crate::windows_native::ProcessJob::attach(&child)?;
    let input = child.stdin.take().ok_or("SEMANTIC_STDIN")?;
    let mut output = BufReader::new(child.stdout.take().ok_or("SEMANTIC_STDOUT")?);
    let line = process::bounded_line(&mut output, 16384)
        .await?
        .ok_or("SEMANTIC_HOST_EXIT")?;
    let ready: Value = serde_json::from_str(&line).map_err(|_| "SEMANTIC_READY_INVALID")?;
    if ready["ready"] != true
        || ready["revision"] != REVISION
        || ready["dimensions"] != 384
        || ready["runtime"] != "1.30.0"
    {
        return Err(ready["error"]
            .as_str()
            .filter(|s| s.starts_with("SEMANTIC_") && s.len() < 100)
            .unwrap_or("SEMANTIC_READY_INVALID")
            .into());
    }
    Ok(Host {
        child,
        input,
        output,
        #[cfg(windows)]
        _job: job,
    })
}
pub async fn embed(
    c: &Arc<Core>,
    texts: &[String],
    cancel: &CancellationToken,
) -> Result<Vec<Vec<f32>>, String> {
    usable(c)?;
    if c.store.setting("semantic_memory") != true {
        return Err("SEMANTIC_DISABLED".into());
    }
    if cancel.is_cancelled() {
        return Err("SEMANTIC_CANCELLED".into());
    }
    if texts.is_empty()
        || texts.len() > 8
        || texts
            .iter()
            .any(|s| s.is_empty() || s.chars().count() > 4096 || crate::policy::contains_secret(s))
    {
        return Err("SEMANTIC_INPUT_LIMIT_OR_SECRET".into());
    }
    let session = c
        .semantic_control
        .lock()
        .map_err(|_| "SEMANTIC_LOCK")?
        .session
        .clone();
    let mut host = tokio::select! {_=session.cancelled()=>return Err("SEMANTIC_CANCELLED".into()),_=cancel.cancelled()=>return Err("SEMANTIC_CANCELLED".into()),host=c.semantic_host.lock()=>host};
    if host
        .as_mut()
        .is_some_and(|h| h.child.try_wait().ok().flatten().is_some())
    {
        *host = None;
    }
    let mut worker = host.take();
    let work = async {
        if worker.is_none() {
            worker = Some(spawn(c).await?);
        }
        let worker = worker.as_mut().ok_or("SEMANTIC_HOST")?;
        let id = storage::id();
        let request = json!({"id":id,"operation":"embed","texts":texts}).to_string() + "\n";
        worker
            .input
            .write_all(request.as_bytes())
            .await
            .map_err(|_| "SEMANTIC_PIPE_WRITE")?;
        let line = process::bounded_line(&mut worker.output, 128 * 1024)
            .await?
            .ok_or("SEMANTIC_HOST_EXIT")?;
        let response: Value =
            serde_json::from_str(&line).map_err(|_| "SEMANTIC_RESPONSE_INVALID")?;
        if response["id"] != id || response["ok"] != true {
            return Err("SEMANTIC_INFERENCE_FAILED".into());
        }
        let vectors: Vec<Vec<f32>> = serde_json::from_value(response["vectors"].clone())
            .map_err(|_| "SEMANTIC_VECTOR_INVALID")?;
        if vectors.len() != texts.len() || vectors.iter().any(|v| !semantic_store::valid_vector(v))
        {
            return Err("SEMANTIC_VECTOR_INVALID".into());
        }
        Ok::<_, String>(vectors)
    };
    let result = tokio::select! {_=session.cancelled()=>Err("SEMANTIC_CANCELLED".into()),_=cancel.cancelled()=>Err("SEMANTIC_CANCELLED".into()),result=tokio::time::timeout(Duration::from_secs(45),work)=>result.map_err(|_| "SEMANTIC_TIMEOUT".to_string()).and_then(|r|r)};
    if result.is_ok()
        && !session.is_cancelled()
        && !cancel.is_cancelled()
        && usable(c).is_ok()
        && c.store.setting("semantic_memory") == true
    {
        *host = worker;
    } // Otherwise dropping it closes the Job Object.
    result
}
async fn index(
    c: &Arc<Core>,
    id: &str,
    token: &CancellationToken,
    rebuild: bool,
) -> Result<Value, String> {
    if rebuild {
        c.store.execute("DELETE FROM semantic_embeddings", &[])?;
    }
    semantic_store::purge_orphans(&c.store)?;
    let pending = semantic_store::pending(&c.store)?;
    let total = pending.len();
    let mut completed = 0;
    let mut skipped = 0;
    for batch in pending.chunks(8) {
        if token.is_cancelled() {
            return Err("SEMANTIC_CANCELLED".into());
        }
        let texts = batch.iter().map(|d| d.text.clone()).collect::<Vec<_>>();
        let vectors = embed(c, &texts, token).await?;
        if token.is_cancelled() {
            return Err("SEMANTIC_CANCELLED".into());
        }
        for (document, vector) in batch.iter().zip(vectors) {
            if semantic_store::persist(&c.store, document, &vector)? {
                completed += 1;
            } else {
                skipped += 1;
            }
        }
        publish(
            c,
            id,
            json!({"id":id,"status":"running","phase":"index","completed":completed,"skipped":skipped,"total":total}),
        )?;
    }
    Ok(json!({"indexed":completed,"skipped":skipped}))
}
pub async fn retrieve(c: &Arc<Core>, query: &str, project: Option<&str>) -> Result<Value, String> {
    retrieve_with(c, query, project, &CancellationToken::new(), false).await
}
pub async fn retrieve_with(
    c: &Arc<Core>,
    query: &str,
    project: Option<&str>,
    cancel: &CancellationToken,
    all_scopes: bool,
) -> Result<Value, String> {
    if semantic_store::blocked(&c.store) {
        return Ok(json!([]));
    }
    if !cfg!(feature = "semantic_memory") || c.store.setting("semantic_memory") != true {
        return if all_scopes {
            semantic_store::retrieve_all(&c.store, query, None)
        } else {
            c.store.retrieve(query, project)
        };
    }
    schedule(c);
    let bounded = query.chars().take(4096).collect::<String>();
    let result = embed(c, &[bounded], cancel).await;
    if cancel.is_cancelled() {
        return Err("SEMANTIC_CANCELLED".into());
    }
    let vector = match result {
        Ok(mut vectors) => {
            c.store.set_setting("semantic_last_error", &Value::Null)?;
            vectors.pop()
        }
        Err(error) => {
            c.store.set_setting("semantic_last_error", &json!(error))?;
            c.emit("alfredpet:semantic", &json!({"error":error}));
            None
        }
    };
    if semantic_store::blocked(&c.store) {
        return Ok(json!([]));
    }
    let vector = if c.store.setting("semantic_memory") == true {
        vector.as_deref()
    } else {
        None
    };
    if all_scopes {
        semantic_store::retrieve_all(&c.store, query, vector)
    } else {
        semantic_store::retrieve(&c.store, query, project, vector)
    }
}

#[cfg(test)]
#[path = "semantic_tests.rs"]
mod tests;
