//! Installed-app Drive OAuth actor. Credential contents stay in the native vault;
//! no legacy token import, arbitrary endpoint, automatic exchange/refresh replay.
use crate::{asset_files, drive_credentials as vault, integrations, runtime::Core, storage};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;
pub(crate) const DRIVE_SCOPE: &str = "https://www.googleapis.com/auth/drive.file";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const PROFILE_KEY: &str = "drive.profile_identity";
type Result<T> = std::result::Result<T, String>;

fn canonical(id: &str) -> Result<()> {
    let uuid = Uuid::parse_str(id).map_err(|_| "DRIVE_CONNECTION_ID")?;
    if uuid.is_nil() || uuid.to_string() != id {
        return Err("DRIVE_CONNECTION_ID".into());
    }
    Ok(())
}
pub(crate) fn resource_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 200
        || id == "root"
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err("DRIVE_RESOURCE_ID".into());
    }
    Ok(())
}
pub(crate) fn validate_config(v: &Value) -> Result<()> {
    let schema = json!({"type":"object","required":["auth","client_id","client_secret_mode","allowed_files","allowed_folders"],"properties":{
        "auth":{"const":"installed_app_pkce"},"client_id":{"type":"string","minLength":1,"maxLength":512},
        "client_secret_mode":{"enum":["none","credential_manager"]},"read_only":{"type":"boolean"},
        "base_url":{"const":"https://www.googleapis.com"},"scopes":{"const":[DRIVE_SCOPE]},
        "allowed_files":{"type":"array","maxItems":1000,"uniqueItems":true,"items":{"type":"string"}},
        "allowed_folders":{"type":"array","maxItems":1000,"uniqueItems":true,"items":{"type":"string"}}
    },"additionalProperties":false});
    if !jsonschema::validator_for(&schema)
        .map_err(|_| "DRIVE_PROFILE_SCHEMA")?
        .is_valid(v)
    {
        return Err("DRIVE_PROFILE_INVALID".into());
    }
    let client = v["client_id"].as_str().ok_or("DRIVE_CLIENT_ID")?;
    if !client.ends_with(".apps.googleusercontent.com")
        || !client
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err("DRIVE_CLIENT_ID".into());
    }
    for key in ["allowed_files", "allowed_folders"] {
        for id in v[key].as_array().ok_or("DRIVE_PROFILE_INVALID")? {
            resource_id(id.as_str().ok_or("DRIVE_RESOURCE_ID")?)?;
        }
    }
    Ok(())
}
pub(crate) fn record(c: &Core, id: &str) -> Result<Value> {
    canonical(id)?;
    let rows = c.store.query(
        "SELECT id,module_id,config_json,secret_ref FROM connections WHERE id=? COLLATE NOCASE",
        &[json!(id)],
    )?;
    if rows.as_array().is_none_or(|r| r.len() != 1)
        || rows[0]["id"] != id
        || rows[0]["module_id"] != "drive"
        || rows[0]["secret_ref"] != format!("drive.credentials.{id}")
    {
        return Err("DRIVE_CONNECTION_ALIAS_CONFLICT".into());
    }
    let config: Value = serde_json::from_str(
        rows[0]["config_json"]
            .as_str()
            .ok_or("DRIVE_PROFILE_INVALID")?,
    )
    .map_err(|_| "DRIVE_PROFILE_INVALID")?;
    validate_config(&config)?;
    Ok(config)
}
fn binding(c: &Core, id: &str, config: &Value, initialize: bool) -> Result<vault::Binding> {
    canonical(id)?;
    let profile = c.store.setting(PROFILE_KEY);
    let profile = if profile.is_null() {
        if !initialize {
            return Err("DRIVE_PROFILE_IDENTITY_REQUIRED".into());
        }
        let id = storage::id();
        c.store.set_setting(PROFILE_KEY, &json!(id))?;
        id
    } else {
        let id = profile.as_str().ok_or("DRIVE_PROFILE_IDENTITY")?;
        canonical(id)?;
        id.into()
    };
    let root = asset_files::identity(&asset_files::directory(&c.root)?)?;
    let digest=Sha256::digest(json!({"provider":TOKEN_URL,"client_id":config["client_id"],"mode":config["client_secret_mode"],"root":root}).to_string().as_bytes());
    vault::Binding::new(id, &profile, digest.into()).map_err(str::to_string)
}
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
struct Text(Vec<u8>);
impl Text {
    fn from_str(s: &str, limit: usize) -> Result<Self> {
        if s.is_empty() || s.len() > limit || !s.bytes().all(|b| (33..=126).contains(&b)) {
            return Err("DRIVE_PRIVATE_TOKEN_INVALID".into());
        }
        Ok(Self(s.as_bytes().to_vec()))
    }
    fn text(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("")
    }
}
impl Drop for Text {
    fn drop(&mut self) {
        for b in &mut self.0 {
            unsafe { std::ptr::write_volatile(b, 0) };
        }
        std::sync::atomic::compiler_fence(Ordering::SeqCst);
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u8,
    #[serde(default)]
    authorization_id: Option<String>,
    client_id: String,
    client_secret_mode: String,
    client_secret: Option<Text>,
    access_token: Option<Text>,
    refresh_token: Option<Text>,
    issued_at: Option<i64>,
    expires_at: Option<i64>,
    granted_scopes: Vec<String>,
}
impl Envelope {
    fn empty(config: &Value, secret: Option<Text>) -> Self {
        Self {
            version: 1,
            authorization_id: None,
            client_id: config["client_id"].as_str().unwrap_or("").into(),
            client_secret_mode: config["client_secret_mode"].as_str().unwrap_or("").into(),
            client_secret: secret,
            access_token: None,
            refresh_token: None,
            issued_at: None,
            expires_at: None,
            granted_scopes: vec![],
        }
    }
    fn validate(&self, config: &Value) -> Result<()> {
        if let Some(id) = &self.authorization_id {
            canonical(id).map_err(|_| "DRIVE_ENVELOPE_AUTHORIZATION")?;
            if self.access_token.is_none() {
                return Err("DRIVE_ENVELOPE_AUTHORIZATION".into());
            }
        }
        if self.version != 1
            || self.client_id != config["client_id"]
            || self.client_secret_mode != config["client_secret_mode"]
            || (self.client_secret_mode == "none" && self.client_secret.is_some())
        {
            return Err("DRIVE_ENVELOPE_BINDING".into());
        }
        for (token, limit) in [
            (&self.client_secret, 4096),
            (&self.access_token, 2048),
            (&self.refresh_token, 512),
        ] {
            if let Some(t) = token {
                Text::from_str(t.text(), limit)?;
            }
        }
        if self.access_token.is_some() {
            let issued = self.issued_at.ok_or("DRIVE_ENVELOPE_EXPIRY")?;
            let expiry = self.expires_at.ok_or("DRIVE_ENVELOPE_EXPIRY")?;
            if issued < 0
                || expiry
                    .checked_sub(issued)
                    .is_none_or(|n| !(1..=604800).contains(&n))
                || self.refresh_token.is_none()
                || self.granted_scopes != [DRIVE_SCOPE]
            {
                return Err("DRIVE_ENVELOPE_GRANT".into());
            }
        } else if self.refresh_token.is_some()
            || self.issued_at.is_some()
            || self.expires_at.is_some()
            || !self.granted_scopes.is_empty()
        {
            return Err("DRIVE_ENVELOPE_GRANT".into());
        }
        Ok(())
    }
    fn bytes(&self) -> Result<vault::Secret> {
        let bytes = serde_json::to_vec(self).map_err(|_| "DRIVE_ENVELOPE_ENCODE")?;
        if bytes.len() > 65536 {
            return Err("DRIVE_ENVELOPE_LIMIT".into());
        }
        Ok(vault::Secret::copy(&bytes))
    }
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct TestTransport {
    pub(crate) token_url: String,
    pub(crate) api_base: String,
    pub(crate) vault: Arc<std::sync::Mutex<vault::TestVault>>,
    pub(crate) timeout_ms: u64,
    pub(crate) native_storage: bool,
    pub(crate) headless_permission: bool,
}
fn storage_read(c: &Core, b: &vault::Binding) -> Result<(Uuid, vault::Secret)> {
    #[cfg(not(test))]
    let _ = c;
    #[cfg(test)]
    if let Some(f) = c
        .drive_fixture
        .lock()
        .map_err(|_| "DRIVE_FIXTURE_LOCK")?
        .as_ref()
        .filter(|f| !f.native_storage)
    {
        return f
            .vault
            .lock()
            .map_err(|_| "DRIVE_FIXTURE_LOCK")?
            .read(b)
            .map_err(str::to_string);
    }
    #[cfg(windows)]
    {
        vault::win::read(b).map_err(str::to_string)
    }
    #[cfg(not(windows))]
    {
        let _ = (c, b);
        Err("WINDOWS_11_REQUIRED".into())
    }
}
fn storage_observe(c: &Core, b: &vault::Binding) -> Result<Option<vault::Observation>> {
    #[cfg(not(test))]
    let _ = c;
    #[cfg(test)]
    if let Some(f) = c
        .drive_fixture
        .lock()
        .map_err(|_| "DRIVE_FIXTURE_LOCK")?
        .as_ref()
        .filter(|f| !f.native_storage)
    {
        return f
            .vault
            .lock()
            .map_err(|_| "DRIVE_FIXTURE_LOCK")?
            .observe(b)
            .map_err(str::to_string);
    }
    #[cfg(windows)]
    {
        vault::win::observe(b).map_err(str::to_string)
    }
    #[cfg(not(windows))]
    {
        let _ = (c, b);
        Err("WINDOWS_11_REQUIRED".into())
    }
}
fn storage_save(
    c: &Core,
    b: &vault::Binding,
    observed: Option<&vault::Observation>,
    value: &[u8],
) -> Result<vault::Receipt> {
    #[cfg(not(test))]
    let _ = c;
    #[cfg(test)]
    if let Some(f) = c
        .drive_fixture
        .lock()
        .map_err(|_| "DRIVE_FIXTURE_LOCK")?
        .as_ref()
        .filter(|f| !f.native_storage)
    {
        return f
            .vault
            .lock()
            .map_err(|_| "DRIVE_FIXTURE_LOCK")?
            .reauthorize(b, observed, value)
            .map_err(str::to_string);
    }
    #[cfg(windows)]
    {
        vault::win::reauthorize(b, observed, value).map_err(str::to_string)
    }
    #[cfg(not(windows))]
    {
        let _ = (c, b, observed, value);
        Err("WINDOWS_11_REQUIRED".into())
    }
}
fn storage_replace(
    c: &Core,
    b: &vault::Binding,
    version: Uuid,
    value: &[u8],
) -> Result<vault::Receipt> {
    #[cfg(not(test))]
    let _ = c;
    #[cfg(test)]
    if let Some(f) = c
        .drive_fixture
        .lock()
        .map_err(|_| "DRIVE_FIXTURE_LOCK")?
        .as_ref()
        .filter(|f| !f.native_storage)
    {
        return f
            .vault
            .lock()
            .map_err(|_| "DRIVE_FIXTURE_LOCK")?
            .replace(b, Some(version), value)
            .map_err(str::to_string);
    }
    #[cfg(windows)]
    {
        vault::win::replace(b, Some(version), value).map_err(str::to_string)
    }
    #[cfg(not(windows))]
    {
        let _ = (c, b, version, value);
        Err("WINDOWS_11_REQUIRED".into())
    }
}
fn receipt_context(value: &Value) -> Result<[u8; 32]> {
    let s = value.as_str().ok_or("DRIVE_RECEIPT_CONTEXT")?;
    if s.len() != 64
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("DRIVE_RECEIPT_CONTEXT".into());
    }
    let mut hash = [0; 32];
    for (i, p) in s.as_bytes().chunks_exact(2).enumerate() {
        hash[i] = u8::from_str_radix(
            std::str::from_utf8(p).map_err(|_| "DRIVE_RECEIPT_CONTEXT")?,
            16,
        )
        .map_err(|_| "DRIVE_RECEIPT_CONTEXT")?;
    }
    if hash == [0; 32] {
        return Err("DRIVE_RECEIPT_CONTEXT".into());
    }
    Ok(hash)
}
fn storage_save_capability(
    c: &Core,
    b: &vault::Binding,
    value: &[u8],
    context: [u8; 32],
) -> Result<vault::Receipt> {
    #[cfg(not(test))]
    let _ = c;
    #[cfg(test)]
    if let Some(f) = c
        .drive_fixture
        .lock()
        .map_err(|_| "DRIVE_FIXTURE_LOCK")?
        .as_ref()
        .filter(|f| !f.native_storage)
    {
        return f
            .vault
            .lock()
            .map_err(|_| "DRIVE_FIXTURE_LOCK")?
            .replace_context(b, None, value, context)
            .map_err(str::to_string);
    }
    #[cfg(windows)]
    {
        vault::win::replace_context(b, None, value, context).map_err(str::to_string)
    }
    #[cfg(not(windows))]
    {
        let _ = (c, b, value, context);
        Err("WINDOWS_11_REQUIRED".into())
    }
}
fn storage_erase_capability(
    c: &Core,
    b: &vault::Binding,
    version: Uuid,
    context: [u8; 32],
) -> Result<bool> {
    #[cfg(not(test))]
    let _ = c;
    #[cfg(test)]
    if let Some(f) = c
        .drive_fixture
        .lock()
        .map_err(|_| "DRIVE_FIXTURE_LOCK")?
        .as_ref()
        .filter(|f| !f.native_storage)
    {
        return f
            .vault
            .lock()
            .map_err(|_| "DRIVE_FIXTURE_LOCK")?
            .erase_context(b, version, context)
            .map_err(str::to_string);
    }
    #[cfg(windows)]
    {
        vault::win::erase_context(b, version, context).map_err(str::to_string)
    }
    #[cfg(not(windows))]
    {
        let _ = (c, b, version, context);
        Err("WINDOWS_11_REQUIRED".into())
    }
}
fn storage_recoveries(
    c: &Core,
    b: &vault::Binding,
    context: [u8; 32],
) -> Result<Vec<vault::Recovery>> {
    #[cfg(not(test))]
    let _ = c;
    #[cfg(test)]
    if let Some(f) = c
        .drive_fixture
        .lock()
        .map_err(|_| "DRIVE_FIXTURE_LOCK")?
        .as_ref()
        .filter(|f| !f.native_storage)
    {
        return f
            .vault
            .lock()
            .map_err(|_| "DRIVE_FIXTURE_LOCK")?
            .recoveries(b, Some(context))
            .map_err(str::to_string);
    }
    #[cfg(windows)]
    {
        vault::win::recoveries(b, Some(context)).map_err(str::to_string)
    }
    #[cfg(not(windows))]
    {
        let _ = (c, b, context);
        Err("WINDOWS_11_REQUIRED".into())
    }
}
fn storage_recover(
    c: &Core,
    b: &vault::Binding,
    context: [u8; 32],
    expected: &vault::Recovery,
) -> Result<bool> {
    #[cfg(not(test))]
    let _ = c;
    #[cfg(test)]
    if let Some(f) = c
        .drive_fixture
        .lock()
        .map_err(|_| "DRIVE_FIXTURE_LOCK")?
        .as_ref()
        .filter(|f| !f.native_storage)
    {
        return f
            .vault
            .lock()
            .map_err(|_| "DRIVE_FIXTURE_LOCK")?
            .recover(b, Some(context), expected)
            .map_err(str::to_string);
    }
    #[cfg(windows)]
    {
        vault::win::recover(b, Some(context), expected).map_err(str::to_string)
    }
    #[cfg(not(windows))]
    {
        let _ = (c, b, context, expected);
        Err("WINDOWS_11_REQUIRED".into())
    }
}
/// Called while integrations::save_lock is HELD, after provider/case checks and
/// all migration validation, before updating the public connection record.
pub(crate) fn save(c: &Arc<Core>, id: &str, v: &Value, old: Option<&Value>) -> Result<()> {
    let config = &v["config"];
    canonical(id)?;
    validate_config(config)?;
    let name = v["name"].as_str().ok_or("DRIVE_CONNECTION_NAME")?;
    if name.trim().is_empty() || name.len() > 200 {
        return Err("DRIVE_CONNECTION_NAME".into());
    }
    let previous = old
        .and_then(|r| r["config_json"].as_str())
        .and_then(|s| serde_json::from_str::<Value>(s).ok());
    let changed = old.is_some_and(|r| r["secret_ref"] != format!("drive.credentials.{id}"))
        || (old.is_some()
            && previous.as_ref().is_none_or(|p| {
                validate_config(p).is_err()
                    || p["client_id"] != config["client_id"]
                    || p["client_secret_mode"] != config["client_secret_mode"]
            }));
    let secret = v["secret"].as_str().filter(|s| !s.is_empty());
    if changed && v["reauthorize"] != true {
        return Err("DRIVE_EXPLICIT_REAUTHORIZATION_REQUIRED".into());
    }
    if config["client_secret_mode"] == "none" && secret.is_some() {
        return Err("DRIVE_CLIENT_SECRET_NOT_CONFIGURED".into());
    }
    if changed && config["client_secret_mode"] == "credential_manager" && secret.is_none() {
        return Err("DRIVE_NEW_CLIENT_SECRET_REQUIRED".into());
    }
    let secret = secret.map(|s| Text::from_str(s, 4096)).transpose()?;
    let b = binding(c, id, config, true)?;
    let observed = storage_observe(c, &b)?;
    if old.is_none() && observed.is_some() {
        return Err("DRIVE_CREDENTIAL_ALIAS_COLLISION".into());
    }
    c.store.set_setting(
        &format!("drive.connection_generation.{id}"),
        &json!(storage::id()),
    )?;
    c.store
        .set_setting(&format!("drive.auth_attempt.{id}"), &Value::Null)?;
    if changed || secret.is_some() {
        let bytes = Envelope::empty(config, secret).bytes()?;
        match storage_save(c, &b, observed.as_ref(), bytes.bytes()) {
            Ok(_) => c
                .store
                .set_setting(&state_key(id), &json!({"phase":"reauthorize"}))?,
            Err(_) => {
                c.store.set_setting(
                    &state_key(id),
                    &json!({"phase":"uncertain","error":"DRIVE_CREDENTIAL_SAVE_UNCERTAIN"}),
                )?;
                return Err("DRIVE_CREDENTIAL_SAVE_UNCERTAIN".into());
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct Scope {
    pub(crate) core: Arc<Core>,
    pub(crate) id: String,
    pub(crate) config: Value,
    generation: Value,
    #[cfg(test)]
    fixture: Option<TestTransport>,
}
impl Scope {
    pub(crate) fn new(c: &Arc<Core>, id: &str) -> Result<Self> {
        let lease = integrations::save_lock(c)?;
        let s = Self {
            core: c.clone(),
            id: id.into(),
            config: record(c, id)?,
            generation: c
                .store
                .setting(&format!("drive.connection_generation.{id}")),
            #[cfg(test)]
            fixture: c
                .drive_fixture
                .lock()
                .map_err(|_| "DRIVE_FIXTURE_LOCK")?
                .clone(),
        };
        drop(lease);
        s.check()?;
        Ok(s)
    }
    fn check_held(&self) -> Result<()> {
        if self.core.store.setting("privacy") == true {
            return Err("PRIVACY_DENIED".into());
        }
        if !self.core.enabled("drive") {
            return Err("MODULE_DISABLED: drive".into());
        }
        if record(&self.core, &self.id)? != self.config
            || self
                .core
                .store
                .setting(&format!("drive.connection_generation.{}", self.id))
                != self.generation
        {
            return Err("DRIVE_CONNECTION_CHANGED".into());
        }
        #[cfg(test)]
        {
            let current = self
                .core
                .drive_fixture
                .lock()
                .map_err(|_| "DRIVE_FIXTURE_LOCK")?;
            let same = match (current.as_ref(), self.fixture.as_ref()) {
                (None, None) => true,
                (Some(a), Some(b)) => {
                    a.token_url == b.token_url
                        && a.api_base == b.api_base
                        && a.timeout_ms == b.timeout_ms
                        && a.native_storage == b.native_storage
                        && a.headless_permission == b.headless_permission
                        && Arc::ptr_eq(&a.vault, &b.vault)
                }
                _ => false,
            };
            if !same {
                return Err("DRIVE_FIXTURE_CHANGED".into());
            }
        }
        Ok(())
    }
    pub(crate) fn check(&self) -> Result<()> {
        let _lease = integrations::save_lock(&self.core)?;
        self.check_held()
    }
    fn local_origin_held(&self) -> Result<Value> {
        let profile = self.core.store.setting(PROFILE_KEY);
        canonical(profile.as_str().ok_or("DRIVE_PROFILE_IDENTITY_REQUIRED")?)?;
        let root = asset_files::identity(&asset_files::directory(&self.core.root)?)?;
        Ok(json!({"version":1,"connection":self.id,"profile":profile,"root":root}))
    }
    /// Local file ownership survives consent/client replacement. No vault read.
    pub(crate) fn local_origin(&self) -> Result<Value> {
        let _lease = integrations::save_lock(&self.core)?;
        self.check_held()?;
        self.local_origin_held()
    }
    pub(crate) fn local_guard<T>(
        &self,
        origin: &Value,
        f: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let _lease = integrations::save_lock(&self.core)?;
        self.check_held()?;
        if self.local_origin_held()? != *origin {
            return Err("DRIVE_DOWNLOAD_PROFILE_CHANGED".into());
        }
        let result = f()?;
        self.check_held()?;
        Ok(result)
    }
    pub(crate) async fn guard<T>(&self, f: impl Future<Output = Result<T>>) -> Result<T> {
        self.check()?;
        tokio::pin!(f);
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        loop {
            tokio::select! {biased;_=tick.tick()=>self.check()?,r=&mut f=>{self.check()?;return r;}}
        }
    }
    async fn attempt_guard<T>(&self, nonce: &str, f: impl Future<Output = Result<T>>) -> Result<T> {
        check_attempt(self, nonce)?;
        tokio::pin!(f);
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        loop {
            tokio::select! {biased;_=tick.tick()=>check_attempt(self,nonce)?,r=&mut f=>{check_attempt(self,nonce)?;return r;}}
        }
    }
    fn private<T>(&self, f: impl FnOnce(&vault::Binding) -> Result<T>) -> Result<T> {
        let _lease = integrations::save_lock(&self.core)?;
        self.check_held()?;
        let b = binding(&self.core, &self.id, &self.config, false)?;
        let r = f(&b)?;
        self.check_held()?;
        Ok(r)
    }
    fn observe(&self) -> Result<Option<vault::Observation>> {
        self.private(|b| storage_observe(&self.core, b))
    }
    fn envelope(&self) -> Result<(Uuid, Envelope)> {
        self.private(|b| {
            let (id, bytes) = storage_read(&self.core, b)?;
            let env: Envelope =
                serde_json::from_slice(bytes.bytes()).map_err(|_| "DRIVE_ENVELOPE_INVALID")?;
            env.validate(&self.config)?;
            Ok((id, env))
        })
    }
    pub(crate) fn allows(&self, key: &str, id: &str) -> Result<()> {
        resource_id(id)?;
        if self.config[key]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == id))
        {
            Ok(())
        } else {
            Err("DRIVE_RESOURCE_OUT_OF_SCOPE".into())
        }
    }
    fn capability_descriptor_held(&self, job: &str, authorization: &Value) -> Result<Value> {
        canonical(job)?;
        if job == self.id {
            return Err("DRIVE_CAPABILITY_NAMESPACE".into());
        }
        let auth = authorization
            .as_str()
            .ok_or("DRIVE_CLEANUP_AUTHORIZATION_BINDING")?;
        if auth.len() != 64
            || !auth
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("DRIVE_CLEANUP_AUTHORIZATION_BINDING".into());
        }
        let profile = self.core.store.setting(PROFILE_KEY);
        canonical(profile.as_str().ok_or("DRIVE_PROFILE_IDENTITY")?)?;
        let root = asset_files::identity(&asset_files::directory(&self.core.root)?)?;
        Ok(
            json!({"version":1,"namespace":"resumable-session-v1","job":job,"connection":self.id,"authorization":authorization,"profile":profile,"root":root,"client_id":self.config["client_id"],"client_secret_mode":self.config["client_secret_mode"]}),
        )
    }
    pub(crate) fn capability_descriptor(&self, job: &str, authorization: &Value) -> Result<Value> {
        self.private(|_| self.capability_descriptor_held(job, authorization))
    }
    fn historical_binding_held(&self, job: &str, immutable: &Value) -> Result<vault::Binding> {
        let current = self.capability_descriptor_held(job, &immutable["authorization_binding"])?;
        let descriptor = match immutable.get("capability_binding").filter(|v| !v.is_null()) {
            Some(value) => value.clone(),
            None if immutable["profile_fingerprint"] == crate::drive::digest(&self.config) => {
                current.clone()
            }
            None => return Err("DRIVE_CLEANUP_LEGACY_PROFILE_CHANGED".into()),
        };
        if descriptor.as_object().is_none_or(|o| o.len() != 9)
            || [
                "version",
                "namespace",
                "job",
                "connection",
                "authorization",
                "profile",
                "root",
            ]
            .iter()
            .any(|k| descriptor[*k] != current[*k])
        {
            return Err("DRIVE_CLEANUP_HISTORICAL_BINDING".into());
        }
        validate_config(
            &json!({"auth":"installed_app_pkce","client_id":descriptor["client_id"],"client_secret_mode":descriptor["client_secret_mode"],"allowed_files":[],"allowed_folders":[]}),
        )?;
        let mut bound = descriptor.clone();
        bound
            .as_object_mut()
            .ok_or("DRIVE_CLEANUP_HISTORICAL_BINDING")?
            .remove("version");
        vault::Binding::new(
            job,
            descriptor["profile"]
                .as_str()
                .ok_or("DRIVE_PROFILE_IDENTITY")?,
            Sha256::digest(bound.to_string().as_bytes()).into(),
        )
        .map_err(str::to_string)
    }
    fn historical_snapshot_held(
        &self,
        job: &str,
        immutable: &Value,
        binding: &vault::Binding,
    ) -> Result<Option<Uuid>> {
        let Some(observed) = storage_observe(&self.core, binding)? else {
            return Ok(None);
        };
        if !binding.matches_client(&observed) {
            return Err("DRIVE_CLEANUP_HISTORICAL_BINDING".into());
        }
        let (version, bytes) = storage_read(&self.core, binding)?;
        if version != observed.version {
            return Err("DRIVE_CLEANUP_CAPABILITY_CHANGED_OR_MISSING".into());
        }
        let value: Value =
            serde_json::from_slice(bytes.bytes()).map_err(|_| "DRIVE_CLEANUP_PRIVATE_FORMAT")?;
        if value.as_object().is_none_or(|o| o.len() != 6)
            || value["version"] != 1
            || value["job"] != job
            || value["immutable_sha256"] != crate::drive::digest(immutable)
            || value["authorization_binding"] != immutable["authorization_binding"]
            || value["created_at"].as_i64().is_none()
            || value["uri"]
                .as_str()
                .is_none_or(|s| s.is_empty() || s.len() > 8192)
        {
            return Err("DRIVE_CLEANUP_PRIVATE_BINDING".into());
        }
        Ok(Some(version))
    }
    pub(crate) fn historical_capability_inspect(
        &self,
        job: &str,
        immutable: &Value,
    ) -> Result<Option<Uuid>> {
        self.private(|_| {
            let b = self.historical_binding_held(job, immutable)?;
            self.historical_snapshot_held(job, immutable, &b)
        })
    }
    pub(crate) fn historical_capability_retire(
        &self,
        job: &str,
        immutable: &Value,
        expected: Uuid,
    ) -> Result<bool> {
        self.private(|_| {
            let b = self.historical_binding_held(job, immutable)?;
            if self.historical_snapshot_held(job, immutable, &b)? != Some(expected) {
                return Err("DRIVE_CLEANUP_CAPABILITY_CHANGED_OR_MISSING".into());
            }
            storage_erase_capability(
                &self.core,
                &b,
                expected,
                receipt_context(&json!(crate::drive::digest(immutable)))?,
            )
        })
    }
    pub(crate) fn historical_receipts(
        &self,
        job: &str,
        immutable: &Value,
    ) -> Result<Vec<vault::Recovery>> {
        self.private(|_| {
            let b = self.historical_binding_held(job, immutable)?;
            storage_recoveries(
                &self.core,
                &b,
                receipt_context(&json!(crate::drive::digest(immutable)))?,
            )
        })
    }
    #[cfg(test)]
    pub(crate) fn lose_owned_capability_pointer(&self, job: &str, im: &Value) -> Result<()> {
        let f = self
            .fixture
            .as_ref()
            .ok_or("OWNED_MEMORY_FIXTURE_REQUIRED")?;
        if f.native_storage
            || self.config["client_id"] != "owned-fixture.apps.googleusercontent.com"
        {
            return Err("OWNED_MEMORY_FIXTURE_REQUIRED".into());
        }
        self.private(|_| {
            let b = self.historical_binding_held(job, im)?;
            f.vault
                .lock()
                .map_err(|_| "OWNED_MEMORY_LOCK")?
                .lose_owned_pointer(&b)
                .map_err(str::to_string)
        })
    }
    pub(crate) fn historical_receipt_retire(
        &self,
        job: &str,
        immutable: &Value,
        expected: &vault::Recovery,
    ) -> Result<bool> {
        self.private(|_| {
            let b = self.historical_binding_held(job, immutable)?;
            storage_recover(
                &self.core,
                &b,
                receipt_context(&json!(crate::drive::digest(immutable)))?,
                expected,
            )
        })
    }
    fn client(&self) -> Result<reqwest::Client> {
        crate::http_client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| "DRIVE_HTTP_CLIENT".into())
    }
    fn token_url(&self) -> Result<reqwest::Url> {
        #[cfg(test)]
        if let Some(f) = &self.fixture {
            return fixture_url(&f.token_url, "/token");
        }
        reqwest::Url::parse(TOKEN_URL).map_err(|_| "DRIVE_TOKEN_ORIGIN".into())
    }
    pub(crate) fn api_base(&self) -> Result<reqwest::Url> {
        #[cfg(test)]
        if let Some(f) = &self.fixture {
            return fixture_url(&f.api_base, "/");
        }
        reqwest::Url::parse("https://www.googleapis.com").map_err(|_| "DRIVE_API_ORIGIN".into())
    }
    async fn renewal_lease(&self) -> Result<Vec<std::fs::File>> {
        self.guard(async {
            let until = tokio::time::Instant::now() + Duration::from_secs(25);
            loop {
                match renewal_file(&self.core, &self.id) {
                    Err(e) if e == "DRIVE_AUTH_BUSY" && tokio::time::Instant::now() < until => {
                        tokio::time::sleep(Duration::from_millis(50)).await
                    }
                    r => return r,
                }
            }
        })
        .await
    }
}
#[cfg(test)]
fn fixture_url(url: &str, path: &str) -> Result<reqwest::Url> {
    let u = reqwest::Url::parse(url).map_err(|_| "DRIVE_FIXTURE_ORIGIN")?;
    if u.scheme() != "http"
        || u.host_str() != Some("127.0.0.1")
        || u.port().is_none()
        || u.path() != path
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return Err("DRIVE_FIXTURE_ORIGIN".into());
    }
    Ok(u)
}
#[cfg(windows)]
fn renewal_file(c: &Core, id: &str) -> Result<Vec<std::fs::File>> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    canonical(id)?;
    let root = asset_files::directory(&c.root)?;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .custom_flags(0x00200000)
        .open(c.root.join(format!("drive-auth-{id}.lock")))
        .map_err(|e| {
            if matches!(e.raw_os_error(), Some(32 | 33)) {
                "DRIVE_AUTH_BUSY"
            } else {
                "DRIVE_AUTH_LEASE"
            }
        })?;
    let m = f.metadata().map_err(|_| "DRIVE_AUTH_LEASE")?;
    if !m.is_file() || m.len() != 0 || m.file_attributes() & 0x400 != 0 {
        return Err("DRIVE_AUTH_LEASE_UNSAFE".into());
    }
    asset_files::identity(&f)?;
    Ok(vec![root, f])
}
#[cfg(not(windows))]
fn renewal_file(_: &Core, _: &str) -> Result<Vec<std::fs::File>> {
    Err("WINDOWS_11_REQUIRED".into())
}
fn state_key(id: &str) -> String {
    format!("drive.auth_state.{id}")
}
fn mark(scope: &Scope, nonce: &str, phase: &str, error: Option<&str>) -> Result<Value> {
    let _lease = integrations::save_lock(&scope.core)?;
    scope.check_held()?;
    mark_held(scope, nonce, phase, error)
}
fn mark_attempt(scope: &Scope, nonce: &str, phase: &str) -> Result<Value> {
    let _lease = integrations::save_lock(&scope.core)?;
    check_attempt_held(scope, nonce)?;
    mark_held(scope, nonce, phase, None)
}
fn mark_held(scope: &Scope, nonce: &str, phase: &str, error: Option<&str>) -> Result<Value> {
    let value = json!({"phase":phase,"session_id":nonce,"generation":scope.generation,"error":error,"at":storage::now()});
    scope
        .core
        .store
        .set_setting(&state_key(&scope.id), &value)?;
    Ok(value)
}
fn finish(
    scope: &Scope,
    expected: &Value,
    phase: &str,
    error: Option<&str>,
    receipt: Option<&vault::Receipt>,
) -> Result<()> {
    let value = json!({"phase":phase,"session_id":expected["session_id"],"generation":scope.generation,"error":error,
        "credential_version":receipt.map(|r|r.version.to_string()),"cleanup_pending":receipt.is_some_and(|r|!r.cleanup_complete),"at":storage::now()});
    // Never overwrite a new attempt, changed-client reset or another actor's
    // status after cancellation/dispatch. Tokens and descriptions are absent.
    let changed = scope.core.store.execute(
        "UPDATE settings SET json_value=? WHERE key=? AND json_value=?",
        &[
            json!(value.to_string()),
            json!(state_key(&scope.id)),
            json!(expected.to_string()),
        ],
    )?;
    if changed != 1 {
        return Err("DRIVE_AUTH_STATE_CHANGED".into());
    }
    Ok(())
}
fn ready(scope: &Scope, version: Uuid) -> Result<()> {
    let state = scope.core.store.setting(&state_key(&scope.id));
    if state["phase"] != "ready" || state["credential_version"] != version.to_string() {
        return Err("DRIVE_REAUTHORIZE_REQUIRED".into());
    }
    Ok(())
}
fn parse_tokens(
    body: &Value,
    config: &Value,
    mut env: Envelope,
    refresh: bool,
) -> Result<Envelope> {
    if !body.is_object() || body["token_type"] != "Bearer" {
        return Err("DRIVE_TOKEN_RESPONSE_INVALID".into());
    }
    let token = Text::from_str(
        body["access_token"]
            .as_str()
            .ok_or("DRIVE_TOKEN_RESPONSE_INVALID")?,
        2048,
    )?;
    let lifetime = body["expires_in"]
        .as_i64()
        .filter(|n| *n > 0 && *n <= 604800)
        .ok_or("DRIVE_TOKEN_EXPIRY_INVALID")?;
    let scopes = if refresh && body.get("scope").is_none() {
        env.granted_scopes.clone()
    } else {
        let text = body["scope"]
            .as_str()
            .filter(|s| s.len() <= 4096)
            .ok_or("DRIVE_TOKEN_SCOPE_INVALID")?;
        text.split(' ')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    };
    if scopes != [DRIVE_SCOPE] {
        return Err("DRIVE_TOKEN_SCOPE_INVALID".into());
    }
    let refresh_token = if let Some(s) = body.get("refresh_token") {
        Some(Text::from_str(
            s.as_str().ok_or("DRIVE_REFRESH_TOKEN_INVALID")?,
            512,
        )?)
    } else if refresh {
        env.refresh_token.take()
    } else {
        None
    };
    if refresh_token.is_none() {
        return Err("DRIVE_FRESH_REFRESH_TOKEN_REQUIRED".into());
    }
    let now = chrono::Utc::now().timestamp();
    env.access_token = Some(token);
    if !refresh {
        env.authorization_id = Some(storage::id());
    }
    env.refresh_token = refresh_token;
    env.granted_scopes = scopes;
    env.issued_at = Some(now);
    env.expires_at = Some(
        now.checked_add(lifetime)
            .ok_or("DRIVE_TOKEN_EXPIRY_INVALID")?,
    );
    env.validate(config)?;
    Ok(env)
}
async fn token_request(
    scope: &Scope,
    fields: &[(&str, &str)],
    sent: &AtomicBool,
    attempt: Option<&str>,
) -> Result<Value> {
    let request = scope.client()?.post(scope.token_url()?).form(fields);
    let work = async {
        sent.store(true, Ordering::SeqCst);
        let response = request
            .send()
            .await
            .map_err(|_| "DRIVE_TOKEN_OUTCOME_UNCERTAIN")?;
        let status = response.status().as_u16();
        if response.headers().contains_key("dpop-nonce") {
            return Err("DRIVE_DPOP_UNSUPPORTED".into());
        }
        let mime = response
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("application/json");
        let bytes = integrations::bounded_body(response, 16384)
            .await
            .map_err(|_| "DRIVE_TOKEN_OUTCOME_UNCERTAIN")?;
        let body: Value =
            serde_json::from_slice(&bytes).map_err(|_| "DRIVE_TOKEN_OUTCOME_UNCERTAIN")?;
        if !(200..300).contains(&status) {
            return Err(match (status, body["error"].as_str()) {
                (400 | 401, Some("invalid_grant")) => "DRIVE_REAUTHORIZE_REQUIRED",
                (400 | 401, Some("invalid_client")) => "DRIVE_CLIENT_REJECTED",
                (400, Some("invalid_request" | "unsupported_grant_type" | "invalid_scope")) => {
                    "DRIVE_AUTH_REQUEST_REJECTED"
                }
                (429, _) => "DRIVE_AUTH_RATE_LIMITED",
                _ => "DRIVE_TOKEN_OUTCOME_UNCERTAIN",
            }
            .into());
        }
        if !mime {
            return Err("DRIVE_TOKEN_OUTCOME_UNCERTAIN".into());
        }
        Ok(body)
    };
    if let Some(nonce) = attempt {
        scope.attempt_guard(nonce, work).await
    } else {
        scope.guard(work).await
    }
}
pub(crate) struct Grant {
    pub(crate) scope: Scope,
    token: Text,
    version: Uuid,
    authorization_id: Option<String>,
}
impl Grant {
    pub(crate) fn authorization_binding(&self) -> Result<String> {
        self.check()?;
        let id = self
            .authorization_id
            .as_ref()
            .ok_or("DRIVE_REAUTHORIZE_FOR_UPLOAD_IDENTITY")?;
        Ok(format!(
            "{:x}",
            Sha256::digest(
                format!("AlfredPet/Drive/authorization/{}/{id}", self.scope.id).as_bytes()
            )
        ))
    }
    pub(crate) fn capability_descriptor(&self, job: &str) -> Result<Value> {
        self.scope
            .capability_descriptor(job, &json!(self.authorization_binding()?))
    }
    fn capability_binding(&self, job: &str) -> Result<vault::Binding> {
        canonical(job)?;
        if job == self.scope.id {
            return Err("DRIVE_CAPABILITY_NAMESPACE".into());
        }
        let profile = self.scope.core.store.setting(PROFILE_KEY);
        let profile = profile.as_str().ok_or("DRIVE_PROFILE_IDENTITY")?;
        let root = asset_files::identity(&asset_files::directory(&self.scope.core.root)?)?;
        let digest = Sha256::digest(json!({"namespace":"resumable-session-v1","job":job,"connection":self.scope.id,"authorization":self.authorization_binding()?,"profile":profile,"root":root,"client_id":self.scope.config["client_id"],"client_secret_mode":self.scope.config["client_secret_mode"]}).to_string().as_bytes());
        vault::Binding::new(job, profile, digest.into()).map_err(str::to_string)
    }
    pub(crate) fn capability_save(&self, job: &str, value: &Value) -> Result<Uuid> {
        self.check()?;
        let b = self.capability_binding(job)?;
        let bytes =
            vault::Secret::copy(&serde_json::to_vec(value).map_err(|_| "DRIVE_CAPABILITY_ENCODE")?);
        let receipt = self.scope.private(|_| {
            storage_save_capability(
                &self.scope.core,
                &b,
                bytes.bytes(),
                receipt_context(&value["immutable_sha256"])?,
            )
        })?;
        self.check()?;
        Ok(receipt.version)
    }
    pub(crate) fn capability_read(&self, job: &str, expected: Uuid) -> Result<Value> {
        self.check()?;
        let b = self.capability_binding(job)?;
        self.scope.private(|_| {
            let (version, bytes) = storage_read(&self.scope.core, &b)?;
            if version != expected {
                return Err("DRIVE_CAPABILITY_CHANGED".into());
            }
            serde_json::from_slice(bytes.bytes()).map_err(|_| "DRIVE_CAPABILITY_DECODE".into())
        })
    }
    pub(crate) fn capability_observe(&self, job: &str) -> Result<Option<Uuid>> {
        self.check()?;
        let b = self.capability_binding(job)?;
        self.scope
            .private(|_| Ok(storage_observe(&self.scope.core, &b)?.map(|o| o.version)))
    }
    pub(crate) fn capability_delete(&self, job: &str, expected: Uuid) -> Result<bool> {
        self.check()?;
        let b = self.capability_binding(job)?;
        self.scope.private(|_| {
            let (version, bytes) = storage_read(&self.scope.core, &b)?;
            if version != expected {
                return Err("DRIVE_CAPABILITY_CHANGED".into());
            }
            let value: Value =
                serde_json::from_slice(bytes.bytes()).map_err(|_| "DRIVE_CAPABILITY_DECODE")?;
            storage_erase_capability(
                &self.scope.core,
                &b,
                expected,
                receipt_context(&value["immutable_sha256"])?,
            )
        })
    }
    /// All paths stay on the fixed API origin; bearer and private session URI
    /// are never passed to product input, output or persistent audit.
    pub(crate) async fn request_bytes(
        &self,
        method: reqwest::Method,
        path: &str,
        headers: &[(&str, String)],
        bytes: Vec<u8>,
        dispatched: &AtomicBool,
    ) -> Result<reqwest::Response> {
        self.check()?;
        let url = integrations::scoped_url(self.scope.api_base()?.as_str(), path)?;
        let mut request = self
            .scope
            .client()?
            .request(method, url)
            .bearer_auth(self.token.text());
        for (key, value) in headers {
            request = request.header(*key, value);
        }
        request = request.body(bytes);
        self.guard(async {
            self.check()?;
            dispatched.store(true, Ordering::SeqCst);
            request.send().await.map_err(|_| "DRIVE_API_NETWORK".into())
        })
        .await
    }
    pub(crate) fn check(&self) -> Result<()> {
        self.scope.check()?;
        let valid = self.scope.private(|b| {
            Ok(storage_observe(&self.scope.core, b)?
                .is_some_and(|o| o.version == self.version && b.matches_client(&o)))
        })?;
        if !valid {
            return Err("DRIVE_CREDENTIAL_CHANGED".into());
        }
        ready(&self.scope, self.version)
    }
    pub(crate) async fn guard<T>(&self, f: impl Future<Output = Result<T>>) -> Result<T> {
        self.check()?;
        tokio::pin!(f);
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        loop {
            tokio::select! {biased;_=tick.tick()=>self.check()?,r=&mut f=>{self.check()?;return r;}}
        }
    }
    pub(crate) async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<reqwest::Response> {
        self.check()?;
        let url = integrations::scoped_url(self.scope.api_base()?.as_str(), path)?;
        let mut request = self
            .scope
            .client()?
            .request(method, url)
            .bearer_auth(self.token.text());
        if let Some(body) = body {
            request = request.json(body);
        }
        self.guard(async {
            self.check()?;
            let response = request.send().await.map_err(|_| "DRIVE_API_NETWORK")?;
            self.check()?;
            Ok(response)
        })
        .await
    }
}
pub(crate) async fn grant(c: &Arc<Core>, id: &str) -> Result<Grant> {
    let scope = Scope::new(c, id)?;
    let _renew = scope.renewal_lease().await?;
    let (version, env) = scope.envelope().map_err(|e| {
        if e == "DRIVE_VAULT_NOT_FOUND" {
            "DRIVE_OAUTH_REQUIRED".into()
        } else {
            e
        }
    })?;
    ready(&scope, version)?;
    let issued = env.issued_at.ok_or("DRIVE_OAUTH_REQUIRED")?;
    let expiry = env.expires_at.ok_or("DRIVE_OAUTH_REQUIRED")?;
    let now = chrono::Utc::now().timestamp();
    if now < issued - 5 {
        return Err("DRIVE_TOKEN_CLOCK_CHANGED".into());
    }
    let margin = ((expiry - issued) / 10).clamp(1, 60);
    if expiry > now + margin {
        return Ok(Grant {
            scope,
            version,
            authorization_id: env.authorization_id,
            token: env.access_token.ok_or("DRIVE_OAUTH_REQUIRED")?,
        });
    }
    let nonce = storage::id();
    let expected = mark(&scope, &nonce, "refresh_in_flight", None)?;
    let sent = AtomicBool::new(false);
    let result: Result<Grant> = async {
        let mut fields = vec![
            ("client_id", env.client_id.as_str()),
            ("grant_type", "refresh_token"),
            (
                "refresh_token",
                env.refresh_token
                    .as_ref()
                    .ok_or("DRIVE_OAUTH_REQUIRED")?
                    .text(),
            ),
        ];
        if let Some(secret) = &env.client_secret {
            fields.push(("client_secret", secret.text()));
        }
        let body = token_request(&scope, &fields, &sent, None).await?;
        let env = parse_tokens(&body, &scope.config, env, true)?;
        let bytes = env.bytes()?;
        let receipt = scope.private(|b| storage_replace(c, b, version, bytes.bytes()))?;
        finish(&scope, &expected, "ready", None, Some(&receipt))?;
        let token = env.access_token.ok_or("DRIVE_TOKEN_RESPONSE_INVALID")?;
        Ok(Grant {
            scope: scope.clone(),
            version: receipt.version,
            authorization_id: env.authorization_id,
            token,
        })
    }
    .await;
    if let Err(e) = &result {
        finish(
            &scope,
            &expected,
            if sent.load(Ordering::SeqCst) {
                "uncertain"
            } else {
                "reauthorize"
            },
            Some(e),
            None,
        )?;
    }
    result
}

