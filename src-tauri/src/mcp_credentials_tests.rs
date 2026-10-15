use super::*;
const ID: &str = "11111111-1111-4111-8111-111111111111";
fn config() -> Config {
    mcp_profile::config(&json!({"profile_version":1,"transport":"streamable_http","base_url":"https://mcp.example","endpoint":"/mcp","auth":{"type":"bearer","secret_ref":"main"},"headers":{"x-api-key":{"secret_ref":"key"}}})).unwrap()
}
#[test]
fn envelope_requires_exact_destination_generation_and_reference_set() {
    let c = config();
    let p = publication(
        ID,
        &c,
        r#"{"main":"owned_fictional_token","key":"owned_fictional_key"}"#,
    )
    .unwrap();
    assert!(read(ID, &c, &p.pending, Some(&p.private_json)).is_err());
    assert_eq!(
        read(ID, &c, &p.ready, Some(&p.private_json)).unwrap()["main"],
        "owned_fictional_token"
    );
    let mut changed = c.clone();
    changed.endpoint = Some("/other".into());
    assert!(read(ID, &changed, &p.ready, Some(&p.private_json)).is_err());
    let other = publication(ID, &c, r#"{"main":"new_owned","key":"new_key"}"#).unwrap();
    assert!(read(ID, &c, &other.ready, Some(&p.private_json)).is_err());
    assert!(read(ID, &c, &p.ready, Some("old scalar")).is_err());
    for raw in [
        r#"{"main":"only"}"#,
        r#"{"main":"ok","key":"ok","extra":"bad"}"#,
        r#"{"main":"a b","key":"ok"}"#,
    ] {
        assert!(publication(ID, &c, raw).is_err());
    }
    assert!(!json!(p.ready).to_string().contains("owned_fictional"));
}
#[test]
fn header_debug_is_sensitive_and_permissions_do_not_retarget_native_secrets() {
    let c = config();
    let p = publication(
        ID,
        &c,
        r#"{"main":"owned_fictional_token","key":"owned_fictional_key"}"#,
    )
    .unwrap();
    let values = read(ID, &c, &p.ready, Some(&p.private_json)).unwrap();
    let m = material(&c, &values).unwrap();
    assert_eq!(m.headers["authorization"], "Bearer owned_fictional_token");
    assert!(!format!("{:?}", m.headers).contains("owned_fictional"));
    let mut changed = c.clone();
    changed.permissions.tools.insert("new".into(), true);
    changed.enabled = false;
    assert!(read(ID, &changed, &p.ready, Some(&p.private_json)).is_ok());
    assert!(material(&c, &BTreeMap::new()).is_err());
    let mut bad = c.clone();
    bad.headers.insert(
        "authorization".into(),
        mcp_profile::Header {
            value: Some("bad".into()),
            secret_ref: None,
        },
    );
    assert!(material(&bad, &values).is_err());
}
#[test]
fn private_envelope_capacity_canonical_id_and_unicode_environment() {
    let c = config();
    assert!(publication(
        "11111111-1111-4111-8111-11111111111A",
        &c,
        r#"{"main":"ok","key":"ok"}"#
    )
    .is_err());
    assert!(publication(
        ID,
        &c,
        &json!({"main":"a".repeat(1200),"key":"key"}).to_string()
    )
    .is_err());
    let c=mcp_profile::config(&json!({"profile_version":1,"transport":"stdio","command":"C:\\owned\\node.exe","auth":{"type":"none"},"env":{"OWNED_KEY":"main"}})).unwrap();
    let p = publication(ID, &c, &json!({"main":"Señor 🐾 private"}).to_string()).unwrap();
    let values = read(ID, &c, &p.ready, Some(&p.private_json)).unwrap();
    assert_eq!(
        material(&c, &values).unwrap().env["OWNED_KEY"],
        "Señor 🐾 private"
    );
}
