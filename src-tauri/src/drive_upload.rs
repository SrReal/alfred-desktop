//! Durable, explicitly resumed uploads. Each resume probes acknowledged bytes;
//! uncertain creates/chunks never trigger an automatic new session or replay.
use crate::{
    drive::{self, Prepared, Result},
    drive_local::{self as local, Source},
    storage,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;
const CHUNK: usize = 1024 * 1024;
const MAX_CHUNKS: usize = 4;
fn immutable(row: &Value) -> &Value {
    &row["record"]["immutable"]
}
fn id(row: &Value) -> Result<&str> {
    row["id"].as_str().ok_or_else(|| "DRIVE_JOB_ID".into())
}
fn size(row: &Value) -> Result<u64> {
    immutable(row)["source"]["size"]
        .as_u64()
        .filter(|s| *s <= local::BYTE_LIMIT)
        .ok_or_else(|| "DRIVE_SOURCE_BYTE_LIMIT".into())
}
fn revision(p: &Prepared, row: &Value) -> Result<()> {
    if p.input["action"] != "status"
        && p.input["expected"]["job_fingerprint"] != drive::job_fingerprint(row)
    {
        return Err("DRIVE_STALE_JOB_OBSERVATION".into());
    }
    Ok(())
}
fn session_path(p: &Prepared, uri: &str) -> Result<String> {
    if uri.len() > 8192 {
        return Err("DRIVE_UPLOAD_SESSION_ORIGIN".into());
    }
    let u = reqwest::Url::parse(uri).map_err(|_| "DRIVE_UPLOAD_SESSION_ORIGIN")?;
    let base = p.s.grant.scope.api_base()?;
    if u.origin() != base.origin()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
        || u.path() != "/upload/drive/v3/files"
    {
        return Err("DRIVE_UPLOAD_SESSION_ORIGIN".into());
    }
    let mut seen = std::collections::HashSet::new();
    let mut upload = false;
    let mut resumable = false;
    for (key, value) in u.query_pairs() {
        if !seen.insert(key.to_string()) {
            return Err("DRIVE_UPLOAD_SESSION_QUERY".into());
        }
        match key.as_ref() {
            "uploadType" if value == "resumable" => resumable = true,
            "upload_id"
                if !value.is_empty()
                    && value.len() <= 4096
                    && value.bytes().all(|b| (33..=126).contains(&b)) =>
            {
                upload = true
            }
            "supportsAllDrives" if value == "true" => (),
            "fields" if value.len() <= 2048 && !value.chars().any(|c| c.is_control()) => (),
            _ => return Err("DRIVE_UPLOAD_SESSION_QUERY".into()),
        }
    }
    if !upload || !resumable {
        return Err("DRIVE_UPLOAD_SESSION_QUERY".into());
    }
    Ok(format!(
        "{}?{}",
        u.path(),
        u.query().ok_or("DRIVE_UPLOAD_SESSION_QUERY")?
    ))
}
fn ack(response: &reqwest::Response, total: u64) -> Result<u64> {
    let Some(range) = response.headers().get("range") else {
        return Ok(0);
    };
    let raw = range.to_str().map_err(|_| "DRIVE_UPLOAD_RANGE")?;
    let end = raw
        .strip_prefix("bytes=0-")
        .filter(|s| !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit()))
        .ok_or("DRIVE_UPLOAD_RANGE")?;
    end.parse::<u64>()
        .ok()
        .and_then(|n| n.checked_add(1))
        .filter(|n| *n <= total)
        .ok_or_else(|| "DRIVE_UPLOAD_RANGE".into())
}
async fn complete(p: &Prepared, row: &Value, response: reqwest::Response) -> Result<Value> {
    let raw = p.s.response_json(response).await?;
    let file_id = raw["id"].as_str().ok_or("DRIVE_UPLOAD_COMPLETION_ID")?;
    crate::drive_auth::resource_id(file_id)?;
    if row["record"]
        .get("completion_file_id")
        .is_some_and(|id| id.as_str() != Some(file_id))
    {
        return Err("DRIVE_UPLOAD_COMPLETION_ID_CONFLICT".into());
    }
    // A returned ID is evidence of the create result, even when the following
    // metadata read fails. Persist it before observing; status can reconcile
    // that exact ID without sending another create or any media.
    let mut record = row["record"].clone();
    record["completion_file_id"] = json!(file_id);
    let observed = local::update(&p.s.grant.scope.core, row, "completion_observed", &record)?;
    finish_observed(p, &observed).await
}
async fn finish_observed(p: &Prepared, row: &Value) -> Result<Value> {
    let file_id = row["record"]["completion_file_id"]
        .as_str()
        .ok_or("DRIVE_UPLOAD_COMPLETION_ID")?;
    crate::drive_auth::resource_id(file_id)?;
    let im = immutable(row);
    let parent = im["parent"]["id"].as_str().ok_or("DRIVE_FOLDER_ID")?;
    let file = p.s.observe(file_id, Some(parent)).await?;
    if file["name"] != im["name"]
        || file["mimeType"] != im["mime_type"]
        || file["parents"] != json!([parent])
        || file["trashed"] != false
        || file["size"].as_u64() != Some(size(row)?)
        || file["sha256Checksum"]
            .as_str()
            .is_some_and(|h| h != im["source"]["sha256"])
    {
        return Err("DRIVE_UPLOAD_COMPLETION_MISMATCH".into());
    }
    let mut record = row["record"].clone();
    record["remote_file"] = file;
    record["acknowledged"] = json!(size(row)?);
    record["cleanup_pending"] = json!(true);
    let completed = local::update(&p.s.grant.scope.core, row, "completed", &record)?;
    cleanup(p, &completed)
}
fn cap_version(row: &Value) -> Result<Uuid> {
    let value = row["record"]["capability_version"]
        .as_str()
        .ok_or("DRIVE_UPLOAD_CAPABILITY_MISSING")?;
    let u = Uuid::parse_str(value).map_err(|_| "DRIVE_UPLOAD_CAPABILITY_VERSION")?;
    if u.is_nil() || u.to_string() != value {
        return Err("DRIVE_UPLOAD_CAPABILITY_VERSION".into());
    }
    Ok(u)
}
fn cleanup(p: &Prepared, row: &Value) -> Result<Value> {
    let job = id(row)?;
    let mut record = row["record"].clone();
    let observed = p.s.grant.capability_observe(job)?;
    let im = immutable(row);
    let empty_without_session = im["upload_protocol"] == "empty_multipart_v1"
        && im["source"]["size"] == 0
        && record["capability_version"].is_null();
    let known_retired = record["capability_retirement"]["complete"] == true
        && record["capability_retirement"]["immutable_sha256"] == drive::digest(im);
    let done = match observed {
        None => empty_without_session || known_retired,
        Some(version)
            if Some(version)
                == row["record"]["capability_version"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok()) =>
        {
            let retired = p.s.grant.capability_delete(job, version)?;
            record["capability_retirement"] = json!({"version":version.to_string(),"immutable_sha256":drive::digest(im),"complete":retired,"reason":"terminal_retirement"});
            retired
        }
        Some(_) => false,
    };
    record["cleanup_pending"] = json!(!done);
    if record == row["record"] {
        return Ok(drive::public_job(row));
    }
    Ok(drive::public_job(&local::update(
        &p.s.grant.scope.core,
        row,
        row["state"].as_str().ok_or("DRIVE_JOB_STATE")?,
        &record,
    )?))
}
fn cap(p: &Prepared, row: &Value) -> Result<String> {
    let value = p.s.grant.capability_read(id(row)?, cap_version(row)?)?;
    if value.as_object().is_none_or(|o| o.len() != 6)
        || value["version"] != 1
        || value["job"] != row["id"]
        || value["immutable_sha256"] != drive::digest(immutable(row))
        || value["authorization_binding"] != p.s.grant.authorization_binding()?
    {
        return Err("DRIVE_UPLOAD_CAPABILITY_BINDING".into());
    }
    let created = value["created_at"]
        .as_i64()
        .ok_or("DRIVE_UPLOAD_CAPABILITY_TIME")?;
    let now = chrono::Utc::now().timestamp();
    if created > now + 5 || now - created > 7 * 86400 {
        return Err("DRIVE_UPLOAD_SESSION_LOCALLY_EXPIRED".into());
    }
    session_path(
        p,
        value["uri"].as_str().ok_or("DRIVE_UPLOAD_CAPABILITY_URI")?,
    )
}
async fn start(p: Prepared) -> Result<Value> {
    drive::recheck(&p).await?;
    let source = p.source.as_ref().ok_or("DRIVE_SOURCE_REQUIRED")?;
    let parent = p.parent.as_ref().ok_or("DRIVE_FOLDER_REQUIRED")?;
    let binding = p.s.grant.authorization_binding()?;
    let job = storage::id();
    let core = &p.s.grant.scope.core;
    let _lease = local::job_lease(core, &job)?;
    let capability_binding = if source.record.size > 0 {
        p.s.grant.capability_descriptor(&job)?
    } else {
        Value::Null
    };
    let im = json!({"capability_binding":capability_binding,"source":source.record,"parent":parent,"name":p.input["name"],"mime_type":p.input["mime_type"].as_str().unwrap_or("application/octet-stream"),"authorization_binding":binding,"profile_fingerprint":drive::digest(&p.s.grant.scope.config),"upload_protocol":if source.record.size==0{"empty_multipart_v1"}else{"resumable_v1"}});
    let record = json!({"immutable":im,"capability_version":null,"acknowledged":0,"sent_upper_bound":0,"cleanup_pending":false,"remote_file":null});
    local::insert(
        core,
        &p.s.grant.scope.id,
        &job,
        "upload",
        "prepared",
        &record,
    )?;
    let row = local::job(core, &p.s.grant.scope.id, &job, "upload")?;
    let row = local::update(core, &row, "initiating", &record)?;
    let sent = AtomicBool::new(false);
    let result:Result<Value>=async {
        source.check(&p.s.grant.scope)?;p.s.grant.check()?;
        let body=json!({"name":im["name"],"mimeType":im["mime_type"],"parents":[parent["id"].clone()]});
        if source.record.size==0 {
            // Explicit zero-length media in one bounded multipart create. An
            // empty status PUT is not interpreted as sent content, and a lost
            // create response can never initiate another request automatically.
            let boundary=format!("alfredpet-{}",Uuid::new_v4());
            let mime=im["mime_type"].as_str().ok_or("DRIVE_UPLOAD_MIME")?;
            let bytes=format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{body}\r\n--{boundary}\r\nContent-Type: {mime}\r\n\r\n\r\n--{boundary}--\r\n").into_bytes();
            let path=drive::endpoint("/upload/drive/v3/files",&[("uploadType","multipart".into()),("supportsAllDrives","true".into())])?;
            let response=p.s.grant.request_bytes(reqwest::Method::POST,&path,&[("content-type",format!("multipart/related; boundary={boundary}")),("content-length",bytes.len().to_string())],bytes,&sent).await?;
            if !matches!(response.status().as_u16(),200|201){return Err(format!("DRIVE_EMPTY_UPLOAD_HTTP_{}",response.status().as_u16()));}
            source.check(&p.s.grant.scope)?;p.s.grant.check()?;
            return complete(&p,&row,response).await;
        }
        let path=drive::endpoint("/upload/drive/v3/files",&[("uploadType","resumable".into()),("supportsAllDrives","true".into())])?;
        let r=p.s.grant.request_bytes(reqwest::Method::POST,&path,&[("content-type","application/json; charset=UTF-8".into()),("x-upload-content-type",im["mime_type"].as_str().ok_or("DRIVE_UPLOAD_MIME")?.into()),("x-upload-content-length",source.record.size.to_string())],body.to_string().into_bytes(),&sent).await?;
        if r.status()!=reqwest::StatusCode::OK{return Err(format!("DRIVE_UPLOAD_INIT_HTTP_{}",r.status().as_u16()));}
        if r.content_length().is_some_and(|n|n!=0){return Err("DRIVE_UPLOAD_INIT_RESPONSE_BODY".into());}
        let uri=r.headers().get("location").and_then(|v|v.to_str().ok()).ok_or("DRIVE_UPLOAD_SESSION_MISSING")?;
        session_path(&p,uri)?;
        let version=p.s.grant.capability_save(&job,&json!({"version":1,"job":job,"immutable_sha256":drive::digest(&im),"authorization_binding":binding,"created_at":chrono::Utc::now().timestamp(),"uri":uri}))?;
        let mut record=record.clone();record["capability_version"]=json!(version.to_string());
        p.s.grant.check()?;source.check(&p.s.grant.scope)?;
        let ready=local::update(core,&row,"active",&record)?;
        Ok(drive::public_job(&ready))
    }.await;
    if let Err(e) = &result {
        let mut r = record.clone();
        r["error"] = json!(if sent.load(Ordering::SeqCst) {
            "DRIVE_UPLOAD_INITIATION_UNCERTAIN"
        } else {
            "DRIVE_UPLOAD_NOT_DISPATCHED"
        });
        let _ = local::update(
            core,
            &row,
            if sent.load(Ordering::SeqCst) {
                "initiation_uncertain"
            } else {
                "failed"
            },
            &r,
        );
        let _ = e;
    }
    result.map_err(|e| {
        if sent.load(Ordering::SeqCst) {
            format!("DRIVE_UPLOAD_INITIATION_UNCERTAIN: {e}")
        } else {
            e
        }
    })
}
async fn advance(
    p: &Prepared,
    mut row: Value,
    source: Option<&Source>,
    path: &str,
) -> Result<Value> {
    let core = &p.s.grant.scope.core;
    let total = size(&row)?;
    let mut record = row["record"].clone();
    record["last_operation"] = json!("status");
    row = local::update(core, &row, "probing", &record)?;
    let sent = AtomicBool::new(false);
    let probe =
        p.s.grant
            .request_bytes(
                reqwest::Method::PUT,
                path,
                &[
                    ("content-range", format!("bytes */{total}")),
                    ("content-length", "0".into()),
                ],
                vec![],
                &sent,
            )
            .await;
    let r = match probe {
        Ok(r) => r,
        Err(e) => {
            let _ = local::update(core, &row, "status_uncertain", &record);
            return Err(format!("DRIVE_UPLOAD_STATUS_UNAVAILABLE: {e}"));
        }
    };
    let status = r.status().as_u16();
    if status == 200 || status == 201 {
        return complete(p, &row, r).await;
    }
    if status == 404 {
        let row = local::update(core, &row, "expired", &record)?;
        return cleanup(p, &row);
    }
    if status != 308 {
        let _ = local::update(core, &row, "status_uncertain", &record);
        return Err(format!("DRIVE_UPLOAD_STATUS_HTTP_{status}"));
    }
    let acknowledged = ack(&r, total)?;
    if acknowledged
        < record["acknowledged"]
            .as_u64()
            .ok_or("DRIVE_UPLOAD_JOURNAL_RANGE")?
        || acknowledged
            > record["sent_upper_bound"]
                .as_u64()
                .ok_or("DRIVE_UPLOAD_JOURNAL_RANGE")?
    {
        return Err("DRIVE_UPLOAD_RANGE_CONFLICT".into());
    }
    record["acknowledged"] = json!(acknowledged);
    row = local::update(
        core,
        &row,
        if acknowledged == total {
            "processing"
        } else {
            "active"
        },
        &record,
    )?;
    let Some(source) = source else {
        return Ok(drive::public_job(&row));
    };
    if acknowledged == total {
        return Ok(drive::public_job(&row));
    }
    for _ in 0..MAX_CHUNKS {
        let offset = row["record"]["acknowledged"]
            .as_u64()
            .ok_or("DRIVE_UPLOAD_JOURNAL_RANGE")?;
        let bytes = source.chunk(&p.s.grant.scope, offset, CHUNK)?;
        let end = offset + bytes.len() as u64;
        if end < total && bytes.len() % (256 * 1024) != 0 {
            return Err("DRIVE_UPLOAD_CHUNK_SIZE".into());
        }
        let mut record = row["record"].clone();
        record["last_operation"] = json!("chunk");
        record["sent_lower_bound"] = json!(offset);
        record["sent_upper_bound"] = json!(end.max(
            record["sent_upper_bound"]
                .as_u64()
                .ok_or("DRIVE_UPLOAD_JOURNAL_RANGE")?
        ));
        row = local::update(core, &row, "chunk_in_flight", &record)?;
        source.check(&p.s.grant.scope)?;
        p.s.grant.check()?;
        let dispatched = AtomicBool::new(false);
        let result =
            p.s.grant
                .request_bytes(
                    reqwest::Method::PUT,
                    path,
                    &[
                        (
                            "content-range",
                            format!("bytes {offset}-{}/{total}", end - 1),
                        ),
                        ("content-length", bytes.len().to_string()),
                        (
                            "content-type",
                            immutable(&row)["mime_type"]
                                .as_str()
                                .ok_or("DRIVE_UPLOAD_MIME")?
                                .into(),
                        ),
                    ],
                    bytes,
                    &dispatched,
                )
                .await;
        let r = match result {
            Ok(r) => r,
            Err(e) => {
                let _ = local::update(core, &row, "chunk_uncertain", &record);
                return Err(format!("DRIVE_UPLOAD_CHUNK_OUTCOME_UNCERTAIN: {e}"));
            }
        };
        let status = r.status().as_u16();
        if status == 200 || status == 201 {
            if end != total {
                return Err("DRIVE_UPLOAD_EARLY_COMPLETION".into());
            }
            return complete(p, &row, r).await;
        }
        if status == 404 {
            let row = local::update(core, &row, "expired", &record)?;
            return cleanup(p, &row);
        }
        if status != 308 {
            let _ = local::update(core, &row, "chunk_uncertain", &record);
            return Err(format!("DRIVE_UPLOAD_CHUNK_HTTP_{status}"));
        }
        let next = ack(&r, total)?;
        if next < offset || next > end {
            return Err("DRIVE_UPLOAD_RANGE_CONFLICT".into());
        }
        record["acknowledged"] = json!(next);
        row = local::update(
            core,
            &row,
            if next == total {
                "processing"
            } else {
                "active"
            },
            &record,
        )?;
        if next == offset || next == total {
            return Ok(drive::public_job(&row));
        }
    }
    Ok(drive::public_job(&row))
}
pub(crate) async fn execute(p: Prepared) -> Result<Value> {
    let action = p.input["action"].as_str().ok_or("DRIVE_UPLOAD_ACTION")?;
    if action == "start" {
        return start(p).await;
    }
    let scope = &p.s.grant.scope;
    let core = &scope.core;
    if action == "list" {
        let ids=core.store.query("SELECT id FROM drive_jobs WHERE connection_id=? AND kind='upload' ORDER BY created_at DESC,id LIMIT 100",&[json!(scope.id)])?;
        let mut jobs = vec![];
        for v in ids.as_array().ok_or("DRIVE_JOB_LIST")? {
            jobs.push(drive::public_job(&local::job(
                core,
                &scope.id,
                v["id"].as_str().ok_or("DRIVE_JOB_ID")?,
                "upload",
            )?));
        }
        return Ok(json!({"jobs":jobs,"limit":100}));
    }
    let old = p.job.as_ref().ok_or("DRIVE_JOB_REQUIRED")?;
    let job = id(old)?;
    let _lease = local::job_lease(core, job)?;
    let mut row = local::job(core, &scope.id, job, "upload")?;
    revision(&p, &row)?;
    let im = immutable(&row);
    if im["authorization_binding"] != p.s.grant.authorization_binding()?
        || action != "abandon" && im["profile_fingerprint"] != drive::digest(&scope.config)
    {
        return Err("DRIVE_UPLOAD_AUTHORIZATION_CHANGED".into());
    }
    let parent_id = im["parent"]["id"].as_str().ok_or("DRIVE_FOLDER_ID")?;
    if action != "abandon" {
        scope.allows("allowed_folders", parent_id)?;
    }
    if action == "resume" {
        drive::recheck(&p).await?;
    }
    if ["completed", "expired", "abandoned"].contains(&row["state"].as_str().unwrap_or("")) {
        return cleanup(&p, &row);
    }
    if action != "abandon" && row["record"]["completion_file_id"].is_string() {
        return finish_observed(&p, &row).await;
    }
    if size(&row)? == 0 && action != "abandon" {
        if im["upload_protocol"] != "empty_multipart_v1" {
            return Err("DRIVE_UPLOAD_PROTOCOL_BINDING".into());
        }
        if action == "resume" {
            return Err("DRIVE_EMPTY_UPLOAD_NO_AUTOMATIC_CREATE_REPLAY".into());
        }
        let mut result = drive::public_job(&row);
        result["status_probe_available"] = json!(false);
        result["manual_remote_inspection_required"] = json!(row["state"] != "failed");
        return Ok(result);
    }
    // Recover only an observed matching private capability after uncertain local
    // publication. This never initiates another remote upload session.
    if row["record"]["capability_version"].is_null() {
        if let Some(version) = p.s.grant.capability_observe(job)? {
            let mut record = row["record"].clone();
            record["capability_version"] = json!(version.to_string());
            let mut candidate = row.clone();
            candidate["record"] = record.clone();
            cap(&p, &candidate)?;
            row = local::update(core, &row, "active", &record)?;
        } else if action != "abandon" {
            return Err("DRIVE_UPLOAD_INITIATION_UNCERTAIN_NO_CAPABILITY".into());
        }
    }
    if action == "abandon" {
        let mut record = row["record"].clone();
        record["remote_result_possible"] = json!(true);
        record["cleanup_pending"] = json!(true);
        let row = local::update(core, &row, "abandoned", &record)?;
        return cleanup(&p, &row);
    }
    let path = match cap(&p, &row) {
        Ok(s) => s,
        Err(e) if e == "DRIVE_UPLOAD_SESSION_LOCALLY_EXPIRED" => {
            let row = local::update(core, &row, "expired", &row["record"])?;
            return cleanup(&p, &row);
        }
        Err(e) => return Err(e),
    };
    advance(
        &p,
        row,
        if action == "resume" {
            p.source.as_ref()
        } else {
            None
        },
        &path,
    )
    .await
}
