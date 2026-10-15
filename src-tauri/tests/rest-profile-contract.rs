// Initial pure contracts. Production REST transport remains unchanged until
// the prepared actor, credential publication and import workflow are wired.
#[path = "../src/rest_credentials.rs"]
mod rest_credentials;
#[path = "../src/rest_openapi.rs"]
mod rest_openapi;
#[path = "../src/rest_profile.rs"]
mod rest_profile;
use rest_profile::*;
use serde_json::{json, Value};
fn fixture() -> Value {
    json!({"profile_version":1,"base_url":"https://api.example.com/v1/","allowed_hosts":["api.example.com"],"read_only":false,"auth":{"type":"bearer","secret_ref":"token"},"headers":{"x-client":{"value":"AlfredPet"}},"operations":{"read":{"method":"GET","path":"/items/{id}","path_schema":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"],"additionalProperties":false},"query_schema":{"type":"object","properties":{"q":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":10}},"additionalProperties":false},"response":"json","response_schema":{"type":"object"},"success_statuses":[200],"risk":"mandatory"},"write":{"method":"POST","path":"/items","path_schema":{"type":"object","properties":{},"additionalProperties":false},"query_schema":{"type":"object","properties":{},"additionalProperties":false},"body_schema":{"type":"object","properties":{"title":{"type":"string","minLength":1}},"required":["title"],"additionalProperties":false},"response":"json","success_statuses":[201],"risk":"free"}}})
}
#[test]
fn rest_profile_closed_legacy_and_host_tls_policy() {
    let original = fixture();
    assert!(config(&original).is_ok());
    for (k, v) in [
        ("profile_version", json!(2)),
        ("base_url", json!("https://user:pass@api.example.com/v1/")),
        ("base_url", json!("http://api.example.com/v1/")),
        ("base_url", json!("https://api.example.com/v1/../admin/")),
        ("base_url", json!("https://foreign.example/v1/")),
        ("base_url", json!("https://api.example.com/v1/?token=x")),
        ("base_url", json!("https://api.example.com/v1/#fragment")),
        ("unknown", json!(true)),
    ] {
        let mut p = original.clone();
        p[k] = v;
        assert!(config(&p).is_err(), "{k}");
    }
    let mut p = original;
    p["base_url"] = json!("http://127.0.0.1:12345/v1/");
    p["allowed_hosts"] = json!(["127.0.0.1"]);
    assert!(config(&p).is_ok());
    assert!(config(&json!({"base_url":"https://api.example.com","operations":[]})).is_err());
}
#[test]
fn rest_profile_unicode_encoding_keeps_exact_prefix_and_reviewed_operation() {
    let c = config(&fixture()).unwrap();
    let p = plan(
        &c,
        &json!({"operation":"read","path":{"id":"Señor 🐾"},"query":{"q":"a&b=secret","limit":2}}),
    )
    .unwrap();
    assert_eq!(p.operation, "read");
    assert_eq!(p.method, "GET");
    assert!(!p.mutation);
    assert!(p.body.is_none());
    assert_eq!(p.risk, Risk::Mandatory);
    assert_eq!(
        p.url.origin().ascii_serialization(),
        "https://api.example.com"
    );
    assert!(p.url.path().starts_with("/v1/items/Se%C3%B1or%20"));
    assert_eq!(
        p.url.query_pairs().find(|(k, _)| k == "q").unwrap().1,
        "a&b=secret"
    );
    for id in [
        "..",
        "../foreign",
        "%2fsecret",
        "a/b",
        "a?token=x",
        "\\evil",
    ] {
        assert!(plan(&c, &json!({"operation":"read","path":{"id":id}})).is_err());
    }
    assert!(plan(
        &c,
        &json!({"operation":"read","path":{"id":"ok"},"endpoint":"/admin","method":"DELETE"})
    )
    .is_err());
}
#[test]
fn rest_profile_configured_risk_cannot_downgrade_write_and_readonly_denies() {
    let mut v = fixture();
    let input = json!({"operation":"write","body":{"title":"owned"}});
    let p = plan(&config(&v).unwrap(), &input).unwrap();
    assert!(p.mutation);
    assert_eq!(p.risk, Risk::Mandatory);
    assert_eq!(p.body, input.get("body").cloned());
    v["read_only"] = json!(true);
    assert_eq!(
        plan(&config(&v).unwrap(), &input).unwrap_err(),
        "REST_READ_ONLY"
    );
    for (risk, expected) in [
        ("free", Risk::Free),
        ("contextual", Risk::Contextual),
        ("mandatory", Risk::Mandatory),
    ] {
        v["operations"]["read"]["risk"] = json!(risk);
        assert_eq!(
            plan(
                &config(&v).unwrap(),
                &json!({"operation":"read","path":{"id":"owned"}})
            )
            .unwrap()
            .risk,
            expected
        );
    }
    v.as_object_mut().unwrap().remove("read_only");
    assert!(config(&v).unwrap().read_only);
}
#[test]
fn rest_profile_header_modes_references_and_conflicts_are_explicit() {
    let mut v = fixture();
    v["headers"]["x-api-key"] = json!({"secret_ref":"key"});
    assert_eq!(
        required_secrets(&config(&v).unwrap())
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        vec!["key", "token"]
    );
    for h in [
        "host",
        "content-length",
        "connection",
        "cookie",
        "proxy-authorization",
        "content-type",
        "X-Key",
    ] {
        let mut p = fixture();
        p["headers"][h] = json!({"value":"owned"});
        assert!(config(&p).is_err(), "{h}");
    }
    for h in [
        json!({"value":"literal_token"}),
        json!({"value":"owned","secret_ref":"key"}),
        json!({"secret_ref":"connection.foreign"}),
        json!({"value":"bad\r\nheader"}),
    ] {
        let mut p = fixture();
        p["headers"]["x-api-key"] = h;
        assert!(config(&p).is_err());
    }
    for auth in [
        json!({"type":"none"}),
        json!({"type":"basic","username":"owned-user","secret_ref":"password"}),
        json!({"type":"header","header":"x-api-key","secret_ref":"key"}),
    ] {
        let mut p = fixture();
        p["auth"] = auth;
        assert!(config(&p).is_ok());
    }
    let mut p = fixture();
    p["headers"]["authorization"] = json!({"secret_ref":"other"});
    assert!(config(&p).is_err());
}
#[test]
fn rest_profile_schema_targets_and_unknown_inputs_fail_before_transport() {
    let c = config(&fixture()).unwrap();
    for input in [
        json!({"operation":"foreign"}),
        json!({"operation":"read","path":{"id":"owned"},"query":{"limit":11}}),
        json!({"operation":"read","path":{"id":"owned","foreign":"x"}}),
        json!({"operation":"read","path":{"id":"owned"},"body":{}}),
        json!({"operation":"write","body":{"title":"owned","foreign":true}}),
        json!({"operation":"write"}),
    ] {
        assert!(plan(&c, &input).is_err());
    }
    for s in [
        json!({"$ref":"http://127.0.0.1:1/private"}),
        json!({"$dynamicRef":"#x"}),
        json!({"type":"object","properties":{"x":{"$ref":"#/defs/x"}}}),
    ] {
        assert!(schema(&s).is_err());
    }
    let mut p = fixture();
    p["operations"]["read"]["path_schema"]["required"] = json!([]);
    assert!(config(&p).is_err());
    let mut p = fixture();
    p["operations"]["read"]["query_schema"]["additionalProperties"] = json!(true);
    assert!(config(&p).is_err());
}
#[test]
fn rest_profile_limits_and_response_contracts_are_explicit() {
    let mut p = fixture();
    p["operations"]["read"]["response"] = json!("text");
    assert!(config(&p).is_err());
    p["operations"]["read"]
        .as_object_mut()
        .unwrap()
        .remove("response_schema");
    assert!(config(&p).is_ok());
    p["operations"]["read"]["method"] = json!("HEAD");
    assert!(config(&p).is_err());
    p["operations"]["read"]["response"] = json!("empty");
    assert!(config(&p).is_ok());
    p["operations"]["read"]["success_statuses"] = json!([302]);
    assert!(config(&p).is_err());
    let mut p = fixture();
    p["openapi_path"] = json!("/openapi/{id}");
    assert!(config(&p).is_err());
    p["openapi_path"] = json!("/openapi.json");
    assert!(config(&p).is_ok());
    let c = config(&fixture()).unwrap();
    assert!(plan(
        &c,
        &json!({"operation":"write","body":{"title":"x".repeat(65536)}})
    )
    .is_err());
}

