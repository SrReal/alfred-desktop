//! MCP OAuth actor state and native publication. HTTP/consent and Core entry
//! points are connected separately; a private token is never a public result.
use crate::{
    asset_files, drive_credentials as vault, integrations, mcp_profile, mcp_save, runtime::Core,
    storage,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    future::Future,
    sync::{
        atomic::{compiler_fence, Ordering},
        Arc,
    },
    time::Duration,
};
use uuid::Uuid;

type Result<T> = std::result::Result<T, String>;
const PROFILE_KEY: &str = "mcp.oauth.profile_identity";
pub(crate) const AUTHORITY_KEY: &str = "mcp.authority_generation";
const ENVELOPE_KIND: &str = "alfredpet.mcp.oauth.tokens";
const CLIENT_KIND: &str = "alfredpet.mcp.oauth.client";

fn canonical(s: &str) -> Result<()> {
    let id = Uuid::parse_str(s).map_err(|_| "MCP_OAUTH_ID")?;
    if id.is_nil() || id.to_string() != s {
        return Err("MCP_OAUTH_ID".into());
    }
    Ok(())
}
fn hash(v: &Value) -> [u8; 32] {
    Sha256::digest(v.to_string().as_bytes()).into()
}
fn root_identity(core: &Core) -> Result<String> {
    let identity = asset_files::identity(&asset_files::directory(&core.root)?)?;
    let path = core
        .root
        .canonicalize()
        .map_err(|_| "MCP_OAUTH_ROOT_IDENTITY")?;
    #[cfg(windows)]
    let path = {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    };
    #[cfg(not(windows))]
    let path = path
        .to_str()
        .ok_or("MCP_OAUTH_ROOT_IDENTITY")?
        .as_bytes()
        .to_vec();
    Ok(format!(
        "{:x}",
        Sha256::digest(
            json!({"identity":identity,"path":path})
                .to_string()
                .as_bytes()
        )
    ))
}
fn endpoint(s: &str, local: bool) -> Result<()> {
    endpoint_fields(s, local, false)
}
fn flow_endpoint(s: &str, local: bool) -> Result<()> {
    endpoint_fields(s, local, true)
}
fn endpoint_fields(s: &str, local: bool, query_allowed: bool) -> Result<()> {
    let u = reqwest::Url::parse(s).map_err(|_| "MCP_OAUTH_ENDPOINT")?;
    if s.is_empty()
        || s.len() > 4096
        || s.chars().any(|c| c.is_whitespace() || c.is_control())
        || s.contains('\\')
        || crate::policy::contains_secret(s)
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
        || (!query_allowed && u.query().is_some())
        || !(u.scheme() == "https"
            || (local
                && u.scheme() == "http"
                && matches!(u.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"))))
    {
        return Err("MCP_OAUTH_ENDPOINT_TLS_OR_FIELDS".into());
    }
    Ok(())
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClientAuth {
    None,
    ClientSecretPost,
    ClientSecretBasic,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Authorization {
    pub(crate) issuer: String,
    pub(crate) resource: String,
    pub(crate) token_endpoint: String,
    pub(crate) client_id: String,
    pub(crate) client_auth: ClientAuth,
    pub(crate) requested_scopes: Option<BTreeSet<String>>,
}
fn scope_set(scopes: &Option<BTreeSet<String>>) -> Result<()> {
    if let Some(set) = scopes {
        if set.len() > 256
            || set.iter().map(|s| s.len() + 1).sum::<usize>() > 8192
            || set.iter().any(|s| {
                s.is_empty()
                    || s.len() > 1024
                    || !s.bytes().all(|b| {
                        b == 0x21 || (0x23..=0x5b).contains(&b) || (0x5d..=0x7e).contains(&b)
                    })
            })
        {
            return Err("MCP_OAUTH_SCOPE".into());
        }
    }
    Ok(())
}
impl Authorization {
    fn validate(&self, c: &mcp_profile::Config, local: bool) -> Result<()> {
        if c.transport != mcp_profile::Transport::StreamableHttp
            || self.resource != mcp_profile::endpoint(c)?.as_str()
        {
            return Err("MCP_OAUTH_RESOURCE_BINDING".into());
        }
        if let mcp_profile::Auth::OAuth2 { issuer } = &c.auth {
            if *issuer != self.issuer {
                return Err("MCP_OAUTH_ISSUER_BINDING".into());
            }
        }
        endpoint(&self.issuer, local)?;
        // Exact resource binding is not confidentiality. Production bearer
        // traffic must use TLS too; owned HTTP fixtures are cfg(test) only.
        flow_endpoint(&self.resource, local)
            .map_err(|_| "MCP_OAUTH_RESOURCE_TLS_OR_FIELDS".to_string())?;
        flow_endpoint(&self.token_endpoint, local)?;
        if self.client_id.is_empty()
            || self.client_id.len() > 4096
            || self.client_id.chars().any(char::is_control)
        {
            return Err("MCP_OAUTH_CLIENT_ID".into());
        }
        scope_set(&self.requested_scopes)
    }
}
// Deliberately no Debug or generic public serializer. Owned token bytes are
// cleared; this does not certify erasure of every provider/allocator/OS copy.
pub(crate) struct Text(Vec<u8>);
impl Text {
    pub(crate) fn new(s: String, limit: usize) -> Result<Self> {
        let value = Self(s.into_bytes());
        if value.0.is_empty()
            || value.0.len() > limit
            || !value.0.iter().all(|b| (32..=126).contains(b))
        {
            return Err("MCP_OAUTH_PRIVATE_TEXT".into());
        }
        Ok(value)
    }
    pub(crate) fn text(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("")
    }
}
impl Drop for Text {
    fn drop(&mut self) {
        for b in &mut self.0 {
            unsafe {
                std::ptr::write_volatile(b, 0);
            }
        }
        compiler_fence(Ordering::SeqCst);
    }
}
impl Serialize for Text {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.text())
    }
}
impl<'de> Deserialize<'de> for Text {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::new(s, 49152).map_err(|_| serde::de::Error::custom("private text"))
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Envelope {
    version: u8,
    kind: String,
    connection: String,
    profile: String,
    root_identity: String,
    config_generation: String,
    target_sha256: String,
    publication_id: String,
    authorization_id: String,
    authorization: Authorization,
    pub(crate) client_secret: Option<Text>,
    pub(crate) access_token: Option<Text>,
    pub(crate) refresh_token: Option<Text>,
    granted_scopes: Option<BTreeSet<String>>,
    issued_at: i64,
    expires_at: Option<i64>,
    #[serde(default)]
    registration: Option<enrollment::Registration>,
    #[serde(default)]
    registration_access_token: Option<Text>,
}
impl Envelope {
    fn validate(&self, a: &Actor) -> Result<()> {
        if self.version != 1
            || ![ENVELOPE_KIND, CLIENT_KIND].contains(&self.kind.as_str())
            || self.connection != a.id
            || self.profile != a.profile
            || self.root_identity != a.root_identity
            || self.config_generation != a.generation
            || self.target_sha256 != a.target_sha256
            || self.authorization != a.authorization
        {
            return Err("MCP_OAUTH_ENVELOPE_BINDING".into());
        }
        canonical(&self.publication_id)?;
        canonical(&self.authorization_id)?;
        scope_set(&self.granted_scopes)?;
        if self.kind == ENVELOPE_KIND {
            if let Some(requested) = &a.authorization.requested_scopes {
                if self
                    .granted_scopes
                    .as_ref()
                    .is_none_or(|granted| !granted.is_subset(requested))
                {
                    return Err("MCP_OAUTH_GRANT_EXPANSION".into());
                }
            }
        } else if self.access_token.is_some()
            || self.refresh_token.is_some()
            || self.granted_scopes.is_some()
            || self.expires_at.is_some()
            || self.registration.is_none()
        {
            return Err("MCP_OAUTH_CLIENT_ONLY_FIELDS".into());
        }
        if let Some(registration) = &self.registration {
            registration.validate(
                a,
                self.client_secret.as_ref(),
                self.registration_access_token.as_ref(),
            )?;
        } else if self.registration_access_token.is_some() {
            return Err("MCP_OAUTH_CLIENT_REGISTRATION".into());
        }
        if (a.authorization.client_auth == ClientAuth::None) != self.client_secret.is_none()
            || self
                .client_secret
                .as_ref()
                .is_some_and(|t| t.0.len() > 16384)
            || self
                .refresh_token
                .as_ref()
                .is_some_and(|t| t.0.len() > 32768)
            || self
                .access_token
                .as_ref()
                .is_some_and(|t| t.0.len() > 49152)
            || self.issued_at <= 0
            || self
                .expires_at
                .is_some_and(|t| t <= self.issued_at || t - self.issued_at > 604800)
        {
            return Err("MCP_OAUTH_ENVELOPE_FIELDS".into());
        }
        if self.kind == CLIENT_KIND {
            return Ok(());
        }
        let text = self
            .access_token
            .as_ref()
            .ok_or("MCP_OAUTH_ACCESS_TOKEN_REQUIRED")?
            .text();
        let token = text.trim_end_matches('=');
        if token.is_empty()
            || !token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~+/".contains(&b))
        {
            return Err("MCP_OAUTH_BEARER_FORMAT".into());
        }
        Ok(())
    }
    fn bytes(&self, a: &Actor) -> Result<vault::Secret> {
        self.validate(a)?;
        let bytes = serde_json::to_vec(self).map_err(|_| "MCP_OAUTH_PRIVATE_ENVELOPE")?;
        let result = if bytes.len() <= 65536 {
            Ok(vault::Secret::copy(&bytes))
        } else {
            Err("MCP_OAUTH_ENVELOPE_CAPACITY".into())
        };
        let mut bytes = bytes;
        for b in &mut bytes {
            unsafe {
                std::ptr::write_volatile(b, 0);
            }
        }
        compiler_fence(Ordering::SeqCst);
        result
    }
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Authorizing,
    RegistrationInFlight,
    ExchangeInFlight,
    RefreshInFlight,
    PublicationPending,
    Ready,
    ClientRegistered,
    Uncertain,
    Cancelled,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct State {
    version: u8,
    connection: String,
    profile: String,
    root_identity: String,
    config_generation: String,
    target_sha256: String,
    authorization: Authorization,
    pub(crate) phase: Phase,
    pub(crate) session_id: String,
    pub(crate) publication_id: String,
    prior_version: Option<String>,
    pub(crate) credential_version: Option<String>,
    authority_generation: Value,
    cleanup_pending: bool,
    at: String,
    #[serde(default)]
    client_reference: Option<ClientReference>,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientReference {
    version: String,
    publication_id: String,
    config_generation: String,
    target_sha256: String,
    authorization: Authorization,
}
fn key(id: &str) -> String {
    format!("mcp.oauth.state.{id}")
}
fn authority(core: &Core) -> Result<Value> {
    let value = core.store.setting(AUTHORITY_KEY);
    if !value.is_null() {
        canonical(value.as_str().ok_or("MCP_OAUTH_AUTHORITY_GENERATION")?)?;
    }
    Ok(value)
}
fn record(core: &Core, id: &str) -> Result<Value> {
    canonical(id)?;
    let rows = core.store.query(
        "SELECT id,module_id,config_json,secret_ref FROM connections WHERE id=? COLLATE NOCASE",
        &[json!(id)],
    )?;
    if rows.as_array().is_none_or(|a| a.len() != 1)
        || rows[0]["id"] != id
        || rows[0]["module_id"] != "mcp"
        || rows[0]["secret_ref"] != crate::mcp_credentials::alias(id)?
    {
        return Err("MCP_OAUTH_CONNECTION_BINDING".into());
    }
    serde_json::from_str(rows[0]["config_json"].as_str().ok_or("MCP_OAUTH_PROFILE")?)
        .map_err(|_| "MCP_OAUTH_PROFILE".into())
}
#[derive(Clone)]
pub(crate) struct Actor {
    core: Arc<Core>,
    id: String,
    raw: Value,
    generation: String,
    profile: String,
    root_identity: String,
    target_sha256: String,
    authorization: Authorization,
    authority_generation: Value,
    epoch: u64,
    #[cfg(test)]
    local: bool,
}
impl Actor {
    pub(crate) fn new(core: &Arc<Core>, id: &str, authorization: Authorization) -> Result<Self> {
        Self::build(core, id, authorization, false)
    }
    fn build(
        core: &Arc<Core>,
        id: &str,
        authorization: Authorization,
        local: bool,
    ) -> Result<Self> {
        Self::build_mode(core, id, authorization, local, true)
    }
    fn build_mode(
        core: &Arc<Core>,
        id: &str,
        authorization: Authorization,
        local: bool,
        create: bool,
    ) -> Result<Self> {
        // Called only after individual Core authorization, never from prepare.
        let _lease = integrations::save_lock(core)?;
        if core.store.setting("privacy") == true {
            return Err("PRIVACY_DENIED".into());
        }
        if !core.enabled("mcp") {
            return Err("MODULE_DISABLED: mcp".into());
        }
        let raw = record(core, id)?;
        let c = mcp_profile::config(&raw)?;
        authorization.validate(&c, local)?;
        let authority_generation = authority(core)?;
        let profile = core.store.setting(PROFILE_KEY);
        let profile = if profile.is_null() {
            if !create {
                return Err("MCP_OAUTH_STATE_REQUIRED".into());
            }
            let p = storage::id();
            core.store.set_setting(PROFILE_KEY, &json!(p))?;
            p
        } else {
            profile.as_str().ok_or("MCP_OAUTH_PROFILE_IDENTITY")?.into()
        };
        canonical(&profile)?;
        let generation = core
            .store
            .setting(&mcp_save::generation_key(id))
            .as_str()
            .ok_or("MCP_OAUTH_PROFILE_GENERATION")?
            .to_string();
        canonical(&generation)?;
        let root_identity = root_identity(core)?;
        let target_sha256 = format!(
            "{:x}",
            Sha256::digest(mcp_profile::credential_target(&c).to_string().as_bytes())
        );
        let a = Self {
            core: core.clone(),
            id: id.into(),
            raw,
            generation,
            profile,
            root_identity,
            target_sha256,
            authorization,
            authority_generation,
            epoch: core.mcp_epoch.load(Ordering::SeqCst),
            #[cfg(test)]
            local,
        };
        a.check_held()?;
        Ok(a)
    }
    // Read-only loading never creates an identity or begins an authorization.
    pub(crate) fn load(core: &Arc<Core>, id: &str) -> Result<Self> {
        canonical(id)?;
        let state: State = serde_json::from_value(core.store.setting(&key(id)))
            .map_err(|_| "MCP_OAUTH_STATE_REQUIRED")?;
        if !matches!(
            mcp_profile::config(&record(core, id)?)?.auth,
            mcp_profile::Auth::OAuth2 { .. }
        ) {
            return Err("MCP_OAUTH_PROFILE_REQUIRED".into());
        }
        #[cfg(test)]
        let local = core.mcp_oauth_owned_network.load(Ordering::SeqCst);
        #[cfg(not(test))]
        let local = false;
        let actor = Self::build_mode(core, id, state.authorization.clone(), local, false)?;
        let _lease = integrations::save_lock(core)?;
        actor.state_held()?;
        Ok(actor)
    }
    pub(crate) fn state(&self) -> Result<State> {
        let _lease = integrations::save_lock(&self.core)?;
        self.state_held()
    }
    pub(crate) fn recover_local(&self, expected: &State) -> Result<State> {
        let _ownership = flow::ownership(self)?;
        let _lease = integrations::save_lock(&self.core)?;
        let old = self.state_held()?;
        if old != *expected || !matches!(old.phase, Phase::PublicationPending | Phase::Uncertain) {
            return Err("MCP_OAUTH_RECOVERY_REVIEW_REQUIRED".into());
        }
        let (version, bytes) = native_read(&self.core, &self.binding()?)?;
        let envelope: Envelope = serde_json::from_value(
            crate::mcp_oauth::strict_json(bytes.bytes())
                .map_err(|_| "MCP_OAUTH_PRIVATE_ENVELOPE")?,
        )
        .map_err(|_| "MCP_OAUTH_PRIVATE_ENVELOPE")?;
        envelope.validate(self)?;
        // A predecessor packet never proves the new remote operation succeeded.
        // authorization_id identifies a grant/rotation lineage, not this
        // attempt. publication_id is the private packet's exact intent fence.
        if envelope.publication_id != old.publication_id {
            return Err("MCP_OAUTH_REMOTE_OUTCOME_UNKNOWN".into());
        }
        let mut next = old.clone();
        next.phase = if envelope.kind == CLIENT_KIND {
            Phase::ClientRegistered
        } else {
            Phase::Ready
        };
        next.credential_version = Some(version.to_string());
        next.client_reference = envelope.registration.as_ref().map(|_| ClientReference {
            version: version.to_string(),
            publication_id: envelope.publication_id.clone(),
            config_generation: self.generation.clone(),
            target_sha256: self.target_sha256.clone(),
            authorization: self.authorization.clone(),
        });
        // Protected receipt retirement has its own recovery; keep this visible.
        next.cleanup_pending = true;
        next.at = storage::now();
        self.cas_held(&old, &next)?;
        Ok(next)
    }
    fn receipt_context(&self) -> [u8; 32] {
        hash(
            &json!({"kind":"mcp_oauth_owned_profile","connection":self.id,"profile":self.profile,"root":self.root_identity}),
        )
    }
    pub(crate) fn cleanup_receipts(&self, expected: &State) -> Result<State> {
        let _ownership = flow::ownership(self)?;
        let _lease = integrations::save_lock(&self.core)?;
        let old = self.state_held()?;
        if old != *expected {
            return Err("MCP_OAUTH_STATE_CHANGED_REVIEW_REQUIRED".into());
        }
        let binding = self.binding()?;
        let current = native_observe(&self.core, &binding)?.map(|o| o.version);
        if matches!(old.phase, Phase::Ready | Phase::ClientRegistered) {
            if current.map(|v| v.to_string()) != old.credential_version {
                return Err("MCP_OAUTH_CREDENTIAL_CHANGED".into());
            }
            if old.phase == Phase::Ready {
                self.read_held()?;
            } else {
                self.client_held()?.ok_or("MCP_OAUTH_CLIENT_REGISTRATION")?;
            }
        }
        // Persist local erase intent first. A failed readback/CAS never hides
        // pending cleanup, and a retry remains a separate reviewed action.
        let mut intent = old.clone();
        intent.cleanup_pending = true;
        intent.at = storage::now();
        self.cas_held(&old, &intent)?;
        let context = self.receipt_context();
        let inventory = native_inventory(&self.core, &binding, context)?;
        let mut denied = false;
        for item in inventory {
            if Some(item.recovery.version) == current || item.recovery.retirement_verified {
                continue;
            }
            // Every mutation changes the ledger revision. Reinspect this exact
            // protected generation, then let retire recompare all native bytes.
            let fresh = native_inventory(&self.core, &binding, context)?
                .into_iter()
                .find(|r| r.recovery.version == item.recovery.version)
                .ok_or("MCP_OAUTH_RECEIPT_CHANGED")?;
            if !fresh.recovery.can_retire {
                denied = true;
                continue;
            }
            if !native_retire(&self.core, &fresh.binding, context, &fresh.recovery)? {
                denied = true;
            }
        }
        native_compact(&self.core, &binding, context)?;
        let remaining = native_inventory(&self.core, &binding, context)?;
        let mut next = intent.clone();
        next.cleanup_pending = denied
            || remaining
                .iter()
                .any(|r| Some(r.recovery.version) != current && !r.recovery.retirement_verified);
        next.at = storage::now();
        self.cas_held(&intent, &next)
            .map_err(|e| format!("MCP_OAUTH_CLEANUP_UNCERTAIN:{e}"))?;
        Ok(next)
    }
    pub(crate) fn reset_unknown(&self, expected: &State) -> Result<State> {
        let _ownership = flow::ownership(self)?;
        let _lease = integrations::save_lock(&self.core)?;
        let old = self.state_held()?;
        if old != *expected
            || !matches!(
                old.phase,
                Phase::Uncertain
                    | Phase::RegistrationInFlight
                    | Phase::ExchangeInFlight
                    | Phase::RefreshInFlight
                    | Phase::PublicationPending
            )
        {
            return Err("MCP_OAUTH_RESET_REVIEW_REQUIRED".into());
        }
        // Only a privately verified client survives. No old/new access grant
        // is made Ready, no credential is erased and no remote request occurs.
        let reference = if let Ok((version, bytes)) = native_read(&self.core, &self.binding()?) {
            let env: Envelope = serde_json::from_value(
                crate::mcp_oauth::strict_json(bytes.bytes())
                    .map_err(|_| "MCP_OAUTH_PRIVATE_ENVELOPE")?,
            )
            .map_err(|_| "MCP_OAUTH_PRIVATE_ENVELOPE")?;
            env.validate(self)?;
            env.registration.as_ref().map(|_| ClientReference {
                version: version.to_string(),
                publication_id: env.publication_id.clone(),
                config_generation: self.generation.clone(),
                target_sha256: self.target_sha256.clone(),
                authorization: self.authorization.clone(),
            })
        } else if self.client_held().is_ok_and(|c| c.is_some()) {
            old.client_reference.clone()
        } else {
            None
        };
        let mut next = old.clone();
        next.phase = Phase::Cancelled;
        next.client_reference = reference;
        next.credential_version = None;
        next.cleanup_pending = true;
        next.at = storage::now();
        self.cas_held(&old, &next)?;
        Ok(next)
    }
    pub(crate) fn reconcile_startup(&self) -> Result<Option<State>> {
        let _ownership = flow::ownership(self)?;
        let _lease = integrations::save_lock(&self.core)?;
        let old = self.state_held()?;
        let mut next = old.clone();
        next.phase = match old.phase {
            Phase::Authorizing => Phase::Cancelled,
            Phase::RegistrationInFlight
            | Phase::ExchangeInFlight
            | Phase::RefreshInFlight
            | Phase::PublicationPending => Phase::Uncertain,
            _ => return Ok(None),
        };
        if next.phase == Phase::Uncertain {
            next.cleanup_pending = true;
        }
        next.at = storage::now();
        self.cas_held(&old, &next)?;
        Ok(Some(next))
    }
    fn check_held(&self) -> Result<()> {
        #[cfg(test)]
        let local = self.local;
        #[cfg(not(test))]
        let local = false;
        self.authorization
            .validate(&mcp_profile::config(&self.raw)?, local)?;
        if self.core.store.setting("privacy") == true {
            return Err("PRIVACY_DENIED".into());
        }
        if !self.core.enabled("mcp") {
            return Err("MODULE_DISABLED: mcp".into());
        }
        if self.core.mcp_epoch.load(Ordering::SeqCst) != self.epoch
            || self.core.store.setting(AUTHORITY_KEY) != self.authority_generation
            || record(&self.core, &self.id)? != self.raw
            || self.raw["enabled"] == false
            || self.core.store.setting(PROFILE_KEY) != self.profile
            || self.core.store.setting(&mcp_save::generation_key(&self.id)) != self.generation
            || root_identity(&self.core)? != self.root_identity
        {
            return Err("MCP_OAUTH_AUTHORITY_CHANGED".into());
        }
        Ok(())
    }
    pub(crate) fn check(&self) -> Result<()> {
        let _lease = integrations::save_lock(&self.core)?;
        self.check_held()
    }
    pub(crate) async fn guard<T>(
        &self,
        session: &str,
        f: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        self.check_session(session)?;
        tokio::pin!(f);
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        loop {
            tokio::select! {biased; _=tick.tick()=>self.check_session(session)?,r=&mut f=>{self.check_session(session)?;return r;}}
        }
    }
    fn binding(&self) -> Result<vault::Binding> {
        vault::Binding::for_domain(vault::Domain::McpOauth,&self.id,&self.profile,hash(&json!({"root":self.root_identity,"target":self.target_sha256,"authorization":self.authorization}))).map_err(str::to_string)
    }
    fn state_held(&self) -> Result<State> {
        self.check_held()?;
        let s: State = serde_json::from_value(self.core.store.setting(&key(&self.id)))
            .map_err(|_| "MCP_OAUTH_STATE_REQUIRED")?;
        if s.version != 1
            || s.connection != self.id
            || s.profile != self.profile
            || s.root_identity != self.root_identity
            || s.config_generation != self.generation
            || s.target_sha256 != self.target_sha256
            || s.authorization != self.authorization
            || s.authority_generation != self.authority_generation
        {
            return Err("MCP_OAUTH_STATE_BINDING".into());
        }
        canonical(&s.session_id)?;
        canonical(&s.publication_id)?;
        for v in [&s.prior_version, &s.credential_version]
            .into_iter()
            .flatten()
        {
            canonical(v)?;
        }
        Ok(s)
    }
    fn check_session_held(&self, session: &str) -> Result<State> {
        canonical(session)?;
        let s = self.state_held()?;
        if s.session_id != session || matches!(s.phase, Phase::Cancelled | Phase::Uncertain) {
            return Err("MCP_OAUTH_ATTEMPT_SUPERSEDED".into());
        }
        Ok(s)
    }
    fn check_session(&self, session: &str) -> Result<()> {
        let _lease = integrations::save_lock(&self.core)?;
        self.check_session_held(session).map(|_| ())
    }
    fn check_session_phase(&self, session: &str, phase: &Phase) -> Result<()> {
        let _lease = integrations::save_lock(&self.core)?;
        if self.check_session_held(session)?.phase != *phase {
            return Err("MCP_OAUTH_PHASE".into());
        }
        Ok(())
    }
    async fn guard_phase<T>(
        &self,
        session: &str,
        phase: Phase,
        f: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        self.check_session_phase(session, &phase)?;
        tokio::pin!(f);
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        loop {
            tokio::select! {biased; _=tick.tick()=>self.check_session_phase(session,&phase)?,r=&mut f=>{self.check_session_phase(session,&phase)?;return r;}}
        }
    }
    fn cas_held(&self, old: &State, new: &State) -> Result<()> {
        self.check_held()?;
        let mut db = self.core.store.0.lock().map_err(|_| "SQLITE_LOCK")?;
        let tx = db
            .transaction()
            .map_err(|_| "MCP_OAUTH_STATE_TRANSACTION")?;
        let setting = |name: &str| -> Value {
            tx.query_row("SELECT json_value FROM settings WHERE key=?", [name], |r| {
                r.get::<_, String>(0)
            })
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null)
        };
        let row: (String, String, String) = tx
            .query_row(
                "SELECT module_id,config_json,secret_ref FROM connections WHERE id=?",
                [&self.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|_| "MCP_OAUTH_AUTHORITY_CHANGED")?;
        let enabled: bool = tx
            .query_row(
                "SELECT enabled FROM module_configs WHERE module_id='mcp'",
                [],
                |r| r.get(0),
            )
            .map_err(|_| "MCP_OAUTH_AUTHORITY_CHANGED")?;
        if !enabled
            || setting("privacy") == true
            || setting(AUTHORITY_KEY) != self.authority_generation
            || setting(PROFILE_KEY) != self.profile
            || setting(&mcp_save::generation_key(&self.id)) != self.generation
            || row
                != (
                    "mcp".into(),
                    self.raw.to_string(),
                    crate::mcp_credentials::alias(&self.id)?,
                )
        {
            return Err("MCP_OAUTH_AUTHORITY_CHANGED".into());
        }
        if tx
            .execute(
                "UPDATE settings SET json_value=? WHERE key=? AND json_value=?",
                rusqlite::params![
                    json!(new).to_string(),
                    key(&self.id),
                    json!(old).to_string()
                ],
            )
            .map_err(|_| "MCP_OAUTH_STATE_TRANSACTION")?
            != 1
        {
            return Err("MCP_OAUTH_STATE_CHANGED".into());
        }
        tx.commit()
            .map_err(|_| "MCP_OAUTH_STATE_TRANSACTION".into())
    }
    pub(crate) fn begin(&self, refresh: bool) -> Result<State> {
        let _lease = integrations::save_lock(&self.core)?;
        self.check_held()?;
        let prior = if refresh {
            Some(self.read_held()?.0.to_string())
        } else {
            native_observe(&self.core, &self.binding()?)?.map(|o| o.version.to_string())
        };
        self.begin_held(refresh, prior)
    }
    // Reading a Ready grant and quarantining it belong to the same lease.
    // The HTTP caller must never supply a previously read refresh envelope.
    #[cfg(test)]
    fn begin_refresh(&self) -> Result<(State, Envelope)> {
        self.begin_refresh_checked(None)
    }
    fn begin_refresh_checked(&self, expected: Option<&State>) -> Result<(State, Envelope)> {
        let _lease = integrations::save_lock(&self.core)?;
        if expected.is_some_and(|expected| self.state_held().as_ref() != Ok(expected)) {
            return Err("MCP_OAUTH_STATE_CHANGED_REVIEW_REQUIRED".into());
        }
        let (version, envelope) = self.read_held()?;
        if envelope.refresh_token.is_none() {
            return Err("MCP_OAUTH_REAUTHORIZE_REQUIRED".into());
        }
        if let Some(registration) = &envelope.registration {
            registration.secret_valid_now()?;
        }
        let state = self.begin_held(true, Some(version.to_string()))?;
        Ok((state, envelope))
    }
    fn begin_held(&self, refresh: bool, prior: Option<String>) -> Result<State> {
        // Only a reference to an already verified native enrollment survives
        // a new grant intent. It confers no authority to use old access tokens.
        let client_reference = self.client_reference_held()?;
        let s = State {
            version: 1,
            connection: self.id.clone(),
            profile: self.profile.clone(),
            root_identity: self.root_identity.clone(),
            config_generation: self.generation.clone(),
            target_sha256: self.target_sha256.clone(),
            authorization: self.authorization.clone(),
            phase: if refresh {
                Phase::RefreshInFlight
            } else {
                Phase::Authorizing
            },
            session_id: storage::id(),
            publication_id: storage::id(),
            prior_version: prior,
            credential_version: None,
            authority_generation: self.authority_generation.clone(),
            cleanup_pending: false,
            at: storage::now(),
            client_reference,
        };
        self.core.store.set_setting(&key(&self.id), &json!(s))?;
        Ok(s)
    }
    pub(crate) fn transition(&self, session: &str, from: Phase, to: Phase) -> Result<State> {
        let _lease = integrations::save_lock(&self.core)?;
        let old = self.check_session_held(session)?;
        if old.phase != from
            || !matches!(
                (&from, &to),
                (Phase::Authorizing, Phase::ExchangeInFlight)
                    | (Phase::Authorizing, Phase::RegistrationInFlight)
                    | (Phase::RegistrationInFlight, Phase::PublicationPending)
                    | (
                        Phase::ExchangeInFlight | Phase::RefreshInFlight,
                        Phase::PublicationPending
                    )
            )
        {
            return Err("MCP_OAUTH_PHASE".into());
        }
        let mut next = old.clone();
        next.phase = to;
        next.at = storage::now();
        self.cas_held(&old, &next)?;
        Ok(next)
    }
    pub(crate) fn abandon(&self, session: &str) -> Result<State> {
        let _lease = integrations::save_lock(&self.core)?;
        let old = self.check_session_held(session)?;
        if matches!(old.phase, Phase::Ready | Phase::ClientRegistered) {
            return Err("MCP_OAUTH_ALREADY_FINISHED".into());
        }
        let mut next = old.clone();
        next.phase = if matches!(
            old.phase,
            Phase::RegistrationInFlight
                | Phase::ExchangeInFlight
                | Phase::RefreshInFlight
                | Phase::PublicationPending
        ) {
            Phase::Uncertain
        } else {
            Phase::Cancelled
        };
        next.at = storage::now();
        self.cas_held(&old, &next)?;
        Ok(next)
    }
    fn read_held(&self) -> Result<(Uuid, Envelope)> {
        let state = self.state_held()?;
        if state.phase != Phase::Ready {
            return Err("MCP_OAUTH_REAUTHORIZE_REQUIRED".into());
        }
        let (version, bytes) = native_read(&self.core, &self.binding()?)?;
        if state.credential_version.as_deref() != Some(version.to_string().as_str()) {
            return Err("MCP_OAUTH_CREDENTIAL_CHANGED".into());
        }
        let v = crate::mcp_oauth::strict_json(bytes.bytes())
            .map_err(|_| "MCP_OAUTH_PRIVATE_ENVELOPE")?;
        let env: Envelope = serde_json::from_value(v).map_err(|_| "MCP_OAUTH_PRIVATE_ENVELOPE")?;
        env.validate(self)?;
        if env.kind != ENVELOPE_KIND {
            return Err("MCP_OAUTH_CLIENT_ONLY_NOT_GRANT".into());
        }
        if env.publication_id != state.publication_id {
            return Err("MCP_OAUTH_CREDENTIAL_CHANGED".into());
        }
        Ok((version, env))
    }
    #[cfg(test)]
    pub(crate) fn read(&self) -> Result<(Uuid, Envelope)> {
        let _lease = integrations::save_lock(&self.core)?;
        self.read_held()
    }
    pub(crate) fn grant(&self) -> Result<Grant> {
        let _lease = integrations::save_lock(&self.core)?;
        let (version, envelope) = self.read_held()?;
        let grant = Grant {
            actor: self.clone(),
            version,
            publication_id: envelope.publication_id.clone(),
            expires_at: envelope.expires_at,
            access: envelope
                .access_token
                .ok_or("MCP_OAUTH_ACCESS_TOKEN_REQUIRED")?,
        };
        grant.check_held()?;
        Ok(grant)
    }
    fn client_reference_held(&self) -> Result<Option<ClientReference>> {
        let raw = self.core.store.setting(&key(&self.id));
        if raw.is_null() {
            return Ok(None);
        }
        let old: State = serde_json::from_value(raw).map_err(|_| "MCP_OAUTH_STATE_REQUIRED")?;
        if old.connection != self.id
            || old.profile != self.profile
            || old.root_identity != self.root_identity
            || old.target_sha256 != self.target_sha256
        {
            return Ok(None);
        }
        let Some(reference) = old.client_reference else {
            return Ok(None);
        };
        canonical(&reference.version)?;
        canonical(&reference.publication_id)?;
        canonical(&reference.config_generation)?;
        if reference.target_sha256 != self.target_sha256
            || !same_client(&reference.authorization, &self.authorization)
        {
            return Ok(None);
        }
        reference
            .authorization
            .validate(&mcp_profile::config(&self.raw)?, self.local_network())?;
        Ok(Some(reference))
    }
    fn local_network(&self) -> bool {
        #[cfg(test)]
        {
            self.local
        }
        #[cfg(not(test))]
        {
            false
        }
    }
    fn client_held(&self) -> Result<Option<enrollment::Registered>> {
        self.check_held()?;
        let Some(reference) = self.client_reference_held()? else {
            return Ok(None);
        };
        let mut original = self.clone();
        original.authorization = reference.authorization;
        original.generation = reference.config_generation;
        let (version, bytes) = native_read(&self.core, &original.binding()?)?;
        if reference.version != version.to_string() {
            return Err("MCP_OAUTH_CLIENT_RECOVERY_REQUIRED".into());
        }
        let value = crate::mcp_oauth::strict_json(bytes.bytes())
            .map_err(|_| "MCP_OAUTH_PRIVATE_ENVELOPE")?;
        let envelope: Envelope =
            serde_json::from_value(value).map_err(|_| "MCP_OAUTH_PRIVATE_ENVELOPE")?;
        envelope.validate(&original)?;
        if envelope.publication_id != reference.publication_id {
            return Err("MCP_OAUTH_CLIENT_RECOVERY_REQUIRED".into());
        }
        let info = envelope
            .registration
            .ok_or("MCP_OAUTH_CLIENT_REGISTRATION")?;
        info.secret_valid_now()?;
        Ok(Some(enrollment::Registered {
            info,
            secret: envelope.client_secret,
            management_token: envelope.registration_access_token,
        }))
    }
    fn client(&self) -> Result<Option<enrollment::Registered>> {
        let _lease = integrations::save_lock(&self.core)?;
        self.client_held()
    }
    fn registration_envelope(
        &self,
        state: &State,
        registered: enrollment::Registered,
    ) -> Result<Envelope> {
        let envelope = Envelope {
            version: 1,
            kind: CLIENT_KIND.into(),
            connection: self.id.clone(),
            profile: self.profile.clone(),
            root_identity: self.root_identity.clone(),
            config_generation: self.generation.clone(),
            target_sha256: self.target_sha256.clone(),
            publication_id: state.publication_id.clone(),
            authorization_id: storage::id(),
            authorization: self.authorization.clone(),
            client_secret: registered.secret,
            access_token: None,
            refresh_token: None,
            granted_scopes: None,
            issued_at: chrono::Utc::now().timestamp(),
            expires_at: None,
            registration: Some(registered.info),
            registration_access_token: registered.management_token,
        };
        envelope.validate(self)?;
        Ok(envelope)
    }
    fn tokens_registered(
        &self,
        state: &State,
        body: Value,
        registered: enrollment::Registered,
    ) -> Result<Envelope> {
        registered.info.secret_valid_now()?;
        let mut envelope = self.tokens(state, body, registered.secret, None)?;
        envelope.registration = Some(registered.info);
        envelope.registration_access_token = registered.management_token;
        envelope.validate(self)?;
        Ok(envelope)
    }
    pub(crate) fn tokens(
        &self,
        state: &State,
        body: Value,
        client_secret: Option<Text>,
        previous: Option<Envelope>,
    ) -> Result<Envelope> {
        let refresh = previous.is_some();
        if let Some(env) = &previous {
            env.validate(self)?;
        }
        let Value::Object(mut body) = body else {
            return Err("MCP_OAUTH_TOKEN_RESPONSE".into());
        };
        if !body
            .get("token_type")
            .and_then(Value::as_str)
            .is_some_and(|s| s.eq_ignore_ascii_case("Bearer"))
        {
            return Err("MCP_OAUTH_TOKEN_TYPE".into());
        }
        let Some(Value::String(access)) = body.remove("access_token") else {
            return Err("MCP_OAUTH_TOKEN_RESPONSE".into());
        };
        let access_token = Text::new(access, 49152)?;
        let issued_at = chrono::Utc::now().timestamp();
        let expires_at = body
            .remove("expires_in")
            .map(|v| {
                v.as_i64()
                    .filter(|n| *n > 0 && *n <= 604800)
                    .and_then(|n| issued_at.checked_add(n))
                    .ok_or("MCP_OAUTH_TOKEN_EXPIRY")
            })
            .transpose()?;
        let mut previous = previous;
        if refresh && client_secret.is_some() {
            return Err("MCP_OAUTH_REFRESH_CLIENT_CHANGED".into());
        }
        let client_secret =
            client_secret.or_else(|| previous.as_mut().and_then(|e| e.client_secret.take()));
        let old_grants = previous.as_ref().and_then(|e| e.granted_scopes.clone());
        let granted_scopes = if let Some(v) = body.remove("scope") {
            let s = v
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 8192)
                .ok_or("MCP_OAUTH_SCOPE")?;
            let items = s.split(' ').map(str::to_string).collect::<Vec<_>>();
            let set = items.iter().cloned().collect::<BTreeSet<_>>();
            if items.len() != set.len() {
                return Err("MCP_OAUTH_SCOPE".into());
            }
            Some(set)
        } else if refresh {
            old_grants.clone()
        } else {
            self.authorization.requested_scopes.clone()
        };
        if refresh
            && old_grants.as_ref().is_some_and(|old| {
                granted_scopes
                    .as_ref()
                    .is_none_or(|new| !new.is_subset(old))
            })
        {
            return Err("MCP_OAUTH_GRANT_EXPANSION".into());
        }
        let refresh_token = if let Some(v) = body.remove("refresh_token") {
            let Value::String(s) = v else {
                return Err("MCP_OAUTH_REFRESH_TOKEN".into());
            };
            Some(Text::new(s, 32768)?)
        } else {
            previous.as_mut().and_then(|e| e.refresh_token.take())
        };
        let authorization_id = previous
            .as_ref()
            .map_or_else(storage::id, |e| e.authorization_id.clone());
        let env = Envelope {
            version: 1,
            kind: ENVELOPE_KIND.into(),
            connection: self.id.clone(),
            profile: self.profile.clone(),
            root_identity: self.root_identity.clone(),
            config_generation: self.generation.clone(),
            target_sha256: self.target_sha256.clone(),
            publication_id: state.publication_id.clone(),
            authorization_id,
            authorization: self.authorization.clone(),
            client_secret,
            access_token: Some(access_token),
            refresh_token,
            granted_scopes,
            issued_at,
            expires_at,
            registration: previous.as_ref().and_then(|e| e.registration.clone()),
            registration_access_token: previous
                .as_mut()
                .and_then(|e| e.registration_access_token.take()),
        };
        env.validate(self)?;
        Ok(env)
    }
    pub(crate) fn publish(&self, session: &str, env: Envelope) -> Result<State> {
        let _lease = integrations::save_lock(&self.core)?;
        let old = self.check_session_held(session)?;
        if old.phase != Phase::PublicationPending || env.publication_id != old.publication_id {
            return Err("MCP_OAUTH_PHASE".into());
        }
        let binding = self.binding()?;
        let bytes = env.bytes(self)?;
        let observed = native_observe(&self.core, &binding)?;
        if observed.as_ref().map(|o| o.version.to_string()) != old.prior_version {
            return Err("MCP_OAUTH_CREDENTIAL_CHANGED".into());
        }
        // Stable original-profile context permits rotation/explicit client
        // reauthorization while preserving protected predecessor receipts.
        // Manifest/client hash and envelope/public state bind the grant itself.
        let context = self.receipt_context();
        let receipt = native_publish(
            &self.core,
            &binding,
            observed.as_ref(),
            bytes.bytes(),
            context,
        )?;
        let (version, readback) = native_read(&self.core, &binding)?;
        if receipt.version != version || readback.bytes() != bytes.bytes() {
            return Err("MCP_OAUTH_PUBLICATION_UNCERTAIN".into());
        }
        self.check_session_held(session)?;
        let mut next = old.clone();
        next.phase = if env.kind == CLIENT_KIND {
            Phase::ClientRegistered
        } else {
            Phase::Ready
        };
        next.credential_version = Some(receipt.version.to_string());
        next.cleanup_pending = !receipt.cleanup_complete;
        next.at = storage::now();
        next.client_reference = env.registration.as_ref().map(|_| ClientReference {
            version: receipt.version.to_string(),
            publication_id: env.publication_id.clone(),
            config_generation: self.generation.clone(),
            target_sha256: self.target_sha256.clone(),
            authorization: self.authorization.clone(),
        });
        self.cas_held(&old, &next)?;
        Ok(next)
    }
}
fn same_client(a: &Authorization, b: &Authorization) -> bool {
    a.issuer == b.issuer
        && a.resource == b.resource
        && a.client_id == b.client_id
        && a.client_auth == b.client_auth
}

// Only the validated logical MCP resource receives a header. The transport
// must also enforce its own same-server endpoint/redirect/session constraints.
// Neither this type nor its access token can enter a public generic serializer.
pub(crate) struct Grant {
    actor: Actor,
    version: Uuid,
    publication_id: String,
    expires_at: Option<i64>,
    access: Text,
}
impl Grant {
    pub(crate) fn contains_private(&self, value: &Value) -> bool {
        crate::mcp_client::redact_literal(value, self.access.text()) != *value
    }
    pub(crate) fn redact(&self, value: &Value) -> Value {
        crate::mcp_client::redact_literal(value, self.access.text())
    }
    fn check_held(&self) -> Result<()> {
        let state = self.actor.state_held()?;
        if state.phase != Phase::Ready
            || state.credential_version.as_deref() != Some(self.version.to_string().as_str())
            || state.publication_id != self.publication_id
        {
            return Err("MCP_OAUTH_GRANT_REVOKED".into());
        }
        if self
            .expires_at
            .is_some_and(|at| at <= chrono::Utc::now().timestamp())
        {
            return Err("MCP_OAUTH_REFRESH_REQUIRED".into());
        }
        let binding = self.actor.binding()?;
        if !native_observe(&self.actor.core, &binding)?
            .is_some_and(|o| binding.matches_client(&o) && o.version == self.version)
        {
            return Err("MCP_OAUTH_CREDENTIAL_CHANGED".into());
        }
        Ok(())
    }
    pub(crate) fn check(&self) -> Result<()> {
        let _lease = integrations::save_lock(&self.actor.core)?;
        self.check_held()
    }
    #[cfg(test)]
    pub(crate) async fn guard<T>(&self, f: impl Future<Output = Result<T>>) -> Result<T> {
        self.check()?;
        tokio::pin!(f);
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        loop {
            tokio::select! {biased; _=tick.tick()=>self.check()?,r=&mut f=>{self.check()?;return r;}}
        }
    }
    pub(crate) fn header_for_resource(
        &self,
        resource: &str,
    ) -> Result<reqwest::header::HeaderValue> {
        let _lease = integrations::save_lock(&self.actor.core)?;
        self.check_held()?;
        if resource != self.actor.authorization.resource {
            return Err("MCP_OAUTH_RESOURCE_BINDING".into());
        }
        let mut header =
            reqwest::header::HeaderValue::from_str(&format!("Bearer {}", self.access.text()))
                .map_err(|_| "MCP_OAUTH_BEARER_FORMAT")?;
        header.set_sensitive(true);
        Ok(header)
    }
}

fn native_observe(c: &Core, b: &vault::Binding) -> Result<Option<vault::Observation>> {
    #[cfg(test)]
    if let Some(v) = c
        .mcp_oauth_vault
        .lock()
        .map_err(|_| "MCP_OAUTH_TEST_VAULT_LOCK")?
        .as_ref()
    {
        return v
            .lock()
            .map_err(|_| "MCP_OAUTH_TEST_VAULT_LOCK")?
            .observe(b)
            .map_err(str::to_string);
    }
    #[cfg(windows)]
    {
        let _ = c;
        vault::win::observe(b).map_err(str::to_string)
    }
    #[cfg(not(windows))]
    {
        let _ = (c, b);
        Err("WINDOWS_11_REQUIRED".into())
    }
}
fn native_read(c: &Core, b: &vault::Binding) -> Result<(Uuid, vault::Secret)> {
    #[cfg(test)]
    if let Some(v) = c
        .mcp_oauth_vault
        .lock()
        .map_err(|_| "MCP_OAUTH_TEST_VAULT_LOCK")?
        .as_ref()
    {
        return v
            .lock()
            .map_err(|_| "MCP_OAUTH_TEST_VAULT_LOCK")?
            .read(b)
            .map_err(str::to_string);
    }
    #[cfg(windows)]
    {
        let _ = c;
        vault::win::read(b).map_err(str::to_string)
    }
    #[cfg(not(windows))]
    {
        let _ = (c, b);
        Err("WINDOWS_11_REQUIRED".into())
    }
}
fn native_publish(
    c: &Core,
    b: &vault::Binding,
    o: Option<&vault::Observation>,
    bytes: &[u8],
    context: [u8; 32],
) -> Result<vault::Receipt> {
    #[cfg(test)]
    if let Some(v) = c
        .mcp_oauth_vault
        .lock()
        .map_err(|_| "MCP_OAUTH_TEST_VAULT_LOCK")?
        .as_ref()
    {
        return v
            .lock()
            .map_err(|_| "MCP_OAUTH_TEST_VAULT_LOCK")?
            .reauthorize_context(b, o, bytes, context)
            .map_err(str::to_string);
    }
    #[cfg(windows)]
    {
        let _ = c;
        vault::win::reauthorize_context(b, o, bytes, context).map_err(str::to_string)
    }
    #[cfg(not(windows))]
    {
        let _ = (c, b, o, bytes, context);
        Err("WINDOWS_11_REQUIRED".into())
    }
}

fn native_inventory(
    c: &Core,
    b: &vault::Binding,
    context: [u8; 32],
) -> Result<Vec<vault::OwnedRecovery>> {
    #[cfg(test)]
    if let Some(v) = c
        .mcp_oauth_vault
        .lock()
        .map_err(|_| "MCP_OAUTH_TEST_VAULT_LOCK")?
        .as_ref()
    {
        return v
            .lock()
            .map_err(|_| "MCP_OAUTH_TEST_VAULT_LOCK")?
            .mcp_context_inventory(b, context)
            .map_err(str::to_string);
    }
    #[cfg(windows)]
    {
        let _ = c;
        vault::win::mcp_context_inventory(b, context).map_err(str::to_string)
    }
    #[cfg(not(windows))]
    {
        let _ = (c, b, context);
        Err("WINDOWS_11_REQUIRED".into())
    }
}
fn native_retire(
    c: &Core,
    b: &vault::Binding,
    context: [u8; 32],
    expected: &vault::Recovery,
) -> Result<bool> {
    #[cfg(test)]
    if let Some(v) = c
        .mcp_oauth_vault
        .lock()
        .map_err(|_| "MCP_OAUTH_TEST_VAULT_LOCK")?
        .as_ref()
    {
        return v
            .lock()
            .map_err(|_| "MCP_OAUTH_TEST_VAULT_LOCK")?
            .recover(b, Some(context), expected)
            .map_err(str::to_string);
    }
    #[cfg(windows)]
    {
        let _ = c;
        vault::win::recover(b, Some(context), expected).map_err(str::to_string)
    }
    #[cfg(not(windows))]
    {
        let _ = (c, b, context, expected);
        Err("WINDOWS_11_REQUIRED".into())
    }
}
fn native_compact(c: &Core, b: &vault::Binding, context: [u8; 32]) -> Result<()> {
    #[cfg(test)]
    if let Some(v) = c
        .mcp_oauth_vault
        .lock()
        .map_err(|_| "MCP_OAUTH_TEST_VAULT_LOCK")?
        .as_ref()
    {
        return v
            .lock()
            .map_err(|_| "MCP_OAUTH_TEST_VAULT_LOCK")?
            .compact_mcp_context(b, context)
            .map_err(str::to_string);
    }
    #[cfg(windows)]
    {
        let _ = c;
        vault::win::compact_mcp_context(b, context).map_err(str::to_string)
    }
    #[cfg(not(windows))]
    {
        let _ = (c, b, context);
        Err("WINDOWS_11_REQUIRED".into())
    }
}

#[cfg(test)]
#[path = "mcp_auth_tests.rs"]
mod tests;

#[path = "mcp_auth_flow.rs"]
pub(crate) mod flow;

#[path = "mcp_auth_enrollment.rs"]
pub(crate) mod enrollment;

#[path = "mcp_auth_service.rs"]
pub(crate) mod service;

#[cfg(all(test, windows))]
#[path = "mcp_auth_native_tests.rs"]
mod native_tests;
