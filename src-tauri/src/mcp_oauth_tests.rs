use super::*;
use axum::{
    body::Body,
    extract::{Request, State},
    http::Response,
    Router,
};
use std::sync::{Arc, Mutex};

fn config(base: &str, path: &str) -> mcp_profile::Config {
    mcp_profile::config(&json!({"profile_version":1,"transport":"streamable_http","base_url":base,"endpoint":path,"auth":{"type":"none"}})).unwrap()
}
fn as_metadata(issuer: &str) -> Value {
    json!({"issuer":issuer,"authorization_endpoint":format!("{issuer}/consent"),"token_endpoint":format!("{issuer}/exchange"),"registration_endpoint":format!("{issuer}/register"),"response_types_supported":["code"],"grant_types_supported":["authorization_code","refresh_token"],"code_challenge_methods_supported":["S256"],"token_endpoint_auth_methods_supported":["none","client_secret_basic"],"client_id_metadata_document_supported":true,"authorization_response_iss_parameter_supported":true,"scopes_supported":["advertised-only"],"client_secret":"never published unknown field"})
}
#[test]
fn as_endpoint_query_components_are_preserved_but_issuer_queries_are_denied() {
    let issuer = "https://as.example/tenant";
    let mut metadata = as_metadata(issuer);
    metadata["authorization_endpoint"] = json!("https://as.example/consent?ui_locales=es");
    metadata["token_endpoint"] = json!("https://as.example/exchange?tenant=owned");
    metadata["registration_endpoint"] = json!("https://as.example/register?tenant=owned");
    let reviewed = authorization_server(&metadata, issuer, Network::Production).unwrap();
    assert_eq!(
        reviewed["consent_endpoint"],
        metadata["authorization_endpoint"]
    );
    assert_eq!(reviewed["exchange_endpoint"], metadata["token_endpoint"]);
    assert_eq!(
        reviewed["registration_endpoint"],
        metadata["registration_endpoint"]
    );
    assert!(Plan::new(
        &config("https://resource.example", "/mcp"),
        &json!({"issuer":"https://as.example/tenant?tenant=owned"})
    )
    .is_err());
    metadata["token_endpoint"] = json!("https://as.example/exchange#fragment");
    assert!(authorization_server(&metadata, issuer, Network::Production).is_err());
}
#[test]
fn reviewed_candidates_include_rfc8414_and_both_oidc_path_forms() {
    let p = Plan::new(
        &config("https://resource.example", "/public/mcp"),
        &json!({"issuer":"https://as.example/tenant"}),
    )
    .unwrap();
    assert_eq!(
        p.server_candidates
            .iter()
            .map(Url::as_str)
            .collect::<Vec<_>>(),
        vec![
            "https://as.example/.well-known/oauth-authorization-server/tenant",
            "https://as.example/.well-known/openid-configuration/tenant",
            "https://as.example/tenant/.well-known/openid-configuration"
        ]
    );
    assert_eq!(
        p.resource_candidates[0].as_str(),
        "https://resource.example/.well-known/oauth-protected-resource/public/mcp"
    );
    assert_eq!(
        p.resource_candidates[1].as_str(),
        "https://resource.example/.well-known/oauth-protected-resource"
    );
    assert_eq!(p.review()["anonymous"], true);
    assert_eq!(p.review()["methods"], json!(["GET"]));
    for issuer in ["https://as.example", "https://as.example/"] {
        assert_eq!(
            Plan::new(
                &config("https://resource.example", "/"),
                &json!({"issuer":issuer})
            )
            .unwrap()
            .server_candidates
            .len(),
            2
        );
    }
    let p = Plan::new(
        &config("https://resource.example", "/mcp"),
        &json!({"issuer":"https://as.example/tenant/"}),
    )
    .unwrap();
    assert!(p.server_candidates[0].as_str().ends_with("/tenant/"));
}
#[test]
fn production_as_requires_tls_and_urls_exclude_credentials_queries_fragments() {
    for issuer in [
        "http://127.0.0.1:44",
        "http://as.example",
        "https://u:p@as.example",
        "https://as.example?x=y",
        "https://as.example#x",
        "https://as.example\\route",
        " https://as.example",
    ] {
        assert!(
            Plan::new(
                &config("https://resource.example", "/mcp"),
                &json!({"issuer":issuer})
            )
            .is_err(),
            "{issuer}"
        );
    }
    let mut c = config("https://resource.example", "/mcp");
    c.transport = mcp_profile::Transport::Stdio;
    assert!(Plan::new(&c, &json!({"issuer":"https://as.example"})).is_err());
    assert!(Plan::new(
        &config("https://resource.example", "/mcp"),
        &json!({"issuer":"https://as.example","metadata_url":"http://localhost:44/private"})
    )
    .is_err());
}
#[test]
fn resource_and_selected_issuer_must_match_metadata() {
    let resource = Url::parse("https://resource.example/mcp").unwrap();
    let good = json!({"resource":resource.as_str(),"authorization_servers":["https://as.example","https://other.example"],"bearer_methods_supported":["header"]});
    assert!(
        protected_resource(&good, &resource, "https://as.example", Network::Production).is_ok()
    );
    for (key, v) in [
        ("resource", json!("https://resource.example/else")),
        ("authorization_servers", json!([])),
        ("authorization_servers", json!(["https://other.example"])),
        (
            "authorization_servers",
            json!(["https://as.example", "https://as.example"]),
        ),
        ("bearer_methods_supported", json!(["query"])),
    ] {
        let mut bad = good.clone();
        bad[key] = v;
        assert!(
            protected_resource(&bad, &resource, "https://as.example", Network::Production).is_err()
        );
    }
}
#[test]
fn issuer_is_exact_with_no_case_port_slash_or_percent_normalization() {
    let expected = "https://AS.example:443/tenant%2fchild/";
    let good = as_metadata(expected);
    assert!(authorization_server(&good, expected, Network::Production).is_ok());
    for issuer in [
        "https://as.example:443/tenant%2fchild/",
        "https://AS.example/tenant%2fchild/",
        "https://AS.example:443/tenant%2Fchild/",
        "https://AS.example:443/tenant%2fchild",
    ] {
        assert_eq!(
            authorization_server(&good, issuer, Network::Production).unwrap_err(),
            "MCP_OAUTH_ISSUER_MISMATCH"
        );
    }
}
#[test]
fn metadata_requires_s256_code_and_typed_boolean_capabilities() {
    let good = as_metadata("https://as.example");
    for (key, value) in [
        ("code_challenge_methods_supported", Value::Null),
        ("code_challenge_methods_supported", json!(["plain"])),
        ("response_types_supported", json!(["token"])),
        ("grant_types_supported", json!(["client_credentials"])),
        ("client_id_metadata_document_supported", json!("true")),
        ("authorization_response_iss_parameter_supported", json!(1)),
        ("token_endpoint", json!("http://localhost:88/exchange")),
    ] {
        let mut bad = good.clone();
        bad[key] = value;
        assert!(
            authorization_server(&bad, "https://as.example", Network::Production).is_err(),
            "{key}"
        );
    }
    let mut missing = good.clone();
    missing
        .as_object_mut()
        .unwrap()
        .remove("code_challenge_methods_supported");
    assert_eq!(
        authorization_server(&missing, "https://as.example", Network::Production).unwrap_err(),
        "MCP_OAUTH_PKCE_REQUIRED"
    );
}
#[test]
fn public_metadata_omits_unknown_content_and_keeps_reviewable_endpoints() {
    let result = authorization_server(
        &as_metadata("https://as.example"),
        "https://as.example",
        Network::Production,
    )
    .unwrap();
    assert!(!result.to_string().contains("never published"));
    assert_eq!(crate::policy::redact(&result), result);
    assert_eq!(result["exchange_endpoint"], "https://as.example/exchange");
    assert_eq!(result["client_metadata_documents_supported"], true);
    assert_eq!(result["callback_issuer_required"], true);
    assert_eq!(result["refresh_supported"], true);
}
#[test]
fn duplicate_json_keys_wrong_types_depth_and_size_are_rejected() {
    for bad in [
        r#"{"issuer":"a","issuer":"b"}"#,
        r#"{"nested":{"a":1,"a":2}}"#,
        r#"{"a":1} trailing"#,
        r#"[]"#,
        r#"{"a":"\ud800"}"#,
    ] {
        assert!(strict_json(bad.as_bytes()).is_err());
    }
    assert!(strict_json(&vec![b' '; LIMIT + 1]).is_err());
    let mut deep = json!(0);
    for _ in 0..34 {
        deep = json!({"x":deep});
    }
    assert!(strict_json(deep.to_string().as_bytes()).is_err());
    assert_eq!(
        strict_json(r#"{"text":"Señor","float":1.5,"null":null,"list":[true]}"#.as_bytes())
            .unwrap()["text"],
        "Señor"
    );
}
fn header(text: &str) -> header::HeaderMap {
    let mut h = header::HeaderMap::new();
    h.insert(
        header::WWW_AUTHENTICATE,
        header::HeaderValue::from_str(text).unwrap(),
    );
    h
}
#[test]
fn challenge_parser_handles_multiple_schemes_quoted_commas_and_case() {
    let c=challenge(&header(r#"Basic realm="unrelated,realm", bEaReR ReAlM="a\"b", resource_metadata="https://r.example/prm", scope="dynamic:write files:read""#)).unwrap();
    assert_eq!(c.metadata.as_deref(), Some("https://r.example/prm"));
    assert_eq!(c.scopes.unwrap(), vec!["dynamic:write", "files:read"]);
    assert!(challenge(&header("Basic realm=unrelated"))
        .unwrap()
        .metadata
        .is_none());
    assert!(challenge(&header("Bearer")).unwrap().scopes.is_none());
    assert_eq!(
        challenge(&header("Negotiate YIIB+abc/==, Bearer scope=read "))
            .unwrap()
            .scopes
            .unwrap(),
        vec!["read"]
    );
}
#[test]
fn ambiguous_duplicate_or_malformed_bearer_challenges_cannot_choose_a_destination() {
    for text in [
        r#"Bearer scope="x", Scope="y""#,
        r#"Bearer scope="x", Bearer scope="y""#,
        r#"Bearer scope="x  y""#,
        r#"Bearer scope="x x""#,
        r#"Bearer scope="""#,
        r#"Bearer scope="unterminated"#,
        r#"Bearer abc"#,
        r#"Bearer scope="x","#,
    ] {
        assert!(challenge(&header(text)).is_err(), "{text}");
    }
    let mut h = header("Bearer scope=x");
    h.append(
        header::WWW_AUTHENTICATE,
        header::HeaderValue::from_static("Bearer scope=x"),
    );
    assert!(challenge(&h).is_err());
}

#[derive(Clone)]
struct Reply {
    status: u16,
    body: String,
    content: String,
    challenge: Option<String>,
    location: Option<String>,
}
impl Reply {
    fn json(v: Value) -> Self {
        Self {
            status: 200,
            body: v.to_string(),
            content: "application/json; charset=utf-8".into(),
            challenge: None,
            location: None,
        }
    }
}
struct Mock {
    routes: BTreeMap<String, Reply>,
    seen: Vec<Value>,
}
async fn handler(State(m): State<Arc<Mutex<Mock>>>, r: Request) -> Response<Body> {
    let mut m = m.lock().unwrap();
    m.seen.push(json!({"method":r.method().as_str(),"path":r.uri().path(),"query":r.uri().query(),"headers":r.headers().iter().map(|(k,v)|(k.as_str().to_owned(),v.to_str().unwrap_or("").to_owned())).collect::<BTreeMap<_,_>>()}));
    let reply = m.routes.get(r.uri().path()).cloned().unwrap_or(Reply {
        status: 404,
        ..Reply::json(json!({}))
    });
    let mut b = Response::builder()
        .status(reply.status)
        .header("content-type", reply.content);
    if let Some(c) = reply.challenge {
        b = b.header("www-authenticate", c);
    }
    if let Some(l) = reply.location {
        b = b.header("location", l);
    }
    b.body(Body::from(reply.body)).unwrap()
}
struct Fixture {
    base: String,
    issuer: String,
    m: Arc<Mutex<Mock>>,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let issuer = format!("{base}/tenant");
        let mut routes = BTreeMap::new();
        routes.insert(
            "/mcp".into(),
            Reply {
                status: 401,
                challenge: Some("Bearer scope=\"dynamic:write\"".into()),
                ..Reply::json(json!({}))
            },
        );
        routes.insert("/.well-known/oauth-protected-resource/mcp".into(),Reply::json(json!({"resource":format!("{base}/mcp"),"authorization_servers":[issuer.clone()],"scopes_supported":["minimal:read"]})));
        routes.insert(
            "/.well-known/oauth-authorization-server/tenant".into(),
            Reply::json(as_metadata(&issuer)),
        );
        let m = Arc::new(Mutex::new(Mock {
            routes,
            seen: vec![],
        }));
        let router = Router::new().fallback(handler).with_state(m.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            base,
            issuer,
            m,
            server,
        }
    }
    fn plan(&self, extra: Value) -> Plan {
        let mut input = extra;
        input["issuer"] = json!(self.issuer);
        let mut c = config(&self.base, "/mcp");
        c.headers.insert(
            "x-public".into(),
            mcp_profile::Header {
                value: Some("not inherited".into()),
                secret_ref: None,
            },
        );
        Plan::build(&c, &input, Network::OwnedLoopback).unwrap()
    }
    fn set(&self, path: &str, reply: Reply) {
        self.m.lock().unwrap().routes.insert(path.into(), reply);
    }
    fn seen(&self) -> Vec<Value> {
        self.m.lock().unwrap().seen.clone()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
#[tokio::test]
async fn owned_http_discovery_uses_authoritative_challenge_scopes_and_no_profile_headers() {
    let f = Fixture::new().await;
    let result = f.plan(json!({})).discover().await.unwrap();
    assert_eq!(result["requested_scopes"], json!(["dynamic:write"]));
    assert_eq!(result["scope_source"], "challenge");
    assert_eq!(result["browserStarted"], false);
    assert_eq!(result["credentialsStored"], false);
    assert_eq!(result["server_metadata"]["issuer"], f.issuer);
    assert_eq!(crate::policy::redact(&result), result);
    assert!(!result.to_string().contains("never published"));
    assert_eq!(f.seen().len(), 3);
    for request in f.seen() {
        assert_eq!(request["method"], "GET");
        assert!(request["query"].is_null());
        for key in ["authorization", "cookie", "x-public"] {
            assert!(request["headers"].get(key).is_none());
        }
    }
}
#[tokio::test]
async fn missing_challenge_scope_uses_prm_minimal_set_and_missing_set_omits_scope() {
    let f = Fixture::new().await;
    f.set(
        "/mcp",
        Reply {
            status: 405,
            ..Reply::json(json!({}))
        },
    );
    let result = f.plan(json!({})).discover().await.unwrap();
    assert_eq!(result["requested_scopes"], json!(["minimal:read"]));
    assert_eq!(result["scope_source"], "protected_resource_metadata");
    f.set(
        "/.well-known/oauth-protected-resource/mcp",
        Reply::json(
            json!({"resource":format!("{}/mcp",f.base),"authorization_servers":[f.issuer.clone()]}),
        ),
    );
    let result = f.plan(json!({})).discover().await.unwrap();
    assert!(result["requested_scopes"].is_null());
    assert_eq!(result["scope_source"], "omit");
}
#[tokio::test]
async fn prm_path_then_root_and_oidc_insert_then_append_have_exact_order() {
    let f = Fixture::new().await;
    let prm =
        f.m.lock()
            .unwrap()
            .routes
            .remove("/.well-known/oauth-protected-resource/mcp")
            .unwrap();
    f.set("/.well-known/oauth-protected-resource", prm);
    let server =
        f.m.lock()
            .unwrap()
            .routes
            .remove("/.well-known/oauth-authorization-server/tenant")
            .unwrap();
    f.set("/tenant/.well-known/openid-configuration", server);
    f.plan(json!({})).discover().await.unwrap();
    assert_eq!(
        f.seen()
            .iter()
            .map(|v| v["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "/mcp",
            "/.well-known/oauth-protected-resource/mcp",
            "/.well-known/oauth-protected-resource",
            "/.well-known/oauth-authorization-server/tenant",
            "/.well-known/openid-configuration/tenant",
            "/tenant/.well-known/openid-configuration"
        ]
    );
}
#[tokio::test]
async fn advertised_metadata_has_priority_and_failure_cannot_downgrade_to_well_known() {
    let f = Fixture::new().await;
    f.set(
        "/mcp",
        Reply {
            status: 401,
            challenge: Some(format!("Bearer resource_metadata=\"{}/missing\"", f.base)),
            ..Reply::json(json!({}))
        },
    );
    assert_eq!(
        f.plan(json!({})).discover().await.unwrap_err(),
        "MCP_OAUTH_METADATA_NOT_FOUND"
    );
    assert_eq!(f.seen().len(), 2);
    assert_eq!(f.seen()[1]["path"], "/missing");
}
#[tokio::test]
async fn external_challenge_metadata_is_never_fetched_without_explicit_review() {
    let f = Fixture::new().await;
    f.set(
        "/mcp",
        Reply {
            status: 401,
            challenge: Some("Bearer resource_metadata=\"https://external.example/prm\"".into()),
            ..Reply::json(json!({}))
        },
    );
    assert_eq!(
        f.plan(json!({})).discover().await.unwrap_err(),
        "MCP_OAUTH_EXTERNAL_METADATA_REVIEW_REQUIRED"
    );
    assert_eq!(f.seen().len(), 1);
    assert_eq!(
        f.plan(json!({"metadata_url":"https://reviewed.example/prm"}))
            .discover()
            .await
            .unwrap_err(),
        "MCP_OAUTH_CHALLENGE_METADATA_CHANGED"
    );
    assert_eq!(f.seen().len(), 2);
}
#[tokio::test]
async fn malformed_success_or_mismatched_as_metadata_does_not_fall_through_to_oidc() {
    let f = Fixture::new().await;
    f.set(
        "/.well-known/oauth-authorization-server/tenant",
        Reply::json(as_metadata("https://other.example")),
    );
    assert_eq!(
        f.plan(json!({})).discover().await.unwrap_err(),
        "MCP_OAUTH_ISSUER_MISMATCH"
    );
    assert_eq!(f.seen().len(), 3);
    f.set(
        "/.well-known/oauth-authorization-server/tenant",
        Reply {
            body: "{broken".into(),
            ..Reply::json(json!({}))
        },
    );
    assert_eq!(
        f.plan(json!({})).discover().await.unwrap_err(),
        "MCP_OAUTH_METADATA_JSON"
    );
    assert_eq!(f.seen().len(), 6);
}
#[tokio::test]
async fn redirects_transient_status_content_type_and_body_limits_fail_without_replay() {
    let f = Fixture::new().await;
    for status in [302, 401, 429, 503] {
        f.set(
            "/.well-known/oauth-protected-resource/mcp",
            Reply {
                status,
                location: Some(format!("{}/should-not-fetch", f.base)),
                ..Reply::json(json!({}))
            },
        );
        let before = f.seen().len();
        assert!(f
            .plan(json!({}))
            .discover()
            .await
            .unwrap_err()
            .starts_with("MCP_OAUTH_METADATA_HTTP_"));
        assert_eq!(f.seen().len() - before, 2);
    }
    f.set(
        "/.well-known/oauth-protected-resource/mcp",
        Reply {
            content: "text/html".into(),
            ..Reply::json(json!({}))
        },
    );
    assert_eq!(
        f.plan(json!({})).discover().await.unwrap_err(),
        "MCP_OAUTH_METADATA_CONTENT_TYPE"
    );
    f.set(
        "/.well-known/oauth-protected-resource/mcp",
        Reply {
            body: "x".repeat(LIMIT + 1),
            ..Reply::json(json!({}))
        },
    );
    assert_eq!(
        f.plan(json!({})).discover().await.unwrap_err(),
        "MCP_OAUTH_METADATA_LIMIT"
    );
    assert!(f.seen().iter().all(|v| v["path"] != "/should-not-fetch"));
}
#[tokio::test]
async fn exact_reviewed_metadata_override_is_used_when_challenge_has_no_url() {
    let f = Fixture::new().await;
    let prm =
        f.m.lock()
            .unwrap()
            .routes
            .remove("/.well-known/oauth-protected-resource/mcp")
            .unwrap();
    f.set("/explicit", prm);
    f.plan(json!({"metadata_url":format!("{}/explicit",f.base)}))
        .discover()
        .await
        .unwrap();
    assert_eq!(f.seen()[1]["path"], "/explicit");
}
