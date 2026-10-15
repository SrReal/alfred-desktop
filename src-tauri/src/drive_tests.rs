use super::*;
use axum::{
    body::Body,
    extract::{Request, State},
    http::Response,
    Router,
};
use std::{collections::HashMap, path::PathBuf, sync::Mutex};
fn raw_file(id: &str, name: &str, mime: &str, parents: &[&str], bytes: Option<&[u8]>) -> Value {
    let mut f = json!({"id":id,"name":name,"mimeType":mime,"parents":parents,"trashed":false,"version":"1","modifiedTime":"2026-10-04T00:00:00Z","capabilities":{"canDownload":true,"canEdit":true,"canRename":true,"canTrash":true,"canAddChildren":true,"canAddFolderFromAnotherDrive":true,"canMoveItemWithinDrive":true,"canMoveItemOutOfDrive":true}});
    if let Some(b) = bytes {
        f["size"] = json!(b.len().to_string());
        f["sha256Checksum"] = json!(format!("{:x}", Sha256::digest(b)));
    }
    f
}
struct Mock {
    base: String,
    files: HashMap<String, Value>,
    download: Vec<u8>,
    seen: Vec<Value>,
    upload: Vec<u8>,
    total: usize,
    upload_meta: Value,
    partial: bool,
    fail_chunk: bool,
    foreign_location: bool,
    expire: bool,
    bad_range: bool,
    fail_uploaded_observation: bool,
    wrong_move_drive: bool,
    delay: bool,
    notify: Arc<tokio::sync::Notify>,
    list_extra: bool,
    next: String,
}
fn response(status: u16, value: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(value.to_string()))
        .unwrap()
}
fn progress(m: &Mock) -> Response<Body> {
    let mut r = Response::builder().status(308);
    if m.bad_range {
        r = r.header("range", "bytes=1-20");
    } else if !m.upload.is_empty() {
        r = r.header("range", format!("bytes=0-{}", m.upload.len() - 1));
    }
    r.header("content-length", "0").body(Body::empty()).unwrap()
}
async fn handler(State(shared): State<Arc<Mutex<Mock>>>, request: Request) -> Response<Body> {
    let method = request.method().to_string();
    let path = request.uri().path().to_string();
    let query: HashMap<_, _> = reqwest::Url::parse(&format!("http://127.0.0.1{}", request.uri()))
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let headers = request.headers().clone();
    let bytes = axum::body::to_bytes(request.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap()
        .to_vec();
    let (delay, notify) = {
        let mut m = shared.lock().unwrap();
        m.seen.push(json!({"method":method,"path":path,"query":query,"range":headers.get("content-range").map(|v|v.to_str().unwrap()),"bytes":bytes.len()}));
        (m.delay, m.notify.clone())
    };
    notify.notify_one();
    if delay {
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let mut m = shared.lock().unwrap();
    if path == "/token" {
        return response(
            200,
            json!({"access_token":"owned_fictional_access","refresh_token":"owned_fictional_refresh_rotated","token_type":"Bearer","scope":drive_auth::DRIVE_SCOPE,"expires_in":3600}),
        );
    }
    assert_eq!(
        headers.get("authorization").unwrap(),
        "Bearer owned_fictional_access"
    );
    if path == "/upload/drive/v3/files" {
        if method == "POST" {
            if query.get("uploadType").is_some_and(|v| v == "multipart") {
                assert!(headers.get("content-range").is_none());
                let content_type = headers.get("content-type").unwrap().to_str().unwrap();
                let boundary = content_type
                    .strip_prefix("multipart/related; boundary=")
                    .unwrap();
                assert!(
                    uuid::Uuid::parse_str(boundary.strip_prefix("alfredpet-").unwrap()).is_ok()
                );
                assert_eq!(
                    headers
                        .get("content-length")
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .parse::<usize>()
                        .unwrap(),
                    bytes.len()
                );
                let raw = String::from_utf8(bytes).unwrap();
                let parts = raw.split(&format!("--{boundary}")).collect::<Vec<_>>();
                assert_eq!(parts.len(), 4);
                assert_eq!(parts[0], "");
                assert_eq!(parts[3], "--\r\n");
                let metadata = parts[1]
                    .strip_prefix("\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n")
                    .unwrap()
                    .strip_suffix("\r\n")
                    .unwrap();
                m.upload_meta = serde_json::from_str(metadata).unwrap();
                let mime = m.upload_meta["mimeType"].as_str().unwrap();
                assert_eq!(parts[2], format!("\r\nContent-Type: {mime}\r\n\r\n\r\n"));
                let file = raw_file(
                    "owned_uploaded",
                    m.upload_meta["name"].as_str().unwrap(),
                    mime,
                    &[m.upload_meta["parents"][0].as_str().unwrap()],
                    Some(b""),
                );
                m.files.insert("owned_uploaded".into(), file);
                if m.fail_chunk {
                    m.fail_chunk = false;
                    return response(503, json!({"private":"owned_private_empty_create_error"}));
                }
                return response(200, json!({"id":"owned_uploaded"}));
            }
            assert_eq!(query.get("uploadType").unwrap(), "resumable");
            m.upload.clear();
            m.upload_meta = serde_json::from_slice(&bytes).unwrap();
            m.total = headers
                .get("x-upload-content-length")
                .unwrap()
                .to_str()
                .unwrap()
                .parse()
                .unwrap();
            let uri = if m.foreign_location {
                "http://127.0.0.1:1/upload/drive/v3/files?uploadType=resumable&upload_id=owned_private_session_key".into()
            } else {
                format!("{}/upload/drive/v3/files?uploadType=resumable&upload_id=owned_private_session_key",m.base)
            };
            return Response::builder()
                .status(200)
                .header("location", uri)
                .header("content-length", "0")
                .body(Body::empty())
                .unwrap();
        }
        if m.expire {
            return response(404, json!({"private":"owned_private_provider_error"}));
        }
        assert_eq!(method, "PUT");
        assert_eq!(query.get("upload_id").unwrap(), "owned_private_session_key");
        if bytes.is_empty() {
            assert_eq!(
                headers.get("content-range").unwrap(),
                &format!("bytes */{}", m.total)
            );
            if m.upload.len() == m.total {
                return response(200, json!({"id":"owned_uploaded"}));
            }
            return progress(&m);
        }
        let offset = m.upload.len();
        assert_eq!(
            headers.get("content-range").unwrap(),
            &format!("bytes {offset}-{}/{}", offset + bytes.len() - 1, m.total)
        );
        let n = if m.partial {
            bytes.len().min(256 * 1024)
        } else {
            bytes.len()
        };
        m.upload.extend_from_slice(&bytes[..n]);
        if m.upload.len() == m.total {
            let name = m.upload_meta["name"].as_str().unwrap().to_string();
            let mime = m.upload_meta["mimeType"].as_str().unwrap().to_string();
            let parent = m.upload_meta["parents"][0].as_str().unwrap().to_string();
            let f = raw_file("owned_uploaded", &name, &mime, &[&parent], Some(&m.upload));
            m.files.insert("owned_uploaded".into(), f);
        }
        if m.fail_chunk {
            m.fail_chunk = false;
            return response(503, json!({"private":"owned_private_provider_error"}));
        }
        if m.upload.len() == m.total {
            return response(201, json!({"id":"owned_uploaded"}));
        }
        return progress(&m);
    }
    if path == "/drive/v3/files" {
        if method == "POST" {
            let b: Value = serde_json::from_slice(&bytes).unwrap();
            let f = raw_file(
                "owned_created",
                b["name"].as_str().unwrap(),
                FOLDER,
                &[b["parents"][0].as_str().unwrap()],
                None,
            );
            m.files.insert("owned_created".into(), f.clone());
            return response(200, f);
        }
        let mut rows = vec![m.files["owned_file"].clone()];
        if m.list_extra {
            rows.push(raw_file(
                "foreign_file",
                "Foreign",
                "application/octet-stream",
                &["foreign_folder"],
                Some(b"foreign"),
            ));
        }
        let mut v = json!({"files":rows,"incompleteSearch":false});
        if !m.next.is_empty() {
            v["nextPageToken"] = json!(m.next);
        }
        return response(200, v);
    }
    if let Some(id) = path.strip_prefix("/drive/v3/files/") {
        if id == "owned_uploaded" && m.fail_uploaded_observation {
            m.fail_uploaded_observation = false;
            return response(
                503,
                json!({"private":"owned_private_completion_observation_error"}),
            );
        }
        if id.ends_with("/export") {
            return Response::builder()
                .status(200)
                .header("content-type", "application/pdf")
                .body(Body::from(m.download.clone()))
                .unwrap();
        }
        if query.get("alt").is_some_and(|v| v == "media") {
            return Response::builder()
                .status(200)
                .header("content-type", "application/octet-stream")
                .body(Body::from(m.download.clone()))
                .unwrap();
        }
        if method == "PATCH" {
            let b: Value = serde_json::from_slice(&bytes).unwrap();
            let destination_drive = query
                .get("addParents")
                .map(|parent| m.files[parent]["driveId"].clone());
            let wrong_move_drive = m.wrong_move_drive;
            let f = m.files.get_mut(id).unwrap();
            if let Some(name) = b.get("name") {
                f["name"] = name.clone();
            }
            if b["trashed"] == true {
                f["trashed"] = json!(true);
            }
            if let Some(parent) = query.get("addParents") {
                f["parents"] = json!([parent]);
                if !wrong_move_drive {
                    f["driveId"] = destination_drive.unwrap();
                }
            }
            f["version"] = json!("2");
            return response(200, f.clone());
        }
        return m
            .files
            .get(id)
            .map_or_else(|| response(404, json!({})), |f| response(200, f.clone()));
    }
    response(404, json!({}))
}
struct Fixture {
    core: Arc<Core>,
    id: String,
    root: PathBuf,
    identity: String,
    m: Arc<Mutex<Mock>>,
    server: tokio::task::JoinHandle<()>,
    native: bool,
}
impl Fixture {
    async fn new(read_only: bool, native: bool) -> Self {
        let download = "Señor 🐾 owned binary\0".as_bytes().to_vec();
        let mut files = HashMap::new();
        files.insert(
            "owned_file".into(),
            raw_file(
                "owned_file",
                "Señor 🐾.bin",
                "application/octet-stream",
                &["owned_folder"],
                Some(&download),
            ),
        );
        for f in ["owned_folder", "owned_dest"] {
            files.insert(f.into(), raw_file(f, f, FOLDER, &[], None));
        }
        let m = Arc::new(Mutex::new(Mock {
            base: String::new(),
            files,
            download,
            seen: vec![],
            upload: vec![],
            total: 0,
            upload_meta: Value::Null,
            partial: false,
            fail_chunk: false,
            foreign_location: false,
            expire: false,
            bad_range: false,
            fail_uploaded_observation: false,
            wrong_move_drive: false,
            delay: false,
            notify: Arc::new(tokio::sync::Notify::new()),
            list_extra: false,
            next: String::new(),
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        m.lock().unwrap().base = base.clone();
        let router = Router::new().fallback(handler).with_state(m.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let root = std::env::temp_dir().join(format!("ap-drive-files-{}", storage::id()));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join("workspace")).unwrap();
        let identity =
            crate::asset_files::identity(&crate::asset_files::directory(&root).unwrap()).unwrap();
        let core = Arc::new(
            Core::new(
                root.join("profile"),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        core.store
            .set_setting(
                "allowed_roots",
                &json!([root.join("workspace").canonicalize().unwrap()]),
            )
            .unwrap();
        core.store
            .execute(
                "UPDATE module_configs SET enabled=1 WHERE module_id='drive'",
                &[],
            )
            .unwrap();
        *core.drive_fixture.lock().unwrap() = Some(drive_auth::TestTransport {
            token_url: format!("{base}/token"),
            api_base: format!("{base}/"),
            vault: Arc::new(Mutex::new(crate::drive_credentials::TestVault::default())),
            timeout_ms: 5000,
            native_storage: native,
            headless_permission: true,
        });
        let config = json!({"auth":"installed_app_pkce","client_id":"owned-fixture.apps.googleusercontent.com","client_secret_mode":"none","allowed_files":["owned_file"],"allowed_folders":["owned_folder","owned_dest"],"read_only":read_only});
        let saved=crate::integrations::save(&core,&json!({"module_id":"drive","name":"Owned Drive file fixture","config":config,"secret":""})).await.unwrap();
        let id = saved["id"].as_str().unwrap().to_string();
        drive_auth::install_owned_grant(&core, &id, false).unwrap();
        Self {
            core,
            id,
            root,
            identity,
            m,
            server,
            native,
        }
    }
    fn input(&self, mut v: Value) -> Value {
        v["connection"] = json!(self.id);
        v
    }
    async fn prepare(&self, t: &str, v: Value) -> Prepared {
        prepare(&self.core, t, &self.input(v)).await.unwrap()
    }
    async fn meta(&self, id: &str) -> Value {
        execute(self.prepare("drive.metadata", json!({"file_id":id})).await)
            .await
            .unwrap()
    }
    async fn upload_start(&self, bytes: Vec<u8>) -> Value {
        let path = self.root.join("workspace/source.bin");
        std::fs::write(&path, &bytes).unwrap();
        let parent = self.meta("owned_folder").await;
        let p=self.prepare("drive.upload",json!({"action":"start","folder_id":"owned_folder","name":"Subida 🐾.bin","path":path,"expected":{"parent_fingerprint":parent["file_fingerprint"],"source_sha256":format!("{:x}",Sha256::digest(&bytes))}})).await;
        execute(p).await.unwrap()
    }
    async fn resume(&self, j: &Value) -> Result<Value> {
        let parent = self.meta("owned_folder").await;
        let v=self.input(json!({"action":"resume","job_id":j["id"],"expected":{"job_fingerprint":j["job_fingerprint"],"parent_fingerprint":parent["file_fingerprint"]}}));
        let p = prepare(&self.core, "drive.upload", &v).await?;
        execute(p).await
    }
    async fn status(&self, j: &Value) -> Value {
        execute(
            self.prepare("drive.upload", json!({"action":"status","job_id":j["id"]}))
                .await,
        )
        .await
        .unwrap()
    }
    async fn approve(&self, t: &str, v: Value, answer: &str) -> Result<Value> {
        let core = self.core.clone();
        let input = self.input(v);
        let tool = t.to_string();
        let task = tokio::spawn(async move { core.execute(&tool, input).await });
        let sender = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let key = { self.core.pending.lock().unwrap().keys().next().cloned() };
                if let Some(key) = key {
                    break self.core.pending.lock().unwrap().remove(&key).unwrap();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        sender.send(answer.into()).unwrap();
        task.await.unwrap()
    }
    fn clean(self) {
        let Self {
            core,
            root,
            identity,
            server,
            id,
            native,
            ..
        } = self;
        server.abort();
        if native {
            drive_auth::cleanup_owned_native_grant(&core, &id).unwrap();
        }
        drop(core);
        assert_eq!(
            root.canonicalize().unwrap().parent(),
            Some(std::env::temp_dir().canonicalize().unwrap().as_path())
        );
        assert!(root
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("ap-drive-files-"));
        assert_eq!(
            crate::asset_files::identity(&crate::asset_files::directory(&root).unwrap()).unwrap(),
            identity
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
#[tokio::test]
async fn nine_tools_schema_scope_readonly_and_privacy_deny_before_transport() {
    let f = Fixture::new(true, false).await;
    assert_eq!(
        f.core
            .tools()
            .iter()
            .filter(|t| t["id"].as_str().unwrap().starts_with("drive."))
            .count(),
        9
    );
    for (t, v) in [
        ("drive.metadata", json!({"file_id":"foreign_file"})),
        ("drive.list", json!({"folder_id":"root"})),
        (
            "drive.rename",
            json!({"file_id":"owned_file","name":"Changed","expected":{"file_fingerprint":"a".repeat(64)}}),
        ),
    ] {
        assert!(prepare(&f.core, t, &f.input(v)).await.is_err());
    }
    assert!(f.m.lock().unwrap().seen.is_empty());
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(
        f.core
            .execute("drive.metadata", f.input(json!({"file_id":"owned_file"})))
            .await
            .unwrap_err(),
        "PRIVACY_DENIED"
    );
    assert!(f.m.lock().unwrap().seen.is_empty());
    f.clean();
}
#[tokio::test]
async fn generated_folder_search_manual_cursor_filters_and_cycles() {
    let f = Fixture::new(true, false).await;
    {
        let mut m = f.m.lock().unwrap();
        m.list_extra = true;
        m.next = "opaque_owned_cursor".into();
    }
    let v = json!({"folder_id":"owned_folder","query":"a' \\ b","page_size":2});
    let result = execute(f.prepare("drive.search", v.clone()).await)
        .await
        .unwrap();
    assert_eq!(result["files"].as_array().unwrap().len(), 1);
    assert_eq!(result["filtered_out_of_scope"], 1);
    let query = f.m.lock().unwrap().seen.last().unwrap()["query"]["q"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(query.contains("'owned_folder' in parents"));
    assert!(query.contains("a\\' \\\\ b"));
    let mut next = v.clone();
    next["page_token"] = result["next_cursor"].clone();
    let err = execute(f.prepare("drive.search", next.clone()).await)
        .await
        .unwrap_err();
    assert_eq!(err, "DRIVE_CURSOR_CYCLE");
    next["folder_id"] = json!("owned_dest");
    assert_eq!(
        execute(f.prepare("drive.search", next).await)
            .await
            .unwrap_err(),
        "DRIVE_CURSOR_BINDING"
    );
    {
        let mut m = f.m.lock().unwrap();
        m.next.clear();
        m.files.get_mut("owned_file").unwrap()["mimeType"] = json!(SHORTCUT);
        m.files.get_mut("owned_file").unwrap()["shortcutDetails"] = json!({
            "targetId":"foreign_target", "targetMimeType":FOLDER,
            "targetResourceKey":"owned_fictional_private_resource_key"
        });
    }
    let listed = execute(
        f.prepare("drive.list", json!({"folder_id":"owned_folder"}))
            .await,
    )
    .await
    .unwrap();
    assert_eq!(listed["files"].as_array().unwrap().len(), 1);
    let shortcut = f.meta("owned_file").await;
    assert_eq!(
        shortcut["file"]["shortcutDetails"],
        json!({"targetId":"foreign_target","targetMimeType":FOLDER})
    );
    assert!(!listed.to_string().contains("private_resource_key"));
    assert!(!shortcut.to_string().contains("targetResourceKey"));
    let input = f.input(json!({"file_id":"owned_file","expected":{"file_fingerprint":shortcut["file_fingerprint"]}}));
    assert_eq!(
        prepare(&f.core, "drive.download", &input)
            .await
            .err()
            .unwrap(),
        "DRIVE_DOWNLOAD_DENIED"
    );
    let count = f.m.lock().unwrap().seen.len();
    assert!(prepare(
        &f.core,
        "drive.metadata",
        &f.input(json!({"file_id":"foreign_target"}))
    )
    .await
    .is_err());
    assert_eq!(f.m.lock().unwrap().seen.len(), count);
    assert!(!f
        .m
        .lock()
        .unwrap()
        .seen
        .iter()
        .any(|r| r["path"].as_str().unwrap().contains("foreign_target")));
    assert!(f
        .m
        .lock()
        .unwrap()
        .seen
        .iter()
        .filter_map(|r| r["query"]["fields"].as_str())
        .all(|fields| !fields.contains("targetResourceKey")));
    let mut malformed = f.m.lock().unwrap().files["owned_file"].clone();
    malformed["shortcutDetails"]["targetMimeType"] = json!("malformed");
    assert_eq!(
        normalize(&malformed).unwrap_err(),
        "DRIVE_SHORTCUT_METADATA"
    );
    f.clean();
}
#[tokio::test]
async fn fresh_observed_rename_create_move_and_mandatory_trash() {
    let f = Fixture::new(false, false).await;
    let meta = f.meta("owned_file").await;
    let renamed=f.approve("drive.rename",json!({"file_id":"owned_file","name":"Renombrado 🐾","expected":{"file_fingerprint":meta["file_fingerprint"]}}),"allow").await.unwrap();
    assert_eq!(renamed["file"]["name"], "Renombrado 🐾");
    let parent = f.meta("owned_dest").await;
    let created=f.approve("drive.create_folder",json!({"folder_id":"owned_dest","name":"Nueva","expected":{"parent_fingerprint":parent["file_fingerprint"]}}),"allow").await.unwrap();
    assert_eq!(created["created_folder_requires_explicit_scope"], true);
    let moved=f.approve("drive.move",json!({"file_id":"owned_file","folder_id":"owned_dest","expected":{"file_fingerprint":renamed["file_fingerprint"],"parent_fingerprint":parent["file_fingerprint"]}}),"allow").await.unwrap();
    assert_eq!(moved["file"]["parents"], json!(["owned_dest"]));
    assert_eq!(risk("drive.trash", &json!({})), policy::Risk::Mandatory);
    let trashed=f.approve("drive.trash",json!({"file_id":"owned_file","expected":{"file_fingerprint":moved["file_fingerprint"]}}),"allow").await.unwrap();
    assert_eq!(trashed["file"]["trashed"], true);
    let audit = f
        .core
        .store
        .query(
            "SELECT input_summary,output_summary FROM activity_events WHERE tool_id LIKE 'drive.%'",
            &[],
        )
        .unwrap()
        .to_string();
    assert!(!audit.contains("Renombrado"));
    assert!(!audit.contains("Señor"));
    assert!(!audit.contains("Nueva"));
    f.clean();
}
#[tokio::test]
async fn denied_or_changed_observation_never_dispatches_write() {
    let f = Fixture::new(false, false).await;
    let meta = f.meta("owned_file").await;
    let input = json!({"file_id":"owned_file","name":"Changed","expected":{"file_fingerprint":meta["file_fingerprint"]}});
    assert_eq!(
        f.approve("drive.rename", input.clone(), "deny")
            .await
            .unwrap_err(),
        "PERMISSION_DENIED_OR_TIMEOUT"
    );
    let p = f.prepare("drive.rename", input).await;
    f.m.lock().unwrap().files.get_mut("owned_file").unwrap()["version"] = json!("9");
    assert_eq!(
        execute(p).await.unwrap_err(),
        "DRIVE_STALE_FILE_OBSERVATION"
    );
    assert!(!f
        .m
        .lock()
        .unwrap()
        .seen
        .iter()
        .any(|r| r["method"] == "PATCH"));
    f.clean();
}
#[tokio::test]
async fn cross_drive_moves_use_relevant_capability_and_verify_destination() {
    let f = Fixture::new(false, false).await;
    {
        let mut m = f.m.lock().unwrap();
        m.files.get_mut("owned_file").unwrap()["driveId"] = json!("owned_drive_a");
        m.files.get_mut("owned_file").unwrap()["capabilities"]["canMoveItemWithinDrive"] =
            json!(false);
        m.files.get_mut("owned_dest").unwrap()["driveId"] = json!("owned_drive_b");
    }
    let file = f.meta("owned_file").await;
    let parent = f.meta("owned_dest").await;
    let input = json!({"file_id":"owned_file","folder_id":"owned_dest","expected":{"file_fingerprint":file["file_fingerprint"],"parent_fingerprint":parent["file_fingerprint"]}});
    let moved = f.approve("drive.move", input, "allow").await.unwrap();
    assert_eq!(moved["file"]["driveId"], "owned_drive_b");
    assert_eq!(moved["file"]["parents"], json!(["owned_dest"]));
    {
        let mut m = f.m.lock().unwrap();
        m.files.get_mut("owned_file").unwrap()["parents"] = json!(["owned_folder"]);
        m.files.get_mut("owned_file").unwrap()["capabilities"]["canMoveItemOutOfDrive"] =
            json!(false);
    }
    let file = f.meta("owned_file").await;
    let input = f.input(json!({"file_id":"owned_file","folder_id":"owned_dest","expected":{"file_fingerprint":file["file_fingerprint"],"parent_fingerprint":parent["file_fingerprint"]}}));
    assert_eq!(
        prepare(&f.core, "drive.move", &input).await.err().unwrap(),
        "DRIVE_FILE_NOT_WRITABLE"
    );
    {
        let mut m = f.m.lock().unwrap();
        m.files.get_mut("owned_file").unwrap()["driveId"] = json!("owned_drive_a");
        m.files.get_mut("owned_file").unwrap()["capabilities"]["canMoveItemWithinDrive"] =
            json!(true);
    }
    let file = f.meta("owned_file").await;
    let input = f.input(json!({"file_id":"owned_file","folder_id":"owned_dest","expected":{"file_fingerprint":file["file_fingerprint"],"parent_fingerprint":parent["file_fingerprint"]}}));
    assert_eq!(
        prepare(&f.core, "drive.move", &input).await.err().unwrap(),
        "DRIVE_FILE_NOT_WRITABLE"
    );
    f.m.lock().unwrap().files.get_mut("owned_file").unwrap()["capabilities"]
        ["canMoveItemOutOfDrive"] = json!(true);
    let file = f.meta("owned_file").await;
    let input = json!({"file_id":"owned_file","folder_id":"owned_dest","expected":{"file_fingerprint":file["file_fingerprint"],"parent_fingerprint":parent["file_fingerprint"]}});
    let prepared = f.prepare("drive.move", input.clone()).await;
    f.m.lock().unwrap().files.get_mut("owned_dest").unwrap()["version"] = json!("99");
    assert_eq!(
        execute(prepared).await.unwrap_err(),
        "DRIVE_STALE_PARENT_OBSERVATION"
    );
    let parent = f.meta("owned_dest").await;
    let input = json!({"file_id":"owned_file","folder_id":"owned_dest","expected":{"file_fingerprint":file["file_fingerprint"],"parent_fingerprint":parent["file_fingerprint"]}});
    f.m.lock().unwrap().wrong_move_drive = true;
    assert!(f
        .approve("drive.move", input, "allow")
        .await
        .unwrap_err()
        .contains("DRIVE_WRITE_RESPONSE_MISMATCH"));
    assert_eq!(
        f.m.lock()
            .unwrap()
            .seen
            .iter()
            .filter(|r| r["method"] == "PATCH")
            .count(),
        2
    );
    f.clean();
}
#[tokio::test]
async fn supported_folder_transfers_require_mandatory_approval_and_preserve_provider_limits() {
    let f = Fixture::new(false, false).await;
    f.m.lock().unwrap().files.get_mut("owned_dest").unwrap()["driveId"] = json!("owned_drive_b");
    let folder = f.meta("owned_folder").await;
    let parent = f.meta("owned_dest").await;
    let input = f.input(json!({"file_id":"owned_folder","folder_id":"owned_dest","expected":{"file_fingerprint":folder["file_fingerprint"],"parent_fingerprint":parent["file_fingerprint"]}}));
    assert_eq!(
        prepare(&f.core, "drive.move", &input).await.err().unwrap(),
        "DRIVE_MY_FOLDER_TO_SHARED_DRIVE_UNSUPPORTED"
    );
    f.m.lock().unwrap().files.get_mut("owned_folder").unwrap()["driveId"] = json!("owned_folder");
    let folder = f.meta("owned_folder").await;
    let input = f.input(json!({"file_id":"owned_folder","folder_id":"owned_dest","expected":{"file_fingerprint":folder["file_fingerprint"],"parent_fingerprint":parent["file_fingerprint"]}}));
    assert_eq!(
        prepare(&f.core, "drive.move", &input).await.err().unwrap(),
        "DRIVE_SHARED_DRIVE_ROOT_MOVE_DENIED"
    );
    {
        let mut m = f.m.lock().unwrap();
        m.files.get_mut("owned_folder").unwrap()["driveId"] = json!("owned_drive_a");
        m.files.get_mut("owned_folder").unwrap()["capabilities"]["canMoveItemWithinDrive"] =
            json!(false);
    }
    let folder = f.meta("owned_folder").await;
    f.m.lock().unwrap().files.get_mut("owned_dest").unwrap()["capabilities"]
        ["canAddFolderFromAnotherDrive"] = json!(false);
    let denied_parent = f.meta("owned_dest").await;
    let denied_input = f.input(json!({"file_id":"owned_folder","folder_id":"owned_dest","expected":{"file_fingerprint":folder["file_fingerprint"],"parent_fingerprint":denied_parent["file_fingerprint"]}}));
    assert_eq!(
        prepare(&f.core, "drive.move", &denied_input)
            .await
            .err()
            .unwrap(),
        "DRIVE_FILE_NOT_WRITABLE"
    );
    f.m.lock().unwrap().files.get_mut("owned_dest").unwrap()["capabilities"]
        ["canAddFolderFromAnotherDrive"] = json!(true);
    let input = json!({"file_id":"owned_folder","folder_id":"owned_dest","expected":{"file_fingerprint":folder["file_fingerprint"],"parent_fingerprint":parent["file_fingerprint"]}});
    assert_eq!(
        f.prepare("drive.move", input.clone()).await.risk,
        policy::Risk::Mandatory
    );
    assert_eq!(
        f.approve("drive.move", input.clone(), "deny")
            .await
            .unwrap_err(),
        "PERMISSION_DENIED_OR_TIMEOUT"
    );
    assert!(!f
        .m
        .lock()
        .unwrap()
        .seen
        .iter()
        .any(|r| r["method"] == "PATCH"));
    let moved = f.approve("drive.move", input, "allow").await.unwrap();
    assert_eq!(moved["file"]["driveId"], "owned_drive_b");
    let events = f
        .core
        .store
        .query(
            "SELECT risk FROM activity_events WHERE tool_id='drive.move'",
            &[],
        )
        .unwrap();
    assert!(events
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["risk"] == "Mandatory"));
    assert_eq!(events.as_array().unwrap().len(), 2);
    f.clean();
}
#[tokio::test]
async fn empty_multipart_upload_validates_source_permission_and_terminal_status() {
    let f = Fixture::new(false, false).await;
    let path = f.root.join("workspace/source.bin");
    std::fs::write(&path, b"").unwrap();
    let parent = f.meta("owned_folder").await;
    let input = json!({"action":"start","folder_id":"owned_folder","name":"Vacío 🐾.bin","path":path,"expected":{"parent_fingerprint":parent["file_fingerprint"],"source_sha256":format!("{:x}",Sha256::digest(b""))}});
    let mut wrong = input.clone();
    wrong["expected"]["source_sha256"] = json!("0".repeat(64));
    assert_eq!(
        f.core
            .execute("drive.upload", f.input(wrong))
            .await
            .unwrap_err(),
        "DRIVE_SOURCE_STALE_HASH"
    );
    assert_eq!(
        f.approve("drive.upload", input.clone(), "deny")
            .await
            .unwrap_err(),
        "PERMISSION_DENIED_OR_TIMEOUT"
    );
    assert!(!f
        .m
        .lock()
        .unwrap()
        .seen
        .iter()
        .any(|r| r["method"] == "POST"));
    let completed = f.approve("drive.upload", input, "allow").await.unwrap();
    assert_eq!(completed["state"], "completed");
    assert_eq!(
        completed["record"]["immutable"]["upload_protocol"],
        "empty_multipart_v1"
    );
    assert_eq!(completed["record"]["remote_file"]["size"], 0);
    assert_eq!(completed["record"]["acknowledged"], 0);
    assert_eq!(completed["record"]["cleanup_pending"], false);
    let grant = drive_auth::grant(&f.core, &f.id).await.unwrap();
    assert!(grant
        .capability_observe(completed["id"].as_str().unwrap())
        .unwrap()
        .is_none());
    drop(grant);
    let status = f.status(&completed).await;
    assert_eq!(status["state"], "completed");
    assert_eq!(
        f.m.lock()
            .unwrap()
            .seen
            .iter()
            .filter(|r| r["method"] == "POST" && r["path"] == "/upload/drive/v3/files")
            .count(),
        1
    );
    assert!(!f
        .m
        .lock()
        .unwrap()
        .seen
        .iter()
        .any(|r| r["method"] == "PUT"));
    let audit = f
        .core
        .store
        .query(
            "SELECT input_summary,output_summary FROM activity_events WHERE tool_id='drive.upload'",
            &[],
        )
        .unwrap()
        .to_string();
    assert!(!audit.contains("Vacío"));
    assert!(!audit.contains(path.to_str().unwrap()));
    f.clean();
}
#[tokio::test]
async fn empty_create_unknown_or_metadata_failure_never_replays_after_restart() {
    for returned_id in [true, false] {
        let f = Fixture::new(false, false).await;
        let path = f.root.join("workspace/source.bin");
        std::fs::write(&path, b"").unwrap();
        let parent = f.meta("owned_folder").await;
        {
            let mut m = f.m.lock().unwrap();
            m.fail_uploaded_observation = returned_id;
            m.fail_chunk = !returned_id;
        }
        let p=f.prepare("drive.upload",json!({"action":"start","folder_id":"owned_folder","name":"Empty.bin","path":path,"expected":{"parent_fingerprint":parent["file_fingerprint"],"source_sha256":format!("{:x}",Sha256::digest(b""))}})).await;
        assert!(execute(p)
            .await
            .unwrap_err()
            .starts_with("DRIVE_UPLOAD_INITIATION_UNCERTAIN"));
        let rows = f
            .core
            .store
            .query("SELECT id,state FROM drive_jobs WHERE kind='upload'", &[])
            .unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 1);
        assert_eq!(
            rows[0]["state"],
            if returned_id {
                "completion_observed"
            } else {
                "initiation_uncertain"
            }
        );
        let other = Arc::new(Core::new(f.core.root.clone(), f.core.resource.clone()).unwrap());
        *other.drive_fixture.lock().unwrap() = f.core.drive_fixture.lock().unwrap().clone();
        let status = other
            .execute(
                "drive.upload",
                json!({"connection":f.id,"action":"status","job_id":rows[0]["id"]}),
            )
            .await
            .unwrap();
        drop(other);
        if returned_id {
            assert_eq!(status["state"], "completed");
            assert_eq!(status["record"]["completion_file_id"], "owned_uploaded");
        } else {
            assert_eq!(status["state"], "initiation_uncertain");
            assert_eq!(status["manual_remote_inspection_required"], true);
            assert_eq!(
                f.resume(&status).await.unwrap_err(),
                "DRIVE_EMPTY_UPLOAD_NO_AUTOMATIC_CREATE_REPLAY"
            );
            let abandoned=f.approve("drive.upload",json!({"action":"abandon","job_id":status["id"],"expected":{"job_fingerprint":status["job_fingerprint"]}}),"allow").await.unwrap();
            assert_eq!(abandoned["state"], "abandoned");
            assert!(f.m.lock().unwrap().files.contains_key("owned_uploaded"));
        }
        assert_eq!(
            f.m.lock()
                .unwrap()
                .seen
                .iter()
                .filter(|r| r["method"] == "POST" && r["path"] == "/upload/drive/v3/files")
                .count(),
            1
        );
        assert!(!f
            .m
            .lock()
            .unwrap()
            .seen
            .iter()
            .any(|r| r["method"] == "PUT"));
        f.clean();
    }
}
#[tokio::test]
async fn nonempty_completion_id_reconciles_without_probe_or_media_replay() {
    let f = Fixture::new(false, false).await;
    let started = f.upload_start(b"Owned nonempty payload".to_vec()).await;
    f.m.lock().unwrap().fail_uploaded_observation = true;
    assert!(f.resume(&started).await.is_err());
    let row = local::job(&f.core, &f.id, started["id"].as_str().unwrap(), "upload").unwrap();
    assert_eq!(row["state"], "completion_observed");
    assert_eq!(row["record"]["completion_file_id"], "owned_uploaded");
    let before = f.m.lock().unwrap().seen.clone();
    let other = Arc::new(Core::new(f.core.root.clone(), f.core.resource.clone()).unwrap());
    *other.drive_fixture.lock().unwrap() = f.core.drive_fixture.lock().unwrap().clone();
    let status = other
        .execute(
            "drive.upload",
            json!({"connection":f.id,"action":"status","job_id":row["id"]}),
        )
        .await
        .unwrap();
    assert_eq!(status["state"], "completed");
    assert_eq!(status["record"]["cleanup_pending"], false);
    assert_eq!(
        status["record"]["remote_file"]["size"],
        b"Owned nonempty payload".len()
    );
    let after = f.m.lock().unwrap().seen.clone();
    assert_eq!(
        after
            .iter()
            .filter(|r| r["method"] == "POST" || r["method"] == "PUT")
            .count(),
        before
            .iter()
            .filter(|r| r["method"] == "POST" || r["method"] == "PUT")
            .count()
    );
    assert_eq!(
        after[before.len()..]
            .iter()
            .filter(|r| r["method"] == "GET" && r["path"] == "/drive/v3/files/owned_uploaded")
            .count(),
        1
    );
    let grant = drive_auth::grant(&other, &f.id).await.unwrap();
    assert!(grant
        .capability_observe(row["id"].as_str().unwrap())
        .unwrap()
        .is_none());
    drop(grant);
    drop(other);
    f.clean();
}
#[tokio::test]
async fn private_binary_download_hash_limit_and_exact_delete() {
    let f = Fixture::new(true, false).await;
    let meta = f.meta("owned_file").await;
    let downloaded=f.approve("drive.download",json!({"file_id":"owned_file","max_bytes":1024,"expected":{"file_fingerprint":meta["file_fingerprint"]}}),"allow").await.unwrap();
    let path = downloaded["record"]["local"]["path"].as_str().unwrap();
    assert_eq!(std::fs::read(path).unwrap(), f.m.lock().unwrap().download);
    let file = crate::asset_files::open(std::path::Path::new(path), false).unwrap();
    crate::asset_files::private(&file).unwrap();
    drop(file);
    let deleted=f.approve("drive.download",json!({"action":"delete","job_id":downloaded["id"],"expected":{"job_fingerprint":downloaded["job_fingerprint"]}}),"allow").await.unwrap();
    assert_eq!(deleted["state"], "deleted");
    assert!(!std::path::Path::new(path).exists());
    f.clean();
}
#[tokio::test]
async fn download_failure_removes_only_its_owned_partial_file() {
    let f = Fixture::new(true, false).await;
    let meta = f.meta("owned_file").await;
    f.m.lock().unwrap().download = vec![0; 4096];
    let p=f.prepare("drive.download",json!({"file_id":"owned_file","max_bytes":1024,"expected":{"file_fingerprint":meta["file_fingerprint"]}})).await;
    assert_eq!(execute(p).await.unwrap_err(), "DRIVE_DOWNLOAD_BYTE_LIMIT");
    let rows = f
        .core
        .store
        .query("SELECT id,state FROM drive_jobs WHERE kind='download'", &[])
        .unwrap();
    assert_eq!(rows[0]["state"], "failed");
    assert!(!f
        .core
        .root
        .join("temp")
        .join(format!(
            "drive-download-{}.bin",
            rows[0]["id"].as_str().unwrap()
        ))
        .exists());
    f.clean();
}
#[tokio::test]
async fn resumable_upload_probes_ranges_and_keeps_session_out_of_audit() {
    let f = Fixture::new(false, false).await;
    let bytes = vec![71; 2 * 1024 * 1024 + 17];
    let started = f.upload_start(bytes.clone()).await;
    assert_eq!(started["state"], "active");
    assert!(!started.to_string().contains("owned_private_session_key"));
    let completed = f.resume(&started).await.unwrap();
    assert_eq!(completed["state"], "completed");
    assert_eq!(completed["record"]["cleanup_pending"], false);
    assert_eq!(f.m.lock().unwrap().upload, bytes);
    let seen = f.m.lock().unwrap().seen.clone();
    let puts: Vec<_> = seen.iter().filter(|v| v["method"] == "PUT").collect();
    assert_eq!(puts.len(), 4);
    assert_eq!(puts[0]["bytes"], 0);
    assert_eq!(puts[3]["bytes"], 17);
    let job = completed["id"].as_str().unwrap();
    let grant = drive_auth::grant(&f.core, &f.id).await.unwrap();
    assert!(grant.capability_observe(job).unwrap().is_none());
    drop(grant);
    f.clean();
}
#[tokio::test]
async fn partial_acknowledgment_is_resumed_from_observed_range() {
    let f = Fixture::new(false, false).await;
    f.m.lock().unwrap().partial = true;
    let bytes = vec![53; 1024 * 1024 + 19];
    let started = f.upload_start(bytes.clone()).await;
    let step = f.resume(&started).await.unwrap();
    assert_eq!(step["state"], "active");
    assert_eq!(step["record"]["acknowledged"], 1024 * 1024);
    let completed = f.resume(&step).await.unwrap();
    assert_eq!(completed["state"], "completed");
    assert_eq!(f.m.lock().unwrap().upload, bytes);
    f.clean();
}
#[tokio::test]
async fn interrupted_chunk_recovers_after_core_restart_without_duplicate_create() {
    let f = Fixture::new(false, false).await;
    let started = f.upload_start(vec![99; 1024 * 1024 + 11]).await;
    f.m.lock().unwrap().fail_chunk = true;
    assert!(f
        .resume(&started)
        .await
        .unwrap_err()
        .starts_with("DRIVE_UPLOAD_CHUNK_HTTP_503"));
    let other = Arc::new(Core::new(f.core.root.clone(), f.core.resource.clone()).unwrap());
    *other.drive_fixture.lock().unwrap() = f.core.drive_fixture.lock().unwrap().clone();
    let status = other
        .execute(
            "drive.upload",
            json!({"connection":f.id,"action":"status","job_id":started["id"]}),
        )
        .await
        .unwrap();
    assert_eq!(status["record"]["acknowledged"], 1024 * 1024);
    drop(other);
    let completed = f.resume(&status).await.unwrap();
    assert_eq!(completed["state"], "completed");
    assert_eq!(
        f.m.lock()
            .unwrap()
            .seen
            .iter()
            .filter(|r| r["method"] == "POST" && r["path"] == "/upload/drive/v3/files")
            .count(),
        1
    );
    f.clean();
}
#[tokio::test]
async fn changed_source_revokes_resume_before_private_session_transport() {
    let f = Fixture::new(false, false).await;
    let started = f.upload_start(vec![23; 1024]).await;
    std::fs::write(f.root.join("workspace/source.bin"), vec![24; 1024]).unwrap();
    let before = f.m.lock().unwrap().seen.len();
    assert_eq!(
        f.resume(&started).await.unwrap_err(),
        "DRIVE_SOURCE_CHANGED"
    );
    assert_eq!(f.m.lock().unwrap().seen.len(), before + 2);
    f.clean();
}
#[tokio::test]
async fn foreign_session_and_bad_range_never_dispatch_new_data() {
    let f = Fixture::new(false, false).await;
    f.m.lock().unwrap().foreign_location = true;
    let bytes = vec![5; 1024];
    let path = f.root.join("workspace/source.bin");
    std::fs::write(&path, &bytes).unwrap();
    let parent = f.meta("owned_folder").await;
    let p=f.prepare("drive.upload",json!({"action":"start","folder_id":"owned_folder","name":"Owned","path":path,"expected":{"parent_fingerprint":parent["file_fingerprint"],"source_sha256":format!("{:x}",Sha256::digest(&bytes))}})).await;
    assert!(execute(p)
        .await
        .unwrap_err()
        .contains("DRIVE_UPLOAD_SESSION_ORIGIN"));
    assert!(!f
        .m
        .lock()
        .unwrap()
        .seen
        .iter()
        .any(|v| v["method"] == "PUT"));
    f.clean();
    let f = Fixture::new(false, false).await;
    let started = f.upload_start(vec![5; 1024]).await;
    f.m.lock().unwrap().bad_range = true;
    assert_eq!(f.resume(&started).await.unwrap_err(), "DRIVE_UPLOAD_RANGE");
    assert!(!f
        .m
        .lock()
        .unwrap()
        .seen
        .iter()
        .any(|v| v["method"] == "PUT" && v["bytes"].as_u64().unwrap() > 0));
    f.clean();
}
#[tokio::test]
async fn expiry_and_abandon_retire_observed_capability_without_remote_undo() {
    let f = Fixture::new(false, false).await;
    let started = f.upload_start(vec![1; 1024]).await;
    f.m.lock().unwrap().expire = true;
    let expired = f.status(&started).await;
    assert_eq!(expired["state"], "expired");
    assert_eq!(expired["record"]["cleanup_pending"], false);
    assert_eq!(
        f.m.lock()
            .unwrap()
            .seen
            .iter()
            .filter(|r| r["method"] == "POST" && r["path"] == "/upload/drive/v3/files")
            .count(),
        1
    );
    f.clean();
    let f = Fixture::new(false, false).await;
    let started = f.upload_start(vec![2; 1024]).await;
    let abandoned=f.approve("drive.upload",json!({"action":"abandon","job_id":started["id"],"expected":{"job_fingerprint":started["job_fingerprint"]}}),"allow").await.unwrap();
    assert_eq!(abandoned["state"], "abandoned");
    assert_eq!(abandoned["record"]["remote_result_possible"], true);
    assert_eq!(abandoned["record"]["cleanup_pending"], false);
    f.clean();
}
#[tokio::test]
async fn privacy_during_owned_request_closes_without_public_binary() {
    let f = Fixture::new(true, false).await;
    let meta = f.meta("owned_file").await;
    let p=f.prepare("drive.download",json!({"file_id":"owned_file","expected":{"file_fingerprint":meta["file_fingerprint"]}})).await;
    f.m.lock().unwrap().delay = true;
    f.m.lock().unwrap().notify = Arc::new(tokio::sync::Notify::new());
    let notify = f.m.lock().unwrap().notify.clone();
    let task = tokio::spawn(execute(p));
    notify.notified().await;
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert!(task.await.unwrap().unwrap_err().contains("PRIVACY_DENIED"));
    f.clean();
}
#[tokio::test]
async fn explicit_workspace_export_and_folder_move_are_observed() {
    let f = Fixture::new(false, false).await;
    f.m.lock().unwrap().files.insert(
        "owned_file".into(),
        raw_file(
            "owned_file",
            "Owned document",
            "application/vnd.google-apps.document",
            &["owned_folder"],
            None,
        ),
    );
    let meta = f.meta("owned_file").await;
    let exported=execute(f.prepare("drive.download",json!({"file_id":"owned_file","export_mime":"application/pdf","expected":{"file_fingerprint":meta["file_fingerprint"]}})).await).await.unwrap();
    assert_eq!(exported["state"], "ready");
    let source = f.meta("owned_folder").await;
    let parent = f.meta("owned_dest").await;
    let moved=f.approve("drive.move",json!({"file_id":"owned_folder","folder_id":"owned_dest","expected":{"file_fingerprint":source["file_fingerprint"],"parent_fingerprint":parent["file_fingerprint"]}}),"allow").await.unwrap();
    assert_eq!(moved["file"]["parents"], json!(["owned_dest"]));
    f.clean();
}
#[tokio::test]
async fn revoked_local_scope_allows_explicit_capability_abandonment_only() {
    let f = Fixture::new(false, false).await;
    let started = f.upload_start(vec![3; 1024]).await;
    let mut config = drive_auth::record(&f.core, &f.id).unwrap();
    config["allowed_folders"] = json!([]);
    config["read_only"] = json!(true);
    crate::integrations::save(&f.core,&json!({"id":f.id,"module_id":"drive","name":"Owned Drive file fixture","config":config,"secret":""})).await.unwrap();
    assert!(prepare(
        &f.core,
        "drive.upload",
        &f.input(json!({"action":"status","job_id":started["id"]}))
    )
    .await
    .is_err());
    let abandoned=f.approve("drive.upload",json!({"action":"abandon","job_id":started["id"],"expected":{"job_fingerprint":started["job_fingerprint"]}}),"allow").await.unwrap();
    assert_eq!(abandoned["state"], "abandoned");
    assert_eq!(abandoned["record"]["cleanup_pending"], false);
    f.clean();
}
#[tokio::test]
async fn renewed_token_preserves_upload_identity_but_new_consent_revokes_it() {
    let f = Fixture::new(false, false).await;
    let started = f.upload_start(vec![7; 1024]).await;
    drive_auth::expire_owned_grant(&f.core, &f.id).unwrap();
    let status = f.status(&started).await;
    assert_eq!(status["state"], "active");
    let old = picked_authorization(&f).await;
    assert!(old["phase"] == "ready");
    let parent = f.meta("owned_folder").await;
    let input=f.input(json!({"action":"resume","job_id":status["id"],"expected":{"job_fingerprint":status["job_fingerprint"],"parent_fingerprint":parent["file_fingerprint"]}}));
    let p = prepare(&f.core, "drive.upload", &input).await.unwrap();
    assert_eq!(
        execute(p).await.unwrap_err(),
        "DRIVE_UPLOAD_AUTHORIZATION_CHANGED"
    );
    assert!(!f
        .m
        .lock()
        .unwrap()
        .seen
        .iter()
        .any(|v| v["method"] == "PUT" && v["bytes"].as_u64().unwrap() > 0));
    f.clean();
}
async fn picked_authorization(f: &Fixture) -> Value {
    let attempt = drive_auth::start(&f.core, &json!({"connection":f.id}))
        .await
        .unwrap();
    let url = reqwest::Url::parse(attempt["fixture_auth_url"].as_str().unwrap()).unwrap();
    let fields: HashMap<_, _> = url.query_pairs().collect();
    let mut callback = reqwest::Url::parse(&fields["redirect_uri"]).unwrap();
    callback
        .query_pairs_mut()
        .append_pair("state", &fields["state"])
        .append_pair("code", "owned_fictional_code");
    assert_eq!(
        reqwest::get(callback).await.unwrap().status(),
        reqwest::StatusCode::OK
    );
    drive_auth::status(&f.core, &json!({"connection":f.id})).unwrap()
}
#[tokio::test]
async fn native_vault_actor_and_private_resumable_capability_roundtrip() {
    let f = Fixture::new(false, true).await;
    let attempt = drive_auth::start(&f.core, &json!({"connection":f.id,"pick_resources":true}))
        .await
        .unwrap();
    let url = reqwest::Url::parse(attempt["fixture_auth_url"].as_str().unwrap()).unwrap();
    let fields: HashMap<_, _> = url.query_pairs().collect();
    assert_eq!(fields.get("trigger_onepick").unwrap(), "true");
    let mut callback = reqwest::Url::parse(&fields["redirect_uri"]).unwrap();
    callback
        .query_pairs_mut()
        .append_pair("state", &fields["state"])
        .append_pair("code", "owned_fictional_code")
        .append_pair("picked_file_ids", "owned_file,owned_folder")
        .append_pair("scope", drive_auth::DRIVE_SCOPE);
    assert_eq!(
        reqwest::get(callback).await.unwrap().status(),
        reqwest::StatusCode::OK
    );
    let status = drive_auth::status(&f.core, &json!({"connection":f.id})).unwrap();
    assert_eq!(
        status["picked_file_ids"],
        json!(["owned_file", "owned_folder"])
    );
    assert_eq!(status["picked_ids_require_explicit_local_scope"], true);
    let started = f.upload_start(vec![42; 5 * 1024 * 1024 + 3]).await;
    let step = f.resume(&started).await.unwrap();
    assert_eq!(step["state"], "active");
    let nonce = storage::id();
    f.core
        .store
        .set_setting("drive.owned_process_fixture", &json!(nonce))
        .unwrap();
    drive_auth::expire_owned_grant(&f.core, &f.id).unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--ignored",
            "--exact",
            "drive::tests::owned_native_refresh_and_resume_helper",
            "--nocapture",
        ])
        .env("AP_DRIVE_OWNED_ROOT", &f.core.root)
        .env("AP_DRIVE_OWNED_NONCE", &nonce)
        .env("AP_DRIVE_OWNED_CONNECTION", &f.id)
        .env("AP_DRIVE_OWNED_JOB", step["id"].as_str().unwrap())
        .env("AP_DRIVE_OWNED_BASE", f.m.lock().unwrap().base.clone());
    let out = tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap();
    if !out.status.success() {
        let diagnostic = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        f.clean();
        panic!("{diagnostic}");
    }
    assert!(String::from_utf8_lossy(&out.stdout).contains("1 passed"));
    let completed = f.status(&step).await;
    assert_eq!(completed["state"], "completed");
    assert_eq!(completed["record"]["cleanup_pending"], false);
    let grant = drive_auth::grant(&f.core, &f.id).await.unwrap();
    assert!(grant
        .capability_observe(completed["id"].as_str().unwrap())
        .unwrap()
        .is_none());
    drop(grant);
    f.clean();
}
#[tokio::test]
#[ignore = "Exact owned-process helper invoked by the native actor test"]
async fn owned_native_refresh_and_resume_helper() {
    let root = PathBuf::from(std::env::var("AP_DRIVE_OWNED_ROOT").unwrap());
    let canonical = root.canonicalize().unwrap();
    let outer = canonical.parent().unwrap();
    assert_eq!(
        outer.parent(),
        Some(std::env::temp_dir().canonicalize().unwrap().as_path())
    );
    assert!(outer
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("ap-drive-files-"));
    let core =
        Arc::new(Core::new(root, PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap());
    assert_eq!(
        core.store.setting("drive.owned_process_fixture"),
        std::env::var("AP_DRIVE_OWNED_NONCE").unwrap()
    );
    let connection = std::env::var("AP_DRIVE_OWNED_CONNECTION").unwrap();
    if std::env::var("AP_DRIVE_OWNED_MODE").as_deref() == Ok("cleanup") {
        drive_auth::cleanup_owned_native_grant(&core, &connection).unwrap();
        return;
    }
    let job = std::env::var("AP_DRIVE_OWNED_JOB").unwrap();
    let base = std::env::var("AP_DRIVE_OWNED_BASE").unwrap();
    let url = reqwest::Url::parse(&base).unwrap();
    assert_eq!(url.scheme(), "http");
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    *core.drive_fixture.lock().unwrap() = Some(drive_auth::TestTransport {
        token_url: format!("{base}/token"),
        api_base: format!("{base}/"),
        vault: Arc::new(Mutex::new(crate::drive_credentials::TestVault::default())),
        timeout_ms: 5000,
        native_storage: true,
        headless_permission: true,
    });
    let row = local::job(&core, &connection, &job, "upload").unwrap();
    let parent = core
        .execute(
            "drive.metadata",
            json!({"connection":connection,"file_id":"owned_folder"}),
        )
        .await
        .unwrap();
    let p=prepare(&core,"drive.upload",&json!({"connection":connection,"action":"resume","job_id":job,"expected":{"job_fingerprint":job_fingerprint(&row),"parent_fingerprint":parent["file_fingerprint"]}})).await.unwrap();
    let result = execute(p).await.unwrap();
    assert_eq!(result["state"], "completed");
    assert_eq!(result["record"]["cleanup_pending"], false);
}
#[test]
fn schema11_migration_preserves_rows_and_restorable_backup() {
    let root = std::env::temp_dir().join(format!("ap-drive-schema-{}", storage::id()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("owned.db");
    let store = crate::storage::Store::open(&path).unwrap();
    store
        .set_setting("owned_preserved", &json!("Señor 🐾"))
        .unwrap();
    drop(store);
    let previous_backups = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect::<std::collections::HashSet<_>>();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE chat_progress;DROP TABLE drive_jobs;PRAGMA user_version=11;")
        .unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='drive_jobs'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    drop(db);
    let store = crate::storage::Store::open(&path).unwrap();
    assert_eq!(store.setting("owned_preserved"), "Señor 🐾");
    drop(store);
    let backup = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            !previous_backups.contains(p)
                && p.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .contains("pre-v12-")
        })
        .unwrap();
    let db = rusqlite::Connection::open(backup).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        11
    );
    assert_eq!(
        db.query_row(
            "SELECT json_value FROM settings WHERE key='owned_preserved'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "\"Señor 🐾\""
    );
    assert_eq!(
        db.query_row("PRAGMA quick_check(1)", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    drop(db);
    let target = root.canonicalize().unwrap();
    assert_eq!(
        target.parent(),
        Some(std::env::temp_dir().canonicalize().unwrap().as_path())
    );
    assert!(target
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("ap-drive-schema-"));
    std::fs::remove_dir_all(target).unwrap();
}

#[path = "drive_download_tests.rs"]
mod download_recovery;
#[path = "drive_cleanup_tests.rs"]
mod retirement;
