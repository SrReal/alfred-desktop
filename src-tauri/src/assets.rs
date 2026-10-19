//! Managed assets: immutable bytes/identity, private directories, references and bounded expiry.
use crate::{asset_files as files, runtime::Core, storage};
use base64::Engine;
use chrono::Utc;
use image::ImageDecoder;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Cursor, Write},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
const MAX_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DECODE: u64 = 128 * 1024 * 1024;
const ROOT_KEY: &str = "managed_assets_directory";
static ACTIONS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub ttl_seconds: u64,
    pub max_bytes: u64,
    pub cleanup_enabled: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            ttl_seconds: 86400,
            max_bytes: 512 * 1024 * 1024,
            cleanup_enabled: true,
        }
    }
}
pub fn validate_config(value: &Value) -> Result<Config, String> {
    let config: Config = serde_json::from_value(value.clone()).map_err(|_| "ASSET_POLICY")?;
    if !(300..=30 * 86400).contains(&config.ttl_seconds)
        || !(64 * 1024 * 1024..=2 * 1024 * 1024 * 1024).contains(&config.max_bytes)
    {
        return Err("ASSET_POLICY_BOUNDS".into());
    }
    Ok(config)
}
pub fn config(c: &Core) -> Result<Config, String> {
    let value = c.store.setting("asset_lifecycle");
    if value.is_null() {
        Ok(Config::default())
    } else {
        validate_config(&value)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Root {
    version: u32,
    id: String,
    object_id: String,
    parent_id: String,
    lock_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Object {
    object_id: String,
    sha256: String,
    size: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    id: String,
    root_id: String,
    class: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    capture_module: Option<String>,
    extension: String,
    mime: String,
    label: String,
    original: Object,
    thumbnail: Option<Object>,
    width: u32,
    height: u32,
    created_at: String,
}
fn uuid(id: &str) -> Result<(), String> {
    if uuid::Uuid::parse_str(id)
        .map_err(|_| "ASSET_ID")?
        .to_string()
        != id
    {
        return Err("ASSET_ID".into());
    }
    Ok(())
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
struct Lease<'a> {
    core: &'a Core,
    id: String,
    cancel: CancellationToken,
    deadline: Instant,
    module: Option<String>,
    privacy: bool,
}
impl<'a> Lease<'a> {
    fn new(core: &'a Core, vision: bool, privacy: bool) -> Result<Self, String> {
        Self::for_module(core, vision.then_some("vision"), privacy)
    }
    fn for_module(core: &'a Core, module: Option<&str>, privacy: bool) -> Result<Self, String> {
        let id = format!("{}-{}", module.unwrap_or("shared"), storage::id());
        let cancel = CancellationToken::new();
        core.asset_runs
            .lock()
            .map_err(|_| "ASSET_RUN_LOCK")?
            .insert(id.clone(), cancel.clone());
        let lease = Self {
            core,
            id,
            cancel,
            deadline: Instant::now() + Duration::from_secs(85),
            module: module.map(str::to_owned),
            privacy,
        };
        lease.check()?;
        Ok(lease)
    }
    fn check(&self) -> Result<(), String> {
        if self.cancel.is_cancelled() {
            return Err("ASSET_CANCELLED".into());
        }
        if Instant::now() >= self.deadline {
            return Err("ASSET_DEADLINE".into());
        }
        if self.module.as_ref().is_some_and(|m| !self.core.enabled(m)) {
            return Err("TOOL_DISABLED_OR_UNKNOWN".into());
        }
        if self.privacy && self.core.store.setting("privacy") == true {
            return Err("PRIVACY_DENIED".into());
        }
        Ok(())
    }
}
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Ok(mut runs) = self.core.asset_runs.lock() {
            runs.remove(&self.id);
        }
    }
}
pub fn cancel(c: &Core) -> Result<(), String> {
    for token in c.asset_runs.lock().map_err(|_| "ASSET_RUN_LOCK")?.values() {
        token.cancel();
    }
    Ok(())
}
pub fn cancel_module(c: &Core, module: &str) -> Result<(), String> {
    for (id, token) in c.asset_runs.lock().map_err(|_| "ASSET_RUN_LOCK")?.iter() {
        if id.starts_with(&format!("{module}-")) || id.starts_with("shared-") {
            token.cancel();
        }
    }
    Ok(())
}
struct Profile {
    root: Root,
    path: PathBuf,
    _parents: Vec<File>,
    _lock: File,
}
impl Profile {
    fn get(c: &Core, initialize: bool) -> Result<Self, String> {
        let mut value = c.store.setting(ROOT_KEY);
        if value.is_null() {
            if !initialize {
                return Err("ASSET_ROOT_NOT_INITIALIZED".into());
            }
            let id = storage::id();
            let path = c.root.join("temp").join(format!("assets-{id}"));
            let _parents = files::parents(c, &path)?;
            let parent = files::directory(path.parent().ok_or("ASSET_PARENT")?)?;
            let parent_id = files::identity(&parent)?;
            files::create_directory(&path)?;
            let directory = files::directory(&path)?;
            files::private(&directory)?;
            let lock = files::create(&path.join("profile.lock"))?;
            let root = Root {
                version: 1,
                id,
                object_id: files::identity(&directory)?,
                parent_id,
                lock_id: files::identity(&lock)?,
            };
            value = serde_json::to_value(&root).map_err(|_| "ASSET_ROOT_JSON")?;
            if c.store.execute(
                "INSERT OR IGNORE INTO settings(key,json_value) VALUES(?,?)",
                &[json!(ROOT_KEY), json!(value.to_string())],
            )? != 1
            {
                return Err("ASSET_ROOT_INITIALIZATION_RACED".into());
            }
        }
        let root: Root = serde_json::from_value(value).map_err(|_| "ASSET_ROOT_JSON")?;
        uuid(&root.id)?;
        if root.version != 1 {
            return Err("ASSET_ROOT_VERSION".into());
        }
        let path = c.root.join("temp").join(format!("assets-{}", root.id));
        let parents = files::parents(c, &path.join("profile.lock"))?;
        let directory = files::directory(&path)?;
        let parent = files::directory(path.parent().ok_or("ASSET_PARENT")?)?;
        if files::identity(&directory)? != root.object_id
            || files::identity(&parent)? != root.parent_id
        {
            return Err("ASSET_ROOT_CHANGED".into());
        }
        files::private(&directory)?;
        let lock = files::lock(&path.join("profile.lock"))?;
        if files::identity(&lock)? != root.lock_id {
            return Err("ASSET_LOCK_CHANGED".into());
        }
        Ok(Self {
            root,
            path,
            _parents: parents,
            _lock: lock,
        })
    }
    fn check(&self, c: &Core) -> Result<(), String> {
        if c.store.setting(ROOT_KEY)
            != serde_json::to_value(&self.root).map_err(|_| "ASSET_ROOT_JSON")?
        {
            return Err("ASSET_ROOT_CHANGED".into());
        }
        Ok(())
    }
    fn original(&self, r: &Record) -> PathBuf {
        self.path.join(format!("{}.{}", r.id, r.extension))
    }
    fn thumb(&self, r: &Record) -> PathBuf {
        self.path.join(format!("{}.thumb.png", r.id))
    }
}
fn load(c: &Core, p: &Profile, id: &str) -> Result<(Record, Value), String> {
    uuid(id)?;
    let rows=c.store.query("SELECT asset_records.*, (SELECT COUNT(*) FROM attachments WHERE asset_id=asset_records.id) AS reference_count FROM asset_records WHERE id=?",&[json!(id)])?;
    let row = rows[0].clone();
    let r: Record = serde_json::from_str(row["record_json"].as_str().ok_or("ASSET_NOT_FOUND")?)
        .map_err(|_| "ASSET_RECORD_JSON")?;
    uuid(&r.id)?;
    uuid(&r.root_id)?;
    if r.version != 1
        || r.id != id
        || r.root_id != p.root.id
        || r.class != row["class"]
        || !["capture", "attachment"].contains(&r.class.as_str())
        || r.capture_module.as_ref().is_some_and(|m| {
            r.class != "capture"
                || ![
                    "vision",
                    "gui",
                    "browser",
                    "clipboard",
                    "homeassistant",
                    "images",
                    "documents",
                ]
                .contains(&m.as_str())
        })
        || ![
            "png", "jpg", "webp", "gif", "bmp", "tif", "data", "txt", "md", "json", "csv",
        ]
        .contains(&r.extension.as_str())
        || r.original.size > MAX_BYTES
        || r.original.sha256.len() != 64
        || r.created_at != row["created_at"]
    {
        return Err("ASSET_RECORD_CHANGED".into());
    }
    if r.thumbnail
        .as_ref()
        .is_some_and(|t| t.size > MAX_BYTES || t.sha256.len() != 64)
    {
        return Err("ASSET_RECORD_CHANGED".into());
    }
    Ok((r, row))
}
fn check_object(file: &File, object: &Object, lease: &Lease) -> Result<Vec<u8>, String> {
    lease.check()?;
    files::private(file)?;
    if files::identity(file)? != object.object_id {
        return Err("ASSET_OBJECT_CHANGED".into());
    }
    let bytes = files::read(file, object.size.min(MAX_BYTES))?;
    lease.check()?;
    if bytes.len() as u64 != object.size || digest(&bytes) != object.sha256 {
        return Err("ASSET_CONTENT_CHANGED".into());
    }
    Ok(bytes)
}
struct ImageInfo {
    mime: String,
    extension: String,
    width: u32,
    height: u32,
    thumbnail: Option<Vec<u8>>,
}
fn image_info(bytes: &[u8]) -> Result<ImageInfo, String> {
    let format = match image::guess_format(bytes) {
        Ok(format) => format,
        Err(_) => {
            return Ok(ImageInfo {
                mime: "application/octet-stream".into(),
                extension: "data".into(),
                width: 0,
                height: 0,
                thumbnail: None,
            })
        }
    };
    let (mime, extension) = match format {
        image::ImageFormat::Png => ("image/png", "png"),
        image::ImageFormat::Jpeg => ("image/jpeg", "jpg"),
        image::ImageFormat::WebP => ("image/webp", "webp"),
        image::ImageFormat::Gif => ("image/gif", "gif"),
        image::ImageFormat::Bmp => ("image/bmp", "bmp"),
        image::ImageFormat::Tiff => ("image/tiff", "tif"),
        _ => return Err("ASSET_IMAGE_FORMAT".into()),
    };
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(MAX_DECODE);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(|_| "ASSET_IMAGE_DECODE")?;
    let (width, height) = decoder.dimensions();
    if width == 0
        || height == 0
        || width > 16384
        || height > 16384
        || decoder.total_bytes() > MAX_DECODE
    {
        return Err("ASSET_IMAGE_PIXEL_LIMIT".into());
    }
    // Independently enforce output allocation: DynamicImage::from_decoder is not relied on to enforce max_alloc.
    let image = image::DynamicImage::from_decoder(decoder).map_err(|_| "ASSET_IMAGE_DECODE")?;
    let thumb = image::DynamicImage::ImageRgba8(
        image.thumbnail(width.min(640), height.min(640)).to_rgba8(),
    );
    let mut output = Cursor::new(Vec::new());
    thumb
        .write_to(&mut output, image::ImageFormat::Png)
        .map_err(|_| "ASSET_THUMBNAIL")?;
    Ok(ImageInfo {
        mime: mime.into(),
        extension: extension.into(),
        width,
        height,
        thumbnail: Some(output.into_inner()),
    })
}
fn content_info(bytes: &[u8], class: &str, label: &str) -> Result<ImageInfo, String> {
    let source = PathBuf::from(label);
    let suffix = source
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if class == "attachment"
        && ["txt", "md", "json", "csv"].contains(&suffix.as_str())
        && std::str::from_utf8(bytes).is_ok()
    {
        return Ok(ImageInfo {
            mime: "application/octet-stream".into(),
            extension: suffix,
            width: 0,
            height: 0,
            thumbnail: None,
        });
    }
    image_info(bytes)
}
fn capture_access(c: &Core, r: &Record) -> Result<(), String> {
    if r.class == "capture" {
        if c.store.setting("privacy") == true {
            return Err("PRIVACY_DENIED".into());
        }
        if !c.enabled(r.capture_module.as_deref().unwrap_or("vision")) {
            return Err("TOOL_DISABLED_OR_UNKNOWN".into());
        }
    }
    Ok(())
}
fn store(
    c: &Core,
    p: &Profile,
    bytes: &[u8],
    class: &str,
    label: &str,
    lease: &Lease,
) -> Result<Record, String> {
    lease.check()?;
    p.check(c)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("ASSET_BYTE_LIMIT".into());
    }
    let ImageInfo {
        mime,
        extension,
        width,
        height,
        thumbnail,
    } = content_info(bytes, class, label)?;
    lease.check()?;
    p.check(c)?;
    if class == "capture" && mime != "image/png" {
        return Err("ASSET_CAPTURE_FORMAT".into());
    }
    let quota = config(c)?;
    let quota_rows=c.store.query("SELECT COUNT(*) AS n,COALESCE(SUM(CAST(json_extract(record_json,'$.original.size') AS INTEGER)+COALESCE(CAST(json_extract(record_json,'$.thumbnail.size') AS INTEGER),0)),0) AS bytes FROM asset_records WHERE state!='expired'",&[])?;
    let needed = bytes.len() as u64 + thumbnail.as_ref().map_or(0, |b| b.len() as u64);
    if quota_rows[0]["n"].as_u64().unwrap_or(1000) >= 1000
        || quota_rows[0]["bytes"]
            .as_u64()
            .ok_or("ASSET_QUOTA")?
            .checked_add(needed)
            .is_none_or(|n| n > quota.max_bytes)
    {
        return Err("ASSET_QUOTA".into());
    }
    let id = storage::id();
    let original = files::create(&p.path.join(format!("{id}.{extension}")))?;
    let thumb = if thumbnail.is_some() {
        match files::create(&p.path.join(format!("{id}.thumb.png"))) {
            Ok(file) => Some(file),
            Err(error) => {
                let _ = files::erase(&original);
                return Err(error);
            }
        }
    } else {
        None
    };
    let record = Record {
        version: 1,
        id,
        root_id: p.root.id.clone(),
        class: class.into(),
        capture_module: if class == "capture" {
            lease.module.clone()
        } else {
            None
        },
        extension,
        mime,
        label: label
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect(),
        original: Object {
            object_id: files::identity(&original)?,
            sha256: digest(bytes),
            size: bytes.len() as u64,
        },
        thumbnail: thumb
            .as_ref()
            .zip(thumbnail.as_ref())
            .map(|(file, data)| {
                Ok::<Object, String>(Object {
                    object_id: files::identity(file)?,
                    sha256: digest(data),
                    size: data.len() as u64,
                })
            })
            .transpose()?,
        width,
        height,
        created_at: storage::now(),
    };
    let expires =
        Some((Utc::now() + chrono::Duration::seconds(quota.ttl_seconds as i64)).to_rfc3339());
    let inserted=c.store.execute("INSERT INTO asset_records(id,class,state,record_json,created_at,expires_at) VALUES(?,?,'creating',?,?,?)",&[json!(record.id),json!(class),json!(serde_json::to_string(&record).map_err(|_|"ASSET_RECORD_JSON")?),json!(record.created_at),json!(expires)]);
    if let Err(error) = inserted {
        let _ = files::erase(&original);
        if let Some(file) = &thumb {
            let _ = files::erase(file);
        }
        return Err(error);
    }
    let write = |file: &File, data: &[u8]| -> Result<(), String> {
        let mut writer = file;
        for chunk in data.chunks(65536) {
            lease.check()?;
            p.check(c)?;
            writer.write_all(chunk).map_err(|_| "ASSET_WRITE")?;
        }
        writer.sync_all().map_err(|_| "ASSET_FLUSH".into())
    };
    let result = (|| {
        write(&original, bytes)?;
        if let Some((file, data)) = thumb.as_ref().zip(thumbnail.as_ref()) {
            write(file, data)?;
        }
        check_object(&original, &record.original, lease)?;
        if let Some((file, object)) = thumb.as_ref().zip(record.thumbnail.as_ref()) {
            check_object(file, object, lease)?;
        }
        lease.check()?;
        p.check(c)?;
        if c.store.execute(
            "UPDATE asset_records SET state='ready' WHERE id=? AND state='creating' AND record_json=?",
            &[json!(record.id),json!(serde_json::to_string(&record).map_err(|_|"ASSET_RECORD_JSON")?)],
        )?!=1 { return Err("ASSET_CREATION_CLAIM_CHANGED".into()); }
        Ok::<(), String>(())
    })();
    if let Err(error) = result {
        let _ = c.store.execute(
            "UPDATE asset_records SET state='blocked',error=? WHERE id=?",
            &[json!(error), json!(record.id)],
        );
        return Err(format!("{error}; asset_id={}", record.id));
    }
    Ok(record)
}
fn public(p: &Profile, r: &Record, row: &Value) -> Value {
    json!({"id":r.id,"class":r.class,"capture_module":if r.class == "capture" {r.capture_module.as_deref().unwrap_or("vision")} else {""},"state":row["state"],"label":r.label,"path":p.original(r),"mime":r.mime,"size":r.original.size,"sha256":r.original.sha256,"width":r.width,"height":r.height,"thumbnail_path":r.thumbnail.as_ref().map(|_|p.thumb(r)),"created_at":r.created_at,"expires_at":row["expires_at"],"error":row["error"],"reference_count":row["reference_count"]})
}
fn preview(c: &Core, p: &Profile, r: &Record, row: &Value, lease: &Lease) -> Result<Value, String> {
    capture_access(c, r)?;
    if row["state"] != "ready" {
        return Err("ASSET_NOT_READY".into());
    }
    let original = files::open(&p.original(r), false)?;
    check_object(&original, &r.original, lease)?;
    let mut result = public(p, r, row);
    if let Some(object) = &r.thumbnail {
        if object.size > 2 * 1024 * 1024 {
            return Err("ASSET_THUMBNAIL_LIMIT".into());
        }
        let file = files::open(&p.thumb(r), false)?;
        let bytes = check_object(&file, object, lease)?;
        result["thumbnail_data_url"] = json!(format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ));
    }
    lease.check()?;
    p.check(c)?;
    Ok(result)
}
fn erase_record(c: &Core, p: &Profile, id: &str, lease: &Lease) -> Result<Value, String> {
    lease.check()?;
    p.check(c)?;
    let (record, row) = load(c, p, id)?;
    if row["state"] == "expired" {
        return Ok(json!({"id":id,"state":"expired","reconciled":true}));
    }
    if c.store.query(
        "SELECT COUNT(*) AS n FROM attachments WHERE asset_id=?",
        &[json!(id)],
    )?[0]["n"]
        != 0
    {
        return Err("ASSET_STILL_ATTACHED".into());
    }
    if !["ready", "deleting", "blocked"].contains(&row["state"].as_str().unwrap_or("")) {
        return Err("ASSET_CREATION_UNKNOWN".into());
    }
    let objects = std::iter::once((p.original(&record), &record.original))
        .chain(record.thumbnail.as_ref().map(|t| (p.thumb(&record), t)));
    let mut held = Vec::new();
    for (path, object) in objects {
        lease.check()?;
        if !path.try_exists().map_err(|_| "ASSET_METADATA")? {
            if row["state"] == "deleting" {
                continue;
            }
            return Err("ASSET_OBJECT_MISSING".into());
        }
        let file = files::open(&path, true)?;
        check_object(&file, object, lease)?;
        held.push(file);
    }
    // The entire immutable set is held before deleting any member. This claim survives restart.
    if c.store.execute("UPDATE asset_records SET state='deleting',error=NULL WHERE id=? AND record_json=? AND state=? AND NOT EXISTS(SELECT 1 FROM attachments WHERE asset_id=?)",&[json!(id),row["record_json"].clone(),row["state"].clone(),json!(id)])?!=1{return Err("ASSET_DELETE_CLAIM_CHANGED".into());}
    for file in held {
        lease.check()?;
        p.check(c)?;
        files::erase(&file)?;
        drop(file);
    }
    c.store.execute(
        "UPDATE asset_records SET state='expired',error=NULL WHERE id=? AND state='deleting'",
        &[json!(id)],
    )?;
    Ok(json!({"id":id,"state":"expired"}))
}
fn candidates(c: &Core, cutoff: &str) -> Result<Value, String> {
    let cursor = c.store.setting("asset_cleanup_cursor");
    let query="SELECT id,created_at FROM asset_records WHERE (state='deleting' OR (state='ready' AND expires_at IS NOT NULL AND expires_at<=?)) AND NOT EXISTS(SELECT 1 FROM attachments WHERE asset_id=asset_records.id) AND (created_at,id)>(?,?) ORDER BY created_at,id LIMIT 100";
    let mut rows = c.store.query(
        query,
        &[
            json!(cutoff),
            json!(cursor["created_at"].as_str().unwrap_or("")),
            json!(cursor["id"].as_str().unwrap_or("")),
        ],
    )?;
    if rows.as_array().ok_or("ASSET_ROWS")?.is_empty() && !cursor.is_null() {
        rows = c
            .store
            .query(query, &[json!(cutoff), json!(""), json!("")])?;
    }
    Ok(rows)
}
fn reviewed(
    c: &Core,
    p: Option<&Profile>,
    tool: &str,
    input: &Value,
    cutoff: &str,
) -> Result<Value, String> {
    let mut items = Vec::new();
    if let Some(p) = p {
        let ids = if tool == "vision.asset_delete" {
            json!([{"id":input["id"]}])
        } else {
            candidates(c, cutoff)?
        };
        for row in ids.as_array().ok_or("ASSET_ROWS")? {
            let (r, data) = load(c, p, row["id"].as_str().ok_or("ASSET_ID")?)?;
            items.push(json!({"asset":public(p,&r,&data),"record_json":data["record_json"]}));
        }
        p.check(c)?;
    } else if tool == "vision.asset_delete" {
        return Err("ASSET_ROOT_NOT_INITIALIZED".into());
    }
    Ok(
        json!({"tool":tool,"cutoff":cutoff,"policy":config(c)?,"root":c.store.setting(ROOT_KEY),"cursor":c.store.setting("asset_cleanup_cursor"),"assets":items,"limit":100}),
    )
}
pub async fn review(c: &Arc<Core>, tool: &str, input: &Value) -> Result<Value, String> {
    let lease = Lease::new(c, false, true)?;
    let _serial = ACTIONS.lock().await;
    lease.check()?;
    let p = if c.store.setting(ROOT_KEY).is_null() {
        None
    } else {
        Some(Profile::get(c, false)?)
    };
    reviewed(c, p.as_ref(), tool, input, &storage::now())
}
fn cleanup(c: &Core, p: &Profile, lease: &Lease, cutoff: &str) -> Result<Value, String> {
    let rows = candidates(c, cutoff)?;
    let mut expired = 0;
    let mut blocked = Vec::new();
    for row in rows.as_array().ok_or("ASSET_ROWS")? {
        lease.check()?;
        let id = row["id"].as_str().ok_or("ASSET_ID")?;
        match erase_record(c, p, id, lease) {
            Ok(_) => expired += 1,
            Err(error) => {
                c.store.execute("UPDATE asset_records SET state=CASE WHEN state='deleting' THEN state ELSE 'blocked' END,error=? WHERE id=?",&[json!(error),json!(id)])?;
                blocked.push(json!({"id":id,"error":error}));
            }
        }
        c.store.set_setting(
            "asset_cleanup_cursor",
            &json!({"id":id,"created_at":row["created_at"]}),
        )?;
    }
    Ok(
        json!({"expired":expired,"blocked":blocked,"limit":100,"examined":rows.as_array().ok_or("ASSET_ROWS")?.len(),"cursor":c.store.setting("asset_cleanup_cursor")}),
    )
}
pub fn startup(c: &Core) -> Result<Value, String> {
    if c.store.setting(ROOT_KEY).is_null() {
        return Ok(json!({"expired":0}));
    }
    if !config(c)?.cleanup_enabled {
        return Ok(json!({"skipped":"disabled"}));
    }
    let lease = Lease::new(c, false, false)?;
    let p = Profile::get(c, false)?;
    // Never silently adopt a partially created asset after an interrupted producer.
    c.store.execute("UPDATE asset_records SET state='blocked',error='ASSET_CREATION_INTERRUPTED' WHERE state='creating'",&[])?;
    cleanup(c, &p, &lease, &storage::now())
}
pub async fn capture(c: &Arc<Core>, tool: &str, input: &Value) -> Result<Value, String> {
    let module = match tool {
        "vision.capture_window" | "vision.capture_monitor" | "vision.capture_region" => "vision",
        "gui.screenshot_target" => "gui",
        "browser.screenshot" => "browser",
        "clipboard.image" => "clipboard",
        _ => return Err("ASSET_CAPTURE_TOOL".into()),
    };
    let lease = Lease::for_module(c, Some(module), true)?;
    let _serial = tokio::select! { _=lease.cancel.cancelled()=>return Err("ASSET_CANCELLED".into()), guard=ACTIONS.lock()=>guard };
    lease.check()?;
    let policy = config(c)?;
    if input
        .get("_asset_policy")
        .is_some_and(|v| *v != json!(policy))
    {
        return Err("ASSET_POLICY_CHANGED_REVIEW_REQUIRED".into());
    }
    let mut args = input.clone();
    args.as_object_mut()
        .ok_or("ASSET_INPUT")?
        .remove("_asset_policy");
    args["return_png"] = json!(true);
    let (captured, bytes) = if tool == "clipboard.image" {
        (
            json!({"method":"clipboard_dib","source_expected":args["expected"]}),
            crate::clipboard::image(c, &args, lease.cancel.clone()).await?,
        )
    } else {
        let captured = if tool == "browser.screenshot" {
            c.browser_cancellable(tool, &args, lease.cancel.clone())
                .await?
        } else {
            crate::process::native_capture(&c.native, tool, &args, lease.cancel.clone()).await?
        };
        let encoded = captured["png_base64"]
            .as_str()
            .ok_or("ASSET_CAPTURE_RESULT")?;
        if encoded.len() > MAX_BYTES as usize * 4 / 3 + 8 {
            return Err("ASSET_BYTE_LIMIT".into());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| "ASSET_CAPTURE_RESULT")?;
        (captured, bytes)
    };
    lease.check()?;
    let p = Profile::get(c, true)?;
    let record = store(c, &p, &bytes, "capture", "Captura solicitada", &lease)?;
    let (_, row) = load(c, &p, &record.id)?;
    let mut result = preview(c, &p, &record, &row, &lease)?;
    result["rect"] = captured["rect"].clone();
    result["coordinate_space"] = json!(if module == "clipboard" {
        "image_pixels"
    } else if module == "browser" {
        "css"
    } else {
        "physical"
    });
    result["method"] = captured["method"].clone();
    if module == "clipboard" {
        result["source_expected"] = captured["source_expected"].clone();
        result["rect"] = json!({"x":0,"y":0,"width":record.width,"height":record.height});
    }
    c.emit("alfredpet:assets", &json!({}));
    Ok(result)
}
/// Decode a bounded remote image into a private PNG capture. The origin actor
/// retains its own connection lease; this layer adds asset policy/identity/ACL,
/// quota, thumbnail, cancellation and reference/expiry handling.
pub(crate) async fn remote_image(
    c: &Arc<Core>,
    module: &str,
    bytes: &[u8],
    label: &str,
    expected_policy: &Value,
    check: impl Fn() -> Result<(), String>,
) -> Result<Value, String> {
    check()?;
    let lease = Lease::for_module(c, Some(module), true)?;
    let _serial = ACTIONS.lock().await;
    lease.check()?;
    check()?;
    if json!(config(c)?) != *expected_policy {
        return Err("ASSET_POLICY_CHANGED_REVIEW_REQUIRED".into());
    }
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("ASSET_BYTE_LIMIT".into());
    }
    let info = image_info(bytes)?;
    if info.width == 0 || info.height == 0 {
        return Err("ASSET_IMAGE_DECODE".into());
    }
    let format = image::guess_format(bytes).map_err(|_| "ASSET_IMAGE_DECODE")?;
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(MAX_DECODE);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(|_| "ASSET_IMAGE_DECODE")?;
    if decoder.total_bytes() > MAX_DECODE {
        return Err("ASSET_IMAGE_PIXEL_LIMIT".into());
    }
    let image = image::DynamicImage::from_decoder(decoder).map_err(|_| "ASSET_IMAGE_DECODE")?;
    let mut png = Cursor::new(Vec::new());
    image
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|_| "ASSET_IMAGE_DECODE")?;
    check()?;
    lease.check()?;
    let p = Profile::get(c, true)?;
    let record = store(c, &p, png.get_ref(), "capture", label, &lease)?;
    let (_, row) = load(c, &p, &record.id)?;
    let result = preview(c, &p, &record, &row, &lease)?;
    check()?;
    lease.check()?;
    c.emit("alfredpet:assets", &json!({}));
    Ok(result)
}
pub async fn dispatch(c: &Arc<Core>, tool: &str, input: &Value) -> Result<Value, String> {
    if tool.starts_with("vision.capture_") {
        return capture(c, tool, input).await;
    }
    dispatch_inner(c, tool, input, true).await
}
async fn dispatch_inner(
    c: &Arc<Core>,
    tool: &str,
    input: &Value,
    vision: bool,
) -> Result<Value, String> {
    let lease = Lease::new(c, vision, true)?;
    let _serial = ACTIONS.lock().await;
    lease.check()?;
    let cutoff = input["_asset_review"]["cutoff"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(storage::now);
    if c.store.setting(ROOT_KEY).is_null() && ["vision.assets", "vision.cleanup"].contains(&tool) {
        if let Some(expected) = input.get("_asset_review") {
            if reviewed(c, None, tool, input, &cutoff)? != *expected {
                return Err("ASSET_CHANGED_REVIEW_REQUIRED".into());
            }
        }
        return Ok(if tool == "vision.assets" {
            json!([])
        } else {
            json!({"expired":0,"blocked":[],"limit":100,"examined":0})
        });
    }
    let p = Profile::get(c, false)?;
    if let Some(expected) = input.get("_asset_review") {
        if reviewed(c, Some(&p), tool, input, &cutoff)? != *expected {
            return Err("ASSET_CHANGED_REVIEW_REQUIRED".into());
        }
    }
    match tool {
        "vision.assets" => {
            let rows=c.store.query("SELECT id FROM asset_records WHERE state!='expired' ORDER BY created_at DESC LIMIT 100",&[])?;
            let mut output = Vec::new();
            for row in rows.as_array().ok_or("ASSET_ROWS")? {
                let (r, data) = load(c, &p, row["id"].as_str().ok_or("ASSET_ID")?)?;
                output.push(public(&p, &r, &data));
            }
            Ok(json!(output))
        }
        "vision.asset_get" => {
            let (r, row) = load(c, &p, input["id"].as_str().ok_or("ASSET_ID")?)?;
            preview(c, &p, &r, &row, &lease)
        }
        "vision.asset_delete" => {
            erase_record(c, &p, input["id"].as_str().ok_or("ASSET_ID")?, &lease)
        }
        "vision.cleanup" => cleanup(c, &p, &lease, &cutoff),
        _ => Err("ASSET_TOOL".into()),
    }
}
pub async fn ui(c: &Arc<Core>, action: &str, input: &Value) -> Result<Value, String> {
    let fields = input.as_object().ok_or("ASSET_INPUT")?;
    if fields.len() != 1 || !input["id"].is_string() {
        return Err("ASSET_INPUT".into());
    }
    uuid(input["id"].as_str().ok_or("ASSET_ID")?)?;
    if action == "get" {
        return dispatch_inner(c, "vision.asset_get", input, false).await;
    }
    if action == "image" {
        let lease = Lease::new(c, false, true)?;
        let _serial = ACTIONS.lock().await;
        lease.check()?;
        let p = Profile::get(c, false)?;
        let (r, row) = load(c, &p, input["id"].as_str().ok_or("ASSET_ID")?)?;
        capture_access(c, &r)?;
        if row["state"] != "ready" {
            return Err("ASSET_NOT_READY".into());
        }
        if !["image/png", "image/jpeg", "image/gif", "image/webp"].contains(&r.mime.as_str()) {
            return Err("ASSET_NOT_IMAGE".into());
        }
        if r.original.size > MAX_BYTES {
            return Err("ASSET_BYTE_LIMIT".into());
        }
        let original = files::open(&p.original(&r), false)?;
        let bytes = check_object(&original, &r.original, &lease)?;
        lease.check()?;
        p.check(c)?;
        return Ok(
            json!({"id":r.id,"label":r.label,"width":r.width,"height":r.height,"data_url":format!("data:{};base64,{}",r.mime,base64::engine::general_purpose::STANDARD.encode(bytes))}),
        );
    }
    if action != "delete" {
        return Err("ASSET_TOOL".into());
    }
    let expected = review(c, "vision.asset_delete", input).await?;
    let started = Instant::now();
    if let Err(error) = c
        .permission("assets.delete", &expected, crate::policy::Risk::Mandatory)
        .await
    {
        c.store.audit(
            "assets.delete",
            "Mandatory",
            &expected,
            "denied",
            &json!({"error":error}),
            0,
        )?;
        c.emit("alfredpet:activity", &json!({}));
        return Err(error);
    }
    let result = dispatch_inner(
        c,
        "vision.asset_delete",
        &json!({"id":input["id"],"_asset_review":expected}),
        false,
    )
    .await;
    c.store.audit(
        "assets.delete",
        "Mandatory",
        &expected,
        if result.is_ok() {
            "succeeded"
        } else {
            "failed"
        },
        &result
            .clone()
            .unwrap_or_else(|error| json!({"error":error})),
        started.elapsed().as_millis() as u64,
    )?;
    c.emit("alfredpet:activity", &json!({}));
    result
}
/// Import bytes supplied by a user paste/drop/file picker. Never interprets a source path.
pub async fn import_attachment(c: &Arc<Core>, input: &Value) -> Result<Value, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Import {
        conversation_id: String,
        name: String,
        data_base64: String,
    }
    let data: Import = serde_json::from_value(input.clone()).map_err(|_| "ATTACHMENT_INPUT")?;
    if data.name.is_empty()
        || data.name.len() > 240
        || data
            .name
            .chars()
            .any(|ch| ch.is_control() || "/\\:".contains(ch))
    {
        return Err("ATTACHMENT_NAME".into());
    }
    uuid(&data.conversation_id)?;
    if data.data_base64.len() > MAX_BYTES as usize * 4 / 3 + 8 {
        return Err("ASSET_BYTE_LIMIT".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&data.data_base64)
        .map_err(|_| "ATTACHMENT_ENCODING")?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_BYTES {
        return Err("ASSET_BYTE_LIMIT".into());
    }
    let lease = Lease::new(c, false, true)?;
    let _serial = ACTIONS.lock().await;
    lease.check()?;
    if c.store.query(
        "SELECT COUNT(*) AS n FROM conversations WHERE id=?",
        &[json!(data.conversation_id)],
    )?[0]["n"]
        != 1
    {
        return Err("ATTACHMENT_CONVERSATION".into());
    }
    let p = Profile::get(c, true)?;
    let r = store(c, &p, &bytes, "attachment", &data.name, &lease)?;
    insert_attachment(c, &p, &r, &data.conversation_id, None, &lease)
}
pub async fn attach(c: &Arc<Core>, input: &Value) -> Result<Value, String> {
    attach_selected(c, input, None).await
}
pub async fn attach_from_drop(
    c: &Arc<Core>,
    input: &Value,
    attachment_id: &str,
) -> Result<Value, String> {
    uuid(attachment_id)?;
    attach_selected(c, input, Some(attachment_id)).await
}
async fn attach_selected(
    c: &Arc<Core>,
    input: &Value,
    attachment_id: Option<&str>,
) -> Result<Value, String> {
    let fields = input.as_object().ok_or("ATTACHMENT_INPUT")?;
    if fields
        .keys()
        .any(|k| !["conversation_id", "asset_id", "path"].contains(&k.as_str()))
        || input["asset_id"].is_string() == input["path"].is_string()
        || fields.contains_key("asset_id") && !input["asset_id"].is_string()
        || fields.contains_key("path") && !input["path"].is_string()
    {
        return Err("ATTACHMENT_INPUT".into());
    }
    let lease = Lease::new(c, false, attachment_id.is_some())?;
    let _serial = ACTIONS.lock().await;
    lease.check()?;
    let conversation = input["conversation_id"]
        .as_str()
        .ok_or("ATTACHMENT_CONVERSATION")?;
    uuid(conversation)?;
    if c.store.query(
        "SELECT COUNT(*) AS n FROM conversations WHERE id=?",
        &[json!(conversation)],
    )?[0]["n"]
        != 1
    {
        return Err("ATTACHMENT_CONVERSATION".into());
    }
    let p = Profile::get(c, true)?;
    if let Some(id) = attachment_id {
        let rows = c
            .store
            .query("SELECT * FROM attachments WHERE id=?", &[json!(id)])?;
        if let Some(row) = rows.as_array().and_then(|a| a.first()) {
            if row["conversation_id"] != conversation {
                return Err("DROP_ALREADY_ATTACHED_OTHER_CONVERSATION".into());
            }
            let (r, state) = load(
                c,
                &p,
                row["asset_id"].as_str().ok_or("DROP_ATTACHMENT_CHANGED")?,
            )?;
            if state["state"] != "ready"
                || row["path"] != json!(p.original(&r))
                || row["sha256"] != r.original.sha256
                || row["mime"] != r.mime
                || row["size"] != r.original.size
            {
                return Err("DROP_ATTACHMENT_CHANGED".into());
            }
            let file = files::open(&p.original(&r), false)?;
            check_object(&file, &r.original, &lease)?;
            return Ok(
                json!({"id":id,"asset_id":r.id,"path":p.original(&r),"mime":r.mime,"already_attached":true}),
            );
        }
    }
    let r = if let Some(id) = input["asset_id"].as_str() {
        let (r, row) = load(c, &p, id)?;
        if row["state"] != "ready" {
            return Err("ASSET_NOT_READY".into());
        }
        capture_access(c, &r)?;
        let file = files::open(&p.original(&r), false)?;
        check_object(&file, &r.original, &lease)?;
        r
    } else {
        let source = c.path(input["path"].as_str().ok_or("ATTACHMENT_PATH")?, false)?;
        let _parents = files::parents(c, &source)?;
        let file = files::open(&source, false)?;
        let bytes = files::read(&file, MAX_BYTES)?;
        lease.check()?;
        c.path(source.to_str().ok_or("ASSET_PATH_ENCODING")?, false)?;
        store(
            c,
            &p,
            &bytes,
            "attachment",
            source
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .as_ref(),
            &lease,
        )?
    };
    insert_attachment(c, &p, &r, conversation, attachment_id, &lease)
}
fn insert_attachment(
    c: &Core,
    p: &Profile,
    r: &Record,
    conversation: &str,
    attachment_id: Option<&str>,
    lease: &Lease,
) -> Result<Value, String> {
    lease.check()?;
    capture_access(c, r)?;
    p.check(c)?;
    let id = attachment_id.map(str::to_owned).unwrap_or_else(storage::id);
    c.store.execute("INSERT INTO attachments(id,conversation_id,path,mime,sha256,size,created_at,asset_id,message_id,ephemeral,metadata_json) VALUES(?,?,?,?,?,?,?,?,NULL,0,?)",&[json!(id),json!(conversation),json!(p.original(r)),json!(r.mime),json!(r.original.sha256),json!(r.original.size),json!(storage::now()),json!(r.id),json!(json!({"label":r.label,"width":r.width,"height":r.height}).to_string())])?;
    c.emit("alfredpet:assets", &json!({}));
    Ok(
        json!({"id":id,"asset_id":r.id,"path":p.original(r),"mime":r.mime,"thumbnail_path":r.thumbnail.as_ref().map(|_|p.thumb(r))}),
    )
}
pub fn attachments(c: &Core, conversation: &str) -> Result<Value, String> {
    uuid(conversation)?;
    // Older builds did not store message_id. Recover their presentation link
    // without rewriting records during a read or assigning unsent files backwards.
    c.store.query("SELECT a.id,a.asset_id,a.path,a.mime,a.size,a.created_at,COALESCE(a.message_id,(SELECT m.id FROM messages m WHERE m.conversation_id=a.conversation_id AND m.role='user' AND m.created_at>=a.created_at ORDER BY m.created_at,m.rowid LIMIT 1)) AS message_id,a.ephemeral,a.metadata_json FROM attachments a WHERE a.conversation_id=? ORDER BY a.created_at,a.id",&[json!(conversation)])
}
pub fn attachment_review(c: &Core, input: &Value) -> Result<Value, String> {
    let fields = input.as_object().ok_or("ATTACHMENT_INPUT")?;
    if fields.len() != 2
        || fields
            .keys()
            .any(|k| !["conversation_id", "id"].contains(&k.as_str()))
    {
        return Err("ATTACHMENT_INPUT".into());
    }
    uuid(input["id"].as_str().ok_or("ATTACHMENT_ID")?)?;
    uuid(
        input["conversation_id"]
            .as_str()
            .ok_or("ATTACHMENT_CONVERSATION")?,
    )?;
    let row = c.store.query(
        "SELECT * FROM attachments WHERE id=? AND conversation_id=?",
        &[input["id"].clone(), input["conversation_id"].clone()],
    )?[0]
        .clone();
    if row.is_null() {
        return Err("ATTACHMENT_NOT_FOUND".into());
    }
    let managed = if let Some(id) = row["asset_id"].as_str() {
        let p = Profile::get(c, false)?;
        let (r, data) = load(c, &p, id)?;
        json!({"root":c.store.setting(ROOT_KEY),"record":data,"asset":public(&p,&r,&data)})
    } else {
        Value::Null
    };
    Ok(json!({"attachment":row,"managed_asset":managed}))
}
pub async fn remove_attachment(
    c: &Arc<Core>,
    input: &Value,
    expected: &Value,
) -> Result<Value, String> {
    let lease = Lease::new(c, false, false)?;
    let _serial = ACTIONS.lock().await;
    lease.check()?;
    if attachment_review(c, input)? != *expected {
        return Err("ATTACHMENT_CHANGED_REVIEW_REQUIRED".into());
    }
    let reviewed_row = &expected["attachment"];
    let asset = reviewed_row["asset_id"].as_str();
    let p = if asset.is_some() {
        Some(Profile::get(c, false)?)
    } else {
        None
    };
    if let Some((id, p)) = asset.zip(p.as_ref()) {
        let (r, data) = load(c, p, id)?;
        if json!({"root":c.store.setting(ROOT_KEY),"record":data,"asset":public(p,&r,&data)})
            != expected["managed_asset"]
        {
            return Err("ATTACHMENT_CHANGED_REVIEW_REQUIRED".into());
        }
    }
    if c.store.execute(
        "DELETE FROM attachments WHERE id=? AND conversation_id=? AND path=? AND mime=? AND sha256=? AND size=? AND created_at=? AND asset_id IS ? AND message_id IS ? AND ephemeral=? AND metadata_json=?",
        &[input["id"].clone(), input["conversation_id"].clone(),reviewed_row["path"].clone(),reviewed_row["mime"].clone(),reviewed_row["sha256"].clone(),reviewed_row["size"].clone(),reviewed_row["created_at"].clone(),reviewed_row["asset_id"].clone(),reviewed_row["message_id"].clone(),reviewed_row["ephemeral"].clone(),reviewed_row["metadata_json"].clone()],
    )? != 1
    {
        return Err("ATTACHMENT_CHANGED_REVIEW_REQUIRED".into());
    }
    let mut result = json!({"removed":input["id"],"legacy_file_retained":asset.is_none()});
    if let Some((id, p)) = asset.zip(p.as_ref()) {
        if c.store.query(
            "SELECT COUNT(*) AS n FROM attachments WHERE asset_id=?",
            &[json!(id)],
        )?[0]["n"]
            == 0
        {
            match erase_record(c, p, id, &lease) {
                Ok(value) => result["asset_cleanup"] = value,
                Err(error) => {
                    c.store.execute(
                        "UPDATE asset_records SET state=CASE WHEN state='deleting' THEN state ELSE 'blocked' END,error=? WHERE id=?",
                        &[json!(error), json!(id)],
                    )?;
                    result["asset_retained_reason"] = json!(error);
                }
            }
        }
    }
    c.emit("alfredpet:assets", &json!({}));
    Ok(result)
}
pub struct ImageBatch<'a> {
    images: Vec<(String, Vec<u8>)>,
    lease: Option<Lease<'a>>,
    profile: Option<Profile>,
    _files: Vec<File>,
    paths: Vec<PathBuf>,
    capture_modules: Vec<String>,
    records: Vec<(String, Value)>,
    conversation: String,
    rows: Value,
}
impl std::fmt::Debug for ImageBatch<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageBatch")
            .field("count", &self.images.len())
            .finish()
    }
}
impl std::ops::Deref for ImageBatch<'_> {
    type Target = Vec<(String, Vec<u8>)>;
    fn deref(&self) -> &Self::Target {
        &self.images
    }
}
impl ImageBatch<'_> {
    pub fn check(&self) -> Result<(), String> {
        if let Some(lease) = &self.lease {
            lease.check()?;
            if self.capture_modules.iter().any(|m| !lease.core.enabled(m)) {
                return Err("TOOL_DISABLED_OR_UNKNOWN".into());
            }
            if let Some(profile) = &self.profile {
                profile.check(lease.core)?;
                for (id, expected) in &self.records {
                    if load(lease.core, profile, id)?.1 != *expected {
                        return Err("ASSET_RECORD_CHANGED".into());
                    }
                }
            }
            if lease.core.store.query(
                "SELECT * FROM attachments WHERE conversation_id=? ORDER BY created_at,id",
                &[json!(self.conversation)],
            )? != self.rows
            {
                return Err("ATTACHMENT_RECORD_CHANGED".into());
            }
            for path in &self.paths {
                lease
                    .core
                    .path(path.to_str().ok_or("ASSET_PATH_ENCODING")?, false)?;
            }
        }
        Ok(())
    }
    pub fn cancellation(&self) -> CancellationToken {
        self.lease
            .as_ref()
            .map(|l| l.cancel.clone())
            .unwrap_or_default()
    }
}
/// The outgoing batch pins the asset profile until stdin is written. Keep the
/// same action lock used by previews for that interval, not just acquisition.
pub async fn verified_images_for_turn<'a>(
    c: &'a Core,
    conversation: &str,
) -> Result<(tokio::sync::MutexGuard<'static, ()>, ImageBatch<'a>), String> {
    let action = ACTIONS.lock().await;
    let images = verified_images(c, conversation)?;
    Ok((action, images))
}
pub fn verified_images<'a>(c: &'a Core, conversation: &str) -> Result<ImageBatch<'a>, String> {
    if c.store.setting("privacy") == true {
        return Ok(ImageBatch {
            images: Vec::new(),
            lease: None,
            profile: None,
            _files: Vec::new(),
            paths: Vec::new(),
            capture_modules: Vec::new(),
            records: Vec::new(),
            conversation: conversation.into(),
            rows: json!([]),
        });
    }
    let lease = Lease::new(c, false, true)?;
    let rows = c.store.query(
        "SELECT * FROM attachments WHERE conversation_id=? ORDER BY created_at,id",
        &[json!(conversation)],
    )?;
    let mut images = Vec::new();
    let mut total = 0u64;
    let mut files_held = Vec::new();
    let mut paths = Vec::new();
    let mut capture_modules = Vec::new();
    let mut records = Vec::new();
    let managed = rows
        .as_array()
        .ok_or("ATTACHMENT_ROWS")?
        .iter()
        .any(|row| row["asset_id"].is_string());
    let p = if managed {
        Some(Profile::get(c, false)?)
    } else {
        None
    };
    for row in rows.as_array().ok_or("ATTACHMENT_ROWS")? {
        lease.check()?;
        let mime = row["mime"].as_str().ok_or("ATTACHMENT_MIME")?;
        if !["image/png", "image/jpeg", "image/gif", "image/webp"].contains(&mime) {
            continue;
        }
        if images.len() >= 5 {
            return Err("ATTACHMENT_IMAGE_LIMIT: max five images and 6MB combined".into());
        }
        let data = if let Some(id) = row["asset_id"].as_str() {
            let p = p.as_ref().ok_or("ASSET_ROOT")?;
            let (r, state) = load(c, p, id)?;
            records.push((id.to_owned(), state.clone()));
            capture_access(c, &r)?;
            if r.class == "capture" {
                capture_modules.push(r.capture_module.as_deref().unwrap_or("vision").to_owned());
            }
            if state["state"] != "ready"
                || row["path"] != json!(p.original(&r))
                || row["mime"] != r.mime
                || row["sha256"] != r.original.sha256
                || row["size"] != r.original.size
            {
                return Err("ATTACHMENT_RECORD_CHANGED".into());
            }
            if r.original.size > 6 * 1024 * 1024 - total {
                return Err("ATTACHMENT_IMAGE_LIMIT: max five images and 6MB combined".into());
            }
            let file = files::open(&p.original(&r), false)?;
            let data = check_object(&file, &r.original, &lease)?;
            paths.push(p.original(&r));
            files_held.push(file);
            data
        } else {
            let path = c.path(row["path"].as_str().ok_or("ATTACHMENT_PATH")?, false)?;
            files_held.extend(files::parents(c, &path)?);
            let file = files::open(&path, false)?;
            let data = files::read(&file, 6 * 1024 * 1024 - total)?;
            lease.check()?;
            c.path(path.to_str().ok_or("ASSET_PATH_ENCODING")?, false)?;
            if row["size"].as_u64() != Some(data.len() as u64)
                || row["sha256"].as_str() != Some(digest(&data).as_str())
            {
                return Err("ATTACHMENT_CONTENT_CHANGED".into());
            }
            files_held.push(file);
            paths.push(path);
            data
        };
        total = total
            .checked_add(data.len() as u64)
            .ok_or("ATTACHMENT_IMAGE_LIMIT")?;
        if total > 6 * 1024 * 1024 {
            return Err("ATTACHMENT_IMAGE_LIMIT: max five images and 6MB combined".into());
        }
        images.push((mime.into(), data));
    }
    lease.check()?;
    let batch = ImageBatch {
        images,
        lease: Some(lease),
        profile: p,
        _files: files_held,
        paths,
        capture_modules,
        records,
        conversation: conversation.into(),
        rows,
    };
    batch.check()?;
    Ok(batch)
}
/// Multimodal MCP output for assets produced by an already-authorized tool.
/// Pixels are added after Core auditing; they are never written into activity logs.
pub(crate) fn model_images(c: &Core, ids: &[String]) -> Result<Vec<Value>, String> {
    if ids.len() > 5 {
        return Err("ATTACHMENT_IMAGE_LIMIT".into());
    }
    let lease = Lease::new(c, false, true)?;
    let p = Profile::get(c, false)?;
    let mut content = Vec::new();
    let mut records = Vec::new();
    let mut held = Vec::new();
    let mut total = 0u64;
    for id in ids {
        lease.check()?;
        let (r, row) = load(c, &p, id)?;
        capture_access(c, &r)?;
        if row["state"] != "ready" {
            return Err("ASSET_NOT_READY".into());
        }
        if !["image/png", "image/jpeg", "image/webp", "image/gif"].contains(&r.mime.as_str()) {
            return Err("ASSET_IMAGE_FORMAT".into());
        }
        total = total
            .checked_add(r.original.size)
            .ok_or("ATTACHMENT_IMAGE_LIMIT")?;
        if total > 6 * 1024 * 1024 {
            return Err("ATTACHMENT_IMAGE_LIMIT".into());
        }
        let file = files::open(&p.original(&r), false)?;
        let bytes = check_object(&file, &r.original, &lease)?;
        content.push(json!({"type":"image","mimeType":r.mime,"data":base64::engine::general_purpose::STANDARD.encode(bytes)}));
        held.push(file);
        records.push((r, row));
    }
    for (record, expected) in records {
        capture_access(c, &record)?;
        if load(c, &p, &record.id)?.1 != expected {
            return Err("ASSET_RECORD_CHANGED".into());
        }
    }
    p.check(c)?;
    lease.check()?;
    Ok(content)
}