#[test]
fn rest_profile_explicit_json_null_and_empty_query_remain_distinct_from_absence() {
    let mut v = fixture();
    v["operations"]["write"]["body_schema"] = json!({"type":"null"});
    let c = config(&v).unwrap();
    let p = plan(&c, &json!({"operation":"write","body":null})).unwrap();
    assert_eq!(p.body, Some(Value::Null));
    assert!(plan(&c, &json!({"operation":"write"})).is_err());
    let p = plan(
        &c,
        &json!({"operation":"read","path":{"id":"owned"},"query":{"q":""}}),
    )
    .unwrap();
    assert_eq!(p.url.query(), Some("q="));
    assert!(plan(&c, &json!({"operation":"read","path":{"id":""}})).is_err());
    assert!(plan(
        &c,
        &json!({"operation":"read","path":{"id":"owned"},"risk":"free"})
    )
    .is_err());
}

#[test]
fn rest_profile_declared_formats_are_validated_and_unknown_formats_rejected() {
    let mut v = fixture();
    v["operations"]["read"]["query_schema"]["properties"]["q"] =
        json!({"type":"string","format":"date-time"});
    let c = config(&v).unwrap();
    assert!(plan(
        &c,
        &json!({"operation":"read","path":{"id":"owned"},"query":{"q":"invalid"}})
    )
    .is_err());
    assert!(plan(
        &c,
        &json!({"operation":"read","path":{"id":"owned"},"query":{"q":"2026-10-04T12:00:00Z"}})
    )
    .is_ok());
    assert!(schema(&json!({"type":"string","format":"owned_unknown_format"})).is_err());
    assert!(validator(&json!({"type":"integer","minimum":1}))
        .unwrap()
        .is_valid(&json!(1)));
}

