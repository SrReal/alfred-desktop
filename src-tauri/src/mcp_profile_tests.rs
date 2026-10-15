use super::*;
fn http() -> Value {
    json!({"profile_version":1,"transport":"streamable_http","protocol":"auto","base_url":"https://mcp.example","endpoint":"/mcp","auth":{"type":"none"}})
}
#[test]
fn oauth_profile_pins_a_closed_https_issuer_and_has_no_inline_credential() {
    let mut raw = http();
    raw["auth"] = json!({"type":"oauth","issuer":"https://issuer.example/tenant"});
    let c = config(&raw).unwrap();
    assert!(required_secrets(&c).is_empty());
    for auth in [
        json!({"type":"oauth","issuer":"http://127.0.0.1:4000"}),
        json!({"type":"oauth","issuer":"https://issuer.example?client=secret"}),
        json!({"type":"oauth","issuer":"https://issuer.example","client_secret":"inline"}),
        json!({"type":"oauth","issuer":"https://user:pass@issuer.example"}),
    ] {
        raw["auth"] = auth;
        assert!(config(&raw).is_err());
    }
    let raw = json!({"profile_version":1,"transport":"stdio","command":"C:\\owned\\node.exe","auth":{"type":"oauth","issuer":"https://issuer.example"}});
    assert!(config(&raw).is_err());
}
#[test]
fn closed_fields_and_fixed_http_origin() {
    assert!(config(&http()).is_ok());
    for (key, v) in [
        ("profile_version", json!(2)),
        ("secret", json!("private")),
        ("base_url", json!("http://mcp.example")),
        ("base_url", json!("https://user:pass@mcp.example")),
        ("base_url", json!("https://mcp.example/path")),
        ("endpoint", json!("//other.example/mcp")),
        ("endpoint", json!("/mcp?token=private")),
        ("endpoint", json!("/a/../mcp")),
        ("endpoint", json!("/a/%2e%2e/mcp")),
        ("command", json!("node")),
        ("env", json!({"KEY":"main"})),
        ("auth", json!({"type":"oauth"})),
        ("permissions", json!({"risk":"free"})),
    ] {
        let mut raw = http();
        raw[key] = v;
        assert!(config(&raw).is_err(), "{key}");
    }
    for base in [
        "http://127.0.0.1:54321",
        "http://[::1]:54321",
        "http://localhost:54321",
    ] {
        let mut raw = http();
        raw["base_url"] = json!(base);
        assert!(config(&raw).is_ok(), "{base}");
    }
}
#[test]
fn private_refs_and_reserved_headers_are_closed() {
    for headers in [
        json!({"authorization":{"secret_ref":"key"}}),
        json!({"mcp-name":{"value":"tool"}}),
        json!({"x-api-key":{"value":"plain"}}),
        json!({"X-Key":{"value":"public"}}),
        json!({"x-key":{"value":"public","secret_ref":"key"}}),
        json!({"x-key":{"secret_ref":"../other"}}),
    ] {
        let mut raw = http();
        raw["headers"] = headers;
        assert!(config(&raw).is_err());
    }
    let mut raw = http();
    raw["auth"] = json!({"type":"bearer","secret_ref":"main"});
    raw["headers"] = json!({"x-api-key":{"secret_ref":"key"},"x-client":{"value":"AlfredPet"}});
    assert_eq!(
        required_secrets(&config(&raw).unwrap()),
        BTreeSet::from(["main".into(), "key".into()])
    );
}
#[test]
fn stdio_requires_absolute_command_and_private_environment_references() {
    let mut raw = json!({"profile_version":1,"transport":"stdio","command":"C:\\owned\\node.exe","args":[],"auth":{"type":"none"},"env":{"OWNED_KEY":"main"}});
    assert!(config(&raw).is_ok());
    for env in [
        json!({"PATH":"key"}),
        json!({"HTTP_PROXY":"key"}),
        json!({"NODE_OPTIONS":"key"}),
        json!({"key":"main"}),
        json!({"KEY":"private bytes!"}),
    ] {
        raw["env"] = env;
        assert!(config(&raw).is_err());
    }
    raw["env"] = json!({});
    raw["command"] = json!("node");
    assert!(config(&raw).is_err());
}
#[test]
fn permissions_default_deny_and_do_not_follow_remote_hints() {
    let mut raw = http();
    let c = config(&raw).unwrap();
    for action in ["call", "read_resource", "get_prompt"] {
        assert_eq!(
            allowed(&c, action, &json!({"name":"test","uri":"owned:item"})).unwrap_err(),
            "MCP_SUBJECT_NOT_ALLOWED"
        );
    }
    raw["permissions"] = json!({"tools":{"test":true,"denied":false},"resources":["owned:item"],"prompts":{"test":true}});
    let c = config(&raw).unwrap();
    assert!(allowed(&c, "call", &json!({"name":"test"})).is_ok());
    assert!(allowed(&c, "call", &json!({"name":"denied"})).is_err());
    assert!(allowed(&c, "read_resource", &json!({"uri":"owned:item/child"})).is_err());
    raw["enabled"] = json!(false);
    assert_eq!(
        allowed(&config(&raw).unwrap(), "health", &json!({})).unwrap_err(),
        "MCP_CONNECTION_DISABLED"
    );
}