#[cfg(all(test, windows))]
#[path = "assets_tests.rs"]
mod tests;

/// Called only on a verified offline restore in a fresh private staging tree.
pub(crate) fn rebind_restored(
    store: &storage::Store,
    root: &std::path::Path,
) -> Result<(), String> {
    let value = store.setting(ROOT_KEY);
    if value.is_null() {
        return Ok(());
    }
    let mut managed: Root = serde_json::from_value(value).map_err(|_| "ASSET_ROOT_JSON")?;
    uuid(&managed.id)?;
    let path = root.join("temp").join(format!("assets-{}", managed.id));
    managed.object_id = files::identity(&files::directory(&path)?)?;
    managed.parent_id = files::identity(&files::directory(&root.join("temp"))?)?;
    managed.lock_id = files::identity(&files::open(&path.join("profile.lock"), false)?)?;
    files::private(&files::directory(&path)?)?;
    for row in store
        .query(
            "SELECT id,record_json,state FROM asset_records WHERE state!='expired'",
            &[],
        )?
        .as_array()
        .ok_or("ASSET_ROWS")?
    {
        let mut record: Record =
            serde_json::from_str(row["record_json"].as_str().ok_or("ASSET_RECORD")?)
                .map_err(|_| "ASSET_RECORD_JSON")?;
        uuid(&record.id)?;
        if record.root_id != managed.id
            || record.id != row["id"]
            || ![
                "png", "jpg", "webp", "gif", "bmp", "tif", "data", "txt", "md", "json", "csv",
            ]
            .contains(&record.extension.as_str())
        {
            return Err("ASSET_RECORD_CHANGED".into());
        }
        // Partial creating/deleting records cannot be represented as a complete copy.
        if row["state"] != "ready" {
            return Err("ASSET_RESTORE_INCOMPLETE_RECORD".into());
        }
        let original = path.join(format!("{}.{}", record.id, record.extension));
        for (file_path, object) in std::iter::once((original, &mut record.original)).chain(
            record
                .thumbnail
                .as_mut()
                .map(|o| (path.join(format!("{}.thumb.png", record.id)), o)),
        ) {
            let file = files::open(&file_path, false)?;
            files::private(&file)?;
            let bytes = files::read(&file, MAX_BYTES)?;
            if bytes.len() as u64 != object.size || digest(&bytes) != object.sha256 {
                return Err("ASSET_CONTENT_CHANGED".into());
            }
            object.object_id = files::identity(&file)?;
        }
        store.execute(
            "UPDATE asset_records SET record_json=? WHERE id=?",
            &[
                json!(serde_json::to_string(&record).map_err(|_| "ASSET_RECORD_JSON")?),
                row["id"].clone(),
            ],
        )?;
    }
    store.set_setting(ROOT_KEY, &json!(managed))?;
    Ok(())
}