#[test]
fn rest_profile_optional_body_and_status_specific_responses_preserve_contracts() {
    let mut v = fixture();
    v["operations"]["write"]["body_required"] = json!(false);
    v["operations"]["write"]["request_content_type"] = json!("application/vnd.owned+json");
    v["operations"]["write"]["success_statuses"] = json!([201, 204]);
    v["operations"]["write"]["responses"] = json!({"201":{"response":"json","schema":{"type":"object","required":["id"]},"content_type":"application/json"},"204":{"response":"empty"}});
    let c = config(&v).unwrap();
    assert!(plan(&c, &json!({"operation":"write"}))
        .unwrap()
        .body
        .is_none());
    let op = &c.operations["write"];
    assert_eq!(
        response_spec(op, 201).unwrap().schema.unwrap()["required"],
        json!(["id"])
    );
    assert_eq!(
        response_spec(op, 204).unwrap().response,
        ResponseKind::Empty
    );
    assert!(response_spec(op, 200).is_err());
    v["operations"]["write"]["responses"]
        .as_object_mut()
        .unwrap()
        .remove("204");
    assert!(config(&v).is_err());
    assert!(!json_media("application/json;bad=header"));
}

fn api_document() -> Value {
    json!({"openapi":"3.1.1","info":{"title":"Owned API","version":"1"},"servers":[{"url":"https://api.example.com/v1"}],"components":{"parameters":{"ItemId":{"in":"path","name":"id","required":true,"schema":{"type":"string"}}},"schemas":{"Item":{"type":"object","properties":{"id":{"type":"string","readOnly":true},"title":{"type":"string","minLength":1},"pin":{"type":"string","writeOnly":true}},"required":["id","title","pin"],"additionalProperties":false}},"responses":{"Created":{"description":"Owned JSON","content":{"application/json":{"schema":{"$ref":"#/components/schemas/Item"}}}}}},"paths":{"/items/{id}":{"parameters":[{"$ref":"#/components/parameters/ItemId"}],"get":{"operationId":"getItem","responses":{"200":{"$ref":"#/components/responses/Created"}}}},"/items":{"post":{"operationId":"createItem","requestBody":{"content":{"application/json":{"schema":{"$ref":"#/components/schemas/Item"}}}},"responses":{"201":{"$ref":"#/components/responses/Created"},"204":{"description":"No content"}}}}}})
}
#[test]
fn rest_openapi_local_refs_optional_body_read_write_projection_and_selected_proposals() {
    let current = config(&fixture()).unwrap();
    let p = rest_openapi::preview(&api_document(), &current, None).unwrap();
    assert_eq!(p.version, "3.1.1");
    assert_eq!(p.operations_seen, 2);
    assert_eq!(p.operations_proposed, 2);
    assert!(p.diagnostics.is_empty());
    assert!(!p.automatically_enabled);
    assert!(p.profile.read_only);
    assert!(rest_openapi::select(&p, &[]).is_err());
    assert!(rest_openapi::select(&p, &["foreign".into()]).is_err());
    let mut c = rest_openapi::select(&p, &["createItem".into()]).unwrap();
    assert_eq!(c.operations.len(), 1);
    c.read_only = false;
    let action = plan(
        &c,
        &json!({"operation":"createItem","body":{"title":"Owned","pin":"fictional"}}),
    )
    .unwrap();
    assert_eq!(action.operation, "createItem");
    assert_eq!(action.method, "POST");
    assert_eq!(action.url.as_str(), "https://api.example.com/v1/items");
    assert!(action.mutation);
    assert_eq!(action.risk, Risk::Mandatory);
    assert!(action.body.is_some());
    assert!(plan(&c, &json!({"operation":"createItem"}))
        .unwrap()
        .body
        .is_none());
    assert!(plan(&c,&json!({"operation":"createItem","body":{"id":"implicit-server-id","title":"Owned","pin":"fictional"}})).is_err());
    let response = response_spec(&c.operations["createItem"], 201).unwrap();
    assert!(validator(&response.schema.clone().unwrap())
        .unwrap()
        .is_valid(&json!({"id":"owned","title":"Owned"})));
    assert!(!validator(&response.schema.unwrap())
        .unwrap()
        .is_valid(&json!({"id":"owned","title":"Owned","pin":"should-never-output"})));
    assert_eq!(
        response_spec(&c.operations["createItem"], 204)
            .unwrap()
            .response,
        ResponseKind::Empty
    );
}
#[test]
fn rest_openapi_legacy_nullable_exclusive_bound_and_numeric_format_preserved() {
    let mut d = api_document();
    d["openapi"] = json!("3.0.4");
    d["components"]["schemas"]["Item"] = json!({"type":"object","nullable":true,"properties":{"n":{"type":"number","minimum":1,"exclusiveMinimum":true},"i":{"type":"integer","format":"int32"}},"required":["n","i"],"additionalProperties":false});
    let mut c = rest_openapi::preview(&d, &config(&fixture()).unwrap(), None)
        .unwrap()
        .profile;
    c.read_only = false;
    assert!(plan(&c, &json!({"operation":"createItem","body":null})).is_ok());
    assert!(plan(&c, &json!({"operation":"createItem","body":{"n":1,"i":1}})).is_err());
    assert!(plan(
        &c,
        &json!({"operation":"createItem","body":{"n":1.5,"i":2147483648u64}})
    )
    .is_err());
    assert!(plan(
        &c,
        &json!({"operation":"createItem","body":{"n":1.5,"i":2147483647}})
    )
    .is_ok());
    d["components"]["schemas"]["Item"]["enum"] = json!([{"n":1.5,"i":1}]);
    let mut c = rest_openapi::preview(&d, &config(&fixture()).unwrap(), None)
        .unwrap()
        .profile;
    c.read_only = false;
    assert!(plan(&c, &json!({"operation":"createItem","body":null})).is_err());
}
#[test]
fn rest_openapi_schema_reference_siblings_are_constraints_and_legacy_is_explicit() {
    let mut d = api_document();
    d["components"]["schemas"]["Text"] = json!({"type":"string"});
    d["components"]["schemas"]["Item"] = json!({"type":"object","properties":{"title":{"$ref":"#/components/schemas/Text","minLength":3}},"required":["title"],"additionalProperties":false});
    let mut c = rest_openapi::preview(&d, &config(&fixture()).unwrap(), None)
        .unwrap()
        .profile;
    c.read_only = false;
    assert!(plan(&c, &json!({"operation":"createItem","body":{"title":"ab"}})).is_err());
    assert!(plan(
        &c,
        &json!({"operation":"createItem","body":{"title":"owned"}})
    )
    .is_ok());
    d["openapi"] = json!("3.0.4");
    let p = rest_openapi::preview(&d, &config(&fixture()).unwrap(), None).unwrap();
    assert_eq!(p.operations_proposed, 0);
    assert!(p
        .diagnostics
        .iter()
        .all(|d| d.code == "OPENAPI_REFERENCE_SIBLING_UNSUPPORTED"));
}
#[test]
fn rest_openapi_external_missing_recursive_refs_and_unknown_versions_are_diagnosed() {
    for reference in [
        "http://127.0.0.1:1/never-fetch",
        "#/components/schemas/Missing",
        "#anchor",
    ] {
        let mut d = api_document();
        d["components"]["schemas"]["Item"] = json!({"$ref":reference});
        let p = rest_openapi::preview(&d, &config(&fixture()).unwrap(), None).unwrap();
        assert_eq!(p.operations_proposed, 0);
        assert_eq!(p.diagnostics.len(), 2);
    }
    let mut d = api_document();
    d["components"]["schemas"]["Item"] =
        json!({"type":"object","properties":{"child":{"$ref":"#/components/schemas/Item"}}});
    let p = rest_openapi::preview(&d, &config(&fixture()).unwrap(), None).unwrap();
    assert!(p
        .diagnostics
        .iter()
        .all(|d| d.code == "OPENAPI_REFERENCE_CYCLE"));
    d["openapi"] = json!("4.0.0");
    assert_eq!(
        rest_openapi::preview(&d, &config(&fixture()).unwrap(), None).unwrap_err(),
        "OPENAPI_VERSION_UNSUPPORTED"
    );
}
#[test]
fn rest_openapi_server_and_auth_declarations_never_expand_authority() {
    let c = config(&fixture()).unwrap();
    let mut d = api_document();
    d["servers"] = json!([{"url":"https://foreign.example/v1"}]);
    let p = rest_openapi::preview(&d, &c, None).unwrap();
    assert_eq!(p.operations_proposed, 0);
    assert!(p
        .diagnostics
        .iter()
        .all(|d| d.code == "OPENAPI_SERVER_OUT_OF_PROFILE_OR_UNRESOLVED"));
    d["servers"] = json!([{"url":"./"}]);
    assert_eq!(
        rest_openapi::preview(&d, &c, None)
            .unwrap()
            .operations_proposed,
        0
    );
    let source = reqwest::Url::parse("https://api.example.com/v1/openapi.json").unwrap();
    assert_eq!(
        rest_openapi::preview(&d, &c, Some(&source))
            .unwrap()
            .operations_proposed,
        2
    );
    let foreign = reqwest::Url::parse("https://foreign.example/v1/openapi.json").unwrap();
    assert!(rest_openapi::preview(&d, &c, Some(&foreign)).is_err());
    d["servers"] = json!([{"url":"/v1/"}]);
    d["components"]["securitySchemes"]["Bearer"] = json!({"type":"http","scheme":"bearer"});
    d["security"] = json!([{"Bearer":[]}]);
    assert_eq!(
        rest_openapi::preview(&d, &c, None)
            .unwrap()
            .operations_proposed,
        2
    );
    let mut c = c;
    c.auth = Auth::None;
    assert_eq!(
        rest_openapi::preview(&d, &c, None)
            .unwrap()
            .operations_proposed,
        0
    );
    d["paths"]["/items"]["post"]["security"] = json!([]);
    let p = rest_openapi::preview(&d, &c, None).unwrap();
    assert_eq!(p.operations_proposed, 1);
    assert_eq!(p.profile.operations.len(), 1);
}
#[test]
fn rest_openapi_operation_parameters_override_base_and_response_ranges_remain_per_status() {
    let mut d = api_document();
    d["paths"]["/items/{id}"]["parameters"]
        .as_array_mut()
        .unwrap()
        .push(json!({"in":"query","name":"limit","schema":{"type":"integer","maximum":2}}));
    d["paths"]["/items/{id}"]["get"]["parameters"] =
        json!([{"in":"query","name":"limit","schema":{"type":"integer","maximum":5}}]);
    d["paths"]["/items"]["post"]["responses"] =
        json!({"2XX":{"$ref":"#/components/responses/Created"},"204":{"description":"empty"}});
    let p = rest_openapi::preview(&d, &config(&fixture()).unwrap(), None).unwrap();
    assert!(plan(
        &p.profile,
        &json!({"operation":"getItem","path":{"id":"owned"},"query":{"limit":5}})
    )
    .is_ok());
    assert!(plan(
        &p.profile,
        &json!({"operation":"getItem","path":{"id":"owned"},"query":{"limit":6}})
    )
    .is_err());
    let op = &p.profile.operations["createItem"];
    assert_eq!(op.success_statuses.len(), 100);
    assert_eq!(
        response_spec(op, 204).unwrap().response,
        ResponseKind::Empty
    );
    assert_eq!(response_spec(op, 226).unwrap().response, ResponseKind::Json);
    d["paths"]["/items/{id}"]["get"]["operationId"] = json!("createItem");
    assert_eq!(
        rest_openapi::preview(&d, &config(&fixture()).unwrap(), None).unwrap_err(),
        "OPENAPI_OPERATION_ID_DUPLICATE"
    );
}
#[test]
fn rest_openapi_unsupported_serialization_is_visible_without_silent_partial_acceptance() {
    let mut d = api_document();
    d["paths"]["/items/{id}"]["get"]["parameters"] =
        json!([{"in":"query","name":"ids","schema":{"type":"array","items":{"type":"string"}}}]);
    let p = rest_openapi::preview(&d, &config(&fixture()).unwrap(), None).unwrap();
    assert_eq!(p.operations_seen, 2);
    assert_eq!(p.operations_proposed, 1);
    assert_eq!(p.diagnostics[0].method, "GET");
    assert_eq!(p.diagnostics[0].path, "/items/{id}");
    assert_eq!(p.diagnostics[0].code, "REST_PARAMETER_PRIMITIVE_REQUIRED");
    let mut d = api_document();
    d["paths"]["/items"]["post"]["requestBody"]["content"] =
        json!({"multipart/form-data":{"schema":{"type":"object"}}});
    let p = rest_openapi::preview(&d, &config(&fixture()).unwrap(), None).unwrap();
    assert_eq!(p.operations_proposed, 1);
    assert_eq!(p.diagnostics[0].code, "OPENAPI_MEDIA_UNSUPPORTED");
}

