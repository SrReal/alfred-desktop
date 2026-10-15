//! Offline inspection and exact local deletion. Partial bytes are never ready.
use super::{closed_mode, job_fingerprint, local, public_job, schema, uuid, Result};
use crate::{drive_auth::Scope, policy, runtime::Core, storage};
use serde_json::{json, Value};
use std::sync::Arc;

pub(crate) struct Prepared {
    scope: Scope,
    input: Value,
    origin: Value,
    job: Option<Value>,
    observed: Option<Value>,
    pub risk: policy::Risk,
}
impl Prepared {
    pub fn review(&self) -> Value {
        json!({"connection":self.scope.id,"operation":"drive.download","input":self.input,"review_content":{"job":self.job.as_ref().map(|j|&j["record"]),"local":self.observed},"warning":"Local file only. Interrupted bytes are unverified and cannot be resumed automatically. Delete removes the exact reviewed private file; it cannot undo the Google Drive file."})
    }
}
pub(crate) fn prepare(core: &Arc<Core>, input: &Value) -> Result<Prepared> {
    if input.to_string().len() > 40000
        || !jsonschema::validator_for(&schema("drive.download"))
            .map_err(|_| "DRIVE_SCHEMA")?
            .is_valid(input)
    {
        return Err("DRIVE_INPUT_INVALID".into());
    }
    let action = input["action"].as_str().ok_or("DRIVE_INPUT_MODE")?;
    closed_mode(
        input,
        match action {
            "list" => &["connection", "action"],
            "inspect" => &["connection", "action", "job_id"],
            "delete" => &["connection", "action", "job_id", "expected"],
            _ => return Err("DRIVE_INPUT_MODE".into()),
        },
    )?;
    let connection = uuid(input, "connection")?;
    let scope = Scope::new(core, &connection)?;
    let origin = scope.local_origin()?;
    let mut job = None;
    let mut observed = None;
    if action != "list" {
        let id = uuid(input, "job_id")?;
        let row = local::job(core, &connection, &id, "download")?;
        if action == "delete" {
            if input["expected"].as_object().is_none_or(|e| e.len() != 1)
                || input["expected"]["job_fingerprint"] != job_fingerprint(&row)
            {
                return Err("DRIVE_STALE_JOB_OBSERVATION".into());
            }
            if !["ready", "interrupted"].contains(&row["state"].as_str().unwrap_or("")) {
                return Err("DRIVE_DOWNLOAD_INSPECTION_REQUIRED".into());
            }
            let _lease = local::job_lease(core, &id)?;
            observed = Some(local::observe_download(&scope, &row)?.local);
        }
        job = Some(row);
    }
    Ok(Prepared {
        scope,
        input: input.clone(),
        origin,
        job,
        observed,
        risk: if action == "delete" {
            policy::Risk::Mandatory
        } else {
            policy::Risk::Free
        },
    })
}
fn inspect(scope: &Scope, id: &str) -> Result<Value> {
    scope.check()?;
    let row = local::job(&scope.core, &scope.id, id, "download")?;
    let _lease = match local::job_lease(&scope.core, id) {
        Ok(lease) => lease,
        Err(e) => {
            scope.check()?;
            return Ok(
                json!({"job":public_job(&row),"local_only":true,"delete_available":false,"diagnostic":e,"active_or_lock_unverified":true,"provider_completion_verified":false}),
            );
        }
    };
    // Refresh after taking the exclusive lease; a cooperating writer may have
    // published between the initial list query and acquiring this handle.
    let mut row = local::job(&scope.core, &scope.id, id, "download")?;
    if row["state"] == "deleted" {
        return Ok(
            json!({"job":public_job(&row),"local_only":true,"delete_available":false,"diagnostic":"deleted","provider_completion_verified":false}),
        );
    }
    match local::observe_download(scope, &row) {
        Ok(observed) => {
            if ["receiving", "failed"].contains(&row["state"].as_str().unwrap_or("")) {
                let mut record = row["record"].clone();
                record["local"] = observed.local.clone();
                record["interrupted_at"] = json!(storage::now());
                record["provider_completion_verified"] = json!(false);
                row = scope.local_guard(&observed.origin, || {
                    local::update(&scope.core, &row, "interrupted", &record)
                })?;
            }
            scope.check()?;
            Ok(
                json!({"job":public_job(&row),"local_only":true,"delete_available":(["ready","interrupted"].contains(&row["state"].as_str().unwrap_or(""))),"diagnostic":if observed.legacy{"legacy_origin_unverified"}else{"verified_local_identity"},"provider_completion_verified":row["state"]=="ready","local_bytes_observed":observed.local,"legacy_origin_unverified":observed.legacy,"no_remote_resume":true}),
            )
        }
        Err(e) => {
            scope.check()?;
            Ok(
                json!({"job":public_job(&row),"local_only":true,"delete_available":false,"diagnostic":e,"provider_completion_verified":false,"preserved":true,"no_remote_resume":true}),
            )
        }
    }
}
pub(crate) fn execute(p: Prepared) -> Result<Value> {
    p.scope.local_guard(&p.origin, || Ok(()))?;
    let action = p.input["action"].as_str().ok_or("DRIVE_INPUT_MODE")?;
    if action == "list" {
        let rows=p.scope.core.store.query("SELECT id FROM drive_jobs WHERE connection_id=? AND kind='download' ORDER BY created_at DESC,id LIMIT 100",&[json!(p.scope.id)])?;
        let mut jobs = vec![];
        for row in rows.as_array().ok_or("DRIVE_JOB_LIST")? {
            jobs.push(inspect(
                &p.scope,
                row["id"].as_str().ok_or("DRIVE_JOB_ID")?,
            )?);
        }
        p.scope.check()?;
        return Ok(json!({"jobs":jobs,"limit":100,"local_only":true,"no_remote_resume":true}));
    }
    let old = p.job.as_ref().ok_or("DRIVE_JOB_REQUIRED")?;
    let id = old["id"].as_str().ok_or("DRIVE_JOB_ID")?;
    if action == "inspect" {
        return inspect(&p.scope, id);
    }
    let _lease = local::job_lease(&p.scope.core, id)?;
    let row = local::job(&p.scope.core, &p.scope.id, id, "download")?;
    if row != *old {
        return Err("DRIVE_STALE_JOB_OBSERVATION".into());
    }
    let observed = local::observe_download(&p.scope, &row)?;
    if Some(&observed.local) != p.observed.as_ref() {
        return Err("DRIVE_DOWNLOAD_CHANGED".into());
    }
    observed.erase(&p.scope)?;
    let updated = local::update(&p.scope.core, &row, "deleted", &row["record"])
        .map_err(|_| "DRIVE_LOCAL_DELETE_JOURNAL_OUTCOME_UNCERTAIN")?;
    let mut result = public_job(&updated);
    result["local_only"] = json!(true);
    Ok(result)
}

