use super::*;
use catalog::{Category, Fence};

#[tokio::test]
async fn peer_store_invalidation_denies_prepared_actions_and_preserves_other_catalogs() {
    let f = Fixture::new().await;
    let input = f.input(f.observed().await);
    let prepared = prepare(&f.core, "mcp.call", &input).await.unwrap();
    let cloned = prepared.catalog.clone().unwrap();
    let resources = Fence::capture(&f.core.store, &f.id, Category::Resources).unwrap();
    let other = Fence::capture(&f.core.store, &crate::storage::id(), Category::Tools).unwrap();
    let epoch = f.core.mcp_epoch.load(Ordering::SeqCst);
    let authority = f.core.store.setting(crate::mcp_auth::AUTHORITY_KEY);
    let peer = Arc::new(Core::new(f.root.join("profile"), f.core.resource.clone()).unwrap());
    catalog::invalidate(&peer, &f.id, Category::Tools).unwrap();
    assert_eq!(
        cloned.check(&f.core.store).unwrap_err(),
        "MCP_CATALOG_CHANGED_REVIEW_REQUIRED"
    );
    resources.check(&f.core.store).unwrap();
    other.check(&f.core.store).unwrap();
    assert_eq!(f.core.mcp_epoch.load(Ordering::SeqCst), epoch);
    assert_eq!(
        f.core.store.setting(crate::mcp_auth::AUTHORITY_KEY),
        authority
    );
    assert_eq!(
        execute(prepared).await.unwrap_err(),
        "MCP_CATALOG_CHANGED_REVIEW_REQUIRED"
    );
    assert_eq!(f.count("tools/call"), 0);
    drop(peer);
    f.clean().await;
}

#[tokio::test]
async fn checked_catalog_storage_distinguishes_initial_absence_corruption_and_sql_error() {
    let f = Fixture::new().await;
    let absent = Fence::capture(&f.core.store, &f.id, Category::Prompts).unwrap();
    let key = format!("mcp.catalog.{}.prompts", f.id);
    for raw in [
        "{",
        "null",
        "7",
        "{}",
        "\"00000000-0000-0000-0000-000000000000\"",
        "\"AAAAAAAA-AAAA-4AAA-AAAA-AAAAAAAAAAAA\"",
    ] {
        f.core.store.execute("INSERT INTO settings VALUES(?,?) ON CONFLICT(key) DO UPDATE SET json_value=excluded.json_value", &[json!(key), json!(raw)]).unwrap();
        assert_eq!(
            absent.check(&f.core.store).unwrap_err(),
            "MCP_CATALOG_FORMAT"
        );
        assert_eq!(
            Fence::capture(&f.core.store, &f.id, Category::Prompts)
                .err()
                .unwrap(),
            "MCP_CATALOG_FORMAT"
        );
        assert_eq!(
            catalog::invalidate(&f.core, &f.id, Category::Prompts).unwrap_err(),
            "MCP_CATALOG_FORMAT"
        );
    }
    f.core
        .store
        .execute("DELETE FROM settings WHERE key=?", &[json!(key)])
        .unwrap();
    catalog::invalidate(&f.core, &f.id, Category::Prompts).unwrap();
    let fresh = Fence::capture(&f.core.store, &f.id, Category::Prompts).unwrap();
    f.core
        .store
        .execute("DELETE FROM settings WHERE key=?", &[json!(key)])
        .unwrap();
    assert_eq!(
        fresh.check(&f.core.store).unwrap_err(),
        "MCP_CATALOG_CHANGED_REVIEW_REQUIRED"
    );
    f.core
        .store
        .execute("ALTER TABLE settings RENAME TO owned_missing_settings", &[])
        .unwrap();
    assert_eq!(
        absent.check(&f.core.store).unwrap_err(),
        "MCP_CATALOG_STORAGE"
    );
    assert!(Fence::capture(&f.core.store, &f.id, Category::Prompts).is_err());
    f.core
        .store
        .execute("ALTER TABLE owned_missing_settings RENAME TO settings", &[])
        .unwrap();
    f.clean().await;
}

