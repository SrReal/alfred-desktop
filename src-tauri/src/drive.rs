//! Nine scoped Drive tools. Provider data is untrusted; mutations use fresh
//! client observations and never claim remote CAS, replay or generic undo.
use crate::{
    drive_auth::{self, Grant, Scope},
    drive_local::{self as local, Source},
    policy,
    runtime::Core,
    storage,
};
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
#[path = "drive_cleanup.rs"]
mod cleanup;
#[path = "drive_download.rs"]
mod local_download;
pub(crate) type Result<T> = std::result::Result<T, String>;
pub(crate) const TOOLS: &[&str] = &[
    "drive.search",
    "drive.list",
    "drive.metadata",
    "drive.download",
    "drive.upload",
    "drive.create_folder",
    "drive.move",
    "drive.rename",
    "drive.trash",
];
pub(crate) const FOLDER: &str = "application/vnd.google-apps.folder";
const SHORTCUT: &str = "application/vnd.google-apps.shortcut";
const FIELDS:&str="id,name,mimeType,parents,trashed,version,modifiedTime,size,sha256Checksum,driveId,shortcutDetails(targetId,targetMimeType),capabilities(canDownload,canEdit,canRename,canTrash,canAddChildren,canAddFolderFromAnotherDrive,canMoveItemWithinDrive,canMoveItemOutOfDrive)";
pub(crate) fn digest(v: &Value) -> String {
    format!("{:x}", Sha256::digest(v.to_string().as_bytes()))
}
fn rid(v: &Value, key: &str) -> Result<String> {
    let s = v[key].as_str().ok_or("DRIVE_RESOURCE_ID")?;
    drive_auth::resource_id(s)?;
    Ok(s.into())
}
fn uuid(v: &Value, key: &str) -> Result<String> {
    let s = v[key].as_str().ok_or("DRIVE_CONNECTION_ID")?;
    let u = uuid::Uuid::parse_str(s).map_err(|_| "DRIVE_CONNECTION_ID")?;
    if u.is_nil() || u.to_string() != s {
        return Err("DRIVE_CONNECTION_ID".into());
    }
    Ok(s.into())
}
pub(crate) fn risk(tool: &str, input: &Value) -> policy::Risk {
    if tool == "drive.trash"
        || tool == "drive.download" && input["action"] == "delete"
        || tool == "drive.upload"
            && ["abandon", "cleanup", "cleanup_receipt"]
                .contains(&input["action"].as_str().unwrap_or(""))
    {
        policy::Risk::Mandatory
    } else if [
        "drive.download",
        "drive.upload",
        "drive.create_folder",
        "drive.move",
        "drive.rename",
    ]
    .contains(&tool)
        && input["action"] != "list"
        && input["action"] != "status"
        && input["action"] != "inspect_cleanup"
        && input["action"] != "inspect_receipts"
        && input["action"] != "inspect"
    {
        policy::Risk::Contextual
    } else {
        policy::Risk::Free
    }
}
pub(crate) fn schema(tool: &str) -> Value {
    if !TOOLS.contains(&tool) {
        return json!({"not":{}});
    }
    let id = json!({"type":"string","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_-]+$"});
    let hash = json!({"type":"string","pattern":"^[a-f0-9]{64}$"});
    let name = json!({"type":"string","minLength":1,"maxLength":255});
    let mut p = serde_json::Map::new();
    p.insert(
        "connection".into(),
        json!({"type":"string","format":"uuid"}),
    );
    let mut required = vec!["connection"];
    let mut expected = serde_json::Map::new();
    let mut expected_required = vec![];
    if [
        "drive.metadata",
        "drive.rename",
        "drive.move",
        "drive.trash",
    ]
    .contains(&tool)
        || tool == "drive.download"
    {
        p.insert("file_id".into(), id.clone());
        p.insert("parent_id".into(), id.clone());
        if tool != "drive.download" {
            required.push("file_id");
        }
    }
    if ["drive.search", "drive.list"].contains(&tool) {
        p.insert("folder_id".into(), id.clone());
        required.push("folder_id");
        p.insert(
            "page_size".into(),
            json!({"type":"integer","minimum":1,"maximum":100}),
        );
        p.insert(
            "page_token".into(),
            json!({"type":"string","minLength":1,"maxLength":8192}),
        );
        p.insert("seen_cursors".into(),json!({"type":"array","maxItems":64,"uniqueItems":true,"items":{"type":"string","minLength":1,"maxLength":8192}}));
    }
    if tool == "drive.search" {
        p.insert(
            "query".into(),
            json!({"type":"string","minLength":1,"maxLength":2000}),
        );
        required.push("query");
    }
    if ["drive.create_folder", "drive.move", "drive.upload"].contains(&tool) {
        p.insert("folder_id".into(), id);
        if tool != "drive.upload" {
            required.push("folder_id");
        }
        expected.insert("parent_fingerprint".into(), hash.clone());
        if tool != "drive.upload" {
            expected_required.push("parent_fingerprint");
        }
    }
    if ["drive.create_folder", "drive.rename", "drive.upload"].contains(&tool) {
        p.insert("name".into(), name);
        if tool != "drive.upload" {
            required.push("name");
        }
    }
    if [
        "drive.rename",
        "drive.move",
        "drive.trash",
        "drive.download",
    ]
    .contains(&tool)
    {
        expected.insert("file_fingerprint".into(), hash.clone());
        if tool != "drive.download" {
            expected_required.push("file_fingerprint");
        }
    }
    if tool == "drive.download" {
        p.insert(
            "action".into(),
            json!({"enum":["fetch","list","inspect","delete"]}),
        );
        p.insert("job_id".into(), json!({"type":"string","format":"uuid"}));
        p.insert(
            "max_bytes".into(),
            json!({"type":"integer","minimum":1,"maximum":local::BYTE_LIMIT}),
        );
        p.insert("export_mime".into(),json!({"enum":["application/pdf","text/plain","application/vnd.openxmlformats-officedocument.wordprocessingml.document","application/vnd.openxmlformats-officedocument.spreadsheetml.sheet","application/vnd.openxmlformats-officedocument.presentationml.presentation"]}));
        expected.insert("job_fingerprint".into(), hash.clone());
    }
    if tool == "drive.upload" {
        p.insert(
            "action".into(),
            json!({"enum":["start","status","resume","abandon","list","inspect_cleanup","cleanup","inspect_receipts","cleanup_receipt"]}),
        );
        required.push("action");
        p.insert("job_id".into(), json!({"type":"string","format":"uuid"}));
        p.insert(
            "path".into(),
            json!({"type":"string","minLength":1,"maxLength":32768}),
        );
        p.insert(
            "mime_type".into(),
            json!({"type":"string","minLength":1,"maxLength":128}),
        );
        expected.insert(
            "capability_version".into(),
            json!({"type":"string","format":"uuid"}),
        );
        expected.insert(
            "receipt_generation".into(),
            json!({"type":"string","format":"uuid"}),
        );
        expected.insert(
            "receipt_revision".into(),
            json!({"type":"string","format":"uuid"}),
        );
        expected.insert("source_sha256".into(), hash.clone());
        expected.insert("job_fingerprint".into(), hash);
    }
    if !expected.is_empty() {
        p.insert("expected".into(),json!({"type":"object","required":expected_required,"properties":expected,"additionalProperties":false}));
        if !["drive.download", "drive.upload"].contains(&tool) {
            required.push("expected");
        }
    }
    json!({"type":"object","required":required,"properties":p,"additionalProperties":false})
}
fn text(s: &str, max: usize) -> Result<()> {
    if s.is_empty() || s.len() > max || s.chars().any(|c| c.is_control()) {
        return Err("DRIVE_TEXT_INVALID".into());
    }
    Ok(())
}
pub(crate) fn normalize(v: &Value) -> Result<Value> {
    let id = rid(v, "id")?;
    let name = v["name"].as_str().ok_or("DRIVE_FILE_RESPONSE")?;
    text(name, 1024)?;
    let mime = v["mimeType"].as_str().ok_or("DRIVE_FILE_RESPONSE")?;
    text(mime, 128)?;
    if !mime.is_ascii() || !mime.contains('/') || !v["trashed"].is_boolean() {
        return Err("DRIVE_FILE_RESPONSE".into());
    }
    let version = v["version"].as_str().ok_or("DRIVE_FILE_VERSION")?;
    if version.is_empty() || version.len() > 32 || !version.bytes().all(|b| b.is_ascii_digit()) {
        return Err("DRIVE_FILE_VERSION".into());
    }
    let modified = v["modifiedTime"].as_str().ok_or("DRIVE_FILE_VERSION")?;
    chrono::DateTime::parse_from_rfc3339(modified).map_err(|_| "DRIVE_FILE_VERSION")?;
    let parents = v.get("parents").cloned().unwrap_or(json!([]));
    let a = parents
        .as_array()
        .filter(|a| a.len() <= 1)
        .ok_or("DRIVE_FILE_PARENTS")?;
    for id in a {
        drive_auth::resource_id(id.as_str().ok_or("DRIVE_FILE_PARENTS")?)?;
    }
    let mut caps = serde_json::Map::new();
    if !v["capabilities"].is_object() {
        return Err("DRIVE_FILE_CAPABILITIES".into());
    }
    for key in [
        "canDownload",
        "canEdit",
        "canRename",
        "canTrash",
        "canAddChildren",
        "canAddFolderFromAnotherDrive",
        "canMoveItemWithinDrive",
        "canMoveItemOutOfDrive",
    ] {
        let value = v["capabilities"].get(key).cloned().unwrap_or(Value::Null);
        if !value.is_null() && !value.is_boolean() {
            return Err("DRIVE_FILE_CAPABILITIES".into());
        }
        caps.insert(key.into(), value);
    }
    let size = if let Some(s) = v.get("size") {
        Some(
            s.as_str()
                .ok_or("DRIVE_FILE_SIZE")?
                .parse::<u64>()
                .map_err(|_| "DRIVE_FILE_SIZE")?,
        )
    } else {
        None
    };
    let checksum = v.get("sha256Checksum").cloned().unwrap_or(Value::Null);
    if !checksum.is_null()
        && checksum.as_str().is_none_or(|s| {
            s.len() != 64
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        })
    {
        return Err("DRIVE_FILE_CHECKSUM".into());
    }
    let drive = v.get("driveId").cloned().unwrap_or(Value::Null);
    if !drive.is_null() {
        drive_auth::resource_id(drive.as_str().ok_or("DRIVE_FILE_RESPONSE")?)?;
    }
    let mut file = json!({"id":id,"name":name,"mimeType":mime,"parents":parents,"trashed":v["trashed"],"version":version,"modifiedTime":modified,"size":size,"sha256Checksum":checksum,"driveId":drive,"capabilities":caps});
    if mime == SHORTCUT {
        let details = &v["shortcutDetails"];
        let target_id = rid(details, "targetId")?;
        let target_mime = details["targetMimeType"]
            .as_str()
            .ok_or("DRIVE_SHORTCUT_METADATA")?;
        text(target_mime, 128)?;
        if !target_mime.is_ascii() || !target_mime.contains('/') {
            return Err("DRIVE_SHORTCUT_METADATA".into());
        }
        // Describe only this scoped shortcut. Neither its target nor a resource
        // key grants local scope, and no operation follows the target.
        file["shortcutDetails"] = json!({"targetId":target_id,"targetMimeType":target_mime});
    } else if v.get("shortcutDetails").is_some_and(|v| !v.is_null()) {
        return Err("DRIVE_SHORTCUT_METADATA".into());
    }
    Ok(file)
}
pub(crate) struct Session {
    pub grant: Grant,
}
impl Session {
    pub(crate) async fn json(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
        sent: Option<&AtomicBool>,
    ) -> Result<Value> {
        let until = self
            .grant
            .scope
            .core
            .store
            .setting(&format!("drive.cooldown.{}", self.grant.scope.id))
            .as_i64()
            .unwrap_or(0);
        if until > chrono::Utc::now().timestamp() {
            return Err("DRIVE_PROVIDER_COOLDOWN".into());
        }
        let response = if let Some(sent) = sent {
            self.grant
                .request_bytes(
                    method,
                    path,
                    &[("content-type", "application/json".into())],
                    body.map_or(vec![], |b| b.to_string().into_bytes()),
                    sent,
                )
                .await?
        } else {
            self.grant.request(method, path, body).await?
        };
        self.response_json(response).await
    }
    pub(crate) async fn response_json(&self, mut r: reqwest::Response) -> Result<Value> {
        let status = r.status().as_u16();
        if status == 429 {
            let seconds = r
                .headers()
                .get("retry-after")
                .and_then(|s| s.to_str().ok())
                .and_then(|s| s.parse::<i64>().ok())
                .filter(|s| (1..=600).contains(s))
                .unwrap_or(30);
            self.grant.scope.core.store.set_setting(
                &format!("drive.cooldown.{}", self.grant.scope.id),
                &json!(chrono::Utc::now().timestamp() + seconds),
            )?;
        }
        if !(200..300).contains(&status) {
            return Err(format!("DRIVE_PROVIDER_HTTP_{status}"));
        }
        if r.headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .is_none_or(|s| s.split(';').next() != Some("application/json"))
        {
            return Err("DRIVE_RESPONSE_CONTENT_TYPE".into());
        }
        if r.content_length().is_some_and(|n| n > 2 * 1024 * 1024) {
            return Err("DRIVE_RESPONSE_LIMIT".into());
        }
        let mut bytes = Vec::new();
        self.grant
            .guard(async {
                while let Some(chunk) = r.chunk().await.map_err(|_| "DRIVE_RESPONSE_READ")? {
                    if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
                        return Err("DRIVE_RESPONSE_LIMIT".into());
                    }
                    bytes.extend_from_slice(&chunk);
                }
                serde_json::from_slice(&bytes).map_err(|_| "DRIVE_RESPONSE_JSON".into())
            })
            .await
    }
    pub(crate) async fn observe(&self, id: &str, parent: Option<&str>) -> Result<Value> {
        let path = endpoint(
            &format!("/drive/v3/files/{id}"),
            &[
                ("fields", FIELDS.into()),
                ("supportsAllDrives", "true".into()),
            ],
        )?;
        let file = normalize(&self.json(reqwest::Method::GET, &path, None, None).await?)?;
        if file["id"] != id {
            return Err("DRIVE_FILE_ID_MISMATCH".into());
        }
        if self.grant.scope.allows("allowed_files", id).is_err()
            && self.grant.scope.allows("allowed_folders", id).is_err()
        {
            let p = parent.ok_or("DRIVE_RESOURCE_OUT_OF_SCOPE")?;
            self.grant.scope.allows("allowed_folders", p)?;
            if !file["parents"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v == p))
            {
                return Err("DRIVE_PARENT_OBSERVATION_CHANGED".into());
            }
        }
        Ok(file)
    }
}
pub(crate) fn endpoint(path: &str, params: &[(&str, String)]) -> Result<String> {
    let mut url = reqwest::Url::parse(&format!("https://www.googleapis.com{path}"))
        .map_err(|_| "DRIVE_API_ROUTE")?;
    for (k, v) in params {
        url.query_pairs_mut().append_pair(k, v);
    }
    Ok(format!(
        "{}{}",
        url.path(),
        url.query().map_or(String::new(), |q| format!("?{q}"))
    ))
}
fn check_target(scope: &Scope, input: &Value) -> Result<Option<String>> {
    if let Some(id) = input.get("file_id") {
        let id = id.as_str().ok_or("DRIVE_RESOURCE_ID")?;
        drive_auth::resource_id(id)?;
        if scope.allows("allowed_files", id).is_err()
            && scope.allows("allowed_folders", id).is_err()
        {
            scope.allows("allowed_folders", &rid(input, "parent_id")?)?;
        }
        if input.get("parent_id").is_some() {
            scope.allows("allowed_folders", &rid(input, "parent_id")?)?;
        }
        return Ok(Some(id.into()));
    }
    Ok(None)
}
pub(crate) fn writable(file: &Value, key: &str) -> Result<()> {
    if file["trashed"] != false || file["capabilities"][key] != true {
        return Err("DRIVE_FILE_NOT_WRITABLE".into());
    }
    Ok(())
}
pub(crate) fn folder(file: &Value) -> Result<()> {
    if file["mimeType"] != FOLDER || file["trashed"] != false {
        return Err("DRIVE_FOLDER_REQUIRED".into());
    }
    Ok(())
}
pub(crate) struct Prepared {
    pub s: Session,
    pub tool: String,
    pub input: Value,
    pub file: Option<Value>,
    pub parent: Option<Value>,
    pub source: Option<Source>,
    pub job: Option<Value>,
    pub risk: policy::Risk,
}
impl Prepared {
    pub fn review(&self) -> Value {
        json!({"connection":self.s.grant.scope.id,"operation":self.tool,"input":self.input,"review_content":{"file":self.file,"parent":self.parent,"source":self.source.as_ref().map(|s|&s.record),"job":self.job.as_ref().map(|j|&j["record"])},"warning":"Direct children only; Google per-file permission is separate from local IDs. Client observations are not remote CAS. A folder move between drives may affect descendants and inherited permissions; the server validates hierarchy/ownership constraints, and individual mandatory approval is required. Empty uploads use one multipart create, other uploads use resumable sessions. No automatic duplicate create, replay or remote undo. An abandoned upload can have a remote result."})
    }
}
fn closed_mode(input: &Value, allowed: &[&str]) -> Result<()> {
    let object = input.as_object().ok_or("DRIVE_INPUT_INVALID")?;
    if object.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err("DRIVE_INPUT_MODE_FIELDS".into());
    }
    Ok(())
}
fn move_preflight(file: &Value, parent: &Value) -> Result<()> {
    if file["id"] == parent["id"]
        || parent["parents"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == &file["id"]))
    {
        return Err("DRIVE_FOLDER_MOVE_CYCLE".into());
    }
    if file["parents"]
        .as_array()
        .is_some_and(|a| a.iter().any(|v| v == &parent["id"]))
    {
        return Err("DRIVE_MOVE_ALREADY_IN_FOLDER".into());
    }
    if !file["driveId"].is_null() && file["id"] == file["driveId"] {
        return Err("DRIVE_SHARED_DRIVE_ROOT_MOVE_DENIED".into());
    }
    if file["driveId"] == parent["driveId"] {
        writable(file, "canMoveItemWithinDrive")
    } else {
        // Google's addParents API cannot move My Drive folders into a shared
        // drive. Other transfers depend on the observed provider capabilities;
        // the server still validates ownership and descendant restrictions.
        if file["mimeType"] == FOLDER && file["driveId"].is_null() && !parent["driveId"].is_null() {
            return Err("DRIVE_MY_FOLDER_TO_SHARED_DRIVE_UNSUPPORTED".into());
        }
        if file["mimeType"] == FOLDER && !parent["driveId"].is_null() {
            writable(parent, "canAddFolderFromAnotherDrive")?;
        }
        writable(file, "canMoveItemOutOfDrive")
    }
}
pub(crate) enum PreparedOperation {
    Remote(Box<Prepared>),
    Local(Box<cleanup::Prepared>),
    LocalDownload(Box<local_download::Prepared>),
}
impl PreparedOperation {
    pub fn risk(&self) -> policy::Risk {
        match self {
            Self::Remote(p) => p.risk,
            Self::Local(p) => p.risk,
            Self::LocalDownload(p) => p.risk,
        }
    }
    pub fn review(&self) -> Value {
        match self {
            Self::Remote(p) => p.review(),
            Self::Local(p) => p.review(),
            Self::LocalDownload(p) => p.review(),
        }
    }
}
pub(crate) async fn prepare_operation(
    core: &Arc<Core>,
    tool: &str,
    input: &Value,
) -> Result<PreparedOperation> {
    if tool == "drive.upload"
        && [
            "inspect_cleanup",
            "cleanup",
            "inspect_receipts",
            "cleanup_receipt",
        ]
        .contains(&input["action"].as_str().unwrap_or(""))
    {
        Ok(PreparedOperation::Local(Box::new(cleanup::prepare(
            core, input,
        )?)))
    } else if tool == "drive.download"
        && ["list", "inspect", "delete"].contains(&input["action"].as_str().unwrap_or(""))
    {
        Ok(PreparedOperation::LocalDownload(Box::new(
            local_download::prepare(core, input)?,
        )))
    } else {
        Ok(PreparedOperation::Remote(Box::new(
            prepare(core, tool, input).await?,
        )))
    }
}
pub(crate) async fn execute_operation(p: PreparedOperation) -> Result<Value> {
    match p {
        PreparedOperation::Remote(p) => execute(*p).await,
        PreparedOperation::Local(p) => cleanup::execute(*p),
        PreparedOperation::LocalDownload(p) => local_download::execute(*p),
    }
}
pub(crate) async fn prepare(core: &Arc<Core>, tool: &str, input: &Value) -> Result<Prepared> {
    tokio::time::timeout(Duration::from_secs(65), async {
        if !TOOLS.contains(&tool)
            || !jsonschema::validator_for(&schema(tool))
                .map_err(|_| "DRIVE_SCHEMA")?
                .is_valid(input)
            || input.to_string().len() > 40000
        {
            return Err("DRIVE_INPUT_INVALID".into());
        }
        let connection = uuid(input, "connection")?;
        let scope = Scope::new(core, &connection)?;
        let target = check_target(&scope, input)?;
        if let Some(id) = input.get("folder_id") {
            scope.allows("allowed_folders", id.as_str().ok_or("DRIVE_FOLDER_ID")?)?;
        }
        for key in ["name", "query"] {
            if let Some(s) = input.get(key) {
                text(
                    s.as_str().ok_or("DRIVE_TEXT_INVALID")?,
                    if key == "name" { 1024 } else { 8000 },
                )?;
                if policy::contains_secret(&s.to_string()) {
                    return Err("DRIVE_SENSITIVE_INPUT".into());
                }
            }
        }
        let action = input["action"].as_str().unwrap_or("fetch");
        let remote_write = [
            "drive.create_folder",
            "drive.move",
            "drive.rename",
            "drive.trash",
        ]
        .contains(&tool)
            || tool == "drive.upload" && ["start", "resume"].contains(&action);
        if remote_write && scope.config["read_only"] != false {
            return Err("DRIVE_CONNECTION_READ_ONLY".into());
        }
        let mut job = None;
        let mut source = None;
        if ["drive.download", "drive.upload"].contains(&tool) {
            let kind = if tool == "drive.upload" {
                "upload"
            } else {
                "download"
            };
            match action {
                "list" => closed_mode(input, &["connection", "action"])?,
                "status" | "resume" | "abandon" | "delete" => {
                    closed_mode(input, &["connection", "action", "job_id", "expected"])?;
                    let id = uuid(input, "job_id")?;
                    let row = local::job(core, &connection, &id, kind)?;
                    if action != "status"
                        && input["expected"]["job_fingerprint"] != job_fingerprint(&row)
                    {
                        return Err("DRIVE_STALE_JOB_OBSERVATION".into());
                    }
                    job = Some(row);
                }
                "start" if tool == "drive.upload" => {
                    closed_mode(
                        input,
                        &[
                            "connection",
                            "action",
                            "folder_id",
                            "path",
                            "name",
                            "mime_type",
                            "expected",
                        ],
                    )?;
                    rid(input, "folder_id")?;
                    text(input["name"].as_str().ok_or("DRIVE_NAME_REQUIRED")?, 1024)?;
                    let mime = input["mime_type"]
                        .as_str()
                        .unwrap_or("application/octet-stream");
                    text(mime, 128)?;
                    if !mime.is_ascii()
                        || !mime.contains('/')
                        || mime.starts_with("application/vnd.google-apps.")
                    {
                        return Err("DRIVE_UPLOAD_MIME_TYPE".into());
                    }
                    let src =
                        Source::new(&scope, input["path"].as_str().ok_or("DRIVE_SOURCE_PATH")?)?;
                    if input["expected"]["source_sha256"] != src.record.sha256 {
                        return Err("DRIVE_SOURCE_STALE_HASH".into());
                    }
                    source = Some(src);
                }
                "fetch" if tool == "drive.download" => {
                    if target.is_none() || !input["expected"]["file_fingerprint"].is_string() {
                        return Err("DRIVE_DOWNLOAD_OBSERVATION_REQUIRED".into());
                    }
                    if input.get("job_id").is_some() {
                        return Err("DRIVE_INPUT_MODE_FIELDS".into());
                    }
                }
                _ => return Err("DRIVE_INPUT_MODE".into()),
            }
        }
        if tool == "drive.upload" {
            if let Some(row) = &job {
                let im = &row["record"]["immutable"];
                if action != "abandon" {
                    scope.allows(
                        "allowed_folders",
                        im["parent"]["id"].as_str().ok_or("DRIVE_FOLDER_ID")?,
                    )?;
                }
                if action != "abandon" && im["profile_fingerprint"] != digest(&scope.config) {
                    return Err("DRIVE_UPLOAD_PROFILE_CHANGED".into());
                }
                if action == "resume" {
                    let expected: local::SourceRecord =
                        serde_json::from_value(im["source"].clone())
                            .map_err(|_| "DRIVE_SOURCE_RECORD")?;
                    let src = Source::new(&scope, &expected.path)?;
                    if src.record != expected {
                        return Err("DRIVE_SOURCE_CHANGED".into());
                    }
                    source = Some(src);
                }
            }
        }
        let s = Session {
            grant: drive_auth::grant(core, &connection).await?,
        };
        let mut parent = None;
        let mut file = None;
        if let Some(id) = target {
            let f = s.observe(&id, input["parent_id"].as_str()).await?;
            if tool != "drive.metadata" && input["expected"]["file_fingerprint"] != digest(&f) {
                return Err("DRIVE_STALE_FILE_OBSERVATION".into());
            }
            file = Some(f);
        }
        if let Some(id) = input["folder_id"].as_str() {
            let p = s.observe(id, None).await?;
            folder(&p)?;
            if ["drive.create_folder", "drive.move", "drive.upload"].contains(&tool) {
                writable(&p, "canAddChildren")?;
                if input["expected"]["parent_fingerprint"] != digest(&p) {
                    return Err("DRIVE_STALE_PARENT_OBSERVATION".into());
                }
            }
            parent = Some(p);
        }
        if tool == "drive.upload" && action == "resume" {
            let id = job.as_ref().ok_or("DRIVE_JOB_REQUIRED")?["record"]["immutable"]["parent"]
                ["id"]
                .as_str()
                .ok_or("DRIVE_FOLDER_ID")?;
            let observed = s.observe(id, None).await?;
            folder(&observed)?;
            writable(&observed, "canAddChildren")?;
            if input["expected"]["parent_fingerprint"] != digest(&observed) {
                return Err("DRIVE_STALE_PARENT_OBSERVATION".into());
            }
            parent = Some(observed);
        }
        if let Some(f) = &file {
            match tool {
                "drive.rename" => writable(f, "canRename")?,
                "drive.move" => move_preflight(f, parent.as_ref().ok_or("DRIVE_FOLDER_REQUIRED")?)?,
                "drive.trash" => writable(f, "canTrash")?,
                "drive.download"
                    if f["trashed"] != false
                        || f["capabilities"]["canDownload"] != true
                        || f["mimeType"] == FOLDER
                        || f["mimeType"] == SHORTCUT =>
                {
                    return Err("DRIVE_DOWNLOAD_DENIED".into());
                }
                _ => (),
            }
        }
        s.grant.check()?;
        let effective_risk = if tool == "drive.move"
            && file.as_ref().is_some_and(|f| {
                f["mimeType"] == FOLDER
                    && parent
                        .as_ref()
                        .is_some_and(|p| f["driveId"] != p["driveId"])
            }) {
            policy::Risk::Mandatory
        } else {
            risk(tool, input)
        };
        Ok(Prepared {
            s,
            tool: tool.into(),
            input: input.clone(),
            file,
            parent,
            source,
            job,
            risk: effective_risk,
        })
    })
    .await
    .map_err(|_| "DRIVE_PREPARATION_TIMEOUT".to_string())?
}
pub(crate) fn job_fingerprint(row: &Value) -> Value {
    json!(digest(
        &json!({"id":row["id"],"connection":row["connection_id"],"kind":row["kind"],"state":row["state"],"revision":row["revision"],"record":row["record"]})
    ))
}
pub(crate) fn public_job(row: &Value) -> Value {
    json!({"id":row["id"],"state":row["state"],"revision":row["revision"],"record":row["record"],"job_fingerprint":job_fingerprint(row),"no_remote_undo":true,"untrusted":true})
}
pub(crate) async fn recheck(p: &Prepared) -> Result<()> {
    p.s.grant.check()?;
    if let Some(f) = &p.file {
        let current =
            p.s.observe(
                f["id"].as_str().ok_or("DRIVE_FILE_ID")?,
                p.input["parent_id"].as_str(),
            )
            .await?;
        if current != *f {
            return Err("DRIVE_STALE_FILE_OBSERVATION".into());
        }
    }
    if let Some(f) = &p.parent {
        let current =
            p.s.observe(f["id"].as_str().ok_or("DRIVE_FOLDER_ID")?, None)
                .await?;
        if current != *f {
            return Err("DRIVE_STALE_PARENT_OBSERVATION".into());
        }
    }
    if let Some(source) = &p.source {
        source.check(&p.s.grant.scope)?;
    }
    Ok(())
}
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}
fn cursor_binding(p: &Prepared) -> Result<String> {
    Ok(digest(
        &json!({"version":1,"connection":p.s.grant.scope.id,"authorization":p.s.grant.authorization_binding()?,"config":p.s.grant.scope.config,"folder":p.input["folder_id"],"query":p.input["query"],"page_size":p.input["page_size"]}),
    ))
}
fn cursor(raw: &str, binding: &str) -> Result<String> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| "DRIVE_CURSOR")?;
    if bytes.len() > 6000 {
        return Err("DRIVE_CURSOR".into());
    }
    let v: Value = serde_json::from_slice(&bytes).map_err(|_| "DRIVE_CURSOR")?;
    if !v.is_object()
        || v.as_object().is_none_or(|o| o.len() != 3)
        || v["version"] != 1
        || v["binding"] != binding
    {
        return Err("DRIVE_CURSOR_BINDING".into());
    }
    let token = v["token"].as_str().ok_or("DRIVE_CURSOR")?;
    text(token, 4096)?;
    Ok(token.into())
}
async fn list(p: &Prepared) -> Result<Value> {
    let folder = rid(&p.input, "folder_id")?;
    let binding = cursor_binding(p)?;
    let previous = if let Some(c) = p.input["page_token"].as_str() {
        Some(cursor(c, &binding)?)
    } else {
        None
    };
    let seen = p.input["seen_cursors"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|v| cursor(v.as_str().ok_or("DRIVE_CURSOR")?, &binding))
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    if previous.as_ref().is_some_and(|c| seen.contains(c)) {
        return Err("DRIVE_CURSOR_CYCLE".into());
    }
    let mut q = format!("trashed = false and '{}' in parents", escape(&folder));
    if p.tool == "drive.search" {
        q.push_str(&format!(
            " and name contains '{}'",
            escape(p.input["query"].as_str().ok_or("DRIVE_QUERY")?)
        ));
    }
    let count = p.input["page_size"].as_u64().unwrap_or(100);
    let mut params = vec![
        ("q", q),
        ("pageSize", count.to_string()),
        (
            "fields",
            format!("nextPageToken,incompleteSearch,files({FIELDS})"),
        ),
        ("spaces", "drive".into()),
        ("supportsAllDrives", "true".into()),
        ("includeItemsFromAllDrives", "true".into()),
        ("orderBy", "name_natural".into()),
    ];
    if let Some(c) = &previous {
        params.push(("pageToken", c.clone()));
    }
    let value =
        p.s.json(
            reqwest::Method::GET,
            &endpoint("/drive/v3/files", &params)?,
            None,
            None,
        )
        .await?;
    let rows = value["files"]
        .as_array()
        .filter(|a| a.len() <= count as usize)
        .ok_or("DRIVE_LIST_RESPONSE")?;
    let mut files = vec![];
    let mut filtered = 0;
    for raw in rows {
        let f = normalize(raw)?;
        if f["trashed"] != false
            || !f["parents"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v == &folder))
        {
            filtered += 1;
            continue;
        }
        if files.iter().any(|v: &Value| v["id"] == f["id"]) {
            return Err("DRIVE_LIST_DUPLICATE".into());
        }
        let fp = digest(&f);
        files.push(json!({"file":f,"file_fingerprint":fp}));
    }
    let next = if let Some(c) = value.get("nextPageToken") {
        let c = c.as_str().ok_or("DRIVE_CURSOR")?;
        text(c, 4096)?;
        if previous.as_ref().is_some_and(|v| v == c) || seen.iter().any(|v| v == c) {
            return Err("DRIVE_CURSOR_CYCLE".into());
        }
        Some(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(json!({"version":1,"binding":binding,"token":c}).to_string()),
        )
    } else {
        None
    };
    Ok(
        json!({"files":files,"next_cursor":next,"filtered_out_of_scope":filtered,"incomplete_search":value["incompleteSearch"].as_bool().unwrap_or(false),"manual_pagination":true,"untrusted":true,"scope":"direct_children_only"}),
    )
}
pub(crate) fn timeout_error(tool: &str) -> String {
    if [
        "drive.upload",
        "drive.create_folder",
        "drive.move",
        "drive.rename",
        "drive.trash",
    ]
    .contains(&tool)
    {
        "DRIVE_WRITE_OUTCOME_UNCERTAIN: TOOL_TIMEOUT".into()
    } else {
        "DRIVE_TOOL_TIMEOUT".into()
    }
}
pub(crate) async fn execute(p: Prepared) -> Result<Value> {
    if p.tool == "drive.upload" {
        return super::drive_upload::execute(p).await;
    }
    if p.tool == "drive.download" {
        return download(p).await;
    }
    recheck(&p).await?;
    if ["drive.search", "drive.list"].contains(&p.tool.as_str()) {
        return list(&p).await;
    }
    if p.tool == "drive.metadata" {
        let file = p.file.ok_or("DRIVE_FILE_REQUIRED")?;
        return Ok(json!({"file_fingerprint":digest(&file),"file":file,"untrusted":true}));
    }
    let sent = AtomicBool::new(false);
    let result:Result<Value>=async {
        let (method,path,body)=if p.tool=="drive.create_folder" {
            (reqwest::Method::POST,endpoint("/drive/v3/files",&[("supportsAllDrives","true".into()),("fields",FIELDS.into())])?,json!({"name":p.input["name"],"mimeType":FOLDER,"parents":[p.input["folder_id"].clone()]}))
        }else{
            let f=p.file.as_ref().ok_or("DRIVE_FILE_REQUIRED")?;let id=f["id"].as_str().ok_or("DRIVE_FILE_ID")?;
            let mut params=vec![("supportsAllDrives","true".into()),("fields",FIELDS.into())];
            let body=match p.tool.as_str(){"drive.rename"=>json!({"name":p.input["name"]}),"drive.trash"=>json!({"trashed":true}),"drive.move"=>{
                let dest=p.parent.as_ref().ok_or("DRIVE_FOLDER_REQUIRED")?;
                move_preflight(f,dest)?;
                let parent=p.input["folder_id"].as_str().ok_or("DRIVE_FOLDER_ID")?;
                params.push(("addParents",parent.into()));
                if let Some(old)=f["parents"][0].as_str(){params.push(("removeParents",old.into()));}
                json!({})},_=>return Err("DRIVE_TOOL_UNKNOWN".into())};
            (reqwest::Method::PATCH,endpoint(&format!("/drive/v3/files/{id}"),&params)?,body)
        };
        let file=normalize(&p.s.json(method,&path,Some(&body),Some(&sent)).await?)?;
        if p.tool=="drive.create_folder" {if file["mimeType"]!=FOLDER||file["name"]!=p.input["name"]||file["parents"]!=json!([p.input["folder_id"].clone()])||file["trashed"]!=false {return Err("DRIVE_WRITE_RESPONSE_MISMATCH".into());}}
        else{if file["id"]!=p.input["file_id"] {return Err("DRIVE_WRITE_RESPONSE_MISMATCH".into());}match p.tool.as_str(){"drive.rename" if file["name"]!=p.input["name"]=>return Err("DRIVE_WRITE_RESPONSE_MISMATCH".into()),"drive.move" if file["parents"]!=json!([p.input["folder_id"].clone()]) || file["driveId"]!=p.parent.as_ref().ok_or("DRIVE_FOLDER_REQUIRED")?["driveId"]=>return Err("DRIVE_WRITE_RESPONSE_MISMATCH".into()),"drive.trash" if file["trashed"]!=true=>return Err("DRIVE_WRITE_RESPONSE_MISMATCH".into()),_=>()}}
        p.s.grant.check()?;Ok(json!({"file_fingerprint":digest(&file),"file":file,"untrusted":true,"created_folder_requires_explicit_scope":p.tool=="drive.create_folder","remote_cas":false,"undo_available":false}))
    }.await;
    result.map_err(|e| {
        if sent.load(Ordering::SeqCst) {
            format!("DRIVE_WRITE_OUTCOME_UNCERTAIN: {e}")
        } else {
            e
        }
    })
}
async fn download(mut p: Prepared) -> Result<Value> {
    let scope = &p.s.grant.scope;
    let core = &scope.core;
    let action = p.input["action"].as_str().unwrap_or("fetch");
    if action != "fetch" {
        // Internal callers also use the guarded local implementation. Public
        // operations reach it before obtaining any OAuth grant.
        return local_download::execute(local_download::prepare(core, &p.input)?);
    }
    recheck(&p).await?;
    let file = p.file.as_ref().ok_or("DRIVE_FILE_REQUIRED")?;
    let mime = file["mimeType"].as_str().ok_or("DRIVE_FILE_RESPONSE")?;
    let max = p.input["max_bytes"].as_u64().unwrap_or(25 * 1024 * 1024);
    let id = storage::id();
    let _lease = local::job_lease(core, &id)?;
    let params = if mime.starts_with("application/vnd.google-apps.") {
        let export = p.input["export_mime"]
            .as_str()
            .ok_or("DRIVE_EXPORT_MIME_REQUIRED")?;
        let allowed = match mime {
            "application/vnd.google-apps.document" => [
                "application/pdf",
                "text/plain",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            ]
            .contains(&export),
            "application/vnd.google-apps.spreadsheet" => [
                "application/pdf",
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            ]
            .contains(&export),
            "application/vnd.google-apps.presentation" => [
                "application/pdf",
                "text/plain",
                "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            ]
            .contains(&export),
            _ => false,
        };
        if !allowed {
            return Err("DRIVE_EXPORT_MIME_UNSUPPORTED".into());
        }
        (
            format!("/drive/v3/files/{}/export", rid(&p.input, "file_id")?),
            vec![("mimeType", export.into())],
        )
    } else {
        if p.input.get("export_mime").is_some() {
            return Err("DRIVE_EXPORT_MIME_UNEXPECTED".into());
        }
        if file["size"].as_u64().is_none_or(|s| s > max) {
            return Err("DRIVE_DOWNLOAD_BYTE_LIMIT".into());
        }
        (
            format!("/drive/v3/files/{}", rid(&p.input, "file_id")?),
            vec![
                ("alt", "media".into()),
                ("supportsAllDrives", "true".into()),
            ],
        )
    };
    let mut destination = local::Destination::new(core, &id)?;
    #[cfg(test)]
    local_download::barrier(core, &id, "create")?;
    let initial = destination.initial(scope)?;
    let record =
        json!({"remote":file,"initial":initial,"local":{"id":id},"requested_at":storage::now()});
    local::insert(core, &scope.id, &id, "download", "receiving", &record)?;
    let original = local::job(core, &scope.id, &id, "download")?;
    #[cfg(test)]
    local_download::barrier(core, &id, "journal")?;
    let result: Result<Value> = async {
        let mut r =
            p.s.grant
                .request(reqwest::Method::GET, &endpoint(&params.0, &params.1)?, None)
                .await?;
        if r.status() != reqwest::StatusCode::OK {
            return Err(format!("DRIVE_DOWNLOAD_HTTP_{}", r.status().as_u16()));
        }
        let length = r.content_length();
        if length.is_some_and(|n| n > max) {
            return Err("DRIVE_DOWNLOAD_BYTE_LIMIT".into());
        }
        let mut size = 0u64;
        let mut hash = Sha256::new();
        p.s.grant
            .guard(async {
                while let Some(bytes) = r.chunk().await.map_err(|_| "DRIVE_DOWNLOAD_READ")? {
                    size += bytes.len() as u64;
                    if size > max {
                        return Err("DRIVE_DOWNLOAD_BYTE_LIMIT".into());
                    }
                    destination.write(&bytes)?;
                    #[cfg(test)]
                    if size == bytes.len() as u64 { local_download::barrier(core, &id, "first_chunk")?; }
                    hash.update(&bytes);
                }
                Ok(())
            })
            .await?;
        if length.is_some_and(|n| n != size)
            || !mime.starts_with("application/vnd.google-apps.")
                && file["size"].as_u64() != Some(size)
        {
            return Err("DRIVE_DOWNLOAD_SIZE_MISMATCH".into());
        }
        let sha = format!("{:x}", hash.finalize());
        if let Some(expected) = file["sha256Checksum"].as_str() {
            if expected != sha {
                return Err("DRIVE_DOWNLOAD_CHECKSUM_MISMATCH".into());
            }
        }
        recheck(&p).await?;
        let stored = destination.finish(size, &sha)?;
        #[cfg(test)]
        local_download::barrier(core, &id, "fsync")?;
        let row = local::update(
            core,
            &original,
            "ready",
            &json!({"remote":file,"initial":initial,"local":stored,"requested_at":record["requested_at"],"downloaded_at":storage::now(),"provider_completion_verified":true}),
        )?;
        if let Err(e) = p.s.grant.check() {
            let _ = local::update(core, &row, "failed", &row["record"]);
            return Err(e);
        }
        #[cfg(test)]
        local_download::barrier(core, &id, "ready")?;
        destination.retain();
        Ok(public_job(&row))
    }
    .await;
    if result.is_err() {
        let _ = local::update(core, &original, "failed", &record);
    }
    let _ = &mut p;
    result
}
pub(crate) fn durable(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        if [
                            "query",
                            "name",
                            "path",
                            "source",
                            "local",
                            "file",
                            "files",
                            "record",
                            "body",
                            "review_content",
                            "page_token",
                            "next_cursor",
                            "seen_cursors",
                            "mime_type",
                            "export_mime",
                        ]
                        .contains(&k.as_str())
                        {
                            json!("[DRIVE_CONTENT_OMITTED]")
                        } else {
                            durable(v)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(durable).collect()),
        _ => v.clone(),
    }
}
#[cfg(all(test, windows))]
#[path = "drive_tests.rs"]
mod tests;
