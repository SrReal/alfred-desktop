use super::super::tests::Fixture;
use super::*;
use axum::{
    body::Bytes,
    extract::{OriginalUri, State as HttpState},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use std::sync::{atomic::AtomicUsize, Mutex};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
};

fn public_metadata() -> Value {
    json!({"public_client_supported":true,"basic_client_supported":true,"post_client_supported":true,"client_metadata_documents_supported":true,"registration_endpoint":"https://issuer.invalid/register","refresh_supported":true})
}
fn preregistration(secret: bool) -> Registered {
    Registered {
        info: Registration {
            mode: Mode::PreRegistered,
            redirect_uri: "http://127.0.0.1/oauth/mcp/callback".into(),
            refresh_registered: true,
            secret_expires_at: None,
            management_uri: None,
        },
        secret: secret.then(|| Text::new("owned-registration-secret".into(), 16384).unwrap()),
        management_token: None,
    }
}
fn actor(f: &Fixture, auth: ClientAuth) -> Actor {
    Actor::new(
        &f.core,
        &f.id,
        Authorization {
            issuer: "https://issuer.invalid".into(),
            resource: "https://mcp.invalid/mcp".into(),
            token_endpoint: "https://issuer.invalid/token".into(),
            client_id: "owned-client".into(),
            client_auth: auth,
            requested_scopes: Some(BTreeSet::from(["files:read".into()])),
        },
    )
    .unwrap()
}
fn publish_client(a: &Actor, registered: Registered) -> State {
    let state = a.begin(false).unwrap();
    a.transition(
        &state.session_id,
        Phase::Authorizing,
        Phase::RegistrationInFlight,
    )
    .unwrap();
    let envelope = a.registration_envelope(&state, registered).unwrap();
    a.transition(
        &state.session_id,
        Phase::RegistrationInFlight,
        Phase::PublicationPending,
    )
    .unwrap();
    a.publish(&state.session_id, envelope).unwrap()
}
#[test]
fn selection_prefers_preregistration_then_real_cimd_then_explicit_legacy_or_manual() {
    let metadata = public_metadata();
    let p = Plan::select(
        &metadata,
        Some(("existing", "http://127.0.0.1/oauth/mcp/callback")),
        Some("https://client.invalid/metadata.json"),
        true,
        ClientAuth::None,
    )
    .unwrap();
    assert_eq!(p.client_id().unwrap(), "existing");
    assert_eq!(p.review()["choice"]["type"], "pre_registered");
    let p = Plan::select(
        &metadata,
        None,
        Some("https://client.invalid/metadata.json"),
        true,
        ClientAuth::None,
    )
    .unwrap();
    assert_eq!(
        p.client_id().unwrap(),
        "https://client.invalid/metadata.json"
    );
    assert_eq!(
        Plan::select(&metadata, None, None, true, ClientAuth::None)
            .unwrap()
            .review()["choice"]["type"],
        "legacy_dynamic"
    );
    assert!(Plan::select(&metadata, None, None, false, ClientAuth::None)
        .unwrap()
        .client_id()
        .is_err());
    assert!(Plan::select(
        &metadata,
        None,
        Some("https://client.invalid/metadata.json"),
        true,
        ClientAuth::ClientSecretBasic
    )
    .is_err());
    for url in [
        "http://client.invalid/metadata.json",
        "https://client.invalid",
        "https://client.invalid/../metadata.json",
        "https://client.invalid/%2E%2E/metadata.json",
        "https://user:pass@client.invalid/metadata.json",
        "https://client.invalid/metadata.json#fragment",
    ] {
        assert!(Plan::select(&metadata, None, Some(url), true, ClientAuth::None).is_err());
    }
}
#[tokio::test]
async fn client_only_native_enrollment_survives_cancel_but_cannot_be_an_access_grant() {
    let f = Fixture::new().await;
    let a = actor(&f, ClientAuth::ClientSecretBasic);
    let ready = publish_client(&a, preregistration(true));
    assert!(ready.phase == Phase::ClientRegistered);
    assert!(a.read().is_err() && a.grant().is_err());
    let state = a.begin(false).unwrap();
    a.abandon(&state.session_id).unwrap();
    let cached = a.client().unwrap().unwrap();
    assert_eq!(
        cached.secret.as_ref().unwrap().text(),
        "owned-registration-secret"
    );
    let public = f
        .core
        .store
        .query("SELECT key,json_value FROM settings", &[])
        .unwrap()
        .to_string();
    assert!(!public.contains("owned-registration-secret"));
    let mut forged = a.state_held().unwrap();
    forged.phase = Phase::Ready;
    f.core
        .store
        .set_setting(&key(&f.id), &json!(forged))
        .unwrap();
    assert_eq!(a.read().err().unwrap(), "MCP_OAUTH_CREDENTIAL_CHANGED");
    forged.credential_version = ready.credential_version;
    forged.publication_id = ready.publication_id;
    f.core
        .store
        .set_setting(&key(&f.id), &json!(forged))
        .unwrap();
    assert_eq!(a.read().err().unwrap(), "MCP_OAUTH_CLIENT_ONLY_NOT_GRANT");
    drop(cached);
    drop(a);
    f.finish();
}
#[tokio::test]
async fn client_reference_reuses_only_client_credentials_across_reviewed_scopes_and_profile_generation(
) {
    let f = Fixture::new().await;
    let a = actor(&f, ClientAuth::ClientSecretBasic);
    publish_client(&a, preregistration(true));
    let mut auth = a.authorization.clone();
    auth.requested_scopes = Some(BTreeSet::from(["files:read".into(), "files:write".into()]));
    auth.token_endpoint = "https://issuer.invalid/new-token".into();
    f.core
        .store
        .set_setting(&mcp_save::generation_key(&f.id), &json!(storage::id()))
        .unwrap();
    let next = Actor::new(&f.core, &f.id, auth).unwrap();
    assert!(a.check().is_err());
    let client = next.client().unwrap().unwrap();
    assert_eq!(
        client.secret.as_ref().unwrap().text(),
        "owned-registration-secret"
    );
    assert!(next.read().is_err());
    publish_client(&next, client);
    assert!(next.state_held().unwrap().phase == Phase::ClientRegistered);
    let mut other = next.authorization.clone();
    other.issuer = "https://other.invalid".into();
    let other = Actor::new(&f.core, &f.id, other).unwrap();
    assert!(other.client().unwrap().is_none());
    drop(other);
    drop(next);
    drop(a);
    f.finish();
}
#[tokio::test]
async fn private_client_reference_cannot_adopt_a_changed_native_version_or_forged_identity() {
    let f = Fixture::new().await;
    let a = actor(&f, ClientAuth::None);
    let state = publish_client(&a, preregistration(false));
    let mut forged = state.clone();
    forged.client_reference.as_mut().unwrap().version = storage::id();
    f.core
        .store
        .set_setting(&key(&f.id), &json!(forged))
        .unwrap();
    assert!(a.client().is_err());
    let mut forged = state.clone();
    forged
        .client_reference
        .as_mut()
        .unwrap()
        .authorization
        .client_id = "foreign-client".into();
    f.core
        .store
        .set_setting(&key(&f.id), &json!(forged))
        .unwrap();
    assert!(a.client().unwrap().is_none());
    let mut forged = state;
    forged.client_reference.as_mut().unwrap().config_generation = storage::id();
    f.core
        .store
        .set_setting(&key(&f.id), &json!(forged))
        .unwrap();
    assert!(a.client().is_err());
    drop(a);
    f.finish();
}
#[tokio::test]
async fn registration_and_management_secrets_survive_tokens_and_refresh_but_expiry_blocks_renewal()
{
    let f = Fixture::new().await;
    let a = actor(&f, ClientAuth::ClientSecretBasic);
    let registered = Registered {
        info: Registration {
            mode: Mode::LegacyDynamic {
                registration_endpoint: "https://issuer.invalid/register".into(),
            },
            redirect_uri: "http://127.0.0.1:54321/oauth/mcp/callback".into(),
            refresh_registered: true,
            secret_expires_at: Some(0),
            management_uri: Some("https://issuer.invalid/client/owned".into()),
        },
        secret: Some(Text::new("owned-registration-secret".into(), 16384).unwrap()),
        management_token: Some(Text::new("owned-management-secret".into(), 16384).unwrap()),
    };
    publish_client(&a, registered);
    let registered = a.client().unwrap().unwrap();
    let state = a.begin(false).unwrap();
    a.transition(
        &state.session_id,
        Phase::Authorizing,
        Phase::ExchangeInFlight,
    )
    .unwrap();
    let env=a.tokens_registered(&state,json!({"token_type":"Bearer","access_token":"owned-access","refresh_token":"owned-refresh"}),registered).unwrap();
    a.transition(
        &state.session_id,
        Phase::ExchangeInFlight,
        Phase::PublicationPending,
    )
    .unwrap();
    a.publish(&state.session_id, env).unwrap();
    let (state, previous) = a.begin_refresh().unwrap();
    let env=a.tokens(&state,json!({"token_type":"Bearer","access_token":"owned-renewed","refresh_token":"owned-rotated"}),None,Some(previous)).unwrap();
    assert_eq!(
        env.registration_access_token.as_ref().unwrap().text(),
        "owned-management-secret"
    );
    a.transition(
        &state.session_id,
        Phase::RefreshInFlight,
        Phase::PublicationPending,
    )
    .unwrap();
    a.publish(&state.session_id, env).unwrap();
    let (_, mut env) = a.read().unwrap();
    env.registration.as_mut().unwrap().secret_expires_at = Some(chrono::Utc::now().timestamp() - 1);
    let state = a.begin(false).unwrap();
    a.transition(
        &state.session_id,
        Phase::Authorizing,
        Phase::ExchangeInFlight,
    )
    .unwrap();
    env.publication_id = state.publication_id.clone();
    a.transition(
        &state.session_id,
        Phase::ExchangeInFlight,
        Phase::PublicationPending,
    )
    .unwrap();
    a.publish(&state.session_id, env).unwrap();
    assert!(a.grant().is_ok());
    assert_eq!(
        a.begin_refresh().err().unwrap(),
        "MCP_OAUTH_CLIENT_SECRET_EXPIRED"
    );
    let public = f
        .core
        .store
        .query("SELECT json_value FROM settings", &[])
        .unwrap()
        .to_string();
    assert!(
        !public.contains("owned-management-secret")
            && !public.contains("owned-registration-secret")
    );
    drop(a);
    f.finish();
}
#[tokio::test]
async fn dynamic_response_requires_exact_redirect_auth_expiry_and_metadata_without_order_dependence(
) {
    let f = Fixture::new().await;
    let a = actor(&f, ClientAuth::ClientSecretBasic);
    let redirect = "http://127.0.0.1:54321/oauth/mcp/callback";
    let good = json!({"client_id":"issued-client","client_secret":"owned-issued-secret","client_secret_expires_at":0,"token_endpoint_auth_method":"client_secret_basic","redirect_uris":[redirect],"response_types":["code"],"grant_types":["refresh_token","authorization_code"]});
    assert!(dynamic_response(
        &a,
        "https://issuer.invalid/register",
        redirect,
        true,
        good.clone()
    )
    .is_ok());
    for (key, value) in [
        ("token_endpoint_auth_method", json!("none")),
        ("application_type", json!("web")),
        ("redirect_uris", json!(["http://127.0.0.1:54321/other"])),
        ("client_secret_expires_at", json!(-1)),
        (
            "grant_types",
            json!(["authorization_code", "authorization_code"]),
        ),
        ("client_id", json!("")),
        (
            "registration_access_token",
            json!("unpaired-management-token"),
        ),
    ] {
        let mut v = good.clone();
        v[key] = value;
        assert!(
            dynamic_response(&a, "https://issuer.invalid/register", redirect, true, v).is_err()
        );
    }
    let mut absent = good;
    absent
        .as_object_mut()
        .unwrap()
        .remove("client_secret_expires_at");
    assert!(dynamic_response(
        &a,
        "https://issuer.invalid/register",
        redirect,
        true,
        absent
    )
    .is_err());
    drop(a);
    f.finish();
}
#[tokio::test]
async fn actual_cimd_identity_and_registered_loopback_uri_are_required_and_secret_methods_rejected()
{
    let f = Fixture::new().await;
    let mut a = actor(&f, ClientAuth::None);
    a.authorization.client_id = "https://client.invalid/metadata.json".into();
    let good = json!({"client_id":a.authorization.client_id,"client_name":"AlfredPet","redirect_uris":["https://client.invalid/web-callback","http://127.0.0.1/oauth/mcp/callback"],"token_endpoint_auth_method":"none","grant_types":["authorization_code","refresh_token"]});
    assert!(client_document(
        &a,
        &a.authorization.client_id,
        "http://127.0.0.1:54321/oauth/mcp/callback",
        good.clone()
    )
    .is_ok());
    for (key, value) in [
        ("client_id", json!("https://CLIENT.invalid/metadata.json")),
        ("client_name", json!("")),
        ("client_secret", Value::Null),
        ("token_endpoint_auth_method", json!("client_secret_basic")),
        (
            "redirect_uris",
            json!(["http://localhost/oauth/mcp/callback"]),
        ),
        ("grant_types", json!(["client_credentials"])),
        (
            "grant_types",
            json!(["authorization_code", "authorization_code"]),
        ),
        ("response_types", json!(["code", 7])),
    ] {
        let mut value_doc = good.clone();
        value_doc[key] = value;
        assert!(client_document(
            &a,
            &a.authorization.client_id,
            "http://127.0.0.1:54321/oauth/mcp/callback",
            value_doc
        )
        .is_err());
    }
    drop(a);
    f.finish();
}

