//! Explicit local retirement. No OAuth grant, refresh or Drive HTTP request.
use super::{closed_mode, digest, job_fingerprint, local, public_job, schema, uuid, Result};
use crate::{drive_auth::Scope, drive_credentials::Recovery, policy, runtime::Core};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub(crate) struct Prepared {
    scope: Scope,
    input: Value,
    job: Value,
    observed: Option<Uuid>,
    receipt: Option<Recovery>,
    pub risk: policy::Risk,
}
impl Prepared {
    pub fn review(&self) -> Value {
        json!({"connection":self.scope.id,"operation":"drive.upload","input":self.input,"review_content":{"job":self.job["record"],"observed_capability_version":self.observed,"receipt":self.receipt.as_ref().map(public_receipt)},"warning":"Local capability inspection/retirement only. Cleanup stops local continuation; it cannot undo a remote result. No OAuth renewal or remote request. Missing or changed native entries can leave unverified orphan parts."})
    }
}
fn public_receipt(r: &Recovery) -> Value {
    json!({"generation":r.version,"revision":r.revision,"state":r.state,"cleanup_available":r.can_retire,"retirement_verified":r.retirement_verified,"present_parts":r.present_parts,"absent_parts":r.absent_parts,"other_pointer_preserved":r.other_pointer_preserved})
}
fn no_capability(job: &Value) -> bool {
    job["record"]["immutable"]["upload_protocol"] == "empty_multipart_v1"
        && job["record"]["immutable"]["source"]["size"] == 0
        && job["record"]["capability_version"].is_null()
}
fn known_version(job: &Value, observed: Option<Uuid>) -> bool {
    job["record"]["capability_version"].is_null()
        || observed.map(|v| v.to_string()).as_deref()
            == job["record"]["capability_version"].as_str()
}
pub(crate) fn prepare(core: &Arc<Core>, input: &Value) -> Result<Prepared> {
    if input.to_string().len() > 40000
        || !jsonschema::validator_for(&schema("drive.upload"))
            .map_err(|_| "DRIVE_SCHEMA")?
            .is_valid(input)
    {
        return Err("DRIVE_INPUT_INVALID".into());
    }
    let receipt_mode =
        ["inspect_receipts", "cleanup_receipt"].contains(&input["action"].as_str().unwrap_or(""));
    let cleaning = input["action"] == "cleanup" || input["action"] == "cleanup_receipt";
    closed_mode(
        input,
        if cleaning {
            &["connection", "action", "job_id", "expected"]
        } else {
            &["connection", "action", "job_id"]
        },
    )?;
    let connection = uuid(input, "connection")?;
    let job_id = uuid(input, "job_id")?;
    let scope = Scope::new(core, &connection)?;
    let _lease = local::job_lease(core, &job_id)?;
    let job = local::job(core, &connection, &job_id, "upload")?;
    let observed = if no_capability(&job) || receipt_mode {
        None
    } else {
        scope.historical_capability_inspect(&job_id, &job["record"]["immutable"])?
    };
    let mut receipt = None;
    if receipt_mode {
        let receipts = if no_capability(&job) {
            vec![]
        } else {
            scope.historical_receipts(&job_id, &job["record"]["immutable"])?
        };
        if cleaning {
            if input["expected"].as_object().is_none_or(|e| e.len() != 3)
                || input["expected"]["job_fingerprint"] != job_fingerprint(&job)
            {
                return Err("DRIVE_STALE_JOB_OBSERVATION".into());
            }
            let generation = uuid(&input["expected"], "receipt_generation")?;
            let revision = uuid(&input["expected"], "receipt_revision")?;
            let found = receipts
                .into_iter()
                .find(|r| r.version.to_string() == generation && r.revision.to_string() == revision)
                .ok_or("DRIVE_RECEIPT_OBSERVATION_CHANGED")?;
            if !found.can_retire {
                return Err("DRIVE_RECEIPT_RECOVERY_DENIED".into());
            }
            receipt = Some(found);
        }
    } else if cleaning {
        let expected = input["expected"]
            .as_object()
            .ok_or("DRIVE_CLEANUP_OBSERVATION_REQUIRED")?;
        if expected.len() != 2 || input["expected"]["job_fingerprint"] != job_fingerprint(&job) {
            return Err("DRIVE_STALE_JOB_OBSERVATION".into());
        }
        let version = uuid(&input["expected"], "capability_version")?;
        if !known_version(&job, observed)
            || observed.map(|v| v.to_string()).as_deref() != Some(version.as_str())
        {
            return Err("DRIVE_CLEANUP_CAPABILITY_CHANGED_OR_MISSING".into());
        }
    }
    Ok(Prepared {
        scope,
        input: input.clone(),
        job,
        observed,
        receipt,
        risk: if cleaning {
            policy::Risk::Mandatory
        } else {
            policy::Risk::Free
        },
    })
}
pub(crate) fn execute(p: Prepared) -> Result<Value> {
    p.scope.check()?;
    let job_id = p.job["id"].as_str().ok_or("DRIVE_JOB_ID")?;
    let _lease = local::job_lease(&p.scope.core, job_id)?;
    let current = local::job(&p.scope.core, &p.scope.id, job_id, "upload")?;
    if current != p.job {
        return Err("DRIVE_STALE_JOB_OBSERVATION".into());
    }
    let im = &current["record"]["immutable"];
    if ["inspect_receipts", "cleanup_receipt"].contains(&p.input["action"].as_str().unwrap_or("")) {
        let receipts = if no_capability(&current) {
            vec![]
        } else {
            p.scope.historical_receipts(job_id, im)?
        };
        if p.input["action"] == "inspect_receipts" {
            return Ok(
                json!({"job":public_job(&current),"local_only":true,"receipts":receipts.iter().map(public_receipt).collect::<Vec<_>>(),"unknown_legacy_parts_unverified":receipts.is_empty()&&!no_capability(&current),"no_remote_undo":true}),
            );
        }
        let reviewed = p
            .receipt
            .as_ref()
            .ok_or("DRIVE_RECEIPT_OBSERVATION_REQUIRED")?;
        let fresh = receipts
            .iter()
            .find(|r| r.version == reviewed.version)
            .ok_or("DRIVE_RECEIPT_OBSERVATION_CHANGED")?;
        if fresh != reviewed {
            return Err("DRIVE_RECEIPT_OBSERVATION_CHANGED".into());
        }
        let complete = p.scope.historical_receipt_retire(job_id, im, reviewed)?;
        let remaining = p.scope.historical_receipts(job_id, im)?;
        let all_retired =
            complete && !remaining.is_empty() && remaining.iter().all(|r| r.retirement_verified);
        let mut record = current["record"].clone();
        record["cleanup_pending"] =
            json!(!all_retired || remaining.iter().any(|r| r.other_pointer_preserved));
        record["receipt_retirement"] = json!({"generation":reviewed.version,"complete":complete,"immutable_sha256":digest(im),"reason":"explicit_protected_receipt_retirement"});
        if all_retired
            && (record["capability_version"].is_null()
                || record["capability_version"] == reviewed.version.to_string())
        {
            record["capability_retirement"] = json!({"version":reviewed.version,"complete":true,"immutable_sha256":digest(im),"reason":"protected_receipt_retirement"});
        }
        let state = current["state"].as_str().ok_or("DRIVE_JOB_STATE")?;
        let state = if ["completed", "expired", "abandoned"].contains(&state) {
            state
        } else {
            "abandoned"
        };
        let updated = local::update(&p.scope.core, &current, state, &record)
            .map_err(|_| "DRIVE_LOCAL_CLEANUP_JOURNAL_OUTCOME_UNCERTAIN")?;
        return Ok(
            json!({"job":public_job(&updated),"local_only":true,"known_receipt_retired":complete,"all_known_receipts_retired":all_retired,"no_remote_undo":true}),
        );
    }
    let observed = if no_capability(&current) {
        None
    } else {
        p.scope.historical_capability_inspect(job_id, im)?
    };
    if observed != p.observed {
        return Err("DRIVE_CLEANUP_CAPABILITY_CHANGED_OR_MISSING".into());
    }
    if p.input["action"] == "inspect_cleanup" {
        let retired = current["record"]["capability_retirement"]["complete"] == true
            && current["record"]["capability_retirement"]["immutable_sha256"] == digest(im)
            && current["record"]["cleanup_pending"] == false;
        let state = if no_capability(&current) {
            "not_applicable"
        } else if observed.is_some() && !known_version(&current, observed) {
            "changed_known_generation_preserved"
        } else if observed.is_some() {
            "observed"
        } else if retired {
            "retired_known_generation"
        } else {
            "missing_or_retired_unverified"
        };
        return Ok(
            json!({"job":public_job(&current),"local_only":true,"capability_version":observed,"cleanup_available":observed.is_some()&&known_version(&current,observed),"capability_state":state,"orphan_parts_unverified":!no_capability(&current)&&((observed.is_none()&&!retired)||(observed.is_some()&&!known_version(&current,observed))),"no_remote_undo":true}),
        );
    }
    let version = observed.ok_or("DRIVE_CLEANUP_CAPABILITY_CHANGED_OR_MISSING")?;
    let complete = p.scope.historical_capability_retire(job_id, im, version)?;
    let mut record = current["record"].clone();
    record["cleanup_pending"] = json!(!complete);
    record["capability_retirement"] = json!({"version":version,"immutable_sha256":digest(im),"complete":complete,"reason":"explicit_local_retirement"});
    let state = current["state"].as_str().ok_or("DRIVE_JOB_STATE")?;
    let state = if ["completed", "expired", "abandoned"].contains(&state) {
        state
    } else {
        "abandoned"
    };
    let updated = local::update(&p.scope.core, &current, state, &record)
        .map_err(|_| "DRIVE_LOCAL_CLEANUP_JOURNAL_OUTCOME_UNCERTAIN")?;
    let mut result = public_job(&updated);
    result["local_only"] = json!(true);
    result["known_capability_retired"] = json!(complete);
    result["orphan_parts_unverified"] = json!(!complete);
    Ok(result)
}