pub(crate) async fn start(c: &Arc<Core>, v: &Value) -> Result<Value> {
    let schema = json!({"type":"object","required":["connection"],"properties":{"connection":{"type":"string"},"pick_resources":{"type":"boolean"}},"additionalProperties":false});
    if !jsonschema::validator_for(&schema)
        .map_err(|_| "DRIVE_AUTH_INPUT")?
        .is_valid(v)
    {
        return Err("DRIVE_AUTH_INPUT".into());
    }
    let scope = Scope::new(c, v["connection"].as_str().ok_or("DRIVE_CONNECTION_ID")?)?;
    let nonce = storage::id();
    {
        let _lease = integrations::save_lock(c)?;
        scope.check_held()?;
        c.store
            .set_setting(&format!("drive.auth_attempt.{}", scope.id), &json!(nonce))?;
        // Quarantine the previous grant before waiting on another actor's
        // renewal lease or launching the browser. Old finishes cannot publish
        // ready over this new public nonce.
        mark_held(&scope, &nonce, "authorization_pending", None)?;
    }
    let result = start_attempt(scope.clone(), nonce.clone(), v["pick_resources"] == true).await;
    if let Err(error) = &result {
        let current = c.store.setting(&state_key(&scope.id));
        if current["session_id"] == nonce
            && [
                "authorization_pending",
                "browser_launch_in_flight",
                "authorizing",
            ]
            .contains(&current["phase"].as_str().unwrap_or(""))
        {
            let phase = if current["phase"] == "browser_launch_in_flight"
                || error == "DRIVE_SYSTEM_BROWSER_OPEN_UNCERTAIN"
            {
                "uncertain"
            } else {
                "reauthorize"
            };
            let _ = finish(&scope, &current, phase, Some(error), None);
        }
    }
    result
}
async fn start_attempt(scope: Scope, nonce: String, picker: bool) -> Result<Value> {
    // A new attempt invalidates the older cross-process listener via its nonce.
    let renew = scope.attempt_guard(&nonce, scope.renewal_lease()).await?;
    check_attempt(&scope, &nonce)?;
    let observed = scope.observe()?;
    let env = if scope.config["client_secret_mode"] == "credential_manager" {
        let (_, env) = scope.envelope().map_err(|e| {
            if e == "DRIVE_VAULT_NOT_FOUND" {
                "DRIVE_CLIENT_SECRET_REQUIRED".into()
            } else {
                e
            }
        })?;
        if env.client_secret.is_none() {
            return Err("DRIVE_CLIENT_SECRET_REQUIRED".into());
        }
        Envelope::empty(&scope.config, env.client_secret)
    } else {
        Envelope::empty(&scope.config, None)
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| "DRIVE_CALLBACK_BIND")?;
    let path = format!("/oauth/{}", storage::id());
    let port = listener
        .local_addr()
        .map_err(|_| "DRIVE_CALLBACK_BIND")?
        .port();
    let redirect = format!("http://127.0.0.1:{port}{path}");
    let state = Text::from_str(&format!("{}{}", storage::id(), storage::id()), 128)?;
    let verifier = Text::from_str(&format!("{}{}", storage::id(), storage::id()), 128)?;
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.text().as_bytes()));
    let mut url = reqwest::Url::parse(AUTH_URL).map_err(|_| "DRIVE_AUTH_ORIGIN")?;
    url.query_pairs_mut()
        .append_pair("client_id", &env.client_id)
        .append_pair("redirect_uri", &redirect)
        .append_pair("response_type", "code")
        .append_pair("scope", DRIVE_SCOPE)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("state", state.text())
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    if picker {
        url.query_pairs_mut()
            .append_pair("trigger_onepick", "true")
            .append_pair("allow_multiple", "true")
            .append_pair("allow_folder_selection", "true");
    }
    #[cfg(test)]
    let fixture = scope.fixture.is_some();
    #[cfg(not(test))]
    let fixture = false;
    if !fixture {
        let launch_url = url.to_string();
        mark_attempt(&scope, &nonce, "browser_launch_in_flight")?;
        scope
            .attempt_guard(&nonce, async {
                tokio::time::timeout(
                    Duration::from_secs(5),
                    tokio::task::spawn_blocking(move || open_system_browser(&launch_url)),
                )
                .await
                .map_err(|_| "DRIVE_SYSTEM_BROWSER_OPEN_UNCERTAIN")?
                .map_err(|_| "DRIVE_SYSTEM_BROWSER_OPEN_UNCERTAIN")?
            })
            .await?;
    }
    mark_attempt(&scope, &nonce, "authorizing")?;
    let response = json!({"session_id":nonce,"system_browser_launched":!fixture,"expires_in":180});
    #[cfg(test)]
    let response = {
        let mut r = response;
        r["fixture_auth_url"] = json!(url.as_str());
        r
    };
    tokio::spawn(async move {
        let _renew = renew;
        let timeout = Duration::from_secs(180);
        #[cfg(test)]
        let timeout = scope
            .fixture
            .as_ref()
            .map_or(timeout, |f| Duration::from_millis(f.timeout_ms));
        let attempt = Attempt {
            scope: scope.clone(),
            nonce: nonce.clone(),
            path,
            port,
            redirect,
            verifier,
            state,
            env,
            observed,
            picker,
        };
        let result = tokio::time::timeout(timeout, authorize_callback(attempt, listener)).await;
        let result = result.unwrap_or_else(|_| Err("DRIVE_AUTH_TIMEOUT".into()));
        if let Err(e) = &result {
            let current = scope.core.store.setting(&state_key(&scope.id));
            if current["session_id"] == nonce
                && ["authorizing", "exchange_in_flight", "ready"]
                    .contains(&current["phase"].as_str().unwrap_or(""))
            {
                let phase = if ["exchange_in_flight", "ready"]
                    .contains(&current["phase"].as_str().unwrap_or(""))
                {
                    "uncertain"
                } else {
                    "reauthorize"
                };
                let _ = finish(&scope, &current, phase, Some(e), None);
            }
        }
        scope.core.emit("alfredpet:oauth",&json!({"connection":scope.id,"session_id":nonce,"ok":result.is_ok(),"error":result.err()}));
    });
    Ok(response)
}
fn check_attempt(scope: &Scope, nonce: &str) -> Result<()> {
    scope.check()?;
    if scope
        .core
        .store
        .setting(&format!("drive.auth_attempt.{}", scope.id))
        != nonce
    {
        return Err("DRIVE_AUTH_SUPERSEDED".into());
    }
    Ok(())
}
struct Attempt {
    scope: Scope,
    nonce: String,
    path: String,
    port: u16,
    redirect: String,
    verifier: Text,
    state: Text,
    env: Envelope,
    observed: Option<vault::Observation>,
    picker: bool,
}
async fn authorize_callback(attempt: Attempt, listener: tokio::net::TcpListener) -> Result<()> {
    let Attempt {
        scope,
        nonce,
        path,
        port,
        redirect,
        verifier,
        state,
        env,
        observed,
        picker,
    } = attempt;
    let (scope, nonce, path, redirect) = (&scope, nonce.as_str(), path.as_str(), redirect.as_str());
    let mut accepted = 0;
    loop {
        check_attempt(scope, nonce)?;
        let (mut socket, peer) = scope
            .attempt_guard(nonce, listener.accept().map_error())
            .await?;
        if !peer.ip().is_loopback() {
            return Err("DRIVE_CALLBACK_PEER".into());
        }
        accepted += 1;
        if accepted > 16 {
            return Err("DRIVE_CALLBACK_PROBE_LIMIT".into());
        }
        let mut data = Vec::new();
        let parsed = scope
            .attempt_guard(nonce, async {
                tokio::time::timeout(Duration::from_secs(2), async {
                    let mut buf = [0u8; 1024];
                    while !data.windows(4).any(|b| b == b"\r\n\r\n") {
                        let n = socket
                            .read(&mut buf)
                            .await
                            .map_err(|_| "DRIVE_CALLBACK_READ")?;
                        if n == 0 {
                            return Err("DRIVE_CALLBACK_INCOMPLETE".into());
                        }
                        data.extend_from_slice(&buf[..n]);
                        if data.len() > 8192 {
                            return Err("DRIVE_CALLBACK_LIMIT".into());
                        }
                    }
                    let code = parse_callback(&data, path, port, state.text())?;
                    let selected = picked_resources(&data, port, picker)?;
                    Ok::<_, String>((code, selected))
                })
                .await
                .map_err(|_| "DRIVE_CALLBACK_READ_TIMEOUT")?
            })
            .await;
        let (code, selected) = match parsed {
            Ok(code) => code,
            Err(e) if e.starts_with("DRIVE_CALLBACK_") => {
                let _ = reply(&mut socket, false).await;
                continue;
            }
            Err(e) => {
                let _ = reply(&mut socket, false).await;
                return Err(e);
            }
        };
        check_attempt(scope, nonce)?;
        let expected = mark_attempt(scope, nonce, "exchange_in_flight")?;
        let sent = AtomicBool::new(false);
        let result: Result<()> = async {
            let mut fields = vec![
                ("client_id", env.client_id.as_str()),
                ("grant_type", "authorization_code"),
                ("code", code.text()),
                ("redirect_uri", redirect),
                ("code_verifier", verifier.text()),
            ];
            if let Some(secret) = &env.client_secret {
                fields.push(("client_secret", secret.text()));
            }
            let body = token_request(scope, &fields, &sent, Some(nonce)).await?;
            check_attempt(scope, nonce)?;
            let env = parse_tokens(&body, &scope.config, env, false)?;
            let bytes = env.bytes()?;
            scope.private(|b| {
                check_attempt_held(scope, nonce)?;
                let receipt = storage_save(&scope.core, b, observed.as_ref(), bytes.bytes())?;
                check_attempt_held(scope, nonce)?;
                // Cooperating cancel/start/save actors cannot interleave
                // between private pointer publication and ready publication.
                finish(scope, &expected, "ready", None, Some(&receipt))?;
                let mut public = scope.core.store.setting(&state_key(&scope.id));
                public["picked_file_ids"] = json!(selected);
                public["picked_ids_require_explicit_local_scope"] = json!(true);
                scope
                    .core
                    .store
                    .set_setting(&state_key(&scope.id), &public)?;
                Ok(())
            })?;
            Ok(())
        }
        .await;
        // Tokens are committed even if a browser disconnects during this bounded
        // reply. The storage result is authoritative, not the TCP success page.
        let _ = reply(&mut socket, result.is_ok()).await;
        return result;
    }
}
fn check_attempt_held(scope: &Scope, nonce: &str) -> Result<()> {
    scope.check_held()?;
    if scope
        .core
        .store
        .setting(&format!("drive.auth_attempt.{}", scope.id))
        != nonce
    {
        return Err("DRIVE_AUTH_SUPERSEDED".into());
    }
    Ok(())
}
// A small adapter for a socket accept's io::Error; never expose socket text.
trait AcceptFuture: Sized {
    fn map_error(
        self,
    ) -> impl Future<Output = Result<(tokio::net::TcpStream, std::net::SocketAddr)>>;
}
impl<F: Future<Output = std::io::Result<(tokio::net::TcpStream, std::net::SocketAddr)>>>
    AcceptFuture for F
{
    async fn map_error(self) -> Result<(tokio::net::TcpStream, std::net::SocketAddr)> {
        self.await.map_err(|_| "DRIVE_CALLBACK_ACCEPT".into())
    }
}
fn parse_callback(bytes: &[u8], path: &str, port: u16, nonce: &str) -> Result<Text> {
    if bytes.len() > 8192 || !bytes.ends_with(b"\r\n\r\n") || bytes.contains(&0) {
        return Err("DRIVE_CALLBACK_FORMAT".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "DRIVE_CALLBACK_FORMAT")?;
    let mut lines = text[..text.len() - 4].split("\r\n");
    let request = lines.next().ok_or("DRIVE_CALLBACK_FORMAT")?;
    let fields: Vec<_> = request.split(' ').collect();
    if fields.len() != 3
        || fields[0] != "GET"
        || fields[2] != "HTTP/1.1"
        || !fields[1].starts_with(&format!("{path}?"))
        || fields[1].contains('#')
        || fields[1].contains('\\')
        || fields[1].bytes().any(|b| b.is_ascii_control())
    {
        return Err("DRIVE_CALLBACK_ROUTE".into());
    }
    let mut host = false;
    for line in lines {
        let (key, value) = line.split_once(':').ok_or("DRIVE_CALLBACK_HEADERS")?;
        if key.is_empty()
            || key.bytes().any(|b| !b.is_ascii_alphanumeric() && b != b'-')
            || value.bytes().any(|b| b.is_ascii_control() && b != b'\t')
        {
            return Err("DRIVE_CALLBACK_HEADERS".into());
        }
        if key.eq_ignore_ascii_case("host") {
            if host || value.trim() != format!("127.0.0.1:{port}") {
                return Err("DRIVE_CALLBACK_HOST".into());
            }
            host = true;
        }
        if key.eq_ignore_ascii_case("transfer-encoding")
            || (key.eq_ignore_ascii_case("content-length") && value.trim() != "0")
        {
            return Err("DRIVE_CALLBACK_BODY_DENIED".into());
        }
    }
    if !host {
        return Err("DRIVE_CALLBACK_HOST".into());
    }
    let url = reqwest::Url::parse(&format!("http://127.0.0.1:{port}{}", fields[1]))
        .map_err(|_| "DRIVE_CALLBACK_ROUTE")?;
    if url.path() != path {
        return Err("DRIVE_CALLBACK_ROUTE".into());
    }
    let mut seen = std::collections::HashMap::new();
    for (key, value) in url.query_pairs() {
        if seen.insert(key.to_string(), value.to_string()).is_some() {
            return Err("DRIVE_CALLBACK_DUPLICATE".into());
        }
    }
    if seen.get("state").map(String::as_str) != Some(nonce) {
        return Err("DRIVE_CALLBACK_STATE".into());
    }
    if seen.contains_key("error") {
        if seen.contains_key("code") {
            return Err("DRIVE_CALLBACK_CONTRADICTORY".into());
        }
        return Err("DRIVE_AUTH_CONSENT_DENIED".into());
    }
    if seen
        .get("iss")
        .is_some_and(|s| s != "https://accounts.google.com")
    {
        return Err("DRIVE_CALLBACK_ISSUER".into());
    }
    Text::from_str(seen.get("code").ok_or("DRIVE_CALLBACK_CODE")?, 256)
        .map_err(|_| "DRIVE_CALLBACK_CODE".into())
}
fn picked_resources(bytes: &[u8], port: u16, picker: bool) -> Result<Vec<String>> {
    if !picker {
        return Ok(vec![]);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "DRIVE_CALLBACK_FORMAT")?;
    let route = text
        .split("\r\n")
        .next()
        .and_then(|s| s.split(' ').nth(1))
        .ok_or("DRIVE_CALLBACK_ROUTE")?;
    let url = reqwest::Url::parse(&format!("http://127.0.0.1:{port}{route}"))
        .map_err(|_| "DRIVE_CALLBACK_ROUTE")?;
    let mut ids = vec![];
    for (key, value) in url.query_pairs() {
        if key == "scope" && value != DRIVE_SCOPE {
            return Err("DRIVE_CALLBACK_PICKER_SCOPE".into());
        }
        if key == "picked_file_ids" && !value.is_empty() {
            for id in value.split(',') {
                resource_id(id).map_err(|_| "DRIVE_CALLBACK_PICKER_ID")?;
                if ids.len() == 100 || ids.iter().any(|v| v == id) {
                    return Err("DRIVE_CALLBACK_PICKER_IDS".into());
                }
                ids.push(id.to_string());
            }
        }
    }
    Ok(ids)
}
async fn reply(socket: &mut tokio::net::TcpStream, ok: bool) -> Result<()> {
    let (status, body) = if ok {
        (
            "200 OK",
            "AlfredPet: authorization saved. Close this tab and return to the app.",
        )
    } else {
        (
            "400 Bad Request",
            "AlfredPet: this authorization response was not accepted.",
        )
    };
    let reply=format!("HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",body.len());
    tokio::time::timeout(Duration::from_secs(2), socket.write_all(reply.as_bytes()))
        .await
        .map_err(|_| "DRIVE_CALLBACK_REPLY")?
        .map_err(|_| "DRIVE_CALLBACK_REPLY".into())
}
#[cfg(windows)]
fn open_system_browser(url: &str) -> Result<()> {
    use windows::{
        core::{w, PCWSTR},
        Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
    };
    let wide: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as usize <= 32 {
        return Err("DRIVE_SYSTEM_BROWSER_OPEN".into());
    }
    Ok(())
}
#[cfg(not(windows))]
fn open_system_browser(_: &str) -> Result<()> {
    Err("WINDOWS_11_REQUIRED".into())
}
#[cfg(all(test, windows))]
pub(crate) fn install_owned_grant(c: &Arc<Core>, id: &str, expired: bool) -> Result<()> {
    let scope = Scope::new(c, id)?;
    let env = Envelope::empty(&scope.config, None);
    let mut env = parse_tokens(
        &json!({"access_token":"owned_fictional_access","refresh_token":"owned_fictional_refresh","token_type":"Bearer","scope":DRIVE_SCOPE,"expires_in":3600}),
        &scope.config,
        env,
        false,
    )?;
    if expired {
        env.issued_at = Some(chrono::Utc::now().timestamp() - 4000);
        env.expires_at = Some(chrono::Utc::now().timestamp() - 1);
    }
    let bytes = env.bytes()?;
    let receipt = scope.private(|b| storage_save(c, b, None, bytes.bytes()))?;
    let expected = mark(&scope, "owned_fixture_session", "fixture", None)?;
    finish(&scope, &expected, "ready", None, Some(&receipt))
}
#[cfg(test)]
pub(crate) fn owned_grant_version(c: &Arc<Core>, id: &str) -> Result<Uuid> {
    let scope = Scope::new(c, id)?;
    if !matches!(
        scope.config["client_id"].as_str(),
        Some(
            "owned-fixture.apps.googleusercontent.com"
                | "owned-new-client.apps.googleusercontent.com"
        )
    ) {
        return Err("OWNED_NATIVE_CLIENT_REQUIRED".into());
    }
    scope.private(|b| {
        let o = storage_observe(c, b)?.ok_or("OWNED_GRANT_MISSING")?;
        if !b.matches_client(&o) {
            return Err("OWNED_GRANT_BINDING".into());
        }
        Ok(o.version)
    })
}
#[cfg(all(test, windows))]
pub(crate) fn cleanup_owned_native_grant(c: &Core, id: &str) -> Result<()> {
    let config = record(c, id)?;
    let b = binding(c, id, &config, false)?;
    if config["client_id"] != "owned-fixture.apps.googleusercontent.com" {
        return Err("OWNED_NATIVE_CLIENT_REQUIRED".into());
    }
    let profile = c.store.setting(PROFILE_KEY);
    let profile = profile.as_str().ok_or("DRIVE_PROFILE_IDENTITY")?;
    let root = asset_files::identity(&asset_files::directory(&c.root)?)?;
    let jobs = c.store.query(
        "SELECT id,record_json FROM drive_jobs WHERE connection_id=? AND kind='upload'",
        &[json!(id)],
    )?;
    for job in jobs.as_array().ok_or("OWNED_NATIVE_JOB_RECORD")? {
        let record: Value = serde_json::from_str(
            job["record_json"]
                .as_str()
                .ok_or("OWNED_NATIVE_JOB_RECORD")?,
        )
        .map_err(|_| "OWNED_NATIVE_JOB_RECORD")?;
        let hash=Sha256::digest(json!({"namespace":"resumable-session-v1","job":job["id"],"connection":id,"authorization":record["immutable"]["authorization_binding"],"profile":profile,"root":root,"client_id":config["client_id"],"client_secret_mode":config["client_secret_mode"]}).to_string().as_bytes());
        let cap = vault::Binding::new(
            job["id"].as_str().ok_or("OWNED_NATIVE_JOB_RECORD")?,
            profile,
            hash.into(),
        )
        .map_err(str::to_string)?;
        if let Some(o) = vault::win::observe(&cap).map_err(str::to_string)? {
            if !cap.matches_client(&o)
                || !vault::win::erase_context(
                    &cap,
                    o.version,
                    receipt_context(&json!(crate::drive::digest(&record["immutable"])))?,
                )
                .map_err(str::to_string)?
            {
                return Err("OWNED_NATIVE_CLEANUP_INCOMPLETE".into());
            }
        }
        vault::win::purge_retired(&cap).map_err(str::to_string)?;
    }
    if let Some(o) = vault::win::observe(&b).map_err(str::to_string)? {
        if !b.matches_client(&o)
            || !vault::win::erase_owned(&b, o.version).map_err(str::to_string)?
        {
            return Err("OWNED_NATIVE_CLEANUP_INCOMPLETE".into());
        }
    }
    if vault::win::observe(&b).map_err(str::to_string)?.is_some() {
        return Err("OWNED_NATIVE_CLEANUP_INCOMPLETE".into());
    }
    vault::win::purge_retired(&b).map_err(str::to_string)?;
    Ok(())
}
#[cfg(all(test, windows))]
pub(crate) fn expire_owned_grant(c: &Arc<Core>, id: &str) -> Result<()> {
    let s = Scope::new(c, id)?;
    let (version, mut env) = s.envelope()?;
    env.issued_at = Some(chrono::Utc::now().timestamp() - 4000);
    env.expires_at = Some(chrono::Utc::now().timestamp() - 1);
    let bytes = env.bytes()?;
    let expected = c.store.setting(&state_key(id));
    let receipt = s.private(|b| storage_replace(c, b, version, bytes.bytes()))?;
    finish(&s, &expected, "ready", None, Some(&receipt))
}
pub(crate) fn status(c: &Arc<Core>, v: &Value) -> Result<Value> {
    auth_input(v, false)?;
    let scope = Scope::new(c, v["connection"].as_str().ok_or("DRIVE_CONNECTION_ID")?)?;
    scope.check()?;
    let state = c.store.setting(&state_key(&scope.id));
    if state["phase"] == "ready" {
        scope.private(|b| {
            if c.store.setting(&state_key(&scope.id)) != state {
                return Err("DRIVE_AUTH_STATE_CHANGED".into());
            }
            let valid = storage_observe(c, b)?.is_some_and(|o| {
                b.matches_client(&o) && state["credential_version"] == o.version.to_string()
            });
            if !valid {
                return Err("DRIVE_CREDENTIAL_CHANGED".into());
            }
            Ok(())
        })?;
    }
    Ok(state)
}
pub(crate) fn cancel(c: &Arc<Core>, v: &Value) -> Result<Value> {
    auth_input(v, true)?;
    let scope = Scope::new(c, v["connection"].as_str().ok_or("DRIVE_CONNECTION_ID")?)?;
    let nonce = v["session_id"].as_str().ok_or("DRIVE_AUTH_SESSION")?;
    canonical(nonce).map_err(|_| "DRIVE_AUTH_SESSION")?;
    let _lease = integrations::save_lock(c)?;
    check_attempt_held(&scope, nonce)?;
    let state = c.store.setting(&state_key(&scope.id));
    if state["session_id"] != nonce
        || ![
            "authorization_pending",
            "browser_launch_in_flight",
            "authorizing",
            "exchange_in_flight",
        ]
        .contains(&state["phase"].as_str().unwrap_or(""))
    {
        return Err("DRIVE_AUTH_ALREADY_FINISHED".into());
    }
    c.store
        .set_setting(&format!("drive.auth_attempt.{}", scope.id), &Value::Null)?;
    let launch_uncertain = state["phase"] == "browser_launch_in_flight";
    finish(
        &scope,
        &state,
        if launch_uncertain {
            "uncertain"
        } else {
            "reauthorize"
        },
        Some(if launch_uncertain {
            "DRIVE_SYSTEM_BROWSER_OPEN_UNCERTAIN"
        } else {
            "DRIVE_AUTH_CANCELLED"
        }),
        None,
    )?;
    Ok(json!({"cancelled":true,"system_browser_may_have_opened":launch_uncertain}))
}
fn auth_input(v: &Value, cancel: bool) -> Result<()> {
    let mut keys = vec!["connection"];
    if cancel {
        keys.push("session_id");
    }
    let object = v.as_object().ok_or("DRIVE_AUTH_INPUT")?;
    if object.len() != keys.len()
        || keys
            .iter()
            .any(|key| object.get(*key).is_none_or(|v| !v.is_string()))
    {
        return Err("DRIVE_AUTH_INPUT".into());
    }
    Ok(())
}
#[cfg(all(test, windows))]
#[path = "drive_auth_tests.rs"]
mod tests;