#[derive(Clone)]
struct Capture {
    register: Arc<AtomicUsize>,
    tokens: Arc<AtomicUsize>,
    documents: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<Value>>>,
    status: u16,
    bad_document: bool,
}
struct Server {
    base: String,
    task: tokio::task::JoinHandle<()>,
    capture: Capture,
}
impl Server {
    async fn new(status: u16, bad_document: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let capture = Capture {
            register: Arc::new(AtomicUsize::new(0)),
            tokens: Arc::new(AtomicUsize::new(0)),
            documents: Arc::new(AtomicUsize::new(0)),
            requests: Arc::new(Mutex::new(vec![])),
            status,
            bad_document,
        };
        let docbase = base.clone();
        let app=Router::new().route("/register",post(|HttpState(c):HttpState<Capture>,bytes:Bytes|async move {
            c.register.fetch_add(1,Ordering::SeqCst);let mut body:Value=serde_json::from_slice(&bytes).unwrap();c.requests.lock().unwrap().push(body.clone());body["client_id"]=json!("owned-issued-client");body["client_secret"]=json!("owned-issued-registration-secret");body["client_secret_expires_at"]=json!(0);body["registration_access_token"]=json!("owned-private-management-token");
            let redirect=body["redirect_uris"][0].as_str().unwrap();let origin=Url::parse(redirect).unwrap().origin().ascii_serialization();body["registration_client_uri"]=json!(format!("{origin}/client/owned"));
            (StatusCode::from_u16(c.status).unwrap(),[(header::CONTENT_TYPE,"application/json")],body.to_string()).into_response()
        })).route("/token",post(|HttpState(c):HttpState<Capture>,headers:axum::http::HeaderMap,bytes:Bytes|async move {
            c.tokens.fetch_add(1,Ordering::SeqCst);let form=Url::parse(&format!("http://localhost/?{}",std::str::from_utf8(&bytes).unwrap())).unwrap();let fields=form.query_pairs().collect::<std::collections::BTreeMap<_,_>>();assert_eq!(fields["resource"],"https://mcp.invalid/mcp");
            let public=c.requests.lock().unwrap().first().is_none_or(|v|v["token_endpoint_auth_method"]=="none");
            if public {assert!(!headers.contains_key(header::AUTHORIZATION));assert!(!fields.contains_key("client_secret"));assert!(fields.contains_key("client_id"));}
            else {assert!(headers[header::AUTHORIZATION].to_str().unwrap().starts_with("Basic "));assert!(!fields.contains_key("client_secret"));}
            ([(header::CONTENT_TYPE,"application/json")],json!({"token_type":"Bearer","access_token":"owned-registered-access","refresh_token":"owned-registered-refresh","scope":"files:read"}).to_string())
        })).route("/metadata.json",get(move |HttpState(c):HttpState<Capture>,OriginalUri(_):OriginalUri|{let base=docbase.clone();async move {
            c.documents.fetch_add(1,Ordering::SeqCst);
            ([(header::CONTENT_TYPE,"application/client+json")],json!({"client_id":if c.bad_document {format!("{base}/wrong.json")}else{format!("{base}/metadata.json")},"client_name":"AlfredPet","redirect_uris":["http://127.0.0.1/oauth/mcp/callback"],"token_endpoint_auth_method":"none"}).to_string())
        }})).with_state(capture.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            base,
            task,
            capture,
        }
    }
    fn actor(&self, f: &Fixture, auth: ClientAuth, id: &str) -> Actor {
        Actor::build(
            &f.core,
            &f.id,
            Authorization {
                issuer: self.base.clone(),
                resource: "https://mcp.invalid/mcp".into(),
                token_endpoint: format!("{}/token", self.base),
                client_id: id.into(),
                client_auth: auth,
                requested_scopes: Some(BTreeSet::from(["files:read".into()])),
            },
            true,
        )
        .unwrap()
    }
    fn plan(&self, auth: ClientAuth, cimd: bool) -> Plan {
        Plan {
            choice: if cimd {
                Choice::ClientMetadataDocument {
                    document_url: format!("{}/metadata.json", self.base),
                }
            } else {
                Choice::LegacyDynamic {
                    registration_endpoint: format!("{}/register", self.base),
                }
            },
            auth,
            refresh: true,
        }
    }
    async fn finish(self) {
        self.task.abort();
        assert!(self.task.await.unwrap_err().is_cancelled());
    }
}
async fn send_callback(consent: &super::super::flow::Consent, issuer: &str) {
    let url = Url::parse(consent.browser_url()).unwrap();
    let pairs = url
        .query_pairs()
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut redirect = Url::parse(&pairs["redirect_uri"]).unwrap();
    redirect
        .query_pairs_mut()
        .append_pair("state", &pairs["state"])
        .append_pair("code", "owned-registered-code")
        .append_pair("iss", issuer);
    let mut socket = TcpStream::connect(("127.0.0.1", redirect.port().unwrap()))
        .await
        .unwrap();
    socket
        .write_all(
            format!(
                "GET {}?{} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
                redirect.path(),
                redirect.query().unwrap(),
                redirect.port().unwrap()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
}
#[tokio::test]
async fn real_dcr_native_client_before_consent_survives_cancel_reuses_client_and_keeps_it_through_refresh(
) {
    let f = Fixture::new().await;
    let server = Server::new(201, false).await;
    let a = server.actor(
        &f,
        ClientAuth::ClientSecretBasic,
        "__alfredpet_dcr_pending__",
    );
    let consent = flow::Consent::bind_registered(
        a.clone(),
        &format!("{}/authorize", server.base),
        true,
        server.plan(ClientAuth::ClientSecretBasic, false),
        None,
    )
    .await
    .unwrap();
    assert_eq!(server.capture.register.load(Ordering::SeqCst), 1);
    assert_eq!(server.capture.tokens.load(Ordering::SeqCst), 0);
    let actual = server.actor(&f, ClientAuth::ClientSecretBasic, "owned-issued-client");
    assert!(actual.client().unwrap().is_some());
    assert!(actual.grant().is_err());
    actual.abandon(&consent.public_state().session_id).unwrap();
    drop(consent);
    let mut changed_endpoint_plan = server.plan(ClientAuth::ClientSecretBasic, false);
    changed_endpoint_plan.choice = Choice::LegacyDynamic {
        registration_endpoint: format!("{}/new-register", server.base),
    };
    let consent = flow::Consent::bind_registered(
        a.clone(),
        &format!("{}/authorize", server.base),
        true,
        changed_endpoint_plan,
        None,
    )
    .await
    .unwrap();
    assert_eq!(server.capture.register.load(Ordering::SeqCst), 1);
    send_callback(&consent, &server.base).await;
    assert!(consent.complete().await.unwrap().phase == Phase::Ready);
    flow::refresh(actual.clone()).await.unwrap();
    assert!(actual.client().unwrap().is_some());
    assert_eq!(server.capture.tokens.load(Ordering::SeqCst), 2);
    let public = f
        .core
        .store
        .query("SELECT json_value FROM settings", &[])
        .unwrap()
        .to_string();
    for secret in [
        "owned-issued-registration-secret",
        "owned-private-management-token",
        "owned-registered-access",
        "owned-registered-refresh",
    ] {
        assert!(!public.contains(secret));
    }
    {
        let requests = server.capture.requests.lock().unwrap();
        assert_eq!(requests[0]["application_type"], "native");
        assert_eq!(requests[0]["client_name"], "AlfredPet");
    }
    drop(actual);
    drop(a);
    server.finish().await;
    f.finish();
}
#[tokio::test]
async fn rejected_dcr_is_uncertain_and_never_retried_by_a_second_authorization() {
    let f = Fixture::new().await;
    let server = Server::new(429, false).await;
    let a = server.actor(
        &f,
        ClientAuth::ClientSecretBasic,
        "__alfredpet_dcr_pending__",
    );
    for index in 0..2 {
        let error = flow::Consent::bind_registered(
            a.clone(),
            &format!("{}/authorize", server.base),
            true,
            server.plan(ClientAuth::ClientSecretBasic, false),
            None,
        )
        .await
        .err()
        .unwrap();
        assert_eq!(
            error,
            if index == 0 {
                "MCP_OAUTH_REGISTRATION_HTTP_OUTCOME"
            } else {
                "MCP_OAUTH_CLIENT_RECOVERY_REQUIRED"
            }
        );
    }
    assert_eq!(server.capture.register.load(Ordering::SeqCst), 1);
    assert!(a.state_held().unwrap().phase == Phase::Uncertain);
    drop(a);
    server.finish().await;
    f.finish();
}
#[tokio::test]
async fn real_owned_cimd_get_validates_document_and_does_not_dynamically_register() {
    for bad in [false, true] {
        let f = Fixture::new().await;
        let server = Server::new(201, bad).await;
        let a = server.actor(
            &f,
            ClientAuth::None,
            &format!("{}/metadata.json", server.base),
        );
        let result = flow::Consent::bind_registered(
            a.clone(),
            &format!("{}/authorize", server.base),
            true,
            server.plan(ClientAuth::None, true),
            None,
        )
        .await;
        assert_eq!(server.capture.documents.load(Ordering::SeqCst), 1);
        assert_eq!(server.capture.register.load(Ordering::SeqCst), 0);
        assert_eq!(server.capture.tokens.load(Ordering::SeqCst), 0);
        if bad {
            assert!(result.is_err());
        } else {
            let consent = result.unwrap();
            assert!(a.client().unwrap().is_some());
            a.abandon(&consent.public_state().session_id).unwrap();
            drop(consent);
        }
        drop(a);
        server.finish().await;
        f.finish();
    }
}
#[tokio::test]
async fn empty_requested_scopes_fail_before_native_intent_and_http_side_effects() {
    let f = Fixture::new().await;
    let server = Server::new(201, false).await;
    let mut a = server.actor(&f, ClientAuth::None, "owned-client");
    a.authorization.requested_scopes = Some(BTreeSet::new());
    let before = f.core.store.setting(&key(&f.id));
    assert_eq!(
        flow::Consent::bind(a.clone(), &format!("{}/authorize", server.base), true, None)
            .await
            .err()
            .unwrap(),
        "MCP_OAUTH_EMPTY_SCOPE_REVIEW_REQUIRED"
    );
    assert_eq!(f.core.store.setting(&key(&f.id)), before);
    assert_eq!(server.capture.register.load(Ordering::SeqCst), 0);
    drop(a);
    server.finish().await;
    f.finish();
}

#[tokio::test]
async fn supplied_preregistered_secret_replaces_expired_native_client_without_reusing_old_access() {
    let f = Fixture::new().await;
    let a = actor(&f, ClientAuth::ClientSecretBasic);
    let mut expired = preregistration(true);
    expired.info.secret_expires_at = Some(chrono::Utc::now().timestamp() - 1);
    publish_client(&a, expired);
    assert_eq!(a.client().err().unwrap(), "MCP_OAUTH_CLIENT_SECRET_EXPIRED");
    let plan = Plan::select(
        &public_metadata(),
        Some(("owned-client", "http://127.0.0.1/oauth/mcp/callback")),
        None,
        false,
        ClientAuth::ClientSecretBasic,
    )
    .unwrap();
    let consent = flow::Consent::bind_registered(
        a.clone(),
        "https://issuer.invalid/authorize",
        true,
        plan,
        Some(Text::new("owned-replacement-secret".into(), 16384).unwrap()),
    )
    .await
    .unwrap();
    let cached = a.client().unwrap().unwrap();
    assert_eq!(
        cached.secret.as_ref().unwrap().text(),
        "owned-replacement-secret"
    );
    assert!(a.grant().is_err());
    a.abandon(&consent.public_state().session_id).unwrap();
    drop(cached);
    drop(consent);
    drop(a);
    f.finish();
}

#[tokio::test]
async fn real_public_dcr_discards_unused_issued_secret_and_uses_none_for_code_and_refresh() {
    let f = Fixture::new().await;
    let server = Server::new(201, false).await;
    let a = server.actor(&f, ClientAuth::None, "__alfredpet_dcr_pending__");
    let consent = flow::Consent::bind_registered(
        a.clone(),
        &format!("{}/authorize", server.base),
        true,
        server.plan(ClientAuth::None, false),
        None,
    )
    .await
    .unwrap();
    let actual = server.actor(&f, ClientAuth::None, "owned-issued-client");
    let cached = actual.client().unwrap().unwrap();
    assert!(cached.secret.is_none());
    assert!(cached.info.secret_expires_at.is_none());
    assert_eq!(
        cached.management_token.as_ref().unwrap().text(),
        "owned-private-management-token"
    );
    send_callback(&consent, &server.base).await;
    assert!(consent.complete().await.unwrap().phase == Phase::Ready);
    flow::refresh(actual.clone()).await.unwrap();
    assert_eq!(server.capture.register.load(Ordering::SeqCst), 1);
    assert_eq!(server.capture.tokens.load(Ordering::SeqCst), 2);
    assert!(actual.grant().is_ok());
    drop(cached);
    drop(actual);
    drop(a);
    server.finish().await;
    f.finish();
}
