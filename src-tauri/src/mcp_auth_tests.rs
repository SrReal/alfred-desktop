use super::*;
use std::{path::PathBuf, sync::Mutex};

pub(super) struct Fixture {
    pub(super) root: PathBuf,
    pub(super) identity: String,
    pub(super) core: Arc<Core>,
    pub(super) id: String,
    memory: Arc<Mutex<vault::TestVault>>,
}
impl Fixture {
    pub(super) async fn new() -> Self {
        let parent = std::env::temp_dir().canonicalize().unwrap();
        let root = parent.join(format!("ap-mcp-auth-{}", storage::id()));
        std::fs::create_dir(&root).unwrap();
        let identity = asset_files::identity(&asset_files::directory(&root).unwrap()).unwrap();
        let core = Arc::new(
            Core::new(
                root.join("profile"),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        core.store
            .execute(
                "UPDATE module_configs SET enabled=1 WHERE module_id='mcp'",
                &[],
            )
            .unwrap();
        let memory = Arc::new(Mutex::new(vault::TestVault::default()));
        *core.mcp_oauth_vault.lock().unwrap() = Some(memory.clone());
        let raw = json!({"profile_version":1,"transport":"streamable_http","base_url":"https://mcp.invalid","endpoint":"/mcp","auth":{"type":"none"}});
        let saved = integrations::save(
            &core,
            &json!({"module_id":"mcp","name":"Owned OAuth actor","config":raw}),
        )
        .await
        .unwrap();
        Self {
            root,
            identity,
            core,
            id: saved["id"].as_str().unwrap().into(),
            memory,
        }
    }
    fn authorization(&self) -> Authorization {
        Authorization {
            issuer: "https://issuer.invalid/tenant".into(),
            resource: "https://mcp.invalid/mcp".into(),
            token_endpoint: "https://issuer.invalid/tenant/token".into(),
            client_id: "owned-public-client".into(),
            client_auth: ClientAuth::None,
            requested_scopes: Some(BTreeSet::from(["files:read".into()])),
        }
    }
    fn actor(&self) -> Actor {
        Actor::new(&self.core, &self.id, self.authorization()).unwrap()
    }
    fn body() -> Value {
        json!({"token_type":"Bearer","access_token":"owned-access-token","refresh_token":"owned-refresh-token","expires_in":3600,"scope":"files:read"})
    }
    fn candidate(&self, a: &Actor) -> (State, Envelope) {
        let state = a.begin(false).unwrap();
        let state = a
            .transition(
                &state.session_id,
                Phase::Authorizing,
                Phase::ExchangeInFlight,
            )
            .unwrap();
        let env = a.tokens(&state, Self::body(), None, None).unwrap();
        let state = a
            .transition(
                &state.session_id,
                Phase::ExchangeInFlight,
                Phase::PublicationPending,
            )
            .unwrap();
        (state, env)
    }
    pub(super) fn finish(self) {
        assert_eq!(
            Arc::strong_count(&self.core),
            1,
            "drop actor/other Core references before fixture cleanup"
        );
        let parent = std::env::temp_dir().canonicalize().unwrap();
        let target = self.root.canonicalize().unwrap();
        assert!(
            target.is_absolute()
                && target.starts_with(&parent)
                && target.parent() == Some(parent.as_path())
        );
        let name = target.file_name().unwrap().to_str().unwrap();
        canonical(name.strip_prefix("ap-mcp-auth-").unwrap()).unwrap();
        assert_eq!(
            asset_files::identity(&asset_files::directory(&target).unwrap()).unwrap(),
            self.identity
        );
        drop(self.core);
        drop(self.memory);
        std::fs::remove_dir_all(&target).unwrap();
    }
}

#[tokio::test]
async fn publication_requires_pending_intent_exact_native_readback_and_public_cas() {
    let f = Fixture::new().await;
    let a = f.actor();
    let state = a.begin(false).unwrap();
    assert!(a.read().is_err());
    let env = a.tokens(&state, Fixture::body(), None, None).unwrap();
    assert_eq!(
        a.publish(&state.session_id, env).err().unwrap(),
        "MCP_OAUTH_PHASE"
    );
    let (state, env) = f.candidate(&a);
    let ready = a.publish(&state.session_id, env).unwrap();
    assert!(ready.phase == Phase::Ready && ready.credential_version.is_some());
    let (version, env) = a.read().unwrap();
    assert_eq!(ready.credential_version.unwrap(), version.to_string());
    assert_eq!(
        env.access_token.as_ref().unwrap().text(),
        "owned-access-token"
    );
    let settings = f
        .core
        .store
        .query("SELECT key,json_value FROM settings", &[])
        .unwrap()
        .to_string();
    assert!(
        !settings.contains("owned-access-token")
            && !settings.contains("owned-refresh-token")
            && !settings.contains("access_token")
            && !settings.contains("refresh_token")
    );
    drop(env);
    drop(a);
    f.finish();
}
#[tokio::test]
async fn large_tokens_use_binary_native_payload_and_do_not_escape_to_public_state() {
    let f = Fixture::new().await;
    let a = f.actor();
    let (state, _) = f.candidate(&a);
    let access = "access-owned-".repeat(3300);
    let refresh = "refresh-owned-".repeat(500);
    let env=a.tokens(&state,json!({"token_type":"bEaReR","access_token":access,"refresh_token":refresh,"scope":"files:read"}),None,None).unwrap();
    assert!(env.bytes(&a).unwrap().bytes().len() > 40000);
    let public = a.publish(&state.session_id, env).unwrap();
    let (_, env) = a.read().unwrap();
    assert_eq!(env.access_token.as_ref().unwrap().text(), access);
    assert_eq!(env.refresh_token.as_ref().unwrap().text(), refresh);
    assert!(!json!(public).to_string().contains("access-owned-"));
    drop(env);
    drop(a);
    f.finish();
}
#[tokio::test]
async fn cancellation_before_exchange_denies_publication_and_remote_continuation() {
    let f = Fixture::new().await;
    let a = f.actor();
    let state = a.begin(false).unwrap();
    let env = a.tokens(&state, Fixture::body(), None, None).unwrap();
    assert!(a.abandon(&state.session_id).unwrap().phase == Phase::Cancelled);
    assert!(a.publish(&state.session_id, env).is_err());
    let polled = std::sync::atomic::AtomicBool::new(false);
    assert!(a
        .guard(&state.session_id, async {
            polled.store(true, Ordering::SeqCst);
            Ok::<_, String>(())
        })
        .await
        .is_err());
    assert!(!polled.load(Ordering::SeqCst));
    assert!(native_observe(&f.core, &a.binding().unwrap())
        .unwrap()
        .is_none());
    drop(a);
    f.finish();
}
#[tokio::test]
async fn cancellation_after_exchange_or_refresh_is_uncertain_and_never_ready() {
    let f = Fixture::new().await;
    let a = f.actor();
    let (state, env) = f.candidate(&a);
    assert!(a.abandon(&state.session_id).unwrap().phase == Phase::Uncertain);
    assert!(a.publish(&state.session_id, env).is_err() && a.read().is_err());
    drop(a);
    f.finish();
}
#[tokio::test]
async fn superseded_attempt_cannot_publish_over_a_new_public_nonce() {
    let f = Fixture::new().await;
    let a = f.actor();
    let (old, env) = f.candidate(&a);
    let current = a.begin(false).unwrap();
    assert_ne!(old.session_id, current.session_id);
    assert_eq!(
        a.publish(&old.session_id, env).err().unwrap(),
        "MCP_OAUTH_ATTEMPT_SUPERSEDED"
    );
    assert_eq!(f.core.store.setting(&key(&f.id)), json!(current));
    assert!(native_observe(&f.core, &a.binding().unwrap())
        .unwrap()
        .is_none());
    drop(a);
    f.finish();
}
#[tokio::test]
async fn pointer_write_reported_failed_after_write_remains_blocked_without_retry() {
    let f = Fixture::new().await;
    let a = f.actor();
    let (state, env) = f.candidate(&a);
    f.memory.lock().unwrap().fail_commit = true;
    assert!(a.publish(&state.session_id, env).is_err());
    assert!(a.read().is_err());
    let state: State = serde_json::from_value(f.core.store.setting(&key(&f.id))).unwrap();
    assert!(state.phase == Phase::PublicationPending && state.credential_version.is_none());
    assert!(native_observe(&f.core, &a.binding().unwrap())
        .unwrap()
        .is_some());
    assert!(a.abandon(&state.session_id).unwrap().phase == Phase::Uncertain);
    drop(a);
    f.finish();
}
#[tokio::test]
async fn profile_generation_change_blocks_old_actor_before_native_publication() {
    let f = Fixture::new().await;
    let a = f.actor();
    let (state, env) = f.candidate(&a);
    f.core
        .store
        .set_setting(&mcp_save::generation_key(&f.id), &json!(storage::id()))
        .unwrap();
    assert_eq!(
        a.publish(&state.session_id, env).err().unwrap(),
        "MCP_OAUTH_AUTHORITY_CHANGED"
    );
    assert!(native_observe(&f.core, &a.binding().unwrap())
        .unwrap()
        .is_none());
    drop(a);
    f.finish();
}
#[tokio::test]
async fn second_core_revocation_fences_old_actor_even_after_module_reenabled() {
    let f = Fixture::new().await;
    let a = f.actor();
    let (state, env) = f.candidate(&a);
    let other = Arc::new(Core::new(f.core.root.clone(), f.core.resource.clone()).unwrap());
    assert_eq!(f.core.mcp_epoch.load(Ordering::SeqCst), 0);
    {
        let _lease = integrations::save_lock(&other).unwrap();
        other
            .store
            .execute(
                "UPDATE module_configs SET enabled=0 WHERE module_id='mcp'",
                &[],
            )
            .unwrap();
        crate::mcp::revoke(&other).unwrap();
        other
            .store
            .execute(
                "UPDATE module_configs SET enabled=1 WHERE module_id='mcp'",
                &[],
            )
            .unwrap();
        crate::mcp::revoke(&other).unwrap();
    }
    assert_eq!(f.core.mcp_epoch.load(Ordering::SeqCst), 0);
    assert!(a.check().is_err());
    assert_eq!(
        a.publish(&state.session_id, env).err().unwrap(),
        "MCP_OAUTH_AUTHORITY_CHANGED"
    );
    assert!(native_observe(&f.core, &a.binding().unwrap())
        .unwrap()
        .is_none());
    drop(other);
    drop(a);
    f.finish();
}
#[tokio::test]
async fn issuer_resource_client_profile_and_private_generation_are_exactly_bound() {
    let f = Fixture::new().await;
    let a = f.actor();
    let (state, env) = f.candidate(&a);
    let bytes = env.bytes(&a).unwrap();
    for (field, value) in [
        ("connection", json!(storage::id())),
        ("profile", json!(storage::id())),
        ("root_identity", json!("foreign")),
        ("config_generation", json!(storage::id())),
        ("target_sha256", json!("foreign")),
        ("publication_id", json!(Uuid::nil().to_string())),
    ] {
        let mut v = crate::mcp_oauth::strict_json(bytes.bytes()).unwrap();
        v[field] = value;
        let env: Envelope = serde_json::from_value(v).unwrap();
        assert!(env.validate(&a).is_err(), "{field}");
    }
    for field in ["issuer", "resource", "client_id", "token_endpoint"] {
        let mut v = crate::mcp_oauth::strict_json(bytes.bytes()).unwrap();
        v["authorization"][field] = json!("foreign");
        let env: Envelope = serde_json::from_value(v).unwrap();
        assert!(env.validate(&a).is_err(), "{field}");
    }
    let mut v = crate::mcp_oauth::strict_json(bytes.bytes()).unwrap();
    v["private_extra"] = json!(true);
    assert!(serde_json::from_value::<Envelope>(v).is_err());
    let text = String::from_utf8(bytes.bytes().to_vec()).unwrap();
    let duplicated = text.replacen("\"version\":1", "\"version\":1,\"version\":1", 1);
    assert!(crate::mcp_oauth::strict_json(duplicated.as_bytes()).is_err());
    assert!(a.tokens(&state, Fixture::body(), None, Some(env)).is_ok());
    drop(a);
    f.finish();
}
#[tokio::test]
async fn refresh_rotation_preserves_client_grant_identity_and_omitted_scopes() {
    let f = Fixture::new().await;
    let a = f.actor();
    let (state, env) = f.candidate(&a);
    a.publish(&state.session_id, env).unwrap();
    let (_, previous) = a.read().unwrap();
    let authorization_id = previous.authorization_id.clone();
    let state = a.begin(true).unwrap();
    let env=a.tokens(&state,json!({"token_type":"Bearer","access_token":"rotated-access","refresh_token":"rotated-refresh","expires_in":7200}),None,Some(previous)).unwrap();
    assert_eq!(env.authorization_id, authorization_id);
    assert_eq!(env.granted_scopes, a.authorization.requested_scopes);
    let state = a
        .transition(
            &state.session_id,
            Phase::RefreshInFlight,
            Phase::PublicationPending,
        )
        .unwrap();
    a.publish(&state.session_id, env).unwrap();
    let (_, env) = a.read().unwrap();
    assert_eq!(
        env.refresh_token.as_ref().unwrap().text(),
        "rotated-refresh"
    );
    assert_eq!(env.authorization_id, authorization_id);
    drop(env);
    drop(a);
    f.finish();
}
#[tokio::test]
async fn omitted_refresh_token_retains_previous_private_token_without_scope_expansion() {
    let f = Fixture::new().await;
    let a = f.actor();
    let (state, env) = f.candidate(&a);
    a.publish(&state.session_id, env).unwrap();
    let (_, previous) = a.read().unwrap();
    let state = a.begin(true).unwrap();
    let env = a
        .tokens(
            &state,
            json!({"token_type":"Bearer","access_token":"next-access"}),
            None,
            Some(previous),
        )
        .unwrap();
    assert_eq!(
        env.refresh_token.as_ref().unwrap().text(),
        "owned-refresh-token"
    );
    drop(env);
    drop(a);
    f.finish();
}
#[tokio::test]
async fn invalid_token_types_scopes_expiry_and_private_capacity_fail_before_publish() {
    let f = Fixture::new().await;
    let a = f.actor();
    let state = a.begin(false).unwrap();
    for (field, value) in [
        ("token_type", json!("DPoP")),
        ("access_token", json!("illegal token")),
        ("access_token", json!("illegal=inside")),
        ("expires_in", json!(0)),
        ("expires_in", json!(604801)),
        ("scope", json!("files:read files:write")),
        ("scope", json!("files:read files:read")),
        ("scope", json!("files:read ")),
        ("refresh_token", json!("x".repeat(32769))),
    ] {
        let mut body = Fixture::body();
        body[field] = value;
        assert!(a.tokens(&state, body, None, None).is_err(), "{field}");
    }
    let env=a.tokens(&state,json!({"token_type":"Bearer","access_token":"a".repeat(49152),"refresh_token":"b".repeat(32768),"scope":"files:read"}),None,None).unwrap();
    assert_eq!(env.bytes(&a).err().unwrap(), "MCP_OAUTH_ENVELOPE_CAPACITY");
    assert!(native_observe(&f.core, &a.binding().unwrap())
        .unwrap()
        .is_none());
    drop(env);
    drop(a);
    f.finish();
}
#[tokio::test]
async fn actor_reopens_same_root_but_changed_public_state_cannot_adopt_private_grant() {
    let f = Fixture::new().await;
    let a = f.actor();
    let (state, env) = f.candidate(&a);
    let ready = a.publish(&state.session_id, env).unwrap();
    drop(a);
    let other = Arc::new(Core::new(f.core.root.clone(), f.core.resource.clone()).unwrap());
    *other.mcp_oauth_vault.lock().unwrap() = Some(f.memory.clone());
    let a = Actor::new(&other, &f.id, f.authorization()).unwrap();
    let (_, env) = a.read().unwrap();
    assert_eq!(
        env.access_token.as_ref().unwrap().text(),
        "owned-access-token"
    );
    drop(env);
    let mut changed = json!(ready);
    changed["publication_id"] = json!(storage::id());
    f.core.store.set_setting(&key(&f.id), &changed).unwrap();
    assert_eq!(a.read().err().unwrap(), "MCP_OAUTH_CREDENTIAL_CHANGED");
    drop(a);
    drop(other);
    f.finish();
}
#[tokio::test]
async fn public_cas_and_invalid_phase_transitions_preserve_later_state() {
    let f = Fixture::new().await;
    let a = f.actor();
    let state = a.begin(false).unwrap();
    assert!(a
        .transition(&state.session_id, Phase::Authorizing, Phase::Ready)
        .is_err());
    let newer = a.begin(false).unwrap();
    {
        let _lease = integrations::save_lock(&f.core).unwrap();
        assert_eq!(
            a.cas_held(&state, &state).err().unwrap(),
            "MCP_OAUTH_STATE_CHANGED"
        );
    }
    assert_eq!(f.core.store.setting(&key(&f.id)), json!(newer));
    drop(a);
    f.finish();
}
#[tokio::test]
async fn invalid_tls_resource_ids_and_authority_fail_before_profile_identity_creation() {
    let f = Fixture::new().await;
    for field in ["issuer", "resource", "token_endpoint"] {
        let mut auth = f.authorization();
        match field {
            "issuer" => auth.issuer = "http://issuer.invalid".into(),
            "resource" => auth.resource = "https://mcp.invalid/other".into(),
            _ => auth.token_endpoint = "https://user:pass@issuer.invalid/token".into(),
        };
        assert!(Actor::new(&f.core, &f.id, auth).is_err());
        assert!(f.core.store.setting(PROFILE_KEY).is_null());
    }
    assert!(Actor::new(&f.core, &Uuid::nil().to_string(), f.authorization()).is_err());
    f.core
        .store
        .set_setting(AUTHORITY_KEY, &json!({"invalid":"foreign"}))
        .unwrap();
    assert!(Actor::new(&f.core, &f.id, f.authorization()).is_err());
    assert!(f.core.store.setting(PROFILE_KEY).is_null());
    f.finish();
}
#[tokio::test]
async fn production_resource_http_is_denied_before_identity_or_intent_creation() {
    let f = Fixture::new().await;
    for base in [
        "http://mcp.invalid",
        "http://127.0.0.1:8765",
        "http://localhost:8765",
        "http://[::1]:8765",
    ] {
        let mut raw = record(&f.core, &f.id).unwrap();
        raw["base_url"] = json!(base);
        raw["auth"] = json!({"type":"oauth","issuer":"https://issuer.invalid/tenant"});
        let saved = integrations::save(
            &f.core,
            &json!({"id":f.id,"module_id":"mcp","name":"Owned TLS rejection","config":raw}),
        )
        .await;
        if base == "http://mcp.invalid" {
            // The shared transport profile already rejects remote HTTP.
            assert_eq!(saved.unwrap_err(), "MCP_PROFILE_ORIGIN_OR_TLS");
        } else {
            saved.unwrap();
            let mut auth = f.authorization();
            auth.resource = mcp_profile::endpoint(
                &mcp_profile::config(&record(&f.core, &f.id).unwrap()).unwrap(),
            )
            .unwrap()
            .to_string();
            assert_eq!(
                Actor::new(&f.core, &f.id, auth).err().unwrap(),
                "MCP_OAUTH_RESOURCE_TLS_OR_FIELDS"
            );
        }
        assert!(f.core.store.setting(PROFILE_KEY).is_null());
        assert!(f.core.store.setting(&key(&f.id)).is_null());
    }
    f.finish();
}
#[tokio::test]
async fn private_grant_header_requires_exact_logical_resource_and_is_sensitive() {
    let f = Fixture::new().await;
    let a = f.actor();
    let (state, env) = f.candidate(&a);
    a.publish(&state.session_id, env).unwrap();
    let grant = a.grant().unwrap();
    for resource in [
        "https://issuer.invalid/tenant/token",
        "https://mcp.invalid/other",
        "https://mcp.invalid/mcp?token=owned",
        "https://foreign.invalid/mcp",
    ] {
        assert_eq!(
            grant.header_for_resource(resource).err().unwrap(),
            "MCP_OAUTH_RESOURCE_BINDING"
        );
    }
    let header = grant
        .header_for_resource("https://mcp.invalid/mcp")
        .unwrap();
    assert!(header.is_sensitive());
    assert_eq!(header.to_str().unwrap(), "Bearer owned-access-token");
    assert!(!format!("{header:?}").contains("owned-access-token"));
    assert!(grant.check().is_ok());
    drop(grant);
    drop(a);
    f.finish();
}
#[tokio::test]
async fn quarantine_and_refresh_rotation_invalidate_previously_materialized_grant() {
    let f = Fixture::new().await;
    let a = f.actor();
    let (state, env) = f.candidate(&a);
    a.publish(&state.session_id, env).unwrap();
    let old = a.grant().unwrap();
    let (_, previous) = a.read().unwrap();
    let state = a.begin(true).unwrap();
    assert_eq!(old.check().err().unwrap(), "MCP_OAUTH_GRANT_REVOKED");
    let env=a.tokens(&state,json!({"token_type":"Bearer","access_token":"new-rotated-access","refresh_token":"new-rotated-refresh"}),None,Some(previous)).unwrap();
    let state = a
        .transition(
            &state.session_id,
            Phase::RefreshInFlight,
            Phase::PublicationPending,
        )
        .unwrap();
    a.publish(&state.session_id, env).unwrap();
    assert_eq!(old.check().err().unwrap(), "MCP_OAUTH_GRANT_REVOKED");
    let current = a.grant().unwrap();
    assert_eq!(
        current
            .header_for_resource("https://mcp.invalid/mcp")
            .unwrap()
            .to_str()
            .unwrap(),
        "Bearer new-rotated-access"
    );
    drop(current);
    drop(old);
    drop(a);
    f.finish();
}
#[tokio::test]
async fn expired_private_access_requires_explicit_refresh_without_native_or_remote_retry() {
    let f = Fixture::new().await;
    let a = f.actor();
    let (state, mut env) = f.candidate(&a);
    env.issued_at = chrono::Utc::now().timestamp() - 4000;
    env.expires_at = Some(chrono::Utc::now().timestamp() - 1);
    let ready = a.publish(&state.session_id, env).unwrap();
    assert_eq!(a.grant().err().unwrap(), "MCP_OAUTH_REFRESH_REQUIRED");
    assert_eq!(f.core.store.setting(&key(&f.id)), json!(ready));
    assert_eq!(
        native_observe(&f.core, &a.binding().unwrap())
            .unwrap()
            .unwrap()
            .version
            .to_string(),
        ready.credential_version.unwrap()
    );
    drop(a);
    f.finish();
}
#[tokio::test]
async fn privacy_revocation_aborts_an_active_grant_future_and_prevents_new_headers() {
    let f = Fixture::new().await;
    let a = f.actor();
    let (state, env) = f.candidate(&a);
    a.publish(&state.session_id, env).unwrap();
    let grant = Arc::new(a.grant().unwrap());
    let signal = Arc::new(tokio::sync::Notify::new());
    let worker_grant = grant.clone();
    let worker_signal = signal.clone();
    let job = tokio::spawn(async move {
        worker_grant
            .guard(async {
                worker_signal.notify_one();
                std::future::pending::<Result<()>>().await
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), signal.notified())
        .await
        .unwrap();
    {
        let _lease = integrations::save_lock(&f.core).unwrap();
        f.core.store.set_setting("privacy", &json!(true)).unwrap();
        crate::mcp::revoke(&f.core).unwrap();
    }
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), job)
            .await
            .unwrap()
            .unwrap()
            .err()
            .unwrap(),
        "PRIVACY_DENIED"
    );
    assert_eq!(
        grant
            .header_for_resource("https://mcp.invalid/mcp")
            .err()
            .unwrap(),
        "PRIVACY_DENIED"
    );
    drop(grant);
    drop(a);
    f.finish();
}
#[tokio::test]
async fn native_metadata_change_denies_an_existing_private_grant_before_header_creation() {
    let f = Fixture::new().await;
    let a = f.actor();
    let (state, env) = f.candidate(&a);
    a.publish(&state.session_id, env).unwrap();
    let grant = a.grant().unwrap();
    f.memory
        .lock()
        .unwrap()
        .corrupt_client_metadata(&a.binding().unwrap())
        .unwrap();
    assert_eq!(
        grant
            .header_for_resource("https://mcp.invalid/mcp")
            .err()
            .unwrap(),
        "MCP_OAUTH_CREDENTIAL_CHANGED"
    );
    drop(grant);
    drop(a);
    f.finish();
}