#[cfg(test)]
pub(super) fn barrier(core: &Core, id: &str, phase: &str) -> Result<()> {
    if std::env::var("AP_DRIVE_DOWNLOAD_PHASE").as_deref() != Ok(phase) {
        return Ok(());
    }
    let nonce = std::env::var("AP_DRIVE_OWNED_NONCE").map_err(|_| "OWNED_DOWNLOAD_NONCE")?;
    if core.store.setting("drive.owned_process_fixture") != nonce
        || core
            .root
            .canonicalize()
            .map_err(|_| "OWNED_DOWNLOAD_ROOT")?
            != std::path::PathBuf::from(
                std::env::var("AP_DRIVE_OWNED_ROOT").map_err(|_| "OWNED_DOWNLOAD_ROOT")?,
            )
            .canonicalize()
            .map_err(|_| "OWNED_DOWNLOAD_ROOT")?
    {
        return Err("OWNED_DOWNLOAD_BINDING".into());
    }
    let root = core
        .root
        .canonicalize()
        .map_err(|_| "OWNED_DOWNLOAD_ROOT")?;
    let outer = root.parent().ok_or("OWNED_DOWNLOAD_ROOT")?;
    if outer.parent()
        != Some(
            std::env::temp_dir()
                .canonicalize()
                .map_err(|_| "OWNED_DOWNLOAD_ROOT")?
                .as_path(),
        )
        || !outer
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("ap-drive-files-"))
    {
        return Err("OWNED_DOWNLOAD_ROOT".into());
    }
    use std::io::Write;
    println!("OWNED_DOWNLOAD_BARRIER:{phase}:{id}");
    std::io::stdout()
        .flush()
        .map_err(|_| "OWNED_DOWNLOAD_SIGNAL")?;
    std::thread::sleep(std::time::Duration::from_secs(30));
    Err("OWNED_DOWNLOAD_BARRIER_NOT_TERMINATED".into())
}
