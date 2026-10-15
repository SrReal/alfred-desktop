use super::*;
fn snapshot(day: u8, sections: Value) -> Snapshot {
    serde_json::from_value(json!({"id":format!("s{day}"),"savedAt":"2026-10-14T10:00:00Z","formatVersion":1,"origin":{"id":"test-origin","computer":"TEST-PC"},"elevated":false,"untrusted":true,"checkedAt":format!("2026-10-{day:02}T10:00:00Z"),"scope":"Synthetic observations","sections":sections})).unwrap()
}
fn section(index: usize, data: Value) -> Value {
    json!({"name":SPECS[index].name,"status":"observed","data":data})
}
fn result(value: &Value, index: usize) -> &Value {
    value["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == SPECS[index].name)
        .unwrap()
}
#[test]
fn inventories_normalize_powershell_null_singletons_and_order() {
    let row = json!({"Name":"Fixture","Location":"HKCU/Test"});
    let a = snapshot(
        12,
        json!([section(5, row.clone()), section(2, Value::Null)]),
    );
    let b = snapshot(13, json!([section(5, json!([row])), section(2, json!([]))]));
    let diff = compare(&a, &b).unwrap();
    assert_eq!(result(&diff, 5)["status"], "unchanged");
    assert_eq!(result(&diff, 2)["status"], "unchanged");
    let spec = &SPECS[5];
    let x = json!({"Name":"B","Location":"HKCU/Test"});
    let y = json!({"Name":"A","Location":"HKCU/Test"});
    assert!(
        changes(&json!([x.clone(), y.clone()]), &json!([y, x]), spec)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn observed_changes_preserve_before_after_and_do_not_diagnose() {
    let a = snapshot(
        12,
        json!([
            section(1, json!({"RealTimeProtectionEnabled":true})),
            section(5, json!([{"Name":"Old","Location":"HKCU/Test"}]))
        ]),
    );
    let b = snapshot(
        13,
        json!([
            section(1, json!({"RealTimeProtectionEnabled":false})),
            section(5, json!([{"Name":"New","Location":"HKCU/Test"}]))
        ]),
    );
    let diff = compare(&a, &b).unwrap();
    assert_eq!(diff["changedSections"], 2);
    assert_eq!(result(&diff, 1)["changes"][0]["before"], true);
    assert_eq!(result(&diff, 1)["changes"][0]["after"], false);
    assert_eq!(result(&diff, 5)["changes"].as_array().unwrap().len(), 2);
    assert!(diff["incompleteSections"].as_u64().unwrap() > 0);
}
#[test]
fn unavailable_ambiguous_and_unknown_data_never_become_removals() {
    let row = json!({"Name":"Fixture","Location":"HKCU/Test"});
    let a = snapshot(
        12,
        json!([section(1,json!({"AntivirusEnabled":true})),section(5,row.clone()),section(6,json!([])),{"name":"Future","status":"observed","data":{}}]),
    );
    let b = snapshot(
        13,
        json!([{"name":SPECS[1].name,"status":"unavailable","error":"denied"},section(5,json!([row.clone(),row])),section(6,json!([{"wrong":"shape"}])),{"name":"Future","status":"observed","data":{}}]),
    );
    let diff = compare(&a, &b).unwrap();
    for (index, status) in [(1, "unavailable"), (5, "unsupported"), (6, "unsupported")] {
        assert_eq!(result(&diff, index)["status"], status);
        assert_eq!(result(&diff, index)["changes"], json!([]));
    }
    assert_eq!(diff["changedSections"], 0);
    assert!(diff["sections"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["name"] == "Future" && s["status"] == "unsupported"));
}
#[test]
fn rolling_errors_compare_frequencies_and_keep_sampling_limit_visible() {
    let e = json!({"log":"System","provider":"Fixture","id":5,"level":"Error","time":"2026-10-12T10:00:00Z"});
    let a = snapshot(12, json!([section(3, json!([e.clone(), e.clone()]))]));
    let b = snapshot(13, json!([section(3, json!([e]))]));
    let diff = compare(&a, &b).unwrap();
    let errors = result(&diff, 3);
    assert_eq!(errors["changes"][0]["before"], 2);
    assert_eq!(errors["changes"][0]["after"], 1);
    assert!(errors["note"]
        .as_str()
        .unwrap()
        .contains("sin haberse corregido"));
}
#[test]
fn comparison_rejects_different_origins_formats_and_reversed_or_equal_dates() {
    let a = snapshot(12, json!([]));
    let mut b = snapshot(13, json!([]));
    assert!(compare(&a, &a).is_err());
    assert!(compare(&b, &a).is_err());
    b.report.checked_at = a.report.checked_at.clone();
    assert!(compare(&a, &b).is_err());
    b.report.checked_at = "2026-10-13T10:00:00Z".into();
    b.origin.id = "other".into();
    assert!(compare(&a, &b).is_err());
    b.origin = a.origin.clone();
    b.format_version = 2;
    assert!(compare(&a, &b).is_err());
    b.format_version = 1;
    b.elevated = true;
    assert_eq!(compare(&a, &b).unwrap()["differentElevation"], true);
}
#[test]
fn history_is_bounded_durable_and_deletion_needs_confirmation() {
    let path = std::env::temp_dir().join(format!("ap-audits-{}.db", storage::id()));
    let store = Store::open(&path).unwrap();
    let mut latest = Value::Null;
    for _ in 0..23 {
        latest = save(
            &store,
            serde_json::to_value(snapshot(13, json!([])).report).unwrap(),
            false,
        )
        .unwrap();
    }
    let first_origin = latest["origin"]["id"].clone();
    assert_eq!(
        dispatch(&store, "system_audit.list", &json!({}))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        20
    );
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(
        dispatch(&store, "system_audit.get", &json!({"id":latest["id"]})).unwrap(),
        latest
    );
    assert!(dispatch(&store, "system_audit.delete", &json!({"id":latest["id"]})).is_err());
    dispatch(
        &store,
        "system_audit.delete",
        &json!({"id":latest["id"],"confirmed":true}),
    )
    .unwrap();
    assert!(dispatch(&store, "system_audit.get", &json!({"id":latest["id"]})).is_err());
    store
        .execute("DELETE FROM settings WHERE key=?", &[json!(ORIGIN)])
        .unwrap();
    let new = save(
        &store,
        serde_json::to_value(snapshot(14, json!([])).report).unwrap(),
        false,
    )
    .unwrap();
    assert_ne!(first_origin, new["origin"]["id"]);
    assert_eq!(
        dispatch(&store, "system_audit.list", &json!({}))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        20
    );
    drop(store);
    std::fs::remove_file(path).unwrap();
}
#[test]
fn malformed_reports_and_history_are_not_silently_overwritten() {
    let mut r = snapshot(13, json!([section(1, json!({})), section(1, json!({}))])).report;
    assert!(validate(&r).is_err());
    r.sections.clear();
    r.checked_at = "invalid".into();
    assert!(validate(&r).is_err());
    r.checked_at = "2026-10-13T10:00:00Z".into();
    r.scope = "a".repeat(MAX_REPORT);
    assert!(validate(&r).is_err());
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,json_value TEXT)")
        .unwrap();
    db.execute("INSERT INTO settings VALUES(?,?)", params![KEY, "{broken"])
        .unwrap();
    let store = Store(std::sync::Mutex::new(db));
    assert!(save(
        &store,
        serde_json::to_value(snapshot(13, json!([])).report).unwrap(),
        false
    )
    .is_err());
    assert!(dispatch(&store, "system_audit.list", &json!({})).is_err());
}
