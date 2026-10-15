//! Closed native client registration. Client-only envelopes never authorize
//! resource requests; verified enrollment can outlive aborted consent.
use super::*;
use reqwest::{header, Url};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Mode {
    PreRegistered,
    ClientMetadataDocument { document_url: String },
    LegacyDynamic { registration_endpoint: String },
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Registration {
    pub(crate) mode: Mode,
    pub(crate) redirect_uri: String,
    pub(crate) refresh_registered: bool,
    pub(crate) secret_expires_at: Option<i64>,
    pub(crate) management_uri: Option<String>,
}
pub(crate) struct Registered {
    pub(crate) info: Registration,
    pub(crate) secret: Option<Text>,
    pub(crate) management_token: Option<Text>,
}
pub(crate) fn redirect_uri(text: &str) -> Result<Url> {
    let url = Url::parse(text).map_err(|_| "MCP_OAUTH_REGISTERED_REDIRECT")?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/oauth/mcp/callback"
        || url.query().is_some()
        || url.fragment().is_some()
        || text != url.as_str()
    {
        return Err("MCP_OAUTH_REGISTERED_REDIRECT".into());
    }
    Ok(url)
}
// Native loopback registration permits only the port to vary. No normalized
// path, host, query or fragment can substitute for the registered endpoint.
pub(crate) fn redirect_matches(registered: &str, actual: &str) -> Result<()> {
    let mut registered = redirect_uri(registered)?;
    let mut actual = redirect_uri(actual)?;
    if actual.port().is_none() {
        return Err("MCP_OAUTH_CALLBACK_PORT".into());
    }
    registered
        .set_port(None)
        .map_err(|_| "MCP_OAUTH_REGISTERED_REDIRECT")?;
    actual
        .set_port(None)
        .map_err(|_| "MCP_OAUTH_REGISTERED_REDIRECT")?;
    if actual != registered {
        return Err("MCP_OAUTH_REGISTERED_REDIRECT".into());
    }
    Ok(())
}
fn metadata_url(text: &str, local: bool) -> Result<()> {
    flow_endpoint(text, local)?;
    let url = Url::parse(text).map_err(|_| "MCP_OAUTH_CLIENT_METADATA_URL")?;
    let authority_end = text.find("://").ok_or("MCP_OAUTH_CLIENT_METADATA_URL")? + 3;
    let path = text[authority_end..]
        .find('/')
        .map(|i| &text[authority_end + i..])
        .ok_or("MCP_OAUTH_CLIENT_METADATA_PATH")?;
    let path = path.split('?').next().unwrap_or("");
    if url.path() == "/"
        || path.split('/').any(|s| {
            matches!(
                s.to_ascii_lowercase().as_str(),
                "." | ".." | "%2e" | ".%2e" | "%2e." | "%2e%2e"
            )
        })
    {
        return Err("MCP_OAUTH_CLIENT_METADATA_PATH".into());
    }
    Ok(())
}
impl Registration {
    pub(crate) fn validate(
        &self,
        actor: &Actor,
        secret: Option<&Text>,
        management: Option<&Text>,
    ) -> Result<()> {
        redirect_uri(&self.redirect_uri)?;
        if (actor.authorization.client_auth == ClientAuth::None) != secret.is_none()
            || secret.is_some_and(|s| s.0.len() > 16384)
            || self.secret_expires_at.is_some_and(|at| at < 0)
            || (secret.is_none() && self.secret_expires_at.is_some())
            || management.is_some() != self.management_uri.is_some()
            || management.is_some_and(|s| s.0.len() > 16384)
        {
            return Err("MCP_OAUTH_CLIENT_REGISTRATION".into());
        }
        match &self.mode {
            Mode::PreRegistered => {}
            Mode::ClientMetadataDocument { document_url } => {
                metadata_url(document_url, actor.local_network())?;
                if actor.authorization.client_id != *document_url
                    || actor.authorization.client_auth != ClientAuth::None
                    || secret.is_some()
                    || management.is_some()
                {
                    return Err("MCP_OAUTH_CLIENT_METADATA_BINDING".into());
                }
            }
            Mode::LegacyDynamic {
                registration_endpoint,
            } => {
                flow_endpoint(registration_endpoint, actor.local_network())?;
                if secret.is_some() && self.secret_expires_at.is_none() {
                    return Err("MCP_OAUTH_CLIENT_SECRET_EXPIRY_REQUIRED".into());
                }
            }
        }
        if let Some(uri) = &self.management_uri {
            flow_endpoint(uri, actor.local_network())?;
        }
        Ok(())
    }
    pub(crate) fn secret_valid_now(&self) -> Result<()> {
        if self
            .secret_expires_at
            .is_some_and(|at| at != 0 && at <= chrono::Utc::now().timestamp())
        {
            return Err("MCP_OAUTH_CLIENT_SECRET_EXPIRED".into());
        }
        Ok(())
    }
}
fn authentication(auth: &ClientAuth) -> &'static str {
    match auth {
        ClientAuth::None => "none",
        ClientAuth::ClientSecretPost => "client_secret_post",
        ClientAuth::ClientSecretBasic => "client_secret_basic",
    }
}
fn validate_client_id(text: &str) -> Result<()> {
    if text.is_empty() || text.len() > 4096 || text.chars().any(char::is_control) {
        return Err("MCP_OAUTH_CLIENT_ID".into());
    }
    Ok(())
}
fn string_set(value: &Value) -> Result<BTreeSet<String>> {
    let items = value
        .as_array()
        .filter(|a| a.len() <= 16)
        .ok_or("MCP_OAUTH_REGISTERED_METADATA_CHANGED")?;
    let set = items
        .iter()
        .map(|v| {
            v.as_str()
                .filter(|s| !s.is_empty() && s.len() <= 4096 && !s.chars().any(char::is_control))
                .map(str::to_string)
                .ok_or("MCP_OAUTH_REGISTERED_METADATA_CHANGED")
        })
        .collect::<std::result::Result<BTreeSet<_>, _>>()?;
    if set.len() != items.len() {
        return Err("MCP_OAUTH_REGISTERED_METADATA_CHANGED".into());
    }
    Ok(set)
}
pub(crate) fn dynamic_response(
    actor: &Actor,
    endpoint: &str,
    redirect: &str,
    refresh: bool,
    body: Value,
) -> Result<(String, Registered)> {
    let Value::Object(mut body) = body else {
        return Err("MCP_OAUTH_REGISTRATION_RESPONSE".into());
    };
    let client_id = body
        .remove("client_id")
        .and_then(|v| v.as_str().map(str::to_string))
        .ok_or("MCP_OAUTH_CLIENT_ID")?;
    validate_client_id(&client_id)?;
    if body
        .get("token_endpoint_auth_method")
        .and_then(Value::as_str)
        != Some(authentication(&actor.authorization.client_auth))
        || body.get("application_type").is_some_and(|v| v != "native")
        || string_set(
            body.get("redirect_uris")
                .ok_or("MCP_OAUTH_REGISTERED_METADATA_CHANGED")?,
        )? != BTreeSet::from([redirect.to_string()])
        || string_set(
            body.get("response_types")
                .ok_or("MCP_OAUTH_REGISTERED_METADATA_CHANGED")?,
        )? != BTreeSet::from(["code".to_string()])
        || string_set(
            body.get("grant_types")
                .ok_or("MCP_OAUTH_REGISTERED_METADATA_CHANGED")?,
        )? != if refresh {
            BTreeSet::from([
                "authorization_code".to_string(),
                "refresh_token".to_string(),
            ])
        } else {
            BTreeSet::from(["authorization_code".to_string()])
        }
    {
        return Err("MCP_OAUTH_REGISTERED_METADATA_CHANGED".into());
    }
    let take_text =
        |body: &mut serde_json::Map<String, Value>, key: &str| -> Result<Option<Text>> {
            body.remove(key)
                .map(|v| {
                    if let Value::String(s) = v {
                        Text::new(s, 16384)
                    } else {
                        Err("MCP_OAUTH_CLIENT_REGISTRATION".into())
                    }
                })
                .transpose()
        };
    let mut secret = take_text(&mut body, "client_secret")?;
    let mut secret_expires_at = body
        .remove("client_secret_expires_at")
        .map(|v| {
            v.as_i64()
                .filter(|at| *at >= 0)
                .ok_or("MCP_OAUTH_CLIENT_SECRET_EXPIRY")
        })
        .transpose()?;
    if secret.is_some() && secret_expires_at.is_none() {
        return Err("MCP_OAUTH_CLIENT_SECRET_EXPIRY_REQUIRED".into());
    }
    // Registration servers may issue an unused secret to a public client.
    // Validate the response, then discard it rather than changing the reviewed
    // authentication method or sending it in the subsequent token request.
    if actor.authorization.client_auth == ClientAuth::None {
        secret = None;
        secret_expires_at = None;
    }
    if body.get("client_id_issued_at").is_some_and(|v| {
        v.as_i64()
            .is_none_or(|at| at <= 0 || at > chrono::Utc::now().timestamp() + 300)
    }) {
        return Err("MCP_OAUTH_CLIENT_ID_ISSUED_AT".into());
    }
    let management_token = take_text(&mut body, "registration_access_token")?;
    let management_uri = body
        .remove("registration_client_uri")
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or("MCP_OAUTH_REGISTRATION_MANAGEMENT_URI")
        })
        .transpose()?;
    let registered = Registered {
        info: Registration {
            mode: Mode::LegacyDynamic {
                registration_endpoint: endpoint.into(),
            },
            redirect_uri: redirect.into(),
            refresh_registered: refresh,
            secret_expires_at,
            management_uri,
        },
        secret,
        management_token,
    };
    let mut actual = actor.clone();
    actual.authorization.client_id = client_id.clone();
    registered.info.validate(
        &actual,
        registered.secret.as_ref(),
        registered.management_token.as_ref(),
    )?;
    registered.info.secret_valid_now()?;
    Ok((client_id, registered))
}
pub(crate) fn client_document(
    actor: &Actor,
    document_url: &str,
    actual_redirect: &str,
    body: Value,
) -> Result<Registered> {
    metadata_url(document_url, actor.local_network())?;
    if body["client_id"] != document_url
        || body["client_name"]
            .as_str()
            .is_none_or(|s| s.trim().is_empty() || s.len() > 256 || s.chars().any(char::is_control))
        || actor.authorization.client_id != document_url
        || actor.authorization.client_auth != ClientAuth::None
        || body.get("client_secret").is_some()
        || body.get("client_secret_expires_at").is_some()
        || body
            .get("token_endpoint_auth_method")
            .is_some_and(|v| v != "none")
    {
        return Err("MCP_OAUTH_CLIENT_METADATA_BINDING".into());
    }
    let redirects = body["redirect_uris"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 16)
        .ok_or("MCP_OAUTH_CLIENT_METADATA_REDIRECTS")?;
    let mut selected = None;
    for uri in redirects {
        let text = uri.as_str().ok_or("MCP_OAUTH_CLIENT_METADATA_REDIRECTS")?;
        if redirect_matches(text, actual_redirect).is_ok() {
            selected = Some(text.to_string());
        }
    }
    let refresh_registered = body
        .get("grant_types")
        .map(|v| -> Result<bool> {
            let a = string_set(v).map_err(|_| "MCP_OAUTH_CLIENT_METADATA_GRANTS")?;
            if !a.contains("authorization_code") {
                return Err("MCP_OAUTH_CLIENT_METADATA_GRANTS".into());
            }
            Ok(a.contains("refresh_token"))
        })
        .transpose()?
        .unwrap_or(false);
    if let Some(value) = body.get("response_types") {
        if !string_set(value)
            .map_err(|_| "MCP_OAUTH_CLIENT_METADATA_GRANTS")?
            .contains("code")
        {
            return Err("MCP_OAUTH_CLIENT_METADATA_GRANTS".into());
        }
    }
    let registered = Registered {
        info: Registration {
            mode: Mode::ClientMetadataDocument {
                document_url: document_url.into(),
            },
            redirect_uri: selected.ok_or("MCP_OAUTH_CLIENT_METADATA_REDIRECTS")?,
            refresh_registered,
            secret_expires_at: None,
            management_uri: None,
        },
        secret: None,
        management_token: None,
    };
    registered.info.validate(actor, None, None)?;
    Ok(registered)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Choice {
    PreRegistered {
        client_id: String,
        registered_redirect: String,
    },
    ClientMetadataDocument {
        document_url: String,
    },
    LegacyDynamic {
        registration_endpoint: String,
    },
    Manual,
}
pub(crate) struct Plan {
    choice: Choice,
    auth: ClientAuth,
    refresh: bool,
}

// Only public choices enter Core's permission review. The owned discovery
// observation supplies server endpoints; inline client credentials are absent
// from this schema and from the durable permission/audit payload.
pub(crate) struct Reviewed {
    plan: Plan,
    authorization: Authorization,
    consent_endpoint: String,
    issuer_required: bool,
    login_available: bool,
}
impl Reviewed {
    pub(crate) fn for_core(
        core: &Arc<Core>,
        config: &mcp_profile::Config,
        input: &Value,
        discovery: &Value,
    ) -> Result<Self> {
        #[cfg(test)]
        let local = core.mcp_oauth_owned_network.load(Ordering::SeqCst);
        #[cfg(not(test))]
        let local = {
            let _ = core;
            false
        };
        Self::build(config, input, discovery, local)
    }
    fn build(
        config: &mcp_profile::Config,
        input: &Value,
        discovery: &Value,
        local: bool,
    ) -> Result<Self> {
        if discovery["issuer"] != input["issuer"]
            || discovery["resource"] != mcp_profile::endpoint(config)?.as_str()
            || discovery["server_metadata"]["issuer"] != input["issuer"]
            || discovery["server_metadata"]["pkce_s256"] != true
            || discovery["server_metadata"]["code_flow"] != true
        {
            return Err("MCP_OAUTH_DISCOVERY_BINDING".into());
        }
        let metadata = &discovery["server_metadata"];
        let auth = input
            .get("client_auth")
            .map(|v| serde_json::from_value(v.clone()).map_err(|_| "MCP_OAUTH_CLIENT_AUTH_BINDING"))
            .transpose()?
            .unwrap_or(ClientAuth::None);
        let preregistered = input
            .get("preregistered")
            .map(|v| -> Result<(&str, &str)> {
                Ok((
                    v["client_id"].as_str().ok_or("MCP_OAUTH_CLIENT_ID")?,
                    v["redirect_uri"]
                        .as_str()
                        .ok_or("MCP_OAUTH_REGISTERED_REDIRECT")?,
                ))
            })
            .transpose()?;
        let document = input
            .get("metadata_document_url")
            .map(|v| v.as_str().ok_or("MCP_OAUTH_CLIENT_METADATA_URL"))
            .transpose()?;
        let plan = Plan::select_mode(
            metadata,
            preregistered,
            document,
            input["allow_legacy_dynamic"] == true,
            auth.clone(),
            local,
        )?;
        let scopes = input
            .get("requested_scopes")
            .unwrap_or(&discovery["requested_scopes"]);
        let requested_scopes = if scopes.is_null() {
            None
        } else {
            let items: Vec<String> =
                serde_json::from_value(scopes.clone()).map_err(|_| "MCP_OAUTH_SCOPE")?;
            let unique: BTreeSet<String> = items.iter().cloned().collect();
            if unique.len() != items.len() {
                return Err("MCP_OAUTH_SCOPE".into());
            }
            Some(unique)
        };
        if requested_scopes.as_ref().is_some_and(BTreeSet::is_empty) {
            return Err("MCP_OAUTH_EMPTY_SCOPE_REVIEW_REQUIRED".into());
        }
        let authorization = Authorization {
            issuer: input["issuer"]
                .as_str()
                .ok_or("MCP_OAUTH_ISSUER_REQUIRED")?
                .into(),
            resource: discovery["resource"]
                .as_str()
                .ok_or("MCP_OAUTH_RESOURCE_BINDING")?
                .into(),
            token_endpoint: metadata["exchange_endpoint"]
                .as_str()
                .ok_or("MCP_OAUTH_AS_ENDPOINT")?
                .into(),
            client_id: match &plan.choice {
                Choice::Manual => "__alfredpet_manual_required__",
                _ => plan.client_id()?,
            }
            .into(),
            client_auth: auth,
            requested_scopes,
        };
        authorization.validate(config, local)?;
        let consent_endpoint = metadata["consent_endpoint"]
            .as_str()
            .ok_or("MCP_OAUTH_AS_ENDPOINT")?
            .to_string();
        flow_endpoint(&consent_endpoint, local)?;
        let issuer_required = metadata["callback_issuer_required"]
            .as_bool()
            .ok_or("MCP_OAUTH_METADATA_BOOLEAN")?;
        let login_available = matches!(&config.auth, mcp_profile::Auth::OAuth2 { .. })
            && !matches!(plan.choice, Choice::Manual);
        Ok(Self {
            plan,
            authorization,
            consent_endpoint,
            issuer_required,
            login_available,
        })
    }
    pub(crate) fn validate_start(&self, supplied: bool) -> Result<()> {
        if !self.login_available {
            return Err("MCP_OAUTH_PROFILE_OR_CLIENT_REQUIRED".into());
        }
        if supplied
            && (!matches!(self.plan.choice, Choice::PreRegistered { .. })
                || self.authorization.client_auth == ClientAuth::None)
        {
            return Err("MCP_OAUTH_CLIENT_SECRET_BINDING".into());
        }
        Ok(())
    }
    pub(crate) async fn consent(
        self,
        core: &Arc<Core>,
        id: &str,
        secret: Option<Text>,
    ) -> Result<flow::Consent> {
        self.validate_start(secret.is_some())?;
        #[cfg(test)]
        let actor = Actor::build(
            core,
            id,
            self.authorization,
            core.mcp_oauth_owned_network.load(Ordering::SeqCst),
        )?;
        #[cfg(not(test))]
        let actor = Actor::new(core, id, self.authorization)?;
        flow::Consent::bind_registered(
            actor,
            &self.consent_endpoint,
            self.issuer_required,
            self.plan,
            secret,
        )
        .await
    }
    pub(crate) fn review(&self) -> Value {
        json!({"registration":self.plan.review(),"issuer":self.authorization.issuer,"resource":self.authorization.resource,
            "consent_endpoint":self.consent_endpoint,"exchange_endpoint":self.authorization.token_endpoint,
            "client_id":if matches!(self.plan.choice, Choice::LegacyDynamic {..}|Choice::Manual) {None}else{Some(&self.authorization.client_id)},
            "client_auth":self.authorization.client_auth,"requested_scopes":self.authorization.requested_scopes,
            "callback_issuer_required":self.issuer_required,"browserStarted":false,"credentialsStored":false,"registrationPerformed":false,
            "loginAvailable":self.login_available,"automaticReplay":false})
    }
}
impl Plan {
    #[cfg(test)]
    pub(crate) fn select(
        metadata: &Value,
        preregistered: Option<(&str, &str)>,
        document: Option<&str>,
        legacy: bool,
        auth: ClientAuth,
    ) -> Result<Self> {
        Self::select_mode(metadata, preregistered, document, legacy, auth, false)
    }
    fn select_mode(
        metadata: &Value,
        preregistered: Option<(&str, &str)>,
        document: Option<&str>,
        legacy: bool,
        auth: ClientAuth,
        local: bool,
    ) -> Result<Self> {
        let support = match auth {
            ClientAuth::None => "public_client_supported",
            ClientAuth::ClientSecretBasic => "basic_client_supported",
            ClientAuth::ClientSecretPost => "post_client_supported",
        };
        if metadata[support] != true {
            return Err("MCP_OAUTH_CLIENT_AUTH_UNSUPPORTED".into());
        }
        let choice = if let Some((client_id, registered_redirect)) = preregistered {
            validate_client_id(client_id)?;
            redirect_uri(registered_redirect)?;
            Choice::PreRegistered {
                client_id: client_id.into(),
                registered_redirect: registered_redirect.into(),
            }
        } else if metadata["client_metadata_documents_supported"] == true && document.is_some() {
            let document_url = document.ok_or("MCP_OAUTH_CLIENT_METADATA_URL")?;
            metadata_url(document_url, local)?;
            if auth != ClientAuth::None {
                return Err("MCP_OAUTH_CLIENT_METADATA_BINDING".into());
            }
            Choice::ClientMetadataDocument {
                document_url: document_url.into(),
            }
        } else if legacy {
            let registration_endpoint = metadata["registration_endpoint"]
                .as_str()
                .ok_or("MCP_OAUTH_MANUAL_REGISTRATION_REQUIRED")?;
            flow_endpoint(registration_endpoint, local)?;
            Choice::LegacyDynamic {
                registration_endpoint: registration_endpoint.into(),
            }
        } else {
            Choice::Manual
        };
        Ok(Self {
            choice,
            auth,
            refresh: metadata["refresh_supported"] == true,
        })
    }
    pub(crate) fn client_id(&self) -> Result<&str> {
        match &self.choice {
            Choice::PreRegistered { client_id, .. } => Ok(client_id),
            Choice::ClientMetadataDocument { document_url } => Ok(document_url),
            Choice::LegacyDynamic { .. } => Ok("__alfredpet_dcr_pending__"),
            Choice::Manual => Err("MCP_OAUTH_MANUAL_REGISTRATION_REQUIRED".into()),
        }
    }
    pub(crate) fn review(&self) -> Value {
        json!({"choice":self.choice,"client_auth":self.auth,"request_refresh_registration":self.refresh,"automatic_retry":false,"native_before_consent":true})
    }
    pub(crate) async fn acquire(
        &self,
        mut actor: Actor,
        redirect: &str,
        supplied_secret: Option<Text>,
    ) -> Result<(Actor, Registered)> {
        actor.check()?;
        redirect_uri(redirect)?;
        if actor.authorization.client_auth != self.auth {
            return Err("MCP_OAUTH_CLIENT_AUTH_BINDING".into());
        }
        match &self.choice {
            Choice::Manual => return Err("MCP_OAUTH_MANUAL_REGISTRATION_REQUIRED".into()),
            Choice::PreRegistered {
                client_id,
                registered_redirect,
            } => {
                if *client_id != actor.authorization.client_id {
                    return Err("MCP_OAUTH_CLIENT_ID_BINDING".into());
                }
                redirect_matches(registered_redirect, redirect)?;
            }
            Choice::ClientMetadataDocument { document_url } => {
                if *document_url != actor.authorization.client_id || supplied_secret.is_some() {
                    return Err("MCP_OAUTH_CLIENT_METADATA_BINDING".into());
                }
                metadata_url(document_url, actor.local_network())?;
            }
            Choice::LegacyDynamic {
                registration_endpoint,
            } => {
                if supplied_secret.is_some() {
                    return Err("MCP_OAUTH_CLIENT_SECRET_BINDING".into());
                }
                flow_endpoint(registration_endpoint, actor.local_network())?;
                // The stored ID is only a candidate. The protected private
                // envelope below must verify it before any reuse or POST.
                let _lease = integrations::save_lock(&actor.core)?;
                let raw = actor.core.store.setting(&key(&actor.id));
                if !raw.is_null() {
                    let prior: State =
                        serde_json::from_value(raw).map_err(|_| "MCP_OAUTH_STATE_REQUIRED")?;
                    if let Some(reference) = &prior.client_reference {
                        let mut candidate = actor.authorization.clone();
                        candidate.client_id = reference.authorization.client_id.clone();
                        if same_client(&candidate, &reference.authorization) {
                            actor.authorization = candidate;
                        }
                    }
                }
            }
        }
        let replacing_secret =
            matches!(self.choice, Choice::PreRegistered { .. }) && supplied_secret.is_some();
        let cached = if replacing_secret {
            None
        } else {
            actor.client()?
        };
        let mut cached = cached.filter(|c| match (&self.choice, &c.info.mode) {
            (
                Choice::PreRegistered {
                    registered_redirect,
                    ..
                },
                Mode::PreRegistered,
            ) => c.info.redirect_uri == *registered_redirect,
            (
                Choice::ClientMetadataDocument { document_url },
                Mode::ClientMetadataDocument { document_url: old },
            ) => document_url == old,
            (Choice::LegacyDynamic { .. }, Mode::LegacyDynamic { .. }) => true,
            _ => false,
        });
        if let Some(cached) = cached.as_ref() {
            redirect_matches(&cached.info.redirect_uri, redirect)?;
        }
        let registered = match &self.choice {
            Choice::PreRegistered {
                registered_redirect,
                ..
            } => {
                if supplied_secret.is_none() && cached.is_some() {
                    cached.take().ok_or("MCP_OAUTH_CLIENT_REGISTRATION")?
                } else {
                    Registered {
                        info: Registration {
                            mode: Mode::PreRegistered,
                            redirect_uri: registered_redirect.clone(),
                            refresh_registered: self.refresh,
                            secret_expires_at: None,
                            management_uri: None,
                        },
                        secret: supplied_secret,
                        management_token: None,
                    }
                }
            }
            Choice::ClientMetadataDocument { document_url } => {
                // Recheck the actual supplied document for this consent; no
                // stale cache promises hosted metadata is still available.
                let state = actor.begin(false)?;
                let result = actor
                    .guard_phase(
                        &state.session_id,
                        Phase::Authorizing,
                        document_request(document_url),
                    )
                    .await;
                let body = match result {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = actor.abandon(&state.session_id);
                        return Err(e);
                    }
                };
                match client_document(&actor, document_url, redirect, body) {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = actor.abandon(&state.session_id);
                        return Err(e);
                    }
                }
            }
            Choice::LegacyDynamic {
                registration_endpoint,
            } => {
                if let Some(cached) = cached.take() {
                    cached
                } else {
                    // A durable uncertain registration must be recovered or
                    // explicitly reset through a separate reviewed route.
                    let prior = actor.core.store.setting(&key(&actor.id));
                    if matches!(
                        prior["phase"].as_str(),
                        Some("registration_in_flight" | "publication_pending" | "uncertain")
                    ) && prior["authorization"]["issuer"] == actor.authorization.issuer
                    {
                        return Err("MCP_OAUTH_CLIENT_RECOVERY_REQUIRED".into());
                    }
                    let state = actor.begin(false)?;
                    actor.transition(
                        &state.session_id,
                        Phase::Authorizing,
                        Phase::RegistrationInFlight,
                    )?;
                    let result = actor
                        .guard_phase(
                            &state.session_id,
                            Phase::RegistrationInFlight,
                            dynamic_request(&actor, registration_endpoint, redirect, self.refresh),
                        )
                        .await;
                    let body = match result {
                        Ok(v) => v,
                        Err(e) => {
                            let _ = actor.abandon(&state.session_id);
                            return Err(e);
                        }
                    };
                    let (client_id, registered) = match dynamic_response(
                        &actor,
                        registration_endpoint,
                        redirect,
                        self.refresh,
                        body,
                    ) {
                        Ok(v) => v,
                        Err(e) => {
                            let _ = actor.abandon(&state.session_id);
                            return Err(e);
                        }
                    };
                    let _lease = integrations::save_lock(&actor.core)?;
                    let old = actor.check_session_held(&state.session_id)?;
                    if old.phase != Phase::RegistrationInFlight {
                        return Err("MCP_OAUTH_PHASE".into());
                    }
                    let mut next = old.clone();
                    next.authorization.client_id = client_id.clone();
                    actor.cas_held(&old, &next)?;
                    actor.authorization.client_id = client_id;
                    registered
                }
            }
            Choice::Manual => return Err("MCP_OAUTH_MANUAL_REGISTRATION_REQUIRED".into()),
        };
        registered.info.validate(
            &actor,
            registered.secret.as_ref(),
            registered.management_token.as_ref(),
        )?;
        registered.info.secret_valid_now()?;
        // Cache or local registration is also republished for the fresh grant
        // intent, so cancellation has a current-generation native reference.
        let current = {
            let _lease = integrations::save_lock(&actor.core)?;
            actor.check_held()?;
            let current = actor.core.store.setting(&key(&actor.id));
            if current["phase"] == "registration_in_flight"
                && current["authorization"] == json!(actor.authorization)
            {
                Some(actor.state_held()?)
            } else {
                None
            }
        };
        let state = if let Some(current) = current {
            current
        } else {
            let s = actor.begin(false)?;
            actor.transition(
                &s.session_id,
                Phase::Authorizing,
                Phase::RegistrationInFlight,
            )?
        };
        let result = async {
            let envelope = actor.registration_envelope(&state, registered)?;
            actor.transition(
                &state.session_id,
                Phase::RegistrationInFlight,
                Phase::PublicationPending,
            )?;
            actor.publish(&state.session_id, envelope)?;
            actor
                .client()?
                .ok_or("MCP_OAUTH_CLIENT_REGISTRATION".into())
        }
        .await;
        match result {
            Ok(c) => Ok((actor, c)),
            Err(e) => {
                let _ = actor.abandon(&state.session_id);
                Err(e)
            }
        }
    }
}
async fn body(
    mut response: reqwest::Response,
    required_status: u16,
    document: bool,
) -> Result<Value> {
    if response.status().as_u16() != required_status {
        return Err("MCP_OAUTH_REGISTRATION_HTTP_OUTCOME".into());
    }
    let mime = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .ok_or("MCP_OAUTH_REGISTRATION_CONTENT_TYPE")?;
    let mut parts = mime.split(';');
    let mime = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    if !(mime == "application/json"
        || (document && mime.starts_with("application/") && mime.ends_with("+json")))
        || parts.any(|p| !p.trim().eq_ignore_ascii_case("charset=utf-8"))
    {
        return Err("MCP_OAUTH_REGISTRATION_CONTENT_TYPE".into());
    }
    if response.content_length().is_some_and(|n| n > 65536) {
        return Err("MCP_OAUTH_REGISTRATION_LIMIT".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "MCP_OAUTH_REGISTRATION_NETWORK_OUTCOME")?
    {
        if bytes.len() + chunk.len() > 65536 {
            return Err("MCP_OAUTH_REGISTRATION_LIMIT".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let result =
        crate::mcp_oauth::strict_json(&bytes).map_err(|_| "MCP_OAUTH_REGISTRATION_JSON".into());
    for b in &mut bytes {
        unsafe {
            std::ptr::write_volatile(b, 0);
        }
    }
    compiler_fence(Ordering::SeqCst);
    result
}
fn http() -> Result<reqwest::Client> {
    crate::http_client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| "MCP_OAUTH_REGISTRATION_CLIENT".into())
}
async fn document_request(url: &str) -> Result<Value> {
    let response = http()?
        .get(url)
        .header(header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|_| "MCP_OAUTH_CLIENT_METADATA_NETWORK")?;
    body(response, 200, true).await
}
async fn dynamic_request(actor: &Actor, url: &str, redirect: &str, refresh: bool) -> Result<Value> {
    let request = json!({"client_name":"AlfredPet","application_type":"native","redirect_uris":[redirect],"response_types":["code"],"grant_types":if refresh {json!(["authorization_code","refresh_token"])}else{json!(["authorization_code"])},"token_endpoint_auth_method":authentication(&actor.authorization.client_auth)});
    let response = http()?
        .post(url)
        .header(header::ACCEPT, "application/json")
        .json(&request)
        .send()
        .await
        .map_err(|_| "MCP_OAUTH_REGISTRATION_NETWORK_OUTCOME")?;
    body(response, 201, false).await
}

#[cfg(test)]
#[path = "mcp_auth_enrollment_tests.rs"]
mod tests;