#[test]
fn rest_openapi_32_query_and_additional_methods_stay_declared_and_mandatory() {
    let mut d = api_document();
    d["openapi"] = json!("3.2.1");
    d["paths"]["/items"]["query"] = json!({"operationId":"queryItems","requestBody":{"required":true,"content":{"application/json":{"schema":{"type":"object","properties":{"term":{"type":"string"}},"required":["term"],"additionalProperties":false}}}},"responses":{"200":{"description":"owned","content":{"application/json":{"schema":{"type":"array","items":{"type":"string"}}}}}}});
    d["paths"]["/items"]["additionalOperations"] = json!({"COPY":{"operationId":"copyItems","responses":{"204":{"description":"empty"}}},"CONNECT":{"operationId":"neverTunnel","responses":{"200":{"description":"empty"}}}});
    let p = rest_openapi::preview(&d, &config(&fixture()).unwrap(), None).unwrap();
    assert_eq!(p.operations_seen, 5);
    assert_eq!(p.operations_proposed, 4);
    assert_eq!(p.diagnostics[0].method, "CONNECT");
    let mut c = p.profile;
    c.read_only = false;
    let q = plan(
        &c,
        &json!({"operation":"queryItems","body":{"term":"owned"}}),
    )
    .unwrap();
    assert_eq!(q.method, "QUERY");
    assert_eq!(q.risk, Risk::Mandatory);
    assert_eq!(
        plan(&c, &json!({"operation":"copyItems"})).unwrap().method,
        "COPY"
    );
    assert!(plan(&c, &json!({"operation":"copyItems","method":"CONNECT"})).is_err());
    d["paths"]["/items"]["additionalOperations"]["POST"] =
        json!({"responses":{"204":{"description":"bad duplicate"}}});
    assert_eq!(
        rest_openapi::preview(&d, &config(&fixture()).unwrap(), None).unwrap_err(),
        "OPENAPI_ADDITIONAL_METHOD_CONFLICT"
    );
}
#[test]
fn rest_openapi_forbidden_projection_still_validates_reference_semantics() {
    let mut d = api_document();
    d["openapi"] = json!("3.0.4");
    d["components"]["schemas"]["Text"] = json!({"type":"string"});
    d["components"]["schemas"]["Item"] = json!({"type":"object","properties":{"id":{"$ref":"#/components/schemas/Text","readOnly":true}},"required":["id"]});
    let p = rest_openapi::preview(&d, &config(&fixture()).unwrap(), None).unwrap();
    assert_eq!(p.operations_proposed, 0);
    assert!(p
        .diagnostics
        .iter()
        .all(|d| d.code == "OPENAPI_REFERENCE_SIBLING_UNSUPPORTED"));
}