#[tokio::test]
async fn failed_durable_notification_preserves_generation_and_cached_observation() {
    let f = Fixture::new().await;
    catalog::invalidate(&f.core, &f.id, Category::Tools).unwrap();
    let input = f.input(f.observed().await);
    let prepared = prepare(&f.core, "mcp.call", &input).await.unwrap();
    f.core.store.execute("CREATE TRIGGER owned_catalog_failure BEFORE UPDATE ON settings WHEN NEW.key LIKE 'mcp.catalog.%' BEGIN SELECT RAISE(ABORT,'owned failure'); END", &[]).unwrap();
    assert_eq!(
        catalog::invalidate(&f.core, &f.id, Category::Tools).unwrap_err(),
        "MCP_CATALOG_STORAGE"
    );
    prepared.check_catalog().unwrap();
    check_owned_observation(&prepared).unwrap();
    drop(prepared);
    f.clean().await;
}

#[tokio::test]
async fn notification_while_permission_waits_prevents_discovery_and_business_dispatch() {
    let f = Fixture::new().await;
    let input = f.observed().await;
    let discoveries = f.count("server/discover");
    let error = f
        .approve("mcp.call", input, "allow", || {
            catalog::invalidate(&f.core, &f.id, Category::Tools).unwrap()
        })
        .await
        .unwrap_err();
    assert_eq!(error, "MCP_CATALOG_CHANGED_REVIEW_REQUIRED");
    assert_eq!(f.count("server/discover"), discoveries);
    assert_eq!(f.count("tools/call"), 0);
    assert!(f.core.pending.lock().unwrap().is_empty());
    f.clean().await;
}

#[tokio::test]
async fn notification_during_fetch_cannot_publish_observations_for_the_old_catalog() {
    let f = Fixture::new().await;
    f.m.lock().unwrap().delay_method = Some("tools/list".into());
    let started = f.m.lock().unwrap().started.clone();
    let core = f.core.clone();
    let id = f.id.clone();
    let change = async move {
        tokio::time::timeout(Duration::from_secs(8), started.notified())
            .await
            .unwrap();
        catalog::invalidate(&core, &id, Category::Tools).unwrap();
    };
    let (result, _) = tokio::join!(f.approve("mcp.tools", json!({}), "allow", || ()), change);
    assert_eq!(result.unwrap_err(), "MCP_CATALOG_CHANGED_REVIEW_REQUIRED");
    assert!(f.core.mcp_observations.lock().unwrap().is_empty());
    assert_eq!(f.count("tools/list"), 1);
    f.clean().await;
}

#[tokio::test]
async fn notification_after_remote_dispatch_is_uncertain_and_never_replayed() {
    let f = Fixture::new().await;
    let input = f.observed().await;
    f.m.lock().unwrap().delay = true;
    let started = f.m.lock().unwrap().started.clone();
    let core = f.core.clone();
    let id = f.id.clone();
    let change = async move {
        tokio::time::timeout(Duration::from_secs(8), started.notified())
            .await
            .unwrap();
        catalog::invalidate(&core, &id, Category::Tools).unwrap();
    };
    let (result, _) = tokio::join!(f.approve("mcp.call", input, "allow", || ()), change);
    assert_eq!(
        result.unwrap_err(),
        "MCP_OUTCOME_UNCERTAIN:MCP_CATALOG_CHANGED_REVIEW_REQUIRED"
    );
    assert_eq!(f.count("tools/call"), 1);
    f.clean().await;
}

#[tokio::test]
async fn catalog_changes_leave_oauth_observations_independent() {
    let f = Fixture::new().await;
    let expected = registration_observation(&f).await;
    catalog::invalidate(&f.core, &f.id, Category::Tools).unwrap();
    let prepared = prepare(&f.core, "mcp.oauth_registration", &f.input(json!({"issuer":"https://as.example/tenant","expected":expected,"allow_legacy_dynamic":true,"requested_scopes":null}))).await.unwrap();
    assert!(prepared.catalog.is_none());
    assert!(prepared.observation.as_ref().unwrap().catalog.is_none());
    check_owned_observation(&prepared).unwrap();
    drop(prepared);
    assert_eq!(f.count("tools/call"), 0);
    f.clean().await;
}
