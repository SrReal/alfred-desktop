//! Explicit printer targets and conservative submission, never a claim of paper output.
use crate::{asset_files, policy::Risk, process, runtime::Core};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs::File, path::PathBuf, sync::Arc};
pub const TOOLS: &[&str] = &[
    "printers.list",
    "printers.default",
    "printers.queue",
    "printers.jobs",
    "printers.status",
    "printers.cancel_job",
    "printers.print",
];
pub fn risk(tool: &str) -> Risk {
    match tool {
        "printers.print" => Risk::Contextual,
        "printers.cancel_job" => Risk::Mandatory,
        _ => Risk::Free,
    }
}
pub fn schema(tool: &str) -> Value {
    let name =
        json!({"type":"string","minLength":1,"maxLength":256,"pattern":"^[^\\\"\\r\\n\\u0000]+$"});
    let mut properties = json!({}).as_object().unwrap().clone();
    let mut required = Vec::new();
    if !["printers.list", "printers.default"].contains(&tool) {
        properties.insert("name".into(), name);
        if tool != "printers.print" {
            required.push("name");
        }
    }
    if tool == "printers.print" {
        properties.insert(
            "path".into(),
            json!({"type":"string","minLength":1,"maxLength":4096}),
        );
        required.push("path");
    }
    if tool == "printers.cancel_job" {
        properties.insert(
            "job_id".into(),
            json!({"type":"integer","minimum":1,"maximum":2147483647}),
        );
        required.push("job_id");
    }
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
fn fingerprint(v: &Value) -> String {
    format!("{:x}", Sha256::digest(v.to_string().as_bytes()))
}
pub struct Prepared {
    tool: String,
    input: Value,
    printer: Value,
    job: Option<Value>,
    source: Option<(PathBuf, File, String, Vec<File>)>,
    roots: Value,
    authority: Value,
}
impl Prepared {
    pub async fn prepare(c: &Arc<Core>, tool: &str, input: &Value) -> Result<Self, String> {
        if !c.enabled("printers") || c.store.setting("privacy") == true {
            return Err("PRINTER_SCOPE_REVOKED".into());
        }
        let authority = crate::media::authority(c)?;
        if !TOOLS.contains(&tool)
            || !jsonschema::validator_for(&schema(tool))
                .map_err(|_| "PRINTER_SCHEMA")?
                .is_valid(input)
        {
            return Err("INVALID_TOOL_INPUT".into());
        }
        let mut input = input.clone();
        let mutation = ["printers.print", "printers.cancel_job"].contains(&tool);
        let printer = if tool == "printers.print" && input["name"].is_null() {
            let p = process::native(&c.native, "printers.default", &json!({})).await?;
            let name = p["Name"]
                .as_str()
                .ok_or("BLOCKED_EXTERNAL: PRINTER_DEFAULT_MISSING")?;
            input["name"] = json!(name);
            p
        } else if !mutation {
            Value::Null
        } else {
            process::native(&c.native, "printers.status", &json!({"name":input["name"]})).await?
        };
        if !jsonschema::validator_for(&schema(tool))
            .map_err(|_| "PRINTER_SCHEMA")?
            .is_valid(&input)
        {
            return Err("PRINT_TARGET_INVALID".into());
        }
        let job = if tool == "printers.cancel_job" {
            let jobs =
                process::native(&c.native, "printers.jobs", &json!({"name":input["name"]})).await?;
            Some(find_job(&jobs, &input["job_id"])?.clone())
        } else {
            None
        };
        let source = if tool == "printers.print" {
            let path = c.path(input["path"].as_str().ok_or("PRINTER_PATH")?, false)?;
            if path.extension().is_none_or(|e| {
                ![
                    "pdf", "txt", "png", "jpg", "jpeg", "bmp", "tif", "tiff", "docx", "xlsx",
                ]
                .iter()
                .any(|x| e.eq_ignore_ascii_case(x))
            }) {
                return Err("PRINT_FILE_FORMAT_UNSUPPORTED".into());
            }
            let parents = asset_files::parents(c, &path)?;
            let file = asset_files::open(&path, false)?;
            let bytes = asset_files::read(&file, 32 * 1024 * 1024)?;
            let sha = format!("{:x}", Sha256::digest(&bytes));
            Some((path, file, sha, parents))
        } else {
            None
        };
        let p = Self {
            tool: tool.into(),
            input,
            printer,
            job,
            source,
            roots: c.store.setting("allowed_roots"),
            authority,
        };
        p.scope(c)?;
        Ok(p)
    }
    pub fn review(&self) -> Value {
        json!({"operation":self.tool,"printer":self.printer,"job":self.job,"path":self.source.as_ref().map(|s|&s.0),"sha256":self.source.as_ref().map(|s|&s.2),"explicit_target":self.input["name"],"completion":"Submission only; physical output not guaranteed"})
    }
    fn scope(&self, c: &Core) -> Result<(), String> {
        if !c.enabled("printers")
            || c.store.setting("privacy") == true
            || c.store.setting("allowed_roots") != self.roots
            || crate::media::authority(c)? != self.authority
        {
            return Err("PRINTER_SCOPE_REVOKED".into());
        }
        Ok(())
    }
    async fn check(&self, c: &Arc<Core>) -> Result<(), String> {
        self.scope(c)?;
        if !self.printer.is_null() {
            let now = process::native(
                &c.native,
                "printers.status",
                &json!({"name":self.input["name"]}),
            )
            .await?;
            if printer_identity(&now) != printer_identity(&self.printer) {
                return Err("PRINTER_CHANGED_REVIEW_REQUIRED".into());
            }
        }
        if let Some(expected) = &self.job {
            let jobs = process::native(
                &c.native,
                "printers.jobs",
                &json!({"name":self.input["name"]}),
            )
            .await?;
            if find_job(&jobs, &self.input["job_id"])? != expected {
                return Err("PRINT_JOB_CHANGED_REVIEW_REQUIRED".into());
            }
        }
        if let Some((path, file, sha, _)) = &self.source {
            c.path(path.to_str().ok_or("PRINTER_PATH")?, false)?;
            let bytes = asset_files::read(file, 32 * 1024 * 1024)?;
            if format!("{:x}", Sha256::digest(bytes)) != *sha {
                return Err("PRINT_FILE_CHANGED".into());
            }
        }
        self.scope(c)
    }
    pub async fn execute(self, c: &Arc<Core>) -> Result<Value, String> {
        self.check(c).await?;
        let mut input = self.input.clone();
        if self.tool == "printers.print" {
            input["path"] = json!(self.source.as_ref().unwrap().0);
            input["_printer_identity"] = printer_identity(&self.printer);
        }
        if self.tool == "printers.cancel_job" {
            input["_job_fingerprint"] = json!(fingerprint(self.job.as_ref().unwrap()));
            input["_job"] = self.job.clone().unwrap();
        }
        let cancel = tokio_util::sync::CancellationToken::new();
        struct Cancel(tokio_util::sync::CancellationToken);
        impl Drop for Cancel {
            fn drop(&mut self) {
                self.0.cancel();
            }
        }
        let _cancel = Cancel(cancel.clone());
        let operation = process::native_cancellable(&c.native, &self.tool, &input, cancel.clone());
        tokio::pin!(operation);
        let mut timer = tokio::time::interval(std::time::Duration::from_millis(100));
        let result = loop {
            tokio::select! {
                result=&mut operation=>break result,
                _=timer.tick()=>{if !c.enabled("printers")||c.store.setting("privacy")==true||c.store.setting("allowed_roots")!=self.roots||crate::media::authority(c)?!=self.authority{cancel.cancel();let _=(&mut operation).await;break Err("PRINTER_SCOPE_REVOKED".into());}}
            }
        };
        result.map_err(|e| {
            if ["printers.print", "printers.cancel_job"].contains(&self.tool.as_str()) {
                format!("PRINT_OUTCOME_UNCERTAIN: {e}")
            } else {
                e
            }
        })
    }
}
fn printer_identity(v: &Value) -> Value {
    json!({"Name":v["Name"],"DriverName":v["DriverName"],"PortName":v["PortName"]})
}
fn find_job<'a>(jobs: &'a Value, id: &Value) -> Result<&'a Value, String> {
    jobs.as_array()
        .and_then(|a| a.iter().find(|j| j["ID"] == *id))
        .ok_or_else(|| "PRINT_JOB_NOT_FOUND".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn printer_closed_targets_and_submission_risks() {
        for tool in TOOLS {
            assert!(!jsonschema::validator_for(&schema(tool))
                .unwrap()
                .is_valid(&json!({"wildcard":true})));
        }
        let v = jsonschema::validator_for(&schema("printers.cancel_job")).unwrap();
        assert!(v.is_valid(&json!({"name":"Owned spool","job_id":3})));
        assert!(!v.is_valid(&json!({"name":"Owned spool","job_id":0})));
        assert!(!v.is_valid(&json!({"name":"wild\ncard","job_id":2})));
        assert_eq!(risk("printers.print"), Risk::Contextual);
        assert_eq!(risk("printers.cancel_job"), Risk::Mandatory);
        assert_eq!(risk("printers.default"), Risk::Free);
    }
    #[test]
    fn printer_identity_and_exact_job_do_not_select_a_neighbour() {
        let a = json!({"Name":"A","DriverName":"D","PortName":"P","PrinterStatus":0});
        let mut b = a.clone();
        b["PrinterStatus"] = json!(1);
        assert_eq!(printer_identity(&a), printer_identity(&b));
        b["PortName"] = json!("Other");
        assert_ne!(printer_identity(&a), printer_identity(&b));
        let jobs = json!([{"ID":1,"DocumentName":"Owned A"},{"ID":2,"DocumentName":"Owned B"}]);
        assert_eq!(
            find_job(&jobs, &json!(2)).unwrap()["DocumentName"],
            "Owned B"
        );
        assert!(find_job(&jobs, &json!(3)).is_err());
    }
}
#[cfg(all(test, windows))]
#[path = "printer_contracts.rs"]
mod contracts;