#[test]
fn rest_credentials_bind_owned_subject_exact_prefix_auth_refs_and_generation() {
    let id = uuid::Uuid::new_v4().to_string();
    let c = config(&fixture()).unwrap();
    let p = rest_credentials::publication(&id, &c, r#"{"token":"owned_fictional_rest_token"}"#)
        .unwrap();
    assert_eq!(
        rest_credentials::alias(&id).unwrap(),
        format!("rest.credentials.{id}")
    );
    assert_eq!(
        rest_credentials::read(&id, &c, &p.ready, Some(&p.private_json)).unwrap()["token"],
        "owned_fictional_rest_token"
    );
    assert!(rest_credentials::read(&id, &c, &p.pending, Some(&p.private_json)).is_err());
    assert!(!json!(p.ready)
        .to_string()
        .contains("owned_fictional_rest_token"));
    for field in [
        "generation",
        "connection",
        "target_sha256",
        "kind",
        "version",
    ] {
        let mut v: Value = serde_json::from_str(&p.private_json).unwrap();
        v[field] = json!("foreign");
        assert!(
            rest_credentials::read(&id, &c, &p.ready, Some(&v.to_string())).is_err(),
            "{field}"
        );
    }
    let mut changed = c.clone();
    changed.base_url = "https://api.example.com/v2/".into();
    assert!(rest_credentials::read(&id, &changed, &p.ready, Some(&p.private_json)).is_err());
    changed = c.clone();
    changed.auth = Auth::Header {
        header: "x-api-key".into(),
        secret_ref: "token".into(),
    };
    assert!(rest_credentials::read(&id, &changed, &p.ready, Some(&p.private_json)).is_err());
    changed = c;
    changed.headers.insert(
        "x-new-key".into(),
        Header {
            value: None,
            secret_ref: Some("token".into()),
        },
    );
    assert!(rest_credentials::read(&id, &changed, &p.ready, Some(&p.private_json)).is_err());
}
#[test]
fn rest_credentials_unconfigured_no_auth_and_exact_named_secret_map_are_distinct() {
    let id = uuid::Uuid::new_v4().to_string();
    let mut c = config(&fixture()).unwrap();
    let b = rest_credentials::unconfigured(&id, &c).unwrap();
    assert!(rest_credentials::read(&id, &c, &b, None).is_err());
    c.auth = Auth::None;
    let b = rest_credentials::unconfigured(&id, &c).unwrap();
    assert!(rest_credentials::read(&id, &c, &b, None)
        .unwrap()
        .is_empty());
    assert!(rest_credentials::publication(&id, &c, "{}").is_err());
    c.auth = Auth::Bearer {
        secret_ref: "token".into(),
    };
    for raw in [
        r#"{"token":"owned","foreign":"x"}"#,
        r#"{"foreign":"x"}"#,
        r#"{"token":3}"#,
        "owned_legacy_scalar",
        r#"{"token":"bad token with spaces"}"#,
    ] {
        assert!(rest_credentials::publication(&id, &c, raw).is_err());
    }
    c.auth = Auth::Basic {
        username: "owned-user".into(),
        secret_ref: "password".into(),
    };
    let p = rest_credentials::publication(&id, &c, r#"{"password":""}"#).unwrap();
    assert_eq!(
        rest_credentials::read(&id, &c, &p.ready, Some(&p.private_json)).unwrap()["password"],
        ""
    );
}
#[test]
fn rest_credentials_private_capacity_closed_shapes_and_fresh_rotation_fail_closed() {
    let id = uuid::Uuid::new_v4().to_string();
    let c = config(&fixture()).unwrap();
    assert!(
        rest_credentials::publication(&id, &c, &json!({"token":"x".repeat(1280)}).to_string())
            .is_err()
    );
    let p = rest_credentials::publication(&id, &c, r#"{"token":"owned"}"#).unwrap();
    let q = rest_credentials::publication(&id, &c, r#"{"token":"owned"}"#).unwrap();
    assert_ne!(p.ready.generation, q.ready.generation);
    assert!(rest_credentials::read(&id, &c, &q.ready, Some(&p.private_json)).is_err());
    let mut private: Value = serde_json::from_str(&p.private_json).unwrap();
    private["unknown"] = json!(true);
    assert!(rest_credentials::read(&id, &c, &p.ready, Some(&private.to_string())).is_err());
    assert!(rest_credentials::read(&id, &c, &p.ready, Some("owned_legacy_scalar")).is_err());
    let mut public = json!(p.ready);
    public["unknown"] = json!(true);
    assert!(rest_credentials::public_binding(&public).is_err());
    public.as_object_mut().unwrap().remove("unknown");
    let mut b = rest_credentials::public_binding(&public).unwrap();
    b.generation = None;
    assert!(rest_credentials::validate_binding(&id, &c, &b).is_err());
}
