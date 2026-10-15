use super::*;
use serde_json::json;

#[test]
fn schemas_use_local_definitions_without_network_or_file_retrieval() {
    let schema = json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","$defs":{"code":{"type":"string","pattern":"^[A-Z]{2}$"}},"properties":{"code":{"$ref":"#/$defs/code"}},"required":["code"],"additionalProperties":false});
    let v = validator(&schema).unwrap();
    assert!(v.is_valid(&json!({"code":"ES"})));
    assert!(!v.is_valid(&json!({"code":"Es","extra":1})));
    for uri in ["https://127.0.0.1:1/secret", "file:///C:/private.json"] {
        assert!(validator(&json!({"type":"object","properties":{"value":{"$ref":uri}}})).is_err());
    }
    assert_eq!(
        validator(&json!({"type":"object","$schema":"https://127.0.0.1:1/schema"}))
            .err()
            .unwrap(),
        "MCP_SCHEMA_DIALECT_UNSUPPORTED"
    );
}
#[test]
fn input_and_structured_output_are_validated_separately() {
    let t = Tool::new(&json!({"name":"observed","inputSchema":{"type":"object","properties":{"n":{"type":"integer"}},"required":["n"],"additionalProperties":false},"outputSchema":{"type":"object","properties":{"done":{"const":true}},"required":["done"]}}), true).unwrap();
    assert!(t.arguments(&json!({"n":1})).is_ok());
    for args in [json!({"n":"1"}), json!({"n":1,"extra":2}), json!(null)] {
        assert_eq!(t.arguments(&args).unwrap_err(), "MCP_TOOL_INPUT_SCHEMA");
    }
    assert!(t
        .output(&json!({"content":[],"structuredContent":{"done":true}}))
        .is_ok());
    assert_eq!(
        t.output(&json!({"content":[]})).unwrap_err(),
        "MCP_TOOL_OUTPUT_SCHEMA"
    );
    assert_eq!(
        t.output(&json!({"content":[],"structuredContent":{"done":false}}))
            .unwrap_err(),
        "MCP_TOOL_OUTPUT_SCHEMA"
    );
    assert_eq!(
        t.output(&json!({"structuredContent":{"done":true}}))
            .unwrap_err(),
        "MCP_TOOL_CONTENT"
    );
}
#[test]
fn header_validation_is_transport_specific_and_does_not_follow_refs() {
    let t = json!({"name":"tool","inputSchema":{"type":"object","$defs":{"v":{"type":"string","x-mcp-header":"Tenant"}},"properties":{"v":{"$ref":"#/$defs/v"}}}});
    assert!(Tool::new(&t, false).is_ok());
    assert_eq!(Tool::new(&t, true).err().unwrap(), "MCP_HEADER_ANNOTATION");
    assert!(Tool::new(
        &json!({"name":"tool","inputSchema":{"type":"array"}}),
        false
    )
    .is_err());
}
#[test]
fn bounded_instances_and_schema_depth_prevent_unbounded_validation() {
    let mut v = json!(null);
    for _ in 0..40 {
        v = json!({"child":v});
    }
    assert_eq!(bounded(&v, 65536).unwrap_err(), "MCP_VALUE_LIMIT");
    assert_eq!(
        bounded(&json!("x".repeat(65537)), 65536).unwrap_err(),
        "MCP_VALUE_LIMIT"
    );
    assert!(validator(&json!({"type":"object","properties":{"name":{"pattern":"["}}})).is_err());
}
