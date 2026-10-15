use super::*;
use crate::storage;
use axum::{
    body::Body,
    extract::{Request, State},
    http::Response,
    Router,
};
use std::{os::windows::fs::MetadataExt, sync::Mutex};

fn repo() -> Value {
    json!({"id":7,"full_name":"owned/project","private":true,"archived":false,"disabled":false,"has_issues":true,"default_branch":"main","updated_at":"2026-10-03T00:00:00Z","permissions":{"push":true}})
}
fn pr() -> Value {
    json!({"id":8,"number":2,"title":"Review owned PR","body":"Owned proposed change","state":"open","draft":false,"merged":false,"mergeable":true,"updated_at":"2026-10-03T00:00:00Z","head":{"sha":"a".repeat(40),"ref":"owned","repo":{"id":9}},"base":{"sha":"b".repeat(40),"ref":"main","repo":{"id":7}}})
}
fn rel() -> Value {
    json!({"id":9,"name":"Owned draft","body":"Owned notes","tag_name":"v0.1.0","target_commitish":"main","draft":true,"prerelease":true,"immutable":false,"published_at":null,"assets":[{"id":11,"name":"owned.zip","size":12,"state":"uploaded","digest":"sha256:owned","updated_at":"2026-10-03T00:00:00Z"}]})
}
fn iss() -> Value {
    json!({"id":12,"number":3,"title":"Owned issue","body":"Owned content","state":"open","html_url":"https://github.com/owned/project/issues/3","updated_at":"2026-10-03T00:00:00Z"})
}
struct Mock {
    repo: Value,
    pr: Value,
    rel: Value,
    account: Value,
    seen: Vec<Value>,
    error: Option<(String, u16, String)>,
    link: Option<String>,
    delay: Option<String>,
    started: Arc<tokio::sync::Notify>,
    malformed_write: bool,
}
async fn handler(State(state): State<Arc<Mutex<Mock>>>, request: Request) -> Response<Body> {
    let path = request.uri().path().to_string();
    let uri = request.uri().to_string();
    let method = request.method().to_string();
    let version = request
        .headers()
        .get("x-github-api-version")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let correct_auth = request
        .headers()
        .get("authorization")
        .is_some_and(|v| v == "Bearer owned_fixture_pat");
    let bytes = axum::body::to_bytes(request.into_body(), 256 * 1024)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    let (delay, notify) = {
        let mut m = state.lock().unwrap();
        m.seen.push(json!({"method":method,"path":path,"uri":uri,"body":body,"apiVersion":version,"fictionalAuthMatched":correct_auth}));
        (m.delay.as_deref() == Some(&path), m.started.clone())
    };
    if delay {
        notify.notify_one();
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
    let mut m = state.lock().unwrap();
    if let Some((p, status, text)) = &m.error {
        if p == &path {
            return Response::builder()
                .status(*status)
                .header("content-type", "application/json")
                .header("retry-after", "1")
                .body(Body::from(text.clone()))
                .unwrap();
        }
    }
    let mut status = 200;
    let data = match (method.as_str(), path.as_str()) {
        ("GET", "/user") => m.account.clone(),
        ("GET", "/user/repos") => {
            let mut foreign = m.repo.clone();
            foreign["id"] = json!(77);
            foreign["full_name"] = json!("foreign/repository");
            json!([m.repo, foreign])
        }
        ("GET", "/repos/owned/project") => m.repo.clone(),
        ("GET", "/repos/owned/project/issues") => {
            let mut disguised = iss();
            disguised["pull_request"] = json!({});
            json!([iss(), disguised])
        }
        ("GET", "/repos/owned/project/pulls") => json!([m.pr]),
        ("GET", "/repos/owned/project/pulls/2") => m.pr.clone(),
        ("GET", "/repos/owned/project/releases") => json!([m.rel]),
        ("GET", "/repos/owned/project/releases/9") => m.rel.clone(),
        ("GET", "/repos/owned/project/actions/runs") => {
            json!({"workflow_runs":[{"id":33,"name":"Owned build","status":"completed","conclusion":"success","head_sha":"a".repeat(40),"run_number":1,"html_url":"https://github.com/owned/project/actions/runs/33"}]})
        }
        ("POST", "/repos/owned/project/issues") => {
            status = 201;
            let mut i = iss();
            i["title"] = body["title"].clone();
            i["body"] = body["body"].clone();
            i
        }
        ("PUT", "/repos/owned/project/pulls/2/merge") => {
            if body["sha"] != m.pr["head"]["sha"] {
                status = 409;
                json!({})
            } else {
                m.pr["merged"] = json!(true);
                m.pr["state"] = json!("closed");
                json!({"merged":true,"sha":"c".repeat(40)})
            }
        }
        ("PATCH", "/repos/owned/project/releases/9") => {
            assert_eq!(body, json!({"draft":false}));
            m.rel["draft"] = json!(false);
            m.rel["published_at"] = json!("2026-10-03T01:00:00Z");
            m.rel.clone()
        }
        _ => {
            status = 404;
            json!({"message":"Owned route not found"})
        }
    };
    let mut response = Response::builder()
        .status(status)
        .header("content-type", "application/json");
    if method == "GET" && path.ends_with("/issues") {
        if let Some(link) = &m.link {
            response = response.header("link", link);
        }
    }
    let text = if m.malformed_write && method != "GET" {
        "not-json".into()
    } else {
        data.to_string()
    };
    response.body(Body::from(text)).unwrap()
}
struct Fixture {
    core: Arc<Core>,
    id: String,
    root: PathBuf,
    identity: String,
    state: Arc<Mutex<Mock>>,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new() -> Self {
        let state = Arc::new(Mutex::new(Mock {
            repo: repo(),
            pr: pr(),
            rel: rel(),
            account: json!({"id":1,"login":"owned"}),
            seen: vec![],
            error: None,
            link: None,
            delay: None,
            started: Arc::new(tokio::sync::Notify::new()),
            malformed_write: false,
        }));
        let router = Router::new().fallback(handler).with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let root = std::env::temp_dir().join(format!("ap-github-{}", storage::id()));
        std::fs::create_dir(&root).unwrap();
        let identity = asset_files::identity(&asset_files::directory(&root).unwrap()).unwrap();
        let core = Arc::new(
            Core::new(
                root.clone(),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        let saved=crate::integrations::save(&core,&json!({"module_id":"git","name":"Owned GitHub fixture","config":{"repos":["owned/project"],"auth":"pat"}})).await.unwrap();
        let id = saved["id"].as_str().unwrap().to_string();
        *core.github_fixture.lock().unwrap() = Some(TestTransport {
            base: format!("http://{address}/"),
            pat: Some("owned_fixture_pat".into()),
            gh: None,
            execution_timeout: None,
        });
        Self {
            core,
            id,
            root,
            identity,
            state,
            server,
        }
    }
    fn input(&self, extra: Value) -> Value {
        let mut v = json!({"connection":self.id,"repo":"owned/project"});
        for (k, vv) in extra.as_object().unwrap() {
            v[k] = vv.clone();
        }
        v
    }
    async fn observe(&self, tool: &str, extra: Value) -> Value {
        self.core.execute(tool, self.input(extra)).await.unwrap()["expected"].clone()
    }
    fn writes(&self) -> Vec<Value> {
        self.state
            .lock()
            .unwrap()
            .seen
            .iter()
            .filter(|s| s["method"] != "GET")
            .cloned()
            .collect()
    }
    fn prefer(&self) {
        let config = json!({"repos":["owned/project"],"auth":"prefer_gh"});
        self.core
            .store
            .execute(
                "UPDATE connections SET config_json=? WHERE id=?",
                &[json!(config.to_string()), json!(self.id)],
            )
            .unwrap();
    }
    fn cli(&self, mode: &str) {
        self.prefer();
        let script = self.root.join("owned-gh.mjs");
        let responses = self.root.join("cli-responses.json");
        std::fs::write(&responses,json!({"/user":{"id":1,"login":"owned"},"/repos/owned/project":repo(),"/repos/owned/project/issues":iss()}).to_string()).unwrap();
        let log = self.root.join("cli-log.jsonl");
        let source = r#"import fs from 'node:fs';
const [mode,responses,log,...args]=process.argv.slice(2);
const endpoint=args.at(-1),method=args[args.indexOf('--method')+1];
let input='';if(args.includes('--input')){const chunks=[];for await(const chunk of process.stdin)chunks.push(chunk);input=Buffer.concat(chunks).toString('utf8');}
fs.appendFileSync(log,JSON.stringify({args,method,endpoint,input,ambientTokenPresent:!!(process.env.GH_TOKEN||process.env.GITHUB_TOKEN),ambientHostPresent:!!process.env.GH_HOST})+'\n');
if(mode==='unauth')process.exit(4);
if(mode==='wait'){fs.writeFileSync(log+'.ready','ready');await new Promise(r=>setTimeout(r,30000));}
if(mode==='large'){process.stdout.write('HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n'+'x'.repeat(5000000));}
else if(mode==='write-error'&&method!=='GET'){process.stdout.write('HTTP/1.1 500 Error\r\nContent-Type: application/json\r\n\r\n'+JSON.stringify({message:'owned failure'}));process.exitCode=1;}
else{const data=JSON.parse(fs.readFileSync(responses,'utf8'))[endpoint];if(method==='POST')Object.assign(data,JSON.parse(input));process.stdout.write('HTTP/1.1 '+(method==='POST'?'201 Created':'200 OK')+'\r\nContent-Type: application/json\r\n\r\n'+JSON.stringify(data));}
"#;
        std::fs::write(&script, source).unwrap();
        // Native argument vector and UTF-8 pipes like gh; no shell binder.
        let exe = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .filter(|p| p.is_absolute())
            .map(|p| p.join("node.exe"))
            .find(|p| p.is_file())
            .unwrap()
            .canonicalize()
            .unwrap();
        self.core
            .github_fixture
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .gh = Some((
            exe,
            vec![
                script.to_str().unwrap().into(),
                mode.into(),
                responses.to_str().unwrap().into(),
                log.to_str().unwrap().into(),
            ],
        ));
    }
    async fn clean(self) {
        self.server.abort();
        let _ = self.server.await;
        assert_eq!(
            self.root.canonicalize().unwrap().parent(),
            Some(std::env::temp_dir().canonicalize().unwrap().as_path())
        );
        assert!(self
            .root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("ap-github-"));
        assert_eq!(
            asset_files::identity(&asset_files::directory(&self.root).unwrap()).unwrap(),
            self.identity
        );
        let mut queue = vec![self.root.clone()];
        while let Some(p) = queue.pop() {
            assert_eq!(
                std::fs::symlink_metadata(&p).unwrap().file_attributes() & 0x400,
                0
            );
            if p.is_dir() {
                queue.extend(std::fs::read_dir(p).unwrap().map(|e| e.unwrap().path()));
            }
        }
        drop(self.core);
        // Windows can retain an owned process file briefly after cancellation.
        // Recheck the fixture identity and retry only sharing violations.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if !self.root.exists() {
                break;
            }
            assert_eq!(
                asset_files::identity(&asset_files::directory(&self.root).unwrap()).unwrap(),
                self.identity
            );
            match std::fs::remove_dir_all(&self.root) {
                Ok(()) => break,
                Err(error)
                    if error.raw_os_error() == Some(32) && std::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
                Err(error) => panic!("owned GitHub fixture cleanup failed: {error}"),
            }
        }
    }
}

#[test]
fn closed_catalog_connection_scope_and_mandatory_write_schemas() {
    for tool in TOOLS {
        let mut v = json!({"connection":storage::id(),"repo":"owned/project","number":2,"release_id":9,"title":"Owned","body":"Content","expected":{"fingerprint":"a".repeat(64)}});
        let schema = crate::catalog::tool_schema(tool, "");
        v.as_object_mut()
            .unwrap()
            .retain(|k, _| schema["properties"].get(k).is_some());
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.is_valid(&v), "{tool}");
        v["endpoint"] = json!("/arbitrary");
        assert!(!validator.is_valid(&v));
        assert_eq!(
            policy::classify(tool, &v),
            if mutation(tool) {
                policy::Risk::Mandatory
            } else {
                policy::Risk::Free
            }
        );
    }
    for name in [
        "../repo",
        "owner/repo/third",
        "-owner/repo",
        "owner/.",
        "owner/..",
        "https://github.com/owner/repo",
    ] {
        assert!(!repo_name(name));
    }
    for name in ["owner/.github", "owner/repo..name", "owner/-repo"] {
        assert!(repo_name(name));
    }
    assert!(
        validate_config(&json!({"repos":["owned/project"],"base_url":"http://127.0.0.1"})).is_err()
    );
    assert!(validate_config(&json!({"repos":["owned/project","OWNED/PROJECT"]})).is_err());
}
#[tokio::test]
async fn owned_http_lists_filter_repositories_issues_and_manual_pages() {
    let f = Fixture::new().await;
    let base = f
        .core
        .github_fixture
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .base
        .clone();
    f.state.lock().unwrap().link = Some(format!(
        "<{base}repos/owned/project/issues?per_page=2&page=2&state=open>; rel=\"next\""
    ));
    for tool in [
        "git.repos",
        "git.issues",
        "git.prs",
        "git.actions",
        "git.releases",
    ] {
        let mut input = f.input(json!({"per_page":2}));
        if tool == "git.repos" {
            input.as_object_mut().unwrap().remove("repo");
        }
        let r = f.core.execute(tool, input).await.unwrap();
        assert_eq!(r["items"].as_array().unwrap().len(), 1);
        assert_eq!(r["remote_content_untrusted"], true);
        if tool == "git.issues" {
            assert_eq!(r["next_page"], 2);
        }
    }
    let seen = f.state.lock().unwrap().seen.clone();
    assert!(seen
        .iter()
        .all(|s| s["fictionalAuthMatched"] == true && s["apiVersion"] == API_VERSION));
    assert!(!seen.iter().any(|s| reqwest::Url::parse(&format!(
        "http://127.0.0.1{}",
        s["uri"].as_str().unwrap()
    ))
    .unwrap()
    .query_pairs()
    .any(|(k, v)| k == "page" && v == "2")));
    f.clean().await;
}
#[tokio::test]
async fn observed_remote_identity_and_headless_permission_omit_content() {
    let f = Fixture::new().await;
    let expected = f.observe("git.repo", json!({})).await;
    let input = f
        .input(json!({"title":"Reviewed title","body":"Transient issue body","expected":expected}));
    assert_eq!(
        f.core.execute("git.create_issue", input).await.unwrap_err(),
        "APP_MISSING"
    );
    assert!(f.writes().is_empty());
    let a=f.core.store.query("SELECT input_summary,output_summary FROM activity_events WHERE tool_id='git.create_issue'",&[]).unwrap().to_string();
    assert!(!a.contains("Reviewed title") && !a.contains("Transient issue body"));
    assert!(a.contains("GITHUB_CONTENT_OMITTED"));
    let p = f
        .core
        .store
        .query("SELECT input_json_redacted FROM permission_requests", &[])
        .unwrap()
        .to_string();
    assert!(!p.contains("Transient issue body"));
    assert!(f.core.permission_previews.lock().unwrap().is_empty());
    f.clean().await;
}
#[tokio::test]
async fn reviewed_issue_uses_json_body_and_has_no_automatic_retry() {
    let f = Fixture::new().await;
    let expected = f.observe("git.repo", json!({})).await;
    let p=prepare(&f.core,"git.create_issue",&f.input(json!({"expected":expected,"title":"Owned Unicode ñ 🐾","body":"Literal @file {owner} \n content"}))).await.unwrap();
    assert_eq!(p.risk, policy::Risk::Mandatory);
    let r = execute(p).await.unwrap();
    assert_eq!(r["result"]["issue"]["title"], "Owned Unicode ñ 🐾");
    assert_eq!(f.writes().len(), 1);
    assert_eq!(
        f.writes()[0]["body"]["body"],
        "Literal @file {owner} \n content"
    );
    f.clean().await;
}
#[tokio::test]
async fn merge_rechecks_head_and_sends_server_enforced_sha() {
    let f = Fixture::new().await;
    let expected = f.observe("git.pr", json!({"number":2})).await;
    let input = f.input(json!({"number":2,"method":"squash","expected":expected}));
    let p = prepare(&f.core, "git.merge", &input).await.unwrap();
    f.state.lock().unwrap().pr["head"]["sha"] = json!("d".repeat(40));
    assert_eq!(execute(p).await.unwrap_err(), "GITHUB_STALE_OBSERVATION");
    assert!(f.writes().is_empty());
    let expected = f.observe("git.pr", json!({"number":2})).await;
    let p = prepare(
        &f.core,
        "git.merge",
        &f.input(json!({"number":2,"expected":expected})),
    )
    .await
    .unwrap();
    execute(p).await.unwrap();
    assert_eq!(
        f.writes()[0]["body"],
        json!({"sha":"d".repeat(40),"merge_method":"merge"})
    );
    f.clean().await;
}
#[tokio::test]
async fn draft_release_assets_body_and_publication_are_observed() {
    let f = Fixture::new().await;
    let expected = f.observe("git.release", json!({"release_id":9})).await;
    let p = prepare(
        &f.core,
        "git.publish_release",
        &f.input(json!({"release_id":9,"expected":expected})),
    )
    .await
    .unwrap();
    f.state.lock().unwrap().rel["assets"][0]["size"] = json!(13);
    assert_eq!(execute(p).await.unwrap_err(), "GITHUB_STALE_OBSERVATION");
    assert!(f.writes().is_empty());
    let expected = f.observe("git.release", json!({"release_id":9})).await;
    let p = prepare(
        &f.core,
        "git.publish_release",
        &f.input(json!({"release_id":9,"expected":expected})),
    )
    .await
    .unwrap();
    let result = execute(p).await.unwrap();
    assert_eq!(result["result"]["release"]["draft"], false);
    assert_eq!(f.writes()[0]["body"], json!({"draft":false}));
    f.clean().await;
}
#[test]
fn status_headers_json_and_cli_error_bodies_are_not_exposed() {
    for (code, want) in [
        (401, "GITHUB_AUTH_REQUIRED"),
        (403, "GITHUB_FORBIDDEN"),
        (404, "GITHUB_NOT_FOUND"),
        (409, "GITHUB_CONFLICT"),
        (422, "GITHUB_UNPROCESSABLE"),
        (429, "GITHUB_RATE_LIMITED"),
        (500, "GITHUB_SERVER_ERROR"),
        (302, "GITHUB_REDIRECT_DENIED"),
    ] {
        assert_eq!(
            decoded(code, BTreeMap::new(), b"untrusted secret body")
                .err()
                .unwrap(),
            want
        );
    }
    assert_eq!(
        gh_response(Some(4), b"").err().unwrap(),
        "GITHUB_AUTH_REQUIRED"
    );
    assert_eq!(
        gh_response(
            Some(0),
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\nnot-json"
        )
        .err()
        .unwrap(),
        "GITHUB_MALFORMED_JSON"
    );
    assert!(gh_response(
        Some(0),
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Type: text/html\r\n\r\n{}"
    )
    .is_err());
}
#[tokio::test]
async fn privacy_module_config_and_account_revocation_stop_requests() {
    let f = Fixture::new().await;
    let before = f.state.lock().unwrap().seen.len();
    assert_eq!(
        f.core
            .execute("git.repo", f.input(json!({"repo":"foreign/repository"})))
            .await
            .unwrap_err(),
        "GITHUB_REPOSITORY_SCOPE"
    );
    assert_eq!(f.state.lock().unwrap().seen.len(), before);
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(
        f.core
            .execute("git.repo", f.input(json!({})))
            .await
            .unwrap_err(),
        "PRIVACY_DENIED"
    );
    f.core.store.set_setting("privacy", &json!(false)).unwrap();
    let expected = f.observe("git.repo", json!({})).await;
    let p = prepare(
        &f.core,
        "git.create_issue",
        &f.input(json!({"expected":expected,"title":"Owned","body":"Owned"})),
    )
    .await
    .unwrap();
    f.state.lock().unwrap().account["id"] = json!(2);
    assert_eq!(execute(p).await.unwrap_err(), "GITHUB_ACCOUNT_CHANGED");
    assert!(f.writes().is_empty());
    f.clean().await;
}
#[tokio::test]
async fn inflight_owned_http_privacy_closes_before_the_deadline() {
    let f = Fixture::new().await;
    let notify = {
        let mut m = f.state.lock().unwrap();
        m.delay = Some("/repos/owned/project/issues".into());
        m.started.clone()
    };
    let core = f.core.clone();
    let input = f.input(json!({}));
    let task = tokio::spawn(async move { core.execute("git.issues", input).await });
    tokio::time::timeout(Duration::from_secs(5), notify.notified())
        .await
        .unwrap();
    let start = std::time::Instant::now();
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        "PRIVACY_DENIED"
    );
    assert!(start.elapsed() < Duration::from_secs(5));
    f.clean().await;
}
#[tokio::test]
async fn owned_native_credential_manager_pat_roundtrip_and_cleanup() {
    use windows::{
        core::PCWSTR,
        Win32::Security::Credentials::{
            CredDeleteW, CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC,
        },
    };
    struct OwnedCredential {
        target: Vec<u16>,
        bytes: Vec<u8>,
        armed: bool,
    }
    impl OwnedCredential {
        fn matches(&self) -> Result<Option<bool>, &'static str> {
            let mut ptr: *mut CREDENTIALW = std::ptr::null_mut();
            match unsafe {
                CredReadW(
                    PCWSTR(self.target.as_ptr()),
                    CRED_TYPE_GENERIC,
                    None,
                    &mut ptr,
                )
            } {
                Err(e) if e.code().0 as u32 == 0x80070490 => Ok(None),
                Err(_) => Err("OWNED_CREDENTIAL_READ_FAILED"),
                Ok(()) => {
                    if ptr.is_null() {
                        return Err("OWNED_CREDENTIAL_NULL");
                    }
                    let matches = unsafe {
                        let c = &*ptr;
                        c.CredentialBlobSize as usize == self.bytes.len()
                            && !c.CredentialBlob.is_null()
                            && std::slice::from_raw_parts(c.CredentialBlob, self.bytes.len())
                                == self.bytes
                    };
                    unsafe { CredFree(ptr.cast()) };
                    Ok(Some(matches))
                }
            }
        }
        fn remove(&mut self) -> Result<(), &'static str> {
            match self.matches()? {
                None => (),
                Some(false) => return Err("OWNED_CREDENTIAL_REPLACED_PRESERVED"),
                Some(true) => unsafe {
                    CredDeleteW(PCWSTR(self.target.as_ptr()), CRED_TYPE_GENERIC, None)
                        .map_err(|_| "OWNED_CREDENTIAL_DELETE_FAILED")?;
                },
            }
            if self.matches()?.is_some() {
                return Err("OWNED_CREDENTIAL_DELETE_NOT_CONFIRMED");
            }
            self.armed = false;
            Ok(())
        }
    }
    impl Drop for OwnedCredential {
        fn drop(&mut self) {
            if self.armed {
                // Exact UUID alias only; preserve any value replacing our fictional bytes.
                let _ = self.remove();
            }
            self.bytes.fill(0);
        }
    }
    let f = Fixture::new().await;
    let fictional = "owned_fixture_pat";
    let target = format!("alfredpet.connection.{}", f.id);
    let mut credential = OwnedCredential {
        target: target.encode_utf16().chain(Some(0)).collect(),
        bytes: fictional
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect(),
        armed: false,
    };
    assert_eq!(credential.matches().unwrap(), None);
    credential.armed = true;
    crate::integrations::save(&f.core, &json!({"id":f.id,"module_id":"git","name":"Owned native PAT fixture","config":{"repos":["owned/project"],"auth":"pat"},"secret":fictional})).await.unwrap();
    assert_eq!(credential.matches().unwrap(), Some(true));
    let case_variant = OwnedCredential {
        target: target
            .to_ascii_uppercase()
            .encode_utf16()
            .chain(Some(0))
            .collect(),
        bytes: credential.bytes.clone(),
        armed: false,
    };
    // This reads only the same fictional UUID credential through a case variant.
    assert_eq!(case_variant.matches().unwrap(), Some(true));
    drop(case_variant);
    let generation = f
        .core
        .store
        .setting(&format!("github.connection_generation.{}", f.id));
    assert_eq!(crate::integrations::save(&f.core, &json!({"id":f.id.to_ascii_uppercase(),"module_id":"notion","name":"Owned rejected provider","config":{},"secret":fictional})).await.unwrap_err(), "CONNECTION_MODULE_CHANGED");
    assert_eq!(credential.matches().unwrap(), Some(true));
    assert_eq!(
        f.core
            .store
            .setting(&format!("github.connection_generation.{}", f.id)),
        generation
    );
    assert_eq!(
        f.core
            .store
            .query("SELECT id,module_id FROM connections", &[])
            .unwrap(),
        json!([{"id":f.id,"module_id":"git"}])
    );
    f.core.github_fixture.lock().unwrap().as_mut().unwrap().pat = None;
    let result = f
        .core
        .execute("git.repo", f.input(json!({})))
        .await
        .unwrap();
    assert_eq!(result["authentication"], "credential_manager_pat");
    {
        let state = f.state.lock().unwrap();
        assert!(!state.seen.is_empty());
        assert!(state.seen.iter().all(|r| r["fictionalAuthMatched"] == true));
    }
    for query in [
        "SELECT config_json,secret_ref FROM connections",
        "SELECT key,json_value FROM settings",
        "SELECT input_summary,output_summary FROM activity_events",
    ] {
        assert!(!f
            .core
            .store
            .query(query, &[])
            .unwrap()
            .to_string()
            .contains(fictional));
    }
    assert!(!result.to_string().contains(fictional));
    credential.remove().unwrap();
    assert_eq!(credential.matches().unwrap(), None);
    drop(credential);
    f.clean().await;
}
#[tokio::test]
async fn connection_alias_casing_and_profile_actor_guards_fail_before_credentials() {
    let f = Fixture::new().await;
    let mut id = storage::id();
    id.replace_range(..1, "a"); // Keep UUID v4 bits; guarantee a different uppercase spelling.
    let upper = id.to_ascii_uppercase();
    assert_ne!(id, upper);
    let config = json!({"repos":["owned/project"],"auth":"pat"});
    f.core
        .store
        .execute(
            "INSERT INTO connections(id,module_id,name,config_json,secret_ref) VALUES(?,?,?,?,?)",
            &[
                json!(id),
                json!("git"),
                json!("Owned SQL alias"),
                json!(config.to_string()),
                json!(format!("connection.{id}")),
            ],
        )
        .unwrap();
    let before = f
        .core
        .store
        .query("SELECT * FROM connections ORDER BY id", &[])
        .unwrap();
    assert_eq!(
        crate::integrations::save(
            &f.core,
            &json!({"id":upper,"module_id":"git","name":"Owned rejected case","config":config})
        )
        .await
        .unwrap_err(),
        "GITHUB_CONNECTION_ID"
    );
    assert_eq!(
        crate::integrations::save(
            &f.core,
            &json!({"id":upper,"module_id":"notion","name":"Owned rejected module","config":{}})
        )
        .await
        .unwrap_err(),
        "CONNECTION_MODULE_CHANGED"
    );
    assert_eq!(crate::integrations::save(&f.core, &json!({"id":storage::id(),"module_id":"git","name":"Owned absent selector","config":config})).await.unwrap_err(), "CONNECTION_NOT_FOUND");
    assert_eq!(
        f.core
            .store
            .query("SELECT * FROM connections ORDER BY id", &[])
            .unwrap(),
        before
    );
    f.core
        .store
        .execute(
            "INSERT INTO connections(id,module_id,name,config_json,secret_ref) VALUES(?,?,?,?,?)",
            &[
                json!(upper),
                json!("notion"),
                json!("Owned ambiguous legacy row"),
                json!("{}"),
                json!(format!("connection.{upper}")),
            ],
        )
        .unwrap();
    assert_eq!(
        f.core
            .execute("git.repo", f.input(json!({"connection":id})))
            .await
            .unwrap_err(),
        "GITHUB_CONNECTION_ALIAS_CONFLICT"
    );
    assert_eq!(
        crate::integrations::save(
            &f.core,
            &json!({"id":id,"module_id":"git","name":"Owned ambiguous selector","config":config})
        )
        .await
        .unwrap_err(),
        "CONNECTION_ALIAS_CONFLICT"
    );
    assert!(f.state.lock().unwrap().seen.is_empty());
    f.core
        .store
        .execute("DELETE FROM connections WHERE id=?", &[json!(upper)])
        .unwrap();
    let lock = crate::integrations::save_lock(&f.core).unwrap();
    let other = Arc::new(
        Core::new(
            f.root.clone(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
        )
        .unwrap(),
    );
    let generation = f
        .core
        .store
        .setting(&format!("github.connection_generation.{}", f.id));
    assert_eq!(
        crate::integrations::save(
            &other,
            &json!({"id":f.id,"module_id":"git","name":"Owned blocked save","config":config})
        )
        .await
        .unwrap_err(),
        "CONNECTION_SAVE_BUSY"
    );
    assert_eq!(
        f.core
            .execute("git.repo", f.input(json!({})))
            .await
            .unwrap_err(),
        "CONNECTION_SAVE_BUSY"
    );
    assert!(f.state.lock().unwrap().seen.is_empty());
    assert_eq!(
        f.core
            .store
            .setting(&format!("github.connection_generation.{}", f.id)),
        generation
    );
    assert_eq!(
        f.core
            .store
            .query("SELECT * FROM connections ORDER BY id", &[])
            .unwrap(),
        before
    );
    drop(other);
    drop(lock);
    f.core
        .execute("git.repo", f.input(json!({"connection":id})))
        .await
        .unwrap();
    assert!(!f.state.lock().unwrap().seen.is_empty());
    f.clean().await;
}
#[tokio::test]
async fn credential_generation_save_invalidates_existing_preparation() {
    let f = Fixture::new().await;
    let expected = f.observe("git.repo", json!({})).await;
    let p = prepare(
        &f.core,
        "git.create_issue",
        &f.input(json!({"expected":expected,"title":"Owned","body":"Owned"})),
    )
    .await
    .unwrap();
    crate::integrations::save(&f.core,&json!({"id":f.id,"module_id":"git","name":"Reviewed connection","config":{"repos":["owned/project"],"auth":"pat"}})).await.unwrap();
    assert_eq!(execute(p).await.unwrap_err(), "GITHUB_CONNECTION_CHANGED");
    assert!(f.writes().is_empty());
    let error = crate::integrations::save(
        &f.core,
        &json!({"id":f.id,"module_id":"notion","name":"Different module","config":{}}),
    )
    .await
    .unwrap_err();
    assert_eq!(error, "CONNECTION_MODULE_CHANGED");
    f.clean().await;
}
#[tokio::test]
async fn malformed_successful_write_is_uncertain_and_never_replayed() {
    let f = Fixture::new().await;
    let expected = f.observe("git.repo", json!({})).await;
    let p = prepare(
        &f.core,
        "git.create_issue",
        &f.input(json!({"expected":expected,"title":"Owned","body":"Owned"})),
    )
    .await
    .unwrap();
    f.state.lock().unwrap().malformed_write = true;
    let e = execute(p).await.unwrap_err();
    assert!(e.starts_with("GITHUB_MUTATION_OUTCOME_UNCERTAIN: GITHUB_MALFORMED_JSON"));
    assert_eq!(f.writes().len(), 1);
    f.clean().await;
}
#[tokio::test]
async fn owned_remote_mutation_execution_deadline_is_uncertain_without_replay() {
    let f = Fixture::new().await;
    f.core
        .github_fixture
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .execution_timeout = Some(Duration::from_secs(2));
    let expected = f.observe("git.repo", json!({})).await;
    let p = prepare(
        &f.core,
        "git.create_issue",
        &f.input(json!({"expected":expected,"title":"Owned timed issue","body":"Owned deadline"})),
    )
    .await
    .unwrap();
    let notify = {
        let mut m = f.state.lock().unwrap();
        m.delay = Some("/repos/owned/project/issues".into());
        m.started.clone()
    };
    let task = tokio::spawn(execute(p));
    tokio::time::timeout(Duration::from_secs(5), notify.notified())
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(result, "GITHUB_MUTATION_OUTCOME_UNCERTAIN: GITHUB_EXECUTION_TIMEOUT; inspect remote state before retry");
    assert_eq!(f.writes().len(), 1);
    assert_eq!(
        f.writes()[0]["body"],
        json!({"title":"Owned timed issue","body":"Owned deadline"})
    );
    f.clean().await;
}
#[tokio::test]
async fn malicious_link_and_sensitive_write_cannot_expand_scope() {
    let f = Fixture::new().await;
    f.state.lock().unwrap().link = Some(
        "<http://127.0.0.2/repos/owned/project/issues?page=2&per_page=30&state=open>; rel=\"next\""
            .into(),
    );
    assert_eq!(
        f.core
            .execute("git.issues", f.input(json!({})))
            .await
            .unwrap_err(),
        "GITHUB_PAGE_SCOPE"
    );
    let before = f.state.lock().unwrap().seen.len();
    let e=f.core.execute("git.create_issue",f.input(json!({"title":"ghp_".to_owned()+&"x".repeat(40),"body":"Owned","expected":{"fingerprint":"a".repeat(64)}}))).await.unwrap_err();
    assert_eq!(e, "GITHUB_SENSITIVE_INPUT");
    assert_eq!(f.state.lock().unwrap().seen.len(), before);
    assert!(f.writes().is_empty());
    f.clean().await;
}
#[tokio::test]
async fn owned_cli_preferred_stdin_payload_and_environment_are_bounded() {
    let f = Fixture::new().await;
    f.cli("ok");
    let expected = f.observe("git.repo", json!({})).await;
    let p = prepare(
        &f.core,
        "git.create_issue",
        &f.input(
            json!({"expected":expected,"title":"Owned ñ 🐾","body":"Body with @file and {owner} ñ 🐾"}),
        ),
    )
    .await
    .unwrap();
    let r = execute(p).await.unwrap();
    assert_eq!(r["authentication"], "gh");
    assert!(f.state.lock().unwrap().seen.is_empty());
    let records: Vec<Value> = std::fs::read_to_string(f.root.join("cli-log.jsonl"))
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert!(records
        .iter()
        .all(|r| r["ambientTokenPresent"] == false && r["ambientHostPresent"] == false));
    let writes: Vec<_> = records.iter().filter(|r| r["method"] != "GET").collect();
    assert_eq!(writes.len(), 1);
    assert_eq!(
        serde_json::from_str::<Value>(writes[0]["input"].as_str().unwrap()).unwrap()["body"],
        "Body with @file and {owner} ñ 🐾"
    );
    assert!(!writes[0]["args"].to_string().contains("Body with"));
    f.clean().await;
}
#[tokio::test]
async fn owned_cli_authentication_fallback_only_precedes_operation() {
    let f = Fixture::new().await;
    f.cli("unauth");
    let r = f
        .core
        .execute("git.repo", f.input(json!({})))
        .await
        .unwrap();
    assert_eq!(r["authentication"], "credential_manager_pat");
    assert!(!f.state.lock().unwrap().seen.is_empty());
    f.state.lock().unwrap().seen.clear();
    f.cli("write-error");
    let expected = f.observe("git.repo", json!({})).await;
    let p = prepare(
        &f.core,
        "git.create_issue",
        &f.input(json!({"expected":expected,"title":"Owned","body":"Owned"})),
    )
    .await
    .unwrap();
    assert!(execute(p)
        .await
        .unwrap_err()
        .starts_with("GITHUB_MUTATION_OUTCOME_UNCERTAIN: GITHUB_SERVER_ERROR"));
    assert!(f.state.lock().unwrap().seen.is_empty());
    f.clean().await;
}
#[tokio::test]
async fn owned_cli_output_limit_and_active_process_revocation() {
    let f = Fixture::new().await;
    f.cli("large");
    assert_eq!(
        f.core
            .execute("git.repo", f.input(json!({})))
            .await
            .unwrap_err(),
        "GITHUB_RESPONSE_LIMIT"
    );
    f.cli("wait");
    let core = f.core.clone();
    let input = f.input(json!({}));
    let task = tokio::spawn(async move { core.execute("git.repo", input).await });
    let ready = f.root.join("cli-log.jsonl.ready");
    tokio::time::timeout(Duration::from_secs(8), async {
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='git'",
            &[],
        )
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        "MODULE_DISABLED: git"
    );
    f.clean().await;
}

#[tokio::test]
async fn owned_http_response_bounds_and_rate_errors_fail_closed() {
    let f = Fixture::new().await;
    for (status, text, want) in [
        (403, "{}".to_string(), "GITHUB_RATE_LIMITED"),
        (429, "{}".to_string(), "GITHUB_RATE_LIMITED"),
        (500, "{}".to_string(), "GITHUB_SERVER_ERROR"),
        (200, "not-json".to_string(), "GITHUB_MALFORMED_JSON"),
        (200, "x".repeat(LIMIT + 1), "GITHUB_RESPONSE_LIMIT"),
    ] {
        f.state.lock().unwrap().error = Some(("/repos/owned/project/issues".into(), status, text));
        assert_eq!(
            f.core
                .execute("git.issues", f.input(json!({})))
                .await
                .unwrap_err(),
            want
        );
    }
    assert!(f.writes().is_empty());
    f.clean().await;
}

#[tokio::test]
async fn owned_chunked_body_privacy_drop_closes_the_socket() {
    let f = Fixture::new().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    f.core.github_fixture.lock().unwrap().as_mut().unwrap().base = format!("http://{address}/");
    let started = Arc::new(tokio::sync::Notify::new());
    let observed = started.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 2048];
            while !bytes.windows(4).any(|b| b == b"\r\n\r\n") {
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0 && bytes.len() + n <= 8192);
                bytes.extend_from_slice(&buffer[..n]);
            }
            let line = std::str::from_utf8(&bytes).unwrap().lines().next().unwrap();
            let path = line.split_whitespace().nth(1).unwrap();
            if path.starts_with("/repos/owned/project/issues") {
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n1\r\n[\r\n").await.unwrap();
                observed.notify_one();
                let n = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut buffer))
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(n, 0, "cancelled client must close its owned socket");
                return;
            }
            let data = if path == "/user" {
                json!({"id":1,"login":"owned"})
            } else {
                assert_eq!(path, "/repos/owned/project");
                repo()
            };
            let body = data.to_string();
            let response=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let core = f.core.clone();
    let input = f.input(json!({}));
    let task = tokio::spawn(async move { core.execute("git.issues", input).await });
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        "PRIVACY_DENIED"
    );
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    f.clean().await;
}
