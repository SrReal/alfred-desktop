use super::*;
#[test]
fn modern_metadata_is_per_request_and_does_not_advertise_unimplemented_capabilities() {
    let r=modern_request(&json!({"jsonrpc":"2.0","id":"owned","method":"tools/list","params":{"cursor":"Señor 🐾","_meta":{"io.modelcontextprotocol/clientCapabilities":{"roots":{}}}}})).unwrap();
    assert_eq!(r["params"]["cursor"], "Señor 🐾");
    assert_eq!(r["params"]["_meta"][META_VERSION], MODERN);
    assert_eq!(
        r["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"],
        json!({})
    );
    assert_eq!(headers(&r, &[]).unwrap()["mcp-protocol-version"], MODERN);
    assert_eq!(headers(&r, &[]).unwrap()["mcp-method"], "tools/list");
    assert!(discover(&Value::Null).is_err());
}
#[test]
fn rpc_response_rejects_wrong_scope_dual_envelopes_and_server_requests() {
    let id = json!(2);
    for v in [
        json!([]),
        json!({"jsonrpc":"2.0","id":3,"result":{}}),
        json!({"jsonrpc":"2.0","id":2,"result":{},"error":{"code":-1,"message":"private"}}),
        json!({"jsonrpc":"2.0","id":2,"method":"sampling/createMessage","result":{}}),
        json!({"jsonrpc":"2.0","id":2,"result":null}),
        json!({"jsonrpc":"2.0","id":2,"error":{"code":"-1","message":"private"}}),
    ] {
        assert!(reply(&v, &id).is_err());
    }
    assert!(matches!(
        reply(
            &json!({"jsonrpc":"2.0","id":2,"error":{"code":-32022,"message":"owned"}}),
            &id
        )
        .unwrap(),
        Reply::Error(-32022)
    ));
}
#[test]
fn complete_input_required_and_legacy_absent_discriminator_are_distinct() {
    let id = json!("owned");
    let envelope = |r: Value| json!({"jsonrpc":"2.0","id":id,"result":r});
    assert!(complete(&envelope(json!({})), &id, false).is_ok());
    assert_eq!(
        complete(&envelope(json!({})), &id, true).unwrap_err(),
        "MCP_RESULT_TYPE"
    );
    assert_eq!(
        complete(
            &envelope(json!({"resultType":"input_required","requestState":"private state"})),
            &id,
            true
        )
        .unwrap_err(),
        "MCP_INPUT_REQUIRED"
    );
    assert_eq!(
        complete(
            &envelope(json!({"resultType":"complete","isError":true})),
            &id,
            true
        )
        .unwrap_err(),
        "MCP_TOOL_ERROR"
    );
    assert!(complete(
        &envelope(json!({"resultType":"complete","content":[]})),
        &id,
        true
    )
    .is_ok());
}
#[test]
fn discovery_never_downgrades_recognized_modern_errors_or_incompatible_versions() {
    let id = json!("d");
    for code in [-32020, -32021, -32022] {
        assert!(modern_error(code));
        assert_eq!(
            discovery(
                &json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":"private"}}),
                &id
            )
            .unwrap_err(),
            "MCP_MODERN_PROTOCOL_ERROR"
        );
    }
    let r = json!({"jsonrpc":"2.0","id":id,"result":{"resultType":"complete","supportedVersions":[MODERN],"capabilities":{"tools":{}},"_meta":{"io.modelcontextprotocol/serverInfo":{"name":"not authority"}}}});
    assert!(discovery(&r, &id).is_ok());
    let mut wrong = r.clone();
    wrong["result"]["supportedVersions"] = json!(["2025-11-25"]);
    assert_eq!(
        discovery(&wrong, &id).unwrap_err(),
        "MCP_PROTOCOL_UNSUPPORTED"
    );
}
#[test]
fn mirrored_headers_encode_unicode_controls_whitespace_and_literal_sentinels() {
    assert_eq!(encoded_header("us-west1").unwrap(), "us-west1");
    for value in [
        "Señor 🐾",
        " padded ",
        "line1\nline2",
        "\0private",
        "=?base64?literal?=",
    ] {
        let encoded = encoded_header(value).unwrap();
        assert!(encoded.starts_with("=?base64?"));
        let raw = encoded
            .strip_prefix("=?base64?")
            .unwrap()
            .strip_suffix("?=")
            .unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(raw)
                .unwrap(),
            value.as_bytes()
        );
    }
    let r=modern_request(&json!({"jsonrpc":"2.0","id":2,"method":"resources/read","params":{"uri":"file:///Señor 🐾.txt"}})).unwrap();
    assert_eq!(
        headers(&r, &[]).unwrap()["mcp-name"],
        encoded_header("file:///Señor 🐾.txt").unwrap()
    );
}
#[test]
fn header_annotations_are_unique_primitive_static_property_paths() {
    let good = json!({"type":"object","properties":{"nested":{"type":"object","properties":{"region":{"type":"string","x-mcp-header":"Region"}}}},"examples":[{"x-mcp-header":"literal data"}]});
    let f = fields(&good).unwrap();
    assert_eq!(f[0].path, vec!["nested", "region"]);
    for bad in [
        json!({"type":"string","x-mcp-header":"Root"}),
        json!({"properties":{"p":{"type":"number","x-mcp-header":"N"}}}),
        json!({"properties":{"p":{"type":"string","x-mcp-header":"bad\nname"}}}),
        json!({"allOf":[{"properties":{"p":{"type":"string","x-mcp-header":"P"}}}]}),
        json!({"$defs":{"p":{"type":"string","x-mcp-header":"P"}}}),
        json!({"properties":{"a":{"type":"string","x-mcp-header":"P"},"b":{"type":"boolean","x-mcp-header":"p"}}}),
    ] {
        assert!(fields(&bad).is_err(), "invalid annotation accepted");
    }
}
#[test]
fn mirrored_values_keep_optional_null_boolean_and_safe_integer_semantics() {
    let schema = json!({"properties":{"p":{"type":"integer","x-mcp-header":"Count"},"b":{"type":"boolean","x-mcp-header":"Flag"},"s":{"type":"string","x-mcp-header":"Text"}}});
    let f = fields(&schema).unwrap();
    let r=modern_request(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"Señor tool","arguments":{"p":9007199254740991_i64,"b":false,"s":null}}})).unwrap();
    let h = headers(&r, &f).unwrap();
    assert_eq!(h["mcp-param-count"], "9007199254740991");
    assert_eq!(h["mcp-param-flag"], "false");
    assert!(!h.contains_key("mcp-param-text"));
    let mut bad = r.clone();
    bad["params"]["arguments"]["p"] = json!(9007199254740992_u64);
    assert!(headers(&bad, &f).is_err());
    bad["params"]["arguments"]["p"] = json!(1.5);
    assert!(headers(&bad, &f).is_err());
    assert!(h.values().all(reqwest::header::HeaderValue::is_sensitive));
}
